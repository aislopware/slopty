//! Capture latency against the live window server, gated by `SLOPTY_SCREEN_E2E=1`.
//!
//! The content is a Ghostty window this test launches itself running `yes` (a scrolling
//! terminal, the worst case for a window) and kills afterwards. Latency is the window
//! server's display time of a frame (`SCStreamFrameInfoDisplayTime`) → our callback.
//! Numbers go to MEASUREMENTS.md; the guard test pins the ruled floor.

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "measurement arithmetic on small counts and microseconds"
)]
mod tests {
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// Block this test thread: it waits for another process's window or for a stream to settle.
    #[expect(clippy::disallowed_methods, reason = "a test thread, not library code")]
    fn pause(d: Duration) {
        std::thread::sleep(d);
    }

    use slopty_capture::{
        Capture, CaptureConfig, CapturedFrame, PixelFormat, Shareable, Target, enumerate,
        sck_defaults, window_owner_pid,
    };
    use slopty_core::WindowId;
    use slopty_proto::screen::CaptureTarget;

    const GHOSTTY: &str = "/Applications/Ghostty.app/Contents/MacOS/ghostty";

    /// A Ghostty window running `command`, killed on drop.
    struct Scroller(Child);

    impl Drop for Scroller {
        fn drop(&mut self) {
            let _killed = self.0.kill();
            let _reaped = self.0.wait();
        }
    }

