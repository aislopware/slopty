//! What an echo waits for noq's pacer while video is being paced out.
//!
//! The pacer holds a packet until its bytes are earned at the pacing rate and sets a timer for
//! the shortfall. That timer is tokio's, which rounds up to its millisecond wheel, and the
//! kernel coalesces it by the thread's tier on top (MEASUREMENTS.md, "timers fire late by the
//! thread's latency tier"). A packet the rate earns in half a millisecond can therefore wait
//! two. An echo written while a frame's datagrams are paced out goes in the next packet (streams
//! go ahead of datagrams), so that timer is what it waits for, unless its stream's priority
//! is `noq::TransportConfig::stream_priority_unpaced` (`vendor/noq-proto/SLOPTY.md`, patch 8).
//!
//! Here one connection over loopback has a controller that paces at a fixed rate and never
//! binds its window, so the pacer alone decides when a packet leaves and nothing queues past
//! it. The echo's stream sits at [`streams::ECHO_PRIORITY`], as a lifted session stream does.

#[cfg(test)]
mod tests {
    use std::any::Any;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use bytes::Bytes;
    use noq::congestion::{Controller, ControllerFactory, ControllerMetrics};
    use slopty_net::{crypto, endpoint, streams};

    const MTU: u64 = 1252;
    const FRAME_PERIOD: Duration = Duration::from_micros(16_667);
    const P_FRAME: usize = 25_000;
    const KEYFRAME: usize = 130_000;
    const FRAMES_PER_KEYFRAME: u64 = 60;
    const ECHO_BYTES: usize = 240;
    const WARMUP_FRAMES: u64 = 30;
    const KEYS: u64 = 400;

    /// Paces at `rate` bytes a second with a window that never binds.
    #[derive(Clone, Debug)]
    struct FixedRate {
        rate: u64,
    }

    impl Controller for FixedRate {
        fn on_congestion_event(
            &mut self,
            _now: Instant,
            _sent: Instant,
            _is_persistent_congestion: bool,
            _is_ecn: bool,
            _lost_bytes: u64,
            _largest_lost: u64,
        ) {
        }

        fn on_mtu_update(&mut self, _new_mtu: u16) {}

        fn window(&self) -> u64 {
            64 << 20
        }

        fn metrics(&self) -> ControllerMetrics {
            let mut metrics = ControllerMetrics::default();
            metrics.congestion_window = self.window();
            metrics.pacing_rate = Some(self.rate);
            // BBR's `C.send_quantum`: a millisecond of the rate, two packets to 64 KiB.
            metrics.send_quantum = Some((self.rate / 1000).clamp(2 * MTU, 64 << 10));
            metrics
        }

        fn clone_box(&self) -> Box<dyn Controller> {
            Box::new(self.clone())
        }

        fn initial_window(&self) -> u64 {
            self.window()
        }

        fn into_any(self: Box<Self>) -> Box<dyn Any> {
            self
        }
    }

    impl ControllerFactory for FixedRate {
        fn build(self: Arc<Self>, _now: Instant, _current_mtu: u16) -> Box<dyn Controller> {
            Box::new((*self).clone())
        }
    }

    /// Slopty's transport at a fixed pacing `rate`; `paced` puts the echo back behind the pacer.
    fn bind(rate: u64, paced: bool, server: bool) -> noq::Endpoint {
        let mut transport = endpoint::transport_config();
        transport.congestion_controller_factory(Arc::new(FixedRate { rate }));
        if paced {
            transport.stream_priority_unpaced(None);
        }
        let transport = Arc::new(transport);
        let server_config = server.then(|| {
            let mut config = crypto::server_config();
            config.transport_config(Arc::clone(&transport));
            config
        });
        let socket = std::net::UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        let endpoint = noq::Endpoint::new(
            crypto::endpoint_config(),
            server_config,
            socket,
            Arc::new(noq::TokioRuntime),
        )
        .unwrap();
        let mut client = crypto::client_config();
        client.transport_config(transport);
        endpoint.set_default_client_config(client);
        endpoint
    }

    /// A sender and a receiver on one connection, and the echo stream between them, opened.
    struct Link {
        sender: noq::Connection,
        echo: noq::SendStream,
        reader: tokio::task::JoinHandle<Vec<Instant>>,
        drain: tokio::task::JoinHandle<()>,
    }

