//! The composer beyond its text: its `/` and `@` menus, attachments, and changing a message
//! that waits in the queue.

use gpui::{Modifiers, MouseButton, TestAppContext};
use slopty_core::SessionId;
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{
    AgentId, Cap, Command, Delivery, IntentId, Pending, PendingState, ThreadState,
};

use super::{asked, hub, intents, snapshot, view};
use crate::conversation::composer::{self, Attach};
use crate::conversation::thread::fixtures;
use crate::conversation::thread::hub::ThreadHub;
use crate::conversation::thread::view::{ThreadView, ThreadViewEvent};

const ROOT: &str = "/Users/me/app";

fn command(name: &str, description: &str, source: &str) -> Command {
    Command {
        name: name.to_owned(),
        description: description.to_owned(),
        argument_hint: None,
        source: source.to_owned(),
    }
}

fn state() -> ThreadState {
    let mut state = fixtures::empty();
    state.meta.cwd = ROOT.to_owned();
    state.meta.caps = vec![Cap::named(Cap::QUEUE), Cap::named(Cap::STEER)];
    state
}

/// `/` lists the agent's own commands, the project's first; ↵ writes the one the keyboard is
/// on into the draft and sends nothing.
#[gpui::test]
fn a_slash_lists_the_agent_s_commands_and_return_picks_one(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = state();
    let thread = state.meta.id;
    state.commands = vec![
        command("compact", "Free up context", "built-in"),
        command("commit", "Commit the staged work", "project"),
        command("model", "Set the model", "built-in"),
    ];
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();

    cx.simulate_input("/co");
    assert!(cx.debug_bounds("thread-menu").is_some(), "the menu opens in that frame");
    assert!(cx.debug_bounds("thread-menu-1").is_some(), "two commands match");
    assert!(cx.debug_bounds("thread-menu-2").is_none(), "and only two");
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, ThreadView::draft), "/commit ", "the project's comes first");
    assert!(intents(&sent).is_empty(), "picking sends nothing");
    assert!(cx.debug_bounds("thread-menu").is_none(), "the word is whole: the menu closes");

    cx.simulate_keystrokes("enter");
    assert!(
        matches!(intents(&sent).as_slice(), [Intent::Send { text, .. }] if text == "/commit"),
        "↵ with the menu closed sends: {:?}",
        intents(&sent)
    );
}

