//! Client core, UI-toolkit agnostic.
//!
//! * [`term`] — [`term::TermState`]: applies `TermEvent`s to a screen + absolute line cache, tracks
//!   the viewport (scrolled or following), and says what to ask the host for.
//! * [`link`] — [`link::HostLink`]: one connection to a host; fans control and session streams into
//!   a single event channel and queues outbound messages.
//! * [`directory`] — [`directory::Directory`]: the worker directory the server keeps, as last heard
//!   (cached for degraded mode), and whether to dial each worker.
//! * [`server`] — [`server::spawn`]: the one link to the server, redialled after every drop.
//! * [`items`] — [`items::ItemDoc`]: one worker's item registry, worker-authoritative, applied
//!   optimistically.
//! * [`layout`] — [`layout::Layout`]: this device's scrollable tiling of every worker's items
//!   (workspaces of columns of tiles), with its springs and gestures; pure, clocked by the caller.
//! * [`screen`] — [`screen::ScreenHandle`]: one remote window stream, reassembled, decoded, and
//!   published as its newest frame plus the host's cursor position.
//! * [`pacing`] — [`pacing::Pacer`]: when a decoded frame goes on screen, and the arrival → present
//!   numbers the overlay and the tests read.

#![forbid(unsafe_code)]

pub mod directory;
pub mod items;
pub mod layout;
pub mod link;
pub mod pacing;
pub mod screen;
pub mod server;
pub mod term;

pub use items::{ItemChange, ItemDoc};
pub use link::{HostLink, LinkEvent, warm_up_decoder};
pub use pacing::{Clock, FrameStamp, Pace, Pacer, PacingStats, SystemClock};
pub use screen::{CursorState, Presentable, ScreenHandle, ScreenStats};
pub use term::{Effect, TermState, ViewRow};
