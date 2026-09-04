//! One authenticated client connection.

use std::collections::HashMap;

use bytes::Bytes;
use slopty_core::{ClientId, SessionId, StreamId};
use slopty_host::HostError;
use slopty_host::screen::{DATAGRAM_QUEUE, DatagramBudget, ScreenStream, listing};
use slopty_net::host::{AuthenticatedClient, open_session_stream};
use slopty_net::{ClientMsg, Connection, HostMsg, NetError};
use slopty_proto::PROTOCOL_VERSION;
use slopty_proto::handshake::{Caps, HelloAck};
use slopty_proto::screen::{ScreenEvent, ScreenRequest};
use slopty_proto::terminal::{CloseReason, TermEvent, TermRequest, TermSize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::Daemon;

/// Events buffered per attached session before the client is considered stuck.
const SINK_DEPTH: usize = 256;
/// Outbound control messages buffered before the writer applies backpressure.
const CONTROL_DEPTH: usize = 1024;

pub async fn serve(daemon: Daemon, client: AuthenticatedClient) {
    let remote = client.remote;
    let id = client.hello.client;
    tracing::info!(%remote, client = %id, name = %client.hello.name, "client connected");
    if let Err(e) = run(&daemon, client).await {
        tracing::info!(%remote, error = %e, "client finished");
    }
    daemon.host.client_gone(id);
}

async fn run(daemon: &Daemon, client: AuthenticatedClient) -> Result<(), NetError> {
    let AuthenticatedClient { conn, hello, mut tx, mut rx, .. } = client;

    let (out, mut out_rx) = mpsc::channel::<HostMsg>(CONTROL_DEPTH);
    let writer: JoinHandle<Result<(), NetError>> = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            tx.send(&msg).await?;
        }
        Ok(())
    });

    let ack = HelloAck {
        protocol: PROTOCOL_VERSION,
        host: daemon.id,
        name: daemon.name.clone(),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        caps: Caps::empty(),
        sessions: daemon.host.summaries().await,
    };
    out.send(HostMsg::HelloAck(ack)).await.map_err(|_gone| NetError::Closed)?;
    out.send(HostMsg::Canvas(daemon.canvas.snapshot())).await.map_err(|_gone| NetError::Closed)?;

    let mut events = daemon.events.subscribe();
    let (datagrams, datagram_rx) = mpsc::channel::<Bytes>(DATAGRAM_QUEUE);
    let budget = DatagramBudget::new();
    let pump = tokio::spawn(pump_datagrams(conn.clone(), datagram_rx, budget.clone()));
    let mut peer = Peer {
        daemon,
        conn,
        client: hello.client,
        out,
        attached: HashMap::new(),
        screens: HashMap::new(),
        next_stream: 1,
        datagrams,
        budget,
        pump,
    };

    let result = loop {
        tokio::select! {
            msg = rx.recv() => {
                let msg = match msg {
                    Ok(m) => m,
                    Err(NetError::Closed) => break Ok(()),
                    Err(e) => break Err(e),
                };
                peer.handle(msg).await;
            }
            ev = events.recv() => {
                match ev {
                    Ok(msg) => {
                        if peer.out.send(msg).await.is_err() {
                            break Ok(());
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(client = %peer.client, lagged = n, "missed host events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break Ok(()),
                }
            }
            reason = peer.conn.closed() => {
                tracing::debug!(client = %peer.client, %reason, "connection closed");
                break Ok(());
            }
        }
    };
    peer.close_screens().await;
    drop(peer);
    writer.abort();
    result
}

/// Drain the media queue into QUIC datagrams, tracking what the path can carry.
async fn pump_datagrams(conn: Connection, mut rx: mpsc::Receiver<Bytes>, budget: DatagramBudget) {
    let mut last_budget = 0;
    while let Some(datagram) = rx.recv().await {
        if let Some(max) = conn.max_datagram_size() {
            if max != last_budget {
                tracing::debug!(max, "datagram budget");
                budget.set(max);
                last_budget = max;
            }
            if datagram.len() > max {
                // Cut for a larger budget than the path has now; the next frame adapts.
                continue;
            }
        }
        if let Err(e) = conn.send_datagram(datagram) {
            tracing::debug!(error = %e, "send_datagram");
            break;
        }
    }
}

/// One client's view of the daemon: its connection, outbound control queue, and attachments.
struct Peer<'d> {
    daemon: &'d Daemon,
    conn: Connection,
    client: ClientId,
    out: mpsc::Sender<HostMsg>,
    attached: HashMap<SessionId, JoinHandle<()>>,
    screens: HashMap<StreamId, ScreenStream>,
    next_stream: u32,
    datagrams: mpsc::Sender<Bytes>,
    budget: DatagramBudget,
    pump: JoinHandle<()>,
}

impl Drop for Peer<'_> {
    fn drop(&mut self) {
        for task in self.attached.values() {
            task.abort();
        }
        self.pump.abort();
    }
}

