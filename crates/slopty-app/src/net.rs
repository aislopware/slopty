//! Networking on the tokio side.

use anyhow::{Context as _, Result};
use slopty_client::{HostLink, LinkEvent};
use slopty_core::ClientId;
use slopty_net::client::{HandshakeError, bind_client, connect, connect_with_ticket};
use slopty_net::identity::{Identity, KnownHost};
use slopty_net::pairing::PairTicket;
use slopty_net::{EndpointId, Reach};
use slopty_proto::ClientMsg;
use slopty_proto::handshake::{Caps, ClientKind, Hello, HelloAck, Rejection};
use tokio::sync::mpsc;

/// What the UI needs once the link is up.
#[derive(Debug)]
pub struct Connected {
    /// Our identity on the wire.
    pub me: ClientId,
    /// The host's greeting (name, live sessions).
    pub ack: HelloAck,
    /// Outbound control messages.
    pub sender: mpsc::Sender<ClientMsg>,
    /// Inbound link events.
    pub events: mpsc::Receiver<LinkEvent>,
    /// Keeps the connection's tasks alive; dropping it disconnects.
    pub link: HostLink,
}

fn identity() -> Result<Identity> {
    Ok(Identity::open(&slopty_settings::data_dir().join("client.json"))?)
}

/// The app's one endpoint, bound on first use with the installation's key.
static ENDPOINT: tokio::sync::OnceCell<slopty_net::Endpoint> = tokio::sync::OnceCell::const_new();

/// The process-wide client endpoint (pairing and every host link share it).
///
/// One endpoint per process is how iroh is meant to be used: connections come and go on it
/// independently, the relay handshake happens once, and the host keeps seeing us from the
/// same socket. Binding a fresh endpoint per attempt with the same key made the dial right
/// after pairing hang (observed 2026-09-05: the host never saw the second endpoint's packets).
async fn endpoint(me: &Identity, reach: Reach) -> Result<slopty_net::Endpoint, ConnectError> {
    ENDPOINT
        .get_or_try_init(|| bind_client(me.secret().clone(), reach))
        .await
        .cloned()
        .map_err(|e| ConnectError::Other(format!("{e:#}")))
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
        pair_token: None,
    }
}

/// The hosts this installation is paired with, by name.
///
/// # Errors
///
/// When the identity file cannot be read or created.
pub fn known_hosts() -> Result<Vec<(EndpointId, KnownHost)>> {
    let mut hosts = identity()?.hosts();
    hosts.sort_by(|a, b| a.1.name.cmp(&b.1.name).then_with(|| a.1.paired_at.cmp(&b.1.paired_at)));
    Ok(hosts)
}

/// Drop a host from the pairing store.
///
/// # Errors
///
/// When the identity file cannot be read or written.
pub fn forget_host(id: EndpointId) -> Result<bool> {
    Ok(identity()?.forget(&id)?)
}

/// A host just paired with.
#[derive(Clone, Debug)]
pub struct Paired {
    /// Its transport identity (the key of the pairing store).
    pub id: EndpointId,
    /// Its display name.
    pub name: String,
}

/// Why a connection attempt failed, as far as the chrome cares.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// The host no longer knows us: the pairing must be redone.
    #[error("not paired with this host")]
    NotPaired,
    /// Anything else (unreachable, timed out, protocol mismatch); retried.
    #[error("{0}")]
    Other(String),
}

/// Redeem a pairing ticket (`sloptypair…`, printed by `slopty host ticket`) and remember the
/// host.
///
/// # Errors
///
/// When the ticket does not parse, the host cannot be reached, or it rejects the token.
pub async fn pair_host(ticket: &str) -> Result<Paired> {
    let ticket: PairTicket = ticket.trim().parse().context("parse ticket")?;
    let mut me = identity()?;
    let reach = Reach::from_env();
    let endpoint = endpoint(&me, reach).await?;
    let conn = connect_with_ticket(&endpoint, reach, &ticket, hello(me.client())).await?;
    let name = conn.ack.name.clone();
    let paired_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let id = ticket.addr.id;
    me.remember(KnownHost {
        host: conn.ack.host,
        name: name.clone(),
        addr: ticket.addr,
        paired_at,
    })?;
    conn.conn.close(0_u32.into(), b"paired");
    Ok(Paired { id, name })
}

/// Connect to one paired host on the shared endpoint.
///
/// # Errors
///
/// [`ConnectError::NotPaired`] when the host rejects us as unknown; everything else is
/// [`ConnectError::Other`].
pub async fn connect_to(id: EndpointId) -> Result<Connected, ConnectError> {
    let other = |e: &dyn std::fmt::Display| ConnectError::Other(format!("{e:#}"));
    let mut me = identity().map_err(|e| other(&e))?;
    let (_id, host) = me
        .hosts()
        .into_iter()
        .find(|(known, _)| *known == id)
        .ok_or_else(|| ConnectError::Other("host forgotten".to_owned()))?;
    let reach = Reach::from_env();
    let endpoint = endpoint(&me, reach).await?;
    tracing::debug!(host = %id, addr = ?host.addr, "dialing");
    let conn = match connect(&endpoint, reach, host.addr.clone(), hello(me.client())).await {
        Ok(conn) => conn,
        Err(HandshakeError::Rejected(Rejection::NotPaired)) => {
            return Err(ConnectError::NotPaired);
        }
        Err(e) => return Err(other(&e)),
    };
    let ack = conn.ack.clone();
    tracing::debug!(host = %id, name = %ack.name, sessions = ack.sessions.len(), "connected");
    // The switcher shows the stored name until the link is up; keep it current.
    if ack.name != host.name
        && let Err(e) = me.remember(KnownHost { name: ack.name.clone(), ..host })
    {
        tracing::warn!(error = %e, "refresh host name");
    }
    let mut link = HostLink::start(conn);
    let events = link.events().ok_or_else(|| ConnectError::Other("events".to_owned()))?;
    let sender = link.sender();
    Ok(Connected { me: me.client(), ack, sender, events, link })
}
