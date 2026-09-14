//! Remote window streaming: one pipeline per open stream.
//!
//! ```text
//! ScreenCaptureKit queue ──frame──▶ encoder.encode ──VideoToolbox thread──▶ packetize ──▶ queue
//!                                                                                          │
//!                              transport (hostd) drains the queue into QUIC datagrams ◀─────┘
//! ```
//!
//! Nothing here touches the network: datagrams go into a bounded queue the transport owns, and
//! everything the client sends back (reports, NACKs, refresh requests, quality changes) lands on
//! [`ScreenStream`] methods. When the queue is full the capture callback drops whole frames
//! rather than letting latency build up; the client notices the gap and asks for a refresh.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use bytes::Bytes;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use slopty_capture::{
    Capture, CaptureConfig, CaptureError, CapturedAudio, CapturedFrame, Crop, HideWatch,
    PixelFormat, Rect, Shareable, Target, host_now_us,
};
use slopty_codec::audio::OpusEncoder;
use slopty_codec::{CodecError, EncodedPacket, Encoder, EncoderConfig, FrameOptions};
use slopty_core::StreamId;
use slopty_input::{Injector, InputError};
pub use slopty_media::PathSample;
use slopty_media::{
    Cadence, Decision, EncodedFrame, HEARTBEAT_AFTER, MediaError, Packetizer, RateController,
    Redundancy, audio_datagram, cursor_datagram, frame_due, heartbeat_datagram,
};
use slopty_proto::media::MAX_DATAGRAM;
use slopty_proto::screen::{
    CaptureTarget, CursorShape, Quality, ReceiverReport, ScreenEvent, ScreenInput, SourceState,
    VideoCodec,
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

/// Datagrams buffered between the pipeline and the transport. At 1.2 kB each this is a few
/// 4K keyframes; the capture callback starts dropping frames when fewer than
/// `LOW_WATER` slots are free.
pub const DATAGRAM_QUEUE: usize = 4096;
/// Free queue slots below which a captured frame is dropped instead of encoded.
const LOW_WATER: usize = 256;

/// A datagram in the queue between the pipeline and the transport, stamped with when it was
/// put there.
///
/// The pump reads the stamp on the way out: the difference is the time the datagram spent
/// waiting for the pump to run, which no turn timing can see for a queue that was empty when
/// the pump last looked.
#[derive(Clone, Debug)]
pub struct Queued {
    /// The datagram to send.
    pub datagram: Bytes,
    /// `host_now_us()` when it was queued.
    pub queued_at_us: u64,
}

impl Queued {
    /// How long this datagram has been in the queue, microseconds.
    #[must_use]
    pub fn waited_us(&self) -> u64 {
        host_now_us().saturating_sub(self.queued_at_us)
    }
}
/// Cursor sample period (120 Hz); a datagram goes out only when the position changed.
const CURSOR_PERIOD: Duration = Duration::from_micros(8_333);
/// Cursor ticks between re-reads of the target's bounds (10 Hz).
const BOUNDS_EVERY: u64 = 12;
/// How often the cursor's picture is read while the pointer is over the target (30 Hz). It
/// goes to the client only when it changed.
const SHAPE_PERIOD: Duration = Duration::from_millis(33);

/// What a stream tells its owner outside the datagram path.
#[derive(Debug)]
pub enum StreamEvent {
    /// Capture ended (ScreenCaptureKit stopped it, or the cropped window closed).
    Stopped(CaptureError),
    /// The host's cursor picture changed while the pointer was over the target.
    Cursor(CursorShape),
}

/// How long a window may not be served as a crop after the accessibility API says a window of
/// its application went away, if the window list has not confirmed it by then.
///
/// The accessibility signal cannot name the window, so it is a suspicion; the window list is
/// the confirmation and it lags the order-out by 256–266 ms (MEASUREMENTS.md, "which signal
/// knows first"), read on a geometry tick every `BOUNDS_EVERY × CURSOR_PERIOD` ≈ 100 ms. The
/// watch matches the target to its element once, so another window of the application going
/// is not a suspicion (`ScreenStats::siblings`); only when it could not match — the
/// application does not list the window — does any window's going cost a freeze this long. A
/// true suspicion is confirmed inside the hold and never leaks a frame of the desktop.
pub const SUSPICION_HOLD: Duration = Duration::from_millis(400);

/// Screen pipeline errors.
#[derive(Debug, thiserror::Error)]
pub enum ScreenError {
    /// ScreenCaptureKit.
    #[error(transparent)]
    Capture(#[from] CaptureError),
    /// VideoToolbox.
    #[error(transparent)]
    Codec(#[from] CodecError),
    /// Packetizer.
    #[error(transparent)]
    Media(#[from] MediaError),
    /// Event injection.
    #[error(transparent)]
    Input(#[from] InputError),
    /// The accessibility API would not resize the window.
    #[error(transparent)]
    Resize(#[from] slopty_capture::AxError),
    /// The window is gone from the window list.
    #[error("the window is gone")]
    WindowGone,
    /// A callback-based framework call never completed.
    #[error("screen pipeline closed")]
    Closed,
}

/// What the transport reports about the connection, shared by every stream on it.
///
/// The bytes one datagram may carry (starts at the protocol maximum; lowered while QUIC's
/// path MTU is still 1200 bytes), how many bytes of datagrams QUIC is holding in its send
/// buffer waiting for the congestion window, and how wide that window is.
#[derive(Clone, Debug)]
pub struct DatagramBudget {
    max_datagram: Arc<AtomicUsize>,
    held: Arc<AtomicUsize>,
    cwnd: Arc<AtomicU64>,
}

impl Default for DatagramBudget {
    fn default() -> Self {
        Self::new()
    }
}

impl DatagramBudget {
    /// Protocol maximum, nothing held, no window sampled yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_datagram: Arc::new(AtomicUsize::new(MAX_DATAGRAM)),
            held: Arc::new(AtomicUsize::new(0)),
            cwnd: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Record what the path carries.
    pub fn set(&self, bytes: usize) {
        self.max_datagram.store(bytes, Ordering::Relaxed);
    }

    /// Current budget.
    #[must_use]
    pub fn get(&self) -> usize {
        self.max_datagram.load(Ordering::Relaxed)
    }

    /// Record how many bytes QUIC is holding back right now.
    pub fn set_held(&self, bytes: usize) {
        self.held.store(bytes, Ordering::Relaxed);
    }

    /// Bytes QUIC is holding back, as last reported.
    #[must_use]
    pub fn held(&self) -> usize {
        self.held.load(Ordering::Relaxed)
    }

    /// Record the selected path's congestion window.
    pub fn set_cwnd(&self, bytes: u64) {
        self.cwnd.store(bytes, Ordering::Relaxed);
    }

    /// The congestion window as last sampled; `0` until the pump has held something.
    #[must_use]
    pub fn cwnd(&self) -> u64 {
        self.cwnd.load(Ordering::Relaxed)
    }
}

/// Frames' worth of bytes (at the current target rate) QUIC may hold before a captured frame
/// is dropped instead of encoded.
///
/// The datagram send buffer is 4 MiB; a link whose window collapsed held 275–365 KB of
/// frames for 4–7 s and delivered every one of them stale (MEASUREMENTS.md, "start-up over
/// the mesh"). Two frames keep a keyframe's tail flowing and stop the queue there: the next
/// capture is fresher than anything that would wait behind it.
const HELD_FRAMES: u64 = 2;

/// Whether a captured frame should be encoded given what is queued ahead of it.
///
/// `free` slots in the datagram queue, `held` bytes in QUIC's send buffer against a congestion
/// window of `cwnd`, at `target_bps` and `fps`.
///
/// Two frames' worth is a *time* budget written in bytes, so it has to be recomputed from the
/// rate actually in force: at 30 Mbit/s and 60 fps it is 125 KB, at the 1 Mbit/s floor it is
/// 4 KB. A fixed byte floor under it turns into a fixed queue of *seconds* as the path slows —
/// 32 KB is 33 ms at 8 Mbit/s and 260 ms at 1 Mbit/s, charged to every frame behind it
/// (MEASUREMENTS.md and `docs/decisions/transport.md`, "the two controllers fail in opposite
/// ways": holds peaking at the floor plus one frame with the guard dropping throughout).
///
/// One congestion window is the floor instead. Bytes inside the window are not a standing
/// queue — QUIC sends them on the next acknowledgement — so refusing a frame below `cwnd`
/// drops one the link would have carried. That case is real where the round trip is long:
/// `per_frame × 2` falls under one window once `rtt × fps` passes 1.8.
#[must_use]
pub const fn frame_fits(free: usize, held: usize, cwnd: u64, target_bps: u64, fps: u16) -> bool {
    if free < LOW_WATER {
        return false;
    }
    let fps = if fps == 0 { 1 } else { fps as u64 };
    let per_frame = match (target_bps / 8).checked_div(fps) {
        Some(bytes) => bytes,
        None => 0,
    };
    let frames = per_frame.saturating_mul(HELD_FRAMES);
    let limit = if frames < cwnd { cwnd } else { frames };
    (held as u64) <= limit
}

/// How long the link may take to drain a keyframe before one is deferred instead of encoded.
///
/// The encoder sizes a keyframe from the picture and the average rate, never from what the path
/// can carry right now, so a collapsed link is handed one that does not fit. The shaped ladder's
/// keyframes ran 87–134 kB (MEASUREMENTS.md, 2026-09-15), and 134 kB at the 1 Mbit/s floor is
/// 1.07 s of link in a single frame. [`frame_fits`] cannot reach it: that gate runs before the
/// encode, so it bounds what is queued *ahead* of a frame and never the frame itself.
const KEYFRAME_DRAIN_MS: u64 = 400;

/// How long a keyframe may be deferred before it goes out whatever it costs.
///
/// The safety valve. Without one a link that never recovers never gets a keyframe and the client
/// sits on a hole for good, which is worse than the stall the deferral prevents. A second bounds
/// the wait against the 4 796 ms hold that motivated the rule, and a picture goes out meanwhile:
/// deferral only happens when an LTR refresh is available to send instead.
const KEYFRAME_VALVE_US: u64 = 1_000_000;

/// Whether a keyframe of `estimate` bytes should be encoded now.
///
/// `held` bytes are in QUIC's send buffer already and the link is running at `target_bps`. True
/// while the two together drain inside 400 ms, and true whenever there is no estimate yet: the
/// first keyframe of a stream is the one the client has no picture without.
#[must_use]
pub const fn keyframe_fits(estimate: u64, held: usize, target_bps: u64) -> bool {
    if estimate == 0 {
        return true;
    }
    let drainable = (target_bps / 8).saturating_mul(KEYFRAME_DRAIN_MS) / 1000;
    estimate.saturating_add(held as u64) <= drainable
}

/// Fold an encoded keyframe's size into the running estimate: up at once, down by an eighth.
///
/// Rising immediately is what makes the rule safe — one cheap keyframe (a blank screen, a
/// scrolled-off window) must not license the next expensive one — and decaying at all is what
/// lets a stream whose picture genuinely got simpler stop deferring.
#[must_use]
pub const fn keyframe_estimate(previous: u64, observed: u64) -> u64 {
    if observed >= previous {
        return observed;
    }
    previous.saturating_sub(previous / 8).saturating_add(observed / 8)
}

/// p50 / p95 / max of a latency over the last `LATENCY_WINDOW` samples, microseconds.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Quantiles {
    /// Samples in the window.
    pub n: u32,
    /// Median.
    pub p50_us: u64,
    /// 95th percentile.
    pub p95_us: u64,
    /// Worst in the window.
    pub max_us: u64,
}

impl Quantiles {
    /// Quantiles of `samples` (any order).
    #[must_use]
    pub fn of(samples: &[u64]) -> Self {
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let last = sorted.len().saturating_sub(1);
        let at = |q: f64| -> u64 {
            #[expect(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "an index below 2^53"
            )]
            let i = (last as f64 * q).round() as usize;
            sorted.get(i.min(last)).copied().unwrap_or(0)
        };
        Self {
            n: u32::try_from(sorted.len()).unwrap_or(u32::MAX),
            p50_us: at(0.5),
            p95_us: at(0.95),
            max_us: sorted.last().copied().unwrap_or(0),
        }
    }

    /// `p50 / p95 / max ms (n)`.
    #[must_use]
    pub fn describe(&self) -> String {
        #[expect(clippy::cast_precision_loss, reason = "microseconds well below 2^53")]
        let ms = |us: u64| us as f64 / 1e3;
        format!(
            "{:.2} / {:.2} / {:.2} ms (n={})",
            ms(self.p50_us),
            ms(self.p95_us),
            ms(self.max_us),
            self.n
        )
    }
}

/// Samples the latency quantiles are computed over: 10 s at 60 fps.
const LATENCY_WINDOW: usize = 600;

/// A ring of the last [`LATENCY_WINDOW`] latency samples.
#[derive(Debug, Default)]
struct LatencyRing(VecDeque<u64>);

impl LatencyRing {
    fn push(&mut self, us: u64) {
        if self.0.len() >= LATENCY_WINDOW {
            self.0.pop_front();
        }
        self.0.push_back(us);
    }

    fn quantiles(&self) -> Quantiles {
        let (a, b) = self.0.as_slices();
        let mut all = Vec::with_capacity(a.len().saturating_add(b.len()));
        all.extend_from_slice(a);
        all.extend_from_slice(b);
        Quantiles::of(&all)
    }
}

/// Counters for logs and telemetry.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct ScreenStats {
    /// Frames ScreenCaptureKit delivered.
    pub captured: u64,
    /// Frames dropped because the datagram queue was nearly full.
    pub dropped: u64,
    /// Frames captured and thrown away because the target was not on screen: the picture was of
    /// whatever is behind it. Zero on a stream whose target never left the screen.
    pub withheld: u64,
    /// Frames held because the accessibility API had just said a window of the target's
    /// application went away and the window list had not yet answered (see
    /// [`SUSPICION_HOLD`]). Counted apart from [`Self::withheld`] so a hide shows where it was
    /// caught: here in the first ~260 ms, there once core graphics agrees.
    pub suspected: u64,
    /// Accessibility notifications that raised a suspicion: the target hidden, minimised or
    /// destroyed — or, when the watch could not match the target to its accessibility element,
    /// any window of its application.
    pub suspicions: u64,
    /// Accessibility notifications for a window of the target's application that was not the
    /// target (a sibling window, a pop-up, a tooltip) going away. Never a hold: each one is a
    /// round trip through the window filter, because the capture framework stalls on it
    /// (`Shared::filter_stalled`).
    pub siblings: u64,
    /// Encoded frames packetized.
    pub encoded: u64,
    /// Datagrams queued for the transport (data, parity, retransmits, cursor).
    pub datagrams: u64,
    /// Datagrams discarded because the queue was full.
    pub queue_full: u64,
    /// Heartbeats sent while the source was quiet.
    pub heartbeats: u64,
    /// Refresh requests the client sent for this stream (what the receiver's cap bounds).
    pub refreshes: u64,
    /// Times a wanted keyframe was put off because the link could not drain one, counted once
    /// per episode rather than per frame (see [`keyframe_fits`]). Each one sent an LTR refresh in
    /// its place and ended either when the link could carry the keyframe or when the one-second
    /// valve opened.
    pub keyframes_deferred: u64,
    /// Worst capture-to-packet latency seen, microseconds.
    pub latency_max_us: u64,
    /// Sum of capture-to-packet latencies, microseconds (divide by `encoded`).
    pub latency_sum_us: u64,
    /// Opus packets sent.
    pub audio_packets: u64,
    /// Bitrate the controller last asked the encoder for.
    pub bitrate_bps: u64,
    /// Capture latency: the window server's display time of a frame → ScreenCaptureKit's
    /// callback (what SCK adds), over the last `LATENCY_WINDOW` frames.
    pub capture: Quantiles,
    /// Encode latency: `VTCompressionSessionEncodeFrame` → the output callback.
    pub encode: Quantiles,
    /// Time between two heartbeats, over the last `LATENCY_WINDOW` of them. The beat is what
    /// tells the receiver the host is alive while nothing is being drawn, and the receiver calls
    /// a silence of `STALL_GAP` a stall, so this is the number that says whether the host is
    /// keeping its own promise.
    pub beat_gap: Quantiles,
    /// How long the window-geometry call in the cursor loop took, over the last
    /// `LATENCY_WINDOW` of them: the work the beat used to wait behind.
    pub bounds: Quantiles,
    /// The longest gap between two beats since the stream opened, microseconds. The quantiles
    /// above are over a sliding window of `LATENCY_WINDOW` beats — about twenty seconds — so
    /// a single late beat early in a long stream would be gone from them by the end. This is
    /// the one that cannot forget, and it is what a rule about the beat has to be written on.
    pub beat_gap_worst_us: u64,
    /// Frames sent from the display-crop path (a window served as a `sourceRect` of its display
    /// rather than through the window filter). Frames the crop delivered after it stopped holding
    /// the target are not among them — those are [`Self::withheld`].
    pub cropped: u64,
    /// Whether the stream is on the display-crop path *right now*. The counter above says how
    /// many frames came that way; this says where the next one will come from, which is what a
    /// test asking "did the crop go away when the window did" has to look at.
    pub on_crop: bool,
}

/// How long a stream that has drawn may go without a frame before the client is told the source
/// is idle again.
///
/// Longer than [`SOURCE_IDLE_AFTER`] on purpose: a target that draws slowly (a clock, a log that
/// scrolls once a second) would otherwise flap between the two states and spend a control message
/// on every change, and the receiver only needs to know before it starts asking for refreshes.
pub const SOURCE_QUIET_AFTER: Duration = Duration::from_secs(2);

/// What the client is told about the capture source, decided from the frames it has actually
/// produced lately rather than from whether it ever produced one.
///
/// The distinction matters because the receiver acts on it: while the source is idle it stops
/// asking for refreshes no refresh can answer, and it does not charge the silence to the link.
/// A latch on "has ever encoded a frame" gets the first answer right and every later one wrong —
/// a window that draws once and then hides, or is closed and left up, stays `Live` for the rest
/// of the stream.
#[derive(Clone, Copy, Debug)]
pub struct SourceTracker {
    /// The last state the client was told.
    reported: Option<SourceState>,
    /// `encoded` when a frame was last seen, and when that was.
    seen: u64,
    last_frame: Option<Instant>,
    opened_at: Instant,
}

impl SourceTracker {
    /// A tracker for a stream opened at `now`.
    #[must_use]
    pub const fn new(now: Instant) -> Self {
        Self { reported: None, seen: 0, last_frame: None, opened_at: now }
    }

    /// The state to report, or `None` when it has not changed (or is not yet knowable).
    ///
    /// `hidden` is the geometry tick's verdict on the target: a window that is not on screen
    /// cannot be drawing, whatever the frame counter says, and saying so at once is better than
    /// waiting out the quiet period.
    pub fn poll(&mut self, encoded: u64, hidden: bool, now: Instant) -> Option<SourceState> {
        if encoded > self.seen {
            self.seen = encoded;
            self.last_frame = Some(now);
        }
        let state = match self.last_frame {
            _ if hidden => SourceState::Idle,
            None if now.saturating_duration_since(self.opened_at) < SOURCE_IDLE_AFTER => {
                // Still inside the grace: say nothing rather than call a slow start idle.
                return None;
            }
            None => SourceState::Idle,
            Some(at) if now.saturating_duration_since(at) >= SOURCE_QUIET_AFTER => {
                SourceState::Idle
            }
            Some(_drew) => SourceState::Live,
        };
        (self.reported != Some(state)).then(|| {
            self.reported = Some(state);
            state
        })
    }
}

/// One stream as the control socket lists it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ScreenSummary {
    /// The client that opened it.
    pub client: String,
    /// Stream id on that connection.
    pub stream: u32,
    /// What it captures.
    pub target: CaptureTarget,
    /// Counters (final ones for a closed stream).
    pub stats: ScreenStats,
}

/// Closed streams the registry remembers.
const CLOSED_KEEP: usize = 8;

/// Every live stream in the daemon plus the last few closed ones, for the control socket.
#[derive(Clone, Default, Debug)]
pub struct Registry {
    inner: Arc<Mutex<RegistryInner>>,
    observer: Arc<Mutex<Observer>>,
}

#[derive(Default, Debug)]
struct RegistryInner {
    live: Vec<(String, CaptureTarget, StatsHandle)>,
    closed: VecDeque<ScreenSummary>,
}

/// Told the live-stream count after every change (the daemon's sleep policy listens). Called
/// with the registry's own lock held, so counts arrive in mutation order: two closes racing
/// an open could otherwise report `0` after `1` and release a hold with a stream still live.
/// The listener must therefore never call back into the registry.
#[derive(Default)]
struct Observer(Option<Box<dyn Fn(usize) + Send>>);

impl std::fmt::Debug for Observer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() { "Observer(set)" } else { "Observer(none)" })
    }
}

