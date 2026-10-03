//! The files behind a client's file tiles, followed by the kernel's own events.
//!
//! kqueue(2) on macOS and inotify(7) on Linux: one descriptor per client that the runtime's
//! reactor wakes on, so no thread waits on it and nothing looks at a file that has not moved.
//! Not `FSEvents`, alone or through the `notify` crate: fseventsd delivers an event about
//! 11 ms after the change, where kqueue delivers it in about 0.1 ms, and an `FSEvents` stream
//! carries its whole subtree, so a tile of `~/notes.md` would hear every write under the home
//! folder (docs/decisions/workspace.md, "File tiles follow the disk on the kernel's events").
//!
//! What is watched for a path is the deepest directory of it that exists, the directory its
//! symlink ends in, and on macOS the file itself, since a write to a file does not touch its
//! directory there. A directory watch sees a save that renames a temporary file over the path,
//! a delete, and a file made again, which a watch on the file's own inode would lose. Every
//! event only says "look": a file is looked at once its events have been quiet for [`QUIET`]
//! (or after [`HOLD`] of steady writes), and it is reported only when its stamp moved, so the
//! syscalls of one save are one report. A file whose directory is on a volume that cannot tell
//! this machine of another one's writes (SMB, NFS, FUSE), or that could not be watched at all,
//! is also looked at every [`Limits::poll`].
//!
//! A folder tile's directory is followed the same way ([`follow_folders`]): a watch on the
//! directory itself hears an entry added, removed or renamed, and the one on its parent hears
//! the directory go and come back. It is reported when its own stamp moved, which such a change
//! always does. A write inside one of its files moves no stamp of the folder's, but changes the
//! size its listing shows: inotify's directory watch hears it, and on macOS an `FSEvents` stream
//! over the followed folders does (`kqueue::Queue::follow_contents`), since a kqueue watch on a
//! directory does not. Such a folder is reported after [`CONTENT_HOLD`], at that rate at most.

use std::collections::{BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::sync::{mpsc, watch};

#[cfg(any(target_os = "linux", target_os = "android"))]
mod inotify;
#[cfg(any(target_os = "linux", target_os = "android"))]
use inotify as queue;
#[cfg(target_vendor = "apple")]
mod kqueue;
#[cfg(target_vendor = "apple")]
use kqueue as queue;
#[cfg(target_vendor = "apple")]
use queue::Node;
use queue::{Queue, WatchId};

/// How long a file's events must stop before it is looked at.
///
/// The syscalls of one save (a truncate and its writes, or a temporary file renamed over the
/// path) land microseconds apart, so a look after them sees the save whole and sends it once.
pub const QUIET: Duration = Duration::from_millis(2);
/// The longest a file that keeps changing waits to be looked at.
///
/// Also the shortest time between two reports of one file, and the longest a file just emptied
/// waits for the writes that usually follow a truncate.
pub const HOLD: Duration = Duration::from_millis(50);
/// How long a folder whose entries only changed inside waits to be listed again.
///
/// Also the shortest time between two such listings: sizes can lag this much, where a listing
/// of 2 000 entries every [`HOLD`] while a log grows would be traffic for nothing.
pub const CONTENT_HOLD: Duration = Duration::from_millis(250);

/// What one client's following may cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Kernel watches at most: directories, and on macOS the files themselves. There each is
    /// an open descriptor, and a launchd daemon starts at a soft limit of 256 for all of its
    /// sockets, terminals and files. A path past it is polled.
    pub watches: usize,
    /// How often a path the kernel cannot report on is looked at.
    pub poll: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self { watches: 64, poll: Duration::from_secs(1) }
    }
}

/// How the following stands, after the last watch list was taken.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// Paths followed.
    pub files: usize,
    /// Of those, the ones also looked at every [`Limits::poll`], sorted.
    pub polled: Vec<String>,
    /// Whether the kernel's events are in use at all.
    pub events: bool,
    /// Watch lists taken so far.
    pub lists: u64,
}

