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
