//! `xtask remote`: a lane's checks of its working tree, here and on GitHub's runners.
//!
//! `remote` commits the working tree as it is, staged or not, beside the real index, and checks
//! that commit: its crates here, in a lane directory held only while the work here runs
//! (`crate::local`), and what this Mac cannot check on a runner, where it pushes the commit to
//! `check/<name>` and starts `.github/workflows/check.yml` on it: Linux's clippy and tests of
//! the crates named, an `xtask e2e` case, a probe. It waits for the run, prints what failed, and
//! after an e2e case that accepts goldens, or a check that accepts snapshots, applies what the
//! run wrote to the tree. The real index, `HEAD` and the tree are never touched, so lanes editing
//! this checkout keep working meanwhile. Once the verdict is out, an after-pass in the
//! background prunes and warms the lane directories (`docs/decisions/tooling.md`, "A lane's
//! check returns in minutes").
//!
//! The run is dispatched on `gate`, so it reads and fills the compile cache the gate's lanes
//! use. Checks of one name run one after another, a newer one waiting for the older.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use camino::{Utf8Path as Path, Utf8PathBuf as PathBuf};
use xshell::{Shell, cmd};

mod guest;
pub mod stage;

/// The workflow that runs the checks.
const WORKFLOW: &str = "check.yml";

/// The ref it is dispatched on, whose compile cache it shares.
const REF: &str = "gate";

/// How often the run is asked about: a 20 s poll added about 10 s to every check that waited on a
/// runner (`.research/check-speed-2026-10-31/A-anatomy.md` §7).
const POLL: Duration = Duration::from_secs(5);

/// The longest wait for GitHub to list the run after dispatching it.
const FIND: Duration = Duration::from_secs(180);

/// The longest wait for the run to finish, queueing behind the gate's macOS jobs included.
const FINISH: Duration = Duration::from_hours(3);

/// The most lines of a failed log printed; the whole log stays under `target/remote/<name>/`.
const SHOWN: usize = 400;

/// What a runner run's half of a check is told by the half here: set once the check failed here,
/// so the runners' half stops waiting, cancels its run and returns at once.
///
/// The half here fails in seconds (fmt, clippy, a test: a median of 28 s over 38 such checks on
/// 2026-10-16 and 17), while the runners' half went on for the rest of its run (a median of 16
/// min), so a lane heard of a red it could have fixed fifteen minutes earlier, and its run held
/// a Mac slot meanwhile for a verdict the lane's next check made moot.
type Abandon = AtomicBool;

/// Sleeps `period`, a second at a time, and says whether the check was abandoned meanwhile.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling GitHub")]
fn pause(period: Duration, abandon: Option<&Abandon>) -> bool {
    let started = Instant::now();
    while started.elapsed() < period {
        if abandon.is_some_and(|a| a.load(Ordering::Acquire)) {
            return true;
        }
        std::thread::sleep(Duration::from_secs(1).min(period));
    }
    abandon.is_some_and(|a| a.load(Ordering::Acquire))
}

/// The error a check's runners' half returns once the half here failed first.
const ABANDONED: &str = "cancelled: the check failed here first";

/// The artifacts `check.yml`'s jobs leave their logs in, one a job: the lints, the tests, and
/// the probe or e2e case. Each holds that job's `*.log`, and the tests' and the run's a
/// `changes.patch` of what they accepted.
const LOG_ARTIFACTS: [&str; 3] = ["logs-lint", "logs-test", "logs-run"];

/// The runner a check's tests take: Linux when every crate named tests there, as CI's linux lane
/// tests it, so the check takes none of the five macOS runners; a Mac otherwise. A full check
/// takes a Mac too for a crate a Mac shard also tests ([`mac_tested`]): its tests that a Mac
/// alone builds or steers never run on Linux, and so `slopty-client`'s cut-link flake, one Mac
/// run in three, hid from every Linux check of it.
fn test_runner(packages: &[String], full: bool) -> &'static str {
    let linux = crate::gate::linux_tested();
    let on_linux = packages.iter().all(|p| linux.contains(&p.as_str()));
    let mac_too = full && !mac_tested(packages).is_empty();
    if !packages.is_empty() && on_linux && !mac_too { "ubuntu-24.04" } else { "xcode-27" }
}

/// Those of `packages` whose tests a gate's Mac shard runs, all or some: those a Linux check of
/// them leaves out.
fn mac_tested(packages: &[String]) -> Vec<&str> {
    packages.iter().map(String::as_str).filter(|p| !crate::gate::linux_alone(p)).collect()
}

/// The macOS jobs a check holds at most while the gate runs: the gate's three test shards need
/// the other three of the account's five macOS runners. Lane checks once held all five, and
/// the gate's shards queued behind them until each newer push cancelled the run (gate run
/// 38136668846: nothing reached main for over an hour).
const CHECK_MACS: usize = 2;

/// How often `remote` asks again whether a Mac is free for a check.
const MAC_POLL: Duration = Duration::from_secs(60);

/// How many macOS jobs the check asked for takes: the tests when they need a Mac, and a probe
/// or an e2e case on a Mac. Linux's never wait.
fn mac_jobs(opts: &RemoteOpts) -> usize {
    let tests = !opts.packages.is_empty() && a_mac(tests_on(opts));
    let run = (opts.probe.is_some() || opts.e2e.is_some()) && a_mac(&opts.runner);
    usize::from(tests).saturating_add(usize::from(run))
}

/// Whether a runner label names a Mac: GitHub's `macos-*` images and its `xcode-*` ones, which
/// carry a macOS of their own.
fn a_mac(label: &str) -> bool {
    label.starts_with("macos") || label.starts_with("xcode")
}

/// A macOS job queued or running in the repository's Actions.
#[derive(Debug)]
struct MacJob {
    /// A job of the gate: `ci.yml` on `gate`.
    gate: bool,
    /// Waiting for a runner.
    queued: bool,
    /// The check's name, for a run of `check.yml` (`check <name> <sha>`).
    check: Option<String>,
}

/// The lane directory a check's name shares: what comes before its first `-` (`L-h` and `L-x`
/// share `target/local/L`); a name without one has a directory of its own.
fn lane(name: &str) -> &str {
    name.split('-').next().unwrap_or(name)
}

/// Why a check of `name` taking `mine` macOS jobs must wait, given the macOS jobs in flight;
/// `None` once it may start.
/// - The gate goes first: none while any of its macOS jobs waits for a runner.
/// - Checks hold at most [`CHECK_MACS`] macOS jobs, this one's counted. Held means running, or a
///   check's job queued for a runner: it passed this same wait and takes the next free Mac, so two
///   lanes asking at once do not both start. Another workflow's queued jobs are not held: the Deep
///   workflow's matrix lists all its macOS jobs as queued while it runs one at a time, and counting
///   them kept every check waiting for hours (Deep run 38133881188).
/// - A check of the same name holds none: one of its runs still in flight is a stopped check's,
///   since checks of a name run one at a time, and dispatching cancels it (`check.yml`'s
///   concurrency group), giving its Mac back.
fn mac_verdict(jobs: &[MacJob], name: &str, mine: usize) -> Option<String> {
    if jobs.iter().any(|j| j.gate && j.queued) {
        return Some("the gate's macOS jobs are queued".to_owned());
    }
    let held = jobs
        .iter()
        .filter(|j| !j.gate && j.check.as_deref() != Some(name))
        .filter(|j| !j.queued || j.check.is_some())
        .count();
    (held.saturating_add(mine) > CHECK_MACS)
        .then(|| format!("{held} macOS jobs outside the gate already, {CHECK_MACS} at most"))
}

/// The macOS jobs queued or running in the repository, as the Actions API lists them.
fn mac_jobs_in_flight(sh: &Shell) -> Result<Vec<MacJob>> {
    #[derive(serde::Deserialize)]
    struct Runs {
        workflow_runs: Vec<Run>,
    }
    #[derive(serde::Deserialize)]
    struct Run {
        id: u64,
        path: String,
        head_branch: Option<String>,
        display_title: String,
    }
    #[derive(serde::Deserialize)]
    struct Jobs {
        jobs: Vec<Job>,
    }
    #[derive(serde::Deserialize)]
    struct Job {
        status: String,
        labels: Vec<String>,
    }
    let mut found = Vec::new();
    for status in ["queued", "in_progress"] {
        let url = format!("repos/{{owner}}/{{repo}}/actions/runs?status={status}&per_page=100");
        let runs: Runs = serde_json::from_str(&cmd!(sh, "gh api {url}").quiet().read()?)
            .context("the Actions API's runs")?;
        for run in runs.workflow_runs {
            let url = format!("repos/{{owner}}/{{repo}}/actions/runs/{}/jobs?per_page=100", run.id);
            let jobs: Jobs = serde_json::from_str(&cmd!(sh, "gh api {url}").quiet().read()?)
                .context("the Actions API's jobs")?;
            let gate =
                run.path.ends_with("workflows/ci.yml") && run.head_branch.as_deref() == Some(REF);
            let check = run
                .path
                .ends_with(WORKFLOW)
                .then(|| run.display_title.split(' ').nth(1).map(str::to_owned))
                .flatten();
            for job in jobs.jobs {
                let mac = job.labels.iter().any(|l| a_mac(l));
                if mac && matches!(job.status.as_str(), "queued" | "in_progress" | "waiting") {
                    let queued = job.status != "in_progress";
                    found.push(MacJob { gate, queued, check: check.clone() });
                }
            }
        }
    }
    Ok(found)
}

/// `xtask gate-wait`: waits until none of the gate's macOS jobs is queued or running, asking every
/// [`MAC_POLL`], for at most `hours`, saying why on stderr while it waits. Prints `true` once the
/// gate is idle and `false` when it never was, for a workflow's step output. The Deep workflow
/// waits so on a Linux runner before it takes a Mac for hours: GitHub can start its nightly
/// schedule hours late, into the gate's busiest time (run 38133881188 started at 10:58Z).
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling GitHub")]
pub fn gate_wait(sh: &Shell, hours: u64) -> Result<()> {
    let cap = Duration::from_hours(hours);
    let started = Instant::now();
    let mut said = String::new();
    let mut answered = false;
    let idle = loop {
        // A transient API error is asked again, not taken for an idle gate.
        if let Ok(jobs) = mac_jobs_in_flight(sh) {
            answered = true;
            let busy = jobs.iter().filter(|j| j.gate).count();
            if busy == 0 {
                break true;
            }
            let why = format!("{busy} of the gate's macOS jobs queued or running");
            if why != said {
                let secs = started.elapsed().as_secs();
                eprintln!("  waiting for the gate: {why} ({}m{:02}s)", secs / 60, secs % 60);
                said = why;
            }
        }
        if started.elapsed() >= cap {
            ensure!(answered, "the Actions API never answered in {hours} h");
            eprintln!("  the gate was never idle in {hours} h");
            break false;
        }
        std::thread::sleep(MAC_POLL);
    };
    println!("{idle}");
    Ok(())
}

/// Waits until a check of `name` taking `mine` macOS jobs may start ([`mac_verdict`]), asking
/// every [`MAC_POLL`] and saying why while it waits. The lane keeps working meanwhile; its check
/// only starts later.
fn wait_for_mac(sh: &Shell, name: &str, mine: usize, abandon: Option<&Abandon>) -> Result<()> {
    let started = Instant::now();
    let mut said = String::new();
    loop {
        ensure!(
            !abandon.is_some_and(|a| a.load(Ordering::Acquire)),
            "{ABANDONED}; none dispatched"
        );
        // A transient API error is asked again, not taken for a free Mac.
        if let Ok(jobs) = mac_jobs_in_flight(sh) {
            let Some(why) = mac_verdict(&jobs, name, mine) else {
                return Ok(());
            };
            if why != said {
                let secs = started.elapsed().as_secs();
                println!(
                    "  waiting for a Mac slot (gate first): {why} ({}m{:02}s)",
                    secs / 60,
                    secs % 60
                );
                said = why;
            }
        }
        ensure!(started.elapsed() < FINISH, "no Mac slot for {name} after {FINISH:?}");
        ensure!(!pause(MAC_POLL, abandon), "{ABANDONED}; none dispatched");
    }
}

/// What a run does with the goldens of its e2e case.
#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Goldens {
    /// Compare every frame with its golden.
    #[default]
    Compare,
    /// Write every golden its frame differs from by a pixel or a word, and bring them back into
    /// the tree.
    Accept,
    /// Rewrite every golden, and bring them back into the tree.
    AcceptAll,
    /// Fail on no golden and write none: every changed frame's render and diff come back.
    Review,
}

impl Goldens {
    const fn input(self) -> &'static str {
        match self {
            Self::Compare => "compare",
            Self::Accept => "accept",
            Self::AcceptAll => "accept-all",
            Self::Review => "review",
        }
    }
}

