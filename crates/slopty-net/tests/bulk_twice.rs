//! Two bulk streams one after the other on one connection, through a relay shaped as a tailnet's
//! (4 ms each way, up to 2 ms of jitter, 3 % loss): the second must go as fast as the first.
//!
//! An app drop test saw a connection's second 24 MiB upload take 4.4 to 10.2 s where the first
//! took 0.7 to 0.8 s. This holds the transport to that, apart from the app.
//!
//! `SLOPTY_BULK_TRACE=1` prints the sender's congestion picture every 20 ms.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;

    use slopty_core::{ClientId, WallMs, WorkerId, XferId};
    use slopty_net::admission::Admission;
    use slopty_net::client::{bind_client, connect_addr};
    use slopty_net::streams::{self, Uni};
    use slopty_net::worker::WorkerListener;
    use slopty_net::{Connection, WorkerMsg, congestion};
    use slopty_proto::handshake::{Hello, HelloAck};
    use slopty_proto::transfer::{BulkHeader, Purpose};
    use tokio::sync::mpsc;
    use tokio::time::Instant;

    const SIZE: usize = 24 << 20;
    const WAIT: Duration = Duration::from_secs(120);
    const TRACE_EVERY: Duration = Duration::from_millis(20);

    /// The e2e's tailnet shape (`slopty_e2e::harness::TAILNET`).
    const TAILNET: slopty_shape::Link = slopty_shape::Link {
        delay: Duration::from_millis(4),
        jitter: Duration::from_millis(2),
        loss: 0.03,
        ..slopty_shape::Link::CLEAR
    };

    /// Send `count` streams of [`SIZE`] bytes one after the other on one connection over
    /// `link`, `gap` apart; how long each took from its open to its last byte arriving.
    async fn send(link: slopty_shape::Link, count: usize, gap: Duration) -> Vec<Duration> {
        let listener =
            WorkerListener::bind(slopty_net::endpoint::any(0), Admission::default()).unwrap();
        let worker_at = SocketAddr::from(([127, 0, 0, 1], listener.local_addr().unwrap().port()));
        let relay = Arc::new(
            slopty_shape::relay::Relay::bind(
                SocketAddr::from(([127, 0, 0, 1], 0)),
                worker_at,
                link,
                0x5107_7e2e,
            )
            .await
            .unwrap(),
        );
        let relayed = {
            let relay = Arc::clone(&relay);
            tokio::spawn(async move { relay.run().await })
        };
        let worker = WorkerId::new();
        let (done_tx, mut done_rx) = mpsc::unbounded_channel();
        let received = tokio::spawn(async move {
            let mut client = listener.accept().await.unwrap();
            let ack = HelloAck {
                settings: String::new(),
                worker,
                name: "worker".to_owned(),
                home: String::new(),
                caps: slopty_proto::server::WorkerCaps::bare(slopty_proto::server::Os::MacOs),
                load: 0.0,
                sessions: Vec::new(),
            };
            client.tx.send(&WorkerMsg::HelloAck(ack)).await.unwrap();
            for _ in 0..count {
                let Uni::Bulk { mut rx, .. } = streams::accept_uni(&client.conn).await.unwrap()
                else {
                    panic!("a bulk stream")
                };
                let mut got = 0_usize;
                while let Some(chunk) = rx.chunk(64 << 10).await.unwrap() {
                    got = chunk.len().saturating_add(got);
                }
                assert_eq!(got, SIZE);
                done_tx.send(Instant::now()).unwrap();
            }
            client
        });
        let endpoint = bind_client().unwrap();
        let hello = Hello { client: ClientId::new(), name: "test".to_owned() };
        let conn = connect_addr(&endpoint, relay.addr().unwrap(), hello).await.unwrap();
        let tracer = std::env::var("SLOPTY_BULK_TRACE")
            .is_ok()
            .then(|| tokio::spawn(trace(conn.conn.clone())));
        let mut took = Vec::new();
        for n in 0..count {
            if n > 0 {
                tokio::time::sleep(gap).await;
            }
            let header = BulkHeader {
                xfer: XferId::new(),
                purpose: Purpose::Upload,
                name: format!("f{n}.bin"),
                size: SIZE as u64,
                mtime_ms: WallMs::from_millis(1),
                mode: 0o644,
                offset: 0,
            };
            let began = Instant::now();
            let mut send = streams::open_bulk(&conn.conn, header).await.unwrap();
            send.write_all(&vec![7_u8; SIZE]).await.unwrap();
            send.finish().unwrap();
            let done = tokio::time::timeout(WAIT, done_rx.recv()).await.unwrap().unwrap();
            let one = done.saturating_duration_since(began);
            eprintln!(
                "MEASURE stream {n}: {:.0} ms; sender {:?}",
                one.as_secs_f64() * 1e3,
                congestion::snapshot(&conn.conn)
            );
            took.push(one);
        }
        let _client = tokio::time::timeout(WAIT, received).await.unwrap().unwrap();
        if let Some(tracer) = tracer {
            tracer.abort();
        }
        relayed.abort();
        took
    }

    /// Print the sender's congestion picture and its controller every [`TRACE_EVERY`].
    async fn trace(conn: Connection) {
        let start = Instant::now();
        while conn.close_reason().is_none() {
            tokio::time::sleep(TRACE_EVERY).await;
            eprintln!(
                "{:>6} ms {:?}\n         {}",
                start.elapsed().as_millis(),
                congestion::snapshot(&conn),
                congestion::debug_state(&conn).unwrap_or_default()
            );
        }
    }

    /// Two streams over the tailnet shape, 100 ms apart, as the drop test sends them.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "a measurement: cargo nextest run -p slopty-net --test bulk_twice --run-ignored only --no-capture"]
    async fn a_second_stream_over_a_tailnet() {
        let took = send(TAILNET, 3, Duration::from_millis(100)).await;
        eprintln!("MEASURE tailnet: {took:?}");
    }

    /// `BULK_STREAMS` streams (4 unless set) over the tailnet's delay with `BULK_LOSS` percent
    /// loss (3 unless set), for a sweep: `BULK_LOSS=1 cargo nextest run -p slopty-net --test
    /// bulk_twice --run-ignored only --no-capture streams_from_the_environment`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "a measurement at the loss BULK_LOSS names"]
    async fn streams_from_the_environment() {
        let loss =
            std::env::var("BULK_LOSS").ok().and_then(|v| v.parse().ok()).unwrap_or(3.0_f32) / 100.0;
        let count =
            std::env::var("BULK_STREAMS").ok().and_then(|v| v.parse().ok()).unwrap_or(4_usize);
        let took =
            send(slopty_shape::Link { loss, ..TAILNET }, count, Duration::from_millis(100)).await;
        let ms: Vec<u128> = took.iter().map(Duration::as_millis).collect();
        eprintln!("MEASURE loss {:.1} %: {ms:?} ms", loss * 100.0);
    }

    /// The same with no loss, to tell loss from the rest.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "a measurement: cargo nextest run -p slopty-net --test bulk_twice --run-ignored only --no-capture"]
    async fn a_second_stream_over_a_clean_delay() {
        let link = slopty_shape::Link { loss: 0.0, ..TAILNET };
        let took = send(link, 3, Duration::from_millis(100)).await;
        eprintln!("MEASURE clean: {took:?}");
    }
}
