//! Remote windows on the client: datagram routing, reassembly, hardware decode, and
//! latest-frame delivery to the UI.
//!
//! [`HostLink`](crate::HostLink) reads every datagram on the connection and hands it to a
//! [`ScreenRouter`], which fans out by stream id. A stream the UI has not attached yet keeps a
//! short backlog (its first datagrams usually beat the `Opened` control message), so nothing
//! is lost at start-up. [`spawn_screen`] runs one task per stream: reassemble, NACK and refresh
//! on the reassembler's schedule, decode, report every 50 ms, and publish the newest decoded
//! frame and cursor position on `watch` channels the UI polls at paint time.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use parking_lot::Mutex;
use slopty_codec::audio::{OpusDecoder, Player};
use slopty_codec::{DecodedFrame, Decoder};
use slopty_core::StreamId;
use slopty_media::{Action, Config, Ingest, Reassembler, ReassemblerStats};
use slopty_proto::ClientMsg;
use slopty_proto::media::{MAX_DATAGRAM, MediaHeader};
use slopty_proto::screen::{Feedback, ScreenRequest, VideoCodec};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::pacing::{CaptureClock, FrameStamp};

/// Datagrams buffered per attached stream before the task must drain them.
const STREAM_DEPTH: usize = 2048;
/// Datagrams kept for a stream nobody has attached yet.
const PENDING_DEPTH: usize = 512;
/// How often a receiver report goes to the host.
const REPORT_EVERY: Duration = Duration::from_millis(50);
/// Reassembler timer resolution while frames are pending.
const TICK: Duration = Duration::from_millis(2);
/// Timer period while nothing is pending (only refresh repeats depend on it).
const IDLE_TICK: Duration = Duration::from_millis(50);
/// RTT assumed before the transport has measured one.
const DEFAULT_RTT: Duration = Duration::from_millis(20);

/// Seed of the loss-injection sequence. Fixed, so a run at a given drop rate repeats exactly
/// and two builds can be compared on the same losses.
const LOSS_SEED: u64 = 0x2545_F491_4F6C_DD1D;

/// Fans incoming datagrams out to per-stream queues.
#[derive(Clone, Debug)]
pub struct ScreenRouter {
    inner: Arc<Mutex<Routes>>,
    /// Test-only loss injection: drop this many datagrams per thousand, from
    /// `SLOPTY_E2E_DROP_PERMILLE` (read once, at construction). Zero in normal use, which is
    /// the only state the shipped app is ever in — nothing sets the variable but the gated
    /// tests in `apps/slopty-hostd/tests/e2e.rs`.
    drop_permille: Arc<AtomicU32>,
    lcg: Arc<AtomicU64>,
}

impl Default for ScreenRouter {
    fn default() -> Self {
        Self {
            inner: Arc::default(),
            drop_permille: Arc::new(AtomicU32::new(0)),
            lcg: Arc::new(AtomicU64::new(LOSS_SEED)),
        }
    }
}

/// A datagram and when the connection handed it over: the stream worker may be busy (a decode,
/// a report) when it lands, and the reassembler's stall clock must not charge that wait to
/// the link.
type Arrival = (Instant, Bytes);

#[derive(Debug, Default)]
struct Routes {
    attached: HashMap<StreamId, mpsc::Sender<Arrival>>,
    pending: HashMap<StreamId, VecDeque<Arrival>>,
}

impl Routes {
    fn deliver(&mut self, stream: StreamId, arrival: Arrival) {
        if let Some(tx) = self.attached.get(&stream) {
            // A full queue means the stream task is behind; dropping is the right call.
            let _dropped = tx.try_send(arrival);
            return;
        }
        let backlog = self.pending.entry(stream).or_default();
        if backlog.len() >= PENDING_DEPTH {
            backlog.pop_front();
        }
        backlog.push_back(arrival);
    }
}

impl ScreenRouter {
    /// Empty router. Reads `SLOPTY_E2E_DROP_PERMILLE` once for loss injection.
    #[must_use]
    pub fn new() -> Self {
        Self::with_loss(
            std::env::var("SLOPTY_E2E_DROP_PERMILLE")
                .or_else(|_unset| std::env::var("SLOPTY_DROP_PERMILLE"))
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .map_or(0, |v| v.min(1000)),
        )
    }

