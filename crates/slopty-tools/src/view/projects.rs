//! Projects: the record, the task tree with Claude Code's own subagents as its leaves, the
//! timeline, the workers' facts and where a task may run, as JSON for scripts and models and as
//! text for a person.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use serde::Serialize;
use serde_json::Value;
use slopty_core::{WallMs, WorkerId};
use slopty_proto::agent::{PullRequest, Review};
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::{
    Attempts, Bounds, Fact, Facts, Finding, Limits, Live, Moment, NativeCounts, Natives, Need,
    NodeDetail, Peer, Placement, Project, ProjectStatus, Report, ReportKind, ReviewRun, Reviewer,
    Schedule, StepKind, StepState, Suggestion, Task, TaskCard, TaskState, TaskStep, TimelineEntry,
    VerifierRun,
};
use slopty_proto::server::Os;

use super::term_string;

/// A task's state as JSON and text name it.
#[must_use]
pub const fn state_word(state: TaskState) -> &'static str {
    match state {
        TaskState::Planned => "planned",
        TaskState::Running => "running",
        TaskState::Waiting => "waiting",
        TaskState::Blocked => "blocked",
        TaskState::Verifying => "verifying",
        TaskState::Done => "done",
        TaskState::Merged => "merged",
        TaskState::Failed => "failed",
    }
}

/// The state a word names.
#[must_use]
pub fn state_named(word: &str) -> Option<TaskState> {
    [
        TaskState::Planned,
        TaskState::Running,
        TaskState::Waiting,
        TaskState::Blocked,
        TaskState::Verifying,
        TaskState::Done,
        TaskState::Merged,
        TaskState::Failed,
    ]
    .into_iter()
    .find(|s| state_word(*s) == word)
}

/// An operating system as facts name it.
#[must_use]
pub const fn os_word(os: Os) -> &'static str {
    match os {
        Os::MacOs => "macos",
        Os::Linux => "linux",
    }
}

/// The operating system a word names: `macos` or `linux`.
#[must_use]
pub fn os_named(word: &str) -> Option<Os> {
    match word.to_ascii_lowercase().as_str() {
        "macos" | "mac" => Some(Os::MacOs),
        "linux" => Some(Os::Linux),
        _ => None,
    }
}

/// A metadata document as JSON: the object it holds.
fn metadata(text: Option<&str>) -> Option<Value> {
    text.and_then(|t| serde_json::from_str(t).ok())
}

/// A fact as plain JSON.
#[must_use]
pub fn fact(f: &Fact) -> Value {
    match f {
        Fact::Bool(b) => Value::Bool(*b),
        Fact::Int(n) => Value::from(*n),
        Fact::Float(x) => serde_json::Number::from_f64(*x).map_or(Value::Null, Value::Number),
        Fact::Text(t) => Value::String(t.clone()),
        Fact::List(items) => Value::Array(items.iter().map(fact).collect()),
        Fact::Map(parts) => {
            Value::Object(parts.iter().map(|(k, v)| (k.clone(), fact(v))).collect())
        }
    }
}

/// A worker's facts as a plain JSON object.
#[must_use]
pub fn facts(f: &Facts) -> BTreeMap<&str, Value> {
    f.iter().map(|(k, v)| (k.as_str(), fact(v))).collect()
}

/// A project, for JSON.
#[derive(Debug, Serialize)]
pub struct ProjectView<'a> {
    project: &'a str,
    title: &'a str,
    repo: &'a str,
    target: &'a str,
    verifier: Option<&'a str>,
    orchestrator: Option<String>,
    limits: Limits,
    metadata: Option<Value>,
    /// What each kind of its work needs of its machines.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    needs: Vec<NeedView<'a>>,
    /// The tasks it runs on a schedule the person set.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    schedules: Vec<ScheduleView<'a>>,
    created_ms: WallMs,
}

/// A schedule, for JSON: what it makes, when, and how its last run went.
#[derive(Debug, Serialize)]
pub struct ScheduleView<'a> {
    schedule: u32,
    title: &'a str,
    when: &'a str,
    zone: &'a str,
    paused: bool,
    next_ms: Option<WallMs>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_ms: Option<WallMs>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_task: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_why: Option<&'a str>,
}

fn schedule(s: &Schedule) -> ScheduleView<'_> {
    let last = s.last.as_ref();
    ScheduleView {
        schedule: s.id,
        title: &s.spec.task.title,
        when: &s.spec.when,
        zone: &s.spec.zone,
        paused: s.spec.paused,
        next_ms: s.next_ms,
        last_ms: last.map(|l| l.at_ms),
        last_task: last.and_then(|l| l.task).map(|t| t.0),
        last_why: last.and_then(|l| l.why.as_deref()),
    }
}

/// A schedule on one line: its number, when, what, and how its last run went.
#[must_use]
pub fn schedule_text(s: &Schedule) -> String {
    let when = format!("{} {}", s.spec.when, s.spec.zone);
    let paused = if s.spec.paused { "  paused" } else { "" };
    let last = s.last.as_ref().map_or_else(String::new, |l| match (&l.why, l.task) {
        (Some(why), _) => format!("  last: {why}"),
        (None, Some(task)) => format!("  last: #{task}"),
        (None, None) => String::new(),
    });
    format!("schedule {} [{when}] {}{paused}{last}", s.id, s.spec.task.title)
}

