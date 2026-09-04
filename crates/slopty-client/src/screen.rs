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
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use bytes::Bytes;
use parking_lot::Mutex;
use slopty_codec::{DecodedFrame, Decoder};
use slopty_core::StreamId;
use slopty_media::{Action, Config, Ingest, Reassembler};
use slopty_proto::ClientMsg;
use slopty_proto::media::MediaHeader;
use slopty_proto::screen::{ScreenRequest, VideoCodec};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

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

/// Fans incoming datagrams out to per-stream queues.
#[derive(Clone, Debug, Default)]
pub struct ScreenRouter {
    inner: Arc<Mutex<Routes>>,
    /// Diagnostic loss injection: drop this many datagrams per thousand, from
    /// `SLOPTY_DROP_PERMILLE`. Zero in normal use.
    drop_permille: u32,
    lcg: Arc<std::sync::atomic::AtomicU64>,
}

#[derive(Debug, Default)]
struct Routes {
    attached: HashMap<StreamId, mpsc::Sender<Bytes>>,
    pending: HashMap<StreamId, VecDeque<Bytes>>,
}

impl Routes {
    fn deliver(&mut self, stream: StreamId, datagram: Bytes) {
        if let Some(tx) = self.attached.get(&stream) {
            // A full queue means the stream task is behind; dropping is the right call.
            let _dropped = tx.try_send(datagram);
            return;
        }
        let backlog = self.pending.entry(stream).or_default();
        if backlog.len() >= PENDING_DEPTH {
            backlog.pop_front();
        }
        backlog.push_back(datagram);
    }
}

impl ScreenRouter {
    /// Empty router. Reads `SLOPTY_DROP_PERMILLE` once for loss injection.
    #[must_use]
    pub fn new() -> Self {
        let drop_permille = std::env::var("SLOPTY_DROP_PERMILLE")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .map_or(0, |v| v.min(1000));
        if drop_permille > 0 {
            tracing::warn!(drop_permille, "media loss injection is on");
        }
        Self { drop_permille, ..Self::default() }
    }

