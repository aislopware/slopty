//! `ScreenView`: one remote window or display, painted from the newest decoded frame.
//!
//! The frame arrives as a `slopty_client::Presentable`: an IOSurface-backed `CVPixelBuffer`
//! plus the instants that got it here. GPUI's `surface` element samples it through
//! `CVMetalTextureCache`, so nothing is copied on the client, and a `slopty_client::Pacer`
//! decides when it goes up (present on arrival, never a queue) and measures how long the
//! journey took. The worker's cursor is drawn here from the cursor channel (one RTT behind the
//! pointer, not one video pipeline). Pointer, scroll and key events inside the view go to the
//! worker as
//! `ScreenInput` in stream pixels; the worker injects them. ⌘ chords the canvas binds (⌘T/⌘O/⌘W,
//! zoom) never reach the view because GPUI runs key bindings before key listeners; every other
//! chord (⌘C, ⌘V, ⌘Z, ⌘S…) is forwarded to the remote window. The view also asks the worker for a
//! smaller stream when it is painted small (canvas zoomed out), quantised so the encoder is
//! not rebuilt on every wheel tick.

use std::sync::Arc;
use std::time::{Duration, Instant};

use core_foundation::base::TCFType as _;
use core_video::pixel_buffer::CVPixelBuffer;
use gpui::{
    Autocapitalize, Bounds, Context, CursorStyle, ElementInputHandler, EntityInputHandler,
    EventEmitter, FocusHandle, Focusable, InteractiveElement as _, IntoElement, KeyDownEvent,
    KeyUpEvent, Keystroke, LongPressEvent, Modifiers, ModifiersChangedEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, ParentElement as _, PathBuilder,
    Pixels, Point, Render, RenderImage, ScrollDelta, ScrollWheelEvent, Size,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TextInputAction,
    TextInputConfiguration, TouchPhase, UTF16Selection, Window, canvas, div, point, px, size,
    surface,
};
use slopty_client::pacing::{Pace, Pacer, PacingStats};
use slopty_client::{CursorState, Presentable, ScreenHandle, ScreenStats};
use slopty_core::StreamId;
use slopty_proto::ClientMsg;
use slopty_proto::input::{KeyAction, KeyCode, Mods, MouseButton as ProtoButton};
use slopty_proto::screen::{
    CaptureTarget, CursorShape, Quality, RateVerdict, ScreenInput, ScreenRequest, ScrollPhase,
    SourceState, VideoCodec,
};
use slopty_theme::{Theme, alpha};
use tokio::sync::mpsc;

use crate::colors::hsla;
use crate::keys;

/// What goes on the control stream ahead of a paste chord: this client's clipboard offer, when
/// the worker has not heard it yet (`crate::clipboard::ClipSync::offer_for`).
pub type PasteHook = std::rc::Rc<dyn Fn() -> Option<ClientMsg>>;

/// Makes a client-side stream for an `Opened` event (wraps `WorkerLink::screen`).
pub type ScreenFactory = Arc<dyn Fn(StreamId, VideoCodec) -> ScreenHandle + Send + Sync>;

/// Quality change rate limit.
const QUALITY_COOLDOWN: Duration = Duration::from_millis(400);
/// Smallest scale the view asks for.
const MIN_SCALE: f32 = 0.25;

