//! The workspace layout: niri's scrollable tiling, as a pure model.
//!
//! Workspaces are stacked vertically, always with one empty workspace at the end. Each holds an
//! infinite horizontal strip of columns; each column holds tiles stacked vertically (or tabbed).
//! The view looks at a window of the strip. Everything is a port of niri v26.04
//! (`src/layout/{scrolling,monitor}.rs`), cut down to one monitor and no floating layer.
//!
//! Pure: no clock, no toolkit. The caller supplies the time ([`Layout::set_clock`]) and the
//! viewport ([`Layout::set_viewport`]) and reads a [`Frame`] back. Tiles name `(worker, item)`
//! pairs; what a tile shows is the caller's business.
//!
//! # Coordinates
//!
//! * **Strip** coordinates: x = 0 at the left edge of a workspace's first column; y = 0 at the top
//!   of the workspace. Column `i` starts at `Σ (width + gaps)` of the columns before it.
//! * **View** coordinates: strip x minus the view position (`column_x(active) + view_offset`).
//! * **Viewport** coordinates (what [`Frame`] reports): view coordinates placed into the
//!   workspace's rectangle, scaled by the overview zoom. `(0, 0)` is the top-left of the area given
//!   to [`Layout::set_viewport`].
//!
//! Gesture deltas are in view-offset space: a positive `dx` moves the view right (the content
//! left), a positive `dy` goes towards the workspace below.

mod spring;
mod swipe;

use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use slopty_core::ItemId;
pub use spring::{Animation, Curve, Spring, SpringParams};
pub use swipe::{Axis, AxisLock, RubberBand, SwipeTracker, WheelTracker};

/// How long a new tile takes to appear.
const OPEN_FOR: Duration = Duration::from_millis(150);
/// How long a closed tile takes to fade.
const CLOSE_FOR: Duration = Duration::from_millis(150);
/// Size changes no larger than this land at once rather than animate.
const RESIZE_THRESHOLD: f32 = 10.0;
/// Gap between workspaces as a share of the viewport height.
const WORKSPACE_GAP: f32 = 0.1;
/// The overview's zoom.
const OVERVIEW_ZOOM: f32 = 0.5;
/// A wheel step, in points: how far a ⌘⌥ wheel moves before a column or workspace step.
pub const WHEEL_TICK: f32 = 50.0;
/// Workspace steps from the wheel are at least this far apart.
const WHEEL_COOLDOWN: Duration = Duration::from_millis(150);
/// Edge scrolling during a drag: the band at each edge that scrolls, the wait before it starts, and
/// the speed at the very edge.
const DND_TRIGGER: f32 = 30.0;
const DND_DELAY: Duration = Duration::from_millis(100);
const DND_SPEED: f32 = 1500.0;
/// The workspace gesture's resistance past the neighbours.
const WORKSPACE_BAND: RubberBand = RubberBand { stiffness: 0.5, limit: 0.05 };
/// A drop within this share of a column's width from its edge makes a new column.
const DROP_EDGE: f32 = 0.2;

/// Horizontal view movement, window movement and resize, and the overview: 1.0 / 800 / 0.0001.
fn view_spring() -> SpringParams {
    SpringParams::new(1.0, 800.0, 0.0001)
}

/// The workspace switch: 1.0 / 1000 / 0.0001.
fn switch_spring() -> SpringParams {
    SpringParams::new(1.0, 1000.0, 0.0001)
}

#[expect(clippy::cast_possible_truncation, reason = "layout values are points, well inside f32")]
const fn narrow(v: f64) -> f32 {
    v as f32
}

#[expect(clippy::cast_precision_loss, reason = "counts of tiles and workspaces are tiny")]
const fn count(n: usize) -> f32 {
    n as f32
}

/// An opaque worker identity: whatever the caller keys its workers by, squeezed into 128 bits.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkerKey(u128);

impl WorkerKey {
    /// A key from its value.
    #[must_use]
    pub const fn new(value: u128) -> Self {
        Self(value)
    }

    /// A key from the first 16 bytes of `bytes`, big-endian, zero-padded.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut buf = [0_u8; 16];
        for (to, from) in buf.iter_mut().zip(bytes) {
            *to = *from;
        }
        Self(u128::from_be_bytes(buf))
    }

    /// The value.
    #[must_use]
    pub const fn value(self) -> u128 {
        self.0
    }
}

impl fmt::Display for WorkerKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

impl fmt::Debug for WorkerKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WorkerKey({self})")
    }
}

impl Serialize for WorkerKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for WorkerKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        u128::from_str_radix(&text, 16).map(Self).map_err(serde::de::Error::custom)
    }
}

/// One tile's identity: an item on a worker.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct TileRef {
    /// The worker the item lives on.
    pub worker: WorkerKey,
    /// The item.
    pub item: ItemId,
}

/// A rectangle in points.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rect {
    /// Left.
    pub x: f32,
    /// Top.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

impl Rect {
    /// Whether `(x, y)` is inside (right and bottom edges excluded).
    #[must_use]
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }

    /// Whether the two share any area.
    #[must_use]
    pub fn intersects(&self, other: &Self) -> bool {
        self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }

    /// Right edge.
    #[must_use]
    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    /// Bottom edge.
    #[must_use]
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

/// How wide a column is.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum ColumnWidth {
    /// A share of the working width: `(working − gaps) × p − gaps`.
    Proportion(f32),
    /// Points.
    Fixed(f32),
}

/// How a column shows its tiles.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum DisplayMode {
    /// Stacked top to bottom.
    #[default]
    Normal,
    /// One at a time, the others behind tabs.
    Tabbed,
}

/// How tall a tile is within its column.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum TileHeight {
    /// A share of what the fixed tiles leave.
    Auto {
        /// Its share.
        weight: f32,
    },
    /// Points.
    Fixed(f32),
}

impl Default for TileHeight {
    fn default() -> Self {
        Self::Auto { weight: 1.0 }
    }
}

/// The layout's constants.
#[derive(Clone, PartialEq, Debug)]
pub struct LayoutConfig {
    /// Between columns, between tiles, and around them.
    pub gaps: f32,
    /// Column widths ⌘R cycles through, as proportions.
    pub presets: Vec<f32>,
    /// A new column's width, as a proportion.
    pub default_width: f32,
    /// A viewport narrower than this is a phone: new columns take the full width.
    pub phone_below: f32,
    /// A phone's struts, each side: how much of each neighbouring column shows.
    pub phone_peek: f32,
    /// Whether changes animate (off: Reduce Motion, the self-test).
    pub animate: bool,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            gaps: 8.0,
            presets: vec![1.0 / 3.0, 0.5, 2.0 / 3.0],
            default_width: 0.5,
            phone_below: 700.0,
            phone_peek: 12.0,
            animate: true,
        }
    }
}

/// Where a tile being opened comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Placement {
    /// This client opened it: a new column right of the focused one, focused.
    Local,
    /// It appeared from elsewhere: the last column of the workspace that most recently held
    /// that worker's tiles, focus untouched. A worker with no tiles yet fills an empty active
    /// workspace (focused), or else gets a new workspace above the trailing empty one.
    Remote,
}

/// A tile's place.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Pos {
    /// Workspace index.
    pub workspace: usize,
    /// Column index in the workspace.
    pub column: usize,
    /// Tile index in the column.
    pub tile: usize,
}

/// Where a dragged tile would land.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DropTarget {
    /// Into an existing column, before the tile at `index` (`index == len` is the bottom).
    IntoColumn {
        /// Workspace.
        workspace: usize,
        /// Column.
        column: usize,
        /// Position in the column.
        index: usize,
    },
    /// A new column at `index`.
    NewColumn {
        /// Workspace.
        workspace: usize,
        /// Column index it will have.
        index: usize,
    },
    /// A new workspace at `index` (overview only).
    NewWorkspace {
        /// Workspace index it will have.
        index: usize,
    },
}

/// One tile as drawn this frame.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Placed {
    /// Which.
    pub tile: TileRef,
    /// Where, in viewport coordinates: render offsets and the overview zoom applied, the open
    /// animation's scale not (apply `scale` about the centre).
    pub rect: Rect,
    /// Where it comes to rest: every slide, spring and switch landed and the overview closed
    /// (zoom 1). Size a terminal's grid from this, so a width spring does not resize its PTY
    /// every frame; clip it to `rect`.
    pub target: Rect,
    /// Its place.
    pub pos: Pos,
    /// It has the focus (the active tile of the active column of the active workspace).
    pub focused: bool,
    /// Opacity (the open animation).
    pub alpha: f32,
    /// Scale about the rect's centre (the open animation).
    pub scale: f32,
    /// Behind another tab of a tabbed column: not drawn.
    pub hidden: bool,
    /// For a tabbed column: `(active, count)`.
    pub tabs: Option<(usize, usize)>,
    /// Worth laying out: its column meets the viewport, or is next to one that does, or is
    /// the focused column.
    pub near: bool,
    /// Its column fills the viewport.
    pub fullscreen: bool,
}

/// A tile that was removed, fading out where it last stood.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Closing {
    /// Which.
    pub tile: TileRef,
    /// Where it was, in viewport coordinates.
    pub rect: Rect,
    /// Opacity, 1 → 0.
    pub alpha: f32,
    /// Scale about the centre, 1 → 0.8.
    pub scale: f32,
}

/// The active workspace's strip, for the titlebar indicator.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Strip {
    /// Each column's `(x, width)` in strip coordinates.
    pub columns: Vec<(f32, f32)>,
    /// The view's `(x, width)` in strip coordinates.
    pub view: (f32, f32),
    /// The active column.
    pub active: Option<usize>,
}

/// Everything the UI draws, at the clock.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Frame {
    /// Every tile, workspace by workspace, column by column.
    pub tiles: Vec<Placed>,
    /// Tiles fading out.
    pub closing: Vec<Closing>,
    /// Each workspace's rectangle in viewport coordinates.
    pub workspaces: Vec<(usize, Rect)>,
    /// Overview progress, 0 (closed) to 1 (open).
    pub overview: f32,
    /// The zoom the workspaces are drawn at.
    pub zoom: f32,
    /// The active workspace's strip.
    pub strip: Strip,
    /// Something is still moving: ask for another frame.
    pub animating: bool,
}

/// The saved form of a layout (`layout.json`).
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Saved {
    /// Top to bottom.
    pub workspaces: Vec<SavedWorkspace>,
    /// The active one.
    pub active: usize,
}

/// A saved workspace.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedWorkspace {
    /// Its name.
    pub name: Option<String>,
    /// Left to right.
    pub columns: Vec<SavedColumn>,
    /// The active column.
    pub active_column: usize,
    /// The view offset, relative to the active column.
    pub view_offset: f32,
}

/// A saved column.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedColumn {
    /// Top to bottom.
    pub tiles: Vec<SavedTile>,
    /// The active tile.
    pub active_tile: usize,
    /// Its width.
    pub width: ColumnWidth,
    /// Which preset that width is.
    pub preset: Option<usize>,
    /// Maximised.
    pub full_width: bool,
    /// Stacked or tabbed.
    pub mode: DisplayMode,
}

impl Default for SavedColumn {
    fn default() -> Self {
        Self {
            tiles: Vec::new(),
            active_tile: 0,
            width: ColumnWidth::Proportion(0.5),
            preset: None,
            full_width: false,
            mode: DisplayMode::Normal,
        }
    }
}

/// A saved tile.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct SavedTile {
    /// Which.
    pub tile: TileRef,
    /// Its height.
    #[serde(default)]
    pub height: TileHeight,
}

/// The viewport and the constants every computation needs.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Geom {
    view_w: f32,
    view_h: f32,
    strut: f32,
    gaps: f32,
}

impl Geom {
    const fn working_x(&self) -> f32 {
        self.strut
    }

    fn working_w(&self) -> f32 {
        2.0_f32.mul_add(-self.strut, self.view_w).max(1.0)
    }

    fn resolve(&self, width: ColumnWidth) -> f32 {
        match width {
            ColumnWidth::Proportion(p) => {
                (self.working_w() - self.gaps).mul_add(p, -self.gaps).max(1.0)
            }
            ColumnWidth::Fixed(w) => w.max(1.0),
        }
    }

    fn min_width(&self) -> f32 {
        (self.working_w() / 10.0).max(1.0)
    }

    fn max_width(&self) -> f32 {
        self.resolve(ColumnWidth::Proportion(1.0))
    }

    const fn viewport(&self) -> Rect {
        Rect { x: 0.0, y: 0.0, w: self.view_w, h: self.view_h }
    }
}

/// What every operation needs besides the workspace it works on.
#[derive(Clone, PartialEq, Debug)]
struct Ctx {
    g: Geom,
    now: Duration,
    animate: bool,
    presets: Vec<f32>,
    new_width: ColumnWidth,
}

impl Ctx {
    fn spring(&self, from: f32, to: f32, velocity: f64, params: SpringParams) -> Option<Animation> {
        self.animate
            .then(|| Animation::spring(self.now, f64::from(from), f64::from(to), velocity, params))
    }

