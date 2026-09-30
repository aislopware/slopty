//! Crash reports for every Slopty process, kept on the machine they happened on.
//!
//! [`install`] is the first line of every binary's `main`. From then on:
//!
//! - **A panic** on any thread writes `<data dir>/crashes/<ms>-<pid>-<process>.json` from the panic
//!   hook: the message, where it was raised, the thread and the frames, resolved in the panicking
//!   process by std's own symbolizer (the `backtrace` crate). The previous hook still runs, so
//!   stderr reads as before.
//! - **A fatal signal** (`SIGSEGV`, `SIGBUS`, `SIGILL`, `SIGFPE`, `SIGABRT`, `SIGTRAP`) writes a
//!   `.native` record from an async-signal-safe handler: the signal, the fault address, the thread
//!   and the raw return addresses walked up the frame-pointer chain. The handler then hands the
//!   signal on (to Rust's stack-overflow handler, else the default action), so the process dies of
//!   it as before and macOS's `ReportCrash` still writes its `.ips`.
//! - **The next run of the same binary** turns its `.native` records into `.json` reports on a
//!   background thread: it resolves the frames against its own copy of the code, which is the
//!   crashed image when the build UUIDs match. [`reports`] does the same for the binary that calls
//!   it.
//!
//! - **A hang** of the app's main thread, which the app's hang monitor measures, is written by
//!   [`record_hang`] as a `.hang` report: how long the thread was held and by what. The process
//!   lives on; hangs are kept apart from crashes, so a run of them never pushes a crash out.
//!
//! [`reports`] lists the newest reports first, together with macOS's own reports of the same
//! processes from `~/Library/Logs/DiagnosticReports` (see [`ips`]); a `.ips` of a process that
//! left a report of its own is linked to it rather than listed twice. Each process keeps its
//! newest [`KEPT_PER_PROCESS`] reports.
//!
//! Nothing here runs on a hot path: [`install`] costs a few system calls at start, and the rest
//! runs only once something has already gone wrong. `docs/decisions/crashes.md` says why this and
//! not minidumps.

#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

pub mod ips;
pub mod probe;

mod panic;
mod report;
mod store;
mod time;

#[cfg(target_vendor = "apple")]
mod image;
#[cfg(target_vendor = "apple")]
mod native;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

pub use report::{Build, Frame, Kind, Report};
pub use store::KEPT_PER_PROCESS;

/// The environment variable that makes [`install`] crash its process on purpose, for tests:
/// `panic`, `abort` or `segv`.
pub const TRIGGER_ENV: &str = "SLOPTY_CRASH_TEST";

/// Which Slopty program a process is; its reports are filed under [`Process::name`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Process {
    /// The macOS app, `slopty-app`.
    App,
    /// The iOS and iPadOS app, `Slopty`.
    IosApp,
    /// The command line, `slopty`.
    Cli,
    /// The worker daemon, `slopty-worker`.
    Worker,
    /// The server daemon, `slopty-server`.
    Server,
    /// The PTY custodian, `slopty-ptyd`.
    Ptyd,
}

impl Process {
    /// Every process, in no particular order.
    pub const ALL: [Self; 6] =
        [Self::App, Self::IosApp, Self::Cli, Self::Worker, Self::Server, Self::Ptyd];

    /// The executable's name, which is also the process name macOS files its own reports under.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::App => "slopty-app",
            Self::IosApp => "Slopty",
            Self::Cli => "slopty",
            Self::Worker => "slopty-worker",
            Self::Server => "slopty-server",
            Self::Ptyd => "slopty-ptyd",
        }
    }

    /// The process named `name`, if it is one of ours.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }
}

/// Where the reports of every process that shares `data_dir` go.
#[must_use]
pub fn crash_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("crashes")
}

/// Where [`install`] files this process's reports, for [`record_hang`].
static INSTALLED: OnceLock<(Process, PathBuf, Build)> = OnceLock::new();

/// Starts reporting this process's crashes into `crash_dir(data_dir)`. Call it once, first
/// thing in `main`; later calls do nothing.
///
/// Sets the panic hook (keeping the previous one), installs the fatal-signal handlers on Apple
/// platforms, and turns records a previous run of this binary left into reports on a background
/// thread when there are any. With [`TRIGGER_ENV`] set, the process then crashes as asked.
pub fn install(process: Process, data_dir: &Path) {
    let mut first = false;
    let (_, dir, build) = INSTALLED.get_or_init(|| {
        first = true;
        (process, crash_dir(data_dir), Build::current())
    });
    if !first {
        return;
    }
    let (dir, build) = (dir.clone(), build.clone());
    #[cfg(target_vendor = "apple")]
    native::install(process, &dir, &build);
    #[cfg(target_vendor = "apple")]
    native::finalize_in_background(process, &dir);
    panic::install(process, dir, build);
    if let Some(trigger) = std::env::var_os(TRIGGER_ENV) {
        probe::fire(&trigger.to_string_lossy());
    }
}

