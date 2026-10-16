//! The repositories behind a client's git status and diffs, followed on the file system's own
//! events, so the client asks git again only when something git reports on changed.
//!
//! A client names folders (`ClientMsg::WatchRepos`); each is resolved to its worktree's top, its
//! git directory and the common one a linked worktree shares. What is heard:
//!
//! - In the working tree, any file or folder written, made, removed, renamed or given a new mode,
//!   however deep and whether or not a folder of it is open anywhere, unless git ignores it
//!   ([`Ignores`]: the folders' `.gitignore` files, `info/exclude` and the global excludes, as git
//!   reads them) and git does not track it anyway. A `target/` or `node_modules/` churning costs a
//!   match, and no report.
//! - In the git directory, only what moves a status or a diff against `HEAD`: `HEAD`, the index,
//!   `packed-refs`, the branches (`refs/heads`, `refs/remotes`), a merge's or a pick's head,
//!   `info/exclude` and a submodule's `HEAD` or index. Objects, logs, locks and the worker's own
//!   thread snapshots (`refs/slopty`) are not, so a status or a diff asked because of a change
//!   never causes another.
//!
//! On macOS one `FSEvents` stream covers each repository however deep (`fsevents`); on Linux an
//! inotify watch on each directory git does not ignore, and on the git directory's
//! (`inotify`). A burst of events is one report: a repository is reported once its events have
//! been quiet for [`QUIET`], or after [`HOLD`] of steady changes, and two reports of one are
//! [`HOLD`] apart at least. It is also reported once when its watch is up, since what changed
//! before then is not known.
//!
//! Where events cannot say everything (a network or FUSE volume, `FSEvents` refusing a stream,
//! inotify past its watches, [`Limits::watches`] or the kernel's, another system), the repository
//! is looked at every [`POLL`] instead: its `git status` and the stamps of the files it names,
//! reported only when they moved, so a client still asks nothing while nothing changes.
//!
//! A repository is where its folders are: its worktree's top and its git directory, each by
//! path and by inode. One deleted, renamed or replaced (`rm -rf .git && git init`, a clone moved
//! in) is reported, then found again from the folders named and followed afresh, or, when no
//! repository is there any more, its folders are watched for one made there.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use parking_lot::Mutex;
use tokio::sync::{Notify, mpsc, watch};

#[cfg(target_os = "macos")]
#[path = "repowatch/fsevents.rs"]
mod backend;
#[cfg(target_os = "linux")]
#[path = "repowatch/inotify.rs"]
mod backend;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[path = "repowatch/none.rs"]
mod backend;

/// How long a repository's events must stop before it is reported.
///
/// A save, a `git add` or a checkout of a few files is one report; the status and the diff the
/// report brings then see it whole.
pub const QUIET: Duration = Duration::from_millis(30);
/// The longest a repository that keeps changing waits to be reported, and the shortest time
/// between two reports of it.
///
/// A build writing into a tracked folder asks for at most five statuses a second, and the
/// client folds those it is still answering into one.
pub const HOLD: Duration = Duration::from_millis(200);
/// How often a repository the events cannot cover is looked at.
pub const POLL: Duration = Duration::from_secs(3);

/// What one client's following may cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The kernel watches one repository may hold: a folder each for inotify, one stream for
    /// `FSEvents`. A repository that needs more is looked at every [`POLL`] instead.
    pub watches: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { watches: 65_536 }
    }
}

/// How the following stands, after the last list was taken.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// Lists taken so far.
    pub lists: u64,
    /// The folders of the last list that are in a repository, sorted.
    pub followed: Vec<String>,
    /// The folders of the last list in no repository, sorted: each is watched for one to be
    /// made there.
    pub outside: Vec<String>,
    /// The tops of the repositories followed, sorted.
    pub repos: Vec<String>,
    /// Of those, the ones looked at every [`POLL`] instead of followed by events, sorted.
    pub polled: Vec<String>,
    /// Of those, the ones whose watch is up or which are looked at, sorted: a change before then
    /// is in the report a watch makes as it comes up.
    pub up: Vec<String>,
}

