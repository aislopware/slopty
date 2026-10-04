//! Each project's lane: its tasks' verifiers and its merge queue, one job at a time
//! (`docs/decisions/projects.md`, "A task is done when its verifier passes; the server merges
//! one at a time").
//!
//! A task whose agent reports done has its branch brought to the orchestrator's clone, then
//! moves to verifying. The lane runs its verifier ([`Hub::verify_job`]) in the project's own
//! checkout of that clone, in a terminal every client shows, and a pass leaves the task done
//! and ready to merge: only the person's Merge puts it in the queue. The head of the queue
//! ([`Hub::merge_job`]) is rebased onto the target there, each commit carrying the task and the
//! thread it came from as trailers, verified again unless the rebase left the tree already
//! verified, and the target is fast-forwarded to it. A failure or a conflict gives the task
//! back to its agent with the reason, through its hooks, and the orchestrator hears too; past
//! [`GIVE_BACKS_MAX`] the failure waits for the person instead.
//!
//! The lane holds nothing the store does not: what it does next is read from the tasks each
//! time ([`crate::project::Job`]), so a server that restarts takes it up where it stood. A job
//! that cannot go on for a reason that is not the task's (the worker is away, the person's
//! checkout has changes in the way) stops the lane with the reason on the task's step, and the
//! next change that concerns it starts it again.

use std::collections::HashSet;
use std::time::Duration;

use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::orchestration::{ErrorCode, Outcome, TermRef, Verb};
use slopty_proto::project::{
    Commits, GIVE_BACKS_MAX, Merge, Moment, ProjectId, StepKind, StepState, TEST_PATHS_KEY, Task,
    TaskId, TaskState, TaskStep, TestDiff, VerifierRun,
};
use slopty_proto::terminal::SessionState;
use slopty_proto::thread::ThreadId;

use super::steps::{Onto, said};
use super::{Hub, State, error};
use crate::deliver::Kind;
use crate::project::{Advance, Caller, Job, Queue};

/// How often a running verifier's last line is read for its step.
const PROGRESS_EVERY: Duration = Duration::from_secs(2);
/// How many times the queue tries one task again when the target moved under it.
const MERGE_TRIES: usize = 3;
/// The most paths a project's metadata may add to where tests are found.
const TEST_PATHS_MAX: usize = 32;
/// The longest line a step's progress shows.
const PROGRESS_MAX: usize = 160;
/// The trailer naming the project and task a merged commit's work came from.
pub(super) const TASK_TRAILER: &str = "Slopty-Task";
/// The trailer naming the thread whose agent wrote a merged commit's work.
pub(super) const THREAD_TRAILER: &str = "Slopty-Thread";

/// The lanes running, one per project at most.
#[derive(Debug, Default)]
pub(super) struct Lanes {
    running: HashSet<ProjectId>,
    /// Asked to look again once the job under way ends.
    again: HashSet<ProjectId>,
}

/// Where a project's work is verified and merged, and with what.
#[derive(Clone, Debug)]
pub(super) struct Place {
    /// The orchestrator's worker.
    pub worker: WorkerId,
    /// Its clone of the project's repository.
    pub clone: String,
    /// The task's work there: its branch brought home, or its own branch in that clone.
    pub head: String,
    /// The branch work lands on.
    pub target: String,
    /// The task's verifier, else the project's.
    pub verifier: Option<String>,
    /// Push the target after a merge.
    pub push: bool,
    /// The project's checkout's name.
    pub worktree: String,
}

/// How a job ended for the lane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Went {
    /// On to the next job.
    Next,
    /// Stopped for a reason that is not the task's: the lane waits to be started again.
    Hold,
}

/// How a verifier run ended.
enum Ran {
    /// It ran to an end, or its terminal closed first.
    Judged(VerifierRun, TermRef),
    /// It could not run for a reason that is not the work's: the worker is away.
    Held(String),
    /// It could not run on this work: the branch is not there, it shares no history.
    Refused(String),
}

/// `ms` as people read a duration.
pub(super) fn took(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..60 => format!("{secs}s"),
        _ => format!("{}m {}s", secs / 60, secs % 60),
    }
}

/// A commit as people read it.
pub(super) fn short(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

/// The last line `text` holds that says anything.
fn last_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).rfind(|l| !l.is_empty())
}

/// The end of `text`, at most `max` bytes of whole lines, so a failure's last words are kept.
fn tail(text: &str, max: usize) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0_usize;
    for line in text.lines().rev() {
        let len = line.len().saturating_add(1);
        if used.saturating_add(len) > max {
            break;
        }
        used = used.saturating_add(len);
        kept.push(line);
    }
    kept.reverse();
    kept.join("\n")
}

