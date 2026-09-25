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
use slopty_net::streams::{RawRecv, Uni, accept_uni};
use slopty_net::{ClientMsg, NetError, WorkerMsg};
use slopty_proto::datagram::{ClientDatagram, TermDatagram, parse_term_datagram};
use slopty_proto::handshake::HelloAck;
use slopty_proto::screen::VideoCodec;
use slopty_proto::terminal::{Frame, TermEvent, TermRequest};
use slopty_proto::transfer::{BulkHeader, Purpose};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::clip::{ClipCache, MAX_CLIP_BYTES};
use crate::remote::{LinkRemote, Remote};
use crate::screen::{ScreenHandle, ScreenRouter, Uplink as ScreenUplink, spawn_screen};
use crate::tunnel::{Forward, Forwards};
use crate::xfer::{Table, Uplink};

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
    router: ScreenRouter,
    /// The runtime the link's tasks run on; screen workers join them from any thread.
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
        warm_up_decoder();
        let WorkerConn { conn: quic, ack, mut tx, mut rx, .. } = conn;
        let (events_tx, events_rx) = mpsc::channel(EVENT_DEPTH);
        let (out_tx, mut out_rx) = mpsc::channel::<ClientMsg>(OUT_DEPTH);
        let mut tasks = JoinSet::new();

        let table = Arc::new(Table::default());
        let clips = Arc::new(ClipCache::new(out_tx.clone()));

        let control_events = events_tx.clone();
        let (control_table, control_clips) = (Arc::clone(&table), Arc::clone(&clips));
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
        tasks.spawn(async move {
            loop {
                let uni = match accept_uni(&acceptor_conn).await {
                    Ok(uni) => uni,
                    Err(e) => {
                        tracing::debug!(error = %e, "accept_uni ended");
                        if acceptor_conn.close_reason().is_some() {
                            break;
                        }
                        continue;
                    }
                };
                match uni {
                    Uni::Session { session, rx } => {
                        let copies = pump_echoes.open(session);
                        let pump = Pump {
                            session,
                            events: stream_events.clone(),
                            echoes: pump_echoes.clone(),
                            taken: Arc::clone(&pump_taken),
                        };
                        tokio::spawn(pump_session(pump, rx, copies));
                    }
                    Uni::Bulk { header, rx } => {
                        tokio::spawn(receive_bulk(
                            header,
                            rx,
                            Arc::clone(&bulk_table),
                            Arc::clone(&bulk_clips),
                        ));
                    }
                }
            }
        });

        let copy_conn = quic.clone();
        let copies = slopty_net::echo::Copies::from_env();
        tasks.spawn(async move {
            let mut inputs = InputNumbers::default();
            while let Some(msg) = out_rx.recv().await {
                tracing::trace!(kind = msg.kind(), "control send");
                let copy = inputs
                    .number(&msg)
                    .filter(|_numbered| copies.is_some())
                    .and_then(|(session, seq, req)| input_copy(session, seq, req));
                if let Err(e) = tx.send(&msg).await {
                    tracing::debug!(error = %e, "control write failed");
                    break;
                }
                if let (Some(copies), Some(copy)) = (copies, copy) {
                    copies.send(&copy_conn, copy);
                }
            }
        });

        let router = ScreenRouter::new();
        let datagram_conn = quic.clone();
        let datagram_router = router.clone();
        tasks.spawn(async move {
            loop {
                match datagram_conn.read_datagram().await {
                    Ok(datagram) => match parse_term_datagram(&datagram) {
                        Some(copy) => echoes.deliver(copy),
                        None => datagram_router.route(datagram, std::time::Instant::now()),
                    },
                    Err(e) => {
                        tracing::debug!(error = %e, "read_datagram ended");
                        break;
                    }
                }
            }
        });

        let runtime = tokio::runtime::Handle::current();
        let up = Uplink { conn: quic.clone(), out: out_tx.clone(), table };
        let remote = Arc::new(LinkRemote::new(
            up,
            Arc::clone(&clips),
            events_tx,
            runtime.clone(),
            shared_forwards,
        ));
        Self {
            ack,
            out: out_tx,
            events: Some(events_rx),
            conn: quic,
            router,
            runtime,
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

/// The datagram copy of input `seq`, unless it is too large for one datagram.
fn input_copy(session: SessionId, seq: u64, req: &TermRequest) -> Option<Bytes> {
    let long = |len: usize| len > slopty_proto::media::MAX_DATAGRAM;
    if matches!(req, TermRequest::Paste(text) if long(text.len()))
        || matches!(req, TermRequest::Raw(bytes) if long(bytes.len()))
    {
        return None;
    }
    let copy = ClientDatagram::Input { session, seq, req: req.clone() };
    slopty_proto::codec::encode_body(&copy).ok().map(Bytes::from)
}

/// Where each session's frame copies go: the pump of its newest stream.
#[derive(Clone, Debug, Default)]
struct Echoes(Arc<Mutex<HashMap<SessionId, mpsc::Sender<Frame>>>>);

impl Echoes {
    /// Route `session`'s copies to a new pump from now on.
    fn open(&self, session: SessionId) -> mpsc::Receiver<Frame> {
        let (tx, rx) = mpsc::channel(COPY_DEPTH);
        self.0.lock().insert(session, tx);
        rx
    }

    /// A pump is done and has let its copies go; they go nowhere now, unless a newer pump
    /// took them.
    fn close(&self, session: SessionId) {
        let mut routes = self.0.lock();
        if routes.get(&session).is_some_and(mpsc::Sender::is_closed) {
            routes.remove(&session);
        }
    }

    fn deliver(&self, TermDatagram { session, event }: TermDatagram) {
        let TermEvent::Frame(frame) = event else { return };
        if let Some(tx) = self.0.lock().get(&session) {
            let _full_or_gone = tx.try_send(frame);
        }
    }
}

/// Which frames of one session stream go on to the app: each frame once, in order, from
/// whichever copy came first. The stream carries every frame; a datagram copy may come sooner
/// and is taken only when it follows the last frame passed on, so it never opens a gap (which
/// would ask for every row again). The stream copy of a frame already passed on is dropped.
#[derive(Debug, Default)]
struct FrameOrder {
    last: Option<u64>,
}

impl FrameOrder {
    /// The stream brought `frame`: whether it goes on.
    fn stream(&mut self, frame: &Frame) -> bool {
        // A stream's own diffs only climb, so a diff at or below the last number passed on is
        // the one a copy brought. A whole frame always goes: a joiner's carries the others'
        // number.
        if !frame.full && self.last.is_some_and(|last| frame.seq <= last) {
            return false;
        }
        self.last = Some(frame.seq);
        true
    }

    /// A datagram brought a copy of `frame`: whether it goes on.
    fn copy(&mut self, frame: &Frame) -> bool {
        let next =
            !frame.full && self.last.is_some_and(|last| last.checked_add(1) == Some(frame.seq));
        if next {
            self.last = Some(frame.seq);
        }
        next
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
    mut copies: mpsc::Receiver<Frame>,
) {
    let Pump { session, events, echoes, taken } = pump;
    let mut order = FrameOrder::default();
    loop {
        let event = tokio::select! {
            received = stream.recv() => match received {
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
        Purpose::Upload => {
            tracing::debug!(xfer = %header.xfer, "a worker does not upload; stopping it");
            rx.stop();
        }
    }
}

/// Whether the process has warmed VideoToolbox's decoder up yet.
static DECODER_WARM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
        let mut order = FrameOrder::default();
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

    /// What the app sees of copies that come early, late and out of order: every frame once, and
    /// never a request for every row again.
    #[test]
    fn copies_out_of_order_never_make_the_app_resync() {
        let mut order = FrameOrder::default();
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

    /// A keystroke's copy decodes as the worker reads it; a paste too long for a datagram has
    /// none.
    #[test]
    fn a_keystroke_has_a_copy_and_a_long_paste_does_not() {
        let session = SessionId::new();
        let req = TermRequest::Raw(b"x".to_vec());
        let copy = input_copy(session, 7, &req).unwrap();
        let decoded: ClientDatagram = slopty_proto::codec::decode_body(&copy).unwrap();
        assert_eq!(decoded, ClientDatagram::Input { session, seq: 7, req });
        let long = TermRequest::Paste("x".repeat(slopty_proto::media::MAX_DATAGRAM + 1));
        assert_eq!(input_copy(session, 8, &long), None);
    }
}
