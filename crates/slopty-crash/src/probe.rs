//! Crashing on purpose, so a test can see what a real crash leaves behind.
//!
//! [`crate::install`] reads [`crate::TRIGGER_ENV`] last and crashes as it says, which reaches
//! every binary through the one line that installs the reporter. [`run`] starts a binary that
//! way, with its own data directory, and reads the reports it left.

use std::io;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

use crate::Report;

/// How to crash.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trigger {
    /// A Rust panic on the main thread.
    Panic,
    /// `abort()`, as a panic that cannot unwind ends.
    Abort,
    /// A load from an unmapped address, as a bad pointer in native code does.
    Segv,
}

impl Trigger {
    /// The value [`crate::TRIGGER_ENV`] takes for it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Panic => "panic",
            Self::Abort => "abort",
            Self::Segv => "segv",
        }
    }
}

/// What a crashed run left.
#[derive(Debug)]
pub struct Crashed {
    /// How the process ended.
    pub status: ExitStatus,
    /// What it wrote to stderr.
    pub stderr: String,
    /// The reports in its data directory, newest first (not macOS's).
    pub reports: Vec<Report>,
}

/// Runs `exe` with `args`, crashing as `trigger` says, and reads what it left in `data_dir`.
///
/// `$SLOPTY_DATA_DIR` is `data_dir` for the run. A signal record is resolved only when `exe` is
/// the calling binary; otherwise its frames stay named by offset.
pub fn run(exe: &Path, args: &[&str], trigger: Trigger, data_dir: &Path) -> io::Result<Crashed> {
    let output = Command::new(exe)
        .args(args)
        .env(crate::TRIGGER_ENV, trigger.as_str())
        .env("SLOPTY_DATA_DIR", data_dir)
        .env_remove("RUST_BACKTRACE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    Ok(Crashed {
        status: output.status,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        reports: crate::reports_with(&crate::crash_dir(data_dir), None),
    })
}

/// Crashes this process as `trigger` names; an unknown name does nothing.
pub(crate) fn fire(trigger: &str) {
    match trigger {
        "panic" => panic(),
        "abort" => std::process::abort(),
        "segv" => segv(),
        _ => {}
    }
}

#[inline(never)]
#[expect(clippy::panic, reason = "the panic is the point: a test asked for one")]
fn panic() {
    panic!("{} asked this process to panic", crate::TRIGGER_ENV);
}

/// Loads from address 16, which is never mapped: `__PAGEZERO` covers the first 4 GiB of an
/// arm64 macOS process, and iOS maps nothing at the bottom either.
#[inline(never)]
fn segv() {
    #[cfg(target_arch = "aarch64")]
    // SAFETY: the load faults and the handler chain ends the process with SIGSEGV; the asm
    // touches no Rust-visible state and no stack, and nothing after it runs.
    unsafe {
        std::arch::asm!(
            "ldr {value}, [{address}]",
            address = in(reg) 16_usize,
            value = out(reg) _,
            options(nostack, readonly),
        );
    }
    std::process::abort();
}
