//! `xtask ci-health`: how CI is doing, in one output. The last Deep nightly job by job (its
//! verdict, wall time and what failed), each push to `gate` over the last day and how long it
//! took to reach main ([`crate::latency`]), the day's red gate runs by the lane that failed, and
//! how long the lanes' checks took to a verdict.

use anyhow::{Context as _, Result};
use xshell::{Shell, cmd};

/// A run of `deep.yml` as `gh run list` gives it.
#[derive(serde::Deserialize)]
struct DeepRun {
    #[serde(rename = "databaseId")]
    id: u64,
    #[serde(rename = "createdAt")]
    at: String,
    status: String,
    conclusion: String,
    event: String,
    #[serde(rename = "headBranch")]
    branch: String,
    url: String,
}

/// One job of a run.
#[derive(serde::Deserialize)]
struct Job {
    #[serde(rename = "databaseId")]
    id: u64,
    name: String,
    status: String,
    conclusion: String,
    #[serde(rename = "startedAt")]
    started: String,
    #[serde(rename = "completedAt")]
    completed: String,
    steps: Vec<Step>,
}

/// One step of a job.
#[derive(serde::Deserialize)]
struct Step {
    name: String,
    conclusion: String,
}

/// How long after its 04:37 UTC schedule a nightly counts as missing: GitHub starts a schedule
/// late under load, by six hours at worst so far (run 38133881188).
const OVERDUE_HOURS: i64 = 26;

/// The most lines of a failed job's log shown.
const SHOWN: usize = 4;

/// The Deep nightly, then the gate's latency since `since` (the last day by default), then how
/// long the lanes' checks took.
pub fn run(sh: &Shell, since: Option<&str>) -> Result<()> {
    deep(sh)?;
    println!();
    crate::latency::run(sh, since)?;
    println!();
    reds(sh)?;
    println!();
    checks()
}

/// One check's latency as `xtask remote` recorded it (`target/remote/history.jsonl`).
#[derive(serde::Deserialize)]
struct Check {
    at: u64,
    full: bool,
    total_s: u64,
    local_s: u64,
    /// How long it waited for its lane directory (recorded since 2026-10-31).
    #[serde(default)]
    queued_s: u64,
    /// A warm of the after-pass, not a lane's check.
    #[serde(default)]
    warm: bool,
    /// How long its e2e case on a hosted Mac took, when it had one (recorded since 2026-10-31).
    #[serde(default)]
    e2e_s: Option<u64>,
}

/// `p50 …s, p90 …s` of `samples`, or `-` for none.
fn spread(mut samples: Vec<u64>) -> String {
    slopty_testkit::stats::Spread::of(&mut samples).map_or_else(
        || "-".to_owned(),
        |s| format!("p50 {}s, p90 {}s, max {}s", s.p50, s.p90, s.max),
    )
}

/// The spread of the lanes' checks over the last day, quick and full, from what
/// `xtask remote` recorded (`target/remote/history.jsonl`): from its start to its verdict, and
/// how long it waited for its lane directory; the after-pass's warms apart. A
/// check that carried an e2e case waits for it on a hosted Mac, which the part here leaves out:
/// those are shown apart, so the verdict's gap over the part here reads as the e2e case it is
/// and not as a queue (2026-10-18: 161 of 300 checks; the others' gap p50 1 s, p90 6 s).
fn checks() -> Result<()> {
    let path = crate::tools::repo_root()?.join("target").join("remote").join("history.jsonl");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("the clock is before 1970")?
        .as_secs();
    let (warms, day): (Vec<Check>, Vec<Check>) = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Check>(line).ok())
        .filter(|c| now.saturating_sub(c.at) <= 86_400)
        .partition(|c| c.warm);
    for full in [false, true] {
        let depth = if full { "full" } else { "quick" };
        let of: Vec<&Check> = day.iter().filter(|c| c.full == full).collect();
        if of.is_empty() {
            println!("no {depth} check recorded in a day ({path})");
            continue;
        }
        println!(
            "{} {depth} checks in a day: verdict {}; the part here {}; queued for a lane \
             directory {}",
            of.len(),
            spread(of.iter().map(|c| c.total_s).collect()),
            spread(of.iter().map(|c| c.local_s).collect()),
            spread(of.iter().map(|c| c.queued_s).collect()),
        );
        let (with_e2e, crates_alone): (Vec<&Check>, Vec<&Check>) =
            of.iter().partition(|c| c.e2e_s.is_some());
        if !with_e2e.is_empty() {
            println!(
                "  {} with an e2e case on a hosted Mac: verdict {}; the e2e case {}",
                with_e2e.len(),
                spread(with_e2e.iter().map(|c| c.total_s).collect()),
                spread(with_e2e.iter().filter_map(|c| c.e2e_s).collect()),
            );
        }
        if !crates_alone.is_empty() {
            println!(
                "  {} of crates alone: verdict {}; past the part here {}",
                crates_alone.len(),
                spread(crates_alone.iter().map(|c| c.total_s).collect()),
                spread(crates_alone.iter().map(|c| c.total_s.saturating_sub(c.local_s)).collect()),
            );
        }
    }
    if !warms.is_empty() {
        println!(
            "{} warms of lane directories after main's lock moved: {}",
            warms.len(),
            spread(warms.iter().map(|c| c.total_s).collect())
        );
    }
    Ok(())
}

