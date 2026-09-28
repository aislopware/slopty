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

/// The platform this build serves: Linux streams no desktop yet.
#[cfg(not(target_os = "macos"))]
pub type Native = headless::Headless;

/// A platform with no desktop to stream: every capture, encode and input call fails as
/// unsupported.
///
/// Its worker advertises no display, encoder, capture or input (`caps`), so no
/// client asks for a stream; one that does anyway hears why it failed. Built on every target,
/// so it is checked and tested on the Mac.
pub mod headless {
    use std::sync::OnceLock;
    use std::time::Instant;

    use slopty_capture::{
        AudioSink, AxError, CaptureConfig, CaptureError, CaptureSource, CapturedFrame, Crop, Rect,
        TargetWindow, Went, WindowState,
    };
    use slopty_codec::{
        AudioEncoder, CodecError, EncodedPacket, EncoderConfig, FrameOptions, VideoEncoder,
    };
    use slopty_core::WindowId;
    use slopty_input::{InputError, InputSink, PointerWatch};
    use slopty_proto::screen::{CaptureTarget, CursorShape, DisplayInfo, ScreenInput, WindowInfo};

    use super::Platform;

    /// No capture, no encoders, no input.
    #[derive(Clone, Copy, Debug)]
    pub enum Headless {}

    impl Platform for Headless {
        type Audio = NoAudio;
        type Capture = NoCapture;
        type Input = NoInput;
        type Video = NoVideo;
    }

    /// Nothing is ever resolved, started, watched or encoded with, so none of these has a
    /// value.
    #[derive(Clone, Copy, Debug)]
    pub enum Never {}

    impl Never {
        /// What a call on a value that cannot exist returns.
        const fn absurd(self) -> ! {
            match self {}
        }
    }

    /// Capture that enumerates nothing and starts nothing.
    #[derive(Clone, Copy, Debug)]
    pub enum NoCapture {}

    impl CaptureSource for NoCapture {
        type Content = ();
        type HideWatch = Never;
        type Image = ();
        type Stream = Never;
        type Target = Never;

        fn can_capture() -> bool {
            false
        }

        fn enumerate(done: impl FnOnce(Result<(), CaptureError>) + Send + 'static) {
            done(Err(CaptureError::Unsupported));
        }

        fn windows((): &()) -> Vec<WindowInfo> {
            Vec::new()
        }

        fn displays((): &()) -> Vec<DisplayInfo> {
            Vec::new()
        }

        fn resolve((): &(), _kind: CaptureTarget) -> Result<Never, CaptureError> {
            Err(CaptureError::Unsupported)
        }

        fn resolve_crop((): &(), _id: WindowId) -> Result<Option<Never>, CaptureError> {
            Err(CaptureError::Unsupported)
        }

        fn crop(target: &Never) -> Option<Crop> {
            target.absurd()
        }

        fn pixel_size(target: &Never) -> (u32, u32) {
            target.absurd()
        }

        fn point_scale(target: &Never) -> f32 {
            target.absurd()
        }

        fn start(
            target: &Never,
            _config: &CaptureConfig,
            _sink: impl Fn(CapturedFrame<()>) + Send + Sync + 'static,
            _audio: Option<AudioSink>,
            _on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
            _done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
        ) -> Result<Never, CaptureError> {
            target.absurd()
        }

        fn update(
            stream: &Never,
            _config: &CaptureConfig,
            _done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
        ) {
            stream.absurd()
        }

        fn retarget(
            stream: &Never,
            _target: &Never,
            _done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
        ) {
            stream.absurd()
        }

        fn stop(stream: &Never, _done: impl FnOnce(Result<(), CaptureError>) + Send + 'static) {
            stream.absurd()
        }

        fn now_us() -> u64 {
            static EPOCH: OnceLock<Instant> = OnceLock::new();
            let since = EPOCH.get_or_init(Instant::now).elapsed().as_micros();
            u64::try_from(since).unwrap_or(u64::MAX)
        }

        fn target_bounds(_target: CaptureTarget) -> Option<Rect> {
            None
        }

        fn refresh_hz(_target: CaptureTarget) -> Option<f64> {
            None
        }

        fn window_state(_id: WindowId) -> Option<WindowState> {
            None
        }

        fn window_bounds(_id: WindowId) -> Option<Rect> {
            None
        }

