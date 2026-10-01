//! A fresh-context reviewer for each task's work before it merges (`docs/decisions/projects.md`,
//! "A reviewer reads the work with fresh eyes before it merges").
//!
//! Once a task's verifier passes, or at once when it has none, a project with a reviewer's
//! brief has the lane start a reviewer: a Claude Code session of its own on the orchestrator's
//! worker, in a checkout of the verified head with the diff from where the work left the
//! target beside it ([`Verb::ReviewCheckout`]). It knows the task's brief and nothing of the
//! conversation that wrote the work, and it cannot edit. It runs beside the lane, which goes on
//! with other tasks, and the person can open it like any agent. It answers with Slopty's
//! `review_report` tool ([`Verb::TaskReview`]): an approval puts the task in the merge queue,
//! and changes asked give it back to its agent with the findings, through its hooks. The
//! person may say either over it, at any time.

use std::collections::HashMap;

use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::agent::AgentKind;
use slopty_proto::orchestration::{ErrorCode, Outcome, TermRef, Verb};
use slopty_proto::project::{
    Merge, Moment, PROJECT_ENV, ProjectId, REVIEW_DIFF, ReviewRun, ReviewVerdict, Reviewer,
    StepKind, StepState, TASK_ENV, Task, TaskId, TaskState, TaskStep,
};

use super::projects::{fleet_room, live, started_args};
use super::queue::{Place, Told, Went, short, unreachable};
use super::steps::said;
use super::{Hub, State, error};
use crate::deliver::plain;
use crate::project::{Advance, Caller, Queue, Running, bounded_review};

/// The tools a reviewer goes without: it reads, and changes nothing.
const READ_ONLY: &str = "Edit,Write,NotebookEdit";

/// A reviewer at work: its terminal and the commits it reads.
#[derive(Clone, Debug)]
pub(super) struct Reading {
    term: TermRef,
    head: String,
    base: String,
    since: tokio::time::Instant,
}

impl Reading {
    /// Its terminal.
    pub(super) const fn term(&self) -> TermRef {
        self.term
    }
}

/// The reviewers at work, by the task each reads. Not kept across a restart: a review under
/// way then is marked as stopped on load, and the person settles it.
pub(super) type Reviews = HashMap<(ProjectId, TaskId), Reading>;

