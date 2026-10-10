//! Session table + ptyd connection.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use slopty_core::{SessionId, WallMs};
use slopty_proto::ptyd::PtydError;
use slopty_proto::terminal::{OpenSession, Restored, SessionState, SessionSummary, TermSize};
use slopty_pty::protocol::{SessionInfo, socket_path};
use slopty_pty::{PtyError, PtydClient, SpawnSpec};
use tokio::sync::{Notify, mpsc};

use crate::WorkerError;
use crate::orchestrate::Agents;
use crate::restore::{AgentLaunch, Keeper, Recipe};
use crate::session::{self, Probe, SessionHandle, SessionStart, Tap};

/// Scrollback lines the engine retains per session.
pub const SCROLLBACK_LINES: u32 = 50_000;

/// How long a session whose program exited is kept while no client watches it.
///
/// An exited shell stays with its last screen until a human closes its tile
/// (`docs/decisions/terminal.md`, "An exited shell stays until it is closed"); this bounds the
/// ones nobody comes back to. A day outlasts a night and a working day away from the machine.
pub const EXITED_UNWATCHED: Duration = Duration::from_hours(24);

/// The variable naming a session's presence file: while it exists, Claude Code sends no
/// Remote Control push (<https://code.claude.com/docs/en/env-vars>).
pub const PRESENCE_ENV: &str = "CLAUDE_CLIENT_PRESENCE_FILE";

/// How long a starting worker waits for an older one to let go of a session: that worker is
/// exiting, and ptyd takes the session back the moment its connection drops.
const HANDOVER_WAIT: Duration = Duration::from_secs(30);

/// How often a session an older worker holds is asked for again.
const HANDOVER_POLL: Duration = Duration::from_millis(50);

/// The first wait before ptyd is dialled again once its link is lost, doubled on each refusal
/// up to [`REDIAL_MAX`]: a ptyd handing itself to a new build is back within milliseconds, one
/// its manager restarts within a second.
const REDIAL_FIRST: Duration = Duration::from_millis(20);

/// The longest wait between two dials of a lost ptyd.
const REDIAL_MAX: Duration = Duration::from_secs(2);

/// How long a ptyd that answers has to say its custody under its own pid: it writes the file
/// just after it binds, so a dial can come between the two.
const CUSTODY_WAIT: Duration = Duration::from_secs(2);

/// How often its custody file is read meanwhile.
const CUSTODY_LOOK: Duration = Duration::from_millis(20);

struct Entry {
    handle: SessionHandle,
    command: Vec<String>,
    exited: Option<i32>,
    /// When ptyd spawned the child.
    started_ms: WallMs,
    /// The child ptyd spawned, which a ptyd that started afresh is told of.
    pid: u32,
    /// The terminfo name the child was given as `TERM`.
    term: String,
}

/// What a session is adopted with besides what ptyd hands over.
#[derive(Clone, Debug, Default)]
struct Adoption {
    /// The child ptyd spawned.
    pid: u32,
    /// What it was opened to run.
    command: Vec<String>,
    /// Its child's exit status, when ptyd reaped it before this worker adopted it.
    exited: Option<i32>,
    /// It was reopened after its shell was lost.
    restored: Option<Restored>,
    /// The lost shell's screen, to replay with the divider under it: ptyd holds nothing of
    /// the new shell but its first output.
    screen: Option<Vec<u8>>,
}

/// An [`Entry`] taken out of the table's lock, to be summarised.
struct Listed {
    id: SessionId,
    handle: SessionHandle,
    command: Vec<String>,
    exited: Option<i32>,
    started_ms: WallMs,
}

impl Listed {
    fn of(id: SessionId, e: &Entry) -> Self {
        Self {
            id,
            handle: e.handle.clone(),
            command: e.command.clone(),
            exited: e.exited,
            started_ms: e.started_ms,
        }
    }
}

/// The worker's session table. Cheap to clone; shared by every connection handler.
#[derive(Clone)]
pub struct Worker {
    inner: Arc<Inner>,
}

struct Inner {
    /// The link to ptyd, replaced when a lost one is dialled again ([`keep_ptyd`]).
    ptyd: tokio::sync::Mutex<PtydClient>,
    /// Where ptyd listens.
    socket: PathBuf,
    /// The custody this build speaks, which a ptyd must keep to be dialled; `None` takes any.
    custody: Option<String>,
    /// Each child exit, from whichever link to ptyd reported it ([`Reports::exits`]).
    exits: mpsc::UnboundedSender<(SessionId, i32)>,
    /// Output copies and checkpoints for ptyd, drained onto `ptyd` by [`tap_loop`].
    tap: mpsc::Sender<Tap>,
    sessions: Mutex<HashMap<SessionId, Entry>>,
    /// Sessions asked of ptyd and not in [`Self::sessions`] yet: their programs run already, and
    /// may speak (a hook) before the open returns.
    opening: Mutex<HashSet<SessionId>>,
    /// Told whenever an open ends, well or not.
    opened: Notify,
    /// Environment every session gets on top of the request's (`SLOPTY_WORKER_SOCKET`).
    session_env: Mutex<Vec<(String, String)>>,
    /// Where each session's presence file goes (`CLAUDE_CLIENT_PRESENCE_FILE`), once set.
    presence: Mutex<Option<PathBuf>>,
    /// The `slopty` binary agents report through and are served Slopty's tools by
    /// ([`Worker::set_relay`]).
    relay: Mutex<Option<PathBuf>>,
    /// The key each session's token is made under ([`slopty_agent::vouch`]), once set.
    session_key: Mutex<Option<slopty_agent::vouch::SessionKey>>,
    /// Sessions whose output named a local server ([`SessionStart::port_hints`]).
    port_hints: mpsc::UnboundedSender<SessionId>,
    /// Sessions whose place changed ([`SessionStart::moves`]).
    moves: mpsc::UnboundedSender<SessionId>,
    /// The coding agents seen in the sessions, as their hooks and the daemon's poll say.
    agents: Arc<dyn Agents>,
    /// Each repository's changes against `HEAD`, which every summary in it carries.
    changes: crate::changes::Changes,
    /// Which repository each one is on any machine, which every summary in it carries.
    repo_ids: crate::repo::Identities,
    /// Since when each exited session has had no viewer.
    unwatched: Mutex<Unwatched>,
    /// The sessions kept on disk, to reopen after their shell is lost.
    keeper: Keeper,
    /// Kept sessions ptyd did not hold when this worker connected: their shells were lost, and
    /// [`Worker::restore`] reopens them.
    lost: Mutex<HashMap<SessionId, Recipe>>,
    /// What the sessions' programs read of the clipboard ([`Worker::share_clipboard`]).
    clipboard: Mutex<Option<Arc<dyn crate::clip::ForSessions>>>,
}