    fn gated() -> bool {
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("skipped: set SLOPTY_SCREEN_E2E=1");
            return false;
        }
        true
    }

    fn seconds() -> u64 {
        std::env::var("SLOPTY_E2E_SECONDS").ok().and_then(|v| v.parse().ok()).unwrap_or(5)
    }

    fn shareable() -> Shareable {
        let (tx, rx) = mpsc::channel();
        enumerate(move |r| {
            let _gone = tx.send(r);
        });
        rx.recv_timeout(Duration::from_secs(10)).expect("enumerate").expect("content")
    }

    /// Launch Ghostty running `command` and wait for its window.
    fn launch(command: &[&str]) -> (Scroller, WindowId, Shareable) {
        let child = Command::new(GHOSTTY)
            .arg("-e")
            .args(command)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("Ghostty.app installed");
        let pid = i32::try_from(child.id()).expect("pid");
        let scroller = Scroller(child);
        let started = Instant::now();
        loop {
            let content = shareable();
            let window = content
                .windows()
                .into_iter()
                .find(|w| w.on_screen && window_owner_pid(w.id) == Some(pid));
            if let Some(w) = window {
                eprintln!(
                    "window {} {}×{} at ({}, {}) {} — {}",
                    w.id.0, w.w, w.h, w.x, w.y, w.app, w.title
                );
                // Let the terminal reach steady state before timing anything.
                pause(Duration::from_millis(1500));
                return (scroller, w.id, shareable());
            }
            assert!(started.elapsed() < Duration::from_secs(15), "no Ghostty window for pid {pid}");
            pause(Duration::from_millis(250));
        }
    }

    /// One capture run: `(n, p50, p95, max)` of the capture latency in microseconds, the
    /// display-time → pts offset p50 and the number of inter-frame gaps over 25 ms.
    struct Run {
        n: usize,
        p50: u64,
        p95: u64,
        max: u64,
        offset_p50_us: i64,
        gaps: usize,
        fps: f64,
    }

    fn quantile(sorted: &[u64], q: f64) -> u64 {
        if sorted.is_empty() {
            return 0;
        }
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "test")]
        let i = ((sorted.len() - 1) as f64 * q).round() as usize;
        sorted[i.min(sorted.len() - 1)]
    }

    fn run(target: &Target, config: &CaptureConfig, seconds: u64) -> Run {
        let (tx, rx) = mpsc::channel::<(u64, i64, u64)>();
        let (started_tx, started_rx) = mpsc::channel();
        let capture = Capture::start(
            target,
            config,
            move |frame: CapturedFrame| {
                let offset = frame.display_ts_us.map_or(0, |d| {
                    i64::try_from(d).unwrap_or(0) - i64::try_from(frame.capture_ts_us).unwrap_or(0)
                });
                let _gone = tx.send((frame.latency_us, offset, slopty_capture::host_now_us()));
            },
            None,
            |e| panic!("capture stopped: {e}"),
            move |r| {
                let _gone = started_tx.send(r);
            },
        )
        .expect("start");
        started_rx.recv_timeout(Duration::from_secs(10)).expect("start callback").expect("started");
        // Skip the first half second: stream start-up and the encoder-less warm frames.
        let settle = Instant::now() + Duration::from_millis(500);
        while Instant::now() < settle {
            let _skipped = rx.recv_timeout(settle - Instant::now());
        }
        let deadline = Instant::now() + Duration::from_secs(seconds);
        let mut latencies = Vec::new();
        let mut offsets = Vec::new();
        let mut gaps = 0;
        let mut last_at: Option<u64> = None;
        while let Ok(remaining) = deadline.checked_duration_since(Instant::now()).ok_or(()) {
            let Ok((latency, offset, at)) = rx.recv_timeout(remaining) else { break };
            latencies.push(latency);
            offsets.push(offset);
            if let Some(prev) = last_at
                && at.saturating_sub(prev) > 25_000
            {
                gaps += 1;
            }
            last_at = Some(at);
        }
        let (tx, rx) = mpsc::channel();
        capture.stop(move |r| {
            let _gone = tx.send(r);
        });
        let _stopped = rx.recv_timeout(Duration::from_secs(5));
        latencies.sort_unstable();
        offsets.sort_unstable();
        let n = latencies.len();
        let fps = n as f64 / seconds as f64;
        Run {
            n,
            p50: quantile(&latencies, 0.5),
            p95: quantile(&latencies, 0.95),
            max: quantile(&latencies, 1.0),
            offset_p50_us: offsets.get(n / 2).copied().unwrap_or(0),
            gaps,
            fps,
        }
    }

    fn config(size: (u32, u32), depth: u8, crop: Option<slopty_capture::Crop>) -> CaptureConfig {
        CaptureConfig {
            width: size.0,
            height: size.1,
            fps: 60,
            format: PixelFormat::Nv12,
            queue_depth: depth,
            audio: false,
            crop,
        }
    }

    fn row(label: &str, depth: u8, r: &Run) {
        eprintln!(
            "| {label:<14} | {depth} | {:>4} ({:.1} fps) | {:.2} / {:.2} / {:.2} ms | offset {:.2} ms | gaps>25ms {} |",
            r.n,
            r.fps,
            r.p50 as f64 / 1e3,
            r.p95 as f64 / 1e3,
            r.max as f64 / 1e3,
            r.offset_p50_us as f64 / 1e3,
            r.gaps
        );
    }

    /// The luma plane of a frame (NV12 plane 0), row by row.
    fn luma(frame: &CapturedFrame) -> Vec<u8> {
        use objc2_core_video::{
            CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
            CVPixelBufferGetHeightOfPlane, CVPixelBufferGetWidthOfPlane,
            CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
        };
        let cv = frame.image.as_cv();
        // SAFETY: valid buffer; a read-only lock, unlocked below.
        let locked = unsafe { CVPixelBufferLockBaseAddress(cv, CVPixelBufferLockFlags::ReadOnly) };
        assert_eq!(locked, 0, "lock");
        let (w, h, stride) = (
            CVPixelBufferGetWidthOfPlane(cv, 0),
            CVPixelBufferGetHeightOfPlane(cv, 0),
            CVPixelBufferGetBytesPerRowOfPlane(cv, 0),
        );
        let base = CVPixelBufferGetBaseAddressOfPlane(cv, 0).cast::<u8>();
        let mut out = Vec::with_capacity(w * h);
        for row in 0..h {
            // SAFETY: row `row` starts `row * stride` bytes into the locked plane.
            let start = unsafe { base.add(row * stride) };
            // SAFETY: every row has at least `w` readable bytes while the buffer is locked.
            let line = unsafe { std::slice::from_raw_parts(start, w) };
            out.extend_from_slice(line);
        }
        // SAFETY: matches the lock above.
        let _unlocked =
            unsafe { CVPixelBufferUnlockBaseAddress(cv, CVPixelBufferLockFlags::ReadOnly) };
        out
    }

    /// One frame from `target`, luma only.
    fn one_frame(target: &Target, config: &CaptureConfig) -> (Vec<u8>, (usize, usize)) {
        let (tx, rx) = mpsc::channel::<CapturedFrame>();
        let (started_tx, started_rx) = mpsc::channel();
        let capture = Capture::start(
            target,
            config,
            move |frame| {
                let _gone = tx.send(frame);
            },
            None,
            |e| panic!("capture stopped: {e}"),
            move |r| {
                let _gone = started_tx.send(r);
            },
        )
        .expect("start");
        started_rx.recv_timeout(Duration::from_secs(10)).expect("start callback").expect("started");
        // The first frames settle the stream's scaling; take the third.
        let mut frame = None;
        for _ in 0..3 {
            frame = Some(rx.recv_timeout(Duration::from_secs(5)).expect("a frame"));
        }
        let frame = frame.expect("three frames");
        let size = (frame.image.width(), frame.image.height());
        let luma = luma(&frame);
        let (tx, rx) = mpsc::channel();
        capture.stop(move |r| {
            let _gone = tx.send(r);
        });
        let _stopped = rx.recv_timeout(Duration::from_secs(5));
        (luma, size)
    }

    /// The window filter and the display crop must show the same picture of an unobscured
    /// window: mean absolute luma difference under 2/255 (the corners differ: the window
    /// filter leaves them transparent, the crop shows what is behind them).
    #[test]
    fn display_crop_shows_the_window_filter_picture() {
        if !gated() {
            return;
        }
        let (_sleeper, id, content) = launch(&["sleep", "600"]);
        let window = Target::resolve(&content, CaptureTarget::Window(id)).expect("window target");
        let crop =
            Target::resolve_crop(&content, id).expect("crop resolve").expect("on one display");
        assert_eq!(window.pixel_size(), crop.pixel_size(), "same output size");
        let (a, size_a) = one_frame(&window, &config(window.pixel_size(), 2, None));
        let (b, size_b) = one_frame(&crop, &config(crop.pixel_size(), 2, crop.crop()));
        assert_eq!(size_a, size_b);
        let total: u64 = a.iter().zip(&b).map(|(&x, &y)| u64::from(x.abs_diff(y))).sum();
        let mean = total as f64 / a.len() as f64;
        // Interior only (16 px in from every edge): the picture without corners or shadow.
        let (w, h) = size_a;
        let mut inner_total = 0_u64;
        let mut inner_n = 0_u64;
        for y in 16..h.saturating_sub(16) {
            for x in 16..w.saturating_sub(16) {
                inner_total += u64::from(a[y * w + x].abs_diff(b[y * w + x]));
                inner_n += 1;
            }
        }
        let inner_mean = inner_total as f64 / inner_n.max(1) as f64;
        eprintln!("window vs crop {w}×{h}: mean |Δluma| {mean:.3} (interior {inner_mean:.3})");
        assert!(
            inner_mean < 2.0,
            "the crop shows a different picture: interior mean |Δ| {inner_mean:.3}"
        );
    }

    /// What moving the crop costs: `updateConfiguration` with a shifted `sourceRect` on the
    /// live stream, timed to its completion and to the next frame.
    #[test]
    fn moving_the_crop_is_one_configuration_update() {
        if !gated() {
            return;
        }
        let (_scroller, id, content) = launch(&["yes"]);
        let crop =
            Target::resolve_crop(&content, id).expect("crop resolve").expect("on one display");
        let (tx, rx) = mpsc::channel::<u64>();
        let (started_tx, started_rx) = mpsc::channel();
        let mut cfg = config(crop.pixel_size(), 2, crop.crop());
        let capture = Capture::start(
            &crop,
            &cfg,
            move |_frame| {
                let _gone = tx.send(slopty_capture::host_now_us());
            },
            None,
            |e| panic!("capture stopped: {e}"),
            move |r| {
                let _gone = started_tx.send(r);
            },
        )
        .expect("start");
        started_rx.recv_timeout(Duration::from_secs(10)).expect("start callback").expect("started");
        let _first = rx.recv_timeout(Duration::from_secs(5)).expect("a frame");
        pause(Duration::from_millis(500));
        while rx.try_recv().is_ok() {}
        let mut updates = Vec::new();
        let mut next_frame = Vec::new();
        for i in 1..=5_u32 {
            let mut moved = crop.crop().expect("crop");
            moved.x += f64::from(i);
            cfg.crop = Some(moved);
            let (done_tx, done_rx) = mpsc::channel();
            let at = slopty_capture::host_now_us();
            capture.update(&cfg, move |r| {
                let _gone = done_tx.send(r);
            });
            done_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("update callback")
                .expect("updated");
            updates.push(slopty_capture::host_now_us() - at);
            let frame_at =
                rx.recv_timeout(Duration::from_secs(5)).expect("a frame after the update");
            next_frame.push(frame_at.saturating_sub(at));
            while rx.try_recv().is_ok() {}
            pause(Duration::from_millis(200));
        }
        eprintln!(
            "crop move: update completion {updates:?} µs, next frame after the update {next_frame:?} µs"
        );
        let (tx, rx) = mpsc::channel();
        capture.stop(move |r| {
            let _gone = tx.send(r);
        });
        let _stopped = rx.recv_timeout(Duration::from_secs(5));
    }

    /// The ruled floor for a window target's capture latency on this machine class
    /// (MEASUREMENTS.md "capture floor"): p95 ≈ 1 ms through the window filter. The margin
    /// covers a loaded machine.
    const WINDOW_P95_FLOOR_US: u64 = 1_000;
    const WINDOW_P95_MARGIN_US: u64 = 3_000;

    /// Guard: the window filter's capture latency p95 stays under the ruled floor plus
    /// margin, and the display crop is no worse.
    #[test]
    fn window_capture_latency_p95_stays_under_the_floor() {
        if !gated() {
            return;
        }
        let (_scroller, id, content) = launch(&["yes"]);
        let window = Target::resolve(&content, CaptureTarget::Window(id)).expect("window target");
        let w = run(&window, &config(window.pixel_size(), 2, None), 3);
        row("window", 2, &w);
        assert!(w.n >= 100, "a scrolling terminal delivers frames: {}", w.n);
        assert!(
            w.p95 <= WINDOW_P95_FLOOR_US + WINDOW_P95_MARGIN_US,
            "window capture latency p95 {} µs is over the {} µs floor + {} µs margin",
            w.p95,
            WINDOW_P95_FLOOR_US,
            WINDOW_P95_MARGIN_US
        );
        if let Some(crop) = Target::resolve_crop(&content, id).expect("crop resolve") {
            let c = run(&crop, &config(crop.pixel_size(), 2, crop.crop()), 3);
            row("display-crop", 2, &c);
            assert!(
                c.p95 <= WINDOW_P95_FLOOR_US + WINDOW_P95_MARGIN_US,
                "display-crop capture latency p95 {} µs is over the floor + margin",
                c.p95
            );
        }
    }

    /// The survey: display filter vs window filter vs display crop, and `queueDepth`
    /// 2 / 3 / 5 / 8 on the window filter. Prints one Markdown row per run.
    #[test]
    fn capture_latency_floor_by_path_and_queue_depth() {
        if !gated() {
            return;
        }
        let defaults = sck_defaults();
        eprintln!("SCStreamConfiguration defaults: {defaults:?}");
        let (_scroller, id, content) = launch(&["yes"]);
        let seconds = seconds();
        let window = Target::resolve(&content, CaptureTarget::Window(id)).expect("window target");
        let crop = Target::resolve_crop(&content, id)
            .expect("crop resolve")
            .expect("window entirely on one display");
        let display_id =
            slopty_capture::display_enclosing(&slopty_capture::window_bounds(id).unwrap()).unwrap();
        let display =
            Target::resolve(&content, CaptureTarget::Display(display_id)).expect("display target");
        eprintln!("window {window:?}\ncrop {crop:?}\ndisplay {display:?}");
        eprintln!("| path | depth | frames | p50 / p95 / max | display→pts | gaps |");
        eprintln!("| --- | --- | --- | --- | --- | --- |");
        for depth in [2_u8, 3, 5, 8] {
            let r = run(&window, &config(window.pixel_size(), depth, None), seconds);
            row("window", depth, &r);
        }
        for depth in [2_u8, 8] {
            let r = run(&display, &config(display.pixel_size(), depth, None), seconds);
            row("display", depth, &r);
        }
        for depth in [2_u8, 8] {
            let r = run(&crop, &config(crop.pixel_size(), depth, crop.crop()), seconds);
            row("display-crop", depth, &r);
        }
    }
}

