//! The drag-and-drop spikes (`docs/decisions/audio.md`, "Drag and drop lands at the point, both
//! ways", P0): what a real drag on a Mac does when HID events drive it, which the design stands
//! or falls on. They stay as regression checks of that platform behaviour.
//!
//! They move the one real pointer and post HID events, so they run only in a macOS guest
//! (`SLOPTY_DND_E2E=1` and `SLOPTY_VM=1`, both set by the lane) and never on a Mac someone uses:
//! `cargo xtask vm live -p slopty-dnd --test spikes`. Every drag starts in the test's own
//! `slopty-drag-source` and ends on its own `slopty-drop-target` (`tests/support/`); nothing
//! else is posted to. A spike passes or fails on what those apps say they got, and prints every
//! line both said as its evidence.

#[cfg(test)]
#[cfg(target_os = "macos")]
#[path = "support/harness.rs"]
mod harness;

#[cfg(test)]
#[cfg(target_os = "macos")]
#[expect(clippy::cast_possible_truncation, reason = "a live test's points")]
mod spikes {
    use std::process::Command;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use objc2_app_kit::{NSPasteboard, NSPasteboardNameDrag, NSPasteboardTypeFileURL};
    use objc2_core_foundation::CGPoint;
    use objc2_core_graphics::{
        CGEvent, CGEventField, CGEventFlags, CGEventType, CGMainDisplayID, CGMouseButton,
    };
    use slopty_capture::{Picture, Shareable, Target};
    use slopty_core::{DisplayId, WindowId};
    use slopty_input::{Backend as _, Event, Injector, Post, Route, System};
    use slopty_proto::input::{Mods, MouseButton};
    use slopty_proto::screen::{CaptureTarget, ScreenInput};

    use crate::harness::{App, Hand, Numbers, at_arg, centre, file, lerp, live, ms, pace};

    /// Where the apps' windows go, in global points from the main display's top left.
    const SOURCE: (f64, f64, f64, f64) = (60.0, 60.0, 60.0, 60.0);
    const ACCEPTS: (f64, f64, f64, f64) = (240.0, 160.0, 280.0, 200.0);
    const REFUSES: (f64, f64, f64, f64) = (600.0, 160.0, 280.0, 200.0);
    /// `kVK_Escape`.
    const ESCAPE: u16 = 53;
    /// The press number spike 4 posts under, as the injector would give one press.
    const PID_PRESS: i64 = 4242;

    /// Press on the source's window, drag onto `over`, rest there `rest`, release; the last
    /// line the source said about the session's end.
    fn drop_on(
        hand: &mut Hand,
        source: &mut App,
        over: (f64, f64),
        rest: Duration,
    ) -> Option<String> {
        hand.to(centre(SOURCE));
        pace(ms(100));
        hand.button(true);
        pace(ms(50));
        hand.glide(over);
        pace(rest);
        hand.button(false);
        source.wait(Duration::from_secs(5), |l| l.starts_with("ended"));
        source.since(0, "ended").pop()
    }

    /// (1) A press posted through the HID tap into the helper's window, an accessory app's that
    /// is not active, begins a session there, and its release over the test's drop target
    /// delivers the file's URL at that point.
    #[test]
    #[ignore = "live: moves the real pointer; cargo xtask vm live -p slopty-dnd --test spikes"]
    fn p0_1_an_hid_press_into_the_helpers_window_drops_at_the_point() {
        if !live() {
            return;
        }
        let (_keep, path) = file();
        let at = at_arg(SOURCE);
        let mut source = App::source(&[
            "--at",
            &at,
            "--begin",
            "down",
            "--first-mouse",
            "1",
            "--file",
            &path,
            "--image",
            "clear",
            "--ignore",
            "1",
            "--level",
            "popup",
        ]);
        let mut target = App::target(ACCEPTS, "copy", &[]);
        let active = System.is_active(source.pid);
        let mut hand = Hand::new(Numbers::Injector);
        let ended = drop_on(&mut hand, &mut source, centre(ACCEPTS), ms(300));
        target.wait(Duration::from_secs(5), |l| l.starts_with("file "));
        source.show();
        target.show();
        eprintln!("source active before the press: {active}");
        assert!(!active, "the helper is never made active");
        assert!(source.any("began"), "the session began");
        assert!(target.any("entered"), "the drag entered the target");
        let url = format!("file://{path}");
        assert!(
            target.since(0, "perform").iter().any(|l| l.contains(&url)),
            "the target got the file's URL"
        );
        assert!(target.any(&format!("file path={path} exists=1")), "the URL names the file");
        assert_eq!(ended.as_deref().and_then(|l| l.split(' ').nth(1)), Some("op=1"), "a copy");
    }

