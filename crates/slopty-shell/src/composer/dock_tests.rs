// slopty: Slopty's own tests (not in zeron).
//! slopty: the dock around the composer as assistive tech reads it: the Background tray, the
//! footer, a request's card by its kind, the `@` agents, the suggestion, `!` mode, the queue's
//! pause, a mod's band. Each test draws a real composer of chat "chat" with no engine, its dock
//! set as the view model would send it, and judges by the accessibility tree; a call that cannot
//! go (no engine) is the failure each one also proves.

use std::sync::Arc;

use gpui::accesskit::NodeId;
use gpui::{TestAppContext, VisualTestContext};
use slopty_view_doc::dock::{ChatDock, DockCan, GoalLine, ModSurface, ModeChip, ModeChoice, SubagentKindRow};
use slopty_view_doc::work::{MainRow, WorkCounts, WorkRow, WorkTray};
use slopty_view_doc::{MessageStatus, QueueDeliveryGate, QueuedMessage};

use super::*;

struct Host(Entity<Composer>);

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().flex().flex_col().justify_end().child(self.0.clone())
    }
}

/// A node of the last frame's accessibility tree.
#[derive(Debug, Clone)]
struct Node {
    role: Role,
    label: String,
    value: String,
    description: String,
    selected: Option<bool>,
    expanded: Option<bool>,
    y: f32,
}

fn tree(window: &Window) -> Vec<Node> {
    let Some(update) = window.a11y_tree() else { return Vec::new() };
    let scale = f64::from(window.scale_factor());
    let mut nodes: Vec<(NodeId, Node)> = update
        .nodes
        .iter()
        .map(|(id, node)| {
            let r = node.bounds().unwrap_or_default();
            (
                *id,
                Node {
                    role: node.role(),
                    label: node.label().unwrap_or_default().to_owned(),
                    value: node.value().unwrap_or_default().to_owned(),
                    description: node.description().unwrap_or_default().to_owned(),
                    selected: node.is_selected(),
                    expanded: node.is_expanded(),
                    y: (r.y0 / scale) as f32,
                },
            )
        })
        .collect();
    nodes.sort_by(|a, b| a.1.y.total_cmp(&b.1.y));
    nodes.into_iter().map(|(_, node)| node).collect()
}

fn find<'a>(nodes: &'a [Node], role: Role, label: &str) -> Option<&'a Node> {
    nodes.iter().find(|node| node.role == role && node.label == label)
}

fn labels(nodes: &[Node], role: Role) -> Vec<String> {
    nodes.iter().filter(|node| node.role == role).map(|node| node.label.clone()).collect()
}

fn draw(cx: &mut VisualTestContext) -> Vec<Node> {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.set_a11y_active(true);
        window.refresh();
        window.draw(cx).clear(cx);
        tree(window)
    })
}

fn can() -> DockCan {
    DockCan { stop_work: true, shell: true, surfaces: true, set_mode: true, ..DockCan::default() }
}

fn modes(id: &str) -> ModeChip {
    let choice = |id: &str, label: &str| ModeChoice { id: id.into(), label: label.into(), description: None, recommended: false };
    ModeChip {
        id: id.into(),
        label: if id == "plan" { "Plan".into() } else { "Default".into() },
        words: String::new(),
        tone: if id == "plan" { ModeChip::PLAN.into() } else { ModeChip::QUIET.into() },
        choices: vec![choice("default", "Default"), choice("plan", "Plan"), choice("bypassPermissions", "Bypass")],
    }
}

fn work_row(id: &str, kind: &str, live: bool) -> WorkRow {
    WorkRow {
        id: id.into(),
        kind: kind.into(),
        noun: kind.into(),
        title: format!("{kind} {id}"),
        agent_type: None,
        model: None,
        state: if live { "running".into() } else { "completed".into() },
        state_words: if live { "running".into() } else { "done".into() },
        live,
        wakes: false,
        awaited: false,
        ambient: false,
        needs_you: None,
        step: None,
        tokens: None,
        tool_uses: None,
        duration_ms: Some(3_000),
        figures: String::new(),
        started_at: chrono::Utc::now().timestamp_millis(),
        ended_at: None,
        outcome: None,
        output: None,
        timeout_ms: None,
        persistent: false,
        call_part_id: None,
        chat_id: None,
        parent: None,
        depth: 0,
        can_stop: live,
    }
}

