//! The tiling model: the tree's normal form and geometry, the keys' moves, where new work opens,
//! projects and their tabs, and what a relaunch keeps.

use std::str::FromStr as _;

use super::super::tree::{Laid, PANE_MIN_H, PANE_MIN_W, SplitAxis};
use super::*;

fn item(n: u128) -> ItemId {
    ItemId::from_str(&format!("00000000-0000-0000-0000-{n:012x}")).unwrap()
}

fn tile(worker: u128, n: u128) -> TileRef {
    TileRef { worker: WorkerKey::new(worker), item: item(n) }
}

fn t(n: u128) -> TileRef {
    tile(1, n)
}

fn home(name: &str) -> GroupKey {
    GroupKey::new("project", name)
}

/// A workspace laid out on a 1264 × 800 tab, a Mac window's.
fn mac() -> Tiling {
    let mut tiling = Tiling::new(TilingConfig::default());
    tiling.set_area(1264.0, 800.0);
    tiling
}

/// Each pane's rectangle in the tab on show, by its tiles.
fn rects(tiling: &Tiling) -> Vec<(Vec<TileRef>, Rect)> {
    let tab = tiling.shown_tab().unwrap();
    tiling
        .frame()
        .panes
        .iter()
        .map(|l| (tab.pane(l.pane).unwrap().tiles().to_vec(), l.rect))
        .collect()
}

fn rect_of(tiling: &Tiling, tile: TileRef) -> Rect {
    rects(tiling).into_iter().find(|(tiles, _)| tiles.contains(&tile)).unwrap().1
}

/// The rectangles tile `area` exactly: their areas add up to it, none overlaps another, and
/// each lies inside it.
#[track_caller]
fn tiles_exactly(laid: &[Laid], area: Rect) {
    let total: f32 = laid.iter().map(|l| l.rect.w * l.rect.h).sum();
    assert!(
        area.w.mul_add(-area.h, total).abs() < 0.5,
        "{total} of {} in {laid:?}",
        area.w * area.h
    );
    for (i, a) in laid.iter().enumerate() {
        assert!(a.rect.x >= area.x && a.rect.right() <= area.right() + 0.01, "{a:?} inside");
        assert!(a.rect.y >= area.y && a.rect.bottom() <= area.bottom() + 0.01, "{a:?} inside");
        for b in laid.iter().skip(i.saturating_add(1)) {
            assert!(!a.rect.intersects(&b.rect), "{a:?} overlaps {b:?}");
        }
    }
}

/// Three tiles in a row, the third split below: `[1 | 2 | 3/4]`, laid out on the Mac.
fn three_and_one() -> Tiling {
    let mut tiling = mac();
    tiling.config.room = Room { min_w: 100.0, min_h: 100.0 };
    tiling.new_tab(t(1), &home("atlas"));
    tiling.split_focused(t(2), Side::Right, &home("atlas"));
    tiling.split_focused(t(3), Side::Right, &home("atlas"));
    tiling.split_focused(t(4), Side::Bottom, &home("atlas"));
    tiling
}

// ----- the tree --------------------------------------------------------------------------------

/// The normal form: a split of one child is that child, a split's child of its own axis is
/// spliced in with its part of the share, an empty pane goes, shares that are not positive
/// numbers are made equal, and every set sums to 1. Bringing a normal tree to its normal form
/// changes nothing.
#[test]
fn normalisation_is_idempotent() {
    let pane = |id: u64, n: u128| Node::Pane(Pane::new(PaneId::of(id), t(n)));
    let inner =
        Node::Split(Split::of(SplitAxis::Row, vec![pane(2, 2), pane(3, 3)], vec![0.5, 0.5]));
    let lone = Node::Split(Split::of(SplitAxis::Column, vec![pane(4, 4)], vec![1.0]));
    let root = Node::Split(Split::of(
        SplitAxis::Row,
        vec![pane(1, 1), inner, lone],
        vec![0.5, 0.25, f32::NAN],
    ));
    let mut tab = Tab::restored(TabId(1), root, &[0], false, None).unwrap();
    let Node::Split(split) = tab.root() else { panic!("a split") };
    assert_eq!(split.axis(), SplitAxis::Row);
    assert_eq!(split.children().len(), 4, "the inner row spliced in, the lone column its pane");
    assert!(split.children().iter().all(|c| matches!(c, Node::Pane(_))));
    let sum: f32 = split.shares().iter().sum();
    assert!((sum - 1.0).abs() < 1e-5, "{:?}", split.shares());
    let shares = split.shares().to_vec();
    assert!((shares[1] - shares[2]).abs() < 1e-6, "the inner row's halves of its share");
    assert!(4.0_f32.mul_add(-shares[1], shares[0]).abs() < 1e-5, "{shares:?}");

    let before = tab.clone();
    assert!(tab.normalize());
    assert_eq!(tab, before, "a normal tree stays as it is");

    // A pane emptied goes, and a split left with one child is that child.
    assert_eq!(tab.take(t(2)), Taken::Gone);
    assert_eq!(tab.take(t(3)), Taken::Gone);
    assert_eq!(tab.take(t(4)), Taken::Gone);
    assert!(matches!(tab.root(), Node::Pane(p) if p.tiles() == [t(1)]), "{:?}", tab.root());
    assert_eq!(tab.take(t(1)), Taken::Emptied, "the last tile empties the tab");
}

