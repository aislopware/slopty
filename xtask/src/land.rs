//! `xtask land`: hand the commits on `main` to CI, which moves `main` to them once every gate
//! lane passed there (`.github/workflows/ci.yml`, its `promote` job).
//!
//! The `gate` branch is always `origin/main` plus what is landing, so each land replaces what the
//! last one left, with a lease, so a push this checkout has not seen is never dropped. Each push
//! runs at once, beside any run still going; main moves to whichever green comes, and a later
//! one carries every commit under it. Only CI's `promote` moves main, with the repository's
//! deploy key, workflow changes included; should that key ever break, a plain `git push` of the
//! green commit to main is the way out.
//!
//! It pushes at once: nothing compiles on this Mac, whose cores and disk are the person's
//! (`docs/decisions/tooling.md`, "Every compile runs on a runner"). A change likely to go red is
//! checked first on a runner with `cargo xtask remote`. `--check` still lints the changed
//! packages and their dependents here under `nice` (host and iOS clippy and rustdoc,
//! [`crate::gate::land_checks`]), and `--full` adds their tests and Linux clippy.

use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use xshell::{Shell, cmd};

/// The branch CI gates.
const BRANCH: &str = "gate";

/// The workflow that gates it.
const WORKFLOW: &str = "ci.yml";

/// How often `--wait` asks GitHub about the run.
const POLL: Duration = Duration::from_secs(20);

/// The longest `--wait` waits for the run to show up after the push.
const FIND: Duration = Duration::from_secs(120);

/// The longest `--wait` waits for the run to finish: a cold gate takes about an hour.
const FINISH: Duration = Duration::from_hours(3);

/// What GitHub reads in a pushed commit's message as "start no workflow" (its "Skipping workflow
/// runs"), compared without case. A land saying one would leave `gate` on a commit no run
/// gates, older runs standing aside for it, and main never moving.
const SKIPS: [&str; 7] = [
    "[skip ci]",
    "[ci skip]",
    "[no ci]",
    "[skip actions]",
    "[actions skip]",
    "skip-checks:true",
    "skip-checks: true",
];

/// The first of [`SKIPS`] that `messages` say.
fn skips_ci(messages: &str) -> Option<&'static str> {
    let messages = messages.to_lowercase();
    SKIPS.into_iter().find(|skip| messages.contains(skip))
}

/// `cargo xtask land` options.
#[derive(clap::Args, Debug)]
pub struct LandOpts {
    /// Block until CI promoted the commit to main, or failed, and report which.
    #[arg(long)]
    pub wait: bool,
    /// Lint the changed packages here before the push, which compiles on this Mac.
    #[arg(long)]
    pub check: bool,
    /// With `--check`: also run the changed packages' tests and Linux clippy.
    #[arg(long, requires = "check")]
    pub full: bool,
}