/// Things the canvas may react to.
#[derive(Clone, Copy, Debug)]
pub enum ScreenViewEvent {
    /// First frame painted.
    Ready,
    /// Pressed: the canvas should make this item active. (The view stops the mouse event so
    /// the canvas does not pan, which also keeps it from the item's own activate handler.)
    Pressed,
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

/// Where a cursor picture goes: its hotspot on `at`.
fn pointer_bounds(at: Point<Pixels>, size: Size<Pixels>, hot: Point<Pixels>) -> Bounds<Pixels> {
    Bounds { origin: point(at.x - hot.x, at.y - hot.y), size }
}

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
    /// Newest frame and its Metal-ready wrapper.
    latest: Option<(Arc<Presentable>, CVPixelBuffer)>,
    /// Stream size in pixels as opened.
    size: (u32, u32),
    /// Native pixel size of the target (stream size at scale 1).
    native: (f32, f32),
    quality: Quality,
    quality_changed: Instant,
    cursor: CursorState,
    /// The worker's cursor as it is drawn at that position.
    pointer: Pointer,
    out: mpsc::Sender<ClientMsg>,
    theme: Theme,
    focus: FocusHandle,
    bounds: Bounds<Pixels>,
    frames: u64,
    /// Keys whose press went to the worker, so a release for a locally-handled chord (its press
    /// was eaten by a canvas binding) is not forwarded as a stray key-up.
    held: Vec<KeyCode>,
    /// Asked before a paste chord goes, so the worker pastes what this client copied.
    paste_hook: Option<PasteHook>,
    /// Modifiers armed by the phone key bar; applied to the next key, then cleared.
    sticky: Modifiers,
    /// The modifier keys as last reported, so a change forwards the key that moved.
    modifiers: Modifiers,
    /// Focus leaving the view and the window going inactive each release what is held on the
    /// worker (registered at the first render: the constructor has no window).
    let_go: Option<[Subscription; 2]>,
    /// Input-method composition in progress (nothing is sent until it commits).
    marked: Option<String>,
    /// The stats overlay (⌘⇧I).
    hud: Option<Hud>,
    /// Decides when a decoded frame goes up and measures arrival → present. The element only
    /// feeds it: a frame on one side, a paint on the other.
    pacer: Pacer,
    /// Link RTT from the canvas, for the overlay.
    rtt: Option<Duration>,
    /// Holds the device out of idle sleep (the Mac) or its screen on (the phone) for as long as
    /// this window streams; dropping the view lets go.
    _awake: Task<()>,
    /// On the Mac, GPUI's hold is the system's only: this one keeps the display on too, since
    /// a viewer watching a remote window is not touching the keyboard.
    #[cfg(target_os = "macos")]
    _display: slopty_platform::Activity,
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
    _pump: Task<()>,
}

/// Rates for the stats overlay, re-sampled about once a second.
#[derive(Clone, Debug)]
struct Hud {
    sampled_at: Instant,
    sample: ScreenStats,
    text: String,
}

/// What the overlay shows that is not in [`ScreenStats`].
#[derive(Clone, Copy, Debug)]
pub struct HudInput<'a> {
    /// Stream size in pixels.
    pub size: (u32, u32),
    /// Capture scale.
    pub scale: f32,
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

