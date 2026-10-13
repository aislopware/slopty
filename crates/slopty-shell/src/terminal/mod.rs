// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Terminal panel (feature-inventory §1.10).
//!
//! slopty: each tab shows Slopty's own terminal view of a session ([`crate::live`]);
//! upstream's `alacritty_terminal` emulator is not taken.
//!
//! - [`view`] — the font metrics, padding and panel fill;
//! - [`panel`] — session-scoped tabs, drag-reorder, and the Cmd/Ctrl+J toggle action.
//!
//! Method names come from `slopty_view_rpc::methods` and wire types from
//! `slopty_view_proto` (`TerminalSession`) — the same contract the engine serves
//! (feature-inventory §2.1).

pub(crate) mod dock;
pub mod panel;
pub mod view;
