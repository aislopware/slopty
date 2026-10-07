//! Turn snapshots and the review over them, for every thread whatever drives its agent
//! (`crate::repo::snapshot` is the git half).
//!
//! The worker takes a snapshot of the thread's working tree as each turn begins and ends, off
//! the turn's own path: the turn's edge goes out at once, and the snapshot follows as an
//! [`Action::Snapshot`] when git is done. A turn's start is snapshotted at the earliest word of
//! it, the agent starting to work ([`Moment::Busy`]), since an adapter may only see the turn
//! itself once the agent has begun. A thread whose directory is not in git takes none.
//!
//! A review is a diff between two snapshots, "now" being one taken for it. Keeping and putting
//! back act once per intent, each checked against the blobs the review showed. An edit from a
//! turn puts the folder back to its before-snapshot ([`Snapshots::restore`]).
//!
//! An agent's own review ([`Intent::Review`]) takes the change a review showed as commits it can
//! name ([`Snapshots::review_range`]): Claude Code's `/code-review` takes `base...head`, Codex's
//! reviewer the head commit.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use slopty_proto::thread::wire::{EXPANDED_CHARS, Expanded, Intent, Outcome, Review, ReviewScope};
use slopty_proto::thread::{Action, Cap, Edge, IntentId, ThreadId, ThreadState, TreeRef, TurnId};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use super::Host;
use super::host::{Moment, TurnEdge};
use crate::repo::snapshot::{Failed, Repo};

/// Why a review has nothing to compare.
pub const NOT_IN_GIT: &str = "This thread's folder is not in a git repository";

/// The snapshots of every thread a host holds. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Snapshots {
    host: Host,
    dir: PathBuf,
    git: Option<PathBuf>,
    /// One git at a time on each thread's index.
    locks: Arc<Mutex<HashMap<ThreadId, Arc<tokio::sync::Mutex<()>>>>>,
    /// Keeps and reverts under way, so a repeat that comes meanwhile does nothing.
    picking: Arc<Mutex<HashSet<IntentId>>>,
}

impl Snapshots {
    /// Snapshots of `host`'s threads with `git`, their indexes kept under `dir`. With no git,
    /// no thread takes any.
    #[must_use]
    pub fn new(host: Host, dir: &Path, git: Option<PathBuf>) -> Self {
        Self { host, dir: dir.to_owned(), git, locks: Arc::default(), picking: Arc::default() }
    }

