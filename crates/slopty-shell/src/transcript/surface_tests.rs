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
    // Its words name it: a reply's parts each have their own id.
    MessagePart::Text { id: format!("text:{text}").into(), text: text.into() }
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

/// [`open`], with the settled turn's divider opened, so its work's rows draw: a settled turn
/// folds them all behind it (Amp's settled turn).
fn open_turn(
    cx: &mut gpui::TestAppContext,
    reply: SessionMessageEntry,
) -> (Entity<Transcript>, &mut VisualTestContext) {
    let (transcript, cx) = open(cx, reply);
    unfold(&transcript, cx);
    (transcript, cx)
}

/// Opens every turn's divider that is shut.
fn unfold(transcript: &Entity<Transcript>, cx: &mut VisualTestContext) {
    transcript.update(cx, |this, cx| {
        let shut: Vec<gpui::SharedString> = this
            .rows
            .iter()
            .filter(|row| {
                matches!(row.kind, super::RowKind::ToolGroup { compact_shell: true, .. })
                    && this.folds.get(&row.id).and_then(|fold| fold.open) != Some(true)
            })
            .map(|row| row.id.clone())
            .collect();
        for id in shut {
            this.toggle_fold(id.clone(), 0.0, false, cx);
            // Open already, not opening, whatever motion the test turns on next.
            if let Some(fold) = this.folds.get_mut(&id) {
                fold.toggled_at = None;
                fold.disclosure_at = None;
            }
        }
        cx.notify();
    });
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
    let (transcript, cx) = open_turn(cx, entry(MessageStatus::Complete, vec![spawn(progress)]));
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

/// slopty: a subagent card's chevron takes no room at rest and arrives with the pointer, as
/// opencode's card action does; an open run keeps it after the pointer leaves.
#[gpui::test]
fn a_subagent_cards_chevron_arrives_with_the_pointer(cx: &mut gpui::TestAppContext) {
    let progress = SubagentProgress {
        running: true,
        state: SubagentProgress::RUNNING.into(),
        step: Some("Reading parser.rs".into()),
        chat_id: Some("child-chat".into()),
        row: Some("task:a1".into()),
        ..SubagentProgress::default()
    };
    let (_transcript, cx) = open_turn(cx, entry(MessageStatus::Complete, vec![spawn(progress)]));
    let nodes = draw(cx);
    let card = find(&nodes, Role::Button, "Subagent Count lines").expect("the card").clone();
    let width = |cx: &mut VisualTestContext| cx.debug_bounds("subagent-chevron").map_or(0.0, |b| f32::from(b.size.width));
    assert!(width(cx) < 0.5, "no room at rest");
    cx.simulate_mouse_move(card.center, None, gpui::Modifiers::default());
    draw(cx);
    assert!((width(cx) - 18.0).abs() < 0.5, "arrived under the pointer: {}", width(cx));
    cx.simulate_click(card.center, gpui::Modifiers::default());
    draw(cx);
    cx.simulate_mouse_move(gpui::point(px(2.0), px(2.0)), None, gpui::Modifiers::default());
    draw(cx);
    assert!((width(cx) - 18.0).abs() < 0.5, "kept while the run is open: {}", width(cx));
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
    let (transcript, cx) = open_turn(cx, entry(MessageStatus::Complete, vec![spawn(progress)]));
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

/// A background task's row opens the tray at its row, and names its task on the right unless
/// its words already do; a retry says what is tried again.
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
    let named = TaskRow {
        task: "t2".into(),
        row: "task:t2".into(),
        text: "Background command \"Build\" completed (exit code 0)".into(),
        title: "Build".into(),
        ..task.clone()
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
                MessagePart::Task { id: "named".into(), task: Box::new(named) },
                MessagePart::Note { id: "note".into(), note: Box::new(note) },
            ],
        ),
    );
    let nodes = draw(cx);
    let row = find(&nodes, Role::Button, "Background command finished · cargo test")
        .expect("the task's row")
        .clone();
    assert!(
        find(&nodes, Role::Button, "Background command \"Build\" completed (exit code 0)").is_some(),
        "words that name the task say it once"
    );
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
    let details = find(&nodes, Role::Button, "Show details").expect("Show details").clone();
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
    let (transcript, cx) = open_turn(cx, entry(MessageStatus::Complete, vec![spawn(progress)]));
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