/// The paths whose files changed, as the follower reports them. Dropping it ends the follower.
#[derive(Debug)]
pub struct Changes {
    ready: Arc<Mutex<BTreeSet<String>>>,
    bell: mpsc::Receiver<()>,
    status: watch::Receiver<Status>,
}

impl Changes {
    /// The paths that changed since the last call, each once, however often it changed in
    /// between; `None` once the watch lists' sender is gone.
    pub async fn next(&mut self) -> Option<Vec<String>> {
        loop {
            self.bell.recv().await?;
            let paths = std::mem::take(&mut *self.ready.lock());
            if !paths.is_empty() {
                return Some(paths.into_iter().collect());
            }
        }
    }

    /// How the following stands, updated after each list is taken.
    #[must_use]
    pub fn status(&self) -> watch::Receiver<Status> {
        self.status.clone()
    }
}

/// Follow the files `lists` names (`~` is the worker's home).
///
/// Each list replaces the last: a path kept keeps what was last seen of it, and a new one is
/// taken as it is now, since the tile's own read shows that state. Runs on a task of its own
/// until `lists`' sender or the [`Changes`] is dropped.
#[must_use]
pub fn follow(lists: watch::Receiver<Vec<String>>, limits: Limits) -> Changes {
    spawn(lists, limits, Kind::File)
}

/// Follow the directories `lists` names, as [`follow`] does files: a directory is reported when
/// an entry in it is added, removed or renamed, and when it goes or comes back.
#[must_use]
pub fn follow_folders(lists: watch::Receiver<Vec<String>>, limits: Limits) -> Changes {
    spawn(lists, limits, Kind::Folder)
}

fn spawn(lists: watch::Receiver<Vec<String>>, limits: Limits, kind: Kind) -> Changes {
    let ready = Arc::new(Mutex::new(BTreeSet::new()));
    let (ring, bell) = mpsc::channel(1);
    let (told, status) = watch::channel(Status::default());
    let task = Follower { lists, ready: Arc::clone(&ready), ring, told };
    tokio::spawn(tracing::Instrument::in_current_span(task.run(limits, kind)));
    Changes { ready, bell, status }
}

/// What a follower's paths are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Files: a change of content moves one.
    File,
    /// Directories: a change of entries moves one.
    Folder,
}

/// The follower task's ends.
struct Follower {
    lists: watch::Receiver<Vec<String>>,
    ready: Arc<Mutex<BTreeSet<String>>>,
    ring: mpsc::Sender<()>,
    told: watch::Sender<Status>,
}

impl Follower {
    async fn run(mut self, limits: Limits, kind: Kind) {
        let mut state = State::new(limits, kind);
        let reactor = match state
            .queue
            .as_ref()
            .map(|q| AsyncFd::with_interest(q.fd(), Interest::READABLE))
        {
            Some(Ok(fd)) => Some(fd),
            Some(Err(e)) => {
                tracing::info!(error = %e, "file events unavailable, polling");
                state.queue = None;
                None
            }
            None => None,
        };
        let mut poll = tokio::time::interval(limits.poll);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut lists: u64 = 0;
        loop {
            // A path already due (a change the kernel said is whole) is looked at now, not on
            // the timer wheel's next millisecond.
            let now = Instant::now();
            let due = state.due();
            if due.is_some_and(|due| due <= now) {
                let paths = state.take_due(now);
                let Some(back) = self.look(state, paths, now).await else { return };
                state = back;
                continue;
            }
            tokio::select! {
                () = self.ring.closed() => return,
                changed = self.lists.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let paths = self.lists.borrow_and_update().clone();
                    let Some((back, ())) = blocking(state, move |s| s.set(paths)).await else {
                        return;
                    };
                    state = back;
                    lists = lists.saturating_add(1);
                    let status = state.status(lists);
                    tracing::debug!(
                        files = status.files,
                        polled = status.polled.len(),
                        "watch files"
                    );
                    self.told.send_replace(status);
                }
                guard = readable(reactor.as_ref()) => {
                    if let Ok(mut guard) = guard {
                        state.drain(Instant::now());
                        guard.clear_ready();
                    }
                }
                () = sleep_until(due), if due.is_some() => {}
                _ = poll.tick(), if state.polls() => {
                    let polled = state.polled();
                    let Some(back) = self.look(state, polled, Instant::now()).await else {
                        return;
                    };
                    state = back;
                }
            }
        }
    }

    /// Look at `paths` off the runtime and report the ones that moved; `None` once the pool or
    /// the [`Changes`] is gone.
    async fn look(
        &self,
        state: State,
        paths: Vec<(String, Option<Touch>)>,
        now: Instant,
    ) -> Option<State> {
        let (state, moved) = blocking(state, move |s| s.look(paths, now)).await?;
        self.report(moved).then_some(state)
    }

    /// Hand `moved` to the [`Changes`]; `false` once it is gone.
    fn report(&self, moved: Vec<String>) -> bool {
        if moved.is_empty() {
            return true;
        }
        self.ready.lock().extend(moved);
        !matches!(self.ring.try_send(()), Err(mpsc::error::TrySendError::Closed(())))
    }
}

