//! A worktree's index follows its tree on the file system's own events.
//!
//! On macOS one `FSEvents` stream covers the whole worktree, however deep: that is what it is
//! for, where a kqueue watch is one descriptor a directory (the file tiles' follower,
//! `crate::fswatch`, watches a handful of files, and chose kqueue for its 0.1 ms against
//! `FSEvents`' 11 ms). An index is caught up only when it is next asked, so the stream's delay
//! never shows. The stream reports each item made, removed or renamed; a write inside a file
//! changes no path and is dropped at once. Events the system drops (`MustScanSubDirs`, a
//! dropped queue, the root moved) walk the worktree again.
//!
//! On Linux inotify(7) watches each directory the index holds, one watch a directory: an
//! ignored `target/` or `node_modules/` takes none, where `notify`'s recursive mode walks and
//! watches everything under the top. The watches are added after the walk ([`Watch::cover`]),
//! so each directory's time is then compared with the walk's, and one that moved in between is
//! listed again. A thread of the watch's own reads the events. Past the kernel's
//! `fs.inotify.max_user_watches` (or the index's own limit) the watch is let go whole, which
//! frees its watches for the file tiles, and the index says so in the log and looks at its
//! directories' times before a query instead: slower on a large tree, never stale.
//!
//! Elsewhere there are no events (`Watch::start` is `None`), and the index looks at times.

pub(super) use imp::Watch;

/// The watch could not cover every directory: the kernel's limit, or the index's own.
#[derive(Debug)]
pub(super) struct Exhausted {
    /// Watches held when it ran out.
    pub held: usize,
    /// Why, for the log.
    pub why: String,
}

use super::index::Shared;

#[cfg(target_os = "macos")]
mod imp {
    use std::sync::Arc;

    use super::Shared;
    use crate::fsevents::{
        LOST, PATH_CHANGED, Stream, kFSEventStreamEventFlagItemInodeMetaMod,
        kFSEventStreamEventFlagItemModified,
    };

    /// A file written in place: only an ignore file's matters.
    const WRITTEN: crate::fsevents::Flags =
        kFSEventStreamEventFlagItemModified | kFSEventStreamEventFlagItemInodeMetaMod;

    /// Seconds the stream gathers events before it calls: short, since a query waits on none.
    const LATENCY: f64 = 0.01;

    /// A running `FSEvents` stream over a worktree, stopped when dropped.
    #[derive(Debug)]
    pub(in crate::find) struct Watch {
        _stream: Stream,
    }

    impl Watch {
        /// Start a stream over `shared`'s top; `None` when `FSEvents` refuses it.
        pub(in crate::find) fn start(shared: Arc<Shared>, _limit: usize) -> Option<Self> {
            let top = shared.top().to_path_buf();
            let stream = Stream::start(
                &[&top],
                None,
                LATENCY,
                "io.slopty.find.watch",
                Box::new(move |path: &std::path::Path, flags: crate::fsevents::Flags| {
                    if flags & LOST != 0 {
                        shared.lost();
                    } else if flags & (PATH_CHANGED | WRITTEN) != 0 {
                        shared.changed(path, flags & PATH_CHANGED != 0);
                    }
                }),
            )?;
            Some(Self { _stream: stream })
        }

