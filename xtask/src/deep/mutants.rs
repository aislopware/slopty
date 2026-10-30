//! `cargo xtask deep mutants`: which changes to the code no test notices (`cargo mutants`).
//!
//! Two shapes. A whole crate (`-p`, or `--rotate` for this week's tier A crate, one `--shard` of it
//! at a time), which the weekly run takes; and the code a diff touched (`--in-diff <base>`, or
//! `--since <time>` for the day's landings), by tier (`docs/TESTING.md`, "Mutation in the
//! diff"):
//! - Tier A, the pure kernels: every mutant the diff makes must be caught, so a missed one fails
//!   the run. CI runs it on each change that touches them, beside the gate.
//! - Tier B, the stateful daemons and their links: its missed mutants are work, listed, until the
//!   tier has its baseline; they fail nothing yet.
//!
//! Every run writes what it missed to `target/deep/mutants/lane-work.md`, and to the job's
//! summary on a runner: one line a lane can take, write the test that fails on the change, and
//! tick. The run itself fails only on Tier A's missed mutants and on a run that went wrong (a
//! baseline that fails, a usage or internal error): a red is never "a mutant lived elsewhere".

use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::{Context as _, Result, bail, ensure};
use camino::Utf8Path;
use clap::ValueEnum;
use xshell::{Shell, cmd};

use crate::tools::step;

/// What to mutate, and how.
#[derive(clap::Args, Debug)]
pub struct Opts {
    /// A crate to mutate whole; repeat for several. With a diff, only these of its tiers' crates.
    #[arg(short, long)]
    pub package: Vec<String>,
    /// Mutate this week's tier A crate whole: each whole crate of the tier in turn, a week each,
    /// so what no diff touched lately is read too.
    #[arg(long, conflicts_with_all = ["package", "in_diff", "since"])]
    pub rotate: bool,
    /// Mutate only the code changed since the merge base with this revision (`origin/main`).
    #[arg(long, conflicts_with = "since")]
    pub in_diff: Option<String>,
    /// Mutate only the code `HEAD` changed since this time (`git log --since`'s words, such as
    /// "24 hours ago"): the day's landings.
    #[arg(long)]
    pub since: Option<String>,
    /// The tiers a diff's mutants are kept for; repeat for several (default: a).
    #[arg(long, value_enum)]
    pub tier: Vec<Tier>,
    /// Seconds a mutant's tests may run before it counts as a timeout (caught, and listed).
    #[arg(long, default_value_t = 300)]
    pub timeout: u64,
    /// Only this share of the mutants, `k/n` counting from 0 (`cargo mutants --shard`).
    #[arg(long)]
    pub shard: Option<String>,
    /// Mutants built and tested at once, each in its own copy of the tree.
    #[arg(long, default_value_t = 1)]
    pub jobs: u32,
    /// At most this many mutants: a diff that makes more is cut to an even sample of them
    /// (`--shard 0/k`, round robin), and the night's run, which has no cap, takes the rest.
    #[arg(long, conflicts_with = "shard")]
    pub cap: Option<usize>,
    /// Test the unmutated tree first (`run`), or trust that its tests pass (`skip`): CI's
    /// per-change run skips it, since the gate tests the same crates at the same commit.
    #[arg(long, value_enum, default_value_t = Baseline::Run)]
    pub baseline: Baseline,
}

/// Whether `cargo mutants` tests the unmutated tree first.
#[derive(Clone, Copy, ValueEnum, Debug, PartialEq, Eq)]
pub enum Baseline {
    /// Its tests must pass before any mutant is tried.
    Run,
    /// Taken as passing, as the gate showed.
    Skip,
}

impl Baseline {
    const fn flag(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Skip => "skip",
        }
    }
}

/// A crate tier of research §4.1 (`.research/testing-rust-modern-2026-10-29.md`).
#[derive(Clone, Copy, ValueEnum, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// The pure kernels: every mutant of a diff caught.
    A,
    /// The stateful daemons and their links: missed mutants listed as work.
    B,
}

impl Tier {
    const fn crates(self) -> &'static [(&'static str, &'static [&'static str])] {
        match self {
            Self::A => TIER_A,
            Self::B => TIER_B,
        }
    }

    /// Whether a missed mutant of this tier fails the run.
    const fn strict(self) -> bool {
        matches!(self, Self::A)
    }

    const fn name(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
        }
    }
}

