//! Endpoint construction.

use std::future::poll_fn;
use std::pin::Pin;
use std::time::Duration;

use iroh::endpoint::{BindOpts, IdleTimeout, PathEvent, QuicTransportConfig, VarInt, presets};
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
///
/// Congestion control is BBR3 (model-based: pacing at the measured bottleneck rate) rather
/// than the Cubic default: on the Wi-Fi/mesh path Cubic cut the window to 13–20 KB after a
/// handful of real losses per 15 s, which at a 10 ms round trip caps a media stream near
/// 10 Mbit/s — a third of the 30 Mbit/s target (MEASUREMENTS.md, 2026-09-05).
#[must_use]
pub fn transport_config() -> QuicTransportConfig {
    let idle = IdleTimeout::try_from(IDLE_TIMEOUT).ok();
    QuicTransportConfig::builder()
        .congestion_controller_factory(congestion_controller())
        .max_idle_timeout(idle)
        .keep_alive_interval(KEEP_ALIVE)
        .datagram_receive_buffer_size(Some(DATAGRAM_BUFFER))
        .datagram_send_buffer_size(DATAGRAM_BUFFER)
        .max_concurrent_bidi_streams(VarInt::from_u32(16))
        .max_concurrent_uni_streams(VarInt::from_u32(256))
        .build()
}

/// Environment override for the congestion controller: `cubic`, `bbr3` or `newreno`.
pub const CC_ENV: &str = "SLOPTY_CC";

/// The congestion controller factory: [`CC_ENV`] if set and known, else BBR3.
fn congestion_controller()
-> std::sync::Arc<dyn noq_proto::congestion::ControllerFactory + Send + Sync + 'static> {
    use noq_proto::congestion::{Bbr3Config, CubicConfig, NewRenoConfig};
    let choice = std::env::var(CC_ENV).unwrap_or_default();
    match choice.as_str() {
        "cubic" => std::sync::Arc::new(CubicConfig::default()),
        "newreno" => std::sync::Arc::new(NewRenoConfig::default()),
        "bbr3" | "" => std::sync::Arc::new(Bbr3Config::default()),
        other => {
            tracing::warn!(%other, "unknown {CC_ENV}; using bbr3");
            std::sync::Arc::new(Bbr3Config::default())
        }
    }
}

/// UDP port a host binds unless told otherwise (`slopty-hostd --port`).
///
/// A host must keep its port across restarts: a paired client on another subnet (a phone on a
/// mesh, a laptop on Wi-Fi) only knows the address from the ticket and cannot hear mDNS, so a
/// random port per launch would strand it until it re-pairs.
pub const HOST_PORT: u16 = 45550;

/// Bind an endpoint on any free port.
///
/// [`Reach::Anywhere`] uses n0's production relays and DNS/pkarr address lookup so a host is
/// reachable from anywhere by id; [`Reach::DirectOnly`] turns both off. With the `mdns`
/// feature either mode also advertises/looks up on the LAN.
pub async fn bind(secret: SecretKey, role: Role, reach: Reach) -> Result<Endpoint, NetError> {
    bind_on(secret, role, reach, 0).await
}

/// [`bind`] on a fixed UDP `port` (0 for any free port). A port already in use is an error:
/// falling back to a random one would reintroduce the strand-on-restart problem
/// [`HOST_PORT`] exists to avoid.
pub async fn bind_on(
    secret: SecretKey,
    role: Role,
    reach: Reach,
    port: u16,
) -> Result<Endpoint, NetError> {
    let builder = Endpoint::builder(presets::N0)
        .secret_key(secret)
        .alpns(vec![ALPN.to_vec()])
        .transport_config(transport_config());
    let builder = if port == 0 {
        builder
    } else {
        let v4 = std::net::SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, port));
        let v6 = std::net::SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port));
        builder
            .clear_ip_transports()
            .bind_addr(v4)
            .and_then(|b| b.bind_addr_with_opts(v6, BindOpts::default().set_is_required(false)))
            .map_err(|e| NetError::Bind(e.to_string()))?
    };
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

/// The selected path's congestion picture plus the datagram send buffer headroom, for logs.
///
/// `cwnd` is the congestion window in bytes; `space` is how many bytes of datagrams the QUIC
/// stack will still queue before it starts dropping the oldest — media that sits in that
/// buffer waiting for `cwnd` is latency the receiver sees as loss.
#[must_use]
pub fn describe_health(conn: &iroh::endpoint::Connection) -> String {
    let space = conn.datagram_send_buffer_space();
    conn.paths().iter().find(iroh::endpoint::Path::is_selected).map_or_else(
        || format!("no selected path; space {space}"),
        |p| {
            let s = p.stats();
            format!(
                "rtt {:.1?} cwnd {} congestion {} lost {} pkts/{} B mtu {} space {space}",
                s.rtt, s.cwnd, s.congestion_events, s.lost_packets, s.lost_bytes, s.current_mtu
            )
        },
    )
}

/// The selected path's smoothed rtt and congestion window, for the media rate controller.
#[must_use]
pub fn selected_path(conn: &iroh::endpoint::Connection) -> Option<(Duration, u64)> {
    conn.paths().iter().find(iroh::endpoint::Path::is_selected).map(|p| {
        let s = p.stats();
        (s.rtt, s.cwnd)
    })
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
