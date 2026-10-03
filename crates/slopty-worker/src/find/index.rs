//! One worktree's paths in memory for quick open, and the worker's indexes by worktree.
//!
//! An index is built on the first query of its worktree by one parallel walk, then caught up
//! before each query with what changed since: the directories the file system's events named
//! ([`super::watch`]), or, with no events, the directories whose modification time moved. A
//! directory is caught up by listing it alone and walking only what is new in it, so a file an
//! agent writes costs one directory's listing at the next keystroke. A change to an ignore file,
//! or events the system dropped, walk the worktree again.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, OnceLock, mpsc};
use std::time::{Duration, Instant, SystemTime};

use parking_lot::{Mutex, RwLock};

use super::watch::Watch;
use super::{narrows, rank, walk_matching, walker, worktree_of};

/// Paths an index holds at most; a tree past it is indexed in part, as the walk found it.
pub const MAX_PATHS: usize = 2_000_000;
/// Worktrees indexed at once; past it the one asked least recently goes.
const KEPT: usize = 8;
/// An index not asked for this long goes at the next query.
const IDLE: Duration = Duration::from_mins(15);
/// Directories a burst of changes names before the index is walked again instead.
const PENDING_MAX: usize = 10_000;
/// With no events, the least time between two looks at the directories' times.
const LOOK_EVERY: Duration = Duration::from_millis(500);

/// A directory's key in an index: relative to the worktree's top, ending in `/`; `""` is the
/// top itself.
type DirKey = Box<str>;

/// What changed under a worktree since its index last caught up.
#[derive(Debug, Default)]
pub(super) struct Pending {
    /// Directories whose entries changed.
    dirs: BTreeSet<DirKey>,
    /// Too much, or events lost: walk it all again.
    everything: bool,
}

impl Pending {
    fn add(&mut self, dir: DirKey) {
        if self.everything {
            return;
        }
        if self.dirs.len() >= PENDING_MAX {
            self.everything();
            return;
        }
        self.dirs.insert(dir);
    }

    fn everything(&mut self) {
        self.everything = true;
        self.dirs.clear();
    }
}

/// A worktree's paths.
#[derive(Debug, Default)]
struct Tree {
    /// Relative to the worktree's top, `/` between names, a directory ending in `/`; sorted, so a
    /// directory's contents follow it as one run.
    paths: Vec<Box<str>>,
    /// Each directory listed, `""` the top, with its modification time when it was.
    dirs: HashMap<DirKey, Option<SystemTime>>,
    /// The walk ended at [`MAX_PATHS`].
    partial: bool,
    /// Moves on every change, so indices into `paths` kept from before are known stale.
    version: u64,
}

impl Tree {
    /// The run of paths under `dir` (all of them for the top).
    fn under(&self, dir: &str) -> std::ops::Range<usize> {
        let start = self.paths.partition_point(|p| p.as_ref() < dir);
        let rest = self.paths.get(start..).unwrap_or_default();
        start..start.saturating_add(rest.partition_point(|p| p.starts_with(dir)))
    }

    /// The paths under `dir`, each without `dir` in front.
    fn below<'a>(&'a self, dir: &'a str) -> impl Iterator<Item = &'a str> {
        let run = self.paths.get(self.under(dir)).unwrap_or_default();
        run.iter().filter_map(move |p| p.get(dir.len()..)).filter(|p| !p.is_empty())
    }
}

/// What an index and its watch share: where the worktree is, its paths, and what changed.
#[derive(Debug)]
pub(super) struct Shared {
    /// The worktree's top, canonical, as the file system's events name it.
    top: PathBuf,
    tree: OnceLock<RwLock<Tree>>,
    pending: Mutex<Pending>,
}

impl Shared {
    /// The top, as events are matched against it.
    pub(super) fn top(&self) -> &Path {
        &self.top
    }

    /// The file system says something at `path` was made, removed or renamed (`entries`), or
    /// written. A written file matters only when it is an ignore file. Only a change in a
    /// directory the index holds is kept: an ignored `target/` churning costs nothing.
    pub(super) fn changed(&self, path: &Path, entries: bool) {
        let Ok(rel) = path.strip_prefix(&self.top) else { return };
        let mut names = rel.iter();
        if names.next().is_some_and(|first| first == ".git") {
            return;
        }
        if rel.file_name().is_some_and(|n| n == ".gitignore" || n == ".ignore") {
            self.lost();
            return;
        }
        if !entries {
            return;
        }
        let Some(dir) = rel.parent().map(dir_key) else { return };
        if let Some(tree) = self.tree.get()
            && !tree.read().dirs.contains_key(&dir)
        {
            return;
        }
        self.pending.lock().add(dir);
    }

    /// Events were lost, or the worktree itself moved: walk it again.
    pub(super) fn lost(&self) {
        self.pending.lock().everything();
    }
}

/// `rel`, a directory relative to the top, as a [`DirKey`].
fn dir_key(rel: &Path) -> DirKey {
    let mut key = rel.to_string_lossy().into_owned();
    if !key.is_empty() {
        key.push('/');
    }
    key.into()
}

