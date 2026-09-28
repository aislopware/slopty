//! One connection to a worker, as a channel of events plus a queue of outbound messages.
//!
//! Runs on tokio; a UI on another executor just holds the receiver and the sender.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use parking_lot::Mutex;
use slopty_core::{SessionId, StreamId, XferId};
use slopty_net::client::WorkerConn;
use slopty_net::framed::FramedRecv;
use slopty_net::streams::{RawRecv, Uni, read_uni};
use slopty_net::{ClientMsg, NetError, WorkerMsg};
use slopty_proto::conversation::ConversationEvent;
use slopty_proto::datagram::{ClientDatagram, split_term_datagram};
use slopty_proto::file::INLINE_FILE_BYTES;
use slopty_proto::handshake::HelloAck;
#[cfg(target_vendor = "apple")]
use slopty_proto::screen::VideoCodec;
use slopty_proto::terminal::{Frame, TermEvent, TermRequest, frame_head};
use slopty_proto::transfer::{BulkHeader, Purpose};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::clip::{ClipCache, MAX_CLIP_BYTES};
use crate::remote::{LinkRemote, Remote};
#[cfg(target_vendor = "apple")]
use crate::screen::{ScreenHandle, ScreenRouter, Uplink as ScreenUplink, spawn_screen};
use crate::tunnel::{Forward, Forwards};
use crate::xfer::{Table, Uplink};

mod files;

/// Streamed file reads between their announcement and their text. An async lock, held while
/// the joined read is queued, so the reads of a path reach the owner in the order the control
/// stream announced them.
type FileJoin = Arc<tokio::sync::Mutex<files::Join>>;

/// Bounded queues: a client that cannot keep up sees backpressure, not unbounded memory.
const EVENT_DEPTH: usize = 4096;
const OUT_DEPTH: usize = 1024;
/// Frame copies waiting for their session's pump. One that finds it full is dropped: the
/// stream brings the frame regardless.
const COPY_DEPTH: usize = 16;

/// Everything the UI hears from one worker.
#[derive(Debug)]
pub enum LinkEvent {
    /// A control-stream message (session list changes, pongs, agent events, …).
    Control(WorkerMsg),
    /// A session-stream event.
    Term {
        /// Session.
        session: SessionId,
        /// Event.
        event: TermEvent,
    },
    /// The listening ports of a session changed; each is forwarded here.
    Ports {
        /// Session.
        session: SessionId,
        /// Its ports and where each is reachable here.
        forwards: Vec<Forward>,
    },
    /// An upload failed on this side (a file unreadable, the stream cut past resuming).
    XferFailed {
        /// The transfer.
        xfer: XferId,
        /// Why, for a person.
        error: String,
    },
    /// An event of a conversation this client follows, in the order the worker sent it. The
    /// stream ending (an unfollow, the session gone) sends nothing more.
    Conversation {
        /// The terminal session the agent runs in.
        session: SessionId,
        /// Event.
        event: ConversationEvent,
    },
    /// The connection is gone; `WorkerLink` is dead after this.
    Disconnected(String),
}

/// A live worker connection.
#[derive(Debug)]
pub struct WorkerLink {
    ack: HelloAck,
    out: mpsc::Sender<ClientMsg>,
    events: Option<mpsc::Receiver<LinkEvent>>,
    conn: slopty_net::Connection,
    #[cfg(target_vendor = "apple")]
    router: ScreenRouter,
    /// The runtime the link's tasks run on; screen workers join them from any thread.
    #[cfg(target_vendor = "apple")]
    runtime: tokio::runtime::Handle,
    remote: Arc<dyn Remote>,
    /// Closed with the link, so a paste waiting on it stops at once.
    clips: Arc<ClipCache>,
    /// Frames shown from their datagram copy, ahead of the session stream.
    copies_taken: Arc<AtomicU64>,
    tasks: JoinSet<()>,
}

impl WorkerLink {
    /// Wrap a connection: spawns the control reader, the session-stream acceptor and the writer.
    /// The worker's listening ports arrive as they are (`WorkerMsg::Ports`), forwarded nowhere.
    #[must_use]
    pub fn start(conn: WorkerConn) -> Self {
        Self::launch(conn, false)
    }

    /// [`Self::start`], and every port the worker's shells listen on is served on this
    /// machine's loopback ([`LinkEvent::Ports`] says where): the app's link.
    #[must_use]
    pub fn start_forwarding(conn: WorkerConn) -> Self {
        Self::launch(conn, true)
    }

