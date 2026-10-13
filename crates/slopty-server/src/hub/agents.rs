//! The agents' verbs (`docs/decisions/projects.md`): an agent starts another on any worker,
//! tells it, waits for it, reads it and stops it, and brings a branch from one machine to
//! another. The same verbs serve at any depth: an agent another started starts its own.
//!
//! An agent that starts another is its parent: the child's row names it
//! ([`slopty_proto::agent::PARENT_FACT`]), so every device draws the same tree. What the
//! child comes to (a rest, a wait on the person, its end) reaches the parent through the
//! parent's own hooks ([`crate::deliver`]), as do words another agent tells it. Nothing is
//! typed into a terminal, and no agent answers for the person.
//!
//! How freely a started agent acts is the person's one setting (`[agents] autonomy`), which
//! the server turns into each harness's own mode ([`mode_for`]); an agent names no harness
//! arguments of its own.

use std::collections::{BTreeMap, HashMap, HashSet};

use slopty_core::{SessionId, WallMs, WorkerId, XferId};
use slopty_proto::agent::{AgentEntry, AgentStart, PARENT_FACT, TELL_MAX, WAIT_MAX};
use slopty_proto::orchestration::{
    BUNDLES, BranchBundle, ErrorCode, IdempotencyKey, Outcome, TermRef, ThreadOf, UploadPart, Verb,
};
use slopty_proto::screen::VideoCodec;
use slopty_proto::server::{Fact, Facts, FromServer, Liveness, Os, WorkerFacts};
use slopty_proto::terminal::{RepoId, SessionSummary};
use slopty_proto::thread::wire::{NewWorktree, Start, ThreadRow};
use slopty_proto::thread::{AgentId, Phase, ThreadId, TurnId};
use slopty_settings::Autonomy;

use super::ladder::{parent_of, seat_of, there};
use super::{Entry, Hub, State, WAIT_CAP_MS, error, unknown_worker};
use crate::deliver::{Batch, Kind};

/// The most bytes of a bundle read or uploaded in one request.
const PART: u64 = 4 << 20;
/// The most of an agent's last words its parent reads of a rest, in bytes.
const LAST_WORDS_MAX: usize = 2048;
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
    /// Where each agent another started was last said to stand, by its worker, for its
    /// parent to hear only of a move.
    said: HashMap<ThreadId, (WorkerId, Stands)>,
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

/// Where an agent another started stands, as its parent hears it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stands {
    /// At work, or not yet begun.
    Busy,
    /// Its turn (this one, ended then by its worker's clock) ended and it waits at its prompt:
    /// a later end is news again, whether or not its work was seen in between.
    Rest(Option<(TurnId, WallMs)>),
    /// It waits on the person.
    Needs,
    /// Its program ended.
    Ended,
}

impl Stands {
    fn of(row: &ThreadRow) -> Self {
        if !there(row) {
            return Self::Ended;
        }
        if row.status.phase == Phase::NeedsYou || !row.requests.is_empty() {
            return Self::Needs;
        }
        match row.status.phase {
            // Idle before its first turn ended is a start not yet under way.
            Phase::Idle if row.ended.is_none() => Self::Busy,
            Phase::Idle | Phase::Done | Phase::Failed | Phase::Stopped => {
                Self::Rest(row.ended.as_ref().map(|e| (e.turn, e.at_ms)))
            }
            Phase::Working | Phase::Waiting | Phase::NeedsYou => Self::Busy,
        }
    }
}

/// The mode `agent` starts in under `autonomy`, by the harness's own name: Claude Code's
/// permission mode, Codex's approval policy, pi's gate; an ACP agent's own default.
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
    } else if agent.is(AgentId::PI) {
        match autonomy {
            Autonomy::Auto => slopty_agent::pi::modes::EDITS,
            Autonomy::Full => slopty_agent::pi::modes::OWN,
            Autonomy::Ask => slopty_agent::pi::modes::ASK,
        }
    } else {
        return None;
    };
    Some(mode.to_owned())
}

