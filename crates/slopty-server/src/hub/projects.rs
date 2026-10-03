//! The hub's side of projects (`docs/decisions/projects.md`): the verbs the store answers,
//! placement over the workers' facts, every start of an agent or a task's terminal counted
//! against the person's bounds and the project's limits from the moment it is placed, and the
//! reports on their way up the tree.
//!
//! A start's terminal id is the hub's to choose (the start's token): the worker opens the
//! terminal under it, and a start asked again under it answers that terminal instead of
//! opening another. So a start whose answer was lost still counts, and its terminal is put on
//! its task when the worker announces it. A caller never chooses the id.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::agent::AgentKind;
use slopty_proto::orchestration::{ErrorCode, IdempotencyKey, Outcome, TermRef, Verb};
use slopty_proto::project::{
    Bounds, Fact, Facts, LOOSENED_ITEM_MAX, LOOSENED_MAX, PERMISSION_MODE_FLAG, PROJECT_ENV,
    Placed, Placement, Project, ProjectId, ProjectStatus, ProjectsPart, Proposal, Proposed, Report,
    ReportKind, Runner, SAFE_MODES, Suggestion, TASK_ENV, Task, TaskId, TaskLaunch, TimelineEntry,
    WorkerFacts,
};
use slopty_proto::screen::VideoCodec;
use slopty_proto::server::{FromServer, Liveness, Os};
use slopty_proto::terminal::{RepoId, SessionSummary};
use slopty_proto::thread::AgentId;

use super::{
    Again, Entry, Hub, State, WAIT_CAP_MS, WeakHub, branch_of, codex, digest, error, keep_start,
    keyed, known_term, remember, start_again, start_answered, term_of,
};
use crate::deliver::{Batch, plain};
use crate::placement::{self, Candidate, Ranking};
use crate::project::{
    Assignee, Caller, Drove, Keep, NewProject, Policy, ProjectChange, Running, Starting, Watched,
    clipped,
};

/// How long a start still counts once its worker answered (or its answer was lost), until its
/// terminal counts on its own: past it, the terminal is gone or never came.
pub(super) const STARTED_GRACE: Duration = Duration::from_secs(30);

/// How long ranking may judge rules before it stops: a rule not judged by then does not hold.
const RANK_DEADLINE: Duration = Duration::from_secs(2);
/// Rankings run at once on the blocking pool; more wait their turn, up to the deadline.
pub(super) const RANKERS: usize = 2;
/// The most bytes of projects one [`FromServer::Projects`] part carries, well within a frame.
const PART_BYTES: usize = 8 << 20;
/// The longest rules text a project's metadata gives its agents or its orchestrator, in bytes.
const RULES_MAX: usize = 2048;

/// Flags that pick the conversation a `claude` run is in.
const PICKS_CONVERSATION: [&str; 6] =
    ["--session-id", "--resume", "-r", "--continue", "-c", "--from-pr"];

/// The terminals and the agents live now, with the starts that count no more pruned: those
/// answered whose grace ended, a plain agent's once its agent shows, a task's once its task
/// holds its terminal.
pub(super) fn live(state: &mut State) -> (HashSet<TermRef>, HashSet<TermRef>) {
    let mut terminals = HashSet::new();
    let mut agents = HashSet::new();
    for entry in state.workers.values() {
        for s in &entry.sessions {
            let term = TermRef { worker: entry.info.worker, session: s.id };
            terminals.insert(term);
            // A terminal an agent opened or typed into counts as an agent's whatever runs in
            // it now: the agent may start one there at any moment, past every count.
            if s.agent.is_some() || driven(state, s.id) {
                agents.insert(term);
            }
        }
    }
    let now = tokio::time::Instant::now();
    let projects = &state.projects;
    state.starting.retain(|s| {
        let counted = if s.agent && s.task.is_none() {
            agents.contains(&s.term)
        } else {
            // Its task holds it, and its worker announced it: it counts as the task's.
            terminals.contains(&s.term)
                && s.task.as_ref().is_some_and(|(p, t)| {
                    projects.working_on(s.term) == Some((p.clone(), Some(*t)))
                })
        };
        !(counted || (s.answered && now.duration_since(s.since) >= STARTED_GRACE))
    });
    // A watched terminal that is gone is forgotten once any start would be, but only by a
    // worker that is here to say it is gone: one a restart has not heard from yet keeps its.
    let here: HashSet<WorkerId> =
        state.workers.values().filter(|e| e.link.is_some()).map(|e| e.info.worker).collect();
    let gone: Vec<SessionId> = state
        .watched
        .values()
        .filter(|(w, since)| {
            here.contains(&w.term.worker)
                && !terminals.contains(&w.term)
                && !state.starting.iter().any(|s| s.term == w.term)
                && now.duration_since(*since) >= STARTED_GRACE
        })
        .map(|(w, _)| w.term.session)
        .collect();
    for session in gone {
        unwatch(state, session);
    }
    (terminals, agents)
}

/// Whether an agent opened or typed into the terminal `session`.
pub(super) fn driven(state: &State, session: SessionId) -> bool {
    state.watched.get(&session).is_some_and(|(w, _)| w.drove.is_some())
}

/// Watch `term` as `change` leaves it, and have the store keep it when it changed.
pub(super) fn watch(state: &mut State, term: TermRef, change: impl FnOnce(&mut Watched)) {
    let mut fresh = false;
    let (held, _) = state.watched.entry(term.session).or_insert_with(|| {
        fresh = true;
        (Watched { term, locked: false, drove: None }, tokio::time::Instant::now())
    });
    let before = *held;
    change(held);
    let after = *held;
    if fresh || after != before {
        keep(state, Keep::Watch(after));
    }
}

/// The project and task the terminal `from` works on, when an agent proved it speaks from one.
pub(super) fn node_of(
    state: &State,
    from: Option<SessionId>,
) -> Option<(ProjectId, Option<TaskId>)> {
    state.projects.working_on(term_of(state, from?)?)
}

/// The project `term` works for: the one it works on, or else the one whose agent opened it.
pub(super) fn project_of(state: &State, term: TermRef) -> Option<ProjectId> {
    if let Some((project, _)) = state.projects.working_on(term) {
        return Some(project);
    }
    match state.watched.get(&term.session).and_then(|(w, _)| w.drove) {
        Some(Drove::Opened { by }) => node_of(state, by).map(|(project, _)| project),
        _ => None,
    }
}

/// Whether the person allows looser permissions (`[server.projects] permission_flags`) to a
/// start for `project` by `caller` speaking from `from`. The allowance is a project's: an agent
/// has it only in the project it proves it works in, and names none of another's; a start that
/// names no project has the agent's own.
pub(super) fn allowance(
    state: &State,
    caller: Caller,
    from: Option<SessionId>,
    project: Option<&ProjectId>,
) -> bool {
    let project = match caller {
        Caller::Person => project.cloned(),
        Caller::Agent => {
            let own = node_of(state, from).map(|(project, _)| project);
            match project {
                Some(named) if own.as_ref() != Some(named) => return false,
                _ => own,
            }
        }
    };
    state.projects.policy().bounds_for(project.as_ref()).permission_flags
}

/// Whether `term` is `project`'s to put to work, for an agent speaking from `from`: its own
/// terminal, one it opened, one the project holds (its orchestrator's, a task's, a start's for
/// it), or one an agent of the project opened. Never the person's own.
fn theirs(state: &State, from: SessionId, project: &ProjectId, term: TermRef) -> bool {
    if term.session == from {
        return true;
    }
    let holds = state.projects.working_on(term).is_some_and(|(p, _)| p == *project)
        || state
            .starting
            .iter()
            .any(|s| s.term == term && s.task.as_ref().is_some_and(|(p, _)| p == project));
    if holds {
        return true;
    }
    match state.watched.get(&term.session).and_then(|(w, _)| w.drove) {
        Some(Drove::Opened { by: Some(by) }) => {
            by == from || node_of(state, Some(by)).is_some_and(|(p, _)| p == *project)
        }
        _ => false,
    }
}

/// What an agent speaking from `from` may do to the projects with `verb`, and the verb as it
/// is done. An agent works in the project it proves it works in: its orchestrator in all of
/// it, a task's agent in its own task and what is split from it, a task it splits off going
/// under its own. Only an orchestrator makes a project. The terminals it puts to work are the
/// project's or its own ([`theirs`]), never the person's.
pub(super) fn agent_scope(
    state: &State,
    from: Option<SessionId>,
    verb: Verb,
) -> Result<Verb, Outcome> {
    let changes = matches!(
        verb,
        Verb::ProjectCreate { .. }
            | Verb::ProjectSet { .. }
            | Verb::ProjectNeeds { .. }
            | Verb::TaskCreate { .. }
            | Verb::TaskClaim { .. }
            | Verb::TaskUpdate { .. }
            | Verb::TaskSpawn { .. }
            | Verb::TaskAssign { .. }
    );
    if !changes {
        return Ok(verb);
    }
    let refuse = |why: &str| Err(error(ErrorCode::Forbidden, why));
    let Some((own, task_of)) = node_of(state, from) else {
        return refuse(&format!(
            "an agent changes a project only from a Slopty terminal that works in it, proven by \
             its {}; this caller proves none",
            slopty_proto::ctl::SESSION_TOKEN_ENV
        ));
    };
    let from = from.unwrap_or_default();
    let in_own = |project: &ProjectId| {
        if *project == own {
            Ok(())
        } else {
            Err(error(
                ErrorCode::Forbidden,
                &format!("this agent works in project {own}, not {project}"),
            ))
        }
    };
    let under = |project: &ProjectId, task: TaskId| match task_of {
        Some(root) if !state.projects.under(project, root, task) => Err(error(
            ErrorCode::Forbidden,
            &format!(
                "this agent works on task {root}, and task {task} is neither it nor split from it"
            ),
        )),
        _ => Ok(()),
    };
    let named = |project: &ProjectId, term: TermRef| {
        if theirs(state, from, project, term) {
            Ok(())
        } else {
            Err(error(
                ErrorCode::Forbidden,
                "an agent puts to work only its own terminal, one it opened, or one its project \
                 holds or an agent of the project opened; the person's own terminals are the \
                 person's to give",
            ))
        }
    };
    let verb = match verb {
        Verb::TaskCreate { project, mut spec } => {
            in_own(&project)?;
            match (spec.parent, task_of) {
                (None, Some(root)) => spec.parent = Some(root),
                (Some(parent), _) => under(&project, parent)?,
                (None, None) => {}
            }
            return Ok(Verb::TaskCreate { project, spec });
        }
        other => other,
    };
    match &verb {
        Verb::ProjectCreate { project, orchestrator, .. } => {
            if task_of.is_some() {
                return refuse("only the person or an orchestrator makes a project");
            }
            if let Some(term) = orchestrator {
                named(project, *term)?;
            }
        }
        Verb::ProjectSet { project, orchestrator, .. } => {
            in_own(project)?;
            if task_of.is_some() {
                return refuse("only the person or the project's orchestrator changes a project");
            }
            if let Some(term) = orchestrator {
                named(project, *term)?;
            }
        }
        Verb::ProjectNeeds { project, .. } => {
            in_own(project)?;
            if task_of.is_some() {
                return refuse("only the person or the project's orchestrator says what it needs");
            }
        }
        Verb::TaskClaim { project, task, .. }
        | Verb::TaskUpdate { project, task, .. }
        | Verb::TaskSpawn { project, task, .. } => {
            in_own(project)?;
            under(project, *task)?;
        }
        Verb::TaskAssign { project, task, term } => {
            in_own(project)?;
            under(project, *task)?;
            named(project, *term)?;
        }
        _ => {}
    }
    Ok(verb)
}

/// Stop watching the terminal `session`, and have the store forget it.
pub(super) fn unwatch(state: &mut State, session: SessionId) {
    if state.watched.remove(&session).is_some() {
        keep(state, Keep::Unwatch(session));
    }
}

/// Send the store what it keeps, in the order the changes were made (under the hub's lock).
pub(super) fn keep(state: &mut State, keep: Keep) {
    if let Some(keeper) = &state.keeper
        && keeper.send(keep).is_err()
    {
        tracing::warn!("the projects keeper is gone; changes are no longer kept");
        state.keeper = None;
    }
}

