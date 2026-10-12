//! The daemons' log, bounded.
//!
//! [`init`] sets up `tracing` for a daemon: the `RUST_LOG` filter, else its default, written to
//! stderr, in colour only on a terminal. launchd points a `LaunchAgent`'s stdout and stderr at
//! `~/Library/Logs/Slopty/<program>.log` and never rotates it, so a daemon that runs for weeks
//! would grow it without end. When stderr is a regular file, the log caps it itself: once the
//! file passes [`CAP`] it moves to `<file>.1`, replacing the one before, and a new file takes
//! its name, put in place of stdout too when that was the same file. A daemon's log so holds
//! at most twice [`CAP`]. A journal (systemd) or a terminal is left to bound itself.
//!
//! [`Repeats`] keeps a failure that repeats from writing a line each time it happens: the
//! first, then one every so often while it lasts, then how long it lasted once it ends.

use std::io::IsTerminal as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The most a daemon's log file holds before it moves aside.
pub const CAP: u64 = 8 * 1024 * 1024;

/// How often a failure that goes on is told again ([`Repeats`]).
pub const STILL_EVERY: Duration = Duration::from_hours(1);

/// `tracing` for a daemon, to stderr, filtered by `RUST_LOG` else `default` (`"info"`), with
/// stderr's file capped at [`CAP`] when it is one. A second call changes nothing.
pub fn init(default: &str) {
    init_capped(default, CAP);
}

/// [`init`] with the cap at `cap` bytes.
pub fn init_capped(default: &str, cap: u64) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    let ansi = std::io::stderr().is_terminal();
    let builder = tracing_subscriber::fmt().with_env_filter(filter).with_ansi(ansi);
    // Already set, by a test's harness or an earlier call: that one stays.
    let _set = match Capped::of_stderr(cap) {
        Some(capped) => builder.with_writer(capped).try_init(),
        None => builder.with_writer(std::io::stderr).try_init(),
    };
}

/// Stderr, written through and counted, moved aside past its cap.
#[derive(Clone)]
struct Capped(Arc<File>);

/// The file stderr writes to.
struct File {
    /// Its path, as the descriptor names it.
    path: PathBuf,
    /// Whether stdout is the same file, so it follows when the file moves aside.
    stdout_too: bool,
    /// The most it holds.
    cap: u64,
    /// Its size as last known: at start, then counted with every line written.
    size: AtomicU64,
    /// Set while one writer moves it aside, so no other does at the same time.
    moving: AtomicBool,
}

impl Capped {
    /// The cap on stderr's file, or none when stderr is no regular file whose path is known.
    fn of_stderr(cap: u64) -> Option<Self> {
        let stderr = std::io::stderr();
        let stat = rustix::fs::fstat(&stderr).ok()?;
        if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile {
            return None;
        }
        let path = path_of_stderr()?;
        let stdout_too = rustix::fs::fstat(std::io::stdout())
            .is_ok_and(|out| out.st_dev == stat.st_dev && out.st_ino == stat.st_ino);
        let size = u64::try_from(stat.st_size).unwrap_or(0);
        let file = File {
            path,
            stdout_too,
            cap,
            size: AtomicU64::new(size),
            moving: AtomicBool::new(false),
        };
        let capped = Self(Arc::new(file));
        capped.0.wrote(0);
        Some(capped)
    }
}

/// The path stderr's file has: `F_GETPATH` on Apple's systems, `/proc` elsewhere.
fn path_of_stderr() -> Option<PathBuf> {
    #[cfg(target_vendor = "apple")]
    {
        use std::os::unix::ffi::OsStringExt as _;
        let path = rustix::fs::getpath(std::io::stderr()).ok()?;
        Some(PathBuf::from(std::ffi::OsString::from_vec(path.into_bytes())))
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        std::fs::read_link("/proc/self/fd/2").ok()
    }
}

impl File {
    /// `bytes` more were written; past the cap, the file moves aside.
    fn wrote(&self, bytes: u64) {
        let size = self.size.fetch_add(bytes, Ordering::Relaxed).saturating_add(bytes);
        if size <= self.cap || self.moving.swap(true, Ordering::Acquire) {
            return;
        }
        // A file that cannot move aside is written on: the next line tries again.
        if self.move_aside().is_ok() {
            self.size.store(0, Ordering::Relaxed);
        }
        self.moving.store(false, Ordering::Release);
    }

