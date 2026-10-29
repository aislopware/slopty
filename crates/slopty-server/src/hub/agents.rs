//! The agents' verbs (`docs/decisions/projects.md`): an agent starts another on any worker,
//! tells it, waits for it, reads it and stops it, and brings a branch from one machine to
//! another. The same verbs serve at any depth: an agent another started starts its own.
//!
//! An agent that starts another is its parent: the child's row names it
//! ([`slopty_proto::agent::PARENT_FACT`]), so every device draws the same tree. The parent reads
//! and waits on what its children come to (`agent_read`, `agent_wait`). Words told an agent are
//! typed into its TUI as one message by the worker that holds it, never while it asks the person
//! at a dialog, and a stop is an Esc while its turn runs (`docs/decisions/agents.md`,
//! "TUI-first"). No agent answers for the person.
//!
//! How freely a started agent acts is the person's one setting (`[agents] autonomy`), which
//! the server turns into each harness's own mode ([`mode_for`]); an agent names no harness
//! arguments of its own.

use std::collections::{BTreeMap, HashSet};

use slopty_core::{SessionId, WorkerId, XferId};
use slopty_proto::agent::{AgentEntry, AgentStart, PARENT_FACT, TELL_MAX, WAIT_MAX};
use slopty_proto::orchestration::{
    BUNDLES, BranchBundle, ErrorCode, IdempotencyKey, Outcome, ThreadOf, UploadPart, Verb,
};
use slopty_proto::screen::VideoCodec;
use slopty_proto::server::{Fact, Facts, Liveness, Os, WorkerFacts};
use slopty_proto::terminal::{RepoId, SessionSummary};
use slopty_proto::thread::wire::{NewWorktree, Start, ThreadRow};
use slopty_proto::thread::{AgentId, Phase, ThreadId};
use slopty_settings::Autonomy;

use super::ladder::{parent_of, there};
use super::{Entry, Hub, State, WAIT_CAP_MS, error, unknown_worker};

/// The most bytes of a bundle read or uploaded in one request.
const PART: u64 = 4 << 20;
/// The fact a worker says its clones are in, and the server the repositories each worker
/// has: each key of a repository's identity ([`RepoId::keys`]) to the clone's path.
const REPOS: &str = "repos";
/// Claude Code's program, and its name among a worker's `agents` facts.
const CLAUDE: &str = "claude";
/// The most items of a fact's list or map the server keeps.
const FACT_ITEMS_MAX: usize = 1024;
/// The most facts, nested ones counted, the server keeps of a worker.
const FACTS_MAX: usize = 4096;
/// The longest text fact the server keeps, in bytes.
const FACT_TEXT_MAX: usize = 4096;
/// The most bytes of facts the server keeps of a worker.
const FACTS_BYTES_MAX: usize = 64 * 1024;

/// What the agents' verbs keep between calls.
#[derive(Debug, Default)]
pub(super) struct Agents {
    /// How freely a started agent acts: the person's setting.
    autonomy: Autonomy,
    /// Terminals an agent opened or typed into: the CLI there speaks for an agent.
    driven: HashSet<SessionId>,
    /// The last clone number asked for.
    next_clone: u64,
}

impl Agents {
    /// Whether an agent drove the terminal `session`: it opened it or typed into it.
    pub(super) fn driven(&self, session: SessionId) -> bool {
        self.driven.contains(&session)
    }

    /// An agent opened or typed into the terminal `session`.
    pub(super) fn drove(&mut self, session: SessionId) {
        self.driven.insert(session);
    }

    /// The terminal `session` closed.
    pub(super) fn closed(&mut self, session: SessionId) {
        self.driven.remove(&session);
    }
}

/// The mode `agent` starts in under `autonomy`, by the harness's own name: Claude Code's
/// permission mode, Codex's approval policy; any other agent starts in its own default.
fn mode_for(agent: &AgentId, autonomy: Autonomy) -> Option<String> {
    let mode = if agent.is(AgentId::CLAUDE_CODE) {
        match autonomy {
            Autonomy::Auto => "auto",
            Autonomy::Full => "bypassPermissions",
            Autonomy::Ask => return None,
        }
    } else if agent.is(AgentId::CODEX) {
        match autonomy {
            Autonomy::Auto => "on-request",
            Autonomy::Full => "never",
            Autonomy::Ask => "untrusted",
        }
    } else {
        return None;
    };
    Some(mode.to_owned())
}

