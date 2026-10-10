//! The hub's side of projects (`docs/decisions/projects.md`): the verbs the store answers,
//! where each start goes, every start of an agent or a task's terminal counted against the
//! person's bounds from the moment it is placed, and the reports on their way up the tree.
//!
//! A start's terminal id is the hub's to choose (the start's token): the worker opens the
//! terminal under it, and a start asked again under it answers that terminal instead of
//! opening another. So a start whose answer was lost still counts, and its terminal is put on
//! its task when the worker announces it. A caller never chooses the id.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::agent::AgentBranch;
use slopty_proto::orchestration::{ErrorCode, IdempotencyKey, Outcome, TermRef, ThreadOf, Verb};
use slopty_proto::project::{
    ASKING_ENV, Bounds, Fact, Facts, LOOSENED_ITEM_MAX, LOOSENED_MAX, PERMISSION_MODE_FLAG,
    PROJECT_ENV, Project, ProjectId, ProjectStatus, ProjectsPart, Runner, SAFE_MODES, TASK_ENV,
    Task, TaskId, TaskLaunch, TaskState, TimelineEntry, WorkerFacts,
};
use slopty_proto::screen::VideoCodec;
use slopty_proto::server::{FromServer, Liveness, Os};
use slopty_proto::terminal::{RepoId, SessionSummary};
use slopty_proto::thread::wire::{NewWorktree, Start};
use slopty_proto::thread::{AgentId, ThreadId};

use super::{
    Again, Entry, Hub, State, WAIT_CAP_MS, branch_of, codex, digest, error, keep_start, keyed,
    known_term, remember, start_again, start_answered, term_of,
};
use crate::deliver::{Batch, plain};
use crate::placement::{self, Candidate, Installed, Wanted};
use crate::project::{
    Assignee, Caller, Drove, Keep, NewProject, Policy, ProjectChange, Running, Starting, Teller,
    Watched, clipped,
};