/// A composer of chat "chat" drawn with `dock` and `tray` as the view model sent them.
fn open(
    cx: &mut TestAppContext,
    dock: ChatDock,
    tray: Option<WorkTray>,
) -> (Entity<Composer>, Entity<AppState>, &mut VisualTestContext) {
    let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
        crate::app_menus::init(cx);
        crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
        cx.set_reduce_motion(true);
    });
    let state = cx.new(|_| AppState::new());
    state.update(cx, |state, _| {
        state.chats = vec![
            serde_json::from_value(serde_json::json!({
                "id": "chat", "title": "Chat", "deviceId": "dev", "archived": false,
                "createdAt": chrono::Utc::now(), "cwd": "/work/app",
            }))
            .unwrap(),
        ];
        state.selected_chat = Some("chat".into());
    });
    let composer = cx.new(|cx| Composer::new(state.clone(), cx));
    composer.update(cx, |composer, _| {
        composer.dock.chat = Some("chat".into());
        composer.dock.dock = Some(Arc::new(dock));
        composer.dock.tray = tray.map(Arc::new);
    });
    let (_, cx) = cx.add_window_view(|_, _| Host(composer.clone()));
    (composer, state, cx)
}

/// A question of the chat's, open in the composer.
fn ask(cx: &mut VisualTestContext, state: &Entity<AppState>, question: UserInputQuestion) {
    state.update(cx, |state, cx| {
        state.transcript = vec![SessionMessageEntry {
            id: "answer:1".into(),
            role: MessageRole::Assistant,
            parts: vec![MessagePart::Input {
                id: "ask:1".into(),
                request_id: "1".into(),
                questions: vec![question],
                resolved: false,
            }],
            created_at: 0,
            device_id: "dev".into(),
            status: Some(MessageStatus::Streaming),
            continuation_of: None,
            duration_ms: None,
        }];
        state.transcript_revision += 1;
        cx.notify();
    });
}

/// The tray lists what needs the person first, then what runs, and opens by itself for it; a
/// stop that cannot go takes its "stopping" back and says why.
#[gpui::test]
fn the_tray_puts_what_needs_you_first_and_a_failed_stop_says_why(cx: &mut TestAppContext) {
    let mut asks = work_row("chat:child", WorkRow::SUBAGENT, true);
    asks.needs_you = Some("Allow cargo test?".into());
    let tray = WorkTray {
        main: MainRow { working: true, step: Some("Reading files".into()), ..MainRow::default() },
        rows: vec![work_row("task:a", WorkRow::COMMAND, true), asks],
        counts: WorkCounts { commands: 1, subagents: 1, needs_you: 1, ..WorkCounts::default() },
        summary: "1 command · 1 subagent".into(),
        can_stop_all: true,
    };
    let (composer, _, cx) = open(cx, ChatDock { can: can(), ..ChatDock::default() }, Some(tray));
    let nodes = draw(cx);
    let toggle = find(&nodes, Role::Button, "Collapse background work").expect("the tray opened by itself");
    assert_eq!(toggle.expanded, Some(true));
    let rows: Vec<String> = labels(&nodes, Role::ListItem)
        .into_iter()
        .filter(|label| label.starts_with("main") || label.starts_with("subagent") || label.starts_with("command"))
        .collect();
    assert!(rows[0].starts_with("main · Reading files"), "{rows:?}");
    assert!(rows[1].starts_with("subagent") && rows[1].contains("Allow cargo test?"), "{rows:?}");
    assert!(rows[2].starts_with("command"), "{rows:?}");

    composer.update(cx, |composer, cx| composer.stop_row("task:a".into(), cx));
    let nodes = draw(cx);
    assert!(
        labels(&nodes, Role::ListItem).iter().all(|label| !label.contains("stopping")),
        "a stop that never left is not shown as under way"
    );
    let failure = composer.read_with(cx, |composer, _| composer.failure().cloned());
    assert_eq!(failure.as_deref(), Some("Couldn't stop it while Slopty is offline. Try again once it reconnects."));
}

/// A reveal from the transcript opens the tray at its row, an ended one included.
#[gpui::test]
fn a_reveal_opens_the_tray_at_its_row(cx: &mut TestAppContext) {
    let tray = WorkTray {
        main: MainRow::default(),
        rows: vec![work_row("task:a", WorkRow::COMMAND, true), work_row("task:b", WorkRow::MONITOR, false)],
        counts: WorkCounts { commands: 1, ..WorkCounts::default() },
        summary: "1 command".into(),
        can_stop_all: true,
    };
    let (_, _, cx) = open(cx, ChatDock { can: can(), ..ChatDock::default() }, Some(tray));
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Button, "Expand background work").is_some(), "closed until asked");
    cx.update(|_, cx| crate::transcript::reveal_work("chat", "task:b", cx));
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Button, "Collapse background work").is_some());
    let marked = nodes
        .iter()
        .find(|node| node.role == Role::ListItem && node.label.starts_with("monitor"))
        .expect("the ended row shows");
    assert_eq!(marked.selected, Some(true));
}

