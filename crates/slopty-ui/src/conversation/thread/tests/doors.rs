//! What the person asks of an agent beyond a message, through the agent's own doors, and what
//! the thread says when the worker turns it down: a refusal in words, a denial with a reason,
//! the agent's own TUI for a request, an exited agent taken up again, and one asleep woken.

use gpui::{Modifiers, TestAppContext, px};
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

/// "Deny with a reason…", behind the deny's chevron, turns the request's answers into a field:
/// what is written goes with the plain deny as the answer's message, and Cancel brings the
/// answers back.
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

    click(cx, "denials-a");
    click(cx, "denials-menu-a-why");
    assert!(cx.debug_bounds("deny-reason-a").is_some(), "the field is up");
    assert!(cx.debug_bounds("answer-a-allow").is_none(), "in the answers' place");
    click(cx, "deny-why-cancel");
    assert!(cx.debug_bounds("answer-a-allow").is_some(), "the answers are back");
    assert!(intents(&sent).is_empty(), "nothing went");

    click(cx, "denials-a");
    click(cx, "denials-menu-a-why");
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

/// A reason keeps its lines: pasted line ends stay and ⇧↵ breaks a line, where ↵ still denies.
#[gpui::test]
fn a_reason_keeps_its_lines(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.requests = vec![approval("a")];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();

    click(cx, "denials-a");
    click(cx, "denials-menu-a-why");
    cx.simulate_input("Not on main.\nUse a branch.");
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_input("Then ask again.");
    assert!(intents(&sent).is_empty(), "⇧↵ only broke the line");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        intents(&sent),
        [Intent::Answer {
            ask: AskId("a".to_owned()),
            choice: "deny".to_owned(),
            message: Some("Not on main.\nUse a branch.\nThen ask again.".to_owned()),
        }]
    );
}

/// An approval is one decision: Deny and Allow side by side a base unit apart, the solid last;
/// "Deny and stop" waits behind the deny's chevron and answers from there, and so does
/// answering in the terminal; the standing grant leads the same row from its other end.
#[gpui::test]
fn an_approval_is_allow_and_deny_with_the_rest_set_apart(cx: &mut TestAppContext) {
    use slopty_proto::thread::{Choice, Effect};

    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    let mut asks = approval("a");
    let choice = |id: &str, label: &str, effect, scope: Option<&str>, stops| Choice {
        id: id.to_owned(),
        label: label.to_owned(),
        effect,
        scope: scope.map(str::to_owned),
        stops,
    };
    asks.options = vec![
        choice("always", "Always allow", Effect::Allow, Some("/work; accept edits mode"), false),
        choice("deny", "Deny", Effect::Deny, None, false),
        choice("stop", "Deny and stop", Effect::Deny, None, true),
        choice("allow", "Allow", Effect::Allow, None, false),
    ];
    state.requests = vec![asks];
    state.meta.terminal = Some(SessionId::new());
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();

    let bounds = |cx: &mut gpui::VisualTestContext, s: &'static str| {
        cx.debug_bounds(s).unwrap_or_else(|| panic!("{s} is on show"))
    };
    let (deny, chevron, allow) =
        (bounds(cx, "answer-a-deny"), bounds(cx, "denials-a"), bounds(cx, "answer-a-allow"));
    assert!(deny.right() <= chevron.left() && chevron.right() < allow.left(), "deny, then allow");
    let gap = f32::from(allow.left() - chevron.right());
    assert!((gap - 8.0).abs() < 0.5, "a base unit apart: {gap}");
    assert!(cx.debug_bounds("answer-a-stop").is_none(), "the other denial waits in its menu");
    let always = bounds(cx, "answer-a-always");
    assert!(always.right() <= deny.left(), "the standing grant leads the row");
    assert!(always.top() < deny.bottom() && deny.top() < always.bottom(), "on the same row");
    assert!(bounds(cx, "standing-a").contains(&always.center()), "and only there");
    assert!(cx.debug_bounds("release-a").is_none(), "the terminal waits in the menu");

    click(cx, "denials-a");
    cx.run_until_parked();
    assert!(cx.debug_bounds("denials-menu-a-release").is_some(), "Answer in the terminal");
    click(cx, "denials-menu-a-stop");
    cx.run_until_parked();
    assert_eq!(
        intents(&sent),
        [Intent::Answer { ask: AskId("a".to_owned()), choice: "stop".to_owned(), message: None }]
    );
}

