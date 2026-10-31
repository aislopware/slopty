//! The gate's latency (`xtask ci-health`): how long a push to `gate` takes to reach main, the wait
//! every lane feels, rather than any one job's time.
//!
//! Each push to `gate` starts a run of `ci.yml`. Main moves when a run's `promote` job
//! fast-forwards it (before 2026-10-31 a change to a workflow was promoted by hand from a
//! checkout), which the repository's push events show. A push has landed at the first move of
//! main, at or after it, to a commit that contains it. A push whose run was cancelled, or carried
//! by a later green, lands with that one, so its wait counts the later run's too.

use anyhow::{Context as _, Result};
use xshell::{Shell, cmd};

/// A push to `gate`, from its run of `ci.yml`.
#[derive(serde::Deserialize)]
struct Pushed {
    #[serde(rename = "databaseId")]
    id: u64,
    #[serde(rename = "createdAt")]
    at: String,
    #[serde(rename = "headSha")]
    sha: String,
    conclusion: String,
}

/// A move of main: when, and to which commit.
struct Move {
    at: i64,
    sha: String,
}

/// Seconds since the Unix epoch of an RFC 3339 time in UTC as GitHub writes it
/// (`2026-10-11T12:03:55Z`); `None` for anything else.
pub fn epoch(text: &str) -> Option<i64> {
    let (date, time) = text.strip_suffix('Z')?.split_once('T')?;
    let mut ymd = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (year, month, day) = (ymd.next()??, ymd.next()??, ymd.next()??);
    let mut hms = time.splitn(3, ':').map(|p| p.split('.').next().and_then(|p| p.parse().ok()));
    let (hour, minute, second): (i64, i64, i64) = (hms.next()??, hms.next()??, hms.next()??);
    // Hinnant's days-from-civil, in the proleptic Gregorian calendar.
    let year = if month <= 2 { year.wrapping_sub(1) } else { year };
    let era = year.div_euclid(400);
    let yoe = year.wrapping_sub(era.wrapping_mul(400));
    let mp = if month > 2 { month.wrapping_sub(3) } else { month.wrapping_add(9) };
    let doy = mp.wrapping_mul(153).wrapping_add(2).div_euclid(5).wrapping_add(day).wrapping_sub(1);
    let doe = yoe
        .wrapping_mul(365)
        .wrapping_add(yoe.div_euclid(4))
        .wrapping_sub(yoe.div_euclid(100))
        .wrapping_add(doy);
    let days = era.wrapping_mul(146_097).wrapping_add(doe).wrapping_sub(719_468);
    Some(
        days.wrapping_mul(86_400)
            .wrapping_add(hour.wrapping_mul(3_600))
            .wrapping_add(minute.wrapping_mul(60))
            .wrapping_add(second),
    )
}

/// The moves of main: each `promote` job that passed, and each push event to main (the events
/// API keeps the last few hundred).
fn moves(sh: &Shell, pushed: &[Pushed]) -> Result<Vec<Move>> {
    #[derive(serde::Deserialize)]
    struct Jobs {
        jobs: Vec<Job>,
    }
    #[derive(serde::Deserialize)]
    struct Job {
        name: String,
        conclusion: Option<String>,
        completed_at: Option<String>,
    }
    let mut moves = Vec::new();
    for push in pushed.iter().filter(|p| p.conclusion == "success" || p.conclusion == "failure") {
        let url = format!("repos/{{owner}}/{{repo}}/actions/runs/{}/jobs?per_page=100", push.id);
        let jobs: Jobs = serde_json::from_str(&cmd!(sh, "gh api {url}").quiet().read()?)
            .context("the Actions API's jobs")?;
        let promoted = jobs
            .jobs
            .into_iter()
            .find(|j| j.name == "promote" && j.conclusion.as_deref() == Some("success"));
        if let Some(at) = promoted.and_then(|j| j.completed_at).as_deref().and_then(epoch) {
            moves.push(Move { at, sha: push.sha.clone() });
        }
    }
    let url = "repos/{owner}/{repo}/events?per_page=100";
    let jq = r#".[] | select(.type == "PushEvent" and .payload.ref == "refs/heads/main") | "\(.created_at) \(.payload.head)""#;
    let events = cmd!(sh, "gh api {url} --paginate --jq {jq}")
        .quiet()
        .read()
        .context("the repository's push events")?;
    for line in events.lines() {
        if let Some((at, sha)) = line.split_once(' ')
            && let Some(at) = epoch(at)
        {
            moves.push(Move { at, sha: sha.to_owned() });
        }
    }
    moves.sort_by_key(|m| m.at);
    Ok(moves)
}