/// `cargo xtask remote` options.
#[derive(clap::Args, Clone, Debug)]
pub struct RemoteOpts {
    /// Who asks: a lane's name. Checks of one name run one at a time, a newer one waiting for
    /// the older, and the commit goes to the branch `check/<name>`. Names alike before their
    /// first `-` share a lane directory (`L-H` and `L-X` work in `target/local/L`).
    #[arg(long, env = "SLOPTY_LANE", default_value = "main")]
    pub name: String,
    /// A crate to check (fmt, host clippy, rustdoc, shear, typos and the tests; iOS and Linux
    /// clippy with `--full`); repeat for several.
    #[arg(short = 'p', long = "package")]
    pub packages: Vec<String>,
    /// An `xtask e2e` case to run after the checks: app, ios, server, worker, …; or `auto`, the
    /// smoke cases the check's change reaches (`gate::impact`), each where it runs here.
    #[arg(long)]
    pub e2e: Option<String>,
    /// The e2e case's nextest filterset, such as `test(a_folder_tile_browses_the_worker)`;
    /// `failed` runs only the tests the last run of this name failed, read from its log under
    /// `target/remote/<name>/` (the iteration after a red).
    #[arg(short = 'E', long, requires = "e2e")]
    pub filter: Option<String>,
    /// What the e2e case does with the goldens; an accept applies the run's to the tree.
    #[arg(long, value_enum, default_value_t, requires = "e2e")]
    pub goldens: Goldens,
    /// The simulator the ios case uses: iphone or ipad.
    #[arg(long, default_value = "iphone", requires = "e2e")]
    pub sim: String,
    /// Run the e2e case (in the guest) N times in one session, each on a fresh app and
    /// simulator, keeping every failed run's artifacts, then say how often it failed and how many
    /// clean runs in a row a fix needs at 95 %: the rate of a flake.
    #[arg(long, value_name = "N", requires = "e2e", conflicts_with = "until_fail",
          value_parser = clap::value_parser!(u32).range(1..))]
    pub repeat: Option<u32>,
    /// Run the e2e case (in the guest) up to N times, stopping at the first failure and keeping
    /// its artifacts: whether a fix holds (N from `--repeat`'s rate).
    #[arg(long, value_name = "N", requires = "e2e", value_parser = clap::value_parser!(u32).range(1..))]
    pub until_fail: Option<u32>,
    /// Take the tree's edits under these paths only (files or directories, repeatable), on top
    /// of `HEAD`: another lane's unfinished edits elsewhere stay out of the check.
    #[arg(long = "only", value_name = "PATH")]
    pub only: Vec<String>,
    /// Every check: iOS clippy here and Linux clippy on a runner too, not only fmt, host clippy,
    /// rustdoc and the tests.
    #[arg(long)]
    pub full: bool,
    /// Accept every insta snapshot the tests change, and apply them to the tree.
    #[arg(long)]
    pub accept_snapshots: bool,
    /// Follow run `ID` already started (a wrapper that was stopped, say) instead of starting
    /// one: wait for it, print what failed and apply what it accepted, as the same options say.
    #[arg(long = "run", value_name = "ID")]
    pub attach: Option<u64>,
    /// The runner image: xcode-27, or ubuntu-24.04 / ubuntu-24.04-arm for what needs no Mac.
    #[arg(long, default_value = "xcode-27")]
    pub runner: String,
    /// A shell command to run on the runner after its setup, to try a step there.
    #[arg(long)]
    pub probe: Option<String>,
    /// Check `HEAD` as committed, without the tree's edits: other lanes' unfinished work stays
    /// out of it.
    #[arg(long)]
    pub head: bool,
    /// Dispatch the `check.yml` of this branch instead of `gate`'s, to try a change to the
    /// workflow before it lands. Its compile cache is that branch's, so it starts cold.
    #[arg(long, value_name = "BRANCH")]
    pub workflow_ref: Option<String>,
    /// Keep the branch `check/<name>` once the run ends, which is otherwise deleted then.
    #[arg(long)]
    pub keep_branch: bool,
    /// Where the crates' checks run: here and on runners, whichever returns soonest (`auto`), or
    /// on runners alone (`runner`).
    #[arg(long, value_enum, default_value_t)]
    pub place: Place,
    /// The runner the dispatched tests take, when a local check has taken the Mac's share:
    /// `none`, or Linux for Linux's own paths.
    #[arg(skip)]
    pub tests_on: Option<&'static str>,
    /// The runner lints Linux alone: the rest of the lints ran here.
    #[arg(skip)]
    pub linux_only: bool,
    /// The after-pass a check starts in the background once its verdict is out: the idle lane
    /// directories evicted, a prune pass, and a warm of each directory used lately that is
    /// built at another lock than `HEAD`'s.
    #[arg(long, hide = true)]
    pub after: bool,
    /// Fail at once, naming the holder, rather than wait: for another check of this name, or for
    /// a check, clone or eviction in the lane directory (a warm there gives way at once).
    #[arg(long)]
    pub no_wait: bool,
}

/// Where a check's crates are checked.
#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Place {
    /// fmt, host clippy, rustdoc, shear, typos, the Mac's tests and a full check's iOS clippy on
    /// this Mac, in a lane directory (`crate::local`); a full check's Linux clippy and Linux's
    /// tests, and every e2e case and probe, on runners, side by side.
    #[default]
    Auto,
    /// Everything on runners, as before the router.
    Runner,
}

/// The runner the dispatched tests of `opts` take.
fn tests_on(opts: &RemoteOpts) -> &'static str {
    opts.tests_on.unwrap_or_else(|| test_runner(&opts.packages, opts.full))
}

/// A run as `gh run list` describes it.
#[derive(serde::Deserialize)]
struct Listed {
    #[serde(rename = "databaseId")]
    id: u64,
    #[serde(rename = "displayTitle")]
    title: String,
    url: String,
}

pub fn run(sh: &Shell, opts: &RemoteOpts) -> Result<()> {
    if opts.after {
        return after(sh);
    }
    if opts.filter.as_deref() == Some(FAILED) {
        let mut narrowed = opts.clone();
        narrowed.filter = Some(last_failed(&opts.name)?);
        println!(
            "▶ the tests run {} failed last: {}",
            opts.name,
            narrowed.filter.as_deref().unwrap_or("")
        );
        return run(sh, &narrowed);
    }
    ensure!(
        opts.attach.is_some()
            || !opts.packages.is_empty()
            || opts.e2e.is_some()
            || opts.probe.is_some(),
        "name what to check: `-p <crate>` (repeatable), `--e2e <case>`, `--probe <command>`"
    );
    ensure!(
        opts.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "a check's name is letters, digits, `-` and `_`: {}",
        opts.name
    );
    let root = crate::tools::repo_root()?;
    let dir = root.join("target").join("remote").join(&opts.name);
    std::fs::create_dir_all(&dir)?;
    let checked = {
        // One check of a name at a time: its snapshot's index, its logs and its records are the
        // name's.
        let (_named, waited) = hold_name(&dir, &opts.name, opts.no_wait)?;
        check(sh, opts, &root, &dir, waited)
    };
    after_in_background(&root);
    checked
}

/// A check of `opts`, its records in `dir`, which waited `named` for another check of its name.
fn check(sh: &Shell, opts: &RemoteOpts, root: &Path, dir: &Path, named: Duration) -> Result<()> {
    // The app's e2e case in this Mac's guest rather than on a runner Mac
    // (`docs/decisions/tooling.md`, "An e2e case runs in a guest here").
    let in_guest = opts.attach.is_none()
        && opts.place == Place::Auto
        && matches!(opts.e2e.as_deref(), Some("app" | "ios" | "auto"))
        && opts.probe.is_none()
        && !opts.accept_snapshots
        && e2e_in_guest()
        && guest::available();
    let auto = opts.e2e.as_deref() == Some("auto");
    ensure!(
        !auto || (in_guest && opts.filter.is_none()),
        "--e2e auto runs here, the smoke cases' own tests: --place auto, no --run, --probe or -E, \
         and the guest there (`SLOPTY_E2E_GUEST` not 0)"
    );
    ensure!(
        in_guest || (opts.repeat.is_none() && opts.until_fail.is_none()),
        "--repeat and --until-fail run in the e2e guest: the app or ios case, --place auto, no \
         --run or --probe, and the guest there (`SLOPTY_E2E_GUEST` not 0)"
    );
    let here = opts.attach.is_none()
        && (!opts.packages.is_empty() || in_guest)
        && opts.place == Place::Auto
        && !opts.accept_snapshots
        && cfg!(target_os = "macos");
    if !here {
        let ran = elsewhere(sh, opts, root, dir);
        stage::verdict(dir, ran.is_ok());
        return ran;
    }
    let started = Instant::now();
    // The tree as the lane asked about it, before any wait for its directory.
    let sha = if opts.head {
        cmd!(sh, "git rev-parse HEAD").read()?
    } else {
        format_own(sh, &opts.only);
        snapshot(sh, root, dir, &opts.name, &opts.only)?
    };
    stage::started(sh, dir, &sha, if opts.head { &[] } else { &opts.only });
    let changed = changed_paths(sh, opts.head, &sha)?;
    let place = Workdir::of(&opts.name, &changed);
    let there = runners_part(opts, in_guest);
    let remote = !there.packages.is_empty() || there.e2e.is_some() || there.probe.is_some();
    let case = opts.e2e.as_deref().unwrap_or("app");
    let ios = if case == "ios" {
        let sim = <crate::ios::SimKind as clap::ValueEnum>::from_str(&opts.sim, true)
            .map_err(|e| anyhow::anyhow!("--sim {}: {e}", opts.sim))?;
        Some(sim)
    } else {
        None
    };
    let guest_said = if in_guest {
        format!("; e2e {case} in an e2e guest, {} here", crate::vm::E2E_GUESTS.join(" or "))
    } else {
        String::new()
    };
    let ios_said = if opts.full { ", iOS clippy" } else { "" };
    println!(
        "▶ {} checked here in target/local/{} (fmt, host clippy, rustdoc, shear, typos{ios_said}, \
         the Mac's tests){guest_said}{}",
        opts.packages.join(" "),
        place.name,
        if remote { format!("; on runners: {}", describe(&there)) } else { String::new() }
    );
    let depth = if opts.full { crate::local::Depth::Full } else { crate::local::Depth::Quick };
    // A failure here ends the runners' half at once: its run is cancelled, or never dispatched.
    let abandon = Abandon::new(false);
    let (local, runners) = std::thread::scope(|scope| {
        let local = scope.spawn(|| {
            // The runners' half is dispatched meanwhile: it waits for no lane directory.
            let mut held = match place.hold(root, &sha, opts, true) {
                Ok(held) => held,
                Err(e) => {
                    abandon.store(true, Ordering::Release);
                    return Err(e);
                }
            };
            let queued = held.waited().saturating_add(named);
            let outcome = match tools_hold(root, &place.name, &sha, &changed, &opts.packages) {
                Err(e) => {
                    println!("✘ the gate's tools: {e:#}");
                    Ok(crate::local::Outcome {
                        failed: Some(format!("the gate's tools: {e}")),
                        passed: false,
                        ..crate::local::Outcome::nothing()
                    })
                }
                Ok(()) if opts.packages.is_empty() => Ok(crate::local::Outcome::nothing()),
                Ok(()) => crate::local::check(
                    root,
                    &mut held,
                    &sha,
                    &opts.packages,
                    depth,
                    place.moves_lock,
                ),
            };
            let passed = outcome.as_ref().is_ok_and(|o| o.passed);
            let e2e = if passed && in_guest {
                e2e_runs(root, opts, &place.name, &sha, ios)
            } else {
                Vec::new()
            };
            // The work here is done, the guest's e2e runs with it: the directory goes to the
            // next check while the runners finish. Held through the runners' wait, it sat idle
            // for 44 % of the time it was held, and 29 % of the checks queued for it over 30 s
            // (p90 311 s; `.research/check-speed-2026-10-31/A-anatomy.md` §2).
            drop(held);
            if !passed || e2e.iter().any(|ran| !ran.result.as_ref().is_ok_and(|o| o.passed)) {
                abandon.store(true, Ordering::Release);
            }
            outcome.map(|o| (o, e2e, queued))
        });
        let runners = remote.then(|| on_runners(sh, &there, root, dir, Some(&sha), Some(&abandon)));
        (local.join(), runners)
    });
    let (local, e2e, queued) = match local {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(e)) => {
            stage::verdict(dir, false);
            return Err(e);
        }
        Err(_) => {
            stage::verdict(dir, false);
            bail!("the local check panicked");
        }
    };
    let said: Vec<String> = e2e
        .iter()
        .map(|ran| {
            let said = match &ran.result {
                Ok(o) => format!(
                    "{} in {}: {} ({}; {}m{:02}s in all)",
                    if o.passed { "passed" } else { "FAILED" },
                    o.guest,
                    o.count,
                    o.phases,
                    o.wall.as_secs() / 60,
                    o.wall.as_secs() % 60
                ),
                Err(e) => format!("FAILED: {}", e.to_string().lines().next().unwrap_or("")),
            };
            println!("  e2e {}: {said}", ran.label);
            format!("e2e {} {said}", ran.label)
        })
        .collect();
    let e2e_said = (!said.is_empty()).then(|| said.join("; "));
    let e2e_passed = e2e.iter().all(|ran| ran.result.as_ref().is_ok_and(|o| o.passed));
    let took = started.elapsed().as_secs();
    let thermal = local.thermal.map_or_else(|| "unread".to_owned(), |t| t.to_string());
    if !opts.packages.is_empty() {
        println!(
            "  here: {} in {}m{:02}s; thermal pressure at most {thermal}, load at most {:.1}",
            if local.passed { "passed" } else { "failed" },
            local.took.as_secs() / 60,
            local.took.as_secs() % 60,
            local.load
        );
    }
    record(
        root,
        &Record {
            name: &opts.name,
            full: opts.full,
            packages: &opts.packages,
            local: &local,
            runners: runners.as_ref().map(Result::is_ok),
            took,
            queued,
            dir: &place.name,
            moves_lock: place.moves_lock,
            warm: false,
        },
    );
    for ran in &e2e {
        if let Ok(o) = &ran.result {
            record_e2e(root, opts, ran, o);
        }
    }
    // Each half's verdict, last, where the lane reads: the runners' failure came above the
    // local half's lines, which said "passed", and a lane took the check for green.
    let runners_failed = runners.as_ref().and_then(|r| r.as_ref().err());
    stage::verdict(dir, local.passed && runners_failed.is_none() && e2e_passed);
    if !local.passed || runners_failed.is_some() || !e2e_passed {
        let here = if local.passed { "passed" } else { "FAILED" };
        let here = match &e2e_said {
            Some(said) => format!("{here}; {said}"),
            None => here.to_owned(),
        };
        let said = match (&runners, runners_failed) {
            (None, _) => "nothing ran there".to_owned(),
            (Some(_), None) => "passed".to_owned(),
            (Some(_), Some(e)) => format!("FAILED: {}", e.to_string().lines().next().unwrap_or("")),
        };
        bail!(
            "check {} failed\n  here ({}): {here}\n  on runners ({}): {said}",
            opts.name,
            opts.packages.join(" "),
            if remote { describe(&there) } else { "none".to_owned() }
        );
    }
    let depth = if opts.full { "full" } else { "quick" };
    let and = if remote { " and on runners" } else { "" };
    let what = if opts.packages.is_empty() {
        format!("e2e {case} in the guest")
    } else {
        format!("{depth} check {}", opts.packages.join(" "))
    };
    println!("✔ {what} passed here{and} ({}m{:02}s)", took / 60, took % 60);
    Ok(())
}

