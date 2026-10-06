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
use super::view::{ThreadView, ThreadViewEvent};

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
    view_in(cx, hub, thread, Theme::default())
}

/// [`view`] in `theme`.
fn view_in<'a>(
    cx: &'a mut TestAppContext,
    hub: &Entity<ThreadHub>,
    thread: ThreadId,
    theme: Theme,
) -> (Entity<ThreadView>, &'a mut VisualTestContext) {
    let hub = hub.clone();
    let (view, cx) = cx.add_window_view(|window, cx| {
        let view = ThreadView::new(hub, thread, theme, window, cx);
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

/// What the view asked of the workspace from now on.
fn asked(
    cx: &mut VisualTestContext,
    view: &Entity<ThreadView>,
) -> Rc<RefCell<Vec<ThreadViewEvent>>> {
    let asked: Rc<RefCell<Vec<ThreadViewEvent>>> = Rc::default();
    let into = Rc::clone(&asked);
    cx.update(|_window, cx| {
        cx.subscribe(view, move |_view, event: &ThreadViewEvent, _cx| {
            into.borrow_mut().push(event.clone());
        })
        .detach();
    });
    asked
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

/// An empty thread is its composer: centred under one question, what its agent should do in
/// its folder, with where it works in the composer's foot and no notice in the rows' place. A
/// thread whose only row is a request asks nothing, since the request's card says it all; and
/// once the thread has rows the composer docks at the foot with the keyboard still in it (at
/// once, under Reduce Motion).
#[gpui::test]
fn an_empty_thread_is_its_composer_under_a_question(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let state = fixtures::empty();
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    cx.update(|window, _cx| window.set_a11y_active(true));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state.clone(), 0), cx));
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.rows().is_empty()));
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    assert!(
        tree.iter().any(|n| n.is("Heading", Some("What should Claude Code do in w?"))),
        "the question, by the folder's name"
    );
    assert!(cx.debug_bounds("thread-empty").is_none(), "no notice in the rows' place");
    assert!(cx.debug_bounds("thread-place").is_some(), "where, in the composer's foot");
    let middle = |cx: &mut VisualTestContext| {
        let composer = cx.debug_bounds("thread-composer").expect("the composer");
        composer.bottom() < px(450.0)
    };
    assert!(middle(cx), "the composer stands in the middle, not at the foot");

    let mut working = state.clone();
    working.status.phase = slopty_proto::thread::Phase::Working;
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(working, 1), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-hero").is_none(), "an agent at work asks nothing");

    let mut asking = state.clone();
    asking.requests = vec![approval("a")];
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(asking, 2), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("request-a").is_some(), "the request is on show");
    assert!(cx.debug_bounds("thread-hero").is_none(), "and nothing asks over it");

    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 3), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-hero").is_some(), "asked again");
    view.update(cx, |v, cx| v.set_briefed(true, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-hero").is_none(), "a thread a project briefed asks nothing");
    view.update(cx, |v, cx| v.set_briefed(false, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-hero").is_some(), "unbriefed, it asks once more");
    let field = view.read_with(cx, gpui::Focusable::focus_handle);
    let typing = |cx: &mut VisualTestContext| cx.update(|window, _| field.is_focused(window));
    assert!(typing(cx), "the field has the keyboard");
    let full = fixtures::thread("edit");
    let mut moved = full;
    moved.meta.id = thread;
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(moved, 4), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-hero").is_none(), "a thread with rows asks nothing");
    assert!(cx.debug_bounds("thread-place").is_none(), "its header says where");
    assert!(!middle(cx), "the composer docks at the foot");
    assert!(typing(cx), "with the keyboard still in it");
}

/// With motion on, the composer does not jump to the foot after the first message: it sets
/// off from the middle and the dock's move starts, which ends with the composer at the foot.
/// The move runs on wall time, which a busy machine may have spent before any frame is read, so
/// the test reads where the composer stands before, that the move started, and, with motion
/// then reduced, where it ends.
#[gpui::test]
fn the_composer_moves_to_the_foot_after_the_first_message(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let state = fixtures::empty();
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    cx.update(|_w, cx| cx.set_reduce_motion(false));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();
    let bottom = |cx: &mut VisualTestContext| {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        cx.debug_bounds("thread-composer").expect("the composer").bottom()
    };
    let before = bottom(cx);
    assert!(before < px(450.0), "it stands in the middle: {before:?}");
    assert!(cx.debug_bounds("thread-dock").is_none(), "nothing moves before the message");
    let mut moved = fixtures::thread("edit");
    moved.meta.id = thread;
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(moved, 1), cx));
    let _setting_off = bottom(cx);
    assert!(cx.debug_bounds("thread-dock").is_some(), "the move to the foot sets off");
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    let landed = bottom(cx);
    assert!(landed > px(560.0), "and ends at the foot: {landed:?}");
}

/// Under Reduce Motion the first message puts the composer at the foot at once: no move sets
/// off.
#[gpui::test]
fn under_reduce_motion_the_composer_docks_at_once(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let state = fixtures::empty();
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();
    let mut moved = fixtures::thread("edit");
    moved.meta.id = thread;
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(moved, 1), cx));
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let at_once = cx.debug_bounds("thread-composer").expect("the composer").bottom();
    assert!(at_once > px(560.0), "at the foot at once: {at_once:?}");
    assert!(cx.debug_bounds("thread-dock").is_none(), "no move under Reduce Motion");
}