/// The first thing in `args`, Claude Code's own arguments, that loosens its permissions
/// ([`slopty_agent::loosening::args`]). The server reads no worker's disk and adds none of
/// Slopty's own flags itself, so a settings file, a hook or a tools server an agent names
/// loosens.
fn loosening(args: &[String]) -> Option<String> {
    let own = slopty_agent::loosening::Own::default();
    slopty_agent::loosening::args(args, None, &own).into_iter().next()
}

/// Whether `args` pick something themselves, among `flags`, before a `--`.
fn names(args: &[String], flags: &[&str]) -> bool {
    args.iter().take_while(|w| *w != "--").any(|word| {
        let flag = word.split_once('=').map_or(word.as_str(), |(flag, _)| flag);
        flags.contains(&flag)
    })
}

/// Whether `verb` names a verifier, for a project or a task: what the merge queue will hold
/// work to.
fn names_verifier(verb: &Verb) -> bool {
    match verb {
        Verb::ProjectCreate { verifier, .. } | Verb::ProjectSet { verifier, .. } => {
            verifier.is_some()
        }
        Verb::TaskCreate { spec, .. } => spec.verifier.is_some(),
        Verb::TaskUpdate { change, .. } => change.verifier.is_some(),
        _ => false,
    }
}

/// Every rule of `needs` compiles, within the comprehensions the person allows, and all of
/// them together fit one placement, as they do for a task that has every need.
fn needs_compile(needs: &[slopty_proto::project::Need], depth: u8) -> Result<(), Outcome> {
    let mut all = Placement::default();
    for need in needs {
        let rules = Placement {
            require: need.require.clone(),
            prefer: need.prefer.clone(),
            ..Placement::default()
        };
        placement::check(&rules, depth).map_err(|refused| match refused {
            Outcome::Error { code, message } => {
                Outcome::Error { code, message: format!("the need {:?}: {message}", need.name) }
            }
            other => other,
        })?;
        all.require.extend(rules.require);
        all.prefer.extend(rules.prefer);
    }
    placement::check(&all, depth).map_err(|refused| match refused {
        Outcome::Error { code, message } => Outcome::Error {
            code,
            message: format!("the needs together, as a task with all of them has them: {message}"),
        },
        other => other,
    })
}

/// Whether `verb` turns pushing a project's target on or off: publishing is the person's.
const fn names_push(verb: &Verb) -> bool {
    match verb {
        Verb::ProjectCreate { push, .. } => *push,
        Verb::ProjectSet { push, .. } => push.is_some(),
        _ => false,
    }
}

/// Whether `verb` says whether a project's starts wait for the person: how far a project runs
/// on its own is the person's to say. An agent's new project starts as it asks, as before.
const fn names_ask_to_start(verb: &Verb) -> bool {
    match verb {
        Verb::ProjectCreate { ask_to_start, .. } => *ask_to_start,
        Verb::ProjectSet { ask_to_start, .. } => ask_to_start.is_some(),
        _ => false,
    }
}

/// The arguments `claude` gets from a command line that runs it: `claude …`, a runtime running
/// its script, or a shell line with `claude` as one of its programs (`sh -c "cd x && claude
/// --allowedTools Bash"`), read as the shell reads it ([`slopty_agent::detect`]).
fn claude_args(argv: &[String]) -> Option<Vec<String>> {
    use slopty_agent::detect;
    if let Some(command) = detect::shell_command(argv) {
        return detect::shell_agent_args(command);
    }
    let program = argv.first()?;
    let name = program.rsplit('/').next().unwrap_or(program);
    detect::is_claude(name, argv).then(|| detect::agent_args(argv).to_vec())
}

fn loosened(flag: &str, project: Option<&ProjectId>) -> Outcome {
    let whose = project.map_or_else(
        || "an agent started outside a project".to_owned(),
        |p| format!("project {p}'s agents"),
    );
    error(
        ErrorCode::Limit,
        &format!(
            "{flag} may give the agent more than its starter has (only what is known to ask the \
             person no less is let through); the person allows it for {whose} only in the \
             server's settings.toml (`[server.projects] permission_flags`)"
        ),
    )
}

/// `args` for an agent the server starts: the permission mode pinned to `default` when they
/// name none and the person allows no looser one, so no settings file an agent may have
/// written starts it in a looser mode; and, for a task, a conversation id chosen now
/// (`--session-id`), so the task knows it before the first hook, and the role it plays
/// (`--append-system-prompt`). The id, when chosen, comes back too.
pub(super) fn started_args(
    mut args: Vec<String>,
    permission_flags: bool,
    role: Option<String>,
) -> (Vec<String>, Option<String>) {
    let mut first = Vec::new();
    if !permission_flags && !names(&args, &[PERMISSION_MODE_FLAG]) {
        first.extend([PERMISSION_MODE_FLAG.to_owned(), "default".to_owned()]);
    }
    let mut conversation = None;
    if let Some(role) = role {
        if !names(&args, &PICKS_CONVERSATION) {
            let id = SessionId::new().to_string();
            first.extend(["--session-id".to_owned(), id.clone()]);
            conversation = Some(id);
        }
        first.push(format!("--append-system-prompt={role}"));
    }
    first.append(&mut args);
    (first, conversation)
}

/// Refused when the fleet runs as many agents as the person allows, or `worker` as many as
/// one worker may.
pub(super) fn fleet_room(
    state: &State,
    running: &Running<'_>,
    bounds: Bounds,
    worker: Option<WorkerId>,
) -> Result<(), Outcome> {
    let fleet = state.projects.fleet(running);
    if fleet >= bounds.live_agents {
        return Err(error(
            ErrorCode::Limit,
            &format!(
                "the fleet runs {fleet} agents, the {} the person allows (`[server.projects] \
                 live_agents` in the server's settings.toml); wait for one to end",
                bounds.live_agents
            ),
        ));
    }
    if let Some(worker) = worker {
        let here = state.projects.live_on_worker(worker, running);
        if here >= bounds.live_per_worker {
            return Err(error(
                ErrorCode::Limit,
                &format!(
                    "that worker runs {here} agents, the {} the person allows one worker \
                     (`[server.projects] live_per_worker`); choose another or wait",
                    bounds.live_per_worker
                ),
            ));
        }
    }
    Ok(())
}

const fn os_word(os: Os) -> &'static str {
    match os {
        Os::MacOs => "macos",
        Os::Linux => "linux",
    }
}

/// Why `worker` was chosen, from its place in `ranked`.
fn placed_on(ranked: &[Suggestion], worker: WorkerId) -> Option<Placed> {
    ranked.iter().find(|s| s.worker == worker).map(Placed::of)
}

/// A worker's facts: its own, with what the server knows of it over them.
fn facts_of(entry: &Entry, agents_here: u16, made: &[(String, RepoId)]) -> Facts {
    let (info, caps) = (&entry.info, &entry.info.caps);
    let mut facts = entry.facts.clone();
    let text = |t: &str| Fact::Text(t.to_owned());
    let int = |n: u64| Fact::Int(i64::try_from(n).unwrap_or(i64::MAX));
    let codecs = caps.encoders.iter().map(|c| {
        text(match c {
            VideoCodec::Hevc => "hevc",
            VideoCodec::H264 => "h264",
        })
    });
    let known = [
        ("name", text(&info.name)),
        ("worker", text(&info.worker.to_string())),
        ("os", text(os_word(caps.os))),
        ("os_version", text(&caps.os_version)),
        ("arch", text(&caps.arch)),
        ("cpus", Fact::Int(i64::from(caps.cpus))),
        ("memory_mb", int(caps.memory / (1024 * 1024))),
        ("encoders", Fact::List(codecs.collect())),
        ("displays", int(u64::try_from(caps.displays.len()).unwrap_or(u64::MAX))),
        ("can_capture", Fact::Bool(caps.can_capture)),
        ("can_inject", Fact::Bool(caps.can_inject)),
        ("virtual_displays", Fact::Bool(caps.virtual_displays)),
        ("slopty_version", text(&caps.version)),
        ("load", Fact::Float(f64::from(info.load))),
        ("online", Fact::Bool(info.liveness == Liveness::Online)),
        ("live_agents", Fact::Int(i64::from(agents_here))),
        ("repos", repos_of(&entry.sessions, made)),
    ];
    // What the server knows of a worker is its word over the worker's own.
    for (name, fact) in known {
        facts.insert(name.to_owned(), fact);
    }
    // The agents it registered with are installed before its own facts say so: those with an
    // adapter under `agents` by program, those reached over ACP under `acp` by name.
    for agent in &caps.agents {
        let (map, name) = match agent.agent.acp_name() {
            Some(name) => ("acp", name),
            None if agent.agent.is(AgentId::CLAUDE_CODE) => ("agents", CLAUDE),
            None => ("agents", agent.agent.0.as_str()),
        };
        let entry = facts.entry(map.to_owned()).or_insert_with(|| Fact::Map(Facts::new()));
        if let Fact::Map(installed) = entry {
            installed.entry(name.to_owned()).or_insert_with(|| text(&agent.version));
        }
    }
    facts
}

/// Which repository a project's orchestrator works in, by the key every clone of it shares,
/// and where each worker has one (its name, the path), in name order.
#[derive(Debug, Default)]
struct Clones {
    key: Option<String>,
    on: Vec<(String, String)>,
}

/// [`Clones`] of `project`'s repository (learned from where its orchestrator works): keyed by
/// its origin when it has one, else its first commit.
fn clones_of(state: &State, project: &Project) -> Clones {
    let Some(id) = &project.repo_id else { return Clones::default() };
    let Some(key) = id.origin.clone().or_else(|| id.root.clone()) else {
        return Clones::default();
    };
    let mut on: Vec<(String, String)> = state
        .workers
        .values()
        .filter_map(|e| {
            let made = made_on(state, e.info.worker);
            let Fact::Map(repos) = repos_of(&e.sessions, made) else { return None };
            let path = id.keys().find_map(|k| match repos.get(k) {
                Some(Fact::Text(path)) => Some(path.clone()),
                _ => None,
            })?;
            Some((e.info.name.clone(), path))
        })
        .collect();
    on.sort();
    Clones { key: Some(key), on }
}

/// The clones the server had made on `worker` ([`super::steps::Steps::made_on`]).
fn made_on(state: &State, worker: WorkerId) -> &[(String, RepoId)] {
    state.steps.made_on(worker)
}

/// The repositories a worker has a shell in or the server had cloned there (`made`), by each
/// key of their identity ([`RepoId`]: the normalized origin, the first commit), to where the
/// clone is. One repository cloned on two workers has the same keys on both, so
/// `"github.com/o/r" in repos` places a task beside a clone of it and `repos["github.com/o/r"]`
/// says where; with several clones on one worker the first path in order is named.
fn repos_of(sessions: &[SessionSummary], made: &[(String, RepoId)]) -> Fact {
    let mut repos: BTreeMap<String, &str> = BTreeMap::new();
    let shells = sessions.iter().filter_map(|s| Some((s.repo.as_ref()?, s.repo_id.as_ref()?)));
    let cloned = made.iter().map(|(path, id)| (path, id));
    for (path, id) in shells.chain(cloned) {
        for key in id.keys() {
            let at = repos.entry(key.to_owned()).or_insert(path);
            if path.as_str() < *at {
                *at = path;
            }
        }
    }
    Fact::Map(repos.into_iter().map(|(key, path)| (key, Fact::Text(path.to_owned()))).collect())
}

/// A rules text from a project's metadata (`agent_rules`, `orchestrator_rules`).
fn rules(project: &Project, key: &str) -> Option<String> {
    let doc: serde_json::Value = serde_json::from_str(project.metadata.as_deref()?).ok()?;
    let text = doc.get(key)?.as_str()?.trim();
    let mut end = text.len().min(RULES_MAX);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    Some(plain(text.get(..end).unwrap_or_default())).filter(|t| !t.is_empty())
}

/// Claude Code's flag that starts a session in a git worktree of its own, by its two names.
const WORKTREE_FLAGS: [&str; 2] = ["--worktree", "-w"];