/// A session being opened, from before ptyd starts it until it is in the table or the open
/// failed ([`Worker::get_opened`]).
struct Opening<'a> {
    inner: &'a Inner,
    id: SessionId,
}

impl<'a> Opening<'a> {
    fn new(inner: &'a Inner, id: SessionId) -> Self {
        inner.opening.lock().insert(id);
        Self { inner, id }
    }
}

impl Drop for Opening<'_> {
    fn drop(&mut self) {
        self.inner.opening.lock().remove(&self.id);
        self.inner.opened.notify_waiters();
    }
}

impl std::fmt::Debug for Worker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Worker").field("sessions", &self.inner.sessions.lock().len()).finish()
    }
}

/// What the sessions report that the daemon acts on, each read by one task of its own.
#[derive(Debug)]
pub struct Reports {
    /// Each child exit, as ptyd reports it; the caller pumps it into [`Worker::on_exit`]. A
    /// lost link to ptyd is dialled again (`keep_ptyd`), so it ends only with the worker.
    pub exits: mpsc::UnboundedReceiver<(SessionId, i32)>,
    /// The sessions whose output named a local server.
    pub port_hints: mpsc::UnboundedReceiver<SessionId>,
    /// The sessions whose directory, repository or branch changed, whose summaries are then
    /// out of date.
    pub moves: mpsc::UnboundedReceiver<SessionId>,
}

impl Worker {
    /// Connect to ptyd (default socket or `$SLOPTY_PTYD_SOCKET`) and adopt every session it
    /// already holds. `agents` is the daemon's agent table. `kept` is
    /// where sessions are kept on disk ([`crate::restore`]); the ones ptyd no longer holds wait
    /// for [`Self::restore`]. `custody` is the one this build speaks: a ptyd that says another
    /// beside its socket is refused, its sessions left alone (`dial`).
    ///
    /// A link to ptyd that is lost later is dialled again, and every session the worker runs
    /// is handed back to the ptyd it reaches (`keep_ptyd`).
    pub async fn connect(
        socket: Option<PathBuf>,
        agents: Arc<dyn Agents>,
        kept: &Path,
        custody: Option<String>,
    ) -> Result<(Self, Reports), WorkerError> {
        let path = socket.unwrap_or_else(socket_path);
        let (mut client, link_exits) = dial(&path, custody.as_deref()).await?;
        let existing = client.list().await?;
        let (keeper, mut recipes) = Keeper::open(kept)
            .map_err(|source| PtyError::Os { context: "open the kept sessions", source })?;
        let held: Vec<(SessionInfo, Option<Recipe>)> = existing
            .into_iter()
            .map(|info| {
                let recipe = recipes.remove(&info.id);
                (info, recipe)
            })
            .collect();
        let (tap, tap_rx) = mpsc::channel(TAP_QUEUE);
        let (exits_tx, exits) = mpsc::unbounded_channel();
        let (port_hints, port_hints_rx) = mpsc::unbounded_channel();
        let (moves, moves_rx) = mpsc::unbounded_channel();
        let changes = crate::changes::Changes::start(crate::changes::git_counter(), moves.clone());
        let repo_ids = crate::repo::Identities::new(crate::repo::git_identify(), moves.clone());
        let worker = Self {
            inner: Arc::new(Inner {
                ptyd: tokio::sync::Mutex::new(client),
                socket: path,
                custody,
                exits: exits_tx,
                tap,
                sessions: Mutex::new(HashMap::new()),
                opening: Mutex::new(HashSet::new()),
                opened: Notify::new(),
                session_env: Mutex::new(Vec::new()),
                presence: Mutex::new(None),
                relay: Mutex::new(slopty_agent::hooks::relay_beside_this_binary()),
                session_key: Mutex::new(None),
                port_hints,
                moves,
                agents,
                changes,
                repo_ids,
                unwatched: Mutex::new(Unwatched::default()),
                keeper,
                lost: Mutex::new(recipes),
                clipboard: Mutex::new(None),
            }),
        };
        tokio::spawn(tap_loop(Arc::downgrade(&worker.inner), tap_rx));
        tokio::spawn(keep_ptyd(Arc::downgrade(&worker.inner), link_exits));
        for (info, recipe) in held {
            tracing::info!(session = %info.id, pid = info.pid, "adopting session from ptyd");
            worker.adopt_listed(info, recipe).await;
        }
        Ok((worker, Reports { exits, port_hints: port_hints_rx, moves: moves_rx }))
    }

