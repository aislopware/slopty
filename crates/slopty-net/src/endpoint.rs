//! Endpoint construction.

use std::future::poll_fn;
use std::pin::Pin;
use std::time::Duration;

use iroh::endpoint::{IdleTimeout, PathEvent, QuicTransportConfig, VarInt, presets};
use iroh::{Endpoint, RelayMode, SecretKey, TransportAddr, Watcher as _};

use crate::{ALPN, NetError};

/// Role-specific knobs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    /// Accepts connections; advertises on the LAN.
    Host,
    /// Dials hosts.
    Client,
}

/// How far an endpoint must reach.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Reach {
    /// n0 relays plus DNS/pkarr lookup: reachable from anywhere by id, degrading to a relayed
    /// path when no direct one holds.
    #[default]
    Anywhere,
    /// No relay, no wide-area lookup: only addresses the peer already knows (LAN, mDNS, or a
    /// private mesh such as NetBird/Tailscale). A lost direct path drops the connection instead
    /// of silently sliding onto a relay with ten times the latency.
    DirectOnly,
}

impl Reach {
    /// Environment variable that selects [`Reach::DirectOnly`] when set to `1`/`true`.
    pub const ENV: &'static str = "SLOPTY_DIRECT_ONLY";

    /// [`Reach::DirectOnly`] when [`Self::ENV`] is `1` or `true`, else [`Reach::Anywhere`].
    #[must_use]
    pub fn from_env() -> Self {
        match std::env::var(Self::ENV).as_deref() {
            Ok("1" | "true" | "yes") => Self::DirectOnly,
            _ => Self::Anywhere,
        }
    }

    /// Whether relays are off.
    #[must_use]
    pub const fn is_direct_only(self) -> bool {
        matches!(self, Self::DirectOnly)
    }
}

/// Idle timeout before a silent connection is dropped. Generous: a phone in a pocket keeps its
/// session across brief radio gaps; QUIC migration handles the address change.
const IDLE_TIMEOUT: Duration = Duration::from_secs(45);
/// Keep-alive so NAT bindings survive and RTT stays measured while idle.
const KEEP_ALIVE: Duration = Duration::from_secs(5);
/// Datagram buffers: a few frames of 4K video at 60 fps.
const DATAGRAM_BUFFER: usize = 4 << 20;

/// Transport config shared by both roles.
#[must_use]
pub fn transport_config() -> QuicTransportConfig {
    let idle = IdleTimeout::try_from(IDLE_TIMEOUT).ok();
    QuicTransportConfig::builder()
        .max_idle_timeout(idle)
        .keep_alive_interval(KEEP_ALIVE)
        .datagram_receive_buffer_size(Some(DATAGRAM_BUFFER))
        .datagram_send_buffer_size(DATAGRAM_BUFFER)
        .max_concurrent_bidi_streams(VarInt::from_u32(16))
        .max_concurrent_uni_streams(VarInt::from_u32(256))
        .build()
}

/// Bind an endpoint.
///
/// [`Reach::Anywhere`] uses n0's production relays and DNS/pkarr address lookup so a host is
/// reachable from anywhere by id; [`Reach::DirectOnly`] turns both off. With the `mdns`
/// feature either mode also advertises/looks up on the LAN.
pub async fn bind(secret: SecretKey, role: Role, reach: Reach) -> Result<Endpoint, NetError> {
    let builder = Endpoint::builder(presets::N0)
        .secret_key(secret)
        .alpns(vec![ALPN.to_vec()])
        .transport_config(transport_config());
    let builder = match reach {
        Reach::Anywhere => builder,
        Reach::DirectOnly => builder.relay_mode(RelayMode::Disabled).clear_address_lookup(),
    };
    #[cfg(feature = "mdns")]
    let builder = builder.address_lookup(
        iroh_mdns_address_lookup::MdnsAddressLookup::builder()
            .advertise(role == Role::Host)
            .service_name("slopty"),
    );
    #[cfg(not(feature = "mdns"))]
    tracing::debug!(?role, "mdns feature off; LAN discovery disabled");
    builder.bind().await.map_err(|e| NetError::Bind(e.to_string()))
}

