//! A repository followed by inotify(7): one watch on each folder of its working tree git does not
//! ignore, added as folders are made, and on the folders of its git directories that hold what
//! moves a status. A thread of the watch's own walks the tree, then reads the events.
//!
//! Past the folders it may hold (`Limits::watches`), or the kernel's
//! `fs.inotify.max_user_watches`, the watch is let go whole, which frees its watches for the file
//! tiles, and the repository is looked at every `POLL` instead. Events the kernel dropped
//! (`IN_Q_OVERFLOW`) touch the repository and have its folders watched again.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use rustix::event::{EventfdFlags, PollFd, PollFlags, eventfd, poll};
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};

use super::{Cover, Place, Repo, Said};

/// What each folder is watched for: its entries' writes (`MODIFY` for a writer that keeps the
/// file open, `CLOSE_WRITE` for one that is done), their modes, their coming and going, and the
/// folder itself going.
const EVENTS: WatchFlags = WatchFlags::MODIFY
    .union(WatchFlags::CLOSE_WRITE)
    .union(WatchFlags::ATTRIB)
    .union(WatchFlags::CREATE)
    .union(WatchFlags::DELETE)
    .union(WatchFlags::MOVED_FROM)
    .union(WatchFlags::MOVED_TO)
    .union(WatchFlags::DELETE_SELF)
    .union(WatchFlags::MOVE_SELF)
    .union(WatchFlags::ONLYDIR)
    .union(WatchFlags::DONT_FOLLOW)
    .union(WatchFlags::EXCL_UNLINK);
/// Of those, the ones after which a folder is new at the path.
const MADE: ReadFlags = ReadFlags::CREATE.union(ReadFlags::MOVED_TO);
/// Room for many events per read; one takes 16 bytes and its name up to `NAME_MAX` + 1.
const BUFFER: usize = 16 * 1024;
/// Of a git directory's folders, the ones that never hold what a status reads.
const NOT_STATUS: [&str; 6] = ["objects", "logs", "hooks", "lfs", "rr-cache", "worktrees"];

/// The inotify instance and each watch's folder, shared by the watch, its thread and the
/// repository's [`Cover`].
#[derive(Debug)]
struct Inner {
    fd: OwnedFd,
    dirs: Mutex<HashMap<i32, PathBuf>>,
    /// Folders it may watch at most.
    watches: usize,
}

/// The watch ran out: its own limit, or the kernel's.
#[derive(Debug)]
struct Full;

impl Inner {
    /// Watch each of `dirs`; one gone since it was named is left to its parent's events.
    fn cover(&self, dirs: impl IntoIterator<Item = PathBuf>) -> Result<(), Full> {
        let mut held = self.dirs.lock();
        for dir in dirs {
            if held.len() >= self.watches {
                let watches = self.watches;
                tracing::info!(dir = %dir.display(), "a repository's watch holds {watches} folders");
                return Err(Full);
            }
            match inotify::add_watch(&self.fd, &dir, EVENTS) {
                Ok(wd) => {
                    held.insert(wd, dir);
                }
                Err(rustix::io::Errno::NOSPC) => {
                    tracing::info!(dir = %dir.display(), "fs.inotify.max_user_watches reached");
                    return Err(Full);
                }
                Err(_) => {}
            }
        }
        drop(held);
        Ok(())
    }
}

/// A repository's inotify watch; dropping it ends its thread, and the kernel frees its watches
/// once the thread is out.
#[derive(Debug)]
pub(super) struct Watch {
    stop: Arc<OwnedFd>,
    stopped: Arc<AtomicBool>,
}

impl Watch {
    /// Start watching `repo` on a thread of its own, which touches it once its folders are
    /// watched, `watches` at most; `None` when the kernel gives no instance
    /// (`max_user_instances`).
    pub(super) fn start(repo: Arc<Repo>, watches: usize) -> Option<Self> {
        let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)
            .inspect_err(|e| tracing::info!(error = %e, "no inotify instance for a repository"))
            .ok()?;
        let stop = Arc::new(eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK).ok()?);
        let stopped = Arc::new(AtomicBool::new(false));
        let inner = Arc::new(Inner { fd, dirs: Mutex::default(), watches });
        let (weak, heard, top) =
            (Arc::downgrade(&inner), Arc::clone(&repo.heard), repo.place.top.clone());
        *repo.cover.lock() = Some(Cover(Box::new(move |dirs: &[PathBuf]| {
            if let Some(inner) = weak.upgrade()
                && inner.cover(dirs.iter().cloned()).is_err()
            {
                heard.gave_out(&top);
            }
        })));
        let (ends, ended) = (Arc::clone(&stop), Arc::clone(&stopped));
        std::thread::Builder::new()
            .name("repo-watch".into())
            .spawn(move || read(&inner, &ends, &ended, &repo))
            .inspect_err(|e| tracing::warn!(error = %e, "a repository's watch did not start"))
            .ok()?;
        Some(Self { stop, stopped })
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        let _rung = rustix::io::write(&*self.stop, &1_u64.to_ne_bytes());
    }
}

