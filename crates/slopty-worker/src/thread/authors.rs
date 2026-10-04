//! Which thread wrote a line: `git blame` over the turns the worker snapshotted
//! ([`Authorship::authors`]).
//!
//! Every thread in a working tree keeps a snapshot of the tree as each of its turns begins and
//! ends (`super::review`). Laid end to end in the order the turns ended, they are a history of
//! that tree: the tree before a turn, as a commit of whatever came between the turns (the
//! person's edits, another agent's), then the tree after it, as the turn's own commit. Blamed
//! against that history, a line of the file as it is now comes from the turn that brought it
//! in. The commits are made the same way each time (`Repo::commit_at`), so the history is made
//! once per tree and grows a turn at a time; nothing names them, and the person's repository
//! gains no ref.
//!
//! A line from before the first snapshot, or from between the turns, may still be a thread's
//! from long ago: blamed against the person's own history, its commit names the thread in a
//! `Slopty-Thread` trailer when a project merged it. That holds on any machine with the commit.
//! The answer goes to the client whole, and the client keeps it until the file changes, so no
//! hover over a line asks the worker anything.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use slopty_core::WallMs;
use slopty_proto::thread::wire::{AuthorRun, Authors};
use slopty_proto::thread::{ThreadId, ThreadState, TreeRef, TurnId};

use super::Host;
use crate::repo::snapshot::{Failed, Repo};

/// A file larger than this is not blamed: blame reads all of it against every turn.
const BLAME_BYTES: u64 = 4 << 20;

/// What the answer says of a file outside git.
pub const NOT_IN_GIT: &str = "Not in a git repository";

/// A turn of a thread, as the history of its tree has it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Step {
    /// The thread.
    pub thread: ThreadId,
    /// Its turn.
    pub turn: TurnId,
    /// The tree as it began.
    pub before: TreeRef,
    /// The tree as it ended.
    pub after: TreeRef,
    /// When it ended.
    pub at: WallMs,
}

/// Who wrote one line.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Wrote {
    thread: ThreadId,
    turn: Option<TurnId>,
    commit: Option<String>,
    at: WallMs,
}

/// The turns of `state` with both snapshots, when it works in the tree at `root`.
#[must_use]
pub fn steps_of(state: &ThreadState, root: &Path) -> Vec<Step> {
    if crate::repo::root_of(Path::new(&state.meta.cwd)).as_deref() != Some(root) {
        return Vec::new();
    }
    state
        .turns
        .iter()
        .filter_map(|t| {
            Some(Step {
                thread: state.meta.id,
                turn: t.id,
                before: t.before.clone()?,
                after: t.after.clone()?,
                at: t.ended_ms.unwrap_or(t.started_ms),
            })
        })
        .collect()
}

/// The history of a tree's turns as commits: for each step, the commit of what came before it
/// (when the tree changed since the last step) and the step's own.
#[derive(Clone, Debug, Default)]
struct History {
    /// The steps made so far, in order.
    steps: Vec<Step>,
    /// Each step's own commit.
    commits: Vec<String>,
}

impl History {
    /// The last commit.
    fn head(&self) -> Option<&str> {
        self.commits.last().map(String::as_str)
    }

    /// The step a commit is the turn of.
    fn step_of(&self, commit: &str) -> Option<&Step> {
        self.commits.iter().position(|c| c == commit).and_then(|at| self.steps.get(at))
    }
}

/// Answers which thread wrote the lines of a file.
#[derive(Clone, Debug)]
pub struct Authorship {
    host: Host,
    git: Option<PathBuf>,
    /// Each tree's history as last made, by its root.
    histories: Arc<tokio::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<History>>>>>,
}

impl Authorship {
    /// The authors of files in `host`'s threads' trees, with `git`; none without git.
    #[must_use]
    pub fn new(host: Host, git: Option<PathBuf>) -> Self {
        Self { host, git, histories: Arc::default() }
    }