    async fn link(rate: u64, paced: bool) -> Link {
        let server = bind(rate, paced, true);
        let client = bind(rate, paced, false);
        let at = server.local_addr().unwrap();
        let accepted = tokio::spawn(async move { server.accept().await.unwrap().await.unwrap() });
        let receiver = client.connect(at, "127.0.0.1").unwrap().await.unwrap();
        let sender = accepted.await.unwrap();

        let mut echo = sender.open_uni().await.unwrap();
        echo.set_priority(streams::ECHO_PRIORITY).unwrap();
        echo.write_all(&[0; ECHO_BYTES]).await.unwrap();
        let mut incoming = receiver.accept_uni().await.unwrap();
        let mut buf = [0; ECHO_BYTES];
        incoming.read_exact(&mut buf).await.unwrap();

        let drain = {
            let receiver = receiver.clone();
            tokio::spawn(async move { while receiver.read_datagram().await.is_ok() {} })
        };
        let reader = tokio::spawn(async move {
            let _client = client;
            let mut arrived = Vec::new();
            let mut buf = [0; ECHO_BYTES];
            while incoming.read_exact(&mut buf).await.is_ok() {
                arrived.push(Instant::now());
            }
            arrived
        });
        Link { sender, echo, reader, drain }
    }

    fn send_frame(sender: &noq::Connection, size: usize) {
        let chunk = sender.max_datagram_size().unwrap();
        let data = Bytes::from(vec![0_u8; size]);
        let mut at = 0;
        while at < size {
            let end = at.saturating_add(chunk).min(size);
            sender.send_datagram(data.slice(at..end)).unwrap();
            at = end;
        }
    }

    /// Each echo's one-way time in ms, sorted.
    async fn finish(link: Link, written: &[Instant]) -> Vec<f64> {
        let Link { sender, mut echo, reader, drain } = link;
        echo.finish().unwrap();
        let arrived = reader.await.unwrap();
        drain.abort();
        sender.close(0_u32.into(), b"done");
        assert_eq!(arrived.len(), written.len(), "every echo arrives");
        let mut ms: Vec<f64> = written
            .iter()
            .zip(&arrived)
            .map(|(w, a)| a.duration_since(*w).as_secs_f64() * 1e3)
            .collect();
        ms.sort_by(f64::total_cmp);
        ms
    }

    /// A small xorshift, so each run's phases are the same.
    fn next(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    fn pct(sorted: &[f64], percent: usize) -> f64 {
        sorted[sorted.len().saturating_sub(1).saturating_mul(percent) / 100]
    }

    /// Video the way the encoder hands frames over (a burst a frame at 60 fps, a keyframe a
    /// second), and an echo at a random phase of each frame.
    async fn beside_video(rate: u64, paced: bool) -> Vec<f64> {
        let mut link = link(rate, paced).await;
        let mut written = Vec::new();
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let start = tokio::time::Instant::now();
        for frame in 0..WARMUP_FRAMES + KEYS {
            let due = start
                .checked_add(FRAME_PERIOD.saturating_mul(u32::try_from(frame).unwrap()))
                .unwrap();
            tokio::time::sleep_until(due).await;
            let size = if frame % FRAMES_PER_KEYFRAME == 0 { KEYFRAME } else { P_FRAME };
            send_frame(&link.sender, size);
            if frame < WARMUP_FRAMES {
                continue;
            }
            let phase = Duration::from_micros(next(&mut seed) % 16_667);
            tokio::time::sleep_until(due.checked_add(phase).unwrap()).await;
            written.push(Instant::now());
            link.echo.write_all(&[0; ECHO_BYTES]).await.unwrap();
        }
        finish(link, &written).await
    }

    /// With a backlog of datagrams at 50 kB/s the pacer lets a packet out every 25 ms, so a paced
    /// echo waits half that at the median. An echo on the unpaced priority goes at once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_echo_does_not_wait_for_the_pacer() {
        const RATE: u64 = 50_000;
        const ECHOES: usize = 20;
        let mut link = link(RATE, false).await;
        send_frame(&link.sender, 200_000);
        let mut written = Vec::new();
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        for _ in 0..ECHOES {
            let gap = Duration::from_micros(20_000 + next(&mut seed) % 10_000);
            tokio::time::sleep(gap).await;
            written.push(Instant::now());
            link.echo.write_all(&[0; ECHO_BYTES]).await.unwrap();
        }
        let ms = finish(link, &written).await;
        let median = pct(&ms, 50);
        assert!(median < 5.0, "an echo waited {median:.2} ms at the median behind the pacer");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "measurement: prints an echo's wait behind paced video"]
    async fn an_echo_behind_paced_video() {
        let rates = [("20 Mbit/s", 2_500_000_u64), ("100 Mbit/s", 12_500_000)];
        for round in 1..=3 {
            for (name, rate) in rates {
                for (arm, paced) in [("paced", true), ("unpaced", false)] {
                    let ms = beside_video(rate, paced).await;
                    println!(
                        "MEASURE pacer round {round} {name} {arm}: echo one-way p50 {:.2} p90 \
                         {:.2} p99 {:.2} max {:.2} ms (a packet at the rate: {:.2} ms)",
                        pct(&ms, 50),
                        pct(&ms, 90),
                        pct(&ms, 99),
                        ms[ms.len() - 1],
                        Duration::from_nanos(
                            MTU.saturating_mul(1_000_000_000).checked_div(rate).unwrap()
                        )
                        .as_secs_f64()
                            * 1e3,
                    );
                }
            }
        }
    }
}
