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
    use std::io::{self, IoSliceMut};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::task::{Context, Poll, ready};
    use std::time::Duration;

    use noq::udp::{RecvMeta, Transmit};
    use noq::{AsyncUdpSocket, UdpSender};
    use parking_lot::Mutex;
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
            settings: String::new(),
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
        let (worker, client) = (net.bind(WORKER).unwrap(), net.bind(CLIENT).unwrap());
        link_on(net, Box::new(worker), Box::new(client), seed).await
    }

    /// A worker on `worker` and a client on `client`, sockets on `net`, connected.
    async fn link_on(
        net: Net,
        worker: Box<dyn AsyncUdpSocket>,
        client: Box<dyn AsyncUdpSocket>,
        seed: u64,
    ) -> Result<Linked, String> {
        let worker_end = endpoint::bind_on(worker, true, Some(seed)).unwrap();
        let allow: Cidr = "192.0.2.0/24".parse().unwrap();
        let listener = WorkerListener::on(worker_end, Admission::with_tailnet(vec![allow], None));
        let client_end = endpoint::bind_on(client, false, Some(seed.rotate_left(32))).unwrap();
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

    // (f) Video beside keys on a 20 Mbit/s hop.

    /// `echo_beside_flood`'s link: 20 Mbit/s, 2 ms each way, 100 ms of queue, and for the
    /// report up to `SLOPTY_SIM_JITTER_US` more each way, as a Wi-Fi hop or a loaded host's
    /// timers add.
    fn video_link() -> Faults {
        let rate = 2_500_000;
        let jitter = std::env::var("SLOPTY_SIM_JITTER_US")
            .ok()
            .and_then(|us| us.parse().ok())
            .map_or(Duration::ZERO, Duration::from_micros);
        Faults::over(Link { delay: ms(2), jitter, rate, queue: rate / 10, ..Link::CLEAR })
    }

    const FRAME_PERIOD: Duration = Duration::from_micros(16_667);
    /// 12 Mbit/s of P-frames, and a keyframe of 52 ms of the link each second.
    const P_FRAME: usize = 25_000;
    const KEYFRAME: usize = 130_000;
    const KEY_EVERY: u64 = 60;
    /// What the capture guard lets QUIC hold before it skips a P-frame.
    const GUARD_BYTES: usize = 50_000;
    /// A datagram's frame number, its frame's part count, and when the frame was handed to
    /// QUIC in microseconds since the run began.
    const TAG_BYTES: usize = 18;
    const VIDEO_WARMUP: Duration = Duration::from_secs(2);
    const SAMPLE_EVERY: Duration = Duration::from_millis(10);

    /// What a video run saw after its warm-up.
    #[derive(Debug, Default, PartialEq, Eq)]
    struct Watched {
        /// From a frame's handover to QUIC to its last part at the client.
        keyframes: Vec<Duration>,
        p_frames: Vec<Duration>,
        echoes: Vec<Duration>,
        /// Frames handed to QUIC that the client never had whole.
        missing: u64,
        /// BBR3's state every [`SAMPLE_EVERY`], by name; empty under another controller.
        states: std::collections::BTreeMap<&'static str, u32>,
        /// The worker's window in force, noq's own window and its pacing rate every
        /// [`SAMPLE_EVERY`], each sorted.
        cwnd: Vec<u64>,
        inner_cwnd: Vec<u64>,
        pacing: Vec<u64>,
        lost_packets: u64,
        digest: u64,
    }

    fn micros(since: Duration) -> u64 {
        u64::try_from(since.as_micros()).unwrap_or(u64::MAX)
    }

    /// A frame of `bytes` cut into the path's largest datagrams, each tagged.
    fn frame(conn: &Connection, bytes: usize, seq: u64, sent: u64) -> Vec<bytes::Bytes> {
        let size = conn.max_datagram_size().unwrap_or(1_000);
        let parts = bytes.div_ceil(size);
        (0..parts)
            .map(|i| {
                let len = size.min(bytes.saturating_sub(i.saturating_mul(size))).max(TAG_BYTES);
                let mut datagram = vec![0_u8; len];
                datagram[..8].copy_from_slice(&seq.to_be_bytes());
                datagram[8..10].copy_from_slice(&u16::try_from(parts).unwrap().to_be_bytes());
                datagram[10..18].copy_from_slice(&sent.to_be_bytes());
                bytes::Bytes::from(datagram)
            })
            .collect()
    }

    /// The worker's screen at 60 fps, a keyframe a second, a P-frame skipped while QUIC holds
    /// more than [`GUARD_BYTES`], as the capture guard does. Counts the frames handed over
    /// after the warm-up.
    fn stream_video(conn: Connection, epoch: Instant, handed: Arc<AtomicU64>) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut period = tokio::time::interval(FRAME_PERIOD);
            let mut seq = 0_u64;
            while conn.close_reason().is_none() {
                period.tick().await;
                seq = seq.saturating_add(1);
                let key = seq % KEY_EVERY == 1;
                let held =
                    endpoint::DATAGRAM_BUFFER.saturating_sub(conn.datagram_send_buffer_space());
                if !key && held > GUARD_BYTES {
                    continue;
                }
                let sent = epoch.elapsed();
                let datagrams =
                    frame(&conn, if key { KEYFRAME } else { P_FRAME }, seq, micros(sent));
                if sent >= VIDEO_WARMUP {
                    handed.fetch_add(1, Ordering::Relaxed);
                }
                let _queued = conn.send_many_datagrams(&datagrams);
            }
        })
    }

    /// Frames the client has had whole, and those it still waits on.
    #[derive(Default)]
    struct Seen {
        /// Frame number → parts still missing.
        pending: std::collections::HashMap<u64, u16>,
        keyframes: Vec<Duration>,
        p_frames: Vec<Duration>,
    }

    /// The client's side of the video: every frame timed from its handover to its last part.
    fn watch_video(conn: Connection, epoch: Instant, frames: Arc<Mutex<Seen>>) -> JoinHandle<()> {
        tokio::spawn(async move {
            while let Ok(datagram) = conn.read_datagram().await {
                let seq = u64::from_be_bytes(datagram[..8].try_into().unwrap());
                let parts = u16::from_be_bytes(datagram[8..10].try_into().unwrap());
                let sent =
                    Duration::from_micros(u64::from_be_bytes(datagram[10..18].try_into().unwrap()));
                let mut frames = frames.lock();
                let left = frames.pending.entry(seq).or_insert(parts);
                *left = left.saturating_sub(1);
                if *left > 0 {
                    continue;
                }
                frames.pending.remove(&seq);
                if sent < VIDEO_WARMUP {
                    continue;
                }
                let took = epoch.elapsed().saturating_sub(sent);
                if seq % KEY_EVERY == 1 {
                    frames.keyframes.push(took);
                } else {
                    frames.p_frames.push(took);
                }
            }
        })
    }

    /// The worker's congestion picture every [`SAMPLE_EVERY`] after the warm-up.
    #[derive(Default)]
    struct Sampled {
        states: std::collections::BTreeMap<&'static str, u32>,
        cwnd: Vec<u64>,
        inner_cwnd: Vec<u64>,
        pacing: Vec<u64>,
    }

    fn sample_path(
        conn: Connection,
        epoch: Instant,
        sampled: Arc<Mutex<Sampled>>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            tokio::time::sleep_until(epoch.checked_add(VIDEO_WARMUP).unwrap()).await;
            let mut tick = tokio::time::interval(SAMPLE_EVERY);
            while conn.close_reason().is_none() {
                tick.tick().await;
                let Some(snapshot) = slopty_net::congestion::snapshot(&conn) else { continue };
                let mut sampled = sampled.lock();
                if let Some(state) = snapshot.model_state {
                    let count = sampled.states.entry(state).or_default();
                    *count = count.saturating_add(1);
                }
                sampled.cwnd.push(snapshot.cwnd);
                sampled.inner_cwnd.push(snapshot.inner_cwnd);
                sampled.pacing.extend(snapshot.pacing_rate);
            }
        })
    }

    /// Keys typed while the worker streams its screen through [`video_link`].
    async fn video(seed: u64, keys: u16) -> Watched {
        let mut pair =
            link(video_link(), seed).await.unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        let epoch = Instant::now();
        let handed = Arc::new(AtomicU64::new(0));
        let frames = Arc::new(Mutex::new(Seen::default()));
        let sampled = Arc::new(Mutex::new(Sampled::default()));
        let tasks = [
            stream_video(pair.worker.clone(), epoch, Arc::clone(&handed)),
            watch_video(pair.client.clone(), epoch, Arc::clone(&frames)),
            sample_path(pair.worker.clone(), epoch, Arc::clone(&sampled)),
        ];
        let typed = type_through(&mut pair, keys, ms(50), None, VIDEO_WARMUP, |_| {}).await;
        assert_once_in_order(&typed, keys, seed);
        // The frames already handed over land before the count is read.
        tokio::time::sleep(ms(500)).await;
        tasks[0].abort();
        tokio::time::sleep(ms(500)).await;
        for task in tasks {
            task.abort();
        }
        let lost_packets = pair.worker.path_stats(noq::PathId::ZERO).map_or(0, |s| s.lost_packets);
        let frames = std::mem::take(&mut *frames.lock());
        let whole =
            u64::try_from(frames.keyframes.len().saturating_add(frames.p_frames.len())).unwrap();
        let sampled = std::mem::take(&mut *sampled.lock());
        let sort = |mut v: Vec<u64>| {
            v.sort_unstable();
            v
        };
        Watched {
            missing: handed.load(Ordering::Relaxed).saturating_sub(whole),
            keyframes: sorted(frames.keyframes),
            p_frames: sorted(frames.p_frames),
            echoes: sorted(typed.round_trips),
            states: sampled.states,
            cwnd: sort(sampled.cwnd),
            inner_cwnd: sort(sampled.inner_cwnd),
            pacing: sort(sampled.pacing),
            lost_packets,
            digest: pair.net.digest(),
        }
    }

    /// Keys typed for 15 s over a 20 Mbit/s hop while the worker streams its screen.
    async fn video_run(seed: u64) -> Watched {
        video(seed, 300).await
    }

    /// Video that leaves the link idle between frames keeps its keyframes quick and every frame
    /// whole: BBR3 refreshes its round-trip minimum from each frame's first packet instead of
    /// dipping into `ProbeRTT`, which held a keyframe caught in it to 100 to 250 ms against 55
    /// (noq-proto patch 13, `vendor/noq-proto/SLOPTY.md`).
    #[test]
    fn video_beside_keys_keeps_its_keyframes_out_of_probe_rtt() {
        for seed in 1..=2 {
            let run = simulate(seed, video_run);
            let key_p99 = percentile(&run.keyframes, 99);
            assert!(key_p99 < ms(80), "seed {seed}: keyframe p99 {key_p99:?}");
            assert_eq!(run.states.get("ProbeRTT"), None, "seed {seed}: {:?}", run.states);
            assert_eq!(run.states.get("Startup"), None, "seed {seed}: {:?}", run.states);
            assert_eq!(run.lost_packets, 0, "seed {seed}");
            assert!(run.missing <= 2, "seed {seed}: {} frames missing", run.missing);
        }
    }

    /// What video beside keys does on [`video_link`], per seed, for `docs/MEASUREMENTS.md`
    /// (`SLOPTY_CC` picks the controller).
    #[test]
    #[ignore = "diagnostic: cargo nextest run -p slopty-net --release --test sim --run-ignored only -E 'test(video_report)' --no-capture"]
    fn video_report() {
        let seeds: u64 =
            std::env::var("SLOPTY_SIM_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(8);
        let (mut keys, mut ps, mut echoes, mut missing, mut lost) =
            (Vec::new(), Vec::new(), Vec::new(), 0, 0);
        let (mut cwnd, mut inner, mut pacing) = (Vec::new(), Vec::new(), Vec::new());
        let mut states = std::collections::BTreeMap::<&str, u32>::new();
        for seed in 1..=seeds {
            let run = simulate(seed, video_run);
            println!(
                "seed {seed}: keyframe p50 {:?} p99 {:?} · P-frame p99 {:?} · echo p50 {:?} p99 {:?} · missing {} · lost {} · {:?}",
                percentile(&run.keyframes, 50),
                percentile(&run.keyframes, 99),
                percentile(&run.p_frames, 99),
                percentile(&run.echoes, 50),
                percentile(&run.echoes, 99),
                run.missing,
                run.lost_packets,
                run.states,
            );
            cwnd.extend(run.cwnd);
            inner.extend(run.inner_cwnd);
            pacing.extend(run.pacing);
            keys.extend(run.keyframes);
            ps.extend(run.p_frames);
            echoes.extend(run.echoes);
            missing += run.missing;
            lost += run.lost_packets;
            for (state, n) in run.states {
                *states.entry(state).or_default() += n;
            }
        }
        let (keys, ps, echoes) = (sorted(keys), sorted(ps), sorted(echoes));
        let mid = |mut v: Vec<u64>| {
            v.sort_unstable();
            (v.get(v.len() / 2).copied().unwrap_or(0), v.last().copied().unwrap_or(0))
        };
        println!(
            "window in force p50/max {:?} · noq's window p50/max {:?} · pacing B/s p50/max {:?}",
            mid(cwnd),
            mid(inner),
            mid(pacing)
        );
        println!(
            "all: keyframe p50 {:?} p99 {:?} max {:?} · P-frame p50 {:?} p99 {:?} · echo p50 {:?} p99 {:?} max {:?} · missing {missing} · lost {lost} · {states:?}",
            percentile(&keys, 50),
            percentile(&keys, 99),
            keys.last(),
            percentile(&ps, 50),
            percentile(&ps, 99),
            percentile(&echoes, 50),
            percentile(&echoes, 99),
            echoes.last(),
        );
    }

    // (g) Packets per keystroke.

    /// Keys typed a tenth of a second apart on a quiet 5 ms hop: the packets each end sent per
    /// key, worker then client.
    async fn packets_per_key(seed: u64) -> (f64, f64) {
        let mut pair = link(hop(0.0), seed).await.unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        let keys = 50;
        let sent = |conn: &Connection| conn.stats().udp_tx.datagrams;
        let (worker_before, client_before) = (sent(&pair.worker), sent(&pair.client));
        let typed = type_through(&mut pair, keys, ms(100), None, ms(0), |_| {}).await;
        assert_once_in_order(&typed, keys, seed);
        tokio::time::sleep(ms(50)).await;
        #[expect(clippy::cast_precision_loss, reason = "packet counts below 2^53")]
        let per_key = |n: u64| n as f64 / f64::from(keys);
        (
            per_key(sent(&pair.worker).saturating_sub(worker_before)),
            per_key(sent(&pair.client).saturating_sub(client_before)),
        )
    }

    /// The worker's echo leaves before its delayed-ACK timer on the key it answers, and carries
    /// that ACK: one packet a key, where an ACK of its own made two (noq-proto patch 15,
    /// `vendor/noq-proto/SLOPTY.md`).
    #[test]
    fn an_echo_carries_the_ack_of_its_key() {
        for seed in 1..=2 {
            let (worker, client) = simulate(seed, packets_per_key);
            println!(
                "seed {seed}: {worker:.2} packets a key from the worker, {client:.2} from the client"
            );
            assert!(worker < 1.2, "seed {seed}: {worker:.2} packets a key from the worker");
        }
    }

    // (h) A path narrower than `PATH_MTU`.

    /// Largest UDP payload an IPv4 path with a 1240-byte MTU carries, as some L2TP, IP security
    /// and cellular VPNs have: above the handshake's 1200, below `endpoint::PATH_MTU`.
    const NARROW: usize = 1240 - 28;

    /// A socket whose path drops, without a word, every datagram larger than `max`.
    #[derive(Debug)]
    struct Narrow<S> {
        inner: S,
        max: usize,
    }

    impl AsyncUdpSocket for Narrow<slopty_shape::sim::Socket> {
        fn create_sender(&self) -> Pin<Box<dyn UdpSender>> {
            Box::pin(Narrow { inner: self.inner.create_sender(), max: self.max })
        }

        fn poll_recv(
            &mut self,
            cx: &mut Context<'_>,
            bufs: &mut [IoSliceMut<'_>],
            meta: &mut [RecvMeta],
        ) -> Poll<io::Result<usize>> {
            self.inner.poll_recv(cx, bufs, meta)
        }

        fn local_addr(&self) -> io::Result<SocketAddr> {
            self.inner.local_addr()
        }

        fn may_fragment(&self) -> bool {
            false
        }
    }

    impl UdpSender for Narrow<Pin<Box<dyn UdpSender>>> {
        fn poll_send(
            self: Pin<&mut Self>,
            transmit: &Transmit<'_>,
            cx: &mut Context<'_>,
        ) -> Poll<io::Result<()>> {
            let this = self.get_mut();
            let size = transmit.segment_size.unwrap_or(transmit.contents.len());
            for datagram in transmit.contents.chunks(size).filter(|d| d.len() <= this.max) {
                let one = Transmit { contents: datagram, segment_size: None, ..*transmit };
                // The simulated network takes every send at once, so nothing is sent twice.
                ready!(this.inner.as_mut().poll_send(&one, cx))?;
            }
            Poll::Ready(Ok(()))
        }
    }

    /// 256 kB from the worker to the client over a [`NARROW`] path, as full packets: how long
    /// it took, the MTU the worker ended on, and the black holes it detected.
    async fn narrow_path(seed: u64) -> (Duration, u16, u64) {
        let net = Net::new(hop(0.0), seed);
        let narrow = |at| Narrow { inner: net.bind(at).unwrap(), max: NARROW };
        let (worker, client) = (Box::new(narrow(WORKER)), Box::new(narrow(CLIENT)));
        let pair = link_on(net.clone(), worker, client, seed)
            .await
            .unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        let bytes = vec![7_u8; 256 * 1024];
        let started = Instant::now();
        let sending = tokio::spawn({
            let (worker, bytes) = (pair.worker.clone(), bytes.clone());
            async move {
                let mut tx = worker.open_uni().await.unwrap();
                tx.write_all(&bytes).await.unwrap();
                tx.finish().unwrap();
            }
        });
        let mut rx = pair.client.accept_uni().await.unwrap();
        let got = rx.read_to_end(bytes.len()).await.unwrap();
        let took = started.elapsed();
        sending.await.unwrap();
        assert_eq!(got, bytes, "seed {seed}");
        let path = pair.worker.path_stats(noq::PathId::ZERO).unwrap();
        (took, path.current_mtu, path.black_holes_detected)
    }

    /// A path narrower than `PATH_MTU` carries the handshake, which is padded to 1200 only, and
    /// then loses every full packet. Black-hole detection sees the bursts and falls back to
    /// `MIN_MTU`, and the connection carries on at it.
    #[test]
    fn a_path_narrower_than_the_packets_falls_back_to_the_minimum() {
        for seed in GATE_SEEDS {
            let (took, mtu, black_holes) = simulate(seed, narrow_path);
            println!("seed {seed}: 256 kB in {took:?}, MTU {mtu}, {black_holes} black holes");
            assert_eq!(mtu, endpoint::MIN_MTU, "seed {seed}");
            assert!(black_holes >= 1, "seed {seed}");
            assert!(took < Duration::from_secs(5), "seed {seed}: {took:?}");
        }
    }

    // (i) A path that delivers every datagram twice.

    /// A socket whose path delivers every datagram it sends twice, back to back.
    #[derive(Debug)]
    struct Twice<S> {
        inner: S,
    }

    impl AsyncUdpSocket for Twice<slopty_shape::sim::Socket> {
        fn create_sender(&self) -> Pin<Box<dyn UdpSender>> {
            Box::pin(Twice { inner: self.inner.create_sender() })
        }

        fn poll_recv(
            &mut self,
            cx: &mut Context<'_>,
            bufs: &mut [IoSliceMut<'_>],
            meta: &mut [RecvMeta],
        ) -> Poll<io::Result<usize>> {
            self.inner.poll_recv(cx, bufs, meta)
        }

        fn local_addr(&self) -> io::Result<SocketAddr> {
            self.inner.local_addr()
        }
    }

    impl UdpSender for Twice<Pin<Box<dyn UdpSender>>> {
        fn poll_send(
            self: Pin<&mut Self>,
            transmit: &Transmit<'_>,
            cx: &mut Context<'_>,
        ) -> Poll<io::Result<()>> {
            let this = self.get_mut();
            let size = transmit.segment_size.unwrap_or(transmit.contents.len());
            for datagram in transmit.contents.chunks(size) {
                let one = Transmit { contents: datagram, segment_size: None, ..*transmit };
                // The simulated network takes every send at once, so nothing is sent a third time.
                ready!(this.inner.as_mut().poll_send(&one, cx))?;
                ready!(this.inner.as_mut().poll_send(&one, cx))?;
            }
            Poll::Ready(Ok(()))
        }
    }

    /// A dial where every datagram arrives twice, and what it took.
    async fn doubled(seed: u64) -> Result<Duration, String> {
        let net = Net::new(hop(0.0), seed);
        let twice = |at| Twice { inner: net.bind(at).unwrap() };
        let (worker, client) = (Box::new(twice(WORKER)), Box::new(twice(CLIENT)));
        let pair = link_on(net.clone(), worker, client, seed).await?;
        Ok(pair.took)
    }

    /// A dial where every datagram arrives twice connects. The worker's first Initial leaves
    /// coalesced ahead of its Handshake and 1-RTT packets, so the padding goes to those and the
    /// Initial ends in the worker's HELLO. When the shuffled transport parameters end in its
    /// stateless reset token, the copy of that Initial, read after the first had handed the
    /// client the token, ended in the token. noq took it for a stateless reset and the dial
    /// failed with "reset by peer": 25 of the first 300 seeds, seed 7 the first
    /// (`vendor/noq-proto/SLOPTY.md`, patch 17). Under load a real client reads the worker's
    /// resent Initial in the same batch as its first, which is the same thing.
    #[test]
    fn a_path_that_delivers_every_datagram_twice_still_connects() {
        for seed in GATE_SEEDS {
            let took = simulate(seed, doubled).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
            assert!(took < LOSSY_DIAL, "seed {seed}: the dial took {took:?}");
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
        let doubled: Vec<Duration> = NIGHTLY_SEEDS
            .map(|seed| simulate(seed, doubled).unwrap_or_else(|e| panic!("seed {seed}: {e}")))
            .collect();
        println!(
            "dial with every datagram twice: {} ({:?} real)",
            spread(doubled),
            wall_since(started)
        );

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