/// A split and then the close of what it opened give back the tree it started from, its
/// shares and its focus too.
#[test]
fn a_split_then_its_close_gives_back_the_tree() {
    let mut tiling = three_and_one();
    let before = tiling.shown_tab().unwrap().clone();
    tiling.split_focused(t(5), Side::Right, &home("atlas"));
    assert_ne!(tiling.shown_tab().unwrap().root(), before.root());
    tiling.remove(t(5));
    let after = tiling.shown_tab().unwrap();
    assert_eq!(after.root(), before.root(), "the same tree");
    assert_eq!(after.focused(), Some(t(4)), "the focus back where the split came from");
}

/// The frame tiles the tab's area exactly, panes meeting edge to edge on whole points, and a
/// sash lies on each edge two neighbours share.
#[test]
fn the_frame_tiles_the_area_with_no_gap_and_no_overlap() {
    let tiling = three_and_one();
    let frame = tiling.frame();
    assert_eq!(frame.panes.len(), 4);
    tiles_exactly(&frame.panes, tiling.area());
    for l in &frame.panes {
        assert_eq!(l.rect.x.fract(), 0.0, "{l:?} on a whole point");
        assert_eq!(l.rect.w.fract(), 0.0, "{l:?} on a whole point");
    }
    assert_eq!(frame.sashes.len(), 3, "two upright in the row, one across the last column");
    let upright = frame.sashes.iter().filter(|s| s.axis == SplitAxis::Row).count();
    assert_eq!(upright, 2);
    let first = rect_of(&tiling, t(1));
    let row = frame.sashes.iter().find(|s| s.axis == SplitAxis::Row).unwrap();
    assert_eq!(row.line.x, first.right(), "on the edge the two share");
}

/// A sash drag keeps each side at its least room, and the shares go on summing to 1; the
/// moved distance is what was let.
#[test]
fn a_sash_drag_keeps_the_least_room_and_the_shares_whole() {
    let mut tiling = mac();
    tiling.new_tab(t(1), &home("atlas"));
    tiling.split_focused(t(2), Side::Right, &home("atlas"));
    let sash = tiling.frame().sashes[0].clone();
    let moved = tiling.drag_sash(&sash, -40.0);
    assert!((moved + 40.0).abs() < 0.01, "free room moves as asked: {moved}");
    assert!((rect_of(&tiling, t(1)).w - 592.0).abs() < 1.0);
    let moved = tiling.drag_sash(&tiling.frame().sashes[0].clone(), -1000.0);
    assert!(moved < 0.0);
    let left = rect_of(&tiling, t(1)).w;
    assert!((left - PANE_MIN_W).abs() < 1.0, "never below the least: {left}");
    let moved = tiling.drag_sash(&tiling.frame().sashes[0].clone(), -10.0);
    assert!(moved.abs() < 0.01, "at its least it holds: {moved}");
    let Node::Split(s) = tiling.shown_tab().unwrap().root() else { panic!() };
    let sum: f32 = s.shares().iter().sum();
    assert!((sum - 1.0).abs() < 1e-5);
    assert!(tiling.equalize(&[]));
    assert!((rect_of(&tiling, t(1)).w - 632.0).abs() < 1.0, "a double-click evens them");
}

/// Focus moves to the pane that shares the edge on that side, the one overlapping most;
/// with none there, it stays.
#[test]
fn spatial_focus_picks_the_pane_that_shares_the_edge() {
    let mut tiling = three_and_one();
    assert_eq!(tiling.focused(), Some(t(4)));
    assert!(tiling.focus_side(Side::Top));
    assert_eq!(tiling.focused(), Some(t(3)));
    assert!(tiling.focus_side(Side::Left));
    assert_eq!(tiling.focused(), Some(t(2)));
    assert!(tiling.focus_side(Side::Left));
    assert_eq!(tiling.focused(), Some(t(1)));
    assert!(!tiling.focus_side(Side::Left), "nothing further left");
    assert!(!tiling.focus_side(Side::Top));
    assert_eq!(tiling.focused(), Some(t(1)));
}

