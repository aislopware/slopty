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
//! - **the object files no link reads.** On macOS a test or binary keeps its debug info in the
//!   `.rcgu.o` files beside it, named by the rustc invocation that wrote them, and nothing deletes
//!   the previous invocation's. They made up 99 782 of the 102 568 entries of the tests lane's
//!   `deps/`, and every process that opens `VideoToolbox` or `CoreAudio`, or draws an SF Symbol,
//!   pays for the entries in its executable's directory (`docs/decisions/tooling.md`, "A test
//!   binary's directory, not the volume" and "Prune deletes every object no link reads"). An
//!   executable's debug map names the ones it reads; a library's are read by nobody once its rlib
//!   is written.
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

/// What `target/` may hold, in GB (`SLOPTY_TARGET_BUDGET_GB`). Lanes check their own crates here
/// at full power (`docs/decisions/tooling.md`, "Checks run where they return soonest"), and a
/// warm unit is a compile saved: a full gate's lanes took 32 GB and a day of agents' builds
/// about 60 GB, so this holds several days of them. The volume's floor still guards the disk.
pub const DEFAULT_BUDGET_GB: u64 = 200;

/// The free space `target/`'s volume keeps, in GB (`SLOPTY_DISK_FLOOR_GB`): a lane's cold local
/// check of the app's crates writes up to about 30 GB, so one that starts above the floor
/// finishes before the disk does. It was 50 while a full gate, about 35 GB cold, built here;
/// the gate runs on hosted runners now (`docs/decisions/tooling.md`, "The gate and e2e run on
/// free hosted runners"), and at 50 with this volume 75-85 GB free a pass acted after almost
/// every check, evicting units idle for an hour that the next check built again (1 501 units,
/// 21.5 GB, in two passes on 2026-10-31; "A lane's check returns in minutes").
pub const DEFAULT_FLOOR_GB: u64 = 30;

/// The free space a pass that acts leaves above the floor, in GB (`SLOPTY_DISK_HEADROOM_GB`):
/// what the checks that start before the next pass may write. A pass runs after each check, but
/// lanes build side by side, and on 2026-10-12 the volume fell under the floor while
/// 30 GB of units sat unused for 1 to 6 hours: a lane's local check of the app's crates writes
/// up to about 30 GB cold, and a fork's cold build more. A pass that has to act frees this much
/// more than the floor, least recently used first, so the floor holds between passes.
pub const DEFAULT_HEADROOM_GB: u64 = 30;

/// What each fork or study checkout's build output under `.research/` may hold, in GB
/// (`SLOPTY_FORK_BUDGET_GB`). The gpui-fast and gpui-kit forks each grew past 85 GB in a day of
/// syncs and A/B builds and filled the volume (2026-09-30), since nothing pruned them.
pub const DEFAULT_FORK_BUDGET_GB: u64 = 80;

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
    /// The floor a pass holds once it acts: the floor and the headroom above it
    /// (`SLOPTY_DISK_HEADROOM_GB`, else [`DEFAULT_HEADROOM_GB`]).
    pub fn with_headroom(self) -> Result<Self> {
        let var = |name: &str| std::env::var(name).ok();
        let headroom = gigabytes("SLOPTY_DISK_HEADROOM_GB", var, DEFAULT_HEADROOM_GB)?;
        Ok(Self { floor: self.floor.saturating_add(headroom), ..self })
    }

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

/// `cargo xtask prune`: every build directory (`target/` and the fork checkouts'), waiting for
/// busy ones if asked, with a report each.
pub fn run(opts: Options) -> Result<()> {
    let root = repo_root()?;
    let targets = targets(&root, opts.limits.budget)?;
    let opts = Options { limits: opts.limits.with_headroom()?, ..opts };
    let reports = prune_all(&targets, opts, SystemTime::now(), &free_space)?;
    for ((dir, _), report) in targets.iter().zip(&reports) {
        println!("▶ {dir}");
        print(dir, report, opts.dry_run);
    }
    let target = root.join("target");
    let gone = prebuilt(&target.join(PREBUILT), SystemTime::now(), opts.dry_run)?;
    if gone > 0 {
        println!("  {gone} prebuilt libghostty-vt unused for {PREBUILT_IDLE_DAYS} days");
    }
    Ok(())
}

