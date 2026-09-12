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
use slopty_codec::audio::{Conceal, OpusDecoder, Player};
use slopty_codec::{DecodedFrame, Decoder};
use slopty_core::StreamId;
use slopty_media::{
    Action, Config, Ingest, Reassembler, ReassemblerStats, STALL_GAP, StallAttribution,
};
use slopty_proto::ClientMsg;
use slopty_proto::media::{MAX_DATAGRAM, MediaHeader};
use slopty_proto::screen::{Feedback, ScreenRequest, VideoCodec};
use tokio::sync::{mpsc, oneshot, watch};
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
/// Timer period while nothing is pending: refresh repeats depend on it, and so does the stall
/// detector, which subtracts silence this loop slept through from what it charges to the link.
/// Half the stall gap, for the same reason the host heartbeats at half it — a receiver that
/// looks exactly as often as a stall is long cannot tell one it watched from one it missed.
const IDLE_TICK: Duration = Duration::from_millis(25);
/// RTT assumed before the transport has measured one.
const DEFAULT_RTT: Duration = Duration::from_millis(20);

/// Seed of the loss-injection sequence. Fixed, so a run at a given drop rate repeats exactly
/// and two builds can be compared on the same losses.
const LOSS_SEED: u64 = 0x2545_f491_4f6c_dd1d;

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
    /// Lost packets papered over with the previous packet fading out (short gaps only).
    pub audio_concealed: u64,
    /// Stalls that released: nothing arrived for a stall gap, then everything at once.
    pub stalls: u64,
    /// Time spent stalled, milliseconds (released stalls plus the one in progress).
    pub stalled_ms: u64,
    /// The link is stalled right now (as of the last report).
    pub stalled: bool,
    /// Where every silence past the stall gap went — the stall count broken down by what the
    /// host's send stamps made of it.
    pub silences: StallAttribution,
    /// Worst delay between the connection's reader stamping a datagram's arrival and this
    /// worker feeding it to the reassembler. The reassembler measures gaps on the arrival
    /// stamps, so this delay cannot invent a stall by itself; it says whether the runtime was
    /// being starved at all, which is the one thing that would make the stamps late too.
    pub reader_lag_max: Duration,
    /// Datagrams this worker read a stall gap or more after they arrived.
    pub reader_lag_over_gap: u64,
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
    /// The player is being created on a blocking thread: `CoreAudio`'s first client in a process
    /// initialises the HAL, which took ~7 s on the mac-studio (`docs/MEASUREMENTS.md`,
    /// 2026-09-13), and the worker must keep reassembling and reporting meanwhile.
    Opening(oneshot::Receiver<Result<Audio, slopty_codec::CodecError>>),
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
    /// The previous packet, for short gaps.
    conceal: Conceal,
    /// Scratch for the stand-in samples.
    stand_in: Vec<f32>,
}

impl Audio {
    fn new() -> Result<Self, slopty_codec::CodecError> {
        Ok(Self {
            decoder: OpusDecoder::new()?,
            player: Player::new()?,
            seq: 0,
            conceal: Conceal::default(),
            stand_in: Vec::new(),
        })
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
    /// The worker; a detached test handle has none.
    task: Option<JoinHandle<()>>,
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
        if let Some(task) = &self.task {
            task.abort();
        }
        self.router.detach(self.stream);
    }
}