/// Tier A, by package and the paths of its code in the tier: whole crates, and the pure modules
/// of the crates that are otherwise stateful.
const TIER_A: &[(&str, &[&str])] = &[
    ("slopty-core", &["crates/slopty-core/src"]),
    ("slopty-grid", &["crates/slopty-grid/src"]),
    ("slopty-predict", &["crates/slopty-predict/src"]),
    ("slopty-proto", &["crates/slopty-proto/src"]),
    ("slopty-media", &["crates/slopty-media/src"]),
    ("slopty-math", &["crates/slopty-math/src"]),
    ("slopty-settings", &["crates/slopty-settings/src"]),
    ("slopty-theme", &["crates/slopty-theme/src"]),
    ("slopty-markdown", &["crates/slopty-markdown/src"]),
    ("slopty-syntax", &["crates/slopty-syntax/src"]),
    ("slopty-view-doc", &["crates/slopty-view-doc/src"]),
    ("slopty-view-proto", &["crates/slopty-view-proto/src"]),
    ("slopty-tools", &["crates/slopty-tools/src"]),
    (
        "slopty-net",
        &[
            "crates/slopty-net/src/echo.rs",
            "crates/slopty-net/src/framed.rs",
            "crates/slopty-net/src/admission.rs",
        ],
    ),
    ("slopty-client", &["crates/slopty-client/src/term.rs", "crates/slopty-client/src/items.rs"]),
    ("slopty-pty", &["crates/slopty-pty/src/ring.rs", "crates/slopty-pty/src/protocol.rs"]),
    (
        "slopty-agent",
        &["crates/slopty-agent/src/conversation.rs", "crates/slopty-agent/src/conversation"],
    ),
];

/// Tier B. The File Provider (`apps/slopty-files`) is Apple's alone and does not build where the
/// mutants run, so it waits for a Mac run.
const TIER_B: &[(&str, &[&str])] = &[
    ("slopty-worker", &["crates/slopty-worker/src"]),
    ("slopty-workerd", &["apps/slopty-worker/src"]),
    ("slopty-server", &["crates/slopty-server/src"]),
    ("slopty-serverd", &["apps/slopty-server/src"]),
    ("slopty-client", &["crates/slopty-client/src/link.rs", "crates/slopty-client/src/link"]),
    ("slopty-cli", &["apps/slopty-cli/src"]),
    ("slopty-ptyd", &["apps/slopty-ptyd/src"]),
    ("slopty-deploy", &["crates/slopty-deploy/src"]),
    ("slopty-tailnet", &["crates/slopty-tailnet/src"]),
];

/// Where every report goes, a directory per run under it.
const OUT: &str = "target/deep/mutants";

/// The packages a run mutates, for the `mutants` profile's setup script ([`spawned`]), which
/// builds the binaries their tests spawn in each mutant's tree.
const PACKAGES: &str = "SLOPTY_MUTANTS_PACKAGES";

