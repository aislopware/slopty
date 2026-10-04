//! What the person asks of an agent beyond a message, through the agent's own doors, and what
//! the thread says when the worker turns it down: a refusal in words, a denial with a reason,
//! the agent's own TUI for a request, an exited agent taken up again, and one asleep woken.

use gpui::{Modifiers, TestAppContext};
use slopty_core::SessionId;
use slopty_proto::ClientMsg;
use slopty_proto::thread::wire::{Intent, IntentDone, Outcome, ThreadRequest};
use slopty_proto::thread::{AgentId, AskId, Cap, Drive, Liveness, ThreadState};

use super::{Sent, approval, asked, hub, intents, snapshot, view};
use crate::conversation::thread::fixtures;
use crate::conversation::thread::hub::ThreadHub;
use crate::conversation::thread::view::ThreadViewEvent;

fn click(cx: &mut gpui::VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is on show")).center();
    cx.simulate_click(at, Modifiers::none());
}

/// The starts the hub asked to send, with their ids.
fn starts(sent: &Sent) -> Vec<(slopty_proto::thread::IntentId, Vec<String>)> {
    sent.borrow()
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Start { id, start }) => {
                Some((*id, start.args.clone()))
            }
            _ => None,
        })
        .collect()
}

fn exited(agent: &str, drive: &str) -> ThreadState {
    let mut state = fixtures::empty();
    state.meta.agent = AgentId::named(agent);
    state.meta.drive = Drive::named(drive);
    state.status.liveness = Liveness::Exited { resumable: true };
    state
}

/// An intent the worker turns down, other than a message, says what and why on a line of its
/// own in the activity bar, until the person dismisses it; one the agent cannot take through
/// Slopty says that.
#[gpui::test]
fn a_refused_intent_says_why_until_dismissed(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let state = fixtures::empty();
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();

    let stop = hub.update(cx, |hub, cx| hub.intent(thread, Intent::Interrupt, cx));
    let mode = hub
        .update(cx, |hub, cx| hub.intent(thread, Intent::SetMode { mode: "plan".to_owned() }, cx));
    let refused = Outcome::Refused { reason: "no turn is running.".to_owned() };
    hub.update(cx, |hub, cx| hub.done(&IntentDone { id: stop, outcome: refused }, cx));
    let unsupported = Outcome::Unsupported { cap: Cap::named(Cap::SET_MODE) };
    hub.update(cx, |hub, cx| hub.done(&IntentDone { id: mode, outcome: unsupported }, cx));
    cx.run_until_parked();

    let words: Vec<String> =
        hub.read_with(cx, |h, _| h.refusals(thread).map(|r| r.words.clone()).collect());
    assert_eq!(
        words,
        [
            "Couldn't stop: no turn is running".to_owned(),
            "Couldn't switch to plan: the agent can't do that through Slopty".to_owned(),
        ]
    );
    // A selector is named for the test's life.
    let (stopped, moded) = (format!("refused-{stop}").leak(), format!("refused-{mode}").leak());
    assert!(cx.debug_bounds(stopped).is_some(), "said in the thread");
    assert!(cx.debug_bounds(moded).is_some());

    click(cx, format!("refused-dismiss-{stop}").leak());
    assert!(cx.debug_bounds(stopped).is_none(), "gone once read");
    assert!(cx.debug_bounds(moded).is_some(), "the other stays");
}

/// "Deny…" turns the request's answers into a field: what is written goes with the plain deny
/// as the answer's message, and Cancel brings the answers back.
#[gpui::test]
fn deny_with_a_reason_sends_the_reason_with_the_deny(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.requests = vec![approval("a")];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();

    click(cx, "deny-why-a");
    assert!(cx.debug_bounds("deny-reason-a").is_some(), "the field is up");
    assert!(cx.debug_bounds("answer-a-allow").is_none(), "in the answers' place");
    click(cx, "deny-why-cancel");
    assert!(cx.debug_bounds("answer-a-allow").is_some(), "the answers are back");
    assert!(intents(&sent).is_empty(), "nothing went");

    click(cx, "deny-why-a");
    cx.simulate_input("Not on main, use a branch");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        intents(&sent),
        [Intent::Answer {
            ask: AskId("a".to_owned()),
            choice: "deny".to_owned(),
            message: Some("Not on main, use a branch".to_owned()),
        }]
    );
}

