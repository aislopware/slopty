//! `xtask prune`: keep `target/` bounded, by age and by size.
//!
//! Cargo never deletes what it built. Every new unit hash (a `cargo update`, a fork rebase, a new
//! toolchain, a feature set nobody built before) leaves the old artifacts behind for good, and
//! with several agents and gate lanes building all day `target/` grew past 370 GB, then to
//! 310 GB on a volume 98 % full (`docs/decisions/tooling.md`, "target/ stays bounded"). In every
//! cargo profile directory under `target/` (`debug`, `<triple>/debug`, the gate lanes, the deep
//! checks), each pass deletes:
//! - **units nobody built or checked for the idle window**, with their artifacts;
//! - **incremental caches untouched for the window**, which rustc rewrites on every compile of
//!   their unit, so an old one only serves a crate nobody edited;
//! - **the object files of a unit's earlier compiles.** On macOS a test or binary keeps its debug
//!   info in the `.rcgu.o` files beside it, named by the rustc invocation that wrote them, and
//!   nothing deletes the previous invocation's. They made up 99 782 of the 102 568 entries of the
//!   tests lane's `deps/`, and every process that opens `VideoToolbox` or `CoreAudio` pays for the
//!   entries in its executable's directory (`docs/decisions/tooling.md`, "A test binary's
//!   directory, not the volume").
//!
//! Then, when `target/` is over its byte budget or its volume is under the free-space floor, it
//! deletes the least recently used units and caches, oldest first, until both hold again with a
//! tenth to spare. Nothing used in the last hour goes that way, so a build's own output
//! survives the pass that follows it.
//!
//! Use is read from the access times of a unit's fingerprint files, which cargo reads on every
//! build that includes the unit, fresh or not. APFS stamps an access time only while it is older
//! than the modification time, so each pass records what it saw in a ledger (`.xtask-prune`) and
//! sets the access times back to the epoch: the next read stamps them again. An incremental
//! cache is used when rustc compiles its crate, which starts a new session directory in it.
//!
//! Both of cargo's layouts are read: `.fingerprint/<unit>` beside `deps/` (to Cargo 1.99) and
//! `build/<package>/<hash>/` with the fingerprint inside (from Cargo 1.100). A profile directory
//! in neither is an error, not an empty pass. Each directory is pruned under cargo's own locks, so
//! no build runs in it meanwhile.

use std::collections::{HashMap, HashSet};
use std::fs::{File, FileTimes};
use std::os::unix::fs::MetadataExt as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};

use crate::tools::repo_root;

/// How long a unit or an incremental cache may go unused before it goes.
pub const DEFAULT_IDLE_HOURS: u64 = 24;

/// What `target/` may hold, in GB (`SLOPTY_TARGET_BUDGET_GB`). A full gate's lanes take 32 GB
/// and a day of agents' builds in `target/debug` about 60 GB (`docs/MEASUREMENTS.md`,
/// 2026-09-30, "target/ under a budget"); the rest is room for a toolchain or fork bump, which
/// rebuilds everything once beside the old units until they age out.
pub const DEFAULT_BUDGET_GB: u64 = 160;

/// The free space `target/`'s volume keeps, in GB (`SLOPTY_DISK_FLOOR_GB`): a cold full gate
/// writes about 35 GB, so a build that starts above the floor finishes before the disk does.
pub const DEFAULT_FLOOR_GB: u64 = 50;

/// Nothing used this recently is deleted for the budget: the build that just ran, and the tests
/// it is about to run from those binaries.
const GUARD: Duration = Duration::from_secs(3600);

/// The ledger of when each unit was last seen in use, per profile directory.
const LEDGER: &str = ".xtask-prune";

const GB: u64 = 1_000_000_000;

/// How much a pass may delete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Bytes `target/` may hold.
    pub budget: u64,
    /// Bytes its volume keeps free.
    pub floor: u64,
}

impl Limits {
    /// The defaults, or `SLOPTY_TARGET_BUDGET_GB` and `SLOPTY_DISK_FLOOR_GB`.
    pub fn from_env() -> Result<Self> {
        let gb = |var: &str, default: u64| -> Result<u64> {
            let value = match std::env::var(var) {
                Ok(v) => v.trim().parse().with_context(|| format!("{var}={v} is not a number"))?,
                Err(_) => default,
            };
            Ok(value.saturating_mul(GB))
        };
        Ok(Self {
            budget: gb("SLOPTY_TARGET_BUDGET_GB", DEFAULT_BUDGET_GB)?,
            floor: gb("SLOPTY_DISK_FLOOR_GB", DEFAULT_FLOOR_GB)?,
        })
    }
}