/// Run `step` on the state off the runtime, since a stat or an open on a network volume can
/// block; `None` if the pool is gone.
async fn blocking<T: Send + 'static>(
    mut state: State,
    step: impl FnOnce(&mut State) -> T + Send + 'static,
) -> Option<(State, T)> {
    tokio::task::spawn_blocking(move || {
        let out = step(&mut state);
        (state, out)
    })
    .await
    .ok()
}

async fn readable(
    fd: Option<&AsyncFd<Arc<OwnedFd>>>,
) -> io::Result<tokio::io::unix::AsyncFdReadyGuard<'_, Arc<OwnedFd>>> {
    match fd {
        Some(fd) => fd.readable().await,
        None => std::future::pending().await,
    }
}

async fn sleep_until(due: Option<Instant>) {
    if let Some(due) = due {
        tokio::time::sleep_until(tokio::time::Instant::from_std(due)).await;
    }
}

/// What the kernel said about one watch.
#[derive(Debug)]
enum Hit {
    /// Something in a watched directory changed: the entry named, or any. `done` when the
    /// change is whole (a writer closed the entry, or it was renamed in); `content` when it was
    /// inside the entry, not which entries there are.
    Dir { id: WatchId, name: Option<OsString>, done: bool, content: bool },
    /// A watched directory is gone or moved; what it anchored must be found again.
    DirGone(WatchId),
    /// A watched file changed.
    #[cfg(target_vendor = "apple")]
    Node(WatchId),
    /// Events were lost: look at everything.
    All,
}

/// A watched directory.
#[derive(Debug)]
struct Dir {
    /// Its device and inode, so two paths to one directory share a watch.
    key: (u64, u64),
    /// Its volume cannot report another machine's writes.
    remote: bool,
}

/// A watch the kernel was asked for, as the backend reports it.
#[derive(Debug)]
struct DirWatch {
    id: WatchId,
    remote: bool,
}

/// One path followed.
#[derive(Debug)]
struct Followed {
    /// The path on disk (`~` expanded).
    path: PathBuf,
    /// The file as last seen: at the list that brought it, or at its last report.
    seen: Option<Stamp>,
    /// The directories watched for it, with the entry in each that leads to it; `None` for a
    /// followed directory's own watch, where any entry is a change.
    anchors: Vec<(WatchId, Option<OsString>)>,
    /// The file's own watch, since a write to a file does not touch its directory here.
    #[cfg(target_vendor = "apple")]
    node: Option<Node>,
    /// Also looked at every poll period.
    polled: bool,
    /// When it was last reported.
    reported: Option<Instant>,
}

/// When a path's events came.
#[derive(Debug, Clone, Copy)]
struct Touch {
    first: Instant,
    last: Instant,
    /// Emptied at the last look, so it waits out [`HOLD`] for the writes that follow.
    held: bool,
    /// The last event said the change is whole, so there is nothing to wait for.
    done: bool,
    /// A [`HOLD`] after its last report: a file that keeps changing is sent at that rate.
    not_before: Option<Instant>,
    /// A followed folder whose entries only changed inside: it is due after [`CONTENT_HOLD`],
    /// and reported though its own stamp did not move.
    content: bool,
}

