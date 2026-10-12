//! Carrying in the headless workspace, beyond a pane: a navigator's tile row dropped on a
//! project's row goes to that project, and a thread's row dropped on a pane's edge opens there.
//! Each landing is drawn while the pointer is over it.

use super::palette::shell_in;
use super::*;

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

fn bounds(cx: &mut VisualTestContext, selector: &'static str) -> Bounds<Pixels> {
    cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"))
}

/// Pressed at `from`, moved past the slop and on to `to`, `over` asked there, then let go.
fn carry(
    cx: &mut VisualTestContext,
    from: Point<Pixels>,
    to: Point<Pixels>,
    over: impl FnOnce(&mut VisualTestContext),
) {
    let left = gpui::MouseButton::Left;
    cx.simulate_mouse_down(from, left, Modifiers::default());
    cx.simulate_mouse_move(point(from.x + px(12.0), from.y), Some(left), Modifiers::default());
    cx.simulate_mouse_move(to, Some(left), Modifiers::default());
    cx.run_until_parked();
    over(cx);
    cx.simulate_mouse_up(to, left, Modifiers::default());
    cx.run_until_parked();
}

/// A point a little inside `b`'s leading or trailing edge, at its middle height.
fn near(b: Bounds<Pixels>, trailing: bool) -> Point<Pixels> {
    let x = if trailing { b.right() - px(4.0) } else { b.left() + px(4.0) };
    point(x, b.center().y)
}

/// A navigator's tile row carried onto another project's row goes to that project in a tab of
/// its own, shown and focused; over its own project's row, nothing is washed and nothing moves.
#[gpui::test]
fn a_tile_row_dropped_on_a_project_row_moves_the_tile_there(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let (_, atlas) = shell_in(&view, cx, &fake, 1, "/w/atlas", true);
    let (_, bolt) = shell_in(&view, cx, &fake, 2, "/w/bolt", false);
    let group = |view: &Entity<WorkspaceView>, cx: &VisualTestContext, tile: TileRef| {
        let key =
            view.read_with(cx, |v, _| v.project_groups().group_of(tile).map(|g| g.key.clone()));
        leak(format!("nav-group-{}", key.expect("its group")))
    };
    let (atlas_row, bolt_row) = (group(&view, cx, atlas), group(&view, cx, bolt));

    let row = bounds(cx, selector("nav-tile", bolt.item)).center();
    let to = bounds(cx, bolt_row).center();
    carry(cx, row, to, |cx| {
        assert!(cx.debug_bounds("nav-group-drop").is_none(), "its own project is no landing");
    });
    assert_ne!(pos_of(&view, cx, bolt).project, pos_of(&view, cx, atlas).project, "unmoved");

    let row = bounds(cx, selector("nav-tile", bolt.item)).center();
    let to = bounds(cx, atlas_row).center();
    carry(cx, row, to, |cx| {
        let wash = bounds(cx, "nav-group-drop");
        assert_eq!(wash, bounds(cx, atlas_row), "atlas's row is washed");
    });
    let (moved, home) = (pos_of(&view, cx, bolt), pos_of(&view, cx, atlas));
    assert_eq!(moved.project, home.project, "in atlas's project now");
    assert_ne!(moved.tab, home.tab, "a tab of its own");
    assert_eq!(focused(&view, cx), Some(bolt), "and focused");
    assert!(cx.debug_bounds("nav-group-drop").is_none(), "the wash goes with the drop");
}

/// A thread with no tile here is warmed by its row's press: the worker is asked to follow it
/// before the click. Its row carried onto a pane's trailing edge opens its tile in a pane of
/// its own there, once the worker's list has it.
#[gpui::test]
fn a_threads_row_warms_on_its_press_and_drops_on_a_panes_edge(cx: &mut TestAppContext) {
    use slopty_proto::thread::wire::{TableFrame, ThreadRequest};
    use slopty_proto::thread::{Cursor, ThreadId};

    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let (_, shell) = shell_in(&view, cx, &studio, 1, "/w/atlas", true);
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = None;
    let mut row = state.row(WallMs::ZERO);
    row.id = ThreadId::new();
    row.cwd = Some("/w/atlas/src".to_owned());
    row.repo = Some("/w/atlas".to_owned());
    // Waiting on the person, so it is listed under its project rather than folded away.
    row.requests = vec![slopty_proto::thread::wire::RequestCard {
        buttons: Vec::new(),
        id: slopty_proto::thread::AskId("ask-1".to_owned()),
        item: None,
        kind: slopty_proto::thread::Request::APPROVAL.to_owned(),
        title: "Run `cargo test`".to_owned(),
        options: Vec::new(),
        opened_ms: WallMs::ZERO,
    }];
    let thread = row.id;
    view.update_in(cx, |v, _w, cx| {
        v.threads_linked(key, cx);
        let table = TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 1 }, rows: vec![row] };
        v.thread_table(key, &table, cx);
    });
    cx.run_until_parked();
    studio.drain();

    let from = bounds(cx, leak(format!("nav-thread-{thread}"))).center();
    let pane = view.read_with(cx, |v, _| v.tile_bounds(shell)).expect("the shell's pane");
    carry(cx, from, near(pane, true), |cx| {
        assert_eq!(view.read_with(cx, |v, _| v.warmed()), Some(thread), "warm from the press");
        assert!(cx.debug_bounds("drop-wash").is_some(), "the landing is drawn");
    });
    let sent = studio.drain();
    let followed = sent.iter().any(
        |m| matches!(m, ClientMsg::Thread(ThreadRequest::Follow { thread: t, .. }) if *t == thread),
    );
    assert!(followed, "the press asked the worker to follow it: {sent:?}");
    let item = sent
        .into_iter()
        .find_map(|m| match m {
            ClientMsg::Items(ItemOp::Add(item)) if item.kind == ItemKind::Thread { thread } => {
                Some(item)
            }
            _ => None,
        })
        .expect("its tile is asked for");
    let tile = TileRef { worker: key, item: item.id };
    view.update_in(cx, |v, _w, cx| {
        let op = ItemOp::Add(item);
        v.apply_sync(key, ItemSync::Delta { version: 2, by: studio.me, op }, cx);
    });
    cx.run_until_parked();
    let (a, b) = (pos_of(&view, cx, shell), pos_of(&view, cx, tile));
    assert_eq!(a.tab, b.tab, "in the tab on show");
    assert_ne!(a.pane, b.pane, "in a pane of its own");
    let at = |tile: TileRef| view.read_with(cx, |v, _| v.tile_bounds(tile)).expect("drawn");
    assert!(at(tile).left() >= at(shell).right() - px(1.0), "on the trailing edge");
}