/// A tile moved toward a neighbour joins that pane's tabs; at the tab's edge it splits out
/// along the edge, taking an equal part of it; alone at that edge, it stays.
#[test]
fn a_move_to_the_border_splits_out() {
    let mut tiling = three_and_one();
    assert!(tiling.move_focused(Side::Top), "4 joins 3's pane");
    let tab = tiling.shown_tab().unwrap();
    assert_eq!(tab.panes().count(), 3);
    assert_eq!(tab.pane(tab.pane_of(t(3)).unwrap()).unwrap().tiles(), [t(3), t(4)]);
    assert_eq!(tiling.focused(), Some(t(4)));

    assert!(tiling.move_focused(Side::Bottom), "at the bottom edge: out along it");
    let r = rect_of(&tiling, t(4));
    assert_eq!((r.x, r.w), (0.0, 1264.0), "the whole width");
    assert!((r.h - 400.0).abs() < 1.0, "an equal part of the height: {r:?}");
    tiles_exactly(&tiling.frame().panes, tiling.area());
    assert!(!tiling.move_focused(Side::Bottom), "alone at that edge already");
}

/// A hidden pane keeps its place and takes no room, and coming back takes it again; the
/// tab's terminal is one, below the whole tab at a third of its height.
#[test]
fn a_hidden_pane_takes_no_room() {
    let mut tiling = mac();
    tiling.new_tab(t(1), &home("atlas"));
    tiling.split_focused(t(2), Side::Right, &home("atlas"));
    assert_eq!(tiling.toggle_terminal(), None, "no terminal yet");
    assert!(tiling.set_terminal(t(9)));
    let term = rect_of(&tiling, t(9));
    assert_eq!((term.w, term.y + term.h), (1264.0, 800.0));
    assert!((term.h - 267.0).abs() < 1.0, "a third: {term:?}");
    assert_eq!(tiling.focused(), Some(t(9)));

    assert_eq!(tiling.toggle_terminal(), Some(false), "put away");
    let frame = tiling.frame();
    assert_eq!(frame.panes.len(), 2);
    tiles_exactly(&frame.panes, tiling.area());
    assert_eq!(rect_of(&tiling, t(1)).h, 800.0, "the rest take its room");
    assert_ne!(tiling.focused(), Some(t(9)), "the focus leaves it");
    assert!(!tiling.on_show(t(9)));
    assert_eq!(tiling.toggle_terminal(), Some(true), "back, the same pane");
    assert!((rect_of(&tiling, t(9)).h - 267.0).abs() < 1.0);
    assert_eq!(tiling.focused(), Some(t(9)));
}

/// Zoom fills the tab with the focused pane and draws no sash; a split lets it go.
#[test]
fn zoom_fills_the_tab_and_a_split_undoes_it() {
    let mut tiling = three_and_one();
    assert!(tiling.toggle_zoom());
    let frame = tiling.frame();
    assert_eq!(frame.panes.len(), 1);
    assert_eq!(frame.panes[0].rect, tiling.area());
    assert_eq!(frame.sashes, []);
    assert!(!tiling.on_show(t(1)));
    tiling.split_focused(t(5), Side::Right, &home("atlas"));
    assert_eq!(tiling.shown_tab().unwrap().zoomed(), None, "any change to the tree");
    assert_eq!(tiling.frame().panes.len(), 5);
    assert!(tiling.toggle_zoom());
    assert!(!tiling.toggle_zoom(), "the second press lets it go");
    // The focus going to another pane lets it go too, so the tile focused shows.
    assert!(tiling.toggle_zoom());
    tiling.focus(t(1));
    assert_eq!(tiling.shown_tab().unwrap().zoomed(), None, "the focus went elsewhere");
    assert!(tiling.on_show(t(1)));
}

/// What a relaunch begins from comes back the same: the projects, their tabs, each tree with
/// its shares, the focus, the zoom, the terminal put away, and the tabs visited.
#[test]
fn saving_and_restoring_round_trips() {
    let mut tiling = three_and_one();
    let sash = tiling.frame().sashes[0].clone();
    tiling.drag_sash(&sash, 30.0);
    tiling.set_terminal(t(8));
    tiling.toggle_terminal();
    tiling.new_tab(t(5), &home("atlas"));
    tiling.new_tab(t(6), &home("web"));
    tiling.select_tab(0);
    tiling.show_project(&home("atlas"));
    tiling.select_tab(0);
    tiling.toggle_zoom();

    let saved = tiling.save();
    let json = serde_json::to_string(&saved).unwrap();
    let back: SavedTiling = serde_json::from_str(&json).unwrap();
    let restored = Tiling::restore(back, TilingConfig::default());
    assert_eq!(restored.save(), saved, "the same again");
    let tab = restored.shown_tab().unwrap();
    assert_eq!(restored.focused(), tiling.focused());
    assert!(tab.zoomed().is_some());
    let term = tab.terminal().and_then(|p| tab.pane(p)).unwrap();
    assert!(term.hidden() && term.tiles() == [t(8)]);
    assert_eq!(restored.projects().len(), 2);
}