/// A Codex request with nothing to answer here is handed to Codex's own TUI: the button says
/// so, sends the release, and the TUI's terminal comes into view once the thread names it.
#[gpui::test]
fn answer_in_codex_brings_its_terminal_into_view_once_it_runs(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    state.meta.agent = AgentId::named(AgentId::CODEX);
    state.meta.drive = Drive::named(Drive::SHARED);
    state.meta.caps = vec![Cap::named(Cap::LIVE_TUI), Cap::named(Cap::APPROVALS)];
    let mut secret = approval("s");
    secret.kind = "permissions".to_owned();
    secret.options.clear();
    state.requests = vec![secret];
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state.clone(), 0), cx));
    cx.run_until_parked();
    let asked = asked(cx, &view);

    let release = cx.debug_bounds("release-s").expect("the way to Codex's own prompt");
    cx.simulate_click(release.center(), Modifiers::none());
    assert_eq!(intents(&sent), [Intent::Release { ask: AskId("s".to_owned()) }]);
    assert!(asked.borrow().is_empty(), "no terminal to show yet");

    state.meta.terminal = Some(SessionId::new());
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    assert!(
        matches!(asked.borrow().as_slice(), [ThreadViewEvent::ShowTerminal]),
        "Codex's terminal comes into view: {:?}",
        asked.borrow()
    );
}

/// An exited Claude Code thread gives the composer's place to Resume, which starts its own
/// session again (`--resume <id>`) and says so while it is on its way; a start the worker turns
/// down says why.
#[gpui::test]
fn an_exited_claude_thread_is_resumed_on_its_own_session(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let state = exited(AgentId::CLAUDE_CODE, Drive::OBSERVED);
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-exited").is_some(), "the strip in the composer's place");

    click(cx, "thread-resume");
    let started = starts(&sent);
    let [(id, args)] = started.as_slice() else { panic!("one start: {started:?}") };
    assert_eq!(args, &["--resume".to_owned(), "s1".to_owned()]);
    assert!(hub.read_with(cx, |h, _| h.resuming(thread)));
    assert!(cx.debug_bounds("thread-resume").is_none(), "once");

    let refused = Outcome::Refused { reason: "Claude Code runs this session already".to_owned() };
    hub.update(cx, |hub, cx| hub.done(&IntentDone { id: *id, outcome: refused }, cx));
    cx.run_until_parked();
    let words: Vec<String> =
        hub.read_with(cx, |h, _| h.refusals(thread).map(|r| r.words.clone()).collect());
    assert_eq!(words, ["Couldn't resume: Claude Code runs this session already".to_owned()]);
    assert!(cx.debug_bounds("thread-resume").is_some(), "to try again");
}

/// An exited Codex thread is taken up again by a start of the same thread, which brings the
/// person's app-server up on the worker.
#[gpui::test]
fn an_exited_codex_thread_is_resumed_by_a_start_of_the_same_thread(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let state = exited(AgentId::CODEX, Drive::SHARED);
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();

    click(cx, "thread-resume");
    let started = starts(&sent);
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].1, ["resume".to_owned(), "s1".to_owned()]);
}

/// An exited pi thread keeps its composer and says the next message starts pi again.
#[gpui::test]
fn an_exited_pi_thread_goes_on_with_the_next_message(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let state = exited(AgentId::PI, Drive::DRIVEN);
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();

    assert!(cx.debug_bounds("thread-exited-line").is_some());
    assert!(cx.debug_bounds("thread-exited").is_none(), "no strip: the composer stays");
    cx.simulate_input("Carry on");
    cx.simulate_keystrokes("enter");
    assert_eq!(intents(&sent).len(), 1, "the message goes, and starts pi again");
    assert_eq!(starts(&sent), Vec::<(slopty_proto::thread::IntentId, Vec<String>)>::new());
}

/// The screen the agent drove last is offered in the composer's toolbar, by its window's
/// title, and in the palette; either opens it beside the thread. With none, neither shows.
#[gpui::test]
fn the_screen_the_agent_drives_is_offered_beside_it(cx: &mut TestAppContext) {
    use slopty_proto::screen::CaptureTarget;
    use slopty_proto::thread::AgentScreen;

    use crate::conversation::WatchAgentScreen;

    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state.clone(), 0), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-screen").is_none(), "no screen, no chip");
    let available = |cx: &mut gpui::VisualTestContext| {
        cx.update(|window, cx| window.is_action_available(&WatchAgentScreen, cx))
    };
    assert!(!available(cx));

    let screen = AgentScreen {
        target: CaptureTarget::Window(slopty_core::WindowId(41)),
        kind: AgentScreen::SIMULATOR.to_owned(),
        label: "Simulator \u{2014} iPhone 17 Pro".to_owned(),
        used_ms: slopty_core::WallMs::ZERO,
    };
    state.screens = vec![screen.clone()];
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    let asked = asked(cx, &view);
    click(cx, "thread-screen");
    assert!(
        matches!(asked.borrow().as_slice(), [ThreadViewEvent::Watch { screen: s, .. }] if *s == screen),
        "{:?}",
        asked.borrow()
    );
    assert!(available(cx), "and in the palette");
}
