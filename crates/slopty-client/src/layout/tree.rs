//! A tab's tiling: an n-ary split tree whose leaves are panes, as a pure model.
//!
//! An inner node splits its area along one axis among its children by shares that sum to 1. A
//! leaf is a [`Pane`], which holds one tile or several as tabs. The tree is kept normal after
//! every edit, by `AeroSpace`'s and gpui-kit's rules (`dock/layout/normalize.rs`): no split has a
//! single child, no split directly holds a split of its own axis, and no pane is empty.
//!
//! Panes meet edge to edge: the rectangles a [`Tab`] lays out tile its area with no gap and no
//! overlap, and a sash is the line two neighbours share, drawn over it, taking no room. Shares
//! survive a window's resize; the minimums ([`Room`]) are points, and a sash drag never takes a
//! neighbour below them. A hidden pane (the tab's terminal put away) keeps its place and takes
//! no room. A zoomed pane takes the whole area while the zoom holds; any change to the tree lets
//! it go (Ghostty's rule).
//!
//! The rules and the edit vocabulary follow gpui-kit's `PaneTree` (`dock/layout`), `MonoCode`'s
//! `layout.ts` and Zed's `PaneGroup`; the ids are stable across edits, so a caller can key its
//! views on them.

use serde::{Deserialize, Serialize};

use super::{Rect, TileRef};

/// The narrowest a pane is let be, in points, with a pointer: about 64 columns of 13 pt mono.
pub const PANE_MIN_W: f32 = 520.0;
/// The narrowest a pane is let be on touch, in points.
pub const PANE_MIN_W_TOUCH: f32 = 480.0;
/// The shortest a pane is let be, in points: about 14 rows of 13 pt mono.
pub const PANE_MIN_H: f32 = 300.0;

/// The least room a pane takes.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Room {
    /// Width, in points.
    pub min_w: f32,
    /// Height, in points.
    pub min_h: f32,
}

impl Room {
    /// A pointer's.
    pub const POINTER: Self = Self { min_w: PANE_MIN_W, min_h: PANE_MIN_H };
    /// Touch's.
    pub const TOUCH: Self = Self { min_w: PANE_MIN_W_TOUCH, min_h: PANE_MIN_H };

    /// The least extent along `axis`.
    #[must_use]
    pub const fn along(self, axis: SplitAxis) -> f32 {
        match axis {
            SplitAxis::Row => self.min_w,
            SplitAxis::Column => self.min_h,
        }
    }
}

impl Default for Room {
    fn default() -> Self {
        Self::POINTER
    }
}

/// A pane's identity, stable across edits.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct PaneId(u64);

impl PaneId {
    /// Pane number `n`.
    pub(crate) const fn of(n: u64) -> Self {
        Self(n)
    }

    /// Its number: unique among the panes of one [`super::tiling::Tiling`].
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// How a split lays its children out.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum SplitAxis {
    /// Side by side, left to right.
    Row,
    /// Stacked, top to bottom.
    Column,
}

/// A side of a pane or of the tab: where a split goes, which way focus moves.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Side {
    /// Left.
    Left,
    /// Right.
    Right,
    /// Above.
    Top,
    /// Below.
    Bottom,
}

impl Side {
    /// The axis a split on this side runs along.
    #[must_use]
    pub const fn axis(self) -> SplitAxis {
        match self {
            Self::Left | Self::Right => SplitAxis::Row,
            Self::Top | Self::Bottom => SplitAxis::Column,
        }
    }

    /// Whether it comes after: right or below.
    #[must_use]
    pub const fn after(self) -> bool {
        matches!(self, Self::Right | Self::Bottom)
    }
}

/// A leaf: one tile, or several as tabs with one shown.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Pane {
    id: PaneId,
    tiles: Vec<TileRef>,
    shown: usize,
    hidden: bool,
}

impl Pane {
    /// A pane holding `tile`.
    #[must_use]
    pub fn new(id: PaneId, tile: TileRef) -> Self {
        Self { id, tiles: vec![tile], shown: 0, hidden: false }
    }