    fn launch(conn: WorkerConn, forward: bool) -> Self {
        #[cfg(target_vendor = "apple")]
        warm_up_decoder();
        let WorkerConn { conn: quic, ack, mut tx, mut rx, .. } = conn;
        let (events_tx, events_rx) = mpsc::channel(EVENT_DEPTH);
        let (out_tx, mut out_rx) = mpsc::channel::<ClientMsg>(OUT_DEPTH);
        let mut tasks = JoinSet::new();

        let table = Arc::new(Table::default());
        let clips = Arc::new(ClipCache::new(out_tx.clone()));

        let file_join = FileJoin::default();
        let control_events = events_tx.clone();
        let (control_table, control_clips) = (Arc::clone(&table), Arc::clone(&clips));
        let control_files = Arc::clone(&file_join);
        let forwards = Arc::new(Mutex::new(Forwards::new(quic.clone())));
        let shared_forwards = forward.then(|| Arc::clone(&forwards));
        tasks.spawn(async move {
            loop {
                let msg = match rx.recv().await {
                    Ok(msg) => msg,
                    Err(e) => {
                        forwards.lock().clear();
                        control_clips.close();
                        let _sent =
                            control_events.send(LinkEvent::Disconnected(e.to_string())).await;
                        break;
                    }
                };
                tracing::trace!(kind = msg.kind(), "control message");
                let event = match msg {
                    WorkerMsg::Xfer(x) if control_table.on_control(&x) => continue,
                    WorkerMsg::Clip(c) if control_clips.on_control(&c) => continue,
                    WorkerMsg::File { path, read } => {
                        let take = |join: &mut files::Join| join.on_read(path, read);
                        if !hand_on_file(&control_files, &control_events, take).await {
                            break;
                        }
                        continue;
                    }
                    WorkerMsg::Ports { session, ports } if forward => {
                        let forwards = forwards.lock().update(session, ports);
                        LinkEvent::Ports { session, forwards }
                    }
                    msg => LinkEvent::Control(msg),
                };
                if control_events.send(event).await.is_err() {
                    break;
                }
            }
        });

        let echoes = Echoes::default();
        let copies_taken = Arc::new(AtomicU64::new(0));
        let acceptor_conn = quic.clone();
        let stream_events = events_tx.clone();
        let (pump_echoes, pump_taken) = (echoes.clone(), Arc::clone(&copies_taken));
        let (bulk_table, bulk_clips) = (Arc::clone(&table), Arc::clone(&clips));
        let bulk_files = Arc::clone(&file_join);
        // Each header is read on the stream's own task: one lost and retransmitted holds up
        // only its stream, not the accepts behind it.
        tasks.spawn(async move {
            loop {
                let recv = match acceptor_conn.accept_uni().await {
                    Ok(recv) => recv,
                    Err(e) => {
                        tracing::debug!(error = %e, "unidirectional streams end");
                        break;
                    }
                };
                let (events, echoes, taken) =
                    (stream_events.clone(), pump_echoes.clone(), Arc::clone(&pump_taken));
                let (table, clips) = (Arc::clone(&bulk_table), Arc::clone(&bulk_clips));
                let files = Arc::clone(&bulk_files);
                tokio::spawn(async move {
                    match read_uni(recv).await {
                        Ok(Uni::Session { session, rx }) => {
                            let copies = echoes.open(session);
                            let pump = Pump { session, events, echoes, taken };
                            pump_session(pump, rx, copies).await;
                        }
                        Ok(Uni::Bulk { header, mut rx }) if header.purpose == Purpose::FileText => {
                            let text = files::read_text(&header, &mut rx).await;
                            let take = |join: &mut files::Join| join.on_text(header.xfer, text);
                            hand_on_file(&files, &events, take).await;
                        }
                        Ok(Uni::Bulk { header, rx }) => {
                            receive_bulk(header, rx, table, clips).await;
                        }
                        Ok(Uni::Conversation { session, mut rx }) => {
                            while let Ok(event) = rx.recv().await {
                                let event = LinkEvent::Conversation { session, event };
                                if events.send(event).await.is_err() {
                                    break;
                                }
                            }
                        }
                        Err(e) => tracing::debug!(error = %e, "unidirectional stream refused"),
                    }
                });
            }
        });

        let copy_conn = quic.clone();
        let copies = slopty_net::echo::Copies::from_env();
        let save_events = events_tx.clone();
        tasks.spawn(async move {
            let mut inputs = InputNumbers::default();
            let mut screen_inputs = ScreenNumbers::default();
            // Typed input and a window's input leave unpaced, as an echo does on the way back;
            // a long paste is paced like any other write. Window input behind the pacer, with
            // its copies doubling the packets, stalled for up to 700 ms on a lossy link
            // (MEASUREMENTS.md, "window input through a lossy link").
            let mut lift = slopty_net::streams::EchoLift::default();
            while let Some(msg) = out_rx.recv().await {
                tracing::trace!(kind = msg.kind(), "control send");
                // A large save on the control stream would hold every keystroke behind it.
                let msg = match msg {
                    ClientMsg::WriteFile { path, text, base_modified_ms }
                        if text.len() > INLINE_FILE_BYTES =>
                    {
                        let (conn, events) = (copy_conn.clone(), save_events.clone());
                        tokio::spawn(async move {
                            if let Some(failed) =
                                files::send_save(&conn, path, text, base_modified_ms).await
                            {
                                let _sent = events.send(LinkEvent::Control(failed)).await;
                            }
                        });
                        continue;
                    }
                    msg => msg,
                };
                let numbered = inputs.number(&msg);
                let window_input = matches!(
                    &msg,
                    ClientMsg::Screen(slopty_proto::screen::ScreenRequest::Input { .. })
                );
                let typed = numbered.is_some_and(|(_, _, req)| !too_long(req));
                lift.before_frame(&tx, typed || window_input);
                let copy = numbered
                    .filter(|_numbered| copies.is_some())
                    .and_then(|(session, seq, req)| input_copy(session, seq, req));
                let screen_copy = screen_inputs.number(&msg).filter(|_copy| copies.is_some());
                if let Err(e) = tx.send(&msg).await {
                    tracing::debug!(error = %e, "control write failed");
                    break;
                }
                if let Some(copies) = copies {
                    for copy in [copy, screen_copy].into_iter().flatten() {
                        copies.send(&copy_conn, copy);
                    }
                }
            }
        });

        #[cfg(target_vendor = "apple")]
        let router = ScreenRouter::new();
        let datagram_conn = quic.clone();
        #[cfg(target_vendor = "apple")]
        let datagram_router = router.clone();
        tasks.spawn(read_datagrams(
            datagram_conn,
            echoes,
            #[cfg(target_vendor = "apple")]
            datagram_router,
        ));

        let up = Uplink { conn: quic.clone(), out: out_tx.clone(), table };
        let remote = Arc::new(LinkRemote::new(
            up,
            Arc::clone(&clips),
            events_tx,
            tokio::runtime::Handle::current(),
            shared_forwards,
        ));
        Self {
            ack,
            out: out_tx,
            events: Some(events_rx),
            conn: quic,
            #[cfg(target_vendor = "apple")]
            router,
            #[cfg(target_vendor = "apple")]
            runtime: tokio::runtime::Handle::current(),
            remote,
            clips,
            copies_taken,
            tasks,
        }
    }

    /// Frames shown from their datagram copy because it came before the session stream's.
    #[must_use]
    pub fn echo_copies_taken(&self) -> u64 {
        self.copies_taken.load(Ordering::Relaxed)
    }