        fn window_owner(_id: WindowId) -> Option<i32> {
            None
        }

        fn window_on_screen(_id: WindowId) -> bool {
            false
        }

        fn window_title(_id: WindowId) -> Option<String> {
            None
        }

        fn occluded(_id: WindowId, _bounds: &Rect, _owner: i32) -> bool {
            false
        }

        fn display_enclosing(_rect: &Rect) -> Option<u32> {
            None
        }

        fn display_bounds(_id: u32) -> Rect {
            Rect::default()
        }

        fn resize_window(
            _pid: i32,
            _target: &TargetWindow,
            _width: f64,
            _height: f64,
        ) -> Result<(), AxError> {
            Err(AxError::Unsupported)
        }

        fn watch_hides(
            _pid: i32,
            _target: TargetWindow,
            _on_went: impl Fn(Went) + Send + Sync + 'static,
        ) -> Result<Never, AxError> {
            Err(AxError::Unsupported)
        }

        fn watch_targeted(watch: &Never) -> bool {
            watch.absurd()
        }

        fn pointer_moves() -> u32 {
            0
        }

        fn pointer_location() -> (f64, f64) {
            (0.0, 0.0)
        }

        fn cursor_shape(_scale: u8) -> Option<CursorShape> {
            None
        }
    }

    /// A video encoder that cannot be made.
    #[derive(Clone, Copy, Debug)]
    pub struct NoVideo(Never);

    impl VideoEncoder for NoVideo {
        type Image = ();

        fn new(
            config: EncoderConfig,
            _sink: impl Fn(EncodedPacket) + Send + Sync + 'static,
        ) -> Result<Self, CodecError> {
            Err(CodecError::Unsupported(config.codec))
        }

        fn encode(&self, (): &(), _pts_us: u64, _options: &FrameOptions) -> Result<(), CodecError> {
            self.0.absurd()
        }

        fn set_bitrate(&self, _bps: u32) -> Result<(), CodecError> {
            self.0.absurd()
        }

        fn set_frame_rate(&self, _fps: u16) -> Result<(), CodecError> {
            self.0.absurd()
        }
    }

    /// An audio encoder that cannot be made.
    #[derive(Clone, Copy, Debug)]
    pub struct NoAudio(Never);

    impl AudioEncoder for NoAudio {
        fn new() -> Result<Self, CodecError> {
            Err(CodecError::NoAudio)
        }

        fn push(&mut self, _samples: &[f32], _out: impl FnMut(&[u8])) -> Result<(), CodecError> {
            self.0.absurd()
        }
    }

    /// Input that refuses every event.
    #[derive(Debug, Default)]
    pub struct NoInput {
        pointer: PointerWatch,
    }

    impl InputSink for NoInput {
        fn new(_target: CaptureTarget, _scale: f64) -> Self {
            Self::default()
        }

        fn set_scale(&mut self, _scale: f64) {}

        fn set_bounds(&mut self, _bounds: Option<Rect>, _at: Instant) {}

        fn inject(&mut self, _input: &ScreenInput) -> Result<(), InputError> {
            Err(InputError::Unsupported)
        }

        fn focus(&mut self) -> Result<(), InputError> {
            Err(InputError::Unsupported)
        }

        fn release_all(&mut self) {}

        fn pointer(&self) -> PointerWatch {
            self.pointer.clone()
        }
    }

    #[cfg(test)]
    mod tests {
        use slopty_core::DisplayId;

        use super::*;

        /// Asking a headless worker for its screens, an encoder or input is refused as
        /// unsupported, never answered with an empty success.
        #[test]
        fn a_headless_platform_refuses_every_desktop_call() {
            let (tx, rx) = std::sync::mpsc::channel();
            NoCapture::enumerate(move |got| tx.send(got.map(drop)).unwrap());
            assert!(matches!(rx.recv().unwrap(), Err(CaptureError::Unsupported)));
            assert!(matches!(
                NoCapture::resolve(&(), CaptureTarget::Display(DisplayId(1))),
                Err(CaptureError::Unsupported)
            ));
            assert!(!NoCapture::can_capture());
            assert!(matches!(NoAudio::new(), Err(CodecError::NoAudio)));
            let mut input = NoInput::new(CaptureTarget::Display(DisplayId(1)), 2.0);
            assert!(matches!(input.focus(), Err(InputError::Unsupported)));
        }
    }
}
