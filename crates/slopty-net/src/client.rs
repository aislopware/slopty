//! Client side: connect, say hello, get the control stream.

use std::net::SocketAddr;
use std::time::Duration;

use noq::{Connection, Endpoint};
use slopty_proto::handshake::{Hello, HelloAck, Rejection};
use slopty_proto::{ClientMsg, WorkerMsg};

use crate::NetError;
use crate::addr::HostAddr;
use crate::framed::{FramedRecv, FramedSend};

/// How long the QUIC handshake may take before the address counts as unreachable. A path that
/// answers at all answers in one round trip; without this a dead address waits out the 45 s idle
/// timeout. Kept short because a dial is how a worker that comes back is found: noq's Initial
/// probes back off to 2 s apart, so a longer dial only adds a gap in which the worker is not
/// asked, where a new dial after the redial backoff asks it again at once.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long to wait for the worker's answer to `Hello`.
const ACK_TIMEOUT: Duration = Duration::from_secs(15);

/// Errors from the handshake: the worker said no, or the transport failed.
#[derive(Debug, thiserror::Error)]
pub enum HandshakeError {
    /// The worker refused.
    #[error("rejected: {0:?}")]
    Rejected(Rejection),
    /// Transport.
    #[error(transparent)]
    Net(#[from] NetError),
}

/// A live connection to a worker after a successful `Hello`.
#[derive(Debug)]
pub struct WorkerConn {
    /// The QUIC connection.
    pub conn: Connection,
    /// The address that answered.
    pub remote: SocketAddr,
    /// What the worker told us.
    pub ack: HelloAck,
    /// Control stream, client → worker.
    pub tx: FramedSend<ClientMsg>,
    /// Control stream, worker → client.
    pub rx: FramedRecv<WorkerMsg>,
}

/// Bind a client endpoint on every interface, both families, any free port. One per process is
/// enough: connections come and go on it independently.
pub fn bind_client() -> Result<Endpoint, NetError> {
    crate::endpoint::bind(crate::endpoint::any(0), false)
}

/// Connect to `addr` by name: each address it resolves to in turn, IPv4 first (a tailnet's
/// IPv4 address is the one every peer has), until one completes the handshake.
pub async fn connect(
    endpoint: &Endpoint,
    addr: &HostAddr,
    hello: Hello,
) -> Result<WorkerConn, HandshakeError> {
    let (conn, remote) = dial_any(endpoint, addr, None).await?;
    greet(conn, remote, hello).await
}

/// The QUIC handshake with the first address `addr` resolves to that completes one, IPv4 first
/// (a tailnet's IPv4 address is the one every peer has), on `config` or the endpoint's default.
/// The error is the last address's.
pub(crate) async fn dial_any(
    endpoint: &Endpoint,
    addr: &HostAddr,
    config: Option<noq::ClientConfig>,
) -> Result<(Connection, SocketAddr), NetError> {
    let mut candidates = addr.resolve().await?;
    candidates.sort_by_key(SocketAddr::is_ipv6);
    let mut last = NetError::Connect(format!("{addr}: no address"));
    for candidate in candidates {
        match dial(endpoint, candidate, addr.host(), config.clone()).await {
            Ok(conn) => return Ok((conn, candidate)),
            Err(e) => {
                tracing::debug!(%addr, %candidate, error = %e, "address did not answer");
                last = e;
            }
        }
    }
    Err(last)
}

/// Connect to one socket address.
pub async fn connect_addr(
    endpoint: &Endpoint,
    addr: SocketAddr,
    hello: Hello,
) -> Result<WorkerConn, HandshakeError> {
    let conn = dial(endpoint, addr, &addr.ip().to_string(), None).await?;
    greet(conn, addr, hello).await
}

/// The QUIC handshake alone, on `config` or the endpoint's default client config.
pub(crate) async fn dial(
    endpoint: &Endpoint,
    addr: SocketAddr,
    name: &str,
    config: Option<noq::ClientConfig>,
) -> Result<Connection, NetError> {
    let connecting = match config {
        Some(config) => endpoint.connect_with(config, addr, name),
        None => endpoint.connect(addr, name),
    }
    .map_err(|e| NetError::Connect(format!("{addr}: {e}")))?;
    tokio::time::timeout(HANDSHAKE_TIMEOUT, connecting)
        .await
        .map_err(|_elapsed| NetError::Connect(format!("{addr}: no answer")))?
        .map_err(|e| NetError::Connect(format!("{addr}: {e}")))
}

/// `Hello` on a fresh control stream, and the worker's answer.
async fn greet(
    conn: Connection,
    remote: SocketAddr,
    hello: Hello,
) -> Result<WorkerConn, HandshakeError> {
    let (send, recv) = conn.open_bi().await.map_err(|e| NetError::stream(&e))?;
    let mut tx = FramedSend::<ClientMsg>::new(send);
    let mut rx = FramedRecv::<WorkerMsg>::new(recv);
    tx.send(&ClientMsg::Hello(hello)).await?;
    let reply = tokio::time::timeout(ACK_TIMEOUT, rx.recv())
        .await
        .map_err(|_elapsed| NetError::Protocol("hello ack timeout"))??;
    match reply {
        WorkerMsg::HelloAck(ack) => Ok(WorkerConn { conn, remote, ack, tx, rx }),
        WorkerMsg::Rejected(why) => {
            conn.close(0_u32.into(), b"rejected");
            Err(HandshakeError::Rejected(why))
        }
        _other => Err(NetError::Protocol("expected HelloAck").into()),
    }
}

impl WorkerConn {
    /// Current RTT.
    #[must_use]
    pub fn rtt(&self) -> Option<Duration> {
        crate::endpoint::rtt(&self.conn)
    }

    /// Close the connection.
    pub fn close(&self) {
        self.conn.close(0_u32.into(), b"bye");
    }
}
