//! Endpoint construction and what a connection can tell about its path.

use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use noq::{
    AckFrequencyConfig, Connection, Endpoint, IdleTimeout, MtuDiscoveryConfig, PathId,
    TransportConfig, VarInt,
};

use crate::NetError;

/// UDP port a worker binds unless told otherwise (`slopty-worker --port`), and the port a client
/// dials when an address names none.
pub const WORKER_PORT: u16 = 45550;

/// UDP port the server binds unless told otherwise, and the port a worker or client dials when a
/// server address names none.
pub const SERVER_PORT: u16 = 45560;

/// TCP port the server's MCP endpoint (Streamable HTTP) listens on unless told otherwise.
pub const MCP_PORT: u16 = 45561;

/// Keep-alive on a server link (worker, client or agent ↔ server): the server's pings keep a
/// healthy link busy both ways, so [`LEASE_IDLE_TIMEOUT`] only fires on a dead path.
pub const LEASE_KEEP_ALIVE: Duration = Duration::from_secs(1);
/// Silence after which a server link is dead; for a worker this ends its lease and the
/// directory marks it unreachable (`docs/decisions/topology.md`).
pub const LEASE_IDLE_TIMEOUT: Duration = Duration::from_secs(5);

/// Idle timeout before a silent connection is dropped. Generous: a phone in a pocket keeps its
/// session across brief radio gaps; QUIC migration handles the address change.
const IDLE_TIMEOUT: Duration = Duration::from_secs(45);
/// Keep-alive so NAT bindings survive and RTT stays measured while idle.
const KEEP_ALIVE: Duration = Duration::from_secs(5);
/// Datagram buffers: a few frames of 4K video at 60 fps.
///
/// The worker's capture guard skips a frame while the transport still holds the last one's bytes,
/// so this is the ceiling a keyframe burst needs, not the queue a stream lives in: noq drops the
/// *oldest* datagram when the buffer is full, and a buffer smaller than a keyframe would cut the
/// head off every one.
pub const DATAGRAM_BUFFER: usize = 4 << 20;
/// Longest the peer may sit on an ACK (QUIC's default is 25 ms).
///
/// Media leaves the worker in one burst per frame, larger than the congestion window on a
/// short path (BBR sizes the window from bandwidth × min RTT, ~2 frames on loopback), so the
/// tail of every frame waits for the ACK of its head. With the default the ACK of an odd
/// last packet waits the full 25 ms — longer than a frame interval — and that wait reached
/// the receiver as a missing tail and a NACK (MEASUREMENTS.md, "start-up on a cold
/// connection"). 2 ms keeps the window turning at the path's round trip.
const MAX_ACK_DELAY: Duration = Duration::from_millis(2);
/// The round trip assumed before the first sample: RFC 9002's 333 ms is for an unknown path
/// on the open Internet, and it sets the handshake's first retransmission timer. Every Slopty
/// path is a LAN or a mesh a few milliseconds long.
const INITIAL_RTT: Duration = Duration::from_millis(5);
/// Largest UDP payload MTU discovery climbs to: Tailscale's TUN MTU is 1280, less 28 bytes of
/// IPv4 and UDP header. Probing past it only loses probes; on a LAN it costs ~4% of payload
/// against 1452.
const MTU_UPPER_BOUND: u16 = 1252;

/// Environment override for the congestion controller: `cubic`, `bbr3` or `newreno`.
///
/// Each is held to [`crate::congestion::CEILING_BDPS`] of the measured path unless
/// [`UNBOUNDED_SUFFIX`] follows it (diagnostics: `SLOPTY_CC=bbr3-unbounded` is noq's BBR3 as it
/// ships).
pub const CC_ENV: &str = "SLOPTY_CC";
/// Appended to a [`CC_ENV`] choice, leaves noq's window alone.
pub const UNBOUNDED_SUFFIX: &str = "-unbounded";
/// Set to `1`, noq writes queued datagrams ahead of every stream, as it ships (diagnostics:
/// measuring against [`crate::streams::AHEAD_OF_DATAGRAMS`]).
pub const DATAGRAMS_FIRST_ENV: &str = "SLOPTY_DATAGRAMS_FIRST";
/// Environment override for the initial congestion window, in packets (diagnostics).
pub const INITIAL_WINDOW_ENV: &str = "SLOPTY_QUIC_IW";
/// Initial congestion window, in packets of the initial 1200-byte datagram size.
///
/// RFC 9002's 10 packets (noq's default) is sized for an unknown peer on the open Internet;
/// a worker and client on a LAN or a private mesh can afford the burst Chromium's QUIC
/// starts with. The first keyframe is tens to hundreds of kilobytes and every window's worth
/// of it costs a round trip (MEASUREMENTS.md, "start-up over the mesh").
const INITIAL_WINDOW_PACKETS: u64 = 32;
/// Streams of each direction the peer may have open at once.
///
/// Every forwarded TCP connection is a bidirectional stream, and a browser on a dev server keeps
/// six sockets per origin plus its hot-reload websockets; every attached terminal and every file
/// in flight is a unidirectional one. A stream past the limit does not fail, it waits for one
/// to close, so a low limit shows up as a page that never loads. Each stream's receive window
/// (noq's 1.25 MB default) still bounds what one stalled stream can hold.
pub const MAX_STREAMS: u32 = 1024;