/// `row`'s agent as the fleet has it.
fn entry_of(worker: WorkerId, row: &ThreadRow) -> AgentEntry {
    AgentEntry {
        id: row.id,
        parent: parent_of(row),
        worker,
        agent: row.agent.clone(),
        title: row.title.clone(),
        folder: row.cwd.clone(),
        branch: row.facts.get("branch").cloned(),
        phase: row.status.phase,
        wait: row.status.wait.clone(),
        ended: !there(row),
        asks: row.requests.first().map(|r| r.title.clone()).filter(|t| !t.trim().is_empty()),
        last: row.last_line.clone().filter(|l| !l.trim().is_empty()),
        since_ms: row.status.since_ms,
        rests: rests(row),
    }
}

/// Whether `row`'s agent has come to something its parent waits for: its program ended, it
/// waits on the person, or its turn ended and it waits at its prompt. Idle before its first
/// turn ended is a start not yet under way.
fn rests(row: &ThreadRow) -> bool {
    if !there(row) || row.status.phase == Phase::NeedsYou || !row.requests.is_empty() {
        return true;
    }
    match row.status.phase {
        Phase::Idle => row.ended.is_some(),
        Phase::Done | Phase::Failed | Phase::Stopped => true,
        Phase::Working | Phase::Waiting | Phase::NeedsYou => false,
    }
}

/// The agent at the top of `thread`'s tree, through every parent the rows name.
fn top_of(state: &State, thread: ThreadId) -> ThreadId {
    let mut at = thread;
    let mut seen = HashSet::new();
    while seen.insert(at) {
        match state.board.row(at).and_then(|(_, r)| parent_of(r)) {
            Some(parent) => at = parent,
            None => break,
        }
    }
    at
}

/// Whether `thread` is `above` or hangs from it at any depth.
fn under(state: &State, thread: ThreadId, above: ThreadId) -> bool {
    let mut at = thread;
    let mut seen = HashSet::new();
    while seen.insert(at) {
        if at == above {
            return true;
        }
        match state.board.row(at).and_then(|(_, r)| parent_of(r)) {
            Some(parent) => at = parent,
            None => return false,
        }
    }
    false
}

/// What an agent named `name`, thread `from`, telling another `text` has typed into the other's
/// terminal: marked as an agent's words, so the agent hearing them never takes them for the
/// person's.
fn told(name: &str, from: ThreadId, text: &str) -> String {
    format!("Agent {name} ({from}) says (an agent, not the person):\n{}", text.trim())
}

/// Whether `row`'s agent is in a turn an Esc stops: at work, asking nobody.
fn at_work(row: &ThreadRow) -> bool {
    row.status.phase == Phase::Working && row.status.wait.is_none() && row.requests.is_empty()
}

/// The name an agent is read by: its title, else its id.
fn name_of(row: &ThreadRow) -> String {
    let title = row.title.trim();
    if title.is_empty() { row.id.to_string() } else { title.to_owned() }
}

/// A worker's facts as the server keeps them: lists and maps cut to [`FACT_ITEMS_MAX`] items,
/// texts to [`FACT_TEXT_MAX`] bytes, and [`FACTS_MAX`] facts and [`FACTS_BYTES_MAX`] bytes in
/// all, so every worker's facts fit a frame. What is past a bound is dropped, in name order.
pub(super) fn bounded(facts: Facts) -> Facts {
    let mut budget = Budget { facts: FACTS_MAX, bytes: FACTS_BYTES_MAX };
    facts
        .into_iter()
        .map_while(|(name, fact)| {
            budget.take_bytes(name.len())?;
            Some((name, cut(fact, &mut budget)?))
        })
        .collect()
}

/// What is left of a worker's bounds on its facts.
struct Budget {
    facts: usize,
    bytes: usize,
}

impl Budget {
    fn take_bytes(&mut self, n: usize) -> Option<()> {
        self.bytes = self.bytes.checked_sub(n.saturating_add(8))?;
        Some(())
    }
}

