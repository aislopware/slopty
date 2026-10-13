//! What the shell's live surfaces are handed of the workspace's own views
//! (`workspace::view_model::surfaces::live`): a terminal's frame, and the view with what it says
//! of itself.

use gpui::{AnyView, FocusHandle, SharedString};

/// The frame the shell gives a terminal's grid; its face and palette come with the theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveLook {
    /// The row height, in points.
    pub line_height: f32,
    /// The grid's inset, in points.
    pub padding: f32,
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
