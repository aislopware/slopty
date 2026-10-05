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

use std::collections::HashSet;

use gpui::{Context, Window};
use slopty_client::layout::WorkerKey;
use slopty_proto::git::GitOp;
use slopty_proto::items::{ItemKind, ItemOp};

use super::WorkspaceView;
use super::actions::RemoveWorktree;
use crate::conversation::thread::git::Said;

/// The palette's line.
pub(crate) const REMOVE_WORKTREE: &str = "Remove this worktree";

/// Where an agent's worktrees are, under their clone's root.
const UNDER: &str = "/.claude/worktrees/";

/// The removals asked and not yet answered: each worktree's root, on its machine.
#[derive(Default)]
pub(super) struct Asked(HashSet<(WorkerKey, String)>);

/// The root of the agent's worktree `path` is in or at, under its clone's
/// `.claude/worktrees/`; `None` for a path in none.
#[must_use]
pub(crate) fn worktree_root(path: &str) -> Option<String> {
    let (clone, rest) = path.split_once(UNDER)?;
    let name = rest.split('/').next().filter(|name| !name.is_empty())?;
    Some(format!("{clone}{UNDER}{name}"))
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
            ItemKind::Folder { path } | ItemKind::Changes { path } => path.clone(),
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
        self.worktrees.0.insert((key, root.to_owned()));
        let hub = self.thread_hub(key, cx);
        let _asked = hub.update(cx, |hub, cx| hub.git_op(root, GitOp::RemoveWorktree, cx));
    }

    /// `key`'s repository `repo` moved: a removal asked of it that has its answer is said, and
    /// once it went, the folder tiles in it close.
    pub(super) fn worktree_heard(&mut self, key: WorkerKey, repo: &str, cx: &mut Context<Self>) {
        let asked = (key, repo.to_owned());
        if !self.worktrees.0.contains(&asked) {
            return;
        }
        let Some(hub) = self.held_hub(key) else { return };
        let git = hub.read(cx).git();
        if git.busy(repo).is_some() {
            return;
        }
        let said = git.repo(repo).and_then(|r| r.said.as_ref()).map(|(_, said)| said.clone());
        self.worktrees.0.remove(&asked);
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

    /// Close the folder and changes tiles on `key` that show `root` or a folder in it.
    fn close_tiles_in(&mut self, key: WorkerKey, root: &str, cx: &mut Context<Self>) {
        let gone: Vec<_> = self
            .items()
            .filter(|(worker, item)| {
                *worker == key
                    && matches!(&item.kind,
                        ItemKind::Folder { path } | ItemKind::Changes { path } if within(path, root))
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