/// The paths of one worktree, for quick open.
#[derive(Debug)]
pub struct Index {
    shared: Arc<Shared>,
    /// The file system's events over the tree, when it gives them.
    watch: Mutex<Option<Watch>>,
    /// The watch starting beside a walk, until it is up.
    starting: Mutex<Option<std::thread::JoinHandle<Option<Watch>>>>,
    /// Kernel watches the index may hold (inotify's, one a directory).
    limit: usize,
    /// Whether it asks for events at all.
    events: AtomicBool,
    /// When the directories' times were last looked at, with no events.
    looked: Mutex<Option<Instant>>,
    /// How long the first walk took.
    built_in: OnceLock<Duration>,
    /// The last query's matches, which the next keystroke scores again instead of the tree.
    last: Mutex<Option<Last>>,
    /// What the person should be told about this index, not yet said.
    notice: Mutex<Option<String>>,
}

/// A query's matches, kept for the query typed on from it.
#[derive(Debug)]
struct Last {
    /// The tree's [`Tree::version`] they index.
    version: u64,
    /// The directory asked under.
    dir: DirKey,
    query: String,
    /// Indices into the tree's paths.
    matched: Vec<usize>,
}

impl Index {
    /// An index of the worktree at `top` (canonical), walked on its first query.
    #[must_use]
    pub fn new(top: PathBuf) -> Self {
        let shared = Arc::new(Shared {
            top,
            tree: OnceLock::new(),
            pending: Mutex::new(Pending::default()),
        });
        Self {
            shared,
            watch: Mutex::new(None),
            starting: Mutex::new(None),
            limit: usize::MAX,
            events: AtomicBool::new(true),
            looked: Mutex::new(None),
            built_in: OnceLock::new(),
            last: Mutex::new(None),
            notice: Mutex::new(None),
        }
    }

    /// An index that holds at most `limit` kernel watches, as if the kernel's limit were that.
    #[must_use]
    pub fn limited(top: PathBuf, limit: usize) -> Self {
        Self { limit, ..Self::new(top) }
    }

    /// What the person should know of how it answers, once: the watches ran out, or the tree
    /// is held in part. `None` after it has been taken.
    #[must_use]
    pub fn take_notice(&self) -> Option<String> {
        self.notice.lock().take()
    }

    /// Kernel watches it holds: one a directory with inotify, none with `FSEvents`' stream.
    #[must_use]
    pub fn watches(&self) -> usize {
        self.watch.lock().as_ref().map_or(0, Watch::watches)
    }

    /// The worktree's top.
    #[must_use]
    pub fn top(&self) -> &Path {
        &self.shared.top
    }

    /// Whether the file system's events keep it fresh (else it looks at directories' times).
    #[must_use]
    pub fn follows_events(&self) -> bool {
        self.watch.lock().is_some()
    }

    /// How long the first walk took, once it has run.
    #[must_use]
    pub fn built_in(&self) -> Option<Duration> {
        self.built_in.get().copied()
    }

    /// Paths it holds now, directories included.
    #[must_use]
    pub fn len(&self) -> usize {
        self.shared.tree.get().map_or(0, |t| t.read().paths.len())
    }

    /// Whether it holds nothing (not walked yet, or an empty worktree).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Wait for the watch started with the walk, as a test that follows events must.
    #[cfg(test)]
    fn wait_for_watch(&self) {
        if let Some(tree) = self.shared.tree.get() {
            self.take_started(&tree.read(), true);
        }
    }

    /// The tree, walked the first time.
    fn ready(&self) -> &RwLock<Tree> {
        self.shared.tree.get_or_init(|| {
            let at = Instant::now();
            let tree = self.build();
            let _first = self.built_in.set(at.elapsed());
            tracing::info!(
                top = %self.shared.top.display(),
                paths = tree.paths.len(),
                partial = tree.partial,
                ms = at.elapsed().as_millis(),
                events = self.follows_events(),
                watches = self.watches(),
                "worktree indexed"
            );
            RwLock::new(tree)
        })
    }

    /// Walk the whole tree while a new watch starts beside it. Starting an `FSEvents` stream
    /// can take most of a second while `fseventsd` is busy, so the first answer does not wait
    /// for it: until the watch is up, the index looks at its directories' times instead.
    fn build(&self) -> Tree {
        // The old watch, if any, goes here, and its watches with it.
        *self.watch.lock() = None;
        *self.starting.lock() = self.events.load(Ordering::Relaxed).then(|| self.start()).flatten();
        let tree = walk_all(&self.shared.top);
        self.take_started(&tree, false);
        if tree.partial {
            *self.notice.lock() = Some(format!(
                "Quick open holds the first {MAX_PATHS} paths of {}: past them it finds nothing",
                self.shared.top.display()
            ));
        }
        tree
    }

    /// Start a watch on a thread of its own.
    fn start(&self) -> Option<std::thread::JoinHandle<Option<Watch>>> {
        let (shared, limit) = (Arc::clone(&self.shared), self.limit);
        std::thread::Builder::new()
            .name("slopty-find-watch".into())
            .spawn(move || Watch::start(shared, limit))
            .inspect_err(|e| tracing::warn!(%e, "quick open's watch thread did not start"))
            .ok()
    }

