//! Networking on the tokio side.

use anyhow::{Result, anyhow};
use slopty_client::server::{ServerEvent, ServerTask};
use slopty_client::{LinkEvent, WorkerLink};
use slopty_core::{ClientId, WorkerId};
use slopty_net::HostAddr;
use slopty_net::client::{bind_client, connect};
use slopty_net::server::{DialError, ServerLink};
use slopty_proto::ClientMsg;
use slopty_proto::handshake::{Hello, HelloAck};
use slopty_proto::server::{Refusal, Role};
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

/// This installation's id on the wire, kept in the data directory.
pub(crate) fn client_id() -> Result<ClientId> {
    Ok(slopty_net::known::client_id_in(&slopty_platform::dirs::data_dir())?)
}

/// The app's one endpoint, bound on first use.
static ENDPOINT: std::sync::OnceLock<slopty_net::Endpoint> = std::sync::OnceLock::new();

/// The network path moved under the app: every connection on its endpoint migrates to it.
///
/// That is RFC 9000's migration (§9, noq's `Endpoint::handle_network_change`): each connection
/// takes its local address afresh and pings over the new path. Nothing when no endpoint is bound
/// yet.
pub fn path_changed() {
    if let Some(endpoint) = ENDPOINT.get() {
        endpoint.handle_network_change(None);
    }
}

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

/// What answered on the tailnet: the Slopty servers and the workers, each best first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tailnet {
    /// Nodes that answered a server's handshake.
    pub servers: Vec<slopty_net::discover::Found>,
    /// Nodes that answered a worker's handshake.
    pub workers: Vec<slopty_net::discover::Found>,
    /// This machine's Tailscale answered and is up, so an empty answer means nothing there
    /// runs Slopty rather than that nothing was asked.
    pub running: bool,
}

/// The servers and workers this machine's Tailscale finds on the tailnet, for the panel.
///
/// Both are looked for at once. Nothing is found while Tailscale is not up here, nor on iOS,
/// where no app can read it.
pub async fn find_on_tailnet() -> Tailnet {
    let Some(status) = tailnet_status().await.filter(slopty_tailnet::Status::running) else {
        return Tailnet::default();
    };
    let Ok(endpoint) = endpoint() else {
        return Tailnet { running: true, ..Tailnet::default() };
    };
    let (servers, workers) = tokio::join!(
        slopty_net::discover::servers(&endpoint, &status),
        slopty_net::discover::workers(&endpoint, &status),
    );
    Tailnet { servers, workers, running: true }
}

/// The tailnet as this machine's Tailscale describes it, if one this process can read runs; in
/// the e2e build, the stand-in the harness names instead (`slopty_e2e::TAILNET_STATUS_ENV`).
async fn tailnet_status() -> Option<slopty_tailnet::Status> {
    #[cfg(feature = "e2e")]
    if let Some(path) = std::env::var_os(slopty_e2e::TAILNET_STATUS_ENV) {
        let text = std::fs::read(path).ok()?;
        return serde_json::from_slice(&text).ok();
    }
    slopty_tailnet::LocalApi::find()?.status().await.ok()
}

/// This machine's name on the tailnet: its `MagicDNS` name, or its tailnet address where
/// `MagicDNS` is off; `None` with no tailnet up here.
pub(crate) async fn tailnet_name() -> Option<String> {
    let me = tailnet_status().await.filter(slopty_tailnet::Status::running)?.me?;
    match me.name() {
        "" => me.ipv4().map(|ip| ip.to_string()),
        name => Some(name.to_owned()),
    }
}

/// Which app this is, by platform, as workers and the server show it.
#[cfg(target_os = "ios")]
const NAME: &str = "Slopty for iPhone";
#[cfg(not(target_os = "ios"))]
const NAME: &str = "Slopty for Mac";

/// Our greeting.
fn hello(client: ClientId) -> Hello {
    Hello { client, name: NAME.to_owned() }
}

/// Why a dial to a worker failed, by the kind a person is told: the raw chain goes to the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialFailed {
    /// The worker turned this device away: the tailnet policy grants it no client role there.
    NotGranted,
    /// The worker runs a different build: what to tell the person, and what updates it.
    WrongBuild(slopty_client::update::UpdateNotice),
    /// Nothing answered at its address.
    NoAnswer,
    /// Its name does not resolve.
    NoSuchHost,
    /// It turned this device away by its `[worker] allow` ranges: this device's address on the
    /// path to it, when the route can be told.
    Refused(Option<std::net::IpAddr>),
    /// The link was made and then ended.
    Dropped,
    /// Anything else, as a line for the status.
    Other(String),
}

impl DialFailed {
    /// `e` as a person is told it: its kind ([`slopty_net::NetError::unreached`]), or the
    /// failure's own words where it is about this device and not the machine.
    fn from_net(host: &str, e: &slopty_net::NetError) -> Self {
        use slopty_net::Unreached;
        tracing::info!(%host, error = %format!("{e:#}"), "dial failed");
        match e.unreached() {
            Some(Unreached::NotGranted) => Self::NotGranted,
            Some(Unreached::WrongBuild) => {
                slopty_client::update::UpdateNotice::for_worker_dial(host, e)
                    .map_or_else(|| Self::Other(format!("{e:#}")), Self::WrongBuild)
            }
            Some(Unreached::NoAnswer) => Self::NoAnswer,
            Some(Unreached::NoSuchHost) => Self::NoSuchHost,
            Some(Unreached::Refused) => Self::Refused(match e {
                slopty_net::NetError::Refused(peer) => peer.parse().ok().and_then(source_toward),
                _ => None,
            }),
            Some(Unreached::Dropped) => Self::Dropped,
            None => Self::Other(format!("{e:#}")),
        }
    }
}

