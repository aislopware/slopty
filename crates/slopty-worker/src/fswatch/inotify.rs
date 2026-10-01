//! inotify(7): one watch per directory, which names the entry each event is about, so a
//! file's writes, a rename over it, its delete and its return all arrive on its directory.

use std::ffi::OsStr;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::sync::Arc;

use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};

use super::{DirWatch, Hit};

/// A watch is its watch descriptor.
pub(super) type WatchId = i32;

/// What a directory is watched for: its entries' writes (`MODIFY` for a writer that keeps the
/// file open, `CLOSE_WRITE` for one that is done), their modes, their coming and going, and
/// the directory itself going. `EXCL_UNLINK` leaves out writes to an entry already unlinked.
const DIR_EVENTS: WatchFlags = WatchFlags::MODIFY
    .union(WatchFlags::CLOSE_WRITE)
    .union(WatchFlags::ATTRIB)
    .union(WatchFlags::CREATE)
    .union(WatchFlags::DELETE)
    .union(WatchFlags::MOVED_FROM)
    .union(WatchFlags::MOVED_TO)
    .union(WatchFlags::DELETE_SELF)
    .union(WatchFlags::MOVE_SELF)
    .union(WatchFlags::ONLYDIR)
    .union(WatchFlags::EXCL_UNLINK);
/// Events after which the watch no longer names the directory at its path.
const GONE: ReadFlags =
    ReadFlags::DELETE_SELF.union(ReadFlags::MOVE_SELF).union(ReadFlags::IGNORED);
/// Events after which an entry's change is whole: its writer closed it, or it was renamed in.
const DONE: ReadFlags = ReadFlags::CLOSE_WRITE.union(ReadFlags::MOVED_TO);
/// Events that change what is in an entry, not which entries there are.
const CONTENT: ReadFlags = ReadFlags::MODIFY.union(ReadFlags::CLOSE_WRITE).union(ReadFlags::ATTRIB);
/// Events that change which entries there are.
const ENTRIES: ReadFlags = ReadFlags::CREATE
    .union(ReadFlags::DELETE)
    .union(ReadFlags::MOVED_FROM)
    .union(ReadFlags::MOVED_TO);
/// Room for many events per read; one takes 16 bytes and its name up to `NAME_MAX` + 1.
const BUFFER: usize = 16 * 1024;

/// One client's inotify instance.
#[derive(Debug)]
pub(super) struct Queue {
    fd: Arc<OwnedFd>,
}

impl Queue {
    /// Fails with `EMFILE` past `max_user_instances`, and every path is then polled.
    pub(super) fn open() -> io::Result<Self> {
        Ok(Self { fd: Arc::new(inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)?) })
    }

    /// The instance's descriptor, readable while events wait.
    pub(super) fn fd(&self) -> Arc<OwnedFd> {
        Arc::clone(&self.fd)
    }

    /// Fails with `ENOSPC` past `max_user_watches`, and the paths under it are then polled.
    pub(super) fn add_dir(&self, path: &Path) -> io::Result<DirWatch> {
        let id = inotify::add_watch(&*self.fd, path, DIR_EVENTS)?;
        let remote = rustix::fs::statfs(path)
            .ok()
            .and_then(|fs| u64::try_from(fs.f_type).ok())
            .is_some_and(super::linux_remote);
        Ok(DirWatch { id, remote })
    }

    /// A watch the kernel already dropped answers `EINVAL`, which leaves nothing to undo.
    pub(super) fn remove_dir(&self, id: WatchId) {
        let _gone = inotify::remove_watch(&*self.fd, id);
    }

    /// Nothing to do: a directory's watch already hears each write to its entries.
    #[expect(clippy::unused_self, reason = "the kqueue backend's counterpart does work")]
    pub(super) fn follow_contents(&self, _folders: Vec<(std::path::PathBuf, WatchId)>) {}

    /// Take every event waiting, without blocking.
    pub(super) fn drain(&self, hits: &mut Vec<Hit>) {
        let mut buffer = [MaybeUninit::<u8>::uninit(); BUFFER];
        let mut reader = inotify::Reader::new(&*self.fd, &mut buffer);
        while let Ok(event) = reader.next() {
            let flags = event.events();
            if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
                hits.push(Hit::All);
            } else if flags.intersects(GONE) {
                hits.push(Hit::DirGone(event.wd()));
            } else {
                let name =
                    event.file_name().map(|n| OsStr::from_bytes(n.to_bytes()).to_os_string());
                let done = flags.intersects(DONE);
                let content = flags.intersects(CONTENT) && !flags.intersects(ENTRIES);
                hits.push(Hit::Dir { id: event.wd(), name, done, content });
            }
        }
    }
}
