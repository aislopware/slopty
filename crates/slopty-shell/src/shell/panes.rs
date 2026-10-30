//! A session's view as panes (`docs/decisions/layout.md`, "One session per view, splits on
//! demand"): the session's own terminal alone until the person splits it or summons a view
//! beside it, then a tree of splits, Ghostty's.
//!
//! A split is binary: two children side by side ([`Axis::Row`]) or one over the other
//! ([`Axis::Column`]), the first's share of the room a ratio. The tree lays itself out in
//! points ([`Panes::layout`]): a 1 pt divider between the two children, a terminal's share
//! snapped to whole cells, no pane under [`MIN_PANE`]. Focus walks the laid-out rectangles by
//! direction (⌥⌘ arrows) and the panes in reading order (⌘] ⌘[); a pane can fill the view
//! (zoom) while the tree keeps its shape. The session's pane never closes: closing every other
//! pane brings the view back to it.

use gpui::{Bounds, Pixels, Size, point, px, size};

use super::RightSurface;

/// What a pane shows.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Pane {
    /// The session's own terminal: the agent's TUI, or the session's shell.
    Session,
    /// A shell opened beside the session (⌘D), by its number in the view.
    Shell(u64),
    /// A view summoned beside the session: the inspector, Files, Changes, a Browser, a file,
    /// a remote window or an agent's page.
    View(RightSurface),
}

impl Pane {
    /// Whether it shows a terminal, whose share snaps to whole cells.
    pub(crate) const fn is_terminal(self) -> bool {
        matches!(self, Self::Session | Self::Shell(_))
    }
}

/// How a split lays out its two children.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Axis {
    /// Side by side: the first on the left.
    Row,
    /// One over the other: the first on top.
    Column,
}

/// A direction focus moves in, or a split opens toward.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    /// The axis a split toward it divides, and whether the new pane comes second.
    const fn split(self) -> (Axis, bool) {
        match self {
            Self::Left => (Axis::Row, false),
            Self::Right => (Axis::Row, true),
            Self::Up => (Axis::Column, false),
            Self::Down => (Axis::Column, true),
        }
    }
}

/// The visible divider between two panes, in points (Ghostty's).
pub(crate) const DIVIDER: f32 = 1.0;
/// The divider's grab target, in points: the hairline and 3 pt either side (Ghostty's 7).
pub(crate) const DIVIDER_GRAB: f32 = 7.0;
/// The smallest a pane is laid out, in points (Ghostty's 10, raised to keep a terminal's grid
/// a few cells wide and a view's header whole).
pub(crate) const MIN_PANE: f32 = 80.0;
/// A ratio's bounds, so neither child of a split can be dragged away (Ghostty's 10-90 %).
const RATIO: (f32, f32) = (0.1, 0.9);

/// The grid a terminal pane draws: its cell, and what its pane takes besides whole cells along
/// each axis (its padding, and a pane header's height while the view holds more than one).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Grid {
    pub(crate) cell: Size<Pixels>,
    pub(crate) frame: Size<Pixels>,
}

impl Grid {
    /// The cell's extent and the frame's along `axis`; none for an empty cell.
    fn along(self, axis: Axis) -> Option<(f32, f32)> {
        let (cell, frame) = match axis {
            Axis::Row => (self.cell.width, self.frame.width),
            Axis::Column => (self.cell.height, self.frame.height),
        };
        (cell > Pixels::ZERO).then(|| (f32::from(cell), f32::from(frame)))
    }
}