/// This device's address on the route to `peer`: what a machine that admits by address sees.
/// A connected UDP socket asks the routing table and sends nothing.
fn source_toward(peer: std::net::SocketAddr) -> Option<std::net::IpAddr> {
    let any: std::net::SocketAddr = if peer.is_ipv4() {
        (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let socket = std::net::UdpSocket::bind(any).ok()?;
    socket.connect(peer).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified()).then_some(ip)
}

/// Connect to worker `id` at `address` (the directory's) on the shared endpoint.
pub async fn connect_to(id: WorkerId, address: HostAddr) -> Result<Connected, DialFailed> {
    let other = |e: &dyn std::fmt::Display| DialFailed::Other(format!("{e:#}"));
    let me = client_id().map_err(|e| other(&e))?;
    let endpoint = endpoint().map_err(DialFailed::Other)?;
    tracing::debug!(worker = %id, %address, "dialing");
    let conn = connect(&endpoint, &address, hello(me))
        .await
        .map_err(|e| DialFailed::from_net(address.host(), &e))?;
    let ack = conn.ack.clone();
    if ack.worker != id {
        conn.close();
        return Err(DialFailed::Other(format!("{address} now answers as another machine")));
    }
    tracing::debug!(worker = %id, name = %ack.name, sessions = ack.sessions.len(), "connected");
    let mut link = WorkerLink::start_forwarding(conn);
    let events = link.events().ok_or_else(|| DialFailed::Other("events".to_owned()))?;
    let sender = link.sender();
    Ok(Connected { me, ack, sender, events, link })
}

/// Who this app is to the server.
fn server_role() -> Role {
    Role::Client { name: NAME.to_owned() }
}

/// Dial the server at `address` once, to prove it answers before it is saved; the link goes on
/// to [`serve_directory`].
///
/// # Errors
///
/// When nothing answers there, or it refuses this app.
pub async fn link_server(address: &HostAddr) -> Result<ServerLink> {
    let endpoint = endpoint().map_err(anyhow::Error::msg)?;
    slopty_net::server::connect(&endpoint, address, server_role()).await.map_err(|e| match e {
        DialError::Refused(Refusal::NotGranted)
        | DialError::Net(slopty_net::NetError::NotGranted) => {
            anyhow!("{}. {}", crate::server::NOT_GRANTED, crate::server::GRANT_WHERE)
        }
        DialError::Refused(why) => anyhow!(why.text()),
        DialError::Net(e) => anyhow::Error::new(e).context(format!("connect to {address}")),
    })
}

/// Keep a link to the server at `address` on this runtime, starting with `first` when the
/// address was just proven. Must run on the runtime.
///
/// # Errors
///
/// When the endpoint cannot be bound.
pub fn serve_directory(
    address: HostAddr,
    first: Option<ServerLink>,
) -> Result<(ServerTask, mpsc::Receiver<ServerEvent>), String> {
    let endpoint = endpoint()?;
    let runtime = tokio::runtime::Handle::current();
    Ok(slopty_client::server::spawn(&runtime, endpoint, address, server_role(), first))
}

/// A link to the server at `address` on an endpoint of its own. Must run on the runtime.
///
/// It is for work on a runtime that ends with it. The process's endpoint is bound to the
/// app's runtime, and one first bound on a short-lived runtime would die with it.
///
/// # Errors
///
/// When the endpoint cannot be bound.
pub fn serve_alone(address: HostAddr) -> Result<(ServerTask, mpsc::Receiver<ServerEvent>), String> {
    let endpoint = bind_client().map_err(|e| format!("{e:#}"))?;
    let runtime = tokio::runtime::Handle::current();
    Ok(slopty_client::server::spawn(&runtime, endpoint, address, server_role(), None))
}

#[cfg(test)]
mod tests {
    use slopty_net::NetError;

    use super::*;

    /// A failed dial is told by its kind, not its words: a refusal carries this device's address
    /// on the route to the machine, and only a failure of this device's own keeps its text.
    #[test]
    fn a_failed_dial_is_told_by_its_kind() {
        let from = |e: NetError| DialFailed::from_net("studio", &e);
        let loopback = std::net::IpAddr::from([127, 0, 0, 1]);
        assert_eq!(
            from(NetError::Refused("127.0.0.1:45550".into())),
            DialFailed::Refused(Some(loopback)),
            "the address the machine saw"
        );
        assert_eq!(from(NetError::Refused("not an address".into())), DialFailed::Refused(None));
        assert_eq!(from(NetError::NoAnswer("127.0.0.1:45550".into())), DialFailed::NoAnswer);
        assert_eq!(from(NetError::Resolve("studio".into())), DialFailed::NoSuchHost);
        assert_eq!(from(NetError::Closed), DialFailed::Dropped);
        assert_eq!(from(NetError::NotGranted), DialFailed::NotGranted);
        let local = NetError::Address("bad".into());
        assert_eq!(from(local), DialFailed::NoSuchHost, "an address that does not parse");
        let own = NetError::io("the client's id", std::io::Error::other("read-only"));
        assert_eq!(
            from(own),
            DialFailed::Other("the client's id: read-only".into()),
            "this device's"
        );
    }
}