/// Prune options.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// The idle window.
    pub idle: Duration,
    /// The budget and the floor.
    pub limits: Limits,
    /// Wait for a busy build directory instead of skipping it.
    pub wait: bool,
    /// Report what would go without deleting it.
    pub dry_run: bool,
}

/// What one pass removed (or would remove).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Freed {
    pub units: usize,
    pub caches: usize,
    /// Artifacts of a unit that is gone, and object files of a unit's earlier compiles.
    pub leftovers: usize,
    pub bytes: u64,
}

impl Freed {
    const fn add(&mut self, other: Self) {
        self.units = self.units.saturating_add(other.units);
        self.caches = self.caches.saturating_add(other.caches);
        self.leftovers = self.leftovers.saturating_add(other.leftovers);
        self.bytes = self.bytes.saturating_add(other.bytes);
    }

    const fn is_empty(&self) -> bool {
        self.units == 0 && self.caches == 0 && self.leftovers == 0
    }
}

impl std::fmt::Display for Freed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} units, {} incremental caches, {} leftover files, {}",
            self.units,
            self.caches,
            self.leftovers,
            gb(self.bytes)
        )
    }
}

/// One pass over a target directory.
#[derive(Debug, Default)]
pub struct Report {
    /// Gone for being idle, and leftovers.
    pub idle: Freed,
    /// Gone for the budget or the floor.
    pub budget: Freed,
    /// The target directory's size before and after.
    pub size_before: u64,
    pub size_after: u64,
    /// Its volume's free space before and after.
    pub free_before: u64,
    pub free_after: u64,
    /// Bytes still over the budget or under the floor: nothing old enough was left to delete.
    pub short: u64,
    /// Profile directories a build held, relative to the target directory.
    pub busy: Vec<String>,
    /// Bytes per top-level entry of the target directory, largest first.
    pub largest: Vec<(String, u64)>,
    /// Bytes of units and caches by time since last use: under 1 h, 6 h, 24 h, 3 days, older.
    pub ages: [u64; 5],
    /// How long the sweep of the profile directories and the size walk took.
    pub sweep: Duration,
    pub walk: Duration,
}

/// `cargo xtask prune`: every profile directory, waiting for busy ones if asked, with a report.
pub fn run(opts: Options) -> Result<()> {
    let target = repo_root()?.join("target");
    let report = prune(&target, opts, SystemTime::now(), &free_space)?;
    print(&target, &report, opts.dry_run);
    Ok(())
}

/// The pass after `check` and `gate`: skips busy directories, never fails the command.
pub fn auto() {
    let pass = || -> Result<Report> {
        let opts = Options {
            idle: Duration::from_secs(DEFAULT_IDLE_HOURS.saturating_mul(3600)),
            limits: Limits::from_env()?,
            wait: false,
            dry_run: false,
        };
        prune(&repo_root()?.join("target"), opts, SystemTime::now(), &free_space)
    };
    match pass() {
        Ok(report) => {
            let mut freed = report.idle;
            freed.add(report.budget);
            if !freed.is_empty() {
                println!(
                    "  prune target/: {freed}; {} left, {} free",
                    gb(report.size_after),
                    gb(report.free_after)
                );
            }
            if report.short > 0 {
                eprintln!(
                    "  prune target/: still {} over the budget or under the floor",
                    gb(report.short)
                );
            }
        }
        Err(e) => eprintln!("  prune target/ skipped: {e:#}"),
    }
}

/// Before a gate: its volume must have the floor free. Below it, prune for the budget and the
/// floor; still below, refuse with where the space went.
pub fn ensure_room(target: &Utf8Path) -> Result<()> {
    let limits = Limits::from_env()?;
    std::fs::create_dir_all(target).with_context(|| format!("create {target}"))?;
    if free_space(target)? >= limits.floor {
        return Ok(());
    }
    let opts = Options {
        idle: Duration::from_secs(DEFAULT_IDLE_HOURS.saturating_mul(3600)),
        limits,
        wait: false,
        dry_run: false,
    };
    let report = prune(target, opts, SystemTime::now(), &free_space)?;
    print(target, &report, false);
    check_room(target, &report, limits)
}

