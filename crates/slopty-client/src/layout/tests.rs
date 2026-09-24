//! The layout model against niri's numbers. Desktop default: a 1280 × 800 viewport, gaps 8, so
//! a half-width column is `(1280 − 8) × 0.5 − 8 = 628` wide and column `i` starts at `636 i`.

use std::str::FromStr as _;

use super::*;

const MS: fn(u64) -> Duration = Duration::from_millis;
const HALF: f32 = 628.0;
const STEP: f32 = 636.0;

/// Where column `i` of half-width columns starts, in strip coordinates.
fn col_x(i: u8) -> f32 {
    f32::from(i) * STEP
}

fn item(n: u128) -> ItemId {
    ItemId::from_str(&format!("00000000-0000-0000-0000-{n:012x}")).unwrap()
}

fn tile(worker: u128, n: u128) -> TileRef {
    TileRef { worker: WorkerKey::new(worker), item: item(n) }
}

/// Worker 1's item `n`.
fn t(n: u128) -> TileRef {
    tile(1, n)
}

fn still() -> Layout {
    Layout::new(LayoutConfig { animate: false, ..LayoutConfig::default() })
}

fn moving() -> Layout {
    Layout::new(LayoutConfig::default())
}

/// A still layout with worker 1's items `1..=n`, each opened locally (column by column).
fn columns(n: u128) -> Layout {
    let mut l = still();
    for i in 1..=n {
        l.open(t(i), Placement::Local);
    }
    l
}

#[track_caller]
fn near(a: f32, b: f32) {
    assert!((a - b).abs() < 0.01, "{a} != {b}");
}

fn placed(l: &Layout, tile: TileRef) -> Placed {
    *l.frame().tiles.iter().find(|p| p.tile == tile).unwrap()
}

fn rect(l: &Layout, tile: TileRef) -> Rect {
    placed(l, tile).rect
}

fn view_pos(l: &Layout) -> f32 {
    l.frame().strip.view.0
}

/// Workspaces → columns → tiles.
fn shape(l: &Layout) -> Vec<Vec<Vec<TileRef>>> {
    l.workspaces()
        .iter()
        .map(|ws| ws.columns().iter().map(|c| c.tiles().iter().map(Tile::tile).collect()).collect())
        .collect()
}

fn active_col(l: &Layout) -> usize {
    l.workspaces()[l.active_workspace()].active_column()
}

fn width_of(l: &Layout, tile: TileRef) -> f32 {
    rect(l, tile).w
}

// ----- the pieces ---------------------------------------------------------------------------

#[test]
fn compute_new_view_offset_moves_the_view_as_little_as_it_can() {
    // Wider than the view: its left edge at the view's.
    near(compute_new_view_offset(100.0, 1280.0, 0.0, 1300.0, 8.0), 0.0);
    // Already fully visible, padding included: the view stays (offset = cur − col).
    near(compute_new_view_offset(-8.0, 1280.0, 636.0, 628.0, 8.0), -644.0);
    // Off to the right: right-aligned with the padding.
    near(compute_new_view_offset(-8.0, 1280.0, 1272.0, 628.0, 8.0), -644.0);
    // Off to the left: left-aligned with the padding.
    near(compute_new_view_offset(628.0, 1280.0, 0.0, 628.0, 8.0), -8.0);
    // The padding shrinks to what is left: a 1276 column in a 1280 view gets 2 each side.
    near(compute_new_view_offset(500.0, 1280.0, 0.0, 1276.0, 8.0), -2.0);
    // Equidistant: left wins.
    near(compute_new_view_offset(-100.0, 1000.0, 400.0, 1000.0 - 800.0 + 600.0, 100.0), -100.0);
}

#[test]
fn a_worker_key_prints_and_serialises_as_32_hex_digits() {
    let key = WorkerKey::new(0xab);
    assert_eq!(key.to_string(), format!("{:032x}", 0xab));
    assert_eq!(format!("{key:?}"), format!("WorkerKey({key})"));
    let json = serde_json::to_string(&key).unwrap();
    assert_eq!(json, format!("\"{key}\""));
    assert_eq!(serde_json::from_str::<WorkerKey>(&json).unwrap(), key);
    assert!(serde_json::from_str::<WorkerKey>("\"not hex\"").is_err(), "garbage is refused");
    // The first 16 bytes, big-endian; short input zero-padded at the end.
    let long: Vec<u8> = (1..=20).collect();
    assert_eq!(
        WorkerKey::from_bytes(&long).value(),
        u128::from_be_bytes(long[..16].try_into().unwrap())
    );
    assert_eq!(WorkerKey::from_bytes(&[0xff]).value(), 0xff_u128 << 120);
    assert_eq!(WorkerKey::from_bytes(&[]).value(), 0);
    // A tile ref round-trips.
    let r = tile(7, 9);
    let back: TileRef = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
    assert_eq!(back, r);
}

#[test]
fn a_rect_contains_its_top_left_but_not_its_far_edges() {
    let r = Rect { x: 10.0, y: 10.0, w: 100.0, h: 50.0 };
    assert!(r.contains(10.0, 10.0));
    assert!(r.contains(109.9, 59.9));
    assert!(!r.contains(110.0, 30.0) && !r.contains(50.0, 60.0) && !r.contains(9.9, 30.0));
    assert!(r.intersects(&Rect { x: 100.0, y: 50.0, w: 20.0, h: 20.0 }));
    assert!(
        !r.intersects(&Rect { x: 110.0, y: 10.0, w: 20.0, h: 20.0 }),
        "touching is not meeting"
    );
    assert!(!r.intersects(&Rect { x: 0.0, y: 60.0, w: 500.0, h: 5.0 }));
}

// ----- placement ----------------------------------------------------------------------------

#[test]
fn local_tiles_open_right_of_the_focus_and_the_view_moves_the_least() {
    let mut l = still();
    l.open(t(1), Placement::Local);
    let a = rect(&l, t(1));
    near(a.x, (1280.0 - HALF) / 2.0);
    near(a.w, HALF);
    near(a.y, 8.0);
    near(a.h, 800.0 - 16.0);
    assert_eq!(l.focused(), Some(t(1)));

    // The second: the view leaves the lone column's centre only as far as showing both needs.
    l.open(t(2), Placement::Local);
    near(view_pos(&l), -8.0);
    near(rect(&l, t(2)).x, 8.0 + STEP);
    assert_eq!(l.focused(), Some(t(2)));

    // The third does not: the view right-aligns it (636 instead of 1272 to left-align).
    l.open(t(3), Placement::Local);
    near(view_pos(&l), col_x(2) - 644.0);
    near(rect(&l, t(3)).right(), 1280.0 - 8.0);

    // Local opens next to the focus, not at the end.
    l.focus_column_first();
    l.open(t(4), Placement::Local);
    assert_eq!(shape(&l)[0], vec![vec![t(1)], vec![t(4)], vec![t(2)], vec![t(3)]]);
    assert_eq!(l.focused(), Some(t(4)));
    // Opening what is already there does nothing.
    l.open(t(4), Placement::Remote);
    assert_eq!(l.tiles().count(), 4);
}

