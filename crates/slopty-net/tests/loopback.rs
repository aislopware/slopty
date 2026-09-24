//! Host and client in one process over real UDP: connect by address, talk, get refused.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::{Duration, Instant};

    use slopty_core::{ClientId, SessionId, WorkerId};
    use slopty_net::admission::Admission;
    use slopty_net::client::{HandshakeError, bind_client, connect, connect_addr};
    use slopty_net::host::{HostListener, open_session_stream};
    use slopty_net::{HostAddr, HostMsg, NetError};
    use slopty_proto::PROTOCOL_VERSION;
    use slopty_proto::handshake::{Caps, ClientKind, Hello, HelloAck, Rejection};
    use slopty_proto::terminal::TermEvent;

    fn hello() -> Hello {
        Hello {
            protocol: PROTOCOL_VERSION,
            client: ClientId::new(),
            kind: ClientKind::Tool,
            name: "test".to_owned(),
            app_version: "0".to_owned(),
            caps: Caps::empty(),
        }
    }

    fn ack(worker: WorkerId) -> HelloAck {
        HelloAck {
            protocol: PROTOCOL_VERSION,
            worker,
            name: "host".to_owned(),
            app_version: "0".to_owned(),
            caps: Caps::empty(),
            sessions: Vec::new(),
        }
    }

    /// A host on every interface, any port, answering each `Hello` with `ack(id)` and holding
    /// the connection until the client closes it.
    fn host(admission: Admission) -> (HostListener, u16, WorkerId) {
        let listener = HostListener::bind(slopty_net::endpoint::any(0), admission).unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port, WorkerId::new())
    }

    fn answer_every_hello(listener: &HostListener, id: WorkerId) {
        let listener = listener.clone();
        tokio::spawn(async move {
            while let Some(mut client) = listener.accept().await {
                assert!(client.remote.ip().is_loopback(), "{}", client.remote);
                client.tx.send(&HostMsg::HelloAck(ack(id))).await.unwrap();
                tokio::spawn(async move { client.conn.closed().await });
            }
        });
    }

    #[tokio::test]
    async fn a_client_connects_by_address_and_streams_a_session() {
        let (listener, port, id) = host(Admission::default());
        let session = SessionId::new();
        let host_task = {
            let listener = listener.clone();
            tokio::spawn(async move {
                let mut client = listener.accept().await.unwrap();
                assert_eq!(client.hello.name, "test");
                assert_eq!(client.remote.ip(), std::net::Ipv4Addr::LOCALHOST, "canonical IPv4");
                client.tx.send(&HostMsg::HelloAck(ack(id))).await.unwrap();
                let mut stream = open_session_stream(&client.conn, session).await.unwrap();
                stream.send(&TermEvent::Bell).await.unwrap();
                // A pre-encoded frame (the fan-out path), then a graceful end.
                let raw = slopty_proto::codec::encode(&TermEvent::Bell).unwrap();
                stream.send_raw(&raw).await.unwrap();
                stream.finish().unwrap();
                client.conn.closed().await;
            })
        };
        let endpoint = bind_client().unwrap();
        let addr: HostAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let conn = connect(&endpoint, &addr, hello()).await.unwrap();
        assert_eq!(conn.ack.worker, id);
        assert_eq!(conn.remote, SocketAddr::from(([127, 0, 0, 1], port)));
        let (header, mut events) = conn.accept_session_stream().await.unwrap();
        assert_eq!(header.session, session);
        assert_eq!(events.recv().await.unwrap(), TermEvent::Bell);
        assert_eq!(events.recv().await.unwrap(), TermEvent::Bell, "the raw frame");
        let end = tokio::time::timeout(Duration::from_secs(5), events.recv()).await.unwrap();
        assert!(matches!(end, Err(NetError::Closed)), "finished: {end:?}");

        // The path is measured and described.
        let rtt = conn.rtt().expect("a handshake measures the rtt");
        assert!(rtt > Duration::ZERO, "{rtt:?}");
        assert!(slopty_net::endpoint::received_datagrams(&conn.conn) > 1, "a handshake is several");
        let path = slopty_net::endpoint::describe_path(&conn.conn);
        assert!(path.starts_with(&format!("127.0.0.1:{port} rtt")), "{path}");
        let health = slopty_net::endpoint::describe_health(&conn.conn);
        assert!(health.contains("cwnd") && health.contains("mtu"), "{health}");
        let (srtt, cwnd) = slopty_net::endpoint::path_rtt_cwnd(&conn.conn).unwrap();
        assert!(srtt > Duration::ZERO && cwnd > 1, "{srtt:?} {cwnd}");
        let max = conn.conn.max_datagram_size().unwrap();
        assert!(max >= 1150, "no AEAD tag eats into a datagram: {max}");
        conn.close();
        tokio::time::timeout(Duration::from_secs(10), host_task).await.unwrap().unwrap();
    }

    /// One socket answers both families, and a name resolves.
    #[tokio::test]
    async fn a_host_on_every_interface_answers_ipv4_ipv6_and_a_name() {
        let (listener, port, id) = host(Admission::default());
        answer_every_hello(&listener, id);
        let endpoint = bind_client().unwrap();
        for addr in
            [format!("127.0.0.1:{port}"), format!("[::1]:{port}"), format!("localhost:{port}")]
        {
            let addr: HostAddr = addr.parse().unwrap();
            let conn =
                tokio::time::timeout(Duration::from_secs(5), connect(&endpoint, &addr, hello()))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(conn.ack.worker, id, "{addr}");
            conn.close();
        }
    }

    #[tokio::test]
    async fn another_protocol_version_is_rejected_readably() {
        let (listener, port, _id) = host(Admission::default());
        let drained = {
            let listener = listener.clone();
            tokio::spawn(async move { listener.accept().await.is_none() })
        };
        let endpoint = bind_client().unwrap();
        let old = Hello { protocol: PROTOCOL_VERSION.wrapping_sub(1), ..hello() };
        let err = connect_addr(&endpoint, SocketAddr::from(([127, 0, 0, 1], port)), old)
            .await
            .unwrap_err();
        assert!(
            matches!(err, HandshakeError::Rejected(Rejection::ProtocolVersion { host }) if host == PROTOCOL_VERSION),
            "{err:?}"
        );
        assert!(!drained.is_finished(), "a rejected client never reaches the caller");
        drained.abort();
    }

    /// A peer outside the admitted ranges is refused at its first packet: the client hears so
    /// at once rather than waiting out a timeout, and the caller never sees it. The peer here is
    /// this machine's own link-local address on `lo0`, which is not loopback, against a host
    /// that admits only 10/8.
    #[tokio::test]
    async fn a_peer_outside_the_admitted_ranges_is_refused_before_the_handshake() {
        let (listener, port, id) = host(Admission::new(vec!["10.0.0.0/8".parse().unwrap()]));
        let seen = {
            let listener = listener.clone();
            tokio::spawn(async move {
                let mut client = listener.accept().await.unwrap();
                client.tx.send(&HostMsg::HelloAck(ack(id))).await.unwrap();
                client
            })
        };
        let endpoint = bind_client().unwrap();
        let link_local: SocketAddr = format!("[fe80::1%1]:{port}").parse().unwrap();
        let started = Instant::now();
        let err = connect_addr(&endpoint, link_local, hello()).await.unwrap_err();
        let took = started.elapsed();
        assert!(
            matches!(&err, HandshakeError::Net(NetError::Connect(why)) if why.contains("refused")),
            "{err:?}"
        );
        assert!(took < Duration::from_secs(1), "refused at once, not timed out: {took:?}");

        // Loopback is admitted whatever the list says, and it is the first client through.
        let loopback = SocketAddr::from(([127, 0, 0, 1], port));
        let conn = connect_addr(&endpoint, loopback, hello()).await.unwrap();
        let first = tokio::time::timeout(Duration::from_secs(5), seen).await.unwrap().unwrap();
        assert!(first.remote.ip().is_loopback(), "the refused peer never got through");
        conn.close();
    }

    #[tokio::test]
    async fn an_address_with_nothing_behind_it_fails_in_seconds() {
        let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = silent.local_addr().unwrap();
        let endpoint = bind_client().unwrap();
        let started = Instant::now();
        let err = connect_addr(&endpoint, addr, hello()).await.unwrap_err();
        assert!(matches!(err, HandshakeError::Net(NetError::Connect(_))), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(8), "{:?}", started.elapsed());
    }
}
