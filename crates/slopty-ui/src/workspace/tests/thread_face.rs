//! An agent tile whose thread the worker's table names draws that thread's view as its face,
//! under the tile's own header, and gives it the keyboard.

use slopty_core::WallMs;
use slopty_proto::thread::Cursor;
use slopty_proto::thread::wire::TableFrame;

use super::*;

/// Known to the table, the agent's thread is drawn in its tile in place of the conversation
/// face, with no header of its own (the tile's says the same), and what is typed goes to its
/// composer.
#[gpui::test]
fn an_agent_tile_whose_thread_is_known_draws_and_focuses_its_thread_view(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens(&view, cx, &studio, session, studio.me, 1);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
        v.threads_linked(key, cx);
    });
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(session);
    let table = TableFrame::Snapshot {
        cursor: Cursor { epoch: 1, seq: 1 },
        rows: vec![state.row(WallMs::ZERO)],
    };
    view.update_in(cx, |v, _w, cx| {
        v.thread_table(key, &table, cx);
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();

    let body = cx.debug_bounds(selector("item", tile.item)).expect("the tile");
    let composer = cx.debug_bounds("thread-composer").expect("the thread view's composer");
    assert!(body.contains(&composer.center()), "in the tile: {body:?} {composer:?}");
    assert!(cx.debug_bounds("composer").is_none(), "not the conversation face");
    assert!(cx.debug_bounds("thread-header").is_none(), "the tile's header says it once");

    cx.simulate_input("ship it");
    cx.run_until_parked();
    let draft = view.read_with(cx, |v, cx| v.thread_face(session).map(|t| t.read(cx).draft(cx)));
    assert_eq!(draft.as_deref(), Some("ship it"), "the keyboard is the thread view's");
}