#[test]
fn remote_tiles_join_the_workspace_that_last_held_their_worker() {
    let mut l = still();
    // Worker 1 fills the empty active workspace, focused.
    l.open(tile(1, 1), Placement::Remote);
    assert_eq!(l.focused(), Some(tile(1, 1)));
    assert_eq!(l.workspaces().len(), 2, "a trailing empty workspace, always");

    // Worker 2 has no tiles and the active workspace is taken: a new workspace above the
    // trailing one, focus untouched.
    l.open(tile(2, 1), Placement::Remote);
    assert_eq!(shape(&l), vec![vec![vec![tile(1, 1)]], vec![vec![tile(2, 1)]], vec![]]);
    assert_eq!(l.active_workspace(), 0);
    assert_eq!(l.focused(), Some(tile(1, 1)));

    // More of worker 1: appended at the end of its workspace, not focused.
    l.open(tile(1, 2), Placement::Local);
    l.focus_column_first();
    l.open(tile(1, 3), Placement::Remote);
    assert_eq!(shape(&l)[0], vec![vec![tile(1, 1)], vec![tile(1, 2)], vec![tile(1, 3)]]);
    assert_eq!(l.focused(), Some(tile(1, 1)));

    // Worker 1's tile moved to worker 2's workspace, which is then visited: that one is now
    // the most recent holder of worker 1.
    l.focus(tile(1, 3));
    l.move_column_to_workspace_down();
    assert_eq!(l.active_workspace(), 1);
    l.focus_workspace_up();
    l.focus_workspace_down();
    l.focus_workspace_up();
    // Workspace 0 was active last: it wins.
    l.open(tile(1, 4), Placement::Remote);
    assert_eq!(l.position(tile(1, 4)).unwrap().workspace, 0);
    l.focus_workspace_down();
    l.open(tile(1, 5), Placement::Remote);
    assert_eq!(l.position(tile(1, 5)).unwrap().workspace, 1, "now workspace 1 is the newer");
    assert!(l.workspaces().last().unwrap().is_empty());
    assert_eq!(l.workspaces()[1].workers(), BTreeSet::from([WorkerKey::new(1), WorkerKey::new(2)]));
}

#[test]
fn there_is_always_exactly_one_trailing_empty_workspace() {
    let mut l = still();
    assert_eq!(l.workspaces().len(), 1);
    l.open(t(1), Placement::Local);
    assert_eq!(l.workspaces().len(), 2);
    // Go to the empty one and open there: another appears.
    l.focus_workspace_down();
    l.open(t(2), Placement::Local);
    assert_eq!(shape(&l), vec![vec![vec![t(1)]], vec![vec![t(2)]], vec![]]);
    // Emptying a workspace that is not active removes it.
    l.remove(t(1));
    assert_eq!(shape(&l), vec![vec![vec![t(2)]], vec![]]);
    assert_eq!(l.active_workspace(), 0, "the index followed the removal");
    // Emptying the active one keeps it until the focus leaves.
    l.open(tile(2, 3), Placement::Remote);
    l.remove(t(2));
    assert_eq!(l.workspaces().len(), 3);
    l.focus_workspace_down();
    assert_eq!(shape(&l), vec![vec![vec![tile(2, 3)]], vec![]]);
}

// ----- removal ------------------------------------------------------------------------------

#[test]
fn closing_a_just_opened_column_goes_back_to_where_the_view_was() {
    let mut l = columns(2);
    near(view_pos(&l), -8.0);
    l.open(t(3), Placement::Local);
    near(view_pos(&l), 628.0);
    l.remove(t(3));
    assert_eq!(l.focused(), Some(t(2)), "the one it was opened from");
    near(view_pos(&l), -8.0);
}

#[test]
fn closing_after_the_focus_moved_takes_the_next_column_in_place() {
    let mut l = columns(3);
    l.focus_column_left();
    l.focus_column_right();
    // The memory is gone: the right neighbour, or the last one, takes the focus.
    l.remove(t(3));
    assert_eq!(l.focused(), Some(t(2)));
    near(view_pos(&l), 628.0);
    l.focus_column_first();
    l.remove(t(1));
    assert_eq!(l.focused(), Some(t(2)), "the column that slid into its place");
    near(rect(&l, t(2)).x, (1280.0 - HALF) / 2.0);
    l.remove(t(2));
    assert_eq!(l.focused(), None);
    l.remove(t(2));
}

#[test]
fn closing_a_column_left_of_the_focus_keeps_the_focused_column_still() {
    let mut l = columns(3);
    let before = rect(&l, t(3));
    l.remove(t(1));
    assert_eq!(l.focused(), Some(t(3)));
    assert_eq!(rect(&l, t(3)), before);
}

#[test]
fn closing_a_tile_in_a_column_keeps_the_column() {
    let mut l = columns(2);
    l.consume_or_expel_window_left();
    assert_eq!(shape(&l)[0], vec![vec![t(1), t(2)]]);
    l.focus_window_up();
    l.remove(t(1));
    assert_eq!(shape(&l)[0], vec![vec![t(2)]]);
    assert_eq!(l.focused(), Some(t(2)));
    near(rect(&l, t(2)).h, 784.0);
}

#[test]
fn retain_worker_drops_only_that_workers_missing_items() {
    let mut l = still();
    l.open(tile(1, 1), Placement::Local);
    l.open(tile(1, 2), Placement::Local);
    l.open(tile(2, 1), Placement::Local);
    l.open(tile(2, 2), Placement::Local);
    l.retain_worker(WorkerKey::new(1), |i| i == item(2));
    let left: Vec<TileRef> = l.tiles().collect();
    assert_eq!(left, vec![tile(1, 2), tile(2, 1), tile(2, 2)]);
    l.retain_worker(WorkerKey::new(3), |_| false);
    assert_eq!(l.tiles().count(), 3, "an unknown worker has nothing to drop");
}

// ----- focus --------------------------------------------------------------------------------

#[test]
fn focus_moves_stop_at_the_ends() {
    let mut l = columns(3);
    l.focus_column_right();
    assert_eq!(active_col(&l), 2);
    l.focus_column_first();
    l.focus_column_left();
    assert_eq!(active_col(&l), 0);
    l.focus_column(1);
    assert_eq!(active_col(&l), 1);
    l.focus_column(99);
    assert_eq!(active_col(&l), 2, "clamped to the last");
    l.strip_jump(0);
    assert_eq!(active_col(&l), 0);
    l.focus_column_last();
    assert_eq!(active_col(&l), 2);
    // Nothing to do on an empty workspace.
    let mut e = still();
    e.focus_column_left();
    e.focus_column_right();
    e.focus_window_up();
    e.focus_column(3);
    assert_eq!(e.focused(), None);
}

#[test]
fn focus_window_or_workspace_walks_the_column_then_the_workspaces() {
    let mut l = columns(2);
    l.consume_or_expel_window_left();
    l.open(tile(2, 9), Placement::Remote);
    // Workspace 0: [1, 2]; workspace 1: [w2]. Focus is on 2 (bottom).
    assert_eq!(l.focused(), Some(t(2)));
    l.focus_window_or_workspace_up();
    assert_eq!(l.focused(), Some(t(1)));
    l.focus_window_or_workspace_down();
    assert_eq!(l.focused(), Some(t(2)));
    l.focus_window_or_workspace_down();
    assert_eq!(l.active_workspace(), 1);
    assert_eq!(l.focused(), Some(tile(2, 9)));
    l.focus_window_or_workspace_up();
    assert_eq!(l.active_workspace(), 0);
    assert_eq!(l.focused(), Some(t(2)), "the column remembers its tile");
    l.focus_window_or_workspace_up();
    l.focus_window_or_workspace_up();
    assert_eq!(l.active_workspace(), 0, "no workspace above the first");
}

#[test]
fn focusing_a_tile_brings_its_workspace_and_column() {
    let mut l = columns(3);
    l.open(tile(2, 1), Placement::Remote);
    l.focus(tile(2, 1));
    assert_eq!(l.active_workspace(), 1);
    l.focus(t(1));
    assert_eq!((l.active_workspace(), active_col(&l)), (0, 0));
    near(view_pos(&l), -8.0);
    l.focus_workspace_previous();
    assert_eq!(l.active_workspace(), 1);
    l.focus_workspace_previous();
    assert_eq!(l.active_workspace(), 0);
    l.focus(tile(9, 9));
    assert_eq!(l.focused(), Some(t(1)), "an unknown tile changes nothing");
}

// ----- moves --------------------------------------------------------------------------------