/// The footer says the mode and the goal (the tray above it counts the background work); a
/// mode switch that cannot go goes back to the mode the agent is in.
#[gpui::test]
fn the_footer_says_mode_and_goal(cx: &mut TestAppContext) {
    let dock = ChatDock {
        mode: Some(modes("default")),
        goal: Some(GoalLine {
            condition: "all tests pass".into(),
            set_at: chrono::Utc::now().timestamp_millis() - 5 * 3_600_000,
            ..GoalLine::default()
        }),
        can: can(),
        ..ChatDock::default()
    };
    let (composer, _, cx) = open(cx, dock, None);
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Button, "Mode: Default").is_some(), "{:?}", labels(&nodes, Role::Button));
    let goal = find(&nodes, Role::Button, "/goal active (5h)").expect("the goal shows");
    assert!(goal.description.contains("all tests pass"));

    let cycled = composer.update(cx, |composer, cx| composer.cycle_mode(cx));
    assert!(cycled, "⇧Tab takes the next mode");
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Button, "Mode: Default").is_some(), "a switch that failed went back");
    let failure = composer.read_with(cx, |composer, _| composer.failure().cloned());
    assert_eq!(failure.as_deref(), Some("Couldn't switch the mode while Slopty is offline. Try again once it reconnects."));
}

/// The mode menu takes the keyboard as the shared menu does: opened from it, the current mode
/// is marked; ↑ ↓, letters, Home and End move the mark; Esc gives the keyboard back to the
/// composer; ↩ chooses on its release.
#[gpui::test]
fn the_mode_menu_walks_by_keys(cx: &mut TestAppContext) {
    let dock = ChatDock { mode: Some(modes("default")), can: can(), ..ChatDock::default() };
    let (composer, _, cx) = open(cx, dock, None);
    draw(cx);
    let key = |cx: &mut VisualTestContext, key: &str| {
        cx.update(|window, cx| {
            for event in [
                gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                    keystroke: gpui::Keystroke::parse(key).unwrap(),
                    is_held: false,
                    prefer_character_input: false,
                }),
                gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke: gpui::Keystroke::parse(key).unwrap() }),
            ] {
                window.dispatch_event(event, cx);
            }
        });
        cx.run_until_parked();
    };
    let at = |cx: &mut VisualTestContext| composer.read_with(cx, |composer, _| composer.footer.keys.at);
    composer.update_in(cx, |composer, window, cx| composer.toggle_footer_menu(footer::FooterMenu::Mode, true, window, cx));
    let nodes = draw(cx);
    assert_eq!(labels(&nodes, Role::MenuItemRadio), ["Default", "Plan", "Bypass"]);
    assert_eq!(at(cx), Some(0), "opened from the keyboard on the current mode");
    key(cx, "down");
    assert_eq!(at(cx), Some(1));
    key(cx, "b");
    assert_eq!(at(cx), Some(2), "a letter goes to the row it starts");
    key(cx, "home");
    assert_eq!(at(cx), Some(0));
    key(cx, "end");
    assert_eq!(at(cx), Some(2));
    key(cx, "escape");
    let open = composer.read_with(cx, |composer, _| composer.footer.menu.as_open().is_some());
    assert!(!open, "Esc closes it");
    let back = composer.update_in(cx, |composer, window, cx| composer.input.focus_handle(cx).is_focused(window));
    assert!(back, "the keyboard is the composer's again");

    composer.update_in(cx, |composer, window, cx| composer.toggle_footer_menu(footer::FooterMenu::Mode, true, window, cx));
    draw(cx);
    key(cx, "p");
    assert_eq!(at(cx), Some(1));
    key(cx, "enter");
    let open = composer.read_with(cx, |composer, _| composer.footer.menu.as_open().is_some());
    assert!(!open, "↩ chose and closed it");
    let failure = composer.read_with(cx, |composer, _| composer.failure().cloned());
    assert_eq!(failure.as_deref(), Some("Couldn't switch the mode while Slopty is offline. Try again once it reconnects."));
}

/// A question says what each option means and who sent it, and folds to a strip that gives
/// the composer back.
#[gpui::test]
fn a_question_shows_its_options_meaning_and_folds(cx: &mut TestAppContext) {
    let (composer, state, cx) = open(cx, ChatDock { can: can(), ..ChatDock::default() }, None);
    ask(
        cx,
        &state,
        UserInputQuestion {
            id: "0".into(),
            header: "Database".into(),
            question: "Which one?".into(),
            options: vec!["Postgres".into(), "SQLite".into()],
            option_descriptions: vec!["Runs as a server".into(), String::new()],
            kind: "question".into(),
            sent_by: Some("reviewer".into()),
            reason: Some("Permission rule Bash(rm:*) requires confirmation".into()),
            until_ms: Some(chrono::Utc::now().timestamp_millis() + 60_000),
            ..UserInputQuestion::default()
        },
    );
    let nodes = draw(cx);
    let postgres = find(&nodes, Role::Button, "Postgres").expect("the option shows");
    assert_eq!(postgres.description, "Runs as a server");
    assert_eq!(find(&nodes, Role::Button, "SQLite").map(|n| n.description.as_str()), Some(""));
    assert!(find(&nodes, Role::Note, "Sent by reviewer").is_some());
    assert!(find(&nodes, Role::Note, "Permission rule Bash(rm:*) requires confirmation").is_some());
    assert!(nodes.iter().any(|n| n.role == Role::Timer && n.label.starts_with("Declines by itself in")));
    assert!(find(&nodes, Role::Button, "Skip").is_some(), "an agent's questions may be skipped");

    composer.update(cx, |composer, cx| composer.fold_question(cx));
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Button, "Asks: reviewer · Database: Which one?").is_some());
    assert!(nodes.iter().all(|n| n.label != "Postgres"), "the panel folded away");
    assert!(composer.read_with(cx, |composer, _| !composer.wizard_open()));
}