impl Touch {
    fn due(self) -> Instant {
        if self.content {
            let paced = self.not_before.map(|at| later(at, CONTENT_HOLD.saturating_sub(HOLD)));
            let due = later(self.first, CONTENT_HOLD);
            return paced.map_or(due, |at| due.max(at));
        }
        let hold = later(self.first, HOLD);
        let due = if self.held {
            hold
        } else if self.done {
            self.last
        } else {
            later(self.last, QUIET).min(hold)
        };
        self.not_before.map_or(due, |at| due.max(at))
    }
}

fn later(t: Instant, by: Duration) -> Instant {
    t.checked_add(by).unwrap_or(t)
}

/// A file as the disk has it: a change of content, of mode, or a new file at the path each
/// move it. `ctime` cannot be set back, so a copy that keeps the old time and size still moves
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

/// What is at `path` as `kind` follows it, symlinks followed: a file for a file, a directory for
/// a folder; `None` when there is no such thing.
fn stamp(path: &Path, kind: Kind) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.is_dir() != (kind == Kind::Folder) {
        return None;
    }
    Some(Stamp {
        dev: meta.dev(),
        ino: meta.ino(),
        len: meta.len(),
        mtime: (meta.mtime(), meta.mtime_nsec()),
        ctime: (meta.ctime(), meta.ctime_nsec()),
    })
}

/// The follower's state, moved to a blocking thread for each step that touches the disk.
#[derive(Debug)]
struct State {
    queue: Option<Queue>,
    limits: Limits,
    kind: Kind,
    files: HashMap<String, Followed>,
    dirs: HashMap<WatchId, Dir>,
    by_key: HashMap<(u64, u64), WatchId>,
    touched: HashMap<String, Touch>,
}

impl State {
    fn new(limits: Limits, kind: Kind) -> Self {
        let queue = match Queue::open() {
            Ok(queue) => Some(queue),
            Err(e) => {
                tracing::info!(error = %e, "file events unavailable, polling");
                None
            }
        };
        Self {
            queue,
            limits,
            kind,
            files: HashMap::new(),
            dirs: HashMap::new(),
            by_key: HashMap::new(),
            touched: HashMap::new(),
        }
    }

    fn status(&self, lists: u64) -> Status {
        let mut polled: Vec<String> =
            self.files.iter().filter(|(_, f)| f.polled).map(|(k, _)| k.clone()).collect();
        polled.sort();
        Status { files: self.files.len(), polled, events: self.queue.is_some(), lists }
    }

    /// Take a new watch list. A kept path that was polled is watched again if the watches it
    /// lacked are free now, and looked at once, since the poll that would have seen a change
    /// since its last look no longer runs.
    fn set(&mut self, paths: Vec<String>) {
        let keep: BTreeSet<String> = paths.into_iter().collect();
        self.files.retain(|k, _| keep.contains(k));
        self.touched.retain(|k, _| keep.contains(k));
        self.release();
        let polled: Vec<String> =
            self.files.iter().filter(|(_, f)| f.polled).map(|(k, _)| k.clone()).collect();
        for key in &polled {
            self.resolve(key);
        }
        self.touch(polled, Instant::now(), false);
        for key in keep {
            if self.files.contains_key(&key) {
                continue;
            }
            let path = crate::file::expand_home(Path::new(&key));
            self.files.insert(
                key.clone(),
                Followed {
                    path,
                    seen: None,
                    anchors: Vec::new(),
                    #[cfg(target_vendor = "apple")]
                    node: None,
                    polled: false,
                    reported: None,
                },
            );
            // Watched before it is stamped, so a write between the two is an event.
            self.resolve(&key);
            if let Some(file) = self.files.get_mut(&key) {
                file.seen = stamp(&file.path, self.kind);
            }
        }
        self.release();
        self.follow_contents();
    }

