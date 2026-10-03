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
//!
//! A directory a build holds keeps its units and object files, since the build may link any
//! unit's rlib. Its incremental caches still go, one session at a time under the session's own
//! lock, as rustc's collector deletes them: with several agents building in `target/debug` all
//! day, its lock is almost never free, and its idle caches are most of what it holds.

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

/// What each fork or study checkout's build output under `.research/` may hold, in GB
/// (`SLOPTY_FORK_BUDGET_GB`). The gpui-fast and gpui-kit forks each grew past 85 GB in a day of
/// syncs and A/B builds and filled the volume (2026-09-30), since nothing pruned them.
pub const DEFAULT_FORK_BUDGET_GB: u64 = 40;

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
        let var = |name: &str| std::env::var(name).ok();
        Ok(Self {
            budget: gigabytes("SLOPTY_TARGET_BUDGET_GB", var, DEFAULT_BUDGET_GB)?,
            floor: gigabytes("SLOPTY_DISK_FLOOR_GB", var, DEFAULT_FLOOR_GB)?,
        })
    }
}

/// Bytes from the GB variable `name` as `var` reads it, else `default` GB. An empty value is
/// unset: CI's matrix sets the floor for one job and leaves it empty on the others.
fn gigabytes(name: &str, var: impl Fn(&str) -> Option<String>, default: u64) -> Result<u64> {
    let value = match var(name) {
        Some(v) if !v.trim().is_empty() => {
            v.trim().parse().with_context(|| format!("{name}={v} is not a number"))?
        }
        _ => default,
    };
    Ok(value.saturating_mul(GB))
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
    /// Profile directories a build held, so their units stayed, by name.
    pub busy: Vec<Busy>,
    /// Bytes per top-level entry of the target directory, largest first.
    pub largest: Vec<(String, u64)>,
    /// Bytes of units and caches by time since last use: under 1 h, 6 h, 24 h, 3 days, older.
    pub ages: [u64; 5],
    /// How long the sweep of the profile directories and the size walk took.
    pub sweep: Duration,
    pub walk: Duration,
}

/// A profile directory a build held during the pass: its units stayed, and its caches went
/// session by session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Busy {
    /// Relative to the target directory.
    pub dir: String,
    /// The caches swept whole (idle or for the budget), and the bytes of every session that went.
    pub swept: Freed,
    /// Sessions a compile held, left for a later pass.
    pub sessions_held: usize,
    /// Bytes the pass would have deleted but the build held: units the budget or the floor
    /// reached, and sessions in use.
    pub held_back: u64,
}

impl Report {
    fn on_busy(&mut self, dir: &str, record: impl FnOnce(&mut Busy)) {
        if let Some(busy) = self.busy.iter_mut().find(|b| b.dir == dir) {
            record(busy);
        } else {
            let mut busy = Busy { dir: dir.to_owned(), ..Busy::default() };
            record(&mut busy);
            self.busy.push(busy);
        }
    }
}

/// `cargo xtask prune`: every profile directory, waiting for busy ones if asked, with a report;
/// the fork checkouts' first, so the floor is met from them before Slopty's own.
pub fn run(opts: Options) -> Result<()> {
    let root = repo_root()?;
    let fork_limits = Limits { budget: fork_budget()?, floor: opts.limits.floor };
    for fork in fork_targets(&root) {
        let report =
            prune(&fork, Options { limits: fork_limits, ..opts }, SystemTime::now(), &free_space)?;
        println!("▶ {fork}");
        print(&fork, &report, opts.dry_run);
    }
    let target = root.join("target");
    let report = prune(&target, opts, SystemTime::now(), &free_space)?;
    println!("▶ {target}");
    print(&target, &report, opts.dry_run);
    Ok(())
}

/// `SLOPTY_FORK_BUDGET_GB`, else [`DEFAULT_FORK_BUDGET_GB`], in bytes.
fn fork_budget() -> Result<u64> {
    let gb = match std::env::var("SLOPTY_FORK_BUDGET_GB") {
        Ok(v) => v
            .trim()
            .parse()
            .with_context(|| format!("SLOPTY_FORK_BUDGET_GB={v} is not a number"))?,
        Err(_) => DEFAULT_FORK_BUDGET_GB,
    };
    Ok(u64::saturating_mul(gb, GB))
}

/// The build directories of the checkouts under `.research/`: each `target*` directory of a
/// checkout that holds cargo's `CACHEDIR.TAG`.
pub fn fork_targets(root: &Utf8Path) -> Vec<Utf8PathBuf> {
    let dirs = |dir: &Utf8Path| -> Vec<Utf8PathBuf> {
        dir.read_dir_utf8().map_or_else(
            |_| Vec::new(),
            |entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .map(|e| e.path().to_owned())
                    .collect()
            },
        )
    };
    let mut out: Vec<Utf8PathBuf> = dirs(&root.join(".research"))
        .iter()
        .flat_map(|checkout| dirs(checkout))
        .filter(|d| d.file_name().is_some_and(|n| n.starts_with("target")))
        .filter(|d| d.join("CACHEDIR.TAG").is_file())
        .collect();
    out.sort();
    out
}