/// Whether an answer says the worker could not be reached, rather than anything of the work.
pub(super) const fn unreachable(outcome: &Outcome) -> bool {
    matches!(
        outcome,
        Outcome::Error { code: ErrorCode::WorkerUnreachable | ErrorCode::Interrupted, .. }
    )
}

impl Hub {
    /// Start `project`'s lane, or have the one running look again once its job ends.
    pub(super) fn kick(&self, state: &mut State, project: &ProjectId) {
        if state.lanes.running.contains(project) {
            state.lanes.again.insert(project.clone());
            return;
        }
        if state.projects.next_job(project).is_none() {
            return;
        }
        state.lanes.running.insert(project.clone());
        let (hub, project) = (self.downgrade(), project.clone());
        tokio::spawn(async move {
            let Some(hub) = hub.upgrade() else { return };
            hub.lane(project).await;
        });
    }

    /// `task`'s agent said it is done and its branch is in the orchestrator's clone: its
    /// verifier runs there, when the task or its project names one and there is a
    /// clone to run them in. A task with none is ready to merge at once: done, waiting for the
    /// person's Merge.
    pub(super) fn verify_soon(&self, state: &mut State, (project, task): (&ProjectId, TaskId)) {
        let (Ok(record), Ok(t)) =
            (state.projects.project(project), state.projects.task(project, task))
        else {
            return;
        };
        let checks = t.verifier.is_some() || record.verifier.is_some();
        let placed =
            record.orchestrator.is_some() && record.repo_id.is_some() && t.branch.is_some();
        if !placed || t.state == TaskState::Merged || t.read_only {
            return;
        }
        let to = if checks { TaskState::Verifying } else { TaskState::Done };
        let moment = (t.state != to).then_some(Moment::State { from: t.state, to });
        let advance = Advance {
            state: Some(to),
            merge: Queue::Leave,
            fresh: true,
            moment,
            ..Advance::default()
        };
        self.let_go(state, (project, task));
        match state.projects.advance(project, task, advance, WallMs::now()) {
            Ok((_, updates)) => self.projects_moved(state, updates),
            Err(refused) => tracing::debug!(%project, %task, ?refused, "not verifying"),
        }
        self.kick(state, project);
    }

    /// `task`'s work is to be judged afresh: the terminal its last verifier was kept in is
    /// closed.
    pub(super) fn let_go(&self, state: &State, (project, task): (&ProjectId, TaskId)) {
        let kept = state
            .projects
            .task(project, task)
            .ok()
            .and_then(|t| t.step.as_ref().filter(|s| !s.running())?.term);
        if let Some(term) = kept {
            self.close_soon(term);
        }
    }

    /// Close `term`, in the background: nothing waits on it.
    pub(super) fn close_soon(&self, term: TermRef) {
        let hub = self.clone();
        tokio::spawn(async move {
            if let failed @ Outcome::Error { .. } = hub.forward(None, Verb::Close { term }).await {
                tracing::debug!(?failed, "a terminal not closed");
            }
        });
    }

    /// What `task`'s work did to the project's tests, read in the orchestrator's clone once
    /// its branch is there: on its card, and in what its agent is told.
    pub(super) async fn read_tests(&self, (project, task): (&ProjectId, TaskId)) {
        let Ok(place) = self.lane_place(project, task) else { return };
        let test_paths = {
            let state = self.inner.state.lock();
            let metadata = state.projects.project(project).ok().and_then(|p| p.metadata.clone());
            drop(state);
            test_paths(metadata.as_deref())
        };
        let verb = Verb::TestDiff {
            worker: place.worker,
            repo: place.clone,
            head: place.head,
            target: place.target,
            test_paths,
        };
        match self.forward(None, verb).await {
            Outcome::TestDiff(tests) => {
                let mut state = self.inner.state.lock();
                let updates = state.projects.set_tests(project, task, tests, WallMs::now());
                self.projects_moved(&mut state, updates);
                drop(state);
            }
            other => tracing::debug!(%project, %task, ?other, "its tests not read"),
        }
    }

    /// The person's ask for `task`'s merge, made once its branch came home for it.
    pub(super) fn merge_asked(&self, state: &mut State, (project, task): (&ProjectId, TaskId)) {
        self.let_go(state, (project, task));
        match state.projects.ask_merge(project, task, WallMs::now()) {
            Ok((_, updates)) => self.projects_moved(state, updates),
            Err(refused) => tracing::debug!(%project, %task, ?refused, "not merged after all"),
        }
        self.kick(state, project);
    }

    /// Start every project's lane that has work: after a restart, or once a worker is back.
    pub(super) fn kick_all(&self, state: &mut State) {
        for project in state.projects.with_jobs() {
            self.kick(state, &project);
        }
    }

