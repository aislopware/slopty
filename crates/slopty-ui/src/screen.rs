//! `ScreenView`: one remote window or display, painted from the newest decoded frame.
//!
//! The frame arrives as a `slopty_client::Presentable`: an IOSurface-backed `CVPixelBuffer`
//! plus the instants that got it here. GPUI's `surface` element samples it through
//! `CVMetalTextureCache`, so nothing is copied on the client, and a `slopty_client::Pacer`
//! decides when it goes up (present on arrival, never a queue) and measures how long the
//! journey took. The pointer is drawn here in the worker's cursor picture: at this client's own
//! pointer while this client drives it (always on a window stream), else where the cursor
//! channel says the worker's is. Pointer, scroll and key events inside the view go to the worker
//! as `ScreenInput` in stream pixels; the worker injects them. ⌘ chords the workspace binds
//! (⌘T/⌘O/⌘W, the text size) never reach the view because GPUI runs key bindings before key
//! listeners; every other chord (⌘C, ⌘V, ⌘Z, ⌘S…) is forwarded to the remote window. The view also
//! asks the worker for a smaller stream when it is painted small (a narrow column, the overview),
//! quantised so the encoder is not rebuilt on every step.
//!
//! On a phone or an iPad the picture zooms inside its tile: a pinch magnifies it about the
//! fingers from fit to twice one-to-one, two fingers pan it, and a double tap goes between fit
//! and one to one (`zoom`). A zoomed picture asks the worker for the scale it is drawn at, so it
//! sharpens as it grows. Trackpad mode turns the fingers into an iPad trackpad over the remote
//! pointer (`touch`). Every point sent to the worker goes through the zoom, so a tap lands on
//! the pixel under the finger.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use core_foundation::base::TCFType as _;
use core_video::pixel_buffer::CVPixelBuffer;
use gpui::{
    Animation, AnimationExt as _, App, Autocapitalize, Bounds, Context, CursorStyle,
    ElementInputHandler, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, KeyUpEvent, Keystroke, LongPressEvent,
    Modifiers, ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ObjectFit, ParentElement as _, Path, PathBuilder, PinchEvent, Pixels, Point, Render,
    RenderImage, ScrollDelta, ScrollWheelEvent, SharedString, Size,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TextInputAction,
    TextInputConfiguration, TouchDragEvent, TouchPhase, UTF16Selection, Window, canvas, div, point,
    px, relative, size, surface,
};
use slopty_client::pacing::{Pace, Pacer, PacingStats};
use slopty_client::{CursorState, Presentable, ScreenHandle, ScreenStats};
use slopty_core::StreamId;
use slopty_proto::ClientMsg;
use slopty_proto::input::{KeyAction, KeyCode, Mods, MouseButton as ProtoButton};
use slopty_proto::screen::{
    CaptureTarget, Chroma, CursorShape, Quality, RateVerdict, ScreenInput, ScreenRequest,
    ScrollPhase, SourceState, VideoCodec,
};
use slopty_theme::Theme;
use tokio::sync::{Notify, mpsc, watch};

use crate::colors::hsla;
use crate::{keys, kit};

mod health;
mod touch;
mod zoom;

pub use health::{Figure, Health, RTT_WARN_FROM};
pub use zoom::Zoom;

#[expect(clippy::derive_partial_eq_without_eq, reason = "gpui::actions! derives PartialEq only")]
mod actions {
    gpui::actions!(
        screen,
        [
            /// Turn the focused remote picture's fingers into a trackpad, or back.
            ToggleTrackpad,
        ]
    );
}
pub use actions::ToggleTrackpad;

/// What the header's trackpad control and the palette's command call trackpad mode: one name,
/// on or off.
pub const TRACKPAD_MODE: &str = "Trackpad mode";

/// The most of the clipboard "Type the clipboard" types, in bytes: a password or a line for a
/// field that refuses paste, not a document keyed in at typing speed.
pub const TYPE_MAX: usize = 1024;

/// Characters typed at once: two events each, well inside the outbox.
const TYPE_BURST: usize = 64;

/// The wait between bursts of typing: a frame.
const TYPE_STEP: Duration = Duration::from_millis(16);

/// How long the zoom readout stays before it fades.
const READOUT_HOLD: Duration = Duration::from_millis(700);

/// Whether fingers are this device's pointer: a double tap zooms, two fingers tapped
/// right-click, and trackpad mode is offered in the tile's header.
const TOUCH: bool = cfg!(target_os = "ios");

/// The zoom readout: up, or fading out under the generation that started the fade.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Readout {
    Shown,
    Fading(u64),
}

/// Asked when a paste chord goes to the worker: what must reach it first.
pub type PasteHook = Rc<dyn Fn() -> PasteAhead>;

/// What a paste chord needs ahead of it.
#[derive(Debug, Default)]
pub struct PasteAhead {
    /// This client's clipboard offer, when the worker has not heard it yet
    /// (`crate::clipboard::ClipSync::offer_for`); it goes on the control stream first.
    pub offer: Option<ClientMsg>,
    /// Files on the clipboard: they go to the worker's staging and its pasteboard before the
    /// chord does, so the window's input waits ([`ScreenViewEvent::PasteFiles`]).
    pub files: Option<crate::clipboard::ClipFiles>,
}

/// Makes a client-side stream for an `Opened` event (wraps `WorkerLink::screen`).
pub type ScreenFactory = Arc<dyn Fn(StreamId, VideoCodec) -> ScreenHandle + Send + Sync>;

/// Quality change rate limit.
const QUALITY_COOLDOWN: Duration = Duration::from_millis(400);
/// Smallest scale the view asks for.
const MIN_SCALE: f32 = 0.25;
/// Messages the outbox holds while the outbound queue is full, past which input that lets go of
/// nothing is dropped (moves never count: they coalesce).
const OUTBOX_DEPTH: usize = 256;

/// Things the workspace may react to.
#[derive(Clone, Debug)]
pub enum ScreenViewEvent {
    /// First frame painted.
    Ready,
    /// Pressed: the workspace should make this tile active. (The view stops the mouse event,
    /// which keeps it from the tile's own activate handler.)
    Pressed,
    /// The stream's health changed ([`ScreenView::health`]): the header's mark follows.
    Health,
    /// A paste of files: the view holds its input, the paste chord first, until
    /// [`ScreenView::release_paste`] once the files are on the worker's pasteboard.
    PasteFiles(crate::clipboard::ClipFiles),
}

/// What the worker answered to `Open`, plus what we asked for.
#[derive(Clone, Copy, Debug)]
pub struct Opened {
    /// Stream id.
    pub stream: StreamId,
    /// Target.
    pub target: CaptureTarget,
    /// Stream pixel size.
    pub size: (u32, u32),
    /// Requested quality.
    pub quality: Quality,
}

/// What is drawn at the worker's pointer position: the client's own arrow until the worker has
/// said which cursor it shows, then that picture (`ScreenEvent::Cursor`).
#[derive(Clone)]
enum Pointer {
    /// A drawn arrow, the same on every worker.
    Arrow,
    /// The worker's cursor picture, its size and hotspot in points.
    Image {
        /// Premultiplied BGRA, as GPUI keeps images.
        image: Arc<RenderImage>,
        /// Size in points (the picture's pixels over its backing scale).
        size: Size<Pixels>,
        /// The pixel that sits on the pointer position, in points from the top left.
        hot: Point<Pixels>,
    },
}

impl Pointer {
    /// The worker's cursor picture as a pointer, or the arrow when there is none or its bytes
    /// do not fill its size.
    fn from_shape(shape: Option<CursorShape>) -> Self {
        let Some(shape) = shape else { return Self::Arrow };
        let Some(buffer) =
            image::RgbaImage::from_raw(u32::from(shape.w), u32::from(shape.h), shape.bgra)
        else {
            return Self::Arrow;
        };
        let scale = f32::from(shape.scale.max(1));
        let image = Arc::new(RenderImage::new([image::Frame::new(buffer)]));
        Self::Image {
            image,
            size: size(px(f32::from(shape.w) / scale), px(f32::from(shape.h) / scale)),
            hot: point(px(f32::from(shape.hot_x) / scale), px(f32::from(shape.hot_y) / scale)),
        }
    }
}

impl Pointer {
    /// Free the picture's texture: each cursor the worker shows is a new image.
    fn drop_image(&self, cx: &mut App) {
        if let Self::Image { image, .. } = self {
            cx.drop_image(Arc::clone(image), None);
        }
    }
}

/// Where a cursor picture goes: its hotspot on `at`.
fn pointer_bounds(at: Point<Pixels>, size: Size<Pixels>, hot: Point<Pixels>) -> Bounds<Pixels> {
    Bounds { origin: point(at.x - hot.x, at.y - hot.y), size }
}

/// The drawn arrow, its tip at the origin: an outline in the canvas colour under a fill in the
/// text colour. Tessellated once per thread and moved into place for each paint.
struct Arrow {
    outline: Path<Pixels>,
    fill: Path<Pixels>,
}

impl Arrow {
    fn build() -> Option<Self> {
        let arrow = |inset: f32, scale: f32| {
            let mut path = PathBuilder::fill();
            let at =
                |x: f32, y: f32| point(px(x.mul_add(scale, inset)), px(y.mul_add(scale, inset)));
            path.move_to(at(0.0, 0.0));
            path.line_to(at(0.0, 16.0));
            path.line_to(at(4.0, 12.5));
            path.line_to(at(7.0, 18.5));
            path.line_to(at(9.5, 17.5));
            path.line_to(at(6.5, 11.5));
            path.line_to(at(11.5, 11.5));
            path.close();
            path.build().ok()
        };
        Some(Self { outline: arrow(-1.0, 1.15)?, fill: arrow(0.0, 1.0)? })
    }

    /// The arrow's extent, its tip at the origin.
    const fn bounds(&self) -> Bounds<Pixels> {
        self.outline.bounds
    }
}

thread_local! {
    static ARROW: Option<Rc<Arrow>> = Arrow::build().map(Rc::new);
}

/// `path` moved by `by`.
fn moved(path: &Path<Pixels>, by: Point<Pixels>) -> Path<Pixels> {
    let mut path = path.clone();
    path.bounds.origin += by;
    for vertex in &mut path.vertices {
        vertex.xy_position += by;
    }
    path
}

/// How long after this client last put a display's pointer somewhere its own point is drawn
/// rather than the worker's sample, on top of the round trip. The samples that come back in
/// that time are the echo of this client's moves, a round trip old. After it, a sample is where
/// the pointer went without this client (another user, an app's warp).
const LOCAL_HOLD: Duration = Duration::from_millis(200);

/// Half a 60 Hz refresh: how late past its due time a frame may be before a pointer change that
/// waits for it is drawn on its own.
const FOLD: Duration = Duration::from_micros(8_333);

/// How long a fling may go quiet before the worker is told its momentum ended. Both platforms say
/// so themselves, so this is the backstop for a lost or dropped close. Momentum events arrive
/// about a frame apart, which makes this several frames of silence.
const MOMENTUM_GAP: Duration = Duration::from_millis(120);

/// Where a scroll gesture over the picture has got to.
///
/// gpui reads `NSEvent.phase` and never `momentumPhase`, so a fling's momentum reaches us as
/// plain `Moved` events *after* `Ended`. Forwarding those as more of the same gesture tells the
/// remote app the scroll ended and then went on changing, which is not a thing macOS does. iOS
/// coasts through its own touch recognizer, which sends the same run of `Moved` and then closes
/// it with a second `Ended`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Scrolling {
    /// No gesture in flight. A mouse wheel notch never leaves this state.
    #[default]
    Idle,
    /// Between `Started` and `Ended`: the fingers are down.
    Fingers,
    /// After `Ended`: everything further is momentum, `begun` once the first has been sent.
    Momentum { begun: bool },
}

/// One stream on screen.
pub struct ScreenView {
    stream: StreamId,
    target: CaptureTarget,
    handle: ScreenHandle,
    /// The newest frame's picture. The wrapper holds its own retain on the decoder's buffer, so
    /// the frame it came in needs no keeping.
    latest: Option<CVPixelBuffer>,
    /// How much colour the newest picture carries, read off its pixel format; `None` before
    /// the first.
    chroma: Option<Chroma>,
    /// Stream size in pixels as opened.
    size: (u32, u32),
    /// Native pixel size of the target (stream size at scale 1).
    native: (f32, f32),
    quality: Quality,
    quality_changed: Instant,
    /// The painted width a change asked for inside the cooldown, taken when it ends: without
    /// it a window left at a small scale by the overview stays there until something repaints.
    wanted_width: Option<f32>,
    /// The worker's last cursor sample.
    cursor: CursorState,
    /// The worker's cursor picture, drawn wherever the pointer is drawn.
    pointer: Pointer,
    /// When this client last put the worker's pointer somewhere (a move, a press or a release
    /// over the picture), on the executor's clock.
    placed: Option<Instant>,
    /// Where the last render drew the pointer, in fractions of the picture; `None` when it drew
    /// none.
    drawn: Option<(f32, f32)>,
    /// When the last frame went up, on the executor's clock.
    last_frame: Option<Instant>,
    /// When a pointer change waiting for a frame to carry it is drawn on its own.
    fold: Option<Instant>,
    /// The picture's accessible label.
    label: SharedString,
    /// Renders so far.
    renders: u32,
    /// The stream size the worker maps input with: the size last asked for, or last told by
    /// `Geometry`. The worker takes a new scale in order with the input behind it, so frames
    /// still in flight at the old scale must not move it (unlike `size`, the picture's).
    mapped: (u32, u32),
    out: Outbox,
    theme: Theme,
    focus: FocusHandle,
    bounds: Bounds<Pixels>,
    frames: u64,
    /// When the painted rate was last worked out, the count then, and the rate: the status bar
    /// reads it every draw, and it is worked out at most once a second.
    fps_sample: std::cell::Cell<(Instant, u64, f32)>,
    /// Keys whose press went to the worker, so a release for a locally-handled chord (its press
    /// was eaten by a workspace binding) is not forwarded as a stray key-up.
    held: Vec<KeyCode>,
    /// Buttons whose press went to the worker. gpui reports a release only over the element that
    /// saw the press, so one let go elsewhere, or never seen at all (focus gone mid-drag), is
    /// released from here: a button left down turns every later hover into a drag.
    buttons: Vec<ProtoButton>,
    /// Where the pointer was last sent, in stream pixels: where it is drawn while this client
    /// drives it, and where a button is let go when the real release has no position on the
    /// picture.
    pointer_at: (f32, f32),
    /// Asked before a paste chord goes, so the worker pastes what this client copied.
    paste_hook: Option<PasteHook>,
    /// Pastes of files still on their way to the worker, and the input held behind them.
    paste_hold: (u32, Vec<ScreenInput>),
    /// Modifiers armed by the phone key bar; applied to the next key, then cleared.
    sticky: Modifiers,
    /// The modifier keys as last reported, so a change forwards the key that moved.
    modifiers: Modifiers,
    /// Focus leaving the view and the window going inactive each release what is held on the
    /// worker, and the window moving to another screen asks for that screen's refresh. They are
    /// registered with the window the view renders in (the constructor has none), and again
    /// when it renders in another: a tile popped out into a window of its own.
    let_go: Option<(gpui::AnyWindowHandle, [Subscription; 3])>,
    /// The screen the view's window is on, and its refresh in hertz (0 when it does not say);
    /// `None` until the view renders.
    screen: Option<(Option<u32>, u16)>,
    /// Input-method composition in progress (nothing is sent until it commits).
    marked: Option<String>,
    /// The stats overlay (⌘⇧I).
    hud: Option<Hud>,
    /// The overlay shows its engineering lines under the plain one.
    hud_details: bool,
    /// Reads the counters once a second for the header's health mark.
    probe: health::Probe,
    /// What is wrong with the stream, as last read.
    health: Option<Health>,
    _health: Task<()>,
    /// Decides when a decoded frame goes up and measures arrival → present. The element only
    /// feeds it: a frame on one side, a paint on the other.
    pacer: Pacer,
    /// Link RTT from the workspace, for the overlay.
    rtt: Option<Duration>,
    /// Holds the device out of idle sleep (the Mac) or its screen on (the phone) for as long as
    /// this window streams; dropping the view lets go.
    _awake: Task<()>,
    /// On the Mac, GPUI's hold is the system's only: this one keeps the display on too, since
    /// a viewer watching a remote window is not touching the keyboard. `None` under test, which
    /// must not keep this machine's display awake (and whose first activity costs ~17 s in a
    /// fresh process); GPUI's hold is the one a test sees.
    #[cfg(target_os = "macos")]
    _display: Option<slopty_platform::Activity>,
    /// The worker's last bitrate decision (`ScreenEvent::Rate`): target, verdict, cwnd-capped.
    rate: Option<(u32, RateVerdict, bool)>,
    /// What the worker says its capture target is doing (`ScreenEvent::Source`). A target that
    /// has drawn nothing is not a broken stream, and the placeholder should not claim it is.
    source: SourceState,
    /// Where the scroll gesture over the picture has got to, so the worker is sent the phases
    /// macOS would have sent rather than the ones gpui reports.
    scrolling: Scrolling,
    /// Fires [`MOMENTUM_GAP`] after the last momentum event to close the fling. Replaced (so
    /// cancelled) by every event that keeps it alive.
    momentum_end: Option<Task<()>>,
    /// How the picture is drawn over the body: kept while the view lives, so per tile.
    zoom: Zoom,
    /// The body's width in device pixels at fit, as the workspace last reported it.
    painted: f32,
    /// The window's backing scale as last drawn, for one to one.
    scale_factor: f32,
    /// Two fingers on the picture, from the pinch recognizer.
    two: Option<touch::Two>,
    /// Whether a two-finger drag has begun a scroll on the worker (trackpad mode).
    two_scrolling: bool,
    /// Trackpad mode: the pointer the fingers move, while it is on.
    trackpad: Option<touch::Trackpad>,
    /// The system's own shortcuts (⌘Tab, ⌘Space, Mission Control) go to the worker while
    /// this tile has the keyboard: the person turned it on for this tile.
    system_keys: bool,
    /// What "Type the clipboard" has still to type, and the task typing it.
    typing: VecDeque<char>,
    typer: Option<Task<()>>,
    /// Fingers are the pointer here (see [`TOUCH`]); tests turn it on.
    touch: bool,
    /// The zoom readout while it shows, the generation of the last change, and the timer that
    /// takes it down.
    readout: Option<Readout>,
    readout_gen: u64,
    readout_timer: Option<Task<()>>,
    _pump: Task<()>,
}

/// Rates for the stats overlay, re-sampled about once a second.
#[derive(Clone, Debug)]
struct Hud {
    sampled_at: Instant,
    sample: ScreenStats,
    summary: Vec<Figure>,
    text: SharedString,
}

/// What the overlay shows that is not in [`ScreenStats`].
#[derive(Clone, Copy, Debug)]
pub struct HudInput<'a> {
    /// Stream size in pixels.
    pub size: (u32, u32),
    /// Capture scale.
    pub scale: f32,
    /// How much colour the pictures carry, as decoded; `None` before the first.
    pub chroma: Option<Chroma>,
    /// The rate the stream is asked for, frames a second: its display period.
    pub target_fps: u16,
    /// Decoded frames per second over the last sample period.
    pub fps: f64,
    /// Received megabits per second over the last sample period.
    pub mbps: f64,
    /// Link round trip.
    pub rtt: Option<Duration>,
    /// How long ago the frame on screen was taken from the decoder.
    pub frame_age: Option<Duration>,
    /// The worker's last bitrate decision: target, verdict, cwnd-capped.
    pub rate: Option<(u32, RateVerdict, bool)>,
    /// The receiver's counters.
    pub stats: &'a ScreenStats,
    /// Arrival → present, from the element's pacer.
    pub pacing: &'a PacingStats,
    /// The UI's own frame times, when the app installed a probe.
    pub ui: Option<&'a crate::frames::FrameStats>,
}

/// The five lines of the stats overlay: what is on screen, how it got there, what the sound
/// did, when the picture was shown, and how the UI itself keeps up.
///
/// Line one is the picture: its size and capture scale, how much colour it carries (4:4:4 when
/// the decoder hands back full-chroma pictures, `xf44`, else 4:2:0) and the age of the frame
/// being shown; its rate, throughput and round trip are the plain line's
/// (`health::summary`). Line two is the path: jitter (RFC 3550 interarrival), how long frames
/// waited for their last fragment (p50 / p95 of the last report), the in-order queue, recovery
/// counts, stalls and the worker's bitrate verdict. Line three is the audio: packets played, lost
/// and concealed, the times playback ran dry, how much the jitter buffer trimmed and stretched to
/// hold its depth, and the depth it aims for. Line four is the presentation: how long a
/// frame takes from the arrival of the datagram that completed it to the paint that shows it
/// (p50 / p95 / worst of the last `slopty_client::pacing::RING` frames, with the decoder's share
/// of it), the spacing of those paints and its jitter, and the two cadence faults —
/// `skip` (a frame the display never saw) and `repeat` (a paint that showed the picture again).
/// Line five is the UI: draw time of the whole window (p50 / p95 / p99 / max over the last
/// [`crate::frames::RING`] frames), the spacing of frames, and how many went over the display
/// period or were dropped ([`crate::frames::hud_line`]). Pure so it can be checked without a
/// window.
#[must_use]
pub fn hud_lines(input: &HudInput<'_>) -> String {
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    let stats = input.stats;
    let chroma = input.chroma.map_or("chroma \u{2013}", chroma_label);
    let age =
        input.frame_age.map_or_else(|| "age –".to_owned(), |d| format!("age {:.0} ms", ms(d)));
    let stall = if stats.stalled { "stalled" } else { "flowing" };
    let rate = input.rate.map_or_else(
        || "target –".to_owned(),
        |(bps, verdict, capped)| {
            format!(
                "target {:.1} Mb/s {}{}",
                f64::from(bps) / 1e6,
                verdict_label(verdict),
                if capped { " (cwnd)" } else { "" }
            )
        },
    );
    let pacing = input.pacing;
    let ui = crate::frames::hud_line(input.ui);
    format!(
        "{}×{} @{:.2}  ·  {chroma}  ·  {age}\n\
         jitter {:.1} ms  ·  hold {:.1} / {:.1} ms  ·  queue {}  ·  fec {} lost {} nack {} refresh {}  ·  stalls {} ({} ms) {stall}  ·  {rate}\n\
         audio {} lost {} concealed {}  ·  dry {}  ·  trimmed {:.0} ms stretched {:.0} ms  ·  target {:.0} ms\n\
         present {:.1} / {:.1} / {:.1} ms (decode {:.1})  ·  every {:.1} ms ±{:.1}  ·  shown {} skip {} repeat {} late {}\n\
         {ui}",
        input.size.0,
        input.size.1,
        input.scale,
        ms(stats.jitter),
        ms(stats.hold_p50),
        ms(stats.hold_p95),
        stats.queue_depth,
        stats.frames_fec,
        stats.frames_lost,
        stats.nacks,
        stats.refreshes,
        stats.stalls,
        stats.stalled_ms,
        stats.audio_packets,
        stats.audio_lost,
        stats.audio_concealed,
        stats.audio_underruns,
        ms(stats.audio_trimmed),
        ms(stats.audio_stretched),
        ms(stats.audio_target),
        ms(pacing.latency_p50),
        ms(pacing.latency_p95),
        ms(pacing.latency_max),
        ms(pacing.decode_p50),
        ms(pacing.interval_p50),
        ms(pacing.interval_jitter),
        pacing.presented,
        pacing.skipped,
        pacing.repeats,
        pacing.late,
    )
}

