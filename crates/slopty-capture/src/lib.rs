//! ScreenCaptureKit for the host: enumerate windows and displays, stream one of them as
//! IOSurface-backed pixel buffers ready for the encoder.
//!
//! Everything is callback driven and thread-agnostic. ScreenCaptureKit runs its own queues;
//! completion handlers and frames arrive on them, and every callback type here is `Send`.
//! Frames skip the `Idle` status (nothing changed), so an unchanged screen costs nothing.

#![cfg(target_os = "macos")]

mod content;
mod geometry;
mod stream;

pub use content::{Shareable, enumerate};
pub use geometry::{
    Rect, can_capture, pointer_location, target_bounds, window_bounds, window_owner_pid,
};
pub use stream::{
    AUDIO_CHANNELS, AUDIO_RATE, AudioSink, Capture, CaptureConfig, CapturedAudio, CapturedFrame,
    PixelFormat, Target, host_now_us,
};

/// Capture failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    /// ScreenCaptureKit reported an error (`SCStreamErrorDomain` codes; -3801 is "user
    /// declined", i.e. no Screen Recording permission).
    #[error("ScreenCaptureKit: {message} (code {code})")]
    Sck {
        /// `NSError.code`.
        code: i64,
        /// `NSError.localizedDescription`.
        message: String,
    },
    /// The window or display is not in the shareable content list.
    #[error("no such capture target: {0:?}")]
    NotFound(slopty_proto::screen::CaptureTarget),
    /// The stream stopped on its own (window closed, display unplugged, permission revoked).
    #[error("stream stopped: {0}")]
    Stopped(String),
}

impl CaptureError {
    fn from_ns(error: &objc2_foundation::NSError) -> Self {
        Self::Sck {
            code: i64::try_from(error.code()).unwrap_or(i64::MIN),
            message: error.localizedDescription().to_string(),
        }
    }
}

/// Connect the process to the WindowServer once. `SCContentFilter` init calls into SkyLight,
/// which asserts (`CGS_REQUIRE_INIT`) if nothing has initialised CoreGraphics yet; that is the
/// case in a daemon whose first ScreenCaptureKit call is a window filter, not a display query.
pub(crate) fn ensure_core_graphics() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _main = objc2_core_graphics::CGMainDisplayID();
    });
}
