//! The transcript's own rows as assistive tech reads them: a subagent's row, the rows
//! of background work, a reply's images and formulas. Each test draws a real transcript in a
//! test window and judges by its accessibility tree.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::rc::Rc;
use std::sync::Arc;

use gpui::accesskit::{NodeId, Role};
use gpui::prelude::*;
use gpui::{Entity, IntoElement, Pixels, Point, Render, VisualTestContext, Window, div, point, px};
use slopty_view_doc::{
    BlobLink, ExitCode, MessagePart, MessageRole, MessageStatus, NoteRow, SessionMessageEntry,
    SubagentProgress, SubagentStatus, TaskRow, ToolFacts,
};
use slopty_view_proto::ToolCall;

use super::{SubagentDrill, Transcript};
use crate::state::AppState;
use crate::theme::Theme;

struct Host(Entity<Transcript>);

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.0.clone())
    }
}

/// A node of the last frame's accessibility tree.
#[derive(Debug, Clone)]
struct Node {
    role: Role,
    label: String,
    value: String,
    expanded: Option<bool>,
    center: Point<Pixels>,
    height: f32,
    width: f32,
    children: Vec<NodeId>,
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "accessibility bounds are window points, far inside f32's range"
)]
fn tree(window: &Window) -> HashMap<NodeId, Node> {
    let Some(update) = window.a11y_tree() else { return HashMap::new() };
    let scale = f64::from(window.scale_factor());
    update
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
                    expanded: node.is_expanded(),
                    center: point(
                        px((f64::midpoint(r.x0, r.x1) / scale) as f32),
                        px((f64::midpoint(r.y0, r.y1) / scale) as f32),
                    ),
                    height: ((r.y1 - r.y0) / scale) as f32,
                    width: ((r.x1 - r.x0) / scale) as f32,
                    children: node.children().to_vec(),
                },
            )
        })
        .collect()
}

fn find<'a>(nodes: &'a HashMap<NodeId, Node>, role: Role, label: &str) -> Option<&'a Node> {
    nodes.values().find(|node| node.role == role && node.label == label)
}

fn descendants<'a>(nodes: &'a HashMap<NodeId, Node>, of: &Node, role: Role) -> Vec<&'a Node> {
    let mut found = Vec::new();
    let mut stack: Vec<NodeId> = of.children.clone();
    while let Some(id) = stack.pop() {
        let Some(node) = nodes.get(&id) else { continue };
        if node.role == role {
            found.push(node);
        }
        stack.extend(node.children.iter().copied());
    }
    found
}

fn draw(cx: &mut VisualTestContext) -> HashMap<NodeId, Node> {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.set_a11y_active(true);
        window.refresh();
        window.draw(cx).clear(cx);
        tree(window)
    })
}

/// The frames after a press, as the platform delivers them: each frame asked for, then drawn.
fn settle(cx: &mut VisualTestContext) -> HashMap<NodeId, Node> {
    let mut nodes = draw(cx);
    for _ in 0..4 {
        if cx.update(Window::simulate_next_frame) == 0 {
            break;
        }
        nodes = draw(cx);
    }
    nodes
}

fn entry(status: MessageStatus, parts: Vec<MessagePart>) -> SessionMessageEntry {
    SessionMessageEntry {
        id: "reply".into(),
        role: MessageRole::Assistant,
        parts,
        created_at: 0,
        device_id: "dev".into(),
        status: Some(status),
        duration_ms: None,
    }
}

fn text(text: &str) -> MessagePart {
    // Its words name it: a reply's parts each have their own id.
    MessagePart::Text { id: format!("text:{text}"), text: text.into() }
}

/// A transcript of chat "chat" (folder `/work/app`, no engine) showing `reply`, drawn.
fn open(
    cx: &mut gpui::TestAppContext,
    reply: SessionMessageEntry,
) -> (Entity<Transcript>, &mut VisualTestContext) {
    let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
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
        state.transcript_replayed = true;
        state.transcript = vec![reply];
        state.transcript_revision = state.transcript_revision.wrapping_add(1);
    });
    let transcript = cx.new(|cx| Transcript::new(state, cx));
    transcript.update(cx, Transcript::sync);
    let (_, cx) = cx.add_window_view(|_, _| Host(transcript.clone()));
    cx.simulate_resize(gpui::size(px(1000.0), px(800.0)));
    (transcript, cx)
}

fn spawn(progress: SubagentProgress) -> MessagePart {
    MessagePart::Tool {
        id: "spawn".into(),
        call: ToolCall::Unknown {
            name: "Agent: Count lines".into(),
            input: Some(serde_json::json!({ "description": "Count lines" })),
        },
        is_error: false,
        resolved: false,
        output: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: None,
        subagent_ref: Some("child-chat".into()),
        subagent_status: Some(SubagentStatus::Running),
        facts: Some(Box::new(ToolFacts {
            subagent_progress: Some(progress),
            ..ToolFacts::default()
        })),
    }
}

/// The drills `transcript` asks of the inspector from here on, as their runs' chats.
fn drills(
    transcript: &Entity<Transcript>,
    cx: &mut VisualTestContext,
) -> (Rc<RefCell<Vec<String>>>, gpui::Subscription) {
    let drilled = Rc::new(RefCell::new(Vec::new()));
    let events = cx.update(|_, cx| {
        let drilled = Rc::clone(&drilled);
        cx.subscribe(transcript, move |_, event: &SubagentDrill, _| {
            drilled.borrow_mut().push(event.chat_id.clone());
        })
    });
    (drilled, events)
}

/// The words of a run's own steps: "Line {n} of the run.", one entry each.
fn run_line(n: usize) -> SessionMessageEntry {
    SessionMessageEntry {
        id: format!("child:{n}"),
        parts: vec![MessagePart::Text {
            id: format!("child:{n}:t"),
            text: format!("Line {n} of the run."),
        }],
        ..entry(MessageStatus::Complete, Vec::new())
    }
}

/// A subagent's row reads its step and never opens by itself; a press splices its run's steps
/// in under it, a level in, and another takes them out; Enter on it drills in.
#[gpui::test]
fn a_subagents_row_opens_its_steps_in_place_and_drills_in(cx: &mut gpui::TestAppContext) {
    let progress = SubagentProgress {
        agent_type: Some("Explore".into()),
        running: true,
        state: SubagentProgress::RUNNING.into(),
        step: Some("Reading parser.rs".into()),
        tool_uses: Some(12),
        figures: "12 tools".into(),
        chat_id: Some("child-chat".into()),
        row: Some("task:a1".into()),
        ..SubagentProgress::default()
    };
    let (transcript, cx) = open(cx, entry(MessageStatus::Complete, vec![spawn(progress)]));
    let (drilled, _events) = drills(&transcript, cx);
    let state = transcript.read_with(cx, |transcript, _| transcript.state.clone());
    state.update(cx, |state, _| {
        state.set_subagent_snapshot("child-chat".into(), (0..2).map(run_line).collect());
    });
    let nodes = draw(cx);
    let row = find(&nodes, Role::Button, "Subagent Count lines").expect("the row").clone();
    assert!(
        row.value.starts_with("Reading parser.rs · 12 tools"),
        "its step, then its facts: {row:?}"
    );
    assert_eq!(row.expanded, Some(false));
    assert!(row.height < 40.0, "one line: {row:?}");
    assert!(find(&nodes, Role::Paragraph, "Line 0 of the run.").is_none(), "shut until pressed");

    cx.simulate_click(row.center, gpui::Modifiers::default());
    // What opened at the end of the trace comes into view as it lays out.
    let nodes = settle(cx);
    assert_eq!(find(&nodes, Role::Button, "Subagent Count lines").unwrap().expanded, Some(true));
    let step =
        find(&nodes, Role::Paragraph, "Line 1 of the run.").expect("its steps, in place").clone();
    let row = find(&nodes, Role::Button, "Subagent Count lines").unwrap().clone();
    assert!(top(&step) > bottom(&row), "under its row: {step:?}");
    // The parent's reply is the first of the replies shown; its run's own replies are under it.
    let parent = nodes
        .values()
        .filter(|node| node.role == Role::Article && node.label == "Agent")
        .min_by(|a, b| a.center.y.partial_cmp(&b.center.y).unwrap_or(std::cmp::Ordering::Equal))
        .expect("the parent's reply")
        .clone();
    let inset = |node: &Node| f32::from(node.center.x) - node.width / 2.0;
    assert!(
        inset(&step) >= inset(&parent) + super::subagents::NEST_INDENT - 0.5,
        "a level in: {step:?} {parent:?}"
    );
    assert!(drilled.borrow().is_empty(), "a press opens it in place, never drills");

    cx.simulate_click(row.center, gpui::Modifiers::default());
    let nodes = draw(cx);
    assert_eq!(find(&nodes, Role::Button, "Subagent Count lines").unwrap().expanded, Some(false));
    assert!(find(&nodes, Role::Paragraph, "Line 1 of the run.").is_none(), "taken out again");

    while !focused(cx).starts_with("Subagent Count lines") {
        tab(cx);
    }
    press_enter(cx);
    draw(cx);
    assert_eq!(*drilled.borrow(), ["child-chat"], "Enter drills in");
    assert_eq!(
        find(&draw(cx), Role::Button, "Subagent Count lines").unwrap().expanded,
        Some(false),
        "and opens nothing here"
    );
}