/// On touch a request's answers are a finger's target, 44 points tall, as the theme's touch
/// density asks; with a pointer they are a control's height.
#[gpui::test]
fn a_requests_answers_are_a_fingers_target_on_touch(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.requests = vec![approval("a")];
    hub.update(cx, ThreadHub::connected);
    let theme = Theme { density: slopty_theme::Density::TOUCH, ..Theme::default() };
    let (_view, cx) = view_in(cx, &hub, thread, theme);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();
    for answer in ["answer-a-allow", "answer-a-deny"] {
        let bounds = cx.debug_bounds(answer).expect("the answer");
        assert!(bounds.size.height >= px(44.0), "{answer}: {bounds:?}");
    }
}

/// ⌘↵ and ⌘⌫ answer a request only once the person has put the keyboard on it: in the
/// composer, empty or not, ⌘↵ is never an answer, so a request that arrives while the person
/// types changes nothing a habitual key does. A press on the card gives it the keyboard; then
/// ⌘↵ allows it, and on the next, ⌘⌫ denies it.
#[gpui::test]
fn a_request_is_answered_by_its_key_only_where_it_has_the_keyboard(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.requests = vec![approval("a"), approval("b")];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();
    let answers = |sent: &Sent| {
        intents(sent).into_iter().filter(|i| matches!(i, Intent::Answer { .. })).collect::<Vec<_>>()
    };

    cx.simulate_keystrokes("cmd-enter");
    cx.simulate_keystrokes("cmd-backspace");
    assert!(answers(&sent).is_empty(), "the composer has the keyboard: no answer");

    let press_card = |cx: &mut VisualTestContext| {
        let card = cx.debug_bounds("thread-decision").expect("the request's card");
        let corner = card.origin + gpui::point(px(4.0), px(4.0));
        cx.simulate_click(corner, Modifiers::none());
    };
    press_card(cx);
    cx.simulate_keystrokes("cmd-enter");
    assert_eq!(
        answers(&sent),
        [Intent::Answer { ask: AskId("a".to_owned()), choice: "allow".to_owned(), message: None }]
    );
    cx.run_until_parked();
    press_card(cx);
    cx.simulate_keystrokes("cmd-backspace");
    assert_eq!(
        answers(&sent).last(),
        Some(&Intent::Answer {
            ask: AskId("b".to_owned()),
            choice: "deny".to_owned(),
            message: None
        })
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
        [Intent::Send { text, delivery: Delivery::Steer, .. }] if text == "Run the tests" => {
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
        Intent::Send {
            text: "And then the docs".to_owned(),
            delivery: Delivery::Queue,
            attachments: vec![]
        }
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
        [Intent::Send {
            text: "Run the tests".to_owned(),
            delivery: Delivery::Queue,
            attachments: vec![]
        }]
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

/// The thread's head is parted from its turns by a soft edge, not a rule: no hairline under
/// the header, and the turns fade at the top only while some lie above it, as macOS 26's
/// scroll edge does. A long thread opens on its newest turn, so its top fades.
#[gpui::test]
fn the_turns_fade_under_the_header_with_no_rule(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let state = fixtures::long(6, 12);
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    // The fade reads the list's extent as it lays out, so it lands a frame later.
    cx.update(gpui::Window::simulate_next_frame);
    cx.run_until_parked();
    let header = cx.debug_bounds("thread-header").expect("the header");
    let rows = cx.debug_bounds("thread-rows").expect("the turns");
    let ruled = cx.update(|window, _| {
        window.painted_quads().into_iter().any(|q| {
            q.border_widths.bottom.0 > 0.0
                && (q.bounds.bottom().0 / window.scale_factor() - f32::from(header.bottom())).abs()
                    < 1.0
        })
    });
    assert!(!ruled, "no hairline under the header");
    let faded = cx.update(|window, _| crate::retained::faded_edges(window, rows));
    assert!(faded.top, "the turns above the newest fade under the header");
    assert!(!faded.bottom, "nothing lies below the newest");
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

mod aside;
mod carry;
mod commit;
mod composing;
mod doors;
mod face;
mod find;
mod questions;
mod steps;

/// What the soft top edge costs a thread's frame: a list of 400 rows of a turn's words, scrolled
/// to its end as a thread opens, drawn after a notify with and without the edge fade over it
/// (`gpui::edge_fade` hidden by the list). Run by hand, in release:
/// `cargo test -p slopty-ui --release --lib soft_edge_cost -- --ignored --nocapture`.
#[gpui::test]
#[ignore = "measurement, run by hand"]
fn soft_edge_cost(cx: &mut TestAppContext) {
    use gpui::{
        Context, IntoElement, ListAlignment, ListState, ParentElement as _, Render, Styled as _,
        Window, div, list, px,
    };

    struct Turns {
        list: ListState,
        fade: bool,
    }
    impl Render for Turns {
        fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let rows = list(self.list.clone(), |ix, _w, _cx| {
                div()
                    .px(px(16.0))
                    .py(px(4.0))
                    .child(format!(
                        "Row {ix}: the agent read the parser, split the lexer and ran the tests \
                         again, and they passed."
                    ))
                    .into_any_element()
            })
            .size_full();
            let body = if self.fade {
                let edges = gpui::Edges { top: px(16.0), ..gpui::Edges::default() };
                gpui::edge_fade(rows, gpui::EdgeFade::new(edges))
                    .hidden_by_list(&self.list)
                    .into_any_element()
            } else {
                rows.into_any_element()
            };
            div().size_full().child(body)
        }
    }

    const FRAMES: usize = 400;
    const WARM: usize = 40;
    let mut line = Vec::new();
    for fade in [false, true, false, true] {
        let (view, cx) = cx.add_window_view(|_, _| {
            let list = ListState::new(400, ListAlignment::Bottom, px(200.0));
            Turns { list, fade }
        });
        cx.simulate_resize(size(px(800.0), px(600.0)));
        cx.run_until_parked();
        let mut samples = Vec::new();
        for n in 0..WARM + FRAMES {
            let started = std::time::Instant::now();
            view.update(cx, |_, cx| cx.notify());
            cx.run_until_parked();
            if n >= WARM {
                samples.push(started.elapsed());
            }
        }
        samples.sort_unstable();
        let at = |q: usize| {
            let ix = samples.len().saturating_sub(1).saturating_mul(q) / 100;
            samples.get(ix).copied().unwrap_or_default()
        };
        line.push(format!(
            "{}: {:?} / {:?} / {:?}",
            if fade { "faded" } else { "plain" },
            at(50),
            at(95),
            at(99)
        ));
    }
    println!(
        "MEASURE 400 rows at their end, a notify and its draw (p50 / p95 / p99): {}",
        line.join(" · ")
    );
}
