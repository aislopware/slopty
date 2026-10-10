//! What the workspace says while setup is unfinished: the server out of reach with no machine
//! listed, the server's line on a phone, and a remote tile on a Mac that cannot take input.

use std::cell::Cell;

use super::*;

/// The server's menu as the app gives it: try now, connect to another.
fn server_menu(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Rc<Cell<u32>> {
    let tried = Rc::new(Cell::new(0_u32));
    let count = Rc::clone(&tried);
    let entry = |label: &str, run: MenuRun| MenuEntry {
        group: MenuGroup::Connections,
        label: label.to_owned().into(),
        detail: SharedString::default(),
        run,
    };
    view.update(cx, |v, _cx| {
        v.set_server_menu(vec![
            entry("Retry now", Rc::new(move |_w, _cx| count.set(count.get().saturating_add(1)))),
            entry("Connect to another server", Rc::new(|_w, _cx| ())),
        ]);
    });
    tried
}

/// With the server out of reach and no machine listed, the empty page leads with the server:
/// its state, what that means, and its doors, the first raised; adding a machine comes after.
/// It does not say there are no machines, since nobody is there to list them.
#[gpui::test]
fn with_the_server_away_the_empty_page_says_so_first(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let tried = server_menu(&view, cx);
    let add: MenuRun = Rc::new(|_w, _cx| ());
    view.update_in(cx, |v, _w, cx| {
        v.set_host_actions(HashMap::new(), Some(add), cx);
        v.set_server_status(Some("server offline".into()), cx);
    });
    cx.run_until_parked();
    let nodes = tree(cx);
    let labels: Vec<&str> = nodes.iter().filter_map(|n| n.label.as_deref()).collect();
    assert!(!labels.contains(&NO_WORKERS), "{labels:#?}");
    for part in ["empty-server-retry", "empty-server-other", "empty-add-worker"] {
        assert!(cx.debug_bounds(part).is_some(), "{part} is drawn");
    }
    let retry = cx.debug_bounds("empty-server-retry").expect("drawn");
    assert!(retry.top() < cx.debug_bounds("empty-add-worker").expect("drawn").top(), "first");
    cx.simulate_click(retry.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(tried.get(), 1, "the app's retry ran");

    view.update_in(cx, |v, _w, cx| v.set_server_status(None, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("empty-server-retry").is_none(), "the server is back");
}

/// A phone's bar has no readouts, so its navigator says the server's state under its header,
/// with the way to try it now.
#[gpui::test]
fn a_phone_s_navigator_says_the_server_is_away(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let tried = server_menu(&view, cx);
    cx.simulate_resize(size(px(390.0), px(760.0)));
    view.update_in(cx, |v, _w, cx| {
        v.set_server_status(Some("server offline".into()), cx);
        v.nav.open = true;
        cx.notify();
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("readout-server").is_none(), "no readouts on a phone's bar");
    assert!(cx.debug_bounds("nav-server").is_some(), "the navigator says it");
    let retry = cx.debug_bounds("nav-server-retry").expect("its door");
    cx.simulate_click(retry.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(tried.get(), 1);
}

/// A remote window on a Mac without Accessibility says, over its picture, that clicks and keys
/// do nothing there and what to turn on; with it granted, nothing is said.
#[gpui::test]
fn a_remote_window_on_a_mac_that_takes_no_input_says_so(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let tile = arrives(&view, cx, &fake, ItemKind::Window { window: slopty_core::WindowId(7) }, 1);
    let line = selector("no-input", tile.item);
    assert!(cx.debug_bounds(line).is_none(), "the hello's caps grant input");

    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        if let Some(w) = v.workers.get_mut(&key) {
            w.caps = Some(WorkerCaps { can_inject: false, ..healthy() });
        }
        cx.notify();
    });
    cx.run_until_parked();
    let drawn = cx.debug_bounds(line).expect("the line");
    let body = cx.debug_bounds(selector("item", tile.item)).expect("the tile");
    assert!(body.contains(&drawn.center()), "on the tile");
    let nodes = tree(cx);
    let said = tile::no_input("studio");
    assert!(nodes.iter().any(|n| n.is("Status", Some(&said))), "{nodes:#?}");
}

/// A device whose notes were never asked for says so under its navigator's header, and its
/// line asks, once a press; notes turned off say that instead, and its door goes to the
/// system's settings. Notes that reach the person say nothing.
#[gpui::test]
fn a_phone_s_navigator_asks_for_notes_until_they_reach_the_person(cx: &mut TestAppContext) {
    use slopty_platform::notify::Alerts;

    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    cx.simulate_resize(size(px(390.0), px(760.0)));
    let asked = Rc::new(Cell::new(0_u32));
    let door = |count: &Rc<Cell<u32>>| -> MenuRun {
        let count = Rc::clone(count);
        Rc::new(move |_w, _cx| count.set(count.get().saturating_add(1)))
    };
    view.update_in(cx, |v, _w, cx| {
        v.nav.open = true;
        v.set_notes(Alerts::Unasked, door(&asked), cx);
    });
    cx.run_until_parked();
    let nodes = tree(cx);
    assert!(
        nodes.iter().any(|n| n.label.as_deref() == Some(readouts::NOTES_ASK)),
        "it asks: {nodes:#?}"
    );
    let line = cx.debug_bounds("nav-notes").expect("the line");
    let header = cx.debug_bounds("navigator").expect("the navigator");
    assert!(header.contains(&line.center()), "in the navigator: {line:?}");
    let allow = cx.debug_bounds("nav-notes-door").expect("its door");
    cx.simulate_click(allow.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(asked.get(), 1, "one press asks once");

    let opened = Rc::new(Cell::new(0_u32));
    view.update_in(cx, |v, _w, cx| v.set_notes(Alerts::Denied, door(&opened), cx));
    cx.run_until_parked();
    let nodes = tree(cx);
    assert!(
        nodes.iter().any(|n| n.label.as_deref() == Some(readouts::NOTES_OFF_LINE)),
        "it says they are off: {nodes:#?}"
    );
    let turn_on = cx.debug_bounds("nav-notes-door").expect("its door");
    cx.simulate_click(turn_on.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!((asked.get(), opened.get()), (1, 1), "the settings' door, not the question");

    for quiet in [Alerts::Allowed, Alerts::Unavailable] {
        view.update_in(cx, |v, _w, cx| v.set_notes(quiet, door(&asked), cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("nav-notes").is_none(), "{quiet:?} says nothing");
    }
}