pub fn run(sh: &Shell, opts: &LandOpts) -> Result<()> {
    let branch = cmd!(sh, "git symbolic-ref --quiet --short HEAD")
        .read()
        .context("HEAD is detached; land from main")?;
    ensure!(branch == "main", "land commits on main; HEAD is on {branch}");
    cmd!(sh, "git fetch --quiet origin main").run()?;
    // The branch is missing until the first land; then the lease is that it still is.
    let _gate =
        cmd!(sh, "git fetch --quiet origin +refs/heads/{BRANCH}:refs/remotes/origin/{BRANCH}")
            .quiet()
            .ignore_stderr()
            .run();
    let head = cmd!(sh, "git rev-parse HEAD").read()?;
    let main = cmd!(sh, "git rev-parse origin/main").read()?;
    ensure!(head != main, "nothing to land: HEAD is origin/main");
    let on_main = cmd!(sh, "git merge-base --is-ancestor origin/main HEAD").quiet().run().is_ok();
    ensure!(
        on_main,
        "HEAD is not on top of origin/main, and main only fast-forwards: `git rebase origin/main` \
         (then the quick gate again) and land"
    );
    // The gate checks the one message it is given, but a gated batch split into several commits
    // writes the others after it, and CI's tools lane then failed on one (run 37194759190).
    cmd!(sh, "committed origin/main..HEAD --no-merge-commit")
        .run()
        .context("a commit to land breaks Conventional Commits: reword it, then land")?;
    let messages = cmd!(sh, "git log --format=%B origin/main..HEAD").read()?;
    if let Some(skip) = skips_ci(&messages) {
        bail!(
            "a commit to land says `{skip}`, so GitHub would start no gate on it and main would \
             never take it: reword it, then land"
        );
    }
    let leased = cmd!(sh, "git rev-parse --verify --quiet refs/remotes/origin/{BRANCH}")
        .quiet()
        .read()
        .unwrap_or_default();
    if opts.check {
        // What `gate` already holds is CI's to check, so a land on top of one still running is
        // checked for its own commits alone: from main, a lockfile change in the earlier land
        // left every later one unchecked.
        let pushed = !leased.is_empty()
            && cmd!(sh, "git merge-base --is-ancestor origin/main {leased}").quiet().run().is_ok()
            && cmd!(sh, "git merge-base --is-ancestor {leased} HEAD").quiet().run().is_ok();
        crate::gate::land_checks(if pushed { &leased } else { "origin/main" }, opts.full)?;
    } else {
        println!("  no checks before the push: CI alone decides");
    }
    let lease = format!("--force-with-lease=refs/heads/{BRANCH}:{leased}");
    cmd!(sh, "git push --quiet {lease} origin HEAD:refs/heads/{BRANCH}").run()?;
    let short = head.get(..10).unwrap_or(&head);
    let commits = cmd!(sh, "git rev-list --count origin/main..HEAD").read()?;
    println!("▶ {short} ({commits} commits on top of origin/main) pushed to {BRANCH}");
    println!("  CI runs every gate lane on it and fast-forwards main once all pass");
    // Checks stopped before their runs ended leave their branches behind.
    if let Err(e) = crate::remote::sweep(sh) {
        println!("  check branches not swept: {e:#}");
    }
    if !opts.wait {
        println!("watch: gh run list --workflow {WORKFLOW} --branch {BRANCH} --commit {head}");
        println!("       then `gh run watch <id> --exit-status`, or land with --wait");
        return Ok(());
    }
    let mut run = find_run(sh, &head)?;
    loop {
        println!("  run {}: {}", run.id, run.url);
        let conclusion = finish(sh, run.id)?;
        match outcome(sh, &run, &head)? {
            Next::Held(main) if main == head => {
                println!("✔ main is at {head}");
                return Ok(());
            }
            Next::Held(main) => {
                println!("✔ main holds {head}, and is at {main}");
                return Ok(());
            }
            Next::Red(jobs) => {
                for job in &jobs {
                    eprintln!("✘ {job}");
                }
                bail!(
                    "the gate {conclusion} on {}; main stays. The run's summary names each failed \
                     lane and test: {}\n  its failed steps' logs: gh run view {} --log-failed",
                    run.sha,
                    run.url,
                    run.id
                )
            }
            Next::Follow(tip) => {
                println!(
                    "  run {} {conclusion} without a red, and {BRANCH} moved on to {tip}, which \
                     carries {head}: following its run",
                    run.id
                );
                run = find_run(sh, &tip)?;
            }
            Next::Withdrawn(tip) => bail!(
                "run {} {conclusion}, and {BRANCH} ({tip}) no longer holds {head}: a later land \
                 withdrew it, so main never takes it: {}",
                run.id,
                run.url
            ),
            Next::Ended => bail!(
                "run {} {conclusion} without a red, and main does not hold {head}: someone \
                 cancelled it, or its promote stopped; `gh run rerun {} --failed` runs it again: \
                 {}",
                run.id,
                run.id,
                run.url
            ),
        }
    }
}

/// A workflow run as `gh run list` describes it.
#[derive(serde::Deserialize)]
struct Run {
    #[serde(rename = "databaseId")]
    id: u64,
    url: String,
    /// The commit it gates: the landed one, or one on top of it that a later land pushed.
    #[serde(rename = "headSha")]
    sha: String,
}