/// What sits above a window: the occlusion query's inputs, printed for the measurement log.
#[cfg(test)]
mod occlusion {
    use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};
    use objc2_core_graphics::{
        CGWindowListCopyWindowInfo, CGWindowListOption, kCGWindowAlpha, kCGWindowBounds,
        kCGWindowLayer, kCGWindowOwnerName, kCGWindowOwnerPID,
    };
    use slopty_core::WindowId;

    #[test]
    fn print_windows_above_the_given_window() {
        let Some(id) = std::env::var("SLOPTY_E2E_WINDOW").ok().and_then(|v| v.parse::<u32>().ok())
        else {
            eprintln!("skipped: set SLOPTY_E2E_WINDOW=<id>");
            return;
        };
        let id = WindowId(id);
        let bounds = slopty_capture::window_bounds(id).expect("bounds");
        let pid = slopty_capture::window_owner_pid(id).expect("pid");
        eprintln!(
            "window {id} {bounds:?} pid {pid} occluded {}",
            slopty_capture::occluded(id, &bounds, pid)
        );
        let list = CGWindowListCopyWindowInfo(CGWindowListOption::OptionOnScreenAboveWindow, id.0)
            .expect("list");
        // SAFETY: dictionaries keyed by `kCGWindow*` strings.
        let list: CFRetained<CFArray<CFDictionary<CFString, CFType>>> =
            unsafe { CFRetained::cast_unchecked(list) };
        // SAFETY: framework constants.
        let (pid_key, layer_key, name_key, alpha_key, bounds_key): (
            &CFString,
            &CFString,
            &CFString,
            &CFString,
            &CFString,
        ) = unsafe {
            (kCGWindowOwnerPID, kCGWindowLayer, kCGWindowOwnerName, kCGWindowAlpha, kCGWindowBounds)
        };
        for d in list.iter() {
            let pid =
                d.get(pid_key).and_then(|v| v.downcast::<CFNumber>().ok()).and_then(|n| n.as_i32());
            let layer = d
                .get(layer_key)
                .and_then(|v| v.downcast::<CFNumber>().ok())
                .and_then(|n| n.as_i32());
            let name =
                d.get(name_key).and_then(|v| v.downcast::<CFString>().ok()).map(|s| s.to_string());
            let alpha = d
                .get(alpha_key)
                .and_then(|v| v.downcast::<CFNumber>().ok())
                .and_then(|n| n.as_f64());
            let rect = d.get(bounds_key).and_then(|v| v.downcast::<CFDictionary>().ok()).map(|b| {
                let mut cg = objc2_core_foundation::CGRect::default();
                // SAFETY: dictionary form of a rect; valid out pointer.
                let ok = unsafe {
                    objc2_core_graphics::CGRectMakeWithDictionaryRepresentation(
                        Some(&b),
                        std::ptr::from_mut(&mut cg),
                    )
                };
                (ok, cg.origin.x, cg.origin.y, cg.size.width, cg.size.height)
            });
            eprintln!("  above: pid {pid:?} layer {layer:?} alpha {alpha:?} {name:?} {rect:?}");
        }
    }
}