/// A need, for JSON.
#[derive(Debug, Serialize)]
pub struct NeedView<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    paths: &'a [String],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    require: &'a [String],
    #[serde(skip_serializing_if = "Vec::is_empty")]
    prefer: Vec<PreferView<'a>>,
}

/// A project, for JSON.
#[must_use]
pub fn project(p: &Project) -> ProjectView<'_> {
    ProjectView {
        project: p.id.as_str(),
        title: &p.title,
        repo: &p.repo,
        target: &p.target,
        verifier: p.verifier.as_deref(),
        orchestrator: p.orchestrator.map(term_string),
        limits: p.limits.clone(),
        metadata: metadata(p.metadata.as_deref()),
        needs: p
            .needs
            .iter()
            .map(|n| NeedView {
                name: &n.name,
                paths: &n.paths,
                require: &n.require,
                prefer: n
                    .prefer
                    .iter()
                    .map(|p| PreferView { expr: &p.expr, weight: p.weight })
                    .collect(),
            })
            .collect(),
        schedules: p.schedules.iter().map(schedule).collect(),
        created_ms: p.created_ms,
    }
}

/// A task or a worker a task runs beside or away from: `#3`, or the worker's id.
fn peer(p: &Peer) -> String {
    match p {
        Peer::Task(t) => format!("#{t}"),
        Peer::Worker(w) => w.to_string(),
    }
}

/// One preference, for JSON.
#[derive(Debug, Serialize)]
pub struct PreferView<'a> {
    expr: &'a str,
    weight: i32,
}

/// A placement, for JSON.
#[derive(Debug, Serialize)]
pub struct PlacementView<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pin: Option<WorkerId>,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    require: &'a [String],
    #[serde(skip_serializing_if = "Vec::is_empty")]
    prefer: Vec<PreferView<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    near: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    avoid: Vec<String>,
}

/// A placement, for JSON.
#[must_use]
pub fn placement(p: &Placement) -> PlacementView<'_> {
    PlacementView {
        pin: p.pin,
        require: &p.require,
        prefer: p.prefer.iter().map(|x| PreferView { expr: &x.expr, weight: x.weight }).collect(),
        near: p.near.iter().map(peer).collect(),
        avoid: p.avoid.iter().map(peer).collect(),
    }
}

/// A pull request, for JSON.
#[derive(Debug, Serialize)]
pub struct PrView<'a> {
    number: u32,
    url: &'a str,
    review: Option<&'static str>,
}

/// A verifier's word, for JSON.
#[derive(Debug, Serialize)]
pub struct VerifiedView<'a> {
    passed: bool,
    summary: &'a str,
    head: &'a str,
    base: &'a str,
}

fn verified(v: &VerifierRun) -> VerifiedView<'_> {
    VerifiedView { passed: v.passed, summary: &v.summary, head: &v.head, base: &v.base }
}

/// One thing a reviewer found, for JSON.
#[derive(Debug, Serialize)]
pub struct FindingView<'a> {
    path: Option<&'a str>,
    line: Option<u32>,
    severity: &'a str,
    blocking: bool,
    body: &'a str,
}

/// A reviewer's word, for JSON: `more` counts the findings past those kept.
#[derive(Debug, Serialize)]
pub struct ReviewedView<'a> {
    approved: bool,
    by: String,
    summary: &'a str,
    findings: Vec<FindingView<'a>>,
    more: u16,
    head: &'a str,
    base: &'a str,
}

fn finding(f: &Finding) -> FindingView<'_> {
    FindingView {
        path: f.path.as_deref(),
        line: f.line,
        severity: &f.severity,
        blocking: f.blocking,
        body: &f.body,
    }
}

fn reviewed(r: &ReviewRun) -> ReviewedView<'_> {
    ReviewedView {
        approved: r.verdict.approved,
        by: match r.by {
            Reviewer::Agent(term) => term_string(term),
            Reviewer::Person => "person".to_owned(),
        },
        summary: &r.verdict.summary,
        findings: r.verdict.findings.iter().map(finding).collect(),
        more: r.more,
        head: &r.head,
        base: &r.base,
    }
}