    #[cfg(target_vendor = "apple")]
    /// Start receiving a screen stream the worker has `Opened`. Drop the handle to stop; send
    /// `ScreenRequest::Close` as well so the worker stops capturing. Callable from any thread.
    #[must_use]
    pub fn screen(&self, stream: StreamId, codec: VideoCodec) -> ScreenHandle {
        let conn = self.conn.clone();
        let feedback_conn = self.conn.clone();
        let feedback = move |datagram: Bytes| match feedback_conn.send_datagram(datagram) {
            Ok(()) => true,
            Err(e) => {
                tracing::debug!(%stream, error = %e, "feedback datagram");
                feedback_conn.close_reason().is_none()
            }
        };
        let uplink = ScreenUplink {
            control: self.out.clone(),
            feedback: Box::new(feedback),
            rtt: Box::new(move || slopty_net::endpoint::rtt(&conn)),
        };
        spawn_screen(&self.runtime, &self.router, stream, codec, uplink)
    }

    #[cfg(target_vendor = "apple")]
    /// The datagram router (to forget backlogs of streams that closed before attaching).
    #[must_use]
    pub const fn screens(&self) -> &ScreenRouter {
        &self.router
    }

    /// Files and clipboard bytes to and from this worker.
    #[must_use]
    pub fn remote(&self) -> Arc<dyn Remote> {
        Arc::clone(&self.remote)
    }

    /// The worker's `HelloAck`.
    #[must_use]
    pub const fn ack(&self) -> &HelloAck {
        &self.ack
    }

    /// Take the event receiver (once).
    #[must_use]
    pub const fn events(&mut self) -> Option<mpsc::Receiver<LinkEvent>> {
        self.events.take()
    }

    /// A cloneable handle for sending.
    #[must_use]
    pub fn sender(&self) -> mpsc::Sender<ClientMsg> {
        self.out.clone()
    }

    /// Queue a message.
    pub async fn send(&self, msg: ClientMsg) -> Result<(), NetError> {
        self.out.send(msg).await.map_err(|_gone| NetError::Closed)
    }

    /// RTT of the path.
    #[must_use]
    pub fn rtt(&self) -> Option<std::time::Duration> {
        slopty_net::endpoint::rtt(&self.conn)
    }

    /// UDP datagrams received so far; unchanged for several seconds means the worker is silent.
    #[must_use]
    pub fn received_datagrams(&self) -> u64 {
        slopty_net::endpoint::received_datagrams(&self.conn)
    }

    /// Probe the worker now ([`slopty_net::endpoint::ping`]): a restarted one answers with a
    /// reset, which ends the link.
    pub fn ping(&self) {
        slopty_net::endpoint::ping(&self.conn);
    }

    /// Where the worker is and the round trip to it, for diagnostics.
    #[must_use]
    pub fn path(&self) -> String {
        slopty_net::endpoint::describe_path(&self.conn)
    }

    /// The path's congestion picture (this side's sending), for diagnostics.
    #[must_use]
    pub fn health(&self) -> String {
        slopty_net::endpoint::describe_health(&self.conn)
    }

    /// Give the connection up from another task: the QUIC close makes the control reader
    /// fail, which surfaces as [`LinkEvent::Disconnected`] so the owner's reconnect path runs.
    /// Unlike [`close`](Self::close) the tasks keep running until that event is delivered.
    pub fn abandon(&self, reason: &str) {
        self.conn.close(1_u32.into(), reason.as_bytes());
    }

    /// Close the connection and stop the tasks.
    pub fn close(&mut self) {
        self.conn.close(0_u32.into(), b"bye");
        self.tasks.abort_all();
    }
}

impl Drop for WorkerLink {
    fn drop(&mut self) {
        self.clips.close();
        self.tasks.abort_all();
    }
}

/// Datagrams taken off the connection at most per read: a 62 KB frame's 51 fit.
const DATAGRAM_BATCH: usize = 64;

/// The connection's datagrams, until it ends: terminal frame copies to their session's pump,
/// the rest to the screen streams. Each read takes everything the connection holds, under one
/// lock of it, and the screen datagrams among them are routed under one lock of the router.
async fn read_datagrams(
    conn: slopty_net::Connection,
    echoes: Echoes,
    #[cfg(target_vendor = "apple")] router: ScreenRouter,
) {
    let mut batch = vec![Bytes::new(); DATAGRAM_BATCH];
    #[cfg(target_vendor = "apple")]
    let mut screens = Vec::with_capacity(DATAGRAM_BATCH);
    loop {
        let read = match conn.read_many_datagrams(&mut batch).await {
            Ok(read) => read,
            Err(e) => {
                tracing::debug!(error = %e, "read_datagram ended");
                break;
            }
        };
        #[cfg(target_vendor = "apple")]
        let now = std::time::Instant::now();
        for datagram in batch.iter_mut().take(read) {
            let datagram = std::mem::take(datagram);
            match split_term_datagram(&datagram) {
                Some((session, body)) => echoes.deliver(session, body),
                #[cfg(target_vendor = "apple")]
                None => screens.push(datagram),
                // No screen stream is ever opened without a decoder.
                #[cfg(not(target_vendor = "apple"))]
                None => {}
            }
        }
        #[cfg(target_vendor = "apple")]
        #[expect(clippy::iter_with_drain, reason = "the scratch keeps its capacity across reads")]
        router.route_many(screens.drain(..), now);
    }
}

/// Numbers each session's inputs as the control stream carries them
/// (`TermRequest::is_input`), for their datagram copies.
#[derive(Debug, Default)]
struct InputNumbers(HashMap<SessionId, u64>);

impl InputNumbers {
    /// `msg`'s session, its number among that session's inputs, and the request, when it is
    /// one. Every message the stream carries goes through here, in order.
    fn number<'m>(&mut self, msg: &'m ClientMsg) -> Option<(SessionId, u64, &'m TermRequest)> {
        let ClientMsg::Term { session, req } = msg else { return None };
        if !req.is_input() {
            return None;
        }
        let seq = self.0.entry(*session).or_default();
        *seq = seq.saturating_add(1);
        Some((*session, *seq, req))
    }
}