/// Transport config shared by both roles.
///
/// Congestion control is BBR3 (model-based: pacing at the measured bottleneck rate) rather
/// than the Cubic default: on the Wi-Fi/mesh path Cubic cut the window to 13–20 KB after a
/// handful of real losses per 15 s, which at a 10 ms round trip caps a media stream near
/// 10 Mbit/s — a third of the 30 Mbit/s target (MEASUREMENTS.md, 2026-09-05). Its window is
/// held to twice the measured bandwidth-delay product ([`crate::congestion`]), since noq's grows
/// without bound under bursty video and moves every burst into the bottleneck's queue
/// (MEASUREMENTS.md, "BBR3's window under bursty video").
#[must_use]
pub fn transport_config() -> TransportConfig {
    let mut acks = AckFrequencyConfig::default();
    acks.max_ack_delay(Some(MAX_ACK_DELAY));
    let mut mtu = MtuDiscoveryConfig::default();
    mtu.upper_bound(MTU_UPPER_BOUND);
    let mut config = TransportConfig::default();
    config
        .congestion_controller_factory(congestion_controller())
        .ack_frequency_config(Some(acks))
        .initial_rtt(INITIAL_RTT)
        .mtu_discovery_config(Some(mtu))
        .max_idle_timeout(IdleTimeout::try_from(IDLE_TIMEOUT).ok())
        .keep_alive_interval(Some(KEEP_ALIVE))
        .datagram_receive_buffer_size(Some(DATAGRAM_BUFFER))
        .datagram_send_buffer_size(DATAGRAM_BUFFER)
        .max_concurrent_bidi_streams(VarInt::from_u32(MAX_STREAMS))
        .max_concurrent_uni_streams(VarInt::from_u32(MAX_STREAMS))
        .stream_priority_before_datagrams(streams_ahead_of_datagrams());
    config
}

/// [`crate::streams::AHEAD_OF_DATAGRAMS`], unless [`DATAGRAMS_FIRST_ENV`] is `1`.
fn streams_ahead_of_datagrams() -> Option<i32> {
    let datagrams_first = std::env::var(DATAGRAMS_FIRST_ENV).is_ok_and(|v| v == "1");
    (!datagrams_first).then_some(crate::streams::AHEAD_OF_DATAGRAMS)
}

/// Transport config for server links: [`transport_config`] with the lease's timings.
///
/// The idle timeout is negotiated down to the smaller side's, so a server endpoint on this
/// config holds every link to it to [`LEASE_IDLE_TIMEOUT`].
#[must_use]
pub fn lease_transport_config() -> TransportConfig {
    let mut config = transport_config();
    config
        .max_idle_timeout(IdleTimeout::try_from(LEASE_IDLE_TIMEOUT).ok())
        .keep_alive_interval(Some(LEASE_KEEP_ALIVE));
    config
}

/// A client config that dials a server on [`lease_transport_config`], for an endpoint whose
/// default config is the worker's.
#[must_use]
pub fn lease_client_config() -> noq::ClientConfig {
    let mut config = crate::crypto::client_config();
    config.transport_config(Arc::new(lease_transport_config()));
    config
}

/// The initial congestion window in bytes: [`INITIAL_WINDOW_ENV`] packets if set, else
/// [`INITIAL_WINDOW_PACKETS`].
fn initial_window() -> u64 {
    initial_window_of(std::env::var(INITIAL_WINDOW_ENV).ok().as_deref())
}

/// [`initial_window`] without the environment: `packets` if it reads as a positive number.
fn initial_window_of(packets: Option<&str>) -> u64 {
    let packets = packets
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|p| *p > 0)
        .unwrap_or(INITIAL_WINDOW_PACKETS);
    packets.saturating_mul(1200)
}