/// Where the server starts a task that named no directory: beside a clone of the project's
/// repository on the worker it placed it on.
struct Place {
    /// The clone's root there.
    path: String,
    /// The worktree its agent works in, for an agent that writes. Each such task has its own,
    /// so agents in one clone never edit the same checkout.
    worktree: Option<Worktree>,
}

/// The git worktree a writing agent works in, made by the agent itself.
enum Worktree {
    /// Claude Code's, by the name it is given (`--worktree <name>`), reopened by that name.
    Named(String),
    /// Codex's, which it makes and names itself (`--worktree`).
    Codex,
}

/// The placement rule that holds on a worker with a clone of `id`: either key of it in its
/// `repos`. `None` for an identity with no key.
fn beside_a_clone(id: &RepoId) -> Option<String> {
    let held: Vec<String> = id
        .keys()
        .filter_map(|key| serde_json::to_string(key).ok())
        .map(|key| format!("{key} in repos"))
        .collect();
    (!held.is_empty()).then(|| held.join(" || "))
}

/// Where `worker` has a clone of `project`'s repository.
pub(super) fn clone_on(state: &State, project: &Project, worker: WorkerId) -> Option<String> {
    let id = project.repo_id.as_ref()?;
    let sessions = &state.workers.get(&worker)?.sessions;
    let Fact::Map(repos) = repos_of(sessions, made_on(state, worker)) else { return None };
    id.keys().find_map(|key| match repos.get(key) {
        Some(Fact::Text(path)) => Some(path.clone()),
        _ => None,
    })
}

/// What an agent started for `task` is told of its role, beside Claude Code's own prompt.
fn agent_role(project: &Project, task: &Task, at: Option<&Place>) -> String {
    let owns = if task.read_only {
        "none: it only reads".to_owned()
    } else if task.owns.is_empty() {
        "none yet; claim what you will write with task_claim first".to_owned()
    } else {
        task.owns.join(", ")
    };
    let reports_to = task.parent.map_or_else(
        || "the project's orchestrator".to_owned(),
        |parent| format!("task {parent}'s agent"),
    );
    let mut lines = vec![
        format!(
            "You are the agent of task {} (\"{}\") of the Slopty project {} ({}, work lands \
             on {}). Your first prompt is its brief.",
            task.id,
            plain(&task.title),
            project.id,
            plain(&project.repo),
            plain(&project.target)
        ),
        "Slopty's tools are the `slopty` MCP server; with no project or task named they act on \
         your own."
            .to_owned(),
        format!(
            "- You alone may write the paths your task owns: {owns}. Other tasks own the rest."
        ),
        format!(
            "- Report to {reports_to} with task_report: checkpoint for progress, needs_input for \
             an answer, stuck when you cannot go on, done when finished (with the branch and \
             what you made). Say done; never merge."
        ),
        "- To split your work, read project_status first, then task_create subtasks under your \
         task and task_spawn them; their reports reach you in <slopty-reports> blocks."
            .to_owned(),
        "- Never type into another agent's terminal, and leave git remotes and git config as \
         they are."
            .to_owned(),
    ];
    if let Some(Place { path, worktree }) = at {
        lines.push(match worktree {
            Some(Worktree::Codex) => format!(
                "- You work in a git worktree Codex made for you from the clone at {}. Commit \
                 your work there, and name its branch when you report done.",
                plain(path)
            ),
            Some(Worktree::Named(name)) => format!(
                "- You work in a git worktree of your own, {name}, made from the clone at {} \
                 (branch worktree-{name}). Commit your work there, and name that branch when \
                 you report done.",
                plain(path)
            ),
            None => format!("- You work in the clone at {}.", plain(path)),
        });
    }
    lines.extend(rules(project, "agent_rules"));
    lines.join("\n")
}

/// What a project's orchestrator is told once it is named, through its hooks.
fn orchestrator_role(project: &Project, clones: &Clones) -> String {
    let mut lines = vec![
        format!(
            "You orchestrate the Slopty project {} (\"{}\", {}, work lands on {}), through the \
             `slopty` MCP server.",
            project.id,
            plain(&project.title),
            plain(&project.repo),
            plain(&project.target)
        ),
        "- Split the goal into tasks that own disjoint paths (task_create), place and start \
         them (placement_suggest, task_spawn), and follow them with project_status. Reports \
         come to you in <slopty-reports> blocks like this one."
            .to_owned(),
        "- Implement nothing yourself, merge nothing, and answer no permission: approvals are \
         the person's."
            .to_owned(),
        "- When the person asks to start each task themselves, task_spawn proposes it \
         (`proposed` on the task) and it starts once they say so: propose the whole plan, then \
         wait for the starts and reports rather than spawning again."
            .to_owned(),
    ];
    if project.needs.is_empty() {
        lines.push(
            "- Work that needs no Apple platform belongs on a Linux worker. Say so once with \
             project_needs before you split the goal: a need such as \"Apple work\" over the \
             paths that build only on a Mac requires `os == \"macos\"`, and one such as \
             \"Linux first\" over every path prefers `os == \"linux\"`. The board then names \
             the need as the reason each task went where it did."
                .to_owned(),
        );
    } else {
        let names: Vec<String> = project.needs.iter().map(|n| plain(&n.name)).collect();
        lines.push(format!(
            "- Its needs place every task by the paths it owns ({}); project_status shows them, \
             and project_needs says them anew.",
            names.join(", ")
        ));
    }
    lines.push(
        "- task_spawn runs Claude Code; `agent: \"codex\"` runs the person's Codex instead, \
         with the same tools, on a worker that has it."
            .to_owned(),
    );
    if let Some(key) = &clones.key {
        let on: Vec<String> = clones
            .on
            .iter()
            .map(|(name, path)| format!("{} ({})", plain(name), plain(path)))
            .collect();
        let on = if on.is_empty() {
            "no worker has a clone of it yet".to_owned()
        } else {
            format!("clones of it are on {}", on.join(", "))
        };
        let cloned = if project.repo_id.as_ref().is_some_and(|id| id.url.is_some()) {
            " A worker your placement picks that has none gets one cloned first, which the \
             task's step shows."
        } else {
            ""
        };
        lines.push(format!(
            "- Its repository is {key} on every worker; {on}. A task started with no cwd goes \
             beside a clone, in a git worktree of its own when its agent writes.{cloned} For a \
             placement of your own, `\"{key}\" in repos` holds on a worker with a clone and \
             `repos[\"{key}\"]` is its path there."
        ));
        lines.push(format!(
            "- When a task done on another machine reports its branch, the server fetches it \
             into your clone as slopty/{}/<task>, and the task's step says when it is there.",
            project.id
        ));
    }
    lines.extend(rules(project, "orchestrator_rules"));
    lines.join("\n")
}

/// The projects a link is sent, in parts that each fit a frame: a project larger than a part
/// goes over several, each carrying its record again with more of its tasks and the first its
/// timeline.
pub(super) fn parts(seq: u64, projects: Vec<ProjectStatus>) -> Vec<ProjectsPart> {
    let mut parts: Vec<ProjectsPart> = Vec::new();
    let mut current: Vec<ProjectStatus> = Vec::new();
    let mut used = 0_usize;
    let mut flush = |current: &mut Vec<ProjectStatus>, used: &mut usize| {
        let first = parts.is_empty();
        parts.push(ProjectsPart { seq, first, last: false, projects: std::mem::take(current) });
        *used = 0;
    };
    for mut status in projects {
        let head = status.project.approx_bytes().saturating_add(
            status.timeline.iter().map(TimelineEntry::approx_bytes).fold(0, usize::saturating_add),
        );
        let tasks = std::mem::take(&mut status.tasks);
        if used.saturating_add(head) > PART_BYTES && !current.is_empty() {
            flush(&mut current, &mut used);
        }
        used = used.saturating_add(head);
        let mut part = status.clone();
        for card in tasks {
            let bytes = card.approx_bytes();
            if used.saturating_add(bytes) > PART_BYTES
                && !(current.is_empty() && part.tasks.is_empty())
            {
                current.push(part);
                flush(&mut current, &mut used);
                part = ProjectStatus { tasks: Vec::new(), timeline: Vec::new(), ..status.clone() };
                used = status.project.approx_bytes();
            }
            used = used.saturating_add(bytes);
            part.tasks.push(card);
        }
        current.push(part);
    }
    flush(&mut current, &mut used);
    if let Some(last) = parts.last_mut() {
        last.last = true;
    }
    parts
}

/// A start's place, given up when the start ends before it settles.
struct InFlight<'h> {
    hub: &'h Hub,
    id: u64,
    settled: bool,
}

impl InFlight<'_> {
    /// The worker answered, or its answer was lost: the start counts on for its grace, until
    /// what it opened counts on its own.
    fn answered(mut self) {
        self.settled = true;
        let mut state = self.hub.inner.state.lock();
        if let Some(s) = state.starting.iter_mut().find(|s| s.id == self.id) {
            s.answered = true;
            s.since = tokio::time::Instant::now();
        }
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        if !self.settled {
            self.hub.inner.state.lock().starting.retain(|s| s.id != self.id);
        }
    }
}

/// Whether an answer leaves open that the worker did the work.
const fn maybe_done(outcome: &Outcome) -> bool {
    matches!(outcome, Outcome::Error { code: ErrorCode::Interrupted, .. })
}

impl Hub {
    /// Take up the person's policy: the bounds on every project and on the fleet.
    pub fn set_policy(&self, policy: Policy) {
        let mut state = self.inner.state.lock();
        let updates = state.projects.set_policy(policy);
        self.projects_moved(&mut state, updates);
        // Pushed under the lock, so every link hears changes in the order they were made.
        drop(state);
    }

    pub(super) fn status_of(
        state: &mut State,
        project: &ProjectId,
        since: Option<u64>,
    ) -> Result<ProjectStatus, Outcome> {
        let (terminals, agents) = live(state);
        let running = Running { terminals: &terminals, agents: &agents, starting: &state.starting };
        state.projects.status(project, since, &running)
    }

    /// Every project whole, with what runs now.
    /// The person lets `project` go: its record, its lane's work, a reviewer reading for it
    /// and the reports waiting in it. Every client is sent the projects afresh, which a
    /// snapshot's first part replaces whole, as no change says a project is gone.
    pub(super) fn project_delete(&self, project: &ProjectId) -> Outcome {
        let mut guard = self.inner.state.lock();
        let state = &mut *guard;
        if let Err(refused) = state.projects.delete(project) {
            return refused;
        }
        let readers: Vec<TermRef> = state
            .reviews
            .iter()
            .filter(|((p, _), _)| p == project)
            .map(|(_, reading)| reading.term())
            .collect();
        state.reviews.retain(|(p, _), _| p != project);
        keep(state, Keep::Forget(project.clone()));
        let projects = Self::projects_snapshot(state);
        let seq = self.inner.log.lock().next.saturating_sub(1);
        for part in parts(seq, projects) {
            self.announce(FromServer::Projects(Box::new(part)));
        }
        drop(guard);
        for term in readers {
            self.close_soon(term);
        }
        tracing::info!(%project, "project let go");
        Outcome::Done
    }

    pub(super) fn projects_snapshot(state: &mut State) -> Vec<ProjectStatus> {
        let (terminals, agents) = live(state);
        let running = Running { terminals: &terminals, agents: &agents, starting: &state.starting };
        state.projects.snapshot(&running)
    }

    /// A project whole, its timeline from `since`, waiting up to `timeout_ms` (capped at
    /// [`WAIT_CAP_MS`]) for an entry past it.
    pub(super) async fn project_status(
        &self,
        project: &ProjectId,
        since: Option<u64>,
        timeout_ms: u32,
    ) -> Outcome {
        let wait = Duration::from_millis(u64::from(timeout_ms.min(WAIT_CAP_MS)));
        let deadline = tokio::time::Instant::now().checked_add(wait);
        let mut head = self.inner.head.subscribe();
        loop {
            // Seen before the read: a change logged after it wakes the wait below.
            head.borrow_and_update();
            let read = Self::status_of(&mut self.inner.state.lock(), project, since);
            let status = match read {
                Ok(status) => status,
                Err(refused) => return refused,
            };
            let news = since.is_none_or(|since| status.next > since);
            if news || timeout_ms == 0 {
                return Outcome::Project(Box::new(status));
            }
            let woke = match deadline {
                Some(at) => tokio::time::timeout_at(at, head.changed()).await,
                None => Ok(head.changed().await),
            };
            if !matches!(woke, Ok(Ok(()))) {
                return Outcome::Project(Box::new(status));
            }
        }
    }

