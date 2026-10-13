//! What the shell's live surfaces are handed of the workspace's own views
//! (`workspace::view_model::surfaces::live`): a terminal's look, and the view with what it says
//! of itself.

use gpui::{AnyView, FocusHandle, Hsla, SharedString};

/// The look the shell gives a terminal: zeron's font, padding and palette.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveLook {
    /// The monospace family.
    pub family: SharedString,
    /// The font size, in points.
    pub font_size: f32,
    /// The row height, in points.
    pub line_height: f32,
    /// The grid's inset, in points.
    pub padding: f32,
    /// The fill under the grid.
    pub background: Hsla,
    /// Default text.
    pub foreground: Hsla,
    /// The block cursor.
    pub cursor: Hsla,
    /// The selection wash.
    pub selection: Hsla,
    /// ANSI 0–15.
    pub ansi: [Hsla; 16],
}

/// One of the workspace's views, for the shell to place.
#[derive(Clone, Debug)]
pub struct LiveView {
    /// What is drawn.
    pub view: AnyView,
    /// Where the keyboard goes.
    pub focus: FocusHandle,
    /// Its title now.
    pub title: Option<SharedString>,
    /// Its program has ended.
    pub ended: bool,
}
