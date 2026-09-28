//! A worker platform whose pictures are drawn, not captured: [`Drawn`].
//!
//! Its capture is [`slopty_capture::synthetic::Canvas`], which stands in for the displays and
//! draws a picture on each one's beat. Its encoders are the real ones, and so is everything
//! after them, so a [`Pipeline<Drawn>`](super::Pipeline) runs the same encode, packetize and
//! send path as a real capture. Its input sink ([`Poke`]) stands in for the application a click
//! lands in: every event it takes changes the next picture
//! ([`slopty_capture::synthetic::take_input`]).
//!
//! It exists to time the frame path end to end where ScreenCaptureKit cannot run. Nothing in
//! the product builds a stream on it; the measurement in its tests does
//! (`docs/MEASUREMENTS.md`, "Capture to the glass").

use std::time::Instant;

use slopty_capture::Rect;
use slopty_input::{InputError, InputSink, PointerWatch};
use slopty_proto::screen::{CaptureTarget, ScreenInput};

use crate::platform::Platform;

/// Drawn pictures, the real encoders, and input that changes the picture.
#[derive(Clone, Copy, Debug)]
pub enum Drawn {}

impl Platform for Drawn {
    type Audio = slopty_codec::Opus;
    type Capture = slopty_capture::synthetic::Canvas;
    type Input = Poke;
    type Video = slopty_codec::VideoToolbox;
}

/// Input that changes the canvas: each event is one more input its strip counts.
#[derive(Debug, Default)]
pub struct Poke {
    pointer: PointerWatch,
}

impl InputSink for Poke {
    fn new(_target: CaptureTarget, _scale: f64) -> Self {
        Self::default()
    }

    fn set_scale(&mut self, _scale: f64) {}

    fn set_bounds(&mut self, _bounds: Option<Rect>, _at: Instant) {}

    fn inject(&mut self, _input: &ScreenInput) -> Result<(), InputError> {
        slopty_capture::synthetic::take_input();
        Ok(())
    }

    fn focus(&mut self) -> Result<(), InputError> {
        Ok(())
    }

    fn release_all(&mut self) {}

