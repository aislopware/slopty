//! One authenticated client connection.

use std::collections::HashMap;

use slopty_core::{ClientId, SessionId};
use slopty_host::HostError;
use slopty_net::host::{AuthenticatedClient, open_session_stream};
use slopty_net::{ClientMsg, Connection, HostMsg, NetError};
use slopty_proto::PROTOCOL_VERSION;
use slopty_proto::handshake::{Caps, HelloAck};
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
    let mut peer = Peer { daemon, conn, client: hello.client, out, attached: HashMap::new() };

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
    drop(peer);
    writer.abort();
    result
}

/// One client's view of the daemon: its connection, outbound control queue, and attachments.
struct Peer<'d> {
    daemon: &'d Daemon,
    conn: Connection,
    client: ClientId,
    out: mpsc::Sender<HostMsg>,
    attached: HashMap<SessionId, JoinHandle<()>>,
}

impl Drop for Peer<'_> {
    fn drop(&mut self) {
        for task in self.attached.values() {
            task.abort();
        }
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
            ClientMsg::Screen(_) => {
                tracing::debug!(client = %self.client, "screen not served yet");
            }
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