#[test]
fn moving_a_column_keeps_the_camera() {
    let mut l = columns(3);
    let view = view_pos(&l);
    l.move_column_left();
    assert_eq!(shape(&l)[0], vec![vec![t(1)], vec![t(3)], vec![t(2)]]);
    assert_eq!(l.focused(), Some(t(3)));
    near(view_pos(&l), view);
    l.move_column_to_first();
    assert_eq!(shape(&l)[0], vec![vec![t(3)], vec![t(1)], vec![t(2)]]);
    near(view_pos(&l), -8.0);
    l.move_column_left();
    assert_eq!(shape(&l)[0][0], vec![t(3)], "nothing left of the first");
    l.move_column_to_last();
    assert_eq!(shape(&l)[0], vec![vec![t(1)], vec![t(2)], vec![t(3)]]);
    l.move_column_right();
    assert_eq!(shape(&l)[0][2], vec![t(3)], "nothing right of the last");
}

#[test]
fn moving_a_window_within_its_column_and_out_to_the_next_workspace() {
    let mut l = columns(2);
    l.consume_or_expel_window_left();
    l.move_window_up();
    assert_eq!(shape(&l)[0], vec![vec![t(2), t(1)]]);
    assert_eq!(l.focused(), Some(t(2)));
    l.move_window_up();
    assert_eq!(shape(&l)[0], vec![vec![t(2), t(1)]], "already at the top");
    l.move_window_down();
    assert_eq!(shape(&l)[0], vec![vec![t(1), t(2)]]);
    // At the bottom: out to the workspace below, in a column of its own, focused.
    l.move_window_down_or_to_workspace_down();
    assert_eq!(shape(&l), vec![vec![vec![t(1)]], vec![vec![t(2)]], vec![]]);
    assert_eq!((l.active_workspace(), l.focused()), (1, Some(t(2))));
    // Up from the only tile: back up, which empties workspace 1 and drops it.
    l.move_window_up_or_to_workspace_up();
    assert_eq!(shape(&l), vec![vec![vec![t(1)], vec![t(2)]], vec![]]);
    assert_eq!((l.active_workspace(), l.focused()), (0, Some(t(2))));
    l.move_window_up_or_to_workspace_up();
    assert_eq!(shape(&l), vec![vec![vec![t(1)], vec![t(2)]], vec![]], "no workspace above");
}

#[test]
fn moving_a_column_to_another_workspace_keeps_its_width_and_tiles() {
    let mut l = columns(2);
    l.consume_or_expel_window_left();
    l.switch_preset_width(false);
    l.move_column_to_workspace_down();
    // Workspace 0 emptied and was dropped once left.
    assert_eq!(shape(&l), vec![vec![vec![t(1), t(2)]], vec![]]);
    assert_eq!(l.focused(), Some(t(2)));
    near(width_of(&l, t(1)), 416.0);
    l.move_column_to_workspace_up();
    assert_eq!(shape(&l), vec![vec![vec![t(1), t(2)]], vec![]], "nothing above");
}

#[test]
fn moving_a_workspace_swaps_it_with_its_neighbour() {
    let mut l = columns(1);
    l.open(tile(2, 1), Placement::Remote);
    l.move_workspace_down();
    assert_eq!(shape(&l), vec![vec![vec![tile(2, 1)]], vec![vec![t(1)]], vec![]]);
    assert_eq!(l.active_workspace(), 1);
    assert_eq!(l.focused(), Some(t(1)));
    // Past the trailing one: a new trailing one, and the empty one left behind goes (niri).
    l.move_workspace_down();
    assert_eq!(shape(&l), vec![vec![vec![tile(2, 1)]], vec![vec![t(1)]], vec![]]);
    assert_eq!(l.active_workspace(), 1);
    l.move_workspace_up();
    assert_eq!(shape(&l), vec![vec![vec![t(1)]], vec![vec![tile(2, 1)]], vec![]]);
    assert_eq!(l.active_workspace(), 0);
    l.move_workspace_up();
    assert_eq!(l.active_workspace(), 0, "nothing above the first");
}

// ----- consume and expel --------------------------------------------------------------------

#[test]
fn consume_or_expel_joins_a_lone_tile_to_its_neighbour_and_splits_a_stacked_one() {
    let mut l = columns(3);
    // Alone: into the column on the left, at the bottom, focused.
    l.consume_or_expel_window_left();
    assert_eq!(shape(&l)[0], vec![vec![t(1)], vec![t(2), t(3)]]);
    assert_eq!(l.focused(), Some(t(3)));
    let (a, b) = (rect(&l, t(2)), rect(&l, t(3)));
    near(a.h, (800.0 - 24.0) / 2.0);
    near(b.y, a.bottom() + 8.0);
    // Stacked: out into a new column on the left.
    l.consume_or_expel_window_left();
    assert_eq!(shape(&l)[0], vec![vec![t(1)], vec![t(3)], vec![t(2)]]);
    assert_eq!(l.focused(), Some(t(3)));
    // Right: alone joins the right neighbour.
    l.consume_or_expel_window_right();
    assert_eq!(shape(&l)[0], vec![vec![t(1)], vec![t(2), t(3)]]);
    assert_eq!(l.focused(), Some(t(3)));
    // Stacked: out to the right.
    l.consume_or_expel_window_right();
    assert_eq!(shape(&l)[0], vec![vec![t(1)], vec![t(2)], vec![t(3)]]);
    // At the ends nothing happens.
    l.consume_or_expel_window_right();
    assert_eq!(shape(&l)[0], vec![vec![t(1)], vec![t(2)], vec![t(3)]]);
    l.focus_column_first();
    l.consume_or_expel_window_left();
    assert_eq!(shape(&l)[0], vec![vec![t(1)], vec![t(2)], vec![t(3)]]);
}

#[test]
fn consume_into_and_expel_from_the_focused_column() {
    let mut l = columns(3);
    l.focus_column_first();
    l.consume_into_column();
    assert_eq!(shape(&l)[0], vec![vec![t(1), t(2)], vec![t(3)]]);
    assert_eq!(l.focused(), Some(t(1)), "the focus stays");
    l.consume_into_column();
    l.consume_into_column();
    assert_eq!(shape(&l)[0], vec![vec![t(1), t(2), t(3)]], "nothing more to take");
    l.expel_from_column();
    assert_eq!(shape(&l)[0], vec![vec![t(1), t(2)], vec![t(3)]], "the bottom one leaves");
    assert_eq!(l.focused(), Some(t(1)));
    l.expel_from_column();
    l.expel_from_column();
    assert_eq!(shape(&l)[0], vec![vec![t(1)], vec![t(2)], vec![t(3)]], "a lone tile stays");
}

// ----- widths -------------------------------------------------------------------------------

#[test]
fn presets_cycle_both_ways_from_a_preset_and_from_any_width() {
    let mut l = columns(1);
    // 628 is not marked as a preset: forward goes to the first one wider.
    l.switch_preset_width(true);
    near(width_of(&l, t(1)), 840.0);
    l.switch_preset_width(true);
    near(width_of(&l, t(1)), 416.0);
    l.switch_preset_width(true);
    near(width_of(&l, t(1)), HALF);
    l.switch_preset_width(false);
    near(width_of(&l, t(1)), 416.0);
    l.switch_preset_width(false);
    near(width_of(&l, t(1)), 840.0);
    // From a width between presets.
    l.set_width_delta(-10.0);
    let w = width_of(&l, t(1));
    assert!(w > HALF && w < 840.0, "{w}");
    l.switch_preset_width(false);
    near(width_of(&l, t(1)), HALF);
    l.set_width_delta(10.0);
    l.switch_preset_width(true);
    near(width_of(&l, t(1)), 840.0);
    // Backward from narrower than every preset: round to the widest.
    let mut l = columns(1);
    l.set_width_delta(-100.0);
    l.switch_preset_width(false);
    near(width_of(&l, t(1)), 840.0);
    assert_eq!(l.workspaces()[0].columns()[0].preset(), Some(2));
}

