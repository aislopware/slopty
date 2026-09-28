//! The face in a headless window over the recorded conversations: where the list sits as the
//! conversation grows and is replayed, and what the keys do.

use gpui::{
    Entity, Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase,
    VisualTestContext, point, px, size,
};
use slopty_core::SessionId;
use slopty_proto::conversation::ConversationEvent;
use slopty_theme::Theme;

use super::ConversationView;
use crate::conversation::fixtures;
use crate::conversation::rows::Density;

/// A face 800 × 400 points over the recorded session `name`, its composer focused.
fn face<'a>(
    cx: &'a mut TestAppContext,
    name: &str,
) -> (Entity<ConversationView>, &'a mut VisualTestContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.bind_keys(crate::workspace::key_bindings());
    });
    let (view, cx) = cx.add_window_view(|window, cx| {
        let mut face = ConversationView::new(SessionId::new(), Theme::default(), window, cx);
        face.set_shown(true, cx);
        face.focus(window, cx);
        face
    });
    cx.simulate_resize(size(px(800.0), px(400.0)));
    feed(&view, cx, fixtures::events(name));
    (view, cx)
}

fn feed(
    view: &Entity<ConversationView>,
    cx: &mut VisualTestContext,
    events: Vec<ConversationEvent>,
) {
    view.update(cx, |v, cx| {
        for event in events {
            v.apply(event, cx);
        }
    });
    cx.run_until_parked();
}

fn scroll(cx: &mut VisualTestContext, dy: f32) {
    let at = cx.debug_bounds("conversation").expect("drawn").center();
    cx.simulate_event(ScrollWheelEvent {
        position: at,
        delta: ScrollDelta::Pixels(point(px(0.0), px(dy))),
        modifiers: Modifiers::default(),
        touch_phase: TouchPhase::Moved,
    });
    cx.run_until_parked();
}

/// The list opens on the newest row and stays there as rows arrive; scrolled up, it holds its
/// place through a whole replay (a re-follow after a toggle) and offers the way back, which
/// follows the tail again.
#[gpui::test]
fn the_list_follows_the_tail_and_holds_its_place_through_a_replay(cx: &mut TestAppContext) {
    let (view, cx) = face(cx, "tools");
    // Every step shown, so the list is taller than the face.
    cx.simulate_keystrokes("ctrl-o ctrl-o");
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.following()), "opens on the newest row");
    assert!(cx.debug_bounds("conversation-latest").is_none());

    scroll(cx, 200.0);
    assert!(!view.read_with(cx, |v, _| v.following()), "scrolling up lets go of the tail");
    let place = |cx: &mut VisualTestContext| {
        let top = view.read_with(cx, |v, _| v.anchor());
        (top.item_ix, top.offset_in_item)
    };
    let anchor = place(cx);
    assert!(cx.debug_bounds("conversation-latest").is_some(), "the way back shows");

    feed(&view, cx, fixtures::events("tools"));
    assert_eq!(place(cx), anchor, "a replay keeps the place");
    assert!(!view.read_with(cx, |v, _| v.following()));

    let back = cx.debug_bounds("conversation-latest").expect("the way back").center();
    cx.simulate_click(back, Modifiers::none());
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.following()), "following again");
}

/// ⌃O steps through the densities, each showing more of the work: Thinking adds the model's
/// thinking inside the turns that are open, Verbose opens every settled turn.
#[gpui::test]
fn ctrl_o_steps_through_the_densities(cx: &mut TestAppContext) {
    let (view, cx) = face(cx, "tools");
    let count =
        |cx: &mut VisualTestContext| view.read_with(cx, |v, _| (v.density(), v.rows().len()));
    let (density, normal) = count(cx);
    assert_eq!(density, Density::Normal);
    cx.simulate_keystrokes("ctrl-o");
    cx.run_until_parked();
    let (density, thinking) = count(cx);
    assert_eq!(density, Density::Thinking);
    cx.simulate_keystrokes("ctrl-o");
    cx.run_until_parked();
    let (density, verbose) = count(cx);
    assert_eq!(density, Density::Verbose);
    assert!(normal <= thinking && thinking < verbose, "{normal} ≤ {thinking} < {verbose}");
    cx.simulate_keystrokes("ctrl-o");
    cx.run_until_parked();
    assert_eq!(count(cx).0, Density::Normal, "and round again");
}

