//! Freeing a worktree the person started work in (`GitOp::RemoveWorktree`).
//!
//! "Remove this worktree" offers itself where the focus works in an agent's worktree under its
//! clone's `.claude/worktrees/`: a folder tile or a changes tile in it, or a thread's or a
//! review's tile whose agent works there. The same removal is a button where it is wanted at a
//! glance: on a thread whose agent has exited, and in the path bar of a folder tile in one. It is
//! refused here while an agent that has not exited works in it, since an agent driven over its
//! protocol has no terminal for the worker to see, and on the worker while a terminal works in it
//! or anything in it is not committed. What went and what stayed is said in a notice, and the
//! folder tiles left showing it close.
//!
//! "Remove merged worktrees" sweeps the clone the focus is in (`GitOp::Worktrees`): every agent's
//! worktree whose work has landed, with nothing in it not committed, no terminal in it and no
//! agent that has not exited working there, is asked to go as one would be alone. One notice
//! says how many went and why any stayed, once every answer is in.

use std::collections::HashSet;

use gpui::{Context, Window};
use slopty_client::layout::WorkerKey;
use slopty_proto::git::GitOp;
use slopty_proto::items::{ItemKind, ItemOp};

use super::WorkspaceView;
use super::actions::{RemoveMerged, RemoveWorktree};
use crate::conversation::thread::git::Said;

/// The palette's line.
pub(crate) const REMOVE_WORKTREE: &str = "Remove this worktree";
/// The palette's line for the sweep.
pub(crate) const REMOVE_MERGED: &str = "Remove merged worktrees";

/// Where an agent's worktrees are, under their clone's root.
const UNDER: &str = "/.claude/worktrees/";

/// What was asked of the worktrees and is not yet answered.
#[derive(Default)]
pub(super) struct Asked {
    /// The removals: each worktree's root, on its machine.
    removals: HashSet<(WorkerKey, String)>,
    /// The sweeps' listings: each folder they were asked in, on its machine.
    listings: HashSet<(WorkerKey, String)>,
    /// Each sweep whose removals are not all answered.
    sweeps: Vec<Sweep>,
}

/// A sweep's removals, as they are answered.
struct Sweep {
    key: WorkerKey,
    /// The worktrees not answered yet.
    waiting: HashSet<String>,
    /// How many went.
    went: usize,
    /// Why each that stayed did, in the worker's words.
    kept: Vec<String>,
    /// How many of the merged ones were left alone: in use, or with changes not committed.
    passed: usize,
}

/// The root of the agent's worktree `path` is in or at, under its clone's
/// `.claude/worktrees/`; `None` for a path in none.
#[must_use]
pub(crate) fn worktree_root(path: &str) -> Option<String> {
    let (clone, rest) = path.split_once(UNDER)?;
    let name = rest.split('/').next().filter(|name| !name.is_empty())?;
    Some(format!("{clone}{UNDER}{name}"))
}

/// `n` worktrees, in words: "1 worktree", "3 worktrees".
fn count(n: usize) -> String {
    if n == 1 { "1 worktree".to_owned() } else { format!("{n} worktrees") }
}

