//! The accept side both listeners share: each incoming peer on a task of its own, so a peer
//! that connects and says nothing, or a slow word from Tailscale, holds up nobody behind it.
//! A peer [`Admission`] refuses gets its refusal at its first packet. A tailnet node granted no
//! role here finishes the handshake and is closed with [`close_code::NOT_GRANTED`] at once, so
//! it hears that a grant is missing rather than nothing at all. The rest are greeted. A greeting
//! is the QUIC handshake, the control stream the peer opens, the wire prefixes each way (a peer
//! on another build is closed there, `crate::prefix`), and its first message, which must be its
//! hello. A discovery probe (`crate::discover`) goes as far as the prefixes and closes with
//! [`crate::discover::PROBE_REASON`] wherever it stops.

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
pub(crate) struct Greeted<S, R, H> {
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
pub(crate) fn spawn<S, R, H>(
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
                Verdict::Ungranted => return not_granted(incoming, who).await,
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
                Err(NetError::WrongBuild(wrong)) => {
                    let peer_build = wrong.peer_build();
                    let this = slopty_proto::wire::BUILD;
                    tracing::warn!(%peer, peer_build, this, "{who} runs a different build; closed");
                }
                Err(e) => tracing::info!(%peer, error = %e, "{who} dropped"),
            }
        });
    }
}

/// Finish the handshake with a tailnet node granted no role here, then close with
/// [`close_code::NOT_GRANTED`]: a client reads it as `NetError::NotGranted`, a probe as a node
/// that needs a grant.
pub(crate) async fn not_granted(incoming: noq::Incoming, who: &'static str) {
    let peer = crate::endpoint::canonical(incoming.remote_address());
    match tokio::time::timeout(HELLO_TIMEOUT, incoming).await {
        Ok(Ok(conn)) => {
            tracing::info!(%peer, "{who} refused: the tailnet grants it no role here");
            conn.close(close_code::NOT_GRANTED.into(), b"not granted");
        }
        Ok(Err(e)) => tracing::debug!(%peer, error = %e, "{who} gone before its refusal"),
        Err(_elapsed) => tracing::debug!(%peer, "{who} never finished the handshake"),
    }
}

/// Whether the peer closed `conn` as a discovery probe does.
fn probed(conn: &Connection) -> bool {
    matches!(
        conn.close_reason(),
        Some(noq::ConnectionError::ApplicationClosed(close))
            if close.reason.as_ref() == crate::discover::PROBE_REASON
    )
}

/// Finish the handshake and read the hello; `None` for a discovery probe
/// (`crate::discover`), which closes once it has read this end's prefix, or sooner.
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
    let mut tx = FramedSend::<S>::new(send);
    let mut rx = FramedRecv::<R>::new(recv);
    let first = async {
        crate::prefix::say(&mut tx).await?;
        crate::prefix::check(&conn, &mut rx, HELLO_TIMEOUT).await?;
        tokio::time::timeout(HELLO_TIMEOUT, rx.recv())
            .await
            .map_err(|_elapsed| NetError::Protocol("hello timeout"))?
    };
    let first = match first.await {
        Ok(first) => first,
        Err(_) if probed(&conn) => return Ok(None),
        Err(e) => return Err(e),
    };
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

    /// A discovery probe, which closes once the handshake is done or once it has read this
    /// end's prefix, is told apart from a peer that went away before its hello: the listener
    /// logs it as a probe.
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
        let listener = listening.clone();
        let greeting = tokio::spawn(async move {
            let incoming = listener.accept().await.unwrap();
            greet::<WorkerMsg, ClientMsg, Hello>(incoming, hello, Grant::ALL).await.unwrap()
        });
        let found = crate::discover::Found {
            name: "here".to_owned(),
            addr: at,
            tags: Vec::new(),
            answer: None,
        };
        let answered = crate::discover::answering_workers(&dialing, vec![found]).await;
        assert_eq!(answered[0].answer, Some(crate::discover::Answer::Ready));
        assert!(greeting.await.unwrap().is_none(), "a probe past the prefixes is a probe too");
    }
}