impl Registry {
    /// Call `f` with the number of live streams after every open and close.
    pub fn observe(&self, f: impl Fn(usize) + Send + 'static) {
        self.observer.lock().0 = Some(Box::new(f));
    }

    fn changed(&self, inner: &RegistryInner) {
        if let Some(f) = &self.observer.lock().0 {
            f(inner.live.len());
        }
    }

    /// Track a stream `client` opened.
    pub fn insert(
        &self,
        client: &impl std::fmt::Display,
        target: CaptureTarget,
        handle: StatsHandle,
    ) {
        let mut inner = self.inner.lock();
        inner.live.push((client.to_string(), target, handle));
        self.changed(&inner);
        drop(inner);
    }

    /// The stream closed: keep its final counters.
    pub fn remove(&self, client: &impl std::fmt::Display, id: StreamId) {
        let client = client.to_string();
        let mut inner = self.inner.lock();
        let Some(at) = inner.live.iter().position(|(c, _, h)| *c == client && h.id() == id) else {
            return;
        };
        let (client, target, handle) = inner.live.remove(at);
        if inner.closed.len() >= CLOSED_KEEP {
            inner.closed.pop_front();
        }
        inner.closed.push_back(ScreenSummary {
            client,
            stream: id.0,
            target,
            stats: handle.stats(),
        });
        self.changed(&inner);
        drop(inner);
    }

    /// Live streams with their counters right now, then the closed ones (oldest first).
    #[must_use]
    pub fn summaries(&self) -> (Vec<ScreenSummary>, Vec<ScreenSummary>) {
        let inner = self.inner.lock();
        let live = inner
            .live
            .iter()
            .map(|(client, target, handle)| ScreenSummary {
                client: client.clone(),
                stream: handle.id().0,
                target: *target,
                stats: handle.stats(),
            })
            .collect();
        (live, inner.closed.iter().cloned().collect())
    }
}

/// How long an enumeration of shareable content is reused. A client lists, picks and opens
/// within a few seconds, and `SCShareableContent` costs 60–75 ms per call (MEASUREMENTS.md,
/// "start-up on a cold connection"); the second call would return the same objects.
const SHAREABLE_TTL: Duration = Duration::from_secs(2);

/// The last enumeration and when it was taken.
static SHAREABLE: Mutex<Option<(Instant, Arc<Shareable>)>> = Mutex::new(None);

/// Enumerate shareable content, reusing an enumeration younger than `SHAREABLE_TTL`.
pub async fn shareable() -> Result<Arc<Shareable>, ScreenError> {
    if let Some((taken, content)) = SHAREABLE.lock().as_ref()
        && taken.elapsed() < SHAREABLE_TTL
    {
        return Ok(Arc::clone(content));
    }
    let (tx, rx) = oneshot::channel();
    slopty_capture::enumerate(move |result| {
        let _receiver_gone = tx.send(result);
    });
    let content = Arc::new(rx.await.map_err(|_dropped| ScreenError::Closed)??);
    *SHAREABLE.lock() = Some((Instant::now(), Arc::clone(&content)));
    Ok(content)
}

/// Start and stop one small capture of the first display; returns how long that took.
///
/// The first stream a client opens then does not pay ScreenCaptureKit's first start in this
/// process: ~300 ms cold against ~115 ms warm (MEASUREMENTS.md, "start-up on a cold
/// connection").
pub async fn warm_up() -> Result<Duration, ScreenError> {
    let started = Instant::now();
    let encoder = Encoder::new(
        EncoderConfig {
            width: 64,
            height: 64,
            codec: VideoCodec::Hevc,
            fps: 1,
            bitrate_bps: 100_000,
            rate_control: slopty_codec::RateControl::LowLatency,
        },
        |_packet| {},
    )?;
    drop(encoder);
    let content = shareable().await?;
    let display = content.displays().into_iter().next().ok_or(ScreenError::Closed)?;
    let resolved = Target::resolve(&content, CaptureTarget::Display(display.id))?;
    let config = CaptureConfig {
        width: 64,
        height: 64,
        fps: 1,
        format: PixelFormat::Nv12,
        queue_depth: 1,
        audio: true,
        crop: None,
    };
    let (tx, rx) = oneshot::channel();
    let capture = Capture::start(
        &resolved,
        &config,
        |_frame| {},
        Some(Box::new(|_chunk| {})),
        |_stopped| {},
        move |result| {
            let _receiver_gone = tx.send(result);
        },
    )?;
    rx.await.map_err(|_dropped| ScreenError::Closed)??;
    let (tx, rx) = oneshot::channel();
    capture.stop(move |result| {
        let _receiver_gone = tx.send(result);
    });
    let _stopped = rx.await;
    Ok(started.elapsed())
}

/// The `Listing` event for the current windows and displays.
pub async fn listing() -> Result<ScreenEvent, ScreenError> {
    let content = shareable().await?;
    Ok(ScreenEvent::Listing { windows: content.windows(), displays: content.displays() })
}

/// Requests folded into the next encoded frame.
#[derive(Default)]
struct Pending {
    keyframe: bool,
    refresh: bool,
    acked: Vec<u64>,
}

struct Counters {
    audio_packets: AtomicU64,
    captured: AtomicU64,
    dropped: AtomicU64,
    withheld: AtomicU64,
    suspected: AtomicU64,
    suspicions: AtomicU64,
    siblings: AtomicU64,
    encoded: AtomicU64,
    datagrams: AtomicU64,
    queue_full: AtomicU64,
    /// Heartbeats sent while the source was quiet.
    heartbeats: AtomicU64,
    /// Refresh requests received from the client.
    refreshes: AtomicU64,
    /// Deferral episodes, one per run of put-off keyframes.
    keyframes_deferred: AtomicU64,
    latency_max_us: AtomicU64,
    latency_sum_us: AtomicU64,
    bitrate_bps: AtomicU64,
    cropped: AtomicU64,
    capture: Mutex<LatencyRing>,
    encode: Mutex<LatencyRing>,
    beat_gap: Mutex<LatencyRing>,
    beat_gap_worst_us: AtomicU64,
    bounds: Mutex<LatencyRing>,
    /// `(pts, submitted at)` of frames inside the encoder, oldest first.
    in_flight: Mutex<VecDeque<(u64, u64)>>,
}

/// Frames the encoder may hold before the oldest submit record is forgotten (the encoder
/// runs with `MaxFrameDelayCount` 0, so this is only a bound against a callback that never
/// comes).
const IN_FLIGHT_MAX: usize = 16;

impl Counters {
    fn new() -> Self {
        Self {
            audio_packets: AtomicU64::new(0),
            captured: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            withheld: AtomicU64::new(0),
            suspected: AtomicU64::new(0),
            suspicions: AtomicU64::new(0),
            siblings: AtomicU64::new(0),
            encoded: AtomicU64::new(0),
            datagrams: AtomicU64::new(0),
            queue_full: AtomicU64::new(0),
            heartbeats: AtomicU64::new(0),
            refreshes: AtomicU64::new(0),
            keyframes_deferred: AtomicU64::new(0),
            latency_max_us: AtomicU64::new(0),
            latency_sum_us: AtomicU64::new(0),
            bitrate_bps: AtomicU64::new(0),
            cropped: AtomicU64::new(0),
            capture: Mutex::new(LatencyRing::default()),
            encode: Mutex::new(LatencyRing::default()),
            beat_gap: Mutex::new(LatencyRing::default()),
            beat_gap_worst_us: AtomicU64::new(0),
            bounds: Mutex::new(LatencyRing::default()),
            in_flight: Mutex::new(VecDeque::with_capacity(IN_FLIGHT_MAX)),
        }
    }

    /// A frame went into the encoder at `now`.
    fn submitted(&self, pts_us: u64, now: u64) {
        let mut in_flight = self.in_flight.lock();
        if in_flight.len() >= IN_FLIGHT_MAX {
            in_flight.pop_front();
        }
        in_flight.push_back((pts_us, now));
    }

    /// The encoder returned the frame with `pts_us` at `now`: record its encode latency.
    fn returned(&self, pts_us: u64, now: u64) {
        let submitted = {
            let mut in_flight = self.in_flight.lock();
            let at = in_flight.iter().position(|&(pts, _)| pts == pts_us);
            let found = at.and_then(|i| in_flight.remove(i)).map(|(_, at)| at);
            drop(in_flight);
            found
        };
        if let Some(at) = submitted {
            self.encode.lock().push(now.saturating_sub(at));
        }
    }

    fn snapshot(&self) -> ScreenStats {
        ScreenStats {
            captured: self.captured.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            withheld: self.withheld.load(Ordering::Relaxed),
            keyframes_deferred: self.keyframes_deferred.load(Ordering::Relaxed),
            suspected: self.suspected.load(Ordering::Relaxed),
            suspicions: self.suspicions.load(Ordering::Relaxed),
            siblings: self.siblings.load(Ordering::Relaxed),
            encoded: self.encoded.load(Ordering::Relaxed),
            datagrams: self.datagrams.load(Ordering::Relaxed),
            queue_full: self.queue_full.load(Ordering::Relaxed),
            heartbeats: self.heartbeats.load(Ordering::Relaxed),
            refreshes: self.refreshes.load(Ordering::Relaxed),
            latency_max_us: self.latency_max_us.load(Ordering::Relaxed),
            latency_sum_us: self.latency_sum_us.load(Ordering::Relaxed),
            audio_packets: self.audio_packets.load(Ordering::Relaxed),
            bitrate_bps: self.bitrate_bps.load(Ordering::Relaxed),
            capture: self.capture.lock().quantiles(),
            encode: self.encode.lock().quantiles(),
            beat_gap: self.beat_gap.lock().quantiles(),
            beat_gap_worst_us: self.beat_gap_worst_us.load(Ordering::Relaxed),
            bounds: self.bounds.lock().quantiles(),
            cropped: self.cropped.load(Ordering::Relaxed),
            // Which path is live is the stream's state, not a counter: `Shared::stats` fills it,
            // and nothing else may hand this snapshot out (it would claim the window filter).
            on_crop: false,
        }
    }
}