fn cut(fact: Fact, budget: &mut Budget) -> Option<Fact> {
    budget.facts = budget.facts.checked_sub(1)?;
    Some(match fact {
        Fact::Text(t) => {
            let mut end = t.len().min(FACT_TEXT_MAX);
            while !t.is_char_boundary(end) {
                end = end.saturating_sub(1);
            }
            budget.take_bytes(end)?;
            Fact::Text(t.get(..end).unwrap_or_default().to_owned())
        }
        Fact::List(items) => Fact::List(
            items.into_iter().take(FACT_ITEMS_MAX).map_while(|f| cut(f, budget)).collect(),
        ),
        Fact::Map(parts) => Fact::Map(
            parts
                .into_iter()
                .take(FACT_ITEMS_MAX)
                .map_while(|(k, f)| {
                    budget.take_bytes(k.len())?;
                    Some((k, cut(f, budget)?))
                })
                .collect(),
        ),
        other => {
            budget.take_bytes(0)?;
            other
        }
    })
}

const fn os_word(os: Os) -> &'static str {
    match os {
        Os::MacOs => "macos",
        Os::Linux => "linux",
    }
}

/// A worker's facts: its own, with what the server knows of it over them.
fn facts_of(entry: &Entry, agents_here: usize) -> Facts {
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
        ("live_agents", int(u64::try_from(agents_here).unwrap_or(u64::MAX))),
        (REPOS, repos_of(&entry.sessions, entry.facts.get(REPOS))),
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

/// The repositories a worker has a shell in or the server had cloned there (`reported`), by
/// each key of their identity ([`RepoId`]: the normalized origin, the first commit), to where
/// the clone is. One repository cloned on two workers has the same keys on both; with several
/// clones on one worker the first path in order is named.
fn repos_of(sessions: &[SessionSummary], reported: Option<&Fact>) -> Fact {
    let shells = sessions.iter().filter_map(|s| Some((s.repo.as_deref()?, s.repo_id.as_ref()?)));
    let mut found: Vec<(&str, &str)> =
        shells.flat_map(|(path, id)| id.keys().map(move |key| (key, path))).collect();
    if let Some(Fact::Map(cloned)) = reported {
        found.extend(cloned.iter().filter_map(|(key, path)| match path {
            Fact::Text(path) => Some((key.as_str(), path.as_str())),
            _ => None,
        }));
    }
    let mut repos: BTreeMap<String, &str> = BTreeMap::new();
    for (key, path) in found {
        let at = repos.entry(key.to_owned()).or_insert(path);
        if path < *at {
            *at = path;
        }
    }
    Fact::Map(repos.into_iter().map(|(key, path)| (key, Fact::Text(path.to_owned()))).collect())
}

/// `worker` has a clone of `id` at `path`, which the server had made there: its `repos` fact
/// holds it at once, before the worker's own facts say so again.
fn cloned(state: &mut State, worker: WorkerId, path: &str, id: &RepoId) {
    let Some(entry) = state.workers.get_mut(&worker) else { return };
    let repos = entry.facts.entry(REPOS.to_owned()).or_insert_with(|| Fact::Map(Facts::new()));
    if let Fact::Map(repos) = repos {
        for key in id.keys() {
            repos.insert(key.to_owned(), Fact::Text(path.to_owned()));
        }
    }
}

/// The repository whose clone or worktree is at `path` on `worker`, as a terminal or a thread
/// there says.
fn repo_at(state: &State, worker: WorkerId, path: &str) -> Option<RepoId> {
    let entry = state.workers.get(&worker)?;
    let shell = entry
        .sessions
        .iter()
        .find(|s| s.repo.as_deref() == Some(path))
        .and_then(|s| s.repo_id.clone());
    shell.or_else(|| {
        state
            .board
            .rows()
            .filter(|(w, _)| *w == worker)
            .find(|(_, r)| r.repo.as_deref() == Some(path) || r.cwd.as_deref() == Some(path))
            .and_then(|(_, r)| r.repo_id.clone())
    })
}

/// Where `worker` has a clone of `id`, by its `repos` fact.
fn clone_of(state: &State, worker: WorkerId, id: &RepoId) -> Option<String> {
    let entry = state.workers.get(&worker)?;
    let Fact::Map(repos) = repos_of(&entry.sessions, entry.facts.get(REPOS)) else { return None };
    id.keys().find_map(|k| match repos.get(k) {
        Some(Fact::Text(path)) => Some(path.clone()),
        _ => None,
    })
}

/// What a forwarded verb that failed said.
fn said(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Error { message, .. } => message.clone(),
        other => format!("an unexpected answer: {other:?}"),
    }
}

