//! One bulk stream through a relay that adds a round trip and nothing else: no rate limit and no
//! loss, so nothing tells the congestion controller to back off. The rate a transfer reaches is
//! whatever the controller's model of the path lets it reach.
//!
//! BBR3 once took its first round-trip sample from the RTT estimator before the estimator had
//! seen that sample, so `BBR.min_rtt` started at the configured initial RTT (5 ms) and held for
//! ten seconds on any longer path. Its window was then sized to a fraction of the path's product,
//! and one stream crawled at 10 Mbit/s over a 60 ms round trip (`vendor/noq-proto/SLOPTY.md`,
//! patch 6; MEASUREMENTS.md, "one bulk stream over a long round trip").
//!
//! `SLOPTY_BULK_TRACE=1` prints the sender's congestion picture every 20 ms; `UP_MS` sets the
//! one-way delay of `one_way_delay_from_the_environment`.

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use slopty_core::{ClientId, WorkerId, XferId};
    use slopty_net::admission::Admission;
    use slopty_net::client::{bind_client, connect_addr};
    use slopty_net::streams::{self, Uni};
    use slopty_net::worker::WorkerListener;
    use slopty_net::{Connection, WorkerMsg, congestion};
    use slopty_proto::handshake::{Hello, HelloAck};
    use slopty_proto::transfer::{BulkHeader, Purpose};

    const SIZE: usize = 16 << 20;
    const WAIT: Duration = Duration::from_secs(60);
    /// Well under what one stream reaches on this machine at each delay tested (100 to 500
    /// Mbit/s), and far above the 10 Mbit/s a 60 ms round trip got while the minimum was stuck.
    const FLOOR_MBIT: f64 = 80.0;
    const TRACE_EVERY: Duration = Duration::from_millis(20);

    /// Send [`SIZE`] bytes on one bulk stream over a link adding `one_way` each way, and return
    /// the rate the receiver saw, Mbit/s.
    async fn rate(one_way: Duration) -> f64 {
        let listener =
            WorkerListener::bind(slopty_net::endpoint::any(0), Admission::default()).unwrap();
        let worker_at = SocketAddr::from(([127, 0, 0, 1], listener.local_addr().unwrap().port()));
        let link = slopty_shape::Link { delay: one_way, ..slopty_shape::Link::CLEAR };
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
        let received = tokio::spawn(async move {
            let mut client = listener.accept().await.unwrap();
            let ack = HelloAck { worker, name: "worker".to_owned(), sessions: Vec::new() };
            client.tx.send(&WorkerMsg::HelloAck(ack)).await.unwrap();
            let Uni::Bulk { mut rx, .. } = streams::accept_uni(&client.conn).await.unwrap() else {
                panic!("a bulk stream")
            };
            let mut got = 0;
            while let Some(chunk) = rx.chunk(64 << 10).await.unwrap() {
                got = chunk.len().saturating_add(got);
            }
            (got, Instant::now(), client)
        });
        let endpoint = bind_client().unwrap();
        let hello = Hello { client: ClientId::new(), name: "test".to_owned() };
        let conn = connect_addr(&endpoint, relay.addr().unwrap(), hello).await.unwrap();
        let header = BulkHeader {
            xfer: XferId::new(),
            purpose: Purpose::Upload,
            name: "f.bin".to_owned(),
            size: SIZE as u64,
            mtime_ms: 1,
            mode: 0o644,
            offset: 0,
        };
        let tracer = std::env::var("SLOPTY_BULK_TRACE")
            .is_ok()
            .then(|| tokio::spawn(trace(conn.conn.clone())));
        let began = Instant::now();
        let mut send = streams::open_bulk(&conn.conn, header).await.unwrap();
        send.write_all(&vec![7_u8; SIZE]).await.unwrap();
        send.finish().unwrap();
        let (got, done, _client) = tokio::time::timeout(WAIT, received).await.unwrap().unwrap();
        if let Some(tracer) = tracer {
            tracer.abort();
        }
        relayed.abort();
        assert_eq!(got, SIZE);
        let took = done.saturating_duration_since(began);
        #[expect(clippy::cast_precision_loss, reason = "16 MiB in bits is far below 2^53")]
        let mbit = (SIZE * 8) as f64 / took.as_secs_f64() / 1e6;
        eprintln!(
            "MEASURE {} ms each way: {mbit:.0} Mbit/s in {took:.2?}; sender {:?}",
            one_way.as_millis(),
            congestion::snapshot(&conn.conn)
        );
        mbit
    }

    /// Print the sender's congestion picture and BBR3's model every [`TRACE_EVERY`].
    async fn trace(conn: Connection) {
        let start = Instant::now();
        while conn.close_reason().is_none() {
            tokio::time::sleep(TRACE_EVERY).await;
            let model = congestion::debug_state(&conn).unwrap_or_default();
            eprintln!(
                "{:>6} ms {:?}\n         {}",
                start.elapsed().as_millis(),
                congestion::snapshot(&conn),
                model_fields(&model)
            );
        }
    }

    /// The fields of BBR3's `Debug` that say what bounds the rate.
    fn model_fields(model: &str) -> String {
        const NAMES: [&str; 11] = [
            "state",
            "cwnd",
            "pacing_rate",
            "max_bw",
            "min_rtt",
            "full_bw_reached",
            "full_bw_count",
            "app_limited",
            "delivered",
            "inflight",
            "extra_acked",
        ];
        let mut out = String::new();
        for name in NAMES {
            let key = format!(" {name}: ");
            let Some((_, rest)) = model.split_once(&key) else { continue };
            let value = rest.split_once(", ").map_or(rest, |(value, _)| value);
            let _infallible = write!(out, "{name}={value} ");
        }
        out
    }

    /// A 60 ms round trip: BBR3's minimum stuck at the initial RTT held this to 10 Mbit/s.
    #[tokio::test(flavor = "multi_thread")]
    async fn one_stream_fills_a_60ms_round_trip() {
        let mbit = rate(Duration::from_millis(30)).await;
        assert!(mbit >= FLOOR_MBIT, "{mbit:.0} Mbit/s over a 60 ms round trip");
    }

    /// Shorter round trips, where the stuck minimum cost less, keep their rate.
    #[tokio::test(flavor = "multi_thread")]
    async fn one_stream_keeps_its_rate_on_short_round_trips() {
        for ms in [0, 5, 10] {
            let mbit = rate(Duration::from_millis(ms)).await;
            assert!(mbit >= FLOOR_MBIT, "{mbit:.0} Mbit/s at {ms} ms each way");
        }
    }

    /// One run at `UP_MS` milliseconds each way (30 unless set), for a sweep:
    /// `UP_MS=20 cargo nextest run -p slopty-net --test bulk_over_delay --run-ignored only
    /// --no-capture`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "a measurement at the delay UP_MS names"]
    async fn one_way_delay_from_the_environment() {
        let ms = std::env::var("UP_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
        rate(Duration::from_millis(ms)).await;
    }
}