/// A handle on one stream's counters that outlives the [`ScreenStream`]'s owner borrow, for
/// the daemon's control socket (`slopty bench screen` reads the host side through it).
#[derive(Clone)]
pub struct StatsHandle(Arc<Shared>);

impl std::fmt::Debug for StatsHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatsHandle").field("stream", &self.0.id).finish()
    }
}

impl StatsHandle {
    /// The stream.
    #[must_use]
    pub fn id(&self) -> StreamId {
        self.0.id
    }

    /// Counters right now.
    #[must_use]
    pub fn stats(&self) -> ScreenStats {
        self.0.stats()
    }
}

/// Audio gate: after this long without a sample above [`AUDIO_FLOOR`] the stream stops
/// sending packets (silent apps cost nothing on the wire; the client pads silence).
const AUDIO_HOLD_US: u64 = 300_000;
/// Anything quieter than this is silence (-80 dBFS).
const AUDIO_FLOOR: f32 = 1e-4;

/// The encoder and the silence gate.
struct AudioState {
    encoder: Option<OpusEncoder>,
    seq: u32,
    last_loud_us: u64,
}

struct Shared {
    id: StreamId,
    encoder: RwLock<Option<Encoder>>,
    audio: Mutex<AudioState>,
    pending: Mutex<Pending>,
    packetizer: Mutex<Packetizer>,
    redundancy: Mutex<Redundancy>,
    rate: Mutex<RateController>,
    /// `datagrams_sent` at the previous receiver report.
    sent_at_report: AtomicU64,
    /// `host_now_us()` when the last datagram was queued; the heartbeat clock.
    last_push_us: AtomicU64,
    /// The cadence rung in force: how many of the captures reach the encoder, and the frame rate
    /// the held-bytes limit is computed from.
    fps: std::sync::atomic::AtomicU16,
    /// The cadence the client asked for; the ladder never climbs past it.
    fps_ceiling: std::sync::atomic::AtomicU16,
    /// `capture_ts_us` of the last frame handed to the encoder; the cadence gate's clock.
    last_encoded_us: AtomicU64,
    /// What a keyframe costs on this stream, as [`keyframe_estimate`] tracks it; `0` until one
    /// has been encoded.
    keyframe_bytes: AtomicU64,
    /// `host_now_us()` when the current run of deferrals began, `0` when none is running: the
    /// valve's clock.
    keyframe_deferred_us: AtomicU64,
    /// The client has acknowledged a long-term reference, so an LTR refresh is a picture it can
    /// decode and a keyframe is not the only way out of a hole. Nothing clears it: a client that
    /// goes away takes the stream with it.
    ltr_acked: std::sync::atomic::AtomicBool,
    /// Whether frames come through the display-crop path right now.
    cropped: std::sync::atomic::AtomicBool,
    /// The target window is not on screen. Nothing ScreenCaptureKit delivers can be a picture of
    /// it, so nothing is sent: under a display crop the rectangle holds whatever is behind the
    /// window, and a swap to the window filter that the framework rejected leaves the crop
    /// running while this side believes otherwise. Set from the geometry tick.
    target_hidden: std::sync::atomic::AtomicBool,
    /// The host's pointer is over the target, as the cursor loop last saw it; the shape loop
    /// reads the cursor's picture only while it is.
    pointer_over: std::sync::atomic::AtomicBool,
    /// `host_now_us()` until which frames are held on the accessibility API's word alone
    /// (zero: no suspicion). Set by the [`HideWatch`] callback, read on every frame; the
    /// geometry tick's `target_hidden` is the confirmation that outlives it.
    suspect_until_us: AtomicU64,
    /// A window of the target's application other than the target went since the filter was
    /// last changed. ScreenCaptureKit stops delivering for an application-scoped display filter
    /// when any window of that application is ordered out, and only a change of filter kind
    /// wakes it (MEASUREMENTS.md, "a sibling window closing stalls the crop"); the geometry
    /// tick takes a stream on the crop through the window filter and back.
    filter_stalled: std::sync::atomic::AtomicBool,
    out: mpsc::Sender<Queued>,
    budget: DatagramBudget,
    counters: Counters,
}

impl Shared {
    /// The counters plus the state only the stream knows: whether the display crop is what is
    /// being served right now. Every reader goes through here, so no caller can publish the
    /// snapshot's placeholder and report the window filter for a stream on the crop.
    fn stats(&self) -> ScreenStats {
        ScreenStats { on_crop: self.cropped.load(Ordering::Relaxed), ..self.counters.snapshot() }
    }

    /// Point the live encoder at `bps` (a rebuild picks the controller's target up again).
    fn apply_bitrate(&self, bps: u32) {
        let result = self.encoder.read().as_ref().map(|e| e.set_bitrate(bps));
        match result {
            Some(Ok(())) => {
                self.counters.bitrate_bps.store(u64::from(bps), Ordering::Relaxed);
                tracing::debug!(stream = %self.id, bps, "bitrate");
            }
            Some(Err(e)) => tracing::warn!(stream = %self.id, bps, error = %e, "set bitrate"),
            None => {}
        }
    }

    /// Move the cadence to the rung `bps` affords, and tell the encoder it did.
    ///
    /// Capture is left at the ceiling either way: a change on screen is still seen within a display
    /// beat, only fewer of those captures are encoded, so the picture that does go out is worth its
    /// bandwidth (`docs/decisions/video.md`, the cadence ladder).
    fn apply_cadence(&self, bps: u32) {
        let ceiling = self.fps_ceiling.load(Ordering::Relaxed);
        let mut cadence = Cadence::resume(ceiling, self.fps.load(Ordering::Relaxed));
        let Some(fps) = cadence.update(bps) else { return };
        self.fps.store(fps, Ordering::Relaxed);
        let result = self.encoder.read().as_ref().map(|e| e.set_frame_rate(fps));
        if let Some(Err(e)) = result {
            tracing::warn!(stream = %self.id, fps, error = %e, "set frame rate");
        }
        tracing::debug!(stream = %self.id, fps, bps, "cadence");
    }

