//! Where the person is, as this client would tell the server: the seat, the terminals on screen
//! and the one with the keyboard.

use slopty_proto::orchestration::TermRef;
use slopty_proto::thread::attention::Seat;

use super::*;

/// A Mac is a desk; what it says is on screen are the terminals the panes drew, and the focus
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

/// A program's own notification and an agent's moment are told apart, so a server that leads
/// the agent moments leaves the program's to this client: an `OSC 9` is `Program`, an agent that
/// needs the person is `Attention`.
#[gpui::test]
fn a_program_s_notification_is_its_own_event_apart_from_an_agent_s(cx: &mut TestAppContext) {
    use crate::terminal::TerminalViewEvent;

    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let _tile = opens(&view, cx, &studio, session, studio.me, 1);
    let events = Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = Rc::clone(&events);
    cx.update(|_w, cx| {
        cx.subscribe(&view, move |_v, e: &WorkspaceEvent, _cx| seen.borrow_mut().push(*e)).detach();
    });
    let terminal = view.read_with(cx, |v, _| v.terminals.get(&session).cloned()).expect("a shell");
    terminal.update(cx, |_, cx| {
        cx.emit(TerminalViewEvent::Notification { title: "build".into(), body: "done".into() });
    });
    cx.run_until_parked();
    assert_eq!(events.borrow().as_slice(), [WorkspaceEvent::Program(session)]);

    // The agent at work, then asking: its thread's row comes to need the person.
    let working = AgentEvent { status: AgentStatus::Working, ..blocked(session) };
    view.update_in(cx, |v, _w, cx| v.agent_event(working, cx));
    cx.run_until_parked();
    events.borrow_mut().clear();
    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(session), cx));
    cx.run_until_parked();
    let thread = view.read_with(cx, |v, _| v.session_thread(session)).expect("its thread");
    assert!(events.borrow().contains(&WorkspaceEvent::Attention(thread)), "{:?}", events.borrow());
    assert!(!events.borrow().contains(&WorkspaceEvent::Program(session)));
}
