//! Slopty's File Provider extension: each worker's home in Finder, as a place of its own whose
//! files come down from the worker as they are read (`docs/decisions/platform.md`).
//!
//! The system runs the extension whether the app is open or not, so it reaches the workers
//! itself, at the addresses the app writes to the container the two share
//! (`slopty_platform::files`). [`item`] names a worker's files as the system does, [`worker`]
//! is the extension's link to one worker, [`changes`] keeps what the system was told so the
//! changes the worker reports can follow, and [`domain`] holds one worker's domain together.
//! The Objective-C face the system calls is `extension`, on macOS.

pub mod changes;
#[cfg(target_os = "macos")]
pub mod domain;
#[cfg(target_os = "macos")]
pub mod extension;
pub mod item;
#[cfg(target_os = "macos")]
pub mod worker;
