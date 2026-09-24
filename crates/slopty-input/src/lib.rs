//! Remote-window input on the worker: turn a client's [`ScreenInput`] into `CGEvent`s.
//!
//! One [`Injector`] per screen stream. It knows the stream's target and its pixels-per-point
//! scale, maps stream pixels back to global display points, and decides what to post and
//! where: straight to the owning process (`CGEventPostToPid`, window streams: the window need
//! not be frontmost, nothing on the worker's own desktop moves) or to the HID event tap
//! (display streams: the whole screen is the target, so the real pointer follows). Posting
//! itself is a [`Backend`]: [`System`] for the daemon, [`Recorder`] for tests, so every
//! decision here is unit-tested without Accessibility access and without a real event.
//! Posting needs the worker process to be granted *Accessibility* (post-event access);
//! [`can_post`] and [`request_post`] wrap the preflight and prompt.
//!
//! macOS only delivers keyboard events to the *active* application: events posted to an
//! inactive pid queue up until it is activated (observed macOS 26.5). So a window stream
//! activates its owner before the first click or key press after it lost activation; the
//! worker's own desktop sees that app come to the front, which is the price of typing into it.
//!
//! Magnify gestures have no public `CGEvent` constructor and are ignored.
//!
//! [`InputSink`] is the worker's input seam, compiled on every target; [`CgEvents`] is the
//! macOS implementation (`docs/decisions/topology.md`). [`pasteboard::Board`] is the clipboard
//! seam.

#[cfg(target_os = "macos")]
pub mod backend;
#[cfg(target_os = "macos")]
mod injector;
#[cfg(target_os = "macos")]
pub mod keymap;
pub mod pasteboard;

#[cfg(target_os = "macos")]
pub use backend::{Backend, Event, Post, Recorder, Route, System};
#[cfg(target_os = "macos")]
pub use injector::{CgEvents, Injector, can_post, flags_for, request_post, to_point};
#[cfg(target_os = "macos")]
pub use pasteboard::MacBoard;
pub use pasteboard::{Board, Rep};
use slopty_proto::screen::{CaptureTarget, ScreenInput};

/// What went wrong posting an event.
#[derive(Clone, Copy, thiserror::Error, Debug)]
pub enum InputError {
    /// The target window is gone (no bounds in the window list).
    #[error("target has no bounds; window closed?")]
    NoBounds,
    /// `CGEventCreate*` returned null.
    #[error("CGEvent creation failed")]
    Create,
    /// The owning application could not be found for `Focus`.
    #[error("owning application not running")]
    NoApplication,
}

/// Where a screen stream's client input goes: the worker's input seam.
///
/// One per stream, owned by the stream's task. It maps the stream's pixels back to the
/// target's place on the worker's screens and delivers the event to it.
pub trait InputSink: Send + Sized + 'static {
    /// A sink for `target`, whose stream has `scale` pixels per display point.
    fn new(target: CaptureTarget, scale: f64) -> Self;
    /// The stream was re-scaled: `scale` stream pixels per display point from now on.
    fn set_scale(&mut self, scale: f64);
    /// Deliver one input event.
    ///
    /// # Errors
    ///
    /// The target is gone, or the system would not take the event.
    fn inject(&mut self, input: &ScreenInput) -> Result<(), InputError>;
    /// Give the target's application keyboard focus.
    ///
    /// # Errors
    ///
    /// The application is gone.
    fn focus(&mut self) -> Result<(), InputError>;
}