impl Peer<'_> {
    async fn handle(&mut self, msg: ClientMsg) {
        match msg {
            ClientMsg::Hello(_) => {
                tracing::warn!(client = %self.client, "duplicate Hello ignored");
            }
            ClientMsg::Ping { sent_at } => {
                let _sent = self.out.send(HostMsg::Pong { sent_at }).await;
            }
            ClientMsg::OpenSession(req) => match self.daemon.host.open(&req).await {
                Ok(handle) => {
                    let session = handle.id();
                    let summaries = self.daemon.host.summaries().await;
                    if let Some(summary) = summaries.into_iter().find(|s| s.id == session) {
                        let _sent = self.daemon.events.send(HostMsg::SessionOpened(summary));
                    }
                    if let Some(delta) = self.daemon.canvas.ensure_terminal(session, self.client) {
                        let _sent = self.daemon.events.send(HostMsg::Canvas(delta));
                    }
                    if req.attach {
                        self.attach(session, req.size).await;
                    }
                }
                Err(e) => self.report(SessionId::nil(), &e).await,
            },
            ClientMsg::Term { session, req } => self.term(session, req).await,
            ClientMsg::Canvas(op) => match self.daemon.canvas.apply(op, self.client) {
                Ok(delta) => {
                    let _sent = self.daemon.events.send(HostMsg::Canvas(delta));
                }
                Err(e) => self.report(SessionId::nil(), &e).await,
            },
            ClientMsg::Screen(req) => self.screen(req).await,
        }
    }

    async fn screen(&mut self, req: ScreenRequest) {
        match req {
            ScreenRequest::List => {
                let out = self.out.clone();
                let client = self.client;
                tokio::spawn(async move {
                    match listing().await {
                        Ok(event) => {
                            let _sent = out.send(HostMsg::Screen(event)).await;
                        }
                        Err(e) => tracing::warn!(%client, error = %e, "screen listing"),
                    }
                });
            }
            ScreenRequest::Open { target, quality } => {
                let id = StreamId(self.next_stream);
                self.next_stream = self.next_stream.wrapping_add(1).max(1);
                let out = self.out.clone();
                let on_stop = move |e: slopty_capture::CaptureError| {
                    let reason = e.to_string();
                    let event = ScreenEvent::Closed { stream: id, reason };
                    let _sent = out.try_send(HostMsg::Screen(event));
                };
                let opened = ScreenStream::open(
                    id,
                    target,
                    quality,
                    self.datagrams.clone(),
                    self.budget.clone(),
                    on_stop,
                )
                .await;
                match opened {
                    Ok((stream, event)) => {
                        tracing::info!(client = %self.client, %id, ?target, "screen opened");
                        self.screens.insert(id, stream);
                        let _sent = self.out.send(HostMsg::Screen(event)).await;
                    }
                    Err(e) => {
                        tracing::warn!(client = %self.client, ?target, error = %e, "screen open");
                        let event = ScreenEvent::Closed { stream: id, reason: e.to_string() };
                        let _sent = self.out.send(HostMsg::Screen(event)).await;
                    }
                }
            }
            ScreenRequest::Close(id) => {
                if let Some(stream) = self.screens.remove(&id) {
                    stream.close().await;
                    let reason = "closed by client".to_owned();
                    let event = ScreenEvent::Closed { stream: id, reason };
                    let _sent = self.out.send(HostMsg::Screen(event)).await;
                }
            }
            ScreenRequest::SetQuality { stream, quality } => {
                if let Some(s) = self.screens.get_mut(&stream)
                    && let Err(e) = s.set_quality(&quality)
                {
                    tracing::warn!(client = %self.client, %stream, error = %e, "set quality");
                }
            }
            ScreenRequest::Report { stream, report } => {
                if let Some(s) = self.screens.get(&stream) {
                    s.report(&report);
                }
            }
            ScreenRequest::RequestRefresh { stream, last_good_frame } => {
                if let Some(s) = self.screens.get(&stream) {
                    s.request_refresh(last_good_frame);
                }
            }
            ScreenRequest::Nack { stream, frame, fragments } => {
                if let Some(s) = self.screens.get(&stream) {
                    s.nack(frame, &fragments);
                }
            }
            ScreenRequest::Input { stream, input } => {
                if let Some(s) = self.screens.get_mut(&stream)
                    && let Err(e) = s.inject(&input)
                {
                    tracing::debug!(client = %self.client, %stream, error = %e, "input");
                }
            }
            ScreenRequest::Focus(stream) => {
                if let Some(s) = self.screens.get(&stream)
                    && let Err(e) = s.focus()
                {
                    tracing::debug!(client = %self.client, %stream, error = %e, "focus");
                }
            }
        }
    }

    /// Stop every screen stream (the connection is going away).
    async fn close_screens(&mut self) {
        for (_id, stream) in self.screens.drain() {
            stream.close().await;
        }
    }

    async fn term(&mut self, session: SessionId, req: TermRequest) {
        match req {
            TermRequest::Attach { size } => self.attach(session, size).await,
            TermRequest::Detach => {
                self.forget(session);
                if let Ok(h) = self.daemon.host.get(session) {
                    let _ignored = h.detach(self.client);
                }
            }
            TermRequest::Close => {
                self.forget(session);
                match self.daemon.host.close(session).await {
                    Ok(()) => {
                        let reason = CloseReason::Requested;
                        let _sent =
                            self.daemon.events.send(HostMsg::SessionClosed { session, reason });
                        for delta in self.daemon.canvas.remove_session(session, self.client) {
                            let _sent = self.daemon.events.send(HostMsg::Canvas(delta));
                        }
                    }
                    Err(e) => self.report(session, &e).await,
                }
            }
            other => {
                let outcome =
                    self.daemon.host.get(session).and_then(|h| h.request(self.client, other));
                if let Err(e) = outcome {
                    self.report(session, &e).await;
                }
            }
        }
    }

    fn forget(&mut self, session: SessionId) {
        if let Some(task) = self.attached.remove(&session) {
            task.abort();
        }
    }

    async fn report(&self, session: SessionId, e: &HostError) {
        tracing::warn!(client = %self.client, %session, error = %e, "request failed");
        let event = TermEvent::Error(e.to_string());
        let _sent = self.out.send(HostMsg::Term { session, event }).await;
    }

    /// Attach to `session`: open a uni stream and pump the actor's events into it.
    async fn attach(&mut self, session: SessionId, size: TermSize) {
        let handle = match self.daemon.host.get(session) {
            Ok(h) => h,
            Err(e) => return self.report(session, &e).await,
        };
        self.forget(session);
        let stream = match open_session_stream(&self.conn, session).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(client = %self.client, %session, error = %e, "open session stream");
                return;
            }
        };
        let (sink, mut events) = mpsc::channel::<TermEvent>(SINK_DEPTH);
        if let Err(e) = handle.attach(self.client, size, sink) {
            return self.report(session, &e).await;
        }
        let client = self.client;
        let task = tokio::spawn(async move {
            let mut stream = stream;
            while let Some(ev) = events.recv().await {
                if let Err(e) = stream.send(&ev).await {
                    tracing::debug!(%client, %session, error = %e, "session stream ended");
                    break;
                }
            }
            let _finished = stream.finish();
        });
        self.attached.insert(session, task);
    }
}