    /// Queue a datagram; a full queue drops it (video is unreliable by design).
    fn push(&self, datagram: Bytes) -> bool {
        let queued_at_us = host_now_us();
        match self.out.try_send(Queued { datagram, queued_at_us }) {
            Ok(()) => {
                self.counters.datagrams.fetch_add(1, Ordering::Relaxed);
                self.last_push_us.store(queued_at_us, Ordering::Relaxed);
                true
            }
            Err(_full_or_closed) => {
                self.counters.queue_full.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// The accessibility API says a window of the target's application went at `now`: hold
    /// frames for [`SUSPICION_HOLD`] while the window list catches up; the geometry tick moves
    /// the stream to the window filter meanwhile.
    fn suspect(&self, now: u64) {
        let hold_us = u64::try_from(SUSPICION_HOLD.as_micros()).unwrap_or(u64::MAX);
        self.suspect_until_us.store(now.saturating_add(hold_us), Ordering::Relaxed);
        let suspicions = self.counters.suspicions.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(stream = %self.id, suspicions = suspicions.saturating_add(1), "hide suspected");
    }

    /// The accessibility API says a window of the target's application that is not the target
    /// went: no hold, but the capture may have stalled on it, so the next geometry tick takes
    /// the stream through the window filter.
    fn sibling_went(&self) {
        self.filter_stalled.store(true, Ordering::Relaxed);
        let siblings = self.counters.siblings.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(stream = %self.id, siblings = siblings.saturating_add(1), "another window of the application went");
    }

    /// Whether a suspicion raised by [`Self::suspect`] is still holding frames at `now`.
    fn suspected_at(&self, now: u64) -> bool {
        now < self.suspect_until_us.load(Ordering::Relaxed)
    }

    /// ScreenCaptureKit delivered a frame.
    fn on_frame(&self, frame: &CapturedFrame) {
        self.counters.captured.fetch_add(1, Ordering::Relaxed);
        self.counters.capture.lock().push(frame.latency_us);
        // Before anything is counted as a picture of the target: while the window is off screen
        // no frame can be one, and a display crop keeps delivering the desktop behind it
        // (MEASUREMENTS.md, "a hidden window on the crop path").
        if self.target_hidden.load(Ordering::Relaxed) {
            self.counters.withheld.fetch_add(1, Ordering::Relaxed);
            return;
        }
        // And for the ~260 ms before the window list knows, the accessibility API's word: a
        // window of that application just went, so a crop may be the desktop already. The
        // geometry tick moves a suspected stream to the window filter meanwhile, but its first
        // frames are held too: ScreenCaptureKit still delivers a frame or two of the old filter
        // after the swap has settled, and with the swap landing before the window list knows,
        // those showed the backdrop (MEASUREMENTS.md, "a sibling window closing stalls the
        // crop"). Nothing is sent for the hold, whichever path it arrives on.
        if self.suspected_at(host_now_us()) {
            self.counters.suspected.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if self.cropped.load(Ordering::Relaxed) {
            self.counters.cropped.fetch_add(1, Ordering::Relaxed);
        }
        // The cadence rung, before the congestion guard: a capture the ladder is not asking for is
        // not a frame the link failed to carry, and skipping it is what gives the next one the
        // bytes to be worth sending. No refresh is owed, the client is missing nothing.
        // A keyframe is the one thing the rung does not hold back: it is what a client with no
        // picture at all is waiting on, and there is at most one in flight. A pending *refresh* is
        // not urgent in the same way — at a collapsed rate the guard below sets one on every frame
        // it drops, and letting those through would take the cadence off exactly where it is
        // needed.
        let since_encoded =
            frame.capture_ts_us.saturating_sub(self.last_encoded_us.load(Ordering::Relaxed));
        let due = frame_due(since_encoded, self.fps.load(Ordering::Relaxed));
        let want_keyframe = self.pending.lock().keyframe;
        if !due && !want_keyframe {
            return;
        }
        let fits = frame_fits(
            self.out.capacity(),
            self.budget.held(),
            self.budget.cwnd(),
            self.counters.bitrate_bps.load(Ordering::Relaxed),
            self.fps.load(Ordering::Relaxed),
        );
        if !fits {
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            // The client will see a hole; make the next frame decodable on its own.
            self.pending.lock().refresh = true;
            return;
        }
        // A keyframe the link cannot drain is put off and an LTR refresh encoded in its place: a
        // picture the client can decode, at a fraction of the bytes. The request stays pending, so
        // the keyframe follows as soon as the link can carry one or the valve opens. With it
        // deferred there is nothing urgent in this frame, so the cadence rung applies again.
        let defer = want_keyframe && !self.keyframe_admitted(host_now_us());
        if defer && !due {
            return;
        }
        let options = {
            let mut pending = self.pending.lock();
            let force_keyframe = if defer { false } else { std::mem::take(&mut pending.keyframe) };
            FrameOptions {
                force_keyframe,
                force_ltr_refresh: std::mem::take(&mut pending.refresh) || defer,
                acked_ltr: std::mem::take(&mut pending.acked),
            }
        };
        self.last_encoded_us.store(frame.capture_ts_us, Ordering::Relaxed);
        self.counters.submitted(frame.capture_ts_us, host_now_us());
        let outcome = self
            .encoder
            .read()
            .as_ref()
            .map(|encoder| encoder.encode(frame.image.as_cv(), frame.capture_ts_us, &options));
        if let Some(Err(e)) = outcome {
            self.counters.returned(frame.capture_ts_us, host_now_us());
            tracing::warn!(stream = %self.id, error = %e, "encode failed");
            let mut pending = self.pending.lock();
            pending.keyframe |= options.force_keyframe;
            pending.refresh |= options.force_ltr_refresh;
            pending.acked.extend(options.acked_ltr);
        }
    }

    /// ScreenCaptureKit delivered PCM: encode and send unless the source has gone quiet.
    fn on_audio(&self, chunk: &CapturedAudio) {
        let now = host_now_us();
        let Some(packets) = self.encode_audio(&chunk.samples, now) else { return };
        for (seq, packet) in packets {
            if let Some(datagram) = audio_datagram(self.id, seq, send_ms_lo(now), &packet)
                && self.push(datagram)
            {
                self.counters.audio_packets.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Run the silence gate and the encoder under the audio lock; `None` when nothing goes out.
    fn encode_audio(&self, samples: &[f32], now: u64) -> Option<Vec<(u32, Bytes)>> {
        let loud = samples.iter().any(|s| s.abs() > AUDIO_FLOOR);
        let mut audio = self.audio.lock();
        if loud {
            audio.last_loud_us = now;
        } else if now.saturating_sub(audio.last_loud_us) > AUDIO_HOLD_US {
            return None;
        }
        if audio.encoder.is_none() {
            match OpusEncoder::new() {
                Ok(encoder) => audio.encoder = Some(encoder),
                Err(e) => {
                    tracing::warn!(stream = %self.id, error = %e, "no Opus encoder; audio off");
                    // Never retried: keep the gate closed for good.
                    audio.last_loud_us = 0;
                    return None;
                }
            }
        }
        let AudioState { encoder: Some(encoder), seq, .. } = &mut *audio else { return None };
        let mut packets = Vec::new();
        let encoded = encoder.push(samples, |packet| {
            *seq = seq.wrapping_add(1);
            packets.push((*seq, Bytes::copy_from_slice(packet)));
        });
        drop(audio);
        match encoded {
            Ok(()) => Some(packets),
            Err(e) => {
                tracing::debug!(stream = %self.id, error = %e, "opus encode");
                None
            }
        }
    }

    /// VideoToolbox produced an access unit.
    fn on_packet(&self, packet: &EncodedPacket) {
        let now = host_now_us();
        self.counters.returned(packet.pts_us, now);
        let latency = now.saturating_sub(packet.pts_us);
        let encoded = self.counters.encoded.fetch_add(1, Ordering::Relaxed);
        if packet.keyframe {
            let bytes = u64::try_from(packet.data.len()).unwrap_or(u64::MAX);
            let estimate = keyframe_estimate(self.keyframe_bytes.load(Ordering::Relaxed), bytes);
            self.keyframe_bytes.store(estimate, Ordering::Relaxed);
            self.keyframe_deferred_us.store(0, Ordering::Relaxed);
        }
        if encoded == 0 || packet.keyframe {
            tracing::debug!(
                stream = %self.id,
                frame = encoded,
                bytes = packet.data.len(),
                keyframe = packet.keyframe,
                encode_ms = latency / 1000,
                "keyframe encoded"
            );
        }
        self.counters.latency_max_us.fetch_max(latency, Ordering::Relaxed);
        self.counters.latency_sum_us.fetch_add(latency, Ordering::Relaxed);
        #[expect(clippy::cast_possible_truncation, reason = "low bits by design")]
        let capture_ts_us = packet.pts_us as u32;
        let frame = EncodedFrame {
            data: &packet.data,
            keyframe: packet.keyframe,
            ltr_token: packet.ltr_token,
            ltr_refresh: packet.ltr_refresh,
            capture_ts_us,
        };
        let mut packetizer = self.packetizer.lock();
        packetizer.set_max_datagram(self.budget.get());
        match packetizer.packetize(&frame, send_ms_lo(now)) {
            Ok(sent) => {
                for datagram in &sent.datagrams {
                    if !self.push(datagram.clone()) {
                        break;
                    }
                }
            }
            Err(e) => tracing::warn!(stream = %self.id, error = %e, "packetize failed"),
        }
    }
}

impl Shared {
    /// A receiver report: acknowledged LTR tokens go to the encoder, the loss to the parity
    /// and rate controllers; a changed target is applied to the encoder.
    fn report(&self, report: &ReceiverReport, path: Option<PathSample>) -> Option<Decision> {
        let acked = report.acked_ltr.iter().take(usize::from(report.acked_ltr_len)).copied();
        self.pending.lock().acked.extend(acked);
        if report.acked_ltr_len > 0 {
            // From here a refresh is a picture, so a keyframe can be deferred
            // (`keyframe_admitted`).
            self.ltr_acked.store(true, Ordering::Relaxed);
        }
        let sent_total = self.packetizer.lock().datagrams_sent();
        let previous = self.sent_at_report.swap(sent_total, Ordering::Relaxed);
        let sent = u32::try_from(sent_total.saturating_sub(previous)).unwrap_or(u32::MAX);
        let permille = self.redundancy.lock().on_report(report, sent);
        self.packetizer.lock().set_parity_permille(permille);
        let decision = self.rate.lock().on_report(report, sent, path)?;
        tracing::debug!(
            stream = %self.id,
            verdict = ?decision.verdict,
            target_bps = decision.target_bps,
            capped = decision.capped,
            loss_permille = decision.window.loss_permille(),
            queue_max = decision.window.queue_max,
            hold_max_ms = decision.window.hold_max.as_millis(),
            stalled_ms = decision.window.stalled_ms,
            stalls = decision.window.stalls,
            "rate decision"
        );
        if decision.changed {
            self.apply_bitrate(decision.target_bps);
            self.apply_cadence(decision.target_bps);
        }
        Some(decision)
    }

    /// Whether a wanted keyframe goes into the frame being encoded now.
    ///
    /// Deferring needs somewhere to fall back to: with no acknowledged long-term reference the
    /// client can decode nothing but a keyframe, so one always goes out. Past that the estimate
    /// decides, and [`KEYFRAME_VALVE_US`] after the run began it goes out regardless.
    fn keyframe_admitted(&self, now: u64) -> bool {
        if !self.ltr_acked.load(Ordering::Relaxed) {
            return true;
        }
        if keyframe_fits(
            self.keyframe_bytes.load(Ordering::Relaxed),
            self.budget.held(),
            self.counters.bitrate_bps.load(Ordering::Relaxed),
        ) {
            return true;
        }
        // Counted where the clock starts, so the number is deferral episodes and not the frames
        // each one spans — at 60 fps a one-second run would otherwise read as sixty.
        match self.keyframe_deferred_us.compare_exchange(
            0,
            now,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_started) => {
                self.counters.keyframes_deferred.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    stream = %self.id,
                    estimate = self.keyframe_bytes.load(Ordering::Relaxed),
                    held = self.budget.held(),
                    target_bps = self.counters.bitrate_bps.load(Ordering::Relaxed),
                    "keyframe deferred; sending an LTR refresh instead"
                );
                false
            }
            Err(started) => now.saturating_sub(started) >= KEYFRAME_VALVE_US,
        }
    }

    /// The client lost a frame it cannot recover: make the next frame stand on its own.
    fn request_refresh(&self, last_good_frame: u32) {
        tracing::debug!(stream = %self.id, last_good_frame, "refresh requested");
        self.counters.refreshes.fetch_add(1, Ordering::Relaxed);
        self.pending.lock().refresh = true;
    }

    /// Retransmit fragments of a recent frame, unless the transport is holding more than the
    /// frame budget (see [`ScreenStream::nack`]).
    fn nack(&self, frame: u32, fragments: &[u16]) {
        let fits = frame_fits(
            self.out.capacity(),
            self.budget.held(),
            self.budget.cwnd(),
            self.counters.bitrate_bps.load(Ordering::Relaxed),
            self.fps.load(Ordering::Relaxed),
        );
        if !fits {
            tracing::debug!(stream = %self.id, frame, held = self.budget.held(), "nack not answered: transport is holding frames");
            return;
        }
        let datagrams = self.packetizer.lock().retransmit(frame, fragments);
        if datagrams.is_empty() {
            tracing::debug!(stream = %self.id, frame, "nack for a frame outside the history");
        }
        for datagram in datagrams {
            if !self.push(datagram) {
                break;
            }
        }
    }
}

/// Low byte of the host millisecond clock.
const fn send_ms_lo(now_us: u64) -> u8 {
    #[expect(clippy::cast_possible_truncation, reason = "low byte by design")]
    let lo = (now_us / 1000) as u8;
    lo
}

/// Build an encoder whose packets flow back into `shared`.
fn build_encoder(shared: &Weak<Shared>, config: EncoderConfig) -> Result<Encoder, CodecError> {
    let weak = Weak::clone(shared);
    Encoder::new(config, move |packet| {
        if let Some(shared) = weak.upgrade() {
            shared.on_packet(&packet);
        }
    })
}

/// How a window target is served by ScreenCaptureKit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WindowPath {
    /// `SCContentFilter(desktopIndependentWindow:)`: the window alone, wherever it is.
    Filter,
    /// The display filter with `sourceRect` at the window's frame: the composited desktop
    /// cropped, which ScreenCaptureKit serves without the per-window pass.
    DisplayCrop,
}

/// Whether windows are served as a crop of their display when they are entirely on one and
/// nothing of another process overlaps them. `SLOPTY_WINDOW_CAPTURE=window|crop` overrides
/// it (the measurement and support knob).
const CROP_WINDOWS: bool = true;

fn crop_windows() -> bool {
    crop_windows_from(std::env::var("SLOPTY_WINDOW_CAPTURE").ok().as_deref())
}

/// [`crop_windows`] for the knob's value: an unknown or missing value is the default.
fn crop_windows_from(knob: Option<&str>) -> bool {
    match knob {
        Some("window") => false,
        Some("crop") => true,
        _other => CROP_WINDOWS,
    }
}

/// The rule of the display-crop path.
///
/// A window is served as a crop of its display only while it is on screen (not minimised,
/// not on another Space), entirely on one display (`crop` is the geometry's answer) and
/// nothing counts as covering it. Anything else is the window filter, which shows the
/// window and nothing but the window wherever it is.
#[must_use]
pub const fn crop_allowed(on_screen: bool, crop: Option<Crop>, occluded: bool) -> Option<Crop> {
    if on_screen && !occluded { crop } else { None }
}

/// The crop a window wants right now, or `None` when it must go through the window filter.
fn wanted_crop(
    id: slopty_core::WindowId,
    bounds: &Rect,
    point_scale: f64,
    on_screen: bool,
) -> Option<Crop> {
    let crop = slopty_capture::display_enclosing(bounds).and_then(|display| {
        let display = slopty_capture::display_bounds(display);
        slopty_capture::crop_for(bounds, &display, point_scale).map(|(crop, _pixels)| crop)
    });
    let owner = slopty_capture::window_owner_pid(id)?;
    let occluded = slopty_capture::occluded(id, bounds, owner);
    crop_allowed(on_screen, crop, occluded)
}

/// Resolve a target, choosing the path for a window.
fn resolve(
    content: &Shareable,
    target: CaptureTarget,
) -> Result<(Target, WindowPath), ScreenError> {
    if let CaptureTarget::Window(id) = target
        && crop_windows()
        && let Some(bounds) = slopty_capture::window_bounds(id)
        && let Some(owner) = slopty_capture::window_owner_pid(id)
        && let Some(candidate) = Target::resolve_crop(content, id)?
    {
        let on_screen = slopty_capture::window_on_screen(id);
        let occluded = slopty_capture::occluded(id, &bounds, owner);
        if crop_allowed(on_screen, candidate.crop(), occluded).is_some() {
            return Ok((candidate, WindowPath::DisplayCrop));
        }
        tracing::debug!(%id, on_screen, occluded, crop = ?candidate.crop(), "window filter");
    }
    Ok((Target::resolve(content, target)?, WindowPath::Filter))
}

/// What the stream is asking ScreenCaptureKit to become: a path and crop that are committed
/// only once every asynchronous call for them has completed without error. Shared with the
/// completion callbacks.
#[derive(Debug)]
struct Transition {
    path: WindowPath,
    crop: Option<Crop>,
    /// Completion callbacks still to come.
    outstanding: u8,
    failed: bool,
}

/// What [`Transitions::settle`] found.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Settled {
    /// Nothing in flight.
    Idle,
    /// Callbacks still to come: leave the stream alone this tick.
    Busy,
    /// Every call completed; the stream is now on this path and crop.
    Commit(WindowPath, Option<Crop>),
    /// A call failed: the stream is wherever it was; the next tick asks again.
    Failed(WindowPath, Option<Crop>),
}

/// The in-flight transition, if any, behind a lock the callbacks can take.
#[derive(Clone, Default, Debug)]
struct Transitions(Arc<Mutex<Option<Transition>>>);

impl Transitions {
    /// Start a transition that `calls` completion callbacks will finish. False (and nothing
    /// started) while one is still in flight.
    fn begin(&self, path: WindowPath, crop: Option<Crop>, calls: u8) -> bool {
        let mut slot = self.0.lock();
        if slot.as_ref().is_some_and(|t| t.outstanding > 0) {
            return false;
        }
        *slot = Some(Transition { path, crop, outstanding: calls, failed: false });
        true
    }

    /// One call completed.
    fn on_result(&self, ok: bool) {
        if let Some(t) = self.0.lock().as_mut() {
            t.outstanding = t.outstanding.saturating_sub(1);
            t.failed |= !ok;
        }
    }

    /// Take the outcome once every call has completed.
    fn settle(&self) -> Settled {
        let mut slot = self.0.lock();
        match slot.as_ref() {
            None => Settled::Idle,
            Some(t) if t.outstanding > 0 => Settled::Busy,
            Some(t) => {
                let outcome = if t.failed {
                    Settled::Failed(t.path, t.crop)
                } else {
                    Settled::Commit(t.path, t.crop)
                };
                *slot = None;
                outcome
            }
        }
    }
}

/// Capture and encoder settings for a target at a requested quality.
fn configs(native: (u32, u32), quality: &Quality) -> (CaptureConfig, EncoderConfig) {
    let scale =
        if quality.scale.is_finite() { f64::from(quality.scale).clamp(0.05, 1.0) } else { 1.0 };
    let side = |px: u32| -> u32 {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped")]
        let v = (f64::from(px) * scale).round().clamp(2.0, 16_384.0) as u32;
        v.next_multiple_of(2)
    };
    let (width, height) = (side(native.0), side(native.1));
    let fps = quality.fps.clamp(1, 240);
    let codec = quality.codec;
    let format =
        if codec == VideoCodec::HevcMain10 { PixelFormat::P010 } else { PixelFormat::Nv12 };
    let capture =
        CaptureConfig { width, height, fps, format, queue_depth: 2, audio: true, crop: None };
    let encoder = EncoderConfig {
        width,
        height,
        codec,
        fps,
        bitrate_bps: quality.bitrate_bps.max(100_000),
        rate_control: slopty_codec::RateControl::LowLatency,
    };
    (capture, encoder)
}

/// One live stream: capture → encode → packetize into the transport's queue.
pub struct ScreenStream {
    id: StreamId,
    target: CaptureTarget,
    native: (u32, u32),
    capture: Capture,
    shared: Arc<Shared>,
    capture_config: CaptureConfig,
    encoder_config: EncoderConfig,
    cursor: JoinHandle<()>,
    /// The task that reads the cursor's picture and reports a change.
    shape: JoinHandle<()>,
    beat: JoinHandle<()>,
    /// The accessibility observer on the window's application, for a window target of a
    /// trusted host; `None` for a display, an untrusted process or an application that would
    /// not be observed. Dropped with the stream.
    hide_watch: Option<HideWatch>,
    /// Client input aimed at this stream, in its pixel coordinates.
    injector: Injector,
    point_scale: f64,
    /// Last requested quality; re-applied when the target changes size.
    quality: Quality,
    /// What the client has been told about the source, and the frame history it follows.
    source: SourceTracker,
    /// The enumeration the target was resolved from; filters for a path switch come from it.
    content: Arc<Shareable>,
    /// How a window is served right now (committed; see `transitions`).
    path: WindowPath,
    /// The path switch or crop move waiting for ScreenCaptureKit's completion callbacks.
    transitions: Transitions,
    /// What tells the connection the stream ended; the crop path calls it when its window
    /// closes, since a display stream does not stop by itself.
    on_stop: Arc<dyn Fn(CaptureError) + Send + Sync>,
    /// `on_stop` has been called.
    stopped: bool,
}

/// How long a stream may produce no frame at all before the client is told the target is idle.
///
/// Long enough that a normally starting stream never reports `Idle` first (first frame on
/// loopback is ~130 ms, MEASUREMENTS.md), short enough that a hidden window costs the receiver
/// only a couple of refresh requests before it stops asking.
pub const SOURCE_IDLE_AFTER: Duration = Duration::from_millis(400);

impl std::fmt::Debug for ScreenStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScreenStream")
            .field("id", &self.id)
            .field("target", &self.target)
            .field("capture", &self.capture_config)
            .finish_non_exhaustive()
    }
}

impl ScreenStream {
    /// Resolve `target`, start capturing at `quality`, and return the `Opened` event to send.
    /// Datagrams are queued on `out`; `on_event` hears [`StreamEvent::Stopped`] if ScreenCaptureKit
    /// ends the stream (window closed, permission revoked).
    pub async fn open(
        id: StreamId,
        target: CaptureTarget,
        quality: Quality,
        out: mpsc::Sender<Queued>,
        budget: DatagramBudget,
        on_event: impl Fn(StreamEvent) + Send + Sync + 'static,
    ) -> Result<(Self, ScreenEvent), ScreenError> {
        let on_event: Arc<dyn Fn(StreamEvent) + Send + Sync> = Arc::new(on_event);
        let on_stop: Arc<dyn Fn(CaptureError) + Send + Sync> = {
            let on_event = Arc::clone(&on_event);
            Arc::new(move |e| on_event(StreamEvent::Stopped(e)))
        };
        let t0 = Instant::now();
        let content = shareable().await?;
        let enumerated = t0.elapsed();
        let (resolved, path) = resolve(&content, target)?;
        let (mut capture_config, encoder_config) = configs(resolved.pixel_size(), &quality);
        capture_config.crop = resolved.crop();

        let shared = Arc::new(Shared {
            id,
            encoder: RwLock::new(None),
            audio: Mutex::new(AudioState { encoder: None, seq: 0, last_loud_us: 0 }),
            pending: Mutex::new(Pending { keyframe: true, ..Pending::default() }),
            packetizer: Mutex::new(Packetizer::new(id)),
            redundancy: Mutex::new(Redundancy::new()),
            rate: Mutex::new(RateController::new(encoder_config.bitrate_bps)),
            sent_at_report: AtomicU64::new(0),
            last_push_us: AtomicU64::new(host_now_us()),
            fps: std::sync::atomic::AtomicU16::new(capture_config.fps),
            fps_ceiling: std::sync::atomic::AtomicU16::new(capture_config.fps),
            last_encoded_us: AtomicU64::new(0),
            keyframe_bytes: AtomicU64::new(0),
            keyframe_deferred_us: AtomicU64::new(0),
            ltr_acked: std::sync::atomic::AtomicBool::new(false),
            cropped: std::sync::atomic::AtomicBool::new(path == WindowPath::DisplayCrop),
            target_hidden: std::sync::atomic::AtomicBool::new(false),
            pointer_over: std::sync::atomic::AtomicBool::new(false),
            suspect_until_us: AtomicU64::new(0),
            filter_stalled: std::sync::atomic::AtomicBool::new(false),
            out,
            budget,
            counters: Counters::new(),
        });
        let t_encoder = Instant::now();
        let encoder = build_encoder(&Arc::downgrade(&shared), encoder_config)?;
        let encoder_built = t_encoder.elapsed();
        *shared.encoder.write() = Some(encoder);
        let start = shared.rate.lock().target_bps();
        shared.apply_bitrate(start);
        shared.apply_cadence(start);

        let (started_tx, started_rx) = oneshot::channel();
        let sink = Arc::clone(&shared);
        let audio_sink = Arc::clone(&shared);
        let capture = Capture::start(
            &resolved,
            &capture_config,
            move |frame| sink.on_frame(&frame),
            Some(Box::new(move |chunk| audio_sink.on_audio(&chunk))),
            {
                let on_stop = Arc::clone(&on_stop);
                move |e| on_stop(e)
            },
            move |result| {
                let _receiver_gone = started_tx.send(result);
            },
        )?;
        started_rx.await.map_err(|_dropped| ScreenError::Closed)??;
        tracing::debug!(
            stream = %id,
            enumerate_ms = enumerated.as_millis(),
            encoder_ms = encoder_built.as_millis(),
            total_ms = t0.elapsed().as_millis(),
            ?path,
            crop = ?capture_config.crop,
            "capture started"
        );

        let native = resolved.pixel_size();
        let zoom = f64::from(capture_config.width) / f64::from(native.0);
        let point_scale = f64::from(resolved.point_scale());
        // Two tasks, not one. The beat is a promise about time and must never be behind work
        // that takes any: the geometry and pointer calls in the cursor loop are window-server
        // round trips that have been measured at 90 ms, three beats' worth.
        let beat = tokio::spawn(beat_loop(Arc::clone(&shared)));
        let cursor = tokio::spawn(cursor_loop(Arc::clone(&shared), target, zoom, point_scale));
        #[expect(clippy::cast_possible_truncation, reason = "a display scale is 1 to 4")]
        #[expect(clippy::cast_sign_loss, reason = "a display scale is positive")]
        let backing = point_scale.round().clamp(1.0, 4.0) as u8;
        let shape = tokio::spawn(shape_loop(Arc::clone(&shared), backing, Arc::clone(&on_event)));
        let hide_watch = hide_watch_for(id, target, &shared).await;
        #[expect(clippy::cast_possible_truncation, reason = "a small ratio")]
        let scale = (point_scale * zoom) as f32;
        let opened = ScreenEvent::Opened {
            stream: id,
            target,
            codec: encoder_config.codec,
            width: capture_config.width,
            height: capture_config.height,
            scale,
            hdr: false,
        };
        let injector = Injector::new(target, point_scale * zoom);
        let stream = Self {
            id,
            target,
            native,
            capture,
            shared,
            capture_config,
            encoder_config,
            cursor,
            shape,
            beat,
            hide_watch,
            injector,
            point_scale,
            quality,
            source: SourceTracker::new(Instant::now()),
            content,
            path,
            transitions: Transitions::default(),
            on_stop,
            stopped: false,
        };
        Ok((stream, opened))
    }

