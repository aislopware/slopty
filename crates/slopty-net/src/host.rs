//! Host side: admit, accept, read the `Hello`, hand over the control stream.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use noq::{Connection, Endpoint};
use slopty_core::SessionId;
use slopty_proto::handshake::{Hello, Rejection};
use slopty_proto::terminal::TermEvent;
use slopty_proto::{ClientMsg, HostMsg, PROTOCOL_VERSION, StreamHeader};
use tokio::sync::{Mutex, mpsc};

use crate::NetError;
use crate::admission::Admission;
use crate::framed::{FramedRecv, FramedSend};

/// How long a client has to send `Hello` after connecting.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// How long we let a rejected client read its `Rejected` before closing under it.
const REJECT_LINGER: Duration = Duration::from_secs(2);

/// QUIC close codes.
pub mod close_code {
    /// Normal shutdown.
    pub const NORMAL: u32 = 0;
    /// Protocol error.
    pub const PROTOCOL: u32 = 2;
}

/// The host's listening endpoint and who it lets in.
#[derive(Debug, Clone)]
pub struct HostListener {
    endpoint: Endpoint,
    admission: Admission,
    greeted: Arc<Mutex<mpsc::Receiver<AcceptedClient>>>,
}

/// A client that said `Hello` with our protocol version. The caller answers with a `HelloAck`.
#[derive(Debug)]
pub struct AcceptedClient {
    /// The QUIC connection (for session streams and datagrams).
    pub conn: Connection,
    /// Where it connected from.
    pub remote: SocketAddr,
    /// Its hello.
    pub hello: Hello,
    /// Control stream, host → client.
    pub tx: FramedSend<HostMsg>,
    /// Control stream, client → host.
    pub rx: FramedRecv<ClientMsg>,
}

impl HostListener {
    /// Listen on `local` (see [`crate::endpoint::bind`]), letting in whom `admission` admits.
    /// Must be called on a tokio runtime: the accept loop runs as a task of its own.
    pub fn bind(local: SocketAddr, admission: Admission) -> Result<Self, NetError> {
        let endpoint = crate::endpoint::bind(local, true)?;
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(admit(endpoint.clone(), admission.clone(), tx));
        Ok(Self { endpoint, admission, greeted: Arc::new(Mutex::new(rx)) })
    }

    /// The endpoint.
    #[must_use]
    pub const fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Where it listens.
    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        self.endpoint.local_addr().map_err(|e| NetError::Bind(e.to_string()))
    }

    /// Who it lets in.
    #[must_use]
    pub const fn admission(&self) -> &Admission {
        &self.admission
    }

    /// The next client that said `Hello`; `None` once the endpoint is closed.
    pub async fn accept(&self) -> Option<AcceptedClient> {
        self.greeted.lock().await.recv().await
    }
}

/// The accept loop: refuse peers outside `admission` before any connection state exists, and
/// greet the rest each on a task of its own, so a peer that connects and says nothing holds up
/// nobody behind it. Refused, malformed and silent connections are logged and dropped.
async fn admit(endpoint: Endpoint, admission: Admission, greeted: mpsc::Sender<AcceptedClient>) {
    while let Some(incoming) = endpoint.accept().await {
        let peer = crate::endpoint::canonical(incoming.remote_address());
        if !admission.admits(peer.ip()) {
            tracing::info!(%peer, "refused: outside the admitted ranges");
            incoming.refuse();
            continue;
        }
        let greeted = greeted.clone();
        tokio::spawn(async move {
            match greet(incoming).await {
                Ok(client) => {
                    let _sent = greeted.send(client).await;
                }
                Err(e) => tracing::info!(%peer, error = %e, "client dropped"),
            }
        });
    }
}

/// Finish the handshake and read the client's `Hello`.
async fn greet(incoming: noq::Incoming) -> Result<AcceptedClient, NetError> {
    let remote = crate::endpoint::canonical(incoming.remote_address());
    let conn = incoming.await.map_err(|e| NetError::Connect(e.to_string()))?;
    let (send, recv) = tokio::time::timeout(HELLO_TIMEOUT, conn.accept_bi())
        .await
        .map_err(|_elapsed| NetError::Protocol("no control stream"))?
        .map_err(|e| NetError::stream(&e))?;
    let tx = FramedSend::<HostMsg>::new(send);
    let mut rx = FramedRecv::<ClientMsg>::new(recv);
    let first = tokio::time::timeout(HELLO_TIMEOUT, rx.recv())
        .await
        .map_err(|_elapsed| NetError::Protocol("hello timeout"))??;
    let ClientMsg::Hello(hello) = first else {
        conn.close(close_code::PROTOCOL.into(), b"hello first");
        return Err(NetError::Protocol("first message must be Hello"));
    };
    if hello.protocol != PROTOCOL_VERSION {
        let why = Rejection::ProtocolVersion { host: PROTOCOL_VERSION };
        reject(&conn, tx, why, close_code::PROTOCOL).await;
        return Err(NetError::Protocol("protocol version"));
    }
    Ok(AcceptedClient { conn, remote, hello, tx, rx })
}

/// Send `Rejected`, give the client a moment to read it (a close would discard unread data), then
/// close.
async fn reject(conn: &Connection, mut tx: FramedSend<HostMsg>, why: Rejection, code: u32) {
    let _sent = tx.send(&HostMsg::Rejected(why)).await;
    let _finished = tx.finish();
    let _closed_by_peer = tokio::time::timeout(REJECT_LINGER, conn.closed()).await;
    conn.close(code.into(), b"rejected");
}

/// Open a session stream to a client and write its header.
pub async fn open_session_stream(
    conn: &Connection,
    session: SessionId,
) -> Result<FramedSend<TermEvent>, NetError> {
    let send = conn.open_uni().await.map_err(|e| NetError::stream(&e))?;
    let mut header = FramedSend::<StreamHeader>::new(send);
    header.send(&StreamHeader { session }).await?;
    Ok(header.retype())
}
