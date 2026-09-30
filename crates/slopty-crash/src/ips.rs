//! macOS's own crash reports: the `.ips` files `ReportCrash` writes to
//! `~/Library/Logs/DiagnosticReports` for any process that dies of a signal.
//!
//! They see what no hook in the process can: a crash inside a framework (`VideoToolbox`,
//! `AppKit`, the Objective-C runtime) that never reaches Rust, every thread, the loaded images,
//! and an uncaught Objective-C exception's own backtrace. A `.ips` is two JSON documents: a
//! one-line header (`bug_type` `309` is a crash), then the report.

use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::Process;
use crate::report::{Build, Frame, Kind, Report, demangle};

/// Why a `.ips` could not be read as a crash.
#[derive(Debug)]
pub enum ParseError {
    /// The header or the body is not JSON.
    Json(serde_json::Error),
    /// The header's `bug_type` is not a crash's (`309`); it is a hang, a spin, a jetsam event.
    NotACrash(String),
    /// A field every crash report has is missing.
    Missing(&'static str),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(e) => write!(f, "not an .ips report: {e}"),
            Self::NotACrash(kind) => write!(f, "bug type {kind} is not a crash"),
            Self::Missing(field) => write!(f, "no `{field}` in the report"),
        }
    }
}

impl std::error::Error for ParseError {}

/// The crash `text` (a whole `.ips` file) reports. Its frames are the faulting thread's, or,
/// when an uncaught Objective-C exception ended the process, the exception's.
pub fn parse(text: &str) -> Result<Report, ParseError> {
    let (header, body) = text.split_once('\n').ok_or(ParseError::Missing("header"))?;
    let header: Value = serde_json::from_str(header).map_err(ParseError::Json)?;
    let bug_type = header.get("bug_type").and_then(Value::as_str).unwrap_or_default();
    if bug_type != "309" {
        return Err(ParseError::NotACrash(bug_type.to_owned()));
    }
    let body: Value = serde_json::from_str(body).map_err(ParseError::Json)?;

    let process = str_at(&body, "procName").ok_or(ParseError::Missing("procName"))?;
    let pid = body
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .ok_or(ParseError::Missing("pid"))?;
    let time_ms = str_at(&body, "captureTime")
        .and_then(|stamp| crate::time::parse_ips(&stamp))
        .ok_or(ParseError::Missing("captureTime"))?;

    let exception = body.get("exception");
    let kind = Kind::Exception {
        exception: exception
            .and_then(|e| str_at(e, "type"))
            .ok_or(ParseError::Missing("exception"))?,
        signal: exception.and_then(|e| str_at(e, "signal")),
        reason: reason(&body),
    };

    let images: &[Value] =
        body.get("usedImages").and_then(Value::as_array).map_or(&[], Vec::as_slice);
    let faulting = body
        .get("faultingThread")
        .and_then(Value::as_u64)
        .and_then(|i| usize::try_from(i).ok())
        .and_then(|i| body.get("threads")?.as_array()?.get(i));
    let thread = faulting.and_then(|t| str_at(t, "name").or_else(|| str_at(t, "queue")));
    let exception_frames = body.get("lastExceptionBacktrace").and_then(Value::as_array);
    let raw_frames = exception_frames
        .filter(|frames| !frames.is_empty())
        .or_else(|| faulting?.get("frames")?.as_array());
    let frames =
        raw_frames.map(|raw| raw.iter().map(|f| frame(f, images)).collect()).unwrap_or_default();

    let build = Build {
        version: str_at(&header, "app_version").filter(|v| !v.is_empty()),
        exe: str_at(&body, "procPath"),
        uuid: str_at(&header, "slice_uuid").map(|uuid| uuid.to_uppercase()),
    };
    Ok(Report {
        process,
        pid,
        time_ms,
        thread,
        kind,
        frames,
        build,
        ips: None,
        path: PathBuf::new(),
    })
}

/// The largest `.ips` read. A crash report with every thread of a busy app is a few hundred
/// kilobytes; a larger file is not one `ReportCrash` wrote, and is skipped.
const MAX_IPS_BYTES: u64 = 4 << 20;

/// The `.ips` reports in `dir` of Slopty's own processes, as reports that link their file.
pub(crate) fn list(dir: &Path) -> Vec<Report> {
    list_up_to(dir, MAX_IPS_BYTES)
}

/// [`list`], skipping files over `max_bytes`.
fn list_up_to(dir: &Path, max_bytes: u64) -> Vec<Report> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    read.filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_str().is_some_and(is_ours))
        .filter_map(|entry| {
            let path = entry.path();
            let mut report = parse(&read_up_to(&path, max_bytes)?).ok()?;
            report.ips = Some(path.clone());
            report.path = path;
            Some(report)
        })
        .collect()
}

/// The file at `path` as text, unless it is longer than `max_bytes`; it may grow while read,
/// so the read itself stops there too.
fn read_up_to(path: &Path, max_bytes: u64) -> Option<String> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > max_bytes {
        return None;
    }
    let mut text = String::new();
    file.take(max_bytes.checked_add(1)?).read_to_string(&mut text).ok()?;
    (text.len() as u64 <= max_bytes).then_some(text)
}

