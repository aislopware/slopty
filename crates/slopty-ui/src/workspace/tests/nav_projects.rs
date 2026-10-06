//! Declared projects in the navigator: each heads its group with its board's row, and is
//! listed with that row alone where none of its tiles is here. A thread at work with no tile
//! here lists under its project.

use gpui::Modifiers;
use slopty_client::groups::{self, GroupKey, fact};
use slopty_core::WorkerId;
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::TaskState;
use slopty_proto::thread::wire::{RequestCard, TableFrame, ThreadRow};
use slopty_proto::thread::{AskId, Cursor, Request, ThreadId};

use super::palette::shell_in;
use super::*;
use crate::project::fixtures::{card, on, project, snapshot, status};

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

fn top(cx: &mut VisualTestContext, selector: &'static str) -> f32 {
    let bounds = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"));
    f32::from(bounds.origin.y)
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"));
    cx.simulate_click(at.center(), Modifiers::default());
    cx.run_until_parked();
}

/// A worker whose key is the one its server id maps to, as the app's are.
fn studio(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> (WorkerId, Fake) {
    let worker = WorkerId::new();
    (worker, connect(view, cx, worker.as_uuid().as_u128(), "studio"))
}

/// `board` as the server holds it, its orchestrator in `orchestrator` on `worker`: one task
/// waits on the person, one is merged.
fn declare(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    worker: WorkerId,
    orchestrator: SessionId,
) {
    let term = TermRef { worker, session: orchestrator };
    let tasks = vec![
        on(card(1, "Read the store", TaskState::Blocked), worker, SessionId::new()),
        card(2, "Wire the board", TaskState::Merged),
    ];
    view.update_in(cx, |v, _w, cx| {
        v.projects_part(snapshot(1, vec![status(project("board", Some(term)), tasks, vec![])]), cx);
    });
    cx.run_until_parked();
}

/// A declared project's board leads its group, under its header and above its tiles, with how
/// its tasks stand; a click shows the board in its orchestrator's tile.
#[gpui::test]
fn a_projects_board_heads_its_group(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (worker, studio) = studio(&view, cx);
    let orchestrator = SessionId::new();
    let tile = opens_in(&view, cx, &studio, orchestrator, studio.me, 1, Some("/w/board"));
    declare(&view, cx, worker, orchestrator);
    let group = leak(format!("nav-group-{}", GroupKey::new(fact::PROJECT, "board")));
    let (head, board, row) =
        (top(cx, group), top(cx, "nav-board-board"), top(cx, selector("nav-tile", tile.item)));
    assert!(head < board && board < row, "{head} {board} {row}");
    cx.update(|window, _cx| window.set_a11y_active(true));
    cx.run_until_parked();
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    let label = "Board, 1 needs you · 1 of 2 merged";
    assert!(tree.iter().any(|n| n.is("Button", Some(label))), "{tree:#?}");
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    click(cx, "nav-board-board");
    assert!(view.read_with(cx, |v, _| v.board_shown(orchestrator)), "the board is shown");
}

/// A project whose orchestrator has no tile here is still listed, its board row alone under
/// its header; a click opens the orchestrator's terminal in a tile on its worker and shows the
/// board there.
#[gpui::test]
fn a_project_whose_orchestrator_has_no_tile_here_still_lists_and_opens(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (worker, mut studio) = studio(&view, cx);
    let _other = opens_in(&view, cx, &studio, SessionId::new(), studio.me, 1, Some("/w/site"));
    let orchestrator = SessionId::new();
    declare(&view, cx, worker, orchestrator);
    let group = leak(format!("nav-group-{}", GroupKey::new(fact::PROJECT, "board")));
    assert!(cx.debug_bounds(group).is_some(), "the project is listed");
    studio.drain();
    click(cx, "nav-board-board");
    let opened = studio.drain().into_iter().any(|m| {
        matches!(m, ClientMsg::Items(ItemOp::Add(Item { kind: ItemKind::Terminal { session }, .. }))
            if session == orchestrator)
    });
    assert!(opened, "its orchestrator's terminal is opened in a tile");
    let focused = focused(&view, cx).expect("a tile has the focus");
    let kind = view.read_with(cx, |v, _| v.item(focused).map(|i| i.kind.clone()));
    assert_eq!(kind, Some(ItemKind::Terminal { session: orchestrator }));
    assert!(view.read_with(cx, |v, _| v.board_shown(orchestrator)), "the board is shown");
}

/// A thread at `cwd` in the repository `repo`, on no terminal, waiting on the person.
fn thread_at(cwd: &str, repo: Option<&str>) -> ThreadRow {
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = None;
    let mut row = state.row(WallMs::ZERO);
    row.id = ThreadId::new();
    row.cwd = Some(cwd.to_owned());
    row.repo = repo.map(str::to_owned);
    row.requests = vec![RequestCard {
        id: AskId("ask-1".to_owned()),
        item: None,
        kind: Request::APPROVAL.to_owned(),
        title: "Run `cargo test`".to_owned(),
        options: Vec::new(),
        opened_ms: WallMs::ZERO,
    }];
    row
}

/// A thread at work with no tile here lists under the project its place is in, after that
/// project's tiles; one in a place no tile is in lists under a group of its own. A click opens
/// its tile, and its row gives way to the tile's.
#[gpui::test]
fn a_thread_with_no_tile_lists_under_its_project_and_opens_its_tile(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let (_, atlas) = shell_in(&view, cx, &studio, 1, "/w/atlas", true);
    view.update_in(cx, |v, _w, cx| v.threads_linked(key, cx));
    let here = thread_at("/w/atlas/src", Some("/w/atlas"));
    let elsewhere = thread_at("/w/docs", None);
    let (thread, other) = (here.id, elsewhere.id);
    let table =
        TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 1 }, rows: vec![here, elsewhere] };
    view.update_in(cx, |v, _w, cx| v.thread_table(key, &table, cx));
    cx.run_until_parked();

    let atlas_group = view
        .read_with(cx, |v, _| v.project_groups().group_of(atlas).map(|g| g.key.clone()))
        .expect("atlas's group");
    let (head, shell, row) = (
        top(cx, leak(format!("nav-group-{atlas_group}"))),
        top(cx, selector("nav-tile", atlas.item)),
        top(cx, leak(format!("nav-thread-{thread}"))),
    );
    assert!(head < shell && shell < row, "under atlas, after its tiles: {head} {shell} {row}");
    let docs = GroupKey::new(fact::FOLDER, &groups::at(key, "/w/docs"));
    let (docs_head, docs_row) =
        (top(cx, leak(format!("nav-group-{docs}"))), top(cx, leak(format!("nav-thread-{other}"))));
    assert!(docs_head < docs_row, "a group of its own: {docs_head} {docs_row}");

    studio.drain();
    click(cx, leak(format!("nav-thread-{thread}")));
    let opened = studio.drain().into_iter().any(|m| {
        matches!(m, ClientMsg::Items(ItemOp::Add(Item { kind: ItemKind::Thread { thread: t }, .. }))
            if t == thread)
    });
    assert!(opened, "its tile is opened");
    assert!(
        cx.debug_bounds(leak(format!("nav-thread-{thread}"))).is_none(),
        "its row gives way to its tile's"
    );
}

/// On a phone the drawer lists the projects, then the workers with what is in no project.
#[gpui::test]
fn on_a_phone_the_drawer_lists_the_projects_then_the_workers(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let (_, atlas) = shell_in(&view, cx, &studio, 1, "/w/atlas", true);
    let _loose = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    cx.simulate_resize(size(px(390.0), px(760.0)));
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert!(cx.debug_bounds("navigator").is_some(), "the drawer is out");
    let group = view
        .read_with(cx, |v, _| v.project_groups().group_of(atlas).map(|g| g.key.clone()))
        .expect("atlas's group");
    let order = [
        top(cx, "nav-projects"),
        top(cx, leak(format!("nav-group-{group}"))),
        top(cx, "nav-workers"),
        top(cx, leak(format!("nav-worker-{}", studio.key))),
    ];
    assert!(order.is_sorted(), "projects, then workers: {order:?}");
}
