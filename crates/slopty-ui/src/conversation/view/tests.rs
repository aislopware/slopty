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

/// A subagent's card opens its thread, with the bar that names it and leads back.
#[gpui::test]
fn a_subagent_opens_its_own_thread(cx: &mut TestAppContext) {
    let (view, cx) = face(cx, "tools");
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
