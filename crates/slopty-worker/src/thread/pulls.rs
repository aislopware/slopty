//! Each thread's pull request, read again from the forge as it moves.
//!
//! A check that fails, changes asked for or a pull request ready to merge then reach the person
//! without anyone opening the commit sheet (`docs/decisions/agents.md`, "A thread's pull
//! request is watched").
//!
//! The pull request is the one of the branch the thread's folder has checked out, as the
//! person's own gh reads it ([`crate::repo::pull::status`]). Threads that share a checkout share
//! one read. A checkout on its repository's default branch, a thread that ended days ago and
//! a worker without gh are never asked about. What the forge said is summed up in a
//! [`PullSeen`] and put on the thread with [`Action::PullSeen`] when it changed, so its row
//! carries it to every client and to the server's attention ladder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use slopty_proto::git::{CheckBucket, PullStatus};
use slopty_proto::thread::wire::{PullSeen, PullStands};
use slopty_proto::thread::{Action, Liveness, Phase, ThreadId};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::Host;

/// How often the watcher looks for reads falling due.
const TICK: Duration = Duration::from_secs(15);
/// How soon a pull request is read again while its checks run or its thread works: a failed
/// check reaches the person within the minute.
const LIVELY: Duration = Duration::from_secs(60);
/// How soon once it has settled and its threads rest.
const SETTLED: Duration = Duration::from_mins(5);
/// How soon after the branch had none, or gh could not answer.
const QUIET: Duration = Duration::from_mins(10);
/// How long after a thread ended its pull request is still watched.
const ENDED_FOR: Duration = Duration::from_hours(72);

/// A checkout and the branch it has checked out: one read serves every thread in it.
type Key = (PathBuf, String);

/// Watch the pull requests of `host`'s threads with `gh` until the host is gone.
#[must_use]
pub fn spawn(host: Host, gh: PathBuf) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut due: HashMap<Key, Instant> = HashMap::new();
        loop {
            tokio::time::sleep(TICK).await;
            round(&host, &gh, &mut due).await;
        }
    })
}

/// One thread's facts the watcher reads.
struct Watched {
    thread: ThreadId,
    key: Key,
    lively: bool,
    pull: Option<PullSeen>,
}

/// One round: read every pull request that falls due, put what changed on its threads, and
/// say when each is due again.
async fn round(host: &Host, gh: &Path, due: &mut HashMap<Key, Instant>) {
    let now_ms = slopty_core::WallMs::now().as_millis();
    let watched = host.visit(|state| {
        let ended = match state.status.liveness {
            Liveness::Exited { .. } => {
                let since = state.status.since_ms.as_millis();
                now_ms.saturating_sub(since) > u64::try_from(ENDED_FOR.as_millis()).ok()?
            }
            _ => false,
        };
        if ended || state.meta.cwd.is_empty() {
            return None;
        }
        let key = checkout(Path::new(&state.meta.cwd))?;
        let lively = matches!(state.status.phase, Phase::Working | Phase::Waiting);
        Some(Watched { thread: state.meta.id, key, lively, pull: state.pull.clone() })
    });
    due.retain(|key, _| watched.iter().any(|w| w.key == *key));
    let mut keys: HashMap<&Key, Vec<&Watched>> = HashMap::new();
    for w in &watched {
        keys.entry(&w.key).or_default().push(w);
    }
    let now = Instant::now();
    for (key, threads) in keys {
        if due.get(key).is_some_and(|when| *when > now) {
            continue;
        }
        let read = crate::repo::pull::status(Some(gh), &key.0).await;
        let seen = match read {
            Ok(status) => status.map(|s| seen(&s)),
            Err(failed) => {
                tracing::debug!(?failed, checkout = %key.0.display(), "a pull request not read");
                due.insert(key.clone(), now.checked_add(QUIET).unwrap_or(now));
                continue;
            }
        };
        let lively = threads.iter().any(|w| w.lively)
            || seen.as_ref().is_some_and(|s| s.stands == PullStands::Running);
        let next = match &seen {
            None => QUIET,
            Some(_) if lively => LIVELY,
            Some(_) => SETTLED,
        };
        due.insert(key.clone(), now.checked_add(next).unwrap_or(now));
        for w in threads.iter().filter(|w| w.pull != seen) {
            host.apply(w.thread, vec![Action::PullSeen(seen.clone())]);
        }
    }
}

/// The checkout `cwd` is in and the branch it has checked out, when that is a branch other
/// than its repository's default: a pull request merges from such a branch.
fn checkout(cwd: &Path) -> Option<Key> {
    let root = crate::repo::root_of(cwd)?;
    let branch = crate::repo::branch_of(&root)?;
    let refs = crate::repo::common_dir(&root)?;
    let default = std::fs::read_to_string(refs.join("refs/remotes/origin/HEAD")).ok();
    let default =
        default.as_deref().and_then(|t| t.trim().strip_prefix("ref: refs/remotes/origin/"));
    let is_branch = refs.join("refs/heads").join(&branch).exists() || packed(&refs, &branch);
    // With no `origin/HEAD` recorded, the usual names of a default branch stand in for it.
    let default =
        default.map_or_else(|| ["main", "master"].contains(&branch.as_str()), |d| d == branch);
    (is_branch && !default).then_some((root, branch))
}

/// Whether `branch` is among the clone's packed refs.
fn packed(refs: &Path, branch: &str) -> bool {
    let wanted = format!(" refs/heads/{branch}");
    std::fs::read_to_string(refs.join("packed-refs"))
        .is_ok_and(|text| text.lines().any(|line| line.ends_with(&wanted)))
}

