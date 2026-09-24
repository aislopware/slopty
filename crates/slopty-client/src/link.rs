//! One connection to a host, as a channel of events plus a queue of outbound messages.
//!
//! Runs on tokio; a UI on another executor just holds the receiver and the sender.

use std::sync::Arc;

use slopty_core::{SessionId, StreamId, XferId};
use slopty_net::client::HostConn;
use slopty_net::framed::FramedRecv;
use slopty_net::streams::{RawRecv, Uni, accept_uni};
use slopty_net::{ClientMsg, HostMsg, NetError};
use slopty_proto::handshake::HelloAck;
use slopty_proto::screen::VideoCodec;
use slopty_proto::terminal::TermEvent;
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

/// Everything the UI hears from one host.
#[derive(Debug)]
pub enum LinkEvent {
    /// A control-stream message (session list changes, pongs, agent events, …).
    Control(HostMsg),
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
    /// The connection is gone; `HostLink` is dead after this.
    Disconnected(String),
}

/// A live host connection.
#[derive(Debug)]
pub struct HostLink {
    ack: HelloAck,
    out: mpsc::Sender<ClientMsg>,
    events: Option<mpsc::Receiver<LinkEvent>>,
    conn: slopty_net::Connection,
    router: ScreenRouter,
    /// The runtime the link's tasks run on; screen workers join them from any thread.
    runtime: tokio::runtime::Handle,
    remote: Arc<dyn Remote>,
    tasks: JoinSet<()>,
}

impl HostLink {
    /// Wrap a connection: spawns the control reader, the session-stream acceptor and the writer.
    /// The worker's listening ports arrive as they are (`HostMsg::Ports`), forwarded nowhere.
    #[must_use]
    pub fn start(conn: HostConn) -> Self {
        Self::launch(conn, false)
    }

    /// [`Self::start`], and every port the worker's shells listen on is served on this
    /// machine's loopback ([`LinkEvent::Ports`] says where): the app's link.
    #[must_use]
    pub fn start_forwarding(conn: HostConn) -> Self {
        Self::launch(conn, true)
    }

    fn launch(conn: HostConn, forward: bool) -> Self {
        warm_up_decoder();
        let HostConn { conn: quic, ack, mut tx, mut rx, .. } = conn;
        let (events_tx, events_rx) = mpsc::channel(EVENT_DEPTH);
        let (out_tx, mut out_rx) = mpsc::channel::<ClientMsg>(OUT_DEPTH);
        let mut tasks = JoinSet::new();

        let table = Arc::new(Table::default());
        let clips = Arc::new(ClipCache::new(out_tx.clone()));

        let control_events = events_tx.clone();
        let (control_table, control_clips) = (Arc::clone(&table), Arc::clone(&clips));
        let mut forwards = Forwards::new(quic.clone());
        tasks.spawn(async move {
            loop {
                let msg = match rx.recv().await {
                    Ok(msg) => msg,
                    Err(e) => {
                        forwards.clear();
                        let _sent =
                            control_events.send(LinkEvent::Disconnected(e.to_string())).await;
                        break;
                    }
                };
                tracing::trace!(kind = msg.kind(), "control message");
                let event = match msg {
                    HostMsg::Xfer(x) if control_table.on_control(&x) => continue,
                    HostMsg::Clip(c) if control_clips.on_control(&c) => continue,
                    HostMsg::Ports { session, ports } if forward => {
                        let forwards = forwards.update(session, ports);
                        LinkEvent::Ports { session, forwards }
                    }
                    msg => LinkEvent::Control(msg),
                };
                if control_events.send(event).await.is_err() {
                    break;
                }
            }
        });

        let acceptor_conn = quic.clone();
        let stream_events = events_tx.clone();
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
                        tokio::spawn(pump_session(session, rx, stream_events.clone()));
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

        tasks.spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                tracing::trace!(kind = msg.kind(), "control send");
                if let Err(e) = tx.send(&msg).await {
                    tracing::debug!(error = %e, "control write failed");
                    break;
                }
            }
        });

        let router = ScreenRouter::new();
        let datagram_conn = quic.clone();
        let datagram_router = router.clone();
        tasks.spawn(async move {
            loop {
                match datagram_conn.read_datagram().await {
                    Ok(datagram) => datagram_router.route(datagram, std::time::Instant::now()),
                    Err(e) => {
                        tracing::debug!(error = %e, "read_datagram ended");
                        break;
                    }
                }
            }
        });

        let runtime = tokio::runtime::Handle::current();
        let up = Uplink { conn: quic.clone(), out: out_tx.clone(), table };
        let remote = Arc::new(LinkRemote::new(up, clips, events_tx, runtime.clone()));
        Self {
            ack,
            out: out_tx,
            events: Some(events_rx),
            conn: quic,
            router,
            runtime,
            remote,
            tasks,
        }
    }

    /// Start receiving a screen stream the host has `Opened`. Drop the handle to stop; send
    /// `ScreenRequest::Close` as well so the host stops capturing. Callable from any thread.
    #[must_use]
    pub fn screen(&self, stream: StreamId, codec: VideoCodec) -> ScreenHandle {
        let conn = self.conn.clone();
        let feedback_conn = self.conn.clone();
        let feedback = move |datagram: bytes::Bytes| match feedback_conn.send_datagram(datagram) {
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

    /// The host's `HelloAck`.
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

    /// UDP datagrams received so far; unchanged for several seconds means the host is silent.
    #[must_use]
    pub fn received_datagrams(&self) -> u64 {
        slopty_net::endpoint::received_datagrams(&self.conn)
    }

    /// Where the host is and the round trip to it, for diagnostics.
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

impl Drop for HostLink {
    fn drop(&mut self) {
        self.tasks.abort_all();
    }
}

/// A session's terminal events, onto the link's event channel until the stream ends.
async fn pump_session(
    session: SessionId,
    mut stream: FramedRecv<TermEvent>,
    events: mpsc::Sender<LinkEvent>,
) {
    loop {
        match stream.recv().await {
            Ok(event) => {
                if matches!(event, TermEvent::Frame(_)) {
                    tracing::trace!(%session, "frame received");
                }
                if events.send(LinkEvent::Term { session, event }).await.is_err() {
                    break;
                }
            }
            Err(NetError::Closed) => break,
            Err(e) => {
                tracing::debug!(%session, error = %e, "session stream ended");
                break;
            }
        }
    }
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
/// Once per process; later calls are free. [`HostLink::start`] calls it, but a session
/// created while the first stream is already starting still delays that stream's own
/// session, so the apps call it at launch, before any host is dialed.
pub fn warm_up_decoder() {
    if DECODER_WARM.swap(true, std::sync::atomic::Ordering::Relaxed) {
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
