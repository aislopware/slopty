//! `xtask prune`: keep `target/` bounded.
//!
//! Cargo never deletes what it built. Every new unit hash (a `cargo update`, a fork rebase, a new
//! toolchain, a feature set nobody built before) leaves the old artifacts behind for good, and
//! with several agents and gate lanes building all day that grew `target/` past 370 GB
//! (`docs/decisions/tooling.md`, "target/ stays bounded"). This deletes, in every cargo profile
//! directory under `target/` (`debug`, `<triple>/debug`, the gate lanes, the deep checks):
//! - **units nobody built or checked for a whole window**, with their artifacts in `deps/` and
//!   `build/` (and any artifact whose unit is gone);
//! - **incremental caches untouched for a window**, which rustc rewrites on every compile of their
//!   unit, so an old one only serves a crate nobody edited.
//!
//! Use is read from the access time of the unit's `.fingerprint` files, which cargo reads on
//! every build that includes the unit, fresh or not. APFS updates an access time only while it
//! is older than the modification time, so at the start of each window the pass sets the access
//! times to the epoch: any build that reads a file in the window then stamps it. A unit is gone
//! after one to two idle windows. A deleted unit that turns out to be wanted is rebuilt,
//! dependencies from sccache.
//!
//! Each directory is pruned under cargo's own build lock, so no build runs in it meanwhile.

use std::fs::{File, FileTimes};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use camino::{Utf8Path, Utf8PathBuf};

use crate::tools::repo_root;

/// How long a unit or an incremental cache may go unused before it goes.
pub const DEFAULT_IDLE_HOURS: u64 = 24;

/// Where the start of the current window is recorded, per profile directory.
const STAMP: &str = ".xtask-prune";

/// Prune options.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// The idle window.
    pub idle: Duration,
    /// Wait for a busy build directory instead of skipping it.
    pub wait: bool,
    /// Report what would go without deleting it.
    pub dry_run: bool,
}

/// What one pass removed (or would remove).
#[derive(Clone, Copy, Debug, Default)]
struct Freed {
    units: usize,
    incremental: usize,
    bytes: u64,
}

impl Freed {
    const fn add(&mut self, other: Self) {
        self.units = self.units.saturating_add(other.units);
        self.incremental = self.incremental.saturating_add(other.incremental);
        self.bytes = self.bytes.saturating_add(other.bytes);
    }

    const fn is_empty(&self) -> bool {
        self.units == 0 && self.incremental == 0
    }
}

/// `cargo xtask prune`: every profile directory, waiting for busy ones if asked, with a report.
pub fn run(opts: Options) -> Result<()> {
    let target = repo_root()?.join("target");
    let mut total = Freed::default();
    for dir in profile_dirs(&target)? {
        match prune_dir(&dir, opts)? {
            Some(freed) => {
                if !freed.is_empty() {
                    println!("  {}: {}", rel(&target, &dir), describe(freed, opts.dry_run));
                }
                total.add(freed);
            }
            None => println!("  {}: busy, skipped", rel(&target, &dir)),
        }
    }
    println!("✔ prune: {}", describe(total, opts.dry_run));
    Ok(())
}

/// The pass after `check` and `gate`: skips busy directories, never fails the command.
pub fn auto() {
    let opts = Options {
        idle: Duration::from_secs(DEFAULT_IDLE_HOURS.saturating_mul(3600)),
        wait: false,
        dry_run: false,
    };
    let pass = || -> Result<Freed> {
        let target = repo_root()?.join("target");
        let mut total = Freed::default();
        for dir in profile_dirs(&target)? {
            if let Some(freed) = prune_dir(&dir, opts)? {
                total.add(freed);
            }
        }
        Ok(total)
    };
    match pass() {
        Ok(freed) if !freed.is_empty() => println!("  prune target/: {}", describe(freed, false)),
        Ok(_) => {}
        Err(e) => eprintln!("  prune target/ skipped: {e:#}"),
    }
}

fn describe(freed: Freed, dry_run: bool) -> String {
    #[expect(clippy::cast_precision_loss, reason = "a size for people to read")]
    let gb = freed.bytes as f64 / 1e9;
    let verb = if dry_run { "would free" } else { "freed" };
    format!("{} units, {} incremental caches, {verb} {gb:.1} GB", freed.units, freed.incremental)
}

fn rel<'a>(target: &Utf8Path, dir: &'a Utf8Path) -> &'a str {
    dir.strip_prefix(target).map_or(dir.as_str(), Utf8Path::as_str)
}