/// Numbers each window stream's input and quality changes as the control stream carries them
/// (`ScreenRequest::numbered`), for the input's datagram copies.
#[derive(Debug, Default)]
struct ScreenNumbers(HashMap<StreamId, ScreenPlace>);

/// Where a stream's numbering stands: requests numbered, and how many of them apply in order.
#[derive(Clone, Copy, Debug, Default)]
struct ScreenPlace {
    seq: u64,
    ordered: u64,
}

impl ScreenNumbers {
    /// Number `msg` when it is one of a stream's numbered requests, and return the datagram
    /// copy it gets: input, unless it is a paste chord, whose clipboard offer rides the control
    /// stream just ahead of it. Every message the stream carries goes through here, in order.
    fn number(&mut self, msg: &ClientMsg) -> Option<Bytes> {
        use slopty_proto::screen::ScreenRequest;

        let ClientMsg::Screen(req) = msg else { return None };
        if let ScreenRequest::Close(stream) = req {
            self.0.remove(stream);
            return None;
        }
        let (stream, in_order) = req.numbered()?;
        let place = self.0.entry(stream).or_default();
        place.seq = place.seq.saturating_add(1);
        if in_order {
            place.ordered = place.ordered.saturating_add(1);
        }
        let ScreenRequest::Input { input, .. } = req else { return None };
        if input.is_paste_chord() {
            return None;
        }
        let copy = ClientDatagram::ScreenInput {
            stream,
            seq: place.seq,
            ordered: place.ordered,
            input: input.clone(),
        };
        slopty_proto::codec::encode_body(&copy).ok().map(Bytes::from)
    }
}

/// Input too large for one datagram: a long paste or raw write.
fn too_long(req: &TermRequest) -> bool {
    let long = |len: usize| len > slopty_proto::media::MAX_DATAGRAM;
    matches!(req, TermRequest::Paste(text) if long(text.len()))
        || matches!(req, TermRequest::Raw(bytes) if long(bytes.len()))
}

/// The datagram copy of input `seq`, unless it is too large for one datagram or is a paste of a
/// picture. The picture's offer rides the control stream just ahead of it, and a copy that
/// overtook the offer would paste what the worker's pasteboard held before.
fn input_copy(session: SessionId, seq: u64, req: &TermRequest) -> Option<Bytes> {
    if too_long(req) || matches!(req, TermRequest::PastePicture(_)) {
        return None;
    }
    let copy = ClientDatagram::Input { session, seq, req: req.clone() };
    slopty_proto::codec::encode_body(&copy).ok().map(Bytes::from)
}

/// Where each session's frame copies go: the pump of its newest stream.
#[derive(Clone, Debug, Default)]
struct Echoes(Arc<Mutex<HashMap<SessionId, Route>>>);

/// One session's copies: the pump they go to, and the last frame number it passed on.
#[derive(Debug)]
struct Route {
    tx: mpsc::Sender<Frame>,
    passed: Arc<AtomicU64>,
}

/// [`Route::passed`] before any frame.
const NONE_PASSED: u64 = u64::MAX;

impl Echoes {
    /// Route `session`'s copies to a new pump from now on.
    fn open(&self, session: SessionId) -> (mpsc::Receiver<Frame>, FrameOrder) {
        let (tx, rx) = mpsc::channel(COPY_DEPTH);
        let passed = Arc::new(AtomicU64::new(NONE_PASSED));
        let order = FrameOrder { last: None, passed: Arc::clone(&passed) };
        self.0.lock().insert(session, Route { tx, passed });
        (rx, order)
    }

    /// A pump is done and has let its copies go; they go nowhere now, unless a newer pump
    /// took them.
    fn close(&self, session: SessionId) {
        let mut routes = self.0.lock();
        if routes.get(&session).is_some_and(|route| route.tx.is_closed()) {
            routes.remove(&session);
        }
    }

    /// A datagram brought `session`'s event `body`. A frame its pump already passed on (the
    /// stream copy came first) is dropped undecoded.
    fn deliver(&self, session: SessionId, body: &[u8]) {
        let Some(tx) = self.route(session, body) else { return };
        if let Ok(TermEvent::Frame(frame)) = slopty_proto::codec::decode_body(body) {
            let _full_or_gone = tx.try_send(frame);
        }
    }

    /// Where a copy goes, unless it is no frame or one already passed on.
    fn route(&self, session: SessionId, body: &[u8]) -> Option<mpsc::Sender<Frame>> {
        let (seq, _full) = frame_head(body)?;
        let routes = self.0.lock();
        let route = routes.get(&session)?;
        let passed = route.passed.load(Ordering::Relaxed);
        let tx = (passed == NONE_PASSED || seq > passed).then(|| route.tx.clone());
        drop(routes);
        tx
    }
}

/// Which frames of one session stream go on to the app: each frame once, in order, from
/// whichever copy came first. The stream carries every frame; a datagram copy may come sooner
/// and is taken only when it follows the last frame passed on, so it never opens a gap (which
/// would ask for every row again). The stream copy of a frame already passed on is dropped.
#[derive(Debug)]
struct FrameOrder {
    last: Option<u64>,
    /// `last` for the datagram reader, which drops a copy of a frame passed on undecoded.
    passed: Arc<AtomicU64>,
}

impl FrameOrder {
    /// The stream brought `frame`: whether it goes on.
    fn stream(&mut self, frame: &Frame) -> bool {
        if self.stream_has(frame.seq, frame.full) {
            return false;
        }
        self.pass(frame.seq);
        true
    }

    /// Whether the stream's frame `seq` was already passed on from its copy: dropped then, and
    /// before it is decoded.
    fn stream_has(&self, seq: u64, full: bool) -> bool {
        // A stream's own diffs only climb, so a diff at or below the last number passed on is
        // the one a copy brought. A whole frame always goes: a joiner's carries the others'
        // number.
        !full && self.last.is_some_and(|last| seq <= last)
    }

    /// A datagram brought a copy of `frame`: whether it goes on.
    fn copy(&mut self, frame: &Frame) -> bool {
        let next =
            !frame.full && self.last.is_some_and(|last| last.checked_add(1) == Some(frame.seq));
        if next {
            self.pass(frame.seq);
        }
        next
    }