#[test]
fn width_steps_clamp_and_turn_a_fixed_width_proportional() {
    let mut l = columns(1);
    l.set_width_delta(10.0);
    near(width_of(&l, t(1)), 1272.0_f32.mul_add(0.6, -8.0));
    l.set_width_delta(100.0);
    near(width_of(&l, t(1)), 1264.0);
    l.set_width_delta(-200.0);
    near(width_of(&l, t(1)), 128.0);
    assert_eq!(l.workspaces()[0].columns()[0].preset(), None);
    // A fixed width (an interactive resize) steps from its proportion.
    l.resize_begin(0);
    l.resize_update(372.0);
    l.resize_end();
    near(width_of(&l, t(1)), 500.0);
    assert_eq!(l.workspaces()[0].columns()[0].width(), ColumnWidth::Fixed(500.0));
    l.set_width_delta(10.0);
    let ColumnWidth::Proportion(p) = l.workspaces()[0].columns()[0].width() else { panic!() };
    near(p, 508.0 / 1272.0 + 0.1);
    near(width_of(&l, t(1)), 500.0 + 127.2);
}

#[test]
fn full_width_maximises_and_comes_back() {
    let mut l = columns(2);
    l.toggle_full_width();
    near(width_of(&l, t(2)), 1264.0);
    near(rect(&l, t(2)).x, 8.0);
    assert!(l.workspaces()[0].columns()[1].is_full_width());
    l.toggle_full_width();
    near(width_of(&l, t(2)), HALF);
    // A preset step leaves full width, wrapping to the first preset (nothing is wider).
    l.toggle_full_width();
    l.switch_preset_width(true);
    assert!(!l.workspaces()[0].columns()[1].is_full_width());
    near(width_of(&l, t(2)), 416.0);
}

#[test]
fn fullscreen_fills_the_viewport_and_restores_the_view() {
    let mut l = columns(2);
    l.consume_or_expel_window_left();
    l.open(t(3), Placement::Local);
    l.focus_column_first();
    l.focus_window_up();
    // [1, 2], [3]; focus on 1. Fullscreen takes it out of its column first.
    l.toggle_fullscreen();
    assert_eq!(shape(&l)[0], vec![vec![t(2)], vec![t(1)], vec![t(3)]]);
    let p = placed(&l, t(1));
    assert!(p.fullscreen);
    assert_eq!(p.rect, Rect { x: 0.0, y: 0.0, w: 1280.0, h: 800.0 });
    l.toggle_fullscreen();
    let p = placed(&l, t(1));
    assert!(!p.fullscreen);
    near(p.rect.w, HALF);
    assert!(p.rect.x >= 0.0 && p.rect.right() <= 1280.0, "back in view: {p:?}");
    // A tile joining a fullscreen column ends its fullscreen.
    l.toggle_fullscreen();
    l.focus_column_last();
    l.consume_or_expel_window_left();
    assert!(!l.workspaces()[0].columns()[1].is_fullscreen());
    near(width_of(&l, t(3)), HALF);
}

#[test]
fn fullscreen_on_and_off_puts_the_view_back_exactly() {
    let mut l = columns(3);
    l.focus_column_left();
    let view = view_pos(&l);
    l.toggle_fullscreen();
    near(view_pos(&l), STEP);
    l.toggle_fullscreen();
    near(view_pos(&l), view);
}

#[test]
fn expand_and_centre_fill_and_balance_the_visible_space() {
    let mut l = columns(2);
    l.switch_preset_width(false);
    // [628][416] visible; the active 416 takes what is free.
    l.focus_column_first();
    l.focus_column_right();
    l.expand_to_available_width();
    let (a, b) = (rect(&l, t(1)), rect(&l, t(2)));
    near(a.x, 8.0);
    near(b.right(), 1272.0);
    near(b.x, a.right() + 8.0);
    // Alone on screen: full width.
    let mut l = columns(1);
    l.expand_to_available_width();
    near(width_of(&l, t(1)), 1264.0);
    // Centre one column, then the visible pair.
    let mut l = columns(2);
    l.switch_preset_width(true);
    l.switch_preset_width(true);
    l.center_column();
    let r = rect(&l, t(2));
    near(r.x, (1280.0 - r.w) / 2.0);
    let mut l = columns(2);
    l.switch_preset_width(true);
    l.switch_preset_width(true);
    l.focus_column_left();
    l.switch_preset_width(true);
    l.switch_preset_width(true);
    // Two 416s: 840 with the gap between, centred.
    l.center_visible_columns();
    near(rect(&l, t(1)).x, (1280.0 - 840.0) / 2.0);
    near(rect(&l, t(2)).right(), 1280.0 - (1280.0 - 840.0) / 2.0);
}

#[test]
fn tile_heights_split_by_weight_after_fixed_ones() {
    let mut col = Column::new(Tile::new(t(1)), ColumnWidth::Proportion(0.5), false);
    col.tiles.push(Tile { height: TileHeight::Fixed(200.0), ..Tile::new(t(2)) });
    col.tiles.push(Tile { height: TileHeight::Auto { weight: 2.0 }, ..Tile::new(t(3)) });
    let g = Geom { view_w: 1280.0, view_h: 800.0, strut: 0.0, gaps: 8.0 };
    let r = col.tile_rects(&g);
    let left = 800.0 - 32.0 - 200.0;
    near(r[0].h, left / 3.0);
    near(r[1].h, 200.0);
    near(r[2].h, left * 2.0 / 3.0);
    near(r[2].bottom(), 792.0);
    near(r[1].y, r[0].bottom() + 8.0);
}

// ----- tabbed -------------------------------------------------------------------------------

#[test]
fn a_tabbed_column_shows_one_tile_at_full_height() {
    let mut l = columns(3);
    l.consume_or_expel_window_left();
    l.focus_column_first();
    l.consume_into_column();
    l.consume_into_column();
    l.toggle_tabbed();
    assert_eq!(l.workspaces()[0].columns()[0].mode(), DisplayMode::Tabbed);
    let f = l.frame();
    for p in &f.tiles {
        assert_eq!(p.tabs, Some((0, 3)));
        near(p.rect.h, 784.0);
        near(p.rect.y, 8.0);
        assert_eq!(p.hidden, p.tile != t(1), "{p:?}");
    }
    l.focus_window_down();
    assert!(!placed(&l, t(2)).hidden && placed(&l, t(1)).hidden);
    // Dropping onto a tabbed column (alone, so centred) appends.
    let target = l.drop_target(640.0, 100.0);
    assert_eq!(target, Some(DropTarget::IntoColumn { workspace: 0, column: 0, index: 3 }));
    l.toggle_tabbed();
    assert!(l.frame().tiles.iter().all(|p| !p.hidden && p.tabs.is_none()));
}

// ----- workspaces ---------------------------------------------------------------------------

#[test]
fn a_named_workspace_stays_when_empty() {
    let mut l = columns(1);
    l.set_workspace_name(1, Some("  build  ".to_owned()));
    assert_eq!(l.workspaces()[1].name(), Some("build"));
    assert_eq!(l.workspaces().len(), 3, "the named one is no longer the trailing empty one");
    l.focus_workspace_down();
    l.focus_workspace_up();
    assert_eq!(l.workspaces().len(), 3, "kept while empty");
    l.set_workspace_name(1, Some("   ".to_owned()));
    assert_eq!(l.workspaces()[1].name(), None);
    assert_eq!(l.workspaces().len(), 2, "blank unnames it and it goes");
    l.set_workspace_name(9, Some("nowhere".to_owned()));
    assert_eq!(l.workspaces().len(), 2);
}

#[test]
fn switching_workspaces_springs_and_cleans_up_when_it_lands() {
    let mut l = moving();
    l.open(t(1), Placement::Local);
    l.open(tile(2, 1), Placement::Remote);
    l.set_clock(MS(1000));
    l.focus_workspace_down();
    let f = l.frame();
    near(f.workspaces[0].1.y, 0.0);
    assert!(f.animating);
    l.set_clock(MS(1100));
    let y = l.frame().workspaces[1].1.y;
    assert!(y > 0.0 && y < 880.0, "{y}");
    l.set_clock(MS(3000));
    let f = l.frame();
    near(f.workspaces[1].1.y, 0.0);
    near(f.workspaces[0].1.y, -880.0);
    assert!(!f.animating);
    // The source emptied mid-switch stays until the switch lands.
    l.focus(t(1));
    l.set_clock(MS(3010));
    l.remove(t(1));
    l.focus_workspace_down();
    assert_eq!(l.workspaces().len(), 3, "a switch is running");
    l.set_clock(MS(6000));
    assert_eq!(shape(&l), vec![vec![vec![tile(2, 1)]], vec![]]);
}