/// What of check `opts` only a runner checks, beside the work here: a full check's Linux clippy
/// of the crates that build there, with Linux's tests when every crate tests there too; and the
/// e2e case (unless it runs `in_guest`) or the probe. A full check of crates no Linux builds
/// dispatches nothing: its iOS clippy runs here, where a runner repeated host clippy and rustdoc
/// after a cold setup first (`.research/check-speed-2026-10-31/A-anatomy.md` §4).
fn runners_part(opts: &RemoteOpts, in_guest: bool) -> RemoteOpts {
    let mut there = opts.clone();
    if opts.full {
        let linux = crate::gate::linux_tested();
        let all = opts.packages.iter().all(|p| linux.contains(&p.as_str()));
        there.tests_on = Some(if all { "ubuntu-24.04" } else { "none" });
        there.linux_only = true;
        there.packages.retain(|p| crate::tools::LINUX_CRATES.contains(&p.as_str()));
    } else {
        there.packages.clear();
    }
    if in_guest {
        there.e2e = None;
        there.filter = None;
    }
    there
}

/// A check whose crates are not checked here (`--place runner`, `--run`, an accept): the gate's
/// tool steps the change reaches here first, in seconds, in its lane directory, then the runners'
/// part.
fn elsewhere(sh: &Shell, opts: &RemoteOpts, root: &Path, dir: &Path) -> Result<()> {
    let sha = if opts.attach.is_some() {
        None
    } else {
        let sha = if opts.head {
            cmd!(sh, "git rev-parse HEAD").read()?
        } else {
            format_own(sh, &opts.only);
            snapshot(sh, root, dir, &opts.name, &opts.only)?
        };
        stage::started(sh, dir, &sha, if opts.head { &[] } else { &opts.only });
        let changed = changed_paths(sh, opts.head, &sha)?;
        // The tools' tree in a lane directory, held while they run, and never cloned for them:
        // nothing builds there. Nothing at all when the commit changes nothing (`--head`).
        let tools = if changed.is_empty() {
            Ok(())
        } else {
            let place = Workdir::of(&opts.name, &changed);
            let held = place.hold(root, &sha, opts, false)?;
            let tools = tools_hold(root, &place.name, &sha, &changed, &opts.packages);
            drop(held);
            tools
        };
        if let Err(e) = tools {
            println!("✘ the gate's tools: {e:#}");
            return Err(e.context("the gate's tools"));
        }
        Some(sha)
    };
    on_runners(sh, opts, root, dir, sha.as_deref(), None)
}

/// The lane directory a check works in.
struct Workdir {
    /// Its name under `target/local/`.
    name: String,
    /// Whether the check's commit moves the lock ([`moves_lock`]).
    moves_lock: bool,
}

impl Workdir {
    /// Where check `name` of a commit that changes `changed` works: the directory its name
    /// shares ([`lane`]), or `<name>-lock` of its own when the commit moves the lock, so the
    /// checks that share a directory never build at its lock, nor it at theirs, and a lane's own
    /// directory stays at `HEAD`'s for its checks once the lock lands.
    fn of(name: &str, changed: &[String]) -> Self {
        let moves = moves_lock(changed);
        let name = if moves {
            format!("{name}{}", crate::local::LOCK_SUFFIX)
        } else {
            lane(name).to_owned()
        };
        Self { name, moves_lock: moves }
    }

    /// The directory held for check `opts` of commit `sha`, with `seed` cloned from a warm one
    /// first where it is new, and its state saying at once what the check is
    /// (`local::State::at`).
    fn hold(
        &self,
        root: &Path,
        sha: &str,
        opts: &RemoteOpts,
        seed: bool,
    ) -> Result<crate::local::Held> {
        if self.moves_lock {
            println!(
                "▶ this check moves the lock: it works in target/local/{}, not in \
                 target/local/{}",
                self.name,
                lane(&opts.name)
            );
        }
        let who = crate::local::Who::Check { no_wait: opts.no_wait };
        let mut held = crate::local::hold(root, &self.name, who)?;
        if seed {
            let sh = Shell::new()?;
            sh.change_dir(root);
            let spec = format!("{sha}:Cargo.lock");
            let lock = cmd!(sh, "git show {spec}").quiet().output()?.stdout;
            held.waited_too(crate::local::seed_dir(root, &self.name, &lock, &opts.packages)?);
        }
        let dir = crate::local::dir(root, &self.name);
        crate::local::State::read(&dir).at(sha, self.moves_lock).write(&dir);
        Ok(held)
    }
}

/// The paths commit `sha` changes on its parent, the `HEAD` it was made on: none for a check of
/// `HEAD` as committed.
fn changed_paths(sh: &Shell, head: bool, sha: &str) -> Result<Vec<String>> {
    if head {
        return Ok(Vec::new());
    }
    let parent = format!("{sha}^");
    let changed = cmd!(sh, "git diff --name-only {parent} {sha}").quiet().read()?;
    Ok(changed.lines().map(str::to_owned).collect())
}

/// Whether a change to `changed` (paths from the repository's root) moves what the dependencies
/// build from: the lock, the root manifest, the workspace hack, a vendored crate or submodule,
/// cargo's configuration or the toolchain. Lanes carrying an unlanded lock into a directory
/// others shared alternated gpui revisions minute by minute, and each switch rebuilt gpui and
/// every GUI crate: 22 % of checks recompiled gpui and took 49 % of all local cargo time
/// (`.research/check-speed-2026-10-31/A-anatomy.md` §3.2).
fn moves_lock(changed: &[String]) -> bool {
    changed.iter().any(|path| {
        matches!(path.as_str(), "Cargo.lock" | "Cargo.toml" | "rust-toolchain.toml")
            || ["workspace-hack/", "vendor/", ".cargo/"].iter().any(|dir| path.starts_with(dir))
    })
}

/// The lock on check name `name`'s records (`target/remote/<name>/.lock`), held until dropped,
/// and how long it was waited for; with `no_wait` refused at once. Two agents checking under one
/// name wrote one snapshot index.
fn hold_name(dir: &Path, name: &str, no_wait: bool) -> Result<(std::fs::File, Duration)> {
    let path = dir.join(".lock");
    let file = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("open {path}"))?;
    let asked = Instant::now();
    if file.try_lock().is_err() {
        ensure!(!no_wait, "another check named {name} runs: wait for it (no --no-wait)");
        println!("  waiting for the other check named {name}");
        file.lock().with_context(|| format!("lock {path}"))?;
        println!("  took the name {name} after {}s", asked.elapsed().as_secs());
    }
    Ok((file, asked.elapsed()))
}

/// The gate's tool steps (`gate::Tool`) that the check's commit `sha`, changing `changed`,
/// reaches, over the check's own tree in lane directory `dir`, side by side, before anything
/// compiles: they take seconds, and each was a red only the gate found (a stale
/// `workspace-hack`, a vendored file `cargo fmt --all` reaches, a lock whose new `tempfile` moved
/// the hack; 2026-10-31). A change inside the checked `crates` is formatted, sheared and spelled
/// by their own steps (`crate::check`), so those three run over the whole tree only for a change
/// outside them.
fn tools_hold(
    root: &Path,
    dir: &str,
    sha: &str,
    changed: &[String],
    crates: &[String],
) -> Result<()> {
    if changed.is_empty() {
        return Ok(());
    }
    let changed: Vec<&str> = changed.iter().map(String::as_str).collect();
    let tree = crate::local::sync_tree(root, &crate::local::dir(root, dir), sha)?;
    let dirs: Vec<String> = crate::tools::packages_in(&tree)?
        .into_iter()
        .filter(|package| crates.contains(&package.name))
        .filter_map(|package| Some(format!("{}/", package.dir.strip_prefix(&tree).ok()?)))
        .collect();
    let tools = reached_tools(&changed, &dirs);
    if tools.is_empty() {
        return Ok(());
    }
    let names: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();
    println!("▶ the gate's tools this change reaches: {}", names.join(", "));
    let started = Instant::now();
    let failed: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = tools
            .iter()
            .map(|&tool| {
                let tree = &tree;
                scope.spawn(move || {
                    let sh = Shell::new()?;
                    sh.change_dir(tree);
                    tool.run(&sh)
                })
            })
            .collect();
        tools
            .iter()
            .zip(handles)
            .filter_map(|(tool, handle)| match handle.join() {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(format!("{}: {e:#}", tool.name())),
                Err(_) => Some(format!("{}: panicked", tool.name())),
            })
            .collect()
    });
    ensure!(failed.is_empty(), "{}", failed.join("; "));
    println!("  ✓ the gate's tools ({:.1?})", started.elapsed());
    Ok(())
}

/// The gate's tool steps a change to `changed` reaches, of which a path under one of the
/// checked crates' `dirs` reaches fmt, shear and typos only through those crates' own steps.
fn reached_tools(changed: &[&str], dirs: &[String]) -> Vec<crate::gate::Tool> {
    use crate::gate::Tool;
    let own = |path: &str| dirs.iter().any(|dir| path.starts_with(dir.as_str()));
    Tool::ALL
        .into_iter()
        .filter(|&tool| {
            let crates_judge = matches!(tool, Tool::Rustfmt | Tool::Shear | Tool::Typos);
            changed.iter().any(|path| tool.reached_by(path) && !(crates_judge && own(path)))
        })
        .collect()
}

/// Whether the app's e2e case of a check here goes to this Mac's guest: unless
/// `SLOPTY_E2E_GUEST=0`, or `--place runner`.
fn e2e_in_guest() -> bool {
    std::env::var("SLOPTY_E2E_GUEST").map_or(true, |v| v != "0")
}

/// One e2e run of a check ([`e2e_runs`]): what ran, with which filterset, and how it went.
struct Ran {
    /// The case as the verdict names it: `app`, `ios iphone`, `server`, `linux`, …
    label: String,
    /// The target it ran.
    case: &'static str,
    /// The simulator, for the simulator's case.
    sim: Option<&'static str>,
    /// The filterset it ran with.
    filter: Option<String>,
    /// Its outcome.
    result: Result<guest::Outcome>,
}

/// The check's e2e runs of commit `sha`: the case `--e2e` names, in an e2e guest; or with `--e2e
/// auto`, every smoke case the change reaches (`gate::impact`), from `HEAD` to the commit (from
/// `origin/main` with `--head`): the app's, the server's and, on the iPhone and the iPad, the
/// simulator's in a guest with their smoke tests alone. S6 and the VM's, which need Docker and
/// tart, are named when reached and run nowhere here: this Mac runs no VM
/// (`docs/decisions/tooling.md`, "The gate and e2e run on free hosted runners").
fn e2e_runs(
    root: &Path,
    opts: &RemoteOpts,
    lane: &str,
    sha: &str,
    ios: Option<crate::ios::SimKind>,
) -> Vec<Ran> {
    use crate::gate::impact::Runs;
    use crate::ios::SimKind;
    let repeat = match (opts.repeat, opts.until_fail) {
        (_, Some(n)) => guest::Repeat::UntilFail(n),
        (n, None) => guest::Repeat::Times(n.unwrap_or(1)),
    };
    let in_guest = |target: guest::Target, filter: Option<&str>| {
        let run = guest::Run { name: &opts.name, filter, goldens: opts.goldens, target, repeat };
        let label = match target.sim() {
            Some(sim) => format!("ios {}", sim.family().to_lowercase()),
            None => target.case().to_owned(),
        };
        Ran {
            label: format!("{label} in the guest"),
            case: target.case(),
            sim: target.sim().map(SimKind::family),
            filter: filter.map(str::to_owned),
            result: guest::e2e(root, lane, sha, &run),
        }
    };
    if opts.e2e.as_deref() != Some("auto") {
        let target = match ios {
            Some(sim) => guest::Target::Ios(sim),
            None => guest::Target::App,
        };
        return vec![in_guest(target, opts.filter.as_deref())];
    }
    let base = if opts.head { "origin/main" } else { "HEAD" };
    let reach = Shell::new().map_err(anyhow::Error::from).and_then(|sh| {
        sh.change_dir(root);
        crate::gate::impact::reach(&sh, base, sha)
    });
    let reach = match reach {
        Ok(reach) => reach,
        Err(e) => {
            let result = Err(e.context("what the change reaches"));
            return vec![Ran {
                label: "auto".to_owned(),
                case: "auto",
                sim: None,
                filter: None,
                result,
            }];
        }
    };
    println!("▶ e2e auto, from {base}: {reach}");
    let mut runs = Vec::new();
    let targets = [
        (Runs::App, guest::Target::App),
        (Runs::Server, guest::Target::Server),
        (Runs::Ios, guest::Target::Ios(SimKind::Iphone)),
        (Runs::Ios, guest::Target::Ios(SimKind::Ipad)),
    ];
    for (runs_as, target) in targets {
        if let Some(filter) = reach.filterset(runs_as) {
            runs.push(in_guest(target, Some(&filter)));
        }
    }
    let elsewhere: Vec<&str> = ["S6", "VM"].into_iter().filter(|id| reach.has(id)).collect();
    if !elsewhere.is_empty() {
        println!("  reached and not run here (they need Docker or tart): {}", elsewhere.join(", "));
    }
    runs
}

