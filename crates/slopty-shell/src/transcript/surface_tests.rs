//! slopty: the transcript's own rows as assistive tech reads them: a subagent's card, the rows
//! of background work, a reply's images and formulas. Each test draws a real transcript in a
//! test window and judges by its accessibility tree.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::accesskit::{NodeId, Role};
use gpui::{Entity, IntoElement, Pixels, Point, Render, VisualTestContext, Window, div, point, prelude::*, px};
use slopty_view_doc::{
    MessagePart, MessageRole, MessageStatus, NoteRow, SessionMessageEntry, SubagentProgress,
    SubagentStatus, TaskRow, ToolFacts,
};
use slopty_view_proto::ToolCall;

use super::{Transcript, TranscriptEvent};
use crate::state::AppState;
use crate::theme::Theme;

struct Host(Entity<Transcript>);

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
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
                        px(((r.x0 + r.x1) / 2.0 / scale) as f32),
                        px(((r.y0 + r.y1) / 2.0 / scale) as f32),
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

fn entry(status: MessageStatus, parts: Vec<MessagePart>) -> SessionMessageEntry {
    SessionMessageEntry {
        id: "reply".into(),
        role: MessageRole::Assistant,
        parts,
        created_at: 0,
        device_id: "dev".into(),
        status: Some(status),
        continuation_of: None,
        duration_ms: None,
    }
}

fn text(text: &str) -> MessagePart {
    MessagePart::Text { id: "text".into(), text: text.into() }
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
        state.transcript_revision += 1;
    });
    let transcript = cx.new(|cx| Transcript::new(state, cx));
    transcript.update(cx, |this, cx| this.sync(cx));
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
        diff: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: None,
        subagent_ref: Some("child-chat".into()),
        subagent_status: Some(SubagentStatus::Running),
        subagent_tail: None,
        facts: Some(Box::new(ToolFacts {
            subagent_progress: Some(progress),
            ..ToolFacts::default()
        })),
    }
}

/// A subagent's card reads its step; a click opens its run in place, Escape folds it, and
/// ⌘↩ drills into it.
#[gpui::test]
fn a_subagent_card_opens_in_place_and_drills_in(cx: &mut gpui::TestAppContext) {
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
    let opened = Rc::new(RefCell::new(Vec::new()));
    let _events = cx.update(|_, cx| {
        let opened = opened.clone();
        cx.subscribe(&transcript, move |_, event: &TranscriptEvent, _| {
            if let TranscriptEvent::OpenSubagent { chat_id } = event {
                opened.borrow_mut().push(chat_id.clone());
            }
        })
    });
    let nodes = draw(cx);
    let card = find(&nodes, Role::Button, "Subagent Count lines").expect("the card").clone();
    assert!(card.value.starts_with("Reading parser.rs"), "{card:?}");
    assert_eq!(card.expanded, Some(false));
    assert!(find(&nodes, Role::Group, "Transcript of Count lines").is_none());

    cx.simulate_click(card.center, gpui::Modifiers::default());
    let nodes = draw(cx);
    let card = find(&nodes, Role::Button, "Subagent Count lines").expect("the card");
    assert_eq!(card.expanded, Some(true));
    assert!(find(&nodes, Role::Group, "Transcript of Count lines").is_some(), "open in place");

    cx.simulate_keystrokes("escape");
    let nodes = draw(cx);
    assert_eq!(find(&nodes, Role::Button, "Subagent Count lines").unwrap().expanded, Some(false));
    assert!(find(&nodes, Role::Group, "Transcript of Count lines").is_none(), "folded");

    cx.simulate_keystrokes("cmd-enter");
    draw(cx);
    assert_eq!(*opened.borrow(), ["child-chat"]);
}