/// A subagent's card opens its thread, with the bar that names it and leads back; the
/// session's background work stays with the session.
#[gpui::test]
fn a_subagent_opens_its_own_thread(cx: &mut TestAppContext) {
    let (view, cx) = face(cx, "tools");
    assert!(cx.debug_bounds("background").is_some(), "the session's background command");
    cx.simulate_keystrokes("ctrl-o ctrl-o");
    cx.run_until_parked();
    let id = view.read_with(cx, |v, _| {
        v.model()
            .thread(&slopty_proto::conversation::ThreadId::Main)
            .and_then(|t| {
                t.entries().iter().find(|e| {
                    matches!(&e.body, slopty_proto::conversation::Body::Tool(c)
                        if matches!(c.detail, slopty_proto::conversation::ToolDetail::Agent(_)))
                })
            })
            .map(|e| e.id.clone())
            .expect("an Agent call")
    });
    let card = format!("subagent-{id}");
    let row = view.read_with(cx, |v, _| {
        v.rows().iter().position(|r| r.key().contains(&id)).expect("the call has a row")
    });
    view.update(cx, |v, cx| v.scroll_to_row(row, cx));
    cx.run_until_parked();
    let at = cx.debug_bounds(Box::leak(card.into_boxed_str())).expect("the card").center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-bar").is_some(), "the subagent's thread is up");
    assert!(cx.debug_bounds("background").is_none(), "the session's background work stays");
    assert!(matches!(
        view.read_with(cx, |v, _| v.thread().clone()),
        slopty_proto::conversation::ThreadId::Agent(_)
    ));
}

/// The main thread's first entry of the kind `pick` finds.
fn main_entry(
    view: &Entity<ConversationView>,
    cx: &VisualTestContext,
    pick: impl Fn(&slopty_proto::conversation::Entry) -> bool,
) -> slopty_proto::conversation::Entry {
    view.read_with(cx, |v, _| {
        v.model()
            .thread(&slopty_proto::conversation::ThreadId::Main)
            .and_then(|t| t.entries().iter().find(|e| pick(e)).cloned())
            .expect("an entry")
    })
}

fn leak(name: String) -> &'static str {
    Box::leak(name.into_boxed_str())
}

/// Put row `ix` at the top of the view and draw.
fn show_row(view: &Entity<ConversationView>, cx: &mut VisualTestContext, ix: usize) {
    view.update(cx, |v, cx| v.scroll_to_row(ix, cx));
    cx.run_until_parked();
}

/// A settled turn ends on its answer and the files it changed; a click on a file opens the
/// session's changes there, each file over its diffs, and the bar leads back.
#[gpui::test]
fn a_changed_file_opens_the_sessions_changes(cx: &mut TestAppContext) {
    let (view, cx) = face(cx, "tools");
    let prompt =
        main_entry(&view, cx, |e| matches!(e.body, slopty_proto::conversation::Body::Prompt(_)));
    let row = view.read_with(cx, |v, _| {
        v.rows().iter().position(|r| r.key() == format!("c:{}", prompt.id)).expect("changes row")
    });
    show_row(&view, cx, row);
    let files = view.read_with(cx, |v, _| v.session_files());
    let file = files.first().expect("a changed file");
    let name = crate::conversation::tools::file_name(&file.path).to_owned();
    let bounds = cx
        .debug_bounds(leak(format!("changed-{}-{name}", prompt.id)))
        .expect("the file under the answer");
    // Its name, at the left: the way back to the newest row floats over the middle.
    let at = point(bounds.origin.x + px(40.0), bounds.center().y);
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| *v.pane()), super::Pane::Changes);
    let rows: Vec<String> = view
        .read_with(cx, |v, _| v.rows().iter().map(crate::conversation::rows::Row::key).collect());
    assert_eq!(rows.first(), Some(&format!("file:{}", file.path)));
    let (edit_thread, edit_id) = file.edits.first().expect("an edit").clone();
    assert!(rows.contains(&format!("edit:{edit_id}")), "{rows:?}");
    assert_eq!(edit_thread, slopty_proto::conversation::ThreadId::Main);
    assert!(cx.debug_bounds(leak(format!("file-{name}"))).is_some());
    assert!(cx.debug_bounds(leak(format!("edit-{edit_id}"))).is_some(), "each edit under it");
    assert!(cx.debug_bounds("composer").is_none(), "the composer steps aside");

    let back = cx.debug_bounds("thread-back").expect("the way back").center();
    cx.simulate_click(back, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| *v.pane()), super::Pane::Conversation);
    assert!(cx.debug_bounds("composer").is_some());
}