    fn pass(&mut self, seq: u64) {
        self.last = Some(seq);
        self.passed.store(seq, Ordering::Relaxed);
    }
}

/// What a session stream's pump sends to, and where it hears its frames' copies.
struct Pump {
    session: SessionId,
    events: mpsc::Sender<LinkEvent>,
    echoes: Echoes,
    taken: Arc<AtomicU64>,
}

/// A session's terminal events, onto the link's event channel until the stream ends, with the
/// copies of its frames that come first ([`FrameOrder`]).
async fn pump_session(
    pump: Pump,
    mut stream: FramedRecv<TermEvent>,
    (mut copies, mut order): (mpsc::Receiver<Frame>, FrameOrder),
) {
    let Pump { session, events, echoes, taken } = pump;
    loop {
        let seen =
            |body: &[u8]| frame_head(body).is_some_and(|(seq, full)| order.stream_has(seq, full));
        let event = tokio::select! {
            received = stream.recv_unless(seen) => match received {
                Ok(event) => {
                    if let TermEvent::Frame(frame) = &event {
                        tracing::trace!(%session, seq = frame.seq, "frame received");
                        if !order.stream(frame) {
                            continue;
                        }
                    }
                    event
                }
                Err(NetError::Closed) => break,
                Err(e) => {
                    tracing::debug!(%session, error = %e, "session stream ended");
                    break;
                }
            },
            Some(frame) = copies.recv() => {
                if !order.copy(&frame) {
                    continue;
                }
                tracing::trace!(%session, seq = frame.seq, "frame copy received");
                taken.fetch_add(1, Ordering::Relaxed);
                TermEvent::Frame(frame)
            }
        };
        if events.send(LinkEvent::Term { session, event }).await.is_err() {
            break;
        }
    }
    drop(copies);
    echoes.close(session);
}

/// Hand the owner the file read `take` makes of the join, if any, holding the join until the
/// read is queued so the reads of a path keep the control stream's order; `false` when the
/// owner is gone.
async fn hand_on_file(
    join: &FileJoin,
    events: &mpsc::Sender<LinkEvent>,
    take: impl FnOnce(&mut files::Join) -> Option<WorkerMsg>,
) -> bool {
    let mut guard = join.lock().await;
    let Some(msg) = take(&mut guard) else { return true };
    let sent = events.send(LinkEvent::Control(msg)).await.is_ok();
    drop(guard);
    sent
}

/// A bulk stream the worker opened: a file of a download, or a clipboard representation too
/// big to inline.
async fn receive_bulk(
    header: BulkHeader,
    mut rx: RawRecv,
    table: Arc<Table>,
    clips: Arc<ClipCache>,
) {
    match header.purpose.clone() {
        Purpose::Download => crate::xfer::receive(&table, header, rx).await,
        Purpose::Clip { generation, uti } => {
            if header.size > MAX_CLIP_BYTES {
                tracing::warn!(generation, size = header.size, "clipboard too big; refused");
                rx.stop();
                clips.gone(generation);
                return;
            }
            let mut bytes = Vec::with_capacity(usize::try_from(header.size).unwrap_or(0));
            loop {
                match rx.chunk(256 * 1024).await {
                    Ok(Some(chunk)) => bytes.extend_from_slice(&chunk),
                    Ok(None) => break,
                    Err(e) => {
                        tracing::debug!(generation, error = %e, "clipboard bulk cut");
                        clips.gone(generation);
                        return;
                    }
                }
            }
            clips.fill(generation, uti, bytes);
        }
        Purpose::Upload | Purpose::Save { .. } => {
            tracing::debug!(xfer = %header.xfer, "a worker does not upload or save; stopping it");
            rx.stop();
        }
        Purpose::FileText => {
            tracing::debug!(xfer = %header.xfer, "a file's text off the file join; stopping it");
            rx.stop();
        }
    }
}

#[cfg(target_vendor = "apple")]
/// Whether the process has warmed VideoToolbox's decoder up yet.
static DECODER_WARM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(target_vendor = "apple")]
/// Pay VideoToolbox's first-session cost (150–400 ms) now, on its own thread, rather than in
/// the first stream's worker where it holds the keyframe and reads as a link stall.
///
/// Once per process; later calls are free. [`WorkerLink::start`] calls it, but a session
/// created while the first stream is already starting still delays that stream's own
/// session, so the apps call it at launch, before any worker is dialed.
pub fn warm_up_decoder() {
    if DECODER_WARM.swap(true, Ordering::Relaxed) {
        return;
    }
    let spawned = std::thread::Builder::new().name("decoder-warm-up".to_owned()).spawn(|| {
        match slopty_codec::warm_up() {
            Ok(took) => tracing::debug!(ms = took.as_millis(), "decoder warmed up"),
            Err(e) => tracing::debug!(error = %e, "decoder warm-up failed"),
        }
    });
    if let Err(e) = spawned {
        tracing::debug!(error = %e, "decoder warm-up thread");
    }
}

#[cfg(test)]
mod tests {
    use slopty_grid::{Cursor, LineIndex, TermModes};
    use slopty_proto::terminal::TermSize;

    use super::*;
    use crate::term::{Effect, TermState};

    fn frame(seq: u64, full: bool) -> Frame {
        Frame {
            seq,
            full,
            epoch: 1,
            cols: 80,
            rows: 24,
            cursor: Cursor::default(),
            modes: TermModes::empty(),
            oldest_line: LineIndex(0),
            first_visible_line: LineIndex(0),
            total_lines: 24,
            input_ack: 0,
            updates: Vec::new(),
            images: Vec::new(),
        }
    }

