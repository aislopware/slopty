//! The drag helper's roles on a real desktop (`docs/decisions/audio.md`, "Drag and drop lands at
//! the point, both ways", P1's platform pieces): `slopty_dnd`'s source dropping the client's
//! items at a point, its promises answered as a target reads them, a resting drag nudged until a
//! spring-loaded target springs, and a drag out of an app seen on the drag pasteboard and caught
//! with its files and promises.
//!
//! Like the spikes they move the one real pointer through the HID tap, so they run only in a
//! macOS guest (`SLOPTY_DND_E2E=1` and `SLOPTY_VM=1`, set by the lane): `cargo xtask vm live -p
//! slopty-dnd --test roles`. Every drag starts and ends in the test's own apps
//! (`tests/support/`); nothing else is posted to. Each passes or fails on what those apps say.

#[cfg(test)]
#[cfg(target_os = "macos")]
#[path = "support/harness.rs"]
mod harness;

#[cfg(test)]
#[cfg(target_os = "macos")]
mod roles {
    use std::time::{Duration, Instant};

    use slopty_dnd::nudge::{self, Nudge};
    use slopty_dnd::watch::DragWatch;

    use crate::harness::{App, Hand, Numbers, at_arg, centre, file, live, ms, pace};

    /// Where the helper's source waits: the point the client's drag entered.
    const SOURCE_AT: (f64, f64) = (90.0, 90.0);
    /// The test's drop target, in global points from the main display's top left.
    const TARGET: (f64, f64, f64, f64) = (240.0, 160.0, 280.0, 200.0);
    /// The window an app on the worker drags out of.
    const APP: (f64, f64, f64, f64) = (60.0, 60.0, 60.0, 60.0);
    /// Where the catcher waits: where the client's pointer left the tile.
    const CATCH_AT: (f64, f64) = (700.0, 300.0);

    fn point((x, y): (f64, f64)) -> String {
        format!("{x},{y}")
    }

    fn helper(args: &[&str]) -> App {
        App::start("slopty-dnd-helper", args)
    }

    /// Press into the helper's source and carry the drag onto `over`, still held.
    fn drag_to(hand: &mut Hand, over: (f64, f64)) {
        hand.to(SOURCE_AT);
        pace(ms(100));
        hand.button(true);
        pace(ms(50));
        hand.glide(over);
    }

    /// Press into the helper's source, carry the drag onto `over`, rest `rest`, release.
    fn drag_from_source(hand: &mut Hand, over: (f64, f64), rest: Duration) {
        drag_to(hand, over);
        pace(rest);
        hand.button(false);
    }

    /// A drop from the client: a file whole on the worker, text and a file still arriving, both
    /// promised. The target gets all three at the point, the late file whole at its path when
    /// it reads it, and the source hears a copy.
    #[test]
    #[ignore = "live: moves the real pointer; cargo xtask vm live -p slopty-dnd --test roles"]
    fn the_source_drops_a_file_text_and_a_late_file_at_the_point() {
        if !live() {
            return;
        }
        let (_keep, whole) = file();
        let dir = tempfile::tempdir().unwrap();
        let late = dir.path().canonicalize().unwrap().join("arrived at the drop.bin");
        let late_spec = format!("{}:4096:300", late.display());
        let at = point(SOURCE_AT);
        let mut source = helper(&[
            "--role",
            "source",
            "--at",
            &at,
            "--file",
            &whole,
            "--later-text",
            "dropped words",
            "--later-file",
            &late_spec,
        ]);
        let mut target = App::target(TARGET, "copy", &[]);
        let mut hand = Hand::new(Numbers::Injector);
        drag_from_source(&mut hand, centre(TARGET), ms(300));
        let late_line = format!("file path={} exists=1 size=4096", late.display());
        let landed = target.wait(Duration::from_secs(10), |l| l == late_line);
        source.wait(Duration::from_secs(5), |l| l.starts_with("ended"));
        source.show();
        target.show();
        assert!(source.any("began"), "the press began the helper's session");
        let perform = target.since(0, "perform").pop().unwrap_or_default();
        assert!(perform.contains(&format!("file://{whole}")), "the whole file's URL: {perform}");
        assert!(perform.contains("dropped words"), "the promised text: {perform}");
        assert!(landed, "the late file was whole at its path when the target read it");
        assert!(target.any(&format!("file path={whole} exists=1")), "the whole file");
        assert!(source.any("provide item=1 type=public.utf8-plain-text"), "text on read");
        assert!(source.any("provide item=2 type=public.file-url"), "the late file on read");
        assert!(source.any("ended op=1"), "a copy");
    }