    /// A router that drops `drop_permille` of the datagrams handed to it, from a fixed seed.
    /// The tests call this directly; the app only ever gets zero.
    #[must_use]
    pub fn with_loss(drop_permille: u32) -> Self {
        let router = Self::default();
        router.set_loss(drop_permille);
        router
    }

    /// Drop this many datagrams per thousand from now on, and restart the sequence so a run at
    /// a given rate always sees the same losses. Tests only.
    pub fn set_loss(&self, drop_permille: u32) {
        let permille = drop_permille.min(1000);
        self.lcg.store(LOSS_SEED, Ordering::Relaxed);
        self.drop_permille.store(permille, Ordering::Relaxed);
        if permille > 0 {
            tracing::warn!(drop_permille = permille, "media loss injection is on");
        }
    }

    /// Whether loss injection says to drop this datagram (a 64-bit LCG, no `rand` dependency).
    fn inject_loss(&self) -> bool {
        let permille = self.drop_permille.load(Ordering::Relaxed);
        if permille == 0 {
            return false;
        }
        let next = self
            .lcg
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |x| {
                Some(
                    x.wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407),
                )
            })
            .unwrap_or(0);
        let roll = u32::try_from((next >> 33) % 1000).unwrap_or(0);
        roll < permille
    }

    /// Deliver one datagram that arrived at `now` (called by the connection's datagram reader).
    pub fn route(&self, datagram: Bytes, now: Instant) {
        if self.inject_loss() {
            return;
        }
        let Some((header, _payload)) = MediaHeader::parse(&datagram) else { return };
        let stream = StreamId(header.stream.get());
        self.inner.lock().deliver(stream, (now, datagram));
    }

    /// Start receiving `stream`'s datagrams, backlog first.
    #[must_use]
    pub fn attach(&self, stream: StreamId) -> mpsc::Receiver<Arrival> {
        let (tx, rx) = mpsc::channel(STREAM_DEPTH);
        let mut routes = self.inner.lock();
        if let Some(backlog) = routes.pending.remove(&stream) {
            for datagram in backlog {
                let _full = tx.try_send(datagram);
            }
        }
        routes.attached.insert(stream, tx);
        rx
    }

    /// Stop routing `stream`.
    pub fn detach(&self, stream: StreamId) {
        let mut routes = self.inner.lock();
        routes.attached.remove(&stream);
        routes.pending.remove(&stream);
    }

    /// Drop backlogs for streams nobody attached (call when a `Closed` arrives).
    pub fn forget(&self, stream: StreamId) {
        self.inner.lock().pending.remove(&stream);
    }
}

/// Frames whose arrival instant is remembered while the decoder works on them. VideoToolbox is
/// asynchronous and gives the callback nothing but the presentation timestamp, so the worker
/// parks the stamp under that timestamp and the callback picks it back up. Two frames' worth of
/// a second is plenty; anything older has been answered or dropped.
const ARRIVALS: usize = 128;

/// A decoded picture together with the timing that got it here, which is what the element needs
/// to pace and to say how old what it paints is.
#[derive(Debug)]
pub struct Presentable {
    /// The picture.
    pub frame: DecodedFrame,
    /// Arrival of the datagram that completed the frame, and when the decoder returned it.
    pub stamp: FrameStamp,
}

/// Arrival instants parked by presentation timestamp for the decoder callback.
#[derive(Debug, Default)]
struct Arrivals(VecDeque<(u64, Instant)>);

impl Arrivals {
    fn park(&mut self, pts_us: u64, arrived: Instant) {
        if self.0.len() >= ARRIVALS {
            self.0.pop_front();
        }
        self.0.push_back((pts_us, arrived));
    }

    /// The arrival of the frame with this timestamp, and everything older forgotten with it.
    fn take(&mut self, pts_us: u64) -> Option<Instant> {
        let at = self.0.iter().position(|&(pts, _)| pts == pts_us)?;
        self.0.drain(..=at).next_back().map(|(_pts, t)| t)
    }
}