/// Subagents started together stand under one line that says how many work, each its
/// own row under it, with no gap between them.
#[gpui::test]
fn subagents_started_together_stand_under_one_line(cx: &mut gpui::TestAppContext) {
    let run = |id: &str, chat: &str, running: bool| {
        let MessagePart::Tool { call, subagent_status, facts, .. } = spawn(SubagentProgress {
            agent_type: Some("Explore".into()),
            running,
            state: if running { SubagentProgress::RUNNING } else { SubagentProgress::DONE }.into(),
            chat_id: Some(chat.into()),
            ..SubagentProgress::default()
        }) else {
            panic!("a spawn part is a tool part")
        };
        MessagePart::Tool {
            id: id.into(),
            call,
            is_error: false,
            resolved: !running,
            output: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            diff_stats: None,
            subagent_ref: Some(chat.into()),
            subagent_status: if running { subagent_status } else { Some(SubagentStatus::Done) },
            facts,
        }
    };
    let (_, cx) = open(
        cx,
        entry(
            MessageStatus::Complete,
            vec![run("a", "chat-a", true), run("b", "chat-b", false), text("Waiting on them.")],
        ),
    );
    let nodes = draw(cx);
    let line = nodes
        .values()
        .find(|node| {
            node.role == Role::Heading && node.label.starts_with("2 Explore agents · 1 working")
        })
        .expect("the batch's line")
        .clone();
    let mut rows: Vec<&Node> = nodes
        .values()
        .filter(|node| node.role == Role::Button && node.label == "Subagent Count lines")
        .collect();
    rows.sort_by(|a, b| top(a).total_cmp(&top(b)));
    assert_eq!(rows.len(), 2, "a row each");
    assert!(top(rows[0]) >= bottom(&line) - 0.5, "under its line");
    assert!((top(rows[1]) - bottom(rows[0])).abs() < 1.0, "line under line: {rows:?}");
}

/// The footer a transcript ends with (the inspector's mirror of the prompt box): a box of its own.
struct Footer;

impl Render for Footer {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().id("footer").role(Role::Group).aria_label("Footer").w_full().h(px(48.0))
    }
}

/// A press on the footer, which is no row, leaves the view following the end, so new steps
/// still come into view while the person looks at the prompt box; a press in a row lets go.
#[gpui::test]
fn a_press_on_the_footer_leaves_the_view_following(cx: &mut gpui::TestAppContext) {
    let parts = (0..60).map(|n| text(&format!("Paragraph {n} of a long reply."))).collect();
    let (transcript, cx) = open(cx, entry(MessageStatus::Complete, parts));
    let footer = cx.update(|_, cx| cx.new(|_| Footer));
    transcript.update(cx, |transcript, cx| transcript.set_footer(Some(footer.into()), cx));
    let following =
        |cx: &mut VisualTestContext| transcript.read_with(cx, |transcript, _| transcript.pinned);
    let nodes = draw(cx);
    assert!(following(cx), "a session opens following its end");
    let at = find(&nodes, Role::Group, "Footer").expect("the footer under the last row").center;
    cx.simulate_click(at, gpui::Modifiers::default());
    let nodes = draw(cx);
    assert!(following(cx), "a press on the footer leaves it following");
    let row = find(&nodes, Role::Paragraph, "Paragraph 59 of a long reply.")
        .expect("the last row")
        .center;
    cx.simulate_click(row, gpui::Modifiers::default());
    draw(cx);
    assert!(!following(cx), "a press in a row lets go");
}

/// A background task's row says its status and names its task on the right unless its words
/// already do.
#[gpui::test]
fn work_rows_name_their_task(cx: &mut gpui::TestAppContext) {
    let task = TaskRow {
        task: "t1".into(),
        row: "task:t1".into(),
        kind: "command".into(),
        status: "completed".into(),
        text: "Background command finished".into(),
        title: "cargo test".into(),
        ..TaskRow::default()
    };
    let named = TaskRow {
        task: "t2".into(),
        row: "task:t2".into(),
        text: "Background command \"Build\" completed (exit code 0)".into(),
        title: "Build".into(),
        ..task.clone()
    };
    let note =
        NoteRow { kind: "hook".into(), text: "Saved 2 memories".into(), ..NoteRow::default() };
    let (_, cx) = open(
        cx,
        entry(
            MessageStatus::Complete,
            vec![
                text("Started the tests."),
                MessagePart::Task { id: "task".into(), task: Box::new(task) },
                MessagePart::Task { id: "named".into(), task: Box::new(named) },
                MessagePart::Note { id: "note".into(), note: Box::new(note) },
            ],
        ),
    );
    let nodes = draw(cx);
    let row = find(&nodes, Role::Status, "Background command finished · cargo test")
        .expect("the task's row")
        .clone();
    assert!(
        find(&nodes, Role::Status, "Background command \"Build\" completed (exit code 0)")
            .is_some(),
        "words that name the task say it once"
    );
    assert_eq!(row.value, "completed");
    let notice = nodes
        .values()
        .find(|node| node.role == Role::Status && node.label.contains("Saved 2 memories"))
        .expect("the note");
    assert_eq!(notice.value, "hook");
}

/// A reply's images: two side by side make a gallery, a file that cannot be read says
/// "missing", one outside the chat's folder is refused unread, a web address is not fetched.
#[gpui::test]
fn markdown_images_say_how_they_stand(cx: &mut gpui::TestAppContext) {
    let (_, cx) = open(
        cx,
        entry(
            MessageStatus::Complete,
            vec![text(
                "Here:\n\n![chart](img/chart.png) ![secret](../secret.png)\n\n\
                 ![web](https://example.com/a.png)",
            )],
        ),
    );
    draw(cx);
    let nodes = draw(cx);
    let gallery = find(&nodes, Role::Group, "2 images").expect("a gallery");
    let mut tiles: Vec<(String, String)> = descendants(&nodes, gallery, Role::Image)
        .into_iter()
        .map(|image| (image.label.clone(), image.value.clone()))
        .collect();
    tiles.sort();
    assert_eq!(
        tiles,
        [("chart".to_owned(), "missing".to_owned()), ("secret".to_owned(), "refused".to_owned())]
    );
    assert_eq!(find(&nodes, Role::Image, "web").map(|node| node.value.as_str()), Some("remote"));
}

/// A formula typesets taller than a line, its math node labelled with its TeX; one zatex cannot
/// set shows its source; a formula still streaming in keeps its text.
#[gpui::test]
fn display_formulas_typeset_once_settled(cx: &mut gpui::TestAppContext) {
    let (transcript, cx) = open(
        cx,
        entry(
            MessageStatus::Complete,
            vec![text("$$\\frac{a}{b}$$\n\nthen\n\n$$\\nosuchcommand{x}$$")],
        ),
    );
    let nodes = draw(cx);
    let formula = find(&nodes, Role::Math, "\\frac{a}{b}").expect("the formula");
    assert!(formula.height > 22.0, "taller than a line: {formula:?}");
    assert!(find(&nodes, Role::Math, "\\nosuchcommand{x}").is_none(), "not typeset");
    assert!(
        nodes.values().any(|node| node.label.contains("\\nosuchcommand{x}")),
        "the broken formula shows its source"
    );

    // A reply still streaming a formula in: its open `$$` draws no formula yet.
    transcript.update(cx, |this, cx| {
        this.state.update(cx, |state, _| {
            state.transcript = vec![entry(MessageStatus::Streaming, vec![text("$$\\frac{a}")])];
            state.transcript_revision = state.transcript_revision.wrapping_add(1);
        });
        this.sync(cx);
    });
    let nodes = draw(cx);
    assert!(nodes.values().all(|node| node.role != Role::Math), "the tail keeps text");
}

