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
    let choice = |id: &str, label: &str| ModeChoice { id: id.into(), label: label.into(), description: None };
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
    assert_eq!(failure.as_deref(), Some("Couldn't stop it: engine not connected"));
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

/// The footer says the mode, the background counts and the goal; a mode switch that cannot go
/// goes back to the mode the agent is in.
#[gpui::test]
fn the_footer_says_mode_background_and_goal(cx: &mut TestAppContext) {
    let dock = ChatDock {
        mode: Some(modes("default")),
        background: WorkCounts { commands: 2, ..WorkCounts::default() },
        background_words: "2 commands".into(),
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
    assert!(find(&nodes, Role::Button, "2 commands").is_some());
    let goal = find(&nodes, Role::Button, "/goal active (5h)").expect("the goal shows");
    assert!(goal.description.contains("all tests pass"));

    let cycled = composer.update(cx, |composer, cx| composer.cycle_mode(cx));
    assert!(cycled, "⇧Tab takes the next mode");
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Button, "Mode: Default").is_some(), "a switch that failed went back");
    let failure = composer.read_with(cx, |composer, _| composer.failure().cloned());
    assert_eq!(failure.as_deref(), Some("Couldn't switch the mode: engine not connected"));
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
    composer.update(cx, |composer, cx| composer.send_form(cx));
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Alert, "Fill in Name").is_some());
    assert!(composer.read_with(cx, |composer, _| composer.wizard_open()), "nothing was sent");
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

/// The agent's suggestion shows on an empty draft and Tab takes it; with no turn idle it hides.
#[gpui::test]
fn tab_takes_the_suggestion(cx: &mut TestAppContext) {
    let dock = ChatDock { suggestion: Some("Run the tests".into()), can: can(), ..ChatDock::default() };
    let (composer, _, cx) = open(cx, dock, None);
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Button, "Suggested: Run the tests").is_some());
    assert!(composer.update(cx, |composer, cx| composer.take_suggestion(cx)));
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Button, "Suggested: Run the tests").is_none(), "taken, it goes");
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
    composer.update(cx, |composer, cx| {
        composer.dock.dock = Some(Arc::new(ChatDock::default()));
        cx.notify();
    });
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Status, "Shell mode").is_none(), "no shell, no shell mode");
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