pub fn run(sh: &Shell, opts: &Opts) -> Result<()> {
    let root = crate::tools::repo_root()?;
    let base = match (&opts.in_diff, &opts.since) {
        (Some(base), _) => Some(merge_base(sh, base)?),
        (None, Some(since)) => Some(before(sh, since)?),
        (None, None) => None,
    };
    let Some(base) = base else {
        let packages = if opts.rotate {
            let week = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .context("the clock")?
                .as_secs()
                / WEEK;
            let package = rotation(week).to_owned();
            println!("mutants: week {week}'s crate is {package}");
            vec![package]
        } else {
            opts.package.clone()
        };
        ensure!(
            !packages.is_empty(),
            "name a crate (-p, --rotate), or a diff (--in-diff, --since)"
        );
        let mut runs = Vec::new();
        for package in &packages {
            let out = mutants_dir(package, opts.shard.as_deref());
            let shard = opts.shard.as_deref().map(|s| (s.to_owned(), Sharding::Slice));
            let scope = Scope {
                dir: &root,
                packages: std::slice::from_ref(package),
                diff: None,
                out: &out,
            };
            let ran = mutate(sh, &scope, opts, shard)?;
            runs.push(Run { tier: tier_of(package), strict: false, out, ran });
        }
        return report(&root, &runs);
    };
    let tiers: Vec<Tier> = if opts.tier.is_empty() { vec![Tier::A] } else { opts.tier.clone() };
    let mut runs = Vec::new();
    for tier in tiers {
        let out = format!("{OUT}/diff-{}", tier.name());
        let paths: Vec<&str> = tier
            .crates()
            .iter()
            .filter(|(package, _)| {
                opts.package.is_empty() || opts.package.iter().any(|p| p == package)
            })
            .flat_map(|(_, paths)| paths.iter().copied())
            .collect();
        let diff = cmd!(sh, "git diff --no-ext-diff --merge-base {base} -- {paths...}")
            .quiet()
            .read()
            .context("git diff")?;
        let touched = touched(tier, &diff);
        if touched.is_empty() {
            println!("mutants: tier {} changed nothing since {base}", tier.name());
            continue;
        }
        std::fs::create_dir_all(root.join(&out))?;
        let file = root.join(&out).join("git.diff");
        std::fs::write(&file, format!("{diff}\n")).with_context(|| format!("writing {file}"))?;
        let shard = match opts.cap {
            Some(cap) => sample(sh, &root, &touched, &file, cap)?,
            None => opts.shard.as_deref().map(|s| (s.to_owned(), Sharding::Slice)),
        };
        let scope = Scope { dir: &root, packages: &touched, diff: Some(&file), out: &out };
        let ran = mutate(sh, &scope, opts, shard)?;
        runs.push(Run { tier: Some(tier), strict: tier.strict(), out, ran });
    }
    report(&root, &runs)
}

/// How `--shard` splits the mutants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sharding {
    /// Contiguous runs, so a shard's mutants share their builds: a crate split over runners.
    Slice,
    /// Every k-th, an even sample across the diff.
    RoundRobin,
}

/// One `cargo mutants` run: its tier, whether a missed mutant fails it, its report directory and
/// how it ended.
struct Run {
    tier: Option<Tier>,
    strict: bool,
    out: String,
    ran: Ended,
}

/// How `cargo mutants` ended (its exit code, `cargo mutants --help`'s list).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// Every mutant caught or unviable (0).
    Caught,
    /// Some mutant lived (2).
    Missed,
    /// Some mutant's tests outran the timeout, none lived (3).
    TimedOut,
    /// The unmutated tree's tests failed (4): every mutant would have counted as caught.
    BaselineFailed,
    /// A usage or internal error, or a signal.
    Broken(Option<i32>),
}

impl Ended {
    /// What `cargo mutants`' exit code says.
    #[must_use]
    pub const fn from_code(code: Option<i32>) -> Self {
        match code {
            Some(0) => Self::Caught,
            Some(2) => Self::Missed,
            Some(3) => Self::TimedOut,
            Some(4) => Self::BaselineFailed,
            other => Self::Broken(other),
        }
    }
}

/// What one `cargo mutants` run mutates: `packages` of the tree at `dir`, only the code `diff`
/// touches when given; its report goes under `out` (relative to `dir`, or absolute).
pub struct Scope<'a> {
    pub dir: &'a Utf8Path,
    pub packages: &'a [String],
    pub diff: Option<&'a Utf8Path>,
    pub out: &'a str,
}

