//! Links to the server, both ends: the server's accept loop, and the dial a worker, client or
//! agent makes (`slopty_proto::server`, `docs/decisions/topology.md`).
//!
//! Every link is one bidirectional stream opened by the dialer, whose first message is
//! [`ToServer::Hello`]. The answer ([`FromServer::Welcome`], or a refusal such as a duplicate
//! worker) is the caller's.
//! Server links run on [`crate::endpoint::lease_transport_config`]: a link that goes quiet for
//! [`crate::endpoint::LEASE_IDLE_TIMEOUT`] is dead, which for a worker ends its lease.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use noq::{Connection, Endpoint};
use slopty_proto::server::{FromServer, Refusal, Role, ToServer};
use tokio::sync::{Mutex, mpsc};

use crate::NetError;
use crate::addr::HostAddr;
use crate::admission::Admission;
use crate::framed::{FramedRecv, FramedSend};
use crate::listen::Greeted;
use crate::worker::close_code;

/// How long a dialer waits for `Welcome`.
const WELCOME_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a refused dialer gets to read its `Refused` before the close.
const REFUSE_LINGER: Duration = Duration::from_secs(2);

/// The server's listening endpoint.
#[derive(Debug, Clone)]
pub struct ServerListener {
    endpoint: Endpoint,
    admission: Admission,
    greeted: Arc<Mutex<mpsc::Receiver<Greeted<FromServer, ToServer, Role>>>>,
}

/// A dialer that said `Hello`. The caller answers it:
/// [`FromServer::Welcome`] on [`Self::tx`], or [`Self::refuse`].
#[derive(Debug)]
pub struct AcceptedLink {
    /// The QUIC connection.
    pub conn: Connection,
    /// Where it connected from (IPv4 when it is one).
    pub remote: SocketAddr,
    /// Who it says it is.
    pub role: Role,
    /// Server → dialer.
    pub tx: FramedSend<FromServer>,
    /// Dialer → server.
    pub rx: FramedRecv<ToServer>,
}

impl ServerListener {
    /// Listen on `local` on [`crate::endpoint::bind_lease`], letting in whom `admission` admits.
    /// Must be called on a tokio runtime: the accept loop runs as a task of its own.
    pub fn bind(local: SocketAddr, admission: Admission) -> Result<Self, NetError> {
        let endpoint = crate::endpoint::bind_lease(local)?;
        let hello = |first| match first {
            ToServer::Hello { role } => Some(role),
            _ => None,
        };
        let rx = crate::listen::spawn(endpoint.clone(), admission.clone(), hello, "dialer");
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

    /// The next dialer that said `Hello`; `None` once the endpoint is closed.
    pub async fn accept(&self) -> Option<AcceptedLink> {
        let Greeted { conn, remote, hello, tx, rx } = self.greeted.lock().await.recv().await?;
        Some(AcceptedLink { conn, remote, role: hello, tx, rx })
    }
}

impl AcceptedLink {
    /// Send `Refused`, give the dialer a moment to read it, then close.
    pub async fn refuse(mut self, why: Refusal) {
        let _sent = self.tx.send(&FromServer::Refused(why)).await;
        let _finished = self.tx.finish();
        let _closed_by_peer = tokio::time::timeout(REFUSE_LINGER, self.conn.closed()).await;
        self.conn.close(close_code::PROTOCOL.into(), b"refused");
    }
}

/// Why a dial to the server failed.
#[derive(Debug, thiserror::Error)]
pub enum DialError {
    /// The server refused the hello.
    #[error("refused: {0:?}")]
    Refused(Refusal),
    /// Transport.
    #[error(transparent)]
    Net(#[from] NetError),
}

/// A live link to the server after `Welcome`.
#[derive(Debug)]
pub struct ServerLink {
    /// The QUIC connection; [`Connection::closed`] resolves when the lease ends.
    pub conn: Connection,
    /// The address that answered.
    pub remote: SocketAddr,
    /// The server's name, from its `Welcome`.
    pub name: String,
    /// Dialer → server.
    pub tx: FramedSend<ToServer>,
    /// Server → dialer: for a worker, requests; for a client or agent, the directory first,
    /// then changes and replies.
    pub rx: FramedRecv<FromServer>,
}

impl ServerLink {
    /// Close the link (a worker's lease then ends at once rather than after the idle timeout).
    pub fn close(&self) {
        self.conn.close(close_code::NORMAL.into(), b"bye");
    }
}

/// Dial the server at `addr` from `endpoint` on the lease transport, say `Hello` as `role`, and
/// wait for `Welcome`. Each address the name resolves to is tried in turn, IPv4 first.
///
/// `addr` is usually parsed with [`HostAddr::parse_with_port`] and
/// [`crate::endpoint::SERVER_PORT`]. A worker refused as [`Refusal::DuplicateWorker`] right
/// after a restart is waiting out its previous connection's idle timeout, and gets in on a
/// retry a few seconds later.
pub async fn connect(
    endpoint: &Endpoint,
    addr: &HostAddr,
    role: Role,
) -> Result<ServerLink, DialError> {
    let config = crate::endpoint::lease_client_config();
    let (conn, remote) = crate::client::dial_any(endpoint, addr, Some(config)).await?;
    hello(conn, remote, role).await
}

/// `Hello` on a fresh stream and the server's answer.
async fn hello(conn: Connection, remote: SocketAddr, role: Role) -> Result<ServerLink, DialError> {
    let (send, recv) = conn.open_bi().await.map_err(|e| NetError::stream(&e))?;
    let mut tx = FramedSend::<ToServer>::new(send);
    let mut rx = FramedRecv::<FromServer>::new(recv);
    tx.send(&ToServer::Hello { role }).await?;
    let reply = tokio::time::timeout(WELCOME_TIMEOUT, rx.recv())
        .await
        .map_err(|_elapsed| NetError::Protocol("welcome timeout"))??;
    match reply {
        FromServer::Welcome { name } => Ok(ServerLink { conn, remote, name, tx, rx }),
        FromServer::Refused(why) => {
            conn.close(close_code::NORMAL.into(), b"refused");
            Err(DialError::Refused(why))
        }
        _other => Err(NetError::Protocol("expected Welcome").into()),
    }
}
