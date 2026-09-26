//! An upload of many small files over a link with a real round trip: each file's bytes follow
//! the last one's rather than waiting for its acknowledgement (MEASUREMENTS, "uploading many
//! small files").

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use slopty_client::WorkerLink;
    use slopty_core::{ClientId, SessionId, WorkerId, XferId};
    use slopty_net::admission::Admission;
    use slopty_net::client::{bind_client, connect};
    use slopty_net::streams::{self, Uni};
    use slopty_net::worker::WorkerListener;
    use slopty_net::{ClientMsg, HostAddr, WorkerMsg};
    use slopty_proto::handshake::{Hello, HelloAck};
    use slopty_proto::transfer::{Dest, XferMsg};

    const FILES: usize = 100;
    const SIZE: usize = 4096;
    /// One way; the round trip is twice it.
    const DELAY: Duration = Duration::from_millis(10);
    const WAIT: Duration = Duration::from_secs(60);

    /// Upload `files` files of `size` bytes over a link adding `delay` each way, and time the
    /// worker's side from `Begin` to the last byte of the last file.
    async fn upload(files: usize, size: usize, delay: Duration) -> Duration {
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
            let ack = HelloAck { worker, name: "worker".to_owned(), sessions: Vec::new() };
            client.tx.send(&WorkerMsg::HelloAck(ack)).await.unwrap();
            client
        });
        let endpoint = bind_client().unwrap();
        let addr: HostAddr = relay.addr().unwrap().into();
        let hello = Hello { client: ClientId::new(), name: "test".to_owned() };
        let conn = connect(&endpoint, &addr, hello).await.unwrap();
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
        took
    }

    /// Waiting on each file's acknowledgement took a round trip per file: 2.16 s for 100 files
    /// over 20 ms. The bound is a quarter of that.
    #[tokio::test(flavor = "multi_thread")]
    async fn many_small_files_do_not_wait_a_round_trip_each() {
        let took = upload(FILES, SIZE, DELAY).await;
        let serial = DELAY.saturating_mul(2).saturating_mul(u32::try_from(FILES).unwrap());
        assert!(
            took < serial.checked_div(4).unwrap(),
            "{took:?} against {serial:?} for a round trip each"
        );
    }
}
