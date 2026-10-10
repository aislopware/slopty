//! Declared projects in the navigator: each heads its group with its board's row, and is
//! listed with that row alone where none of its tiles is here. A thread at work with no tile
//! here lists under its project. A project's row shows the project on the tab it was left on,
//! a tile's row goes to its tab and pane, and a row carried to a pane's edge splits it.

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

/// Once the orchestrator has said where the goal stands, the board's row carries the first line
/// of it under the tasks' words, so a phone's home shows each goal's line.
#[gpui::test]
fn a_boards_row_carries_where_its_goal_stands(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (worker, studio) = studio(&view, cx);
    let orchestrator = SessionId::new();
    let _tile = opens_in(&view, cx, &studio, orchestrator, studio.me, 1, Some("/w/board"));
    declare(&view, cx, worker, orchestrator);
    let one = cx.debug_bounds("nav-board-board").expect("the row");
    assert!(cx.debug_bounds("nav-board-summary-board").is_none(), "nothing said yet");
    let term = TermRef { worker, session: orchestrator };
    let mut record = project("board", Some(term));
    record.progress = Some(slopty_proto::project::Progress {
        summary: "\nStore merged, wiring the board\nthen the goldens".to_owned(),
        next: None,
        done: false,
        at_ms: WallMs::from_millis(1),
    });
    view.update_in(cx, |v, _w, cx| {
        v.projects_part(snapshot(2, vec![status(record, vec![], vec![])]), cx);
    });
    cx.run_until_parked();
    let two = cx.debug_bounds("nav-board-board").expect("the row");
    let words = cx.debug_bounds("nav-board-words-board").expect("the tasks' words");
    let line = cx.debug_bounds("nav-board-summary-board").expect("the goal's line");
    assert!(two.size.height > one.size.height, "a second line: {one:?} {two:?}");
    assert!(line.top() >= words.bottom() - px(0.5), "under the words: {words:?} {line:?}");
    cx.update(|window, _cx| window.set_a11y_active(true));
    cx.run_until_parked();
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    let label = "Board, No tasks yet, Store merged, wiring the board";
    assert!(tree.iter().any(|n| n.is("Button", Some(label))), "its first line: {tree:#?}");
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
    // Its terminal runs on the worker, with no tile here.
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.session_opened(key, summary(orchestrator, None), cx));
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
        buttons: Vec::new(),
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

/// On a phone the home lists the projects, then the workers with what is in no project.
#[gpui::test]
fn on_a_phone_the_home_lists_the_projects_then_the_workers(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let (_, atlas) = shell_in(&view, cx, &studio, 1, "/w/atlas", true);
    let _loose = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    cx.simulate_resize(size(px(390.0), px(760.0)));
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert!(cx.debug_bounds("navigator").is_some(), "the home is out");
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

/// The key of the group `tile` lists under, as its header's selector.
fn head_of(view: &Entity<WorkspaceView>, cx: &VisualTestContext, tile: TileRef) -> &'static str {
    let key = view.read_with(cx, |v, _| v.project_groups().group_of(tile).map(|g| g.key.clone()));
    leak(format!("nav-group-{}", key.expect("its group")))
}