    /// Take the snapshots at every thread's turn edges from now on.
    #[must_use]
    pub fn spawn(&self) -> JoinHandle<()> {
        let this = self.clone();
        let mut edges = self.host.edges();
        tokio::spawn(async move {
            let mut threads: HashMap<ThreadId, mpsc::UnboundedSender<Moment>> = HashMap::new();
            loop {
                let TurnEdge { thread, moment } = match edges.recv().await {
                    Ok(edge) => edge,
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::warn!(missed, "turn snapshots missed");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                let tx = threads.entry(thread).or_insert_with(|| {
                    let (tx, rx) = mpsc::unbounded_channel();
                    tokio::spawn(this.clone().take_turns(thread, rx));
                    tx
                });
                if let Err(mpsc::error::SendError(moment)) = tx.send(moment) {
                    let (again, rx) = mpsc::unbounded_channel();
                    tokio::spawn(this.clone().take_turns(thread, rx));
                    let _queued = again.send(moment);
                    threads.insert(thread, again);
                }
            }
        })
    }

    /// One thread's snapshots, in the order its moments came.
    async fn take_turns(self, thread: ThreadId, mut moments: mpsc::UnboundedReceiver<Moment>) {
        let mut armed: Option<TreeRef> = None;
        while let Some(moment) = moments.recv().await {
            let Some(repo) = self.repo(thread) else { continue };
            let (turn, edge, tree) = match moment {
                Moment::Busy => {
                    if armed.is_none() {
                        armed = self.take(thread, &repo).await;
                    }
                    continue;
                }
                Moment::Began(turn) => {
                    // An adapter tells a turn again as it goes (its usage, its models); its
                    // start was the first time.
                    if self.host.update(thread, |s| (vec![], begun(s, turn))) != Some(false) {
                        continue;
                    }
                    let tree = match armed.take() {
                        Some(tree) => Some(tree),
                        None => self.take(thread, &repo).await,
                    };
                    (turn, Edge::Before, tree)
                }
                Moment::Ended(turn) => {
                    armed = None;
                    (turn, Edge::After, self.take(thread, &repo).await)
                }
            };
            let Some(tree) = tree else { continue };
            if let Err(e) = repo.record(thread, turn, edge, &tree).await {
                tracing::warn!(%thread, turn = turn.0, "a turn's snapshot was not kept: {e}");
                continue;
            }
            self.host.apply(thread, vec![Action::Snapshot { turn, edge, tree: tree.clone() }]);
            if edge == Edge::After {
                self.judge(thread, &repo, &tree).await;
            }
        }
    }

    /// Mark whether `thread`'s tree `now` holds changes its person has not kept: anything
    /// apart from what they kept, or from the thread's first snapshot when they kept nothing.
    async fn judge(&self, thread: ThreadId, repo: &Repo, now: &TreeRef) {
        let kept = match repo.kept(thread).await {
            Ok(kept) => kept,
            Err(e) => {
                tracing::warn!(%thread, "what was kept could not be read: {e}");
                return;
            }
        };
        self.host.update(thread, |state| {
            let to_review = kept.clone().or_else(|| base(state)).is_some_and(|from| from != *now);
            let actions = if state.to_review == to_review {
                vec![]
            } else {
                vec![Action::ToReview(to_review)]
            };
            (actions, ())
        });
    }

    /// A snapshot of `thread`'s tree now, under its lock.
    async fn take(&self, thread: ThreadId, repo: &Repo) -> Option<TreeRef> {
        let lock = self.lock(thread);
        let _held = lock.lock().await;
        match repo.take().await {
            Ok(tree) => Some(tree),
            Err(e) => {
                tracing::warn!(%thread, "a snapshot failed: {e}");
                None
            }
        }
    }

    fn lock(&self, thread: ThreadId) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(self.locks.lock().entry(thread).or_default())
    }

    /// The repository `thread` works in; `None` outside git, or with no git here.
    fn repo(&self, thread: ThreadId) -> Option<Repo> {
        let git = self.git.clone()?;
        let cwd = self.host.update(thread, |s| (vec![], s.meta.cwd.clone()))?;
        let root = crate::repo::root_of(Path::new(&cwd))?;
        let index = self.dir.join(thread.to_string()).join("index");
        Some(Repo { git, root, index })
    }

    /// What changed in `thread`'s tree over `scope`.
    pub async fn review(&self, thread: ThreadId, scope: ReviewScope) -> Review {
        let absent = |why: &str| Review {
            scope: scope.clone(),
            from: None,
            to: None,
            files: Vec::new(),
            absent: Some(why.to_owned()),
        };
        let Some(repo) = self.repo(thread) else { return absent(NOT_IN_GIT) };
        if let ReviewScope::WorkingTree(against) = &scope {
            let against = against.clone();
            return match crate::repo::snapshot::working_tree(&repo.git, &repo.root, against).await {
                Ok(review) => review,
                Err(e) => absent(&e.to_string()),
            };
        }
        let sides = self.host.update(thread, |state| (vec![], sides(state, &scope)));
        let Some((from, to)) = sides else { return absent("The thread is gone") };
        let from = match (&scope, from) {
            (ReviewScope::Kept, base) => match repo.kept(thread).await {
                Ok(Some(kept)) => Some(kept),
                _ => base,
            },
            (_, from) => from,
        };
        let Some(from) = from else { return absent("No snapshot was taken there") };
        let to = match to {
            Some(to) => Some(to),
            None => self.take(thread, &repo).await,
        };
        let Some(to) = to else { return absent("The tree could not be snapshotted now") };
        match repo.changes(&from, &to).await {
            Ok(files) => Review { scope, from: Some(from), to: Some(to), files, absent: None },
            Err(e) => absent(&e.to_string()),
        }
    }

