//! Slopty's chrome kit: the small parts every view shares, below any view of its own.
//!
//! * [`kit`] — figures of one width, durations and sizes in words, motion's gate, the sparkline.
//! * [`icons`] — how a thing stands ([`icons::Status`]).
//! * [`colors`] — theme tokens → GPUI colours.
//! * [`a11y`] — for tests, the accessibility tree.
//! * [`markdown`] — a line of Markdown said as plain words.
//! * [`fonts`] — the bundled Nerd symbol face, registered at startup, and the terminal's fallbacks.
//! * [`frames`] — the UI frame-time probe (draw percentiles, cadence, drops).
//! * [`shown`] — work timed at the instant a paint reached the display.
//! * [`keys`] — GPUI keystrokes → protocol key events.
//! * [`clock`] — the wall clock the readouts are drawn by.
//! * `retained` — the oracle: the frame shown against the same state drawn from scratch (tests).
//!
//! zeron's design system is the shell's (`slopty-shell`): what Slopty's views draw is drawn with
//! it (`slopty_shell::live`), never again here.
//!
//! The views (`slopty-ui`) sit on it; nothing here reaches up to them
//! (`docs/decisions/ui.md`, "The chrome kit is a crate of its own").

#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

pub mod a11y;
#[cfg(test)]
mod canary;
pub mod clock;
pub mod colors;
pub mod fonts;
pub mod frames;
pub mod icons;
pub mod keys;
pub mod kit;
pub mod markdown;
#[cfg(any(test, feature = "e2e"))]
pub mod retained;
pub mod shown;
pub mod spot;