/// What the forge said of a pull request, in a line ([`PullSeen`]).
#[must_use]
pub fn seen(status: &PullStatus) -> PullSeen {
    let count = |bucket| status.checks.iter().filter(|c| c.bucket() == bucket).count();
    let failed = count(CheckBucket::Failed);
    let running = count(CheckBucket::Running);
    let failed_first =
        status.checks.iter().find(|c| c.bucket() == CheckBucket::Failed).map(|c| c.name.clone());
    let stands = match status.state.as_str() {
        "MERGED" => PullStands::Merged,
        "CLOSED" => PullStands::Closed,
        _ if status.draft => PullStands::Draft,
        _ if status.mergeable == "CONFLICTING" || status.merge_state == "DIRTY" => {
            PullStands::Conflicted
        }
        _ if failed > 0 => PullStands::ChecksFailed,
        _ if status.review == "CHANGES_REQUESTED" => PullStands::ChangesRequested,
        _ if running > 0 => PullStands::Running,
        _ if status.merge_state == "CLEAN" || status.merge_state == "HAS_HOOKS" => {
            PullStands::Ready
        }
        _ => PullStands::Waiting,
    };
    let n = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    PullSeen {
        number: status.number,
        url: status.url.clone(),
        title: status.title.clone(),
        stands,
        failed: n(failed),
        failed_first,
        running: n(running),
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::git::PullCheck;

    use super::*;

    fn status(edit: impl FnOnce(&mut PullStatus)) -> PullStatus {
        let check = |name: &str, state: &str| PullCheck {
            name: name.to_owned(),
            workflow: Some("CI".to_owned()),
            state: state.to_owned(),
            link: None,
        };
        let mut status = PullStatus {
            number: 42,
            url: "https://github.com/o/r/pull/42".to_owned(),
            title: "Fix the login".to_owned(),
            state: "OPEN".to_owned(),
            draft: false,
            head: "fix-login".to_owned(),
            head_commit: "0123abcd".to_owned(),
            base: "main".to_owned(),
            review: String::new(),
            mergeable: "MERGEABLE".to_owned(),
            merge_state: "CLEAN".to_owned(),
            checks: vec![check("test", "SUCCESS"), check("lint", "SUCCESS")],
            more_checks: 0,
        };
        edit(&mut status);
        status
    }

    /// A pull request stands where the first thing that holds puts it: ended, a draft, a
    /// conflict, a failed check, changes asked for, checks running, else ready or waiting.
    #[test]
    fn a_pull_request_stands_where_the_first_thing_that_holds_puts_it() {
        let stands = |edit: fn(&mut PullStatus)| seen(&status(edit)).stands;
        assert_eq!(stands(|_| {}), PullStands::Ready);
        assert_eq!(stands(|s| s.state = "MERGED".to_owned()), PullStands::Merged);
        assert_eq!(stands(|s| s.state = "CLOSED".to_owned()), PullStands::Closed);
        assert_eq!(stands(|s| s.draft = true), PullStands::Draft);
        assert_eq!(stands(|s| s.mergeable = "CONFLICTING".to_owned()), PullStands::Conflicted);
        assert_eq!(stands(|s| s.checks[1].state = "FAILURE".to_owned()), PullStands::ChecksFailed);
        assert_eq!(
            stands(|s| s.review = "CHANGES_REQUESTED".to_owned()),
            PullStands::ChangesRequested
        );
        assert_eq!(stands(|s| s.checks[1].state = "IN_PROGRESS".to_owned()), PullStands::Running);
        assert_eq!(stands(|s| s.merge_state = "BLOCKED".to_owned()), PullStands::Waiting);

        let failed = seen(&status(|s| {
            s.checks[0].state = "FAILURE".to_owned();
            s.checks[1].state = "TIMED_OUT".to_owned();
        }));
        assert_eq!((failed.failed, failed.failed_first.as_deref()), (2, Some("test")));
        assert_eq!(failed.line(), "#42: test and 1 more failed");
        assert_eq!(seen(&status(|_| {})).line(), "#42 is ready to merge");
    }

    /// A checkout on a branch other than its repository's default is watched; one on the
    /// default, one with no commit on its branch yet and one detached are not.
    #[test]
    fn only_a_checkout_on_a_branch_of_its_own_is_watched() {
        let Some(git) = crate::changes::git() else { return };
        let tmp = tempfile::tempdir().expect("temp");
        let work = tmp.path().join("work");
        std::fs::create_dir_all(work.join("src")).expect("mkdir");
        let run = |args: &[&str]| {
            let out = std::process::Command::new(git)
                .arg("-C")
                .arg(&work)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .expect("git runs");
            assert!(out.status.success(), "{args:?}");
        };
        run(&["init", "-q", "-b", "main"]);
        assert_eq!(checkout(&work.join("src")), None, "no commit on its branch yet");
        run(&["commit", "-q", "--allow-empty", "-m", "c0"]);
        assert_eq!(checkout(&work), None, "the default branch");
        run(&["checkout", "-q", "-b", "fix-login"]);
        let root = std::fs::canonicalize(&work).ok();
        let key = checkout(&work.join("src"));
        assert_eq!(
            key.map(|(r, b)| (std::fs::canonicalize(r).ok(), b)),
            Some((root, "fix-login".to_owned()))
        );
        run(&["checkout", "-q", "--detach"]);
        assert_eq!(checkout(&work), None, "detached");
    }
}