/// A kept file another build wrote badly comes back cleaned: a tile twice is kept once, a
/// pane or a tab left empty goes, a project with nothing and no name goes, indices clamp.
#[test]
fn a_restore_cleans_what_it_reads() {
    let pane = |tiles: Vec<TileRef>| SavedNode::Pane { tiles, shown: 7, hidden: false };
    let saved = SavedTiling {
        projects: vec![
            SavedProject { home: home("gone"), name: None, tabs: Vec::new(), shown: 0 },
            SavedProject {
                home: home("atlas"),
                name: None,
                tabs: vec![
                    SavedTab {
                        root: SavedNode::Split {
                            axis: SplitAxis::Row,
                            children: vec![pane(vec![t(1), t(1)]), pane(vec![]), pane(vec![t(1)])],
                            shares: vec![-1.0],
                        },
                        focus: vec![9],
                        zoomed: false,
                        terminal: Some(vec![4]),
                    },
                    SavedTab { root: pane(vec![]), focus: vec![], zoomed: true, terminal: None },
                ],
                shown: 5,
            },
        ],
        shown: Some(1),
        visits: vec![(1, 0), (1, 1), (7, 7)],
    };
    let mut named = saved.clone();
    named.projects.insert(
        0,
        SavedProject {
            home: home("named"),
            name: Some("Named".to_owned()),
            tabs: Vec::new(),
            shown: 3,
        },
    );
    named.shown = Some(2);
    let both = Tiling::restore(named, TilingConfig::default());
    assert_eq!(both.projects().len(), 2, "a named project stays with no tab");
    assert_eq!(both.shown_project().map(Project::home), Some(&home("atlas")), "still on show");
    let tiling = Tiling::restore(saved, TilingConfig::default());
    assert_eq!(tiling.projects().len(), 1);
    let project = &tiling.projects()[0];
    assert_eq!(project.tabs().len(), 1);
    let tab = &project.tabs()[0];
    assert!(matches!(tab.root(), Node::Pane(p) if p.tiles() == [t(1)] && p.shown_index() == 0));
    assert_eq!(tab.terminal(), None);
    assert_eq!(tiling.focused(), Some(t(1)));
}

// ----- where new work opens --------------------------------------------------------------------

/// ⌘T and ⌘⇧T make a tab after the one on show, shown and focused, in its project.
#[test]
fn a_new_tab_comes_after_the_one_on_show() {
    let mut tiling = mac();
    let first = tiling.new_tab(t(1), &home("atlas"));
    let second = tiling.new_tab(t(2), &home("atlas"));
    tiling.select_tab(0);
    let third = tiling.new_tab(t(3), &home("atlas"));
    let ids: Vec<TabId> = tiling.shown_project().unwrap().tabs().iter().map(Tab::id).collect();
    assert_eq!(ids, [first, third, second]);
    assert_eq!(tiling.focused(), Some(t(3)));
    assert_eq!(tiling.shown_tab().map(Tab::id), Some(third));
}

/// The room rule on a 1264 × 800 tab: a second tile goes beside the first, a third splits
/// below (three in a row would go under 520 pt), and a fourth is a tab of its source's pane.
/// On a 2300 pt screen, four go side by side.
#[test]
fn beside_gives_two_panes_then_a_split_below_then_tabs() {
    let mut tiling = mac();
    tiling.new_tab(t(1), &home("atlas"));
    tiling.open_beside(t(2), &home("atlas"));
    assert_eq!(rect_of(&tiling, t(2)), Rect { x: 632.0, y: 0.0, w: 632.0, h: 800.0 });
    tiling.open_beside(t(3), &home("atlas"));
    assert_eq!(rect_of(&tiling, t(3)), Rect { x: 632.0, y: 400.0, w: 632.0, h: 400.0 });
    tiling.open_beside(t(4), &home("atlas"));
    let tab = tiling.shown_tab().unwrap();
    assert_eq!(tab.panes().count(), 3, "no fourth pane");
    assert_eq!(tab.pane(tab.pane_of(t(4)).unwrap()).unwrap().tiles(), [t(3), t(4)]);
    assert_eq!(tiling.focused(), Some(t(4)));
    assert!(rect_of(&tiling, t(1)).w >= PANE_MIN_W && rect_of(&tiling, t(3)).h >= PANE_MIN_H);

    // With a pane to its right, a tile opened from the left one goes there as a tab.
    tiling.focus(t(1));
    tiling.open_beside(t(5), &home("atlas"));
    let tab = tiling.shown_tab().unwrap();
    assert_eq!(tab.panes().count(), 3);
    assert!(tab.pane(tab.pane_of(t(5)).unwrap()).unwrap().tiles().contains(&t(2)));

    let mut wide = Tiling::new(TilingConfig::default());
    wide.set_area(2300.0, 1200.0);
    wide.new_tab(t(1), &home("atlas"));
    for n in 2..=4 {
        wide.open_beside(t(n), &home("atlas"));
    }
    let widths: Vec<f32> = rects(&wide).iter().map(|(_, r)| r.w).collect();
    assert_eq!(widths.len(), 4, "four columns: {widths:?}");
    assert!(widths.iter().all(|w| (*w - 575.0).abs() < 1.0), "{widths:?}");
}