    /// Read what the kernel has said, and mark the paths it touches.
    fn drain(&mut self, now: Instant) {
        let Some(queue) = self.queue.as_mut() else { return };
        let mut hits = Vec::new();
        queue.drain(&mut hits);
        for hit in hits {
            match hit {
                Hit::Dir { id, name, done, content } => {
                    let mut inside = Vec::new();
                    let mut touched = Vec::new();
                    for (key, file) in &self.files {
                        for (at, child) in &file.anchors {
                            if *at != id {
                                continue;
                            }
                            match child {
                                // The followed folder's own watch.
                                None if content => inside.push(key.clone()),
                                None => touched.push(key.clone()),
                                Some(child) => {
                                    if name.as_deref().is_none_or(|n| n == child.as_os_str()) {
                                        touched.push(key.clone());
                                    }
                                }
                            }
                        }
                    }
                    inside.retain(|key| !touched.contains(key));
                    self.touch(touched, now, done);
                    self.touch_content(inside, now);
                }
                Hit::DirGone(id) => {
                    self.drop_dir(id);
                    let mut touched = Vec::new();
                    for (key, file) in &mut self.files {
                        let before = file.anchors.len();
                        file.anchors.retain(|(at, _)| *at != id);
                        if file.anchors.len() != before {
                            touched.push(key.clone());
                        }
                    }
                    self.touch(touched, now, false);
                }
                #[cfg(target_vendor = "apple")]
                Hit::Node(id) => {
                    let touched: Vec<String> = self
                        .files
                        .iter()
                        .filter(|(_, f)| f.node.as_ref().is_some_and(|n| n.id() == id))
                        .map(|(k, _)| k.clone())
                        .collect();
                    self.touch(touched, now, false);
                }
                Hit::All => {
                    let touched: Vec<String> = self.files.keys().cloned().collect();
                    self.touch(touched, now, false);
                }
            }
        }
    }

    fn touch(&mut self, keys: Vec<String>, now: Instant, done: bool) {
        for key in keys {
            let not_before =
                self.files.get(&key).and_then(|f| f.reported).map(|at| later(at, HOLD));
            self.touched
                .entry(key)
                .and_modify(|t| {
                    t.last = now;
                    t.held = false;
                    t.done = done;
                    t.content = false;
                })
                .or_insert(Touch {
                    first: now,
                    last: now,
                    held: false,
                    done,
                    not_before,
                    content: false,
                });
        }
    }

    /// Mark followed folders whose entries changed inside; one already due for a change of its
    /// entries stays due as that.
    fn touch_content(&mut self, keys: Vec<String>, now: Instant) {
        for key in keys {
            let not_before =
                self.files.get(&key).and_then(|f| f.reported).map(|at| later(at, HOLD));
            self.touched.entry(key).or_insert(Touch {
                first: now,
                last: now,
                held: false,
                done: false,
                not_before,
                content: true,
            });
        }
    }

    /// When the next touched path is due to be looked at.
    fn due(&self) -> Option<Instant> {
        self.touched.values().map(|t| t.due()).min()
    }

    /// The touched paths due by `now`, with when each was first touched.
    fn take_due(&mut self, now: Instant) -> Vec<(String, Option<Touch>)> {
        let (due, waiting) = std::mem::take(&mut self.touched)
            .into_iter()
            .partition::<HashMap<_, _>, _>(|(_, t)| t.due() <= now);
        self.touched = waiting;
        due.into_iter().map(|(k, t)| (k, Some(t))).collect()
    }

    fn polls(&self) -> bool {
        self.files.values().any(|f| f.polled)
    }

    fn polled(&self) -> Vec<(String, Option<Touch>)> {
        self.files.iter().filter(|(_, f)| f.polled).map(|(k, _)| (k.clone(), None)).collect()
    }