fn unknown_agent(agent: ThreadId) -> Outcome {
    error(ErrorCode::UnknownAgent, &format!("no agent {agent}; `agents` lists them"))
}

impl Hub {
    /// Start agents under `autonomy` from now on: the person's `[agents] autonomy`.
    pub fn set_autonomy(&self, autonomy: Autonomy) {
        self.inner.state.lock().agents.autonomy = autonomy;
    }

    /// The thread `of` names, on the worker that holds it ([`ThreadOf::On`]): a terminal's is
    /// the one whose agent runs or is seated there; a thread's is found in the workers' tables.
    pub(super) fn thread_on(&self, of: ThreadOf) -> Result<ThreadOf, Outcome> {
        let state = self.inner.state.lock();
        let found = match of {
            ThreadOf::On { .. } => return Ok(of),
            ThreadOf::Thread(thread) => state
                .board
                .worker_of(thread)
                .map(|worker| (worker, thread))
                .ok_or_else(|| unknown_agent(thread)),
            ThreadOf::Term(term) => {
                state.board.thread_at(term).map(|thread| (term.worker, thread)).ok_or_else(|| {
                    let why = format!(
                        "no agent's thread runs in terminal {}/{}",
                        term.worker, term.session
                    );
                    error(ErrorCode::Invalid, &why)
                })
            }
        };
        drop(state);
        let (worker, thread) = found?;
        Ok(ThreadOf::On { worker, thread })
    }

    /// Every worker's facts, or one's, each online worker asked for its own first when it has
    /// not been on this registration ([`Self::ask_facts`]).
    pub(super) async fn worker_facts(&self, only: Option<WorkerId>) -> Outcome {
        if let Some(worker) = only
            && !self.inner.state.lock().workers.contains_key(&worker)
        {
            return unknown_worker(worker);
        }
        self.ask_facts(only).await;
        let state = self.inner.state.lock();
        let mut out: Vec<(String, WorkerFacts)> = state
            .workers
            .values()
            .filter(|e| only.is_none_or(|w| w == e.info.worker))
            .map(|e| {
                let worker = e.info.worker;
                let here = state.board.agent_rows().filter(|(w, r)| *w == worker && there(r));
                let facts = facts_of(e, here.count());
                (e.info.name.clone(), WorkerFacts { worker, facts })
            })
            .collect();
        drop(state);
        out.sort_by(|(a, x), (b, y)| a.cmp(b).then(x.worker.cmp(&y.worker)));
        Outcome::Facts(out.into_iter().map(|(_, f)| f).collect())
    }

    /// Ask each online worker (`only`, else all) not yet asked on its registration for its own
    /// facts, all at once, and keep what each answers.
    ///
    /// A worker gathers them only once asked, and from then on keeps them current itself
    /// ([`slopty_proto::server::ToServer::Facts`]). So a fleet nobody reads the facts of
    /// probes nothing, and the first read waits for the gathering, up to the worker's own
    /// bound.
    async fn ask_facts(&self, only: Option<WorkerId>) {
        let asked: Vec<(WorkerId, u64)> = {
            let mut state = self.inner.state.lock();
            state
                .workers
                .values_mut()
                .filter(|e| only.is_none_or(|w| w == e.info.worker))
                .filter(|e| e.link.is_some() && !e.facts_asked)
                .map(|e| {
                    e.facts_asked = true;
                    (e.info.worker, e.generation)
                })
                .collect()
        };
        let mut answers = tokio::task::JoinSet::new();
        for (worker, generation) in asked {
            let hub = self.clone();
            answers.spawn(async move {
                let verb = Verb::WorkerFacts { worker: Some(worker) };
                (worker, generation, hub.forward(None, verb).await)
            });
        }
        while let Some(answered) = answers.join_next().await {
            let Ok((worker, generation, outcome)) = answered else { continue };
            let Outcome::Facts(said) = outcome else {
                tracing::info!(%worker, ?outcome, "a worker did not say its facts");
                continue;
            };
            let own = said.into_iter().find(|f| f.worker == worker);
            let mut state = self.inner.state.lock();
            if let (Some(own), Some(entry)) = (own, state.workers.get_mut(&worker))
                && entry.generation == generation
            {
                entry.facts = bounded(own.facts);
            }
        }
    }