/// The build directories a pass covers, each with its budget: `target/` with `budget`, first,
/// since the volume's free space is read on it, then each fork checkout's with
/// [`fork_budget`].
fn targets(root: &Utf8Path, budget: u64) -> Result<Vec<(Utf8PathBuf, u64)>> {
    let fork = fork_budget()?;
    let mut out = vec![(root.join("target"), budget)];
    out.extend(fork_targets(root).into_iter().map(|dir| (dir, fork)));
    Ok(out)
}

/// Where libghostty-vt-sys keeps a library built for each set of inputs, under `target/`
/// (`LIBGHOSTTY_VT_SYS_PREBUILT_DIR`, `docs/decisions/tooling.md`, "libghostty-vt is built once
/// per set of inputs").
pub const PREBUILT: &str = "ghostty-prebuilt";

/// How long a prebuilt library may go unused. A ghostty bump leaves every entry of the old
/// revision behind; one in use is touched on every build that takes it.
const PREBUILT_IDLE_DAYS: u64 = 14;

/// Deletes the prebuilt libraries in `dir` whose key file was last touched over
/// [`PREBUILT_IDLE_DAYS`] ago, and a publish's staging directory left over a day by a build that
/// died; returns how many went. A missing `dir` holds none.
fn prebuilt(dir: &Utf8Path, now: SystemTime, dry_run: bool) -> Result<usize> {
    let Ok(entries) = dir.read_dir_utf8() else { return Ok(0) };
    let mut gone = 0_usize;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let staging = entry.file_name().starts_with('.');
        let stamp = if staging { path.to_owned() } else { path.join(".libghostty-vt-sys-key") };
        let used = std::fs::metadata(&stamp).and_then(|m| m.modified()).unwrap_or(UNIX_EPOCH);
        let idle = if staging { 1 } else { PREBUILT_IDLE_DAYS };
        let limit = Duration::from_secs(idle.saturating_mul(24 * 3600));
        if now.duration_since(used).unwrap_or_default() > limit {
            if !dry_run {
                std::fs::remove_dir_all(path).with_context(|| format!("remove {path}"))?;
            }
            gone = gone.saturating_add(1);
        }
    }
    Ok(gone)
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

/// The pass after a check (`check`, `gate`, and `remote`'s after-pass once its verdict is out):
/// every build directory, busy ones skipped, held to the floor and its headroom; never fails its
/// caller.
pub fn auto() {
    let pass = || -> Result<Vec<(Utf8PathBuf, Report)>> {
        let root = repo_root()?;
        let limits = Limits::from_env()?;
        let targets = targets(&root, limits.budget)?;
        let opts = Options {
            idle: Duration::from_secs(DEFAULT_IDLE_HOURS.saturating_mul(3600)),
            limits: limits.with_headroom()?,
            wait: false,
            dry_run: false,
        };
        let reports = prune_all(&targets, opts, SystemTime::now(), &free_space)?;
        Ok(targets.into_iter().map(|(dir, _)| dir).zip(reports).collect())
    };
    match pass() {
        Ok(passes) => {
            for (dir, report) in passes {
                let mut freed = report.idle;
                freed.add(report.budget);
                if !freed.is_empty() {
                    println!(
                        "  prune {dir}: {freed}; {} left, {} free",
                        gb(report.size_after),
                        gb(report.free_after)
                    );
                }
                if report.short > 0 {
                    eprintln!(
                        "  prune {dir}: still {} over its budget or under the floor and its \
                         headroom",
                        gb(report.short)
                    );
                }
            }
        }
        Err(e) => eprintln!("  prune skipped: {e:#}"),
    }
    if let Ok(root) = repo_root()
        && let Err(e) = prebuilt(&root.join("target").join(PREBUILT), SystemTime::now(), false)
    {
        eprintln!("  prune {PREBUILT} skipped: {e:#}");
    }
}

