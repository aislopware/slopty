//! Finding Slopty on the tailnet when nothing is named.
//!
//! Each online node Tailscale lists is tried with a QUIC handshake, all at once, on
//! [`SERVER_PORT`] for the server ([`find`], [`servers`]) or on [`WORKER_PORT`] for the workers
//! ([`workers`]).
//!
//! Only the handshake is made, so a probe registers nothing and says nothing: it closes at once
//! with [`PROBE_REASON`], which the listener logs as a probe, not as a peer that failed. The
//! port and Slopty's own handshake (its null crypto, which nothing else completes) are what
//! mark a Slopty server or worker; each is dialled with its own transport, the lease's for a
//! server and a client's for a worker. Nodes tagged [`SERVER_TAG`] come first, then this
//! machine, then the rest, so a tailnet that names its server gets that one when others answer
//! too.

use std::net::SocketAddr;
use std::time::Duration;

use slopty_tailnet::{LocalApi, Node, Status};
use tokio::task::JoinSet;

use crate::endpoint::{SERVER_PORT, WORKER_PORT};
use crate::worker::close_code;
use crate::{Endpoint, HostAddr};

/// How long a node has to answer the handshake. A tailnet hop is milliseconds once a path is
/// up; the first packet to an idle peer goes over DERP while disco looks for a direct path.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// The tag a tailnet gives the node that runs its Slopty server, so it is tried first.
pub const SERVER_TAG: &str = "tag:slopty-server";

/// The reason a probe closes with, so the listener knows it for one.
pub const PROBE_REASON: &[u8] = b"probe";

/// What a probe looks for.
#[derive(Clone, Copy, Debug)]
enum Probe {
    /// A server, on the lease's transport.
    Server,
    /// A worker, on a client's.
    Worker,
}

impl Probe {
    fn config(self) -> noq::ClientConfig {
        match self {
            Self::Server => crate::endpoint::lease_client_config(),
            Self::Worker => crate::crypto::client_config(),
        }
    }
}

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
/// [`SERVER_TAG`], then this machine, then the rest. Phones run neither a server nor a worker.
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

/// The best server this machine's Tailscale can find, dialled from `endpoint`; `None` without a
/// Tailscale this process can read, while it is not up, or when no node answers.
pub async fn find(endpoint: &Endpoint) -> Option<Found> {
    find_through(LocalApi::find()?, endpoint, SERVER_PORT).await
}

async fn find_through(api: LocalApi, endpoint: &Endpoint, port: u16) -> Option<Found> {
    let status = match api.status().await {
        Ok(status) if status.running() => status,
        Ok(status) => {
            tracing::debug!(state = %status.backend_state, "tailscale is not up");
            return None;
        }
        Err(e) => {
            tracing::debug!(error = %e, "tailscale status");
            return None;
        }
    };
    answering(endpoint, candidates(&status, port)).await.into_iter().next()
}

/// The Slopty servers on the tailnet `status` describes, dialled from `endpoint`, best first.
pub async fn servers(endpoint: &Endpoint, status: &Status) -> Vec<Found> {
    answering(endpoint, candidates(status, SERVER_PORT)).await
}

/// The Slopty workers on the tailnet `status` describes, dialled from `endpoint`, in
/// [`candidates`] order: what a first run offers besides the servers.
pub async fn workers(endpoint: &Endpoint, status: &Status) -> Vec<Found> {
    answering_workers(endpoint, candidates(status, WORKER_PORT)).await
}

/// Those of `candidates` that answer a server's handshake within [`PROBE_TIMEOUT`], in their
/// order.
pub async fn answering(endpoint: &Endpoint, candidates: Vec<Found>) -> Vec<Found> {
    answering_as(endpoint, candidates, Probe::Server).await
}

/// Those of `candidates` that answer a worker's handshake within [`PROBE_TIMEOUT`], in their
/// order.
pub async fn answering_workers(endpoint: &Endpoint, candidates: Vec<Found>) -> Vec<Found> {
    answering_as(endpoint, candidates, Probe::Worker).await
}