/// `row`'s agent as the fleet has it.
fn entry_of(worker: WorkerId, row: &ThreadRow, state: &State) -> AgentEntry {
    let branch = row.facts.get("branch").cloned().or_else(|| {
        let seat = seat_of(row)?;
        let held = state.workers.get(&worker)?.branches.get(&seat)?;
        held.worktree.as_ref()?.branch.clone()
    });
    AgentEntry {
        id: row.id,
        parent: parent_of(row),
        worker,
        agent: row.agent.clone(),
        title: row.title.clone(),
        folder: row.cwd.clone(),
        branch,
        phase: row.status.phase,
        wait: row.status.wait.clone(),
        ended: !there(row),
        asks: row.requests.first().map(|r| r.title.clone()).filter(|t| !t.trim().is_empty()),
        last: row.last_line.clone().filter(|l| !l.trim().is_empty()),
        since_ms: row.status.since_ms,
        rests: Stands::of(row) != Stands::Busy,
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

/// The name an agent is read by: its title, else its id.
fn name_of(row: &ThreadRow) -> String {
    let title = row.title.trim();
    if title.is_empty() { row.id.to_string() } else { title.to_owned() }
}

/// `text` cut to at most `max` bytes, at a character boundary, marked when cut.
fn clipped(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}…", text.get(..end).unwrap_or_default())
}

/// What the parent reads of `row`'s agent on `machine` standing at `kind`.
fn words(row: &ThreadRow, machine: &str, kind: Kind) -> String {
    let who = format!("Agent {} ({}) on {machine}", name_of(row), row.id);
    match kind {
        Kind::Rest => {
            let mut words = format!("{who} came to rest at its prompt.");
            if let Some(last) = row.last_line.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
                words.push_str(" Its last words:");
                for line in clipped(last, LAST_WORDS_MAX).lines() {
                    words.push_str("\n  ");
                    words.push_str(line);
                }
            }
            words
        }
        Kind::Needs => {
            let asks = row.requests.first().map_or("a question", |r| r.title.as_str());
            format!(
                "{who} waits on the person: {}. Only the person answers it.",
                clipped(asks.trim(), LAST_WORDS_MAX)
            )
        }
        Kind::Ended => format!("{who} ended; its conversation is kept."),
    }
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

    /// `text` for `agent`, from the agent `from` or from the person. An agent tells only
    /// agents of its own tree.
    pub(super) fn agent_tell(
        &self,
        caller: super::Caller,
        from: Option<ThreadId>,
        agent: ThreadId,
        text: &str,
    ) -> Outcome {
        if text.len() > TELL_MAX {
            return error(ErrorCode::Invalid, &format!("at most {TELL_MAX} bytes to tell at once"));
        }
        let mut state = self.inner.state.lock();
        let Some((_, row)) = state.board.row(agent) else { return unknown_agent(agent) };
        if seat_of(row).is_none() {
            return error(
                ErrorCode::Unsupported,
                &format!("agent {agent} has no seat to hear through; the person reaches it"),
            );
        }
        let by = match (caller, from) {
            (super::Caller::Person, _) => None,
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
                Some((name, from))
            }
        };
        state.deliveries.words(agent, by, text, tokio::time::Instant::now());
        drop(state);
        self.inner.deliver.notify_one();
        Outcome::Done
    }

    /// Wait until one of `agents`, or each when `all`, needs someone or comes to rest, for at
    /// most `timeout_ms` (capped at [`WAIT_CAP_MS`]); `reader`'s own children it saw so are
    /// no news to it after.
    pub(super) async fn agent_wait(
        &self,
        reader: Option<ThreadId>,
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
                let mut state = self.inner.state.lock();
                let mut entries = Vec::with_capacity(agents.len());
                for agent in agents {
                    let Some((worker, row)) = state.board.row(*agent) else {
                        return unknown_agent(*agent);
                    };
                    entries.push(entry_of(worker, row, &state));
                }
                let met = if all {
                    entries.iter().all(|e| e.rests)
                } else {
                    entries.iter().any(|e| e.rests)
                };
                if met && let Some(reader) = reader {
                    for e in entries.iter().filter(|e| e.rests && e.parent == Some(reader)) {
                        state.deliveries.seen(reader, e.id);
                    }
                }
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
                    .filter_map(|a| state.board.row(*a).map(|(w, r)| entry_of(w, r, &state)))
                    .collect();
                return Outcome::Agents { agents, timed_out: true };
            }
        }
    }

    /// End `agent` and every agent under it, the deepest first, their conversations kept. An
    /// agent stops only itself and the agents under it.
    pub(super) async fn agent_stop(
        &self,
        caller: super::Caller,
        from: Option<ThreadId>,
        agent: ThreadId,
    ) -> Outcome {
        let seats: Vec<TermRef> = {
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
            let mut stops: Vec<(usize, TermRef)> = state
                .board
                .agent_rows()
                .filter(|(_, r)| there(r) && under(&state, r.id, agent))
                .filter_map(|(worker, r)| {
                    let depth = depth_below(&state, r.id, agent);
                    Some((depth, TermRef { worker, session: seat_of(r)? }))
                })
                .collect();
            drop(state);
            stops.sort_by_key(|(depth, _)| std::cmp::Reverse(*depth));
            stops.into_iter().map(|(_, term)| term).collect()
        };
        for term in seats {
            match self.forward(None, Verb::Close { term }).await {
                Outcome::Done | Outcome::Error { code: ErrorCode::UnknownTerminal, .. } => {}
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
            .map(|(worker, r)| entry_of(worker, r, &state))
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

    /// Rows came: each agent another started that moved is told to its parent, to go when it
    /// is due ([`crate::deliver`]), and an agent back at work takes back what was to be said.
    pub(super) fn rows_moved(&self, state: &mut State) {
        let now = tokio::time::Instant::now();
        let mut live: HashSet<ThreadId> = HashSet::new();
        let mut moves: Vec<(ThreadId, ThreadId, Stands, String)> = Vec::new();
        for (worker, row) in state.board.agent_rows() {
            let Some(parent) = parent_of(row) else { continue };
            live.insert(row.id);
            let stands = Stands::of(row);
            let was = state.agents.said.get(&row.id).map(|(_, s)| *s);
            if was == Some(stands) {
                continue;
            }
            let machine = state.workers.get(&worker).map_or("?", |e| e.info.name.as_str());
            let kind = match stands {
                Stands::Busy => None,
                Stands::Rest(_) => Some(Kind::Rest),
                Stands::Needs => Some(Kind::Needs),
                Stands::Ended => Some(Kind::Ended),
            };
            let text = kind.map(|k| words(row, machine, k)).unwrap_or_default();
            tracing::debug!(child = %row.id, %parent, ?stands, "an agent another started moved");
            moves.push((parent, row.id, stands, text));
            state.agents.said.insert(row.id, (worker, stands));
        }
        let tabled: HashSet<WorkerId> = state.board.rows().map(|(w, _)| w).collect();
        state.agents.said.retain(|id, (worker, _)| live.contains(id) || !tabled.contains(worker));
        if moves.is_empty() {
            return;
        }
        for (parent, child, stands, text) in moves {
            match stands {
                Stands::Busy => state.deliveries.moved_on(child),
                Stands::Rest(_) => state.deliveries.outcome(parent, child, Kind::Rest, &text, now),
                Stands::Needs => state.deliveries.outcome(parent, child, Kind::Needs, &text, now),
                Stands::Ended => state.deliveries.outcome(parent, child, Kind::Ended, &text, now),
            }
        }
        self.unpark_deliveries(state);
        self.inner.deliver.notify_one();
    }

    /// The machine `worker`, named `name`, went away (it stayed away [`super::GONE_AFTER`]) or
    /// came `back`: each agent there that another started is told to its parent at once.
    pub(super) fn machine_seen(&self, state: &mut State, worker: WorkerId, name: &str, back: bool) {
        let now = tokio::time::Instant::now();
        let children: Vec<(ThreadId, ThreadId, String)> = state
            .board
            .agent_rows()
            .filter(|(w, r)| *w == worker && there(r))
            .filter_map(|(_, r)| Some((parent_of(r)?, r.id, name_of(r))))
            .collect();
        for (parent, child, agent) in children {
            if back {
                // What its rows say from here on is news again.
                state.agents.said.remove(&child);
                state.deliveries.moved_on(child);
                continue;
            }
            let text = format!(
                "The machine {name} stopped answering, with agent {agent} ({child}) on it. It \
                 goes on when the machine is back."
            );
            state.deliveries.outcome(parent, child, Kind::Ended, &text, now);
        }
        self.inner.deliver.notify_one();
    }

    /// Push every batch of words due now to its agent's worker, and say when the next falls
    /// due.
    pub(super) fn deliver_due(&self) -> Option<tokio::time::Instant> {
        let mut guard = self.inner.state.lock();
        let state = &mut *guard;
        let now = tokio::time::Instant::now();
        // The row with a child's last line may come after the status that ended its turn.
        let (board, workers) = (&state.board, &state.workers);
        state.deliveries.reword(|child, kind| {
            let (worker, row) = board.row(child)?;
            let machine = workers.get(&worker).map_or("?", |e| e.info.name.as_str());
            (Stands::of(row) != Stands::Busy).then(|| words(row, machine, kind))
        });
        let board = &state.board;
        let batches = state.deliveries.take(now, |to| {
            let (worker, row) = board.row(to)?;
            let session = seat_of(row).filter(|_| there(row))?;
            Some(TermRef { worker, session })
        });
        for batch in batches {
            tracing::debug!(to = %batch.to, session = %batch.term.session, batch = batch.number, "words due");
            let _taken = Self::push_batch(state, &batch, now);
        }
        let next = state.deliveries.next_due();
        drop(guard);
        next
    }

    /// Send `batch` to its seat's worker, as of `now`. One the link cannot take now stays
    /// outstanding and goes again after [`crate::deliver::RESEND`], folded into the next
    /// batch; one for a worker with no link goes again when it registers. Whether the link took
    /// it, or there was none to try.
    pub(super) fn push_batch(state: &mut State, batch: &Batch, now: tokio::time::Instant) -> bool {
        let Batch { to, term, number, reports } = batch;
        let link = state.workers.get(&term.worker).and_then(|e| e.link.as_ref());
        let msg =
            FromServer::Deliver { session: term.session, batch: *number, reports: reports.clone() };
        if let Some(link) = link
            && link.tx.try_send(msg).is_err()
        {
            tracing::debug!(session = %term.session, batch = number, "words not sent now");
            state.deliveries.unsent(*to, *number, now);
            return false;
        }
        true
    }

    /// The worker holding `term` handed batch `batch` to its agent: what did not fit it may go
    /// now.
    pub(super) fn delivered(&self, state: &mut State, term: TermRef, batch: u64) {
        if state.deliveries.acked(term, batch).is_some() {
            self.inner.deliver.notify_one();
        }
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