/// The folders whose repositories moved, as the follower reports them. Dropping it ends the
/// follower.
#[derive(Debug)]
pub struct Moves {
    ready: Arc<Mutex<BTreeSet<String>>>,
    bell: mpsc::Receiver<()>,
    status: watch::Receiver<Status>,
}

impl Moves {
    /// The folders whose repositories moved since the last call, each once, as the list named
    /// them; `None` once the lists' sender is gone.
    pub async fn next(&mut self) -> Option<Vec<String>> {
        loop {
            self.bell.recv().await?;
            let named = std::mem::take(&mut *self.ready.lock());
            if !named.is_empty() {
                return Some(named.into_iter().collect());
            }
        }
    }

    /// How the following stands, updated after each list is taken.
    #[must_use]
    pub fn status(&self) -> watch::Receiver<Status> {
        self.status.clone()
    }
}

/// Follow the repositories of the folders `lists` names (absolute, or `~/…`), within the
/// default [`Limits`].
///
/// Each list replaces the last: a repository kept keeps its watch, and one no folder names any
/// more is let go. Runs on a task of its own until `lists`' sender or the [`Moves`] is dropped.
#[must_use]
pub fn follow(lists: watch::Receiver<Vec<String>>) -> Moves {
    follow_within(lists, Limits::default())
}

/// [`follow`], within `limits`.
#[must_use]
pub fn follow_within(lists: watch::Receiver<Vec<String>>, limits: Limits) -> Moves {
    let ready = Arc::new(Mutex::new(BTreeSet::new()));
    let (ring, bell) = mpsc::channel(1);
    let (told, status) = watch::channel(Status::default());
    let (outside, outside_rx) = watch::channel(Vec::new());
    let follower = Follower {
        lists,
        ready: Arc::clone(&ready),
        ring,
        told,
        heard: Arc::default(),
        repos: HashMap::new(),
        named: BTreeMap::new(),
        outside,
        made: crate::fswatch::follow_folders(outside_rx, crate::fswatch::Limits::default()),
        taken: 0,
        limits,
    };
    tokio::spawn(tracing::Instrument::in_current_span(follower.run()));
    Moves { ready, bell, status }
}

/// Where a repository is: its worktree's top, its git directory, and the directory a linked
/// worktree shares with the others (the git directory itself for any other); all canonical.
/// The top and the git directory are known by their inodes too, so one replaced at its path is
/// another place.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Place {
    top: PathBuf,
    git: PathBuf,
    common: PathBuf,
    inodes: [Inode; 2],
}

/// A file's device and inode.
type Inode = (u64, u64);

fn inode(path: &Path) -> Option<Inode> {
    std::fs::metadata(path).ok().map(|meta| (meta.dev(), meta.ino()))
}

impl Place {
    /// Whether the top and the git directory are still the ones followed.
    fn stands(&self) -> bool {
        [inode(&self.top), inode(&self.git)] == self.inodes.map(Some)
    }
}

/// The repository the folder `named` is in, if any.
fn place_of(named: &str) -> Option<Place> {
    let dir = std::fs::canonicalize(crate::file::expand_home(Path::new(named))).ok()?;
    let top = crate::find::worktree_of(&dir)?;
    let dot = top.join(".git");
    let git = if dot.is_dir() {
        dot
    } else {
        let said = std::fs::read_to_string(&dot).ok()?;
        let at = said.lines().find_map(|line| line.strip_prefix("gitdir:"))?.trim();
        std::fs::canonicalize(top.join(at)).ok()?
    };
    let common = std::fs::read_to_string(git.join("commondir"))
        .ok()
        .and_then(|at| std::fs::canonicalize(git.join(at.trim())).ok())
        .unwrap_or_else(|| git.clone());
    let inodes = [inode(&top)?, inode(&git)?];
    Some(Place { top, git, common, inodes })
}

/// What git ignores in a working tree, as it reads its rules: the deepest `.gitignore` first,
/// then `info/exclude`, then the global excludes; a folder ignored ignores everything in it.
#[derive(Debug)]
struct Ignores {
    top: PathBuf,
    /// `info/exclude`.
    exclude: Gitignore,
    /// `core.excludesFile`, or git's default for it.
    global: Gitignore,
    /// Each folder's `.gitignore` read so far, by its path from the top (`""` the top).
    dirs: HashMap<PathBuf, Gitignore>,
    /// Tracked files the rules would ignore, from the top: git reports on them all the same.
    tracked: HashSet<PathBuf>,
}

