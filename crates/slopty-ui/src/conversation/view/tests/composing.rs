//! The composer's menus, the question it answers, a plan's approval, a prompt's mentions,
//! recall, the model it types, the rewind it hands over, and a worker out of reach.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use slopty_core::WallMs;
use slopty_proto::agent::{AgentEvent, AgentKind, AgentSource, AgentStatus, HeardMode};
use slopty_proto::conversation::{
    Answer, Body, Change, Choice, Clipped, CommandSource, ConversationEvent, Entry,
    PermissionEvent, PermissionPrompt, Prompt, Question, QuestionDetail, Settled, SlashCommand,
    ThreadId, ToolDetail, Verdict,
};

use super::{face, feed};
use crate::conversation::question::Answering;
use crate::conversation::view::composing::MenuRows;
use crate::conversation::{ConversationView, FaceEvent};

/// Everything the face asks the workspace for, in order.
fn events(
    view: &Entity<ConversationView>,
    cx: &mut VisualTestContext,
) -> Rc<RefCell<Vec<FaceEvent>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    cx.update(|_, cx| {
        cx.subscribe(view, move |_, event: &FaceEvent, _| sink.borrow_mut().push(event.clone()))
            .detach();
    });
    seen
}

fn draft(view: &Entity<ConversationView>, cx: &VisualTestContext) -> String {
    view.read_with(cx, ConversationView::draft)
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} drawn")).center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
}

fn command(name: &str, description: &str, source: CommandSource) -> SlashCommand {
    SlashCommand {
        name: name.to_owned(),
        description: description.to_owned(),
        argument_hint: None,
        source,
    }
}

fn menu_names(view: &Entity<ConversationView>, cx: &VisualTestContext) -> Option<Vec<String>> {
    view.read_with(cx, |v, cx| match v.menu_rows(cx)? {
        MenuRows::Commands(commands) => Some(commands.into_iter().map(|c| c.name).collect()),
        MenuRows::Paths(paths) => paths,
        MenuRows::Models => Some(vec!["models".to_owned()]),
    })
}

/// A leading `/` opens the command menu, which narrows as the word grows, moves with the
/// arrows and writes the pick into the draft on Enter; a `/` mid-line opens nothing, and the
/// draft is sent as typed only on the Enter after the pick.
#[gpui::test]
fn a_leading_slash_opens_the_command_menu_and_a_pick_is_written(cx: &mut TestAppContext) {
    let (view, cx) = face(cx, "tools");
    let seen = events(&view, cx);
    feed(
        &view,
        cx,
        vec![ConversationEvent::Commands(vec![
            command("compact", "Free up context", CommandSource::BuiltIn),
            command("commit", "Commit the staged work", CommandSource::Project),
            command("model", "Set the AI model", CommandSource::BuiltIn),
        ])],
    );
    cx.simulate_input("fix a/b");
    cx.run_until_parked();
    assert!(cx.debug_bounds("composer-menu").is_none(), "a slash mid-line");
    cx.simulate_keystrokes("cmd-a backspace");
    cx.simulate_input("/co");
    cx.run_until_parked();
    assert_eq!(menu_names(&view, cx), Some(vec!["commit".to_owned(), "compact".to_owned()]));
    assert!(cx.debug_bounds("composer-menu").is_some(), "the menu shows over the field");
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    assert_eq!(draft(&view, cx), "/compact ", "the pick is written, not sent");
    assert!(seen.borrow().iter().all(|e| !matches!(e, FaceEvent::Submit { .. })));
    assert!(cx.debug_bounds("composer-menu").is_none(), "the caret left the word");

    cx.simulate_keystrokes("cmd-a backspace");
    cx.simulate_input("/mo");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(menu_names(&view, cx), None, "Esc closes it for the word");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        seen.borrow()
            .iter()
            .any(|e| matches!(e, FaceEvent::Submit { text, .. } if text.trim() == "/mo")),
        "with the menu closed, Enter sends the draft as typed: {:?} {:?}",
        seen.borrow(),
        draft(&view, cx)
    );
}

