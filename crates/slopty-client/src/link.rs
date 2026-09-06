//! One connection to a host, as a channel of events plus a queue of outbound messages.
//!
//! Runs on tokio; a UI on another executor just holds the receiver and the sender.

use std::sync::Arc;

use slopty_core::{SessionId, StreamId};
use slopty_net::client::HostConn;
use slopty_net::framed::FramedSend;
use slopty_net::{ClientMsg, HostMsg, NetError};
use slopty_proto::handshake::HelloAck;
use slopty_proto::screen::VideoCodec;
use slopty_proto::terminal::TermEvent;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinSet;

use crate::screen::{ScreenHandle, ScreenRouter, Uplink, spawn_screen};

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
    tasks: JoinSet<()>,
}

impl HostLink {
    /// Wrap a connection: spawns the control reader, the session-stream acceptor and the writer.
    #[must_use]
    pub fn start(conn: HostConn) -> Self {
        warm_up_decoder();
        let HostConn { conn: quic, ack, tx, mut rx, .. } = conn;
        let (events_tx, events_rx) = mpsc::channel(EVENT_DEPTH);
        let (out_tx, mut out_rx) = mpsc::channel::<ClientMsg>(OUT_DEPTH);
        let mut tasks = JoinSet::new();

        let writer_tx: Arc<Mutex<FramedSend<ClientMsg>>> = Arc::new(Mutex::new(tx));
        let control_events = events_tx.clone();
        tasks.spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(msg) => {
                        tracing::trace!(kind = msg.kind(), "control message");
                        if control_events.send(LinkEvent::Control(msg)).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _sent =
                            control_events.send(LinkEvent::Disconnected(e.to_string())).await;
                        break;
                    }
                }
            }
        });

        let acceptor_conn = quic.clone();
        let stream_events = events_tx;
        tasks.spawn(async move {
            loop {
                let recv = match acceptor_conn.accept_uni().await {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::debug!(error = %e, "accept_uni ended");
                        break;
                    }
                };
                let events = stream_events.clone();
                tokio::spawn(async move {
                    let mut header =
                        slopty_net::framed::FramedRecv::<slopty_proto::StreamHeader>::new(recv);
                    let Ok(hdr) = header.recv().await else { return };
                    let session = hdr.session;
                    let mut stream = header.retype::<TermEvent>();
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
                });
            }
        });

        tasks.spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                tracing::trace!(kind = msg.kind(), "control send");
                let mut tx = writer_tx.lock().await;
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

        let paths_conn = quic.clone();
        tasks.spawn(slopty_net::endpoint::log_path_events(paths_conn, "client"));
        let runtime = tokio::runtime::Handle::current();
        Self { ack, out: out_tx, events: Some(events_rx), conn: quic, router, runtime, tasks }
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
        let uplink = Uplink {
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

    /// RTT on the selected path.
    #[must_use]
    pub fn rtt(&self) -> Option<std::time::Duration> {
        slopty_net::endpoint::rtt(&self.conn)
    }

    /// UDP datagrams received so far; unchanged for several seconds means the host is silent.
    #[must_use]
    pub fn received_datagrams(&self) -> u64 {
        slopty_net::endpoint::received_datagrams(&self.conn)
    }

    /// Whether the selected path goes through a relay (`None` while no path is selected).
    #[must_use]
    pub fn relayed(&self) -> Option<bool> {
        slopty_net::endpoint::relayed(&self.conn)
    }

    /// Every path, for diagnostics.
    #[must_use]
    pub fn paths(&self) -> String {
        slopty_net::endpoint::describe_paths(&self.conn)
    }

    /// The selected path's congestion picture (this side's sending), for diagnostics.
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
