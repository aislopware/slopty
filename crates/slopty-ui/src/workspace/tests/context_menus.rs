//! A thing's own menu by a right click or a long press: a tile's from its navigator row and its
//! header, a machine's from its row, hung where the press landed.

use gpui::{LongPressEvent, MouseButton};

use super::*;

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

fn bounds(cx: &mut VisualTestContext, selector: &'static str) -> Bounds<Pixels> {
    cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"))
}

fn right_click(cx: &mut VisualTestContext, selector: &'static str) -> Point<Pixels> {
    let at = bounds(cx, selector).center();
    cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    at
}

fn pick(cx: &mut VisualTestContext, label: &str) {
    let at = bounds(cx, leak(format!("menu-{label}"))).center();
    cx.simulate_click(at, Modifiers::default());
    cx.run_until_parked();
}

fn rows(cx: &mut VisualTestContext) -> Vec<String> {
    tree(cx).into_iter().filter(|n| n.role == "MenuItem").filter_map(|n| n.label).collect()
}

/// A right click on a tile's navigator row opens the tile's menu where it landed: Open first,
/// and Copy path puts the file's path on the clipboard. On the tile's own header there is no
/// Open, and Rename opens the header's name field. A machine's row opens its "…" menu there.
/// A long press on a row opens the same menu as a right click; Esc closes it.
#[gpui::test]
fn a_right_click_or_a_long_press_opens_a_things_own_menu(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let file = arrives(&view, cx, &studio, ItemKind::File { path: "/w/notes.txt".into() }, 1);
    let shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    cx.update(|_w, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(String::new())));

    let row = leak(format!("nav-tile-{}", file.item.as_uuid()));
    let at = right_click(cx, row);
    assert!(tree(cx).iter().any(|n| n.is("Menu", Some("Tile"))), "the tile's menu");
    assert_eq!(rows(cx), ["Open", "Rename", "Zoom pane", "Copy path", "Close tile"]);
    let menu = bounds(cx, "menu");
    assert!(menu.contains(&(at + point(px(1.0), px(1.0)))), "hung at the press: {menu:?} {at:?}");
    pick(cx, "Copy path");
    assert!(cx.debug_bounds("menu").is_none(), "a row closes it");
    let copied = cx.update(|_w, cx| cx.read_from_clipboard().and_then(|c| c.text()));
    assert_eq!(copied.as_deref(), Some("/w/notes.txt"));

    right_click(cx, selector("title", shell.item));
    let shown = rows(cx);
    assert!(!shown.iter().any(|r| r == "Open"), "the tile is already open: {shown:?}");
    pick(cx, "Rename");
    assert!(view.read_with(cx, |v, _| v.rename.as_ref().is_some_and(|r| r.tile == shell)));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    right_click(cx, leak(format!("nav-worker-{}", studio.key)));
    assert!(tree(cx).iter().any(|n| n.is("Menu", Some("Machine"))), "the machine's own");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("menu").is_none(), "Esc closes it");

    let at = bounds(cx, row).center();
    cx.simulate_event(LongPressEvent {
        phase: TouchPhase::Started,
        start_position: at,
        position: at,
    });
    cx.run_until_parked();
    assert!(tree(cx).iter().any(|n| n.is("Menu", Some("Tile"))), "a long press opens it too");
}

/// A tab's menu adds what a pane of tabs offers: Split out to the right puts its tile in a
/// pane of its own right of the one it left, and Close other tabs is there beside Close tile.
#[gpui::test]
fn a_tabs_menu_splits_its_tile_out_to_the_right(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let first = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let second = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    one_pane(&view, cx, &[first, second]);
    assert_eq!(pos_of(&view, cx, first), pos_of(&view, cx, second), "one pane");

    right_click(cx, selector("tab", first.item));
    let shown = rows(cx);
    for row in ["Rename", "Split out to the right", "Close tile", "Close other tabs"] {
        assert!(shown.iter().any(|r| r == row), "{row}: {shown:?}");
    }
    assert!(!shown.iter().any(|r| r == "Open"), "the tab is open: {shown:?}");
    pick(cx, "Split out to the right");
    let (out, left) = (pos_of(&view, cx, first), pos_of(&view, cx, second));
    assert_eq!(out.tab, left.tab, "on the same tab");
    assert_ne!(out.pane, left.pane, "a pane of its own");
    let at = |tile: TileRef| view.read_with(cx, |v, _| v.tile_bounds(tile)).expect("drawn");
    assert!(at(first).left() >= at(second).right() - px(1.0), "to the right");
}