    /// (1b) The same press with `acceptsFirstMouse:` answering NO still reaches the view of an
    /// accessory app that is not active, and begins the drag: on macOS 26 the helper does not
    /// depend on its answer, which stays YES.
    #[test]
    #[ignore = "live: moves the real pointer; cargo xtask vm live -p slopty-dnd --test spikes"]
    fn p0_1b_an_inactive_accessory_window_takes_the_first_press_without_first_mouse() {
        if !live() {
            return;
        }
        let (_keep, path) = file();
        let at = at_arg(SOURCE);
        let mut source = App::source(&[
            "--at",
            &at,
            "--begin",
            "down",
            "--first-mouse",
            "0",
            "--file",
            &path,
            "--image",
            "clear",
            "--level",
            "popup",
        ]);
        let mut target = App::target(ACCEPTS, "copy", &[]);
        let mut hand = Hand::new(Numbers::Injector);
        let _ended = drop_on(&mut hand, &mut source, centre(ACCEPTS), ms(300));
        target.wait(ms(1500), |l| l.starts_with("perform"));
        source.show();
        target.show();
        let reached = source.any("view down");
        let began = source.any("began");
        eprintln!("spike 1b: press reached the view={reached} session began={began}");
        assert!(reached && began, "the first press reaches the view and begins the drag");
    }

    /// (2) While the drag is over a target, the system cursor is the copy cursor over one that
    /// accepts and not the copy cursor over one that refuses, read as the worker reads it.
    #[test]
    #[ignore = "live: moves the real pointer; cargo xtask vm live -p slopty-dnd --test spikes"]
    fn p0_2_the_system_cursor_shows_the_targets_answer() {
        if !live() {
            return;
        }
        let (_keep, path) = file();
        let at = at_arg(SOURCE);
        let mut source =
            App::source(&["--at", &at, "--begin", "dragged", "--file", &path, "--image", "clear"]);
        let mut accepts = App::target(ACCEPTS, "copy", &[]);
        let mut refuses = App::target(REFUSES, "none", &[]);
        let mut hand = Hand::new(Numbers::Injector);
        hand.to(centre(SOURCE));
        pace(ms(100));
        hand.button(true);
        pace(ms(50));
        hand.glide(centre(ACCEPTS));
        pace(ms(600));
        // The class the cursor has after the rest: the last change the source saw.
        let cursor_accepts = source.since(0, "cursor").pop();
        hand.glide(centre(REFUSES));
        pace(ms(600));
        let cursor_refuses = source.since(0, "cursor").pop();
        hand.glide(centre(ACCEPTS));
        pace(ms(300));
        hand.button(false);
        source.wait(Duration::from_secs(5), |l| l.starts_with("ended"));
        source.show();
        accepts.show();
        refuses.show();
        eprintln!("spike 2: over accepting {cursor_accepts:?}, over refusing {cursor_refuses:?}");
        assert!(refuses.any("entered"), "the drag was over the refusing target");
        let class = |line: &Option<String>| {
            line.as_deref().and_then(|l| l.split(' ').nth(1)).map(str::to_owned)
        };
        assert_eq!(class(&cursor_accepts).as_deref(), Some("class=copy"), "copy over an accept");
        assert_eq!(
            class(&cursor_refuses).as_deref(),
            Some("class=arrow"),
            "the arrow over a refusal"
        );
    }