    /// Each frame goes on once, in order, from whichever copy came first; a copy that would
    /// skip a frame waits for the stream.
    #[test]
    fn a_frame_goes_on_once_in_order_from_the_first_copy() {
        let (_copies, mut order) = Echoes::default().open(SessionId::new());
        assert!(!order.copy(&frame(4, false)), "nothing shown yet: a diff has no base");
        assert!(order.stream(&frame(4, true)));
        assert!(order.copy(&frame(5, false)), "the next one, early");
        assert!(!order.stream(&frame(5, false)), "its stream copy is dropped");
        assert!(!order.copy(&frame(7, false)), "6 has not come: 7 would open a gap");
        assert!(!order.copy(&frame(5, false)), "a copy twice");
        assert!(order.stream(&frame(6, false)));
        assert!(order.stream(&frame(7, false)), "7's copy was dropped, so its stream copy goes");
        assert!(!order.copy(&frame(8, true)), "a whole frame is never a copy");
        assert!(order.stream(&frame(8, true)));
        assert!(order.stream(&frame(8, true)), "a joiner's whole frame at the same number");
    }

    /// A copy of a frame the stream already brought is dropped before it is decoded, and the
    /// stream's copy of one a datagram brought is skipped undecoded; the next one still goes.
    #[test]
    fn a_copy_already_passed_on_is_dropped_undecoded() {
        let echoes = Echoes::default();
        let session = SessionId::new();
        let (mut copies, mut order) = echoes.open(session);
        let body = |seq| slopty_proto::codec::encode_body(&TermEvent::Frame(frame(seq, false)));
        assert!(order.stream(&frame(4, true)));
        let mut cut = body(4).unwrap();
        cut.truncate(8);
        echoes.deliver(session, &cut);
        assert!(copies.try_recv().is_err(), "4 was passed on: its copy is not even decoded");
        assert!(echoes.route(session, &cut).is_none());
        echoes.deliver(session, &body(5).unwrap());
        let copy = copies.try_recv().expect("5 is new");
        assert!(order.copy(&copy));
        assert!(order.stream_has(5, false), "the stream's 5 is skipped before decoding");
        assert!(!order.stream_has(6, false) && !order.stream_has(5, true));
        let bell = slopty_proto::codec::encode_body(&TermEvent::Bell).unwrap();
        assert!(echoes.route(session, &bell).is_none(), "no frame, no copy");
    }

    /// What the app sees of copies that come early, late and out of order: every frame once, and
    /// never a request for every row again.
    #[test]
    fn copies_out_of_order_never_make_the_app_resync() {
        let (_copies, mut order) = Echoes::default().open(SessionId::new());
        let mut state = TermState::new(TermSize::default());
        let arrivals = [
            (false, frame(1, true)),
            (true, frame(3, false)),
            (true, frame(2, false)),
            (false, frame(2, false)),
            (true, frame(3, false)),
            (false, frame(3, false)),
            (true, frame(5, false)),
            (false, frame(4, false)),
            (false, frame(5, false)),
            (true, frame(6, false)),
            (false, frame(6, false)),
        ];
        let mut shown = Vec::new();
        for (copy, frame) in arrivals {
            let goes = if copy { order.copy(&frame) } else { order.stream(&frame) };
            if goes {
                shown.push(frame.seq);
                let effects = state.apply(TermEvent::Frame(frame));
                assert!(
                    !effects
                        .iter()
                        .any(|e| matches!(e, Effect::Request(TermRequest::Attach { .. }))),
                    "a resync after {shown:?}"
                );
            }
        }
        assert_eq!(shown, [1, 2, 3, 4, 5, 6]);
        assert_eq!(state.frames(), 6);
    }

    /// The link numbers inputs per session as the worker does, and nothing else.
    #[test]
    fn inputs_are_numbered_per_session_and_nothing_else_is() {
        let (a, b) = (SessionId::new(), SessionId::new());
        let term = |session, req| ClientMsg::Term { session, req };
        let mut numbers = InputNumbers::default();
        let seqs: Vec<Option<(SessionId, u64)>> = [
            term(a, TermRequest::Raw(b"x".to_vec())),
            term(a, TermRequest::Resize(TermSize::default())),
            term(b, TermRequest::Paste("p".to_owned())),
            ClientMsg::Ping { sent_at: slopty_core::MonoTime::from_nanos(1) },
            term(a, TermRequest::Clear),
            term(a, TermRequest::Reached { marker: 3 }),
            term(b, TermRequest::Focus { focused: true }),
        ]
        .iter()
        .map(|msg| numbers.number(msg).map(|(session, seq, _req)| (session, seq)))
        .collect();
        assert_eq!(
            seqs,
            [Some((a, 1)), None, Some((b, 1)), None, Some((a, 2)), None, Some((b, 2))]
        );
    }

    /// Each window stream's input and quality changes are numbered as the worker counts them,
    /// with how many apply in order; input has a copy carrying both, except a paste chord, and
    /// a closed stream starts over.
    #[test]
    fn window_input_is_numbered_per_stream_with_its_in_order_count() {
        use slopty_proto::input::{KeyAction, KeyCode, Mods};
        use slopty_proto::screen::{Quality, ScreenInput, ScreenRequest};

        let (a, b) = (StreamId(1), StreamId(2));
        let input = |stream, input| ClientMsg::Screen(ScreenRequest::Input { stream, input });
        let moved = |stream| input(stream, ScreenInput::Move { x: 1.0, y: 2.0 });
        let key = |stream, code, mods| {
            input(stream, ScreenInput::Key { code, action: KeyAction::Press, mods, text: None })
        };
        let quality = |stream| {
            ClientMsg::Screen(ScreenRequest::SetQuality { stream, quality: Quality::default() })
        };
        let mut numbers = ScreenNumbers::default();
        let copies: Vec<Option<(StreamId, u64, u64)>> = [
            moved(a),
            key(a, KeyCode::A, Mods::empty()),
            moved(b),
            quality(a),
            ClientMsg::Screen(ScreenRequest::Focus(a)),
            moved(a),
            key(a, KeyCode::V, Mods::SUPER),
            key(a, KeyCode::B, Mods::empty()),
            ClientMsg::Screen(ScreenRequest::Close(a)),
            moved(a),
        ]
        .iter()
        .map(|msg| {
            let copy = numbers.number(msg)?;
            match slopty_proto::codec::decode_body(&copy).unwrap() {
                ClientDatagram::ScreenInput { stream, seq, ordered, .. } => {
                    Some((stream, seq, ordered))
                }
                other => panic!("not a window input copy: {other:?}"),
            }
        })
        .collect();
        assert_eq!(
            copies,
            [
                Some((a, 1, 0)),
                Some((a, 2, 1)),
                Some((b, 1, 0)),
                None,
                None,
                Some((a, 4, 2)),
                None,
                Some((a, 6, 4)),
                None,
                Some((a, 1, 0)),
            ]
        );
    }