    /// A project change the store answers at once: made once per key, logged and pushed.
    pub(super) fn project_change(
        &self,
        caller: Caller,
        key: Option<IdempotencyKey>,
        verb: &Verb,
    ) -> Outcome {
        if caller == Caller::Agent && names_verifier(verb) {
            return error(
                ErrorCode::Forbidden,
                "a verifier is the person's word on what counts as done, so only the person \
                 names one; say in the task's brief what should be checked",
            );
        }
        if caller == Caller::Agent && names_push(verb) {
            return error(
                ErrorCode::Forbidden,
                "whether merged work is pushed to the forge is the person's choice, so only the \
                 person sets it",
            );
        }
        if caller == Caller::Agent && names_ask_to_start(verb) {
            return error(
                ErrorCode::Forbidden,
                "whether each task waits for the person to start it is the person's choice, so \
                 only the person sets it",
            );
        }
        let mut guard = self.inner.state.lock();
        let state = &mut *guard;
        if let Some(key) = &key
            && let Some(answer) = keyed(state, caller, key, verb)
        {
            return answer;
        }
        let now = WallMs::now();
        let (terminals, agents) = live(state);
        // A copy, as a change may name a start's task while the counts are read.
        let starting = state.starting.clone();
        let running = Running { terminals: &terminals, agents: &agents, starting: &starting };
        let task = |t: Task| Outcome::Task(Box::new(t));
        let status = |s: ProjectStatus| Outcome::Project(Box::new(s));
        let mut named = None;
        let mut kick = None;
        let mut told = None;
        let answered = match verb.clone() {
            Verb::ProjectCreate {
                project,
                title,
                members,
                repo,
                target,
                verifier,
                review,
                push,
                ask_to_start,
                orchestrator,
                limits,
                metadata,
            } => known_term(state, orchestrator).and_then(|()| {
                let new = NewProject {
                    id: project,
                    title,
                    members,
                    repo,
                    target,
                    verifier,
                    review,
                    push,
                    ask_to_start,
                    orchestrator,
                    limits,
                    metadata,
                };
                named = orchestrator.map(|_| new.id.clone());
                state.projects.create(new, &running, now).map(|(s, u)| (status(s), u))
            }),
            Verb::ProjectSet {
                project,
                members,
                orchestrator,
                verifier,
                review,
                push,
                ask_to_start,
                limits,
                metadata,
            } => known_term(state, orchestrator).and_then(|()| {
                let before = state.projects.status(&project, None, &running).ok();
                let change = ProjectChange {
                    members,
                    orchestrator,
                    verifier,
                    review,
                    push,
                    ask_to_start,
                    limits,
                    metadata,
                };
                let set = state.projects.set(&project, change, &running, now)?;
                let was = before.and_then(|b| b.project.orchestrator);
                if orchestrator.is_some() && set.0.project.orchestrator != was {
                    named = Some(project);
                }
                Ok((status(set.0), set.1))
            }),
            Verb::ProjectNeeds { project, needs } => {
                let depth = state.projects.policy().bounds_for(Some(&project)).comprehension_depth;
                needs_compile(&needs, depth).and_then(|()| {
                    let set = state.projects.set_needs(&project, needs, &running, now)?;
                    Ok((status(set.0), set.1))
                })
            }
            Verb::TaskCreate { project, spec } => {
                state.projects.create_task(&project, *spec, now).map(|(t, u)| (task(t), u))
            }
            Verb::TaskClaim { project, task: id, paths } => {
                state.projects.claim(&project, id, &paths, now).map(|(t, u)| (task(t), u))
            }
            Verb::TaskTell { project, task: id, text } => {
                state.projects.tell(&project, id, &text, &terminals, now).map(|(words, u)| {
                    told = Some((project, id, words));
                    (Outcome::Done, u)
                })
            }
            Verb::TaskUpdate { project, task: id, change } => state
                .projects
                .update_task(&project, id, *change, caller, now)
                .map(|(t, u)| (task(t), u)),
            Verb::TaskAssign { project, task: id, term } => {
                // A terminal its worker has opened but not yet announced is live already: an
                // orchestrator that starts an agent and assigns it at once is not refused.
                let opening = state.starting.iter().find(|s| s.term == term);
                let theirs =
                    opening.and_then(|s| s.task.as_ref()).filter(|t| **t != (project.clone(), id));
                let known = match (opening, theirs) {
                    (_, Some((p, t))) => Err(error(
                        ErrorCode::Conflict,
                        &format!("that terminal is being started for task {t} of {p}"),
                    )),
                    (Some(_), None) => Ok(()),
                    (None, None) => known_term(state, Some(term)),
                };
                let room = state.projects.room_for(&project, term, &running);
                known.and(room).and_then(|()| {
                    let branch = branch_of(state, term);
                    let who = Assignee {
                        term,
                        spawned: false,
                        branch: branch.as_ref(),
                        conversation: None,
                        placed: None,
                    };
                    let mut open = terminals.clone();
                    open.insert(term);
                    let assigned = state.projects.assign(&project, id, who, &open, now)?;
                    // Until it is announced, the start counts against its project's limits.
                    if let Some(s) = state.starting.iter_mut().find(|s| s.term == term) {
                        s.task = Some((project, id));
                    }
                    Ok((task(assigned.0), assigned.1))
                })
            }
            Verb::TaskReport { project, task: id, report } => {
                let reported = state.projects.report_task(&project, id, &report, now);
                if reported.is_ok() && report.kind == ReportKind::Done {
                    self.bring_home_soon(state, (project.clone(), id), report.branch.clone());
                }
                reported.map(|((t, parent), u)| {
                    let at = tokio::time::Instant::now();
                    state.deliveries.add((project, parent), Some(id), report, at);
                    self.inner.deliver.notify_one();
                    (task(t), u)
                })
            }
            Verb::TaskMerge { .. } if caller == Caller::Agent => Err(error(
                ErrorCode::Forbidden,
                "only the person asks for a merge; a task whose verifier passes joins the merge \
                 queue by itself, so report it done",
            )),
            Verb::TaskMerge { project, task: id } => {
                let checked = state.projects.may_merge(&project, id);
                if checked.is_ok() && self.merge_when_home(state, (&project, id)) {
                    state.projects.task(&project, id).map(|t| (task(t.clone()), Vec::new()))
                } else {
                    if checked.is_ok() {
                        self.let_go(state, (&project, id));
                    }
                    let asked = checked.and_then(|()| state.projects.ask_merge(&project, id, now));
                    if asked.is_ok() {
                        kick = Some(project);
                    }
                    asked.map(|(t, u)| (task(t), u))
                }
            }
            _other => Err(error(ErrorCode::Invalid, "not a project change")),
        };
        let outcome = match answered {
            Ok((outcome, updates)) => {
                self.projects_moved(state, updates);
                outcome
            }
            Err(refused) => refused,
        };
        if let Some(project) = kick {
            self.kick(state, &project);
        }
        if let Some((project, task, words)) = told {
            state.deliveries.person(project, task, &words, tokio::time::Instant::now());
            self.inner.deliver.notify_one();
        }
        if let Some(term) =
            named.as_ref().and_then(|p| state.projects.project(p).ok()?.orchestrator)
        {
            self.repo_seen(state, term);
        }
        if let Some(project) = named
            && let Ok(record) = state.projects.project(&project)
        {
            let clones = clones_of(state, record);
            let role = orchestrator_role(record, &clones);
            let words = Report {
                kind: ReportKind::NeedsInput,
                note: role,
                artifacts: Vec::new(),
                branch: None,
                pr: None,
            };
            state.deliveries.add((project, None), None, words, tokio::time::Instant::now());
            self.inner.deliver.notify_one();
        }
        if let Some(key) = key {
            remember(state, caller, key, verb, &outcome);
        }
        drop(guard);
        outcome
    }

    /// One node of a project in full.
    pub(super) fn task_get(&self, project: &ProjectId, task: Option<TaskId>) -> Outcome {
        let node = self.inner.state.lock().projects.node(project, task);
        match node {
            Ok(node) => Outcome::Node(Box::new(node)),
            Err(refused) => refused,
        }
    }

    /// What the terminal `session` names works on, by the server's record.
    pub(super) fn working_on(&self, session: SessionId) -> Outcome {
        let state = self.inner.state.lock();
        let on = term_of(&state, session).and_then(|term| state.projects.working_on(term));
        drop(state);
        Outcome::WorkingOn(on)
    }

    /// What `term`'s summary says of its repository goes to the projects it orchestrates.
    pub(super) fn repo_seen(&self, state: &mut State, term: TermRef) {
        let id = state
            .workers
            .get(&term.worker)
            .and_then(|e| e.sessions.iter().find(|s| s.id == term.session))
            .and_then(|s| s.repo_id.clone());
        if let Some(id) = id {
            let updates = state.projects.repo_seen(term, &id);
            self.projects_moved(state, updates);
        }
    }

    /// Every worker's facts, or one's.
    pub(super) fn worker_facts(&self, only: Option<WorkerId>) -> Outcome {
        let mut guard = self.inner.state.lock();
        let state = &mut *guard;
        if let Some(worker) = only
            && !state.workers.contains_key(&worker)
        {
            return super::unknown_worker(worker);
        }
        let (terminals, agents) = live(state);
        let running = Running { terminals: &terminals, agents: &agents, starting: &state.starting };
        let mut out: Vec<(String, WorkerFacts)> = state
            .workers
            .values()
            .filter(|e| only.is_none_or(|w| w == e.info.worker))
            .map(|e| {
                let here = state.projects.live_on_worker(e.info.worker, &running);
                let facts = facts_of(e, here, made_on(state, e.info.worker));
                (e.info.name.clone(), WorkerFacts { worker: e.info.worker, facts })
            })
            .collect();
        drop(guard);
        out.sort_by(|(a, x), (b, y)| a.cmp(b).then(x.worker.cmp(&y.worker)));
        Outcome::Facts(out.into_iter().map(|(_, f)| f).collect())
    }

    /// The candidates for a start in `project`, read together: every worker with its facts,
    /// the project's and the fleet's live agents on it, where the project's tasks run, and the
    /// caps a ranking is held to.
    fn candidates(state: &mut State, project: Option<&ProjectId>) -> Gathered {
        let (terminals, agents) = live(state);
        let running = Running { terminals: &terminals, agents: &agents, starting: &state.starting };
        let bounds = state.projects.policy().bounds_for(project);
        let candidates = state
            .workers
            .values()
            .map(|e| {
                let worker = e.info.worker;
                let fleet_live = state.projects.live_on_worker(worker, &running);
                Candidate {
                    worker,
                    name: e.info.name.clone(),
                    online: e.info.liveness == Liveness::Online && e.link.is_some(),
                    reported: !e.facts.is_empty(),
                    facts: facts_of(e, fleet_live, made_on(state, worker)),
                    live: project.map_or(0, |p| state.projects.live_on(p, worker, &running)),
                    fleet_live,
                }
            })
            .collect();
        let peers = project.map(|p| state.projects.peers(p, &running)).unwrap_or_default();
        let per_worker =
            project.and_then(|p| state.projects.limits(p).ok()).map(|l| l.live_per_worker);
        let ranking = Ranking {
            per_worker,
            fleet_per_worker: Some(bounds.live_per_worker),
            comprehensions: bounds.comprehension_depth,
            until: None,
            agent: None,
        };
        Gathered { candidates, peers, ranking, named: Vec::new() }
    }

