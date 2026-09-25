//! A terminal's echo beside a video stream on one connection, through a shaped link.
//!
//! noq as it ships fills each packet with queued datagrams before it writes any stream data, so
//! an echo written while a frame's datagrams wait for the congestion window leaves after the
//! frame. Slopty's endpoint writes the control and session streams first
//! (`slopty_net::streams::AHEAD_OF_DATAGRAMS`); `SLOPTY_DATAGRAMS_FIRST=1` puts noq's order back.
//! Here the worker's side of a real connection runs `/bin/cat` behind the control stream and a
//! session stream, the way a terminal does, while it floods datagrams the way the encoder hands
//! frames over: one burst per frame at 60 fps, a keyframe a second. The link is
//! `slopty-shape` at 20 Mbit/s, so the path, not this machine, is the bottleneck. After a clear
//! link for reference, five runs on fresh connections: no video, video in bursts, video kept
//! to a 5 ms slice of the link while the connection is interactive (the rule the worker's audio
//! lane applies while audio flows), that slice also metered to 80% of the link, and bursts on a
//! link that falls to half its rate as the typing starts, the way Wi-Fi or LTE does.
//!
//! The congestion controller is the endpoint's, so `SLOPTY_CC` picks it for a whole run.
//! `SLOPTY_ECHO_QUEUE_MS` sizes the bottleneck queue (100 ms of the link unless set),
//! `SLOPTY_ECHO_LOSS` drops that share of packets at random, `SLOPTY_ECHO_ARMS` picks arms
//! (`off,bursts,laned,metered,halved`), and `SLOPTY_ECHO_TRACE` names a directory (absolute:
//! the test runs in the crate's directory) for a CSV per arm: the worker's window, bytes in
//! flight, pacing and delivery rates, round trips, losses and the bottleneck's queue every
//! 10 ms, with BBR3's model beside them.

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::fmt::Write as _;
    use std::net::SocketAddr;
    use std::process::Stdio;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use bytes::Bytes;
    use parking_lot::Mutex;
    use slopty_core::{ClientId, SessionId, WorkerId};
    use slopty_net::admission::Admission;
    use slopty_net::client::{bind_client, connect_addr};
    use slopty_net::endpoint::{self, DATAGRAM_BUFFER};
    use slopty_net::streams::{self, Uni};
    use slopty_net::worker::WorkerListener;
    use slopty_net::{ClientMsg, Connection, WorkerMsg, congestion};
    use slopty_proto::handshake::{Caps, ClientKind, Hello, HelloAck};
    use slopty_proto::terminal::{TermEvent, TermRequest};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    /// The link: 20 Mbit/s, 2 ms each way, and a bottleneck queue of [`queue_ms`].
    const RATE: u64 = 2_500_000;
    /// A home router's buffer at this rate: 100 ms.
    const QUEUE_MS: u64 = 100;
    const WARMUP: Duration = Duration::from_secs(1);
    const SAMPLE_EVERY: Duration = Duration::from_millis(10);
    /// Each datagram starts with its frame's number, its part, the frame's parts and when the
    /// frame was handed to QUIC, so the client can time whole frames.
    const TAG_BYTES: usize = 20;
    const FRAME_PERIOD: Duration = Duration::from_micros(16_667);
    /// A P-frame: 12 Mbit/s at 60 fps, well under the link.
    const P_FRAME: usize = 25_000;
    const KEYFRAME: usize = 130_000;
    const FRAMES_PER_KEYFRAME: u64 = 60;
    /// The worker's cut: the largest datagram the path carries, at most `media::MAX_DATAGRAM`, so
    /// each one fills its packet and leaves no room for stream data beside it.
    const MAX_DATAGRAM: usize = slopty_proto::media::MAX_DATAGRAM;
    /// What a keystroke's echo costs on the session stream: a frame with one row, about 240 B.
    const ECHO_BYTES: usize = 240;
    /// What the lane lets QUIC hold ahead of an echo: 5 ms of the link.
    const SLICE_BYTES: usize = 12_500;
    const LANE_TICK: Duration = Duration::from_millis(1);
    /// Metered, what the lane hands QUIC a tick: 2 MB/s, 80% of the link, the rate a controller
    /// that knows the path would allow.
    const METER_BYTES_PER_TICK: usize = 2_000;
    /// What the capture guard lets QUIC hold before it skips a frame.
    const GUARD_BYTES: usize = 50_000;
    const KEYS: usize = 320;
    /// The clear link only sets the floor: fewer keys.
    const CLEAR_KEYS: usize = 100;
    /// An echo not back by then is counted as a stall at this latency, and the next key goes.
    const STALL: Duration = Duration::from_secs(5);

    /// Someone typed or saw an echo within the last second: the lane is on.
    const INTERACTIVE_HOLD: Duration = Duration::from_secs(1);

    /// When the connection last carried a key in or an echo out.
    #[derive(Clone, Default)]
    struct Interactive(Arc<Mutex<Option<Instant>>>);

    impl Interactive {
        fn mark(&self) {
            *self.0.lock() = Some(Instant::now());
        }

        fn is_active(&self) -> bool {
            self.0.lock().is_some_and(|at| at.elapsed() <= INTERACTIVE_HOLD)
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Video {
        Off,
        Bursts,
        Laned,
        Metered,
        /// Bursts, and the link falls to half its rate as the typing starts.
        Halved,
    }

    fn held(conn: &Connection) -> usize {
        DATAGRAM_BUFFER.saturating_sub(conn.datagram_send_buffer_space())
    }

    fn link() -> slopty_shape::Link {
        let queue_ms = queue_ms();
        slopty_shape::Link {
            delay: Duration::from_millis(2),
            jitter: Duration::ZERO,
            loss: std::env::var("SLOPTY_ECHO_LOSS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0),
            rate: RATE,
            queue: RATE.saturating_mul(queue_ms) / 1_000,
        }
    }

    fn queue_ms() -> u64 {
        std::env::var("SLOPTY_ECHO_QUEUE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(QUEUE_MS)
    }

    fn micros(since: Duration) -> u64 {
        u64::try_from(since.as_micros()).unwrap_or(u64::MAX)
    }

    fn frame(conn: &Connection, bytes: usize, seq: u64, epoch: Instant) -> Vec<Bytes> {
        let size = conn.max_datagram_size().unwrap_or(MAX_DATAGRAM).min(MAX_DATAGRAM);
        let parts = bytes.div_ceil(size);
        let sent = micros(epoch.elapsed());
        (0..parts)
            .map(|i| {
                let len = size.min(bytes.saturating_sub(i.saturating_mul(size))).max(TAG_BYTES);
                let mut datagram = vec![0_u8; len];
                datagram[..8].copy_from_slice(&seq.to_be_bytes());
                datagram[8..10].copy_from_slice(&u16::try_from(i).unwrap().to_be_bytes());
                datagram[10..12].copy_from_slice(&u16::try_from(parts).unwrap().to_be_bytes());
                datagram[12..20].copy_from_slice(&sent.to_be_bytes());
                Bytes::from(datagram)
            })
            .collect()
    }

    /// What the client saw of the video.
    #[derive(Default)]
    struct Received {
        /// Frame number → parts still missing, and when it was sent.
        pending: HashMap<u64, (u16, u64)>,
        /// Handed to QUIC → last part arrived, ms, for frames sent after the warm-up.
        keyframes: Vec<f64>,
        p_frames: Vec<f64>,
        bytes: u64,
    }

    impl Received {
        fn take(&mut self, datagram: &[u8], epoch: Instant) {
            let (Some(seq), Some(parts), Some(sent)) = (
                datagram.get(..8).and_then(|b| b.try_into().ok()).map(u64::from_be_bytes),
                datagram.get(10..12).and_then(|b| b.try_into().ok()).map(u16::from_be_bytes),
                datagram.get(12..20).and_then(|b| b.try_into().ok()).map(u64::from_be_bytes),
            ) else {
                return;
            };
            let warm = micros(WARMUP);
            if sent >= warm {
                self.bytes = self.bytes.saturating_add(datagram.len() as u64);
            }
            let left = &mut self.pending.entry(seq).or_insert((parts, sent)).0;
            *left = left.saturating_sub(1);
            if *left > 0 {
                return;
            }
            self.pending.remove(&seq);
            if sent < warm {
                return;
            }
            #[expect(clippy::cast_precision_loss, reason = "microseconds below 2^53")]
            let took = micros(epoch.elapsed()).saturating_sub(sent) as f64 / 1e3;
            if seq % FRAMES_PER_KEYFRAME == 1 {
                self.keyframes.push(took);
            } else {
                self.p_frames.push(took);
            }
        }
    }

    /// Video waiting to be handed to QUIC a slice at a time. Metered, it is also handed over
    /// no faster than [`METER_BYTES_PER_TICK`] a tick.
    #[derive(Default)]
    struct Lane {
        queue: VecDeque<Bytes>,
        metered: bool,
        tokens: usize,
    }

    impl Lane {
        fn pump(&mut self, conn: &Connection) {
            let held = held(conn);
            let mut room = SLICE_BYTES.saturating_sub(held);
            if self.metered {
                room = room.min(self.tokens);
            }
            let mut batch = Vec::new();
            let mut bytes = 0_usize;
            while let Some(front) = self.queue.front() {
                let first_into_empty = !self.metered && batch.is_empty() && held == 0;
                if front.len() > room && !first_into_empty {
                    break;
                }
                room = room.saturating_sub(front.len());
                bytes = bytes.saturating_add(front.len());
                batch.extend(self.queue.pop_front());
            }
            self.tokens = self.tokens.saturating_sub(bytes);
            if !batch.is_empty() {
                let _queued = conn.send_many_datagrams(&batch);
            }
        }

        fn tick(&mut self, conn: &Connection) {
            if self.metered {
                self.tokens = self.tokens.saturating_add(METER_BYTES_PER_TICK).min(SLICE_BYTES);
            }
            self.pump(conn);
        }
    }

    /// Frames at 60 fps until the connection closes; returns how many went out.
    async fn flood(
        conn: Connection,
        video: Video,
        interactive: Interactive,
        sent: Arc<AtomicU64>,
        epoch: Instant,
    ) {
        let metered = video == Video::Metered;
        let lane = Arc::new(Mutex::new(Lane { metered, ..Lane::default() }));
        if matches!(video, Video::Laned | Video::Metered) {
            let (conn, lane) = (conn.clone(), Arc::clone(&lane));
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(LANE_TICK);
                while conn.close_reason().is_none() {
                    tick.tick().await;
                    lane.lock().tick(&conn);
                }
            });
        }
        let mut period = tokio::time::interval(FRAME_PERIOD);
        let mut seq = 0_u64;
        while conn.close_reason().is_none() {
            period.tick().await;
            seq = seq.saturating_add(1);
            let key = seq % FRAMES_PER_KEYFRAME == 1;
            let waiting = lane.lock().queue.iter().map(Bytes::len).sum::<usize>();
            if !key && held(&conn).saturating_add(waiting) > GUARD_BYTES {
                continue;
            }
            let datagrams = frame(&conn, if key { KEYFRAME } else { P_FRAME }, seq, epoch);
            if epoch.elapsed() >= WARMUP {
                sent.fetch_add(1, Ordering::Relaxed);
            }
            let laned = matches!(video, Video::Laned | Video::Metered)
                && (interactive.is_active() || waiting > 0);
            if laned {
                let mut lane = lane.lock();
                lane.queue.extend(datagrams);
                lane.pump(&conn);
            } else {
                let _queued = conn.send_many_datagrams(&datagrams);
            }
        }
    }

    /// The worker's side: `/bin/cat` behind the control stream and one session stream, and the
    /// video beside them.
    async fn serve(
        listener: WorkerListener,
        video: Video,
        frames: Arc<AtomicU64>,
        ahead: Arc<Mutex<Vec<f64>>>,
        epoch: Instant,
        accepted: tokio::sync::oneshot::Sender<Connection>,
    ) {
        let mut client = listener.accept().await.unwrap();
        let _unwatched = accepted.send(client.conn.clone());
        let ack = HelloAck {
            worker: WorkerId::new(),
            name: "worker".to_owned(),
            app_version: "0".to_owned(),
            caps: Caps::empty(),
            sessions: Vec::new(),
        };
        client.tx.send(&WorkerMsg::HelloAck(ack)).await.unwrap();
        let wait = streams::SESSION_STREAM_WAIT;
        let mut stream = streams::open_session(&client.conn, SessionId::new(), wait).await.unwrap();
        let interactive = Interactive::default();
        if video != Video::Off {
            tokio::spawn(flood(client.conn.clone(), video, interactive.clone(), frames, epoch));
        }
        let mut cat = tokio::process::Command::new("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let (mut stdin, mut stdout) = (cat.stdin.take().unwrap(), cat.stdout.take().unwrap());
        let echo = {
            let (interactive, conn) = (interactive.clone(), client.conn.clone());
            tokio::spawn(async move {
                let mut buf = [0_u8; 256];
                loop {
                    let n = stdout.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    interactive.mark();
                    let mut text = String::from_utf8_lossy(&buf[..n]).into_owned();
                    text.extend(std::iter::repeat_n(' ', ECHO_BYTES));
                    let held = u32::try_from(held(&conn)).unwrap_or(u32::MAX);
                    ahead.lock().push(f64::from(held) / 1e3);
                    if stream.send(&TermEvent::Title(text)).await.is_err() {
                        break;
                    }
                }
            })
        };
        while let Ok(msg) = client.rx.recv().await {
            if let ClientMsg::Term { req: TermRequest::Raw(bytes), .. } = msg {
                interactive.mark();
                stdin.write_all(&bytes).await.unwrap();
            }
        }
        echo.abort();
    }

    fn quantiles(samples: &mut [f64]) -> (f64, f64, f64) {
        if samples.is_empty() {
            return (f64::NAN, f64::NAN, f64::NAN);
        }
        samples.sort_by(f64::total_cmp);
        let at = |q: f64| {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss,
                reason = "an index below 2^53"
            )]
            let i = ((samples.len().saturating_sub(1)) as f64 * q).round() as usize;
            samples[i]
        };
        (at(0.5), at(0.99), at(1.0))
    }

    /// One reading of the worker's path.
    struct Sample {
        at: Duration,
        cwnd: u64,
        inner_cwnd: u64,
        in_flight: u64,
        pacing: Option<u64>,
        delivery: Option<u64>,
        min_rtt: Option<Duration>,
        rtt: Duration,
        lost: u64,
        congestion: u64,
        held: usize,
        queue: Duration,
        model: Option<String>,
    }

    /// The fields of BBR3's model worth a column, out of its `Debug`.
    const MODEL_FIELDS: [&str; 8] = [
        "state",
        "max_bw",
        "bw",
        "min_rtt",
        "extra_acked",
        "inflight_longterm",
        "inflight_shortterm",
        "full_bw_reached",
    ];

    /// `name`'s value in a `Debug` print: what follows ` name: ` up to the next `,`.
    fn field<'a>(debug: &'a str, name: &str) -> &'a str {
        let key = format!(" {name}: ");
        debug
            .find(&key)
            .and_then(|at| debug.get(at.saturating_add(key.len())..))
            .and_then(|rest| rest.split([',', ' ']).next())
            .unwrap_or("")
    }

    /// The worker's path every [`SAMPLE_EVERY`] until the run ends.
    async fn watch(
        conn: Connection,
        relay: Arc<slopty_shape::relay::Relay>,
        epoch: Instant,
        model: bool,
        samples: Arc<Mutex<Vec<Sample>>>,
    ) {
        let mut tick = tokio::time::interval(SAMPLE_EVERY);
        while conn.close_reason().is_none() {
            tick.tick().await;
            let (Some(path), Some(cc)) =
                (conn.path_stats(noq::PathId::ZERO), congestion::snapshot(&conn))
            else {
                continue;
            };
            let queue = relay.queue_delay_down().await;
            samples.lock().push(Sample {
                at: epoch.elapsed(),
                cwnd: cc.cwnd,
                inner_cwnd: cc.inner_cwnd,
                in_flight: cc.in_flight,
                pacing: cc.pacing_rate,
                delivery: cc.delivery_rate,
                min_rtt: cc.min_rtt,
                rtt: path.rtt,
                lost: path.lost_packets,
                congestion: path.congestion_events,
                held: held(&conn),
                queue,
                model: model.then(|| congestion::debug_state(&conn)).flatten(),
            });
        }
    }

    fn write_trace(dir: &str, name: &str, samples: &[Sample]) {
        let mut csv = String::from(
            "ms,cwnd,inner_cwnd,in_flight,pacing_Bps,delivery_Bps,min_rtt_us,rtt_us,lost,congestion,held,queue_us,",
        );
        csv.push_str(&MODEL_FIELDS.join(","));
        csv.push('\n');
        for s in samples {
            write!(
                csv,
                "{},{},{},{},{},{},{},{},{},{},{},{}",
                s.at.as_millis(),
                s.cwnd,
                s.inner_cwnd,
                s.in_flight,
                s.pacing.map_or_else(String::new, |p| p.to_string()),
                s.delivery.map_or_else(String::new, |p| p.to_string()),
                s.min_rtt.map_or_else(String::new, |r| r.as_micros().to_string()),
                s.rtt.as_micros(),
                s.lost,
                s.congestion,
                s.held,
                s.queue.as_micros(),
            )
            .unwrap();
            for name in MODEL_FIELDS {
                csv.push(',');
                csv.push_str(s.model.as_deref().map_or("", |m| field(m, name)));
            }
            csv.push('\n');
        }
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(format!("{dir}/{name}.csv"), csv).unwrap();
    }

    struct Run {
        echo: (f64, f64, f64),
        echo_p95: f64,
        /// Kilobytes of datagrams QUIC held when each echo was written: what it waited behind.
        ahead: (f64, f64, f64),
        /// While the keys went: the window and bytes in flight (kB), the smoothed round trip and
        /// the bottleneck's queue (ms), sampled every 10 ms.
        cwnd: (f64, f64, f64),
        in_flight: (f64, f64, f64),
        rtt: (f64, f64, f64),
        queue: (f64, f64, f64),
        /// Packets QUIC declared lost while the keys went.
        lost: u64,
        congestion: u64,
        frames: u64,
        /// Handed to QUIC → last datagram arrived, ms.
        keyframe: (f64, f64, f64),
        p_frame: (f64, f64, f64),
        /// Frames sent after the warm-up whose every datagram arrived.
        complete: usize,
        /// Video bytes delivered after the warm-up, Mbit/s.
        goodput: f64,
        overflowed: u64,
        /// Echoes not back within [`STALL`].
        stalls: u32,
    }

    #[expect(clippy::cast_precision_loss, reason = "byte counts below 2^53")]
    fn kb(bytes: u64) -> f64 {
        bytes as f64 / 1e3
    }

    /// Key → echo on a fresh connection through a fresh shaper, after a second of video for
    /// the congestion controller to find the link.
    async fn run(video: Video, link: slopty_shape::Link, keys: usize, trace: Option<&str>) -> Run {
        let epoch = Instant::now();
        let listener =
            WorkerListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)), Admission::default())
                .unwrap();
        let worker_addr = listener.local_addr().unwrap();
        let frames = Arc::new(AtomicU64::new(0));
        let ahead = Arc::new(Mutex::new(Vec::new()));
        let (accepted, worker_conn) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(serve(
            listener,
            video,
            Arc::clone(&frames),
            Arc::clone(&ahead),
            epoch,
            accepted,
        ));
        let relay = Arc::new(
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
            let relay = Arc::clone(&relay);
            tokio::spawn(async move { relay.run().await })
        };
        let endpoint = bind_client().unwrap();
        let hello = Hello {
            client: ClientId::new(),
            kind: ClientKind::Tool,
            name: "echo".to_owned(),
            app_version: "0".to_owned(),
            caps: Caps::empty(),
        };
        let mut worker = connect_addr(&endpoint, relay.addr().unwrap(), hello).await.unwrap();
        let samples = Arc::new(Mutex::new(Vec::new()));
        let watcher = tokio::spawn(watch(
            worker_conn.await.unwrap(),
            Arc::clone(&relay),
            epoch,
            trace.is_some(),
            Arc::clone(&samples),
        ));
        let Uni::Session { session, rx: mut echoes } =
            streams::accept_uni(&worker.conn).await.unwrap()
        else {
            panic!("a session stream");
        };
        let received = Arc::new(Mutex::new(Received::default()));
        let reader = {
            let (conn, received) = (worker.conn.clone(), Arc::clone(&received));
            tokio::spawn(async move {
                while let Ok(datagram) = conn.read_datagram().await {
                    received.lock().take(&datagram, epoch);
                }
            })
        };
        tokio::time::sleep(WARMUP.saturating_sub(epoch.elapsed())).await;
        if video == Video::Halved {
            relay.set_rate(link.rate / 2).await;
        }
        let typing = epoch.elapsed();

        let mut took = Vec::with_capacity(keys);
        let mut stalls = 0_u32;
        for i in 0..keys {
            let key = b'a'.saturating_add(u8::try_from(i % 26).unwrap());
            let sent = Instant::now();
            let req = TermRequest::Raw(vec![key]);
            worker.tx.send(&ClientMsg::Term { session, req }).await.unwrap();
            let deadline = tokio::time::Instant::from_std(sent.checked_add(STALL).unwrap());
            loop {
                let Ok(event) = tokio::time::timeout_at(deadline, echoes.recv()).await else {
                    stalls = stalls.saturating_add(1);
                    break;
                };
                if matches!(event.unwrap(), TermEvent::Title(t) if t.as_bytes().contains(&key)) {
                    break;
                }
            }
            took.push(sent.elapsed().as_secs_f64() * 1e3);
            // Keys land anywhere in the frame period, not in step with it.
            let gap = 20_u64.saturating_add(u64::try_from(i).unwrap().saturating_mul(7) % 23);
            tokio::time::sleep(Duration::from_millis(gap)).await;
        }
        let typed = epoch.elapsed().saturating_sub(typing);
        worker.close();
        endpoint.close(0_u32.into(), b"done");
        let _idle = tokio::time::timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
        let carried = relay.carried().await;
        reader.abort();
        relayed.abort();
        server.abort();
        watcher.abort();

        let samples = std::mem::take(&mut *samples.lock());
        if let Some(dir) = trace {
            let cc = std::env::var(endpoint::CC_ENV).unwrap_or_else(|_| "default".to_owned());
            write_trace(dir, &format!("{cc}-q{}-{video:?}", queue_ms()), &samples);
        }
        let during: Vec<&Sample> = samples.iter().filter(|s| s.at >= typing).collect();
        let column = |f: fn(&Sample) -> f64| {
            let mut values: Vec<f64> = during.iter().map(|s| f(s)).collect();
            quantiles(&mut values)
        };
        let delta = |f: fn(&Sample) -> u64| {
            during.last().map_or(0, |l| f(l).saturating_sub(during.first().map_or(0, |s| f(s))))
        };
        let mut received = std::mem::take(&mut *received.lock());
        let complete = received.keyframes.len().saturating_add(received.p_frames.len());
        #[expect(clippy::cast_precision_loss, reason = "bytes below 2^53")]
        let goodput = received.bytes as f64 * 8.0 / 1e6 / typed.as_secs_f64();
        let echo = quantiles(&mut took);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "an index below 2^53"
        )]
        let echo_p95 = took[((took.len().saturating_sub(1)) as f64 * 0.95).round() as usize];
        Run {
            echo,
            echo_p95,
            ahead: quantiles(&mut ahead.lock()),
            cwnd: column(|s| kb(s.cwnd)),
            in_flight: column(|s| kb(s.in_flight)),
            rtt: column(|s| s.rtt.as_secs_f64() * 1e3),
            queue: column(|s| s.queue.as_secs_f64() * 1e3),
            lost: delta(|s| s.lost),
            congestion: delta(|s| s.congestion),
            frames: frames.load(Ordering::Relaxed),
            keyframe: quantiles(&mut received.keyframes),
            p_frame: quantiles(&mut received.p_frames),
            complete,
            goodput,
            overflowed: carried.down.overflowed,
            stalls,
        }
    }

    fn arms() -> Vec<Video> {
        let all = [Video::Off, Video::Bursts, Video::Laned, Video::Metered, Video::Halved];
        let Ok(chosen) = std::env::var("SLOPTY_ECHO_ARMS") else {
            return all.to_vec();
        };
        all.into_iter()
            .filter(|v| chosen.split(',').any(|c| c.eq_ignore_ascii_case(&format!("{v:?}"))))
            .collect()
    }

    /// The measurement behind "nothing on a worker connection waits behind anything slow" and
    /// the congestion controller's ruling (MEASUREMENTS.md). Run it in release:
    /// `cargo nextest run -p slopty-net --release --test echo_beside_flood --run-ignored only
    /// --no-capture`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "a measurement: about 90 s over a shaped link"]
    async fn echo_beside_a_video_flood() {
        let trace = std::env::var("SLOPTY_ECHO_TRACE").ok();
        let cc = std::env::var(endpoint::CC_ENV).unwrap_or_else(|_| "default".to_owned());
        let link = link();
        eprintln!(
            "MEASURE cc {cc}, queue {} ms ({} B), loss {}",
            queue_ms(),
            link.queue,
            link.loss
        );
        let clear = run(Video::Off, slopty_shape::Link::CLEAR, CLEAR_KEYS, None).await.echo;
        eprintln!(
            "MEASURE clear link, no video: echo p50 {:.2} / p99 {:.2} / max {:.2} ms",
            clear.0, clear.1, clear.2
        );
        let mut rows = Vec::new();
        for video in arms() {
            let run = run(video, link, KEYS, trace.as_deref()).await;
            let (p50, p99, max) = run.echo;
            let (a50, a99, amax) = run.ahead;
            eprintln!(
                "MEASURE {video:?}: echo p50 {p50:.2} / p95 {:.2} / p99 {p99:.2} / max {max:.2} ms; \
                 datagrams ahead of it p50 {a50:.1} / p99 {a99:.1} / max {amax:.1} kB; \
                 cwnd p50 {:.0} / p99 {:.0} kB; in flight p50 {:.1} / p99 {:.1} / max {:.1} kB; \
                 rtt p50 {:.1} / p99 {:.1} ms; queue p50 {:.1} / p99 {:.1} / max {:.1} ms; \
                 {} lost, {} congestion events, {} overflowed; \
                 keyframe p50 {:.1} / p99 {:.1} / max {:.1} ms; P-frame p50 {:.1} / p99 {:.1} ms; \
                 {} of {} frames whole, {:.1} Mbit/s; {} echoes stalled",
                run.echo_p95,
                run.cwnd.0,
                run.cwnd.1,
                run.in_flight.0,
                run.in_flight.1,
                run.in_flight.2,
                run.rtt.0,
                run.rtt.1,
                run.queue.0,
                run.queue.1,
                run.queue.2,
                run.lost,
                run.congestion,
                run.overflowed,
                run.keyframe.0,
                run.keyframe.1,
                run.keyframe.2,
                run.p_frame.0,
                run.p_frame.1,
                run.complete,
                run.frames,
                run.goodput,
                run.stalls,
            );
            rows.push((video, run));
        }
        // Load-independent, unlike the round trip on a shared machine: what QUIC held in front
        // of each echo while the lane ran. A slice and one datagram past it at most.
        if let Some((_, laned)) = rows.iter().find(|(video, _)| *video == Video::Laned) {
            assert!(laned.ahead.1 <= 14.0, "the lane holds QUIC to its slice: {:?}", laned.ahead);
        }
    }
}