    /// Whether loss injection says to drop this datagram (a 64-bit LCG, no `rand` dependency).
    fn inject_loss(&self) -> bool {
        if self.drop_permille == 0 {
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
        roll < self.drop_permille
    }

    /// Deliver one datagram (called by the connection's datagram reader).
    pub fn route(&self, datagram: Bytes) {
        if self.inject_loss() {
            return;
        }
        let Some((header, _payload)) = MediaHeader::parse(&datagram) else { return };
        let stream = StreamId(header.stream.get());
        self.inner.lock().deliver(stream, datagram);
    }

    /// Start receiving `stream`'s datagrams, backlog first.
    #[must_use]
    pub fn attach(&self, stream: StreamId) -> mpsc::Receiver<Bytes> {
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
    /// Frames given up on.
    pub frames_lost: u64,
    /// NACKs sent.
    pub nacks: u64,
    /// Refresh requests sent.
    pub refreshes: u64,
    /// Decoder rejections.
    pub decode_errors: u64,
    /// Datagrams seen.
    pub datagrams: u64,
}

/// A live client-side stream. Dropping it stops the task and unroutes the stream; the caller
/// still sends `ScreenRequest::Close` so the host stops capturing.
#[derive(Debug)]
pub struct ScreenHandle {
    stream: StreamId,
    frames: watch::Receiver<Option<Arc<DecodedFrame>>>,
    cursor: watch::Receiver<CursorState>,
    stats: watch::Receiver<ScreenStats>,
    router: ScreenRouter,
    task: JoinHandle<()>,
}

impl ScreenHandle {
    /// Stream id.
    #[must_use]
    pub const fn stream(&self) -> StreamId {
        self.stream
    }

    /// Newest decoded frame; `changed().await` wakes when a new one lands.
    #[must_use]
    pub fn frames(&self) -> watch::Receiver<Option<Arc<DecodedFrame>>> {
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
}

impl Drop for ScreenHandle {
    fn drop(&mut self) {
        self.task.abort();
        self.router.detach(self.stream);
    }
}

/// Start receiving `stream`: reassembly, decode, NACK/refresh/report traffic on `out`.
#[must_use]
pub fn spawn_screen(
    runtime: &tokio::runtime::Handle,
    router: &ScreenRouter,
    stream: StreamId,
    codec: VideoCodec,
    out: mpsc::Sender<ClientMsg>,
    rtt: impl Fn() -> Option<Duration> + Send + 'static,
) -> ScreenHandle {
    let datagrams = router.attach(stream);
    let (frames_tx, frames) = watch::channel(None);
    let (cursor_tx, cursor) = watch::channel(CursorState::default());
    let (stats_tx, stats) = watch::channel(ScreenStats::default());
    let decoder = Decoder::new(codec, move |frame| {
        let _no_receiver = frames_tx.send(Some(Arc::new(frame)));
    });
    let worker = Worker {
        stream,
        datagrams,
        reassembler: Reassembler::new(stream, Config::default(), Instant::now()),
        decoder,
        out,
        rtt: Box::new(rtt),
        cursor: cursor_tx,
        stats: stats_tx,
        cursor_seq: None,
        counters: ScreenStats::default(),
    };
    let task = runtime.spawn(worker.run());
    ScreenHandle { stream, frames, cursor, stats, router: router.clone(), task }
}

struct Worker {
    stream: StreamId,
    datagrams: mpsc::Receiver<Bytes>,
    reassembler: Reassembler,
    decoder: Decoder,
    out: mpsc::Sender<ClientMsg>,
    rtt: Box<dyn Fn() -> Option<Duration> + Send>,
    cursor: watch::Sender<CursorState>,
    stats: watch::Sender<ScreenStats>,
    cursor_seq: Option<u32>,
    counters: ScreenStats,
}

impl Worker {
    async fn run(mut self) {
        let mut report = tokio::time::interval(REPORT_EVERY);
        report.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let busy = self.reassembler.queue_depth() > 0 || self.reassembler.awaiting_refresh();
            let tick = tokio::time::sleep(if busy { TICK } else { IDLE_TICK });
            tokio::select! {
                datagram = self.datagrams.recv() => {
                    let Some(datagram) = datagram else { break };
                    self.ingest(&datagram);
                }
                () = tick => {}
                _instant = report.tick() => self.report().await,
            }
            if !self.actions().await {
                break;
            }
        }
        tracing::debug!(stream = %self.stream, "screen worker finished");
    }

    fn ingest(&mut self, datagram: &Bytes) {
        self.counters.datagrams = self.counters.datagrams.saturating_add(1);
        let now = Instant::now();
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
            Ingest::Audio { .. } | Ingest::Ignored(_) => {}
        }
    }

    /// Push every frame that is now complete into the decoder.
    fn deliver(&mut self) {
        while let Some(frame) = self.reassembler.next_frame() {
            self.counters.frames = self.counters.frames.saturating_add(1);
            let pts = u64::from(frame.info.capture_ts_us);
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
    async fn actions(&mut self) -> bool {
        let rtt = (self.rtt)().unwrap_or(DEFAULT_RTT);
        for action in self.reassembler.tick(Instant::now(), rtt) {
            let stream = self.stream;
            let req = match action {
                Action::Nack { frame, fragments } => {
                    self.counters.nacks = self.counters.nacks.saturating_add(1);
                    ScreenRequest::Nack { stream, frame, fragments }
                }
                Action::RequestRefresh { last_good_frame } => {
                    self.counters.refreshes = self.counters.refreshes.saturating_add(1);
                    ScreenRequest::RequestRefresh { stream, last_good_frame }
                }
            };
            if self.out.send(ClientMsg::Screen(req)).await.is_err() {
                return false;
            }
        }
        true
    }

    async fn report(&mut self) {
        let report = self.reassembler.take_report(0);
        let stats = self.reassembler.stats();
        self.counters.frames_fec = stats.frames_fec;
        self.counters.frames_lost = stats.frames_lost;
        self.stats.send_replace(self.counters);
        let stream = self.stream;
        let _gone =
            self.out.send(ClientMsg::Screen(ScreenRequest::Report { stream, report })).await;
    }
}
