//! Remote windows on the client: datagram routing, reassembly, hardware decode, and
//! latest-frame delivery to the UI.
//!
//! [`WorkerLink`](crate::WorkerLink) reads every datagram on the connection and hands it to a
//! [`ScreenRouter`], which fans out by stream id. A stream the UI has not attached yet keeps a
//! short backlog (its first datagrams usually beat the `Opened` control message), so nothing
//! is lost at start-up. [`spawn_screen`] runs one task per stream: reassemble, NACK and refresh
//! on the reassembler's schedule, decode, report every 50 ms, and publish the newest decoded
//! frame and cursor position on `watch` channels the UI polls at paint time.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use bytes::Bytes;
use parking_lot::Mutex;
use slopty_codec::{DecodeFailure, DecodedFrame, Decoder};
use slopty_core::StreamId;
use slopty_media::{
    Action, ClockSync, Config, Ingest, Reassembler, ReassemblerStats, STALL_GAP, StallAttribution,
};
use slopty_proto::ClientMsg;
use slopty_proto::datagram::ClientDatagram;
use slopty_proto::media::{ClockEcho, Kind, MAX_DATAGRAM, MediaHeader, flags};
use slopty_proto::screen::{Feedback, ReceiverReport, Region, ScreenRequest, Stripe, VideoCodec};
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinHandle;

use crate::pacing::{CaptureClock, Captured, ClockEstimate, FrameStamp};

pub mod display;
mod sound;

/// Datagrams buffered per attached stream before the task must drain them.
const STREAM_DEPTH: usize = 2048;
/// Datagrams kept for a stream nobody has attached yet.
const PENDING_DEPTH: usize = 512;
/// Streams that may wait unattached at once. An `Opened` nobody asked for is closed without a
/// view ever attaching, so its backlog is never taken; past this many, the stream heard from
/// longest ago lets its backlog go.
const PENDING_STREAMS: usize = 8;
/// Detached streams remembered, so the datagrams still in flight when a view lets go of its
/// stream are dropped rather than backlogged as a stream not attached yet. Stream ids count up
/// per connection, so the oldest can be forgotten once this many newer ones have gone.
const TOMBSTONES: usize = 64;
/// How often a receiver report goes to the worker.
const REPORT_EVERY: Duration = Duration::from_millis(50);
/// Reassembler timer resolution while frames are pending.
const TICK: Duration = Duration::from_millis(2);
/// Timer period while nothing is pending: refresh repeats depend on it, and so does the stall
/// detector, which subtracts silence this loop slept through from what it charges to the link.
/// Half the stall gap, for the same reason the worker heartbeats at half it — a receiver that
/// looks exactly as often as a stall is long cannot tell one it watched from one it missed.
const IDLE_TICK: Duration = Duration::from_millis(25);

/// How long a new stream waits for its first video datagram before it asks for a keyframe again.
///
/// The worker opens a stream once its encoder is built, and the session's first frame takes
/// 60–130 ms more at 3024 × 1964, longer than the usual first repeat (100 ms and two round trips):
/// asked again, it made a second full keyframe behind the first. The worker's own word on a target
/// that draws nothing comes at this age (`ScreenEvent::Source`, the worker's `SOURCE_IDLE_AFTER`),
/// which is where waiting on a first picture stops being the expected thing (MEASUREMENTS.md, "a
/// refresh asked for while the keyframe is encoded").
const FIRST_KEYFRAME_WAIT: Duration = Duration::from_millis(400);

/// Clock probes sent one a report at the start of a stream, so the first pictures are timed
/// from their capture within a few round trips; after these, one every [`PROBE_EVERY`] reports.
const PROBES_AT_ONCE: u64 = 8;
/// Reports between clock probes once the estimate has its first probes: four a second, which
/// fills [`slopty_media::CLOCK_WINDOW`] with 120 of them for a few dozen bytes a second.
const PROBE_EVERY: u64 = 5;

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
    /// tests in `apps/slopty-worker/tests/e2e.rs`.
    drop_permille: Arc<AtomicU32>,
    lcg: Arc<AtomicU64>,
    /// The worker's sound, while a stream of the connection holds it.
    sound: Arc<Mutex<Weak<sound::Sound>>>,
    /// Its mute, for the connection's life: a sound started again by a new tile after the last
    /// one closed keeps the choice.
    muted: Arc<sound::Muted>,
}

impl Default for ScreenRouter {
    fn default() -> Self {
        Self {
            inner: Arc::default(),
            drop_permille: Arc::new(AtomicU32::new(0)),
            lcg: Arc::new(AtomicU64::new(LOSS_SEED)),
            sound: Arc::default(),
            muted: Arc::default(),
        }
    }
}

/// A datagram and when the connection handed it over: the stream worker may be busy (a decode,
/// a report) when it lands, and the reassembler's stall clock must not charge that wait to
/// the link.
type Arrival = (Instant, Bytes);

#[derive(Debug, Default)]
struct Routes {
    attached: HashMap<StreamId, Route>,
    pending: HashMap<StreamId, Backlog>,
    /// Streams let go of, newest last ([`TOMBSTONES`] at most).
    detached: VecDeque<StreamId>,
}

/// An attached stream's queue, and the count of what did not fit in it.
#[derive(Debug)]
struct Route {
    tx: mpsc::Sender<Arrival>,
    dropped: Arc<AtomicU64>,
}

/// What a stream nobody has attached yet brought, and how much of it the bound let go.
#[derive(Debug, Default)]
struct Backlog {
    arrivals: VecDeque<Arrival>,
    dropped: u64,
}

/// An attached stream's datagrams, as the router hands them over.
#[derive(Debug)]
struct Attached {
    datagrams: mpsc::Receiver<Arrival>,
    /// Datagrams of the stream the router dropped before its task read them: a full queue, or
    /// a backlog past its bound before the stream attached ([`ScreenStats::datagrams_dropped`]).
    dropped: Arc<AtomicU64>,
}

impl Routes {
    fn deliver(&mut self, stream: StreamId, arrival: Arrival) {
        if let Some(route) = self.attached.get(&stream) {
            // A full queue means the stream task is behind; dropping is the right call, and
            // counting it is what tells that apart from a datagram the link lost.
            if let Err(mpsc::error::TrySendError::Full(_)) = route.tx.try_send(arrival) {
                route.dropped.fetch_add(1, Ordering::Relaxed);
            }
            return;
        }
        if self.detached.contains(&stream) {
            return;
        }
        if !self.pending.contains_key(&stream) && self.pending.len() >= PENDING_STREAMS {
            let stalest = self
                .pending
                .iter()
                .min_by_key(|(_, backlog)| backlog.arrivals.back().map(|(at, _)| *at))
                .map(|(id, _)| *id);
            if let Some(id) = stalest {
                self.pending.remove(&id);
            }
        }
        let backlog = self.pending.entry(stream).or_default();
        if backlog.arrivals.len() >= PENDING_DEPTH {
            backlog.arrivals.pop_front();
            backlog.dropped = backlog.dropped.saturating_add(1);
        }
        backlog.arrivals.push_back(arrival);
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
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |x| {
                Some(
                    x.wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407),
                )
            })
            .unwrap_or(0);
        let roll = u32::try_from((next >> 33) % 1000).unwrap_or(0);
        roll < permille
    }

    /// Deliver one datagram that arrived at `now`.
    pub fn route(&self, datagram: Bytes, now: Instant) {
        self.route_many([datagram], now);
    }

    /// Deliver datagrams that arrived together at `now`, in order, under one lock of the
    /// routes (called by the connection's datagram reader with each read's worth).
    pub fn route_many(&self, datagrams: impl IntoIterator<Item = Bytes>, now: Instant) {
        let mut routes = None;
        for datagram in datagrams {
            if self.inject_loss() {
                continue;
            }
            let Some((header, _payload)) = MediaHeader::parse(&datagram) else { continue };
            // A stripe's media stream goes to its stream's task, which tells them apart.
            let (stream, _stripe) = Stripe::stream_of(StreamId(header.stream.get()));
            routes.get_or_insert_with(|| self.inner.lock()).deliver(stream, (now, datagram));
        }
    }

    /// Start receiving `stream`'s datagrams, backlog first.
    #[must_use]
    pub fn attach(&self, stream: StreamId) -> mpsc::Receiver<Arrival> {
        self.attach_counted(stream).datagrams
    }

    /// [`Self::attach`], with the count of the stream's datagrams the router drops.
    fn attach_counted(&self, stream: StreamId) -> Attached {
        let (tx, datagrams) = mpsc::channel(STREAM_DEPTH);
        let mut routes = self.inner.lock();
        routes.detached.retain(|id| *id != stream);
        let mut dropped = 0_u64;
        if let Some(backlog) = routes.pending.remove(&stream) {
            dropped = backlog.dropped;
            for datagram in backlog.arrivals {
                if tx.try_send(datagram).is_err() {
                    dropped = dropped.saturating_add(1);
                }
            }
        }
        let dropped = Arc::new(AtomicU64::new(dropped));
        routes.attached.insert(stream, Route { tx, dropped: Arc::clone(&dropped) });
        drop(routes);
        Attached { datagrams, dropped }
    }

    /// The worker's sound, started on `runtime` when no stream holds it, reporting on
    /// `control`.
    fn sound(
        &self,
        runtime: &tokio::runtime::Handle,
        control: &mpsc::Sender<ClientMsg>,
    ) -> Arc<sound::Sound> {
        let mut slot = self.sound.lock();
        if let Some(sound) = slot.upgrade() {
            return sound;
        }
        let sound =
            Arc::new(sound::Sound::spawn(runtime, self, Arc::clone(&self.muted), control.clone()));
        *slot = Arc::downgrade(&sound);
        sound
    }

    /// Stop routing `stream`: what is in flight for it from here on is dropped, not kept for
    /// an attach that will not come.
    pub fn detach(&self, stream: StreamId) {
        let mut routes = self.inner.lock();
        routes.attached.remove(&stream);
        routes.pending.remove(&stream);
        if !routes.detached.contains(&stream) {
            if routes.detached.len() >= TOMBSTONES {
                routes.detached.pop_front();
            }
            routes.detached.push_back(stream);
        }
    }
}

/// Frames remembered while the decoder works on them. VideoToolbox is
/// asynchronous and gives the callback nothing but the presentation timestamp, so the worker
/// parks the stamp under that timestamp and the callback picks it back up. Two frames' worth of
/// a second is plenty; anything older has been answered or dropped.
const ARRIVALS: usize = 128;

/// A decoded picture together with the timing that got it here, which is what the element needs
/// to pace and to say how old what it paints is.
#[derive(Debug)]
pub struct Presentable {
    /// The picture: the whole of it, or for a striped stream the top stripe's.
    pub frame: DecodedFrame,
    /// Arrival of the datagram that completed the frame, and when the decoder returned it.
    pub stamp: FrameStamp,
    /// For a striped stream, the lower stripe and where the two meet; `None` for one picture.
    pub stripes: Option<Stitched>,
    /// The part of the target the picture shows, in the target's native pixels; `None` for all
    /// of it ([`slopty_proto::media::FramePrefix::region`]). A zoomed picture's frames show its
    /// region, and the view draws each at that region's place in the whole picture.
    pub region: Option<Region>,
}

/// The two stripes of one capture, each decoded by a session of its own, to be shown one over
/// the other with no copy between them (`docs/decisions/video.md`, "Two stripes").
///
/// Each stripe's picture holds the rows it codes, which run [`slopty_codec::stripes::OVERLAP`]
/// past the seam into the other's. The top one ([`Presentable::frame`]) shows its first
/// [`Self::top_rows`]; the lower one shows its rows from [`Self::lower_from`] down, right under
/// them.
#[derive(Clone, Debug)]
pub struct Stitched {
    /// The lower stripe's picture.
    pub lower: DecodedFrame,
    /// Rows of the top stripe's picture shown: the rows above the seam.
    pub top_rows: u32,
    /// The first row of the lower stripe's picture shown: the rows it codes above the seam
    /// are the top stripe's.
    pub lower_from: u32,
}

impl Presentable {
    /// The picture's size in pixels, both stripes together.
    #[must_use]
    pub fn size(&self) -> (usize, usize) {
        let width = self.frame.image.width();
        match &self.stripes {
            None => (width, self.frame.image.height()),
            Some(stitched) => {
                let top = usize::try_from(stitched.top_rows).unwrap_or(usize::MAX);
                let from = usize::try_from(stitched.lower_from).unwrap_or(usize::MAX);
                (width, top.saturating_add(stitched.lower.image.height().saturating_sub(from)))
            }
        }
    }
}

/// Shows a decoded picture on the decoder's own thread, the moment it comes out
/// ([`ScreenHandle::set_present`]): a presenter with a layer of its own waits for no UI frame.
pub type Present = Arc<dyn Fn(&Presentable) + Send + Sync>;

/// Where the decoder's thread finds the [`Present`], if one is set.
#[derive(Default)]
struct PresentSlot(Mutex<Option<Present>>);

impl std::fmt::Debug for PresentSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("PresentSlot").field(&self.0.lock().is_some()).finish()
    }
}

/// A frame handed to the decoder, parked by presentation timestamp for its callback.
#[derive(Clone, Copy, Debug)]
struct Parked {
    pts_us: u64,
    arrived: Instant,
    ltr_token: Option<u64>,
    /// A keyframe or an LTR refresh: the frame that restarts decoding.
    restarts: bool,
    /// Nothing later refers to it: failing on it leaves every reference intact.
    discardable: bool,
    /// How it was coded: its capture's stripes and the build of the sessions.
    coded: Coded,
}

/// What the decoder leaves for the stream worker: the frames it is working on, the long-term
/// references it really decoded, and whether it failed or refused anything.
#[derive(Debug, Default)]
struct Inflight {
    parked: VecDeque<Parked>,
    /// Tokens of frames the decoder returned a picture for, not yet given to the reassembler.
    acks: Vec<u64>,
    failed: Option<Failed>,
    /// Failures on frames nothing refers to, which need no refresh.
    failed_discardable: u64,
    /// Frames refused as they were submitted, since the worker last looked.
    refused: u64,
    /// The refresh those refusals ask for, `Some(keyframe)`: a keyframe if any of them asks
    /// for one ([`refresh_after_refusal`]).
    refusal_refresh: Option<bool>,
    /// The newest failure's status, until the worker takes it.
    status: Option<Status>,
}

/// A failure's status, as [`ScreenStats::decode_failure`] gives it: VideoToolbox's, or `None`
/// for a refusal that was not VideoToolbox's.
#[derive(Clone, Copy, Debug)]
struct Status(Option<i32>);

/// Failures since the worker last looked, folded into one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Failed {
    /// The newest failed frame's timestamp.
    pts_us: u64,
    /// Any of them lost the session, so only a keyframe helps.
    session_lost: bool,
    /// Any of them was a frame that restarts decoding: another refresh off the same
    /// references could fail the same way, so only a keyframe helps.
    restart: bool,
    count: u64,
}

impl Inflight {
    fn park(&mut self, parked: Parked) {
        if self.parked.len() >= ARRIVALS {
            self.parked.pop_front();
        }
        self.parked.push_back(parked);
    }

    /// The frame with this timestamp, and everything older forgotten with it.
    fn take(&mut self, pts_us: u64) -> Option<Parked> {
        let at = self.parked.iter().position(|p| p.pts_us == pts_us)?;
        self.parked.drain(..=at).next_back()
    }

    /// Forget the frame with this timestamp alone: it never reached the decoder, so the frames
    /// parked before it are still in there.
    fn unpark(&mut self, pts_us: u64) {
        if let Some(at) = self.parked.iter().rposition(|p| p.pts_us == pts_us) {
            let _gone = self.parked.remove(at);
        }
    }