/// Whether `path` is `root` or under it.
fn within(path: &str, root: &str) -> bool {
    path.strip_prefix(root).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

impl WorkspaceView {
    /// The worktree the focus works in, with its machine: a folder or changes tile in it, or a
    /// thread's or a review's tile whose agent works there. A shell is left out: one standing
    /// in it is what keeps it.
    pub(super) fn worktree_here(&self) -> Option<(WorkerKey, String)> {
        let tile = self.focused()?;
        let path = match &self.item(tile)?.kind {
            ItemKind::Folder { path } | ItemKind::Changes { path, .. } => path.clone(),
            ItemKind::Thread { thread } | ItemKind::Review { thread } => {
                self.thread_place(*thread)?.cwd.clone()?
            }
            _ => return None,
        };
        Some((tile.worker, worktree_root(&path)?))
    }

    /// "Remove this worktree": the focused work's worktree asked to go, unless an agent still
    /// works in it.
    pub(super) fn remove_worktree(
        &mut self,
        _: &RemoveWorktree,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((key, root)) = self.worktree_here() else { return };
        self.remove_worktree_at(key, &root, cx);
    }

    /// The worktree at `root` on `key` asked to go, unless an agent still works in it.
    pub(super) fn remove_worktree_at(
        &mut self,
        key: WorkerKey,
        root: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(thread) = self.threads_working_in(key, root).first() {
            let who = self.thread_named(*thread).unwrap_or_else(|| "An agent".to_owned());
            self.show_notice(format!("{who} still works in this worktree; end it first"), cx);
            return;
        }
        tracing::info!(%key, %root, "remove worktree");
        self.worktrees.removals.insert((key, root.to_owned()));
        let hub = self.thread_hub(key, cx);
        let _asked = hub.update(cx, |hub, cx| hub.git_op(root, GitOp::RemoveWorktree, cx));
    }

    /// "Remove merged worktrees": the agents' worktrees of the clone the focus is in, listed with
    /// their state, so the merged ones can go.
    pub(super) fn remove_merged(
        &mut self,
        _: &RemoveMerged,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((key, path)) = self.changes_here() else { return };
        tracing::info!(%key, %path, "remove merged worktrees");
        self.worktrees.listings.insert((key, path.clone()));
        let hub = self.thread_hub(key, cx);
        let _asked = hub.update(cx, |hub, cx| hub.git_op(&path, GitOp::Worktrees, cx));
    }

    /// `key`'s repository `repo` moved: a sweep's listing that has its answer asks each merged
    /// worktree free to go; a removal that has its answer is said (or counted in its sweep), and
    /// once it went, the folder tiles in it close.
    pub(super) fn worktree_heard(&mut self, key: WorkerKey, repo: &str, cx: &mut Context<Self>) {
        let asked = (key, repo.to_owned());
        if self.worktrees.listings.contains(&asked) {
            self.listing_heard(key, repo, cx);
        }
        if !self.worktrees.removals.contains(&asked) {
            return;
        }
        let Some(hub) = self.held_hub(key) else { return };
        let git = hub.read(cx).git();
        if git.busy(repo).is_some() {
            return;
        }
        let said = git.repo(repo).and_then(|r| r.said.as_ref()).map(|(_, said)| said.clone());
        self.worktrees.removals.remove(&asked);
        if let Some(at) =
            self.worktrees.sweeps.iter().position(|s| s.key == key && s.waiting.contains(repo))
        {
            self.swept(at, repo, said, cx);
            return;
        }
        match said {
            Some(Said::Freed { said }) => {
                self.show_notice(said, cx);
                self.close_tiles_in(key, repo, cx);
            }
            Some(Said::Refused { why }) => {
                self.show_failure(format!("Kept the worktree: {why}"), cx);
            }
            Some(Said::Failed { said }) => {
                self.show_failure(format!("The worktree was not removed: {said}"), cx);
            }
            _ => {}
        }
    }

    /// A sweep's listing of `repo` on `key` came: each merged worktree free to go is asked to,
    /// and one with changes not committed, a terminal in it or an agent working there is passed
    /// over. With none to remove, that is said at once.
    fn listing_heard(&mut self, key: WorkerKey, repo: &str, cx: &mut Context<Self>) {
        let Some(hub) = self.held_hub(key) else { return };
        let git = hub.read(cx).git();
        if git.asking(repo, &GitOp::Worktrees) {
            return;
        }
        let found = git.repo(repo);
        let listed = found.and_then(|r| r.worktrees.clone());
        let why = found.and_then(|r| r.said.as_ref()).and_then(|(_, said)| match said {
            Said::Refused { why } | Said::Failed { said: why } => Some(why.clone()),
            _ => None,
        });
        self.worktrees.listings.remove(&(key, repo.to_owned()));
        let Some(listed) = listed else {
            let why = why.unwrap_or_else(|| "the machine did not answer".to_owned());
            self.show_failure(format!("The worktrees could not be listed: {why}"), cx);
            return;
        };
        let (free, held): (Vec<_>, Vec<_>) = listed
            .list
            .iter()
            .filter(|w| w.merged)
            .partition(|w| w.removable() && self.threads_working_in(key, &w.path).is_empty());
        if free.is_empty() {
            let said = match held.len() {
                0 => "No merged worktree to remove".to_owned(),
                n => format!("No merged worktree to remove; {} in use or not committed", count(n)),
            };
            self.show_notice(said, cx);
            return;
        }
        let waiting: HashSet<String> = free.iter().map(|w| w.path.clone()).collect();
        let sweep =
            Sweep { key, waiting: waiting.clone(), went: 0, kept: Vec::new(), passed: held.len() };
        self.worktrees.sweeps.push(sweep);
        for path in waiting {
            self.worktrees.removals.insert((key, path.clone()));
            let hub = self.thread_hub(key, cx);
            let _asked = hub.update(cx, |hub, cx| hub.git_op(&path, GitOp::RemoveWorktree, cx));
        }
    }

    /// Sweep `at`'s removal of `repo` was answered `said`; once it was the last, one notice says
    /// how the sweep went.
    fn swept(&mut self, at: usize, repo: &str, said: Option<Said>, cx: &mut Context<Self>) {
        let Some(sweep) = self.worktrees.sweeps.get_mut(at) else { return };
        sweep.waiting.remove(repo);
        let key = sweep.key;
        let went = matches!(said, Some(Said::Freed { .. }));
        match said {
            Some(Said::Freed { .. }) => sweep.went = sweep.went.saturating_add(1),
            Some(Said::Refused { why } | Said::Failed { said: why }) => sweep.kept.push(why),
            _ => sweep.kept.push("the machine did not answer".to_owned()),
        }
        let done = sweep.waiting.is_empty();
        if went {
            self.close_tiles_in(key, repo, cx);
        }
        if !done {
            return;
        }
        let sweep = self.worktrees.sweeps.remove(at);
        let mut said = match sweep.went {
            0 => "Removed no merged worktree".to_owned(),
            1 => "Removed 1 merged worktree".to_owned(),
            n => format!("Removed {n} merged worktrees"),
        };
        if let Some(first) = sweep.kept.first() {
            said = format!("{said}; kept {}: {first}", count(sweep.kept.len()));
        }
        if sweep.passed > 0 {
            said = format!("{said}; {} in use or not committed", count(sweep.passed));
        }
        if sweep.went == 0 {
            self.show_failure(said, cx);
        } else {
            self.show_notice(said, cx);
        }
    }

    /// Close the folder and changes tiles on `key` that show `root` or a folder in it.
    fn close_tiles_in(&mut self, key: WorkerKey, root: &str, cx: &mut Context<Self>) {
        let gone: Vec<_> = self
            .items()
            .filter(|(worker, item)| {
                *worker == key
                    && matches!(&item.kind,
                        ItemKind::Folder { path } | ItemKind::Changes { path, .. } if within(path, root))
            })
            .map(|(_, item)| item.id)
            .collect();
        for item in gone {
            self.propose(key, ItemOp::Remove(item), cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A worktree's root is found from any folder in it, and only under `.claude/worktrees/`.
    #[test]
    fn a_worktrees_root_is_read_from_any_folder_in_it() {
        let root = "/w/atlas/.claude/worktrees/fix-login";
        assert_eq!(worktree_root(root).as_deref(), Some(root));
        assert_eq!(worktree_root(&format!("{root}/src/app")).as_deref(), Some(root));
        assert_eq!(worktree_root("/w/atlas"), None);
        assert_eq!(worktree_root("/w/atlas/.claude/worktrees/"), None);
        assert!(within(&format!("{root}/src"), root));
        assert!(!within(&format!("{root}-two"), root), "a sibling with a longer name");
    }
}
