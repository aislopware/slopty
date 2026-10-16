//! slopty: the transcript's own rows as assistive tech reads them: a subagent's card, the rows
//! of background work, a reply's images and formulas. Each test draws a real transcript in a
//! test window and judges by its accessibility tree.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

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

/// A settled formula typesets taller than a line, labelled with its TeX; a broken one shows its
/// source with the value "error"; the streaming tail keeps text.
#[gpui::test]
fn display_formulas_typeset_once_settled(cx: &mut gpui::TestAppContext) {
    let (transcript, cx) = open(
        cx,
        entry(MessageStatus::Complete, vec![text("$$\\frac{a}{b}$$\n\nthen\n\n$$\\nosuchcommand{x}$$")]),
    );
    let nodes = draw(cx);
    let formula = find(&nodes, Role::Math, "\\frac{a}{b}").expect("the formula");
    assert!(formula.value.is_empty());
    assert!(formula.height > 22.0, "taller than a line: {formula:?}");
    let broken = find(&nodes, Role::Math, "\\nosuchcommand{x}").expect("the broken formula");
    assert_eq!(broken.value, "error");

    // The same reply still streaming: its last block draws no formula yet.
    transcript.update(cx, |this, cx| {
        this.state.update(cx, |state, _| {
            state.transcript = vec![entry(MessageStatus::Streaming, vec![text("$$\\frac{a}{b}$$")])];
            state.transcript_revision += 1;
        });
        this.sync(cx);
    });
    let nodes = draw(cx);
    assert!(nodes.values().all(|node| node.role != Role::Math), "the tail keeps text");
}

/// slopty: a failed turn's card offers its error to copy, the agent's words with its detail, and
/// the detail opens in a box that scrolls (T51).
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
}
