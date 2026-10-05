//! "Remove this worktree": offered where the focus works in an agent's worktree, refused here
//! while an agent still works in it, asked of the worker, and said once answered.

use slopty_proto::RequestId;
use slopty_proto::git::{GitDone, GitOp, GitOutcome};
use slopty_proto::thread::Cursor;
use slopty_proto::thread::wire::TableFrame;

use super::*;
use crate::workspace::actions::RemoveWorktree;
use crate::workspace::worktrees::REMOVE_WORKTREE;

const ROOT: &str = "/w/atlas/.claude/worktrees/fix-login";

/// The removals the workspace asked of `fake`.
fn removals(fake: &mut Fake) -> Vec<(RequestId, String)> {
    fake.drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Git { request, repo, op: GitOp::RemoveWorktree } => Some((request, repo)),
            _ => None,
        })
        .collect()
}

/// A folder tile inside a worktree offers its removal; the worktree's root is what is asked.
/// While a live agent's thread works there, the removal is refused here in words and nothing
/// is asked. Once its agent has exited, it is asked; the worker's refusal is said, and once it
/// went, what went is said and the folder tile in it closes. A folder in no worktree offers
/// nothing.
#[gpui::test]
fn a_worktree_is_removed_from_a_folder_in_it_once_no_agent_works_there(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let plain = arrives(&view, cx, &studio, ItemKind::Folder { path: "/w/atlas".into() }, 1);
    view.update_in(cx, |v, _w, cx| v.focus_tile(plain, cx));
    cx.run_until_parked();
    let offered = |cx: &mut VisualTestContext| {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| v.offered_lines(window, cx))
            .iter()
            .any(|l| l.label == REMOVE_WORKTREE)
    };
    assert!(!offered(cx), "a folder in no worktree");

    let inside = ItemKind::Folder { path: format!("{ROOT}/src") };
    let folder = arrives(&view, cx, &studio, inside, 2);
    view.update_in(cx, |v, _w, cx| v.focus_tile(folder, cx));
    cx.run_until_parked();
    assert!(offered(cx), "a folder in an agent's worktree");

    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.cwd = ROOT.to_owned();
    let table = |state: &slopty_proto::thread::ThreadState, seq| TableFrame::Snapshot {
        cursor: Cursor { epoch: 1, seq },
        rows: vec![state.row(WallMs::ZERO)],
    };
    view.update_in(cx, |v, _w, cx| v.thread_table(key, &table(&state, 1), cx));
    cx.run_until_parked();
    studio.drain();
    cx.dispatch_action(RemoveWorktree);
    cx.run_until_parked();
    assert!(removals(&mut studio).is_empty(), "nothing asked while its agent works there");
    let told = view.read_with(cx, |v, _| v.toast_text()).unwrap_or_default();
    assert!(told.contains("still works in this worktree"), "{told}");

    state.status.liveness = slopty_proto::thread::Liveness::Exited { resumable: true };
    view.update_in(cx, |v, _w, cx| v.thread_table(key, &table(&state, 2), cx));
    cx.run_until_parked();
    cx.dispatch_action(RemoveWorktree);
    cx.run_until_parked();
    let asked = removals(&mut studio);
    let [(request, repo)] = asked.as_slice() else { panic!("one removal: {asked:?}") };
    assert_eq!(repo, ROOT, "the worktree's root, not the folder in it");
    let why = "/w/atlas/.claude/worktrees/fix-login has changes not committed: ?? draft.txt";
    let refused = GitOutcome::Refused { why: why.to_owned() };
    view.update_in(cx, |v, _w, cx| v.git_done(key, *request, refused, cx));
    cx.run_until_parked();
    let told = view.read_with(cx, |v, _| v.toast_text()).unwrap_or_default();
    assert!(told.starts_with("Kept the worktree: ") && told.contains("draft.txt"), "{told}");
    assert!(
        !studio.drain().iter().any(|m| matches!(m, ClientMsg::Git { op: GitOp::Status, .. })),
        "no status asked of a worktree asked to go"
    );

    cx.dispatch_action(RemoveWorktree);
    cx.run_until_parked();
    let asked = removals(&mut studio);
    let [(request, _)] = asked.as_slice() else { panic!("one removal: {asked:?}") };
    let done = GitOutcome::Done(GitDone::WorktreeRemoved {
        branch: Some("worktree-fix-login".to_owned()),
        branch_removed: false,
    });
    view.update_in(cx, |v, _w, cx| v.git_done(key, *request, done, cx));
    cx.run_until_parked();
    let told = view.read_with(cx, |v, _| v.toast_text()).unwrap_or_default();
    assert_eq!(
        told,
        "Removed the worktree; kept its branch worktree-fix-login, which holds work not merged"
    );
    let closed: Vec<ItemOp> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Items(op @ ItemOp::Remove(_)) => Some(op),
            _ => None,
        })
        .collect();
    assert_eq!(closed, [ItemOp::Remove(folder.item)], "the folder in it closes, the other stays");
}