/// Where the host's pointer is, in stream pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct CursorState {
    /// X.
    pub x: i32,
    /// Y.
    pub y: i32,
    /// Inside the streamed area.
    pub visible: bool,
}

/// Receiver-side counters, refreshed with every report.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ScreenStats {
    /// Frames delivered to the decoder.
    pub frames: u64,
    /// Frames recovered by FEC.
    pub frames_fec: u64,
    /// Frames that needed a retransmission.
    pub frames_retransmit: u64,
    /// Frames given up on.
    pub frames_lost: u64,
    /// Data fragments that never arrived (counted as each frame resolves).
    pub datagrams_lost: u64,
    /// Parity the host is sending, in thousandths of the data fragments, as observed on the
    /// wire: the receiver's view of what the redundancy controller settled on.
    pub parity_permille: u16,
    /// Data and parity fragments the host cut the frames seen so far into. The ratio is
    /// [`Self::parity_permille`]; the counts also say how big the frames were, which is what
    /// decides whether the parity policy could cost anything at all.
    pub data_shards: u64,
    /// Parity fragments the host added to them.
    pub parity_shards: u64,
    /// NACKs sent.
    pub nacks: u64,
    /// Refresh requests sent.
    pub refreshes: u64,
    /// Decoder rejections.
    pub decode_errors: u64,
    /// Datagrams seen.
    pub datagrams: u64,
    /// Bytes received in datagrams (video, cursor, audio, parity).
    pub bytes: u64,
    /// Opus packets played.
    pub audio_packets: u64,
    /// Opus packets missing from the sequence (or too late to play).
    pub audio_lost: u64,
    /// Stalls that released: nothing arrived for a stall gap, then everything at once.
    pub stalls: u64,
    /// Time spent stalled, milliseconds (released stalls plus the one in progress).
    pub stalled_ms: u64,
    /// The link is stalled right now (as of the last report).
    pub stalled: bool,
    /// When the first datagram of the stream arrived.
    pub first_datagram_at: Option<Instant>,
    /// When the first frame was complete and handed to the decoder.
    pub first_frame_at: Option<Instant>,
    /// When the first decoded picture came back.
    pub first_decoded_at: Option<Instant>,
    /// Longest wait from a frame's first fragment to its completion (the first frame's
    /// figure is the keyframe's spread over the wire).
    pub hold_max: Duration,
    /// Median wait from first fragment to completion in the last report window.
    pub hold_p50: Duration,
    /// 95th percentile of the same.
    pub hold_p95: Duration,
    /// RFC 3550 interarrival jitter on the host's capture clock, as last reported.
    pub jitter: Duration,
    /// Frames the worker is holding in order behind a missing one, as last reported.
    pub queue_depth: u8,
}

/// Playback for one stream, created on its first audio packet.
enum AudioSlot {
    /// No audio has arrived yet.
    Unopened,
    /// Playing.
    Open(Audio),
    /// Playback could not start; packets are dropped.
    Failed,
}

/// Decoder, player and sequence state.
struct Audio {
    decoder: OpusDecoder,
    player: Player,
    /// Last sequence played.
    seq: u32,
}

impl Audio {
    fn new() -> Result<Self, slopty_codec::CodecError> {
        Ok(Self { decoder: OpusDecoder::new()?, player: Player::new()?, seq: 0 })
    }
}

/// A live client-side stream. Dropping it stops the task and unroutes the stream; the caller
/// still sends `ScreenRequest::Close` so the host stops capturing.
#[derive(Debug)]
pub struct ScreenHandle {
    stream: StreamId,
    frames: watch::Receiver<Option<Arc<Presentable>>>,
    cursor: watch::Receiver<CursorState>,
    stats: watch::Receiver<ScreenStats>,
    /// Audio is decoded but not played while set (shared with the worker).
    muted: Arc<AtomicBool>,
    /// The host's capture target is producing pictures (shared with the worker).
    source_live: Arc<AtomicBool>,
    router: ScreenRouter,
    task: JoinHandle<()>,
}

impl ScreenHandle {
    /// Stream id.
    #[must_use]
    pub const fn stream(&self) -> StreamId {
        self.stream
    }

