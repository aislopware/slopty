//! The client's link against a scripted worker in-process, over UDP on loopback: an upload
//! resumed after the worker cut its stream, a download and one resumed after a cut, a clipboard
//! representation fetched on paste, a port forwarded to a real local connection, and one port of
//! the worker served by two clients on this machine at two local ports, and a dial the worker
//! closes because the tailnet grants this device no client role.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use slopty_client::clip::Fetched;
    use slopty_client::{LinkEvent, WorkerLink};
    use slopty_core::{ClientId, SessionId, WallMs, WorkerId, XferId};
    use slopty_net::admission::Admission;
    use slopty_net::client::{bind_client, connect};
    use slopty_net::streams::{self, RawRecv, Uni};
    use slopty_net::worker::close_code::NOT_GRANTED;
    use slopty_net::worker::{AcceptedClient, WorkerListener};
    use slopty_net::{ClientMsg, HostAddr, WorkerMsg};
    use slopty_proto::handshake::{Hello, HelloAck};
    use slopty_proto::orchestration::Port;
    use slopty_proto::transfer::{
        BulkHeader, ClipEntry, ClipFormat, ClipMsg, ClipType, Dest, Offer, Peer, Purpose, Rep,
        XferMsg,
    };
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::sync::mpsc;

    const WAIT: Duration = Duration::from_secs(20);

    fn hello() -> Hello {
        Hello { client: ClientId::new(), name: "test".to_owned() }
    }

    /// A worker that greets one client and hands its end to the test, and the client's link.
    async fn pair() -> (AcceptedClient, WorkerLink, mpsc::Receiver<LinkEvent>) {
        let listener =
            WorkerListener::bind(slopty_net::endpoint::any(0), Admission::default()).unwrap();
        let port = listener.local_addr().unwrap().port();
        let worker = WorkerId::new();
        let accepted = tokio::spawn(async move {
            let mut client = listener.accept().await.unwrap();
            let ack = HelloAck {
                worker,
                name: "worker".to_owned(),
                home: String::new(),
                caps: slopty_proto::server::WorkerCaps::bare(slopty_proto::server::Os::MacOs),
                load: 0.0,
                sessions: Vec::new(),
            };
            client.tx.send(&WorkerMsg::HelloAck(ack)).await.unwrap();
            client
        });
        let endpoint = bind_client().unwrap();
        let addr: HostAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let conn = connect(&endpoint, &addr, hello()).await.unwrap();
        let mut link = WorkerLink::start_forwarding(conn);
        let events = link.events().unwrap();
        let client = tokio::time::timeout(WAIT, accepted).await.unwrap().unwrap();
        (client, link, events)
    }

    /// The next control message the client sends that `pick` wants.
    async fn expect<T>(client: &mut AcceptedClient, pick: impl Fn(ClientMsg) -> Option<T>) -> T {
        loop {
            let msg = tokio::time::timeout(WAIT, client.rx.recv()).await.unwrap().unwrap();
            if let Some(found) = pick(msg) {
                return found;
            }
        }
    }

    async fn bulk(client: &AcceptedClient) -> (BulkHeader, RawRecv) {
        match tokio::time::timeout(WAIT, streams::accept_uni(&client.conn)).await.unwrap().unwrap()
        {
            Uni::Bulk { header, rx } => (header, rx),
            Uni::Session { .. } => panic!("a bulk stream"),
            Uni::Conversation { .. } => panic!("a conversation stream"),
        }
    }

    async fn drain(rx: &mut RawRecv) -> Vec<u8> {
        let mut got = Vec::new();
        while let Some(chunk) = rx.chunk(64 << 10).await.unwrap() {
            got.extend_from_slice(&chunk);
        }
        got
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_upload_cut_part_way_resumes_from_what_the_worker_holds() {
        let (mut client, link, _events) = pair().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big file.bin");
        let content: Vec<u8> = (0..3_000_000_u32).map(|i| (i % 253) as u8).collect();
        std::fs::write(&path, &content).unwrap();
        let session = SessionId::new();
        let xfer = XferId::new();
        link.remote().upload(xfer, vec![path], Dest::SessionCwd(session));

        let (dest, files, bytes) = expect(&mut client, |m| match m {
            ClientMsg::Xfer(XferMsg::Begin { dest, files, bytes, .. }) => {
                Some((dest, files, bytes))
            }
            _ => None,
        })
        .await;
        assert_eq!((dest, files, bytes), (Some(Dest::SessionCwd(session)), 1, 3_000_000));

        // The first stream: the worker keeps a prefix and cuts the rest.
        let (header, mut rx) = bulk(&client).await;
        assert_eq!((header.name.as_str(), header.offset), ("big file.bin", 0));
        assert_eq!(header.purpose, Purpose::Upload);
        let mut held = Vec::new();
        while held.len() < 1_000_000 {
            held.extend_from_slice(&rx.chunk(64 << 10).await.unwrap().unwrap());
        }
        rx.stop();
        let name = expect(&mut client, |m| match m {
            ClientMsg::Xfer(XferMsg::Resume { xfer: x, name }) if x == xfer => Some(name),
            _ => None,
        })
        .await;
        let durable = held.len() as u64;
        let offset = XferMsg::Offset { xfer, name, durable };
        client.tx.send(&WorkerMsg::Xfer(offset)).await.unwrap();

        let (header, mut rx) = bulk(&client).await;
        assert_eq!(header.offset, durable, "sent again from what the worker holds");
        held.extend_from_slice(&drain(&mut rx).await);
        assert!(held == content, "the file arrives whole across the cut");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_download_lands_in_place_and_answers_the_blocking_caller() {
        let (mut client, link, _events) = pair().await;
        let into = tempfile::tempdir().unwrap();
        let remote = link.remote();
        let target = into.path().to_owned();
        let waiting =
            std::thread::spawn(move || remote.download("~/notes/todo.txt".to_owned(), target));
        let (xfer, path, held) = expect(&mut client, |m| match m {
            ClientMsg::Xfer(XferMsg::Fetch { xfer, path, held }) => Some((xfer, path, held)),
            _ => None,
        })
        .await;
        assert!(held.is_empty(), "a first fetch holds nothing");
        assert_eq!(path, "~/notes/todo.txt");
        let body = b"milk\neggs\n";
        let header = BulkHeader {
            xfer,
            purpose: Purpose::Download,
            name: "todo.txt".to_owned(),
            size: body.len() as u64,
            mtime_ms: WallMs::ZERO,
            mode: 0o640,
            offset: 0,
        };
        let mut send = streams::open_bulk(&client.conn, header).await.unwrap();
        send.write_all(body).await.unwrap();
        send.finish().unwrap();
        let begin = XferMsg::Begin { xfer, dest: None, files: 1, bytes: body.len() as u64 };
        client.tx.send(&WorkerMsg::Xfer(begin)).await.unwrap();
        client.tx.send(&WorkerMsg::Xfer(done(xfer, "todo.txt", body))).await.unwrap();
        let landed =
            tokio::task::spawn_blocking(move || waiting.join().unwrap()).await.unwrap().unwrap();
        let expected = into.path().join("todo.txt");
        assert_eq!(landed, std::slice::from_ref(&expected));
        assert_eq!(std::fs::read(&expected).unwrap(), body);
        assert!(!PathBuf::from(format!("{}.partial", expected.display())).exists());
    }

    /// The worker's word that `name` is sent, with the digest of all of it.
    fn done(xfer: XferId, name: &str, whole: &[u8]) -> XferMsg {
        let (name, path) = (name.to_owned(), format!("/w/{name}"));
        XferMsg::Done { xfer, name, path, hash: *blake3::hash(whole).as_bytes() }
    }

    fn download_header(xfer: XferId, name: &str, size: usize, offset: u64) -> BulkHeader {
        BulkHeader {
            xfer,
            purpose: Purpose::Download,
            name: name.to_owned(),
            size: size as u64,
            mtime_ms: WallMs::from_millis(1_700_000_000_000),
            mode: 0o644,
            offset,
        }
    }

    async fn fetched(client: &mut AcceptedClient) -> (XferId, Vec<(String, u64)>) {
        expect(client, |m| match m {
            ClientMsg::Xfer(XferMsg::Fetch { xfer, held, .. }) => Some((xfer, held)),
            _ => None,
        })
        .await
    }

    /// A download whose stream the worker cuts part way fetches again under a new transfer,
    /// naming what it holds; the rest comes from there and the file lands whole, checked
    /// against the digest. A file already landed is held whole and not sent again.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_download_cut_part_way_resumes_from_what_the_client_holds() {
        let (mut client, link, _events) = pair().await;
        let into = tempfile::tempdir().unwrap();
        let remote = link.remote();
        let target = into.path().to_owned();
        let waiting = std::thread::spawn(move || remote.download("~/out".to_owned(), target));
        let (first, held) = fetched(&mut client).await;
        assert!(held.is_empty());
        let small = b"landed first".to_vec();
        let big: Vec<u8> = (0..3_000_000_u32).map(|i| (i % 249) as u8).collect();
        let bytes = (small.len() + big.len()) as u64;
        let begin = XferMsg::Begin { xfer: first, dest: None, files: 2, bytes };
        client.tx.send(&WorkerMsg::Xfer(begin)).await.unwrap();

        let header = download_header(first, "out/small.txt", small.len(), 0);
        let mut send = streams::open_bulk(&client.conn, header).await.unwrap();
        send.write_all(&small).await.unwrap();
        send.finish().unwrap();
        client.tx.send(&WorkerMsg::Xfer(done(first, "out/small.txt", &small))).await.unwrap();

        // The big file: a third of it, then the worker's stream is reset.
        let header = download_header(first, "out/big.bin", big.len(), 0);
        let mut send = streams::open_bulk(&client.conn, header).await.unwrap();
        send.write_all(&big[..1_000_000]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        send.reset(0_u32.into()).unwrap();

        let cancelled = expect(&mut client, |m| match m {
            ClientMsg::Xfer(XferMsg::Cancel { xfer }) => Some(xfer),
            _ => None,
        })
        .await;
        assert_eq!(cancelled, first, "the cut attempt is called off");
        let (second, mut held) = fetched(&mut client).await;
        assert_ne!(second, first, "a retry is a transfer of its own");
        held.sort();
        assert_eq!(held[1], ("out/small.txt".to_owned(), small.len() as u64), "landed: whole");
        let (name, durable) = held[0].clone();
        assert_eq!(name, "out/big.bin");
        assert!(durable > 0 && durable <= 1_000_000, "held {durable}");
        let partial = into.path().join("out/big.bin.partial");
        assert_eq!(std::fs::metadata(&partial).unwrap().len(), durable, "durable on disk");

        let begin = XferMsg::Begin { xfer: second, dest: None, files: 2, bytes };
        client.tx.send(&WorkerMsg::Xfer(begin)).await.unwrap();
        let header = download_header(second, "out/small.txt", small.len(), small.len() as u64);
        streams::open_bulk(&client.conn, header).await.unwrap().finish().unwrap();
        client.tx.send(&WorkerMsg::Xfer(done(second, "out/small.txt", &small))).await.unwrap();
        let header = download_header(second, "out/big.bin", big.len(), durable);
        let mut send = streams::open_bulk(&client.conn, header).await.unwrap();
        send.write_all(&big[usize::try_from(durable).unwrap()..]).await.unwrap();
        send.finish().unwrap();
        client.tx.send(&WorkerMsg::Xfer(done(second, "out/big.bin", &big))).await.unwrap();

        let landed =
            tokio::task::spawn_blocking(move || waiting.join().unwrap()).await.unwrap().unwrap();
        let small_at = into.path().join("out/small.txt");
        let big_at = into.path().join("out/big.bin");
        assert_eq!(landed, [small_at.clone(), big_at.clone()]);
        assert_eq!(std::fs::read(&small_at).unwrap(), small);
        assert!(std::fs::read(&big_at).unwrap() == big, "whole across the cut");
        assert!(!partial.exists());
        let mtime = std::fs::metadata(&big_at).unwrap().modified().unwrap();
        let ms = mtime.duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
        assert_eq!(ms, 1_700_000_000_000, "the worker's modification time");
    }

    /// Bytes that do not match the worker's digest never land under the file's name.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_download_that_fails_its_digest_does_not_land() {
        let (mut client, link, _events) = pair().await;
        let into = tempfile::tempdir().unwrap();
        let remote = link.remote();
        let target = into.path().to_owned();
        let waiting = std::thread::spawn(move || remote.download("~/a.txt".to_owned(), target));
        for _attempt in 0..3 {
            let (xfer, _held) = fetched(&mut client).await;
            let begin = XferMsg::Begin { xfer, dest: None, files: 1, bytes: 3 };
            client.tx.send(&WorkerMsg::Xfer(begin)).await.unwrap();
            let header = download_header(xfer, "a.txt", 3, 0);
            let mut send = streams::open_bulk(&client.conn, header).await.unwrap();
            send.write_all(b"bad").await.unwrap();
            send.finish().unwrap();
            client.tx.send(&WorkerMsg::Xfer(done(xfer, "a.txt", b"abc"))).await.unwrap();
        }
        let failed = tokio::task::spawn_blocking(move || waiting.join().unwrap()).await.unwrap();
        let error = failed.unwrap_err();
        assert!(
            matches!(&error, slopty_client::xfer::XferError::Mismatch(why) if why.contains("digest")),
            "three tries, then the reason: {error:?}"
        );
        assert!(!into.path().join("a.txt").exists());
        assert!(!into.path().join("a.txt.partial").exists(), "bad bytes are not kept to resume");
    }

    /// A stream whose header has not all arrived (as when its packet is lost) holds up only
    /// itself: the session stream opened after it is read and delivered.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_stream_waiting_on_its_header_does_not_hold_up_the_next() {
        let (client, _link, mut events) = pair().await;
        let mut stalled = client.conn.open_uni().await.unwrap();
        stalled.write_all(&[9]).await.unwrap();
        let session = SessionId::new();
        let mut stream = streams::open_session(&client.conn, session, streams::SESSION_STREAM_WAIT)
            .await
            .unwrap();
        stream.send(&slopty_proto::terminal::TermEvent::Bell).await.unwrap();
        loop {
            match tokio::time::timeout(WAIT, events.recv()).await.unwrap().unwrap() {
                LinkEvent::Term { session: s, event } if s == session => {
                    assert_eq!(event, slopty_proto::terminal::TermEvent::Bell);
                    break;
                }
                LinkEvent::Disconnected(why) => panic!("{why}"),
                _ => {}
            }
        }
        drop(stalled);
    }

    /// A representation past the prefetch budget is fetched when something pastes it, whole and
    /// ahead of background transfers, and arrives over a bulk stream.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_big_clipboard_representation_is_fetched_on_paste_over_a_bulk_stream() {
        let (mut client, link, _events) = pair().await;
        let size = slopty_client::clip::PREFETCH_MAX + (1 << 20);
        let png: Vec<u8> = (0..size).map(|i| (i % 7) as u8).collect();
        let offer = Offer {
            origin: Peer::Worker(WorkerId::new()),
            generation: 9,
            age_ms: 0,
            concealed: false,
            items: vec![ClipEntry {
                reps: vec![Rep {
                    kind: ClipType::Format(ClipFormat::Png),
                    size: Some(size),
                    hash: Some(*blake3::hash(&png).as_bytes()),
                    inline: None,
                }],
            }],
        };
        let rep = offer.rep_ref(0, ClipType::Format(ClipFormat::Png));
        client.tx.send(&WorkerMsg::Clip(ClipMsg::Offer(offer))).await.unwrap();
        let remote = link.remote();
        let asked = rep.clone();
        let paste = std::thread::spawn(move || remote.clip_fetch(&asked, None, WAIT));
        let fetch = expect(&mut client, |m| match m {
            ClientMsg::Clip(ClipMsg::Fetch { rep, max, urgent }) => Some((rep, max, urgent)),
            _ => None,
        })
        .await;
        assert_eq!(fetch, (rep.clone(), None, true), "past the budget: only the paste asks");
        let header = BulkHeader {
            xfer: XferId::new(),
            purpose: Purpose::Rep { rep },
            name: String::new(),
            size,
            mtime_ms: WallMs::ZERO,
            mode: 0o600,
            offset: 0,
        };
        let mut send = streams::open_bulk(&client.conn, header).await.unwrap();
        send.write_all(&png).await.unwrap();
        send.finish().unwrap();
        let got = tokio::task::spawn_blocking(move || paste.join().unwrap()).await.unwrap();
        assert!(got == Fetched::Data(png), "the paste gets the worker's bytes");
    }

    /// A representation within the prefetch budget is fetched as soon as the offer arrives, in
    /// the background and whole, so the paste that comes later is answered from memory. Prints
    /// what that paste took (`docs/MEASUREMENTS.md`).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_small_picture_is_fetched_ahead_and_pastes_from_memory() {
        let (mut client, link, _events) = pair().await;
        let png: Vec<u8> = (0..300_000_u32).map(|i| (i % 11) as u8).collect();
        let offer = Offer {
            origin: Peer::Worker(WorkerId::new()),
            generation: 2,
            age_ms: 0,
            concealed: false,
            items: vec![ClipEntry {
                reps: vec![Rep {
                    kind: ClipType::Format(ClipFormat::Png),
                    size: Some(png.len() as u64),
                    hash: Some(*blake3::hash(&png).as_bytes()),
                    inline: None,
                }],
            }],
        };
        let rep = offer.rep_ref(0, ClipType::Format(ClipFormat::Png));
        client.tx.send(&WorkerMsg::Clip(ClipMsg::Offer(offer))).await.unwrap();
        let fetch = expect(&mut client, |m| match m {
            ClientMsg::Clip(ClipMsg::Fetch { rep, max, urgent }) => Some((rep, max, urgent)),
            _ => None,
        })
        .await;
        assert_eq!(fetch, (rep.clone(), None, false), "fetched ahead, in the background");
        let header = BulkHeader {
            xfer: XferId::new(),
            purpose: Purpose::Rep { rep: rep.clone() },
            name: String::new(),
            size: png.len() as u64,
            mtime_ms: WallMs::ZERO,
            mode: 0o600,
            offset: 0,
        };
        let mut send = streams::open_bulk(&client.conn, header).await.unwrap();
        send.write_all(&png).await.unwrap();
        send.finish().unwrap();
        let remote = link.remote();
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            if remote.clip_fetch(&rep, None, Duration::ZERO) == Fetched::Data(png.clone()) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "the prefetch lands");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let mut took = Vec::new();
        for _ in 0..20 {
            let started = std::time::Instant::now();
            let got = remote.clip_fetch(&rep, None, WAIT);
            took.push(started.elapsed());
            assert!(matches!(&got, Fetched::Data(bytes) if *bytes == png), "the picture, whole");
        }
        took.sort();
        println!("prefetched paste of 300 kB, 20 pastes: p50 {:?} max {:?}", took[10], took[19]);
        assert!(took[19] < Duration::from_millis(5), "from memory: {:?}", took[19]);
        assert!(
            !matches!(
                tokio::time::timeout(Duration::from_millis(300), client.rx.recv()).await,
                Ok(Ok(ClientMsg::Clip(ClipMsg::Fetch { .. })))
            ),
            "a paste of what is here asks nothing"
        );
    }

    /// A free loopback port, released for the forward to take.
    /// A port free now, from below the ephemeral range (macOS starts it at 49152, Linux at
    /// 32768). A port the kernel handed out for `:0` goes back to that pool when it is let go,
    /// where any socket another test binds meanwhile can take it before the forward does. The
    /// scan starts at a point the process id picks, so tests running at once try apart.
    fn free_port() -> u16 {
        const PORTS: std::ops::Range<u16> = 10_000..30_000;
        let start = std::process::id().checked_rem(u32::try_from(PORTS.len()).unwrap()).unwrap();
        PORTS
            .cycle()
            .skip(usize::try_from(start).unwrap())
            .take(PORTS.len())
            .find(|&p| std::net::TcpListener::bind(("127.0.0.1", p)).is_ok())
            .unwrap()
    }

    fn port(number: u16, session: SessionId) -> Port {
        Port { number, pid: 1, process: "vite".to_owned(), session: Some(session) }
    }

    async fn forwarded(
        events: &mut mpsc::Receiver<LinkEvent>,
    ) -> Vec<slopty_client::tunnel::Forward> {
        loop {
            let event = tokio::time::timeout(WAIT, events.recv()).await.unwrap().unwrap();
            if let LinkEvent::Ports { forwards, .. } = event {
                return forwards;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_worker_port_is_served_here_on_the_same_port_or_the_next_free_one() {
        let (mut client, _link, mut events) = pair().await;
        let session = SessionId::new();
        let wanted = free_port();
        let ports = vec![port(wanted, session)];
        client.tx.send(&WorkerMsg::Ports { session, ports }).await.unwrap();
        let forwards = forwarded(&mut events).await;
        assert_eq!(forwards.len(), 1);
        assert_eq!(forwards[0].local, Some(wanted), "the same port when it is free");
        assert_eq!(forwards[0].url().as_deref(), Some(&*format!("http://localhost:{wanted}")));

        // A browser connects; the worker joins the tunnel to its "server" and answers.
        let worker = {
            let conn = client.conn.clone();
            tokio::spawn(async move {
                let (open, mut send, mut rx) = streams::accept_tunnel(&conn).await.unwrap();
                assert_eq!(open.port, wanted);
                let request = drain(&mut rx).await;
                send.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
                send.write_all(&request).await.unwrap();
                send.finish().unwrap();
            })
        };
        let mut browser = tokio::net::TcpStream::connect(("127.0.0.1", wanted)).await.unwrap();
        browser.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
        browser.shutdown().await.unwrap();
        let mut answer = Vec::new();
        tokio::time::timeout(WAIT, browser.read_to_end(&mut answer)).await.unwrap().unwrap();
        assert_eq!(answer, b"HTTP/1.1 200 OK\r\n\r\nGET / HTTP/1.1\r\n\r\n", "both halves close");
        tokio::time::timeout(WAIT, worker).await.unwrap().unwrap();

        // A port taken here, even by a server on every interface, moves to the next free one.
        let taken = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let busy = taken.local_addr().unwrap().port();
        let ports = vec![port(busy, session)];
        client.tx.send(&WorkerMsg::Ports { session, ports }).await.unwrap();
        let forwards = forwarded(&mut events).await;
        let local = forwards[0].local.unwrap();
        assert_ne!(local, busy, "not the taken port");
        tokio::net::TcpStream::connect(("127.0.0.1", local)).await.unwrap();
        // The port the session no longer lists is let go: it can be bound again.
        drop(std::net::TcpListener::bind(("127.0.0.1", wanted)).unwrap());
    }

    /// A browser tile names the worker's port; each client serves it where it can. Two clients
    /// on one machine asking for the same worker port get two local ports, each reaching that
    /// port on its worker, and a pin outlives the sessions' port lists.
    #[tokio::test(flavor = "multi_thread")]
    async fn two_clients_serve_one_worker_port_on_their_own_local_ports() {
        let (mut first, first_link, mut first_events) = pair().await;
        let (second, second_link, _second_events) = pair().await;
        let wanted = free_port();
        let here = first_link.remote().forward(wanted);
        let there = second_link.remote().forward(wanted);
        assert_eq!(here, Some(wanted), "the first client gets the worker's own port");
        let there = there.unwrap();
        assert_ne!(there, wanted, "the second moves to a free one");
        assert_eq!(second_link.remote().forward(wanted), Some(there), "asking again is stable");

        let worker = {
            let conn = second.conn.clone();
            tokio::spawn(async move {
                let (open, mut send, _rx) = streams::accept_tunnel(&conn).await.unwrap();
                send.write_all(b"HTTP/1.1 204 No Content\r\n\r\n").await.unwrap();
                send.finish().unwrap();
                open.port
            })
        };
        let mut browser = tokio::net::TcpStream::connect(("127.0.0.1", there)).await.unwrap();
        browser.shutdown().await.unwrap();
        let mut answer = Vec::new();
        tokio::time::timeout(WAIT, browser.read_to_end(&mut answer)).await.unwrap().unwrap();
        assert_eq!(answer, b"HTTP/1.1 204 No Content\r\n\r\n");
        let reached = tokio::time::timeout(WAIT, worker).await.unwrap().unwrap();
        assert_eq!(reached, wanted, "the local port reaches the worker's port, not its own");

        // A session that listed the port and stops keeps the pinned forward up.
        let session = SessionId::new();
        let ports = vec![port(wanted, session)];
        first.tx.send(&WorkerMsg::Ports { session, ports }).await.unwrap();
        assert_eq!(forwarded(&mut first_events).await[0].local, Some(wanted), "one listener");
        first.tx.send(&WorkerMsg::Ports { session, ports: Vec::new() }).await.unwrap();
        assert!(forwarded(&mut first_events).await.is_empty());
        assert!(std::net::TcpListener::bind(("127.0.0.1", wanted)).is_err(), "still served");
    }

    /// A worker that closes a greeting with `NOT_GRANTED`, as it does for a node the tailnet
    /// grants no client role, fails the dial as that; any other close is something else.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_worker_closing_with_not_granted_is_told_from_other_closes() {
        let listener =
            WorkerListener::bind(slopty_net::endpoint::any(0), Admission::default()).unwrap();
        let addr: HostAddr =
            format!("127.0.0.1:{}", listener.local_addr().unwrap().port()).parse().unwrap();
        let endpoint = bind_client().unwrap();
        let mut closes = Vec::new();
        for (code, reason) in [(NOT_GRANTED, &b"not granted"[..]), (NOT_GRANTED, b""), (0, b"bye")]
        {
            let worker = async {
                let client = tokio::time::timeout(WAIT, listener.accept()).await.unwrap().unwrap();
                client.conn.close(code.into(), reason);
                client
            };
            let (dialled, _client) = tokio::join!(connect(&endpoint, &addr, hello()), worker);
            closes.push(matches!(dialled.unwrap_err(), slopty_net::NetError::NotGranted));
        }
        assert_eq!(closes, [true, true, false]);
    }

    /// The catch of a drag out reaches the drag from the link's own reader: data a target reads
    /// on a thread that hears nothing else (as the main thread, blocked in the read, hears
    /// nothing) gets the bytes the catch kept inline, while the event still waits unread for
    /// the app.
    #[cfg(target_vendor = "apple")]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_drag_outs_catch_reaches_a_read_waiting_off_the_apps_events() {
        use slopty_client::dnd::out::{DataAt, Outgoing, Shared};
        use slopty_core::StreamId;
        use slopty_proto::drag::{DragEvent, DragId, DragItem};
        use slopty_proto::screen::ScreenEvent;
        let (mut client, link, mut events) = pair().await;
        let drag = DragId::new();
        let lazy =
            Rep { kind: ClipType::Format(ClipFormat::Png), size: None, hash: None, inline: None };
        let began = vec![DragItem { file: None, promised: None, reps: vec![lazy] }];
        let shared = std::sync::Arc::new(Shared::new(Outgoing::began(drag, began)));
        link.remote().watch_drag_out(&shared);
        let uti = slopty_platform::pasteboard::uti_of_type(&ClipType::Format(ClipFormat::Png));
        let reader = {
            let shared = std::sync::Arc::clone(&shared);
            std::thread::spawn(move || shared.data(0, uti, WAIT))
        };
        let png = Rep {
            kind: ClipType::Format(ClipFormat::Png),
            size: Some(3),
            hash: None,
            inline: Some(b"png".to_vec()),
        };
        let items = vec![DragItem { file: None, promised: None, reps: vec![png] }];
        let event = DragEvent::OutCaught { drag, items };
        client
            .tx
            .send(&WorkerMsg::Screen(ScreenEvent::Drag { stream: StreamId(1), event }))
            .await
            .unwrap();
        let read = tokio::task::spawn_blocking(move || reader.join().unwrap()).await.unwrap();
        assert_eq!(read, DataAt::Bytes(b"png".to_vec()));
        let told = tokio::time::timeout(WAIT, async {
            loop {
                if let Some(LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Drag { .. }))) =
                    events.recv().await
                {
                    return;
                }
            }
        });
        told.await.unwrap();
    }
}