    /// Rank `gathered` for `placement` on the blocking pool, [`RANKERS`] at a time, judging
    /// for at most [`RANK_DEADLINE`]: the rules are agents' own and take their time, never the
    /// hub's lock or a runtime thread.
    async fn rank(
        &self,
        placement: Placement,
        gathered: Gathered,
    ) -> Result<Vec<Suggestion>, Outcome> {
        let start = std::time::Instant::now();
        let busy = || {
            error(ErrorCode::Limit, "placement is busy with other rankings; ask again in a moment")
        };
        let turn = Arc::clone(&self.inner.rankers).acquire_owned();
        let Ok(Ok(permit)) = tokio::time::timeout(RANK_DEADLINE, turn).await else {
            return Err(busy());
        };
        let Gathered { candidates, peers, mut ranking, named } = gathered;
        ranking.until = start.checked_add(RANK_DEADLINE);
        let ranked = tokio::task::spawn_blocking(move || {
            let _turn = permit;
            placement::rank(&placement, &candidates, &peers, ranking)
        })
        .await;
        let mut ranked = ranked
            .unwrap_or_else(|e| Err(error(ErrorCode::Failed, &format!("ranking ended: {e}"))))?;
        // A rule a need brought says the need's name.
        for reason in ranked.iter_mut().flat_map(|s| s.reasons.iter_mut()) {
            reason.need =
                named.iter().find(|(rule, _)| *rule == reason.rule).map(|(_, need)| need.clone());
        }
        Ok(ranked)
    }

    /// Rank every worker for a placement.
    pub(super) async fn placement_suggest(
        &self,
        project: Option<&ProjectId>,
        task: Option<TaskId>,
        placement: Option<Placement>,
    ) -> Outcome {
        let inputs =
            Self::suggestion_inputs(&mut self.inner.state.lock(), project, task, placement);
        let (placement, gathered) = match inputs {
            Ok(inputs) => inputs,
            Err(refused) => return refused,
        };
        match self.rank(placement, gathered).await {
            Ok(ranked) => Outcome::Suggestions(ranked),
            Err(refused) => refused,
        }
    }

    /// The placement to rank and what it is ranked over.
    fn suggestion_inputs(
        state: &mut State,
        project: Option<&ProjectId>,
        task: Option<TaskId>,
        placement: Option<Placement>,
    ) -> Result<(Placement, Gathered), Outcome> {
        let wanted = match (project, task, placement) {
            (_, _, Some(placement)) => Wanted { placement, ..Wanted::default() },
            (Some(p), Some(t), None) => {
                // A start already proposed says which agent it runs.
                let proposed = state.projects.task(p, t)?.proposal.as_ref();
                let agent = proposed.and_then(|proposal| agent_of(&proposal.launch.run));
                Wanted { agent, ..Self::needed(state, p, t)? }
            }
            (None, Some(_), None) => {
                return Err(error(ErrorCode::Invalid, "a task is named within its project"));
            }
            (_, None, None) => Wanted::default(),
        };
        if let Some(p) = project {
            state.projects.limits(p)?;
        }
        let gathered = Self::candidates(state, project).wanting(&wanted);
        Ok((wanted.placement, gathered))
    }

    /// Start a plain agent under an id the hub chooses: checked against the person's bounds
    /// and counted from the moment it is placed, like a task's.
    pub(super) async fn spawn_agent(
        &self,
        caller: Caller,
        from: Option<SessionId>,
        key: Option<IdempotencyKey>,
        verb: Verb,
    ) -> Outcome {
        let sent = digest(caller, &verb);
        if let Some(answer) = self.start_keyed(key.as_ref(), sent).await {
            return answer;
        }
        let Verb::SpawnAgent { worker, agent, cwd, prompt, args, env, size, .. } = verb else {
            return error(ErrorCode::Invalid, "not an agent's start");
        };
        let admitted = Self::admit_agent(&mut self.inner.state.lock(), caller, from, worker, &args);
        let (id, term, permission_flags) = match admitted {
            Ok(admitted) => admitted,
            Err(refused) => return refused,
        };
        // An agent's own starts never begin looser than `default`; a person's keep their
        // settings' mode.
        let args = match caller {
            Caller::Agent => {
                let mut state = self.inner.state.lock();
                watch(&mut state, term, |w| {
                    w.drove = Some(Drove::Opened { by: from });
                    w.locked |= !permission_flags;
                });
                drop(state);
                started_args(args, permission_flags, None).0
            }
            Caller::Person => args,
        };
        let session = Some(term.session);
        let start = Verb::SpawnAgent {
            worker,
            agent,
            cwd,
            prompt,
            args,
            env,
            size,
            session,
            permission_flags,
        };
        if let Some(key) = &key {
            keep_start(&mut self.inner.state.lock(), key.clone(), sent, &start);
        }
        self.forward_placed(id, key, start).await
    }

    /// Forward the start placed as `id`, detached from its caller: what the worker opens is
    /// counted and its answer kept under `key` even when the caller is gone.
    async fn forward_placed(&self, id: u64, key: Option<IdempotencyKey>, start: Verb) -> Outcome {
        let hub = self.clone();
        let started = tokio::spawn(async move {
            let placed = InFlight { hub: &hub, id, settled: false };
            let outcome = hub.forward(key.clone(), start).await;
            if matches!(outcome, Outcome::Opened(_)) || maybe_done(&outcome) {
                placed.answered();
            }
            if let Some(key) = &key {
                start_answered(&mut hub.inner.state.lock(), key, &outcome);
            }
            outcome
        });
        started.await.unwrap_or_else(|e| error(ErrorCode::Failed, &format!("the start ended: {e}")))
    }

    /// Place a plain agent's start on `worker`, if the person's bounds allow it: its id, its
    /// terminal, and whether it may loosen its permissions.
    fn admit_agent(
        state: &mut State,
        caller: Caller,
        from: Option<SessionId>,
        worker: WorkerId,
        args: &[String],
    ) -> Result<(u64, TermRef, bool), Outcome> {
        let bounds = state.projects.policy().bounds_for(None);
        let allowed = allowance(state, caller, from, None);
        if !allowed && let Some(flag) = loosening(args) {
            return Err(loosened(&flag, None));
        }
        let (terminals, agents) = live(state);
        let running = Running { terminals: &terminals, agents: &agents, starting: &state.starting };
        fleet_room(state, &running, bounds, Some(worker))?;
        let (id, term) = Self::place(state, worker, None, true, None);
        Ok((id, term, allowed))
    }

    /// Open a terminal under an id the hub chooses; one whose command is `claude` with flags
    /// that loosen its permissions is refused. One an agent opens is the agent's: the CLI in it
    /// speaks for an agent.
    pub(super) async fn open_terminal(
        &self,
        caller: Caller,
        from: Option<SessionId>,
        key: Option<IdempotencyKey>,
        verb: Verb,
    ) -> Outcome {
        let sent = digest(caller, &verb);
        if let Some(answer) = self.start_keyed(key.as_ref(), sent).await {
            return answer;
        }
        let Verb::OpenTerminal { worker, cwd, command, env, name, size, .. } = verb else {
            return error(ErrorCode::Invalid, "not a terminal's start");
        };
        if let Some(args) = claude_args(&command) {
            let allowed = allowance(&self.inner.state.lock(), caller, from, None);
            if let Some(flag) = loosening(&args).filter(|_| !allowed) {
                return loosened(&flag, None);
            }
        }
        // An agent's terminal counts as an agent's start from now: it may run one there.
        let placed = match caller {
            Caller::Agent => {
                let admitted = Self::admit_terminal(&mut self.inner.state.lock(), worker);
                match admitted {
                    Ok(placed) => Some(placed),
                    Err(refused) => return refused,
                }
            }
            Caller::Person => None,
        };
        let session = placed.map_or_else(SessionId::new, |(_, term)| term.session);
        let start =
            Verb::OpenTerminal { worker, cwd, command, env, name, size, session: Some(session) };
        Self::opening(&mut self.inner.state.lock(), caller, from, key.as_ref(), sent, &start);
        if let Some((id, _)) = placed {
            return self.forward_placed(id, key, start).await;
        }
        let outcome = self.forward(key.clone(), start).await;
        if let Some(key) = &key {
            start_answered(&mut self.inner.state.lock(), key, &outcome);
        }
        outcome
    }

    /// Place an agent's terminal on `worker`, if the person's bounds allow another agent
    /// there: its start's id and its terminal.
    fn admit_terminal(state: &mut State, worker: WorkerId) -> Result<(u64, TermRef), Outcome> {
        let bounds = state.projects.policy().bounds_for(None);
        let (terminals, agents) = live(state);
        let running = Running { terminals: &terminals, agents: &agents, starting: &state.starting };
        fleet_room(state, &running, bounds, Some(worker))?;
        Ok(Self::place(state, worker, None, true, None))
    }

    /// Note a terminal's start about to be forwarded: an agent's is driven by it, and a keyed
    /// one is kept for a repeat.
    fn opening(
        state: &mut State,
        caller: Caller,
        from: Option<SessionId>,
        key: Option<&IdempotencyKey>,
        sent: blake3::Hash,
        start: &Verb,
    ) {
        if caller == Caller::Agent
            && let Verb::OpenTerminal { worker, session: Some(session), .. } = start
        {
            let term = TermRef { worker: *worker, session: *session };
            watch(state, term, |w| w.drove = Some(Drove::Opened { by: from }));
        }
        if let Some(key) = key {
            keep_start(state, key.clone(), sent, start);
        }
    }

    /// The answer to a repeat of a keyed start (`sent` as its caller sent it): the first
    /// start forwarded again, which its worker answers as it answered the first, or a refusal
    /// of the key used with other arguments; `None` when there is no key or it is new.
    async fn start_keyed(
        &self,
        key: Option<&IdempotencyKey>,
        sent: blake3::Hash,
    ) -> Option<Outcome> {
        let key = key?;
        let again = start_again(&mut self.inner.state.lock(), key, sent)?;
        Some(match again {
            Ok(Again::Forward(start)) => {
                let outcome = self.forward(Some(key.clone()), start).await;
                start_answered(&mut self.inner.state.lock(), key, &outcome);
                outcome
            }
            Ok(Again::Answer(outcome)) => outcome,
            Err(refused) => refused,
        })
    }

    /// Hold a start's place: its id and the terminal it opens, under an id chosen here.
    fn place(
        state: &mut State,
        worker: WorkerId,
        task: Option<(ProjectId, TaskId)>,
        agent: bool,
        placed: Option<Placed>,
    ) -> (u64, TermRef) {
        state.next_start = state.next_start.wrapping_add(1);
        let id = state.next_start;
        let term = TermRef { worker, session: SessionId::new() };
        let since = tokio::time::Instant::now();
        let conversation = None;
        state.starting.push(Starting {
            id,
            term,
            task,
            agent,
            since,
            answered: false,
            conversation,
            placed,
        });
        (id, term)
    }

    /// Start what runs for a task where its pin or placement says, and put its terminal on
    /// the task. The start goes on if its caller leaves, so what the worker opens is always
    /// counted and assigned.
    pub(super) async fn task_spawn(
        &self,
        caller: Caller,
        key: Option<IdempotencyKey>,
        project: ProjectId,
        task: TaskId,
        launch: TaskLaunch,
    ) -> Outcome {
        let verb = Verb::TaskSpawn { project: project.clone(), task, launch: launch.clone() };
        if let Some(key) = &key
            && let Some(answer) = keyed(&mut self.inner.state.lock(), caller, key, &verb)
        {
            return answer;
        }
        let hub = self.clone();
        let started = tokio::spawn(async move {
            let asks =
                hub.inner.state.lock().projects.project(&project).is_ok_and(|p| p.ask_to_start);
            let outcome = if caller == Caller::Agent && asks {
                hub.propose(&project, task, launch).await
            } else {
                hub.start_task_once(key.clone(), &project, task, launch).await
            };
            if let Some(key) = key {
                remember(&mut hub.inner.state.lock(), caller, key, &verb, &outcome);
            }
            outcome
        });
        started.await.unwrap_or_else(|e| error(ErrorCode::Failed, &format!("the start ended: {e}")))
    }