    /// A pane as it was kept: none when it holds nothing.
    pub(crate) fn restored(
        id: PaneId,
        tiles: Vec<TileRef>,
        shown: usize,
        hidden: bool,
    ) -> Option<Self> {
        let shown = shown.min(tiles.len().checked_sub(1)?);
        Some(Self { id, tiles, shown, hidden })
    }

    /// Its id.
    #[must_use]
    pub const fn id(&self) -> PaneId {
        self.id
    }

    /// Its tiles, as its tabs run.
    #[must_use]
    pub fn tiles(&self) -> &[TileRef] {
        &self.tiles
    }

    /// The tile it shows.
    #[must_use]
    pub fn shown(&self) -> Option<TileRef> {
        self.tiles.get(self.shown).copied()
    }

    /// Which of its tabs it shows.
    #[must_use]
    pub const fn shown_index(&self) -> usize {
        self.shown
    }

    /// Put away, taking no room.
    #[must_use]
    pub const fn hidden(&self) -> bool {
        self.hidden
    }

    /// Show `tile`, when it holds it.
    pub fn show(&mut self, tile: TileRef) -> bool {
        let Some(at) = self.tiles.iter().position(|t| *t == tile) else { return false };
        self.shown = at;
        true
    }

    /// Add `tile` as a tab after the one shown, and show it.
    pub fn push(&mut self, tile: TileRef) {
        if self.show(tile) {
            return;
        }
        let at = self.shown.saturating_add(1).min(self.tiles.len());
        self.tiles.insert(at, tile);
        self.shown = at;
    }

    /// Show the tab `step` away from the one shown, round the ends.
    pub const fn step(&mut self, forward: bool) {
        let n = self.tiles.len();
        if n > 1 {
            self.shown = round_step(self.shown, n, forward);
        }
    }

    /// Take `tile` out; the tab before it is shown where it was.
    fn take(&mut self, tile: TileRef) -> bool {
        let Some(at) = self.tiles.iter().position(|t| *t == tile) else { return false };
        self.tiles.remove(at);
        if self.shown > at || self.shown >= self.tiles.len() {
            self.shown = self.shown.saturating_sub(1);
        }
        true
    }
}

/// An inner node: its children along `axis`, by `shares` summing to 1.
#[derive(Clone, PartialEq, Debug)]
pub struct Split {
    axis: SplitAxis,
    children: Vec<Node>,
    shares: Vec<f32>,
}

impl Split {
    /// A split of `children` along `axis` by `shares`, as given: the tab brings it to its
    /// normal form.
    pub(crate) const fn of(axis: SplitAxis, children: Vec<Node>, shares: Vec<f32>) -> Self {
        Self { axis, children, shares }
    }

    /// A split of nothing: a place held while a tree is rebuilt.
    pub(crate) const fn empty(axis: SplitAxis) -> Self {
        Self { axis, children: Vec::new(), shares: Vec::new() }
    }

    /// Its axis.
    #[must_use]
    pub const fn axis(&self) -> SplitAxis {
        self.axis
    }

    /// Its children.
    #[must_use]
    pub fn children(&self) -> &[Node] {
        &self.children
    }

    /// Each child's share.
    #[must_use]
    pub fn shares(&self) -> &[f32] {
        &self.shares
    }

    /// The shares made equal.
    fn equalize(&mut self) {
        let n = count(self.children.len()).max(1.0);
        self.shares = vec![1.0 / n; self.children.len()];
    }
}

/// A node of the tree.
#[derive(Clone, PartialEq, Debug)]
pub enum Node {
    /// An inner node.
    Split(Split),
    /// A leaf.
    Pane(Pane),
}