/// The four lines of the stats overlay: what is on screen, how it got there, when, and how
/// the UI itself keeps up.
///
/// Line one is the picture: size, rate, throughput, round trip and the age of the frame being
/// shown. Line two is the path: jitter (RFC 3550 interarrival), how long frames waited for
/// their last fragment (p50 / p95 of the last report), the in-order queue, recovery counts,
/// stalls, the worker's bitrate verdict and audio. Line three is the presentation: how long a
/// frame takes from the arrival of the datagram that completed it to the paint that shows it
/// (p50 / p95 / worst of the last `slopty_client::pacing::RING` frames, with the decoder's share
/// of it), the spacing of those paints and its jitter, and the two cadence faults —
/// `skip` (a frame the display never saw) and `repeat` (a paint that showed the picture again).
/// Line four is the UI: draw time of the whole window (p50 / p95 / p99 / max over the last
/// [`crate::frames::RING`] frames), the spacing of frames, and how many went over the display
/// period or were dropped ([`crate::frames::hud_line`]). Pure so it can be checked without a
/// window.
#[must_use]
pub fn hud_lines(input: &HudInput<'_>) -> String {
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    let stats = input.stats;
    let rtt = input.rtt.map_or_else(|| "rtt –".to_owned(), |d| format!("rtt {:.1} ms", ms(d)));
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
        "{}×{} @{:.2}  ·  {:.0} fps  ·  {:.2} Mb/s  ·  {rtt}  ·  {age}\n\
         jitter {:.1} ms  ·  hold {:.1} / {:.1} ms  ·  queue {}  ·  fec {} lost {} nack {} refresh {}  ·  stalls {} ({} ms) {stall}  ·  {rate}  ·  audio {} lost {} concealed {}\n\
         present {:.1} / {:.1} / {:.1} ms (decode {:.1})  ·  every {:.1} ms ±{:.1}  ·  shown {} skip {} repeat {} late {}\n\
         {ui}",
        input.size.0,
        input.size.1,
        input.scale,
        input.fps,
        input.mbps,
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

/// How often the overlay's rates are recomputed.
const HUD_PERIOD: Duration = Duration::from_millis(1000);

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
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

/// The quality a stream is asked for: the settings' rate and ceiling at `scale`.
#[must_use]
pub const fn quality_of(prefs: slopty_theme::StreamPrefs, scale: f32) -> Quality {
    Quality { fps: prefs.fps, bitrate_bps: prefs.max_bitrate_bps, scale, codec: VideoCodec::Hevc }
}

impl ScreenView {
    /// Swap the theme: chrome colours, and the stream settings, which a live stream asks
    /// the worker for at once (the scale it holds stays; that follows the canvas).
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme == theme {
            return;
        }
        let wanted = quality_of(theme.behaviour.stream, self.quality.scale);
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

    /// Wrap an opened stream.
    pub fn new(
        opened: Opened,
        handle: ScreenHandle,
        out: mpsc::Sender<ClientMsg>,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Self {
        let Opened { stream, target, size, quality } = opened;
        handle.set_muted(theme.behaviour.stream.muted);
        let mut frames = handle.frames();
        let mut cursor = handle.cursor();
        let pump = cx.spawn(async move |this, cx| {
            loop {
                tokio::select! {
                    changed = frames.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        let frame = frames.borrow_and_update().clone();
                        let alive = this.update(cx, |view, cx| {
                            view.take_frame(frame, cx);
                            cx.notify();
                        });
                        if alive.is_err() {
                            break;
                        }
                    }
                    changed = cursor.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        let state = *cursor.borrow_and_update();
                        let alive = this.update(cx, |view, cx| {
                            view.cursor = state;
                            cx.notify();
                        });
                        if alive.is_err() {
                            break;
                        }
                    }
                }
            }
        });
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
            size,
            native,
            quality,
            quality_changed: Instant::now(),
            cursor: CursorState::default(),
            pointer: Pointer::Arrow,
            out,
            theme,
            focus: cx.focus_handle(),
            bounds: Bounds::default(),
            frames: 0,
            held: Vec::new(),
            modifiers: Modifiers::default(),
            let_go: None,
            paste_hook: None,
            sticky: Modifiers::default(),
            marked: None,
            hud: None,
            rtt: None,
            _awake: awake,
            #[cfg(target_os = "macos")]
            _display: slopty_platform::Activity::display_awake("Slopty remote window"),
            pacer: Pacer::default(),
            rate: None,
            source: SourceState::Live,
            scrolling: Scrolling::Idle,
            momentum_end: None,
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
    fn a11y_label(&self) -> String {
        match self.target {
            CaptureTarget::Display(id) => format!("Remote display {id}"),
            CaptureTarget::Window(id) => format!("Remote window {}", id.0),
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
            text: "…".to_owned(),
        });
        cx.notify();
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

    /// The worker's latest bitrate decision, shown in the overlay.
    pub const fn set_rate(&mut self, target_bps: u32, verdict: RateVerdict, capped: bool) {
        self.rate = Some((target_bps, verdict, capped));
    }

    /// The worker said which cursor it shows (`ScreenEvent::Cursor`): draw that picture at the
    /// pointer from now on, or the arrow again for `None`.
    pub fn set_cursor_shape(&mut self, shape: Option<CursorShape>, cx: &mut Context<Self>) {
        self.pointer = Pointer::from_shape(shape);
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

    /// Recompute the overlay's rates when a second has passed; returns the text to draw.
    fn hud_text(&mut self, cx: &gpui::App) -> Option<String> {
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
            hud.text = hud_lines(&HudInput {
                size: self.size,
                scale: self.quality.scale,
                fps,
                mbps,
                rtt: self.rtt,
                frame_age: self.pacer.age(),
                rate: self.rate,
                stats: &stats,
                pacing: &self.pacer.stats(),
                ui: ui.as_ref(),
            });
            hud.sample = stats;
            hud.sampled_at = now;
        }
        Some(hud.text.clone())
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
    /// The pill lives in the canvas title bar, so the caller notifies its own entity.
    pub fn toggle_mute(&self) {
        self.handle.set_muted(!self.handle.muted());
    }

    /// The canvas reports how wide the view is painted (device pixels) so the stream can be
    /// downscaled at the worker when zoomed out. Quantised to quarter steps and rate limited.
    pub fn set_painted_width(&mut self, device_px: f32) {
        let wanted = (device_px / self.native.0).clamp(MIN_SCALE, 1.0);
        let bucket = (wanted * 4.0).ceil() / 4.0;
        if (bucket - self.quality.scale).abs() < f32::EPSILON
            || self.quality_changed.elapsed() < QUALITY_COOLDOWN
        {
            return;
        }
        self.quality.scale = bucket;
        self.quality_changed = Instant::now();
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped")]
        let side = |native: f32| ((native * bucket).round().max(2.0) as u32).next_multiple_of(2);
        self.size = (side(self.native.0), side(self.native.1));
        self.send(ScreenRequest::SetQuality { stream: self.stream, quality: self.quality });
    }

    /// The worker resized the target: the stream now has this pixel size at the current quality
    /// scale, so the native size follows from it.
    pub fn set_geometry(&mut self, width: u32, height: u32) {
        self.size = (width, height);
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
    fn take_frame(&mut self, frame: Option<Arc<Presentable>>, cx: &mut Context<Self>) {
        let Some(frame) = frame else { return };
        if self.pacer.offer(frame.stamp) == Pace::Drop {
            return;
        }
        let raw = std::ptr::from_ref(frame.frame.image.as_cv())
            .cast_mut()
            .cast::<core_video::buffer::__CVBuffer>();
        // SAFETY: `raw` is a live `CVPixelBufferRef` owned by `frame`; `wrap_under_get_rule`
        // takes its own retain, so the wrapper stays valid even if `frame` is dropped first.
        let buffer = unsafe { CVPixelBuffer::wrap_under_get_rule(raw) };
        #[expect(clippy::cast_possible_truncation, reason = "pixel counts")]
        let size = (frame.frame.image.width() as u32, frame.frame.image.height() as u32);
        self.size = size;
        self.latest = Some((frame, buffer));
        self.frames = self.frames.saturating_add(1);
        if self.frames == 1 {
            cx.emit(ScreenViewEvent::Ready);
        }
    }

    /// Arrival → present numbers for the last frames (the overlay and the app self-test).
    #[must_use]
    pub fn pacing(&self) -> PacingStats {
        self.pacer.stats()
    }

    fn send(&self, req: ScreenRequest) {
        if let Err(e) = self.out.try_send(ClientMsg::Screen(req)) {
            tracing::warn!(stream = %self.stream, error = %e, "outbound queue");
        }
    }

    fn input(&self, input: ScreenInput) {
        self.send(ScreenRequest::Input { stream: self.stream, input });
    }

    /// Window position → stream pixels.
    fn to_stream(&self, position: Point<Pixels>) -> (f32, f32) {
        let width = f32::from(self.bounds.size.width).max(1.0);
        let height = f32::from(self.bounds.size.height).max(1.0);
        #[expect(clippy::cast_precision_loss, reason = "pixel counts are small")]
        let (stream_w, stream_h) = (self.size.0 as f32, self.size.1 as f32);
        let x = (f32::from(position.x) - f32::from(self.bounds.origin.x)) / width * stream_w;
        let y = (f32::from(position.y) - f32::from(self.bounds.origin.y)) / height * stream_h;
        tracing::trace!(?position, bounds = ?self.bounds, size = ?self.size, x, y, "to_stream");
        (x, y)
    }

    fn inside(&self, p: Point<Pixels>) -> bool {
        self.bounds.contains(&p)
    }

    fn mouse_move(&mut self, ev: &MouseMoveEvent, _w: &mut Window, _cx: &mut Context<Self>) {
        if !self.inside(ev.position) {
            return;
        }
        let (x, y) = self.to_stream(ev.position);
        self.input(ScreenInput::Move { x, y });
    }

    fn mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) {
            self.send(ScreenRequest::Focus(self.stream));
        }
        self.focus.focus(window, cx);
        cx.emit(ScreenViewEvent::Pressed);
        let button = proto_button(ev.button);
        let (x, y) = self.to_stream(ev.position);
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

    fn mouse_up(&mut self, ev: &MouseUpEvent, _w: &mut Window, _cx: &mut Context<Self>) {
        let button = proto_button(ev.button);
        let (x, y) = self.to_stream(ev.position);
        self.input(ScreenInput::Button {
            button,
            down: false,
            x,
            y,
            clicks: u8::try_from(ev.click_count).unwrap_or(u8::MAX),
            mods: keys::mods(ev.modifiers),
        });
    }

    fn scroll_wheel(&mut self, ev: &ScrollWheelEvent, _w: &mut Window, cx: &mut Context<Self>) {
        if ev.modifiers.platform {
            // ⌘-scroll is the canvas zoom gesture; let it through.
            return;
        }
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
            self.push_clipboard();
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

    /// Focus left the view or the window went inactive: release on the worker every key and
    /// modifier whose press went there, since the release never will (⌘-tab away with ⌘
    /// held, a click on another card while a key is down) — a key stuck down on the worker is
    /// the one thing a remote desktop must never leave behind.
    fn let_go(&mut self, cx: &mut Context<Self>) {
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
    /// worker has not heard it yet.
    fn push_clipboard(&self) {
        let Some(msg) = self.paste_hook.as_ref().and_then(|hook| hook()) else { return };
        if let Err(e) = self.out.try_send(msg) {
            tracing::warn!(stream = %self.stream, error = %e, "outbound queue");
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
            self.push_clipboard();
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
    /// a plain drag stays a canvas pan. Returns whether the gesture was claimed.
    fn long_press(&self, ev: &LongPressEvent, cx: &mut Context<Self>) -> bool {
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

    /// The phone's "paste" key: ⌘V on the worker, this client's clipboard pushed first.
    pub fn paste_key(&mut self, cx: &mut Context<Self>) {
        self.press(chord("v"), cx);
    }

    /// The phone's "copy" key: ⌘C on the worker; the worker's pasteboard then flows back here.
    pub fn copy_key(&mut self, cx: &mut Context<Self>) {
        self.press(chord("c"), cx);
    }

    /// The worker's pointer in view coordinates: its own cursor picture when the worker has sent
    /// one, else a drawn arrow.
    fn cursor_overlay(&self) -> Option<impl IntoElement + use<>> {
        if !self.cursor.visible || self.latest.is_none() {
            return None;
        }
        let picture = match &self.pointer {
            Pointer::Arrow => None,
            Pointer::Image { image, size, hot } => Some((Arc::clone(image), *size, *hot)),
        };
        let w = f32::from(self.bounds.size.width).max(1.0);
        let h = f32::from(self.bounds.size.height).max(1.0);
        #[expect(clippy::cast_precision_loss, reason = "pixel counts are small")]
        let (sw, sh) = (self.size.0.max(1) as f32, self.size.1.max(1) as f32);
        #[expect(clippy::cast_precision_loss, reason = "cursor coordinates are small")]
        let (cx_px, cy_px) = (self.cursor.x as f32 / sw * w, self.cursor.y as f32 / sh * h);
        let fill = hsla(self.theme.surfaces.text);
        let outline = hsla(self.theme.surfaces.canvas);
        Some(
            canvas(
                |_bounds, _window, _cx| {},
                move |bounds, (), window, _cx| {
                    let origin = point(bounds.origin.x + px(cx_px), bounds.origin.y + px(cy_px));
                    if let Some((image, size, hot)) = picture {
                        let at = pointer_bounds(origin, size, hot);
                        let _painted =
                            window.paint_image(at, at, gpui::Corners::default(), image, 0, false);
                        return;
                    }
                    let arrow = |inset: f32, scale: f32| {
                        let mut path = PathBuilder::fill();
                        let at = |x: f32, y: f32| {
                            point(
                                origin.x + px(x.mul_add(scale, inset)),
                                origin.y + px(y.mul_add(scale, inset)),
                            )
                        };
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
                    if let Some(outline_path) = arrow(-1.0, 1.15) {
                        window.paint_path(outline_path, outline);
                    }
                    if let Some(fill_path) = arrow(0.0, 1.0) {
                        window.paint_path(fill_path, fill);
                    }
                },
            )
            .absolute()
            .size_full(),
        )
    }
}

impl Drop for ScreenView {
    fn drop(&mut self) {
        self.send(ScreenRequest::Close(self.stream));
    }
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
        if self.let_go.is_none() {
            let blur = cx.on_blur(&self.focus, window, |this, _window, cx| this.let_go(cx));
            let inactive = cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    this.let_go(cx);
                }
            });
            self.let_go = Some([blur, inactive]);
        }
        let entity = cx.entity();
        let handler = cx.entity();
        let focus = self.focus.clone();
        let record_bounds = canvas(
            move |bounds, _window, cx| {
                entity.update(cx, |this, _| this.bounds = bounds);
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

        let picture = self.latest.as_ref().map_or_else(
            || {
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
            |(_frame, buffer)| {
                surface(buffer.clone()).object_fit(ObjectFit::Fill).size_full().into_any_element()
            },
        );

        let hud = self.hud_text(cx).map(|text| {
            div()
                .absolute()
                .top(px(self.theme.spacing.xs))
                .right(px(self.theme.spacing.xs))
                .px(px(self.theme.spacing.sm))
                .py(px(self.theme.spacing.xxs))
                .rounded(px(self.theme.radii.xs))
                .bg(crate::colors::hsla_alpha(self.theme.surfaces.overlay, alpha::VEIL))
                .text_size(px(self.theme.typography.caption()))
                .text_color(hsla(self.theme.surfaces.text_secondary))
                .font_family(self.theme.typography.ui_family.clone())
                .child(text)
        });
        div()
            .id("screen")
            // While the view has the keys, the canvas's own chords stand back (`!Screen`).
            .key_context("Screen")
            .role(gpui::accesskit::Role::Image)
            .aria_label(gpui::SharedString::from(self.a11y_label()))
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(hsla(self.theme.surfaces.canvas))
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
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .child(picture)
            .child(record_bounds)
            .children(self.cursor_overlay())
            .children(hud)
    }
}

/// The pointer the client shows over the card: none while a frame is up, since the worker's
/// pointer is drawn on it (its picture, or an arrow, or nothing when the worker hides it, all
/// one round trip behind — two pointers that far apart read as a lag), and the arrow before
/// the first frame, when there is nothing to point at yet.
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
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // The worker's pointer stands in for a caret: candidate windows hang there.
        let (w, h) = (f32::from(self.bounds.size.width), f32::from(self.bounds.size.height));
        #[expect(clippy::cast_precision_loss, reason = "pixel counts are small")]
        let (sw, sh) = ((self.size.0 as f32).max(1.0), (self.size.1 as f32).max(1.0));
        #[expect(clippy::cast_precision_loss, reason = "pixel counts are small")]
        let (x, y) = (self.cursor.x as f32 / sw * w, self.cursor.y as f32 / sh * h);
        let origin = point(self.bounds.origin.x + px(x), self.bounds.origin.y + px(y));
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
    /// and what the presentation path did with it all.
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
                "1920×1080 @1.00  ·  60 fps  ·  18.25 Mb/s  ·  rtt 9.4 ms  ·  age 12 ms",
                "jitter 1.2 ms  ·  hold 2.0 / 9.0 ms  ·  queue 1  ·  fec 3 lost 1 nack 4 refresh 1  ·  stalls 2 (140 ms) flowing  ·  target 19.2 Mb/s hold (stall) (cwnd)  ·  audio 50 lost 0 concealed 0",
                "present 5.4 / 11.9 / 28.0 ms (decode 2.1)  ·  every 16.7 ms ±1.4  ·  shown 1204 skip 2 repeat 7 late 0",
                "ui –",
            ]
        );
        let blank = hud_lines(&HudInput {
            size: (0, 0),
            scale: 0.5,
            fps: 0.0,
            mbps: 0.0,
            rtt: None,
            frame_age: None,
            rate: None,
            stats: &ScreenStats::default(),
            pacing: &PacingStats::default(),
            ui: None,
        });
        assert!(blank.contains("rtt –") && blank.contains("age –") && blank.contains("target –"));
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
            target: CaptureTarget::Display(2),
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
        assert!((quality.scale - 1.0).abs() < f32::EPSILON, "the scale is the canvas's");
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

    /// The client's own pointer hides over a card that shows a frame, where the worker's is
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

    /// Zooming the card out asks the worker for a smaller picture, in quarter steps, not more
    /// than once per cooldown; a resize from the worker keeps the native size consistent with
    /// the scale in force.
    #[gpui::test]
    fn the_painted_width_asks_for_a_scale_in_quarter_steps_once_per_cooldown(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, mut rx) = view(cx);
        view.update(cx, |v, _| {
            assert_eq!(v.native(), (800.0, 600.0));
            assert_eq!(v.a11y_label(), "Remote display 2");
            // Pinned here, not left to `new`: a loaded machine can spend the cooldown before
            // the ask.
            v.quality_changed = Instant::now();
            v.set_painted_width(300.0);
        });
        assert!(sent(&mut rx).is_empty(), "within the cooldown: nothing asked");
        view.update(cx, |v, _| {
            v.quality_changed = past_cooldown();
            v.set_painted_width(300.0);
            assert_eq!(v.size(), (400, 300), "300/800 = 0.375 → the 0.5 bucket");
        });
        let asked = sent(&mut rx);
        assert!(
            matches!(asked.as_slice(), [ScreenRequest::SetQuality { stream: StreamId(4), quality }] if (quality.scale - 0.5).abs() < f32::EPSILON),
            "{asked:?}"
        );
        view.update(cx, |v, _| {
            v.quality_changed = past_cooldown();
            v.set_painted_width(10.0);
            assert_eq!(v.size(), (200, 150), "never below the minimum scale");
            v.quality_changed = past_cooldown();
            v.set_painted_width(10.0);
        });
        assert_eq!(sent(&mut rx).len(), 1, "the same bucket again asks nothing");
        view.update(cx, |v, _| {
            v.set_geometry(100, 50);
            assert_eq!(v.size(), (100, 50));
            assert_eq!(v.native(), (400.0, 200.0), "native follows the scale in force (0.25)");
        });
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
        let pending = std::rc::Rc::new(std::cell::Cell::new(true));
        let once = std::rc::Rc::clone(&pending);
        view.update(cx, |v, _| {
            v.set_paste_hook(std::rc::Rc::new(move || {
                once.replace(false).then(|| {
                    let origin = Peer::Client(slopty_core::ClientId::new());
                    let offer = Offer { origin, generation: 1, items: Vec::new() };
                    ClientMsg::Clip(ClipMsg::Offer(offer))
                })
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
        let (out, rx) = mpsc::channel(64);
        let opened = Opened {
            stream: StreamId(4),
            target: CaptureTarget::Display(2),
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
    /// `CGEvent` needs; ⌘-scroll is the canvas's zoom and sends nothing.
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
        assert_eq!(got.len(), 5, "{got:?}");
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

        // Off the picture the pointer belongs to another card: a move there sends nothing.
        cx.simulate_mouse_move(
            point(bounds.origin.x - px(5.0), bounds.origin.y - px(5.0)),
            None,
            Modifiers::default(),
        );
        cx.run_until_parked();
        assert!(inputs(&mut rx).is_empty());
    }
}
