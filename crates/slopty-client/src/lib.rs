//! Client core, UI-toolkit agnostic.
//!
//! * [`term`] — [`term::TermState`]: applies `TermEvent`s to a screen + absolute line cache, tracks
//!   the viewport (scrolled or following), and says what to ask the host for.
//! * [`link`] — [`link::HostLink`]: one connection to a host; fans control and session streams into
//!   a single event channel and queues outbound messages.

pub mod link;
pub mod term;

pub use link::{HostLink, LinkEvent};
pub use term::{Effect, TermState, ViewRow};
