//! The order of runs on `gate`, which overlap (`docs/decisions/tooling.md`, "The gate and e2e
//! run on free hosted runners; this Mac runs none", item 1): which run stands aside for a newer
//! one, and how a job of a run ended.
//!
//! A run stands aside only before its e2e starts, and only once `gate` has moved off its commit
//! ([`stands_aside`]): its plan asks as it ends, the e2e job before its setup, and the e2e's
//! watch before its build. The run then is cancelled whole, as it can never promote, and the
//! newer run carries its commit. Nothing else cancels a job of another run, so the run on
//! `gate`'s tip is never set aside, and the newest commit always reaches main or goes red
//! (`docs/decisions/testing.md`, "The gate runs the smoke cases a push reaches").

use xshell::{Shell, cmd};

/// Whether the run on `sha` stands aside, with `gate` at `tip`: once `gate` has moved on, the
/// newer run carries the commit (its plan diffs from a main without it, so it reaches at least
/// the same cases), or a later land withdrew it. A tip that could not be read sets nothing aside.
pub fn stands_aside(tip: Option<&str>, sha: &str) -> bool {
    tip.is_some_and(|tip| !tip.is_empty() && tip != sha)
}

/// `gate`'s tip on `origin`, or `None` when it cannot be read.
pub fn tip(sh: &Shell) -> Option<String> {
    let listed =
        cmd!(sh, "git ls-remote origin refs/heads/gate").quiet().ignore_stderr().read().ok()?;
    listed.split_whitespace().next().map(str::to_owned)
}

/// The words of the annotation GitHub leaves on a job that ran past its `timeout-minutes` ("The
/// job has exceeded the maximum execution time of 1h30m0s", job 112345159635), whose conclusion
/// it lists as `cancelled`, as it does a job of a cancelled run.
pub const TIMED_OUT: &str = "has exceeded the maximum execution time";

/// How a job ended, for a run that needs every job green.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum End {
    /// Not yet.
    Running,
    /// Green or skipped.
    Green,
    /// Failed, ran past its timeout, or ended any other way but cancelled.
    Red,
    /// Cancelled, as every job of a cancelled run is.
    Cancelled,
}

/// How a job whose conclusion is `conclusion` ended (`None` or empty while it runs); for a
/// cancelled one, `timed_out` says whether it ran past its timeout, which GitHub also calls
/// cancelled.
pub fn end(conclusion: Option<&str>, timed_out: impl FnOnce() -> bool) -> End {
    match conclusion {
        None | Some("") => End::Running,
        Some("success" | "skipped") => End::Green,
        Some("cancelled") if timed_out() => End::Red,
        Some("cancelled") => End::Cancelled,
        Some(_) => End::Red,
    }
}