/// A phone shows the focused pane alone over the whole area, and nothing splits there: what
/// opens is a tab.
#[test]
fn a_phone_shows_one_pane_and_never_splits() {
    let mut tiling = Tiling::new(TilingConfig::default());
    tiling.set_area(390.0, 760.0);
    assert!(tiling.is_phone());
    tiling.new_tab(t(1), &home("atlas"));
    tiling.open_beside(t(2), &home("atlas"));
    tiling.split_focused(t(3), Side::Right, &home("atlas"));
    let tab = tiling.shown_tab().unwrap();
    assert_eq!(tab.panes().count(), 1);
    let frame = tiling.frame();
    assert_eq!(frame.panes.len(), 1);
    assert_eq!(frame.panes[0].rect, tiling.area());
    assert_eq!(tiling.focused(), Some(t(3)));
}

/// A tile from elsewhere is a background tab at the end of its project: the focus does not
/// move, and nothing on show changes. Into an empty workspace, its project shows.
#[test]
fn an_arrival_is_a_background_tab_and_the_focus_stays() {
    let mut tiling = mac();
    tiling.arrive(t(1), &home("web"));
    assert_eq!(
        tiling.shown_project().map(Project::home),
        Some(&home("web")),
        "nothing was on show"
    );
    tiling.new_tab(t(2), &home("atlas"));
    let before = tiling.frame();
    tiling.arrive(t(3), &home("atlas"));
    tiling.arrive(t(4), &home("web"));
    tiling.arrive(t(5), &home("docs"));
    assert_eq!(tiling.focused(), Some(t(2)), "the focus stays");
    assert_eq!(tiling.frame(), before, "the tab on show is as it was");
    let atlas = &tiling.projects()[tiling.project_of(&home("atlas")).unwrap()];
    assert_eq!(atlas.tabs().len(), 2);
    assert_eq!(atlas.tabs()[1].focused(), Some(t(3)), "at the end");
    assert!(tiling.project_of(&home("docs")).is_some(), "a new project for a new home");
    assert!(!tiling.on_show(t(3)));
}

/// A lead's helper from elsewhere lands right of the lead in its tab, and the next one joins
/// the first's pane as a tab, so many keep one column; the focus stays where it was, in the
/// lead's tab and in the workspace. A lead not placed, a tile placed already, or a phone leave
/// it to arrive as any tile does.
#[test]
fn a_leads_helpers_arrive_beside_it_in_one_column() {
    let mut tiling = mac();
    tiling.new_tab(t(1), &home("web"));
    tiling.new_tab(t(9), &home("web"));
    let helps = |tile: TileRef| [t(2), t(3)].contains(&tile);
    assert!(tiling.arrive_beside(t(2), t(1), helps));
    assert!(tiling.arrive_beside(t(3), t(1), helps));
    assert_eq!(tiling.focused(), Some(t(9)), "the focus stays");
    assert!(!tiling.on_show(t(2)), "in the lead's tab, not the one on show");
    let lead = tiling.position(t(1)).unwrap();
    let tab = tiling.tab_of(lead.tab).unwrap();
    assert_eq!(tab.panes().count(), 2, "the lead and one column of helpers");
    assert_eq!(tab.pane_of(t(2)), tab.pane_of(t(3)), "the second joins the first");
    assert_eq!(tab.focus(), lead.pane, "the lead keeps its tab's focus");
    tiling.focus(t(1));
    assert!(rect_of(&tiling, t(2)).x > rect_of(&tiling, t(1)).x, "right of the lead");

    assert!(!tiling.arrive_beside(t(4), t(7), helps), "a lead not placed");
    assert!(!tiling.arrive_beside(t(2), t(1), helps), "placed already");
    let mut phone = Tiling::new(TilingConfig::default());
    phone.set_area(390.0, 800.0);
    phone.new_tab(t(1), &home("web"));
    assert!(!phone.arrive_beside(t(2), t(1), helps), "a phone's one pane");
}

/// A project comes back on the tab it was left on, whichever way it is gone to; a project
/// left with no tab and no name goes.
#[test]
fn a_project_comes_back_on_the_tab_it_was_left_on() {
    let mut tiling = mac();
    tiling.new_tab(t(1), &home("atlas"));
    tiling.new_tab(t(2), &home("atlas"));
    tiling.new_tab(t(3), &home("atlas"));
    tiling.select_tab(1);
    tiling.new_tab(t(9), &home("web"));
    assert_eq!(tiling.focused(), Some(t(9)));
    tiling.show_project(&home("atlas"));
    assert_eq!(tiling.focused(), Some(t(2)), "where it was left");
    tiling.step_project(true);
    assert_eq!(tiling.focused(), Some(t(9)));
    tiling.step_project(true);
    assert_eq!(tiling.focused(), Some(t(2)), "round the ends");

    tiling.show_project(&home("empty"));
    assert_eq!(tiling.shown_tab(), None, "an empty project, for its first tab");
    tiling.show_project(&home("web"));
    assert_eq!(tiling.project_of(&home("empty")), None, "left with nothing, it goes");
    tiling.set_name(&home("named"), Some("Named".to_owned()));
    tiling.show_project(&home("named"));
    tiling.show_project(&home("web"));
    assert!(tiling.project_of(&home("named")).is_some(), "a named one stays");
}