    /// One project's jobs, one at a time, until there are none or one holds.
    async fn lane(&self, project: ProjectId) {
        loop {
            let job = { self.inner.state.lock().projects.next_job(&project) };
            let went = match job {
                Some(Job::Verify(task)) => self.verify_job(&project, task).await,
                Some(Job::Merge(task)) => self.merge_job(&project, task).await,
                None => Went::Hold,
            };
            let mut state = self.inner.state.lock();
            let again = state.lanes.again.remove(&project);
            if went == Went::Hold && !again {
                state.lanes.running.remove(&project);
                drop(state);
                return;
            }
            drop(state);
        }
    }

    /// Where `task`'s work is verified and merged: the orchestrator's clone, with the task's
    /// branch as it is there.
    pub(super) fn lane_place(&self, project: &ProjectId, task: TaskId) -> Result<Place, String> {
        let landed = self.landed(project, task)?;
        let state = self.inner.state.lock();
        let found = state
            .projects
            .project(project)
            .cloned()
            .and_then(|record| Ok((record, state.projects.task(project, task)?.clone())));
        drop(state);
        let (record, card) = found.map_err(|refused| said(&refused))?;
        if record.orchestrator.is_none() {
            return Err("the project has no orchestrator, in whose clone its work is verified \
                        and merged"
                .to_owned());
        }
        let Some(landed) = landed else {
            return Err(if card.branch.is_none() {
                "the task names no branch to verify".to_owned()
            } else {
                "the project's repository is not known yet, nor where it is cloned".to_owned()
            });
        };
        Ok(Place {
            worker: landed.worker,
            clone: landed.clone,
            head: landed.branch,
            target: record.target,
            verifier: card.verifier.or(record.verifier),
            push: record.push,
            worktree: project.as_str().to_owned(),
        })
    }

    /// Move `task` as the lane says, and push the change.
    pub(super) fn advance(
        &self,
        (project, task): (&ProjectId, TaskId),
        advance: Advance,
    ) -> Option<Task> {
        let mut state = self.inner.state.lock();
        match state.projects.advance(project, task, advance, WallMs::now()) {
            Ok((moved, updates)) => {
                self.projects_moved(&mut state, updates);
                Some(moved)
            }
            Err(refused) => {
                tracing::debug!(%project, %task, ?refused, "a lane's move refused");
                None
            }
        }
    }

    /// `task`'s step, moved on as it goes.
    pub(super) fn progress(&self, (project, task): (&ProjectId, TaskId), step: TaskStep) {
        let mut state = self.inner.state.lock();
        if let Ok(updates) = state.projects.set_step(project, task, step, WallMs::now()) {
            self.projects_moved(&mut state, updates);
        }
    }

    /// Close the terminal a failed verifier was kept in, now that `task` is judged again.
    pub(super) async fn close_kept(&self, (project, task): (&ProjectId, TaskId)) {
        let kept = {
            let state = self.inner.state.lock();
            state.projects.task(project, task).ok().and_then(|t| t.step.as_ref()?.term)
        };
        if let Some(term) = kept
            && let failed @ Outcome::Error { .. } = self.forward(None, Verb::Close { term }).await
        {
            tracing::debug!(%project, %task, ?failed, "a verifier's terminal not closed");
        }
    }

    /// Run `place`'s verifier on `head` in the project's checkout, in a terminal of its own,
    /// showing its last line on `task`'s step of `kind` as it goes (after `label`, when there
    /// is one). A pass closes its terminal; a failure keeps it for its whole output.
    async fn run_verifier(
        &self,
        at: (&ProjectId, TaskId),
        place: &Place,
        head: &str,
        (kind, label): (StepKind, &str),
    ) -> Ran {
        let Some(command) = place.verifier.clone() else {
            return Ran::Refused("no verifier is named".to_owned());
        };
        let term = TermRef { worker: place.worker, session: SessionId::new() };
        let started = WallMs::now();
        let starting = TaskStep {
            kind,
            worker: place.worker,
            state: StepState::Running { phase: label.to_owned(), percent: None },
            since_ms: started,
            term: Some(term),
            commits: None,
        };
        self.progress(at, starting);
        let verb = Verb::Verify {
            worker: place.worker,
            repo: place.clone.clone(),
            worktree: place.worktree.clone(),
            head: head.to_owned(),
            target: place.target.clone(),
            command,
            session: term.session,
            title: format!("Verifier for {} #{}", at.0, at.1),
        };
        let commits = match self.forward(None, verb).await {
            Outcome::Verifying { head, base, .. } => Commits { head, base },
            other if unreachable(&other) => return Ran::Held(said(&other)),
            other => return Ran::Refused(said(&other)),
        };
        self.watch_verifier(at, term, commits, (kind, label), started).await
    }