    fn ease(&self, duration: Duration, curve: Curve) -> Option<Animation> {
        self.animate.then(|| Animation::ease(self.now, 0.0, 1.0, duration, curve))
    }
}

/// niri's `compute_new_view_offset`: the offset (relative to the column) that shows the column
/// with the least movement from `cur_x`, or keeps the view when it is already fully visible.
fn compute_new_view_offset(cur_x: f32, view_w: f32, col_x: f32, col_w: f32, gaps: f32) -> f32 {
    if view_w <= col_w {
        return 0.0;
    }
    let padding = ((view_w - col_w) / 2.0).clamp(0.0, gaps);
    let new_x = col_x - padding;
    let new_right_x = col_x + col_w + padding;
    if cur_x <= new_x && new_right_x <= cur_x + view_w {
        return -(col_x - cur_x);
    }
    let dist_to_left = (cur_x - new_x).abs();
    let dist_to_right = ((cur_x + view_w) - new_right_x).abs();
    if dist_to_left <= dist_to_right { -padding } else { -(view_w - padding - col_w) }
}

/// A tile's slide from where it was: an offset that springs from full to nothing.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Move {
    anim: Animation,
    dx: f32,
    dy: f32,
    dw: f32,
    dh: f32,
}

/// A tile in a column.
#[derive(Clone, PartialEq, Debug)]
pub struct Tile {
    key: TileRef,
    height: TileHeight,
    open: Option<Animation>,
    moving: Option<Move>,
}

impl Tile {
    const fn new(key: TileRef) -> Self {
        Self { key, height: TileHeight::Auto { weight: 1.0 }, open: None, moving: None }
    }

    /// Which.
    #[must_use]
    pub const fn tile(&self) -> TileRef {
        self.key
    }

    /// Its height rule.
    #[must_use]
    pub const fn height(&self) -> TileHeight {
        self.height
    }

    /// The slide's offset at `now`: `(dx, dy, dw, dh)`.
    fn offset(&self, now: Duration) -> (f32, f32, f32, f32) {
        self.moving.map_or((0.0, 0.0, 0.0, 0.0), |m| {
            let v = narrow(m.anim.value_at(now));
            (m.dx * v, m.dy * v, m.dw * v, m.dh * v)
        })
    }

    fn open_progress(&self, now: Duration) -> f32 {
        self.open.map_or(1.0, |a| narrow(a.value_at(now)))
    }

    fn settle(&mut self, now: Duration) {
        if self.open.is_some_and(|a| a.is_done(now)) {
            self.open = None;
        }
        if self.moving.is_some_and(|m| m.anim.is_done(now)) {
            self.moving = None;
        }
    }

    fn animating(&self, now: Duration) -> bool {
        self.open.is_some_and(|a| !a.is_done(now))
            || self.moving.is_some_and(|m| !m.anim.is_done(now))
    }
}

/// A column of tiles.
#[derive(Clone, PartialEq, Debug)]
pub struct Column {
    tiles: Vec<Tile>,
    active: usize,
    width: ColumnWidth,
    preset: Option<usize>,
    full_width: bool,
    fullscreen: bool,
    mode: DisplayMode,
}

impl Column {
    fn new(tile: Tile, width: ColumnWidth, full_width: bool) -> Self {
        Self {
            tiles: vec![tile],
            active: 0,
            width,
            preset: None,
            full_width,
            fullscreen: false,
            mode: DisplayMode::Normal,
        }
    }

    /// Its tiles, top to bottom.
    #[must_use]
    pub fn tiles(&self) -> &[Tile] {
        &self.tiles
    }

    /// The active tile's index.
    #[must_use]
    pub const fn active_tile(&self) -> usize {
        self.active
    }

    /// Its width rule (full width and fullscreen override it).
    #[must_use]
    pub const fn width(&self) -> ColumnWidth {
        self.width
    }

    /// Which preset the width is, when it is one.
    #[must_use]
    pub const fn preset(&self) -> Option<usize> {
        self.preset
    }

    /// Maximised to the working width.
    #[must_use]
    pub const fn is_full_width(&self) -> bool {
        self.full_width
    }

    /// Filling the viewport.
    #[must_use]
    pub const fn is_fullscreen(&self) -> bool {
        self.fullscreen
    }

    /// Stacked or tabbed.
    #[must_use]
    pub const fn mode(&self) -> DisplayMode {
        self.mode
    }

    fn resolved_width(&self, g: &Geom) -> f32 {
        if self.fullscreen { g.view_w } else { self.normal_width(g) }
    }

    /// The width outside fullscreen.
    fn normal_width(&self, g: &Geom) -> f32 {
        if self.full_width { g.max_width() } else { g.resolve(self.width) }
    }

    /// Each tile's rectangle in column coordinates (x = 0 at the column's left edge, y = 0 at
    /// the workspace's top).
    fn tile_rects(&self, g: &Geom) -> Vec<Rect> {
        let width = self.resolved_width(g);
        if self.fullscreen {
            return self.tiles.iter().map(|_| g.viewport()).collect();
        }
        let gaps = g.gaps;
        if self.mode == DisplayMode::Tabbed {
            let height = 2.0_f32.mul_add(-gaps, g.view_h).max(1.0);
            return self
                .tiles
                .iter()
                .map(|_| Rect { x: 0.0, y: gaps, w: width, h: height })
                .collect();
        }
        let tiles = count(self.tiles.len());
        let available = (tiles + 1.0).mul_add(-gaps, g.view_h).max(0.0);
        let fixed = self
            .tiles
            .iter()
            .filter_map(|tile| match tile.height {
                TileHeight::Fixed(height) => Some(height.max(1.0)),
                TileHeight::Auto { .. } => None,
            })
            .sum::<f32>()
            .min(available);
        let weights: f32 = self
            .tiles
            .iter()
            .filter_map(|tile| match tile.height {
                TileHeight::Auto { weight } => Some(weight.max(0.0)),
                TileHeight::Fixed(_) => None,
            })
            .sum();
        let left = (available - fixed).max(0.0);
        let mut top = gaps;
        self.tiles
            .iter()
            .map(|tile| {
                let height = match tile.height {
                    TileHeight::Fixed(height) => height.max(1.0).min(available),
                    TileHeight::Auto { weight } if weights > 0.0 => {
                        left * weight.max(0.0) / weights
                    }
                    TileHeight::Auto { .. } => left / tiles.max(1.0),
                }
                .max(1.0);
                let rect = Rect { x: 0.0, y: top, w: width, h: height };
                top += height + gaps;
                rect
            })
            .collect()
    }

    fn activate_idx(&mut self, idx: usize) -> bool {
        let idx = idx.min(self.tiles.len().saturating_sub(1));
        if self.active == idx {
            return false;
        }
        self.active = idx;
        true
    }

    fn move_up(&mut self) -> bool {
        let new = self.active.saturating_sub(1);
        if new == self.active {
            return false;
        }
        self.tiles.swap(self.active, new);
        self.active = new;
        true
    }

    fn move_down(&mut self) -> bool {
        let new = self.active.saturating_add(1);
        if new >= self.tiles.len() {
            return false;
        }
        self.tiles.swap(self.active, new);
        self.active = new;
        true
    }

    /// A tile left alone in its column takes the whole height again.
    const fn reset_single_height(&mut self) {
        if let [only] = self.tiles.as_mut_slice() {
            only.height = TileHeight::Auto { weight: 1.0 };
        }
    }
}

/// The view's offset from the active column's x.
#[derive(Clone, PartialEq, Debug)]
enum ViewOffset {
    Static(f32),
    Anim(Animation),
    Gesture(ViewGesture),
}

/// A touch or trackpad drag of the view, or a dragged tile scrolling it at the edge.
#[derive(Clone, PartialEq, Debug)]
struct ViewGesture {
    current: f32,
    tracker: SwipeTracker,
    delta_from_tracker: f32,
    stationary: f32,
    dnd: Option<DndScroll>,
}

/// Edge scrolling during a drag: when it last moved, and since when the pointer has been in
/// the band.
#[derive(Clone, Copy, PartialEq, Debug)]
struct DndScroll {
    last: Duration,
    nonzero_since: Option<Duration>,
}

impl ViewOffset {
    fn current(&self, now: Duration) -> f32 {
        match self {
            Self::Static(v) => *v,
            Self::Anim(a) => narrow(a.value_at(now)),
            Self::Gesture(g) => g.current,
        }
    }

    const fn target(&self) -> f32 {
        match self {
            Self::Static(v) => *v,
            Self::Anim(a) => narrow(a.to()),
            Self::Gesture(g) => g.current,
        }
    }

    const fn stationary(&self) -> f32 {
        match self {
            Self::Static(v) => *v,
            Self::Anim(a) => narrow(a.to()),
            Self::Gesture(g) => g.stationary,
        }
    }

    fn velocity(&self, now: Duration) -> f64 {
        match self {
            Self::Anim(a) => a.velocity_at(now),
            Self::Static(_) | Self::Gesture(_) => 0.0,
        }
    }

    fn offset(&mut self, delta: f32) {
        match self {
            Self::Static(v) => *v += delta,
            Self::Anim(a) => a.offset(f64::from(delta)),
            Self::Gesture(g) => {
                g.stationary += delta;
                g.delta_from_tracker += delta;
                g.current += delta;
            }
        }
    }

    const fn is_gesture(&self) -> bool {
        matches!(self, Self::Gesture(_))
    }

    fn settle(&mut self, now: Duration) {
        if let Self::Anim(a) = self
            && a.is_done(now)
        {
            *self = Self::Static(narrow(a.to()));
        }
    }

    fn animating(&self, now: Duration) -> bool {
        matches!(self, Self::Anim(a) if !a.is_done(now))
    }
}

/// A column being resized by dragging the gap to its right.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Resize {
    column: usize,
    original: f32,
}

/// A tile out of any column, with the width a column made for it takes (its old column's).
#[derive(Clone, PartialEq, Debug)]
struct Loose {
    tile: Tile,
    width: ColumnWidth,
    full_width: bool,
}

/// One workspace: a strip of columns and the view on it.
#[derive(Clone, PartialEq, Debug)]
pub struct Workspace {
    id: u64,
    name: Option<String>,
    columns: Vec<Column>,
    active: usize,
    view: ViewOffset,
    /// The view offset to go back to if the just-opened column is closed before the focus
    /// moves (niri's `activate_prev_column_on_removal`).
    prev_on_removal: Option<f32>,
    /// The view offset from before the active column went fullscreen.
    restore: Option<f32>,
    /// When it was last active (a counter); remote tiles join the most recent holder.
    stamp: u64,
    resize: Option<Resize>,
}

impl Workspace {
    const fn new(id: u64) -> Self {
        Self {
            id,
            name: None,
            columns: Vec::new(),
            active: 0,
            view: ViewOffset::Static(0.0),
            prev_on_removal: None,
            restore: None,
            stamp: 0,
            resize: None,
        }
    }

    /// A stable identity for the workspace's life.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// Its name, if it was given one.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Its columns, left to right.
    #[must_use]
    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    /// The active column's index.
    #[must_use]
    pub const fn active_column(&self) -> usize {
        self.active
    }

    /// No tiles.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }

    /// The workers its tiles are on.
    #[must_use]
    pub fn workers(&self) -> BTreeSet<WorkerKey> {
        self.refs().map(|t| t.worker).collect()
    }

    fn holds(&self, worker: WorkerKey) -> bool {
        self.refs().any(|t| t.worker == worker)
    }

