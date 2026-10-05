//! `xtask land`: hand the commits on `main` to CI, which moves `main` to them once every gate
//! lane passed there (`.github/workflows/ci.yml`, its `promote` job).
//!
//! The `gate` branch is always `origin/main` plus what is landing, so each land replaces what the
//! last one left, with a lease, so a push this checkout has not seen is never dropped. CI runs one
//! gate at a time on that branch; a newer push waits behind the run in progress, and only the
//! newest waits, since its green covers every commit under it.
//!
//! Before the push, the tests, rustdoc and the iOS and Linux clippy of the packages the commits
//! change, and of their dependents, run here ([`crate::gate::land_checks`]): a red run on CI
//! costs the better part of an hour, most of them a test or a lint a few minutes here fail.

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

/// `cargo xtask land` options.
#[derive(clap::Args, Debug)]
pub struct LandOpts {
    /// Block until CI promoted the commit to main, or failed, and report which.
    #[arg(long)]
    pub wait: bool,
    /// Push without first checking the packages the commits change (tests, rustdoc, iOS and
    /// Linux clippy).
    #[arg(long)]
    pub no_tests: bool,
}

/// A run as `promote` reads it from `gh run list`.
#[derive(serde::Deserialize)]
struct Listed {
    #[serde(rename = "databaseId")]
    id: u64,
    url: String,
    status: String,
}

/// `cargo xtask promote` options.
#[derive(clap::Args, Debug)]
pub struct PromoteOpts {
    /// The commit to move main to; the `gate` branch's head when left out.
    pub commit: Option<String>,
}

/// Move main to a commit every gate lane passed on, from this checkout. CI's `promote` job does
/// this with the run's own token, which GitHub never lets update a workflow file: a commit that
/// changes `.github/workflows/` is refused there however green (runs 37254289819, 37255378937).
/// The same checks as that job: the run on the commit is complete, every `gate` job in it
/// passed, and main fast-forwards to the commit.
pub fn promote(sh: &Shell, opts: &PromoteOpts) -> Result<()> {
    cmd!(sh, "git fetch --quiet origin main").run()?;
    cmd!(sh, "git fetch --quiet origin +refs/heads/{BRANCH}:refs/remotes/origin/{BRANCH}").run()?;
    let wanted = opts.commit.clone().unwrap_or_else(|| format!("origin/{BRANCH}"));
    let wanted = format!("{wanted}^{{commit}}");
    let head = cmd!(sh, "git rev-parse --verify {wanted}").read()?;
    let main = cmd!(sh, "git rev-parse origin/main").read()?;
    if main == head {
        println!("✔ main is already at {head}");
        return Ok(());
    }
    let listed = cmd!(
        sh,
        "gh run list --workflow {WORKFLOW} --branch {BRANCH} --commit {head} --limit 1 --json databaseId,url,status"
    )
    .quiet()
    .read()
    .context("gh run list (is `gh` installed and logged in?)")?;
    let runs: Vec<Listed> = serde_json::from_str(&listed).context("gh run list's JSON")?;
    let Some(run) = runs.into_iter().next() else {
        bail!("no {WORKFLOW} run on {BRANCH} for {head}: land it first");
    };
    ensure!(run.status == "completed", "the gate on {head} is still {}: {}", run.status, run.url);
    ensure!(gate_passed(sh, run.id)?, "not every gate lane passed on {head}: {}", run.url);
    let on_main = cmd!(sh, "git merge-base --is-ancestor {main} {head}").quiet().run().is_ok();
    ensure!(on_main, "{head} is not on top of origin/main ({main}); main only fast-forwards");
    cmd!(sh, "git push --quiet origin {head}:refs/heads/main").run()?;
    println!("✔ main fast-forwarded to {head}, which every gate lane passed in {}", run.url);
    Ok(())
}