/// A task, for JSON.
#[derive(Debug, Serialize)]
pub struct TaskView<'a> {
    task: u32,
    parent: Option<u32>,
    depends_on: Vec<u32>,
    kind: &'a str,
    title: &'a str,
    brief: &'a str,
    owns: &'a [String],
    read_only: bool,
    placement: PlacementView<'a>,
    verifier: Option<&'a str>,
    metadata: Option<Value>,
    state: &'static str,
    /// What its agent says it is doing.
    status: Option<&'a str>,
    /// Its terminal.
    term: Option<String>,
    /// Why its agent went to the worker it runs on, in the ranking's words.
    placed: Option<&'a str>,
    agent_since_ms: Option<WallMs>,
    agent_ended_ms: Option<WallMs>,
    branch: Option<&'a str>,
    worktree: Option<&'a str>,
    base: Option<&'a str>,
    pr: Option<PrView<'a>>,
    /// Its pull request's own checks, as its forge last said.
    checks: Option<ChecksView<'a>>,
    verified: Option<VerifiedView<'a>>,
    reviewed: Option<ReviewedView<'a>>,
    /// Its time at work, as on its card.
    active_ms: u64,
    at_work_since_ms: Option<WallMs>,
    /// Its start is proposed and waits for the person, who starts it from the board.
    proposed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    attempts: Option<AttemptsView>,
    created_ms: WallMs,
    updated_ms: WallMs,
}

/// The attempts at a task, for JSON: each a task of its own, and the one picked to land.
#[derive(Debug, Serialize)]
pub struct AttemptsView {
    tried: Vec<u32>,
    picked: Option<u32>,
}

fn attempts(a: &Attempts) -> AttemptsView {
    AttemptsView { tried: a.tried.iter().map(|t| t.0).collect(), picked: a.picked.map(|t| t.0) }
}

/// A task's line in the tree, for JSON: `task_get` has the rest.
#[derive(Debug, Serialize)]
pub struct CardView<'a> {
    task: u32,
    parent: Option<u32>,
    depends_on: Vec<u32>,
    kind: &'a str,
    title: &'a str,
    read_only: bool,
    state: &'static str,
    status: Option<&'a str>,
    term: Option<String>,
    /// Why its agent went to the worker it runs on.
    placed: Option<&'a str>,
    agent_since_ms: Option<WallMs>,
    agent_ended_ms: Option<WallMs>,
    branch: Option<&'a str>,
    worktree: Option<&'a str>,
    pr: Option<PrView<'a>>,
    /// Its pull request's own checks, as its forge last said.
    checks: Option<ChecksView<'a>>,
    verified: Option<VerifiedView<'a>>,
    reviewed: Option<ReviewedView<'a>>,
    /// Its time at work, idle waits left out: the stretches that ended, and when the one under
    /// way began.
    active_ms: u64,
    at_work_since_ms: Option<WallMs>,
    /// Its start is proposed and waits for the person.
    proposed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    attempts: Option<AttemptsView>,
    natives: NativeCounts,
    created_ms: WallMs,
    updated_ms: WallMs,
}

/// A task's line in the tree, for JSON.
#[must_use]
pub fn card(t: &TaskCard) -> CardView<'_> {
    CardView {
        task: t.id.0,
        parent: t.parent.map(|p| p.0),
        depends_on: t.depends_on.iter().map(|d| d.0).collect(),
        kind: &t.kind,
        title: &t.title,
        read_only: t.read_only,
        state: state_word(t.state),
        status: t.status.as_deref(),
        term: t.assignment.as_ref().map(|a| term_string(a.term)),
        placed: t.assignment.as_ref().and_then(|a| a.placed.as_ref()).map(|p| p.why.as_str()),
        agent_since_ms: t.assignment.as_ref().map(|a| a.since_ms),
        agent_ended_ms: t.assignment.as_ref().and_then(|a| a.ended_ms),
        branch: t.branch.as_deref(),
        worktree: t.worktree.as_deref(),
        pr: t.pr.as_ref().map(pr),
        checks: t.checks.as_ref().map(checks),
        verified: t.verified.as_ref().map(verified),
        reviewed: t.reviewed.as_ref().map(reviewed),
        active_ms: t.spent.active_ms,
        at_work_since_ms: t.spent.since_ms,
        proposed: t.proposed.is_some(),
        attempts: t.attempts.as_ref().map(attempts),
        natives: t.natives,
        created_ms: t.created_ms,
        updated_ms: t.updated_ms,
    }
}

const fn review(r: Review) -> &'static str {
    match r {
        Review::Approved => "approved",
        Review::Pending => "pending",
        Review::ChangesRequested => "changes_requested",
        Review::Draft => "draft",
    }
}

fn pr(pr: &PullRequest) -> PrView<'_> {
    PrView { number: pr.number, url: &pr.url, review: pr.review.map(review) }
}

/// A task, for JSON.
#[must_use]
pub fn task(t: &Task) -> TaskView<'_> {
    TaskView {
        task: t.id.0,
        parent: t.parent.map(|p| p.0),
        depends_on: t.depends_on.iter().map(|d| d.0).collect(),
        kind: &t.kind,
        title: &t.title,
        brief: &t.brief,
        owns: &t.owns,
        read_only: t.read_only,
        placement: placement(&t.placement),
        verifier: t.verifier.as_deref(),
        metadata: metadata(t.metadata.as_deref()),
        state: state_word(t.state),
        status: t.status.as_deref(),
        term: t.assignment.as_ref().map(|a| term_string(a.term)),
        placed: t.assignment.as_ref().and_then(|a| a.placed.as_ref()).map(|p| p.why.as_str()),
        agent_since_ms: t.assignment.as_ref().map(|a| a.since_ms),
        agent_ended_ms: t.assignment.as_ref().and_then(|a| a.ended_ms),
        branch: t.branch.as_deref(),
        worktree: t.worktree.as_deref(),
        base: t.base.as_deref(),
        pr: t.pr.as_ref().map(pr),
        checks: t.checks.as_ref().map(checks),
        verified: t.verified.as_ref().map(verified),
        reviewed: t.reviewed.as_ref().map(reviewed),
        active_ms: t.spent.active_ms,
        at_work_since_ms: t.spent.since_ms,
        proposed: t.proposal.is_some(),
        attempts: t.attempts.as_ref().map(attempts),
        created_ms: t.created_ms,
        updated_ms: t.updated_ms,
    }
}

