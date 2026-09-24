//! Worker and client in one process over real UDP: connect by address, talk, get refused.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::{Duration, Instant};

    use slopty_core::{ClientId, SessionId, WorkerId};
    use slopty_net::admission::Admission;
    use slopty_net::client::{HandshakeError, bind_client, connect, connect_addr};
    use slopty_net::streams::{self, Uni};
    use slopty_net::worker::WorkerListener;
    use slopty_net::{HostAddr, NetError, WorkerMsg};
    use slopty_proto::PROTOCOL_VERSION;
    use slopty_proto::handshake::{Caps, ClientKind, Hello, HelloAck, Rejection};
    use slopty_proto::terminal::TermEvent;
    use slopty_proto::transfer::{BulkHeader, Purpose};

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
            name: "worker".to_owned(),
            app_version: "0".to_owned(),
            caps: Caps::empty(),
            sessions: Vec::new(),
        }
    }

    /// A worker on every interface, any port, answering each `Hello` with `ack(id)` and holding
    /// the connection until the client closes it.
    fn worker(admission: Admission) -> (WorkerListener, u16, WorkerId) {
        let listener = WorkerListener::bind(slopty_net::endpoint::any(0), admission).unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port, WorkerId::new())
    }

    fn answer_every_hello(listener: &WorkerListener, id: WorkerId) {
        let listener = listener.clone();
        tokio::spawn(async move {
            while let Some(mut client) = listener.accept().await {
                assert!(client.remote.ip().is_loopback(), "{}", client.remote);
                client.tx.send(&WorkerMsg::HelloAck(ack(id))).await.unwrap();
                tokio::spawn(async move { client.conn.closed().await });
            }
        });
    }

    #[tokio::test]
    async fn a_client_connects_by_address_and_streams_a_session() {
        let (listener, port, id) = worker(Admission::default());
        let session = SessionId::new();
        let worker_task = {
            let listener = listener.clone();
            tokio::spawn(async move {
                let mut client = listener.accept().await.unwrap();
                assert_eq!(client.hello.name, "test");
                assert_eq!(client.remote.ip(), std::net::Ipv4Addr::LOCALHOST, "canonical IPv4");
                client.tx.send(&WorkerMsg::HelloAck(ack(id))).await.unwrap();
                let mut stream = streams::open_session(&client.conn, session).await.unwrap();
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
        let Uni::Session { session: opened, rx: mut events } =
            streams::accept_uni(&conn.conn).await.unwrap()
        else {
            panic!("a session stream");
        };
        assert_eq!(opened, session);
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
        tokio::time::timeout(Duration::from_secs(10), worker_task).await.unwrap().unwrap();
    }

    fn header(size: u64, purpose: Purpose) -> BulkHeader {
        BulkHeader {
            xfer: slopty_core::XferId::new(),
            purpose,
            name: "dir/f.bin".to_owned(),
            size,
            mtime_ms: 1,
            mode: 0o644,
            offset: 0,
        }
    }

    /// Everything left on a raw stream.
    async fn drain(rx: &mut streams::RawRecv) -> Vec<u8> {
        let mut got = Vec::new();
        while let Some(chunk) = rx.chunk(64 << 10).await.unwrap() {
            got.extend_from_slice(&chunk);
        }
        got
    }

    /// A file up, a file down, and a tunnel echoing, all beside the control stream.
    #[tokio::test]
    async fn bulk_streams_carry_raw_bytes_both_ways_and_a_tunnel_echoes() {
        let (listener, port, id) = worker(Admission::default());
        let up: Vec<u8> = (0..3_000_000_u32).map(|i| (i % 251) as u8).collect();
        let down = b"the worker's file".to_vec();
        let worker_task = {
            let (listener, up, down) = (listener.clone(), up.clone(), down.clone());
            tokio::spawn(async move {
                let mut client = listener.accept().await.unwrap();
                client.tx.send(&WorkerMsg::HelloAck(ack(id))).await.unwrap();
                let Uni::Bulk { header, mut rx } = streams::accept_uni(&client.conn).await.unwrap()
                else {
                    panic!("a bulk stream");
                };
                assert_eq!((header.purpose, header.size), (Purpose::Upload, up.len() as u64));
                assert!(drain(&mut rx).await == up, "the upload arrives whole and in order");
                let mut send = streams::open_bulk(
                    &client.conn,
                    self::header(down.len() as u64, Purpose::Download),
                )
                .await
                .unwrap();
                send.write_all(&down).await.unwrap();
                send.finish().unwrap();
                let (open, mut send, mut rx) = streams::accept_tunnel(&client.conn).await.unwrap();
                assert_eq!(open.port, 5173);
                let echo = drain(&mut rx).await;
                send.write_all(&echo).await.unwrap();
                send.finish().unwrap();
                client.conn.closed().await;
            })
        };
        let endpoint = bind_client().unwrap();
        let conn = connect_addr(&endpoint, SocketAddr::from(([127, 0, 0, 1], port)), hello())
            .await
            .unwrap();
        let mut send =
            streams::open_bulk(&conn.conn, header(up.len() as u64, Purpose::Upload)).await.unwrap();
        send.write_all(&up).await.unwrap();
        send.finish().unwrap();
        let Uni::Bulk { header: got, mut rx } = streams::accept_uni(&conn.conn).await.unwrap()
        else {
            panic!("a bulk stream");
        };
        assert_eq!(got.purpose, Purpose::Download);
        assert_eq!(drain(&mut rx).await, down);
        let (mut send, mut rx) = streams::open_tunnel(&conn.conn, 5173).await.unwrap();
        send.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
        send.finish().unwrap();
        assert_eq!(drain(&mut rx).await, b"GET / HTTP/1.1\r\n\r\n", "half-close ends the echo");
        conn.close();
        tokio::time::timeout(Duration::from_secs(10), worker_task).await.unwrap().unwrap();
    }

    /// One socket answers both families, and a name resolves.
    #[tokio::test]
    async fn a_worker_on_every_interface_answers_ipv4_ipv6_and_a_name() {
        let (listener, port, id) = worker(Admission::default());
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
        let (listener, port, _id) = worker(Admission::default());
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
            matches!(err, HandshakeError::Rejected(Rejection::ProtocolVersion { worker }) if worker == PROTOCOL_VERSION),
            "{err:?}"
        );
        assert!(!drained.is_finished(), "a rejected client never reaches the caller");
        drained.abort();
    }

    /// A peer outside the admitted ranges is refused at its first packet: the client hears so
    /// at once rather than waiting out a timeout, and the caller never sees it. The peer here is
    /// this machine's own link-local address on `lo0`, which is not loopback, against a worker
    /// that admits only 10/8.
    #[tokio::test]
    async fn a_peer_outside_the_admitted_ranges_is_refused_before_the_handshake() {
        let (listener, port, id) = worker(Admission::new(vec!["10.0.0.0/8".parse().unwrap()]));
        let seen = {
            let listener = listener.clone();
            tokio::spawn(async move {
                let mut client = listener.accept().await.unwrap();
                client.tx.send(&WorkerMsg::HelloAck(ack(id))).await.unwrap();
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