    /// Start an agent for `parent` (none for the person), as `start` asks: in its folder, or
    /// the machine's clone of its repository, made first when there is none; in a fresh
    /// worktree when it names one. The harness's mode follows the person's autonomy setting.
    /// A repeat under `key` answers the agent the first started.
    pub(super) async fn agent_start(
        &self,
        parent: Option<ThreadId>,
        key: Option<IdempotencyKey>,
        start: AgentStart,
    ) -> Outcome {
        let AgentStart { worker, folder, prompt, agent, model, effort, worktree, repo } = start;
        let (autonomy, parent_said) = {
            let state = self.inner.state.lock();
            if !state.workers.contains_key(&worker) {
                return unknown_worker(worker);
            }
            let said = parent.and_then(|p| {
                let (on, row) = state.board.row(p)?;
                let machine = state.workers.get(&on).map(|e| e.info.name.clone())?;
                Some(format!(
                    "You were started by agent {} ({p}) on {machine}; it hears when you come to \
                     rest or need the person.",
                    name_of(row)
                ))
            });
            (state.agents.autonomy, said)
        };
        let cwd = match (folder, repo) {
            (Some(folder), _) => folder,
            (None, Some(url)) => match self.clone_on(worker, url).await {
                Ok(path) => path,
                Err(refused) => return refused,
            },
            (None, None) => {
                return error(
                    ErrorCode::Invalid,
                    "name the folder it works in, or a repo to clone",
                );
            }
        };
        let agent = agent.unwrap_or_else(|| AgentId::named(AgentId::CLAUDE_CODE));
        let mode = mode_for(&agent, autonomy);
        let start = Start {
            agent,
            cwd,
            drive: None,
            prompt: Some(prompt),
            model,
            mode,
            effort,
            attachments: Vec::new(),
            args: Vec::new(),
            options: BTreeMap::new(),
            worktree: worktree.map(NewWorktree::named),
        };
        // A repeat under the same key takes the same seat, which the worker answers with the
        // thread it started there.
        let seat = key.as_ref().map_or_else(SessionId::new, |key| {
            let by = parent.map(|p| p.to_string()).unwrap_or_default();
            SessionId::from_uuid(*ThreadId::derived(&["agent_start", &by, key.as_str()]).as_uuid())
        });
        let facts = parent.map(|p| (PARENT_FACT.to_owned(), p.to_string())).into_iter().collect();
        let verb =
            Verb::StartThread { worker, start: Box::new(start), seat, facts, system: parent_said };
        self.forward(key, verb).await
    }

    /// A clone of `url` on `worker`: the one the worker has, or one it makes.
    async fn clone_on(&self, worker: WorkerId, url: String) -> Result<String, Outcome> {
        let clone = {
            let mut state = self.inner.state.lock();
            state.agents.next_clone = state.agents.next_clone.wrapping_add(1);
            state.agents.next_clone
        };
        match self.forward(None, Verb::CloneRepo { worker, url, clone }).await {
            Outcome::Cloned { path, repo } => {
                cloned(&mut self.inner.state.lock(), worker, &path, &repo);
                Ok(path)
            }
            refused @ Outcome::Error { .. } => Err(refused),
            other => Err(error(ErrorCode::Failed, &said(&other))),
        }
    }

