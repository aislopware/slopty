// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Zeron's numbers: spacing, radii and the shell's measures.
//!
//! Pixels at the 16 px base (`.research/zeron-adopt-2026-10-13.md` §1.9 and §1.11). Numbers
//! drive layout and never depend on which colour is painted.

/// The spacing ladder (`theme.rs` `SPACE_*` of zeron), plus the numbers that recur outside it.
pub mod space {
    /// The title/description stack's optical gap.
    pub const STACK: f32 = 1.0;
    /// A menu row's gap, the sidebar list's gap.
    pub const XXS: f32 = 2.0;
    /// The smallest step.
    pub const XS: f32 = 4.0;
    /// A row's vertical pad.
    pub const ROW_Y: f32 = 6.0;
    /// The small step.
    pub const SM: f32 = 8.0;
    /// A menu row's icon gap, the titlebar cluster's pad.
    pub const ICON_GAP: f32 = 10.0;
    /// The medium step.
    pub const MD: f32 = 12.0;
    /// The large step.
    pub const LG: f32 = 16.0;
    /// A dialog's pad.
    pub const DIALOG: f32 = 20.0;
    /// A fade band, a section card's top.
    pub const BAND: f32 = 24.0;
    /// Between settings sections.
    pub const SECTION: f32 = 32.0;
    /// A settings page's side pad.
    pub const PAGE_X: f32 = 40.0;
    /// The transcript's gutters, a settings page's bottom pad.
    pub const GUTTER: f32 = 48.0;
}

/// Corner radii (§1.9).
pub mod radius {
    /// A project monogram.
    pub const MONOGRAM: f32 = 3.0;
    /// A pull request badge.
    pub const BADGE: f32 = 4.0;
    /// Inline code.
    pub const INLINE_CODE: f32 = 4.5;
    /// A key cap, a key hint, a tool's file badge.
    pub const KEY: f32 = 5.0;
    /// Buttons, chips, tab chips, toolbar controls, tooltips (`CONTROL_RADIUS`).
    pub const CONTROL: f32 = 6.0;
    /// A menu row: the card's 12 less its border and inset.
    pub const MENU_ROW: f32 = 7.0;
    /// A sidebar row, a dialog field, a button, a badge pill, a terminal tab.
    pub const ROW: f32 = 8.0;
    /// A subagent card.
    pub const SUBAGENT: f32 = 9.0;
    /// A code block, a palette row, a notice (`PANEL_RADIUS`).
    pub const PANEL: f32 = 10.0;
    /// A popover, menu, toast, settings block (`CARD_RADIUS`).
    pub const CARD: f32 = 12.0;
    /// A user bubble, the palette, a dialog, the queue tray (`BUBBLE_RADIUS`).
    pub const BUBBLE: f32 = 16.0;
    /// The composer pill.
    pub const COMPOSER: f32 = 26.0;
}

/// The window's titlebar overlay.
pub const TITLEBAR_HEIGHT: f32 = 38.0;
/// The titlebar's top pad: 38 / 2 + 4 / 2 = 21, the traffic lights' centre.
pub const TITLEBAR_TOP_PAD: f32 = 4.0;
/// An in-pane header.
pub const HEADER_HEIGHT: f32 = 44.0;
/// A surface toolbar.
pub const TOOLBAR_HEIGHT: f32 = 38.0;
/// The working line above the composer.
pub const STATUS_STRIP_HEIGHT: f32 = 24.0;
/// The transcript's edge fade.
pub const TRANSCRIPT_FADE_BAND: f32 = 24.0;
/// Where the titlebar cluster starts, past the traffic lights.
pub const TITLEBAR_CLUSTER_X: f32 = 88.0;
/// A titlebar button.
pub const TITLEBAR_BUTTON: f32 = 24.0;
/// A trailing titlebar control.
pub const TITLEBAR_CONTROL: f32 = 28.0;

/// The sidebar's width: least, default, most.
pub const SIDEBAR_WIDTH: (f32, f32, f32) = (242.0, 256.0, 400.0);
/// The files column's width: least, default, most.
pub const FILES_WIDTH: (f32, f32, f32) = (220.0, 286.0, 440.0);
/// The right pane's least and default width.
pub const RIGHT_PANE_WIDTH: (f32, f32) = (360.0, 520.0);
/// What the conversation keeps beside the right pane.
pub const CONVERSATION_MIN: f32 = 300.0;
/// The terminal dock's height: least, default; it takes at most [`TERMINAL_DOCK_MAX`] of the
/// window.
pub const TERMINAL_DOCK_HEIGHT: (f32, f32) = (160.0, 280.0);
/// The terminal dock's most, as a share of the window.
pub const TERMINAL_DOCK_MAX: f32 = 0.55;
/// The transcript column: default, least, most, step.
pub const TRANSCRIPT_WIDTH: (f32, f32, f32, f32) = (736.0, 560.0, 1200.0, 16.0);
/// A resize seam's hit target, either side.
pub const SEAM_HIT: f32 = 10.0;

/// The composer pill on one line.
pub const COMPOSER_COMPACT: f32 = 49.0;
/// The expanded composer: least, most.
pub const COMPOSER_EXPANDED: (f32, f32) = (120.0, 304.0);
/// The new-thread canvas's widest composer.
pub const COMPOSER_CANVAS_MAX: f32 = 768.0;
/// The footer row under the composer.
pub const COMPOSER_FOOTER: f32 = 24.0;
/// The send and stop circle.
pub const SEND_BUTTON: f32 = 28.0;
/// The scroll-to-bottom pill.
pub const SCROLL_PILL: f32 = 30.0;

/// A compact sidebar row.
pub const SIDEBAR_ROW_COMPACT: f32 = 29.0;
/// A detailed sidebar row.
pub const SIDEBAR_ROW_DETAILED: f32 = 45.0;
/// A sidebar section header.
pub const SIDEBAR_SECTION: f32 = 28.0;
/// A sidebar foot button.
pub const SIDEBAR_FOOT_BUTTON: f32 = 28.0;

/// A surface tab chip in the titlebar row: width, height.
pub const SURFACE_TAB: (f32, f32) = (112.0, 24.0);
/// The terminal dock's tab bar.
pub const TERMINAL_TAB_BAR: f32 = 40.0;
/// A terminal tab: width, height.
pub const TERMINAL_TAB: (f32, f32) = (118.0, 28.0);
/// A picker row in a popover.
pub const PICKER_ROW: f32 = 32.0;
/// A popover's search row.
pub const SEARCH_ROW: f32 = 40.0;
/// The command palette's widest.
pub const PALETTE_WIDTH: f32 = 560.0;
/// A dialog card's width.
pub const DIALOG_WIDTH: f32 = 360.0;
/// A toast's width.
pub const TOAST_WIDTH: f32 = 356.0;
/// A settings page's widest column.
pub const SETTINGS_COLUMN: f32 = 760.0;
/// A settings row's least height.
pub const SETTINGS_ROW: f32 = 60.0;
/// A file tree or search row.
pub const FILE_ROW: f32 = 27.0;
/// A diff's file header.
pub const DIFF_FILE_HEADER: f32 = 38.0;
/// A diff hunk header.
pub const DIFF_HUNK: f32 = 28.0;
/// A diff line.
pub const DIFF_LINE: f32 = 21.0;
/// The session info card's width.
pub const SESSION_INFO_WIDTH: f32 = 272.0;