/// slopty: on a phone a tap on a subagent's card drills into its run: no room to open it in
/// place, so it says no open state and asks the shell for the run's chat.
#[gpui::test]
fn a_phones_tap_on_a_subagents_card_drills_in(cx: &mut gpui::TestAppContext) {
    let progress = SubagentProgress {
        running: true,
        state: SubagentProgress::RUNNING.into(),
        step: Some("Reading parser.rs".into()),
        chat_id: Some("child-chat".into()),
        row: Some("task:a1".into()),
        ..SubagentProgress::default()
    };
    let (transcript, cx) = open(cx, entry(MessageStatus::Complete, vec![spawn(progress)]));
    cx.simulate_resize(gpui::size(px(390.0), px(800.0)));
    let opened = Rc::new(RefCell::new(Vec::new()));
    let _events = cx.update(|_, cx| {
        let opened = opened.clone();
        cx.subscribe(&transcript, move |_, event: &TranscriptEvent, _| {
            if let TranscriptEvent::OpenSubagent { chat_id } = event {
                opened.borrow_mut().push(chat_id.clone());
            }
        })
    });
    let nodes = draw(cx);
    let card = find(&nodes, Role::Button, "Subagent Count lines").expect("the card").clone();
    assert_eq!(card.expanded, None, "a phone's card does not open in place: {card:?}");
    cx.simulate_click(card.center, gpui::Modifiers::default());
    draw(cx);
    assert_eq!(*opened.borrow(), ["child-chat"], "the tap drills in");
}

/// A background task's row opens the tray at its row; a retry says what is tried again.
#[gpui::test]
fn work_rows_link_to_the_tray(cx: &mut gpui::TestAppContext) {
    let task = TaskRow {
        task: "t1".into(),
        row: "task:t1".into(),
        kind: "command".into(),
        status: "completed".into(),
        text: "Background command finished".into(),
        title: "cargo test".into(),
        ..TaskRow::default()
    };
    let note = NoteRow {
        kind: "hook".into(),
        text: "Saved 2 memories".into(),
        ..NoteRow::default()
    };
    let (_, cx) = open(
        cx,
        entry(
            MessageStatus::Complete,
            vec![
                text("Started the tests."),
                MessagePart::Task { id: "task".into(), task: Box::new(task) },
                MessagePart::Note { id: "note".into(), note: Box::new(note) },
            ],
        ),
    );
    let nodes = draw(cx);
    let row = find(&nodes, Role::Button, "Background command finished · cargo test")
        .expect("the task's row")
        .clone();
    assert_eq!(row.value, "completed");
    let notice = nodes
        .values()
        .find(|node| node.role == Role::Status && node.label.contains("Saved 2 memories"))
        .expect("the note");
    assert_eq!(notice.value, "hook");
    assert!(cx.update(|_, cx| cx.try_global::<super::WorkReveal>().is_none()));

    cx.simulate_click(row.center, gpui::Modifiers::default());
    draw(cx);
    let reveal = cx.update(|_, cx| cx.try_global::<super::WorkReveal>().cloned());
    let reveal = reveal.expect("the tray was asked to open");
    assert_eq!((reveal.chat_id.as_str(), reveal.row_id.as_str(), reveal.seq), ("chat", "task:t1", 1));
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
        entry(MessageStatus::Complete, vec![text("$$\\frac{a}{b}$$\n\nthen\n\n$$\\nosuchcommand{x}$$")]),
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
            state.transcript_revision += 1;
        });
        this.sync(cx);
    });
    let nodes = draw(cx);
    assert!(nodes.values().all(|node| node.role != Role::Math), "the tail keeps text");
}

/// slopty: a failed turn's card offers its error to copy, the agent's words with its detail, and
/// the detail opens in a box that scrolls (T51) and says it to the screen reader.
#[gpui::test]
fn an_error_card_copies_its_error_and_scrolls_its_detail(cx: &mut gpui::TestAppContext) {
    let detail = (1..=40).map(|n| format!("trace line {n}")).collect::<Vec<_>>().join("\n");
    let failed = MessagePart::Error {
        id: "error".into(),
        message: "The turn failed".into(),
        detail: Some(detail.clone()),
        retry: true,
    };
    let (_, cx) = open(cx, entry(MessageStatus::Complete, vec![failed]));
    let nodes = draw(cx);
    let copy = find(&nodes, Role::Button, "Copy error").expect("Copy error").clone();
    cx.simulate_click(copy.center, gpui::Modifiers::default());
    draw(cx);
    let copied = cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some(format!("The turn failed\n\n{detail}").as_str()));
    let details = find(&nodes, Role::Button, "View details").expect("View details").clone();
    cx.simulate_click(details.center, gpui::Modifiers::default());
    let nodes = draw(cx);
    assert!(find(&nodes, Role::Button, "Hide details").is_some(), "the detail opened");
    assert!(find(&nodes, Role::Paragraph, &detail).is_some(), "the detail says why");
}