// ----- overview and drops -------------------------------------------------------------------

#[test]
fn the_overview_zooms_out_by_half_with_a_gap_between_rows() {
    let mut l = columns(1);
    l.open(tile(2, 1), Placement::Remote);
    l.set_overview(true);
    let f = l.frame();
    near(f.zoom, 0.5);
    near(f.overview, 1.0);
    assert_eq!(f.workspaces[0].1, Rect { x: 320.0, y: 200.0, w: 640.0, h: 400.0 });
    near(f.workspaces[1].1.y, 200.0 + 400.0 + 40.0);
    let a = rect(&l, t(1));
    near(a.x, 320.0 + (1280.0 - HALF) / 4.0);
    near(a.w, HALF / 2.0);
    l.toggle_overview();
    near(l.frame().zoom, 1.0);
}

/// niri's `always-center-single-column`: a lone column sits in the middle of the window, and
/// stays there when the window or the column changes width; a second column brings back the
/// usual fit.
#[test]
fn a_lone_column_is_centred_and_stays_centred() {
    let mut l = columns(1);
    let centred = |l: &Layout| {
        let r = rect(l, t(1));
        near(r.x, (l.viewport().0 - r.w) / 2.0);
    };
    centred(&l);
    l.set_viewport(1600.0, 900.0);
    centred(&l);
    l.switch_preset_width(true);
    centred(&l);
    l.set_viewport(1280.0, 800.0);
    centred(&l);
    l.open(t(2), Placement::Local);
    l.focus_column_first();
    l.remove(t(2));
    centred(&l);
}

/// In the overview a strip that fits the zoomed-out window is centred in it, whatever column
/// the view was scrolled to; the view itself does not move, so closing the overview lands
/// where it was, and a drop is found where the tile is drawn.
#[test]
fn the_overview_centres_a_strip_that_fits() {
    let mut l = columns(3);
    let before = view_pos(&l);
    l.set_overview(true);
    let (first, last) = (rect(&l, t(1)), rect(&l, t(3)));
    near(first.x, 1280.0 - last.right());
    near(view_pos(&l), before);
    let panel = l.frame().workspaces[0].1;
    assert!(panel.x < first.x && panel.right() > last.right(), "the panel holds it: {panel:?}");
    let hit = l.drop_target(first.x + first.w / 2.0, first.y + first.h / 2.0);
    assert!(matches!(hit, Some(DropTarget::IntoColumn { workspace: 0, column: 0, .. })), "{hit:?}");
    l.set_overview(false);
    near(view_pos(&l), before);
    near(rect(&l, t(3)).right(), 1280.0 - 8.0);
}

#[test]
fn overview_drops_land_in_columns_between_columns_and_between_workspaces() {
    let mut l = columns(2);
    l.consume_or_expel_window_left();
    l.open(t(3), Placement::Local);
    // Workspace 0: [1, 2], [3], view at -8.
    l.set_overview(true);
    // Column 0 spans x 324..638 at zoom 0.5; tile 1 is its top half.
    let into = |y: f32| l.drop_target(480.0, y);
    assert_eq!(into(260.0), Some(DropTarget::IntoColumn { workspace: 0, column: 0, index: 0 }));
    assert_eq!(into(340.0), Some(DropTarget::IntoColumn { workspace: 0, column: 0, index: 1 }));
    assert_eq!(into(560.0), Some(DropTarget::IntoColumn { workspace: 0, column: 0, index: 2 }));
    // The outer 20 % of a column: a new column beside it.
    assert_eq!(l.drop_target(330.0, 300.0), Some(DropTarget::NewColumn { workspace: 0, index: 0 }));
    assert_eq!(l.drop_target(630.0, 300.0), Some(DropTarget::NewColumn { workspace: 0, index: 1 }));
    // The gap between columns, and past the last one.
    assert_eq!(l.drop_target(639.0, 300.0), Some(DropTarget::NewColumn { workspace: 0, index: 1 }));
    assert_eq!(l.drop_target(955.0, 300.0), Some(DropTarget::NewColumn { workspace: 0, index: 2 }));
    // A row reaches the full width of the viewport.
    assert_eq!(
        l.drop_target(1200.0, 300.0),
        Some(DropTarget::NewColumn { workspace: 0, index: 2 })
    );
    // Between rows, above the first: a new workspace.
    assert_eq!(l.drop_target(640.0, 620.0), Some(DropTarget::NewWorkspace { index: 1 }));
    assert_eq!(l.drop_target(640.0, 100.0), Some(DropTarget::NewWorkspace { index: 0 }));
    // The trailing empty row takes a new column.
    assert_eq!(l.drop_target(640.0, 700.0), Some(DropTarget::NewColumn { workspace: 1, index: 0 }));
    // Out of the overview there is no gap to hit.
    l.set_overview(false);
    assert_eq!(l.drop_target(640.0, 900.0), None);
    assert_eq!(
        l.drop_target(300.0, 100.0),
        Some(DropTarget::IntoColumn { workspace: 0, column: 0, index: 0 })
    );
    assert_eq!(l.drop_target(100.0, 100.0), Some(DropTarget::NewColumn { workspace: 0, index: 0 }));
}

#[test]
fn moving_a_tile_to_a_drop_target() {
    let mut l = columns(3);
    // 3 into the bottom of column 0.
    l.move_tile(t(3), DropTarget::IntoColumn { workspace: 0, column: 0, index: 1 });
    assert_eq!(shape(&l)[0], vec![vec![t(1), t(3)], vec![t(2)]]);
    assert_eq!(l.focused(), Some(t(3)));
    // 2's column goes; the index given was read before it went.
    l.move_tile(t(2), DropTarget::NewColumn { workspace: 0, index: 0 });
    assert_eq!(shape(&l)[0], vec![vec![t(2)], vec![t(1), t(3)]]);
    l.move_tile(t(2), DropTarget::IntoColumn { workspace: 0, column: 1, index: 0 });
    assert_eq!(shape(&l)[0], vec![vec![t(2), t(1), t(3)]]);
    // Within its own column, down one.
    l.move_tile(t(2), DropTarget::IntoColumn { workspace: 0, column: 0, index: 2 });
    assert_eq!(shape(&l)[0], vec![vec![t(1), t(2), t(3)]]);
    // Onto itself: nothing but the focus.
    l.move_tile(t(2), DropTarget::IntoColumn { workspace: 0, column: 0, index: 1 });
    assert_eq!(shape(&l)[0], vec![vec![t(1), t(2), t(3)]]);
    // Out of the column to the right, where the old column still counts.
    l.move_tile(t(1), DropTarget::NewColumn { workspace: 0, index: 1 });
    assert_eq!(shape(&l)[0], vec![vec![t(2), t(3)], vec![t(1)]]);
    // A new workspace; past the end means above the trailing one.
    l.move_tile(t(3), DropTarget::NewWorkspace { index: 0 });
    assert_eq!(shape(&l), vec![vec![vec![t(3)]], vec![vec![t(2)], vec![t(1)]], vec![]]);
    assert_eq!((l.active_workspace(), l.focused()), (0, Some(t(3))));
    l.move_tile(t(3), DropTarget::NewWorkspace { index: 99 });
    assert_eq!(shape(&l), vec![vec![vec![t(2)], vec![t(1)]], vec![vec![t(3)]], vec![]]);
    // Into the trailing empty one: a new trailing one appears.
    l.move_tile(t(1), DropTarget::NewColumn { workspace: 2, index: 0 });
    assert_eq!(shape(&l).len(), 4);
    assert_eq!(l.position(t(1)).unwrap().workspace, 2);
    // Nonsense targets do not lose the tile.
    l.move_tile(t(1), DropTarget::IntoColumn { workspace: 9, column: 9, index: 9 });
    assert!(l.contains(t(1)));
}