    /// Look at `due` again: watch each anew (its file or directory may be a new one), then
    /// stamp it, and answer the ones that moved. A file just emptied waits out [`HOLD`] from its
    /// first event for the writes that follow a truncate.
    fn look(&mut self, due: Vec<(String, Option<Touch>)>, now: Instant) -> Vec<String> {
        let mut moved = Vec::new();
        for (key, touch) in due {
            self.resolve(&key);
            let Some(file) = self.files.get_mut(&key) else { continue };
            let now_stamp = stamp(&file.path, self.kind);
            let inside = touch.is_some_and(|t| t.content);
            if now_stamp == file.seen && !inside {
                continue;
            }
            let emptied =
                file.seen.is_some_and(|s| s.len > 0) && now_stamp.is_some_and(|s| s.len == 0);
            if let Some(touch) = touch.filter(|t| emptied && !t.done && now < later(t.first, HOLD))
            {
                self.touched.insert(key, Touch { last: now, held: true, ..touch });
                continue;
            }
            file.seen = now_stamp;
            file.reported = Some(now);
            moved.push(key);
        }
        self.release();
        self.follow_contents();
        moved
    }

    /// Have the backend follow the contents of each followed folder that has its own watch.
    fn follow_contents(&mut self) {
        if self.kind != Kind::Folder {
            return;
        }
        let folders: Vec<(PathBuf, WatchId)> = self
            .files
            .values()
            .filter_map(|f| {
                let own = f.anchors.iter().find(|(_, child)| child.is_none())?;
                Some((f.path.clone(), own.0))
            })
            .collect();
        if let Some(queue) = self.queue.as_mut() {
            queue.follow_contents(folders);
        }
    }

    /// Watch what `key`'s path needs now, dropping what it no longer does.
    fn resolve(&mut self, key: &str) {
        let Some(file) = self.files.get(key) else { return };
        let path = file.path.clone();
        let mut polled = self.queue.is_none();
        let mut anchors = Vec::new();
        let own = (self.kind == Kind::Folder && path.is_dir()).then(|| (path.clone(), None));
        let around = anchor_dirs(&path).into_iter().map(|(dir, child)| (dir, Some(child)));
        for (dir, child) in own.into_iter().chain(around) {
            match self.dir(&dir) {
                Ok((id, remote)) => {
                    polled |= remote;
                    if !anchors.iter().any(|(at, c)| *at == id && *c == child) {
                        anchors.push((id, child));
                    }
                }
                Err(e) => {
                    tracing::debug!(dir = %dir.display(), error = %e, "directory not watched");
                    polled = true;
                }
            }
        }
        if anchors.is_empty() {
            polled = true;
        }
        #[cfg(target_vendor = "apple")]
        let node = self.node(key, &path, &mut polled);
        if let Some(file) = self.files.get_mut(key) {
            if polled && !file.polled {
                tracing::info!(path = %path.display(), "file polled, not followed by events");
            }
            file.anchors = anchors;
            #[cfg(target_vendor = "apple")]
            {
                file.node = node;
            }
            file.polled = polled;
        }
    }

    /// The file's own watch: the one it has if the file at the path is still that file, a new
    /// one, or none while there is no file.
    #[cfg(target_vendor = "apple")]
    fn node(&mut self, key: &str, path: &Path, polled: &mut bool) -> Option<Node> {
        let old = self.files.get_mut(key).and_then(|f| f.node.take());
        let meta = std::fs::metadata(path).ok().filter(std::fs::Metadata::is_file)?;
        let at = (meta.dev(), meta.ino());
        if let Some(old) = old.filter(|n| n.key() == at) {
            return Some(old);
        }
        if self.watches() >= self.limits.watches {
            *polled = true;
            return None;
        }
        let queue = self.queue.as_ref()?;
        match queue.add_node(path, at) {
            Ok(node) => Some(node),
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "file not watched");
                *polled = true;
                None
            }
        }
    }

    /// The watch on `dir`, shared with every path that has one on the same directory.
    fn dir(&mut self, dir: &Path) -> io::Result<(WatchId, bool)> {
        let meta = std::fs::metadata(dir)?;
        let key = (meta.dev(), meta.ino());
        if let Some(id) = self.by_key.get(&key) {
            let remote = self.dirs.get(id).is_some_and(|d| d.remote);
            return Ok((*id, remote));
        }
        if self.watches() >= self.limits.watches {
            return Err(io::Error::new(io::ErrorKind::QuotaExceeded, "watch limit reached"));
        }
        let queue = self
            .queue
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "no file events"))?;
        let watch = queue.add_dir(dir)?;
        if let Some(stale) = self.dirs.insert(watch.id, Dir { key, remote: watch.remote }) {
            self.by_key.remove(&stale.key);
        }
        self.by_key.insert(key, watch.id);
        Ok((watch.id, watch.remote))
    }

    /// Kernel watches held.
    fn watches(&self) -> usize {
        #[cfg(target_vendor = "apple")]
        let nodes = self.files.values().filter(|f| f.node.is_some()).count();
        #[cfg(not(target_vendor = "apple"))]
        let nodes = 0;
        self.dirs.len().saturating_add(nodes)
    }

    fn drop_dir(&mut self, id: WatchId) {
        if let Some(dir) = self.dirs.remove(&id) {
            self.by_key.remove(&dir.key);
            if let Some(queue) = self.queue.as_mut() {
                queue.remove_dir(id);
            }
        }
    }

    /// Stop watching the directories no path needs any more.
    fn release(&mut self) {
        let unused: Vec<WatchId> = self
            .dirs
            .keys()
            .copied()
            .filter(|id| !self.files.values().any(|f| f.anchors.iter().any(|(at, _)| at == id)))
            .collect();
        for id in unused {
            self.drop_dir(id);
        }
    }
}

