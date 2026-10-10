//! "Remove this worktree": offered where the focus works in an agent's worktree, refused here
//! while an agent still works in it, asked of the worker, and said once answered.

use slopty_proto::RequestId;
use slopty_proto::git::{GitDone, GitOp, GitOutcome};
use slopty_proto::thread::Cursor;
use slopty_proto::thread::wire::{TableFrame, ThreadFrame};

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

/// The exited agent's thread offers "Remove worktree" beside its way back, and the folder tile
/// in the worktree "Remove this worktree" in its path bar, an icon named for it. Either asks the
/// worktree's root of its worker, and the worker's refusal of work not committed is said in
/// words, the worktree kept. A live agent's thread offers no button.
#[gpui::test]
fn the_exited_thread_and_the_folder_in_a_worktree_offer_its_removal(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.cwd = format!("{ROOT}/src");
    state.meta.terminal = None;
    let thread = state.meta.id;
    let tile = arrives(&view, cx, &studio, ItemKind::Thread { thread }, 1);
    let show = |state: &slopty_proto::thread::ThreadState, seq, cx: &mut VisualTestContext| {
        let rows = TableFrame::Snapshot {
            cursor: Cursor { epoch: 1, seq },
            rows: vec![state.row(WallMs::ZERO)],
        };
        view.update_in(cx, |v, _w, cx| v.thread_table(key, &rows, cx));
        let frame = ThreadFrame::Snapshot {
            cursor: Cursor { epoch: 1, seq },
            state: Box::new(state.clone()),
        };
        view.update_in(cx, |v, _w, cx| v.thread_frame(key, thread, frame, cx));
        cx.run_until_parked();
    };
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    show(&state, 1, cx);
    assert!(cx.debug_bounds("thread-remove-worktree").is_none(), "not while its agent runs");

    state.status.liveness = slopty_proto::thread::Liveness::Exited { resumable: true };
    show(&state, 2, cx);
    studio.drain();
    let at = cx.debug_bounds("thread-remove-worktree").expect("beside the way back");
    cx.simulate_click(at.center(), Modifiers::none());
    cx.run_until_parked();
    let asked = removals(&mut studio);
    let [(request, repo)] = asked.as_slice() else { panic!("one removal: {asked:?}") };
    assert_eq!(repo, ROOT, "the worktree's root, from the folder the agent worked in");
    let why = "/w/atlas/.claude/worktrees/fix-login has changes not committed: M src/lib.rs";
    let refused = GitOutcome::Refused { why: why.to_owned() };
    view.update_in(cx, |v, _w, cx| v.git_done(key, *request, refused, cx));
    cx.run_until_parked();
    let told = view.read_with(cx, |v, _| v.toast_text()).unwrap_or_default();
    assert!(told.starts_with("Kept the worktree: ") && told.contains("not committed"), "{told}");

    let folder = arrives(&view, cx, &studio, ItemKind::Folder { path: ROOT.to_owned() }, 2);
    let listed = slopty_proto::folder::Listing::Listed {
        dir: ROOT.to_owned(),
        entries: Vec::new(),
        total: 0,
    };
    view.update_in(cx, |v, _w, cx| v.folder_listed(key, ROOT, &listed, cx));
    view.update_in(cx, |v, _w, cx| v.focus_tile(folder, cx));
    cx.run_until_parked();
    studio.drain();
    let nodes = tree(cx);
    assert!(nodes.iter().any(|n| n.is("Button", Some(REMOVE_WORKTREE))), "named: {nodes:#?}");
    let button = leak(format!("folder-remove-worktree-{}", folder.item.as_uuid()));
    let at = cx.debug_bounds(button).expect("in the folder's path bar");
    cx.simulate_click(at.center(), Modifiers::none());
    cx.run_until_parked();
    let asked = removals(&mut studio);
    let [(_, repo)] = asked.as_slice() else { panic!("one removal: {asked:?}") };
    assert_eq!(repo, ROOT);
}

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

/// One of the clone's worktrees as the worker lists it.
fn listed(name: &str, merged: bool, changed: u32) -> slopty_proto::git::AgentWorktree {
    slopty_proto::git::AgentWorktree {
        path: format!("/w/atlas/.claude/worktrees/{name}"),
        branch: Some(format!("worktree-{name}")),
        changed,
        busy: false,
        ahead: u32::from(!merged),
        merged,
        committed: 1,
        made_by: Some(slopty_proto::thread::AgentId::named(
            slopty_proto::thread::AgentId::CLAUDE_CODE,
        )),
    }
}

