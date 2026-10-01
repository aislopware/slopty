//! The empty workspace with no worker at all: it says where a worker comes from and offers the
//! app's way to add one.

use std::cell::Cell;

use super::*;

/// With no worker the page names what a worker is and has a button that runs the app's
/// "Add a worker"; with no such way given (an app that cannot add one) it only says so.
#[gpui::test]
fn with_no_worker_the_empty_workspace_offers_to_add_one(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(cx.debug_bounds("empty-add-worker").is_none(), "nothing to run it with");

    let asked = Rc::new(Cell::new(0_u32));
    let count = Rc::clone(&asked);
    let add: MenuRun = Rc::new(move |_window, _cx| count.set(count.get().saturating_add(1)));
    view.update_in(cx, |v, _w, cx| v.set_host_actions(HashMap::new(), Some(add), cx));
    cx.run_until_parked();
    let button = cx.debug_bounds("empty-add-worker").expect("the page's button");
    let nodes = tree(cx);
    assert!(nodes.iter().any(|n| n.is("Button", Some(ADD_WORKER))), "{nodes:#?}");
    cx.simulate_click(button.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(asked.get(), 1, "the app's add worker ran");
}
