//! Uploads over a link with a real round trip: each small file's bytes follow the last one's
//! rather than waiting for its acknowledgement (MEASUREMENTS, "uploading many small files"), and
//! one large file fills the path (MEASUREMENTS, "one bulk stream over a long round trip").

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use slopty_client::WorkerLink;
    use slopty_core::{ClientId, SessionId, WorkerId, XferId};
    use slopty_net::admission::Admission;
    use slopty_net::client::{bind_client, connect};
    use slopty_net::congestion::{self, Snapshot};
    use slopty_net::streams::{self, Uni};
    use slopty_net::worker::WorkerListener;
    use slopty_net::{ClientMsg, HostAddr, WorkerMsg};
    use slopty_proto::handshake::{Hello, HelloAck};
    use slopty_proto::transfer::{Dest, XferMsg};

    /// Two windows of the client's 16 streams awaiting acknowledgement.
    const FILES: usize = 32;
    const SIZE: usize = 4096;
    /// One way; the round trip is twice it. Long enough that the round trips a serial upload
    /// pays dwarf what a loaded machine adds per file, so the bound holds on a busy runner.
    const DELAY: Duration = Duration::from_millis(100);
    const WAIT: Duration = Duration::from_secs(60);

    /// Upload `files` files of `size` bytes over a link adding `delay` each way: the worker's
    /// time from `Begin` to the last byte of the last file, and the client's congestion picture.
    async fn upload(files: usize, size: usize, delay: Duration) -> (Duration, Snapshot) {
        let listener =
            WorkerListener::bind(slopty_net::endpoint::any(0), Admission::default()).unwrap();
        let worker_at = SocketAddr::from(([127, 0, 0, 1], listener.local_addr().unwrap().port()));
        let link = slopty_shape::Link { delay, ..slopty_shape::Link::CLEAR };
        let relay = Arc::new(
            slopty_shape::relay::Relay::bind(
                SocketAddr::from(([127, 0, 0, 1], 0)),
                worker_at,
                link,
                1,
            )
            .await
            .unwrap(),
        );
        let relayed = {
            let relay = Arc::clone(&relay);
            tokio::spawn(async move { relay.run().await })
        };
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
        let addr: HostAddr = relay.addr().unwrap().into();
        let hello = Hello { client: ClientId::new(), name: "test".to_owned() };
        let conn = connect(&endpoint, &addr, hello).await.unwrap();
        let sender = conn.conn.clone();
        let link = WorkerLink::start_forwarding(conn);
        let mut client = tokio::time::timeout(WAIT, accepted).await.unwrap().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..files)
            .map(|i| {
                let path = dir.path().join(format!("f{i:03}"));
                std::fs::write(&path, vec![u8::try_from(i % 251).unwrap(); size]).unwrap();
                path
            })
            .collect();
        link.remote().upload(XferId::new(), paths, Dest::SessionCwd(SessionId::new()));
        loop {
            let msg = tokio::time::timeout(WAIT, client.rx.recv()).await.unwrap().unwrap();
            if matches!(msg, ClientMsg::Xfer(XferMsg::Begin { .. })) {
                break;
            }
        }
        let began = Instant::now();
        let conn = client.conn.clone();
        let mut drains = tokio::task::JoinSet::new();
        for _ in 0..files {
            let Uni::Bulk { mut rx, .. } =
                tokio::time::timeout(WAIT, streams::accept_uni(&conn)).await.unwrap().unwrap()
            else {
                panic!("a bulk stream")
            };
            drains.spawn(async move {
                let mut got = 0_usize;
                while let Some(chunk) = rx.chunk(64 << 10).await.unwrap() {
                    got = got.saturating_add(chunk.len());
                }
                got
            });
        }
        let got: Vec<usize> = drains.join_all().await;
        let took = began.elapsed();
        eprintln!(
            "{files} files of {size} B over a {:?} round trip: {took:?}",
            delay.saturating_mul(2)
        );
        assert!(got.iter().all(|&n| n == size), "every file whole");
        relayed.abort();
        let snapshot = congestion::snapshot(&sender).unwrap();
        eprintln!("MEASURE sender {snapshot:?}");
        (took, snapshot)
    }

    /// Waiting on each file's acknowledgement took a round trip per file: 2.16 s for 100 files
    /// over 20 ms. The bound is half a round trip per file. Pipelined, the upload takes a few
    /// round trips whatever the file count, and the load only adds milliseconds a file: at a
    /// 10 ms delay a runner with every core taken came within 25 % of the bound, so the delay
    /// is long enough now for the round trips to decide it.
    #[tokio::test(flavor = "multi_thread")]
    async fn many_small_files_do_not_wait_a_round_trip_each() {
        let (took, _) = upload(FILES, SIZE, DELAY).await;
        let serial = DELAY.saturating_mul(2).saturating_mul(u32::try_from(FILES).unwrap());
        assert!(
            took < serial.checked_div(2).unwrap(),
            "{took:?} against {serial:?} for a round trip each"
        );
    }

    /// One large file over a 60 ms round trip goes at the link's pace: 16 MiB took 13 s
    /// (10 Mbit/s) while BBR3 took its first round trip from the 5 ms initial estimate and sized
    /// its window to a twelfth of the path, and 1.2 s (114 Mbit/s) since on a quiet machine. The
    /// time depends on the cores free, so the test holds the model: BBR3's `BBR.min_rtt` is the
    /// path's own minimum, not the initial estimate (`slopty-net`'s `bulk_over_delay` does the
    /// same for a bare stream).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_large_file_fills_a_long_round_trip() {
        let (took, s) = upload(1, 16 << 20, Duration::from_millis(30)).await;
        let (Some(model), Some(path)) = (s.model_min_rtt, s.min_rtt) else {
            panic!("no BBR3 model or no round trip measured: {s:?}")
        };
        let ratio = model.as_secs_f64() / path.as_secs_f64();
        assert!(ratio >= 0.5, "16 MiB in {took:?}, BBR.min_rtt {ratio:.2}× the path's: {s:?}");
    }

    /// A file here that cannot be read fails the upload at once as a local error: the worker
    /// hears `Failed`, never a `Resume`, and no bulk stream opens for it to wait on. It used to
    /// be taken for a cut stream and resumed twice before it failed.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unreadable_file_fails_as_local_and_is_not_resumed() {
        use std::os::unix::fs::PermissionsExt as _;

        use slopty_client::LinkEvent;
        use slopty_client::xfer::XferError;

        let listener =
            WorkerListener::bind(slopty_net::endpoint::any(0), Admission::default()).unwrap();
        let worker_at: HostAddr =
            SocketAddr::from(([127, 0, 0, 1], listener.local_addr().unwrap().port())).into();
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
        let hello = Hello { client: ClientId::new(), name: "test".to_owned() };
        let conn = connect(&endpoint, &worker_at, hello).await.unwrap();
        let mut link = WorkerLink::start_forwarding(conn);
        let mut events = link.events().unwrap();
        let mut client = tokio::time::timeout(WAIT, accepted).await.unwrap().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locked");
        std::fs::write(&path, b"secret").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let xfer = XferId::new();
        link.remote().upload(xfer, vec![path], Dest::SessionCwd(SessionId::new()));

        let error = tokio::time::timeout(WAIT, async {
            loop {
                if let Some(LinkEvent::XferFailed { xfer: failed, error }) = events.recv().await
                    && failed == xfer
                {
                    return error;
                }
            }
        })
        .await
        .unwrap();
        assert!(matches!(error, XferError::Local { .. }), "{error:?}");
        assert!(!error.worth_retrying());

        let mut heard = Vec::new();
        loop {
            let msg = tokio::time::timeout(WAIT, client.rx.recv()).await.unwrap().unwrap();
            let failed = matches!(msg, ClientMsg::Xfer(XferMsg::Failed { .. }));
            heard.push(msg);
            if failed {
                break;
            }
        }
        assert!(
            !heard.iter().any(|m| matches!(m, ClientMsg::Xfer(XferMsg::Resume { .. }))),
            "no resume: {heard:?}"
        );
        let opened =
            tokio::time::timeout(Duration::from_millis(300), streams::accept_uni(&client.conn))
                .await;
        assert!(opened.is_err(), "no bulk stream opened");
    }
}