/// ⌘F finds words inside a folded turn: the fold opens, the match comes into view, Enter
/// steps on and wraps, and Esc closes the bar and gives the composer the keyboard back.
#[gpui::test]
fn cmd_f_finds_words_in_a_folded_turn(cx: &mut TestAppContext) {
    let (view, cx) = face(cx, "tools");
    let glob = main_entry(&view, cx, |e| e.id == "toolu_05");
    assert!(
        crate::conversation::find::row_of(&view.read_with(cx, |v, _| v.rows().to_vec()), &glob.id)
            .is_none(),
        "the call starts folded away"
    );
    cx.simulate_keystrokes("cmd-f");
    cx.run_until_parked();
    assert!(cx.debug_bounds("conversation-find").is_some(), "the bar opens");
    cx.simulate_input("*.rs");
    cx.run_until_parked();
    let (at, count) = view.read_with(cx, |v, _| v.found()).expect("finding");
    assert_eq!(at, 0);
    let hits =
        view.read_with(cx, |v, _| v.find.as_ref().map(|f| f.hits.clone()).unwrap_or_default());
    assert!(hits.contains(&glob.id), "the call is among {hits:?}");
    for step in 0..count {
        assert!(view.read_with(cx, |v, _| v.find_row()).is_some(), "match {step} has a row");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
    }
    assert_eq!(view.read_with(cx, |v, _| v.found()), Some((0, count)), "round again");
    let rows = view.read_with(cx, |v, _| v.rows().to_vec());
    assert!(
        rows.iter().any(|r| matches!(r, crate::conversation::rows::Row::Fold { open: true, .. })),
        "the fold holding the call opened"
    );
    assert!(!view.read_with(cx, |v, _| v.following()), "the list went to the match");

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.found()).is_none(), "closed");
    assert!(cx.debug_bounds("conversation-find").is_none());
    cx.simulate_input("hi");
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, ConversationView::draft), "hi", "the composer has the keyboard");
}

/// An answer's copy puts its words on the clipboard and says so.
#[gpui::test]
fn an_answer_copies_its_words(cx: &mut TestAppContext) {
    let (view, cx) = face(cx, "tools");
    let (row, id) = view.read_with(cx, |v, _| {
        v.rows()
            .iter()
            .enumerate()
            .find_map(|(ix, r)| match r {
                crate::conversation::rows::Row::Answer { id, end: true } => Some((ix, id.clone())),
                _ => None,
            })
            .expect("a turn's last answer")
    });
    show_row(&view, cx, row);
    let answer = cx.debug_bounds(leak(format!("answer-{id}"))).expect("the answer");
    cx.simulate_mouse_move(answer.center(), None, Modifiers::default());
    cx.run_until_parked();
    let copy = cx.debug_bounds(leak(format!("copy-a:{id}"))).expect("its copy").center();
    cx.simulate_click(copy, Modifiers::none());
    cx.run_until_parked();
    let words = main_entry(&view, cx, |e| e.id == id);
    let slopty_proto::conversation::Body::Text(text) = words.body else { panic!("text") };
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some(text.text.as_str())
    );
    assert!(view.read_with(cx, |v, _| v.copied.is_some()), "it says it copied");
}

