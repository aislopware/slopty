//! Posts real events; needs post-event (Accessibility) access, so it is gated by
//! `SLOPTY_INPUT_E2E=1` and skips itself when the process lacks the permission.
//!
//! [`gestures`] posts only to an application the test starts itself
//! (`tests/support/gesture_app.rs`) and reads back what AppKit made of each event:
//! `SLOPTY_INPUT_E2E=1 cargo nextest run -p slopty-input --test inject gestures --no-capture`.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use objc2_core_graphics::CGMainDisplayID;
    use slopty_core::DisplayId;
    use slopty_input::Injector;
    use slopty_proto::screen::{CaptureTarget, ScreenInput};

    /// Quarter of the way across and down the main display, in display points.
    const FRACTION: f64 = 0.25;

    #[expect(clippy::cast_possible_truncation, reason = "display points fit f32")]
    const fn to_f32(v: f64) -> f32 {
        v as f32
    }

    #[tokio::test]
    async fn moves_the_real_pointer_on_a_display_stream() {
        if std::env::var_os("SLOPTY_INPUT_E2E").is_none() {
            slopty_testkit::live::skip("set SLOPTY_INPUT_E2E=1");
            return;
        }
        if !slopty_input::can_post() {
            slopty_testkit::live::skip("no post-event access");
            return;
        }
        let display = CGMainDisplayID();
        let bounds = slopty_capture::target_bounds(CaptureTarget::Display(DisplayId(display)))
            .expect("main display has bounds");
        let (start_x, start_y) = slopty_capture::pointer_location();

        // A 1:1 stream, so stream pixels are display points.
        let mut injector = Injector::new(CaptureTarget::Display(DisplayId(display)), 1.0);
        let (x, y) = (bounds.w * FRACTION, bounds.h * FRACTION);
        injector.inject(&ScreenInput::Move { x: to_f32(x), y: to_f32(y) }).expect("post");
        tokio::time::sleep(Duration::from_millis(50)).await;
        let (px, py) = slopty_capture::pointer_location();
        assert!(
            (px - (bounds.x + x)).abs() < 2.0 && (py - (bounds.y + y)).abs() < 2.0,
            "{px},{py}"
        );

        // Put it back.
        let back =
            ScreenInput::Move { x: to_f32(start_x - bounds.x), y: to_f32(start_y - bounds.y) };
        injector.inject(&back).expect("post");
    }
}

/// The trackpad gestures a window stream posts, as the application it streams receives them:
/// the test's own app, the only process these tests post to, whose window is far off every
/// display (`tests/support/gesture_app.rs`, which says what it reads and why at the
/// application's queue).
#[cfg(test)]
#[cfg(target_os = "macos")]
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a live test printing what it saw and its timings"
)]
mod gestures {
    use std::io::{BufRead as _, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use objc2_core_foundation::CGPoint;
    use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventType, CGMouseButton};
    use slopty_capture::Rect;
    use slopty_core::WindowId;
    use slopty_input::{Backend, Event, Injector, InputError, Post, Route, System};
    use slopty_proto::input::{Mods, MouseButton};
    use slopty_proto::screen::{CaptureTarget, ScreenInput, ScrollPhase, SwipeDirection};

    const NONE: ScrollPhase = ScrollPhase::None;
    /// Where in the window the events go, in its points (a 1:1 stream).
    const X: f32 = 200.0;
    const Y: f32 = 150.0;

    /// The real backend, except that the test's app is taken to be active already: a press
    /// posts as it does to any window stream, and nothing raises the app over the desktop of
    /// whoever is at the Mac.
    struct Unraised;

    impl Backend for Unraised {
        fn owner_pid(&self, target: CaptureTarget) -> Option<i32> {
            System.owner_pid(target)
        }

        fn bounds(&mut self, target: CaptureTarget) -> Option<Rect> {
            System.bounds(target)
        }

        fn is_active(&mut self, _pid: i32) -> bool {
            true
        }

        fn activate(&mut self, _pid: i32, _window: Option<u32>) -> Result<(), InputError> {
            Ok(())
        }

        fn post(&mut self, post: Post) -> Result<(), InputError> {
            System.post(post)
        }
    }

    /// The test's own application, and what it says.
    struct App {
        child: Child,
        lines: mpsc::Receiver<String>,
        pid: i32,
        window: u32,
        swipe_tracking: bool,
    }

    impl App {
        fn start() -> Self {
            Self::start_with(&[])
        }

