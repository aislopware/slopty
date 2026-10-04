//! The composer beyond its text: its `/` and `@` menus, attachments, changing a message that
//! waits in the queue, what the send button says, the meter's panel, and recalling what was
//! sent.

use gpui::{Modifiers, MouseButton, TestAppContext};
use slopty_core::{SessionId, WallMs};
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{
    AgentId, Cap, Changed, Clipped, Command, Delivery, IntentId, Item, ItemBody, ItemId, Limit,
    Pending, PendingState, Phase, ThreadState, Turn, TurnId, TurnState, Usage, UserMessage,
};

use super::{asked, hub, intents, snapshot, view};
use crate::conversation::attach::{self, Attach};
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
    let text = attach::with_paths("Look at this", &landed);
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

/// `state` with a turn under way.
fn working(mut state: ThreadState) -> ThreadState {
    state.status.phase = Phase::Working;
    state.turns = vec![Turn {
        id: TurnId(1),
        input: None,
        state: TurnState::Active,
        started_ms: WallMs::ZERO,
        ended_ms: None,
        usage: Usage::default(),
        models: Vec::new(),
        changed: Changed::default(),
        before: None,
        after: None,
    }];
    state
}

fn waiting(text: &str) -> Pending {
    Pending {
        intent: IntentId::new(),
        text: text.to_owned(),
        attachments: vec![],
        delivery: Delivery::Queue,
        state: PendingState::Waiting,
    }
}

/// The send button names what ↵ will do: Send with no turn under way, Steer into one where
/// the agent takes a steer, Queue after it where it only queues, and Update while a waiting
/// message is being changed.
#[gpui::test]
fn the_send_button_says_what_return_will_do(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = state();
    let thread = state.meta.id;
    let first = waiting("after this");
    state.pending = vec![first.clone()];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    cx.update(|window, _cx| window.set_a11y_active(true));
    let said = |cx: &mut gpui::VisualTestContext| {
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        ["Send", "Steer", "Queue", "Update"]
            .into_iter()
            .find(|l| tree.iter().any(|n| n.is("Button", Some(l))))
    };
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state.clone(), 0), cx));
    cx.simulate_input("next");
    assert_eq!(said(cx), Some("Send"));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(working(state.clone()), 1), cx));
    assert_eq!(said(cx), Some("Steer"));
    let mut queues = working(state);
    queues.meta.caps = vec![Cap::named(Cap::QUEUE)];
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(queues, 2), cx));
    assert_eq!(said(cx), Some("Queue"));
    let pencil = format!("edit-{}", first.intent).leak();
    let at = cx.debug_bounds(pencil).expect("a waiting message").center();
    cx.simulate_click(at, Modifiers::none());
    assert_eq!(said(cx), Some("Update"));
}

/// ⌘↵ is the keymap's "Queue message": the draft waits for the turn under way. ⌥↑ takes the
/// last waiting message into the composer to change, as its pencil does.
#[gpui::test]
fn command_return_queues_and_option_up_edits_the_last_waiting(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = working(state());
    let thread = state.meta.id;
    state.pending = vec![waiting("first"), waiting("second")];
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();

    cx.simulate_input("later");
    cx.simulate_keystrokes("cmd-enter");
    assert_eq!(
        intents(&sent),
        [Intent::Send { text: "later".to_owned(), delivery: Delivery::Queue, attachments: vec![] }]
    );
    cx.simulate_keystrokes("alt-up");
    assert_eq!(view.read_with(cx, ThreadView::draft), "second", "the last one waiting");
    assert!(cx.debug_bounds("thread-editing").is_some(), "the composer says so");
}

