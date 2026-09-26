//! The worker's capture seam (`docs/decisions/topology.md`).
//!
//! [`CaptureSource`] and the plain types that cross it, compiled on every target. macOS
//! implements it with ScreenCaptureKit and the window server (`ScreenCaptureKit` in this
//! crate); another platform adds a module of its own and the worker core stays as it is.

use slopty_core::WindowId;
use slopty_proto::screen::{CaptureTarget, CursorShape, DisplayInfo, WindowInfo};

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
    NotFound(CaptureTarget),
    /// The stream stopped on its own (window closed, display unplugged, permission revoked).
    #[error("stream stopped: {0}")]
    Stopped(String),
}

/// A rectangle in global display points (origin top-left, y down, as `CGWindow` reports).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

impl Rect {
    /// Whether `(x, y)` lies inside.
    #[must_use]
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }

    /// Whether `other` lies entirely inside.
    #[must_use]
    pub fn encloses(&self, other: &Self) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.x + other.w <= self.x + self.w
            && other.y + other.h <= self.y + self.h
    }

    /// Whether the interiors overlap (touching edges do not count).
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }
}

/// The part of a display a stream samples: a window's frame in the display's own point space
/// (origin at the display's top-left corner), what `SCStreamConfiguration.sourceRect` wants.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Crop {
    /// Left edge, display points.
    pub x: f64,
    /// Top edge, display points.
    pub y: f64,
    /// Width, display points.
    pub w: f64,
    /// Height, display points.
    pub h: f64,
}

/// Where a window sits on a display, for the display-crop capture path.
///
/// Returns the crop in the display's point space and the output size in pixels at the
/// display's `scale`; `None` when the window is not entirely on that display (partly
/// off-screen, straddling two displays): a crop would show a slice of the desktop where the
/// window continues, so those keep the window filter.
#[must_use]
pub fn crop_for(window: &Rect, display: &Rect, scale: f64) -> Option<(Crop, (u32, u32))> {
    if window.w <= 0.0 || window.h <= 0.0 || scale <= 0.0 || !display.encloses(window) {
        return None;
    }
    let crop = Crop { x: window.x - display.x, y: window.y - display.y, w: window.w, h: window.h };
    let even = |points: f64| -> u32 {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped")]
        let px = (points * scale).round().clamp(2.0, 16_384.0) as u32;
        px.next_multiple_of(2)
    };
    Some((crop, (even(window.w), even(window.h))))
}

/// What the window list says about one window, read in one round trip.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct WindowState {
    /// Where it is, in global points.
    pub bounds: Rect,
    /// `kCGWindowIsOnscreen`: not minimised, not on another Space, not hidden.
    pub on_screen: bool,
    /// The process that owns it.
    pub owner_pid: i32,
}

/// Pixel layout of captured frames.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PixelFormat {
    /// 8-bit 4:2:0 bi-planar, full range (`420f`), BT.709 matrix: what the encoder takes and
    /// the client's decoder hands its Metal surface path unconverted.
    Nv12Full,
    /// 8-bit BGRA; for debugging and screenshots.
    Bgra,
}

/// Stream settings.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CaptureConfig {
    /// Output width in pixels (even).
    pub width: u32,
    /// Output height in pixels (even).
    pub height: u32,
    /// Frame rate ceiling; 0 captures at the display's own refresh, whatever it is
    /// (`minimumFrameInterval` = `kCMTimeZero`).
    pub fps: u16,
    /// Pixel layout.
    pub format: PixelFormat,
    /// Surfaces in ScreenCaptureKit's pool: the ones the stream and the encoder hold plus one to
    /// render the next capture into (the worker asks for 3).
    pub queue_depth: u8,
    /// Also capture the target's audio (48 kHz stereo, this process excluded).
    pub audio: bool,
    /// Sample only this part of the target (a window's frame on a display target); the
    /// whole target when `None`.
    pub crop: Option<Crop>,
}

/// Audio sample rate ScreenCaptureKit is asked for.
pub const AUDIO_RATE: u32 = 48_000;
/// Audio channels ScreenCaptureKit is asked for.
pub const AUDIO_CHANNELS: u32 = 2;

/// A run of audio from the stream: interleaved stereo float at [`AUDIO_RATE`].
#[derive(Debug)]
pub struct CapturedAudio {
    /// Presentation time on the host clock, microseconds.
    pub pts_us: u64,
    /// Interleaved L/R samples.
    pub samples: Vec<f32>,
}