/// The fork checkouts' pass, never failing its caller: idle units, then each fork's budget and
/// the volume's floor.
fn prune_forks(limits: Limits, quiet: bool) {
    let pass = || -> Result<()> {
        let root = repo_root()?;
        let opts = Options {
            idle: Duration::from_secs(DEFAULT_IDLE_HOURS.saturating_mul(3600)),
            limits: Limits { budget: fork_budget()?, floor: limits.floor },
            wait: false,
            dry_run: false,
        };
        for fork in fork_targets(&root) {
            let report = prune(&fork, opts, SystemTime::now(), &free_space)?;
            let mut freed = report.idle;
            freed.add(report.budget);
            if !(quiet && freed.is_empty()) {
                println!("  prune {fork}: {freed}; {} left", gb(report.size_after));
            }
        }
        Ok(())
    };
    if let Err(e) = pass() {
        eprintln!("  prune of the fork checkouts skipped: {e:#}");
    }
}

/// The pass after `check` and `gate`: skips busy directories, never fails the command.
pub fn auto() {
    if let Ok(limits) = Limits::from_env() {
        prune_forks(limits, true);
    }
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
    // The forks' build output rebuilds without holding up a gate: it goes first.
    prune_forks(limits, false);
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
    let held: Vec<String> = report
        .busy
        .iter()
        .filter(|b| b.held_back > 0)
        .map(|b| format!("{} ({})", b.dir, gb(b.held_back)))
        .collect();
    // Only when a build held what the pass would have deleted is "busy" why it fell short.
    let busy = if held.is_empty() {
        String::new()
    } else {
        format!("\nbusy, held by a build and so not pruned: {}", held.join(", "))
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
    let swept = if dry_run { "would be swept" } else { "swept" };
    for busy in &report.busy {
        let held = if busy.sessions_held > 0 {
            format!(", {} sessions in use kept", busy.sessions_held)
        } else {
            String::new()
        };
        println!(
            "  {}: busy, units kept; {} caches {swept}, {}{held}",
            busy.dir,
            busy.swept.caches,
            gb(busy.swept.bytes)
        );
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

/// A profile directory's sweep: what it freed and what is left for the budget. `busy` is set
/// when a build held it, and then no item of kind [`Kind::Unit`] may go.
struct Swept {
    freed: Freed,
    items: Vec<Item>,
    busy: Option<Busy>,
}

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
    let swept: Vec<Result<Swept>> = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(dirs.len());
        for dir in &dirs {
            handles.push(scope.spawn(move || match Locks::take(dir, opts.wait)? {
                Some(_locks) => {
                    let (freed, items) = sweep(dir, opts, now)?;
                    Ok(Swept { freed, items, busy: None })
                }
                None => Ok(sweep_busy(dir, rel(target, dir), opts, now)),
            }));
        }
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Err(anyhow::anyhow!("a prune thread panicked"))))
            .collect()
    });
    let mut surveyed: Vec<Surveyed> = Vec::new();
    for (dir, result) in dirs.iter().zip(swept) {
        let Swept { freed, items, busy } = result?;
        report.idle.add(freed);
        let was_busy = busy.is_some();
        report.busy.extend(busy);
        surveyed.push(Surveyed { dir: dir.clone(), busy: was_busy, items });
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
    for (d, Surveyed { items, .. }) in surveyed.iter().enumerate() {
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
            let Some(Surveyed { dir, busy, items }) = surveyed.get(d) else { continue };
            if *busy && items.get(i).is_some_and(|item| item.kind == Kind::Unit) {
                report.on_busy(rel(target, dir), |b| {
                    b.held_back = b.held_back.saturating_add(bytes);
                });
                continue;
            }
            planned = planned.saturating_add(bytes);
            cutoff = cutoff.max(last_use);
            chosen.entry(d).or_default().push(i);
        }
    }
    for (d, picks) in &chosen {
        let Some(Surveyed { dir, items, .. }) = surveyed.get(*d) else { continue };
        let locks = Locks::take(dir, opts.wait)?;
        let name = rel(target, dir);
        for item in picks.iter().filter_map(|i| items.get(*i)) {
            // A build since the sweep read it: it is in use after all.
            if item.last_use_now() > cutoff {
                continue;
            }
            let bytes: u64 = item.paths.iter().filter_map(|p| census.sizes.get(p)).sum();
            match (&locks, item.kind) {
                (Some(_), Kind::Unit) => {
                    item.remove(opts.dry_run)?;
                    report.budget.bytes = report.budget.bytes.saturating_add(bytes);
                    report.budget.units = report.budget.units.saturating_add(1);
                }
                // As in the sweep, a cache that cannot go stays and the pass goes on.
                (Some(_), Kind::Cache) => {
                    if item.remove(opts.dry_run).is_ok() {
                        report.budget.bytes = report.budget.bytes.saturating_add(bytes);
                        report.budget.caches = report.budget.caches.saturating_add(1);
                    }
                }
                (None, Kind::Cache) => {
                    let swept = item.paths.first().map(|c| sweep_sessions(c, opts.dry_run));
                    let swept = swept.unwrap_or_default();
                    report.budget.add(swept.freed());
                    report.on_busy(name, |b| b.record(swept));
                }
                (None, Kind::Unit) => report.on_busy(name, |b| {
                    b.held_back = b.held_back.saturating_add(bytes);
                }),
            }
        }
        if locks.is_some() && !opts.dry_run {
            remove_empty_packages(dir)?;
        }
    }
    report.busy.sort_by(|a, b| a.dir.cmp(&b.dir));
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

