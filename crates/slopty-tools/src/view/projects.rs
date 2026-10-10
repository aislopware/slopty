//! Projects: the record, the task tree with Claude Code's own subagents as its leaves, the
//! timeline and the workers' facts, as JSON for scripts and models and as text for a person.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use serde::Serialize;
use serde_json::Value;
use slopty_core::{WallMs, WorkerId};
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::{
    Autonomy, Bounds, Fact, Facts, GiveBacks, Limits, Live, Moment, NativeCounts, Natives,
    NodeDetail, Progress, Project, ProjectStatus, Report, StepKind, StepState, Task, TaskCard,
    TaskState, TaskStep, TestDiff, TimelineEntry, VerifierRun,
};
use slopty_proto::server::Os;
use slopty_proto::thread::wire::{PullSeen, PullStands};

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
    /// The goal the person handed over.
    goal: Option<&'a str>,
    /// `ask`, `edits` or `own`: how far its agents go before they ask the person.
    autonomy: &'static str,
    /// Where the orchestrator last said the goal stands.
    progress: Option<ProgressView<'a>>,
    verifier: Option<&'a str>,
    orchestrator: Option<String>,
    limits: Limits,
    metadata: Option<Value>,
    created_ms: WallMs,
}

/// Where the orchestrator last said the goal stands, for JSON.
#[derive(Debug, Serialize)]
pub struct ProgressView<'a> {
    summary: &'a str,
    next: Option<&'a str>,
    done: bool,
    at_ms: WallMs,
}

/// An autonomy level's word.
#[must_use]
pub const fn autonomy_word(autonomy: Autonomy) -> &'static str {
    match autonomy {
        Autonomy::Ask => "ask",
        Autonomy::Edits => "edits",
        Autonomy::Own => "own",
    }
}

/// The autonomy level `word` names ([`autonomy_word`]'s words).
#[must_use]
pub fn autonomy_named(word: &str) -> Option<Autonomy> {
    [Autonomy::Ask, Autonomy::Edits, Autonomy::Own]
        .into_iter()
        .find(|a| autonomy_word(*a) == word.trim())
}

/// A project, for JSON.
#[must_use]
pub fn project(p: &Project) -> ProjectView<'_> {
    ProjectView {
        project: p.id.as_str(),
        title: &p.title,
        repo: &p.repo,
        target: &p.target,
        goal: p.goal.as_deref(),
        autonomy: autonomy_word(p.autonomy),
        progress: p.progress.as_ref().map(|g| ProgressView {
            summary: &g.summary,
            next: g.next.as_deref(),
            done: g.done,
            at_ms: g.at_ms,
        }),
        verifier: p.verifier.as_deref(),
        orchestrator: p.orchestrator.map(term_string),
        limits: p.limits,
        metadata: metadata(p.metadata.as_deref()),
        created_ms: p.created_ms,
    }
}

/// A task's pull request as its thread's row last said it, for JSON.
#[derive(Debug, Serialize)]
pub struct PullView<'a> {
    /// `github` or `gitlab`.
    forge: &'static str,
    number: u32,
    url: &'a str,
    title: &'a str,
    base: &'a str,
    /// `merged`, `closed`, `draft`, `conflicted`, `checks_failed`, `changes_requested`,
    /// `running`, `waiting` or `ready`.
    stands: &'static str,
    /// How many of its checks failed, and the first of them.
    failed: u32,
    failed_first: Option<&'a str>,
    /// How many still run.
    running: u32,
    /// In a line, as a person reads it.
    line: String,
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

/// A task, for JSON.
#[derive(Debug, Serialize)]
pub struct TaskView<'a> {
    task: u32,
    depends_on: Vec<u32>,
    kind: &'a str,
    title: &'a str,
    brief: &'a str,
    read_only: bool,
    /// The worker it runs on and no other.
    #[serde(skip_serializing_if = "Option::is_none")]
    pin: Option<WorkerId>,
    verifier: Option<&'a str>,
    metadata: Option<Value>,
    state: &'static str,
    /// What its agent says it is doing.
    status: Option<&'a str>,
    /// Its terminal.
    term: Option<String>,
    agent_since_ms: Option<WallMs>,
    agent_ended_ms: Option<WallMs>,
    branch: Option<&'a str>,
    worktree: Option<&'a str>,
    base: Option<&'a str>,
    pull: Option<PullView<'a>>,
    verified: Option<VerifiedView<'a>>,
    /// Its time at work, as on its card.
    active_ms: u64,
    at_work_since_ms: Option<WallMs>,
    /// Its work was checked and waits for the person's merge.
    ready_to_merge: bool,
    /// The server's automatic give-backs since the person last spoke on it.
    give_backs: GiveBacksView,
    /// What its work did to the project's tests.
    #[serde(skip_serializing_if = "Option::is_none")]
    tests: Option<TestsView<'a>>,
    created_ms: WallMs,
    updated_ms: WallMs,
}