    /// Follow the verifier in `term`, on `commits`, to its end, showing its last line on
    /// `task`'s step of `kind` as it goes (after `label`, when there is one), and judge it: a
    /// pass closes its terminal, a failure keeps it for its whole output. One a restart of the
    /// server left running is followed as it is.
    async fn watch_verifier(
        &self,
        at: (&ProjectId, TaskId),
        term: TermRef,
        commits: Commits,
        (kind, label): (StepKind, &str),
        started: WallMs,
    ) -> Ran {
        let step = |phase: String| TaskStep {
            kind,
            worker: term.worker,
            state: StepState::Running { phase, percent: None },
            since_ms: started,
            term: Some(term),
            commits: Some(commits.clone()),
        };
        self.progress(at, step(label.to_owned()));
        let mut head_seen = self.inner.head.subscribe();
        let clock = tokio::time::Instant::now();
        let mut shown = String::new();
        let mut tick = tokio::time::interval(PROGRESS_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut seen = false;
        // The worker tells of a terminal it opened before it answers, so one it does not list
        // soon after is gone.
        let grace = Duration::from_secs(5);
        let exit = loop {
            let session = {
                let state = self.inner.state.lock();
                state.workers.get(&term.worker).map(|e| {
                    let online = e.link.is_some();
                    let found = e.sessions.iter().find(|s| s.id == term.session);
                    (found.map(|s| s.state), online)
                })
            };
            match session {
                Some((Some(SessionState::Exited { status }), _)) => break Some(status),
                Some((Some(SessionState::Running), _)) => seen = true,
                // Gone from a worker whose link is up: its terminal was closed.
                Some((None, true)) if seen || clock.elapsed() > grace => break None,
                _ => {}
            }
            tokio::select! {
                _ = head_seen.changed() => continue,
                _ = tick.tick() => {}
            }
            if let Outcome::Screen(screen) = self.forward(None, Verb::ReadScreen { term }).await {
                let last = screen.lines.iter().rev().map(|l| l.text.trim()).find(|l| !l.is_empty());
                let line = crate::project::clipped(last.unwrap_or_default(), PROGRESS_MAX);
                if line != shown {
                    shown.clone_from(&line);
                    let phase = if label.is_empty() { line } else { format!("{label}: {line}") };
                    self.progress(at, step(phase));
                }
            }
        };
        let took_ms = WallMs::now().millis_since(started);
        let screen = match self.forward(None, Verb::ReadScreen { term }).await {
            Outcome::Screen(screen) => {
                screen.lines.into_iter().map(|l| l.text).collect::<Vec<_>>().join("\n")
            }
            _ => String::new(),
        };
        let passed = exit == Some(0);
        let summary = if passed {
            last_line(&screen).unwrap_or_default().to_owned()
        } else {
            tail(screen.trim_end(), slopty_proto::project::SUMMARY_MAX)
        };
        if passed
            && let failed @ Outcome::Error { .. } = self.forward(None, Verb::Close { term }).await
        {
            tracing::debug!(?failed, "a passed verifier's terminal not closed");
        }
        let Commits { head, base } = commits;
        Ran::Judged(VerifierRun { passed, summary, head, base, exit, took_ms }, term)
    }

    /// Verify `task`'s branch as it is: a pass puts it in the queue, a failure gives it back
    /// to its agent.
    async fn verify_job(&self, project: &ProjectId, task: TaskId) -> Went {
        let at = (project, task);
        let place = match self.lane_place(project, task) {
            Ok(place) => place,
            Err(why) => {
                self.give_back(at, StepKind::Verify, None, &why, None);
                return Went::Next;
            }
        };
        if place.verifier.is_none() {
            // Its verifier was taken away while it waited: it is ready to merge, or in the
            // queue when the person's Merge said so.
            let advance =
                Advance { state: Some(TaskState::Done), merge: Queue::Keep, ..Advance::default() };
            self.advance(at, advance);
            return Went::Next;
        }
        // A verifier a restart of the server left running is followed as it is, not run again.
        let left = self.inner.state.lock().steps.left_running(&(project.clone(), task));
        let ran = if let Some((term, commits, since)) = left {
            self.watch_verifier(at, term, commits, (StepKind::Verify, ""), since).await
        } else {
            self.close_kept(at).await;
            let head = place.head.clone();
            self.run_verifier(at, &place, &head, (StepKind::Verify, "")).await
        };
        let (run, term) = match ran {
            Ran::Judged(run, term) => (run, term),
            Ran::Held(why) => {
                self.held(at, StepKind::Verify, place.worker, why);
                return Went::Hold;
            }
            Ran::Refused(why) => {
                self.give_back(at, StepKind::Verify, Some(place.worker), &why, None);
                return Went::Next;
            }
        };
        if run.passed {
            let step = TaskStep {
                kind: StepKind::Verify,
                worker: place.worker,
                state: StepState::Done { detail: format!("passed at {}", short(&run.head)) },
                since_ms: WallMs::now(),
                term: None,
                commits: None,
            };
            // Done, it is ready to merge, or in the queue when the person's Merge said so.
            let advance = Advance {
                state: Some(TaskState::Done),
                step: Some(step),
                merge: Queue::Keep,
                moment: Some(Moment::Verified(run.clone())),
                verified: Some(run),
                ..Advance::default()
            };
            self.advance(at, advance);
        } else {
            let why = last_line(&run.summary).map_or_else(|| exit_words(run.exit), str::to_owned);
            let words = verifier_words(&place, &run, None);
            let told = Told { run: Some((run, term)), words };
            self.give_back(at, StepKind::Verify, Some(place.worker), &why, Some(told));
        }
        Went::Next
    }

    /// Merge the task at the head of the queue: rebase its verified work onto the target with
    /// its provenance on every commit, verify what that made unless its tree is what was
    /// verified, and fast-forward the target.
    async fn merge_job(&self, project: &ProjectId, task: TaskId) -> Went {
        let at = (project, task);
        let place = match self.lane_place(project, task) {
            Ok(place) => place,
            Err(why) => {
                self.give_back(at, StepKind::Merge, None, &why, None);
                return Went::Next;
            }
        };
        let (verified, trailers) = {
            let state = self.inner.state.lock();
            let t = state.projects.task(project, task).ok();
            let thread = t
                .and_then(|t| t.assignment.as_ref())
                .and_then(|a| a.thread.or_else(|| state.board.thread_at(a.term)));
            let read = (t.and_then(|t| t.verified.clone()), provenance(project, task, thread));
            drop(state);
            read
        };
        let mut candidate = match (&place.verifier, verified) {
            (Some(_), Some(run)) if run.passed => run.head,
            (Some(_), _) => {
                // Queued without a pass to merge: it is verified first, and keeps the
                // person's Merge.
                let advance = Advance {
                    state: Some(TaskState::Verifying),
                    merge: Queue::Keep,
                    ..Advance::default()
                };
                self.advance(at, advance);
                return Went::Next;
            }
            (None, _) => place.head.clone(),
        };
        self.close_kept(at).await;
        let since_ms = WallMs::now();
        let step = |state: StepState| TaskStep {
            kind: StepKind::Merge,
            worker: place.worker,
            state,
            since_ms,
            term: None,
            commits: None,
        };
        let running = |phase: String| step(StepState::Running { phase, percent: None });
        for _ in 0..MERGE_TRIES {
            self.progress(at, running(format!("Rebasing onto {}", place.target)));
            let judged = {
                let state = self.inner.state.lock();
                state.projects.task(project, task).ok().and_then(|t| t.verified.clone())
            };
            let passed = judged.filter(|r| r.passed).map(|r| r.head);
            let rebase = Verb::Rebase {
                worker: place.worker,
                repo: place.clone.clone(),
                worktree: place.worktree.clone(),
                head: candidate.clone(),
                onto: place.target.clone(),
                trailers: trailers.clone(),
                verified: passed.clone(),
            };
            let (rebased, onto, same_tree) = match self.forward(None, rebase).await {
                Outcome::Rebased { head, onto, verified } => (head, onto, verified),
                Outcome::Error { code: ErrorCode::Conflict, message } => {
                    let onto = onto_words(&place.target, &self.send_target(at, place.worker).await);
                    let told = format!(
                        "Your work at {} does not rebase onto {}: {message}. {onto}, resolve \
                         the conflicts, commit, and report done again with task_report.",
                        short(&candidate),
                        place.target,
                    );
                    let told = Told { run: None, words: told };
                    // Its own step, so the board offers the person "Resolve conflicts".
                    self.give_back(at, StepKind::Rebase, Some(place.worker), &message, Some(told));
                    return Went::Next;
                }
                other => {
                    self.held(at, StepKind::Merge, place.worker, said(&other));
                    return Went::Hold;
                }
            };
            // A rebase that changed only the commits' messages left the tree verified.
            let already = passed.is_some_and(|head| head == rebased || same_tree);
            if place.verifier.is_some() && !already {
                let label = format!("Verifying on {} at {}", place.target, short(&onto));
                match self.run_verifier(at, &place, &rebased, (StepKind::Merge, &label)).await {
                    Ran::Judged(run, _) if run.passed => {
                        let moment = Some(Moment::Verified(run.clone()));
                        let advance = Advance { verified: Some(run), moment, ..Advance::default() };
                        self.advance(at, advance);
                    }
                    Ran::Judged(run, term) => {
                        let why = format!(
                            "the verifier failed on it rebased onto {} at {}",
                            place.target,
                            short(&onto)
                        );
                        let sent = self.send_target(at, place.worker).await;
                        let next = onto_words(&place.target, &sent);
                        let words = verifier_words(&place, &run, Some((&onto, &next)));
                        let told = Told { run: Some((run, term)), words };
                        self.give_back(at, StepKind::Merge, Some(place.worker), &why, Some(told));
                        return Went::Next;
                    }
                    Ran::Held(why) | Ran::Refused(why) => {
                        self.held(at, StepKind::Merge, place.worker, why);
                        return Went::Hold;
                    }
                }
            }
            self.progress(at, running(format!("Fast-forwarding {}", place.target)));
            let forward = Verb::FastForward {
                worker: place.worker,
                repo: place.clone.clone(),
                target: place.target.clone(),
                from: onto.clone(),
                to: rebased.clone(),
                push: place.push,
            };
            match self.forward(None, forward).await {
                Outcome::FastForwarded { head, pushed, push_failed } => {
                    self.merged(at, &place, &head, (pushed, push_failed), step);
                    return Went::Next;
                }
                // The target moved since the rebase: the rebased work goes on top of it.
                Outcome::Error { code: ErrorCode::Conflict, .. } => candidate = rebased,
                other => {
                    self.held(at, StepKind::Merge, place.worker, said(&other));
                    return Went::Hold;
                }
            }
        }
        let why = format!("{} kept moving while the queue merged", place.target);
        self.held(at, StepKind::Merge, place.worker, why);
        Went::Hold
    }
}

/// What a task given back carries beyond its reason: the verifier's run and the terminal it
/// is kept in, and the words its agent reads.
pub(super) struct Told {
    pub run: Option<(VerifierRun, TermRef)>,
    pub words: String,
}

impl Hub {
    /// Give `task` back to its agent: out of the queue, its step failed with `why` (on
    /// `worker`, else the orchestrator's), and its agent and the orchestrator told. It waits
    /// at its agent's prompt when that still runs, and is up next otherwise.
    ///
    /// Past [`GIVE_BACKS_MAX`] give-backs the failure is held for the person instead: its
    /// agent is not told, and the task waits on them until they say what next.
    pub(super) fn give_back(
        &self,
        (project, task): (&ProjectId, TaskId),
        kind: StepKind,
        worker: Option<WorkerId>,
        why: &str,
        told: Option<Told>,
    ) {
        let mut state = self.inner.state.lock();
        let (Ok(record), Ok(t)) =
            (state.projects.project(project), state.projects.task(project, task))
        else {
            return;
        };
        let live = t.assignment.as_ref().is_some_and(slopty_proto::project::Assignment::open);
        let worker = worker
            .or_else(|| record.orchestrator.map(|o| o.worker))
            .or_else(|| t.assignment.as_ref().map(|a| a.term.worker));
        let target = record.target.clone();
        let (run, words) = match told {
            Some(Told { run, words }) => (run, words),
            None => (None, format!("Your work could not be verified or merged: {why}.")),
        };
        let mut give_backs = t.give_backs;
        let held = !give_backs.room();
        if held {
            give_backs.held = true;
        } else {
            give_backs.count = give_backs.count.saturating_add(1);
        }
        let tests = t.tests.as_ref().map(TestDiff::line);
        let term = run.as_ref().map(|(_, term)| *term);
        let step = worker.map(|worker| TaskStep {
            kind,
            worker,
            state: StepState::Failed { why: why.to_owned() },
            since_ms: WallMs::now(),
            term,
            commits: None,
        });
        let verified = run.map(|(run, _)| run);
        let moment = match (&verified, &step) {
            (Some(run), _) => Some(Moment::Verified(run.clone())),
            (None, Some(step)) => Some(Moment::Step(step.clone())),
            (None, None) => None,
        };
        let to = if live { TaskState::Waiting } else { TaskState::Planned };
        let advance = Advance {
            state: Some(to),
            step,
            verified,
            merge: Queue::Leave,
            moment,
            fresh: false,
            give_backs: Some(give_backs),
        };
        match state.projects.advance(project, task, advance, WallMs::now()) {
            Ok((_, updates)) => self.projects_moved(&mut state, updates),
            Err(refused) => tracing::debug!(%project, %task, ?refused, "not given back"),
        }
        let at = tokio::time::Instant::now();
        if held {
            let above = format!(
                "task {task} failed again: {why}. It was given back {} times, as often as it may, \
                 so it waits on the person now and its agent was not told; leave it to them.",
                give_backs.count
            );
            state.deliveries.notice((project.clone(), None), task, Kind::Stuck, &above, at);
            drop(state);
            self.inner.deliver.notify_one();
            return;
        }
        let mut own = words;
        if let Some(tests) = &tests {
            own.push('\n');
            own.push_str(tests);
        }
        let own = format!("{own}\nThe task is back with you; {target} has not moved for it.");
        state.deliveries.notice((project.clone(), Some(task)), task, Kind::NeedsInput, &own, at);
        let next = if live {
            "its agent was told"
        } else {
            "nothing runs for it now; task_start starts it again"
        };
        let count = give_backs.count;
        let above = format!(
            "task {task} was given back (give-back {count} of {GIVE_BACKS_MAX}): {why}; {next}."
        );
        state.deliveries.notice((project.clone(), None), task, Kind::Stuck, &above, at);
        drop(state);
        self.inner.deliver.notify_one();
    }