/// A hang of this process's main thread, as its hang monitor measured it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Hang {
    /// The longest single piece of main-thread work: the freeze as a user saw it.
    pub stall: Duration,
    /// From what started it to the frame that ended it.
    pub active: Duration,
    /// That longest piece of work, in words: `a task spawned at src/x.rs:12`, `the action
    /// file::SaveFile`, `drawing a window`.
    pub cause: String,
    /// No single piece of work was past the threshold: many short ones filled one frame's time.
    pub piled_up: bool,
    /// Where the work that held it came from, longest first, as far as the monitor knows it (a
    /// task's spawn site); the frames of a crash, for a hang.
    pub frames: Vec<Frame>,
}

/// Files `hang` as a report of this process, next to its crashes, where `slopty crashes` lists
/// it; the report's path.
///
/// # Errors
///
/// Before [`install`], or when the report could not be written.
pub fn record_hang(hang: Hang) -> std::io::Result<PathBuf> {
    let (process, dir, build) = INSTALLED
        .get()
        .ok_or_else(|| std::io::Error::other("the crash reporter is not installed"))?;
    write_hang(dir, *process, build.clone(), hang)
}

/// [`record_hang`] into the crash directory `dir`.
fn write_hang(dir: &Path, process: Process, build: Build, hang: Hang) -> std::io::Result<PathBuf> {
    let millis = |d: Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
    let report = Report {
        process: process.name().to_owned(),
        pid: std::process::id(),
        time_ms: time::now_ms(),
        thread: Some("main".to_owned()),
        kind: Kind::Hang {
            stall_ms: millis(hang.stall),
            active_ms: millis(hang.active),
            cause: hang.cause,
            piled_up: hang.piled_up,
        },
        frames: hang.frames,
        build,
        ips: None,
        path: PathBuf::new(),
    };
    store::write_new(dir, &report)
}

/// Every report of every process sharing `data_dir`, newest first.
///
/// macOS's own reports of them, from the user's `DiagnosticReports`, come too. This binary's
/// pending signal records are turned into reports first.
#[must_use]
pub fn reports(data_dir: &Path) -> Vec<Report> {
    reports_with(&crash_dir(data_dir), store::diagnostic_reports().as_deref())
}

/// [`reports`] over a given crash directory and, if any, a given `DiagnosticReports` directory.
#[must_use]
pub fn reports_with(crash_dir: &Path, diagnostic_reports: Option<&Path>) -> Vec<Report> {
    store::sweep(crash_dir);
    #[cfg(target_vendor = "apple")]
    native::finalize(crash_dir);
    store::list(crash_dir, diagnostic_reports)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Build, Frame, Hang, KEPT_PER_PROCESS, Kind, Process, reports_with, write_hang};

    fn hang(stall_ms: u64) -> Hang {
        Hang {
            stall: Duration::from_millis(stall_ms),
            active: Duration::from_millis(stall_ms.saturating_add(20)),
            cause: "a task spawned at crates/slopty-ui/src/file.rs:12".to_owned(),
            piled_up: false,
            frames: vec![Frame {
                file: Some("crates/slopty-ui/src/file.rs".to_owned()),
                line: Some(12),
                ..Frame::default()
            }],
        }
    }

    /// A hang is listed with the crashes, says how long and by what, and a run of hangs keeps
    /// its own newest without pushing a crash out.
    #[test]
    fn a_hang_is_listed_and_never_pushes_a_crash_out() {
        let dir = tempfile::tempdir().unwrap();
        let crash = crate::report::Report {
            process: Process::App.name().to_owned(),
            pid: 1,
            time_ms: 1,
            thread: Some("main".to_owned()),
            kind: Kind::Panic { message: "boom".to_owned(), location: None, aborted: false },
            frames: Vec::new(),
            build: Build::default(),
            ips: None,
            path: std::path::PathBuf::new(),
        };
        crate::store::write_new(dir.path(), &crash).unwrap();
        for ms in 0..30 {
            write_hang(dir.path(), Process::App, Build::default(), hang(300 + ms)).unwrap();
        }
        let listed = reports_with(dir.path(), None);
        let hangs: Vec<_> = listed.iter().filter(|r| matches!(r.kind, Kind::Hang { .. })).collect();
        assert_eq!(hangs.len(), KEPT_PER_PROCESS, "the newest hangs are kept");
        assert!(listed.iter().any(|r| matches!(r.kind, Kind::Panic { .. })), "the crash stays");
        // Reports written in one millisecond take the next free one, so the order is the
        // files', not the writes'.
        let said =
            "hang: main thread held 329 ms by a task spawned at crates/slopty-ui/src/file.rs:12";
        let last = hangs.iter().find(|r| r.headline() == said).expect("the last hang is kept");
        assert_eq!(last.frames.first().and_then(|f| f.line), Some(12), "where it came from");
    }
}