/// "Remove merged worktrees", offered in a folder of a clone, lists the clone's worktrees and
/// asks to remove each merged one an agent made that is clean, with no terminal and no live
/// agent in it, Codex's as Claude Code's; a merged one with changes not committed, or a live
/// agent's, is passed over, one not merged is left alone, and one the person made by hand is
/// theirs. Once every removal is answered, one notice says what went and whose, what stayed
/// and what was left, and the folder tile in the worktree that went closes.
#[gpui::test]
fn remove_merged_takes_only_the_landed_worktrees_nothing_works_in(cx: &mut TestAppContext) {
    use slopty_proto::git::Worktrees;

    use crate::workspace::actions::RemoveMerged;
    use crate::workspace::worktrees::REMOVE_MERGED;

    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.cwd = "/w/atlas/.claude/worktrees/agent-busy".to_owned();
    let rows = TableFrame::Snapshot {
        cursor: Cursor { epoch: 1, seq: 1 },
        rows: vec![state.row(WallMs::ZERO)],
    };
    view.update_in(cx, |v, _w, cx| v.thread_table(key, &rows, cx));
    let gone = ItemKind::Folder { path: "/w/atlas/.claude/worktrees/landed/src".into() };
    let gone = arrives(&view, cx, &studio, gone, 1);
    let clone = arrives(&view, cx, &studio, ItemKind::Folder { path: "/w/atlas".into() }, 2);
    view.update_in(cx, |v, _w, cx| v.focus_tile(clone, cx));
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let offered = view.update_in(cx, |v, window, cx| v.offered_lines(window, cx));
    assert!(offered.iter().any(|l| l.label == REMOVE_MERGED), "in a clone's folder");

    studio.drain();
    cx.dispatch_action(RemoveMerged);
    cx.run_until_parked();
    let asked: Vec<_> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Git { request, repo, op: GitOp::Worktrees } => Some((request, repo)),
            _ => None,
        })
        .collect();
    let [(request, repo)] = asked.as_slice() else { panic!("one listing: {asked:?}") };
    assert_eq!(repo, "/w/atlas");
    let codex = slopty_proto::git::AgentWorktree {
        path: "/Users/me/.codex/worktrees/1f2e/atlas".to_owned(),
        made_by: Some(slopty_proto::thread::AgentId::named(slopty_proto::thread::AgentId::CODEX)),
        ..listed("codex", true, 0)
    };
    let own = slopty_proto::git::AgentWorktree {
        path: "/w/atlas-hotfix".to_owned(),
        made_by: None,
        ..listed("hotfix", true, 0)
    };
    let list = vec![
        listed("landed", true, 0),
        listed("agent-busy", true, 0),
        listed("draft", true, 2),
        listed("open", false, 0),
        codex.clone(),
        own,
    ];
    let worktrees = Worktrees { clone: "/w/atlas".to_owned(), list, more: 0 };
    let done = GitOutcome::Done(GitDone::Worktrees(Box::new(worktrees)));
    view.update_in(cx, |v, _w, cx| v.git_done(key, *request, done, cx));
    cx.run_until_parked();
    let mut asked = removals(&mut studio);
    asked.sort_by(|a, b| a.1.cmp(&b.1));
    let repos: Vec<&str> = asked.iter().map(|(_, repo)| repo.as_str()).collect();
    assert_eq!(
        repos,
        [codex.path.as_str(), "/w/atlas/.claude/worktrees/landed"],
        "the agents' ones free to go; the person's own is left"
    );

    for (request, _) in &asked {
        let done = GitOutcome::Done(GitDone::WorktreeRemoved {
            branch: Some("worktree-landed".to_owned()),
            branch_removed: true,
        });
        view.update_in(cx, |v, _w, cx| v.git_done(key, *request, done, cx));
    }
    cx.run_until_parked();
    let told = view.read_with(cx, |v, _| v.toast_text()).unwrap_or_default();
    assert_eq!(
        told,
        "Removed 2 merged worktrees (1 Claude Code, 1 Codex); 2 worktrees in use or not \
         committed; 1 worktree of your own left"
    );
    let closed: Vec<ItemOp> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Items(op @ ItemOp::Remove(_)) => Some(op),
            _ => None,
        })
        .collect();
    assert_eq!(closed, [ItemOp::Remove(gone.item)], "the folder in the worktree that went");
}