/// Auto mode's decline put to the person is two answers, the agent's own words on them: "Let it
/// try again" the solid, "Keep it declined" beside it. Neither a reason nor the agent's terminal
/// waits behind the keep, since a reason reaches nobody and the terminal asks nothing.
#[gpui::test]
fn auto_modes_decline_is_let_try_again_or_kept(cx: &mut TestAppContext) {
    use slopty_proto::thread::{Choice, Effect, Request};

    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    let mut declined = approval("r");
    declined.kind = Request::RETRY.to_owned();
    declined.title = "Auto mode declined Bash: Irreversible Local Destruction".to_owned();
    let choice = |id: &str, label: &str, effect| Choice {
        id: id.to_owned(),
        label: label.to_owned(),
        effect,
        scope: None,
        stops: false,
    };
    declined.options = vec![
        choice("deny", "Keep it declined", Effect::Deny),
        choice("allow", "Let it try again", Effect::Allow),
    ];
    state.requests = vec![declined];
    state.meta.terminal = Some(SessionId::new());
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();

    let (keep, again) = (cx.debug_bounds("answer-r-deny"), cx.debug_bounds("answer-r-allow"));
    let (keep, again) = (keep.expect("Keep it declined"), again.expect("Let it try again"));
    assert!(keep.right() < again.left(), "keep, then the solid");
    assert!(cx.debug_bounds("denials-r").is_none(), "nothing waits behind the keep");
    assert!(cx.debug_bounds("release-r").is_none(), "no prompt in the terminal");
    click(cx, "answer-r-allow");
    cx.run_until_parked();
    assert_eq!(
        intents(&sent),
        [Intent::Answer { ask: AskId("r".to_owned()), choice: "allow".to_owned(), message: None }]
    );
}

/// In a column beside a board (312 pt) long answers wrap among themselves rather than push
/// the primary out of the card: every answer stands inside the card, and the one solid ends
/// the last line.
#[gpui::test]
fn long_answers_wrap_inside_a_narrow_card(cx: &mut TestAppContext) {
    use slopty_proto::thread::{Choice, Effect};

    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    let mut asks = approval("a");
    let choice = |id: &str, label: &str, effect| Choice {
        id: id.to_owned(),
        label: label.to_owned(),
        effect,
        scope: None,
        stops: false,
    };
    asks.options = vec![
        choice("session", "Yes, and don't ask again for this command", Effect::Answer),
        choice("deny", "No, and tell Codex what to do differently", Effect::Deny),
        choice("allow", "Yes, run it once", Effect::Allow),
    ];
    state.requests = vec![asks];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    cx.simulate_resize(gpui::size(px(312.0), px(600.0)));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();

    let card = cx.debug_bounds("request-a").expect("the card");
    let answers = ["answer-a-session", "answer-a-deny", "answer-a-allow"];
    for answer in answers {
        let b = cx.debug_bounds(answer).unwrap_or_else(|| panic!("{answer} is on show"));
        assert!(b.left() >= card.left() && b.right() <= card.right(), "{answer} {b:?} in {card:?}");
    }
    let allow = cx.debug_bounds("answer-a-allow").expect("allow");
    let session = cx.debug_bounds("answer-a-session").expect("the session's answer");
    assert!(allow.top() >= session.bottom(), "they wrap: {session:?} over {allow:?}");
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

/// Each of the thread's buttons is an action too, for the palette and a bound key, answered only
/// while its button would show: Review over the last turn's edits, Take back while its own TUI
/// holds the session, Compact where the agent compacts through Slopty, Branch from here under
/// the last message, and Resume once the agent has exited. The agent's terminal is ⌘J's, on the
/// tile, so the thread has no action of its own for it.
#[gpui::test]
fn every_button_of_the_thread_is_an_action_while_it_shows(cx: &mut TestAppContext) {
    use crate::conversation::{
        BranchFromHere, CompactContext, ResumeAgent, ReviewChanges, TakeBack,
    };

    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::thread("edit");
    state.meta.caps = [Cap::HANDOFF, Cap::COMPACT, Cap::FORK].map(Cap::named).to_vec();
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state.clone(), 0), cx));
    cx.run_until_parked();
    let asked = asked(cx, &view);
    let available = |cx: &mut gpui::VisualTestContext, action: &dyn gpui::Action| {
        cx.update(|window, cx| window.is_action_available(action, cx))
    };
    assert!(!available(cx, &TakeBack), "Slopty drives it");
    assert!(!available(cx, &ResumeAgent), "it runs");

    cx.dispatch_action(ReviewChanges);
    assert!(matches!(asked.borrow().as_slice(), [ThreadViewEvent::Review { .. }]));
    cx.dispatch_action(CompactContext);
    assert_eq!(intents(&sent), [Intent::Compact]);
    cx.dispatch_action(BranchFromHere);
    cx.run_until_parked();
    assert!(cx.debug_bounds("branch-panel").is_some(), "open under the last message");

    state.meta.terminal = Some(SessionId::new());
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state.clone(), 1), cx));
    cx.run_until_parked();
    assert!(available(cx, &CompactContext), "focus held");
    assert!(available(cx, &TakeBack), "the TUI holds it");
    cx.dispatch_action(TakeBack);
    assert_eq!(intents(&sent), [Intent::Compact, Intent::TakeBack]);
    cx.run_until_parked();
    assert!(!available(cx, &TakeBack), "once, while it is on its way");
    state.meta.terminal = None;
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state.clone(), 2), cx));
    cx.run_until_parked();
    let field = view.read_with(cx, gpui::Focusable::focus_handle);
    assert!(cx.update(|window, _| field.is_focused(window)), "the field has the keyboard back");

    state.status.liveness = Liveness::Exited { resumable: true };
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 3), cx));
    cx.run_until_parked();
    cx.dispatch_action(ResumeAgent);
    let started = starts(&sent);
    assert_eq!(started.len(), 1, "{started:?}");
    assert_eq!(started[0].1.first().map(String::as_str), Some("--resume"));
}