    /// `text` for `agent`, from the agent `from` or from the person, typed into its terminal
    /// by the worker that holds it as one message. An agent tells only agents of its own tree,
    /// and its words go marked as an agent's, not the person's.
    pub(super) async fn agent_tell(
        &self,
        caller: super::Caller,
        from: Option<ThreadId>,
        agent: ThreadId,
        text: &str,
    ) -> Outcome {
        if text.len() > TELL_MAX {
            return error(ErrorCode::Invalid, &format!("at most {TELL_MAX} bytes to tell at once"));
        }
        let (worker, said) = {
            let state = self.inner.state.lock();
            let Some((worker, _)) = state.board.row(agent) else { return unknown_agent(agent) };
            let said = match (caller, from) {
                (super::Caller::Person, _) => text.to_owned(),
                (super::Caller::Agent, None) => {
                    return error(
                        ErrorCode::Forbidden,
                        "the server cannot tell which agent is speaking, so it tells nobody",
                    );
                }
                (super::Caller::Agent, Some(from)) => {
                    if top_of(&state, from) != top_of(&state, agent) {
                        return error(
                            ErrorCode::Forbidden,
                            &format!("agent {agent} is not of the tree this agent is in"),
                        );
                    }
                    let name =
                        state.board.row(from).map_or_else(|| from.to_string(), |(_, r)| name_of(r));
                    told(&name, from, text)
                }
            };
            drop(state);
            (worker, said)
        };
        let tell = Verb::AgentTell { agent, text: said };
        self.forward_to(worker, None, tell, super::FORWARD_TIMEOUT).await
    }

    /// Wait until one of `agents`, or each when `all`, needs someone or comes to rest, for at
    /// most `timeout_ms` (capped at [`WAIT_CAP_MS`]).
    pub(super) async fn agent_wait(
        &self,
        agents: &[ThreadId],
        all: bool,
        timeout_ms: u32,
    ) -> Outcome {
        if agents.is_empty() || agents.len() > WAIT_MAX {
            return error(ErrorCode::Invalid, &format!("wait for 1 to {WAIT_MAX} agents"));
        }
        let wait = std::time::Duration::from_millis(u64::from(timeout_ms.min(WAIT_CAP_MS)));
        let deadline = tokio::time::Instant::now().checked_add(wait);
        let mut rows = self.inner.state.lock().board.rows_changed();
        loop {
            rows.borrow_and_update();
            let entries = {
                let state = self.inner.state.lock();
                let mut entries = Vec::with_capacity(agents.len());
                for agent in agents {
                    let Some((worker, row)) = state.board.row(*agent) else {
                        return unknown_agent(*agent);
                    };
                    entries.push(entry_of(worker, row));
                }
                let met = if all {
                    entries.iter().all(|e| e.rests)
                } else {
                    entries.iter().any(|e| e.rests)
                };
                drop(state);
                met.then_some(entries)
            };
            if let Some(agents) = entries {
                return Outcome::Agents { agents, timed_out: false };
            }
            let woke = match deadline {
                Some(at) => tokio::time::timeout_at(at, rows.changed()).await,
                None => Ok(rows.changed().await),
            };
            if !matches!(woke, Ok(Ok(()))) {
                let state = self.inner.state.lock();
                let agents = agents
                    .iter()
                    .filter_map(|a| state.board.row(*a).map(|(w, r)| entry_of(w, r)))
                    .collect();
                return Outcome::Agents { agents, timed_out: true };
            }
        }
    }

    /// Stop the turn of `agent` and of every agent under it at work, the deepest first: an Esc
    /// in each one's terminal, its session and terminal kept. One that waits on the person is
    /// left to the person, and with none at work there is nothing to stop. An agent stops only
    /// itself and the agents under it.
    pub(super) async fn agent_stop(
        &self,
        caller: super::Caller,
        from: Option<ThreadId>,
        agent: ThreadId,
    ) -> Outcome {
        let stops: Vec<(WorkerId, ThreadId)> = {
            let state = self.inner.state.lock();
            if state.board.row(agent).is_none() {
                return unknown_agent(agent);
            }
            if caller == super::Caller::Agent && from.is_none_or(|from| !under(&state, agent, from))
            {
                return error(
                    ErrorCode::Forbidden,
                    &format!("agent {agent} is not this agent or one it started"),
                );
            }
            let mut stops: Vec<(usize, WorkerId, ThreadId)> = state
                .board
                .agent_rows()
                .chain(state.board.row(agent))
                .filter(|(_, r)| there(r) && at_work(r) && under(&state, r.id, agent))
                .map(|(worker, r)| (depth_below(&state, r.id, agent), worker, r.id))
                .collect();
            drop(state);
            stops.sort_by_key(|(depth, _, id)| (std::cmp::Reverse(*depth), *id));
            stops.dedup_by_key(|(_, _, id)| *id);
            stops.into_iter().map(|(_, worker, id)| (worker, id)).collect()
        };
        if stops.is_empty() {
            return error(
                ErrorCode::Conflict,
                &format!("agent {agent} and the agents under it are not at work: nothing to stop"),
            );
        }
        for (worker, thread) in stops {
            let stop = Verb::AgentStop { agent: thread };
            match self.forward_to(worker, None, stop, super::FORWARD_TIMEOUT).await {
                Outcome::Done | Outcome::Error { code: ErrorCode::Conflict, .. } => {}
                failed => return failed,
            }
        }
        Outcome::Done
    }