/// One e2e run in the guest, appended to `target/remote/e2e-history.jsonl`: the case, every
/// phase's seconds and the whole run's, and what nextest ran over all of the case's groups.
fn record_e2e(root: &Path, opts: &RemoteOpts, ran: &Ran, outcome: &guest::Outcome) {
    let tenths = |d: Duration| (d.as_secs_f64() * 10.0).round() / 10.0;
    let phases: serde_json::Map<String, serde_json::Value> =
        outcome.phases.iter().map(|(name, took)| (name.to_owned(), tenths(took).into())).collect();
    let mut tests = serde_json::Map::new();
    tests.insert("run".to_owned(), outcome.count.run.into());
    for (outcome, n) in &outcome.count.outcomes {
        tests.insert(outcome.clone(), (*n).into());
    }
    tests.insert("skipped".to_owned(), outcome.count.skipped.into());
    tests.insert("groups".to_owned(), outcome.count.groups.into());
    let line = serde_json::json!({
        "at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        "name": opts.name,
        "case": ran.case,
        "sim": ran.sim,
        "auto": opts.e2e.as_deref() == Some("auto"),
        "guest": outcome.guest,
        "filter": ran.filter,
        "passed": outcome.passed,
        "wall_s": outcome.wall.as_secs(),
        "phases": phases,
        "tests": tests,
    });
    let path = root.join("target").join("remote").join("e2e-history.jsonl");
    let _written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, format!("{line}\n").as_bytes()));
}

/// One check's latency, as [`record`] writes it.
struct Record<'a> {
    /// The check's name, `<dir>-warm` for a warm.
    name: &'a str,
    /// Whether it was a full check.
    full: bool,
    /// The crates it checked.
    packages: &'a [String],
    /// What the work here found and cost.
    local: &'a crate::local::Outcome,
    /// Whether the runners' part passed, when there was one.
    runners: Option<bool>,
    /// Seconds from its start to its verdict.
    took: u64,
    /// How long it waited for its lane directory.
    queued: Duration,
    /// Its lane directory under `target/local/`.
    dir: &'a str,
    /// Whether its commit moved the lock.
    moves_lock: bool,
    /// Whether it was a warm of the after-pass, not a lane's check.
    warm: bool,
}

/// One check's latency, appended to `target/remote/history.jsonl` for `ci-health`.
fn record(root: &Path, check: &Record<'_>) {
    let line = serde_json::json!({
        "at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        "name": check.name,
        "full": check.full,
        "packages": check.packages,
        "local_s": check.local.took.as_secs(),
        "local_passed": check.local.passed,
        "local_failed": check.local.failed,
        "runners_passed": check.runners,
        "total_s": check.took,
        "queued_s": check.queued.as_secs(),
        "dir": check.dir,
        "moves_lock": check.moves_lock,
        "warm": check.warm,
        "thermal": check.local.thermal,
        "load": check.local.load,
    });
    let path = root.join("target").join("remote").join("history.jsonl");
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, format!("{line}\n").as_bytes()));
    if let Err(e) = written {
        println!("  latency not recorded in {path}: {e}");
    }
}

/// The after-pass, started in the background once a check's verdict is out
/// ([`after_in_background`]). One pass at a time evicts and prunes: a lock-moving check's
/// directory goes once `HEAD` holds what it moved, the lane directories idle for a day go whole
/// (for three hours when the volume is under its floor and headroom), then a prune pass. Then,
/// one pass at a time again, under a lock of its own so the passes behind it still prune, it
/// warms each lane directory used lately whose build is at another lock than `HEAD`'s
/// (`local::warm_due`), so the first check after a gpui-fast bump finds gpui built; none while
/// the volume is under its floor and headroom. A warm holds its directory as one, which gives
/// way to a check that wants it (`local::Held::stoppable`).
fn after(sh: &Shell) -> Result<()> {
    let root = crate::tools::repo_root()?;
    sh.change_dir(&root);
    let remote = root.join("target").join("remote");
    let started = Instant::now();
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    println!("▶ after-pass {} at {at}", std::process::id());
    let head = cmd!(sh, "git rev-parse HEAD").read()?;
    let limits = crate::prune::Limits::from_env()?;
    if let Some(_pruning) = try_hold(&remote.join("after.lock"))? {
        // A directory a lock-moving check worked in, once `HEAD` holds what it moved.
        for (name, sha) in crate::local::lock_dirs(&root) {
            let changed =
                cmd!(sh, "git diff --name-only {head} {sha}").quiet().ignore_stderr().read();
            let changed: Option<Vec<String>> =
                changed.ok().map(|c| c.lines().map(str::to_owned).collect());
            if changed.is_some_and(|changed| !moves_lock(&changed)) {
                crate::local::evict(&root, &name, "HEAD holds the lock its check moved");
            }
        }
        let short = crate::prune::free_space(&root)? < limits.with_headroom()?.floor;
        let idle = if short { crate::local::EVICTED_SHORT } else { Duration::MAX };
        crate::local::evict_idle(&root, idle);
        crate::prune::auto();
    }
    let Some(_warming) = try_hold(&remote.join("warm.lock"))? else {
        println!("  after-pass done, another warms ({:.1?})", started.elapsed());
        return Ok(());
    };
    let free = crate::prune::free_space(&root)?;
    if free < limits.with_headroom()?.floor {
        println!(
            "  no warm: {} GB free, under the floor and its headroom ({:.1?})",
            free / 1_000_000_000,
            started.elapsed()
        );
        return Ok(());
    }
    // The warms' builds are one process group, which a check that wants a directory stops
    // whole: cargo and rustc go with the warm. Spawned by a check, the pass leads one already.
    if let Err(e) = rustix::process::setpgid(None, None) {
        println!("  no warm: the pass leads no process group of its own ({e})");
        return Ok(());
    }
    let spec = format!("{head}:Cargo.lock");
    let head_lock = cmd!(sh, "git show {spec}").quiet().output()?.stdout;
    for (name, packages) in crate::local::warm_due(&root, &head_lock) {
        let Ok(mut held) = crate::local::hold(&root, &name, crate::local::Who::Free) else {
            continue;
        };
        let short = head.get(..10).unwrap_or(&head);
        println!("▶ warming target/local/{name} at {short}: {}", packages.join(" "));
        let started = Instant::now();
        let depth = crate::local::Depth::Warm;
        match crate::local::check(&root, &mut held, &head, &packages, depth, false) {
            Ok(local) => record(
                &root,
                &Record {
                    name: &format!("{name}-warm"),
                    full: false,
                    packages: &packages,
                    local: &local,
                    runners: None,
                    took: started.elapsed().as_secs(),
                    queued: Duration::ZERO,
                    dir: &name,
                    moves_lock: false,
                    warm: true,
                },
            ),
            Err(e) => println!("  the warm of target/local/{name} failed: {e:#}"),
        }
        drop(held);
    }
    println!("  after-pass done ({:.1?})", started.elapsed());
    Ok(())
}

/// The lock file at `path` held until dropped, or `None` when another holds it.
fn try_hold(path: &Path) -> Result<Option<std::fs::File>> {
    let file = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("open {path}"))?;
    Ok(file.try_lock().is_ok().then_some(file))
}

/// The largest the after-pass's log grows before a pass starts it again.
const AFTER_LOG: u64 = 4 << 20;

/// Start the after-pass ([`after`]) in a process group of its own, its output in
/// `target/remote/after.log`, and return at once: a prune pass before and after every check took
/// about 5 s of each and ran inside it (`.research/check-speed-2026-10-31/A-anatomy.md` §1.1).
fn after_in_background(root: &Path) {
    use std::os::unix::process::CommandExt as _;
    let Ok(exe) = std::env::current_exe() else { return };
    let log = root.join("target").join("remote").join("after.log");
    let fresh = std::fs::metadata(&log).is_ok_and(|m| m.len() > AFTER_LOG);
    let opened = std::fs::OpenOptions::new()
        .create(true)
        .append(!fresh)
        .write(true)
        .truncate(fresh)
        .open(&log);
    let Ok(out) = opened else { return };
    let Ok(err) = out.try_clone() else { return };
    let started = std::process::Command::new(exe)
        .args(["remote", "--after"])
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err)
        .process_group(0)
        .spawn();
    if let Err(e) = started {
        println!("  the after-pass did not start: {e}");
    }
}

/// The runners' part of a check: dispatched on the commit `sha` (made here when `None`), or the
/// run `--run` names, waited for, its logs fetched and what it accepted applied.
fn on_runners(
    sh: &Shell,
    opts: &RemoteOpts,
    root: &Path,
    dir: &Path,
    sha: Option<&str>,
    abandon: Option<&Abandon>,
) -> Result<()> {
    let started = Instant::now();
    let mut what = describe(opts);
    let (run, short) = if let Some(id) = opts.attach {
        let run_id = id.to_string();
        let viewed = cmd!(sh, "gh run view {run_id} --json url,displayTitle").read()?;
        let viewed: Viewed = serde_json::from_str(&viewed).context("gh run view's JSON")?;
        (Listed { id, title: viewed.title, url: viewed.url }, format!("run {id}"))
    } else {
        let sha = if let Some(sha) = sha {
            sha.to_owned()
        } else {
            let sha = if opts.head {
                cmd!(sh, "git rev-parse HEAD").read()?
            } else {
                format_own(sh, &opts.only);
                snapshot(sh, root, dir, &opts.name, &opts.only)?
            };
            stage::started(sh, dir, &sha, if opts.head { &[] } else { &opts.only });
            sha
        };
        let on = opts.workflow_ref.as_deref().unwrap_or(REF);
        let opts = &actions_present(sh, on, &sha, opts)?;
        what = describe(opts);
        // The tree as it was asked about, dispatched once a Mac is free for it.
        let macs = mac_jobs(opts);
        if macs > 0 {
            wait_for_mac(sh, &opts.name, macs, abandon)?;
        }
        let branch = format!("check/{}", opts.name);
        cmd!(sh, "git push --quiet --force --no-verify origin {sha}:refs/heads/{branch}")
            .run()
            .context("pushing the snapshot")?;
        let title = format!("check {} {sha}", opts.name);
        // A check of the same commit under the same name ran before (a retry on `--head`): its
        // runs are known now, so the one found below is the one this dispatch starts.
        let before: Vec<u64> = listed(sh)?.into_iter().map(|r| r.id).collect();
        dispatch(sh, opts, &sha)?;
        let run = find(sh, &title, &before)?;
        (run, sha.get(..10).unwrap_or(&sha).to_owned())
    };
    println!("▶ {short} ({what}) on a runner: {}", run.url);
    println!("  if this is stopped: cargo xtask remote --name {} --run {} …", opts.name, run.id);
    let conclusion = finish(sh, run.id, abandon)?;
    if !opts.keep_branch
        && let Some((name, sha)) = checked(&run.title)
    {
        let _deleted = delete_branch(sh, name, sha);
    }
    if conclusion == ABANDONED {
        bail!("{ABANDONED}: run {} cancelled", run.id);
    }
    let logs = dir.join("logs");
    if logs.exists() {
        std::fs::remove_dir_all(&logs)?;
    }
    let jobs: Vec<PathBuf> = LOG_ARTIFACTS
        .iter()
        .filter_map(|name| download(sh, run.id, name, &logs.join(name)))
        .collect();
    if conclusion != "success" {
        for job in &jobs {
            for name in ["lint.log", "test.log", "probe.log", "e2e.log"] {
                let path = job.join(name);
                if path.exists() {
                    show(&path)?;
                }
            }
        }
    }
    if opts.e2e.is_some() {
        let renders = root.join("target").join("e2e").join("artifacts");
        if download(sh, run.id, "renders", &renders).is_some() {
            println!("  renders and diffs of changed frames: {renders}");
        }
    }
    let accepts =
        opts.accept_snapshots || matches!(opts.goldens, Goldens::Accept | Goldens::AcceptAll);
    if accepts {
        let patches: Vec<PathBuf> =
            jobs.iter().map(|j| j.join("changes.patch")).filter(|p| p.exists()).collect();
        ensure!(!patches.is_empty(), "the run left no patch of what it accepted");
        let mut files = 0_usize;
        for patch in patches.iter().filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() > 0)) {
            files = files.saturating_add(apply_accepted(sh, patch)?);
        }
        if files == 0 {
            println!("  the run accepted nothing new");
        } else {
            println!("  {files} accepted files applied to the tree");
        }
    }
    let took = started.elapsed().as_secs();
    if conclusion == "success" {
        println!("✔ {what} passed on {short} ({}m{:02}s)", took / 60, took % 60);
        return Ok(());
    }
    bail!(
        "{what} {conclusion} on {short} ({}m{:02}s): {}\n  every log: {}",
        took / 60,
        took % 60,
        run.url,
        dir
    )
}

/// The `-E` that reruns the e2e tests the last run of a check's name failed.
const FAILED: &str = "failed";

/// The nextest filterset of the e2e tests the last run of check `name` failed, from the e2e log
/// it brought back (`target/remote/<name>/logs/logs-run/e2e.log`).
fn last_failed(name: &str) -> Result<String> {
    let log =
        crate::tools::repo_root()?.join("target/remote").join(name).join("logs/logs-run/e2e.log");
    let text = std::fs::read_to_string(&log)
        .with_context(|| format!("no e2e log of the last run of {name} at {log}"))?;
    let failed = failed_tests(&text);
    ensure!(!failed.is_empty(), "the last e2e run of {name} failed no test ({log})");
    Ok(failed.iter().map(|t| format!("test(={t})")).collect::<Vec<_>>().join(" | "))
}