/// Every cargo profile directory under `target/`: one holding a `.fingerprint`. They sit at
/// most four levels down (`gate/<lane>/<triple>/debug`).
fn profile_dirs(target: &Utf8Path) -> Result<Vec<Utf8PathBuf>> {
    fn walk(dir: &Utf8Path, depth: u8, out: &mut Vec<Utf8PathBuf>) -> Result<()> {
        if dir.join(".fingerprint").is_dir() {
            out.push(dir.to_owned());
            return Ok(());
        }
        if depth == 0 {
            return Ok(());
        }
        let Ok(entries) = dir.read_dir_utf8() else { return Ok(()) };
        for entry in entries {
            let entry = entry?;
            // The gate's source snapshot and submodule checkouts hold no build output.
            if entry.file_type()?.is_dir() && !matches!(entry.file_name(), "tree" | "modules") {
                walk(entry.path(), depth.saturating_sub(1), out)?;
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(target, 4, &mut out)?;
    out.sort();
    Ok(out)
}

/// Prune one profile directory under its build lock; `None` when a build holds the lock and
/// `opts.wait` is off.
fn prune_dir(dir: &Utf8Path, opts: Options) -> Result<Option<Freed>> {
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(".cargo-lock"))
        .with_context(|| format!("open {dir}/.cargo-lock"))?;
    if opts.wait {
        lock.lock().with_context(|| format!("lock {dir}"))?;
    } else if lock.try_lock().is_err() {
        return Ok(None);
    }
    let now = SystemTime::now();
    let mut freed = prune_incremental(dir, now, opts)?;
    freed.add(prune_units(dir, now, opts)?);
    Ok(Some(freed))
}

/// Remove the incremental caches no compile has touched for a window. rustc starts a new
/// session directory inside a crate's cache on every compile, so the newest mtime among the
/// cache and its sessions is its last compile.
fn prune_incremental(dir: &Utf8Path, now: SystemTime, opts: Options) -> Result<Freed> {
    let mut freed = Freed::default();
    let Ok(entries) = dir.join("incremental").read_dir_utf8() else { return Ok(freed) };
    for entry in entries {
        let cache = entry?.into_path();
        let mut newest = modified(&cache);
        if let Ok(sessions) = cache.read_dir_utf8() {
            for session in sessions.flatten() {
                newest = newest.max(modified(session.path()));
            }
        }
        if now.duration_since(newest).is_ok_and(|age| age > opts.idle) {
            freed.bytes = freed.bytes.saturating_add(remove(&cache, opts.dry_run)?);
            freed.incremental = freed.incremental.saturating_add(1);
        }
    }
    Ok(freed)
}

/// Once per window: drop the units no build read since the window began, then open the next.
fn prune_units(dir: &Utf8Path, now: SystemTime, opts: Options) -> Result<Freed> {
    let mut freed = Freed::default();
    let stamp = dir.join(STAMP);
    let window_start = std::fs::read_to_string(&stamp)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .and_then(|secs| UNIX_EPOCH.checked_add(Duration::from_secs(secs)));
    if window_start.is_some_and(|start| now.duration_since(start).unwrap_or_default() < opts.idle) {
        return Ok(freed);
    }
    let fingerprints = dir.join(".fingerprint");
    let mut live = std::collections::HashSet::new();
    let mut live_files = Vec::new();
    for entry in fingerprints.read_dir_utf8().with_context(|| format!("read {fingerprints}"))? {
        let unit = entry?.into_path();
        let Some(hash) = unit.file_name().and_then(unit_hash) else { continue };
        let files: Vec<Utf8PathBuf> = unit
            .read_dir_utf8()
            .map(|it| it.flatten().map(camino::Utf8DirEntry::into_path).collect())
            .unwrap_or_default();
        // Without a window start there is no evidence yet: everything is kept this time.
        // The directory by its mtime only: listing it here may stamp its access time.
        let used = window_start.is_none_or(|start| {
            modified(&unit) >= start || files.iter().any(|f| last_touched(f) >= start)
        });
        if used {
            live.insert(hash.to_owned());
            live_files.extend(files);
        } else {
            freed.bytes = freed.bytes.saturating_add(remove(&unit, opts.dry_run)?);
            freed.units = freed.units.saturating_add(1);
        }
    }
    // Artifacts are named `<crate>-<hash>` and cargo cannot use one whose unit is gone.
    for sub in ["deps", "build"] {
        let Ok(entries) = dir.join(sub).read_dir_utf8() else { continue };
        for entry in entries {
            let path = entry?.into_path();
            let Some(hash) = path.file_name().and_then(unit_hash) else { continue };
            if !live.contains(hash) {
                freed.bytes = freed.bytes.saturating_add(remove(&path, opts.dry_run)?);
            }
        }
    }
    if opts.dry_run {
        return Ok(freed);
    }
    let epoch = FileTimes::new().set_accessed(UNIX_EPOCH);
    for file in &live_files {
        // Only the access time moves; cargo compares modification times.
        File::open(file)
            .and_then(|f| f.set_times(epoch))
            .with_context(|| format!("reset the access time of {file}"))?;
    }
    let secs = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    std::fs::write(&stamp, secs.to_string()).with_context(|| format!("write {stamp}"))?;
    Ok(freed)
}

/// The unit hash in an artifact or unit directory name: `libfoo-0123456789abcdef.rlib`,
/// `foo-0123456789abcdef.foo.1a2b-cgu.0.rcgu.o`, `.fingerprint/foo-0123456789abcdef`.
fn unit_hash(name: &str) -> Option<&str> {
    let stem = name.split('.').next()?;
    let (_, hash) = stem.rsplit_once('-')?;
    (hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hash)
}

fn modified(path: &Utf8Path) -> SystemTime {
    std::fs::symlink_metadata(path).and_then(|m| m.modified()).unwrap_or(UNIX_EPOCH)
}

/// The later of the access and modification times: a file written in the window counts as used.
fn last_touched(path: &Utf8Path) -> SystemTime {
    std::fs::symlink_metadata(path).map_or(UNIX_EPOCH, |m| {
        let accessed = m.accessed().unwrap_or(UNIX_EPOCH);
        let modified = m.modified().unwrap_or(UNIX_EPOCH);
        accessed.max(modified)
    })
}

/// Delete a file or directory tree and return the bytes it held.
fn remove(path: &Utf8Path, dry_run: bool) -> Result<u64> {
    fn size(path: &Utf8Path) -> u64 {
        let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
        if !meta.is_dir() {
            return meta.len();
        }
        path.read_dir_utf8().map_or(0, |entries| {
            entries.flatten().fold(0_u64, |sum, e| sum.saturating_add(size(e.path())))
        })
    }
    let bytes = size(path);
    if dry_run {
        return Ok(bytes);
    }
    let result = if std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match result {
        Ok(()) => Ok(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e).with_context(|| format!("remove {path}")),
    }
}
