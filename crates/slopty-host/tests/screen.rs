//! Screen pipeline on real hardware: list, open the first display, count datagrams, close.
//! Needs Screen Recording permission, so it runs only with `SLOPTY_SCREEN_E2E=1`.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slopty_host::screen::{DATAGRAM_QUEUE, DatagramBudget, ScreenStream, listing};
    use slopty_proto::media::{Kind, MediaHeader, flags};
    use slopty_proto::screen::{CaptureTarget, Quality, ScreenEvent};
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn display_stream_produces_datagrams() {
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("SLOPTY_SCREEN_E2E unset; skipping");
            return;
        }
        let ScreenEvent::Listing { windows, displays } = listing().await.unwrap() else {
            panic!("listing")
        };
        eprintln!("{} windows, {} displays", windows.len(), displays.len());
        let display = displays.first().expect("a display");

        let (tx, mut rx) = mpsc::channel(DATAGRAM_QUEUE);
        let quality = Quality { fps: 60, bitrate_bps: 8_000_000, scale: 0.5, ..Quality::default() };
        let (mut stream, opened) = ScreenStream::open(
            slopty_core::StreamId(7),
            CaptureTarget::Display(display.id),
            quality,
            tx,
            DatagramBudget::new(),
            |e| panic!("capture stopped: {e}"),
        )
        .await
        .unwrap();
        eprintln!("{opened:?}");

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let (mut data, mut parity, mut cursor, mut keyframes) = (0_u32, 0_u32, 0_u32, 0_u32);
        let mut frames = std::collections::BTreeSet::new();
        while let Ok(Some(datagram)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            let (header, _payload) = MediaHeader::parse(&datagram).expect("well-formed");
            match Kind::from_u8(header.kind) {
                Some(Kind::VideoData) => {
                    data += 1;
                    frames.insert(header.frame.get());
                    if header.index.get() == 0 && header.flags & flags::KEYFRAME != 0 {
                        keyframes += 1;
                    }
                }
                Some(Kind::VideoParity) => parity += 1,
                Some(Kind::Cursor) => cursor += 1,
                _other => {}
            }
        }
        let stats = stream.stats();
        eprintln!(
            "frames {} data {data} parity {parity} cursor {cursor} keyframes {keyframes} {stats:?}",
            frames.len()
        );
        assert!(keyframes >= 1, "first frame is an IDR");
        assert!(frames.len() >= 10, "steady stream, got {} frames", frames.len());
        assert_eq!(stats.encoded, frames.len() as u64);

        // Bitrate-only change applies in place; a scale change rebuilds capture and encoder.
        stream.set_quality(&Quality { bitrate_bps: 4_000_000, ..quality }).unwrap();
        stream.set_quality(&Quality { scale: 0.25, ..quality }).unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        let mut after = 0_u32;
        while let Ok(Some(_datagram)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            after += 1;
        }
        assert!(after > 0, "stream keeps flowing after a quality change");
        stream.close().await;
    }
}

/// Encoder rate control on real content, gated by `SLOPTY_SCREEN_E2E=1`: the low-latency
/// mode the host runs (`EnableLowLatencyRateControl` + `AverageBitRate` + `DataRateLimits`)
/// against macOS 26's `VariableBitRate` + VBV keys, on a Ghostty window this test launches
/// running `yes` (scrolling) and one running `sleep` (static). Same window size, same
/// bitrate; prints encode latency, keyframe size and frame-size spikes. Numbers go to
/// MEASUREMENTS.md, the ruling to DECISIONS.md.
#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "measurement arithmetic on small counts and byte sizes"
)]
mod encoder_rate_control {
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use objc2_core_foundation::CFType;
    use slopty_capture::{Capture, CaptureConfig, PixelFormat, Target, host_now_us};
    use slopty_codec::{Encoder, EncoderConfig, FrameOptions, RateControl};
    use slopty_core::WindowId;
    use slopty_host::screen::{Quantiles, shareable};
    use slopty_proto::screen::{CaptureTarget, VideoCodec};

    const GHOSTTY: &str = "/Applications/Ghostty.app/Contents/MacOS/ghostty";

    struct Window(Child);

    impl Drop for Window {
        fn drop(&mut self) {
            let _killed = self.0.kill();
            let _reaped = self.0.wait();
        }
    }