/// Wait until the endpoint can be dialed.
///
/// That is a relay connection for [`Reach::Anywhere`] (so a ticket carries a relay URL), or at
/// least one direct address for [`Reach::DirectOnly`]. Never returns if the endpoint is dropped
/// meanwhile.
pub async fn online(endpoint: &Endpoint, reach: Reach) {
    match reach {
        Reach::Anywhere => endpoint.online().await,
        Reach::DirectOnly => {
            let mut watcher = endpoint.watch_addr();
            loop {
                let addr = watcher.get();
                if addr.addrs.iter().any(|a| matches!(a, TransportAddr::Ip(_))) {
                    return;
                }
                if watcher.updated().await.is_err() {
                    std::future::pending::<()>().await;
                }
            }
        }
    }
}

/// Round-trip time on the selected path, if measured.
#[must_use]
pub fn rtt(conn: &iroh::endpoint::Connection) -> Option<Duration> {
    conn.paths().iter().find(iroh::endpoint::Path::is_selected).map(|p| p.rtt())
}

/// UDP datagrams received on the connection so far.
///
/// With `KEEP_ALIVE` pings from the other side a healthy connection moves this counter every
/// few seconds; a counter that stands still is the earliest sign the peer is gone, long before
/// `IDLE_TIMEOUT`.
#[must_use]
pub fn received_datagrams(conn: &iroh::endpoint::Connection) -> u64 {
    conn.stats().udp_rx.datagrams
}

/// Whether the selected path is relayed (`None` while no path is selected).
#[must_use]
pub fn relayed(conn: &iroh::endpoint::Connection) -> Option<bool> {
    conn.paths().iter().find(iroh::endpoint::Path::is_selected).map(|p| p.is_relay())
}

/// Human-readable description of every path (for diagnostics): `*` marks the selected one.
#[must_use]
pub fn describe_paths(conn: &iroh::endpoint::Connection) -> String {
    conn.paths()
        .iter()
        .map(|p| {
            let kind = if p.is_relay() { "relay" } else { "direct" };
            let mark = if p.is_selected() { "*" } else { " " };
            format!("{mark}{kind} {} rtt {:.1?}", p.remote_addr(), p.rtt())
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Log every path change on `conn` at info level until the connection closes.
///
/// Opened, closed (with the path's final stats) and selected. Path flaps are the first thing
/// to look at when latency jumps, so both ends run this for every connection.
pub async fn log_path_events(conn: iroh::endpoint::Connection, side: &'static str) {
    let mut events = conn.path_events();
    let remote = conn.remote_id().fmt_short();
    loop {
        let next = poll_fn(|cx| {
            use futures_core::Stream as _;
            Pin::new(&mut events).poll_next(cx)
        })
        .await;
        let Some(event) = next else { break };
        match event {
            PathEvent::Opened { id, remote_addr, local_addr, .. } => {
                tracing::info!(side, %remote, ?id, %remote_addr, ?local_addr, "path opened");
            }
            PathEvent::Closed { id, remote_addr, last_stats, .. } => {
                tracing::info!(
                    side,
                    %remote,
                    ?id,
                    %remote_addr,
                    rtt_ms = last_stats.rtt.as_secs_f64() * 1000.0,
                    lost = last_stats.lost_packets,
                    black_holes = last_stats.black_holes_detected,
                    congestion_events = last_stats.congestion_events,
                    "path closed"
                );
            }
            PathEvent::Selected { id, remote_addr, .. } => {
                tracing::info!(side, %remote, ?id, %remote_addr, "path selected");
            }
            PathEvent::Lagged { missed, .. } => {
                tracing::debug!(side, missed, "path events lagged");
            }
            _ => {}
        }
    }
}
