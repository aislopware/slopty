//! The one link a client keeps to the server: dialled at start, redialled forever after it
//! drops, every message it carries handed on as a [`ServerEvent`].
//!
//! The server is the control plane only, so nothing here is on a terminal's or a stream's
//! path: while this link is down the workers are still dialled directly
//! ([`crate::directory`]).

use slopty_net::HostAddr;
use slopty_net::server::{DialError, ServerLink, connect};
use slopty_proto::server::{FromServer, Role};
use tokio::sync::mpsc;

/// Server messages queued for the UI at most.
const EVENT_DEPTH: usize = 256;

/// What the link says.
#[derive(Debug)]
pub enum ServerEvent {
    /// The server welcomed this client.
    Linked {
        /// Its name.
        name: String,
    },
    /// A message from it: the directory, a worker's change, an event.
    Message(Box<FromServer>),
    /// A dial failed or the link dropped; the next attempt is on its way.
    Unlinked {
        /// Why.
        why: String,
    },
}

/// The running link; dropping it closes the link and ends the redials.
#[derive(Debug)]
pub struct ServerTask {
    task: tokio::task::JoinHandle<()>,
}

impl Drop for ServerTask {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Dial the server at `addr` as `role` on `endpoint` and keep dialling after every drop, on
/// `runtime`. `first` is a link already open (the one that proved the address), used before
/// any dial.
#[must_use]
pub fn spawn(
    runtime: &tokio::runtime::Handle,
    endpoint: slopty_net::Endpoint,
    addr: HostAddr,
    role: Role,
    first: Option<ServerLink>,
) -> (ServerTask, mpsc::Receiver<ServerEvent>) {
    let (tx, rx) = mpsc::channel(EVENT_DEPTH);
    let task = runtime.spawn(run(endpoint, addr, role, first, tx));
    (ServerTask { task }, rx)
}

async fn run(
    endpoint: slopty_net::Endpoint,
    addr: HostAddr,
    role: Role,
    mut first: Option<ServerLink>,
    tx: mpsc::Sender<ServerEvent>,
) {
    let mut redial = crate::redial::Redial::default();
    loop {
        let dialled = match first.take() {
            Some(link) => Ok(link),
            None => connect(&endpoint, &addr, role.clone()).await,
        };
        let why = match dialled {
            Ok(link) => {
                redial.linked(std::time::Instant::now());
                let Some(why) = pump(link, &tx).await else { return };
                why
            }
            Err(DialError::Refused(why)) => format!("refused: {why:?}"),
            Err(DialError::Net(e)) => e.to_string(),
        };
        tracing::debug!(server = %addr, %why, "server link down");
        if tx.send(ServerEvent::Unlinked { why }).await.is_err() {
            return;
        }
        tokio::time::sleep(redial.next(std::time::Instant::now())).await;
    }
}

/// Hand on everything the link carries until it ends; the reason, or `None` once nobody
/// listens.
async fn pump(mut link: ServerLink, tx: &mpsc::Sender<ServerEvent>) -> Option<String> {
    tracing::debug!(server = %link.remote, name = %link.name, "server linked");
    if tx.send(ServerEvent::Linked { name: link.name.clone() }).await.is_err() {
        link.close();
        return None;
    }
    loop {
        match link.rx.recv().await {
            Ok(msg) => {
                if tx.send(ServerEvent::Message(Box::new(msg))).await.is_err() {
                    link.close();
                    return None;
                }
            }
            Err(e) => return Some(e.to_string()),
        }
    }
}
