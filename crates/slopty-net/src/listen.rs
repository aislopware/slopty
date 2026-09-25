//! The accept side both listeners share: refuse a peer outside the admitted ranges at its first
//! packet, and greet the rest each on a task of its own, so a peer that connects and says nothing
//! holds up nobody behind it. A greeting is the QUIC handshake, the control stream the peer
//! opens, and its first message, which must be its hello.

use std::net::SocketAddr;
use std::time::Duration;

use noq::{Connection, Endpoint};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::mpsc;

use crate::NetError;
use crate::admission::Admission;
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
        if !admission.admits(peer.ip()) {
            tracing::info!(%peer, "refused: outside the admitted ranges");
            incoming.refuse();
            continue;
        }
        let greeted = greeted.clone();
        tokio::spawn(async move {
            match greet(incoming, hello).await {
                Ok(peer) => {
                    let _sent = greeted.send(peer).await;
                }
                Err(e) => tracing::info!(%peer, error = %e, "{who} dropped"),
            }
        });
    }
}

/// Finish the handshake and read the hello.
async fn greet<S, R, H>(
    incoming: noq::Incoming,
    hello: fn(R) -> Option<H>,
) -> Result<Greeted<S, R, H>, NetError>
where
    S: Serialize,
    R: DeserializeOwned,
{
    let remote = crate::endpoint::canonical(incoming.remote_address());
    let conn = incoming.await.map_err(|e| NetError::Connect(e.to_string()))?;
    let (send, recv) = tokio::time::timeout(HELLO_TIMEOUT, conn.accept_bi())
        .await
        .map_err(|_elapsed| NetError::Protocol("no control stream"))?
        .map_err(|e| NetError::stream(&e))?;
    let tx = FramedSend::<S>::new(send);
    let mut rx = FramedRecv::<R>::new(recv);
    let first = tokio::time::timeout(HELLO_TIMEOUT, rx.recv())
        .await
        .map_err(|_elapsed| NetError::Protocol("hello timeout"))??;
    let Some(hello) = hello(first) else {
        conn.close(close_code::PROTOCOL.into(), b"hello first");
        return Err(NetError::Protocol("first message must be Hello"));
    };
    Ok(Greeted { conn, remote, hello, tx, rx })
}
