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
//! activates its owner before a click or key press when it is not active; the
//! worker's own desktop sees that app come to the front, which is the price of typing into it.
//!
//! Keys go by position and carry no text: the target's layout makes the character, the
//! client's own once the worker has taken the client's input source ([`sources`]). Text the
//! client composed itself goes as its own event, in pieces a key event can carry ([`text`]).
//! Caps Lock is set as a lock, and media keys go to the system as system-defined events.
//! Trackpad gestures (a pinch, a rotation, smart zoom, a swipe, and, while the tile sends its
//! gestures, the gesture a trackpad scroll comes with) have no public `CGEvent` constructor:
//! they are built as the trackpad's are, from a blank event and the fields AppKit reads
//! ([`backend`]). A press, its drags and its release carry one event number, as AppKit follows
//! a drag by it. A drag session a worker feeds for a drop goes through the HID tap for its
//! life, since the drag manager follows the real pointer ([`Injector::enter_drag`]), and a drag
//! resting there is nudged so spring-loaded targets spring ([`nudge`]). A left press on a window
//! stream whose window is on top at the point goes through the HID tap too, until its release, so
//! an app can begin a drag of its own from it; the release puts the real pointer back. The stream's
//! task carries such a drag as [`DragStep`]s ([`InputSink::drag`]).
//!
//! [`InputSink`] is the worker's input seam, compiled on every target; [`CgEvents`] is the
//! macOS implementation (`docs/decisions/topology.md`): the stream's [`Injector`] on an
//! [`InputThread`], so no window-server call is made on the stream's task. The target's bounds
//! come from the stream's geometry probe ([`InputSink::set_bounds`]), and where the input put the
//! pointer goes back to the stream's cursor samples ([`PointerWatch`]). [`pasteboard::Board`] is
//! the clipboard seam.

#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

#[cfg(target_os = "macos")]
pub mod backend;
#[cfg(target_os = "macos")]
mod injector;
#[cfg(target_os = "macos")]
pub mod keymap;
pub mod nudge;
pub mod pasteboard;
mod pointer;
pub mod sources;
#[cfg(target_os = "macos")]
pub mod text;
#[cfg(target_os = "macos")]
mod thread;

use std::time::Instant;

#[cfg(target_os = "macos")]
pub use backend::{Backend, Event, Gesture, Post, Recorder, Route, System};
#[cfg(target_os = "macos")]
pub use injector::{
    CapsClaims, CapsKept, DRAG_START, Injector, SharedCaps, can_post, flags_for, keep_caps,
    request_post, to_point,
};
#[cfg(target_os = "macos")]
pub use pasteboard::MacBoard;
pub use pasteboard::{Board, ClipFormat, board_type, format_of};
pub use pointer::{Pointer, PointerChanges, PointerWatch};
use slopty_capture::Rect;
use slopty_proto::screen::{CaptureTarget, ScreenInput};
#[cfg(target_os = "macos")]
pub use thread::{CgEvents, InputThread, let_go_everywhere};

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
    /// The stream's input thread is gone.
    #[error("input thread stopped")]
    Stopped,
    /// This platform injects no input (`docs/decisions/platform.md`, "Linux seams").
    #[error("input injection is unsupported on this platform")]
    Unsupported,
}

/// Where a screen stream's client input goes: the worker's input seam.
///
/// One per stream, owned by the stream's task. It maps the stream's pixels back to the
/// target's place on the worker's screens and delivers the event to it. The calls return
/// without waiting on the window server; an implementation that must wait does so elsewhere.
pub trait InputSink: Send + Sized + 'static {
    /// A sink for `target`, whose stream has `scale` pixels per display point.
    fn new(target: CaptureTarget, scale: f64) -> Self;
    /// The stream was re-scaled: `scale` stream pixels per display point from now on.
    fn set_scale(&mut self, scale: f64);
    /// The target's bounds in global display points as read at `at` (`None`: the window is
    /// gone): what the pointer events after this one map through. The stream's geometry probe
    /// hands every read over, so the sink need not read them in front of an event.
    fn set_bounds(&mut self, bounds: Option<Rect>, at: Instant);
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
    /// Let go of every key and button held down through this sink: the stream is ending.
    /// Dropping the sink does the same.
    fn release_all(&mut self);
    /// Where this sink's input puts the worker's pointer, readable from any thread.
    fn pointer(&self) -> PointerWatch;
    /// Carry a drag session for a drop from the client one step on, in the input's order. A
    /// step that fails is logged; [`DragStep::Enter`] answers whether it could.
    fn drag(&mut self, step: DragStep);
}

/// One step of a drag session a stream's task carries on the worker, for a drop from the client.
///
/// See `docs/decisions/audio.md`, "Drag and drop lands at the point, both ways". Points are in
/// the stream's pixels. The drag manager follows the real pointer, so from [`Self::Enter`] until
/// [`Self::Release`] or [`Self::Cancel`] every pointer event of the stream goes through the HID
/// tap, and a drag resting in one place is nudged on its own so spring-loaded targets spring
/// ([`nudge`]).
#[derive(Debug)]
pub enum DragStep {
    /// Begin feeding a drag through the HID tap, raising a window stream's window, and answer
    /// where `(x, y)` is on the worker's screens, in global points: where the drag helper puts
    /// its source window for [`Self::Press`].
    Enter {
        /// Where, in stream pixels.
        x: f32,
        /// Where.
        y: f32,
        /// The point in global points, or why there is none (the window is gone).
        answer: tokio::sync::oneshot::Sender<Result<(f64, f64), InputError>>,
    },
    /// Press at `(x, y)` and drag a little: the view under the press, the helper's source,
    /// begins its session from it.
    Press {
        /// Where, in stream pixels.
        x: f32,
        /// Where.
        y: f32,
    },
    /// Carry the drag to `(x, y)`.
    Move {
        /// Where, in stream pixels.
        x: f32,
        /// Where.
        y: f32,
    },
    /// Let go where the drag rests: the drop. The stream's own route is back after it.
    Release,
    /// End the drag with nothing dropped. The stream's own route is back after it. A drag out
    /// of an app that the client's own press holds ends the same way.
    Cancel,
    /// Answer where `(x, y)` is on the worker's screens, in global points, and nothing else:
    /// where the helper puts its catcher for a drag out of an app.
    Locate {
        /// Where, in stream pixels.
        x: f32,
        /// Where.
        y: f32,
        /// The point in global points, or why there is none.
        answer: tokio::sync::oneshot::Sender<Result<(f64, f64), InputError>>,
    },
}

impl DragStep {
    /// Answer a step that cannot be taken here.
    pub fn refuse(self, why: InputError) {
        if let Self::Enter { answer, .. } | Self::Locate { answer, .. } = self {
            let _gone = answer.send(Err(why));
        }
    }
}