/// A failed turn's card offers its error to copy, the agent's words with its detail, and
/// the detail opens in a box that scrolls (T51) and says it to the screen reader.
#[gpui::test]
fn an_error_card_copies_its_error_and_scrolls_its_detail(cx: &mut gpui::TestAppContext) {
    let detail = (1..=40).map(|n| format!("trace line {n}")).collect::<Vec<_>>().join("\n");
    let failed = MessagePart::Error {
        id: "error".into(),
        message: "The turn failed".into(),
        detail: Some(detail.clone()),
    };
    let (_, cx) = open(cx, entry(MessageStatus::Complete, vec![failed]));
    let nodes = draw(cx);
    let copy = find(&nodes, Role::Button, "Copy error").expect("Copy error").clone();
    cx.simulate_click(copy.center, gpui::Modifiers::default());
    draw(cx);
    let copied = cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some(format!("The turn failed\n\n{detail}").as_str()));
    let details = find(&nodes, Role::Button, "Show details").expect("Show details").clone();
    cx.simulate_click(details.center, gpui::Modifiers::default());
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Button, "Hide details").is_some(), "the detail opened");
    assert!(find(&nodes, Role::Paragraph, &detail).is_some(), "the detail says why");
}

/// A subagent opened in place before its run arrived (its run is fetched once it opens) says
/// its steps are on their way on every frame, and shows them in their place once they land.
fn opened_before_its_run(cx: &mut gpui::TestAppContext, running: bool) {
    let progress = SubagentProgress {
        running,
        state: if running { SubagentProgress::RUNNING } else { SubagentProgress::DONE }.into(),
        chat_id: Some("child-chat".into()),
        row: running.then(|| "task:a1".into()),
        ..SubagentProgress::default()
    };
    let (transcript, cx) = open(cx, entry(MessageStatus::Complete, vec![spawn(progress)]));
    let nodes = draw(cx);
    let row = find(&nodes, Role::Button, "Subagent Count lines").expect("the row").clone();
    cx.simulate_click(row.center, gpui::Modifiers::default());
    let loading = |nodes: &HashMap<NodeId, Node>| {
        nodes.values().any(|node| node.label.contains("Loading its steps"))
    };
    for frame in 0..4 {
        let nodes = draw(cx);
        assert!(loading(&nodes), "frame {frame}: on their way");
    }
    let state = transcript.read_with(cx, |transcript, _| transcript.state.clone());
    state.update(cx, |state, cx| {
        state.set_subagent_snapshot("child-chat".into(), vec![run_line(0), run_line(1)]);
        state.transcript_revision = state.transcript_revision.wrapping_add(1);
        cx.notify();
    });
    // Its steps land in view: what the person opened comes into view once its record is in.
    settle(cx);
    let row = find(&draw(cx), Role::Button, "Subagent Count lines").expect("the row").clone();
    for frame in 0..4 {
        let nodes = draw(cx);
        let step = find(&nodes, Role::Paragraph, "Line 1 of the run.");
        assert!(
            step.is_some_and(|step| top(step) > bottom(&row)),
            "its steps in, frame {frame}: {step:?}"
        );
        assert!(!loading(&nodes), "frame {frame}: no longer on their way");
    }
}

#[gpui::test]
fn an_ended_run_opened_before_it_arrives_shows_it_once_in(cx: &mut gpui::TestAppContext) {
    opened_before_its_run(cx, false);
}

#[gpui::test]
fn a_running_run_opened_before_it_arrives_shows_it_once_in(cx: &mut gpui::TestAppContext) {
    opened_before_its_run(cx, true);
}

/// The transcript's words are in the accessibility tree: the person's message is an
/// article "You" holding a paragraph of their words, the reply an article named by its agent
/// ("Agent" when the chat names none) at its first row, and each of the reply's blocks a
/// paragraph of its plain text.
#[gpui::test]
fn a_chats_words_are_articles_and_paragraphs(cx: &mut gpui::TestAppContext) {
    let reply =
        entry(MessageStatus::Complete, vec![text("The ls **failed**.\n\nThen it was done.")]);
    let (transcript, cx) = open(cx, reply.clone());
    let asked = SessionMessageEntry {
        id: "ask".into(),
        role: MessageRole::User,
        parts: vec![MessagePart::Text { id: "ask:t".into(), text: "Survey the files".into() }],
        status: None,
        ..reply.clone()
    };
    // A fork's seam is the app's, not a message the agent said (#8).
    let seam = SessionMessageEntry {
        id: "seam".into(),
        role: MessageRole::System,
        parts: vec![MessagePart::Marker { id: "seam:m".into(), text: "Forked from Survey".into() }],
        status: None,
        ..reply.clone()
    };
    transcript.update(cx, |this, cx| {
        this.state.update(cx, |state, _| {
            state.transcript = vec![seam, asked, reply];
            state.transcript_revision = state.transcript_revision.wrapping_add(1);
        });
        this.sync(cx);
    });
    let nodes = draw(cx);
    let you = find(&nodes, Role::Article, "You").expect("the person's message").clone();
    let said = descendants(&nodes, &you, Role::Paragraph);
    assert!(said.iter().any(|p| p.label == "Survey the files"), "{said:?}");
    let claude = find(&nodes, Role::Article, "Agent").expect("the reply").clone();
    let first = descendants(&nodes, &claude, Role::Paragraph);
    assert!(first.iter().any(|p| p.label == "The ls failed."), "plain text: {first:?}");
    assert!(find(&nodes, Role::Paragraph, "Then it was done.").is_some(), "the second block");
    let articles = nodes.values().filter(|n| n.role == Role::Article).count();
    assert_eq!(articles, 2, "one article a message, none for the seam");
}

/// A link to a chat in a reply is a live chip: it says the chat's title and how it
/// stands as they are now, and says the new ones once they change.
#[gpui::test]
fn a_link_to_a_chat_says_how_it_stands_now(cx: &mut gpui::TestAppContext) {
    let link = crate::links::slopty_conversation_link("chat", "here");
    let written = format!("See [that chat]({link}).");
    let (transcript, cx) = open(cx, entry(MessageStatus::Complete, vec![text(&written)]));
    let shown = |cx: &mut VisualTestContext| {
        transcript.update(cx, |this, cx| {
            let tree = Arc::new(crate::markdown::parse_full(&written));
            let chipped = this.chat_chips(&"row".into(), tree, cx);
            let crate::markdown::Block::Paragraph { runs } = &chipped.blocks[0].block else {
                panic!("a paragraph");
            };
            runs.iter()
                .map(|run| run.style.file_label.clone().unwrap_or_else(|| run.text.clone()))
                .collect::<String>()
        })
    };
    assert_eq!(shown(cx), "See Chat.");
    let state = transcript.read_with(cx, |this, _| this.state.clone());
    state.update(cx, |state, _| {
        state.chats[0].title = Some("Fix the parser".into());
        state.apply_sessions(vec![slopty_view_proto::Session {
            last_completed_turn: None,
            chat_id: "chat".into(),
            device_id: "dev".into(),
            status: slopty_view_proto::SessionStatus::Working,
            started_at: None,
            updated_at: chrono::Utc::now(),
            running_subagents: 0,
            status_since: None,
            waiting_on: None,
        }]);
    });
    assert_eq!(shown(cx), "See Fix the parser · Working.");
    draw(cx);
}

/// A subagent row's quiet Open draws in its 18 pt footprint but takes a press over at
/// least 24 pt (`surface_chrome::press_target`): a press 1.5 pt past its footprint drills in.
#[gpui::test]
fn a_subagents_quiet_open_takes_a_press_past_what_it_draws(cx: &mut gpui::TestAppContext) {
    let progress = SubagentProgress {
        running: false,
        state: SubagentProgress::DONE.into(),
        chat_id: Some("child-chat".into()),
        ..SubagentProgress::default()
    };
    let (transcript, cx) = open(cx, entry(MessageStatus::Complete, vec![spawn(progress)]));
    let (opened, _events) = drills(&transcript, cx);
    let nodes = draw(cx);
    let target = find(&nodes, Role::Button, "Open subagent").expect("the quiet Open").clone();
    assert!(target.height >= 24.0, "pressed over 24 pt: {target:?}");
    let past = point(target.center.x + px(10.5), target.center.y);
    cx.simulate_click(past, gpui::Modifiers::default());
    draw(cx);
    assert_eq!(*opened.borrow(), ["child-chat"], "a press past the 18 pt footprint opens it");
}