/// One held prompt at most: a subagent's run open in its card, scrolled past its task so the
/// task would hold, holds none; the conversation's own prompt holds above it, alone.
#[gpui::test]
fn a_subagents_run_holds_no_prompt_of_its_own(cx: &mut gpui::TestAppContext) {
    let progress = SubagentProgress {
        running: false,
        state: SubagentProgress::DONE.into(),
        chat_id: Some("child-chat".into()),
        ..SubagentProgress::default()
    };
    // The card, then more of the answer, so the conversation scrolls past its prompt.
    let parts = std::iter::once(spawn(progress))
        .chain((0..40).map(|n| text(&format!("Paragraph {n} after the run."))))
        .collect();
    let (transcript, cx) = open_turn(cx, entry(MessageStatus::Complete, parts));
    let state = transcript.read_with(cx, |transcript, _| transcript.state.clone());
    let prompt = SessionMessageEntry {
        id: "prompt".into(),
        role: MessageRole::User,
        parts: vec![text("Survey the files")],
        ..entry(MessageStatus::Complete, Vec::new())
    };
    let task = SessionMessageEntry {
        id: "child:task".into(),
        role: MessageRole::User,
        parts: vec![MessagePart::Text { id: "child:task:t".into(), text: "You are lane C".into() }],
        ..entry(MessageStatus::Complete, Vec::new())
    };
    let said = |n: usize| SessionMessageEntry {
        id: format!("child:{n}"),
        parts: vec![MessagePart::Text { id: format!("child:{n}:t"), text: format!("Line {n} of the run.") }],
        ..entry(MessageStatus::Complete, Vec::new())
    };
    state.update(cx, |state, cx| {
        state.transcript.insert(0, prompt);
        state.transcript_revision += 1;
        let run = std::iter::once(task).chain((0..60).map(said)).collect();
        state.set_subagent_snapshot("child-chat".into(), run);
        cx.notify();
    });
    transcript.update(cx, |this, cx| {
        this.sync(cx);
        this.pinned = false;
        this.list.scroll_to(gpui::ListOffset { item_ix: 0, offset_in_item: px(0.0) });
        cx.notify();
    });
    let nodes = draw(cx);
    let card = find(&nodes, Role::Button, "Subagent Count lines").expect("the card").clone();
    cx.simulate_click(card.center, gpui::Modifiers::default());
    for _ in 0..4 {
        draw(cx);
    }
    // The card fills the view, the person's prompt scrolled away above it; the run scrolled on
    // past its task.
    transcript.update(cx, |this, cx| {
        this.list.scroll_to(gpui::ListOffset { item_ix: 1, offset_in_item: px(10.0) });
        cx.notify();
    });
    let child = transcript
        .read_with(cx, |transcript, _| transcript.subagents.child("child-chat"))
        .expect("open in place");
    child.update(cx, |this, cx| {
        this.list.scroll_to(gpui::ListOffset { item_ix: 20, offset_in_item: px(0.0) });
        cx.notify();
    });
    let nodes = (0..4).map(|_| draw(cx)).last().unwrap_or_default();
    assert_eq!(child.read_with(cx, |this, _| this.sticky_prompt()), Some(0), "its task would hold");
    assert_eq!(child.read_with(cx, |this, _| this.held_prompt()), None, "but its run holds none");
    let held: Vec<&str> = nodes
        .values()
        .filter(|node| node.label.starts_with("Back to your prompt"))
        .map(|node| node.label.as_str())
        .collect();
    assert_eq!(held, ["Back to your prompt: Survey the files"], "the person's prompt, alone");
}

/// A held prompt is a band across the transcript's top, flush under the titlebar: as wide as the
/// transcript, so nothing scrolling under it is cut but by one straight edge, and one line high.
#[gpui::test]
fn a_held_prompt_spans_the_top_as_part_of_the_page(cx: &mut gpui::TestAppContext) {
    let said = |n: usize| text(&format!("Paragraph {n} of the long answer."));
    let (transcript, cx) = open(cx, entry(MessageStatus::Complete, (0..80).map(said).collect()));
    let state = transcript.read_with(cx, |transcript, _| transcript.state.clone());
    let prompt = SessionMessageEntry {
        id: "prompt".into(),
        role: MessageRole::User,
        parts: vec![text("Explain the upload path\nin detail")],
        ..entry(MessageStatus::Complete, Vec::new())
    };
    state.update(cx, |state, cx| {
        state.transcript.insert(0, prompt);
        state.transcript_revision += 1;
        cx.notify();
    });
    transcript.update(cx, |this, cx| this.sync(cx));
    draw(cx);
    transcript.update(cx, |this, cx| {
        this.list.scroll_to(gpui::ListOffset { item_ix: 1, offset_in_item: px(300.0) });
        cx.notify();
    });
    let nodes = (0..3).map(|_| draw(cx)).last().unwrap_or_default();
    let band = find(&nodes, Role::Button, "Back to your prompt: Explain the upload path in detail")
        .expect("the prompt held");
    assert!((band.width - 1000.0).abs() <= 1.0, "as wide as the transcript: {band:?}");
    let top = f32::from(band.center.y) - band.height / 2.0;
    assert!((top - Theme::TITLEBAR_HEIGHT).abs() <= 0.5, "flush under the titlebar: {band:?}");
    assert!(band.height <= Transcript::sticky_height() + 0.5, "one line: {band:?}");
}