/// Where audio goes; `None` leaves audio off.
pub type AudioSink = Box<dyn Fn(CapturedAudio) + Send + Sync>;

/// What a macOS frame carries: an `IOSurface`-backed pixel buffer.
#[cfg(target_os = "macos")]
pub type DefaultImage = slopty_codec::PixelBuffer;
/// What a frame carries where no capture is implemented yet.
#[cfg(not(target_os = "macos"))]
pub type DefaultImage = ();

/// A frame from the stream.
#[derive(Debug)]
pub struct CapturedFrame<I = DefaultImage> {
    /// The picture; on macOS `IOSurface`-backed, handed straight to the encoder.
    pub image: I,
    /// Capture time on the host clock (`CMClockGetHostTimeClock`), microseconds: the
    /// sample's presentation timestamp.
    pub capture_ts_us: u64,
    /// When the window server displayed the frame (`SCStreamFrameInfoDisplayTime`), on the
    /// same clock; `None` when the attachment is missing.
    pub display_ts_us: Option<u64>,
    /// Time between the presentation timestamp and this callback, microseconds.
    pub age_us: u64,
    /// Capture latency: display time → this callback, microseconds (falls back to `age_us`
    /// without a display time). What ScreenCaptureKit itself adds.
    pub latency_us: u64,
}

/// What the watch heard go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Went {
    /// The target: its element was destroyed or minimised, or the application was hidden. Also
    /// every window of the application when no element could be matched to the target.
    Target,
    /// Another window of the application; the target is untouched.
    Other,
}

/// The target as the window list describes it, for matching it to its accessibility element.
#[derive(Debug, Clone, PartialEq)]
pub struct TargetWindow {
    /// `kCGWindowBounds`: screen points, top-left origin, the same space as `AXPosition`.
    pub bounds: Rect,
    /// `kCGWindowName`, if the window has one.
    pub title: Option<String>,
}

/// Why a watch could not be started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AxError {
    /// The process is not trusted for accessibility; the API answers nothing without it.
    #[error("this process is not trusted for accessibility")]
    NotTrusted,
    /// The accessibility API refused (an `AXError` code: -25204 is "cannot complete", the
    /// usual answer for a process that is gone or not an application).
    #[error("accessibility observer failed with AXError {0}")]
    Observer(i32),
    /// The watch thread ended before it reported.
    #[error("the accessibility watch thread ended before it was ready")]
    Thread,
    /// The application lists no window with the target's frame and title.
    #[error("the application lists no window matching the target")]
    NoWindow,
    /// The accessibility API refused to set an attribute (an `AXError` code).
    #[error("accessibility attribute write failed with AXError {0}")]
    Attribute(i32),
}