async fn answering_as(endpoint: &Endpoint, candidates: Vec<Found>, what: Probe) -> Vec<Found> {
    let mut probes = JoinSet::new();
    for (i, found) in candidates.into_iter().enumerate() {
        let endpoint = endpoint.clone();
        probes.spawn(async move { probe(&endpoint, found.addr, what).await.then_some((i, found)) });
    }
    let mut answered: Vec<(usize, Found)> = probes.join_all().await.into_iter().flatten().collect();
    answered.sort_by_key(|(i, _)| *i);
    answered.into_iter().map(|(_, found)| found).collect()
}

async fn probe(endpoint: &Endpoint, addr: SocketAddr, what: Probe) -> bool {
    let name = addr.ip().to_string();
    let dialed = crate::client::dial(endpoint, addr, &name, Some(what.config()));
    match tokio::time::timeout(PROBE_TIMEOUT, dialed).await {
        Ok(Ok(conn)) => {
            conn.close(close_code::NORMAL.into(), PROBE_REASON);
            true
        }
        Ok(Err(e)) => {
            tracing::debug!(%addr, ?what, error = %e, "nothing answered");
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
    use crate::worker::WorkerListener;

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

    /// Through the daemon: a server listening where this node's status says is found; a
    /// daemon that is not up, or a node with nothing listening, finds nothing.
    #[tokio::test]
    async fn the_server_is_found_through_the_local_tailscale() {
        let listener = ServerListener::bind(
            "127.0.0.1:0".parse().unwrap(),
            Admission::with_tailnet(Vec::new(), None),
        )
        .unwrap();
        let port = listener.local_addr().unwrap().port();
        let endpoint = crate::endpoint::bind_lease("127.0.0.1:0".parse().unwrap()).unwrap();
        let (up, _) = slopty_tailnet::fake::daemon(|_| {
            let me = r#"{"BackendState":"Running","Self":{"ID":"n1","HostName":"mac",
                "DNSName":"mac.ts.net.","OS":"macOS","TailscaleIPs":["127.0.0.1"]}}"#;
            (200, me.to_owned())
        })
        .await
        .unwrap();
        let found = find_through(up.clone(), &endpoint, port).await;
        assert_eq!(found.map(|f| (f.name, f.addr.port())), Some(("mac.ts.net".to_owned(), port)));
        let closed = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        assert_eq!(find_through(up, &endpoint, closed.port()).await, None, "nothing listening");
        let (down, _) = slopty_tailnet::fake::daemon(|_| {
            (200, r#"{"BackendState":"Stopped","Self":null}"#.to_owned())
        })
        .await
        .unwrap();
        assert_eq!(find_through(down, &endpoint, port).await, None, "tailscale is down");
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
        let started = tokio::time::Instant::now();
        let answered = answering(&endpoint, vec![found("dead", dead), found("live", live)]).await;
        assert_eq!(answered, [found("live", live)]);
        assert!(
            started.elapsed() < PROBE_TIMEOUT + Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
    }

    /// A listening worker answers the worker probe from a client's endpoint; a closed port
    /// does not; and the tailnet's nodes are tried on the worker port.
    #[tokio::test]
    async fn a_listening_worker_answers_the_worker_probe() {
        let listener = WorkerListener::bind(
            "127.0.0.1:0".parse().unwrap(),
            Admission::with_tailnet(Vec::new(), None),
        )
        .unwrap();
        let live = listener.local_addr().unwrap();
        let dead = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let endpoint = crate::client::bind_client().unwrap();
        let found = |name: &str, addr| Found { name: name.to_owned(), addr };
        let answered =
            answering_workers(&endpoint, vec![found("dead", dead), found("live", live)]).await;
        assert_eq!(answered, [found("live", live)]);
        let status: Status = serde_json::from_str(STATUS).unwrap();
        let ports: Vec<u16> =
            candidates(&status, WORKER_PORT).iter().map(|f| f.addr.port()).collect();
        assert_eq!(ports, [WORKER_PORT; 3], "the three nodes that are not phones");
    }
}