/// Before a build: when the volume has less than the floor and its headroom free, the pass after
/// a check runs first, so the build's own output cannot take the volume under the floor.
pub fn before_build() {
    let room = || -> Result<bool> {
        let limits = Limits::from_env()?.with_headroom()?;
        Ok(free_space(&repo_root()?)? >= limits.floor)
    };
    if !room().unwrap_or(false) {
        auto();
    }
}

/// Before a gate: its volume must have the floor free. Below it, prune every build directory to
/// the floor and its headroom; still below the floor, refuse with where the space went.
pub fn ensure_room(target: &Utf8Path) -> Result<()> {
    let limits = Limits::from_env()?;
    std::fs::create_dir_all(target).with_context(|| format!("create {target}"))?;
    if free_space(target)? >= limits.floor {
        return Ok(());
    }
    let root = repo_root()?;
    let targets = targets(&root, limits.budget)?;
    let opts = Options {
        idle: Duration::from_secs(DEFAULT_IDLE_HOURS.saturating_mul(3600)),
        limits: limits.with_headroom()?,
        wait: false,
        dry_run: false,
    };
    let reports = prune_all(&targets, opts, SystemTime::now(), &free_space)?;
    for ((dir, _), report) in targets.iter().zip(&reports) {
        print(dir, report, false);
    }
    let report = reports.first().context("a pass reports on target/")?;
    check_room(target, report, limits)
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

/// One pass over `target` alone: the idle sweep in every profile directory, then the budget and
/// the floor. `free` reads a volume's free space.
#[cfg(test)]
fn prune(
    target: &Utf8Path,
    opts: Options,
    now: SystemTime,
    free: &dyn Fn(&Utf8Path) -> Result<u64>,
) -> Result<Report> {
    let mut reports = prune_all(&[(target.to_owned(), opts.limits.budget)], opts, now, free)?;
    reports.pop().context("a pass reports on the directory it was given")
}

/// One pass over several build directories on one volume, each with its byte budget: the idle
/// sweep in each, then each one's budget from its own least recently used, then the volume's
/// floor (`opts.limits.floor`) from the least recently used of them all, whichever directory
/// holds it. One pool keeps the warmest builds: a fork's unit used an hour ago outlives a lane's
/// untouched since the morning. A report per directory, in order.
pub fn prune_all(
    targets: &[(Utf8PathBuf, u64)],
    opts: Options,
    now: SystemTime,
    free: &dyn Fn(&Utf8Path) -> Result<u64>,
) -> Result<Vec<Report>> {
    let mut surveys = targets
        .iter()
        .map(|(dir, budget)| survey(dir, *budget, opts, now))
        .collect::<Result<Vec<_>>>()?;
    let Some(volume) = surveys.first().map(|s| s.target.clone()) else { return Ok(Vec::new()) };
    // A dry run deleted nothing, so the volume still holds what the sweeps would delete.
    let pending: u64 =
        if opts.dry_run { surveys.iter().map(|s| s.report.idle.bytes).sum() } else { 0 };
    let free_before = free(&volume)?.saturating_add(pending);

    let mut candidates: Vec<Candidate> = Vec::new();
    for (s, survey) in surveys.iter_mut().enumerate() {
        survey.report.free_before = free_before;
        for (d, Surveyed { items, .. }) in survey.surveyed.iter().enumerate() {
            for (i, item) in items.iter().enumerate() {
                let bytes: u64 = item.paths.iter().filter_map(|p| survey.census.sizes.get(p)).sum();
                let age = now.duration_since(item.last_use).unwrap_or_default().as_secs();
                let bucket =
                    [1_u64, 6, 24, 72].iter().take_while(|h| age >= h.saturating_mul(3600)).count();
                if let Some(slot) = survey.report.ages.get_mut(bucket) {
                    *slot = slot.saturating_add(bytes);
                }
                candidates.push(Candidate {
                    s,
                    d,
                    i,
                    last_use: item.last_use,
                    bytes,
                    fate: Fate::Kept,
                });
            }
        }
    }
    candidates.sort_by_key(|c| c.last_use);

    let mut plan = Plan { guard: now.checked_sub(GUARD).unwrap_or(UNIX_EPOCH), cutoff: UNIX_EPOCH };
    let mut over = vec![false; surveys.len()];
    let mut planned = 0_u64;
    // Each directory's budget, from its own least recently used, to a tenth under it.
    for (s, survey) in surveys.iter_mut().enumerate() {
        if survey.report.size_before <= survey.budget {
            continue;
        }
        if let Some(flag) = over.get_mut(s) {
            *flag = true;
        }
        let excess = survey
            .report
            .size_before
            .saturating_sub(survey.budget.saturating_sub(survey.budget / 10));
        let mine = candidates.iter_mut().filter(|c| c.s == s);
        planned = planned.saturating_add(plan.take(mine, excess, std::slice::from_mut(survey), s));
    }
    // The volume's floor, from the least recently used of every directory, to a tenth over it.
    let floor = opts.limits.floor;
    let under = free_before < floor;
    if under {
        over.fill(true);
        let excess =
            floor.saturating_add(floor / 10).saturating_sub(free_before).saturating_sub(planned);
        plan.take(candidates.iter_mut(), excess, &mut surveys, 0);
    }

    for (s, survey) in surveys.iter_mut().enumerate() {
        let picks = candidates.iter().filter(|c| c.s == s && c.fate == Fate::Chosen);
        let mut by_dir: HashMap<usize, Vec<usize>> = HashMap::new();
        for c in picks {
            by_dir.entry(c.d).or_default().push(c.i);
        }
        survey.remove(&by_dir, plan.cutoff, opts)?;
    }
    let deleted: u64 = surveys.iter().map(|s| s.report.budget.bytes).sum();
    let free_after =
        if opts.dry_run { free_before.saturating_add(deleted) } else { free(&volume)? };
    let mut reports = Vec::with_capacity(surveys.len());
    for (survey, over) in surveys.into_iter().zip(over) {
        let mut report = survey.report;
        report.busy.sort_by(|a, b| a.dir.cmp(&b.dir));
        report.size_after = report.size_before.saturating_sub(report.budget.bytes);
        report.free_after = free_after;
        if over {
            let size_short = report.size_after.saturating_sub(survey.budget);
            let free_short = floor.saturating_sub(free_after);
            report.short = size_short.max(free_short);
        }
        reports.push(report);
    }
    Ok(reports)
}

/// A unit or cache any directory of a pass could give up, in the order the pass would.
struct Candidate {
    /// Its survey, profile directory and item.
    s: usize,
    d: usize,
    i: usize,
    last_use: SystemTime,
    bytes: u64,
    fate: Fate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fate {
    Kept,
    Chosen,
    /// A unit of a directory a build holds, which the pass reached and had to leave.
    Held,
}

/// What the budget and floor passes chose so far.
struct Plan {
    /// Nothing used after it goes.
    guard: SystemTime,
    /// The latest last use among what was chosen: anything used since is in use after all.
    cutoff: SystemTime,
}

impl Plan {
    /// Choose from `candidates`, least recently used first, until `excess` bytes; returns the
    /// bytes chosen. `surveys` are theirs, the first being survey `first`. A busy directory's
    /// unit is held back, not chosen, and said so.
    fn take<'c>(
        &mut self,
        candidates: impl Iterator<Item = &'c mut Candidate>,
        excess: u64,
        surveys: &mut [Survey],
        first: usize,
    ) -> u64 {
        let mut planned = 0_u64;
        let recent = self.guard;
        for c in candidates {
            if planned >= excess || c.last_use > recent {
                break;
            }
            // A unit whose blocks a clone shares all frees nothing (`own_bytes`): it stays.
            if c.fate != Fate::Kept || c.bytes == 0 {
                continue;
            }
            let Some(survey) = surveys.get_mut(c.s.saturating_sub(first)) else { continue };
            let Some(Surveyed { dir, busy, items }) = survey.surveyed.get(c.d) else { continue };
            if *busy && items.get(c.i).is_some_and(|item| item.kind == Kind::Unit) {
                let name = rel(&survey.target, dir).to_owned();
                survey.report.on_busy(&name, |b| b.held_back = b.held_back.saturating_add(c.bytes));
                c.fate = Fate::Held;
                continue;
            }
            planned = planned.saturating_add(c.bytes);
            self.cutoff = self.cutoff.max(c.last_use);
            c.fate = Fate::Chosen;
        }
        planned
    }
}

/// A build directory after its idle sweep, sized, as the budget and the floor see it.
struct Survey {
    target: Utf8PathBuf,
    budget: u64,
    surveyed: Vec<Surveyed>,
    census: Census,
    report: Report,
}

/// The idle sweep of every profile directory in `target`, side by side, and its sizes.
fn survey(target: &Utf8Path, budget: u64, opts: Options, now: SystemTime) -> Result<Survey> {
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
    report.largest.clone_from(&census.largest);
    Ok(Survey { target: target.to_owned(), budget, surveyed, census, report })
}

impl Survey {
    /// Delete what was chosen, `by_dir` per profile directory, under each one's locks; what was
    /// used after `cutoff` stays.
    fn remove(
        &mut self,
        by_dir: &HashMap<usize, Vec<usize>>,
        cutoff: SystemTime,
        opts: Options,
    ) -> Result<()> {
        let report = &mut self.report;
        for (d, picks) in by_dir {
            let Some(Surveyed { dir, items, .. }) = self.surveyed.get(*d) else { continue };
            let locks = Locks::take(dir, opts.wait)?;
            let name = rel(&self.target, dir);
            for item in picks.iter().filter_map(|i| items.get(*i)) {
                // A build since the sweep read it: it is in use after all.
                if item.last_use_now() > cutoff {
                    continue;
                }
                let bytes: u64 = item.paths.iter().filter_map(|p| self.census.sizes.get(p)).sum();
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
        Ok(())
    }
}

fn rel<'a>(target: &Utf8Path, dir: &'a Utf8Path) -> &'a str {
    dir.strip_prefix(target).map_or(dir.as_str(), Utf8Path::as_str)
}

/// Every cargo profile directory under `target`: one holding cargo's `.cargo-lock`. They sit at
/// most five levels down (`local/<lane>/target/<triple>/debug`).
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
    walk(target, 5, &mut out)?;
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
            leftovers.extend(unread_objects(&unit.objects, now));
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

/// The sweep of a profile directory a build holds. Its units stay, since the build may link any
/// unit's rlib, and cargo's locks are what guard them. The objects no link reads go all the same
/// ([`unread_objects`], sparing an hour of a compile's writes): `target/debug` is held almost all
/// day by the agents' builds, which left 145 000 of them in one `deps/`
/// (`docs/decisions/tooling.md`, "Prune deletes every object no link reads"). Its idle caches go
/// session by session under rustc's own locks ([`sweep_sessions`]). Its units are read for the
/// budget, which counts what the build held back, and nothing of theirs is written: no ledger, no
/// access time.
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
    // A layout mid-write reads as an error here, and then the pass only does less; the sweep
    // under the lock is where a layout it does not know stops the pass.
    let Ok((units, _)) = units(dir) else {
        return Swept { freed: busy.swept, items, busy: Some(busy) };
    };
    let settled = now.checked_sub(GUARD).unwrap_or(UNIX_EPOCH);
    for unit in units {
        // Every unit is looked at, not only those compiled since the last pass: a busy pass
        // writes no ledger, so the last pass may have left the objects of any of them.
        for object in unread_objects(&unit.objects, settled) {
            let bytes = size(&object);
            // One a concurrent pass took first is gone all the same.
            if opts.dry_run || remove(&object).is_ok() {
                busy.swept.leftovers = busy.swept.leftovers.saturating_add(1);
                busy.swept.bytes = busy.swept.bytes.saturating_add(bytes);
            }
        }
        // Without a ledger no unit's idleness is known, so none is one the budget could have
        // taken.
        if ledger.since.is_some() {
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

/// The object files of a unit that nothing reads: its earlier compiles' ([`superseded_objects`]),
/// and where its executable or rlib is beside them, those that artifact does not read
/// ([`unlinked_objects`]), with `settled` as that takes it.
fn unread_objects(objects: &[Utf8PathBuf], settled: SystemTime) -> Vec<Utf8PathBuf> {
    let mut by_stem: HashMap<(&Utf8Path, &str), Vec<Utf8PathBuf>> = HashMap::new();
    for path in objects {
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else { continue };
        let stem = name.split('.').next().unwrap_or(name);
        by_stem.entry((dir, stem)).or_default().push(path.clone());
    }
    let mut unread = Vec::new();
    for ((dir, stem), objects) in by_stem {
        // In a directory a build holds, a compile or a link may still be writing an object
        // that looks superseded: only one written before the build's guard goes. By its write
        // time, since an earlier compile's objects are what is asked about, not hard links.
        let superseded: Vec<_> =
            superseded_objects(&objects).into_iter().filter(|o| modified(o) <= settled).collect();
        let unlinked = unlinked_objects(dir, stem, &objects, settled).unwrap_or_default();
        unread.extend(unlinked.into_iter().filter(|o| !superseded.contains(o)));
        unread.extend(superseded);
    }
    unread
}

/// Whether `object`'s inode is unchanged since `written`: a compile that writes an object, or
/// links a reused one under its own invocation's name, changes it.
fn unchanged_since(object: &Utf8Path, written: SystemTime) -> bool {
    std::fs::symlink_metadata(object).is_ok_and(|m| {
        let secs = u64::try_from(m.ctime()).unwrap_or(0);
        let nanos = u32::try_from(m.ctime_nsec()).unwrap_or(0);
        UNIX_EPOCH.checked_add(Duration::new(secs, nanos)).is_some_and(|changed| changed <= written)
    })
}

/// The objects of `stem` in `dir` that its artifact beside them does not read, or `None` when
/// there is no artifact to ask or its debug map cannot be read.
///
/// The objects are kept for debug info alone (unpacked split debuginfo). An executable's debug
/// map names each object it was linked from (`N_OSO`), and that is what `dsymutil`, lldb and a
/// backtrace read. Each compile names its objects after its own invocation, and those it reused
/// from the incremental cache are hard links of the earlier ones, so every invocation of a
/// unit can share one modification time and only the debug map tells them apart. A library's
/// objects are read by nobody once its rlib is written: an executable's debug map names the
/// rlib's members (`libfoo-<hash>.rlib(foo-<hash>.<cgu>.rcgu.o)`), never the loose files.
///
/// In a directory a build holds, an object whose inode changed after both the artifact was
/// written and `settled` stays: a compile may be writing it, its artifact not written yet. The
/// artifact's time alone clears few objects, since rustc links each into its finished
/// incremental session after the link, so `settled` is the hour the budget also spares. A pass
/// under cargo's locks, where no compile runs, sets it to now.
fn unlinked_objects(
    dir: &Utf8Path,
    stem: &str,
    objects: &[Utf8PathBuf],
    settled: SystemTime,
) -> Option<Vec<Utf8PathBuf>> {
    let executable = dir.join(stem);
    let rlib = dir.join(format!("lib{stem}.rlib"));
    let (written, read) = if executable.is_file() {
        let written = std::fs::metadata(&executable).and_then(|m| m.modified()).ok()?;
        (written, linked_objects(&executable).ok()?)
    } else if rlib.is_file() {
        (std::fs::metadata(&rlib).and_then(|m| m.modified()).ok()?, HashSet::new())
    } else {
        return None;
    };
    // By file name: the debug map holds the path the build was given, which may not be spelled
    // as this pass spells it, and an invocation's names are unique in their directory.
    let unread = objects.iter().filter(|o| {
        o.file_name().is_some_and(|name| !read.contains(name))
            && unchanged_since(o, written.max(settled))
    });
    Some(unread.cloned().collect())
}

/// The file names of the objects an arm64 Mach-O executable's debug map names (`N_OSO` stabs),
/// read from its symbol table without reading the rest of the file. The layout is
/// `<mach-o/loader.h>`'s: a `mach_header_64` of 32 bytes (`ncmds` at 16, `sizeofcmds` at 20),
/// then the load commands, each `cmd` and `cmdsize` first; `LC_SYMTAB` (2) holds `symoff`,
/// `nsyms`, `stroff` and `strsize`. Each `nlist_64` (`<mach-o/nlist.h>`) is 16 bytes, `n_strx`
/// at 0 and `n_type` at 4; `N_OSO` is 0x66 (`<mach-o/stab.h>`).
fn linked_objects(executable: &Utf8Path) -> Result<HashSet<String>> {
    use std::os::unix::fs::FileExt as _;
    const MH_MAGIC_64: u32 = 0xfeed_facf;
    const LC_SYMTAB: u32 = 2;
    const N_OSO: u8 = 0x66;
    let file = File::open(executable).with_context(|| format!("open {executable}"))?;
    let read = |offset: u64, len: usize| -> Result<Vec<u8>> {
        let mut buf = vec![0; len];
        file.read_exact_at(&mut buf, offset).with_context(|| format!("read {executable}"))?;
        Ok(buf)
    };
    let u32_at = |buf: &[u8], at: usize| -> Option<u32> {
        buf.get(at..at.checked_add(4)?)?.try_into().ok().map(u32::from_le_bytes)
    };
    let header = read(0, 32)?;
    let (Some(MH_MAGIC_64), Some(ncmds), Some(sizeofcmds)) =
        (u32_at(&header, 0), u32_at(&header, 16), u32_at(&header, 20))
    else {
        bail!("{executable} is not a 64-bit Mach-O");
    };
    let commands = read(32, usize::try_from(sizeofcmds)?)?;
    let mut at = 0_usize;
    let mut symtab = None;
    for _ in 0..ncmds {
        let field = |n: usize| u32_at(&commands, at.saturating_add(n));
        let (Some(cmd), Some(size)) = (field(0), field(4)) else {
            bail!("{executable}: a load command runs past the header");
        };
        if cmd == LC_SYMTAB {
            symtab = Some((field(8), field(12), field(16), field(20)));
            break;
        }
        at = at.checked_add(usize::try_from(size)?).context("load commands overflow")?;
    }
    let Some((Some(symoff), Some(nsyms), Some(stroff), Some(strsize))) = symtab else {
        bail!("{executable} has no symbol table");
    };
    let table = read(u64::from(symoff), usize::try_from(nsyms)?.saturating_mul(16))?;
    let mut names = HashSet::new();
    let (entries, _) = table.as_chunks::<16>();
    for entry in entries {
        if entry[4] != N_OSO {
            continue;
        }
        let Some(strx) = u32_at(entry, 0).filter(|x| *x < strsize) else { continue };
        let len = usize::try_from(strsize.saturating_sub(strx).min(4096))?;
        let raw = read(u64::from(stroff).saturating_add(u64::from(strx)), len)?;
        let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
        let path = std::str::from_utf8(raw.get(..end).unwrap_or_default()).map(Utf8Path::new);
        if let Some(name) = path.ok().and_then(Utf8Path::file_name) {
            names.insert(name.to_owned());
        }
    }
    Ok(names)
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
        // Exactly `<stem>.<cgu>.<invocation>.rcgu.o`. A thin LTO link writes its modules as
        // `<binary>.<dependency>.<dependency>.<hash>-cgu.<n>.rcgu.o.rcgu.o`, where the third part
        // names a dependency, not an invocation: read as one, every dependency's modules but
        // the newest were "superseded" and went while the link still needed them.
        let parts: Vec<&str> = name.split('.').collect();
        let [stem, _cgu, invocation, "rcgu", "o"] = parts.as_slice() else { continue };
        let (stem, invocation) = (*stem, *invocation);
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

/// A tree's bytes on disk ([`own_bytes`]): those of files with one name, and the inodes of files
/// with several, so a total counts a hard-linked file once.
#[derive(Default)]
struct Tally {
    bytes: u64,
    single: u64,
    linked: Vec<(u64, u64, u64)>,
}

fn tally(path: &Utf8Path) -> Tally {
    fn walk(path: &std::path::Path, out: &mut Tally) {
        let Ok(meta) = std::fs::symlink_metadata(path) else { return };
        let bytes = own_bytes(path, &meta);
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

/// The bytes the file at `path` (`meta` its metadata) holds on disk that no other file shares:
/// on APFS the private size of a file that may share blocks (`ATTR_CMNEXT_PRIVATESIZE`), so a
/// lane directory cloned from another (`local::seed_dir`) counts only the blocks either side
/// wrote since, and a unit both share counts as what deleting it frees, nothing; elsewhere, and
/// for anything else, its blocks. Counted at full size, each clone added 16-20 GB to `target/`
/// (from 82 to 96 GB when one appeared, 2026-10-31), and five took it over its budget for space
/// no pass could free.
fn own_bytes(path: &std::path::Path, meta: &std::fs::Metadata) -> u64 {
    let blocks = meta.blocks().saturating_mul(512);
    if meta.is_file() { private_size(path).unwrap_or(blocks) } else { blocks }
}

/// The private size of the file at `path`, its bytes no clone shares (`getattrlist(2)`), when it
/// may share blocks with another (`EF_MAY_SHARE_BLOCKS`); `None` for any other, or where the
/// volume does not say. A file never cloned is left to its blocks: APFS counts a write in its
/// private size only once the write reaches the disk.
#[cfg(target_os = "macos")]
fn private_size(path: &std::path::Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt as _;
    /// `<sys/stat.h>`'s: the file may share blocks with another.
    const EF_MAY_SHARE_BLOCKS: u64 = 0x1;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let extended = libc::ATTR_CMNEXT_PRIVATESIZE | libc::ATTR_CMNEXT_EXT_FLAGS;
    let mut asked = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: extended,
    };
    // The answer's length (a `u32`), the attributes it holds (an `attribute_set_t`, five `u32`s,
    // the extended common ones last), then the private size (an `off_t`) and the extended flags
    // (a `u64`), in the order of their bits.
    let mut answer = [0_u8; 40];
    // SAFETY: `getattrlist(2)` reads the NUL-terminated path and the attribute list and writes at
    // most `answer.len()` bytes into `answer`, all of which outlive the call.
    let status = unsafe {
        libc::getattrlist(
            path.as_ptr(),
            (&raw mut asked).cast(),
            answer.as_mut_ptr().cast(),
            answer.len(),
            libc::FSOPT_NOFOLLOW | libc::FSOPT_ATTR_CMN_EXTENDED,
        )
    };
    let returned = answer.get(20..24)?.try_into().ok().map(u32::from_ne_bytes)?;
    let flags = answer.get(32..40)?.try_into().ok().map(u64::from_ne_bytes)?;
    if status != 0 || returned & extended != extended || flags & EF_MAY_SHARE_BLOCKS == 0 {
        return None;
    }
    u64::try_from(i64::from_ne_bytes(answer.get(24..32)?.try_into().ok()?)).ok()
}

/// Off a Mac no volume says: a file's blocks stand for its size.
#[cfg(not(target_os = "macos"))]
const fn private_size(_path: &std::path::Path) -> Option<u64> {
    None
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