    /// Once the watch being started is up (or, with `wait`, when it is), follow `tree`'s
    /// directories on it.
    fn take_started(&self, tree: &Tree, wait: bool) {
        let Some(started) = self.starting.lock().take_if(|s| wait || s.is_finished()) else {
            return;
        };
        let Ok(Some(watch)) = started.join() else { return };
        *self.watch.lock() = Some(watch);
        self.cover(tree.dirs.iter().map(|(dir, time)| (dir.clone(), *time)).collect());
    }

    /// Watch `dirs` (with their times when they were listed), and list again any whose time
    /// moved before the watch saw it. Past the watches there are, let the watch go and say so.
    fn cover(&self, dirs: Vec<(DirKey, Option<SystemTime>)>) {
        let mut watch = self.watch.lock();
        let Some(held) = watch.as_mut() else { return };
        let top = &self.shared.top;
        match held.cover(dirs.iter().map(|(dir, _)| top.join(dir.as_ref()))) {
            Ok(()) => {
                drop(watch);
                let moved = dirs
                    .into_iter()
                    .filter(|(dir, then)| modified(&top.join(dir.as_ref())) != *then);
                let mut pending = self.shared.pending.lock();
                for (dir, _) in moved {
                    pending.add(dir);
                }
            }
            Err(out) => {
                tracing::warn!(
                    top = %top.display(),
                    held = out.held,
                    limit = %out.why,
                    "quick open's index ran out of file watches: it looks at its folders' \
                     times before each query instead, slower on a tree this large"
                );
                *watch = None;
                drop(watch);
                *self.notice.lock() = Some(format!(
                    "Quick open ran out of file watches ({}) under {}: it looks at the folders \
                     before each search instead, which is slower on a tree this large",
                    out.why,
                    top.display()
                ));
            }
        }
    }

    /// The paths under `dir` (canonical, inside the worktree) that `query` matches, best
    /// first, at most `limit`, relative to `dir`.
    ///
    /// A query typed on from the last one, with the tree unchanged, scores only the last one's
    /// matches, so each keystroke costs less than the one before it.
    #[must_use]
    pub fn matching(&self, dir: &Path, query: &str, limit: usize) -> Vec<String> {
        let tree = self.ready();
        self.catch_up(tree);
        let Ok(rel) = dir.strip_prefix(&self.shared.top) else { return Vec::new() };
        let prefix = dir_key(rel);
        let tree = tree.read();
        let previous = self.last.lock().take();
        let candidates = match previous {
            Some(last)
                if last.version == tree.version
                    && last.dir == prefix
                    && narrows(&last.query, query) =>
            {
                last.matched
            }
            _ => tree.under(&prefix).collect(),
        };
        let ranking = rank(&tree.paths, prefix.len(), &candidates, query, limit);
        let found = ranking
            .best
            .iter()
            .filter_map(|&at| tree.paths.get(at)?.get(prefix.len()..))
            .map(str::to_owned)
            .collect();
        *self.last.lock() = Some(Last {
            version: tree.version,
            dir: prefix,
            query: query.to_owned(),
            matched: ranking.matched,
        });
        drop(tree);
        found
    }

    /// Bring the tree up to what the disk holds: what the events named, or with none, what the
    /// directories' times say.
    fn catch_up(&self, tree: &RwLock<Tree>) {
        if self.starting.lock().as_ref().is_some_and(std::thread::JoinHandle::is_finished) {
            self.take_started(&tree.read(), false);
        }
        if !self.follows_events() {
            self.look_at_times(tree);
        }
        let Pending { dirs, everything } = std::mem::take(&mut *self.shared.pending.lock());
        if everything {
            let fresh = self.build();
            let mut tree = tree.write();
            let version = tree.version.wrapping_add(1);
            *tree = Tree { version, ..fresh };
            drop(tree);
            return;
        }
        if dirs.is_empty() {
            return;
        }
        let mut tree = tree.write();
        let mut gone = BTreeSet::new();
        let mut added = Vec::new();
        for dir in &dirs {
            // A directory the index no longer holds went with its parent's listing.
            if tree.dirs.contains_key(dir) {
                relist(&self.shared.top, &mut tree, dir, &mut gone, &mut added);
            }
        }
        let new_dirs: Vec<(DirKey, Option<SystemTime>)> = added
            .iter()
            .filter_map(|f| match f.kind {
                Kind::Dir(time) => Some((f.path.clone(), time)),
                Kind::File => None,
            })
            .collect();
        apply(&mut tree, &gone, added);
        drop(tree);
        if !new_dirs.is_empty() {
            self.cover(new_dirs);
        }
    }