/// One node in full, for JSON: the task (none for the orchestrator's node) and its natives.
#[derive(Debug, Serialize)]
pub struct NodeView<'a> {
    task: Option<TaskView<'a>>,
    natives: &'a Natives,
}

/// One node in full, for JSON.
#[must_use]
pub fn node(n: &NodeDetail) -> NodeView<'_> {
    NodeView { task: n.task.as_ref().map(task), natives: &n.natives }
}

/// A timeline entry, for JSON.
#[derive(Debug, Serialize)]
pub struct EntryView {
    seq: u64,
    at_ms: WallMs,
    task: Option<u32>,
    kind: &'static str,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    term: Option<String>,
}

/// A timeline entry, for JSON.
#[must_use]
pub fn entry(e: &TimelineEntry) -> EntryView {
    let (kind, text) = moment(&e.what);
    let term = match &e.what {
        Moment::Orchestrator { term }
        | Moment::Assigned { term, .. }
        | Moment::AgentGone { term }
        | Moment::Delivered { term, .. } => Some(term_string(*term)),
        _ => None,
    };
    EntryView { seq: e.seq, at_ms: e.at_ms, task: e.task.map(|t| t.0), kind, text, term }
}

/// A moment's kind and its words.
#[must_use]
pub fn moment(what: &Moment) -> (&'static str, String) {
    match what {
        Moment::Created => ("created", "project made".to_owned()),
        Moment::Orchestrator { .. } => ("orchestrator", "orchestrator named".to_owned()),
        Moment::Limits { limits } => ("limits", format!("limits now {}", limits_text(limits))),
        Moment::Budget { meter, share_bp } if *share_bp >= 10_000 => {
            ("budget", format!("budget reached on {meter}: no new work until it is raised"))
        }
        Moment::Budget { meter, share_bp } => {
            ("budget", format!("{}% of the {meter} budget spent", share_bp / 100))
        }
        Moment::TaskCreated { title } => ("task_created", format!("made: {title}")),
        Moment::Claimed { paths } => ("claimed", format!("owns {}", paths_text(paths))),
        Moment::Needs { names } if names.is_empty() => ("needs", "needs nothing".to_owned()),
        Moment::Needs { names } => ("needs", format!("needs: {}", names.join(", "))),
        Moment::Assigned { spawned: true, .. } => ("assigned", "started for it".to_owned()),
        Moment::Assigned { spawned: false, .. } => ("assigned", "terminal put on it".to_owned()),
        Moment::Proposed { .. } => ("proposed", "start proposed, waits for the person".to_owned()),
        Moment::State { from, to } => {
            ("state", format!("{} (was {})", state_word(*to), state_word(*from)))
        }
        Moment::Branch { branch, pr } => {
            let branch = branch.as_deref().unwrap_or("no branch");
            let text =
                pr.map_or_else(|| branch.to_owned(), |n| format!("{branch}, pull request #{n}"));
            ("branch", text)
        }
        Moment::Verified(run) => ("verified", verified_text(run)),
        Moment::Checks(checks) => ("checks", checks_text(checks)),
        Moment::Reviewed(run) => ("reviewed", reviewed_text(run)),
        Moment::AgentGone { .. } => ("agent_gone", "its terminal closed".to_owned()),
        Moment::Note { text } => ("note", text.clone()),
        Moment::Told { text } => ("told", format!("the person told its agent: {text}")),
        Moment::Reported { report } => ("reported", report_text(report)),
        Moment::Delivered { reports, .. } => {
            ("delivered", format!("{reports} report(s) handed to its agent"))
        }
        Moment::Step(step) => ("step", step_text(step)),
    }
}

/// A pull request's own checks, for JSON.
#[derive(Debug, Serialize)]
pub struct ChecksView<'a> {
    /// `none`, `pending`, `passing` or `failing`.
    state: &'static str,
    passed: u16,
    failed: u16,
    pending: u16,
    skipped: u16,
    /// The first failing checks, by name.
    failing: &'a [String],
    at_ms: WallMs,
}

fn checks(c: &slopty_proto::project::Checks) -> ChecksView<'_> {
    use slopty_proto::project::ChecksState;
    let state = match c.state {
        ChecksState::None => "none",
        ChecksState::Pending => "pending",
        ChecksState::Passing => "passing",
        ChecksState::Failing => "failing",
        ChecksState::Unknown => "unknown",
    };
    ChecksView {
        state,
        passed: c.passed,
        failed: c.failed,
        pending: c.pending,
        skipped: c.skipped,
        failing: &c.failing,
        at_ms: c.at_ms,
    }
}

