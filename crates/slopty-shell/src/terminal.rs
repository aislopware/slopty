// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Terminal panes (`docs/decisions/layout.md`, "One session per view, splits on demand").
//!
//! Each pane shows Slopty's own terminal view of a session ([`crate::live`]).
//!
//! - [`view`] — the font metrics, padding and panel fill;
//! - [`panel`] — the session's terminal, or a shell opened beside it.
//!
//! Method names come from `slopty_view_rpc::methods` and wire types from
//! `slopty_view_proto` (`TerminalSession`), the contract the view model serves.

pub mod panel;
pub mod view;