/// A node of the tree.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Node {
    Leaf(Pane),
    Split {
        axis: Axis,
        /// The first child's share of the room, between [`RATIO`]'s bounds.
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

impl Node {
    fn contains(&self, pane: Pane) -> bool {
        match self {
            Self::Leaf(leaf) => *leaf == pane,
            Self::Split { first, second, .. } => first.contains(pane) || second.contains(pane),
        }
    }

    fn panes(&self, into: &mut Vec<Pane>) {
        match self {
            Self::Leaf(pane) => into.push(*pane),
            Self::Split { first, second, .. } => {
                first.panes(into);
                second.panes(into);
            }
        }
    }

    /// Replace leaf `at` with a split of it and `new`, `new` second when `after`.
    fn split(&mut self, at: Pane, axis: Axis, new: Pane, after: bool) -> bool {
        match self {
            Self::Leaf(leaf) if *leaf == at => {
                let (first, second) = if after { (at, new) } else { (new, at) };
                *self = Self::Split {
                    axis,
                    ratio: 0.5,
                    first: Box::new(Self::Leaf(first)),
                    second: Box::new(Self::Leaf(second)),
                };
                true
            }
            Self::Leaf(_) => false,
            Self::Split { first, second, .. } => {
                first.split(at, axis, new, after) || second.split(at, axis, new, after)
            }
        }
    }

    /// Take leaf `pane` out; its sibling takes the split's place. The sibling's pane that stood
    /// nearest it, or `None` when it is not here or it is the whole tree.
    fn remove(&mut self, pane: Pane) -> Option<Pane> {
        let Self::Split { first, second, .. } = self else { return None };
        let keep = if **first == Self::Leaf(pane) {
            Some((std::mem::replace(second.as_mut(), Self::Leaf(pane)), false))
        } else if **second == Self::Leaf(pane) {
            Some((std::mem::replace(first.as_mut(), Self::Leaf(pane)), true))
        } else {
            None
        };
        match keep {
            Some((sibling, last)) => {
                let nearest = sibling.edge_leaf(last);
                *self = sibling;
                Some(nearest)
            }
            None => first.remove(pane).or_else(|| second.remove(pane)),
        }
    }

    fn equalize(&mut self) {
        if let Self::Split { ratio, first, second, .. } = self {
            *ratio = 0.5;
            first.equalize();
            second.equalize();
        }
    }

    fn layout(&self, at: Bounds<Pixels>, grid: Option<Grid>, into: &mut Laid) {
        match self {
            Self::Leaf(pane) => into.panes.push((*pane, at)),
            Self::Split { axis, ratio, first, second } => {
                let (origin, extent) = match axis {
                    Axis::Row => (at.origin.x, at.size.width),
                    Axis::Column => (at.origin.y, at.size.height),
                };
                let room = (f32::from(extent) - DIVIDER).max(0.0);
                let mut share = room * ratio;
                // A terminal's side gets whole cells past its frame, so its grid fills it to the
                // edge: the first child's when it is one grid across this axis, else the
                // second's.
                if let Some((step, frame)) = grid.and_then(|grid| grid.along(*axis)) {
                    let whole = |extent: f32| frame + ((extent - frame) / step).round() * step;
                    if first.one_grid(*axis) {
                        share = whole(share);
                    } else if second.one_grid(*axis) {
                        share = room - whole(room - share);
                    }
                }
                let floor = MIN_PANE.min(room / 2.0);
                let share = share.clamp(floor, (room - floor).max(floor));
                let divider = f32::from(origin) + share;
                into.dividers.push(Divider {
                    axis: *axis,
                    at: px(divider),
                    span: at,
                    path: into.path.clone(),
                });
                let (a, b) = match axis {
                    Axis::Row => (
                        Bounds::new(at.origin, size(px(share), at.size.height)),
                        Bounds::new(
                            point(px(divider + DIVIDER), at.origin.y),
                            size(px(room - share), at.size.height),
                        ),
                    ),
                    Axis::Column => (
                        Bounds::new(at.origin, size(at.size.width, px(share))),
                        Bounds::new(
                            point(at.origin.x, px(divider + DIVIDER)),
                            size(at.size.width, px(room - share)),
                        ),
                    ),
                };
                into.path.push(false);
                first.layout(a, grid, into);
                into.path.pop();
                into.path.push(true);
                second.layout(b, grid, into);
                into.path.pop();
            }
        }
    }

    /// Whether its extent along `axis` is one terminal's grid, so it snaps to whole cells: a
    /// terminal, or a split across the axis holding one, whose children all span it.
    fn one_grid(&self, axis: Axis) -> bool {
        match self {
            Self::Leaf(pane) => pane.is_terminal(),
            Self::Split { axis: across, first, second, .. } => {
                *across != axis && (first.one_grid(axis) || second.one_grid(axis))
            }
        }
    }

    /// The leaf nearest the far edge of its first child (`last`) or the near edge of its second.
    fn edge_leaf(&self, last: bool) -> Pane {
        match self {
            Self::Leaf(pane) => *pane,
            Self::Split { first, second, .. } => {
                if last { second.edge_leaf(true) } else { first.edge_leaf(false) }
            }
        }
    }

    fn at_path(&mut self, path: &[bool]) -> Option<&mut Self> {
        let Some((second, rest)) = path.split_first() else { return Some(self) };
        match self {
            Self::Leaf(_) => None,
            Self::Split { first, second: other, .. } => {
                if *second { other.at_path(rest) } else { first.at_path(rest) }
            }
        }
    }
}

/// A divider as laid out: where it stands across `axis`, the split's whole rectangle, and the
/// split's place in the tree (`false` first, `true` second, from the root).
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Divider {
    pub(crate) axis: Axis,
    pub(crate) at: Pixels,
    pub(crate) span: Bounds<Pixels>,
    pub(crate) path: Vec<bool>,
}

impl Divider {
    /// The divider's grab target: [`DIVIDER_GRAB`] across, its split's length along.
    pub(crate) fn grab(&self) -> Bounds<Pixels> {
        let half = (DIVIDER_GRAB - DIVIDER) / 2.0;
        match self.axis {
            Axis::Row => Bounds::new(
                point(self.at - px(half), self.span.origin.y),
                size(px(DIVIDER_GRAB), self.span.size.height),
            ),
            Axis::Column => Bounds::new(
                point(self.span.origin.x, self.at - px(half)),
                size(self.span.size.width, px(DIVIDER_GRAB)),
            ),
        }
    }
}

/// The tree laid out: each pane's rectangle in reading order, and each divider.
#[derive(Clone, Default, PartialEq, Debug)]
pub(crate) struct Laid {
    pub(crate) panes: Vec<(Pane, Bounds<Pixels>)>,
    pub(crate) dividers: Vec<Divider>,
    path: Vec<bool>,
}

impl Laid {
    /// Where `pane` stands.
    pub(crate) fn of(&self, pane: Pane) -> Option<Bounds<Pixels>> {
        self.panes.iter().find(|(laid, _)| *laid == pane).map(|(_, at)| *at)
    }
}

/// A session's view: its tree, the pane with the keyboard, and the pane filling the view while
/// zoomed.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Panes {
    root: Node,
    focused: Pane,
    zoomed: Option<Pane>,
}