/// The folders to watch in `place`'s git directories: each one itself, its `info`, the
/// branches' folders however deep, and each submodule's git directory.
fn git_dirs(place: &Place) -> Vec<PathBuf> {
    let mut dirs = vec![place.git.clone(), place.common.clone(), place.common.join("info")];
    for under in ["refs/heads", "refs/remotes"] {
        dirs.extend(all_dirs(&place.common.join(under), &[]));
    }
    dirs.extend(all_dirs(&place.common.join("modules"), &NOT_STATUS));
    dirs.sort();
    dirs.dedup();
    dirs.retain(|dir| dir.is_dir());
    dirs
}

/// `from` and every folder under it, but those named in `skip`.
fn all_dirs(from: &Path, skip: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![from.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        out.push(dir);
        for entry in entries.flatten() {
            let name = entry.file_name();
            if entry.file_type().is_ok_and(|t| t.is_dir())
                && !skip.iter().any(|s| OsStr::new(s) == name.as_os_str())
            {
                stack.push(entry.path());
            }
        }
    }
    out
}

/// `from` and every folder under it in `repo`'s working tree that git does not ignore, short of
/// a nested git directory; empty once `stopped`.
fn tree_dirs(repo: &Repo, from: &Path, stopped: &AtomicBool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![from.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if stopped.load(Ordering::Relaxed) {
            return Vec::new();
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        out.push(dir);
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_dir()) || entry.file_name() == ".git" {
                continue;
            }
            let path = entry.path();
            let Ok(rel) = path.strip_prefix(&repo.place.top) else { continue };
            if !repo.ignores.lock().ignored(rel, Some(true)) {
                stack.push(path);
            }
        }
    }
    out
}

/// Watch everything `repo` needs watched: its git directories and its whole working tree.
fn cover_all(inner: &Inner, repo: &Repo, stopped: &AtomicBool) -> Result<(), Full> {
    inner.cover(git_dirs(&repo.place))?;
    inner.cover(tree_dirs(repo, &repo.place.top, stopped))?;
    inner.cover(repo.tracked_dirs())
}

/// The thread: watch `repo`'s folders, say it is up, then hear each event until `stop`.
fn read(inner: &Inner, stop: &OwnedFd, stopped: &AtomicBool, repo: &Repo) {
    let top = &repo.place.top;
    if cover_all(inner, repo, stopped).is_err() {
        repo.heard.gave_out(top);
        return;
    }
    if stopped.load(Ordering::Relaxed) {
        return;
    }
    repo.heard.came_up(top);
    let mut buffer = [MaybeUninit::<u8>::uninit(); BUFFER];
    loop {
        let mut fds = [PollFd::new(&inner.fd, PollFlags::IN), PollFd::new(stop, PollFlags::IN)];
        match poll(&mut fds, None) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => return,
        }
        if stopped.load(Ordering::Relaxed) {
            return;
        }
        let mut reader = inotify::Reader::new(&inner.fd, &mut buffer);
        while let Ok(event) = reader.next() {
            let flags = event.events();
            let more = if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
                repo.lost();
                cover_all(inner, repo, stopped)
            } else if flags.contains(ReadFlags::IGNORED) {
                inner.dirs.lock().remove(&event.wd());
                Ok(())
            } else {
                let name = event.file_name().map(|n| OsStr::from_bytes(n.to_bytes()));
                heard(inner, repo, stopped, event.wd(), flags, name)
            };
            if more.is_err() {
                repo.heard.gave_out(top);
                return;
            }
        }
    }
}

/// One event, on the folder of the watch `wd`, about its entry `name` (or itself): the
/// repository is touched if git reports on it, and a folder made, or let back in by a rule, is
/// watched.
fn heard(
    inner: &Inner,
    repo: &Repo,
    stopped: &AtomicBool,
    wd: i32,
    flags: ReadFlags,
    name: Option<&OsStr>,
) -> Result<(), Full> {
    let Some(dir) = inner.dirs.lock().get(&wd).cloned() else { return Ok(()) };
    let place = &repo.place;
    if flags.intersects(ReadFlags::DELETE_SELF | ReadFlags::MOVE_SELF) {
        if dir == place.top || dir == place.git {
            repo.displaced();
        }
        return Ok(());
    }
    let Some(name) = name else { return Ok(()) };
    let path = dir.join(name);
    let is_dir = flags.contains(ReadFlags::ISDIR);
    match repo.heard(&path, Some(is_dir)) {
        Said::Moved if is_dir && flags.intersects(MADE) => {
            if path.starts_with(&place.git) || path.starts_with(&place.common) {
                inner.cover(all_dirs(&path, &NOT_STATUS))
            } else {
                inner.cover(tree_dirs(repo, &path, stopped))
            }
        }
        Said::Nothing | Said::Moved => Ok(()),
        // What is ignored changed: a folder let back in is watched from now on.
        Said::Rules if path.starts_with(&place.common) => {
            inner.cover(tree_dirs(repo, &place.top, stopped))
        }
        Said::Rules => inner.cover(tree_dirs(repo, &dir, stopped)),
    }
}
