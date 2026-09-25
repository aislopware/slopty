//! What one client input event costs the thread that hands it to the injector: the stream's
//! tokio task on the worker. Real window-server reads (the target's bounds, the owner's
//! activation state) against a real on-screen window, and no event posted and nothing
//! activated, so it needs no Accessibility grant and leaves the desktop alone. A measurement,
//! run by hand: `docs/MEASUREMENTS.md`, "input injection off the runtime".

#[cfg(test)]
#[expect(clippy::cast_precision_loss, reason = "measurement arithmetic on small counts")]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};

    use slopty_capture::Rect;
    use slopty_core::WindowId;
    use slopty_input::{Backend, Injector, InputError, InputSink as _, InputThread, Post, System};
    use slopty_proto::input::{KeyAction, KeyCode, Mods};
    use slopty_proto::screen::{CaptureTarget, ScreenInput};

    /// `System`'s reads, counted; posting and activation do nothing but note when the post
    /// happened.
    #[derive(Clone, Debug, Default)]
    struct Dry {
        /// Bounds reads in front of an event.
        bounds_reads: Arc<AtomicUsize>,
        active_checks: Arc<AtomicUsize>,
        posted: Option<mpsc::Sender<Instant>>,
    }

    impl Backend for Dry {
        fn owner_pid(&self, target: CaptureTarget) -> Option<i32> {
            System.owner_pid(target)
        }

        fn bounds(&mut self, target: CaptureTarget) -> Option<Rect> {
            self.bounds_reads.fetch_add(1, Ordering::Relaxed);
            System.bounds(target)
        }

        fn is_active(&mut self, pid: i32) -> bool {
            self.active_checks.fetch_add(1, Ordering::Relaxed);
            System.is_active(pid)
        }

        fn activate(&mut self, _pid: i32) -> Result<(), InputError> {
            Ok(())
        }

        fn post(&mut self, _post: Post) -> Result<(), InputError> {
            if let Some(posted) = &self.posted {
                let _gone = posted.send(Instant::now());
            }
            Ok(())
        }
    }

    /// A normal-layer window on screen, from the window list.
    fn some_window() -> Option<WindowId> {
        let everywhere = Rect { x: -1.0e5, y: -1.0e5, w: 2.0e5, h: 2.0e5 };
        slopty_capture::occluders(WindowId(0), &everywhere, -1)
            .into_iter()
            .find(|w| w.layer == 0)
            .map(|w| w.id)
    }

    const MOVES: usize = 1500;
    const MOVE_EVERY: Duration = Duration::from_millis(2);
    /// Moves between two geometry probes: 100 ms.
    const PROBE_EVERY: usize = 50;
    const KEYS: usize = 200;
    const KEY_EVERY: Duration = Duration::from_millis(5);

    fn pace(started: Instant, every: Duration) {
        #[expect(clippy::disallowed_methods, reason = "the event clock of a measurement")]
        std::thread::sleep(every.saturating_sub(started.elapsed()));
    }

    fn quantiles(label: &str, mut us: Vec<f64>) {
        us.sort_by(f64::total_cmp);
        let at = |q: f64| {
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "index")]
            let i = ((us.len().saturating_sub(1)) as f64 * q).round() as usize;
            us.get(i).copied().unwrap_or(0.0)
        };
        eprintln!(
            "{label}: n {} p50 {:.1} / p99 {:.1} / max {:.1} µs",
            us.len(),
            at(0.5),
            at(0.99),
            us.last().copied().unwrap_or(0.0),
        );
    }

    /// What the stream's task hands the injector: an event, or the bounds its geometry probe
    /// read.
    enum Feed<'a> {
        Input(&'a ScreenInput),
        Bounds(Option<Rect>),
    }

    /// Moves at 500 Hz with the bounds read every 100 ms beside them, as the stream's geometry
    /// probe does, then key presses at 200 Hz, each timed on the calling thread, with the
    /// window-server reads made in front of an event. Returns when each event was handed over,
    /// in order.
    fn drive(
        label: &str,
        target: CaptureTarget,
        mut feed: impl FnMut(Feed<'_>),
        dry: &Dry,
    ) -> Vec<Instant> {
        let mut handed = Vec::with_capacity(MOVES + 2 * KEYS);
        let mut moves = Vec::with_capacity(MOVES);
        for n in 0..MOVES {
            if n % PROBE_EVERY == 0 {
                feed(Feed::Bounds(System.bounds(target)));
            }
            let started = Instant::now();
            let at = (n % 400) as f32;
            handed.push(Instant::now());
            feed(Feed::Input(&ScreenInput::Move { x: at, y: at }));
            moves.push(started.elapsed().as_secs_f64() * 1e6);
            pace(started, MOVE_EVERY);
        }
        let reads = dry.bounds_reads.swap(0, Ordering::Relaxed);
        quantiles(&format!("{label} move, caller"), moves);
        eprintln!("{label}: {reads} bounds reads in the event path, {MOVES} moves");
        let mut keys = Vec::with_capacity(KEYS);
        for _ in 0..KEYS {
            let started = Instant::now();
            handed.push(Instant::now());
            feed(Feed::Input(&ScreenInput::Key {
                code: KeyCode::A,
                action: KeyAction::Press,
                mods: Mods::empty(),
                text: Some("a".into()),
            }));
            keys.push(started.elapsed().as_secs_f64() * 1e6);
            handed.push(Instant::now());
            feed(Feed::Input(&ScreenInput::Key {
                code: KeyCode::A,
                action: KeyAction::Release,
                mods: Mods::empty(),
                text: None,
            }));
            pace(started, KEY_EVERY);
        }
        let checks = dry.active_checks.swap(0, Ordering::Relaxed);
        quantiles(&format!("{label} key press, caller"), keys);
        eprintln!("{label}: {checks} activation checks over {KEYS} presses");
        handed
    }

    #[test]
    #[ignore = "measurement"]
    fn injection_cost_on_the_callers_thread() {
        let Some(window) = some_window() else {
            eprintln!("skipped: no window on screen");
            return;
        };
        let target = CaptureTarget::Window(window);
        let dry = Dry::default();
        // The injector as it was called before the input thread: on the caller, reading the
        // bounds itself.
        let mut inline = Injector::with_backend(target, 2.0, dry.clone());
        drive(
            "inline",
            target,
            |feed| {
                if let Feed::Input(input) = feed {
                    let _posted = inline.inject(input);
                }
            },
            &dry,
        );

        let (posted, posts) = mpsc::channel();
        let dry = Dry { posted: Some(posted), ..Dry::default() };
        let mut thread = InputThread::spawn(target, 2.0, dry.clone());
        let handed = drive(
            "thread",
            target,
            |feed| match feed {
                Feed::Input(input) => {
                    let _queued = thread.inject(input);
                }
                Feed::Bounds(bounds) => thread.set_bounds(bounds, Instant::now()),
            },
            &dry,
        );
        drop(thread);
        drop(dry);
        let delays: Vec<f64> = handed
            .iter()
            .zip(posts.iter())
            .map(|(handed, posted)| posted.duration_since(*handed).as_secs_f64() * 1e6)
            .collect();
        quantiles("thread handed → posted", delays);
    }
}