impl Default for Panes {
    fn default() -> Self {
        Self { root: Node::Leaf(Pane::Session), focused: Pane::Session, zoomed: None }
    }
}

impl Panes {
    /// Its panes in reading order.
    pub(crate) fn panes(&self) -> Vec<Pane> {
        let mut panes = Vec::new();
        self.root.panes(&mut panes);
        panes
    }

    /// How many panes it holds; a header shows on each only past one.
    pub(crate) fn len(&self) -> usize {
        self.panes().len()
    }

    pub(crate) fn contains(&self, pane: Pane) -> bool {
        self.root.contains(pane)
    }

    pub(crate) const fn focused(&self) -> Pane {
        self.focused
    }

    pub(crate) const fn zoomed(&self) -> Option<Pane> {
        self.zoomed
    }

    /// Give `pane` the keyboard, when it is in the view. A zoom moves with it, as Ghostty's
    /// `split-preserve-zoom = navigation` does, so the pane focused is the one on screen.
    pub(crate) fn focus(&mut self, pane: Pane) -> bool {
        if !self.contains(pane) {
            return false;
        }
        self.focused = pane;
        if self.zoomed.is_some() {
            self.zoomed = Some(pane);
        }
        true
    }

    /// Open `new` beside the focused pane, toward `toward`, and focus it. Unzooms, so the new
    /// pane shows. `false` when `new` is already in the view.
    pub(crate) fn split(&mut self, toward: Direction, new: Pane) -> bool {
        if self.contains(new) {
            return false;
        }
        let (axis, after) = toward.split();
        self.zoomed = None;
        self.root.split(self.focused, axis, new, after);
        self.focused = new;
        true
    }

    /// The direction a split of the focused pane opens toward where there is most room (⌘⏎):
    /// along its longer side, as Ghostty's `new_split:auto` does.
    pub(crate) fn roomiest(&self, laid: &Laid) -> Direction {
        match laid.of(self.focused) {
            Some(at) if at.size.height > at.size.width => Direction::Down,
            _ => Direction::Right,
        }
    }

