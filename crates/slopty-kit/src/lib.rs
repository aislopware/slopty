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
//! Zeron's design system, copied from zeron (MIT, `THIRD_PARTY_NOTICES.md`) and adapted:
//!
//! * [`theme`] — the [`theme::Theme`] global: zeron's tokens as GPUI colours, the material
//!   functions and the paint helpers ([`theme::ink`], [`theme::hairline`], [`theme::wash`]).
//! * [`appearance`], [`typography`] — light and dark, the accent and material, Geist and the rem
//!   scale, and the hook the app keeps the person's choices through.
//! * [`motion`] — the motion catalogue, hover fades, state morphs, the loaders' pulse clock.
//! * [`solar`], [`file_icons`] — the Solar icons and the Symbols file icons, and their assets.
//! * [`glass`], [`frost`], [`edge_fade`], [`surface_chrome`] — plates, frosted floats, per-pixel
//!   edge fades, the surface toolbars.
//! * [`syntax`] — zeron's highlight contract on Slopty's grammars.
//!
//! The views (`slopty-ui`) sit on it; nothing here reaches up to them
//! (`docs/decisions/ui.md`, "The chrome kit is a crate of its own").

#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

pub mod a11y;
pub mod appearance;
pub mod chrome_text;
pub mod clock;
pub mod colors;
pub mod draw;
pub mod edge_fade;
pub mod file_icons;
pub mod file_types;
pub mod fonts;
pub mod frames;
pub mod frost;
pub mod fuzzy;
pub mod glass;
pub mod highlight;
pub mod icons;
pub mod keys;
pub mod kit;
pub mod layer;
pub mod loaders;
pub mod markdown;
pub mod motion;
pub mod notice;
pub mod popover;
#[cfg(any(test, feature = "e2e"))]
pub mod retained;
pub mod rounded_clip;
pub mod running_pill;
pub mod shown;
pub mod solar;
pub mod surface_chrome;
pub mod syntax;
pub mod syntax_cache;
pub mod text_ranges;
pub mod theme;
pub mod toast;
pub mod typography;
pub mod widgets;
