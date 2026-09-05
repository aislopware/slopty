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
    Capture, CaptureConfig, CaptureError, CapturedAudio, CapturedFrame, Crop, PixelFormat, Rect,
    Shareable, Target, host_now_us,
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
/// Cursor sample period (120 Hz); a datagram goes out only when the position changed.
const CURSOR_PERIOD: Duration = Duration::from_micros(8_333);
/// Cursor ticks between re-reads of the target's bounds (10 Hz).
const BOUNDS_EVERY: u64 = 12;

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
    /// Frames ScreenCaptureKit delivered through the display-crop path (a window served as
    /// a `sourceRect` of its display rather than through the window filter).
    pub cropped: u64,
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
}

#[derive(Default, Debug)]
struct RegistryInner {
    live: Vec<(String, CaptureTarget, StatsHandle)>,
    closed: VecDeque<ScreenSummary>,
}

impl Registry {
    /// Track a stream `client` opened.
    pub fn insert(
        &self,
        client: &impl std::fmt::Display,
        target: CaptureTarget,
        handle: StatsHandle,
    ) {
        self.inner.lock().live.push((client.to_string(), target, handle));
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
            cropped: self.cropped.load(Ordering::Relaxed),
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
        self.0.counters.snapshot()
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
    out: mpsc::Sender<Bytes>,
    budget: DatagramBudget,
    counters: Counters,
}

impl Shared {
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
        match self.out.try_send(datagram) {
            Ok(()) => {
                self.counters.datagrams.fetch_add(1, Ordering::Relaxed);
                self.last_push_us.store(host_now_us(), Ordering::Relaxed);
                true
            }
            Err(_full_or_closed) => {
                self.counters.queue_full.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// ScreenCaptureKit delivered a frame.
    fn on_frame(&self, frame: &CapturedFrame) {
        self.counters.captured.fetch_add(1, Ordering::Relaxed);
        self.counters.capture.lock().push(frame.latency_us);
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
fn wanted_crop(id: slopty_core::WindowId, bounds: &Rect, point_scale: f64) -> Option<Crop> {
    let crop = slopty_capture::display_enclosing(bounds).and_then(|display| {
        let display = slopty_capture::display_bounds(display);
        slopty_capture::crop_for(bounds, &display, point_scale).map(|(crop, _pixels)| crop)
    });
    let owner = slopty_capture::window_owner_pid(id)?;
    let occluded = slopty_capture::occluded(id, bounds, owner);
    crop_allowed(slopty_capture::window_on_screen(id), crop, occluded)
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
    /// Client input aimed at this stream, in its pixel coordinates.
    injector: Injector,
    point_scale: f64,
    /// Last requested quality; re-applied when the target changes size.
    quality: Quality,
    /// When capture started, so a target that has drawn nothing can be told apart from one
    /// that is merely still starting up.
    opened_at: Instant,
    /// The last [`SourceState`] the client was told, so only changes are sent.
    source_reported: Option<SourceState>,
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
        out: mpsc::Sender<Bytes>,
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
        let cursor = tokio::spawn(cursor_loop(Arc::clone(&shared), target, zoom, point_scale));
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
            injector,
            point_scale,
            quality,
            opened_at: Instant::now(),
            source_reported: None,
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
        self.shared.counters.snapshot()
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
        let live = self.shared.counters.encoded.load(Ordering::Relaxed) > 0;
        let state = if live {
            SourceState::Live
        } else if self.opened_at.elapsed() >= SOURCE_IDLE_AFTER {
            SourceState::Idle
        } else {
            // Still inside the grace: say nothing rather than call a slow start idle.
            return None;
        };
        if self.source_reported == Some(state) {
            return None;
        }
        tracing::debug!(stream = %self.id, ?state, "capture source state");
        self.source_reported = Some(state);
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
        let wanted = wanted_crop(id, rect, self.point_scale);
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
                    tracing::info!(stream = %self.id, "window covered, hidden or off its display: window filter");
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
    pub async fn close(self) {
        self.cursor.abort();
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

/// Sample the pointer and send its position in stream pixels whenever it moves, and a
/// heartbeat whenever nothing at all left for [`HEARTBEAT_AFTER`] (a quiet source must not
/// read as a stalled link). Runs until the task is aborted by [`ScreenStream::close`] or the
/// transport queue closes.
async fn cursor_loop(shared: Arc<Shared>, target: CaptureTarget, zoom: f64, point_scale: f64) {
    let mut ticks = tokio::time::interval(CURSOR_PERIOD);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut bounds: Option<Rect> = None;
    let mut last: Option<(i32, i32, bool)> = None;
    let mut seq: u32 = 0;
    let mut beats: u32 = 0;
    let mut tick: u64 = 0;
    let heartbeat_after_us = u64::try_from(HEARTBEAT_AFTER.as_micros()).unwrap_or(u64::MAX);
    while !shared.out.is_closed() {
        ticks.tick().await;
        let now = host_now_us();
        let silence_us = now.saturating_sub(shared.last_push_us.load(Ordering::Relaxed));
        if silence_us >= heartbeat_after_us {
            beats = beats.wrapping_add(1);
            tracing::trace!(stream = %shared.id, beats, silence_us, "heartbeat");
            shared.counters.heartbeats.fetch_add(1, Ordering::Relaxed);
            shared.push(heartbeat_datagram(shared.id, beats, send_ms_lo(now)));
        }
        if tick.is_multiple_of(BOUNDS_EVERY) {
            bounds = slopty_capture::target_bounds(target);
        }
        tick = tick.wrapping_add(1);
        let Some(rect) = bounds else { continue };
        let (px, py) = slopty_capture::pointer_location();
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