/// A form is drawn from its schema; sent with a required field empty, it says so and stays.
#[gpui::test]
fn a_form_says_what_it_still_needs(cx: &mut TestAppContext) {
    let (composer, state, cx) = open(cx, ChatDock { can: can(), ..ChatDock::default() }, None);
    ask(
        cx,
        &state,
        UserInputQuestion {
            id: "choice".into(),
            header: "github".into(),
            question: "Fill in the token's name".into(),
            options: vec!["Decline".into(), "Cancel".into()],
            choices_only: true,
            kind: "elicitation".into(),
            schema_json: Some(
                r#"{"type":"object","properties":{"name":{"type":"string","title":"Name"},"ok":{"type":"boolean"}},"required":["name"]}"#
                    .into(),
            ),
            ..UserInputQuestion::default()
        },
    );
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Form, "Form").is_some());
    assert!(find(&nodes, Role::Group, "Name *").is_some());
    assert!(find(&nodes, Role::RadioButton, "Yes").is_some());
    assert!(find(&nodes, Role::Button, "Skip").is_none(), "a form is no agent's question to skip");
    composer.update(cx, |composer, cx| composer.send_form(cx));
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Alert, "Fill in Name").is_some());
    assert!(composer.read_with(cx, |composer, _| composer.wizard_open()), "nothing was sent");
}

/// Typing in a form's field fills it and answers nothing, long after the card came: no digit
/// picks Decline or Cancel, and no Enter takes one. The form's answers are buttons beside its
/// Submit, unnumbered, and Enter in a field is that Submit, which says what is still missing.
#[gpui::test]
fn typing_in_a_form_field_answers_nothing(cx: &mut TestAppContext) {
    let (composer, state, cx) = open(cx, ChatDock { can: can(), ..ChatDock::default() }, None);
    ask(
        cx,
        &state,
        UserInputQuestion {
            id: "choice".into(),
            header: "tracker".into(),
            question: "Which ticket?".into(),
            options: vec!["Decline".into(), "Cancel".into()],
            choices_only: true,
            kind: "elicitation".into(),
            schema_json: Some(
                r#"{"type":"object","properties":{"ticket":{"type":"string","title":"Ticket"},"note":{"type":"string","minLength":3}},"required":["ticket","note"]}"#
                    .into(),
            ),
            ..UserInputQuestion::default()
        },
    );
    let nodes = draw(cx);
    let buttons = labels(&nodes, Role::Button);
    let at = |label: &str| buttons.iter().position(|b| b == label).unwrap_or_else(|| panic!("{label} in {buttons:?}"));
    assert!(at("Decline") < at("Cancel") && at("Cancel") < at("Submit form"), "{buttons:?}");
    assert!(nodes.iter().all(|n| n.label != "1" && n.label != "2"), "no numbered answers");
    // The card has been up a while and the person has not typed in the composer.
    composer.update(cx, |composer, _| {
        composer.card.guard = request_card::KeyGuard { typed_at: None, shown_at: Some(Instant::now() - Duration::from_secs(10)) };
    });
    let field = composer.read_with(cx, |composer, _| composer.card_input("ticket")).expect("the Ticket field");
    cx.update(|window, cx| {
        let focus = field.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
    });
    draw(cx);
    cx.simulate_input("SLO-42");
    cx.simulate_keystrokes("up down");
    assert_eq!(field.read_with(cx, |field, _| field.text().to_owned()), "SLO-42", "the field holds what was typed");
    assert!(composer.read_with(cx, |composer, _| composer.wizard_open() && composer.answered_requests.is_empty()));
    cx.simulate_keystrokes("enter");
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Alert, "Fill in note").is_some(), "Enter is the form's Submit");
    assert!(composer.read_with(cx, |composer, _| composer.wizard_open() && composer.answered_requests.is_empty()));
}

