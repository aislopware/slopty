//! `ScreenView`: one remote window or display, painted from the newest decoded frame.
//!
//! The picture never goes through a GPUI frame. The decoder hands each IOSurface-backed
//! `CVPixelBuffer` to the stream's `VideoLayer` on its own thread (`glass`), and the layer draws
//! it on the next refresh; a `slopty_client::Pacer` there drops a picture older than the one up
//! and times each from its arrival to the window server's report of it. The view places the
//! layer where the picture goes, clipped by the tile, in its own frame, and draws over it what is
//! GPUI's: the pointer, the zoom readout and the stats overlay. It draws again only when those
//! change or the picture's size does. The pointer is drawn here in the worker's cursor picture:
//! at this client's own pointer while this client drives it (always on a window stream), else
//! where the cursor channel says the worker's is. Pointer, scroll and key events inside the view go
//! to the worker as `ScreenInput` in stream pixels; the worker injects them. ⌘ chords the workspace
//! binds (⌘T/⌘O/⌘W, the text size) never reach the view because GPUI runs key bindings before key
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

#[cfg(test)]
use core_video::pixel_buffer::CVPixelBuffer;
use gpui::composition::{NativeHost, NativeHostOptions};
use gpui::{
    Animation, AnimationExt as _, App, Autocapitalize, Bounds, ContentMask, Context, CursorImage,
    CursorImageId, CursorStyle, DevicePixels, ElementInputHandler, EntityInputHandler,
    EventEmitter, FocusHandle, Focusable, Global, InteractiveElement as _, IntoElement, Keystroke,
    LongPressEvent, Modifiers, MouseButton, MouseDownEvent, MouseExitEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, Path, PathBuilder, PinchEvent, Pixels, Point, Render,
    RenderImage, ScrollDelta, ScrollWheelEvent, SharedString, Size,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TextInputAction,
    TextInputConfiguration, TouchDragEvent, TouchPhase, UTF16Selection, Window, canvas, div, point,
    px, size,
};
use slopty_client::pacing::{PacingStats, PaintRate, Spread};
use slopty_client::{CursorState, ScreenHandle, ScreenStats};
use slopty_core::StreamId;
use slopty_proto::ClientMsg;
use slopty_proto::drag::DragInput;
use slopty_proto::input::{KeyAction, KeyCode, Mods, MouseButton as ProtoButton};
use slopty_proto::screen::{
    CaptureTarget, Chroma, CursorShape, Quality, RateVerdict, Region, ScreenInput, ScreenRequest,
    ScrollPhase, SourceState, TextField, VideoCodec,
};
use slopty_theme::Theme;
use tokio::sync::{Notify, mpsc, watch};

use crate::colors::hsla;
use crate::{keys, kit};

mod driver;
mod drop;
mod glass;
mod health;
mod keyboard;
mod touch;
mod zoom;

pub use driver::{Driver, HAND_BACK, IN_CONTROL, TAKE_CONTROL};
pub use health::{Figure, Health, RTT_WARN_FROM, fps_label};
pub use zoom::Zoom;

#[expect(clippy::derive_partial_eq_without_eq, reason = "gpui::actions! derives PartialEq only")]
mod actions {
    gpui::actions!(
        screen,
        [
            /// Turn the focused remote picture's fingers into a trackpad, or back.
            ToggleTrackpad,
            /// Send the focused remote picture's trackpad gestures to the app under the pointer
            /// on the worker, or keep them for zooming the picture here.
            ToggleRemoteGestures,
        ]
    );
}
pub use actions::{ToggleRemoteGestures, ToggleTrackpad};

/// What the palette's command calls sending a picture's gestures on: one name, on or off.
pub const REMOTE_GESTURES: &str = "Gestures to the remote app";

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

/// Quality change rate limit, and how long a zoom holds still before the stream follows it.
const QUALITY_COOLDOWN: Duration = Duration::from_millis(400);
/// How fast this Mac's pointer pushing at an edge with all its weight pans a zoomed picture, in
/// frames a second (`docs/decisions/input.md`, "A zoomed picture pans when the pointer pushes
/// at the edge").
const EDGE_PAN_FRAMES_PER_S: f32 = 1.5;
/// How often the push moves the picture: a 120 Hz refresh.
const EDGE_TICK: Duration = Duration::from_micros(8_333);
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
    /// A drag out of the worker's app left the tile with the button held: the workspace drags
    /// what it carries on from the mouse event being handled (`drop`).
    DragOut(Arc<slopty_client::dnd::out::Shared>),
    /// A drag out of the worker's app could not be caught there: why, for a person.
    DragOutFailed(String),
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

/// How long a fling may go quiet before the worker is told its momentum ended. Both platforms say
/// so themselves (`ScrollWheelEvent::momentum_phase`), so this is the backstop for a lost or
/// dropped close. Momentum events arrive about a frame apart, which makes this several frames of
/// silence.
const MOMENTUM_GAP: Duration = Duration::from_millis(120);

/// Where a scroll gesture over the picture has got to: what the worker has been told is open,
/// so a second `Started` is not a second gesture and a fling whose close was lost is closed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Scrolling {
    /// Nothing open. A mouse wheel notch never leaves this state.
    #[default]
    Idle,
    /// Between `Started` and `Ended`: the fingers are down.
    Fingers,
    /// The fingers have lifted and the momentum they left is still coasting.
    Momentum,
}

/// One stream on screen.
pub struct ScreenView {
    stream: StreamId,
    target: CaptureTarget,
    handle: ScreenHandle,
    /// The pictures' way to the glass, and what the view knows of them.
    glass: Arc<glass::Glass>,
    /// The newest picture's size and colour; `None` before the first.
    shape: Option<glass::Shape>,
    /// Where the picture's layer is placed from, in the window the view last drew in.
    host: Option<Host>,
    /// Where a striped picture's lower stripe's layer is placed from, in that window.
    lower_host: Option<Host>,
    /// Where the stripes meet in the layout the last paint placed the layers for, as the glass
    /// was told it ([`glass::Glass::place`]); `None` before the first.
    laid_out: Rc<std::cell::Cell<Option<glass::Placed>>>,
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
    /// This view's name for the worker's cursor picture as the system pointer
    /// (`CursorStyle::Image`), and whether the platform has a picture under it.
    system_pointer: (CursorImageId, bool),
    /// The system pointer the last render asked for over the picture.
    styled: CursorStyle,
    /// When this client last put the worker's pointer somewhere (a move, a press or a release
    /// over the picture), on the executor's clock.
    placed: Option<Instant>,
    /// Where the last render drew the pointer, in fractions of the picture; `None` when it drew
    /// none.
    drawn: Option<(f32, f32)>,
    /// The picture's accessible label.
    label: SharedString,
    /// Renders so far.
    renders: u32,
    /// The body's bounds as the last render read them (tests).
    #[cfg(test)]
    rendered_at: Bounds<Pixels>,
    /// Pictures a test put up.
    #[cfg(test)]
    test_pictures: u64,
    /// Where the last paint placed the layer, and the mask that clipped it (tests: the test
    /// platform presents no frames, so its hosts record no placement).
    #[cfg(test)]
    layer_at: LayerAt,
    /// The same for a striped picture's lower stripe's layer (tests).
    #[cfg(test)]
    lower_at: LayerAt,
    /// Where the last paint drew the whole target's picture under a region's layer (tests).
    #[cfg(test)]
    base_at: Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
    /// The stream size the worker maps input with: the size last asked for, or last told by
    /// `Geometry`. The worker takes a new scale in order with the input behind it, so frames
    /// still in flight at the old scale must not move it (unlike `size`, the picture's).
    mapped: (u32, u32),
    out: Outbox,
    theme: Theme,
    focus: FocusHandle,
    bounds: Bounds<Pixels>,
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
    /// Keys by position, composed text, Caps Lock and the input source (`keyboard`).
    keyboard: keyboard::Keyboard,
    /// The modifier keys as last reported, so a change forwards the key that moved.
    modifiers: Modifiers,
    /// Focus leaving the view and the window going inactive each release what is held on the
    /// worker, and the window moving to another screen asks for that screen's refresh. They are
    /// registered with the window the view renders in (the constructor has none), and again
    /// when it renders in another: a tile popped out into a window of its own.
    let_go: Option<(gpui::AnyWindowHandle, [Subscription; 4])>,
    /// What the worker was last told of this tile's focus ([`ScreenRequest::Focused`]); a
    /// stream starts unfocused there.
    focus_told: bool,
    /// The screen the view's window is on, and its refresh in hertz (0 when it does not say);
    /// `None` until the view renders.
    screen: Option<(Option<u32>, u16)>,
    /// Input-method composition in progress (nothing is sent until it commits).
    marked: Option<String>,
    /// The worker's text field that has the keyboard
    /// ([`slopty_proto::screen::ScreenEvent::Field`]): where a composition's candidate window
    /// hangs, and whether it is a password field.
    field: Option<TextField>,
    /// The stats overlay (⌘⇧I).
    hud: Option<Hud>,
    /// The overlay shows its engineering lines under the plain one.
    hud_details: bool,
    /// Reads the counters once a second for the header's health mark.
    probe: health::Probe,
    /// What is wrong with the stream, as last read.
    health: Option<Health>,
    _health: Task<()>,
    /// The link's round trip, from the workspace: how long this client's hold on a display's
    /// pointer outlasts [`LOCAL_HOLD`].
    rtt: Option<Duration>,
    /// The round trip the overlay prints: the link's, unless the workspace pins what readouts
    /// show (the e2e harness does, so a golden never carries the machine's load).
    shown_rtt: Option<Duration>,
    /// Holds the device out of idle sleep (the Mac) or its screen on (the phone) for as long as
    /// this window streams; dropping the view lets go.
    _awake: Task<()>,
    /// A detached global observer outlives the view until the global next changes.
    _capture: Subscription,
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
    /// "Unlock here" was pressed on a locked Mac's notice: the scrim is lifted and the keys go
    /// to the lock screen, until the Mac says otherwise.
    unlocking: bool,
    /// The agent that drives the screen, where the workspace opened it from a thread
    /// (`driver`).
    driver: Option<Driver>,
    /// The person took control from the agent that drives the screen.
    control: bool,
    /// Where the scroll gesture over the picture has got to.
    scrolling: Scrolling,
    /// Fires [`MOMENTUM_GAP`] after the last momentum event to close the fling. Replaced (so
    /// cancelled) by every event that keeps it alive.
    momentum_end: Option<Task<()>>,
    /// How the picture is drawn over the body: kept while the view lives, so per tile.
    zoom: Zoom,
    /// Asks for the scale and region the zoom settled at, once it has ([`Self::settle_zoom`]).
    settle: Option<Task<()>>,
    /// Where this Mac's pointer is over the body while a zoomed picture is drawn there, for
    /// the push at the edges ([`zoom::edge_push`]); `None` off the body.
    edge_at: Option<Point<Pixels>>,
    /// The pointer's push at the edges is panning the zoomed picture ([`Self::push_at_edges`]).
    edge_panning: bool,
    /// The trackpad scroll under way pans the zoomed picture here: it began with ⌥ held.
    pan_scroll: bool,
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
    /// What the time a gesture's report was read is counted from ([`Self::time_us`]).
    epoch: Instant,
    /// A trackpad's pinch goes to the app under the pointer on the worker rather than zooming
    /// the picture here ([`ToggleRemoteGestures`]); a pinch it sent is still going on.
    remote_gestures: bool,
    remote_pinch: bool,
    /// The zoom readout while it shows, the generation of the last change, and the timer that
    /// takes it down.
    readout: Option<Readout>,
    readout_gen: u64,
    readout_timer: Option<Task<()>>,
    /// A drag from this device over the tile (`drop`).
    drop: Option<drop::Dropping>,
    /// A drag out of the worker's app under this tile's press (`drop`).
    taking: Option<drop::Taking>,
    _pump: Task<()>,
}

/// Rates for the stats overlay, re-sampled about once a second.
#[derive(Clone, Debug)]
struct Hud {
    sampled_at: Instant,
    sample: ScreenStats,
    summary: Vec<Figure>,
    text: SharedString,
    history: History,
}

/// How many samples the overlay's trends hold: half a minute, one a [`HUD_PERIOD`].
const HUD_HISTORY: usize = 30;

/// The overlay's last half minute of the figures that move under a stalling link: how old the
/// frame on screen is, the interarrival jitter and the round trip, in milliseconds, oldest
/// first. A figure not known yet is a gap.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct History {
    age: VecDeque<f32>,
    jitter: VecDeque<f32>,
    rtt: VecDeque<f32>,
    /// Every sample taken, which keys the trends' slide.
    pushed: u64,
}

impl History {
    fn push(&mut self, age: Option<Duration>, jitter: Duration, rtt: Option<Duration>) {
        #[expect(clippy::cast_possible_truncation, reason = "milliseconds on screen")]
        let ms = |d: Option<Duration>| d.map_or(f32::NAN, |d| (d.as_secs_f64() * 1e3) as f32);
        for (ring, value) in [
            (&mut self.age, ms(age)),
            (&mut self.jitter, ms(Some(jitter))),
            (&mut self.rtt, ms(rtt)),
        ] {
            if ring.len() == HUD_HISTORY {
                ring.pop_front();
            }
            ring.push_back(value);
        }
        self.pushed = self.pushed.saturating_add(1);
    }
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
    /// A striped stream's captures shown with both stripes in one refresh, and apart; `None`
    /// for one picture.
    pub seams: Option<(u64, u64)>,
    /// The rate the stream is asked for, frames a second: its display period.
    pub target_fps: u16,
    /// The stream's frames a second, and the frames that missed the display beside them
    /// ([`ScreenView::paint_rate`]).
    pub paint: PaintRate,
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
    /// Capture on the worker → the glass, from the element's pacer; empty until the stream's
    /// clock probes have placed the worker's clock ([`ScreenStats::clock`]).
    pub capture: &'a Spread,
    /// The UI's own frame times, when the app installed a probe.
    pub ui: Option<&'a crate::frames::FrameStats>,
}

/// The five lines of the stats overlay: what is on screen, how it got there, what the sound
/// did, when the picture was shown, and how the UI itself keeps up.
///
/// Line one is the picture: its size and capture scale, how much colour it carries (4:4:4 when
/// the decoder hands back full-chroma pictures, `xf44`, else 4:2:0), the age of the frame
/// being shown and, for a stream coded as two stripes, how many captures reached the glass with
/// both stripes in one refresh and how many apart; its rate, throughput and round trip are the
/// plain line's
/// (`health::summary`). Line two is the path: jitter (RFC 3550 interarrival), how long frames
/// waited for their last fragment (p50 / p95 of the last report), the in-order queue, recovery
/// counts, stalls and the worker's bitrate verdict. Line three is the audio: packets played, lost
/// and concealed, the times playback ran dry, how much the jitter buffer trimmed and stretched to
/// hold its depth, and the depth it aims for. Line four is the presentation: how long a
/// frame takes from its capture on the worker to the glass (p50 / p95 / worst, and how far the
/// estimate of the worker's clock may be off: `docs/decisions/video.md`, "Capture to glass on
/// any link"), then from the arrival of the datagram that completed it to the glass (p50 / p95 /
/// worst of the last `slopty_client::pacing::RING` frames, with the decoder's share of it), the
/// spacing of those paints and its jitter, and the two cadence faults —
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
    let capture = match (input.capture.count, stats.clock) {
        (1.., Some(clock)) => format!(
            "capture {:.1} / {:.1} / {:.1} ms ±{:.1}",
            ms(input.capture.p50),
            ms(input.capture.p95),
            ms(input.capture.max),
            ms(clock.bound)
        ),
        _ => "capture –".to_owned(),
    };
    let ui = crate::frames::hud_line(input.ui);
    let stripes = input.seams.map_or_else(String::new, |(together, apart)| {
        format!("  ·  stripes {together} together {apart} apart")
    });
    format!(
        "{}×{} @{:.2}  ·  {chroma}  ·  {age}{stripes}\n\
         jitter {:.1} ms  ·  hold {:.1} / {:.1} ms  ·  queue {}  ·  fec {} lost {} nack {} refresh {}  ·  stalls {} ({} ms) {stall}  ·  {rate}\n\
         audio {} lost {} concealed {}  ·  dry {}  ·  trimmed {:.0} ms stretched {:.0} ms  ·  target {:.0} ms\n\
         {capture}  ·  present {:.1} / {:.1} / {:.1} ms (decode {:.1})  ·  every {:.1} ms ±{:.1}  ·  shown {} skip {} repeat {} late {}\n\
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

/// A gesture's phase as the wire carries it: GPUI folds AppKit's may-begin into its start.
#[must_use]
pub const fn gesture_phase(phase: TouchPhase) -> ScrollPhase {
    match phase {
        TouchPhase::Started => ScrollPhase::Began,
        TouchPhase::Moved => ScrollPhase::Changed,
        TouchPhase::Ended => ScrollPhase::Ended,
        TouchPhase::Cancelled => ScrollPhase::Cancelled,
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
        SourceState::Locked | SourceState::Away => console_notice(source).0,
    }
}

/// What the tile says over its picture while the worker's Mac shows nothing of the session.
///
/// A title and a line under it, for a locked Mac or one whose screens another session has; an
/// empty title otherwise (`docs/decisions/video.md`, "The client is told when the Mac is
/// locked"). It says what is so and when it ends. A locked Mac also offers [`UNLOCK_HERE`]:
/// its lock screen takes the keys the stream sends, so the person types their own password.
/// The login window does not: the worker's session is not the one on the screen, and its keys
/// do not reach it.
#[must_use]
pub const fn console_notice(source: SourceState) -> (&'static str, &'static str) {
    match source {
        SourceState::Locked => {
            ("The Mac is locked", "The picture returns once it is unlocked, here or at the Mac.")
        }
        SourceState::Away => (
            "The Mac is at the login window",
            "Or another user has its screen. The picture returns when this account does.",
        ),
        SourceState::Idle | SourceState::Live => ("", ""),
    }
}

/// Why a remote window or display did not open, or why the machine could not list what it
/// shares, in words: what is so, then what to do where something can be done.
#[must_use]
pub fn failure_text(failure: &slopty_proto::screen::ScreenFailure, machine: &str) -> String {
    use slopty_proto::screen::ScreenFailure;
    match failure {
        ScreenFailure::NotPermitted => format!(
            "{machine} may not record its screen. Turn on Screen Recording for slopty-worker \
             in its System Settings."
        ),
        ScreenFailure::Gone => "It is not there any more.".to_owned(),
        ScreenFailure::Unsupported => format!("{machine} has no screen to share."),
        ScreenFailure::Failed(why) => {
            let why = why.trim_end_matches('.');
            let mut chars = why.chars();
            let why: String = chars
                .next()
                .map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect());
            format!("{why}.")
        }
    }
}

/// A locked Mac's notice's way to unlock it from here.
pub const UNLOCK_HERE: &str = "Unlock here";
/// What the slim notice says once [`UNLOCK_HERE`] is pressed.
pub const TYPE_THE_PASSWORD: &str = "Type the Mac's password, then Return";

/// How long a body waits on its worker before it says it is waiting: "Opening…",
/// "Reading…", "Attaching…".
///
/// Two frames at 60 Hz: the frame the answer lands in, plus a round trip of up to one frame
/// budget. The tailnet's median round trip is about 1 ms and the shaped tailnet profile's worst
/// is 12 ms (`docs/MEASUREMENTS.md`, "loading placeholders after a grace"), so a file read or an
/// attach that answers in time shows its content in place of a blank body, never a word that
/// flashes and goes.
pub const LOADING_GRACE: Duration = Duration::from_millis(32);

/// What a remote tile's header shows of its stream: its health and the trackpad's toggle.
///
/// Copied out of the view as it changes, so the header is drawn without reading the view,
/// which changes with every frame it shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamHeader {
    stream: u32,
    health: Option<Health>,
    /// A touch screen, where the trackpad's toggle shows.
    touch: bool,
    trackpad: bool,
}

/// Whether a waiting body's grace has run out, kept as its element's state.
struct Waiting(bool);

/// Whether the waiting body keyed by `key` has been drawn for [`LOADING_GRACE`] or longer.
///
/// The first draw starts a timer that marks the grace over and draws the view again, so the
/// words come in on their own. The mark is the timer's, not a reading of the clock while
/// drawing: a view drawn from the last frame and the same view built again agree on it. It is
/// dropped with the element: a body that got its content and later waits again starts a new
/// grace.
pub fn past_grace(key: impl Into<gpui::ElementId>, window: &mut Window, cx: &mut App) -> bool {
    let over = window.use_keyed_state(key, cx, |_window, cx| {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LOADING_GRACE).await;
            let _gone = this.update(cx, |over: &mut Waiting, cx| {
                over.0 = true;
                cx.notify();
            });
        })
        .detach();
        Waiting(false)
    });
    over.read(cx).0
}

