//! A terminal's echo beside a video stream on one connection, through a shaped link.
//!
//! noq fills each packet with queued datagrams before it writes any stream data, so an echo
//! written while a frame's datagrams wait for the congestion window leaves after the frame.
//! Here the worker's side of a real connection runs `/bin/cat` behind the control stream and a
//! session stream, the way a terminal does, while it floods datagrams the way the encoder hands
//! frames over: one burst per frame at 60 fps, a keyframe a second. The link is
//! `slopty-shape` at 20 Mbit/s, so the path, not this machine, is the bottleneck. After a clear
//! link for reference, four runs on fresh connections: no video, video in bursts, video kept
//! to a 5 ms slice of the link while the connection is interactive (the rule the worker's audio
//! lane applies while audio flows), and that slice also metered to 80% of the link.

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
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
    use slopty_net::endpoint::DATAGRAM_BUFFER;
    use slopty_net::streams::{self, Uni};
    use slopty_net::worker::WorkerListener;
    use slopty_net::{ClientMsg, Connection, WorkerMsg};
    use slopty_proto::PROTOCOL_VERSION;
    use slopty_proto::handshake::{Caps, ClientKind, Hello, HelloAck};
    use slopty_proto::terminal::{TermEvent, TermRequest};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    /// The link: 20 Mbit/s, 2 ms each way, a bottleneck queue of 100 ms.
    const LINK: slopty_shape::Link = slopty_shape::Link {
        delay: Duration::from_millis(2),
        jitter: Duration::ZERO,
        loss: 0.0,
        rate: 2_500_000,
        queue: 250_000,
    };
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
    const KEYS: usize = 200;

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
    }

    fn held(conn: &Connection) -> usize {
        DATAGRAM_BUFFER.saturating_sub(conn.datagram_send_buffer_space())
    }

    fn frame(conn: &Connection, bytes: usize, seq: u64) -> Vec<Bytes> {
        let size = conn.max_datagram_size().unwrap_or(MAX_DATAGRAM).min(MAX_DATAGRAM);
        let fill = u8::try_from(seq % 251).unwrap();
        (0..bytes.div_ceil(size))
            .map(|i| {
                Bytes::from(vec![fill; size.min(bytes.saturating_sub(i.saturating_mul(size)))])
            })
            .collect()
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
    async fn flood(conn: Connection, video: Video, interactive: Interactive, sent: Arc<AtomicU64>) {
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
            let datagrams = frame(&conn, if key { KEYFRAME } else { P_FRAME }, seq);
            sent.fetch_add(1, Ordering::Relaxed);
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
        cwnd: Arc<Mutex<Vec<f64>>>,
    ) {
        let mut client = listener.accept().await.unwrap();
        let ack = HelloAck {
            protocol: PROTOCOL_VERSION,
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
            tokio::spawn(flood(client.conn.clone(), video, interactive.clone(), frames));
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
                    if let Some((_rtt, window)) = slopty_net::endpoint::path_rtt_cwnd(&conn) {
                        cwnd.lock()
                            .push(f64::from(u32::try_from(window).unwrap_or(u32::MAX)) / 1e3);
                    }
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

    struct Run {
        echo: (f64, f64, f64),
        /// Kilobytes of datagrams QUIC held when each echo was written: what it waited behind.
        ahead: (f64, f64, f64),
        /// The congestion window when each echo was written, kilobytes.
        cwnd: (f64, f64, f64),
        frames: u64,
        datagrams: u64,
        overflowed: u64,
    }

    /// Key → echo on a fresh connection through a fresh shaper, after a second of video for
    /// the congestion controller to find the link.
    async fn run(video: Video, link: slopty_shape::Link) -> Run {
        let listener =
            WorkerListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)), Admission::default())
                .unwrap();
        let worker_addr = listener.local_addr().unwrap();
        let frames = Arc::new(AtomicU64::new(0));
        let ahead = Arc::new(Mutex::new(Vec::new()));
        let cwnd = Arc::new(Mutex::new(Vec::new()));
        let server = tokio::spawn(serve(
            listener,
            video,
            Arc::clone(&frames),
            Arc::clone(&ahead),
            Arc::clone(&cwnd),
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
            protocol: PROTOCOL_VERSION,
            client: ClientId::new(),
            kind: ClientKind::Tool,
            name: "echo".to_owned(),
            app_version: "0".to_owned(),
            caps: Caps::empty(),
        };
        let mut worker = connect_addr(&endpoint, relay.addr().unwrap(), hello).await.unwrap();
        let Uni::Session { session, rx: mut echoes } =
            streams::accept_uni(&worker.conn).await.unwrap()
        else {
            panic!("a session stream");
        };
        let datagrams = Arc::new(AtomicU64::new(0));
        let reader = {
            let (conn, datagrams) = (worker.conn.clone(), Arc::clone(&datagrams));
            tokio::spawn(async move {
                while conn.read_datagram().await.is_ok() {
                    datagrams.fetch_add(1, Ordering::Relaxed);
                }
            })
        };
        tokio::time::sleep(Duration::from_secs(1)).await;

        let mut took = Vec::with_capacity(KEYS);
        for i in 0..KEYS {
            let key = b'a'.saturating_add(u8::try_from(i % 26).unwrap());
            let sent = Instant::now();
            let req = TermRequest::Raw(vec![key]);
            worker.tx.send(&ClientMsg::Term { session, req }).await.unwrap();
            loop {
                let event = tokio::time::timeout(Duration::from_secs(5), echoes.recv())
                    .await
                    .expect("the echo within 5 s")
                    .unwrap();
                if matches!(&event, TermEvent::Title(t) if t.as_bytes().contains(&key)) {
                    break;
                }
            }
            took.push(sent.elapsed().as_secs_f64() * 1e3);
            // Keys land anywhere in the frame period, not in step with it.
            let gap = 20_u64.saturating_add(u64::try_from(i).unwrap().saturating_mul(7) % 23);
            tokio::time::sleep(Duration::from_millis(gap)).await;
        }
        worker.close();
        endpoint.close(0_u32.into(), b"done");
        let _idle = tokio::time::timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
        let carried = relay.carried().await;
        reader.abort();
        relayed.abort();
        server.abort();
        Run {
            echo: quantiles(&mut took),
            ahead: quantiles(&mut ahead.lock()),
            cwnd: quantiles(&mut cwnd.lock()),
            frames: frames.load(Ordering::Relaxed),
            datagrams: datagrams.load(Ordering::Relaxed),
            overflowed: carried.down.overflowed,
        }
    }

    /// The measurement behind "nothing on a worker connection waits behind anything slow"
    /// (MEASUREMENTS.md). Run it in release:
    /// `cargo nextest run -p slopty-net --release --test echo_beside_flood --run-ignored only
    /// --no-capture`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "a measurement: about 25 s over a shaped link"]
    async fn echo_beside_a_video_flood() {
        let clear = run(Video::Off, slopty_shape::Link::CLEAR).await.echo;
        eprintln!(
            "MEASURE clear link, no video: echo p50 {:.2} / p99 {:.2} / max {:.2} ms",
            clear.0, clear.1, clear.2
        );
        let mut rows = Vec::new();
        for video in [Video::Off, Video::Bursts, Video::Laned, Video::Metered] {
            let run = run(video, LINK).await;
            let (p50, p99, max) = run.echo;
            let (a50, a99, amax) = run.ahead;
            eprintln!(
                "MEASURE {video:?}: echo p50 {p50:.2} / p99 {p99:.2} / max {max:.2} ms; \
                 datagrams ahead of it p50 {a50:.1} / p99 {a99:.1} / max {amax:.1} kB; \
                 cwnd p50 {:.0} kB; {} frames, {} datagrams arrived, {} packets overflowed",
                run.cwnd.0, run.frames, run.datagrams, run.overflowed
            );
            rows.push((video, run));
        }
        let ahead = |v: Video| rows.iter().find(|(video, _)| *video == v).map(|(_, r)| r.ahead);
        let (Some(bursts), Some(laned)) = (ahead(Video::Bursts), ahead(Video::Laned)) else {
            panic!("every run measured");
        };
        // Load-independent, unlike the round trip on a shared machine: what QUIC held in front
        // of each echo. A slice and one datagram past it at most while someone types.
        assert!(laned.1 <= 14.0, "the lane holds QUIC to its slice: {laned:?}");
        assert!(bursts.1 > laned.1, "bursts put more in front of an echo: {bursts:?}");
    }
}