    async fn launch(command: &[&str]) -> (Window, WindowId) {
        let child = Command::new(GHOSTTY)
            .arg("-e")
            .args(command)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("Ghostty.app installed");
        let pid = i32::try_from(child.id()).expect("pid");
        let window = Window(child);
        let started = Instant::now();
        loop {
            // Bypass the 2 s enumeration cache: the window is new.
            tokio::time::sleep(Duration::from_millis(400)).await;
            let content = shareable().await.expect("content");
            let found = content
                .windows()
                .into_iter()
                .find(|w| w.on_screen && slopty_capture::window_owner_pid(w.id) == Some(pid));
            if let Some(w) = found {
                eprintln!("window {} {}×{} {} — {}", w.id.0, w.w, w.h, w.app, w.title);
                tokio::time::sleep(Duration::from_millis(1500)).await;
                return (window, w.id);
            }
            assert!(started.elapsed() < Duration::from_secs(20), "no Ghostty window for pid {pid}");
        }
    }

    /// One encoder variant: the rate-control mode plus public properties layered on top.
    #[derive(Clone, Copy)]
    struct Variant {
        name: &'static str,
        mode: RateControl,
        codec: VideoCodec,
        /// `(key, value, name)` set after the session is configured.
        extra: fn(&Encoder),
    }

    fn nothing(_e: &Encoder) {}

    /// `MaximizePowerEfficiency = false` (both encoders list it).
    fn no_power_saving(e: &Encoder) {
        // SAFETY: framework-provided constant string.
        let key = unsafe { objc2_video_toolbox::kVTCompressionPropertyKey_MaximizePowerEfficiency };
        let value: &CFType = objc2_core_foundation::CFBoolean::new(false);
        eprintln!(
            "MaximizePowerEfficiency=false: {:?}",
            e.set_property(key, value, "MaximizePowerEfficiency")
        );
    }

    /// `ExpectedFrameRate = 120`: tell the rate controller frames come twice as fast.
    fn expect_120(e: &Encoder) {
        // SAFETY: framework-provided constant string.
        let key = unsafe { objc2_video_toolbox::kVTCompressionPropertyKey_ExpectedFrameRate };
        let value = objc2_core_foundation::CFNumber::new_f64(120.0);
        eprintln!("ExpectedFrameRate=120: {:?}", e.set_property(key, &value, "ExpectedFrameRate"));
    }

    const VARIANTS: [Variant; 5] = [
        Variant {
            name: "LowLatency",
            mode: RateControl::LowLatency,
            codec: VideoCodec::Hevc,
            extra: nothing,
        },
        Variant { name: "Vbv", mode: RateControl::Vbv, codec: VideoCodec::Hevc, extra: nothing },
        Variant {
            name: "LL+noPowerSave",
            mode: RateControl::LowLatency,
            codec: VideoCodec::Hevc,
            extra: no_power_saving,
        },
        Variant {
            name: "LL+expect120",
            mode: RateControl::LowLatency,
            codec: VideoCodec::Hevc,
            extra: expect_120,
        },
        Variant {
            name: "LL H264",
            mode: RateControl::LowLatency,
            codec: VideoCodec::H264,
            extra: nothing,
        },
    ];

    struct Outcome {
        frames: usize,
        encode: Quantiles,
        keyframe_bytes: usize,
        p50_bytes: u64,
        max_bytes: u64,
        kbit_s: f64,
        ltr: bool,
    }

