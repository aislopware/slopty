//! The component preview (`crate::components`): from the palette it takes the panes' place
//! with every surface over its samples and the controls beside them, and its Done gives the
//! panes back.

use super::*;
use crate::project::model::Lane;
use crate::workspace::actions::OpenComponentPreview;

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

fn settle(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

/// Every surface shows from its samples: the thread's plan, each lane of the board, the review's
/// first file, the settings' form and the controls, each part named once in the
/// accessibility tree; Done leaves.
#[gpui::test]
fn the_component_preview_shows_every_surface_then_leaves(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    cx.simulate_resize(size(px(1600.0), px(1000.0)));
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    // The tree is built, so a name two parts share fails here as it fails the app.
    cx.update(|window, _| window.set_a11y_active(true));
    view.update_in(cx, |v, window, cx| {
        v.open_component_preview(&OpenComponentPreview, window, cx);
    });
    settle(cx);
    for cell in ["thread", "board", "review", "settings", "kit"] {
        let at = cx.debug_bounds(leak(format!("components-{cell}")));
        assert!(at.is_some_and(|b| b.size.height > px(0.0)), "the {cell} cell is drawn");
    }
    assert!(cx.debug_bounds("thread-plan").is_some(), "the thread's plan");
    for lane in Lane::ALL {
        let named = leak(format!("project-lane-{}", lane.selector()));
        assert!(cx.debug_bounds(named).is_some(), "the board's {} lane", lane.title());
    }
    // The review's cell is the height of its first file; the second waits under it.
    assert!(cx.debug_bounds("review-head-0").is_some(), "the review's first file");
    assert!(cx.debug_bounds("components-primary").is_some(), "the buttons");
    assert!(cx.debug_bounds("components-menu").is_some(), "the menu");

    let done = cx.debug_bounds("components-done").expect("the page's Done").center();
    cx.simulate_click(done, Modifiers::none());
    settle(cx);
    assert!(cx.debug_bounds("components").is_none(), "the panes again");
}