/// The directories to watch for `path`, each with the entry in it that leads to the file: the
/// deepest one that exists (its parent, unless that is gone too), and the one its symlinks end
/// in when that is another.
fn anchor_dirs(path: &Path) -> Vec<(PathBuf, OsString)> {
    let mut anchors = Vec::new();
    let mut child = path.file_name().map(OsStr::to_os_string);
    let mut dir = path.parent();
    while let Some(at) = dir {
        if at.as_os_str().is_empty() {
            break;
        }
        if at.is_dir() {
            if let Some(child) = child {
                anchors.push((at.to_path_buf(), child));
            }
            break;
        }
        child = at.file_name().map(OsStr::to_os_string);
        dir = at.parent();
    }
    if let Ok(target) = std::fs::canonicalize(path)
        && let (Some(parent), Some(name)) = (target.parent(), target.file_name())
    {
        let same = anchors
            .first()
            .and_then(|(dir, _)| std::fs::canonicalize(dir).ok())
            .is_some_and(|dir| dir == parent && path.file_name() == Some(name));
        if !same {
            anchors.push((parent.to_path_buf(), name.to_os_string()));
        }
    }
    anchors
}

/// Raise this process's soft limit on open descriptors as far as it may go, and answer the
/// limit it has now.
///
/// A launchd daemon starts at 256 (`launchctl limit maxfiles`), which its sockets, terminals
/// and, on macOS, every file watch share. setrlimit(2) on macOS refuses a soft limit past
/// `OPEN_MAX` (sys/syslimits.h), whatever the hard one.
///
/// # Errors
///
/// When `setrlimit` refuses the raise; the limit is then as it was.
pub fn raise_descriptor_limit() -> io::Result<u64> {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
    let limit = getrlimit(Resource::Nofile);
    let hard = limit.maximum.unwrap_or(u64::MAX);
    #[cfg(target_vendor = "apple")]
    let want = hard.min(10_240);
    #[cfg(not(target_vendor = "apple"))]
    let want = hard;
    match limit.current {
        None => Ok(u64::MAX),
        Some(now) if now >= want => Ok(now),
        Some(_) => {
            setrlimit(Resource::Nofile, Rlimit { current: Some(want), maximum: limit.maximum })?;
            Ok(want)
        }
    }
}

/// Whether a macOS volume can miss another machine's writes: one not local (SMB, NFS, AFP,
/// `WebDAV`), or a FUSE file system, whose events are only this kernel's own writes.
#[cfg_attr(
    all(not(test), not(target_vendor = "apple")),
    expect(dead_code, reason = "macOS volumes only")
)]
fn apple_remote(local: bool, kind: &str) -> bool {
    !local || ["macfuse", "osxfuse", "fusefs", "fuse"].iter().any(|f| kind.starts_with(f))
}