/// `cargo mutants` over `scope`, one `shard` of it when given; how it ended. Every other option
/// is `opts`'s.
pub fn mutate(
    sh: &Shell,
    scope: &Scope<'_>,
    opts: &Opts,
    shard: Option<(String, Sharding)>,
) -> Result<Ended> {
    let Scope { dir, packages, diff, out } = *scope;
    let _dir = sh.push_dir(dir);
    let _env = sh.push_env("AWS_LC_SYS_CMAKE_BUILDER", "1");
    // `.cargo/config.toml` names libghostty-vt's prebuilt directory relative to the tree, and
    // cargo-mutants copies the tree without `target/`, so each copy ran its own minute of zig
    // build; the tree's is the one every copy shares, as the gate's snapshots share the
    // checkout's (`docs/decisions/tooling.md`, "libghostty-vt is built once per set of inputs").
    // A caller's (a lane's check) stands.
    let _prebuilt = std::env::var_os(PREBUILT_DIR)
        .is_none()
        .then(|| sh.push_env(PREBUILT_DIR, dir.join("target").join(crate::prune::PREBUILT)));
    // The `mutants` profile's setup script builds the binaries these packages' tests spawn in
    // each mutant's tree (`spawned`), through this xtask: the gate's own script would build the
    // whole workspace, which does not build off a Mac. The gate's returns at once.
    let _packages = sh.push_env(PACKAGES, packages.join(","));
    let _fresh = sh.push_env(crate::gate::BINS_FRESH, "1");
    let _xtask = sh.push_env("SLOPTY_XTASK", std::env::current_exe().context("the xtask binary")?);
    // cargo-mutants makes the report's own directory but not the one above it, and ended every
    // shard of the weekly run at once without it (run 38055397887).
    std::fs::create_dir_all(dir.join(out)).with_context(|| format!("create {out}"))?;
    let packages: Vec<String> =
        packages.iter().flat_map(|p| ["-p".to_owned(), p.clone()]).collect();
    let diff: Vec<String> =
        diff.map(|d| vec!["--in-diff".to_owned(), d.to_string()]).unwrap_or_default();
    let shard: Vec<String> = shard
        .map(|(s, how)| {
            let how = match how {
                Sharding::Slice => "slice",
                Sharding::RoundRobin => "round-robin",
            };
            vec!["--shard".to_owned(), s, "--sharding".to_owned(), how.to_owned()]
        })
        .unwrap_or_default();
    let annotations: &[&str] =
        if std::env::var_os("GITHUB_ACTIONS").is_some() { &["--annotations=github"] } else { &[] };
    let (timeout, jobs) = (opts.timeout.to_string(), opts.jobs.max(1).to_string());
    let baseline = opts.baseline.flag();
    let title = format!("cargo mutants: {}", out.rsplit('/').next().unwrap_or(out));
    println!("▶ {title}");
    let started = std::time::Instant::now();
    let status = cmd!(
        sh,
        "cargo mutants {packages...} {diff...} --baseline {baseline} --timeout {timeout} --jobs {jobs} {shard...} --no-shuffle {annotations...} --output {out}"
    )
    .ignore_status()
    .output()
    .map(|o| {
        print!("{}", String::from_utf8_lossy(&o.stdout));
        eprint!("{}", String::from_utf8_lossy(&o.stderr));
        o.status.code()
    })
    .context("starting cargo mutants")?;
    let ended = Ended::from_code(status);
    println!("  {title}: {ended:?} ({:.0?})", started.elapsed());
    Ok(ended)
}

/// The packages of `tier` whose code in the tier `diff` touches.
fn touched(tier: Tier, diff: &str) -> Vec<String> {
    let files: Vec<&str> = diff.lines().filter_map(|l| l.strip_prefix("+++ b/")).collect();
    let mut names: Vec<String> = tier
        .crates()
        .iter()
        .filter(|(_, paths)| files.iter().any(|f| paths.iter().any(|p| within(f, p))))
        .map(|(package, _)| (*package).to_owned())
        .collect();
    names.dedup();
    names
}

/// Whether `file` is `path` or under it.
fn within(file: &str, path: &str) -> bool {
    file == path || file.strip_prefix(path).is_some_and(|rest| rest.starts_with('/'))
}

/// The share to run when the diff makes more than `cap` mutants: `0/k`, every k-th, so the run
/// samples the whole diff evenly. `None` when they all fit.
fn sample(
    sh: &Shell,
    dir: &Utf8Path,
    packages: &[String],
    diff: &Utf8Path,
    cap: usize,
) -> Result<Option<(String, Sharding)>> {
    let _dir = sh.push_dir(dir);
    let packages: Vec<String> =
        packages.iter().flat_map(|p| ["-p".to_owned(), p.clone()]).collect();
    let listed = cmd!(sh, "cargo mutants --list {packages...} --in-diff {diff}")
        .quiet()
        .read()
        .context("cargo mutants --list")?;
    let count = listed.lines().filter(|l| !l.trim().is_empty()).count();
    let shards = shards_for(count, cap);
    if shards > 1 {
        println!(
            "mutants: {count} in the diff, past the cap of {cap}: every {shards}th now, all of them in the night's run"
        );
    }
    Ok((shards > 1).then(|| (format!("0/{shards}"), Sharding::RoundRobin)))
}

/// How many shards keep each under `cap` mutants of `count`.
const fn shards_for(count: usize, cap: usize) -> usize {
    if cap == 0 { 1 } else { count.div_ceil(cap) }
}

/// Seconds in a week, the rotation's step.
const WEEK: u64 = 7 * 24 * 60 * 60;

