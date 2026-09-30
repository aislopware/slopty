//! What a crash report says, as its JSON file holds it.

use std::fmt::Write as _;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One crash of one process.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Report {
    /// The process's [`crate::Process::name`], or for a macOS report its process name.
    pub process: String,
    /// Its process id.
    pub pid: u32,
    /// When it crashed, in milliseconds since the Unix epoch.
    pub time_ms: u64,
    /// The thread that crashed: its name, else `main` or its id.
    pub thread: Option<String>,
    /// What happened.
    #[serde(flatten)]
    pub kind: Kind,
    /// The crashed thread's frames, innermost first. An inlined call is a frame of its own, at
    /// the same address as the frame it was inlined into.
    pub frames: Vec<Frame>,
    /// The build that crashed.
    pub build: Build,
    /// macOS's own report of the same crash, when there is one: every thread, the registers and
    /// the loaded images.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ips: Option<PathBuf>,
    /// The file this report was read from.
    #[serde(skip)]
    pub path: PathBuf,
}

/// What a crash was.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Kind {
    /// A Rust panic.
    Panic {
        /// The panic's message.
        message: String,
        /// `file:line:column` of the code that panicked.
        location: Option<String>,
        /// Whether the process then aborted (a panic that could not unwind, or one during
        /// another), rather than unwinding.
        #[serde(default)]
        aborted: bool,
    },
    /// A fatal signal, caught by Slopty's own handler.
    Signal {
        /// The signal number.
        signal: i32,
        /// Its name, such as `SIGSEGV`.
        name: String,
        /// The faulting address (`si_addr`), for `SIGSEGV` and `SIGBUS` the memory touched.
        address: u64,
    },
    /// A crash macOS reported in a `.ips` file.
    Exception {
        /// The Mach exception, such as `EXC_BAD_ACCESS`.
        exception: String,
        /// The signal it became, such as `SIGSEGV`.
        signal: Option<String>,
        /// Why, when the report says: an uncaught exception's reason, a guard violation, the
        /// termination's description.
        reason: Option<String>,
    },
}

/// One frame of a crashed thread.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Frame {
    /// The instruction address in the crashed process.
    pub address: u64,
    /// The image (executable or library) holding it, by file name.
    pub image: Option<String>,
    /// The address's offset in that image, which is what `atos -l` and a dSYM take.
    pub offset: Option<u64>,
    /// The function, demangled and without its hash.
    pub function: Option<String>,
    /// The source file.
    pub file: Option<String>,
    /// The source line.
    pub line: Option<u32>,
}

/// The build a crash happened in.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Build {
    /// The workspace version.
    pub version: Option<String>,
    /// The executable's path.
    pub exe: Option<String>,
    /// The executable's Mach-O UUID, which names the dSYM that symbolicates it.
    pub uuid: Option<String>,
}

impl Build {
    /// This process's build.
    pub(crate) fn current() -> Self {
        #[cfg(target_vendor = "apple")]
        let uuid = crate::image::Image::current().map(|image| image.uuid_string());
        #[cfg(not(target_vendor = "apple"))]
        let uuid = None;
        Self {
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            exe: std::env::current_exe().ok().map(|exe| exe.display().to_string()),
            uuid,
        }
    }
}

impl Report {
    /// When it crashed, as `2026-09-30 12:34:56 UTC`.
    #[must_use]
    pub fn when(&self) -> String {
        crate::time::format_utc(self.time_ms)
    }

    /// One line saying what happened: the panic message, or the signal or exception.
    #[must_use]
    pub fn headline(&self) -> String {
        match &self.kind {
            Kind::Panic { message, location, aborted } => {
                let mut line = format!("panic: {}", message.lines().next().unwrap_or_default());
                if let Some(at) = location {
                    let _infallible = write!(line, " at {at}");
                }
                if *aborted {
                    line.push_str(", then aborted");
                }
                line
            }
            Kind::Signal { name, address, .. } => format!("{name} at {address:#x}"),
            Kind::Exception { exception, signal, reason } => {
                let mut line = exception.clone();
                if let Some(signal) = signal {
                    let _infallible = write!(line, " ({signal})");
                }
                if let Some(reason) = reason {
                    let _infallible = write!(line, ": {}", reason.lines().next().unwrap_or(""));
                }
                line
            }
        }
    }
}

impl Frame {
    /// `function (file:line)`, else `image + offset`, else the bare address.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut text = match (&self.function, &self.image, self.offset) {
            (Some(function), ..) => function.clone(),
            (None, Some(image), Some(offset)) => format!("{image} + {offset:#x}"),
            _ => format!("{:#x}", self.address),
        };
        if let Some(file) = &self.file {
            let _infallible = match self.line {
                Some(line) => write!(text, " ({file}:{line})"),
                None => write!(text, " ({file})"),
            };
        }
        text
    }
}

/// A symbol as Rust spells it, without the hash, when it is a Rust symbol; as it is otherwise.
pub(crate) fn demangle(symbol: &str) -> String {
    rustc_demangle::try_demangle(symbol)
        .map_or_else(|_| symbol.to_owned(), |name| format!("{name:#}"))
}

#[cfg(test)]
mod tests {
    use super::{Build, Frame, Kind, Report, demangle};

    #[test]
    fn rust_symbols_demangle_and_others_stay() {
        assert_eq!(
            demangle("_RNvCs1234_7mycrate3foo"),
            "mycrate::foo",
            "a v0 symbol, as ReportCrash leaves it"
        );
        assert_eq!(demangle("__pthread_kill"), "__pthread_kill", "a C symbol");
    }

    fn report(kind: Kind) -> Report {
        Report {
            process: "slopty-worker".to_owned(),
            pid: 42,
            time_ms: 1_790_000_000_000,
            thread: Some("main".to_owned()),
            kind,
            frames: vec![Frame {
                address: 0x1_0000_1000,
                image: Some("slopty-worker".to_owned()),
                offset: Some(0x1000),
                function: Some("slopty_worker::main".to_owned()),
                file: Some("apps/slopty-worker/src/main.rs".to_owned()),
                line: Some(7),
            }],
            build: Build::default(),
            ips: None,
            path: std::path::PathBuf::new(),
        }
    }

    #[test]
    fn a_report_reads_back_as_written() {
        let written = report(Kind::Signal { signal: 11, name: "SIGSEGV".to_owned(), address: 8 });
        let json = serde_json::to_string(&written).unwrap();
        assert!(json.contains(r#""kind":"signal""#), "the kind is tagged inline: {json}");
        let read: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(read, written, "a report survives its own file");
    }

    #[test]
    fn headlines_say_what_happened() {
        let panic = report(Kind::Panic {
            message: "boom\nsecond line".to_owned(),
            location: Some("src/a.rs:1:2".to_owned()),
            aborted: true,
        });
        assert_eq!(panic.headline(), "panic: boom at src/a.rs:1:2, then aborted", "panic");
        let exception = report(Kind::Exception {
            exception: "EXC_BAD_ACCESS".to_owned(),
            signal: Some("SIGSEGV".to_owned()),
            reason: None,
        });
        assert_eq!(exception.headline(), "EXC_BAD_ACCESS (SIGSEGV)", "exception");
        assert_eq!(
            panic.frames.first().map(Frame::describe).as_deref(),
            Some("slopty_worker::main (apps/slopty-worker/src/main.rs:7)"),
            "a resolved frame names its function and line"
        );
        let raw = Frame {
            address: 0x10,
            image: Some("x".to_owned()),
            offset: Some(4),
            ..Frame::default()
        };
        assert_eq!(raw.describe(), "x + 0x4", "an unresolved frame names its image offset");
    }
}
