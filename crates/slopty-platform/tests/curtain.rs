//! The curtain against the live window server, in the VM lane's guest (`cargo xtask vm live -p
//! slopty-platform --test curtain`): it covers the screens and posts HID events, so never on a
//! Mac someone is using.
//!
//! - The shield is on the screens and out of the capture. A child app
//!   (`tests/support/curtain_app.rs`) puts a magenta window over the main display and the shield
//!   over every display. ScreenCaptureKit reads the display through the filter that leaves the
//!   shield's windows out, and through the plain one: the first sees magenta and the second the
//!   shield's black. Both a still picture and a stream's frames are read. The stream's timings with
//!   and without the exclusion are printed (`MEASURE`).
//! - The hold drops the local input and passes the worker's. An untagged pointer move posted at the
//!   HID tap moves nothing and is counted; a tagged one moves the pointer.

#[cfg(test)]
#[cfg(target_os = "macos")]
#[expect(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a live test's arithmetic on pixel offsets and microsecond counts"
)]
mod tests {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::process::{Child, ChildStdin, Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use objc2_core_foundation::CGPoint;
    use objc2_core_graphics::{
        CGEvent, CGEventField, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
        CGEventType, CGMainDisplayID, CGMouseButton,
    };
    use objc2_core_video::{
        CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
        CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
        CVPixelBufferUnlockBaseAddress,
    };
    use slopty_capture::{
        Capture, CaptureConfig, CapturedFrame, PixelFormat, PixelOrder, Shareable, Target,
        enumerate,
    };
    use slopty_core::DisplayId;
    use slopty_platform::curtain::InputHold;
    use slopty_proto::screen::CaptureTarget;

    /// The worker's tag (`slopty_input::SLOPTY_EVENT`): "SLOP".
    const OURS: i64 = 0x534c_4f50;

    #[expect(clippy::disallowed_methods, reason = "a test thread, not library code")]
    fn pause(d: Duration) {
        std::thread::sleep(d);
    }

    fn shareable() -> Shareable {
        let (tx, rx) = mpsc::channel();
        enumerate(move |r| {
            let _gone = tx.send(r);
        });
        rx.recv_timeout(Duration::from_secs(10)).expect("enumerate").expect("content")
    }

    /// The child app, its stdin to drive it and its stdout's lines.
    struct App {
        child: Child,
        input: ChildStdin,
        lines: std::io::Lines<BufReader<std::process::ChildStdout>>,
    }

    impl App {
        fn start() -> (Self, u32, Vec<u32>) {
            let mut child = Command::new(env!("CARGO_BIN_EXE_slopty-curtain-app"))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .expect("the curtain app starts");
            let input = child.stdin.take().expect("its stdin");
            let mut lines = BufReader::new(child.stdout.take().expect("its stdout")).lines();
            let made = lines.next().and_then(Result::ok).unwrap_or_default();
            let number = |key: &str| {
                made.split_whitespace()
                    .find_map(|part| part.strip_prefix(key))
                    .unwrap_or_else(|| panic!("the app said {made:?}"))
                    .to_owned()
            };
            let test: u32 = number("test=").parse().expect("the test window's number");
            let shield: Vec<u32> =
                number("shield=").split(',').map(|n| n.parse().expect("a number")).collect();
            assert!(!shield.is_empty(), "a shield window per display");
            (Self { child, input, lines }, test, shield)
        }

        fn ask(&mut self, line: &str, answer: &str) {
            writeln!(self.input, "{line}").expect("the app reads");
            let said = self.lines.next().and_then(Result::ok).unwrap_or_default();
            assert_eq!(said, answer);
        }
    }

    impl Drop for App {
        fn drop(&mut self) {
            let _killed = self.child.kill();
            let _reaped = self.child.wait();
        }
    }

    /// The middle pixel of a still picture of `target`, as (red, green, blue).
    fn middle_of_snapshot(target: &Target) -> [u8; 3] {
        let (tx, rx) = mpsc::channel();
        target.snapshot(move |r| {
            let _gone = tx.send(r);
        });
        let picture = rx.recv_timeout(Duration::from_secs(10)).expect("a picture").expect("ok");
        let at = (picture.height as usize / 2) * picture.bytes_per_row
            + (picture.width as usize / 2) * 4;
        let px = &picture.data[at..at + 4];
        match picture.order {
            PixelOrder::Bgra => [px[2], px[1], px[0]],
            PixelOrder::Argb => [px[1], px[2], px[3]],
        }
    }