    /// Take over a session ptyd listed. One closed since the listing is let go. One an older
    /// worker still holds (it is exiting as this one starts) is taken in the background once
    /// that worker lets go, and announced through [`Reports::moves`] like any changed summary.
    async fn adopt_listed(&self, info: SessionInfo, recipe: Option<Recipe>) {
        let (id, size) = (info.id, info.size);
        let adoption = Adoption {
            pid: info.pid,
            exited: info.exited,
            command: recipe.as_ref().map(|r| r.command.clone()).unwrap_or_default(),
            restored: recipe.and_then(|r| r.restored),
            screen: None,
        };
        match self.adopt(id, size, adoption.clone()).await {
            Ok(_handle) => {}
            Err(WorkerError::Pty(PtyError::Daemon(PtydError::NoSuchSession))) => {
                tracing::debug!(session = %id, "closed before it was adopted");
            }
            Err(WorkerError::Pty(PtyError::Daemon(PtydError::AttachedElsewhere))) => {
                tracing::info!(session = %id, "held by an older worker; adopting once it lets go");
                let worker = self.clone();
                tokio::spawn(async move { worker.adopt_when_free(id, size, adoption).await });
            }
            Err(e) => tracing::warn!(session = %id, error = %e, "adopt failed"),
        }
    }

    async fn adopt_when_free(&self, id: SessionId, size: TermSize, adoption: Adoption) {
        let asked = tokio::time::Instant::now();
        loop {
            tokio::time::sleep(HANDOVER_POLL).await;
            match self.adopt(id, size, adoption.clone()).await {
                Ok(_handle) => {
                    tracing::info!(session = %id, "adopted from the older worker");
                    let _sent = self.inner.moves.send(id);
                    return;
                }
                Err(WorkerError::Pty(PtyError::Daemon(PtydError::AttachedElsewhere)))
                    if asked.elapsed() < HANDOVER_WAIT => {}
                Err(WorkerError::Pty(PtyError::Daemon(PtydError::NoSuchSession))) => {
                    tracing::debug!(session = %id, "closed while an older worker held it");
                    return;
                }
                Err(e) => {
                    tracing::warn!(session = %id, error = %e, "adopt failed");
                    return;
                }
            }
        }
    }

    /// The daemon's agent table.
    #[must_use]
    pub fn agents(&self) -> &dyn Agents {
        &*self.inner.agents
    }

    /// The daemon's agent table, to keep.
    #[must_use]
    pub fn shared_agents(&self) -> Arc<dyn Agents> {
        Arc::clone(&self.inner.agents)
    }

    /// Record a child exit reported by ptyd, and tell the session's viewers. A clean exit
    /// takes any conversation kept with the session with it: the person ended the program the
    /// agent ran in, or the agent itself. A signal (what a reboot sends) does not.
    pub fn on_exit(&self, id: SessionId, status: i32) {
        if let Some(e) = self.inner.sessions.lock().get_mut(&id) {
            e.exited = Some(status);
            e.handle.exited(status);
        }
        if status == 0 {
            self.inner.keeper.agent(id, None);
        }
    }

    /// What the daemon's agent tick knows of the Claude Code conversation in session `id`,
    /// kept so a reboot can resume it ([`crate::restore`]).
    pub fn keep_agent(&self, id: SessionId, agent: slopty_agent::resume::Resumable) {
        use slopty_agent::resume::Resumable;
        match agent {
            Resumable::Unknown => {}
            Resumable::No => self.inner.keeper.agent(id, None),
            Resumable::Yes(conversation) => self.inner.keeper.agent(id, Some(conversation)),
        }
    }

    /// Environment variables every future session is spawned with, in addition to the
    /// request's. The daemon uses this to tell sessions where its control socket is.
    pub fn set_session_env(&self, env: Vec<(String, String)>) {
        *self.inner.session_env.lock() = env;
    }

    /// Give every future session a token under `key` beside its id
    /// ([`slopty_proto::ctl::SESSION_TOKEN_ENV`]), proving the session its programs speak from.
    pub fn set_session_key(&self, key: slopty_agent::vouch::SessionKey) {
        *self.inner.session_key.lock() = Some(key);
    }

    /// Tell every future session where its presence file goes: a file under `dir` named by
    /// the session ([`crate::handoff::presence_file`]), which the daemon keeps while a client
    /// is focused on it.
    pub fn set_presence_dir(&self, dir: PathBuf) {
        *self.inner.presence.lock() = Some(dir);
    }

    /// Name the `slopty` binary the agents this worker starts report through and get Slopty's
    /// tools from; the one beside this binary until told otherwise, none when there is none.
    pub fn set_relay(&self, relay: Option<PathBuf>) {
        *self.inner.relay.lock() = relay;
    }

    /// What a terminal opened under `seat` would find in its environment, with `extra` under
    /// the worker's own: for a server task's thread that runs in no terminal, whose agent and
    /// Slopty tools speak as the seat all the same.
    #[must_use]
    pub fn seat_env(&self, seat: SessionId, extra: &[(String, String)]) -> Vec<(String, String)> {
        self.env_for(seat, extra)
    }

