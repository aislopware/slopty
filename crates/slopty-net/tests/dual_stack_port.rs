//! A dual-stack endpoint shares its port with no IPv4 socket.
//!
//! XNU hands `[::]:0` a port an IPv4 socket holds, and IPv4 datagrams to that port then reach
//! the IPv4 socket. A fresh client that drew a loopback server's own port sent its Initial to
//! the server, which answered itself, and the dial got no answer (docs/decisions/transport.md,
//! "A dual-stack port no IPv4 socket holds").

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::net::{SocketAddr, UdpSocket};
    use std::time::Duration;

    use slopty_net::admission::Admission;
    use slopty_net::endpoint::any;
    use slopty_net::server::ServerListener;
    use slopty_net::{HostAddr, NetError};
    use slopty_proto::server::{FromServer, Role};
    use tokio::time::Instant;

    /// IPv4 sockets held on each of loopback and the wildcard. Each fresh dual-stack bind took
    /// one of their ports about one time in 80 before the fix.
    const HELD: usize = 100;
    const BINDS: usize = 1000;

    /// 1 000 fresh endpoints beside 200 IPv4 sockets: none takes a port they hold. Before the
    /// fix about 12 of them did.
    #[tokio::test]
    async fn a_fresh_endpoint_takes_no_port_an_ipv4_socket_holds() {
        let held: Vec<UdpSocket> = (0..HELD)
            .flat_map(|_| ["127.0.0.1:0", "0.0.0.0:0"])
            .map(|at| UdpSocket::bind(at).unwrap())
            .collect();
        let ports: HashSet<u16> = held.iter().map(|s| s.local_addr().unwrap().port()).collect();
        let shared = (0..BINDS)
            .filter(|_| {
                let endpoint = slopty_net::client::bind_client().unwrap();
                ports.contains(&endpoint.local_addr().unwrap().port())
            })
            .count();
        assert_eq!(shared, 0, "{shared} of {BINDS} endpoints on a port an IPv4 socket holds");
    }

    /// A port a socket on `0.0.0.0` holds is in use for an endpoint on every interface, as it
    /// is on Linux. XNU let the bind through, and the endpoint never heard an IPv4 packet.
    #[tokio::test]
    async fn an_endpoint_on_a_port_an_ipv4_socket_holds_is_refused() {
        for at in ["0.0.0.0:0", "127.0.0.1:0"] {
            let holder = UdpSocket::bind(at).unwrap();
            let port = holder.local_addr().unwrap().port();
            match slopty_net::endpoint::bind(any(port), true) {
                Err(NetError::Bind { source, .. }) => {
                    assert_eq!(source.kind(), std::io::ErrorKind::AddrInUse, "{at}");
                }
                Err(other) => panic!("{at}: {other}"),
                Ok(endpoint) => panic!("{at}: bound beside it at {:?}", endpoint.local_addr()),
            }
        }
    }

    /// The fill's load in one process: fresh endpoints dialing a loopback server that pushes to
    /// every link every 2 ms, `TASKS` at a time. Prints the dials that got no answer and the
    /// cost of a bind.
    ///
    /// `ROUNDS=100000 TASKS=8 cargo test -p slopty-net --release --test dual_stack_port --
    /// --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    #[ignore = "a measurement: docs/MEASUREMENTS.md, a dual-stack port no IPv4 socket holds"]
    async fn fresh_endpoints_dial_a_loopback_server() {
        let listener =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::default()).unwrap();
        let at = listener.local_addr().unwrap();
        tokio::spawn(push_to_every_link(listener));
        let var = |name: &str, default: usize| {
            std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
        };
        let (rounds, tasks) = (var("ROUNDS", 20_000), var("TASKS", 8));
        let addr: HostAddr = at.to_string().parse().unwrap();
        let started = Instant::now();
        let dialers: Vec<_> =
            std::iter::repeat_with(|| tokio::spawn(dial_repeatedly(addr.clone(), rounds / tasks)))
                .take(tasks)
                .collect();
        let mut failed = Vec::new();
        for dialer in dialers {
            failed.extend(dialer.await.unwrap());
        }
        for (port, why) in &failed {
            eprintln!("client port {port}: {why}");
        }
        eprintln!(
            "server {at}: {} of {rounds} dials failed in {:.1} s",
            failed.len(),
            started.elapsed().as_secs_f64()
        );
        bind_cost();
    }

    /// Welcome every link, then send it a directory every 2 ms until it closes.
    async fn push_to_every_link(listener: ServerListener) {
        while let Some(mut link) = listener.accept().await {
            tokio::spawn(async move {
                let welcome = FromServer::Welcome {
                    name: "server".to_owned(),
                    link: 1,
                    build: String::new(),
                };
                if link.tx.send(&welcome).await.is_err() {
                    return;
                }
                let push = FromServer::Directory(Vec::new());
                loop {
                    tokio::select! {
                        sent = link.tx.send(&push) => if sent.is_err() { break },
                        _closed = link.conn.closed() => break,
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            });
        }
    }

    /// `rounds` links, each on a fresh endpoint: dial, read one push, close. The failed ones
    /// with their client port.
    async fn dial_repeatedly(addr: HostAddr, rounds: usize) -> Vec<(u16, String)> {
        let mut failed = Vec::new();
        for _ in 0..rounds {
            let endpoint = slopty_net::client::bind_client().unwrap();
            let port = endpoint.local_addr().unwrap().port();
            let role = Role::Client { name: "cli".to_owned() };
            match slopty_net::server::connect(&endpoint, &addr, role).await {
                Ok(mut link) => {
                    let _push =
                        tokio::time::timeout(Duration::from_millis(5), link.rx.recv()).await;
                    link.close();
                }
                Err(e) => failed.push((port, e.to_string())),
            }
        }
        failed
    }

    /// What the IPv4 bind ahead of each dual-stack one costs.
    fn bind_cost() {
        const N: u32 = 10_000;
        let started = Instant::now();
        for _ in 0..N {
            drop(UdpSocket::bind("0.0.0.0:0").unwrap());
        }
        let probe = started.elapsed().checked_div(N).unwrap_or_default();
        let started = Instant::now();
        for _ in 0..N {
            drop(slopty_net::client::bind_client().unwrap());
        }
        let endpoint = started.elapsed().checked_div(N).unwrap_or_default();
        let wildcard: SocketAddr = "[::]:0".parse().unwrap();
        eprintln!("per bind: IPv4 bind and close {probe:?}; endpoint on {wildcard} {endpoint:?}");
    }
}