/// #11: on a phone the band stands under the navigation bar, whose title and subtitle reach
/// lower than a Mac's titlebar, never over them.
#[gpui::test]
fn a_phones_held_prompt_stands_under_its_navigation_bar(cx: &mut gpui::TestAppContext) {
    let said = |n: usize| text(&format!("Paragraph {n} of the long answer."));
    let (transcript, cx) = open(cx, entry(MessageStatus::Complete, (0..80).map(said).collect()));
    crate::shell::force_touch(true);
    cx.simulate_resize(gpui::size(px(402.0), px(874.0)));
    let state = transcript.read_with(cx, |transcript, _| transcript.state.clone());
    let prompt = SessionMessageEntry {
        id: "prompt".into(),
        role: MessageRole::User,
        parts: vec![text("Explain the upload path")],
        ..entry(MessageStatus::Complete, Vec::new())
    };
    state.update(cx, |state, cx| {
        state.transcript.insert(0, prompt);
        state.transcript_revision += 1;
        cx.notify();
    });
    transcript.update(cx, |this, cx| this.sync(cx));
    draw(cx);
    transcript.update(cx, |this, cx| {
        this.list.scroll_to(gpui::ListOffset { item_ix: 1, offset_in_item: px(300.0) });
        cx.notify();
    });
    let nodes = (0..3).map(|_| draw(cx)).last().unwrap_or_default();
    crate::shell::force_touch(false);
    let band = find(&nodes, Role::Button, "Back to your prompt: Explain the upload path")
        .expect("the prompt held");
    let top = f32::from(band.center.y) - band.height / 2.0;
    let bar = Theme::TITLEBAR_HEIGHT + Theme::TITLEBAR_TOP_PAD;
    assert!((top - bar).abs() <= 0.5, "under the phone's bar ({bar}): {band:?}");
}

/// slopty: a list held at its end, as a finished turn rests, holds its prompt once the prompt
/// is above the band: the list keeps no top row there, and the band reads where its rows stand
/// all the same (#18). Short enough to fit, it holds none.
#[gpui::test]
fn a_list_held_at_its_end_holds_the_prompt_it_scrolled_past(cx: &mut gpui::TestAppContext) {
    let said = |n: usize| text(&format!("Paragraph {n} of the long answer."));
    let (transcript, cx) = open(cx, entry(MessageStatus::Complete, (0..80).map(said).collect()));
    cx.simulate_resize(gpui::size(px(1320.0), px(880.0)));
    let state = transcript.read_with(cx, |transcript, _| transcript.state.clone());
    let prompt = SessionMessageEntry {
        id: "prompt".into(),
        role: MessageRole::User,
        parts: vec![text("Survey the files")],
        ..entry(MessageStatus::Complete, Vec::new())
    };
    state.update(cx, |state, cx| {
        state.transcript.insert(0, prompt.clone());
        state.transcript_revision += 1;
        cx.notify();
    });
    transcript.update(cx, |this, cx| this.sync(cx));
    let nodes = (0..3).map(|_| draw(cx)).last().unwrap_or_default();
    assert!(
        transcript.read_with(cx, |this, _| this.is_glued()),
        "the list rests at its end, with no top row"
    );
    assert!(find(&nodes, Role::Button, "Back to your prompt: Survey the files").is_some(), "the prompt held");
    // One short answer under it fits, and nothing is held.
    state.update(cx, |state, cx| {
        state.transcript = vec![prompt, entry(MessageStatus::Complete, vec![text("Done.")])];
        state.transcript_revision += 1;
        cx.notify();
    });
    transcript.update(cx, |this, cx| this.sync(cx));
    let nodes = (0..3).map(|_| draw(cx)).last().unwrap_or_default();
    assert!(!nodes.values().any(|node| node.label.starts_with("Back to your prompt")), "all of it fits");
}