impl Ignores {
    fn new(place: &Place) -> Self {
        let (global, _unreadable) = GitignoreBuilder::new(&place.top).build_global();
        let mut ignores = Self {
            top: place.top.clone(),
            exclude: Gitignore::empty(),
            global,
            dirs: HashMap::new(),
            tracked: HashSet::new(),
        };
        ignores.read_exclude(&place.common);
        ignores
    }

    /// Read `info/exclude` of the git directory `common` again.
    fn read_exclude(&mut self, common: &Path) {
        let mut exclude = GitignoreBuilder::new(&self.top);
        let _unreadable = exclude.add(common.join("info").join("exclude"));
        self.exclude = exclude.build().unwrap_or_else(|_| Gitignore::empty());
    }

    /// The folder `dir` (from the top) has a new `.gitignore`, or none.
    fn forget(&mut self, dir: &Path) {
        self.dirs.remove(dir);
    }

    /// Whether git ignores `rel` (from the top), `dir` when it is a folder; looked at on disk
    /// when not known.
    fn ignored(&mut self, rel: &Path, dir: Option<bool>) -> bool {
        if self.tracked.contains(rel) {
            return false;
        }
        let names: Vec<&OsStr> = rel.iter().collect();
        let mut path = PathBuf::new();
        for (at, name) in names.iter().enumerate() {
            let parent = path.clone();
            path.push(name);
            let last = at.saturating_add(1) == names.len();
            let is_dir =
                if last { dir.unwrap_or_else(|| self.top.join(&path).is_dir()) } else { true };
            if self.excluded(&parent, &path, is_dir) {
                return true;
            }
        }
        false
    }

    /// Whether the rules exclude `path` (from the top), whose folder is `parent`.
    fn excluded(&mut self, parent: &Path, path: &Path, is_dir: bool) -> bool {
        let full = self.top.join(path);
        let mut at = Some(parent);
        while let Some(dir) = at {
            match self.folder(dir).matched(&full, is_dir) {
                Match::Ignore(_) => return true,
                Match::Whitelist(_) => return false,
                Match::None => {}
            }
            at = dir.parent();
        }
        for rules in [&self.exclude, &self.global] {
            match rules.matched(&full, is_dir) {
                Match::Ignore(_) => return true,
                Match::Whitelist(_) => return false,
                Match::None => {}
            }
        }
        false
    }

    /// The rules of the folder `dir`'s own `.gitignore`, read once.
    fn folder(&mut self, dir: &Path) -> &Gitignore {
        let top = &self.top;
        self.dirs
            .entry(dir.to_path_buf())
            .or_insert_with(|| Gitignore::new(top.join(dir).join(".gitignore")).0)
    }
}

/// One repository followed: where it is, what it ignores, and who hears of it.
#[derive(Debug)]
struct Repo {
    place: Place,
    ignores: Mutex<Ignores>,
    /// The index moved since the tracked files git would ignore were last read.
    index_moved: AtomicBool,
    heard: Arc<Heard>,
    /// How its watch takes more folders, where it needs them named (inotify's, one a folder).
    cover: Mutex<Option<Cover>>,
    /// Its top or git directory was deleted or moved away, as its watch saw it. The inodes
    /// alone do not say so where the file system hands a freed inode to the next folder made,
    /// as Linux's do: `rm -rf .git && git init` can make a git directory with the old one's.
    displaced: AtomicBool,
}

/// Has a repository's watch follow the folders named too.
struct Cover(Box<CoverFolders>);

/// What a [`Cover`] calls with the folders to follow.
type CoverFolders = dyn Fn(&[PathBuf]) + Send + Sync;

impl std::fmt::Debug for Cover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cover")
    }
}

/// What the file system said of a path under a repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Said {
    /// Nothing git reports on.
    Nothing,
    /// Something git reports on changed.
    Moved,
    /// A `.gitignore` or `info/exclude` changed: what is ignored may be something else now.
    Rules,
}