/// ⌘1…⌘9 and the steps pick title tabs; back and forward retrace the tabs visited across
/// projects, past one since closed, without adding to the trail.
#[test]
fn back_and_forward_retrace_the_tabs_visited() {
    let mut tiling = mac();
    tiling.new_tab(t(1), &home("atlas"));
    tiling.new_tab(t(2), &home("atlas"));
    tiling.new_tab(t(3), &home("atlas"));
    tiling.select_tab(0);
    assert_eq!(tiling.focused(), Some(t(1)));
    tiling.last_tab();
    assert_eq!(tiling.focused(), Some(t(3)));
    tiling.step_tab(true);
    assert_eq!(tiling.focused(), Some(t(1)), "round the end");
    tiling.new_tab(t(4), &home("web"));
    // Visited: 1, 2, 3, 1(sel 0), 3(last), 1(step), 4.
    tiling.remove(t(3));
    assert!(tiling.go_back(false));
    assert_eq!(tiling.focused(), Some(t(1)));
    assert!(tiling.go_back(false));
    assert_eq!(tiling.focused(), Some(t(2)), "past the closed tab");
    assert!(tiling.go_back(true));
    assert!(tiling.go_back(true));
    assert_eq!(tiling.focused(), Some(t(4)));
    assert!(!tiling.can_go_back(true), "the forward arrow says nothing further");
    assert!(!tiling.go_back(true), "nothing further");
    assert!(tiling.can_go_back(false), "the back arrow says there is");
    assert_eq!(tiling.focused(), Some(t(4)), "asking moves nothing");
}

/// A worker's snapshot that no longer has some of its tiles takes them out: a pane emptied
/// goes, then a tab, and the tree closes up; another worker's tiles stay.
#[test]
fn retain_worker_empties_a_pane_a_tab_and_collapses_the_tree() {
    let mut tiling = mac();
    tiling.config.room = Room { min_w: 100.0, min_h: 100.0 };
    tiling.new_tab(tile(1, 1), &home("atlas"));
    tiling.split_focused(tile(2, 2), Side::Right, &home("atlas"));
    tiling.split_focused(tile(1, 3), Side::Bottom, &home("atlas"));
    tiling.new_tab(tile(1, 4), &home("atlas"));
    tiling.select_tab(0);
    tiling.retain_worker(WorkerKey::new(1), |i| i == item(1));
    let project = tiling.shown_project().unwrap();
    assert_eq!(project.tabs().len(), 1, "the emptied tab went");
    let tab = &project.tabs()[0];
    let Node::Split(s) = tab.root() else { panic!("{:?}", tab.root()) };
    assert_eq!((s.axis(), s.children().len()), (SplitAxis::Row, 2), "the column closed up");
    assert!(tiling.contains(tile(2, 2)), "the other worker's stays");
    tiles_exactly(&tiling.frame().panes, tiling.area());
}

/// A drop near a pane's edge, within a fifth of its shorter side, splits there; in its
/// middle it joins the pane's tabs; a tile dropped on its own lone pane stays.
#[test]
fn a_drop_splits_at_an_edge_and_joins_in_the_middle() {
    let mut tiling = mac();
    tiling.new_tab(t(1), &home("atlas"));
    tiling.split_focused(t(2), Side::Right, &home("atlas"));
    let left = tiling.shown_tab().unwrap().pane_of(t(1)).unwrap();
    // The left pane is 632 × 800: the band is 126 pt.
    assert_eq!(tiling.drop_target(20.0, 400.0), Some(Drop { pane: left, edge: Some(Side::Left) }));
    assert_eq!(tiling.drop_target(316.0, 790.0).unwrap().edge, Some(Side::Bottom));
    assert_eq!(tiling.drop_target(316.0, 400.0), Some(Drop { pane: left, edge: None }));

    assert!(!tiling.place(t(1), Drop { pane: left, edge: Some(Side::Top) }), "alone, it stays");
    let right = tiling.shown_tab().unwrap().pane_of(t(2)).unwrap();
    assert!(tiling.place(t(1), Drop { pane: right, edge: Some(Side::Bottom) }));
    let r = rect_of(&tiling, t(1));
    assert_eq!((r.w, r.h), (1264.0, 400.0), "{r:?}: its old pane closed, the new one below");
    tiles_exactly(&tiling.frame().panes, tiling.area());
    let right = tiling.shown_tab().unwrap().pane_of(t(2)).unwrap();
    assert!(tiling.place(t(1), Drop { pane: right, edge: None }));
    let tab = tiling.shown_tab().unwrap();
    assert_eq!(tab.panes().count(), 1);
    assert_eq!(tiling.focused(), Some(t(1)));
}