/// `shown` once the grace keyed by `key` has passed ([`past_grace`]), nothing before. Its clock
/// starts as it is laid out, so a view built from another view's state, which cannot start
/// one while it reads, can hold a grace too.
#[derive(IntoElement)]
pub(crate) struct AfterGrace {
    key: gpui::ElementId,
    shown: gpui::AnyElement,
}

impl AfterGrace {
    pub(crate) fn new(key: impl Into<gpui::ElementId>, shown: impl IntoElement) -> Self {
        Self { key: key.into(), shown: shown.into_any_element() }
    }
}

impl gpui::RenderOnce for AfterGrace {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        if past_grace(self.key, window, cx) { self.shown } else { gpui::Empty.into_any_element() }
    }
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
            .field("frames", &self.frames())
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

/// The most frames a second a stream asks for: no screen here refreshes faster.
const MAX_FPS: u16 = 120;

/// The frames a second a stream asks for.
///
/// That is the refresh of the screen its view is on, up to `MAX_FPS`
/// (`docs/decisions/video.md`, "The stream follows the screen's refresh"). A screen that does
/// not say (`refresh_hz` 0) counts as 60 Hz.
#[must_use]
pub const fn stream_fps(refresh_hz: u16) -> u16 {
    let screen = if refresh_hz == 0 { UNKNOWN_REFRESH_HZ } else { refresh_hz };
    if screen < MAX_FPS { screen } else { MAX_FPS }
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

/// The quality a stream is asked for: the refresh of the screen the view is on
/// ([`stream_fps`]), and the settings' bitrate ceiling at `scale`.
#[must_use]
pub const fn quality_of(prefs: slopty_theme::StreamPrefs, scale: f32, refresh_hz: u16) -> Quality {
    Quality {
        fps: stream_fps(refresh_hz),
        bitrate_bps: prefs.max_bitrate_bps,
        scale,
        region: None,
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
        let wanted = Quality {
            region: self.quality.region,
            ..quality_of(theme.behaviour.stream, self.quality.scale, self.refresh_hz())
        };
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
    /// stream asks for that rate when it is not the one it has.
    fn on_screen(&mut self, screen: Option<u32>, hz: u16) {
        self.screen = Some((screen, hz));
        let fps = stream_fps(hz);
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
        cx: &mut Context<Self>,
    ) -> Self {
        let Opened { stream, target, size, quality } = opened;
        // A cursor picture's texture lives in every window's atlas until it is dropped from it.
        cx.on_release(|view, cx| {
            view.pointer.drop_image(cx);
            cx.set_cursor_image(view.system_pointer.0, None);
        })
        .detach();
        handle.mute_by_default(theme.behaviour.stream.muted);
        let glass = glass::Glass::new();
        let presenter = Arc::clone(&glass);
        handle.set_present(Some(Arc::new(move |frame| {
            presenter.offer(glass::Picture::of(frame), frame.stamp);
        })));
        let pump = Self::pump(glass.shapes(), handle.cursor(), cx);
        // A render that captures the window draws the picture itself ([`capture_pictures`]).
        let capture = cx.observe_global::<CapturePictures>(|_view, cx| cx.notify());
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
            glass,
            shape: None,
            host: None,
            lower_host: None,
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
            system_pointer: (next_pointer_id(), false),
            styled: CursorStyle::Arrow,
            placed: None,
            drawn: None,
            label: match target {
                CaptureTarget::Display(id) => format!("Remote display {}", id.0).into(),
                CaptureTarget::Window(id) => format!("Remote window {}", id.0).into(),
            },
            renders: 0,
            #[cfg(test)]
            rendered_at: Bounds::default(),
            #[cfg(test)]
            test_pictures: 0,
            #[cfg(test)]
            layer_at: Rc::default(),
            #[cfg(test)]
            lower_at: Rc::default(),
            #[cfg(test)]
            base_at: Rc::default(),
            laid_out: Rc::default(),
            mapped: size,
            out: Outbox::new(out, cx),
            theme,
            focus: cx.focus_handle(),
            bounds: Bounds::default(),
            held: Vec::new(),
            buttons: Vec::new(),
            pointer_at: (0.0, 0.0),
            modifiers: Modifiers::default(),
            let_go: None,
            focus_told: false,
            screen: None,
            paste_hook: None,
            paste_hold: (0, Vec::new()),
            sticky: Modifiers::default(),
            keyboard: keyboard::Keyboard::new(cx),
            marked: None,
            field: None,
            hud: None,
            hud_details: false,
            probe: health::Probe::default(),
            health: None,
            _health: Self::watch_health(cx),
            rtt: None,
            shown_rtt: None,
            _awake: awake,
            _capture: capture,
            #[cfg(target_os = "macos")]
            _display: (!cfg!(test))
                .then(|| slopty_platform::Activity::display_awake("Slopty remote window")),
            rate: None,
            source: SourceState::Live,
            unlocking: false,
            driver: None,
            control: false,
            scrolling: Scrolling::Idle,
            momentum_end: None,
            zoom: Zoom::FIT,
            settle: None,
            edge_at: None,
            edge_panning: false,
            pan_scroll: false,
            painted: 0.0,
            scale_factor: 1.0,
            two: None,
            two_scrolling: false,
            trackpad: None,
            touch: TOUCH,
            epoch: cx.background_executor().now(),
            remote_gestures: false,
            remote_pinch: false,
            readout: None,
            readout_gen: 0,
            readout_timer: None,
            drop: None,
            taking: None,
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

    /// Where the body was drawn, and where the last render laid the picture out for (tests).
    #[cfg(test)]
    pub(crate) const fn bounds_rendered(&self) -> (Bounds<Pixels>, Bounds<Pixels>) {
        (self.bounds, self.rendered_at)
    }

    /// Take what the stream hands over: a new shape of picture (the first, a new size) draws
    /// the view, and a cursor sample draws it only when the pointer drawn moves
    /// ([`Self::pointer_changed`]), at a time that function picks. The pictures themselves go
    /// to the layer without the view ([`glass`]).
    fn pump(
        mut shapes: watch::Receiver<Option<glass::Shape>>,
        mut cursor: watch::Receiver<CursorState>,
        cx: &Context<Self>,
    ) -> Task<()> {
        enum Step {
            Shape(Option<glass::Shape>),
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
                    changed = shapes.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        Step::Shape(*shapes.borrow_and_update())
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
                        Step::Shape(shape) => {
                            if let Some(shape) = shape {
                                view.shaped(shape, cx);
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

    /// Something the drawn pointer follows changed: a cursor sample, or a hold that ran out.
    /// The view draws again, at once, only when the pointer it would draw is not the one on
    /// screen: the picture is on a layer of its own, so no frame of GPUI's is coming that would
    /// carry the change. Returns when to look again.
    fn pointer_changed(&self, now: Instant, cx: &mut Context<Self>) -> Option<Instant> {
        let recheck = match self.target {
            CaptureTarget::Display(_) => self.hold_end().filter(|end| now < *end),
            CaptureTarget::Window(_) => None,
        };
        if self.pointer_drawn(now) != self.drawn || self.pointer_style(now) != self.styled {
            cx.notify();
        }
        recheck
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
        (shown && self.shape.is_some() && !self.hardware_pointer(now)).then_some(at)
    }

    /// Whether the system pointer is the worker's cursor rather than a pointer this view draws:
    /// on macOS, over a picture, outside trackpad mode, whenever the pointer is this client's
    /// own (always on a window stream, for the hold after a move on a display). The window
    /// server then moves it with the hand, with no frame of this app's; the view draws it only
    /// where the worker moves the pointer itself.
    fn hardware_pointer(&self, now: Instant) -> bool {
        cfg!(target_os = "macos")
            && self.trackpad.is_none()
            && self.shape.is_some()
            && match self.target {
                CaptureTarget::Window(_) => true,
                CaptureTarget::Display(_) => self.local_drives(now),
            }
    }

    /// The system pointer over the picture now ([`pointer_style`]).
    fn pointer_style(&self, now: Instant) -> CursorStyle {
        let (id, pictured) = self.system_pointer;
        pointer_style(self.shape.is_some(), self.hardware_pointer(now), pictured.then_some(id))
    }

    /// Draw again when the pointer this client just placed is not where the last render drew it.
    fn redraw_pointer(&self, now: Instant, cx: &mut Context<Self>) {
        if self.pointer_drawn(now) != self.drawn || self.pointer_style(now) != self.styled {
            cx.notify();
        }
    }

    /// The native host the picture's layer is placed from, in the window the view last drew in.
    #[must_use]
    pub fn layer_host(&self) -> Option<gpui::composition::NativeId> {
        self.host.as_ref().map(|host| host.native.id())
    }

    /// Pictures put up on the layer so far.
    #[must_use]
    pub fn frames(&self) -> u64 {
        self.glass.put_up()
    }

    /// Stream pixel size of the picture last painted.
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The stream's frames a second: pictures painted on this client over the last second,
    /// and apart from them the frames that missed the display. The one rate every readout says,
    /// the overlay's and the status bar's alike. A still window paints nothing, and says so.
    #[must_use]
    pub fn paint_rate(&self) -> PaintRate {
        self.glass.rate()
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
            history: History::default(),
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

    /// Read the counters; a change of health is the header's news. The header draws from the
    /// workspace's copy of the view's facts, which it takes again when the view notifies, so a
    /// change notifies: with the event alone the header kept its old word until something else
    /// notified the view, which a still stream may never do. The overlay, while it shows, is
    /// sampled here and drawn again: a still stream draws no frame that would.
    fn read_health(&mut self, cx: &mut Context<Self>) {
        let health = self.probe.read(&self.handle.stats(), &self.glass.pacing(), Instant::now());
        let changed = health != self.health;
        if changed {
            self.health = health;
            cx.emit(ScreenViewEvent::Health);
        }
        if self.hud.is_some() {
            self.sample_hud(cx);
        }
        if changed || self.hud.is_some() {
            cx.notify();
        }
    }

    /// What is wrong with the stream, as last read; `None` while all is well.
    #[must_use]
    pub const fn health(&self) -> Option<Health> {
        self.health
    }

    /// What its tile's header shows of the stream now ([`StreamHeader`]).
    #[must_use]
    pub const fn header(&self) -> StreamHeader {
        StreamHeader {
            stream: self.stream.0,
            health: self.health,
            touch: self.touch,
            trackpad: self.trackpad(),
        }
    }

    /// The header's health mark for `view`, whose header shows `header`: a dot and one word,
    /// only while something is wrong. A click opens the stats overlay.
    #[must_use]
    pub fn health_mark(
        view: &gpui::Entity<Self>,
        header: StreamHeader,
        theme: &Theme,
        k: f32,
    ) -> Option<gpui::AnyElement> {
        let health = header.health?;
        let s = theme.surfaces;
        let dot = if health == Health::Stalled { s.error_fill } else { s.warn_fill };
        let view = view.clone();
        let mark = div()
            .id(SharedString::from(format!("health-{}", header.stream)))
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
            .hover(move |el| el.bg(hsla(s.hover)))
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

    /// The link's round trip `rtt`, which times the pointer hold, and the one the overlay
    /// prints, `shown`: drawn again when the printed one moves while the overlay shows.
    pub fn set_rtt(
        &mut self,
        rtt: Option<Duration>,
        shown: Option<Duration>,
        cx: &mut Context<Self>,
    ) {
        self.rtt = rtt;
        if self.shown_rtt != shown {
            self.shown_rtt = shown;
            if self.hud.is_some() {
                cx.notify();
            }
        }
    }

    /// The worker's latest bitrate decision, shown in the overlay (drawn again with it while it
    /// shows) and read for health.
    pub fn set_rate(
        &mut self,
        target_bps: u32,
        verdict: RateVerdict,
        capped: bool,
        cx: &mut Context<Self>,
    ) {
        self.probe.verdict(verdict, Instant::now());
        let rate = Some((target_bps, verdict, capped));
        if self.rate != rate {
            self.rate = rate;
            if self.hud.is_some() {
                cx.notify();
            }
        }
    }

    /// The worker said which text field has the keyboard (`ScreenEvent::Field`), or none.
    pub fn set_field(&mut self, field: Option<TextField>, cx: &mut Context<Self>) {
        if self.field != field {
            self.field = field;
            cx.notify();
        }
    }

    /// Whether the worker's field that has the keyboard is a password field.
    #[must_use]
    pub fn in_password_field(&self) -> bool {
        self.field.is_some_and(|f| f.secure)
    }

    /// Where the worker's caret is drawn, from the view's window origin, in points: the
    /// stream pixels it was read in scale by the size input maps with, as the pointer's do.
    fn caret_bounds(&self) -> Option<Bounds<Pixels>> {
        let caret = self.field?.caret?;
        let (w, h) = self.mapped_f32();
        let (w, h) = (w.max(1.0), h.max(1.0));
        let (x, y) = self.frame_to_body(self.zoom.to_frame((caret.x / w, caret.y / h)));
        let tall = caret.height / h * self.frame().size.1 * self.zoom.scale();
        let origin = point(self.bounds.origin.x + x, self.bounds.origin.y + y);
        Some(Bounds::new(origin, size(px(caret.width.max(1.0)), px(tall.max(1.0)))))
    }

    /// The worker said which cursor it shows (`ScreenEvent::Cursor`): draw that picture at the
    /// pointer from now on, or the arrow again for `None`.
    ///
    /// The system pointer takes the picture at once, before any frame
    /// (`App::set_cursor_image`); the view draws again only when the drawn pointer shows it or
    /// the system pointer changes between the picture and the arrow.
    pub fn set_cursor_shape(&mut self, shape: Option<CursorShape>, cx: &mut Context<Self>) {
        let image = shape.as_ref().and_then(system_picture);
        self.system_pointer.1 = image.is_some();
        cx.set_cursor_image(self.system_pointer.0, image);
        std::mem::replace(&mut self.pointer, Pointer::from_shape(shape)).drop_image(cx);
        let now = cx.background_executor().now();
        if self.drawn.is_some() || self.pointer_style(now) != self.styled {
            cx.notify();
        }
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
            self.unlocking = false;
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

    /// The overlay's plain line and its engineering lines, as last sampled.
    pub(crate) fn hud_text(&self) -> Option<(Vec<Figure>, SharedString)> {
        self.hud.as_ref().map(|hud| (hud.summary.clone(), hud.text.clone()))
    }

    /// The overlay's trends, as last sampled.
    #[cfg(test)]
    pub(crate) fn hud_history(&self) -> Option<History> {
        self.hud.as_ref().map(|hud| hud.history.clone())
    }

    /// Sample the overlay's figures, once a [`HUD_PERIOD`] ([`Self::read_health`]), never in a
    /// render: a render that sampled whenever a period had passed drew figures a frame drawn a
    /// moment later would not, and the frame shown was stale against it.
    fn sample_hud(&mut self, cx: &App) {
        let Some(hud) = self.hud.as_mut() else { return };
        let now = Instant::now();
        let elapsed = now.duration_since(hud.sampled_at);
        if !elapsed.is_zero() {
            // The frame and pacing percentiles sort their rings: once a HUD period, never
            // per paint.
            let ui = crate::frames::stats(cx);
            let stats = self.handle.stats();
            let secs = elapsed.as_secs_f64();
            #[expect(clippy::cast_precision_loss, reason = "counter deltas over a second")]
            let mbps = stats.bytes.saturating_sub(hud.sample.bytes) as f64 * 8.0 / secs / 1e6;
            let pacing = self.glass.pacing();
            let glass = self.glass.glass();
            let input = HudInput {
                size: self.size,
                scale: self.quality.scale,
                chroma: self.shape.map(|shape| shape.chroma),
                seams: self.shape.and_then(|shape| shape.seam).map(|_seam| {
                    let seams = self.glass.seams();
                    (seams.together, seams.split)
                }),
                target_fps: self.quality.fps,
                paint: self.glass.rate(),
                mbps,
                rtt: self.shown_rtt,
                frame_age: self.glass.age(),
                rate: self.rate,
                stats: &stats,
                pacing: &pacing,
                capture: &glass.capture,
                ui: ui.as_ref(),
            };
            hud.summary = health::summary(&input);
            hud.text = hud_lines(&input).into();
            hud.history.push(input.frame_age, stats.jitter, input.rtt);
            hud.sample = stats;
            hud.sampled_at = now;
        }
    }

    /// Whether the worker has sent any sound to this client (the mute control is pointless
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

    /// Silence or resume the worker's sound on this client only: every tile of the worker hears
    /// it, so the caller redraws them all.
    pub fn toggle_mute(&self) {
        self.handle.set_muted(!self.handle.muted());
    }

    /// The workspace reports how wide the view is painted (device pixels) so the stream can be
    /// downscaled at the worker when it is drawn small. Quantised to quarter steps and rate
    /// limited: a change inside the cooldown is taken when it ends, the latest width asked for
    /// winning. A picture narrower than its tile's aspect is drawn narrower than the tile, and a
    /// picture zoomed inside the tile wider; each asks for the width it is drawn at. A zoomed
    /// picture asks for its region with it (`wanted_region`): what it shows and a margin
    /// round it, at that scale.
    pub fn set_painted_width(&mut self, device_px: f32, cx: &Context<Self>) {
        self.painted = device_px;
        let body = f32::from(self.bounds.size.width);
        let share = if body > 0.0 { self.frame().size.0 / body } else { 1.0 };
        let drawn = device_px * share * self.zoom.scale();
        let wanted = (drawn / self.native.0).clamp(MIN_SCALE, 1.0);
        let bucket = (wanted * 4.0).ceil() / 4.0;
        let region = self.wanted_region();
        if (bucket - self.quality.scale).abs() < f32::EPSILON && region == self.quality.region {
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
        self.quality.region = region;
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
    /// scale, so the native size follows from it, and the picture is drawn at it.
    pub fn set_geometry(&mut self, width: u32, height: u32, cx: &mut Context<Self>) {
        if self.size != (width, height) {
            cx.notify();
        }
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

    /// The picture's shape changed (the first picture, a new size or chroma): what the view
    /// draws around it is laid out again.
    fn shaped(&mut self, shape: glass::Shape, cx: &mut Context<Self>) {
        if self.shape == Some(shape) {
            return;
        }
        let first = self.shape.is_none();
        self.shape = Some(shape);
        // A region's picture is a part of the stream, whose size `Opened` and `Geometry` say.
        if shape.region.is_none() {
            self.size = shape.size;
        }
        if first {
            cx.emit(ScreenViewEvent::Ready);
        }
        cx.notify();
    }

    /// Put `buffer` up as a picture off the stream would be (tests and the workspace's frame
    /// measurements): on the layer, with the view told of its shape at once.
    #[cfg(test)]
    pub(crate) fn show_picture(&mut self, buffer: CVPixelBuffer, cx: &mut Context<Self>) {
        let picture = glass::Picture::new(buffer);
        let shape = glass::Shape::of(&picture);
        self.glass.offer(picture, glass::test_stamp(self.test_pictures));
        self.test_pictures = self.test_pictures.saturating_add(1);
        self.shaped(shape, cx);
    }

    /// Put a picture of `region` of the target up (tests).
    #[cfg(test)]
    fn show_region(&mut self, buffer: CVPixelBuffer, region: Region, cx: &mut Context<Self>) {
        let picture = glass::Picture::of_region(buffer, region);
        let shape = glass::Shape::of(&picture);
        self.glass.offer(picture, glass::test_stamp(self.test_pictures));
        self.test_pictures = self.test_pictures.saturating_add(1);
        self.shaped(shape, cx);
    }

    /// Put a striped picture up (tests): `top` showing its first `top_rows`, `lower` its rows
    /// from `lower_from`.
    #[cfg(test)]
    fn show_striped(
        &mut self,
        (top, top_rows): (CVPixelBuffer, u32),
        (lower, lower_from): (CVPixelBuffer, u32),
        cx: &mut Context<Self>,
    ) {
        let picture = glass::Picture::striped(top, top_rows, lower, lower_from);
        let shape = glass::Shape::of(&picture);
        self.glass.offer(picture, glass::test_stamp(self.test_pictures));
        self.test_pictures = self.test_pictures.saturating_add(1);
        self.shaped(shape, cx);
    }

    /// Arrival → glass numbers for the last pictures (the overlay and the app self-test).
    #[must_use]
    pub fn pacing(&self) -> PacingStats {
        self.glass.pacing()
    }

    fn send(&self, req: ScreenRequest) {
        self.out.send(ClientMsg::Screen(req));
    }

    /// Tell the worker whether this tile has the keyboard in an active window, on a change
    /// only: it favours the focused stream when its encoders are full.
    fn tell_focus(&mut self, focused: bool) {
        if focused != self.focus_told {
            self.focus_told = focused;
            self.send(ScreenRequest::Focused { stream: self.stream, focused });
        }
    }

    /// Send `input` after the keys taken ahead of it (`keyboard`).
    fn input(&mut self, input: ScreenInput) {
        self.drain_taken();
        self.send_input(input);
    }

    fn send_input(&mut self, input: ScreenInput) {
        if self.watching() {
            return;
        }
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
        let (fx, fy) = self.zoom.to_picture(self.frame_fraction(position));
        let (stream_w, stream_h) = self.mapped_f32();
        let (x, y) = (fx * stream_w, fy * stream_h);
        tracing::trace!(?position, bounds = ?self.bounds, mapped = ?self.mapped, x, y, "to_stream");
        (x, y)
    }

    /// [`Self::to_stream`] held to the picture: the nearest pixel of its edge for a position off
    /// it.
    fn to_stream_edge(&self, position: Point<Pixels>) -> (f32, f32) {
        let (x, y) = self.to_stream(position);
        let (w, h) = self.mapped_f32();
        (x.clamp(0.0, w), y.clamp(0.0, h))
    }

    /// Window position → a point of the frame, in fractions of its size.
    fn frame_fraction(&self, position: Point<Pixels>) -> (f32, f32) {
        let frame = self.frame();
        (
            (f32::from(position.x) - f32::from(self.bounds.origin.x) - frame.origin.0)
                / frame.size.0,
            (f32::from(position.y) - f32::from(self.bounds.origin.y) - frame.origin.1)
                / frame.size.1,
        )
    }

    /// Where the picture is drawn at fit, in points from the body's top-left: the stream's aspect
    /// kept, centred, never of zero size ([`zoom::fit`]). The zoom and every pointer mapping
    /// are fractions of this rectangle, not of the body.
    fn frame(&self) -> zoom::Frame {
        let body = (f32::from(self.bounds.size.width), f32::from(self.bounds.size.height));
        let frame = zoom::fit(body, self.size);
        zoom::Frame { size: (frame.size.0.max(1.0), frame.size.1.max(1.0)), ..frame }
    }

    /// The frame's size in points, never zero.
    fn frame_size(&self) -> (f32, f32) {
        self.frame().size
    }

    /// How the picture is drawn over the body.
    #[must_use]
    pub const fn zoom(&self) -> Zoom {
        self.zoom
    }

    /// The scale at which a pixel of the target is a pixel of this device.
    fn one_to_one(&self) -> f32 {
        zoom::one_to_one(self.native.0, self.frame_size().0, self.scale_factor)
    }

    /// Draw the picture as `zoom` (held to its limits), say so, and ask the worker for the
    /// scale and the region it is now drawn at once the zoom settles: [`QUALITY_COOLDOWN`] after
    /// its last change, so a pinch or a pan asks once, at its end, and not for every step on
    /// the way (each new size of region is a new encoder session and a keyframe). A pan that
    /// shows what the stream does not carry asks at once.
    fn set_zoom(&mut self, zoom: Zoom, cx: &mut Context<Self>) {
        let zoom = zoom.clamped(zoom::max_scale(self.one_to_one()));
        if zoom == self.zoom {
            return;
        }
        let scaled = (zoom.scale() - self.zoom.scale()).abs() > f32::EPSILON;
        self.zoom = zoom;
        if scaled {
            self.show_readout(cx);
        }
        if self.painted > 0.0 {
            if !scaled && !self.streams_the_view() {
                self.set_painted_width(self.painted, cx);
            } else {
                self.settle_zoom(cx);
            }
        }
        cx.notify();
    }

    /// Ask for the scale and the region the picture is drawn at [`QUALITY_COOLDOWN`] from now,
    /// unless the zoom moves again before then.
    fn settle_zoom(&mut self, cx: &Context<Self>) {
        self.settle = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(QUALITY_COOLDOWN).await;
            let _gone = this.update(cx, |this, cx| {
                this.settle = None;
                this.set_painted_width(this.painted, cx);
            });
        }));
    }

    /// The part of the picture the body shows, in fractions of the picture.
    fn in_view(&self) -> zoom::Part {
        let body = (f32::from(self.bounds.size.width), f32::from(self.bounds.size.height));
        zoom::in_view(self.zoom, body, self.frame())
    }

    /// The region of the target this view wants streamed, in its native pixels: what it shows
    /// and a margin round it ([`zoom::streamed`]); `None` at fit, before the view is laid out,
    /// or when that is all of the target. A side is a function of the zoom alone, so a pan
    /// asks for a region of the size in force, which the worker moves with no new session.
    fn wanted_region(&self) -> Option<Region> {
        if self.zoom.is_fit() || self.bounds.size.width <= px(0.0) {
            return None;
        }
        let (left, top, right, bottom) = zoom::streamed(self.in_view())?;
        let side = |from: f32, to: f32, native: f32| -> (u16, u16) {
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped")]
            let even = |v: f32| ((v / 2.0).round() * 2.0).clamp(0.0, 65_534.0) as u16;
            let len = even((to - from) * native).max(2);
            let start = even(from * native).min(even(native).saturating_sub(len));
            (start, len)
        };
        let (x, w) = side(left, right, self.native.0);
        let (y, h) = side(top, bottom, self.native.1);
        Some(Region { x, y, w, h })
    }

    /// Whether what the body shows is all inside what the stream carries: the whole target,
    /// or the region last asked for.
    fn streams_the_view(&self) -> bool {
        let Some(region) = self.quality.region else { return true };
        let (w, h) = (self.native.0.max(1.0), self.native.1.max(1.0));
        let part = (
            f32::from(region.x) / w,
            f32::from(region.y) / h,
            f32::from(region.x.saturating_add(region.w)) / w,
            f32::from(region.y.saturating_add(region.h)) / h,
        );
        zoom::covers(part, self.in_view())
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
        // Each pinch keeps the side it began on, so turning the toggle over halfway through
        // leaves neither a zoom nor the remote app's gesture without its end. A start is a new
        // pinch whatever the last one left behind, should its end never have come.
        let starts = ev.phase == TouchPhase::Started;
        if starts {
            self.remote_pinch = false;
        }
        let zooming = self.two.is_some() && !starts;
        if self.remote_pinch || (self.remote_gestures && !self.touch && !zooming) {
            self.pinch_remote(ev, cx);
            return;
        }
        if self.two.is_none() && (ev.phase != TouchPhase::Started || !self.inside(ev.position)) {
            return;
        }
        cx.stop_propagation();
        let now = cx.background_executor().now();
        let at = self.frame_fraction(ev.position);
        let (w, h) = self.frame_size();
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
                    self.trackpad_scroll((0.0, 0.0), ScrollPhase::Ended, now);
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

    /// A trackpad's pinch for the app under the fingers on the worker: each report as the
    /// magnification AppKit read, in its phase, at the stream pixel under the fingers. Like the
    /// zoom's, it belongs to what it started on: one that began off the picture is not taken,
    /// and one that began on it is followed to its end even off it (`docs/decisions/input.md`,
    /// "Trackpad gestures reach the remote app").
    fn pinch_remote(&mut self, ev: &PinchEvent, cx: &mut Context<Self>) {
        if !self.remote_pinch && (ev.phase != TouchPhase::Started || !self.inside(ev.position)) {
            return;
        }
        cx.stop_propagation();
        self.remote_pinch = !matches!(ev.phase, TouchPhase::Ended | TouchPhase::Cancelled);
        let (x, y) = self.to_stream_edge(ev.position);
        let time_us = self.time_us(cx.background_executor().now());
        let phase = gesture_phase(ev.phase);
        self.input(ScreenInput::Magnify { delta: ev.delta, phase, x, y, time_us });
    }

    /// Send this picture's trackpad gestures to the worker (`true`) or keep them here. A pinch
    /// under way finishes where it began, here or on the worker. The worker is told, in order with
    /// the input, since a trackpad scroll comes with its gesture there only while they are
    /// sent.
    pub fn set_remote_gestures(&mut self, on: bool, cx: &mut Context<Self>) {
        if on != self.remote_gestures {
            self.remote_gestures = on;
            self.input(ScreenInput::Gestures { remote: on });
            cx.notify();
        }
    }

    /// Flip [`Self::set_remote_gestures`] (the palette's command).
    pub fn toggle_remote_gestures(&mut self, cx: &mut Context<Self>) {
        self.set_remote_gestures(!self.remote_gestures, cx);
    }

    /// Whether this picture's trackpad gestures go to the worker.
    #[must_use]
    pub const fn remote_gestures(&self) -> bool {
        self.remote_gestures
    }

    /// One report of the two fingers: `delta` is the scale step less one, `points` and `at`
    /// the centroid in the body's points and fractions.
    fn two_step(&mut self, delta: f32, points: (f32, f32), at: (f32, f32), cx: &mut Context<Self>) {
        let Some(two) = self.two.as_mut() else { return };
        let step = two.step((1.0 + delta).max(0.01), points);
        let max = zoom::max_scale(self.one_to_one());
        let (w, h) = self.frame_size();
        if self.trackpad.is_some() {
            match step.kind {
                touch::TwoKind::Pinch => self.set_zoom(self.zoom.about(at, step.factor, max), cx),
                touch::TwoKind::Drag => {
                    let phase =
                        if self.two_scrolling { ScrollPhase::Changed } else { ScrollPhase::Began };
                    self.two_scrolling = true;
                    self.trackpad_scroll(step.by, phase, cx.background_executor().now());
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

    /// A two-finger drag in trackpad mode: a precise scroll at the pointer, read `now`.
    fn trackpad_scroll(&mut self, by: (f32, f32), phase: ScrollPhase, now: Instant) {
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
            time_us: self.time_us(now),
        });
    }

    /// When a gesture's report was read, for the wire: microseconds since this view was made,
    /// the low 32 bits ([`ScreenInput::time_us`]). GPUI hands over no time of AppKit's own, so
    /// it is the moment the view takes the event, which the main thread's own delay is in.
    fn time_us(&self, now: Instant) -> u32 {
        #[expect(clippy::cast_possible_truncation, reason = "the wire's low 32 bits by design")]
        let us = now.saturating_duration_since(self.epoch).as_micros() as u32;
        us
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
        header: StreamHeader,
        theme: &Theme,
        k: f32,
    ) -> Option<gpui::AnyElement> {
        if !header.touch {
            return None;
        }
        let id = format!("trackpad-{}", header.stream);
        let icon = crate::icons::IconName::MousePointer2;
        let button = kit::icon_toggle(theme, id, icon, TRACKPAD_MODE, header.trackpad, k);
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
        let (w, h) = self.frame_size();
        let at = self.frame_fraction(ev.position);
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
        if !self.focus.is_focused(window) && !self.watching() {
            self.send(ScreenRequest::Focus(self.stream));
        }
        self.focus.focus(window, cx);
        cx.emit(ScreenViewEvent::Pressed);
    }

    /// Whether `p` is over the picture as it is drawn: inside the body, and not on the bare
    /// body beside a picture of another aspect.
    fn inside(&self, p: Point<Pixels>) -> bool {
        let on_picture = |f: f32| (0.0..=1.0).contains(&f);
        let (fx, fy) = self.zoom.to_picture(self.frame_fraction(p));
        self.bounds.contains(&p) && on_picture(fx) && on_picture(fy)
    }

    /// The pointer from the body's top-left, in view pixels, through the zoom
    /// ([`Self::pointer_spot`]).
    fn cursor_offset(&self, now: Instant) -> (Pixels, Pixels) {
        self.frame_to_body(self.zoom.to_frame(self.pointer_spot(now).0))
    }

    /// A point of the frame (fractions of it) in points from the body's top-left.
    fn frame_to_body(&self, (u, v): (f32, f32)) -> (Pixels, Pixels) {
        let frame = self.frame();
        (px(u.mul_add(frame.size.0, frame.origin.0)), px(v.mul_add(frame.size.1, frame.origin.1)))
    }

    /// The pointer moved over the picture: sent to the worker, and drawn there in the same
    /// frame, not when the worker's sample of it comes back.
    fn mouse_move(&mut self, ev: &MouseMoveEvent, _w: &mut Window, cx: &mut Context<Self>) {
        // Watching, the pointer drawn is the agent's.
        if self.watching() || !self.inside(ev.position) {
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
        if self.watching() {
            cx.stop_propagation();
            return;
        }
        // On glass a double tap is the zoom's: the first tap has clicked already.
        if self.touch && self.trackpad.is_none() && ev.click_count == 2 {
            let target = zoom::double_tap_target(self.one_to_one());
            let max = zoom::max_scale(self.one_to_one());
            self.set_zoom(self.zoom.toggled(self.frame_fraction(ev.position), target, max), cx);
            cx.stop_propagation();
            return;
        }
        // A press on the bare body beside a picture of another aspect only takes the keys:
        // nothing of the remote screen is under it.
        if !self.inside(ev.position) {
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
        if button == slopty_proto::input::MouseButton::Left {
            self.drag_out_released(cx);
        }
        self.buttons.swap_remove(at);
        let (x, y) = self.to_stream_edge(ev.position);
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
        // Over the bare body beside the picture a scroll goes on at the picture's edge, so a
        // gesture that drifts off it still ends where it began.
        let (x, y) = self.to_stream_edge(ev.position);
        let mods = keys::mods(ev.modifiers);
        let time_us = self.time_us(cx.background_executor().now());
        // A finger landing on a fling stops it, and macOS closes the old momentum before it
        // opens the new gesture.
        if precise && ev.touch_phase == TouchPhase::Started {
            self.end_momentum(x, y, mods, time_us);
        }
        if self.scroll_pans(precise, ev) {
            self.pan_by(ev.delta, cx);
            cx.stop_propagation();
            return;
        }
        let (phase, momentum) = self.scroll_phases(precise, ev.touch_phase, ev.momentum_phase);
        self.input(ScreenInput::Scroll { dx, dy, precise, phase, momentum, x, y, mods, time_us });
        if precise {
            if self.scrolling == Scrolling::Momentum {
                self.arm_momentum_end(x, y, mods, cx);
            } else {
                self.momentum_end = None;
            }
        }
        cx.stop_propagation();
    }

    /// Whether this wheel event pans a zoomed picture here rather than scrolling the remote app:
    /// ⌥ held on a zoomed picture under this Mac's pointer. A trackpad's gesture decides as it
    /// begins and keeps its side to the end of its coast, as a pinch does, so letting go of ⌥
    /// halfway leaves neither the pan nor the remote app's scroll without its end; a wheel's
    /// notch decides on its own (`docs/decisions/input.md`, "A zoomed picture pans when the
    /// pointer pushes at the edge").
    fn scroll_pans(&mut self, precise: bool, ev: &ScrollWheelEvent) -> bool {
        let asks = ev.modifiers.alt && !self.zoom.is_fit() && !self.touch;
        if !precise {
            return asks;
        }
        if ev.touch_phase == TouchPhase::Started && ev.momentum_phase.is_none() {
            self.pan_scroll = asks;
        }
        self.pan_scroll
    }

    /// Pan the zoomed picture by a scroll's `delta`, the way the fingers moved; a wheel's line
    /// is `spacing.xl`.
    fn pan_by(&mut self, delta: ScrollDelta, cx: &mut Context<Self>) {
        let by = delta.pixel_delta(px(self.theme.spacing.xl));
        let (w, h) = self.frame_size();
        let max = zoom::max_scale(self.one_to_one());
        let panned = self.zoom.panned((f32::from(by.x) / w, f32::from(by.y) / h), max);
        self.set_zoom(panned, cx);
        self.follow_pointer(cx);
    }

    /// This Mac's pointer is at `position` in the window, over the tile or not: a zoomed
    /// picture pans while it pushes at an edge of the body ([`zoom::edge_push`]), as Screen
    /// Sharing's "When the cursor reaches an edge" does, and stops when it leaves the body.
    fn track_edges(&mut self, position: Option<Point<Pixels>>, cx: &Context<Self>) {
        let over = position.filter(|&at| !self.touch && self.bounds.contains(&at));
        self.edge_at = over.filter(|_| !self.zoom.is_fit());
        if self.edge_at.is_some() && !self.edge_panning && self.edge_push() != (0.0, 0.0) {
            self.edge_panning = true;
            Self::push_at_edges(cx);
        }
    }

    /// How hard the pointer pushes at the body's edges now, each axis in `-1..=1`.
    fn edge_push(&self) -> (f32, f32) {
        let Some(at) = self.edge_at else { return (0.0, 0.0) };
        let origin = self.bounds.origin;
        let at = (f32::from(at.x - origin.x), f32::from(at.y - origin.y));
        let body = (f32::from(self.bounds.size.width), f32::from(self.bounds.size.height));
        zoom::edge_push(at, body, self.theme.spacing.xl)
    }

    /// Pan while the pointer pushes, a step every [`EDGE_TICK`] for the time it took.
    fn push_at_edges(cx: &Context<Self>) {
        cx.spawn(async move |this, cx| {
            let executor = cx.background_executor().clone();
            let mut last = executor.now();
            loop {
                executor.timer(EDGE_TICK).await;
                let now = executor.now();
                let seconds = now.saturating_duration_since(last).as_secs_f32();
                last = now;
                let going = this.update(cx, |view, cx| view.edge_step(seconds, cx));
                if !matches!(going, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// One step of the push, `seconds` of it: the picture panned toward the edges pushed at,
    /// harder the deeper the pointer is in the band, and the worker's pointer moved to what is
    /// now under this Mac's. Whether to go on: not once nothing pushes or the picture's own edge
    /// is reached.
    fn edge_step(&mut self, seconds: f32, cx: &mut Context<Self>) -> bool {
        let push = self.edge_push();
        if push == (0.0, 0.0) || self.zoom.is_fit() {
            self.edge_panning = false;
            return false;
        }
        let step = |p: f32| -p * p.abs() * EDGE_PAN_FRAMES_PER_S * seconds;
        let max = zoom::max_scale(self.one_to_one());
        let panned = self.zoom.panned((step(push.0), step(push.1)), max);
        if panned == self.zoom {
            self.edge_panning = false;
            return false;
        }
        self.set_zoom(panned, cx);
        self.follow_pointer(cx);
        true
    }

    /// The picture moved under this Mac's still pointer: the worker's pointer goes to what is
    /// under it now.
    fn follow_pointer(&mut self, cx: &mut Context<Self>) {
        let Some(at) = self.edge_at.filter(|&at| self.inside(at)) else { return };
        let now = cx.background_executor().now();
        let (x, y) = self.to_stream(at);
        self.place((x, y), now);
        self.input(ScreenInput::Move { x, y });
        self.redraw_pointer(now, cx);
    }

    /// The `phase` and `momentum` a worker `CGEvent` needs for this wheel event, moving the
    /// gesture on as it goes. A mouse wheel notch is part of no gesture and carries neither.
    const fn scroll_phases(
        &mut self,
        precise: bool,
        touch: TouchPhase,
        momentum: Option<TouchPhase>,
    ) -> (ScrollPhase, ScrollPhase) {
        const fn phase(touch: TouchPhase) -> ScrollPhase {
            match touch {
                TouchPhase::Started => ScrollPhase::Began,
                TouchPhase::Moved => ScrollPhase::Changed,
                TouchPhase::Ended => ScrollPhase::Ended,
                TouchPhase::Cancelled => ScrollPhase::Cancelled,
            }
        }
        if !precise {
            return (ScrollPhase::None, ScrollPhase::None);
        }
        if let Some(momentum) = momentum {
            self.scrolling = match momentum {
                TouchPhase::Started | TouchPhase::Moved => Scrolling::Momentum,
                TouchPhase::Ended | TouchPhase::Cancelled => Scrolling::Idle,
            };
            return (ScrollPhase::None, phase(momentum));
        }
        match (touch, self.scrolling) {
            // gpui reads `NSEventPhaseMayBegin` and `NSEventPhaseBegan` as the same `Started`, so
            // fingers that rest before they push open the gesture twice. The second one is the
            // same gesture; telling the worker it began again would restart its rubber-banding.
            (TouchPhase::Started, Scrolling::Fingers) | (TouchPhase::Moved, _) => {
                (ScrollPhase::Changed, ScrollPhase::None)
            }
            (TouchPhase::Started, _) => {
                self.scrolling = Scrolling::Fingers;
                (ScrollPhase::Began, ScrollPhase::None)
            }
            (TouchPhase::Ended | TouchPhase::Cancelled, _) => {
                self.scrolling = Scrolling::Idle;
                (phase(touch), ScrollPhase::None)
            }
        }
    }

    /// Wait out [`MOMENTUM_GAP`] and, if nothing else has arrived, close the fling.
    fn arm_momentum_end(&mut self, x: f32, y: f32, mods: Mods, cx: &Context<Self>) {
        self.momentum_end = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(MOMENTUM_GAP).await;
            let _gone = this.update(cx, |this, cx| {
                let time_us = this.time_us(cx.background_executor().now());
                this.end_momentum(x, y, mods, time_us);
            });
        }));
    }

    /// The zero-delta `momentumPhase = End` macOS sends when a fling stops, for a coast whose
    /// own close never came: without it the remote app is left latched to a scroll that never
    /// ended.
    fn end_momentum(&mut self, x: f32, y: f32, mods: Mods, time_us: u32) {
        if self.scrolling != Scrolling::Momentum {
            return;
        }
        self.scrolling = Scrolling::Idle;
        self.momentum_end = None;
        self.input(ScreenInput::Scroll {
            dx: 0.0,
            dy: 0.0,
            precise: true,
            phase: ScrollPhase::None,
            momentum: ScrollPhase::Ended,
            x,
            y,
            mods,
            time_us,
        });
    }

    /// Focus left the view or the window went inactive: release on the worker every button,
    /// key and modifier whose press went there, since the release never will (⌘-tab away with
    /// ⌘ held, a click on another tile while a key is down) — input stuck down on the worker
    /// is the one thing a remote desktop must never leave behind. Keys go before modifiers.
    fn let_go(&mut self, cx: &Context<Self>) {
        self.keyboard_blurred(cx);
        let (x, y) = self.pointer_at;
        for button in std::mem::take(&mut self.buttons) {
            self.send_input(ScreenInput::Button {
                button,
                down: false,
                x,
                y,
                clicks: 1,
                mods: Mods::empty(),
            });
        }
        let (modifiers, plain): (Vec<KeyCode>, Vec<KeyCode>) =
            std::mem::take(&mut self.held).into_iter().partition(|&code| is_modifier(code));
        for code in plain.into_iter().chain(modifiers) {
            let action = KeyAction::Release;
            self.send_input(ScreenInput::Key { code, action, mods: Mods::empty() });
        }
        self.modifiers = Modifiers::default();
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

    /// Type `text` on the worker as committed text is, a line break as ↩, so it lands where a
    /// paste cannot (a login window, a field that refuses paste). Only the first
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
            self.commit_text(&burst, cx);
        }
        !self.typing.is_empty()
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

    /// The phone's "paste" key: ⌘V on the worker, this client's clipboard pushed first.
    pub fn paste_key(&mut self, cx: &mut Context<Self>) {
        self.press(chord("v"), cx);
    }

    /// The phone's "copy" key: ⌘C on the worker; the worker's pasteboard then flows back here.
    pub fn copy_key(&mut self, cx: &mut Context<Self>) {
        self.press(chord("c"), cx);
    }

    /// Keep a native host in the window the view draws in, `here`, and the pictures' layer on
    /// it once there is a picture: a view drawn in another window (a tile popped out) gets a
    /// host there, and its layer moves to it.
    ///
    /// A striped picture gets a second host and layer for its lower stripe, stacked under the
    /// first one's ([`stripe_places`]).
    fn place_layer(&mut self, here: gpui::AnyWindowHandle, window: &mut Window, cx: &mut App) {
        if self.host.as_ref().is_none_or(|host| host.window != here) {
            self.glass.detach();
            self.host = self.new_host(here, window, cx);
            self.lower_host = None;
        }
        let Some(host) = self.host.as_mut() else { return };
        let Some(shape) = self.shape else { return };
        if !host.tried {
            host.tried = true;
            if let Err(error) = self.glass.attach(&host.native) {
                tracing::warn!(%error, "no video layer for the picture");
            }
        }
        if shape.seam.is_none() {
            return;
        }
        if self.lower_host.is_none() {
            self.lower_host = self.new_host(here, window, cx);
        }
        let Some(lower) = self.lower_host.as_mut() else { return };
        if !lower.tried {
            lower.tried = true;
            if let Err(error) = self.glass.attach_lower(&lower.native) {
                tracing::warn!(%error, "no video layer for the lower stripe");
            }
        }
    }

    /// A native host for a layer of the picture in the window `here`.
    fn new_host(
        &self,
        here: gpui::AnyWindowHandle,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Host> {
        let options =
            NativeHostOptions { opaque: true, interactive: false, label: Some(self.label.clone()) };
        window
            .create_native_host(options, cx)
            .inspect_err(|error| tracing::warn!(%error, "no native host for the picture's layer"))
            .ok()
            .map(|native| Host { window: here, native, tried: false })
    }

    /// The pointer drawn at `spot`, a point of the picture: the worker's cursor picture when it
    /// has sent one, else a drawn arrow. An element of the pointer's own extent, which tests
    /// find where it is drawn (`screen-pointer`).
    fn cursor_overlay(&self, spot: Option<(f32, f32)>) -> Option<impl IntoElement + use<>> {
        let (x, y) = self.frame_to_body(self.zoom.to_frame(spot?));
        let at = point(x, y);
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

/// Where a paint placed the layer and the mask that clipped it (tests).
#[cfg(test)]
type LayerAt = Rc<std::cell::Cell<Option<(Bounds<Pixels>, Bounds<Pixels>)>>>;

/// The native host a view places its picture's layer from, in one window.
struct Host {
    window: gpui::AnyWindowHandle,
    native: NativeHost,
    /// A layer was attached to it, or tried: a failure is not tried again every frame.
    tried: bool,
}

/// Where one stripe's layer goes, and the part of it that shows.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Place {
    /// The stripe's whole picture, its rows past the seam too.
    layer: Bounds<Pixels>,
    /// Its rows shown: the layer is clipped to these.
    shown: Bounds<Pixels>,
}

/// Where a striped picture drawn at `at` puts its two stripes' layers, top first: each picture
/// at the scale of the whole, the lower one with its row at the seam on the top one's first
/// row not shown, and each clipped at the seam. `None` for one picture.
///
/// Each layer's edges round to device pixels on their own, so at a scale that puts the seam
/// between them the two can sit up to a device pixel apart; at one to one they meet exactly.
fn stripe_places(at: Bounds<Pixels>, shape: glass::Shape) -> Option<[Place; 2]> {
    let seam = shape.seam?;
    #[expect(clippy::cast_precision_loss, reason = "pixel counts")]
    let row = |rows: u32| px(rows as f32 * f32::from(at.size.height) / shape.size.1.max(1) as f32);
    let top_layer = Bounds { origin: at.origin, size: size(at.size.width, row(seam.top)) };
    let cut = at.origin.y + row(seam.top_rows);
    let top_shown = Bounds { origin: at.origin, size: size(at.size.width, cut - at.origin.y) };
    let lower_layer = Bounds {
        origin: point(at.origin.x, cut - row(seam.lower_from)),
        size: size(at.size.width, row(seam.lower)),
    };
    let lower_shown = Bounds {
        origin: point(at.origin.x, cut),
        size: size(at.size.width, at.origin.y + at.size.height - cut),
    };
    Some([
        Place { layer: top_layer, shown: top_shown },
        Place { layer: lower_layer, shown: lower_shown },
    ])
}

/// Where a picture showing `region` of a target `native` pixels in size goes when the whole
/// target is drawn at `whole`.
fn region_bounds(whole: Bounds<Pixels>, region: Region, native: (f32, f32)) -> Bounds<Pixels> {
    let (w, h) = (native.0.max(1.0), native.1.max(1.0));
    let x = |v: u16| px(f32::from(v) / w * f32::from(whole.size.width));
    let y = |v: u16| px(f32::from(v) / h * f32::from(whole.size.height));
    Bounds {
        origin: whole.origin + point(x(region.x), y(region.y)),
        size: size(x(region.w), y(region.h)),
    }
}

/// Draw `picture` at `at` in GPUI's own frame: both stripes of a striped one, each clipped to
/// its rows.
fn paint_picture(window: &mut Window, at: Bounds<Pixels>, picture: &glass::Picture) {
    let Some([top, lower]) = stripe_places(at, glass::Shape::of(picture)) else {
        window.paint_surface(at, picture.buffer());
        return;
    };
    window.with_content_mask(Some(ContentMask { bounds: top.shown }), |window| {
        window.paint_surface(top.layer, picture.buffer());
    });
    if let Some(buffer) = picture.lower() {
        window.with_content_mask(Some(ContentMask { bounds: lower.shown }), |window| {
            window.paint_surface(lower.layer, buffer);
        });
    }
}

/// Where the picture of `picture` pixels is drawn in a body at `body`: the frame it fits at
/// ([`zoom::fit`]), or `zoom.scale()` times that with its top-left at `zoom.origin()` in the
/// frame's fractions.
fn picture_bounds(body: Bounds<Pixels>, picture: (u32, u32), zoom: Zoom) -> Bounds<Pixels> {
    let frame = zoom::fit((f32::from(body.size.width), f32::from(body.size.height)), picture);
    let (u, v) = zoom.origin();
    let scale = zoom.scale();
    Bounds {
        origin: body.origin
            + point(
                px(u.mul_add(frame.size.0, frame.origin.0)),
                px(v.mul_add(frame.size.1, frame.origin.1)),
            ),
        size: size(px(frame.size.0 * scale), px(frame.size.1 * scale)),
    }
}

/// Whether every stream's picture is drawn by GPUI too, over its layer ([`capture_pictures`]).
#[derive(Default)]
struct CapturePictures(bool);

impl Global for CapturePictures {}

/// While `on`, every stream draws its newest picture in GPUI's own frame too, over its layer.
///
/// A render of the window (`Window::render_to_image`) has GPUI's drawable only, and the layers
/// are the window server's to composite. The surface shader is the layer's, so the picture is
/// the one on the glass.
pub fn capture_pictures(on: bool, cx: &mut App) {
    cx.set_global(CapturePictures(on));
}

pub(crate) fn capturing(cx: &App) -> bool {
    cx.try_global::<CapturePictures>().is_some_and(|capture| capture.0)
}

/// What the pointer overlay paints.
enum PointerPaint {
    Image(Arc<RenderImage>),
    Arrow(Rc<Arrow>),
}

impl Drop for ScreenView {
    fn drop(&mut self) {
        self.handle.set_present(None);
        self.glass.detach();
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
    if let ClientMsg::Screen(ScreenRequest::Input { stream, input: to }) = &msg
        && let Some(ClientMsg::Screen(ScreenRequest::Input { stream: behind, input: last })) =
            waiting.back_mut()
        && behind == stream
        && same_move(last, to)
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

/// Whether `next` is a move that may take the place of `last`: a pointer move after a pointer
/// move, or a drag's after the same drag's.
fn same_move(last: &ScreenInput, next: &ScreenInput) -> bool {
    match (last, next) {
        (ScreenInput::Move { .. }, ScreenInput::Move { .. }) => true,
        (
            ScreenInput::Drag(DragInput::Move { drag: a, .. }),
            ScreenInput::Drag(DragInput::Move { drag: b, .. }),
        ) => a == b,
        _ => false,
    }
}

/// Whether `msg` may not be dropped: anything but input, and input that ends something — a key
/// or button release, a scroll gesture's or momentum's end, a drag's every step but its moves —
/// or that a letter would be missing without: text, the paste chord, Caps Lock's state, the
/// input source the keys are read under and its release.
const fn must_arrive(msg: &ClientMsg) -> bool {
    let ClientMsg::Screen(ScreenRequest::Input { input, .. }) = msg else { return true };
    matches!(
        input,
        ScreenInput::Drag(
            DragInput::Enter { .. }
                | DragInput::Leave { .. }
                | DragInput::Drop { .. }
                | DragInput::Catch { .. }
        ) | ScreenInput::Key { action: KeyAction::Release, .. }
            | ScreenInput::PasteChord { .. }
            | ScreenInput::Text { .. }
            | ScreenInput::Lock { .. }
            | ScreenInput::KeyboardSource { .. }
            | ScreenInput::KeyboardReleased
            | ScreenInput::Media { down: false, .. }
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
            let blur = cx.on_blur(&self.focus, window, |this, _window, cx| {
                this.tell_focus(false);
                this.let_go(cx);
            });
            // Taking the keyboard tells the worker this device's input source and Caps Lock.
            let focus = cx.on_focus(&self.focus, window, |this, window, _cx| {
                this.keyboard_focused();
                this.tell_focus(window.is_window_active());
            });
            let inactive = cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    this.tell_focus(false);
                    this.let_go(cx);
                } else if this.focus.is_focused(window) {
                    this.keyboard_focused();
                    this.tell_focus(true);
                }
            });
            let moved =
                cx.observe_window_bounds(window, |this, window, cx| this.follow_screen(window, cx));
            self.let_go = Some((here, [blur, focus, inactive, moved]));
            // A view made for a stream a reconnect opened may have the keyboard already.
            self.tell_focus(self.focus.is_focused(window) && window.is_window_active());
            self.follow_screen(window, cx);
        }
        self.place_layer(here, window, cx);
        self.renders = self.renders.wrapping_add(1);
        #[cfg(test)]
        {
            self.rendered_at = self.bounds;
        }
        // Whatever asked for this render, it draws the pointer as it is now: a change that
        // waited for a frame has it.
        let now = cx.background_executor().now();
        let drawn = self.pointer_drawn(now);
        self.drawn = drawn;
        let styled = self.pointer_style(now);
        self.styled = styled;
        let entity = cx.entity();
        let handler = cx.entity();
        let focus = self.focus.clone();
        let record_bounds = canvas(
            move |bounds, window, cx| {
                let scale_factor = window.scale_factor();
                let this = entity.read(cx);
                #[expect(clippy::float_cmp, reason = "any change of scale is news")]
                let moved = this.bounds != bounds || this.scale_factor != scale_factor;
                if moved {
                    // The pointer was laid out at the old bounds in this frame (the layer is
                    // placed from the body as painted): the next one lays it out at these, and
                    // asks for the stream's size again at the width it is now drawn.
                    entity.update(cx, |this, _| {
                        this.bounds = bounds;
                        this.scale_factor = scale_factor;
                    });
                    let entity = entity.downgrade();
                    window.on_next_frame(move |_window, cx| {
                        let _gone = entity.update(cx, |this, cx| {
                            this.set_painted_width(this.painted, cx);
                            cx.notify();
                        });
                    });
                }
            },
            // Registering as a text input is what raises the soft keyboard on iOS and lets an
            // input method compose; typed text arrives in `replace_text_in_range`.
            move |bounds, (), window, cx| {
                window.handle_input(&focus, ElementInputHandler::new(bounds, handler.clone()), cx);
                // Anywhere in the window, not just over the tile: a drag out of the worker's
                // app is handed over as it leaves (`drop`).
                let leaving = handler.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
                    if phase == gpui::DispatchPhase::Capture {
                        leaving.update(cx, |view, cx| {
                            view.drag_out_moved(event, cx);
                            view.track_edges(Some(event.position), cx);
                        });
                    }
                });
                let gone = handler.clone();
                window.on_mouse_event(move |_event: &MouseExitEvent, phase, _window, cx| {
                    if phase == gpui::DispatchPhase::Capture {
                        gone.update(cx, |view, cx| view.track_edges(None, cx));
                    }
                });
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

        let waited = self.shape.is_none() && past_grace("screen-waiting", window, cx);
        let picture = match (self.shape, &self.host) {
            (Some(shape), Some(host)) => {
                let native = host.native.clone();
                let lower_native = self.lower_host.as_ref().map(|host| host.native.clone());
                let (zoom, whole, target) = (self.zoom, self.size, self.native);
                let captured = capturing(cx).then(|| self.glass.last()).flatten();
                // Under a region's layer, the newest picture of the whole target, drawn by GPUI:
                // it fills what the region does not reach until a region that does comes.
                let base = shape.region.and_then(|_| self.glass.base());
                let (glass, laid_out) = (Arc::clone(&self.glass), Rc::clone(&self.laid_out));
                #[cfg(test)]
                let (layer_at, lower_at, base_at) = (
                    Rc::clone(&self.layer_at),
                    Rc::clone(&self.lower_at),
                    Rc::clone(&self.base_at),
                );
                // The layer goes where the picture is drawn, from the body as this frame lays
                // it out, clipped by the body and whatever clips the tile: a region's picture at
                // its region's place in the whole. GPUI keeps the pointer over it (no hitbox):
                // the view forwards it to the worker. A striped picture's two layers stack, each
                // clipped to its rows.
                canvas(
                    |_bounds, _window, _cx| {},
                    move |body, (), window, _cx| {
                        let picture = picture_bounds(body, whole, zoom);
                        let at = shape
                            .region
                            .map_or(picture, |region| region_bounds(picture, region, target));
                        // Pictures of another seam or region wait until the layers are placed
                        // for them.
                        let placed = glass::Placed { seam: shape.seam, region: shape.region };
                        if laid_out.get() != Some(placed) {
                            laid_out.set(Some(placed));
                            glass.place(placed);
                        }
                        if let Some(base) = &base {
                            paint_picture(window, picture, base);
                            #[cfg(test)]
                            base_at.set(Some(picture));
                        }
                        let Some([top, lower]) = stripe_places(at, shape) else {
                            window.paint_native(&native, at, gpui::Corners::default(), None);
                            #[cfg(test)]
                            layer_at.set(Some((at, window.content_mask().bounds)));
                            if let Some(picture) = captured {
                                window.paint_surface(at, picture.buffer());
                            }
                            return;
                        };
                        window.with_content_mask(
                            Some(ContentMask { bounds: top.shown }),
                            |window| {
                                window.paint_native(
                                    &native,
                                    top.layer,
                                    gpui::Corners::default(),
                                    None,
                                );
                                #[cfg(test)]
                                layer_at.set(Some((top.layer, window.content_mask().bounds)));
                                if let Some(picture) = &captured {
                                    window.paint_surface(top.layer, picture.buffer());
                                }
                            },
                        );
                        let mask = Some(ContentMask { bounds: lower.shown });
                        window.with_content_mask(mask, |window| {
                            if let Some(native) = &lower_native {
                                let corners = gpui::Corners::default();
                                window.paint_native(native, lower.layer, corners, None);
                                #[cfg(test)]
                                lower_at.set(Some((lower.layer, window.content_mask().bounds)));
                            }
                            if let Some(buffer) = captured.as_ref().and_then(glass::Picture::lower)
                            {
                                window.paint_surface(lower.layer, buffer);
                            }
                        });
                    },
                )
                .absolute()
                .inset_0()
                .into_any_element()
            }
            // The notice over the body says it; a second line under it would repeat it.
            (None, _) if matches!(self.source, SourceState::Locked | SourceState::Away) => {
                div().size_full().into_any_element()
            }
            (None, _) if waited => {
                let text = waiting_text(self.source);
                div()
                    // A status, not a picture: it is the only thing a screen reader can be told
                    // while the surface is empty, and it changes when the worker reports the
                    // source.
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
            }
            _ => div().size_full().into_any_element(),
        };
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

        let hud = self.hud_text().map(|(summary, text)| self.hud_panel(&summary, &text, cx));
        let console = self.console_overlay(cx);
        // While the view has the keys, the workspace's own chords stand back (`!Screen`); with
        // system shortcuts sent, so do the app's ⌘Q and ⌘H (`!SystemKeys`).
        let mut key_context = gpui::KeyContext::new_with_defaults();
        key_context.add("Screen");
        if self.system_keys {
            key_context.add(SYSTEM_KEYS_CTX);
        }
        div()
            .id("screen")
            .key_context(key_context)
            .role(gpui::accesskit::Role::Image)
            .aria_label(self.a11y_label().clone())
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .overflow_hidden()
            // A picture sits on the stage, dark in both appearances as media is, so its edge
            // reads as a screen's; until there is one, the body is the page its words are on.
            .bg(hsla(if self.shape.is_some() {
                self.theme.surfaces.stage
            } else {
                self.theme.content()
            }))
            .cursor(styled)
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
            .on_action(cx.listener(|this, _: &ToggleRemoteGestures, _window, cx| {
                this.toggle_remote_gestures(cx);
            }))
            .child(picture)
            .child(record_bounds)
            // The system's drag is the pointer while one is over the tile.
            .children(self.cursor_overlay(drawn.filter(|_| self.drop.is_none())))
            .children(self.drop_ring())
            .children(
                self.taking_icons(self.frame_to_body(self.zoom.to_frame(self.pointer_spot(now).0))),
            )
            .children(console)
            .children(readout)
            .children(self.driver_pill(cx))
            .children(hud)
    }
}

impl ScreenView {
    /// While the worker's Mac is locked or another session has its screens: the body dimmed
    /// under the modal scrim, whatever picture it last showed kept under it, and in its middle
    /// a lifted card that says so ([`console_notice`]). A status, so a screen reader hears the
    /// change. Nothing moves in or out: it comes and goes with the worker's word.
    ///
    /// A locked Mac's card offers [`UNLOCK_HERE`]: the scrim lifts to a slim line at the top
    /// saying what to type, and the keyboard goes to the stream, whose keys reach the lock
    /// screen as they would at the Mac. Slopty forwards the keystrokes and nothing else: it
    /// never keeps, fills or types a password.
    fn console_overlay(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let (title, detail) = console_notice(self.source);
        if title.is_empty() {
            return None;
        }
        let theme = &self.theme;
        if self.unlocking {
            return Some(self.unlock_line());
        }
        let icon = match self.source {
            SourceState::Away => crate::icons::IconName::MonitorOff,
            _ => crate::icons::IconName::Lock,
        };
        let unlock = (self.source == SourceState::Locked).then(|| {
            kit::button(theme, "screen-unlock-here", UNLOCK_HERE, kit::ButtonKind::Secondary)
                .debug_selector(|| "screen-unlock-here".to_owned())
                .mt(px(theme.spacing.md))
                .on_click(cx.listener(|this, _ev, window, cx| this.unlock_here(window, cx)))
        });
        let card = kit::elevate(div(), theme)
            .rounded(px(theme.radii.lg))
            .py(px(theme.spacing.lg))
            .max_w(px(kit::Overlay::List.bounds().0 / 2.0))
            .child(
                kit::notice(
                    theme,
                    1.0,
                    kit::notice_mark(theme, icon, 1.0),
                    title,
                    Some(SharedString::new_static(detail)),
                )
                .children(unlock),
            );
        Some(
            div()
                .id("screen-console")
                .debug_selector(|| "screen-console".to_owned())
                .role(gpui::accesskit::Role::Status)
                .aria_label(title)
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .p(px(theme.spacing.md))
                .bg(kit::scrim(theme))
                .child(card)
                .into_any_element(),
        )
    }

    /// "Unlock here": the scrim lifts and the stream takes the keyboard, so what is typed goes
    /// to the locked Mac's lock screen.
    pub fn unlock_here(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.source != SourceState::Locked {
            return;
        }
        self.unlocking = true;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Whether "Unlock here" lifted the scrim.
    #[must_use]
    pub const fn unlocking(&self) -> bool {
        self.unlocking
    }

    /// The slim line over a locked Mac's picture once "Unlock here" is pressed: what to type,
    /// on a lifted pill at the top, the rest of the body the stream's.
    fn unlock_line(&self) -> gpui::AnyElement {
        let theme = &self.theme;
        let pill = kit::elevate(kit::pill_frame(theme, 1.0), theme)
            .child(
                crate::icons::icon(
                    theme,
                    crate::icons::IconName::Lock,
                    crate::icons::IconSize::Inline,
                    hsla(theme.surfaces.text_muted),
                )
                .size(px(theme.typography.icon())),
            )
            .text_color(hsla(theme.surfaces.text_secondary))
            .font_family(theme.typography.ui_family.clone())
            .child(TYPE_THE_PASSWORD);
        div()
            .id("screen-unlocking")
            .debug_selector(|| "screen-unlocking".to_owned())
            .role(gpui::accesskit::Role::Status)
            .aria_label(TYPE_THE_PASSWORD)
            .absolute()
            .top(px(theme.spacing.xs))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(pill)
            .into_any_element()
    }

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
            // The figures are said as they are drawn, so a screen reader hears them and a
            // golden can find them to hold out of its comparison.
            div()
                .id("stream-stats-line")
                .debug_selector(|| "stream-stats-line".to_owned())
                .role(gpui::accesskit::Role::Label)
                .aria_label(line.clone())
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
        let trends = open.then(|| self.hud_trends()).flatten();
        let lines = open.then(|| {
            div()
                .debug_selector(|| "stream-stats-details-lines".to_owned())
                .pt(px(theme.spacing.xs))
                .border_t(kit::hair(theme))
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
            .text_size(px(theme.typography.small()))
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_ev, _w, cx| cx.stop_propagation())
            .child(
                div().flex().items_center().gap(px(theme.spacing.sm)).child(plain).child(details),
            )
            .children(trends)
            .children(lines);
        div()
            .absolute()
            .top(px(theme.spacing.sm))
            .right(px(theme.spacing.sm))
            .child(panel)
            .into_any_element()
    }
}

/// A trend's width, in points: a step for each of [`HUD_HISTORY`] samples, wide enough to
/// read a rise.
const TREND_WIDTH: f32 = 96.0;

impl ScreenView {
    /// Behind "Details", over the engineering lines: the last half minute of the frame's age,
    /// the jitter and the round trip, each a sparkline with its latest figure.
    fn hud_trends(&self) -> Option<gpui::AnyElement> {
        let history = &self.hud.as_ref()?.history;
        let theme = &self.theme;
        let s = theme.surfaces;
        let trend = |id: &'static str, name: &'static str, values: &VecDeque<f32>| {
            let latest = values.back().copied().filter(|v| v.is_finite());
            let figure = latest.map_or_else(|| "\u{2013}".to_owned(), |v| format!("{v:.0} ms"));
            let label = format!("{name} {figure}");
            div()
                .flex()
                .flex_col()
                .gap(px(theme.spacing.xxs))
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .gap(px(theme.spacing.sm))
                        .text_size(px(theme.typography.caption()))
                        .child(div().text_color(hsla(s.text_muted)).child(name))
                        .child(
                            kit::tabular(div())
                                .text_color(hsla(s.text_secondary))
                                .child(SharedString::from(figure)),
                        ),
                )
                .child(
                    kit::Spark::new(
                        id,
                        values.iter().copied().collect(),
                        history.pushed,
                        HUD_HISTORY,
                        label,
                    )
                    .tone(hsla(s.accent))
                    .size(px(TREND_WIDTH), px(theme.spacing.lg)),
                )
        };
        Some(
            div()
                .debug_selector(|| "stream-stats-trends".to_owned())
                .flex()
                .gap(px(theme.spacing.md))
                .child(trend("stream-trend-age", "Frame age", &history.age))
                .child(trend("stream-trend-jitter", "Jitter", &history.jitter))
                .child(trend("stream-trend-rtt", "Round trip", &history.rtt))
                .into_any_element(),
        )
    }
}

/// The system pointer over the picture. Before the first frame there is nothing to point at
/// yet: the arrow. While the system pointer is the worker's cursor (`hardware`), its picture
/// under `picture`, or the arrow until the worker has sent one. Otherwise none: the view draws
/// the pointer on the picture in the worker's cursor (or an arrow, or nothing when the
/// worker's is off the target).
const fn pointer_style(
    showing: bool,
    hardware: bool,
    picture: Option<CursorImageId>,
) -> CursorStyle {
    match (showing, hardware, picture) {
        (true, true, Some(id)) => CursorStyle::Image(id),
        (true, false, _) => CursorStyle::None,
        (false, ..) | (true, true, None) => CursorStyle::Arrow,
    }
}

/// The worker's cursor picture as the system pointer's: its pixels, hotspot and scale as the
/// worker read them, so it shows at the size in points it has on the worker's display, with the
/// hotspot on the same point. `None` for a picture whose bytes do not fill it.
fn system_picture(shape: &CursorShape) -> Option<CursorImage> {
    CursorImage::new(
        shape.bgra.clone(),
        size(DevicePixels(i32::from(shape.w)), DevicePixels(i32::from(shape.h))),
        point(DevicePixels(i32::from(shape.hot_x)), DevicePixels(i32::from(shape.hot_y))),
        f32::from(shape.scale.max(1)),
    )
    .ok()
}

/// A name for a view's system pointer picture, unique in the process.
fn next_pointer_id() -> CursorImageId {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    CursorImageId(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
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
            self.follow_taking();
            cx.notify();
        }
    }

    /// Committed text: what this device's text system made of the keys (a character, a dead
    /// key's or an input method's result, dictation) goes as text, which the worker types
    /// whatever its own layout.
    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = None;
        self.commit_text(text, cx);
        self.follow_taking();
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
        self.follow_taking();
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // The worker's caret, once it has said where it is; till then the pointer stands in
        // for it: candidate windows hang there.
        self.caret_bounds().or_else(|| {
            let (dx, dy) = self.cursor_offset(cx.background_executor().now());
            let origin = point(self.bounds.origin.x + dx, self.bounds.origin.y + dy);
            Some(Bounds::new(origin, size(px(1.0), px(16.0))))
        })
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

/// Keys that only hold a modifier: let go of after the others.
const fn is_modifier(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::ShiftLeft
            | KeyCode::ShiftRight
            | KeyCode::ControlLeft
            | KeyCode::ControlRight
            | KeyCode::AltLeft
            | KeyCode::AltRight
            | KeyCode::MetaLeft
            | KeyCode::MetaRight
            | KeyCode::Fn
    )
}

/// The key context a remote view adds while it sends system shortcuts to its worker: the app
/// binds ⌘Q, ⌘H and ⌘⌥H outside it ([`crate::keymap::app_chords`]), so there they quit or hide
/// the remote app.
pub const SYSTEM_KEYS_CTX: &str = "SystemKeys";

/// ⌘V (and ⇧⌘V, "paste and match style"): the chords that make the worker read its pasteboard,
/// by the character this device's layout typed (a Dvorak ⌘V is the key a US layout calls ".").
fn is_paste_chord(keystroke: &Keystroke) -> bool {
    let m = keystroke.modifiers;
    m.platform && !m.control && !m.alt && keystroke.key == "v"
}

#[cfg(test)]
mod tests {
    use gpui::AppContext as _;
    use slopty_client::pacing::{ClockAnchor, ClockEstimate};
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
            clock: Some(ClockEstimate {
                anchor: ClockAnchor { at: Instant::now(), host_us: 0 },
                bound: Duration::from_micros(280),
                rtt: Duration::from_micros(560),
                drift_ppm: 0,
            }),
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
            seams: Some((1190, 3)),
            target_fps: 60,
            paint: PaintRate { painted: 60, missed: 0 },
            mbps: 18.25,
            rtt: Some(Duration::from_micros(9_400)),
            frame_age: Some(Duration::from_millis(12)),
            rate: Some((19_200_000, RateVerdict::Stall, true)),
            stats: &stats,
            pacing: &pacing,
            capture: &Spread {
                p50: Duration::from_micros(31_200),
                p95: Duration::from_millis(38),
                max: Duration::from_micros(52_100),
                count: 240,
            },
            ui: None,
        });
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines,
            vec![
                "1920×1080 @1.00  ·  4:4:4  ·  age 12 ms  ·  stripes 1190 together 3 apart",
                "jitter 1.2 ms  ·  hold 2.0 / 9.0 ms  ·  queue 1  ·  fec 3 lost 1 nack 4 refresh 1  ·  stalls 2 (140 ms) flowing  ·  target 19.2 Mb/s hold (stall) (cwnd)",
                "audio 50 lost 0 concealed 0  ·  dry 2  ·  trimmed 40 ms stretched 20 ms  ·  target 60 ms",
                "capture 31.2 / 38.0 / 52.1 ms ±0.3  ·  present 5.4 / 11.9 / 28.0 ms (decode 2.1)  ·  every 16.7 ms ±1.4  ·  shown 1204 skip 2 repeat 7 late 0",
                "ui –",
            ]
        );
        let blank = hud_lines(&HudInput {
            size: (0, 0),
            scale: 0.5,
            chroma: None,
            seams: None,
            target_fps: 60,
            paint: PaintRate::default(),
            mbps: 0.0,
            rtt: None,
            frame_age: None,
            rate: None,
            stats: &ScreenStats::default(),
            pacing: &PacingStats::default(),
            capture: &Spread::default(),
            ui: None,
        });
        assert!(
            blank.contains("chroma –")
                && blank.contains("age –")
                && blank.contains("target –")
                && blank.contains("capture –"),
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

    /// A settings change to the stream's ceiling or depth reaches a live stream as a
    /// `SetQuality` at the scale it holds; a chrome-only change asks nothing.
    #[gpui::test]
    fn new_stream_settings_are_asked_of_a_live_stream(cx: &mut gpui::TestAppContext) {
        let (view, mut rx) = view(cx);
        let mut theme = Theme::default();
        theme.behaviour.copy_on_select = true;
        view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        assert!(sent(&mut rx).is_empty(), "nothing about the stream changed");
        theme.behaviour.stream.max_bitrate_bps = 8_000_000;
        view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        let asked = sent(&mut rx);
        let [ScreenRequest::SetQuality { stream: StreamId(4), quality }] = asked.as_slice() else {
            panic!("{asked:?}");
        };
        assert_eq!(quality.bitrate_bps, 8_000_000);
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
        theme.behaviour.stream.sharp_text = true;
        view.update(cx, |v, cx| v.set_theme(theme, cx));
        assert!(!muted(cx), "the pill's toggle stands");
        assert!(!sent(&mut rx).is_empty(), "the depth asked");
    }

    /// The stream asks for the refresh of the screen it is drawn on, up to 120, whenever its
    /// view lands on a screen of another rate; a screen that does not say counts as 60 Hz.
    #[test]
    fn the_rate_follows_the_screen_up_to_the_ceiling() {
        assert_eq!(stream_fps(120), 120, "a ProMotion screen");
        assert_eq!(stream_fps(60), 60);
        assert_eq!(stream_fps(144), 120, "the ceiling holds");
        assert_eq!(stream_fps(0), 60, "a screen that does not say");
        let prefs = slopty_theme::StreamPrefs::default();
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
        view.update(cx, |v, _| v.on_screen(Some(4), 144));
        assert_eq!(fps(&sent(&mut rx)), [120], "no screen takes it past the ceiling");
        assert_eq!(view.read_with(cx, |v, _| v.fps()), 120);
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

    /// The system pointer is the arrow before the first frame; over a frame it is the worker's
    /// picture while this client drives the pointer (the arrow until a picture came), and
    /// hidden while the view draws the worker's.
    #[test]
    fn the_system_pointer_is_the_workers_picture_while_this_client_drives_it() {
        let id = CursorImageId(9);
        assert_eq!(pointer_style(false, true, Some(id)), CursorStyle::Arrow);
        assert_eq!(pointer_style(true, true, Some(id)), CursorStyle::Image(id));
        assert_eq!(pointer_style(true, true, None), CursorStyle::Arrow);
        assert_eq!(pointer_style(true, false, Some(id)), CursorStyle::None);
    }

    /// A change of health notifies the view, which is what has the workspace take its facts
    /// (and the header's word) again; a reading that changes nothing draws nothing.
    #[gpui::test]
    fn a_change_of_health_notifies_the_view(cx: &mut gpui::TestAppContext) {
        let (view, _rx, cx) = windowed(cx);
        let notified = Rc::new(std::cell::Cell::new(0_u32));
        cx.update(|_, cx| {
            let notified = Rc::clone(&notified);
            cx.observe(&view, move |_, _| notified.set(notified.get().saturating_add(1))).detach();
        });
        view.update(cx, ScreenView::read_health);
        cx.run_until_parked();
        assert_eq!(notified.get(), 0, "all well, and still: nothing to tell");

        let long_ago = Instant::now().checked_sub(CUT_FOR_TEST).expect("a clock past boot");
        view.update(cx, |v, cx| {
            v.probe.verdict(RateVerdict::Cut, long_ago);
            v.read_health(cx);
        });
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.header().health), Some(Health::LowBandwidth));
        assert_eq!(notified.get(), 1, "the header's word changed: the view notified");
    }

    /// The overlay's figures are sampled by the once-a-second reading, never by a render: a
    /// render drawn a period after the last sample shows that sample, as a frame drawn from
    /// scratch a moment later does, and the reading brings the new figures.
    #[gpui::test]
    fn the_overlay_is_sampled_by_its_timer_not_by_a_render(cx: &mut gpui::TestAppContext) {
        let (view, _rx, cx) = windowed(cx);
        view.update(cx, |v, cx| v.set_hud(true, cx));
        cx.run_until_parked();
        let long_ago = Instant::now().checked_sub(CUT_FOR_TEST).expect("a clock past boot");
        view.update(cx, |v, cx| {
            if let Some(hud) = v.hud.as_mut() {
                hud.sampled_at = long_ago;
            }
            cx.notify();
        });
        cx.run_until_parked();
        let figures = |cx: &mut gpui::VisualTestContext| {
            view.read_with(cx, |v, _| v.hud_text().map(|(summary, _)| summary.len()))
        };
        assert_eq!(figures(cx), Some(0), "a render a period on samples nothing");
        view.update(cx, ScreenView::read_health);
        assert_eq!(figures(cx), Some(4), "the reading samples: rate, glass, bitrate, round trip");
    }

    /// Each reading adds to the overlay's half-minute trends, the oldest going past thirty;
    /// "Details" shows them over the engineering lines, each said with its latest figure.
    #[gpui::test]
    fn the_details_show_the_last_half_minute_of_age_jitter_and_round_trip(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, _rx, cx) = windowed(cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        view.update(cx, |v, cx| {
            v.set_hud(true, cx);
            v.set_rtt(None, Some(Duration::from_millis(7)), cx);
        });
        let long_ago = Instant::now().checked_sub(CUT_FOR_TEST).expect("a clock past boot");
        for _ in 0..HUD_HISTORY.saturating_add(2) {
            view.update(cx, |v, cx| {
                if let Some(hud) = v.hud.as_mut() {
                    hud.sampled_at = long_ago;
                }
                ScreenView::read_health(v, cx);
            });
        }
        let history = view.read_with(cx, |v, _| v.hud_history()).expect("the overlay");
        assert_eq!((history.rtt.len(), history.pushed), (HUD_HISTORY, 32), "half a minute");
        assert_eq!(history.rtt.back().copied(), Some(7.0));
        assert!(history.age.back().is_some_and(|v| v.is_nan()), "no frame yet: a gap");
        cx.run_until_parked();
        assert!(cx.debug_bounds("stream-stats-trends").is_none(), "behind Details");
        view.update(cx, ScreenView::toggle_hud_details);
        cx.run_until_parked();
        assert!(cx.debug_bounds("stream-trend-rtt").is_some());
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Image", Some("Round trip 7 ms"))), "{tree:#?}");
        assert!(tree.iter().any(|n| n.is("Image", Some("Frame age \u{2013}"))), "{tree:#?}");
    }

    /// Longer than the cut the header waits for ([`health::CUT_FOR`]).
    const CUT_FOR_TEST: Duration = Duration::from_secs(4);

    /// A worker on a Retina display reads its cursor at 2×: 64 pixels a side, hotspot at pixel
    /// (11, 12). The system pointer shows it 32 points a side with the hotspot at (5.5, 6)
    /// points, the size and spot it has on the worker's screen, on a client display of any
    /// scale; the same picture read on a 1× display shows at its pixels as points. A picture
    /// short of bytes, or with the hotspot off it, is no system pointer.
    #[test]
    fn the_system_pointer_takes_the_workers_scale_so_the_hotspot_stays_on_its_point() {
        let at_2x = CursorShape {
            w: 64,
            h: 64,
            hot_x: 11,
            hot_y: 12,
            bgra: vec![7; 64 * 64 * 4],
            scale: 2,
        };
        let image = system_picture(&at_2x).expect("a picture");
        assert_eq!(image.size_in_points(), size(32.0, 32.0));
        assert_eq!(image.hotspot_in_points(), point(5.5, 6.0));
        assert_eq!(image.bgra(), at_2x.bgra.as_slice(), "the bytes as read, BGRA premultiplied");

        let at_1x =
            CursorShape { w: 32, h: 32, hot_x: 5, hot_y: 6, bgra: vec![7; 32 * 32 * 4], scale: 1 };
        let image = system_picture(&at_1x).expect("a picture");
        assert_eq!(image.size_in_points(), size(32.0, 32.0));
        assert_eq!(image.hotspot_in_points(), point(5.0, 6.0));

        let off = CursorShape { hot_x: 64, ..at_2x.clone() };
        let short = CursorShape { bgra: vec![7; 64 * 64 * 4 - 1], ..at_2x };
        assert!(system_picture(&short).is_none(), "a byte short");
        assert!(system_picture(&off).is_none(), "the hotspot off the right edge");
    }

    /// A modifier pressed on its own reaches the worker as that key: each one that moves is a
    /// press or a release carrying the new state, an unchanged state sends nothing, and the
    /// fn key is never forwarded as a key, only as the state the others carry.
    #[gpui::test]
    fn modifier_keys_go_to_the_worker_as_they_move(cx: &mut gpui::TestAppContext) {
        let (view, mut rx) = view(cx);
        let key = |req: &ScreenRequest| match req {
            ScreenRequest::Input { input: ScreenInput::Key { code, action, mods }, .. } => {
                Some((*code, *action, *mods))
            }
            _ => None,
        };
        let shift = Modifiers { shift: true, ..Modifiers::default() };
        view.update(cx, |v, _| v.modifiers_moved(shift, false));
        assert_eq!(
            sent(&mut rx).iter().filter_map(key).collect::<Vec<_>>(),
            vec![(KeyCode::ShiftLeft, KeyAction::Press, Mods::SHIFT)]
        );
        view.update(cx, |v, _| v.modifiers_moved(shift, false));
        assert!(sent(&mut rx).is_empty(), "nothing moved");
        let both =
            Modifiers { shift: true, platform: true, function: true, ..Modifiers::default() };
        view.update(cx, |v, _| v.modifiers_moved(both, false));
        assert_eq!(
            sent(&mut rx).iter().filter_map(key).collect::<Vec<_>>(),
            vec![(KeyCode::MetaLeft, KeyAction::Press, Mods::SHIFT | Mods::SUPER | Mods::FN)],
            "⌘ joins ⇧; fn rides on it"
        );
        view.update(cx, |v, _| v.modifiers_moved(Modifiers::default(), false));
        assert_eq!(
            sent(&mut rx).iter().filter_map(key).collect::<Vec<_>>(),
            vec![
                (KeyCode::ShiftLeft, KeyAction::Release, Mods::empty()),
                (KeyCode::MetaLeft, KeyAction::Release, Mods::empty()),
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
            v.modifiers_moved(cmd, false);
            let a = Keystroke { modifiers: cmd, key: "a".into(), key_char: None };
            v.key_pressed(&a, false, cx);
            v.key_pressed(&a, true, cx);
        });
        assert_eq!(
            keys(sent(&mut rx)),
            vec![
                (KeyCode::MetaLeft, KeyAction::Press),
                (KeyCode::A, KeyAction::Press),
                (KeyCode::A, KeyAction::Repeat),
            ]
        );
        view.update(cx, |v, cx| v.let_go(cx));
        assert_eq!(
            keys(sent(&mut rx)),
            vec![(KeyCode::A, KeyAction::Release), (KeyCode::MetaLeft, KeyAction::Release)],
            "the key, then the modifier, let go"
        );
        view.update(cx, |v, cx| v.let_go(cx));
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
        view.update(cx, |v, cx| {
            v.set_geometry(100, 50, cx);
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
        let keys: Vec<(KeyAction, Mods)> = sent(&mut rx)
            .into_iter()
            .filter_map(|req| match req {
                ScreenRequest::Input { input: ScreenInput::Key { action, mods, .. }, .. } => {
                    Some((action, mods))
                }
                _ => None,
            })
            .collect();
        assert_eq!(keys, [(KeyAction::Press, Mods::SUPER), (KeyAction::Release, Mods::SUPER)]);
        // A plain key is its place; a character with none is typed as text.
        let typed = Keystroke { key_char: Some("b".to_owned()), ..chord("b") };
        view.update(cx, |v, cx| v.press(typed, cx));
        let plain = sent(&mut rx);
        assert!(
            matches!(plain.as_slice(), [ScreenRequest::Input { input: ScreenInput::Key { code: KeyCode::B, mods, .. }, .. }, _] if mods.is_empty()),
            "a plain key goes by its place: {plain:?}"
        );
        let ch = Keystroke { key_char: Some("ч".to_owned()), ..chord("ч") };
        view.update(cx, |v, cx| v.press(ch, cx));
        assert_eq!(
            sent(&mut rx),
            [ScreenRequest::Input {
                stream: StreamId(4),
                input: ScreenInput::Text { text: "ч".to_owned() }
            }]
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
                    let offer = Offer {
                        origin,
                        generation: 1,
                        age_ms: 0,
                        concealed: false,
                        items: Vec::new(),
                    };
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
                        input:
                            ScreenInput::Key { code, action: KeyAction::Press, .. }
                            | ScreenInput::PasteChord { code, .. },
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
        windowed_sized(cx, target, (800, 600))
    }

    /// [`windowed_on`], with a stream of `pixels` in the 400 × 300 body.
    fn windowed_sized(
        cx: &mut gpui::TestAppContext,
        target: CaptureTarget,
        pixels: (u32, u32),
    ) -> (gpui::Entity<ScreenView>, mpsc::Receiver<ClientMsg>, &mut gpui::VisualTestContext) {
        let (out, rx) = mpsc::channel(64);
        let opened = Opened {
            stream: StreamId(4),
            target,
            size: pixels,
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

    /// The worker saying its Mac is locked puts a status over the body that says so, above the
    /// picture's place, and stops the refresh requests; another session on the Mac's screens
    /// says that instead, and the notice goes when the session is back.
    #[gpui::test]
    fn a_locked_mac_is_said_over_the_picture(cx: &mut gpui::TestAppContext) {
        let (view, _rx, cx) = windowed(cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        let statuses = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, _cx| {
                crate::a11y::tree(window)
                    .into_iter()
                    .filter(|n| n.role == "Status")
                    .filter_map(|n| n.label)
                    .collect::<Vec<_>>()
            })
        };
        assert!(cx.debug_bounds("screen-console").is_none(), "nothing to say while live");
        view.update(cx, |v, cx| v.set_source_state(SourceState::Locked, cx));
        cx.run_until_parked();
        let over = cx.debug_bounds("screen-console").expect("the notice is up");
        assert_eq!(over.size, size(px(400.0), px(300.0)), "over the whole body");
        assert!(statuses(cx).contains(&"The Mac is locked".to_owned()), "{:?}", statuses(cx));
        assert!(!view.read_with(cx, |v, _| v.handle.source_live()), "no refreshes asked");
        view.update(cx, |v, cx| v.set_source_state(SourceState::Away, cx));
        cx.run_until_parked();
        let said = statuses(cx);
        assert!(said.contains(&"The Mac is at the login window".to_owned()), "{said:?}");
        view.update(cx, |v, cx| v.set_source_state(SourceState::Live, cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("screen-console").is_none(), "gone with the session back");
        assert!(view.read_with(cx, |v, _| v.handle.source_live()));
    }

    /// The notice's words are sentence case, say what is so and when it ends, and never
    /// ask for anything the client cannot do.
    #[test]
    fn the_console_notice_says_what_is_so() {
        assert_eq!(console_notice(SourceState::Live), ("", ""));
        assert_eq!(console_notice(SourceState::Idle), ("", ""));
        for state in [SourceState::Locked, SourceState::Away] {
            let (title, detail) = console_notice(state);
            assert!(title.starts_with("The Mac is"), "{title}");
            assert!(detail.ends_with('.'), "{detail}");
            assert_eq!(waiting_text(state), title);
        }
        // Only a locked Mac can be unlocked from here, so only its notice speaks of it.
        assert!(console_notice(SourceState::Locked).1.contains("unlocked, here"));
        assert!(!console_notice(SourceState::Away).1.to_lowercase().contains("unlock"));
    }

    /// "Unlock here" on a locked Mac's notice lifts the scrim to a line saying what to type and
    /// gives the stream the keyboard, so the keys reach the lock screen; the login window offers
    /// nothing of the kind, and the Mac back on its session takes the line away.
    #[gpui::test]
    fn unlock_here_hands_the_keys_to_the_lock_screen(cx: &mut gpui::TestAppContext) {
        let (view, mut rx, cx) = windowed(cx);
        view.update(cx, |v, cx| v.set_source_state(SourceState::Away, cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("screen-unlock-here").is_none(), "not at the login window");
        view.update(cx, |v, cx| v.set_source_state(SourceState::Locked, cx));
        cx.run_until_parked();
        let unlock = cx.debug_bounds("screen-unlock-here").expect("offered on a locked Mac");
        cx.update(Window::blur);
        cx.simulate_click(unlock.center(), Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("screen-console").is_none(), "the scrim lifted");
        assert!(cx.debug_bounds("screen-unlocking").is_some(), "the line says what to type");
        assert!(view.read_with(cx, |v, _| v.unlocking()));
        let focused = cx.update(|window, cx| view.read(cx).focus.is_focused(window));
        assert!(focused, "the stream has the keyboard");
        let _before = sent(&mut rx);
        cx.simulate_keystrokes("a");
        let keys = sent(&mut rx)
            .into_iter()
            .filter(|r| {
                matches!(
                    r,
                    ScreenRequest::Input {
                        input: ScreenInput::Key { .. } | ScreenInput::Text { .. },
                        ..
                    }
                )
            })
            .count();
        assert!(keys > 0, "a typed key goes to the worker");
        view.update(cx, |v, cx| v.set_source_state(SourceState::Live, cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("screen-unlocking").is_none(), "unlocked: the line goes");
        assert!(!view.read_with(cx, |v, _| v.unlocking()));
    }

    /// The `Focused` requests the worker was sent for the test views' stream.
    fn told_focus(rx: &mut mpsc::Receiver<ClientMsg>) -> Vec<bool> {
        sent(rx)
            .into_iter()
            .filter_map(|req| match req {
                ScreenRequest::Focused { stream: StreamId(4), focused } => Some(focused),
                _ => None,
            })
            .collect()
    }

    /// The worker hears whether the tile has the keyboard in an active window, once per change:
    /// a focused tile in a window that is not active is not focused, and becomes so when the
    /// window does; losing the keyboard and taking it back, and the window going inactive and
    /// active again, each say so once. (The test platform opens windows inactive; macOS makes a
    /// window opened with `focus` key, and the same activation reaches the view.)
    #[gpui::test]
    fn the_worker_hears_the_tiles_focus_once_per_change(cx: &mut gpui::TestAppContext) {
        let (view, mut rx, cx) = windowed(cx);
        assert_eq!(told_focus(&mut rx), [false; 0], "the window is not active");
        cx.update(|window, _cx| window.activate_window());
        cx.run_until_parked();
        assert_eq!(told_focus(&mut rx), [true], "the window became active");
        cx.update(Window::blur);
        cx.run_until_parked();
        cx.update(Window::blur);
        cx.run_until_parked();
        assert_eq!(told_focus(&mut rx), [false], "said once");
        view.update_in(cx, |v, window, cx| window.focus(&v.focus, cx));
        cx.run_until_parked();
        assert_eq!(told_focus(&mut rx), [true]);
        cx.deactivate_window();
        assert_eq!(told_focus(&mut rx), [false], "the window went inactive");
        cx.update(|window, _cx| window.activate_window());
        cx.run_until_parked();
        assert_eq!(told_focus(&mut rx), [true], "and active again, still focused");
    }

    /// A tile first drawn with the keyboard in a window that is already active, as one a
    /// reconnect opens or one added to the workspace, is told focused by its first render: no
    /// focus or activation event comes for it.
    #[gpui::test]
    fn a_tile_drawn_focused_in_an_active_window_is_focused_at_once(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        cx.update(|window, _cx| window.activate_window());
        cx.run_until_parked();
        let (out, mut rx) = mpsc::channel(64);
        let opened = Opened {
            stream: StreamId(4),
            target: CaptureTarget::Display(DisplayId(2)),
            size: (800, 600),
            quality: Quality { scale: 1.0, ..Quality::default() },
        };
        cx.update(|window, cx| {
            window.replace_root(cx, |window, cx| {
                let view = ScreenView::new(
                    opened,
                    ScreenHandle::detached(StreamId(4)),
                    out,
                    Theme::default(),
                    cx,
                );
                window.focus(&view.focus, cx);
                view
            })
        });
        cx.run_until_parked();
        assert_eq!(told_focus(&mut rx), [true]);
    }

    /// A fling reaches the worker shaped the way macOS shapes one: the gesture begins, changes and
    /// ends, and the momentum after it begins, continues and ends in phases of its own, which
    /// gpui reports as `momentum_phase`. A scroll without one is never momentum, whenever it
    /// comes: a smooth-scrolling mouse right after a swipe is the mouse.
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
        let wheel = |cx: &mut gpui::VisualTestContext, dy: f32, touch_phase, momentum_phase| {
            cx.simulate_event(ScrollWheelEvent {
                position: at,
                delta: ScrollDelta::Pixels(point(px(0.0), px(dy))),
                modifiers: Modifiers::default(),
                touch_phase,
                momentum_phase,
            });
            cx.run_until_parked();
        };
        let fingers = |cx: &mut gpui::VisualTestContext, dy: f32, touch| wheel(cx, dy, touch, None);
        let coast = |cx: &mut gpui::VisualTestContext, dy: f32, momentum| {
            wheel(cx, dy, TouchPhase::Moved, Some(momentum));
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

        fingers(cx, -8.0, TouchPhase::Started);
        fingers(cx, -30.0, TouchPhase::Moved);
        fingers(cx, -30.0, TouchPhase::Ended);
        coast(cx, -20.0, TouchPhase::Started);
        coast(cx, -12.0, TouchPhase::Moved);
        coast(cx, 0.0, TouchPhase::Ended);
        assert_eq!(
            phases(&mut rx),
            [
                (Began, Off),
                (Changed, Off),
                (Ended, Off),
                (Off, Began),
                (Off, Changed),
                (Off, Ended)
            ],
            "the fingers' part of the fling, then its coast"
        );
        cx.executor().advance_clock(PAST_THE_GAP);
        cx.run_until_parked();
        assert!(inputs(&mut rx).is_empty(), "the fling is closed once");

        // A mouse that scrolls in pixels and reports no phases, right after a swipe that left
        // no momentum, is the mouse: it neither opens a coast nor has one closed behind it.
        fingers(cx, -8.0, TouchPhase::Started);
        fingers(cx, -8.0, TouchPhase::Ended);
        fingers(cx, -6.0, TouchPhase::Moved);
        fingers(cx, -6.0, TouchPhase::Moved);
        assert_eq!(
            phases(&mut rx),
            [(Began, Off), (Ended, Off), (Changed, Off), (Changed, Off)],
            "no momentum said, none sent"
        );
        cx.executor().advance_clock(PAST_THE_GAP);
        cx.run_until_parked();
        assert!(inputs(&mut rx).is_empty(), "nothing coasting, nothing to close");

        // If the coast's own close is lost, the silence closes it, with the zero-delta end the
        // worker needs to let the gesture go.
        fingers(cx, -8.0, TouchPhase::Started);
        fingers(cx, -30.0, TouchPhase::Ended);
        coast(cx, -20.0, TouchPhase::Started);
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
        // `Ended` as its touch phase too. That closes the coast; the gesture does not end twice.
        fingers(cx, -8.0, TouchPhase::Started);
        fingers(cx, -30.0, TouchPhase::Ended);
        coast(cx, -20.0, TouchPhase::Started);
        wheel(cx, -4.0, TouchPhase::Ended, Some(TouchPhase::Ended));
        assert_eq!(
            phases(&mut rx),
            [(Began, Off), (Ended, Off), (Off, Began), (Off, Ended)],
            "the coast closes itself on iOS"
        );

        // Fingers that rest on the trackpad before they push open the gesture twice, because
        // gpui reads `MayBegin` and `Began` as the same phase. The worker is told once.
        fingers(cx, 0.0, TouchPhase::Started);
        fingers(cx, 0.0, TouchPhase::Started);
        fingers(cx, -30.0, TouchPhase::Moved);
        fingers(cx, -30.0, TouchPhase::Ended);
        assert_eq!(
            phases(&mut rx),
            [(Began, Off), (Changed, Off), (Changed, Off), (Ended, Off)],
            "resting fingers open the gesture once"
        );
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

    /// A screen an agent drives is watched: no move, press, key or scroll leaves this device,
    /// nor does a press raise the window, and its pill says who drives. Taking control stops the
    /// agent's turn once and the person's input goes from then; handing back watches again.
    #[gpui::test]
    fn an_agents_screen_is_watched_until_the_person_takes_control(cx: &mut gpui::TestAppContext) {
        let (view, mut rx, cx) = windowed_on(cx, CaptureTarget::Window(slopty_core::WindowId(9)));
        drop(sent(&mut rx));
        let stops = Rc::new(std::cell::Cell::new(0_u32));
        let counted = Rc::clone(&stops);
        let driver = Driver {
            agent: slopty_proto::thread::AgentId(
                slopty_proto::thread::AgentId::CLAUDE_CODE.to_owned(),
            ),
            name: "Claude Code".to_owned(),
            working: true,
            take: Rc::new(move |_cx: &mut App| counted.set(counted.get().saturating_add(1))),
        };
        view.update(cx, |v, cx| v.set_driver(Some(driver.clone()), cx));
        cx.run_until_parked();
        let bounds = view.read_with(cx, |v, _| v.bounds);
        let middle = bounds.center();
        let poke = |cx: &mut gpui::VisualTestContext| {
            cx.simulate_mouse_move(middle, None, Modifiers::default());
            cx.simulate_mouse_down(middle, MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_up(middle, MouseButton::Left, Modifiers::default());
            cx.simulate_keystrokes("a");
            cx.run_until_parked();
        };
        poke(cx);
        let watched = sent(&mut rx);
        assert!(
            !watched
                .iter()
                .any(|r| matches!(r, ScreenRequest::Input { .. } | ScreenRequest::Focus(_))),
            "{watched:?}"
        );
        assert!(cx.debug_bounds("screen-driver").is_some(), "the pill says who drives");

        let act = cx.debug_bounds("screen-driver-act").expect("the pill's button").center();
        cx.simulate_click(act, Modifiers::default());
        cx.run_until_parked();
        assert_eq!(stops.get(), 1, "the agent's turn was stopped once");
        assert!(view.read_with(cx, |v, _| !v.watching()));
        drop(sent(&mut rx));
        poke(cx);
        let taken = inputs(&mut rx);
        assert!(
            taken.iter().any(|i| matches!(i, ScreenInput::Button { down: true, .. })),
            "{taken:?}"
        );

        view.update(cx, |v, cx| v.set_driver(Some(driver), cx));
        assert!(view.read_with(cx, |v, _| !v.watching()), "the same driver keeps who has control");
        let back = cx.debug_bounds("screen-driver-act").expect("the pill's button").center();
        cx.simulate_click(back, Modifiers::default());
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.watching()), "handed back");
        assert_eq!(stops.get(), 1, "handing back stops nothing");
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
            momentum_phase: None,
        });
        cx.simulate_event(ScrollWheelEvent {
            position: at(0.5, 0.5),
            delta: ScrollDelta::Lines(point(0.0, 2.0)),
            modifiers: Modifiers { alt: true, ..Modifiers::default() },
            touch_phase: TouchPhase::Started,
            momentum_phase: None,
        });
        cx.simulate_event(ScrollWheelEvent {
            position: at(0.5, 0.5),
            delta: ScrollDelta::Lines(point(0.0, 1.0)),
            modifiers: Modifiers { platform: true, ..Modifiers::default() },
            touch_phase: TouchPhase::Moved,
            momentum_phase: None,
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
            ScreenInput::Scroll { dx, dy, precise: true, phase, momentum, x, y, mods, .. } => {
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
        assert_eq!(inputs(&mut rx), Vec::<ScreenInput>::new());
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
        view.update(cx, |v, cx| v.let_go(cx));
        let got = inputs(&mut rx);
        assert_eq!(released(&got), [(ProtoButton::Right, 400.0, 300.0)], "{got:?}");
        view.update(cx, |v, cx| v.let_go(cx));
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
        let key = |action| ScreenInput::Key { code: KeyCode::A, action, mods: Mods::SUPER };
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

    /// While this Mac composes for a remote field, the candidate window hangs under the
    /// worker's caret once the worker says where it is, scaled as the pointer is; with no field
    /// (or one that gives no caret) the pointer stands in. A password field is told apart.
    #[gpui::test]
    fn the_input_method_hangs_its_candidates_under_the_workers_caret(
        cx: &mut gpui::TestAppContext,
    ) {
        use slopty_proto::screen::Caret;
        let (view, _rx, cx) = windowed(cx);
        let caret = |view: &gpui::Entity<ScreenView>, cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| {
                view.update(cx, |v, cx| v.bounds_for_range(0..0, Bounds::default(), window, cx))
            })
        };
        let bounds = view.read_with(cx, |v, _| v.bounds);
        view.update(cx, |v, _| v.cursor = CursorState { x: 400, y: 150, visible: true });
        let pointer = caret(&view, cx).map(|b| b.origin);
        let field = |caret, secure| Some(TextField { caret, secure });
        let at = Caret { x: 200.0, y: 300.0, width: 0.0, height: 30.0 };
        view.update(cx, |v, cx| v.set_field(field(Some(at), false), cx));
        let got = caret(&view, cx).expect("a caret");
        let quarter_half = point(
            bounds.origin.x + bounds.size.width * 0.25,
            bounds.origin.y + bounds.size.height * 0.5,
        );
        assert_eq!(got.origin, quarter_half, "at the caret, a quarter across and half down");
        assert_eq!(got.size.height, bounds.size.height * (30.0 / 600.0), "the line's height");
        assert!(!view.read_with(cx, |v, _| v.in_password_field()));
        view.update(cx, |v, cx| v.set_field(field(None, true), cx));
        assert_eq!(caret(&view, cx).map(|b| b.origin), pointer, "no caret: the pointer");
        assert!(view.read_with(cx, |v, _| v.in_password_field()));
        view.update(cx, |v, cx| v.set_field(None, cx));
        assert!(!view.read_with(cx, |v, _| v.in_password_field()));
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

    /// A picture of another aspect than its body keeps its own, centred with the body bare above
    /// and below, and the pointer maps through the picture as drawn: a point of the picture is
    /// its stream pixel, a move or a press on the bare body sends nothing, a release there lands
    /// on the picture's edge, the worker's pointer is drawn on the picture, and zoomed the
    /// picture is drawn from the same frame.
    #[gpui::test]
    fn a_picture_of_another_aspect_is_letterboxed_and_the_pointer_follows(
        cx: &mut gpui::TestAppContext,
    ) {
        // 2:1 in a 4:3 body: 400 × 200 points at (0, 50).
        let (view, mut rx, cx) =
            windowed_sized(cx, CaptureTarget::Display(DisplayId(2)), (800, 400));
        drop(sent(&mut rx));
        let b = view.read_with(cx, |v, _| v.bounds);
        assert_eq!(b.size, size(px(400.0), px(300.0)));
        let frame = view.read_with(cx, |v, _| v.frame());
        assert_eq!(frame, zoom::Frame { origin: (0.0, 50.0), size: (400.0, 200.0) });
        let at = |x: f32, y: f32| point(b.origin.x + px(x), b.origin.y + px(y));

        cx.simulate_mouse_move(at(100.0, 150.0), None, Modifiers::default());
        cx.run_until_parked();
        match inputs(&mut rx).as_slice() {
            [ScreenInput::Move { x, y }] => near_px((*x, *y), (200.0, 200.0)),
            other => panic!("expected one move, got {other:?}"),
        }
        // The bars: nothing of the remote screen is there.
        cx.simulate_mouse_move(at(200.0, 20.0), None, Modifiers::default());
        cx.simulate_mouse_down(at(200.0, 290.0), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(at(200.0, 290.0), MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        assert!(inputs(&mut rx).is_empty(), "the bare body sends nothing");

        // Pressed on the picture, let go on the bar below it: at the picture's bottom edge.
        cx.simulate_mouse_down(at(300.0, 225.0), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(at(300.0, 290.0), MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        let got = presses(&inputs(&mut rx));
        assert_eq!(got.len(), 2, "{got:?}");
        near_px((got[0].2, got[0].3), (600.0, 350.0));
        near_px((got[1].2, got[1].3), (600.0, 400.0));

        // The worker's pointer at its pixel is drawn over that pixel of the picture.
        cx.executor().advance_clock(LOCAL_HOLD);
        view.update(cx, |v, _| v.cursor = CursorState { x: 200, y: 100, visible: true });
        near_px(offset(&view, cx), (100.0, 100.0));

        // Zoomed twice about the picture's middle: the middle stays, the rest of the frame
        // shows the picture's middle half, and the bars show more of it.
        view.update(cx, |v, cx| {
            v.zoom = Zoom::FIT.about((0.5, 0.5), 2.0, 8.0);
            cx.notify();
        });
        cx.run_until_parked();
        cx.simulate_mouse_move(at(200.0, 150.0), None, Modifiers::default());
        cx.simulate_mouse_move(at(0.5, 50.0), None, Modifiers::default());
        cx.simulate_mouse_move(at(200.0, 20.0), None, Modifiers::default());
        cx.run_until_parked();
        let moves: Vec<(f32, f32)> = inputs(&mut rx)
            .iter()
            .filter_map(|i| match i {
                ScreenInput::Move { x, y } => Some((*x, *y)),
                _ => None,
            })
            .collect();
        assert_eq!(moves.len(), 3, "{moves:?}");
        near_px(moves[0], (400.0, 200.0));
        near_px(moves[1], (200.5, 100.0));
        near_px(moves[2], (400.0, 70.0));
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

    /// With the picture's gestures sent on, a trackpad pinch that begins on the picture reaches
    /// the worker as the magnification AppKit read, in its phases, at the stream pixel under the
    /// fingers, followed to its end even off the picture, and the picture keeps its zoom; one
    /// that begins off the picture is not sent. Turned off, a pinch zooms the picture again. The
    /// worker is told of each turn, ahead of the input after it.
    #[gpui::test]
    fn a_pinch_goes_to_the_remote_app_when_its_gestures_are_sent(cx: &mut gpui::TestAppContext) {
        // A wide picture, drawn across the middle of the body with the body's page above it.
        let (view, mut rx, cx) =
            windowed_sized(cx, CaptureTarget::Display(DisplayId(2)), (800, 300));
        drop(sent(&mut rx));
        view.update(cx, |v, cx| v.set_remote_gestures(true, cx));
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
        let outside = at_fraction(&view, cx, 0.5, 0.1);
        pinch(cx, middle, 0.0, TouchPhase::Started);
        pinch(cx, middle, 0.25, TouchPhase::Moved);
        pinch(cx, outside, -0.1, TouchPhase::Moved);
        pinch(cx, outside, 0.0, TouchPhase::Ended);
        pinch(cx, outside, 0.0, TouchPhase::Started);
        let sent_on = inputs(&mut rx);
        assert_eq!(sent_on.first(), Some(&ScreenInput::Gestures { remote: true }), "{sent_on:?}");
        let magnified: Vec<(f32, ScrollPhase, f32, f32)> = sent_on
            .into_iter()
            .filter_map(|input| match input {
                ScreenInput::Magnify { delta, phase, x, y, .. } => Some((delta, phase, x, y)),
                _ => None,
            })
            .collect();
        assert_eq!(
            magnified.iter().map(|m| (m.0, m.1)).collect::<Vec<_>>(),
            [
                (0.0, ScrollPhase::Began),
                (0.25, ScrollPhase::Changed),
                (-0.1, ScrollPhase::Changed),
                (0.0, ScrollPhase::Ended),
            ],
            "the pinch that began off the picture is not sent"
        );
        assert!((magnified[0].2 - 400.0).abs() < 1.0, "the middle of 800 px: {magnified:?}");
        assert!(
            magnified[2..].iter().all(|m| m.3 == 0.0),
            "followed off the picture, at its nearest edge: {magnified:?}"
        );
        assert_eq!(view.read_with(cx, |v, _| v.zoom), Zoom::FIT, "the picture keeps its zoom");

        view.update(cx, ScreenView::toggle_remote_gestures);
        assert!(!view.read_with(cx, |v, _| v.remote_gestures()));
        pinch(cx, middle, 0.0, TouchPhase::Started);
        pinch(cx, middle, 0.5, TouchPhase::Moved);
        pinch(cx, middle, 0.0, TouchPhase::Ended);
        assert!(view.read_with(cx, |v, _| v.zoom) != Zoom::FIT, "zooms here again");
        let sent_off = inputs(&mut rx);
        assert_eq!(sent_off.first(), Some(&ScreenInput::Gestures { remote: false }));
        assert!(sent_off.iter().all(|i| !matches!(i, ScreenInput::Magnify { .. })));
    }

    /// A trackpad scroll's reports and a pinch's carry when the view read them, in microseconds
    /// of one clock, so the worker can post them at the fingers' spacing whatever the path
    /// does to them on the way.
    #[gpui::test]
    fn a_gesture_s_reports_carry_when_they_were_read(cx: &mut gpui::TestAppContext) {
        let (view, mut rx, cx) =
            windowed_sized(cx, CaptureTarget::Display(DisplayId(2)), (800, 300));
        drop(sent(&mut rx));
        let at = at_fraction(&view, cx, 0.5, 0.5);
        let steps = [Duration::ZERO, Duration::from_micros(8_333), Duration::from_millis(40)];
        for (k, step) in steps.iter().enumerate() {
            cx.executor().advance_clock(*step);
            cx.simulate_event(ScrollWheelEvent {
                position: at,
                delta: ScrollDelta::Pixels(point(px(-10.0), px(0.0))),
                modifiers: Modifiers::default(),
                touch_phase: if k == 0 { TouchPhase::Started } else { TouchPhase::Moved },
                momentum_phase: None,
            });
            cx.run_until_parked();
        }
        view.update(cx, |v, cx| v.set_remote_gestures(true, cx));
        for (step, phase) in
            [(Duration::ZERO, TouchPhase::Started), (Duration::from_millis(9), TouchPhase::Moved)]
        {
            cx.executor().advance_clock(step);
            cx.simulate_event(PinchEvent {
                position: at,
                delta: 0.1,
                modifiers: Modifiers::default(),
                phase,
            });
            cx.run_until_parked();
        }
        let times: Vec<u32> = inputs(&mut rx).iter().filter_map(ScreenInput::time_us).collect();
        assert_eq!(times.len(), 5, "{times:?}");
        let gaps: Vec<u32> = times.windows(2).map(|w| w[1].wrapping_sub(w[0])).collect();
        assert_eq!(gaps, [8_333, 40_000, 0, 9_000], "{times:?}");
    }

    /// Turning the gestures over halfway through a pinch leaves that pinch where it began: one
    /// zooming the picture keeps zooming it to its end, and one sent to the worker is sent to
    /// its end. The next pinch takes the new side.
    #[gpui::test]
    fn a_pinch_keeps_its_side_when_the_gestures_turn_over(cx: &mut gpui::TestAppContext) {
        let (view, mut rx, cx) =
            windowed_sized(cx, CaptureTarget::Display(DisplayId(2)), (800, 300));
        let middle = at_fraction(&view, cx, 0.5, 0.5);
        let pinch = |cx: &mut gpui::VisualTestContext, delta, phase| {
            cx.simulate_event(PinchEvent {
                position: middle,
                delta,
                modifiers: Modifiers::default(),
                phase,
            });
            cx.run_until_parked();
        };
        let magnified = |rx: &mut mpsc::Receiver<ClientMsg>| -> Vec<ScrollPhase> {
            inputs(rx)
                .into_iter()
                .filter_map(|input| match input {
                    ScreenInput::Magnify { phase, .. } => Some(phase),
                    _ => None,
                })
                .collect()
        };
        drop(sent(&mut rx));

        // A zoom here, turned on halfway: it zooms to its end and nothing is sent.
        pinch(cx, 0.0, TouchPhase::Started);
        pinch(cx, 0.5, TouchPhase::Moved);
        view.update(cx, |v, cx| v.set_remote_gestures(true, cx));
        pinch(cx, 0.5, TouchPhase::Moved);
        pinch(cx, 0.0, TouchPhase::Ended);
        let zoomed = view.read_with(cx, |v, _| v.zoom);
        assert!(zoomed != Zoom::FIT, "the zoom went on to its end");
        assert!(magnified(&mut rx).is_empty(), "no half a gesture on the worker");

        // Sent to the worker, turned off halfway: it is sent to its end, and the zoom stays.
        pinch(cx, 0.0, TouchPhase::Started);
        pinch(cx, 0.2, TouchPhase::Moved);
        view.update(cx, |v, cx| v.set_remote_gestures(false, cx));
        pinch(cx, 0.2, TouchPhase::Moved);
        pinch(cx, 0.0, TouchPhase::Ended);
        assert_eq!(
            magnified(&mut rx),
            [ScrollPhase::Began, ScrollPhase::Changed, ScrollPhase::Changed, ScrollPhase::Ended],
            "the worker's gesture has its end"
        );
        assert_eq!(view.read_with(cx, |v, _| v.zoom), zoomed, "not zoomed here meanwhile");

        // The next pinch is the picture's again.
        pinch(cx, 0.0, TouchPhase::Started);
        pinch(cx, 0.5, TouchPhase::Moved);
        pinch(cx, 0.0, TouchPhase::Ended);
        let zoomed = view.read_with(cx, |v, _| v.zoom);
        assert!(zoomed != Zoom::FIT, "zooms here");
        assert_eq!(magnified(&mut rx), Vec::<ScrollPhase>::new());

        // A pinch sent to the worker whose end never came does not keep the next one there
        // once the gestures are the picture's.
        view.update(cx, |v, cx| v.set_remote_gestures(true, cx));
        pinch(cx, 0.0, TouchPhase::Started);
        pinch(cx, 0.2, TouchPhase::Moved);
        view.update(cx, |v, cx| v.set_remote_gestures(false, cx));
        drop(magnified(&mut rx));
        // Out: the picture is at its largest already, where a pinch in changes nothing.
        pinch(cx, 0.0, TouchPhase::Started);
        pinch(cx, -0.3, TouchPhase::Moved);
        pinch(cx, 0.0, TouchPhase::Ended);
        assert!(magnified(&mut rx).is_empty(), "the new pinch is the picture's");
        assert!(view.read_with(cx, |v, _| v.zoom) != zoomed, "and zooms it");
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
        let (u, _) = zoom.to_frame(at);
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
        assert_eq!(scale(&mut rx), None, "not while the zoom may still move");
        cx.executor().advance_clock(QUALITY_COOLDOWN);
        cx.run_until_parked();
        assert_eq!(scale(&mut rx), Some(0.5), "drawn twice as wide, once it settled");
        let state = |cx: &mut gpui::VisualTestContext| view.read_with(cx, |v, _| v.readout);
        assert_eq!(state(cx), Some(Readout::Shown));
        assert!(
            view.read_with(cx, |v, _| v.readout())
                .is_some_and(|r| r.ends_with('%') && !r.ends_with(" %"))
        );
        cx.executor().advance_clock(READOUT_HOLD.saturating_sub(QUALITY_COOLDOWN));
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

    /// The region a zoom asks for, from the quality asks it sends.
    fn regions(rx: &mut mpsc::Receiver<ClientMsg>) -> Vec<(f32, Option<Region>)> {
        sent(rx)
            .into_iter()
            .filter_map(|r| match r {
                ScreenRequest::SetQuality { quality, .. } => Some((quality.scale, quality.region)),
                _ => None,
            })
            .collect()
    }

    /// A zoom asks for what it shows and a quarter of it on every side, at the scale it is drawn
    /// at, once it has held still for the cooldown, and not on the way there. A pan inside that
    /// margin asks for the region moved, at its size, once it settles; one that shows what the
    /// stream does not carry asks at once. Back at fit it asks for the whole target.
    #[gpui::test]
    fn a_zoom_asks_for_its_region_once_it_settles(cx: &mut gpui::TestAppContext) {
        let (view, mut rx, cx) = windowed(cx);
        view.update(cx, |v, cx| {
            v.native = (3200.0, 2400.0);
            v.quality_changed = past_cooldown();
            v.set_painted_width(800.0, cx);
        });
        assert_eq!(regions(&mut rx), [(0.25, None)], "fit: the whole target");
        let settle = |cx: &mut gpui::VisualTestContext| {
            cx.executor().advance_clock(QUALITY_COOLDOWN);
            cx.run_until_parked();
        };
        // Four times about the middle, in two steps: a quarter of each side in view.
        view.update(cx, |v, cx| {
            v.quality_changed = past_cooldown();
            v.set_zoom(Zoom::FIT.about((0.5, 0.5), 2.0, 8.0), cx);
            v.set_zoom(Zoom::FIT.about((0.5, 0.5), 4.0, 8.0), cx);
        });
        assert_eq!(regions(&mut rx), [], "the zoom may still move");
        settle(cx);
        let a = Region { x: 1000, y: 750, w: 1200, h: 900 };
        assert_eq!(regions(&mut rx), [(1.0, Some(a))], "native, the view and its margin");

        let pan = |cx: &mut gpui::VisualTestContext, by: f32| {
            view.update(cx, |v, cx| {
                v.quality_changed = past_cooldown();
                v.set_zoom(v.zoom.panned((by, 0.0), 8.0), cx);
            });
        };
        pan(cx, 0.05);
        assert_eq!(regions(&mut rx), [], "inside the margin");
        settle(cx);
        assert_eq!(regions(&mut rx), [(1.0, Some(Region { x: 960, ..a }))], "moved, one size");
        pan(cx, 0.5);
        assert_eq!(regions(&mut rx), [(1.0, Some(Region { x: 560, ..a }))], "past it: at once");

        view.update(cx, |v, cx| {
            v.quality_changed = past_cooldown();
            v.set_zoom(Zoom::FIT, cx);
        });
        settle(cx);
        assert_eq!(regions(&mut rx), [(0.25, None)], "fit again");
    }

    /// A picture of a region goes where that region is drawn in the zoomed whole, over the
    /// newest picture of the whole target, which GPUI draws under it; the stream's size is the
    /// whole target's throughout. A picture of the whole target goes back over all of it, with
    /// nothing under it.
    #[gpui::test]
    fn a_region_picture_goes_to_its_place_over_the_whole(cx: &mut gpui::TestAppContext) {
        let (view, _rx, cx) = windowed(cx);
        view.update(cx, |v, cx| v.show_picture(picture(800, 600), cx));
        cx.run_until_parked();
        let fit = rect(0., 0., 400., 300.);
        assert_eq!(layer(&view, cx), Some((fit, fit)));
        let base = |cx: &gpui::VisualTestContext| view.read_with(cx, |v, _| v.base_at.take());

        // Twice about the middle: the whole picture at (-200, -150), 800 × 600 points, and the
        // middle quarter of the target is the body.
        let region = Region { x: 200, y: 150, w: 400, h: 300 };
        view.update(cx, |v, cx| {
            v.zoom = Zoom::FIT.about((0.5, 0.5), 2.0, 8.0);
            v.show_region(picture(400, 300), region, cx);
        });
        cx.run_until_parked();
        assert_eq!(layer(&view, cx), Some((fit, fit)), "the region's place");
        assert_eq!(base(cx), Some(rect(-200., -150., 800., 600.)), "the whole under it");
        assert_eq!(view.read_with(cx, |v, _| v.size), (800, 600), "the stream's own size");

        // Panned a quarter of the frame right: the region's place moves with the picture.
        view.update(cx, |v, cx| {
            v.zoom = v.zoom.panned((0.25, 0.0), 8.0);
            cx.notify();
        });
        cx.run_until_parked();
        let moved = rect(100., 0., 400., 300.);
        assert_eq!(layer(&view, cx), Some((moved, rect(100., 0., 300., 300.))), "clipped");
        assert_eq!(base(cx), Some(rect(-100., -150., 800., 600.)));

        view.update(cx, |v, cx| v.show_picture(picture(800, 600), cx));
        cx.run_until_parked();
        assert_eq!(layer(&view, cx), Some((rect(-100., -150., 800., 600.), fit)), "the whole");
        assert_eq!(base(cx), None, "nothing under a picture of the whole");
    }

    /// This Mac's pointer pushing at an edge of a zoomed picture pans it toward that edge for
    /// as long as it pushes, harder deeper in the band, and moves the worker's pointer to what
    /// is now under it; away from the edges, off the body or at fit, nothing moves. ⌥ with a
    /// scroll pans the zoomed picture here and sends no scroll, a plain scroll goes to the
    /// worker, and a trackpad's gesture keeps the side it began on.
    #[gpui::test]
    fn the_mac_pans_a_zoomed_picture_at_the_edges_and_with_option_scroll(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, mut rx, cx) = windowed(cx);
        view.update(cx, |v, cx| {
            v.touch = false;
            v.show_picture(picture(800, 600), cx);
        });
        cx.run_until_parked();
        let origin = |cx: &gpui::VisualTestContext| view.read_with(cx, |v, _| v.zoom.origin());
        // A refresh at a time, as the push's own ticks come.
        let wait = |cx: &mut gpui::VisualTestContext, ms: u64| {
            for _ in 0..ms.div_ceil(8) {
                cx.executor().advance_clock(Duration::from_millis(8));
                cx.run_until_parked();
            }
        };
        // At fit the edge pushes nothing.
        cx.simulate_mouse_move(point(px(398.0), px(150.0)), None, Modifiers::default());
        wait(cx, 100);
        assert_eq!(origin(cx), (0.0, 0.0));

        view.update(cx, |v, cx| v.set_zoom(Zoom::FIT.about((0.5, 0.5), 2.0, 8.0), cx));
        cx.simulate_mouse_move(point(px(200.0), px(150.0)), None, Modifiers::default());
        wait(cx, 100);
        assert_eq!(origin(cx), (-0.5, -0.5), "the middle pushes nothing");
        drop(inputs(&mut rx));

        cx.simulate_mouse_move(point(px(398.0), px(150.0)), None, Modifiers::default());
        wait(cx, 100);
        let pushed = origin(cx);
        assert!(pushed.0 < -0.55 && (pushed.1 + 0.5).abs() < 1e-6, "panned right: {pushed:?}");
        let moves =
            inputs(&mut rx).into_iter().filter(|i| matches!(i, ScreenInput::Move { .. })).count();
        assert!(moves > 1, "the worker's pointer follows what is under this one: {moves}");
        wait(cx, 2_000);
        assert!((origin(cx).0 + 1.0).abs() < 1e-6, "to the picture's edge and no further");

        // Off the window: the push stops.
        cx.simulate_mouse_move(point(px(2.0), px(150.0)), None, Modifiers::default());
        wait(cx, 50);
        cx.simulate_event(MouseExitEvent {
            position: point(px(-5.0), px(150.0)),
            pressed_button: None,
            modifiers: Modifiers::default(),
        });
        let left = origin(cx);
        wait(cx, 200);
        assert_eq!(origin(cx), left, "nothing pushes once the pointer has gone");
        assert!(left.0 > -1.0, "it had pushed left while it was there");

        // ⌥ with a trackpad's scroll pans here, to the gesture's end even once ⌥ is let go.
        cx.simulate_mouse_move(point(px(200.0), px(150.0)), None, Modifiers::default());
        drop(inputs(&mut rx));
        let before = origin(cx);
        let scroll = |cx: &mut gpui::VisualTestContext, dx: f32, alt: bool, phase| {
            cx.simulate_event(ScrollWheelEvent {
                position: point(px(200.0), px(150.0)),
                delta: ScrollDelta::Pixels(point(px(dx), px(0.0))),
                modifiers: Modifiers { alt, ..Modifiers::default() },
                touch_phase: phase,
                momentum_phase: None,
            });
            cx.run_until_parked();
        };
        scroll(cx, 20.0, true, TouchPhase::Started);
        scroll(cx, 20.0, false, TouchPhase::Moved);
        scroll(cx, 0.0, false, TouchPhase::Ended);
        let after = origin(cx);
        assert!((after.0 - before.0 - 0.1).abs() < 1e-5, "40 of the 400-point frame: {after:?}");
        let scrolls = |rx: &mut mpsc::Receiver<ClientMsg>| {
            inputs(rx).into_iter().filter(|i| matches!(i, ScreenInput::Scroll { .. })).count()
        };
        assert_eq!(scrolls(&mut rx), 0, "nothing scrolled on the worker");
        scroll(cx, 20.0, false, TouchPhase::Started);
        scroll(cx, 0.0, true, TouchPhase::Ended);
        assert_eq!(origin(cx), after, "a plain gesture is the worker's to its end");
        assert_eq!(scrolls(&mut rx), 2);
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
        view.update(cx, |v, cx| {
            v.show_picture(buffer, cx);
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
        let chroma =
            |cx: &mut gpui::TestAppContext| view.read_with(cx, |v, _| v.shape.map(|s| s.chroma));
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

    /// Pictures go to the layer, not through the view: the first draws the view once (the
    /// layer is placed), more of the same shape draw nothing, and a picture of a new size draws
    /// it once more (the layer is placed again).
    #[gpui::test]
    fn a_picture_costs_the_view_no_frame(cx: &mut gpui::TestAppContext) {
        let (view, _rx, cx) = windowed(cx);
        let drawn = renders(&view, cx);
        view.update(cx, |v, cx| v.show_picture(picture(800, 600), cx));
        cx.run_until_parked();
        assert_eq!(renders(&view, cx), drawn.saturating_add(1), "the first picture places it");
        for _ in 0..10 {
            view.update(cx, |v, cx| v.show_picture(picture(800, 600), cx));
            cx.run_until_parked();
        }
        assert_eq!(renders(&view, cx), drawn.saturating_add(1), "ten more, no frame");
        assert_eq!(view.read_with(cx, |v, _| v.frames()), 11, "all went up on the layer");
        view.update(cx, |v, cx| v.show_picture(picture(400, 300), cx));
        cx.run_until_parked();
        assert_eq!(renders(&view, cx), drawn.saturating_add(2), "a new size, placed again");
    }

    /// Where the frames drawn since the last look placed the stream's layer, and the part of it
    /// that shows; `None` when none placed it.
    fn layer(
        view: &gpui::Entity<ScreenView>,
        cx: &gpui::VisualTestContext,
    ) -> Option<(Bounds<Pixels>, Bounds<Pixels>)> {
        view.read_with(cx, |v, _| v.layer_at.take()).map(|(at, mask)| (at, at.intersect(&mask)))
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds { origin: point(px(x), px(y)), size: size(px(w), px(h)) }
    }

    /// A picture of another aspect than its body letterboxes on the stage, the same near-black
    /// in the light appearance as in the dark; before its first picture the body is the page
    /// its words are on.
    #[gpui::test]
    fn a_letterbox_is_the_stage_in_both_appearances(cx: &mut gpui::TestAppContext) {
        let (view, _rx, cx) = windowed(cx);
        let body = |cx: &mut gpui::VisualTestContext| {
            let (scale, quads) =
                cx.update(|window, _| (window.scale_factor(), window.painted_quads()));
            quads
                .iter()
                .filter(|q| 400.0_f32.mul_add(-scale, q.bounds.size.width.0).abs() < 0.5)
                .filter(|q| 300.0_f32.mul_add(-scale, q.bounds.size.height.0).abs() < 0.5)
                .filter_map(|q| q.background.as_solid())
                .collect::<Vec<_>>()
        };
        for variant in [slopty_theme::Variant::Dark, slopty_theme::Variant::Light] {
            let theme = Theme::new(variant);
            view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
            cx.run_until_parked();
            if variant == slopty_theme::Variant::Dark {
                assert_eq!(body(cx), [hsla(theme.content())], "no picture yet: the page");
                view.update(cx, |v, cx| v.show_picture(picture(800, 400), cx));
                cx.run_until_parked();
            }
            assert_eq!(body(cx), [hsla(slopty_theme::STAGE)], "{variant:?}: the stage");
        }
    }

    /// The layer goes where the picture is drawn: at fit its aspect kept and centred in the
    /// 400 × 300 body, zoomed twice about the middle past the body on every side, clipped to it.
    /// Before the first picture it is placed nowhere.
    #[gpui::test]
    fn the_layer_goes_where_the_picture_is_drawn(cx: &mut gpui::TestAppContext) {
        let (view, _rx, cx) = windowed(cx);
        assert_eq!(layer(&view, cx), None, "no picture, no layer");
        view.update(cx, |v, cx| v.show_picture(picture(800, 400), cx));
        cx.run_until_parked();
        let fit = rect(0., 50., 400., 200.);
        assert_eq!(layer(&view, cx), Some((fit, fit)), "fit, centred");

        view.update(cx, |v, cx| v.set_zoom(Zoom::FIT.about((0.5, 0.5), 2.0, 8.0), cx));
        cx.run_until_parked();
        let zoomed = rect(-200., -50., 800., 400.);
        assert_eq!(
            layer(&view, cx),
            Some((zoomed, rect(0., 0., 400., 300.))),
            "clipped to the body"
        );
    }

    /// A striped picture's two layers stack where the picture is drawn: at fit, the 800 × 400
    /// picture's top stripe (256 rows coded, 192 shown) over its lower one (272 coded from row
    /// 128, shown from its row 64), each clipped at the seam, which falls on the same line for
    /// both. Zoomed past the body, both are clipped to it too.
    #[gpui::test]
    fn a_striped_pictures_layers_meet_at_the_seam(cx: &mut gpui::TestAppContext) {
        let (view, _rx, cx) = windowed(cx);
        view.update(cx, |v, cx| {
            v.show_striped((picture(800, 256), 192), (picture(800, 272), 64), cx);
        });
        cx.run_until_parked();
        let lower = |cx: &gpui::VisualTestContext| {
            view.read_with(cx, |v, _| v.lower_at.take()).map(|(at, mask)| (at, at.intersect(&mask)))
        };
        assert_eq!(view.read_with(cx, |v, _| v.size), (800, 400), "both stripes' shown rows");
        // Fit at half size in the 400 × 300 body: the picture at y 50 to 250, the seam at 146.
        assert_eq!(layer(&view, cx), Some((rect(0., 50., 400., 128.), rect(0., 50., 400., 96.))));
        assert_eq!(lower(cx), Some((rect(0., 114., 400., 136.), rect(0., 146., 400., 104.))));

        view.update(cx, |v, cx| v.set_zoom(Zoom::FIT.about((0.5, 0.5), 2.0, 8.0), cx));
        cx.run_until_parked();
        // Twice that about the middle: the picture at y -50 to 350, the seam at 142.
        assert_eq!(
            layer(&view, cx),
            Some((rect(-200., -50., 800., 256.), rect(0., 0., 400., 142.)))
        );
        assert_eq!(lower(cx), Some((rect(-200., 78., 800., 272.), rect(0., 142., 400., 158.))));
    }

    /// Each stripe's layer is its whole picture at the scale of the whole: the lower one's row
    /// at the seam lands on the top one's first row not shown, and the shown parts tile the
    /// picture with no gap and no overlap, at one to one and at any scale.
    #[test]
    fn a_stripes_shown_rows_tile_the_picture() {
        let seam = glass::Seam { top: 1152, top_rows: 1088, lower: 1136, lower_from: 64 };
        let shape = glass::Shape {
            size: (3840, 2160),
            chroma: Chroma::Subsampled,
            seam: Some(seam),
            region: None,
        };
        for (at, scale) in [(rect(0., 0., 3840., 2160.), 1.0), (rect(10., 20., 960., 540.), 0.25)] {
            let [top, lower] = stripe_places(at, shape).expect("striped");
            let row = |rows: f32| at.origin.y + px(rows * scale);
            assert_eq!(top.layer.origin, at.origin);
            assert_eq!(top.layer.size.height, px(1152. * scale));
            assert_eq!(top.shown.bottom(), row(1088.), "the seam");
            assert_eq!(lower.shown.top(), top.shown.bottom(), "no gap, no overlap");
            assert_eq!(lower.layer.top() + px(64. * scale), row(1088.), "its seam row at the seam");
            assert_eq!(lower.layer.size.height, px(1136. * scale));
            assert_eq!(lower.shown.bottom(), at.bottom(), "down to the picture's last row");
            assert_eq!(
                (top.shown.size.width, lower.shown.size.width),
                (at.size.width, at.size.width)
            );
        }
        let whole = glass::Shape { seam: None, ..shape };
        assert_eq!(stripe_places(rect(0., 0., 1., 1.), whole), None);
    }

    /// A tile the strip holds.
    struct Strip {
        screen: gpui::Entity<ScreenView>,
        left: f32,
        shown: bool,
    }

    impl Render for Strip {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().overflow_hidden().children(self.shown.then(|| {
                div()
                    .absolute()
                    .top_0()
                    .left(px(self.left))
                    .w(px(400.0))
                    .h(px(300.0))
                    .child(self.screen.clone())
            }))
        }
    }

    /// The layer follows its tile as the strip scrolls it, in the frame that moves it; half off
    /// the viewport it is clipped to what shows, and a tile the strip no longer draws places it
    /// nowhere, which hides it.
    #[gpui::test]
    fn the_layer_follows_its_tile_through_the_strip(cx: &mut gpui::TestAppContext) {
        let (out, _rx) = mpsc::channel(64);
        let opened = Opened {
            stream: StreamId(4),
            target: CaptureTarget::Display(DisplayId(2)),
            size: (800, 600),
            quality: Quality { scale: 1.0, ..Quality::default() },
        };
        let (strip, cx) = cx.add_window_view(|_window, cx| {
            let handle = ScreenHandle::detached(StreamId(4));
            let screen = cx.new(|cx| ScreenView::new(opened, handle, out, Theme::default(), cx));
            Strip { screen, left: 0.0, shown: true }
        });
        cx.simulate_resize(size(px(600.0), px(300.0)));
        let view = strip.read_with(cx, |s, _| s.screen.clone());
        view.update(cx, |v, cx| v.show_picture(picture(800, 600), cx));
        cx.run_until_parked();
        let whole = rect(0., 0., 400., 300.);
        assert_eq!(layer(&view, cx), Some((whole, whole)));

        for left in [40.0, 120.0, 400.0] {
            strip.update(cx, |s, cx| {
                s.left = left;
                s.screen.update(cx, |_, cx| cx.notify());
                cx.notify();
            });
            cx.run_until_parked();
            let shows = (600.0 - left).min(400.0);
            assert_eq!(
                layer(&view, cx),
                Some((rect(left, 0., 400., 300.), rect(left, 0., shows, 300.))),
                "moved with the tile, clipped to the viewport"
            );
        }

        strip.update(cx, |s, cx| {
            s.shown = false;
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(layer(&view, cx), None, "a tile not drawn places no layer");
    }

    /// The stats overlay on a still stream is drawn again with each reading of the counters,
    /// and with the rate and the round trip as the worker and the link report them: no frame
    /// off the stream comes to draw it.
    #[gpui::test]
    fn the_overlay_follows_its_numbers_on_a_still_stream(cx: &mut gpui::TestAppContext) {
        let (view, _rx, cx) = windowed(cx);
        view.update(cx, |v, cx| {
            v.show_picture(picture(800, 600), cx);
            v.set_hud(true, cx);
        });
        cx.run_until_parked();
        let drawn = renders(&view, cx);
        cx.executor().advance_clock(HUD_PERIOD);
        cx.run_until_parked();
        assert!(renders(&view, cx) > drawn, "the counters read again, drawn");
        let drawn = renders(&view, cx);
        view.update(cx, |v, cx| v.set_rate(8_000_000, RateVerdict::Cut, false, cx));
        cx.run_until_parked();
        assert_eq!(renders(&view, cx), drawn.saturating_add(1), "the rate, drawn");
        let rtt = Some(Duration::from_millis(12));
        view.update(cx, |v, cx| v.set_rtt(rtt, rtt, cx));
        cx.run_until_parked();
        assert_eq!(renders(&view, cx), drawn.saturating_add(2), "the round trip, drawn");
        view.update(cx, |v, cx| v.set_rtt(Some(Duration::from_millis(30)), rtt, cx));
        cx.run_until_parked();
        assert_eq!(renders(&view, cx), drawn.saturating_add(2), "the same printed one: no frame");
    }

    /// The overlay prints the round trip the workspace pins (the e2e harness's), while the
    /// pointer hold keeps timing itself off the link's own.
    #[gpui::test]
    fn the_overlay_prints_the_pinned_round_trip_and_the_hold_keeps_the_links(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, _rx, cx) = windowed(cx);
        let (live, pinned) = (Duration::from_millis(40), Duration::from_millis(1));
        view.update(cx, |v, cx| {
            v.show_picture(picture(800, 600), cx);
            v.set_rtt(Some(live), Some(pinned), cx);
            v.set_hud(true, cx);
        });
        cx.executor().advance_clock(HUD_PERIOD);
        cx.run_until_parked();
        let printed = view.read_with(cx, |v, _| {
            v.hud_text().map(|(summary, _)| summary.into_iter().map(|f| f.text).collect::<Vec<_>>())
        });
        let printed = printed.unwrap_or_default();
        assert!(printed.iter().any(|t| t == "RTT 1.0 ms"), "{printed:?}");
        let now = Instant::now();
        let end = view.update(cx, |v, _| {
            v.place((0.0, 0.0), now);
            v.hold_end()
        });
        assert_eq!(end, now.checked_add(LOCAL_HOLD.saturating_add(live)), "the link's figure");
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

    /// A sender for the cursor samples the view's pump reads, as the stream's own.
    fn pumped(
        view: &gpui::Entity<ScreenView>,
        cx: &mut gpui::VisualTestContext,
    ) -> watch::Sender<CursorState> {
        let (cursor_tx, cursor) = watch::channel(CursorState::default());
        #[expect(clippy::used_underscore_binding, reason = "the pump is kept, not read, but here")]
        view.update(cx, |v, cx| v._pump = ScreenView::pump(v.glass.shapes(), cursor, cx));
        cursor_tx
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

    /// On a window stream the pointer is the system pointer in the worker's cursor picture, which
    /// the window server moves with the hand: the view never draws it, a move draws no frame, a
    /// new picture from the worker draws none, and a late echo neither moves nor draws anything.
    #[gpui::test]
    fn a_window_streams_pointer_is_the_system_pointer_in_the_workers_picture(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, mut rx, cx) = windowed_on(cx, CaptureTarget::Window(slopty_core::WindowId(9)));
        let cursor = pumped(&view, cx);
        let shape =
            |fill| CursorShape { w: 8, h: 8, hot_x: 2, hot_y: 2, bgra: vec![fill; 256], scale: 2 };
        view.update(cx, |v, cx| {
            v.show_picture(picture(800, 600), cx);
            v.set_cursor_shape(Some(shape(0)), cx);
        });
        cx.run_until_parked();
        let id = view.read_with(cx, |v, _| v.system_pointer.0);
        let style = |cx: &mut gpui::VisualTestContext| view.read_with(cx, |v, _| v.styled);
        assert_eq!(style(cx), CursorStyle::Image(id), "the worker's picture as the system pointer");
        assert_eq!(drawn_at(cx), None, "and none drawn");

        let first = at_fraction(&view, cx, 0.25, 0.5);
        cx.simulate_mouse_move(first, None, Modifiers::default());
        cx.run_until_parked();
        let before = renders(&view, cx);
        let second = at_fraction(&view, cx, 0.75, 0.25);
        cx.simulate_mouse_move(second, None, Modifiers::default());
        cx.run_until_parked();
        assert_eq!(renders(&view, cx), before, "a move draws no frame");
        assert_eq!(drawn_at(cx), None);
        let moves = inputs(&mut rx);
        assert!(
            matches!(moves.as_slice(), [ScreenInput::Move { .. }, ScreenInput::Move { x, y }] if (*x - 600.0).abs() < 0.5 && (*y - 150.0).abs() < 0.5),
            "both moves went: {moves:?}"
        );

        view.update(cx, |v, cx| v.set_cursor_shape(Some(shape(255)), cx));
        cx.run_until_parked();
        assert_eq!(renders(&view, cx), before, "a new picture draws no frame");
        assert_eq!(style(cx), CursorStyle::Image(id), "under the same name");

        cursor.send(CursorState { x: 200, y: 300, visible: true }).expect("the pump listens");
        cx.run_until_parked();
        assert_eq!(drawn_at(cx), None, "the late echo draws no pointer");
        assert_eq!(renders(&view, cx), before, "and no frame");
    }

    /// On a display the worker's sample draws the pointer while this client is not moving it
    /// (another user's hand, an app's warp), the system pointer is it while this client moves
    /// it, and once the hold runs out the worker's sample takes over, drawn where the worker
    /// says the pointer went.
    #[gpui::test]
    fn a_displays_pointer_follows_the_worker_unless_this_client_moves_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, _rx, cx) = windowed(cx);
        let cursor = pumped(&view, cx);
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

        let style = |cx: &mut gpui::VisualTestContext| view.read_with(cx, |v, _| v.styled);
        assert_eq!(style(cx), CursorStyle::None, "the view draws the worker's");
        let here = at_fraction(&view, cx, 0.25, 0.25);
        cx.simulate_mouse_move(here, None, Modifiers::default());
        cx.run_until_parked();
        assert_eq!(arrow_drawn(cx), None, "this client's is the system pointer, at once");
        assert_eq!(style(cx), CursorStyle::Arrow, "the arrow until the worker sends a picture");

        let drawn = renders(&view, cx);
        let further = at_fraction(&view, cx, 0.3, 0.3);
        cx.simulate_mouse_move(further, None, Modifiers::default());
        cursor.send(CursorState { x: 600, y: 450, visible: true }).expect("the pump listens");
        cx.run_until_parked();
        assert_eq!(arrow_drawn(cx), None, "held while this client drives it");
        assert_eq!(renders(&view, cx), drawn, "a move or a sample under the hold draws nothing");

        cx.executor().advance_clock(LOCAL_HOLD);
        cx.run_until_parked();
        let there = at_fraction(&view, cx, 0.75, 0.75);
        assert_eq!(
            arrow_drawn(cx),
            Some(arrow_at(there)),
            "the hold ran out: where the worker's went"
        );
        assert_eq!(style(cx), CursorStyle::None, "drawn by the view again");

        cursor.send(CursorState { x: 80, y: 60, visible: true }).expect("the pump listens");
        cx.run_until_parked();
        let warped = at_fraction(&view, cx, 0.1, 0.1);
        assert_eq!(arrow_drawn(cx), Some(arrow_at(warped)), "moved on the worker alone");
    }

    /// Pictures at 60 Hz with cursor samples at 120 Hz, the worker moving the pointer: the
    /// pictures go to the layer and draw nothing, and every sample that moves the pointer draws
    /// it at once. A sample that moves nothing drawn draws nothing: the same place, or hidden
    /// and hidden.
    #[gpui::test]
    fn samples_draw_at_once_and_pictures_draw_nothing(cx: &mut gpui::TestAppContext) {
        const FRAMES: u64 = 60;
        const SAMPLES: u64 = 120;
        let (view, _rx, cx) = windowed(cx);
        let cursor = pumped(&view, cx);
        view.update(cx, |v, cx| v.show_picture(picture(800, 600), cx));
        cx.run_until_parked();
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
                view.update(cx, |v, cx| v.show_picture(picture(800, 600), cx));
            } else {
                x = x.saturating_add(3);
                cursor.send(CursorState { x, y: 300, visible: true }).expect("the pump listens");
            }
            cx.run_until_parked();
        }
        let draws = renders(&view, cx).saturating_sub(before);
        println!(
            "MEASURE {FRAMES} pictures at 60 Hz and {SAMPLES} samples at 120 Hz: {draws} draws"
        );
        assert_eq!(draws, u32::try_from(SAMPLES).expect("small"), "one a sample, none a picture");
        #[expect(clippy::cast_precision_loss, reason = "a small coordinate")]
        let last = x as f32 / 800.0;
        near_px(offset(&view, cx), (last * 400.0, 150.0));

        let drawn = renders(&view, cx);
        cursor.send(CursorState { x, y: 300, visible: true }).expect("the pump listens");
        cx.run_until_parked();
        cursor.send(CursorState { x: 0, y: 0, visible: false }).expect("the pump listens");
        cx.run_until_parked();
        cursor.send(CursorState { x: 5, y: 5, visible: false }).expect("the pump listens");
        cx.run_until_parked();
        assert_eq!(renders(&view, cx), drawn.saturating_add(1), "only the hiding drew");
    }
}