    /// (3) Escape posted through the HID tap mid-drag ends the session with no operation, and the
    /// release after it drops nothing.
    #[test]
    #[ignore = "live: moves the real pointer; cargo xtask vm live -p slopty-dnd --test spikes"]
    fn p0_3_escape_ends_the_drag_with_none() {
        if !live() {
            return;
        }
        let (_keep, path) = file();
        let at = at_arg(SOURCE);
        let mut source = App::source(&["--at", &at, "--begin", "dragged", "--file", &path]);
        let mut target = App::target(ACCEPTS, "copy", &[]);
        let mut hand = Hand::new(Numbers::Injector);
        hand.to(centre(SOURCE));
        pace(ms(100));
        hand.button(true);
        pace(ms(50));
        hand.glide(centre(ACCEPTS));
        pace(ms(300));
        Hand::key(ESCAPE, true);
        Hand::key(ESCAPE, false);
        let escaped = source.wait(Duration::from_secs(2), |l| l.starts_with("ended"));
        pace(ms(200));
        hand.button(false);
        source.wait(Duration::from_secs(3), |l| l.starts_with("ended"));
        target.wait(ms(500), |l| l.starts_with("perform"));
        source.show();
        target.show();
        eprintln!("spike 3: the session ended on the escape, before the release: {escaped}");
        let ended = source.since(0, "ended").pop();
        assert_eq!(
            ended.as_deref().and_then(|l| l.split(' ').nth(1)),
            Some("op=0"),
            "no operation"
        );
        assert!(!target.any("perform"), "nothing dropped");
    }