    /// Stream id.
    #[must_use]
    pub const fn id(&self) -> StreamId {
        self.id
    }

    /// What is being streamed.
    #[must_use]
    pub const fn target(&self) -> CaptureTarget {
        self.target
    }

    /// How a window is served right now (`Filter` for a display target).
    #[must_use]
    pub const fn path(&self) -> WindowPath {
        self.path
    }

    /// Counters.
    #[must_use]
    pub fn stats(&self) -> ScreenStats {
        self.shared.stats()
    }

    /// A handle on the counters for the daemon's registry.
    #[must_use]
    pub fn stats_handle(&self) -> StatsHandle {
        StatsHandle(Arc::clone(&self.shared))
    }

    /// Change quality. A size, rate or codec change rebuilds the encoder and reconfigures the
    /// capture; a bitrate-only change is applied in place.
    pub fn set_quality(&mut self, quality: &Quality) -> Result<(), ScreenError> {
        self.quality = *quality;
        let (mut capture_config, encoder_config) = configs(self.native, quality);
        capture_config.crop = self.capture_config.crop;
        if capture_config == self.capture_config
            && encoder_config.codec == self.encoder_config.codec
        {
            if encoder_config.bitrate_bps != self.encoder_config.bitrate_bps {
                // The client moved its ceiling; the controller keeps its place under it.
                let target = {
                    let mut rate = self.shared.rate.lock();
                    rate.set_max(encoder_config.bitrate_bps);
                    rate.target_bps()
                };
                self.shared.apply_bitrate(target);
                self.encoder_config = encoder_config;
            }
            return Ok(());
        }
        self.reconfigure(capture_config, encoder_config)
    }

    /// Follow the target: a window the user resized on the host gets a stream of its new
    /// size (fresh encoder, keyframe) and the client hears `Geometry`; one on the display-crop
    /// path that moved gets its crop moved, and one that went under another window (or off
    /// its display) falls back to the window filter until it is clear again. Cheap when
    /// nothing changed (a WindowServer query or three); call it a few times a second.
    pub fn check_geometry(&mut self) -> Result<Option<ScreenEvent>, ScreenError> {
        let Some(rect) = slopty_capture::target_bounds(self.target) else {
            self.window_gone();
            return Ok(None);
        };
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped")]
        let px = |points: f64| (points * self.point_scale).round().clamp(2.0, 16_384.0) as u32;
        let native = (px(rect.w), px(rect.h));
        let resized = native != self.native;
        if let CaptureTarget::Window(id) = self.target
            && crop_windows()
        {
            self.follow_window(id, &rect);
        }
        if !resized {
            return Ok(None);
        }
        tracing::info!(stream = %self.id, from = ?self.native, to = ?native, "target resized");
        self.native = native;
        let (mut capture_config, encoder_config) = configs(native, &self.quality);
        capture_config.crop = self.capture_config.crop;
        self.reconfigure(capture_config, encoder_config)?;
        Ok(Some(ScreenEvent::Geometry {
            stream: self.id,
            width: self.capture_config.width,
            height: self.capture_config.height,
        }))
    }

    /// Whether the target has drawn anything, when that answer changed since the client was
    /// last told. Polled beside [`Self::check_geometry`].
    ///
    /// A window that is hidden, minimised or has not drawn yields no frames, and the receiver
    /// cannot tell that from a stream whose frames are all being lost: it sits in "need refresh"
    /// and asks again every backoff period, forever, for something no refresh can produce
    /// (MEASUREMENTS.md, "a target that never produces a frame"). Saying so once stops it.
    pub fn check_source(&mut self) -> Option<ScreenEvent> {
        let encoded = self.shared.counters.encoded.load(Ordering::Relaxed);
        let hidden = self.shared.target_hidden.load(Ordering::Relaxed);
        let state = self.source.poll(encoded, hidden, Instant::now())?;
        tracing::debug!(stream = %self.id, ?state, "capture source state");
        Some(ScreenEvent::Source { stream: self.id, state })
    }

    /// The window list no longer knows the target. Through the window filter ScreenCaptureKit
    /// stops the stream itself and `on_stop` fires from its delegate; a display crop would keep
    /// streaming the desktop where the window was, so the stream stops itself and says so
    /// the same way, once.
    fn window_gone(&mut self) {
        if self.stopped || !matches!(self.target, CaptureTarget::Window(_)) {
            return;
        }
        if self.path != WindowPath::DisplayCrop {
            return;
        }
        // Nothing more goes out while the stop is in flight: the crop outlives the window.
        self.shared.target_hidden.store(true, Ordering::Relaxed);
        self.stopped = true;
        tracing::info!(stream = %self.id, "window closed under a display crop: stopping");
        let id = self.id;
        self.capture.stop(move |result| {
            if let Err(e) = result {
                tracing::debug!(stream = %id, error = %e, "capture stop");
            }
        });
        (self.on_stop)(CaptureError::Stopped("window closed".to_owned()));
    }

    /// Keep a window on the right path: crop where it is now, or the window filter while
    /// something covers it. On-screen state and occlusion are checked every time since other
    /// windows move too. Nothing is committed here: the completion callbacks of the
    /// ScreenCaptureKit calls settle the transition on a later tick, and a failed one is simply
    /// asked again.
    fn follow_window(&mut self, id: slopty_core::WindowId, rect: &Rect) {
        // First, and outside everything below: the guard must not depend on the transition state
        // machine. A swap that ScreenCaptureKit keeps rejecting leaves `settle` busy for as long
        // as it keeps failing, and those are exactly the ticks where the crop is still running
        // over a window that is no longer there.
        let on_screen = slopty_capture::window_on_screen(id);
        if self.shared.target_hidden.swap(!on_screen, Ordering::Relaxed) == on_screen {
            tracing::info!(stream = %self.id, on_screen, "target visibility");
        }
        match self.transitions.settle() {
            Settled::Busy => return,
            Settled::Idle => {}
            Settled::Commit(path, crop) => {
                self.path = path;
                self.capture_config.crop = crop;
                self.shared.cropped.store(path == WindowPath::DisplayCrop, Ordering::Relaxed);
                tracing::debug!(stream = %self.id, ?path, ?crop, "capture path settled");
            }
            Settled::Failed(path, crop) => {
                tracing::warn!(stream = %self.id, ?path, ?crop, "capture path change failed; retrying");
            }
        }
        // Keyed on the target, not on the path this side believes it is on: a `retarget` the
        // framework rejects leaves the crop running while the bookkeeping says window filter,
        // and every frame it delivers is then a picture of the desktop.
        // A suspicion is the other reason the crop is not allowed: the accessibility API has
        // said a window of this application went, and for the hold the window list cannot be
        // trusted to say which. Frames are held for the hold either way (`Shared::on_frame`);
        // the swap is what wakes ScreenCaptureKit, which stops delivering for an
        // application-scoped display filter once a window of that application is ordered out
        // and is woken by no re-application of the same kind of filter, only by a change of
        // kind (MEASUREMENTS.md, "a sibling window closing stalls the crop"). A false suspicion
        // comes back to the crop on the first tick after the hold.
        let suspected = self.shared.suspected_at(host_now_us());
        let mut wanted =
            if suspected { None } else { wanted_crop(id, rect, self.point_scale, on_screen) };
        // Another window of the application went (`Shared::sibling_went`): no suspicion, but
        // a crop that keeps its filter is a crop that never gets another frame. One tick on
        // the window filter is the change of kind that wakes the framework; the crop is
        // wanted again on the next. Any other path change is a change of kind as well and
        // clears the flag by itself.
        let stalled = self.shared.filter_stalled.load(Ordering::Relaxed);
        let waking = stalled && wanted.is_some() && self.path == WindowPath::DisplayCrop;
        if waking {
            wanted = None;
        }
        let wanted_path =
            if wanted.is_some() { WindowPath::DisplayCrop } else { WindowPath::Filter };
        if wanted_path == self.path && wanted == self.capture_config.crop {
            return;
        }
        match (self.path, wanted) {
            (WindowPath::DisplayCrop, Some(crop)) => {
                // A move: one configuration update carries the new rectangle.
                if self.transitions.begin(WindowPath::DisplayCrop, Some(crop), 1) {
                    self.update_capture(Some(crop));
                }
            }
            (WindowPath::DisplayCrop, None) => {
                // To the window filter: clear the crop first, a `sourceRect` on a window
                // stream would be read in the window's own space.
                let target = match Target::resolve(&self.content, self.target) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!(stream = %self.id, error = %e, "window filter");
                        return;
                    }
                };
                if self.transitions.begin(WindowPath::Filter, None, 2) {
                    self.shared.filter_stalled.store(false, Ordering::Relaxed);
                    if waking {
                        tracing::info!(stream = %self.id, "another window of the application went: waking the capture through the window filter");
                    } else {
                        tracing::info!(stream = %self.id, on_screen, suspected, "window covered, hidden, off its display or suspected: window filter");
                    }
                    self.update_capture(None);
                    self.retarget(&target);
                }
            }
            (WindowPath::Filter, Some(crop)) => {
                let target = match Target::resolve_crop(&self.content, id) {
                    Ok(Some(t)) => t,
                    Ok(None) => return,
                    Err(e) => {
                        tracing::warn!(stream = %self.id, error = %e, "display crop");
                        return;
                    }
                };
                if self.transitions.begin(WindowPath::DisplayCrop, Some(crop), 2) {
                    self.shared.filter_stalled.store(false, Ordering::Relaxed);
                    tracing::info!(stream = %self.id, ?crop, "window clear: display crop");
                    self.retarget(&target);
                    self.update_capture(Some(crop));
                }
            }
            (WindowPath::Filter, None) => {}
        }
    }

    /// Push the current configuration with `crop` to the live stream; the completion lands
    /// in the transition.
    fn update_capture(&self, crop: Option<Crop>) {
        let id = self.id;
        let transitions = self.transitions.clone();
        let config = CaptureConfig { crop, ..self.capture_config };
        self.capture.update(&config, move |result| {
            if let Err(e) = &result {
                tracing::warn!(stream = %id, error = %e, "capture update failed");
            }
            transitions.on_result(result.is_ok());
        });
    }

    /// Swap the live stream's filter; the completion lands in the transition.
    fn retarget(&self, target: &Target) {
        let id = self.id;
        let transitions = self.transitions.clone();
        self.capture.retarget(target, move |result| {
            if let Err(e) = &result {
                tracing::warn!(stream = %id, error = %e, "capture retarget failed");
            }
            transitions.on_result(result.is_ok());
        });
    }

    /// Rebuild the encoder and reconfigure the capture for a new size, rate or codec.
    fn reconfigure(
        &mut self,
        capture_config: CaptureConfig,
        encoder_config: EncoderConfig,
    ) -> Result<(), ScreenError> {
        let weak = Arc::downgrade(&self.shared);
        let encoder = build_encoder(&weak, encoder_config)?;
        *self.shared.encoder.write() = Some(encoder);
        let target = {
            let mut rate = self.shared.rate.lock();
            rate.set_max(encoder_config.bitrate_bps);
            rate.target_bps()
        };
        self.shared.apply_bitrate(target);
        // A new quality sets a new ceiling, and the ladder starts from it again: the rung that was
        // in force answered a bitrate the client has just replaced.
        self.shared.fps_ceiling.store(capture_config.fps, Ordering::Relaxed);
        self.shared.fps.store(capture_config.fps, Ordering::Relaxed);
        self.shared.apply_cadence(target);
        self.shared.pending.lock().keyframe = true;
        let id = self.id;
        self.capture.update(&capture_config, move |result| {
            if let Err(e) = result {
                tracing::warn!(stream = %id, error = %e, "capture reconfigure failed");
            }
        });
        let zoom = f64::from(capture_config.width) / f64::from(self.native.0);
        self.injector.set_scale(self.point_scale * zoom);
        self.capture_config = capture_config;
        self.encoder_config = encoder_config;
        Ok(())
    }

    /// Deliver client input to the streamed window or display.
    pub fn inject(&mut self, input: &ScreenInput) -> Result<(), ScreenError> {
        Ok(self.injector.inject(input)?)
    }

    /// Give the streamed window's application keyboard focus on the host.
    pub fn focus(&mut self) -> Result<(), ScreenError> {
        Ok(self.injector.focus()?)
    }

    /// What a client's resize to `(width, height)` native pixels asks of the host: the window
    /// and the size in points, for [`resize_window`] off the runtime. A display stream asks
    /// nothing.
    #[must_use]
    pub fn resize_points(
        &self,
        width: u32,
        height: u32,
    ) -> Option<(slopty_core::WindowId, f64, f64)> {
        let CaptureTarget::Window(id) = self.target else { return None };
        if self.point_scale <= 0.0 || width == 0 || height == 0 {
            return None;
        }
        Some((id, f64::from(width) / self.point_scale, f64::from(height) / self.point_scale))
    }

    /// Fold in a receiver report: LTR acks feed the encoder, loss feeds the parity ratio, and
    /// loss, queueing, stalls and the QUIC path (`path`) drive the bitrate. Returns the
    /// controller's decision when this report completed a decision window.
    pub fn report(&self, report: &ReceiverReport, path: Option<PathSample>) -> Option<Decision> {
        self.shared.report(report, path)
    }

    /// The bitrate the controller is asking the encoder for right now.
    #[must_use]
    pub fn bitrate_bps(&self) -> u32 {
        self.shared.rate.lock().target_bps()
    }

    /// The client lost a frame it cannot recover: make the next frame stand on its own.
    pub fn request_refresh(&self, last_good_frame: u32) {
        self.shared.request_refresh(last_good_frame);
    }

    /// Retransmit fragments of a recent frame, unless QUIC is already holding more than the
    /// frame budget: an answer that leaves behind seconds of queued frames arrives after the
    /// receiver has given up, and on a collapsed link every NACK answered that way stacked
    /// another copy of the frame into the queue (64 000 datagrams for 12 frames,
    /// MEASUREMENTS.md "start-up over the mesh").
    pub fn nack(&self, frame: u32, fragments: &[u16]) {
        self.shared.nack(frame, fragments);
    }

    /// Stop capturing and tear down.
    pub async fn close(mut self) {
        self.cursor.abort();
        self.shape.abort();
        self.beat.abort();
        drop(self.hide_watch.take());
        let (tx, rx) = oneshot::channel();
        self.capture.stop(move |result| {
            let _receiver_gone = tx.send(result);
        });
        if let Ok(Err(e)) = rx.await {
            tracing::debug!(stream = %self.id, error = %e, "capture stop");
        }
        let stats = self.stats();
        tracing::info!(stream = %self.id, ?stats, "screen stream closed");
    }
}