/// How much colour a decoded picture of CoreVideo pixel format `format` carries: the decoder
/// hands back bi-planar 4:4:4 (`xf44` at 10 bits, `444f` at 8) only for a stream encoded so.
#[must_use]
pub const fn chroma_of(format: u32) -> Chroma {
    use core_video::pixel_buffer::{
        kCVPixelFormatType_444YpCbCr8BiPlanarFullRange,
        kCVPixelFormatType_444YpCbCr10BiPlanarFullRange,
    };
    if format == kCVPixelFormatType_444YpCbCr10BiPlanarFullRange
        || format == kCVPixelFormatType_444YpCbCr8BiPlanarFullRange
    {
        Chroma::Full
    } else {
        Chroma::Subsampled
    }
}

/// How the overlay names `chroma`.
#[must_use]
pub const fn chroma_label(chroma: Chroma) -> &'static str {
    match chroma {
        Chroma::Full => "4:4:4",
        Chroma::Subsampled => "4:2:0",
    }
}

/// What the item says while it has no picture yet.
///
/// The two cases look identical to the viewer and are not: a stream still starting up will draw
/// in a moment, while a window that is hidden, minimised or has never drawn will not draw until
/// something on the worker changes. Saying which is the visible half of the refresh-storm guard.
#[must_use]
pub const fn waiting_text(source: SourceState) -> &'static str {
    match source {
        SourceState::Live => "Waiting for the first frame…",
        SourceState::Idle => "Waiting for the window to draw…",
    }
}

/// How long a body waits on its worker before it says it is waiting: "Opening…",
/// "Reading…", "Attaching…".
///
/// Two frames at 60 Hz: the frame the answer lands in, plus a round trip of up to one frame
/// budget. The tailnet's median round trip is about 1 ms and the shaped tailnet profile's worst
/// is 12 ms (`docs/MEASUREMENTS.md`, "loading placeholders after a grace"), so a file read or an
/// attach that answers in time shows its content in place of a blank body, never a word that
/// flashes and goes.
pub const LOADING_GRACE: Duration = Duration::from_millis(32);

/// When a waiting body was first drawn, kept as its element's state.
struct Waiting(Instant);

/// Whether the waiting body keyed by `key` has been drawn for [`LOADING_GRACE`] or longer.
///
/// The first draw starts the clock and a timer that draws the view again when it runs out, so
/// the words come in on their own. The clock is dropped with the element: a body that got its
/// content and later waits again starts a new grace.
pub fn past_grace(key: impl Into<gpui::ElementId>, window: &mut Window, cx: &mut App) -> bool {
    let since = window.use_keyed_state(key, cx, |_window, cx| {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LOADING_GRACE).await;
            let _gone = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
        Waiting(cx.background_executor().now())
    });
    let since = since.read(cx).0;
    cx.background_executor().now().saturating_duration_since(since) >= LOADING_GRACE
}

/// How often the overlay's rates are recomputed, and the stream's health read.
const HUD_PERIOD: Duration = Duration::from_millis(1000);

/// The health mark's dot, in points at zoom 1.
const HEALTH_DOT: f32 = 6.0;

/// The controller's verdict as the overlay words it.
#[must_use]
pub const fn verdict_label(verdict: RateVerdict) -> &'static str {
    match verdict {
        RateVerdict::Cut => "cut",
        RateVerdict::Stall => "hold (stall)",
        RateVerdict::Steady => "steady",
        RateVerdict::Grow => "grow",
    }
}

/// A modifier the phone key bar can arm for the next key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sticky {
    /// ⌃.
    Control,
    /// ⌘.
    Command,
}

impl std::fmt::Debug for ScreenView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScreenView")
            .field("stream", &self.stream)
            .field("target", &self.target)
            .field("size", &self.size)
            .field("frames", &self.frames)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<ScreenViewEvent> for ScreenView {}

impl Focusable for ScreenView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// The frame rate asked for when the screen's refresh is not known: what nearly every display
/// runs at.
const UNKNOWN_REFRESH_HZ: u16 = 60;

/// The frames a second a stream asks for.
///
/// That is the refresh of the screen its view is on, up to the settings' `ceiling`
/// (`docs/decisions/video.md`, "The stream follows the screen's refresh"). A screen that does
/// not say (`refresh_hz` 0) counts as 60 Hz.
#[must_use]
pub const fn stream_fps(ceiling: u16, refresh_hz: u16) -> u16 {
    let screen = if refresh_hz == 0 { UNKNOWN_REFRESH_HZ } else { refresh_hz };
    let fps = if screen < ceiling { screen } else { ceiling };
    if fps == 0 { 1 } else { fps }
}

/// A refresh period as whole hertz; 0 for none.
fn hz_of(period: Option<Duration>) -> u16 {
    period.filter(|period| !period.is_zero()).map_or(0, |period| {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "≤ 240")]
        let hz = (1.0 / period.as_secs_f64()).round().min(f64::from(u16::MAX)) as u16;
        hz
    })
}

/// The refresh of the main screen (the one with the key window), hertz; 0 when it is not
/// known. What a stream opens at before its view is drawn in a window of its own.
#[must_use]
pub fn main_refresh_hz() -> u16 {
    hz_of(slopty_platform::display_refresh())
}

/// The CoreGraphics id of the screen `window` is on; `None` when GPUI knows of none.
fn window_screen(window: &Window, cx: &App) -> Option<u32> {
    window.display(cx).and_then(|display| u32::try_from(u64::from(display.id())).ok())
}

/// The refresh of `screen`, hertz; 0 when it is not known.
fn screen_refresh_hz(screen: Option<u32>) -> u16 {
    screen.map_or_else(main_refresh_hz, |id| hz_of(slopty_platform::display_refresh_of(id)))
}

/// The quality a stream is asked for: the settings' ceiling on the rate, at the refresh of the
/// screen the view is on ([`stream_fps`]), and their bitrate ceiling at `scale`.
#[must_use]
pub const fn quality_of(prefs: slopty_theme::StreamPrefs, scale: f32, refresh_hz: u16) -> Quality {
    Quality {
        fps: stream_fps(prefs.fps, refresh_hz),
        bitrate_bps: prefs.max_bitrate_bps,
        scale,
        codec: VideoCodec::Hevc,
        chroma: if prefs.sharp_text { Chroma::Full } else { Chroma::Subsampled },
    }
}