/// A tile moved to another project on purpose goes as a new tab there, and goes with the
/// focus; the tab it left closes up.
#[test]
fn a_tile_moves_to_another_project_on_purpose() {
    let mut tiling = mac();
    tiling.new_tab(t(1), &home("atlas"));
    tiling.split_focused(t(2), Side::Right, &home("atlas"));
    tiling.move_to_project(t(2), &home("web"));
    assert_eq!(tiling.shown_project().map(Project::home), Some(&home("web")));
    assert_eq!(tiling.focused(), Some(t(2)));
    let atlas = &tiling.projects()[tiling.project_of(&home("atlas")).unwrap()];
    assert_eq!(atlas.tabs()[0].panes().count(), 1);
}

/// The tabs of the project on show, each by its first tile.
fn strip(tiling: &Tiling) -> Vec<TileRef> {
    tiling.shown_project().unwrap().tabs().iter().map(|t| t.tiles().next().unwrap()).collect()
}

/// A tile dropped on the title strip becomes a tab of its own between the two it fell
/// between, shown and focused, and the pane it left closes up. One alone in its tab dropped on
/// the strip only moves its tab; past the end it goes last.
#[test]
fn a_tile_dropped_on_the_strip_is_a_tab_where_it_fell() {
    let mut tiling = mac();
    tiling.new_tab(t(1), &home("atlas"));
    tiling.new_tab(t(2), &home("atlas"));
    tiling.split_focused(t(3), Side::Right, &home("atlas"));
    assert_eq!(strip(&tiling), [t(1), t(2)]);

    let id = tiling.new_tab_at(t(3), &home("atlas"), 1);
    assert_eq!(strip(&tiling), [t(1), t(3), t(2)], "between the two");
    assert_eq!(tiling.shown_tab().map(Tab::id), Some(id));
    assert_eq!(tiling.focused(), Some(t(3)));
    let left = tiling.position(t(2)).unwrap();
    let tab = tiling.tab_place(left.tab).unwrap();
    assert_eq!(tiling.projects()[tab.0].tabs()[tab.1].panes().count(), 1, "it closed up");

    let alone = tiling.position(t(1)).unwrap().tab;
    assert_eq!(tiling.new_tab_at(t(1), &home("atlas"), 9), alone, "its own tab, moved");
    assert_eq!(strip(&tiling), [t(3), t(2), t(1)], "past the end: last");
    assert_eq!(tiling.focused(), Some(t(1)));

    // From another project: a new tab here, and its old project closes up behind it.
    tiling.new_tab(t(4), &home("web"));
    tiling.new_tab(t(5), &home("web"));
    tiling.new_tab_at(t(4), &home("atlas"), 0);
    assert_eq!(tiling.shown_project().map(Project::home), Some(&home("atlas")));
    assert_eq!(strip(&tiling), [t(4), t(3), t(2), t(1)]);
    let web = &tiling.projects()[tiling.project_of(&home("web")).unwrap()];
    assert_eq!(web.tabs().len(), 1);
}

/// A title tab dragged along the strip goes before the tab it is dropped on, else last; the
/// tab on show stays the one on show, and a drop on its own place moves nothing.
#[test]
fn a_title_tab_moves_along_the_strip() {
    let mut tiling = mac();
    for n in 1..=3 {
        tiling.new_tab(t(n), &home("atlas"));
    }
    tiling.select_tab(1);
    let first = tiling.position(t(1)).unwrap().tab;
    assert!(!tiling.move_tab(first, 0), "its own place");
    assert!(tiling.move_tab(first, 2), "before the third");
    assert_eq!(strip(&tiling), [t(2), t(1), t(3)]);
    assert_eq!(tiling.focused(), Some(t(2)), "the shown tab kept");
    assert!(tiling.move_tab(first, 5));
    assert_eq!(strip(&tiling), [t(2), t(3), t(1)], "past the end: last");
    assert_eq!(tiling.focused(), Some(t(2)));
}

/// A title tab dropped on another project's row goes there whole, its layout kept, and shows;
/// the project it left closes up, and goes when it was left with nothing and no name.
#[test]
fn a_title_tab_moves_whole_to_another_project() {
    let mut tiling = mac();
    tiling.new_tab(t(1), &home("atlas"));
    tiling.split_focused(t(2), Side::Right, &home("atlas"));
    tiling.new_tab(t(3), &home("web"));
    let tab = tiling.position(t(1)).unwrap().tab;
    assert!(!tiling.move_tab_to_project(tab, &home("atlas")), "already there");
    tiling.show_tab(tab);
    assert!(tiling.move_tab_to_project(tab, &home("web")));
    assert_eq!(tiling.shown_project().map(Project::home), Some(&home("web")));
    assert_eq!(tiling.shown_tab().map(Tab::id), Some(tab), "shown there");
    assert_eq!(tiling.shown_tab().unwrap().panes().count(), 2, "its layout kept");
    assert_eq!(strip(&tiling), [t(3), t(1)], "last");
    assert_eq!(tiling.project_of(&home("atlas")), None, "emptied and unnamed: gone");
    tiles_exactly(&tiling.frame().panes, tiling.area());
}