    /// Every agent on the fleet: each live one, and each another started, with its parent.
    pub(super) fn agents(&self) -> Outcome {
        let state = self.inner.state.lock();
        let mut agents: Vec<AgentEntry> = state
            .board
            .agent_rows()
            .filter(|(_, r)| there(r) || parent_of(r).is_some())
            .map(|(worker, r)| entry_of(worker, r))
            .collect();
        drop(state);
        agents.sort_by_key(|a| (a.parent, a.since_ms, a.id));
        Outcome::Agents { agents, timed_out: false }
    }

    /// Bring `branch` from the clone `repo` on `from` into the clone `into` on `to` (the clone
    /// of the same repository there when absent), as `name` (its own when absent): a git
    /// bundle made there, read in parts through the server, and fetched here.
    pub(super) async fn branch_bring(
        &self,
        (from, repo, branch): (WorkerId, String, String),
        (to, into, name): (WorkerId, Option<String>, Option<String>),
    ) -> Outcome {
        let into = into.or_else(|| {
            let state = self.inner.state.lock();
            let id = repo_at(&state, from, &repo);
            let found = id.as_ref().and_then(|id| clone_of(&state, to, id));
            drop(state);
            found
        });
        let Some(into) = into else {
            return error(
                ErrorCode::Invalid,
                "no clone of that repository is known on the machine it goes to; name the \
                 clone it lands in",
            );
        };
        let bundle =
            Verb::BundleBranch { worker: from, repo, branch: branch.clone(), target: None };
        let BranchBundle { path, name: bundle, size, digest, head, .. } =
            match self.forward(None, bundle).await {
                Outcome::Bundle(made) => *made,
                other => return other,
            };
        let upload = XferId::new();
        let at = format!("{BUNDLES}/{bundle}");
        let mut offset = 0;
        while offset < size {
            let read = Verb::ReadFile {
                worker: from,
                path: path.clone(),
                offset,
                length: Some(PART.min(size.saturating_sub(offset))),
            };
            let bytes = match self.forward(None, read).await {
                Outcome::File { bytes, .. } if !bytes.is_empty() => bytes,
                other => return error(ErrorCode::Failed, &said(&other)),
            };
            let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            let part = UploadPart::Bytes { offset, bytes };
            let sent = Verb::Upload { worker: to, path: at.clone(), upload, part };
            if let failed @ Outcome::Error { .. } = self.forward(None, sent).await {
                return failed;
            }
            offset = offset.saturating_add(len);
        }
        let finish = UploadPart::Finish { size, digest, mode: None };
        let finished = Verb::Upload { worker: to, path: at, upload, part: finish };
        if let failed @ Outcome::Error { .. } = self.forward(None, finished).await {
            return failed;
        }
        let into_branch = name.unwrap_or_else(|| branch.clone());
        let fetch =
            Verb::FetchBundle { worker: to, repo: into, bundle, branch, into: into_branch, head };
        self.forward(None, fetch).await
    }
}

/// How many parents up from `thread` `above` is.
fn depth_below(state: &State, thread: ThreadId, above: ThreadId) -> usize {
    let mut at = thread;
    let mut depth = 0_usize;
    let mut seen = HashSet::new();
    while at != above && seen.insert(at) {
        match state.board.row(at).and_then(|(_, r)| parent_of(r)) {
            Some(parent) => at = parent,
            None => break,
        }
        depth = depth.saturating_add(1);
    }
    depth
}