impl ScreenView {
    /// Swap the theme: chrome colours, and the stream settings, which a live stream asks
    /// the worker for at once (the scale it holds stays; that follows the width the tile is drawn
    /// at).
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme == theme {
            return;
        }
        let wanted = quality_of(theme.behaviour.stream, self.quality.scale, self.refresh_hz());
        if wanted != self.quality {
            self.quality = wanted;
            self.send(ScreenRequest::SetQuality { stream: self.stream, quality: self.quality });
        }
        // The pill's own toggles stand; only a change of the setting moves the switch.
        if theme.behaviour.stream.muted != self.theme.behaviour.stream.muted {
            self.handle.set_muted(theme.behaviour.stream.muted);
        }
        self.theme = theme;
        cx.notify();
    }

    /// Ask for the refresh of the screen `window` is on when it is not the one asked for: the
    /// view drew on another screen, or its window moved to one. Only the rate changes.
    fn follow_screen(&mut self, window: &Window, cx: &App) {
        let screen = window_screen(window, cx);
        if self.screen.is_some_and(|(on, _)| on == screen) {
            return;
        }
        self.on_screen(screen, screen_refresh_hz(screen));
    }

    /// The view is drawn on `screen`, which refreshes at `hz` (0 when it does not say): the
    /// stream asks for that rate, up to the settings' ceiling, when it is not the one it has.
    fn on_screen(&mut self, screen: Option<u32>, hz: u16) {
        self.screen = Some((screen, hz));
        let fps = stream_fps(self.theme.behaviour.stream.fps, hz);
        if fps != self.quality.fps {
            self.quality.fps = fps;
            self.send(ScreenRequest::SetQuality { stream: self.stream, quality: self.quality });
        }
    }

    /// The refresh of the screen the view is drawn on, hertz; 0 when it is not known.
    fn refresh_hz(&self) -> u16 {
        self.screen.map_or(0, |(_, hz)| hz)
    }

    /// The frames a second this stream asks for now.
    #[must_use]
    pub const fn fps(&self) -> u16 {
        self.quality.fps
    }

    /// Wrap an opened stream.
    pub fn new(
        opened: Opened,
        handle: ScreenHandle,
        out: mpsc::Sender<ClientMsg>,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Self {
        let Opened { stream, target, size, quality } = opened;
        // A cursor picture's texture lives in every window's atlas until it is dropped from it.
        cx.on_release(|view, cx| view.pointer.drop_image(cx)).detach();
        handle.set_muted(theme.behaviour.stream.muted);
        let pump = Self::pump(handle.frames(), handle.cursor(), Self::take_frame, cx);
        // Somebody is watching a remote window: the platform must not dim or sleep under it.
        let acquisition = cx.prevent_idle_sleep("Slopty remote window");
        let awake = cx.spawn(async move |_this, _cx| match acquisition.await {
            Ok(guard) => {
                let _guard = guard;
                std::future::pending::<()>().await;
            }
            Err(e) => tracing::warn!(error = %e, "idle sleep prevention"),
        });
        let scale = quality.scale.clamp(MIN_SCALE, 1.0);
        #[expect(clippy::cast_precision_loss, reason = "pixel counts are small")]
        let native = (size.0 as f32 / scale, size.1 as f32 / scale);
        Self {
            stream,
            target,
            handle,
            latest: None,
            chroma: None,
            system_keys: false,
            typing: VecDeque::new(),
            typer: None,
            size,
            native,
            quality,
            quality_changed: Instant::now(),
            wanted_width: None,
            cursor: CursorState::default(),
            pointer: Pointer::Arrow,
            placed: None,
            drawn: None,
            last_frame: None,
            fold: None,
            label: match target {
                CaptureTarget::Display(id) => format!("Remote display {}", id.0).into(),
                CaptureTarget::Window(id) => format!("Remote window {}", id.0).into(),
            },
            renders: 0,
            mapped: size,
            out: Outbox::new(out, cx),
            theme,
            focus: cx.focus_handle(),
            bounds: Bounds::default(),
            frames: 0,
            fps_sample: std::cell::Cell::new((Instant::now(), 0, 0.0)),
            held: Vec::new(),
            buttons: Vec::new(),
            pointer_at: (0.0, 0.0),
            modifiers: Modifiers::default(),
            let_go: None,
            screen: None,
            paste_hook: None,
            paste_hold: (0, Vec::new()),
            sticky: Modifiers::default(),
            marked: None,
            hud: None,
            hud_details: false,
            probe: health::Probe::default(),
            health: None,
            _health: Self::watch_health(cx),
            rtt: None,
            _awake: awake,
            #[cfg(target_os = "macos")]
            _display: (!cfg!(test))
                .then(|| slopty_platform::Activity::display_awake("Slopty remote window")),
            pacer: Pacer::default(),
            rate: None,
            source: SourceState::Live,
            scrolling: Scrolling::Idle,
            momentum_end: None,
            zoom: Zoom::FIT,
            painted: 0.0,
            scale_factor: 1.0,
            two: None,
            two_scrolling: false,
            trackpad: None,
            touch: TOUCH,
            readout: None,
            readout_gen: 0,
            readout_timer: None,
            _pump: pump,
        }
    }

    /// Stream id.
    #[must_use]
    pub const fn stream(&self) -> StreamId {
        self.stream
    }

    /// What is streamed.
    #[must_use]
    pub const fn target(&self) -> CaptureTarget {
        self.target
    }

    /// The picture's accessible label.
    const fn a11y_label(&self) -> &SharedString {
        &self.label
    }

    /// Renders so far.
    #[must_use]
    pub const fn renders(&self) -> u32 {
        self.renders
    }

    /// Take what the stream hands over: each frame `take` puts up draws the view, and a cursor
    /// sample draws it only when the pointer drawn moves ([`Self::pointer_changed`]), at a time
    /// that function picks. Generic over the frame so a test can feed pictures of its own.
    fn pump<F: Clone + 'static>(
        mut frames: watch::Receiver<F>,
        mut cursor: watch::Receiver<CursorState>,
        take: fn(&mut Self, F, &mut Context<Self>) -> bool,
        cx: &Context<Self>,
    ) -> Task<()> {
        enum Step<F> {
            Frame(F),
            Cursor(CursorState),
            Due,
        }
        cx.spawn(async move |this, cx| {
            let executor = cx.background_executor().clone();
            let mut due: Option<Instant> = None;
            loop {
                let wake = async {
                    match due {
                        Some(at) => {
                            executor.timer(at.saturating_duration_since(executor.now())).await;
                        }
                        None => std::future::pending::<()>().await,
                    }
                };
                let step = tokio::select! {
                    changed = frames.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        Step::Frame(frames.borrow_and_update().clone())
                    }
                    changed = cursor.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        Step::Cursor(*cursor.borrow_and_update())
                    }
                    () = wake => Step::Due,
                };
                let next = this.update(cx, |view, cx| {
                    let now = cx.background_executor().now();
                    match step {
                        // A picture the pacer drops (late, or the one already up) changes
                        // nothing on screen: no frame for it.
                        Step::Frame(frame) => {
                            if take(view, frame, cx) {
                                view.last_frame = Some(now);
                                cx.notify();
                            }
                            due
                        }
                        Step::Cursor(state) => {
                            view.cursor = state;
                            view.pointer_changed(now, cx)
                        }
                        Step::Due => view.pointer_changed(now, cx),
                    }
                });
                match next {
                    Ok(next) => due = next,
                    Err(_gone) => break,
                }
            }
        })
    }

    /// Something the drawn pointer follows changed: a cursor sample, a hold that ran out, a
    /// fold that came due. The view draws again only when the pointer it would draw is not the
    /// one on screen. While frames flow, the change waits for the next frame, which draws it
    /// anyway, and draws on its own only when that frame is [`FOLD`] late: a draw of its own
    /// just before a frame makes the frame the second in its refresh, and a `CAMetalLayer`
    /// shows that a refresh late (`docs/MEASUREMENTS.md`, "echo, key → glass"). Returns when
    /// to look again.
    fn pointer_changed(&mut self, now: Instant, cx: &mut Context<Self>) -> Option<Instant> {
        let recheck = match self.target {
            CaptureTarget::Display(_) => self.hold_end().filter(|end| now < *end),
            CaptureTarget::Window(_) => None,
        };
        if self.pointer_drawn(now) == self.drawn {
            self.fold = None;
            return recheck;
        }
        let fold =
            self.fold.or_else(|| self.next_frame_due(now).and_then(|due| due.checked_add(FOLD)));
        if let Some(at) = fold
            && now < at
        {
            self.fold = Some(at);
            return Some(recheck.map_or(at, |end| end.min(at)));
        }
        self.fold = None;
        cx.notify();
        recheck
    }

    /// When the next frame is due, one period of the rate asked for after the last; `None`
    /// once two periods have passed without one (a still window sends none).
    fn next_frame_due(&self, now: Instant) -> Option<Instant> {
        let period = Duration::from_secs(1).checked_div(u32::from(self.quality.fps))?;
        let last = self.last_frame?;
        if now.saturating_duration_since(last) >= period.saturating_mul(2) {
            return None;
        }
        last.checked_add(period)
    }

    /// When this client's hold on a display's pointer ends ([`LOCAL_HOLD`]).
    fn hold_end(&self) -> Option<Instant> {
        self.placed
            .and_then(|at| at.checked_add(LOCAL_HOLD.saturating_add(self.rtt.unwrap_or_default())))
    }

    /// This client put the worker's pointer at `at` (stream pixels).
    const fn place(&mut self, at: (f32, f32), now: Instant) {
        self.pointer_at = at;
        self.placed = Some(now);
    }

    /// Whether the pointer drawn is this client's own. On a window stream it always is once
    /// this client has put it somewhere: input posted to a window's owner leaves the worker's
    /// pointer alone, so the worker's sample is only the echo of this client's input, a round
    /// trip and a cursor tick late. On a display it is for [`LOCAL_HOLD`] and a round trip
    /// after this client last moved it; after that the worker's sample shows where it went.
    fn local_drives(&self, now: Instant) -> bool {
        match self.target {
            CaptureTarget::Window(_) => self.placed.is_some(),
            CaptureTarget::Display(_) => self.hold_end().is_some_and(|end| now < end),
        }
    }

    /// The pointer on the picture, in fractions of it, and whether it is drawn there: the
    /// trackpad's in trackpad mode, this client's own while it drives it, else the worker's
    /// last sample. The worker's samples are in the stream pixels it maps input with, so they
    /// scale by the size input does, not by the size of the frame in flight.
    fn pointer_spot(&self, now: Instant) -> ((f32, f32), bool) {
        if let Some(pad) = self.trackpad {
            return (pad.at(), true);
        }
        let (w, h) = self.mapped_f32();
        let (w, h) = (w.max(1.0), h.max(1.0));
        if self.local_drives(now) {
            // Whole stream pixels, as the worker's sample has them: when the hold ends on a
            // still pointer, the sample that takes over draws it where it was.
            let (x, y) = self.pointer_at;
            return (((x.round() / w).clamp(0.0, 1.0), (y.round() / h).clamp(0.0, 1.0)), true);
        }
        #[expect(clippy::cast_precision_loss, reason = "cursor coordinates are small")]
        let at = (self.cursor.x as f32 / w, self.cursor.y as f32 / h);
        (at, self.cursor.visible)
    }

    /// Where the pointer is drawn on the picture now, or `None` when none is (no picture yet,
    /// or the worker's pointer is off the target).
    fn pointer_drawn(&self, now: Instant) -> Option<(f32, f32)> {
        let (at, shown) = self.pointer_spot(now);
        (shown && self.latest.is_some()).then_some(at)
    }

    /// Draw again when the pointer this client just placed is not where the last render drew it.
    fn redraw_pointer(&self, now: Instant, cx: &mut Context<Self>) {
        if self.pointer_drawn(now) != self.drawn {
            cx.notify();
        }
    }

    /// Frames painted so far.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.frames
    }

    /// Stream pixel size of the picture last painted.
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Pictures painted a second, over the last second or so: what the status bar says of a
    /// focused stream. A still window paints nothing, and says so.
    #[must_use]
    pub fn painted_fps(&self) -> f32 {
        let (at, frames, fps) = self.fps_sample.get();
        let elapsed = at.elapsed();
        if elapsed < Duration::from_secs(1) {
            return fps;
        }
        #[expect(clippy::cast_precision_loss, reason = "frames painted in a second or so")]
        let fps = self.frames.saturating_sub(frames) as f32 / elapsed.as_secs_f32();
        self.fps_sample.set((Instant::now(), self.frames, fps));
        fps
    }

    /// Receiver counters.
    #[must_use]
    pub fn stats(&self) -> ScreenStats {
        self.handle.stats()
    }

    /// Show or hide the stats overlay.
    pub fn set_hud(&mut self, on: bool, cx: &mut Context<Self>) {
        self.hud = on.then(|| Hud {
            sampled_at: Instant::now(),
            sample: self.handle.stats(),
            summary: Vec::new(),
            text: SharedString::new_static("…"),
        });
        cx.notify();
    }

    /// Open or close the overlay's engineering lines.
    fn toggle_hud_details(&mut self, cx: &mut Context<Self>) {
        self.hud_details = !self.hud_details;
        cx.notify();
    }

    /// Read the stream's health once a second for as long as the view lives.
    fn watch_health(cx: &Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(HUD_PERIOD).await;
                if this.update(cx, Self::read_health).is_err() {
                    return;
                }
            }
        })
    }

    /// Read the counters; a change of health is the header's news.
    fn read_health(&mut self, cx: &mut Context<Self>) {
        let health = self.probe.read(&self.handle.stats(), &self.pacer.stats(), Instant::now());
        if health != self.health {
            self.health = health;
            cx.emit(ScreenViewEvent::Health);
        }
    }

    /// What is wrong with the stream, as last read; `None` while all is well.
    #[must_use]
    pub const fn health(&self) -> Option<Health> {
        self.health
    }

    /// The header's health mark for `view`: a dot and one word, only while something is wrong.
    /// A click opens the stats overlay.
    #[must_use]
    pub fn health_mark(
        view: &gpui::Entity<Self>,
        theme: &Theme,
        k: f32,
        cx: &App,
    ) -> Option<gpui::AnyElement> {
        let this = view.read(cx);
        let health = this.health?;
        let s = theme.surfaces;
        let dot = if health == Health::Stalled { s.error_fill } else { s.warn_fill };
        let view = view.clone();
        let mark = div()
            .id(SharedString::from(format!("health-{}", this.stream.0)))
            .debug_selector(|| "stream-health".to_owned())
            .role(gpui::accesskit::Role::Button)
            .aria_label(SharedString::new_static(health.word()))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs * k))
            .px(px(theme.spacing.xs * k))
            .rounded(px(theme.radii.sm * k))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla(s.raised)))
            .text_size(px(theme.typography.small() * k))
            .text_color(hsla(s.text_secondary))
            .child(div().flex_none().size(px(HEALTH_DOT * k)).rounded_full().bg(hsla(dot)))
            .child(health.word());
        Some(
            crate::a11y::tab_stop(mark, s.accent)
                .on_click(move |_ev, _w, cx| view.update(cx, |v, cx| v.set_hud(true, cx)))
                .into_any_element(),
        )
    }

    /// Whether the stats overlay is showing.
    #[must_use]
    pub const fn hud(&self) -> bool {
        self.hud.is_some()
    }

    /// Link RTT, shown in the overlay.
    pub const fn set_rtt(&mut self, rtt: Option<Duration>) {
        self.rtt = rtt;
    }

    /// The worker's latest bitrate decision, shown in the overlay and read for health.
    pub fn set_rate(&mut self, target_bps: u32, verdict: RateVerdict, capped: bool) {
        self.rate = Some((target_bps, verdict, capped));
        self.probe.verdict(verdict, Instant::now());
    }

    /// The worker said which cursor it shows (`ScreenEvent::Cursor`): draw that picture at the
    /// pointer from now on, or the arrow again for `None`.
    pub fn set_cursor_shape(&mut self, shape: Option<CursorShape>, cx: &mut Context<Self>) {
        std::mem::replace(&mut self.pointer, Pointer::from_shape(shape)).drop_image(cx);
        cx.notify();
    }

    /// The worker cursor picture's size and hotspot in points, when one is drawn.
    #[cfg(test)]
    const fn pointer_picture(&self) -> Option<(Size<Pixels>, Point<Pixels>)> {
        match &self.pointer {
            Pointer::Arrow => None,
            Pointer::Image { size, hot, .. } => Some((*size, *hot)),
        }
    }

    /// The worker said whether its capture target is drawing. While it is not, the receiver stops
    /// asking for refreshes (a hidden window has nothing to refresh from) and the placeholder
    /// says so instead of "Waiting for the first frame".
    pub fn set_source_state(&mut self, state: SourceState, cx: &mut Context<Self>) {
        if self.source != state {
            self.source = state;
            self.handle.set_source_live(state == SourceState::Live);
            cx.notify();
        }
    }

    /// What the worker says its capture target is doing.
    #[must_use]
    pub const fn source_state(&self) -> SourceState {
        self.source
    }

    /// The worker's latest bitrate decision: target, verdict, whether the cwnd cap holds it.
    #[must_use]
    pub const fn rate(&self) -> Option<(u32, RateVerdict, bool)> {
        self.rate
    }

    /// Recompute the overlay's rates when a second has passed; returns its plain line and its
    /// engineering lines.
    fn hud_text(&mut self, cx: &App) -> Option<(Vec<Figure>, SharedString)> {
        let hud = self.hud.as_mut()?;
        let now = Instant::now();
        let elapsed = now.duration_since(hud.sampled_at);
        if elapsed >= HUD_PERIOD {
            // The frame and pacing percentiles sort their rings: once a HUD period, never
            // per paint.
            let ui = crate::frames::stats(cx);
            let stats = self.handle.stats();
            let secs = elapsed.as_secs_f64();
            #[expect(clippy::cast_precision_loss, reason = "counter deltas over a second")]
            let fps = stats.frames.saturating_sub(hud.sample.frames) as f64 / secs;
            #[expect(clippy::cast_precision_loss, reason = "counter deltas over a second")]
            let mbps = stats.bytes.saturating_sub(hud.sample.bytes) as f64 * 8.0 / secs / 1e6;
            let pacing = self.pacer.stats();
            let input = HudInput {
                size: self.size,
                scale: self.quality.scale,
                chroma: self.chroma,
                target_fps: self.quality.fps,
                fps,
                mbps,
                rtt: self.rtt,
                frame_age: self.pacer.age(),
                rate: self.rate,
                stats: &stats,
                pacing: &pacing,
                ui: ui.as_ref(),
            };
            hud.summary = health::summary(&input);
            hud.text = hud_lines(&input).into();
            hud.sample = stats;
            hud.sampled_at = now;
        }
        Some((hud.summary.clone(), hud.text.clone()))
    }

    /// Whether the worker has sent any audio for this stream (the mute control is pointless
    /// before that).
    #[must_use]
    pub fn has_audio(&self) -> bool {
        self.handle.stats().audio_packets > 0
    }

    /// Whether audio playback is silenced on this client.
    #[must_use]
    pub fn muted(&self) -> bool {
        self.handle.muted()
    }

    /// Silence or resume this stream's audio on this client only.
    /// The pill lives in the tile's header, so the caller notifies its own entity.
    pub fn toggle_mute(&self) {
        self.handle.set_muted(!self.handle.muted());
    }

    /// The workspace reports how wide the view is painted (device pixels) so the stream can be
    /// downscaled at the worker when it is drawn small. Quantised to quarter steps and rate
    /// limited: a change inside the cooldown is taken when it ends, the latest width asked for
    /// winning. A picture zoomed inside the tile is drawn wider than the tile, and asks for
    /// that width.
    pub fn set_painted_width(&mut self, device_px: f32, cx: &Context<Self>) {
        self.painted = device_px;
        let drawn = device_px * self.zoom.scale();
        let wanted = (drawn / self.native.0).clamp(MIN_SCALE, 1.0);
        let bucket = (wanted * 4.0).ceil() / 4.0;
        if (bucket - self.quality.scale).abs() < f32::EPSILON {
            self.wanted_width = None;
            return;
        }
        let since = self.quality_changed.elapsed();
        if since < QUALITY_COOLDOWN {
            if self.wanted_width.replace(device_px).is_none() {
                let wait = QUALITY_COOLDOWN.saturating_sub(since);
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(wait).await;
                    let _gone = this.update(cx, |this, cx| {
                        if let Some(width) = this.wanted_width.take() {
                            this.set_painted_width(width, cx);
                        }
                    });
                })
                .detach();
            }
            return;
        }
        self.wanted_width = None;
        self.quality.scale = bucket;
        self.quality_changed = Instant::now();
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped")]
        let side = |native: f32| ((native * bucket).round().max(2.0) as u32).next_multiple_of(2);
        self.size = (side(self.native.0), side(self.native.1));
        // The worker's pointer is drawn at the new scale from here on; the sample it last sent
        // is carried over, so a still pointer stays put until the worker's first sample at the
        // new scale comes back.
        let rescale = |v: i32, from: u32, to: u32| -> i32 {
            #[expect(clippy::cast_possible_truncation, reason = "a pixel position, rounded")]
            let p = (f64::from(v) * f64::from(to) / f64::from(from.max(1))).round() as i32;
            p
        };
        self.cursor.x = rescale(self.cursor.x, self.mapped.0, self.size.0);
        self.cursor.y = rescale(self.cursor.y, self.mapped.1, self.size.1);
        #[expect(clippy::cast_precision_loss, reason = "pixel counts are small")]
        let rescale = |v: f32, from: u32, to: u32| v * to as f32 / from.max(1) as f32;
        self.pointer_at = (
            rescale(self.pointer_at.0, self.mapped.0, self.size.0),
            rescale(self.pointer_at.1, self.mapped.1, self.size.1),
        );
        self.mapped = self.size;
        self.send(ScreenRequest::SetQuality { stream: self.stream, quality: self.quality });
    }

    /// The worker resized the target: the stream now has this pixel size at the current quality
    /// scale, so the native size follows from it.
    pub fn set_geometry(&mut self, width: u32, height: u32) {
        self.size = (width, height);
        self.mapped = self.size;
        let scale = self.quality.scale.clamp(MIN_SCALE, 1.0);
        #[expect(clippy::cast_precision_loss, reason = "pixel counts are small")]
        let native = (width as f32 / scale, height as f32 / scale);
        self.native = native;
    }

    /// Native size of the target in stream pixels at scale 1.
    #[must_use]
    pub const fn native(&self) -> (f32, f32) {
        self.native
    }

    /// A decoded frame came off the stream. The pacer decides whether it goes up (it always
    /// does unless it is older than the picture already showing); nothing is queued for a later
    /// paint, so the frame on screen is always the newest one that had arrived by paint time.
    /// Whether it went up.
    fn take_frame(&mut self, frame: Option<Arc<Presentable>>, cx: &mut Context<Self>) -> bool {
        let Some(frame) = frame else { return false };
        if self.pacer.offer(frame.stamp) == Pace::Drop {
            return false;
        }
        let raw = std::ptr::from_ref(frame.frame.image.as_cv())
            .cast_mut()
            .cast::<core_video::buffer::__CVBuffer>();
        // SAFETY: `raw` is a live `CVPixelBufferRef` owned by `frame`; `wrap_under_get_rule`
        // takes its own retain, so the wrapper stays valid even if `frame` is dropped first.
        let buffer = unsafe { CVPixelBuffer::wrap_under_get_rule(raw) };
        self.show(buffer, cx);
        true
    }

    /// Put `buffer` up as the picture.
    fn show(&mut self, buffer: CVPixelBuffer, cx: &mut Context<Self>) {
        #[expect(clippy::cast_possible_truncation, reason = "pixel counts")]
        let size = (buffer.get_width() as u32, buffer.get_height() as u32);
        self.size = size;
        self.chroma = Some(chroma_of(buffer.get_pixel_format()));
        self.latest = Some(buffer);
        self.frames = self.frames.saturating_add(1);
        if self.frames == 1 {
            cx.emit(ScreenViewEvent::Ready);
        }
    }

    /// Put `buffer` up as the picture, as a frame off the stream would (for the workspace's
    /// frame measurements).
    #[cfg(test)]
    pub(crate) fn show_picture(&mut self, buffer: CVPixelBuffer, cx: &mut Context<Self>) {
        self.show(buffer, cx);
    }

    /// Arrival → present numbers for the last frames (the overlay and the app self-test).
    #[must_use]
    pub fn pacing(&self) -> PacingStats {
        self.pacer.stats()
    }

    fn send(&self, req: ScreenRequest) {
        self.out.send(ClientMsg::Screen(req));
    }

    fn input(&mut self, input: ScreenInput) {
        if self.paste_hold.0 > 0 {
            self.paste_hold.1.push(input);
            return;
        }
        self.send(ScreenRequest::Input { stream: self.stream, input });
    }

    /// Files a paste waited for are on the worker's pasteboard, or will not come: once no
    /// other paste is waiting, the held input goes on, in order.
    pub fn release_paste(&mut self) {
        self.paste_hold.0 = self.paste_hold.0.saturating_sub(1);
        if self.paste_hold.0 > 0 {
            return;
        }
        for input in std::mem::take(&mut self.paste_hold.1) {
            self.send(ScreenRequest::Input { stream: self.stream, input });
        }
    }

    /// Whether input waits behind a paste of files.
    #[must_use]
    pub const fn paste_held(&self) -> bool {
        self.paste_hold.0 > 0
    }

    /// Window position → stream pixels, at the size the worker maps them with, through the
    /// zoom: the pixel of the picture drawn under `position`.
    fn to_stream(&self, position: Point<Pixels>) -> (f32, f32) {
        let (fx, fy) = self.zoom.to_picture(self.body_fraction(position));
        let (stream_w, stream_h) = self.mapped_f32();
        let (x, y) = (fx * stream_w, fy * stream_h);
        tracing::trace!(?position, bounds = ?self.bounds, mapped = ?self.mapped, x, y, "to_stream");
        (x, y)
    }

    /// Window position → a point of the body, in fractions of its size.
    fn body_fraction(&self, position: Point<Pixels>) -> (f32, f32) {
        let (w, h) = self.body_size();
        (
            (f32::from(position.x) - f32::from(self.bounds.origin.x)) / w,
            (f32::from(position.y) - f32::from(self.bounds.origin.y)) / h,
        )
    }

    /// The body's size in points, never zero.
    fn body_size(&self) -> (f32, f32) {
        (f32::from(self.bounds.size.width).max(1.0), f32::from(self.bounds.size.height).max(1.0))
    }

    /// How the picture is drawn over the body.
    #[must_use]
    pub const fn zoom(&self) -> Zoom {
        self.zoom
    }

    /// The scale at which a pixel of the target is a pixel of this device.
    fn one_to_one(&self) -> f32 {
        zoom::one_to_one(self.native.0, self.body_size().0, self.scale_factor)
    }

    /// Draw the picture as `zoom` (held to its limits), say so, and ask the worker for the
    /// scale it is now drawn at.
    fn set_zoom(&mut self, zoom: Zoom, cx: &mut Context<Self>) {
        let zoom = zoom.clamped(zoom::max_scale(self.one_to_one()));
        if zoom == self.zoom {
            return;
        }
        let scaled = (zoom.scale() - self.zoom.scale()).abs() > f32::EPSILON;
        self.zoom = zoom;
        if scaled {
            self.show_readout(cx);
            if self.painted > 0.0 {
                self.set_painted_width(self.painted, cx);
            }
        }
        cx.notify();
    }

    /// Put the zoom readout up, and take it down after [`READOUT_HOLD`]: fading over
    /// [`kit::FADE`], or at once under Reduce Motion.
    fn show_readout(&mut self, cx: &Context<Self>) {
        self.readout = Some(Readout::Shown);
        self.readout_gen = self.readout_gen.wrapping_add(1);
        let generation = self.readout_gen;
        let fade = kit::motion(cx);
        self.readout_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(READOUT_HOLD).await;
            if fade {
                let _gone = this.update(cx, |v, cx| {
                    v.readout = Some(Readout::Fading(generation));
                    cx.notify();
                });
                cx.background_executor().timer(kit::FADE).await;
            }
            let _gone = this.update(cx, |v, cx| {
                v.readout = None;
                cx.notify();
            });
        }));
    }

    /// What the zoom readout says while it is up.
    #[must_use]
    pub fn readout(&self) -> Option<String> {
        self.readout.map(|_| zoom::readout(self.zoom, self.one_to_one()))
    }

    /// Two fingers on the picture: a pinch zooms about them, and their drag pans the zoomed
    /// picture (in trackpad mode it scrolls the remote app instead, and the two lock to
    /// whichever they did first). Two fingers tapped and lifted right-click.
    fn pinch(&mut self, ev: &PinchEvent, _w: &mut Window, cx: &mut Context<Self>) {
        if self.two.is_none() && (ev.phase != TouchPhase::Started || !self.inside(ev.position)) {
            return;
        }
        cx.stop_propagation();
        let now = cx.background_executor().now();
        let at = self.body_fraction(ev.position);
        let (w, h) = self.body_size();
        let points = (at.0 * w, at.1 * h);
        match ev.phase {
            TouchPhase::Started => {
                self.two = Some(touch::Two::begin(points, now));
                self.two_scrolling = false;
                self.two_step(ev.delta, points, at, cx);
            }
            TouchPhase::Moved => self.two_step(ev.delta, points, at, cx),
            TouchPhase::Ended | TouchPhase::Cancelled => {
                let Some(two) = self.two.take() else { return };
                if self.two_scrolling {
                    self.two_scrolling = false;
                    self.trackpad_scroll((0.0, 0.0), ScrollPhase::Ended);
                }
                if ev.phase == TouchPhase::Ended && self.touch && two.tapped(now) {
                    let (x, y) = match self.trackpad {
                        Some(pad) => self.picture_to_stream(pad.at()),
                        None => self.to_stream(ev.position),
                    };
                    self.click_at(ProtoButton::Right, 1, (x, y), now);
                }
            }
        }
    }

    /// One report of the two fingers: `delta` is the scale step less one, `points` and `at`
    /// the centroid in the body's points and fractions.
    fn two_step(&mut self, delta: f32, points: (f32, f32), at: (f32, f32), cx: &mut Context<Self>) {
        let Some(two) = self.two.as_mut() else { return };
        let step = two.step((1.0 + delta).max(0.01), points);
        let max = zoom::max_scale(self.one_to_one());
        let (w, h) = self.body_size();
        if self.trackpad.is_some() {
            match step.kind {
                touch::TwoKind::Pinch => self.set_zoom(self.zoom.about(at, step.factor, max), cx),
                touch::TwoKind::Drag => {
                    let phase =
                        if self.two_scrolling { ScrollPhase::Changed } else { ScrollPhase::Began };
                    self.two_scrolling = true;
                    self.trackpad_scroll(step.by, phase);
                }
                touch::TwoKind::Undecided => {}
            }
            return;
        }
        // The centroid's move pans the picture it holds; the zoom then keeps the point under
        // the new centroid where it is.
        let panned = self.zoom.panned((step.by.0 / w, step.by.1 / h), max);
        self.set_zoom(panned.about(at, step.factor, max), cx);
    }

    /// A two-finger drag in trackpad mode: a precise scroll at the pointer.
    fn trackpad_scroll(&mut self, by: (f32, f32), phase: ScrollPhase) {
        let Some(pad) = self.trackpad else { return };
        let (x, y) = self.picture_to_stream(pad.at());
        self.input(ScreenInput::Scroll {
            dx: by.0,
            dy: by.1,
            precise: true,
            phase,
            momentum: ScrollPhase::None,
            x,
            y,
            mods: keys::mods(self.modifiers),
        });
    }

    /// A point of the picture (0 to 1) in the stream pixels input maps with.
    fn picture_to_stream(&self, f: (f32, f32)) -> (f32, f32) {
        let (w, h) = self.mapped_f32();
        (f.0 * w, f.1 * h)
    }

    /// A press and a release of `button` at `(x, y)`.
    fn click_at(&mut self, button: ProtoButton, clicks: u8, (x, y): (f32, f32), now: Instant) {
        self.place((x, y), now);
        let mods = keys::mods(self.modifiers);
        for down in [true, false] {
            self.input(ScreenInput::Button { button, down, x, y, clicks, mods });
        }
    }

    /// Whether trackpad mode is on.
    #[must_use]
    pub const fn trackpad(&self) -> bool {
        self.trackpad.is_some()
    }

    /// Turn trackpad mode on or off. The pointer starts where it is drawn, or in the middle of
    /// what is in view; turning it off lets go of a drag in progress.
    pub fn set_trackpad(&mut self, on: bool, cx: &mut Context<Self>) {
        if on == self.trackpad.is_some() {
            return;
        }
        if on {
            let (at, shown) = self.pointer_spot(cx.background_executor().now());
            let start = if shown { at } else { self.zoom.to_picture((0.5, 0.5)) };
            self.trackpad = Some(touch::Trackpad::new(start));
        } else if let Some(mut pad) = self.trackpad.take() {
            let acts = pad.cancelled();
            self.trackpad_acts(pad, &acts, cx.background_executor().now());
        }
        cx.notify();
    }

    /// Flip trackpad mode (the palette's command and the header's control).
    pub fn toggle_trackpad(&mut self, cx: &mut Context<Self>) {
        self.set_trackpad(self.trackpad.is_none(), cx);
    }

    /// The header's trackpad control for `view`, on a device whose fingers are its pointer:
    /// a quiet toggle beside the tile's other buttons, pressed while the mode is on.
    #[must_use]
    pub fn trackpad_button(
        view: &gpui::Entity<Self>,
        theme: &Theme,
        k: f32,
        cx: &App,
    ) -> Option<gpui::AnyElement> {
        let this = view.read(cx);
        if !this.touch {
            return None;
        }
        let id = format!("trackpad-{}", this.stream.0);
        let icon = crate::icons::IconName::MousePointer2;
        let button = kit::icon_toggle(theme, id, icon, TRACKPAD_MODE, this.trackpad(), k);
        let view = view.clone();
        Some(
            button
                .on_click(move |_ev, _w, cx| view.update(cx, Self::toggle_trackpad))
                .into_any_element(),
        )
    }

    /// Send what the trackpad asked for, at its pointer, and keep the pointer in view.
    fn trackpad_acts(&mut self, pad: touch::Trackpad, acts: &[touch::Act], now: Instant) {
        let (x, y) = self.picture_to_stream(pad.at());
        let mods = keys::mods(self.modifiers);
        for act in acts {
            let (button, down, clicks) = match *act {
                touch::Act::Move => {
                    self.place((x, y), now);
                    self.input(ScreenInput::Move { x, y });
                    continue;
                }
                touch::Act::Press(b, clicks) => (b, true, clicks),
                touch::Act::Release(b, clicks) => (b, false, clicks),
            };
            let button = match button {
                touch::Button::Left => ProtoButton::Left,
                touch::Button::Right => ProtoButton::Right,
            };
            if down && !self.buttons.contains(&button) {
                self.buttons.push(button);
            } else if !down {
                self.buttons.retain(|b| *b != button);
            }
            self.place((x, y), now);
            self.input(ScreenInput::Button { button, down, x, y, clicks, mods });
        }
    }

    /// One finger in trackpad mode (a touch drag gpui offers before it is a tap or a pan; the
    /// view claims it). Returns whether it was this view's.
    fn touch_drag(
        &mut self,
        ev: &TouchDragEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(mut pad) = self.trackpad else { return false };
        let now = cx.background_executor().now();
        let (w, h) = self.body_size();
        let at = self.body_fraction(ev.position);
        let points = (at.0 * w, at.1 * h);
        let acts = match ev.phase {
            TouchPhase::Started => {
                if !self.inside(ev.start_position) {
                    return false;
                }
                self.take_focus(window, cx);
                pad.begin(points, now);
                Vec::new()
            }
            _ if !pad.touching() => return false,
            TouchPhase::Moved => {
                let span = (w * self.zoom.scale(), h * self.zoom.scale());
                pad.moved(points, now, span)
            }
            TouchPhase::Ended => pad.ended(now),
            TouchPhase::Cancelled => pad.cancelled(),
        };
        self.trackpad = Some(pad);
        self.trackpad_acts(pad, &acts, now);
        if acts.contains(&touch::Act::Move) {
            let margin = self.theme.spacing.xl;
            let max = zoom::max_scale(self.one_to_one());
            let kept = self.zoom.revealing(pad.at(), (margin / w, margin / h), max);
            self.set_zoom(kept, cx);
        }
        cx.notify();
        true
    }

    /// The view takes the keys, and the worker raises its window, as a press on it does.
    fn take_focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) {
            self.send(ScreenRequest::Focus(self.stream));
        }
        self.focus.focus(window, cx);
        cx.emit(ScreenViewEvent::Pressed);
    }

    fn inside(&self, p: Point<Pixels>) -> bool {
        self.bounds.contains(&p)
    }

    /// The pointer from the body's top-left, in view pixels, through the zoom
    /// ([`Self::pointer_spot`]).
    fn cursor_offset(&self, now: Instant) -> (Pixels, Pixels) {
        let (w, h) = self.body_size();
        let (u, v) = self.zoom.to_body(self.pointer_spot(now).0);
        (px(u * w), px(v * h))
    }

    /// The pointer moved over the picture: sent to the worker, and drawn there in the same
    /// frame, not when the worker's sample of it comes back.
    fn mouse_move(&mut self, ev: &MouseMoveEvent, _w: &mut Window, cx: &mut Context<Self>) {
        if !self.inside(ev.position) {
            return;
        }
        let now = cx.background_executor().now();
        let (x, y) = self.to_stream(ev.position);
        self.place((x, y), now);
        let (w, h) = self.mapped_f32();
        if let Some(pad) = self.trackpad.as_mut() {
            pad.place((x / w.max(1.0), y / h.max(1.0)));
        }
        self.input(ScreenInput::Move { x, y });
        self.redraw_pointer(now, cx);
    }

    fn mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.take_focus(window, cx);
        // On glass a double tap is the zoom's: the first tap has clicked already.
        if self.touch && self.trackpad.is_none() && ev.click_count == 2 {
            let target = zoom::double_tap_target(self.one_to_one());
            let max = zoom::max_scale(self.one_to_one());
            self.set_zoom(self.zoom.toggled(self.body_fraction(ev.position), target, max), cx);
            cx.stop_propagation();
            return;
        }
        let button = proto_button(ev.button);
        let now = cx.background_executor().now();
        let (x, y) = self.to_stream(ev.position);
        self.place((x, y), now);
        self.redraw_pointer(now, cx);
        if !self.buttons.contains(&button) {
            self.buttons.push(button);
        }
        self.input(ScreenInput::Button {
            button,
            down: true,
            x,
            y,
            clicks: u8::try_from(ev.click_count).unwrap_or(u8::MAX),
            mods: keys::mods(ev.modifiers),
        });
        cx.stop_propagation();
    }

    /// A button came up, over the picture or anywhere else in the window: released on the
    /// worker if its press went there, at the nearest point of the picture.
    fn mouse_up(&mut self, ev: &MouseUpEvent, _w: &mut Window, cx: &mut Context<Self>) {
        let button = proto_button(ev.button);
        let Some(at) = self.buttons.iter().position(|&b| b == button) else { return };
        self.buttons.swap_remove(at);
        let (x, y) = self.to_stream(ev.position);
        let (w, h) = self.mapped_f32();
        let (x, y) = (x.clamp(0.0, w), y.clamp(0.0, h));
        let now = cx.background_executor().now();
        self.place((x, y), now);
        self.redraw_pointer(now, cx);
        self.input(ScreenInput::Button {
            button,
            down: false,
            x,
            y,
            clicks: u8::try_from(ev.click_count).unwrap_or(u8::MAX),
            mods: keys::mods(ev.modifiers),
        });
    }

    /// The size the worker maps input with, as floats.
    const fn mapped_f32(&self) -> (f32, f32) {
        #[expect(clippy::cast_precision_loss, reason = "pixel counts are small")]
        let mapped = (self.mapped.0 as f32, self.mapped.1 as f32);
        mapped
    }

    fn scroll_wheel(&mut self, ev: &ScrollWheelEvent, _w: &mut Window, cx: &mut Context<Self>) {
        let (dx, dy, precise) = match ev.delta {
            ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y), true),
            ScrollDelta::Lines(l) => (l.x, l.y, false),
        };
        let (x, y) = self.to_stream(ev.position);
        let mods = keys::mods(ev.modifiers);
        // A finger landing on a fling stops it, and macOS closes the old momentum before it
        // opens the new gesture.
        if precise && ev.touch_phase == TouchPhase::Started {
            self.end_momentum(x, y, mods);
        }
        let still = dx.abs() <= f32::EPSILON && dy.abs() <= f32::EPSILON;
        let (phase, momentum) = self.scroll_phases(precise, ev.touch_phase, still);
        self.input(ScreenInput::Scroll { dx, dy, precise, phase, momentum, x, y, mods });
        if precise {
            if matches!(self.scrolling, Scrolling::Momentum { .. }) {
                self.arm_momentum_end(x, y, mods, cx);
            } else {
                self.momentum_end = None;
            }
        }
        cx.stop_propagation();
    }

    /// The `phase` and `momentum` a worker `CGEvent` needs for this wheel event, moving the
    /// gesture on as it goes. A mouse wheel notch is part of no gesture and carries neither.
    const fn scroll_phases(
        &mut self,
        precise: bool,
        touch: TouchPhase,
        still: bool,
    ) -> (ScrollPhase, ScrollPhase) {
        if !precise {
            return (ScrollPhase::None, ScrollPhase::None);
        }
        match (touch, self.scrolling) {
            // macOS closes a coast with an event that moves nothing and carries
            // `momentumPhase = End`. Its `phase()` is none, so gpui hands it over as a plain
            // `Moved`: a momentum event that has stopped moving *is* the end, and reading it
            // here is a frame or two earlier than waiting out `MOMENTUM_GAP` for the same news.
            (TouchPhase::Moved, Scrolling::Momentum { begun: true }) if still => {
                self.scrolling = Scrolling::Idle;
                (ScrollPhase::None, ScrollPhase::Ended)
            }
            // gpui reads `NSEventPhaseMayBegin` and `NSEventPhaseBegan` as the same `Started`, so
            // fingers that rest before they push open the gesture twice. The second one is the
            // same gesture; telling the worker it began again would restart its rubber-banding.
            (TouchPhase::Started, Scrolling::Fingers) => (ScrollPhase::Changed, ScrollPhase::None),
            (TouchPhase::Started, _) => {
                self.scrolling = Scrolling::Fingers;
                (ScrollPhase::Began, ScrollPhase::None)
            }
            // iOS coasts on its own recognizer and closes the coast with one more `Ended`. The
            // fingers lifted a fling ago, so this ends the momentum, not the gesture; reading it
            // as a second gesture end would leave the momentum open forever.
            (TouchPhase::Ended, Scrolling::Momentum { begun: true }) => {
                self.scrolling = Scrolling::Idle;
                (ScrollPhase::None, ScrollPhase::Ended)
            }
            (TouchPhase::Ended, _) => {
                self.scrolling = Scrolling::Momentum { begun: false };
                (ScrollPhase::Ended, ScrollPhase::None)
            }
            (TouchPhase::Cancelled, _) => {
                self.scrolling = Scrolling::Idle;
                (ScrollPhase::Cancelled, ScrollPhase::None)
            }
            (TouchPhase::Moved, Scrolling::Momentum { begun }) => {
                self.scrolling = Scrolling::Momentum { begun: true };
                (ScrollPhase::None, if begun { ScrollPhase::Changed } else { ScrollPhase::Began })
            }
            (TouchPhase::Moved, Scrolling::Idle | Scrolling::Fingers) => {
                (ScrollPhase::Changed, ScrollPhase::None)
            }
        }
    }

    /// Wait out [`MOMENTUM_GAP`] and, if nothing else has arrived, close the fling.
    fn arm_momentum_end(&mut self, x: f32, y: f32, mods: Mods, cx: &Context<Self>) {
        self.momentum_end = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(MOMENTUM_GAP).await;
            let _gone = this.update(cx, |this, _cx| this.end_momentum(x, y, mods));
        }));
    }

    /// The zero-delta `momentumPhase = End` macOS sends when a fling stops. gpui has no event
    /// for it, so without this the remote app is left latched to a scroll that never ended.
    fn end_momentum(&mut self, x: f32, y: f32, mods: Mods) {
        let Scrolling::Momentum { begun } = self.scrolling else { return };
        self.scrolling = Scrolling::Idle;
        self.momentum_end = None;
        // A gesture that ended without a fling behind it is already closed by its own `Ended`.
        if begun {
            self.input(ScreenInput::Scroll {
                dx: 0.0,
                dy: 0.0,
                precise: true,
                phase: ScrollPhase::None,
                momentum: ScrollPhase::Ended,
                x,
                y,
                mods,
            });
        }
    }

    fn key_down(&mut self, ev: &KeyDownEvent, _w: &mut Window, cx: &mut Context<Self>) {
        if is_paste_chord(&ev.keystroke) {
            self.push_clipboard(cx);
        }
        let text = ev.keystroke.key_char.clone().filter(|t| !t.is_empty());
        self.press_key(&ev.keystroke, ev.is_held, text);
        cx.stop_propagation();
    }

    /// A key went down (or repeats): remember it as held and send it.
    fn press_key(&mut self, keystroke: &Keystroke, repeat: bool, text: Option<String>) {
        let code = keys::key_code(&keystroke.key);
        if !self.held.contains(&code) {
            self.held.push(code);
        }
        self.input(ScreenInput::Key {
            code,
            action: if repeat { KeyAction::Repeat } else { KeyAction::Press },
            mods: keys::mods(keystroke.modifiers),
            text,
        });
    }

    /// Focus left the view or the window went inactive: release on the worker every button,
    /// key and modifier whose press went there, since the release never will (⌘-tab away with
    /// ⌘ held, a click on another tile while a key is down) — input stuck down on the worker
    /// is the one thing a remote desktop must never leave behind.
    fn let_go(&mut self, cx: &mut Context<Self>) {
        let (x, y) = self.pointer_at;
        for button in std::mem::take(&mut self.buttons) {
            self.input(ScreenInput::Button {
                button,
                down: false,
                x,
                y,
                clicks: 1,
                mods: Mods::empty(),
            });
        }
        for code in std::mem::take(&mut self.held) {
            self.input(ScreenInput::Key {
                code,
                action: KeyAction::Release,
                mods: Mods::empty(),
                text: None,
            });
        }
        self.modifiers(Modifiers::default(), cx);
    }

    fn key_up(&mut self, ev: &KeyUpEvent, _w: &mut Window, cx: &mut Context<Self>) {
        let code = keys::key_code(&ev.keystroke.key);
        let Some(at) = self.held.iter().position(|&c| c == code) else { return };
        self.held.swap_remove(at);
        self.input(ScreenInput::Key {
            code,
            action: KeyAction::Release,
            mods: keys::mods(ev.keystroke.modifiers),
            text: None,
        });
        cx.stop_propagation();
    }

    fn modifiers_changed(
        &mut self,
        ev: &ModifiersChangedEvent,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modifiers(ev.modifiers, cx);
    }

    /// The modifier keys moved: forward each one that went down or up as its own key, so a
    /// remote program sees ⌘ held on its own, ⌥ pressed over a menu, ⇧ held to run. GPUI
    /// names no side, so the left key stands for both.
    fn modifiers(&mut self, now: Modifiers, cx: &mut Context<Self>) {
        let was = std::mem::replace(&mut self.modifiers, now);
        for (code, action) in modifier_keys(was, now) {
            self.input(ScreenInput::Key { code, action, mods: keys::mods(now), text: None });
        }
        cx.stop_propagation();
    }

    /// Before a paste reaches the worker, make sure it pastes what this client copied: this
    /// client's clipboard offer goes on the (ordered) control stream ahead of the key, when the
    /// worker has not heard it yet. Copied files go to the worker first, and the chord and all
    /// after it wait for them.
    fn push_clipboard(&mut self, cx: &mut Context<Self>) {
        let Some(hook) = self.paste_hook.clone() else { return };
        let ahead = hook();
        if let Some(msg) = ahead.offer {
            self.out.send(msg);
        }
        if let Some(files) = ahead.files {
            self.paste_hold.0 = self.paste_hold.0.saturating_add(1);
            cx.emit(ScreenViewEvent::PasteFiles(files));
        }
    }

    /// What to send ahead of a paste chord.
    pub fn set_paste_hook(&mut self, hook: PasteHook) {
        self.paste_hook = Some(hook);
    }

    /// Whether `which` is armed for the next key.
    #[must_use]
    pub const fn sticky(&self, which: Sticky) -> bool {
        match which {
            Sticky::Control => self.sticky.control,
            Sticky::Command => self.sticky.platform,
        }
    }

    /// Arm or disarm a modifier for the next key (the phone key bar's ⌃ and ⌘).
    pub fn set_sticky(&mut self, which: Sticky, on: bool, cx: &mut Context<Self>) {
        match which {
            Sticky::Control => self.sticky.control = on,
            Sticky::Command => self.sticky.platform = on,
        }
        cx.notify();
    }

    /// Press and release one key on the worker: the phone key bar and the soft keyboard, which
    /// have no key-up of their own. Armed modifiers apply and clear.
    pub fn press(&mut self, mut keystroke: Keystroke, cx: &mut Context<Self>) {
        let armed = std::mem::take(&mut self.sticky);
        keystroke.modifiers.control |= armed.control;
        keystroke.modifiers.platform |= armed.platform;
        if is_paste_chord(&keystroke) {
            self.push_clipboard(cx);
        }
        let code = keys::key_code(&keystroke.key);
        let mods = keys::mods(keystroke.modifiers);
        // A modified key is a chord, not typing: no text, or the worker would insert it too.
        let text = keystroke
            .key_char
            .filter(|t| !t.is_empty() && !mods.intersects(Mods::CTRL | Mods::SUPER));
        self.input(ScreenInput::Key { code, action: KeyAction::Press, mods, text });
        self.input(ScreenInput::Key { code, action: KeyAction::Release, mods, text: None });
        cx.notify();
    }

    /// Touch: a long press over the picture is a right click on the worker (context menus);
    /// a plain drag stays the strip's. Returns whether the gesture was claimed.
    fn long_press(&mut self, ev: &LongPressEvent, cx: &mut Context<Self>) -> bool {
        if ev.phase != TouchPhase::Started || !self.inside(ev.start_position) {
            return false;
        }
        let (x, y) = self.to_stream(ev.start_position);
        for down in [true, false] {
            self.input(ScreenInput::Button {
                button: ProtoButton::Right,
                down,
                x,
                y,
                clicks: 1,
                mods: keys::mods(Modifiers::default()),
            });
        }
        cx.notify();
        true
    }

    /// Type `text` on the worker one key per character, as committed text is, so it lands
    /// where a paste cannot (a login window, a field that refuses paste). Only the first
    /// [`TYPE_MAX`] bytes go, as Jump caps it; returns whether the text was cut.
    ///
    /// It goes `TYPE_BURST` characters at a time, each burst once the last has left the
    /// outbox: a kilobyte at once is two thousand events, past what the outbox holds, and
    /// the presses it drops would be letters missing from a password.
    pub fn type_text(&mut self, text: &str, cx: &Context<Self>) -> bool {
        let text = text.replace("\r\n", "\n");
        let mut end = text.len().min(TYPE_MAX);
        while !text.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        let (typed, cut) = text.split_at(end);
        let idle = self.typing.is_empty();
        self.typing.extend(typed.chars());
        if idle && !self.typing.is_empty() {
            self.typer = Some(cx.spawn(async move |this, cx| {
                while this.update(cx, Self::type_burst).unwrap_or(false) {
                    cx.background_executor().timer(TYPE_STEP).await;
                }
            }));
        }
        !cut.is_empty()
    }

    /// Type the next burst of what waits to be typed, once the outbox has sent the last;
    /// whether more waits.
    fn type_burst(&mut self, cx: &mut Context<Self>) -> bool {
        if self.out.waiting.borrow().is_empty() {
            let take = self.typing.len().min(TYPE_BURST);
            let burst: String = self.typing.drain(..take).collect();
            self.type_keys(&burst, cx);
        }
        !self.typing.is_empty()
    }

    /// One key per character: the worker sees ordinary typing (a single event carrying a whole
    /// string trips apps that read the key code, not the string).
    fn type_keys(&mut self, text: &str, cx: &mut Context<Self>) {
        for c in text.chars() {
            let key = match c {
                '\n' | '\r' => "enter".to_owned(),
                '\t' => "tab".to_owned(),
                ' ' => "space".to_owned(),
                _ => c.to_string(),
            };
            let key_char = (!c.is_control()).then(|| c.to_string());
            self.press(Keystroke { modifiers: Modifiers::default(), key, key_char }, cx);
        }
    }

    /// Whether the system's shortcuts go to the worker while this tile has the keyboard.
    #[must_use]
    pub const fn system_keys(&self) -> bool {
        self.system_keys
    }

    /// Send the system's shortcuts to the worker while this tile has the keyboard, or leave
    /// them to this Mac.
    pub fn set_system_keys(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.system_keys != on {
            self.system_keys = on;
            cx.notify();
        }
    }

    /// A system shortcut's key, taken off this Mac for the worker: pressed (or repeated), or
    /// let go. Its modifiers went already, as the person pressed them; a release whose press
    /// never went here is dropped, and one still held is let go with the rest when the tile
    /// loses the keyboard.
    pub fn system_key(&mut self, code: KeyCode, down: bool, mods: Mods, cx: &mut Context<Self>) {
        let action = if down {
            if !self.held.contains(&code) {
                self.held.push(code);
            }
            KeyAction::Press
        } else {
            let Some(at) = self.held.iter().position(|k| *k == code) else { return };
            self.held.remove(at);
            KeyAction::Release
        };
        self.input(ScreenInput::Key { code, action, mods, text: None });
        cx.notify();
    }

    /// The phone's "paste" key: ⌘V on the worker, this client's clipboard pushed first.
    pub fn paste_key(&mut self, cx: &mut Context<Self>) {
        self.press(chord("v"), cx);
    }

    /// The phone's "copy" key: ⌘C on the worker; the worker's pasteboard then flows back here.
    pub fn copy_key(&mut self, cx: &mut Context<Self>) {
        self.press(chord("c"), cx);
    }

    /// The pointer drawn at `spot`, a point of the picture: the worker's cursor picture when it
    /// has sent one, else a drawn arrow. An element of the pointer's own extent, which tests
    /// find where it is drawn (`screen-pointer`).
    fn cursor_overlay(&self, spot: Option<(f32, f32)>) -> Option<impl IntoElement + use<>> {
        let (w, h) = self.body_size();
        let (u, v) = self.zoom.to_body(spot?);
        let at = point(px(u * w), px(v * h));
        let (bounds, paint) = match &self.pointer {
            Pointer::Image { image, size, hot } => {
                (pointer_bounds(at, *size, *hot), PointerPaint::Image(Arc::clone(image)))
            }
            Pointer::Arrow => {
                let arrow = ARROW.with(Clone::clone)?;
                let extent = arrow.bounds();
                (
                    Bounds { origin: at + extent.origin, size: extent.size },
                    PointerPaint::Arrow(arrow),
                )
            }
        };
        let fill = hsla(self.theme.surfaces.text);
        let outline = hsla(self.theme.surfaces.canvas);
        Some(
            div()
                .absolute()
                .left(bounds.origin.x)
                .top(bounds.origin.y)
                .w(bounds.size.width)
                .h(bounds.size.height)
                .debug_selector(|| "screen-pointer".to_owned())
                .child(
                    canvas(
                        |_bounds, _window, _cx| {},
                        move |bounds, (), window, _cx| match paint {
                            PointerPaint::Image(image) => {
                                let _painted = window.paint_image(
                                    bounds,
                                    bounds,
                                    gpui::Corners::default(),
                                    image,
                                    0,
                                    false,
                                );
                            }
                            PointerPaint::Arrow(arrow) => {
                                let tip = bounds.origin - arrow.bounds().origin;
                                window.paint_path(moved(&arrow.outline, tip), outline);
                                window.paint_path(moved(&arrow.fill, tip), fill);
                            }
                        },
                    )
                    .size_full(),
                ),
        )
    }
}