/// While an input method holds a word in the composer (Telex on its way to "cơm", kana before
/// conversion), the keys it reads are its own: with the command menu open, the arrows, Enter
/// and Esc pick nothing, move nothing, close nothing and send nothing. Once the word is
/// committed, Enter picks from the menu as it always does.
#[gpui::test]
fn the_keys_an_input_method_reads_are_its_own_while_it_composes(cx: &mut TestAppContext) {
    use gpui::EntityInputHandler as _;
    let (view, cx) = face(cx, "tools");
    let seen = events(&view, cx);
    feed(
        &view,
        cx,
        vec![ConversationEvent::Commands(vec![
            command("commit", "Commit the staged work", CommandSource::Project),
            command("compact", "Free up context", CommandSource::BuiltIn),
        ])],
    );
    cx.simulate_input("/co");
    cx.run_until_parked();
    let listed = Some(vec!["commit".to_owned(), "compact".to_owned()]);
    assert_eq!(menu_names(&view, cx), listed, "the menu is open over the word");
    view.update_in(cx, |v, window, cx| {
        v.composer.update(cx, |field, cx| {
            field.replace_and_mark_text_in_range(None, "m", Some(1..1), window, cx);
        });
    });
    cx.run_until_parked();
    let composing =
        |cx: &mut VisualTestContext| view.read_with(cx, |v, cx| v.composer.read(cx).is_composing());
    assert!(composing(cx), "the input method holds its word");
    for key in ["down", "up", "tab", "enter", "escape"] {
        cx.simulate_keystrokes(key);
        cx.run_until_parked();
        assert_eq!(draft(&view, cx), "/com", "{key} mid-word leaves the draft as it is");
        assert_eq!(menu_names(&view, cx), listed, "{key} mid-word leaves the menu open");
        assert_eq!(view.read_with(cx, |v, _| v.menu.selected), 0, "{key} left the menu's row");
    }
    assert!(
        seen.borrow().iter().all(|e| !matches!(e, FaceEvent::Submit { .. })),
        "nothing was sent mid-word: {:?}",
        seen.borrow()
    );
    view.update_in(cx, |v, window, cx| {
        v.composer.update(cx, |field, cx| field.replace_text_in_range(None, "m", window, cx));
    });
    cx.run_until_parked();
    assert!(!composing(cx), "the word is committed");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(draft(&view, cx), "/commit ", "the Enter after the word picks the first row");
}