    /// A start the person must say yes to: kept on the task with where the server would put
    /// it now and why, and answered with the task. Nothing is reserved: the start is checked
    /// in full when the person makes it.
    async fn propose(&self, project: &ProjectId, task: TaskId, launch: TaskLaunch) -> Outcome {
        let inputs = {
            let mut state = self.inner.state.lock();
            Self::loosens(&state, project, &launch)
                .and_then(|()| Self::placement_for(&state, project, task, &launch))
                .map(|wanted| {
                    let gathered = Self::candidates(&mut state, Some(project)).wanting(&wanted);
                    (wanted.placement, gathered)
                })
        };
        let (placement, gathered) = match inputs {
            Ok(inputs) => inputs,
            Err(refused) => return refused,
        };
        let (on, why) = match self.rank(placement.clone(), gathered).await {
            Ok(ranked) => match placement::choose(&placement, &ranked) {
                Ok(worker) => (Some(worker), placed_on(&ranked, worker).map(|p| p.why)),
                Err(why) => (None, Some(format!("no worker fits now: {why}"))),
            },
            Err(Outcome::Error { message, .. }) => (None, Some(message)),
            Err(_) => (None, None),
        };
        let runs = match &launch.run {
            Runner::Claude { .. } => CLAUDE.to_owned(),
            Runner::Codex { .. } => codex::PROGRAM.to_owned(),
            Runner::Command { argv } => {
                argv.first().map_or("a shell", |p| p.rsplit('/').next().unwrap_or(p)).to_owned()
            }
        };
        let mut why = why.unwrap_or_default();
        if why.len() > Suggestion::WHY_MAX {
            let mut cut = Suggestion::WHY_MAX;
            while !why.is_char_boundary(cut) {
                cut = cut.saturating_sub(1);
            }
            why.truncate(cut);
        }
        let proposed = Proposed { since_ms: WallMs::now(), runs, on, why };
        let mut state = self.inner.state.lock();
        let proposal = Proposal { launch, proposed };
        match state.projects.propose(project, task, proposal, WallMs::now()) {
            Ok((task, updates)) => {
                self.projects_moved(&mut state, updates);
                Outcome::Task(Box::new(task))
            }
            Err(refused) => refused,
        }
    }

    /// Start a task its orchestrator proposed, as the person asks: as proposed, on `pin` when
    /// they chose a worker.
    pub(super) async fn task_start(
        &self,
        key: Option<IdempotencyKey>,
        project: ProjectId,
        task: TaskId,
        pin: Option<WorkerId>,
    ) -> Outcome {
        let verb = Verb::TaskStart { project: project.clone(), task, pin };
        if let Some(key) = &key
            && let Some(answer) = keyed(&mut self.inner.state.lock(), Caller::Person, key, &verb)
        {
            return answer;
        }
        let proposed = {
            let state = self.inner.state.lock();
            state
                .projects
                .task(&project, task)
                .map(|t| t.proposal.as_ref().map(|p| p.launch.clone()))
        };
        let mut launch = match proposed {
            Ok(Some(launch)) => launch,
            Ok(None) => {
                return error(
                    ErrorCode::Invalid,
                    &format!(
                        "task {task} has no start proposed; its orchestrator proposes one with \
                         task_spawn"
                    ),
                );
            }
            Err(refused) => return refused,
        };
        launch.pin = pin.or(launch.pin);
        let hub = self.clone();
        let started = tokio::spawn(async move {
            let outcome = hub.start_task_once(key.clone(), &project, task, launch).await;
            if let Some(key) = key {
                remember(&mut hub.inner.state.lock(), Caller::Person, key, &verb, &outcome);
            }
            outcome
        });
        started.await.unwrap_or_else(|e| error(ErrorCode::Failed, &format!("the start ended: {e}")))
    }

    /// Refused when `launch` would loosen its agent's permissions and the person does not let
    /// this project's agents.
    fn loosens(state: &State, project: &ProjectId, launch: &TaskLaunch) -> Result<(), Outcome> {
        let bounds = state.projects.policy().bounds_for(Some(project));
        if bounds.permission_flags {
            return Ok(());
        }
        let flag = match &launch.run {
            Runner::Claude { args, .. } => loosening(args),
            Runner::Command { argv } => claude_args(argv)
                .as_deref()
                .and_then(loosening)
                .or_else(|| codex::args_of(argv).and_then(codex::loosening)),
            Runner::Codex { args, .. } => codex::loosening(args),
        };
        flag.map_or(Ok(()), |flag| Err(loosened(&flag, Some(project))))
    }

    /// What a start of `task` asks: the task's placement with its needs', pinned where the
    /// start says, beside a clone of the project's repository when it names no directory, on
    /// a worker with the agent it runs.
    fn placement_for(
        state: &State,
        project: &ProjectId,
        task: TaskId,
        launch: &TaskLaunch,
    ) -> Result<Wanted, Outcome> {
        let mut wanted = Self::needed(state, project, task)?;
        wanted.placement.pin = launch.pin.or(wanted.placement.pin);
        wanted.agent = agent_of(&launch.run);
        // With no directory named, it goes beside a clone of the project's repository.
        let repo = state.projects.project(project)?.repo_id.as_ref();
        if launch.cwd.trim().is_empty()
            && let Some(rule) = repo.and_then(beside_a_clone)
        {
            wanted.placement.require.push(rule);
        }
        Ok(wanted)
    }

    /// `task`'s own placement with the rules of each of its project's needs it has
    /// ([`slopty_proto::project::Need`]), each named after its need. A rule the task holds
    /// already is not added twice.
    ///
    /// # Errors
    /// Refused when the task's own rules and its needs' come to more than a placement holds.
    fn needed(state: &State, project: &ProjectId, task: TaskId) -> Result<Wanted, Outcome> {
        let mut placement = state.projects.task(project, task)?.placement.clone();
        let needs = state.projects.needs_of(project, task)?;
        let mut named = Vec::new();
        for need in &needs {
            for rule in &need.require {
                if !placement.require.iter().any(|r| r.trim() == rule.trim()) {
                    placement.require.push(rule.clone());
                }
                named.push((rule.trim().to_owned(), need.name.clone()));
            }
            for preference in &need.prefer {
                if !placement.prefer.iter().any(|p| p.expr.trim() == preference.expr.trim()) {
                    placement.prefer.push(preference.clone());
                }
                named.push((preference.expr.trim().to_owned(), need.name.clone()));
            }
        }
        let counts = [("require", placement.require.len()), ("prefer", placement.prefer.len())];
        let most = slopty_proto::project::RULES_MAX;
        if let Some((kind, n)) = counts.into_iter().find(|(_, n)| *n > most) {
            let names: Vec<&str> = needs.iter().map(|n| n.name.as_str()).collect();
            return Err(error(
                ErrorCode::BadExpression,
                &format!(
                    "task {task}'s own rules and those of its needs ({}) make {n} {kind} rules, \
                     over the {most} a placement may hold: fewer rules on the task or its \
                     needs",
                    names.join(", ")
                ),
            ));
        }
        Ok(Wanted { placement, named, agent: None })
    }

    /// Everything a start checks before it is placed, read together: the placement it ranks,
    /// and whether the person lets it loosen its permissions.
    fn may_start(
        state: &mut State,
        project: &ProjectId,
        task: TaskId,
        launch: &TaskLaunch,
    ) -> Result<(Wanted, bool), Outcome> {
        let bounds = state.projects.policy().bounds_for(Some(project));
        Self::loosens(state, project, launch)?;
        let (terminals, agents) = live(state);
        let running = Running { terminals: &terminals, agents: &agents, starting: &state.starting };
        state.projects.may_start(project, task, launch.ignore_dependencies, &running)?;
        fleet_room(state, &running, bounds, None)?;
        let wanted = Self::placement_for(state, project, task, launch)?;
        Ok((wanted, bounds.permission_flags))
    }

    /// Hold a place on `worker` for a start, read again: others may have started while the
    /// rules ran.
    fn reserve(
        state: &mut State,
        (project, task): (&ProjectId, TaskId),
        launch: &TaskLaunch,
        (worker, placed): (WorkerId, Option<Placed>),
    ) -> Result<(u64, TermRef), Outcome> {
        Self::may_start(state, project, task, launch)?;
        let (terminals, agents) = live(state);
        let running = Running { terminals: &terminals, agents: &agents, starting: &state.starting };
        let bounds = state.projects.policy().bounds_for(Some(project));
        let here = state.projects.live_on(project, worker, &running);
        let per_worker = state.projects.limits(project)?.live_per_worker;
        if here >= per_worker {
            let message = format!(
                "no worker can take task {task} now: the one chosen filled up to the project's \
                 live_per_worker meanwhile"
            );
            return Err(error(ErrorCode::Unplaced, &message));
        }
        fleet_room(state, &running, bounds, Some(worker))?;
        let agent = matches!(launch.run, Runner::Claude { .. } | Runner::Codex { .. });
        Ok(Self::place(state, worker, Some((project.clone(), task)), agent, placed))
    }

    /// Put the terminal a start opened on its task, and push the change.
    fn assign_started(
        &self,
        state: &mut State,
        (project, task): (&ProjectId, TaskId),
        term: TermRef,
        conversation: Option<String>,
    ) -> Result<Task, Outcome> {
        let (mut terminals, _) = live(state);
        terminals.insert(term);
        let branch = branch_of(state, term);
        let placed = state.starting.iter().find(|s| s.term == term).and_then(|s| s.placed.clone());
        let who = Assignee { term, spawned: true, branch: branch.as_ref(), conversation, placed };
        let (task, updates) =
            state.projects.assign(project, task, who, &terminals, WallMs::now())?;
        state.starting.retain(|s| s.term != term);
        self.projects_moved(state, updates);
        Ok(task)
    }

    /// Where a task with no directory goes when no worker with a clone of its repository fits:
    /// the worker its rules would choose but for the clone, when there is an address to clone
    /// from.
    async fn placed_for_a_clone(
        &self,
        project: &ProjectId,
        launch: &TaskLaunch,
        wanted: &Wanted,
    ) -> Option<(WorkerId, Option<Placed>)> {
        if !launch.cwd.trim().is_empty() {
            return None;
        }
        let (bare, gathered) = self.without_the_clone_rule(project, wanted)?;
        let ranked = self.rank(bare.clone(), gathered).await.ok()?;
        let worker = placement::choose(&bare, &ranked).ok()?;
        Some((worker, placed_on(&ranked, worker)))
    }

    /// `placement` without the rule a task with no directory gets, and the workers to rank it
    /// over; `None` when there is no address to clone the project's repository from.
    fn without_the_clone_rule(
        &self,
        project: &ProjectId,
        wanted: &Wanted,
    ) -> Option<(Placement, Gathered)> {
        let mut state = self.inner.state.lock();
        let id = state.projects.project(project).ok()?.repo_id.clone()?;
        id.url.as_ref()?;
        let rule = beside_a_clone(&id)?;
        let mut bare = wanted.placement.clone();
        bare.require.retain(|r| *r != rule);
        let gathered = Self::candidates(&mut state, Some(project)).wanting(wanted);
        drop(state);
        Some((bare, gathered))
    }

    /// The address to clone the project's repository from onto `worker`, when a task with no
    /// directory goes there and it has no clone.
    fn clone_needed(
        &self,
        project: &ProjectId,
        launch: &TaskLaunch,
        worker: WorkerId,
    ) -> Option<String> {
        if !launch.cwd.trim().is_empty() {
            return None;
        }
        let state = self.inner.state.lock();
        let record = state.projects.project(project).ok()?;
        let url = record.repo_id.as_ref()?.url.clone()?;
        let has = clone_on(&state, record, worker).is_some();
        drop(state);
        (!has).then_some(url)
    }