    /// Act on a keep or a revert once: a repeat of `id` gets the first outcome back, and one
    /// that comes while the first is under way gets nothing (the first one's answer answers
    /// it). `None` for any other intent, and for that repeat.
    pub async fn pick(&self, thread: ThreadId, id: IntentId, intent: &Intent) -> Option<Outcome> {
        let (Intent::Keep(pick) | Intent::Revert(pick)) = intent else { return None };
        if let Some(outcome) = self.host.outcome(thread, id) {
            return Some(outcome);
        }
        if !self.picking.lock().insert(id) {
            return None;
        }
        let refused = |reason: String| Outcome::Refused { reason };
        let can = self.host.update(thread, |s| (vec![], (s.meta.can(Cap::SNAPSHOTS), base(s))));
        let outcome = match (can, self.repo(thread)) {
            (None, _) => refused("The thread is gone".to_owned()),
            (Some((false, _)), _) => Outcome::Unsupported { cap: Cap::named(Cap::SNAPSHOTS) },
            (_, None) => refused(NOT_IN_GIT.to_owned()),
            (Some((true, base)), Some(repo)) => {
                let lock = self.lock(thread);
                let _held = lock.lock().await;
                let done: Result<(), Failed> = match (intent, base) {
                    (Intent::Revert(_), _) => repo.revert(pick).await,
                    (_, Some(base)) => repo.keep(thread, &base, pick).await.map(|_kept| ()),
                    (_, None) => Err(Failed("No snapshot was taken to keep from".to_owned())),
                };
                if done.is_ok() {
                    match repo.take().await {
                        Ok(now) => self.judge(thread, &repo, &now).await,
                        Err(e) => tracing::warn!(%thread, "a snapshot failed: {e}"),
                    }
                }
                done.map_or_else(|e| refused(e.0), |()| Outcome::Done)
            }
        };
        let recorded = self.host.intent(thread, id, |_state| (outcome, Vec::new()));
        self.picking.lock().remove(&id);
        recorded
    }
}

impl Snapshots {
    /// Put `thread`'s folder back to `turn`'s before-snapshot for intent `id`, once, what it
    /// held first kept under the thread's refs as `<turn>-rewound`: a repeat of `id` gets the
    /// first outcome back, and one that comes meanwhile waits for nothing and gets `None`.
    pub async fn restore(&self, thread: ThreadId, id: IntentId, turn: TurnId) -> Option<Outcome> {
        if let Some(outcome) = self.host.outcome(thread, id) {
            return Some(outcome);
        }
        if !self.picking.lock().insert(id) {
            return None;
        }
        let refused = |reason: &str| Outcome::Refused { reason: reason.to_owned() };
        let before = self.host.update(thread, |s| {
            (vec![], s.turns.iter().find(|t| t.id == turn).map(|t| t.before.clone()))
        });
        let outcome = match (before, self.repo(thread)) {
            (None, _) => refused("The thread is gone"),
            (_, None) => refused(NOT_IN_GIT),
            (Some(None), _) => refused(&format!("There is no turn {} here", turn.0)),
            (Some(Some(None)), _) => refused("No snapshot was taken before that turn"),
            (Some(Some(Some(tree))), Some(repo)) => {
                let lock = self.lock(thread);
                let _held = lock.lock().await;
                let name = format!("{}-rewound", turn.0);
                match repo.restore(thread, &name, &tree).await {
                    Ok(()) => {
                        self.judge(thread, &repo, &tree).await;
                        Outcome::Done
                    }
                    Err(e) => refused(&e.0),
                }
            }
        };
        let recorded = self.host.intent(thread, id, |_state| (outcome, Vec::new()));
        self.picking.lock().remove(&id);
        recorded
    }
}