    /// The decoder returned this frame's picture: its arrival and how it was coded, and its
    /// token is now held.
    fn decoded(&mut self, pts_us: u64) -> Option<(Instant, Coded)> {
        let parked = self.take(pts_us)?;
        self.acks.extend(parked.ltr_token);
        Some((parked.arrived, parked.coded))
    }

    /// The decoder returned no picture for this frame: its token is never acknowledged. A frame
    /// nothing refers to, failed with the session intact, took no reference with it, so it asks
    /// for nothing.
    fn failed(&mut self, failure: DecodeFailure) {
        self.status = Some(Status(Some(failure.status)));
        let parked = self.take(failure.pts_us);
        if parked.is_some_and(|p| p.discardable) && !failure.session_lost() {
            self.failed_discardable = self.failed_discardable.saturating_add(1);
            return;
        }
        let restarts = parked.is_some_and(|p| p.restarts);
        let before = self.failed.unwrap_or(Failed {
            pts_us: 0,
            session_lost: false,
            restart: false,
            count: 0,
        });
        self.failed = Some(Failed {
            pts_us: before.pts_us.max(failure.pts_us),
            session_lost: before.session_lost || failure.session_lost(),
            restart: before.restart || restarts,
            count: before.count.saturating_add(1),
        });
    }

    /// The frame with this timestamp was refused as it was submitted, with VideoToolbox's
    /// `status` when the refusal was VideoToolbox's: it is forgotten, and `refresh` is what it
    /// asks for.
    fn refused(&mut self, pts_us: u64, status: Option<i32>, refresh: Option<bool>) {
        let _gone = self.take(pts_us);
        self.status = Some(Status(status));
        self.refused = self.refused.saturating_add(1);
        if let Some(keyframe) = refresh {
            self.refusal_refresh = Some(self.refusal_refresh.unwrap_or(false) || keyframe);
        }
    }
}

/// Where the worker's pointer is, in stream pixels.
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
    /// Frames delivered to the decoder: of a striped stream, the top stripe's, one a capture.
    pub frames: u64,
    /// Frames recovered by FEC.
    pub frames_fec: u64,
    /// Frames that needed a retransmission.
    pub frames_retransmit: u64,
    /// Frames given up on.
    pub frames_lost: u64,
    /// Frames nothing refers to that were skipped instead of repaired, at no refresh.
    pub frames_skipped: u64,
    /// Data fragments that never arrived (counted as each frame resolves).
    pub datagrams_lost: u64,
    /// Datagrams that arrived and were dropped before this stream read them: its queue was full
    /// (`STREAM_DEPTH` behind), or, before it attached, its backlog was (`PENDING_DEPTH`).
    /// Not in [`Self::datagrams_lost`], which counts fragments of the frames the reassembler saw
    /// and cannot count a frame that went whole.
    pub datagrams_dropped: u64,
    /// Parity the worker is sending, in thousandths of the data fragments, as observed on the
    /// wire: the receiver's view of what the redundancy controller settled on.
    pub parity_permille: u16,
    /// Data and parity fragments the worker cut the frames seen so far into. The ratio is
    /// [`Self::parity_permille`]; the counts also say how big the frames were, which is what
    /// decides whether the parity policy could cost anything at all.
    pub data_shards: u64,
    /// Parity fragments the worker added to them.
    pub parity_shards: u64,
    /// NACKs sent.
    pub nacks: u64,
    /// Refresh requests sent.
    pub refreshes: u64,
    /// Decoder rejections, and frames dropped for a decoder a whole queue behind.
    pub decode_errors: u64,
    /// VideoToolbox's status for the newest rejection (zero for a frame it dropped without
    /// naming an error); `None` before the first, and for a failure that was not VideoToolbox's
    /// (no parameter sets yet, a malformed unit).
    pub decode_failure: Option<i32>,
    /// Frames in the decoders now: handed to them, and neither a picture nor a failure back.
    pub decoding: u64,
    /// Decoders given up on and built anew: one whose submission stayed inside VideoToolbox
    /// past `DECODE_STUCK` (doubled for each in a row that brought no picture), or whose
    /// thread was gone.
    pub decoders_replaced: u64,
    /// Datagrams seen.
    pub datagrams: u64,
    /// The stream is coded as two stripes now ([`Stitched`]).
    pub striped: bool,
    /// Captures of a striped stream that went up with one stripe's previous picture: its
    /// stripe was late past a display refresh (`STITCH_WAIT`).
    pub seam_tears: u64,
    /// Bytes received in datagrams (video, cursor, parity).
    pub bytes: u64,
    /// Opus packets of the worker's sound played: every stream of the worker counts the same
    /// one sound, as do the other `audio_` counters.
    pub audio_packets: u64,
    /// Opus packets missing from the sequence (or too late to play).
    pub audio_lost: u64,
    /// Lost packets papered over by concealment (short gaps only;
    /// [`slopty_codec::audio::Conceal`]).
    pub audio_concealed: u64,
    /// Lost packets decoded from a copy a later audio datagram carried.
    pub audio_recovered: u64,
    /// Times playback ran dry under sound (see [`slopty_codec::audio::PlayoutStats`]).
    pub audio_underruns: u64,
    /// Audio dropped to keep the delay at the target.
    pub audio_trimmed: Duration,
    /// Audio played twice to keep the depth at the target.
    pub audio_stretched: Duration,
    /// Depth the jitter buffer aims for when a packet arrives on time.
    pub audio_target: Duration,
    /// Stalls that released: nothing arrived for a stall gap, then everything at once.
    pub stalls: u64,
    /// Time spent stalled, milliseconds (released stalls plus the one in progress).
    pub stalled_ms: u64,
    /// The link is stalled right now (as of the last report).
    pub stalled: bool,
    /// Where every silence past the stall gap went — the stall count broken down by what the
    /// worker's send stamps made of it.
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
    /// RFC 3550 interarrival jitter on the worker's capture clock, as last reported.
    pub jitter: Duration,
    /// Frames the worker is holding in order behind a missing one, as last reported.
    pub queue_depth: u8,
    /// The worker's capture clock placed on this one by the stream's clock probes; `None` until
    /// one has come back.
    pub clock: Option<ClockEstimate>,
    /// When the stream's task published these counters; `None` before its first report. Its
    /// age says whether the task still runs: it reports every `REPORT_EVERY` while it does.
    pub reported_at: Option<Instant>,
}

/// A live client-side stream. Dropping it stops the task and unroutes the stream; the caller
/// still sends `ScreenRequest::Close` so the worker stops capturing.
#[derive(Debug)]
pub struct ScreenHandle {
    stream: StreamId,
    frames: watch::Receiver<Option<Arc<Presentable>>>,
    /// Handed every picture before [`Self::frames`] is, on the decoder's thread.
    present: Arc<PresentSlot>,
    cursor: watch::Receiver<CursorState>,
    stats: watch::Receiver<ScreenStats>,
    /// The worker's sound, which every stream of the connection shares.
    sound: Arc<sound::Sound>,
    /// The worker's capture target is producing pictures (shared with the worker).
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

    /// Hand every picture to `present` on the decoder's thread as it comes out, before
    /// [`Self::frames`] has it; `None` stops. Only pictures decoded from here on reach it.
    pub fn set_present(&self, present: Option<Present>) {
        *self.present.0.lock() = present;
    }

    /// Worker pointer position.
    #[must_use]
    pub fn cursor(&self) -> watch::Receiver<CursorState> {
        self.cursor.clone()
    }

    /// Counters, the audio ones the worker's sound's.
    #[must_use]
    pub fn stats(&self) -> ScreenStats {
        self.sound.stats().over(*self.stats.borrow())
    }

    /// Whether the worker's sound is silenced on this client. Packets keep arriving and are
    /// still decoded (the Opus state stays continuous), only playback stops; other clients are
    /// unaffected. One switch for every stream of the worker: it has one sound.
    #[must_use]
    pub fn muted(&self) -> bool {
        self.sound.muted().get()
    }

    /// Silence or resume the worker's sound on this client, for every one of its streams.
    pub fn set_muted(&self, muted: bool) {
        self.sound.muted().set(muted);
    }

    /// The worker's `ScreenEvent::Source`: whether the capture target is drawing anything. While
    /// it is not, the worker stops asking for refreshes no frame could answer.
    pub fn set_source_live(&self, live: bool) {
        self.source_live.store(live, Ordering::Relaxed);
    }

    /// Whether the worker's capture target is drawing, as last reported.
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
        Self::detached_on(&ScreenRouter::default(), stream)
    }

    /// A handle with no worker behind it, on `router`: the handles of one router share their
    /// worker's sound, and its mute, as a connection's do.
    #[must_use]
    pub fn detached_on(router: &ScreenRouter, stream: StreamId) -> Self {
        let (_frames_tx, frames) = watch::channel(None);
        let (_cursor_tx, cursor) = watch::channel(CursorState::default());
        let (_stats_tx, stats) = watch::channel(ScreenStats::default());
        let sound = {
            let mut slot = router.sound.lock();
            slot.upgrade().unwrap_or_else(|| {
                let sound = Arc::new(sound::Sound::detached(Arc::clone(&router.muted)));
                *slot = Arc::downgrade(&sound);
                sound
            })
        };
        Self {
            stream,
            frames,
            present: Arc::default(),
            cursor,
            stats,
            sound,
            source_live: Arc::new(AtomicBool::new(true)),
            router: router.clone(),
            task: None,
        }
    }
}

/// How a screen worker talks back to the worker.
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
///
/// A striped stream's lower stripe comes on a media stream of its own
/// ([`Stripe::media_of`]), which the router hands this task with the stream's own: each is
/// reassembled and decoded on its own, and a `Stitch` puts the two stripes of a capture up
/// together.
#[must_use]
pub fn spawn_screen(
    runtime: &tokio::runtime::Handle,
    router: &ScreenRouter,
    stream: StreamId,
    codec: VideoCodec,
    uplink: Uplink,
) -> ScreenHandle {
    let sound = router.sound(runtime, &uplink.control);
    let Attached { datagrams, dropped } = router.attach_counted(stream);
    let (frames_tx, frames) = watch::channel(None);
    let (cursor_tx, cursor) = watch::channel(CursorState::default());
    let (stats_tx, stats) = watch::channel(ScreenStats::default());
    let source_live = Arc::new(AtomicBool::new(true));
    let present = Arc::<PresentSlot>::default();
    let output = Arc::new(Output {
        frames: frames_tx,
        present: Arc::clone(&present),
        decode_seq: AtomicU64::new(0),
        estimate: Mutex::new(None),
        first_decoded: Mutex::new(None),
        stitch: Mutex::new(Stitch::default()),
    });
    let failed = Arc::new(Notify::new());
    let rtt = (uplink.rtt)().unwrap_or(DEFAULT_RTT);
    let top = Lane::new(Stripe::media_of(stream, 0), 0, codec, &output, &failed);
    let worker = Worker {
        stream,
        codec,
        datagrams,
        dropped,
        top,
        lower: None,
        top_whole: false,
        output,
        failed,
        out: uplink.control,
        feedback: uplink.feedback,
        path_rtt: uplink.rtt,
        rtt,
        cursor: cursor_tx,
        stats: stats_tx,
        cursor_seq: None,
        counters: ScreenStats::default(),
        lower_counters: ReassemblerStats::default(),
        source_live: Arc::clone(&source_live),
        source_hint: true,
        capture_clock: CaptureClock::new(),
        clock: ClockSync::new(Instant::now()),
        reports: 0,
        unstuck_at: Instant::now(),
    };
    let task = runtime.spawn(worker.run());
    let task = Some(task);
    ScreenHandle {
        stream,
        frames,
        present,
        cursor,
        stats,
        sound,
        source_live,
        router: router.clone(),
        task,
    }
}

/// How long the stitch waits for a capture's other stripe once one has decoded: a display
/// refresh. Past it the capture goes up with that stripe's previous picture, a seam tear
/// counted ([`ScreenStats::seam_tears`]), and the late stripe goes up when it comes.
const STITCH_WAIT: Duration = Duration::from_micros(16_667);

/// How a frame was coded, from its prefix: the stripes coded from its capture (zero for one
/// picture), the build of the worker's sessions that coded it, and the region of the target it
/// shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Coded {
    stripes: u8,
    build: u8,
    region: Option<Region>,
}

/// Where decoded pictures go, from every coded picture's decoder: the stitch, the present hook
/// and the newest-picture channel.
struct Output {
    frames: watch::Sender<Option<Arc<Presentable>>>,
    present: Arc<PresentSlot>,
    /// Counted here, on the decoder's side of the newest-only channel, so the element can tell
    /// how many pictures the channel swallowed before it looked.
    decode_seq: AtomicU64,
    /// Read on the decoder's thread for every picture, written by the worker once an echo.
    estimate: Mutex<Option<ClockEstimate>>,
    /// When the first picture came back.
    first_decoded: Mutex<Option<Instant>>,
    stitch: Mutex<Stitch>,
}

impl Output {
    /// Coder `index`'s decoder returned `frame`, which the datagram that completed it brought
    /// at `arrived`, coded as `coded` says.
    fn decoded(&self, index: usize, frame: DecodedFrame, arrived: Instant, coded: Coded) {
        let decoded = Instant::now();
        self.first_decoded.lock().get_or_insert(decoded);
        let captured = self.estimate.lock().and_then(|e| Captured::by(&e, frame.pts_us));
        // Numbered as it goes up, in [`Self::show`]: the pacer reads a gap as a skipped picture,
        // and a capture's two stripes are one picture.
        let stamp = FrameStamp { pts_us: frame.pts_us, decode_seq: 0, arrived, decoded, captured };
        let picture = if coded.stripes == 0 {
            if index != 0 {
                return;
            }
            self.stitch.lock().whole();
            Some(Presentable { frame, stamp, stripes: None, region: coded.region })
        } else {
            let joined = self.stitch.lock().decoded(index, frame, stamp, coded, decoded);
            joined.map(Joined::presentable)
        };
        if let Some(picture) = picture {
            self.show(picture);
        }
    }

    /// A capture whose other stripe is late past [`STITCH_WAIT`] goes up at `now` without it.
    fn expire(&self, now: Instant) {
        let joined = self.stitch.lock().settle(now);
        if let Some(joined) = joined {
            self.show(joined.presentable());
        }
    }

    fn show(&self, mut picture: Presentable) {
        picture.stamp.decode_seq = self.decode_seq.fetch_add(1, Ordering::Relaxed);
        let picture = Arc::new(picture);
        let present = self.present.0.lock().clone();
        if let Some(present) = present {
            present(&picture);
        }
        let _no_receiver = self.frames.send(Some(picture));
    }
}

/// A stripe's newest picture, and the build of the sessions that coded it.
#[derive(Clone, Debug)]
struct Shown<F> {
    frame: F,
    stamp: FrameStamp,
    /// The build and the region of the capture it was coded from ([`Coded`]).
    coded: Coded,
}

/// The capture the stitch waits on.
#[derive(Clone, Copy, Debug)]
struct Waiting {
    pts: u64,
    coded: Coded,
    /// When the first stripe not yet shown came out: the oldest wait, which a newer capture
    /// that replaces this one keeps, so a stripe running behind never starves the picture.
    since: Instant,
}

/// Two stripes' pictures to show as one.
#[derive(Debug)]
struct Joined<F> {
    top: F,
    lower: F,
    stamp: FrameStamp,
    /// The region of the target the capture shows, which both stripes of one build share.
    region: Option<Region>,
}