/// A sweep with nothing merged to take says so at once and asks nothing to go; a listing the
/// worker refused says why.
#[gpui::test]
fn remove_merged_says_when_there_is_nothing_to_take(cx: &mut TestAppContext) {
    use slopty_proto::git::Worktrees;

    use crate::workspace::actions::RemoveMerged;

    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let clone = arrives(&view, cx, &studio, ItemKind::Folder { path: "/w/atlas".into() }, 1);
    view.update_in(cx, |v, _w, cx| v.focus_tile(clone, cx));
    cx.run_until_parked();
    let mut ask = |cx: &mut VisualTestContext| {
        studio.drain();
        cx.dispatch_action(RemoveMerged);
        cx.run_until_parked();
        let asked: Vec<RequestId> = studio
            .drain()
            .into_iter()
            .filter_map(|m| match m {
                ClientMsg::Git { request, op: GitOp::Worktrees, .. } => Some(request),
                _ => None,
            })
            .collect();
        let [request] = asked.as_slice() else { panic!("one listing: {asked:?}") };
        *request
    };

    let request = ask(cx);
    let list = vec![listed("open", false, 0)];
    let worktrees = Worktrees { clone: "/w/atlas".to_owned(), list, more: 0 };
    let done = GitOutcome::Done(GitDone::Worktrees(Box::new(worktrees)));
    view.update_in(cx, |v, _w, cx| v.git_done(key, request, done, cx));
    cx.run_until_parked();
    let told = view.read_with(cx, |v, _| v.toast_text()).unwrap_or_default();
    assert_eq!(told, "No merged worktree to remove");

    let request = ask(cx);
    let refused = GitOutcome::Refused { why: "/w/atlas is in no git repository".to_owned() };
    view.update_in(cx, |v, _w, cx| v.git_done(key, request, refused, cx));
    cx.run_until_parked();
    let told = view.read_with(cx, |v, _| v.toast_text()).unwrap_or_default();
    assert_eq!(told, "The worktrees could not be listed: /w/atlas is in no git repository");
}

/// The merged commit sheet's "End `<agent>` and remove": the agent running in the worktree in a
/// terminal of its own has that terminal closed, and the worktree is asked to go only once its
/// worker's table says the agent exited, so the worker never refuses it for a terminal still
/// there.
#[gpui::test]
fn ending_the_agent_frees_its_worktree_once_it_exited(cx: &mut TestAppContext) {
    use slopty_proto::terminal::TermRequest;

    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let session = SessionId::new();
    let _tile = opens(&view, cx, &studio, session, studio.me, 1);
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.cwd = ROOT.to_owned();
    state.meta.terminal = Some(session);
    let table = |state: &slopty_proto::thread::ThreadState, seq| TableFrame::Snapshot {
        cursor: Cursor { epoch: 1, seq },
        rows: vec![state.row(WallMs::ZERO)],
    };
    view.update_in(cx, |v, _w, cx| v.thread_table(key, &table(&state, 1), cx));
    cx.run_until_parked();
    studio.drain();

    view.update_in(cx, |v, _w, cx| v.end_and_remove(key, ROOT, cx));
    cx.run_until_parked();
    let sent = studio.drain();
    let closed = sent.iter().any(
        |m| matches!(m, ClientMsg::Term { session: s, req: TermRequest::Close } if *s == session),
    );
    assert!(closed, "the agent's terminal is closed: {sent:?}");
    assert!(
        !sent.iter().any(|m| matches!(m, ClientMsg::Git { op: GitOp::RemoveWorktree, .. })),
        "nothing asked while the agent still runs"
    );

    state.status.liveness = slopty_proto::thread::Liveness::Exited { resumable: true };
    view.update_in(cx, |v, _w, cx| v.thread_table(key, &table(&state, 2), cx));
    cx.run_until_parked();
    let asked = removals(&mut studio);
    let [(_, repo)] = asked.as_slice() else { panic!("one removal: {asked:?}") };
    assert_eq!(repo, ROOT, "asked once it exited");
}
