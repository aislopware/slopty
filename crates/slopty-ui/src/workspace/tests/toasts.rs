//! The notices in the title bar: over no tile, one being read is never taken away under the
//! pointer, none lapses while the app is not in front, and a failure stays until dismissed.

use super::*;
use crate::workspace::toast::{SAY_AFTER_HOVER, SAY_FOR};

/// A notice whose time comes while the pointer is over it stays; once the pointer leaves it
/// goes after a short while more.
#[gpui::test]
fn a_notice_under_the_pointer_stays_until_the_pointer_leaves(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    view.update_in(cx, |v, _w, cx| v.show_notice("Copied the address".to_owned(), cx));
    cx.run_until_parked();
    let notice = cx.debug_bounds("said").expect("the notice");
    cx.simulate_mouse_move(notice.center(), None, Modifiers::default());
    cx.run_until_parked();

    cx.executor().advance_clock(SAY_FOR.saturating_add(SAY_FOR));
    cx.run_until_parked();
    let up = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.toast_texts());
    assert_eq!(up(cx), ["Copied the address"], "held while it is read");

    cx.simulate_mouse_move(point(px(10.0), px(400.0)), None, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(up(cx), ["Copied the address"], "not at once");
    cx.executor().advance_clock(SAY_AFTER_HOVER.saturating_add(crate::kit::Pace::Fade.duration()));
    cx.run_until_parked();
    assert!(up(cx).is_empty(), "then it goes");
}

/// Typing under a resting pointer keeps the hold: a key is not the pointer leaving, so the
/// notice being read does not start its countdown. (Ely's
/// `typing_under_a_resting_pointer_keeps_the_hold`, as ours.)
#[gpui::test]
fn typing_under_a_resting_pointer_keeps_the_hold(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    view.update_in(cx, |v, _w, cx| v.show_notice("Copied the address".to_owned(), cx));
    cx.run_until_parked();
    let notice = cx.debug_bounds("said").expect("the notice");
    cx.simulate_mouse_move(notice.center(), None, Modifiers::default());
    cx.run_until_parked();

    cx.simulate_keystrokes("a");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    cx.executor().advance_clock(SAY_FOR.saturating_add(SAY_FOR));
    cx.run_until_parked();
    let up = view.read_with(cx, |v, _| v.toast_texts());
    assert_eq!(up, ["Copied the address"], "a key leaves the pointer where it is");
}

/// A notice about no one tile sits in the title bar, over no tile: with a shell open it lies
/// inside the bar, above the strip; with no worker yet the bar holds it all the same.
#[gpui::test]
fn a_notice_sits_in_the_title_bar_over_no_tile(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    view.update_in(cx, |v, _w, cx| v.show_notice("Settings did not parse".to_owned(), cx));
    cx.run_until_parked();
    let (bar, notice) = (
        cx.debug_bounds("titlebar").expect("the title bar"),
        cx.debug_bounds("said").expect("the notice"),
    );
    assert!(bar.contains(&notice.center()), "{notice:?} in {bar:?}");

    let studio = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    view.update_in(cx, |v, _w, cx| v.show_notice("Copied the address".to_owned(), cx));
    cx.run_until_parked();
    let (bar, tile) = (
        cx.debug_bounds("titlebar").expect("the bar"),
        cx.debug_bounds(selector("item", shell.item)).expect("the tile"),
    );
    let notice = cx.debug_bounds("said").expect("the notice");
    assert!(bar.contains(&notice.center()), "{notice:?} in {bar:?}");
    assert!(notice.bottom() <= tile.top(), "above the tile, not over it: {notice:?} {tile:?}");
}

/// A notice whose time comes while the app is not in front waits for it: it outlives its time
/// and goes a short while after the app comes back, so nothing lapses unseen.
#[gpui::test]
fn a_notice_waits_while_the_app_is_not_in_front(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    view.update_in(cx, |v, _w, cx| {
        v.set_app_active(false, cx);
        v.show_notice("Copied the address".to_owned(), cx);
    });
    cx.executor().advance_clock(SAY_FOR.saturating_add(SAY_FOR));
    cx.run_until_parked();
    let up = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.toast_texts());
    assert_eq!(up(cx), ["Copied the address"], "held while nobody looks");

    view.update_in(cx, |v, _w, cx| v.set_app_active(true, cx));
    cx.run_until_parked();
    assert_eq!(up(cx), ["Copied the address"], "not at once");
    cx.executor().advance_clock(SAY_AFTER_HOVER.saturating_add(crate::kit::Pace::Fade.duration()));
    cx.run_until_parked();
    assert!(up(cx).is_empty(), "then it goes");
}

/// A failure stays until it is dismissed, in the error's mark, and offers its words to copy;
/// newer words push out an older word before they push out a failure.
#[gpui::test]
fn a_failure_stays_until_dismissed_and_copies(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    view.update_in(cx, |v, _w, cx| v.show_failure("Settings: line 3 is not TOML".to_owned(), cx));
    cx.executor().advance_clock(SAY_FOR.saturating_mul(3));
    cx.run_until_parked();
    let up = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.toast_texts());
    assert_eq!(up(cx), ["Settings: line 3 is not TOML"], "it stays");
    view.update_in(cx, |v, _w, cx| {
        v.show_notice("one".to_owned(), cx);
        v.show_notice("two".to_owned(), cx);
    });
    cx.run_until_parked();
    assert_eq!(up(cx), ["Settings: line 3 is not TOML", "two"], "the older word went");

    let copy = cx.debug_bounds("toast-copy").expect("Copy").center();
    cx.simulate_click(copy, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("Settings: line 3 is not TOML")
    );
    let dismiss = cx.debug_bounds("toast-dismiss").expect("Dismiss").center();
    cx.simulate_click(dismiss, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(up(cx), ["two"], "dismissed");
}
