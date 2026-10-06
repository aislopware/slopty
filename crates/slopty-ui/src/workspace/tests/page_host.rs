//! A page tile's page is a native the window composes: its body is the page's element, so the
//! page shows where its pane draws the tile and GPUI draws over it what it draws after. The
//! page's keyboard is GPUI's focus on that element, both ways. The web view itself is stood in
//! for: the test platform records its host.

use std::io::Cursor;

use gpui::composition::TestNativeHost;
use slopty_platform::web::WebEvent;

use super::*;
use crate::browser::Edit;

const HOME: &str = "http://127.0.0.1:5173/";

/// A page tile on `fake`, focused, its page open on a host of the test platform's, in an
/// active window (a window tells its focus only while it is active).
fn open_page(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, fake: &Fake) -> TileRef {
    cx.update(|window, _| window.activate_window());
    let tile = arrives(view, cx, fake, ItemKind::Browser { url: HOME.into() }, 1);
    view.update_in(cx, |v, window, cx| {
        v.focus_tile(tile, cx);
        let page = v.browser(tile.item).cloned().expect("a page view");
        page.update(cx, |page, cx| page.open_stand_in(window, cx));
    });
    cx.run_until_parked();
    tile
}

fn page_view(
    view: &Entity<WorkspaceView>,
    cx: &VisualTestContext,
    tile: TileRef,
) -> Entity<crate::browser::BrowserView> {
    view.read_with(cx, |v, _| v.browser(tile.item).cloned()).expect("a page view")
}

/// The platform's side of the page's host, as the test platform keeps it.
fn host_of(
    view: &Entity<WorkspaceView>,
    cx: &VisualTestContext,
    tile: TileRef,
) -> gpui::composition::NativeHost {
    page_view(view, cx, tile).read_with(cx, |p, _| p.native_host().cloned()).expect("a host")
}

fn holds_keyboard(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, tile: TileRef) -> bool {
    let page = page_view(view, cx, tile);
    cx.update(|window, cx| page.read(cx).holds_keyboard(window))
}

fn workspace_focused(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| view.read(cx).focus.is_focused(window))
}

/// A small PNG, as the platform hands a page's snapshot over.
fn png() -> Vec<u8> {
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(4, 4, image::Rgba([10, 20, 30, 255]))
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
        .expect("a PNG");
    bytes
}

/// The tile's body is the page's element, inside the tile, and nothing else stands in for
/// the page there: no measuring, no placing after the frame.
#[gpui::test]
fn a_page_is_its_tile_s_body_where_the_strip_draws_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    let fake = connect(&view, cx, 1, "studio");
    let tile = open_page(&view, cx, &fake);

    let page = cx.debug_bounds(selector("page", tile.item)).expect("the page is drawn");
    let body = cx.debug_bounds(selector("browser", tile.item)).expect("the tile's body");
    let item = cx.debug_bounds(selector("item", tile.item)).expect("the tile");
    assert!(page.size.width > px(100.0) && page.size.height > px(100.0), "{page:?}");
    let inside = |outer: Bounds<Pixels>, inner: Bounds<Pixels>| {
        inner.origin.x >= outer.origin.x
            && inner.origin.y >= outer.origin.y
            && inner.bottom_right().x <= outer.bottom_right().x
            && inner.bottom_right().y <= outer.bottom_right().y
    };
    assert!(inside(body, page) && inside(item, body), "{item:?} ⊇ {body:?} ⊇ {page:?}");
    let host = host_of(&view, cx, tile);
    assert!(
        host.platform().as_any().downcast_ref::<TestNativeHost>().is_some(),
        "the page is composed through the window's native host"
    );
}

/// A page's picture decoded leaves the page itself on show, and a failed page is gone with
/// its picture, its tile saying why.
#[gpui::test]
fn a_page_stays_live_over_its_picture_and_a_failed_page_says_why(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    let fake = connect(&view, cx, 1, "studio");
    let tile = open_page(&view, cx, &fake);
    let page = page_view(&view, cx, tile);
    cx.update(|window, cx| {
        page.update(cx, |p, cx| p.native_event(WebEvent::Snapshot(png()), window, cx));
    });
    cx.run_until_parked();
    assert!(page.read_with(cx, |p, _| p.has_snapshot()), "the picture is decoded");
    assert!(cx.debug_bounds(selector("page-picture", tile.item)).is_none(), "the page shows");

    cx.update(|window, cx| {
        page.update(cx, |p, cx| p.native_event(WebEvent::Failed("refused".into()), window, cx));
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("page", tile.item)).is_none(), "a failed page is gone");
    assert!(cx.debug_bounds(selector("page-picture", tile.item)).is_none(), "and its picture");
}