/// Whether job `job` ran past its timeout, from its check run's annotations under `repo`
/// (`repos/<owner>/<name>`, or `gh`'s `repos/{owner}/{repo}`); `false` when they cannot be read.
pub fn timed_out(sh: &Shell, repo: &str, job: u64) -> bool {
    let path = format!("{repo}/check-runs/{job}/annotations?per_page=100");
    cmd!(sh, "gh api {path} --jq .[].message")
        .quiet()
        .ignore_stderr()
        .read()
        .is_ok_and(|said| said.contains(TIMED_OUT))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{End, end, stands_aside};
    use crate::land::{Gate, Next, decide};

    /// A run stands aside once `gate` names another commit, and never on a tip it could not
    /// read; a job cancelled past its timeout is red, any other cancel is a cancel.
    #[test]
    fn a_run_stands_aside_only_once_gate_moved_off_it() {
        assert!(!stands_aside(Some("a"), "a"));
        assert!(stands_aside(Some("b"), "a"));
        assert!(!stands_aside(None, "a"), "an unread tip");
        assert!(!stands_aside(Some(""), "a"), "an empty one");
        assert_eq!(end(None, || true), End::Running);
        assert_eq!(end(Some(""), || true), End::Running);
        assert_eq!(end(Some("skipped"), || true), End::Green);
        assert_eq!(end(Some("failure"), || true), End::Red);
        assert_eq!(end(Some("timed_out"), || true), End::Red);
        assert_eq!(end(Some("cancelled"), || true), End::Red, "past its timeout");
        assert_eq!(end(Some("cancelled"), || false), End::Cancelled);
    }

    /// The rules the model follows: the ones built, or the concurrency groups they replaced.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Rules {
        /// No group: a run stands aside by [`stands_aside`] as its plan ends and as its e2e
        /// starts, and is then cancelled whole.
        StandAside,
        /// One e2e job running and one waiting across runs, the newest to queue cancelling the
        /// one waiting, and no run standing aside.
        Groups,
    }

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    enum E2e {
        /// The plan has not chosen yet, or chose none.
        Unplanned,
        /// Queued for a Mac.
        Waiting,
        Running,
        Passed,
        /// Cancelled while it waited, by another run's job of its group.
        Displaced,
    }

    #[derive(Clone, PartialEq, Eq, Hash, Debug)]
    struct Run {
        pushed: bool,
        planned: bool,
        e2e: E2e,
        shards: bool,
        cancelled: bool,
        promoted: bool,
        /// Re-run once (`gh run rerun --failed`), which the model allows each run once.
        rerun: bool,
    }

    #[derive(Clone, PartialEq, Eq, Hash, Debug)]
    struct World {
        /// `gate`'s tip and main, as commits (0 for none, main's start).
        tip: usize,
        main: usize,
        runs: Vec<Run>,
        /// The group's job running and the one waiting (`Rules::Groups`).
        running: Option<usize>,
        waiting: Option<usize>,
    }

    /// The commits pushed to `gate`, in order: `parents[i]` is commit `i + 1`'s parent (0 being
    /// main's start), so a commit whose parent is not the one before replaced it on `gate`.
    struct History<'a> {
        parents: &'a [usize],
        /// Whether each commit's plan reaches an e2e case.
        cases: &'a [bool],
    }

    impl History<'_> {
        /// Whether `a` is `b` or under it.
        fn under(&self, a: usize, mut b: usize) -> bool {
            loop {
                if a == b {
                    return true;
                }
                let Some(&parent) = b.checked_sub(1).and_then(|k| self.parents.get(k)) else {
                    return false;
                };
                b = parent;
            }
        }
    }

    /// Every state `world` can step to under `rules`, as each job of each run ends in any order
    /// and every test passes; then, apart, every re-run someone could start by hand.
    fn steps(world: &World, history: &History<'_>, rules: Rules) -> (Vec<World>, Vec<World>) {
        let mut next = Vec::new();
        let commits = history.parents.len();
        // The next push, after every push before it; `land` pushes only a commit on top of
        // main, so one that main has moved off is never pushed.
        if let Some(i) = (0..commits).find(|&i| !world.runs[i].pushed)
            && history.under(world.main, i.saturating_add(1))
        {
            let mut w = world.clone();
            w.runs[i].pushed = true;
            w.tip = i.saturating_add(1);
            next.push(w);
        }
        for i in 0..commits {
            let run = &world.runs[i];
            let commit = i.saturating_add(1);
            let sha = commit.to_string();
            let tip = world.tip.to_string();
            if !run.pushed || run.cancelled {
                continue;
            }
            // The plan ends: it stands aside, or its e2e queues.
            if !run.planned {
                let mut w = world.clone();
                w.runs[i].planned = true;
                if history.cases[i] {
                    if rules == Rules::StandAside && stands_aside(Some(&tip), &sha) {
                        w.runs[i].cancelled = true;
                    } else {
                        queue(&mut w, i, rules);
                    }
                }
                next.push(w);
            }
            // A Mac takes the e2e job: it stands aside, or runs.
            let free = match rules {
                Rules::StandAside => true,
                Rules::Groups => world.waiting == Some(i) && world.running.is_none(),
            };
            if run.e2e == E2e::Waiting && free {
                let mut w = world.clone();
                if rules == Rules::StandAside && stands_aside(Some(&tip), &sha) {
                    w.runs[i].cancelled = true;
                } else {
                    w.runs[i].e2e = E2e::Running;
                    if rules == Rules::Groups {
                        w.waiting = None;
                        w.running = Some(i);
                    }
                }
                next.push(w);
            }
            if run.e2e == E2e::Running {
                let mut w = world.clone();
                w.runs[i].e2e = E2e::Passed;
                if w.running == Some(i) {
                    w.running = None;
                }
                next.push(w);
            }
            if !run.shards {
                let mut w = world.clone();
                w.runs[i].shards = true;
                next.push(w);
            }
            // `promote`: every lane green, the e2e passed or the plan chose none; it moves main
            // forward only, and only to a commit `gate` still holds.
            let e2e_done = if history.cases[i] { run.e2e == E2e::Passed } else { run.planned };
            if run.shards && e2e_done && !run.promoted {
                let mut w = world.clone();
                w.runs[i].promoted = true;
                if history.under(commit, world.tip) && history.under(world.main, commit) {
                    w.main = commit;
                }
                next.push(w);
            }
        }
        // A re-run of a run that ended without promoting: its cancelled and failed jobs again,
        // with the plan's old output.
        let mut reruns = Vec::new();
        for i in 0..commits {
            let run = &world.runs[i];
            let ended = run.cancelled || run.e2e == E2e::Displaced;
            if run.pushed && run.planned && ended && !run.rerun && !run.promoted {
                let mut w = world.clone();
                let again = &mut w.runs[i];
                again.rerun = true;
                again.cancelled = false;
                again.e2e = E2e::Unplanned;
                if history.cases[i] {
                    queue(&mut w, i, rules);
                }
                reruns.push(w);
            }
        }
        (next, reruns)
    }

    /// Queue run `i`'s e2e job: under the groups, the one waiting gives way to it.
    fn queue(world: &mut World, i: usize, rules: Rules) {
        world.runs[i].e2e = E2e::Waiting;
        if rules == Rules::Groups
            && let Some(older) = world.waiting.replace(i)
        {
            world.runs[older].e2e = E2e::Displaced;
        }
    }

    /// Every state where nothing more happens unless someone re-runs a run, over every order of
    /// every job's end and with any re-runs before.
    fn ends(history: &History<'_>, rules: Rules) -> Vec<World> {
        let start = World {
            tip: 0,
            main: 0,
            runs: vec![
                Run {
                    pushed: false,
                    planned: false,
                    e2e: E2e::Unplanned,
                    shards: false,
                    cancelled: false,
                    promoted: false,
                    rerun: false,
                };
                history.parents.len()
            ],
            running: None,
            waiting: None,
        };
        let mut seen = HashSet::new();
        let mut todo = vec![start];
        let mut ends = Vec::new();
        while let Some(world) = todo.pop() {
            if !seen.insert(world.clone()) {
                continue;
            }
            let (next, reruns) = steps(&world, history, rules);
            if next.is_empty() {
                ends.push(world);
            }
            todo.extend(next);
            todo.extend(reruns);
        }
        ends
    }

    /// What `land --wait` says of commit `sha` once nothing more happens.
    fn said(world: &World, history: &History<'_>, sha: usize) -> Next {
        let gate = Gate {
            main: world.main.to_string(),
            holds: history.under(sha, world.main),
            red: Vec::new(),
            tip: world.tip.to_string(),
            carries: history.under(sha, world.tip),
        };
        decide(gate, &sha.to_string())
    }

    /// With every test green, whatever order the plans, the e2e jobs and the shards end in, and
    /// with any run re-run, main ends at `gate`'s tip, and `land --wait` hears every commit held
    /// or withdrawn, never stranded. The concurrency groups this replaced strand one: an older
    /// run whose plan ends after a newer one's gives its e2e job the queue, the newer run's
    /// waiting job gives way, and nothing is left to promote the newer commit.
    #[test]
    fn the_newest_commit_reaches_main_whatever_order_the_jobs_end_in() {
        let histories = [
            History { parents: &[0, 1], cases: &[true, true] },
            History { parents: &[0, 1, 2], cases: &[true, true, true] },
            History { parents: &[0, 1, 2], cases: &[true, false, true] },
            History { parents: &[0, 1, 2], cases: &[false, true, false] },
            // The third replaced the second on `gate`, which it withdrew.
            History { parents: &[0, 1, 1], cases: &[true, true, true] },
        ];
        for history in &histories {
            let ends = ends(history, Rules::StandAside);
            assert_ne!(ends, []);
            for world in &ends {
                assert_eq!(world.main, world.tip, "main at the newest commit: {world:?}");
                for (sha, _) in (1..).zip(&world.runs).filter(|(_, run)| run.pushed) {
                    let next = said(world, history, sha);
                    assert!(
                        matches!(next, Next::Held(_) | Next::Withdrawn(_)),
                        "commit {sha}: {next:?} in {world:?}"
                    );
                }
            }
        }
        let stranded = ends(&histories[0], Rules::Groups)
            .into_iter()
            .find(|world| world.main != 2)
            .expect("the groups strand the newer commit in some order");
        assert_eq!(said(&stranded, &histories[0], 2), Next::Ended, "{stranded:?}");
    }
}