/// A click on a subagent's row between two reads of the tree, as the self-test's driver
/// clicks: the frames around it drawn as a person's are (no screen reader, the list scrolled onto
/// its layer), rows under it, its run there to splice in. Once the frames settle, the run is
/// open in place and the frame shown is the one the same state draws from scratch: no row under
/// it is left where it stood before its steps came in.
#[gpui::test]
fn a_subagent_clicked_on_a_persons_frame_opens_on_the_next(cx: &mut gpui::TestAppContext) {
    let progress = SubagentProgress {
        running: false,
        state: SubagentProgress::DONE.into(),
        chat_id: Some("child-chat".into()),
        ..SubagentProgress::default()
    };
    let words = (0..40).fold(String::new(), |mut words, n| {
        let _written = write!(words, "Paragraph {n} of the survey.\n\n");
        words
    });
    let (transcript, cx) = open(
        cx,
        entry(MessageStatus::Complete, vec![text(&words), spawn(progress), text("Counted.")]),
    );
    let state = transcript.read_with(cx, |transcript, _| transcript.state.clone());
    state.update(cx, |state, _| {
        state.set_subagent_snapshot("child-chat".into(), (0..3).map(run_line).collect());
    });
    // A frame as the platform draws one: the frames asked for, then the draw.
    let plain = |cx: &mut VisualTestContext| {
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.set_a11y_active(false);
            window.draw(cx).clear(cx);
        });
    };
    plain(cx);
    for dy in [-30.0, 30.0, -30.0, 30.0, -30.0, 30.0] {
        cx.simulate_event(gpui::ScrollEvent {
            position: point(px(500.0), px(400.0)),
            delta: gpui::ScrollDelta::Pixels(point(px(0.0), px(dy))),
            ..Default::default()
        });
        plain(cx);
    }
    // A dump: the tree from a frame of its own.
    let card = find(&draw(cx), Role::Button, "Subagent Count lines").expect("the row").center;
    plain(cx);
    cx.simulate_event(gpui::MouseMoveEvent { position: card, ..Default::default() });
    cx.simulate_event(gpui::MouseDownEvent {
        position: card,
        button: gpui::MouseButton::Left,
        click_count: 1,
        ..Default::default()
    });
    cx.simulate_event(gpui::MouseUpEvent {
        position: card,
        button: gpui::MouseButton::Left,
        click_count: 1,
        ..Default::default()
    });
    // The frames settle: its steps come in under it and lay out.
    for _ in 0..4 {
        plain(cx);
    }
    let painted = |window: &Window| {
        let mut lines = window.painted_primitives();
        lines.sort_unstable();
        lines
    };
    let (shown, scratch) = cx.update(|window, cx| {
        let shown = painted(window);
        window.refresh();
        window.draw(cx).clear(cx);
        (shown, painted(window))
    });
    let missed: Vec<_> = scratch.iter().filter(|line| !shown.contains(line)).take(8).collect();
    let extra: Vec<_> = shown.iter().filter(|line| !scratch.contains(line)).take(8).collect();
    assert!(
        missed.is_empty() && extra.is_empty(),
        "stale once open: missed {missed:#?}, extra {extra:#?}"
    );
    let nodes = draw(cx);
    assert_eq!(
        find(&nodes, Role::Button, "Subagent Count lines").and_then(|c| c.expanded),
        Some(true)
    );
    assert!(find(&nodes, Role::Paragraph, "Line 2 of the run.").is_some(), "its steps, in place");
}

/// What runs reads the transcript's clock, taken on its tick and when its state moves,
/// never as a frame is built: a running subagent's row says the time the clock says, and a
/// frame built again from scratch later says the same.
#[gpui::test]
fn a_running_subagent_says_the_clocks_time_in_every_frame(cx: &mut gpui::TestAppContext) {
    let started = 1_700_000_000_000_i64;
    let progress = SubagentProgress {
        running: true,
        state: SubagentProgress::RUNNING.into(),
        chat_id: Some("child-chat".into()),
        started_at: Some(started),
        ..SubagentProgress::default()
    };
    let (transcript, cx) = open(cx, entry(MessageStatus::Complete, vec![spawn(progress)]));
    transcript.update(cx, |this, cx| {
        this.clock = chrono::DateTime::from_timestamp_millis(started + 42_000).unwrap_or_default();
        cx.notify();
    });
    let value = |cx: &mut VisualTestContext| {
        find(&draw(cx), Role::Button, "Subagent Count lines").map(|row| row.value.clone())
    };
    let first = value(cx).expect("the row");
    assert!(first.contains("42s"), "{first}");
    assert_eq!(value(cx).as_deref(), Some(first.as_str()), "drawn again, the same second");
}

/// On a touch screen a reply's controls take a finger's 44 pt press (#23): its tool
/// group's summary row and the strip's Copy; on a Mac each is its drawn size.
#[gpui::test]
fn the_controls_of_a_reply_take_a_fingers_press(cx: &mut gpui::TestAppContext) {
    let run = MessagePart::Tool {
        id: "run".into(),
        call: ToolCall::Exec { command: "cargo test".into() },
        is_error: false,
        resolved: true,
        output: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: None,
        subagent_ref: None,
        subagent_status: None,
        facts: None,
    };
    // Two calls before a reply, so their group draws its summary row.
    let again = exec("run2", "cargo build", true, false);
    crate::shell::force_touch(true);
    let (transcript, cx) =
        open(cx, entry(MessageStatus::Complete, vec![run, again, text("All green.")]));
    let ours = |transcript: &Entity<Transcript>, cx: &mut VisualTestContext| {
        // The strip shows on hover; the iPad's last row shows it unasked.
        transcript.update(cx, |this, cx| {
            this.hovered_entry = Some(("reply".into(), "reply".into()));
            cx.notify();
        });
        let nodes = draw(cx);
        let buttons = || nodes.values().filter(|node| node.role == Role::Button);
        let names: Vec<_> = buttons().map(|node| node.label.clone()).collect();
        let group = buttons()
            .find(|node| node.label.starts_with("Ran 2 commands"))
            .unwrap_or_else(|| panic!("{names:?}"))
            .clone();
        let copy = find(&nodes, Role::Button, "Copy message")
            .unwrap_or_else(|| panic!("{names:?}"))
            .clone();
        (group.height, copy.height, copy.width)
    };
    let (group, copy, copy_width) = ours(&transcript, cx);
    assert!(group >= 43.9, "the summary under a finger's 44 pt: {group}");
    assert!(copy.min(copy_width) >= 43.9, "Copy under a finger's 44 pt: {copy_width}x{copy}");

    crate::shell::force_touch(false);
    let (group, copy, copy_width) = ours(&transcript, cx);
    assert_eq!(group.round(), 26.0, "the summary row as drawn on a Mac");
    assert_eq!((copy_width.round(), copy.round()), (24.0, 24.0), "as drawn on a Mac");
}

// ---------------------------------------------------------------------------
// The transcript's polish (`docs/decisions/ui.md`, "The transcript reads as one
// conversation"): each test proves one of the audit's changes by what assistive tech reads.
// ---------------------------------------------------------------------------

/// A command `command` the agent ran, settled (`resolved`) or not, failed (`is_error`) or not.
fn exec(id: &str, command: &str, resolved: bool, is_error: bool) -> MessagePart {
    MessagePart::Tool {
        id: id.into(),
        call: ToolCall::Exec { command: command.into() },
        is_error,
        resolved,
        output: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: None,
        subagent_ref: None,
        subagent_status: None,
        facts: None,
    }
}

/// A message of the person's, `id`, saying `words`.
fn prompt(id: &str, words: &str) -> SessionMessageEntry {
    SessionMessageEntry {
        id: id.into(),
        role: MessageRole::User,
        parts: vec![MessagePart::Text { id: format!("{id}:t"), text: words.into() }],
        created_at: 0,
        device_id: "dev".into(),
        status: None,
        duration_ms: None,
    }
}

/// The reply `id` saying `words`.
fn answer(id: &str, words: &str) -> SessionMessageEntry {
    SessionMessageEntry { id: id.into(), ..entry(MessageStatus::Complete, vec![text(words)]) }
}

/// Show `entries` in the transcript, drawn.
fn show(
    transcript: &Entity<Transcript>,
    entries: Vec<SessionMessageEntry>,
    cx: &mut VisualTestContext,
) -> HashMap<NodeId, Node> {
    transcript.update(cx, |this, cx| {
        this.state.update(cx, |state, _| {
            state.transcript = entries;
            state.transcript_revision = state.transcript_revision.wrapping_add(1);
        });
        this.sync(cx);
    });
    draw(cx)
}

/// The label of the node holding the keyboard, in the last frame's tree.
fn focused(cx: &mut VisualTestContext) -> String {
    draw(cx);
    cx.update(|window, _| {
        let update = window.a11y_tree()?;
        update
            .nodes
            .iter()
            .find(|(id, _)| *id == update.focus)
            .map(|(_, node)| node.label().unwrap_or_default().to_owned())
    })
    .unwrap_or_default()
}

