//! Screen capture for the worker: enumerate windows and displays, stream one of them as
//! pictures ready for the encoder.
//!
//! [`CaptureSource`] is the worker's capture seam and compiles everywhere ([`source`]). On
//! macOS, [`ScreenCaptureKit`] implements it with ScreenCaptureKit, the window list and the
//! accessibility API, streaming `IOSurface`-backed pixel buffers.
//!
//! Everything is callback driven and thread-agnostic. ScreenCaptureKit runs its own queues;
//! completion handlers and frames arrive on them, and every callback type here is `Send`.
//! Frames skip the `Idle` status (nothing changed), so an unchanged screen costs nothing.

#[cfg(target_os = "macos")]
mod ax;
#[cfg(target_os = "macos")]
mod content;
#[cfg(target_os = "macos")]
mod cursor;
#[cfg(target_os = "macos")]
mod geometry;
#[cfg(target_os = "macos")]
mod sck;
#[cfg(target_os = "macos")]
mod snapshot;
pub mod source;
#[cfg(target_os = "macos")]
mod stream;

#[cfg(target_os = "macos")]
pub use ax::{HideWatch, resize_window};
#[cfg(target_os = "macos")]
pub use content::{Shareable, enumerate};
#[cfg(target_os = "macos")]
pub use cursor::{AlphaAt, Layout, bgra_premultiplied, cursor_shape, warm_cursor};
#[cfg(target_os = "macos")]
pub use geometry::{
    Above, can_capture, counts_as_occluder, display_bounds, display_enclosing, occluded, occluders,
    pointer_location, pointer_moves, request_capture, target_bounds, window_bounds,
    window_on_screen, window_owner_pid, window_state, window_title,
};
#[cfg(target_os = "macos")]
pub use sck::ScreenCaptureKit;
#[cfg(target_os = "macos")]
pub use snapshot::{Picture, PixelOrder};
pub use source::{
    AUDIO_CHANNELS, AUDIO_RATE, AudioSink, AxError, CaptureConfig, CaptureError, CaptureSource,
    CapturedAudio, CapturedFrame, Crop, DefaultImage, PixelFormat, Rect, TargetWindow, Went,
    WindowState, crop_for,
};
#[cfg(target_os = "macos")]
pub use stream::{Capture, SckDefaults, Target, host_now_us, sck_defaults};

#[cfg(target_os = "macos")]
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
#[cfg(target_os = "macos")]
pub(crate) fn ensure_core_graphics() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _main = objc2_core_graphics::CGMainDisplayID();
    });
}