/// Whether a Linux volume, by its `statfs` magic, can miss writes made elsewhere: network and
/// FUSE file systems, and the host shares of a VM or container.
#[cfg_attr(
    all(not(test), not(any(target_os = "linux", target_os = "android"))),
    expect(dead_code, reason = "Linux volumes only")
)]
fn linux_remote(magic: u64) -> bool {
    const REMOTE: [u64; 14] = [
        0x6969,      // NFS
        0x517b,      // SMB
        0xff53_4d42, // CIFS
        0xfe53_4d42, // SMB2
        0x6573_5546, // FUSE
        0x0102_1997, // 9P
        0x00c3_6400, // Ceph
        0x5346_414f, // AFS
        0x6b41_4653, // kAFS
        0x7375_7245, // Coda
        0x564c,      // NCP
        0x1983_0326, // OrangeFS
        0x6a65_6a63, // virtiofs
        0x786f_4256, // vboxsf
    ];
    REMOTE.contains(&magic)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_network_or_fuse_volume_is_polled_and_a_local_one_is_not() {
        assert!(!apple_remote(true, "apfs"));
        assert!(!apple_remote(true, "hfs"));
        assert!(apple_remote(false, "smbfs"));
        assert!(apple_remote(false, "nfs"));
        assert!(apple_remote(true, "macfuse"));
        assert!(!linux_remote(0xef53)); // ext4
        assert!(!linux_remote(0x9123_683e)); // btrfs
        assert!(!linux_remote(0x794c_7630)); // overlayfs
        assert!(linux_remote(0x6969));
        assert!(linux_remote(0xff53_4d42));
        assert!(linux_remote(0x6573_5546));
    }

    #[test]
    fn the_descriptor_limit_is_raised_and_stays() {
        let raised = raise_descriptor_limit().unwrap();
        assert!(raised > 256, "{raised}");
        assert_eq!(raise_descriptor_limit().unwrap(), raised);
    }

    #[test]
    fn the_nearest_existing_directory_anchors_a_missing_path() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("a/b/file.txt");
        let anchors = anchor_dirs(&path);
        assert_eq!(anchors, vec![(root.path().to_path_buf(), OsString::from("a"))]);
    }

    #[test]
    fn a_symlink_also_anchors_its_target_directory() {
        let root = tempfile::tempdir().unwrap();
        let (here, there) = (root.path().join("here"), root.path().join("there"));
        std::fs::create_dir_all(&here).unwrap();
        std::fs::create_dir_all(&there).unwrap();
        std::fs::write(there.join("real.txt"), "x").unwrap();
        std::os::unix::fs::symlink(there.join("real.txt"), here.join("link.txt")).unwrap();
        let anchors = anchor_dirs(&here.join("link.txt"));
        let target = std::fs::canonicalize(&there).unwrap();
        assert_eq!(
            anchors,
            vec![(here, OsString::from("link.txt")), (target, OsString::from("real.txt"))]
        );
    }

    #[test]
    fn a_touch_is_due_after_quiet_at_the_hold_or_when_whole() {
        let t0 = Instant::now();
        let quiet = Touch {
            first: t0,
            last: t0,
            held: false,
            done: false,
            not_before: None,
            content: false,
        };
        assert_eq!(quiet.due(), later(t0, QUIET));
        let steady = Touch { last: later(t0, HOLD), ..quiet };
        assert_eq!(steady.due(), later(t0, HOLD));
        let held = Touch { held: true, ..quiet };
        assert_eq!(held.due(), later(t0, HOLD));
        let done = Touch { done: true, ..quiet };
        assert_eq!(done.due(), t0);
        let soon_after_a_report = Touch { not_before: Some(later(t0, HOLD)), ..done };
        assert_eq!(soon_after_a_report.due(), later(t0, HOLD));
        let inside = Touch { content: true, ..quiet };
        assert_eq!(inside.due(), later(t0, CONTENT_HOLD), "a size waits");
        let inside_again = Touch { not_before: Some(later(t0, HOLD * 2)), ..inside };
        assert_eq!(inside_again.due(), later(t0, HOLD + CONTENT_HOLD), "paced from the report");
    }
}