    /// A keystroke's copy decodes as the worker reads it, and the keystroke leaves unpaced; a
    /// paste too long for a datagram has no copy and is paced like any other write.
    #[test]
    fn a_keystroke_has_a_copy_and_a_long_paste_does_not() {
        let session = SessionId::new();
        let req = TermRequest::Raw(b"x".to_vec());
        assert!(!too_long(&req), "lifted past the pacer");
        let copy = input_copy(session, 7, &req).unwrap();
        let decoded: ClientDatagram = slopty_proto::codec::decode_body(&copy).unwrap();
        assert_eq!(decoded, ClientDatagram::Input { session, seq: 7, req });
        let long = TermRequest::Paste("x".repeat(slopty_proto::media::MAX_DATAGRAM + 1));
        assert!(too_long(&long), "paced");
        assert_eq!(input_copy(session, 8, &long), None);
    }

    /// A shell's paste of a picture goes behind its offer on the control stream and has no
    /// datagram copy: a copy could reach the worker before the offer does. It still counts as
    /// an input, so the keys after it are numbered as the worker numbers them.
    #[test]
    fn a_picture_paste_follows_its_offer_and_has_no_copy() {
        use slopty_proto::terminal::PasteChord;
        use slopty_proto::transfer::{ClipMsg, Offer, Peer};

        let session = SessionId::new();
        let offer = ClientMsg::Clip(ClipMsg::Offer(Offer {
            origin: Peer::Client(slopty_core::ClientId::new()),
            generation: 1,
            items: Vec::new(),
        }));
        let paste =
            ClientMsg::Term { session, req: TermRequest::PastePicture(PasteChord::Command) };
        let key = ClientMsg::Term { session, req: TermRequest::Raw(b"x".to_vec()) };
        let mut numbers = InputNumbers::default();
        let wire: Vec<_> = [offer, paste, key]
            .iter()
            .map(|msg| {
                let numbered = numbers.number(msg);
                let copy = numbered.and_then(|(s, seq, req)| input_copy(s, seq, req));
                (msg.kind(), numbered.map(|(_, seq, _)| seq), copy.is_some())
            })
            .collect();
        assert_eq!(
            wire,
            [("Clip", None, false), ("Term", Some(1), false), ("Term", Some(2), true)],
            "the offer first, then the paste with no copy, then a key that has one"
        );
    }

    /// A QUIC connection over loopback, each end on a runtime of its own: `sender` is the
    /// worker's end, `conn` the client's.
    #[cfg(target_vendor = "apple")]
    struct Loopback {
        sender_rt: tokio::runtime::Runtime,
        client_rt: tokio::runtime::Runtime,
        sender: slopty_net::Connection,
        conn: slopty_net::Connection,
    }

    #[cfg(target_vendor = "apple")]
    impl Loopback {
        fn open() -> Self {
            use std::net::SocketAddr;
            use std::time::Duration;

            use slopty_proto::media::MAX_DATAGRAM;

            let runtime = |name: &str| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name(name)
                    .enable_all()
                    .build()
                    .unwrap()
            };
            let (sender_rt, client_rt) = (runtime("sender"), runtime("client"));
            let loopback = SocketAddr::from(([127, 0, 0, 1], 0));
            let bind = |rt: &tokio::runtime::Runtime, server| {
                let _entered = rt.enter();
                slopty_net::endpoint::bind(loopback, server).unwrap()
            };
            let (server, client) = (bind(&sender_rt, true), bind(&client_rt, false));
            let at = server.local_addr().unwrap();
            let accepted =
                sender_rt.spawn(async move { server.accept().await.unwrap().await.unwrap() });
            let conn = client_rt
                .block_on(async { client.connect(at, "127.0.0.1").unwrap().await.unwrap() });
            let sender = sender_rt.block_on(accepted).unwrap();
            // The peer's datagram limit arrives with its transport parameters, a moment after
            // the handshake: sent before it, a datagram is refused as too large.
            sender_rt.block_on(async {
                while sender.max_datagram_size().is_none_or(|max| max < MAX_DATAGRAM) {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            });
            Self { sender_rt, client_rt, sender, conn }
        }
    }

    /// One read can bring a terminal frame's copy among a screen stream's datagrams: the copy
    /// goes to its session's pump, and the screen datagrams to their stream in the order they
    /// were sent.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn the_reader_sorts_a_burst_between_sessions_and_screens() {
        use std::time::Duration;

        use slopty_core::StreamId;
        use slopty_media::{EncodedFrame, Packetizer};
        use slopty_proto::datagram::term_datagram;

        let link = Loopback::open();
        let echoes = Echoes::default();
        let session = SessionId::new();
        let (mut copies, _order) = echoes.open(session);
        let router = ScreenRouter::with_loss(0);
        let mut screen = router.attach(StreamId(1));
        let _reader = link.client_rt.spawn(read_datagrams(link.conn.clone(), echoes, router));

        let body = slopty_proto::codec::encode_body(&TermEvent::Frame(frame(5, false))).unwrap();
        let copy = term_datagram(session, &body).unwrap();
        let mut packetizer = Packetizer::new(StreamId(1));
        packetizer.set_parity_permille(0);
        let data = vec![7_u8; 3_000];
        let frame = EncodedFrame {
            data: &data,
            keyframe: true,
            ltr_token: None,
            ltr_refresh: false,
            capture_ts_us: 1,
        };
        let datagrams = packetizer.packetize(&frame, 0, |_| {}).unwrap().datagrams.clone();
        assert!(datagrams.len() >= 3);
        let (head, rest) = datagrams.split_at(1);
        for d in head.iter().chain([&copy]).chain(rest) {
            link.sender.send_datagram(d.clone()).unwrap();
        }