/// Prints, oldest first, each push to `gate` since `since` (RFC 3339; the last day without
/// one) and the minutes until main held it, then their spread.
pub fn run(sh: &Shell, since: Option<&str>) -> Result<()> {
    cmd!(sh, "git fetch --quiet origin main gate").run().context("fetching main and gate")?;
    let listed = cmd!(
        sh,
        "gh run list --workflow ci.yml --branch gate --event push --limit 300 --json databaseId,createdAt,headSha,conclusion"
    )
    .quiet()
    .read()
    .context("gh run list")?;
    let mut pushed: Vec<Pushed> = serde_json::from_str(&listed).context("gh run list's JSON")?;
    let now = cmd!(sh, "date -u +%Y-%m-%dT%H:%M:%SZ").read()?;
    let now = epoch(&now).context("the time now")?;
    let since = match since {
        Some(text) => {
            epoch(text).with_context(|| format!("{text} is not like 2026-10-11T12:00:00Z"))?
        }
        None => now.wrapping_sub(86_400),
    };
    pushed.retain(|p| epoch(&p.at).is_some_and(|at| at >= since));
    pushed.sort_by(|a, b| a.at.cmp(&b.at));
    let moves = moves(sh, &pushed)?;
    let contains = |head: &str, sha: &str| {
        cmd!(sh, "git merge-base --is-ancestor {sha} {head}").quiet().ignore_stderr().run().is_ok()
    };
    let mut waits = Vec::new();
    // The pushes whose own run passed: what a lane waits for when nothing goes red.
    let mut green = Vec::new();
    let mut open = 0_usize;
    for push in &pushed {
        let at = epoch(&push.at).context("a run's time")?;
        let landed = moves.iter().find(|m| m.at >= at && contains(&m.sha, &push.sha));
        let short = push.sha.get(..8).unwrap_or(&push.sha);
        let run = if push.conclusion.is_empty() { "running" } else { push.conclusion.as_str() };
        if let Some(m) = landed {
            let seconds = u64::try_from(m.at.wrapping_sub(at)).unwrap_or_default();
            waits.push(seconds);
            if push.conclusion == "success" {
                green.push(seconds);
            }
            println!("{} {short} {run:<9} {} min", push.at, seconds.div_euclid(60));
        } else {
            open = open.saturating_add(1);
            println!("{} {short} {run:<9} not on main yet", push.at);
        }
    }
    let minutes = |s: u64| s.div_euclid(60);
    match slopty_testkit::stats::Spread::of(&mut waits) {
        Some(s) => println!(
            "{} pushes landed: median {} min, p90 {} min, max {} min; {open} not yet",
            s.n,
            minutes(s.p50),
            minutes(s.p90),
            minutes(s.max)
        ),
        None => println!("no push landed since then; {open} not yet"),
    }
    if let Some(s) = slopty_testkit::stats::Spread::of(&mut green) {
        println!(
            "  of them {} whose own run passed: median {} min, p90 {} min, max {} min",
            s.n,
            minutes(s.p50),
            minutes(s.p90),
            minutes(s.max)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::epoch;

    #[test]
    fn github_s_times_read_as_seconds_since_the_epoch() {
        assert_eq!(epoch("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(epoch("2026-10-11T12:03:55Z"), Some(1_791_720_235));
        assert_eq!(epoch("2026-10-11T12:03:55.123Z"), Some(1_791_720_235));
        assert_eq!(epoch("2026-10-11 12:03:55"), None);
        assert_eq!(epoch("yesterday"), None);
    }
}