/// The rewind menu hangs from the person's bubble's left edge, under it, not from wherever the
/// press landed (flush with its right edge under a narrower bubble); its note says what its first row does, then what the row under the pointer
/// does (#20).
#[gpui::test]
fn the_rewind_menu_hangs_from_the_bubble_and_says_what_its_row_does(cx: &mut gpui::TestAppContext) {
    // A long answer under the message, read from its top, so the menu has room under it.
    let said = (0..40).map(|n| text(&format!("Paragraph {n} of the answer."))).collect();
    let (transcript, cx) = open(cx, entry(MessageStatus::Complete, said));
    cx.simulate_resize(gpui::size(px(1600.0), px(800.0)));
    let state = transcript.read_with(cx, |transcript, _| transcript.state.clone());
    let prompt = SessionMessageEntry {
        id: "prompt".into(),
        role: MessageRole::User,
        parts: vec![text("Write the notes file")],
        created_at: 1,
        ..entry(MessageStatus::Complete, Vec::new())
    };
    state.update(cx, |state, cx| {
        state.transcript.insert(0, prompt);
        state.transcript_revision += 1;
        cx.notify();
    });
    transcript.update(cx, |this, cx| {
        this.chat_id = Some("chat".into());
        let can = slopty_view_doc::dock::DockCan { rewind: true, fork: true, ..Default::default() };
        this.let_rewind("chat", can);
        this.sync(cx);
        this.pinned = false;
        this.list.scroll_to(gpui::ListOffset { item_ix: 0, offset_in_item: px(0.0) });
        cx.notify();
    });
    let nodes = draw(cx);
    let bubble = find(&nodes, Role::Paragraph, "Write the notes file").expect("the bubble").clone();
    let rewind = find(&nodes, Role::Button, "Rewind…").expect("its Rewind").clone();
    // The pointer comes over the bubble's foot, as a person's does, and the strip shows.
    cx.simulate_mouse_move(rewind.center, None, gpui::Modifiers::default());
    draw(cx);
    cx.simulate_click(rewind.center, gpui::Modifiers::default());
    let nodes = draw(cx);
    let menu = find(&nodes, Role::Menu, "Rewind to before this message").expect("the menu").clone();
    let left = |node: &Node| f32::from(node.center.x) - node.width / 2.0;
    let top = |node: &Node| f32::from(node.center.y) - node.height / 2.0;
    let bottom = |node: &Node| f32::from(node.center.y) + node.height / 2.0;
    // A bubble narrower than the menu has it flush with its right edge, in the column.
    let right = |node: &Node| f32::from(node.center.x) + node.width / 2.0;
    let edge = left(&bubble).min(right(&bubble) - menu.width);
    assert!((left(&menu) - edge).abs() <= 1.0, "from the bubble's edge: {menu:?} {bubble:?}");
    assert!(top(&menu) >= bottom(&bubble), "under it: {menu:?} {bubble:?}");
    let note = |cx: &mut VisualTestContext| transcript.read_with(cx, |this, _| this.rewind_note());
    let first = note(cx).expect("a note");
    assert!(first.starts_with("Files and the chat go back"), "{first}");
    let row = find(&nodes, Role::MenuItem, "Conversation only").expect("the chat's row").clone();
    cx.simulate_mouse_move(row.center, None, gpui::Modifiers::default());
    draw(cx);
    assert_eq!(note(cx).as_deref(), Some("The chat goes back; files stay as they are."));
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
    let (transcript, cx) = open_turn(cx, entry(MessageStatus::Complete, vec![spawn(progress)]));
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
    let (transcript, cx) = open_turn(cx, entry(MessageStatus::Complete, vec![spawn(progress)]));
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
/// article "You" holding a paragraph of their words, the reply an article named by its agent
/// ("Agent" when the chat names none) at its first row, and each of the reply's blocks a
/// paragraph of its plain text.
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
    // slopty: a fork's seam is the app's, not a message the agent said (#8).
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
            state.transcript_revision += 1;
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
    let (transcript, cx) = open_turn(cx, entry(MessageStatus::Complete, vec![spawn(progress)]));
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
    let (transcript, cx) = open_turn(
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
        cx.simulate_event(gpui::ScrollEvent {
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
    let (transcript, cx) = open_turn(cx, entry(MessageStatus::Complete, vec![spawn(progress)]));
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
fn the_controls_of_a_reply_take_a_fingers_press(cx: &mut gpui::TestAppContext) {
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
    // slopty: two calls before a reply, so the settled turn draws its divider, the summary.
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
        let group = buttons().find(|node| node.label.starts_with("Ran")).unwrap_or_else(|| panic!("{names:?}")).clone();
        let copy = find(&nodes, Role::Button, "Copy message").unwrap_or_else(|| panic!("{names:?}")).clone();
        (group.height, copy.height, copy.width)
    };
    let (group, copy, copy_width) = ours(&transcript, cx);
    assert!(group >= 43.9, "the divider under a finger's 44 pt: {group}");
    assert!(copy.min(copy_width) >= 43.9, "Copy under a finger's 44 pt: {copy_width}x{copy}");

    crate::shell::force_touch(false);
    let (group, copy, copy_width) = ours(&transcript, cx);
    assert_eq!(group.round(), 22.0, "the divider's button as drawn on a Mac");
    assert_eq!((copy_width.round(), copy.round()), (24.0, 24.0), "as drawn on a Mac");
}

// ---------------------------------------------------------------------------
// slopty: the transcript's polish (`docs/decisions/ui.md`, "The transcript reads as one
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
        diff: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: None,
        subagent_ref: None,
        subagent_status: None,
        subagent_tail: None,
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
        continuation_of: None,
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
            state.transcript_revision += 1;
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
        update.nodes.iter().find(|(id, _)| *id == update.focus).map(|(_, node)| {
            node.label().unwrap_or_default().to_owned()
        })
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
    cx.update(|window, cx| window.focus_next(cx));
}

fn top(node: &Node) -> f32 {
    f32::from(node.center.y) - node.height / 2.0
}

fn bottom(node: &Node) -> f32 {
    f32::from(node.center.y) + node.height / 2.0
}

/// #1: a prompt heads its turn: the big step comes before it, its answer starts a turn gap under
/// it, and under a pointer its strip waits beside the bubble's foot, taking no room.
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
    let second = find(&nodes, Role::Paragraph, "Second answer.").expect("the second answer").clone();
    let after = top(&second) - bottom(&asked);
    let before = top(&asked) - bottom(&first);
    assert!((after - Theme::SPACE_LG).abs() <= 1.0, "16 pt under the prompt: {after}");
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
    let (transcript, cx) = open_turn(
        cx,
        entry(MessageStatus::Complete, vec![exec("one", "ls missing-file", true, false), text("Then:")]),
    );
    let nodes = draw(cx);
    assert!(
        nodes.values().any(|node| node.label == "Run ls missing-file"),
        "the call by its words"
    );
    assert!(
        !nodes.values().any(|node| node.label.starts_with("Ran ")),
        "no summary over one call"
    );
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

/// #12: a turn that ended on a call that never settled says it stopped, never that it runs.
#[test]
fn a_finished_turns_summary_never_says_running() {
    let reply = entry(
        MessageStatus::Complete,
        vec![exec("one", "cargo fmt", true, false), exec("two", "cargo test", false, false)],
    );
    let rows = super::work_rows(&reply, false, &mut |_, text| {
        Arc::new(crate::markdown::parse_full(text))
    });
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
            vec![exec("one", "cargo fmt", true, false), exec("two", "touch refused.txt", false, false), ask],
        )],
        cx,
    );
    let labels: Vec<String> = nodes.values().map(|n| n.label.clone()).collect();
    assert!(
        find(&nodes, Role::Button, "Ran 1 command and waiting for approval (run touch refused.txt)")
            .is_some(),
        "{labels:?}"
    );
    // #8: the request's chip names what it waits for and shows the command, not its header.
    assert!(
        find(&nodes, Role::Status, "Needs your approval: touch refused.txt").is_some(),
        "{labels:?}"
    );
}