/// A subagent's run open in its card is as tall as its rows, nothing under its last line; a run
/// longer than the card's room takes that room and scrolls in it.
#[gpui::test]
fn a_subagent_run_open_in_its_card_is_as_tall_as_its_rows(cx: &mut gpui::TestAppContext) {
    let progress = SubagentProgress {
        running: false,
        state: SubagentProgress::DONE.into(),
        chat_id: Some("child-chat".into()),
        ..SubagentProgress::default()
    };
    let (transcript, cx) = open(cx, entry(MessageStatus::Complete, vec![spawn(progress)]));
    let said = |n: usize| SessionMessageEntry {
        id: format!("child:{n}"),
        parts: vec![MessagePart::Text { id: format!("child:{n}:t"), text: format!("Line {n} of the run.") }],
        ..entry(MessageStatus::Complete, Vec::new())
    };
    let state = transcript.read_with(cx, |transcript, _| transcript.state.clone());
    state.update(cx, |state, _| state.set_subagent_snapshot("child-chat".into(), vec![said(0)]));
    let nodes = draw(cx);
    let card = find(&nodes, Role::Button, "Subagent Count lines").expect("the card").clone();
    cx.simulate_click(card.center, gpui::Modifiers::default());
    // Its rows lay out, then the card takes their height: a frame or two.
    let settled = |cx: &mut VisualTestContext| (0..4).map(|_| draw(cx)).last().unwrap_or_default();
    let nodes = settled(cx);
    let run = find(&nodes, Role::Group, "Transcript of Count lines").expect("open in place").clone();
    let cap = transcript.read_with(cx, |transcript, _| transcript.subagents.child_room());
    assert!(run.height > 20.0 && run.height < 120.0, "one line's height, not the room's {cap}: {run:?}");
    state.update(cx, |state, cx| {
        state.set_subagent_snapshot("child-chat".into(), (0..60).map(said).collect());
        cx.notify();
    });
    let nodes = settled(cx);
    let run = find(&nodes, Role::Group, "Transcript of Count lines").expect("open in place");
    assert!((run.height - cap).abs() <= 1.5, "the card's room, {cap}: {run:?}");
}