    fn refs(&self) -> impl Iterator<Item = TileRef> + '_ {
        self.columns.iter().flat_map(|c| c.tiles.iter().map(|t| t.key))
    }

    fn find(&self, tile: TileRef) -> Option<(usize, usize)> {
        self.columns
            .iter()
            .enumerate()
            .find_map(|(c, col)| col.tiles.iter().position(|t| t.key == tile).map(|t| (c, t)))
    }

    fn column_x(&self, idx: usize, g: &Geom) -> f32 {
        self.columns.iter().take(idx).map(|c| c.resolved_width(g) + g.gaps).sum()
    }

    fn view_pos(&self, ctx: &Ctx) -> f32 {
        self.column_x(self.active, &ctx.g) + self.view.current(ctx.now)
    }

    fn target_view_pos(&self, g: &Geom) -> f32 {
        self.column_x(self.active, g) + self.view.target()
    }

    /// The offset that shows column `idx`: niri's fit, except that a lone column is centred
    /// (niri's `always-center-single-column`). Left-aligned, one column on a wide window
    /// leaves half of it empty on one side, which reads as something missing.
    fn fit_offset(&self, g: &Geom, target_x: Option<f32>, idx: usize) -> f32 {
        if self.columns.len() == 1 {
            return self.centred_offset(g, idx);
        }
        self.fit_offset_niri(g, target_x, idx)
    }

    fn fit_offset_niri(&self, g: &Geom, target_x: Option<f32>, idx: usize) -> f32 {
        let Some(col) = self.columns.get(idx) else { return 0.0 };
        if col.fullscreen {
            return 0.0;
        }
        let x = target_x.unwrap_or_else(|| self.target_view_pos(g));
        compute_new_view_offset(
            x + g.working_x(),
            g.working_w(),
            self.column_x(idx, g),
            col.resolved_width(g),
            g.gaps,
        ) - g.working_x()
    }

    fn centred_offset(&self, g: &Geom, idx: usize) -> f32 {
        let Some(col) = self.columns.get(idx) else { return 0.0 };
        let w = col.resolved_width(g);
        if col.fullscreen || g.working_w() <= w {
            return self.fit_offset_niri(g, None, idx);
        }
        -(g.working_w() - w) / 2.0 - g.working_x()
    }

    /// Re-base the view onto column `idx` and move it to `new_offset` (niri's
    /// `animate_view_offset_with_config`): nothing within a pixel of where it is going.
    fn animate_view(&mut self, ctx: &Ctx, idx: usize, new_offset: f32) {
        let delta = self.column_x(self.active, &ctx.g) - self.column_x(idx, &ctx.g);
        self.view.offset(delta);
        let to_diff = new_offset - self.view.target();
        if to_diff.abs() < 1.0 {
            self.view.offset(to_diff);
            return;
        }
        let current = self.view.current(ctx.now);
        let velocity = self.view.velocity(ctx.now);
        self.view = match ctx.spring(current, new_offset, velocity, view_spring()) {
            Some(a) => ViewOffset::Anim(a),
            None => ViewOffset::Static(new_offset),
        };
    }

    fn animate_view_to_column(&mut self, ctx: &Ctx, target_x: Option<f32>, idx: usize) {
        let offset = self.fit_offset(&ctx.g, target_x, idx);
        self.animate_view(ctx, idx, offset);
    }

    fn activate_column(&mut self, ctx: &Ctx, idx: usize) {
        if self.active == idx {
            return;
        }
        self.animate_view_to_column(ctx, None, idx);
        self.active = idx;
        self.prev_on_removal = None;
        self.restore = None;
        self.resize = None;
    }

    fn add_column(&mut self, ctx: &Ctx, idx: Option<usize>, column: Column, activate: bool) {
        let was_empty = self.columns.is_empty();
        let idx = idx
            .unwrap_or_else(|| if was_empty { 0 } else { self.active.saturating_add(1) })
            .min(self.columns.len());
        self.columns.insert(idx, column);
        if was_empty {
            self.active = 0;
            // A first column: whatever offset was left from before means nothing.
            self.view = ViewOffset::Static(0.0);
            self.view = ViewOffset::Static(self.fit_offset(&ctx.g, None, 0));
        } else if idx <= self.active {
            self.active = self.active.saturating_add(1);
        }
        if activate {
            let prev = (!was_empty && idx == self.active.saturating_add(1))
                .then(|| self.view.stationary());
            self.activate_column(ctx, idx);
            self.prev_on_removal = prev;
        }
    }

    fn add_tile(&mut self, ctx: &Ctx, idx: Option<usize>, loose: Loose, activate: bool) {
        let column = Column::new(loose.tile, loose.width, loose.full_width);
        self.add_column(ctx, idx, column, activate);
    }

    fn add_tile_to_column(
        &mut self,
        ctx: &Ctx,
        col_idx: usize,
        tile_idx: Option<usize>,
        tile: Tile,
        activate: bool,
    ) {
        let Some(column) = self.columns.get_mut(col_idx) else { return };
        let at = tile_idx.unwrap_or(column.tiles.len()).min(column.tiles.len());
        column.tiles.insert(at, tile);
        if at <= column.active && column.tiles.len() > 1 {
            column.active = column.active.saturating_add(1);
        }
        // A stacked column of several tiles is never fullscreen.
        let unfullscreen = column.fullscreen && column.mode == DisplayMode::Normal;
        if activate {
            column.active = at;
        }
        if unfullscreen {
            self.set_column_fullscreen(ctx, col_idx, false);
        }
        if activate && self.active != col_idx {
            self.activate_column(ctx, col_idx);
        }
    }

    fn remove_tile_by_idx(&mut self, ctx: &Ctx, col_idx: usize, tile_idx: usize) -> Option<Loose> {
        let len = self.columns.get(col_idx)?.tiles.len();
        if tile_idx >= len {
            return None;
        }
        if len == 1 {
            let mut column = self.remove_column_by_idx(ctx, col_idx)?;
            let tile = column.tiles.pop()?;
            return Some(Loose { tile, width: column.width, full_width: column.full_width });
        }
        let column = self.columns.get_mut(col_idx)?;
        let tile = column.tiles.remove(tile_idx);
        column.reset_single_height();
        if tile_idx < column.active {
            column.active = column.active.saturating_sub(1);
        } else if column.active >= column.tiles.len() {
            column.active = column.tiles.len().saturating_sub(1);
        }
        Some(Loose { tile, width: column.width, full_width: column.full_width })
    }

    fn remove_column_by_idx(&mut self, ctx: &Ctx, col_idx: usize) -> Option<Column> {
        if col_idx >= self.columns.len() {
            return None;
        }
        let column = self.columns.remove(col_idx);
        self.resize = None;
        if col_idx.saturating_add(1) == self.active {
            // The column that would have been returned to is itself gone.
            self.prev_on_removal = None;
        }
        if col_idx == self.active {
            self.restore = None;
        }
        let Some(last) = self.columns.len().checked_sub(1) else {
            self.active = 0;
            return Some(column);
        };
        if col_idx < self.active {
            self.active = self.active.saturating_sub(1);
            self.prev_on_removal = None;
        } else if col_idx == self.active && self.prev_on_removal.is_some() {
            if col_idx > 0 {
                let prev = self.prev_on_removal.take().unwrap_or_default();
                self.activate_column(ctx, self.active.saturating_sub(1));
                self.animate_view(ctx, self.active, prev);
                self.animate_view_to_column(ctx, None, self.active);
            }
        } else {
            self.activate_column(ctx, self.active.min(last));
        }
        if last == 0 {
            // Alone now: to the centre, wherever the view was.
            self.animate_view_to_column(ctx, None, 0);
        }
        Some(column)
    }

    fn active_column_mut(&mut self) -> Option<&mut Column> {
        self.columns.get_mut(self.active)
    }

    fn focus_left(&mut self, ctx: &Ctx) -> bool {
        if self.active == 0 || self.columns.is_empty() {
            return false;
        }
        self.activate_column(ctx, self.active.saturating_sub(1));
        true
    }

    fn focus_right(&mut self, ctx: &Ctx) -> bool {
        if self.active.saturating_add(1) >= self.columns.len() {
            return false;
        }
        self.activate_column(ctx, self.active.saturating_add(1));
        true
    }

    fn focus_column(&mut self, ctx: &Ctx, idx: usize) {
        if let Some(last) = self.columns.len().checked_sub(1) {
            self.activate_column(ctx, idx.min(last));
        }
    }

    fn focus_down(&mut self) -> bool {
        self.active_column_mut().is_some_and(|c| c.activate_idx(c.active.saturating_add(1)))
    }

    fn focus_up(&mut self) -> bool {
        self.active_column_mut().is_some_and(|c| c.activate_idx(c.active.saturating_sub(1)))
    }

    fn move_column_to(&mut self, ctx: &Ctx, new_idx: usize) {
        if new_idx >= self.columns.len() || self.active == new_idx {
            return;
        }
        let current_x = self.column_x(self.active, &ctx.g);
        let column = self.columns.remove(self.active);
        self.columns.insert(new_idx, column);
        // The camera stays where it is.
        let delta = current_x - self.column_x(self.active, &ctx.g);
        self.view.offset(delta);
        self.resize = None;
        self.activate_column(ctx, new_idx);
    }

    fn consume_or_expel_left(&mut self, ctx: &Ctx) {
        let (sc, st) = (self.active, self.columns.get(self.active).map_or(0, |c| c.active));
        let Some(len) = self.columns.get(sc).map(|c| c.tiles.len()) else { return };
        if len == 1 {
            let Some(target) = sc.checked_sub(1) else { return };
            // Closing it right after would go back to where the view is now.
            let offset = self.column_x(sc, &ctx.g) - self.column_x(target, &ctx.g);
            if self.prev_on_removal.is_none() {
                self.prev_on_removal = Some(self.view.stationary() + offset);
            }
            if let Some(removed) = self.remove_tile_by_idx(ctx, sc, 0) {
                self.add_tile_to_column(ctx, target, None, removed.tile, true);
            }
        } else if let Some(removed) = self.remove_tile_by_idx(ctx, sc, st) {
            self.add_tile(ctx, Some(sc), removed, true);
            self.prev_on_removal = None;
        }
    }

    fn consume_or_expel_right(&mut self, ctx: &Ctx) {
        let (sc, st) = (self.active, self.columns.get(self.active).map_or(0, |c| c.active));
        let Some(len) = self.columns.get(sc).map(|c| c.tiles.len()) else { return };
        if len == 1 {
            if sc.saturating_add(1) >= self.columns.len() {
                return;
            }
            self.prev_on_removal = None;
            if let Some(removed) = self.remove_tile_by_idx(ctx, sc, 0) {
                self.add_tile_to_column(ctx, sc, None, removed.tile, true);
            }
        } else if let Some(removed) = self.remove_tile_by_idx(ctx, sc, st) {
            let target = sc.saturating_add(1);
            self.add_tile(ctx, Some(target), removed, true);
        }
    }

    fn consume_into_column(&mut self, ctx: &Ctx) {
        let source = self.active.saturating_add(1);
        if self.columns.len() < 2 || source >= self.columns.len() {
            return;
        }
        if let Some(removed) = self.remove_tile_by_idx(ctx, source, 0) {
            self.add_tile_to_column(ctx, self.active, None, removed.tile, false);
        }
    }

    fn expel_from_column(&mut self, ctx: &Ctx) {
        let Some(len) = self.columns.get(self.active).map(|c| c.tiles.len()) else { return };
        if len < 2 {
            return;
        }
        if let Some(removed) = self.remove_tile_by_idx(ctx, self.active, len.saturating_sub(1)) {
            let target = self.active.saturating_add(1);
            self.add_tile(ctx, Some(target), removed, false);
        }
    }

    fn toggle_width(&mut self, ctx: &Ctx, forward: bool) {
        let g = ctx.g;
        let len = ctx.presets.len();
        let Some(col) = self.columns.get_mut(self.active) else { return };
        if len == 0 {
            return;
        }
        let preset = if col.full_width { None } else { col.preset };
        let next = match preset {
            Some(i) if forward => i.saturating_add(1).checked_rem(len).unwrap_or(0),
            Some(i) => i.saturating_add(len).saturating_sub(1).checked_rem(len).unwrap_or(0),
            None => {
                let current = col.normal_width(&g);
                let mut resolved =
                    ctx.presets.iter().map(|p| g.resolve(ColumnWidth::Proportion(*p)));
                if forward {
                    resolved.position(|r| current + 1.0 < r).unwrap_or(0)
                } else {
                    resolved
                        .rposition(|r| r + 1.0 < current)
                        .unwrap_or_else(|| len.saturating_sub(1))
                }
            }
        };
        let Some(&p) = ctx.presets.get(next) else { return };
        col.width = ColumnWidth::Proportion(p);
        col.preset = Some(next);
        col.full_width = false;
        self.resize = None;
        self.animate_view_to_column(ctx, None, self.active);
    }

    fn set_width_delta(&mut self, ctx: &Ctx, percent: f32) {
        let g = ctx.g;
        let Some(col) = self.columns.get_mut(self.active) else { return };
        let current = if col.full_width { ColumnWidth::Proportion(1.0) } else { col.width };
        let proportion = match current {
            ColumnWidth::Proportion(p) => p,
            ColumnWidth::Fixed(_) => {
                let full = g.working_w() - g.gaps;
                if full <= 0.0 { 1.0 } else { (g.resolve(current) + g.gaps) / full }
            }
        };
        let min = (g.min_width() + g.gaps) / (g.working_w() - g.gaps).max(1.0);
        let proportion = (proportion + percent / 100.0).clamp(min.min(1.0), 1.0);
        col.width = ColumnWidth::Proportion(proportion);
        col.preset = None;
        col.full_width = false;
        self.resize = None;
        self.animate_view_to_column(ctx, None, self.active);
    }

    fn toggle_full_width(&mut self, ctx: &Ctx) {
        let Some(col) = self.columns.get_mut(self.active) else { return };
        col.full_width = !col.full_width;
        self.resize = None;
        self.animate_view_to_column(ctx, None, self.active);
    }

    /// Fullscreen on or off for column `idx`: the view offset from before is kept while the
    /// active column is fullscreen and restored when it comes back.
    fn set_column_fullscreen(&mut self, ctx: &Ctx, idx: usize, on: bool) {
        let Some(col) = self.columns.get_mut(idx) else { return };
        if col.fullscreen == on {
            return;
        }
        col.fullscreen = on;
        if idx != self.active {
            return;
        }
        if on {
            self.restore = Some(self.view.stationary());
        } else if let Some(prev) = self.restore.take() {
            self.animate_view(ctx, idx, prev);
        }
        self.animate_view_to_column(ctx, None, idx);
    }

    fn toggle_fullscreen(&mut self, ctx: &Ctx) {
        let Some(col) = self.columns.get(self.active) else { return };
        let on = !col.fullscreen;
        if on && col.tiles.len() > 1 && col.mode == DisplayMode::Normal {
            // Out of its column first: fullscreen is a column's, and the others stay put.
            self.consume_or_expel_right(ctx);
        }
        self.resize = None;
        self.set_column_fullscreen(ctx, self.active, on);
    }

    fn toggle_tabbed(&mut self, ctx: &Ctx) {
        let Some(col) = self.columns.get_mut(self.active) else { return };
        col.mode = match col.mode {
            DisplayMode::Normal => DisplayMode::Tabbed,
            DisplayMode::Tabbed => DisplayMode::Normal,
        };
        let unfullscreen = col.mode == DisplayMode::Normal && col.tiles.len() > 1 && col.fullscreen;
        if unfullscreen {
            self.set_column_fullscreen(ctx, self.active, false);
        }
    }

    fn center_column(&mut self, ctx: &Ctx) {
        if self.columns.is_empty() {
            return;
        }
        let offset = self.centred_offset(&ctx.g, self.active);
        self.animate_view(ctx, self.active, offset);
        self.resize = None;
    }

    /// The columns fully inside the working area as the view will be: `(width taken with
    /// gaps, leftmost x, the active column's x, whether another column counted)`.
    fn fully_visible(&self, g: &Geom) -> (f32, Option<f32>, Option<f32>, bool) {
        let view_x = self.target_view_pos(g);
        let (wx, ww, gap) = (g.working_x(), g.working_w(), g.gaps);
        let mut taken = 0.0;
        let mut leftmost = None;
        let mut active_x = None;
        let mut other = false;
        let mut x = 0.0;
        for (idx, col) in self.columns.iter().enumerate() {
            let w = col.resolved_width(g);
            let col_x = x;
            x += w + gap;
            if col_x < view_x + wx + gap {
                continue;
            }
            leftmost.get_or_insert(col_x);
            if view_x + wx + ww < col_x + w + gap {
                break;
            }
            if idx == self.active {
                active_x = Some(col_x);
            } else {
                other = true;
            }
            taken += w + gap;
        }
        (taken, leftmost, active_x, other)
    }

    fn center_visible_columns(&mut self, ctx: &Ctx) {
        let g = ctx.g;
        let (taken, leftmost, active_x, _) = self.fully_visible(&g);
        let (Some(leftmost), Some(active_x)) = (leftmost, active_x) else { return };
        let free = g.working_w() - taken + g.gaps;
        let new_view_x = leftmost - free / 2.0 - g.working_x();
        self.resize = None;
        self.animate_view(ctx, self.active, new_view_x - active_x);
        self.animate_view_to_column(ctx, None, self.active);
    }

    fn expand_to_available_width(&mut self, ctx: &Ctx) {
        let g = ctx.g;
        let Some(col) = self.columns.get(self.active) else { return };
        if col.fullscreen || col.full_width {
            return;
        }
        let (taken, leftmost, active_x, other) = self.fully_visible(&g);
        let (Some(leftmost), Some(active_x)) = (leftmost, active_x) else { return };
        let available = g.working_w() - g.gaps - taken;
        if available <= 0.0 {
            return;
        }
        self.resize = None;
        let active_w = self.columns.get(self.active).map_or(0.0, |c| c.resolved_width(&g));
        let Some(col) = self.columns.get_mut(self.active) else { return };
        if !other {
            col.full_width = true;
            self.animate_view_to_column(ctx, None, self.active);
            return;
        }
        col.width = ColumnWidth::Fixed(active_w + available);
        col.preset = None;
        col.full_width = false;
        let new_view_x = leftmost - g.gaps - g.working_x();
        self.animate_view(ctx, self.active, new_view_x - active_x);
        self.animate_view_to_column(ctx, None, self.active);
    }

    fn gesture_begin(&mut self, ctx: &Ctx) {
        if self.columns.is_empty() || self.resize.is_some() {
            return;
        }
        let current = self.view.current(ctx.now);
        self.view = ViewOffset::Gesture(ViewGesture {
            current,
            tracker: SwipeTracker::new(),
            delta_from_tracker: current,
            stationary: self.view.stationary(),
            dnd: None,
        });
    }

    fn gesture_update(&mut self, dx: f32, at: Duration) -> bool {
        let ViewOffset::Gesture(g) = &mut self.view else { return false };
        if g.dnd.is_some() {
            return false;
        }
        g.tracker.push(f64::from(dx), at);
        g.current = narrow(g.tracker.pos()) + g.delta_from_tracker;
        true
    }

    /// Snap as niri's `view_offset_gesture_end`: the snap point nearest where the fling would
    /// stop, the furthest fully visible column that way focused, a spring at the fling's speed.
    fn gesture_end(&mut self, ctx: &Ctx, cancelled: bool) -> bool {
        let ViewOffset::Gesture(gesture) = &mut self.view else { return false };
        gesture.tracker.push(0.0, ctx.now);
        let velocity = gesture.tracker.velocity();
        let current = narrow(gesture.tracker.pos()) + gesture.delta_from_tracker;
        let target_offset =
            narrow(gesture.tracker.projected_end_pos()) + gesture.delta_from_tracker;
        if self.columns.is_empty() {
            self.view = ViewOffset::Static(current);
            return true;
        }
        if cancelled {
            self.view = ViewOffset::Static(current);
            self.animate_view_to_column(ctx, None, self.active);
            return true;
        }
        let g = ctx.g;
        let gaps = g.gaps;
        let (left_strut, right_strut) = (g.working_x(), g.view_w - g.working_w() - g.working_x());
        let snaps_of = |col_x: f32, col: &Column| {
            let w = col.resolved_width(&g);
            if col.fullscreen {
                (col_x, col_x + w)
            } else {
                let padding = ((g.working_w() - w) / 2.0).clamp(0.0, gaps);
                (col_x - padding - left_strut, col_x + w + padding + right_strut)
            }
        };
        let mut xs = Vec::with_capacity(self.columns.len());
        let mut x = 0.0;
        for col in &self.columns {
            xs.push(x);
            x += col.resolved_width(&g) + gaps;
        }
        let (Some(first), Some(last), Some(&last_x)) =
            (self.columns.first(), self.columns.last(), xs.last())
        else {
            return true;
        };
        let leftmost = snaps_of(0.0, first).0;
        let rightmost = snaps_of(last_x, last).1 - g.view_w;
        let last_idx = self.columns.len().saturating_sub(1);
        let mut snaps: Vec<(f32, usize)> = vec![(leftmost, 0), (rightmost, last_idx)];
        for (idx, (col, &col_x)) in self.columns.iter().zip(&xs).enumerate() {
            let (left, right) = snaps_of(col_x, col);
            if leftmost < left && left < rightmost {
                snaps.push((left, idx));
            }
            let right = right - g.view_w;
            if leftmost < right && right < rightmost {
                snaps.push((right, idx));
            }
        }
        let active_x = self.column_x(self.active, &g);
        let target_pos = active_x + target_offset;
        let Some(&(snap_pos, snap_col)) = snaps
            .iter()
            .min_by(|a, b| (a.0 - target_pos).abs().total_cmp(&(b.0 - target_pos).abs()))
        else {
            return true;
        };
        let mut new_col = snap_col;
        if target_offset >= current {
            for (idx, (col, &col_x)) in
                self.columns.iter().zip(&xs).enumerate().skip(new_col.saturating_add(1))
            {
                let w = col.resolved_width(&g);
                if col.fullscreen {
                    if snap_pos + g.view_w < col_x + w {
                        break;
                    }
                } else {
                    let padding = ((g.working_w() - w) / 2.0).clamp(0.0, gaps);
                    if snap_pos + left_strut + g.working_w() < col_x + w + padding {
                        break;
                    }
                }
                new_col = idx;
            }
        } else {
            for (idx, (col, &col_x)) in self.columns.iter().zip(&xs).enumerate().take(new_col).rev()
            {
                let w = col.resolved_width(&g);
                if col.fullscreen {
                    if col_x < snap_pos {
                        break;
                    }
                } else {
                    let padding = ((g.working_w() - w) / 2.0).clamp(0.0, gaps);
                    if col_x - padding < snap_pos + left_strut {
                        break;
                    }
                }
                new_col = idx;
            }
        }
        let new_col_x = xs.get(new_col).copied().unwrap_or(0.0);
        let delta = active_x - new_col_x;
        if self.active != new_col {
            self.restore = None;
            self.prev_on_removal = None;
        }
        self.active = new_col;
        let target = snap_pos - new_col_x;
        self.view = match ctx.spring(current + delta, target, velocity, view_spring()) {
            Some(a) => ViewOffset::Anim(a),
            None => ViewOffset::Static(target),
        };
        // Snapping to the right edge of a column wider than the view still shows its left.
        self.animate_view_to_column(ctx, None, new_col);
        true
    }

    /// niri's `dnd_scroll_gesture_{begin,scroll}`: the pointer's depth into the band at either
    /// edge sets the speed; the time since the last call sets the distance.
    fn dnd_scroll(&mut self, ctx: &Ctx, pointer_x: f32) -> bool {
        let g = ctx.g;
        let is_dnd = matches!(&self.view, ViewOffset::Gesture(ViewGesture { dnd: Some(_), .. }));
        if !is_dnd {
            self.resize = None;
            let current = self.view.current(ctx.now);
            self.view = ViewOffset::Gesture(ViewGesture {
                current,
                tracker: SwipeTracker::new(),
                delta_from_tracker: current,
                stationary: self.view.stationary(),
                dnd: Some(DndScroll { last: ctx.now, nonzero_since: None }),
            });
        }
        let width = g.working_w();
        let inside = (pointer_x - g.working_x()).clamp(0.0, width);
        let trigger = DND_TRIGGER.clamp(0.0, width / 2.0);
        let depth = if trigger < 0.01 {
            0.0
        } else if inside < trigger {
            -(trigger - inside) / trigger
        } else if width - inside < trigger {
            (trigger - (width - inside)) / trigger
        } else {
            0.0
        };
        let (leftmost, rightmost) = if self.columns.is_empty() {
            (0.0, 0.0)
        } else {
            let active_x = self.column_x(self.active, &g);
            let last = self.columns.len().saturating_sub(1);
            let last_w = self.columns.last().map_or(0.0, |c| c.resolved_width(&g));
            let right = self.column_x(last, &g) + last_w - g.working_x();
            (-width - active_x, right - active_x)
        };
        let ViewOffset::Gesture(gesture) = &mut self.view else { return false };
        let Some(dnd) = &mut gesture.dnd else { return false };
        let last = dnd.last;
        dnd.last = ctx.now;
        if depth == 0.0 {
            dnd.nonzero_since = None;
            return false;
        }
        let since = *dnd.nonzero_since.get_or_insert(ctx.now);
        if ctx.now.saturating_sub(since) < DND_DELAY {
            return true;
        }
        let moved = depth * narrow(ctx.now.saturating_sub(last).as_secs_f64()) * DND_SPEED;
        gesture.tracker.push(f64::from(moved), ctx.now);
        let offset = narrow(gesture.tracker.pos()) + gesture.delta_from_tracker;
        let clamped = offset.clamp(leftmost, rightmost.max(leftmost));
        // Keep the tracker's position in step with the clamp, as niri does.
        gesture.delta_from_tracker += clamped - offset;
        gesture.current = clamped;
        true
    }

    /// niri's `dnd_scroll_gesture_end`: a scroll snaps like a released swipe; no scroll keeps
    /// the view where it is.
    fn dnd_scroll_end(&mut self, ctx: &Ctx) {
        let ViewOffset::Gesture(gesture) = &self.view else { return };
        if gesture.dnd.is_none() {
            return;
        }
        if gesture.tracker.pos() == 0.0 {
            self.view = ViewOffset::Static(gesture.delta_from_tracker);
            if !self.columns.is_empty() {
                self.animate_view_to_column(ctx, None, self.active);
            }
            return;
        }
        self.gesture_end(ctx, false);
    }

    fn resize_begin(&mut self, ctx: &Ctx, column: usize) -> bool {
        let Some(col) = self.columns.get(column) else { return false };
        if col.fullscreen || self.resize.is_some() {
            return false;
        }
        self.resize = Some(Resize { column, original: col.resolved_width(&ctx.g) });
        self.view = ViewOffset::Static(self.view.current(ctx.now));
        true
    }

    fn resize_update(&mut self, ctx: &Ctx, dx: f32) -> bool {
        let Some(resize) = self.resize else { return false };
        let g = ctx.g;
        let Some(col) = self.columns.get_mut(resize.column) else { return false };
        let old = col.resolved_width(&g);
        let new = (resize.original + dx).clamp(g.min_width(), g.max_width().max(g.min_width()));
        col.width = ColumnWidth::Fixed(new);
        col.preset = None;
        col.full_width = false;
        // Keep the camera still: a column left of the active one would drag the view with it.
        if resize.column < self.active {
            self.view.offset(-(new - old));
        }
        true
    }

    fn resize_end(&mut self, ctx: &Ctx) {
        if self.resize.take().is_some() && !self.columns.is_empty() {
            self.animate_view_to_column(ctx, None, self.active);
        }
    }

    /// Every tile's rectangle in view coordinates as laid out, no animation.
    fn target_rects(&self, ctx: &Ctx) -> Vec<(TileRef, Rect)> {
        self.rects_at(&ctx.g, self.view_pos(ctx))
    }

    /// Every tile's rectangle in view coordinates once the view has landed, no animation.
    fn rest_rects(&self, g: &Geom) -> Vec<(TileRef, Rect)> {
        self.rects_at(g, self.target_view_pos(g))
    }

    fn rects_at(&self, g: &Geom, view_pos: f32) -> Vec<(TileRef, Rect)> {
        let mut out = Vec::new();
        let mut x = 0.0;
        for col in &self.columns {
            for (tile, r) in col.tiles.iter().zip(col.tile_rects(g)) {
                out.push((tile.key, Rect { x: r.x + x - view_pos, ..r }));
            }
            x += col.resolved_width(g) + g.gaps;
        }
        out
    }

    /// Every tile's rectangle in view coordinates as drawn now, slides included.
    fn view_rects(&self, ctx: &Ctx) -> Vec<(TileRef, Rect)> {
        let tiles = self.columns.iter().flat_map(|c| c.tiles.iter());
        self.target_rects(ctx)
            .into_iter()
            .zip(tiles)
            .map(|((key, r), tile)| {
                let (dx, dy, dw, dh) = tile.offset(ctx.now);
                (key, Rect { x: r.x + dx, y: r.y + dy, w: r.w + dw, h: r.h + dh })
            })
            .collect()
    }

    /// After a change: every tile that moved slides from where it was drawn (`before`) to its
    /// new place; a size change of at most [`RESIZE_THRESHOLD`] lands at once.
    fn flip(&mut self, ctx: &Ctx, before: &[(TileRef, Rect)]) {
        let targets = self.target_rects(ctx);
        let tiles = self.columns.iter_mut().flat_map(|c| c.tiles.iter_mut());
        for ((key, target), tile) in targets.into_iter().zip(tiles) {
            let Some(&(_, was)) = before.iter().find(|(k, _)| *k == key) else { continue };
            if !ctx.animate {
                tile.moving = None;
                continue;
            }
            let (ox, oy, ow, oh) = tile.offset(ctx.now);
            let now =
                Rect { x: target.x + ox, y: target.y + oy, w: target.w + ow, h: target.h + oh };
            let unchanged = (was.x - now.x).abs() < 0.5
                && (was.y - now.y).abs() < 0.5
                && (was.w - now.w).abs() < 0.5
                && (was.h - now.h).abs() < 0.5;
            if unchanged {
                continue;
            }
            let size = |d: f32| if d.abs() <= RESIZE_THRESHOLD { 0.0 } else { d };
            let (dx, dy, dw, dh) = (
                was.x - target.x,
                was.y - target.y,
                size(was.w - target.w),
                size(was.h - target.h),
            );
            if dx.abs() < 0.5 && dy.abs() < 0.5 && dw == 0.0 && dh == 0.0 {
                tile.moving = None;
                continue;
            }
            tile.moving =
                ctx.spring(1.0, 0.0, 0.0, view_spring()).map(|anim| Move { anim, dx, dy, dw, dh });
        }
    }

    fn settle(&mut self, now: Duration) {
        self.view.settle(now);
        for tile in self.columns.iter_mut().flat_map(|c| c.tiles.iter_mut()) {
            tile.settle(now);
        }
    }

    fn animating(&self, now: Duration) -> bool {
        self.view.animating(now)
            || self.columns.iter().flat_map(|c| &c.tiles).any(|t| t.animating(now))
    }
}