    /// The views in it, in reading order.
    pub(crate) fn views(&self) -> Vec<RightSurface> {
        self.panes()
            .into_iter()
            .filter_map(|pane| match pane {
                Pane::View(view) => Some(view),
                _ => None,
            })
            .collect()
    }

    /// Dock `view` beside the session and focus it (`docs/decisions/layout.md`, "No permanent
    /// right column"): the first view takes `share` of the session's room at its right; a view
    /// more stacks under the view with the keyboard, or else the last one, so the views stand in
    /// one column and the terminal keeps the larger part. Already in the view, it is focused.
    pub(crate) fn dock(&mut self, view: Pane, share: f32) {
        if !self.contains(view) {
            self.zoomed = None;
            let beside = match self.focused {
                pane @ Pane::View(_) => Some(pane),
                _ => self.panes().into_iter().rev().find(|pane| matches!(pane, Pane::View(_))),
            };
            match beside {
                Some(pane) => {
                    self.root.split(pane, Axis::Column, view, true);
                }
                None => {
                    self.root.split(Pane::Session, Axis::Row, view, true);
                    if let Some(path) = self.path_to_split_of(view)
                        && let Some(Node::Split { ratio, .. }) = self.root.at_path(&path)
                    {
                        *ratio = (1.0 - share).clamp(RATIO.0, RATIO.1);
                    }
                }
            }
        }
        self.focus(view);
    }

    /// Summon `view` by its key: docked and focused ([`Self::dock`]); already focused, the
    /// keyboard goes back to the session's terminal, as Esc does. The view stays either way.
    pub(crate) fn summon(&mut self, view: Pane, share: f32) {
        if self.focused == view {
            self.focus(Pane::Session);
        } else {
            self.dock(view, share);
        }
    }

    /// The path to the split whose child leaf is `pane`.
    fn path_to_split_of(&self, pane: Pane) -> Option<Vec<bool>> {
        fn walk(node: &Node, pane: Pane, path: &mut Vec<bool>) -> bool {
            let Node::Split { first, second, .. } = node else { return false };
            if **first == Node::Leaf(pane) || **second == Node::Leaf(pane) {
                return true;
            }
            for (side, child) in [(false, first), (true, second)] {
                path.push(side);
                if walk(child, pane, path) {
                    return true;
                }
                path.pop();
            }
            false
        }
        let mut path = Vec::new();
        walk(&self.root, pane, &mut path).then_some(path)
    }

    /// Close `pane`: its sibling takes its room, and the keyboard, when it had it, goes to the
    /// sibling's pane that stood nearest it, as Ghostty's does. The session's pane never
    /// closes. `false` when it was not there.
    pub(crate) fn close(&mut self, pane: Pane) -> bool {
        if pane == Pane::Session {
            return false;
        }
        let Some(nearest) = self.root.remove(pane) else { return false };
        if self.focused == pane {
            self.focused = nearest;
        }
        if self.zoomed == Some(pane) {
            self.zoomed = None;
        }
        true
    }

    /// Fill the view with the focused pane, or put the view back (⇧⌘⏎). Nothing with one pane.
    pub(crate) fn toggle_zoom(&mut self) {
        self.zoomed = match self.zoomed {
            Some(_) => None,
            None if self.len() > 1 => Some(self.focused),
            None => None,
        };
    }

    /// Every split back to even shares (a divider's double-click), as Ghostty's equalize.
    pub(crate) fn equalize(&mut self) {
        self.root.equalize();
    }

    /// The split at `path` (a divider's [`Divider::path`]) gives its first child `ratio` of
    /// the room, within the bounds a child keeps.
    pub(crate) fn set_ratio(&mut self, path: &[bool], to: f32) {
        if let Some(Node::Split { ratio, .. }) = self.root.at_path(path) {
            *ratio = to.clamp(RATIO.0, RATIO.1);
        }
    }

    /// Lay the view out in `at`, its terminals on `grid` when it is known: the zoomed pane
    /// alone over all of it, else every pane and divider.
    pub(crate) fn layout(&self, at: Bounds<Pixels>, grid: Option<Grid>) -> Laid {
        let mut laid = Laid::default();
        match self.zoomed {
            Some(pane) => laid.panes.push((pane, at)),
            None => self.root.layout(at, grid, &mut laid),
        }
        laid
    }