/// The refusal, when a pass could not bring the free space back to the floor.
fn check_room(target: &Utf8Path, report: &Report, limits: Limits) -> Result<()> {
    if report.free_after >= limits.floor {
        return Ok(());
    }
    let largest: Vec<String> = report
        .largest
        .iter()
        .take(6)
        .map(|(name, bytes)| format!("  {target}/{name}  {}", gb(*bytes)))
        .collect();
    let busy = if report.busy.is_empty() {
        String::new()
    } else {
        format!("\nbusy, so not pruned: {}", report.busy.join(", "))
    };
    bail!(
        "only {} free on the volume of {target}, under the {} floor, after pruning everything \
         unused for an hour. The largest in {target}:\n{}{busy}\nDelete a directory there that no \
         session needs (a one-off CARGO_TARGET_DIR), run `cargo xtask prune --budget-gb <less>`, \
         or free space elsewhere on the volume. SLOPTY_DISK_FLOOR_GB sets the floor.",
        gb(report.free_after),
        gb(limits.floor),
        largest.join("\n"),
    )
}

fn print(target: &Utf8Path, report: &Report, dry_run: bool) {
    let verb = if dry_run { "would go" } else { "gone" };
    println!("  idle and leftovers {verb}: {}", report.idle);
    println!("  for the budget or the floor {verb}: {}", report.budget);
    let largest: Vec<String> = report
        .largest
        .iter()
        .take(5)
        .map(|(name, bytes)| format!("{name} {}", gb(*bytes)))
        .collect();
    println!("  largest: {}", largest.join(", "));
    for dir in &report.busy {
        println!("  {dir}: busy, skipped");
    }
    let [hour, six, day, three, older] = report.ages.map(gb);
    println!(
        "  units and caches by last use: <1 h {hour}, <6 h {six}, <24 h {day}, <3 d {three}, \
         older {older}; swept in {:.1?}, sized in {:.1?}",
        report.sweep, report.walk
    );
    println!(
        "✔ prune {target}: {} → {}, volume free {} → {}",
        gb(report.size_before),
        gb(report.size_after),
        gb(report.free_before),
        gb(report.free_after),
    );
    if report.short > 0 {
        println!("  still {} over the budget or under the floor", gb(report.short));
    }
}

fn gb(bytes: u64) -> String {
    #[expect(clippy::cast_precision_loss, reason = "a size for people to read")]
    let gb = bytes as f64 / 1e9;
    format!("{gb:.1} GB")
}

/// Bytes free for an unprivileged writer on `path`'s volume.
pub fn free_space(path: &Utf8Path) -> Result<u64> {
    let stat =
        rustix::fs::statvfs(path.as_std_path()).with_context(|| format!("statvfs {path}"))?;
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
}

/// A profile directory's sweep: what it freed and what is left, or `None` when it was busy.
type Swept = Result<Option<(Freed, Vec<Item>)>>;

