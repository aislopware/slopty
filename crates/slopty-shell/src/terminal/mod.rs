// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Terminal panes (`docs/decisions/layout.md`, "One session per view, splits on demand").
//!
//! slopty: each pane shows Slopty's own terminal view of a session ([`crate::live`]);
//! upstream's `alacritty_terminal` emulator is not taken.
//!
//! - [`view`] — the font metrics, padding and panel fill;
//! - [`panel`] — the session's terminal, or a shell opened beside it.
//!
//! Method names come from `slopty_view_rpc::methods` and wire types from
//! `slopty_view_proto` (`TerminalSession`) — the same contract the engine serves
//! (feature-inventory §2.1).

pub mod panel;
pub mod view;