    /// With no events: note every directory whose modification time moved as changed, at most
    /// every [`LOOK_EVERY`].
    fn look_at_times(&self, tree: &RwLock<Tree>) {
        let now = Instant::now();
        {
            let mut looked = self.looked.lock();
            if looked.is_some_and(|at| now.duration_since(at) < LOOK_EVERY) {
                return;
            }
            *looked = Some(now);
        }
        let moved: Vec<DirKey> = tree
            .read()
            .dirs
            .iter()
            .filter(|(dir, then)| modified(&self.shared.top.join(dir.as_ref())) != **then)
            .map(|(dir, _)| dir.clone())
            .collect();
        let mut pending = self.shared.pending.lock();
        for dir in moved {
            pending.add(dir);
        }
    }
}

/// A directory's modification time, `None` when it cannot be read.
fn modified(dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(dir).and_then(|m| m.modified()).ok()
}

/// One path the walk found, relative to the top.
struct Found {
    path: Box<str>,
    kind: Kind,
}

/// What a found path is.
#[derive(Clone, Copy)]
enum Kind {
    File,
    /// A directory, with its modification time when it could be read.
    Dir(Option<SystemTime>),
}

impl Found {
    /// What `entry` (at `name` relative to the top) is.
    fn of(entry: &ignore::DirEntry, name: &str) -> Self {
        if entry.file_type().is_some_and(|t| t.is_dir()) {
            let time = entry.metadata().ok().and_then(|m| m.modified().ok());
            Self { path: format!("{name}/").into(), kind: Kind::Dir(time) }
        } else {
            Self { path: name.into(), kind: Kind::File }
        }
    }
}

/// Walk `from` (under `top`) to `depth`, in parallel, as quick open sees a tree: its paths and
/// directories relative to `top`, and whether it stopped at `budget` paths.
fn walk(top: &Path, from: &Path, depth: Option<usize>, budget: usize) -> (Vec<Found>, bool) {
    let (send, receive) = mpsc::channel::<Vec<Found>>();
    let count = AtomicUsize::new(0);
    let full = AtomicBool::new(false);
    walker(from, depth).build_parallel().run(|| {
        let mut batch = Batch { found: Vec::new(), send: send.clone() };
        let (count, full) = (&count, &full);
        Box::new(move |entry| {
            let Ok(entry) = entry else { return ignore::WalkState::Continue };
            let Ok(rel) = entry.path().strip_prefix(top) else {
                return ignore::WalkState::Continue;
            };
            // A name that is not UTF-8 cannot be named on the wire; the rest of the tree can.
            let Some(name) = rel.to_str().filter(|n| !n.is_empty()) else {
                return ignore::WalkState::Continue;
            };
            if count.fetch_add(1, Ordering::Relaxed) >= budget {
                full.store(true, Ordering::Relaxed);
                return ignore::WalkState::Quit;
            }
            batch.push(Found::of(&entry, name));
            ignore::WalkState::Continue
        })
    });
    drop(send);
    let found: Vec<Found> = receive.into_iter().flatten().collect();
    (found, full.into_inner())
}

/// A walker thread's finds, sent on in batches and the rest when the thread's visitor goes.
struct Batch {
    found: Vec<Found>,
    send: mpsc::Sender<Vec<Found>>,
}

impl Batch {
    fn push(&mut self, found: Found) {
        self.found.push(found);
        if self.found.len() >= 4096 {
            let full = std::mem::take(&mut self.found);
            let _gone = self.send.send(full);
        }
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        if !self.found.is_empty() {
            let _gone = self.send.send(std::mem::take(&mut self.found));
        }
    }
}

/// The entries of directory `at` (under `top`) as quick open sees them, relative to `top`: the
/// ignore files of its parents honoured. Nothing when it is not a directory any more.
fn list(top: &Path, at: &Path) -> Vec<Found> {
    walker(at, Some(1))
        .build()
        .flatten()
        .filter(|entry| entry.depth() == 1)
        .filter_map(|entry| {
            let name = entry.path().strip_prefix(top).ok()?.to_str()?;
            Some(Found::of(&entry, name))
        })
        .collect()
}

/// The whole worktree at `top`, walked.
fn walk_all(top: &Path) -> Tree {
    let (found, partial) = walk(top, top, None, MAX_PATHS);
    let mut tree = Tree { partial, ..Tree::default() };
    tree.dirs.insert("".into(), modified(top));
    tree.paths.reserve(found.len());
    for Found { path, kind } in found {
        if let Kind::Dir(time) = kind {
            tree.dirs.insert(path.clone(), time);
        }
        tree.paths.push(path);
    }
    tree.paths.sort_unstable();
    tree
}

/// List `dir` again: its entries gone go into `gone` (a directory with all under it), and each
/// new one into `added`, a new directory walked whole.
fn relist(
    top: &Path,
    tree: &mut Tree,
    dir: &str,
    gone: &mut BTreeSet<Box<str>>,
    added: &mut Vec<Found>,
) {
    let at = top.join(dir);
    let now: HashMap<Box<str>, Found> =
        list(top, &at).into_iter().map(|f| (f.path.clone(), f)).collect();
    // Its entries as the index holds them: the paths under it with no `/` but a last one.
    let before: HashSet<Box<str>> = tree
        .below(dir)
        .filter(|rest| !rest.trim_end_matches('/').contains('/'))
        .map(|rest| format!("{dir}{rest}").into())
        .collect();
    for path in &before {
        if !now.contains_key(path) {
            gone.insert(path.clone());
        }
    }
    for (path, found) in now {
        if before.contains(&path) {
            continue;
        }
        if matches!(found.kind, Kind::Dir(_)) {
            let inside = top.join(path.as_ref());
            let budget = MAX_PATHS.saturating_sub(tree.paths.len().saturating_add(added.len()));
            let (below, _) = walk(top, &inside, None, budget);
            added.extend(below.into_iter().filter(|f| f.path != path));
        }
        added.push(found);
    }
    tree.dirs.insert(dir.into(), modified(&at));
}