/// #4: a turn ends on its done line, after what it changed, and its strip does not say the time
/// again.
#[test]
fn a_turn_ends_on_its_done_line() {
    let edit = MessagePart::Tool {
        id: "edit".into(),
        call: ToolCall::WriteFile { path: "/work/app/main.rs".into(), content: None },
        is_error: false,
        resolved: true,
        output: None,
        diff: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: Some(vec![slopty_view_doc::ToolDiffStat {
            path: "/work/app/main.rs".into(),
            additions: 2,
            deletions: 0,
        }]),
        subagent_ref: None,
        subagent_status: None,
        subagent_tail: None,
        facts: None,
    };
    let done = MessagePart::Note {
        id: "reply:done".into(),
        note: Box::new(NoteRow {
            kind: "done".into(),
            text: "Worked for 4s · done 9:00 AM".into(),
            tone: "info".into(),
            ..NoteRow::default()
        }),
    };
    let reply = SessionMessageEntry {
        created_at: 1_760_000_000_000,
        ..entry(MessageStatus::Complete, vec![edit, text("done"), done])
    };
    let rows = super::work_rows(&reply, false, &mut |_, text| {
        Arc::new(crate::markdown::parse_full(text))
    });
    let kinds: Vec<&str> = rows
        .iter()
        .map(|row| match &row.kind {
            super::RowKind::TurnChanges { .. } => "changes",
            super::RowKind::Notice { .. } => "done",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds.last(), Some(&"done"), "the done line last: {kinds:?}");
    assert_eq!(kinds.iter().rev().nth(1), Some(&"changes"), "the files above it: {kinds:?}");
    assert_eq!(rows.last().and_then(|row| row.timestamp), Some(0), "the time said once");
}

/// #7: a turn that ends on its done line has its Copy at the end of the line's words, on the
/// line, not alone in a lane under it.
#[gpui::test]
fn a_done_lines_copy_rides_on_its_line(cx: &mut gpui::TestAppContext) {
    let done = MessagePart::Note {
        id: "reply:done".into(),
        note: Box::new(NoteRow {
            kind: "done".into(),
            text: "Worked for 4s · done 9:00 AM".into(),
            tone: "info".into(),
            ..NoteRow::default()
        }),
    };
    let reply = SessionMessageEntry {
        created_at: 1_760_000_000_000,
        ..entry(MessageStatus::Complete, vec![text("All done."), done])
    };
    let (_, cx) = open(cx, reply);
    let nodes = draw(cx);
    let line = find(&nodes, Role::Status, "Worked for 4s · done 9:00 AM").expect("the done line");
    let copy = find(&nodes, Role::Button, "Copy message").expect("its Copy");
    assert!(
        (copy.center.y - line.center.y).abs() < px(2.0),
        "on the line: {:?} vs {:?}",
        copy.center,
        line.center
    );
    assert!(copy.center.x > line.center.x - px(line.width / 2.0), "after its words");
    assert!(
        descendants(&nodes, line, Role::Button).iter().any(|b| b.label == "Copy message"),
        "the line's own control"
    );
}

/// #5: a subagent's run drilled into as a chat reads from the top, not risen from the bottom,
/// starting under the breadcrumb.
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
            state.transcript_revision += 1;
        });
        this.sync(cx);
    });
    let nodes = draw(cx);
    let said = find(&nodes, Role::Paragraph, "It has 4.").expect("the run's answer").clone();
    assert!(f32::from(said.center.y) < 300.0, "near the top of an 800 pt pane: {said:?}");
    // Its task starts under the breadcrumb that hangs below the titlebar, clear of it.
    let task = find(&nodes, Role::Paragraph, "Task: Count the lines").expect("its task").clone();
    let top = f32::from(task.center.y) - task.height / 2.0;
    let crumb = Theme::TITLEBAR_HEIGHT + super::DRILL_CRUMB_HEIGHT;
    assert!(top >= crumb + Theme::SPACE_SM, "under the breadcrumb's {crumb}: {task:?}");
}