impl Joined<DecodedFrame> {
    /// The picture the view shows: the top stripe's rows above the seam, the lower one's from
    /// the seam down. Each codes [`slopty_codec::stripes::OVERLAP`] rows past the seam.
    fn presentable(self) -> Presentable {
        let overlap = slopty_codec::stripes::OVERLAP;
        let top_height = u32::try_from(self.top.image.height()).unwrap_or(u32::MAX);
        Presentable {
            frame: self.top,
            stamp: self.stamp,
            stripes: Some(Stitched {
                lower: self.lower,
                top_rows: top_height.saturating_sub(overlap),
                lower_from: overlap,
            }),
            region: self.region,
        }
    }
}

/// Puts the two stripes of a capture up together.
///
/// Each stripe's session decodes on its own thread. The frame prefix names the stripes coded
/// from a capture: once every one it names has decoded that capture, the two go up as one
/// picture. A stripe it does not name keeps the picture it has (a refresh or a refinement
/// codes only the stripe that needed it), and so does one whose media stream waits for a
/// refresh ([`Self::set_stalled`]): the other stripe goes on without it. One that is merely
/// late is waited for [`STITCH_WAIT`], counted from the first stripe that came out and was
/// not shown: a newer capture replaces the one waited on but not its clock, and a stripe of a
/// capture the other stripe has gone past is only kept as its stripe's newest picture.
///
/// Only stripes of one build of the worker's sessions go up together ([`Coded::build`]): a
/// resize or a chroma switch codes both stripes anew at another size or in another format,
/// and a stripe of the old build beside one of the new would be a picture of neither.
#[derive(Debug)]
struct Stitch<F = DecodedFrame> {
    /// Each stripe's newest picture, top first.
    newest: [Option<Shown<F>>; Stripe::MAX],
    waiting: Option<Waiting>,
    /// Stripes whose media stream waits for a refresh, bit `i` for stripe `i`.
    stalled: u8,
    /// Captures that went up with a stripe's previous picture.
    tears: u64,
}

impl<F> Default for Stitch<F> {
    fn default() -> Self {
        Self { newest: [None, None], waiting: None, stalled: 0, tears: 0 }
    }
}

impl<F: Clone> Stitch<F> {
    /// A picture of the whole stream came out: the stream is not striped (any more).
    fn whole(&mut self) {
        *self = Self { tears: self.tears, ..Self::default() };
    }

    /// Whether stripe `index`'s media stream waits for a refresh.
    const fn set_stalled(&mut self, index: usize, stalled: bool) {
        let bit = 1_u8 << index;
        if stalled { self.stalled |= bit } else { self.stalled &= !bit }
    }

    /// Stripe `index` decoded `frame`, of a capture coded as `coded` says, at `now`: the
    /// picture to put up, when this completed the capture waited on.
    fn decoded(
        &mut self,
        index: usize,
        frame: F,
        stamp: FrameStamp,
        coded: Coded,
        now: Instant,
    ) -> Option<Joined<F>> {
        let pts = stamp.pts_us;
        // A stripe of a capture another stripe has already gone past completes nothing: it is
        // only this stripe's newest picture, for the next capture to go up beside.
        let behind = self.newest.iter().flatten().any(|shown| shown.stamp.pts_us > pts);
        if let Some(slot) = self.newest.get_mut(index) {
            *slot = Some(Shown { frame, stamp, coded });
        }
        if !behind {
            let since = self.waiting.map_or(now, |waiting| waiting.since);
            self.waiting = Some(Waiting { pts, coded, since });
        }
        self.settle(now)
    }

    /// The capture waited on, once every stripe it names has decoded it, or the late ones
    /// have stalled, or [`STITCH_WAIT`] has passed at `now`: shown with the other stripe's
    /// newest picture, if that is of the same build.
    fn settle(&mut self, now: Instant) -> Option<Joined<F>> {
        let waiting = self.waiting?;
        let late = self.missing(waiting) & !self.stalled;
        if late != 0 && now.saturating_duration_since(waiting.since) < STITCH_WAIT {
            return None;
        }
        self.waiting = None;
        let joined = self.joined(waiting)?;
        if self.missing(waiting) != 0 {
            self.tears = self.tears.saturating_add(1);
        }
        Some(joined)
    }

    /// The stripes `waiting` names whose newest picture is not of its capture.
    fn missing(&self, waiting: Waiting) -> u8 {
        let mut missing = 0;
        for (i, newest) in self.newest.iter().enumerate() {
            let bit = 1_u8 << i;
            let has = newest.as_ref().is_some_and(|shown| shown.stamp.pts_us == waiting.pts);
            if waiting.coded.stripes & bit != 0 && !has {
                missing |= bit;
            }
        }
        missing
    }

    /// Whether a capture waits for a stripe.
    const fn waiting(&self) -> bool {
        self.waiting.is_some()
    }

    /// The two stripes' newest pictures as one, stamped as `waiting`'s capture: arrived when
    /// the later of the two did. `None` until both stripes have a picture of its build and its
    /// region: a region that moves keeps the build, and a stripe of the old region beside one
    /// of the new would be a picture of neither place.
    fn joined(&self, waiting: Waiting) -> Option<Joined<F>> {
        let [Some(top), Some(lower)] = &self.newest else { return None };
        let fits = |shown: &Shown<F>| {
            (shown.coded.build, shown.coded.region) == (waiting.coded.build, waiting.coded.region)
        };
        if !fits(top) || !fits(lower) {
            return None;
        }
        let stamp = [top, lower].into_iter().find(|shown| shown.stamp.pts_us == waiting.pts)?.stamp;
        let arrived = if top.stamp.pts_us == lower.stamp.pts_us {
            top.stamp.arrived.max(lower.stamp.arrived)
        } else {
            stamp.arrived
        };
        Some(Joined {
            top: top.frame.clone(),
            lower: lower.frame.clone(),
            stamp: FrameStamp { arrived, ..stamp },
            region: waiting.coded.region,
        })
    }
}

/// Frames a lane's decode thread may have waiting before the lane drops the next itself. A
/// submission returns in about a tenth of a millisecond (MEASUREMENTS.md, "HEVC travels
/// length-prefixed"), so the queue holds a frame at most; one this deep is a decoder that has
/// stopped, and [`DECODE_STUCK`] is what gives it up. Until then the lane asks for nothing: a
/// refresh could not reach the decoder either, and every one answered would be dropped and
/// asked for again.
const DECODE_QUEUE: usize = 32;

/// How long one submission may stay inside VideoToolbox before its decoder is given up on and
/// another built for a keyframe. A submission returns in a fraction of a millisecond and a
/// session is built in a few, 150 ms cold. On a hosted virtual Mac one never returned after the
/// decoder had failed a frame (MEASUREMENTS.md, "a decode submission that never returned").
const DECODE_STUCK: Duration = Duration::from_secs(2);
/// The most of a submission's time inside VideoToolbox one report charges it: two report
/// periods. A report later than that found the worker itself held up.
const STUCK_CREDIT: Duration = Duration::from_millis(100);
/// The longest [`DECODE_STUCK`] grows to as decoders in a row are given up on without a
/// picture: each one given up on leaves a thread waiting inside VideoToolbox.
const DECODE_STUCK_MAX: Duration = Duration::from_secs(32);

/// A complete frame on its way to the decode thread.
struct Submit {
    data: Bytes,
    pts_us: u64,
    /// Its number on the wire, for the log.
    frame: u32,
    discardable: bool,
    restarts: bool,
}

/// Where a lane's decoder leaves what it made of each frame, for the stream worker to take.
#[derive(Clone)]
struct Outcomes {
    /// The coder the lane decodes for: 0 for the whole picture or the top stripe.
    index: usize,
    /// Frames parked for the decoder, and what it left behind.
    inflight: Arc<Mutex<Inflight>>,
    /// Set once the decoder has left something in [`Self::inflight`], so a wake with nothing
    /// back from the decoder does not take the lock the decoder takes.
    news: Arc<AtomicBool>,
    /// Wakes the worker for a failure.
    wake: Arc<Notify>,
    output: Arc<Output>,
}

impl Outcomes {
    /// Something was left in [`Self::inflight`] that asks for an answer: the worker wakes for it.
    fn failed(&self) {
        self.news.store(true, Ordering::Release);
        self.wake.notify_one();
    }
}

/// What a lane's decode thread shares with the stream worker.
#[derive(Debug)]
struct DecodeState {
    /// The number of the submission inside VideoToolbox now, counting from one; zero while
    /// none is.
    submitting: AtomicU64,
    /// Frames handed to the thread and not yet taken by it.
    queued: AtomicUsize,
    /// The decoder has a session ([`Decoder::ready`]), as of its last submission.
    ready: AtomicBool,
    /// Given up on: nothing more is submitted to it, and what it returns is not shown.
    abandoned: AtomicBool,
    /// Pictures it returned.
    pictures: AtomicU64,
}

/// A lane's decoder, on a thread of its own: the stream worker only hands it complete frames.
///
/// A VideoToolbox call can block its caller. On a hosted virtual Mac, after the decoder had
/// failed a frame, the stream's task stopped as if one had for good, and took the runtime
/// thread it ran on with it: it read no more datagrams, sent no reports, asked for no refresh,
/// and its counters stopped where they were. The worker's encoders run on threads of their own
/// for the same reason. Dropping it lets the thread end, which invalidates the session there,
/// since that waits for the session's callbacks.
struct DecodeThread {
    submits: std::sync::mpsc::SyncSender<Submit>,
    state: Arc<DecodeState>,
}

impl DecodeThread {
    /// A decoder for `media`'s frames, leaving what it makes of each in `outcomes`. A thread the
    /// system will not start leaves a decoder that refuses everything as gone, which the lane
    /// replaces ([`Lane::replace_decoder`]).
    fn start(media: StreamId, codec: VideoCodec, outcomes: &Outcomes) -> Self {
        let state = Arc::new(DecodeState {
            submitting: AtomicU64::new(0),
            queued: AtomicUsize::new(0),
            ready: AtomicBool::new(false),
            abandoned: AtomicBool::new(false),
            pictures: AtomicU64::new(0),
        });
        let decoder = {
            let state = Arc::clone(&state);
            let Outcomes { index, inflight, news, wake, output } = outcomes.clone();
            Decoder::with_outcomes(codec, move |outcome| {
                if state.abandoned.load(Ordering::Acquire) {
                    return;
                }
                let frame = match outcome {
                    Ok(frame) => frame,
                    Err(failure) => {
                        inflight.lock().failed(failure);
                        news.store(true, Ordering::Release);
                        wake.notify_one();
                        return;
                    }
                };
                state.pictures.fetch_add(1, Ordering::Relaxed);
                // A picture whose arrival is no longer parked (a duplicate from the decoder, or
                // one that outlived the ring) is still shown; its timing simply does not enter
                // the ring.
                let parked = inflight.lock().decoded(frame.pts_us);
                news.store(true, Ordering::Release);
                let (arrived, coded) = parked.unwrap_or_else(|| (Instant::now(), Coded::default()));
                output.decoded(index, frame, arrived, coded);
            })
        };
        let (submits, queue) = std::sync::mpsc::sync_channel::<Submit>(DECODE_QUEUE);
        let thread = Arc::clone(&state);
        let outcomes = outcomes.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("slopty-decode-{}", media.0))
            .spawn(move || decode_thread(media, decoder, &queue, &thread, &outcomes));
        if let Err(e) = spawned {
            tracing::warn!(stream = %media, error = %e, "no thread to decode on");
        }
        Self { submits, state }
    }

    /// Hand `submit` to the thread: the frame back when its queue is full, or the thread gone.
    fn submit(&self, submit: Submit) -> Result<(), std::sync::mpsc::TrySendError<Submit>> {
        self.state.queued.fetch_add(1, Ordering::AcqRel);
        let sent = self.submits.try_send(submit);
        if sent.is_err() {
            self.state.queued.fetch_sub(1, Ordering::AcqRel);
        }
        sent
    }

    /// The thread has taken all but a few of what it was handed.
    fn has_room(&self) -> bool {
        self.state.queued.load(Ordering::Acquire) < DECODE_QUEUE / 2
    }

    fn ready(&self) -> bool {
        self.state.ready.load(Ordering::Acquire)
    }
}

impl Drop for DecodeThread {
    fn drop(&mut self) {
        self.state.abandoned.store(true, Ordering::Release);
    }
}

/// The decode thread's loop: submit each frame `queue` brings, and leave a refusal in
/// `outcomes` for the worker. It ends once its [`DecodeThread`] is dropped or given up on.
fn decode_thread(
    media: StreamId,
    mut decoder: Decoder,
    queue: &std::sync::mpsc::Receiver<Submit>,
    state: &DecodeState,
    outcomes: &Outcomes,
) {
    slopty_platform::user_interactive_thread();
    let mut submissions = 0_u64;
    while let Ok(submit) = queue.recv() {
        state.queued.fetch_sub(1, Ordering::AcqRel);
        if state.abandoned.load(Ordering::Acquire) {
            break;
        }
        submissions = submissions.saturating_add(1);
        state.submitting.store(submissions, Ordering::Release);
        #[cfg(test)]
        worker_tests::hold::wait(media);
        let result = decoder.decode(&submit.data, submit.pts_us);
        state.submitting.store(0, Ordering::Release);
        let ready = decoder.ready();
        state.ready.store(ready, Ordering::Release);
        if state.abandoned.load(Ordering::Acquire) {
            break;
        }
        let Err(e) = result else { continue };
        let status =
            if let slopty_codec::CodecError::Os { status, .. } = &e { Some(*status) } else { None };
        let refresh = refresh_after_refusal(submit.discardable, ready, submit.restarts);
        tracing::debug!(stream = %media, frame = submit.frame, error = %e, ?refresh, "decode refused");
        outcomes.inflight.lock().refused(submit.pts_us, status, refresh);
        outcomes.failed();
    }
}

/// One coded picture's way in: the whole picture, or one stripe of it, on its media stream.
struct Lane {
    media: StreamId,
    codec: VideoCodec,
    reassembler: Reassembler,
    decoder: DecodeThread,
    /// Where the decoder leaves what it made of each frame.
    outcomes: Outcomes,
    /// Timestamp of the last frame that restarts decoding (a keyframe or an LTR refresh). A
    /// failure older than it was already answered by it.
    restart_pts: Option<u64>,
    /// How long a submission may stay inside VideoToolbox before the decoder in place is given
    /// up on: [`DECODE_STUCK`], doubled for each decoder in a row given up on without a picture.
    stuck_after: Duration,
    /// The submission seen inside VideoToolbox at the last report, and how long the worker has
    /// run on time since it went in ([`Worker::unstick`]).
    stuck: Option<(u64, Duration)>,
    /// A frame was dropped for a full decode queue ([`DECODE_QUEUE`]): what is predicted from it
    /// is dropped too, until a restart reaches the decoder or one is asked for once it has room.
    backed_up: bool,
}

impl Lane {
    /// Coder `index`'s lane, on `media`: its pictures go to `output`, and a failure wakes the
    /// worker through `failed`.
    fn new(
        media: StreamId,
        index: usize,
        codec: VideoCodec,
        output: &Arc<Output>,
        failed: &Arc<Notify>,
    ) -> Self {
        let outcomes = Outcomes {
            index,
            inflight: Arc::default(),
            news: Arc::default(),
            wake: Arc::clone(failed),
            output: Arc::clone(output),
        };
        Self {
            media,
            codec,
            // The loop ticks after every branch, so the longest it goes without one is the idle
            // sleep; the reassembler needs that number to tell a link that held datagrams from
            // a runtime that did not run this task.
            reassembler: Reassembler::new(
                media,
                Config {
                    tick_period: IDLE_TICK,
                    first_repeat_after: FIRST_KEYFRAME_WAIT,
                    ..Config::default()
                },
                Instant::now(),
            ),
            decoder: DecodeThread::start(media, codec, &outcomes),
            outcomes,
            restart_pts: None,
            stuck_after: DECODE_STUCK,
            stuck: None,
            backed_up: false,
        }
    }