/// A profile directory after its sweep, as the budget sees it.
struct Surveyed {
    dir: Utf8PathBuf,
    /// A build held it: its units stay.
    busy: bool,
    items: Vec<Item>,
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

impl Found {
    /// Its last use (the ledger's, its fingerprint files' reads, its writes) and its last write.
    fn last_use(&self, ledger: &Ledger) -> (SystemTime, SystemTime) {
        let mut last_use = ledger.get(&self.key);
        let mut written = UNIX_EPOCH;
        for path in self.paths.iter().take(1) {
            written = written.max(modified(path));
        }
        for file in &self.evidence {
            let (accessed, modified) = times(file);
            written = written.max(modified);
            last_use = last_use.max(accessed);
        }
        (last_use.max(written), written)
    }
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
        let (last_use, written) = unit.last_use(&ledger);
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
    for cache in caches(dir) {
        if cache.last_use < idle_since {
            let bytes = size_all(&cache.paths);
            // One that cannot go (a directory it may not read) stays; the pass goes on.
            if cache.remove(opts.dry_run).is_ok() {
                freed.bytes = freed.bytes.saturating_add(bytes);
                freed.caches = freed.caches.saturating_add(1);
            }
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
/// is its last compile. Only directories are caches: rustc writes nothing else there, and an
/// entry it cannot have written (a file, a name that is not UTF-8) is left alone.
fn caches(dir: &Utf8Path) -> Vec<Item> {
    let mut out = Vec::new();
    let Ok(entries) = dir.join("incremental").read_dir_utf8() else { return out };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let cache = entry.into_path();
        let evidence = files_in(&cache);
        let last_use = evidence.iter().map(|p| modified(p)).fold(modified(&cache), SystemTime::max);
        out.push(Item { kind: Kind::Cache, paths: vec![cache], evidence, last_use });
    }
    out
}

/// The sweep of a profile directory a build holds. Its units and object files stay, since the
/// build may link any unit's rlib, and cargo's locks are what guard them. Its idle caches go
/// session by session under rustc's own locks ([`sweep_sessions`]). Its units are read for the
/// budget, which counts what the build held back, and nothing of theirs is written: no ledger,
/// no access time.
fn sweep_busy(dir: &Utf8Path, name: &str, opts: Options, now: SystemTime) -> Swept {
    let idle_since = now.checked_sub(opts.idle).unwrap_or(UNIX_EPOCH);
    let mut busy = Busy { dir: name.to_owned(), ..Busy::default() };
    let mut items = Vec::new();
    for cache in caches(dir) {
        match cache.paths.first() {
            Some(path) if cache.last_use < idle_since => {
                busy.record(sweep_sessions(path, opts.dry_run));
            }
            _ => items.push(cache),
        }
    }
    let ledger = Ledger::read(dir);
    // Without a ledger no unit's idleness is known, so none is one the budget could have taken.
    // A layout mid-write reads as an error here, and then only the count of what was held is
    // lower; the sweep under the lock is where a layout it does not know stops the pass.
    if ledger.since.is_some()
        && let Ok((units, _)) = units(dir)
    {
        for unit in units {
            let (last_use, _) = unit.last_use(&ledger);
            let Found { paths, evidence, .. } = unit;
            items.push(Item { kind: Kind::Unit, paths, evidence, last_use });
        }
    }
    Swept { freed: busy.swept, items, busy: Some(busy) }
}

impl Busy {
    fn record(&mut self, sessions: Sessions) {
        self.swept.add(sessions.freed());
        self.sessions_held = self.sessions_held.saturating_add(sessions.held);
        self.held_back = self.held_back.saturating_add(sessions.held_bytes);
    }
}

/// What [`sweep_sessions`] did with one cache.
#[derive(Clone, Copy, Debug, Default)]
struct Sessions {
    /// Session directories and stray lock files that went.
    removed: usize,
    bytes: u64,
    /// Sessions a compile held, and their bytes.
    held: usize,
    held_bytes: u64,
}

impl Sessions {
    /// A cache counts once it lost something and kept nothing in use.
    fn freed(self) -> Freed {
        let whole = self.removed > 0 && self.held == 0;
        Freed { caches: usize::from(whole), bytes: self.bytes, ..Freed::default() }
    }
}

/// Deletes an incremental cache's sessions without cargo's lock, as rustc's own collector does
/// (`rustc_incremental::persist::fs`, `garbage_collect_session_directories`). A session
/// `s-<time>-<random>-<svh>` (or `-working` while a compile writes it) has its lock file
/// `s-<time>-<random>.lock` beside it. A compile holds that lock exclusively while it writes the
/// session and shared while it reads one; the collector deletes a session only under the lock
/// taken exclusively without waiting, then its lock file, and leaves one it cannot lock. rustc
/// creates and locks a lock file before its session, so a session without one is debris and
/// goes outright, and a lock file without a session goes under its lock. rustc spares a
/// `-working` session younger than ten seconds, the moment between creating its lock file and
/// locking it; every cache here was untouched for an hour at least. Anything else (a name rustc
/// does not write, a directory it may not read) stays, and the pass goes on. The cache's own
/// directory stays even when empty: rustc creates it, then its lock file inside, and a
/// directory removed between the two fails that compile.
fn sweep_sessions(cache: &Utf8Path, dry_run: bool) -> Sessions {
    let mut out = Sessions::default();
    let Ok(entries) = cache.read_dir_utf8() else { return out };
    let mut sessions = Vec::new();
    let mut locks = HashSet::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.starts_with("s-") {
            continue;
        }
        if Utf8Path::new(name).extension() == Some("lock") {
            locks.insert(name.to_owned());
        } else if entry.file_type().is_ok_and(|t| t.is_dir()) {
            sessions.push(name.to_owned());
        }
    }
    let mut matched = HashSet::new();
    for session in &sessions {
        let Some(lock) = session_lock(session) else { continue };
        let path = cache.join(session);
        let has_lock = locks.contains(&lock);
        if has_lock {
            matched.insert(lock.clone());
        }
        let guard = match has_lock.then(|| claim(&cache.join(&lock))) {
            Some(None) => {
                out.held = out.held.saturating_add(1);
                out.held_bytes = out.held_bytes.saturating_add(size(&path));
                continue;
            }
            claimed => claimed.flatten(),
        };
        let bytes = size(&path);
        if !dry_run {
            if remove(&path).is_err() {
                out.bytes = out.bytes.saturating_add(bytes.saturating_sub(size(&path)));
                continue;
            }
            if guard.is_some() {
                let _gone = remove(&cache.join(&lock));
            }
        }
        // Held until the session and its lock file are gone, as rustc holds it.
        drop(guard);
        out.removed = out.removed.saturating_add(1);
        out.bytes = out.bytes.saturating_add(bytes);
    }
    for lock in locks.difference(&matched) {
        let path = cache.join(lock);
        let Some(guard) = claim(&path) else { continue };
        if dry_run || remove(&path).is_ok() {
            out.removed = out.removed.saturating_add(1);
        }
        drop(guard);
    }
    out
}

/// A session's lock file, as rustc names it (`lock_file_path`): the session's name up to its
/// third dash, then `.lock`. `None` for a name without three dashes, which rustc never writes.
fn session_lock(session: &str) -> Option<String> {
    let dashes: Vec<usize> = session.match_indices('-').map(|(i, _)| i).collect();
    let [_, _, third] = dashes.as_slice() else { return None };
    Some(format!("{}.lock", session.get(..*third)?))
}

/// A session's lock, taken exclusively without waiting and never creating the file, or `None`
/// when a compile holds it (or the file cannot be opened). rustc 1.99 locks with
/// `fcntl(F_SETLK)` on macOS and with `flock` on Linux; nightly locks with `flock` (std's
/// `File::try_lock`) everywhere. Darwin keeps both kinds in one lock list, so either blocks this
/// `flock`; a test holds each.
fn claim(lock: &Utf8Path) -> Option<File> {
    let file = File::open(lock).ok()?;
    file.try_lock().ok()?;
    Some(file)
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

fn census(target: &Utf8Path, surveyed: &[Surveyed]) -> Census {
    let tracked: HashSet<&Utf8Path> = surveyed
        .iter()
        .flat_map(|s| s.items.iter().flat_map(|i| i.paths.iter().map(Utf8PathBuf::as_path)))
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
