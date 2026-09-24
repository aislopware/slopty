//! The platform a worker runs on, as the seams its screen streams use.
//!
//! The seams (`docs/decisions/topology.md`) are capture, the video and audio encoders, and
//! input. Each is a trait of its platform crate, and a [`Platform`] names one implementation of
//! each. A screen stream is generic over it, so every call on the frame path is a direct one.
//!
//! The PTY needs no seam (both targets are Unix), and the clipboard's is
//! [`slopty_input::pasteboard::Board`], which [`crate::clip::Clipboard`] takes directly.

use slopty_capture::CaptureSource;
use slopty_codec::{AudioEncoder, VideoEncoder};
use slopty_input::InputSink;

/// One implementation of every seam a screen stream uses.
pub trait Platform: 'static {
    /// Displays, windows and frames.
    type Capture: CaptureSource;
    /// The video encoder, which takes the capture's pictures as they come.
    type Video: VideoEncoder<Image = <Self::Capture as CaptureSource>::Image>;
    /// The audio encoder.
    type Audio: AudioEncoder;
    /// Where a stream's client input goes.
    type Input: InputSink;
}

/// macOS: ScreenCaptureKit, VideoToolbox, Opus through `AudioConverter`, and `CGEvent`s.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug)]
pub enum MacOs {}

#[cfg(target_os = "macos")]
impl Platform for MacOs {
    type Audio = slopty_codec::Opus;
    type Capture = slopty_capture::ScreenCaptureKit;
    type Input = slopty_input::CgEvents;
    type Video = slopty_codec::VideoToolbox;
}

/// The platform this build serves.
#[cfg(target_os = "macos")]
pub type Native = MacOs;