    /// The lane stops on `task` for a reason that is not its own: its step says why, and it
    /// keeps its place.
    pub(super) fn held(
        &self,
        at: (&ProjectId, TaskId),
        kind: StepKind,
        worker: WorkerId,
        why: String,
    ) {
        let step = TaskStep {
            kind,
            worker,
            state: StepState::Failed { why },
            since_ms: WallMs::now(),
            term: None,
            commits: None,
        };
        let moment = Some(Moment::Step(step.clone()));
        self.advance(at, Advance { step: Some(step), moment, ..Advance::default() });
    }

    /// `task`'s work is on the target at `head`. The orchestrator hears. A task reported done
    /// again while it merged keeps its new state, and its newer work goes through again.
    fn merged(
        &self,
        (project, task): (&ProjectId, TaskId),
        place: &Place,
        head: &str,
        (pushed, push_failed): (bool, Option<String>),
        step: impl Fn(StepState) -> TaskStep,
    ) {
        let mut detail = format!("{} at {}", place.target, short(head));
        if pushed {
            detail.push_str(", pushed to origin");
        }
        if let Some(why) = &push_failed {
            detail.push_str(", not pushed: ");
            detail.push_str(why);
        }
        let done = step(StepState::Done { detail });
        let mut state = self.inner.state.lock();
        let Ok(t) = state.projects.task(project, task) else { return };
        let still =
            t.state == TaskState::Done && t.merge.as_ref().is_some_and(|m| m.queued().is_some());
        let now = WallMs::now();
        let merge = Merge::Merged {
            target: place.target.clone(),
            head: head.to_owned(),
            at_ms: now,
            pushed,
            push_failed: push_failed.clone(),
        };
        let advance = Advance {
            state: still.then_some(TaskState::Merged),
            moment: Some(Moment::Step(done.clone())),
            step: Some(done),
            merge: if still { Queue::Set(merge) } else { Queue::Keep },
            ..Advance::default()
        };
        match state.projects.advance(project, task, advance, now) {
            Ok((_, updates)) => self.projects_moved(&mut state, updates),
            Err(refused) => tracing::debug!(%project, %task, ?refused, "a merge not recorded"),
        }
        let words = match &push_failed {
            Some(why) => format!(
                "task {task} merged into {} at {}, but the push to origin failed: {why}. The \
                 person pushes again from the board.",
                place.target,
                short(head)
            ),
            None => format!("task {task} merged into {} at {}.", place.target, short(head)),
        };
        let at = tokio::time::Instant::now();
        // At once: a merge is final, and what depends on the task can start now.
        let kind = Kind::NeedsInput;
        state.deliveries.notice((project.clone(), None), task, kind, &words, at);
        drop(state);
        self.inner.deliver.notify_one();
    }

