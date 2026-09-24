//! The client's link against a scripted worker in-process, over UDP on loopback: an upload
//! resumed after the worker cut its stream, a download, a clipboard representation fetched on
//! paste, and a port forwarded to a real local connection.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use slopty_client::{LinkEvent, WorkerLink};
    use slopty_core::{ClientId, SessionId, WorkerId, XferId};
    use slopty_net::admission::Admission;
    use slopty_net::client::{bind_client, connect};
    use slopty_net::streams::{self, RawRecv, Uni};
    use slopty_net::worker::{AcceptedClient, WorkerListener};
    use slopty_net::{ClientMsg, HostAddr, WorkerMsg};
    use slopty_proto::PROTOCOL_VERSION;
    use slopty_proto::handshake::{Caps, ClientKind, Hello, HelloAck};
    use slopty_proto::orchestration::Port;
    use slopty_proto::transfer::{
        BulkHeader, ClipItem, ClipMsg, Dest, Offer, Peer, Purpose, XferMsg,
    };
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::sync::mpsc;

    const WAIT: Duration = Duration::from_secs(20);

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

    /// A worker that greets one client and hands its end to the test, and the client's link.
    async fn pair() -> (AcceptedClient, WorkerLink, mpsc::Receiver<LinkEvent>) {
        let listener =
            WorkerListener::bind(slopty_net::endpoint::any(0), Admission::default()).unwrap();
        let port = listener.local_addr().unwrap().port();
        let worker = WorkerId::new();
        let accepted = tokio::spawn(async move {
            let mut client = listener.accept().await.unwrap();
            let ack = HelloAck {
                protocol: PROTOCOL_VERSION,
                worker,
                name: "worker".to_owned(),
                app_version: "0".to_owned(),
                caps: Caps::empty(),
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
        let (xfer, path) = expect(&mut client, |m| match m {
            ClientMsg::Xfer(XferMsg::Fetch { xfer, path }) => Some((xfer, path)),
            _ => None,
        })
        .await;
        assert_eq!(path, "~/notes/todo.txt");
        let body = b"milk\neggs\n";
        let header = BulkHeader {
            xfer,
            purpose: Purpose::Download,
            name: "todo.txt".to_owned(),
            size: body.len() as u64,
            mtime_ms: 0,
            mode: 0o640,
            offset: 0,
        };
        let mut send = streams::open_bulk(&client.conn, header).await.unwrap();
        send.write_all(body).await.unwrap();
        send.finish().unwrap();
        let begin = XferMsg::Begin { xfer, dest: None, files: 1, bytes: body.len() as u64 };
        client.tx.send(&WorkerMsg::Xfer(begin)).await.unwrap();
        let landed =
            tokio::task::spawn_blocking(move || waiting.join().unwrap()).await.unwrap().unwrap();
        let expected = into.path().join("todo.txt");
        assert_eq!(landed, std::slice::from_ref(&expected));
        assert_eq!(std::fs::read(&expected).unwrap(), body);
        assert!(!PathBuf::from(format!("{}.partial", expected.display())).exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_big_clipboard_representation_is_fetched_on_paste_over_a_bulk_stream() {
        let (mut client, link, _events) = pair().await;
        let png: Vec<u8> = (0..3_000_000_u32).map(|i| (i % 7) as u8).collect();
        let offer = Offer {
            origin: Peer::Worker(WorkerId::new()),
            generation: 9,
            items: vec![ClipItem {
                uti: "public.png".to_owned(),
                size: png.len() as u64,
                hash: *blake3::hash(&png).as_bytes(),
                inline: None,
            }],
        };
        client.tx.send(&WorkerMsg::Clip(ClipMsg::Offer(offer))).await.unwrap();
        let remote = link.remote();
        let paste = std::thread::spawn(move || remote.clip_data(9, "public.png", WAIT));
        let (generation, uti) = expect(&mut client, |m| match m {
            ClientMsg::Clip(ClipMsg::Fetch { generation, uti }) => Some((generation, uti)),
            _ => None,
        })
        .await;
        assert_eq!((generation, uti.as_str()), (9, "public.png"));
        let header = BulkHeader {
            xfer: XferId::new(),
            purpose: Purpose::Clip { generation, uti },
            name: String::new(),
            size: png.len() as u64,
            mtime_ms: 0,
            mode: 0o600,
            offset: 0,
        };
        let mut send = streams::open_bulk(&client.conn, header).await.unwrap();
        send.write_all(&png).await.unwrap();
        send.finish().unwrap();
        let got = tokio::task::spawn_blocking(move || paste.join().unwrap()).await.unwrap();
        assert!(got.as_deref() == Some(&*png), "the paste gets the worker's bytes");
    }

    /// A free loopback port, released for the forward to take.
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
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
}