/// A press on the meter opens its panel in the tray: the context, each window with its reset,
/// and "Compact context" where the agent compacts through Slopty. No dollar figure is shown.
#[gpui::test]
fn the_meter_opens_its_panel_and_compacts_on_a_press(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = state();
    state.meta.caps.push(Cap::named(Cap::COMPACT));
    state.meters.context_tokens = Some(50_000);
    state.meters.context_window = Some(200_000);
    state.meters.limits =
        vec![Limit { name: "five-hour".to_owned(), used_bp: 4_200, resets_ms: None }];
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    cx.update(|window, _cx| window.set_a11y_active(true));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-meter-panel").is_none(), "quiet until asked");

    let meter = cx.debug_bounds("thread-meter").expect("the meter").center();
    cx.simulate_click(meter, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-meter-panel").is_some(), "the panel is open");
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    let words: Vec<_> = tree.iter().filter_map(|n| n.label.clone()).collect();
    assert!(!words.iter().any(|w| w.contains('$')), "{words:?}");
    assert!(words.iter().any(|w| w.contains("Five hour 42%")), "{words:?}");
    let compact = cx.debug_bounds("thread-compact").expect("Compact context").center();
    cx.simulate_click(compact, Modifiers::none());
    assert_eq!(intents(&sent), [Intent::Compact]);
}

fn sent_message(id: &str, words: &str) -> Item {
    Item {
        id: ItemId(id.to_owned()),
        turn: TurnId(1),
        at_ms: WallMs::ZERO,
        body: ItemBody::User(UserMessage {
            text: Clipped::whole(words),
            images: Vec::new(),
            command: None,
            intent: None,
        }),
    }
}

/// ↑ on the first line of an empty composer brings back the message sent before, and ↓ on the
/// last line the one after, down to an empty draft. Inside a recalled message of two lines the
/// arrows move the caret first, and a draft of the person's own is left alone.
#[gpui::test]
fn up_recalls_the_messages_sent_and_down_comes_back(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = state();
    let thread = state.meta.id;
    state.items =
        vec![sent_message("u1", "Count the lines"), sent_message("u2", "Read it\nthen fix it")];
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();
    let draft = |cx: &mut gpui::VisualTestContext| view.read_with(cx, ThreadView::draft);

    cx.simulate_keystrokes("up");
    assert_eq!(draft(cx), "Read it\nthen fix it", "the newest first");
    cx.simulate_keystrokes("up");
    assert_eq!(draft(cx), "Read it\nthen fix it", "the caret climbs to the first line");
    cx.simulate_keystrokes("up");
    assert_eq!(draft(cx), "Count the lines");
    cx.simulate_keystrokes("up");
    assert_eq!(draft(cx), "Count the lines", "nothing before the first");
    cx.simulate_keystrokes("down");
    assert_eq!(draft(cx), "Read it\nthen fix it");
    cx.simulate_keystrokes("down");
    assert_eq!(draft(cx), "", "past the newest, an empty draft");

    cx.simulate_input("mine");
    cx.simulate_keystrokes("up");
    assert_eq!(draft(cx), "mine", "a draft of one's own is not replaced");
}

/// A thread a usage limit stopped says over the field when the limit lifts, where the agent
/// takes a message held until its moment; "Continue at" sends the draft, or "Continue" with
/// nothing typed, to go then. Once one waits, the line goes.
#[gpui::test]
fn a_thread_a_limit_stopped_continues_when_it_lifts(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = state();
    let thread = state.meta.id;
    let lifts = WallMs::from_millis(WallMs::now().as_millis().saturating_add(3_600_000));
    let mut stopped = working(state.clone()).turns.remove(0);
    stopped.state = TurnState::Failed { error: "limit".to_owned(), until_ms: Some(lifts) };
    stopped.ended_ms = Some(WallMs::from_millis(1_000));
    state.turns = vec![stopped];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state.clone(), 0), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-limit").is_none(), "no door, no line");

    state.meta.caps.push(Cap::named(Cap::SCHEDULE));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state.clone(), 1), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-limit").is_some(), "when the limit lifts");
    let at = cx.debug_bounds("thread-continue-at").expect("Continue at").center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    let said = intents(&sent);
    let [Intent::Send { text, delivery: Delivery::At { at_ms }, .. }] = said.as_slice() else {
        panic!("held until the limit lifts: {said:?}")
    };
    assert_eq!((text.as_str(), *at_ms), ("Continue", lifts));

    let mut held = waiting("Continue");
    held.delivery = Delivery::At { at_ms: lifts };
    state.pending = vec![held];
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 2), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-limit").is_none(), "the tray says when it goes");
}

/// What waits for its moment says in the tray when it goes or what it waits on, and offers
/// Send now where the agent takes a message into the turn under way.
#[gpui::test]
fn a_held_message_says_when_and_goes_now_on_a_press(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = working(state());
    let thread = state.meta.id;
    state.meta.caps.push(Cap::named(Cap::SCHEDULE));
    let mut held = waiting("after the build");
    let at_ms = WallMs::from_millis(WallMs::now().as_millis().saturating_add(3_600_000));
    held.delivery = Delivery::At { at_ms };
    let when = crate::conversation::figures::stamp(at_ms, WallMs::now()).expect("a time");
    state.pending = vec![held.clone()];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    cx.update(|window, _cx| window.set_a11y_active(true));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    assert!(
        tree.iter().any(|n| n.is("ListItem", Some(&format!("after the build, {when}")))),
        "what it waits on"
    );
    let now = format!("promote-{}", held.intent).leak();
    let at = cx.debug_bounds(now).expect("Send now").center();
    cx.simulate_click(at, Modifiers::none());
    assert_eq!(intents(&sent), [Intent::Promote { pending: held.intent }]);
}

/// After the person stops a turn, what waited in the queue is held for them: one line over the
/// queue says so, each held message can be sent now, and the line goes once the worker lets
/// them wait for their turn again.
#[gpui::test]
fn a_stop_pauses_the_queue_until_the_next_message(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = state();
    let thread = state.meta.id;
    let mut first = waiting("then the docs");
    assert!(first.hold_for_stop());
    state.pending = vec![first.clone()];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    cx.update(|window, _cx| window.set_a11y_active(true));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state.clone(), 0), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-queue-paused").is_some(), "the queue says it paused");
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    let said = format!("then the docs, {}", Pending::STOPPED);
    assert!(!tree.iter().any(|n| n.label.as_deref() == Some(&said)), "said once, not per row");

    let send_now = format!("promote-{}", first.intent).leak();
    let at = cx.debug_bounds(send_now).expect("a held message can go now").center();
    cx.simulate_click(at, Modifiers::none());
    assert_eq!(intents(&sent), [Intent::Promote { pending: first.intent }]);

    first.state = PendingState::Waiting;
    state.pending = vec![first];
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-queue-paused").is_none(), "let go");
}