/// The congestion controller factory: [`CC_ENV`] if set and known, else BBR3, bounded by
/// [`crate::congestion::Bounded`] unless the choice says otherwise.
fn congestion_controller() -> Arc<dyn noq::congestion::ControllerFactory + Send + Sync + 'static> {
    let choice = std::env::var(CC_ENV).unwrap_or_default();
    let (inner, ceiling) = match choice.strip_suffix(UNBOUNDED_SUFFIX) {
        Some(inner) => (inner, None),
        None => (choice.as_str(), Some(crate::congestion::CEILING_BDPS)),
    };
    Arc::new(crate::congestion::BoundedFactory::new(noq_controller(inner), ceiling))
}

/// noq's controller factory `choice` names, BBR3 for anything else.
fn noq_controller(choice: &str) -> Arc<dyn noq::congestion::ControllerFactory + Send + Sync> {
    use noq::congestion::{Bbr3Config, CubicConfig, NewRenoConfig};
    let window = initial_window();
    let bbr3 = || {
        let mut config = Bbr3Config::default();
        config.initial_window(window);
        Arc::new(config)
    };
    match choice {
        "cubic" => {
            let mut config = CubicConfig::default();
            config.initial_window(window);
            Arc::new(config)
        }
        "newreno" => {
            let mut config = NewRenoConfig::default();
            config.initial_window(window);
            Arc::new(config)
        }
        "bbr3" | "" => bbr3(),
        other => {
            tracing::warn!(%other, "unknown {CC_ENV}; using bbr3");
            bbr3()
        }
    }
}

/// Bind a QUIC endpoint on `local`; with `server` it also accepts connections.
///
/// An unspecified IPv6 address (`[::]`) is bound dual-stack, so one socket answers IPv4 and
/// IPv6 peers alike (IPv4 ones appear as `::ffff:a.b.c.d`). An address already in use is an
/// error: falling back to a random port would strand every client that knows the worker by its
/// port.
pub fn bind(local: SocketAddr, server: bool) -> Result<Endpoint, NetError> {
    bind_with(local, server, transport_config())
}

/// [`bind`] for the server's endpoint: accepting, every link on [`lease_transport_config`].
pub fn bind_lease(local: SocketAddr) -> Result<Endpoint, NetError> {
    bind_with(local, true, lease_transport_config())
}

fn bind_with(
    local: SocketAddr,
    server: bool,
    transport: TransportConfig,
) -> Result<Endpoint, NetError> {
    let bind_err = |e: std::io::Error| NetError::Bind(format!("{local}: {e}"));
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(local),
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .map_err(bind_err)?;
    if local.is_ipv6() {
        socket.set_only_v6(false).map_err(bind_err)?;
    }
    socket.bind(&local.into()).map_err(bind_err)?;
    let transport = Arc::new(transport);
    let server_config = server.then(|| {
        let mut config = crate::crypto::server_config();
        config.transport_config(Arc::clone(&transport));
        config
    });
    let endpoint = Endpoint::new(
        crate::crypto::endpoint_config(),
        server_config,
        socket.into(),
        Arc::new(noq::TokioRuntime),
    )
    .map_err(bind_err)?;
    let mut client = crate::crypto::client_config();
    client.transport_config(transport);
    endpoint.set_default_client_config(client);
    Ok(endpoint)
}

/// Every interface, both families, on `port`.
#[must_use]
pub const fn any(port: u16) -> SocketAddr {
    SocketAddr::new(std::net::IpAddr::V6(Ipv6Addr::UNSPECIFIED), port)
}

/// Stats of the connection's one path.
fn path(conn: &Connection) -> Option<noq::PathStats> {
    conn.path_stats(PathId::ZERO)
}

/// Round-trip time, if measured.
#[must_use]
pub fn rtt(conn: &Connection) -> Option<Duration> {
    conn.rtt(PathId::ZERO)
}

/// UDP datagrams received on the connection so far.
///
/// With `KEEP_ALIVE` pings from the other side a healthy connection moves this counter every
/// few seconds; a counter that stands still is the earliest sign the peer is gone, long before
/// `IDLE_TIMEOUT`.
#[must_use]
pub fn received_datagrams(conn: &Connection) -> u64 {
    conn.stats().udp_rx.datagrams
}

/// Where the peer is now (it moves when the peer migrates), as an IPv4 address when it is one;
/// `None` once the connection is gone.
#[must_use]
pub fn remote(conn: &Connection) -> Option<SocketAddr> {
    conn.path(PathId::ZERO)?.remote_address().ok().map(canonical)
}