/// The tests nextest's status lines in `log` name as failed (`FAIL`, `TIMEOUT`, a signal, a
/// leak), each once, in order. A retry's (`TRY 1 FAIL`) is not one, and neither is the tests'
/// own output that the summary shows under a failure.
fn failed_tests(log: &str) -> Vec<String> {
    let mut failed: Vec<String> = Vec::new();
    for line in log.lines() {
        let plain = without_colour(line);
        let words: Vec<&str> = plain.split_whitespace().collect();
        let [status, time, .., binary, test] = words.as_slice() else { continue };
        let signal = status.len() > 3
            && status.starts_with("SIG")
            && status.chars().all(|c| c.is_ascii_uppercase());
        let red = matches!(*status, "FAIL" | "TIMEOUT" | "LEAK-FAIL" | "ABORT") || signal;
        if red
            && time.starts_with('[')
            && binary.contains("::")
            && !failed.iter().any(|t| t == test)
        {
            failed.push((*test).to_owned());
        }
    }
    failed
}

/// `line` without its terminal colour codes (`ESC [ … m`), which a runner's log keeps
/// (`CARGO_TERM_COLOR=always`).
fn without_colour(line: &str) -> String {
    let mut plain = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            plain.push(c);
        }
    }
    plain
}

/// The paths in `git apply --numstat`'s report that no accept writes: an accept writes the e2e
/// goldens and insta's snapshots, nothing else. A patch with others was cut from more of the
/// runner's tree than the accepted files (run 38110915311 brought 61 MB of zig's cache).
fn unaccepted(numstat: &str) -> Vec<&str> {
    numstat
        .lines()
        .filter_map(|line| line.splitn(3, '\t').nth(2))
        .filter(|path| {
            !(path.starts_with("crates/slopty-e2e/golden/")
                || Path::new(path).extension() == Some("snap"))
        })
        .collect()
}

/// Applies the patch of what a run accepted to the tree, once it is shown to touch only what an
/// accept writes, and says how many files it changed.
fn apply_accepted(sh: &Shell, patch: &Path) -> Result<usize> {
    let numstat = cmd!(sh, "git apply --numstat {patch}").read()?;
    let others = unaccepted(&numstat);
    ensure!(
        others.is_empty(),
        "{patch} touches {} paths no accept writes, so none of it is applied: {}",
        others.len(),
        others.iter().take(8).copied().collect::<Vec<_>>().join(", ")
    );
    cmd!(sh, "git apply --binary {patch}")
        .run()
        .context("applying what the run accepted to the tree")?;
    Ok(numstat.lines().count())
}

/// What the run checks, in a few words.
fn describe(opts: &RemoteOpts) -> String {
    let mut parts = Vec::new();
    if !opts.packages.is_empty() && opts.linux_only {
        let tests = tests_on(opts);
        let tests = if tests == "none" { String::new() } else { format!(", tests on {tests}") };
        parts.push(format!("Linux clippy of {}{tests}", opts.packages.join(" ")));
    } else if !opts.packages.is_empty() {
        let depth = if opts.full { "full" } else { "quick" };
        let tests = tests_on(opts);
        let mac = mac_tested(&opts.packages);
        let left = if tests.starts_with("ubuntu") && !mac.is_empty() && opts.tests_on.is_none() {
            format!("; the Mac's own tests of {} wait for --full or the gate", mac.join(", "))
        } else {
            String::new()
        };
        let on =
            if tests == "none" { "tests here".to_owned() } else { format!("tests on {tests}") };
        parts.push(format!("{depth} check {} ({on}{left})", opts.packages.join(" ")));
    }
    if let Some(case) = &opts.e2e {
        let filter = opts.filter.as_deref().map(|f| format!(" {f}")).unwrap_or_default();
        parts.push(format!("e2e {case}{filter} ({})", opts.goldens.input()));
    }
    if let Some(probe) = &opts.probe {
        parts.push(format!("probe `{probe}`"));
    }
    if opts.runner != "xcode-27" {
        parts.push(format!("on {}", opts.runner));
    }
    parts.join(", ")
}

/// The working tree as a commit on `HEAD`, written through an index of its own, so the real
/// index stays as the person or a lane staged it. With no `only`, it starts from a copy of the
/// real index, whose stat cache spares hashing what did not change, and takes every edit; with
/// `only`, it starts from `HEAD` and takes the edits under those paths alone, its lock files
/// resolved for what it holds ([`locks`]). Files `.gitignore` names stay out, as they would
/// from a commit; the submodules go at the commits checked out in them.
fn snapshot(sh: &Shell, root: &Path, dir: &Path, name: &str, only: &[String]) -> Result<String> {
    let index = dir.join("index");
    if only.is_empty() {
        let real = cmd!(sh, "git rev-parse --git-path index").read()?;
        std::fs::copy(root.join(real), &index).context("copying the index")?;
        cmd!(sh, "git add --all")
            .env("GIT_INDEX_FILE", &index)
            .quiet()
            .run()
            .context("adding the tree to the snapshot")?;
    } else {
        cmd!(sh, "git read-tree HEAD").env("GIT_INDEX_FILE", &index).quiet().run()?;
        // The lock files are what cargo makes of the manifests, so they are made below, never
        // taken from the tree, which other lanes' manifests moved too.
        let unlocked: Vec<String> = LOCKS.iter().map(|lock| format!(":(exclude){lock}")).collect();
        cmd!(sh, "git add --all -- {only...} {unlocked...}")
            .env("GIT_INDEX_FILE", &index)
            .quiet()
            .run()
            .context("adding the paths to the snapshot")?;
        locks(sh, root, dir, &index, only)?;
    }
    let tree = cmd!(sh, "git write-tree").env("GIT_INDEX_FILE", &index).quiet().read()?;
    let message = format!("check: {name}");
    let sha = cmd!(sh, "git commit-tree --no-gpg-sign -p HEAD -m {message} {tree}").read()?;
    Ok(sha)
}

/// Where the Rust files the gate's `cargo fmt --all` formats live: the workspace's members, the
/// vendored crates it reaches as path dependencies (`noq-proto` and `noq-udp`, outside the
/// workspace; their re-vendored files passed a check and went red at the gate, 2026-10-31), and
/// the fuzz crate, which the gate formats on its own.
const FORMATTED: [&str; 6] = ["crates/", "apps/", "xtask/", "workspace-hack/", "vendor/", "fuzz/"];

/// The Rust files a lane changed under `only` (against `HEAD`, untracked ones too), formatted in
/// the tree with nightly rustfmt and the repository's `rustfmt.toml`, as `cargo xtask fmt` would,
/// before they are checked: a check never fails on formatting. Of the runners' lint failures on
/// 2026-10-16 and 17, 9 of 42 were `cargo fmt` (`.research/devloop-2026-10-17.md`). Only the
/// lane's own paths are touched, so another lane's files never move under it; without `only`
/// nothing is formatted and the check's fmt step judges. A file rustfmt cannot parse is left to
/// clippy to name.
fn format_own(sh: &Shell, only: &[String]) {
    if only.is_empty() {
        return;
    }
    let listed = |args: &[&str]| -> Vec<String> {
        cmd!(sh, "git {args...} -- {only...}")
            .quiet()
            .ignore_stderr()
            .read()
            .map(|out| out.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    };
    let mut files = listed(&["diff", "--name-only", "--diff-filter=d", "HEAD"]);
    files.extend(listed(&["ls-files", "--others", "--exclude-standard"]));
    files.sort_unstable();
    files.dedup();
    files.retain(|f| {
        Path::new(f).extension() == Some("rs")
            && FORMATTED.iter().any(|dir| f.starts_with(dir))
            && Path::new(f).is_file()
    });
    if files.is_empty() {
        return;
    }
    let before: Vec<Option<Vec<u8>>> = files.iter().map(|f| std::fs::read(f).ok()).collect();
    let f = &files;
    let ran = cmd!(sh, "rustup run nightly rustfmt --config skip_children=true {f...}")
        .quiet()
        .ignore_status()
        .output();
    if let Ok(out) = &ran
        && !out.status.success()
    {
        let why = String::from_utf8_lossy(&out.stderr);
        println!("  rustfmt left some files as they were: {}", why.lines().next().unwrap_or(""));
    }
    let moved: Vec<&String> = files
        .iter()
        .zip(&before)
        .filter(|(f, was)| std::fs::read(f).ok() != **was)
        .map(|(f, _)| f)
        .collect();
    if !moved.is_empty() {
        let names: Vec<&str> = moved.iter().map(|f| f.as_str()).collect();
        println!("  formatted {} of your files before the check: {}", moved.len(), names.join(" "));
    }
}

/// The workspaces' lock files, each beside the manifest of a workspace of its own.
const LOCKS: [&str; 2] = ["Cargo.lock", "fuzz/Cargo.lock"];

/// Make the snapshot's lock files at `index` hold what its manifests need: `HEAD`'s lock (the
/// tree's, when `only` names it) resolved again by cargo against the manifests the snapshot
/// holds, which are `HEAD`'s with the edits under `only`.
///
/// Taken from the tree, a lock carries what every lane's manifest edits made of it, and a check
/// with `cargo --locked` of a snapshot without those manifests fails. Left at `HEAD`'s, it lacks
/// what the snapshot's own manifest edits add. So the snapshot is resolved apart, in `dir`: its
/// manifests as they are, each of its Rust files as an empty one (cargo finds targets by their
/// paths and never reads them to resolve), and its lock files to start from. Cargo resolves
/// offline, keeping every version the lock pins, so a lock differs only by what the manifests
/// changed. Nothing happens when no manifest and no lock is among the snapshot's edits.
fn locks(sh: &Shell, root: &Path, dir: &Path, index: &Path, only: &[String]) -> Result<()> {
    let named = |lock: &str| {
        only.iter().any(|path| {
            let path = path.trim_end_matches('/');
            path == lock || path == "." || lock.starts_with(&format!("{path}/"))
        })
    };
    let changed = cmd!(sh, "git diff-index --cached --name-only HEAD")
        .env("GIT_INDEX_FILE", index)
        .quiet()
        .read()?;
    if !changed.lines().any(|path| path.ends_with("Cargo.toml")) && !LOCKS.iter().any(|l| named(l))
    {
        return Ok(());
    }
    let export = dir.join("locks");
    if export.exists() {
        std::fs::remove_dir_all(&export).with_context(|| format!("clearing {export}"))?;
    }
    let listed =
        cmd!(sh, "git ls-files --cached -z").env("GIT_INDEX_FILE", index).quiet().read()?;
    for path in listed.split('\0').filter(|p| !p.is_empty()) {
        let to = export.join(path);
        if path.ends_with("Cargo.toml") {
            let spec = format!(":{path}");
            let text = cmd!(sh, "git show {spec}").env("GIT_INDEX_FILE", index).quiet().read()?;
            write_new(&to, text.as_bytes())?;
        } else if Path::new(path).extension() == Some("rs") {
            write_new(&to, b"")?;
        }
    }
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    for lock in LOCKS {
        let base = if named(lock) {
            std::fs::read(root.join(lock)).with_context(|| format!("reading {lock}"))?
        } else {
            let spec = format!("HEAD:{lock}");
            cmd!(sh, "git show {spec}").quiet().ignore_stderr().ignore_status().output()?.stdout
        };
        let manifest = export.join(lock).with_file_name("Cargo.toml");
        if base.is_empty() || !manifest.exists() {
            continue;
        }
        write_new(&export.join(lock), &base)?;
        cmd!(sh, "{cargo} metadata --offline --format-version 1 --manifest-path {manifest}")
            .quiet()
            .ignore_stdout()
            .run()
            .with_context(|| format!("resolving {lock} for the snapshot"))?;
        let made = export.join(lock);
        let blob = cmd!(sh, "git hash-object -w {made}").read()?;
        let entry = format!("100644,{blob},{lock}");
        cmd!(sh, "git update-index --add --cacheinfo {entry}")
            .env("GIT_INDEX_FILE", index)
            .quiet()
            .run()?;
    }
    Ok(())
}

/// Write `bytes` to `path`, making its directory.
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("making {parent}"))?;
    }
    std::fs::write(path, bytes).with_context(|| format!("writing {path}"))
}

/// The local actions (`uses: ./<path>`) `workflow`'s text runs, by path.
fn local_actions(workflow: &str) -> Vec<&str> {
    let mut paths: Vec<&str> = workflow
        .lines()
        .filter_map(|line| line.trim_start().trim_start_matches("- ").strip_prefix("uses: ./"))
        .map(str::trim)
        .collect();
    paths.sort_unstable();
    paths.dedup();
    paths
}