/// What the pointer overlay paints.
enum PointerPaint {
    Image(Arc<RenderImage>),
    Arrow(Rc<Arrow>),
}

impl Drop for ScreenView {
    fn drop(&mut self) {
        self.send(ScreenRequest::Close(self.stream));
    }
}

/// The view's way to the worker: the connection's outbound queue, and what waits in order for
/// room in it.
///
/// The queue is shared with everything else on the connection, and a full one used to drop
/// whatever was sent, releases included: a key or button whose release was lost stays down on
/// the worker. Now nothing is dropped that lets go of something. While messages wait, a move
/// replaces a move waiting last, since only where the pointer ends up matters.
struct Outbox {
    out: mpsc::Sender<ClientMsg>,
    waiting: Rc<RefCell<VecDeque<ClientMsg>>>,
    wake: Rc<Notify>,
}

impl Outbox {
    /// An outbox into `out`, with the task that moves what waits into it as room frees. The
    /// task outlives the view until what the view left waiting (its `Close`, say) has gone.
    fn new(out: mpsc::Sender<ClientMsg>, cx: &App) -> Self {
        let waiting = Rc::default();
        let wake = Rc::new(Notify::new());
        cx.foreground_executor()
            .spawn(flush(out.clone(), Rc::clone(&waiting), Rc::clone(&wake)))
            .detach();
        Self { out, waiting, wake }
    }

    fn send(&self, msg: ClientMsg) {
        let mut waiting = self.waiting.borrow_mut();
        if waiting.is_empty() {
            match self.out.try_send(msg) {
                Ok(()) => return,
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    tracing::debug!("outbound queue closed");
                    return;
                }
                Err(mpsc::error::TrySendError::Full(msg)) => waiting.push_back(msg),
            }
        } else {
            hold(&mut waiting, msg);
        }
        self.wake.notify_one();
    }
}

impl Drop for Outbox {
    fn drop(&mut self) {
        // The flush task ends once it holds the last reference and nothing waits.
        self.wake.notify_one();
    }
}