/// A card that comes while the composer holds the keyboard takes it: its number keys pick at
/// once (once the typing guard lets them), and a letter typed goes to its answer box, which then
/// keeps every key, digits included. An approval's options take their numbers alike.
#[gpui::test]
fn a_card_that_comes_takes_the_keyboard(cx: &mut TestAppContext) {
    let (composer, state, cx) = open(cx, ChatDock { can: can(), ..ChatDock::default() }, None);
    draw(cx);
    let input = composer.read_with(cx, |composer, _| composer.input.clone());
    cx.update(|window, cx| {
        let focus = input.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
    });
    draw(cx);
    let settled = |composer: &Entity<Composer>, cx: &mut VisualTestContext| {
        composer.update(cx, |composer, _| {
            composer.card.guard =
                request_card::KeyGuard { typed_at: None, shown_at: Some(Instant::now() - Duration::from_secs(10)) };
        });
    };
    ask(
        cx,
        &state,
        UserInputQuestion {
            id: "db".into(),
            header: "Database".into(),
            question: "Which one?".into(),
            options: vec!["Postgres".into(), "SQLite".into()],
            kind: "question".into(),
            ..UserInputQuestion::default()
        },
    );
    draw(cx);
    let focused_input = |cx: &mut VisualTestContext| cx.update(|window, cx| input.read(cx).focus_handle.is_focused(window));
    assert!(!focused_input(cx), "the card took the keyboard");
    settled(&composer, cx);
    cx.simulate_input("x");
    assert!(focused_input(cx), "words went to the answer box");
    cx.simulate_input("2");
    assert_eq!(input.read_with(cx, |input, _| input.text().to_owned()), "x2", "the box keeps its digits");
    assert!(composer.read_with(cx, |composer, _| composer.wizard.as_ref().is_some_and(|w| !w.page_has_pick())));
    let card = composer.read_with(cx, |composer, _| composer.wizard_focus.clone());
    cx.update(|window, cx| window.focus(&card, cx));
    draw(cx);
    // Typed just now, a digit is held back and goes to the box; once the typing settled, it picks.
    cx.simulate_input("3");
    assert_eq!(input.read_with(cx, |input, _| input.text().to_owned()), "x23", "held while typing");
    cx.update(|window, cx| window.focus(&card, cx));
    draw(cx);
    settled(&composer, cx);
    cx.simulate_input("1");
    assert!(composer.read_with(cx, |composer, _| composer.wizard.as_ref().is_some_and(|w| w.is_picked(0))), "1 picked");
}

/// An approval that came while the composer held the keyboard answers to its numbers too.
#[gpui::test]
fn an_approvals_numbers_answer_it(cx: &mut TestAppContext) {
    let (composer, state, cx) = open(cx, ChatDock { can: can(), ..ChatDock::default() }, None);
    draw(cx);
    let input = composer.read_with(cx, |composer, _| composer.input.clone());
    cx.update(|window, cx| {
        let focus = input.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
    });
    draw(cx);
    ask(
        cx,
        &state,
        UserInputQuestion {
            id: "choice".into(),
            header: "Read".into(),
            question: "Read src/main.rs?".into(),
            options: vec!["Allow".into(), "Deny".into()],
            choices_only: true,
            deny_option: Some(1),
            allow_option: Some(0),
            kind: "approval".into(),
            ..UserInputQuestion::default()
        },
    );
    draw(cx);
    composer.update(cx, |composer, _| {
        composer.card.guard =
            request_card::KeyGuard { typed_at: None, shown_at: Some(Instant::now() - Duration::from_secs(10)) };
    });
    cx.simulate_input("2");
    assert!(composer.read_with(cx, |composer, _| !composer.wizard_open() && composer.answered_requests.contains("1")), "2 denied it");
}

/// A dialog Slopty does not know shows its payload and takes OK or Dismiss, never words.
#[gpui::test]
fn an_unknown_dialog_shows_its_payload(cx: &mut TestAppContext) {
    let (_, state, cx) = open(cx, ChatDock { can: can(), ..ChatDock::default() }, None);
    ask(
        cx,
        &state,
        UserInputQuestion {
            id: "choice".into(),
            header: "Input".into(),
            question: "brand new thing".into(),
            multiline: true,
            kind: "dialog:brand_new_thing".into(),
            schema_json: Some(r#"{"hostName":"api.example.com","retries":2}"#.into()),
            ..UserInputQuestion::default()
        },
    );
    let nodes = draw(cx);
    assert!(find(&nodes, Role::ListItem, "Host name: api.example.com").is_some());
    assert!(find(&nodes, Role::ListItem, "Retries: 2").is_some());
    assert!(find(&nodes, Role::Button, "OK").is_some());
    assert!(find(&nodes, Role::Button, "Dismiss").is_some());
    assert!(nodes.iter().all(|n| n.label != "Submit"), "it takes no words");
}

/// "Revise…" on a plan lets words go instead of a choice; empty words send nothing.
#[gpui::test]
fn revise_sends_words_instead_of_a_choice(cx: &mut TestAppContext) {
    let (composer, state, cx) = open(cx, ChatDock { can: can(), ..ChatDock::default() }, None);
    ask(
        cx,
        &state,
        UserInputQuestion {
            id: "choice".into(),
            header: "Plan".into(),
            question: String::new(),
            options: vec!["Approve".into(), "Keep planning".into()],
            choices_only: true,
            deny_option: Some(1),
            kind: "plan".into(),
            ..UserInputQuestion::default()
        },
    );
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Button, "Revise…").is_some());
    composer.update(cx, |composer, cx| composer.set_revising(true, cx));
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Button, "Revise…").is_none());
    composer.update(cx, |composer, cx| composer.wizard_advance(cx));
    assert!(composer.read_with(cx, |composer, _| composer.wizard_open()), "no words, nothing sent");
    composer.update(cx, |composer, cx| {
        composer.input.update(cx, |input, cx| input.set_text("Split step 2", cx));
        let typed = composer.input.read(cx).text().to_owned();
        let wizard = composer.wizard.as_mut().expect("open");
        wizard.set_typed(typed);
        let answers = wizard.answers();
        assert_eq!(answers[0].labels, ["Split step 2"]);
    });
}