impl Node {
    /// Every pane under it, left to right and top to bottom.
    pub fn panes(&self) -> Box<dyn Iterator<Item = &Pane> + '_> {
        match self {
            Self::Pane(p) => Box::new(std::iter::once(p)),
            Self::Split(s) => Box::new(s.children.iter().flat_map(Self::panes)),
        }
    }

    fn panes_mut(&mut self) -> Box<dyn Iterator<Item = &mut Pane> + '_> {
        match self {
            Self::Pane(p) => Box::new(std::iter::once(p)),
            Self::Split(s) => Box::new(s.children.iter_mut().flat_map(Self::panes_mut)),
        }
    }

    /// The node at `path`, a child index per level.
    #[must_use]
    pub fn at(&self, path: &[usize]) -> Option<&Self> {
        let Some((first, rest)) = path.split_first() else { return Some(self) };
        match self {
            Self::Split(s) => s.children.get(*first)?.at(rest),
            Self::Pane(_) => None,
        }
    }

    fn at_mut(&mut self, path: &[usize]) -> Option<&mut Self> {
        let Some((first, rest)) = path.split_first() else { return Some(self) };
        match self {
            Self::Split(s) => s.children.get_mut(*first)?.at_mut(rest),
            Self::Pane(_) => None,
        }
    }

    /// The path to pane `id`.
    #[must_use]
    pub fn path_of(&self, id: PaneId) -> Option<Vec<usize>> {
        match self {
            Self::Pane(p) => (p.id == id).then(Vec::new),
            Self::Split(s) => s.children.iter().enumerate().find_map(|(i, c)| {
                let mut path = c.path_of(id)?;
                path.insert(0, i);
                Some(path)
            }),
        }
    }

    /// Whether anything under it takes room.
    fn shows(&self) -> bool {
        self.panes().any(|p| !p.hidden)
    }

    /// The least extent it takes along `axis`: a row of panes the sum of theirs across it, a
    /// stack the most of theirs.
    fn least(&self, axis: SplitAxis, room: Room) -> f32 {
        match self {
            Self::Pane(p) if p.hidden => 0.0,
            Self::Pane(_) => room.along(axis),
            Self::Split(s) => {
                let each = s.children.iter().map(|c| c.least(axis, room));
                if s.axis == axis { each.sum() } else { each.fold(0.0, f32::max) }
            }
        }
    }

    /// The normal form of this node, or none when nothing is left in it: empty panes gone, a
    /// split of one child that child, a split's children of its own axis spliced in, shares
    /// that are not numbers or not positive made equal, every share set summing to 1.
    fn normal(self) -> Option<Self> {
        match self {
            Self::Pane(mut p) => {
                if p.tiles.is_empty() {
                    return None;
                }
                p.shown = p.shown.min(p.tiles.len().saturating_sub(1));
                Some(Self::Pane(p))
            }
            Self::Split(Split { axis, children, shares }) => {
                let fair = 1.0 / count(children.len()).max(1.0);
                let mut kids: Vec<Self> = Vec::new();
                let mut parts: Vec<f32> = Vec::new();
                for (i, child) in children.into_iter().enumerate() {
                    let share = shares.get(i).copied().filter(|s| s.is_finite() && *s > 0.0);
                    let share = share.unwrap_or(fair);
                    match child.normal() {
                        None => {}
                        Some(Self::Split(inner)) if inner.axis == axis => {
                            for (c, s) in inner.children.into_iter().zip(inner.shares) {
                                kids.push(c);
                                parts.push(share * s);
                            }
                        }
                        Some(node) => {
                            kids.push(node);
                            parts.push(share);
                        }
                    }
                }
                match kids.len() {
                    0 => None,
                    1 => kids.pop(),
                    _ => {
                        let sum: f32 = parts.iter().sum();
                        // Shares that sum to 1 already stay as they are, so the form is a fixed
                        // point.
                        let whole = (sum - 1.0).abs() <= 1e-6;
                        let shares = if whole {
                            parts
                        } else {
                            parts.into_iter().map(|p| p / sum).collect()
                        };
                        Some(Self::Split(Split { axis, children: kids, shares }))
                    }
                }
            }
        }
    }

    /// Lay it out in `area`, each pane's rectangle onto `out` and each sash onto `sashes`.
    fn lay_out(
        &self,
        area: Rect,
        path: &mut Vec<usize>,
        out: &mut Vec<Laid>,
        sashes: &mut Vec<Sash>,
    ) {
        match self {
            Self::Pane(p) if p.hidden => {}
            Self::Pane(p) => out.push(Laid { pane: p.id, rect: area }),
            Self::Split(s) => {
                let parts = s.parts(area);
                let mut last: Option<(usize, Rect)> = None;
                for (i, part) in parts.iter().enumerate() {
                    let Some(rect) = *part else { continue };
                    if let Some((before, prior)) = last {
                        let line = match s.axis {
                            SplitAxis::Row => Rect { x: prior.right(), w: 0.0, ..prior },
                            SplitAxis::Column => Rect { y: prior.bottom(), h: 0.0, ..prior },
                        };
                        sashes.push(Sash {
                            path: path.clone(),
                            before,
                            after: i,
                            axis: s.axis,
                            line,
                        });
                    }
                    path.push(i);
                    if let Some(child) = s.children.get(i) {
                        child.lay_out(rect, path, out, sashes);
                    }
                    path.pop();
                    last = Some((i, rect));
                }
            }
        }
    }
}

