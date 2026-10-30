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
            let TranscriptEvent::OpenSubagent { chat_id } = event;
            opened.borrow_mut().push(chat_id.clone());
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
    let (_transcript, cx) = open(cx, entry(MessageStatus::Complete, vec![spawn(progress)]));
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
    let (transcript, cx) = open(cx, entry(MessageStatus::Complete, vec![spawn(progress)]));
    cx.simulate_resize(gpui::size(px(390.0), px(800.0)));
    let opened = Rc::new(RefCell::new(Vec::new()));
    let _events = cx.update(|_, cx| {
        let opened = opened.clone();
        cx.subscribe(&transcript, move |_, event: &TranscriptEvent, _| {
            let TranscriptEvent::OpenSubagent { chat_id } = event;
            opened.borrow_mut().push(chat_id.clone());
        })
    });
    let nodes = draw(cx);
    let card = find(&nodes, Role::Button, "Subagent Count lines").expect("the card").clone();
    assert_eq!(card.expanded, None, "a phone's card does not open in place: {card:?}");
    cx.simulate_click(card.center, gpui::Modifiers::default());
    draw(cx);
    assert_eq!(*opened.borrow(), ["child-chat"], "the tap drills in");
}

/// The footer a transcript ends with (the inspector's mirror of the prompt box): a box of its own.
struct Footer;

impl Render for Footer {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        div().id("footer").role(gpui::Role::Group).aria_label("Footer").w_full().h(px(48.0))
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
    let following = |cx: &mut VisualTestContext| transcript.read_with(cx, |transcript, _| transcript.pinned);
    let nodes = draw(cx);
    assert!(following(cx), "a session opens following its end");
    let at = find(&nodes, Role::Group, "Footer").expect("the footer under the last row").center;
    cx.simulate_click(at, gpui::Modifiers::default());
    let nodes = draw(cx);
    assert!(following(cx), "a press on the footer leaves it following");
    let row = find(&nodes, Role::Paragraph, "Paragraph 59 of a long reply.").expect("the last row").center;
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
    let row = find(&nodes, Role::Status, "Background command finished · cargo test")
        .expect("the task's row")
        .clone();
    assert!(
        find(&nodes, Role::Status, "Background command \"Build\" completed (exit code 0)").is_some(),
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
    let (transcript, cx) = open(cx, entry(MessageStatus::Complete, vec![spawn(progress)]));
    let opened = Rc::new(RefCell::new(Vec::new()));
    let _events = cx.update(|_, cx| {
        let opened = opened.clone();
        cx.subscribe(&transcript, move |_, event: &TranscriptEvent, _| {
            let TranscriptEvent::OpenSubagent { chat_id } = event;
            opened.borrow_mut().push(chat_id.clone());
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
    // slopty: two calls before a reply, so their group draws its summary row.
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
        let group = buttons().find(|node| node.label.starts_with("Ran 2 commands")).unwrap_or_else(|| panic!("{names:?}")).clone();
        let copy = find(&nodes, Role::Button, "Copy message").unwrap_or_else(|| panic!("{names:?}")).clone();
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
    let (transcript, cx) = open(
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

/// A call pressed open says its result to assistive tech, whole, as its code block draws it.
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
        nodes.values().any(|node| node.role == Role::Code && node.label.contains(SAID))
    };
    let nodes = draw(cx);
    assert!(!says(&nodes), "folded to its line");
    let line = find(&nodes, Role::Button, "MCP slopty · machines").expect("its line").clone();
    cx.simulate_click(line.center, gpui::Modifiers::default());
    let nodes = draw(cx);
    let words: Vec<_> = nodes.values().map(|node| (node.role, node.label.as_str())).collect();
    assert!(says(&nodes), "its result, whole: {words:?}");
}

/// #12: a turn that ended on a call that never settled says it stopped, never that it runs.
#[test]
fn a_finished_turns_summary_never_says_running() {
    let reply = entry(
        MessageStatus::Complete,
        vec![exec("one", "cargo fmt", true, false), exec("two", "cargo test", false, false)],
    );
    let rows = super::rows_for_entry(&reply, false, &mut |_, text| {
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
        find(&nodes, Role::Button, "Ran 1 command and waiting for approval (run touch refused.txt)").is_some(),
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
    let rows = super::rows_for_entry(&reply, false, &mut |_, text| {
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
            state.transcript_revision += 1;
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
            state.transcript_revision += 1;
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
/// the files card, and the strip's Copy, which a hover alone showed before.
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
    // The settled turn's steps each take the keyboard where they were said, then what it changed
    // and its strip.
    let reached = reach(cx);
    for want in ["Ran 2 commands", "Subagent Count lines", "2 files changed", "Copy message"] {
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

/// #15: a bubble that sends fades up from pending rather than snapping to full, once.
#[gpui::test]
fn a_sent_bubble_fades_up_from_pending(cx: &mut gpui::TestAppContext) {
    let (transcript, cx) = open(cx, answer("a1", "Earlier."));
    cx.update(|_, cx| cx.set_reduce_motion(false));
    transcript.update(cx, |this, cx| {
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
        open(cx, entry(MessageStatus::Complete, vec![text("Counting."), spawn(progress)]));
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
    let rows = super::rows_for_entry(&entry, false, &mut |_, text| {
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
    let (transcript, cx) = open(cx, entry(MessageStatus::Complete, vec![call, text("Done.")]));
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