/// A workspace switch in progress.
#[derive(Clone, PartialEq, Debug)]
enum Switch {
    Anim(Animation),
    Gesture(SwitchGesture),
}

#[derive(Clone, PartialEq, Debug)]
struct SwitchGesture {
    center: usize,
    start: f32,
    current: f32,
    tracker: SwipeTracker,
    clamped: bool,
}

impl Switch {
    fn current(&self, now: Duration) -> f32 {
        match self {
            Self::Anim(a) => narrow(a.value_at(now)),
            Self::Gesture(g) => g.current,
        }
    }

    fn shift(&mut self, delta: f32) {
        match self {
            Self::Anim(a) => a.offset(f64::from(delta)),
            Self::Gesture(g) => {
                g.start += delta;
                g.current += delta;
                if delta > 0.0 {
                    g.center = g.center.saturating_add(1);
                } else {
                    g.center = g.center.saturating_sub(1);
                }
            }
        }
    }

    const fn target(&self) -> f32 {
        match self {
            Self::Anim(a) => narrow(a.to()),
            Self::Gesture(g) => g.current,
        }
    }
}

/// A removed tile fading out.
#[derive(Clone, Copy, PartialEq, Debug)]
struct ClosingTile {
    tile: TileRef,
    rect: Rect,
    anim: Animation,
}