    async fn start_task_once(
        &self,
        key: Option<IdempotencyKey>,
        project: &ProjectId,
        task: TaskId,
        launch: TaskLaunch,
    ) -> Outcome {
        let inputs = {
            let mut state = self.inner.state.lock();
            Self::may_start(&mut state, project, task, &launch).map(|(wanted, _)| {
                let gathered = Self::candidates(&mut state, Some(project)).wanting(&wanted);
                (wanted, gathered)
            })
        };
        let (wanted, gathered) = match inputs {
            Ok(inputs) => inputs,
            Err(refused) => return refused,
        };
        let placement = &wanted.placement;
        let ranked = match self.rank(placement.clone(), gathered).await {
            Ok(ranked) => ranked,
            Err(refused) => return refused,
        };
        let chosen = match placement::choose(placement, &ranked) {
            Ok(worker) => Ok((worker, placed_on(&ranked, worker))),
            Err(why) => self.placed_for_a_clone(project, &launch, &wanted).await.ok_or(why),
        };
        let (worker, placed) = match chosen {
            Ok(chosen) => chosen,
            Err(why) => {
                let message = format!("no worker can take task {task} now: {why}");
                let repo = self.inner.state.lock().projects.project(project).ok().and_then(|p| {
                    let id = p.repo_id.as_ref()?;
                    Some((id.origin.clone().or_else(|| id.root.clone())?, beside_a_clone(id)?))
                });
                let message = match repo.filter(|_| launch.cwd.trim().is_empty()) {
                    Some((key, rule)) => format!(
                        "{message}. With no cwd it must go beside a clone of {key} ({rule}), and \
                         no address to clone it from is known: clone it on a worker that fits, \
                         or name a cwd"
                    ),
                    None => message,
                };
                return error(ErrorCode::Unplaced, &message);
            }
        };
        // A task with no directory on a worker with no clone of its repository gets one there.
        if let Some(url) = self.clone_needed(project, &launch, worker)
            && let Err(why) = self.clone_for((project, task), worker, url).await
        {
            return error(ErrorCode::Failed, &format!("task {task} needed a clone: {why}"));
        }
        let reserved = {
            let mut state = self.inner.state.lock();
            Self::reserve(&mut state, (project, task), &launch, (worker, placed)).and_then(
                |placed| {
                    let permission_flags =
                        state.projects.policy().bounds_for(Some(project)).permission_flags;
                    let (record, card) =
                        (state.projects.project(project)?, state.projects.task(project, task)?);
                    let clone =
                        launch.cwd.trim().is_empty().then(|| clone_on(&state, record, worker));
                    let at = clone.flatten().map(|path| {
                        let worktree = match &launch.run {
                            _ if card.read_only => None,
                            Runner::Claude { args, .. } if !names(args, &WORKTREE_FLAGS) => {
                                Some(Worktree::Named(format!("slopty-{project}-{task}")))
                            }
                            Runner::Codex { .. } => Some(Worktree::Codex),
                            Runner::Claude { .. } | Runner::Command { .. } => None,
                        };
                        Place { path, worktree }
                    });
                    let role = agent_role(record, card, at.as_ref());
                    if !permission_flags && matches!(launch.run, Runner::Claude { .. }) {
                        watch(&mut state, placed.1, |w| w.locked = true);
                    }
                    Ok((placed, permission_flags, role, at))
                },
            )
        };
        let ((id, term), permission_flags, role, at) = match reserved {
            Ok(reserved) => reserved,
            Err(refused) => return refused,
        };
        let placed = InFlight { hub: self, id, settled: false };
        let TaskLaunch { cwd, run, mut env, size, .. } = launch;
        let (cwd, worktree) = match at {
            Some(Place { path, worktree }) => (path, worktree),
            None => (cwd, None),
        };
        // Last, so they win over the caller's own.
        env.push((PROJECT_ENV.to_owned(), project.to_string()));
        env.push((TASK_ENV.to_owned(), task.to_string()));
        let session = Some(term.session);
        let (start, conversation) = match run {
            Runner::Claude { prompt, mut args } => {
                if let Some(Worktree::Named(name)) = worktree {
                    args.splice(0..0, [WORKTREE_FLAGS[0].to_owned(), name]);
                }
                let (args, conversation) = started_args(args, permission_flags, Some(role));
                let agent = AgentKind::ClaudeCode;
                let spawn = Verb::SpawnAgent {
                    worker,
                    agent,
                    cwd,
                    prompt,
                    args,
                    env,
                    size,
                    session,
                    permission_flags,
                };
                (spawn, conversation)
            }
            Runner::Command { argv } => {
                let open = Verb::OpenTerminal {
                    worker,
                    cwd: Some(cwd).filter(|c| !c.trim().is_empty()),
                    command: argv,
                    env,
                    name: Some(format!("{project} #{task}")),
                    size,
                    session,
                };
                (open, None)
            }
            // The worker gives it Slopty's tools as it opens (`Worker::as_agent`).
            Runner::Codex { prompt, args } => {
                let in_worktree = matches!(worktree, Some(Worktree::Codex));
                let open = Verb::OpenTerminal {
                    worker,
                    cwd: Some(cwd).filter(|c| !c.trim().is_empty()),
                    command: codex::command(&role, in_worktree, args, prompt),
                    env,
                    name: Some(format!("{project} #{task}")),
                    size,
                    session,
                };
                (open, None)
            }
        };
        // A refusal is the worker's answer, and a repeat under the key is the worker's to
        // answer; one whose answer was lost may still open, and is put on its task when its
        // worker announces it.
        let outcome = self.forward(key.map(|k| k.part("start")), start).await;
        let opened = match outcome {
            Outcome::Opened(opened) => opened,
            other if maybe_done(&other) => {
                if let Some(s) = self.inner.state.lock().starting.iter_mut().find(|s| s.id == id) {
                    s.conversation = conversation;
                }
                placed.answered();
                return other;
            }
            other => return other,
        };
        placed.answered();
        let assigned = self.assign_started(
            &mut self.inner.state.lock(),
            (project, task),
            opened,
            conversation,
        );
        match assigned {
            Ok(task) => Outcome::Task(Box::new(task)),
            Err(refused) => {
                // Nothing runs for a task that did not take it: what was started is closed.
                let _closed = self.forward(None, Verb::Close { term: opened }).await;
                refused
            }
        }
    }

    /// A terminal the hub started for a task, whose answer was lost, has shown up on its
    /// worker: it goes on its task now.
    pub(super) fn adopt(&self, state: &mut State, term: TermRef) {
        let Some(start) = state.starting.iter().find(|s| s.term == term && s.answered) else {
            return;
        };
        let Some((project, task)) = start.task.clone() else { return };
        let conversation = start.conversation.clone();
        match self.assign_started(state, (&project, task), term, conversation) {
            Ok(_) => tracing::info!(%project, %task, session = %term.session, "a lost start found"),
            Err(refused) => {
                tracing::warn!(%project, %task, ?refused, "a lost start found and not taken");
                state.starting.retain(|s| s.term != term);
            }
        }
    }

    /// Push every batch of reports due now to its terminal's worker, and say when the next
    /// falls due.
    pub(super) fn deliver_due(&self) -> Option<tokio::time::Instant> {
        let mut guard = self.inner.state.lock();
        let state = &mut *guard;
        let (terminals, _) = live(state);
        let projects = &state.projects;
        let now = tokio::time::Instant::now();
        let batches = state
            .deliveries
            .take(now, |(project, node)| projects.node_term(project, *node, &terminals));
        for batch in batches {
            Self::push_batch(state, &batch);
        }
        let next = state.deliveries.next_due();
        drop(guard);
        next
    }

    /// Send `batch` to its terminal's worker. One the link cannot take now stays outstanding,
    /// and goes again with the next batch or when the worker registers again.
    pub(super) fn push_batch(state: &State, batch: &Batch) {
        let Batch { term, number, context, .. } = batch;
        let link = state.workers.get(&term.worker).and_then(|e| e.link.as_ref());
        let msg =
            FromServer::Deliver { session: term.session, batch: *number, context: context.clone() };
        if let Some(link) = link
            && link.tx.try_send(msg).is_err()
        {
            tracing::debug!(session = %term.session, batch = number, "reports not sent now");
        }
    }

    /// The worker holding `term` handed batch `batch` to its agent. The timeline shows reports
    /// delivered; a batch of the server's own words alone (an orchestrator's role) is standing
    /// context, not an event: when an agent reads it follows only from when it starts.
    pub(super) fn delivered(&self, state: &mut State, term: TermRef, batch: u64) {
        let Some(((project, node), reports)) = state.deliveries.acked(term, batch) else { return };
        if reports > 0 {
            let updates = state.projects.delivered(&project, node, term, reports, WallMs::now());
            self.projects_moved(state, updates);
        }
        // What did not fit that batch may go now.
        self.inner.deliver.notify_one();
    }

    /// What the agent in `term` said its permission mode is, at its start and at each change.
    ///
    /// An agent the server started without looser permissions, or one in a terminal an agent
    /// opened or typed into, stays in a mode that asks (`default`, `plan`, `dontAsk`). A looser
    /// one came from a settings file or from keys typed into its TUI, and an agent may type as
    /// well as the person, so its terminal is closed and the timeline says why, unless the
    /// person allows looser modes for its project.
    pub(super) fn permission_mode(&self, state: &mut State, term: TermRef, mode: &str) {
        if SAFE_MODES.contains(&mode) || !Self::held_to_asking(state, term) {
            return;
        }
        let why = format!(
            "its agent went into {mode} mode, looser than the person allows \
             (`[server.projects] permission_flags`), so its terminal was closed"
        );
        self.close_looser(state, term, &why);
    }

    /// What the worker read off the command line of the agent in `term` that loosens its
    /// permissions, however it was started: a `claude` inside a shell's line (`sh -c "cd x &&
    /// claude --allowedTools Bash"`) passes the flag check at the start, which sees only the
    /// program it was handed. Judged as a looser mode is ([`Self::permission_mode`]).
    pub(super) fn loosened(&self, state: &mut State, term: TermRef, found: &[String]) {
        if found.is_empty() || !Self::held_to_asking(state, term) {
            return;
        }
        let shown: Vec<String> =
            found.iter().take(LOOSENED_MAX).map(|f| clipped(f, LOOSENED_ITEM_MAX)).collect();
        let why = format!(
            "its agent runs with {}, more than the person allows (`[server.projects] \
             permission_flags`), so its terminal was closed",
            shown.join(", ")
        );
        self.close_looser(state, term, &why);
    }

    /// Whether the agent in `term` must stay in a mode that asks: the server started it
    /// without looser permissions, or an agent opened or typed into its terminal, and the
    /// person allows nothing looser for the project it works for ([`project_of`]).
    fn held_to_asking(state: &State, term: TermRef) -> bool {
        let project = project_of(state, term);
        let allowed = state.projects.policy().bounds_for(project.as_ref()).permission_flags;
        let watched =
            state.watched.get(&term.session).is_some_and(|(w, _)| w.locked || w.drove.is_some());
        watched && !allowed
    }

    /// Close `term`, whose agent is looser than allowed, saying `why` on its task.
    fn close_looser(&self, state: &mut State, term: TermRef, why: &str) {
        tracing::warn!(session = %term.session, why, "agent looser than allowed; closing");
        if let Some((project, task)) = state.projects.working_on(term) {
            let updates = state.projects.note(&project, task, why, WallMs::now());
            self.projects_moved(state, updates);
        }
        let hub = self.clone();
        tokio::spawn(async move {
            let _closed = hub.forward(None, Verb::Close { term }).await;
        });
    }
}

/// What a ranking reads, gathered under the lock.
pub(super) struct Gathered {
    candidates: Vec<Candidate>,
    peers: BTreeMap<TaskId, WorkerId>,
    ranking: Ranking,
    /// Each rule the task's needs brought, and the need's name, for the reasons to say.
    named: Vec<(String, String)>,
}

impl Gathered {
    /// Ranked for `wanted`: the agent it runs must be installed, and its needs' rules say
    /// their names.
    fn wanting(mut self, wanted: &Wanted) -> Self {
        self.ranking.agent = wanted.agent;
        self.named.clone_from(&wanted.named);
        self
    }
}

/// What a start asks of the worker it goes to.
#[derive(Clone, Debug, Default)]
pub(super) struct Wanted {
    /// The task's own placement, with the rules of each of its project's needs it has.
    placement: Placement,
    /// Each rule a need brought, trimmed as a reason names it, and that need's name.
    named: Vec<(String, String)>,
    /// The agent it runs, by its name among a worker's `agents` facts.
    agent: Option<&'static str>,
}