    /// What session `id` is spawned with: the request's `extra`, then every session's
    /// variables, then its own id, token and presence file. The later wins, so no request moves
    /// what the worker tells its sessions (its control socket, its server, its mod): a hook
    /// relay pointed at another socket would ask a stranger to answer its permissions.
    fn env_for(&self, id: SessionId, extra: &[(String, String)]) -> Vec<(String, String)> {
        let mut env = extra.to_vec();
        env.extend(self.inner.session_env.lock().iter().cloned());
        // Programs in the session (the `slopty hook` relay above all) learn which session they
        // run in from the environment.
        env.push((slopty_proto::ctl::SESSION_ENV.to_owned(), id.to_string()));
        let key = *self.inner.session_key.lock();
        if let Some(key) = key {
            env.push((slopty_proto::ctl::SESSION_TOKEN_ENV.to_owned(), key.token(id)));
        }
        if let Some(dir) = self.inner.presence.lock().as_deref() {
            let file = crate::handoff::presence_file(dir, id);
            env.push((PRESENCE_ENV.to_owned(), file.to_string_lossy().into_owned()));
        }
        env
    }

    /// Create a session.
    pub async fn open(&self, req: &OpenSession) -> Result<SessionHandle, WorkerError> {
        self.open_as(SessionId::new(), req).await
    }

    /// Create a session under an id the caller chose. The caller makes sure no session runs
    /// under it already; ptyd refuses an id it holds.
    pub async fn open_as(
        &self,
        id: SessionId,
        req: &OpenSession,
    ) -> Result<SessionHandle, WorkerError> {
        let _opening = Opening::new(&self.inner, id);
        let (command, extra) = self.as_agent(req).await;
        let env = self.env_for(id, &extra);
        let spec = SpawnSpec {
            command,
            // `~` is this worker's home: a client types it without knowing the path.
            cwd: req.cwd.as_deref().map(|cwd| crate::file::expand_home(Path::new(cwd))),
            env,
            size: req.size,
        };
        let recipe = Recipe {
            command: req.command.clone(),
            env: req.env.clone(),
            cwd: spec.cwd.as_deref().map(|cwd| cwd.to_string_lossy().into_owned()),
            title: req.title.clone(),
            size: req.size,
            saved_ms: WallMs::ZERO,
            restored: None,
            agent: None,
        };
        let pid = self.inner.ptyd.lock().await.spawn(id, spec).await?;
        self.inner.keeper.opened(id, recipe);
        let adoption = Adoption { pid, command: req.command.clone(), ..Adoption::default() };
        self.adopt(id, req.size, adoption).await
    }

    /// What a request runs, and with what environment. One whose program is Claude Code itself
    /// and which Slopty has not wired yet (a tile opened on `claude`, ⌘⇧T) is started the way
    /// the worker starts an agent of its own: with the hook relay, so its status and its
    /// permission prompts reach the app; with Slopty's tools when its session names a project,
    /// else the pointer to Slopty's CLI; with the mod; and
    /// pinned to a conversation id, so it can come back after a reboot. It is the person's own,
    /// so the mode that asks no permission is not locked. The request's
    /// arguments and variables are kept, and the mod's variables go last. One whose program is
    /// Codex gets Slopty's tools on its MCP servers when it is a project's agent (its session
    /// names a project) and this worker has a server; a Codex the person opens in a tile is
    /// theirs, and gets nothing.
    async fn as_agent(&self, req: &OpenSession) -> (Vec<String>, Vec<(String, String)>) {
        let unchanged = (req.command.clone(), req.env.clone());
        let Some(program) = req.command.first() else { return unchanged };
        let name = program.rsplit('/').next().unwrap_or(program);
        let project =
            req.env.iter().any(|(k, v)| k == slopty_proto::project::PROJECT_ENV && !v.is_empty());
        if name == CODEX {
            let relay = self.agent_launch().relay.filter(|_| project && self.served());
            let args = req.command.get(1..).unwrap_or_default();
            return match relay {
                Some(relay) if !codex_names_server(args) => {
                    let wired = codex_with_mcp(args, &relay);
                    (std::iter::once(program.clone()).chain(wired).collect(), req.env.clone())
                }
                _ => unchanged,
            };
        }
        if name != "claude" || !slopty_agent::detect::is_claude(name, &req.command) {
            return unchanged;
        }
        let args = req.command.get(1..).unwrap_or_default().to_vec();
        let launch = self.agent_launch();
        let Some(relay) = launch.relay.clone() else { return unchanged };
        let cwd = req.cwd.as_deref().map_or_else(slopty_platform::dirs::home, |cwd| {
            crate::file::expand_home(Path::new(cwd))
        });
        // Reads the person's settings files for their status line: blocking I/O.
        let wired = tokio::task::spawn_blocking(move || {
            slopty_agent::hooks::wired(args, &relay, &cwd, project)
        })
        .await;
        let Ok(Some(args)) = wired else { return unchanged };
        let (args, env) = match &launch.claude_mod {
            Some(installed) => (
                installed.args(args),
                req.env.iter().cloned().chain(installed.agent_env()).collect(),
            ),
            None => (args, req.env.clone()),
        };
        (std::iter::once(program.clone()).chain(args).collect(), env)
    }

    /// Whether this worker has a server, so `slopty mcp` can serve an agent Slopty's tools.
    fn served(&self) -> bool {
        self.inner.session_env.lock().iter().any(|(k, _)| k == slopty_proto::project::SERVER_ENV)
    }