/// #17: a subagent's run drilled into before the chat list lists it (a note tapped at launch)
/// holds no prompt while it is unknown, and reads from the top once it is listed.
#[gpui::test]
fn a_run_listed_after_it_opened_reads_as_one(cx: &mut gpui::TestAppContext) {
    let (transcript, cx) = open(cx, answer("a1", "Main answer."));
    transcript.update(cx, |this, cx| {
        this.state.update(cx, |state, _| {
            state.selected_chat = Some("child".into());
            state.transcript = vec![prompt("task", "Count the lines"), answer("a2", "It has 4.")];
            state.transcript_revision += 1;
        });
        this.sync(cx);
    });
    draw(cx);
    let (top, holds) = transcript.read_with(cx, |this, _| (this.list_top, this.holds_prompts));
    assert!(!top && !holds, "unknown: no band from its task, laid as a chat");
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
    let (top, holds) = transcript.read_with(cx, |this, _| (this.list_top, this.holds_prompts));
    assert!(top && !holds, "listed as a run: from the top, holding none");
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
    let want = 5.0 * super::USER_LINE_HEIGHT + 4.0 + 18.0 + 20.0;
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
        subagent_tail: None,
        kind: super::ToolItemKind::Call,
        images: Arc::new([]),
        facts: None,
    };
    assert!(!super::fails_loudly(&failed, true), "an activity row's words stay quiet");
    assert!(super::fails_loudly(&failed, false), "a card, with no rail, keeps the tint");
}