impl Repo {
    fn new(place: Place, heard: Arc<Heard>) -> Self {
        let ignores = Mutex::new(Ignores::new(&place));
        let (index_moved, cover, displaced) =
            (AtomicBool::new(true), Mutex::new(None), AtomicBool::new(false));
        Self { place, ignores, index_moved, heard, cover, displaced }
    }

    /// Whether it is still the repository followed: its top and git directory neither seen
    /// going nor replaced at their paths.
    fn stands(&self) -> bool {
        !self.displaced.load(Ordering::Relaxed) && self.place.stands()
    }

    /// The file system says something happened at `path` (a folder when `dir` says so): the
    /// repository is touched when git reports on it.
    fn heard(&self, path: &Path, dir: Option<bool>) -> Said {
        let said = self.said(path, dir);
        if said != Said::Nothing {
            self.heard.touch(&self.place.top, false);
        }
        said
    }

    /// Events were lost, or the repository itself moved: what git says may be anything now.
    fn lost(&self) {
        self.heard.touch(&self.place.top, false);
    }

    /// Its top or git directory went: it is said, then found again from the folders named.
    fn displaced(&self) {
        self.displaced.store(true, Ordering::Relaxed);
        self.lost();
    }

    /// What `path` changing is to the repository's status and diffs.
    fn said(&self, path: &Path, dir: Option<bool>) -> Said {
        let place = &self.place;
        if let Ok(rel) = path.strip_prefix(&place.git) {
            return self.in_git(rel, false);
        }
        if place.common != place.git
            && let Ok(rel) = path.strip_prefix(&place.common)
        {
            return self.in_git(rel, true);
        }
        let Ok(rel) = path.strip_prefix(&place.top) else { return Said::Nothing };
        // A linked worktree's own `.git` file, which says where its git directory is.
        if rel == Path::new(".git") {
            return Said::Moved;
        }
        // A submodule's or a nested repository's own git file or folder.
        if rel.iter().any(|name| name == ".git") {
            return Said::Nothing;
        }
        if rel.as_os_str().is_empty() {
            return Said::Moved;
        }
        let rules = rel.file_name() == Some(OsStr::new(".gitignore"));
        let ignored = {
            let mut ignores = self.ignores.lock();
            if rules {
                ignores.forget(rel.parent().unwrap_or_else(|| Path::new("")));
            }
            ignores.ignored(rel, dir)
        };
        match (ignored, rules) {
            (true, _) => Said::Nothing,
            (false, true) => Said::Rules,
            (false, false) => Said::Moved,
        }
    }

    /// What `rel` changing in the git directory is (in the common one when `shared`): only
    /// what moves a status or a diff against `HEAD` counts.
    fn in_git(&self, rel: &Path, shared: bool) -> Said {
        let names: Vec<&str> = rel.iter().filter_map(OsStr::to_str).collect();
        if names.last().is_some_and(|name| Path::new(name).extension() == Some(OsStr::new("lock")))
        {
            return Said::Nothing;
        }
        match names.as_slice() {
            ["index"] if !shared => {
                self.index_moved.store(true, Ordering::Relaxed);
                Said::Moved
            }
            ["HEAD" | "MERGE_HEAD" | "CHERRY_PICK_HEAD" | "REVERT_HEAD" | "REBASE_HEAD"]
                if !shared =>
            {
                Said::Moved
            }
            // The git directory itself made, removed or renamed (the follower then looks again
            // at where the repository is), packed refs, a branch, a submodule's head or index.
            []
            | ["packed-refs"]
            | ["refs", "heads" | "remotes", _, ..]
            | ["modules", .., "HEAD" | "index"] => Said::Moved,
            ["info", "exclude"] => {
                self.ignores.lock().read_exclude(&self.place.common);
                Said::Rules
            }
            _ => Said::Nothing,
        }
    }

    /// Read again which tracked files the rules would ignore, when the index moved since;
    /// whether it did.
    async fn read_tracked(&self) -> bool {
        if !self.index_moved.swap(false, Ordering::Relaxed) {
            return false;
        }
        let Some(git) = crate::changes::git() else { return false };
        let args = ["ls-files", "-z", "--cached", "--ignored", "--exclude-standard"];
        let Some(listed) = crate::changes::run_git(git, &self.place.top, &args).await else {
            return false;
        };
        let tracked: HashSet<PathBuf> =
            listed.split('\0').filter(|path| !path.is_empty()).map(PathBuf::from).collect();
        self.ignores.lock().tracked = tracked;
        true
    }

