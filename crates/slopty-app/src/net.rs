//! Networking on the tokio side.

use anyhow::{Context as _, Result, bail};
use slopty_client::{HostLink, LinkEvent};
use slopty_core::ClientId;
use slopty_net::Reach;
use slopty_net::client::{bind_client, connect, connect_with_ticket};
use slopty_net::identity::{Identity, KnownHost};
use slopty_net::pairing::PairTicket;
use slopty_proto::ClientMsg;
use slopty_proto::handshake::{Caps, ClientKind, Hello, HelloAck};
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
    /// Keeps the connection's tasks and the endpoint alive; dropping it disconnects.
    pub link: HostLink,
    /// Our endpoint; closed on the tokio runtime when the link drops.
    pub endpoint: slopty_net::Endpoint,
}

fn identity() -> Result<Identity> {
    Ok(Identity::open(&slopty_settings::data_dir().join("client.json"))?)
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

/// Whether this installation has at least one paired host.
///
/// # Errors
///
/// When the identity file cannot be read or created.
pub fn is_paired() -> Result<bool> {
    Ok(!identity()?.hosts().is_empty())
}

/// Redeem a pairing ticket (`sloptypair…`, printed by `slopty host ticket`) and remember the
/// host. Returns the host's name.
///
/// # Errors
///
/// When the ticket does not parse, the host cannot be reached, or it rejects the token.
pub async fn pair_host(ticket: &str) -> Result<String> {
    let ticket: PairTicket = ticket.trim().parse().context("parse ticket")?;
    let mut me = identity()?;
    let reach = Reach::from_env();
    let endpoint = bind_client(me.secret().clone(), reach).await?;
    let conn = connect_with_ticket(&endpoint, reach, &ticket, hello(me.client())).await?;
    let name = conn.ack.name.clone();
    let paired_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    me.remember(KnownHost {
        host: conn.ack.host,
        name: name.clone(),
        addr: ticket.addr,
        paired_at,
    })?;
    conn.conn.close(0_u32.into(), b"paired");
    endpoint.close().await;
    Ok(name)
}

/// Connect to the single paired host.
pub async fn connect_host() -> Result<Connected> {
    let me = identity()?;
    let hosts = me.hosts();
    let (_id, host) = match hosts.as_slice() {
        [one] => one.clone(),
        [] => bail!("no paired host"),
        _many => hosts.first().cloned().context("hosts")?,
    };
    let reach = Reach::from_env();
    let endpoint = bind_client(me.secret().clone(), reach).await?;
    let conn = connect(&endpoint, reach, host.addr.clone(), hello(me.client())).await?;
    let ack = conn.ack.clone();
    let mut link = HostLink::start(conn);
    let events = link.events().context("events")?;
    let sender = link.sender();
    Ok(Connected { me: me.client(), ack, sender, events, link, endpoint })
}
