//! Finding the Slopty server on the tailnet when none is named: each online node Tailscale
//! lists is tried with a QUIC handshake on [`SERVER_PORT`], all at once.
//!
//! Only the handshake is made, so a probe registers nothing and says nothing to the server; the
//! server port and the lease transport's ALPN are what mark a Slopty server. Nodes tagged
//! [`SERVER_TAG`] come first, then this machine, then the rest, so a tailnet that names its
//! server gets that one when others answer too.

use std::net::SocketAddr;
use std::time::Duration;

use slopty_tailnet::{Node, Status};
use tokio::task::JoinSet;

use crate::endpoint::SERVER_PORT;
use crate::worker::close_code;
use crate::{Endpoint, HostAddr};

/// How long a node has to answer the handshake. A tailnet hop is milliseconds once a path is
/// up; the first packet to an idle peer goes over DERP while disco looks for a direct path.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// The tag a tailnet gives the node that runs its Slopty server, so it is tried first.
pub const SERVER_TAG: &str = "tag:slopty-server";

/// A node that answered on the server port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Its `MagicDNS` name, for a person.
    pub name: String,
    /// Where it answered.
    pub addr: SocketAddr,
}

impl Found {
    /// The address to dial it at: its tailnet IP, which needs no `MagicDNS`.
    #[must_use]
    pub fn host_addr(&self) -> HostAddr {
        HostAddr::from(self.addr)
    }
}

/// The online nodes worth trying, best first, each at its IPv4 address on `port`: tagged
/// [`SERVER_TAG`], then this machine, then the rest. Phones never run the server.
#[must_use]
pub fn candidates(status: &Status, port: u16) -> Vec<Found> {
    let rank = |node: &Node, me: bool| {
        if node.tags.iter().any(|t| t == SERVER_TAG) {
            0
        } else if me {
            1
        } else {
            2
        }
    };
    let me = status.me.iter().map(|n| (n, true));
    let peers = status.peer.values().filter(|n| n.online).map(|n| (n, false));
    let mut ranked: Vec<(u8, Found)> = me
        .chain(peers)
        .filter(|(n, _)| !matches!(n.os.as_str(), "iOS" | "android"))
        .filter_map(|(n, me)| {
            let ip = n.ipv4()?;
            Some((
                rank(n, me),
                Found { name: n.name().to_owned(), addr: SocketAddr::new(ip, port) },
            ))
        })
        .collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, found)| found).collect()
}

/// The Slopty servers on the tailnet `status` describes, dialled from `endpoint`, best first.
pub async fn servers(endpoint: &Endpoint, status: &Status) -> Vec<Found> {
    answering(endpoint, candidates(status, SERVER_PORT)).await
}

/// Those of `candidates` that answer a handshake within [`PROBE_TIMEOUT`], in their order.
pub async fn answering(endpoint: &Endpoint, candidates: Vec<Found>) -> Vec<Found> {
    let mut probes = JoinSet::new();
    for (i, found) in candidates.into_iter().enumerate() {
        let endpoint = endpoint.clone();
        probes.spawn(async move { probe(&endpoint, found.addr).await.then_some((i, found)) });
    }
    let mut answered: Vec<(usize, Found)> = probes.join_all().await.into_iter().flatten().collect();
    answered.sort_by_key(|(i, _)| *i);
    answered.into_iter().map(|(_, found)| found).collect()
}

async fn probe(endpoint: &Endpoint, addr: SocketAddr) -> bool {
    let config = crate::endpoint::lease_client_config();
    let name = addr.ip().to_string();
    let dialed = crate::client::dial(endpoint, addr, &name, Some(config));
    match tokio::time::timeout(PROBE_TIMEOUT, dialed).await {
        Ok(Ok(conn)) => {
            conn.close(close_code::NORMAL.into(), b"probe");
            true
        }
        Ok(Err(e)) => {
            tracing::debug!(%addr, error = %e, "no server");
            false
        }
        Err(_elapsed) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::Admission;
    use crate::server::ServerListener;

    const STATUS: &str = r#"{"BackendState":"Running",
      "Self":{"ID":"n1","HostName":"mac","DNSName":"mac.ts.net.","OS":"macOS",
        "TailscaleIPs":["100.64.0.3","fd7a:115c:a1e0::3"],"Online":true},
      "Peer":{
        "a":{"ID":"n2","HostName":"box","DNSName":"box.ts.net.","OS":"linux",
          "TailscaleIPs":["100.64.0.4"],"Online":true},
        "b":{"ID":"n3","HostName":"hub","DNSName":"hub.ts.net.","OS":"linux",
          "TailscaleIPs":["100.64.0.5"],"Tags":["tag:slopty-server"],"Online":true},
        "c":{"ID":"n4","HostName":"off","DNSName":"off.ts.net.","OS":"linux",
          "TailscaleIPs":["100.64.0.6"],"Online":false},
        "d":{"ID":"n5","HostName":"phone","DNSName":"phone.ts.net.","OS":"iOS",
          "TailscaleIPs":["100.64.0.7"],"Online":true}}}"#;

    /// The tagged server first, then this machine, then the rest; offline nodes and phones
    /// are not tried.
    #[test]
    fn the_tagged_server_is_tried_first_and_phones_never() {
        let status: Status = serde_json::from_str(STATUS).unwrap();
        let names: Vec<(String, String)> = candidates(&status, SERVER_PORT)
            .into_iter()
            .map(|f| (f.name, f.addr.to_string()))
            .collect();
        let want = [
            ("hub.ts.net", "100.64.0.5"),
            ("mac.ts.net", "100.64.0.3"),
            ("box.ts.net", "100.64.0.4"),
        ];
        let want: Vec<(String, String)> =
            want.iter().map(|(n, ip)| ((*n).to_owned(), format!("{ip}:{SERVER_PORT}"))).collect();
        assert_eq!(names, want);
    }

    /// A listening server answers the probe; a port nobody listens on does not, and does not
    /// hold the answer up past the probe timeout.
    #[tokio::test]
    async fn a_listening_server_answers_and_a_closed_port_does_not() {
        let listener = ServerListener::bind(
            "127.0.0.1:0".parse().unwrap(),
            Admission::with_tailnet(Vec::new(), None),
        )
        .unwrap();
        let live = listener.local_addr().unwrap();
        let dead = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let endpoint = crate::endpoint::bind_lease("127.0.0.1:0".parse().unwrap()).unwrap();
        let found = |name: &str, addr| Found { name: name.to_owned(), addr };
        let started = std::time::Instant::now();
        let answered = answering(&endpoint, vec![found("dead", dead), found("live", live)]).await;
        assert_eq!(answered, [found("live", live)]);
        assert!(
            started.elapsed() < PROBE_TIMEOUT + Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
    }
}
