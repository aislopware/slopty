//! Networking on the tokio side.

use anyhow::{Context as _, Result};
use slopty_client::server::{ServerEvent, ServerTask};
use slopty_client::{LinkEvent, WorkerLink};
use slopty_core::{ClientId, WorkerId};
use slopty_net::HostAddr;
use slopty_net::client::{bind_client, connect};
use slopty_net::known::{KnownWorker, KnownWorkers};
use slopty_net::server::ServerLink;
use slopty_proto::ClientMsg;
use slopty_proto::handshake::{Caps, ClientKind, Hello, HelloAck};
use slopty_proto::server::Role;
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
    pub link: WorkerLink,
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
/// When the address does not parse, nothing answers there, or the worker turns it away.
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

/// Connect to worker `id` on the shared endpoint: at `address` (the directory's), else at the
/// address it was added with. The error is a line for the status.
pub async fn connect_to(id: WorkerId, address: Option<HostAddr>) -> Result<Connected, String> {
    let other = |e: &dyn std::fmt::Display| format!("{e:#}");
    let mut me = known().map_err(|e| other(&e))?;
    let added = me.get(id).cloned();
    let address = match (address, &added) {
        (Some(address), _) => address,
        (None, Some(added)) => added.address.clone(),
        (None, None) => return Err("worker forgotten".to_owned()),
    };
    let endpoint = endpoint()?;
    tracing::debug!(worker = %id, %address, "dialing");
    let conn = connect(&endpoint, &address, hello(me.client())).await.map_err(|e| other(&e))?;
    let ack = conn.ack.clone();
    if ack.worker != id {
        conn.close();
        return Err(format!("{address} now answers as another worker"));
    }
    tracing::debug!(worker = %id, name = %ack.name, sessions = ack.sessions.len(), "connected");
    // A worker added by address shows the stored name until the link is up; keep it current.
    if let Some(added) = added
        && ack.name != added.name
        && let Err(e) = me.remember(KnownWorker { name: ack.name.clone(), ..added })
    {
        tracing::warn!(error = %e, "refresh worker name");
    }
    let mut link = WorkerLink::start_forwarding(conn);
    let events = link.events().ok_or_else(|| "events".to_owned())?;
    let sender = link.sender();
    Ok(Connected { me: me.client(), ack, sender, events, link })
}

/// Whether worker `id` was added by address (rather than listed by the server).
#[must_use]
pub fn is_added(id: WorkerId) -> bool {
    known().is_ok_and(|k| k.get(id).is_some())
}

/// Who this app is to the server.
fn server_role(client: ClientId) -> Role {
    let Hello { client, kind, name, .. } = hello(client);
    Role::Client { client, kind, name }
}

/// Dial the server at `address` once, to prove it answers before it is saved; the link goes on
/// to [`serve_directory`].
///
/// # Errors
///
/// When nothing answers there, or it refuses this app.
pub async fn link_server(address: &HostAddr) -> Result<ServerLink> {
    let endpoint = endpoint().map_err(anyhow::Error::msg)?;
    let role = server_role(known()?.client());
    slopty_net::server::connect(&endpoint, address, role)
        .await
        .with_context(|| format!("connect to {address}"))
}

/// Keep a link to the server at `address` on this runtime, starting with `first` when the
/// address was just proven. Must run on the runtime.
///
/// # Errors
///
/// When the endpoint cannot be bound or the store read.
pub fn serve_directory(
    address: HostAddr,
    first: Option<ServerLink>,
) -> Result<(ServerTask, mpsc::Receiver<ServerEvent>), String> {
    let endpoint = endpoint()?;
    let role = server_role(known().map_err(|e| format!("{e:#}"))?.client());
    let runtime = tokio::runtime::Handle::current();
    Ok(slopty_client::server::spawn(&runtime, endpoint, address, role, first))
}
