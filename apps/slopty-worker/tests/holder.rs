//! The display holder as the worker's binary runs it (`slopty-worker --display-holder`), making
//! real displays: one made for a worker outlives that worker, and the next worker finds it. A
//! stand-in app's window (`slopty-stub-window`) on such a display shows what a person's window
//! goes through: nothing while the worker is replaced, and a trip to the main display and back
//! while the holder is.

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use std::io::BufRead as _;
    use std::os::unix::process::CommandExt as _;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use objc2_core_graphics::{CGDisplayBounds, CGMainDisplayID};
    use slopty_proto::screen::{DisplayKey, DisplayShape};
    use slopty_worker::screen::holder::{self, HeldBy, Remote};
    use slopty_worker::screen::sized::{self, Enforced, Factory as _};

    /// A worker's link to the holder at `socket`. With `start`, it dials as a worker does
    /// ([`holder::spawning`], from this build's worker): one is started when a display is asked
    /// for and none answers, or when none answers and the list holds displays to make again.
    /// Without, it only ever dials the socket.
    fn worker(socket: &Path, store: &Path, start: bool) -> Remote {
        let program =
            slopty_testkit::bins::bin(env!("CARGO_BIN_EXE_slopty-worker"), "slopty-worker");
        let (socket, store) = (socket.to_path_buf(), store.to_path_buf());
        if start {
            return Remote::new(holder::spawning(program, socket, store));
        }
        Remote::new(Box::new(move |_need| std::os::unix::net::UnixStream::connect(&socket)))
    }

    /// A display the holder made for a worker that then went without letting go is still
    /// there for the next worker, under the same id; ended, it leaves the holder's list.
    #[test]
    fn a_display_the_holder_made_outlives_the_worker_that_asked_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, store) = (dir.path().join("displays.sock"), dir.path().join("displays.json"));
        let key = DisplayKey(*b"slopty-live-hold");
        let shape = DisplayShape { width: 1280, height: 800, scale: 2.0, refresh_hz: 60 };
        let plan = sized::plan_for(key, &shape);

        let mut first = worker(&socket, &store, true);
        let made = first.create(key, &plan).expect("the holder made a display");
        let id = first.id(&made);
        assert_ne!(id, 0);
        assert_eq!(holder::kept(&store), [(key, plan)], "listed on disk");
        drop(first);

        let mut second = worker(&socket, &store, false);
        let held = second.held();
        let [(held_key, held_plan, display)] = held.as_slice() else { panic!("one display") };
        assert_eq!((*held_key, *held_plan, second.id(display)), (key, plan, id), "the same one");
        second.release(*display);
        assert!(holder::kept(&store).is_empty(), "ended, and out of the list");
        assert_eq!(second.held().len(), 0, "nothing held");
        second.quit();
    }

    /// The holder at a test's socket, asked to quit when the test ends, however it ends, so no
    /// holder outlives it with the test's output.
    struct Quits<'a>(&'a Path, &'a Path);

    impl Drop for Quits<'_> {
        fn drop(&mut self) {
            let mut last = worker(self.0, self.1, false);
            let _alive = last.held();
            last.quit();
        }
    }

    /// `display` held to its planned mode as the worker's registry holds it, enforced until
    /// macOS says it is settled (for up to 10 s), since macOS may pick another mode for a new
    /// display; its size in points then, and what the last enforcement said.
    fn enforced(remote: &mut Remote, display: &HeldBy) -> ((f64, f64), String) {
        let asked = Instant::now();
        let mut said = String::from("nothing");
        while asked.elapsed() < Duration::from_secs(10) {
            let state = remote.enforce(display);
            said = format!("{state:?}");
            if matches!(state, Ok(Enforced::Settled)) {
                break;
            }
            #[expect(clippy::disallowed_methods, reason = "a test polling what macOS shows")]
            std::thread::sleep(Duration::from_millis(50));
        }
        let size = CGDisplayBounds(remote.id(display)).size;
        ((size.width, size.height), said)
    }

    /// Where the stand-in's window is: the display its screen shows (0 for none) and its frame
    /// in points from that screen's corner, so the displays laid out again around it (as other
    /// tests' displays come and go) is no move of its own.
    #[derive(Clone, Copy, PartialEq, Debug)]
    struct Frame {
        screen: u32,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }

    impl Frame {
        fn read(line: &str) -> Option<Self> {
            let words: Vec<&str> = line.strip_prefix("frame ")?.split(' ').collect();
            let [screen, x, y, width, height] = words.as_slice() else { return None };
            let at = |word: &str| word.parse::<f64>().ok();
            Some(Self {
                screen: screen.parse().ok()?,
                x: at(x)?,
                y: at(y)?,
                width: at(width)?,
                height: at(height)?,
            })
        }
    }

    /// A stand-in app's window on display `display`, and each frame it says, with when the
    /// test heard it.
    struct Window {
        child: Child,
        frames: mpsc::Receiver<(Instant, Frame)>,
        last: Frame,
    }

    impl Window {
        /// The window `width` × `height` points on display `display`, once it is there.
        fn open(display: u32, width: f64, height: f64) -> Self {
            let anchor = env!("CARGO_BIN_EXE_slopty-worker");
            let stub = slopty_testkit::bins::bin(anchor, "slopty-stub-window");
            let mut child = Command::new(stub)
                .args([display.to_string(), width.to_string(), height.to_string()])
                .stdout(Stdio::piped())
                .process_group(0)
                .spawn()
                .expect("the stand-in window starts");
            slopty_testkit::group::ward(Some(child.id()));
            let out = std::io::BufReader::new(child.stdout.take().expect("its output"));
            let (tx, frames) = mpsc::channel();
            std::thread::spawn(move || {
                for line in out.lines().map_while(Result::ok) {
                    if let Some(frame) = Frame::read(&line)
                        && tx.send((Instant::now(), frame)).is_err()
                    {
                        return;
                    }
                }
            });
            let none = Frame { screen: 0, x: 0.0, y: 0.0, width: 0.0, height: 0.0 };
            let mut window = Self { child, frames, last: none };
            let placed = window.until(Duration::from_secs(20), |f| f.screen == display);
            assert!(placed.is_some(), "the window went on display {display}: {:?}", window.last);
            window
        }

        /// Every frame said within `span`, the last one kept.
        fn during(&mut self, span: Duration) -> Vec<(Instant, Frame)> {
            let end = Instant::now().checked_add(span).expect("a test's span fits");
            let mut said = Vec::new();
            while let Some(left) = end.checked_duration_since(Instant::now()) {
                match self.frames.recv_timeout(left) {
                    Ok(heard) => {
                        self.last = heard.1;
                        said.push(heard);
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => panic!("the window ended"),
                }
            }
            said
        }

        /// The first frame within `span` that `wanted` holds of, and when it was heard.
        fn until(&mut self, span: Duration, wanted: impl Fn(&Frame) -> bool) -> Option<Instant> {
            let end = Instant::now().checked_add(span)?;
            while let Some(left) = end.checked_duration_since(Instant::now()) {
                match self.frames.recv_timeout(left) {
                    Ok((at, frame)) => {
                        self.last = frame;
                        if wanted(&frame) {
                            return Some(at);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => return None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => panic!("the window ended"),
                }
            }
            None
        }
    }

    impl Drop for Window {
        fn drop(&mut self) {
            let reaped = |child: &mut Child| child.try_wait().is_ok_and(|status| status.is_some());
            let children = std::slice::from_mut(&mut self.child);
            let ended = slopty_testkit::group::end(children, |child| Some(child.id()), reaped);
            debug_assert!(ended, "the stand-in window outlived the test");
        }
    }

    /// A worker update's path, end to end at the display: the worker that asked for the display
    /// goes, the next one is up 2 s later and takes it back from the holder. The display never
    /// leaves, so the window on it never moves. Measured: how long the new worker's link, hello
    /// and list take (`docs/MEASUREMENTS.md`, "a person's window across a worker or holder
    /// restart").
    #[test]
    fn a_window_on_a_held_display_stays_put_while_its_worker_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, store) = (dir.path().join("displays.sock"), dir.path().join("displays.json"));
        let _quits = Quits(&socket, &store);
        let key = DisplayKey(*b"slopty-live-move");
        let shape = DisplayShape { width: 2560, height: 1600, scale: 2.0, refresh_hz: 60 };
        let plan = sized::plan_for(key, &shape);
        let mut first = worker(&socket, &store, true);
        let made = first.create(key, &plan).expect("the holder made a display");
        let id = first.id(&made);
        let mut window = Window::open(id, 900.0, 560.0);
        let _settling = window.during(Duration::from_secs(2));
        let before = window.last;

        let gone = Instant::now();
        drop(first);
        let restarting = window.during(Duration::from_secs(2));
        let dialled = Instant::now();
        let mut second = worker(&socket, &store, false);
        let held = second.held();
        let took = dialled.elapsed();
        let [(_, _, display)] = held.as_slice() else {
            panic!("one display held, not {}", held.len())
        };
        assert_eq!(second.id(display), id, "the same display");
        let after = window.during(Duration::from_secs(3));
        // A line comes too when only the screen's own place moved; that is no move.
        let moved: Vec<Frame> =
            restarting.iter().chain(&after).map(|(_, f)| *f).filter(|f| *f != before).collect();
        eprintln!(
            "MEASURE worker-replaced: window moves {} in {:.1} s, new worker took the display \
             back in {:.1} ms, window {}x{} at ({}, {}) on {}",
            moved.len(),
            gone.elapsed().as_secs_f64(),
            took.as_secs_f64() * 1e3,
            before.width,
            before.height,
            before.x,
            before.y,
            before.screen,
        );
        assert!(moved.is_empty(), "the window never moved: {moved:?}");
        assert_eq!(window.last, before, "where it was");
        second.release(*display);
        second.quit();
    }

    /// A holder replaced (its wire changed, or a crash), with a window larger than the main
    /// display on a display it held: macOS moves the window to the main display when the
    /// display goes, shrunk to fit it, and back at its own size once the next holder makes the
    /// display again under the same identity (as a new display id). Measured: how long it is
    /// away, and where it comes back (`docs/MEASUREMENTS.md`, "a person's window across a worker or
    /// holder restart").
    #[test]
    fn a_window_larger_than_the_main_display_comes_back_at_its_size_after_a_holder_restart() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, store) = (dir.path().join("displays.sock"), dir.path().join("displays.json"));
        let _quits = Quits(&socket, &store);
        let main = CGDisplayBounds(CGMainDisplayID()).size;
        let key = DisplayKey(*b"slopty-live-wide");
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a display's side in points, a few thousand"
        )]
        let points = |side: f64| (side * 1.5).round() as u32;
        let (width, height) = (points(main.width), points(main.height));
        let shape = DisplayShape { width, height, scale: 1.0, refresh_hz: 60 };
        let plan = sized::plan_for(key, &shape);
        let mut first = worker(&socket, &store, true);
        let made = first.create(key, &plan).expect("the holder made a display");
        let id = first.id(&made);
        // Half again the main display, and a window a fifth larger than it: what any Mac's
        // virtual display takes, the gate's 1920 × 1080 and a 1024 × 768 guest alike.
        let (room, said) = enforced(&mut first, &made);
        let size = (main.width * 1.2, main.height * 1.2);
        let fits = size.0 + 120.0 <= room.0 && size.1 + 160.0 <= room.1;
        assert!(fits, "{size:?}, past {main:?}, in {room:?} (enforce said {said})");
        let mut window = Window::open(id, size.0, size.1);
        let _settling = window.during(Duration::from_secs(2));
        let before = window.last;

        // Every frame from the quit on is kept, and read once the window is back.
        let quit = Instant::now();
        first.quit();
        let mut seen = Vec::new();
        while !seen.iter().any(|(_, f): &(Instant, Frame)| f.screen != id)
            && quit.elapsed() < Duration::from_secs(10)
        {
            seen.extend(window.during(Duration::from_millis(50)));
        }
        // The next holder is started at once, as a worker does for a holder that quit.
        let mut second = worker(&socket, &store, true);
        let mut held = Vec::new();
        while held.is_empty() && quit.elapsed() < Duration::from_secs(30) {
            held = second.held();
            seen.extend(window.during(Duration::from_millis(50)));
        }
        let made_again = quit.elapsed();
        let [(_, _, display)] = held.as_slice() else { panic!("one display made again") };
        let again = second.id(display);
        // The next worker takes it as lingering and holds it to its mode, as on the first.
        let _room = enforced(&mut second, display);
        while !seen.iter().any(|(_, f)| f.screen == again)
            && quit.elapsed() < Duration::from_secs(45)
        {
            seen.extend(window.during(Duration::from_millis(200)));
        }
        seen.extend(window.during(Duration::from_secs(3)));
        let after = window.last;
        let left = seen.iter().position(|(_, f)| f.screen != id);
        let back = seen.iter().position(|(_, f)| f.screen == again);
        let away = back.and_then(|back| back.checked_sub(1)).and_then(|at| seen.get(at));
        let since = |at: Option<usize>| {
            at.and_then(|at| seen.get(at)).map(|(at, _)| at.duration_since(quit).as_secs_f64())
        };
        let shown =
            |f: &Frame| format!("{}x{} at ({}, {}) on {}", f.width, f.height, f.x, f.y, f.screen);
        eprintln!(
            "MEASURE holder-restart: main {}x{}, display {width}x{height} id {id} then {again}, \
             left after {:?} s, made again after {:.2} s, back after {:?} s; before {}, away {}, \
             after {}",
            main.width,
            main.height,
            since(left),
            made_again.as_secs_f64(),
            since(back),
            shown(&before),
            away.map_or_else(String::new, |(_, f)| shown(f)),
            shown(&after),
        );
        assert!(left.is_some(), "the window left with the display: {seen:?}");
        assert!(back.is_some(), "the window came back to the display: {seen:?}");
        let size = |f: &Frame| (f.width, f.height);
        let (was, now) = (size(&before), size(&after));
        let same = (was.0 - now.0).abs() < 0.5 && (was.1 - now.1).abs() < 0.5;
        assert!(same, "back at its size, larger than the main display: {was:?} then {now:?}");
        second.release(*display);
        second.quit();
    }
}
