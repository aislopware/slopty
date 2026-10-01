//! What the live drag-and-drop tests share (`tests/spikes.rs`, `tests/roles.rs`): the test's own
//! apps and what they say, and a hand on the one real pointer through the HID tap. Only ever
//! run in a macOS guest (`cargo xtask vm live -p slopty-dnd`).

#![allow(dead_code, reason = "each test target uses its own part of the harness")]
#![expect(clippy::cast_possible_truncation, reason = "a live test's points")]

use std::io::{BufRead as _, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventTapLocation, CGEventType, CGMainDisplayID,
    CGMouseButton,
};
use slopty_core::DisplayId;
use slopty_input::{Backend as _, Event, Injector, Post, Route, System};
use slopty_proto::input::{Mods, MouseButton};
use slopty_proto::screen::{CaptureTarget, ScreenInput};
use slopty_testkit::live;

pub fn centre((x, y, w, h): (f64, f64, f64, f64)) -> (f64, f64) {
    (x + w / 2.0, y + h / 2.0)
}

/// The point `t` of the way from `a` to `b`.
pub fn lerp(a: (f64, f64), b: (f64, f64), t: f64) -> (f64, f64) {
    ((b.0 - a.0).mul_add(t, a.0), (b.1 - a.1).mul_add(t, a.1))
}

pub fn at_arg((x, y, w, h): (f64, f64, f64, f64)) -> String {
    format!("{x},{y},{w},{h}")
}

/// Whether this is a guest the lane runs, where posting may move the real pointer. Anywhere
/// else the test says it skipped; in the lane's guest a skip fails it
/// (`slopty_testkit::live::skip`).
pub fn live() -> bool {
    if std::env::var_os("SLOPTY_DND_E2E").is_none() || std::env::var_os(live::IN_VM).is_none() {
        live::skip("moves the real pointer; run in a guest: cargo xtask vm live -p slopty-dnd");
        return false;
    }
    if !slopty_input::can_post() {
        live::skip("no post-event access");
        return false;
    }
    true
}

/// Wait `for_` between two posts, at the pace a client's input comes.
#[expect(clippy::disallowed_methods, reason = "a live test pacing its own posts")]
pub fn pace(for_: Duration) {
    std::thread::sleep(for_);
}

pub fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// The machine's uptime in microseconds, the clock the test's apps stamp their lines with.
pub fn uptime_us() -> u64 {
    let seconds = objc2_foundation::NSProcessInfo::processInfo().systemUptime();
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "µs of uptime")]
    let us = (seconds * 1e6) as u64;
    us
}

/// The path of one of this crate's test apps: nextest's (right in an archive run in the
/// guest), else cargo's.
pub fn bin(name: &str) -> String {
    std::env::var(format!("NEXTEST_BIN_EXE_{name}"))
        .or_else(|_| std::env::var(format!("NEXTEST_BIN_EXE_{}", name.replace('-', "_"))))
        .unwrap_or_else(|_| match name {
            "slopty-drop-target" => env!("CARGO_BIN_EXE_slopty-drop-target").to_owned(),
            "slopty-dnd-helper" => env!("CARGO_BIN_EXE_slopty-dnd-helper").to_owned(),
            "slopty-dnd-wire-helper" => env!("CARGO_BIN_EXE_slopty-dnd-wire-helper").to_owned(),
            _ => env!("CARGO_BIN_EXE_slopty-drag-source").to_owned(),
        })
}

/// One of the test's apps, and everything it has said.
pub struct App {
    pub name: &'static str,
    pub child: Child,
    pub lines: mpsc::Receiver<String>,
    pub said: Vec<String>,
    pub pid: i32,
    pub window: u32,
}