/// The whole layout: workspaces top to bottom, the active one on show.
#[derive(Clone, PartialEq, Debug)]
pub struct Layout {
    config: LayoutConfig,
    view_w: f32,
    view_h: f32,
    now: Duration,
    workspaces: Vec<Workspace>,
    active: usize,
    previous: Option<u64>,
    switch: Option<Switch>,
    overview_open: bool,
    overview: Option<Animation>,
    closing: Vec<ClosingTile>,
    next_id: u64,
    stamps: u64,
    wheel_x: WheelTracker,
    wheel_y: WheelTracker,
    wheel_switched: Option<Duration>,
}

impl Layout {
    /// One empty workspace.
    #[must_use]
    pub fn new(config: LayoutConfig) -> Self {
        let mut first = Workspace::new(1);
        first.stamp = 1;
        Self {
            config,
            view_w: 1280.0,
            view_h: 800.0,
            now: Duration::ZERO,
            workspaces: vec![first],
            active: 0,
            previous: None,
            switch: None,
            overview_open: false,
            overview: None,
            closing: Vec::new(),
            next_id: 2,
            stamps: 1,
            wheel_x: WheelTracker::new(WHEEL_TICK),
            wheel_y: WheelTracker::new(WHEEL_TICK),
            wheel_switched: None,
        }
    }

    /// The constants.
    #[must_use]
    pub const fn config(&self) -> &LayoutConfig {
        &self.config
    }

    /// The workspace area, in points.
    pub fn set_viewport(&mut self, w: f32, h: f32) {
        let (w, h) = (w.max(1.0), h.max(1.0));
        if (w - self.view_w).abs() < f32::EPSILON && (h - self.view_h).abs() < f32::EPSILON {
            return;
        }
        self.view_w = w;
        self.view_h = h;
        // Keep each active column in view at the new size.
        let ctx = self.ctx();
        for ws in &mut self.workspaces {
            if !ws.columns.is_empty() && matches!(ws.view, ViewOffset::Static(_)) {
                ws.view = ViewOffset::Static(ws.fit_offset(&ctx.g, None, ws.active));
            }
        }
    }

    /// The workspace area.
    #[must_use]
    pub const fn viewport(&self) -> (f32, f32) {
        (self.view_w, self.view_h)
    }

    /// A phone-sized viewport: new columns take the full width and neighbours peek.
    #[must_use]
    pub fn is_phone(&self) -> bool {
        self.view_w < self.config.phone_below
    }

    /// The time, on the caller's monotonic clock. Call before every [`Self::frame`] and before
    /// actions; landed animations are dropped here.
    pub fn set_clock(&mut self, now: Duration) {
        self.now = self.now.max(now);
        let now = self.now;
        if let Some(Switch::Anim(a)) = &self.switch
            && a.is_done(now)
        {
            self.switch = None;
            self.clean_up();
        }
        if self.overview.is_some_and(|a| a.is_done(now)) {
            self.overview = None;
        }
        for ws in &mut self.workspaces {
            ws.settle(now);
        }
        self.closing.retain(|c| !c.anim.is_done(now));
    }

    /// Whether changes animate. Off lands everything in progress at once.
    pub fn set_animate(&mut self, on: bool) {
        self.config.animate = on;
        if on {
            return;
        }
        if matches!(self.switch, Some(Switch::Anim(_))) {
            self.switch = None;
            self.clean_up();
        }
        self.overview = None;
        self.closing.clear();
        for ws in &mut self.workspaces {
            if let ViewOffset::Anim(a) = &ws.view {
                ws.view = ViewOffset::Static(narrow(a.to()));
            }
            for tile in ws.columns.iter_mut().flat_map(|c| c.tiles.iter_mut()) {
                tile.open = None;
                tile.moving = None;
            }
        }
    }

    /// Something is still moving at the clock.
    #[must_use]
    pub fn is_animating(&self) -> bool {
        let now = self.now;
        matches!(&self.switch, Some(Switch::Anim(a)) if !a.is_done(now))
            || self.overview.is_some_and(|a| !a.is_done(now))
            || self.closing.iter().any(|c| !c.anim.is_done(now))
            || self.workspaces.iter().any(|ws| ws.animating(now))
    }

    fn geom(&self) -> Geom {
        let strut = if self.is_phone() { self.config.phone_peek.max(0.0) } else { 0.0 };
        Geom { view_w: self.view_w, view_h: self.view_h, strut, gaps: self.config.gaps.max(0.0) }
    }

    fn ctx(&self) -> Ctx {
        let new_width = if self.is_phone() {
            ColumnWidth::Proportion(1.0)
        } else {
            ColumnWidth::Proportion(self.config.default_width)
        };
        Ctx {
            g: self.geom(),
            now: self.now,
            animate: self.config.animate,
            presets: self.config.presets.clone(),
            new_width,
        }
    }

    // ----- queries ---------------------------------------------------------------------------

    /// Every workspace, top to bottom.
    #[must_use]
    pub fn workspaces(&self) -> &[Workspace] {
        &self.workspaces
    }

    /// The active workspace's index.
    #[must_use]
    pub const fn active_workspace(&self) -> usize {
        self.active
    }

    /// The overview is open (or opening).
    #[must_use]
    pub const fn overview_open(&self) -> bool {
        self.overview_open
    }

    /// The focused tile: the active one of the active column of the active workspace.
    #[must_use]
    pub fn focused(&self) -> Option<TileRef> {
        let ws = self.workspaces.get(self.active)?;
        let col = ws.columns.get(ws.active)?;
        col.tiles.get(col.active).map(|t| t.key)
    }

    /// Whether `tile` is anywhere in the layout.
    #[must_use]
    pub fn contains(&self, tile: TileRef) -> bool {
        self.position(tile).is_some()
    }