/// #9: Tab reaches a reply's controls: its group's summary, its call rows, the subagent card,
/// the files card and its Open diff, and the strip's Copy, which a hover alone showed before.
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
        diff: None,
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
        subagent_tail: None,
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
    // The settled turn's divider stands for its work, then what it changed and its strip.
    let reached = reach(cx);
    for want in ["Wrote 2 notes, ran 2 commands", "2 files changed", "Open diff", "Copy message"] {
        assert!(reached.iter().any(|label| label.starts_with(want)), "{want}: {reached:?}");
    }
    // Enter opens the divider, as a click does, and its work's controls join the order.
    while !focused(cx).starts_with("Wrote 2 notes") {
        tab(cx);
    }
    press_enter(cx);
    let nodes = draw(cx);
    let divider = nodes.values().find(|n| n.label.starts_with("Wrote 2 notes")).expect("the divider");
    assert_eq!(divider.expanded, Some(true));
    let reached = reach(cx);
    for want in ["Ran 2 commands", "Subagent Count lines"] {
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
            vec![exec("one", "cargo fmt", true, false), exec("two", "cargo check", true, false), text("Listed.")],
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

/// #11: a send that never arrived says so under its bubble, with one quiet Retry.
#[gpui::test]
fn an_undelivered_send_offers_retry_under_its_bubble(cx: &mut gpui::TestAppContext) {
    let (transcript, cx) = open(cx, answer("a1", "Earlier."));
    transcript.update(cx, |this, cx| {
        this.state.update(cx, |state, _| {
            state.begin_pending_send("chat", "p1", chrono::Utc::now() - chrono::TimeDelta::hours(1));
        });
        cx.notify();
    });
    let nodes = show(&transcript, vec![answer("a1", "Earlier."), prompt("p1", "Hello?")], cx);
    let labels: Vec<String> = nodes.values().map(|n| n.label.clone()).collect();
    let failed = find(&nodes, Role::Status, "Not sent").unwrap_or_else(|| panic!("{labels:?}")).clone();
    let retry = find(&nodes, Role::Button, "Retry").expect("Retry").clone();
    let bubble = find(&nodes, Role::Paragraph, "Hello?").expect("the bubble").clone();
    let right = |node: &Node| f32::from(node.center.x) + node.width / 2.0;
    assert!((right(&retry) - right(&bubble)).abs() <= 1.0, "under the bubble's right edge");
    assert!(top(&failed) >= bottom(&bubble), "under the bubble");
}

/// #12: the files card names a lone file in itself; several open under a chevron, tweened.
#[gpui::test]
fn the_files_card_names_a_lone_file(cx: &mut gpui::TestAppContext) {
    let edit = |id: &str, path: &str| MessagePart::Tool {
        id: id.into(),
        call: ToolCall::WriteFile { path: path.into(), content: None },
        is_error: false,
        resolved: true,
        output: None,
        diff: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: Some(vec![slopty_view_doc::ToolDiffStat {
            path: path.into(),
            additions: 2,
            deletions: 0,
        }]),
        subagent_ref: None,
        subagent_status: None,
        subagent_tail: None,
        facts: None,
    };
    let (transcript, cx) =
        open(cx, entry(MessageStatus::Complete, vec![edit("e1", "/work/app/main.rs"), text("Done.")]));
    let nodes = draw(cx);
    let card = find(&nodes, Role::Group, "main.rs +2 −0").expect("the lone file, named").clone();
    assert_eq!(card.expanded, None, "nothing to open");
    let nodes = show(
        &transcript,
        vec![entry(
            MessageStatus::Complete,
            vec![edit("e1", "/work/app/main.rs"), edit("e2", "/work/app/lib.rs"), text("Done.")],
        )],
        cx,
    );
    let card = find(&nodes, Role::Button, "2 files changed +4 −0").expect("the card").clone();
    assert_eq!(card.expanded, Some(false));
    cx.simulate_click(card.center, gpui::Modifiers::default());
    let nodes = draw(cx);
    assert_eq!(find(&nodes, Role::Button, "2 files changed +4 −0").and_then(|c| c.expanded), Some(true));
    assert!(find(&nodes, Role::ListItem, "lib.rs +2 −0").is_some(), "its files listed");
}

/// #15: the person's own send rises and fades in as its bubble first draws, and only once; a
/// bubble that sends fades up from pending rather than snapping to full.
#[gpui::test]
fn an_own_send_rises_in_once(cx: &mut gpui::TestAppContext) {
    let (transcript, cx) = open(cx, answer("a1", "Earlier."));
    cx.update(|_, cx| cx.set_reduce_motion(false));
    transcript.update(cx, |this, cx| this.on_own_send("chat".into(), "p1".into(), cx));
    let entries = || vec![answer("a1", "Earlier."), prompt("p1", "Hello?")];
    let gap = |nodes: &HashMap<NodeId, Node>| {
        let earlier = find(nodes, Role::Paragraph, "Earlier.").expect("the answer").clone();
        let bubble = find(nodes, Role::Paragraph, "Hello?").expect("the bubble").clone();
        top(&bubble) - bottom(&earlier)
    };
    let entering = gap(&show(&transcript, entries(), cx));
    cx.executor().advance_clock(std::time::Duration::from_millis(400));
    let landed = gap(&draw(cx));
    assert!(
        (entering - landed - super::SEND_RISE).abs() <= 0.5,
        "it rises {} pt: {entering} then {landed}",
        super::SEND_RISE
    );
    transcript.update(cx, |this, cx| {
        assert!(!this.bubble_enters(&"p1".into(), cx), "once: a later draw stays put");
        assert!(!this.bubble_enters(&"a1".into(), cx), "an answer never comes in so");
        let row: gpui::SharedString = "p1#row".into();
        assert!(!this.bubble_sent(&row, true, cx), "pending holds its dim");
        assert!(this.bubble_sent(&row, false, cx), "sent, it fades up");
        assert!(!this.bubble_sent(&"a1#row".into(), false, cx), "never pending, nothing fades");
    });
    cx.executor().advance_clock(std::time::Duration::from_millis(200));
    draw(cx);
    transcript.update(cx, |this, cx| {
        assert!(!this.bubble_sent(&"p1#row".into(), false, cx), "full once faded");
    });
}

/// #24: a run read in its card speaks at the card's compact body, a 20 pt line under its 12 pt
/// title; the reply around the card keeps its 22.
#[gpui::test]
fn a_run_in_its_card_reads_compact(cx: &mut gpui::TestAppContext) {
    let progress = SubagentProgress {
        running: false,
        state: SubagentProgress::DONE.into(),
        chat_id: Some("child-chat".into()),
        ..SubagentProgress::default()
    };
    let (transcript, cx) =
        open_turn(cx, entry(MessageStatus::Complete, vec![text("Counting."), spawn(progress)]));
    let said = SessionMessageEntry {
        id: "child:0".into(),
        parts: vec![MessagePart::Text { id: "child:0:t".into(), text: "Four.".into() }],
        ..entry(MessageStatus::Complete, Vec::new())
    };
    let state = transcript.read_with(cx, |transcript, _| transcript.state.clone());
    state.update(cx, |state, _| state.set_subagent_snapshot("child-chat".into(), vec![said]));
    let nodes = draw(cx);
    let card = find(&nodes, Role::Button, "Subagent Count lines").expect("the card").clone();
    cx.simulate_click(card.center, gpui::Modifiers::default());
    let nodes = (0..4).map(|_| draw(cx)).last().unwrap_or_default();
    let child = find(&nodes, Role::Paragraph, "Four.").expect("the run's words").clone();
    let parent = find(&nodes, Role::Paragraph, "Counting.").expect("the reply's words").clone();
    assert!((child.height - super::render::MD_COMPACT_LINE_HEIGHT).abs() <= 0.5, "{child:?}");
    assert!((parent.height - super::render::MD_LINE_HEIGHT).abs() <= 0.5, "{parent:?}");
}

/// #14: a call under way sweeps its words with its group title's light; a settled one, a
/// thought and a subagent's spawn, whose card says it runs, do not. (The headless text system
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
    let rows = super::work_rows(&entry, false, &mut |_, text| {
        Arc::new(crate::markdown::parse_full(text))
    });
    let tools: Vec<_> = rows
        .iter()
        .filter_map(|row| match &row.kind {
            super::RowKind::ToolGroup { tools, .. } => Some(tools.iter().cloned()),
            _ => None,
        })
        .flatten()
        .collect();
    let swept: Vec<bool> = tools.iter().map(super::sweeps).collect();
    assert_eq!(swept, [true, false, false], "{tools:?}");
}

/// #18: a fetch that failed says so in the transcript's voice, and offers one Retry that says
/// what it fetches again.
#[gpui::test]
fn a_failed_fetch_says_so_and_offers_retry(cx: &mut gpui::TestAppContext) {
    let mut call = exec("one", "cargo test", true, false);
    if let MessagePart::Tool { output, output_ref, output_bytes, .. } = &mut call {
        *output = Some("test result: ok".into());
        *output_ref = Some("blob-1".into());
        *output_bytes = Some(12_000);
    }
    let (transcript, cx) = open_turn(cx, entry(MessageStatus::Complete, vec![call, text("Done.")]));
    transcript.update(cx, |this, cx| {
        this.blob_details.insert("blob-1".into(), super::BlobFetch::Failed);
        cx.notify();
    });
    let nodes = draw(cx);
    let row = find(&nodes, Role::Button, "Run cargo test").expect("the call").clone();
    cx.simulate_click(row.center, gpui::Modifiers::default());
    let nodes = draw(cx);
    let labels: Vec<String> = nodes.values().map(|n| n.label.clone()).collect();
    assert!(
        find(&nodes, Role::Button, "Retry loading the full output").is_some(),
        "{labels:?}"
    );
}