/// Enter on the focused control, down and up: a key press clicks as it comes up.
fn press_enter(cx: &mut VisualTestContext) {
    cx.simulate_keystrokes("enter");
    cx.simulate_event(gpui::KeyUpEvent { keystroke: gpui::Keystroke::parse("enter").unwrap() });
    cx.run_until_parked();
}

/// Tab, as the window's keyboard moves focus to the next stop.
fn tab(cx: &mut VisualTestContext) {
    cx.update(Window::focus_next);
}

fn top(node: &Node) -> f32 {
    f32::from(node.center.y) - node.height / 2.0
}

fn bottom(node: &Node) -> f32 {
    f32::from(node.center.y) + node.height / 2.0
}

/// #1: a prompt heads its turn: the big step comes before it, its answer starts a turn gap under
/// its line of figures, and under a pointer its strip waits beside the bubble's foot, taking no
/// room.
#[gpui::test]
fn a_prompt_heads_its_turn(cx: &mut gpui::TestAppContext) {
    let (transcript, cx) = open(cx, answer("a1", "First answer."));
    let nodes = show(
        &transcript,
        vec![
            prompt("p1", "First question"),
            answer("a1", "First answer."),
            prompt("p2", "Second question"),
            answer("a2", "Second answer."),
        ],
        cx,
    );
    let first = find(&nodes, Role::Paragraph, "First answer.").expect("the first answer").clone();
    let asked = find(&nodes, Role::Paragraph, "Second question").expect("the bubble").clone();
    let second =
        find(&nodes, Role::Paragraph, "Second answer.").expect("the second answer").clone();
    let after = top(&second) - bottom(&asked);
    let before = top(&asked) - bottom(&first);
    let under = super::turns::TURN_LINE + Theme::SPACE_LG;
    assert!((after - under).abs() <= 1.0, "the turn's line and 16 pt under the prompt: {after}");
    assert!((before - 56.0).abs() <= 1.0, "the strip's lane and 24 pt over it: {before}");

    // Hovered, the strip's Copy stands left of the bubble, inside its height.
    cx.simulate_mouse_move(asked.center, None, gpui::Modifiers::default());
    let nodes = draw(cx);
    let asked = find(&nodes, Role::Paragraph, "Second question").expect("the bubble").clone();
    let copies: Vec<&Node> = nodes
        .values()
        .filter(|node| node.role == Role::Button && node.label == "Copy message")
        .collect();
    let left = f32::from(asked.center.x) - asked.width / 2.0;
    assert!(
        copies.iter().any(|copy| {
            f32::from(copy.center.x) < left
                && f32::from(copy.center.y) > top(&asked)
                && f32::from(copy.center.y) < bottom(&asked)
        }),
        "Copy beside the bubble's foot: {copies:?} {asked:?}"
    );
}

/// #2: a group of one call shows the call itself; two calls keep their summary.
#[gpui::test]
fn a_lone_call_shows_itself(cx: &mut gpui::TestAppContext) {
    let (transcript, cx) = open(
        cx,
        entry(
            MessageStatus::Complete,
            vec![exec("one", "ls missing-file", true, false), text("Then:")],
        ),
    );
    let nodes = draw(cx);
    assert!(
        nodes.values().any(|node| node.label == "Run ls missing-file"),
        "the call by its words"
    );
    assert!(!nodes.values().any(|node| node.label.starts_with("Ran ")), "no summary over one call");
    let nodes = show(
        &transcript,
        vec![entry(
            MessageStatus::Complete,
            vec![exec("one", "cargo fmt", true, false), exec("two", "cargo check", true, false)],
        )],
        cx,
    );
    assert!(find(&nodes, Role::Button, "Ran 2 commands").is_some(), "two calls fold");
}

/// A call pressed open says its result to assistive tech, whole: JSON as the tree it draws.
#[gpui::test]
fn an_open_calls_result_says_its_lines(cx: &mut gpui::TestAppContext) {
    const SAID: &str = "\"liveness\": \"online\"";
    let mut call = exec("call", "", true, false);
    if let MessagePart::Tool { call, output, .. } = &mut call {
        *call = ToolCall::Mcp { server: "slopty".into(), tool: "machines".into(), input: None };
        *output = Some(format!("[\n  {{\n    \"name\": \"studio\",\n    {SAID}\n  }}\n]"));
    }
    let (_, cx) = open(cx, entry(MessageStatus::Complete, vec![call, text("One machine.")]));
    let says = |nodes: &HashMap<NodeId, Node>| {
        find(nodes, Role::TreeItem, "liveness: \"online\"").is_some()
    };
    let nodes = draw(cx);
    assert!(!says(&nodes), "folded to its line");
    let line = find(&nodes, Role::Button, "MCP slopty · machines").expect("its line").clone();
    cx.simulate_click(line.center, gpui::Modifiers::default());
    let nodes = draw(cx);
    let words: Vec<_> = nodes.values().map(|node| (node.role, node.label.as_str())).collect();
    assert!(says(&nodes), "its result, whole: {words:?}");
}

/// Step 4b: an open step reads in its kind's format from a menu that keeps its shape, says its
/// exit code on a line of its own, and offers what the model read and its raw record, fetched
/// when opened, the record a tree whose values fold at a press; a running command's output sits
/// in a capped box, so the trace under it holds still.
#[gpui::test]
fn an_open_step_reads_in_its_format_with_its_exit_code_and_records(cx: &mut gpui::TestAppContext) {
    let mut done = exec("one", "cargo test", true, false);
    if let MessagePart::Tool { output, facts, .. } = &mut done {
        *output = Some("{\"passed\": 3, \"failed\": [\"a\"]}".into());
        *facts = Some(Box::new(ToolFacts {
            exit: Some(ExitCode { code: 2, quiet: false }),
            seen: Some(BlobLink { blob: "chat/one.seen".into(), lines: 3 }),
            raw: Some("chat/one.raw".into()),
            ..ToolFacts::default()
        }));
    }
    let mut running = exec("two", "npm run dev", true, false);
    if let MessagePart::Tool { output, facts, .. } = &mut running {
        *output = Some((1..=40).fold(String::new(), |mut out, n| {
            let _written = writeln!(out, "tick {n}");
            out
        }));
        *facts = Some(Box::new(ToolFacts { growing: true, ..ToolFacts::default() }));
    }
    let parts = vec![done, text("Done."), running, text("Watching.")];
    let (transcript, cx) = open(cx, entry(MessageStatus::Complete, parts));
    let press =
        |nodes: &HashMap<NodeId, Node>, role: Role, label: &str, cx: &mut VisualTestContext| {
            let labels: Vec<&str> = nodes.values().map(|n| n.label.as_str()).collect();
            let node =
                find(nodes, role, label).unwrap_or_else(|| panic!("{label}: {labels:?}")).clone();
            cx.simulate_click(node.center, gpui::Modifiers::default());
            draw(cx)
        };
    let nodes = press(&draw(cx), Role::Button, "Run cargo test", cx);

    // JSON reads as a tree by itself, under a menu of all three formats.
    for format in ["Text", "JSON", "Markdown"] {
        assert!(find(&nodes, Role::Tab, format).is_some(), "the menu keeps its shape: {format}");
    }
    assert!(find(&nodes, Role::TreeItem, "passed: 3").is_some(), "JSON as a tree");
    assert!(find(&nodes, Role::Paragraph, "Exit code 2").is_some(), "its exit code, on its line");
    let nodes = press(&nodes, Role::TreeItem, "{2}", cx);
    assert!(find(&nodes, Role::TreeItem, "passed: 3").is_none(), "a value folds at a press");
    assert!(find(&nodes, Role::TreeItem, "{passed: 3, failed: […]}").is_some(), "to its preview");

    // The format chosen is kept for the kind; text reads as the TUI shows it.
    let nodes = press(&nodes, Role::Tab, "Text", cx);
    let kept = cx.update(|_, cx| crate::settings::output_format("Bash", cx));
    assert_eq!(kept.as_deref(), Some("text"));
    assert!(nodes.values().any(|n| n.role == Role::Code && n.label.contains("\"passed\": 3")));

    // What the model read, and the raw record, open once fetched.
    transcript.update(cx, |this, cx| {
        for (blob, text) in [
            ("chat/one.seen", "Exit code 2\nfailed: a\nok"),
            ("chat/one.raw", "[{\"type\": \"assistant\"}]"),
        ] {
            let detail = super::blob_detail(text, false).expect("a body");
            this.blob_details.insert(blob.into(), super::BlobFetch::Ready(Arc::new(detail)));
            this.step_detail.fetched(blob.into(), text);
        }
        cx.notify();
    });
    let nodes = press(&draw(cx), Role::Button, "Details for Claude · 3 lines", cx);
    assert!(
        nodes.values().any(|n| n.role == Role::Code && n.label.contains("failed: a")),
        "what the model read"
    );
    let nodes = press(&nodes, Role::Button, "Raw record", cx);
    assert!(
        find(&nodes, Role::TreeItem, "type: \"assistant\"").is_some(),
        "its raw record, as a tree"
    );

    // A running command's 40 lines sit in a box of 16: with its row, its command and its menu,
    // the step is far short of the 720 pt the 40 lines alone would take.
    let nodes = press(&nodes, Role::Button, "Run cargo test", cx);
    let nodes = press(&nodes, Role::Button, "Run npm run dev", cx);
    let row = find(&nodes, Role::Button, "Run npm run dev").expect("the call").clone();
    let under = find(&nodes, Role::Paragraph, "Watching.").expect("what follows").clone();
    let held = top(&under) - top(&row);
    assert!(held < 25.0 * super::OUTPUT_LINE_HEIGHT, "capped: {held}");
    assert!(
        nodes.values().any(|n| n.role == Role::Code && n.label.contains("tick 40")),
        "its tail, whole"
    );
}