fn shown_project(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> Option<usize> {
    view.read_with(cx, |v, _| v.layout().shown_index())
}

/// A project's row shows that project on the tab it was left on, and its chevron folds it
/// without going there.
#[gpui::test]
fn a_project_row_shows_its_project_on_the_tab_it_was_left_on(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let (_, first) = shell_in(&view, cx, &studio, 1, "/w/atlas", true);
    let (_, second) = shell_in(&view, cx, &studio, 2, "/w/atlas", true);
    on_new_tab(&view, cx, second);
    let left_on = pos_of(&view, cx, second).tab;
    assert_ne!(pos_of(&view, cx, first).tab, left_on, "two tabs");
    let (_, bolt) = shell_in(&view, cx, &studio, 3, "/w/bolt", false);
    let (atlas_row, bolt_row) = (head_of(&view, cx, first), head_of(&view, cx, bolt));

    click(cx, bolt_row);
    assert_eq!(shown_project(&view, cx), Some(pos_of(&view, cx, bolt).project), "bolt's");
    assert_eq!(focused(&view, cx), Some(bolt));
    click(cx, atlas_row);
    assert_eq!(shown_project(&view, cx), Some(pos_of(&view, cx, first).project), "atlas's");
    let tab = view.read_with(cx, |v, _| v.layout().shown_tab().map(slopty_client::layout::Tab::id));
    assert_eq!(tab, Some(left_on), "on the tab it was left on");
    assert_eq!(focused(&view, cx), Some(second));

    let at = cx.debug_bounds(bolt_row).expect("bolt's row").center();
    cx.simulate_mouse_move(at, None, Modifiers::default());
    cx.run_until_parked();
    click(cx, leak(bolt_row.replacen("nav-group-", "nav-group-fold-", 1)));
    assert!(cx.debug_bounds(selector("nav-tile", bolt.item)).is_none(), "folded");
    assert_eq!(shown_project(&view, cx), Some(pos_of(&view, cx, first).project), "still atlas");
}

/// A tile's row goes to its project, its tab and its pane, from another project on show.
#[gpui::test]
fn a_tile_row_goes_to_its_tab_and_pane(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let (_, first) = shell_in(&view, cx, &studio, 1, "/w/atlas", true);
    let (_, second) = shell_in(&view, cx, &studio, 2, "/w/atlas", true);
    on_new_tab(&view, cx, second);
    let (_, bolt) = shell_in(&view, cx, &studio, 3, "/w/bolt", false);
    click(cx, head_of(&view, cx, bolt));
    assert_eq!(focused(&view, cx), Some(bolt), "bolt on show");

    click(cx, selector("nav-tile", first.item));
    let at = pos_of(&view, cx, first);
    assert_eq!(shown_project(&view, cx), Some(at.project), "its project");
    let tab = view.read_with(cx, |v, _| v.layout().shown_tab().map(slopty_client::layout::Tab::id));
    assert_eq!(tab, Some(at.tab), "its tab, not the one atlas was left on");
    assert_eq!(focused(&view, cx), Some(first), "its pane, focused");
}

/// A tile's row carried to a pane's edge splits that pane there, its tile in a pane of its own
/// in the tab on show, focused.
#[gpui::test]
fn a_tile_row_dragged_to_a_panes_edge_splits_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let (_, atlas) = shell_in(&view, cx, &studio, 1, "/w/atlas", true);
    let (_, bolt) = shell_in(&view, cx, &studio, 2, "/w/bolt", false);
    view.update(cx, |v, cx| v.focus_tile(atlas, cx));
    cx.run_until_parked();
    let pane = cx.debug_bounds(selector("item", atlas.item)).expect("atlas drawn");
    let from = cx.debug_bounds(selector("nav-tile", bolt.item)).expect("bolt's row").center();
    let to = point(pane.right() - px(6.0), pane.center().y);
    let left = gpui::MouseButton::Left;
    cx.simulate_mouse_down(from, left, Modifiers::default());
    cx.simulate_mouse_move(point(from.x + px(12.0), from.y), Some(left), Modifiers::default());
    cx.simulate_mouse_move(to, Some(left), Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("drop-wash").is_some(), "the panel it would become is washed");
    cx.simulate_mouse_up(to, left, Modifiers::default());
    cx.run_until_parked();

    let (a, b) = (pos_of(&view, cx, atlas), pos_of(&view, cx, bolt));
    assert_eq!((a.project, a.tab), (b.project, b.tab), "in atlas's tab");
    assert_ne!(a.pane, b.pane, "a pane of its own");
    let (left_pane, right_pane) = (
        cx.debug_bounds(selector("item", atlas.item)).expect("atlas"),
        cx.debug_bounds(selector("item", bolt.item)).expect("bolt"),
    );
    assert!(left_pane.right() <= right_pane.left(), "split on the right edge");
    assert_eq!(focused(&view, cx), Some(bolt), "focused");
}