/// `@` offers the session's agents after the files, and takes one as the agent's mention.
#[gpui::test]
fn at_offers_the_sessions_agents(cx: &mut TestAppContext) {
    let dock = ChatDock {
        subagent_kinds: vec![
            SubagentKindRow { name: "reviewer".into(), description: "Reviews diffs".into(), model: Some("opus".into()) },
            SubagentKindRow { name: "explorer".into(), description: "Maps code".into(), model: None },
        ],
        can: can(),
        ..ChatDock::default()
    };
    let (composer, _, cx) = open(cx, dock, None);
    composer.update(cx, |composer, cx| composer.input.update(cx, |input, cx| input.set_text("@rev", cx)));
    let nodes = draw(cx);
    assert!(composer.read_with(cx, |composer, _| composer.mention.token.is_some()), "@rev is a token");
    let row = find(&nodes, Role::ListBoxOption, "Agent reviewer").expect("the agent is offered");
    assert_eq!(row.selected, Some(true), "Enter starts on it");
    assert_eq!(row.description, "Reviews diffs · opus");
    assert!(nodes.iter().all(|n| n.label != "Agent explorer"), "only what the query names");
    composer.update(cx, |composer, cx| {
        assert_eq!(composer.mention.active, Some(0), "the agent is where Enter starts");
        composer.accept_mention(cx);
    });
    let text = composer.read_with(cx, |composer, cx| composer.input.read(cx).text().to_owned());
    assert_eq!(text.trim_end(), "@agent-reviewer");
}

/// The agent's suggestion shows faint in the empty draft, taking no room above it, and Tab takes
/// it; taken, it goes.
#[gpui::test]
fn tab_takes_the_suggestion(cx: &mut TestAppContext) {
    let dock = ChatDock { suggestion: Some("Run the tests".into()), can: can(), ..ChatDock::default() };
    let (composer, _, cx) = open(cx, dock, None);
    draw(cx);
    let hint = composer.read_with(cx, |composer, cx| composer.input.read(cx).hint().cloned());
    assert_eq!(hint.as_deref(), Some("Run the tests  ⇥ Tab"));
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Status, "Run the tests").is_none(), "no line of its own over the draft");
    assert!(composer.update(cx, |composer, cx| composer.take_suggestion(cx)));
    draw(cx);
    let hint = composer.read_with(cx, |composer, cx| composer.input.read(cx).hint().cloned());
    assert_eq!(hint, None, "taken, it goes");
    let text = composer.read_with(cx, |composer, cx| composer.input.read(cx).text().to_owned());
    assert_eq!(text, "Run the tests");
    assert!(!composer.update(cx, |composer, cx| composer.take_suggestion(cx)), "nothing to take now");
}

/// A `!` draft turns the composer to shell mode; without the agent's shell, it stays words.
#[gpui::test]
fn a_bang_draft_is_shell_mode(cx: &mut TestAppContext) {
    let (composer, _, cx) = open(cx, ChatDock { can: can(), ..ChatDock::default() }, None);
    composer.update(cx, |composer, cx| composer.input.update(cx, |input, cx| input.set_text("!ls -la", cx)));
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Status, "Shell mode").is_some());
    // The tag says the mode: the draft is drawn without its `!`, which it still holds.
    let (text, shown) = composer.read_with(cx, |composer, cx| {
        let input = composer.input.read(cx);
        (input.text().to_owned(), input.projection.display.clone())
    });
    assert_eq!((text.as_str(), shown.as_str()), ("!ls -la", "ls -la"));
    composer.update(cx, |composer, cx| {
        composer.dock.dock = Some(Arc::new(ChatDock::default()));
        cx.notify();
    });
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Status, "Shell mode").is_none(), "no shell, no shell mode");
    let shown = composer.read_with(cx, |composer, cx| composer.input.read(cx).projection.display.clone());
    assert_eq!(shown, "!ls -la", "words, drawn as typed");
}