/// A pull request's checks, in a line.
fn checks_text(checks: &slopty_proto::project::Checks) -> String {
    use slopty_proto::project::ChecksState;
    let counts = format!(
        "{} passed, {} failed, {} pending, {} skipped",
        checks.passed, checks.failed, checks.pending, checks.skipped
    );
    match checks.state {
        ChecksState::None => "its pull request has no checks".to_owned(),
        ChecksState::Pending => format!("its pull request's checks run: {counts}"),
        ChecksState::Passing => format!("its pull request's checks pass: {counts}"),
        ChecksState::Failing => {
            format!("its pull request's checks fail ({}): {counts}", checks.failing.join(", "))
        }
        ChecksState::Unknown => format!(
            "its pull request's checks could not be read: {}",
            checks.why.as_deref().unwrap_or("the forge did not answer")
        ),
    }
}

/// A step the server took for a task, in a line.
fn step_text(step: &TaskStep) -> String {
    let what = match step.kind {
        StepKind::Clone => format!("clone on worker {}", step.worker),
        StepKind::Home => format!("branch brought to worker {}", step.worker),
        StepKind::Verify => format!("verifier on worker {}", step.worker),
        StepKind::Merge => format!("merge on worker {}", step.worker),
        StepKind::Review => format!("review on worker {}", step.worker),
        StepKind::Rebase => format!("rebase onto the target on worker {}", step.worker),
    };
    match &step.state {
        StepState::Running { phase, percent: Some(p) } => format!("{what}: {phase} {p}%"),
        StepState::Running { phase, percent: None } => format!("{what}: {phase}"),
        StepState::Done { detail } => format!("{what} done: {detail}"),
        StepState::Failed { why } => format!("{what} failed: {why}"),
    }
}

/// What a review said, by whom and at which commits.
fn reviewed_text(run: &ReviewRun) -> String {
    let word = if run.verdict.approved { "approved" } else { "changes asked" };
    let by = match run.by {
        Reviewer::Agent(_) => "the reviewer",
        Reviewer::Person => "the person",
    };
    let short = |c: &str| c.get(..7).unwrap_or(c).to_owned();
    let blocking = run.blocking().count();
    let found = run.verdict.findings.len().saturating_add(usize::from(run.more));
    format!(
        "{word} by {by} at {} over {}: {found} finding(s), {blocking} blocking",
        short(&run.head),
        short(&run.base)
    )
}

/// What a verifier said, at which commits.
fn verified_text(run: &VerifierRun) -> String {
    let short = |c: &str| c.get(..7).unwrap_or(c).to_owned();
    let word = if run.passed { "passed" } else { "failed" };
    let exit = run.exit.filter(|_| !run.passed).map(|e| format!(", exit {e}")).unwrap_or_default();
    let at = format!("{} on {}", short(&run.head), short(&run.base));
    format!("verifier {word} at {at}{exit}: {}", run.summary)
}

/// A report in a line: its kind, its note's first line, where its work is.
fn report_text(report: &Report) -> String {
    let kind = report_word(report.kind);
    let note = report.note.lines().next().unwrap_or_default().trim();
    let mut text = if note.is_empty() { kind.to_owned() } else { format!("{kind}: {note}") };
    if let Some(branch) = &report.branch {
        text = format!("{text} ({branch})");
    }
    text
}

/// A report kind as the tools spell it.
#[must_use]
pub const fn report_word(kind: ReportKind) -> &'static str {
    match kind {
        ReportKind::Checkpoint => "checkpoint",
        ReportKind::NeedsInput => "needs_input",
        ReportKind::Stuck => "stuck",
        ReportKind::Done => "done",
    }
}

fn limits_text(l: &Limits) -> String {
    let budget = l.budget.as_ref().map_or_else(String::new, |b| {
        let caps: Vec<String> =
            b.0.iter()
                .map(|(meter, cap)| {
                    format!("{meter} {}", slopty_proto::project::Budget::figure(meter, *cap))
                })
                .collect();
        format!(", budget {}", caps.join(", "))
    });
    format!(
        "{} agents per worker, {} in all, {} deep, {} entries kept{budget}",
        l.live_per_worker, l.live_per_project, l.depth, l.timeline_kept
    )
}

fn paths_text(paths: &[String]) -> String {
    let shown: Vec<&str> =
        paths.iter().map(|p| if p.is_empty() { "." } else { p.as_str() }).collect();
    shown.join(", ")
}

/// A project's tree, for JSON.
#[derive(Debug, Serialize)]
pub struct StatusView<'a> {
    project: ProjectView<'a>,
    tasks: Vec<CardView<'a>>,
    orchestrator_natives: NativeCounts,
    bounds: Bounds,
    live: Live,
    timeline: Vec<EntryView>,
    next: u64,
}