    /// Give the decoder in place up and start another, which waits for a keyframe: what the
    /// old one was working on never comes back, and a picture it returns after all is not
    /// shown. The old thread ends once its call into VideoToolbox returns, if it ever does.
    fn replace_decoder(&mut self, now: Instant) {
        self.stuck_after = if self.decoder.state.pictures.load(Ordering::Relaxed) > 0 {
            DECODE_STUCK
        } else {
            self.stuck_after.saturating_mul(2).min(DECODE_STUCK_MAX)
        };
        self.decoder = DecodeThread::start(self.media, self.codec, &self.outcomes);
        self.outcomes.inflight.lock().parked.clear();
        self.restart_pts = None;
        self.stuck = None;
        self.backed_up = false;
        self.reassembler.force_refresh(now, true);
    }

    /// Frames waiting behind a missing one, or a refresh asked for and not yet answered.
    fn busy(&self) -> bool {
        self.reassembler.queue_depth() > 0 || self.reassembler.awaiting_refresh()
    }
}

struct Worker {
    stream: StreamId,
    codec: VideoCodec,
    datagrams: mpsc::Receiver<Arrival>,
    /// The stream's datagrams the router dropped before this worker read them.
    dropped: Arc<AtomicU64>,
    /// The whole picture, or the top stripe: the stream's own media stream, which also carries
    /// its cursor, heartbeats and clock echoes.
    top: Lane,
    /// The lower stripe, from its first datagram until the stream is one picture again.
    lower: Option<Lane>,
    /// The top lane's newest frame was of the whole picture.
    top_whole: bool,
    output: Arc<Output>,
    /// A decoder failed or refused a frame.
    failed: Arc<Notify>,
    out: mpsc::Sender<ClientMsg>,
    feedback: Box<dyn Fn(Bytes) -> bool + Send>,
    /// Reads the path's round trip off the connection, under its lock: once a report.
    path_rtt: Box<dyn Fn() -> Option<Duration> + Send>,
    /// The round trip as of the last report, which the reassembler's timers go by.
    rtt: Duration,
    cursor: watch::Sender<CursorState>,
    stats: watch::Sender<ScreenStats>,
    cursor_seq: Option<u32>,
    counters: ScreenStats,
    /// The lower stripe's reassembler counters as of its last report.
    lower_counters: ReassemblerStats,
    /// The worker says its capture target is producing pictures.
    source_live: Arc<AtomicBool>,
    /// The last hint handed to the reassembler, so a hint that has not changed does not
    /// overwrite what the stream itself proved (a video fragment means the source is live,
    /// whatever the worker last said).
    source_hint: bool,
    /// Widens the wire's 32-bit capture timestamp, so ordering survives its ~71-minute wrap.
    capture_clock: CaptureClock,
    /// Places the worker's capture clock on this one from the probes' echoes.
    clock: ClockSync,
    /// Reports sent so far: the probes ride on their schedule.
    reports: u64,
    /// When the decoders were last looked at for a stuck submission ([`Self::unstick`]).
    unstuck_at: Instant,
}

impl Worker {
    async fn run(mut self) {
        let mut report = tokio::time::interval(REPORT_EVERY);
        report.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // One timer for the task's life, moved on each wake: a deadline pushed later is one
        // atomic update, where a sleep made anew registered with the timer wheel and the one
        // dropped unregistered, each under the wheel's lock.
        let tick = tokio::time::sleep(IDLE_TICK);
        tokio::pin!(tick);
        loop {
            let busy = self.top.busy()
                || self.lower.as_ref().is_some_and(Lane::busy)
                || self.output.stitch.lock().waiting();
            let period = if busy { TICK } else { IDLE_TICK };
            let now = tokio::time::Instant::now();
            tick.as_mut().reset(now.checked_add(period).unwrap_or(now));
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
                () = &mut tick => {}
                () = self.failed.notified() => {}
                _instant = report.tick() => self.report(),
            }
            self.outcomes();
            if !self.actions() {
                break;
            }
            // The tick's own `drain` can release a frame that was queued behind a lost one, and
            // on a still screen the next datagram is a heartbeat half a stall gap away: without
            // this the picture would wait for it.
            self.deliver(0);
            self.deliver(1);
            self.stitch();
        }
        tracing::debug!(stream = %self.stream, "screen worker finished");
    }

    /// Coder `index`'s lane: the top one, or the lower stripe's while there is one.
    const fn lane(&mut self, index: usize) -> Option<&mut Lane> {
        if index == 0 { Some(&mut self.top) } else { self.lower.as_mut() }
    }

    /// Tell the stitch which stripes wait for a refresh, and put up a capture whose other
    /// stripe is late past [`STITCH_WAIT`].
    fn stitch(&self) {
        let lower = self.lower.as_ref().is_some_and(|lane| lane.reassembler.awaiting_refresh());
        {
            let mut stitch = self.output.stitch.lock();
            stitch.set_stalled(0, self.top.reassembler.awaiting_refresh());
            stitch.set_stalled(1, lower);
        }
        self.output.expire(Instant::now());
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
        let Some((header, _payload)) = MediaHeader::parse(datagram) else { return };
        let (_stream, index) = Stripe::stream_of(StreamId(header.stream.get()));
        if index != 0 {
            // A lower stripe's datagram still on its way when the stream went back to one
            // picture opens no lane; a new striped session starts on its keyframe.
            if self.lower.is_none() && self.top_whole && header.flags & flags::KEYFRAME == 0 {
                return;
            }
            let media = StreamId(header.stream.get());
            let lower = self.lower.get_or_insert_with(|| {
                tracing::debug!(stream = %self.stream, %media, "the lower stripe's first datagram");
                Lane::new(media, 1, self.codec, &self.output, &self.failed)
            });
            if lower.reassembler.ingest(datagram, now) == Ingest::Video {
                self.deliver(1);
            }
            return;
        }
        self.clock_echo(datagram, now);
        match self.top.reassembler.ingest(datagram, now) {
            Ingest::Video => self.deliver(0),
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
            // The worker's sound comes on its own media stream ([`sound`]).
            Ingest::Audio { .. } | Ingest::Heartbeat | Ingest::Ignored(_) => {}
        }
    }

    /// Take a clock probe's echo that arrived at `now`: the arrival stamp is the connection
    /// reader's, so the time this task took to get to it is not charged to the round trip.
    fn clock_echo(&mut self, datagram: &Bytes, now: Instant) {
        let Some((header, payload)) = MediaHeader::parse(datagram) else { return };
        if header.kind() != Some(Kind::Clock) {
            return;
        }
        let Some(echo) = ClockEcho::parse(payload) else { return };
        self.clock.observe(echo.sent.get(), echo.received.get(), echo.echoed.get(), now);
        *self.output.estimate.lock() = self.clock.estimate();
    }

    /// Send a clock probe when one is due: every report at first, then every [`PROBE_EVERY`].
    /// `false` once the connection is gone.
    fn probe(&mut self) -> bool {
        let due = self.reports < PROBES_AT_ONCE || self.reports.is_multiple_of(PROBE_EVERY);
        self.reports = self.reports.saturating_add(1);
        if !due {
            return true;
        }
        let sent_us = self.clock.stamp(Instant::now());
        (self.feedback)(encode_feedback(Feedback::Clock { stream: self.stream, sent_us }))
    }

    /// Push every frame of coder `index`'s lane that is now complete into its decoder. A frame
    /// of the whole picture on the top lane says the stream is not striped: the lower stripe's
    /// lane goes, so it asks for nothing more.
    fn deliver(&mut self, index: usize) {
        let Self { top, lower, counters, capture_clock, stream, top_whole, .. } = self;
        let Some(lane) = (if index == 0 { Some(top) } else { lower.as_mut() }) else { return };
        let (mut whole, mut frame_seen) = (false, false);
        while let Some(frame) = lane.reassembler.next_frame() {
            frame_seen = true;
            if index == 0 {
                counters.frames = counters.frames.saturating_add(1);
            }
            counters.first_frame_at.get_or_insert_with(Instant::now);
            if frame.hold > counters.hold_max {
                counters.hold_max = frame.hold;
            }
            whole = index == 0 && frame.info.stripes == 0;
            // Widened here, at the one place the wire's 32-bit stamp becomes a u64: the decoder
            // echoes whatever it is given back to the callback, so the parked arrivals and the
            // pacer's ordering both inherit a timestamp that survives the wrap.
            let pts = capture_clock.widen(frame.info.capture_ts_us);
            // Park the frame before submitting: VideoToolbox may call back on another thread
            // before `decode` returns. Its token is acknowledged only once a picture comes back.
            let restarts = frame.info.keyframe || frame.info.ltr_refresh;
            let discardable = frame.info.discardable;
            lane.outcomes.inflight.lock().park(Parked {
                pts_us: pts,
                arrived: frame.arrived,
                ltr_token: frame.info.ltr_token,
                restarts,
                discardable,
                coded: Coded {
                    stripes: frame.info.stripes,
                    build: frame.info.build,
                    region: frame.info.region,
                },
            });
            if restarts {
                lane.restart_pts = Some(pts);
            }
            if lane.backed_up && !restarts {
                lane.outcomes.inflight.lock().unpark(pts);
                counters.decode_errors = counters.decode_errors.saturating_add(1);
                continue;
            }
            let submit = Submit {
                data: frame.data,
                pts_us: pts,
                frame: frame.info.frame,
                discardable,
                restarts,
            };
            match lane.decoder.submit(submit) {
                Ok(()) => lane.backed_up = false,
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    if !lane.backed_up {
                        tracing::debug!(stream = %lane.media, frame = frame.info.frame, "the decoder is a queue behind: frames dropped");
                    }
                    lane.outcomes.inflight.lock().unpark(pts);
                    counters.decode_errors = counters.decode_errors.saturating_add(1);
                    lane.backed_up = true;
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    tracing::warn!(stream = %lane.media, "the decode thread is gone: a new decoder");
                    lane.replace_decoder(Instant::now());
                    counters.decoders_replaced = counters.decoders_replaced.saturating_add(1);
                }
            }
        }
        if index == 0 && frame_seen {
            *top_whole = whole;
        }
        counters.striped = lower.is_some() && !*top_whole;
        if whole && lower.take().is_some() {
            tracing::debug!(stream = %stream, "one picture again: the lower stripe's lane goes");
        }
    }

    /// Take what the decoders' callbacks left: acknowledge the references they decoded, and ask
    /// for a refresh when one failed on a frame the last restart did not already replace.
    fn outcomes(&mut self) {
        for index in 0..Stripe::MAX {
            let Self { top, lower, counters, .. } = self;
            let Some(lane) = (if index == 0 { Some(top) } else { lower.as_mut() }) else {
                continue;
            };
            if !lane.outcomes.news.swap(false, Ordering::Acquire) {
                continue;
            }
            let (acks, failed, failed_discardable, status, refused, refusal_refresh) = {
                let mut inflight = lane.outcomes.inflight.lock();
                (
                    std::mem::take(&mut inflight.acks),
                    inflight.failed.take(),
                    std::mem::take(&mut inflight.failed_discardable),
                    inflight.status.take(),
                    std::mem::take(&mut inflight.refused),
                    inflight.refusal_refresh.take(),
                )
            };
            for token in acks {
                lane.reassembler.ack_ltr(token);
            }
            if let Some(Status(status)) = status {
                counters.decode_failure = status;
            }
            counters.decode_errors =
                counters.decode_errors.saturating_add(failed_discardable).saturating_add(refused);
            if let Some(keyframe) = refusal_refresh {
                tracing::debug!(stream = %lane.media, refused, keyframe, "the decoder refused frames");
                lane.reassembler.force_refresh(Instant::now(), keyframe);
            }
            let Some(failed) = failed else { continue };
            counters.decode_errors = counters.decode_errors.saturating_add(failed.count);
            let Some(keyframe) = refresh_for(failed, lane.restart_pts, lane.decoder.ready()) else {
                continue;
            };
            tracing::debug!(stream = %lane.media, ?failed, keyframe, "decoder returned no picture");
            lane.reassembler.force_refresh(Instant::now(), keyframe);
        }
    }

    /// Run the reassemblers' timers; `false` when the connection is gone.
    fn actions(&mut self) -> bool {
        let hint = self.source_live.load(Ordering::Relaxed);
        let hinted = hint != self.source_hint;
        self.source_hint = hint;
        for index in 0..Stripe::MAX {
            let rtt = self.rtt;
            let Some(lane) = self.lane(index) else { continue };
            if hinted {
                lane.reassembler.set_source_live(hint);
            }
            let media = lane.media;
            let actions = lane.reassembler.tick(Instant::now(), rtt);
            for action in actions {
                let feedback = match action {
                    Action::Nack { frame, fragments } => {
                        self.counters.nacks = self.counters.nacks.saturating_add(1);
                        Feedback::Nack { stream: media, frame, fragments }
                    }
                    Action::RequestRefresh { last_good_frame, keyframe } => {
                        self.counters.refreshes = self.counters.refreshes.saturating_add(1);
                        Feedback::Refresh { stream: media, last_good_frame, keyframe }
                    }
                };
                if !(self.feedback)(encode_feedback(feedback)) {
                    return false;
                }
            }
        }
        true
    }

    /// Publish the counters and send the worker a receiver report for each media stream. A
    /// report never waits for room on the control channel: on a full one its counts and
    /// acknowledgements go into the next report, [`REPORT_EVERY`] later, rather than holding
    /// reassembly and decode behind it.
    fn report(&mut self) {
        self.outcomes();
        self.rtt = (self.path_rtt)().unwrap_or(DEFAULT_RTT);
        let now = Instant::now();
        self.unstick(now);
        let report = self.top.reassembler.take_report(now, 0);
        let stats = self.top.reassembler.stats();
        if let Some(lower) = &self.lower {
            self.lower_counters = lower.reassembler.stats();
        }
        let lower = &self.lower_counters;
        self.counters.frames_fec = stats.frames_fec.saturating_add(lower.frames_fec);
        self.counters.frames_retransmit =
            stats.frames_retransmit.saturating_add(lower.frames_retransmit);
        self.counters.frames_lost = stats.frames_lost.saturating_add(lower.frames_lost);
        self.counters.frames_skipped = stats.frames_skipped.saturating_add(lower.frames_skipped);
        self.counters.datagrams_lost = stats.datagrams_lost.saturating_add(lower.datagrams_lost);
        self.counters.parity_permille = observed_parity(&stats);
        self.counters.data_shards = stats.data_shards.saturating_add(lower.data_shards);
        self.counters.parity_shards = stats.parity_shards.saturating_add(lower.parity_shards);
        self.counters.stalls = stats.stalls;
        self.counters.stalled_ms = stats.stalled_ms;
        self.counters.silences = stats.silences;
        self.counters.stalled = self.top.reassembler.stalled(now);
        self.counters.hold_p50 = report.hold_p50.to_std();
        self.counters.hold_p95 = report.hold_p95.to_std();
        self.counters.jitter = report.owd_jitter.to_std();
        self.counters.queue_depth = report.queue_depth;
        self.counters.decoding = [Some(&self.top), self.lower.as_ref()]
            .into_iter()
            .flatten()
            .map(|lane| {
                u64::try_from(lane.outcomes.inflight.lock().parked.len()).unwrap_or(u64::MAX)
            })
            .sum();
        self.counters.datagrams_dropped = self.dropped.load(Ordering::Relaxed);
        self.counters.reported_at = Some(now);
        self.counters.first_decoded_at = *self.output.first_decoded.lock();
        self.counters.seam_tears = self.output.stitch.lock().tears;
        self.counters.clock = self.clock.estimate();
        self.stats.send_replace(self.counters);
        let stream = self.stream;
        if !self.send_report(stream, report) {
            self.top.reassembler.take_back(&report);
        }
        if let Some(lower) = &mut self.lower {
            let report = lower.reassembler.take_report(now, 0);
            let media = lower.media;
            let sent = self
                .out
                .try_send(ClientMsg::Screen(ScreenRequest::Report { stream: media, report }));
            if let Err(mpsc::error::TrySendError::Full(_)) = sent {
                lower.reassembler.take_back(&report);
            }
        }
        // The connection going is the feedback's to notice: the next NACK or refresh sees it.
        let _connected = self.probe();
    }

    /// Give up on a decoder whose submission has stayed inside VideoToolbox past its lane's
    /// patience, and start another for a keyframe ([`Lane::replace_decoder`]). A decoder that
    /// dropped frames for a full queue and has room again is asked a refresh for.
    ///
    /// The patience is spent only while this worker runs on time: a report later than
    /// [`STUCK_CREDIT`] says the process itself was held, as a starved machine holds it for
    /// seconds, and a submission that waited out the same hold has not stopped.
    fn unstick(&mut self, now: Instant) {
        let ran = now.saturating_duration_since(self.unstuck_at).min(STUCK_CREDIT);
        self.unstuck_at = now;
        for index in 0..Stripe::MAX {
            let Self { top, lower, counters, .. } = self;
            let Some(lane) = (if index == 0 { Some(top) } else { lower.as_mut() }) else {
                continue;
            };
            let inside = lane.decoder.state.submitting.load(Ordering::Acquire);
            lane.stuck = (inside != 0).then(|| match lane.stuck {
                Some((seen, stuck)) if seen == inside => (inside, stuck.saturating_add(ran)),
                _ => (inside, Duration::ZERO),
            });
            let stuck = lane.stuck.map(|(_, stuck)| stuck);
            let Some(stuck) = stuck.filter(|stuck| *stuck >= lane.stuck_after) else {
                if lane.backed_up && lane.decoder.has_room() {
                    tracing::debug!(stream = %lane.media, "the decoder caught up: a refresh");
                    lane.backed_up = false;
                    lane.reassembler.force_refresh(now, !lane.decoder.ready());
                }
                continue;
            };
            tracing::warn!(stream = %lane.media, ?stuck, "a decode submission has not returned: a new decoder");
            lane.replace_decoder(now);
            counters.decoders_replaced = counters.decoders_replaced.saturating_add(1);
        }
    }

    /// Hand the control stream `report` for `media`: `false` when it was full, for the caller to
    /// carry the report to the next.
    fn send_report(&self, media: StreamId, report: ReceiverReport) -> bool {
        match self.out.try_send(ClientMsg::Screen(ScreenRequest::Report { stream: media, report }))
        {
            Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::debug!(stream = %media, "control channel full; report carried to the next");
                false
            }
        }
    }
}