/// Messages a stop held say "Paused" with Resume; one held for another reason does not.
#[gpui::test]
fn a_stop_pauses_the_queue(cx: &mut TestAppContext) {
    let (_, state, cx) = open(cx, ChatDock { can: can(), ..ChatDock::default() }, None);
    let held = |id: &str, reason: &str| QueuedMessage {
        delivery_gate: Some(QueueDeliveryGate::Held { reason: reason.into() }),
        ..QueuedMessage::new(id, format!("message {id}"), "dev")
    };
    state.update(cx, |state, cx| {
        state.queue = vec![held("a", crate::queue::STOPPED_HOLD), held("b", crate::queue::STOPPED_HOLD)];
        cx.notify();
    });
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Status, "Paused · 2 held since you stopped").is_some());
    assert!(find(&nodes, Role::Button, "Resume").is_some());
    state.update(cx, |state, cx| {
        state.queue = vec![held("c", "A draft is in the way")];
        cx.notify();
    });
    let nodes = draw(cx);
    assert!(nodes.iter().all(|n| !n.label.starts_with("Paused")));
}

/// A mod's band draws its tree over the composer, a kind this build does not know by its words.
#[gpui::test]
fn a_mod_band_draws_its_tree(cx: &mut TestAppContext) {
    let dock = ChatDock {
        surfaces: vec![ModSurface {
            id: "deploy".into(),
            kind: ModSurface::BAND.into(),
            plugin: "shipit".into(),
            title: Some("Deploy".into()),
            tree: serde_json::json!({"type": "Row", "children": [
                {"type": "Text", "props": {"text": "Ready to ship"}},
                {"type": "Button", "props": {"id": "go", "label": "Ship it"}},
                {"type": "Gauge", "props": {"label": "72%"}}
            ]}),
            ..ModSurface::default()
        }],
        can: can(),
        ..ChatDock::default()
    };
    let (_, _, cx) = open(cx, dock, None);
    let nodes = draw(cx);
    let band = find(&nodes, Role::Group, "shipit · Deploy").expect("the band shows");
    assert!(band.description.contains("Ready to ship") && band.description.contains("72%"), "{band:?}");
    assert!(find(&nodes, Role::Button, "Ship it").is_some());
}

/// An approval whose agent takes words with its allow offers "Allow, and say what's next…": the
/// pick goes back as the allow with the words beside it. With no previews there is no pane.
#[gpui::test]
fn an_allow_takes_words_beside_it(cx: &mut TestAppContext) {
    let (composer, state, cx) = open(cx, ChatDock { can: can(), ..ChatDock::default() }, None);
    ask(
        cx,
        &state,
        UserInputQuestion {
            id: "choice".into(),
            header: "Bash command".into(),
            question: "cargo test".into(),
            options: vec!["Yes".into(), "No".into()],
            choices_only: true,
            deny_option: Some(1),
            allow_option: Some(0),
            kind: "approval".into(),
            ..UserInputQuestion::default()
        },
    );
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Button, "Allow, and say what's next…").is_some());
    assert!(nodes.iter().all(|n| !n.label.starts_with("Preview of")), "no previews, no pane");
    assert!(find(&nodes, Role::Note, "Needs you at the card").is_none(), "any answer of the person's");
    composer.update(cx, |composer, cx| composer.set_allowing(0, cx));
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Button, "Allow, and say what's next…").is_none());
    composer.update(cx, |composer, cx| {
        composer.input.update(cx, |input, cx| input.set_text("then commit", cx));
        let typed = composer.input.read(cx).text().to_owned();
        let wizard = composer.wizard.as_mut().expect("open");
        wizard.set_typed(typed);
        assert_eq!(wizard.answers()[0].labels, ["Yes", "then commit"]);
    });
}

/// A request only the person at its card answers says so on the card, which still takes their
/// click: "Needs you at the card".
#[gpui::test]
fn a_card_says_it_needs_the_person_at_it(cx: &mut TestAppContext) {
    let (_, state, cx) = open(cx, ChatDock { can: can(), ..ChatDock::default() }, None);
    ask(
        cx,
        &state,
        UserInputQuestion {
            id: "choice".into(),
            header: "Bash command".into(),
            question: "rm -rf build".into(),
            options: vec!["Yes".into(), "No".into()],
            choices_only: true,
            deny_option: Some(1),
            kind: "approval".into(),
            needs_presence: true,
            ..UserInputQuestion::default()
        },
    );
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Note, "Needs you at the card").is_some(), "the card says so");
    assert!(find(&nodes, Role::Button, "Yes").is_some(), "and takes the person's click");
}

