//! A pull request's own checks, as its forge reports them, read with the forge's own command
//! line: `gh pr checks` for GitHub, `glab mr view` for a GitLab merge request's pipeline.
//!
//! The command runs as the worker's user, signed in as they signed it in. Nothing of the
//! sign-in is read or passed: a command that is not signed in says so, and that is the answer.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use slopty_core::WallMs;
use slopty_proto::project::{CHECK_NAME_MAX, CHECKS_NAMED, Checks, ChecksState};

/// The longest a forge may take to answer.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Where a forge's command line is looked for beyond `PATH`: a daemon started by launchd has
/// little on its `PATH`, and Homebrew puts both commands here.
const KNOWN: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"];

/// Why the checks could not be read.
#[derive(Debug, PartialEq, Eq)]
pub enum Failed {
    /// The worker has no such command.
    Missing(&'static str),
    /// The command ran and said why not, or said something that is not its JSON.
    Said(String),
}

/// `program` on `path`, else in the places Homebrew and the system put it.
#[must_use]
pub fn find(program: &str, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let on_path = path.map(std::env::split_paths).into_iter().flatten();
    on_path
        .chain(KNOWN.iter().map(PathBuf::from))
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// The checks of pull request `number` (a merge request's when `merge_request`), read in the
/// checkout at `cwd`.
///
/// # Errors
/// When the command is missing, fails or says something else.
pub async fn read(cwd: &Path, number: u32, merge_request: bool) -> Result<Checks, Failed> {
    read_on(std::env::var_os("PATH").as_deref(), cwd, number, merge_request).await
}

/// [`read`], with the command looked for on `path`.
async fn read_on(
    path: Option<&std::ffi::OsStr>,
    cwd: &Path,
    number: u32,
    merge_request: bool,
) -> Result<Checks, Failed> {
    let number = number.to_string();
    let (program, args): (&'static str, Vec<&str>) = if merge_request {
        ("glab", vec!["mr", "view", &number, "--output", "json"])
    } else {
        ("gh", vec!["pr", "checks", &number, "--json", "name,bucket"])
    };
    let found = find(program, path).ok_or(Failed::Missing(program))?;
    let ran = tokio::process::Command::new(found)
        .args(&args)
        .current_dir(cwd)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(TIMEOUT, ran)
        .await
        .map_err(|_elapsed| Failed::Said(format!("{program} took too long")))?
        .map_err(|e| Failed::Said(format!("{program} did not start: {e}")))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let now = WallMs::now();
    if merge_request {
        return if out.status.success() { glab(&stdout, now) } else { Err(said(&stderr)) };
    }
    // `gh pr checks` ends 8 while checks run and 1 once one fails, with its JSON all the same.
    match gh(&stdout, now) {
        Ok(checks) => Ok(checks),
        Err(_) if stderr.contains("no checks reported") => Ok(none(now)),
        Err(not_json) if out.status.success() => Err(not_json),
        Err(_) => Err(said(&stderr)),
    }
}

/// The last thing a command said on its error stream, in a line.
fn said(stderr: &str) -> Failed {
    let line = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("it failed");
    Failed::Said(line.trim().to_owned())
}

const fn none(at_ms: WallMs) -> Checks {
    Checks {
        state: ChecksState::None,
        passed: 0,
        failed: 0,
        pending: 0,
        skipped: 0,
        failing: Vec::new(),
        at_ms,
    }
}

/// One check as `gh pr checks --json name,bucket` lists it.
#[derive(Deserialize)]
struct GhCheck {
    name: String,
    /// `pass`, `fail`, `pending`, `skipping` or `cancel`.
    bucket: String,
}

/// What `gh pr checks --json name,bucket` printed, together.
///
/// # Errors
/// When it is not that JSON.
pub fn gh(stdout: &str, at_ms: WallMs) -> Result<Checks, Failed> {
    let listed: Vec<GhCheck> = serde_json::from_str(stdout)
        .map_err(|e| Failed::Said(format!("gh printed something else: {e}")))?;
    Ok(together(listed.iter().map(|c| (c.name.as_str(), c.bucket.as_str())), at_ms))
}

/// A merge request as `glab mr view --output json` prints it, as far as its pipeline.
#[derive(Deserialize)]
struct GlabRequest {
    head_pipeline: Option<GlabPipeline>,
}

#[derive(Deserialize)]
struct GlabPipeline {
    status: String,
}

/// What `glab mr view --output json` printed of the merge request's pipeline, as one check.
///
/// # Errors
/// When it is not that JSON.
pub fn glab(stdout: &str, at_ms: WallMs) -> Result<Checks, Failed> {
    let request: GlabRequest = serde_json::from_str(stdout)
        .map_err(|e| Failed::Said(format!("glab printed something else: {e}")))?;
    let Some(pipeline) = request.head_pipeline else { return Ok(none(at_ms)) };
    let bucket = match pipeline.status.as_str() {
        "success" => "pass",
        "failed" => "fail",
        "canceled" => "cancel",
        "skipped" => "skipping",
        _ => "pending",
    };
    Ok(together([("pipeline", bucket)], at_ms))
}

/// Checks named with their buckets, counted and judged together. A bucket the forge adds later
/// counts as pending: it is not a pass.
fn together<'a>(listed: impl IntoIterator<Item = (&'a str, &'a str)>, at_ms: WallMs) -> Checks {
    let mut checks = none(at_ms);
    let add = |n: &mut u16| *n = n.saturating_add(1);
    for (name, bucket) in listed {
        match bucket {
            "pass" => add(&mut checks.passed),
            "skipping" => add(&mut checks.skipped),
            "fail" | "cancel" => {
                add(&mut checks.failed);
                if checks.failing.len() < CHECKS_NAMED {
                    checks.failing.push(clipped(name));
                }
            }
            _ => add(&mut checks.pending),
        }
    }
    checks.state = if checks.failed > 0 {
        ChecksState::Failing
    } else if checks.pending > 0 {
        ChecksState::Pending
    } else if checks.passed > 0 || checks.skipped > 0 {
        ChecksState::Passing
    } else {
        ChecksState::None
    };
    checks
}

/// `name` within [`CHECK_NAME_MAX`], cut at a character.
fn clipped(name: &str) -> String {
    let mut end = name.len().min(CHECK_NAME_MAX);
    while !name.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    name.get(..end).unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests;