    /// The folders of the tracked files the rules would ignore, which inotify must watch too.
    fn tracked_dirs(&self) -> Vec<PathBuf> {
        let dirs: BTreeSet<PathBuf> = self
            .ignores
            .lock()
            .tracked
            .iter()
            .filter_map(|rel| rel.parent())
            .map(|dir| self.place.top.join(dir))
            .collect();
        dirs.into_iter().collect()
    }
}

/// What every repository's events said since the follower last looked, and its bell.
#[derive(Debug, Default)]
struct Heard {
    bursts: Mutex<HashMap<PathBuf, Burst>>,
    /// Repositories whose events gave out: they are looked at every [`POLL`] from now on.
    gave_out: Mutex<Vec<PathBuf>>,
    /// Repositories whose watch came up.
    came_up: Mutex<Vec<PathBuf>>,
    bell: Notify,
}

/// When a repository's events came.
#[derive(Debug, Clone, Copy)]
struct Burst {
    first: Instant,
    last: Instant,
    /// Due at once: its watch just came up, or a look found it moved.
    now: bool,
}

impl Heard {
    /// The repository at `top` was touched; due at once when `now`.
    fn touch(&self, top: &Path, now: bool) {
        let at = Instant::now();
        let mut bursts = self.bursts.lock();
        if let Some(burst) = bursts.get_mut(top) {
            burst.last = at;
            burst.now |= now;
        } else {
            bursts.insert(top.to_path_buf(), Burst { first: at, last: at, now });
        }
        drop(bursts);
        self.bell.notify_one();
    }

    /// The events of the repository at `top` gave out.
    fn gave_out(&self, top: &Path) {
        self.gave_out.lock().push(top.to_path_buf());
        self.bell.notify_one();
    }

    /// The watch of the repository at `top` is up: it is reported at once, since what changed
    /// before is not known.
    fn came_up(&self, top: &Path) {
        self.came_up.lock().push(top.to_path_buf());
        self.touch(top, true);
    }
}

impl Burst {
    /// When it is due, its repository last reported at `reported`.
    fn due(self, reported: Option<Instant>) -> Instant {
        let due = if self.now {
            self.first
        } else {
            later(self.last, QUIET).min(later(self.first, HOLD))
        };
        reported.map_or(due, |at| due.max(later(at, HOLD)))
    }
}

fn later(t: Instant, by: Duration) -> Instant {
    t.checked_add(by).unwrap_or(t)
}

/// How a repository is followed.
#[derive(Debug)]
enum Source {
    /// On the file system's events; the watch ends when dropped.
    Events {
        /// Its watch, ended when dropped.
        _watch: backend::Watch,
    },
    /// Looked at every [`POLL`]: what the last look saw.
    Polled(Option<u64>),
}

/// A repository followed for the folders that name it.
#[derive(Debug)]
struct Followed {
    repo: Arc<Repo>,
    source: Source,
    reported: Option<Instant>,
    /// Its watch is up, or it is looked at.
    up: bool,
}

/// The follower task's state.
struct Follower {
    lists: watch::Receiver<Vec<String>>,
    ready: Arc<Mutex<BTreeSet<String>>>,
    ring: mpsc::Sender<()>,
    told: watch::Sender<Status>,
    heard: Arc<Heard>,
    /// By top.
    repos: HashMap<PathBuf, Followed>,
    /// Each folder of the list, with the top of its repository, if any.
    named: BTreeMap<String, Option<PathBuf>>,
    /// The folders in no repository, followed so a repository made there is noticed.
    outside: watch::Sender<Vec<String>>,
    made: crate::fswatch::Changes,
    taken: u64,
    limits: Limits,
}