/// A question's options show their previews in a pane beside them, the first one's at first.
#[gpui::test]
fn an_option_shows_its_preview_beside_it(cx: &mut TestAppContext) {
    let (_, state, cx) = open(cx, ChatDock { can: can(), ..ChatDock::default() }, None);
    ask(
        cx,
        &state,
        UserInputQuestion {
            id: "0".into(),
            header: "Layout".into(),
            question: "Which layout?".into(),
            options: vec!["Grid".into(), "List".into()],
            option_previews: vec!["[ ][ ]".into(), String::new()],
            kind: "question".into(),
            ..UserInputQuestion::default()
        },
    );
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Region, "Preview of Grid").is_some(), "{:?}", labels(&nodes, Role::Region));
}

/// Keys typed into the composer after another chat comes on screen land in its draft: given
/// the keyboard while drawn from the last frame, the input still takes the window's text.
#[gpui::test]
fn keys_land_in_the_draft_after_a_chat_switch(cx: &mut TestAppContext) {
    let (composer, state, cx) = open(cx, ChatDock { can: can(), ..ChatDock::default() }, None);
    draw(cx);
    // Drawn as a frame is, from the last one where nothing changed.
    let frame = |cx: &mut VisualTestContext| {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    };
    let elsewhere = composer.read_with(cx, |composer, _| composer.preview_focus.clone());
    cx.update(|window, cx| window.focus(&elsewhere, cx));
    frame(cx);
    state.update(cx, |state, cx| {
        let mut other = state.chats[0].clone();
        other.id = "other".into();
        other.title = Some("Other".into());
        state.chats.push(other);
        state.selected_chat = Some("other".into());
        cx.notify();
    });
    frame(cx);
    let input = composer.read_with(cx, |composer, cx| composer.input.focus_handle(cx));
    cx.update(|window, cx| window.focus(&input, cx));
    frame(cx);
    cx.update(|window, cx| {
        for ch in "half a thought".chars() {
            let keystroke = gpui::Keystroke {
                modifiers: gpui::Modifiers::default(),
                key: ch.to_string(),
                key_char: Some(ch.to_string()),
            };
            window.dispatch_keystroke(keystroke, cx);
        }
    });
    let text = composer.read_with(cx, |composer, cx| composer.input.read(cx).text().to_owned());
    assert_eq!(text, "half a thought");
}

/// `/btw` asks the agent beside the conversation and is never sent as a message: the draft
/// empties, and with no engine the composer says the question could not go. The newest exchange
/// shows in a card above the composer, the agent at it and then its answer, until dismissed; a
/// newer one brings it back. An agent that takes no side question gets no card.
#[gpui::test]
fn a_btw_draft_asks_beside_and_its_answer_shows_until_dismissed(cx: &mut TestAppContext) {
    let dock = ChatDock { can: DockCan { side: true, ..can() }, ..ChatDock::default() };
    let (composer, _, cx) = open(cx, dock.clone(), None);
    composer.update(cx, |composer, cx| {
        composer.input.update(cx, |input, cx| input.set_text("/btw which file?", cx));
        composer.on_submit(cx);
    });
    let (draft, failure) =
        composer.read_with(cx, |composer, cx| (composer.input.read(cx).text().to_owned(), composer.visible_failure()));
    assert_eq!(draft, "", "taken as a side question");
    assert!(failure.is_some_and(|f| f.starts_with("Couldn't ask the side question")), "the call went");
    let line = |id: &str, answer: Option<&str>| slopty_view_doc::SideLine {
        id: id.into(),
        question: "Which file?".into(),
        answer: answer.map(Into::into),
        failed: None,
        at: 0,
    };
    let set = |cx: &mut VisualTestContext, side: Vec<slopty_view_doc::SideLine>| {
        let dock = ChatDock { side, ..dock.clone() };
        composer.update(cx, |composer, cx| {
            composer.dock.dock = Some(Arc::new(dock));
            cx.notify();
        });
        draw(cx)
    };
    let nodes = set(cx, vec![line("1", None)]);
    assert!(find(&nodes, Role::Group, "Side question: Which file?").is_some());
    assert!(find(&nodes, Role::Status, "Answering").is_some());
    let nodes = set(cx, vec![line("1", Some("In `codec.rs`."))]);
    assert!(find(&nodes, Role::Group, "Side question: Which file?").is_some());
    assert!(find(&nodes, Role::Status, "Answering").is_none(), "answered");
    assert!(find(&nodes, Role::Button, "Dismiss the side answer").is_some());
    composer.update(cx, |composer, cx| composer.dismiss_side(cx));
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Group, "Side question: Which file?").is_none(), "dismissed");
    let nodes = set(cx, vec![line("1", Some("In `codec.rs`.")), line("2", None)]);
    assert!(find(&nodes, Role::Status, "Answering").is_some(), "the newer one shows");
    let nodes = set(cx, Vec::new());
    assert!(find(&nodes, Role::Group, "Side question: Which file?").is_none());
    composer.update(cx, |composer, cx| {
        composer.dock.dock = Some(Arc::new(ChatDock { can: can(), ..ChatDock::default() }));
        assert!(!composer.ask_side("why?".into(), cx), "no side questions here");
    });
}
