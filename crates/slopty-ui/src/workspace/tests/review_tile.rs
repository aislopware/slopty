//! A thread's review opens as a tile of its own on the agent's worker, takes the keyboard,
//! hands its comments to the agent's draft, and lets the thread go once its tile is closed.

use slopty_core::WallMs;
use slopty_proto::thread::Cursor;
use slopty_proto::thread::wire::TableFrame;

use super::*;
use crate::conversation::thread::ThreadViewEvent;

/// The thread view's "Review" adds a review item on the agent's worker; once the registry has
/// it, its tile draws the review and holds the keyboard, and a second ask goes to that tile
/// rather than adding another. Comments added to the message land in the agent's draft, with
/// the agent's tile in front. The tile removed, the review view is let go.
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

    let words = "In `src/lib.rs` line 11:\n```diff\n+    new();\n```\nWhy new?";
    let shown = view.read_with(cx, |v, _| v.review_of(thread).cloned()).expect("the review");
    shown.update(cx, |_, cx| {
        cx.emit(crate::review::ReviewEvent::AddToMessage { thread, text: words.to_owned() });
    });
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let face = view.read_with(cx, |v, _| v.thread_face(session).cloned()).expect("the face");
    assert_eq!(
        face.read_with(cx, crate::conversation::thread::ThreadView::draft),
        words,
        "in the agent's draft"
    );
    assert_eq!(focused(&view, cx), Some(agent), "the agent's tile comes to the front");

    let op = ItemOp::Remove(review.id);
    view.update_in(cx, |v, _w, cx| v.apply_sync(key, ItemSync::Delta { version: 3, by, op }, cx));
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.review_of(thread).is_none()), "let go with its tile");
}

/// A thread is named the same everywhere: where its agent runs in a tile here, by that tile's
/// title, in the navigator, a thread tile's header and the author of a line in a review, not
/// by the title its worker's table gives it.
#[gpui::test]
fn a_thread_goes_by_its_agents_tile_everywhere(cx: &mut TestAppContext) {
    use slopty_proto::thread::TurnId;
    use slopty_proto::thread::wire::{AuthorRun, Authors};

    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let agent = opens(&view, cx, &studio, session, studio.me, 1);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
        v.threads_linked(key, cx);
    });
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = Some(session);
    state.meta.title = "What the table says".to_owned();
    let thread = state.meta.id;
    let table = TableFrame::Snapshot {
        cursor: Cursor { epoch: 1, seq: 1 },
        rows: vec![state.row(WallMs::ZERO)],
    };
    view.update_in(cx, |v, _w, cx| v.thread_table(key, &table, cx));
    let by = studio.me;
    let named = ItemOp::Rename { id: agent.item, name: Some("Login fix".to_owned()) };
    view.update_in(cx, |v, _w, cx| {
        v.apply_sync(key, ItemSync::Delta { version: 2, by, op: named }, cx);
    });
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.thread_title(thread)), "Login fix");

    let review = view.update_in(cx, |v, window, cx| v.open_review(key, thread, window, cx));
    let authors = Authors {
        thread: Some(thread),
        path: "src/lib.rs".to_owned(),
        modified_ms: None,
        blob: Some("b".to_owned()),
        runs: vec![AuthorRun {
            start: 1,
            lines: 1,
            thread,
            turn: Some(TurnId(1)),
            commit: None,
            at_ms: WallMs::ZERO,
        }],
        absent: None,
    };
    review.update(cx, |r, cx| r.take_authors(0, authors, cx));
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let named = review.read_with(cx, |r, _| r.writer(thread).map(|w| w.title.clone()));
    assert_eq!(named.as_deref(), Some("Login fix"));
}