/// Move what waits into the queue in order as room frees, until the outbox is gone and nothing
/// waits, or the connection is.
async fn flush(
    out: mpsc::Sender<ClientMsg>,
    waiting: Rc<RefCell<VecDeque<ClientMsg>>>,
    wake: Rc<Notify>,
) {
    loop {
        while !waiting.borrow().is_empty() {
            let Ok(permit) = out.reserve().await else { return };
            let Some(msg) = waiting.borrow_mut().pop_front() else { break };
            permit.send(msg);
        }
        if Rc::strong_count(&waiting) == 1 {
            return;
        }
        wake.notified().await;
    }
}

/// Queue `msg` behind what already waits: a move replaces a move of the same stream waiting
/// last; past [`OUTBOX_DEPTH`] waiting, input is dropped unless it lets go of something.
fn hold(waiting: &mut VecDeque<ClientMsg>, msg: ClientMsg) {
    if let ClientMsg::Screen(ScreenRequest::Input { stream, input: to @ ScreenInput::Move { .. } }) =
        &msg
        && let Some(ClientMsg::Screen(ScreenRequest::Input {
            stream: behind,
            input: last @ ScreenInput::Move { .. },
        })) = waiting.back_mut()
        && behind == stream
    {
        *last = to.clone();
        return;
    }
    if waiting.len() >= OUTBOX_DEPTH && !must_arrive(&msg) {
        tracing::debug!(?msg, "outbound queue full; dropped");
        return;
    }
    waiting.push_back(msg);
}

/// Whether `msg` may not be dropped: anything but input, and input that ends something — a key
/// or button release, a scroll gesture's or momentum's end.
const fn must_arrive(msg: &ClientMsg) -> bool {
    let ClientMsg::Screen(ScreenRequest::Input { input, .. }) = msg else { return true };
    matches!(
        input,
        ScreenInput::Key { action: KeyAction::Release, .. }
            | ScreenInput::Button { down: false, .. }
            | ScreenInput::Scroll { phase: ScrollPhase::Ended | ScrollPhase::Cancelled, .. }
            | ScreenInput::Scroll { momentum: ScrollPhase::Ended | ScrollPhase::Cancelled, .. }
    )
}

const fn proto_button(button: MouseButton) -> ProtoButton {
    match button {
        MouseButton::Left => ProtoButton::Left,
        MouseButton::Right => ProtoButton::Right,
        MouseButton::Middle => ProtoButton::Middle,
        MouseButton::Navigate(gpui::NavigationDirection::Back) => ProtoButton::Back,
        MouseButton::Navigate(gpui::NavigationDirection::Forward) => ProtoButton::Forward,
    }
}

impl Render for ScreenView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let here = window.window_handle();
        if self.let_go.as_ref().is_none_or(|(at, _)| *at != here) {
            // A view moved to another window lets go of what it held in the last.
            if self.let_go.is_some() {
                self.let_go(cx);
            }
            let blur = cx.on_blur(&self.focus, window, |this, _window, cx| this.let_go(cx));
            let inactive = cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    this.let_go(cx);
                }
            });
            let moved =
                cx.observe_window_bounds(window, |this, window, cx| this.follow_screen(window, cx));
            self.let_go = Some((here, [blur, inactive, moved]));
            self.follow_screen(window, cx);
        }
        self.renders = self.renders.wrapping_add(1);
        // Whatever asked for this render, it draws the pointer as it is now: a change that
        // waited for a frame has it.
        let drawn = self.pointer_drawn(cx.background_executor().now());
        self.drawn = drawn;
        self.fold = None;
        let entity = cx.entity();
        let handler = cx.entity();
        let focus = self.focus.clone();
        let record_bounds = canvas(
            move |bounds, window, cx| {
                let scale_factor = window.scale_factor();
                entity.update(cx, |this, _| {
                    this.bounds = bounds;
                    this.scale_factor = scale_factor;
                });
            },
            // Registering as a text input is what raises the soft keyboard on iOS and lets an
            // input method compose; typed text arrives in `replace_text_in_range`.
            move |bounds, (), window, cx| {
                // What this paint put up is timed when the display shows it.
                if let Some(stamp) = handler.update(cx, |view, _cx| view.pacer.painted()) {
                    let view = handler.clone();
                    crate::shown::after_paint(window, cx, move |shown, cx| {
                        view.update(cx, |view, _cx| view.pacer.shown(stamp, shown.presented));
                    });
                }
                window.handle_input(&focus, ElementInputHandler::new(bounds, handler.clone()), cx);
                let dragger = handler.clone();
                window.on_mouse_event(move |event: &TouchDragEvent, phase, window, cx| {
                    if phase != gpui::DispatchPhase::Bubble {
                        return;
                    }
                    if dragger.update(cx, |view, cx| view.touch_drag(event, window, cx)) {
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                });
                window.on_mouse_event(move |event: &LongPressEvent, phase, window, cx| {
                    if phase != gpui::DispatchPhase::Bubble {
                        return;
                    }
                    if handler.update(cx, |view, cx| view.long_press(event, cx)) {
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                });
            },
        )
        // `inset_0` matters: an absolute element without insets sits at its static position,
        // which for a later sibling is *below* the picture, one body-height off.
        .absolute()
        .inset_0();

        let waited = self.latest.is_none() && past_grace("screen-waiting", window, cx);
        let picture = self.latest.as_ref().map_or_else(
            || {
                if !waited {
                    return div().size_full().into_any_element();
                }
                let text = waiting_text(self.source);
                div()
                    // A status, not a picture: it is the only thing a screen reader can be told
                    // while the surface is empty, and it changes when the worker reports the source.
                    .id("screen-waiting")
                    .role(gpui::accesskit::Role::Status)
                    .aria_label(text)
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(self.theme.typography.small()))
                    .text_color(hsla(self.theme.surfaces.text_muted))
                    .font_family(self.theme.typography.ui_family.clone())
                    .child(text)
                    .into_any_element()
            },
            |buffer| {
                let picture = surface(buffer.clone()).object_fit(ObjectFit::Fill);
                if self.zoom.is_fit() {
                    return picture.size_full().into_any_element();
                }
                // Placed in fractions of the body, so a body that changes size between the
                // zoom and this draw keeps the same part of the picture in view.
                let (s, (ox, oy)) = (self.zoom.scale(), self.zoom.origin());
                picture
                    .absolute()
                    .left(relative(ox))
                    .top(relative(oy))
                    .w(relative(s))
                    .h(relative(s))
                    .into_any_element()
            },
        );
        let readout = self.readout.zip(self.readout()).map(|(state, text)| {
            let theme = &self.theme;
            // A pill over a picture floats: over a remote desktop's own white or black a
            // veil of the chrome's grey could vanish, and the lifted surface cannot.
            let pill = kit::tabular(kit::elevate(kit::pill_frame(theme, 1.0), theme))
                .id("zoom-readout")
                .role(gpui::accesskit::Role::Status)
                .aria_label(SharedString::from(text.clone()))
                .text_color(hsla(theme.surfaces.text_secondary))
                .font_family(theme.typography.ui_family.clone())
                .child(text);
            let pill = match state {
                Readout::Shown => pill.into_any_element(),
                Readout::Fading(generation) => pill
                    .with_animation(
                        ("zoom-readout-fade", generation),
                        Animation::new(kit::FADE).with_easing(kit::ease_out()),
                        |el, t| el.opacity(1.0 - t),
                    )
                    .into_any_element(),
            };
            div()
                .absolute()
                .top(px(theme.spacing.xs))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(pill)
        });

        let hud = self.hud_text(cx).map(|(summary, text)| self.hud_panel(&summary, &text, cx));
        div()
            .id("screen")
            // While the view has the keys, the workspace's own chords stand back (`!Screen`).
            .key_context("Screen")
            .role(gpui::accesskit::Role::Image)
            .aria_label(self.a11y_label().clone())
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .overflow_hidden()
            // The tile's body surface: a picture of another aspect sits on the page it would
            // be, not in a grey well.
            .bg(hsla(self.theme.content()))
            .cursor(local_pointer(self.latest.is_some()))
            .on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(Self::key_up))
            .on_modifiers_changed(cx.listener(Self::modifiers_changed))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .on_pinch(cx.listener(Self::pinch))
            .on_action(cx.listener(|this, _: &ToggleTrackpad, _window, cx| {
                this.toggle_trackpad(cx);
            }))
            .child(picture)
            .child(record_bounds)
            .children(self.cursor_overlay(drawn))
            .children(readout)
            .children(hud)
    }
}

impl ScreenView {
    /// The stats overlay: a lifted panel at the picture's top right with the plain line (a
    /// figure past its threshold in the warning tone) and, behind "Details", the engineering
    /// lines in the mono face. It floats: over a remote desktop's own white or black a veil of
    /// the chrome's grey could vanish, and the lifted surface cannot. A press on it stays here.
    fn hud_panel(&self, summary: &[Figure], text: &str, cx: &Context<Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let separator = " \u{b7} ";
        let mut line = String::new();
        let mut runs = Vec::new();
        let font = gpui::font(theme.typography.ui_family.clone());
        let run = |len: usize, tone| gpui::TextRun {
            len,
            font: font.clone(),
            color: hsla(tone),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        for (ix, figure) in summary.iter().enumerate() {
            if ix > 0 {
                line.push_str(separator);
                runs.push(run(separator.len(), s.text_muted));
            }
            line.push_str(&figure.text);
            runs.push(run(figure.text.len(), if figure.warn { s.warn } else { s.text_secondary }));
        }
        let plain = if line.is_empty() {
            div().text_color(hsla(s.text_muted)).child("\u{2026}").into_any_element()
        } else {
            let line = SharedString::from(line);
            div()
                .debug_selector(|| "stream-stats-line".to_owned())
                .whitespace_nowrap()
                .child(gpui::StyledText::new(line).with_runs(runs))
                .into_any_element()
        };
        let open = self.hud_details;
        let details = kit::icon_button(
            theme,
            "stream-stats-details",
            if open {
                crate::icons::IconName::ChevronUp
            } else {
                crate::icons::IconName::ChevronDown
            },
            "Details",
        )
        .aria_expanded(open)
        .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
        .on_click(cx.listener(|this, _ev, _w, cx| this.toggle_hud_details(cx)));
        let lines = open.then(|| {
            div()
                .debug_selector(|| "stream-stats-details-lines".to_owned())
                .pt(px(theme.spacing.xs))
                .border_t_1()
                .border_color(hsla(s.border_subtle))
                .font_family(theme.typography.mono_families.first().cloned().unwrap_or_default())
                .text_size(px(theme.typography.caption()))
                .text_color(hsla(s.text_muted))
                .whitespace_nowrap()
                .child(SharedString::from(text.to_owned()))
        });
        let panel = kit::tabular(kit::elevate(div(), theme))
            .id("stream-stats")
            .debug_selector(|| "stream-stats".to_owned())
            .role(gpui::accesskit::Role::Status)
            .aria_label("Stream stats")
            .flex()
            .flex_col()
            .gap(px(theme.spacing.xs))
            .p(px(theme.spacing.sm))
            .rounded(px(theme.radii.lg))
            .cursor(CursorStyle::Arrow)
            .font_family(theme.typography.ui_family.clone())
            .text_size(px(theme.typography.meta()))
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_ev, _w, cx| cx.stop_propagation())
            .child(
                div().flex().items_center().gap(px(theme.spacing.sm)).child(plain).child(details),
            )
            .children(lines);
        div()
            .absolute()
            .top(px(theme.spacing.sm))
            .right(px(theme.spacing.sm))
            .child(panel)
            .into_any_element()
    }
}

/// The system pointer over the picture: none while a frame is up, since the pointer is drawn on
/// it in the worker's cursor picture (or an arrow, or nothing when the worker's is off the
/// target), and the arrow before the first frame, when there is nothing to point at yet.
const fn local_pointer(showing: bool) -> CursorStyle {
    if showing { CursorStyle::None } else { CursorStyle::Arrow }
}

/// ⌘ + `key`.
fn chord(key: &str) -> Keystroke {
    Keystroke {
        modifiers: Modifiers { platform: true, ..Modifiers::default() },
        key: key.to_owned(),
        key_char: None,
    }
}

impl EntityInputHandler for ScreenView {
    fn text_for_range(
        &mut self,
        _range: std::ops::Range<usize>,
        _adjusted_range: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: 0..0, reversed: false })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        self.marked.as_ref().map(|m| 0..m.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.marked.take().is_some() {
            cx.notify();
        }
    }

    /// Committed text: one key per character so the worker sees ordinary typing (a single
    /// event carrying a whole string trips apps that read the key code, not the string).
    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = None;
        self.type_keys(text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = (!new_text.is_empty()).then(|| new_text.to_owned());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // The pointer stands in for a caret: candidate windows hang there.
        let (dx, dy) = self.cursor_offset(cx.background_executor().now());
        let origin = point(self.bounds.origin.x + dx, self.bounds.origin.y + dy);
        Some(Bounds::new(origin, size(px(1.0), px(16.0))))
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }

    fn text_input_configuration(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> TextInputConfiguration {
        TextInputConfiguration {
            autocorrect: false,
            autocapitalize: Autocapitalize::None,
            suggestions: false,
            input_action: TextInputAction::Enter,
        }
    }
}

/// The modifier keys that went down or up between `was` and `now`, as presses and releases.
/// The fn key is left out: the worker's own fn setting (emoji picker, dictation) would fire.
fn modifier_keys(was: Modifiers, now: Modifiers) -> Vec<(KeyCode, KeyAction)> {
    [
        (was.shift, now.shift, KeyCode::ShiftLeft),
        (was.control, now.control, KeyCode::ControlLeft),
        (was.alt, now.alt, KeyCode::AltLeft),
        (was.platform, now.platform, KeyCode::MetaLeft),
    ]
    .into_iter()
    .filter(|(before, after, _)| before != after)
    .map(|(_, down, code)| (code, if down { KeyAction::Press } else { KeyAction::Release }))
    .collect()
}

/// ⌘V (and ⇧⌘V, "paste and match style"): the chords that make the worker read its pasteboard.
fn is_paste_chord(keystroke: &Keystroke) -> bool {
    let m = keystroke.modifiers;
    m.platform && !m.control && !m.alt && keystroke.key == "v"
}

#[cfg(test)]
mod tests {
    use gpui::AppContext as _;
    use slopty_core::DisplayId;

    use super::*;

    fn chord(s: &str) -> Keystroke {
        Keystroke::parse(s).expect("keystroke")
    }

    /// The placeholder distinguishes "the picture has not got here yet" from "the worker says its
    /// target has not drawn": the second is not a network problem and no amount of asking fixes
    /// it, so the wait must not read like a stall.
    #[test]
    fn the_placeholder_says_which_end_is_waiting() {
        assert_eq!(waiting_text(SourceState::Live), "Waiting for the first frame…");
        assert_eq!(waiting_text(SourceState::Idle), "Waiting for the window to draw…");
    }