/// An `@` starting a word asks the worker for what the rest of the word matches; its answer
/// fills the menu, and a pick writes the path over the word.
#[gpui::test]
fn an_at_asks_the_worker_and_a_pick_writes_the_path(cx: &mut TestAppContext) {
    let (view, cx) = face(cx, "tools");
    let seen = events(&view, cx);
    cx.simulate_input("see @ma");
    cx.run_until_parked();
    let searched: Vec<String> = seen
        .borrow()
        .iter()
        .filter_map(|e| match e {
            FaceEvent::Search { query } => Some(query.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(searched.last().map(String::as_str), Some("ma"), "asked as the word grew");
    assert!(cx.debug_bounds("composer-menu-note").is_some(), "searching");
    let found = ConversationEvent::Found {
        query: "ma".to_owned(),
        paths: vec!["src/main.rs".to_owned(), "docs/manual/".to_owned()],
    };
    feed(&view, cx, vec![found]);
    assert!(cx.debug_bounds("composer-menu-1").is_some(), "a row a path");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(draft(&view, cx), "see @src/main.rs ");
}

fn question_prompt(view: &Entity<ConversationView>, cx: &VisualTestContext) -> PermissionPrompt {
    let choice = |label: &str| Choice { label: label.to_owned(), description: None };
    PermissionPrompt {
        editable: Vec::new(),
        session: view.read_with(cx, |v, _| v.session()),
        ask: 5,
        tool: "AskUserQuestion".to_owned(),
        detail: ToolDetail::Question(QuestionDetail {
            questions: vec![
                Question {
                    text: "Which layout?".to_owned(),
                    header: Some("Layout".to_owned()),
                    options: vec![choice("Split"), choice("Stacked")],
                    multi_select: false,
                },
                Question {
                    text: "Which panes?".to_owned(),
                    header: Some("Panes".to_owned()),
                    options: vec![choice("Files"), choice("Terminal")],
                    multi_select: true,
                },
            ],
            answers: Vec::new(),
        }),
        suggestions: Vec::new(),
        mode: None,
        asked_ms: WallMs::ZERO,
        until_ms: WallMs::from_millis(600_000),
    }
}

fn ask(view: &Entity<ConversationView>, cx: &mut VisualTestContext, prompt: PermissionPrompt) {
    view.update(cx, |v, cx| v.permission(PermissionEvent::Asked(Box::new(prompt)), None, cx));
    cx.run_until_parked();
}

/// A question takes the composer's shell: a digit picks a single choice and goes on, a click
/// toggles a multi choice, and Enter answers; the answers go back as the permission's answer,
/// keyed by each question's text.
#[gpui::test]
fn a_question_is_answered_from_the_composer(cx: &mut TestAppContext) {
    let (view, cx) = face(cx, "tools");
    let seen = events(&view, cx);
    let prompt = question_prompt(&view, cx);
    ask(&view, cx, prompt);
    assert!(cx.debug_bounds("question").is_some(), "the question has the shell");
    assert!(cx.debug_bounds("composer").is_none());
    assert_eq!(view.read_with(cx, |v, _| v.answering().map(Answering::position)), Some((1, 2)));

    cx.simulate_keystrokes("2");
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.answering().map(Answering::position)), Some((2, 2)));
    click(cx, "question-option-0");
    assert!(
        view.read_with(cx, |v, _| v.answering().is_some_and(|a| a.is_picked(0))),
        "a click picks"
    );
    cx.simulate_keystrokes("2");
    cx.run_until_parked();
    assert!(
        view.read_with(cx, |v, _| v.answering().is_some_and(|a| a.is_picked(1))),
        "and a digit"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let answers = vec![
        Answer { question: "Which layout?".to_owned(), answer: "Stacked".to_owned() },
        Answer { question: "Which panes?".to_owned(), answer: "Files, Terminal".to_owned() },
    ];
    assert_eq!(
        *seen.borrow(),
        [FaceEvent::Answer { ask: 5, verdict: Verdict::Answer { answers } }],
        "one answer, nothing typed into the terminal"
    );
}

/// A plan is approved or kept with words for Claude, never allowed for good.
#[gpui::test]
fn a_plan_is_approved_or_kept(cx: &mut TestAppContext) {
    let (view, cx) = face(cx, "tools");
    let seen = events(&view, cx);
    let mut plan =
        crate::conversation::fixtures::bash_prompt(view.read_with(cx, |v, _| v.session()), 8);
    plan.tool = "ExitPlanMode".to_owned();
    plan.detail = ToolDetail::Plan {
        plan: Clipped { text: "1. Split the view".to_owned(), lines: 1, chars: 17, full: None },
    };
    ask(&view, cx, plan.clone());
    assert!(cx.debug_bounds("allow-always").is_none(), "no always for a plan");
    click(cx, "deny");
    cx.simulate_input("Keep the old layout");
    click(cx, "deny-send");
    assert_eq!(
        *seen.borrow(),
        [FaceEvent::Answer {
            ask: 8,
            verdict: Verdict::Deny { message: "Keep the old layout".to_owned(), interrupt: false }
        }]
    );
    seen.borrow_mut().clear();
    plan.ask = 9;
    ask(&view, cx, plan);
    click(cx, "allow-once");
    assert_eq!(*seen.borrow(), [FaceEvent::Answer { ask: 9, verdict: Verdict::Allow }]);
}

/// A sent prompt's `@` mentions are chips under its words, and a chip opens its file.
#[gpui::test]
fn a_prompts_mentions_are_chips_that_open(cx: &mut TestAppContext) {
    let (view, cx) = face(cx, "tools");
    let seen = events(&view, cx);
    let text = "Tidy @src/main.rs and @docs/";
    let prompt = Entry {
        id: "p-mention".to_owned(),
        at_ms: WallMs::from_millis(1),
        body: Body::Prompt(Prompt {
            text: Clipped { text: text.to_owned(), lines: 1, chars: 28, full: None },
            images: Vec::new(),
            command: None,
        }),
    };
    feed(
        &view,
        cx,
        vec![ConversationEvent::Changes(vec![Change::Upsert {
            thread: ThreadId::Main,
            entry: prompt,
        }])],
    );
    click(cx, "mention-src/main.rs");
    assert_eq!(*seen.borrow(), [FaceEvent::OpenPath { path: "src/main.rs".to_owned() }]);
}

/// ↑ in an empty composer brings back the prompts sent before, newest first, and ↓ goes
/// forward to the empty draft again.
#[gpui::test]
fn up_in_an_empty_composer_recalls_the_prompts(cx: &mut TestAppContext) {
    let (view, cx) = face(cx, "tools");
    let newest = view.read_with(cx, |v, _| {
        v.model()
            .thread(&ThreadId::Main)
            .and_then(|t| {
                t.entries().iter().rev().find_map(|e| match &e.body {
                    Body::Prompt(p) => Some(p.text.text.clone()),
                    _ => None,
                })
            })
            .expect("the session has a prompt")
    });
    cx.simulate_keystrokes("up");
    cx.run_until_parked();
    assert_eq!(draft(&view, cx), newest);
    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    assert_eq!(draft(&view, cx), "");
    cx.simulate_input("typed");
    cx.simulate_keystrokes("up");
    cx.run_until_parked();
    assert_eq!(draft(&view, cx), "typed", "a draft of one's own is not replaced");
}

/// The model in the foot opens the models `/model` takes; a pick types `/model <alias>`.
#[gpui::test]
fn the_model_picker_types_the_model_command(cx: &mut TestAppContext) {
    let (view, cx) = face(cx, "tools");
    let seen = events(&view, cx);
    click(cx, "composer-model");
    assert_eq!(menu_names(&view, cx), Some(vec!["models".to_owned()]));
    click(cx, "composer-menu-1");
    assert_eq!(
        *seen.borrow(),
        [FaceEvent::Submit { text: "/model sonnet".to_owned(), paths: Vec::new() }]
    );
}

/// A prompt's Rewind hands the choice to the terminal.
#[gpui::test]
fn rewind_hands_over_to_the_terminal(cx: &mut TestAppContext) {
    let (view, cx) = face(cx, "tools");
    let seen = events(&view, cx);
    let (row, id) = view
        .read_with(cx, |v, _| {
            v.rows().iter().enumerate().find_map(|(ix, r)| match r {
                crate::conversation::rows::Row::Prompt { id } => Some((ix, id.clone())),
                _ => None,
            })
        })
        .expect("a prompt row");
    super::show_row(&view, cx, row);
    let prompt = cx.debug_bounds(super::leak(format!("prompt-{id}"))).expect("the prompt");
    cx.simulate_mouse_move(prompt.center(), None, Modifiers::default());
    cx.run_until_parked();
    click(cx, super::leak(format!("rewind-{id}")));
    assert_eq!(*seen.borrow(), [FaceEvent::Rewind]);
}

/// With the worker out of reach the composer says so and keeps the draft: Enter sends
/// nothing.
#[gpui::test]
fn an_unreachable_worker_keeps_the_draft(cx: &mut TestAppContext) {
    let (view, cx) = face(cx, "tools");
    let seen = events(&view, cx);
    view.update(cx, |v, cx| v.set_away(Some("studio".to_owned()), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("composer-away").is_some());
    cx.simulate_input("keep me");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(seen.borrow().iter().all(|e| !matches!(e, FaceEvent::Submit { .. })));
    assert_eq!(draft(&view, cx), "keep me");

    let at = cx.debug_bounds("composer-reconnect").expect("a way to dial it now");
    cx.simulate_click(at.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(seen.borrow().last(), Some(&FaceEvent::Reconnect));
    assert_eq!(draft(&view, cx), "keep me", "the draft stays");
}

/// The foot's mode chip says the freshest word on the mode: a hook's, with a permission prompt
/// asked after the last prompt was sent, outlives the prompt; one older than the transcript's
/// gives way to it.
#[gpui::test]
fn the_mode_chip_takes_the_freshest_word(cx: &mut TestAppContext) {
    let (view, cx) = face(cx, "tools");
    let mode = |view: &Entity<ConversationView>, cx: &VisualTestContext| {
        view.read_with(cx, |v, _| v.permission_mode())
    };
    let sent = mode(&view, cx);
    let started = view
        .read_with(cx, |v, _| {
            v.model().thread(&ThreadId::Main).and_then(|t| t.last_turn()).map(|t| t.started_ms)
        })
        .expect("a turn");
    assert!(cx.debug_bounds("composer-mode").is_some(), "the chip is drawn");

    let mut stale = question_prompt(&view, cx);
    stale.mode = Some("bypassPermissions".to_owned());
    stale.asked_ms = WallMs::ZERO;
    ask(&view, cx, stale);
    assert_eq!(mode(&view, cx), sent, "a word older than the prompt gives way");

    let mut fresh = question_prompt(&view, cx);
    fresh.ask = 6;
    fresh.mode = Some("plan".to_owned());
    fresh.asked_ms = WallMs::from_millis(started.as_millis().saturating_add(1));
    ask(&view, cx, fresh);
    let session = view.read_with(cx, |v, _| v.session());
    for ask in [5, 6] {
        view.update(cx, |v, cx| {
            v.permission(
                PermissionEvent::Settled { session, ask, outcome: Settled::Withdrawn },
                None,
                cx,
            );
        });
    }
    cx.run_until_parked();
    assert_eq!(mode(&view, cx), "plan", "the hook's word outlives its prompt");
    assert!(cx.debug_bounds("composer-mode").is_some(), "the composer is back");
}

/// The mode the worker heard the agent switch to (Shift-Tab in the TUI) is the chip's word
/// once it is fresher than the last prompt's; one heard before that prompt gives way to it.
#[gpui::test]
fn the_mode_chip_follows_the_live_mode(cx: &mut TestAppContext) {
    let (view, cx) = face(cx, "tools");
    let mode = |view: &Entity<ConversationView>, cx: &VisualTestContext| {
        view.read_with(cx, |v, _| v.permission_mode())
    };
    let sent = mode(&view, cx);
    let started = view
        .read_with(cx, |v, _| {
            v.model().thread(&ThreadId::Main).and_then(|t| t.last_turn()).map(|t| t.started_ms)
        })
        .expect("a turn");
    let session = view.read_with(cx, |v, _| v.session());
    let agent = |name: &str, heard_ms: WallMs| AgentEvent {
        session,
        kind: AgentKind::ClaudeCode,
        status: AgentStatus::Idle,
        agent_session: None,
        detail: None,
        attention: false,
        source: AgentSource::Hook,
        since_ms: WallMs::ZERO,
        mode: Some(HeardMode { name: name.to_owned(), heard_ms }),
    };

    let stale = agent("bypassPermissions", WallMs::ZERO);
    view.update(cx, |v, cx| v.set_agent(Some(stale), cx));
    cx.run_until_parked();
    assert_eq!(mode(&view, cx), sent, "a switch before the prompt gives way");

    let switched = agent("acceptEdits", WallMs::from_millis(started.as_millis().saturating_add(1)));
    view.update(cx, |v, cx| v.set_agent(Some(switched), cx));
    cx.run_until_parked();
    assert_eq!(mode(&view, cx), "acceptEdits", "the live mode, heard since");
    assert!(cx.debug_bounds("composer-mode").is_some(), "the chip is drawn");
}