/// A project's row with no tile of it here (its threads at work elsewhere) opens an agent's
/// composer in a tab of its own in that project, in its place on the machine.
#[gpui::test]
fn a_project_row_with_no_tile_here_opens_a_composer_there(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let (_, atlas) = shell_in(&view, cx, &studio, 1, "/w/atlas", true);
    view.update_in(cx, |v, _w, cx| {
        v.set_worker_caps(key, healthy(), cx);
        v.threads_linked(key, cx);
    });
    let elsewhere = thread_at("/w/docs", None);
    let table = TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 1 }, rows: vec![elsewhere] };
    view.update_in(cx, |v, _w, cx| v.thread_table(key, &table, cx));
    cx.run_until_parked();
    let docs = GroupKey::new(fact::FOLDER, &groups::at(key, "/w/docs"));

    click(cx, leak(format!("nav-group-{docs}")));
    let start = focused(&view, cx).expect("the composer has the focus");
    assert_ne!(start, atlas);
    let cwd = view.read_with(cx, |v, _| v.starting.get(start.item).map(|s| s.cwd.clone()));
    assert_eq!(cwd.as_deref(), Some("/w/docs"), "in its place");
    let project = view.read_with(cx, |v, _| {
        let at = v.layout().position(start)?;
        v.layout().projects().get(at.project).map(|p| p.home().clone())
    });
    assert_eq!(project, Some(docs), "in a tab of that project");
}

/// A project's head says what its working trees have changed, each checkout once however many
/// of its shells are listed (`MonoCode`'s project card). Its menu pins it above the rest, in the
/// order pinned, and mutes its notifications; a glyph after its name says each, and both are
/// saved with the navigator.
#[gpui::test]
fn a_projects_head_shows_its_changes_and_pins_and_mutes_it(cx: &mut TestAppContext) {
    use slopty_proto::terminal::RepoChanges;

    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let (one, atlas) = shell_in(&view, cx, &fake, 1, "/w/atlas", true);
    let (two, _) = shell_in(&view, cx, &fake, 2, "/w/atlas", true);
    let (_, bolt) = shell_in(&view, cx, &fake, 3, "/w/bolt", true);
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        for session in [one, two] {
            let summary = SessionSummary {
                repo: Some("/w/atlas".to_owned()),
                changes: Some(RepoChanges { files: 2, added: 12, removed: 3 }),
                ..summary(session, Some("/w/atlas"))
            };
            v.session_opened(key, summary, cx);
        }
    });
    cx.run_until_parked();
    let group = |cx: &mut VisualTestContext, tile: TileRef| {
        view.read_with(cx, |v, _| v.project_groups().group_of(tile).map(|g| g.key.clone()))
            .expect("a project")
    };
    let (a, b) = (group(cx, atlas), group(cx, bolt));
    let head = |k: &GroupKey| leak(format!("nav-group-{k}"));
    assert!(cx.debug_bounds(leak(format!("nav-group-changes-{a}"))).is_some(), "its changes");
    assert!(cx.debug_bounds(leak(format!("nav-group-changes-{b}"))).is_none(), "none changed");
    assert!(top(cx, head(&a)) < top(cx, head(&b)), "by name first");

    let pick = |cx: &mut VisualTestContext, k: &GroupKey, row: &str| {
        let at = cx.debug_bounds(head(k)).expect("the head").center();
        cx.simulate_mouse_down(at, gpui::MouseButton::Right, Modifiers::default());
        cx.run_until_parked();
        click(cx, leak(format!("menu-{row}")));
    };
    pick(cx, &b, "Pin to top");
    assert!(top(cx, head(&b)) < top(cx, head(&a)), "the pinned above the rest");
    assert!(cx.debug_bounds(leak(format!("nav-group-pin-{b}"))).is_some(), "its pin");
    pick(cx, &a, "Mute notifications");
    assert!(cx.debug_bounds(leak(format!("nav-group-muted-{a}"))).is_some(), "its mute");
    let look = view.read_with(cx, |v, _| v.attention_look());
    assert!(look.muted.contains(a.as_str()), "its moments post nothing: {:?}", look.muted);
    let saved = view.read_with(cx, |v, _| v.navigator().clone());
    assert_eq!((saved.pinned, saved.muted), (vec![b.clone()], vec![a.clone()]), "saved");
    pick(cx, &b, "Unpin");
    assert!(top(cx, head(&a)) < top(cx, head(&b)), "back in its place by name");
}
