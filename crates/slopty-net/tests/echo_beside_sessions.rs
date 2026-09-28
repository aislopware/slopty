//! A terminal's echo beside other busy terminals on one connection, through a shaped link.
//!
//! Every session stream sits at one priority and noq round-robins equal priorities a packet
//! each, so an echo written while other sessions' frames wait takes its turn behind a packet
//! from each of them. The worker's pump raises the typed session's stream while it carries an
//! echo (`slopty_net::streams::EchoLift`). Here the worker's side of a real connection floods
//! [`FLOODS`] session streams the way busy terminals do (a frame every 8 ms, the actor's pace),
//! beside one session that echoes each key at once, over `slopty-shape` at 20 Mbit/s. Runs
//! alternate between the stream left at the default priority and lifted, on fresh connections.
//! `SLOPTY_FLOOD_BYTES` sizes each flood frame (3000 unless set: the floods then take 90% of
//! the link).

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::Duration;

    use slopty_core::{ClientId, SessionId, WorkerId};
    use slopty_net::admission::Admission;
    use slopty_net::client::{bind_client, connect_addr};
    use slopty_net::streams::{self, EchoLift, Uni};
    use slopty_net::worker::WorkerListener;
    use slopty_net::{ClientMsg, WorkerMsg};
    use slopty_proto::handshake::{Hello, HelloAck};
    use slopty_proto::terminal::{TermEvent, TermRequest};
    use tokio::time::Instant;

    /// The link: 20 Mbit/s, 2 ms each way, a 100 ms bottleneck queue.
    const RATE: u64 = 2_500_000;
    const QUEUE_MS: u64 = 100;
    const FLOODS: usize = 6;
    /// The session actor's frame pace (`MIN_FRAME_INTERVAL`).
    const FLOOD_PERIOD: Duration = Duration::from_millis(8);
    const FLOOD_BYTES: usize = 3_000;
    const ECHO_BYTES: usize = 240;
    const WARMUP: Duration = Duration::from_secs(1);
    const KEYS: usize = 200;
    const ROUNDS: usize = 3;

    fn flood_bytes() -> usize {
        std::env::var("SLOPTY_FLOOD_BYTES").ok().and_then(|v| v.parse().ok()).unwrap_or(FLOOD_BYTES)
    }

    /// The worker's side: [`FLOODS`] busy sessions, each writing a frame per period (its phase
    /// its own), and one session that echoes every key it is sent.
    async fn serve(listener: WorkerListener, lifted: bool) {
        let mut client = listener.accept().await.unwrap();
        let ack = HelloAck {
            worker: WorkerId::new(),
            name: "worker".to_owned(),
            home: String::new(),
            caps: slopty_proto::server::WorkerCaps::bare(slopty_proto::server::Os::MacOs),
            load: 0.0,
            sessions: Vec::new(),
        };
        client.tx.send(&WorkerMsg::HelloAck(ack)).await.unwrap();
        let wait = streams::SESSION_STREAM_WAIT;
        let mut typed = streams::open_session(&client.conn, SessionId::new(), wait).await.unwrap();
        let frame =
            slopty_proto::codec::encode(&TermEvent::Title("x".repeat(flood_bytes()))).unwrap();
        for i in 0..FLOODS {
            let mut stream =
                streams::open_session(&client.conn, SessionId::new(), wait).await.unwrap();
            let frame = frame.clone();
            let phase = FLOOD_PERIOD
                .checked_mul(u32::try_from(i).unwrap())
                .and_then(|d| d.checked_div(u32::try_from(FLOODS).unwrap()))
                .unwrap();
            tokio::spawn(async move {
                tokio::time::sleep(phase).await;
                let mut period = tokio::time::interval(FLOOD_PERIOD);
                period.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    period.tick().await;
                    if stream.send_raw(frame.clone()).await.is_err() {
                        break;
                    }
                }
            });
        }
        let mut lift = EchoLift::default();
        while let Ok(msg) = client.rx.recv().await {
            if let ClientMsg::Term { req: TermRequest::Raw(bytes), .. } = msg {
                let mut text = String::from_utf8_lossy(&bytes).into_owned();
                text.extend(std::iter::repeat_n(' ', ECHO_BYTES));
                if lifted {
                    lift.before_frame(&typed, true);
                }
                if typed.send(&TermEvent::Title(text)).await.is_err() {
                    break;
                }
            }
        }
    }

    /// Key → echo, ms, on a fresh connection through a fresh shaper, after a second of floods.
    async fn run(lifted: bool) -> Vec<f64> {
        let listener =
            WorkerListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)), Admission::default())
                .unwrap();
        let worker_addr = listener.local_addr().unwrap();
        let server = tokio::spawn(serve(listener, lifted));
        let link = slopty_shape::Link {
            delay: Duration::from_millis(2),
            jitter: Duration::ZERO,
            loss: 0.0,
            rate: RATE,
            queue: RATE.saturating_mul(QUEUE_MS) / 1_000,
        };
        let relay = std::sync::Arc::new(
            slopty_shape::relay::Relay::bind(
                SocketAddr::from(([127, 0, 0, 1], 0)),
                worker_addr,
                link,
                7,
            )
            .await
            .unwrap(),
        );
        let relayed = {
            let relay = std::sync::Arc::clone(&relay);
            tokio::spawn(async move { relay.run().await })
        };
        let endpoint = bind_client().unwrap();
        let hello = Hello { client: ClientId::new(), name: "echo".to_owned() };
        let mut worker = connect_addr(&endpoint, relay.addr().unwrap(), hello).await.unwrap();
        let Uni::Session { session, rx: mut echoes } =
            streams::accept_uni(&worker.conn).await.unwrap()
        else {
            panic!("the typed session's stream");
        };
        let mut readers = Vec::new();
        for _ in 0..FLOODS {
            let Uni::Session { rx: mut events, .. } =
                streams::accept_uni(&worker.conn).await.unwrap()
            else {
                panic!("a busy session's stream");
            };
            readers.push(tokio::spawn(async move { while events.recv().await.is_ok() {} }));
        }
        tokio::time::sleep(WARMUP).await;
        let mut took = Vec::with_capacity(KEYS);
        for i in 0..KEYS {
            let key = b'a'.saturating_add(u8::try_from(i % 26).unwrap());
            let sent = Instant::now();
            let req = TermRequest::Raw(vec![key]);
            worker.tx.send(&ClientMsg::Term { session, req }).await.unwrap();
            loop {
                let event = tokio::time::timeout(Duration::from_secs(5), echoes.recv())
                    .await
                    .expect("an echo within 5 s")
                    .unwrap();
                if matches!(event, TermEvent::Title(t) if t.as_bytes().first() == Some(&key)) {
                    break;
                }
            }
            took.push(sent.elapsed().as_secs_f64() * 1e3);
            // Keys land anywhere in the flood's period, not in step with it.
            let gap = 20_u64.saturating_add(u64::try_from(i).unwrap().saturating_mul(7) % 23);
            tokio::time::sleep(Duration::from_millis(gap)).await;
        }
        worker.close();
        endpoint.close(0_u32.into(), b"done");
        for reader in readers {
            reader.abort();
        }
        relayed.abort();
        server.abort();
        took
    }

    /// `(p50, p90, p99, max)`.
    fn quantiles(samples: &mut [f64]) -> (f64, f64, f64, f64) {
        samples.sort_by(f64::total_cmp);
        let at =
            |percent: usize| samples[samples.len().saturating_sub(1).saturating_mul(percent) / 100];
        (at(50), at(90), at(99), at(100))
    }

    /// The measurement behind "an echo beside other busy sessions" (MEASUREMENTS.md). Run it in
    /// release: `cargo nextest run -p slopty-net --release --test echo_beside_sessions
    /// --run-ignored only --no-capture`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "a measurement: about 50 s over a shaped link"]
    async fn echo_beside_session_floods() {
        eprintln!(
            "MEASURE {FLOODS} floods of {} B every {FLOOD_PERIOD:?}, link {RATE} B/s",
            flood_bytes()
        );
        let (mut flat, mut lifted) = (Vec::new(), Vec::new());
        for round in 1..=ROUNDS {
            for lift in [false, true] {
                let mut took = run(lift).await;
                let arm = if lift { "lifted" } else { "flat" };
                let (p50, p90, p99, max) = quantiles(&mut took);
                eprintln!(
                    "MEASURE round {round} {arm}: echo p50 {p50:.2} / p90 {p90:.2} / p99 {p99:.2} / max {max:.2} ms"
                );
                if lift { &mut lifted } else { &mut flat }.extend(took);
            }
        }
        for (arm, took) in [("flat", &mut flat), ("lifted", &mut lifted)] {
            let (p50, p90, p99, max) = quantiles(took);
            eprintln!(
                "MEASURE all {arm}: echo p50 {p50:.2} / p90 {p90:.2} / p99 {p99:.2} / max {max:.2} ms"
            );
        }
    }
}