/// One pass over `target`: the idle sweep in every profile directory, then the budget and the
/// floor. `free` reads a volume's free space.
pub fn prune(
    target: &Utf8Path,
    opts: Options,
    now: SystemTime,
    free: &dyn Fn(&Utf8Path) -> Result<u64>,
) -> Result<Report> {
    let started = std::time::Instant::now();
    let mut report = Report::default();
    let dirs = profile_dirs(target)?;
    // Side by side: most of a sweep is waiting on the file system, a directory at a time.
    let swept: Vec<Swept> = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(dirs.len());
        for dir in &dirs {
            handles.push(scope.spawn(move || {
                let Some(_locks) = Locks::take(dir, opts.wait)? else { return Ok(None) };
                sweep(dir, opts, now).map(Some)
            }));
        }
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Err(anyhow::anyhow!("a prune thread panicked"))))
            .collect()
    });
    let mut surveyed: Vec<(Utf8PathBuf, Vec<Item>)> = Vec::new();
    for (dir, result) in dirs.iter().zip(swept) {
        match result? {
            Some((freed, items)) => {
                report.idle.add(freed);
                surveyed.push((dir.clone(), items));
            }
            None => report.busy.push(rel(target, dir).to_owned()),
        }
    }

    report.sweep = started.elapsed();
    let walking = std::time::Instant::now();
    let census = census(target, &surveyed);
    report.walk = walking.elapsed();
    // A dry run deleted nothing, so the census still counts what the sweep would delete.
    let pending = if opts.dry_run { report.idle.bytes } else { 0 };
    report.size_before = census.total.saturating_sub(pending);
    report.free_before = free(target)?.saturating_add(pending);
    report.largest = census.largest;
    let mut candidates: Vec<(usize, usize, SystemTime, u64)> = Vec::new();
    for (d, (_, items)) in surveyed.iter().enumerate() {
        for (i, item) in items.iter().enumerate() {
            let bytes: u64 = item.paths.iter().filter_map(|p| census.sizes.get(p)).sum();
            let age = now.duration_since(item.last_use).unwrap_or_default().as_secs();
            let bucket =
                [1_u64, 6, 24, 72].iter().take_while(|h| age >= h.saturating_mul(3600)).count();
            if let Some(slot) = report.ages.get_mut(bucket) {
                *slot = slot.saturating_add(bytes);
            }
            candidates.push((d, i, item.last_use, bytes));
        }
    }

    let limits = opts.limits;
    let over = report.size_before > limits.budget || report.free_before < limits.floor;
    let excess = if over {
        let size_target = limits.budget.saturating_sub(limits.budget / 10);
        let free_target = limits.floor.saturating_add(limits.floor / 10);
        report
            .size_before
            .saturating_sub(size_target)
            .max(free_target.saturating_sub(report.free_before))
    } else {
        0
    };
    let mut chosen: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut cutoff = UNIX_EPOCH;
    let mut planned = 0_u64;
    if excess > 0 {
        candidates.sort_by_key(|(_, _, last_use, _)| *last_use);
        let guard = now.checked_sub(GUARD).unwrap_or(UNIX_EPOCH);
        for (d, i, last_use, bytes) in candidates {
            if planned >= excess || last_use > guard {
                break;
            }
            planned = planned.saturating_add(bytes);
            cutoff = cutoff.max(last_use);
            chosen.entry(d).or_default().push(i);
        }
    }
    for (d, picks) in &chosen {
        let Some((dir, items)) = surveyed.get(*d) else { continue };
        let Some(_locks) = Locks::take(dir, opts.wait)? else {
            report.busy.push(rel(target, dir).to_owned());
            continue;
        };
        for item in picks.iter().filter_map(|i| items.get(*i)) {
            // A build since the sweep read it: it is in use after all.
            if item.last_use_now() > cutoff {
                continue;
            }
            let bytes: u64 = item.paths.iter().filter_map(|p| census.sizes.get(p)).sum();
            item.remove(opts.dry_run)?;
            report.budget.bytes = report.budget.bytes.saturating_add(bytes);
            match item.kind {
                Kind::Unit => report.budget.units = report.budget.units.saturating_add(1),
                Kind::Cache => report.budget.caches = report.budget.caches.saturating_add(1),
            }
        }
        if !opts.dry_run {
            remove_empty_packages(dir)?;
        }
    }
    report.busy.sort();
    report.busy.dedup();
    report.size_after = report.size_before.saturating_sub(report.budget.bytes);
    report.free_after = if opts.dry_run {
        report.free_before.saturating_add(report.budget.bytes)
    } else {
        free(target)?
    };
    if over {
        let size_short = report.size_after.saturating_sub(limits.budget);
        let free_short = limits.floor.saturating_sub(report.free_after);
        report.short = size_short.max(free_short);
    }
    Ok(report)
}

fn rel<'a>(target: &Utf8Path, dir: &'a Utf8Path) -> &'a str {
    dir.strip_prefix(target).map_or(dir.as_str(), Utf8Path::as_str)
}