/// A project's tree, for JSON.
#[must_use]
pub fn status(s: &ProjectStatus) -> StatusView<'_> {
    StatusView {
        project: project(&s.project),
        tasks: s.tasks.iter().map(card).collect(),
        orchestrator_natives: s.orchestrator_natives,
        bounds: s.bounds,
        live: s.live,
        timeline: s.timeline.iter().map(entry).collect(),
        next: s.next,
    }
}

/// What `task_wait` saw, for JSON.
#[derive(Debug, Serialize)]
pub struct WaitView<'a> {
    /// The time ran out first; nothing was stopped or cancelled for it.
    timed_out: bool,
    /// The tasks with news, or merged or given up when the wait began.
    ready: Vec<u32>,
    /// Each task followed, with what it did since the wait began and its latest report.
    tasks: Vec<WaitedView<'a>>,
    /// The cursor to wait on from, as `since`.
    next: u64,
}

/// One task `task_wait` followed, for JSON.
#[derive(Debug, Serialize)]
pub struct WaitedView<'a> {
    #[serde(flatten)]
    card: CardView<'a>,
    news: Vec<EntryView>,
    last_report: Option<EntryView>,
}

/// What `task_wait` saw, for JSON.
#[must_use]
pub fn task_wait(w: &crate::ops::TaskWait) -> WaitView<'_> {
    let tasks = w
        .tasks
        .iter()
        .filter_map(|id| {
            let found = w.status.tasks.iter().find(|c| c.id == *id)?;
            let mine = |e: &&TimelineEntry| e.task == Some(*id);
            Some(WaitedView {
                card: card(found),
                news: w.news.iter().filter(mine).map(entry).collect(),
                last_report: w.reports.iter().find(mine).map(entry),
            })
        })
        .collect();
    WaitView {
        timed_out: w.timed_out,
        ready: w.ready.iter().map(|t| t.0).collect(),
        tasks,
        next: w.next,
    }
}

/// What `task_wait` saw, for a person: a line per task with what it did, then whether the
/// time ran out and the cursor to go on from.
#[must_use]
pub fn task_wait_text(w: &crate::ops::TaskWait) -> String {
    let mut out = String::new();
    for id in &w.tasks {
        let Some(card) = w.status.tasks.iter().find(|c| c.id == *id) else { continue };
        let _infallible = writeln!(out, "#{} {}  {}", card.id, state_word(card.state), card.title);
        for e in w.news.iter().filter(|e| e.task == Some(*id)) {
            let _infallible = writeln!(out, "  {}", moment(&e.what).1);
        }
    }
    let ended = if w.timed_out { "timed out; nothing was stopped" } else { "news came" };
    let _infallible = writeln!(out, "{ended}  (next {})", w.next);
    out
}

/// One rule's verdict on a worker, for JSON.
#[derive(Debug, Serialize)]
pub struct ReasonView<'a> {
    rule: &'a str,
    held: bool,
    #[serde(skip_serializing_if = "is_zero")]
    points: i64,
    #[serde(skip_serializing_if = "str::is_empty")]
    detail: &'a str,
    /// The project's need it comes from, by name.
    #[serde(skip_serializing_if = "Option::is_none")]
    need: Option<&'a str>,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
const fn is_zero(n: &i64) -> bool {
    *n == 0
}

/// A worker ranked for a placement, for JSON.
#[derive(Debug, Serialize)]
pub struct SuggestionView<'a> {
    worker: WorkerId,
    name: &'a str,
    fits: bool,
    score: i64,
    reasons: Vec<ReasonView<'a>>,
}

/// Workers ranked for a placement, best first, for JSON.
#[must_use]
pub fn suggestions(ranked: &[Suggestion]) -> Vec<SuggestionView<'_>> {
    ranked
        .iter()
        .map(|s| SuggestionView {
            worker: s.worker,
            name: &s.name,
            fits: s.fits,
            score: s.score,
            reasons: s
                .reasons
                .iter()
                .map(|r| ReasonView {
                    rule: &r.rule,
                    held: r.held,
                    points: r.points,
                    detail: &r.detail,
                    need: r.need.as_deref(),
                })
                .collect(),
        })
        .collect()
}

/// A project's need as a person reads it: `need Apple work (apps/ios): requires os ==
/// "macos"`.
fn need_text(need: &Need) -> String {
    let paths = if need.paths.is_empty() { "every task".to_owned() } else { need.paths.join(", ") };
    let mut said = Vec::new();
    if !need.require.is_empty() {
        said.push(format!("requires {}", need.require.join(" and ")));
    }
    let prefers: Vec<String> =
        need.prefer.iter().map(|p| format!("{} {:+}", p.expr, p.weight)).collect();
    if !prefers.is_empty() {
        said.push(format!("prefers {}", prefers.join(", ")));
    }
    format!("need {} ({paths}): {}", need.name, said.join("; "))
}