    /// How spike 4 posts its press and first drags to the source's pid.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum PidRoute {
        /// A window stream as the worker posts one today: its injector, which activates the owner
        /// before a press, names no window, and keeps the pointer inside the window.
        Injector,
        /// Raw posts that name the source's window in `kCGMouseEventWindowUnderMousePointer` and
        /// `…ThatCanHandleThisEvent`, the app left inactive, going on past the window's edge.
        Named,
        /// The injector again, on a window with a title bar, which can become key when its app is
        /// activated, as an app's document window can.
        InjectorTitled,
    }

    /// Post a left-button event to `pid`, under press `press`, naming `window` if given.
    fn to_pid(pid: i32, window: Option<u32>, kind: CGEventType, (x, y): (f64, f64), press: i64) {
        let event = Event::Mouse {
            kind,
            at: CGPoint { x, y },
            button: CGMouseButton::Left,
            number: 0,
            clicks: i64::from(kind != CGEventType::LeftMouseDragged),
            press,
        };
        let post = Post { route: Route::Pid(pid), flags: CGEventFlags::empty(), event };
        let built = slopty_input::backend::build(&post, None).expect("built");
        if let Some(window) = window {
            for field in [
                CGEventField::MouseEventWindowUnderMousePointer,
                CGEventField::MouseEventWindowUnderMousePointerThatCanHandleThisEvent,
            ] {
                CGEvent::set_integer_value_field(Some(&built), field, i64::from(window));
            }
        }
        CGEvent::post_to_pid(pid, Some(&built));
    }

    /// Post a left-button event through the HID tap under press `press`.
    fn to_hid(kind: CGEventType, (x, y): (f64, f64), press: i64) {
        let event = Event::Mouse {
            kind,
            at: CGPoint { x, y },
            button: CGMouseButton::Left,
            number: 0,
            clicks: i64::from(kind != CGEventType::LeftMouseDragged),
            press,
        };
        System
            .post(Post { route: Route::Hid, flags: CGEventFlags::empty(), event })
            .expect("posted");
    }

    /// What [`pid_drag`] saw.
    #[derive(Debug)]
    struct PidOutcome {
        reached_view: bool,
        began: bool,
        cancelled: bool,
        pointer_moved: bool,
        dropped: bool,
    }

    /// Try to begin a drag in the source by pid-posted events on `route`, then carry the same press
    /// on through the HID tap to the drop target and release it there. Returns whether the press
    /// reached the view, whether a session began, whether it ended with nothing (op 0), whether the
    /// real pointer moved, and whether the target got a drop; it prints the `moved` lines by
    /// pid and by HID.
    fn pid_drag(route: PidRoute) -> PidOutcome {
        let (_keep, path) = file();
        let at = at_arg(SOURCE);
        let titled = if route == PidRoute::InjectorTitled { "1" } else { "0" };
        let mut source =
            App::source(&["--at", &at, "--begin", "dragged", "--file", &path, "--titled", titled]);
        let mut target = App::target(ACCEPTS, "copy", &[]);
        // Park the real pointer away from everything first.
        let mut hand = Hand::new(Numbers::Injector);
        hand.to((20.0, 500.0));
        pace(ms(100));
        let parked = slopty_capture::pointer_location();
        let start = centre(SOURCE);
        let edge = (SOURCE.0 + SOURCE.2 - 1.0, SOURCE.1 + SOURCE.3 - 1.0);
        let halfway = lerp(start, centre(ACCEPTS), 0.5);
        let reached_by_pid = match route {
            PidRoute::Injector | PidRoute::InjectorTitled => {
                let target = CaptureTarget::Window(WindowId(source.window));
                // Stream pixels of a 1:1 window stream are points from the window's top left,
                // its title bar included.
                let bounds = slopty_capture::target_bounds(target).expect("its bounds");
                let mut injector = Injector::new(target, 1.0);
                assert_eq!(
                    injector.route(),
                    Route::Pid(source.pid),
                    "posts only to the test's app"
                );
                let local = |(x, y): (f64, f64)| ((x - bounds.x) as f32, (y - bounds.y) as f32);
                let (x, y) = local(start);
                let button = |down| ScreenInput::Button {
                    button: MouseButton::Left,
                    down,
                    clicks: 1,
                    x,
                    y,
                    mods: Mods::empty(),
                };
                injector.inject(&button(true)).expect("posted");
                pace(ms(50));
                for step in 1..=12_u32 {
                    let (x, y) = local(lerp(start, edge, f64::from(step) / 12.0));
                    injector.inject(&ScreenInput::Move { x, y }).expect("posted");
                    pace(ms(8));
                }
                edge
            }
            PidRoute::Named => {
                let window = Some(source.window);
                to_pid(source.pid, window, CGEventType::LeftMouseDown, start, PID_PRESS);
                pace(ms(50));
                for step in 1..=12_u32 {
                    let at = lerp(start, halfway, f64::from(step) / 12.0);
                    to_pid(source.pid, window, CGEventType::LeftMouseDragged, at, PID_PRESS);
                    pace(ms(8));
                }
                halfway
            }
        };
        pace(ms(300));
        let reached_view = source.any("view down") || source.any("view dragged");
        let began_by_pid = source.any("began");
        let moved_by_pid = source.since(0, "moved").len();
        let pointer_by_pid = slopty_capture::pointer_location();
        let pointer_moved =
            (pointer_by_pid.0 - parked.0).abs() > 1.0 || (pointer_by_pid.1 - parked.1).abs() > 1.0;
        // The press's own number, as the source saw it, carried on through the HID tap.
        let press = source
            .since(0, "event type=1 ")
            .first()
            .and_then(|l| l.split(" number=").nth(1)?.split(' ').next()?.parse().ok())
            .unwrap_or(PID_PRESS);
        let hid = source.mark();
        for step in 1..=24_u32 {
            let at = lerp(reached_by_pid, centre(ACCEPTS), f64::from(step) / 24.0);
            to_hid(CGEventType::LeftMouseDragged, at, press);
            pace(ms(8));
        }
        pace(ms(300));
        let moved_by_hid = source.since(hid, "moved").len();
        to_hid(CGEventType::LeftMouseUp, centre(ACCEPTS), press);
        source.wait(Duration::from_secs(3), |l| l.starts_with("ended"));
        target.wait(ms(1500), |l| l.starts_with("perform"));
        let cancelled_by_pid = source.any("ended op=0");
        source.show();
        target.show();
        let dropped = target.any("perform");
        eprintln!(
            "spike 4 {route:?}: by pid: reached_view={reached_view} began={began_by_pid} \
             cancelled={cancelled_by_pid} moved_lines={moved_by_pid} pointer {parked:?} -> {pointer_by_pid:?}; then by HID \
             under press {press}: moved_lines={moved_by_hid} dropped={dropped}"
        );
        PidOutcome {
            reached_view,
            began: began_by_pid,
            cancelled: cancelled_by_pid,
            pointer_moved,
            dropped,
        }
    }

    /// (4a) and (4c): the press reaches the view and begins a session, which ends with nothing; the
    /// real pointer stays put; nothing drops.
    fn injector_route_begins_and_cancels(outcome: &PidOutcome) {
        assert!(outcome.reached_view && outcome.began, "the press reaches the view and begins");
        assert!(outcome.cancelled, "the session ends with nothing");
        assert!(!outcome.pointer_moved, "the pid route leaves the real pointer where it was");
        assert!(!outcome.dropped, "and carrying the press on through the HID tap drops nothing");
    }

    /// (4a) A window stream's route as the worker has it, the injector's press and drags posted to
    /// the owner's pid, now reaches the view (the injector names the window since the wire
    /// lane's pointer change), and the view begins a session; but the drag manager follows the
    /// real pointer, which stays put, so the session ends with nothing (op 0), at once or during
    /// the HID moves that carry the press on, and nothing drops. Drags into and out of a
    /// streamed app take the HID route.
    #[test]
    #[ignore = "live: moves the real pointer; cargo xtask vm live -p slopty-dnd --test spikes"]
    fn p0_4a_a_drag_under_the_injectors_window_route() {
        if !live() {
            return;
        }
        let outcome = pid_drag(PidRoute::Injector);
        eprintln!("spike 4a: {outcome:?}");
        injector_route_begins_and_cancels(&outcome);
    }

    /// (4b) The same with raw pid posts that name the source's window in fields 91 and 92, which is
    /// left inactive: still no view gets them.
    #[test]
    #[ignore = "live: moves the real pointer; cargo xtask vm live -p slopty-dnd --test spikes"]
    fn p0_4b_a_drag_under_pid_posts_that_name_the_window() {
        if !live() {
            return;
        }
        let outcome = pid_drag(PidRoute::Named);
        eprintln!("spike 4b: {outcome:?}");
        let PidOutcome { reached_view, began, pointer_moved, dropped, .. } = outcome;
        assert!(
            !reached_view && !began,
            "pid-posted presses never reach the view, so no drag begins"
        );
        assert!(!pointer_moved, "the pid route leaves the real pointer where it was");
        assert!(!dropped, "and carrying the press on through the HID tap does not start one");
    }

    /// (4c) The injector's window route on a window with a title bar, which its app's activation
    /// can make key: the same as (4a).
    #[test]
    #[ignore = "live: moves the real pointer; cargo xtask vm live -p slopty-dnd --test spikes"]
    fn p0_4c_a_drag_under_the_injectors_window_route_on_a_titled_window() {
        if !live() {
            return;
        }
        let outcome = pid_drag(PidRoute::InjectorTitled);
        eprintln!("spike 4c: {outcome:?}");
        injector_route_begins_and_cancels(&outcome);
    }

    /// (5) The drag pasteboard's change count moves when an app begins a drag, and not on a click;
    /// its items then name the dragged file.
    #[test]
    #[ignore = "live: moves the real pointer; cargo xtask vm live -p slopty-dnd --test spikes"]
    fn p0_5_the_drag_pasteboard_moves_when_a_drag_begins() {
        if !live() {
            return;
        }
        let (_keep, path) = file();
        let at = at_arg(SOURCE);
        let mut source = App::source(&["--at", &at, "--begin", "dragged", "--file", &path]);
        let mut target = App::target(ACCEPTS, "copy", &[]);
        // SAFETY: framework-provided constant string.
        let board = NSPasteboard::pasteboardWithName(unsafe { NSPasteboardNameDrag });
        let before = board.changeCount();
        let mut hand = Hand::new(Numbers::Injector);
        hand.to(centre(SOURCE));
        pace(ms(100));
        hand.button(true);
        pace(ms(50));
        hand.button(false);
        pace(ms(200));
        let after_click = board.changeCount();
        hand.button(true);
        pace(ms(50));
        hand.glide(centre(ACCEPTS));
        source.wait(Duration::from_secs(2), |l| l.starts_with("began"));
        let polled = Instant::now();
        let mut after_drag = board.changeCount();
        while after_drag == after_click && polled.elapsed() < Duration::from_secs(1) {
            pace(ms(8));
            after_drag = board.changeCount();
        }
        // SAFETY: framework-provided constant string.
        let file_url = unsafe { NSPasteboardTypeFileURL };
        let named: Vec<String> = board
            .pasteboardItems()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.stringForType(file_url))
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();
        let types: Vec<String> =
            board.types().map(|t| t.iter().map(|t| t.to_string()).collect()).unwrap_or_default();
        pace(ms(200));
        hand.button(false);
        source.wait(Duration::from_secs(3), |l| l.starts_with("ended"));
        source.show();
        target.show();
        eprintln!(
            "spike 5: count {before} -> {after_click} after a click -> {after_drag} after a drag; \
             types {types:?}; file URLs {named:?}"
        );
        assert_eq!(after_click, before, "a click leaves the drag pasteboard alone");
        assert!(after_drag > after_click, "a drag moves its count");
        assert!(named.contains(&format!("file://{path}")), "its items name the file");
    }

    /// (6) Whether the press, its drags and its release need one `kCGMouseEventNumber` for a
    /// drag to begin and drop: the injector's number per press, 0 on every event, and the field
    /// left to CoreGraphics.
    #[test]
    #[ignore = "live: moves the real pointer; cargo xtask vm live -p slopty-dnd --test spikes"]
    fn p0_6_a_drag_under_each_numbering() {
        if !live() {
            return;
        }
        let mut results = Vec::new();
        for numbers in [Numbers::Injector, Numbers::Zero, Numbers::Unset] {
            let (_keep, path) = file();
            let at = at_arg(SOURCE);
            let mut source = App::source(&["--at", &at, "--begin", "dragged", "--file", &path]);
            let mut target = App::target(ACCEPTS, "copy", &[]);
            let mut hand = Hand::new(numbers);
            let ended = drop_on(&mut hand, &mut source, centre(ACCEPTS), ms(300));
            target.wait(Duration::from_secs(2), |l| l.starts_with("perform"));
            eprintln!("=== numbering {numbers:?}");
            source.show();
            target.show();
            let numbers_seen: Vec<String> = source
                .since(0, "event ")
                .iter()
                .filter_map(|l| l.split(" number=").nth(1)?.split(' ').next().map(str::to_owned))
                .collect();
            results.push((
                numbers,
                source.any("began"),
                target.any("perform"),
                ended.unwrap_or_default(),
                numbers_seen,
            ));
        }
        for (numbers, began, dropped, ended, seen) in &results {
            eprintln!(
                "spike 6: {numbers:?}: began={began} dropped={dropped} {ended} numbers={seen:?}"
            );
        }
        let injector = &results[0];
        assert!(injector.1 && injector.2, "the injector's numbering drags and drops");
    }

    /// Take one picture of `target` (a window alone, or a display).
    fn picture(target: CaptureTarget) -> Picture {
        let (tx, rx) = mpsc::channel();
        slopty_capture::enumerate(move |content: Result<Shareable, _>| {
            let _sent = tx.send(content);
        });
        let content =
            rx.recv_timeout(Duration::from_secs(10)).expect("content").expect("shareable");
        let target = Target::resolve(&content, target).expect("the target");
        let (tx, rx) = mpsc::channel();
        target.snapshot(move |picture| {
            let _sent = tx.send(picture);
        });
        rx.recv_timeout(Duration::from_secs(10)).expect("a picture").expect("captured")
    }

    /// The pixels of `picture` that are the drag image's magenta.
    fn magenta(picture: &Picture) -> usize {
        let width = usize::try_from(picture.width).unwrap();
        picture
            .data
            .chunks(picture.bytes_per_row)
            .flat_map(|row| row[..width.saturating_mul(4)].as_chunks::<4>().0.iter())
            .filter(|[b, g, r, _]| *r > 200 && *g < 60 && *b > 200)
            .count()
    }

    /// (7) A picture of the window under a drag, taken as a window stream takes one, holds none
    /// of the drag image; a picture of the display does.
    #[test]
    #[ignore = "live: moves the real pointer; cargo xtask vm live -p slopty-dnd --test spikes"]
    fn p0_7_a_window_stream_leaves_the_drag_image_out() {
        if !live() {
            return;
        }
        let (_keep, path) = file();
        let at = at_arg(SOURCE);
        let mut source = App::source(&[
            "--at", &at, "--begin", "dragged", "--file", &path, "--image", "magenta",
        ]);
        let mut target = App::target(ACCEPTS, "copy", &[]);
        let mut hand = Hand::new(Numbers::Injector);
        hand.to(centre(SOURCE));
        pace(ms(100));
        hand.button(true);
        pace(ms(50));
        hand.glide(centre(ACCEPTS));
        pace(ms(400));
        let window = picture(CaptureTarget::Window(WindowId(target.window)));
        let display = picture(CaptureTarget::Display(DisplayId(CGMainDisplayID())));
        hand.button(false);
        source.wait(Duration::from_secs(3), |l| l.starts_with("ended"));
        source.show();
        target.show();
        let (in_window, in_display) = (magenta(&window), magenta(&display));
        eprintln!(
            "spike 7: magenta pixels in the window's picture {in_window} ({}x{}), in the display's {in_display} ({}x{})",
            window.width, window.height, display.width, display.height
        );
        assert!(in_display > 1000, "the display's picture shows the drag image");
        assert_eq!(in_window, 0, "the window's picture does not");
    }

    /// (8a) A destination that reads the drag pasteboard half a second after the drop, as one
    /// that loads its items later does, finds the same file URL there.
    #[test]
    #[ignore = "live: moves the real pointer; cargo xtask vm live -p slopty-dnd --test spikes"]
    fn p0_8a_a_late_read_of_the_drag_pasteboard_finds_the_file() {
        if !live() {
            return;
        }
        let (_keep, path) = file();
        let at = at_arg(SOURCE);
        let mut source = App::source(&["--at", &at, "--begin", "dragged", "--file", &path]);
        let mut target = App::target(ACCEPTS, "copy", &[]);
        let mut hand = Hand::new(Numbers::Injector);
        let ended = drop_on(&mut hand, &mut source, centre(ACCEPTS), ms(300));
        target.wait(Duration::from_secs(2), |l| l.starts_with("later"));
        source.show();
        target.show();
        let url = format!("file://{path}");
        let late = target.since(0, "later").iter().any(|l| l.contains(&url));
        eprintln!("spike 8a: late read found the file={late} {ended:?}");
        assert!(late, "the drag pasteboard still names the file after the drop");
    }

    /// What `defaults read -g <key>` says in this session.
    fn global_default(key: &str) -> String {
        Command::new("/usr/bin/defaults").args(["read", "-g", key]).output().map_or_else(
            |e| format!("unreadable: {e}"),
            |out| {
                let said = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                if said.is_empty() { "unset".to_owned() } else { said }
            },
        )
    }

    /// (8b) A drag held over a spring-loaded target springs it once, after resting, the pointer
    /// moves a point either side every 100 ms. Resting still for 2.5 s did not, on macOS 26.6,
    /// which the test prints. How long the rest must be is `tests/roles.rs`'s
    /// `which_nudge_periods_spring_a_target`.
    #[test]
    #[ignore = "live: moves the real pointer; cargo xtask vm live -p slopty-dnd --test spikes"]
    fn p0_8b_a_drag_held_over_a_spring_loaded_target() {
        if !live() {
            return;
        }
        let enabled = global_default("com.apple.springing.enabled");
        let delay = global_default("com.apple.springing.delay");
        let (_keep, path) = file();
        let at = at_arg(SOURCE);
        let mut source = App::source(&["--at", &at, "--begin", "dragged", "--file", &path]);
        let mut target = App::target(ACCEPTS, "copy", &["--spring", "1"]);
        let mut hand = Hand::new(Numbers::Injector);
        hand.to(centre(SOURCE));
        pace(ms(100));
        hand.button(true);
        pace(ms(50));
        hand.glide(centre(ACCEPTS));
        pace(ms(2500));
        let sprang_resting = target.any("spring activated=1");
        let (x, y) = centre(ACCEPTS);
        for step in 0..30_u32 {
            let nudge = if step % 2 == 0 { 1.0 } else { -1.0 };
            hand.to((x + nudge, y));
            pace(ms(100));
        }
        let sprang_nudged = target.any("spring activated=1");
        hand.button(false);
        source.wait(Duration::from_secs(3), |l| l.starts_with("ended"));
        source.show();
        target.show();
        eprintln!(
            "spike 8b: springing enabled={enabled} delay={delay}; sprang while resting={sprang_resting}, \
             after 3 s of moves a point either side={sprang_nudged}"
        );
        assert!(target.any("spring entered"), "the target was offered spring loading");
        assert!(sprang_nudged, "a held drag that keeps making 1-point moves springs the target");
    }
}