    /// Newest decoded frame; `changed().await` wakes when a new one lands. The channel keeps
    /// only the newest, which is the presentation policy: a frame the element never got to
    /// paint is stale by the time it would have.
    #[must_use]
    pub fn frames(&self) -> watch::Receiver<Option<Arc<Presentable>>> {
        self.frames.clone()
    }

    /// Host pointer position.
    #[must_use]
    pub fn cursor(&self) -> watch::Receiver<CursorState> {
        self.cursor.clone()
    }

    /// Counters.
    #[must_use]
    pub fn stats(&self) -> ScreenStats {
        *self.stats.borrow()
    }

    /// Whether audio is silenced on this client. Packets keep arriving and are still decoded
    /// (the Opus state stays continuous), only playback stops; other clients are unaffected.
    #[must_use]
    pub fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    /// Silence or resume audio playback for this stream on this client.
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    /// The host's `ScreenEvent::Source`: whether the capture target is drawing anything. While
    /// it is not, the worker stops asking for refreshes no frame could answer.
    pub fn set_source_live(&self, live: bool) {
        self.source_live.store(live, Ordering::Relaxed);
    }

    /// Whether the host's capture target is drawing, as last reported.
    #[must_use]
    pub fn source_live(&self) -> bool {
        self.source_live.load(Ordering::Relaxed)
    }
}

impl Drop for ScreenHandle {
    fn drop(&mut self) {
        self.task.abort();
        self.router.detach(self.stream);
    }
}

/// How a screen worker talks back to the host.
pub struct Uplink {
    /// Control stream (reports, close).
    pub control: mpsc::Sender<ClientMsg>,
    /// Sends one loss-feedback datagram; `false` once the connection is gone.
    pub feedback: Box<dyn Fn(Bytes) -> bool + Send>,
    /// Current round-trip estimate of the selected path.
    pub rtt: Box<dyn Fn() -> Option<Duration> + Send>,
}

impl std::fmt::Debug for Uplink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Uplink").finish_non_exhaustive()
    }
}

/// Start receiving `stream`: reassembly, decode, reports and NACK/refresh feedback on `uplink`.
#[must_use]
pub fn spawn_screen(
    runtime: &tokio::runtime::Handle,
    router: &ScreenRouter,
    stream: StreamId,
    codec: VideoCodec,
    uplink: Uplink,
) -> ScreenHandle {
    let datagrams = router.attach(stream);
    let (frames_tx, frames) = watch::channel(None);
    let (cursor_tx, cursor) = watch::channel(CursorState::default());
    let (stats_tx, stats) = watch::channel(ScreenStats::default());
    let muted = Arc::new(AtomicBool::new(false));
    let source_live = Arc::new(AtomicBool::new(true));
    let first_decoded = Arc::new(Mutex::new(None));
    let decoded_at = Arc::clone(&first_decoded);
    let arrivals = Arc::new(Mutex::new(Arrivals::default()));
    let parked = Arc::clone(&arrivals);
    // Counted here, on the decoder's side of the newest-only channel, so the element can tell
    // how many pictures the channel swallowed before it looked.
    let decode_seq = Arc::new(AtomicU64::new(0));
    let seq = Arc::clone(&decode_seq);
    let decoder = Decoder::new(codec, move |frame| {
        let decoded = Instant::now();
        decoded_at.lock().get_or_insert(decoded);
        // A picture whose arrival is no longer parked (a duplicate from the decoder, or one
        // that outlived the ring) is still shown; its timing simply does not enter the ring.
        let arrived = parked.lock().take(frame.pts_us).unwrap_or(decoded);
        let stamp = FrameStamp {
            pts_us: frame.pts_us,
            decode_seq: seq.fetch_add(1, Ordering::Relaxed),
            arrived,
            decoded,
        };
        let _no_receiver = frames_tx.send(Some(Arc::new(Presentable { frame, stamp })));
    });
    let worker = Worker {
        stream,
        datagrams,
        reassembler: Reassembler::new(stream, Config::default(), Instant::now()),
        decoder,
        arrivals,
        out: uplink.control,
        feedback: uplink.feedback,
        rtt: uplink.rtt,
        cursor: cursor_tx,
        stats: stats_tx,
        cursor_seq: None,
        counters: ScreenStats::default(),
        audio: AudioSlot::Unopened,
        muted: Arc::clone(&muted),
        source_live: Arc::clone(&source_live),
        source_hint: true,
        first_decoded,
        capture_clock: CaptureClock::new(),
    };
    let task = runtime.spawn(worker.run());
    ScreenHandle { stream, frames, cursor, stats, muted, source_live, router: router.clone(), task }
}