/// Set a host window's size in points through the accessibility API.
///
/// The window is matched by the frame and title the window list gives it (the API has no
/// window number; see `slopty_capture::ax`). Blocking: call it off the runtime. The stream's
/// geometry poll sees the new frame within its period and tells the client with a `Geometry`
/// event.
pub fn resize_window(
    window: slopty_core::WindowId,
    width: f64,
    height: f64,
) -> Result<(), ScreenError> {
    let pid = slopty_capture::window_owner_pid(window).ok_or(ScreenError::WindowGone)?;
    let bounds = slopty_capture::window_bounds(window).ok_or(ScreenError::WindowGone)?;
    let title = slopty_capture::window_title(window);
    let target = slopty_capture::TargetWindow { bounds, title };
    Ok(slopty_capture::resize_window(pid, &target, width, height)?)
}

/// Put a window target's application under the accessibility watch, off the runtime (the
/// registration is a few window-server round trips). A display has no application to watch;
/// a host that is not trusted for accessibility, or an application that will not be observed,
/// gets the window-list check alone, and says so once.
async fn hide_watch_for(
    id: StreamId,
    target: CaptureTarget,
    shared: &Arc<Shared>,
) -> Option<HideWatch> {
    let CaptureTarget::Window(window) = target else {
        return None;
    };
    let weak = Arc::downgrade(shared);
    let started = tokio::task::spawn_blocking(move || {
        let pid = slopty_capture::window_owner_pid(window)?;
        let bounds = slopty_capture::window_bounds(window)?;
        let title = slopty_capture::window_title(window);
        let target = slopty_capture::TargetWindow { bounds, title };
        Some(HideWatch::start(pid, target, move |went| {
            if let Some(shared) = weak.upgrade() {
                match went {
                    slopty_capture::Went::Target => shared.suspect(host_now_us()),
                    slopty_capture::Went::Other => shared.sibling_went(),
                }
            }
        }))
    })
    .await;
    match started {
        Ok(Some(Ok(watch))) => {
            tracing::debug!(stream = %id, targeted = watch.targeted(), "accessibility hide watch on");
            Some(watch)
        }
        Ok(Some(Err(e))) => {
            tracing::info!(stream = %id, error = %e, "no accessibility hide watch");
            None
        }
        Ok(None) => {
            tracing::info!(stream = %id, "no accessibility hide watch: window owner unknown");
            None
        }
        Err(_panicked) => None,
    }
}

/// Sample the pointer and send its position in stream pixels whenever it moves, and a
/// heartbeat whenever nothing at all left for [`HEARTBEAT_AFTER`] (a quiet source must not
/// read as a stalled link). Runs until the task is aborted by [`ScreenStream::close`] or the
/// transport queue closes.
async fn beat_loop(shared: Arc<Shared>) {
    let mut ticks = tokio::time::interval(CURSOR_PERIOD);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut beats: u32 = 0;
    let mut last_beat_us: Option<u64> = None;
    let heartbeat_after_us = u64::try_from(HEARTBEAT_AFTER.as_micros()).unwrap_or(u64::MAX);
    // Four times the promise, which is twice the gap the receiver already calls a stall: past
    // this the beat has not merely slipped, it has failed at the one thing it is for.
    let late_beat_us = heartbeat_after_us.saturating_mul(4);
    while !shared.out.is_closed() {
        ticks.tick().await;
        let now = host_now_us();
        let silence_us = now.saturating_sub(shared.last_push_us.load(Ordering::Relaxed));
        if silence_us < heartbeat_after_us {
            continue;
        }
        beats = beats.wrapping_add(1);
        tracing::trace!(stream = %shared.id, beats, silence_us, "heartbeat");
        shared.counters.heartbeats.fetch_add(1, Ordering::Relaxed);
        if let Some(previous) = last_beat_us {
            let gap = now.saturating_sub(previous);
            shared.counters.beat_gap.lock().push(gap);
            shared.counters.beat_gap_worst_us.fetch_max(gap, Ordering::Relaxed);
            if gap >= late_beat_us {
                tracing::info!(stream = %shared.id, gap_us = gap, beats, "late heartbeat");
            }
        }
        last_beat_us = Some(now);
        shared.push(heartbeat_datagram(shared.id, beats, send_ms_lo(now)));
    }
}

/// Where the pointer is over the target, sent when it moves.
///
/// Every call in here is a window-server round trip, so all of them run on the blocking pool:
/// what this loop must not do is occupy a runtime worker, because [`beat_loop`] needs one on
/// time (MEASUREMENTS.md, "the beat behind the geometry call").
async fn cursor_loop(shared: Arc<Shared>, target: CaptureTarget, zoom: f64, point_scale: f64) {
    let mut ticks = tokio::time::interval(CURSOR_PERIOD);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut bounds: Option<Rect> = None;
    let mut last: Option<(i32, i32, bool)> = None;
    let mut seq: u32 = 0;
    let mut tick: u64 = 0;
    while !shared.out.is_closed() {
        ticks.tick().await;
        if tick.is_multiple_of(BOUNDS_EVERY) {
            let counters = Arc::clone(&shared);
            let measured = tokio::task::spawn_blocking(move || {
                let started = host_now_us();
                let rect = slopty_capture::target_bounds(target);
                counters.counters.bounds.lock().push(host_now_us().saturating_sub(started));
                rect
            })
            .await;
            let Ok(rect) = measured else { return };
            bounds = rect;
        }
        tick = tick.wrapping_add(1);
        let Some(rect) = bounds else { continue };
        let Ok((px, py)) = tokio::task::spawn_blocking(slopty_capture::pointer_location).await
        else {
            return;
        };
        let visible = rect.contains(px, py);
        shared.pointer_over.store(visible, Ordering::Relaxed);
        let to_pixels = |v: f64| -> i32 {
            #[expect(clippy::cast_possible_truncation, reason = "clamped")]
            let p = (v * point_scale * zoom).round().clamp(-1.0e6, 1.0e6) as i32;
            p
        };
        let sample = (to_pixels(px - rect.x), to_pixels(py - rect.y), visible);
        if last == Some(sample) {
            continue;
        }
        last = Some(sample);
        seq = seq.wrapping_add(1);
        let datagram = cursor_datagram(
            shared.id,
            seq,
            send_ms_lo(host_now_us()),
            sample.0,
            sample.1,
            sample.2,
        );
        shared.push(datagram);
    }
}

/// Read the cursor's picture at `SHAPE_PERIOD` while the pointer is over the target, and
/// report each change through `on_event`. Its own task: the first read in a process takes
/// seconds (`slopty_capture::warm_cursor` pays that at start-up), and a read must never
/// hold the position loop.
async fn shape_loop(
    shared: Arc<Shared>,
    backing: u8,
    on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
) {
    let mut ticks = tokio::time::interval(SHAPE_PERIOD);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut sent = ShapeDedup::default();
    while !shared.out.is_closed() {
        ticks.tick().await;
        if !shared.pointer_over.load(Ordering::Relaxed) {
            continue;
        }
        let Ok(read) =
            tokio::task::spawn_blocking(move || slopty_capture::cursor_shape(backing)).await
        else {
            return;
        };
        if let Some(shape) = sent.observe(read) {
            on_event(StreamEvent::Cursor(shape));
        }
    }
}

/// Which cursor picture the client was last told, so one goes out only when it changed.
///
/// A read that says nothing (the cursor hidden, or the system not answering) changes
/// nothing: the client keeps the last picture, and the position channel says whether to
/// draw it.
#[derive(Debug, Default)]
pub struct ShapeDedup {
    last: Option<CursorShape>,
}

impl ShapeDedup {
    /// The picture to send for this reading, if any.
    pub fn observe(&mut self, read: Option<CursorShape>) -> Option<CursorShape> {
        let shape = read?;
        if self.last.as_ref() == Some(&shape) {
            return None;
        }
        self.last = Some(shape.clone());
        Some(shape)
    }
}

#[cfg(test)]
mod shape_tests {
    use super::*;

    fn shape(px: u8) -> CursorShape {
        CursorShape { w: 1, h: 1, hot_x: 0, hot_y: 0, bgra: vec![px, px, px, 255], scale: 2 }
    }

    #[test]
    fn a_picture_goes_out_once_per_change_and_a_blank_read_changes_nothing() {
        let mut dedup = ShapeDedup::default();
        assert_eq!(dedup.observe(None), None, "nothing read yet, nothing to send");
        assert_eq!(dedup.observe(Some(shape(1))), Some(shape(1)), "the first picture");
        assert_eq!(dedup.observe(Some(shape(1))), None, "the same again is not news");
        assert_eq!(dedup.observe(None), None, "a hidden or unreadable cursor keeps the last");
        assert_eq!(dedup.observe(Some(shape(1))), None, "still the one the client has");
        assert_eq!(dedup.observe(Some(shape(2))), Some(shape(2)), "a new picture");
    }
}

#[cfg(test)]
mod tests {
    use slopty_codec::audio::{CHANNELS, FRAME_SAMPLES};

    use super::*;

    const CROP: Crop = Crop { x: 10.0, y: 20.0, w: 300.0, h: 200.0 };

    /// A stream's shared state with no encoder and nowhere to send: enough to drive
    /// [`Shared::on_frame`] and read what it counted.
    fn shared_for_frames() -> (Arc<Shared>, mpsc::Receiver<Queued>) {
        shared_with_queue(8)
    }

    /// [`shared_for_frames`] with a datagram queue of `capacity`.
    fn shared_with_queue(capacity: usize) -> (Arc<Shared>, mpsc::Receiver<Queued>) {
        let (out, rx) = mpsc::channel(capacity);
        let shared = Arc::new(Shared {
            id: StreamId(1),
            encoder: RwLock::new(None),
            audio: Mutex::new(AudioState { encoder: None, seq: 0, last_loud_us: 0 }),
            pending: Mutex::new(Pending::default()),
            packetizer: Mutex::new(Packetizer::new(StreamId(1))),
            redundancy: Mutex::new(Redundancy::new()),
            rate: Mutex::new(RateController::new(8_000_000)),
            sent_at_report: AtomicU64::new(0),
            last_push_us: AtomicU64::new(0),
            fps: std::sync::atomic::AtomicU16::new(60),
            fps_ceiling: std::sync::atomic::AtomicU16::new(60),
            last_encoded_us: AtomicU64::new(0),
            keyframe_bytes: AtomicU64::new(0),
            keyframe_deferred_us: AtomicU64::new(0),
            ltr_acked: std::sync::atomic::AtomicBool::new(false),
            cropped: std::sync::atomic::AtomicBool::new(true),
            target_hidden: std::sync::atomic::AtomicBool::new(false),
            pointer_over: std::sync::atomic::AtomicBool::new(false),
            suspect_until_us: AtomicU64::new(0),
            filter_stalled: std::sync::atomic::AtomicBool::new(false),
            out,
            budget: DatagramBudget::new(),
            counters: Counters::new(),
        });
        (shared, rx)
    }

    #[test]
    fn quantiles_read_any_order_and_a_ring_keeps_the_window() {
        assert_eq!(Quantiles::of(&[]), Quantiles::default());
        let q = Quantiles::of(&[5, 1, 3, 2, 4]);
        assert_eq!((q.n, q.p50_us, q.p95_us, q.max_us), (5, 3, 5, 5));
        assert_eq!(Quantiles::of(&[7]).describe(), "0.01 / 0.01 / 0.01 ms (n=1)");
        let mut ring = LatencyRing::default();
        for us in 0..u64::try_from(LATENCY_WINDOW).unwrap_or(u64::MAX).saturating_add(10) {
            ring.push(us);
        }
        let q = ring.quantiles();
        assert_eq!(usize::try_from(q.n).unwrap_or(0), LATENCY_WINDOW, "bounded to the window");
        assert_eq!(q.max_us, u64::try_from(LATENCY_WINDOW).unwrap_or(0).saturating_add(9));
        assert!(q.p50_us > 10, "the oldest samples left first: {q:?}");
    }