    /// Who wrote the lines of `path`: absolute, or relative to `thread`'s repository.
    pub async fn authors(&self, thread: Option<ThreadId>, path: &str) -> Authors {
        let answer = |absent: Option<String>| Authors {
            thread,
            path: path.to_owned(),
            modified_ms: None,
            blob: None,
            runs: Vec::new(),
            absent,
        };
        let Some(git) = self.git.clone() else { return answer(Some(NOT_IN_GIT.to_owned())) };
        let Some(file) = self.resolve(thread, path) else {
            return answer(Some(NOT_IN_GIT.to_owned()));
        };
        // Through symlinks, as the root is found: the file must lie under it by name.
        let file = match tokio::fs::canonicalize(&file).await {
            Ok(file) => file,
            Err(e) => return answer(Some(e.to_string())),
        };
        let Some(root) = file.parent().and_then(crate::repo::root_of) else {
            return answer(Some(NOT_IN_GIT.to_owned()));
        };
        let Ok(relative) = file.strip_prefix(&root) else {
            return answer(Some(NOT_IN_GIT.to_owned()));
        };
        let relative = relative.to_string_lossy().into_owned();
        let meta = match tokio::fs::metadata(&file).await {
            Ok(meta) => meta,
            Err(e) => return answer(Some(e.to_string())),
        };
        if meta.len() > BLAME_BYTES {
            return answer(Some("Too large to say who wrote it".to_owned()));
        }
        // As a file tile was told it, so the two meet.
        let modified_ms = Some(crate::listing::modified_ms(&meta));
        // Blame reads no index, and none is made: the thread's own are the snapshots'.
        let repo = Repo { git, root: root.clone(), index: PathBuf::new() };
        let blob = repo.blob_of(&relative).await.ok();
        let wrote = match self.wrote(&repo, &root, &relative).await {
            Ok(wrote) => wrote,
            Err(e) => {
                tracing::debug!(path = %file.display(), "who wrote it is not known: {e}");
                Vec::new()
            }
        };
        Authors {
            thread,
            path: path.to_owned(),
            modified_ms,
            blob,
            runs: runs(&wrote),
            absent: None,
        }
    }

    /// The file `path` names: itself when absolute, else in `thread`'s repository.
    fn resolve(&self, thread: Option<ThreadId>, path: &str) -> Option<PathBuf> {
        let path = Path::new(path);
        if path.is_absolute() {
            return Some(path.to_owned());
        }
        let cwd = self.host.update(thread?, |s| (vec![], s.meta.cwd.clone()))?;
        Some(crate::repo::root_of(Path::new(&cwd))?.join(path))
    }

    /// Who wrote each line of `relative` in the tree at `root`, from the turns' history, then
    /// from the person's own for the lines it leaves.
    async fn wrote(
        &self,
        repo: &Repo,
        root: &Path,
        relative: &str,
    ) -> Result<Vec<Option<Wrote>>, Failed> {
        // In the order the turns ended: the history of the tree.
        let mut steps: Vec<Step> =
            self.host.visit(|s| Some(steps_of(s, root))).into_iter().flatten().collect();
        steps.sort_by(|a, b| {
            a.at.cmp(&b.at).then(a.thread.cmp(&b.thread)).then(a.turn.cmp(&b.turn))
        });
        let history = self.history(repo, root, steps).await?;
        let mut wrote: Vec<Option<Wrote>> = match history.head() {
            Some(head) => match repo.blame(head, relative).await {
                Ok(lines) => lines
                    .iter()
                    .map(|c| {
                        let step = history.step_of(c.as_deref()?)?;
                        Some(Wrote {
                            thread: step.thread,
                            turn: Some(step.turn),
                            commit: None,
                            at: step.at,
                        })
                    })
                    .collect(),
                // A file no turn's tree holds: none of its lines are a turn's.
                Err(_none) => Vec::new(),
            },
            None => Vec::new(),
        };
        if wrote.iter().all(Option::is_some) && !wrote.is_empty() {
            return Ok(wrote);
        }
        let Ok(committed) = repo.blame("HEAD", relative).await else { return Ok(wrote) };
        if wrote.len() < committed.len() {
            wrote.resize(committed.len(), None);
        }
        let asked: Vec<String> = {
            let mut asked: Vec<String> = committed
                .iter()
                .zip(&wrote)
                .filter(|(_, w)| w.is_none())
                .filter_map(|(c, _)| c.clone())
                .collect();
            asked.sort_unstable();
            asked.dedup();
            asked
        };
        let trailed = repo.trailed(&asked).await?;
        for (slot, commit) in wrote.iter_mut().zip(&committed) {
            if slot.is_some() {
                continue;
            }
            let Some(commit) = commit else { continue };
            if let Some((thread, at)) = trailed.get(commit) {
                *slot = Some(Wrote {
                    thread: *thread,
                    turn: None,
                    commit: Some(commit.chars().take(12).collect()),
                    at: *at,
                });
            }
        }
        Ok(wrote)
    }