/// Workers ranked for a placement, best first, as a person reads them.
#[must_use]
pub fn suggestions_text(ranked: &[Suggestion]) -> String {
    let mut out = String::new();
    for s in ranked {
        let fits = if s.fits { "fits" } else { "no" };
        let _infallible = writeln!(out, "{:<16} {fits:<4} {:>6}", s.name, s.score);
        for r in &s.reasons {
            let mark = if r.held { "+" } else { "-" };
            let points = if r.points == 0 { String::new() } else { format!(" ({:+})", r.points) };
            let detail =
                if r.detail.is_empty() { String::new() } else { format!(": {}", r.detail) };
            let need = r.need.as_ref().map_or_else(String::new, |n| format!(" [{n}]"));
            let _infallible = writeln!(out, "  {mark} {}{need}{points}{detail}", r.rule);
        }
    }
    out
}

/// Every project, for JSON.
#[must_use]
pub fn projects(list: &[Project]) -> Vec<ProjectView<'_>> {
    list.iter().map(project).collect()
}

/// Every project, one line each, for a person.
#[must_use]
pub fn projects_text(list: &[Project]) -> String {
    if list.is_empty() {
        return "no projects\n".to_owned();
    }
    let mut out = String::new();
    for p in list {
        let _infallible = writeln!(out, "{}  {}  ({} → {})", p.id, p.title, p.repo, p.target);
    }
    out
}

/// A project whole as a person reads it: the record, the tree with each node's agent and
/// Claude Code's own subagents under it, then the timeline. Workers by name where `names`
/// knows them.
#[must_use]
pub fn status_text<S: std::hash::BuildHasher>(
    s: &ProjectStatus,
    names: &HashMap<WorkerId, String, S>,
) -> String {
    let term = |t: TermRef| {
        let worker = names.get(&t.worker).cloned().unwrap_or_else(|| t.worker.to_string());
        let session = t.session.to_string();
        format!("{worker}/{}", session.get(..8).unwrap_or(&session))
    };
    let p = &s.project;
    let mut out = String::new();
    let _infallible = writeln!(out, "{}  {}", p.id, p.title);
    let verifier = p.verifier.as_deref().unwrap_or("none");
    let _infallible = writeln!(
        out,
        "  {} → {}  verifier: {verifier}  {}",
        p.repo,
        p.target,
        limits_text(&p.limits)
    );
    if let Some(budget) = &p.limits.budget {
        let _infallible =
            writeln!(out, "  budget  {} (estimated)", crate::budget::text(budget, &p.spend));
    } else if p.spend.cost_micro_usd > 0 {
        let spent = slopty_proto::project::Budget::figure(
            slopty_proto::project::Budget::USD,
            p.spend.cost_micro_usd,
        );
        let _infallible = writeln!(out, "  spent  {spent} (estimated), no budget");
    }
    let (b, live) = (&s.bounds, &s.live);
    let _infallible = writeln!(
        out,
        "  running {} of the project's {}, {} of the fleet's {}",
        live.project, p.limits.live_per_project, live.fleet, b.live_agents
    );
    match p.orchestrator {
        Some(t) => {
            let _infallible = writeln!(out, "  orchestrator  {}", term(t));
        }
        None => out.push_str("  orchestrator  none named\n"),
    }
    counts_text(&mut out, s.orchestrator_natives, 2);
    for need in &p.needs {
        let _infallible = writeln!(out, "  {}", need_text(need));
    }
    for s in &p.schedules {
        let _infallible = writeln!(out, "  {}", schedule_text(s));
    }
    let mut children: HashMap<Option<u32>, Vec<&TaskCard>> = HashMap::new();
    for t in &s.tasks {
        children.entry(t.parent.map(|p| p.0)).or_default().push(t);
    }
    let mut stack: Vec<(&TaskCard, usize)> =
        children.get(&None).into_iter().flatten().rev().map(|t| (*t, 1)).collect();
    while let Some((t, depth)) = stack.pop() {
        let indent = "  ".repeat(depth);
        let agent = t.assignment.as_ref().map_or_else(String::new, |a| {
            let gone = if a.ended_ms.is_some() { " (closed)" } else { "" };
            format!("  {}{gone}", term(a.term))
        });
        let branch = t.branch.as_deref().map_or_else(String::new, |b| format!("  {b}"));
        let kind = if t.kind.is_empty() { String::new() } else { format!("[{}] ", t.kind) };
        let _infallible = writeln!(
            out,
            "{indent}#{} {:<9} {kind}{}{agent}{branch}",
            t.id.0,
            state_word(t.state),
            t.title
        );
        if let Some(status) = &t.status {
            let _infallible = writeln!(out, "{indent}   {status}");
        }
        if let Some(why) = t.assignment.as_ref().and_then(|a| a.placed.as_ref()) {
            let _infallible = writeln!(out, "{indent}   placed: {}", why.why);
        }
        if !t.depends_on.is_empty() {
            let on: Vec<String> = t.depends_on.iter().map(|d| format!("#{d}")).collect();
            let _infallible = writeln!(out, "{indent}   after {}", on.join(", "));
        }
        if t.read_only {
            let _infallible = writeln!(out, "{indent}   reads only");
        }
        if let Some(v) = &t.verified {
            let word = if v.passed { "passed" } else { "failed" };
            let head = v.head.get(..8).unwrap_or(&v.head);
            let _infallible = writeln!(out, "{indent}   verifier {word} at {head}: {}", v.summary);
        }
        if let Some(r) = &t.reviewed {
            let _infallible = writeln!(out, "{indent}   {}", reviewed_text(r));
            for f in r.blocking() {
                let at = f.path.as_deref().map_or_else(String::new, |p| match f.line {
                    Some(line) => format!("{p}:{line}: "),
                    None => format!("{p}: "),
                });
                let _infallible = writeln!(out, "{indent}     blocks: {at}{}", f.body);
            }
        }
        counts_text(&mut out, t.natives, depth.saturating_add(2));
        let kids = children.get(&Some(t.id.0)).into_iter().flatten().rev();
        stack.extend(kids.map(|k| (*k, depth.saturating_add(1))));
    }
    if !s.timeline.is_empty() {
        out.push_str("timeline\n");
        for e in &s.timeline {
            let task = e.task.map_or_else(String::new, |t| format!("#{t} "));
            let _infallible = writeln!(out, "{:>6}  {task}{}", e.seq, moment(&e.what).1);
        }
    }
    out
}

