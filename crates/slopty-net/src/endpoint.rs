//! Endpoint construction.

use std::future::poll_fn;
use std::pin::Pin;
use std::time::Duration;

use iroh::endpoint::{IdleTimeout, PathEvent, QuicTransportConfig, VarInt, presets};
use iroh::{Endpoint, SecretKey};

use crate::{ALPN, NetError};

/// Role-specific knobs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    /// Accepts connections; advertises on the LAN.
    Host,
    /// Dials hosts.
    Client,
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

/// Bind an endpoint. Uses n0's production relays and DNS/pkarr address lookup so a host is
/// reachable from anywhere by id; with the `mdns` feature it also advertises/looks up on the LAN.
pub async fn bind(secret: SecretKey, role: Role) -> Result<Endpoint, NetError> {
    let builder = Endpoint::builder(presets::N0)
        .secret_key(secret)
        .alpns(vec![ALPN.to_vec()])
        .transport_config(transport_config());
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

/// Round-trip time on the selected path, if measured.
#[must_use]
pub fn rtt(conn: &iroh::endpoint::Connection) -> Option<Duration> {
    conn.paths().iter().find(iroh::endpoint::Path::is_selected).map(|p| p.rtt())
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