impl Follower {
    async fn run(mut self) {
        let mut poll = tokio::time::interval(POLL);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        poll.reset();
        loop {
            let due = self.due();
            if due.is_some_and(|due| due <= Instant::now()) {
                if !self.report().await {
                    return;
                }
                continue;
            }
            tokio::select! {
                () = self.ring.closed() => return,
                changed = self.lists.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let list = self.lists.borrow_and_update().clone();
                    self.take(list).await;
                }
                () = self.heard.bell.notified() => self.news(),
                () = sleep_until(due), if due.is_some() => {}
                made = self.made.next() => {
                    if made.is_some() {
                        let list = self.lists.borrow().clone();
                        self.take(list).await;
                    }
                }
                _ = poll.tick(), if self.polls() => self.look().await,
            }
        }
    }

    /// Take a new list: follow the repositories it names that are not yet, and let go of those
    /// it no longer names.
    async fn take(&mut self, list: Vec<String>) {
        let named: BTreeSet<String> = list.into_iter().collect();
        let resolved = tokio::task::spawn_blocking(move || {
            named.into_iter().map(|n| (n.clone(), place_of(&n))).collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        let places: HashMap<PathBuf, Place> =
            resolved.iter().filter_map(|(_, p)| p.clone()).map(|p| (p.top.clone(), p)).collect();
        self.repos.retain(|top, followed| {
            places.get(top).is_some_and(|place| *place == followed.repo.place)
                && !followed.repo.displaced.load(Ordering::Relaxed)
        });
        self.heard.bursts.lock().retain(|top, _| places.contains_key(top));
        for (top, place) in places {
            if self.repos.contains_key(&top) {
                continue;
            }
            let repo = Arc::new(Repo::new(place, Arc::clone(&self.heard)));
            let source = start(&repo, self.limits).await;
            tracing::debug!(top = %top.display(), events = matches!(source, Source::Events { .. }), "repository followed");
            tokio::spawn(read_tracked(Arc::clone(&repo)));
            let up = matches!(source, Source::Polled(_));
            self.repos.insert(top, Followed { repo, source, reported: None, up });
        }
        self.named = resolved.into_iter().map(|(n, p)| (n, p.map(|p| p.top))).collect();
        let outside: Vec<String> =
            self.named.iter().filter(|(_, top)| top.is_none()).map(|(n, _)| n.clone()).collect();
        self.outside.send_if_modified(|was| {
            let moved = *was != outside;
            if moved {
                was.clone_from(&outside);
            }
            moved
        });
        self.taken = self.taken.saturating_add(1);
        self.told.send_replace(self.status());
    }

    fn status(&self) -> Status {
        let followed = self.named.iter().filter(|(_, t)| t.is_some()).map(|(n, _)| n.clone());
        let outside = self.named.iter().filter(|(_, t)| t.is_none()).map(|(n, _)| n.clone());
        let mut repos: Vec<String> =
            self.repos.keys().map(|top| top.to_string_lossy().into_owned()).collect();
        repos.sort();
        let mut polled: Vec<String> = self
            .repos
            .iter()
            .filter(|(_, f)| matches!(f.source, Source::Polled(_)))
            .map(|(top, _)| top.to_string_lossy().into_owned())
            .collect();
        polled.sort();
        let mut up: Vec<String> = self
            .repos
            .iter()
            .filter(|(_, f)| f.up)
            .map(|(top, _)| top.to_string_lossy().into_owned())
            .collect();
        up.sort();
        Status {
            lists: self.taken,
            followed: followed.collect(),
            outside: outside.collect(),
            repos,
            polled,
            up,
        }
    }

    /// When the next repository touched is due.
    fn due(&self) -> Option<Instant> {
        let bursts = self.heard.bursts.lock();
        bursts
            .iter()
            .map(|(top, burst)| burst.due(self.repos.get(top).and_then(|f| f.reported)))
            .min()
    }

    /// Report the repositories due by now; `false` once the [`Moves`] is gone.
    async fn report(&mut self) -> bool {
        let now = Instant::now();
        let due: Vec<PathBuf> = {
            let mut bursts = self.heard.bursts.lock();
            let due: Vec<PathBuf> = bursts
                .iter()
                .filter(|(top, burst)| {
                    burst.due(self.repos.get(*top).and_then(|f| f.reported)) <= now
                })
                .map(|(top, _)| top.clone())
                .collect();
            for top in &due {
                bursts.remove(top);
            }
            due
        };
        let mut gone = false;
        let mut moved = Vec::new();
        for top in &due {
            let Some(followed) = self.repos.get_mut(top) else { continue };
            followed.reported = Some(now);
            tokio::spawn(read_tracked(Arc::clone(&followed.repo)));
            gone |= !followed.repo.stands();
            moved.extend(
                self.named.iter().filter(|(_, t)| t.as_ref() == Some(top)).map(|(n, _)| n.clone()),
            );
        }
        // A repository deleted, moved or replaced, or its folder no repository any more: its
        // folders are looked at again, a repository there followed afresh, and a folder in none
        // followed for one made there.
        if gone {
            tracing::debug!(?due, "a repository's place moved: its folders are looked at again");
            let list = self.lists.borrow().clone();
            self.take(list).await;
        }
        if moved.is_empty() {
            return true;
        }
        tracing::trace!(?moved, "repositories moved");
        self.ready.lock().extend(moved);
        !matches!(self.ring.try_send(()), Err(mpsc::error::TrySendError::Closed(())))
    }

    /// Take what the watches said of themselves: the ones up, and the ones whose events gave
    /// out, which are looked at every [`POLL`] from now on.
    fn news(&mut self) {
        let came_up = std::mem::take(&mut *self.heard.came_up.lock());
        let gave_out = std::mem::take(&mut *self.heard.gave_out.lock());
        if came_up.is_empty() && gave_out.is_empty() {
            return;
        }
        for top in came_up {
            if let Some(followed) = self.repos.get_mut(&top) {
                followed.up = true;
            }
        }
        for top in gave_out {
            if let Some(followed) = self.repos.get_mut(&top) {
                tracing::info!(top = %top.display(), "repository's events gave out: it is looked at every few seconds");
                followed.source = Source::Polled(None);
                followed.up = true;
                self.heard.touch(&top, true);
            }
        }
        self.told.send_replace(self.status());
    }

    fn polls(&self) -> bool {
        self.repos.values().any(|f| matches!(f.source, Source::Polled(_)))
    }

    /// Look at each repository followed without events, and touch the ones that moved.
    async fn look(&mut self) {
        for followed in self.repos.values_mut() {
            let Source::Polled(seen) = &mut followed.source else { continue };
            let now = print(&followed.repo.place.top).await;
            if *seen != now {
                let first = seen.is_none();
                *seen = now;
                if !first {
                    self.heard.touch(&followed.repo.place.top, true);
                }
            }
        }
    }
}

/// Read the tracked files `repo`'s rules would ignore, and have its watch follow their folders
/// where it needs them named.
async fn read_tracked(repo: Arc<Repo>) {
    if !repo.read_tracked().await {
        return;
    }
    let dirs = repo.tracked_dirs();
    if !dirs.is_empty()
        && let Some(cover) = repo.cover.lock().as_ref()
    {
        (cover.0)(&dirs);
    }
}

/// Follow `repo` on events, or by looks where they cannot say everything. Either way it is
/// touched once it is followed.
async fn start(repo: &Arc<Repo>, limits: Limits) -> Source {
    let top = repo.place.top.clone();
    let remote = {
        let at = top.clone();
        tokio::task::spawn_blocking(move || crate::fswatch::remote_volume(&at)).await
    };
    if remote.is_ok_and(|remote| !remote)
        && let Some(watch) = backend::Watch::start(Arc::clone(repo), limits.watches)
    {
        return Source::Events { _watch: watch };
    }
    let seen = print(&top).await;
    repo.heard.touch(&top, true);
    Source::Polled(seen)
}

/// What a look at a repository sees: its `git status`, and the stamp of each file it names,
/// since a file edited again stays modified with nothing else to show it. `None` with no git
/// or no repository.
async fn print(top: &Path) -> Option<u64> {
    let git = crate::changes::git()?;
    let args = ["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"];
    let said = crate::changes::run_git(git, top, &args).await?;
    let at = top.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut hasher = DefaultHasher::new();
        said.hash(&mut hasher);
        for path in crate::changes::status_paths(&said) {
            if let Ok(meta) = std::fs::symlink_metadata(at.join(path)) {
                (meta.len(), meta.mtime(), meta.mtime_nsec(), meta.ctime_nsec()).hash(&mut hasher);
            }
        }
        hasher.finish()
    })
    .await
    .ok()
}

