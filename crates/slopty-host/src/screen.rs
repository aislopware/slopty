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
    Decision, EncodedFrame, HEARTBEAT_AFTER, MediaError, Packetizer, RateController, Redundancy,
    audio_datagram, cursor_datagram, heartbeat_datagram,
};
use slopty_proto::media::MAX_DATAGRAM;
use slopty_proto::screen::{
    CaptureTarget, Quality, ReceiverReport, ScreenEvent, ScreenInput, SourceState, VideoCodec,
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

/// Datagrams buffered between the pipeline and the transport. At 1.2 kB each this is a few
/// 4K keyframes; the capture callback starts dropping frames when fewer than
/// [`LOW_WATER`] slots are free.
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
    /// A callback-based framework call never completed.
    #[error("screen pipeline closed")]
    Closed,
}

/// What the transport reports about the connection, shared by every stream on it.
///
/// The bytes one datagram may carry (starts at the protocol maximum; lowered while QUIC's
/// path MTU is still 1200 bytes) and how many bytes of datagrams QUIC is holding in its send
/// buffer, waiting for the congestion window.
#[derive(Clone, Debug)]
pub struct DatagramBudget {
    max_datagram: Arc<AtomicUsize>,
    held: Arc<AtomicUsize>,
}

impl Default for DatagramBudget {
    fn default() -> Self {
        Self::new()
    }
}

impl DatagramBudget {
    /// Protocol maximum, nothing held.
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_datagram: Arc::new(AtomicUsize::new(MAX_DATAGRAM)),
            held: Arc::new(AtomicUsize::new(0)),
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
}

/// Frames' worth of bytes (at the current target rate) QUIC may hold before a captured frame
/// is dropped instead of encoded.
///
/// The datagram send buffer is 4 MiB; a link whose window collapsed held 275–365 KB of
/// frames for 4–7 s and delivered every one of them stale (MEASUREMENTS.md, "start-up over
/// the mesh"). Two frames keep a keyframe's tail flowing and stop the queue there: the next
/// capture is fresher than anything that would wait behind it.
const HELD_FRAMES: u64 = 2;
/// Floor for the held-bytes limit, so a low target does not drop every frame behind a beat.
const HELD_FLOOR: u64 = 32 * 1024;

/// Whether a captured frame should be encoded given what is queued ahead of it: `free` slots
/// in the datagram queue, `held` bytes in QUIC's send buffer, at `target_bps` and `fps`.
#[must_use]
pub const fn frame_fits(free: usize, held: usize, target_bps: u64, fps: u16) -> bool {
    if free < LOW_WATER {
        return false;
    }
    let fps = if fps == 0 { 1 } else { fps as u64 };
    let per_frame = match (target_bps / 8).checked_div(fps) {
        Some(bytes) => bytes,
        None => 0,
    };
    let limit = per_frame.saturating_mul(HELD_FRAMES);
    let limit = if limit < HELD_FLOOR { HELD_FLOOR } else { limit };
    (held as u64) <= limit
}

/// p50 / p95 / max of a latency over the last [`LATENCY_WINDOW`] samples, microseconds.
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
    /// Worst capture-to-packet latency seen, microseconds.
    pub latency_max_us: u64,
    /// Sum of capture-to-packet latencies, microseconds (divide by `encoded`).
    pub latency_sum_us: u64,
    /// Opus packets sent.
    pub audio_packets: u64,
    /// Bitrate the controller last asked the encoder for.
    pub bitrate_bps: u64,
    /// Capture latency: the window server's display time of a frame → ScreenCaptureKit's
    /// callback (what SCK adds), over the last [`LATENCY_WINDOW`] frames.
    pub capture: Quantiles,
    /// Encode latency: `VTCompressionSessionEncodeFrame` → the output callback.
    pub encode: Quantiles,
    /// Time between two heartbeats, over the last [`LATENCY_WINDOW`] of them. The beat is what
    /// tells the receiver the host is alive while nothing is being drawn, and the receiver calls
    /// a silence of `STALL_GAP` a stall, so this is the number that says whether the host is
    /// keeping its own promise.
    pub beat_gap: Quantiles,
    /// How long the window-geometry call in the cursor loop took, over the last
    /// [`LATENCY_WINDOW`] of them: the work the beat used to wait behind.
    pub bounds: Quantiles,
    /// The longest gap between two beats since the stream opened, microseconds. The quantiles
    /// above are over a sliding window of [`LATENCY_WINDOW`] beats — about twenty seconds — so
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
#[derive(Debug)]
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