/// `addr` with an IPv4-mapped IPv6 address turned back into IPv4.
#[must_use]
pub const fn canonical(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

/// The path in one line, for diagnostics.
#[must_use]
pub fn describe_path(conn: &Connection) -> String {
    let rtt = rtt(conn).map_or_else(|| "?".to_owned(), |r| format!("{r:.1?}"));
    let remote = remote(conn).map_or_else(|| "gone".to_owned(), |r| r.to_string());
    format!("{remote} rtt {rtt}")
}

/// The path's congestion picture plus the datagram send buffer headroom, for logs.
///
/// `cwnd` is the congestion window in bytes; `space` is how many bytes of datagrams the QUIC
/// stack will still queue before it starts dropping the oldest — media that sits in that
/// buffer waiting for `cwnd` is latency the receiver sees as loss.
#[must_use]
pub fn describe_health(conn: &Connection) -> String {
    let space = conn.datagram_send_buffer_space();
    path(conn).map_or_else(
        || format!("no path; space {space}"),
        |s| {
            format!(
                "rtt {:.1?} cwnd {} congestion {} lost {} pkts/{} B mtu {} space {space}",
                s.rtt, s.cwnd, s.congestion_events, s.lost_packets, s.lost_bytes, s.current_mtu
            )
        },
    )
}

/// The path's smoothed rtt and congestion window, for the media rate controller.
#[must_use]
pub fn path_rtt_cwnd(conn: &Connection) -> Option<(Duration, u64)> {
    path(conn).map(|s| (s.rtt, s.cwnd))
}

/// Environment variable that turns [`trace_path_health`] on, in milliseconds between samples.
pub const PATH_TRACE_ENV: &str = "SLOPTY_PATH_TRACE_MS";

/// The sampling period from [`PATH_TRACE_ENV`], or `None` when the trace is off.
///
/// A congestion window is only legible as a series: one reading at the end of a run cannot tell
/// a window that sat on BBR's four-packet floor the whole time from one that dipped there for
/// `ProbeRTT`. Off by default because the line is per sample, not per event.
#[must_use]
pub fn path_trace_period() -> Option<Duration> {
    parse_trace_period(std::env::var(PATH_TRACE_ENV).ok()?.as_str())
}

/// [`path_trace_period`] without the environment: milliseconds; `0` and anything unreadable is off.
fn parse_trace_period(raw: &str) -> Option<Duration> {
    let ms: u64 = raw.trim().parse().ok()?;
    (ms > 0).then(|| Duration::from_millis(ms))
}

/// Sample the path every [`path_trace_period`] and log it, until the connection closes.
///
/// Returns at once when the trace is off. `cwnd` against `space` is the whole send-side picture:
/// a window at its floor with the datagram buffer filling is media waiting on ACKs.
pub async fn trace_path_health(conn: Connection, side: &'static str) {
    let Some(period) = path_trace_period() else { return };
    let mut ticker = tokio::time::interval(period);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        if conn.close_reason().is_some() {
            return;
        }
        let space = conn.datagram_send_buffer_space();
        let Some(path) = path(&conn) else { continue };
        tracing::debug!(
            side,
            rtt_us = path.rtt.as_micros(),
            cwnd = path.cwnd,
            space,
            congestion = path.congestion_events,
            lost = path.lost_packets,
            sent = path.udp_tx.datagrams,
            mtu = path.current_mtu,
            "path health"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_initial_window_reads_its_flag() {
        assert_eq!(initial_window_of(None), 32 * 1200);
        assert_eq!(initial_window_of(Some("8")), 8 * 1200);
        for bad in ["0", "-3", "many", ""] {
            assert_eq!(initial_window_of(Some(bad)), 32 * 1200, "{bad:?}");
        }
        assert_eq!(DATAGRAM_BUFFER, 4 * 1024 * 1024);
    }

    #[test]
    fn the_path_trace_is_off_unless_a_period_asks_for_it() {
        assert_eq!(parse_trace_period("100"), Some(Duration::from_millis(100)));
        assert_eq!(parse_trace_period(" 25 "), Some(Duration::from_millis(25)));
        for off in ["", "0", "no", "-1", "1.5"] {
            assert_eq!(parse_trace_period(off), None, "{off:?} should leave the trace off");
        }
    }

    #[test]
    fn a_mapped_address_reads_as_ipv4() {
        let mapped: SocketAddr = "[::ffff:100.64.0.3]:45550".parse().unwrap();
        assert_eq!(canonical(mapped), "100.64.0.3:45550".parse().unwrap());
        let v6: SocketAddr = "[fd7a:115c:a1e0::1]:45550".parse().unwrap();
        assert_eq!(canonical(v6), v6);
    }

    #[tokio::test]
    async fn a_taken_port_is_an_error_not_a_random_one() {
        let first = bind(SocketAddr::from(([127, 0, 0, 1], 0)), true).unwrap();
        let taken = first.local_addr().unwrap();
        let err = bind(taken, true).unwrap_err();
        assert!(err.to_string().contains(&taken.to_string()), "{err}");
    }
}