impl Split {
    /// Each child's rectangle in `area`, `None` for one that takes no room: the children that
    /// show anything share the extent by their shares, each edge on a whole point and the last
    /// meeting the area's own.
    fn parts(&self, area: Rect) -> Vec<Option<Rect>> {
        let shows: Vec<bool> = self.children.iter().map(Node::shows).collect();
        let total: f32 =
            self.shares.iter().zip(&shows).filter(|(_, s)| **s).map(|(share, _)| *share).sum();
        let (start, extent) = match self.axis {
            SplitAxis::Row => (area.x, area.w),
            SplitAxis::Column => (area.y, area.h),
        };
        let lastshown = shows.iter().rposition(|s| *s);
        let mut cum = 0.0;
        let mut from = start;
        let mut parts = Vec::with_capacity(self.children.len());
        for (i, shown) in shows.iter().enumerate() {
            if !*shown || total <= 0.0 {
                parts.push(None);
                continue;
            }
            cum += self.shares.get(i).copied().unwrap_or(0.0) / total;
            let to =
                if Some(i) == lastshown { start + extent } else { start + (cum * extent).round() };
            parts.push(Some(match self.axis {
                SplitAxis::Row => Rect { x: from, w: to - from, ..area },
                SplitAxis::Column => Rect { y: from, h: to - from, ..area },
            }));
            from = to;
        }
        parts
    }
}

/// A pane's rectangle.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Laid {
    /// The pane.
    pub pane: PaneId,
    /// Where, in the area given.
    pub rect: Rect,
}

/// The line two neighbours of a split share, where a drag resizes them.
#[derive(Clone, PartialEq, Debug)]
pub struct Sash {
    /// The split's path.
    pub path: Vec<usize>,
    /// The child before it.
    pub before: usize,
    /// The child after it (the next one shown: a hidden child between takes no room).
    pub after: usize,
    /// The split's axis: a row's sash is upright.
    pub axis: SplitAxis,
    /// The line, of no width (a row's) or no height (a column's).
    pub line: Rect,
}

/// A tab's layout in an area.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct TabFrame {
    /// Each pane that takes room, left to right and top to bottom; the zoomed one alone while
    /// a zoom holds.
    pub panes: Vec<Laid>,
    /// Each sash; none while a zoom holds.
    pub sashes: Vec<Sash>,
}

impl TabFrame {
    /// Where pane `id` is.
    #[must_use]
    pub fn rect(&self, id: PaneId) -> Option<Rect> {
        self.panes.iter().find(|l| l.pane == id).map(|l| l.rect)
    }
}

/// What taking a tile out of a tab left.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Taken {
    /// The tab did not hold it.
    Absent,
    /// It is gone and the tab holds more.
    Gone,
    /// It was the tab's last: the tab is empty, for its holder to drop.
    Emptied,
}

/// A tab: its tree, the pane with the focus, the one zoomed, and its terminal's.
#[derive(Clone, PartialEq, Debug)]
pub struct Tab {
    id: super::tiling::TabId,
    root: Node,
    focus: PaneId,
    zoomed: Option<PaneId>,
    terminal: Option<PaneId>,
}

impl Tab {
    /// A tab of one pane.
    #[must_use]
    pub const fn new(id: super::tiling::TabId, pane: Pane) -> Self {
        let focus = pane.id;
        Self::new_root(id, Node::Pane(pane), focus)
    }