/// A task's give-backs, for JSON: how many, of how many before the person hears instead, and
/// whether a failure waits for the person now.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct GiveBacksView {
    count: u8,
    of: u8,
    held: bool,
}

const fn give_backs(g: GiveBacks) -> GiveBacksView {
    GiveBacksView { count: g.count, of: slopty_proto::project::GIVE_BACKS_MAX, held: g.held }
}

/// What a task's work did to the tests, for JSON: the line people read, and the paths.
#[derive(Debug, Serialize)]
pub struct TestsView<'a> {
    line: String,
    deleted: &'a [String],
    changed: &'a [String],
    deleted_count: u16,
    changed_count: u16,
    added_count: u16,
}

fn tests(t: &TestDiff) -> TestsView<'_> {
    TestsView {
        line: t.line(),
        deleted: &t.deleted,
        changed: &t.changed,
        deleted_count: t.deleted_count,
        changed_count: t.changed_count,
        added_count: t.added_count,
    }
}

/// A task's line in the tree, for JSON: `task_get` has the rest.
#[derive(Debug, Serialize)]
pub struct CardView<'a> {
    task: u32,
    depends_on: Vec<u32>,
    kind: &'a str,
    title: &'a str,
    read_only: bool,
    state: &'static str,
    status: Option<&'a str>,
    term: Option<String>,
    /// The worker it is pinned to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pin: Option<WorkerId>,
    agent_since_ms: Option<WallMs>,
    agent_ended_ms: Option<WallMs>,
    branch: Option<&'a str>,
    worktree: Option<&'a str>,
    pull: Option<PullView<'a>>,
    verified: Option<VerifiedView<'a>>,
    /// Its time at work, idle waits left out: the stretches that ended, and when the one under
    /// way began.
    active_ms: u64,
    at_work_since_ms: Option<WallMs>,
    /// Its work was checked and waits for the person's merge.
    ready_to_merge: bool,
    /// The server's automatic give-backs since the person last spoke on it.
    give_backs: GiveBacksView,
    /// What its work did to the project's tests.
    #[serde(skip_serializing_if = "Option::is_none")]
    tests: Option<TestsView<'a>>,
    natives: NativeCounts,
    created_ms: WallMs,
    updated_ms: WallMs,
}

/// A task's line in the tree, for JSON.
#[must_use]
pub fn card(t: &TaskCard) -> CardView<'_> {
    CardView {
        task: t.id.0,
        depends_on: t.depends_on.iter().map(|d| d.0).collect(),
        kind: &t.kind,
        title: &t.title,
        read_only: t.read_only,
        state: state_word(t.state),
        status: t.status.as_deref(),
        term: t.assignment.as_ref().map(|a| term_string(a.term)),
        pin: t.pin,
        agent_since_ms: t.assignment.as_ref().map(|a| a.since_ms),
        agent_ended_ms: t.assignment.as_ref().and_then(|a| a.ended_ms),
        branch: t.branch.as_deref(),
        worktree: t.worktree.as_deref(),
        pull: t.pull.as_ref().map(pull),
        verified: t.verified.as_ref().map(verified),
        active_ms: t.spent.active_ms,
        at_work_since_ms: t.spent.since_ms,
        ready_to_merge: t.ready_to_merge(),
        give_backs: give_backs(t.give_backs),
        tests: t.tests.as_ref().map(tests),
        natives: t.natives,
        created_ms: t.created_ms,
        updated_ms: t.updated_ms,
    }
}