/// Check `opts` as `on`'s [`WORKFLOW`] can run it ([`fit`]), refused when it cannot, or when its
/// snapshot `sha` lacks a local action the workflow runs. The workflow comes from `on` and the
/// actions from the snapshot, so a commit that removes one and is not on `on` yet failed every
/// lane's check at setup (run 38153490688, the zig action). The check's own branch,
/// `check/<name>`, is pushed only after this, the snapshot on it: its workflow is the
/// snapshot's.
fn actions_present(sh: &Shell, on: &str, sha: &str, opts: &RemoteOpts) -> Result<RemoteOpts> {
    let path = if on == format!("check/{}", opts.name) {
        format!("{sha}:.github/workflows/{WORKFLOW}")
    } else {
        cmd!(sh, "git fetch --quiet origin {on}")
            .run()
            .with_context(|| format!("fetching {on}"))?;
        // The remote-tracking branch, not `FETCH_HEAD`, which another lane's fetch may replace.
        format!("origin/{on}:.github/workflows/{WORKFLOW}")
    };
    let workflow = cmd!(sh, "git show {path}").quiet().read()?;
    let opts = fit(&workflow, on, opts);
    if let Some(unfit) = unfit(&workflow, &opts) {
        bail!(
            "{on}'s {WORKFLOW} cannot run this check: {unfit}. Land the workflow change first, or \
             try it with --workflow-ref <branch>"
        );
    }
    let missing: Vec<&str> = local_actions(&workflow)
        .into_iter()
        .filter(|action| {
            let file = format!("{sha}:{action}/action.yml");
            cmd!(sh, "git cat-file -e {file}").quiet().ignore_stderr().run().is_err()
        })
        .collect();
    ensure!(
        missing.is_empty(),
        "{on}'s {WORKFLOW} runs {}, which this tree lacks: land the change that removed it \
         first, or try that change's workflow with --workflow-ref",
        missing.iter().map(|m| format!("./{m}")).collect::<Vec<_>>().join(", ")
    );
    Ok(opts)
}

/// Check `opts` as `on`'s `workflow` (the text of its [`WORKFLOW`]) can run it: one without the
/// `lint` input, older than a full check's lints here, lints everything on the runner, as
/// before, rather than refuse the check.
fn fit(workflow: &str, on: &str, opts: &RemoteOpts) -> RemoteOpts {
    let mut fitted = opts.clone();
    if opts.linux_only && !declares(workflow, "lint") {
        println!("  {on}'s {WORKFLOW} has no `lint` input yet: the runner lints all of it again");
        fitted.linux_only = false;
    }
    fitted
}

/// Whether `workflow` declares the input `key`.
fn declares(workflow: &str, key: &str) -> bool {
    workflow.lines().any(|l| l.trim_end() == format!("      {key}:"))
}

/// Why `workflow` (the text of the [`WORKFLOW`] a check dispatches) cannot run what `opts` asks,
/// or `None` when it can: an input it would be sent and does not declare, or tests on no runner
/// (`test_runner: none`) where its test job does not stand aside for them. A `check.yml` older
/// than the change that made `none` queued the test job for a runner named "none", which no
/// runner serves, and the lane waited two hours (run 38234704645).
fn unfit(workflow: &str, opts: &RemoteOpts) -> Option<String> {
    let sent = dispatched(opts, "");
    let undeclared: Vec<&str> =
        sent.iter().map(|(key, _)| *key).filter(|key| !declares(workflow, key)).collect();
    if !undeclared.is_empty() {
        return Some(format!("it declares no input {}", undeclared.join(", ")));
    }
    let none = sent.iter().any(|(key, value)| *key == "test_runner" && value == "none");
    (none && !workflow.contains("inputs.test_runner != 'none'"))
        .then(|| "its test job does not stand aside for `test_runner: none`".to_owned())
}

/// The inputs a dispatch of `opts` on the commit `sha` sends: each that differs from the
/// workflow's default, and always the name and the commit.
fn dispatched(opts: &RemoteOpts, sha: &str) -> Vec<(&'static str, String)> {
    let snapshots = if opts.accept_snapshots { "accept" } else { "compare" };
    let lint = if opts.linux_only { "linux" } else { "all" };
    let fields: [(&str, String, &str); 13] = [
        ("name", opts.name.clone(), ""),
        ("sha", sha.to_owned(), ""),
        ("packages", opts.packages.join(" "), ""),
        ("e2e", opts.e2e.clone().unwrap_or_default(), ""),
        ("filter", opts.filter.clone().unwrap_or_default(), ""),
        ("goldens", opts.goldens.input().to_owned(), "compare"),
        ("sim", opts.sim.clone(), "iphone"),
        ("snapshots", snapshots.to_owned(), "compare"),
        ("full", (if opts.full { "yes" } else { "no" }).to_owned(), "no"),
        ("lint", lint.to_owned(), "all"),
        ("runner", opts.runner.clone(), "xcode-27"),
        ("test_runner", tests_on(opts).to_owned(), "xcode-27"),
        ("probe", opts.probe.clone().unwrap_or_default(), ""),
    ];
    fields
        .into_iter()
        .filter(|(key, value, default)| matches!(*key, "name" | "sha") || value != default)
        .map(|(key, value, _)| (key, value))
        .collect()
}

/// Start the workflow for `sha`, on [`REF`] unless `--workflow-ref` names a branch whose
/// `check.yml` to try. An input goes only when it differs from the workflow's default, so a tree
/// whose xtask knows an input `gate`'s `check.yml` does not yet have still dispatches.
fn dispatch(sh: &Shell, opts: &RemoteOpts, sha: &str) -> Result<()> {
    let args: Vec<String> = dispatched(opts, sha)
        .into_iter()
        .flat_map(|(key, value)| ["-f".to_owned(), format!("{key}={value}")])
        .collect();
    let on = opts.workflow_ref.as_deref().unwrap_or(REF);
    cmd!(sh, "gh workflow run {WORKFLOW} --ref {on} {args...}")
        .quiet()
        .ignore_stdout()
        .run()
        .context("gh workflow run (is `gh` logged in, and check.yml on main and gate?)")
}

/// A run as `gh run view` describes it.
#[derive(serde::Deserialize)]
struct Viewed {
    url: String,
    #[serde(rename = "displayTitle")]
    title: String,
}

/// The check's name and the commit it checked, from its run's title, `check <name> <sha>`
/// (`check.yml`'s `run-name`).
fn checked(title: &str) -> Option<(&str, &str)> {
    let mut words = title.strip_prefix("check ")?.split(' ');
    let (name, sha) = (words.next()?, words.next()?);
    let hex = sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit());
    (words.next().is_none() && !name.is_empty() && hex).then_some((name, sha))
}

/// Delete the branch `check/<name>` while it still holds `sha`: a newer check of the same name
/// pushed over it holds a run that is not done. Gone already, or moved on, it stays as it is.
/// Whether it went.
fn delete_branch(sh: &Shell, name: &str, sha: &str) -> bool {
    let branch = format!("refs/heads/check/{name}");
    let lease = format!("--force-with-lease={branch}:{sha}");
    let deleted = cmd!(sh, "git push --quiet --no-verify {lease} origin :{branch}")
        .quiet()
        .ignore_stderr()
        .run()
        .is_ok();
    if deleted {
        println!("  deleted the branch check/{name}");
    }
    deleted
}

/// How old a check branch is when [`sweep`] deletes it, its runs done.
const SWEPT_AFTER: Duration = Duration::from_hours(24);

/// Delete every `check/*` branch older than [`SWEPT_AFTER`] whose runs are all done: those of
/// checks stopped before their run ended, and those kept. Its age is its newest run's, or, with
/// none listed, its commit's. Each goes only while it still holds the commit it was read at.
/// `land` runs it after each push.
pub fn sweep(sh: &Shell) -> Result<()> {
    let heads = cmd!(sh, "git ls-remote --heads origin refs/heads/check/*").quiet().read()?;
    let branches: Vec<(&str, &str)> = heads
        .lines()
        .filter_map(|line| {
            let (sha, branch) = line.split_once('\t')?;
            Some((branch.strip_prefix("refs/heads/check/")?, sha))
        })
        .collect();
    if branches.is_empty() {
        return Ok(());
    }
    // Title, status and start (seconds since the epoch) of the checks' newest runs.
    let jq = ".[] | [.displayTitle, .status, (.createdAt | fromdateiso8601)] | @tsv";
    let runs = cmd!(
        sh,
        "gh run list --workflow {WORKFLOW} --limit 500 --json displayTitle,status,createdAt --jq {jq}"
    )
    .quiet()
    .read()
    .context("gh run list")?;
    let runs: Vec<(&str, &str, u64)> = runs
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let (title, status) = (fields.next()?, fields.next()?);
            Some((title, status, fields.next()?.parse().ok()?))
        })
        .collect();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("the clock is before 1970")?
        .as_secs();
    let mut swept = 0_usize;
    for (name, sha) in branches {
        let title = format!("check {name} {sha}");
        let mine: Vec<&(&str, &str, u64)> = runs.iter().filter(|r| r.0 == title).collect();
        if mine.iter().any(|r| r.1 != "completed") {
            continue;
        }
        let born = mine.iter().map(|r| r.2).max().or_else(|| {
            let jq = ".commit.committer.date | fromdateiso8601";
            let commit = format!("repos/{{owner}}/{{repo}}/commits/{sha}");
            cmd!(sh, "gh api {commit} --jq {jq}").quiet().read().ok()?.trim().parse().ok()
        });
        let old = born.is_some_and(|born| now.saturating_sub(born) >= SWEPT_AFTER.as_secs());
        if old && delete_branch(sh, name, sha) {
            swept = swept.saturating_add(1);
        }
    }
    if swept > 0 {
        println!("  swept {swept} check branches a day old whose runs were done");
    }
    Ok(())
}

/// The newest dispatched runs of [`WORKFLOW`].
fn listed(sh: &Shell) -> Result<Vec<Listed>> {
    let listed = cmd!(
        sh,
        "gh run list --workflow {WORKFLOW} --event workflow_dispatch --limit 30 --json databaseId,displayTitle,url"
    )
    .quiet()
    .read()
    .context("gh run list")?;
    serde_json::from_str(&listed).context("gh run list's JSON")
}

/// The run titled `title` that is none of `before`, once GitHub lists it.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling GitHub")]
fn find(sh: &Shell, title: &str, before: &[u64]) -> Result<Listed> {
    let started = Instant::now();
    loop {
        let fresh = listed(sh)?.into_iter().find(|r| r.title == title && !before.contains(&r.id));
        if let Some(run) = fresh {
            return Ok(run);
        }
        ensure!(started.elapsed() < FIND, "GitHub listed no run `{title}` after {FIND:?}");
        std::thread::sleep(POLL);
    }
}

/// The run's conclusion once it completes, printing each change of its status meanwhile.
///
/// The first red ends it: once the lint job failed while other jobs still run, the run is
/// cancelled and taken for failed, since the lane fixes the lint and checks again, and its Mac
/// job would hold a slot meanwhile (`docs/decisions/tooling.md`, "A check's first red ends it").
/// Once the check failed here (`abandon`), the run is cancelled and `ABANDONED` returned.
///
/// Each poll asks for the run alone, and every [`JOBS_EVERY`] polls, or when its status moves,
/// for its jobs (their reds, and the runners the queued ones ask for), each only if it changed
/// since it was last read ([`Conditional`]): an answer that it did not costs nothing of the
/// hourly limit every lane and the gate's tooling share. `gh run view --json
/// status,conclusion,jobs` made three requests a poll, up to 2,160 an hour for one check at a
/// 5 s poll (`docs/MEASUREMENTS.md`).
fn finish(sh: &Shell, id: u64, abandon: Option<&Abandon>) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct Run {
        status: String,
        conclusion: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Jobs {
        jobs: Vec<RunJob>,
    }
    let mut run = Conditional::new(format!("repos/{{owner}}/{{repo}}/actions/runs/{id}"));
    let mut jobs =
        Conditional::new(format!("repos/{{owner}}/{{repo}}/actions/runs/{id}/jobs?per_page=100"));
    let id = id.to_string();
    let started = Instant::now();
    let mut last = String::new();
    let mut ended_early = false;
    let mut polls = 0_u32;
    loop {
        // A transient API error is asked again, not taken for the run's end.
        let state = run.get(sh).and_then(|body| serde_json::from_str::<Run>(body).ok());
        if let Some(state) = state {
            if state.status == "completed" {
                let conclusion = state.conclusion.unwrap_or_default();
                return Ok(if ended_early { "failure".to_owned() } else { conclusion });
            }
            let listed = (polls.is_multiple_of(JOBS_EVERY) || state.status != last)
                .then(|| jobs.get(sh).and_then(|body| serde_json::from_str::<Jobs>(body).ok()))
                .flatten();
            if let Some(Jobs { jobs }) = listed {
                if let Some((job, label)) = unservable(&jobs) {
                    let _cancelled = cmd!(sh, "gh run cancel {id}")
                        .quiet()
                        .ignore_stdout()
                        .ignore_stderr()
                        .run();
                    bail!(
                        "run {id}'s job `{job}` is queued for runner `{label}`, which no runner \
                         serves, so it would never start; the run is cancelled. Its workflow is \
                         not the one this xtask dispatches for: land the workflow change first, \
                         or try it with --workflow-ref <branch>"
                    );
                }
                if !ended_early && let Some(job) = first_red(&jobs) {
                    println!("  {job} failed: the run's other jobs are cancelled");
                    let _cancelled = cmd!(sh, "gh run cancel {id}")
                        .quiet()
                        .ignore_stdout()
                        .ignore_stderr()
                        .run();
                    ended_early = true;
                }
            }
            if state.status != last {
                let secs = started.elapsed().as_secs();
                println!("  {} ({}m{:02}s)", state.status, secs / 60, secs % 60);
                last = state.status;
            }
        }
        polls = polls.wrapping_add(1);
        ensure!(started.elapsed() < FINISH, "run {id} still {last} after {FINISH:?}");
        if pause(POLL, abandon) {
            let _cancelled =
                cmd!(sh, "gh run cancel {id}").quiet().ignore_stdout().ignore_stderr().run();
            return Ok(ABANDONED.to_owned());
        }
    }
}

/// How many polls of a run go by between two asks for its jobs: 20 s at a 5 s poll.
const JOBS_EVERY: u32 = 4;

/// A GitHub API resource asked for again with the `ETag` it was last read with
/// (`If-None-Match`): the API answers 304 when it did not change, which does not count against
/// the hourly limit (measured 2026-10-31: a 200 took one request of it, a 304 none).
struct Conditional {
    /// Its path, as `gh api` takes it.
    path: String,
    /// The `ETag` of its last answer.
    etag: Option<String>,
    /// Its body as last read.
    body: Option<String>,
}