#[cfg(feature = "headless")]
impl ScreenHandle {
    /// A handle with no worker behind it: nothing ever arrives, nothing is decoded. For the
    /// headless UI tests, which need a stream to exist, not to show pictures.
    #[must_use]
    pub fn detached(stream: StreamId) -> Self {
        let (_frames_tx, frames) = watch::channel(None);
        let (_cursor_tx, cursor) = watch::channel(CursorState::default());
        let (_stats_tx, stats) = watch::channel(ScreenStats::default());
        Self {
            stream,
            frames,
            cursor,
            stats,
            muted: Arc::new(AtomicBool::new(false)),
            source_live: Arc::new(AtomicBool::new(true)),
            router: ScreenRouter::default(),
            task: None,
        }
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
        // The loop below ticks after every branch, so the longest it goes without one is the
        // idle sleep; the reassembler needs that number to tell a link that held datagrams
        // from a runtime that did not run this task.
        reassembler: Reassembler::new(
            stream,
            Config { tick_period: IDLE_TICK, ..Config::default() },
            Instant::now(),
        ),
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
    let task = Some(task);
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
    /// Move the player towards open: start creating it on the first packet, pick it up once
    /// the blocking thread is done. Never waits.
    fn open_audio(&mut self) {
        self.audio = match std::mem::replace(&mut self.audio, AudioSlot::Failed) {
            AudioSlot::Unopened => {
                let (tx, rx) = oneshot::channel();
                drop(tokio::task::spawn_blocking(move || {
                    let _no_worker = tx.send(Audio::new());
                }));
                AudioSlot::Opening(rx)
            }
            AudioSlot::Opening(mut rx) => match rx.try_recv() {
                Ok(Ok(audio)) => AudioSlot::Open(audio),
                Ok(Err(e)) => {
                    tracing::warn!(stream = %self.stream, error = %e, "audio playback unavailable");
                    AudioSlot::Failed
                }
                Err(oneshot::error::TryRecvError::Empty) => AudioSlot::Opening(rx),
                Err(oneshot::error::TryRecvError::Closed) => AudioSlot::Failed,
            },
            open_or_failed => open_or_failed,
        };
    }

    /// Decode and queue one Opus packet; late duplicates are dropped, gaps counted. A packet
    /// that lands while the player is still opening (or after it failed) is lost to playback
    /// and counted as such.
    fn play_audio(&mut self, seq: u32, payload: &Bytes) {
        self.open_audio();
        let AudioSlot::Open(audio) = &mut self.audio else {
            self.counters.audio_lost = self.counters.audio_lost.saturating_add(1);
            return;
        };
        let gap = seq.wrapping_sub(audio.seq);
        if audio.seq != 0 && (gap == 0 || gap > u32::MAX / 2) {
            self.counters.audio_lost = self.counters.audio_lost.saturating_add(1);
            return;
        }
        if audio.seq != 0 && gap > 1 {
            let missing = gap.saturating_sub(1);
            self.counters.audio_lost = self.counters.audio_lost.saturating_add(u64::from(missing));
            audio.stand_in.clear();
            audio.conceal.fill(missing, &mut audio.stand_in);
            if !audio.stand_in.is_empty() {
                if !self.muted.load(Ordering::Relaxed) {
                    audio.player.push(&audio.stand_in);
                }
                self.counters.audio_concealed =
                    self.counters.audio_concealed.saturating_add(u64::from(missing));
            }
        }
        audio.seq = seq;
        match audio.decoder.decode(payload) {
            Ok(pcm) => {
                audio.conceal.remember(pcm);
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
        let lag = Instant::now().saturating_duration_since(now);
        if lag > self.counters.reader_lag_max {
            self.counters.reader_lag_max = lag;
        }
        if lag >= STALL_GAP {
            self.counters.reader_lag_over_gap = self.counters.reader_lag_over_gap.saturating_add(1);
        }
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
        self.counters.silences = stats.silences;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A datagram for `stream`, frame `frame`: a 16-byte little-endian header and one payload
    /// byte, the shape `MediaHeader::parse` reads.
    fn datagram(stream: u32, frame: u32) -> Bytes {
        let mut d = Vec::with_capacity(17);
        d.extend_from_slice(&stream.to_le_bytes());
        d.extend_from_slice(&frame.to_le_bytes());
        d.extend_from_slice(&[0_u8; 8]);
        d.push(0xee);
        Bytes::from(d)
    }

    fn frame_of(arrival: &Arrival) -> u32 {
        MediaHeader::parse(&arrival.1).map_or(u32::MAX, |(h, _)| h.frame.get())
    }

    #[test]
    fn a_stream_backlogs_until_attached_and_the_backlog_keeps_the_newest() {
        let router = ScreenRouter::with_loss(0);
        let now = Instant::now();
        let sent = u32::try_from(PENDING_DEPTH).unwrap_or(u32::MAX).saturating_add(100);
        for frame in 0..sent {
            router.route(datagram(7, frame), now);
        }
        let mut rx = router.attach(StreamId(7));
        let first = rx.try_recv().ok();
        assert_eq!(first.as_ref().map(frame_of), Some(100), "the oldest 100 were dropped");
        let mut got = 1;
        while rx.try_recv().is_ok() {
            got += 1;
        }
        assert_eq!(got, PENDING_DEPTH);
        // Attached now: a datagram goes straight through.
        router.route(datagram(7, 9_999), now);
        assert_eq!(rx.try_recv().ok().as_ref().map(frame_of), Some(9_999));
    }

    #[test]
    fn detach_and_forget_stop_the_stream_and_a_short_datagram_is_ignored() {
        let router = ScreenRouter::with_loss(0);
        let now = Instant::now();
        let mut rx = router.attach(StreamId(1));
        router.route(datagram(1, 1), now);
        assert_eq!(rx.try_recv().ok().as_ref().map(frame_of), Some(1));
        router.detach(StreamId(1));
        router.route(datagram(1, 2), now);
        assert!(rx.try_recv().is_err(), "detached: nothing delivered");
        // What lands between the detach and the host's `Closed` backlogs like a stream nobody
        // attached yet (a re-attach would want it); `forget` on `Closed` lets it go.
        assert!(router.inner.lock().pending.contains_key(&StreamId(1)));
        router.forget(StreamId(1));

        router.route(datagram(2, 1), now);
        assert!(router.inner.lock().pending.contains_key(&StreamId(2)));
        router.forget(StreamId(2));
        assert!(router.inner.lock().pending.is_empty(), "a Closed stream keeps no backlog");

        router.route(Bytes::from_static(&[1, 2, 3]), now);
        assert!(router.inner.lock().pending.is_empty(), "too short for a header");
    }

    #[test]
    fn loss_injection_is_proportional_and_repeats_from_its_seed() {
        let count = |permille: u32| -> Vec<u32> {
            let router = ScreenRouter::with_loss(permille);
            let mut rx = router.attach(StreamId(3));
            let now = Instant::now();
            for frame in 0..1_000 {
                router.route(datagram(3, frame), now);
            }
            let mut got = Vec::new();
            while let Ok(arrival) = rx.try_recv() {
                got.push(frame_of(&arrival));
            }
            got
        };
        assert_eq!(count(0).len(), 1_000, "no loss by default");
        let half = count(500);
        assert!((400..=600).contains(&half.len()), "about half survive: {}", half.len());
        // The seeded stream is part of the contract: a run at a rate sees the same losses.
        assert_eq!(half.len(), 512, "the seed's own count");
        assert_eq!(half, count(500), "the same seed drops the same datagrams");
        assert_ne!(half, count(100));
        assert!(count(1_000).is_empty(), "a thousand per thousand drops everything");
        let router = ScreenRouter::with_loss(1_001);
        assert_eq!(router.drop_permille.load(Ordering::Relaxed), 1_000, "clamped");
    }

    #[test]
    fn a_parked_arrival_is_taken_by_its_timestamp_and_older_ones_go_with_it() {
        let mut arrivals = Arrivals::default();
        let t0 = Instant::now();
        let t = |n: u64| t0 + Duration::from_micros(n);
        arrivals.park(10, t(1));
        arrivals.park(20, t(2));
        arrivals.park(30, t(3));
        assert_eq!(arrivals.take(20), Some(t(2)));
        assert_eq!(arrivals.take(10), None, "older than the one taken: forgotten with it");
        assert_eq!(arrivals.take(30), Some(t(3)));
        assert!(arrivals.0.is_empty());
        for n in 0..u64::try_from(ARRIVALS).unwrap_or(u64::MAX).saturating_add(5) {
            arrivals.park(n, t(n));
        }
        assert_eq!(arrivals.0.len(), ARRIVALS, "bounded");
        assert_eq!(arrivals.take(0), None, "the oldest were dropped to make room");
    }
}

#[cfg(test)]
mod worker_tests {
    use std::sync::atomic::AtomicBool;

    use slopty_codec::audio::{CHANNELS, FRAME_SAMPLES, OpusEncoder};
    use slopty_media::{EncodedFrame, Packetizer, audio_datagram, cursor_datagram};

    use super::*;

    const STREAM: StreamId = StreamId(5);

    /// A worker on its own runtime, with what it sent back caught for inspection.
    struct Harness {
        rt: tokio::runtime::Runtime,
        router: ScreenRouter,
        handle: ScreenHandle,
        control: mpsc::Receiver<ClientMsg>,
        feedback: Arc<Mutex<Vec<Feedback>>>,
        /// The connection is up; `false` makes the next feedback send fail.
        alive: Arc<AtomicBool>,
        /// Datagrams handed to the router, to know when the worker has seen them all.
        routed: Arc<AtomicU64>,
    }

    impl Harness {
        fn start() -> Self {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let router = ScreenRouter::with_loss(0);
            let (control_tx, control) = mpsc::channel(64);
            let feedback = Arc::new(Mutex::new(Vec::new()));
            let alive = Arc::new(AtomicBool::new(true));
            let caught = Arc::clone(&feedback);
            let up = Arc::clone(&alive);
            let uplink = Uplink {
                control: control_tx,
                feedback: Box::new(move |bytes| {
                    if let Ok(fb) = slopty_proto::codec::decode_body::<Feedback>(&bytes) {
                        caught.lock().push(fb);
                    }
                    up.load(Ordering::Relaxed)
                }),
                rtt: Box::new(|| Some(Duration::from_millis(10))),
            };
            let handle = spawn_screen(rt.handle(), &router, STREAM, VideoCodec::Hevc, uplink);
            Self { rt, router, handle, control, feedback, alive, routed: Arc::default() }
        }

        /// Poll `done` every few milliseconds for up to `secs`, draining the reports the worker
        /// sends meanwhile (its report send awaits a slot).
        fn wait_for(&mut self, what: &str, secs: u64, mut done: impl FnMut(&ScreenHandle) -> bool) {
            let deadline = Instant::now().checked_add(Duration::from_secs(secs)).unwrap();
            self.rt.block_on(async {
                loop {
                    while let Ok(msg) = self.control.try_recv() {
                        assert!(
                            matches!(msg, ClientMsg::Screen(ScreenRequest::Report { stream, .. }) if stream == STREAM),
                            "{msg:?}"
                        );
                    }
                    if done(&self.handle) {
                        return;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "timed out waiting for {what}: {:?}",
                        self.handle.stats()
                    );
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            });
        }

        fn route(&self, datagram: Bytes) {
            self.routed.fetch_add(1, Ordering::Relaxed);
            self.router.route(datagram, Instant::now());
        }

        /// Wait until a report has counted every datagram routed so far.
        fn settle(&mut self) -> ScreenStats {
            let routed = Arc::clone(&self.routed);
            self.wait_for("the worker to catch up", 3, |handle| {
                handle.stats().datagrams == routed.load(Ordering::Relaxed)
            });
            self.handle.stats()
        }
    }

    /// A 3000-byte HEVC-shaped access unit with no parameter sets: the decoder rejects it, which
    /// is what a unit test can see of "the frame reached the decoder".
    fn frame_bytes() -> Vec<u8> {
        let mut data = vec![0, 0, 0, 1, 0x02, 0x01];
        data.resize(3000, 0xaa);
        data
    }

    fn packetize(packetizer: &mut Packetizer, keyframe: bool, capture_ts_us: u32) -> Vec<Bytes> {
        let data = frame_bytes();
        let frame = EncodedFrame {
            data: &data,
            keyframe,
            ltr_token: None,
            ltr_refresh: false,
            capture_ts_us,
        };
        packetizer.packetize(&frame, 0).unwrap().datagrams.clone()
    }

    #[test]
    fn a_worker_reassembles_nacks_reports_and_stops_with_the_connection() {
        let mut h = Harness::start();

        // Cursor: the newest sequence wins, an older one is ignored.
        h.route(cursor_datagram(STREAM, 2, 0, 10, 20, true));
        h.wait_for("the cursor", 3, |handle| {
            *handle.cursor().borrow() == CursorState { x: 10, y: 20, visible: true }
        });
        h.route(cursor_datagram(STREAM, 1, 0, 99, 99, false));
        h.route(cursor_datagram(STREAM, 3, 0, 11, 21, false));
        h.wait_for("the newer cursor", 3, |handle| {
            *handle.cursor().borrow() == CursorState { x: 11, y: 21, visible: false }
        });

        // A keyframe with one fragment held back: the worker asks for it, the retransmission
        // completes the frame, and the (rejected) frame is counted at the decoder.
        let mut packetizer = Packetizer::new(STREAM);
        packetizer.set_parity_permille(0);
        let datagrams = packetize(&mut packetizer, true, 1_000);
        assert!(datagrams.len() >= 3, "{} datagrams", datagrams.len());
        for (i, d) in datagrams.iter().enumerate() {
            if i != 1 {
                h.route(d.clone());
            }
        }
        let feedback = Arc::clone(&h.feedback);
        h.wait_for("a nack", 3, |_handle| {
            feedback.lock().iter().any(|fb| {
                matches!(fb, Feedback::Nack { stream, frame: 0, fragments } if *stream == STREAM && fragments == &[1])
            })
        });
        for d in packetizer.retransmit(0, &[1]) {
            h.route(d);
        }
        h.wait_for("the frame", 3, |handle| {
            let s = handle.stats();
            s.frames == 1 && s.frames_retransmit == 1 && s.decode_errors == 1
        });
        let stats = h.handle.stats();
        assert_eq!(stats.nacks, 1);
        assert_eq!(stats.datagrams_lost, 0, "recovered, so not lost");
        assert!(stats.first_datagram_at.is_some() && stats.first_frame_at.is_some());
        assert_eq!(stats.first_decoded_at, None, "nothing decoded");
        assert_eq!(stats.parity_permille, 0);
        assert!(stats.bytes > 3000 && stats.datagrams >= 5, "{stats:?}");

        // Audio, muted: the first packet starts opening the player off the worker, which keeps
        // delivering video meanwhile (CoreAudio's first client took ~7 s on this machine).
        assert!(!h.handle.muted());
        h.handle.set_muted(true);
        assert!(h.handle.muted());
        let mut enc = OpusEncoder::new().unwrap();
        let tone: Vec<f32> = (0..u16::try_from(FRAME_SAMPLES * CHANNELS).unwrap())
            .map(|i| (f32::from(i) * 0.05).sin() * 0.5)
            .collect();
        let mut packets = Vec::new();
        for _ in 0..3 {
            enc.push(&tone, |p| packets.push(p.to_vec())).unwrap();
        }
        assert_eq!(packets.len(), 3);
        let mut seq = 1;
        h.route(audio_datagram(STREAM, seq, 0, &packets[0]).unwrap());
        for d in packetize(&mut packetizer, false, 2_000) {
            h.route(d);
        }
        h.wait_for("a frame while the player opens", 3, |handle| handle.stats().frames == 2);
        let router = h.router.clone();
        let routed = Arc::clone(&h.routed);
        let opus = packets.clone();
        h.wait_for("the player", 20, |handle| {
            seq += 1;
            routed.fetch_add(1, Ordering::Relaxed);
            router.route(
                audio_datagram(STREAM, seq, 0, &opus[seq as usize % 3]).unwrap(),
                Instant::now(),
            );
            handle.stats().audio_packets >= 1
        });
        let before = h.settle();
        assert!(before.audio_lost >= 1, "the packets that landed while opening: {before:?}");
        // A gap of one, then a late duplicate.
        seq += 2;
        h.route(audio_datagram(STREAM, seq, 0, &packets[0]).unwrap());
        let after = h.settle();
        assert_eq!(after.audio_packets, before.audio_packets + 1);
        assert_eq!(after.audio_lost, before.audio_lost + 1, "one missing");
        assert_eq!(after.audio_concealed, before.audio_concealed + 1, "and concealed");
        h.route(audio_datagram(STREAM, seq - 1, 0, &packets[1]).unwrap());
        let late = h.settle();
        assert_eq!(late.audio_lost, after.audio_lost + 1, "too late to play");
        assert_eq!(late.audio_packets, after.audio_packets, "not played");

        // The host's source hint reaches the worker.
        assert!(h.handle.source_live());
        h.handle.set_source_live(false);
        assert!(!h.handle.source_live());

        // The connection goes: the next feedback fails and the worker stops.
        h.alive.store(false, Ordering::Relaxed);
        let datagrams = packetize(&mut packetizer, false, 3_000);
        for d in datagrams.iter().skip(1) {
            h.route(d.clone());
        }
        h.wait_for("the worker to finish", 3, |handle| {
            handle.task.as_ref().is_some_and(JoinHandle::is_finished)
        });
        assert_eq!(h.handle.stream(), STREAM);
        drop(h.handle);
        assert!(h.router.inner.lock().attached.is_empty(), "dropping the handle unroutes");
    }
}