    /// The person pushes `task`'s target to `origin` again after the push that went with its
    /// merge failed ([`Verb::TaskPush`]). The target goes as it is in the orchestrator's clone:
    /// the merge put the task's work there, and what the queue merged since went on top of it.
    pub(super) async fn task_push(
        &self,
        caller: Caller,
        (project, task): (&ProjectId, TaskId),
    ) -> Outcome {
        if caller == Caller::Agent {
            return error(
                ErrorCode::Forbidden,
                "whether merged work is pushed to the forge is the person's choice, so only the \
                 person pushes it again",
            );
        }
        let card = {
            let state = self.inner.state.lock();
            state.projects.task(project, task).cloned()
        };
        let card = match card {
            Ok(card) => card,
            Err(refused) => return error(ErrorCode::Invalid, &said(&refused)),
        };
        let Some(Merge::Merged { target, head, at_ms, pushed, push_failed }) = card.merge.clone()
        else {
            return error(ErrorCode::Invalid, &format!("task {task} is not merged"));
        };
        if push_failed.is_none() {
            return Outcome::Task(Box::new(card));
        }
        let place = match self.lane_place(project, task) {
            Ok(place) => place,
            Err(why) => return error(ErrorCode::Invalid, &why),
        };
        let at = (project, task);
        let worker = place.worker;
        let step = |state| TaskStep {
            kind: StepKind::Merge,
            worker,
            state,
            since_ms: WallMs::now(),
            term: None,
            commits: None,
        };
        let phase = format!("Pushing {target} to origin");
        self.progress(at, step(StepState::Running { phase, percent: None }));
        let branch = format!("refs/heads/{target}");
        let push = Verb::FastForward {
            worker,
            repo: place.clone,
            target: target.clone(),
            from: branch.clone(),
            to: branch,
            push: true,
        };
        let (now_pushed, now_failed, detail) = match self.forward(None, push).await {
            Outcome::FastForwarded { head: now, pushed: true, .. } => {
                (true, None, format!("{target} at {} pushed to origin", short(&now)))
            }
            Outcome::FastForwarded { push_failed, .. } => {
                let why = push_failed.unwrap_or_else(|| "git did not push".to_owned());
                (pushed, Some(why.clone()), format!("{target} not pushed: {why}"))
            }
            other => {
                let why = said(&other);
                (pushed, Some(why.clone()), format!("{target} not pushed: {why}"))
            }
        };
        let done = match &now_failed {
            None => step(StepState::Done { detail }),
            Some(_) => step(StepState::Failed { why: detail }),
        };
        let merge =
            Merge::Merged { target, head, at_ms, pushed: now_pushed, push_failed: now_failed };
        let advance = Advance {
            moment: Some(Moment::Step(done.clone())),
            step: Some(done),
            merge: Queue::Set(merge),
            ..Advance::default()
        };
        match self.advance(at, advance) {
            Some(card) => Outcome::Task(Box::new(card)),
            None => error(ErrorCode::Invalid, &format!("task {task} is not there any more")),
        }
    }
}

/// What a failed verifier tells the agent whose work it judged: what ran, on which commits,
/// how it ended and its last lines, and what to do.
fn verifier_words(place: &Place, run: &VerifierRun, rebased: Option<(&str, &str)>) -> String {
    let command = place.verifier.as_deref().unwrap_or_default();
    let on = match rebased {
        Some((onto, _)) => format!(
            "your work rebased onto {} at {} (as {})",
            place.target,
            short(onto),
            short(&run.head)
        ),
        None => format!(
            "your branch at {}, which left {} at {}",
            short(&run.head),
            place.target,
            short(&run.base)
        ),
    };
    let lines = run.summary.lines().fold(String::new(), |mut all, line| {
        all.push_str("\n    ");
        all.push_str(line);
        all
    });
    let fix = match rebased {
        Some((_, next)) => format!("{next}, fix it"),
        None => "Fix it".to_owned(),
    };
    format!(
        "The verifier `{command}` failed on {on}: {} after {}. Its last lines:{lines}\n{fix}, \
         commit, and report done again with task_report; the server verifies the new head.",
        exit_words(run.exit),
        took(run.took_ms)
    )
}

/// Where a task given back finds the target to rebase onto, as the start of what its agent
/// is told to do.
fn onto_words(target: &str, onto: &Onto) -> String {
    match onto {
        Onto::Here => format!("Rebase it onto {target} as it is now"),
        Onto::Sent { branch, head } => format!(
            "{target} as the queue has it is in your clone as {branch} at {}: rebase onto \
             {branch}",
            short(head)
        ),
        Onto::Forge => format!("Fetch origin and rebase onto origin/{target}"),
        Onto::Failed(why) => format!(
            "{target} could not be sent to your clone ({why}): fetch origin and rebase onto \
             origin/{target}, which may lack what the queue merged since"
        ),
    }
}

/// The trailers every commit of `task`'s work carries once merged: the task, and the thread
/// whose agent wrote it when that is known.
pub(super) fn provenance(
    project: &ProjectId,
    task: TaskId,
    thread: Option<ThreadId>,
) -> Vec<(String, String)> {
    let mut trailers = vec![(TASK_TRAILER.to_owned(), format!("{project}#{task}"))];
    if let Some(thread) = thread {
        trailers.push((THREAD_TRAILER.to_owned(), thread.to_string()));
    }
    trailers
}

/// The paths a project's metadata says hold tests beside the usual ones ([`TEST_PATHS_KEY`]).
fn test_paths(metadata: Option<&str>) -> Vec<String> {
    let doc = metadata.and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok());
    let paths = doc.as_ref().and_then(|d| d.get(TEST_PATHS_KEY)?.as_array().cloned());
    paths
        .unwrap_or_default()
        .iter()
        .filter_map(|p| p.as_str().map(str::to_owned))
        .take(TEST_PATHS_MAX)
        .collect()
}

/// `exit` as a failure says it.
fn exit_words(exit: Option<i32>) -> String {
    match exit {
        Some(code) if code < 0 => format!("ended by signal {}", code.unsigned_abs()),
        Some(code) => format!("exit {code}"),
        None => "its terminal was closed before it ended".to_owned(),
    }
}

#[cfg(test)]
pub(super) mod tests;
