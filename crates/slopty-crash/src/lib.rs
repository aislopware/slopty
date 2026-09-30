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

/// Starts reporting this process's crashes into `crash_dir(data_dir)`. Call it once, first
/// thing in `main`; later calls do nothing.
///
/// Sets the panic hook (keeping the previous one), installs the fatal-signal handlers on Apple
/// platforms, and turns records a previous run of this binary left into reports on a background
/// thread when there are any. With [`TRIGGER_ENV`] set, the process then crashes as asked.
pub fn install(process: Process, data_dir: &Path) {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    let mut first = false;
    INSTALLED.get_or_init(|| first = true);
    if !first {
        return;
    }
    let dir = crash_dir(data_dir);
    let build = Build::current();
    #[cfg(target_vendor = "apple")]
    native::install(process, &dir, &build);
    #[cfg(target_vendor = "apple")]
    native::finalize_in_background(process, &dir);
    panic::install(process, dir, build);
    if let Some(trigger) = std::env::var_os(TRIGGER_ENV) {
        probe::fire(&trigger.to_string_lossy());
    }
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