/// The tier A crate mutated whole in `week` (weeks since the epoch): those whose every path is
/// their whole `src`, in turn.
fn rotation(week: u64) -> &'static str {
    let whole: Vec<&str> = TIER_A
        .iter()
        .filter(|(_, paths)| paths.iter().all(|p| p.ends_with("/src")))
        .map(|(package, _)| *package)
        .collect();
    let at = usize::try_from(week).unwrap_or(0).checked_rem(whole.len()).unwrap_or(0);
    whole.get(at).copied().unwrap_or("slopty-core")
}

/// The last commit on `HEAD`'s first-parent line before `since`: the day-diff's base.
fn before(sh: &Shell, since: &str) -> Result<String> {
    let base = cmd!(sh, "git rev-list -1 --first-parent --before={since} HEAD")
        .quiet()
        .read()
        .context("git rev-list --before")?;
    ensure!(!base.is_empty(), "no commit on HEAD before {since:?}: fetch more history");
    Ok(base)
}

/// `HEAD`'s merge base with `base`.
fn merge_base(sh: &Shell, base: &str) -> Result<String> {
    cmd!(sh, "git merge-base {base} HEAD")
        .quiet()
        .read()
        .with_context(|| format!("no merge base of {base} and HEAD: fetch {base} with its history"))
}

/// The tier `package` is in as a whole crate, if it is.
fn tier_of(package: &str) -> Option<Tier> {
    [Tier::A, Tier::B].into_iter().find(|tier| {
        tier.crates()
            .iter()
            .any(|(p, paths)| *p == package && paths.iter().all(|path| path.ends_with("/src")))
    })
}

/// Where a whole-crate run writes for `package` and `shard`.
fn mutants_dir(package: &str, shard: Option<&str>) -> String {
    let name =
        shard.map_or_else(|| package.to_owned(), |s| format!("{package}-{}", s.replace('/', "of")));
    format!("{OUT}/{name}")
}

/// What a run's report lists, by outcome: each mutant as `file:line:col: what`.
#[derive(Debug, Default)]
pub struct Outcomes {
    pub caught: Vec<String>,
    pub missed: Vec<String>,
    pub timeout: Vec<String>,
    pub unviable: Vec<String>,
}

impl Outcomes {
    /// The lists under `<out>/mutants.out/`; a list cargo-mutants did not write is empty.
    pub fn read(out: &Utf8Path) -> Self {
        let list = |name: &str| -> Vec<String> {
            std::fs::read_to_string(out.join("mutants.out").join(name))
                .unwrap_or_default()
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(str::to_owned)
                .collect()
        };
        Self {
            caught: list("caught.txt"),
            missed: list("missed.txt"),
            timeout: list("timeout.txt"),
            unviable: list("unviable.txt"),
        }
    }
}

/// The package a mutant's line (`crates/slopty-core/src/x.rs:1:2: …`) is in, by its path.
fn package_of(line: &str) -> String {
    let path = line.split(':').next().unwrap_or(line);
    [Tier::A, Tier::B]
        .into_iter()
        .flat_map(|tier| tier.crates().iter())
        .find(|(_, paths)| paths.iter().any(|p| within(path, p)))
        .map_or_else(
            || {
                path.split("/src/")
                    .next()
                    .unwrap_or(path)
                    .rsplit('/')
                    .next()
                    .unwrap_or(path)
                    .to_owned()
            },
            |(package, _)| (*package).to_owned(),
        )
}