    /// The middle pixel of a BGRA frame, as (red, green, blue).
    fn middle_of_frame(frame: &CapturedFrame) -> [u8; 3] {
        let buffer = frame.image.as_cv();
        // SAFETY: a live pixel buffer, locked read-only for the read below and unlocked after.
        unsafe {
            CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly);
        }
        let (width, height) = (CVPixelBufferGetWidth(buffer), CVPixelBufferGetHeight(buffer));
        let row = CVPixelBufferGetBytesPerRow(buffer);
        let base = CVPixelBufferGetBaseAddress(buffer).cast::<u8>();
        let at = (height / 2) * row + (width / 2) * 4;
        // SAFETY: the base address of a locked BGRA buffer of `height` rows of `row` bytes, so
        // the middle pixel's offset lies inside it.
        let middle = unsafe { base.add(at) };
        // SAFETY: the middle pixel's four bytes, inside the locked buffer.
        let px = unsafe { std::slice::from_raw_parts(middle, 4) };
        let rgb = [px[2], px[1], px[0]];
        // SAFETY: the lock taken above.
        unsafe {
            CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly);
        }
        rgb
    }

    fn magenta(rgb: [u8; 3]) -> bool {
        rgb[0] > 200 && rgb[1] < 60 && rgb[2] > 200
    }

    fn black(rgb: [u8; 3]) -> bool {
        rgb.iter().all(|c| *c < 24)
    }

    /// A stream of `target` for `seconds`: the middle pixel of its last frame, how many frames
    /// came, and the capture latency's median and 95th percentile in microseconds.
    fn stream(target: &Target, seconds: f64) -> ([u8; 3], usize, u64, u64) {
        let (width, height) = target.pixel_size();
        let config = CaptureConfig {
            width,
            height,
            align: 1,
            fps: 0,
            format: PixelFormat::Bgra,
            queue_depth: 3,
            crop: None,
            region: None,
        };
        let (tx, rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::channel();
        let capture = Capture::start(
            target,
            &config,
            move |frame: CapturedFrame| {
                let rgb = middle_of_frame(&frame);
                let _gone = tx.send((rgb, frame.latency_us));
            },
            |e| panic!("capture stopped: {e}"),
            move |r| {
                let _gone = started_tx.send(r);
            },
        )
        .expect("start");
        started_rx.recv_timeout(Duration::from_secs(10)).expect("started").expect("ok");
        let settle = Instant::now() + Duration::from_millis(500);
        let end = Instant::now() + Duration::from_secs_f64(seconds);
        let (mut last, mut latencies) = (None, Vec::new());
        while Instant::now() < end {
            if let Ok((rgb, latency)) = rx.recv_timeout(Duration::from_millis(50)) {
                last = Some(rgb);
                if Instant::now() > settle {
                    latencies.push(latency);
                }
            }
        }
        let (stopped_tx, stopped_rx) = mpsc::channel();
        capture.stop(move |r| {
            let _gone = stopped_tx.send(r);
        });
        let _stopped = stopped_rx.recv_timeout(Duration::from_secs(10));
        latencies.sort_unstable();
        let at = |q: f64| latencies[((latencies.len() - 1) as f64 * q) as usize];
        let frames = latencies.len();
        assert!(frames > 0, "the stream sent frames");
        (last.expect("a frame"), frames, at(0.5), at(0.95))
    }

    /// The shield covers the screen, and a display capture that leaves its windows out sees
    /// what is under it, in a still picture and in a stream's frames. A filter made before the
    /// shield went up leaves it out as well, which is the order the worker keeps.
    #[test]
    fn the_shield_is_on_the_screens_and_out_of_the_capture() {
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            slopty_testkit::live::skip("set SLOPTY_SCREEN_E2E=1");
            return;
        }
        let (mut app, test, shield) = App::start();
        pause(Duration::from_millis(500));
        let main = CaptureTarget::Display(DisplayId(CGMainDisplayID()));
        let before = shareable();
        let excluding = Target::resolve_excluding(&before, main, &shield).expect("the display");
        let left_out = magenta(middle_of_snapshot(&excluding));
        assert!(left_out, "the test window {test} is on the main display");

        app.ask("show", "shown");
        pause(Duration::from_millis(500));
        let plain = Target::resolve(&shareable(), main).expect("the display");
        let seen = middle_of_snapshot(&plain);
        assert!(black(seen), "the shield is over the test window: {seen:?}");
        let under = middle_of_snapshot(&excluding);
        assert!(magenta(under), "the shield is left out of the picture: {under:?}");

        let (shown, frames, p50, p95) = stream(&plain, 4.0);
        assert!(black(shown), "a plain stream sees the shield: {shown:?}");
        eprintln!(
            "MEASURE curtain plain display stream: {frames} frames, p50 {p50} µs, p95 {p95} µs"
        );
        let (beneath, frames, p50, p95) = stream(&excluding, 4.0);
        assert!(magenta(beneath), "a stream that leaves the shield out sees under it: {beneath:?}");
        eprintln!("MEASURE curtain shield left out: {frames} frames, p50 {p50} µs, p95 {p95} µs");

        app.ask("hide", "hidden");
        pause(Duration::from_millis(500));
        assert!(magenta(middle_of_snapshot(&plain)), "the shield is gone with its windows");
    }

    /// The pointer where the window server has it.
    fn pointer() -> CGPoint {
        CGEvent::location(CGEvent::new(None).as_deref())
    }

    /// Post a pointer move to `to` at the HID tap, tagged `tag` (0 for none, as a mouse's).
    fn move_to(to: CGPoint, tag: i64) {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
        let event = CGEvent::new_mouse_event(
            source.as_deref(),
            CGEventType::MouseMoved,
            to,
            CGMouseButton::Left,
        )
        .expect("a move");
        if tag != 0 {
            CGEvent::set_integer_value_field(Some(&event), CGEventField::EventSourceUserData, tag);
        }
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        pause(Duration::from_millis(200));
    }

    fn near(a: CGPoint, b: CGPoint) -> bool {
        (a.x - b.x).abs() < 1.5 && (a.y - b.y).abs() < 1.5
    }

    /// While the input is held, a move without the worker's tag is dropped and counted, and a
    /// tagged one moves the pointer; let go, an untagged move moves it again.
    #[test]
    fn the_hold_drops_local_input_and_passes_the_worker_s() {
        if std::env::var_os("SLOPTY_INPUT_E2E").is_none() {
            slopty_testkit::live::skip("set SLOPTY_INPUT_E2E=1");
            return;
        }
        let (a, b, c) =
            (CGPoint::new(200.0, 200.0), CGPoint::new(300.0, 260.0), CGPoint::new(240.0, 320.0));
        move_to(a, 0);
        assert!(near(pointer(), a), "the guest takes an untagged move before the hold");

        let hold =
            InputHold::start(OURS, Duration::from_secs(60)).expect("Accessibility in the guest");
        move_to(b, 0);
        assert!(
            near(pointer(), a),
            "a local move is held: the pointer stays at {a:?}, not {:?}",
            pointer()
        );
        assert!(hold.held() >= 1, "the held move is counted");
        move_to(c, OURS);
        assert!(near(pointer(), c), "the worker's move goes through: {:?}", pointer());
        drop(hold);

        move_to(b, 0);
        assert!(near(pointer(), b), "let go, local input moves the pointer again");

        // A lease nobody renews lapses, and the desk has its input back while still held.
        let lease = Duration::from_millis(300);
        let hold = InputHold::start(OURS, lease).expect("Accessibility in the guest");
        // Nothing is sent on it: the wait is the lease running out.
        let (_quiet, out) = mpsc::channel::<()>();
        let _lapsed = out.recv_timeout(lease * 2);
        move_to(a, 0);
        assert!(near(pointer(), a), "the lease lapsed: a local move goes through");
        drop(hold);
    }
}