/// Where a worker's pictures come from: the displays and windows it can stream, a stream of
/// frames from one of them, and what a window stream must know as its window moves, hides
/// or is covered.
///
/// A platform is a type with no values; every operation is an associated function, so a
/// stream generic over it compiles to direct calls. What ScreenCaptureKit hands back
/// (an enumeration, a resolved target, a live stream, an accessibility watch) is an
/// associated type the worker holds and passes back without looking inside.
pub trait CaptureSource: 'static {
    /// A captured picture, as the encoder takes it.
    type Image: Send + 'static;
    /// Everything shareable at one moment.
    type Content: Send + Sync + 'static;
    /// A display, a window, or a window as a crop of its display, resolved from a
    /// [`Self::Content`].
    type Target: Send + 'static;
    /// A running capture.
    type Stream: Send + Sync + 'static;
    /// A watch on a window's application that hears one of its windows go.
    type HideWatch: Send + 'static;

    /// Whether this process may capture the screen.
    fn can_capture() -> bool;

    /// Enumerate the shareable displays and windows; `done` runs once, on any thread.
    fn enumerate(done: impl FnOnce(Result<Self::Content, CaptureError>) + Send + 'static);
    /// The windows of an enumeration, for a listing.
    fn windows(content: &Self::Content) -> Vec<WindowInfo>;
    /// The displays of an enumeration, for a listing.
    fn displays(content: &Self::Content) -> Vec<DisplayInfo>;
    /// Resolve `kind` against `content`: a display, or a window through the window filter.
    ///
    /// # Errors
    ///
    /// The target is not in the enumeration.
    fn resolve(content: &Self::Content, kind: CaptureTarget) -> Result<Self::Target, CaptureError>;
    /// Resolve window `id` as a crop of the display it is on; `None` when it is not entirely on
    /// one display.
    ///
    /// # Errors
    ///
    /// The window or its display is not in the enumeration.
    fn resolve_crop(
        content: &Self::Content,
        id: WindowId,
    ) -> Result<Option<Self::Target>, CaptureError>;
    /// The crop a resolved target samples, `None` for a whole display or window.
    fn crop(target: &Self::Target) -> Option<Crop>;
    /// The target's native size in pixels.
    fn pixel_size(target: &Self::Target) -> (u32, u32);
    /// The target's pixels per point.
    fn point_scale(target: &Self::Target) -> f32;

    /// Start capturing `target`. Frames go to `sink` and audio to `audio` from the platform's
    /// own threads; `on_stop` hears the stream end by itself; `done` runs once it is live or
    /// has failed.
    ///
    /// # Errors
    ///
    /// The stream could not be built.
    fn start(
        target: &Self::Target,
        config: &CaptureConfig,
        sink: impl Fn(CapturedFrame<Self::Image>) + Send + Sync + 'static,
        audio: Option<AudioSink>,
        on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) -> Result<Self::Stream, CaptureError>;
    /// Change size, rate, format or crop on a live stream.
    fn update(
        stream: &Self::Stream,
        config: &CaptureConfig,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    );
    /// Swap what a live stream captures (window filter and display crop); the crop is
    /// changed separately with [`Self::update`].
    fn retarget(
        stream: &Self::Stream,
        target: &Self::Target,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    );
    /// Stop; `done` runs once the stream is torn down.
    fn stop(stream: &Self::Stream, done: impl FnOnce(Result<(), CaptureError>) + Send + 'static);
    /// Now on the clock frames are stamped with, microseconds.
    fn now_us() -> u64;

    /// The bounds of a display or window, in global points.
    fn target_bounds(target: CaptureTarget) -> Option<Rect>;
    /// The refresh rate of the display the target is drawn on, in hertz; `None` when it
    /// cannot be read.
    fn refresh_hz(target: CaptureTarget) -> Option<f64>;
    /// A window's bounds, on-screen state and owner, in one read; `None` once it is gone.
    fn window_state(id: WindowId) -> Option<WindowState>;
    /// A window's bounds.
    fn window_bounds(id: WindowId) -> Option<Rect>;
    /// The process that owns a window.
    fn window_owner(id: WindowId) -> Option<i32>;
    /// Whether a window is on screen: not minimised, not on another desktop, not hidden.
    fn window_on_screen(id: WindowId) -> bool;
    /// A window's title, if it has one.
    fn window_title(id: WindowId) -> Option<String>;
    /// Whether something that is not the window's own content covers part of it.
    fn occluded(id: WindowId, bounds: &Rect, owner: i32) -> bool;
    /// The display that holds all of `rect`.
    fn display_enclosing(rect: &Rect) -> Option<u32>;
    /// A display's bounds, in global points.
    fn display_bounds(id: u32) -> Rect;
    /// Give the window of process `pid` that matches `target` a size in points. Blocking.
    ///
    /// # Errors
    ///
    /// The window could not be found or would not be resized.
    fn resize_window(
        pid: i32,
        target: &TargetWindow,
        width: f64,
        height: f64,
    ) -> Result<(), AxError>;
    /// Watch the application `pid` for `target` or another of its windows going; `on_went`
    /// runs on the watch's own thread. Blocking.
    ///
    /// # Errors
    ///
    /// The application cannot be observed.
    fn watch_hides(
        pid: i32,
        target: TargetWindow,
        on_went: impl Fn(Went) + Send + Sync + 'static,
    ) -> Result<Self::HideWatch, AxError>;
    /// Whether a watch told the target apart from the application's other windows.
    fn watch_targeted(watch: &Self::HideWatch) -> bool;

    /// A counter that moves whenever the pointer does; cheap enough to read every tick.
    fn pointer_moves() -> u32;
    /// Where the pointer is, in global points. A window-server round trip.
    fn pointer_location() -> (f64, f64);
    /// The cursor's picture at `scale` pixels per point, when there is one to read.
    fn cursor_shape(scale: u8) -> Option<CursorShape>;
}