impl Conditional {
    const fn new(path: String) -> Self {
        Self { path, etag: None, body: None }
    }

    /// The resource as it is now, read again only when it changed; `None` when the API did not
    /// answer with it.
    fn get(&mut self, sh: &Shell) -> Option<&str> {
        let header = self.etag.as_ref().map(|etag| format!("If-None-Match: {etag}"));
        let conditional: Vec<&str> =
            header.iter().flat_map(|header| ["-H", header.as_str()]).collect();
        let path = &self.path;
        // `gh` exits 1 on a 304 and prints its headers all the same.
        let out = cmd!(sh, "gh api --include {conditional...} {path}")
            .quiet()
            .ignore_status()
            .ignore_stderr()
            .output()
            .ok()?;
        let text = String::from_utf8(out.stdout).ok()?;
        let (status, etag, body) = answer(&text)?;
        match status {
            200 => self.body = Some(body.to_owned()),
            304 => {}
            _ => return None,
        }
        if let Some(etag) = etag {
            self.etag = Some(etag.to_owned());
        }
        self.body.as_deref()
    }
}

/// An HTTP answer as `gh api --include` prints it: the status, the `ETag` and the body.
fn answer(text: &str) -> Option<(u16, Option<&str>, &str)> {
    let (head, body) = text.split_once("\r\n\r\n").or_else(|| text.split_once("\n\n"))?;
    let mut lines = head.lines();
    let status = lines.next()?.split(' ').nth(1)?.parse().ok()?;
    let etag = lines.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("etag").then(|| value.trim())
    });
    Some((status, etag, body))
}

/// A job of a run, as the Actions API lists it.
#[derive(serde::Deserialize, Default)]
struct RunJob {
    name: String,
    status: String,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
}

/// The lint job, when it failed while another job of its run has not completed: the red that
/// ends the run ([`finish`]).
fn first_red(jobs: &[RunJob]) -> Option<&str> {
    let red = jobs
        .iter()
        .find(|j| j.name.starts_with("lint") && j.conclusion.as_deref() == Some("failure"))?;
    jobs.iter().any(|j| j.status != "completed").then_some(red.name.as_str())
}

/// The runner images a check's jobs run on: what `--runner` takes and `check.yml` names.
const RUNNERS: [&str; 3] = ["xcode-27", "ubuntu-24.04", "ubuntu-24.04-arm"];

/// A queued job asking for a runner no image serves, with the label it asks for.
fn unservable(jobs: &[RunJob]) -> Option<(String, String)> {
    jobs.iter().filter(|j| j.status == "queued").find_map(|job| {
        let label = job.labels.iter().find(|l| !RUNNERS.contains(&l.as_str()))?;
        Some((job.name.clone(), label.clone()))
    })
}

/// The run's artifact `name` unpacked into `into`, emptied first; `None` when the run left
/// none.
fn download(sh: &Shell, id: u64, name: &str, into: &Path) -> Option<PathBuf> {
    let id = id.to_string();
    if into.exists() && std::fs::remove_dir_all(into).is_err() {
        return None;
    }
    std::fs::create_dir_all(into).ok()?;
    cmd!(sh, "gh run download {id} --name {name} --dir {into}")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
        .ok()?;
    Some(into.to_path_buf())
}

/// The end of a failed log, without the build's progress lines.
fn show(path: &Path) -> Result<()> {
    let text = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = text.lines().filter(|l| !noise(l)).collect();
    let skip = lines.len().saturating_sub(SHOWN);
    let tail = lines.get(skip..).unwrap_or_default();
    println!("── {path} (last {} lines of {}) ──", tail.len(), lines.len());
    for line in tail {
        println!("{line}");
    }
    Ok(())
}

/// A progress line cargo prints while it builds, which says nothing about a failure.
fn noise(line: &str) -> bool {
    let line = line.trim_start();
    ["Compiling ", "Checking ", "Downloaded ", "Downloading ", "Blocking ", "Fresh ", "Building "]
        .iter()
        .any(|p| line.starts_with(p))
}

#[cfg(test)]
mod tests {
    use camino::{Utf8Path, Utf8PathBuf};
    use xshell::{Shell, cmd};

    use super::{MacJob, RUNNERS, RemoteOpts, mac_verdict, noise, snapshot, unaccepted, unfit};

