//! Real client ↔ worker connections over the in-memory network (`slopty_shape::sim`), on
//! tokio's paused clock.
//!
//! Every endpoint here is the real one (`slopty_net::endpoint::bind_on`), with the shipped
//! transport config, the real listener and the real client dial; only the wire is simulated.
//! Each run gets a runtime of its own whose clock jumps whenever every task waits, so a minute
//! of outage costs milliseconds, and the network's seed with noq's seeds repeats a run exactly.
//! `NIGHTLY_SEEDS` sweeps the seeds at night (`cargo test -p slopty-net --test sim -- --ignored`).

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::Duration;

    use slopty_core::{ClientId, SessionId, WorkerId};
    use slopty_net::admission::{Admission, Cidr};
    use slopty_net::client::{WorkerConn, connect_addr};
    use slopty_net::framed::{FramedRecv, FramedSend};
    use slopty_net::streams::{self, EchoLift, Uni};
    use slopty_net::worker::{AcceptedClient, WorkerListener};
    use slopty_net::{ClientMsg, Connection, Endpoint, WorkerMsg, endpoint};
    use slopty_proto::handshake::{Hello, HelloAck};
    use slopty_proto::terminal::{TermEvent, TermRequest};
    use slopty_shape::Link;
    use slopty_shape::sim::{Faults, Net};
    use tokio::task::JoinHandle;
    use tokio::time::Instant;

    const CLIENT: SocketAddr = at(1, 50_000);
    const WORKER: SocketAddr = at(2, endpoint::WORKER_PORT);
    /// Where a NAT moves the client to.
    const MOVED: SocketAddr = at(3, 61_000);
    /// The idle timeout both ends negotiate (`endpoint::IDLE_TIMEOUT`, private there).
    const IDLE_TIMEOUT: Duration = Duration::from_secs(45);
    /// Seeds each scenario runs in the gate.
    const GATE_SEEDS: std::ops::RangeInclusive<u64> = 1..=8;
    /// Seeds each scenario runs at night.
    const NIGHTLY_SEEDS: std::ops::RangeInclusive<u64> = 1..=200;

    /// A test network address: TEST-NET-1, which the admission below lets in.
    const fn at(host: u8, port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, host)), port)
    }

    const fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// A tailnet hop: 5 ms each way, `loss` of the packets lost each way.
    fn hop(loss: f32) -> Faults {
        Faults::over(Link { delay: ms(5), loss, ..Link::CLEAR })
    }

    /// Longer than any scenario runs in simulated time. A run that waits for something that
    /// never comes still sees keep-alives, so its clock runs on for good: this ends it.
    const HUNG: Duration = Duration::from_secs(600);

    /// Run `scenario` for `seed` on a runtime of its own, on a paused clock. Dropping the
    /// runtime ends whatever the scenario left running, so no run leaks into the next.
    fn simulate<T, F: Future<Output = T>>(seed: u64, scenario: impl FnOnce(u64) -> F) -> T {
        let run = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap()
            .block_on(async { tokio::time::timeout(HUNG, scenario(seed)).await });
        run.unwrap_or_else(|_| panic!("seed {seed}: still running after {HUNG:?}"))
    }

    /// The real clock, for what a simulated run costs.
    #[expect(clippy::disallowed_methods, reason = "the simulation's own cost is real time")]
    fn wall() -> std::time::Instant {
        std::time::Instant::now()
    }

    #[expect(clippy::disallowed_methods, reason = "the simulation's own cost is real time")]
    fn wall_since(started: std::time::Instant) -> Duration {
        started.elapsed()
    }

    /// Both ends of a connection over a simulated network.
    struct Linked {
        net: Net,
        client_end: Endpoint,
        listener: WorkerListener,
        client: Connection,
        worker: Connection,
        /// The client's control stream, while nothing is typing on it.
        keys: Option<FramedSend<ClientMsg>>,
        /// The worker's end of it, while nothing is reading it.
        keys_in: Option<FramedRecv<ClientMsg>>,
        /// The other direction of the control stream, kept open.
        _control: (FramedSend<WorkerMsg>, FramedRecv<WorkerMsg>),
        /// From the dial to the worker's answer, in simulated time.
        took: Duration,
    }

    fn ack() -> HelloAck {
        HelloAck {
            worker: WorkerId::new(),
            name: "sim".to_owned(),
            home: String::new(),
            caps: slopty_proto::server::WorkerCaps::bare(slopty_proto::server::Os::MacOs),
            load: 0.0,
            sessions: Vec::new(),
        }
    }

    /// A worker and a client on `faults` drawn from `seed`, connected.
    async fn link(faults: Faults, seed: u64) -> Result<Linked, String> {
        let net = Net::new(faults, seed);
        let socket = Box::new(net.bind(WORKER).unwrap());
        let worker_end = endpoint::bind_on(socket, true, Some(seed)).unwrap();
        let allow: Cidr = "192.0.2.0/24".parse().unwrap();
        let listener = WorkerListener::on(worker_end, Admission::with_tailnet(vec![allow], None));
        let socket = Box::new(net.bind(CLIENT).unwrap());
        let client_end = endpoint::bind_on(socket, false, Some(seed.rotate_left(32))).unwrap();
        let (client, worker, took) = dial(&client_end, &listener).await?;
        Ok(Linked {
            net,
            client_end,
            listener,
            client: client.conn,
            worker: worker.conn,
            keys: Some(client.tx),
            keys_in: Some(worker.rx),
            _control: (worker.tx, client.rx),
            took,
        })
    }

    /// Dial the worker and let it answer: both ends, and how long it took.
    async fn dial(
        client_end: &Endpoint,
        listener: &WorkerListener,
    ) -> Result<(WorkerConn, AcceptedClient, Duration), String> {
        let answering = tokio::spawn({
            let listener = listener.clone();
            async move {
                let mut worker = listener.accept().await?;
                worker.tx.send(&WorkerMsg::HelloAck(ack())).await.ok()?;
                Some(worker)
            }
        });
        let started = Instant::now();
        let hello = Hello { client: ClientId::new(), name: "sim".to_owned() };
        let client = match connect_addr(client_end, WORKER, hello).await {
            Ok(client) => client,
            Err(e) => {
                answering.abort();
                return Err(format!("the dial failed after {:?}: {e}", started.elapsed()));
            }
        };
        let took = started.elapsed();
        let worker = answering.await.unwrap().ok_or("the worker never greeted")?;
        Ok((client, worker, took))
    }

    /// Type keys `0..keys`, `gap` apart, on the control stream: each one's number, as two bytes.
    /// Returns when each was written.
    fn type_keys(
        mut tx: FramedSend<ClientMsg>,
        session: SessionId,
        keys: u16,
        gap: Duration,
    ) -> JoinHandle<(FramedSend<ClientMsg>, Vec<Instant>)> {
        tokio::spawn(async move {
            let mut sent = Vec::new();
            for key in 0..keys {
                let req = TermRequest::Raw(key.to_be_bytes().to_vec());
                sent.push(Instant::now());
                if tx.send(&ClientMsg::Term { session, req }).await.is_err() {
                    break;
                }
                tokio::time::sleep(gap).await;
            }
            (tx, sent)
        })
    }

    /// The worker's side: each key read off the control stream and echoed at once on a session
    /// stream, lifted as the worker lifts an echo. With `flood`, a second session writes a
    /// `flood`-byte frame every 8 ms beside it, the way a busy terminal does. Returns the keys
    /// in the order they came, once `keys` have or the stream ends.
    fn echo_keys(
        conn: Connection,
        mut rx: FramedRecv<ClientMsg>,
        keys: u16,
        flood: Option<usize>,
    ) -> JoinHandle<Vec<u16>> {
        tokio::spawn(async move {
            let wait = streams::SESSION_STREAM_WAIT;
            let mut echo = streams::open_session(&conn, SessionId::new(), wait).await.unwrap();
            if let Some(bytes) = flood {
                let mut busy = streams::open_session(&conn, SessionId::new(), wait).await.unwrap();
                tokio::spawn(async move {
                    let frame = TermEvent::Title("x".repeat(bytes));
                    let mut period = tokio::time::interval(ms(8));
                    loop {
                        period.tick().await;
                        if busy.send(&frame).await.is_err() {
                            break;
                        }
                    }
                });
            }
            let mut lift = EchoLift::default();
            let mut got = Vec::new();
            while got.len() < usize::from(keys) {
                let Ok(ClientMsg::Term { req: TermRequest::Raw(bytes), .. }) = rx.recv().await
                else {
                    break;
                };
                let key = u16::from_be_bytes(bytes.try_into().unwrap());
                got.push(key);
                lift.before_frame(&echo, true);
                if echo.send(&TermEvent::Title(key.to_string())).await.is_err() {
                    break;
                }
            }
            got
        })
    }

    /// The client's side: the echo session's frames until `keys` have come or the stream ends,
    /// with when each came. Any other session (the flood) is drained and dropped.
    fn read_echoes(conn: Connection, keys: u16) -> JoinHandle<Vec<(u16, Instant)>> {
        tokio::spawn(async move {
            let Ok(Uni::Session { mut rx, .. }) = streams::accept_uni(&conn).await else {
                return Vec::new();
            };
            let drain = conn.clone();
            tokio::spawn(async move {
                while let Ok(Uni::Session { mut rx, .. }) = streams::accept_uni(&drain).await {
                    tokio::spawn(async move { while rx.recv().await.is_ok() {} });
                }
            });
            let mut got = Vec::new();
            while got.len() < usize::from(keys) {
                let Ok(TermEvent::Title(text)) = rx.recv().await else { break };
                got.push((text.parse().unwrap(), Instant::now()));
            }
            got
        })
    }

    /// Keys typed and echoed on a linked pair while `during` does something to the network
    /// `after` into the typing.
    struct Typed {
        /// Keys as the worker read them.
        read: Vec<u16>,
        /// Echoes as the client read them.
        echoed: Vec<u16>,
        /// Each key's time from being written to its echo coming back, in key order.
        round_trips: Vec<Duration>,
        /// When `during` ran.
        began: Instant,
        /// When the last echo came.
        last: Instant,
    }

    async fn type_through(
        pair: &mut Linked,
        keys: u16,
        gap: Duration,
        flood: Option<usize>,
        after: Duration,
        during: impl FnOnce(&Net),
    ) -> Typed {
        let session = SessionId::new();
        let (tx, rx) = (pair.keys.take().unwrap(), pair.keys_in.take().unwrap());
        let worker = echo_keys(pair.worker.clone(), rx, keys, flood);
        let echoes = read_echoes(pair.client.clone(), keys);
        let typing = type_keys(tx, session, keys, gap);
        tokio::time::sleep(after).await;
        let began = Instant::now();
        during(&pair.net);
        let read = worker.await.unwrap();
        let echoed = echoes.await.unwrap();
        let (tx, sent) = typing.await.unwrap();
        pair.keys = Some(tx);
        let round_trips = sent
            .iter()
            .zip(&echoed)
            .map(|(sent, (_, came))| came.saturating_duration_since(*sent))
            .collect();
        let last = echoed.last().map_or_else(Instant::now, |(_, came)| *came);
        let echoed = echoed.into_iter().map(|(key, _)| key).collect();
        Typed { read, echoed, round_trips, began, last }
    }

    /// The `p`th percentile of `sorted`, nearest rank.
    fn percentile(sorted: &[Duration], p: usize) -> Duration {
        let rank = sorted.len().saturating_mul(p) / 100;
        sorted[rank.min(sorted.len().saturating_sub(1))]
    }

    /// Every key once and in order, on both legs.
    fn assert_once_in_order(typed: &Typed, keys: u16, seed: u64) {
        let all: Vec<u16> = (0..keys).collect();
        assert_eq!(typed.read, all, "seed {seed}: the worker read the keys out of turn");
        assert_eq!(typed.echoed, all, "seed {seed}: the client read the echoes out of turn");
    }

    /// Neither end has closed, and the worker was not dialled again.
    async fn assert_one_connection(pair: &Linked, seed: u64) {
        assert_eq!(pair.client.close_reason(), None, "seed {seed}: the client's end closed");
        assert_eq!(pair.worker.close_reason(), None, "seed {seed}: the worker's end closed");
        let again = tokio::time::timeout(Duration::from_secs(1), pair.listener.accept()).await;
        assert!(again.is_err(), "seed {seed}: the client connected a second time");
    }

    /// Sorted, for percentiles.
    fn sorted(mut times: Vec<Duration>) -> Vec<Duration> {
        times.sort_unstable();
        times
    }

    // (a) The handshake at a fifth lost each way.

    /// What the dial at 20 % loss each way took, and the run's digest.
    async fn lossy_handshake(seed: u64) -> (Duration, u64) {
        let pair = link(hop(0.2), seed).await.unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        (pair.took, pair.net.digest())
    }

    /// Under the 2 s the client gives a handshake (`client::HANDSHAKE_TIMEOUT`), for every seed:
    /// a lost FINISHED from the worker used to leave the client waiting out its dial.
    const LOSSY_DIAL: Duration = Duration::from_secs(2);

    /// A seed that loses the worker's first flight, HELLO and FINISHED together.
    const LOST_FINISHED: u64 = 31;

    /// A dial through a fifth lost each way finishes inside the client's handshake timeout. The
    /// client sends its FINISHED only after the worker's (`crypto::Stage`), so a lost FINISHED
    /// from the worker is sent again instead of stranding the client.
    #[test]
    fn a_handshake_at_a_fifth_lost_each_way_completes() {
        for seed in GATE_SEEDS.chain([LOST_FINISHED]) {
            let (took, _) = simulate(seed, lossy_handshake);
            assert!(took < LOSSY_DIAL, "seed {seed}: the dial took {took:?}");
        }
    }

    // (b) An outage shorter than the idle timeout.

    const OUTAGE: Duration = Duration::from_secs(30);

    /// Keys typed across a 30 s outage: how long after it the last echo came, and the digest.
    async fn short_outage(seed: u64) -> (Duration, u64) {
        let mut pair = link(hop(0.01), seed).await.unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        let keys = 100;
        let typed = type_through(&mut pair, keys, ms(20), None, ms(500), |net| {
            net.blackout(OUTAGE);
        })
        .await;
        assert_once_in_order(&typed, keys, seed);
        assert_one_connection(&pair, seed).await;
        let back = typed.began.checked_add(OUTAGE).unwrap();
        (typed.last.saturating_duration_since(back), pair.net.digest())
    }

    /// Two seconds of keys, most of them typed into a 30 s outage, reach the worker once and in
    /// order on the connection they were typed on, and their echoes come back the same way,
    /// within noq's longest probe interval of the path's return.
    #[test]
    fn keys_typed_into_an_outage_arrive_once_in_order_after_it() {
        for seed in GATE_SEEDS {
            let (recovery, _) = simulate(seed, short_outage);
            assert!(
                recovery < Duration::from_secs(3),
                "seed {seed}: {recovery:?} after the outage"
            );
        }
    }

    // (c) An outage longer than the idle timeout.

    const LONG_OUTAGE: Duration = Duration::from_secs(60);

    /// A 60 s outage on an idle connection: when each end gave it up, how long a new dial took
    /// once the path was back, and the digest.
    async fn long_outage(seed: u64) -> (Duration, Duration, u64) {
        let pair = link(hop(0.01), seed).await.unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        tokio::time::sleep(ms(500)).await;
        let began = Instant::now();
        pair.net.blackout(LONG_OUTAGE);
        let (client, worker) = tokio::join!(pair.client.closed(), pair.worker.closed());
        let ended = began.elapsed();
        for (side, why) in [("client", client), ("worker", worker)] {
            assert!(
                matches!(why, noq::ConnectionError::TimedOut),
                "seed {seed}: the {side}'s end ended with {why}"
            );
        }
        tokio::time::sleep_until(began.checked_add(LONG_OUTAGE).unwrap()).await;
        let (_client, _worker, redial) = dial(&pair.client_end, &pair.listener)
            .await
            .unwrap_or_else(|e| panic!("seed {seed}: the dial after the outage: {e}"));
        (ended, redial, pair.net.digest())
    }

    /// Both ends give up a connection the idle timeout after the path went dark, not before and
    /// not much after, and the worker takes a new one once the path is back.
    #[test]
    fn an_outage_past_the_idle_timeout_ends_the_connection_on_time() {
        for seed in GATE_SEEDS {
            let (ended, redial, _) = simulate(seed, long_outage);
            assert!(
                (IDLE_TIMEOUT.saturating_sub(Duration::from_secs(1))
                    ..IDLE_TIMEOUT.saturating_add(Duration::from_secs(2)))
                    .contains(&ended),
                "seed {seed}: ended {ended:?} into the outage"
            );
            assert!(redial < LOSSY_DIAL, "seed {seed}: the dial after took {redial:?}");
        }
    }

    // (d) NAT rebinding.

    /// Keys typed while a NAT moves the client: the slowest key's round trip, and the digest.
    async fn rebinding(seed: u64) -> (Duration, u64) {
        let mut pair = link(hop(0.01), seed).await.unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        let keys = 100;
        let typed = type_through(&mut pair, keys, ms(20), None, ms(1_000), |net| {
            net.rebind(CLIENT, MOVED).unwrap();
        })
        .await;
        assert_once_in_order(&typed, keys, seed);
        assert_eq!(endpoint::remote(&pair.worker), Some(MOVED), "seed {seed}: not migrated");
        assert_one_connection(&pair, seed).await;
        let slowest = typed.round_trips.iter().max().copied().unwrap_or_default();
        (slowest, pair.net.digest())
    }

    /// A seed that loses the worker's first `PATH_CHALLENGE` to the client's new address.
    const LOST_CHALLENGE: u64 = 10;

    /// The client's NAT gives it a new address in the middle of typing: the worker follows it
    /// on the same connection, and no key or echo is lost, doubled or reordered. When the
    /// worker's challenge to the new address is lost, it falls back to the old path, and the
    /// client's next packet starts the move again (noq-proto patch 10,
    /// `vendor/noq-proto/SLOPTY.md`).
    #[test]
    fn a_nat_rebinding_moves_the_connection_without_a_reconnect() {
        for seed in GATE_SEEDS.chain([LOST_CHALLENGE]) {
            let (slowest, _) = simulate(seed, rebinding);
            assert!(slowest < Duration::from_secs(1), "seed {seed}: a key took {slowest:?}");
        }
    }

    // (e) A capped, lossy link under a flood.

    /// 2 Mbit/s with 100 ms of queue, 10 ms and up to 2 more each way, 5 % lost each way.
    fn capped() -> Faults {
        let rate = 250_000;
        Faults::over(Link { delay: ms(10), jitter: ms(2), loss: 0.05, rate, queue: rate / 10 })
    }

    /// Keys typed beside a terminal that writes half again the link's rate: the round trips'
    /// median and 99th percentile, and the digest.
    async fn flooded(seed: u64) -> (Duration, Duration, u64) {
        let mut pair = link(capped(), seed).await.unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        let keys = 200;
        let typed = type_through(&mut pair, keys, ms(30), Some(3_000), ms(0), |_| {}).await;
        assert_once_in_order(&typed, keys, seed);
        assert_one_connection(&pair, seed).await;
        let trips = sorted(typed.round_trips);
        (percentile(&trips, 50), percentile(&trips, 99), pair.net.digest())
    }

    /// Keys and their echoes stay exactly once and in order through a rate cap and loss while
    /// a flood fills the link, and the lifted echo keeps ahead of it.
    #[test]
    fn keys_beside_a_flood_on_a_capped_lossy_link_arrive_once_in_order() {
        for seed in GATE_SEEDS {
            let (p50, p99, _) = simulate(seed, flooded);
            assert!(p50 < ms(150), "seed {seed}: echo p50 {p50:?}");
            assert!(p99 < ms(600), "seed {seed}: echo p99 {p99:?}");
        }
    }

    // Repeatability, and the sweep.

    /// A seed is its run: the same packets delivered at the same simulated times.
    #[test]
    fn a_seed_repeats_its_run() {
        for seed in [3, 11] {
            assert_eq!(simulate(seed, lossy_handshake), simulate(seed, lossy_handshake));
            assert_eq!(simulate(seed, short_outage), simulate(seed, short_outage));
            assert_eq!(simulate(seed, rebinding), simulate(seed, rebinding));
            assert_eq!(simulate(seed, flooded), simulate(seed, flooded));
        }
        assert_ne!(simulate(3, lossy_handshake).1, simulate(11, lossy_handshake).1);
    }

    /// `(p50, p99, max)` of `times`, for the sweep's report.
    fn spread(times: Vec<Duration>) -> String {
        let times = sorted(times);
        let max = times.last().copied().unwrap_or_default();
        format!("p50 {:?} p99 {:?} max {max:?}", percentile(&times, 50), percentile(&times, 99))
    }

    /// Every scenario on [`NIGHTLY_SEEDS`], with what each took in simulated time and what the
    /// sweep cost in real time.
    #[test]
    #[ignore = "nightly: every scenario on 200 seeds (cargo test -p slopty-net --test sim -- --ignored)"]
    fn every_scenario_holds_across_the_seed_sweep() {
        let started = wall();
        let dials: Vec<Duration> =
            NIGHTLY_SEEDS.map(|seed| simulate(seed, lossy_handshake).0).collect();
        assert!(
            dials.iter().all(|took| *took < LOSSY_DIAL),
            "a dial took {:?}",
            dials.iter().max()
        );
        println!("dial at 20 % loss: {} ({:?} real)", spread(dials), wall_since(started));

        let started = wall();
        let back: Vec<Duration> =
            NIGHTLY_SEEDS.map(|seed| simulate(seed, short_outage).0).collect();
        println!("echo after a 30 s outage: {} ({:?} real)", spread(back), wall_since(started));

        let started = wall();
        let (mut ended, mut redials) = (Vec::new(), Vec::new());
        for seed in NIGHTLY_SEEDS {
            let (end, redial, _) = simulate(seed, long_outage);
            ended.push(end);
            redials.push(redial);
        }
        println!("end of a 60 s outage: {}", spread(ended));
        println!("dial after it: {} ({:?} real)", spread(redials), wall_since(started));

        let started = wall();
        let slowest: Vec<Duration> =
            NIGHTLY_SEEDS.map(|seed| simulate(seed, rebinding).0).collect();
        println!(
            "slowest key across a rebinding: {} ({:?} real)",
            spread(slowest),
            wall_since(started)
        );

        let started = wall();
        let (mut p50s, mut p99s) = (Vec::new(), Vec::new());
        for seed in NIGHTLY_SEEDS {
            let (p50, p99, _) = simulate(seed, flooded);
            p50s.push(p50);
            p99s.push(p99);
        }
        println!("echo beside a flood, p50 per seed: {}", spread(p50s));
        println!(
            "echo beside a flood, p99 per seed: {} ({:?} real)",
            spread(p99s),
            wall_since(started)
        );
    }
}