    /// Reopen every kept session whose shell was lost (ptyd did not hold it when this worker
    /// connected), under its old id so its items keep their tiles: a new shell in the
    /// directory the old one was last in, below the old screen and a divider. The old
    /// command runs again only when it was a shell itself ([`Recipe::reopen_command`]), and a
    /// Claude Code conversation it held is resumed ([`Recipe::reopen`]).
    /// Returns the sessions reopened; one that fails stays kept for the next start.
    ///
    /// Call it once [`Self::set_session_env`] has said what every session gets.
    pub async fn restore(&self) -> Vec<SessionId> {
        let lost: Vec<(SessionId, Recipe)> =
            std::mem::take(&mut *self.inner.lost.lock()).into_iter().collect();
        if lost.is_empty() {
            return Vec::new();
        }
        let shells = crate::restore::system_shells();
        let mut launch = self.agent_launch();
        let sessions = slopty_agent::roster::sessions_dir(&slopty_platform::dirs::home());
        launch.background = crate::restore::background(sessions, lost.iter().map(|(_, r)| r)).await;
        let mut reopened = Vec::with_capacity(lost.len());
        for (id, recipe) in lost {
            match self.reopen(id, &recipe, &shells, &launch).await {
                Ok(_handle) => {
                    tracing::info!(session = %id, cwd = ?recipe.cwd, "restored a session whose shell was lost");
                    reopened.push(id);
                }
                Err(e) => tracing::warn!(session = %id, error = %e, "session not restored"),
            }
        }
        reopened
    }

    /// What an agent resumed after a reboot is given besides its own flags: the relay beside
    /// this binary, and the mod every session is told of.
    fn agent_launch(&self) -> AgentLaunch {
        let env = self.inner.session_env.lock().clone();
        let var = |name: &str| env.iter().find(|(k, _v)| k == name).map(|(_k, v)| PathBuf::from(v));
        let claude_mod = var(slopty_agent::claude_mod::DIR_ENV)
            .zip(var(slopty_agent::claude_mod::SOCKET_ENV))
            .map(|(dir, socket)| slopty_agent::claude_mod::Installed { dir, socket });
        AgentLaunch {
            relay: self
                .inner
                .relay
                .lock()
                .as_ref()
                .map(|relay| relay.to_string_lossy().into_owned()),
            claude_mod,
            background: Vec::new(),
        }
    }

    async fn reopen(
        &self,
        id: SessionId,
        recipe: &Recipe,
        shells: &str,
        launch: &AgentLaunch,
    ) -> Result<SessionHandle, WorkerError> {
        let _opening = Opening::new(&self.inner, id);
        let plan = recipe.reopen(id, shells, &slopty_platform::dirs::home(), launch);
        let screen = self.inner.keeper.screen(id).await;
        let env = self.env_for(id, &recipe.env);
        let spec = SpawnSpec {
            command: plan.command.clone(),
            cwd: plan.cwd.clone(),
            env,
            size: recipe.size,
        };
        let pid = self.inner.ptyd.lock().await.spawn(id, spec).await?;
        self.inner.keeper.opened(
            id,
            Recipe {
                command: plan.command.clone(),
                cwd: plan.cwd.map(|cwd| cwd.to_string_lossy().into_owned()),
                restored: Some(plan.restored.clone()),
                agent: plan.agent,
                ..recipe.clone()
            },
        );
        let adoption = Adoption {
            pid,
            command: plan.command,
            exited: None,
            restored: Some(plan.restored),
            screen: Some(screen),
        };
        let handle = self.adopt(id, recipe.size, adoption).await?;
        if let Some(line) = plan.launch
            && let Err(e) = handle.type_at_first_prompt(line)
        {
            tracing::warn!(session = %id, error = %e, "agent not resumed");
        }
        Ok(handle)
    }

    /// Write every session's newest screen to disk now, as the worker goes down.
    pub async fn keep_now(&self) {
        self.inner.keeper.flush().await;
    }

    async fn adopt(
        &self,
        id: SessionId,
        size: TermSize,
        adoption: Adoption,
    ) -> Result<SessionHandle, WorkerError> {
        let Adoption { pid, command, exited, restored, screen } = adoption;
        let attached = self.inner.ptyd.lock().await.attach(id).await?;
        if attached.dropped > 0 {
            tracing::warn!(session = %id, dropped = attached.dropped, "output lost before the backlog; the replay starts mid-stream");
        }
        let divide = screen.is_some();
        let term = attached.term.clone();
        let handle = session::spawn(SessionStart {
            id,
            master: attached.master,
            term: attached.term,
            checkpoint: screen.unwrap_or(attached.checkpoint),
            backlog: attached.backlog,
            tap: self.inner.tap.clone(),
            size: if attached.size == TermSize::default() { size } else { attached.size },
            scrollback_lines: SCROLLBACK_LINES,
            exited,
            port_hints: Some(self.inner.port_hints.clone()),
            moves: Some(self.inner.moves.clone()),
            touched: Some(self.inner.changes.toucher()),
            restored,
            divide,
        })?;
        let started_ms = attached.started_ms;
        self.inner
            .sessions
            .lock()
            .insert(id, Entry { handle: handle.clone(), command, exited, started_ms, pid, term });
        // Read after the insert: a reader shared meanwhile reaches the session one way or the
        // other.
        let clip = self.inner.clipboard.lock().clone();
        if let Some(clip) = clip {
            let _closed = handle.share_clipboard(clip);
        }
        Ok(handle)
    }

    /// Give every session, those opened later too, the clipboard its program reads (OSC 52)
    /// and its pastes carry.
    pub fn share_clipboard(&self, clip: &Arc<dyn crate::clip::ForSessions>) {
        *self.inner.clipboard.lock() = Some(Arc::clone(clip));
        let handles: Vec<SessionHandle> =
            self.inner.sessions.lock().values().map(|e| e.handle.clone()).collect();
        for handle in handles {
            let _closed = handle.share_clipboard(Arc::clone(clip));
        }
    }