/// The gate's run on `head`, once GitHub lists it.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling GitHub")]
fn find_run(sh: &Shell, head: &str) -> Result<Run> {
    let started = Instant::now();
    loop {
        let listed = cmd!(
            sh,
            "gh run list --workflow {WORKFLOW} --branch {BRANCH} --commit {head} --limit 1 --json databaseId,url,headSha"
        )
        .quiet()
        .read()
        .context("gh run list (is `gh` installed and logged in?)")?;
        let runs: Vec<Run> = serde_json::from_str(&listed).context("gh run list's JSON")?;
        if let Some(run) = runs.into_iter().next() {
            return Ok(run);
        }
        if started.elapsed() >= FIND {
            bail!("no {WORKFLOW} run on {BRANCH} for {head} after {FIND:?}; is the workflow on?");
        }
        std::thread::sleep(POLL);
    }
}

/// The run's conclusion once it completes, printing each change of its status meanwhile.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling GitHub")]
fn finish(sh: &Shell, id: u64) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct State {
        status: String,
        conclusion: String,
    }
    let id = id.to_string();
    let started = Instant::now();
    let mut last = String::new();
    loop {
        let state = cmd!(sh, "gh run view {id} --json status,conclusion").quiet().read()?;
        let state: State = serde_json::from_str(&state).context("gh run view's JSON")?;
        if state.status == "completed" {
            return Ok(state.conclusion);
        }
        if state.status != last {
            println!("  {} ({:.0?})", state.status, started.elapsed());
            last = state.status;
        }
        ensure!(started.elapsed() < FINISH, "run {id} still {last} after {FINISH:?}");
        std::thread::sleep(POLL);
    }
}

/// What a completed run says of the landed `head`, and where `--wait` goes next.
#[derive(Debug, PartialEq, Eq)]
pub enum Next {
    /// Main holds it, and is at this commit: at it, or past it when a later push's green
    /// promoted first.
    Held(String),
    /// The run went red: these jobs failed or ran past their timeout.
    Red(Vec<String>),
    /// The run ended without a red and without main taking `head` (it stood aside before its
    /// e2e, `gate` having moved on: `crate::gate::order`), and `gate` moved on to this commit on
    /// top of it, whose run carries it.
    Follow(String),
    /// `gate` (at this commit) no longer holds `head`: a later land replaced it.
    Withdrawn(String),
    /// Neither: cancelled by hand, or a promote that stopped.
    Ended,
}

/// Ask git and GitHub where the run on `head` left it ([`decide`]).
fn outcome(sh: &Shell, run: &Run, head: &str) -> Result<Next> {
    cmd!(sh, "git fetch --quiet origin main").run()?;
    let main = cmd!(sh, "git rev-parse origin/main").read()?;
    let holds = cmd!(sh, "git merge-base --is-ancestor {head} {main}").quiet().run().is_ok();
    let id = run.id.to_string();
    let jobs = cmd!(sh, "gh run view {id} --json jobs").quiet().read().unwrap_or_default();
    let red = red_jobs(&jobs, |job| crate::gate::order::timed_out(sh, "repos/{owner}/{repo}", job));
    let _gate =
        cmd!(sh, "git fetch --quiet origin +refs/heads/{BRANCH}:refs/remotes/origin/{BRANCH}")
            .quiet()
            .ignore_stderr()
            .run();
    let tip = cmd!(sh, "git rev-parse --verify --quiet refs/remotes/origin/{BRANCH}")
        .quiet()
        .read()
        .unwrap_or_default();
    let carries = !tip.is_empty()
        && cmd!(sh, "git merge-base --is-ancestor {head} {tip}").quiet().run().is_ok();
    Ok(decide(Gate { main, holds, red, tip, carries }, &run.sha))
}

/// The red jobs in `gh run view --json jobs`'s `listing`: each that failed, or ran past its
/// timeout, which GitHub lists as cancelled and `timed_out` tells apart by its job's id.
fn red_jobs(listing: &str, timed_out: impl Fn(u64) -> bool) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct Listing {
        jobs: Vec<Job>,
    }
    #[derive(serde::Deserialize)]
    struct Job {
        #[serde(rename = "databaseId")]
        id: u64,
        name: String,
        conclusion: String,
    }
    let Ok(listing) = serde_json::from_str::<Listing>(listing) else { return Vec::new() };
    listing
        .jobs
        .into_iter()
        .filter_map(|job| {
            let late = std::cell::Cell::new(false);
            let end = crate::gate::order::end(Some(&job.conclusion), || {
                late.set(timed_out(job.id));
                late.get()
            });
            (end == crate::gate::order::End::Red).then(|| {
                if late.get() { format!("{}, past its timeout", job.name) } else { job.name }
            })
        })
        .collect()
}