/// Take `gone` (each with all under it) out of the tree and put `added` in, in order.
fn apply(tree: &mut Tree, gone: &BTreeSet<Box<str>>, added: Vec<Found>) {
    if gone.is_empty() && added.is_empty() {
        return;
    }
    tree.version = tree.version.wrapping_add(1);
    if !gone.is_empty() {
        let went = |p: &str| {
            gone.contains(p)
                || p.match_indices('/')
                    .any(|(at, _)| p.get(..=at).is_some_and(|d| gone.contains(d)))
        };
        tree.paths.retain(|p| !went(p));
        tree.dirs.retain(|d, _| d.is_empty() || !went(d));
    }
    if added.is_empty() {
        return;
    }
    let mut fresh = Vec::with_capacity(added.len());
    for Found { path, kind } in added {
        if let Kind::Dir(time) = kind {
            tree.dirs.insert(path.clone(), time);
        }
        fresh.push(path);
    }
    fresh.sort_unstable();
    fresh.dedup();
    tree.paths = merged(std::mem::take(&mut tree.paths), fresh);
}

/// `paths` and `fresh`, each sorted without repeats, as one such list. A file made in a large
/// tree costs a search per new path and a move of the rest, not a sort of every path.
fn merged(paths: Vec<Box<str>>, fresh: Vec<Box<str>>) -> Vec<Box<str>> {
    let cuts: Vec<(usize, bool)> = fresh
        .iter()
        .map(|p| {
            let cut = paths.partition_point(|q| q < p);
            (cut, paths.get(cut) == Some(p))
        })
        .collect();
    let mut out = Vec::with_capacity(paths.len().saturating_add(fresh.len()));
    let mut old = paths.into_iter();
    let mut taken = 0;
    for (path, (cut, held)) in fresh.into_iter().zip(cuts) {
        out.extend(old.by_ref().take(cut.saturating_sub(taken)));
        taken = taken.max(cut);
        if !held {
            out.push(path);
        }
    }
    out.extend(old);
    out
}

/// A quick-open answer.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Answer {
    /// Paths best first, relative to the directory asked.
    pub paths: Vec<String>,
    /// What the person should be told, said once ([`Index::take_notice`]).
    pub notice: Option<String>,
}

/// The worker's indexes, one per worktree asked about lately.
#[derive(Debug, Default)]
pub struct Indexes {
    kept: Mutex<Vec<(Arc<Index>, Instant)>>,
}

