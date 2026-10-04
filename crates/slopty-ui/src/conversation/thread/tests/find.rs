//! ⌘F in a thread: the matches newest first, a folded turn opened to show one, the older
//! turns asked of the worker, and the thread paged back to a match it found there.

use gpui::TestAppContext;
use slopty_core::WallMs;
use slopty_proto::ClientMsg;
use slopty_proto::thread::wire::{ItemHit, ThreadHit, ThreadHits, ThreadRequest};
use slopty_proto::thread::{
    Changed, Clipped, Item, ItemBody, ItemId, ThreadState, ToolCall, ToolState, Turn, TurnId,
    TurnState, Usage, UserMessage, kind,
};

use super::{Sent, hub, snapshot, view};
use crate::conversation::thread::fixtures;
use crate::conversation::thread::hub::ThreadHub;
use crate::conversation::thread::rows::Row;
use crate::conversation::thread::view::ThreadView;

fn turn(id: u32) -> Turn {
    Turn {
        id: TurnId(id),
        input: None,
        state: TurnState::Complete,
        started_ms: WallMs::from_millis(1_000),
        ended_ms: Some(WallMs::from_millis(5_000)),
        usage: Usage::default(),
        models: Vec::new(),
        changed: Changed::default(),
        before: None,
        after: None,
    }
}

fn item(id: &str, turn: u32, body: ItemBody) -> Item {
    Item { id: ItemId(id.to_owned()), turn: TurnId(turn), at_ms: WallMs::ZERO, body }
}

fn user(id: &str, turn: u32, words: &str) -> Item {
    let message = UserMessage {
        text: Clipped::whole(words),
        images: Vec::new(),
        command: None,
        intent: None,
    };
    item(id, turn, ItemBody::User(message))
}

fn read(id: &str, turn: u32, title: &str) -> Item {
    item(
        id,
        turn,
        ItemBody::Tool(Box::new(ToolCall {
            name: "Read".to_owned(),
            kind: kind::READ.to_owned(),
            title: title.to_owned(),
            input: Clipped::default(),
            state: ToolState::Completed,
            output: None,
            images: Vec::new(),
            detail: None,
            child: None,
            ended_ms: None,
        })),
    )
}

fn answer(id: &str, turn: u32, words: &str) -> Item {
    item(id, turn, ItemBody::Text(Clipped::whole(words)))
}

/// Two settled turns, each a message, a read folded away and an answer.
fn two_turns() -> ThreadState {
    let mut state = fixtures::empty();
    state.turns = vec![turn(1), turn(2)];
    state.items = vec![
        user("u1", 1, "Look at the parser"),
        read("r1", 1, "Read parser.rs"),
        answer("a1", 1, "It splits on spaces."),
        user("u2", 2, "Now the lexer"),
        read("r2", 2, "Read parser tests"),
        answer("a2", 2, "Done."),
    ];
    state
}

fn found(view: &gpui::Entity<ThreadView>, cx: &gpui::VisualTestContext) -> (usize, usize) {
    view.read_with(cx, |v, _| v.found()).expect("the bar is open")
}

fn searches(sent: &Sent) -> Vec<String> {
    sent.borrow()
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Search { query, .. }) => Some(query.clone()),
            _ => None,
        })
        .collect()
}

fn pages(sent: &Sent) -> Vec<TurnId> {
    sent.borrow()
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Page { before, .. }) => Some(*before),
            _ => None,
        })
        .collect()
}

/// ⌘F opens the bar on the thread's newest match; one in a folded turn opens the fold and
/// shows the step, and ↵ walks back through the matches, round. With every turn held, the
/// worker is not asked. Esc closes the bar.
#[gpui::test]
fn a_match_opens_the_fold_over_it_and_return_walks_back(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let state = two_turns();
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    let folds_open = |view: &gpui::Entity<ThreadView>, cx: &mut gpui::VisualTestContext| {
        view.read_with(cx, |v, _| {
            v.rows().iter().filter(|r| matches!(r, Row::Fold { open: true, .. })).count()
        })
    };
    assert_eq!(folds_open(&view, cx), 0);

    cx.simulate_keystrokes("cmd-f");
    assert!(cx.debug_bounds("thread-find").is_some(), "the bar");
    cx.simulate_input("parser");
    cx.run_until_parked();
    assert_eq!(found(&view, cx), (0, 3), "the newest of three");
    assert_eq!(folds_open(&view, cx), 1, "turn 2's fold opened for its read");
    assert!(cx.debug_bounds("thread-found").is_some(), "the match is washed");

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(found(&view, cx), (1, 3));
    assert_eq!(folds_open(&view, cx), 2, "then turn 1's");
    cx.simulate_keystrokes("enter enter");
    assert_eq!(found(&view, cx), (0, 3), "round again");
    cx.simulate_keystrokes("shift-enter");
    assert_eq!(found(&view, cx), (2, 3), "and back");
    cx.executor().advance_clock(crate::conversation::thread::view::FIND_ASK_AFTER);
    cx.run_until_parked();
    assert!(searches(&sent).is_empty(), "every turn is held: nothing to ask");

    cx.simulate_keystrokes("escape");
    assert!(cx.debug_bounds("thread-find").is_none(), "closed");
}

/// Where the thread has older turns than it holds, the worker is asked once the words rest,
/// its hits there come after the held ones, and going to one pages the thread back to it,
/// once per page.
#[gpui::test]
fn a_match_in_an_older_turn_pages_back_to_it(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = two_turns();
    state.turns = vec![turn(4), turn(5)];
    for (ix, item) in state.items.iter_mut().enumerate() {
        item.turn = TurnId(if ix < 3 { 4 } else { 5 });
    }
    state.older = true;
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state.clone(), 1), cx));
    cx.run_until_parked();

    cx.simulate_keystrokes("cmd-f");
    cx.simulate_input("parser");
    cx.run_until_parked();
    assert!(searches(&sent).is_empty(), "not while typing");
    cx.executor().advance_clock(crate::conversation::thread::view::FIND_ASK_AFTER);
    cx.run_until_parked();
    assert_eq!(searches(&sent), ["parser"], "asked once the words rest");

    let hit = ItemHit {
        item: ItemId("old".to_owned()),
        turn: TurnId(2),
        said: ItemHit::AGENT.to_owned(),
        text: "the parser, first pass".to_owned(),
        spans: Vec::new(),
        cut_before: false,
        cut_after: false,
        at_ms: WallMs::ZERO,
    };
    let hits = ThreadHits {
        query: "parser".to_owned(),
        threads: vec![ThreadHit { thread, hits: vec![hit], more: 2 }],
        more: 0,
    };
    hub.update(cx, |hub, cx| hub.thread_hits(hits, cx));
    cx.run_until_parked();
    assert_eq!(found(&view, cx), (0, 4), "three held, one older");
    let tally = cx.debug_bounds("thread-find-count");
    assert!(tally.is_some(), "the count, with a + for what the worker left out");

    cx.simulate_keystrokes("shift-enter");
    cx.run_until_parked();
    assert_eq!(found(&view, cx), (3, 4), "the oldest is the worker's");
    assert_eq!(pages(&sent), [TurnId(4)], "the thread pages back from its first turn");
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 2), cx));
    cx.run_until_parked();
    assert_eq!(pages(&sent), [TurnId(4)], "not again before the page comes");
}
