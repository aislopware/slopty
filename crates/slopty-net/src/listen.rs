//! The accept side both listeners share: each incoming peer on a task of its own, so a peer
//! that connects and says nothing, or a slow word from Tailscale, holds up nobody behind it.
//! A peer [`Admission`] refuses gets its refusal at its first packet; the rest are greeted. A
//! greeting is the QUIC handshake, the control stream the peer opens, and its first message,
//! which must be its hello.

use std::net::SocketAddr;
use std::time::Duration;

use noq::{Connection, Endpoint};
use serde::Serialize;
use serde::de::DeserializeOwned;
use slopty_tailnet::Grant;
use tokio::sync::mpsc;

use crate::NetError;
use crate::admission::{Admission, Verdict};
use crate::framed::{FramedRecv, FramedSend};
use crate::worker::close_code;

/// How long a peer has to open its control stream and say hello after connecting.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// Greeted peers queued for the listener's `accept`.
const GREETED_DEPTH: usize = 8;

/// A peer that said hello: `H` taken out of its first message `R`, with the stream it came on.
#[derive(Debug)]
pub struct Greeted<S, R, H> {
    pub conn: Connection,
    pub remote: SocketAddr,
    pub hello: H,
    pub grant: Grant,
    pub tx: FramedSend<S>,
    pub rx: FramedRecv<R>,
}

/// Run the accept loop on `endpoint` as a task of its own; the greeted peers come out of the
/// returned queue. `hello` takes a first message apart, `None` when it is not a hello; `who`
/// names the peers in the log.
pub fn spawn<S, R, H>(
    endpoint: Endpoint,
    admission: Admission,
    hello: fn(R) -> Option<H>,
    who: &'static str,
) -> mpsc::Receiver<Greeted<S, R, H>>
where
    S: Serialize + Send + 'static,
    R: DeserializeOwned + Send + 'static,
    H: Send + 'static,
{
    let (tx, rx) = mpsc::channel(GREETED_DEPTH);
    tokio::spawn(admit(endpoint, admission, hello, who, tx));
    rx
}

async fn admit<S, R, H>(
    endpoint: Endpoint,
    admission: Admission,
    hello: fn(R) -> Option<H>,
    who: &'static str,
    greeted: mpsc::Sender<Greeted<S, R, H>>,
) where
    S: Serialize + Send + 'static,
    R: DeserializeOwned + Send + 'static,
    H: Send + 'static,
{
    while let Some(incoming) = endpoint.accept().await {
        let peer = crate::endpoint::canonical(incoming.remote_address());
        let (admission, greeted) = (admission.clone(), greeted.clone());
        tokio::spawn(async move {
            let grant = match admission.check(peer).await {
                Verdict::Admit(grant) => grant,
                Verdict::Refuse(why) => {
                    tracing::info!(%peer, why, "{who} refused");
                    incoming.refuse();
                    return;
                }
            };
            match greet(incoming, hello, grant).await {
                Ok(Some(peer)) => {
                    let _sent = greeted.send(peer).await;
                }
                Ok(None) => tracing::debug!(%peer, "{who} probe"),
                Err(e) => tracing::info!(%peer, error = %e, "{who} dropped"),
            }
        });
    }
}

/// Finish the handshake and read the hello; `None` for a discovery probe
/// (`crate::discover`), which closes as soon as the handshake is done.
async fn greet<S, R, H>(
    incoming: noq::Incoming,
    hello: fn(R) -> Option<H>,
    grant: Grant,
) -> Result<Option<Greeted<S, R, H>>, NetError>
where
    S: Serialize,
    R: DeserializeOwned,
{
    let remote = crate::endpoint::canonical(incoming.remote_address());
    let conn = incoming.await.map_err(|e| NetError::Connect(e.to_string()))?;
    let (send, recv) = match tokio::time::timeout(HELLO_TIMEOUT, conn.accept_bi()).await {
        Err(_elapsed) => return Err(NetError::Protocol("no control stream")),
        Ok(Err(noq::ConnectionError::ApplicationClosed(close)))
            if close.reason.as_ref() == crate::discover::PROBE_REASON =>
        {
            return Ok(None);
        }
        Ok(Err(e)) => return Err(NetError::stream(&e)),
        Ok(Ok(streams)) => streams,
    };
    let tx = FramedSend::<S>::new(send);
    let mut rx = FramedRecv::<R>::new(recv);
    let first = tokio::time::timeout(HELLO_TIMEOUT, rx.recv())
        .await
        .map_err(|_elapsed| NetError::Protocol("hello timeout"))??;
    let Some(hello) = hello(first) else {
        conn.close(close_code::PROTOCOL.into(), b"hello first");
        return Err(NetError::Protocol("first message must be Hello"));
    };
    Ok(Some(Greeted { conn, remote, hello, grant, tx, rx }))
}

#[cfg(test)]
mod tests {
    use slopty_proto::handshake::Hello;
    use slopty_proto::{ClientMsg, WorkerMsg};

    use super::*;

    fn hello(first: ClientMsg) -> Option<Hello> {
        match first {
            ClientMsg::Hello(hello) => Some(hello),
            _ => None,
        }
    }

    /// A discovery probe, which closes once the handshake is done, is told apart from a peer
    /// that went away before its hello: the listener logs it as a probe.
    #[tokio::test]
    async fn a_probe_is_not_a_dropped_peer() {
        let listening = crate::endpoint::bind("127.0.0.1:0".parse().unwrap(), true).unwrap();
        let at = listening.local_addr().unwrap();
        let dialing = crate::client::bind_client().unwrap();
        for (reason, probe) in [(crate::discover::PROBE_REASON, true), (&b"bye"[..], false)] {
            let listener = listening.clone();
            let greeting = tokio::spawn(async move {
                let incoming = listener.accept().await.unwrap();
                let greeted = greet::<WorkerMsg, ClientMsg, Hello>(incoming, hello, Grant::ALL);
                greeted.await.map(|peer| peer.is_some())
            });
            let conn = crate::client::dial(&dialing, at, "probe", None).await.unwrap();
            conn.close(close_code::NORMAL.into(), reason);
            match greeting.await.unwrap() {
                Ok(false) => assert!(probe, "{reason:?} read as a probe"),
                Err(_) => assert!(!probe, "{reason:?} read as a dropped peer"),
                Ok(true) => panic!("no hello was sent"),
            }
        }
    }
}
