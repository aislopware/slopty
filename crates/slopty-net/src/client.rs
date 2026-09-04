//! Client side: connect, say hello, get the control stream.

use std::time::Duration;

use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
use slopty_proto::handshake::{Hello, HelloAck, Rejection};
use slopty_proto::terminal::TermEvent;
use slopty_proto::{ClientMsg, HostMsg, StreamHeader};

use crate::endpoint::{Reach, Role, bind};
use crate::framed::{FramedRecv, FramedSend};
use crate::pairing::PairTicket;
use crate::{ALPN, NetError};

/// How long to wait for the host's answer to `Hello`.
const ACK_TIMEOUT: Duration = Duration::from_secs(15);

/// Errors from the handshake, distinguished so the UI can react (re-pair vs. update).
#[derive(Debug, thiserror::Error)]
pub enum HandshakeError {
    /// The host refused.
    #[error("rejected: {0:?}")]
    Rejected(Rejection),
    /// Transport.
    #[error(transparent)]
    Net(#[from] NetError),
}

/// A live connection to a host after a successful `Hello`.
#[derive(Debug)]
pub struct HostConn {
    /// The QUIC connection.
    pub conn: Connection,
    /// Host identity.
    pub remote: EndpointId,
    /// What the host told us.
    pub ack: HelloAck,
    /// Control stream, client → host.
    pub tx: FramedSend<ClientMsg>,
    /// Control stream, host → client.
    pub rx: FramedRecv<HostMsg>,
}

/// Bind a client endpoint.
pub async fn bind_client(secret: SecretKey, reach: Reach) -> Result<Endpoint, NetError> {
    bind(secret, Role::Client, reach).await
}

/// Connect using a pairing ticket: the token goes into `hello.pair_token`. `reach` must be
/// what `endpoint` was bound with.
pub async fn connect_with_ticket(
    endpoint: &Endpoint,
    reach: Reach,
    ticket: &PairTicket,
    mut hello: Hello,
) -> Result<HostConn, HandshakeError> {
    hello.pair_token = Some(ticket.token);
    connect(endpoint, reach, ticket.addr.clone(), hello).await
}

/// Connect to a host we are already paired with (or with `hello.pair_token` set by the caller).
/// `reach` must be what `endpoint` was bound with.
pub async fn connect(
    endpoint: &Endpoint,
    reach: Reach,
    addr: EndpointAddr,
    hello: Hello,
) -> Result<HostConn, HandshakeError> {
    // A direct-only endpoint has no relays; an address stored by an earlier pairing may still
    // carry one. Drop it so the dial cannot wait on a relay we will never use.
    let addr = if reach.is_direct_only() { direct_only(addr) } else { addr };
    let conn = endpoint.connect(addr, ALPN).await.map_err(|e| NetError::Connect(e.to_string()))?;
    let remote = conn.remote_id();
    let (send, recv) = conn.open_bi().await.map_err(|e| NetError::Stream(e.to_string()))?;
    let mut tx = FramedSend::<ClientMsg>::new(send);
    let mut rx = FramedRecv::<HostMsg>::new(recv);
    tx.send(&ClientMsg::Hello(hello)).await?;
    let reply = tokio::time::timeout(ACK_TIMEOUT, rx.recv())
        .await
        .map_err(|_elapsed| NetError::Protocol("hello ack timeout"))??;
    match reply {
        HostMsg::HelloAck(ack) => Ok(HostConn { conn, remote, ack, tx, rx }),
        HostMsg::Rejected(why) => {
            conn.close(0_u32.into(), b"rejected");
            Err(HandshakeError::Rejected(why))
        }
        _other => Err(NetError::Protocol("expected HelloAck").into()),
    }
}

impl HostConn {
    /// Accept the next session stream the host opens; returns its header and the typed reader.
    pub async fn accept_session_stream(
        &self,
    ) -> Result<(StreamHeader, FramedRecv<TermEvent>), NetError> {
        let recv = self.conn.accept_uni().await.map_err(|e| NetError::Stream(e.to_string()))?;
        let mut header = FramedRecv::<StreamHeader>::new(recv);
        let hdr = header.recv().await?;
        Ok((hdr, header.retype()))
    }

    /// Current RTT on the selected path.
    #[must_use]
    pub fn rtt(&self) -> Option<Duration> {
        crate::endpoint::rtt(&self.conn)
    }
}

/// The same address without its relay entries.
fn direct_only(addr: EndpointAddr) -> EndpointAddr {
    let id = addr.id;
    EndpointAddr::from_parts(id, addr.addrs.into_iter().filter(|a| !a.is_relay()))
}