/// How long a start still counts once its worker answered (or its answer was lost), until its
/// terminal counts on its own: past it, the terminal is gone or never came.
pub(super) const STARTED_GRACE: Duration = Duration::from_secs(30);

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
    // A worker away holds no place: its agents cannot be seen at work, and a task of its may
    // start again elsewhere.
    for entry in state.workers.values().filter(|e| e.link.is_some()) {
        for s in &entry.sessions {
            let term = TermRef { worker: entry.info.worker, session: s.id };
            terminals.insert(term);
            // A terminal an agent opened or typed into counts as an agent's whatever runs in
            // it now: the agent may start one there at any moment, past every count.
            if state.board.agent_at(term).is_some() || driven(state, s.id) {
                agents.insert(term);
            }
        }
    }
    // A task's thread with no terminal of its own lives while its agent is there.
    let linked = |w: WorkerId| state.workers.get(&w).is_some_and(|e| e.link.is_some());
    for seat in state.board.live_seats().into_iter().filter(|seat| linked(seat.worker)) {
        terminals.insert(seat);
        agents.insert(seat);
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
/// is done. An agent works in the project it proves it works in. The orchestrator plans and
/// starts the tasks, one level of them: it makes them, starts them, tells them and changes them.
/// A task's agent works on its own task alone: it changes and reports on that one, and makes,
/// starts and tells none. Only an orchestrator makes a project. The terminals it puts to work are
/// the project's or its own ([`theirs`]), never the person's.
pub(super) fn agent_scope(
    state: &State,
    from: Option<SessionId>,
    verb: Verb,
) -> Result<Verb, Outcome> {
    let changes = matches!(
        verb,
        Verb::ProjectCreate { .. }
            | Verb::ProjectSet { .. }
            | Verb::TaskCreate { .. }
            | Verb::TaskUpdate { .. }
            | Verb::TaskSpawn { .. }
            | Verb::TaskRestart { .. }
            | Verb::TaskTell { .. }
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
        Verb::TaskCreate { project, .. }
        | Verb::TaskSpawn { project, .. }
        | Verb::TaskRestart { project, .. }
        | Verb::TaskTell { project, .. } => {
            in_own(project)?;
            if let Some(own_task) = task_of {
                return refuse(&format!(
                    "this agent works on task {own_task}: only the project's orchestrator makes, \
                     starts and tells tasks. Do the work yourself, and say what else you found \
                     with task_report"
                ));
            }
        }
        Verb::TaskUpdate { project, task, .. } => {
            in_own(project)?;
            if let Some(own_task) = task_of.filter(|own_task| own_task != task) {
                return refuse(&format!(
                    "this agent works on task {own_task}, and changes only that one"
                ));
            }
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

/// What in `args` would loosen the permissions of `agent` run as a thread. Claude Code's and
/// Codex's are judged as their own starts' are. The server knows no other agent's flags (pi's,
/// an ACP agent's), so without the person's leave such an agent takes none, and the first is
/// named.
fn agent_loosening(agent: &AgentId, args: &[String]) -> Option<String> {
    if agent.is(AgentId::CLAUDE_CODE) {
        loosening(args)
    } else if agent.is(AgentId::CODEX) {
        codex::loosening(args)
    } else {
        args.first().cloned()
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

/// Whether `verb` turns pushing a project's target on or off: publishing is the person's.
const fn names_push(verb: &Verb) -> bool {
    match verb {
        Verb::ProjectCreate { push, .. } => *push,
        Verb::ProjectSet { push, .. } => push.is_some(),
        _ => false,
    }
}

/// Whether `verb` sets a project's review limit: how much work may wait on the person is
/// theirs to say.
const fn names_review_limit(verb: &Verb) -> bool {
    match verb {
        Verb::ProjectCreate { limits, .. } | Verb::ProjectSet { limits, .. } => {
            limits.review.is_some()
        }
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
    let why = if names_auto(flag) {
        "in auto mode Claude Code's classifier approves what the person never allowed, so it \
         asks them less than default does"
    } else {
        "only what is known to ask the person no less is let through"
    };
    error(
        ErrorCode::Limit,
        &format!(
            "{flag} may give the agent more than its starter has ({why}); the person allows it \
             for {whose} only in the server's settings.toml (`[server.projects] \
             permission_flags`)"
        ),
    )
}

/// Whether `flag`, as the loosening check words it, asks for auto mode.
fn names_auto(flag: &str) -> bool {
    flag.strip_prefix(PERMISSION_MODE_FLAG)
        .map(|rest| rest.trim_start_matches(['=', ' ']).trim())
        .is_some_and(|mode| mode == "auto")
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

/// Refused when the fleet runs as many agents as the person allows.
pub(super) fn fleet_room(
    state: &State,
    running: &Running<'_>,
    bounds: Bounds,
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
    Ok(())
}

const fn os_word(os: Os) -> &'static str {
    match os {
        Os::MacOs => "macos",
        Os::Linux => "linux",
    }
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
        ("slopty_build", text(&caps.build)),
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

/// The git worktree a writing agent works in.
enum Worktree {
    /// One the worker makes ([`slopty_proto::thread::wire::NewWorktree`]) from the project's
    /// target, so the task's work starts where it is to land: Claude Code's own
    /// `--worktree <name>` opens it, Codex's terminal opens in it, and any other agent's thread
    /// starts in it. Codex's own `--worktree` would start from the clone's `HEAD`, whatever
    /// branch the person left it on.
    Worker {
        /// Its name, reopened by it.
        name: String,
        /// The branch it starts from: the project's target.
        base: String,
    },
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
        "- When the task is done, say so with task_report, with the branch and what you made. \
         Never merge: the server checks your work and the person merges it."
            .to_owned(),
        "- Do the whole task yourself: you start no other agents or tasks. Work you find that is \
         not yours goes in your report, for the orchestrator to plan."
            .to_owned(),
        "- Never type into another agent's terminal, and leave git remotes and git config as \
         they are."
            .to_owned(),
    ];
    if task.read_only {
        lines.push("- Your task only reads: write no files, and report what you found.".to_owned());
    }
    if let Some(Place { path, worktree }) = at {
        lines.push(match worktree {
            Some(Worktree::Worker { name, .. }) => format!(
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
        "- Do sequential and small work yourself. Start a task only for a part that runs in \
         parallel with the rest: task_start makes it and starts its agent in one call, and \
         project_status follows them all. Tasks are one level: their agents start nothing \
         themselves."
            .to_owned(),
        "- Reports come to you in <slopty-reports> blocks like this one, and so does what a \
         task's agent came to when it ended a turn, exited or waits on the person without \
         reporting; task_wait waits for that news where none comes unasked."
            .to_owned(),
        "- task_tell says more to a task's agent, marked as yours: the person's words go \
         first, and nothing you say answers what the person was asked."
            .to_owned(),
        "- Work that passes its checks waits for the person's Merge; merge nothing yourself, \
         and answer no permission: approvals are the person's. While as many tasks wait on the \
         person as the project's review limit, no new task starts: wait for them rather than \
         piling up more."
            .to_owned(),
        "- `slopty --json workers` in your shell shows each worker's facts; name the worker \
         in task_start. Work that needs no Apple platform belongs on Linux."
            .to_owned(),
    ];
    lines.push(
        "- task_start runs Claude Code; `agent: \"codex\"` runs the person's Codex instead, \
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
            " A worker you name that has none gets one cloned first, which the task's step \
             shows."
        } else {
            ""
        };
        lines.push(format!(
            "- Its repository is {key} on every worker; {on}. A task started with no cwd goes \
             beside a clone, in a git worktree of its own when its agent writes.{cloned} A \
             worker's `repos` fact names where its clones are."
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
        self.inner.state.lock().projects.set_policy(policy);
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

    /// The person lets `project` go: its record, its lane's work and the reports waiting in
    /// it. Every client is sent the projects afresh, which a snapshot's first part replaces
    /// whole, as no change says a project is gone.
    pub(super) fn project_delete(&self, project: &ProjectId) -> Outcome {
        let mut guard = self.inner.state.lock();
        let state = &mut *guard;
        let branches = super::steps::server_branches(state, project);
        let worktrees = state.projects.worktrees_of(project);
        if let Err(refused) = state.projects.delete(project) {
            return refused;
        }
        self.clear_away(branches, worktrees);
        keep(state, Keep::Forget(project.clone()));
        let projects = Self::projects_snapshot(state);
        let seq = self.inner.log.lock().next.saturating_sub(1);
        for part in parts(seq, projects) {
            self.announce(FromServer::Projects(Box::new(part)));
        }
        drop(guard);
        tracing::info!(%project, "project let go");
        Outcome::Done
    }

    /// What a project let go leaves on its workers goes: the branches the server named in its
    /// clones, and its tasks' worktrees, each kept by its worker while anything in it is not
    /// committed or a terminal works in it, with the branch of work that did not land.
    fn clear_away(
        &self,
        branches: Vec<((WorkerId, String), Vec<String>)>,
        worktrees: Vec<(WorkerId, String, Vec<String>)>,
    ) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let hub = self.clone();
        tokio::spawn(async move {
            for (worker, worktree, landed) in worktrees {
                let verb = Verb::RemoveWorktree { worker, worktree: worktree.clone(), landed };
                let went = hub.forward(None, verb).await;
                tracing::info!(%worker, %worktree, ?went, "a let-go project's worktree");
            }
            for ((worker, repo), branches) in branches {
                let verb = Verb::DropBranches { worker, repo: repo.clone(), branches };
                let went = hub.forward(None, verb).await;
                tracing::info!(%worker, %repo, ?went, "a let-go project's branches");
            }
        });
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

    /// The person pinned `task` to a worker, or let it run anywhere: its orchestrator hears of
    /// it once it rests, so it starts the task where the person said and moves it nowhere else.
    fn pinned(&self, state: &mut State, project: &ProjectId, task: &Task) {
        let (id, title) = (task.id, &task.title);
        let words = match task.pin {
            Some(worker) => {
                let name = state
                    .workers
                    .get(&worker)
                    .map_or_else(|| worker.to_string(), |entry| entry.info.name.clone());
                format!(
                    "the person pinned task {id} ({title}) to the machine {name}: it runs there \
                     and nowhere else. Start it there when it is due (task_start takes the pin)."
                )
            }
            None => format!(
                "the person let task {id} ({title}) run on any machine: the server places it \
                 where it fits when it starts."
            ),
        };
        let at = tokio::time::Instant::now();
        let node = (project.clone(), None);
        state.deliveries.notice(node, id, crate::deliver::Kind::Done, &words, at);
        self.inner.deliver.notify_one();
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
        if caller == Caller::Agent && names_review_limit(verb) {
            return error(
                ErrorCode::Forbidden,
                "how many tasks may wait on the person is theirs to say, so only the person sets \
                 the review limit",
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
                push,
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
                    push,
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
                push,
                limits,
                metadata,
            } => known_term(state, orchestrator).and_then(|()| {
                let before = state.projects.status(&project, None, &running).ok();
                let change =
                    ProjectChange { members, orchestrator, verifier, push, limits, metadata };
                let set = state.projects.set(&project, change, &running, now)?;
                let was = before.and_then(|b| b.project.orchestrator);
                if orchestrator.is_some() && set.0.project.orchestrator != was {
                    named = Some(project);
                }
                Ok((status(set.0), set.1))
            }),
            Verb::TaskCreate { project, spec } => {
                state.projects.create_task(&project, *spec, now).map(|(t, u)| (task(t), u))
            }
            Verb::TaskTell { project, task: id, text } => {
                // An agent's scope ([`agent_scope`]) proved it the project's orchestrator.
                let by = match caller {
                    Caller::Person => Teller::Person,
                    Caller::Agent => Teller::Orchestrator,
                };
                state.projects.tell(&project, (id, by), &text, &terminals, now).map(|(words, u)| {
                    told = Some((project, id, words, by));
                    (Outcome::Done, u)
                })
            }
            Verb::TaskUpdate { project, task: id, change } => {
                let was = state.projects.task(&project, id).ok().map(|t| t.pin);
                let updated = state.projects.update_task(&project, id, *change, caller, now);
                if let Ok((t, _)) = &updated
                    && caller == Caller::Person
                    && was.is_some_and(|was| was != t.pin)
                {
                    self.pinned(state, &project, t);
                }
                updated.map(|(t, u)| (task(t), u))
            }
            Verb::TaskReport { project, task: id, report } => {
                let reported = state.projects.report_task(&project, id, &report, now);
                if reported.is_ok() {
                    self.bring_home_soon(state, (project.clone(), id), report.branch.clone());
                }
                reported.map(|(t, u)| {
                    let at = tokio::time::Instant::now();
                    state.deliveries.add((project, None), Some(id), report, at);
                    self.inner.deliver.notify_one();
                    (task(t), u)
                })
            }
            Verb::TaskMerge { .. } if caller == Caller::Agent => Err(error(
                ErrorCode::Forbidden,
                "only the person merges: work whose checks pass waits for their Merge, so \
                 report it done",
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
        if let Some((project, task, words, by)) = told {
            let at = tokio::time::Instant::now();
            match by {
                Teller::Person => state.deliveries.person(project, task, &words, at),
                Teller::Orchestrator => state.deliveries.orchestrator((project, task), &words, at),
            }
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
            state.deliveries.instructions((project, None), &role, tokio::time::Instant::now());
            self.inner.deliver.notify_one();
        }
        if let Some(key) = key {
            remember(state, caller, key, verb, &outcome);
        }
        drop(guard);
        outcome
    }

    /// Put the live terminal `term` on `task`, as a start of its own would once announced: for
    /// tests that have an agent at work on a task without starting it through a worker.
    #[cfg(test)]
    pub(super) fn assign_for_test(
        &self,
        project: &ProjectId,
        task: TaskId,
        term: TermRef,
    ) -> Outcome {
        let mut guard = self.inner.state.lock();
        let state = &mut *guard;
        let branch = branch_of(state, term);
        let who = Assignee {
            term,
            spawned: false,
            branch: branch.as_ref(),
            conversation: None,
            thread: None,
        };
        let (mut open, _) = live(state);
        open.insert(term);
        let assigned = match state.projects.assign(project, task, who, &open, WallMs::now()) {
            Ok((task, updates)) => {
                self.projects_moved(state, updates);
                Outcome::Task(Box::new(task))
            }
            Err(refused) => refused,
        };
        drop(guard);
        assigned
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

    /// The thread `of` names, on the worker that holds it ([`ThreadOf::On`]): a task's is its
    /// assignment's thread, or the one seated in its terminal; a terminal's is the one whose
    /// agent runs or is seated there; a thread's is found in the workers' tables.
    pub(super) fn thread_on(&self, of: ThreadOf) -> Result<ThreadOf, Outcome> {
        if let ThreadOf::On { .. } = of {
            return Ok(of);
        }
        let found = thread_found(&self.inner.state.lock(), &of)?;
        let (worker, thread) = found.map_err(|why| error(ErrorCode::Invalid, &why))?;
        Ok(ThreadOf::On { worker, thread })
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

    /// Every worker as a start in `project` sees it: its facts, the fleet's live agents on it,
    /// and whether it has a clone of the project's repository.
    fn candidates(state: &mut State, project: &ProjectId) -> Vec<Candidate> {
        let (terminals, agents) = live(state);
        let running = Running { terminals: &terminals, agents: &agents, starting: &state.starting };
        let record = state.projects.project(project).ok();
        state
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
                    fleet_live,
                    clone: record.is_some_and(|r| clone_on(state, r, worker).is_some()),
                }
            })
            .collect()
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
        let Verb::SpawnAgent { worker, cwd, prompt, args, env, size, worktree, .. } = verb else {
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
            cwd,
            prompt,
            args,
            env,
            size,
            session,
            permission_flags,
            worktree,
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
            if matches!(outcome, Outcome::Opened(_) | Outcome::OpenedIn { .. })
                || maybe_done(&outcome)
            {
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
        fleet_room(state, &running, bounds)?;
        let (id, term) = Self::place(state, worker, None, true);
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
        let Verb::OpenTerminal { worker, cwd, command, mut env, name, size, .. } = verb else {
            return error(ErrorCode::Invalid, "not a terminal's start");
        };
        let allowed = allowance(&self.inner.state.lock(), caller, from, None);
        if let Some(args) = claude_args(&command)
            && let Some(flag) = loosening(&args).filter(|_| !allowed)
        {
            return loosened(&flag, None);
        }
        // An agent's terminal is held to asking: a `claude` typed there is locked to it as one
        // the server starts is, rather than starting in auto and being closed for it.
        env.retain(|(name, _)| name != ASKING_ENV);
        if caller == Caller::Agent && !allowed {
            env.push((ASKING_ENV.to_owned(), "1".to_owned()));
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
        let start = Verb::OpenTerminal {
            worker,
            cwd,
            command,
            env,
            name,
            size,
            session: Some(session),
            worktree: None,
        };
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
        fleet_room(state, &running, bounds)?;
        Ok(Self::place(state, worker, None, true))
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
        });
        (id, term)
    }

    /// Start what runs for a task on the worker it names or the one [`placement::choose`]
    /// finds, and put its terminal on the task. The start goes on if its caller leaves, so what the
    /// worker opens is always counted and assigned.
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
            let room = hub.inner.state.lock().projects.room_to_review(&project);
            // An agent's start waits while the person has as much to look at as they allow.
            let outcome = match room {
                Err(refused) if caller == Caller::Agent => refused,
                _ => hub.start_task_once(key.clone(), &project, task, launch).await,
            };
            if let Some(key) = key {
                remember(&mut hub.inner.state.lock(), caller, key, &verb, &outcome);
            }
            outcome
        });
        started.await.unwrap_or_else(|e| error(ErrorCode::Failed, &format!("the start ended: {e}")))
    }

    /// Start `task`'s work again with a new agent ([`Verb::TaskRestart`]): the one on it now is
    /// closed, and `agent` (else the one it ran last) starts on the worker it ran on, in the
    /// folder it worked in there, told its brief and where the earlier agent's thread is.
    pub(super) async fn task_restart(
        &self,
        caller: Caller,
        key: Option<IdempotencyKey>,
        project: ProjectId,
        task: TaskId,
        agent: Option<AgentId>,
    ) -> Outcome {
        let verb = Verb::TaskRestart { project: project.clone(), task, agent: agent.clone() };
        if let Some(key) = &key
            && let Some(answer) = keyed(&mut self.inner.state.lock(), caller, key, &verb)
        {
            return answer;
        }
        let hub = self.clone();
        let restarted = tokio::spawn(async move {
            let part = key.as_ref().map(|k| k.part("restart"));
            let outcome = hub.restart_once(caller, part, project, task, agent).await;
            if let Some(key) = key {
                remember(&mut hub.inner.state.lock(), caller, key, &verb, &outcome);
            }
            outcome
        });
        restarted
            .await
            .unwrap_or_else(|e| error(ErrorCode::Failed, &format!("the restart ended: {e}")))
    }

    /// [`Self::task_restart`], once its key is checked.
    async fn restart_once(
        &self,
        caller: Caller,
        key: Option<IdempotencyKey>,
        project: ProjectId,
        task: TaskId,
        agent: Option<AgentId>,
    ) -> Outcome {
        let earlier = {
            let mut state = self.inner.state.lock();
            let t = match state.projects.task(&project, task) {
                Ok(t) => t.clone(),
                Err(refused) => return refused,
            };
            if t.state == TaskState::Merged {
                return error(
                    ErrorCode::Invalid,
                    &format!("task {task} is merged; make a new task"),
                );
            }
            let (terminals, _) = live(&mut state);
            let had = t.assignment.as_ref();
            let ran = had.and_then(|a| state.board.ran_at(a.term));
            drop(state);
            let (thread, agent, cwd) = match ran {
                Some((thread, agent, cwd)) => (Some(thread), Some(agent), cwd),
                None => (None, None, None),
            };
            Earlier {
                live: had.filter(|a| a.open() && terminals.contains(&a.term)).map(|a| a.term),
                worker: had.map(|a| a.term.worker),
                thread,
                agent,
                cwd: cwd.or(t.worktree),
                brief: t.brief,
            }
        };
        let Some(agent) = agent.or(earlier.agent) else {
            return error(
                ErrorCode::Invalid,
                &format!("task {task}'s last agent is not known: name the agent to give it to"),
            );
        };
        if let Some(term) = earlier.live {
            if let failed @ Outcome::Error { .. } = self.forward(None, Verb::Close { term }).await {
                return failed;
            }
            // Its worker says so in a moment; the task is free for its next agent now.
            let mut state = self.inner.state.lock();
            let updates = state.projects.session_ended(term, WallMs::now());
            self.projects_moved(&mut state, updates);
            drop(state);
        }
        let launch = TaskLaunch {
            pin: earlier.worker,
            cwd: earlier.cwd.unwrap_or_default(),
            run: runner_for(agent, restart_prompt(&earlier.brief, earlier.thread)),
            env: Vec::new(),
            size: None,
            ignore_dependencies: earlier.worker.is_some(),
        };
        self.task_spawn(caller, key, project, task, launch).await
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
            Runner::Agent { agent, args, .. } => agent_loosening(agent, args),
        };
        flag.map_or(Ok(()), |flag| Err(loosened(&flag, Some(project))))
    }

    /// What a start of `task` asks: the worker the start or the task names, one with a clone
    /// of the project's repository first when it names no directory, and the agent it runs.
    fn placement_for(
        state: &State,
        project: &ProjectId,
        task: TaskId,
        launch: &TaskLaunch,
    ) -> Result<Wanted, Outcome> {
        let pin = launch.pin.or(state.projects.task(project, task)?.pin);
        let repo = state.projects.project(project)?.repo_id.as_ref();
        let clone =
            launch.cwd.trim().is_empty() && repo.is_some_and(|id| id.keys().next().is_some());
        Ok(Wanted { pin, agent: agent_of(&launch.run), clone })
    }

    /// Everything a start checks before it is placed, read together: what it asks of its
    /// worker, and whether the person lets it loosen its permissions.
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
        fleet_room(state, &running, bounds)?;
        let wanted = Self::placement_for(state, project, task, launch)?;
        Ok((wanted, bounds.permission_flags))
    }

    /// Hold a place on `worker` for a start, checked again: others may have started while
    /// a clone was made for it.
    fn reserve(
        state: &mut State,
        (project, task): (&ProjectId, TaskId),
        launch: &TaskLaunch,
        worker: WorkerId,
    ) -> Result<(u64, TermRef), Outcome> {
        Self::may_start(state, project, task, launch)?;
        let agent = !matches!(launch.run, Runner::Command { .. });
        Ok(Self::place(state, worker, Some((project.clone(), task)), agent))
    }

    /// Put the terminal a start opened on its task, and push the change.
    fn assign_started(
        &self,
        state: &mut State,
        (project, task): (&ProjectId, TaskId),
        term: TermRef,
        (conversation, thread, made): (Option<String>, Option<ThreadId>, Option<AgentBranch>),
    ) -> Result<Task, Outcome> {
        let (mut terminals, _) = live(state);
        terminals.insert(term);
        let branch = made.or_else(|| branch_of(state, term));
        let who = Assignee { term, spawned: true, branch: branch.as_ref(), conversation, thread };
        let (task, updates) =
            state.projects.assign(project, task, who, &terminals, WallMs::now())?;
        state.starting.retain(|s| s.term != term);
        self.projects_moved(state, updates);
        Ok(task)
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
        let chosen = {
            let mut state = self.inner.state.lock();
            Self::may_start(&mut state, project, task, &launch).and_then(|(wanted, _)| {
                let candidates = Self::candidates(&mut state, project);
                let unplaced = |why: &str| {
                    error(
                        ErrorCode::Unplaced,
                        &format!("no worker can take task {task} now: {why}"),
                    )
                };
                let worker =
                    placement::choose(&candidates, &wanted).map_err(|why| unplaced(&why))?;
                let beside = candidates.iter().any(|c| c.worker == worker && c.clone);
                let repo = state.projects.project(project)?.repo_id.as_ref();
                // Named or chosen, a worker with no directory given gets a clone or has one:
                // a task's agent started in the worker's home would work on nothing.
                match repo.filter(|id| wanted.clone && !beside && id.url.is_none()) {
                    Some(id) => Err(unplaced(&format!(
                        "with no cwd it goes beside a clone of {}, the worker it would go to has \
                         none, and no address to clone it from is known: clone it on a worker, \
                         pin the task to one that has it, or name a cwd",
                        id.keys().next().unwrap_or_default()
                    ))),
                    None => Ok(worker),
                }
            })
        };
        let worker = match chosen {
            Ok(worker) => worker,
            Err(refused) => return refused,
        };
        // A task with no directory on a worker with no clone of its repository gets one there.
        if let Some(url) = self.clone_needed(project, &launch, worker)
            && let Err(why) = self.clone_for((project, task), worker, url).await
        {
            return error(ErrorCode::Failed, &format!("task {task} needed a clone: {why}"));
        }
        let reserved = {
            let mut state = self.inner.state.lock();
            Self::reserve(&mut state, (project, task), &launch, worker).and_then(|placed| {
                let permission_flags =
                    state.projects.policy().bounds_for(Some(project)).permission_flags;
                let (record, card) =
                    (state.projects.project(project)?, state.projects.task(project, task)?);
                let clone = launch.cwd.trim().is_empty().then(|| clone_on(&state, record, worker));
                let at = clone.flatten().map(|path| {
                    let worktree = match &launch.run {
                        _ if card.read_only => None,
                        Runner::Claude { args, .. } if names(args, &WORKTREE_FLAGS) => None,
                        Runner::Codex { args, .. } if names(args, &[codex::WORKTREE_FLAG]) => None,
                        Runner::Claude { .. } | Runner::Codex { .. } | Runner::Agent { .. } => {
                            Some(Worktree::Worker {
                                name: format!("slopty-{project}-{task}"),
                                base: record.target.clone(),
                            })
                        }
                        Runner::Command { .. } => None,
                    };
                    Place { path, worktree }
                });
                let role = agent_role(record, card, at.as_ref());
                if !permission_flags && matches!(launch.run, Runner::Claude { .. }) {
                    watch(&mut state, placed.1, |w| w.locked = true);
                }
                Ok((placed, permission_flags, role, at))
            })
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
                let worktree = worktree.map(|Worktree::Worker { name, base }| {
                    args.splice(0..0, [WORKTREE_FLAGS[0].to_owned(), name.clone()]);
                    NewWorktree { name, base: Some(base), pull: None, setup: true }
                });
                let (args, conversation) = started_args(args, permission_flags, Some(role));
                let spawn = Verb::SpawnAgent {
                    worker,
                    cwd,
                    prompt,
                    args,
                    env,
                    size,
                    session,
                    permission_flags,
                    worktree,
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
                    worktree: None,
                };
                (open, None)
            }
            // The worker gives it Slopty's tools as it opens (`Worker::as_agent`).
            Runner::Codex { prompt, args } => {
                let open = Verb::OpenTerminal {
                    worker,
                    cwd: Some(cwd).filter(|c| !c.trim().is_empty()),
                    command: codex::command(&role, args, prompt),
                    env,
                    name: Some(format!("{project} #{task}")),
                    size,
                    session,
                    worktree: worktree.map(|Worktree::Worker { name, base }| NewWorktree {
                        name,
                        base: Some(base),
                        pull: None,
                        setup: true,
                    }),
                };
                (open, None)
            }
            // Its adapter gives it Slopty's tools and its role through the agent's own doors.
            Runner::Agent { agent, prompt, model, args } => {
                let worktree = worktree.map(|Worktree::Worker { name, base }| NewWorktree {
                    name,
                    base: Some(base),
                    pull: None,
                    setup: true,
                });
                let start = Start {
                    agent,
                    cwd,
                    drive: None,
                    prompt,
                    model,
                    mode: None,
                    effort: None,
                    attachments: Vec::new(),
                    args,
                    worktree,
                };
                let start = Verb::StartThread {
                    worker,
                    start: Box::new(start),
                    seat: term.session,
                    env,
                    role: Some(role),
                };
                (start, None)
            }
        };
        // A refusal is the worker's answer, and a repeat under the key is the worker's to
        // answer; one whose answer was lost may still open, and is put on its task when its
        // worker announces it.
        let outcome = self.forward(key.map(|k| k.part("start")), start).await;
        let made =
            |worktree: Box<_>| AgentBranch { session: term.session, worktree: Some(*worktree) };
        let (opened, thread, made) = match outcome {
            Outcome::Opened(opened) => (opened, None, None),
            // The worktree the worker made is the task's from the start, so it is freed once the
            // task merges, whatever its agent says of it.
            Outcome::OpenedIn { term: opened, worktree } => (opened, None, Some(made(worktree))),
            Outcome::ThreadStarted { thread, worktree } => (term, Some(thread), worktree.map(made)),
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
            (conversation, thread, made),
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
        let thread = state.board.seated_thread(term);
        match self.assign_started(state, (&project, task), term, (conversation, thread, None)) {
            Ok(_) => tracing::info!(%project, %task, session = %term.session, "a lost start found"),
            Err(refused) => {
                tracing::warn!(%project, %task, ?refused, "a lost start found and not taken");
                state.starting.retain(|s| s.term != term);
            }
        }
    }

    /// Each task's thread on `worker` whose agent is gone from the worker's table ends its
    /// assignment, as a closed terminal does: at once when the table showed it before, and
    /// one it never showed once [`STARTED_GRACE`] has passed since its start, so a table that
    /// has not caught up with a start ends nothing.
    pub(super) fn threads_ended(&self, state: &mut State, worker: WorkerId, now: WallMs) {
        let grace = u64::try_from(STARTED_GRACE.as_millis()).unwrap_or(u64::MAX);
        for (term, thread, since) in state.projects.threads_on(worker) {
            let gone = state.board.thread_there(worker, thread) == Some(false);
            let due = state.board.seen(worker, thread) || now.millis_since(since) >= grace;
            if gone && due {
                tracing::info!(session = %term.session, %thread, "a task's thread ended");
                self.session_closed(state, term);
            }
        }
    }

    /// Push every batch of reports due now to its terminal's worker, and say when the next
    /// falls due.
    pub(super) fn deliver_due(&self) -> Option<tokio::time::Instant> {
        let mut guard = self.inner.state.lock();
        let state = &mut *guard;
        let (terminals, _) = live(state);
        Self::reword_outcomes(state);
        let projects = &state.projects;
        let now = tokio::time::Instant::now();
        let batches = state
            .deliveries
            .take(now, |(project, node)| projects.node_term(project, *node, &terminals));
        for batch in batches {
            let _taken = Self::push_batch(state, &batch, now);
        }
        let next = state.deliveries.next_due();
        drop(guard);
        next
    }

    /// Send `batch` to its terminal's worker, as of `now`. One the link cannot take now stays
    /// outstanding and goes again after [`crate::deliver::RESEND`], folded into the next
    /// batch; one for a worker with no link goes again when it registers. Whether the link took
    /// it, or there was none to try.
    pub(super) fn push_batch(state: &mut State, batch: &Batch, now: tokio::time::Instant) -> bool {
        let Batch { node, term, number, reports, .. } = batch;
        let link = state.workers.get(&term.worker).and_then(|e| e.link.as_ref());
        let msg =
            FromServer::Deliver { session: term.session, batch: *number, reports: reports.clone() };
        if let Some(link) = link
            && link.tx.try_send(msg).is_err()
        {
            tracing::debug!(session = %term.session, batch = number, "reports not sent now");
            state.deliveries.unsent(node, *number, now);
            return false;
        }
        true
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
        let how = if mode == "auto" {
            ", where Claude Code's classifier approves what the person never allowed"
        } else {
            ""
        };
        let why = format!(
            "its agent went into {mode} mode{how}, looser than the person allows \
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

/// What a task restarted on its earlier worker knew before ([`Hub::task_restart`]).
struct Earlier {
    /// Its agent's terminal, or seat, while one runs.
    live: Option<TermRef>,
    /// The worker it ran on last.
    worker: Option<WorkerId>,
    /// The thread its agent ran as last.
    thread: Option<ThreadId>,
    /// That thread's agent.
    agent: Option<AgentId>,
    /// Where it worked: that thread's folder, else the task's worktree. A start there takes
    /// up the work where it is.
    cwd: Option<String>,
    /// Its brief.
    brief: String,
}

/// How `agent` runs for a task, told `prompt` first: Claude Code and Codex in their own
/// terminals, as a task's start names them, any other as a thread.
fn runner_for(agent: AgentId, prompt: Option<String>) -> Runner {
    if agent.is(AgentId::CLAUDE_CODE) {
        Runner::Claude { prompt, args: Vec::new() }
    } else if agent.is(AgentId::CODEX) {
        Runner::Codex { prompt, args: Vec::new() }
    } else {
        Runner::Agent { agent, prompt, model: None, args: Vec::new() }
    }
}

/// A restarted task's first prompt: its brief, then where the earlier agent's work and thread
/// are, for the new agent to read if it helps.
fn restart_prompt(brief: &str, thread: Option<ThreadId>) -> Option<String> {
    let earlier = thread.map(|thread| {
        format!(
            "An agent worked on this task before you: what it changed is in your worktree. Its \
             thread is {thread}; read it with `slopty agent read --thread {thread}` if what it \
             learned helps."
        )
    });
    let brief = Some(brief.trim()).filter(|b| !b.is_empty()).map(str::to_owned);
    match (brief, earlier) {
        (Some(brief), Some(earlier)) => Some(format!("{brief}\n\n{earlier}")),
        (brief, earlier) => brief.or(earlier),
    }
}

/// The agent `run` starts, as a worker's facts name it: a worker must have it installed to be
/// chosen. A command is judged by its program, so `claude …` run as a command still needs
/// Claude Code.
fn agent_of(run: &Runner) -> Option<Installed> {
    match run {
        Runner::Claude { .. } => Some(Installed::program(CLAUDE)),
        Runner::Codex { .. } => Some(Installed::program(codex::PROGRAM)),
        Runner::Agent { agent, .. } => Some(Installed::of(agent)),
        Runner::Command { argv } => {
            let program = argv.first()?;
            match program.rsplit('/').next().unwrap_or(program) {
                CLAUDE => Some(Installed::program(CLAUDE)),
                codex::PROGRAM => Some(Installed::program(codex::PROGRAM)),
                _ => None,
            }
        }
    }
}

/// Claude Code's program, and its name among a worker's `agents` facts.
const CLAUDE: &str = "claude";

/// Where the thread `of` names is, as `state` knows it: its worker and id, or why it is not
/// found. A task that is not there is the store's refusal.
fn thread_found(
    state: &State,
    of: &ThreadOf,
) -> Result<Result<(WorkerId, ThreadId), String>, Outcome> {
    Ok(match of {
        ThreadOf::On { worker, thread } => Ok((*worker, *thread)),
        ThreadOf::Thread(thread) => state
            .board
            .worker_of(*thread)
            .map(|worker| (worker, *thread))
            .ok_or_else(|| format!("no worker's thread table holds thread {thread}")),
        ThreadOf::Term(term) => {
            state.board.thread_at(*term).map(|thread| (term.worker, thread)).ok_or_else(|| {
                format!("no agent's thread runs in terminal {}/{}", term.worker, term.session)
            })
        }
        ThreadOf::Task { project, task } => {
            let card = state.projects.task(project, *task)?;
            match card.assignment.as_ref().filter(|a| a.open()) {
                None => Err(format!("task {task} has no agent at work on it now")),
                Some(a) => a
                    .thread
                    .or_else(|| state.board.thread_at(a.term))
                    .map(|thread| (a.term.worker, thread))
                    .ok_or_else(|| {
                        format!(
                            "task {task}'s agent has no thread its worker reports yet; read its \
                             terminal's screen"
                        )
                    }),
            }
        }
    })
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
            progress: None,
            restored: None,
            program: Vec::new(),
        }
    }

    /// One repository cloned on two workers is found on both by each key it has there; a
    /// worker with none has an empty `repos`.
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

        let Fact::Map(on_linux) = repos_of(&linux, &[]) else { panic!("a map") };
        assert_eq!(on_linux.get(root), Some(&Fact::Text("/home/c/slopty".to_owned())));
        assert_eq!(on_linux.get(origin), None, "known there by its root alone");
        assert_eq!(repos_of(&[], &[]), Fact::Map(BTreeMap::new()));
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
            verifier: None,
            push: false,
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