    /// Every tile, workspace by workspace.
    pub fn tiles(&self) -> impl Iterator<Item = TileRef> + '_ {
        self.workspaces.iter().flat_map(Workspace::refs)
    }

    /// Where `tile` is.
    #[must_use]
    pub fn position(&self, tile: TileRef) -> Option<Pos> {
        self.workspaces.iter().enumerate().find_map(|(workspace, ws)| {
            ws.find(tile).map(|(column, tile)| Pos { workspace, column, tile })
        })
    }

    // ----- workspaces ------------------------------------------------------------------------

    fn stamp(&mut self, idx: usize) {
        self.stamps = self.stamps.saturating_add(1);
        let stamp = self.stamps;
        if let Some(ws) = self.workspaces.get_mut(idx) {
            ws.stamp = stamp;
        }
    }

    const fn new_workspace(&mut self) -> Workspace {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        Workspace::new(id)
    }

    fn add_workspace_at(&mut self, idx: usize) {
        let idx = idx.min(self.workspaces.len());
        let ws = self.new_workspace();
        self.workspaces.insert(idx, ws);
        if idx <= self.active && self.workspaces.len() > 1 {
            self.active = self.active.saturating_add(1);
        }
        if let Some(switch) = &mut self.switch
            && count(idx) <= switch.target()
        {
            switch.shift(1.0);
        }
    }

    /// One empty, unnamed workspace at the end, always.
    fn ensure_trailing(&mut self) {
        let needs = self.workspaces.last().is_none_or(|ws| !ws.is_empty() || ws.name.is_some());
        if needs {
            let ws = self.new_workspace();
            self.workspaces.push(ws);
        }
    }

    /// Drop the empty, unnamed workspaces other than the active one and the last (never while
    /// a switch is moving: the indices it animates between would shift).
    fn clean_up(&mut self) {
        if self.switch.is_some() {
            return;
        }
        let last = self.workspaces.len().saturating_sub(1);
        for idx in (0..last).rev() {
            if idx == self.active {
                continue;
            }
            let empty =
                self.workspaces.get(idx).is_some_and(|ws| ws.is_empty() && ws.name.is_none());
            if empty {
                self.workspaces.remove(idx);
                if self.active > idx {
                    self.active = self.active.saturating_sub(1);
                }
            }
        }
        self.ensure_trailing();
    }

    fn render_idx(&self) -> f32 {
        self.switch.as_ref().map_or_else(|| count(self.active), |s| s.current(self.now))
    }

    fn activate_workspace(&mut self, idx: usize) {
        let idx = idx.min(self.workspaces.len().saturating_sub(1));
        let current = self.render_idx();
        let velocity = match &self.switch {
            Some(Switch::Anim(a)) => a.velocity_at(self.now),
            _ => 0.0,
        };
        let prev = self.active;
        if prev != idx {
            self.previous = self.workspaces.get(prev).map(Workspace::id);
        }
        self.active = idx;
        self.stamp(idx);
        if prev == idx && !matches!(self.switch, Some(Switch::Gesture(_))) {
            return;
        }
        let ctx = self.ctx();
        self.switch = ctx.spring(current, count(idx), velocity, switch_spring()).map(Switch::Anim);
        if self.switch.is_none() {
            self.clean_up();
        }
    }

    /// Give workspace `ws` a name (blank clears it). A named workspace stays when empty.
    pub fn set_workspace_name(&mut self, ws: usize, name: Option<String>) {
        if let Some(workspace) = self.workspaces.get_mut(ws) {
            workspace.name = name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
        }
        self.ensure_trailing();
        self.clean_up();
    }

    // ----- placement -------------------------------------------------------------------------

    /// Put `tile` in the layout (nothing when it is there already). See [`Placement`].
    pub fn open(&mut self, tile: TileRef, placement: Placement) {
        if self.contains(tile) {
            return;
        }
        let ctx = self.ctx();
        let mut new = Tile::new(tile);
        new.open = ctx.ease(OPEN_FOR, Curve::EaseOutExpo);
        let active_empty = self.workspaces.get(self.active).is_none_or(Workspace::is_empty);
        let (ws_idx, activate_tile, activate_ws) = match placement {
            Placement::Local => (self.active, true, true),
            Placement::Remote => {
                let holder = self
                    .workspaces
                    .iter()
                    .enumerate()
                    .filter(|(_, ws)| ws.holds(tile.worker))
                    .max_by_key(|(_, ws)| ws.stamp)
                    .map(|(i, _)| i);
                match holder {
                    Some(i) => (i, false, false),
                    None if active_empty => (self.active, true, true),
                    None => {
                        let at = self.workspaces.len().saturating_sub(1);
                        self.add_workspace_at(at);
                        (at, false, false)
                    }
                }
            }
        };
        let Some(ws) = self.workspaces.get_mut(ws_idx) else { return };
        let before = ws.view_rects(&ctx);
        let at = (!activate_tile).then_some(ws.columns.len());
        let loose = Loose { tile: new, width: ctx.new_width, full_width: false };
        ws.add_tile(&ctx, at, loose, activate_tile);
        ws.flip(&ctx, &before);
        self.ensure_trailing();
        if activate_ws {
            self.activate_workspace(ws_idx);
        }
    }

    /// Take `tile` out: it fades where it stood and its neighbours close the gap.
    pub fn remove(&mut self, tile: TileRef) {
        let Some(pos) = self.position(tile) else { return };
        let ctx = self.ctx();
        if let Some(rect) = self.screen_rect(pos)
            && let Some(anim) = ctx.ease(CLOSE_FOR, Curve::EaseOutQuad)
        {
            self.closing.push(ClosingTile { tile, rect, anim });
        }
        if let Some(ws) = self.workspaces.get_mut(pos.workspace) {
            let before = ws.view_rects(&ctx);
            ws.remove_tile_by_idx(&ctx, pos.column, pos.tile);
            ws.flip(&ctx, &before);
        }
        self.clean_up();
    }

    /// Remove `worker`'s tiles whose item fails `keep` (a snapshot after a reconnect).
    pub fn retain_worker(&mut self, worker: WorkerKey, keep: impl Fn(ItemId) -> bool) {
        let gone: Vec<TileRef> =
            self.tiles().filter(|t| t.worker == worker && !keep(t.item)).collect();
        for tile in gone {
            self.remove(tile);
        }
    }

    /// Focus `tile`: its workspace, column and place in the column, the view following.
    pub fn focus(&mut self, tile: TileRef) {
        let Some(pos) = self.position(tile) else { return };
        let ctx = self.ctx();
        if let Some(ws) = self.workspaces.get_mut(pos.workspace) {
            if let Some(col) = ws.columns.get_mut(pos.column) {
                col.activate_idx(pos.tile);
            }
            ws.activate_column(&ctx, pos.column);
        }
        self.activate_workspace(pos.workspace);
    }

    // ----- actions in the active workspace ---------------------------------------------------

    fn in_active<R>(&mut self, f: impl FnOnce(&mut Workspace, &Ctx) -> R) -> Option<R> {
        let ctx = self.ctx();
        let ws = self.workspaces.get_mut(self.active)?;
        let before = ws.view_rects(&ctx);
        let out = f(ws, &ctx);
        ws.flip(&ctx, &before);
        Some(out)
    }

    /// Focus the column to the left (nothing at the left end).
    pub fn focus_column_left(&mut self) {
        self.in_active(Workspace::focus_left);
    }

    /// Focus the column to the right (nothing at the right end).
    pub fn focus_column_right(&mut self) {
        self.in_active(Workspace::focus_right);
    }

    /// Focus the first column.
    pub fn focus_column_first(&mut self) {
        self.in_active(|ws, ctx| ws.focus_column(ctx, 0));
    }

    /// Focus the last column.
    pub fn focus_column_last(&mut self) {
        self.in_active(|ws, ctx| ws.focus_column(ctx, usize::MAX));
    }

    /// Focus column `n` (0-based, clamped to the last): ⌘1…⌘9.
    pub fn focus_column(&mut self, n: usize) {
        self.in_active(|ws, ctx| ws.focus_column(ctx, n));
    }

    /// The titlebar indicator's click: [`Self::focus_column`].
    pub fn strip_jump(&mut self, column: usize) {
        self.focus_column(column);
    }

    /// Focus the tile above in the column.
    pub fn focus_window_up(&mut self) {
        self.in_active(|ws, _| ws.focus_up());
    }

    /// Focus the tile below in the column.
    pub fn focus_window_down(&mut self) {
        self.in_active(|ws, _| ws.focus_down());
    }

    /// Focus the tile above, or the workspace above from the top tile.
    pub fn focus_window_or_workspace_up(&mut self) {
        if !self.in_active(|ws, _| ws.focus_up()).unwrap_or(false) {
            self.focus_workspace_up();
        }
    }

    /// Focus the tile below, or the workspace below from the bottom tile.
    pub fn focus_window_or_workspace_down(&mut self) {
        if !self.in_active(|ws, _| ws.focus_down()).unwrap_or(false) {
            self.focus_workspace_down();
        }
    }

    /// Switch to the workspace above.
    pub fn focus_workspace_up(&mut self) {
        self.activate_workspace(self.active.saturating_sub(1));
    }

    /// Switch to the workspace below.
    pub fn focus_workspace_down(&mut self) {
        self.activate_workspace(self.active.saturating_add(1));
    }

    /// Switch to workspace `n` (0-based, clamped).
    pub fn focus_workspace(&mut self, n: usize) {
        self.activate_workspace(n);
    }

    /// Switch back to the workspace active before this one.
    pub fn focus_workspace_previous(&mut self) {
        let idx = self.previous.and_then(|id| self.workspaces.iter().position(|w| w.id == id));
        if let Some(idx) = idx {
            self.activate_workspace(idx);
        }
    }

    /// Swap the active column with its left neighbour; the camera stays.
    pub fn move_column_left(&mut self) {
        self.in_active(|ws, ctx| {
            if let Some(to) = ws.active.checked_sub(1) {
                ws.move_column_to(ctx, to);
            }
        });
    }

    /// Swap the active column with its right neighbour; the camera stays.
    pub fn move_column_right(&mut self) {
        self.in_active(|ws, ctx| ws.move_column_to(ctx, ws.active.saturating_add(1)));
    }

    /// Move the active column to the start.
    pub fn move_column_to_first(&mut self) {
        self.in_active(|ws, ctx| ws.move_column_to(ctx, 0));
    }

    /// Move the active column to the end.
    pub fn move_column_to_last(&mut self) {
        self.in_active(|ws, ctx| ws.move_column_to(ctx, ws.columns.len().saturating_sub(1)));
    }

    /// Swap the active tile with the one above.
    pub fn move_window_up(&mut self) {
        self.in_active(|ws, _| ws.active_column_mut().is_some_and(Column::move_up));
    }

    /// Swap the active tile with the one below.
    pub fn move_window_down(&mut self) {
        self.in_active(|ws, _| ws.active_column_mut().is_some_and(Column::move_down));
    }

    /// Move the active tile up in its column, or to the workspace above from the top.
    pub fn move_window_up_or_to_workspace_up(&mut self) {
        if !self
            .in_active(|ws, _| ws.active_column_mut().is_some_and(Column::move_up))
            .unwrap_or(false)
        {
            self.move_tile_to_workspace(false);
        }
    }

    /// Move the active tile down in its column, or to the workspace below from the bottom.
    pub fn move_window_down_or_to_workspace_down(&mut self) {
        if !self
            .in_active(|ws, _| ws.active_column_mut().is_some_and(Column::move_down))
            .unwrap_or(false)
        {
            self.move_tile_to_workspace(true);
        }
    }

    fn neighbour_workspace(&self, down: bool) -> Option<usize> {
        let new = if down {
            self.active.saturating_add(1).min(self.workspaces.len().saturating_sub(1))
        } else {
            self.active.saturating_sub(1)
        };
        (new != self.active).then_some(new)
    }

    /// The active tile into its own column in the workspace above or below, focused there.
    fn move_tile_to_workspace(&mut self, down: bool) {
        let Some(target) = self.neighbour_workspace(down) else { return };
        let ctx = self.ctx();
        let Some(removed) = self.in_active(|ws, ctx| {
            let (c, t) = (ws.active, ws.columns.get(ws.active)?.active);
            ws.remove_tile_by_idx(ctx, c, t)
        }) else {
            return;
        };
        let Some(removed) = removed else { return };
        if let Some(ws) = self.workspaces.get_mut(target) {
            let before = ws.view_rects(&ctx);
            ws.add_tile(&ctx, None, removed, true);
            ws.flip(&ctx, &before);
        }
        self.ensure_trailing();
        self.activate_workspace(target);
    }

    /// Move the active column to the workspace above, focused there.
    pub fn move_column_to_workspace_up(&mut self) {
        self.move_column_to_workspace(false);
    }

    /// Move the active column to the workspace below, focused there.
    pub fn move_column_to_workspace_down(&mut self) {
        self.move_column_to_workspace(true);
    }

    fn move_column_to_workspace(&mut self, down: bool) {
        let Some(target) = self.neighbour_workspace(down) else { return };
        let ctx = self.ctx();
        let Some(Some(column)) = self.in_active(|ws, ctx| ws.remove_column_by_idx(ctx, ws.active))
        else {
            return;
        };
        if let Some(ws) = self.workspaces.get_mut(target) {
            let before = ws.view_rects(&ctx);
            ws.add_column(&ctx, None, column, true);
            ws.flip(&ctx, &before);
        }
        self.ensure_trailing();
        self.activate_workspace(target);
    }

    /// Swap the active workspace with the one above.
    pub fn move_workspace_up(&mut self) {
        let Some(new) = self.neighbour_workspace(false) else { return };
        self.workspaces.swap(self.active, new);
        self.moved_workspace(new);
    }

    /// Swap the active workspace with the one below.
    pub fn move_workspace_down(&mut self) {
        let Some(new) = self.neighbour_workspace(true) else { return };
        self.workspaces.swap(self.active, new);
        self.moved_workspace(new);
    }

    fn moved_workspace(&mut self, new: usize) {
        self.ensure_trailing();
        let previous = self.previous;
        self.switch = None;
        self.active = new;
        self.stamp(new);
        self.previous = previous;
        self.clean_up();
    }

    /// Alone in its column: join the column to the left at the bottom. Otherwise: leave the
    /// column for a new one on its left.
    pub fn consume_or_expel_window_left(&mut self) {
        self.in_active(Workspace::consume_or_expel_left);
    }

    /// Alone in its column: join the column to the right at the bottom. Otherwise: leave the
    /// column for a new one on its right.
    pub fn consume_or_expel_window_right(&mut self) {
        self.in_active(Workspace::consume_or_expel_right);
    }

    /// Pull the first tile of the next column into the bottom of this one.
    pub fn consume_into_column(&mut self) {
        self.in_active(Workspace::consume_into_column);
    }

    /// Push the bottom tile of this column into a new column on its right.
    pub fn expel_from_column(&mut self) {
        self.in_active(Workspace::expel_from_column);
    }

    /// The next (or previous) preset width for the active column.
    pub fn switch_preset_width(&mut self, forward: bool) {
        self.in_active(|ws, ctx| ws.toggle_width(ctx, forward));
    }

    /// Widen (or narrow) the active column by `percent` of the working width.
    pub fn set_width_delta(&mut self, percent: f32) {
        self.in_active(|ws, ctx| ws.set_width_delta(ctx, percent));
    }

    /// Maximise the active column to the working width, or back.
    pub fn toggle_full_width(&mut self) {
        self.in_active(Workspace::toggle_full_width);
    }

    /// The active tile fills the viewport (out of its column first), or back.
    pub fn toggle_fullscreen(&mut self) {
        self.in_active(Workspace::toggle_fullscreen);
    }

    /// Widen the active column over the free space of the fully visible columns.
    pub fn expand_to_available_width(&mut self) {
        self.in_active(Workspace::expand_to_available_width);
    }

    /// Centre the active column in the view.
    pub fn center_column(&mut self) {
        self.in_active(Workspace::center_column);
    }

    /// Centre the fully visible columns as a group.
    pub fn center_visible_columns(&mut self) {
        self.in_active(Workspace::center_visible_columns);
    }

    /// Stack or tab the active column.
    pub fn toggle_tabbed(&mut self) {
        self.in_active(Workspace::toggle_tabbed);
    }

    /// Open or close the overview.
    pub fn set_overview(&mut self, open: bool) {
        if self.overview_open == open {
            return;
        }
        self.overview_open = open;
        let from = self.overview_progress();
        let velocity = self.overview.map_or(0.0, |a| a.velocity_at(self.now));
        let to = if open { 1.0 } else { 0.0 };
        self.overview = self.ctx().spring(from, to, velocity, view_spring());
    }

    /// [`Self::set_overview`] the other way.
    pub fn toggle_overview(&mut self) {
        self.set_overview(!self.overview_open);
    }

    fn overview_progress(&self) -> f32 {
        self.overview.map_or_else(
            || if self.overview_open { 1.0 } else { 0.0 },
            |a| narrow(a.value_at(self.now)),
        )
    }

    fn zoom(&self) -> f32 {
        self.overview_progress().mul_add(-(1.0 - OVERVIEW_ZOOM), 1.0).max(0.01)
    }

    /// Where `ws`'s view sits as drawn: its own position, moved in the overview (by the
    /// overview's progress) to centre the whole strip when it fits the zoomed-out window. The
    /// view scrolled to its focus would otherwise hang the strip off to one side of the panel.
    fn shown_view_pos(&self, ws: &Workspace, ctx: &Ctx) -> f32 {
        let pos = ws.view_pos(ctx);
        let progress = self.overview_progress();
        if progress <= 0.0 || ws.columns.is_empty() {
            return pos;
        }
        let g = &ctx.g;
        let strip_w = ws.column_x(ws.columns.len(), g) - g.gaps;
        if strip_w > g.view_w / OVERVIEW_ZOOM {
            return pos;
        }
        let centred = (strip_w - g.view_w) / 2.0;
        (centred - pos).mul_add(progress, pos)
    }

    // ----- gestures --------------------------------------------------------------------------

    /// A horizontal two-finger or touch drag of the strip begins.
    pub fn view_gesture_begin(&mut self) {
        let ctx = self.ctx();
        if let Some(ws) = self.workspaces.get_mut(self.active) {
            ws.gesture_begin(&ctx);
        }
    }

    /// The drag moved by `dx` points (view-offset space) at `at` (the [`Self::set_clock`]
    /// clock). False when no drag is in progress. Trackpad and touch deltas are both points,
    /// taken 1:1 (niri's libinput normalisation has no counterpart here).
    pub fn view_gesture_update(&mut self, dx: f32, at: Duration) -> bool {
        let dx = dx / self.zoom();
        self.workspaces.get_mut(self.active).is_some_and(|ws| ws.gesture_update(dx, at))
    }

    /// The drag ended: snap (or, `cancelled`, settle where it is with the focus kept in view).
    pub fn view_gesture_end(&mut self, cancelled: bool) -> bool {
        let ctx = self.ctx();
        self.workspaces.get_mut(self.active).is_some_and(|ws| ws.gesture_end(&ctx, cancelled))
    }

    /// Whether a strip drag is in progress.
    #[must_use]
    pub fn view_gesture_active(&self) -> bool {
        self.workspaces.get(self.active).is_some_and(|ws| ws.view.is_gesture())
    }

    /// Whether a drag between workspaces is in progress.
    #[must_use]
    pub const fn ws_gesture_active(&self) -> bool {
        matches!(self.switch, Some(Switch::Gesture(_)))
    }

    /// A vertical drag between workspaces begins.
    pub fn ws_gesture_begin(&mut self) {
        let current = self.render_idx();
        self.switch = Some(Switch::Gesture(SwitchGesture {
            center: self.active,
            start: current,
            current,
            tracker: SwipeTracker::new(),
            clamped: !self.overview_open,
        }));
    }

    /// One viewport height (plus the workspace gap) is one workspace.
    fn ws_gesture_height(&self) -> f32 {
        (self.view_h * (1.0 + WORKSPACE_GAP)).max(1.0)
    }

    fn ws_gesture_range(&self, gesture: &SwitchGesture) -> (f32, f32) {
        let last = self.workspaces.len().saturating_sub(1);
        if gesture.clamped {
            (
                count(gesture.center.saturating_sub(1)),
                count(gesture.center.saturating_add(1).min(last)),
            )
        } else {
            (0.0, count(last))
        }
    }

    /// The workspace drag moved by `dy` points (positive towards the workspace below).
    pub fn ws_gesture_update(&mut self, dy: f32, at: Duration) -> bool {
        let zoom = self.zoom();
        let height = self.ws_gesture_height();
        let Some(Switch::Gesture(g)) = &self.switch else { return false };
        let (min, max) = self.ws_gesture_range(g);
        let Some(Switch::Gesture(g)) = &mut self.switch else { return false };
        g.tracker.push(f64::from(dy / zoom), at);
        let band = RubberBand { limit: WORKSPACE_BAND.limit / f64::from(zoom), ..WORKSPACE_BAND };
        let pos = g.tracker.pos() / f64::from(height);
        let new = narrow(band.clamp(f64::from(min), f64::from(max), f64::from(g.start) + pos));
        let changed = (new - g.current).abs() > f32::EPSILON;
        g.current = new;
        changed
    }

    /// The workspace drag ended: the nearest workspace to where the fling would stop, within
    /// one of the start (or, `cancelled`, back to where it began).
    pub fn ws_gesture_end(&mut self, cancelled: bool) -> bool {
        let now = self.now;
        let zoom = self.zoom();
        let height = f64::from(self.ws_gesture_height());
        let Some(Switch::Gesture(g)) = &self.switch else { return false };
        let (min, max) = self.ws_gesture_range(g);
        let Some(Switch::Gesture(g)) = &mut self.switch else { return false };
        g.tracker.push(0.0, now);
        let band = RubberBand { limit: WORKSPACE_BAND.limit / f64::from(zoom), ..WORKSPACE_BAND };
        let current_pos = g.tracker.pos() / height;
        let projected = g.tracker.projected_end_pos() / height;
        let start = f64::from(g.start);
        let current = g.current;
        let (target, velocity) = if cancelled {
            (g.center, 0.0)
        } else {
            let end = (start + projected).clamp(f64::from(min), f64::from(max)).round().max(0.0);
            let velocity = g.tracker.velocity() / height
                * band.clamp_derivative(f64::from(min), f64::from(max), start + current_pos);
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "rounded, ≥ 0, a workspace index"
            )]
            let end = end as usize;
            (end, velocity)
        };
        let target = target.min(self.workspaces.len().saturating_sub(1));
        if self.active != target {
            self.previous = self.workspaces.get(self.active).map(Workspace::id);
        }
        self.active = target;
        self.stamp(target);
        self.switch =
            self.ctx().spring(current, count(target), velocity, switch_spring()).map(Switch::Anim);
        if self.switch.is_none() {
            self.clean_up();
        }
        true
    }

    /// A ⌘⌥ wheel: horizontal steps focus columns, vertical steps switch workspaces (at most
    /// one per 150 ms). True when something moved.
    pub fn wheel(&mut self, dx: f32, dy: f32, at: Duration) -> bool {
        let mut moved = false;
        let steps = self.wheel_x.accumulate(dx);
        for _ in 0..steps.unsigned_abs() {
            if steps > 0 {
                self.focus_column_right();
            } else {
                self.focus_column_left();
            }
            moved = true;
        }
        let steps = self.wheel_y.accumulate(dy);
        let cooled = self.wheel_switched.is_none_or(|t| at.saturating_sub(t) >= WHEEL_COOLDOWN);
        if steps != 0 && cooled {
            if steps > 0 {
                self.focus_workspace_down();
            } else {
                self.focus_workspace_up();
            }
            self.wheel_switched = Some(at);
            moved = true;
        }
        moved
    }

    // ----- drag and drop ---------------------------------------------------------------------

    /// Where a tile dropped at `(x, y)` (viewport coordinates) would go.
    #[must_use]
    pub fn drop_target(&self, x: f32, y: f32) -> Option<DropTarget> {
        let ctx = self.ctx();
        let zoom = self.zoom();
        let rects = self.ws_rects(zoom);
        let viewport = ctx.g.viewport();
        let mut above: Option<usize> = None;
        for (idx, row) in rects.iter().enumerate() {
            if !row.intersects(&viewport) {
                if row.y <= y {
                    above = Some(idx);
                }
                continue;
            }
            if y >= row.y && y < row.bottom() {
                let ws = self.workspaces.get(idx)?;
                let strip_x = (x - row.x) / zoom + self.shown_view_pos(ws, &ctx);
                return Some(Self::drop_in(&ctx.g, idx, ws, strip_x, (y - row.y) / zoom));
            }
            if y >= row.bottom() {
                above = Some(idx);
            }
        }
        // Between two workspaces: only the overview shows that gap on purpose.
        let index = above.map_or(0, |a| a.saturating_add(1));
        self.overview_open.then_some(DropTarget::NewWorkspace { index })
    }

    /// The drop target at a point in workspace `workspace`'s strip coordinates.
    fn drop_in(
        g: &Geom,
        workspace: usize,
        ws: &Workspace,
        strip_x: f32,
        strip_y: f32,
    ) -> DropTarget {
        let mut col_x = 0.0;
        for (idx, col) in ws.columns.iter().enumerate() {
            let width = col.resolved_width(g);
            if strip_x < col_x {
                return DropTarget::NewColumn { workspace, index: idx };
            }
            if strip_x < col_x + width {
                let edge = width * DROP_EDGE;
                if strip_x < col_x + edge {
                    return DropTarget::NewColumn { workspace, index: idx };
                }
                if strip_x > col_x + width - edge {
                    return DropTarget::NewColumn { workspace, index: idx.saturating_add(1) };
                }
                let index = if col.mode == DisplayMode::Tabbed {
                    col.tiles.len()
                } else {
                    col.tile_rects(g)
                        .iter()
                        .position(|t| strip_y < t.y + t.h / 2.0)
                        .unwrap_or(col.tiles.len())
                };
                return DropTarget::IntoColumn { workspace, column: idx, index };
            }
            col_x += width + g.gaps;
        }
        DropTarget::NewColumn { workspace, index: ws.columns.len() }
    }

    /// Put `tile` where [`Self::drop_target`] said (targets are read as before the tile left
    /// its place) and focus it there.
    pub fn move_tile(&mut self, tile: TileRef, target: DropTarget) {
        let Some(src) = self.position(tile) else { return };
        let exists = match target {
            DropTarget::IntoColumn { workspace, column, .. } => {
                self.workspaces.get(workspace).is_some_and(|ws| column < ws.columns.len())
            }
            DropTarget::NewColumn { workspace, index } => {
                self.workspaces.get(workspace).is_some_and(|ws| index <= ws.columns.len())
            }
            DropTarget::NewWorkspace { .. } => true,
        };
        if !exists {
            return;
        }
        let ctx = self.ctx();
        if let DropTarget::IntoColumn { workspace, column, index } = target
            && workspace == src.workspace
            && column == src.column
            && (index == src.tile || index == src.tile.saturating_add(1))
        {
            self.focus(tile);
            return;
        }
        let Some(src_len) = self
            .workspaces
            .get(src.workspace)
            .and_then(|ws| ws.columns.get(src.column))
            .map(|c| c.tiles.len())
        else {
            return;
        };
        let column_goes = src_len == 1;
        let removed = {
            let Some(ws) = self.workspaces.get_mut(src.workspace) else { return };
            let before = ws.view_rects(&ctx);
            let removed = ws.remove_tile_by_idx(&ctx, src.column, src.tile);
            ws.flip(&ctx, &before);
            removed
        };
        let Some(removed) = removed else { return };
        let shift = |ws: usize, col: usize| {
            if ws == src.workspace && column_goes && src.column < col {
                col.saturating_sub(1)
            } else {
                col
            }
        };
        let ws_idx = match target {
            DropTarget::IntoColumn { workspace, column, index } => {
                let col = shift(workspace, column);
                let index =
                    if workspace == src.workspace && column == src.column && src.tile < index {
                        index.saturating_sub(1)
                    } else {
                        index
                    };
                let Some(ws) = self.workspaces.get_mut(workspace) else { return };
                let before = ws.view_rects(&ctx);
                if col < ws.columns.len() {
                    ws.add_tile_to_column(&ctx, col, Some(index), removed.tile, true);
                } else {
                    ws.add_tile(&ctx, Some(col), removed, true);
                }
                ws.flip(&ctx, &before);
                workspace
            }
            DropTarget::NewColumn { workspace, index } => {
                let index = shift(workspace, index);
                let Some(ws) = self.workspaces.get_mut(workspace) else { return };
                let before = ws.view_rects(&ctx);
                ws.add_tile(&ctx, Some(index), removed, true);
                ws.flip(&ctx, &before);
                workspace
            }
            DropTarget::NewWorkspace { index } => {
                // Never past the trailing empty workspace.
                let at = index.min(self.workspaces.len().saturating_sub(1));
                self.add_workspace_at(at);
                let Some(ws) = self.workspaces.get_mut(at) else { return };
                ws.add_tile(&ctx, None, removed, true);
                at
            }
        };
        self.ensure_trailing();
        // The source workspace may have shifted if a new one went in above it; find the tile.
        let ws_idx = self.position(tile).map_or(ws_idx, |p| p.workspace);
        self.activate_workspace(ws_idx);
        self.clean_up();
    }

    /// While a tile is dragged, the pointer within 30 pt of the working area's left or right
    /// edge scrolls the active strip (after 100 ms, up to 1500 pt/s at the very edge). Call
    /// on every pointer move and every frame of the drag, after [`Self::set_clock`]; `x` is in
    /// viewport coordinates. True while the pointer is in a band.
    pub fn dnd_edge_scroll(&mut self, x: f32) -> bool {
        let ctx = self.ctx();
        let zoom = self.zoom();
        let Some(wr) = self.ws_rects(zoom).get(self.active).copied() else { return false };
        self.workspaces
            .get_mut(self.active)
            .is_some_and(|ws| ws.dnd_scroll(&ctx, (x - wr.x) / zoom))
    }

    /// The drag ended: a strip that scrolled snaps as a released swipe would; one that did not
    /// stays put.
    pub fn dnd_scroll_end(&mut self) {
        let ctx = self.ctx();
        for ws in &mut self.workspaces {
            ws.dnd_scroll_end(&ctx);
        }
    }

    // ----- interactive resize ----------------------------------------------------------------

    /// Dragging the gap right of `column` in the active workspace begins.
    pub fn resize_begin(&mut self, column: usize) -> bool {
        let ctx = self.ctx();
        self.workspaces.get_mut(self.active).is_some_and(|ws| ws.resize_begin(&ctx, column))
    }

    /// The gap moved `dx` points from where the drag began.
    pub fn resize_update(&mut self, dx: f32) -> bool {
        let ctx = self.ctx();
        self.workspaces.get_mut(self.active).is_some_and(|ws| ws.resize_update(&ctx, dx))
    }

    /// The drag ended; the active column is brought back into view if it left.
    pub fn resize_end(&mut self) {
        let ctx = self.ctx();
        if let Some(ws) = self.workspaces.get_mut(self.active) {
            ws.resize_end(&ctx);
        }
    }

    // ----- rendering -------------------------------------------------------------------------

    fn ws_rects(&self, zoom: f32) -> Vec<Rect> {
        let (w, h) = (self.view_w * zoom, self.view_h * zoom);
        let gap = self.view_h * WORKSPACE_GAP * zoom;
        let step = h + gap;
        let (sx, sy) = ((self.view_w - w) / 2.0, (self.view_h - h) / 2.0);
        let first = -self.render_idx() * step;
        (0..self.workspaces.len())
            .map(|i| Rect { x: sx, y: count(i).mul_add(step, first) + sy, w, h })
            .collect()
    }

    /// A tile's current rectangle in viewport coordinates.
    fn screen_rect(&self, pos: Pos) -> Option<Rect> {
        let ctx = self.ctx();
        let zoom = self.zoom();
        let ws = self.workspaces.get(pos.workspace)?;
        let mut wr = *self.ws_rects(zoom).get(pos.workspace)?;
        wr.x = (ws.view_pos(&ctx) - self.shown_view_pos(ws, &ctx)).mul_add(zoom, wr.x);
        let flat = ws
            .columns
            .iter()
            .take(pos.column)
            .map(|c| c.tiles.len())
            .sum::<usize>()
            .saturating_add(pos.tile);
        let (_, r) = *ws.view_rects(&ctx).get(flat)?;
        Some(Rect {
            x: r.x.mul_add(zoom, wr.x),
            y: r.y.mul_add(zoom, wr.y),
            w: r.w * zoom,
            h: r.h * zoom,
        })
    }

    /// Everything to draw at the clock.
    #[must_use]
    pub fn frame(&self) -> Frame {
        let ctx = self.ctx();
        let g = ctx.g;
        let zoom = self.zoom();
        let rects = self.ws_rects(zoom);
        let viewport = g.viewport();
        let focused = self.focused();
        let step = self.view_h * (1.0 + WORKSPACE_GAP);
        let mut tiles = Vec::new();
        let mut panels = rects.clone();
        for (wi, (ws, panel)) in self.workspaces.iter().zip(&rects).enumerate() {
            let shown = panel.intersects(&viewport);
            let view_pos = self.shown_view_pos(ws, &ctx);
            // A centred strip wider than the view gets a panel as wide as it is.
            if let Some(out) = panels.get_mut(wi)
                && !ws.columns.is_empty()
            {
                let strip_w = ws.column_x(ws.columns.len(), &g) + g.gaps;
                let left = (-g.gaps - view_pos).mul_add(zoom, panel.x);
                if (view_pos - ws.view_pos(&ctx)).abs() > f32::EPSILON && strip_w * zoom > out.w {
                    *out = Rect { x: left, w: strip_w * zoom, ..*out };
                }
            }
            // The tiles as the workspace lays them out, moved by what the overview centred.
            let wr = &Rect { x: (ws.view_pos(&ctx) - view_pos).mul_add(zoom, panel.x), ..*panel };
            let mut visible: Option<(usize, usize)> = None;
            let mut x = 0.0;
            for (ci, col) in ws.columns.iter().enumerate() {
                let width = col.resolved_width(&g);
                let left = (x - view_pos).mul_add(zoom, panel.x);
                let right = width.mul_add(zoom, left);
                if shown && right > 0.0 && left < g.view_w {
                    visible = Some(visible.map_or((ci, ci), |(lo, hi)| (lo.min(ci), hi.max(ci))));
                }
                x += width + g.gaps;
            }
            let near_col = |ci: usize| {
                visible.is_some_and(|(lo, hi)| {
                    ci.saturating_add(1) >= lo && ci <= hi.saturating_add(1)
                }) || (wi == self.active && ci == ws.active)
            };
            // At rest: the overview closed, the switch landed on the active workspace.
            let rest_top = (count(wi) - count(self.active)) * step;
            let now_rects = ws.view_rects(&ctx);
            let rest_rects = ws.rest_rects(&g);
            let mut flat = now_rects.iter().zip(&rest_rects);
            for (ci, col) in ws.columns.iter().enumerate() {
                let tabbed = col.mode == DisplayMode::Tabbed;
                for (ti, tile) in col.tiles.iter().enumerate() {
                    let Some((&(key, r), &(_, rest))) = flat.next() else { continue };
                    let open = tile.open_progress(ctx.now);
                    tiles.push(Placed {
                        tile: key,
                        rect: Rect {
                            x: r.x.mul_add(zoom, wr.x),
                            y: r.y.mul_add(zoom, wr.y),
                            w: r.w * zoom,
                            h: r.h * zoom,
                        },
                        target: Rect { y: rest.y + rest_top, ..rest },
                        pos: Pos { workspace: wi, column: ci, tile: ti },
                        focused: focused == Some(key),
                        alpha: open.clamp(0.0, 1.0),
                        scale: 0.5_f32.mul_add(open.clamp(0.0, 1.0), 0.5),
                        hidden: tabbed && ti != col.active,
                        tabs: tabbed.then_some((col.active, col.tiles.len())),
                        near: near_col(ci),
                        fullscreen: col.fullscreen,
                    });
                }
            }
        }
        let closing = self
            .closing
            .iter()
            .filter(|c| !c.anim.is_done(self.now))
            .map(|c| {
                let v = narrow(c.anim.value_at(self.now));
                Closing {
                    tile: c.tile,
                    rect: c.rect,
                    alpha: 1.0 - v,
                    scale: 0.2_f32.mul_add(-v, 1.0),
                }
            })
            .collect();
        let strip = self.workspaces.get(self.active).map_or_else(Strip::default, |ws| {
            let mut x = 0.0;
            let columns = ws
                .columns
                .iter()
                .map(|c| {
                    let w = c.resolved_width(&g);
                    let at = x;
                    x += w + g.gaps;
                    (at, w)
                })
                .collect();
            Strip {
                columns,
                view: (ws.view_pos(&ctx), g.view_w),
                active: (!ws.columns.is_empty()).then_some(ws.active),
            }
        });
        Frame {
            tiles,
            closing,
            workspaces: panels.into_iter().enumerate().collect(),
            overview: self.overview_progress(),
            zoom,
            strip,
            animating: self.is_animating(),
        }
    }

    // ----- persistence -----------------------------------------------------------------------

    /// The layout as `layout.json` keeps it.
    #[must_use]
    pub fn save(&self) -> Saved {
        Saved {
            workspaces: self
                .workspaces
                .iter()
                .map(|ws| SavedWorkspace {
                    name: ws.name.clone(),
                    columns: ws
                        .columns
                        .iter()
                        .map(|c| SavedColumn {
                            tiles: c
                                .tiles
                                .iter()
                                .map(|t| SavedTile { tile: t.key, height: t.height })
                                .collect(),
                            active_tile: c.active,
                            width: c.width,
                            preset: c.preset,
                            full_width: c.full_width,
                            mode: c.mode,
                        })
                        .collect(),
                    active_column: ws.active,
                    view_offset: ws.view.stationary(),
                })
                .collect(),
            active: self.active,
        }
    }

    /// A layout from what [`Self::save`] kept, cleaned: duplicate tiles, empty columns and
    /// empty unnamed workspaces dropped, indices and widths clamped, a trailing empty
    /// workspace ensured.
    #[must_use]
    pub fn restore(saved: Saved, config: LayoutConfig) -> Self {
        let mut layout = Self::new(config);
        let presets = layout.config.presets.len();
        layout.workspaces.clear();
        let mut seen: HashSet<TileRef> = HashSet::new();
        let mut active = 0;
        for (idx, sw) in saved.workspaces.into_iter().enumerate() {
            let columns: Vec<Column> = sw
                .columns
                .into_iter()
                .filter_map(|sc| {
                    let tiles: Vec<Tile> = sc
                        .tiles
                        .into_iter()
                        .filter(|t| seen.insert(t.tile))
                        .map(|t| Tile { height: sanitize_height(t.height), ..Tile::new(t.tile) })
                        .collect();
                    let len = tiles.len();
                    (len > 0).then(|| Column {
                        tiles,
                        active: sc.active_tile.min(len.saturating_sub(1)),
                        width: sanitize_width(sc.width),
                        preset: sc.preset.filter(|p| *p < presets),
                        full_width: sc.full_width,
                        fullscreen: false,
                        mode: sc.mode,
                    })
                })
                .collect();
            let name = sw.name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
            if idx == saved.active {
                // A dropped active workspace hands over to the next one kept.
                active = layout.workspaces.len();
            }
            if columns.is_empty() && name.is_none() {
                continue;
            }
            let mut ws = layout.new_workspace();
            ws.active = sw.active_column.min(columns.len().saturating_sub(1));
            ws.columns = columns;
            ws.name = name;
            let offset = if sw.view_offset.is_finite() { sw.view_offset } else { 0.0 };
            ws.view = ViewOffset::Static(offset);
            layout.workspaces.push(ws);
        }
        layout.ensure_trailing();
        layout.active = active.min(layout.workspaces.len().saturating_sub(1));
        layout.stamp(layout.active);
        layout
    }
}

const fn sanitize_width(width: ColumnWidth) -> ColumnWidth {
    match width {
        ColumnWidth::Proportion(p) if p.is_finite() => ColumnWidth::Proportion(p.clamp(0.05, 1.0)),
        ColumnWidth::Fixed(w) if w.is_finite() => ColumnWidth::Fixed(w.clamp(1.0, 100_000.0)),
        ColumnWidth::Proportion(_) | ColumnWidth::Fixed(_) => ColumnWidth::Proportion(0.5),
    }
}

fn sanitize_height(height: TileHeight) -> TileHeight {
    match height {
        TileHeight::Auto { weight } if weight.is_finite() && weight > 0.0 => height,
        TileHeight::Fixed(h) if h.is_finite() && h >= 1.0 => height,
        TileHeight::Auto { .. } | TileHeight::Fixed(_) => TileHeight::default(),
    }
}

#[cfg(test)]
mod tests;