/// The platform moving the keyboard into the page (a click AppKit gave it) focuses the page's
/// element and its tile; moving it out with nothing else taking it gives the keyboard back
/// to the workspace.
#[gpui::test]
fn the_page_taking_the_keyboard_focuses_its_tile_and_giving_it_back_the_workspace(
    cx: &mut TestAppContext,
) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    let fake = connect(&view, cx, 1, "studio");
    let tile = open_page(&view, cx, &fake);
    let other = arrives(&view, cx, &fake, ItemKind::Browser { url: "http://a.test/".into() }, 2);
    beside(&view, cx, other, tile, slopty_client::layout::Side::Right);
    view.update(cx, |v, cx| v.focus_tile(other, cx));
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(other));

    let host = host_of(&view, cx, tile);
    let simulate = |focused: bool| {
        let platform = host.platform();
        let test = platform.as_any().downcast_ref::<TestNativeHost>().expect("a test host");
        test.simulate_focus(focused);
    };
    simulate(true);
    cx.run_until_parked();
    assert!(holds_keyboard(&view, cx, tile), "GPUI's focus is on the page");
    assert_eq!(focused(&view, cx), Some(tile), "and its tile is the focused one");

    simulate(false);
    cx.run_until_parked();
    assert!(!holds_keyboard(&view, cx, tile), "the page gave the keyboard back");
    assert!(workspace_focused(&view, cx), "to the workspace");
}

/// A click in the page is GPUI's first: the page's element takes the focus, which gives the
/// page the keyboard, and its tile is the focused one.
#[gpui::test]
fn a_click_in_the_page_focuses_it_and_its_tile(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    let fake = connect(&view, cx, 1, "studio");
    let tile = open_page(&view, cx, &fake);
    let other = arrives(&view, cx, &fake, ItemKind::Browser { url: "http://a.test/".into() }, 2);
    beside(&view, cx, other, tile, slopty_client::layout::Side::Right);
    view.update(cx, |v, cx| v.focus_tile(other, cx));
    cx.run_until_parked();
    // The first page is left of the second, still in view.
    let at = cx.debug_bounds(selector("page", tile.item)).expect("the first page is drawn");
    cx.simulate_click(at.center(), Modifiers::default());
    cx.run_until_parked();
    assert!(holds_keyboard(&view, cx, tile), "the page has the keyboard");
    assert_eq!(focused(&view, cx), Some(tile), "its tile is focused");
}

/// Esc once is the page's (its own dialogs and fields close on it); Esc twice gives the
/// keyboard back to the workspace.
#[gpui::test]
fn esc_once_is_the_page_s_and_twice_gives_the_keyboard_back(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    let fake = connect(&view, cx, 1, "studio");
    let tile = open_page(&view, cx, &fake);
    let at = cx.debug_bounds(selector("page", tile.item)).expect("the page is drawn");
    cx.simulate_click(at.center(), Modifiers::default());
    cx.run_until_parked();
    assert!(holds_keyboard(&view, cx, tile));

    cx.simulate_keystrokes("escape");
    assert!(holds_keyboard(&view, cx, tile), "one Esc stays with the page");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!holds_keyboard(&view, cx, tile), "a second gives the keyboard back");
    assert!(workspace_focused(&view, cx), "to the workspace");
}

/// While the page holds the keyboard its edit keys are its own: ⌘Z, ⇧⌘Z, ⌘X and ⌘A reach the
/// page before the workspace's bindings, and "Undo close", which the menu bar runs for ⌘Z before
/// any binding sees it, is the page's undo. With the keyboard back, ⌘Z takes a tile back again.
#[gpui::test]
fn the_edit_keys_are_the_page_s_while_it_holds_the_keyboard(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    let fake = connect(&view, cx, 1, "studio");
    let tile = open_page(&view, cx, &fake);
    let gone = arrives(&view, cx, &fake, ItemKind::Browser { url: "http://a.test/".into() }, 2);
    view.update(cx, |v, cx| v.focus_tile(gone, cx));
    cx.run_until_parked();
    cx.dispatch_action(CloseItem);
    cx.run_until_parked();
    let closed = |cx: &VisualTestContext| view.read_with(cx, |v, _| v.closed.len());
    assert_eq!(closed(cx), 1, "a tile to take back");

    let at = cx.debug_bounds(selector("page", tile.item)).expect("the page is drawn");
    cx.simulate_click(at.center(), Modifiers::default());
    cx.run_until_parked();
    assert!(holds_keyboard(&view, cx, tile));
    cx.simulate_keystrokes("cmd-z cmd-shift-z cmd-x cmd-a");
    cx.dispatch_action(UndoClose);
    cx.run_until_parked();
    let page = page_view(&view, cx, tile);
    let performed = page.read_with(cx, |p, _| p.performed());
    assert_eq!(performed, [Edit::Undo, Edit::Redo, Edit::Cut, Edit::SelectAll, Edit::Undo]);
    assert_eq!(closed(cx), 1, "nothing was taken back");

    cx.simulate_keystrokes("escape escape");
    cx.run_until_parked();
    assert!(workspace_focused(&view, cx));
    cx.simulate_keystrokes("cmd-z");
    cx.run_until_parked();
    assert_eq!(closed(cx), 0, "⌘Z is the workspace's again");
    assert_eq!(page.read_with(cx, |p, _| p.performed().len()), 5, "and not the page's");
}
