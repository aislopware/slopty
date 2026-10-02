//! The notices in the status bar: over no tile, and one being read is never taken away
//! under the pointer.

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

/// A notice sits in the status bar, over no tile: with a shell open it lies inside the bar,
/// clear of the strip; with no worker yet the bar comes up to hold it.
#[gpui::test]
fn a_notice_sits_in_the_status_bar_over_no_tile(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    view.update_in(cx, |v, _w, cx| v.show_notice("Settings did not parse".to_owned(), cx));
    cx.run_until_parked();
    let (bar, notice) = (
        cx.debug_bounds("statusbar").expect("up for the notice"),
        cx.debug_bounds("said").expect("the notice"),
    );
    assert!(bar.contains(&notice.center()), "{notice:?} in {bar:?}");

    let studio = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    view.update_in(cx, |v, _w, cx| v.show_notice("Copied the address".to_owned(), cx));
    cx.run_until_parked();
    let (bar, tile) = (
        cx.debug_bounds("statusbar").expect("the bar"),
        cx.debug_bounds(selector("item", shell.item)).expect("the tile"),
    );
    let notice = cx.debug_bounds("said").expect("the notice");
    assert!(bar.contains(&notice.center()), "{notice:?} in {bar:?}");
    assert!(notice.top() >= tile.bottom(), "under the tile, not over it: {notice:?} {tile:?}");
}