    /// A file still arriving when the target reads it holds the target until it is whole: how
    /// long a drop may wait on the worker's upload before a target gives up (the design's 5 s
    /// provider). Prints how long the target waited and whether it took the file.
    #[test]
    #[ignore = "live: moves the real pointer; cargo xtask vm live -p slopty-dnd --test roles"]
    fn a_target_waits_five_seconds_for_a_file_still_arriving() {
        if !live() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let late = dir.path().canonicalize().unwrap().join("slow upload.bin");
        let late_spec = format!("{}:1048576:5000", late.display());
        let at = point(SOURCE_AT);
        let mut source = helper(&["--role", "source", "--at", &at, "--later-file", &late_spec]);
        let mut target = App::target(TARGET, "copy", &[]);
        let mut hand = Hand::new(Numbers::Injector);
        drag_from_source(&mut hand, centre(TARGET), ms(300));
        let released = Instant::now();
        let late_line = format!("file path={} exists=1 size=1048576", late.display());
        let landed = target.wait(Duration::from_secs(30), |l| l == late_line);
        let waited = released.elapsed();
        source.wait(Duration::from_secs(10), |l| l.starts_with("ended"));
        source.show();
        target.show();
        eprintln!(
            "MEASURE dnd slow provider 5000 ms: release → target has the file {} ms, landed={landed}",
            waited.as_millis()
        );
        assert!(landed, "the target waited for the file and took it whole");
        assert!(source.any("ended op=1"), "and the drag ended as a copy");
    }

    /// A drag resting over a spring-loaded target, kept moving by [`Nudge`] as the worker keeps
    /// it, springs the target open.
    #[test]
    #[ignore = "live: moves the real pointer; cargo xtask vm live -p slopty-dnd --test roles"]
    fn a_nudged_resting_drag_springs_the_target() {
        if !live() {
            return;
        }
        let (_keep, whole) = file();
        let at = point(SOURCE_AT);
        let mut source = helper(&["--role", "source", "--at", &at, "--file", &whole]);
        let mut target = App::target(TARGET, "copy", &["--spring", "1"]);
        let mut hand = Hand::new(Numbers::Injector);
        let over = centre(TARGET);
        drag_to(&mut hand, over);
        let started = Instant::now();
        let now_us = || u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let rest = nudge::rest_us(nudge::spring_delay_s());
        let mut nudge = Nudge::new(over, now_us(), rest);
        let mut nudges = 0_u32;
        let mut sprung_ms = None;
        while started.elapsed() < Duration::from_secs(4) && sprung_ms.is_none() {
            if let Some(to) = nudge.nudge(now_us()) {
                hand.to(to);
                nudges += 1;
            }
            if target.wait(Duration::ZERO, |l| l == "spring activated=1") {
                sprung_ms = Some(started.elapsed().as_millis());
            }
            pace(ms(5));
        }
        hand.button(false);
        source.wait(Duration::from_secs(5), |l| l.starts_with("ended"));
        source.show();
        target.show();
        eprintln!(
            "MEASURE dnd spring: nudged every {} ms, sprang after {sprung_ms:?} ms and {nudges} nudges",
            rest / 1000
        );
        assert!(sprung_ms.is_some(), "the nudged drag sprang the target");
        assert!(nudges <= 3, "within a few nudges: {nudges}");
    }

    /// How often, and how far, a resting drag must be nudged to spring a target: for each
    /// pattern a fresh drag rests over a spring-loaded target and is nudged for up to 4 s, a
    /// point either side of the rest or two points out and back. Prints when each sprang, or
    /// that it did not (MEASUREMENTS.md, "the drag helper's roles").
    #[test]
    #[ignore = "live: moves the real pointer; cargo xtask vm live -p slopty-dnd --test roles"]
    fn which_nudge_periods_spring_a_target() {
        if !live() {
            return;
        }
        let (_keep, whole) = file();
        let at = point(SOURCE_AT);
        let mut sprang = Vec::new();
        // (first nudge after, then every, in ms; the nudge's points either side, or out and back)
        let patterns: [(u64, u64, f64, bool); 9] = [
            (2500, 100, 1.0, true),
            (500, 500, 1.0, true),
            (600, 600, 1.0, true),
            (650, 650, 1.0, true),
            (400, 400, 2.0, false),
            (500, 500, 2.0, false),
            (600, 600, 2.0, false),
            (700, 700, 2.0, false),
            (800, 800, 2.0, false),
        ];
        for (first, every, step, swing) in patterns {
            let mut source = helper(&["--role", "source", "--at", &at, "--file", &whole]);
            let mut target = App::target(TARGET, "copy", &["--spring", "1"]);
            let mut hand = Hand::new(Numbers::Injector);
            drag_to(&mut hand, centre(TARGET));
            let (x, y) = centre(TARGET);
            let started = Instant::now();
            let mut due = first;
            let mut out = false;
            let (mut lit, mut when) = (None, None);
            while started.elapsed() < Duration::from_secs(4) && when.is_none() {
                let now = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                if now >= due {
                    out = !out;
                    let to = match (out, swing) {
                        (true, _) => x + step,
                        (false, true) => x - step,
                        (false, false) => x,
                    };
                    hand.to((to, y));
                    due = now + every;
                }
                target.pump();
                if lit.is_none() && target.any("spring highlight=1") {
                    lit = Some(now);
                }
                if target.any("spring activated=1") {
                    when = Some(now);
                }
                pace(ms(5));
            }
            hand.button(false);
            source.wait(Duration::from_secs(5), |l| l.starts_with("ended"));
            let updates = target.since(0, "updated").len();
            let shape = if swing { "either side" } else { "out and back" };
            eprintln!(
                "MEASURE dnd spring: first {first} ms then every {every} ms, {step} pt {shape}: \
                 lit {lit:?} ms, sprang {when:?} ms, {updates} updates"
            );
            sprang.push((first, every, step, swing, when));
        }
        eprintln!("spring by nudge: {sprang:?}");
        assert!(sprang.iter().any(|p| p.4.is_some()), "some nudge springs the target");
    }