/// `@` asks the worker for the paths under the agent's directory; while the answer for the
/// newest key is on its way the last one, narrowed here, stands in; Esc closes the menu for
/// the word and ⇥ picks.
#[gpui::test]
fn an_at_lists_what_the_worker_found_and_never_blanks_between_keys(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let state = state();
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();
    let asked = asked(cx, &view);

    cx.simulate_input("see @");
    assert!(cx.debug_bounds("thread-menu-note").is_some(), "a bare @ says to type");
    assert!(asked.borrow().is_empty(), "nothing to look up yet");
    cx.simulate_input("ma");
    let finds: Vec<(String, String)> = asked
        .borrow()
        .iter()
        .filter_map(|e| match e {
            ThreadViewEvent::FindFiles { root, query } => Some((root.clone(), query.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(finds.last(), Some(&(ROOT.to_owned(), "ma".to_owned())), "{finds:?}");
    assert!(cx.debug_bounds("thread-menu-note").is_some(), "searching");

    let paths = ["src/main.rs".to_owned(), "src/map/".to_owned(), "README.md".to_owned()];
    view.update(cx, |v, cx| v.files_found("/elsewhere", "ma", &paths, cx));
    assert!(cx.debug_bounds("thread-menu-0").is_none(), "another directory's answer is not ours");
    view.update(cx, |v, cx| v.files_found(ROOT, "ma", &paths, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-menu-2").is_some(), "the worker's three");

    cx.simulate_input("in");
    assert!(cx.debug_bounds("thread-menu-0").is_some(), "the last answer stands in");
    assert!(cx.debug_bounds("thread-menu-1").is_none(), "narrowed to what still matches");

    cx.simulate_keystrokes("escape");
    assert!(cx.debug_bounds("thread-menu").is_none(), "closed for the word");
    cx.simulate_keystrokes("backspace");
    assert!(cx.debug_bounds("thread-menu").is_none(), "still closed while the caret is in it");

    // A fresh word opens it again; ⇥ picks.
    cx.simulate_input(" @ma");
    assert!(cx.debug_bounds("thread-menu-0").is_some(), "open for the new word");
    cx.simulate_keystrokes("tab");
    assert_eq!(view.read_with(cx, ThreadView::draft), "see @mai @src/main.rs ");
}

/// A waiting message's pencil puts its words in the composer, the draft aside; ↵ sends the
/// change and the draft comes back, the line reading the new words at once; Esc puts the
/// draft back and changes nothing.
#[gpui::test]
fn a_queued_message_is_changed_in_the_composer(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = state();
    let thread = state.meta.id;
    let waiting = IntentId::new();
    state.pending = vec![Pending {
        intent: waiting,
        text: "after this".to_owned(),
        attachments: vec![],
        delivery: Delivery::Queue,
        state: PendingState::Waiting,
    }];
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();
    cx.simulate_input("half typed");

    let pencil = format!("edit-{waiting}").leak();
    let at = cx.debug_bounds(pencil).expect("a waiting message can be changed").center();
    cx.simulate_click(at, Modifiers::none());
    assert_eq!(view.read_with(cx, ThreadView::draft), "after this", "its words, to change");
    assert!(cx.debug_bounds("thread-editing").is_some(), "the composer says so");
    assert!(cx.debug_bounds(pencil).is_none(), "one change at a time");

    cx.simulate_keystrokes("escape");
    assert_eq!(view.read_with(cx, ThreadView::draft), "half typed", "Esc puts the draft back");
    assert!(intents(&sent).is_empty(), "and changes nothing, nor stops anything");

    let at = cx.debug_bounds(pencil).expect("again").center();
    cx.simulate_click(at, Modifiers::none());
    cx.simulate_input(" and that");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        intents(&sent),
        [Intent::Edit { pending: waiting, text: "after this and that".to_owned() }]
    );
    assert_eq!(view.read_with(cx, ThreadView::draft), "half typed", "the draft is back");
    assert!(cx.debug_bounds("thread-editing").is_none());
}

/// A thread with a terminal takes files: ↵ while one is on its way up waits for it, says so,
/// and sends by itself the moment it lands, carrying its path.
#[gpui::test]
fn a_message_waits_for_its_attachment_and_carries_its_path(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = state();
    state.meta.terminal = Some(SessionId::new());
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();
    let asked = asked(cx, &view);

    view.update(cx, |v, cx| v.attach(Attach::Files(vec!["/tmp/shot.png".into()]), cx));
    cx.run_until_parked();
    let id = match asked.borrow().as_slice() {
        [ThreadViewEvent::Attach { id, .. }] => *id,
        other => panic!("the workspace is asked to send it up: {other:?}"),
    };
    assert!(cx.debug_bounds("composer-attachment").is_some(), "its chip, at once");
    cx.simulate_input("Look at this");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(intents(&sent).is_empty(), "not while it is on its way up");
    assert_eq!(
        view.read_with(cx, |v, _| v.composer_notice().map(str::to_owned)).as_deref(),
        Some("Sends once the attachments are up"),
        "\u{21b5} is never silent"
    );
    assert!(cx.debug_bounds("thread-composer-notice").is_some());

    let landed = ["/drop/x/shot.png".to_owned()];
    view.update(cx, |v, cx| v.attachment_landed(id, &landed, cx));
    cx.run_until_parked();
    let text = composer::with_paths("Look at this", &landed);
    assert_eq!(
        intents(&sent),
        [Intent::Send { text, delivery: Delivery::Steer, attachments: vec![] }]
    );
    assert!(cx.debug_bounds("composer-attachment").is_none(), "the chip went with it");
    assert!(cx.debug_bounds("thread-composer-notice").is_none(), "nothing left to say");

    view.update(cx, |v, cx| v.attach(Attach::Files(vec!["/tmp/a.txt".into()]), cx));
    let remove = cx.debug_bounds("composer-attachment-remove").expect("its way off").center();
    cx.simulate_click(remove, Modifiers::none());
    assert!(
        matches!(asked.borrow().last(), Some(ThreadViewEvent::Detach { .. })),
        "taken off: its upload stops"
    );
}

/// A send waiting on an upload that fails sends nothing and says so; the words stay in the
/// composer for the person to try again.
#[gpui::test]
fn a_failed_upload_under_a_waiting_send_sends_nothing_and_says_so(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = state();
    state.meta.terminal = Some(SessionId::new());
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();
    let asked = asked(cx, &view);

    view.update(cx, |v, cx| v.attach(Attach::Files(vec!["/tmp/shot.png".into()]), cx));
    cx.run_until_parked();
    let id = match asked.borrow().as_slice() {
        [ThreadViewEvent::Attach { id, .. }] => *id,
        other => panic!("the workspace is asked to send it up: {other:?}"),
    };
    cx.simulate_input("Look at this");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.armed()));

    view.update(cx, |v, cx| v.attachment_ended(id, cx));
    cx.run_until_parked();
    assert!(intents(&sent).is_empty(), "nothing goes without its file");
    assert!(!view.read_with(cx, |v, _| v.armed()));
    assert_eq!(
        view.read_with(cx, |v, _| v.composer_notice().map(str::to_owned)).as_deref(),
        Some("An attachment didn't upload, so nothing was sent")
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        intents(&sent),
        [Intent::Send {
            text: "Look at this".to_owned(),
            delivery: Delivery::Steer,
            attachments: vec![]
        }],
        "the words were kept to send again"
    );
}

/// A thread with no terminal (Codex, pi, ACP) can't take a file yet: offering one says so in
/// the composer, adds no chip, and \u{21b5} still sends the words.
#[gpui::test]
fn a_thread_with_no_terminal_says_it_cannot_take_a_file(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = state();
    state.meta.agent = AgentId::named(AgentId::PI);
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();
    let asked = asked(cx, &view);

    view.update(cx, |v, cx| v.attach(Attach::Files(vec!["/tmp/shot.png".into()]), cx));
    cx.run_until_parked();
    assert!(asked.borrow().is_empty(), "nothing is sent up");
    assert!(cx.debug_bounds("composer-attachment").is_none(), "no chip");
    let notice = view.read_with(cx, |v, _| v.composer_notice().map(str::to_owned));
    assert_eq!(notice.as_deref(), Some("Files can't be attached to pi threads yet"));

    cx.simulate_input("Look at this");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        intents(&sent),
        [Intent::Send {
            text: "Look at this".to_owned(),
            delivery: Delivery::Steer,
            attachments: vec![]
        }]
    );
}

/// A secondary click on Send (a right click, or a long press on touch) queues the message
/// behind the turn instead of steering it.
#[gpui::test]
fn a_secondary_click_on_send_queues_the_message(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let state = state();
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();

    cx.simulate_input("After this turn");
    let send = cx.debug_bounds("thread-send").expect("the send button").center();
    cx.simulate_mouse_down(send, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(send, MouseButton::Right, Modifiers::none());
    assert_eq!(
        intents(&sent),
        [Intent::Send {
            text: "After this turn".to_owned(),
            delivery: Delivery::Queue,
            attachments: vec![]
        }]
    );
}

/// An agent that compacts lists `/compact` even when it lists no such command, and sending
/// it asks the agent to compact rather than saying the words.
#[gpui::test]
fn compact_is_a_command_when_the_agent_compacts(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = state();
    state.meta.caps.push(Cap::named(Cap::COMPACT));
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();

    let listed =
        view.read_with(cx, |v, cx| v.commands(cx).into_iter().map(|c| c.name).collect::<Vec<_>>());
    assert!(listed.iter().any(|c| c == "compact"), "{listed:?}");
    cx.simulate_input("/compact");
    cx.simulate_keystrokes("enter");
    assert!(intents(&sent).is_empty(), "\u{21b5} on the menu writes the command");
    cx.simulate_keystrokes("enter");
    assert_eq!(intents(&sent), [Intent::Compact]);
}
