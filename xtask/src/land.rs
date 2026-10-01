//! `xtask land`: hand the commits on `main` to CI, which moves `main` to them once every gate
//! lane passed there (`.github/workflows/ci.yml`, its `promote` job).
//!
//! The `gate` branch is always `origin/main` plus what is landing, so each land replaces what the
//! last one left, with a lease, so a push this checkout has not seen is never dropped. CI runs one
//! gate at a time on that branch and a newer push cancels an older run: the last green covers
//! every commit under it.

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
    let leased = cmd!(sh, "git rev-parse --verify --quiet refs/remotes/origin/{BRANCH}")
        .quiet()
        .read()
        .unwrap_or_default();
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
            "run {id} was cancelled, most likely by a newer push to {BRANCH}, whose run decides"
        ),
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