        let within = Duration::from_secs(5);
        let got = link.client_rt.block_on(async {
            let copied = tokio::time::timeout(within, copies.recv()).await.unwrap().unwrap();
            let mut routed = Vec::new();
            for _ in 0..datagrams.len() {
                let (_at, d) = tokio::time::timeout(within, screen.recv()).await.unwrap().unwrap();
                routed.push(d);
            }
            (copied, routed)
        });
        assert_eq!(got.0.seq, 5, "the copy reached its session");
        assert_eq!(got.1, datagrams, "the screen's datagrams, in order");
    }

    /// What the connection's datagram reader costs the client's runtime, and how late a frame's
    /// last datagram is handed to its stream after the worker sent it: 62 KB frames (51
    /// datagrams) at 60 a second over loopback QUIC, the sender on a runtime of its own so the
    /// client's busy time is the connection's driver and the reader alone. A measurement, run
    /// by hand: `docs/MEASUREMENTS.md`, "the client's datagram path".
    #[cfg(target_vendor = "apple")]
    #[test]
    #[ignore = "a measurement; run with --run-ignored only --no-capture"]
    #[expect(clippy::cast_precision_loss, reason = "measurement arithmetic")]
    fn datagram_reader_cost() {
        use std::time::{Duration, Instant};

        use slopty_core::StreamId;
        use slopty_media::{EncodedFrame, Packetizer};

        const FRAMES: u32 = 1_200;
        /// Frames per round: the busy time is read per round, and the quietest round is the
        /// figure least disturbed by the rest of the machine.
        const ROUND: u32 = 60;
        const EVERY: Duration = Duration::from_micros(16_667);
        let Loopback { sender_rt: _sender_rt, client_rt, sender, conn } = Loopback::open();
        let router = ScreenRouter::with_loss(0);
        let mut queue = router.attach(StreamId(1));
        let _reader = client_rt.spawn(read_datagrams(conn.clone(), Echoes::default(), router));

        let data: Vec<u8> = (0..62_000_u32).map(|i| u8::try_from(i % 251).unwrap()).collect();
        let mut packetizer = Packetizer::new(StreamId(1));
        packetizer.set_parity_permille(0);
        let metrics = client_rt.metrics();
        let workers = metrics.num_workers();
        let busy = || (0..workers).map(|w| metrics.worker_total_busy_duration(w)).sum::<Duration>();
        let parks = || (0..workers).map(|w| metrics.worker_park_count(w)).sum::<u64>();
        let (busy_before, parks_before) = (busy(), parks());
        let (mut late, mut routed, mut missing) = (Vec::new(), 0_u64, 0_u64);
        let mut rtt_reads = Vec::new();
        let (mut rounds, mut round_at) = (Vec::new(), (busy_before, 0_u64));
        for n in 0..FRAMES {
            if n % ROUND == 0 && n > 0 {
                let now = (busy(), routed);
                let took = now.0.saturating_sub(round_at.0).as_secs_f64() * 1e6;
                rounds.push(took / now.1.saturating_sub(round_at.1).max(1) as f64);
                round_at = now;
            }
            let started = Instant::now();
            let frame = EncodedFrame {
                data: &data,
                keyframe: n == 0,
                ltr_token: None,
                ltr_refresh: false,
                capture_ts_us: n,
            };
            let datagrams = packetizer.packetize(&frame, 0, |_| {}).unwrap().datagrams.clone();
            let sent = Instant::now();
            for d in &datagrams {
                sender.send_datagram(d.clone()).unwrap();
            }
            // What a screen worker paid on every wake for the round trip, read while the frame's
            // datagrams are being received under the same lock.
            let read = Instant::now();
            let _rtt = slopty_net::endpoint::rtt(&conn);
            rtt_reads.push(read.elapsed().as_secs_f64() * 1e9);
            let mut last = None;
            for _ in 0..datagrams.len() {
                let next =
                    client_rt.block_on(async { tokio::time::timeout(EVERY, queue.recv()).await });
                let Ok(Some((at, _))) = next else {
                    missing = missing.saturating_add(1);
                    continue;
                };
                routed = routed.saturating_add(1);
                last = Some(at);
            }
            if let Some(last) = last {
                late.push(last.saturating_duration_since(sent).as_secs_f64() * 1e6);
            }
            #[expect(clippy::disallowed_methods, reason = "the frame clock of a measurement")]
            std::thread::sleep(EVERY.saturating_sub(started.elapsed()));
        }
        let busy = busy().saturating_sub(busy_before);
        let parks = parks().saturating_sub(parks_before);
        late.sort_by(f64::total_cmp);
        rounds.sort_by(f64::total_cmp);
        rtt_reads.sort_by(f64::total_cmp);
        let ns = |p: usize| {
            rtt_reads.get(rtt_reads.len().saturating_sub(1).saturating_mul(p) / 100).copied()
        };
        eprintln!(
            "MEASURE rtt read beside the traffic: p50 {:.0} / p99 {:.0} / max {:.0} ns",
            ns(50).unwrap_or(0.0),
            ns(99).unwrap_or(0.0),
            rtt_reads.last().copied().unwrap_or(0.0),
        );
        let q = |p: usize| late.get(late.len().saturating_sub(1).saturating_mul(p) / 100);
        eprintln!(
            "MEASURE reader: {routed} datagrams routed, {missing} missing; client runtime busy {:.2} µs (rounds: quietest {:.2}, median {:.2}) and {:.3} parks per datagram; sent → last routed p50 {:.0} / p99 {:.0} / max {:.0} µs",
            busy.as_secs_f64() * 1e6 / routed as f64,
            rounds.first().copied().unwrap_or(0.0),
            rounds.get(rounds.len() / 2).copied().unwrap_or(0.0),
            parks as f64 / routed as f64,
            q(50).copied().unwrap_or(0.0),
            q(99).copied().unwrap_or(0.0),
            late.last().copied().unwrap_or(0.0),
        );
        conn.close(0_u32.into(), b"done");
    }
}