    /// The pane focus moves to from the focused one toward `toward` (⌥⌘ arrows): of the panes
    /// beyond its edge on that side that overlap it across, the nearest, then the one most
    /// overlapping. `None` at the view's edge.
    pub(crate) fn neighbour(&self, laid: &Laid, toward: Direction) -> Option<Pane> {
        let from = laid.of(self.focused)?;
        let span = |a: Pixels, b: Pixels, c: Pixels, d: Pixels| (b.min(d) - a.max(c)).max(px(0.0));
        laid.panes
            .iter()
            .filter(|(pane, _)| *pane != self.focused)
            .filter_map(|(pane, at)| {
                let (gap, overlap) = match toward {
                    Direction::Left => (from.left() - at.right(), span(from.top(), from.bottom(), at.top(), at.bottom())),
                    Direction::Right => (at.left() - from.right(), span(from.top(), from.bottom(), at.top(), at.bottom())),
                    Direction::Up => (from.top() - at.bottom(), span(from.left(), from.right(), at.left(), at.right())),
                    Direction::Down => (at.top() - from.bottom(), span(from.left(), from.right(), at.left(), at.right())),
                };
                (gap >= px(0.0) && overlap > px(0.0)).then_some((*pane, gap, overlap))
            })
            .min_by(|a, b| a.1.cmp(&b.1).then(b.2.cmp(&a.2)))
            .map(|(pane, ..)| pane)
    }

