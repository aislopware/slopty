//! Networking on the tokio side.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use slopty_client::{HostLink, LinkEvent};
use slopty_core::ClientId;
use slopty_net::client::{bind_client, connect};
use slopty_net::identity::Identity;
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
    pub sender: mpsc::Sender<ClientMsg>,
    pub events: mpsc::Receiver<LinkEvent>,
    /// Keeps the connection's tasks and the endpoint alive; dropping it disconnects.
    pub link: HostLink,
    pub endpoint: slopty_net::Endpoint,
}

fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SLOPTY_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    home.join("Library").join("Application Support").join("Slopty")
}

/// Connect to the single paired host.
pub async fn connect_host() -> Result<Connected> {
    let me = Identity::open(&data_dir().join("client.json"))?;
    let hosts = me.hosts();
    let (_id, host) = match hosts.as_slice() {
        [one] => one.clone(),
        [] => bail!("no paired host; run `slopty pair <ticket>` first"),
        _many => hosts.first().cloned().context("hosts")?,
    };
    let endpoint = bind_client(me.secret().clone()).await?;
    let hello = Hello {
        protocol: slopty_proto::PROTOCOL_VERSION,
        client: me.client(),
        kind: ClientKind::Mac,
        name: "Slopty for Mac".to_owned(),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        caps: Caps::empty(),
        pair_token: None,
    };
    let conn = connect(&endpoint, host.addr.clone(), hello).await?;
    let ack = conn.ack.clone();
    let mut link = HostLink::start(conn);
    let events = link.events().context("events")?;
    let sender = link.sender();
    Ok(Connected { me: me.client(), ack, sender, events, link, endpoint })
}
