//! The crash directory: how reports are named, written, kept and listed.
//!
//! A report is `<ms>-<pid>-<process>.json`; a signal record not yet resolved is the same stem
//! with `.native`, and a hang of a process that lived on is `.hang`, the same JSON. The time comes
//! first so a listing sorts by it, and the process last because its name has dashes of its own.
//!
//! Two more names are in flight and never listed: `<record>.native.<pid>`, a record the process
//! `<pid>` has claimed to resolve, and `<report>.json.partial.<pid>.<n>`, a report being
//! written. [`sweep`] hands back or deletes those whose process is gone.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use crate::report::{Kind, Report};

/// How many reports each process keeps, and as many hangs besides; older ones are deleted as
/// new ones come. Hangs are kept apart so a run of them never pushes a crash out.
pub const KEPT_PER_PROCESS: usize = 20;

/// The extension of a hang's report.
const HANG: &str = "hang";

/// How far apart a report of ours and a `.ips` of the same pid may be and still be one crash.
/// A pid comes round again, but not within a minute of the crash it named.
const LINK_WINDOW_MS: u64 = 60_000;

/// The parts of a report's file name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Name<'a> {
    time_ms: u64,
    pid: u32,
    process: &'a str,
    ext: &'a str,
}

impl<'a> Name<'a> {
    /// `file`'s parts, if it is named as a report is.
    pub(crate) fn parse(file: &'a str) -> Option<Self> {
        let (stem, ext) = file.rsplit_once('.')?;
        let mut parts = stem.splitn(3, '-');
        let time_ms = parts.next()?.parse().ok()?;
        let pid = parts.next()?.parse().ok()?;
        let process = parts.next().filter(|p| !p.is_empty())?;
        Some(Self { time_ms, pid, process, ext })
    }

    /// The file name.
    pub(crate) fn file(&self) -> String {
        format!("{}-{}-{}.{}", self.time_ms, self.pid, self.process, self.ext)
    }
}

/// Makes the crash directory, readable by this user only.
pub(crate) fn make_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

/// Writes `report` as a new file in `dir`, then drops that process's oldest reports of its
/// class past [`KEPT_PER_PROCESS`]. Two reports of one process in one millisecond take the next
/// one after its newest: never a gap the rotation left among the oldest, which the rotation
/// that follows would take back at once.
pub(crate) fn write_new(dir: &Path, report: &Report) -> io::Result<PathBuf> {
    make_dir(dir)?;
    let json = serde_json::to_vec_pretty(report).map_err(io::Error::other)?;
    let ext = if matches!(report.kind, Kind::Hang { .. }) { HANG } else { "json" };
    let newest = entries(dir)
        .into_iter()
        .filter(|(_, _, pid, process, e)| {
            *pid == report.pid && *process == report.process && e == ext
        })
        .map(|(_, time_ms, ..)| time_ms)
        .max();
    let mut time_ms = match newest {
        Some(newest) if newest >= report.time_ms => newest.saturating_add(1),
        _ => report.time_ms,
    };
    let path = loop {
        let name = Name { time_ms, pid: report.pid, process: &report.process, ext };
        let path = dir.join(name.file());
        match fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
            Ok(mut file) => {
                file.write_all(&json)?;
                break path;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                time_ms = time_ms.checked_add(1).ok_or(e)?;
            }
            Err(e) => return Err(e),
        }
    };
    rotate(dir);
    Ok(path)
}

/// Replaces the report at `path` with `report`, whole or not at all.
///
/// The bytes go to a partial file only this call writes (named for this process and a counter,
/// made with `create_new`), which then takes `path`'s place.
pub(crate) fn rewrite(path: &Path, report: &Report) -> io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static PARTIALS: AtomicU64 = AtomicU64::new(0);

    let json = serde_json::to_vec_pretty(report).map_err(io::Error::other)?;
    let mut name = path.file_name().ok_or_else(|| io::Error::other("no file name"))?.to_owned();
    let nth = PARTIALS.fetch_add(1, Ordering::Relaxed);
    name.push(format!(".partial.{}.{nth}", std::process::id()));
    let partial = path.with_file_name(name);
    let mut file =
        fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&partial)?;
    let written = file.write_all(&json);
    drop(file);
    let moved = written.and_then(|()| fs::rename(&partial, path));
    if moved.is_err() {
        let _gone = fs::remove_file(&partial);
    }
    moved
}

/// Claims the record at `path` for this process: renames it to `<path>.<pid>`, which only one
/// process can do. `None` when another process got there first.
#[cfg(target_vendor = "apple")]
pub(crate) fn claim(path: &Path) -> Option<PathBuf> {
    let mut name = path.file_name()?.to_owned();
    name.push(format!(".{}", std::process::id()));
    let claimed = path.with_file_name(name);
    fs::rename(path, &claimed).ok().map(|()| claimed)
}