        /// The app with `args`. Swipe tracking is turned on for it alone, in its own argument
        /// domain, whatever the Mac has.
        fn start_with(args: &[&str]) -> Self {
            // Where nextest put it, in an archive unpacked in a guest; else where cargo built it.
            let path = std::env::var_os("NEXTEST_BIN_EXE_slopty-gesture-app")
                .unwrap_or_else(|| env!("CARGO_BIN_EXE_slopty-gesture-app").into());
            let mut child = Command::new(path)
                .args(["-AppleEnableSwipeNavigateWithScrolls", "YES"])
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .expect("the test's app starts");
            let stdout = child.stdout.take().expect("its stdout");
            let (tx, lines) = mpsc::channel();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
            let ready = lines.recv_timeout(Duration::from_secs(10)).expect("the app is ready");
            let field = |key: &str| -> i64 {
                ready
                    .split(' ')
                    .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('=')?.parse().ok())
                    .unwrap_or_else(|| panic!("{key} in {ready:?}"))
            };
            Self {
                pid: i32::try_from(field("pid")).unwrap(),
                window: u32::try_from(field("window")).unwrap(),
                swipe_tracking: field("swipe_tracking") == 1,
                child,
                lines,
            }
        }

        /// An injector for the app's window, which posts to this app and nowhere else.
        fn injector(&self) -> Injector {
            let injector = Injector::new(CaptureTarget::Window(WindowId(self.window)), 1.0);
            assert_eq!(injector.route(), Route::Pid(self.pid), "posts only to the test's app");
            injector
        }