/// Whether `file` is `ReportCrash`'s name for a report of one of Slopty's processes:
/// `<process>-<date>[.n].ips`.
fn is_ours(file: &str) -> bool {
    let Some(stem) = file.strip_suffix(".ips") else {
        return false;
    };
    Process::ALL.iter().any(|process| {
        stem.strip_prefix(process.name())
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|date| date.starts_with(|c: char| c.is_ascii_digit()))
    })
}

/// One `.ips` frame: `imageIndex` into `usedImages`, `imageOffset` into that image, and the
/// symbol and source `ReportCrash` found.
fn frame(raw: &Value, images: &[Value]) -> Frame {
    let image = raw
        .get("imageIndex")
        .and_then(Value::as_u64)
        .and_then(|i| images.get(usize::try_from(i).ok()?));
    let offset = raw.get("imageOffset").and_then(Value::as_u64);
    let base = image.and_then(|i| i.get("base")).and_then(Value::as_u64);
    Frame {
        address: base.zip(offset).and_then(|(b, o)| b.checked_add(o)).or(offset).unwrap_or(0),
        image: image.and_then(|i| str_at(i, "name")),
        offset,
        function: str_at(raw, "symbol").map(|symbol| demangle(&symbol)),
        file: str_at(raw, "sourceFile"),
        line: raw.get("sourceLine").and_then(Value::as_u64).and_then(|l| u32::try_from(l).ok()),
    }
}

/// What the report says about why: the libraries' own last words (`asi`: an `abort()`, an
/// uncaught exception's reason), else the exception's subtype, else the termination.
fn reason(body: &Value) -> Option<String> {
    let said: Vec<&str> = body
        .get("asi")
        .and_then(Value::as_object)
        .map(|by_library| {
            by_library
                .values()
                .filter_map(Value::as_array)
                .flatten()
                .filter_map(Value::as_str)
                .collect()
        })
        .unwrap_or_default();
    if !said.is_empty() {
        return Some(said.join("\n"));
    }
    body.get("exception")
        .and_then(|e| str_at(e, "subtype"))
        .or_else(|| body.get("termination").and_then(|t| str_at(t, "indicator")))
}

fn str_at(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::{ParseError, is_ours, list_up_to, parse};
    use crate::report::Kind;

    const WORKER_SEGV: &str = include_str!("../tests/fixtures/slopty-worker-segv.ips");

    #[test]
    fn a_recorded_worker_crash_reads_as_a_report() {
        let report = parse(WORKER_SEGV).unwrap();
        assert_eq!(report.process, "slopty-worker", "process");
        assert!(report.pid > 0, "pid");
        assert!(report.time_ms > 1_780_000_000_000, "a 2026 capture time: {}", report.time_ms);
        let Kind::Exception { exception, signal, .. } = &report.kind else {
            panic!("an exception: {:?}", report.kind);
        };
        assert_eq!(exception, "EXC_BAD_ACCESS", "exception");
        assert_eq!(signal.as_deref(), Some("SIGSEGV"), "signal");
        let names: Vec<&str> = report.frames.iter().filter_map(|f| f.function.as_deref()).collect();
        assert!(
            names.iter().any(|name| name.starts_with("slopty_crash::probe::")),
            "the faulting frame, demangled: {names:?}"
        );
        assert!(
            names.iter().any(|name| name.starts_with("slopty_worker::main")),
            "the worker's main: {names:?}"
        );
        let first = report.frames.first().unwrap();
        assert_eq!(first.image.as_deref(), Some("slopty-worker"), "the image by name");
        assert!(first.offset.is_some(), "an image offset for atos");
        assert!(report.build.uuid.as_deref().is_some_and(|u| u.len() == 36), "the slice UUID");
    }

    #[test]
    fn an_oversize_file_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("slopty-worker-2026-09-30-062603.ips"), WORKER_SEGV)
            .unwrap();
        let size = WORKER_SEGV.len() as u64;
        assert_eq!(list_up_to(dir.path(), size).len(), 1, "a report at the limit is read");
        assert!(list_up_to(dir.path(), size - 1).is_empty(), "one byte over is skipped");
    }

    #[test]
    fn a_hang_is_not_a_crash() {
        let hang = "{\"bug_type\":\"288\"}\n{}";
        assert!(matches!(parse(hang), Err(ParseError::NotACrash(kind)) if kind == "288"), "288");
        assert!(matches!(parse("garbage"), Err(ParseError::Missing("header"))), "one line");
    }

    #[test]
    fn only_slopty_reports_are_picked_up() {
        assert!(is_ours("slopty-worker-2026-09-30-004738.ips"), "worker");
        assert!(is_ours("slopty-2026-09-30-004738.000.ips"), "the CLI, second of the second");
        assert!(is_ours("Slopty-2026-09-25-025836.ips"), "the iOS app");
        assert!(!is_ours("slopty_codec-c023-2026-09-30-004738.ips"), "a test binary");
        assert!(!is_ours("slopty-hostd-2026-09-25-000704.ips"), "a process that is not ours");
        assert!(!is_ours("slopty-worker-2026-09-30.txt"), "not a .ips");
    }
}