/// Every cargo profile directory under `target`: one holding cargo's `.cargo-lock`. They sit at
/// most four levels down (`gate/<lane>/<triple>/debug`).
fn profile_dirs(target: &Utf8Path) -> Result<Vec<Utf8PathBuf>> {
    fn walk(dir: &Utf8Path, depth: u8, out: &mut Vec<Utf8PathBuf>) -> Result<()> {
        if dir.join(".cargo-lock").is_file() {
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

/// Cargo's locks on a profile directory, held until dropped (closing a file releases its lock).
struct Locks {
    _held: Vec<File>,
}

impl Locks {
    /// Exclusive `.cargo-lock` (every cargo holds it, shared since 1.96, for as long as it
    /// builds) and `.cargo-build-lock` (1.96's own build lock), or `None` when a build holds
    /// either and `wait` is off. Waiting blocks on one lock at a time while holding none, so it
    /// cannot deadlock against a cargo that takes them in the other order.
    fn take(dir: &Utf8Path, wait: bool) -> Result<Option<Self>> {
        let open = |name: &str| -> Result<Option<File>> {
            let path = dir.join(name);
            if name != ".cargo-lock" && !path.exists() {
                return Ok(None);
            }
            let file = File::options()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&path)
                .with_context(|| format!("open {path}"))?;
            Ok(Some(file))
        };
        loop {
            let Some(main) = open(".cargo-lock")? else { bail!("{dir}/.cargo-lock is missing") };
            if wait {
                main.lock().with_context(|| format!("lock {dir}"))?;
            } else if main.try_lock().is_err() {
                return Ok(None);
            }
            let Some(build) = open(".cargo-build-lock")? else {
                return Ok(Some(Self { _held: vec![main] }));
            };
            if build.try_lock().is_ok() {
                return Ok(Some(Self { _held: vec![main, build] }));
            }
            drop(main);
            if !wait {
                return Ok(None);
            }
            build.lock().with_context(|| format!("lock {dir}"))?;
            drop(build);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Unit,
    Cache,
}

/// A unit (its fingerprint and artifacts) or an incremental cache, as the budget sees it.
#[derive(Debug)]
struct Item {
    kind: Kind,
    /// What goes with it.
    paths: Vec<Utf8PathBuf>,
    /// Files whose access or modification is a use: a unit's fingerprint files, a cache's
    /// session directories.
    evidence: Vec<Utf8PathBuf>,
    last_use: SystemTime,
}

impl Item {
    /// When it was last used, read again now, never earlier than the sweep saw.
    /// Directories by their modification time only: listing one may stamp its access time.
    fn last_use_now(&self) -> SystemTime {
        let mut last = self.last_use;
        if let Some(first) = self.paths.first() {
            last = last.max(modified(first));
        }
        for path in &self.evidence {
            last = last.max(match self.kind {
                Kind::Unit => last_touched(path),
                Kind::Cache => modified(path),
            });
        }
        last
    }

    fn remove(&self, dry_run: bool) -> Result<()> {
        if dry_run {
            return Ok(());
        }
        for path in &self.paths {
            remove(path)?;
        }
        Ok(())
    }
}

/// The ledger: when it started (its evidence counts only from a pass that reset the access
/// times), and each unit's last use as the previous pass saw it.
struct Ledger {
    since: Option<u64>,
    /// When the previous pass ran: a unit whose files are all older was not rebuilt since.
    pass: Option<u64>,
    seen: HashMap<String, u64>,
}

impl Ledger {
    fn read(dir: &Utf8Path) -> Self {
        let text = std::fs::read_to_string(dir.join(LEDGER)).unwrap_or_default();
        let mut since = None;
        let mut pass = None;
        let mut seen = HashMap::new();
        for line in text.lines() {
            let Some((head, rest)) = line.split_once(' ') else { continue };
            if head == "since" {
                since = rest.trim().parse().ok();
            } else if head == "pass" {
                pass = rest.trim().parse().ok();
            } else if let Ok(secs) = head.parse::<u64>() {
                seen.insert(rest.to_owned(), secs);
            }
        }
        Self { since, pass, seen }
    }

    fn get(&self, key: &str) -> SystemTime {
        self.seen.get(key).map_or(UNIX_EPOCH, |secs| at(*secs))
    }

    fn write(dir: &Utf8Path, since: u64, pass: u64, units: &[(String, SystemTime)]) -> Result<()> {
        use std::fmt::Write as _;
        let mut text = format!("since {since}\npass {pass}\n");
        for (key, last_use) in units {
            let _written = writeln!(text, "{} {key}", secs(*last_use));
        }
        let path = dir.join(LEDGER);
        let staged = dir.join(format!("{LEDGER}.new"));
        std::fs::write(&staged, text).with_context(|| format!("write {staged}"))?;
        std::fs::rename(&staged, &path).with_context(|| format!("write {path}"))
    }
}

fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH.checked_add(Duration::from_secs(secs)).unwrap_or(UNIX_EPOCH)
}

fn secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// A unit found in a profile directory, before its fate is decided.
struct Found {
    key: String,
    paths: Vec<Utf8PathBuf>,
    evidence: Vec<Utf8PathBuf>,
    /// Its object files, for the earlier compiles among them.
    objects: Vec<Utf8PathBuf>,
}

/// The idle sweep of one profile directory, under its locks: delete the idle units and caches
/// and the leftovers, record the evidence, reset the access times, and return what is left.
fn sweep(dir: &Utf8Path, opts: Options, now: SystemTime) -> Result<(Freed, Vec<Item>)> {
    let ledger = Ledger::read(dir);
    let (units, orphans) = units(dir)?;
    let mut freed = Freed::default();
    let idle_since = now.checked_sub(opts.idle).unwrap_or(UNIX_EPOCH);
    let mut items = Vec::new();
    let mut live = Vec::new();
    let mut reset = Vec::new();
    let mut leftovers = orphans;
    for unit in units {
        let mut last_use = ledger.get(&unit.key);
        let mut written = UNIX_EPOCH;
        for path in unit.paths.iter().take(1) {
            written = written.max(modified(path));
        }
        for file in &unit.evidence {
            let (accessed, modified) = times(file);
            written = written.max(modified);
            last_use = last_use.max(accessed);
        }
        last_use = last_use.max(written);
        // Without a ledger the access times were never reset: no evidence of idleness yet.
        if ledger.since.is_some() && last_use < idle_since {
            freed.bytes = freed.bytes.saturating_add(size_all(&unit.paths));
            freed.units = freed.units.saturating_add(1);
            if !opts.dry_run {
                for path in &unit.paths {
                    remove(path)?;
                }
            }
            continue;
        }
        // Only a compile since the last pass can have left objects behind.
        if ledger.pass.is_none_or(|pass| written >= at(pass)) {
            leftovers.extend(superseded_objects(&unit.objects));
        }
        reset.extend(unit.evidence.iter().cloned());
        // Untrusted times only order this pass's budget; the ledger starts every unit's clock
        // now, so none goes idle before a whole window without a read.
        live.push((unit.key.clone(), if ledger.since.is_some() { last_use } else { now }));
        items.push(Item { kind: Kind::Unit, paths: unit.paths, evidence: unit.evidence, last_use });
    }
    for path in &leftovers {
        freed.bytes = freed.bytes.saturating_add(size(path));
        freed.leftovers = freed.leftovers.saturating_add(1);
        if !opts.dry_run {
            remove(path)?;
        }
    }
    for cache in caches(dir)? {
        if cache.last_use < idle_since {
            freed.bytes = freed.bytes.saturating_add(size_all(&cache.paths));
            freed.caches = freed.caches.saturating_add(1);
            cache.remove(opts.dry_run)?;
        } else {
            items.push(cache);
        }
    }
    if opts.dry_run {
        return Ok((freed, items));
    }
    remove_empty_packages(dir)?;
    let epoch = FileTimes::new().set_accessed(UNIX_EPOCH);
    for file in &reset {
        let accessed = std::fs::symlink_metadata(file).and_then(|m| m.accessed());
        if accessed.is_ok_and(|a| a > UNIX_EPOCH) {
            // Only the access time moves; cargo compares modification times.
            File::open(file)
                .and_then(|f| f.set_times(epoch))
                .with_context(|| format!("reset the access time of {file}"))?;
        }
    }
    Ledger::write(dir, ledger.since.unwrap_or_else(|| secs(now)), secs(now), &live)?;
    Ok((freed, items))
}

/// Every unit of a profile directory in either layout, and the artifacts no unit owns.
fn units(dir: &Utf8Path) -> Result<(Vec<Found>, Vec<Utf8PathBuf>)> {
    let mut found = Vec::new();
    let mut orphans = Vec::new();
    let old = dir.join(".fingerprint");
    let has_old = old.is_dir();
    // To Cargo 1.99: `.fingerprint/<name>-<hash>/`, and `<name>-<hash>…` artifacts in `deps/`,
    // `build/` and `examples/` (and the gate's runner links in `run/`).
    let mut artifacts: HashMap<String, Vec<Utf8PathBuf>> = HashMap::new();
    let mut new_packages = Vec::new();
    for sub in ["deps", "build", "examples", "run"] {
        let Ok(entries) = dir.join(sub).read_dir_utf8() else { continue };
        for entry in entries {
            let entry = entry?;
            let path = entry.path().to_owned();
            match unit_hash(entry.file_name()) {
                Some(hash) => artifacts.entry(hash.to_owned()).or_default().push(path),
                // From Cargo 1.100: `build/<package>/<hash>/`.
                None if sub == "build" && entry.file_type()?.is_dir() => new_packages.push(path),
                None => {}
            }
        }
    }
    if has_old {
        for entry in old.read_dir_utf8().with_context(|| format!("read {old}"))? {
            let unit = entry?.into_path();
            let Some(name) = unit.file_name() else { continue };
            let Some(hash) = unit_hash(name) else { continue };
            let evidence = files_in(&unit);
            let mut paths = vec![unit.clone()];
            let owned = artifacts.remove(hash).unwrap_or_default();
            let objects = owned.iter().filter(|p| is_object(p)).cloned().collect();
            paths.extend(owned);
            found.push(Found { key: format!(".fingerprint/{name}"), paths, evidence, objects });
        }
    }
    // Artifacts are named `<crate>-<hash>` and cargo cannot use one whose unit is gone.
    orphans.extend(artifacts.into_values().flatten());
    if !has_old && !orphans.is_empty() {
        bail!(
            "{dir} holds artifacts named by unit hash but no `.fingerprint/`: a cargo layout \
             `xtask prune` does not know; update xtask/src/prune.rs before it prunes here"
        );
    }
    for package in new_packages {
        let Some(name) = package.file_name().map(str::to_owned) else { continue };
        for entry in package.read_dir_utf8().with_context(|| format!("read {package}"))? {
            let unit = entry?.into_path();
            let hash = unit.file_name().unwrap_or_default();
            if !(unit.is_dir() && is_hash(hash)) {
                bail!(
                    "{unit} is not a `build/<package>/<hash>/` unit: a cargo layout `xtask prune` \
                     does not know; update xtask/src/prune.rs before it prunes here"
                );
            }
            let evidence = files_in(&unit.join("fingerprint"));
            let objects =
                files_in(&unit.join("out")).into_iter().filter(|p| is_object(p)).collect();
            let key = format!("build/{name}/{hash}");
            found.push(Found { key, paths: vec![unit], evidence, objects });
        }
    }
    Ok((found, orphans))
}

/// The incremental caches of a profile directory. rustc starts a new session directory inside a
/// crate's cache on every compile, so the newest modification among the cache and its sessions
/// is its last compile.
fn caches(dir: &Utf8Path) -> Result<Vec<Item>> {
    let mut out = Vec::new();
    let Ok(entries) = dir.join("incremental").read_dir_utf8() else { return Ok(out) };
    for entry in entries {
        let cache = entry?.into_path();
        let evidence: Vec<Utf8PathBuf> = cache
            .read_dir_utf8()
            .map(|it| it.flatten().map(camino::Utf8DirEntry::into_path).collect())
            .unwrap_or_default();
        let last_use = evidence.iter().map(|p| modified(p)).fold(modified(&cache), SystemTime::max);
        out.push(Item { kind: Kind::Cache, paths: vec![cache], evidence, last_use });
    }
    Ok(out)
}

/// `build/<package>/` directories whose last unit went.
fn remove_empty_packages(dir: &Utf8Path) -> Result<()> {
    let Ok(entries) = dir.join("build").read_dir_utf8() else { return Ok(()) };
    for entry in entries {
        let entry = entry?;
        if unit_hash(entry.file_name()).is_none()
            && entry.file_type()?.is_dir()
            && entry.path().read_dir_utf8().is_ok_and(|mut it| it.next().is_none())
        {
            let _gone = std::fs::remove_dir(entry.path());
        }
    }
    Ok(())
}

/// The object files of a unit's earlier compiles. On macOS a binary keeps its debug info in its
/// `<stem>.<cgu>.<invocation>.rcgu.o` files (unpacked split debuginfo), and every compile names
/// them after its own invocation, so the previous ones stay behind. The newest invocation's are
/// the ones the binary was linked from; objects reused from the incremental cache keep their
/// old times, so an invocation counts by the newest of its files, and a tie keeps both.
fn superseded_objects(objects: &[Utf8PathBuf]) -> Vec<Utf8PathBuf> {
    let mut newest: HashMap<(&str, &str), SystemTime> = HashMap::new();
    let mut parsed = Vec::new();
    for path in objects {
        let Some(name) = path.file_name() else { continue };
        let mut parts = name.split('.');
        let (Some(stem), Some(_cgu), Some(invocation)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let time = modified(path);
        let slot = newest.entry((stem, invocation)).or_insert(UNIX_EPOCH);
        *slot = (*slot).max(time);
        parsed.push((path, stem, invocation));
    }
    let mut latest: HashMap<&str, SystemTime> = HashMap::new();
    for ((stem, _), time) in &newest {
        let slot = latest.entry(stem).or_insert(UNIX_EPOCH);
        *slot = (*slot).max(*time);
    }
    parsed
        .into_iter()
        .filter(|(_, stem, invocation)| newest.get(&(*stem, *invocation)) < latest.get(stem))
        .map(|(path, ..)| path.clone())
        .collect()
}

fn is_object(path: &Utf8Path) -> bool {
    path.file_name().is_some_and(|n| n.ends_with(".rcgu.o"))
}

fn files_in(dir: &Utf8Path) -> Vec<Utf8PathBuf> {
    dir.read_dir_utf8()
        .map(|it| it.flatten().map(camino::Utf8DirEntry::into_path).collect())
        .unwrap_or_default()
}

/// The unit hash in an artifact or unit directory name: `libfoo-0123456789abcdef.rlib`,
/// `foo-0123456789abcdef.foo.1a2b-cgu.0.rcgu.o`, `.fingerprint/foo-0123456789abcdef`.
fn unit_hash(name: &str) -> Option<&str> {
    let stem = name.split('.').next()?;
    let (_, hash) = stem.rsplit_once('-')?;
    is_hash(hash).then_some(hash)
}

fn is_hash(name: &str) -> bool {
    name.len() == 16 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

fn modified(path: &Utf8Path) -> SystemTime {
    std::fs::symlink_metadata(path).and_then(|m| m.modified()).unwrap_or(UNIX_EPOCH)
}

/// The later of the access and modification times: a file written since counts as used.
fn last_touched(path: &Utf8Path) -> SystemTime {
    let (accessed, modified) = times(path);
    accessed.max(modified)
}

/// A file's access and modification times, the epoch for one that is gone.
fn times(path: &Utf8Path) -> (SystemTime, SystemTime) {
    std::fs::symlink_metadata(path).map_or((UNIX_EPOCH, UNIX_EPOCH), |m| {
        (m.accessed().unwrap_or(UNIX_EPOCH), m.modified().unwrap_or(UNIX_EPOCH))
    })
}

/// Delete a file or directory tree; one already gone is fine.
fn remove(path: &Utf8Path) -> Result<()> {
    let result = if std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match result {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("remove {path}")),
    }
}

fn size_all(paths: &[Utf8PathBuf]) -> u64 {
    paths.iter().map(|p| size(p)).sum()
}

/// The bytes a file or tree holds on disk.
fn size(path: &Utf8Path) -> u64 {
    tally(path).bytes
}

/// A tree's bytes on disk: those of files with one name, and the inodes of files with several,
/// so a total counts a hard-linked file once.
#[derive(Default)]
struct Tally {
    bytes: u64,
    single: u64,
    linked: Vec<(u64, u64, u64)>,
}

fn tally(path: &Utf8Path) -> Tally {
    fn walk(path: &std::path::Path, out: &mut Tally) {
        let Ok(meta) = std::fs::symlink_metadata(path) else { return };
        let bytes = meta.blocks().saturating_mul(512);
        out.bytes = out.bytes.saturating_add(bytes);
        if meta.nlink() > 1 && !meta.is_dir() {
            out.linked.push((meta.dev(), meta.ino(), bytes));
        } else {
            out.single = out.single.saturating_add(bytes);
        }
        if meta.is_dir()
            && let Ok(entries) = std::fs::read_dir(path)
        {
            for entry in entries.flatten() {
                walk(&entry.path(), out);
            }
        }
    }
    let mut out = Tally::default();
    walk(path.as_std_path(), &mut out);
    out
}

/// The sizes a pass needs: the whole target directory's, each unit's and cache's paths', and
/// each top-level entry's. Walked on every core, since a cold walk of 450 000 files on the
/// external volume took 15–20 s on one.
struct Census {
    total: u64,
    sizes: HashMap<Utf8PathBuf, u64>,
    largest: Vec<(String, u64)>,
}

fn census(target: &Utf8Path, surveyed: &[(Utf8PathBuf, Vec<Item>)]) -> Census {
    let tracked: HashSet<&Utf8Path> = surveyed
        .iter()
        .flat_map(|(_, items)| items.iter().flat_map(|i| i.paths.iter().map(Utf8PathBuf::as_path)))
        .collect();
    let mut above: HashSet<&Utf8Path> = HashSet::new();
    for path in &tracked {
        for ancestor in path.ancestors().skip(1) {
            if !above.insert(ancestor) || ancestor == target {
                break;
            }
        }
    }
    let mut work: Vec<Utf8PathBuf> = tracked.iter().map(|p| (*p).to_owned()).collect();
    let mut stack = vec![target.to_owned()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = dir.read_dir_utf8() else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if tracked.contains(path) {
                continue;
            }
            if above.contains(path) {
                stack.push(path.to_owned());
            } else {
                work.push(path.to_owned());
            }
        }
    }
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let results: Vec<Vec<(usize, Tally)>> = std::thread::scope(|scope| {
        let handles: Vec<_> = std::iter::repeat_n((), threads)
            .map(|()| {
                scope.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(path) = work.get(index) else { break };
                        out.push((index, tally(path)));
                    }
                    out
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    let mut total = 0_u64;
    let mut inodes: HashMap<(u64, u64), u64> = HashMap::new();
    let mut sizes = HashMap::new();
    let mut largest: HashMap<String, u64> = HashMap::new();
    for (index, tally) in results.into_iter().flatten() {
        let Some(path) = work.get(index) else { continue };
        total = total.saturating_add(tally.single);
        for (dev, ino, bytes) in &tally.linked {
            inodes.insert((*dev, *ino), *bytes);
        }
        if let Some(top) = path.strip_prefix(target).ok().and_then(|p| p.components().next()) {
            let slot = largest.entry(top.as_str().to_owned()).or_default();
            *slot = slot.saturating_add(tally.bytes);
        }
        sizes.insert(path.clone(), tally.bytes);
    }
    total = total.saturating_add(inodes.values().sum());
    let mut largest: Vec<(String, u64)> = largest.into_iter().collect();
    largest.sort_by_key(|(_, bytes)| std::cmp::Reverse(*bytes));
    Census { total, sizes, largest }
}

#[cfg(test)]
mod tests;
