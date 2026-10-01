//! Drag and drop at a point on a worker (`docs/decisions/audio.md`, "Drag and drop lands at the
//! point, both ways").
//!
//! The drag helper's roles, which the worker's helper process (`slopty-worker dnd`, an
//! accessory `NSApplication` apart from the process that serves the streams) runs on its main
//! thread:
//!
//! - [`source`]: for a drop from the client, a few points of window at the drop's point, which the
//!   worker presses into through the HID tap to begin a real drag of the client's items ([`items`]:
//!   whole files by URL, everything else promised until the target reads it).
//! - [`catcher`]: for a drag out of an app on the worker, a window the worker lets the drag go onto
//!   once the client's pointer has left the tile, taking files as references, promises into a
//!   folder and data whole.
//! - [`watch`]: seeing a drag begin, by the drag pasteboard's change count.
//! - [`operation`]: what the target under a drag would do, read off the system cursor.
//!
//! [`helper`] is the process that runs them, talking to the worker over its stdin and stdout.
//!
//! The live tests that settle the platform's behaviour (`tests/spikes.rs`, P0) and prove these
//! roles (`tests/roles.rs`) run only in a macOS guest (`cargo xtask vm live -p slopty-dnd`), with
//! their own apps under `tests/support/`.

#![warn(unreachable_pub)]

#[cfg(target_os = "macos")]
pub mod catcher;
#[cfg(target_os = "macos")]
pub mod helper;
#[cfg(target_os = "macos")]
pub mod items;
#[cfg(target_os = "macos")]
pub mod operation;
#[cfg(target_os = "macos")]
pub mod source;
#[cfg(target_os = "macos")]
pub mod watch;
#[cfg(target_os = "macos")]
pub mod window;
