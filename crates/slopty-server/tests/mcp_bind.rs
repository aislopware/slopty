//! The MCP listener shares its port with no IPv4 socket (docs/decisions/transport.md, "A
//! dual-stack port no IPv4 socket holds", which found the same XNU rule for UDP).

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::net::TcpListener;

    use slopty_net::endpoint::any;

    const HELD: usize = 100;
    const BINDS: usize = 1000;

    #[tokio::test]
    async fn a_fresh_listener_takes_no_port_an_ipv4_socket_holds() {
        let held: Vec<TcpListener> = (0..HELD)
            .flat_map(|_| ["127.0.0.1:0", "0.0.0.0:0"])
            .map(|at| TcpListener::bind(at).unwrap())
            .collect();
        let ports: HashSet<u16> = held.iter().map(|s| s.local_addr().unwrap().port()).collect();
        let shared = (0..BINDS)
            .filter(|_| {
                let listener = slopty_server::mcp::bind(any(0)).unwrap();
                ports.contains(&listener.local_addr().unwrap().port())
            })
            .count();
        assert_eq!(shared, 0, "{shared} of {BINDS} listeners on a port an IPv4 socket holds");
    }

    /// XNU let `[::]:p` bind beside it, and every IPv4 connection went to the other socket. A
    /// holder on one IPv4 address is BSD's `SO_REUSEADDR` rule and lets a wildcard through on
    /// IPv4 alone too, so only the wildcard holder is asked about.
    #[tokio::test]
    async fn a_listener_on_a_port_the_ipv4_wildcard_holds_is_refused() {
        let holder = TcpListener::bind("0.0.0.0:0").unwrap();
        let port = holder.local_addr().unwrap().port();
        match slopty_server::mcp::bind(any(port)) {
            Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::AddrInUse),
            Ok(listener) => panic!("bound beside it at {:?}", listener.local_addr()),
        }
    }
}