    /// How many sessions the worker runs, exited ones included.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.inner.sessions.lock().len()
    }

    /// The running session `id`.
    pub fn get(&self, id: SessionId) -> Result<SessionHandle, WorkerError> {
        self.inner
            .sessions
            .lock()
            .get(&id)
            .map(|e| e.handle.clone())
            .ok_or(WorkerError::NoSuchSession)
    }

    /// Session `id`, waiting out its open when it is being opened: its program runs from the
    /// moment ptyd starts it, and a hook it fires may come before the open returns.
    pub async fn get_opened(&self, id: SessionId) -> Result<SessionHandle, WorkerError> {
        loop {
            let opened = self.inner.opened.notified();
            tokio::pin!(opened);
            opened.as_mut().enable();
            if let Ok(handle) = self.get(id) {
                return Ok(handle);
            }
            if !self.inner.opening.lock().contains(&id) {
                return Err(WorkerError::NoSuchSession);
            }
            opened.await;
        }
    }

    /// Summaries for the session list.
    pub async fn summaries(&self) -> Vec<SessionSummary> {
        let entries: Vec<Listed> =
            self.inner.sessions.lock().iter().map(|(id, e)| Listed::of(*id, e)).collect();
        let mut out = Vec::with_capacity(entries.len());
        for listed in entries {
            if let Some(summary) = self.summarise(listed).await {
                out.push(summary);
            }
        }
        out
    }

    /// The summary of session `id`, if it runs.
    pub async fn summary(&self, id: SessionId) -> Option<SessionSummary> {
        let listed = self.inner.sessions.lock().get(&id).map(|e| Listed::of(id, e))?;
        self.summarise(listed).await
    }

    async fn summarise(&self, listed: Listed) -> Option<SessionSummary> {
        let Listed { id, handle, command, exited, started_ms } = listed;
        let snap = handle.snapshot().await.ok()?;
        let state = match exited.or(snap.exited) {
            Some(status) => SessionState::Exited { status },
            None => SessionState::Running,
        };
        Some(SessionSummary {
            id,
            title: snap
                .title
                .clone()
                .unwrap_or_else(|| command.first().cloned().unwrap_or_else(|| "shell".to_owned())),
            cwd: snap.cwd,
            changes: snap.repo.as_deref().and_then(|repo| self.inner.changes.get(repo)),
            repo_id: snap.repo.as_deref().and_then(|repo| self.inner.repo_ids.get(id, repo)),
            repo: snap.repo,
            branch: snap.branch,
            started_ms,
            cols: snap.size.cols,
            rows: snap.size.rows,
            state,
            viewers: snap.viewers,
            command,
            progress: snap.progress,
            restored: snap.restored,
            program: snap.program,
        })
    }

    /// What can be seen of every live session from outside it ([`Probe`]): the daemon's agent
    /// tick reads this to attribute sessions no hook has spoken for.
    pub async fn probe(&self) -> Vec<(SessionId, Probe)> {
        let handles: Vec<(SessionId, SessionHandle)> = self
            .inner
            .sessions
            .lock()
            .iter()
            .filter(|(_id, e)| e.exited.is_none())
            .map(|(id, e)| (*id, e.handle.clone()))
            .collect();
        let mut out = Vec::with_capacity(handles.len());
        for (id, handle) in handles {
            if let Ok(probe) = handle.probe().await {
                out.push((id, probe));
            }
        }
        out
    }

    /// The process ptyd spawned for each session whose program still runs: the roots of the
    /// process trees `ListPorts` walks.
    pub async fn pids(&self) -> Result<Vec<(SessionId, u32)>, WorkerError> {
        let infos = self.inner.ptyd.lock().await.list().await?;
        Ok(infos.into_iter().filter(|i| i.exited.is_none()).map(|i| (i.id, i.pid)).collect())
    }

    /// The exited sessions no client has watched for `after` by `now`: the ones to close.
    /// Called on a slow sweep; each call reads the viewers of the exited sessions only.
    pub async fn stale_exits(&self, now: Instant, after: Duration) -> Vec<SessionId> {
        let exited: Vec<(SessionId, SessionHandle)> = self
            .inner
            .sessions
            .lock()
            .iter()
            .filter(|(_id, e)| e.exited.is_some())
            .map(|(id, e)| (*id, e.handle.clone()))
            .collect();
        let mut viewers = Vec::with_capacity(exited.len());
        for (id, handle) in exited {
            // A session whose actor is gone has nobody watching it.
            let watching = handle.snapshot().await.map_or(0, |snap| snap.viewers);
            viewers.push((id, watching));
        }
        self.inner.unwatched.lock().sweep(now, &viewers, after)
    }

    /// Kill the child and drop the session everywhere.
    pub async fn close(&self, id: SessionId) -> Result<(), WorkerError> {
        let entry = self.inner.sessions.lock().remove(&id).ok_or(WorkerError::NoSuchSession)?;
        self.inner.changes.forget(id);
        self.inner.repo_ids.forget(id);
        self.inner.keeper.forget(id);
        entry.handle.close();
        match self.inner.ptyd.lock().await.close(id).await {
            // ptyd has already forgotten it: closed is what was asked for.
            Ok(()) | Err(PtyError::Daemon(PtydError::NoSuchSession)) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// Since when each exited session has had no viewer: the clock [`Worker::stale_exits`] reads.
#[derive(Debug, Default)]
pub struct Unwatched {
    since: HashMap<SessionId, Instant>,
}

impl Unwatched {
    /// Note who watches each exited session now (`exited` holds them all, with their viewer
    /// counts) and return the ones unwatched for `after` or longer. A viewer restarts the
    /// clock; a session no longer listed is forgotten.
    pub fn sweep(
        &mut self,
        now: Instant,
        exited: &[(SessionId, u16)],
        after: Duration,
    ) -> Vec<SessionId> {
        self.since.retain(|id, _since| exited.iter().any(|(e, _viewers)| e == id));
        let mut stale = Vec::new();
        for &(id, viewers) in exited {
            if viewers > 0 {
                self.since.remove(&id);
                continue;
            }
            let since = *self.since.entry(id).or_insert(now);
            if now.saturating_duration_since(since) >= after {
                stale.push(id);
            }
        }
        stale
    }
}

/// Taps waiting for ptyd, from every session: 64 KiB reads at most, so this bounds the memory a
/// stalled ptyd can cost the worker at about 64 MiB. A session whose tap does not fit stops
/// tapping and checkpoints as soon as the queue has room, which replaces what it did not tap.
const TAP_QUEUE: usize = 1024;

/// Forward output copies, checkpoints and sizes to ptyd on the worker's connection until the
/// worker goes away. The taps ride the same connection as the requests, and only that connection
/// may tap (ptyd checks it holds the master), so a dying worker's last taps and its EOF reach ptyd
/// in order. A failed send is logged once and the loop goes on: a lost ptyd is dialled again
/// ([`keep_ptyd`]), which has every session checkpoint, and a rejected frame (too large) must
/// not stop the other sessions' taps.
async fn tap_loop(inner: Weak<Inner>, mut rx: mpsc::Receiver<Tap>) {
    let mut failing = false;
    while let Some(tap) = rx.recv().await {
        let Some(inner) = inner.upgrade() else { return };
        let sent = send_tap(&mut *inner.ptyd.lock().await, &tap).await;
        if let Tap::Checkpoint { id, state, place } = tap {
            inner.keeper.checkpoint(id, state, place);
        }
        match sent {
            Err(e) if !failing => {
                tracing::warn!(error = %e, "ptyd tap not sent");
                failing = true;
            }
            Err(e) => tracing::debug!(error = %e, "ptyd tap not sent"),
            Ok(()) => failing = false,
        }
    }
}

async fn send_tap(ptyd: &mut PtydClient, tap: &Tap) -> Result<(), PtyError> {
    match tap {
        Tap::Output(frame) => ptyd.output(frame).await,
        Tap::Checkpoint { id, state, .. } => ptyd.checkpoint(*id, state).await,
        Tap::Resize { id, size } => ptyd.resize(*id, *size).await,
    }
}

/// Dial ptyd on `socket`, refusing one that keeps another custody than `custody`
/// ([`Worker::connect`]): it would speak another protocol, or have handed its shells other
/// scripts. A refused ptyd and its sessions are left alone. Its custody is what it says beside
/// its socket (`ptyd.custody`, `<pid> <custody> <succession>`), trusted only from the process
/// this connection reached, which has [`CUSTODY_WAIT`] to write it.
async fn dial(
    socket: &Path,
    custody: Option<&str>,
) -> Result<(PtydClient, mpsc::UnboundedReceiver<(SessionId, i32)>), PtyError> {
    let (client, exits) = PtydClient::connect(socket).await?;
    let Some(custody) = custody else { return Ok((client, exits)) };
    let file = socket.with_extension("custody");
    let peer = client.peer_pid();
    let read = || {
        let said = std::fs::read_to_string(&file).unwrap_or_default();
        let mut words = said.split_whitespace();
        let by = words.next().and_then(|pid| pid.parse::<u32>().ok());
        let kept = words.next().map(str::to_owned);
        (by.is_some() && by == peer).then_some(kept).flatten()
    };
    let deadline = tokio::time::Instant::now().checked_add(CUSTODY_WAIT);
    let mut kept = read();
    while kept.is_none() && deadline.is_some_and(|at| tokio::time::Instant::now() < at) {
        tokio::time::sleep(CUSTODY_LOOK).await;
        kept = read();
    }
    let why = match kept {
        Some(kept) if kept == custody => return Ok((client, exits)),
        Some(kept) => format!("it keeps custody {kept}, and this worker speaks {custody}"),
        None => format!("{} does not say its custody", file.display()),
    };
    Err(PtyError::Os {
        context: "slopty-ptyd refused",
        source: std::io::Error::other(format!("{why}; its sessions are left alone")),
    })
}

/// Pump each link's child exits into the worker's, and when the link to ptyd is lost, dial it
/// again until it answers and hand it back every session ([`Worker::take_back`]). A worker
/// that lost ptyd keeps running: its sessions' masters are its own, and their shells live on.
async fn keep_ptyd(inner: Weak<Inner>, mut exits: mpsc::UnboundedReceiver<(SessionId, i32)>) {
    loop {
        while let Some(exit) = exits.recv().await {
            let Some(inner) = inner.upgrade() else { return };
            if inner.exits.send(exit).is_err() {
                return;
            }
        }
        tracing::warn!("lost slopty-ptyd; dialling it again, every session kept meanwhile");
        let mut wait = REDIAL_FIRST;
        let mut refused = false;
        exits = loop {
            tokio::time::sleep(wait).await;
            let Some(strong) = inner.upgrade() else { return };
            match dial(&strong.socket, strong.custody.as_deref()).await {
                Ok((client, link)) => {
                    *strong.ptyd.lock().await = client;
                    Worker { inner: strong }.take_back().await;
                    break link;
                }
                Err(e) => {
                    if !std::mem::replace(&mut refused, true) {
                        tracing::warn!(error = %e, "slopty-ptyd not reached yet");
                    }
                    wait = wait.saturating_mul(2).min(REDIAL_MAX);
                }
            }
        };
        tracing::info!("slopty-ptyd reached again");
    }
}

impl Worker {
    /// Hand every session whose program runs back to the ptyd just dialled: one it holds (it
    /// ran a new build in place) is reclaimed, and its exit recorded when it ended meanwhile;
    /// one it does not (it started afresh) is adopted with a copy of the master. Either way the
    /// taps since the last checkpoint are lost, so the session checkpoints next.
    async fn take_back(&self) {
        let listed = match self.inner.ptyd.lock().await.list().await {
            Ok(listed) => listed,
            Err(e) => {
                tracing::warn!(error = %e, "slopty-ptyd lists nothing; its link is lost again");
                return;
            }
        };
        let running: Vec<(SessionId, SessionHandle, u32, String, WallMs)> = self
            .inner
            .sessions
            .lock()
            .iter()
            .filter(|(_, e)| e.exited.is_none())
            .map(|(id, e)| (*id, e.handle.clone(), e.pid, e.term.clone(), e.started_ms))
            .collect();
        for (id, handle, pid, term, started_ms) in running {
            let held = listed.iter().find(|info| info.id == id);
            if let Some(status) = held.and_then(|info| info.exited) {
                // It ended while the link was down, and its exit went with the old link.
                let _gone = self.inner.exits.send((id, status));
            }
            let taken = if held.is_some() {
                self.inner.ptyd.lock().await.reclaim(id).await.map_err(WorkerError::from)
            } else {
                self.hand_back(
                    id,
                    &handle,
                    slopty_pty::Adoptee { pid, size: TermSize::default(), started_ms, term },
                )
                .await
            };
            match taken {
                Ok(()) => handle.tap_lost(),
                Err(e) => {
                    tracing::warn!(session = %id, error = %e, "not handed back to slopty-ptyd");
                }
            }
        }
    }

    /// Hand session `id` to a ptyd that does not hold it, with a copy of its master and its
    /// size of record.
    async fn hand_back(
        &self,
        id: SessionId,
        handle: &SessionHandle,
        child: slopty_pty::Adoptee,
    ) -> Result<(), WorkerError> {
        let master = handle.master().await?;
        let size = handle.snapshot().await.map_or(child.size, |snap| snap.size);
        let child = slopty_pty::Adoptee { size, ..child };
        self.inner.ptyd.lock().await.adopt(id, std::os::fd::AsFd::as_fd(&master), child).await?;
        Ok(())
    }
}

/// Codex's program name.
const CODEX: &str = "codex";
/// The name Slopty's tools take among Codex's MCP servers.
const CODEX_SERVER: &str = "mcp_servers.slopty";

/// Whether `codex` arguments name Slopty's tools server themselves (`-c mcp_servers.slopty…`).
fn codex_names_server(args: &[String]) -> bool {
    let named = |value: &str| value.trim_start().starts_with(CODEX_SERVER);
    args.iter()
        .zip(args.iter().skip(1))
        .any(|(flag, value)| matches!(flag.as_str(), "-c" | "--config") && named(value))
        || args.iter().any(|a| a.strip_prefix("--config=").is_some_and(named))
}

/// `codex` arguments that also serve it Slopty's tools: `<relay> mcp` among its MCP servers,
/// for that run alone, with the variables that say which server, project, task and terminal it
/// is (Codex hands an MCP server only the variables it is told to).
fn codex_with_mcp(args: &[String], relay: &str) -> Vec<String> {
    use slopty_proto::ctl::{SESSION_ENV, SESSION_TOKEN_ENV};
    use slopty_proto::project::{PROJECT_ENV, SERVER_ENV, TASK_ENV};
    let toml = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let vars = [SERVER_ENV, PROJECT_ENV, TASK_ENV, SESSION_ENV, SESSION_TOKEN_ENV];
    let vars: Vec<String> = vars.iter().map(|v| toml(v)).collect();
    let config = [
        format!("{CODEX_SERVER}.command={}", toml(relay)),
        format!("{CODEX_SERVER}.args=[\"mcp\"]"),
        format!("{CODEX_SERVER}.env_vars=[{}]", vars.join(",")),
    ];
    config.into_iter().flat_map(|c| ["-c".to_owned(), c]).chain(args.iter().cloned()).collect()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use slopty_core::SessionId;

    use super::Unwatched;

    /// An exited session goes once nobody has watched it for the whole bound: a viewer
    /// restarts the clock, and a session closed meanwhile is forgotten.
    #[test]
    fn an_exited_session_goes_only_after_the_bound_unwatched() {
        let (a, b) = (SessionId::new(), SessionId::new());
        let hour = Duration::from_hours(1);
        let bound = 24 * hour;
        let t0 = Instant::now();
        let mut clock = Unwatched::default();
        assert!(clock.sweep(t0, &[(a, 0), (b, 1)], bound).is_empty(), "the clock starts");
        assert_eq!(clock.sweep(t0 + 23 * hour, &[(a, 0), (b, 0)], bound), []);
        assert_eq!(clock.sweep(t0 + bound, &[(a, 0), (b, 0)], bound), vec![a], "a's day is up");
        // b was watched at t0, so its clock began at 23 h; a viewer at 30 h restarts it.
        assert_eq!(clock.sweep(t0 + 30 * hour, &[(b, 2)], bound), []);
        assert!(clock.sweep(t0 + 50 * hour, &[(b, 0)], bound).is_empty(), "20 h unwatched");
        assert_eq!(clock.sweep(t0 + 74 * hour, &[(b, 0)], bound), vec![b]);
        assert_eq!(clock.sweep(t0 + 75 * hour, &[], bound), []);
        assert!(clock.since.is_empty(), "closed sessions are forgotten");
    }
}
