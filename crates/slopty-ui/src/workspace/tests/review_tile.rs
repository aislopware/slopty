//! A thread's review opens as a tile of its own on the agent's worker, takes the keyboard, and
//! lets the thread go once its tile is closed.

use slopty_core::WallMs;
use slopty_proto::thread::Cursor;
use slopty_proto::thread::wire::TableFrame;

use super::*;
use crate::conversation::thread::ThreadViewEvent;

/// The thread view's "Review" adds a review item on the agent's worker; once the registry has
/// it, its tile draws the review and holds the keyboard, and a second ask goes to that tile
/// rather than adding another. The tile removed, the review view is let go.
#[gpui::test]
fn a_thread_s_review_opens_as_a_tile_of_its_own_and_goes_with_it(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let agent = opens(&view, cx, &studio, session, studio.me, 1);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
        v.threads_linked(key, cx);
    });
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(session);
    let thread = state.meta.id;
    let table = TableFrame::Snapshot {
        cursor: Cursor { epoch: 1, seq: 1 },
        rows: vec![state.row(WallMs::ZERO)],
    };
    view.update_in(cx, |v, _w, cx| {
        v.thread_table(key, &table, cx);
        v.focus_tile(agent, cx);
    });
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    studio.drain();

    let ask = |cx: &mut VisualTestContext| {
        let face = view.read_with(cx, |v, _| v.thread_face(session).cloned()).expect("the face");
        face.update(cx, |_, cx| cx.emit(ThreadViewEvent::Review { thread }));
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    };
    ask(cx);
    let added: Vec<Item> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Items(ItemOp::Add(item)) => Some(item),
            _ => None,
        })
        .collect();
    let [review] = added.as_slice() else { panic!("one review item: {added:?}") };
    assert_eq!(review.kind, ItemKind::Review { thread });

    let by = studio.me;
    let op = ItemOp::Add(review.clone());
    view.update_in(cx, |v, _w, cx| v.apply_sync(key, ItemSync::Delta { version: 2, by, op }, cx));
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let tile = TileRef { worker: key, item: review.id };
    assert_eq!(focused(&view, cx), Some(tile), "the review comes to the front");
    let keyboard = view.update_in(cx, |v, window, cx| {
        v.review_of(thread)
            .is_some_and(|r| Focusable::focus_handle(r.read(cx), cx).is_focused(window))
    });
    assert!(keyboard, "the review holds the keyboard");
    let title = view.read_with(cx, |v, _| v.tile_title(review));
    assert_eq!(title, tile::REVIEW);

    view.update_in(cx, |v, _w, cx| v.focus_tile(agent, cx));
    ask(cx);
    let again = studio.drain();
    assert!(!again.iter().any(|m| matches!(m, ClientMsg::Items(ItemOp::Add(_)))), "{again:?}");
    assert_eq!(focused(&view, cx), Some(tile), "a second ask goes to the open tile");

    let op = ItemOp::Remove(review.id);
    view.update_in(cx, |v, _w, cx| v.apply_sync(key, ItemSync::Delta { version: 3, by, op }, cx));
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.review_of(thread).is_none()), "let go with its tile");
}
