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
        /// Bytes of every datagram sent: data, parity, retransmits, cursor and heartbeats.
        bytes: std::sync::atomic::AtomicU64,
    }

    impl Wire {
        fn new(
            router: ScreenRouter,
            line: Option<mpsc::UnboundedSender<(Instant, Vec<Bytes>)>>,
        ) -> Self {
            Self { router, line, bytes: std::sync::atomic::AtomicU64::new(0) }
        }

        fn bytes(&self) -> u64 {
            self.bytes.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    impl DatagramSink for Wire {
        fn send(&self, datagrams: &[Bytes]) -> Result<(), Refused> {
            let now = Instant::now();
            let sent: usize = datagrams.iter().map(Bytes::len).sum();
            self.bytes.fetch_add(sent as u64, std::sync::atomic::Ordering::Relaxed);
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
    /// reassembler and decoder at `fps` frames a second at most, `one_way` each way with
    /// `loss_permille` of the datagrams lost, a click every 80–150 ms, and a paint on the
    /// display's beat.
    #[expect(clippy::too_many_lines, reason = "one measurement, read top to bottom")]
    async fn run(label: &str, fps: u16, one_way: Duration, loss_permille: u32, seconds: u64) {
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
        let wire = Arc::new(Wire::new(router.clone(), line));
        let (mut stream, opened) = Pipeline::<Drawn>::open(
            STREAM,
            CaptureTarget::Display(display.id),
            Quality { fps, ..Quality::default() },
            Arc::<Wire>::clone(&wire),
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
        // Bytes and frames at the end of the warm-up, so the rates cover the measured seconds.
        let mut settled: Option<(u64, u64)> = None;
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
                    if settled.is_none() && tokio::time::Instant::now() > started + warm_up {
                        settled = Some((wire.bytes(), stream.stats().encoded));
                    }
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
            "MEASURE glass {label}: {width}×{height} {codec:?} at {hz:.0} Hz, {fps} fps asked, one way {:.1} ms, loss {:.1} %",
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
        let (bytes0, encoded0) = settled.unwrap_or_default();
        let measured = seconds as f64;
        let encoded = worker.encoded.saturating_sub(encoded0) as f64;
        let wire_bytes = wire.bytes().saturating_sub(bytes0) as f64;
        eprintln!(
            "  rate: {} → encoder at {:.1} Mbit/s, {:.1} frames/s encoded, {:.2} Mbit/s on the wire, {:.1} KiB a frame",
            verdicts.join(" "),
            worker.bitrate_bps as f64 / 1e6,
            encoded / measured,
            wire_bytes * 8.0 / measured / 1e6,
            wire_bytes / encoded.max(1.0) / 1024.0,
        );
        assert!(glass.capture.count > 0, "no frame was timed from its capture");
        assert!(glass.input.count > 0, "no input reached the glass");
        drop(handle);
        reporting.abort();
        stream.close().await;
    }

    /// The pixel format of the next picture the client decodes whose format `wanted` accepts,
    /// and whether its input strip read back; `None` when none comes within 30 s.
    async fn next_picture(
        frames: &mut tokio::sync::watch::Receiver<Option<Arc<slopty_client::screen::Presentable>>>,
        wanted: impl Fn(u32) -> bool,
    ) -> Option<(u32, bool)> {
        let wait = async {
            while frames.changed().await.is_ok() {
                let Some(frame) = frames.borrow_and_update().clone() else { continue };
                let image = frame.frame.image.as_cv();
                let format = objc2_core_video::CVPixelBufferGetPixelFormatType(image);
                if wanted(format) {
                    return Some((format, inputs_shown(image).is_some()));
                }
            }
            None
        };
        tokio::time::timeout(Duration::from_secs(30), wait).await.ok().flatten()
    }

    /// A stream that asks for 4:4:4 is drawn as `xf44`, encoded as HEVC 4:4:4 and handed to the
    /// client's surface as the 4:4:4 picture it decoded, the strip still readable; once loss has
    /// cut the rate under the leave line, the geometry tick moves it to a 4:2:0 session and the
    /// client's pictures are NV12 again, and clean windows bring it back to 4:4:4 after the
    /// hold. Drawn, never captured.
    #[test]
    fn a_full_chroma_stream_arrives_as_444_and_follows_the_rate() {
        use objc2_core_video::{
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            kCVPixelFormatType_444YpCbCr10BiPlanarFullRange,
        };
        use slopty_proto::screen::{Chroma, ReceiverReport};
        let runtime =
            tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build();
        runtime.unwrap().block_on(async {
            let ScreenEvent::Listing { displays, .. } = Pipeline::<Drawn>::listing().await.unwrap()
            else {
                panic!("no listing")
            };
            let display = displays.first().expect("a display");
            let router = ScreenRouter::new();
            let wire = Arc::new(Wire::new(router.clone(), None));
            // A quarter of any display up to 6K is smaller than 1080p, whose enter line is
            // under the 12 Mbit/s a stream opens at.
            let quality = Quality { scale: 0.25, chroma: Chroma::Full, ..Quality::default() };
            let (mut stream, opened) = Pipeline::<Drawn>::open(
                STREAM,
                CaptureTarget::Display(display.id),
                quality,
                wire,
                |_event| {},
            )
            .await
            .unwrap();
            let ScreenEvent::Opened { codec, width, height, .. } = opened else {
                panic!("{opened:?}")
            };
            assert_eq!(stream.chroma(), Chroma::Full, "{width}×{height} opens over the line");

            let control = stream.control();
            let answering = control.clone();
            // The client's own reports go unheard: the test drives the rate.
            let (reports_tx, mut reports) = mpsc::channel::<ClientMsg>(64);
            let drain = tokio::spawn(async move { while reports.recv().await.is_some() {} });
            let uplink = Uplink {
                control: reports_tx,
                feedback: Box::new(move |bytes| {
                    answer(&answering, &bytes);
                    true
                }),
                rtt: Box::new(|| Some(Duration::from_millis(1))),
            };
            let handle =
                spawn_screen(&tokio::runtime::Handle::current(), &router, STREAM, codec, uplink);
            let mut frames = handle.frames();

            let full = kCVPixelFormatType_444YpCbCr10BiPlanarFullRange;
            let got = next_picture(&mut frames, |_any| true).await;
            assert_eq!(got, Some((full, true)), "the first picture is 4:4:4 and reads back");

            // Eight decisions of heavy loss, each a cut to 75 %.
            let lossy =
                ReceiverReport { frames_ok: 3, frames_lost: 3, ..ReceiverReport::default() };
            let targets: Vec<u32> = (0..80)
                .filter_map(|_| control.report(&lossy, None))
                .map(|decision| decision.target_bps)
                .collect();
            let probe = tokio::task::spawn_blocking(stream.prober()).await.unwrap();
            let down = Instant::now();
            let mut rebuild =
                stream.check_geometry(&probe).expect("the geometry tick rebuilds for the chroma");
            let encoder = rebuild.built().await.unwrap();
            assert_eq!(stream.finish_rebuild(rebuild, encoder), None, "the size stays");
            assert_eq!(stream.chroma(), Chroma::Subsampled, "after cuts to {targets:?}");

            let nv12 = kCVPixelFormatType_420YpCbCr8BiPlanarFullRange;
            let got = next_picture(&mut frames, |format| format != full).await;
            assert_eq!(got, Some((nv12, true)), "back on 4:2:0");
            let down = down.elapsed();

            // Clean windows grow the rate back over the enter line; once the hold is out the
            // geometry tick moves the stream to a 4:4:4 session again, the capture ahead of it.
            let clean = ReceiverReport { frames_ok: 3, ..ReceiverReport::default() };
            let mut decisions = 0;
            let rebuild = loop {
                assert!(decisions < 60, "never back on 4:4:4");
                decisions += 1;
                let target = (0..10).find_map(|_| control.report(&clean, None));
                let probe = tokio::task::spawn_blocking(stream.prober()).await.unwrap();
                let up = Instant::now();
                if let Some(rebuild) = stream.check_geometry(&probe) {
                    break (rebuild, target, up);
                }
            };
            let (mut rebuild, target, up) = rebuild;
            let encoder = rebuild.built().await.unwrap();
            assert_eq!(stream.finish_rebuild(rebuild, encoder), None);
            assert_eq!(stream.chroma(), Chroma::Full, "after {decisions} decisions, {target:?}");
            let got = next_picture(&mut frames, |format| format == full).await;
            assert_eq!(got, Some((full, true)), "4:4:4 again");
            eprintln!(
                "MEASURE chroma switch at {width}×{height}, rebuild → first decoded picture: \
                 4:4:4 → 4:2:0 {:.1} ms, 4:2:0 → 4:4:4 {:.1} ms",
                ms(down),
                ms(up.elapsed())
            );
            drop(handle);
            drain.abort();
            stream.close().await;
        });
    }

    /// A quality change that moves only the frame rate (the client's view went to a screen of
    /// another refresh) is taken by the encoder in place: no rebuild, the ceiling and the rung
    /// at the new rate, and the stream keeps sending.
    #[test]
    fn a_new_frame_rate_alone_is_taken_in_place() {
        let runtime =
            tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build();
        runtime.unwrap().block_on(async {
            // A known beat: a panel that reports no refresh would have the capture throttled
            // to the rate, and a new rate would be a new capture.
            slopty_capture::synthetic::set_beat(Some(120));
            let ScreenEvent::Listing { displays, .. } = Pipeline::<Drawn>::listing().await.unwrap()
            else {
                panic!("no listing")
            };
            let display = displays.first().expect("a display");
            let router = ScreenRouter::new();
            let wire = Arc::new(Wire::new(router, None));
            let quality = Quality { fps: 60, scale: 0.25, ..Quality::default() };
            let (mut stream, _opened) = Pipeline::<Drawn>::open(
                STREAM,
                CaptureTarget::Display(display.id),
                quality,
                Arc::<Wire>::clone(&wire),
                |_event| {},
            )
            .await
            .unwrap();
            slopty_capture::synthetic::set_beat(None);
            let ceiling = |stream: &Pipeline<Drawn>| {
                stream.shared.fps_ceiling.load(std::sync::atomic::Ordering::Relaxed)
            };
            assert_eq!(ceiling(&stream), 60);
            let rebuild = stream.set_quality(&Quality { fps: 120, ..quality }, None);
            assert!(rebuild.is_none(), "a new rate alone builds nothing");
            assert_eq!(ceiling(&stream), 120);
            assert_eq!(stream.shared.fps.load(std::sync::atomic::Ordering::Relaxed), 120);
            let sent = wire.bytes();
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(wire.bytes() > sent, "the stream keeps flowing");
            stream.close().await;
        });
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
            run("loopback", 60, Duration::ZERO, 0, seconds).await;
            run("tailnet-shaped", 60, Duration::from_millis(5), 30, seconds).await;
        });
    }

    /// The same on a 120 Hz beat, as a 120 Hz Mac or external display gives:
    /// a stream asked for 60 against one asked for 120, on loopback and tailnet-shaped. The
    /// canvas scrolls at the same speed on either beat (`docs/MEASUREMENTS.md`, "120 fps
    /// against 60"). `SLOPTY_GLASS_SECONDS` sets the run length (default 20).
    #[test]
    #[ignore = "measurement"]
    fn capture_and_input_to_glass_at_120() {
        slopty_platform::user_interactive_thread();
        slopty_capture::synthetic::set_beat(Some(120));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .on_thread_start(slopty_platform::user_interactive_thread)
            .build()
            .unwrap();
        let seconds: u64 =
            std::env::var("SLOPTY_GLASS_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(20);
        runtime.block_on(async {
            run("loopback", 60, Duration::ZERO, 0, seconds).await;
            run("loopback", 120, Duration::ZERO, 0, seconds).await;
            run("tailnet-shaped", 60, Duration::from_millis(5), 30, seconds).await;
            run("tailnet-shaped", 120, Duration::from_millis(5), 30, seconds).await;
        });
        slopty_capture::synthetic::set_beat(None);
    }
}