fn counts_text(out: &mut String, n: NativeCounts, depth: usize) {
    let indent = "  ".repeat(depth);
    if n.agents > 0 {
        let _infallible = writeln!(out, "{indent}· {} subagents, {} running", n.agents, n.running);
    }
    if n.todos > 0 {
        let _infallible = writeln!(out, "{indent}· {} of {} to-dos done", n.done, n.todos);
    }
}

/// One node in full as a person reads it: the task's brief, paths and placement, then its
/// natives.
#[must_use]
pub fn node_text(n: &NodeDetail) -> String {
    let mut out = String::new();
    if let Some(t) = &n.task {
        let _infallible = writeln!(out, "#{} {}  {}", t.id.0, state_word(t.state), t.title);
        if !t.brief.is_empty() {
            let _infallible = writeln!(out, "{}", t.brief.trim_end());
        }
        if !t.owns.is_empty() {
            let _infallible = writeln!(out, "owns {}", paths_text(&t.owns));
        }
        if let Some(base) = &t.base {
            let _infallible = writeln!(out, "base {base}");
        }
        if let (Some(pr), Some(checks)) = (&t.pr, &t.checks) {
            let _infallible = writeln!(out, "PR #{}: {}", pr.number, checks_text(checks));
        }
    }
    natives_text(&mut out, &n.natives, 1);
    out
}

fn natives_text(out: &mut String, natives: &Natives, depth: usize) {
    let indent = "  ".repeat(depth);
    for a in &natives.agents {
        let done = if a.stopped_ms.is_some() { "stopped" } else { "running" };
        let last = a.last.as_deref().map_or_else(String::new, |l| format!("  {l}"));
        let _infallible = writeln!(out, "{indent}· {} {} {done}{last}", a.kind, a.id);
    }
    for t in &natives.tasks {
        let mark = if t.done { "x" } else { " " };
        let _infallible = writeln!(out, "{indent}[{mark}] {}", t.subject);
    }
}

#[cfg(test)]
mod tests {
    use slopty_core::WorkerId;
    use slopty_proto::project::{Need, Preference, Reason, Suggestion};

    /// A need reads as what it covers and what it asks, and a reason a need brought names it,
    /// in text and in JSON.
    #[test]
    fn a_need_and_the_reasons_it_brings_say_its_name() {
        let apple = Need {
            name: "Apple work".to_owned(),
            paths: vec!["apps/ios".to_owned(), "crates/ui".to_owned()],
            require: vec![r#"os == "macos""#.to_owned(), "has(toolchains.xcode)".to_owned()],
            prefer: vec![Preference { expr: "cpus".to_owned(), weight: 2 }],
        };
        assert_eq!(
            super::need_text(&apple),
            r#"need Apple work (apps/ios, crates/ui): requires os == "macos" and has(toolchains.xcode); prefers cpus +2"#
        );
        let linux = Need {
            name: "Linux first".to_owned(),
            paths: Vec::new(),
            require: Vec::new(),
            prefer: vec![Preference { expr: r#"os == "linux""#.to_owned(), weight: 20 }],
        };
        assert_eq!(
            super::need_text(&linux),
            r#"need Linux first (every task): prefers os == "linux" +20"#
        );

        let ranked = [Suggestion {
            worker: WorkerId::nil(),
            name: "box".to_owned(),
            fits: true,
            score: 20,
            reasons: vec![Reason {
                rule: r#"os == "linux""#.to_owned(),
                held: true,
                points: 20,
                detail: String::new(),
                need: Some("Linux first".to_owned()),
            }],
        }];
        let text = super::suggestions_text(&ranked);
        assert!(text.contains(r#"+ os == "linux" [Linux first] (+20)"#), "{text}");
        let json = serde_json::to_value(super::suggestions(&ranked)).unwrap();
        assert_eq!(json[0]["reasons"][0]["need"], "Linux first");
    }
}