    /// The history of `steps` in the tree at `root`: the one made before as far as it agrees,
    /// and commits for the rest.
    async fn history(&self, repo: &Repo, root: &Path, steps: Vec<Step>) -> Result<History, Failed> {
        let held = Arc::clone(self.histories.lock().await.entry(root.to_owned()).or_default());
        // One tree's history is made by one ask at a time: the files a review shows are asked
        // together, and each would make the same commits.
        let mut history = held.lock().await;
        let kept = history.steps.iter().zip(&steps).take_while(|(a, b)| a == b).count();
        history.steps.truncate(kept);
        history.commits.truncate(kept);
        for step in steps.into_iter().skip(kept) {
            let last_after = history.steps.last().map(|s| s.after.clone());
            let mut parent = history.head().map(str::to_owned);
            if last_after.as_ref() != Some(&step.before) {
                let between = repo
                    .commit_at(&step.before, parent.as_deref(), "slopty: between turns", step.at)
                    .await?;
                parent = Some(between);
            }
            let message = format!("slopty: thread {} turn {}", step.thread, step.turn.0);
            let own = repo.commit_at(&step.after, parent.as_deref(), &message, step.at).await?;
            history.commits.push(own);
            history.steps.push(step);
        }
        Ok(history.clone())
    }
}

/// `wrote` as runs of lines with one author each, numbered from 1.
fn runs(wrote: &[Option<Wrote>]) -> Vec<AuthorRun> {
    let mut runs: Vec<AuthorRun> = Vec::new();
    for (at, who) in wrote.iter().enumerate() {
        let Some(who) = who else { continue };
        let line = u32::try_from(at).unwrap_or(u32::MAX).saturating_add(1);
        if let Some(last) = runs.last_mut()
            && last.start.saturating_add(last.lines) == line
            && last.thread == who.thread
            && last.turn == who.turn
            && last.commit == who.commit
        {
            last.lines = last.lines.saturating_add(1);
            continue;
        }
        runs.push(AuthorRun {
            start: line,
            lines: 1,
            thread: who.thread,
            turn: who.turn,
            commit: who.commit.clone(),
            at_ms: who.at,
        });
    }
    runs
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::thread::{ThreadId, TurnId};

    use super::{Wrote, runs};

    #[test]
    fn lines_by_one_author_run_together() {
        let thread = ThreadId::new();
        let by = |turn: u32| {
            Some(Wrote { thread, turn: Some(TurnId(turn)), commit: None, at: WallMs::ZERO })
        };
        let made = runs(&[by(1), by(1), None, by(2), by(2), by(1)]);
        let spans: Vec<(u32, u32, Option<TurnId>)> =
            made.iter().map(|r| (r.start, r.lines, r.turn)).collect();
        assert_eq!(
            spans,
            [(1, 2, Some(TurnId(1))), (4, 2, Some(TurnId(2))), (6, 1, Some(TurnId(1)))]
        );
    }
}