/// A subagent's card slides its run open and shut: its height eases from none to its own and
/// back, the run held through the slide, and let go once shut. The run opens on its task, a quiet
/// paragraph, not the person's bubble.
#[gpui::test]
fn a_subagent_card_slides_its_run_open_and_shut(cx: &mut gpui::TestAppContext) {
    let progress = SubagentProgress {
        running: false,
        state: SubagentProgress::DONE.into(),
        chat_id: Some("child-chat".into()),
        ..SubagentProgress::default()
    };
    let (transcript, cx) = open(cx, entry(MessageStatus::Complete, vec![spawn(progress)]));
    cx.update(|_, cx| cx.set_reduce_motion(false));
    let task = SessionMessageEntry {
        id: "child:task".into(),
        role: MessageRole::User,
        parts: vec![MessagePart::Text { id: "child:task:t".into(), text: "Count the lines".into() }],
        ..entry(MessageStatus::Complete, Vec::new())
    };
    let said = |n: usize| SessionMessageEntry {
        id: format!("child:{n}"),
        parts: vec![MessagePart::Text { id: format!("child:{n}:t"), text: format!("Line {n} of the run.") }],
        ..entry(MessageStatus::Complete, Vec::new())
    };
    let state = transcript.read_with(cx, |transcript, _| transcript.state.clone());
    let run = std::iter::once(task).chain((0..6).map(said)).collect();
    state.update(cx, |state, _| state.set_subagent_snapshot("child-chat".into(), run));
    let height = |nodes: &HashMap<NodeId, Node>| {
        find(nodes, Role::Group, "Transcript of Count lines").map(|run| run.height)
    };
    let step = |cx: &mut VisualTestContext, ms: u64| {
        cx.executor().advance_clock(std::time::Duration::from_millis(ms));
        draw(cx)
    };
    let nodes = draw(cx);
    let card = find(&nodes, Role::Button, "Subagent Count lines").expect("the card").clone();
    cx.simulate_click(card.center, gpui::Modifiers::default());
    let first = height(&draw(cx)).expect("held from its first frame");
    let mid = height(&step(cx, 120)).expect("sliding open");
    let nodes = step(cx, 2000);
    let whole = height(&nodes).expect("open");
    assert!(first < mid && mid < whole, "it eases open: {first} < {mid} < {whole}");
    assert!(find(&nodes, Role::Paragraph, "Task: Count the lines").is_some(), "its task, quiet");
    assert!(find(&nodes, Role::Article, "You").is_none(), "no bubble of the person's");

    let nodes = draw(cx);
    let card = find(&nodes, Role::Button, "Subagent Count lines").expect("the card").clone();
    cx.simulate_click(card.center, gpui::Modifiers::default());
    let shutting = height(&step(cx, 120)).expect("held while it slides shut");
    assert!(shutting < whole, "it eases shut: {shutting} < {whole}");
    let nodes = step(cx, 2000);
    assert_eq!(height(&nodes), None, "shut, its run let go");
    let open = transcript.read_with(cx, |transcript, _| transcript.subagents.open_children());
    assert!(open.is_empty(), "no run watched: {open:?}");
}

/// A subagent's card clicked open before its run arrived (the run is fetched once the card
/// opens) holds its run in place on every frame after, and takes the run's height once it
/// arrives.
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
    let card = find(&nodes, Role::Button, "Subagent Count lines").expect("the card").clone();
    cx.simulate_click(card.center, gpui::Modifiers::default());
    for frame in 0..4 {
        let nodes = draw(cx);
        let run = find(&nodes, Role::Group, "Transcript of Count lines");
        assert!(run.is_some_and(|run| run.height > 0.5), "frame {frame}: {run:?}");
    }
    let said = |n: usize| SessionMessageEntry {
        id: format!("child:{n}"),
        parts: vec![MessagePart::Text {
            id: format!("child:{n}:t"),
            text: format!("Line {n} of the run."),
        }],
        ..entry(MessageStatus::Complete, Vec::new())
    };
    let state = transcript.read_with(cx, |transcript, _| transcript.state.clone());
    state.update(cx, |state, cx| {
        state.set_subagent_snapshot("child-chat".into(), vec![said(0), said(1)]);
        cx.notify();
    });
    for frame in 0..4 {
        let nodes = draw(cx);
        let run = find(&nodes, Role::Group, "Transcript of Count lines");
        assert!(run.is_some_and(|run| run.height > 20.0), "its rows in, frame {frame}: {run:?}");
    }
}

#[gpui::test]
fn a_card_opened_before_its_ended_run_arrives_holds_it(cx: &mut gpui::TestAppContext) {
    opened_before_its_run(cx, false);
}

#[gpui::test]
fn a_card_opened_before_its_running_run_arrives_holds_it(cx: &mut gpui::TestAppContext) {
    opened_before_its_run(cx, true);
}