    /// `<file>` to `<file>.1`, and a new `<file>` in place of stderr (and stdout).
    fn move_aside(&self) -> std::io::Result<()> {
        let mut old = self.path.clone().into_os_string();
        old.push(".1");
        // Gone already (the person deleted it): the new file takes its name all the same.
        match std::fs::rename(&self.path, &old) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let new = std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        rustix::stdio::dup2_stderr(&new)?;
        if self.stdout_too {
            rustix::stdio::dup2_stdout(&new)?;
        }
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capped {
    type Writer = Line<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        Line { file: &self.0 }
    }
}

/// One event's writes to stderr, counted.
struct Line<'a> {
    file: &'a File,
}

impl std::io::Write for Line<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let written = std::io::stderr().write(buf)?;
        self.file.wrote(u64::try_from(written).unwrap_or(u64::MAX));
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()
    }
}

/// A failure that may repeat, as a log tells it.
///
/// It is told the first time and whenever it changes, then once every [`Repeats::new`]'s
/// `every` while it goes on, and how long it went on once it ends. A worker whose server is gone
/// redials every couple of seconds for as long as it is gone.
#[derive(Debug)]
pub struct Repeats {
    every: Duration,
    /// The failure going on: what it is, since when, how many times, and when it was last told.
    current: Option<Current>,
}

#[derive(Debug)]
struct Current {
    what: String,
    since: Instant,
    times: u64,
    told: Instant,
}

/// What to log of a failure, from [`Repeats::failed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tell {
    /// It is new, or not what failed before: tell it.
    New,
    /// It goes on.
    Still {
        /// The failures so far.
        times: u64,
        /// How long ago the first was.
        lasted: Duration,
    },
    /// Told recently enough: nothing.
    Quiet,
}

impl Repeats {
    /// Told again every `every` while a failure goes on.
    #[must_use]
    pub const fn new(every: Duration) -> Self {
        Self { every, current: None }
    }

    /// `what` failed at `now`.
    pub fn failed(&mut self, what: &str, now: Instant) -> Tell {
        match &mut self.current {
            Some(current) if current.what == what => {
                current.times = current.times.saturating_add(1);
                if now.saturating_duration_since(current.told) < self.every {
                    return Tell::Quiet;
                }
                current.told = now;
                Tell::Still {
                    times: current.times,
                    lasted: now.saturating_duration_since(current.since),
                }
            }
            Some(current) => {
                what.clone_into(&mut current.what);
                current.times = current.times.saturating_add(1);
                current.told = now;
                Tell::New
            }
            None => {
                let what = what.to_owned();
                self.current = Some(Current { what, since: now, times: 1, told: now });
                Tell::New
            }
        }
    }

    /// It worked at `now`: how many failures there were, over how long, when there were any.
    pub fn ended(&mut self, now: Instant) -> Option<(u64, Duration)> {
        let current = self.current.take()?;
        Some((current.times, now.saturating_duration_since(current.since)))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{Repeats, Tell};

    /// A failure is told the first time, then once an hour while it repeats every two seconds,
    /// again at once when it changes, and summed up when it ends.
    #[test]
    fn a_repeated_failure_is_told_once_an_hour() {
        let start = Instant::now();
        let at = |s: u64| start.checked_add(Duration::from_secs(s)).unwrap();
        let mut repeats = Repeats::new(Duration::from_hours(1));
        assert_eq!(repeats.ended(at(0)), None, "nothing failed");
        assert_eq!(repeats.failed("no answer", at(0)), Tell::New);
        let mut told = Vec::new();
        for s in (2..=3 * 3600).step_by(2) {
            match repeats.failed("no answer", at(s)) {
                Tell::Quiet => {}
                tell => told.push((s, tell)),
            }
        }
        let still = |s: u64| Tell::Still {
            times: s.checked_div(2).and_then(|n| n.checked_add(1)).unwrap(),
            lasted: Duration::from_secs(s),
        };
        assert_eq!(told, [(3600, still(3600)), (7200, still(7200)), (10800, still(10800))]);
        assert_eq!(repeats.failed("refused", at(10802)), Tell::New, "another failure");
        assert_eq!(repeats.failed("refused", at(10804)), Tell::Quiet);
        assert_eq!(repeats.ended(at(10806)), Some((5403, Duration::from_secs(10806))));
        assert_eq!(repeats.failed("no answer", at(10808)), Tell::New, "a new outage");
    }
}
