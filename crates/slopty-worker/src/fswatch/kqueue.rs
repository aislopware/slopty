//! kqueue(2): an `O_EVTONLY` descriptor on each watched directory and file, registered for
//! `EVFILT_VNODE`. A directory's `NOTE_WRITE` is a change of its entries; a file's is a write.
//! `O_EVTONLY` keeps the volume unmountable while it is watched.

use std::collections::HashMap;
use std::io;
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, OwnedFd, RawFd};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rustix::event::kqueue::{Event, EventFilter, EventFlags, VnodeEvents, kevent, kqueue};
use rustix::fs::{Mode, OFlags};

use super::{DirWatch, Hit};

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

/// One client's kqueue and the directory descriptors registered on it.
#[derive(Debug)]
pub(super) struct Queue {
    fd: Arc<OwnedFd>,
    dirs: HashMap<RawFd, OwnedFd>,
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
        Ok(Self { fd: Arc::new(kqueue()?), dirs: HashMap::new() })
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
    pub(super) fn drain(&self, hits: &mut Vec<Hit>) {
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
                let EventFilter::Vnode { vnode, flags } = event.filter() else { continue };
                hits.push(if !self.dirs.contains_key(&vnode) {
                    Hit::Node(vnode)
                } else if flags.intersects(GONE) {
                    Hit::DirGone(vnode)
                } else {
                    Hit::Dir { id: vnode, name: None, done: false }
                });
            }
            if taken < BATCH {
                return;
            }
        }
    }
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