/// Print each run's score, write the missed mutants as lane work, and fail on a strict tier's
/// missed mutant or a run that went wrong.
fn report(root: &Utf8Path, runs: &[Run]) -> Result<()> {
    let mut work = String::from(
        "# Missed mutants: lane work\n\nEach line is a change to the code that no test noticed. Write \
         the test that fails on it, or, when the change cannot be told apart (an equivalent mutant), \
         add it to `.cargo/mutants.toml` or mark the function `#[mutants::skip]` with the reason.\n",
    );
    let mut failed = Vec::new();
    let mut any = false;
    for run in runs {
        let outcomes = Outcomes::read(&root.join(&run.out));
        let tier = run.tier.map_or("untiered", Tier::name);
        let mut by_package: BTreeMap<String, (usize, Vec<&str>)> = BTreeMap::new();
        for line in outcomes.caught.iter().chain(&outcomes.timeout) {
            let caught = &mut by_package.entry(package_of(line)).or_default().0;
            *caught = caught.saturating_add(1);
        }
        for line in &outcomes.missed {
            by_package.entry(package_of(line)).or_default().1.push(line);
        }
        for (package, (caught, missed)) in &by_package {
            let score = mutation_score(*caught, missed.len());
            println!(
                "mutants {package} (tier {tier}): {caught} caught, {} missed, MS {score}",
                missed.len()
            );
            if missed.is_empty() {
                continue;
            }
            any = true;
            let _heading = write!(
                work,
                "\n## {package}, tier {tier}{}: {} missed of {}, MS {score}\n\n",
                if run.strict { ", every one must be caught" } else { "" },
                missed.len(),
                caught.saturating_add(missed.len()),
            );
            for line in missed {
                let _line = writeln!(work, "- [ ] {line}");
            }
        }
        println!(
            "  {}: {} timed out (counted caught), {} unviable (not counted)",
            run.out,
            outcomes.timeout.len(),
            outcomes.unviable.len()
        );
        match run.ran {
            Ended::Caught | Ended::TimedOut => {}
            Ended::Missed if !run.strict => {}
            Ended::Missed => failed.push(format!(
                "{} mutant(s) of tier {tier} lived ({}/mutants.out/missed.txt)",
                outcomes.missed.len(),
                run.out
            )),
            Ended::BaselineFailed => failed.push(format!(
                "the unmutated tree's tests failed under cargo mutants ({}/mutants.out/log)",
                run.out
            )),
            Ended::Broken(code) => {
                failed.push(format!("cargo mutants failed ({code:?}): {}", run.out));
            }
        }
    }
    if any {
        let path = root.join(OUT).join("lane-work.md");
        std::fs::create_dir_all(root.join(OUT))?;
        std::fs::write(&path, &work).with_context(|| format!("writing {path}"))?;
        println!("mutants: the missed ones are lane work in {path}");
        if let Some(summary) = std::env::var_os("GITHUB_STEP_SUMMARY") {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new().append(true).create(true).open(summary)?;
            file.write_all(work.as_bytes())?;
        }
    }
    if !failed.is_empty() {
        bail!("mutants:\n  {}", failed.join("\n  "));
    }
    Ok(())
}

/// The mutation score, caught over caught and missed, as a percentage to one place; a timeout
/// counts as caught and an unviable mutant not at all.
fn mutation_score(caught: usize, missed: usize) -> String {
    let all = caught.saturating_add(missed);
    if all == 0 {
        return "n/a".to_owned();
    }
    let tenths = caught.saturating_mul(1000).saturating_add(all / 2).checked_div(all).unwrap_or(0);
    format!("{}.{} %", tenths / 10, tenths % 10)
}

/// The `mutants` nextest profile's setup script (`cargo xtask deep mutant-bins`): in a mutant's
/// tree, build the binaries the tests of [`PACKAGES`] spawn, from that tree, so a daemon a test
/// starts carries the mutant too. The run sets [`crate::gate::BINS_FRESH`], so the test kit takes
/// them from beside the test.
///
/// The tree is the one nextest names ([`TESTED_TREE`]), not xtask's own: xtask starts in the
/// checkout it was built from, where the binaries would be built unmutated and beside no test of
/// the mutant's, and each test that spawns one would fail and count its mutant caught.
pub fn spawned(sh: &Shell) -> Result<()> {
    let packages = std::env::var(PACKAGES).unwrap_or_default();
    let names: Vec<&str> = packages.split(',').filter(|p| !p.is_empty()).collect();
    let Some(args) = crate::gate::spawned_bins_for(&names) else { return Ok(()) };
    let tree = std::env::var(TESTED_TREE)
        .with_context(|| format!("{TESTED_TREE}: run by nextest as a setup script"))?;
    let _tree = sh.push_dir(tree);
    step("the binaries the mutated tests spawn", &cmd!(sh, "cargo build --profile test {args...}"))
}

/// Where nextest runs a setup script, the workspace it tests, which it also names in this.
const TESTED_TREE: &str = "NEXTEST_WORKSPACE_ROOT";

