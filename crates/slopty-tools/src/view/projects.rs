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
    Bounds, Fact, Facts, Limits, Live, Moment, NativeCounts, Natives, NodeDetail, Peer, Placement,
    Project, ProjectStatus, Report, ReportKind, StepKind, StepState, Suggestion, Task, TaskCard,
    TaskState, TaskStep, TimelineEntry, VerifierRun,
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
    created_ms: WallMs,
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
        limits: p.limits,
        metadata: metadata(p.metadata.as_deref()),
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
    agent_since_ms: Option<WallMs>,
    agent_ended_ms: Option<WallMs>,
    branch: Option<&'a str>,
    worktree: Option<&'a str>,
    base: Option<&'a str>,
    pr: Option<PrView<'a>>,
    verified: Option<VerifiedView<'a>>,
    created_ms: WallMs,
    updated_ms: WallMs,
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
    agent_since_ms: Option<WallMs>,
    agent_ended_ms: Option<WallMs>,
    branch: Option<&'a str>,
    worktree: Option<&'a str>,
    pr: Option<PrView<'a>>,
    verified: Option<VerifiedView<'a>>,
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
        agent_since_ms: t.assignment.as_ref().map(|a| a.since_ms),
        agent_ended_ms: t.assignment.as_ref().and_then(|a| a.ended_ms),
        branch: t.branch.as_deref(),
        worktree: t.worktree.as_deref(),
        pr: t.pr.as_ref().map(pr),
        verified: t.verified.as_ref().map(verified),
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
        agent_since_ms: t.assignment.as_ref().map(|a| a.since_ms),
        agent_ended_ms: t.assignment.as_ref().and_then(|a| a.ended_ms),
        branch: t.branch.as_deref(),
        worktree: t.worktree.as_deref(),
        base: t.base.as_deref(),
        pr: t.pr.as_ref().map(pr),
        verified: t.verified.as_ref().map(verified),
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
        Moment::TaskCreated { title } => ("task_created", format!("made: {title}")),
        Moment::Claimed { paths } => ("claimed", format!("owns {}", paths_text(paths))),
        Moment::Assigned { spawned: true, .. } => ("assigned", "started for it".to_owned()),
        Moment::Assigned { spawned: false, .. } => ("assigned", "terminal put on it".to_owned()),
        Moment::State { from, to } => {
            ("state", format!("{} (was {})", state_word(*to), state_word(*from)))
        }
        Moment::Branch { branch, pr } => {
            let branch = branch.as_deref().unwrap_or("no branch");
            let text =
                pr.map_or_else(|| branch.to_owned(), |n| format!("{branch}, pull request #{n}"));
            ("branch", text)
        }
        Moment::Verified { passed: true, summary } => {
            ("verified", format!("verifier passed: {summary}"))
        }
        Moment::Verified { passed: false, summary } => {
            ("verified", format!("verifier failed: {summary}"))
        }
        Moment::AgentGone { .. } => ("agent_gone", "its terminal closed".to_owned()),
        Moment::Note { text } => ("note", text.clone()),
        Moment::Reported { report } => ("reported", report_text(report)),
        Moment::Delivered { reports, .. } => {
            ("delivered", format!("{reports} report(s) handed to its agent"))
        }
        Moment::Step(step) => ("step", step_text(step)),
    }
}

/// A step the server took for a task, in a line.
fn step_text(step: &TaskStep) -> String {
    let what = match step.kind {
        StepKind::Clone => format!("clone on worker {}", step.worker),
        StepKind::Home => format!("branch brought to worker {}", step.worker),
    };
    match &step.state {
        StepState::Running { phase, percent: Some(p) } => format!("{what}: {phase} {p}%"),
        StepState::Running { phase, percent: None } => format!("{what}: {phase}"),
        StepState::Done { detail } => format!("{what} done: {detail}"),
        StepState::Failed { why } => format!("{what} failed: {why}"),
    }
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
    format!(
        "{} agents per worker, {} in all, {} deep, {} entries kept",
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

/// One rule's verdict on a worker, for JSON.
#[derive(Debug, Serialize)]
pub struct ReasonView<'a> {
    rule: &'a str,
    held: bool,
    #[serde(skip_serializing_if = "is_zero")]
    points: i64,
    #[serde(skip_serializing_if = "str::is_empty")]
    detail: &'a str,
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
                })
                .collect(),
        })
        .collect()
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
            let _infallible = writeln!(out, "  {mark} {}{points}{detail}", r.rule);
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