async fn sleep_until(due: Option<Instant>) {
    if let Some(due) = due {
        tokio::time::sleep_until(tokio::time::Instant::from_std(due)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(top: &Path) -> Place {
        let (git, inodes) = (top.join(".git"), [(0, 0); 2]);
        Place { top: top.to_path_buf(), common: git.clone(), git, inodes }
    }

    /// git's rules as git reads them: a folder's own file first and deepest, a `!` taking a
    /// file back, a folder ignored ignoring all in it, `info/exclude` last; a tracked file is
    /// never ignored.
    #[test]
    fn the_rules_ignore_what_git_ignores() {
        let root = tempfile::tempdir().unwrap();
        let top = root.path();
        std::fs::create_dir_all(top.join(".git/info")).unwrap();
        std::fs::create_dir_all(top.join("src/gen")).unwrap();
        std::fs::create_dir_all(top.join("target/debug")).unwrap();
        std::fs::write(top.join(".gitignore"), "target/\n*.log\n!keep.log\n").unwrap();
        std::fs::write(top.join("src/.gitignore"), "gen/\n").unwrap();
        std::fs::write(top.join(".git/info/exclude"), "scratch.txt\n").unwrap();
        let mut ignores = Ignores::new(&place(top));
        let ignored = |ignores: &mut Ignores, rel: &str| ignores.ignored(Path::new(rel), None);
        assert!(ignored(&mut ignores, "target"));
        assert!(ignored(&mut ignores, "target/debug/build.rs"), "inside an ignored folder");
        assert!(ignored(&mut ignores, "server.log"));
        assert!(!ignored(&mut ignores, "keep.log"), "taken back");
        assert!(ignored(&mut ignores, "src/gen/out.rs"), "the folder's own rules");
        assert!(!ignored(&mut ignores, "src/lib.rs"));
        assert!(!ignored(&mut ignores, "gen/top.rs"), "a folder's rules stay in it");
        assert!(ignored(&mut ignores, "scratch.txt"), "info/exclude");
        ignores.tracked.insert(PathBuf::from("server.log"));
        assert!(!ignored(&mut ignores, "server.log"), "tracked all the same");
    }

    /// In the git directory only what moves a status or a diff counts: objects, logs, locks and
    /// the worker's own snapshots never bring a report.
    #[test]
    fn in_the_git_directory_only_what_moves_a_status_counts() {
        let root = tempfile::tempdir().unwrap();
        let top = root.path();
        let repo = Repo::new(place(top), Arc::default());
        let said = |rel: &str| repo.said(&top.join(rel), Some(false));
        for moved in [
            ".git/HEAD",
            ".git/index",
            ".git/packed-refs",
            ".git/refs/heads/main",
            ".git/refs/heads/feature/rows",
            ".git/refs/remotes/origin/main",
            ".git/MERGE_HEAD",
            ".git/modules/vendor/lib/HEAD",
            ".git",
            "src/lib.rs",
            ".github/workflows/ci.yml",
        ] {
            assert_eq!(said(moved), Said::Moved, "{moved}");
        }
        for nothing in [
            ".git/index.lock",
            ".git/objects/ab/cdef",
            ".git/logs/HEAD",
            ".git/refs/heads/main.lock",
            ".git/refs/slopty/threads/abc",
            ".git/FETCH_HEAD",
            ".git/ORIG_HEAD",
            "vendor/lib/.git",
        ] {
            assert_eq!(said(nothing), Said::Nothing, "{nothing}");
        }
        assert_eq!(said(".gitignore"), Said::Rules);
    }

    #[test]
    fn a_burst_is_due_after_quiet_at_the_hold_and_paced_after_a_report() {
        let t0 = Instant::now();
        let quiet = Burst { first: t0, last: t0, now: false };
        assert_eq!(quiet.due(None), later(t0, QUIET));
        let steady = Burst { last: later(t0, HOLD), ..quiet };
        assert_eq!(steady.due(None), later(t0, HOLD));
        let up = Burst { now: true, ..quiet };
        assert_eq!(up.due(None), t0);
        assert_eq!(up.due(Some(t0)), later(t0, HOLD), "two reports are a hold apart");
    }
}
