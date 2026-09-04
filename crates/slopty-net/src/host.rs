//! Host side: accept, authenticate, hand over the control stream.

use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use slopty_core::SessionId;
use slopty_proto::handshake::{Hello, Rejection};
use slopty_proto::terminal::TermEvent;
use slopty_proto::{ClientMsg, HostMsg, PROTOCOL_VERSION, StreamHeader};
use tokio::sync::Mutex;

use crate::endpoint::{Role, bind};
use crate::framed::{FramedRecv, FramedSend};
use crate::pairing::{PairTicket, TrustStore};
use crate::{ALPN, NetError};

/// How long a client has to send `Hello` after connecting.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// How long we let a rejected client read its `Rejected` before closing under it.
const REJECT_LINGER: Duration = Duration::from_secs(2);

/// QUIC close codes.
pub mod close_code {
    /// Normal shutdown.
    pub const NORMAL: u32 = 0;
    /// Client is not paired.
    pub const NOT_PAIRED: u32 = 1;
    /// Protocol error.
    pub const PROTOCOL: u32 = 2;
}

/// The host's listening endpoint plus its trust store.
#[derive(Debug, Clone)]
pub struct HostListener {
    endpoint: Endpoint,
    store: Arc<Mutex<TrustStore>>,
}

/// A client that passed authentication. The caller answers its `Hello` with a `HelloAck`.
#[derive(Debug)]
pub struct AuthenticatedClient {
    /// The QUIC connection (for session streams and datagrams).
    pub conn: Connection,
    /// The client's transport identity.
    pub remote: EndpointId,
    /// Its hello.
    pub hello: Hello,
    /// Control stream, host → client.
    pub tx: FramedSend<HostMsg>,
    /// Control stream, client → host.
    pub rx: FramedRecv<ClientMsg>,
    /// True when this connection redeemed a pairing token (first time we see this device).
    pub newly_paired: bool,
}

impl HostListener {
    /// Bind with the store's key.
    pub async fn bind(store: TrustStore) -> Result<Self, NetError> {
        let endpoint = bind(store.secret().clone(), Role::Host).await?;
        Ok(Self { endpoint, store: Arc::new(Mutex::new(store)) })
    }

    /// The endpoint.
    #[must_use]
    pub const fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Our address (id + relay + direct addresses known so far).
    #[must_use]
    pub fn addr(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    /// Wait until relay + direct addresses are known (so a ticket is complete).
    pub async fn online(&self) {
        self.endpoint.online().await;
    }

    /// Mint a pairing ticket.
    pub async fn pair_ticket(&self) -> PairTicket {
        let token = self.store.lock().await.mint_token();
        PairTicket { addr: self.addr(), token }
    }

    /// The trust store.
    #[must_use]
    pub fn store(&self) -> Arc<Mutex<TrustStore>> {
        Arc::clone(&self.store)
    }

    /// Accept the next client. Unauthenticated or malformed connections are closed and skipped;
    /// `None` when the endpoint is closed.
    pub async fn accept(&self) -> Option<AuthenticatedClient> {
        loop {
            let incoming = self.endpoint.accept().await?;
            let accepting = match incoming.accept() {
                Ok(a) => a,
                Err(e) => {
                    tracing::debug!(error = %e, "incoming rejected");
                    continue;
                }
            };
            let conn = match accepting.await {
                Ok(c) => c,
                Err(e) => {
                    tracing::debug!(error = %e, "handshake failed");
                    continue;
                }
            };
            if conn.alpn() != ALPN {
                conn.close(close_code::PROTOCOL.into(), b"alpn");
                continue;
            }
            match self.authenticate(conn).await {
                Ok(client) => return Some(client),
                Err(e) => tracing::info!(error = %e, "client rejected"),
            }
        }
    }

    async fn authenticate(&self, conn: Connection) -> Result<AuthenticatedClient, NetError> {
        let remote = conn.remote_id();
        let (send, recv) = tokio::time::timeout(HELLO_TIMEOUT, conn.accept_bi())
            .await
            .map_err(|_elapsed| NetError::Protocol("no control stream"))?
            .map_err(|e| NetError::Stream(e.to_string()))?;
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
        let mut newly_paired = false;
        {
            let mut store = self.store.lock().await;
            if !store.is_paired(&remote) {
                let redeemed = match hello.pair_token {
                    Some(token) => store.redeem(&token, remote, hello.client, &hello.name)?,
                    None => false,
                };
                if !redeemed {
                    drop(store);
                    reject(&conn, tx, Rejection::NotPaired, close_code::NOT_PAIRED).await;
                    return Err(NetError::NotPaired);
                }
                newly_paired = true;
            }
        }
        Ok(AuthenticatedClient { conn, remote, hello, tx, rx, newly_paired })
    }
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
    let send = conn.open_uni().await.map_err(|e| NetError::Stream(e.to_string()))?;
    let mut header = FramedSend::<StreamHeader>::new(send);
    header.send(&StreamHeader { session }).await?;
    Ok(header.retype())
}