    /// A drag out of an app on the worker: the drag pasteboard's count moves as it begins and
    /// names a file and a promise; let go over the catcher, the file is taken where it is, the
    /// promise is called into the catcher's folder whole, and the app sees a copy.
    #[test]
    #[ignore = "live: moves the real pointer; cargo xtask vm live -p slopty-dnd --test roles"]
    fn a_drag_out_is_seen_and_caught_with_its_file_and_promise() {
        if !live() {
            return;
        }
        let (_keep, whole) = file();
        let landing = tempfile::tempdir().unwrap();
        let landing_dir = landing.path().canonicalize().unwrap();
        let app_at = at_arg(APP);
        let mut app = App::source(&[
            "--at",
            &app_at,
            "--begin",
            "dragged",
            "--file",
            &whole,
            "--promise",
            "promised by the app.bin:2048:200",
            "--image",
            "clear",
            "--level",
            "floating",
        ]);
        let catch_at = point(CATCH_AT);
        let landing_arg = landing_dir.to_string_lossy().into_owned();
        let mut catcher = helper(&["--role", "catcher", "--at", &catch_at, "--dir", &landing_arg]);
        let mut watch = DragWatch::drag();
        let mut hand = Hand::new(Numbers::Injector);
        hand.to(centre(APP));
        pace(ms(100));
        watch.mark();
        hand.button(true);
        pace(ms(50));
        // Move a few points at a time, looking at the count after each, as the worker does
        // every 8 ms from the first move.
        let from = centre(APP);
        let mut seen = None;
        for step in 1..=12_u32 {
            hand.to((f64::from(step).mul_add(3.0, from.0), from.1));
            pace(ms(8));
            if seen.is_none() {
                seen = watch.began().map(|found| (step, found));
            }
        }
        hand.glide(CATCH_AT);
        // Two drags over the catcher, so the drag manager targets it, then the release.
        hand.to((CATCH_AT.0 + 1.0, CATCH_AT.1));
        pace(ms(30));
        hand.to(CATCH_AT);
        pace(ms(30));
        hand.button(false);
        let promised = catcher.wait(Duration::from_secs(10), |l| l.starts_with("promised"));
        app.wait(Duration::from_secs(5), |l| l.starts_with("ended"));
        app.show();
        catcher.show();
        eprintln!("seen: {seen:?}");
        let (step, found) = seen.expect("the drag pasteboard's count moved as the drag began");
        eprintln!("the drag was seen after move {step}");
        assert!(
            found
                .iter()
                .any(|f| f.file.as_ref().is_some_and(|f| f.path.to_string_lossy() == whole)),
            "the drag names the file: {found:?}"
        );
        assert_eq!(
            found.iter().filter(|f| f.promised.is_some()).count(),
            1,
            "and promises one, the item without a file: {found:?}"
        );
        let caught = catcher.since(0, "caught").pop().unwrap_or_default();
        assert!(
            caught.contains(&format!("files={whole}")),
            "the file, taken where it is: {caught}"
        );
        assert!(caught.contains("promises=1"), "one promise called in: {caught}");
        assert!(promised, "the promise was kept");
        let kept = catcher.since(0, "promised path=").pop().unwrap_or_default();
        let (path, size) = kept
            .strip_prefix("promised path=")
            .and_then(|rest| rest.rsplit_once(" size="))
            .unwrap_or_else(|| panic!("a promised file: {kept:?}"));
        // The reader names the folder as given, or through `/var` rather than `/private/var`.
        let path = std::path::Path::new(path).canonicalize().unwrap();
        assert_eq!(path, landing_dir.join("promised by the app.bin"), "in the catcher's folder");
        assert_eq!(size, "2048", "whole");
        assert!(app.any("ended op=1"), "the app saw a copy");
        assert!(std::path::Path::new(&whole).exists(), "the file was not moved");
    }
}
