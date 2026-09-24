//! Networking on the tokio side.

use anyhow::{Context as _, Result};
use slopty_client::{HostLink, LinkEvent};
use slopty_core::{ClientId, WorkerId};
use slopty_net::HostAddr;
use slopty_net::client::{bind_client, connect};
use slopty_net::known::{KnownWorker, KnownWorkers};
use slopty_proto::ClientMsg;
use slopty_proto::handshake::{Caps, ClientKind, Hello, HelloAck};
use tokio::sync::mpsc;

/// What the UI needs once the link is up.
#[derive(Debug)]
pub struct Connected {
    /// Our identity on the wire.
    pub me: ClientId,
    /// The worker's greeting (name, live sessions).
    pub ack: HelloAck,
    /// Outbound control messages.
    pub sender: mpsc::Sender<ClientMsg>,
    /// Inbound link events.
    pub events: mpsc::Receiver<LinkEvent>,
    /// Keeps the connection's tasks alive; dropping it disconnects.
    pub link: HostLink,
}

fn known() -> Result<KnownWorkers> {
    Ok(KnownWorkers::open_in(&slopty_settings::data_dir())?)
}

/// The app's one endpoint, bound on first use.
static ENDPOINT: std::sync::OnceLock<slopty_net::Endpoint> = std::sync::OnceLock::new();

/// The process-wide client endpoint: every worker link shares one socket. Bound on the
/// runtime's thread, which the endpoint's driver task needs.
fn endpoint() -> Result<slopty_net::Endpoint, String> {
    if let Some(endpoint) = ENDPOINT.get() {
        return Ok(endpoint.clone());
    }
    let bound = bind_client().map_err(|e| format!("{e:#}"))?;
    // A racing first call binds a second socket and drops it; the stored one wins.
    Ok(ENDPOINT.get_or_init(|| bound).clone())
}

/// Our greeting: which app this is, by platform.
fn hello(client: ClientId) -> Hello {
    #[cfg(target_os = "ios")]
    let (kind, name) = (ClientKind::IPhone, "Slopty for iPhone");
    #[cfg(not(target_os = "ios"))]
    let (kind, name) = (ClientKind::Mac, "Slopty for Mac");
    Hello {
        protocol: slopty_proto::PROTOCOL_VERSION,
        client,
        kind,
        name: name.to_owned(),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        caps: Caps::empty(),
    }
}

/// The workers this installation has added, by name.
///
/// # Errors
///
/// When the store cannot be read or created.
pub fn known_workers() -> Result<Vec<KnownWorker>> {
    let mut workers = known()?.workers().to_vec();
    workers.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(workers)
}

/// Drop a worker from the store.
///
/// # Errors
///
/// When the store cannot be read or written.
pub fn forget_worker(id: WorkerId) -> Result<bool> {
    Ok(known()?.forget(id)?)
}

/// A worker just added.
#[derive(Clone, Debug)]
pub struct Added {
    /// Its identity (the key of the store and of its slot).
    pub id: WorkerId,
    /// Its display name.
    pub name: String,
}

/// Connect to `address` (`host[:port]`) and remember the worker under the id it answers with.
///
/// # Errors
///
/// When the address does not parse, nothing answers there, or it answers with another
/// protocol version.
pub async fn add_worker(address: &str) -> Result<Added> {
    let address: HostAddr = address.trim().parse()?;
    let mut me = known()?;
    let endpoint = endpoint().map_err(anyhow::Error::msg)?;
    let conn = connect(&endpoint, &address, hello(me.client()))
        .await
        .with_context(|| format!("connect to {address}"))?;
    let added = Added { id: conn.ack.worker, name: conn.ack.name.clone() };
    me.remember(KnownWorker { address, name: added.name.clone(), worker_id: added.id })?;
    conn.close();
    Ok(added)
}

/// Connect to one known worker on the shared endpoint. The error is a line for the status.
pub async fn connect_to(id: WorkerId) -> Result<Connected, String> {
    let other = |e: &dyn std::fmt::Display| format!("{e:#}");
    let mut me = known().map_err(|e| other(&e))?;
    let worker = me.get(id).cloned().ok_or_else(|| "worker forgotten".to_owned())?;
    let endpoint = endpoint()?;
    tracing::debug!(worker = %id, address = %worker.address, "dialing");
    let conn =
        connect(&endpoint, &worker.address, hello(me.client())).await.map_err(|e| other(&e))?;
    let ack = conn.ack.clone();
    if ack.worker != id {
        conn.close();
        return Err(format!("{} now answers as another worker; add it again", worker.address));
    }
    tracing::debug!(worker = %id, name = %ack.name, sessions = ack.sessions.len(), "connected");
    // The switcher shows the stored name until the link is up; keep it current.
    if ack.name != worker.name
        && let Err(e) = me.remember(KnownWorker { name: ack.name.clone(), ..worker })
    {
        tracing::warn!(error = %e, "refresh worker name");
    }
    let mut link = HostLink::start(conn);
    let events = link.events().ok_or_else(|| "events".to_owned())?;
    let sender = link.sender();
    Ok(Connected { me: me.client(), ack, sender, events, link })
}