    /// Capture `id` for `seconds` and push every frame through an encoder in `mode`.
    async fn run(id: WindowId, variant: Variant, seconds: u64) -> Outcome {
        let content = shareable().await.expect("content");
        let target = Target::resolve(&content, CaptureTarget::Window(id)).expect("target");
        let (w, h) = target.pixel_size();
        let (ptx, prx) = mpsc::channel::<(u64, u64, usize, bool)>();
        let encoder = Encoder::new(
            EncoderConfig {
                width: w,
                height: h,
                codec: variant.codec,
                fps: 60,
                bitrate_bps: 8_000_000,
                rate_control: variant.mode,
            },
            move |packet| {
                let _gone =
                    ptx.send((packet.pts_us, host_now_us(), packet.data.len(), packet.keyframe));
            },
        )
        .expect("encoder");
        (variant.extra)(&encoder);
        let ltr = encoder.ltr_enabled();
        let encoder = std::sync::Arc::new(encoder);
        let submits = std::sync::Arc::new(parking_lot::Mutex::new(std::collections::VecDeque::<(
            u64,
            u64,
        )>::new()));
        let first = std::sync::atomic::AtomicBool::new(true);
        let (sink_encoder, sink_submits) =
            (std::sync::Arc::clone(&encoder), std::sync::Arc::clone(&submits));
        let (started_tx, started_rx) = mpsc::channel();
        let config = CaptureConfig {
            width: w,
            height: h,
            fps: 60,
            format: PixelFormat::Nv12,
            queue_depth: 2,
            audio: false,
            crop: None,
        };
        let capture = Capture::start(
            &target,
            &config,
            move |frame| {
                let options = FrameOptions {
                    force_keyframe: first.swap(false, std::sync::atomic::Ordering::Relaxed),
                    ..FrameOptions::default()
                };
                sink_submits.lock().push_back((frame.capture_ts_us, host_now_us()));
                if sink_submits.lock().len() > 32 {
                    sink_submits.lock().pop_front();
                }
                let _encoded =
                    sink_encoder.encode(frame.image.as_cv(), frame.capture_ts_us, &options);
            },
            None,
            |e| panic!("capture stopped: {e}"),
            move |r| {
                let _gone = started_tx.send(r);
            },
        )
        .expect("capture");
        started_rx.recv_timeout(Duration::from_secs(10)).expect("start").expect("started");
        let deadline = Instant::now() + Duration::from_secs(seconds);
        let (mut latencies, mut sizes) = (Vec::new(), Vec::new());
        let mut keyframe_bytes = 0;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            let Ok((pts, at, bytes, keyframe)) = prx.recv_timeout(remaining) else { break };
            let submitted = {
                let mut s = submits.lock();
                s.iter().position(|&(p, _)| p == pts).and_then(|i| s.remove(i)).map(|(_, t)| t)
            };
            if let Some(t) = submitted {
                latencies.push(at.saturating_sub(t));
            }
            if keyframe && keyframe_bytes == 0 {
                keyframe_bytes = bytes;
            } else {
                sizes.push(bytes as u64);
            }
        }
        let (tx, rx) = mpsc::channel();
        capture.stop(move |r| {
            let _gone = tx.send(r);
        });
        let _stopped = rx.recv_timeout(Duration::from_secs(5));
        let total: u64 = sizes.iter().sum::<u64>() + keyframe_bytes as u64;
        sizes.sort_unstable();
        Outcome {
            frames: latencies.len(),
            encode: Quantiles::of(&latencies),
            keyframe_bytes,
            p50_bytes: sizes.get(sizes.len() / 2).copied().unwrap_or(0),
            max_bytes: sizes.last().copied().unwrap_or(0),
            kbit_s: total as f64 * 8.0 / 1e3 / seconds as f64,
            ltr,
        }
    }

    fn row(mode: &str, content: &str, o: &Outcome) {
        eprintln!(
            "| {mode} | {content} | {} | {} | {} B | {} B | {} B ({:.1}×) | {:.0} kbit/s | ltr {} |",
            o.frames,
            o.encode.describe(),
            o.keyframe_bytes,
            o.p50_bytes,
            o.max_bytes,
            if o.p50_bytes > 0 { o.max_bytes as f64 / o.p50_bytes as f64 } else { 0.0 },
            o.kbit_s,
            o.ltr
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn low_latency_versus_vbv_on_a_terminal_window() {
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("SLOPTY_SCREEN_E2E unset; skipping");
            return;
        }
        let seconds: u64 =
            std::env::var("SLOPTY_E2E_SECONDS").ok().and_then(|v| v.parse().ok()).unwrap_or(5);
        // What each mode's session says about the optional keys.
        for mode in [RateControl::LowLatency, RateControl::Vbv] {
            let probe = Encoder::new(
                EncoderConfig {
                    width: 900,
                    height: 500,
                    codec: VideoCodec::Hevc,
                    fps: 60,
                    bitrate_bps: 8_000_000,
                    rate_control: mode,
                },
                |_packet| {},
            );
            match probe {
                Ok(encoder) => {
                    eprintln!(
                        "{mode:?}: ltr {} quality keys {:?}",
                        encoder.ltr_enabled(),
                        encoder.probe_quality_keys()
                    );
                    eprintln!("{mode:?}: supported {}", encoder.supported_properties().join(" "));
                }
                Err(e) => eprintln!("{mode:?}: session failed: {e}"),
            }
        }
        eprintln!(
            "| mode | content | frames | encode p50 / p95 / max | keyframe | p50 frame | max frame (spike) | rate | ltr |"
        );
        eprintln!("| --- | --- | --- | --- | --- | --- | --- | --- | --- |");
        let (scroller, id) = launch(&["yes"]).await;
        for variant in VARIANTS {
            let o = run(id, variant, seconds).await;
            row(variant.name, "scrolling", &o);
        }
        drop(scroller);
        // `top` redraws every second: a mostly static terminal with real text changes.
        let (top, id) = launch(&["top", "-s", "1"]).await;
        for variant in VARIANTS {
            let o = run(id, variant, seconds).await;
            row(variant.name, "top 1 Hz", &o);
        }
        drop(top);
    }
}