/// slopty: the transcript's words are in the accessibility tree: the person's message is an
/// article "You" holding a paragraph of their words, the reply an article "Claude" at its first
/// block, and each of the reply's blocks a paragraph of its plain text.
#[gpui::test]
fn a_chats_words_are_articles_and_paragraphs(cx: &mut gpui::TestAppContext) {
    let reply = entry(MessageStatus::Complete, vec![text("The ls **failed**.\n\nThen it was done.")]);
    let (transcript, cx) = open(cx, reply.clone());
    let asked = SessionMessageEntry {
        id: "ask".into(),
        role: MessageRole::User,
        parts: vec![MessagePart::Text { id: "ask:t".into(), text: "Survey the files".into() }],
        status: None,
        ..reply.clone()
    };
    transcript.update(cx, |this, cx| {
        this.state.update(cx, |state, _| {
            state.transcript = vec![asked, reply];
            state.transcript_revision += 1;
        });
        this.sync(cx);
    });
    let nodes = draw(cx);
    let you = find(&nodes, Role::Article, "You").expect("the person's message").clone();
    let said = descendants(&nodes, &you, Role::Paragraph);
    assert!(said.iter().any(|p| p.label == "Survey the files"), "{said:?}");
    let claude = find(&nodes, Role::Article, "Claude").expect("the reply").clone();
    let first = descendants(&nodes, &claude, Role::Paragraph);
    assert!(first.iter().any(|p| p.label == "The ls failed."), "plain text: {first:?}");
    assert!(find(&nodes, Role::Paragraph, "Then it was done.").is_some(), "the second block");
    let articles = nodes.values().filter(|n| n.role == Role::Article).count();
    assert_eq!(articles, 2, "one article a message");
}

/// slopty: a link to a chat in a reply is a live chip: it says the chat's title and how it
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

/// slopty: a subagent card's quiet Open draws in its 18 pt footprint but takes a press over at
/// least 24 pt (`surface_chrome::press_target`): a press 1.5 pt past its footprint opens the run.
#[gpui::test]
fn a_cards_quiet_open_takes_a_press_past_what_it_draws(cx: &mut gpui::TestAppContext) {
    let progress = SubagentProgress {
        running: false,
        state: SubagentProgress::DONE.into(),
        chat_id: Some("child-chat".into()),
        ..SubagentProgress::default()
    };
    let (transcript, cx) = open(cx, entry(MessageStatus::Complete, vec![spawn(progress)]));
    let opened = Rc::new(RefCell::new(Vec::new()));
    let _events = cx.update(|_, cx| {
        let opened = opened.clone();
        cx.subscribe(&transcript, move |_, event: &TranscriptEvent, _| {
            if let TranscriptEvent::OpenSubagent { chat_id } = event {
                opened.borrow_mut().push(chat_id.clone());
            }
        })
    });
    let nodes = draw(cx);
    let target = find(&nodes, Role::Button, "Open subagent").expect("the quiet Open").clone();
    assert!(target.height >= 24.0, "pressed over 24 pt: {target:?}");
    let past = point(target.center.x + px(10.5), target.center.y);
    cx.simulate_click(past, gpui::Modifiers::default());
    draw(cx);
    assert_eq!(*opened.borrow(), ["child-chat"], "a press past the 18 pt footprint opens it");
}

