//! The niri ops past the arrows reach the layout by key: the workspace keys, carrying a
//! column across workspaces, moving a workspace, the column ends and the visible columns.

use super::*;

fn press(cx: &mut VisualTestContext, keys: &str) {
    cx.simulate_keystrokes(keys);
    cx.run_until_parked();
}

fn workspace_of(view: &Entity<WorkspaceView>, cx: &VisualTestContext, tile: TileRef) -> usize {
    view.read_with(cx, |v, _| v.layout().position(tile).map(|p| p.workspace)).expect("placed")
}

fn active_workspace(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> usize {
    view.read_with(cx, |v, _| v.layout().active_workspace())
}

fn width(cx: &mut VisualTestContext, tile: TileRef) -> f32 {
    cx.debug_bounds(selector("item", tile.item)).map(|b| f32::from(b.size.width)).expect("drawn")
}

/// ⌘⌥⇟/⇞ step between workspaces, ⌘⌥N jumps to one, ⌘⌥` goes back to the one before, and
/// the keyboard follows the focus into each terminal.
#[gpui::test]
fn the_workspace_keys_switch_workspaces(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [_, (s2, second), (s3, third)] = three_shells(&view, cx, &fake);
    view.update_in(cx, |v, _w, cx| {
        v.tick();
        v.layout.move_column_to_workspace_down();
        v.after_focus_moved(cx);
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(workspace_of(&view, cx, third), 1);

    press(cx, "cmd-alt-pageup");
    assert_eq!((active_workspace(&view, cx), focused(&view, cx)), (0, Some(second)));
    assert!(terminal_focused(&view, cx, s2), "the keyboard followed");
    press(cx, "cmd-alt-pagedown");
    assert_eq!((active_workspace(&view, cx), focused(&view, cx)), (1, Some(third)));
    assert!(terminal_focused(&view, cx, s3));

    press(cx, "cmd-alt-1");
    assert_eq!(active_workspace(&view, cx), 0);
    press(cx, "cmd-alt-2");
    assert_eq!(active_workspace(&view, cx), 1);
    press(cx, "cmd-alt-9");
    assert_eq!(active_workspace(&view, cx), 2, "past the last: the trailing empty one");
    press(cx, "cmd-alt-1");
    press(cx, "cmd-alt-2");
    press(cx, "cmd-alt-`");
    assert_eq!(active_workspace(&view, cx), 0, "back where it was before");
}

/// ⌃⌘⌥⇟/⇞ carry the focused column to the next workspace and follow it; ⌃⌘⌥N carries it to
/// workspace N.
#[gpui::test]
fn the_carry_keys_take_the_column_to_another_workspace(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(_, first), _, (s3, third)] = three_shells(&view, cx, &fake);

    press(cx, "ctrl-cmd-alt-pagedown");
    assert_eq!((workspace_of(&view, cx, third), active_workspace(&view, cx)), (1, 1));
    assert_eq!(focused(&view, cx), Some(third));
    assert!(terminal_focused(&view, cx, s3), "the keyboard followed");
    press(cx, "ctrl-cmd-alt-pageup");
    assert_eq!((workspace_of(&view, cx, third), active_workspace(&view, cx)), (0, 0));

    press(cx, "ctrl-cmd-alt-3");
    assert_eq!(workspace_of(&view, cx, third), 1, "clamped to the trailing empty one");
    assert_eq!(workspace_of(&view, cx, first), 0);
    press(cx, "ctrl-cmd-alt-1");
    assert_eq!((workspace_of(&view, cx, third), active_workspace(&view, cx)), (0, 0));
}

/// ⌘⌥⇧⇞/⇟ swap the focused workspace with its neighbour, which stays focused.
#[gpui::test]
fn the_workspace_move_keys_reorder_workspaces(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(_, first), _, (_, third)] = three_shells(&view, cx, &fake);
    press(cx, "ctrl-cmd-alt-pagedown");

    press(cx, "cmd-alt-shift-pageup");
    assert_eq!((workspace_of(&view, cx, third), workspace_of(&view, cx, first)), (0, 1));
    assert_eq!((active_workspace(&view, cx), focused(&view, cx)), (0, Some(third)));
    press(cx, "cmd-alt-shift-pagedown");
    assert_eq!((workspace_of(&view, cx, third), workspace_of(&view, cx, first)), (1, 0));
    assert_eq!(active_workspace(&view, cx), 1);
}

/// ⌘⌥Home/End focus the first and last column; with ⇧ they carry the focused one there.
#[gpui::test]
fn the_end_keys_reach_and_carry_to_the_ends_of_the_strip(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(_, first), (_, second), (_, third)] = three_shells(&view, cx, &fake);

    press(cx, "cmd-alt-home");
    assert_eq!(focused(&view, cx), Some(first));
    press(cx, "cmd-alt-end");
    assert_eq!(focused(&view, cx), Some(third));

    press(cx, "cmd-alt-shift-home");
    assert_eq!(column_of(&view, cx, third), 0);
    assert_eq!((column_of(&view, cx, first), column_of(&view, cx, second)), (1, 2));
    assert_eq!(focused(&view, cx), Some(third), "and still focused");
    press(cx, "cmd-alt-shift-end");
    assert_eq!(column_of(&view, cx, third), 2);
}

/// With a third and a half column in view, ⌘⌥⇧C centres the two as a group, and ⌘⌥⇧F widens
/// the focused one over the room they leave.
#[gpui::test]
fn the_visible_column_keys_centre_and_fill(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(_, first), (_, second), _] = three_shells(&view, cx, &fake);
    press(cx, "cmd-alt-home");
    press(cx, "cmd-shift-r");
    let strip = cx.debug_bounds("strip").expect("the strip");
    let gaps = |cx: &mut VisualTestContext| {
        let (a, b) = (
            cx.debug_bounds(selector("item", first.item)).expect("drawn"),
            cx.debug_bounds(selector("item", second.item)).expect("drawn"),
        );
        (f32::from(a.left() - strip.left()), f32::from(strip.right() - b.right()))
    };
    let (left, right) = gaps(cx);
    assert!(right - left > 50.0, "the free room is on the right: {left} / {right}");

    press(cx, "cmd-alt-shift-c");
    let (left, right) = gaps(cx);
    assert!((left - right).abs() < 2.0, "centred as a group: {left} / {right}");

    let narrow = width(cx, first);
    press(cx, "cmd-alt-shift-f");
    let wide = width(cx, first);
    assert!(wide > narrow + 50.0, "filled the free width: {narrow} → {wide}");
    let (left, right) = gaps(cx);
    assert!(left.abs() < 2.0 && right.abs() < 2.0, "the two span the strip: {left} / {right}");
}

/// Every one of these ops has a palette line that shows its key.
#[test]
fn every_niri_op_has_a_palette_line_with_its_key() {
    let items = palette_items();
    for label in [
        "Workspace above",
        "Workspace below",
        "Previous workspace",
        "First workspace",
        "Move column to the start",
        "Move column to the end",
        "Move column to the workspace above",
        "Move column to the workspace below",
        "Move column to the first workspace",
        "Move workspace up",
        "Move workspace down",
        "Center the visible columns",
        "Fill the free width",
        "Last column",
    ] {
        let line = items.iter().find(|i| i.label == label).unwrap_or_else(|| panic!("{label}"));
        assert!(!line.keys.is_empty(), "{label} shows no key");
    }
}
