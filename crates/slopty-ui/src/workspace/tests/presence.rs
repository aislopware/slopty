//! Where the person is, as this client would tell the server: the seat, the terminals on screen
//! and the one with the keyboard.

use slopty_proto::orchestration::TermRef;

use super::*;
use crate::workspace::presence::Seat;

/// A Mac is a desk; what it says is on screen are the terminals the strip drew, and the focus
/// is the focused tile's terminal. A phone-sized window is carried.
#[gpui::test]
fn presence_names_the_seat_the_terminals_in_view_and_the_focused_one(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let [_, _, (last, tile)] = three_shells(&view, cx, &studio);
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();

    let worker: slopty_core::WorkerId = format!("{:032x}", 1_u128).parse().expect("an id");
    let at = |session| TermRef { worker, session };
    let presence = view.update_in(cx, |v, window, _| v.presence(window));
    assert_eq!(presence.seat, Seat::Desk);
    assert_eq!(presence.focus, Some(at(last)));
    assert!(presence.showing.contains(&at(last)), "{:?}", presence.showing);
    let drawn = view.read_with(cx, |v, _| v.drawn.on_screen.borrow().len());
    assert_eq!(presence.showing.len(), drawn, "every terminal drawn, and only those");

    cx.simulate_resize(size(px(390.0), px(844.0)));
    cx.run_until_parked();
    let presence = view.update_in(cx, |v, window, _| v.presence(window));
    assert_eq!(presence.seat, Seat::Handheld, "a phone is carried");
}