    #[test]
    fn configs_clamp_the_quality_and_keep_even_sides() {
        let q = Quality {
            fps: 500,
            bitrate_bps: 1,
            scale: 0.3333,
            codec: VideoCodec::Hevc,
            hdr: false,
        };
        let (capture, encoder) = configs((1_001, 777), &q);
        assert_eq!((capture.width, capture.height), (334, 260), "scaled, rounded up to even");
        assert_eq!(capture.fps, 240, "fps clamped");
        assert_eq!(capture.format, PixelFormat::Nv12);
        assert_eq!(encoder.bitrate_bps, 100_000, "bitrate floor");
        assert_eq!((encoder.width, encoder.height, encoder.fps), (334, 260, 240));

        let nan = Quality { scale: f32::NAN, codec: VideoCodec::HevcMain10, ..q };
        let (capture, _encoder) = configs((100, 100), &nan);
        assert_eq!((capture.width, capture.height), (100, 100), "a NaN scale is native");
        assert_eq!(capture.format, PixelFormat::P010, "10-bit for Main10");

        let tiny = Quality { scale: 0.0001, ..q };
        let (capture, _encoder) = configs((10, 10), &tiny);
        assert_eq!((capture.width, capture.height), (2, 2), "never below two pixels");
    }

    #[test]
    fn the_registry_lists_live_streams_then_the_last_closed_and_tells_its_observer() {
        let registry = Registry::default();
        let counts = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&counts);
        registry.observe(move |n| seen.lock().push(n));
        let handle = |id: u32| {
            let (shared, _rx) = shared_for_frames();
            // Fresh from the helper: nothing else holds it, so the unwrap cannot fail.
            let mut shared = Arc::try_unwrap(shared).ok().expect("fresh");
            shared.id = StreamId(id);
            StatsHandle(Arc::new(shared))
        };
        registry.insert(&"alice", CaptureTarget::Display(1), handle(1));
        registry.insert(&"bob", CaptureTarget::Display(2), handle(2));
        let (live, closed) = registry.summaries();
        assert_eq!(
            live.iter().map(|s| (s.client.as_str(), s.stream)).collect::<Vec<_>>(),
            [("alice", 1), ("bob", 2)]
        );
        assert!(closed.is_empty());
        registry.remove(&"alice", StreamId(9));
        assert_eq!(registry.summaries().0.len(), 2, "an unknown stream is not removed");
        registry.remove(&"alice", StreamId(1));
        let (live, closed) = registry.summaries();
        assert_eq!(live.len(), 1);
        assert_eq!(closed.iter().map(|s| s.stream).collect::<Vec<_>>(), [1]);
        assert_eq!(*counts.lock(), [1, 2, 1], "the observer hears every change, in order");
        for id in 10..u32::try_from(CLOSED_KEEP).unwrap_or(u32::MAX).saturating_add(12) {
            registry.insert(&"carol", CaptureTarget::Display(id), handle(id));
            registry.remove(&"carol", StreamId(id));
        }
        let (_live, closed) = registry.summaries();
        assert_eq!(closed.len(), CLOSED_KEEP, "only the last few closed are kept");
        assert_eq!(closed.first().map(|s| s.stream), Some(12), "oldest first");
    }

    /// One 16x16 frame. The contents do not matter: with no encoder nothing reads them, and
    /// what is being tested is whether `on_frame` gets that far at all.
    fn a_frame() -> CapturedFrame {
        use std::ptr::{self, NonNull};

        let mut raw: *mut objc2_core_video::CVPixelBuffer = ptr::null_mut();
        // SAFETY: the out-pointer is valid and no attributes dictionary is passed
        // (CoreVideo, `CVPixelBufferCreate`).
        let status = unsafe {
            objc2_core_video::CVPixelBufferCreate(
                None,
                16,
                16,
                objc2_core_video::kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                None,
                NonNull::from(&mut raw),
            )
        };
        assert_eq!(status, 0, "CVPixelBufferCreate");
        // SAFETY: `CVPixelBufferCreate` returned a +1 reference, which this takes over.
        let buffer = unsafe {
            objc2_core_foundation::CFRetained::from_raw(NonNull::new(raw).expect("a buffer"))
        };
        CapturedFrame {
            image: slopty_codec::PixelBuffer::from_retained(buffer),
            capture_ts_us: 1,
            display_ts_us: None,
            age_us: 0,
            latency_us: 0,
        }
    }

    /// The beat is a promise about time, so the thing that must be true of it is that nothing
    /// else the stream does can make it late. Geometry work that takes 300 ms — six times a
    /// stall gap, and three times the worst window-server round trip measured — runs beside it
    /// here, and the beats keep their cadence because they are no longer on that task.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_slow_geometry_call_does_not_make_the_beat_late() {
        let (shared, mut rx) = shared_for_frames();
        let beat = tokio::spawn(beat_loop(Arc::clone(&shared)));
        // Whatever the cursor loop does, it does it like this: off the runtime's workers. It
        // spins rather than sleeps because a sleeping thread is not what a window-server round
        // trip does to one — and because sleeping in this project is what the lint forbids.
        let slow = tokio::task::spawn_blocking(|| {
            let until = Instant::now()
                .checked_add(Duration::from_millis(300))
                .expect("a deadline inside the clock");
            while Instant::now() < until {
                std::hint::spin_loop();
            }
        });

        tokio::time::sleep(Duration::from_millis(500)).await;
        slow.await.expect("the slow call");
        beat.abort();

        let seen = shared.stats();
        assert!(seen.heartbeats >= 10, "a beat every 25 ms over half a second: {seen:?}");
        assert!(
            seen.beat_gap_worst_us <= 100_000,
            "the beat fell behind the geometry call: {} µs",
            seen.beat_gap_worst_us
        );
        // The beats really went out, rather than only being counted.
        let mut sent: u64 = 0;
        while rx.try_recv().is_ok() {
            sent = sent.saturating_add(1_u64);
        }
        assert!(sent >= 8, "only {sent} datagrams for {} beats", seen.heartbeats);
    }

    /// A frame that arrives inside the hold a suspicion opened is kept back and counted as
    /// suspected, not withheld: the window list has not confirmed anything yet. Once the hold
    /// lapses without confirmation, frames flow again.
    #[test]
    fn a_frame_captured_under_suspicion_is_held_until_the_hold_lapses() {
        let (shared, _rx) = shared_for_frames();
        let hold_us = u64::try_from(SUSPICION_HOLD.as_micros()).unwrap();
        // Built first: the first pixel buffer of a process takes longer than the hold.
        let frame = a_frame();

        shared.suspect(1_000);
        assert!(shared.suspected_at(1_000), "the hold opens at once");
        assert!(shared.suspected_at(1_000 + hold_us - 1), "and lasts the whole hold");
        assert!(!shared.suspected_at(1_000 + hold_us), "and no longer");
        assert_eq!(shared.stats().suspicions, 1);

        // A frame now is held as suspected and never reaches the crop count.
        shared.suspect(host_now_us());
        shared.on_frame(&frame);
        let held = shared.stats();
        assert_eq!((held.captured, held.suspected, held.withheld, held.cropped), (1, 1, 0, 0));

        // The window filter's frames are held just the same: the framework still delivers a
        // frame or two of the old filter after a swap has settled.
        shared.cropped.store(false, Ordering::Relaxed);
        shared.on_frame(&frame);
        let filtered = shared.stats();
        assert_eq!((filtered.captured, filtered.suspected, filtered.cropped), (2, 2, 0));
        shared.cropped.store(true, Ordering::Relaxed);

        // Confirmed by the window list: the frame is withheld, the older reason wins.
        shared.target_hidden.store(true, Ordering::Relaxed);
        shared.on_frame(&frame);
        let confirmed = shared.stats();
        assert_eq!((confirmed.suspected, confirmed.withheld), (2, 1));

        // The hold has lapsed and the window list says on screen: frames flow again.
        shared.target_hidden.store(false, Ordering::Relaxed);
        shared.suspect_until_us.store(0, Ordering::Relaxed);
        shared.on_frame(&frame);
        let flowing = shared.stats();
        assert_eq!((flowing.captured, flowing.suspected, flowing.cropped), (4, 2, 1));
    }

    /// The ladder is the host's, not only the policy's: a target that has collapsed moves the rung
    /// the guard and the gate both read, and a target that recovers moves it back.
    #[test]
    fn a_collapsed_target_takes_the_stream_down_the_cadence_ladder() {
        let (shared, _rx) = shared_for_frames();

        shared.apply_cadence(1_000_000);
        assert_eq!(shared.fps.load(Ordering::Relaxed), 15, "2 KB a frame at 60 is not a picture");
        shared.apply_cadence(30_000_000);
        assert_eq!(shared.fps.load(Ordering::Relaxed), 60);
    }

    /// Captures keep arriving on the display's beat whatever the cadence is; the gate is what
    /// decides which of them the encoder is given, and it counts in time rather than in frames.
    #[test]
    fn the_cadence_gate_hands_the_encoder_one_capture_a_period() {
        // A full-sized queue: the guard below the gate refuses everything under its low-water mark.
        let (shared, _rx) = shared_with_queue(DATAGRAM_QUEUE);
        shared.fps.store(30, Ordering::Relaxed);

        // One buffer, moved through the beats: building a `CVPixelBuffer` costs far more than the
        // gate this is about, and nothing downstream of here reads the pixels.
        let mut frame = a_frame();
        let mut encoded = 0_u32;
        for beat in 0..12_u64 {
            // Capture timestamps are the host clock, so the first frame of a stream is always due.
            frame.capture_ts_us = 1_000_000_u64.saturating_add(beat.saturating_mul(16_667));
            shared.on_frame(&frame);
            if shared.last_encoded_us.load(Ordering::Relaxed) == frame.capture_ts_us {
                encoded = encoded.saturating_add(1);
            }
        }

        assert_eq!(encoded, 6, "half of a 60 fps capture beat belongs to a 30 fps cadence");
        assert_eq!(shared.stats().captured, 12, "every capture is still counted");
        assert_eq!(shared.stats().dropped, 0, "a paced skip is not a congestion drop");

        // A client with no picture waits for a keyframe, not for the rung.
        let last = shared.last_encoded_us.load(Ordering::Relaxed);
        shared.pending.lock().keyframe = true;
        frame.capture_ts_us = last.saturating_add(1_000);
        shared.on_frame(&frame);
        assert_eq!(shared.last_encoded_us.load(Ordering::Relaxed), last.saturating_add(1_000));
    }

    /// What the crop must never hand on. The path is decided on the geometry tick, but the
    /// frames keep coming while ScreenCaptureKit works through the swap, so the decision is
    /// applied again where a frame arrives: while the target is off screen the frame is counted
    /// as withheld and goes no further — it is not a picture of the target, and under a display
    /// crop it is a picture of whatever is behind it.
    #[test]
    fn a_frame_captured_while_the_target_is_hidden_is_withheld() {
        let (shared, _rx) = shared_for_frames();

        shared.on_frame(&a_frame());
        let seen = shared.stats();
        assert_eq!((seen.captured, seen.withheld, seen.cropped), (1, 0, 1));

        shared.target_hidden.store(true, Ordering::Relaxed);
        shared.on_frame(&a_frame());
        let hidden = shared.stats();
        assert_eq!(
            (hidden.captured, hidden.withheld, hidden.cropped),
            (2, 1, 1),
            "a frame captured while the window was off screen was served from the crop"
        );

        // And the counter that says which path is live is the stream's, not the snapshot's
        // default: a reader of an active crop must not be told it is on the window filter.
        assert!(hidden.on_crop, "a stream on the display crop reported the window filter");
    }

    /// The clock the tracker is told about, so these run in no time and never flake.
    fn at(base: Instant, ms: u64) -> Instant {
        base.checked_add(Duration::from_millis(ms)).expect("inside the clock")
    }

    #[test]
    fn a_source_that_never_draws_is_idle_after_the_grace_and_not_before() {
        let base = Instant::now();
        let mut t = SourceTracker::new(base);
        assert_eq!(t.poll(0, false, at(base, 100)), None, "inside the grace, say nothing");
        assert_eq!(t.poll(0, false, at(base, 399)), None);
        assert_eq!(t.poll(0, false, at(base, 400)), Some(SourceState::Idle));
        assert_eq!(t.poll(0, false, at(base, 900)), None, "only changes are sent");
    }

    #[test]
    fn the_first_frame_makes_it_live_whenever_it_comes() {
        let base = Instant::now();
        let mut t = SourceTracker::new(base);
        assert_eq!(t.poll(0, false, at(base, 500)), Some(SourceState::Idle));
        assert_eq!(t.poll(1, false, at(base, 600)), Some(SourceState::Live));
        assert_eq!(t.poll(2, false, at(base, 700)), None);
    }

    #[test]
    fn a_source_that_drew_once_and_stopped_goes_idle_again() {
        // The latch this replaces reported `Live` for the rest of the stream, so a window that
        // drew a frame and was then hidden left the receiver asking for refreshes.
        let base = Instant::now();
        let mut t = SourceTracker::new(base);
        assert_eq!(t.poll(1, false, at(base, 100)), Some(SourceState::Live));
        assert_eq!(t.poll(1, false, at(base, 1_000)), None, "quiet, but not for long enough");
        assert_eq!(t.poll(1, false, at(base, 2_100)), Some(SourceState::Idle));
        assert_eq!(t.poll(2, false, at(base, 2_200)), Some(SourceState::Live), "it drew again");
    }

    #[test]
    fn a_slow_target_does_not_flap_between_the_two() {
        // One frame a second: quiet by the 400 ms rule, live by the one that matters.
        let base = Instant::now();
        let mut t = SourceTracker::new(base);
        assert_eq!(t.poll(1, false, at(base, 0)), Some(SourceState::Live));
        for second in 1..10 {
            let now = at(base, second * 1_000);
            assert_eq!(t.poll(second, false, now), None, "no change at {second} s");
        }
    }

    #[test]
    fn a_hidden_target_is_idle_at_once_however_much_it_drew() {
        let base = Instant::now();
        let mut t = SourceTracker::new(base);
        assert_eq!(t.poll(10, false, at(base, 100)), Some(SourceState::Live));
        assert_eq!(t.poll(10, true, at(base, 150)), Some(SourceState::Idle), "no grace for this");
        // And the frames that were already in flight when it went do not undo it.
        assert_eq!(t.poll(11, true, at(base, 200)), None);
        assert_eq!(t.poll(12, false, at(base, 250)), Some(SourceState::Live), "back on screen");
    }

    #[test]
    fn a_crop_is_allowed_only_on_screen_on_one_display_and_uncovered() {
        assert_eq!(crop_allowed(true, Some(CROP), false), Some(CROP));
        assert_eq!(crop_allowed(false, Some(CROP), false), None, "minimised or another Space");
        assert_eq!(crop_allowed(true, None, false), None, "partly off screen");
        assert_eq!(crop_allowed(true, Some(CROP), true), None, "covered");
    }

    #[test]
    fn a_transition_commits_only_when_every_call_succeeded() {
        let t = Transitions::default();
        assert_eq!(t.settle(), Settled::Idle);
        assert!(t.begin(WindowPath::DisplayCrop, Some(CROP), 2));
        assert_eq!(t.settle(), Settled::Busy);
        assert!(!t.begin(WindowPath::Filter, None, 1), "one at a time");
        t.on_result(true);
        assert_eq!(t.settle(), Settled::Busy, "one callback still to come");
        t.on_result(true);
        assert_eq!(t.settle(), Settled::Commit(WindowPath::DisplayCrop, Some(CROP)));
        assert_eq!(t.settle(), Settled::Idle, "taken once");
    }

    #[test]
    fn a_failed_call_leaves_the_stream_where_it_was_and_the_next_tick_retries() {
        let t = Transitions::default();
        assert!(t.begin(WindowPath::Filter, None, 2));
        t.on_result(true);
        t.on_result(false);
        assert_eq!(t.settle(), Settled::Failed(WindowPath::Filter, None));
        // Nothing committed, nothing in flight: the next tick may ask again.
        assert_eq!(t.settle(), Settled::Idle);
        assert!(t.begin(WindowPath::Filter, None, 2));
    }

    /// The encode latency is the time between a frame's submit and its return, matched by
    /// pts whatever order the encoder returns them in; a return with no submit on record is
    /// not a sample, and the record holds `IN_FLIGHT_MAX` frames at most.
    #[test]
    fn encode_latency_is_matched_by_pts_and_bounded_in_flight() {
        let counters = Counters::new();
        counters.submitted(1, 1_000);
        counters.submitted(2, 2_000);
        counters.returned(2, 2_500);
        counters.returned(1, 4_000);
        counters.returned(9, 5_000);
        let encode = counters.snapshot().encode;
        assert_eq!((encode.n, encode.p50_us, encode.max_us), (2, 3_000, 3_000), "500 and 3 000 µs");
        for pts in 0..u64::try_from(IN_FLIGHT_MAX).unwrap_or(u64::MAX) {
            counters.submitted(100 + pts, 10_000);
        }
        counters.submitted(200, 10_000);
        assert_eq!(
            counters.in_flight.lock().front().map(|f| f.0),
            Some(101),
            "the oldest forgotten"
        );
        counters.returned(100, 20_000);
        assert_eq!(counters.snapshot().encode.n, 2, "a forgotten frame is not a sample");
    }

    /// A datagram goes out when the queue has room; a full or closed queue drops it and counts
    /// the drop, so the stats say what the link never saw.
    #[test]
    fn a_full_or_closed_queue_drops_the_datagram_and_counts_it() {
        let (shared, mut rx) = shared_with_queue(1);
        assert!(shared.push(Bytes::from_static(b"a")));
        assert!(!shared.push(Bytes::from_static(b"b")), "the queue holds one");
        assert_eq!(rx.try_recv().map(|q| q.datagram), Ok(Bytes::from_static(b"a")));
        assert!(shared.push(Bytes::from_static(b"c")), "room again once drained");
        drop(rx);
        assert!(!shared.push(Bytes::from_static(b"d")), "closed");
        let stats = shared.stats();
        assert_eq!((stats.datagrams, stats.queue_full), (2, 2));
    }

    fn drain(rx: &mut mpsc::Receiver<Queued>) -> Vec<Bytes> {
        std::iter::from_fn(|| rx.try_recv().ok().map(|q| q.datagram)).collect()
    }

    /// An access unit from the encoder is packetized into the queue and counted; a NACK for a
    /// frame in the packetizer's history answers with those fragments, one outside it with
    /// nothing, and none at all while QUIC holds more than the frame budget.
    #[test]
    fn an_encoded_packet_is_queued_and_a_nack_answers_from_history() {
        let (shared, mut rx) = shared_with_queue(DATAGRAM_QUEUE);
        shared.counters.bitrate_bps.store(30_000_000, Ordering::Relaxed);
        let packet = EncodedPacket {
            data: vec![7; 3000],
            keyframe: true,
            ltr_token: Some(1),
            ltr_refresh: false,
            pts_us: host_now_us(),
        };
        shared.on_packet(&packet);
        let sent = drain(&mut rx);
        assert!(sent.len() >= 3, "3 000 bytes under the MTU: {}", sent.len());
        let stats = shared.stats();
        assert_eq!((stats.encoded, stats.datagrams), (1, sent.len() as u64));
        shared.nack(0, &[0, 1]);
        assert_eq!(drain(&mut rx).len(), 2, "two fragments of frame 0 again");
        shared.nack(0, &[]);
        assert!(!drain(&mut rx).is_empty(), "no fragments named: the whole frame's data");
        shared.nack(9, &[0]);
        assert!(drain(&mut rx).is_empty(), "frame 9 was never sent");
        shared.budget.set_held(10_000_000);
        shared.nack(0, &[0]);
        assert!(drain(&mut rx).is_empty(), "QUIC is holding seconds of frames");
    }

    /// A refresh request marks the next frame; a report hands acknowledged LTR tokens to the
    /// encoder's options and counts the datagrams sent since the last one.
    #[test]
    fn a_refresh_and_a_report_reach_the_next_frame_options() {
        let (shared, _rx) = shared_with_queue(DATAGRAM_QUEUE);
        shared.request_refresh(41);
        assert!(shared.pending.lock().refresh);
        assert_eq!(shared.stats().refreshes, 1);
        let mut acked_ltr = [0; 4];
        acked_ltr[..2].copy_from_slice(&[5, 6]);
        let report = ReceiverReport { acked_ltr, acked_ltr_len: 2, ..ReceiverReport::default() };
        let _decision = shared.report(&report, None);
        assert_eq!(shared.pending.lock().acked, vec![5, 6], "two of the four slots were valid");
        assert_eq!(shared.sent_at_report.load(Ordering::Relaxed), 0, "nothing sent yet");
    }

    /// Audio goes out while the source is loud and for the hold after it; silence past the
    /// hold sends nothing, and each packet carries the next sequence number.
    #[test]
    fn audio_is_gated_by_silence_and_numbered() -> Result<(), String> {
        let (shared, _rx) = shared_with_queue(DATAGRAM_QUEUE);
        let samples = usize::try_from(FRAME_SAMPLES * CHANNELS).map_err(|e| e.to_string())?;
        let quiet = vec![0.0_f32; samples];
        let loud: Vec<f32> = (0..samples).map(|i| if i % 2 == 0 { 0.5 } else { -0.5 }).collect();
        let start = 1_000_000_u64;
        assert!(shared.encode_audio(&quiet, start).is_none(), "silence from the start");
        let first = shared.encode_audio(&loud, start).ok_or("loud: a packet")?;
        let second = shared.encode_audio(&loud, start + 20_000).ok_or("loud again")?;
        let seqs: Vec<u32> = first.iter().chain(&second).map(|(seq, _)| *seq).collect();
        assert_eq!(seqs, vec![1, 2], "one packet per frame, numbered from 1");
        assert!(
            shared.encode_audio(&quiet, start + 20_000 + AUDIO_HOLD_US).is_some(),
            "silence inside the hold still goes out"
        );
        assert!(
            shared.encode_audio(&quiet, start + 20_001 + AUDIO_HOLD_US).is_none(),
            "and past it, nothing"
        );
        Ok(())
    }

    #[test]
    fn the_transport_budget_the_queue_age_and_the_crop_knob_are_plain_values() {
        let budget = DatagramBudget::default();
        assert_eq!((budget.get(), budget.held(), budget.cwnd()), (MAX_DATAGRAM, 0, 0));
        budget.set(1200);
        budget.set_held(4096);
        budget.set_cwnd(4920);
        assert_eq!((budget.get(), budget.held(), budget.cwnd()), (1200, 4096, 4920));
        let shared_budget = budget.clone();
        budget.set_held(0);
        assert_eq!(shared_budget.held(), 0, "clones share the counters");
        assert_eq!(shared_budget.cwnd(), 4920, "the window too");

        let queued = Queued { datagram: Bytes::new(), queued_at_us: host_now_us() - 5_000 };
        assert!(queued.waited_us() >= 5_000, "age is measured from when it was queued");
        let future = Queued { datagram: Bytes::new(), queued_at_us: u64::MAX };
        assert_eq!(future.waited_us(), 0, "a clock step back reads as no wait, not a wrap");

        assert_eq!(send_ms_lo(0), 0);
        assert_eq!(send_ms_lo(255_999), 255);
        assert_eq!(send_ms_lo(256_000), 0, "the low byte of the millisecond clock wraps");

        assert!(crop_windows_from(None), "the build default is the crop");
        assert!(crop_windows_from(Some("crop")));
        assert!(!crop_windows_from(Some("window")));
        assert!(crop_windows_from(Some("anything else")), "an unknown value is the default");

        let (shared, _rx) = shared_for_frames();
        assert!(!shared.filter_stalled.load(Ordering::Relaxed));
        shared.sibling_went();
        shared.sibling_went();
        assert!(shared.filter_stalled.load(Ordering::Relaxed), "the next tick re-filters");
        assert_eq!(shared.stats().siblings, 2, "and it is counted, not a suspicion");
        assert_eq!(shared.stats().suspicions, 0);
        assert!(!shared.suspected_at(host_now_us()), "a sibling holds no frames");
    }

    #[test]
    fn a_frame_fits_unless_the_queue_or_quic_holds_too_much() {
        // 30 Mbit/s at 60 fps: 62.5 KB per frame, two frames may be held.
        assert!(frame_fits(DATAGRAM_QUEUE, 0, 5_808, 30_000_000, 60));
        assert!(frame_fits(DATAGRAM_QUEUE, 125_000, 5_808, 30_000_000, 60));
        assert!(!frame_fits(DATAGRAM_QUEUE, 125_001, 5_808, 30_000_000, 60));
        // The datagram queue's low-water mark still applies.
        assert!(!frame_fits(LOW_WATER - 1, 0, 5_808, 30_000_000, 60));
        // A collapsed path: 1.8 Mbit/s at 60 fps is 3.7 KB a frame, so the budget is 7.4 KB —
        // 33 ms of queue against the 146 ms a fixed 32 KB floor would have allowed at this
        // rate, which is the whole point of sizing it from the rate in force.
        assert!(frame_fits(DATAGRAM_QUEUE, 7_500, 4_920, 1_800_000, 60));
        assert!(!frame_fits(DATAGRAM_QUEUE, 7_501, 4_920, 1_800_000, 60));
        assert!(!frame_fits(DATAGRAM_QUEUE, 32 * 1024, 4_920, 1_800_000, 60));
        // One window is the floor: past `rtt × fps` of 1.8 two frames fall under it, and bytes
        // inside the window leave on the next acknowledgement rather than standing in a queue.
        // 1 Mbit/s at 60 fps is 2 083 B a frame, two of them 4 166, under a 4 920 B window.
        assert!(frame_fits(DATAGRAM_QUEUE, 4_920, 4_920, 1_000_000, 60));
        assert!(!frame_fits(DATAGRAM_QUEUE, 4_921, 4_920, 1_000_000, 60));
        assert!(frame_fits(DATAGRAM_QUEUE, 0, 0, 0, 0), "nothing known, nothing held");
    }

    #[test]
    fn a_keyframe_fits_while_the_link_drains_one_inside_the_budget() {
        // 30 Mbit/s is 3.75 MB/s, so 400 ms carries 1.5 MB: the ladder's biggest keyframe is
        // nothing to a healthy link and must not be deferred there.
        assert!(keyframe_fits(133_960, 0, 30_000_000));
        assert!(keyframe_fits(1_500_000, 0, 30_000_000));
        assert!(!keyframe_fits(1_500_001, 0, 30_000_000));
        // Bytes already queued come out of the same budget.
        assert!(!keyframe_fits(1_500_000, 1, 30_000_000));
        // The `lte` rung settles around 9 Mbit/s, which carries 450 kB: still no deferral. Only
        // the collapsed rung earns one.
        assert!(keyframe_fits(133_960, 0, 9_000_000));
        // The 1 Mbit/s floor is 125 kB/s, so 400 ms carries 50 kB, and the same 134 kB keyframe —
        // 1.07 s of that link on its own — is refused.
        assert!(!keyframe_fits(133_960, 0, 1_000_000));
        assert!(keyframe_fits(50_000, 0, 1_000_000));
        assert!(!keyframe_fits(50_001, 0, 1_000_000));
        // No estimate yet: a stream's first keyframe is never refused, whatever the link.
        assert!(keyframe_fits(0, 4 * 1024 * 1024, 0));
    }

    #[test]
    fn the_keyframe_estimate_rises_at_once_and_falls_by_an_eighth() {
        assert_eq!(keyframe_estimate(0, 133_960), 133_960, "the first one is the estimate");
        assert_eq!(keyframe_estimate(10_000, 133_960), 133_960, "a jump is taken whole");
        // Falling: 133 960 - 16 745 + 1 250 = 118 465. One cheap keyframe moves it 12 %, so a
        // blank screen between two busy ones cannot license the next expensive one.
        assert_eq!(keyframe_estimate(133_960, 10_000), 118_465);
        assert_eq!(keyframe_estimate(8, 8), 8, "a steady stream stays put");
    }

    #[test]
    fn a_keyframe_is_deferred_only_when_a_refresh_can_go_out_instead() {
        let (shared, _rx) = shared_for_frames();
        shared.keyframe_bytes.store(133_960, Ordering::Relaxed);
        shared.counters.bitrate_bps.store(1_000_000, Ordering::Relaxed);
        // The client has never acknowledged a reference, so a refresh would decode into nothing
        // and the keyframe goes out however badly it fits.
        assert!(shared.keyframe_admitted(1_000_000));
        assert_eq!(shared.stats().keyframes_deferred, 0);

        let mut acked_ltr = [0; 4];
        acked_ltr[0] = 7;
        let report = ReceiverReport { acked_ltr, acked_ltr_len: 1, ..ReceiverReport::default() };
        shared.report(&report, None);
        assert!(!shared.keyframe_admitted(1_000_000), "now a refresh is a picture");
        assert_eq!(shared.stats().keyframes_deferred, 1);

        // The run is one episode however many frames it spans, and the valve opens a second in.
        assert!(!shared.keyframe_admitted(1_016_000));
        assert!(!shared.keyframe_admitted(1_999_999));
        assert_eq!(shared.stats().keyframes_deferred, 1);
        assert!(shared.keyframe_admitted(2_000_000), "the valve opens rather than hold forever");
    }

    #[test]
    fn a_link_that_recovers_ends_the_deferral_without_the_valve() {
        let (shared, _rx) = shared_for_frames();
        shared.ltr_acked.store(true, Ordering::Relaxed);
        shared.keyframe_bytes.store(133_960, Ordering::Relaxed);
        shared.counters.bitrate_bps.store(1_000_000, Ordering::Relaxed);
        assert!(!shared.keyframe_admitted(1_000_000));
        // The rate controller climbed back: 12 Mbit/s carries 600 kB in the drain window.
        shared.counters.bitrate_bps.store(12_000_000, Ordering::Relaxed);
        assert!(shared.keyframe_admitted(1_100_000), "not the valve, the link");
        assert_eq!(shared.stats().keyframes_deferred, 1, "still the one episode");
    }
}