/// #12: a turn that ended on a call that never settled says it stopped, never that it runs.
#[test]
fn a_finished_turns_summary_never_says_running() {
    let reply = entry(
        MessageStatus::Complete,
        vec![exec("one", "cargo fmt", true, false), exec("two", "cargo test", false, false)],
    );
    let rows =
        super::rows_for_entry(&reply, &mut |_, text| Arc::new(crate::markdown::parse_full(text)));
    let summaries: Vec<String> = rows
        .iter()
        .filter_map(|row| match &row.kind {
            super::RowKind::ToolGroup { summary, .. } => Some(summary.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(summaries, ["Ran 1 command and 1 command stopped"]);
}

/// #3: a summary says what settled in the past, what runs now in its live verb, and the call a
/// request holds up as waiting.
#[gpui::test]
fn a_summary_says_what_runs_now(cx: &mut gpui::TestAppContext) {
    let (transcript, cx) = open(
        cx,
        entry(
            MessageStatus::Streaming,
            vec![exec("one", "cargo fmt", true, false), exec("two", "cargo test", false, false)],
        ),
    );
    let nodes = draw(cx);
    assert!(
        find(&nodes, Role::Button, "Ran 1 command and running 1 command").is_some(),
        "{:?}",
        nodes.values().map(|n| n.label.clone()).collect::<Vec<_>>()
    );
    let ask = MessagePart::Input {
        id: "ask".into(),
        request_id: "r1".into(),
        questions: vec![slopty_view_proto::UserInputQuestion {
            kind: "approval".into(),
            header: "Bash command".into(),
            question: "touch refused.txt".into(),
            ..Default::default()
        }],
        resolved: false,
    };
    let nodes = show(
        &transcript,
        vec![entry(
            MessageStatus::Streaming,
            vec![
                exec("one", "cargo fmt", true, false),
                exec("two", "touch refused.txt", false, false),
                ask,
            ],
        )],
        cx,
    );
    let labels: Vec<String> = nodes.values().map(|n| n.label.clone()).collect();
    assert!(
        find(
            &nodes,
            Role::Button,
            "Ran 1 command and waiting for approval (run touch refused.txt)"
        )
        .is_some(),
        "{labels:?}"
    );
    // #8: the request's chip names what it waits for and shows the command, not its header.
    assert!(
        find(&nodes, Role::Status, "Needs your approval: touch refused.txt").is_some(),
        "{labels:?}"
    );
}

/// #5: a subagent's run drilled into as a chat reads from the top, not risen from the bottom,
/// starting at its pane's top.
#[gpui::test]
fn a_drilled_run_reads_from_the_top(cx: &mut gpui::TestAppContext) {
    let (transcript, cx) = open(cx, answer("a1", "Main answer."));
    transcript.update(cx, |this, cx| {
        this.state.update(cx, |state, _| {
            state.chats.push(
                serde_json::from_value(serde_json::json!({
                    "id": "child", "title": "Count lines", "deviceId": "dev", "archived": false,
                    "createdAt": chrono::Utc::now(), "cwd": "/work/app", "subagent": true,
                    "parentChatId": "chat",
                }))
                .unwrap(),
            );
            state.selected_chat = Some("child".into());
            state.transcript = vec![prompt("task", "Count the lines"), answer("a2", "It has 4.")];
            state.transcript_revision = state.transcript_revision.wrapping_add(1);
        });
        this.sync(cx);
    });
    let nodes = draw(cx);
    let said = find(&nodes, Role::Paragraph, "It has 4.").expect("the run's answer").clone();
    assert!(f32::from(said.center.y) < 300.0, "near the top of an 800 pt pane: {said:?}");
    // Its task starts one gap under its pane's top: the pane clears the window's chrome, so no
    // titlebar's height stands above it.
    let task = find(&nodes, Role::Paragraph, "Task: Count the lines").expect("its task").clone();
    let top = f32::from(task.center.y) - task.height / 2.0;
    let under = Theme::SPACE_LG..Theme::SPACE_LG + Theme::TITLEBAR_HEIGHT;
    assert!(under.contains(&top), "one gap under the pane's top, {under:?}: {task:?}");
}

/// #17: a subagent's run drilled into before the chat list lists it (a note tapped at launch)
/// lays out as a chat while it is unknown, and reads from the top once it is listed.
#[gpui::test]
fn a_run_listed_after_it_opened_reads_as_one(cx: &mut gpui::TestAppContext) {
    let (transcript, cx) = open(cx, answer("a1", "Main answer."));
    transcript.update(cx, |this, cx| {
        this.state.update(cx, |state, _| {
            state.selected_chat = Some("child".into());
            state.transcript = vec![prompt("task", "Count the lines"), answer("a2", "It has 4.")];
            state.transcript_revision = state.transcript_revision.wrapping_add(1);
        });
        this.sync(cx);
    });
    draw(cx);
    let top = transcript.read_with(cx, |this, _| this.list_top);
    assert!(!top, "unknown: laid as a chat");
    transcript.update(cx, |this, cx| {
        this.state.update(cx, |state, cx| {
            state.chats.push(
                serde_json::from_value(serde_json::json!({
                    "id": "child", "title": "Count lines", "deviceId": "dev", "archived": false,
                    "createdAt": chrono::Utc::now(), "cwd": "/work/app", "subagent": true,
                    "parentChatId": "chat",
                }))
                .unwrap(),
            );
            cx.notify();
        });
    });
    let nodes = draw(cx);
    let top = transcript.read_with(cx, |this, _| this.list_top);
    assert!(top, "listed as a run: from the top");
    let said = find(&nodes, Role::Paragraph, "It has 4.").expect("the run's answer").clone();
    assert!(f32::from(said.center.y) < 300.0, "near the top: {said:?}");
}

/// #6: a long prompt clips to five lines that fade out, with a quiet 12 pt "Show more" under
/// them, which the keyboard reaches and Enter opens.
#[gpui::test]
fn a_long_prompt_folds_quietly(cx: &mut gpui::TestAppContext) {
    let long = "A line of a long prompt.\n".repeat(12);
    let (transcript, cx) = open(cx, answer("a1", "Read."));
    let nodes = show(&transcript, vec![prompt("p1", &long), answer("a1", "Read.")], cx);
    let more = find(&nodes, Role::Button, "Expand message").expect("Show more").clone();
    assert!(more.height <= 20.0, "a 12 pt control: {more:?}");
    let bubble = nodes
        .values()
        .find(|node| node.role == Role::Paragraph && node.label.starts_with("A line"))
        .expect("the bubble")
        .clone();
    // Five lines of 22, the 4 pt gap, the 18 pt control and the bubble's 10 pt pads.
    let want = super::USER_LINE_HEIGHT.mul_add(5.0, 4.0 + 18.0 + 20.0);
    assert!((bubble.height - want).abs() <= 1.0, "no continuation line: {}", bubble.height);
    for _ in 0..12 {
        if focused(cx) == "Expand message" {
            break;
        }
        tab(cx);
    }
    assert_eq!(focused(cx), "Expand message", "Tab reaches Show more");
    press_enter(cx);
    let nodes = draw(cx);
    assert_eq!(find(&nodes, Role::Button, "Collapse message").and_then(|b| b.expanded), Some(true));
}

/// #7: a failed call's row keeps its words quiet; its icon and its result say it failed. A card
/// with no rail keeps its tint.
#[test]
fn a_failed_call_tints_only_its_icon_and_result() {
    let failed = super::ToolItem {
        part_id: "t1".into(),
        call: ToolCall::Exec { command: "ls missing-file".into() },
        is_error: true,
        resolved: true,
        detail: None,
        invocation: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        subagent_ref: None,
        subagent_status: None,
        kind: super::ToolItemKind::Call,
        output_text: None,
        images: Arc::new([]),
        facts: None,
    };
    assert!(!super::fails_loudly(&failed, true), "an activity row's words stay quiet");
    assert!(super::fails_loudly(&failed, false), "a card, with no rail, keeps the tint");
}

/// #9: Tab reaches a reply's controls: its group's summary, its call rows, the subagent's row,
/// and the strip's Copy, which a hover alone showed before.
#[gpui::test]
fn the_keyboard_reaches_the_controls_of_a_reply(cx: &mut gpui::TestAppContext) {
    let progress = SubagentProgress {
        running: false,
        state: "completed".into(),
        chat_id: Some("child-chat".into()),
        ..SubagentProgress::default()
    };
    let edit = |id: &str, path: &str| MessagePart::Tool {
        id: id.into(),
        call: ToolCall::WriteFile { path: path.into(), content: None },
        is_error: false,
        resolved: true,
        output: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: Some(vec![slopty_view_doc::ToolDiffStat {
            path: path.into(),
            additions: 1,
            deletions: 0,
        }]),
        subagent_ref: None,
        subagent_status: None,
        facts: None,
    };
    let (_, cx) = open(
        cx,
        entry(
            MessageStatus::Complete,
            vec![
                exec("one", "cargo fmt", true, false),
                exec("two", "cargo check", true, false),
                text("Then the agent."),
                spawn(progress),
                text("And the files."),
                edit("e1", "/work/app/a.rs"),
                edit("e2", "/work/app/b.rs"),
                text("Done."),
            ],
        ),
    );
    let reach = |cx: &mut VisualTestContext| {
        let mut reached = Vec::new();
        for _ in 0..24 {
            tab(cx);
            reached.push(focused(cx));
        }
        reached
    };
    // The settled turn's steps each take the keyboard where they were said, then its strip.
    let reached = reach(cx);
    for want in ["Ran 2 commands", "Subagent Count lines", "Copy message"] {
        assert!(reached.iter().any(|label| label.starts_with(want)), "{want}: {reached:?}");
    }
    // Enter opens the focused summary, as a click does.
    while !focused(cx).starts_with("Ran 2 commands") {
        tab(cx);
    }
    press_enter(cx);
    let nodes = draw(cx);
    let group = nodes.values().find(|n| n.label.starts_with("Ran 2 commands")).expect("the group");
    assert_eq!(group.expanded, Some(true));
    // #10: open, its rows are named by their calls.
    assert!(nodes.values().any(|node| node.label == "Run cargo fmt"), "the row says its call");
}

/// #10: a reply opens at its first row, whatever it is, named by its agent.
#[gpui::test]
fn a_reply_opens_at_its_first_row(cx: &mut gpui::TestAppContext) {
    let (_, cx) = open(
        cx,
        entry(
            MessageStatus::Complete,
            vec![
                exec("one", "cargo fmt", true, false),
                exec("two", "cargo check", true, false),
                text("Listed."),
            ],
        ),
    );
    let nodes = draw(cx);
    let article = find(&nodes, Role::Article, "Agent").expect("the reply's article").clone();
    let buttons = descendants(&nodes, &article, Role::Button);
    assert!(
        buttons.iter().any(|button| button.label.starts_with("Ran 2 commands")),
        "the first group is the reply's: {buttons:?}"
    );
}

/// #24: a run opened in place speaks at the compact body, a 20 pt line under its subagent's
/// 12 pt row; the reply around it keeps its 22.
#[gpui::test]
fn a_run_opened_in_place_reads_compact(cx: &mut gpui::TestAppContext) {
    let progress = SubagentProgress {
        running: false,
        state: SubagentProgress::DONE.into(),
        chat_id: Some("child-chat".into()),
        ..SubagentProgress::default()
    };
    let (transcript, cx) =
        open(cx, entry(MessageStatus::Complete, vec![text("Counting."), spawn(progress)]));
    let said = SessionMessageEntry {
        id: "child:0".into(),
        parts: vec![MessagePart::Text { id: "child:0:t".into(), text: "Four.".into() }],
        ..entry(MessageStatus::Complete, Vec::new())
    };
    let state = transcript.read_with(cx, |transcript, _| transcript.state.clone());
    state.update(cx, |state, _| state.set_subagent_snapshot("child-chat".into(), vec![said]));
    let nodes = draw(cx);
    let row = find(&nodes, Role::Button, "Subagent Count lines").expect("the row").clone();
    cx.simulate_click(row.center, gpui::Modifiers::default());
    let nodes = settle(cx);
    let child = find(&nodes, Role::Paragraph, "Four.").expect("the run's words").clone();
    let parent = find(&nodes, Role::Paragraph, "Counting.").expect("the reply's words").clone();
    assert!((child.height - super::render::MD_COMPACT_LINE_HEIGHT).abs() <= 0.5, "{child:?}");
    assert!((parent.height - super::render::MD_LINE_HEIGHT).abs() <= 0.5, "{parent:?}");
}

/// #14: a call under way sweeps its words with its group title's light; a settled one, a
/// thought and a subagent, whose own row says it runs, do not. (The headless text system
/// paints no glyphs, so the sweep is judged where it is decided.)
#[test]
fn a_running_call_sweeps_its_words() {
    let entry = entry(
        MessageStatus::Streaming,
        vec![
            exec("one", "cargo test", false, false),
            exec("two", "cargo fmt", true, false),
            spawn(SubagentProgress::default()),
        ],
    );
    let rows =
        super::rows_for_entry(&entry, &mut |_, text| Arc::new(crate::markdown::parse_full(text)));
    let tools: Vec<_> = rows
        .iter()
        .filter_map(|row| match &row.kind {
            super::RowKind::ToolGroup { tools, .. } => Some(tools.iter().cloned()),
            _ => None,
        })
        .flatten()
        .collect();
    let swept: Vec<bool> = tools.iter().map(super::sweeps).collect();
    assert_eq!(swept, [true, false], "{tools:?}");
    let spawn = rows.iter().find_map(|row| match &row.kind {
        super::RowKind::Spawn { tool } => Some(Arc::clone(tool)),
        _ => None,
    });
    assert!(spawn.is_some_and(|tool| !super::sweeps(&tool)), "its own row, unswept");
}

/// #18: a fetch that failed says so, and why, in the transcript's voice, and offers one Retry
/// that says what it fetches again.
#[gpui::test]
fn a_failed_fetch_says_so_and_offers_retry(cx: &mut gpui::TestAppContext) {
    let mut call = exec("one", "cargo test", true, false);
    if let MessagePart::Tool { output, output_ref, output_bytes, .. } = &mut call {
        *output = Some("test result: ok".into());
        *output_ref = Some("blob-1".into());
        *output_bytes = Some(12_000);
    }
    let (transcript, cx) = open(cx, entry(MessageStatus::Complete, vec![call, text("Done.")]));
    transcript.update(cx, |this, cx| {
        this.blob_details
            .insert("blob-1".into(), super::BlobFetch::Failed("the machine went away".into()));
        cx.notify();
    });
    let nodes = draw(cx);
    let row = find(&nodes, Role::Button, "Run cargo test").expect("the call").clone();
    cx.simulate_click(row.center, gpui::Modifiers::default());
    let nodes = draw(cx);
    let labels: Vec<String> = nodes.values().map(|n| n.label.clone()).collect();
    assert!(find(&nodes, Role::Button, "Retry loading output").is_some(), "{labels:?}");
}

/// Find lands on a step the trace hides: at Prompts it shows the steps, opens the step's group
/// and puts the trace's cursor on it, the failed-steps filter finding it with no words typed.
#[gpui::test]
fn find_opens_what_hides_the_step_it_lands_on(cx: &mut gpui::TestAppContext) {
    use super::find::Filter;
    use super::trace::{Cursor, Depth};
    let (transcript, cx) = open(
        cx,
        entry(
            MessageStatus::Complete,
            vec![
                exec("t1", "cargo test", true, false),
                exec("t2", "cargo build", true, true),
                text("Done."),
            ],
        ),
    );
    transcript.update(cx, |this, cx| this.set_depth(Depth::Prompts, cx));
    draw(cx);
    let group = transcript
        .read_with(cx, |this, _| {
            this.whole_rows()
                .iter()
                .find(|row| matches!(row.kind, super::RowKind::ToolGroup { .. }))
                .map(|row| row.id.clone())
        })
        .expect("the run's group");
    assert!(
        transcript.read_with(cx, |this, _| !this.rows.iter().any(|row| row.id == group)),
        "Prompts hides it"
    );
    cx.update(|window, cx| {
        transcript.update(cx, |this, cx| {
            this.open_find(window, cx);
            this.set_find_filter(Filter { failed: true, kind: None }, window, cx);
        });
    });
    draw(cx);
    transcript.read_with(cx, |this, _| {
        assert_eq!(this.depth(), Depth::Steps, "the steps show");
        assert_eq!(
            this.folds.get(&group).and_then(|fold| fold.open),
            Some(true),
            "its group opens"
        );
        assert_eq!(
            this.cursor.as_ref(),
            Some(&Cursor { row: group.clone(), step: Some("t2".into()) }),
            "the cursor on it"
        );
        assert!(this.finding(), "find stays open");
        assert_eq!(this.find.tally().as_deref(), Some("1 of 1"));
    });
}

/// Where the trace holds the keyboard, E goes to the failure, opening its group to land on it;
/// ↑ and ↓ move over the steps shown and the group's line, and ← on the line folds it.
#[gpui::test]
fn the_traces_keys_move_its_cursor_and_e_lands_on_the_failure(cx: &mut gpui::TestAppContext) {
    use super::trace::Cursor;
    let (transcript, cx) = open(
        cx,
        entry(
            MessageStatus::Complete,
            vec![
                text("Looking."),
                exec("t1", "cargo test", true, false),
                exec("t2", "cargo build", true, true),
                text("Done."),
            ],
        ),
    );
    draw(cx);
    let group = transcript
        .read_with(cx, |this, _| {
            this.rows
                .iter()
                .find(|row| matches!(row.kind, super::RowKind::ToolGroup { .. }))
                .map(|row| row.id.clone())
        })
        .expect("the run's group");
    cx.update(|window, cx| {
        let keys = transcript.read(cx).keys_handle();
        window.focus(&keys, cx);
    });
    let at = |cx: &mut VisualTestContext| transcript.read_with(cx, |this, _| this.cursor.clone());
    cx.simulate_keystrokes("e");
    draw(cx);
    assert_eq!(
        at(cx),
        Some(Cursor { row: group.clone(), step: Some("t2".into()) }),
        "on the failure"
    );
    assert_eq!(
        transcript.read_with(cx, |this, _| this.folds.get(&group).and_then(|fold| fold.open)),
        Some(true)
    );
    cx.simulate_keystrokes("up");
    assert_eq!(
        at(cx),
        Some(Cursor { row: group.clone(), step: Some("t1".into()) }),
        "the step before"
    );
    cx.simulate_keystrokes("up");
    assert_eq!(at(cx), Some(Cursor { row: group.clone(), step: None }), "its group's line");
    cx.simulate_keystrokes("left");
    draw(cx);
    assert_eq!(
        transcript.read_with(cx, |this, _| this.folds.get(&group).and_then(|fold| fold.open)),
        Some(false),
        "folded"
    );
    cx.simulate_keystrokes("down");
    let below = transcript.read_with(cx, |this, _| {
        let ix = this.rows.iter().position(|row| row.id == group).unwrap();
        this.rows.get(ix.saturating_add(1)).map(|row| row.id.clone())
    });
    assert_eq!(at(cx).map(|cursor| cursor.row), below, "over the folded group to the row under it");
}

/// The inspector's frame on a long session (`docs/MEASUREMENTS.md`): 300 turns, each a prompt
/// and an answer of a sentence, six commands with twelve lines of output and a closing reply with
/// a list and a code fence, in a 480 × 900 pt pane. Three arms of 300 frames each, timed from the
/// state's change to the drawn scene: held at the end, scrolled through the middle 53 pt a
/// frame, and following an answer that streams a sentence a frame. Each frame also counts the
/// rows it built, which the list's virtualisation keeps to those in view.
#[gpui::test]
#[ignore = "a measurement: run in release with --ignored --nocapture"]
fn long_trace_frame_cost(cx: &mut gpui::TestAppContext) {
    use std::time::{Duration, Instant};
    const TURNS: usize = 300;
    const FRAMES: usize = 300;
    let call = |id: String, n: usize| MessagePart::Tool {
        id,
        call: ToolCall::Exec { command: format!("cargo test -p crate{n} -- --nocapture") },
        is_error: n == 4,
        resolved: true,
        output: Some((0..12).fold(String::new(), |mut out, line| {
            let _written = writeln!(out, "test parser::case_{line} ... ok");
            out
        })),
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: None,
        subagent_ref: None,
        subagent_status: None,
        facts: None,
    };
    let mut entries = Vec::new();
    for turn in 0..TURNS {
        entries.push(prompt(
            &format!("p{turn}"),
            &format!("Turn {turn}: make the parser take nested lists, then run the tests and say what failed."),
        ));
        let mut parts = vec![text(&format!("Looking at the parser for turn {turn}."))];
        parts.extend((0..6).map(|n| call(format!("t{turn}-{n}"), n)));
        parts.push(text(&format!(
            "Turn {turn} is done.\n\n- The lists nest now.\n- Two tests failed: `nested_quote` and \
             `deep_list`.\n\n```rust\nfn parse(input: &str) -> Tree {{\n    Tree::new(input)\n}}\n```"
        )));
        entries.push(SessionMessageEntry {
            id: format!("a{turn}"),
            ..entry(MessageStatus::Complete, parts)
        });
    }
    let (transcript, cx) = open(cx, answer("a", "First."));
    show(&transcript, entries, cx);
    cx.simulate_resize(gpui::size(px(480.0), px(900.0)));
    cx.update(|window, _| window.set_a11y_active(false));
    let frame = |cx: &mut VisualTestContext| {
        let started = Instant::now();
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let took = started.elapsed();
        (took, transcript.read_with(cx, |this, _| this.rendered_rows.len()))
    };
    for _ in 0..30 {
        frame(cx);
    }
    let total = transcript.read_with(cx, |this, _| this.rows.len());
    let report = |name: &str, samples: Vec<(Duration, usize)>| {
        let mut took: Vec<f64> = samples.iter().map(|(took, _)| took.as_secs_f64() * 1e6).collect();
        took.sort_by(f64::total_cmp);
        // The sample `percent` of the way up, rounded to the nearest.
        let at = |percent: usize| {
            took[took.len().saturating_sub(1).saturating_mul(percent).saturating_add(50) / 100]
        };
        let rows: Vec<usize> = samples.iter().map(|(_, rows)| *rows).collect();
        println!(
            "MEASURE long_trace {name}: p50 {:.0} µs, p95 {:.0} µs, max {:.0} µs; rows built {}–{} of {total}",
            at(50),
            at(95),
            at(100),
            rows.iter().min().unwrap_or(&0),
            rows.iter().max().unwrap_or(&0),
        );
    };
    transcript.update(cx, |this, _| this.follow());
    report("held at the end", std::iter::repeat_with(|| frame(cx)).take(FRAMES).collect());
    transcript.update(cx, |this, _| {
        this.let_go();
        this.list.scroll_to(gpui::ListOffset { item_ix: total / 2, offset_in_item: px(0.0) });
    });
    let scrolled = std::iter::repeat_with(|| {
        transcript.update(cx, |this, _| this.list.scroll_by(px(53.0)));
        frame(cx)
    })
    .take(FRAMES)
    .collect();
    report("scrolled through the middle", scrolled);
    transcript.update(cx, |this, cx| {
        this.follow();
        this.state.update(cx, |state, _| {
            let words =
                MessagePart::Text { id: "live:t".into(), text: "The answer begins.".into() };
            state.transcript.push(SessionMessageEntry {
                id: "live".into(),
                ..entry(MessageStatus::Streaming, vec![words])
            });
            state.transcript_revision = state.transcript_revision.wrapping_add(1);
        });
        this.sync(cx);
    });
    let streamed = (0..FRAMES)
        .map(|n| {
            let started = Instant::now();
            transcript.update(cx, |this, cx| {
                this.state.update(cx, |state, _| {
                    let live = state.transcript.last_mut().expect("the live answer");
                    if let Some(MessagePart::Text { text, .. }) = live.parts.last_mut() {
                        let _written = write!(text, " Sentence {n} of the answer as it streams.");
                    }
                    state.transcript_revision = state.transcript_revision.wrapping_add(1);
                });
                this.sync(cx);
            });
            let (_, rows) = frame(cx);
            (started.elapsed(), rows)
        })
        .collect();
    report("following a streaming answer", streamed);
}