/// A pull request as its thread's row says it, for JSON.
fn pull(p: &PullSeen) -> PullView<'_> {
    let forge = match p.forge {
        slopty_proto::git::Forge::GitHub => "github",
        slopty_proto::git::Forge::GitLab => "gitlab",
    };
    let stands = match p.stands {
        PullStands::Merged => "merged",
        PullStands::Closed => "closed",
        PullStands::Draft => "draft",
        PullStands::Conflicted => "conflicted",
        PullStands::ChecksFailed => "checks_failed",
        PullStands::ChangesRequested => "changes_requested",
        PullStands::Running => "running",
        PullStands::Waiting => "waiting",
        PullStands::Ready => "ready",
    };
    PullView {
        forge,
        number: p.number,
        url: &p.url,
        title: &p.title,
        base: &p.base,
        stands,
        failed: p.failed,
        failed_first: p.failed_first.as_deref(),
        running: p.running,
        line: p.line(),
    }
}

/// A task, for JSON.
#[must_use]
pub fn task(t: &Task) -> TaskView<'_> {
    TaskView {
        task: t.id.0,
        depends_on: t.depends_on.iter().map(|d| d.0).collect(),
        kind: &t.kind,
        title: &t.title,
        brief: &t.brief,
        read_only: t.read_only,
        pin: t.pin,
        verifier: t.verifier.as_deref(),
        metadata: metadata(t.metadata.as_deref()),
        state: state_word(t.state),
        status: t.status.as_deref(),
        term: t.assignment.as_ref().map(|a| term_string(a.term)),
        agent_since_ms: t.assignment.as_ref().map(|a| a.since_ms),
        agent_ended_ms: t.assignment.as_ref().and_then(|a| a.ended_ms),
        branch: t.branch.as_deref(),
        worktree: t.worktree.as_deref(),
        base: t.base.as_deref(),
        pull: t.pull.as_ref().map(pull),
        verified: t.verified.as_ref().map(verified),
        active_ms: t.spent.active_ms,
        at_work_since_ms: t.spent.since_ms,
        ready_to_merge: t.state == TaskState::Done && t.merge.is_none(),
        give_backs: give_backs(t.give_backs),
        tests: t.tests.as_ref().map(tests),
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
        Moment::Limits { limits } => ("limits", format!("limits now {}", limits_text(*limits))),
        Moment::TaskCreated { title } => ("task_created", format!("made: {title}")),
        Moment::Assigned { spawned: true, .. } => ("assigned", "started for it".to_owned()),
        Moment::Assigned { spawned: false, .. } => ("assigned", "terminal put on it".to_owned()),
        Moment::State { from, to } => {
            ("state", format!("{} (was {})", state_word(*to), state_word(*from)))
        }
        Moment::Branch { branch } => {
            ("branch", branch.as_deref().unwrap_or("no branch").to_owned())
        }
        Moment::Verified(run) => ("verified", verified_text(run)),
        Moment::Pull(pull) => ("pull", pull.line()),
        Moment::AgentGone { .. } => ("agent_gone", "its terminal closed".to_owned()),
        Moment::Note { text } => ("note", text.clone()),
        Moment::Told { text } => ("told", format!("the person told its agent: {text}")),
        Moment::Reported { report } => ("reported", report_text(report)),
        Moment::Delivered { reports, .. } => {
            ("delivered", format!("{reports} report(s) handed to its agent"))
        }
        Moment::Step(step) => ("step", step_text(step)),
        Moment::Update(progress) => ("update", progress_text(progress)),
    }
}

/// Where the orchestrator says the goal stands, in a line.
fn progress_text(progress: &Progress) -> String {
    let met = if progress.done { "goal met: " } else { "" };
    match &progress.next {
        Some(next) => format!("{met}{}; next: {next}", progress.summary),
        None => format!("{met}{}", progress.summary),
    }
}

/// A step the server took for a task, in a line.
fn step_text(step: &TaskStep) -> String {
    let what = match step.kind {
        StepKind::Clone => format!("clone on worker {}", step.worker),
        StepKind::Send => format!("send to the clone on worker {}", step.worker),
        StepKind::Home => format!("branch brought to worker {}", step.worker),
        StepKind::Verify => format!("verifier on worker {}", step.worker),
        StepKind::Merge => format!("merge on worker {}", step.worker),
        StepKind::Rebase => format!("rebase onto the target on worker {}", step.worker),
    };
    match &step.state {
        StepState::Running { phase, percent: Some(p) } => format!("{what}: {phase} {p}%"),
        StepState::Running { phase, percent: None } => format!("{what}: {phase}"),
        StepState::Done { detail } => format!("{what} done: {detail}"),
        StepState::Failed { why } => format!("{what} failed: {why}"),
    }
}