/// A face 800 × 400 points over the made-up session of [`fixtures::work`], laid out in `dir`,
/// with every `Expand` it asks the worker for.
fn face_at_work<'a>(
    cx: &'a mut TestAppContext,
    dir: &std::path::Path,
) -> (
    Entity<ConversationView>,
    &'a mut VisualTestContext,
    std::rc::Rc<std::cell::RefCell<Vec<slopty_proto::conversation::TextRef>>>,
) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.bind_keys(crate::workspace::key_bindings());
    });
    let (view, cx) = cx.add_window_view(|window, cx| {
        let mut face = ConversationView::new(SessionId::new(), Theme::default(), window, cx);
        face.set_shown(true, cx);
        face.focus(window, cx);
        face
    });
    cx.simulate_resize(size(px(800.0), px(400.0)));
    let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = std::rc::Rc::clone(&asked);
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &super::FaceEvent, _| {
            if let super::FaceEvent::Expand { reference, .. } = event {
                sink.borrow_mut().push(reference.clone());
            }
        })
        .detach();
    });
    feed(&view, cx, fixtures::work(dir));
    (view, cx, asked)
}

/// A pasted picture's bytes are asked for once its row is drawn, and once only however often
/// it is drawn again; they come by digest, and a click opens the picture over the face, which
/// Esc closes.
#[gpui::test]
fn a_picture_is_fetched_when_shown_and_opens_large(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (view, cx, asked) = face_at_work(cx, dir.path());
    let prompt =
        main_entry(&view, cx, |e| matches!(e.body, slopty_proto::conversation::Body::Prompt(_)));
    let slopty_proto::conversation::Body::Prompt(body) = &prompt.body else { panic!("a prompt") };
    let image = body.images.first().expect("the pasted picture").clone();
    show_row(&view, cx, 0);
    scroll(cx, -40.0);
    scroll(cx, 40.0);
    assert_eq!(asked.borrow().as_slice(), std::slice::from_ref(&image.at), "asked for once");

    let bytes = data_bytes();
    view.update(cx, |v, cx| {
        v.apply(
            ConversationEvent::Image {
                thread: slopty_proto::conversation::ThreadId::Main,
                reference: image.at.clone(),
                blob: Some(slopty_proto::conversation::Blob {
                    digest: image.digest.clone(),
                    data: bytes,
                }),
            },
            cx,
        );
    });
    cx.run_until_parked();
    assert!(matches!(
        view.read_with(cx, |v, _| v.model().picture(&image.digest).cloned()),
        Some(crate::conversation::model::Picture::Here(_))
    ));
    let thumb = cx
        .debug_bounds(leak(format!("picture-prompt-{}-0", prompt.id)))
        .expect("the thumbnail")
        .center();
    cx.simulate_click(thumb, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("picture-viewer").is_some(), "open large");
    assert_eq!(
        view.read_with(cx, |v, _| v.viewing().map(|i| i.digest.clone())),
        Some(image.digest)
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("picture-viewer").is_none(), "Esc closes it");
    assert_eq!(asked.borrow().len(), 1, "nothing asked again");
}

/// The bytes a transcript holds for [`fixtures::PICTURE`].
fn data_bytes() -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    bytes.extend(1_600_u32.to_be_bytes());
    bytes.extend(1_000_u32.to_be_bytes());
    bytes.extend([8, 6, 0, 0, 0]);
    bytes
}

/// The build started in the background and the task list are sections of the composer's
/// shell, over the field; the build's row has its state and last line, a click opens its last
/// lines, a second closes them.
#[gpui::test]
fn background_work_sits_over_the_composer(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (view, cx, _) = face_at_work(cx, dir.path());
    let shell = cx.debug_bounds("composer-shell").expect("the shell");
    let field = cx.debug_bounds("composer").expect("the field");
    for section in ["background", "tasks"] {
        let at = cx.debug_bounds(section).expect(section);
        let inside = shell.contains(&at.origin)
            && shell.contains(&(at.bottom_right() - point(px(1.0), px(1.0))));
        assert!(inside, "{section} is in the shell: {at:?} in {shell:?}");
        assert!(at.bottom() <= field.top(), "{section} is over the field");
    }
    let row = cx.debug_bounds("tray-t4").expect("the build's row").center();
    assert!(view.read_with(cx, |v, _| v.background_running()), "the clock has to tick");
    cx.simulate_click(row, Modifiers::none());
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.tray_open.contains("t4")), "opened");
    assert!(cx.debug_bounds("tray-lines-t4").is_some(), "its last lines");
    // The tray grows up from the composer, so the row has moved.
    let row = cx.debug_bounds("tray-t4").expect("the build's row").center();
    cx.simulate_click(row, Modifiers::none());
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.tray_open.contains("t4")), "closed");
}

