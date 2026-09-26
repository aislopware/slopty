//! Worker and client in one process over real UDP: connect by address, talk, get refused.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::{Duration, Instant};

    use slopty_core::{ClientId, SessionId, WorkerId};
    use slopty_net::admission::Admission;
    use slopty_net::client::{bind_client, connect, connect_addr};
    use slopty_net::streams::{self, Uni};
    use slopty_net::worker::WorkerListener;
    use slopty_net::{HostAddr, NetError, WorkerMsg};
    use slopty_proto::handshake::{Hello, HelloAck};
    use slopty_proto::terminal::TermEvent;
    use slopty_proto::transfer::{BulkHeader, Purpose};

    fn hello() -> Hello {
        Hello { client: ClientId::new(), name: "test".to_owned() }
    }

    fn ack(worker: WorkerId) -> HelloAck {
        HelloAck { worker, name: "worker".to_owned(), sessions: Vec::new() }
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
                let mut stream =
                    streams::open_session(&client.conn, session, streams::SESSION_STREAM_WAIT)
                        .await
                        .unwrap();
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

    /// A client that lets the worker open no stream at all: the session stream gives up after
    /// its wait with a timeout, where it used to wait for as long as the client liked.
    #[tokio::test]
    async fn a_session_stream_the_client_never_allows_times_out() {
        let (listener, port, id) = worker(Admission::default());
        let wait = Duration::from_millis(300);
        let worker_task = tokio::spawn(async move {
            let mut client = listener.accept().await.unwrap();
            client.tx.send(&WorkerMsg::HelloAck(ack(id))).await.unwrap();
            let started = Instant::now();
            let opened = streams::open_session(&client.conn, SessionId::new(), wait).await;
            (opened.map(drop), started.elapsed(), client)
        });
        let endpoint = bind_client().unwrap();
        let mut transport = slopty_net::endpoint::transport_config();
        transport.max_concurrent_uni_streams(0_u32.into());
        let mut config = slopty_net::crypto::client_config();
        config.transport_config(std::sync::Arc::new(transport));
        endpoint.set_default_client_config(config);
        let at = SocketAddr::from(([127, 0, 0, 1], port));
        let conn = connect_addr(&endpoint, at, hello()).await.unwrap();

        let (opened, took, _client) =
            tokio::time::timeout(Duration::from_secs(5), worker_task).await.unwrap().unwrap();
        assert!(matches!(opened, Err(NetError::TimedOut(_))), "{opened:?}");
        assert!(took >= wait && took < wait.saturating_mul(3), "gave up after {took:?}");
        conn.close();
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
        assert!(matches!(&err, NetError::Connect(why) if why.contains("refused")), "{err:?}");
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
        assert!(matches!(err, NetError::Connect(_)), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(8), "{:?}", started.elapsed());
    }

    /// One address in front of a worker that can be swapped for another, which is how a worker
    /// killed and started again on its port looks to a client: the old one goes unheard (a
    /// SIGKILL says no goodbye) and the new one answers. Returns the front's address and the
    /// switch.
    async fn front(first: SocketAddr) -> (SocketAddr, tokio::sync::watch::Sender<SocketAddr>) {
        let loopback = SocketAddr::from(([127, 0, 0, 1], 0));
        let front = tokio::net::UdpSocket::bind(loopback).await.unwrap();
        let addr = front.local_addr().unwrap();
        let (swap, mut target) = tokio::sync::watch::channel(first);
        tokio::spawn(async move {
            let mut client = None;
            let (mut up, mut down) = ([0_u8; 2048], [0_u8; 2048]);
            loop {
                // A socket per worker: what the old one still sends reaches a socket nobody reads.
                let behind = tokio::net::UdpSocket::bind(loopback).await.unwrap();
                let to = *target.borrow_and_update();
                behind.connect(to).await.unwrap();
                loop {
                    tokio::select! {
                        Ok((n, from)) = front.recv_from(&mut up) => {
                            client = Some(from);
                            let _sent = behind.send(&up[..n]).await;
                        }
                        Ok(n) = behind.recv(&mut down), if client.is_some() => {
                            if let Some(client) = client {
                                let _sent = front.send_to(&down[..n], client).await;
                            }
                        }
                        changed = target.changed() => {
                            if changed.is_err() {
                                return;
                            }
                            break;
                        }
                    }
                }
            }
        });
        (addr, swap)
    }

    /// A client idle on a worker behind [`front`]; the worker, and the switch to put another
    /// one there.
    async fn idle_on_a_worker()
    -> (slopty_net::client::WorkerConn, WorkerListener, tokio::sync::watch::Sender<SocketAddr>)
    {
        let (old, old_port, id) = worker(Admission::default());
        answer_every_hello(&old, id);
        let (addr, swap) = front(SocketAddr::from(([127, 0, 0, 1], old_port))).await;
        let endpoint = bind_client().unwrap();
        let conn = connect_addr(&endpoint, addr, hello()).await.unwrap();
        // Idle, so the next packet the client sends is its keep-alive.
        tokio::time::sleep(slopty_net::endpoint::KEEP_ALIVE.checked_div(2).unwrap()).await;
        (conn, old, swap)
    }

    /// The worker behind `swap` killed and started again: a new one answers at its address.
    fn restart(swap: &tokio::sync::watch::Sender<SocketAddr>) -> WorkerListener {
        let (new, new_port, _) = worker(Admission::default());
        swap.send(SocketAddr::from(([127, 0, 0, 1], new_port))).unwrap();
        new
    }

    /// A worker killed and started again at the same address answers the old connection's next
    /// packet, a bare keep-alive, with a stateless reset, so the client knows within a
    /// keep-alive instead of at its silence bar. Both halves are needed: every Slopty process
    /// takes the same connection IDs as its own, and every packet is long enough to draw a
    /// reset.
    #[tokio::test]
    async fn a_restarted_worker_resets_the_old_connection_within_a_keep_alive() {
        let (conn, _old, swap) = idle_on_a_worker().await;
        let _new = restart(&swap);
        let restarted = Instant::now();
        let keep_alive = slopty_net::endpoint::KEEP_ALIVE;
        let why = tokio::time::timeout(keep_alive.saturating_mul(3), conn.conn.closed()).await;
        let elapsed = restarted.elapsed();
        assert!(matches!(why, Ok(noq::ConnectionError::Reset)), "{why:?} after {elapsed:?}");
        assert!(elapsed < keep_alive.saturating_mul(2), "reset {elapsed:?} after the restart");
        println!("MEASURE restart: reset {elapsed:?} after the restart, on the keep-alive");
    }

    /// A ping goes out at once, not on the keep-alive or probe timer: a client that has stopped
    /// hearing its worker finds a restarted one on its next tick.
    #[tokio::test]
    async fn a_ping_draws_a_restarted_worker_s_reset_at_once() {
        let (conn, _old, swap) = idle_on_a_worker().await;
        let _new = restart(&swap);
        let restarted = Instant::now();
        slopty_net::endpoint::ping(&conn.conn);
        let keep_alive = slopty_net::endpoint::KEEP_ALIVE;
        let why = tokio::time::timeout(keep_alive, conn.conn.closed()).await;
        let elapsed = restarted.elapsed();
        assert!(matches!(why, Ok(noq::ConnectionError::Reset)), "{why:?} after {elapsed:?}");
        assert!(elapsed < keep_alive.checked_div(10).unwrap(), "reset {elapsed:?} after the ping");
    }

    /// Datagrams queued ahead of a session frame, over a link that takes 1.4 s to carry them: the
    /// frame leaves in the next packet. With `SLOPTY_DATAGRAMS_FIRST=1` it waits for all of them.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_frame_overtakes_queued_datagrams() {
        const DATAGRAMS: u64 = 1_200;
        let listener =
            WorkerListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)), Admission::default())
                .unwrap();
        let worker_addr = listener.local_addr().unwrap();
        let link = slopty_shape::Link {
            delay: Duration::from_millis(1),
            jitter: Duration::ZERO,
            loss: 0.0,
            rate: 1_000_000,
            queue: 1_000_000,
        };
        let relay = std::sync::Arc::new(
            slopty_shape::relay::Relay::bind(
                SocketAddr::from(([127, 0, 0, 1], 0)),
                worker_addr,
                link,
                1,
            )
            .await
            .unwrap(),
        );
        let relayed = {
            let relay = std::sync::Arc::clone(&relay);
            tokio::spawn(async move { relay.run().await })
        };
        let (go, flood) = tokio::sync::oneshot::channel::<()>();
        let worker_task = tokio::spawn(async move {
            let mut client = listener.accept().await.unwrap();
            client.tx.send(&WorkerMsg::HelloAck(ack(WorkerId::new()))).await.unwrap();
            let mut session =
                streams::open_session(&client.conn, SessionId::new(), streams::SESSION_STREAM_WAIT)
                    .await
                    .unwrap();
            session.send(&TermEvent::Bell).await.unwrap();
            flood.await.unwrap();
            // Past start-up: while the worker still acknowledges the client's packets, an ACK
            // leaves no room for a full datagram and stream data takes the packet in either order.
            tokio::time::sleep(Duration::from_millis(500)).await;
            let size = client.conn.max_datagram_size().unwrap();
            for _ in 0..DATAGRAMS {
                client.conn.send_datagram(vec![0_u8; size].into()).unwrap();
            }
            session.send(&TermEvent::Bell).await.unwrap();
            client.conn.closed().await;
        });
        let endpoint = bind_client().unwrap();
        let conn = connect_addr(&endpoint, relay.addr().unwrap(), hello()).await.unwrap();
        let Uni::Session { rx: mut events, .. } = streams::accept_uni(&conn.conn).await.unwrap()
        else {
            panic!("a session stream");
        };
        assert_eq!(events.recv().await.unwrap(), TermEvent::Bell);
        go.send(()).unwrap();

        let echo = tokio::time::timeout(Duration::from_secs(10), events.recv()).await.unwrap();
        let at_echo = conn.conn.stats().frame_rx.datagram;
        assert_eq!(echo.unwrap(), TermEvent::Bell);
        assert!(at_echo < DATAGRAMS / 4, "the frame came after {at_echo} datagrams");
        conn.close();
        worker_task.await.unwrap();
        relayed.abort();
    }
}