#[test]
fn dragging_to_the_edge_scrolls_the_strip_after_a_delay() {
    let mut l = columns(4);
    l.focus_column_first();
    near(view_pos(&l), -8.0);
    l.set_clock(MS(1000));
    assert!(!l.dnd_edge_scroll(640.0), "the middle does not scroll");
    assert!(l.dnd_edge_scroll(1275.0));
    l.set_clock(MS(1050));
    assert!(l.dnd_edge_scroll(1275.0));
    near(view_pos(&l), -8.0);
    // Past the 100 ms delay: 5 of 30 pt from the edge is 5/6 of 1500 pt/s, for the 100 ms
    // since the last call.
    l.set_clock(MS(1150));
    l.dnd_edge_scroll(1275.0);
    near(view_pos(&l), -8.0 + 125.0);
    l.set_clock(MS(1250));
    l.dnd_edge_scroll(1275.0);
    near(view_pos(&l), -8.0 + 250.0);
    // Far to the right it stops with the view's left edge on the last column's right edge.
    for step in 13..60 {
        l.set_clock(MS(step * 100));
        l.dnd_edge_scroll(1280.0);
    }
    near(view_pos(&l), col_x(3) + HALF);
    // Letting go snaps like a swipe.
    l.dnd_scroll_end();
    near(view_pos(&l), col_x(3) - 644.0);
    assert_eq!(active_col(&l), 3);
    // A drag that never scrolled leaves the view alone.
    l.set_clock(MS(7000));
    l.dnd_edge_scroll(640.0);
    l.dnd_scroll_end();
    near(view_pos(&l), col_x(3) - 644.0);
}

// ----- gestures -----------------------------------------------------------------------------

/// Drag the strip by `total` in `steps` readings `every` ms apart, then let go at `release`.
fn swipe(l: &mut Layout, start: u64, total: f32, steps: u16, every: u64, release: u64) {
    l.set_clock(MS(start));
    l.view_gesture_begin();
    let mut at = start;
    for _ in 0..steps {
        at = at.saturating_add(every);
        assert!(l.view_gesture_update(total / f32::from(steps), MS(at)));
    }
    l.set_clock(MS(release));
    assert!(l.view_gesture_end(false));
}

#[test]
fn a_slow_drag_snaps_to_the_nearest_column_edge() {
    let mut l = columns(3);
    l.focus_column_first();
    // Not far enough: the view goes back, and the focus goes the way of the drag to the
    // furthest column fully in view (niri).
    swipe(&mut l, 0, 200.0, 10, 100, 1300);
    l.set_clock(MS(5000));
    near(view_pos(&l), -8.0);
    assert_eq!(active_col(&l), 1);
    l.focus_column_first();
    // Most of the way to the end snap: there, the last column focused.
    swipe(&mut l, 6000, 400.0, 10, 100, 7300);
    l.set_clock(MS(10_000));
    near(view_pos(&l), 628.0);
    assert_eq!(active_col(&l), 2);
    assert!(!l.view_gesture_active());
}

#[test]
fn a_fling_carries_further_than_the_fingers_went() {
    let mut l = moving();
    for i in 1..=5 {
        l.open(t(i), Placement::Local);
    }
    l.set_clock(MS(1000));
    l.focus_column_first();
    l.set_clock(MS(3000));
    near(view_pos(&l), -8.0);
    // 100 pt in 50 ms: 2500 pt/s, about 830 pt more.
    swipe(&mut l, 3000, 100.0, 5, 10, 3050);
    assert!(l.frame().animating);
    l.set_clock(MS(6000));
    let pos = view_pos(&l);
    assert!(pos > 600.0, "{pos}");
    // Every rest is a snap: some column's padded edge on a view edge.
    let snaps: Vec<f32> = (0..5_u8)
        .flat_map(|i| {
            let x = col_x(i);
            [x - 8.0, x + HALF + 8.0 - 1280.0]
        })
        .collect();
    assert!(snaps.iter().any(|s| (s - pos).abs() < 0.01), "{pos}");
    // The focused column is on screen.
    let r = rect(&l, l.focused().unwrap());
    assert!(r.x >= 0.0 && r.right() <= 1280.0, "{r:?}");
}

#[test]
fn a_fling_past_either_end_stops_at_the_end_snap() {
    let mut l = columns(3);
    swipe(&mut l, 0, 3000.0, 5, 10, 50);
    near(view_pos(&l), 628.0);
    assert_eq!(active_col(&l), 2);
    swipe(&mut l, 1000, -5000.0, 5, 10, 1050);
    near(view_pos(&l), -8.0);
    assert_eq!(active_col(&l), 0);
    // A cancelled drag settles with the focus in view, moving the least: right-aligned.
    l.view_gesture_begin();
    l.view_gesture_update(-900.0, MS(2000));
    assert!(l.view_gesture_end(true));
    near(view_pos(&l), -644.0);
    assert_eq!(active_col(&l), 0);
    // No gesture: updates and ends are refused.
    assert!(!l.view_gesture_update(10.0, MS(3000)));
    assert!(!l.view_gesture_end(false));
}

#[test]
fn the_workspace_gesture_resists_past_the_ends_and_lands_on_the_nearest() {
    let mut l = columns(1);
    l.open(tile(2, 1), Placement::Remote);
    // Up from the first workspace: rubber band, never 0.05 past.
    l.ws_gesture_begin();
    assert!(l.ws_gesture_update(-200.0, MS(10)));
    l.ws_gesture_update(-5000.0, MS(20));
    let y = l.frame().workspaces[0].1.y;
    assert!(y > 0.0 && y < 0.05 * 880.0, "{y}");
    l.set_clock(MS(1000));
    assert!(l.ws_gesture_end(false));
    assert_eq!(l.active_workspace(), 0);
    // Most of a workspace down, slowly: the next one.
    l.ws_gesture_begin();
    for i in 1..=10_u16 {
        l.ws_gesture_update(0.07 * 880.0, MS(1000 + u64::from(i) * 100));
    }
    near(l.frame().workspaces[1].1.y, 880.0 * 0.3);
    l.set_clock(MS(2500));
    l.ws_gesture_end(false);
    assert_eq!(l.active_workspace(), 1);
    near(l.frame().workspaces[1].1.y, 0.0);
    // Never more than one away from where it began, however hard the fling.
    l.ws_gesture_begin();
    l.ws_gesture_update(-3000.0, MS(2510));
    l.ws_gesture_update(-3000.0, MS(2520));
    l.set_clock(MS(2520));
    l.ws_gesture_end(false);
    assert_eq!(l.active_workspace(), 0);
    // Cancelled: back to the start.
    l.ws_gesture_begin();
    l.ws_gesture_update(700.0, MS(3000));
    l.ws_gesture_end(true);
    assert_eq!(l.active_workspace(), 0);
    assert!(!l.ws_gesture_update(10.0, MS(4000)), "no gesture");
}

#[test]
fn the_wheel_steps_columns_and_workspaces_with_a_cooldown() {
    let mut l = columns(3);
    l.open(tile(2, 1), Placement::Remote);
    l.open(tile(3, 1), Placement::Remote);
    // Horizontal: a step per 50 pt, carried.
    assert!(!l.wheel(-30.0, 0.0, MS(0)));
    assert!(l.wheel(-30.0, 0.0, MS(10)));
    assert_eq!(active_col(&l), 1);
    assert!(l.wheel(-100.0, 0.0, MS(20)));
    assert_eq!(active_col(&l), 0);
    // Vertical: one workspace per 150 ms at most.
    assert!(l.wheel(0.0, 60.0, MS(1000)));
    assert_eq!(l.active_workspace(), 1);
    assert!(!l.wheel(0.0, 60.0, MS(1100)));
    assert_eq!(l.active_workspace(), 1);
    assert!(l.wheel(0.0, 60.0, MS(1150)));
    assert_eq!(l.active_workspace(), 2);
    assert!(l.wheel(0.0, -60.0, MS(1400)));
    assert_eq!(l.active_workspace(), 1);
}

