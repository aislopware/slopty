//! kqueue(2): an `O_EVTONLY` descriptor on each watched directory and file, registered for
//! `EVFILT_VNODE`. A directory's `NOTE_WRITE` is a change of its entries; a file's is a write.
//! `O_EVTONLY` keeps the volume unmountable while it is watched.
//!
//! A write inside a file does not touch its directory, so a folder tile's sizes would wait for
//! the next change of its entries. The folders followed are also covered by one `FSEvents`
//! stream ([`Queue::follow_contents`]) that notes each write to an entry, and a subfolder's
//! entries changing, and rings the queue through an `EVFILT_USER` event. The stream starts off
//! the follower's thread, since a start can take seconds and no kqueue report may wait for it
//! (`fsevents::Starting`).

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io;
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use rustix::event::kqueue::{
    Event, EventFilter, EventFlags, UserDefinedFlags, UserFlags, VnodeEvents, kevent, kqueue,
};
use rustix::fs::{Mode, OFlags};

use super::{DirWatch, Hit};
use crate::fsevents::{self, Starting};

/// A watch is its descriptor.
pub(super) type WatchId = RawFd;

/// What a directory's entries changing, or the directory going, raises.
const DIR_EVENTS: VnodeEvents = VnodeEvents::WRITE
    .union(VnodeEvents::EXTEND)
    .union(VnodeEvents::LINK)
    .union(VnodeEvents::DELETE)
    .union(VnodeEvents::RENAME)
    .union(VnodeEvents::REVOKE);
/// What a file's content, mode or name changing raises.
const FILE_EVENTS: VnodeEvents = DIR_EVENTS.union(VnodeEvents::ATTRIBUTES);
/// Of those, the ones after which a directory's descriptor no longer names the path.
const GONE: VnodeEvents = VnodeEvents::DELETE.union(VnodeEvents::RENAME).union(VnodeEvents::REVOKE);
/// Events taken from the queue per call.
const BATCH: usize = 64;
/// The `EVFILT_USER` event the contents stream rings.
const RING: libc::intptr_t = 1;
/// Seconds the contents stream gathers events before it calls: a size may lag this much, and
/// an editor's save is then one call.
const CONTENTS_LATENCY: f64 = 0.05;
/// Of an entry's events, the ones that change what its folder's listing says of it.
const CONTENT_CHANGED: fsevents::Flags = fsevents::kFSEventStreamEventFlagItemModified
    | fsevents::kFSEventStreamEventFlagItemInodeMetaMod
    | fsevents::kFSEventStreamEventFlagItemChangeOwner;

/// One client's kqueue and the directory descriptors registered on it.
#[derive(Debug)]
pub(super) struct Queue {
    fd: Arc<OwnedFd>,
    dirs: HashMap<RawFd, OwnedFd>,
    contents: Option<Contents>,
    /// What the contents streams said since the last ring.
    heard: Arc<Mutex<Vec<Heard>>>,
    /// Contents streams asked for so far.
    asked: u64,
}

/// The folders whose entries' contents are followed, and the streams that do it.
#[derive(Debug)]
struct Contents {
    /// Each folder as `FSEvents` names it (symbolic links resolved), with its watch.
    folders: HashMap<PathBuf, RawFd>,
    /// The stream over them, which may not be up yet, and which of the asks it was.
    stream: Starting,
    ask: u64,
    /// The stream before, kept until this one is up, so a folder it followed is heard
    /// throughout; and the folders heard throughout so far.
    covering: Option<Starting>,
    whole: HashSet<PathBuf>,
    /// This stream is up and its folders new to it were listed again: it covers them all.
    settled: bool,
}

/// What a contents stream said.
#[derive(Debug)]
enum Heard {
    /// What is inside the folder's entry of this name changed.
    Inside(PathBuf, OsString),
    /// The stream of this ask is up.
    Up(u64),
}

/// A watched file: its descriptor, and the device and inode it was opened on.
#[derive(Debug)]
pub(super) struct Node {
    fd: OwnedFd,
    key: (u64, u64),
}

impl Node {
    pub(super) fn id(&self) -> WatchId {
        self.fd.as_raw_fd()
    }

    pub(super) const fn key(&self) -> (u64, u64) {
        self.key
    }
}

impl Queue {
    pub(super) fn open() -> io::Result<Self> {
        let queue = Self {
            fd: Arc::new(kqueue()?),
            dirs: HashMap::new(),
            contents: None,
            heard: Arc::new(Mutex::new(Vec::new())),
            asked: 0,
        };
        let ring = [Event::new(
            EventFilter::User { ident: RING, flags: UserFlags::empty(), user_flags: none() },
            EventFlags::ADD | EventFlags::CLEAR,
            std::ptr::null_mut(),
        )];
        let mut out: [Event; 0] = [];
        // SAFETY: an `EVFILT_USER` registration names no descriptor, so nothing it refers to
        // can go stale (kqueue(2)).
        unsafe { kevent(&*queue.fd, &ring, &mut out, Some(Duration::ZERO)) }?;
        Ok(queue)
    }