/// What the decoder refusing a frame as it was submitted asks of the worker: nothing for a
/// frame nothing refers to while the decoder's session holds, since the next frame decodes;
/// otherwise a refresh, and a keyframe when no session is left or the refused frame was itself
/// a restart, which would fail again off the same references. The callback's failures go
/// through [`refresh_for`].
const fn refresh_after_refusal(discardable: bool, ready: bool, restarts: bool) -> Option<bool> {
    if discardable && ready { None } else { Some(!ready || restarts) }
}

/// The refresh a decode failure asks for, `Some(keyframe)`; `None` when the last restart,
/// sent after the failed frame, already replaces it.
fn refresh_for(failed: Failed, restart_pts: Option<u64>, ready: bool) -> Option<bool> {
    if restart_pts.is_some_and(|restart| failed.pts_us < restart) {
        return None;
    }
    Some(failed.session_lost || failed.restart || !ready)
}

/// The parity ratio the worker is sending, in thousandths of the data fragments, from the frame
/// layouts seen so far. Zero until a frame arrives.
fn observed_parity(stats: &ReassemblerStats) -> u16 {
    let permille =
        stats.parity_shards.saturating_mul(1000).checked_div(stats.data_shards).unwrap_or(0);
    u16::try_from(permille).unwrap_or(u16::MAX)
}

/// Encode loss feedback for one datagram. A fragment list too long for a datagram degrades to
/// "every fragment", which the worker answers with the whole frame.
fn encode_feedback(feedback: Feedback) -> Bytes {
    let encode = |feedback| ClientDatagram::Feedback(feedback).encode();
    let feedback = match encode(feedback.clone()) {
        Ok(body) if body.len() <= MAX_DATAGRAM => return body,
        Ok(_) | Err(_) => feedback,
    };
    let whole = match feedback {
        Feedback::Nack { stream, frame, .. } => {
            Feedback::Nack { stream, frame, fragments: Vec::new() }
        }
        other @ (Feedback::Refresh { .. } | Feedback::Clock { .. }) => other,
    };
    encode(whole).unwrap_or_default()
}

/// The feedback in a datagram [`encode_feedback`] made.
#[cfg(test)]
fn decode_feedback(bytes: &[u8]) -> Option<Feedback> {
    match ClientDatagram::decode(bytes)? {
        ClientDatagram::Feedback(feedback) => Some(feedback),
        ClientDatagram::Input { .. } | ClientDatagram::ScreenInput { .. } => None,
    }
}

#[cfg(test)]
mod arrival_tests {
    use super::*;

    /// A frame of one picture, parked.
    pub(super) const fn parked(
        pts_us: u64,
        arrived: Instant,
        ltr_token: Option<u64>,
        restarts: bool,
        discardable: bool,
    ) -> Parked {
        let coded = Coded { stripes: 0, build: 0, region: None };
        Parked { pts_us, arrived, ltr_token, restarts, discardable, coded }
    }

    /// The decoder's callback finds the arrival its frame was parked under, and the frames
    /// before it are forgotten with it (the decoder never goes back).
    #[test]
    fn a_parked_arrival_comes_back_with_its_frame_and_clears_the_older_ones() {
        let epoch = Instant::now();
        let at = |ms: u64| epoch.checked_add(Duration::from_millis(ms)).unwrap();
        let mut arrivals = Inflight::default();
        for i in 0..4_u64 {
            arrivals.park(parked(i * 1_000, at(i * 16), None, false, false));
        }
        assert_eq!(arrivals.decoded(2_000), Some((at(32), Coded::default())));
        assert_eq!(arrivals.decoded(1_000), None, "older frames went with it");
        assert_eq!(arrivals.decoded(3_000), Some((at(48), Coded::default())));
        assert_eq!(arrivals.decoded(3_000), None, "taken once");
    }

    /// A long-term reference is acknowledged when the decoder returns its picture, never when
    /// it was only submitted, and never when the decoder failed on it. Failures fold into one,
    /// and a lost session in any of them says only a keyframe helps.
    #[test]
    fn a_token_is_acknowledged_by_its_picture_and_a_failure_is_kept() {
        let epoch = Instant::now();
        let mut inflight = Inflight::default();
        inflight.park(parked(1, epoch, Some(11), false, false));
        inflight.park(parked(2, epoch, Some(12), false, false));
        inflight.park(parked(3, epoch, None, false, false));
        inflight.park(parked(4, epoch, Some(14), false, false));
        assert!(inflight.acks.is_empty(), "submitted is not decoded");
        let failure = |pts_us: u64, status: i32| DecodeFailure { pts_us, status };
        inflight.failed(failure(1, -12_909));
        assert_eq!(inflight.decoded(2), Some((epoch, Coded::default())));
        inflight.failed(failure(3, -12_903));
        assert_eq!(inflight.acks, vec![12], "only the frame that came back");
        assert_eq!(
            inflight.failed,
            Some(Failed { pts_us: 3, session_lost: true, restart: false, count: 2 }),
            "the newest failure, and the session is gone"
        );
        assert_eq!(inflight.decoded(4), Some((epoch, Coded::default())));
        assert_eq!(inflight.acks, vec![12, 14]);
        assert!(inflight.parked.is_empty());
    }

    /// A refresh that itself fails to decode is answered with a keyframe: asking for another
    /// refresh off the same references could fail the same way for good. A failure after a
    /// refresh that decoded asks for a refresh, and one before the last restart for nothing.
    #[test]
    fn a_failed_refresh_asks_for_a_keyframe() {
        let epoch = Instant::now();
        let failure = |pts_us: u64| DecodeFailure { pts_us, status: -12_909 };
        let mut inflight = Inflight::default();
        inflight.park(parked(10, epoch, None, true, false));
        inflight.park(parked(11, epoch, None, false, false));
        inflight.failed(failure(10));
        let failed = inflight.failed.take().unwrap();
        assert_eq!(refresh_for(failed, Some(10), true), Some(true), "the refresh failed");
        inflight.failed(failure(11));
        let failed = inflight.failed.take().unwrap();
        assert_eq!(refresh_for(failed, Some(10), true), Some(false), "a frame after it failed");
        // Both fail before the worker looks: the fold keeps that the refresh was among them.
        inflight.park(parked(13, epoch, None, true, false));
        inflight.park(parked(14, epoch, None, false, false));
        inflight.failed(failure(13));
        inflight.failed(failure(14));
        let failed = inflight.failed.take().unwrap();
        assert_eq!(refresh_for(failed, Some(13), true), Some(true), "the refresh was among them");
        inflight.park(parked(12, epoch, None, false, false));
        inflight.failed(failure(12));
        let failed = inflight.failed.take().unwrap();
        assert_eq!(refresh_for(failed, Some(20), true), None, "already replaced");
        assert_eq!(refresh_for(failed, Some(12), false), Some(true), "no session left");
    }

    /// A frame the decoder refuses as it is submitted: one nothing refers to asks for nothing
    /// while the session holds; any other asks for a refresh, a keyframe once the session is
    /// gone or when the refused frame was a restart.
    #[test]
    fn a_refused_frame_asks_by_what_it_was_and_what_is_left() {
        assert_eq!(refresh_after_refusal(true, true, false), None, "skippable, session holds");
        assert_eq!(refresh_after_refusal(true, false, false), Some(true), "no session left");
        assert_eq!(refresh_after_refusal(false, true, false), Some(false), "a refresh");
        assert_eq!(refresh_after_refusal(false, true, true), Some(true), "a restart failed");
        assert_eq!(refresh_after_refusal(false, false, false), Some(true));
    }

    /// A frame nothing refers to that fails with the session intact asks for nothing; one that
    /// lost the session asks for a keyframe like any other.
    #[test]
    fn a_failed_frame_nothing_refers_to_asks_for_nothing_unless_the_session_went() {
        let epoch = Instant::now();
        let mut inflight = Inflight::default();
        inflight.park(parked(1, epoch, Some(11), false, true));
        inflight.failed(DecodeFailure { pts_us: 1, status: -12_909 });
        assert_eq!((inflight.failed, inflight.failed_discardable), (None, 1));
        assert!(inflight.acks.is_empty(), "its token is not acknowledged");
        inflight.park(parked(2, epoch, None, false, true));
        inflight.failed(DecodeFailure { pts_us: 2, status: -12_903 });
        assert_eq!(
            inflight.failed,
            Some(Failed { pts_us: 2, session_lost: true, restart: false, count: 1 })
        );
    }

    /// The ring is bounded: a decoder that never calls back cannot grow it.
    #[test]
    fn the_ring_forgets_the_oldest_arrivals() {
        let epoch = Instant::now();
        let mut arrivals = Inflight::default();
        for i in 0..(u64::try_from(ARRIVALS).unwrap() + 10) {
            arrivals.park(parked(i, epoch, None, false, false));
        }
        assert_eq!(arrivals.parked.len(), ARRIVALS);
        assert_eq!(arrivals.decoded(0), None);
        assert_eq!(
            arrivals.decoded(u64::try_from(ARRIVALS).unwrap()),
            Some((epoch, Coded::default()))
        );
    }
}

#[cfg(test)]
mod stitch_tests {
    use super::*;

    /// A stripe's picture, named by its capture and its stripe.
    type Picture = (u64, usize);

    const BOTH: u8 = 0b11;

    struct Clock(Instant);

    impl Clock {
        fn at(&self, ms: f64) -> Instant {
            self.0.checked_add(Duration::from_secs_f64(ms / 1000.0)).unwrap()
        }
    }

    fn stamp(pts: u64, at: Instant) -> FrameStamp {
        FrameStamp { pts_us: pts, decode_seq: 0, arrived: at, decoded: at, captured: None }
    }

    /// Stripe `index` decodes capture `pts`, coded as `stripes` of build `build`, at `at`:
    /// the captures that went up, as the two pictures shown.
    fn decode(
        stitch: &mut Stitch<Picture>,
        index: usize,
        pts: u64,
        (stripes, build): (u8, u8),
        at: Instant,
    ) -> Option<(Picture, Picture)> {
        let coded = Coded { stripes, build, region: None };
        stitch.decoded(index, (pts, index), stamp(pts, at), coded, at).map(|j| (j.top, j.lower))
    }

    /// A capture goes up once both its stripes have decoded it, stamped with the later
    /// arrival, and the first alone waits.
    #[test]
    fn both_stripes_of_a_capture_go_up_together() {
        let t = Clock(Instant::now());
        let mut stitch = Stitch::default();
        assert_eq!(decode(&mut stitch, 0, 10, (BOTH, 1), t.at(0.0)), None, "the lower is out");
        assert!(stitch.waiting());
        let joined = stitch.decoded(
            1,
            (10, 1),
            stamp(10, t.at(3.0)),
            Coded { stripes: BOTH, build: 1, region: None },
            t.at(3.0),
        );
        let joined = joined.expect("both in");
        assert_eq!((joined.top, joined.lower), ((10, 0), (10, 1)));
        assert_eq!((joined.stamp.pts_us, joined.stamp.arrived), (10, t.at(3.0)));
        assert!(!stitch.waiting());
        assert_eq!(stitch.settle(t.at(40.0)).map(|j| j.top), None, "shown once");
        assert_eq!(stitch.tears, 0);
    }

    /// A stripe coded alone (a refresh, a refinement) goes up at once beside the other's
    /// previous picture, and that is no tear.
    #[test]
    fn a_stripe_coded_alone_goes_up_beside_the_others_picture() {
        let t = Clock(Instant::now());
        let mut stitch = Stitch::default();
        let _first = decode(&mut stitch, 0, 10, (BOTH, 1), t.at(0.0));
        assert!(decode(&mut stitch, 1, 10, (BOTH, 1), t.at(1.0)).is_some());
        assert_eq!(decode(&mut stitch, 1, 20, (0b10, 1), t.at(2.0)), Some(((10, 0), (20, 1))));
        assert_eq!(stitch.tears, 0);
    }

    /// A stripe late past a display refresh: the capture goes up with its previous picture,
    /// counted as a tear, and the late one goes up when it comes.
    #[test]
    fn a_late_stripe_leaves_the_capture_up_with_its_previous_picture() {
        let t = Clock(Instant::now());
        let mut stitch = Stitch::default();
        let _first = decode(&mut stitch, 0, 10, (BOTH, 1), t.at(0.0));
        let _both = decode(&mut stitch, 1, 10, (BOTH, 1), t.at(0.0));
        assert_eq!(decode(&mut stitch, 0, 20, (BOTH, 1), t.at(10.0)), None);
        assert!(stitch.settle(t.at(26.0)).is_none(), "within the wait");
        let torn = stitch.settle(t.at(27.0)).map(|j| (j.top, j.lower, j.stamp.pts_us));
        assert_eq!(torn, Some(((20, 0), (10, 1), 20)));
        assert_eq!(stitch.tears, 1);
        assert_eq!(decode(&mut stitch, 1, 20, (BOTH, 1), t.at(30.0)), Some(((20, 0), (20, 1))));
        assert_eq!(stitch.tears, 1);
    }