/// Enumerate shareable content, reusing an enumeration younger than [`SHAREABLE_TTL`].
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
    /// Capture frame rate, for the held-bytes limit.
    fps: std::sync::atomic::AtomicU16,
    /// Whether frames come through the display-crop path right now.
    cropped: std::sync::atomic::AtomicBool,
    /// The target window is not on screen. Nothing ScreenCaptureKit delivers can be a picture of
    /// it, so nothing is sent: under a display crop the rectangle holds whatever is behind the
    /// window, and a swap to the window filter that the framework rejected leaves the crop
    /// running while this side believes otherwise. Set from the geometry tick.
    target_hidden: std::sync::atomic::AtomicBool,
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
        let fits = frame_fits(
            self.out.capacity(),
            self.budget.held(),
            self.counters.bitrate_bps.load(Ordering::Relaxed),
            self.fps.load(Ordering::Relaxed),
        );
        if !fits {
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            // The client will see a hole; make the next frame decodable on its own.
            self.pending.lock().refresh = true;
            return;
        }
        let options = {
            let mut pending = self.pending.lock();
            FrameOptions {
                force_keyframe: std::mem::take(&mut pending.keyframe),
                force_ltr_refresh: std::mem::take(&mut pending.refresh),
                acked_ltr: std::mem::take(&mut pending.acked),
            }
        };
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
    match std::env::var("SLOPTY_WINDOW_CAPTURE").as_deref() {
        Ok("window") => false,
        Ok("crop") => true,
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
    /// Datagrams are queued on `out`; `on_stop` fires if ScreenCaptureKit ends the stream
    /// (window closed, permission revoked).
    pub async fn open(
        id: StreamId,
        target: CaptureTarget,
        quality: Quality,
        out: mpsc::Sender<Queued>,
        budget: DatagramBudget,
        on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
    ) -> Result<(Self, ScreenEvent), ScreenError> {
        let on_stop: Arc<dyn Fn(CaptureError) + Send + Sync> = Arc::new(on_stop);
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
            cropped: std::sync::atomic::AtomicBool::new(path == WindowPath::DisplayCrop),
            target_hidden: std::sync::atomic::AtomicBool::new(false),
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
        self.shared.fps.store(capture_config.fps, Ordering::Relaxed);
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

    /// Fold in a receiver report: LTR acks feed the encoder, loss feeds the parity ratio, and
    /// loss, queueing, stalls and the QUIC path (`path`) drive the bitrate. Returns the
    /// controller's decision when this report completed a decision window.
    pub fn report(&self, report: &ReceiverReport, path: Option<PathSample>) -> Option<Decision> {
        let acked = report.acked_ltr.iter().take(usize::from(report.acked_ltr_len)).copied();
        self.shared.pending.lock().acked.extend(acked);
        let sent_total = self.shared.packetizer.lock().datagrams_sent();
        let previous = self.shared.sent_at_report.swap(sent_total, Ordering::Relaxed);
        let sent = u32::try_from(sent_total.saturating_sub(previous)).unwrap_or(u32::MAX);
        let permille = self.shared.redundancy.lock().on_report(report, sent);
        self.shared.packetizer.lock().set_parity_permille(permille);
        let decision = self.shared.rate.lock().on_report(report, sent, path)?;
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
            self.shared.apply_bitrate(decision.target_bps);
        }
        Some(decision)
    }

    /// The bitrate the controller is asking the encoder for right now.
    #[must_use]
    pub fn bitrate_bps(&self) -> u32 {
        self.shared.rate.lock().target_bps()
    }

    /// The client lost a frame it cannot recover: make the next frame stand on its own.
    pub fn request_refresh(&self, last_good_frame: u32) {
        tracing::debug!(stream = %self.id, last_good_frame, "refresh requested");
        self.shared.counters.refreshes.fetch_add(1, Ordering::Relaxed);
        self.shared.pending.lock().refresh = true;
    }

    /// Retransmit fragments of a recent frame, unless QUIC is already holding more than the
    /// frame budget: an answer that leaves behind seconds of queued frames arrives after the
    /// receiver has given up, and on a collapsed link every NACK answered that way stacked
    /// another copy of the frame into the queue (64 000 datagrams for 12 frames,
    /// MEASUREMENTS.md "start-up over the mesh").
    pub fn nack(&self, frame: u32, fragments: &[u16]) {
        let fits = frame_fits(
            self.shared.out.capacity(),
            self.shared.budget.held(),
            self.shared.counters.bitrate_bps.load(Ordering::Relaxed),
            self.shared.fps.load(Ordering::Relaxed),
        );
        if !fits {
            tracing::debug!(stream = %self.id, frame, held = self.shared.budget.held(), "nack not answered: transport is holding frames");
            return;
        }
        let datagrams = self.shared.packetizer.lock().retransmit(frame, fragments);
        if datagrams.is_empty() {
            tracing::debug!(stream = %self.id, frame, "nack for a frame outside the history");
        }
        for datagram in datagrams {
            if !self.shared.push(datagram) {
                break;
            }
        }
    }

    /// Stop capturing and tear down.
    pub async fn close(mut self) {
        self.cursor.abort();
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

#[cfg(test)]
mod tests {
    use super::*;

    const CROP: Crop = Crop { x: 10.0, y: 20.0, w: 300.0, h: 200.0 };

    /// A stream's shared state with no encoder and nowhere to send: enough to drive
    /// [`Shared::on_frame`] and read what it counted.
    fn shared_for_frames() -> (Arc<Shared>, mpsc::Receiver<Queued>) {
        let (out, rx) = mpsc::channel(8);
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
            cropped: std::sync::atomic::AtomicBool::new(true),
            target_hidden: std::sync::atomic::AtomicBool::new(false),
            suspect_until_us: AtomicU64::new(0),
            filter_stalled: std::sync::atomic::AtomicBool::new(false),
            out,
            budget: DatagramBudget::new(),
            counters: Counters::new(),
        });
        (shared, rx)
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

    #[test]
    fn a_frame_fits_unless_the_queue_or_quic_holds_too_much() {
        // 30 Mbit/s at 60 fps: 62.5 KB per frame, two frames may be held.
        assert!(frame_fits(DATAGRAM_QUEUE, 0, 30_000_000, 60));
        assert!(frame_fits(DATAGRAM_QUEUE, 125_000, 30_000_000, 60));
        assert!(!frame_fits(DATAGRAM_QUEUE, 125_001, 30_000_000, 60));
        // The datagram queue's low-water mark still applies.
        assert!(!frame_fits(LOW_WATER - 1, 0, 30_000_000, 60));
        // At the 1 Mbit/s floor two frames are 4 KB; the 32 KB floor keeps beats and a
        // small keyframe from dropping everything behind them.
        assert!(frame_fits(DATAGRAM_QUEUE, 32 * 1024, 1_000_000, 60));
        assert!(!frame_fits(DATAGRAM_QUEUE, 32 * 1024 + 1, 1_000_000, 60));
        assert!(frame_fits(DATAGRAM_QUEUE, 0, 0, 0), "no rate known: the floor");
    }
}