/// Where libghostty-vt-sys takes a prebuilt library from and publishes the one it builds.
const PREBUILT_DIR: &str = "LIBGHOSTTY_VT_SYS_PREBUILT_DIR";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shard_of_mutants_writes_its_own_report() {
        assert_eq!(mutants_dir("slopty-proto", None), "target/deep/mutants/slopty-proto");
        assert_eq!(
            mutants_dir("slopty-proto", Some("2/4")),
            "target/deep/mutants/slopty-proto-2of4"
        );
    }

    #[test]
    fn the_rotation_takes_each_whole_tier_a_crate_once_a_round() {
        let whole = TIER_A.iter().filter(|(_, paths)| paths.iter().all(|p| p.ends_with("/src")));
        let round = whole.clone().count();
        let weeks: std::collections::BTreeSet<&str> =
            (2900..2900 + round as u64).map(rotation).collect();
        assert_eq!(weeks.len(), round, "a round of weeks repeats a crate: {weeks:?}");
        assert!(whole.clone().all(|(package, _)| weeks.contains(package)));
        assert!(!weeks.contains("slopty-net"), "a crate only partly in the tier is never whole");
        assert_eq!(rotation(2900), rotation(2900 + round as u64));
    }

    #[test]
    fn every_tier_path_is_in_the_package_it_names() {
        let root = crate::tools::repo_root().unwrap();
        for (package, paths) in TIER_A.iter().chain(TIER_B) {
            for path in *paths {
                let full = root.join(path);
                let file = if full.is_dir() { full.clone() } else { full.with_extension("rs") };
                assert!(full.exists() || file.exists(), "{path} ({package}) is gone");
                let manifest =
                    path.split("/src").next().map(|dir| root.join(dir).join("Cargo.toml"));
                let text = std::fs::read_to_string(manifest.unwrap()).unwrap();
                assert!(
                    text.contains(&format!("name = \"{package}\"")),
                    "{path} is not {package}'s"
                );
            }
        }
    }

    #[test]
    fn a_diff_touches_the_tier_packages_whose_paths_it_changes() {
        let diff = "diff --git a/crates/slopty-net/src/echo.rs b/crates/slopty-net/src/echo.rs\n\
                    --- a/crates/slopty-net/src/echo.rs\n+++ b/crates/slopty-net/src/echo.rs\n\
                    diff --git a/crates/slopty-net/src/quic.rs b/crates/slopty-net/src/quic.rs\n\
                    +++ b/crates/slopty-net/src/quic.rs\n\
                    +++ b/crates/slopty-core/src/lib.rs\n+++ b/crates/slopty-corex/src/lib.rs\n";
        assert_eq!(touched(Tier::A, diff), ["slopty-core", "slopty-net"]);
        assert_eq!(touched(Tier::B, diff), Vec::<String>::new());
        assert_eq!(package_of("crates/slopty-net/src/echo.rs:3:1: replace x"), "slopty-net");
        assert_eq!(package_of("apps/slopty-worker/src/conn.rs:3:1: replace x"), "slopty-workerd");
    }

    #[test]
    fn a_diff_past_the_cap_is_sampled_evenly() {
        assert_eq!(shards_for(10, 40), 1);
        assert_eq!(shards_for(40, 40), 1);
        assert_eq!(shards_for(41, 40), 2);
        assert_eq!(shards_for(400, 40), 10);
        assert_eq!(shards_for(5, 0), 1, "no cap");
    }

    #[test]
    fn the_score_counts_timeouts_caught_and_says_when_nothing_was_scored() {
        assert_eq!(mutation_score(1160, 299), "79.5 %");
        assert_eq!(mutation_score(3, 0), "100.0 %");
        assert_eq!(mutation_score(0, 0), "n/a");
    }

    #[test]
    fn cargo_mutants_exit_codes_say_how_it_ended() {
        assert_eq!(Ended::from_code(Some(0)), Ended::Caught);
        assert_eq!(Ended::from_code(Some(2)), Ended::Missed);
        assert_eq!(Ended::from_code(Some(3)), Ended::TimedOut);
        assert_eq!(Ended::from_code(Some(4)), Ended::BaselineFailed);
        assert_eq!(Ended::from_code(Some(1)), Ended::Broken(Some(1)));
        assert_eq!(Ended::from_code(None), Ended::Broken(None));
        assert_eq!(tier_of("slopty-proto"), Some(Tier::A));
        assert_eq!(tier_of("slopty-net"), None, "only some of it is tier A");
        assert_eq!(tier_of("slopty-workerd"), Some(Tier::B));
    }
}