struct Worker {
    stream: StreamId,
    datagrams: mpsc::Receiver<Arrival>,
    reassembler: Reassembler,
    decoder: Decoder,
    /// Arrival instants parked for the decoder callback, keyed by presentation timestamp.
    arrivals: Arc<Mutex<Arrivals>>,
    out: mpsc::Sender<ClientMsg>,
    feedback: Box<dyn Fn(Bytes) -> bool + Send>,
    rtt: Box<dyn Fn() -> Option<Duration> + Send>,
    cursor: watch::Sender<CursorState>,
    stats: watch::Sender<ScreenStats>,
    cursor_seq: Option<u32>,
    counters: ScreenStats,
    audio: AudioSlot,
    /// Decode but do not play while set.
    muted: Arc<AtomicBool>,
    /// The host says its capture target is producing pictures.
    source_live: Arc<AtomicBool>,
    /// The last hint handed to the reassembler, so a hint that has not changed does not
    /// overwrite what the stream itself proved (a video fragment means the source is live,
    /// whatever the host last said).
    source_hint: bool,
    /// Set by the decoder callback when the first picture comes back.
    first_decoded: Arc<Mutex<Option<Instant>>>,
    /// Widens the wire's 32-bit capture timestamp, so ordering survives its ~71-minute wrap.
    capture_clock: CaptureClock,
}

impl Worker {
    /// Decode and queue one Opus packet; late duplicates are dropped, gaps counted.
    fn play_audio(&mut self, seq: u32, payload: &Bytes) {
        if matches!(self.audio, AudioSlot::Unopened) {
            self.audio = match Audio::new() {
                Ok(audio) => AudioSlot::Open(audio),
                Err(e) => {
                    tracing::warn!(error = %e, "audio playback unavailable");
                    AudioSlot::Failed
                }
            };
        }
        let AudioSlot::Open(audio) = &mut self.audio else { return };
        let gap = seq.wrapping_sub(audio.seq);
        if audio.seq != 0 && (gap == 0 || gap > u32::MAX / 2) {
            self.counters.audio_lost = self.counters.audio_lost.saturating_add(1);
            return;
        }
        if audio.seq != 0 && gap > 1 {
            self.counters.audio_lost =
                self.counters.audio_lost.saturating_add(u64::from(gap.saturating_sub(1)));
        }
        audio.seq = seq;
        match audio.decoder.decode(payload) {
            Ok(pcm) => {
                if !self.muted.load(Ordering::Relaxed) {
                    audio.player.push(pcm);
                }
                self.counters.audio_packets = self.counters.audio_packets.saturating_add(1);
            }
            Err(e) => tracing::debug!(stream = %self.stream, error = %e, "opus decode"),
        }
    }