/// What a verifier said, at which commits.
fn verified_text(run: &VerifierRun) -> String {
    let short = |c: &str| c.get(..7).unwrap_or(c).to_owned();
    let word = if run.passed { "passed" } else { "failed" };
    let exit = run.exit.filter(|_| !run.passed).map(|e| format!(", exit {e}")).unwrap_or_default();
    let at = format!("{} on {}", short(&run.head), short(&run.base));
    format!("verifier {word} at {at}{exit}: {}", run.summary)
}

/// A report in a line: its note's first line, where its work is.
fn report_text(report: &Report) -> String {
    let note = report.note.lines().next().unwrap_or_default().trim();
    let mut text = if note.is_empty() { "done".to_owned() } else { format!("done: {note}") };
    if let Some(branch) = &report.branch {
        text = format!("{text} ({branch})");
    }
    text
}

fn limits_text(l: Limits) -> String {
    format!("{} waiting on you at most", l.review)
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
        limits_text(p.limits)
    );
    let (b, live) = (&s.bounds, &s.live);
    let _infallible = writeln!(
        out,
        "  running {} for the project, {} of the fleet's {}",
        live.project, live.fleet, b.live_agents
    );
    match p.orchestrator {
        Some(t) => {
            let _infallible = writeln!(out, "  orchestrator  {}", term(t));
        }
        None => out.push_str("  orchestrator  none named\n"),
    }
    counts_text(&mut out, s.orchestrator_natives, 2);
    let waiting = s.tasks.iter().filter(|t| t.waits_on_person()).count();
    if waiting > 0 {
        let _infallible =
            writeln!(out, "  waiting on you  {waiting} of {} at most", p.limits.review);
    }
    for t in &s.tasks {
        let indent = "  ";
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
        if let Some(pin) = t.pin {
            let worker = names.get(&pin).cloned().unwrap_or_else(|| pin.to_string());
            let _infallible = writeln!(out, "{indent}   runs on {worker}");
        }
        if !t.depends_on.is_empty() {
            let on: Vec<String> = t.depends_on.iter().map(|d| format!("#{d}")).collect();
            let _infallible = writeln!(out, "{indent}   after {}", on.join(", "));
        }
        if t.read_only {
            let _infallible = writeln!(out, "{indent}   reads only");
        }
        if t.ready_to_merge() {
            let _infallible =
                writeln!(out, "{indent}   ready to merge: `slopty task merge {}`", t.id);
        }
        let g = t.give_backs;
        if g.held {
            let _infallible = writeln!(
                out,
                "{indent}   needs you: given back {} times, the last failure is held for you",
                g.count
            );
        } else if g.count > 0 {
            let _infallible = writeln!(
                out,
                "{indent}   given back {} of {} times",
                g.count,
                slopty_proto::project::GIVE_BACKS_MAX
            );
        }
        if let Some(tests) = &t.tests {
            let _infallible = writeln!(out, "{indent}   {}", tests.line());
        }
        if let Some(v) = &t.verified {
            let word = if v.passed { "passed" } else { "failed" };
            let head = v.head.get(..8).unwrap_or(&v.head);
            let _infallible = writeln!(out, "{indent}   verifier {word} at {head}: {}", v.summary);
        }
        counts_text(&mut out, t.natives, 3);
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

/// One node in full as a person reads it: the task's brief, then its natives.
#[must_use]
pub fn node_text(n: &NodeDetail) -> String {
    let mut out = String::new();
    if let Some(t) = &n.task {
        let _infallible = writeln!(out, "#{} {}  {}", t.id.0, state_word(t.state), t.title);
        if !t.brief.is_empty() {
            let _infallible = writeln!(out, "{}", t.brief.trim_end());
        }
        if let Some(base) = &t.base {
            let _infallible = writeln!(out, "base {base}");
        }
        if let Some(pull) = &t.pull {
            let _infallible = writeln!(out, "{}  {}", pull.line(), pull.url);
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