    /// Follow the contents of `folders`, each with its own directory's watch: one stream over
    /// them all, started again when the set changes. It starts off this thread, so the stream
    /// over the folders before is kept until it is up, then retired once what it already heard
    /// is handed over. Once it is, the folders new to it are
    /// listed again for what changed inside them while it started. Unfollowed on an empty set,
    /// and not at all when `FSEvents` refuses (their entries are still followed by kqueue).
    pub(super) fn follow_contents(&mut self, folders: Vec<(PathBuf, RawFd)>) {
        let folders: HashMap<PathBuf, RawFd> = folders
            .into_iter()
            .filter_map(|(path, id)| Some((std::fs::canonicalize(path).ok()?, id)))
            .collect();
        if let Some(contents) = self.contents.as_mut() {
            let same: HashSet<&PathBuf> = contents.folders.keys().collect();
            if same == folders.keys().collect() {
                contents.folders = folders;
                return;
            }
        }
        let before = self.contents.take();
        if folders.is_empty() {
            return;
        }
        self.asked = self.asked.wrapping_add(1);
        let ask = self.asked;
        let paths: Vec<PathBuf> = folders.keys().cloned().collect();
        let (kq, noted, followed) =
            (Arc::clone(&self.fd), Arc::clone(&self.heard), folders.keys().cloned().collect());
        let (rung, said) = (Arc::clone(&self.fd), Arc::clone(&self.heard));
        let stream = Starting::spawn(
            paths,
            CONTENTS_LATENCY,
            "io.slopty.fswatch.contents",
            Box::new(move |path: &Path, flags: fsevents::Flags| {
                hear(&kq, &noted, &followed, path, flags);
            }),
            Box::new(move || {
                said.lock().push(Heard::Up(ask));
                ring(&rung);
            }),
        );
        let (covering, whole) = match before {
            Some(b) if b.settled => (Some(b.stream), b.folders.into_keys().collect()),
            Some(b) => (b.covering, b.whole),
            None => (None, HashSet::new()),
        };
        self.contents = Some(Contents { folders, stream, ask, covering, whole, settled: false });
    }

    /// The queue's descriptor, readable while events wait.
    pub(super) fn fd(&self) -> Arc<OwnedFd> {
        Arc::clone(&self.fd)
    }

    pub(super) fn add_dir(&mut self, path: &Path) -> io::Result<DirWatch> {
        let fd = open(path, OFlags::DIRECTORY)?;
        let remote = rustix::fs::fstatfs(&fd).is_ok_and(|fs| remote(&fs));
        self.register(fd.as_fd(), DIR_EVENTS)?;
        let id = fd.as_raw_fd();
        self.dirs.insert(id, fd);
        Ok(DirWatch { id, remote })
    }

    /// Closing the descriptor takes its registration with it (kqueue(2): "Calling `close()` on
    /// a file descriptor will remove any kevents that reference the descriptor").
    pub(super) fn remove_dir(&mut self, id: WatchId) {
        self.dirs.remove(&id);
    }

    /// A watch on the file at `path`, which is `key`'s device and inode.
    pub(super) fn add_node(&self, path: &Path, key: (u64, u64)) -> io::Result<Node> {
        let fd = open(path, OFlags::empty())?;
        self.register(fd.as_fd(), FILE_EVENTS)?;
        Ok(Node { fd, key })
    }

    fn register(&self, fd: BorrowedFd<'_>, events: VnodeEvents) -> io::Result<()> {
        let change = [Event::new(
            EventFilter::Vnode { vnode: fd.as_raw_fd(), flags: events },
            EventFlags::ADD | EventFlags::CLEAR,
            std::ptr::null_mut(),
        )];
        let mut none: [Event; 0] = [];
        // SAFETY: rustix asks that the descriptors named stay valid while registered. Each one
        // is an `OwnedFd` this queue or its `Node` holds until the watch is dropped, and closing
        // it removes the registration (kqueue(2)), so no event outlives its descriptor.
        unsafe { kevent(&*self.fd, &change, &mut none, Some(Duration::ZERO)) }?;
        Ok(())
    }