/// Opening a fold settles its rows in, and nothing is left settling after; thinking is one
/// line that says how long it took and opens on a click.
#[gpui::test]
fn thinking_is_a_line_that_opens(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (view, cx, _) = face_at_work(cx, dir.path());
    // Thinking shows from the Thinking density on, inside a turn the reader opened.
    cx.simulate_keystrokes("ctrl-o");
    cx.run_until_parked();
    let fold = view.read_with(cx, |v, _| {
        v.rows().iter().position(|r| matches!(r, crate::conversation::rows::Row::Fold { .. }))
    });
    let prompt =
        main_entry(&view, cx, |e| matches!(e.body, slopty_proto::conversation::Body::Prompt(_)));
    show_row(&view, cx, fold.expect("the settled turn folds"));
    let fold_line = cx.debug_bounds(leak(format!("fold-{}", prompt.id))).expect("the fold");
    cx.simulate_click(
        point(fold_line.origin.x + px(24.0), fold_line.center().y),
        Modifiers::none(),
    );
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.settling.is_empty()), "what it opened settles in");
    cx.executor().advance_clock(crate::kit::Pace::Settle.duration());
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.settling.is_empty()), "and has settled");

    let thinking =
        main_entry(&view, cx, |e| matches!(e.body, slopty_proto::conversation::Body::Thinking(_)));
    let row = view.read_with(cx, |v, _| {
        v.rows().iter().position(|r| r.key() == format!("e:{}", thinking.id))
    });
    show_row(&view, cx, row.expect("a thinking row"));
    let line = cx.debug_bounds(leak(format!("thinking-{}", thinking.id))).expect("its line");
    let open = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| v.toggled.contains(&format!("e:{}", thinking.id)))
    };
    assert!(!open(cx));
    cx.simulate_click(point(line.origin.x + px(24.0), line.center().y), Modifiers::none());
    cx.run_until_parked();
    assert!(open(cx), "opened");
}

/// The prompt rail draws once for what it shows: a pan of the list and a word of a streaming
/// answer reuse it as drawn; a new prompt draws it again with one more tick, and a tick's click
/// takes the list to its prompt.
#[gpui::test]
fn the_prompt_rail_draws_only_when_its_prompts_change(cx: &mut TestAppContext) {
    use slopty_proto::conversation::{
        Body, Change, Clipped, Entry, Live, LiveId, LiveKind, Prompt, ThreadId,
    };

    let dir = tempfile::tempdir().unwrap();
    cx.update(gpui_kit::init);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let mut face = ConversationView::new(SessionId::new(), Theme::default(), window, cx);
        face.set_shown(true, cx);
        face
    });
    cx.simulate_resize(size(px(800.0), px(400.0)));
    feed(&view, cx, fixtures::long(dir.path(), 6));
    let renders = |cx: &mut VisualTestContext| {
        let rail = view.read_with(cx, |v, _| v.rail.clone());
        rail.read_with(cx, |r, _| r.renders())
    };
    assert!(cx.debug_bounds("rail-5").is_some() && cx.debug_bounds("rail-6").is_none());
    let drawn = renders(cx);

    scroll(cx, 200.0);
    scroll(cx, -80.0);
    assert_eq!(renders(cx), drawn, "reused through a pan");
    // An answer that starts is a row more, which moves the ticks; its words are not.
    let id = LiveId { turn: "live".into(), step: 0, block: 0 };
    let start = Live::Start { thread: ThreadId::Main, id: id.clone(), kind: LiveKind::Text };
    feed(&view, cx, vec![ConversationEvent::Live(vec![start])]);
    let drawn = renders(cx);
    for text in ["The ", "parser ", "keeps "] {
        let word = Live::Append { id: id.clone(), text: text.into() };
        feed(&view, cx, vec![ConversationEvent::Live(vec![word])]);
    }
    assert_eq!(renders(cx), drawn, "reused through a streaming answer");
    assert!(cx.debug_bounds("rail-5").is_some(), "and still there");

    let text = Clipped { text: "One more thing".into(), lines: 1, chars: 14, full: None };
    let prompt = Prompt { text, images: Vec::new(), command: None };
    let entry = Entry { id: "p7".into(), at_ms: 0, body: Body::Prompt(prompt) };
    let upsert = Change::Upsert { thread: ThreadId::Main, entry };
    feed(&view, cx, vec![ConversationEvent::Changes(vec![upsert])]);
    assert!(renders(cx) > drawn, "a new prompt draws it again");
    assert!(cx.debug_bounds("rail-6").is_some(), "with its tick");

    let first = cx.debug_bounds("rail-0").expect("the first prompt's tick");
    cx.simulate_click(first.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.anchor()).item_ix, 0, "the list went to it");
}