impl App {
    pub fn start(name: &'static str, args: &[&str]) -> Self {
        let mut child = Command::new(bin(name))
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
        // The drag source reads the cursor once before it is ready, which the first time in
        // a process has taken 11 s.
        let mut said = Vec::new();
        let ready = loop {
            let line = lines.recv_timeout(Duration::from_secs(40)).expect("the app is ready");
            if line.starts_with("ready ") {
                break line;
            }
            said.push(line);
        };
        let field = |key: &str| -> i64 {
            ready
                .split(' ')
                .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('=')?.parse().ok())
                .unwrap_or_else(|| panic!("{key} in {ready:?}"))
        };
        let pid = i32::try_from(field("pid")).unwrap();
        let window = u32::try_from(field("window")).unwrap();
        said.push(ready);
        Self { name, child, lines, said, pid, window }
    }

    pub fn source(args: &[&str]) -> Self {
        Self::start("slopty-drag-source", args)
    }

    pub fn target(at: (f64, f64, f64, f64), answer: &str, more: &[&str]) -> Self {
        let at = at_arg(at);
        let mut args = vec!["--at", &at, "--answer", answer];
        args.extend_from_slice(more);
        Self::start("slopty-drop-target", &args)
    }

    /// Take in what the app has said so far.
    pub fn pump(&mut self) {
        while let Ok(line) = self.lines.try_recv() {
            self.said.push(line);
        }
    }

    /// Wait up to `within` for a line `wanted` accepts; whether one came.
    pub fn wait(&mut self, within: Duration, wanted: impl Fn(&str) -> bool) -> bool {
        self.pump();
        if self.said.iter().any(|l| wanted(l)) {
            return true;
        }
        let deadline = Instant::now().checked_add(within).expect("a deadline");
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Ok(line) = self.lines.recv_timeout(left) else { return false };
            let hit = wanted(&line);
            self.said.push(line);
            if hit {
                return true;
            }
        }
    }

    /// The lines said since `mark` that start with `prefix`.
    pub fn since(&mut self, mark: usize, prefix: &str) -> Vec<String> {
        self.pump();
        self.said.iter().skip(mark).filter(|l| l.starts_with(prefix)).cloned().collect()
    }

    pub fn any(&mut self, prefix: &str) -> bool {
        !self.since(0, prefix).is_empty()
    }

    pub fn mark(&mut self) -> usize {
        self.pump();
        self.said.len()
    }

    pub fn show(&mut self) {
        self.pump();
        eprintln!("--- {} said:", self.name);
        for line in &self.said {
            eprintln!("  {line}");
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _killed = self.child.kill();
        let _waited = self.child.wait();
    }
}

/// How the presses a hand posts are numbered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Numbers {
    /// The worker's injector on a display stream: HID, one number per press.
    Injector,
    /// `kCGMouseEventNumber` set to 0 on every press, drag and release.
    Zero,
    /// The field left alone, as CoreGraphics makes the event.
    Unset,
}

/// The pointer and its left button on the main display, through the HID tap.
pub struct Hand {
    pub injector: Injector,
    pub numbers: Numbers,
    pub held: bool,
    pub at: (f64, f64),
}

impl Hand {
    pub fn new(numbers: Numbers) -> Self {
        // A 1:1 stream of the main display, whose origin is the global origin, so stream
        // pixels are global points.
        let injector = Injector::new(CaptureTarget::Display(DisplayId(CGMainDisplayID())), 1.0);
        assert_eq!(injector.route(), Route::Hid, "a display stream posts through the HID tap");
        Self { injector, numbers, held: false, at: (0.0, 0.0) }
    }

    pub fn raw(&self, kind: CGEventType, (x, y): (f64, f64)) {
        let press = kind != CGEventType::MouseMoved;
        let event = Event::Mouse {
            kind,
            at: CGPoint { x, y },
            button: CGMouseButton::Left,
            number: 0,
            clicks: i64::from(press && kind != CGEventType::LeftMouseDragged),
            press: 0,
        };
        let post = Post { route: Route::Hid, flags: CGEventFlags::empty(), event };
        let built = slopty_input::backend::build(&post, None).expect("built");
        if press && self.numbers == Numbers::Zero {
            CGEvent::set_integer_value_field(Some(&built), CGEventField::MouseEventNumber, 0);
        }
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&built));
    }

    pub fn button(&mut self, down: bool) {
        let (x, y) = self.at;
        self.held = down;
        if self.numbers == Numbers::Injector {
            let input = ScreenInput::Button {
                button: MouseButton::Left,
                down,
                clicks: 1,
                x: x as f32,
                y: y as f32,
                mods: Mods::empty(),
            };
            self.injector.inject(&input).expect("posted");
        } else {
            let kind = if down { CGEventType::LeftMouseDown } else { CGEventType::LeftMouseUp };
            self.raw(kind, (x, y));
        }
    }

    pub fn to(&mut self, (x, y): (f64, f64)) {
        self.at = (x, y);
        if self.numbers == Numbers::Injector {
            self.injector.inject(&ScreenInput::Move { x: x as f32, y: y as f32 }).expect("posted");
        } else {
            let kind =
                if self.held { CGEventType::LeftMouseDragged } else { CGEventType::MouseMoved };
            self.raw(kind, (x, y));
        }
    }

    /// Move to `to` in 24 steps 8 ms apart, as a hand moves.
    pub fn glide(&mut self, to: (f64, f64)) {
        let from = self.at;
        for step in 1..=24_u32 {
            let t = f64::from(step) / 24.0;
            self.to(lerp(from, to, t));
            pace(ms(8));
        }
    }

    pub fn key(vk: u16, down: bool) {
        let event = Event::Key { vk, down, modifier: false, repeat: false };
        System
            .post(Post { route: Route::Hid, flags: CGEventFlags::empty(), event })
            .expect("posted");
    }
}

/// A file of the test's own to drag, and its path.
pub fn file() -> (tempfile::NamedTempFile, String) {
    let file = tempfile::Builder::new().prefix("slopty-dnd-").suffix(".txt").tempfile().unwrap();
    std::fs::write(file.path(), b"dragged by the slopty-dnd spikes\n").unwrap();
    let path = file.path().canonicalize().unwrap().to_string_lossy().into_owned();
    (file, path)
}
