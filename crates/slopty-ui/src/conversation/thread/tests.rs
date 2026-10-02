//! The thread view in a headless window over a hub: what shows in the frame the person acts,
//! and what a cached thread draws before the worker says anything.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use slopty_client::threads::{Cache, Cached};
use slopty_proto::ClientMsg;
use slopty_proto::thread::wire::{Intent, ThreadFrame, ThreadRequest};
use slopty_proto::thread::{
    AskId, Cap, Choice, Cursor, Delivery, Effect, Request, RequestState, ThreadId, ThreadState,
};
use slopty_theme::Theme;

use super::fixtures;
use super::hub::{HubEvent, ThreadHub};
use super::rows::Row;
use super::view::ThreadView;

type Sent = Rc<RefCell<Vec<ClientMsg>>>;

/// A hub for a worker called studio, linked, and what it asked to send.
fn hub(cx: &TestAppContext, cache: Option<Cache>) -> (Entity<ThreadHub>, Sent) {
    let sent: Sent = Rc::default();
    let hub = cx.update(|cx| {
        gpui_kit::init(cx);
        cx.bind_keys(crate::workspace::key_bindings());
        let hub = cx.new(|_| ThreadHub::new("studio".to_owned(), cache));
        let into = Rc::clone(&sent);
        cx.subscribe(&hub, move |_hub, event: &HubEvent, _cx| {
            if let HubEvent::Send(msgs) = event {
                into.borrow_mut().extend(msgs.iter().cloned());
            }
        })
        .detach();
        hub
    });
    (hub, sent)
}

/// A view of `thread`, 800 × 600 points, its composer focused.
fn view<'a>(
    cx: &'a mut TestAppContext,
    hub: &Entity<ThreadHub>,
    thread: ThreadId,
) -> (Entity<ThreadView>, &'a mut VisualTestContext) {
    let hub = hub.clone();
    let (view, cx) = cx.add_window_view(|window, cx| {
        let view = ThreadView::new(hub, thread, Theme::default(), window, cx);
        view.focus(window, cx);
        view
    });
    cx.simulate_resize(size(px(800.0), px(600.0)));
    cx.run_until_parked();
    (view, cx)
}

fn snapshot(state: ThreadState, seq: u64) -> ThreadFrame {
    ThreadFrame::Snapshot { cursor: Cursor { epoch: 1, seq }, state: Box::new(state) }
}

fn approval(id: &str) -> Request {
    let choice = |id: &str, label: &str, effect| Choice {
        id: id.to_owned(),
        label: label.to_owned(),
        effect,
        scope: None,
        stops: false,
    };
    Request {
        id: AskId(id.to_owned()),
        item: None,
        kind: Request::APPROVAL.to_owned(),
        title: format!("Run {id}"),
        text: None,
        options: vec![
            choice("allow", "Allow", Effect::Allow),
            choice("deny", "Deny", Effect::Deny),
        ],
        questions: Vec::new(),
        proposed: None,
        schema_json: None,
        url: None,
        state: RequestState::Open,
        opened_ms: slopty_core::WallMs::ZERO,
        until_ms: None,
    }
}

fn intents(sent: &Sent) -> Vec<Intent> {
    sent.borrow()
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Intent { intent, .. }) => Some(intent.clone()),
            _ => None,
        })
        .collect()
}

/// A press on an answer flips the card in the frame it lands, before the worker has said a
/// word: the card goes, the answer shows in its place, the next request is "1 of 1", and the
/// answer is on its way under its id.
#[gpui::test]
fn an_answer_flips_its_card_in_the_frame_it_is_pressed(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.requests = vec![approval("a"), approval("b")];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 3), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("request-a").is_some(), "the first request is on show");

    let allow = cx.debug_bounds("answer-a-allow").expect("its answers").center();
    cx.simulate_click(allow, Modifiers::none());
    assert!(cx.debug_bounds("request-a").is_none(), "the card flipped in that frame");
    assert!(cx.debug_bounds("answered-a").is_some(), "the answer shows in its place");
    assert!(cx.debug_bounds("request-b").is_some(), "the next request is up");
    assert_eq!(
        intents(&sent),
        [Intent::Answer { ask: AskId("a".to_owned()), choice: "allow".to_owned(), message: None }]
    );
}