// ----- animation ----------------------------------------------------------------------------

#[test]
fn a_retargeted_view_keeps_its_place_and_its_speed() {
    let mut l = moving();
    for i in 1..=3 {
        l.open(t(i), Placement::Local);
    }
    l.set_clock(MS(2000));
    near(view_pos(&l), 628.0);
    l.focus_column_first();
    l.set_clock(MS(2050));
    let before = view_pos(&l);
    assert!(before < 628.0 && before > -8.0, "{before}");
    l.focus_column_last();
    near(view_pos(&l), before);
    // Still heading left for a moment: the speed carried over.
    l.set_clock(MS(2055));
    assert!(view_pos(&l) < before, "{} !< {before}", view_pos(&l));
    l.set_clock(MS(5000));
    near(view_pos(&l), 628.0);
}

#[test]
fn view_springs_run_niris_critically_damped_curve() {
    let mut l = moving();
    for i in 1..=3 {
        l.open(t(i), Placement::Local);
    }
    l.set_clock(MS(2000));
    l.focus_column_first();
    let spring = Spring { from: 636.0, to: 0.0, initial_velocity: 0.0, params: view_spring() };
    for ms in [0_u64, 16, 50, 100, 200, 300] {
        l.set_clock(MS(2000 + ms));
        let expected = narrow(spring.value_at(MS(ms))) - 8.0;
        assert!((view_pos(&l) - expected).abs() < 0.01, "{ms} ms: {} vs {expected}", view_pos(&l));
    }
    l.set_clock(MS(2400));
    near(view_pos(&l), -8.0);
    assert!(!l.is_animating());
}

#[test]
fn neighbours_slide_from_where_they_were_drawn() {
    let mut l = moving();
    for i in 1..=2 {
        l.open(t(i), Placement::Local);
    }
    l.set_clock(MS(2000));
    let (a, b) = (rect(&l, t(1)), rect(&l, t(2)));
    l.move_column_left();
    // Drawn where they were at the instant of the change...
    near(rect(&l, t(2)).x, b.x);
    near(rect(&l, t(1)).x, a.x);
    // ...then on their way...
    l.set_clock(MS(2100));
    let mid = rect(&l, t(1)).x;
    assert!(mid > a.x && mid < b.x, "{mid}");
    // ...and home.
    l.set_clock(MS(3000));
    near(rect(&l, t(1)).x, b.x);
    near(rect(&l, t(2)).x, a.x);
    assert!(!l.frame().animating);
}

#[test]
fn opening_fades_in_and_closing_fades_out() {
    let mut l = moving();
    l.set_clock(MS(1000));
    l.open(t(1), Placement::Local);
    let opening = placed(&l, t(1));
    assert!(opening.alpha < 0.01 && (opening.scale - 0.5).abs() < 0.01, "{opening:?}");
    l.set_clock(MS(1075));
    let half = placed(&l, t(1));
    assert!(half.alpha > 0.9 && half.alpha < 1.0, "expo is most of the way at half time: {half:?}");
    l.set_clock(MS(1150));
    let open = placed(&l, t(1));
    assert!((open.alpha - 1.0).abs() < f32::EPSILON && (open.scale - 1.0).abs() < f32::EPSILON);
    l.remove(t(1));
    let frame = l.frame();
    assert!(frame.tiles.is_empty());
    assert_eq!(frame.closing.len(), 1);
    assert_eq!(frame.closing[0].rect, open.rect);
    near(frame.closing[0].alpha, 1.0);
    // Ease-out quad at half time is 3/4 of the way.
    l.set_clock(MS(1225));
    let fading = l.frame().closing[0];
    near(fading.alpha, 0.25);
    near(fading.scale, 0.85);
    l.set_clock(MS(1300));
    assert!(l.frame().closing.is_empty());
    assert!(!l.is_animating());
}

#[test]
fn without_animation_everything_lands_at_once() {
    let mut l = columns(3);
    l.focus_column_first();
    l.move_column_right();
    l.set_overview(true);
    near(l.frame().zoom, 0.5);
    l.set_overview(false);
    l.focus_workspace_down();
    l.focus_workspace_up();
    l.remove(t(1));
    let f = l.frame();
    assert!(!f.animating);
    assert!(f.closing.is_empty());
    assert!(f.tiles.iter().all(|p| (p.alpha - 1.0).abs() < f32::EPSILON && p.rect == p.target));
    // Turning it off mid-flight lands what is running.
    let mut l = moving();
    l.open(t(1), Placement::Local);
    l.open(t(2), Placement::Local);
    l.open(t(3), Placement::Local);
    l.remove(t(2));
    assert!(l.is_animating());
    l.set_animate(false);
    assert!(!l.is_animating());
    assert!(l.frame().closing.is_empty());
}

#[test]
fn the_target_rect_is_where_a_springing_tile_comes_to_rest() {
    let mut l = moving();
    l.open(t(1), Placement::Local);
    l.set_clock(MS(1000));
    l.switch_preset_width(true);
    l.set_clock(MS(1050));
    let p = placed(&l, t(1));
    near(p.target.w, 840.0);
    assert!(p.rect.w < 840.0, "{p:?}");
    l.set_clock(MS(2000));
    let p = placed(&l, t(1));
    assert_eq!(p.rect, p.target);
    // The overview scales the drawing but not the rest size.
    l.set_overview(true);
    l.set_clock(MS(3000));
    let p = placed(&l, t(1));
    near(p.rect.w, 420.0);
    near(p.target.w, 840.0);
}

// ----- phone --------------------------------------------------------------------------------

#[test]
fn on_a_phone_columns_take_the_width_and_neighbours_peek() {
    let mut l = still();
    l.set_viewport(390.0, 844.0);
    assert!(l.is_phone());
    l.open(t(1), Placement::Local);
    l.open(t(2), Placement::Local);
    // Working width 390 − 2 × 12; full width in it is 366 − 16 = 350.
    let (a, b) = (rect(&l, t(1)), rect(&l, t(2)));
    near(b.w, 350.0);
    near(b.x, 20.0);
    near(b.right(), 370.0);
    // The left neighbour shows its last 12 pt.
    near(a.right(), 12.0);
    l.focus_column_left();
    near(rect(&l, t(2)).x, 390.0 - 12.0);
    // Back to desktop: new columns are half again, and the view refits.
    l.set_viewport(1280.0, 800.0);
    assert!(!l.is_phone());
    l.open(t(3), Placement::Local);
    near(width_of(&l, t(3)), HALF);
    let r = rect(&l, t(3));
    assert!(r.x >= 0.0 && r.right() <= 1280.0, "{r:?}");
}

// ----- culling ------------------------------------------------------------------------------

#[test]
fn near_marks_what_is_on_screen_plus_one_either_side() {
    let mut l = columns(8);
    l.focus_column(3);
    // The view shows columns 2 and 3 (right-aligned on 3 after walking left from 7).
    let f = l.frame();
    let near_of = |n: u128| f.tiles.iter().find(|p| p.tile == t(n)).unwrap().near;
    let shown: Vec<bool> = (1..=8).map(near_of).collect();
    let view = f.strip.view.0;
    let on = |i: usize| {
        let x = f.strip.columns[i].0 - view;
        x + f.strip.columns[i].1 > 0.0 && x < 1280.0
    };
    for i in 0..8 {
        let expected = (0..8).filter(|&j| on(j)).any(|j| i + 1 >= j && i <= j + 1);
        assert_eq!(shown[i], expected, "column {i}: {shown:?}");
    }
    assert!(shown.iter().filter(|&&n| !n).count() >= 3, "{shown:?}");
    // Another workspace off screen is not near at all.
    l.open(tile(2, 1), Placement::Remote);
    assert!(!placed(&l, tile(2, 1)).near);
    // In the overview it is on screen.
    l.set_overview(true);
    assert!(placed(&l, tile(2, 1)).near);
}