/// A push's run of `ci.yml` on `gate`.
#[derive(serde::Deserialize)]
struct GateRun {
    #[serde(rename = "databaseId")]
    id: u64,
    #[serde(rename = "createdAt")]
    at: String,
    #[serde(rename = "headSha")]
    sha: String,
    conclusion: String,
}

/// What a job's failure counts as: its lane, as the gate's job names it (`gate (<lane>)`), or
/// the job.
fn red_cause(job: &str) -> &str {
    job.strip_prefix("gate (").and_then(|j| j.strip_suffix(')')).unwrap_or(job)
}

/// The gate's red runs over the last day: each with the lanes that failed and the first line
/// that says why, then how many reds each lane caused. A red cancelled by its newer push (the
/// failed lane's last step) still names the step that failed. The lane that keeps naming reds is
/// the next check to make before the push: rustdoc named 6 of 14 in the 2.5 days to 2026-10-18.
fn reds(sh: &Shell) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Jobs {
        jobs: Vec<Job>,
    }
    let listed = cmd!(
        sh,
        "gh run list --workflow ci.yml --branch gate --event push --limit 100 --json databaseId,createdAt,headSha,conclusion"
    )
    .quiet()
    .read()
    .context("gh run list")?;
    let runs: Vec<GateRun> = serde_json::from_str(&listed).context("gh run list's JSON")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("the clock is before 1970")?
        .as_secs();
    let since = i64::try_from(now).unwrap_or(i64::MAX).saturating_sub(86_400);
    let day = runs.iter().filter(|r| crate::latency::epoch(&r.at).is_some_and(|at| at >= since));
    let (mut total, mut superseded) = (0_usize, 0_usize);
    let mut causes = std::collections::BTreeMap::<String, usize>::new();
    for run in day.filter(|r| !matches!(r.conclusion.as_str(), "success" | "")) {
        let id = run.id.to_string();
        let viewed = cmd!(sh, "gh run view {id} --json jobs").quiet().read()?;
        let Jobs { jobs } = serde_json::from_str(&viewed).context("gh run view's JSON")?;
        let failed: Vec<&Job> =
            jobs.iter().filter(|j| j.steps.iter().any(|s| s.conclusion == "failure")).collect();
        if failed.is_empty() {
            // Replaced while it waited, or cancelled with no lane red: no red of its own.
            superseded = superseded.saturating_add(1);
            continue;
        }
        total = total.saturating_add(1);
        let sha = run.sha.get(..8).unwrap_or(&run.sha);
        for job in failed {
            let why = why(sh, job.id);
            let cause = red_cause(&job.name);
            let n = causes.entry(cause.to_owned()).or_default();
            *n = n.saturating_add(1);
            let first = why.first().map_or("", String::as_str);
            println!("  red {} {sha} {cause}: {first}", run.at);
        }
    }
    let tally: Vec<String> = causes.iter().map(|(cause, n)| format!("{cause} {n}")).collect();
    println!(
        "{total} red gate runs in a day ({superseded} more replaced or cancelled with none red); \
         by lane: {}",
        if tally.is_empty() { "-".to_owned() } else { tally.join(", ") }
    );
    Ok(())
}

/// The last Deep run on main, job by job, and a newer one on another branch (a trial).
fn deep(sh: &Shell) -> Result<()> {
    let listed = cmd!(
        sh,
        "gh run list --workflow deep.yml --limit 30 --json databaseId,createdAt,status,conclusion,event,headBranch,url"
    )
    .quiet()
    .read()
    .context("gh run list")?;
    let runs: Vec<DeepRun> = serde_json::from_str(&listed).context("gh run list's JSON")?;
    let now = cmd!(sh, "date -u +%Y-%m-%dT%H:%M:%SZ").read()?;
    let now = crate::latency::epoch(&now).context("the time now")?;
    // Scheduled, or started by the gate's promote once the schedule was a day late.
    let nightly = runs.iter().find(|r| r.branch == "main");
    let Some(nightly) = nightly else {
        println!("Deep: no nightly run listed");
        return Ok(());
    };
    let hours =
        crate::latency::epoch(&nightly.at).map_or(0, |at| now.wrapping_sub(at).div_euclid(3_600));
    println!(
        "Deep nightly {} ({}, {}h ago): {} — {}",
        nightly.at,
        nightly.event,
        hours,
        verdict(&nightly.status, &nightly.conclusion),
        nightly.url
    );
    if hours >= OVERDUE_HOURS {
        println!(
            "  ⚠ no nightly has started for {hours}h: GitHub's schedule is late or dropped it"
        );
    }
    jobs(sh, nightly.id)?;
    if let Some(trial) = runs.iter().find(|r| r.branch != "main").filter(|r| r.at > nightly.at) {
        println!(
            "Deep on a branch since, {} {}: {} — {}",
            trial.branch,
            trial.at,
            verdict(&trial.status, &trial.conclusion),
            trial.url
        );
    }
    Ok(())
}