    /// A tab as it was kept, its panes named by path: none when nothing is left in it.
    pub(crate) fn restored(
        id: super::tiling::TabId,
        root: Node,
        focus: &[usize],
        zoomed: bool,
        terminal: Option<&[usize]>,
    ) -> Option<Self> {
        let pane_at = |root: &Node, path: &[usize]| match root.at(path) {
            Some(Node::Pane(p)) => Some(p.id),
            _ => None,
        };
        // The paths name the tree as kept, before it is made normal.
        let (focus, terminal) = (pane_at(&root, focus), terminal.and_then(|t| pane_at(&root, t)));
        let first = root.panes().next()?.id;
        let mut tab = Self::new_root(id, root, focus.unwrap_or(first));
        if !tab.normalize() {
            return None;
        }
        if tab.pane(tab.focus).is_none() {
            let first = tab.panes().next()?.id;
            tab.focus = first;
        }
        tab.zoomed = zoomed.then_some(tab.focus);
        tab.terminal = terminal.filter(|t| tab.pane(*t).is_some());
        Some(tab)
    }

    const fn new_root(id: super::tiling::TabId, root: Node, focus: PaneId) -> Self {
        Self { id, root, focus, zoomed: None, terminal: None }
    }

    /// Put `tile` in a new pane `id` below the whole tree, `share` of the height, focused: the
    /// tab's terminal. Lets a zoom go.
    pub(crate) fn add_terminal(&mut self, id: PaneId, tile: TileRef, share: f32) {
        self.zoomed = None;
        let root = std::mem::replace(&mut self.root, Node::Split(Split::empty(SplitAxis::Column)));
        let pane = Node::Pane(Pane::new(id, tile));
        self.root =
            Node::Split(Split::of(SplitAxis::Column, vec![root, pane], vec![1.0 - share, share]));
        self.normalize();
        self.terminal = Some(id);
        self.focus = id;
    }

    /// Its id.
    #[must_use]
    pub const fn id(&self) -> super::tiling::TabId {
        self.id
    }

    /// Its tree.
    #[must_use]
    pub const fn root(&self) -> &Node {
        &self.root
    }

    /// The pane with the focus.
    #[must_use]
    pub const fn focus(&self) -> PaneId {
        self.focus
    }

    /// The pane zoomed over the whole tab.
    #[must_use]
    pub const fn zoomed(&self) -> Option<PaneId> {
        self.zoomed
    }

    /// The pane of the tab's terminal (⌘⌥T), shown or put away.
    #[must_use]
    pub const fn terminal(&self) -> Option<PaneId> {
        self.terminal
    }

    /// Its panes, left to right and top to bottom.
    pub fn panes(&self) -> impl Iterator<Item = &Pane> {
        self.root.panes()
    }