    /// A stripe waiting for a refresh holds nothing up: the other goes on at once beside its
    /// last picture, each a tear.
    #[test]
    fn a_stalled_stripe_holds_nothing_up() {
        let t = Clock(Instant::now());
        let mut stitch = Stitch::default();
        let _first = decode(&mut stitch, 0, 10, (BOTH, 1), t.at(0.0));
        let _both = decode(&mut stitch, 1, 10, (BOTH, 1), t.at(0.0));
        stitch.set_stalled(1, true);
        assert_eq!(decode(&mut stitch, 0, 20, (BOTH, 1), t.at(8.0)), Some(((20, 0), (10, 1))));
        assert_eq!(decode(&mut stitch, 0, 30, (BOTH, 1), t.at(16.0)), Some(((30, 0), (10, 1))));
        assert_eq!(stitch.tears, 2);
        stitch.set_stalled(1, false);
        assert_eq!(decode(&mut stitch, 1, 40, (0b10, 1), t.at(20.0)), Some(((30, 0), (40, 1))));
    }

    /// A stripe running a capture behind the other on a 120 Hz beat neither restarts the wait
    /// nor takes it over: a newer capture replaces the one waited on and keeps its clock, an
    /// older stripe that comes out after it is kept as that stripe's picture, and a picture
    /// goes up at least once a wait.
    #[test]
    fn a_stripe_running_behind_never_starves_the_picture() {
        let t = Clock(Instant::now());
        let mut stitch = Stitch::default();
        let _first = decode(&mut stitch, 0, 0, (BOTH, 1), t.at(0.0));
        let _both = decode(&mut stitch, 1, 0, (BOTH, 1), t.at(0.0));
        // What went up, and when: each step's picture, stamped with the step's time.
        let mut shown: Vec<(f64, (Picture, Picture))> = vec![(0.0, ((0, 0), (0, 1)))];
        let mut at = 0.0;
        for capture in 1..=24_u64 {
            at += 8.333;
            let top = decode(&mut stitch, 0, capture * 10, (BOTH, 1), t.at(at));
            // The lower stripe of the capture before, a beat late.
            let lower = decode(&mut stitch, 1, (capture - 1) * 10, (BOTH, 1), t.at(at + 1.0));
            let late = stitch.settle(t.at(at + 2.0)).map(|j| (j.top, j.lower));
            shown.extend(top.map(|p| (at, p)));
            shown.extend(lower.map(|p| (at + 1.0, p)));
            shown.extend(late.map(|p| (at + 2.0, p)));
        }
        // The wait runs from the first stripe not shown, so a picture goes up at most a wait
        // and a beat after the last, however far the lower stripe runs behind.
        let gaps: Vec<f64> = shown.windows(2).map(|w| w[1].0 - w[0].0).collect();
        assert!(gaps.iter().all(|&gap| gap <= 16.667 + 8.333 + 2.0), "{gaps:?}: {shown:?}");
        assert!(shown.len() >= 8, "{shown:?}");
        let shown: Vec<(Picture, Picture)> = shown.into_iter().skip(1).map(|(_, p)| p).collect();
        assert!(
            shown.iter().all(|(top, lower)| top.0 >= lower.0 && top.0 - lower.0 <= 20),
            "the newest top beside the lower that came: {shown:?}"
        );
        assert!(shown.windows(2).all(|w| w[0].0.0 < w[1].0.0), "captures go forward: {shown:?}");
    }

    /// Stripes of two builds never go up together, not even past the wait: a resize codes
    /// both anew at another size, and a stripe of each would be a picture of neither.
    #[test]
    fn stripes_of_two_builds_never_go_up_together() {
        let t = Clock(Instant::now());
        let mut stitch = Stitch::default();
        let _first = decode(&mut stitch, 0, 10, (BOTH, 1), t.at(0.0));
        let _both = decode(&mut stitch, 1, 10, (BOTH, 1), t.at(0.0));
        assert_eq!(decode(&mut stitch, 0, 20, (BOTH, 2), t.at(8.0)), None);
        assert!(stitch.settle(t.at(40.0)).is_none(), "the lower is of the old build");
        stitch.set_stalled(1, true);
        assert_eq!(decode(&mut stitch, 0, 30, (BOTH, 2), t.at(48.0)), None, "stalled or not");
        stitch.set_stalled(1, false);
        assert_eq!(decode(&mut stitch, 1, 40, (0b10, 2), t.at(60.0)), Some(((30, 0), (40, 1))));
    }

    /// Stripes of two regions of one build never go up together: a region that moves at the
    /// size in force keeps the build, and a stripe of each would draw one place's rows at the
    /// other's. A joined picture says the region its capture shows.
    #[test]
    fn stripes_of_two_regions_never_go_up_together() {
        let t = Clock(Instant::now());
        let (a, b) = (
            Some(Region { x: 0, y: 0, w: 3840, h: 2160 }),
            Some(Region { x: 640, y: 360, w: 3840, h: 2160 }),
        );
        let mut stitch = Stitch::default();
        let mut decode_in = |index, pts, stripes, region, at| {
            let coded = Coded { stripes, build: 1, region };
            stitch.decoded(index, (pts, index), stamp(pts, at), coded, at)
        };
        assert!(decode_in(0, 10, BOTH, a, t.at(0.0)).is_none());
        let joined = decode_in(1, 10, BOTH, a, t.at(1.0)).expect("both of a");
        assert_eq!(joined.region, a);
        assert!(decode_in(0, 20, BOTH, b, t.at(8.0)).is_none(), "the lower is of a");
        assert!(stitch.settle(t.at(40.0)).is_none(), "not even past the wait");
        let joined = stitch.decoded(
            1,
            (20, 1),
            stamp(20, t.at(41.0)),
            Coded { stripes: BOTH, build: 1, region: b },
            t.at(41.0),
        );
        let joined = joined.expect("both of b");
        assert_eq!((joined.top, joined.lower, joined.region), ((20, 0), (20, 1), b));
    }

    /// A picture of the whole stream ends the stripes: nothing of them is kept.
    #[test]
    fn a_whole_picture_forgets_the_stripes() {
        let t = Clock(Instant::now());
        let mut stitch = Stitch::default();
        let _first = decode(&mut stitch, 0, 10, (BOTH, 1), t.at(0.0));
        let _both = decode(&mut stitch, 1, 10, (BOTH, 1), t.at(0.0));
        let _torn = decode(&mut stitch, 0, 20, (BOTH, 1), t.at(1.0));
        stitch.tears = 3;
        stitch.whole();
        assert!(!stitch.waiting());
        assert_eq!(stitch.tears, 3, "the count is the stream's");
        assert_eq!(decode(&mut stitch, 1, 30, (0b10, 1), t.at(2.0)), None, "no top to join");
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
        let decoded = decode_feedback(&bytes).unwrap();
        assert_eq!(decoded, Feedback::Nack { stream: StreamId(3), frame: 9, fragments: vec![] });
    }

    #[test]
    fn the_observed_parity_is_the_shard_ratio_in_thousandths() {
        let stats = |data: u64, parity: u64| ReassemblerStats {
            data_shards: data,
            parity_shards: parity,
            ..ReassemblerStats::default()
        };
        assert_eq!(observed_parity(&stats(0, 0)), 0, "nothing seen yet");
        assert_eq!(observed_parity(&stats(10, 2)), 200);
        assert_eq!(observed_parity(&stats(3, 1)), 333, "rounds down");
        assert_eq!(observed_parity(&stats(1, 100)), u16::MAX, "a silly ratio saturates");
    }

    #[test]
    fn a_short_one_round_trips() {
        let nack = Feedback::Nack { stream: StreamId(3), frame: 9, fragments: vec![1, 4] };
        let bytes = encode_feedback(nack.clone());
        let decoded = decode_feedback(&bytes).unwrap();
        assert_eq!(decoded, nack);
    }
}

#[cfg(test)]
mod tests {
    use super::arrival_tests::parked;
    use super::*;

    /// A datagram for `stream`, frame `frame`: the media channel byte, the rest of a
    /// little-endian header and one payload byte, the shape `MediaHeader::parse` reads.
    fn datagram(stream: u32, frame: u32) -> Bytes {
        let mut d = Vec::with_capacity(slopty_proto::media::HEADER_BYTES + 1);
        d.push(slopty_proto::datagram::Channel::Media as u8);
        d.extend_from_slice(&stream.to_le_bytes());
        d.extend_from_slice(&frame.to_le_bytes());
        d.extend_from_slice(&[0_u8; 8]);
        d.push(0xee);
        Bytes::from(d)
    }

    fn frame_of(arrival: &Arrival) -> u32 {
        MediaHeader::parse(&arrival.1).map_or(u32::MAX, |(h, _)| h.frame.get())
    }

    /// A read's worth of datagrams is routed in one call: each stream gets its own, in the
    /// order they came, a stream not attached yet keeps them as its backlog, and one that does
    /// not parse is dropped without holding up the rest.
    #[test]
    fn a_batch_fans_out_in_order_and_skips_what_does_not_parse() {
        let router = ScreenRouter::with_loss(0);
        let mut first = router.attach(StreamId(1));
        let now = Instant::now();
        let batch = [
            datagram(1, 10),
            datagram(2, 20),
            Bytes::from_static(b"short"),
            datagram(1, 11),
            datagram(2, 21),
            datagram(1, 12),
        ];
        router.route_many(batch, now);
        let taken = |rx: &mut mpsc::Receiver<Arrival>| {
            std::iter::from_fn(|| rx.try_recv().ok()).map(|a| (a.0, frame_of(&a))).collect()
        };
        let got: Vec<(Instant, u32)> = taken(&mut first);
        assert_eq!(got, [(now, 10), (now, 11), (now, 12)]);
        let mut second = router.attach(StreamId(2));
        let got: Vec<(Instant, u32)> = taken(&mut second);
        assert_eq!(got, [(now, 20), (now, 21)], "backlogged until attached, in order");
    }

    #[test]
    fn a_stream_backlogs_until_attached_and_the_backlog_keeps_the_newest() {
        let router = ScreenRouter::with_loss(0);
        let now = Instant::now();
        let sent = u32::try_from(PENDING_DEPTH).unwrap_or(u32::MAX).saturating_add(100);
        for frame in 0..sent {
            router.route(datagram(7, frame), now);
        }
        let Attached { datagrams: mut rx, dropped } = router.attach_counted(StreamId(7));
        let first = rx.try_recv().ok();
        assert_eq!(first.as_ref().map(frame_of), Some(100), "the oldest 100 were dropped");
        assert_eq!(dropped.load(Ordering::Relaxed), 100, "and counted");
        let mut got = 1;
        while rx.try_recv().is_ok() {
            got += 1;
        }
        assert_eq!(got, PENDING_DEPTH);
        // Attached now: a datagram goes straight through.
        router.route(datagram(7, 9_999), now);
        assert_eq!(rx.try_recv().ok().as_ref().map(frame_of), Some(9_999));
    }

    /// A stream whose task is a whole queue behind loses what does not fit, and the count says
    /// how much: the reassembler cannot, for a frame that went whole.
    #[test]
    fn what_a_full_queue_drops_is_counted() {
        let router = ScreenRouter::with_loss(0);
        let now = Instant::now();
        let Attached { datagrams: mut rx, dropped } = router.attach_counted(StreamId(3));
        let sent = u32::try_from(STREAM_DEPTH).unwrap_or(u32::MAX).saturating_add(5);
        for frame in 0..sent {
            router.route(datagram(3, frame), now);
        }
        assert_eq!(dropped.load(Ordering::Relaxed), 5);
        let mut got = 0;
        while rx.try_recv().is_ok() {
            got += 1;
        }
        assert_eq!(got, STREAM_DEPTH, "the queue kept the oldest");
        router.route(datagram(3, sent), now);
        assert_eq!(rx.try_recv().ok().as_ref().map(frame_of), Some(sent), "room again");
        assert_eq!(dropped.load(Ordering::Relaxed), 5);
    }

    /// A detached stream keeps nothing: the datagrams still in flight after its view let go
    /// are dropped, not backlogged for an attach that will not come. The tombstones are
    /// bounded, and an attach of the same id (a new connection counting from one again) lifts
    /// its tombstone.
    #[test]
    fn a_detached_stream_drops_what_arrives_late_and_the_tombstones_are_bounded() {
        let router = ScreenRouter::with_loss(0);
        let now = Instant::now();
        let mut rx = router.attach(StreamId(1));
        router.route(datagram(1, 1), now);
        assert_eq!(rx.try_recv().ok().as_ref().map(frame_of), Some(1));
        router.detach(StreamId(1));
        for frame in 2..600 {
            router.route(datagram(1, frame), now);
        }
        assert!(rx.try_recv().is_err(), "detached: nothing delivered");
        assert!(router.inner.lock().pending.is_empty(), "and nothing kept for it");

        let mut again = router.attach(StreamId(1));
        router.route(datagram(1, 700), now);
        assert_eq!(again.try_recv().ok().as_ref().map(frame_of), Some(700), "a new attach");
        router.detach(StreamId(1));

        let tombstones = u32::try_from(TOMBSTONES).unwrap_or(u32::MAX);
        for id in 10..10 + tombstones * 2 {
            drop(router.attach(StreamId(id)));
            router.detach(StreamId(id));
        }
        assert_eq!(router.inner.lock().detached.len(), TOMBSTONES);

        router.route(Bytes::from_static(&[1, 2, 3]), now);
        assert!(router.inner.lock().pending.is_empty(), "too short for a header");
    }

    /// Streams nobody attaches (an `Opened` the client did not ask for, closed at once) cannot
    /// pile up backlogs: past [`PENDING_STREAMS`], the one heard from longest ago goes.
    #[test]
    fn unattached_backlogs_are_bounded_in_number() {
        let router = ScreenRouter::with_loss(0);
        let t0 = Instant::now();
        let streams = u32::try_from(PENDING_STREAMS).unwrap_or(u32::MAX);
        for (n, id) in (100..100 + streams + 3).enumerate() {
            let at = t0 + Duration::from_millis(u64::try_from(n).unwrap_or(0));
            router.route(datagram(id, 1), at);
        }
        let waiting: Vec<StreamId> = router.inner.lock().pending.keys().copied().collect();
        assert_eq!(waiting.len(), PENDING_STREAMS);
        assert!(!waiting.contains(&StreamId(100)), "the stalest went first");
        assert!(waiting.contains(&StreamId(100 + streams + 2)), "the newest stays");
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
        let mut arrivals = Inflight::default();
        let t0 = Instant::now();
        let t = |n: u64| t0 + Duration::from_micros(n);
        arrivals.park(parked(10, t(1), None, false, false));
        arrivals.park(parked(20, t(2), None, false, false));
        arrivals.park(parked(30, t(3), None, false, false));
        assert_eq!(arrivals.decoded(20), Some((t(2), Coded::default())));
        assert_eq!(arrivals.decoded(10), None, "older than the one taken: forgotten with it");
        assert_eq!(arrivals.decoded(30), Some((t(3), Coded::default())));
        assert!(arrivals.parked.is_empty());
        for n in 0..u64::try_from(ARRIVALS).unwrap_or(u64::MAX).saturating_add(5) {
            arrivals.park(parked(n, t(n), None, false, false));
        }
        assert_eq!(arrivals.parked.len(), ARRIVALS, "bounded");
        assert_eq!(arrivals.decoded(0), None, "the oldest were dropped to make room");
    }
}

