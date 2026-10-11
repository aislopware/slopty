//! Slopty's chrome kit: what every surface is drawn with, below any view of its own.
//!
//! * [`kit`] — gpui-kit's theme kept on the same tokens, and the chrome's parts (buttons, pills,
//!   menus, hints, panes, motion).
//! * [`icons`] — the chrome's glyphs, the agents' marks and the git vocabulary, drawn by us.
//! * [`colors`] — theme tokens → GPUI colours.
//! * [`a11y`] — the keyboard ring and, for tests, the accessibility tree.
//! * [`highlight`] — syntax colouring for a file and a fenced code block.
//! * [`markdown`] — the one `TextView` style every Markdown surface draws by.
//! * [`fonts`] — bundled `IBM Plex Sans`, `Lilex` and Nerd symbols, registered at startup.
//! * [`frames`] — the UI frame-time probe (draw percentiles, cadence, drops).
//! * [`shown`] — work timed at the instant a paint reached the display.
//! * [`draw`] — a view's elements built from another entity's state, read and never written.
//! * [`keys`] — GPUI keystrokes → protocol key events.
//! * [`layer`] — where every floating layer (a popover, a menu, a dialog, a notice) stacks.
//! * [`clock`], [`fuzzy`], [`chrome_text`], [`file_types`] — the small pieces the views share.
//! * `retained` — the oracle: the frame shown against the same state drawn from scratch (tests).
//!
//! The views (`slopty-ui`) sit on it; nothing here reaches up to them
//! (`docs/decisions/ui.md`, "The chrome kit is a crate of its own").

#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

pub mod a11y;
pub mod chrome_text;
pub mod clock;
pub mod colors;
pub mod draw;
pub mod file_types;
pub mod fonts;
pub mod frames;
pub mod fuzzy;
pub mod highlight;
pub mod icons;
pub mod keys;
pub mod kit;
pub mod layer;
pub mod markdown;
#[cfg(any(test, feature = "e2e"))]
pub mod retained;
pub mod shown;