/// Hands back the records claimed by processes that died before resolving them, and deletes
/// the partial reports they left.
pub(crate) fn sweep(dir: &Path) {
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    for entry in read.filter_map(Result::ok) {
        let file = entry.file_name();
        let Some(file) = file.to_str() else {
            continue;
        };
        if let Some((record, pid)) = claimed(file) {
            if !alive(pid) {
                let _released = fs::rename(entry.path(), dir.join(record));
            }
        } else if let Some(pid) = partial(file)
            && !alive(pid)
        {
            let _gone = fs::remove_file(entry.path());
        }
    }
}

/// For a claimed record, `<record>.native.<pid>`: the record's own name and the pid.
fn claimed(file: &str) -> Option<(&str, u32)> {
    let (record, pid) = file.rsplit_once('.')?;
    record.ends_with(".native").then_some(())?;
    Some((record, pid.parse().ok()?))
}

/// For a partial report, `<report>.partial.<pid>.<n>`: the pid writing it.
fn partial(file: &str) -> Option<u32> {
    let (_, rest) = file.split_once(".partial.")?;
    rest.split('.').next()?.parse().ok()
}

/// Whether process `pid` is still running.
fn alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: `kill(2)` with signal 0 sends nothing; it only checks that `pid` exists.
    let found = unsafe { libc::kill(pid, 0) };
    found == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Reads the report at `path`.
pub(crate) fn read(path: &Path) -> Option<Report> {
    let bytes = fs::read(path).ok()?;
    let mut report: Report = serde_json::from_slice(&bytes).ok()?;
    path.clone_into(&mut report.path);
    Some(report)
}

/// The report files in `dir` (`.json`, `.native` and `.hang`), with their names' parts.
pub(crate) fn entries(dir: &Path) -> Vec<(PathBuf, u64, u32, String, String)> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    read.filter_map(Result::ok)
        .filter_map(|entry| {
            let file = entry.file_name();
            let name = Name::parse(file.to_str()?)?;
            matches!(name.ext, "json" | "native" | HANG).then(|| {
                (entry.path(), name.time_ms, name.pid, name.process.to_owned(), name.ext.to_owned())
            })
        })
        .collect()
}

/// Deletes each process's reports past its newest [`KEPT_PER_PROCESS`], and its hangs past
/// their newest as many.
pub(crate) fn rotate(dir: &Path) {
    let mut by_process: HashMap<(String, bool), Vec<(u64, PathBuf)>> = HashMap::new();
    for (path, time_ms, _, process, ext) in entries(dir) {
        by_process.entry((process, ext == HANG)).or_default().push((time_ms, path));
    }
    for mut reports in by_process.into_values() {
        reports.sort_unstable_by(|a, b| b.cmp(a));
        for (_, old) in reports.into_iter().skip(KEPT_PER_PROCESS) {
            let _gone = fs::remove_file(old);
        }
    }
}

/// Every report in `dir`, with macOS's reports from `diagnostic_reports`, newest first. A
/// `.ips` of a crash that left a report here too is linked from it instead of listed.
pub(crate) fn list(dir: &Path, diagnostic_reports: Option<&Path>) -> Vec<Report> {
    let mut reports: Vec<Report> = entries(dir)
        .into_iter()
        .filter_map(|(path, _, _, _, ext)| match ext.as_str() {
            "json" | HANG => read(&path),
            #[cfg(target_vendor = "apple")]
            "native" => crate::native::read_unresolved(&path),
            _ => None,
        })
        .collect();
    for ips in diagnostic_reports.map(crate::ips::list).unwrap_or_default() {
        let own = reports.iter_mut().find(|r| {
            !matches!(r.kind, Kind::Hang { .. })
                && r.pid == ips.pid
                && r.process == ips.process
                && r.time_ms.abs_diff(ips.time_ms) <= LINK_WINDOW_MS
        });
        match own {
            Some(own) => own.ips = Some(ips.path),
            None => reports.push(ips),
        }
    }
    reports.sort_by_key(|report| std::cmp::Reverse(report.time_ms));
    reports
}