/// A run's or job's verdict: its conclusion once complete, else its status.
fn verdict<'a>(status: &'a str, conclusion: &'a str) -> &'a str {
    if status == "completed" { conclusion } else { status }
}

/// Each job of run `id`: verdict, wall time, and for a failed one its failed steps and the
/// first lines of its log that say why.
fn jobs(sh: &Shell, id: u64) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Jobs {
        jobs: Vec<Job>,
    }
    let id = id.to_string();
    let viewed = cmd!(sh, "gh run view {id} --json jobs").quiet().read().context("gh run view")?;
    let Jobs { jobs } = serde_json::from_str(&viewed).context("gh run view's JSON")?;
    for job in &jobs {
        let wall =
            match (crate::latency::epoch(&job.started), crate::latency::epoch(&job.completed)) {
                (Some(start), Some(end)) if end >= start => {
                    let s = end.wrapping_sub(start);
                    format!("{:>3}m{:02}s", s.div_euclid(60), s.rem_euclid(60))
                }
                _ => "      -".to_owned(),
            };
        println!("  {:<10} {wall}  {}", verdict(&job.status, &job.conclusion), job.name);
        if job.conclusion != "failure" {
            continue;
        }
        let failed: Vec<&str> = job
            .steps
            .iter()
            .filter(|s| s.conclusion == "failure")
            .map(|s| s.name.as_str())
            .collect();
        if !failed.is_empty() {
            println!("      failed step: {}", failed.join("; "));
        }
        for line in why(sh, job.id) {
            println!("      {line}");
        }
    }
    Ok(())
}

/// The first lines of job `id`'s failed log that say what went wrong, colour stripped.
fn why(sh: &Shell, id: u64) -> Vec<String> {
    let id = id.to_string();
    let Ok(log) = cmd!(sh, "gh run view --job {id} --log-failed").quiet().ignore_stderr().read()
    else {
        return Vec::new();
    };
    log.lines()
        .map(|line| strip(line.splitn(3, '\t').nth(2).unwrap_or(line)))
        .map(|line| line.split_once("Z ").map_or_else(|| line.clone(), |(_, rest)| rest.to_owned()))
        .filter(|line| says_why(line))
        .take(SHOWN)
        .map(|line| line.chars().take(200).collect())
        .collect()
}

/// Whether a log line says what failed: an error, a failed test, a panic, a crash found.
fn says_why(line: &str) -> bool {
    let line = line.trim_start();
    ["error", "Error:", "FAIL ", "panicked at", "SUMMARY:", "deadly signal", "##[error]"]
        .iter()
        .any(|mark| line.starts_with(mark) || line.contains(&format!(" {mark}")))
        && !line.contains("error: build failed, waiting for other jobs")
}

/// `line` without its terminal colour codes.
fn strip(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{red_cause, says_why, strip};

    /// A red counts against the lane that failed, or the job when it is no lane's.
    #[test]
    fn a_red_counts_against_its_lane() {
        assert_eq!(red_cause("gate (rustdoc)"), "rustdoc");
        assert_eq!(red_cause("gate (tests worker)"), "tests worker");
        assert_eq!(red_cause("promote"), "promote");
    }

    /// A failed job's reason is the error, the failed test or the panic, without its colour;
    /// cargo's progress and its note that it waits for other jobs are not.
    #[test]
    fn a_failed_log_s_reason_is_its_error_lines() {
        assert_eq!(
            strip("\u{1b}[1m\u{1b}[91merror\u{1b}[0m: could not compile"),
            "error: could not compile"
        );
        assert!(says_why("error[E0063]: missing field"));
        assert!(says_why("        FAIL [  55.452s] slopty-client::threads a_test"));
        assert!(says_why("thread 'main' panicked at src/lib.rs:1:1:"));
        assert!(says_why("##[error]Process completed with exit code 1."));
        assert!(!says_why("   Compiling serde v1.0.0"));
        assert!(!says_why("error: build failed, waiting for other jobs to finish..."));
    }
}