    /// The pane after (or before) the focused one in reading order, wrapping (⌘] ⌘[).
    pub(crate) fn cycle(&self, forward: bool) -> Pane {
        let panes = self.panes();
        let at = panes.iter().position(|pane| *pane == self.focused).unwrap_or(0);
        let next = if forward { (at + 1) % panes.len() } else { (at + panes.len() - 1) % panes.len() };
        panes[next]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> Bounds<Pixels> {
        Bounds::new(point(px(0.0), px(0.0)), size(px(1001.0), px(601.0)))
    }

    const CELL: Option<Grid> = Some(Grid {
        cell: Size { width: px(8.0), height: px(16.0) },
        frame: Size { width: px(0.0), height: px(0.0) },
    });

    /// A view starts as the session alone, its terminal over the whole room; ⌘D opens a shell
    /// to its right and ⇧⌘D one below the focused pane, each taking the keyboard, with a 1 pt
    /// divider between and the terminal's share in whole cells.
    #[test]
    fn splits_open_beside_the_focused_pane_snapped_to_cells() {
        let mut panes = Panes::default();
        assert_eq!(panes.layout(view(), CELL).panes, [(Pane::Session, view())]);
        assert!(panes.split(Direction::Right, Pane::Shell(1)));
        assert_eq!(panes.focused(), Pane::Shell(1));
        let laid = panes.layout(view(), CELL);
        let (left, right) = (laid.of(Pane::Session).unwrap(), laid.of(Pane::Shell(1)).unwrap());
        assert_eq!(f32::from(left.size.width) % 8.0, 0.0, "whole cells: {left:?}");
        assert_eq!(right.left() - left.right(), px(DIVIDER), "a 1 pt divider");
        assert_eq!(right.right(), view().right(), "edge to edge");
        assert!(panes.split(Direction::Down, Pane::Shell(2)));
        let laid = panes.layout(view(), CELL);
        let (top, bottom) = (laid.of(Pane::Shell(1)).unwrap(), laid.of(Pane::Shell(2)).unwrap());
        assert_eq!(top.left(), bottom.left(), "one over the other");
        assert_eq!(bottom.top() - top.bottom(), px(DIVIDER));
        assert_eq!(f32::from(top.size.height) % 16.0, 0.0, "whole rows: {top:?}");
        assert_eq!(panes.panes(), [Pane::Session, Pane::Shell(1), Pane::Shell(2)], "reading order");
        assert!(!panes.split(Direction::Right, Pane::Shell(2)), "a pane is in a view once");
    }

    /// A terminal's share is whole cells past its frame (its padding, and a header over a pane
    /// of a split), the first child's or else the second's; a split across the axis snaps as
    /// the grid its children share, and an unknown grid snaps nothing.
    #[test]
    fn a_terminals_share_is_whole_cells_past_its_frame() {
        let grid = Some(Grid {
            cell: size(px(8.0), px(16.0)),
            frame: size(px(12.0), px(30.0)),
        });
        let mut panes = Panes::default();
        panes.summon(Pane::View(RightSurface::Inspector), 0.37);
        let laid = panes.layout(view(), grid);
        let session = laid.of(Pane::Session).unwrap();
        assert_eq!((f32::from(session.size.width) - 12.0) % 8.0, 0.0, "{session:?}");
        panes.focus(Pane::Session);
        panes.split(Direction::Down, Pane::Shell(1));
        let laid = panes.layout(view(), grid);
        let (top, shell) = (laid.of(Pane::Session).unwrap(), laid.of(Pane::Shell(1)).unwrap());
        assert_eq!((f32::from(top.size.height) - 30.0) % 16.0, 0.0, "{top:?}");
        assert_eq!(top.size.width, shell.size.width, "one grid across");
        assert_eq!((f32::from(top.size.width) - 12.0) % 8.0, 0.0, "{top:?}");
        let loose = panes.layout(view(), None);
        assert_eq!(loose.of(Pane::Shell(1)).unwrap().size.height, px(300.0), "half of 600");
    }

    /// Focus walks by direction to the nearest pane overlapping across, stops at the view's
    /// edge, and walks every pane in reading order with ⌘] and ⌘[, wrapping.
    #[test]
    fn focus_walks_by_direction_and_in_reading_order() {
        let mut panes = Panes::default();
        panes.split(Direction::Right, Pane::Shell(1));
        panes.split(Direction::Down, Pane::Shell(2));
        let laid = panes.layout(view(), CELL);
        assert_eq!(panes.neighbour(&laid, Direction::Up), Some(Pane::Shell(1)));
        assert_eq!(panes.neighbour(&laid, Direction::Left), Some(Pane::Session));
        assert_eq!(panes.neighbour(&laid, Direction::Right), None, "the view's edge");
        assert_eq!(panes.cycle(true), Pane::Session, "wraps");
        assert_eq!(panes.cycle(false), Pane::Shell(1));
        assert!(!panes.clone().focus(Pane::Shell(9)), "not in the view");
    }

    /// Closing a pane gives its room to its sibling and the keyboard back to the session when
    /// it had it; the session's own pane never closes.
    #[test]
    fn a_closed_pane_gives_its_room_to_its_sibling() {
        let mut panes = Panes::default();
        panes.split(Direction::Right, Pane::Shell(1));
        panes.split(Direction::Down, Pane::Shell(2));
        assert!(panes.close(Pane::Shell(2)));
        assert_eq!(panes.focused(), Pane::Shell(1), "the pane that stood nearest");
        let laid = panes.layout(view(), CELL);
        assert_eq!(laid.of(Pane::Shell(1)).unwrap().size.height, view().size.height);
        assert!(!panes.close(Pane::Session), "the session stays");
        assert!(!panes.close(Pane::Shell(2)), "already gone");
        assert!(panes.close(Pane::Shell(1)));
        assert_eq!(panes, Panes::default(), "back to the session alone");
        panes.split(Direction::Left, Pane::Shell(3));
        panes.close(Pane::Shell(3));
        assert_eq!(panes.focused(), Pane::Session, "its sibling's nearest pane");
    }

    /// A summoned view docks at the right of the session as a pane of its split, at its share,
    /// whatever pane had the keyboard, and takes it; summoned again it is only focused, and its
    /// key pressed while it has the keyboard gives it back to the session's terminal.
    #[test]
    fn a_summoned_view_docks_at_the_sessions_right() {
        let mut panes = Panes::default();
        panes.split(Direction::Down, Pane::Shell(1));
        let inspector = Pane::View(RightSurface::Inspector);
        panes.summon(inspector, 0.4);
        assert_eq!(panes.focused(), inspector);
        let laid = panes.layout(view(), CELL);
        let (session, docked) = (laid.of(Pane::Session).unwrap(), laid.of(inspector).unwrap());
        assert_eq!(docked.left() - session.right(), px(DIVIDER), "at the session's right");
        assert_eq!(docked.top(), session.top());
        assert!((f32::from(docked.size.width) - 400.0).abs() <= 8.0, "its share: {docked:?}");
        assert_eq!(f32::from(session.size.width) % 8.0, 0.0, "the terminal in whole cells");
        let shape = panes.panes();
        panes.focus(Pane::Shell(1));
        panes.summon(inspector, 0.4);
        assert_eq!((panes.panes(), panes.focused()), (shape.clone(), inspector), "only focused");
        panes.summon(inspector, 0.4);
        assert_eq!((panes.panes(), panes.focused()), (shape, Pane::Session), "back to the terminal");
    }

    /// A view more stacks under the view with the keyboard, in the one column at the session's
    /// right, the session keeping its width; [`Panes::views`] reads them in order.
    #[test]
    fn views_stack_in_one_column_at_the_sessions_right() {
        let mut panes = Panes::default();
        let (files, inspector) = (Pane::View(RightSurface::Files), Pane::View(RightSurface::Inspector));
        panes.dock(files, 0.4);
        let session = panes.layout(view(), CELL).of(Pane::Session).unwrap();
        panes.focus(Pane::Session);
        panes.dock(inspector, 0.4);
        let laid = panes.layout(view(), CELL);
        assert_eq!(laid.of(Pane::Session), Some(session), "the session keeps its width");
        let (top, under) = (laid.of(files).unwrap(), laid.of(inspector).unwrap());
        assert_eq!((top.left(), top.size.width), (under.left(), under.size.width), "one column");
        assert_eq!(under.top() - top.bottom(), px(DIVIDER), "stacked under the last view");
        assert_eq!(panes.views(), [RightSurface::Files, RightSurface::Inspector]);
        assert_eq!(panes.focused(), inspector);
    }

    /// Zoom fills the view with the focused pane and keeps the tree; focus moving carries the
    /// zoom; a split unzooms so the new pane shows; one pane alone does not zoom.
    #[test]
    fn zoom_fills_the_view_and_keeps_the_tree() {
        let mut panes = Panes::default();
        panes.toggle_zoom();
        assert_eq!(panes.zoomed(), None, "nothing to zoom over");
        panes.split(Direction::Right, Pane::Shell(1));
        panes.toggle_zoom();
        assert_eq!(panes.layout(view(), CELL).panes, [(Pane::Shell(1), view())]);
        panes.focus(Pane::Session);
        assert_eq!(panes.zoomed(), Some(Pane::Session), "the zoom moves with focus");
        panes.split(Direction::Down, Pane::Shell(2));
        assert_eq!(panes.zoomed(), None, "a split shows");
        assert_eq!(panes.len(), 3);
    }

    /// A divider's grab target is 7 pt across its 1 pt line; dragging sets its split's share
    /// within 10-90 %, and a double-click evens every split.
    #[test]
    fn dividers_drag_within_bounds_and_double_click_evens() {
        let mut panes = Panes::default();
        panes.split(Direction::Right, Pane::Shell(1));
        let laid = panes.layout(view(), CELL);
        let divider = laid.dividers[0].clone();
        assert_eq!(divider.grab().size.width, px(DIVIDER_GRAB));
        assert!(divider.grab().contains(&point(divider.at, px(300.0))));
        panes.set_ratio(&divider.path, 0.99);
        let left = panes.layout(view(), CELL).of(Pane::Session).unwrap();
        assert!(f32::from(left.size.width) <= 0.9 * 1000.0 + 8.0, "held at 90 %: {left:?}");
        panes.equalize();
        let left = panes.layout(view(), CELL).of(Pane::Session).unwrap();
        assert!((f32::from(left.size.width) - 500.0).abs() <= 4.0, "even: {left:?}");
    }

    /// ⌘⏎ opens toward where there is most room: right in a wide pane, down in a tall one.
    #[test]
    fn auto_split_goes_where_there_is_most_room() {
        let panes = Panes::default();
        let wide = panes.layout(view(), CELL);
        assert_eq!(panes.roomiest(&wide), Direction::Right);
        let tall = Bounds::new(point(px(0.0), px(0.0)), size(px(400.0), px(900.0)));
        assert_eq!(panes.roomiest(&panes.layout(tall, CELL)), Direction::Down);
    }
}