/// Whether the run has `gate` jobs and every one of them passed; `promote` and `release` are not
/// lanes.
fn gate_passed(sh: &Shell, id: u64) -> Result<bool> {
    let id = id.to_string();
    let jobs = cmd!(sh, "gh run view {id} --json jobs --jq .jobs[]|[.name,.conclusion]|@tsv")
        .quiet()
        .read()?;
    let lanes: Vec<(&str, &str)> = jobs
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .filter(|(name, _)| name.starts_with("gate"))
        .collect();
    Ok(!lanes.is_empty() && lanes.iter().all(|(_, conclusion)| *conclusion == "success"))
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
    let leased = cmd!(sh, "git rev-parse --verify --quiet refs/remotes/origin/{BRANCH}")
        .quiet()
        .read()
        .unwrap_or_default();
    if opts.no_tests {
        println!("! pushed without the checks before the push: CI is the first to build this");
    } else {
        // What `gate` already holds is CI's to check, so a land on top of one still running is
        // checked for its own commits alone: from main, a lockfile change in the earlier land
        // left every later one unchecked.
        let pushed = !leased.is_empty()
            && cmd!(sh, "git merge-base --is-ancestor origin/main {leased}").quiet().run().is_ok()
            && cmd!(sh, "git merge-base --is-ancestor {leased} HEAD").quiet().run().is_ok();
        crate::gate::land_checks(if pushed { &leased } else { "origin/main" })?;
    }
    let lease = format!("--force-with-lease=refs/heads/{BRANCH}:{leased}");
    cmd!(sh, "git push --quiet {lease} origin HEAD:refs/heads/{BRANCH}").run()?;
    let short = head.get(..10).unwrap_or(&head);
    let commits = cmd!(sh, "git rev-list --count origin/main..HEAD").read()?;
    println!("▶ {short} ({commits} commits on top of origin/main) pushed to {BRANCH}");
    println!("  CI runs every gate lane on it and fast-forwards main once all pass");
    if !opts.wait {
        println!("watch: gh run list --workflow {WORKFLOW} --branch {BRANCH} --commit {head}");
        println!("       then `gh run watch <id> --exit-status`, or land with --wait");
        return Ok(());
    }
    let run = find_run(sh, &head)?;
    println!("  run {}: {}", run.id, run.url);
    let conclusion = finish(sh, run.id)?;
    outcome(sh, &run, &conclusion, &head)
}

/// A workflow run as `gh run list` describes it.
#[derive(serde::Deserialize)]
struct Run {
    #[serde(rename = "databaseId")]
    id: u64,
    url: String,
}

/// The gate's run on `head`, once GitHub lists it.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling GitHub")]
fn find_run(sh: &Shell, head: &str) -> Result<Run> {
    let started = Instant::now();
    loop {
        let listed = cmd!(
            sh,
            "gh run list --workflow {WORKFLOW} --branch {BRANCH} --commit {head} --limit 1 --json databaseId,url"
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

/// Report how the run on `head` ended, and fail unless main moved to it.
fn outcome(sh: &Shell, run: &Run, conclusion: &str, head: &str) -> Result<()> {
    let id = run.id.to_string();
    match conclusion {
        "success" => {
            cmd!(sh, "git fetch --quiet origin main").run()?;
            let main = cmd!(sh, "git rev-parse origin/main").read()?;
            ensure!(
                main == head,
                "the gate passed but origin/main is {main}, not {head}: see {}",
                run.url
            );
            println!("✔ main is at {head}");
            Ok(())
        }
        "cancelled" => bail!(
            "run {id} was cancelled, most likely while it waited, by a newer push to {BRANCH}, \
             whose run decides"
        ),
        // Every lane passed and only CI's promote was refused: a change to a workflow file.
        _ if gate_passed(sh, run.id)? => {
            println!("  CI could not promote it (a workflow file changed); promoting from here");
            promote(sh, &PromoteOpts { commit: Some(head.to_owned()) })
        }
        _ => {
            let failed = cmd!(
                sh,
                "gh run view {id} --json jobs --jq .jobs[]|select(.conclusion==\"failure\" or .conclusion==\"timed_out\")|.name"
            )
            .quiet()
            .read()
            .unwrap_or_default();
            for job in failed.lines() {
                eprintln!("✘ {job}");
            }
            bail!(
                "the gate {conclusion} on {head}; main stays. The run's summary names each failed \
                 lane and test: {}\n  its failed steps' logs: gh run view {id} --log-failed",
                run.url
            )
        }
    }
}