    /// The overlay names every number a human needs to judge a stream: age of the picture,
    /// jitter, how long frames wait for their tail, the queue, recovery, stalls, the verdict,
    /// what the audio jitter buffer did, and what the presentation path did with it all.
    #[test]
    fn hud_shows_age_jitter_hold_present_cadence_and_the_verdict() {
        let stats = ScreenStats {
            jitter: Duration::from_micros(1_250),
            hold_p50: Duration::from_millis(2),
            hold_p95: Duration::from_millis(9),
            queue_depth: 1,
            frames_fec: 3,
            frames_lost: 1,
            nacks: 4,
            refreshes: 1,
            stalls: 2,
            stalled_ms: 140,
            stalled: false,
            audio_packets: 50,
            audio_lost: 0,
            audio_concealed: 0,
            audio_underruns: 2,
            audio_trimmed: Duration::from_millis(40),
            audio_stretched: Duration::from_millis(20),
            audio_target: Duration::from_millis(60),
            ..ScreenStats::default()
        };
        let pacing = PacingStats {
            presented: 1_204,
            skipped: 2,
            repeats: 7,
            late: 0,
            latency_p50: Duration::from_micros(5_400),
            latency_p95: Duration::from_micros(11_900),
            latency_max: Duration::from_millis(28),
            decode_p50: Duration::from_micros(2_100),
            interval_p50: Duration::from_micros(16_667),
            interval_jitter: Duration::from_micros(1_400),
            window: 240,
        };
        let text = hud_lines(&HudInput {
            size: (1920, 1080),
            scale: 1.0,
            chroma: Some(Chroma::Full),
            target_fps: 60,
            fps: 59.6,
            mbps: 18.25,
            rtt: Some(Duration::from_micros(9_400)),
            frame_age: Some(Duration::from_millis(12)),
            rate: Some((19_200_000, RateVerdict::Stall, true)),
            stats: &stats,
            pacing: &pacing,
            ui: None,
        });
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines,
            vec![
                "1920×1080 @1.00  ·  4:4:4  ·  age 12 ms",
                "jitter 1.2 ms  ·  hold 2.0 / 9.0 ms  ·  queue 1  ·  fec 3 lost 1 nack 4 refresh 1  ·  stalls 2 (140 ms) flowing  ·  target 19.2 Mb/s hold (stall) (cwnd)",
                "audio 50 lost 0 concealed 0  ·  dry 2  ·  trimmed 40 ms stretched 20 ms  ·  target 60 ms",
                "present 5.4 / 11.9 / 28.0 ms (decode 2.1)  ·  every 16.7 ms ±1.4  ·  shown 1204 skip 2 repeat 7 late 0",
                "ui –",
            ]
        );
        let blank = hud_lines(&HudInput {
            size: (0, 0),
            scale: 0.5,
            chroma: None,
            target_fps: 60,
            fps: 0.0,
            mbps: 0.0,
            rtt: None,
            frame_age: None,
            rate: None,
            stats: &ScreenStats::default(),
            pacing: &PacingStats::default(),
            ui: None,
        });
        assert!(
            blank.contains("chroma –") && blank.contains("age –") && blank.contains("target –"),
            "{blank}"
        );
        assert!(blank.contains("present 0.0 / 0.0 / 0.0 ms"), "{blank}");
    }

    /// A view with nowhere to draw: a detached handle and a channel that collects what it
    /// would send the worker.
    fn view(
        cx: &mut gpui::TestAppContext,
    ) -> (gpui::Entity<ScreenView>, mpsc::Receiver<ClientMsg>) {
        let (out, rx) = mpsc::channel(16);
        let opened = Opened {
            stream: StreamId(4),
            target: CaptureTarget::Display(DisplayId(2)),
            size: (800, 600),
            quality: Quality { scale: 1.0, ..Quality::default() },
        };
        let view = cx.new(|cx| {
            ScreenView::new(opened, ScreenHandle::detached(StreamId(4)), out, Theme::default(), cx)
        });
        (view, rx)
    }

    /// A settings change to the stream's rate, ceiling or depth reaches a live stream as a
    /// `SetQuality` at the scale it holds; a chrome-only change asks nothing.
    #[gpui::test]
    fn new_stream_settings_are_asked_of_a_live_stream(cx: &mut gpui::TestAppContext) {
        let (view, mut rx) = view(cx);
        let mut theme = Theme::default();
        theme.behaviour.copy_on_select = true;
        view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        assert!(sent(&mut rx).is_empty(), "nothing about the stream changed");
        theme.behaviour.stream.fps = 30;
        theme.behaviour.stream.max_bitrate_bps = 8_000_000;
        view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        let asked = sent(&mut rx);
        let [ScreenRequest::SetQuality { stream: StreamId(4), quality }] = asked.as_slice() else {
            panic!("{asked:?}");
        };
        assert_eq!((quality.fps, quality.bitrate_bps), (30, 8_000_000));
        assert_eq!(quality.codec, VideoCodec::Hevc, "8-bit HEVC, the one stream format");
        assert!(
            (quality.scale - 1.0).abs() < f32::EPSILON,
            "the scale follows the tile's width, not the theme"
        );
        view.update(cx, |v, cx| v.set_theme(theme, cx));
        assert!(sent(&mut rx).is_empty(), "the same theme again asks nothing");

        // The muted preference moves the switch only when it changes; the pill's own
        // toggle stands through an unrelated theme change.
        let muted = |cx: &mut gpui::TestAppContext| view.read_with(cx, |v, _| v.muted());
        assert!(!muted(cx));
        let mut theme = Theme::default();
        theme.behaviour.stream.muted = true;
        view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        assert!(muted(cx), "the setting silences a live stream");
        view.update(cx, |v, _| v.toggle_mute());
        theme.behaviour.stream.fps = 30;
        view.update(cx, |v, cx| v.set_theme(theme, cx));
        assert!(!muted(cx), "the pill's toggle stands");
        assert!(!sent(&mut rx).is_empty(), "fps change asked");
    }

    /// The stream asks for the refresh of the screen it is drawn on, up to the settings'
    /// ceiling, whenever its view lands on a screen of another rate; a screen that does not
    /// say counts as 60 Hz.
    #[test]
    fn the_rate_follows_the_screen_up_to_the_ceiling() {
        assert_eq!(stream_fps(120, 120), 120, "a ProMotion screen");
        assert_eq!(stream_fps(120, 60), 60);
        assert_eq!(stream_fps(120, 144), 120, "the ceiling holds");
        assert_eq!(stream_fps(60, 120), 60);
        assert_eq!(stream_fps(120, 0), 60, "a screen that does not say");
        assert_eq!(stream_fps(30, 0), 30);
        let prefs = slopty_theme::StreamPrefs::default();
        assert_eq!(prefs.fps, 120, "the default follows any screen");
        assert_eq!(quality_of(prefs, 1.0, 120).fps, 120);
        assert_eq!(hz_of(Some(Duration::from_micros(8_333))), 120);
        assert_eq!(hz_of(Some(Duration::from_micros(16_667))), 60);
        assert_eq!(hz_of(None), 0);
    }

    #[gpui::test]
    fn a_view_on_a_screen_of_another_rate_asks_for_it(cx: &mut gpui::TestAppContext) {
        let (view, mut rx) = view(cx);
        let fps = |asked: &[ScreenRequest]| -> Vec<u16> {
            asked
                .iter()
                .filter_map(|r| match r {
                    ScreenRequest::SetQuality { quality, .. } => Some(quality.fps),
                    _ => None,
                })
                .collect()
        };
        view.update(cx, |v, _| v.on_screen(Some(1), 120));
        assert_eq!(fps(&sent(&mut rx)), [120], "a ProMotion screen");
        view.update(cx, |v, _| v.on_screen(Some(2), 120));
        assert!(sent(&mut rx).is_empty(), "another screen at the same rate asks nothing");
        view.update(cx, |v, _| v.on_screen(Some(3), 60));
        assert_eq!(fps(&sent(&mut rx)), [60]);
        let mut theme = Theme::default();
        theme.behaviour.stream.fps = 30;
        view.update(cx, |v, cx| v.set_theme(theme, cx));
        assert_eq!(fps(&sent(&mut rx)), [30], "the ceiling came down");
        view.update(cx, |v, _| v.on_screen(Some(1), 120));
        assert!(sent(&mut rx).is_empty(), "no screen takes it past the ceiling");
        assert_eq!(view.read_with(cx, |v, _| v.fps()), 30);
    }

    /// The worker's cursor picture replaces the drawn arrow at the pointer, its hotspot on
    /// the position and its size in points; bytes that do not fill it, or `None`, put the
    /// arrow back.
    #[gpui::test]
    fn the_workers_cursor_picture_is_drawn_at_its_hotspot(cx: &mut gpui::TestAppContext) {
        let (view, _rx) = view(cx);
        assert!(view.read_with(cx, |v, _| v.pointer_picture().is_none()), "the arrow to begin");
        let shape = |bgra: Vec<u8>| CursorShape { w: 4, h: 2, hot_x: 2, hot_y: 1, bgra, scale: 2 };
        view.update(cx, |v, cx| v.set_cursor_shape(Some(shape(vec![0; 32])), cx));
        let picture = view.read_with(cx, |v, _| v.pointer_picture());
        assert_eq!(
            picture,
            Some((size(px(2.0), px(1.0)), point(px(1.0), px(0.5)))),
            "pixels over the backing scale"
        );
        let at = pointer_bounds(
            point(px(10.0), px(20.0)),
            size(px(2.0), px(1.0)),
            point(px(1.0), px(0.5)),
        );
        assert_eq!(at.origin, point(px(9.0), px(19.5)), "the hotspot sits on the pointer");
        assert_eq!(at.size, size(px(2.0), px(1.0)));
        view.update(cx, |v, cx| v.set_cursor_shape(Some(shape(vec![0; 8])), cx));
        assert!(view.read_with(cx, |v, _| v.pointer_picture().is_none()), "short bytes: the arrow");
        view.update(cx, |v, cx| v.set_cursor_shape(Some(shape(vec![0; 32])), cx));
        view.update(cx, |v, cx| v.set_cursor_shape(None, cx));
        assert!(view.read_with(cx, |v, _| v.pointer_picture().is_none()), "none: the arrow");
    }

    /// The client's own pointer hides over a picture that shows a frame, where the worker's is
    /// drawn, and stays the arrow before the first frame.
    #[test]
    fn the_local_pointer_hides_once_a_frame_is_up() {
        assert_eq!(local_pointer(false), CursorStyle::Arrow);
        assert_eq!(local_pointer(true), CursorStyle::None);
    }

    /// A modifier pressed on its own reaches the worker as that key: each one that moves is a
    /// press or a release carrying the new state, an unchanged state sends nothing, and the
    /// fn key is never forwarded.
    #[gpui::test]
    fn modifier_keys_go_to_the_worker_as_they_move(cx: &mut gpui::TestAppContext) {
        let (view, mut rx) = view(cx);
        let key = |req: &ScreenRequest| match req {
            ScreenRequest::Input {
                input: ScreenInput::Key { code, action, mods, text }, ..
            } => Some((*code, *action, *mods, text.clone())),
            _ => None,
        };
        let shift = Modifiers { shift: true, ..Modifiers::default() };
        view.update(cx, |v, cx| v.modifiers(shift, cx));
        assert_eq!(
            sent(&mut rx).iter().filter_map(key).collect::<Vec<_>>(),
            vec![(KeyCode::ShiftLeft, KeyAction::Press, Mods::SHIFT, None)]
        );
        view.update(cx, |v, cx| v.modifiers(shift, cx));
        assert!(sent(&mut rx).is_empty(), "nothing moved");
        let both =
            Modifiers { shift: true, platform: true, function: true, ..Modifiers::default() };
        view.update(cx, |v, cx| v.modifiers(both, cx));
        assert_eq!(
            sent(&mut rx).iter().filter_map(key).collect::<Vec<_>>(),
            vec![(KeyCode::MetaLeft, KeyAction::Press, Mods::SHIFT | Mods::SUPER, None)],
            "⌘ joins ⇧; fn stays local"
        );
        view.update(cx, |v, cx| v.modifiers(Modifiers::default(), cx));
        assert_eq!(
            sent(&mut rx).iter().filter_map(key).collect::<Vec<_>>(),
            vec![
                (KeyCode::ShiftLeft, KeyAction::Release, Mods::empty(), None),
                (KeyCode::MetaLeft, KeyAction::Release, Mods::empty(), None),
            ]
        );
    }

    /// Losing focus (or the window going inactive) releases on the worker every key and
    /// modifier whose press went there, once; with nothing held it sends nothing.
    #[gpui::test]
    fn losing_focus_releases_what_is_held_on_the_worker(cx: &mut gpui::TestAppContext) {
        let (view, mut rx) = view(cx);
        let keys = |reqs: Vec<ScreenRequest>| -> Vec<(KeyCode, KeyAction)> {
            reqs.iter()
                .filter_map(|req| match req {
                    ScreenRequest::Input {
                        input: ScreenInput::Key { code, action, .. }, ..
                    } => Some((*code, *action)),
                    _ => None,
                })
                .collect()
        };
        let cmd = Modifiers { platform: true, ..Modifiers::default() };
        view.update(cx, |v, cx| {
            v.modifiers(cmd, cx);
            let a = Keystroke { modifiers: cmd, key: "a".into(), key_char: None };
            v.press_key(&a, false, None);
            v.press_key(&a, true, None);
        });
        assert_eq!(
            keys(sent(&mut rx)),
            vec![
                (KeyCode::MetaLeft, KeyAction::Press),
                (KeyCode::A, KeyAction::Press),
                (KeyCode::A, KeyAction::Repeat),
            ]
        );
        view.update(cx, ScreenView::let_go);
        assert_eq!(
            keys(sent(&mut rx)),
            vec![(KeyCode::A, KeyAction::Release), (KeyCode::MetaLeft, KeyAction::Release)],
            "the key, then the modifier, let go"
        );
        view.update(cx, ScreenView::let_go);
        assert!(sent(&mut rx).is_empty(), "nothing held: nothing sent");
    }

    /// An instant past the quality cooldown.
    fn past_cooldown() -> Instant {
        Instant::now().checked_sub(Duration::from_secs(1)).unwrap_or_else(Instant::now)
    }

    fn sent(rx: &mut mpsc::Receiver<ClientMsg>) -> Vec<ScreenRequest> {
        let mut out = Vec::new();
        while let Ok(ClientMsg::Screen(req)) = rx.try_recv() {
            out.push(req);
        }
        out
    }

    /// Drawing the tile smaller asks the worker for a smaller picture, in quarter steps, not more
    /// than once per cooldown; a resize from the worker keeps the native size consistent with
    /// the scale in force.
    #[gpui::test]
    fn the_painted_width_asks_for_a_scale_in_quarter_steps_once_per_cooldown(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, mut rx) = view(cx);
        view.update(cx, |v, cx| {
            assert_eq!(v.native(), (800.0, 600.0));
            assert_eq!(v.a11y_label().as_ref(), "Remote display 2");
            // Pinned here, not left to `new`: a loaded machine can spend the cooldown before
            // the ask.
            v.quality_changed = Instant::now();
            v.set_painted_width(300.0, cx);
        });
        assert!(sent(&mut rx).is_empty(), "within the cooldown: nothing asked");
        view.update(cx, |v, cx| {
            v.quality_changed = past_cooldown();
            v.set_painted_width(300.0, cx);
            assert_eq!(v.size(), (400, 300), "300/800 = 0.375 → the 0.5 bucket");
        });
        let asked = sent(&mut rx);
        assert!(
            matches!(asked.as_slice(), [ScreenRequest::SetQuality { stream: StreamId(4), quality }] if (quality.scale - 0.5).abs() < f32::EPSILON),
            "{asked:?}"
        );
        view.update(cx, |v, cx| {
            v.quality_changed = past_cooldown();
            v.set_painted_width(10.0, cx);
            assert_eq!(v.size(), (200, 150), "never below the minimum scale");
            v.quality_changed = past_cooldown();
            v.set_painted_width(10.0, cx);
        });
        assert_eq!(sent(&mut rx).len(), 1, "the same bucket again asks nothing");
        view.update(cx, |v, _| {
            v.set_geometry(100, 50);
            assert_eq!(v.size(), (100, 50));
            assert_eq!(v.native(), (400.0, 200.0), "native follows the scale in force (0.25)");
        });
    }

    /// A width asked for inside the cooldown is not dropped: it is taken when the cooldown
    /// ends, even with nothing drawing the view again, and the latest width asked for wins.
    /// (The overview closing is such a width: without it the window stayed small.)
    #[gpui::test]
    fn a_width_asked_for_inside_the_cooldown_is_taken_when_it_ends(cx: &mut gpui::TestAppContext) {
        let (view, mut rx) = view(cx);
        view.update(cx, |v, cx| {
            v.quality_changed = Instant::now();
            v.set_painted_width(300.0, cx);
            v.set_painted_width(100.0, cx);
        });
        assert!(sent(&mut rx).is_empty(), "within the cooldown: nothing asked yet");
        // The cooldown's clock is the wall's; the timer is the executor's.
        view.update(cx, |v, _| v.quality_changed = past_cooldown());
        cx.executor().advance_clock(QUALITY_COOLDOWN);
        cx.run_until_parked();
        let asked = sent(&mut rx);
        assert!(
            matches!(asked.as_slice(), [ScreenRequest::SetQuality { quality, .. }] if (quality.scale - MIN_SCALE).abs() < f32::EPSILON),
            "the latest width, once: {asked:?}"
        );
        view.update(cx, |v, _| assert_eq!(v.wanted_width, None));
    }

    /// The phone's key bar presses and releases a key in one go; an armed modifier rides on
    /// it once, and a chord carries no text so the worker does not type it as well.
    #[gpui::test]
    fn a_pressed_key_is_a_press_and_a_release_with_the_armed_modifier(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, mut rx) = view(cx);
        view.update(cx, |v, cx| {
            v.set_sticky(Sticky::Command, true, cx);
            assert!(v.sticky(Sticky::Command));
            v.press(chord("a"), cx);
            assert!(!v.sticky(Sticky::Command), "armed for one key only");
        });
        let keys: Vec<(KeyAction, Mods, Option<String>)> = sent(&mut rx)
            .into_iter()
            .filter_map(|req| match req {
                ScreenRequest::Input {
                    input: ScreenInput::Key { action, mods, text, .. }, ..
                } => Some((action, mods, text)),
                _ => None,
            })
            .collect();
        assert_eq!(
            keys,
            [(KeyAction::Press, Mods::SUPER, None), (KeyAction::Release, Mods::SUPER, None)]
        );
        // The soft keyboard's stroke carries the typed character.
        let typed = Keystroke { key_char: Some("b".to_owned()), ..chord("b") };
        view.update(cx, |v, cx| v.press(typed, cx));
        let plain = sent(&mut rx);
        assert!(
            matches!(plain.as_slice(), [ScreenRequest::Input { input: ScreenInput::Key { mods, text: Some(t), .. }, .. }, _] if mods.is_empty() && t == "b"),
            "a plain key types its text: {plain:?}"
        );
        view.update(cx, |v, _| {
            assert!(!v.muted());
            v.toggle_mute();
            assert!(v.muted());
        });
    }

    /// ⌘V sends this client's clipboard offer ahead of the key when the hook has one, and only
    /// the key when the worker already heard it.
    #[gpui::test]
    fn a_paste_chord_sends_the_clipboard_offer_ahead_of_the_key(cx: &mut gpui::TestAppContext) {
        use slopty_proto::transfer::{ClipMsg, Offer, Peer};
        let (view, mut rx) = view(cx);
        let pending = Rc::new(std::cell::Cell::new(true));
        let once = Rc::clone(&pending);
        view.update(cx, |v, _| {
            v.set_paste_hook(Rc::new(move || PasteAhead {
                offer: once.replace(false).then(|| {
                    let origin = Peer::Client(slopty_core::ClientId::new());
                    let offer = Offer { origin, generation: 1, items: Vec::new() };
                    ClientMsg::Clip(ClipMsg::Offer(offer))
                }),
                files: None,
            }));
        });
        let kinds = |rx: &mut mpsc::Receiver<ClientMsg>| {
            std::iter::from_fn(|| rx.try_recv().ok()).map(|m| m.kind()).collect::<Vec<_>>()
        };
        view.update(cx, |v, cx| v.press(chord("cmd-v"), cx));
        assert_eq!(kinds(&mut rx)[..2], ["Clip", "Screen"], "the offer goes first");
        view.update(cx, |v, cx| v.press(chord("cmd-v"), cx));
        assert!(kinds(&mut rx).iter().all(|k| *k == "Screen"), "heard already: the key alone");
    }

    /// ⌘V with files on the clipboard asks the workspace to send them, and holds the chord and
    /// everything typed after it until they are on the worker's pasteboard; then it all goes,
    /// in order.
    #[gpui::test]
    fn a_paste_of_files_holds_the_window_input_until_they_are_there(cx: &mut gpui::TestAppContext) {
        use crate::clipboard::ClipFiles;
        let (view, mut rx) = view(cx);
        let files = ClipFiles::Here(vec![std::path::PathBuf::from("/tmp/a.png")]);
        let offered = files.clone();
        view.update(cx, |v, _| {
            v.set_paste_hook(Rc::new(move || PasteAhead {
                offer: None,
                files: Some(offered.clone()),
            }));
        });
        let asked = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&asked);
        cx.update(|cx| {
            cx.subscribe(&view, move |_view, event, _cx| {
                if let ScreenViewEvent::PasteFiles(files) = event {
                    seen.borrow_mut().push(files.clone());
                }
            })
            .detach();
        });
        let keys = |rx: &mut mpsc::Receiver<ClientMsg>| {
            std::iter::from_fn(|| rx.try_recv().ok())
                .filter_map(|m| match m {
                    ClientMsg::Screen(ScreenRequest::Input {
                        input: ScreenInput::Key { code, action: KeyAction::Press, .. },
                        ..
                    }) => Some(code),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        view.update(cx, |v, cx| v.press(chord("cmd-v"), cx));
        view.update(cx, |v, cx| v.press(chord("x"), cx));
        cx.run_until_parked();
        assert_eq!(*asked.borrow(), [files], "the workspace is asked to send them");
        assert!(keys(&mut rx).is_empty(), "nothing goes before the files");
        assert!(view.read_with(cx, |v, _| v.paste_held()));
        view.update(cx, |v, _| v.release_paste());
        assert_eq!(keys(&mut rx), [KeyCode::V, KeyCode::X], "the chord, then what followed");
        assert!(!view.read_with(cx, |v, _| v.paste_held()));
    }

    #[test]
    fn paste_chords() {
        assert!(is_paste_chord(&chord("cmd-v")));
        assert!(is_paste_chord(&chord("cmd-shift-v")));
        assert!(!is_paste_chord(&chord("v")));
        assert!(!is_paste_chord(&chord("ctrl-v")));
        assert!(!is_paste_chord(&chord("cmd-alt-v")));
        assert!(!is_paste_chord(&chord("cmd-c")));
    }

    /// The view in a window, laid out, so the picture's bounds are recorded and pointer
    /// positions can be mapped to stream pixels.
    fn windowed(
        cx: &mut gpui::TestAppContext,
    ) -> (gpui::Entity<ScreenView>, mpsc::Receiver<ClientMsg>, &mut gpui::VisualTestContext) {
        windowed_on(cx, CaptureTarget::Display(DisplayId(2)))
    }

    /// [`windowed`], streaming `target`.
    fn windowed_on(
        cx: &mut gpui::TestAppContext,
        target: CaptureTarget,
    ) -> (gpui::Entity<ScreenView>, mpsc::Receiver<ClientMsg>, &mut gpui::VisualTestContext) {
        let (out, rx) = mpsc::channel(64);
        let opened = Opened {
            stream: StreamId(4),
            target,
            size: (800, 600),
            quality: Quality { scale: 1.0, ..Quality::default() },
        };
        let (view, cx) = cx.add_window_view(|window, cx| {
            let view = ScreenView::new(
                opened,
                ScreenHandle::detached(StreamId(4)),
                out,
                Theme::default(),
                cx,
            );
            window.focus(&view.focus, cx);
            view
        });
        cx.simulate_resize(size(px(400.0), px(300.0)));
        cx.run_until_parked();
        (view, rx, cx)
    }

    /// A fling reaches the worker shaped the way macOS shapes one: the gesture begins, changes and
    /// ends, and everything after that is momentum, which begins, continues and is closed by a
    /// zero-delta end of its own. gpui reports none of that — it reads `NSEvent.phase` and never
    /// `momentumPhase`, so momentum arrives as plain `Moved` events after `Ended` — and passing
    /// them on unchanged would tell the remote app the scroll ended and then went on changing.
    #[gpui::test]
    fn a_fling_over_the_picture_reaches_the_worker_as_a_gesture_and_then_as_momentum(
        cx: &mut gpui::TestAppContext,
    ) {
        use ScrollPhase::{Began, Changed, Ended, None as Off};

        /// Long enough that the gap timer has certainly fired, whatever the clock's grain.
        const PAST_THE_GAP: Duration = Duration::from_millis(500);

        let (view, mut rx, cx) = windowed(cx);
        drop(sent(&mut rx));
        let bounds = view.read_with(cx, |v, _| v.bounds);
        let at = bounds.center();
        let wheel = |cx: &mut gpui::VisualTestContext, dy: f32, touch_phase| {
            cx.simulate_event(ScrollWheelEvent {
                position: at,
                delta: ScrollDelta::Pixels(point(px(0.0), px(dy))),
                modifiers: Modifiers::default(),
                touch_phase,
            });
            cx.run_until_parked();
        };
        let phases = |rx: &mut mpsc::Receiver<ClientMsg>| {
            inputs(rx)
                .into_iter()
                .filter_map(|i| match i {
                    ScreenInput::Scroll { phase, momentum, .. } => Some((phase, momentum)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };

        wheel(cx, -8.0, TouchPhase::Started);
        wheel(cx, -30.0, TouchPhase::Moved);
        wheel(cx, -30.0, TouchPhase::Ended);
        assert_eq!(
            phases(&mut rx),
            [(Began, Off), (Changed, Off), (Ended, Off)],
            "the fingers' own part of the fling"
        );

        // Everything after `Ended` is the fling coasting: no scroll phase at all, and a
        // momentum phase that begins once and then continues.
        wheel(cx, -20.0, TouchPhase::Moved);
        wheel(cx, -12.0, TouchPhase::Moved);
        wheel(cx, -5.0, TouchPhase::Moved);
        assert_eq!(phases(&mut rx), [(Off, Began), (Off, Changed), (Off, Changed)], "the coast");

        // The fling stops, and macOS says so with an event that moves nothing. Reading that
        // closes the coast on the spot instead of waiting out `MOMENTUM_GAP` for the same news.
        wheel(cx, 0.0, TouchPhase::Moved);
        assert_eq!(phases(&mut rx), [(Off, Ended)], "the coast ends when it stops moving");
        // And it is closed only once: the gap timer has nothing left to close.
        cx.executor().advance_clock(PAST_THE_GAP);
        cx.run_until_parked();
        assert!(inputs(&mut rx).is_empty(), "the fling is closed once");

        // If that last event is lost, the silence itself still closes the coast, with the
        // zero-delta end the worker needs to let the gesture go.
        wheel(cx, -8.0, TouchPhase::Started);
        wheel(cx, -30.0, TouchPhase::Ended);
        wheel(cx, -20.0, TouchPhase::Moved);
        drop(inputs(&mut rx));
        cx.executor().advance_clock(PAST_THE_GAP);
        cx.run_until_parked();
        let tail = inputs(&mut rx);
        match tail.as_slice() {
            [ScreenInput::Scroll { dx, dy, phase: Off, momentum: Ended, .. }] => {
                assert!(dx.abs() < f32::EPSILON && dy.abs() < f32::EPSILON, "{tail:?}");
            }
            other => panic!("expected one zero-delta momentum end, got {other:?}"),
        }

        // iOS coasts through gpui's own touch recognizer, whose last momentum step carries
        // `Ended`. That closes the coast; it is not the gesture ending a second time.
        wheel(cx, -8.0, TouchPhase::Started);
        wheel(cx, -30.0, TouchPhase::Ended);
        wheel(cx, -20.0, TouchPhase::Moved);
        wheel(cx, -4.0, TouchPhase::Ended);
        assert_eq!(
            phases(&mut rx),
            [(Began, Off), (Ended, Off), (Off, Began), (Off, Ended)],
            "the coast closes itself on iOS"
        );
        cx.executor().advance_clock(PAST_THE_GAP);
        cx.run_until_parked();
        assert!(inputs(&mut rx).is_empty(), "and the backstop has nothing left to close");

        // Fingers that rest on the trackpad before they push open the gesture twice, because
        // gpui reads `MayBegin` and `Began` as the same phase. The worker is told once.
        wheel(cx, 0.0, TouchPhase::Started);
        wheel(cx, 0.0, TouchPhase::Started);
        wheel(cx, -30.0, TouchPhase::Moved);
        wheel(cx, -30.0, TouchPhase::Ended);
        assert_eq!(
            phases(&mut rx),
            [(Began, Off), (Changed, Off), (Changed, Off), (Ended, Off)],
            "resting fingers open the gesture once"
        );

        // A drag that ends without a fling behind it needs no closing: its own `Ended` did it.
        wheel(cx, -8.0, TouchPhase::Started);
        wheel(cx, -8.0, TouchPhase::Ended);
        drop(sent(&mut rx));
        cx.executor().advance_clock(PAST_THE_GAP);
        cx.run_until_parked();
        assert!(inputs(&mut rx).is_empty(), "nothing coasting, nothing to close");
    }

    fn inputs(rx: &mut mpsc::Receiver<ClientMsg>) -> Vec<ScreenInput> {
        sent(rx)
            .into_iter()
            .filter_map(|req| match req {
                ScreenRequest::Input { stream: StreamId(4), input } => Some(input),
                _ => None,
            })
            .collect()
    }

    /// The pointer reaches the worker in the stream's pixels: a point on the painted picture
    /// scales by the stream size over the picture's bounds. A press carries its button, click
    /// count and modifiers, a release the same, a move only while over the picture, and a
    /// scroll its deltas with the unit (pixels are precise, lines are not) and the phases a
    /// `CGEvent` needs, ⌘ included.
    #[gpui::test]
    fn pointer_and_scroll_reach_the_worker_in_stream_pixels(cx: &mut gpui::TestAppContext) {
        let (view, mut rx, cx) = windowed(cx);
        drop(sent(&mut rx));
        let bounds = view.read_with(cx, |v, _| v.bounds);
        assert!(bounds.size.width > px(0.0), "laid out: {bounds:?}");
        // A point a quarter of the way across and half-way down the picture.
        let at = |fx: f32, fy: f32| {
            point(
                bounds.origin.x + bounds.size.width * fx,
                bounds.origin.y + bounds.size.height * fy,
            )
        };
        let near =
            |(x, y): (f32, f32), ex: f32, ey: f32| (x - ex).abs() < 1.0 && (y - ey).abs() < 1.0;

        cx.simulate_mouse_move(at(0.25, 0.5), None, Modifiers::default());
        cx.simulate_mouse_down(
            at(0.25, 0.5),
            MouseButton::Left,
            Modifiers { shift: true, ..Modifiers::default() },
        );
        cx.simulate_mouse_up(at(0.5, 0.25), MouseButton::Left, Modifiers::default());
        cx.simulate_event(ScrollWheelEvent {
            position: at(0.5, 0.5),
            delta: ScrollDelta::Pixels(point(px(3.0), px(-12.0))),
            modifiers: Modifiers::default(),
            touch_phase: TouchPhase::Moved,
        });
        cx.simulate_event(ScrollWheelEvent {
            position: at(0.5, 0.5),
            delta: ScrollDelta::Lines(point(0.0, 2.0)),
            modifiers: Modifiers { alt: true, ..Modifiers::default() },
            touch_phase: TouchPhase::Started,
        });
        cx.simulate_event(ScrollWheelEvent {
            position: at(0.5, 0.5),
            delta: ScrollDelta::Lines(point(0.0, 1.0)),
            modifiers: Modifiers { platform: true, ..Modifiers::default() },
            touch_phase: TouchPhase::Moved,
        });
        cx.run_until_parked();

        let got = inputs(&mut rx);
        assert_eq!(got.len(), 6, "{got:?}");
        match &got[0] {
            ScreenInput::Move { x, y } => assert!(near((*x, *y), 200.0, 300.0), "{got:?}"),
            other => panic!("expected a move, got {other:?}"),
        }
        match &got[1] {
            ScreenInput::Button {
                button: ProtoButton::Left,
                down: true,
                x,
                y,
                clicks: 1,
                mods,
            } => {
                assert!(near((*x, *y), 200.0, 300.0), "{got:?}");
                assert_eq!(*mods, Mods::SHIFT);
            }
            other => panic!("expected a press, got {other:?}"),
        }
        match &got[2] {
            ScreenInput::Button {
                button: ProtoButton::Left,
                down: false,
                x,
                y,
                clicks: 1,
                mods,
            } => {
                assert!(near((*x, *y), 400.0, 150.0), "{got:?}");
                assert_eq!(*mods, Mods::empty());
            }
            other => panic!("expected a release, got {other:?}"),
        }
        match &got[3] {
            ScreenInput::Scroll { dx, dy, precise: true, phase, momentum, x, y, mods } => {
                assert_eq!((*dx, *dy), (3.0, -12.0));
                assert_eq!((*phase, *momentum), (ScrollPhase::Changed, ScrollPhase::None));
                assert!(near((*x, *y), 400.0, 300.0), "{got:?}");
                assert_eq!(*mods, Mods::empty());
            }
            other => panic!("expected a precise scroll, got {other:?}"),
        }
        match &got[4] {
            ScreenInput::Scroll { dx, dy, precise: false, phase, momentum, mods, .. } => {
                assert_eq!((*dx, *dy), (0.0, 2.0));
                // A wheel notch is part of no gesture, whatever phase gpui puts on it.
                assert_eq!((*phase, *momentum), (ScrollPhase::None, ScrollPhase::None));
                assert_eq!(*mods, Mods::ALT);
            }
            other => panic!("expected a line scroll, got {other:?}"),
        }
        match &got[5] {
            ScreenInput::Scroll { dy, precise: false, mods, .. } => {
                assert!((*dy - 1.0).abs() < f32::EPSILON, "{got:?}");
                assert_eq!(*mods, Mods::SUPER, "⌘-scroll is the worker's, as it would be locally");
            }
            other => panic!("expected a ⌘ line scroll, got {other:?}"),
        }

        // Off the picture the pointer belongs to another tile: a move there sends nothing.
        cx.simulate_mouse_move(
            point(bounds.origin.x - px(5.0), bounds.origin.y - px(5.0)),
            None,
            Modifiers::default(),
        );
        cx.run_until_parked();
        assert!(inputs(&mut rx).is_empty());
    }

    /// A button pressed on the picture is let go on the worker however the press ends: released
    /// off the picture (at the picture's edge, where the worker's pointer stopped), or held
    /// while focus leaves. A release with no press behind it sends nothing.
    #[gpui::test]
    fn a_held_button_is_released_off_the_picture_and_on_focus_loss(cx: &mut gpui::TestAppContext) {
        let (view, mut rx, cx) = windowed(cx);
        drop(sent(&mut rx));
        let bounds = view.read_with(cx, |v, _| v.bounds);
        let middle = bounds.center();
        let released = |got: &[ScreenInput]| -> Vec<(ProtoButton, f32, f32)> {
            got.iter()
                .filter_map(|input| match input {
                    ScreenInput::Button { button, down: false, x, y, .. } => {
                        Some((*button, *x, *y))
                    }
                    _ => None,
                })
                .collect()
        };

        cx.simulate_mouse_down(middle, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(
            point(bounds.right() + px(40.0), bounds.bottom() + px(40.0)),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.run_until_parked();
        let got = inputs(&mut rx);
        assert_eq!(
            released(&got),
            [(ProtoButton::Left, 800.0, 600.0)],
            "released past the corner: at the corner, {got:?}"
        );

        cx.simulate_mouse_up(middle, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        assert!(inputs(&mut rx).is_empty(), "nothing held: nothing to release");

        cx.simulate_mouse_down(middle, MouseButton::Right, Modifiers::default());
        cx.run_until_parked();
        drop(inputs(&mut rx));
        view.update(cx, ScreenView::let_go);
        let got = inputs(&mut rx);
        assert_eq!(released(&got), [(ProtoButton::Right, 400.0, 300.0)], "{got:?}");
        view.update(cx, ScreenView::let_go);
        assert!(inputs(&mut rx).is_empty(), "let go once");
    }

    /// Everything the view sends, in order, letting its outbox refill the queue after each
    /// message is taken.
    fn drain(cx: &gpui::TestAppContext, rx: &mut mpsc::Receiver<ClientMsg>) -> Vec<ScreenInput> {
        let mut got = Vec::new();
        loop {
            cx.run_until_parked();
            match rx.try_recv() {
                Ok(ClientMsg::Screen(ScreenRequest::Input { input, .. })) => got.push(input),
                Ok(other) => panic!("{other:?}"),
                Err(_) => return got,
            }
        }
    }

    /// A full outbound queue never loses a release. What does not fit waits and goes, in order,
    /// as room frees; moves waiting behind each other go as the last one; past the outbox's depth
    /// a press is dropped and every release is still kept.
    #[gpui::test]
    fn a_full_queue_coalesces_moves_and_keeps_every_release(cx: &mut gpui::TestAppContext) {
        let (out, mut rx) = mpsc::channel(1);
        let opened = Opened {
            stream: StreamId(4),
            target: CaptureTarget::Display(DisplayId(2)),
            size: (800, 600),
            quality: Quality { scale: 1.0, ..Quality::default() },
        };
        let view = cx.new(|cx| {
            ScreenView::new(opened, ScreenHandle::detached(StreamId(4)), out, Theme::default(), cx)
        });
        let mv = |x: f32| ScreenInput::Move { x, y: 0.0 };
        let key =
            |action| ScreenInput::Key { code: KeyCode::A, action, mods: Mods::SUPER, text: None };
        let button = |down| ScreenInput::Button {
            button: ProtoButton::Left,
            down,
            clicks: 1,
            x: 0.0,
            y: 0.0,
            mods: Mods::empty(),
        };
        view.update(cx, |v, _| {
            for input in [
                mv(1.0),
                button(true),
                mv(2.0),
                mv(3.0),
                mv(4.0),
                key(KeyAction::Press),
                mv(5.0),
                mv(6.0),
                key(KeyAction::Release),
                button(false),
            ] {
                v.input(input);
            }
        });
        assert_eq!(
            drain(cx, &mut rx),
            [
                mv(1.0),
                button(true),
                mv(4.0),
                key(KeyAction::Press),
                mv(6.0),
                key(KeyAction::Release),
                button(false),
            ]
        );

        view.update(cx, |v, _| {
            v.input(mv(0.0));
            for _ in 0..=OUTBOX_DEPTH {
                v.input(key(KeyAction::Press));
            }
            v.input(key(KeyAction::Release));
            v.input(button(false));
        });
        let got = drain(cx, &mut rx);
        assert_eq!(got.len(), 1 + OUTBOX_DEPTH + 2, "one press past the depth dropped");
        assert_eq!(got[OUTBOX_DEPTH + 1..], [key(KeyAction::Release), button(false)]);
    }

    /// Once the view has asked for a smaller picture, the pointer maps into the size it asked
    /// for even while frames of the old size still arrive: the worker takes the new scale in
    /// order with the input behind it, before those frames stop.
    #[gpui::test]
    fn input_maps_with_the_scale_asked_for_not_the_frame_in_flight(cx: &mut gpui::TestAppContext) {
        let (view, mut rx, cx) = windowed(cx);
        view.update(cx, |v, cx| {
            v.quality_changed = past_cooldown();
            v.set_painted_width(300.0, cx);
            // A frame at the old scale lands after the ask, and the picture takes its size.
            v.size = (800, 600);
        });
        let asked = sent(&mut rx);
        assert!(
            matches!(asked.as_slice(), [ScreenRequest::SetQuality { quality, .. }] if (quality.scale - 0.5).abs() < f32::EPSILON),
            "{asked:?}"
        );
        let bounds = view.read_with(cx, |v, _| v.bounds);
        let at = point(
            bounds.origin.x + bounds.size.width * 0.25,
            bounds.origin.y + bounds.size.height * 0.5,
        );
        cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        let got = inputs(&mut rx);
        assert!(
            matches!(got.as_slice(), [ScreenInput::Button { x, y, .. }]
                if (x - 100.0).abs() < 1.0 && (y - 150.0).abs() < 1.0),
            "a quarter across and half down the 400×300 stream asked for: {got:?}"
        );
    }

    /// The worker's pointer, drawn and standing in for the IME caret, scales by the size input
    /// maps with: a frame at the old scale landing after the ask does not move it, the sample
    /// the worker sent before the ask stays where it was, and so does the worker's first sample
    /// at the new scale.
    #[gpui::test]
    fn the_workers_pointer_is_drawn_at_the_scale_asked_for(cx: &mut gpui::TestAppContext) {
        let (view, _rx, cx) = windowed(cx);
        let caret = |view: &gpui::Entity<ScreenView>, cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| {
                view.update(cx, |v, cx| {
                    v.bounds_for_range(0..0, Bounds::default(), window, cx).map(|b| b.origin)
                })
            })
        };
        let bounds = view.read_with(cx, |v, _| v.bounds);
        let half_quarter = point(
            bounds.origin.x + bounds.size.width * 0.5,
            bounds.origin.y + bounds.size.height * 0.25,
        );
        view.update(cx, |v, _| v.cursor = CursorState { x: 400, y: 150, visible: true });
        assert_eq!(caret(&view, cx), Some(half_quarter), "on the 800×600 stream");
        view.update(cx, |v, cx| {
            v.quality_changed = past_cooldown();
            v.set_painted_width(300.0, cx);
            v.size = (800, 600);
        });
        assert_eq!(caret(&view, cx), Some(half_quarter), "the sample sent before the ask");
        view.update(cx, |v, _| v.cursor = CursorState { x: 200, y: 75, visible: true });
        assert_eq!(caret(&view, cx), Some(half_quarter), "the first at the 400×300 asked for");
    }

    /// The button presses among `got`: button, down, and where.
    fn presses(got: &[ScreenInput]) -> Vec<(ProtoButton, bool, f32, f32, u8)> {
        got.iter()
            .filter_map(|input| match input {
                ScreenInput::Button { button, down, x, y, clicks, .. } => {
                    Some((*button, *down, *x, *y, *clicks))
                }
                _ => None,
            })
            .collect()
    }

    /// A point of the view's body at fractions `(fx, fy)` of it.
    fn at_fraction(
        view: &gpui::Entity<ScreenView>,
        cx: &gpui::VisualTestContext,
        fx: f32,
        fy: f32,
    ) -> Point<Pixels> {
        let b = view.read_with(cx, |v, _| v.bounds);
        point(b.origin.x + b.size.width * fx, b.origin.y + b.size.height * fy)
    }

    /// The pointer's offset from the body's top-left as it is drawn now, in points.
    fn offset(view: &gpui::Entity<ScreenView>, cx: &gpui::VisualTestContext) -> (f32, f32) {
        view.read_with(cx, |v, cx| {
            let (dx, dy) = v.cursor_offset(cx.background_executor().now());
            (f32::from(dx), f32::from(dy))
        })
    }

    #[track_caller]
    fn near_px(got: (f32, f32), want: (f32, f32)) {
        assert!(
            (got.0 - want.0).abs() < 0.5 && (got.1 - want.1).abs() < 0.5,
            "{got:?} vs {want:?}"
        );
    }

    /// A tap lands on the stream pixel drawn under it at any zoom and pan: at fit, zoomed about
    /// the middle, zoomed into either corner (where the pan is held at the picture's edge
    /// however far it is pushed), and a release off the body lands on the picture's edge.
    #[gpui::test]
    fn a_tap_lands_on_the_pixel_drawn_under_it_at_any_zoom(cx: &mut gpui::TestAppContext) {
        let (view, mut rx, cx) = windowed(cx);
        drop(sent(&mut rx));
        let tap = |cx: &mut gpui::VisualTestContext, rx: &mut mpsc::Receiver<ClientMsg>, fx, fy| {
            let at = at_fraction(&view, cx, fx, fy);
            cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::default());
            cx.run_until_parked();
            let got = presses(&inputs(rx));
            assert_eq!(got.len(), 2, "{got:?}");
            let (_, down, x, y, _) = got[0];
            assert!(down);
            (x, y)
        };
        let set = |cx: &mut gpui::VisualTestContext, zoom: Zoom| {
            view.update(cx, |v, cx| {
                v.zoom = zoom;
                cx.notify();
            });
            cx.run_until_parked();
        };
        near_px(tap(cx, &mut rx, 0.25, 0.5), (200.0, 300.0));
        set(cx, Zoom::FIT.about((0.5, 0.5), 2.0, 8.0));
        near_px(tap(cx, &mut rx, 0.25, 0.5), (300.0, 300.0));
        near_px(tap(cx, &mut rx, 0.5, 0.5), (400.0, 300.0));
        set(cx, Zoom::FIT.about((0.0, 0.0), 4.0, 8.0).panned((5.0, 5.0), 8.0));
        near_px(tap(cx, &mut rx, 0.0, 0.0), (0.0, 0.0));
        // The body's far edge is not on it: just inside.
        near_px(tap(cx, &mut rx, 0.99, 0.99), (198.0, 148.5));
        set(cx, Zoom::FIT.about((1.0, 1.0), 4.0, 8.0).panned((-5.0, -5.0), 8.0));
        near_px(tap(cx, &mut rx, 0.0, 0.0), (600.0, 450.0));
        near_px(tap(cx, &mut rx, 0.99, 0.99), (798.0, 598.5));
        near_px(tap(cx, &mut rx, 0.5, 0.5), (700.0, 525.0));

        // Pressed on the body, let go far past its corner: at the picture's corner.
        let inside = at_fraction(&view, cx, 0.5, 0.5);
        let past = at_fraction(&view, cx, 3.0, 3.0);
        cx.simulate_mouse_down(inside, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(past, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        let got = presses(&inputs(&mut rx));
        assert_eq!(got.last().map(|p| (p.1, p.2, p.3)), Some((false, 800.0, 600.0)), "{got:?}");

        // The worker's pointer, once this client's taps no longer hold it, is drawn through
        // the same zoom: its pixel is drawn where a tap would send it.
        cx.executor().advance_clock(LOCAL_HOLD);
        view.update(cx, |v, _| v.cursor = CursorState { x: 700, y: 525, visible: true });
        near_px(offset(&view, cx), (200.0, 150.0));
    }

    /// On glass a double tap goes to one to one about the tapped point and back to fit, and its
    /// second tap is not sent (the first has clicked); with a pointer, a double click is a
    /// double click.
    #[gpui::test]
    fn a_double_tap_toggles_fit_and_one_to_one_on_glass(cx: &mut gpui::TestAppContext) {
        let (view, mut rx, cx) = windowed(cx);
        drop(sent(&mut rx));
        let tap = |cx: &mut gpui::VisualTestContext, at, clicks| {
            cx.simulate_event(MouseDownEvent {
                button: MouseButton::Left,
                position: at,
                modifiers: Modifiers::default(),
                click_count: clicks,
                first_mouse: false,
            });
            cx.simulate_event(MouseUpEvent {
                button: MouseButton::Left,
                position: at,
                modifiers: Modifiers::default(),
                click_count: clicks,
            });
            cx.run_until_parked();
        };
        let at = at_fraction(&view, cx, 0.3, 0.6);
        tap(cx, at, 2);
        assert!(view.read_with(cx, |v, _| v.zoom.is_fit()), "a pointer's double click: no zoom");
        assert_eq!(presses(&inputs(&mut rx)).len(), 2, "and it is sent");

        view.update(cx, |v, _| v.touch = true);
        tap(cx, at, 1);
        tap(cx, at, 2);
        let (zoom, one) = view.read_with(cx, |v, _| (v.zoom, v.one_to_one()));
        let clicks = presses(&inputs(&mut rx));
        assert_eq!(clicks.len(), 2, "the first tap clicked, the second did not: {clicks:?}");
        assert!((zoom.scale() - zoom::double_tap_target(one)).abs() < 1e-4, "{zoom:?} at {one}");
        let f = zoom.to_picture((0.3, 0.6));
        assert!((f.0 - 0.3).abs() < 1e-4 && (f.1 - 0.6).abs() < 1e-4, "about the tap: {f:?}");
        tap(cx, at, 1);
        tap(cx, at, 2);
        assert_eq!(view.read_with(cx, |v, _| v.zoom), Zoom::FIT, "and back");
    }

    /// Two fingers zoom about their centroid and pan the zoomed picture as the centroid moves;
    /// tapped and lifted at once on glass they right-click where they were. A pinch that begins
    /// off the picture is not the view's.
    #[gpui::test]
    fn two_fingers_zoom_pan_and_tap(cx: &mut gpui::TestAppContext) {
        let (view, mut rx, cx) = windowed(cx);
        drop(sent(&mut rx));
        view.update(cx, |v, _| {
            v.touch = true;
            // A 4K target on this body, so the zoom has room.
            v.native = (3200.0, 2400.0);
        });
        let pinch = |cx: &mut gpui::VisualTestContext, at, delta, phase| {
            cx.simulate_event(PinchEvent {
                position: at,
                delta,
                modifiers: Modifiers::default(),
                phase,
            });
            cx.run_until_parked();
        };
        let middle = at_fraction(&view, cx, 0.5, 0.5);
        pinch(cx, middle, 0.0, TouchPhase::Started);
        pinch(cx, middle, 1.0, TouchPhase::Moved);
        let z = view.read_with(cx, |v, _| v.zoom);
        assert!((z.scale() - 2.0).abs() < 1e-4 && (z.origin().0 + 0.5).abs() < 1e-4, "{z:?}");
        // The centroid goes 40 pt right with no scale: the picture follows it.
        let right = at_fraction(&view, cx, 0.6, 0.5);
        pinch(cx, right, 0.0, TouchPhase::Moved);
        pinch(cx, right, 0.0, TouchPhase::Ended);
        let z = view.read_with(cx, |v, _| v.zoom);
        assert!((z.origin().0 + 0.4).abs() < 1e-4, "panned a tenth of the body: {z:?}");
        assert!(presses(&inputs(&mut rx)).is_empty(), "a pinch clicks nothing");

        pinch(cx, middle, 0.0, TouchPhase::Started);
        pinch(cx, middle, 0.01, TouchPhase::Moved);
        pinch(cx, middle, 0.0, TouchPhase::Ended);
        let want = view.read_with(cx, |v, _| v.to_stream(middle));
        let got = presses(&inputs(&mut rx));
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!((got[0].0, got[0].1), (ProtoButton::Right, true));
        near_px((got[0].2, got[0].3), want);

        let off = at_fraction(&view, cx, 1.5, 0.5);
        let before = view.read_with(cx, |v, _| v.zoom);
        pinch(cx, off, 0.0, TouchPhase::Started);
        pinch(cx, off, 0.5, TouchPhase::Moved);
        pinch(cx, off, 0.0, TouchPhase::Ended);
        assert_eq!(view.read_with(cx, |v, _| v.zoom), before, "not over the picture");
    }

    /// Trackpad mode: a finger drag moves the pointer relatively (point for point while
    /// aiming, further when fast), sends it to the worker and draws it there at once; a tap
    /// clicks at the pointer, wherever the finger is; two fingers dragged scroll at the pointer;
    /// zoomed in, the view pans to keep the pointer in sight. Turning the mode off lets go.
    #[gpui::test]
    fn trackpad_mode_moves_a_pointer_and_clicks_where_it_is(cx: &mut gpui::TestAppContext) {
        let (view, mut rx, cx) = windowed(cx);
        drop(sent(&mut rx));
        view.update(cx, |v, cx| {
            v.native = (3200.0, 2400.0);
            v.toggle_trackpad(cx);
            assert!(v.trackpad());
        });
        let drag =
            |cx: &mut gpui::VisualTestContext, start, to: &[Point<Pixels>], wait: Duration| {
                let ev =
                    |phase, position| TouchDragEvent { phase, start_position: start, position };
                cx.simulate_event(ev(TouchPhase::Started, start));
                for p in to {
                    cx.executor().advance_clock(wait);
                    cx.simulate_event(ev(TouchPhase::Moved, *p));
                }
                cx.simulate_event(ev(TouchPhase::Ended, to.last().copied().unwrap_or(start)));
                cx.run_until_parked();
            };
        let a = at_fraction(&view, cx, 0.1, 0.1);
        // 10 pt in 100 ms: aiming, point for point. The pointer began in the middle.
        drag(cx, a, &[point(a.x + px(10.0), a.y)], Duration::from_millis(100));
        let got = inputs(&mut rx);
        assert!(
            matches!(got.as_slice(), [ScreenInput::Move { x, y }] if (x - 420.0).abs() < 0.5 && (y - 300.0).abs() < 0.5),
            "{got:?}"
        );
        near_px(offset(&view, cx), (210.0, 150.0));

        // A tap far from the pointer clicks at the pointer.
        let far = at_fraction(&view, cx, 0.9, 0.9);
        drag(cx, far, &[], Duration::ZERO);
        let got = presses(&inputs(&mut rx));
        assert_eq!(
            got.iter().map(|p| (p.0, p.1, p.4)).collect::<Vec<_>>(),
            [(ProtoButton::Left, true, 1), (ProtoButton::Left, false, 1)]
        );
        for p in &got {
            near_px((p.2, p.3), (420.0, 300.0));
        }

        // 40 pt in 10 ms is a flick: three times as far.
        cx.executor().advance_clock(Duration::from_secs(1));
        drag(cx, a, &[point(a.x + px(40.0), a.y)], Duration::from_millis(10));
        let x = view.read_with(cx, |v, _| v.trackpad.map(|p| p.at().0));
        assert!(x.is_some_and(|x| (x - (0.525 + 120.0 / 400.0)).abs() < 1e-4), "{x:?}");
        drop(inputs(&mut rx));

        // Two fingers dragged: a scroll at the pointer, begun, changed and ended.
        let pinch = |cx: &mut gpui::VisualTestContext, at, phase| {
            cx.simulate_event(PinchEvent {
                position: at,
                delta: 0.0,
                modifiers: Modifiers::default(),
                phase,
            });
        };
        pinch(cx, a, TouchPhase::Started);
        pinch(cx, point(a.x, a.y + px(12.0)), TouchPhase::Moved);
        pinch(cx, point(a.x, a.y + px(20.0)), TouchPhase::Moved);
        pinch(cx, point(a.x, a.y + px(20.0)), TouchPhase::Ended);
        cx.run_until_parked();
        let scrolls: Vec<_> = inputs(&mut rx)
            .into_iter()
            .filter_map(|i| match i {
                ScreenInput::Scroll { dy, phase, x, .. } => Some((dy, phase, x)),
                _ => None,
            })
            .collect();
        assert_eq!(
            scrolls.iter().map(|s| (s.0, s.1)).collect::<Vec<_>>(),
            [(12.0, ScrollPhase::Began), (8.0, ScrollPhase::Changed), (0.0, ScrollPhase::Ended)]
        );
        assert!(scrolls.iter().all(|s| (s.2 / 800.0 - 0.825).abs() < 1e-3), "at the pointer");
        assert!(view.read_with(cx, |v, _| v.zoom.is_fit()), "a drag does not zoom");

        // Zoomed into the top-left quarter, the pointer pushed right past the view pans it.
        view.update(cx, |v, cx| {
            if let Some(pad) = v.trackpad.as_mut() {
                pad.place((0.4, 0.2));
            }
            v.zoom = Zoom::FIT.about((0.0, 0.0), 2.0, 8.0);
            cx.notify();
        });
        cx.executor().advance_clock(Duration::from_secs(1));
        drag(cx, a, &[point(a.x + px(100.0), a.y)], Duration::from_millis(1000));
        let (zoom, at) = view.read_with(cx, |v, _| (v.zoom, v.trackpad.map(|p| p.at())));
        let at = at.unwrap_or_default();
        let (u, _) = zoom.to_body(at);
        let margin = view.read_with(cx, |v, _| v.theme.spacing.xl / 400.0);
        assert!((u - (1.0 - margin)).abs() < 1e-4, "kept at the margin: {u} with {zoom:?}");
        assert!(zoom.origin().0 < 0.0, "panned: {zoom:?}");

        // A drag with the button held (tap, then drag) is let go when the mode goes off.
        drag(cx, a, &[], Duration::ZERO);
        let ev = |phase, position| TouchDragEvent { phase, start_position: a, position };
        cx.simulate_event(ev(TouchPhase::Started, a));
        cx.simulate_event(ev(TouchPhase::Moved, point(a.x + px(30.0), a.y)));
        cx.run_until_parked();
        drop(inputs(&mut rx));
        view.update(cx, |v, cx| v.set_trackpad(false, cx));
        let got = presses(&inputs(&mut rx));
        assert!(matches!(got.as_slice(), [(ProtoButton::Left, false, ..)]), "{got:?}");
        assert!(view.read_with(cx, |v, _| v.buttons.is_empty()));
    }

    /// A zoom puts its readout up, which fades after a moment and goes; under Reduce Motion it
    /// goes at once, with no fade. A zoom also asks the worker for the scale it is drawn at.
    #[gpui::test]
    fn the_zoom_readout_fades_and_the_stream_follows_the_zoom(cx: &mut gpui::TestAppContext) {
        let (view, mut rx, cx) = windowed(cx);
        view.update(cx, |v, cx| {
            v.native = (3200.0, 2400.0);
            v.quality_changed = past_cooldown();
            v.set_painted_width(800.0, cx);
        });
        let scale = |rx: &mut mpsc::Receiver<ClientMsg>| {
            sent(rx).into_iter().find_map(|r| match r {
                ScreenRequest::SetQuality { quality, .. } => Some(quality.scale),
                _ => None,
            })
        };
        assert_eq!(scale(&mut rx), Some(0.25), "800 of 3200");
        view.update(cx, |v, cx| {
            v.quality_changed = past_cooldown();
            v.set_zoom(Zoom::FIT.about((0.5, 0.5), 2.0, 8.0), cx);
        });
        assert_eq!(scale(&mut rx), Some(0.5), "drawn twice as wide");
        let state = |cx: &mut gpui::VisualTestContext| view.read_with(cx, |v, _| v.readout);
        assert_eq!(state(cx), Some(Readout::Shown));
        assert!(
            view.read_with(cx, |v, _| v.readout())
                .is_some_and(|r| r.ends_with('%') && !r.ends_with(" %"))
        );
        cx.executor().advance_clock(READOUT_HOLD);
        cx.run_until_parked();
        assert!(matches!(state(cx), Some(Readout::Fading(_))), "{:?}", state(cx));
        cx.executor().advance_clock(kit::FADE);
        cx.run_until_parked();
        assert_eq!(state(cx), None);

        cx.update(|_w, cx| cx.set_reduce_motion(true));
        view.update(cx, |v, cx| v.set_zoom(Zoom::FIT, cx));
        assert_eq!(state(cx), Some(Readout::Shown));
        assert_eq!(view.read_with(cx, |v, _| v.readout()).as_deref(), Some("Fit"));
        cx.executor().advance_clock(READOUT_HOLD);
        cx.run_until_parked();
        assert_eq!(state(cx), None, "gone at once, no fade");
    }

    /// Frame cost of the picture at fit, zoomed and still, and zoomed with a pinch moving it
    /// every frame (its readout up), on a 5K-sized frame: the draw the window does for the view
    /// alone. Blocks of each alternate so a loaded machine weighs on all three alike.
    #[gpui::test]
    #[ignore = "a measurement: prints MEASURE lines"]
    fn measure_a_zoomed_stream_frame(cx: &mut gpui::TestAppContext) {
        const BLOCK: usize = 100;
        const BLOCKS: usize = 6;
        const WARM: usize = 50;
        let (view, _rx, cx) = windowed(cx);
        cx.simulate_resize(size(px(1170.0), px(2532.0)));
        let buffer = CVPixelBuffer::new(
            core_video::pixel_buffer::kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            5120,
            2880,
            None,
        )
        .expect("pixel buffer");
        view.update(cx, |v, _| {
            v.latest = Some(buffer);
            v.native = (5120.0, 2880.0);
            v.cursor = CursorState { x: 400, y: 300, visible: true };
        });
        cx.run_until_parked();
        let deep = Zoom::FIT.about((0.4, 0.6), 4.0, 8.0);
        let mut took: [Vec<Duration>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for block in 0..(BLOCKS * 3) {
            let case = block % 3;
            view.update(cx, |v, _| v.zoom = if case == 0 { Zoom::FIT } else { deep });
            for n in 0..(WARM + BLOCK) {
                let start = Instant::now();
                view.update(cx, |v, cx| {
                    if case == 2 {
                        let factor = if n % 2 == 0 { 1.01 } else { 1.0 / 1.01 };
                        v.set_zoom(v.zoom.about((0.5, 0.5), factor, 8.0), cx);
                    }
                    cx.notify();
                });
                cx.run_until_parked();
                if n >= WARM {
                    took[case].push(start.elapsed());
                }
            }
        }
        for (name, mut t) in ["fit", "zoomed", "pinching"].into_iter().zip(took) {
            t.sort_unstable();
            let at = |q: f64| {
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    clippy::cast_precision_loss,
                    reason = "an index"
                )]
                let i = ((t.len() as f64 * q).ceil() as usize).saturating_sub(1);
                t[i].as_secs_f64() * 1e6
            };
            println!(
                "MEASURE {name}: p50 {:.1} µs p95 {:.1} µs max {:.1} µs (n {})",
                at(0.5),
                at(0.95),
                at(1.0),
                t.len()
            );
        }
    }

    /// The overlay says 4:4:4 once the decoder hands back a full-chroma picture (`xf44`, or
    /// `444f` at 8 bits), 4:2:0 for NV12, and nothing before the first picture.
    #[gpui::test]
    fn the_overlay_says_4_4_4_from_the_decoded_picture(cx: &mut gpui::TestAppContext) {
        use core_video::pixel_buffer::{
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            kCVPixelFormatType_444YpCbCr8BiPlanarFullRange,
            kCVPixelFormatType_444YpCbCr10BiPlanarFullRange,
        };
        assert_eq!(chroma_of(kCVPixelFormatType_444YpCbCr8BiPlanarFullRange), Chroma::Full);
        let (view, _rx) = view(cx);
        let chroma = |cx: &mut gpui::TestAppContext| view.read_with(cx, |v, _| v.chroma);
        assert_eq!(chroma(cx), None, "no picture yet");
        let full =
            CVPixelBuffer::new(kCVPixelFormatType_444YpCbCr10BiPlanarFullRange, 64, 48, None)
                .expect("pixel buffer");
        view.update(cx, |v, cx| v.show_picture(full, cx));
        assert_eq!(chroma(cx).map(chroma_label), Some("4:4:4"));
        let nv12 = CVPixelBuffer::new(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange, 64, 48, None)
            .expect("pixel buffer");
        view.update(cx, |v, cx| v.show_picture(nv12, cx));
        assert_eq!(chroma(cx).map(chroma_label), Some("4:2:0"));
    }

    /// A picture of `w` × `h` for a test to put up.
    fn picture(w: usize, h: usize) -> CVPixelBuffer {
        CVPixelBuffer::new(
            core_video::pixel_buffer::kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            w,
            h,
            None,
        )
        .expect("pixel buffer")
    }

    /// Senders for the frames and cursor samples the view's pump reads, as the stream's own.
    fn pumped(
        view: &gpui::Entity<ScreenView>,
        cx: &mut gpui::VisualTestContext,
    ) -> (watch::Sender<Option<CVPixelBuffer>>, watch::Sender<CursorState>) {
        fn take(
            view: &mut ScreenView,
            frame: Option<CVPixelBuffer>,
            cx: &mut Context<ScreenView>,
        ) -> bool {
            let Some(buffer) = frame else { return false };
            view.show(buffer, cx);
            true
        }
        let (frames_tx, frames) = watch::channel(None);
        let (cursor_tx, cursor) = watch::channel(CursorState::default());
        #[expect(clippy::used_underscore_binding, reason = "the pump is kept, not read, but here")]
        view.update(cx, |v, cx| v._pump = ScreenView::pump(frames, cursor, take, cx));
        (frames_tx, cursor_tx)
    }

    /// Where the pointer is drawn, in window coordinates, when it is.
    fn drawn_at(cx: &mut gpui::VisualTestContext) -> Option<Bounds<Pixels>> {
        cx.debug_bounds("screen-pointer")
    }

    fn renders(view: &gpui::Entity<ScreenView>, cx: &gpui::VisualTestContext) -> u32 {
        view.read_with(cx, |v, _| v.renders())
    }

    /// Where the drawn arrow's extent starts with its tip on `at` (layout rounds its size).
    fn arrow_at(at: Point<Pixels>) -> Point<Pixels> {
        let extent = ARROW.with(|arrow| arrow.as_ref().map(|a| a.bounds())).expect("the arrow");
        at + extent.origin
    }

    /// Where the drawn pointer's extent starts, when one is drawn.
    fn arrow_drawn(cx: &mut gpui::VisualTestContext) -> Option<Point<Pixels>> {
        drawn_at(cx).map(|b| b.origin)
    }

    /// On a window stream the pointer is drawn where this client put it, in the worker's cursor
    /// picture, by the frame the move itself draws: no cursor sample comes into it. A sample
    /// that does come (the echo of an older move) neither moves it nor draws. Before the first
    /// move there is none, as the worker says.
    #[gpui::test]
    fn a_window_streams_pointer_is_drawn_where_this_client_put_it_in_the_same_frame(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, mut rx, cx) = windowed_on(cx, CaptureTarget::Window(slopty_core::WindowId(9)));
        let (_frames, cursor) = pumped(&view, cx);
        let shape = CursorShape { w: 8, h: 8, hot_x: 2, hot_y: 2, bgra: vec![0; 256], scale: 2 };
        view.update(cx, |v, cx| {
            v.show_picture(picture(800, 600), cx);
            v.set_cursor_shape(Some(shape), cx);
        });
        cx.run_until_parked();
        assert_eq!(drawn_at(cx), None, "put nowhere yet: none");
        let pictured = |at| pointer_bounds(at, size(px(4.0), px(4.0)), point(px(1.0), px(1.0)));

        let first = at_fraction(&view, cx, 0.25, 0.5);
        cx.simulate_mouse_move(first, None, Modifiers::default());
        cx.run_until_parked();
        assert_eq!(drawn_at(cx), Some(pictured(first)));

        let second = at_fraction(&view, cx, 0.75, 0.25);
        let before = renders(&view, cx);
        cx.simulate_mouse_move(second, None, Modifiers::default());
        cx.run_until_parked();
        assert_eq!(renders(&view, cx), before.saturating_add(1), "one frame, the move's own");
        assert_eq!(drawn_at(cx), Some(pictured(second)), "the worker's picture, at the new point");
        let sample = view.read_with(cx, |v, _| v.cursor);
        assert_eq!(sample, CursorState::default(), "with no sample come");
        let moves = inputs(&mut rx);
        assert!(
            matches!(moves.as_slice(), [ScreenInput::Move { .. }, ScreenInput::Move { x, y }] if (*x - 600.0).abs() < 0.5 && (*y - 150.0).abs() < 0.5),
            "both moves went: {moves:?}"
        );

        cursor.send(CursorState { x: 200, y: 300, visible: true }).expect("the pump listens");
        cx.run_until_parked();
        assert_eq!(drawn_at(cx), Some(pictured(second)), "the late echo moves nothing");
        assert_eq!(renders(&view, cx), before.saturating_add(1), "and draws nothing");
    }

    /// On a display the worker's sample draws the pointer while this client is not moving it
    /// (another user's hand, an app's warp), this client's own point draws it while it is, and
    /// once the hold runs out the worker's sample takes over, drawn where the worker says the
    /// pointer went.
    #[gpui::test]
    fn a_displays_pointer_follows_the_worker_unless_this_client_moves_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, _rx, cx) = windowed(cx);
        let (_frames, cursor) = pumped(&view, cx);
        view.update(cx, |v, cx| v.show_picture(picture(800, 600), cx));
        cx.run_until_parked();
        assert_eq!(arrow_drawn(cx), None, "the worker's pointer is off the display");

        cursor.send(CursorState { x: 400, y: 300, visible: true }).expect("the pump listens");
        cx.run_until_parked();
        assert_eq!(
            arrow_drawn(cx),
            Some(arrow_at(at_fraction(&view, cx, 0.5, 0.5))),
            "the worker's"
        );

        let here = at_fraction(&view, cx, 0.25, 0.25);
        cx.simulate_mouse_move(here, None, Modifiers::default());
        cx.run_until_parked();
        assert_eq!(arrow_drawn(cx), Some(arrow_at(here)), "this client's, at once");

        let drawn = renders(&view, cx);
        cursor.send(CursorState { x: 600, y: 450, visible: true }).expect("the pump listens");
        cx.run_until_parked();
        assert_eq!(arrow_drawn(cx), Some(arrow_at(here)), "held while this client drives it");
        assert_eq!(renders(&view, cx), drawn, "a sample under the hold draws nothing");

        cx.executor().advance_clock(LOCAL_HOLD);
        cx.run_until_parked();
        let there = at_fraction(&view, cx, 0.75, 0.75);
        assert_eq!(
            arrow_drawn(cx),
            Some(arrow_at(there)),
            "the hold ran out: where the worker's went"
        );

        cursor.send(CursorState { x: 80, y: 60, visible: true }).expect("the pump listens");
        cx.run_until_parked();
        let warped = at_fraction(&view, cx, 0.1, 0.1);
        assert_eq!(arrow_drawn(cx), Some(arrow_at(warped)), "moved on the worker alone");
    }

    /// Frames at 60 Hz with cursor samples at 120 Hz, the worker moving the pointer: each sample
    /// rides the frame after it, so the view draws once a frame, not once a frame and once a
    /// sample. A sample whose frame is late draws on its own, [`FOLD`] past the frame's due
    /// time; with frames stopped, a sample draws at once.
    #[gpui::test]
    fn cursor_samples_ride_the_frames_while_they_flow(cx: &mut gpui::TestAppContext) {
        const FRAMES: u64 = 60;
        const SAMPLES: u64 = 120;
        let (view, _rx, cx) = windowed(cx);
        let (frames, cursor) = pumped(&view, cx);
        let buffer = picture(800, 600);
        let before = renders(&view, cx);
        let mut events: Vec<(u64, bool)> = (0..FRAMES)
            .map(|k| (k.saturating_mul(16_667), true))
            .chain((0..SAMPLES).map(|k| (k.saturating_mul(8_333).saturating_add(4_000), false)))
            .collect();
        events.sort_unstable();
        let (mut clock, mut x) = (0_u64, 0_i32);
        for (at, frame) in events {
            cx.executor().advance_clock(Duration::from_micros(at.saturating_sub(clock)));
            clock = at;
            if frame {
                frames.send(Some(buffer.clone())).expect("the pump listens");
            } else {
                x = x.saturating_add(3);
                cursor.send(CursorState { x, y: 300, visible: true }).expect("the pump listens");
            }
            cx.run_until_parked();
        }
        // The last two samples came after the last frame; it has no next, so they draw on their
        // own once it is late.
        cx.executor().advance_clock(Duration::from_millis(30));
        cx.run_until_parked();
        let draws = renders(&view, cx).saturating_sub(before);
        println!("MEASURE {FRAMES} frames at 60 Hz and {SAMPLES} samples at 120 Hz: {draws} draws");
        assert_eq!(draws, u32::try_from(FRAMES).expect("small").saturating_add(1));
        #[expect(clippy::cast_precision_loss, reason = "a small coordinate")]
        let last = x as f32 / 800.0;
        near_px(offset(&view, cx), (last * 400.0, 150.0));

        // A frame, then a sample, and the next frame late.
        frames.send(Some(buffer)).expect("the pump listens");
        cx.run_until_parked();
        let drawn = renders(&view, cx);
        cx.executor().advance_clock(Duration::from_millis(4));
        cursor.send(CursorState { x: 100, y: 100, visible: true }).expect("the pump listens");
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(20));
        cx.run_until_parked();
        assert_eq!(renders(&view, cx), drawn, "waiting for the frame, 24 ms on");
        cx.executor().advance_clock(Duration::from_millis(2));
        cx.run_until_parked();
        assert_eq!(
            renders(&view, cx),
            drawn.saturating_add(1),
            "late by more than the fold: drawn alone"
        );

        // Frames stopped: a sample draws at once.
        cx.executor().advance_clock(Duration::from_millis(50));
        cursor.send(CursorState { x: 200, y: 100, visible: true }).expect("the pump listens");
        cx.run_until_parked();
        assert_eq!(renders(&view, cx), drawn.saturating_add(2), "no frames: at once");

        // A sample that moves nothing drawn draws nothing: the same place, or hidden and hidden.
        cursor.send(CursorState { x: 200, y: 100, visible: true }).expect("the pump listens");
        cx.run_until_parked();
        cursor.send(CursorState { x: 0, y: 0, visible: false }).expect("the pump listens");
        cx.run_until_parked();
        cursor.send(CursorState { x: 5, y: 5, visible: false }).expect("the pump listens");
        cx.run_until_parked();
        assert_eq!(renders(&view, cx), drawn.saturating_add(3), "only the hiding drew");
    }
}