    /// Take every event waiting, without blocking.
    pub(super) fn drain(&mut self, hits: &mut Vec<Hit>) {
        let mut events: Vec<Event> = Vec::with_capacity(BATCH);
        loop {
            events.clear();
            // SAFETY: as in `register`; the change list is empty, and the zero timeout returns
            // at once with what is waiting.
            let taken = unsafe {
                kevent(
                    &*self.fd,
                    &[],
                    rustix::buffer::spare_capacity(&mut events),
                    Some(Duration::ZERO),
                )
            };
            let Ok(taken) = taken else {
                hits.push(Hit::All);
                return;
            };
            for event in &events {
                if matches!(event.filter(), EventFilter::User { ident: RING, .. }) {
                    self.contents_heard(hits);
                    continue;
                }
                let EventFilter::Vnode { vnode, flags } = event.filter() else { continue };
                hits.push(if !self.dirs.contains_key(&vnode) {
                    Hit::Node(vnode)
                } else if flags.intersects(GONE) {
                    Hit::DirGone(vnode)
                } else {
                    Hit::Dir { id: vnode, name: None, done: false, content: false }
                });
            }
            if taken < BATCH {
                return;
            }
        }
    }
}

impl Queue {
    /// What the contents streams said since the last ring: changes of an entry's content, and
    /// for a stream now up, each folder it is the first to cover, which may have changed inside
    /// while it started.
    fn contents_heard(&mut self, hits: &mut Vec<Hit>) {
        let heard = std::mem::take(&mut *self.heard.lock());
        let Some(contents) = self.contents.as_mut() else { return };
        for note in heard {
            match note {
                Heard::Inside(folder, name) => {
                    if let Some(id) = contents.folders.get(&folder) {
                        let name = Some(name);
                        hits.push(Hit::Dir { id: *id, name, done: false, content: true });
                    }
                }
                Heard::Up(ask) if ask == contents.ask => {
                    for (folder, id) in &contents.folders {
                        if !contents.whole.contains(folder) {
                            hits.push(Hit::Dir { id: *id, name: None, done: false, content: true });
                        }
                    }
                    if let Some(before) = contents.covering.take() {
                        before.retire();
                    }
                    contents.whole.clear();
                    contents.settled = true;
                }
                Heard::Up(_) => {}
            }
        }
    }
}

/// No user-defined flags.
fn none() -> UserDefinedFlags {
    UserDefinedFlags::new(0)
}

/// The contents stream's handler: a write to an entry of a followed folder, or an entry made or
/// removed in one of its subfolders (whose count the listing shows), is noted against the
/// folder and the entry, and the queue rung.
fn hear(
    kq: &OwnedFd,
    noted: &Mutex<Vec<Heard>>,
    followed: &HashSet<PathBuf>,
    path: &Path,
    flags: fsevents::Flags,
) {
    // One event can carry both: a file made and written between two deliveries.
    let parent = path.parent();
    let written = (flags & CONTENT_CHANGED != 0)
        .then(|| parent.filter(|p| followed.contains(*p)).zip(path.file_name()))
        .flatten();
    let counted = (flags & fsevents::PATH_CHANGED != 0)
        .then(|| {
            let grandparent = parent.and_then(Path::parent);
            grandparent.filter(|g| followed.contains(*g)).zip(parent.and_then(Path::file_name))
        })
        .flatten();
    let Some((folder, name)) = written.or(counted) else { return };
    noted.lock().push(Heard::Inside(folder.to_path_buf(), name.to_os_string()));
    ring(kq);
}

/// Wake the follower: the contents streams said something.
fn ring(kq: &OwnedFd) {
    let ring = [Event::new(
        EventFilter::User { ident: RING, flags: UserFlags::TRIGGER, user_flags: none() },
        EventFlags::empty(),
        std::ptr::null_mut(),
    )];
    let mut out: [Event; 0] = [];
    // SAFETY: as in `Queue::open`; kqueue(2) takes a trigger from any thread.
    let _rung = unsafe { kevent(kq, &ring, &mut out, Some(Duration::ZERO)) };
}

/// `O_EVTONLY` (sys/fcntl.h): a descriptor for events only, which does not hold its volume
/// mounted. Not blocking, since a FIFO at the path would otherwise wait for a writer.
fn open(path: &Path, extra: OFlags) -> io::Result<OwnedFd> {
    let evtonly = OFlags::from_bits_retain(libc::O_EVTONLY.cast_unsigned());
    Ok(rustix::fs::open(path, evtonly | OFlags::CLOEXEC | OFlags::NONBLOCK | extra, Mode::empty())?)
}

/// Whether the volume can miss another machine's writes (`MNT_LOCAL` from sys/mount.h).
fn remote(fs: &rustix::fs::StatFs) -> bool {
    let local = fs.f_flags & libc::MNT_LOCAL.cast_unsigned() != 0;
    let kind: String = fs
        .f_fstypename
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| char::from(c.cast_unsigned()))
        .collect();
    super::apple_remote(local, &kind)
}