impl Indexes {
    /// The daemon's own, which every client's quick open shares.
    #[must_use]
    pub fn shared() -> &'static Self {
        static SHARED: LazyLock<Indexes> = LazyLock::new(Indexes::default);
        &SHARED
    }

    /// The paths under `dir` that `query` matches, best first, at most `limit`, relative to
    /// `dir`: from its worktree's index, or a bounded walk outside one; with what the person
    /// should know of the index, the first time there is something.
    #[must_use]
    pub fn matching(&self, dir: &Path, query: &str, limit: usize) -> Answer {
        if query.trim().is_empty() || limit == 0 {
            return Answer::default();
        }
        let Ok(dir) = std::fs::canonicalize(dir) else { return Answer::default() };
        match worktree_of(&dir) {
            Some(top) => {
                let index = self.index(&top);
                let paths = index.matching(&dir, query, limit);
                Answer { paths, notice: index.take_notice() }
            }
            None => Answer { paths: walk_matching(&dir, query, limit), notice: None },
        }
    }

    /// The index of the worktree at `top` (canonical), made if there is none; the least recently
    /// asked, and any idle past `IDLE`, go.
    #[must_use]
    pub fn index(&self, top: &Path) -> Arc<Index> {
        let now = Instant::now();
        let mut kept = self.kept.lock();
        kept.retain(|(_, used)| now.duration_since(*used) < IDLE);
        if let Some((index, used)) = kept.iter_mut().find(|(i, _)| i.top() == top) {
            *used = now;
            return Arc::clone(index);
        }
        let index = Arc::new(Index::new(top.to_path_buf()));
        kept.push((Arc::clone(&index), now));
        if kept.len() > KEPT
            && let Some(oldest) =
                kept.iter().enumerate().min_by_key(|(_, (_, used))| *used).map(|(at, _)| at)
        {
            kept.swap_remove(oldest);
        }
        index
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// A worktree with a source tree, an ignored build folder and a hidden folder.
    fn worktree() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let top = fs::canonicalize(dir.path()).unwrap();
        for sub in [".git", "src/net", "docs", "target/debug", ".cache"] {
            fs::create_dir_all(top.join(sub)).unwrap();
        }
        for file in ["src/main.rs", "src/net/link.rs", "docs/manual.md", "README.md"] {
            fs::write(top.join(file), "").unwrap();
        }
        fs::write(top.join("target/debug/main"), "").unwrap();
        fs::write(top.join(".cache/main.txt"), "").unwrap();
        fs::write(top.join(".gitignore"), "target\n").unwrap();
        (dir, top)
    }

    /// Ask `index` until `ok` holds of the answer, as a person typing would, for up to 2 s.
    #[expect(clippy::disallowed_methods, reason = "a test asks again as a person would")]
    fn until(
        index: &Index,
        dir: &Path,
        query: &str,
        ok: impl Fn(&[String]) -> bool,
    ) -> Vec<String> {
        let deadline = Instant::now().checked_add(Duration::from_secs(2)).unwrap();
        loop {
            let found = index.matching(dir, query, 8);
            if ok(&found) || Instant::now() > deadline {
                return found;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// New paths go into the sorted list in order, once each, before, between and after the
    /// ones held.
    #[test]
    fn new_paths_merge_into_the_sorted_ones() {
        let boxed = |v: &[&str]| v.iter().map(|p| Box::<str>::from(*p)).collect::<Vec<_>>();
        let held = boxed(&["b", "d", "f"]);
        assert_eq!(
            merged(held.clone(), boxed(&["a", "c", "d", "g"])),
            boxed(&["a", "b", "c", "d", "f", "g"])
        );
        assert_eq!(merged(held.clone(), Vec::new()), held);
        assert_eq!(merged(Vec::new(), held.clone()), held);
        assert_eq!(merged(held, boxed(&["e", "e2"])), boxed(&["b", "d", "e", "e2", "f"]));
    }

    #[test]
    fn a_worktree_is_walked_once_and_asked_from_any_folder_in_it() {
        let (_dir, top) = worktree();
        assert_eq!(worktree_of(&top.join("src/net")).as_deref(), Some(top.as_path()));
        let index = Index::new(top.clone());
        assert_eq!(index.matching(&top, "main", 8), ["src/main.rs"], "ignored and hidden skipped");
        assert!(index.built_in().is_some());
        assert_eq!(index.len(), 7, "src/ src/net/ docs/ and four files");
        assert_eq!(index.matching(&top.join("src"), "link", 8), ["net/link.rs"], "relative");
        assert!(index.matching(&top.join("docs"), "main", 8).is_empty(), "only under it");
        assert_eq!(index.matching(&top, "net", 1), ["src/net/"]);
    }

    /// The events keep the index fresh: a file made, a folder of files made, a rename and a
    /// delete show at the next query without a walk; a build writing into an ignored folder
    /// adds nothing; an ignore file changed walks it again.
    #[test]
    fn the_index_follows_the_tree() {
        let (_dir, top) = worktree();
        let index = Index::new(top.clone());
        assert_eq!(index.matching(&top, "lib", 8), Vec::<String>::new());
        index.wait_for_watch();
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            assert!(index.follows_events(), "FSEvents or inotify over the worktree");
        }
        if cfg!(target_os = "linux") {
            assert_eq!(index.watches(), 4, "the top, src/, src/net/ and docs/: none ignored");
        }

        fs::write(top.join("src/lib.rs"), "").unwrap();
        let found = until(&index, &top, "lib", |f| f == ["src/lib.rs"]);
        assert_eq!(found, ["src/lib.rs"], "a new file");

        fs::create_dir_all(top.join("src/ui/view")).unwrap();
        fs::write(top.join("src/ui/view/pane.rs"), "").unwrap();
        let found = until(&index, &top, "pane", |f| f == ["src/ui/view/pane.rs"]);
        assert_eq!(found, ["src/ui/view/pane.rs"], "a new folder, walked whole");
        fs::write(top.join("src/ui/view/grid.rs"), "").unwrap();
        let found = until(&index, &top, "grid", |f| f == ["src/ui/view/grid.rs"]);
        assert_eq!(found, ["src/ui/view/grid.rs"], "and followed from then on");

        fs::rename(top.join("src/lib.rs"), top.join("src/core.rs")).unwrap();
        let found = until(&index, &top, "core", |f| f == ["src/core.rs"]);
        assert_eq!(found, ["src/core.rs"], "renamed");
        assert!(index.matching(&top, "lib", 8).is_empty(), "the old name goes");

        fs::remove_dir_all(top.join("src/ui")).unwrap();
        let found = until(&index, &top, "pane", <[String]>::is_empty);
        assert!(found.is_empty(), "a folder removed goes with all in it: {found:?}");

        fs::write(top.join("target/debug/lib.rlib"), "").unwrap();
        fs::write(top.join("docs/guide.md"), "").unwrap();
        until(&index, &top, "guide", |f| !f.is_empty());
        assert!(index.matching(&top, "rlib", 8).is_empty(), "an ignored folder stays out");

        fs::write(top.join(".gitignore"), "target\ndocs\n").unwrap();
        let found = until(&index, &top, "manual", <[String]>::is_empty);
        assert!(found.is_empty(), "the ignore file changed: {found:?}");
    }

    /// A query typed on scores only the last one's matches; a change in the tree, or a query
    /// that is not typed on, scores the whole tree again.
    #[test]
    fn typing_on_scores_the_last_matches_and_a_change_starts_again() {
        let (_dir, top) = worktree();
        let index = Index::new(top.clone());
        let scored = |index: &Index| index.last.lock().as_ref().map_or(0, |l| l.matched.len());
        assert_eq!(index.matching(&top, "m", 8).len(), 3, "main.rs, manual.md, README.md");
        assert_eq!(index.matching(&top, "ma", 8), ["src/main.rs", "docs/manual.md"]);
        assert_eq!(scored(&index), 2);
        assert_eq!(index.matching(&top, "mai", 8), ["src/main.rs"]);
        fs::write(top.join("src/maintain.rs"), "").unwrap();
        let found = until(&index, &top, "main", |f| f.len() == 2);
        assert_eq!(found, ["src/main.rs", "src/maintain.rs"], "the change is seen");
        assert_eq!(index.matching(&top, "link", 8), ["src/net/link.rs"], "not typed on");
    }

    /// Past the watches it may hold, the index lets its watch go and looks at its folders'
    /// times instead, and stays right (inotify; `FSEvents` holds no watch a directory).
    #[test]
    #[cfg(target_os = "linux")]
    fn past_its_watches_the_index_looks_at_times_and_stays_right() {
        let (_dir, top) = worktree();
        let index = Index::limited(top.clone(), 2);
        assert_eq!(index.matching(&top, "main", 8), ["src/main.rs"]);
        index.wait_for_watch();
        assert!(!index.follows_events(), "the watch is let go");
        let said = index.take_notice().unwrap_or_default();
        assert!(said.contains("ran out of file watches"), "and the person is told: {said}");
        assert_eq!(index.take_notice(), None, "once");
        assert_eq!(index.watches(), 0, "and its watches with it");
        fs::write(top.join("src/net/lib.rs"), "").unwrap();
        assert_eq!(until(&index, &top, "lib", |f| !f.is_empty()), ["src/net/lib.rs"]);
    }

    /// The kernel's own limit met the same way: run where `max_user_watches` is below the
    /// tree's 60 directories, as in a user namespace of its own:
    ///
    /// ```sh
    /// unshare -Ur sh -c 'echo 20 > /proc/sys/user/max_inotify_watches; <test binary> --ignored past_the_kernels'
    /// ```
    #[test]
    #[cfg(target_os = "linux")]
    #[ignore = "needs max_user_watches lowered below 60"]
    fn past_the_kernels_watches_the_index_looks_at_times_and_stays_right() {
        let (_dir, top) = worktree();
        for n in 0..60 {
            fs::create_dir_all(top.join(format!("many/d{n}"))).unwrap();
        }
        let index = Index::new(top.clone());
        assert_eq!(index.matching(&top, "main", 8), ["src/main.rs"]);
        index.wait_for_watch();
        assert!(!index.follows_events(), "ENOSPC lets the watch go");
        fs::write(top.join("many/d7/lib.rs"), "").unwrap();
        assert_eq!(until(&index, &top, "lib", |f| !f.is_empty()), ["many/d7/lib.rs"]);
    }

    /// Without events, the directories' times say what changed.
    #[test]
    fn without_events_the_times_of_the_folders_say_what_changed() {
        let (_dir, top) = worktree();
        let index = Index::new(top.clone());
        index.events.store(false, Ordering::Relaxed);
        assert_eq!(index.matching(&top, "lib", 8), Vec::<String>::new());
        assert!(!index.follows_events());
        fs::write(top.join("src/net/lib.rs"), "").unwrap();
        let found = until(&index, &top, "lib", |f| !f.is_empty());
        assert_eq!(found, ["src/net/lib.rs"]);
        fs::remove_file(top.join("src/net/lib.rs")).unwrap();
        assert_eq!(until(&index, &top, "lib", <[String]>::is_empty), Vec::<String>::new());
    }

    #[test]
    fn the_worker_keeps_one_index_a_worktree_and_lets_the_oldest_go() {
        let indexes = Indexes::default();
        let trees: Vec<_> = std::iter::repeat_with(worktree).take(KEPT.saturating_add(1)).collect();
        let first = indexes.index(&trees[0].1);
        assert!(Arc::ptr_eq(&first, &indexes.index(&trees[0].1)), "one per worktree");
        for (_, top) in &trees[1..] {
            let _made = indexes.index(top);
        }
        assert_eq!(indexes.kept.lock().len(), KEPT);
        assert!(!Arc::ptr_eq(&first, &indexes.index(&trees[0].1)), "the oldest went");
        let (_dir, top) = &trees[1];
        assert_eq!(indexes.matching(&top.join("src"), "main", 8).paths, ["main.rs"]);
    }

    /// A synthetic worktree of `dirs` folders three deep, `files` files in each.
    fn large(dirs: usize, files: usize) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let top = fs::canonicalize(dir.path()).unwrap();
        fs::create_dir_all(top.join(".git")).unwrap();
        for d in 0..dirs {
            let at = top.join(format!("crate{}/src/module{}", d / 50, d % 50));
            fs::create_dir_all(&at).unwrap();
            for f in 0..files {
                fs::write(at.join(format!("item_{f}_view.rs")), "").unwrap();
            }
        }
        (dir, top)
    }

    /// p50, p99 and the largest of `samples`, in ms.
    fn spread(mut samples: Vec<Duration>) -> String {
        samples.sort_unstable();
        let last = samples.len().saturating_sub(1);
        let at = |q: usize| samples[last.saturating_mul(q) / 100].as_secs_f64() * 1e3;
        format!("p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms", at(50), at(99), at(100))
    }

    /// Quick open's costs on a large worktree: the first walk and when its watch is up, a
    /// keystroke's query (the first of a query, and one typed on) against the bounded walk it
    /// replaced, and how soon a file made shows. `SLOPTY_FIND_TREE` names a real worktree to
    /// measure; without it, a synthetic one of 200,000 files.
    #[test]
    #[ignore = "measurement, run by hand"]
    #[expect(clippy::disallowed_methods, reason = "the tree's own events drain before timing")]
    fn quick_open_costs() {
        let (_dir, top) = if let Some(tree) = std::env::var_os("SLOPTY_FIND_TREE") {
            (tempfile::tempdir().unwrap(), fs::canonicalize(tree).unwrap())
        } else {
            let made = large(4000, 50);
            std::thread::sleep(Duration::from_secs(10));
            made
        };
        let bare = Index::new(top.clone());
        bare.events.store(false, Ordering::Relaxed);
        let at = Instant::now();
        let _first = bare.matching(&top, "x", 50);
        let walk_only = at.elapsed();
        drop(bare);
        let marks = || {
            let slab = fs::read_to_string("/proc/slabinfo").ok()?;
            let line = slab.lines().find(|l| l.starts_with("inotify_inode_mark "))?;
            let n: Vec<u64> = line
                .split_whitespace()
                .skip(1)
                .take(3)
                .map(|x| x.parse().ok())
                .collect::<Option<_>>()?;
            Some(n[0] * n[2])
        };
        let before = marks();
        let index = Index::new(top.clone());
        let at = Instant::now();
        let _first = index.matching(&top, "x", 50);
        let with_watch = at.elapsed();
        index.wait_for_watch();
        let watching = at.elapsed();
        println!(
            "{}: {} paths, first walk {:.0} ms without events, {:.0} ms with, watch up at {:.0} \
             ms, events {}, {} watches",
            top.display(),
            index.len(),
            walk_only.as_secs_f64() * 1e3,
            with_watch.as_secs_f64() * 1e3,
            watching.as_secs_f64() * 1e3,
            index.follows_events(),
            index.watches(),
        );
        if let (Some(before), Some(after)) = (before, marks()) {
            println!(
                "inotify marks: {} KiB more in the kernel's slab, {} bytes a watch",
                after.saturating_sub(before) / 1024,
                after.saturating_sub(before) / (index.watches().max(1) as u64)
            );
        }
        let fresh = ["view", "item 4", "mod12 item", "srcmodvie", "main", "rs"];
        let (mut first, mut typed_on) = (Vec::new(), Vec::new());
        for _ in 0..10 {
            for query in fresh {
                let at = Instant::now();
                let _found = index.matching(&top, query, 50);
                first.push(at.elapsed());
            }
            for end in 1..=8 {
                let query = "srcmodvi".get(..end).unwrap();
                let at = Instant::now();
                let _found = index.matching(&top, query, 50);
                if end > 1 {
                    typed_on.push(at.elapsed());
                }
            }
        }
        println!("a query's first keystroke: {}", spread(first));
        println!("a keystroke typed on: {}", spread(typed_on));
        let mut walks = Vec::new();
        for query in fresh {
            let at = Instant::now();
            let _found = walk_matching(&top, query, 50);
            walks.push(at.elapsed());
        }
        println!("a keystroke by a walk of 20,000 entries at most (before): {}", spread(walks));
        let mut shown = Vec::new();
        let made = top.join("slopty-find-probe");
        fs::create_dir_all(made.join("first")).unwrap();
        // Asked under the probe's own folder, so a query costs next to nothing and the time is
        // the event's and the catch-up's.
        until(&index, &made, "first", |f| !f.is_empty());
        for n in 0..20 {
            let name = format!("probe_{n}_needle.rs");
            let at = Instant::now();
            fs::write(made.join(&name), "").unwrap();
            let found = until(&index, &made, &name, |f| f.iter().any(|p| p.ends_with(&name)));
            assert!(found.iter().any(|p| p.ends_with(&name)), "{name} never showed");
            shown.push(at.elapsed());
        }
        fs::remove_dir_all(&made).unwrap();
        println!("a file made → in the next query: {}", spread(shown));
    }
}