/// The agent `run` starts, by its name among a worker's `agents` facts: a worker must have it
/// installed to be chosen. A command is judged by its program, so `claude …` run as a command
/// still needs Claude Code.
fn agent_of(run: &Runner) -> Option<&'static str> {
    match run {
        Runner::Claude { .. } => Some(CLAUDE),
        Runner::Codex { .. } => Some(codex::PROGRAM),
        Runner::Command { argv } => {
            let program = argv.first()?;
            match program.rsplit('/').next().unwrap_or(program) {
                CLAUDE => Some(CLAUDE),
                codex::PROGRAM => Some(codex::PROGRAM),
                _ => None,
            }
        }
    }
}

/// Claude Code's program, and its name among a worker's `agents` facts.
const CLAUDE: &str = "claude";

/// How often the watcher looks for checks falling due.
const CHECKS_TICK: Duration = Duration::from_secs(10);
/// How soon a pull request's checks are read again while some still run.
const CHECKS_RUNNING: Duration = Duration::from_secs(30);
/// How soon once they have settled: a push starts them again, and the next read sees it.
const CHECKS_SETTLED: Duration = Duration::from_secs(120);
/// How soon after the forge could not be asked: a worker away, a command not signed in.
const CHECKS_FAILED: Duration = Duration::from_secs(300);

impl Hub {
    /// Read every open task's pull request's own checks from its forge, on the worker its
    /// agent ran on and in the worktree it worked in, until the hub is gone: often while they
    /// run, seldom once they settle, and only when one falls due. The card shows them.
    pub async fn watch_checks(hub: WeakHub) {
        let mut due: HashMap<(ProjectId, TaskId), tokio::time::Instant> = HashMap::new();
        loop {
            tokio::time::sleep(CHECKS_TICK).await;
            let Some(hub) = hub.upgrade() else { return };
            hub.read_due_checks(&mut due).await;
        }
    }

    /// One round of [`Hub::watch_checks`]: read the checks that fall due by `due`, and say
    /// when each is due again.
    pub(crate) async fn read_due_checks(
        &self,
        due: &mut HashMap<(ProjectId, TaskId), tokio::time::Instant>,
    ) {
        let watched = self.inner.state.lock().projects.pull_requests();
        due.retain(|at, _| watched.iter().any(|w| (&w.project, w.task) == (&at.0, at.1)));
        let now = tokio::time::Instant::now();
        for watch in watched {
            let at = (watch.project.clone(), watch.task);
            if due.get(&at).is_some_and(|when| *when > now) {
                continue;
            }
            let next = self.read_checks(watch).await;
            due.insert(at, now.checked_add(next).unwrap_or(now));
        }
    }

    /// Ask `watch`'s worker for its pull request's checks and put them on its card; how soon
    /// to ask again.
    async fn read_checks(&self, watch: crate::project::PrWatch) -> Duration {
        let verb = Verb::PullChecks {
            worker: watch.worker,
            cwd: watch.cwd,
            number: watch.number,
            merge_request: watch.merge_request,
        };
        let checks = match self.forward(None, verb).await {
            Outcome::Checks(checks) => checks,
            other => {
                tracing::debug!(project = %watch.project, task = %watch.task, ?other, "no checks");
                return CHECKS_FAILED;
            }
        };
        let running = checks.state == slopty_proto::project::ChecksState::Pending;
        let mut state = self.inner.state.lock();
        let set = state.projects.set_checks(&watch.project, watch.task, checks, WallMs::now());
        match set {
            Ok(changes) => self.projects_moved(&mut state, changes),
            Err(refused) => tracing::debug!(?refused, "checks not kept"),
        }
        drop(state);
        if running { CHECKS_RUNNING } else { CHECKS_SETTLED }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    /// A shell in a clone at `repo`, identified by `origin` and `root`.
    fn shell_in(repo: &str, origin: Option<&str>, root: Option<&str>) -> SessionSummary {
        SessionSummary {
            id: SessionId::new(),
            title: "zsh".to_owned(),
            cwd: Some(repo.to_owned()),
            repo: Some(repo.to_owned()),
            branch: None,
            repo_id: Some(RepoId {
                origin: origin.map(str::to_owned),
                root: root.map(str::to_owned),
                url: None,
            }),
            changes: None,
            started_ms: WallMs::ZERO,
            cols: 80,
            rows: 24,
            state: slopty_proto::terminal::SessionState::Running,
            viewers: 0,
            command: Vec::new(),
            agent: None,
            progress: None,
            restored: None,
        }
    }

    /// One repository cloned on two workers is found on both by either key, and a rule
    /// places a task beside a clone of it; a worker with none has an empty `repos`.
    #[test]
    fn a_repository_is_a_fact_of_every_worker_with_a_clone() {
        let (origin, root) =
            ("github.com/aislopware/slopty", "c08d4c1e5b2a9f7d3e6a1b8c4d2f0e9a7b5c3d1e");
        let studio = [
            shell_in("/w/slopty-wt/board", Some(origin), Some(root)),
            shell_in("/w/slopty", Some(origin), Some(root)),
            shell_in("/w/notes", None, None),
        ];
        let linux = [shell_in("/home/c/slopty", None, Some(root))];
        let Fact::Map(on_studio) = repos_of(&studio, &[]) else { panic!("a map") };
        assert_eq!(
            on_studio.get(origin),
            Some(&Fact::Text("/w/slopty".to_owned())),
            "the first clone"
        );
        assert_eq!(on_studio.get(root), on_studio.get(origin));
        assert_eq!(on_studio.len(), 2, "a shell in no known repository adds nothing");

        let candidate = |name: &str, sessions: &[SessionSummary]| Candidate {
            worker: WorkerId::new(),
            name: name.to_owned(),
            online: true,
            reported: true,
            facts: Facts::from([("repos".to_owned(), repos_of(sessions, &[]))]),
            live: 0,
            fleet_live: 0,
        };
        let fleet =
            [candidate("studio", &studio), candidate("linux", &linux), candidate("bare", &[])];
        let fits = |rule: &str| {
            let placement = Placement { require: vec![rule.to_owned()], ..Placement::default() };
            let ranking = Ranking { comprehensions: 2, ..Ranking::default() };
            let ranked = placement::rank(&placement, &fleet, &BTreeMap::new(), ranking).unwrap();
            let mut fit: Vec<String> =
                ranked.into_iter().filter(|s| s.fits).map(|s| s.name).collect();
            fit.sort();
            fit
        };
        assert_eq!(fits(&format!("{root:?} in repos")), ["linux", "studio"]);
        assert_eq!(fits(&format!("{origin:?} in repos")), ["studio"]);
        assert_eq!(fits(&format!("repos[{origin:?}] == \"/w/slopty\"")), ["studio"]);
    }

    #[test]
    fn flags_that_loosen_permissions_are_found_in_either_form() {
        for loose in [
            "--dangerously-skip-permissions",
            "--model opus --permission-mode bypassPermissions",
            "--permission-mode=acceptEdits",
            "--permission-mode",
            "--allowedTools Bash",
            "--settings={\"permissions\":{\"allow\":[\"Bash\"]}}",
            "--mcp-config={}",
            "--add-dir /",
            "-- --dangerously-skip-permissions",
            "--permission-prompt-tool mcp__x__y",
        ] {
            assert!(loosening(&words(loose)).is_some(), "{loose}");
        }
        for safe in [
            "--model opus",
            "--permission-mode plan",
            "--permission-mode=default",
            "",
            "--resume abc --effort high",
            "--settings={\"model\":\"opus\"}",
        ] {
            assert_eq!(loosening(&words(safe)), None, "{safe}");
        }
        let argv = words("/usr/local/bin/claude --dangerously-skip-permissions");
        assert!(claude_args(&argv).as_deref().and_then(loosening).is_some());
        let wrapped = vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "cd x && claude --allowedTools Bash".to_owned(),
        ];
        assert!(
            claude_args(&wrapped).as_deref().and_then(loosening).is_some(),
            "inside a shell line"
        );
        let echoed =
            vec!["/bin/sh".to_owned(), "-c".to_owned(), "echo claude --allowedTools".to_owned()];
        assert_eq!(claude_args(&echoed), None, "claude only mentioned");
        assert_eq!(claude_args(&words("codex --yolo")), None, "only claude's flags are known");
    }

    /// An agent the server starts is pinned to `default` unless it names a mode or the person
    /// allows looser ones; a task's also gets a conversation id of the server's choosing and
    /// its role, unless its arguments pick the conversation.
    #[test]
    fn a_started_agent_begins_in_default_mode_under_a_chosen_conversation() {
        let (args, id) = started_args(words("--model opus"), false, None);
        assert_eq!(args, words("--permission-mode default --model opus"));
        assert_eq!(id, None);
        let (args, _) = started_args(words("--permission-mode plan"), false, None);
        assert_eq!(args, words("--permission-mode plan"));
        let (args, _) = started_args(Vec::new(), true, None);
        assert!(args.is_empty(), "the person allows looser modes: the settings' mode stands");
        let (args, id) = started_args(words("fix-it"), false, Some("be good".to_owned()));
        let id = id.expect("a conversation");
        assert!(id.parse::<SessionId>().is_ok(), "{id}");
        assert_eq!(
            args,
            [
                "--permission-mode",
                "default",
                "--session-id",
                &id,
                "--append-system-prompt=be good",
                "fix-it"
            ]
        );
        let (args, id) = started_args(words("--resume abc"), false, Some("r".to_owned()));
        assert_eq!(id, None, "the arguments pick the conversation");
        assert!(!args.iter().any(|a| a == "--session-id"), "{args:?}");
    }

    /// Projects too large for one frame go over several parts, each within its bytes: a
    /// project split over parts carries its record again with more of its tasks, its timeline
    /// in the first, and every task once.
    #[test]
    fn projects_are_sent_in_parts_that_each_fit_a_frame() {
        use std::collections::HashSet;

        use slopty_proto::project::{LimitsChange, TaskSpec};

        use crate::project::{NewProject, Projects};

        let none = HashSet::new();
        let running = Running { terminals: &none, agents: &none, starting: &[] };
        let mut p = Projects::default();
        let id = ProjectId::new("big").unwrap();
        let new = NewProject {
            id: id.clone(),
            title: "Big".to_owned(),
            repo: "~/src/big".to_owned(),
            target: "main".to_owned(),
            review: None,
            verifier: None,
            push: false,
            ask_to_start: false,
            orchestrator: None,
            limits: LimitsChange::default(),
            metadata: None,
            members: Vec::new(),
        };
        p.create(new, &running, WallMs::ZERO).unwrap();
        let spec = TaskSpec { title: "t".to_owned(), ..TaskSpec::default() };
        p.create_task(&id, spec, WallMs::ZERO).unwrap();
        let mut big = p.snapshot(&running).remove(0);
        let card = big.tasks.remove(0);
        big.tasks = (1..=2500_u32)
            .map(|n| slopty_proto::project::TaskCard {
                id: TaskId(n),
                title: "t".repeat(4096),
                ..card.clone()
            })
            .collect();
        let small = ProjectStatus { tasks: Vec::new(), ..big.clone() };
        let parts = parts(7, vec![small, big]);
        assert!(parts.len() >= 2, "{}", parts.len());
        assert!(parts.iter().all(|p| p.seq == 7));
        assert!(parts[0].first && !parts[0].last);
        assert!(parts.last().is_some_and(|p| p.last && !p.first));
        for part in &parts {
            let bytes: usize = part
                .projects
                .iter()
                .map(|s| {
                    s.project.approx_bytes()
                        + s.tasks
                            .iter()
                            .map(slopty_proto::project::TaskCard::approx_bytes)
                            .sum::<usize>()
                        + s.timeline.iter().map(TimelineEntry::approx_bytes).sum::<usize>()
                })
                .sum();
            assert!(bytes <= PART_BYTES, "{bytes}");
        }
        let cards: Vec<TaskId> = parts
            .iter()
            .flat_map(|p| &p.projects)
            .flat_map(|s| s.tasks.iter().map(|t| t.id))
            .collect();
        assert_eq!(cards, (1..=2500).map(TaskId).collect::<Vec<_>>(), "every task once, in order");
        let timelines = parts.iter().flat_map(|p| &p.projects).filter(|s| !s.timeline.is_empty());
        assert_eq!(timelines.count(), 2, "each project's timeline once");
    }
}