    fn pointer(&self) -> PointerWatch {
        self.pointer.clone()
    }
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "measurement arithmetic on small counts, printed"
)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::time::Duration;

    use bytes::Bytes;
    use slopty_capture::synthetic::{inputs_shown, inputs_taken};
    use slopty_client::pacing::{ClockAnchor, Pacer, Spread, percentile};
    use slopty_client::screen::{ScreenRouter, Uplink, spawn_screen};
    use slopty_core::StreamId;
    use slopty_proto::ClientMsg;
    use slopty_proto::datagram::ClientDatagram;
    use slopty_proto::input::{Mods, MouseButton};
    use slopty_proto::media::MAX_DATAGRAM;
    use slopty_proto::screen::{Feedback, Quality, ScreenEvent, ScreenRequest};
    use tokio::sync::mpsc;

    use super::*;
    use crate::screen::{DatagramSink, Pipeline, Refused, StreamControl};

    const STREAM: StreamId = StreamId(1);

    /// The worker's capture clock and this process's, read together. Both are mach absolute
    /// time, so on one machine this is exact to the width of the two reads.
    fn anchor() -> ClockAnchor {
        let before = slopty_capture::host_now_us();
        let at = Instant::now();
        let after = slopty_capture::host_now_us();
        ClockAnchor { at, host_us: before + (after - before) / 2 }
    }

    /// One direction of the in-process link: items handed to the far end in order, `delay`
    /// after they were sent.
    fn delay_line<T: Send + 'static>(
        delay: Duration,
        deliver: impl Fn(T, Instant) + Send + 'static,
    ) -> mpsc::UnboundedSender<(Instant, T)> {
        let (tx, mut rx) = mpsc::unbounded_channel::<(Instant, T)>();
        tokio::spawn(async move {
            while let Some((sent, item)) = rx.recv().await {
                tokio::time::sleep_until((sent + delay).into()).await;
                deliver(item, Instant::now());
            }
        });
        tx
    }

    /// The worker's end of the link: datagrams go straight into the client's router on
    /// loopback, or down a delay line.
    struct Wire {
        router: ScreenRouter,
        line: Option<mpsc::UnboundedSender<(Instant, Vec<Bytes>)>>,
    }

    impl DatagramSink for Wire {
        fn send(&self, datagrams: &[Bytes]) -> Result<(), Refused> {
            let now = Instant::now();
            match &self.line {
                None => self.router.route_many(datagrams.iter().cloned(), now),
                Some(line) => {
                    line.send((now, datagrams.to_vec())).map_err(|_gone| Refused::Closed)?;
                }
            }
            Ok(())
        }

        fn max_size(&self) -> Option<usize> {
            Some(MAX_DATAGRAM)
        }

        fn held(&self) -> usize {
            0
        }

        fn cwnd(&self) -> u64 {
            0
        }

        fn is_closed(&self) -> bool {
            false
        }
    }

    /// The client's loss feedback, as the worker's connection answers it.
    fn answer(control: &StreamControl<Drawn>, bytes: &[u8]) {
        match ClientDatagram::decode(bytes) {
            Some(ClientDatagram::Feedback(Feedback::Nack { frame, fragments, .. })) => {
                control.nack(frame, &fragments);
            }
            Some(ClientDatagram::Feedback(Feedback::Refresh {
                last_good_frame, keyframe, ..
            })) => {
                control.request_refresh(last_good_frame, keyframe);
            }
            _other => {}
        }
    }

    fn ms(d: Duration) -> f64 {
        d.as_secs_f64() * 1e3
    }

    fn spread(samples: &mut [Duration]) -> String {
        samples.sort_unstable();
        format!(
            "p50 {:.2} / p95 {:.2} / max {:.2} ms (n={})",
            ms(percentile(samples, 50)),
            ms(percentile(samples, 95)),
            ms(samples.last().copied().unwrap_or_default()),
            samples.len()
        )
    }

    fn ring(s: &Spread) -> String {
        format!(
            "p50 {:.2} / p95 {:.2} / max {:.2} ms (n={})",
            ms(s.p50),
            ms(s.p95),
            ms(s.max),
            s.count
        )
    }

    /// One run: the canvas streamed through the real encoder and packetizer into the client's
    /// reassembler and decoder, `one_way` each way with `loss_permille` of the datagrams lost,
    /// a click every 80–150 ms, and a paint on the display's beat.
    #[expect(clippy::too_many_lines, reason = "one measurement, read top to bottom")]
    async fn run(label: &str, one_way: Duration, loss_permille: u32, seconds: u64) {
        let ScreenEvent::Listing { displays, .. } = Pipeline::<Drawn>::listing().await.unwrap()
        else {
            panic!("no listing")
        };
        let display = displays.first().expect("a display");
        let hz = f64::from(display.hz).clamp(24.0, 240.0);
        let router = if loss_permille > 0 {
            ScreenRouter::with_loss(loss_permille)
        } else {
            ScreenRouter::new()
        };
        let line = (!one_way.is_zero()).then(|| {
            let router = router.clone();
            delay_line(one_way, move |datagrams: Vec<Bytes>, at| router.route_many(datagrams, at))
        });
        let wire = Arc::new(Wire { router: router.clone(), line });
        let (mut stream, opened) = Pipeline::<Drawn>::open(
            STREAM,
            CaptureTarget::Display(display.id),
            Quality::default(),
            wire,
            |_event| {},
        )
        .await
        .unwrap();
        let ScreenEvent::Opened { codec, width, height, .. } = opened else { panic!("{opened:?}") };

        let control = stream.control();
        let feedback = {
            let control = control.clone();
            delay_line(one_way, move |bytes: Bytes, _at| answer(&control, &bytes))
        };
        let (reports_tx, mut reports) = mpsc::channel::<ClientMsg>(64);
        let decisions = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let reporting = {
            let control = control.clone();
            let decisions = Arc::clone(&decisions);
            let line = delay_line(one_way, move |msg: ClientMsg, _at| {
                if let ClientMsg::Screen(ScreenRequest::Report { report, .. }) = msg
                    && let Some(decision) = control.report(&report, None)
                {
                    decisions.lock().push((decision.verdict, decision.target_bps));
                }
            });
            tokio::spawn(async move {
                while let Some(msg) = reports.recv().await {
                    let _gone = line.send((Instant::now(), msg));
                }
            })
        };
        let rtt = one_way * 2 + Duration::from_micros(200);
        let uplink = Uplink {
            control: reports_tx,
            feedback: Box::new(move |bytes| feedback.send((Instant::now(), bytes)).is_ok()),
            rtt: Box::new(move || Some(rtt)),
        };
        let handle =
            spawn_screen(&tokio::runtime::Handle::current(), &router, STREAM, codec, uplink);
        let mut frames = handle.frames();

        let clocks = anchor();
        let mut pacer = Pacer::default();
        pacer.share_clock(clocks);
        let mut paint = tokio::time::interval(Duration::from_secs_f64(1.0 / hz));
        paint.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        #[expect(clippy::cast_possible_truncation, reason = "the strip's 16 bits")]
        let base = inputs_taken() as u16;
        let warm_up = Duration::from_secs(1);
        let started = tokio::time::Instant::now();
        let deadline = started + warm_up + Duration::from_secs(seconds);
        let mut next_input = started + warm_up;
        let mut seq = 0_u64;
        let mut seen = 0_u64;
        let mut lcg = 0x2545_f491_4f6c_dd1d_u64;
        // Inputs on their way to the worker, and when each reached it.
        let mut in_flight: VecDeque<(tokio::time::Instant, u64)> = VecDeque::new();
        let mut reached: VecDeque<(u64, Instant)> = VecDeque::new();
        // Hop by hop, over the whole run after the warm-up.
        let (mut to_arrival, mut to_decoded, mut decode, mut to_paint, mut to_glass) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut reach_to_capture = Vec::new();
        let click = ScreenInput::Button {
            button: MouseButton::Left,
            down: true,
            x: 10.0,
            y: 10.0,
            clicks: 1,
            mods: Mods::empty(),
        };
        loop {
            let delivery = in_flight.front().map_or(deadline, |(due, _)| *due);
            tokio::select! {
                _tick = paint.tick() => {
                    if let Some(stamp) = pacer.painted() {
                        let at = Instant::now();
                        pacer.shown(stamp, at);
                        if tokio::time::Instant::now() > started + warm_up
                            && let Some(captured) = clocks.captured(stamp.pts_us)
                        {
                            to_paint.push(at.saturating_duration_since(stamp.decoded));
                            to_glass.push(at.saturating_duration_since(captured));
                        }
                    }
                }
                changed = frames.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let Some(frame) = frames.borrow_and_update().clone() else { continue };
                    let stamp = frame.stamp;
                    let _pace = pacer.offer(stamp);
                    if tokio::time::Instant::now() > started + warm_up
                        && let Some(captured) = clocks.captured(stamp.pts_us)
                    {
                        to_arrival.push(stamp.arrived.saturating_duration_since(captured));
                        to_decoded.push(stamp.decoded.saturating_duration_since(captured));
                        decode.push(stamp.decoded.saturating_duration_since(stamp.arrived));
                    }
                    if let Some(shown) = inputs_shown(frame.frame.image.as_cv()) {
                        let count = u64::from(shown.wrapping_sub(base));
                        if count > seen && count <= seq {
                            seen = count;
                            pacer.input_visible(seen, stamp.pts_us);
                            let captured = clocks.captured(stamp.pts_us);
                            while let Some(&(s, at)) = reached.front() && s <= seen {
                                reached.pop_front();
                                if let Some(captured) = captured {
                                    reach_to_capture.push(captured.saturating_duration_since(at));
                                }
                            }
                        }
                    }
                }
                () = tokio::time::sleep_until(next_input) => {
                    seq += 1;
                    pacer.input_sent(seq, Instant::now());
                    in_flight.push_back((tokio::time::Instant::now() + one_way, seq));
                    lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                    next_input += Duration::from_millis(80 + (lcg >> 33) % 71);
                }
                () = tokio::time::sleep_until(delivery), if !in_flight.is_empty() => {
                    if let Some((_, s)) = in_flight.pop_front() {
                        stream.inject(&click).unwrap();
                        reached.push_back((s, Instant::now()));
                    }
                }
                () = tokio::time::sleep_until(deadline) => break,
            }
        }
        let glass = pacer.glass();
        let pacing = pacer.stats();
        let stats = handle.stats();
        let worker = stream.stats();
        eprintln!(
            "MEASURE glass {label}: {width}×{height} {codec:?} at {hz:.0} Hz, one way {:.1} ms, loss {:.1} %",
            ms(one_way),
            f64::from(loss_permille) / 10.0
        );
        eprintln!("  capture → present (pacer ring):  {}", ring(&glass.capture));
        eprintln!(
            "  input sent → present (pacer):    {}  pending {}",
            ring(&glass.input),
            glass.inputs_pending
        );
        eprintln!(
            "  arrival → present (pacer ring):  p50 {:.2} / p95 {:.2} / max {:.2} ms",
            ms(pacing.latency_p50),
            ms(pacing.latency_p95),
            ms(pacing.latency_max)
        );
        eprintln!("  hops, whole run:");
        eprintln!(
            "    worker draw (beat → frame to sink): p50 {:.2} / p95 {:.2} ms",
            worker.capture.p50_us as f64 / 1e3,
            worker.capture.p95_us as f64 / 1e3
        );
        eprintln!(
            "    worker encode (submit → VT callback): p50 {:.2} / p95 {:.2} / max {:.2} ms",
            worker.encode.p50_us as f64 / 1e3,
            worker.encode.p95_us as f64 / 1e3,
            worker.encode.max_us as f64 / 1e3
        );
        eprintln!(
            "    worker capture → packetized: mean {:.2} / max {:.2} ms",
            worker.latency_sum_us as f64 / worker.encoded.max(1) as f64 / 1e3,
            worker.latency_max_us as f64 / 1e3
        );
        eprintln!("    capture → arrival:   {}", spread(&mut to_arrival));
        eprintln!("    arrival → decoded:   {}", spread(&mut decode));
        eprintln!("    capture → decoded:   {}", spread(&mut to_decoded));
        eprintln!("    decoded → painted:   {}", spread(&mut to_paint));
        eprintln!("    capture → painted:   {}", spread(&mut to_glass));
        eprintln!("    input at worker → capture showing it: {}", spread(&mut reach_to_capture));
        eprintln!(
            "  frames: captured {} encoded {} dropped {} | client decoded {} lost {} fec {} nacks {} refreshes {} decode errors {} | shown {} skipped {} repeats {} late {} | inputs sent {seq} seen {seen}",
            worker.captured,
            worker.encoded,
            worker.dropped,
            stats.frames,
            stats.frames_lost,
            stats.frames_fec,
            stats.nacks,
            stats.refreshes,
            stats.decode_errors,
            pacing.presented,
            pacing.skipped,
            pacing.repeats,
            pacing.late
        );
        let decisions = decisions.lock().clone();
        let verdicts: Vec<String> = decisions
            .chunk_by(|a, b| a == b)
            .map(|run| {
                let (verdict, bps) = run[0];
                format!("{:.1}({verdict:?}×{})", f64::from(bps) / 1e6, run.len())
            })
            .collect();
        eprintln!(
            "  rate: {} → encoder at {:.1} Mbit/s, {:.1} frames/s encoded",
            verdicts.join(" "),
            worker.bitrate_bps as f64 / 1e6,
            worker.encoded as f64 / (warm_up.as_secs_f64() + seconds as f64)
        );
        assert!(glass.capture.count > 0, "no frame was timed from its capture");
        assert!(glass.input.count > 0, "no input reached the glass");
        drop(handle);
        reporting.abort();
        stream.close().await;
    }

    /// Capture → glass and input → glass on this Mac, from drawn pictures. Run from a copy of the
    /// test binary off the repository volume (`docs/MEASUREMENTS.md`, "Capture to the glass");
    /// `SLOPTY_GLASS_SECONDS` sets the run length (default 20).
    #[test]
    #[ignore = "measurement"]
    fn capture_and_input_to_glass() {
        slopty_platform::user_interactive_thread();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .on_thread_start(slopty_platform::user_interactive_thread)
            .build()
            .unwrap();
        let seconds: u64 =
            std::env::var("SLOPTY_GLASS_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(20);
        runtime.block_on(async {
            run("loopback", Duration::ZERO, 0, seconds).await;
            run("tailnet-shaped", Duration::from_millis(5), 30, seconds).await;
        });
    }
}
