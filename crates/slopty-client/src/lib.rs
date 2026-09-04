//! Client core, UI-toolkit agnostic.
//!
//! * [`term`] — [`term::TermState`]: applies `TermEvent`s to a screen + absolute line cache, tracks
//!   the viewport (scrolled or following), and says what to ask the host for.
//! * [`link`] — [`link::HostLink`]: one connection to a host; fans control and session streams into
//!   a single event channel and queues outbound messages.
//! * [`canvas`] — [`canvas::CanvasDoc`] (host-authoritative item layout, applied optimistically)
//!   and [`canvas::Camera`] (pan/zoom mapping).

pub mod canvas;
pub mod link;
pub mod term;

pub use canvas::{Camera, CanvasChange, CanvasDoc};
pub use link::{HostLink, LinkEvent};
pub use term::{Effect, TermState, ViewRow};