/// ↵ sends now and the message shows as a bubble on its way; ⌘↵ queues it, and it waits in
/// the activity bar, not in the thread.
#[gpui::test]
fn return_sends_now_and_command_return_queues(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let state = fixtures::empty();
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();

    cx.simulate_input("Run the tests");
    cx.simulate_keystrokes("enter");
    let id = match intents(&sent).as_slice() {
        [Intent::Send { text, delivery: Delivery::Steer }] if text == "Run the tests" => {
            hub.read_with(cx, |h, _| h.threads().outbox().all().first().map(|s| s.id)).unwrap()
        }
        other => panic!("one message sent now: {other:?}"),
    };
    assert_eq!(view.read_with(cx, ThreadView::draft), "", "the composer emptied");
    assert!(view.read_with(cx, |v, _| v.rows().contains(&Row::Sending { intent: id })));
    assert!(cx.debug_bounds(format!("sending-{id}").leak()).is_some(), "drawn in that frame");

    cx.simulate_input("And then the docs");
    cx.simulate_keystrokes("cmd-enter");
    let queued = hub
        .read_with(cx, |h, _| h.threads().outbox().all().get(1).map(|s| (s.id, s.intent.clone())))
        .unwrap();
    assert_eq!(
        queued.1,
        Intent::Send { text: "And then the docs".to_owned(), delivery: Delivery::Queue }
    );
    assert!(cx.debug_bounds(format!("queued-{}", queued.0).leak()).is_some(), "waits in the bar");
    assert!(!view.read_with(cx, |v, _| v.rows().contains(&Row::Sending { intent: queued.0 })));
}

/// An agent that takes no message mid-turn but keeps a queue (every ACP agent) has ↵ queue
/// the message, which the worker sends at once while the agent rests: a steer to it would be
/// refused. Its capabilities say so, not its name.
#[gpui::test]
fn return_queues_for_an_agent_that_takes_no_steer(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.meta.agent = slopty_proto::thread::AgentId::named("acp:opencode");
    state.meta.caps = vec![Cap::named(Cap::QUEUE), Cap::named(Cap::INTERRUPT)];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();

    cx.simulate_input("Run the tests");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        intents(&sent),
        [Intent::Send { text: "Run the tests".to_owned(), delivery: Delivery::Queue }]
    );
}

/// A thread the cache kept draws in the view's first frame, before any link: its rows are
/// there the moment the view is, and only then is it followed from the cursor it was kept at.
#[gpui::test]
fn a_cached_thread_draws_in_its_first_frame(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::new(dir.path().join("studio"));
    let state = fixtures::thread("tools");
    let thread = state.meta.id;
    let cursor = Cursor { epoch: 4, seq: 120 };
    cache.keep_thread(thread, &Cached { cursor, state }).unwrap();

    let (hub, sent) = hub(cx, Some(cache));
    let (view, cx) = view(cx, &hub, thread);
    let rows = view.read_with(cx, |v, _| v.rows().len());
    assert!(rows > 0, "rows from the cache");
    assert!(cx.debug_bounds("thread-empty").is_none(), "no empty first frame");
    assert!(cx.debug_bounds("fold-1").is_some(), "the first turn, folded");
    assert!(sent.borrow().is_empty(), "nothing goes while the link is down");

    hub.update(cx, ThreadHub::connected);
    let follow = sent.borrow().iter().find_map(|m| match m {
        ClientMsg::Thread(ThreadRequest::Follow { have, .. }) => Some(*have),
        _ => None,
    });
    assert_eq!(follow, Some(Some(cursor)), "caught up from the cursor it was kept at");
}