/// Where macOS keeps this user's crash reports; iOS keeps them off the device's file system.
pub(crate) fn diagnostic_reports() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        let home = std::env::home_dir().filter(|home| home.is_absolute())?;
        Some(home.join("Library").join("Logs").join("DiagnosticReports"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{KEPT_PER_PROCESS, Name, entries, list, write_new};
    use crate::report::{Build, Kind, Report};

    fn panic_report(process: &str, pid: u32, time_ms: u64) -> Report {
        Report {
            process: process.to_owned(),
            pid,
            time_ms,
            thread: Some("main".to_owned()),
            kind: Kind::Panic { message: "boom".to_owned(), location: None, aborted: false },
            frames: Vec::new(),
            build: Build::default(),
            ips: None,
            path: PathBuf::new(),
        }
    }

    #[test]
    fn names_carry_time_pid_and_a_dashed_process() {
        let name = Name::parse("1790764058284-42-slopty-worker.json").unwrap();
        assert_eq!(
            name,
            Name { time_ms: 1_790_764_058_284, pid: 42, process: "slopty-worker", ext: "json" },
            "parts"
        );
        assert_eq!(name.file(), "1790764058284-42-slopty-worker.json", "and back");
        assert_eq!(Name::parse("notes.txt"), None, "not a report");
        assert_eq!(Name::parse("1-2-.json"), None, "no process");
    }

    #[test]
    fn a_crash_loop_keeps_each_process_its_newest_reports() {
        let dir = tempfile::tempdir().unwrap();
        write_new(dir.path(), &panic_report("slopty-app", 1, 1)).unwrap();
        for i in 0..30 {
            write_new(dir.path(), &panic_report("slopty-worker", 2, 1_000 + i)).unwrap();
        }
        let files = entries(dir.path());
        let worker: Vec<u64> =
            files.iter().filter(|e| e.3 == "slopty-worker").map(|e| e.1).collect();
        assert_eq!(worker.len(), KEPT_PER_PROCESS, "the worker keeps its newest");
        assert!(worker.iter().all(|t| *t >= 1_010), "the oldest went: {worker:?}");
        assert!(files.iter().any(|e| e.3 == "slopty-app"), "another process's report stays");
    }

    #[test]
    fn two_panics_in_one_millisecond_both_land() {
        let dir = tempfile::tempdir().unwrap();
        let first = write_new(dir.path(), &panic_report("slopty", 7, 5)).unwrap();
        let second = write_new(dir.path(), &panic_report("slopty", 7, 5)).unwrap();
        assert_ne!(first, second, "the second takes the next millisecond");
        let reports = list(dir.path(), None);
        assert_eq!(reports.len(), 2, "both listed");
        assert!(reports.iter().all(|r| r.path.exists()), "each knows its file");
    }

    #[test]
    fn a_ips_is_linked_only_to_a_report_near_its_time() {
        let fixture = include_str!("../tests/fixtures/slopty-worker-segv.ips");
        let ips = crate::ips::parse(fixture).unwrap();
        let diagnostic = tempfile::tempdir().unwrap();
        std::fs::write(diagnostic.path().join("slopty-worker-2026-09-30-062603.ips"), fixture)
            .unwrap();

        let same = tempfile::tempdir().unwrap();
        write_new(same.path(), &panic_report("slopty-worker", ips.pid, ips.time_ms + 2_000))
            .unwrap();
        let listed = list(same.path(), Some(diagnostic.path()));
        assert_eq!(listed.len(), 1, "one crash: {listed:#?}");
        assert!(listed[0].ips.is_some(), "linked to macOS's report");

        let reused = tempfile::tempdir().unwrap();
        write_new(reused.path(), &panic_report("slopty-worker", ips.pid, ips.time_ms + 3_600_000))
            .unwrap();
        let listed = list(reused.path(), Some(diagnostic.path()));
        assert_eq!(listed.len(), 2, "an hour apart, the pid came round again: {listed:#?}");
        let ours = listed.iter().find(|r| matches!(r.kind, Kind::Panic { .. })).unwrap();
        assert_eq!(ours.ips, None, "not linked to another process's crash");
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn one_process_claims_a_record_and_a_dead_claimers_come_back() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("1-2-slopty.native");
        std::fs::write(&record, "record").unwrap();
        let claimed = super::claim(&record).expect("the first claim wins");
        assert!(!record.exists(), "the record left its listed name");
        assert_eq!(super::claim(&record), None, "a second claim finds nothing");
        assert!(entries(dir.path()).is_empty(), "a claimed record is not listed");

        let mut gone = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let dead = gone.id();
        gone.wait().unwrap();
        std::fs::rename(&claimed, dir.path().join(format!("1-2-slopty.native.{dead}"))).unwrap();
        let mine = format!("1-2-slopty.json.partial.{}.0", std::process::id());
        std::fs::write(dir.path().join(&mine), "half").unwrap();
        let theirs = format!("1-2-slopty.json.partial.{dead}.0");
        std::fs::write(dir.path().join(&theirs), "half").unwrap();

        super::sweep(dir.path());
        assert!(record.exists(), "a dead process's claim is handed back");
        assert!(dir.path().join(mine).exists(), "a live writer's partial stays");
        assert!(!dir.path().join(theirs).exists(), "a dead writer's partial goes");
    }

    #[test]
    fn a_rewrite_leaves_no_partial_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_new(dir.path(), &panic_report("slopty", 4, 1)).unwrap();
        let mut report = panic_report("slopty", 4, 1);
        report.thread = Some("rewritten".to_owned());
        super::rewrite(&path, &report).unwrap();
        super::rewrite(&path, &report).unwrap();
        let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(files.len(), 1, "only the report: {files:?}");
        assert_eq!(super::read(&path).unwrap().thread.as_deref(), Some("rewritten"), "whole");
    }

    #[test]
    fn a_torn_or_foreign_file_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("1-2-slopty.json"), b"{ not json").unwrap();
        std::fs::write(dir.path().join("readme.txt"), b"hello").unwrap();
        write_new(dir.path(), &panic_report("slopty", 3, 9)).unwrap();
        let reports = list(dir.path(), None);
        assert_eq!(reports.len(), 1, "only the whole report: {reports:?}");
    }
}