/// slopty: a click on a subagent's card between two reads of the tree, as the self-test's driver
/// clicks: the frames around it drawn as a person's are (no screen reader, the list scrolled onto
/// its layer), rows under the card, its run there to fit to. Once the frames settle, the run is
/// open in place and the frame shown is the one the same state draws from scratch: no row under
/// the card is left where it stood before the card grew.
#[gpui::test]
fn a_card_clicked_on_a_persons_frame_opens_on_the_next(cx: &mut gpui::TestAppContext) {
    let progress = SubagentProgress {
        running: false,
        state: SubagentProgress::DONE.into(),
        chat_id: Some("child-chat".into()),
        ..SubagentProgress::default()
    };
    let words = (0..40).map(|n| format!("Paragraph {n} of the survey.\n\n")).collect::<String>();
    let (transcript, cx) = open(
        cx,
        entry(MessageStatus::Complete, vec![text(&words), spawn(progress), text("Counted.")]),
    );
    let said = |n: usize| SessionMessageEntry {
        id: format!("child:{n}"),
        parts: vec![MessagePart::Text {
            id: format!("child:{n}:t"),
            text: format!("Line {n} of the run."),
        }],
        ..entry(MessageStatus::Complete, Vec::new())
    };
    let state = transcript.read_with(cx, |transcript, _| transcript.state.clone());
    state.update(cx, |state, _| state.set_subagent_snapshot("child-chat".into(), (0..3).map(said).collect()));
    let plain = |cx: &mut VisualTestContext| {
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.set_a11y_active(false);
            window.draw(cx).clear(cx);
        });
    };
    plain(cx);
    for dy in [-30.0, 30.0, -30.0, 30.0, -30.0, 30.0] {
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: point(px(500.0), px(400.0)),
            delta: gpui::ScrollDelta::Pixels(point(px(0.0), px(dy))),
            ..Default::default()
        });
        plain(cx);
    }
    // A dump: the tree from a frame of its own.
    let card = find(&draw(cx), Role::Button, "Subagent Count lines").expect("the card").center;
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
    // The frames settle: the run lays out, then the card takes its height.
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
    assert!(missed.is_empty() && extra.is_empty(), "stale once open: missed {missed:#?}, extra {extra:#?}");
    let nodes = draw(cx);
    assert_eq!(find(&nodes, Role::Button, "Subagent Count lines").and_then(|c| c.expanded), Some(true));
    let run = find(&nodes, Role::Group, "Transcript of Count lines").expect("open in place");
    assert!(run.height > 20.0, "fit to its run: {run:?}");
}

/// slopty: what runs reads the transcript's clock, taken on its tick and when its state moves,
/// never as a frame is built: a running subagent's card says the time the clock says, and a
/// frame built again from scratch later says the same.
#[gpui::test]
fn a_running_card_says_the_clocks_time_in_every_frame(cx: &mut gpui::TestAppContext) {
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
        find(&draw(cx), Role::Button, "Subagent Count lines").map(|card| card.value.clone())
    };
    let first = value(cx).expect("the card");
    assert!(first.contains("42s"), "{first}");
    assert_eq!(value(cx).as_deref(), Some(first.as_str()), "drawn again, the same second");
}

/// slopty: on a touch screen a reply's controls take a finger's 44 pt press (#23): its tool
/// group's summary row and the strip's Copy; on a Mac each is its drawn size.
#[gpui::test]
fn a_replys_controls_take_a_fingers_press(cx: &mut gpui::TestAppContext) {
    let run = MessagePart::Tool {
        id: "run".into(),
        call: ToolCall::Exec { command: "cargo test".into() },
        is_error: false,
        resolved: true,
        output: None,
        diff: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: None,
        subagent_ref: None,
        subagent_status: None,
        subagent_tail: None,
        facts: None,
    };
    crate::shell::force_touch(true);
    let (transcript, cx) = open(cx, entry(MessageStatus::Complete, vec![run, text("All green.")]));
    let ours = |transcript: &Entity<Transcript>, cx: &mut VisualTestContext| {
        // The strip shows on hover; the iPad's last row shows it unasked.
        transcript.update(cx, |this, cx| {
            this.hovered_entry = Some(("reply".into(), "reply".into()));
            cx.notify();
        });
        let nodes = draw(cx);
        let buttons = || nodes.values().filter(|node| node.role == Role::Button);
        let names: Vec<_> = buttons().map(|node| node.label.clone()).collect();
        let group = buttons().find(|node| node.label.starts_with("Ran")).unwrap_or_else(|| panic!("{names:?}")).clone();
        let copy = find(&nodes, Role::Button, "Copy message").unwrap_or_else(|| panic!("{names:?}")).clone();
        (group.height, copy.height, copy.width)
    };
    let (group, copy, copy_width) = ours(&transcript, cx);
    assert!(group >= 43.9, "the summary row under a finger's 44 pt: {group}");
    assert!(copy.min(copy_width) >= 43.9, "Copy under a finger's 44 pt: {copy_width}x{copy}");

    crate::shell::force_touch(false);
    let (group, copy, copy_width) = ours(&transcript, cx);
    assert_eq!(group.round(), 26.0, "as drawn on a Mac");
    assert_eq!((copy_width.round(), copy.round()), (24.0, 24.0), "as drawn on a Mac");
}