#[test]
fn the_strip_reports_every_column_and_the_view() {
    let l = columns(3);
    let s = l.frame().strip;
    assert_eq!(s.columns.len(), 3);
    near(s.columns[2].0, col_x(2));
    near(s.columns[2].1, HALF);
    near(s.view.1, 1280.0);
    assert_eq!(s.active, Some(2));
    assert_eq!(still().frame().strip.active, None);
}

// ----- interactive resize -------------------------------------------------------------------

#[test]
fn an_interactive_resize_follows_the_pointer_and_clamps() {
    let mut l = columns(2);
    let b = rect(&l, t(2));
    // Column 0 is left of the focus: its left edge and the camera stay, column 1 is pushed.
    assert!(l.resize_begin(0));
    assert!(!l.resize_begin(1), "one at a time");
    l.resize_update(100.0);
    near(width_of(&l, t(1)), HALF + 100.0);
    near(rect(&l, t(1)).x, 8.0);
    near(rect(&l, t(2)).x, b.x + 100.0);
    l.resize_update(5000.0);
    near(width_of(&l, t(1)), 1264.0);
    l.resize_update(-5000.0);
    near(width_of(&l, t(1)), 128.0);
    l.resize_end();
    assert!(!l.resize_update(10.0), "ended");
    assert!(!l.resize_begin(7));
    // A gesture during a resize is refused.
    l.resize_begin(1);
    l.view_gesture_begin();
    assert!(!l.view_gesture_active());
    l.resize_end();
}

// ----- persistence --------------------------------------------------------------------------

#[test]
fn save_and_restore_round_trip_through_json() {
    let mut l = columns(3);
    l.consume_or_expel_window_left();
    l.toggle_tabbed();
    l.switch_preset_width(true);
    l.focus_column_first();
    l.toggle_full_width();
    l.open(tile(2, 1), Placement::Remote);
    l.set_workspace_name(1, Some("remote".to_owned()));
    l.focus_workspace_down();
    l.set_workspace_name(2, Some("spare".to_owned()));
    let saved = l.save();
    let json = serde_json::to_string(&saved).unwrap();
    let back: Saved = serde_json::from_str(&json).unwrap();
    assert_eq!(back, saved);
    let r = Layout::restore(back, LayoutConfig::default());
    assert_eq!(r.save(), saved);
    assert_eq!(shape(&r), shape(&l));
    assert_eq!(r.active_workspace(), 1);
    assert_eq!(r.focused(), l.focused());
    assert_eq!(
        r.frame().tiles.iter().map(|p| p.rect).collect::<Vec<_>>(),
        l.frame().tiles.iter().map(|p| p.rect).collect::<Vec<_>>()
    );
    // Fullscreen is a moment, not a layout.
    l.toggle_fullscreen();
    assert!(
        Layout::restore(l.save(), LayoutConfig::default())
            .frame()
            .tiles
            .iter()
            .all(|p| !p.fullscreen)
    );
}

#[test]
fn restore_cleans_up_whatever_it_is_given() {
    let col = |tiles: Vec<TileRef>| SavedColumn {
        tiles: tiles
            .into_iter()
            .map(|tile| SavedTile { tile, height: TileHeight::default() })
            .collect(),
        ..SavedColumn::default()
    };
    let saved = Saved {
        workspaces: vec![
            SavedWorkspace::default(),
            SavedWorkspace {
                name: None,
                columns: vec![
                    SavedColumn {
                        active_tile: 9,
                        width: ColumnWidth::Proportion(f32::NAN),
                        preset: Some(7),
                        ..col(vec![t(1), t(1), t(2)])
                    },
                    col(vec![]),
                    SavedColumn {
                        tiles: vec![SavedTile {
                            tile: t(3),
                            height: TileHeight::Auto { weight: -1.0 },
                        }],
                        width: ColumnWidth::Fixed(f32::INFINITY),
                        ..SavedColumn::default()
                    },
                ],
                active_column: 40,
                view_offset: f32::NAN,
            },
            SavedWorkspace { columns: vec![col(vec![t(2), t(4)])], ..SavedWorkspace::default() },
        ],
        active: 0,
    };
    let r = Layout::restore(saved, LayoutConfig::default());
    assert_eq!(shape(&r), vec![vec![vec![t(1), t(2)], vec![t(3)]], vec![vec![t(4)]], vec![]]);
    let ws = &r.workspaces()[0];
    assert_eq!(ws.active_column(), 1);
    let c = &ws.columns()[0];
    assert_eq!(c.active_tile(), 1);
    assert_eq!(c.width(), ColumnWidth::Proportion(0.5));
    assert_eq!(c.preset(), None);
    assert_eq!(ws.columns()[1].width(), ColumnWidth::Proportion(0.5));
    assert_eq!(ws.columns()[1].tiles()[0].height(), TileHeight::default());
    assert_eq!(r.active_workspace(), 0, "the dropped active workspace handed over to the next");
    assert!(r.frame().tiles.iter().all(|p| p.rect.x.is_finite()));
    // Nothing at all: one empty workspace.
    let e = Layout::restore(Saved::default(), LayoutConfig::default());
    assert_eq!(shape(&e), vec![Vec::<Vec<TileRef>>::new()]);
    assert_eq!(e.active_workspace(), 0);
    // Missing fields default.
    let partial: Saved =
        serde_json::from_str(r#"{"workspaces":[{"columns":[{"tiles":[]}]}]}"#).unwrap();
    assert_eq!(partial.workspaces[0].columns[0].width, ColumnWidth::Proportion(0.5));
}

// ----- nothing breaks at the edges ----------------------------------------------------------

#[test]
fn every_action_is_safe_on_an_empty_layout() {
    let mut l = moving();
    l.focus_column_left();
    l.focus_column_right();
    l.focus_column_first();
    l.focus_column_last();
    l.focus_window_up();
    l.focus_window_down();
    l.focus_window_or_workspace_up();
    l.focus_window_or_workspace_down();
    l.focus_workspace_up();
    l.focus_workspace_down();
    l.focus_workspace_previous();
    l.move_column_left();
    l.move_column_right();
    l.move_column_to_first();
    l.move_column_to_last();
    l.move_window_up();
    l.move_window_down();
    l.move_window_up_or_to_workspace_up();
    l.move_window_down_or_to_workspace_down();
    l.move_column_to_workspace_up();
    l.move_column_to_workspace_down();
    l.move_workspace_up();
    l.move_workspace_down();
    l.consume_or_expel_window_left();
    l.consume_or_expel_window_right();
    l.consume_into_column();
    l.expel_from_column();
    l.switch_preset_width(true);
    l.set_width_delta(10.0);
    l.toggle_full_width();
    l.toggle_fullscreen();
    l.expand_to_available_width();
    l.center_column();
    l.center_visible_columns();
    l.toggle_tabbed();
    l.view_gesture_begin();
    l.view_gesture_update(10.0, MS(1));
    l.view_gesture_end(false);
    l.ws_gesture_begin();
    l.ws_gesture_update(100.0, MS(2));
    l.ws_gesture_end(false);
    l.wheel(100.0, 100.0, MS(3));
    l.dnd_edge_scroll(0.0);
    l.dnd_scroll_end();
    l.resize_begin(0);
    l.resize_update(10.0);
    l.resize_end();
    l.remove(t(1));
    l.set_viewport(0.0, -5.0);
    assert!(l.drop_target(f32::NAN, f32::NAN).is_none() || l.overview_open());
    l.set_clock(MS(10_000));
    assert_eq!(l.workspaces().len(), 1);
    assert!(l.frame().tiles.is_empty());
}