#[cfg(test)]
mod worker_tests {
    use std::sync::atomic::AtomicBool;

    use slopty_media::{EncodedFrame, Packetizer, cursor_datagram};

    use super::*;
    use crate::pacing::ClockAnchor;

    const STREAM: StreamId = StreamId(5);

    /// Streams whose decode threads hold every submission until a test lets them go, as
    /// VideoToolbox held one on a hosted virtual Mac.
    pub(super) mod hold {
        use parking_lot::{Condvar, Mutex};
        use slopty_core::StreamId;

        static HELD: Mutex<Vec<StreamId>> = Mutex::new(Vec::new());
        static LET_GO: Condvar = Condvar::new();

        pub(in super::super) fn close(stream: StreamId) {
            HELD.lock().push(stream);
        }

        pub(in super::super) fn open(stream: StreamId) {
            HELD.lock().retain(|held| *held != stream);
            LET_GO.notify_all();
        }

        /// Called by a decode thread before each submission.
        pub(in super::super) fn wait(stream: StreamId) {
            let mut held = HELD.lock();
            while held.contains(&stream) {
                LET_GO.wait(&mut held);
            }
        }
    }

    /// Seconds to wait on the system frameworks (`VideoToolbox`'s decoder, `CoreAudio` opening a
    /// player): nothing here measures them, and on a machine busy with a parallel test run
    /// they take tens of seconds (`docs/decisions/testing.md`). A bound only so a stuck
    /// framework fails with the stats rather than at the harness's kill.
    const FOR_THE_MACHINE: u64 = 100;

    /// A worker on its own runtime, with what it sent back caught for inspection.
    struct Harness {
        rt: tokio::runtime::Runtime,
        router: ScreenRouter,
        handle: ScreenHandle,
        control: mpsc::Receiver<ClientMsg>,
        stream: StreamId,
        feedback: Arc<Mutex<Vec<Feedback>>>,
        /// When each NACK left, by frame.
        nacked: Arc<Mutex<Vec<(u32, Instant)>>>,
        /// The connection is up; `false` makes the next feedback send fail.
        alive: Arc<AtomicBool>,
        /// Datagrams handed to the router, to know when the worker has seen them all.
        routed: Arc<AtomicU64>,
        /// Every token the reports acknowledged.
        acked: Vec<u64>,
    }

    impl Harness {
        fn start() -> Self {
            Self::start_on(STREAM)
        }

        fn start_on(stream: StreamId) -> Self {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let router = ScreenRouter::with_loss(0);
            let (control_tx, control) = mpsc::channel(64);
            let feedback = Arc::new(Mutex::new(Vec::new()));
            let nacked = Arc::new(Mutex::new(Vec::new()));
            let alive = Arc::new(AtomicBool::new(true));
            let caught = Arc::clone(&feedback);
            let stamped = Arc::clone(&nacked);
            let up = Arc::clone(&alive);
            let uplink = Uplink {
                control: control_tx,
                feedback: Box::new(move |bytes| {
                    let at = Instant::now();
                    if let Some(fb) = decode_feedback(&bytes) {
                        if let Feedback::Nack { frame, .. } = fb {
                            stamped.lock().push((frame, at));
                        }
                        caught.lock().push(fb);
                    }
                    up.load(Ordering::Relaxed)
                }),
                rtt: Box::new(|| Some(Duration::from_millis(10))),
            };
            let handle = spawn_screen(rt.handle(), &router, stream, VideoCodec::Hevc, uplink);
            Self {
                rt,
                router,
                handle,
                control,
                stream,
                feedback,
                nacked,
                alive,
                routed: Arc::default(),
                acked: Vec::new(),
            }
        }