/// A close taken back puts the tile where it stood: a tab of its pane while that pane holds
/// others, else a pane of its own in its tab; a tab since gone takes nothing.
#[test]
fn a_close_taken_back_goes_where_it_stood() {
    let mut tiling = mac();
    tiling.new_tab(t(1), &home("atlas"));
    tiling.split_focused(t(2), Side::Right, &home("atlas"));
    tiling.focus(t(1));
    tiling.open_beside(t(3), &home("atlas"));
    let at = tiling.position(t(3)).unwrap();
    assert_eq!(at.pane, tiling.position(t(2)).unwrap().pane, "a tab of the pane beside");
    // Its pane holds another: it comes back a tab there, from a tab of its own.
    tiling.remove(t(3));
    tiling.new_tab(t(3), &home("atlas"));
    assert!(tiling.put_back(t(3), at));
    assert_eq!(tiling.position(t(3)), Some(at));
    assert_eq!(tiling.focused(), Some(t(3)));
    assert_eq!(tiling.shown_project().unwrap().tabs().len(), 1, "the tab it came in went");
    // Its pane went with it: a pane of its own, right of the focused one.
    let lone = tiling.position(t(1)).unwrap();
    tiling.remove(t(1));
    tiling.new_tab(t(1), &home("atlas"));
    assert!(tiling.put_back(t(1), lone));
    let back = tiling.position(t(1)).unwrap();
    assert_eq!(back.tab, lone.tab);
    assert_eq!(tiling.shown_tab().unwrap().panes().count(), 2);
    // Its tab went: nothing moves.
    let gone = tiling.position(t(1)).unwrap();
    let mut other = mac();
    other.new_tab(t(1), &home("web"));
    assert!(!other.put_back(t(1), gone), "no such tab here");
}

/// A project re-homed keeps its tabs, its name and its place in the order, and its arrivals
/// follow the new home; a home another project holds is not taken.
#[test]
fn a_project_rehomed_keeps_its_tabs_and_takes_no_others_home() {
    let mut tiling = mac();
    let machine = GroupKey::machine(WorkerKey::new(1));
    tiling.new_tab(t(1), &machine);
    tiling.set_name(&machine, Some("Mine".to_owned()));
    tiling.new_tab(t(2), &home("web"));
    assert!(tiling.rehome(&machine, home("atlas")));
    let first = &tiling.projects()[0];
    assert_eq!(first.home(), &home("atlas"));
    assert_eq!((first.name(), first.tabs().len()), (Some("Mine"), 1), "its name and its tab");
    tiling.arrive(t(3), &home("atlas"));
    assert_eq!(tiling.position(t(3)).unwrap().project, 0, "its arrivals follow");
    assert!(!tiling.rehome(&home("atlas"), home("web")), "web is another's");
    assert!(!tiling.rehome(&machine, home("elsewhere")), "no project at the old home");
}

/// The last tab of the project on show closed hands the window to the tab visited before it,
/// in its own project, and the emptied project goes; a project not on show emptied goes too,
/// unless the person named it.
#[test]
fn a_project_emptied_hands_the_window_back_and_goes() {
    let mut tiling = mac();
    tiling.new_tab(t(1), &home("atlas"));
    tiling.new_tab(t(2), &home("web"));
    assert_eq!(tiling.shown_project().unwrap().home(), &home("web"));
    tiling.remove(t(2));
    assert_eq!(tiling.shown_project().unwrap().home(), &home("atlas"), "back where it was");
    assert_eq!(tiling.focused(), Some(t(1)));
    assert_eq!(tiling.projects().len(), 1, "the emptied project went");
    tiling.arrive(t(3), &home("docs"));
    tiling.set_name(&home("docs"), Some("Docs".to_owned()));
    tiling.arrive(t(4), &home("notes"));
    tiling.remove(t(4));
    tiling.remove(t(3));
    let homes: Vec<&GroupKey> = tiling.projects().iter().map(Project::home).collect();
    assert_eq!(homes, [&home("atlas"), &home("docs")], "the named one stays");
    assert_eq!(tiling.shown_project().unwrap().home(), &home("atlas"), "still on show");
    let _gone = tiling.drop_tab(tiling.shown_tab().unwrap().id());
    assert!(tiling.shown_tab().is_none(), "nothing left anywhere: empty");
}
