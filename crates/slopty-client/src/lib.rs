//! Client core, UI-toolkit agnostic.
//!
//! * [`term`] — [`term::TermState`]: applies `TermEvent`s to a screen + absolute line cache, tracks
//!   the viewport (scrolled or following), and says what to ask the worker for.
//! * [`link`] — [`link::WorkerLink`]: one connection to a worker; fans control and session streams
//!   into a single event channel and queues outbound messages.
//! * [`directory`] — [`directory::Directory`]: the worker directory the server keeps, as last heard
//!   (cached for degraded mode), and whether to dial each worker.
//! * [`server`] — [`server::spawn`]: the one link to the server, redialled after every drop.
//! * [`items`] — [`items::ItemDoc`]: one worker's item registry, worker-authoritative, applied
//!   optimistically.
//! * [`layout`] — [`layout::Layout`]: this device's scrollable tiling of every worker's items
//!   (workspaces of columns of tiles), with its springs and gestures; pure, clocked by the caller.
//! * [`xfer`] — files both ways: uploads of dropped files (resumed after a cut stream), downloads
//!   of a worker's files, and how a path is typed into a shell.
//! * [`clip`] — [`clip::ClipCache`]: the bytes of the worker's clipboard offer, fetched ahead or on
//!   paste, for a pasteboard provider that must answer before it returns.
//! * [`tunnel`] — [`tunnel::Forwards`]: a worker's listening ports served on this machine's
//!   loopback, each connection a tunnel stream.
//! * [`remote`] — [`remote::Remote`]: what the UI asks of a worker beyond the control stream.
//! * [`screen`] — [`screen::ScreenHandle`]: one remote window stream, reassembled, decoded, and
//!   published as its newest frame plus the worker's cursor position. Apple only: it decodes with
//!   `VideoToolbox`, so a Linux client (the CLI) has terminals and no screens.
//! * [`relay`] — [`relay::RelayWatch`]: whether a worker's link has stayed on a Tailscale DERP
//!   relay long enough to say so, and what to say.
//! * [`pacing`] — [`pacing::Pacer`]: when a decoded frame goes on screen, and the arrival → present
//!   numbers the overlay and the tests read.
//! * [`update`] — [`update::UpdateNotice`]: what to say of a worker or server on a different build,
//!   and the command that updates it.
//! * [`search`] — [`search::SearchResults`]: a text search on a worker as its pages come in, in
//!   path order, and the rows a results list draws.

#![forbid(unsafe_code)]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

pub mod clip;
pub mod directory;
pub mod items;
pub mod layout;
pub mod link;
pub mod pacing;
pub mod relay;
pub mod remote;
#[cfg(target_vendor = "apple")]
pub mod screen;
pub mod search;
pub mod server;
pub mod term;
pub mod tunnel;
pub mod update;
pub mod xfer;

pub use items::{ItemChange, ItemDoc};
#[cfg(target_vendor = "apple")]
pub use link::warm_up_decoder;
pub use link::{LinkEvent, WorkerLink};
pub use pacing::{Clock, FrameStamp, Pace, Pacer, PacingStats, SystemClock};
#[cfg(target_vendor = "apple")]
pub use screen::{CursorState, Presentable, ScreenHandle, ScreenStats};
pub use term::{Effect, TermState, ViewRow};