/// What a frame of the face costs over the long made-up session while an answer streams, the
/// headless twin of the app's frame probe (`docs/MEASUREMENTS.md`, "the conversation face under
/// a streaming answer"): (h) a word a frame with the list on the tail, (i) the same with the
/// reader panning ±40 points twice a word. Run by hand (it prints, it does not judge).
#[gpui::test]
#[ignore = "a measurement, run by hand: see docs/MEASUREMENTS.md"]
fn measure_a_face_frame_while_an_answer_streams(cx: &mut TestAppContext) {
    use std::time::{Duration, Instant};

    use slopty_proto::conversation::{Live, LiveId, LiveKind, ThreadId};

    const WORDS: usize = 300;
    const WARM: usize = 20;
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.bind_keys(crate::workspace::key_bindings());
    });
    let (view, cx) = cx.add_window_view(|window, cx| {
        let mut face = ConversationView::new(SessionId::new(), Theme::default(), window, cx);
        face.set_shown(true, cx);
        face
    });
    cx.simulate_resize(size(px(1000.0), px(720.0)));
    feed(&view, cx, fixtures::long(dir.path(), 80));
    let id = LiveId { turn: "live".into(), step: 0, block: 0 };
    let start = Live::Start { thread: ThreadId::Main, id: id.clone(), kind: LiveKind::Text };
    feed(&view, cx, vec![ConversationEvent::Live(vec![start])]);
    let words = [
        "The ",
        "parser ",
        "keeps ",
        "the ",
        "span ",
        "through ",
        "`recover`, ",
        "and ",
        "an ",
        "empty ",
        "line ",
        "reads ",
        "as ",
        "one.\n\n",
        "- ",
        "fixed ",
        "`parse_header`\n",
    ];
    let mut words = words.iter().cycle();
    let mut run = |cx: &mut VisualTestContext, pan: bool| {
        let mut took: Vec<Duration> = Vec::with_capacity(WORDS * 2);
        for frame in 0..WARM + WORDS {
            let text = words.next().map(|w| (*w).to_owned()).unwrap_or_default();
            let append = Live::Append { id: id.clone(), text };
            let begin = Instant::now();
            feed(&view, cx, vec![ConversationEvent::Live(vec![append])]);
            let mut spent = vec![begin.elapsed()];
            if pan {
                let dy = if (frame / 60).is_multiple_of(2) { 40.0 } else { -40.0 };
                for _ in 0..2 {
                    let begin = Instant::now();
                    scroll(cx, dy);
                    spent.push(begin.elapsed());
                }
            }
            if frame >= WARM {
                took.extend(spent);
            }
        }
        took.sort_unstable();
        let ms = |p: usize| slopty_client::pacing::percentile(&took, p).as_secs_f64() * 1e3;
        let max = took.last().copied().unwrap_or_default().as_secs_f64() * 1e3;
        (ms(50), ms(95), ms(99), max, took.len())
    };
    for (scenario, pan) in [("(h) following", false), ("(i) panning", true)] {
        let (p50, p95, p99, max, frames) = run(cx, pan);
        println!(
            "MEASURE face {scenario}, 80 turns, headless: p50 {p50:.3} ms p95 {p95:.3} ms p99 \
             {p99:.3} ms max {max:.3} ms over {frames} frames"
        );
    }
}