/// A change as an agent's own review names it: two commits whose difference it is, the head on
/// the base.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Range {
    /// The old side's commit.
    pub base: String,
    /// The new side's, whose parent is the base.
    pub head: String,
}

impl Range {
    /// How a ref range names it: `base...head`.
    #[must_use]
    pub fn dots(&self) -> String {
        format!("{}...{}", self.base, self.head)
    }
}

impl Snapshots {
    /// The change from `from` to `to` in `thread`'s repository as commits an agent's review
    /// takes, overwriting the last review's; why not, in words.
    ///
    /// # Errors
    ///
    /// Outside git, or when git refuses the trees.
    pub async fn review_range(
        &self,
        thread: ThreadId,
        from: &TreeRef,
        to: &TreeRef,
    ) -> Result<Range, String> {
        let repo = self.repo(thread).ok_or_else(|| NOT_IN_GIT.to_owned())?;
        let lock = self.lock(thread);
        let _held = lock.lock().await;
        let (base, head) = repo.review_pair(thread, from, to).await.map_err(|e| e.0)?;
        Ok(Range { base, head })
    }

    /// The text of blob `id` in `thread`'s repository, the side of a file a review showed, for
    /// the unchanged lines between its hunks (`ContentRef::blob`). [`Expanded::Gone`] outside
    /// git, for an id that is no blob there, a file that is not UTF-8, or one longer than
    /// [`EXPANDED_CHARS`]: its lines would be cut, so none are given.
    pub async fn blob(&self, thread: ThreadId, id: &str) -> Expanded {
        let Some(repo) = self.repo(thread) else { return Expanded::Gone };
        match repo.object(id).await.map(String::from_utf8) {
            Ok(Ok(text)) if text.len() <= EXPANDED_CHARS => Expanded::Text(text),
            Ok(_) => Expanded::Gone,
            Err(e) => {
                tracing::debug!(%thread, "no blob {id}: {e}");
                Expanded::Gone
            }
        }
    }

    /// Let every ref `state`'s thread keeps in its repository go, before the thread is.
    pub async fn forget(&self, state: &ThreadState) {
        let thread = state.meta.id;
        let Some(repo) = self.repo(thread) else { return };
        let lock = self.lock(thread);
        let held = lock.lock().await;
        if let Err(e) = repo.forget(thread).await {
            tracing::warn!(%thread, "a gone thread's snapshots stayed: {e}");
        }
        drop(held);
        self.locks.lock().remove(&thread);
    }
}

/// The two snapshots `scope` compares in `state`; `None` on the new side for now.
fn sides(state: &ThreadState, scope: &ReviewScope) -> (Option<TreeRef>, Option<TreeRef>) {
    let turn = |id: TurnId| state.turns.iter().find(|t| t.id == id);
    match scope {
        ReviewScope::Turn(id) => match turn(*id) {
            Some(t) => (t.before.clone(), t.after.clone()),
            None => (None, None),
        },
        ReviewScope::Since(id) => (turn(*id).and_then(|t| t.before.clone()), None),
        ReviewScope::Kept => (base(state), None),
        // Not between snapshots: [`Snapshots::review`] reads it from the repository.
        ReviewScope::WorkingTree(_) => (None, None),
    }
}

/// Whether `turn` already has its before-snapshot.
fn begun(state: &ThreadState, turn: TurnId) -> bool {
    state.turns.iter().any(|t| t.id == turn && t.before.is_some())
}

/// Where what is kept starts: the first snapshot the thread took.
fn base(state: &ThreadState) -> Option<TreeRef> {
    state.turns.iter().find_map(|t| t.before.clone())
}