        /// What the app says until a line `last` accepts, that line included.
        fn until(&self, last: impl Fn(&str) -> bool) -> Vec<String> {
            let deadline = Instant::now().checked_add(Duration::from_secs(10)).expect("a deadline");
            let mut said = Vec::new();
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                let Ok(line) = self.lines.recv_timeout(left) else {
                    panic!("the app never said it; it said {said:#?}");
                };
                let done = last(&line);
                said.push(line);
                if done {
                    return said;
                }
            }
        }

        /// Post `inputs`, then a pointer move, and what the app took off its queue up to that
        /// move: every event posted before it, in order.
        fn post<B: Backend>(
            &self,
            injector: &mut Injector<B>,
            inputs: &[ScreenInput],
        ) -> Vec<String> {
            for input in inputs {
                injector.inject(input).expect("posted");
            }
            injector.inject(&ScreenInput::Move { x: 1.0, y: 1.0 }).expect("posted");
            let mut said = self.until(|line| line.starts_with("app type=5"));
            said.pop();
            said.retain(|line| line.starts_with("app "));
            said
        }
    }

    impl Drop for App {
        fn drop(&mut self) {
            drop(self.child.stdin.take());
            let _killed = self.child.kill();
            let _waited = self.child.wait();
        }
    }

    /// A drag in a window stream: the press, each drag and the release reach the app as one
    /// press, under one event number, and the next press under a new one. Posted without it,
    /// CoreGraphics numbers them itself, as the lines it prints show.
    #[test]
    fn a_drag_reaches_the_app_under_its_press_number() {
        if !live() {
            return;
        }
        let app = App::start();
        let target = CaptureTarget::Window(WindowId(app.window));
        let mut injector = Injector::with_backend(target, 1.0, Unraised);
        assert_eq!(injector.route(), Route::Pid(app.pid), "posts only to the test's app");
        let button = |down| ScreenInput::Button {
            button: MouseButton::Left,
            down,
            clicks: 1,
            x: X,
            y: Y,
            mods: Mods::empty(),
        };
        let to = |x| ScreenInput::Move { x, y: Y };
        let said = app.post(
            &mut injector,
            &[
                button(true),
                to(210.0),
                to(220.0),
                to(230.0),
                button(false),
                button(true),
                button(false),
            ],
        );
        eprintln!("numbered: {said:#?}");
        let presses: Vec<(u32, i64)> = said
            .iter()
            .filter_map(|line| {
                let kind = line.strip_prefix("app type=")?.split(' ').next()?.parse().ok()?;
                let number = line.split(" number=").nth(1)?.split(' ').next()?.parse().ok()?;
                Some((kind, number))
            })
            .collect();
        // NSEventType: 1 left down, 6 left dragged, 2 left up. AppKit coalesces drags that
        // wait in its queue, so the three may come as fewer.
        let mut kinds: Vec<u32> = presses.iter().map(|(kind, _)| *kind).collect();
        kinds.dedup();
        assert_eq!(kinds, [1, 6, 2, 1, 2], "{said:#?}");
        let released = presses.iter().position(|(kind, _)| *kind == 2).expect("a release");
        let (dragged, next) = presses.split_at(released + 1);
        let (first, second) = (dragged[0].1, next[0].1);
        assert!(dragged.iter().all(|(_, n)| *n == first), "one number: {presses:?}");
        assert!(next.iter().all(|(_, n)| *n == second), "{presses:?}");
        assert_ne!(first, second, "a new press, a new number");

        // The same drag with no number of the injector's own, for the record.
        let bounds = slopty_capture::target_bounds(target).expect("its bounds");
        let at = |x: f32| CGPoint { x: bounds.x + f64::from(x), y: bounds.y + f64::from(Y) };
        let raw = |kind, x| Event::Mouse {
            kind,
            at: at(x),
            button: CGMouseButton::Left,
            number: 0,
            clicks: i64::from(kind != CGEventType::LeftMouseDragged),
            press: 0,
        };
        for event in [
            raw(CGEventType::LeftMouseDown, X),
            raw(CGEventType::LeftMouseDragged, 210.0),
            raw(CGEventType::LeftMouseDragged, 220.0),
            raw(CGEventType::LeftMouseUp, 220.0),
        ] {
            let post = Post { route: Route::Pid(app.pid), flags: CGEventFlags::empty(), event };
            let built = slopty_input::backend::build(&post, None).expect("built");
            CGEvent::post_to_pid(app.pid, Some(&built));
        }
        let said = app.post(&mut injector, &[]);
        eprintln!("unnumbered: {said:#?}");
    }

    fn live() -> bool {
        if std::env::var_os("SLOPTY_INPUT_E2E").is_none() {
            slopty_testkit::live::skip("set SLOPTY_INPUT_E2E=1");
            return false;
        }
        if !slopty_input::can_post() {
            slopty_testkit::live::skip("no post-event access");
            return false;
        }
        true
    }

    /// Wait `for_` between two posts, at the pace a client's input comes.
    #[expect(clippy::disallowed_methods, reason = "a live test pacing its own posts")]
    fn pace(for_: Duration) {
        std::thread::sleep(for_);
    }

    /// A trackpad's report period: 120 a second.
    const REPORT_US: u32 = 8_333;

    /// A trackpad scroll's report, the client's `k`-th at its trackpad's pace.
    const fn scroll(
        k: u32,
        dx: f32,
        dy: f32,
        phase: ScrollPhase,
        momentum: ScrollPhase,
    ) -> ScreenInput {
        ScreenInput::Scroll {
            dx,
            dy,
            precise: true,
            phase,
            momentum,
            x: X,
            y: Y,
            mods: Mods::empty(),
            time_us: k.wrapping_mul(REPORT_US),
        }
    }

    /// Each gesture reaches the app as the event a trackpad's makes, with its phase and its
    /// amount: a scroll through its phases and its coast, a pinch and a rotation through theirs,
    /// one smart zoom, and exactly one swipe for each swipe, its direction as AppKit reads it.
    #[test]
    fn each_gesture_reaches_the_app_as_a_trackpad_s_does() {
        if !live() {
            return;
        }
        let app = App::start();
        let mut injector = app.injector();
        let (x, y) = (X, Y);
        let said = app.post(
            &mut injector,
            &[
                ScreenInput::Gestures { remote: true },
                scroll(0, 0.0, 2.0, ScrollPhase::Began, NONE),
                scroll(1, 0.0, 6.0, ScrollPhase::Changed, NONE),
                scroll(2, 0.0, 0.0, ScrollPhase::Ended, NONE),
                scroll(3, 0.0, 5.0, NONE, ScrollPhase::Began),
                scroll(4, 0.0, 3.0, NONE, ScrollPhase::Changed),
                scroll(5, 0.0, 0.0, NONE, ScrollPhase::Ended),
                ScreenInput::Magnify { delta: 0.0, phase: ScrollPhase::Began, x, y, time_us: 0 },
                ScreenInput::Magnify { delta: 0.25, phase: ScrollPhase::Changed, x, y, time_us: 1 },
                ScreenInput::Magnify { delta: -0.5, phase: ScrollPhase::Ended, x, y, time_us: 2 },
                ScreenInput::Rotate { degrees: 0.0, phase: ScrollPhase::Began, x, y, time_us: 0 },
                ScreenInput::Rotate {
                    degrees: -12.5,
                    phase: ScrollPhase::Changed,
                    x,
                    y,
                    time_us: 1,
                },
                ScreenInput::Rotate {
                    degrees: 0.0,
                    phase: ScrollPhase::Cancelled,
                    x,
                    y,
                    time_us: 2,
                },
                ScreenInput::SmartMagnify { x, y },
                ScreenInput::Swipe { direction: SwipeDirection::Left, x, y },
                ScreenInput::Swipe { direction: SwipeDirection::Right, x, y },
                ScreenInput::Swipe { direction: SwipeDirection::Up, x, y },
                ScreenInput::Swipe { direction: SwipeDirection::Down, x, y },
            ],
        );
        eprintln!("{said:#?}");
        // An accessory app's first event is its own launch (`NSEventTypeAppKitDefined`).
        let said: Vec<&str> =
            said.iter().map(String::as_str).filter(|l| *l != "app type=13").collect();
        assert_eq!(
            said,
            [
                "app type=22 phase=1 momentum=0 dx=0 dy=2 precise=1",
                "app type=22 phase=4 momentum=0 dx=0 dy=6 precise=1",
                "app type=22 phase=8 momentum=0 dx=0 dy=0 precise=1",
                "app type=22 phase=0 momentum=1 dx=0 dy=5 precise=1",
                "app type=22 phase=0 momentum=4 dx=0 dy=3 precise=1",
                "app type=22 phase=0 momentum=8 dx=0 dy=0 precise=1",
                "app type=30 phase=1 amount=0",
                "app type=30 phase=4 amount=0.25",
                "app type=30 phase=8 amount=-0.5",
                "app type=18 phase=1 amount=0",
                "app type=18 phase=4 amount=-12.5",
                "app type=18 phase=16 amount=0",
                "app type=32",
                "app type=31 dx=1 dy=0",
                "app type=31 dx=-1 dy=0",
                "app type=31 dx=0 dy=1",
                "app type=31 dx=0 dy=-1",
            ],
            "a scroll's gesture is AppKit's to read, and no app sees it as an event"
        );
    }

    /// The steps of AppKit's swipe tracking the app reports, until it completes: (amount,
    /// phase) of each.
    fn tracked(app: &App) -> Vec<(f64, u64)> {
        app.until(|line| line.ends_with("complete=1"))
            .iter()
            .filter_map(|line| {
                let rest = line.strip_prefix("track amount=")?;
                let (amount, rest) = rest.split_once(" phase=")?;
                let (phase, _complete) = rest.split_once(' ')?;
                Some((amount.parse().ok()?, phase.parse().ok()?))
            })
            .collect()
    }

    /// How a client's reports reach the worker.
    #[derive(Clone, Copy, Debug)]
    enum Arrivals {
        /// Every 8.3 ms, a trackpad's 120 a second.
        Steady,
        /// Each report 0 to 12 ms late, in order: a network's jitter, the same 120 a second.
        Jittered,
        /// Two at once every 16.7 ms, as a link that batches its packets delivers them.
        Pairs,
    }

    impl Arrivals {
        /// When each of `n` reports arrives, from the first.
        fn times(self, n: u32) -> Vec<Duration> {
            let tick = Duration::from_nanos(8_333_333);
            // xorshift64, seeded, so a run is repeatable.
            let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
            let mut late = move || {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                Duration::from_micros(seed % 12_000)
            };
            let mut at = Duration::ZERO;
            (0..n)
                .map(|k| {
                    let nominal = tick.saturating_mul(k);
                    at = match self {
                        Self::Steady => nominal,
                        Self::Jittered => at.max(nominal.saturating_add(late())),
                        Self::Pairs => tick.saturating_mul(k & !1),
                    };
                    at
                })
                .collect()
        }
    }

    /// A two-finger swipe to the side, the way Safari goes back a page: an app following it as
    /// it moves (`trackSwipeEventWithOptions:`) sees it move and turns the page only when the
    /// scroll comes with its gesture. Without (how a stream starts), the tracking stays at its
    /// first step and is cancelled when the fingers lift. A trackpad's swipe, 10 points a report
    /// at 120 reports a second, stamped by the client at that pace, whether they arrive steadily,
    /// jittered or in pairs: the worker posts each at the client's spacing, which is what the
    /// lift's verdict reads.
    #[test]
    fn a_swipe_between_pages_follows_the_fingers_only_with_their_gesture() {
        const REPORTS: u32 = 12;
        if !live() {
            return;
        }
        // Stamped by the client at its trackpad's pace, whatever the path then does to them.
        let swipe: Vec<ScreenInput> =
            std::iter::once(scroll(0, -10.0, 0.0, ScrollPhase::Began, NONE))
                .chain((1..REPORTS).map(|k| scroll(k, -10.0, 0.0, ScrollPhase::Changed, NONE)))
                .chain(std::iter::once(scroll(REPORTS, 0.0, 0.0, ScrollPhase::Ended, NONE)))
                .collect();
        let lifted = |steps: &[(f64, u64)]| {
            let at = steps.iter().position(|(_, phase)| *phase != 1 && *phase != 4);
            let (following, after) = steps.split_at(at.unwrap_or(steps.len()));
            (following.iter().map(|(amount, _)| *amount).collect::<Vec<_>>(), after.to_vec())
        };

        // Each in an app of its own: one whose tracking was cancelled takes no more events.
        let swiped = |remote: bool, arrivals: Arrivals| {
            let app = App::start();
            if !app.swipe_tracking {
                slopty_testkit::live::skip("two-finger swipe between pages is off");
                return None;
            }
            let mut injector = app.injector();
            injector.inject(&ScreenInput::Gestures { remote }).expect("posted");
            let times = arrivals.times(REPORTS + 1);
            let start = Instant::now();
            for (input, at) in swipe.iter().zip(&times) {
                pace(at.saturating_sub(start.elapsed()));
                injector.inject(input).expect("posted");
            }
            Some(tracked(&app))
        };
        for arrivals in [Arrivals::Steady, Arrivals::Jittered, Arrivals::Pairs] {
            let Some(alone) = swiped(false, arrivals) else { return };
            eprintln!("{arrivals:?}, scroll alone: {alone:?}");
            let (following, after) = lifted(&alone);
            let first = following.first().copied().unwrap_or_default();
            assert!(first.abs() < 0.02, "one report's worth: {following:?}");
            assert!(following.iter().all(|a| (a - first).abs() < 1e-3), "stayed: {following:?}");
            assert_eq!(after.first().map(|(_, phase)| *phase), Some(16), "cancelled on lifting");
            assert!(after.last().is_some_and(|(amount, _)| amount.abs() < 1e-3), "{after:?}");

            // How far the tracking gets before the lift is the display's to say: it reports
            // once a frame, and a guest's frames come unevenly. The lift's verdict is the
            // swipe's own, from its events' spacing. A guest's scheduler can still hold a
            // whole swipe up, so it gets a second go (MEASUREMENTS.md, "a gesture's events
            // keep the client's spacing": 34 of 36 turned at the first).
            let turned = (0..2).find_map(|attempt| {
                let paired = swiped(true, arrivals)?;
                eprintln!("{arrivals:?}, with its gesture (try {attempt}): {paired:?}");
                let (following, after) = lifted(&paired);
                assert!(following.windows(2).all(|w| w[1] <= w[0]), "it follows: {following:?}");
                let ended = after.first().map(|(_, phase)| *phase) == Some(8);
                ended.then(|| after.last().map(|(amount, _)| *amount)).flatten()
            });
            assert!(turned.is_some_and(|amount| amount <= -0.999), "{arrivals:?}: never turned");
        }
    }

    /// What a trackpad scroll costs the input thread to post, alone and with its gesture:
    /// `docs/MEASUREMENTS.md`, "a trackpad scroll's gesture". Every scroll's travel must reach
    /// the app, though AppKit may coalesce two that wait in its queue into one.
    #[test]
    fn a_trackpad_scroll_s_post_cost() {
        const SCROLLS: usize = 600;
        if !live() {
            return;
        }
        let app = App::start();
        let mut injector = app.injector();
        let mut run = |remote: bool| {
            injector.inject(&ScreenInput::Gestures { remote }).expect("posted");
            injector.inject(&scroll(0, 0.0, 1.0, ScrollPhase::Began, NONE)).expect("posted");
            let mut k = 0;
            let mut took: Vec<f64> = std::iter::repeat_with(|| {
                k += 1;
                let from = Instant::now();
                injector.inject(&scroll(k, 0.0, 1.0, ScrollPhase::Changed, NONE)).expect("posted");
                let spent = from.elapsed();
                // A trackpad's pace, 120 a second.
                pace(Duration::from_micros(8_333).saturating_sub(spent));
                spent.as_secs_f64() * 1e6
            })
            .take(SCROLLS)
            .collect();
            injector.inject(&scroll(k + 1, 0.0, 0.0, ScrollPhase::Ended, NONE)).expect("posted");
            took.sort_by(f64::total_cmp);
            let at = |q: f64| took[((took.len() - 1) as f64 * q).round() as usize];
            (at(0.5), at(0.99), at(1.0))
        };
        let alone = run(false);
        let paired = run(true);
        let said = app.post(&mut injector, &[]);
        let travel: f64 = said
            .iter()
            .filter(|l| l.starts_with("app type=22"))
            .filter_map(|l| l.split(" dy=").nth(1)?.split(' ').next()?.parse::<f64>().ok())
            .sum();
        let scrolls = said.iter().filter(|l| l.starts_with("app type=22")).count();
        eprintln!("{scrolls} scroll events of {} posted", 2 * (SCROLLS + 2));
        let posted = 2.0 * (SCROLLS as f64 + 1.0);
        assert!((travel - posted).abs() < 1e-6, "all of it arrived: {travel} of {posted}");
        eprintln!(
            "trackpad scroll post, µs p50 / p99 / max: alone {:.1} / {:.1} / {:.1}, with its \
             gesture {:.1} / {:.1} / {:.1}",
            alone.0, alone.1, alone.2, paired.0, paired.1, paired.2
        );
    }

    /// A regular application in front, which only a guest of the VM lane may start: its titled
    /// window sits at the normal level and would catch the clicks of whoever is at the Mac.
    fn front_app() -> Option<App> {
        if std::env::var_os("SLOPTY_VM").is_none() {
            slopty_testkit::live::skip("a regular app's window in front: the VM lane only");
            return None;
        }
        let app = App::start_with(&["--front"]);
        // A titled window is placed after it is ordered in: wait for its frame to hold still.
        let target = CaptureTarget::Window(WindowId(app.window));
        let mut last = None;
        let deadline = Instant::now().checked_add(Duration::from_secs(5))?;
        while Instant::now() < deadline {
            let now = slopty_capture::target_bounds(target).filter(|b| b.h > 300.0);
            if now.is_some() && now == last {
                return Some(app);
            }
            last = now;
            pace(Duration::from_millis(50));
        }
        panic!("the window never settled: {last:?}");
    }

    /// Make `app` active and its window key as a person would, by a click on its title bar
    /// through the HID tap, which only the guest's pointer takes; `app`'s window must be the one
    /// on top there.
    fn activate_by_title(app: &App) {
        let bounds = slopty_capture::target_bounds(CaptureTarget::Window(WindowId(app.window)))
            .expect("its bounds");
        let title = bounds.h - 300.0;
        let at = CGPoint { x: bounds.x + 100.0, y: bounds.y + title / 2.0 };
        for kind in [CGEventType::MouseMoved, CGEventType::LeftMouseDown, CGEventType::LeftMouseUp]
        {
            let event = Event::Mouse {
                kind,
                at,
                button: CGMouseButton::Left,
                number: 0,
                clicks: i64::from(kind != CGEventType::MouseMoved),
                press: 0,
            };
            let post = Post { route: Route::Hid, flags: CGEventFlags::empty(), event };
            System.post(post).expect("posted");
            pace(Duration::from_millis(30));
        }
        app.until(|line| line == "state active=1 key=1");
    }

    /// What the app says, view lines and state changes included, until the move `post` ends
    /// with, as [`App::post`] reads it.
    fn post_all<B: Backend>(
        app: &App,
        injector: &mut Injector<B>,
        inputs: &[ScreenInput],
    ) -> Vec<String> {
        for input in inputs {
            injector.inject(input).expect("posted");
        }
        injector.inject(&ScreenInput::Move { x: 1.0, y: 1.0 }).expect("posted");
        let mut said = app.until(|line| line.starts_with("app type=5"));
        said.pop();
        said
    }

    /// A press and a release of the left button at the view's point, then a drag from it.
    fn click_and_drag() -> Vec<ScreenInput> {
        let button = |down, x| ScreenInput::Button {
            button: MouseButton::Left,
            down,
            clicks: 1,
            x,
            y: Y,
            mods: Mods::empty(),
        };
        vec![
            button(true, X),
            button(false, X),
            button(true, X),
            ScreenInput::Move { x: X + 10.0, y: Y },
            ScreenInput::Move { x: X + 20.0, y: Y },
            button(false, X + 20.0),
        ]
    }

    /// The view lines among what the app said, without the point's coordinates.
    fn views(said: &[String]) -> Vec<String> {
        said.iter()
            .filter(|l| l.starts_with("view "))
            .map(|l| l.split(" window=").next().unwrap_or(l).to_owned())
            .collect()
    }

    /// Pointer events posted to a regular app's pid reach its view only bound to its window
    /// (`backend::window_binding`): posted to the pid alone they carry window 0 and AppKit hands
    /// them to no view, in front or not, key or not. Bound, a scroll reaches the view of an app
    /// that is not active; a click there is AppKit's click-through question
    /// (`acceptsFirstMouse:`, which this view answers NO) and reaches no view, as a real click
    /// on an inactive window would. Once the app is active and its window key, every press,
    /// drag, release and pinch reaches the view, under the press's one number.
    #[test]
    fn a_window_stream_s_pointer_reaches_a_regular_app_s_view_bound_to_its_window() {
        if !live() {
            return;
        }
        // Someone else's app is the active one, as on a Mac in use; the test's own goes on top.
        let Some(other) = front_app() else { return };
        activate_by_title(&other);
        let Some(app) = front_app() else { return };
        let target = CaptureTarget::Window(WindowId(app.window));
        let bounds = slopty_capture::target_bounds(target).expect("its bounds");
        let mut injector = Injector::with_backend(target, 1.0, Unraised);
        assert_eq!(injector.route(), Route::Pid(app.pid), "posts only to the test's app");
        // The content view is the window's bottom 300 points; its title bar is above it.
        let title = bounds.h - 300.0;
        #[expect(clippy::cast_possible_truncation, reason = "a title bar's height fits f32")]
        let (x, y) = (X, Y + title as f32);
        let at = CGPoint { x: bounds.x + f64::from(x), y: bounds.y + f64::from(y) };
        let unbound = |kind| {
            let event = Event::Mouse {
                kind,
                at,
                button: CGMouseButton::Left,
                number: 0,
                clicks: 1,
                press: 7,
            };
            let post = Post { route: Route::Pid(app.pid), flags: CGEventFlags::empty(), event };
            let built = slopty_input::backend::build(&post, None).expect("built");
            CGEvent::post_to_pid(app.pid, Some(&built));
        };
        let magnify = |delta, phase, time_us| ScreenInput::Magnify { delta, phase, x, y, time_us };
        let in_view = |inputs: Vec<ScreenInput>| {
            inputs
                .into_iter()
                .map(|input| match input {
                    ScreenInput::Button { button, down, clicks, x, mods, .. } => {
                        ScreenInput::Button { button, down, clicks, x, y, mods }
                    }
                    ScreenInput::Move { x, .. } => ScreenInput::Move { x, y },
                    other => other,
                })
                .collect::<Vec<_>>()
        };

        // Not active: unbound, nothing; bound, the scroll, and a click is asked about.
        for kind in [CGEventType::LeftMouseDown, CGEventType::LeftMouseUp] {
            unbound(kind);
        }
        let said = post_all(&app, &mut injector, &[]);
        eprintln!("unbound, inactive: {said:#?}");
        assert!(views(&said).is_empty(), "{said:#?}");
        assert!(said.iter().any(|l| l.contains(" window=0 ")), "window 0: {said:#?}");
        let mut inputs = in_view(click_and_drag());
        inputs.push(ScreenInput::Scroll {
            dx: 0.0,
            dy: 3.0,
            precise: true,
            phase: ScrollPhase::Began,
            momentum: NONE,
            x,
            y,
            mods: Mods::empty(),
            time_us: 0,
        });
        let said = post_all(&app, &mut injector, &inputs);
        eprintln!("bound, inactive: {said:#?}");
        assert_eq!(
            views(&said),
            ["view first_mouse", "view first_mouse", "view scroll dy=3"],
            "{said:#?}"
        );
        assert!(!said.iter().any(|l| l.starts_with("state active=1")), "no click raised it");

        // Made active and key as a person would.
        activate_by_title(&app);

        for kind in [CGEventType::LeftMouseDown, CGEventType::LeftMouseUp] {
            unbound(kind);
        }
        let said = post_all(&app, &mut injector, &[]);
        eprintln!("unbound, active and key: {said:#?}");
        assert!(views(&said).is_empty(), "{said:#?}");

        let mut inputs = in_view(click_and_drag());
        inputs.extend([
            magnify(0.0, ScrollPhase::Began, 0),
            magnify(0.25, ScrollPhase::Changed, REPORT_US),
            magnify(0.0, ScrollPhase::Ended, 2 * REPORT_US),
        ]);
        let said = post_all(&app, &mut injector, &inputs);
        eprintln!("bound, active and key: {said:#?}");
        let seen = views(&said);
        let number = |line: &str| line.split("number=").nth(1).map(str::to_owned);
        let pressed: Vec<&String> = seen.iter().filter(|l| l.contains("number=")).collect();
        let mut kinds: Vec<&str> =
            pressed.iter().map(|l| l.split(" number=").next().unwrap_or(l)).collect();
        // AppKit coalesces the drags that wait in its queue, so the two may come as one.
        kinds.dedup();
        assert_eq!(
            kinds,
            ["view down", "view up", "view down", "view dragged", "view up"],
            "{said:#?}"
        );
        assert_eq!(number(pressed[0]), number(pressed[1]), "a click under one number");
        assert!(pressed[2..].iter().all(|l| number(l) == number(pressed[2])), "{pressed:?}");
        assert_ne!(number(pressed[0]), number(pressed[2]));
        assert_eq!(
            seen.iter().filter(|l| l.starts_with("view magnify")).count(),
            3,
            "the pinch reaches the view: {said:#?}"
        );
    }

    /// From a process in the background, as the worker is, `NSRunningApplication` does not
    /// activate a regular app while another is active: since macOS 14 an app becomes active
    /// only when the active one yields to it. The window server's own process switch, which the
    /// real backend activates with, does, and makes the streamed window key
    /// (`docs/decisions/input.md`, "A window stream's pointer reaches the view").
    #[test]
    fn only_the_window_server_s_switch_activates_from_the_background() {
        use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
        if !live() {
            return;
        }
        let Some(other) = front_app() else { return };
        activate_by_title(&other);
        let Some(app) = front_app() else { return };
        let running = NSRunningApplication::runningApplicationWithProcessIdentifier(app.pid)
            .expect("the app is running");
        let _asked =
            running.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
        let deadline =
            Instant::now().checked_add(Duration::from_millis(1_500)).expect("a deadline");
        let mut said = Vec::new();
        while let Ok(line) =
            app.lines.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            said.push(line);
        }
        eprintln!("asked through NSRunningApplication: {said:#?}");
        assert!(!said.iter().any(|l| l.starts_with("state active=1")), "{said:#?}");

        let started = Instant::now();
        System.activate(app.pid, Some(app.window)).expect("the app is running");
        app.until(|line| line == "state active=1 key=1");
        eprintln!("the window server's switch: active and key in {:?}", started.elapsed());
    }

    /// A window stream of an app in the background, through the real backend: the first press
    /// switches to the app with the streamed window key, so it reaches the view as a click on
    /// an app in front does, with its drag and release, and a key goes to that window.
    #[test]
    fn a_click_on_an_app_in_the_background_reaches_its_view() {
        if !live() {
            return;
        }
        let Some(other) = front_app() else { return };
        activate_by_title(&other);
        let Some(app) = front_app() else { return };
        let target = CaptureTarget::Window(WindowId(app.window));
        let bounds = slopty_capture::target_bounds(target).expect("its bounds");
        let mut injector = Injector::new(target, 1.0);
        assert_eq!(injector.route(), Route::Pid(app.pid), "posts only to the test's app");
        #[expect(clippy::cast_possible_truncation, reason = "a title bar's height fits f32")]
        let y = Y + (bounds.h - 300.0) as f32;
        let button = |down, x| ScreenInput::Button {
            button: MouseButton::Left,
            down,
            clicks: 1,
            x,
            y,
            mods: Mods::empty(),
        };
        let key = |action| ScreenInput::Key {
            code: slopty_proto::input::KeyCode::A,
            action,
            mods: Mods::empty(),
            chord: None,
        };
        let said = post_all(
            &app,
            &mut injector,
            &[
                button(true, X),
                ScreenInput::Move { x: X + 10.0, y },
                button(false, X + 10.0),
                key(slopty_proto::input::KeyAction::Press),
                key(slopty_proto::input::KeyAction::Release),
            ],
        );
        eprintln!("{said:#?}");
        let mut seen: Vec<String> = views(&said)
            .into_iter()
            .map(|l| l.split(" number=").next().unwrap_or(&l).to_owned())
            .collect();
        seen.dedup();
        assert_eq!(seen, ["view down", "view dragged", "view up"], "{said:#?}");
        assert!(said.iter().any(|l| l == "state active=1 key=1"), "{said:#?}");
        let keys =
            said.iter().filter(|l| l.starts_with("app type=10") || l.starts_with("app type=11"));
        assert_eq!(keys.count(), 2, "a key down and up reached it: {said:#?}");
    }
}