impl Hub {
    /// Start a reviewer on `task`'s work: the commit its verifier passed, else its branch as
    /// it is. The lane goes on at once; the verdict comes later.
    pub(super) async fn review_job(&self, project: &ProjectId, task: TaskId) -> Went {
        let at = (project, task);
        let place = match self.lane_place(project, task) {
            Ok(place) => place,
            Err(why) => {
                self.give_back(at, StepKind::Review, None, &why, None);
                return Went::Next;
            }
        };
        let step = |state: StepState, term: Option<TermRef>| TaskStep {
            kind: StepKind::Review,
            worker: place.worker,
            state,
            since_ms: WallMs::now(),
            term,
        };
        if let Err(why) = self.room_for_a_reviewer(project, place.worker) {
            let waiting = format!("Waiting to start: {why}");
            self.progress(at, step(StepState::Running { phase: waiting, percent: None }, None));
            return Went::Hold;
        }
        self.close_kept(at).await;
        let verified = {
            let state = self.inner.state.lock();
            state.projects.task(project, task).ok().and_then(|t| t.verified.clone())
        };
        let head = verified.filter(|r| r.passed).map_or_else(|| place.head.clone(), |r| r.head);
        let checkout = Verb::ReviewCheckout {
            worker: place.worker,
            repo: place.clone.clone(),
            worktree: format!("{project}-review-{task}"),
            head,
            target: place.target.clone(),
        };
        let (path, head, base) = match self.forward(None, checkout).await {
            Outcome::CheckedOut { path, head, base } => (path, head, base),
            other if unreachable(&other) => {
                self.held(at, StepKind::Review, place.worker, said(&other));
                return Went::Hold;
            }
            other => {
                self.give_back(at, StepKind::Review, Some(place.worker), &said(&other), None);
                return Went::Next;
            }
        };
        let term = TermRef { worker: place.worker, session: SessionId::new() };
        let since = tokio::time::Instant::now();
        let reading = Reading { term, head: head.clone(), base: base.clone(), since };
        let (permission_flags, found) = {
            let mut state = self.inner.state.lock();
            state.reviews.insert((project.clone(), task), reading);
            let flags = state.projects.policy().bounds_for(Some(project)).permission_flags;
            let found = state
                .projects
                .project(project)
                .cloned()
                .and_then(|record| Ok((record, state.projects.task(project, task)?.clone())));
            drop(state);
            (flags, found)
        };
        let Ok((record, card)) = found else { return Went::Next };
        let (spawn, phase) = {
            let role = reviewer_role(&record, &card, &place, (&head, &base));
            let prompt = reviewer_prompt(&card, (&head, &base));
            let args = vec!["--disallowedTools".to_owned(), READ_ONLY.to_owned()];
            let (args, _conversation) = started_args(args, permission_flags, Some(role));
            let spawn = Verb::SpawnAgent {
                worker: place.worker,
                agent: AgentKind::ClaudeCode,
                cwd: path,
                prompt: Some(prompt),
                args,
                env: vec![
                    (PROJECT_ENV.to_owned(), project.to_string()),
                    (TASK_ENV.to_owned(), task.to_string()),
                ],
                size: None,
                session: Some(term.session),
                permission_flags,
            };
            let phase = format!("Reading {} over {}", short(&head), short(&base));
            (spawn, phase)
        };
        // On the task before it starts, so a verdict that comes at once finds it there.
        self.progress(at, step(StepState::Running { phase, percent: None }, Some(term)));
        match self.forward(None, spawn).await {
            Outcome::Opened(_) => {}
            other => {
                self.inner.state.lock().reviews.remove(&(project.clone(), task));
                let why = format!("The reviewer did not start: {}", said(&other));
                self.progress(at, step(StepState::Failed { why }, None));
            }
        }
        Went::Next
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

    /// Why no reviewer may start on `worker` now, when the person's bounds are reached.
    fn room_for_a_reviewer(&self, project: &ProjectId, worker: WorkerId) -> Result<(), String> {
        let mut state = self.inner.state.lock();
        let (terminals, agents) = live(&mut state);
        let starting = state.starting.clone();
        let running = Running { terminals: &terminals, agents: &agents, starting: &starting };
        let bounds = state.projects.policy().bounds_for(Some(project));
        let room = fleet_room(&state, &running, bounds, Some(worker));
        drop(state);
        room.map_err(|refused| said(&refused))
    }

    /// `task`'s reviewer, or the person, says whether its work may merge. An approval queues
    /// it; changes asked give it back to its agent with the findings, the reviewer's session
    /// kept for reading. The person's word ends a reviewer still at work.
    pub(super) fn task_review(
        &self,
        caller: Caller,
        from: Option<SessionId>,
        (project, task): (&ProjectId, TaskId),
        verdict: ReviewVerdict,
    ) -> Outcome {
        let key = (project.clone(), task);
        let mut state = self.inner.state.lock();
        let decided = review_of(&state, caller, from, (project, task), verdict);
        let kept_before = state
            .projects
            .task(project, task)
            .ok()
            .and_then(|t| t.step.as_ref().filter(|s| !s.running())?.term);
        if decided.is_ok() {
            state.reviews.remove(&key);
        }
        drop(state);
        let decided = match decided {
            Ok(decided) => decided,
            Err(refused) => return refused,
        };
        let Decided { run, worker, reviewer } = decided;
        // A reviewer the person spoke over, or one that approved, has nothing left to say, and
        // neither has a terminal kept from the word before, which this one replaces.
        if run.by == Reviewer::Person || run.verdict.approved {
            let mut ending: Vec<TermRef> = reviewer.into_iter().chain(kept_before).collect();
            ending.dedup();
            for term in ending {
                self.close_soon(term);
            }
        }
        let at = (project, task);
        if run.verdict.approved {
            let detail = match run.by {
                Reviewer::Agent(_) => format!("approved {}", short(&run.head)),
                Reviewer::Person => format!("approved {} by you", short(&run.head)),
            };
            let advance = Advance {
                state: Some(TaskState::Done),
                step: Some(TaskStep {
                    kind: StepKind::Review,
                    worker,
                    state: StepState::Done { detail },
                    since_ms: WallMs::now(),
                    term: None,
                }),
                merge: Queue::Set(Merge::Queued { since_ms: WallMs::now() }),
                moment: Some(Moment::Reviewed(run.clone())),
                reviewed: Some(run),
                ..Advance::default()
            };
            let moved = self.advance(at, advance);
            self.kick(&mut self.inner.state.lock(), project);
            return moved.map_or_else(
                || error(ErrorCode::Conflict, &format!("task {task} moved meanwhile")),
                |t| Outcome::Task(Box::new(t)),
            );
        }
        let why = run
            .blocking()
            .next()
            .map_or_else(|| plain(first_line(&run.verdict.summary)), finding_line);
        let kept = reviewer.filter(|_| run.by != Reviewer::Person);
        let words = review_words(&run);
        let told = Told { run: None, review: Some((run, kept)), words };
        self.give_back(at, StepKind::Review, Some(worker), &why, Some(told));
        let state = self.inner.state.lock();
        state
            .projects
            .task(project, task)
            .map_or_else(|refused| refused, |t| Outcome::Task(Box::new(t.clone())))
    }

    /// A terminal ended: a reviewer that ended with no verdict leaves its task waiting on the
    /// person, who approves, asks for changes, or has it read again with `task merge`.
    pub(super) fn reviewer_closed(&self, state: &mut State, term: TermRef) {
        let gone: Vec<(ProjectId, TaskId)> = state
            .reviews
            .iter()
            .filter(|(_, r)| r.term == term)
            .map(|(key, _)| key.clone())
            .collect();
        for key in gone {
            state.reviews.remove(&key);
            let step = TaskStep {
                kind: StepKind::Review,
                worker: term.worker,
                state: StepState::Failed { why: "The reviewer ended without a verdict".to_owned() },
                since_ms: WallMs::now(),
                term: None,
            };
            match state.projects.set_step(&key.0, key.1, step, WallMs::now()) {
                Ok(updates) => self.projects_moved(state, updates),
                Err(refused) => tracing::debug!(?key, ?refused, "a reviewer for no task"),
            }
        }
    }
}

/// A verdict taken: the review, the worker it is shown on, and the reviewer it ends.
struct Decided {
    run: ReviewRun,
    worker: WorkerId,
    reviewer: Option<TermRef>,
}

/// `verdict` on `task` from `caller`, as the review it records, when `caller` may give it:
/// the reviewer the server started for it, from its own terminal, or the person. The person
/// approves only work its verifier passed.
fn review_of(
    state: &State,
    caller: Caller,
    from: Option<SessionId>,
    (project, task): (&ProjectId, TaskId),
    verdict: ReviewVerdict,
) -> Result<Decided, Outcome> {
    let record = state.projects.project(project)?;
    let card: &Task = state.projects.task(project, task)?;
    let reading = state.reviews.get(&(project.clone(), task));
    let by = match (caller, reading) {
        (Caller::Agent, Some(r)) if from == Some(r.term.session) => Reviewer::Agent(r.term),
        (Caller::Agent, _) => {
            return Err(error(
                ErrorCode::Forbidden,
                &format!(
                    "only the reviewer the server started for task {task}, from its own \
                     terminal, or the person says whether its work may merge"
                ),
            ));
        }
        (Caller::Person, _) => Reviewer::Person,
    };
    if matches!(card.state, TaskState::Merged | TaskState::Done) {
        return Err(error(
            ErrorCode::Invalid,
            &format!(
                "task {task} is {} already",
                if card.state == TaskState::Done {
                    "approved and waiting to merge"
                } else {
                    "merged"
                }
            ),
        ));
    }
    let verifies = card.verifier.is_some() || record.verifier.is_some();
    let passed = card.verified.as_ref().filter(|r| r.passed);
    if verdict.approved && verifies && passed.is_none() {
        return Err(error(
            ErrorCode::Invalid,
            &format!(
                "task {task}'s verifier has not passed, and a review approves only verified \
                 work; `slopty task merge` runs it"
            ),
        ));
    }
    let (head, base) = match (reading, passed) {
        (Some(r), _) => (r.head.clone(), r.base.clone()),
        (None, Some(run)) => (run.head.clone(), run.base.clone()),
        (None, None) => (String::new(), String::new()),
    };
    let took_ms = match by {
        Reviewer::Agent(_) => {
            reading.map_or(0, |r| u64::try_from(r.since.elapsed().as_millis()).unwrap_or(u64::MAX))
        }
        Reviewer::Person => 0,
    };
    let worker = reading
        .map(|r| r.term.worker)
        .or_else(|| record.orchestrator.map(|o| o.worker))
        .or_else(|| card.assignment.as_ref().map(|a| a.term.worker))
        .unwrap_or_default();
    let run = bounded_review(ReviewRun { verdict, more: 0, head, base, by, took_ms });
    Ok(Decided { run, worker, reviewer: reading.map(|r| r.term) })
}

/// The first line of `text` that says anything.
fn first_line(text: &str) -> &str {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default()
}

/// A finding in a line: where, and what.
fn finding_line(f: &slopty_proto::project::Finding) -> String {
    let at = match (&f.path, f.line) {
        (Some(path), Some(line)) => format!("{path}:{line}: "),
        (Some(path), None) => format!("{path}: "),
        (None, _) => String::new(),
    };
    format!("{at}{}", plain(first_line(&f.body)))
}

/// What a review that asked for changes tells the agent whose work it read: who, at which
/// commits, what blocks and what else it noted, and what to do.
fn review_words(run: &ReviewRun) -> String {
    let who = match run.by {
        Reviewer::Agent(_) => "A reviewer with fresh context",
        Reviewer::Person => "The person",
    };
    let mut words = format!(
        "{who} read your work at {} (from {}) and asked for changes: {}",
        short(&run.head),
        short(&run.base),
        plain(run.verdict.summary.trim())
    );
    let (blocking, rest): (Vec<_>, Vec<_>) = run.verdict.findings.iter().partition(|f| f.blocking);
    for (title, list) in [("What blocks the merge:", blocking), ("Also noted:", rest)] {
        if list.is_empty() {
            continue;
        }
        words.push('\n');
        words.push_str(title);
        for f in list {
            words.push_str("\n    - ");
            words.push_str(&f.severity);
            words.push_str(", ");
            words.push_str(&finding_line(f));
        }
    }
    if run.more > 0 {
        words.push_str("\n    and ");
        words.push_str(&run.more.to_string());
        words.push_str(" more");
    }
    words.push_str(
        "\nFix what blocks, commit, and report done again with task_report; the server \
         verifies and reviews the new head.",
    );
    words
}

/// What a reviewer is told it is, on its system prompt: whose work, where it is, how to read
/// it, what counts, and how to answer.
fn reviewer_role(
    project: &slopty_proto::project::Project,
    task: &Task,
    place: &Place,
    (head, base): (&str, &str),
) -> String {
    let mut lines = vec![
        format!(
            "You review task {} (\"{}\") of the Slopty project {} before it merges onto {}. \
             You did not write this work and know nothing of how it was written: only its \
             brief, which is your first prompt, and its diff.",
            task.id,
            plain(&task.title),
            project.id,
            plain(&place.target)
        ),
        format!(
            "- The work is checked out here at {head}. {REVIEW_DIFF} holds `git diff \
             {base}..{head}`, everything it changes. Read it, and the files around it as you \
             need. Change nothing: you cannot edit, and git stays as it is."
        ),
        "- Look for what would be wrong to merge: a bug, a broken contract or invariant, a \
         security hole, lost data, a test the brief asked for that is missing. Check each \
         finding against the code before you report it, and leave out what you are not sure \
         of, matters of style, and anything the diff did not change."
            .to_owned(),
        "- Answer once, with Slopty's review_report tool: approved or not, a summary of a few \
         lines, and the findings that matter most first, each with its path, line, severity \
         (blocker, should or nit) and whether it blocks. Only a blocker blocks; approve unless \
         one does."
            .to_owned(),
        "- The person reads your verdict and may overrule it, and may open this session to ask \
         you about it."
            .to_owned(),
    ];
    if let Some(brief) = project.review.as_deref().filter(|b| !b.trim().is_empty()) {
        lines.push(format!("- The person also asks you to look for: {}", plain(brief)));
    }
    lines.join("\n")
}

/// A reviewer's first prompt: the task and its brief, and where the diff is.
fn reviewer_prompt(task: &Task, (head, base): (&str, &str)) -> String {
    let brief = match task.brief.trim() {
        "" => "(none was written)".to_owned(),
        brief => brief.to_owned(),
    };
    format!(
        "Review task {}: {}.\n\nIts brief:\n{brief}\n\nThe diff is in {REVIEW_DIFF} ({}..{}).",
        task.id,
        task.title,
        short(base),
        short(head)
    )
}

#[cfg(test)]
mod tests;