    async fn run(mut self) {
        let mut report = tokio::time::interval(REPORT_EVERY);
        report.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let busy = self.reassembler.queue_depth() > 0 || self.reassembler.awaiting_refresh();
            let tick = tokio::time::sleep(if busy { TICK } else { IDLE_TICK });
            tokio::select! {
                arrival = self.datagrams.recv() => {
                    let Some((at, datagram)) = arrival else { break };
                    self.ingest(&datagram, at);
                    // Everything that is already queued (a burst, or the backlog from before
                    // the stream was attached) goes in before the timers look at frame ages:
                    // the stamps say when those datagrams arrived, not when they were read.
                    while let Ok((at, datagram)) = self.datagrams.try_recv() {
                        self.ingest(&datagram, at);
                    }
                }
                () = tick => {}
                _instant = report.tick() => self.report().await,
            }
            if !self.actions() {
                break;
            }
            // The tick's own `drain` can release a frame that was queued behind a lost one, and
            // on a still screen the next datagram is a heartbeat half a stall gap away: without
            // this the picture would wait for it.
            self.deliver();
        }
        tracing::debug!(stream = %self.stream, "screen worker finished");
    }

    /// Feed one datagram that the connection handed over at `now`.
    fn ingest(&mut self, datagram: &Bytes, now: Instant) {
        self.counters.datagrams = self.counters.datagrams.saturating_add(1);
        let len = u64::try_from(datagram.len()).unwrap_or(u64::MAX);
        self.counters.bytes = self.counters.bytes.saturating_add(len);
        self.counters.first_datagram_at.get_or_insert(now);
        match self.reassembler.ingest(datagram, now) {
            Ingest::Video => self.deliver(),
            Ingest::Cursor { seq, update } => {
                let newer =
                    self.cursor_seq.is_none_or(|last| seq.wrapping_sub(last) < u32::MAX / 2);
                if newer {
                    self.cursor_seq = Some(seq);
                    let state = CursorState {
                        x: update.x.get(),
                        y: update.y.get(),
                        visible: update.visible != 0,
                    };
                    self.cursor.send_replace(state);
                }
            }
            Ingest::Audio { seq, payload } => self.play_audio(seq, &payload),
            Ingest::Heartbeat | Ingest::Ignored(_) => {}
        }
    }

    /// Push every frame that is now complete into the decoder.
    fn deliver(&mut self) {
        while let Some(frame) = self.reassembler.next_frame() {
            self.counters.frames = self.counters.frames.saturating_add(1);
            self.counters.first_frame_at.get_or_insert_with(Instant::now);
            if frame.hold > self.counters.hold_max {
                self.counters.hold_max = frame.hold;
            }
            // Widened here, at the one place the wire's 32-bit stamp becomes a u64: the decoder
            // echoes whatever it is given back to the callback, so the parked arrivals and the
            // pacer's ordering both inherit a timestamp that survives the wrap.
            let pts = self.capture_clock.widen(frame.info.capture_ts_us);
            // Park the arrival before submitting: VideoToolbox may call back on another thread
            // before `decode` returns.
            self.arrivals.lock().park(pts, frame.arrived);
            match self.decoder.decode(&frame.data, pts) {
                Ok(()) => {
                    if let Some(token) = frame.info.ltr_token {
                        self.reassembler.ack_ltr(token);
                    }
                }
                Err(e) => {
                    self.counters.decode_errors = self.counters.decode_errors.saturating_add(1);
                    tracing::debug!(stream = %self.stream, frame = frame.info.frame, error = %e, "decode");
                }
            }
        }
    }

    /// Run the reassembler's timers; `false` when the connection is gone.
    fn actions(&mut self) -> bool {
        let hint = self.source_live.load(Ordering::Relaxed);
        if hint != self.source_hint {
            self.source_hint = hint;
            self.reassembler.set_source_live(hint);
        }
        let rtt = (self.rtt)().unwrap_or(DEFAULT_RTT);
        for action in self.reassembler.tick(Instant::now(), rtt) {
            let stream = self.stream;
            let feedback = match action {
                Action::Nack { frame, fragments } => {
                    self.counters.nacks = self.counters.nacks.saturating_add(1);
                    Feedback::Nack { stream, frame, fragments }
                }
                Action::RequestRefresh { last_good_frame } => {
                    self.counters.refreshes = self.counters.refreshes.saturating_add(1);
                    Feedback::Refresh { stream, last_good_frame }
                }
            };
            if !(self.feedback)(encode_feedback(feedback)) {
                return false;
            }
        }
        true
    }

    async fn report(&mut self) {
        let now = Instant::now();
        let report = self.reassembler.take_report(now, 0);
        let stats = self.reassembler.stats();
        self.counters.frames_fec = stats.frames_fec;
        self.counters.frames_retransmit = stats.frames_retransmit;
        self.counters.frames_lost = stats.frames_lost;
        self.counters.datagrams_lost = stats.datagrams_lost;
        self.counters.parity_permille = observed_parity(&stats);
        self.counters.data_shards = stats.data_shards;
        self.counters.parity_shards = stats.parity_shards;
        self.counters.stalls = stats.stalls;
        self.counters.stalled_ms = stats.stalled_ms;
        self.counters.stalled = self.reassembler.stalled(now);
        self.counters.hold_p50 = report.hold_p50.to_std();
        self.counters.hold_p95 = report.hold_p95.to_std();
        self.counters.jitter = report.owd_jitter.to_std();
        self.counters.queue_depth = report.queue_depth;
        self.counters.first_decoded_at = *self.first_decoded.lock();
        self.stats.send_replace(self.counters);
        let stream = self.stream;
        let _gone =
            self.out.send(ClientMsg::Screen(ScreenRequest::Report { stream, report })).await;
    }
}

