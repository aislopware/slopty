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

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use bytes::Bytes;
use parking_lot::{Mutex, RwLock};
use slopty_capture::{
    Capture, CaptureConfig, CaptureError, CapturedAudio, CapturedFrame, PixelFormat, Rect,
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

/// Counters for logs and telemetry.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
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
    /// Worst capture-to-packet latency seen, microseconds.
    pub latency_max_us: u64,
    /// Sum of capture-to-packet latencies, microseconds (divide by `encoded`).
    pub latency_sum_us: u64,
    /// Opus packets sent.
    pub audio_packets: u64,
    /// Bitrate the controller last asked the encoder for.
    pub bitrate_bps: u64,
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
    latency_max_us: AtomicU64,
    latency_sum_us: AtomicU64,
    bitrate_bps: AtomicU64,
}

impl Counters {
    const fn new() -> Self {
        Self {
            audio_packets: AtomicU64::new(0),
            captured: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            encoded: AtomicU64::new(0),
            datagrams: AtomicU64::new(0),
            queue_full: AtomicU64::new(0),
            heartbeats: AtomicU64::new(0),
            latency_max_us: AtomicU64::new(0),
            latency_sum_us: AtomicU64::new(0),
            bitrate_bps: AtomicU64::new(0),
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
            latency_max_us: self.latency_max_us.load(Ordering::Relaxed),
            latency_sum_us: self.latency_sum_us.load(Ordering::Relaxed),
            audio_packets: self.audio_packets.load(Ordering::Relaxed),
            bitrate_bps: self.bitrate_bps.load(Ordering::Relaxed),
        }
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
        let outcome = self
            .encoder
            .read()
            .as_ref()
            .map(|encoder| encoder.encode(frame.image.as_cv(), frame.capture_ts_us, &options));
        if let Some(Err(e)) = outcome {
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
    let capture = CaptureConfig { width, height, fps, format, queue_depth: 2, audio: true };
    let encoder =
        EncoderConfig { width, height, codec, fps, bitrate_bps: quality.bitrate_bps.max(100_000) };
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
        let t0 = Instant::now();
        let content = shareable().await?;
        let enumerated = t0.elapsed();
        let resolved = Target::resolve(&content, target)?;
        let (capture_config, encoder_config) = configs(resolved.pixel_size(), &quality);

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
            on_stop,
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

    /// Counters.
    #[must_use]
    pub fn stats(&self) -> ScreenStats {
        self.shared.counters.snapshot()
    }

    /// Change quality. A size, rate or codec change rebuilds the encoder and reconfigures the
    /// capture; a bitrate-only change is applied in place.
    pub fn set_quality(&mut self, quality: &Quality) -> Result<(), ScreenError> {
        self.quality = *quality;
        let (capture_config, encoder_config) = configs(self.native, quality);
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

    /// Follow the target's size: a window the user resized on the host gets a stream of its new
    /// size (fresh encoder, keyframe) and the client hears `Geometry`. Cheap when nothing
    /// changed (one WindowServer query); call it a few times a second.
    pub fn check_geometry(&mut self) -> Result<Option<ScreenEvent>, ScreenError> {
        let Some(rect) = slopty_capture::target_bounds(self.target) else { return Ok(None) };
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped")]
        let px = |points: f64| (points * self.point_scale).round().clamp(2.0, 16_384.0) as u32;
        let native = (px(rect.w), px(rect.h));
        if native == self.native {
            return Ok(None);
        }
        tracing::info!(stream = %self.id, from = ?self.native, to = ?native, "target resized");
        self.native = native;
        let (capture_config, encoder_config) = configs(native, &self.quality);
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