        /// Poll `done` every few milliseconds for up to `secs`, draining the reports the worker
        /// sends meanwhile.
        fn wait_for(&mut self, what: &str, secs: u64, mut done: impl FnMut(&ScreenHandle) -> bool) {
            let deadline = Instant::now().checked_add(Duration::from_secs(secs)).unwrap();
            self.rt.block_on(async {
                loop {
                    while let Ok(msg) = self.control.try_recv() {
                        let ClientMsg::Screen(ScreenRequest::Report { stream, report }) = msg
                        else {
                            panic!("{msg:?}");
                        };
                        assert_eq!(stream, self.stream);
                        let len = usize::from(report.acked_ltr_len);
                        self.acked.extend_from_slice(&report.acked_ltr[..len]);
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

    /// A 3000-byte HEVC-shaped access unit with no parameter sets, one unit behind its length:
    /// the decoder rejects it, which is what a unit test can see of "the frame reached the
    /// decoder".
    fn frame_bytes() -> Vec<u8> {
        let mut data = 2996_u32.to_be_bytes().to_vec();
        data.extend([0x02, 0x01]);
        data.resize(3000, 0xaa);
        data
    }

    fn packetize(packetizer: &mut Packetizer, keyframe: bool, capture_ts_us: u32) -> Vec<Bytes> {
        packetize_ltr(packetizer, keyframe, None, capture_ts_us)
    }

    fn packetize_ltr(
        packetizer: &mut Packetizer,
        keyframe: bool,
        ltr_token: Option<u64>,
        capture_ts_us: u32,
    ) -> Vec<Bytes> {
        let data = frame_bytes();
        let frame = EncodedFrame {
            data: &data,
            keyframe,
            ltr_token,
            ltr_refresh: false,
            discardable: false,
            capture_ts_us,
            stripes: 0,
            region: None,
        };
        packetizer.packetize(&frame, 0, |_| {}).unwrap().datagrams.clone()
    }

    /// A frame the decoder rejects asks the worker for a refresh at once and holds back what
    /// was predicted from it, and its long-term reference is never acknowledged: submitted is
    /// not decoded.
    #[test]
    fn a_rejected_frame_asks_for_a_refresh_and_acknowledges_nothing() {
        let mut h = Harness::start();
        let mut packetizer = Packetizer::new(STREAM);
        packetizer.set_parity_permille(0);
        for d in packetize_ltr(&mut packetizer, true, Some(7), 1_000) {
            h.route(d);
        }
        // The decoder counts the rejection and then the refresh it asks for: wait for both.
        h.wait_for("the rejection and its refresh", FOR_THE_MACHINE, |handle| {
            let stats = handle.stats();
            stats.decode_errors == 1 && stats.refreshes >= 1
        });
        for d in packetize_ltr(&mut packetizer, false, Some(8), 2_000) {
            h.route(d);
        }
        let settled = h.settle();
        assert_eq!(settled.frames, 1, "the P-frame waits for a picture it can be decoded from");
        let feedback = Arc::clone(&h.feedback);
        assert!(
            feedback.lock().iter().any(|fb| matches!(fb, Feedback::Refresh { .. })),
            "a refresh went out"
        );
        h.wait_for("a few reports", 3, |handle| {
            handle.stats().first_frame_at.is_some_and(|t| t.elapsed() > REPORT_EVERY * 4)
        });
        assert!(h.acked.is_empty(), "nothing decoded, nothing acknowledged: {:?}", h.acked);
    }

    /// A submission that does not return holds up neither the stream nor its counters: the
    /// worker goes on reading and reporting, counts what waits on the decoder, drops what does
    /// not fit in its queue without asking for refreshes it could not decode either, gives the
    /// decoder up after [`DECODE_STUCK`], and asks for a keyframe for the one it starts. What
    /// the one given up on returns once it is let go is not counted.
    #[test]
    fn a_decoder_stuck_in_a_submission_is_replaced_and_the_stream_runs_on() {
        const HELD: StreamId = StreamId(0x51);
        const OVER: u64 = 4;
        hold::close(HELD);
        let mut h = Harness::start_on(HELD);
        let mut packetizer = Packetizer::new(HELD);
        packetizer.set_parity_permille(0);
        for d in packetize(&mut packetizer, true, 1_000) {
            h.route(d);
        }
        h.wait_for("the keyframe inside the decoder", FOR_THE_MACHINE, |handle| {
            handle.stats().decoding == 1
        });
        let held_at = Instant::now();
        let refreshes = h.handle.stats().refreshes;
        let queue = u64::try_from(DECODE_QUEUE).unwrap_or(u64::MAX);
        for n in 0..queue + OVER {
            let at = u32::try_from(n).unwrap_or(0).saturating_mul(16_667).saturating_add(2_000);
            for d in packetize(&mut packetizer, false, at) {
                h.route(d);
            }
        }
        let settled = h.settle();
        assert_eq!(settled.frames, 1 + queue + OVER, "every frame read: {settled:?}");
        assert_eq!(settled.decoding, 1 + queue, "in hand and queued: {settled:?}");
        assert_eq!(settled.decode_errors, OVER, "what did not fit is dropped: {settled:?}");
        assert_eq!(settled.refreshes, refreshes, "and nothing asked for: {settled:?}");
        assert_eq!(settled.decoders_replaced, 0, "{settled:?}");
        let reported = settled.reported_at;
        h.wait_for("a report after the hold", 3, |handle| handle.stats().reported_at > reported);
        h.wait_for("the decoder given up on", FOR_THE_MACHINE, |handle| {
            handle.stats().decoders_replaced == 1
        });
        let given_up = held_at.elapsed();
        assert!(given_up >= DECODE_STUCK.saturating_sub(REPORT_EVERY), "after {given_up:?}");
        let stats = h.handle.stats();
        assert_eq!(stats.decoding, 0, "what the old decoder held is forgotten: {stats:?}");
        let feedback = Arc::clone(&h.feedback);
        h.wait_for("a keyframe asked for the new decoder", 3, |_| {
            feedback.lock().iter().any(|fb| {
                matches!(fb, Feedback::Refresh { stream, keyframe: true, .. } if *stream == HELD)
            })
        });
        hold::open(HELD);
        for d in packetize(&mut packetizer, true, 3_000_000) {
            h.route(d);
        }
        h.wait_for("the new decoder's answer", FOR_THE_MACHINE, |handle| {
            handle.stats().decode_errors > OVER
        });
        h.wait_for("a report with nothing in the decoder", 3, |handle| {
            handle.stats().reported_at.is_some_and(|at| at.elapsed() < REPORT_EVERY)
                && handle.stats().decoding == 0
        });
        let stats = h.settle();
        assert_eq!(stats.decode_errors, OVER + 1, "only the new decoder's refusal: {stats:?}");
        assert_eq!(stats.decoders_replaced, 1, "{stats:?}");
    }

    /// How late a lost tail fragment is asked for: each NACK's send against the instant the
    /// reassembler's own delay makes it due, the last fragment's arrival plus the NACK delay for
    /// the harness's 10 ms round trip. Only silence waits on the loop's timer, and a lost tail is
    /// exactly that silence. A measurement, run by hand (MEASUREMENTS.md, "the reassembler's 2 ms
    /// tick stays").
    #[test]
    #[ignore = "measurement: prints the NACK's lateness"]
    #[expect(clippy::cast_precision_loss, reason = "microseconds printed as milliseconds")]
    fn tail_loss_nack_lateness() {
        const TRIALS: u32 = 200;
        let mut h = Harness::start();
        let mut packetizer = Packetizer::new(STREAM);
        packetizer.set_parity_permille(0);
        let delay = Config::default().nack_delay.for_rtt(Duration::from_millis(10));
        let mut late_us: Vec<u64> = Vec::new();
        for trial in 0..TRIALS {
            let datagrams = packetize(&mut packetizer, true, trial.saturating_add(1));
            let (tail, body) = datagrams.split_last().unwrap();
            for d in body {
                h.route(d.clone());
            }
            let due = Instant::now().checked_add(delay).unwrap();
            let nacked = Arc::clone(&h.nacked);
            h.wait_for("the nack", 3, |_handle| nacked.lock().iter().any(|(f, _)| *f == trial));
            let at = nacked.lock().iter().find(|(f, _)| *f == trial).map(|(_, at)| *at).unwrap();
            late_us.push(
                u64::try_from(at.saturating_duration_since(due).as_micros()).unwrap_or(u64::MAX),
            );
            h.route(tail.clone());
            h.wait_for("the frame", FOR_THE_MACHINE, |handle| {
                handle.stats().frames == u64::from(trial).saturating_add(1)
            });
        }
        late_us.sort_unstable();
        let at = |q: usize| late_us.get(late_us.len().saturating_sub(1).saturating_mul(q) / 100);
        let ms = |v: Option<&u64>| v.map_or(0.0, |&us| us as f64 / 1e3);
        eprintln!(
            "MEASURE nack lateness past last arrival + {delay:?}: p50 {:.2} / p90 {:.2} / p99 {:.2} / max {:.2} ms (n={})",
            ms(at(50)),
            ms(at(90)),
            ms(at(99)),
            ms(late_us.last()),
            late_us.len()
        );
    }

    /// What a frame costs the stream's worker between the router and the decoder, and how long
    /// after its last datagram was routed the worker has handed it to the decoder: 62 KB
    /// keyframes (51 datagrams) at 60 a second, each routed as the connection's reader routes
    /// what one read takes off the connection. The decoder rejects each, as everywhere here,
    /// and a rejected keyframe does not hold back the next. The hand-over is read off a cursor
    /// datagram routed behind each frame: the worker takes it only after the frame's last
    /// datagram went to the decoder. A measurement, run by hand: `docs/MEASUREMENTS.md`, "the
    /// client's datagram path".
    #[test]
    #[ignore = "a measurement; run with --run-ignored only --no-capture"]
    #[expect(clippy::cast_precision_loss, reason = "measurement arithmetic")]
    fn frame_path_cost() {
        const FRAMES: u32 = 1_200;
        /// Frames per round: the busy time is read per round, and the quietest round is the
        /// figure least disturbed by the rest of the machine.
        const ROUND: u32 = 60;
        const EVERY: Duration = Duration::from_micros(16_667);
        let h = Harness::start();
        let data: Vec<u8> = {
            let mut data = frame_bytes();
            data.resize(62_000, 0xaa);
            data
        };
        let mut packetizer = Packetizer::new(STREAM);
        packetizer.set_parity_permille(0);
        let metrics = h.rt.metrics();
        let workers = metrics.num_workers();
        let busy = || (0..workers).map(|w| metrics.worker_total_busy_duration(w)).sum::<Duration>();
        let parks = || (0..workers).map(|w| metrics.worker_park_count(w)).sum::<u64>();
        let mut cursor = h.handle.cursor();
        let (busy_before, parks_before) = (busy(), parks());
        let (mut submitted, mut routing, mut missed) = (Vec::new(), Duration::ZERO, 0_u32);
        let (mut rounds, mut round_busy) = (Vec::new(), busy_before);
        for n in 0..FRAMES {
            if n % ROUND == 0 && n > 0 {
                let now = busy();
                rounds.push(now.saturating_sub(round_busy).as_secs_f64() * 1e6 / f64::from(ROUND));
                round_busy = now;
            }
            let started = Instant::now();
            let frame = EncodedFrame {
                data: &data,
                keyframe: true,
                ltr_token: None,
                ltr_refresh: false,
                discardable: false,
                capture_ts_us: n.saturating_mul(16_667),
                stripes: 0,
                region: None,
            };
            let datagrams = packetizer.packetize(&frame, 0, |_| {}).unwrap().datagrams.clone();
            let x = i32::try_from(n).unwrap();
            let marker = cursor_datagram(STREAM, n.saturating_add(1), 0, x, 0, true);
            let routed = Instant::now();
            h.router.route_many(datagrams, routed);
            let handed = Instant::now();
            routing = routing.saturating_add(handed.saturating_duration_since(routed));
            h.router.route(marker, handed);
            let waited = async {
                while cursor.borrow_and_update().x != x {
                    if cursor.changed().await.is_err() {
                        return;
                    }
                }
            };
            let seen =
                h.rt.block_on(async { tokio::time::timeout(Duration::from_secs(1), waited).await });
            if seen.is_ok() {
                submitted.push(handed.elapsed().as_secs_f64() * 1e6);
            } else {
                missed = missed.saturating_add(1);
            }
            #[expect(clippy::disallowed_methods, reason = "the frame clock of a measurement")]
            std::thread::sleep(EVERY.saturating_sub(started.elapsed()));
        }
        let busy = busy().saturating_sub(busy_before);
        let parks = parks().saturating_sub(parks_before);
        let stats = h.handle.stats();
        submitted.sort_by(f64::total_cmp);
        rounds.sort_by(f64::total_cmp);
        let q = |p: usize| {
            submitted.get(submitted.len().saturating_sub(1).saturating_mul(p) / 100).copied()
        };
        let per = |d: Duration| d.as_secs_f64() * 1e6 / f64::from(FRAMES);
        eprintln!(
            "MEASURE frame path: {} frames at the decoder, {missed} missed; per frame: routing {:.1} µs on the caller, worker runtime busy {:.1} µs (rounds: quietest {:.1}, median {:.1}), {:.2} parks; last routed → at the decoder p50 {:.0} / p99 {:.0} / max {:.0} µs",
            stats.frames,
            per(routing),
            per(busy),
            rounds.first().copied().unwrap_or(0.0),
            rounds.get(rounds.len() / 2).copied().unwrap_or(0.0),
            parks as f64 / f64::from(FRAMES),
            q(50).unwrap_or(0.0),
            q(99).unwrap_or(0.0),
            submitted.last().copied().unwrap_or(0.0),
        );
    }

    /// What handing a frame to its lane's decode thread costs the frame: the time from the
    /// stream's task leaving it on the queue to the thread, parked since the last frame, having
    /// it in hand, at 60 frames a second. Beside it, the wake every frame already paid before
    /// the thread: a datagram handed to a stream task parked on a user-interactive runtime, as
    /// the router hands one over. A measurement, run by hand: `docs/MEASUREMENTS.md`, "a decode
    /// submission that never returned".
    #[test]
    #[ignore = "a measurement; run with --run-ignored only --no-capture"]
    fn decode_hop_cost() {
        const FRAMES: usize = 1_200;
        const EVERY: Duration = Duration::from_micros(16_667);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .on_thread_start(slopty_platform::user_interactive_thread)
            .build()
            .unwrap();
        let (submits, queue) = std::sync::mpsc::sync_channel::<Instant>(DECODE_QUEUE);
        let (took_tx, took) = std::sync::mpsc::channel::<Duration>();
        let to_task = took_tx.clone();
        let thread = std::thread::spawn(move || {
            slopty_platform::user_interactive_thread();
            while let Ok(sent) = queue.recv() {
                let _gone = took_tx.send(sent.elapsed());
            }
        });
        let (datagrams, mut arrivals) = mpsc::channel::<Instant>(STREAM_DEPTH);
        let task = rt.spawn(async move {
            while let Some(sent) = arrivals.recv().await {
                let _gone = to_task.send(sent.elapsed());
            }
        });
        let (mut hops, mut wakes) = (Vec::with_capacity(FRAMES), Vec::with_capacity(FRAMES));
        for _ in 0..FRAMES {
            let started = Instant::now();
            submits.try_send(Instant::now()).unwrap();
            hops.push(took.recv().unwrap().as_secs_f64() * 1e6);
            datagrams.try_send(Instant::now()).unwrap();
            wakes.push(took.recv().unwrap().as_secs_f64() * 1e6);
            #[expect(clippy::disallowed_methods, reason = "the frame clock of a measurement")]
            std::thread::sleep(EVERY.saturating_sub(started.elapsed()));
        }
        drop((submits, datagrams));
        thread.join().unwrap();
        rt.block_on(task).unwrap();
        for (what, samples) in [("decode thread hop", &mut hops), ("stream task wake", &mut wakes)]
        {
            samples.sort_by(f64::total_cmp);
            let q = |p: usize| {
                samples.get(samples.len().saturating_sub(1).saturating_mul(p) / 100).copied()
            };
            eprintln!(
                "MEASURE {what} over {FRAMES} frames at 60 a second: p50 {:.1} / p90 {:.1} / p99 {:.1} / max {:.1} µs",
                q(50).unwrap_or(0.0),
                q(90).unwrap_or(0.0),
                q(99).unwrap_or(0.0),
                samples.last().copied().unwrap_or(0.0),
            );
        }
    }

    /// What the worker's loop pays on each wake for its timer: a sleep made anew, registered
    /// and dropped unfired, as the loop did, against one sleep whose deadline is pushed later.
    /// A measurement, run by hand: `docs/MEASUREMENTS.md`, "the client's datagram path".
    #[test]
    #[ignore = "a measurement; run with --run-ignored only --no-capture"]
    fn wake_timer_cost() {
        const WAKES: u32 = 200_000;
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            for round in 0..3 {
                let started = Instant::now();
                for _ in 0..WAKES {
                    let tick = tokio::time::sleep(IDLE_TICK);
                    tokio::select! {
                        biased;
                        () = tick => {}
                        () = std::future::ready(()) => {}
                    }
                }
                let fresh = started.elapsed().checked_div(WAKES).unwrap_or_default();
                let tick = tokio::time::sleep(IDLE_TICK);
                tokio::pin!(tick);
                let started = Instant::now();
                for _ in 0..WAKES {
                    let now = tokio::time::Instant::now();
                    tick.as_mut().reset(now.checked_add(IDLE_TICK).unwrap_or(now));
                    tokio::select! {
                        biased;
                        () = &mut tick => {}
                        () = std::future::ready(()) => {}
                    }
                }
                let moved = started.elapsed().checked_div(WAKES).unwrap_or_default();
                eprintln!(
                    "MEASURE timer per wake, round {round}: made anew {fresh:?}, moved {moved:?}"
                );
            }
        });
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
        h.wait_for("the frame", FOR_THE_MACHINE, |handle| {
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

        // The worker's source hint reaches the worker.
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

    /// A worker has one sound: its three tiles' streams share it, so a mute in one is the mute
    /// in all, and a tile opened later reads it. The choice outlives the sound itself: a tile
    /// opened after the last one closed starts a new one, still silenced.
    #[test]
    fn a_workers_streams_share_one_sound_and_its_mute() {
        let h = Harness::start();
        let open = |stream| {
            let (control, _gone) = mpsc::channel(4);
            let uplink =
                Uplink { control, feedback: Box::new(|_bytes| true), rtt: Box::new(|| None) };
            spawn_screen(h.rt.handle(), &h.router, stream, VideoCodec::Hevc, uplink)
        };
        let (second, third) = (open(StreamId(7)), open(StreamId(9)));
        assert!(Arc::ptr_eq(&h.handle.sound, &second.sound));
        assert!(Arc::ptr_eq(&h.handle.sound, &third.sound));
        assert!(!h.handle.muted(), "sound plays until the pill silences it");
        third.set_muted(true);
        assert!(h.handle.muted() && second.muted(), "one tile silences the worker's sound");
        let Harness { handle, router, rt, .. } = h;
        drop((handle, second, third));
        assert!(router.sound.lock().upgrade().is_none(), "the last stream ends the sound");
        assert!(router.inner.lock().attached.is_empty(), "and lets go of its lane");
        let (control, _gone) = mpsc::channel(4);
        let uplink = Uplink { control, feedback: Box::new(|_bytes| true), rtt: Box::new(|| None) };
        let again = spawn_screen(rt.handle(), &router, StreamId(11), VideoCodec::Hevc, uplink);
        assert!(again.muted(), "a new sound keeps the connection's choice");
        again.set_muted(false);
        assert!(!again.muted());
    }

    /// The round trip is read off the connection once a report, not on every wake: the read
    /// takes the connection's lock, which its driver holds while it receives. Two hundred
    /// wakes, one per cursor datagram, read it no more often than the reports went out.
    #[test]
    fn the_round_trip_is_read_once_a_report() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let router = ScreenRouter::with_loss(0);
        let (control_tx, mut control) = mpsc::channel(256);
        let reads = Arc::new(AtomicU32::new(0));
        let counted = Arc::clone(&reads);
        let uplink = Uplink {
            control: control_tx,
            feedback: Box::new(|_bytes| true),
            rtt: Box::new(move || {
                counted.fetch_add(1, Ordering::Relaxed);
                Some(Duration::from_millis(10))
            }),
        };
        let handle = spawn_screen(rt.handle(), &router, STREAM, VideoCodec::Hevc, uplink);
        let mut cursor = handle.cursor();
        rt.block_on(async {
            for n in 0..200_u32 {
                let x = i32::try_from(n).unwrap();
                router.route(
                    cursor_datagram(STREAM, n.saturating_add(1), 0, x, 0, true),
                    Instant::now(),
                );
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            let last = async {
                while cursor.borrow_and_update().x != 199 {
                    cursor.changed().await.unwrap();
                }
            };
            tokio::time::timeout(Duration::from_secs(3), last).await.unwrap();
        });
        let reports =
            u32::try_from(std::iter::from_fn(|| control.try_recv().ok()).count()).unwrap();
        let reads = reads.load(Ordering::Relaxed);
        assert!(reports >= 2, "{reports} reports");
        // One read as the stream starts, and one for a report sent after the count.
        assert!(reads <= reports.saturating_add(2), "{reads} reads for {reports} reports");
    }

    /// A stream probes the worker's clock at once and then four times a second, and an echo
    /// places the worker's capture clock on this one: the stats carry the estimate, and the
    /// worker's clock reading 5 s ahead reads back as 5 s ahead within the round trip.
    #[test]
    fn clock_probes_go_out_and_an_echo_places_the_worker_clock() {
        use slopty_proto::media::ClockEcho;
        let mut h = Harness::start();
        let probes = |h: &Harness| {
            h.feedback
                .lock()
                .iter()
                .filter_map(|f| match f {
                    Feedback::Clock { stream, sent_us } => Some((*stream, *sent_us)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let seen = Arc::clone(&h.feedback);
        h.wait_for("the first probes", FOR_THE_MACHINE, |_| {
            seen.lock().iter().filter(|f| matches!(f, Feedback::Clock { .. })).count() >= 3
        });
        assert!(h.handle.stats().clock.is_none(), "no echo, no estimate");
        let (stream, sent_us) = probes(&h)[2];
        assert_eq!(stream, STREAM);
        // The worker's clock is this process's plus 5 s: `sent_us` is on the stream's own epoch,
        // so the worker reads the moment the probe left as its epoch-relative time plus that.
        let epoch_on_worker = 5_000_000 + sent_us;
        let echo = ClockEcho::new(sent_us, epoch_on_worker, epoch_on_worker + 30);
        // The echo comes back a round trip later, longer than the 30 µs it says the worker
        // held it: the report's timer can wake this thread within microseconds of the probe,
        // and an echo sooner than its own hold is one the estimate throws away.
        h.rt.block_on(async { tokio::time::sleep(Duration::from_millis(2)).await });
        let routed_at = Instant::now();
        h.route(echo.datagram(STREAM.0, 0));
        h.wait_for("the estimate", FOR_THE_MACHINE, |handle| handle.stats().clock.is_some());
        let estimate = h.handle.stats().clock.unwrap();
        assert!(estimate.bound <= estimate.rtt, "{estimate:?}");
        // The worker read `epoch_on_worker` as the probe left; the estimate puts that moment
        // between the echo's arrival less the round trip and the arrival.
        let left = estimate.anchor.captured(epoch_on_worker).unwrap();
        assert!(left <= routed_at, "{estimate:?}");
        assert!(left >= routed_at.checked_sub(estimate.rtt).unwrap(), "{estimate:?}");
    }

    /// What the clock probes add to the frame path, measured alone: the look at every datagram
    /// for an echo (a video fragment, which is not one), and the anchor read the decoder's
    /// callback makes for every picture. A measurement, run by hand: `docs/MEASUREMENTS.md`,
    /// "capture to glass on any link".
    #[test]
    #[ignore = "a measurement; run with --run-ignored only --no-capture"]
    #[expect(clippy::cast_precision_loss, reason = "measurement arithmetic")]
    fn clock_path_cost() {
        use std::hint::black_box;
        const N: u32 = 2_000_000;
        let mut packetizer = Packetizer::new(STREAM);
        let fragment = packetize(&mut packetizer, true, 1)[0].clone();
        let estimate = Mutex::new(Some(ClockEstimate {
            anchor: ClockAnchor { at: Instant::now(), host_us: 1_000_000 },
            bound: Duration::from_micros(150),
            rtt: Duration::from_micros(300),
            drift_ppm: 12,
        }));
        for round in 0..3 {
            let started = Instant::now();
            for _ in 0..N {
                let fragment = black_box(&fragment);
                let echo = MediaHeader::parse(fragment)
                    .is_some_and(|(header, _)| header.kind() == Some(Kind::Clock));
                black_box(echo);
            }
            let look = started.elapsed().as_nanos() as f64 / f64::from(N);
            let started = Instant::now();
            for pts in 0..u64::from(N) {
                let captured = black_box(&estimate).lock().and_then(|e| Captured::by(&e, pts));
                black_box(captured);
            }
            let read = started.elapsed().as_nanos() as f64 / f64::from(N);
            // The read before each picture carried its estimate's bound: the anchor alone.
            let anchor = Mutex::new(estimate.lock().map(|e| e.anchor));
            let started = Instant::now();
            for pts in 0..u64::from(N) {
                let captured = black_box(&anchor).lock().and_then(|a| a.captured(pts));
                black_box(captured);
            }
            let anchor_only = started.elapsed().as_nanos() as f64 / f64::from(N);
            eprintln!(
                "MEASURE clock path, round {round}: echo look per datagram {look:.1} ns, capture placed with its bound per picture {read:.1} ns (the anchor alone {anchor_only:.1} ns)"
            );
        }
    }

    /// A control channel nobody drains fills up; the worker drops its reports and goes on
    /// reassembling and publishing, instead of waiting for room.
    #[test]
    fn a_full_control_channel_does_not_stall_the_worker() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let router = ScreenRouter::with_loss(0);
        let (control_tx, _control) = mpsc::channel(1);
        let uplink = Uplink {
            control: control_tx,
            feedback: Box::new(|_bytes| true),
            rtt: Box::new(|| Some(Duration::from_millis(10))),
        };
        let handle = spawn_screen(rt.handle(), &router, STREAM, VideoCodec::Hevc, uplink);
        let mut packetizer = Packetizer::new(STREAM);
        packetizer.set_parity_permille(0);
        rt.block_on(async {
            // Two report periods: the one slot is taken and the next report finds it full.
            tokio::time::sleep(REPORT_EVERY.saturating_mul(3)).await;
            // Keyframes both: the decoder rejects these, and after a rejection only a keyframe
            // is let through.
            for (n, ts) in [(1_u64, 1_000_u32), (2, 2_000)] {
                for d in packetize(&mut packetizer, true, ts) {
                    router.route(d, Instant::now());
                }
                let deadline = Instant::now().checked_add(Duration::from_secs(3)).unwrap();
                while handle.stats().frames < n {
                    assert!(Instant::now() < deadline, "the worker stalled: {:?}", handle.stats());
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
        });
    }
}