/// The parity ratio the host is sending, in thousandths of the data fragments, from the frame
/// layouts seen so far. Zero until a frame arrives.
fn observed_parity(stats: &ReassemblerStats) -> u16 {
    let permille =
        stats.parity_shards.saturating_mul(1000).checked_div(stats.data_shards).unwrap_or(0);
    u16::try_from(permille).unwrap_or(u16::MAX)
}

/// Encode loss feedback for one datagram. A fragment list too long for a datagram degrades to
/// "every fragment", which the host answers with the whole frame.
fn encode_feedback(feedback: Feedback) -> Bytes {
    if let Ok(body) = slopty_proto::codec::encode_body(&feedback)
        && body.len() <= MAX_DATAGRAM
    {
        return Bytes::from(body);
    }
    let whole = match feedback {
        Feedback::Nack { stream, frame, .. } => {
            Feedback::Nack { stream, frame, fragments: Vec::new() }
        }
        refresh @ Feedback::Refresh { .. } => refresh,
    };
    slopty_proto::codec::encode_body(&whole).map(Bytes::from).unwrap_or_default()
}

#[cfg(test)]
mod arrival_tests {
    use super::*;

    /// The decoder's callback finds the arrival its frame was parked under, and the frames
    /// before it are forgotten with it (the decoder never goes back).
    #[test]
    fn a_parked_arrival_comes_back_with_its_frame_and_clears_the_older_ones() {
        let epoch = Instant::now();
        let at = |ms: u64| epoch.checked_add(Duration::from_millis(ms)).unwrap();
        let mut arrivals = Arrivals::default();
        for i in 0..4_u64 {
            arrivals.park(i * 1_000, at(i * 16));
        }
        assert_eq!(arrivals.take(2_000), Some(at(32)));
        assert_eq!(arrivals.take(1_000), None, "older frames went with it");
        assert_eq!(arrivals.take(3_000), Some(at(48)));
        assert_eq!(arrivals.take(3_000), None, "taken once");
    }

    /// The ring is bounded: a decoder that never calls back cannot grow it.
    #[test]
    fn the_ring_forgets_the_oldest_arrivals() {
        let epoch = Instant::now();
        let mut arrivals = Arrivals::default();
        for i in 0..(u64::try_from(ARRIVALS).unwrap() + 10) {
            arrivals.park(i, epoch);
        }
        assert_eq!(arrivals.0.len(), ARRIVALS);
        assert_eq!(arrivals.take(0), None);
        assert_eq!(arrivals.take(u64::try_from(ARRIVALS).unwrap()), Some(epoch));
    }
}

#[cfg(test)]
mod feedback_tests {
    use super::*;

    #[test]
    fn a_long_fragment_list_degrades_to_the_whole_frame() {
        let fragments: Vec<u16> = (0..2000).collect();
        let bytes = encode_feedback(Feedback::Nack { stream: StreamId(3), frame: 9, fragments });
        assert!(bytes.len() <= MAX_DATAGRAM);
        let decoded: Feedback = slopty_proto::codec::decode_body(&bytes).unwrap();
        assert_eq!(decoded, Feedback::Nack { stream: StreamId(3), frame: 9, fragments: vec![] });
    }

    #[test]
    fn a_short_one_round_trips() {
        let nack = Feedback::Nack { stream: StreamId(3), frame: 9, fragments: vec![1, 4] };
        let bytes = encode_feedback(nack.clone());
        let decoded: Feedback = slopty_proto::codec::decode_body(&bytes).unwrap();
        assert_eq!(decoded, nack);
    }
}
