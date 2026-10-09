//! The workspace layout: projects, their tabs, and each tab's tiling, as a pure model.
//!
//! A project ([`GroupKey`]) owns its tabs; each tab holds an n-ary split tree whose leaves are
//! panes ([`tree`]), and one project is on show ([`tiling`]). Pure: no clock, no toolkit. The
//! caller gives the area a tab is laid out in and reads each pane's rectangle back. Tiles name
//! `(worker, item)` pairs; what a tile shows is the caller's business.
//!
//! What a relaunch begins from is [`Saved`] (`layout.json`): the projects and their trees, and
//! beside them what the UI keeps with the arrangement it frames (the window, the faces, the
//! popouts, the timelines read, the frecency and the navigator).

pub mod tiling;
pub mod tree;

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use slopty_core::ItemId;
pub use tiling::{Drop, Pos, Project, SavedTiling, TabId, Tiling, TilingConfig};
pub use tree::{Laid, Pane, PaneId, Room, Sash, Side, SplitAxis, Tab, TabFrame};

pub use crate::groups::GroupKey;
use crate::groups::{DEFAULT_CHAIN, Frecency};

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

/// The saved form of a layout (`layout.json`).
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Saved {
    /// The projects, their tabs and each tab's tree.
    pub tiling: SavedTiling,
    /// The navigator beside the panes.
    pub navigator: Navigator,
    /// Where the main window stood, to open it there again. The layout saves nothing of its
    /// own here, nor in the ones below: the UI keeps them beside the arrangement they frame.
    pub window: Option<WindowFrame>,
    /// The agents' tiles whose face or TUI the person picked, and which.
    pub faces: Vec<SavedFace>,
    /// The tiles shown in windows of their own, and where those stood.
    pub popouts: Vec<SavedPopout>,
    /// The display tiles the person streamed from a display made for this device, or from the
    /// physical one, where that differs from what this device does unasked.
    pub displays: Vec<SavedDisplay>,
    /// How far this device read each project's timeline, for the recap its board opens on.
    pub looked: Vec<SavedLooked>,
    /// The tiles that show a project's board on their own, not in its orchestrator's tile.
    pub boards: Vec<SavedBoard>,
    /// How often and how lately each project was gone to here, for the palette to rank them.
    pub frecency: Frecency,
}

/// Where a window stood: its rectangle in points on its display, the display named by its
/// UUID (display ids do not outlive a reboot; a UUID names the same screen), and whether it
/// filled the screen.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct WindowFrame {
    /// The display it was on, by UUID; `None` where the platform names none.
    pub display: Option<String>,
    /// Left edge, from the display's.
    pub x: f32,
    /// Top edge, from the display's.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
    /// It was full screen; the rectangle is where it goes back to.
    pub fullscreen: bool,
}

impl WindowFrame {
    /// The smallest window worth opening again, in points: a frame narrower or shorter (one
    /// dragged to a sliver, a value that is not a number) is not kept.
    pub const MIN: f32 = 200.0;

    /// Whether the frame can be opened again as it is.
    #[must_use]
    pub fn sane(&self) -> bool {
        [self.x, self.y, self.width, self.height].iter().all(|v| v.is_finite())
            && self.width >= Self::MIN
            && self.height >= Self::MIN
    }
}

/// A tile whose agent shows its face (`true`) or its TUI, as the person picked.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SavedFace {
    /// Which.
    pub tile: TileRef,
    /// The face rather than the TUI.
    pub face: bool,
}

/// A display tile streamed from a display made for this device (`true`) or from the physical
/// one, as the person picked.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SavedDisplay {
    /// Which.
    pub tile: TileRef,
    /// From a display made for this device.
    pub sized: bool,
}

/// A tile shown in a window of its own.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct SavedPopout {
    /// Which.
    pub tile: TileRef,
    /// Where its window stood.
    pub frame: WindowFrame,
}

/// A tile that shows `project`'s board.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SavedBoard {
    /// Which.
    pub tile: TileRef,
    /// The project.
    pub project: slopty_proto::project::ProjectId,
}

/// How far this device read a project's timeline: the last entry its board showed, and when
/// it hid.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SavedLooked {
    /// The project.
    pub project: slopty_proto::project::ProjectId,
    /// The entry's `seq`.
    pub seq: u64,
    /// When the board hid, by this device's clock.
    pub at_ms: slopty_core::WallMs,
}

/// The navigator beside the panes, as this device left it. The layout keeps it only to save
/// it with the arrangement it frames; the UI draws it.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Navigator {
    /// Docked beside the panes, where the window is wide enough to dock it.
    pub shown: bool,
    /// Its width, in points, from [`Navigator::MIN_WIDTH`] to [`Navigator::MAX_WIDTH`].
    pub width: f32,
    /// The fact keys it groups the tiles by, the first a tile has winning
    /// ([`crate::groups::group`]): [`DEFAULT_CHAIN`] by project, `["machine"]` by machine, or
    /// any fact a tile reports.
    pub group_by: Vec<String>,
    /// The groups pinned above the rest, by key, in the order they were pinned (`MonoCode`'s
    /// pinned projects).
    pub pinned: Vec<GroupKey>,
    /// The groups whose moments post no notification here, by key.
    pub muted: Vec<GroupKey>,
}

impl Navigator {
    /// Its width before it is ever dragged.
    pub const DEFAULT_WIDTH: f32 = 248.0;
    /// The widest it is dragged to, in points.
    pub const MAX_WIDTH: f32 = 400.0;
    /// The narrowest it is dragged to, in points.
    pub const MIN_WIDTH: f32 = 200.0;

    /// `width` within the clamps; a width that is not a number is the default.
    #[must_use]
    pub const fn clamp_width(width: f32) -> f32 {
        if width.is_finite() {
            width.clamp(Self::MIN_WIDTH, Self::MAX_WIDTH)
        } else {
            Self::DEFAULT_WIDTH
        }
    }
}

impl Navigator {
    /// The chain the navigator groups by before the person picks another: by project.
    #[must_use]
    pub fn by_project() -> Vec<String> {
        DEFAULT_CHAIN.iter().map(|&k| k.to_owned()).collect()
    }
}

impl Default for Navigator {
    fn default() -> Self {
        Self {
            shown: true,
            width: Self::DEFAULT_WIDTH,
            group_by: Self::by_project(),
            pinned: Vec::new(),
            muted: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests;