    /// Pane `id`.
    #[must_use]
    pub fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.root.panes().find(|p| p.id == id)
    }

    pub(crate) fn pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        self.root.panes_mut().find(|p| p.id == id)
    }

    /// Every tile, pane by pane.
    pub fn tiles(&self) -> impl Iterator<Item = TileRef> + '_ {
        self.root.panes().flat_map(|p| p.tiles.iter().copied())
    }

    /// The pane holding `tile`.
    #[must_use]
    pub fn pane_of(&self, tile: TileRef) -> Option<PaneId> {
        self.root.panes().find(|p| p.tiles.contains(&tile)).map(|p| p.id)
    }

    /// The tile the focused pane shows.
    #[must_use]
    pub fn focused(&self) -> Option<TileRef> {
        self.pane(self.focus).and_then(Pane::shown)
    }

    /// Its layout in `area`.
    #[must_use]
    pub fn frame(&self, area: Rect) -> TabFrame {
        if let Some(zoomed) = self.zoomed.filter(|z| self.pane(*z).is_some()) {
            return TabFrame { panes: vec![Laid { pane: zoomed, rect: area }], sashes: Vec::new() };
        }
        let (mut panes, mut sashes) = (Vec::new(), Vec::new());
        self.root.lay_out(area, &mut Vec::new(), &mut panes, &mut sashes);
        TabFrame { panes, sashes }
    }

    /// Focus pane `id`, showing `tile` in it when given.
    pub fn focus_pane(&mut self, id: PaneId, tile: Option<TileRef>) -> bool {
        let Some(pane) = self.pane_mut(id) else { return false };
        if let Some(tile) = tile {
            pane.show(tile);
        }
        if pane.hidden {
            pane.hidden = false;
        }
        self.focus = id;
        true
    }

    /// Put `tile` in a new pane `id` on `side` of pane `of`, focused: beside it in its split
    /// when that runs the same way (the row's shares made equal again), else in a new split of
    /// the two, halves each. Lets a zoom go.
    pub fn split(&mut self, of: PaneId, side: Side, id: PaneId, tile: TileRef) -> bool {
        let Some(path) = self.root.path_of(of) else { return false };
        self.zoomed = None;
        let new = Node::Pane(Pane::new(id, tile));
        let axis = side.axis();
        let parent = path.split_last().map(|(last, up)| (*last, up.to_vec()));
        let same = parent.as_ref().and_then(|(i, up)| match self.root.at_mut(up) {
            Some(Node::Split(s)) if s.axis == axis => Some((*i, s)),
            _ => None,
        });
        if let Some((i, split)) = same {
            let at = if side.after() { i.saturating_add(1) } else { i };
            split.children.insert(at, new);
            split.shares.insert(at, 0.0);
            split.equalize();
        } else if let Some(node) = self.root.at_mut(&path) {
            let old = std::mem::replace(node, Node::Pane(Pane::new(id, tile)));
            let children = if side.after() { vec![old, new] } else { vec![new, old] };
            *node = Node::Split(Split { axis, children, shares: vec![0.5, 0.5] });
        }
        self.normalize();
        self.focus = id;
        true
    }

    /// Add `tile` to pane `id` as a tab, shown and focused.
    pub fn join(&mut self, id: PaneId, tile: TileRef) -> bool {
        let Some(pane) = self.pane_mut(id) else { return false };
        pane.push(tile);
        pane.hidden = false;
        self.focus = id;
        true
    }

    /// Take `tile` out. A pane it leaves empty goes, its room going to its neighbours, and
    /// the focus to the pane that took its place. Lets a zoom go.
    pub fn take(&mut self, tile: TileRef) -> Taken {
        let Some(id) = self.pane_of(tile) else { return Taken::Absent };
        self.zoomed = None;
        let emptied = self.pane_mut(id).is_some_and(|p| p.take(tile) && p.tiles.is_empty());
        if !emptied {
            return Taken::Gone;
        }
        // The neighbour that takes the room: the one after it in its split, else before.
        let next = self.neighbour_in_tree(id);
        if !self.normalize() {
            return Taken::Emptied;
        }
        if self.terminal == Some(id) {
            self.terminal = None;
        }
        if self.focus == id || self.pane(self.focus).is_none() {
            let fallback = self.panes().find(|p| !p.hidden).or_else(|| self.panes().next());
            self.focus = next
                .filter(|n| self.pane(*n).is_some())
                .or_else(|| fallback.map(Pane::id))
                .unwrap_or(id);
        }
        Taken::Gone
    }

    /// The pane nearest `id` in its split: the first of the next sibling, else of the one
    /// before.
    fn neighbour_in_tree(&self, id: PaneId) -> Option<PaneId> {
        let path = self.root.path_of(id)?;
        let (last, up) = path.split_last()?;
        let Node::Split(s) = self.root.at(up)? else { return None };
        let near = s
            .children
            .get(last.saturating_add(1))
            .or_else(|| last.checked_sub(1).and_then(|i| s.children.get(i)));
        near?.panes().find(|p| !p.hidden && p.id != id).map(Pane::id)
    }

    /// Bring the tree back to its normal form; false when nothing is left in it.
    pub fn normalize(&mut self) -> bool {
        let placeholder =
            Node::Split(Split { axis: SplitAxis::Row, children: Vec::new(), shares: Vec::new() });
        let root = std::mem::replace(&mut self.root, placeholder);
        match root.normal() {
            Some(root) => {
                self.root = root;
                if self.zoomed.is_some_and(|z| self.pane(z).is_none()) {
                    self.zoomed = None;
                }
                if self.terminal.is_some_and(|t| self.pane(t).is_none()) {
                    self.terminal = None;
                }
                true
            }
            None => false,
        }
    }

    /// The pane on `side` of pane `of` in `area`: of those whose edge meets that side, the one
    /// it overlaps most, the nearer centre breaking a tie and then the one above or left
    /// (`MonoCode`'s `neighborLeafId`).
    #[must_use]
    pub fn neighbour(&self, of: PaneId, side: Side, area: Rect) -> Option<PaneId> {
        let frame = self.frame(area);
        let from = frame.rect(of)?;
        let touching = |r: &Rect| match side {
            Side::Left => near(r.right(), from.x),
            Side::Right => near(r.x, from.right()),
            Side::Top => near(r.bottom(), from.y),
            Side::Bottom => near(r.y, from.bottom()),
        };
        let overlap = |r: &Rect| match side.axis() {
            SplitAxis::Row => (r.bottom().min(from.bottom()) - r.y.max(from.y)).max(0.0),
            SplitAxis::Column => (r.right().min(from.right()) - r.x.max(from.x)).max(0.0),
        };
        let centre = |r: &Rect| match side.axis() {
            SplitAxis::Row => (r.y + r.h / 2.0 - (from.y + from.h / 2.0)).abs(),
            SplitAxis::Column => (r.x + r.w / 2.0 - (from.x + from.w / 2.0)).abs(),
        };
        // From the end, so of two alike the first (above, or left) wins.
        frame
            .panes
            .iter()
            .rev()
            .filter(|l| l.pane != of && touching(&l.rect) && overlap(&l.rect) > 0.0)
            .max_by(|a, b| {
                overlap(&a.rect)
                    .total_cmp(&overlap(&b.rect))
                    .then(centre(&b.rect).total_cmp(&centre(&a.rect)))
            })
            .map(|l| l.pane)
    }

    /// Move `tile` toward `side`: into the neighbouring pane there as a tab, focused; at the
    /// tab's edge, out into a new pane `id` along that edge (Zed's `move_to_border`). A lone
    /// tile already alone at that edge stays. Lets a zoom go.
    pub fn move_tile(&mut self, tile: TileRef, side: Side, area: Rect, id: PaneId) -> bool {
        let Some(from) = self.pane_of(tile) else { return false };
        if let Some(to) = self.neighbour(from, side, area) {
            self.take(tile);
            return self.join(to, tile);
        }
        let alone = self.pane(from).is_some_and(|p| p.tiles.len() == 1);
        if alone && self.panes().count() == 1 {
            return false;
        }
        if alone && self.at_border(from, side) {
            return false;
        }
        self.take(tile);
        self.zoomed = None;
        let new = Node::Pane(Pane::new(id, tile));
        let axis = side.axis();
        let placeholder = Node::Split(Split { axis, children: Vec::new(), shares: Vec::new() });
        let root = std::mem::replace(&mut self.root, placeholder);
        let n = match &root {
            Node::Split(s) if s.axis == axis => count(s.children.len()),
            _ => 1.0,
        };
        // The new pane takes one equal part of the edge it lands on.
        let share = 1.0 / (n + 1.0);
        let (children, shares) = if side.after() {
            (vec![root, new], vec![1.0 - share, share])
        } else {
            (vec![new, root], vec![share, 1.0 - share])
        };
        self.root = Node::Split(Split { axis, children, shares });
        self.normalize();
        self.focus = id;
        true
    }

    /// Whether pane `id` is the last one on `side` of the tree, in its own whole span along the
    /// root (a root split of that axis with it at that end).
    fn at_border(&self, id: PaneId, side: Side) -> bool {
        match &self.root {
            Node::Split(s) if s.axis == side.axis() => {
                let end = if side.after() { s.children.last() } else { s.children.first() };
                matches!(end, Some(Node::Pane(p)) if p.id == id)
            }
            _ => false,
        }
    }

    /// Drag `sash` by `delta` points (right or down is positive), the tab laid out in `area`:
    /// each side keeps at least the room its panes need. Returns how far it moved.
    pub fn drag_sash(&mut self, sash: &Sash, delta: f32, area: Rect, room: Room) -> f32 {
        let (before, after) = (sash.before, sash.after);
        let Some(rect) = self.rect_of(&sash.path, area) else { return 0.0 };
        let Some(Node::Split(s)) = self.root.at_mut(&sash.path) else { return 0.0 };
        let extent = match s.axis {
            SplitAxis::Row => rect.w,
            SplitAxis::Column => rect.h,
        };
        let shown: f32 =
            s.children.iter().zip(&s.shares).filter(|(c, _)| c.shows()).map(|(_, sh)| *sh).sum();
        let (Some(a), Some(b)) = (s.shares.get(before).copied(), s.shares.get(after).copied())
        else {
            return 0.0;
        };
        let (Some(first), Some(second)) = (s.children.get(before), s.children.get(after)) else {
            return 0.0;
        };
        if before >= after || extent <= 0.0 || shown <= 0.0 {
            return 0.0;
        }
        let unit = extent / shown;
        let (a, b) = (a * unit, b * unit);
        let (least_a, least_b) = (first.least(s.axis, room), second.least(s.axis, room));
        // Neither side goes below its least; where one already is, the sash holds that way.
        let lo = (least_a - a).min(0.0);
        let hi = (b - least_b).max(0.0);
        let moved = delta.clamp(lo, hi);
        if let Some(share) = s.shares.get_mut(before) {
            *share = (a + moved) / unit;
        }
        if let Some(share) = s.shares.get_mut(after) {
            *share = (b - moved) / unit;
        }
        moved
    }

    /// Make the shares of the split at `path` equal again (a double-click on its sash).
    pub fn equalize(&mut self, path: &[usize]) -> bool {
        match self.root.at_mut(path) {
            Some(Node::Split(s)) => {
                s.equalize();
                true
            }
            _ => false,
        }
    }

    /// Make every split's shares equal ("Equalize panes").
    pub fn equalize_all(&mut self) {
        fn walk(node: &mut Node) {
            if let Node::Split(s) = node {
                s.equalize();
                s.children.iter_mut().for_each(walk);
            }
        }
        walk(&mut self.root);
    }

    /// The rectangle the node at `path` is laid out in, in `area`.
    fn rect_of(&self, path: &[usize], area: Rect) -> Option<Rect> {
        let mut node = &self.root;
        let mut rect = area;
        for i in path {
            let Node::Split(s) = node else { return None };
            rect = s.parts(rect).get(*i).copied().flatten()?;
            node = s.children.get(*i)?;
        }
        Some(rect)
    }

    /// Zoom the focused pane over the whole tab, or let the zoom go.
    pub const fn toggle_zoom(&mut self) -> bool {
        self.zoomed = match self.zoomed {
            Some(_) => None,
            None => Some(self.focus),
        };
        self.zoomed.is_some()
    }

    /// Put pane `id` away or bring it back; a pane put away hands the focus to the first
    /// shown one, and one brought back takes it.
    pub fn set_hidden(&mut self, id: PaneId, hidden: bool) -> bool {
        let others = self.panes().any(|p| p.id != id && !p.hidden);
        let Some(pane) = self.pane_mut(id) else { return false };
        if hidden && !others {
            return false;
        }
        pane.hidden = hidden;
        if hidden && self.focus == id {
            let first = self.panes().find(|p| !p.hidden).map(Pane::id);
            if let Some(first) = first {
                self.focus = first;
            }
        } else if !hidden {
            self.focus = id;
        }
        if self.zoomed == Some(id) && hidden {
            self.zoomed = None;
        }
        true
    }
}

/// The index `forward` (else back) of `at` among `n`, round the ends.
pub(crate) const fn round_step(at: usize, n: usize, forward: bool) -> usize {
    if n == 0 {
        return 0;
    }
    if forward {
        if at.saturating_add(1) >= n { 0 } else { at.saturating_add(1) }
    } else {
        match at.checked_sub(1) {
            Some(back) if back < n => back,
            _ => n.saturating_sub(1),
        }
    }
}

/// Whether two coordinates are the same edge.
fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.5
}

#[expect(clippy::cast_precision_loss, reason = "counts of panes are tiny")]
const fn count(n: usize) -> f32 {
    n as f32
}