/// A word streamed into the answer builds the rows again but measures only the row it grew.
#[gpui::test]
fn a_streamed_word_moves_only_its_own_row(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let state = fixtures::thread("tools");
    let thread = state.meta.id;
    let last = state.items.last().map(|i| i.id.clone()).unwrap();
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 9), cx));
    cx.run_until_parked();
    let before = view.read_with(cx, |v, _| (v.rows().to_vec(), v.keys().to_vec()));
    let grow = slopty_proto::thread::Action::Append {
        item: last,
        part: slopty_proto::thread::PartKey::Body,
        text: " more".to_owned(),
    };
    let frame = ThreadFrame::Actions { epoch: 1, first: 9, next: 10, actions: vec![grow] };
    hub.update(cx, |hub, cx| hub.frame(thread, frame, cx));
    cx.run_until_parked();
    let after = view.read_with(cx, |v, _| (v.rows().to_vec(), v.keys().to_vec()));
    assert_eq!(after.0, before.0, "the same rows");
    let moved = before.1.iter().zip(&after.1).filter(|(a, b)| a != b).count();
    assert_eq!(moved, 1, "only the row that grew is measured again");
}

/// What the thread path costs on the UI thread for a snapshot's worth of a long thread
/// (`SNAPSHOT_TURNS` turns of 40 calls): the cache read that draws the first frame, a streamed
/// word applied and the rows built again, and the bar.
#[test]
#[ignore = "timing: cargo nextest run -p slopty-ui --release --run-ignored only timing_of_the_thread_path --no-capture"]
fn timing_of_the_thread_path() {
    use std::time::{Duration, Instant};

    use slopty_client::threads::{SNAPSHOT_TURNS, Threads};
    use slopty_proto::thread::{Action, PartKey, TurnId};

    use super::activity::Activity;
    use super::rows::{self, Input};

    fn median(mut f: impl FnMut()) -> Duration {
        let mut took: Vec<Duration> = std::iter::repeat_with(|| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .take(21)
        .collect();
        took.sort();
        took.get(10).copied().unwrap_or_default()
    }
    let state = fixtures::long(SNAPSHOT_TURNS, 40);
    let thread = state.meta.id;
    let items = state.items.len();
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::new(dir.path().to_path_buf());
    let cursor = Cursor { epoch: 1, seq: 1 };
    cache.keep_thread(thread, &Cached { cursor, state: state.clone() }).unwrap();
    let bytes = std::fs::metadata(dir.path().join(format!("{thread}.thread"))).unwrap().len();
    let read = median(|| drop(std::hint::black_box(cache.thread(thread))));
    let keep =
        median(|| cache.keep_thread(thread, &Cached { cursor, state: state.clone() }).unwrap());

    let mut threads = Threads::default();
    let _sent = threads.connected();
    let _follow = threads.open_thread(thread, Some(Cached { cursor, state: state.clone() }));
    let last = state.items.last().map(|i| i.id.clone()).unwrap();
    let mut seq = 1;
    let word = median(|| {
        let grow =
            Action::Append { item: last.clone(), part: PartKey::Body, text: " word".to_owned() };
        let frame =
            ThreadFrame::Actions { epoch: 1, first: seq, next: seq + 1, actions: vec![grow] };
        seq += 1;
        drop(std::hint::black_box(threads.frame(thread, frame)));
    });
    let mirrored = threads.mirror(thread).and_then(slopty_client::threads::Mirror::state).unwrap();
    let open = std::collections::HashSet::new();
    let groups = std::collections::HashSet::new();
    let built = median(|| {
        drop(std::hint::black_box(rows::build(Input {
            state: mirrored,
            unshown: &[],
            open: &open,
            groups: &groups,
        })));
    });
    let mut all: std::collections::HashSet<TurnId> = mirrored.turns.iter().map(|t| t.id).collect();
    all.insert(TurnId(0));
    let built_open = median(|| {
        drop(std::hint::black_box(rows::build(Input {
            state: mirrored,
            unshown: &[],
            open: &all,
            groups: &groups,
        })));
    });
    let bar = median(|| drop(std::hint::black_box(Activity::of(&threads, thread, mirrored))));
    println!("{items} items, {bytes} bytes cached");
    println!("cache read + decode: {read:?}; encode + write: {keep:?}");
    println!("a streamed word applied: {word:?}");
    println!("rows built, settled turns folded: {built:?}; every turn open: {built_open:?}");
    println!("activity bar: {bar:?}");
}

/// What the thread view's frames cost in a headless window, for a snapshot's worth of a long
/// thread (`SNAPSHOT_TURNS` turns of 40 calls): the first frame drawn from the cache, a send
/// drawn as its bubble in the frame of its ↵, and every step opened by ⌃O. Layout and paint
/// only: the headless window rasterizes nothing.
#[gpui::test]
#[ignore = "timing: cargo nextest run -p slopty-ui --release --run-ignored only timing_of_the_thread_s_frames --no-capture"]
fn timing_of_the_thread_s_frames(cx: &mut TestAppContext) {
    use std::time::{Duration, Instant};

    use slopty_client::threads::SNAPSHOT_TURNS;

    fn median(mut took: Vec<Duration>) -> Duration {
        took.sort();
        took.get(took.len() / 2).copied().unwrap_or_default()
    }
    let dir = tempfile::tempdir().unwrap();
    let state = fixtures::long(SNAPSHOT_TURNS, 40);
    let thread = state.meta.id;
    let cursor = Cursor { epoch: 1, seq: 1 };
    // Each view over a cache of its own, so every one reads it; made in a window already open.
    let hubs: Vec<Entity<ThreadHub>> = (0..11)
        .map(|n| {
            let cache = Cache::new(dir.path().join(format!("studio-{n}")));
            cache.keep_thread(thread, &Cached { cursor, state: state.clone() }).unwrap();
            hub(cx, Some(cache)).0
        })
        .collect();
    let (mut made, mut first) = (Vec::new(), Vec::new());
    {
        let window = cx.add_empty_window();
        for hub in hubs {
            let begin = Instant::now();
            let view = window.update(|window, cx| {
                cx.new(|cx| ThreadView::new(hub, thread, Theme::default(), window, cx))
            });
            made.push(begin.elapsed());
            let space = size(px(800.0), px(600.0)).map(gpui::AvailableSpace::Definite);
            window.draw(gpui::point(px(0.0), px(0.0)), space, |_, _| {
                gpui::IntoElement::into_any_element(view.clone())
            });
            first.push(begin.elapsed());
            assert!(view.read_with(window, |v, _| !v.rows().is_empty()), "drawn from the cache");
        }
    }

    let cache = Cache::new(dir.path().join("studio"));
    cache.keep_thread(thread, &Cached { cursor, state }).unwrap();
    let (hub, _sent) = hub(cx, Some(cache));
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    let mut sends = Vec::new();
    for n in 0..21 {
        cx.simulate_input(&format!("Message {n}"));
        let begin = Instant::now();
        cx.simulate_keystrokes("enter");
        sends.push(begin.elapsed());
        let id = hub.read_with(cx, |h, _| h.threads().outbox().all().last().map(|s| s.id)).unwrap();
        assert!(view.read_with(cx, |v, _| v.rows().contains(&Row::Sending { intent: id })));
        assert!(cx.debug_bounds(format!("sending-{id}").leak()).is_some(), "in that frame");
    }
    let mut every = Vec::new();
    for _ in 0..21 {
        let begin = Instant::now();
        cx.simulate_keystrokes("ctrl-o");
        every.push(begin.elapsed());
    }
    println!("view made, its cache read: {:?}", median(made));
    println!("first frame from the cache, view made and drawn: {:?}", median(first));
    println!("a send drawn in the frame of its return: {:?}", median(sends));
    println!("every step opened or folded by ctrl-o: {:?}", median(every));
}

mod composing;
mod face;
mod questions;
mod steps;