    /// `remote`'s options as its command line gives them.
    fn opts(args: &[&str]) -> RemoteOpts {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(flatten)]
            opts: RemoteOpts,
        }
        let line = std::iter::once("remote").chain(args.iter().copied());
        <Cli as clap::Parser>::try_parse_from(line).expect("the options parse").opts
    }

    /// `--repeat` and `--until-fail` take an e2e case and at least one run, and not each other.
    #[test]
    fn a_repeat_takes_an_e2e_case_and_one_way_of_repeating() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(flatten)]
            opts: RemoteOpts,
        }
        let parses = |args: &[&str]| {
            <Cli as clap::Parser>::try_parse_from(
                std::iter::once("remote").chain(args.iter().copied()),
            )
            .is_ok()
        };
        assert_eq!(opts(&["--e2e", "ios", "--repeat", "20"]).repeat, Some(20));
        assert_eq!(opts(&["--e2e", "app", "--until-fail", "14"]).until_fail, Some(14));
        assert!(!parses(&["--repeat", "20"]));
        assert!(!parses(&["--e2e", "app", "--repeat", "0"]));
        assert!(!parses(&["--e2e", "app", "--repeat", "5", "--until-fail", "14"]));
    }

    /// A check asks only what the dispatched `check.yml` can run: this tree's runs a full check
    /// with no Mac tests, and one whose test job does not stand aside for `test_runner: none`, or
    /// that lacks an input sent, is refused before anything is dispatched.
    #[test]
    fn a_check_is_refused_by_a_workflow_that_cannot_run_it() {
        let ours = std::fs::read_to_string(
            crate::tools::repo_root().expect("root").join(".github/workflows/check.yml"),
        )
        .expect("check.yml");
        let mut full = opts(&["-p", "slopty-ui", "--full"]);
        full.tests_on = Some("none");
        assert_eq!(unfit(&ours, &full), None);
        let older = ours.replace("inputs.test_runner != 'none'", "true");
        let why = unfit(&older, &full).expect("refused");
        assert!(why.contains("test_runner: none"), "{why}");
        let probe = opts(&["--probe", "true"]);
        let without = ours.replace("      probe:\n", "      tried:\n");
        assert_eq!(unfit(&without, &probe).as_deref(), Some("it declares no input probe"));
        // A Linux-only lint goes to a workflow without the input as a whole one, not refused.
        let linux = super::runners_part(&opts(&["-p", "slopty-proto", "--full"]), false);
        assert!(super::fit(&ours, "gate", &linux).linux_only);
        let older = ours.replace("      lint:\n", "      linted:\n");
        let fitted = super::fit(&older, "gate", &linux);
        assert!(!fitted.linux_only && unfit(&older, &fitted).is_none());
    }

    /// An answer `gh api --include` prints gives its status, `ETag` and body, a 304's empty.
    #[test]
    fn an_answer_gives_its_status_etag_and_body() {
        let ok = "HTTP/2.0 200 OK\nAccess-Control-Allow-Origin: *\r\nEtag: W/\"5a29\"\r\n\
                  X-Ratelimit-Used: 469\r\n\r\n{\"status\":\"queued\"}";
        assert_eq!(super::answer(ok), Some((200, Some("W/\"5a29\""), "{\"status\":\"queued\"}")));
        let same = "HTTP/2.0 304 Not Modified\nEtag: \"5a29\"\r\nVary: Accept\r\n\r\n";
        assert_eq!(super::answer(same), Some((304, Some("\"5a29\""), "")));
        assert_eq!(super::answer("gh: not found"), None);
    }

    /// Every runner a workflow's job names outright is one a check knows is served, so a job
    /// queued for any other is one that would never start.
    #[test]
    fn every_runner_a_workflow_names_is_known() {
        let dir = crate::tools::repo_root().expect("root").join(".github/workflows");
        for entry in std::fs::read_dir(&dir).expect("workflows") {
            let path = entry.expect("an entry").path();
            let text = std::fs::read_to_string(&path).expect("a workflow");
            for label in text.lines().filter_map(|l| l.trim().strip_prefix("runs-on: ")) {
                if !label.starts_with("${{") {
                    assert!(RUNNERS.contains(&label), "{}: {label}", path.display());
                }
            }
        }
    }

    /// A shell in `root` whose git ignores the user's configuration (signing, hooks, branch).
    fn git_shell(root: &Utf8Path) -> Shell {
        let sh = Shell::new().expect("shell");
        sh.change_dir(root);
        sh.set_var("GIT_CONFIG_GLOBAL", "/dev/null");
        sh.set_var("GIT_CONFIG_NOSYSTEM", "1");
        for (key, value) in [
            ("GIT_AUTHOR_NAME", "test"),
            ("GIT_AUTHOR_EMAIL", "test@example.com"),
            ("GIT_COMMITTER_NAME", "test"),
            ("GIT_COMMITTER_EMAIL", "test@example.com"),
        ] {
            sh.set_var(key, value);
        }
        sh
    }

    fn scratch(name: &str) -> Utf8PathBuf {
        let dir = std::env::temp_dir().join(format!("xtask-remote-{name}-{}", std::process::id()));
        let _gone = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmp dir");
        Utf8PathBuf::from_path_buf(dir).expect("utf8 tmp dir")
    }

    fn write(root: &Utf8Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(path, text).expect("write");
    }

    /// Crate `name`'s manifest, depending on the crates `on` by path.
    fn manifest(name: &str, on: &[&str]) -> String {
        use std::fmt::Write as _;
        let deps = on.iter().fold(String::new(), |mut deps, d| {
            let _written = writeln!(deps, "{d} = {{ path = \"../{d}\" }}");
            deps
        });
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\n{deps}"
        )
    }

    /// The `dependencies` of package `name` in a lock file's text.
    fn locked_deps(lock: &str, name: &str) -> Vec<String> {
        let block = lock
            .split("[[package]]")
            .find(|block| block.contains(&format!("name = \"{name}\"\n")))
            .unwrap_or_else(|| panic!("{name} is in the lock:\n{lock}"));
        block
            .lines()
            .skip_while(|line| !line.starts_with("dependencies"))
            .skip(1)
            .take_while(|line| !line.starts_with(']'))
            .map(|line| line.trim().trim_matches(|c| c == '"' || c == ',').to_owned())
            .collect()
    }

    /// A snapshot of some paths holds the lock its own manifests make: a dependency a crate
    /// among the paths gained is in it, and one another lane's crate gained in the tree is not,
    /// though the tree's lock has both. With no manifest among the paths, it is `HEAD`'s lock.
    #[test]
    fn a_snapshot_s_lock_is_made_for_its_own_manifests() {
        let root = scratch("lock-repo");
        let dir = scratch("lock-check");
        let sh = git_shell(&root);
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        write(
            &root,
            "Cargo.toml",
            "[workspace]\nmembers = [\"a\", \"b\", \"c\", \"d\"]\nresolver = \"3\"\n",
        );
        for name in ["a", "b", "c", "d"] {
            write(&root, &format!("{name}/Cargo.toml"), &manifest(name, &[]));
            write(&root, &format!("{name}/src/lib.rs"), "");
        }
        let lock = |sh: &Shell| {
            cmd!(sh, "{cargo} metadata --offline --format-version 1").quiet().ignore_stdout().run()
        };
        lock(&sh).expect("the first lock");
        cmd!(sh, "git init -q").run().expect("init");
        cmd!(sh, "git add -A").run().expect("add");
        cmd!(sh, "git commit -q -m first").run().expect("commit");
        let head = cmd!(sh, "git show HEAD:Cargo.lock").read().expect("HEAD's lock");

        // This lane's crate `a` takes `c`; another lane's `b` takes `d`. The tree's lock has both.
        write(&root, "a/Cargo.toml", &manifest("a", &["c"]));
        write(&root, "b/Cargo.toml", &manifest("b", &["d"]));
        write(&root, "a/src/lib.rs", "//! a\n");
        lock(&sh).expect("the tree's lock");
        let tree = std::fs::read_to_string(root.join("Cargo.lock")).expect("tree lock");
        assert_eq!(locked_deps(&tree, "b"), ["d"], "the tree's lock has the other lane's");

        let only = |paths: &[&str]| -> String {
            let paths: Vec<String> = paths.iter().map(|p| (*p).to_owned()).collect();
            let sha = snapshot(&sh, &root, &dir, "test", &paths).expect("a snapshot");
            let spec = format!("{sha}:Cargo.lock");
            cmd!(sh, "git show {spec}").read().expect("its lock")
        };
        let mine = only(&["a"]);
        assert_eq!(locked_deps(&mine, "a"), ["c"], "its own crate's new dependency");
        assert!(locked_deps(&mine, "b").is_empty(), "not the other lane's: {mine}");
        assert_eq!(only(&["a/src"]), head, "no manifest among the paths: HEAD's lock");
        let named = only(&["a", "Cargo.lock"]);
        assert!(locked_deps(&named, "b").is_empty(), "the tree's lock, made for the snapshot");
        assert_eq!(std::fs::read_to_string(root.join("Cargo.lock")).expect("lock"), tree, "kept");
        let _gone = std::fs::remove_dir_all(&root);
        let _gone = std::fs::remove_dir_all(&dir);
    }

    fn check(name: &str, queued: bool) -> MacJob {
        MacJob { gate: false, queued, check: Some(name.to_owned()) }
    }

    fn gate(queued: bool) -> MacJob {
        MacJob { gate: true, queued, check: None }
    }

    /// A check that moves the lock works in a directory of its own; any other in the directory
    /// its name shares.
    #[test]
    fn a_check_that_moves_the_lock_works_apart() {
        let place = |name: &str, paths: &[&str]| {
            let changed: Vec<String> = paths.iter().map(|p| (*p).to_owned()).collect();
            let place = super::Workdir::of(name, &changed);
            (place.name, place.moves_lock)
        };
        assert_eq!(place("L-h", &["crates/slopty-shell/src/lib.rs"]), ("L".to_owned(), false));
        assert_eq!(place("L-h", &[]), ("L".to_owned(), false));
        for moved in [
            "Cargo.lock",
            "Cargo.toml",
            "workspace-hack/Cargo.toml",
            "vendor/ghostty",
            "vendor/noq-udp/src/unix.rs",
            ".cargo/config.toml",
            "rust-toolchain.toml",
        ] {
            let moving = place("L-h", &["crates/a/src/lib.rs", moved]);
            assert_eq!(moving, ("L-h-lock".to_owned(), true));
        }
        assert_eq!(place("U", &["crates/slopty-net/Cargo.toml"]), ("U".to_owned(), false));
        assert_eq!(place("U", &["Cargo.lock"]), ("U-lock".to_owned(), true));
    }

    /// A quick check dispatches nothing of its crates; a full one only Linux's clippy, and the
    /// tests when Linux tests every crate; none for crates no Linux builds.
    #[test]
    fn a_full_check_asks_a_runner_only_for_linux() {
        let quick = super::runners_part(&opts(&["-p", "slopty-net"]), false);
        assert_eq!(quick.packages, [""; 0], "a quick check's crates are checked here");
        let mac = super::runners_part(&opts(&["-p", "slopty-shell", "--full"]), false);
        assert!(mac.packages.is_empty(), "iOS clippy runs here");
        let net = super::runners_part(&opts(&["-p", "slopty-proto", "--full"]), false);
        assert!(net.linux_only && net.packages == ["slopty-proto"]);
        assert_eq!(super::tests_on(&net), "ubuntu-24.04");
        assert!(super::dispatched(&net, "").contains(&("lint", "linux".to_owned())));
        let mixed = super::runners_part(
            &opts(&["-p", "slopty-proto", "-p", "slopty-shell", "--full"]),
            false,
        );
        assert_eq!(super::tests_on(&mixed), "none");
        assert!(mixed.linux_only && mixed.packages == ["slopty-proto"], "Linux builds the one");
        let placed = opts(&["-p", "slopty-proto", "--full", "--place", "runner"]);
        assert!(!super::dispatched(&placed, "").iter().any(|(key, _)| *key == "lint"));
    }

    /// A check runs the gate's hakari step when it changes a member's manifest, the lock, the
    /// hack or hakari's configuration, and its deny step for those and deny's configuration; not
    /// for the fuzz workspace's or anything else.
    #[test]
    fn a_change_reaches_the_gate_s_tools_that_read_it() {
        use crate::gate::Tool;
        let reached =
            |paths: &[&str]| super::reached_tools(paths, &["crates/slopty-net/".to_owned()]);
        // U's crate refresh: the lock moved `tempfile` and with it the hack.
        let lock = reached(&["Cargo.lock"]);
        for tool in [Tool::Locked, Tool::Deny, Tool::Hakari, Tool::Shear, Tool::Invariants] {
            assert!(lock.contains(&tool), "{tool:?}: {lock:?}");
        }
        assert!(!lock.contains(&Tool::Rustfmt) && !lock.contains(&Tool::Taplo), "{lock:?}");
        // N's re-vendored noq: `cargo fmt --all` reaches the path dependency.
        let vendored = reached(&["vendor/noq-udp/src/unix.rs", "crates/slopty-net/src/lib.rs"]);
        assert!(vendored.contains(&Tool::Rustfmt) && vendored.contains(&Tool::Shear));
        // A change inside the checked crates is formatted by their own fmt.
        let own = reached(&["crates/slopty-net/src/lib.rs"]);
        assert!(!own.contains(&Tool::Rustfmt) && !own.contains(&Tool::Typos), "{own:?}");
        assert!(own.contains(&Tool::Invariants) && !own.contains(&Tool::Hakari), "{own:?}");
        let hack = reached(&["workspace-hack/Cargo.toml"]);
        assert!(hack.contains(&Tool::Hakari) && hack.contains(&Tool::Taplo), "{hack:?}");
        assert!(reached(&["deny.toml"]).contains(&Tool::Deny));
        assert!(!reached(&["fuzz/Cargo.lock"]).contains(&Tool::Hakari));
        // A manifest's edition sets how its crate formats: the whole tree's fmt for another
        // crate's, the checked crate's own fmt for its own.
        assert!(reached(&["crates/slopty-proto/Cargo.toml"]).contains(&Tool::Rustfmt));
        assert!(!reached(&["crates/slopty-net/Cargo.toml"]).contains(&Tool::Rustfmt));
        assert_eq!(reached(&["docs/DEV.md"]), [Tool::Typos]);
        assert_eq!(reached(&[]), []);
    }

    /// A run's title gives back the check's name and commit; another title gives nothing.
    #[test]
    fn a_check_s_run_title_names_its_branch_and_commit() {
        let sha = "fda027eb2144c1e6b6dbb5d1c27f7e6fa0b1c2d3";
        assert_eq!(super::checked(&format!("check W-v {sha}")), Some(("W-v", sha)));
        assert_eq!(super::checked("check W-v fda027eb21"), None, "a short sha");
        assert_eq!(super::checked(&format!("gate {sha}")), None);
        assert_eq!(super::checked(&format!("check W-v {sha} more")), None);
    }

    /// A quick check of crates Linux tests runs there; a full one takes a Mac for a crate a Mac
    /// shard tests too, all of it or some binaries, and stays on Linux for one Linux alone tests.
    #[test]
    fn a_full_check_runs_the_mac_s_own_tests_on_a_mac() {
        let of = |names: &[&str], full| {
            let packages: Vec<String> = names.iter().map(|&n| n.to_owned()).collect();
            super::test_runner(&packages, full)
        };
        assert_eq!(of(&["slopty-client"], false), "ubuntu-24.04");
        assert_eq!(of(&["slopty-client"], true), "xcode-27");
        assert_eq!(of(&["slopty-serverd"], true), "xcode-27");
        assert_eq!(of(&["slopty-proto", "slopty-server"], true), "ubuntu-24.04");
        assert_eq!(of(&["slopty-ui"], false), "xcode-27");
    }

    #[test]
    fn a_check_yields_its_mac_to_the_gate() {
        // Free: nothing in flight, or the gate's shards all running.
        assert_eq!(mac_verdict(&[], "A-cuts", 2), None);
        assert_eq!(mac_verdict(&[gate(false), gate(false), gate(false)], "A-cuts", 1), None);
        // The gate waits for a runner: every check waits for it.
        let why = mac_verdict(&[gate(true), gate(false)], "A-cuts", 1);
        assert!(why.is_some_and(|w| w.contains("gate")));
        // Two macOS jobs outside the gate at most, this check's counted.
        assert_eq!(mac_verdict(&[check("W-stream", false)], "A-cuts", 1), None);
        assert!(mac_verdict(&[check("W-stream", false)], "A-cuts", 2).is_some());
        let two = [check("W-stream", false), check("C-ab", true)];
        assert!(mac_verdict(&two, "A-cuts", 1).is_some());
        // An e2e run of its own workflow counts as outside the gate.
        let e2e = MacJob { gate: false, queued: false, check: None };
        assert!(mac_verdict(&[e2e, check("W-stream", false)], "A-cuts", 1).is_some());
    }

    #[test]
    fn a_workflow_s_local_actions_are_read_from_its_steps() {
        let workflow = "steps:\n      - uses: actions/checkout@v5\n      - uses: ./.github/actions/sccache\n        with:\n          x: 1\n      - if: a\n        uses: ./.github/actions/zig\n      - uses: ./.github/actions/sccache\n";
        assert_eq!(
            super::local_actions(workflow),
            [".github/actions/sccache", ".github/actions/zig"]
        );
    }

    #[test]
    fn another_workflows_queued_macos_jobs_hold_no_mac() {
        // The Deep workflow running one macOS job and listing its other nine as queued.
        let running = MacJob { gate: false, queued: false, check: None };
        let mut deep: Vec<MacJob> =
            std::iter::repeat_with(|| MacJob { gate: false, queued: true, check: None })
                .take(9)
                .collect();
        deep.push(running);
        assert_eq!(mac_verdict(&deep, "A-cuts", 1), None);
        // Its one running job and a check queued for a runner are two: a third waits.
        deep.push(check("W-stream", true));
        assert!(mac_verdict(&deep, "A-cuts", 1).is_some());
    }

    #[test]
    fn a_rerun_replaces_its_own_mac_check() {
        // Another check of a name sharing the directory is another lane's: one Mac of two.
        assert_eq!(mac_verdict(&[check("A-perf", false)], "A-cuts", 1), None);
        // The same name does not wait: its dispatch cancels the older run, whose Macs come
        // back, so they are not counted against it either.
        let own = [check("A-cuts", false), check("A-cuts", false), check("W-x", false)];
        assert_eq!(mac_verdict(&own, "A-cuts", 1), None);
    }

    /// A failed lint job ends its run while another job still runs; a failed test job, a lint
    /// job still running, or a run all done ends nothing.
    #[test]
    fn a_failed_lint_job_ends_its_run() {
        let job = |name: &str, status: &str, conclusion: Option<&str>| super::RunJob {
            name: name.to_owned(),
            status: status.to_owned(),
            conclusion: conclusion.map(str::to_owned),
            labels: vec!["xcode-27".to_owned()],
        };
        let red = job("lint (A)", "completed", Some("failure"));
        let running = job("run (A)", "in_progress", None);
        assert_eq!(super::first_red(&[red, running]), Some("lint (A)"));
        let red = job("lint (A)", "completed", Some("failure"));
        let done = job("run (A)", "completed", Some("success"));
        assert_eq!(super::first_red(&[red, done]), None, "nothing left to cancel");
        let tests = job("test (A)", "completed", Some("failure"));
        let running = job("run (A)", "in_progress", None);
        assert_eq!(super::first_red(&[tests, running]), None, "an e2e run outlives a test's red");
        let lint = job("lint (A)", "in_progress", None);
        let running = job("run (A)", "in_progress", None);
        assert_eq!(super::first_red(&[lint, running]), None);
    }

    /// An abandoned check stops its pause at once; one left alone sleeps it out.
    #[test]
    fn a_pause_ends_when_the_check_is_abandoned() {
        let abandon = super::Abandon::new(true);
        let started = std::time::Instant::now();
        assert!(super::pause(std::time::Duration::from_secs(30), Some(&abandon)));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert!(!super::pause(std::time::Duration::from_millis(10), None));
    }

    /// The tests the status lines name as failed, each once; a retry's red, a slow test and
    /// the tests' own output are not taken.
    #[test]
    fn a_rerun_takes_the_tests_the_last_summaries_failed() {
        let log = "\
        FAIL [  31.275s] ( 9/22) slopty-e2e::app interface::open_in_hands_the_folder_to_an_editor
     Summary [ 314.916s] 22 tests run: 19 passed (2 slow), 3 failed, 1 skipped
        SLOW [> 30.000s] (─────) slopty-e2e::app agent_ui::the_window
        FAIL [  31.275s] ( 9/22) slopty-e2e::app interface::open_in_hands_the_folder_to_an_editor
     TIMEOUT [ 120.000s] (19/22) slopty-e2e::app surfaces_live::the_terminal_s_floats_wear_the_shell
  ✘ slopty-e2e app not (test(~screen_recording::)) (315.3s)
  stdout ───
    test interface::open_in_hands_the_folder_to_an_editor ... FAILED
    TRY 1 FAIL [   1.000s] ( 1/1) slopty-e2e::app retried::and_passed
     Summary [   0.001s] 1 test run: 0 passed, 1 failed
     SIGABRT [   2.000s] ( 1/1) slopty-e2e::app streams::a_window_streams
";
        assert_eq!(
            super::failed_tests(log),
            [
                "interface::open_in_hands_the_folder_to_an_editor",
                "surfaces_live::the_terminal_s_floats_wear_the_shell",
                "streams::a_window_streams",
            ]
        );
        let coloured = "\u{1b}[31;1m     Summary\u{1b}[0m [ 1.0s] 1 test run\n\
                        \u{1b}[31;1m        FAIL\u{1b}[0m [ 1.0s] (1/1) slopty-e2e::app a::b\n";
        assert_eq!(super::failed_tests(coloured), ["a::b"]);
        let none: [&str; 0] = [];
        assert_eq!(super::failed_tests("     Summary [ 1s] 3 tests run: 3 passed"), none);
    }

    #[test]
    fn progress_lines_are_noise_and_diagnostics_are_not() {
        assert!(noise("   Compiling slopty-ui v0.1.0"));
        assert!(noise("    Checking serde v1.0.0"));
        assert!(!noise("error[E0308]: mismatched types"));
        assert!(!noise("        FAIL [   0.1s] slopty-ui kit::tests::x"));
    }

    #[test]
    fn an_accept_patch_may_touch_only_goldens_and_snapshots() {
        let numstat = "-\t-\tcrates/slopty-e2e/golden/ipad/home.png\n\
                       3\t1\tcrates/slopty-proto/tests/snapshots/golden__hello.snap\n\
                       -\t-\t.zig-cache/o/1f/libc.a\n\
                       2\t0\tCargo.lock\n";
        assert_eq!(unaccepted(numstat), [".zig-cache/o/1f/libc.a", "Cargo.lock"]);
        assert_eq!(unaccepted(""), [""; 0]);
    }
}