/// What [`outcome`] found once the run on `sha` ended.
pub struct Gate {
    /// Main's head, and whether it holds the landed commit.
    pub main: String,
    pub holds: bool,
    /// The run's jobs that failed or ran past their timeout.
    pub red: Vec<String>,
    /// `gate`'s head, and whether it holds the landed commit.
    pub tip: String,
    pub carries: bool,
}

/// Where the run on `sha` left the landed commit: held by main, red, carried by a newer run on
/// `gate` to follow, withdrawn from `gate`, or none of these. `crate::gate::order`'s model
/// holds it to every commit held or withdrawn once the runs end green, in any order.
pub fn decide(gate: Gate, sha: &str) -> Next {
    if gate.holds {
        Next::Held(gate.main)
    } else if !gate.red.is_empty() {
        Next::Red(gate.red)
    } else if !gate.carries {
        Next::Withdrawn(gate.tip)
    } else if gate.tip != sha {
        Next::Follow(gate.tip)
    } else {
        Next::Ended
    }
}

#[cfg(test)]
mod tests {
    use super::{Gate, Next, decide, red_jobs, skips_ci};

    /// A message GitHub would start no workflow for is refused, in any case, and only that.
    #[test]
    fn a_commit_that_skips_ci_is_refused() {
        assert_eq!(skips_ci("fix(ci): a thing [Skip CI]"), Some("[skip ci]"));
        assert_eq!(skips_ci("docs: x\n\n\nskip-checks: true"), Some("skip-checks: true"));
        assert_eq!(skips_ci("ci: skip the cache when it is cold"), None);
    }

    /// A run's red jobs are those that failed or ran past their timeout, which GitHub lists as
    /// cancelled; a job cancelled with its run is not one.
    #[test]
    fn a_job_past_its_timeout_is_red_and_a_cancelled_one_is_not() {
        let listing = r#"{"jobs": [
            {"databaseId": 1, "name": "gate (ui)", "conclusion": "success"},
            {"databaseId": 2, "name": "gate (rest)", "conclusion": "failure"},
            {"databaseId": 3, "name": "gate (worker)", "conclusion": "cancelled"},
            {"databaseId": 4, "name": "e2e (mac)", "conclusion": "cancelled"},
            {"databaseId": 5, "name": "promote", "conclusion": "skipped"},
            {"databaseId": 6, "name": "e2e (ios)", "conclusion": ""}
        ]}"#;
        assert_eq!(
            red_jobs(listing, |job| job == 3),
            ["gate (rest)", "gate (worker), past its timeout"]
        );
        assert_eq!(red_jobs("not json", |_| true), Vec::<String>::new());
    }

    /// `--wait` reports main holding the commit first, whatever the run said; then a red; and
    /// follows a newer run on `gate` only when the last ended without one, so a run that stood
    /// aside before its e2e ends in that run's verdict.
    #[test]
    fn a_run_without_a_red_hands_its_commit_to_the_newer_run_carrying_it() {
        let gate = |holds, red: &[&str], tip: &str, carries| Gate {
            main: "m".to_owned(),
            holds,
            red: red.iter().map(|job| (*job).to_owned()).collect(),
            tip: tip.to_owned(),
            carries,
        };
        assert_eq!(decide(gate(true, &["gate (ui)"], "a", true), "a"), Next::Held("m".to_owned()));
        assert_eq!(
            decide(gate(false, &["gate (ui)"], "b", true), "a"),
            Next::Red(vec!["gate (ui)".to_owned()]),
            "a red is the land's to fix, though a newer run carries it"
        );
        assert_eq!(decide(gate(false, &[], "b", true), "a"), Next::Follow("b".to_owned()));
        assert_eq!(decide(gate(false, &[], "c", false), "a"), Next::Withdrawn("c".to_owned()));
        assert_eq!(decide(gate(false, &[], "", false), "a"), Next::Withdrawn(String::new()));
        assert_eq!(decide(gate(false, &[], "a", true), "a"), Next::Ended, "nothing newer runs");
    }
}