        /// Nothing to add: the stream covers the tree however deep.
        #[expect(
            clippy::unused_self,
            clippy::unnecessary_wraps,
            reason = "inotify's counterpart adds a watch a directory, and can run out"
        )]
        pub(in crate::find) fn cover(
            &self,
            _dirs: impl IntoIterator<Item = std::path::PathBuf>,
        ) -> Result<(), super::Exhausted> {
            Ok(())
        }

        /// Kernel watches held: none, one stream.
        #[expect(clippy::unused_self, reason = "inotify's counterpart counts")]
        pub(in crate::find) const fn watches(&self) -> usize {
            0
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::mem::MaybeUninit;
    use std::os::fd::OwnedFd;
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::PathBuf;
    use std::sync::Arc;

    use parking_lot::Mutex;
    use rustix::event::{EventfdFlags, PollFd, PollFlags, eventfd, poll};
    use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};

    use super::{Exhausted, Shared};

    /// What each directory is watched for: its entries coming and going, a file closed after a
    /// write (for an ignore file changed in place), and the directory itself moving or going.
    const EVENTS: WatchFlags = WatchFlags::CREATE
        .union(WatchFlags::DELETE)
        .union(WatchFlags::MOVED_FROM)
        .union(WatchFlags::MOVED_TO)
        .union(WatchFlags::CLOSE_WRITE)
        .union(WatchFlags::DELETE_SELF)
        .union(WatchFlags::MOVE_SELF)
        .union(WatchFlags::ONLYDIR)
        .union(WatchFlags::DONT_FOLLOW)
        .union(WatchFlags::EXCL_UNLINK);
    /// Of those, the ones that change which entries a directory has.
    const ENTRIES: ReadFlags = ReadFlags::CREATE
        .union(ReadFlags::DELETE)
        .union(ReadFlags::MOVED_FROM)
        .union(ReadFlags::MOVED_TO);
    /// Room for many events per read; one takes 16 bytes and its name up to `NAME_MAX` + 1.
    const BUFFER: usize = 16 * 1024;

    /// Each watch's directory.
    type Dirs = Arc<Mutex<HashMap<i32, PathBuf>>>;

    /// An inotify instance over a worktree's directories, and the thread that reads it; both
    /// end when it is dropped, and the kernel frees its watches with it.
    #[derive(Debug)]
    pub(in crate::find) struct Watch {
        fd: Arc<OwnedFd>,
        stop: Arc<OwnedFd>,
        dirs: Dirs,
        limit: usize,
        reader: Option<std::thread::JoinHandle<()>>,
    }

    impl Watch {
        /// An instance with no watches yet, which holds at most `limit`; `None` when the
        /// kernel gives none (`max_user_instances`).
        pub(in crate::find) fn start(shared: Arc<Shared>, limit: usize) -> Option<Self> {
            let made = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK);
            let fd = match made {
                Ok(fd) => Arc::new(fd),
                Err(e) => {
                    tracing::warn!(error = %e, "quick open's index has no inotify instance");
                    return None;
                }
            };
            let stop = Arc::new(eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK).ok()?);
            let dirs: Dirs = Arc::default();
            let (read_fd, read_stop, read_dirs) =
                (Arc::clone(&fd), Arc::clone(&stop), Arc::clone(&dirs));
            let reader = std::thread::Builder::new()
                .name("find-watch".into())
                .spawn(move || read(&read_fd, &read_stop, &read_dirs, &shared))
                .ok()?;
            Some(Self { fd, stop, dirs, limit, reader: Some(reader) })
        }

        /// Watch each of `dirs`. Past the limit or the kernel's, nothing more is added and the
        /// caller lets the watch go.
        pub(in crate::find) fn cover(
            &self,
            dirs: impl IntoIterator<Item = PathBuf>,
        ) -> Result<(), Exhausted> {
            let mut held = self.dirs.lock();
            for dir in dirs {
                if held.len() >= self.limit {
                    let why = format!("the index's limit of {} watches", self.limit);
                    return Err(Exhausted { held: held.len(), why });
                }
                match inotify::add_watch(&*self.fd, &dir, EVENTS) {
                    Ok(wd) => {
                        held.insert(wd, dir);
                    }
                    Err(rustix::io::Errno::NOSPC) => {
                        let why = format!(
                            "fs.inotify.max_user_watches ({})",
                            std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
                                .map_or_else(|_| "unknown".to_owned(), |n| n.trim().to_owned())
                        );
                        return Err(Exhausted { held: held.len(), why });
                    }
                    // Gone since the walk, or not a directory any more: its parent's events
                    // tell.
                    Err(_) => {}
                }
            }
            drop(held);
            Ok(())
        }

        /// Kernel watches held.
        pub(in crate::find) fn watches(&self) -> usize {
            self.dirs.lock().len()
        }
    }

    impl Drop for Watch {
        fn drop(&mut self) {
            let _rung = rustix::io::write(&*self.stop, &1_u64.to_ne_bytes());
            if let Some(reader) = self.reader.take() {
                let _ended = reader.join();
            }
        }
    }

    /// The reader thread: each entry made, removed or renamed to the index, until `stop`.
    fn read(fd: &OwnedFd, stop: &OwnedFd, dirs: &Mutex<HashMap<i32, PathBuf>>, shared: &Shared) {
        let mut buffer = [MaybeUninit::<u8>::uninit(); BUFFER];
        loop {
            let mut fds = [PollFd::new(fd, PollFlags::IN), PollFd::new(stop, PollFlags::IN)];
            match poll(&mut fds, None) {
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => continue,
                Err(_) => return,
            }
            if fds[1].revents().contains(PollFlags::IN) {
                return;
            }
            let mut reader = inotify::Reader::new(fd, &mut buffer);
            while let Ok(event) = reader.next() {
                let flags = event.events();
                if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
                    shared.lost();
                    continue;
                }
                if flags.contains(ReadFlags::IGNORED) {
                    dirs.lock().remove(&event.wd());
                    continue;
                }
                let Some(dir) = dirs.lock().get(&event.wd()).cloned() else { continue };
                if flags.intersects(ReadFlags::DELETE_SELF | ReadFlags::MOVE_SELF) {
                    if dir.as_path() == shared.top() {
                        shared.lost();
                    }
                    continue;
                }
                let Some(name) = event.file_name() else { continue };
                let path = dir.join(OsStr::from_bytes(name.to_bytes()));
                shared.changed(&path, flags.intersects(ENTRIES));
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use std::sync::Arc;

    use super::{Exhausted, Shared};

    /// No events here: the index looks at its directories' times instead.
    #[derive(Debug)]
    pub(in crate::find) struct Watch;

    impl Watch {
        /// Never a watch.
        pub(in crate::find) fn start(_shared: Arc<Shared>, _limit: usize) -> Option<Self> {
            None
        }

        #[expect(
            clippy::unused_self,
            clippy::unnecessary_wraps,
            reason = "never called: there is no watch"
        )]
        pub(in crate::find) fn cover(
            &self,
            _dirs: impl IntoIterator<Item = std::path::PathBuf>,
        ) -> Result<(), Exhausted> {
            Ok(())
        }

        #[expect(clippy::unused_self, reason = "never called: there is no watch")]
        pub(in crate::find) const fn watches(&self) -> usize {
            0
        }
    }
}
