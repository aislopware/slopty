//! Agents the host drives over Claude Code's stream-json protocol.
//!
//! A `SessionKind::Agent` session is a `claude` process the daemon owns: no PTY, one JSON
//! record per line on stdout, the host's prompts and answers on stdin
//! (`slopty_agent::stream`, DECISIONS "Claude Code", structured driving). Each one runs in its
//! own task here, which reads the process, folds what it says into the conversation the
//! clients see, and broadcasts every change on the daemon's event channel: transcript entries
//! as `HostMsg::Transcript`, the streaming text as `HostMsg::AgentPartial` (coalesced to
//! [`PARTIAL_EVERY`]), status as `HostMsg::Agent` with `AgentSource::Driven`, and a tool
//! waiting on the human as `HostMsg::AgentPermission`. The table keeps a mirror of that state
//! so a client that arrives later gets a snapshot.
//!
//! The process lives until the session is closed (stdin closes, the child is killed) or it
//! exits by itself, which ends the session as a terminal's child exiting would.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use slopty_agent::stream::{self, Fold, Update};
use slopty_core::{ClientId, SessionId};
use slopty_proto::HostMsg;
use slopty_proto::agent::{
    AgentAnswer, AgentEvent, AgentInfo, AgentKind, AgentSessionInfo, AgentSource, AgentStatus,
    AgentTask, BlockReason, Image, OpenAgent, PermissionRequest, TranscriptEntry, TranscriptUpdate,
};
use slopty_proto::terminal::{CloseReason, SessionKind, SessionState, SessionSummary};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc};

use crate::Daemon;

/// The binary to run instead of `claude` through the login shell (tests point it at a fake).
pub const CLAUDE_BIN_ENV: &str = "SLOPTY_CLAUDE_BIN";
/// How often the streaming text is sent while it grows: tokens arrive faster than a client
/// can usefully repaint markdown.
const PARTIAL_EVERY: Duration = Duration::from_millis(40);
/// Entries kept per agent for the snapshot a late client gets.
const KEEP_ENTRIES: usize = 400;
/// How long a closed agent gets to exit on its own before it is killed.
const EXIT_GRACE: Duration = Duration::from_millis(500);
/// How many past conversations a `ListAgentSessions` answers with.
const SESSIONS_LISTED: usize = 30;

/// What a client can ask a driven agent.
#[derive(Debug)]
enum Cmd {
    Say { text: String, images: Vec<Image> },
    Answer(AgentAnswer),
    Interrupt,
    Set { model: Option<String>, permission_mode: Option<String> },
    Close,
}

/// One driven agent, as the table mirrors it for snapshots.
#[derive(Debug)]
struct Entry {
    summary: SessionSummary,
    cmd: mpsc::UnboundedSender<Cmd>,
    entries: VecDeque<TranscriptEntry>,
    partial: String,
    pending: Vec<PermissionRequest>,
    event: AgentEvent,
    info: AgentInfo,
    /// The subagents seen, newest state per call.
    tasks: Vec<AgentTask>,
}

/// A snapshot of one driven agent for a client that attaches or follows it.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// The last entries.
    pub entries: Vec<TranscriptEntry>,
    /// The text being streamed, if any.
    pub partial: String,
    /// Tools waiting on the human.
    pub pending: Vec<PermissionRequest>,
    /// The agent's status.
    pub event: AgentEvent,
    /// What the agent said about itself.
    pub info: AgentInfo,
    /// The subagents it spawned, newest state per call.
    pub tasks: Vec<AgentTask>,
}

/// The driven agents of this daemon.
#[derive(Debug, Clone, Default)]
pub struct Driven {
    inner: Arc<Mutex<HashMap<SessionId, Entry>>>,
}

/// Why an agent could not be started or addressed.
#[derive(Debug, thiserror::Error)]
pub enum DrivenError {
    /// Spawning the process failed.
    #[error("cannot start claude: {0}")]
    Spawn(std::io::Error),
    /// No driven agent has that session id.
    #[error("no driven agent {0}")]
    Unknown(SessionId),
    /// A prompt carried more pictures, or a larger one, than the wire allows.
    #[error("too many pictures or one too large: {0} pictures, largest {1} bytes")]
    Pictures(usize, usize),
}

impl Driven {
    /// Whether `session` is a driven agent.
    #[must_use]
    pub fn contains(&self, session: SessionId) -> bool {
        self.inner.lock().contains_key(&session)
    }

    /// Every driven agent, for the session list.
    #[must_use]
    pub fn summaries(&self) -> Vec<SessionSummary> {
        self.inner.lock().values().map(|e| e.summary.clone()).collect()
    }

    /// Every driven agent's status, for a client that just connected.
    #[must_use]
    pub fn events(&self) -> Vec<AgentEvent> {
        self.inner.lock().values().map(|e| e.event.clone()).collect()
    }

    /// What a client attaching to `session` is shown.
    #[must_use]
    pub fn snapshot(&self, session: SessionId) -> Option<Snapshot> {
        let table = self.inner.lock();
        let entry = table.get(&session)?;
        let snapshot = Snapshot {
            entries: entry.entries.iter().cloned().collect(),
            partial: entry.partial.clone(),
            pending: entry.pending.clone(),
            event: entry.event.clone(),
            info: entry.info.clone(),
            tasks: entry.tasks.clone(),
        };
        drop(table);
        Some(snapshot)
    }

    /// The conversations Claude Code has on disk for `cwd`, or for every directory on this
    /// host when `None`, newest first, for a client that wants to resume one.
    #[must_use]
    pub fn sessions(cwd: Option<&str>) -> (Option<String>, Vec<AgentSessionInfo>) {
        let home = std::env::var_os("HOME").map_or_else(|| "/".into(), std::path::PathBuf::from);
        let sessions = match cwd {
            Some(cwd) => {
                slopty_agent::discover::sessions(&home, std::path::Path::new(cwd), SESSIONS_LISTED)
            }
            None => slopty_agent::discover::all_sessions(&home, SESSIONS_LISTED),
        };
        (cwd.map(str::to_owned), sessions)
    }

    /// Start an agent. The summary is returned for the `SessionOpened` the caller broadcasts.
    ///
    /// # Errors
    ///
    /// When the process cannot be spawned.
    pub fn open(&self, daemon: &Daemon, req: &OpenAgent) -> Result<SessionSummary, DrivenError> {
        let cwd = req.cwd.clone().unwrap_or_else(default_cwd);
        let mut command = launch(req, &cwd);
        let mut child = command.spawn().map_err(DrivenError::Spawn)?;
        let session = SessionId::new();
        let summary = SessionSummary {
            id: session,
            kind: SessionKind::Agent,
            title: req.title.clone().unwrap_or_else(|| "claude".to_owned()),
            repo: slopty_host::repo::root_of_str(&cwd),
            cwd: Some(cwd),
            cols: 0,
            rows: 0,
            state: SessionState::Running,
            viewers: 0,
            command: vec!["claude".to_owned()],
        };
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let event = AgentEvent {
            session,
            kind: AgentKind::ClaudeCode,
            status: AgentStatus::Working,
            agent_session: None,
            detail: Some("starting".to_owned()),
            attention: false,
            source: AgentSource::Driven,
        };
        self.inner.lock().insert(
            session,
            Entry {
                summary: summary.clone(),
                cmd: cmd_tx,
                entries: VecDeque::new(),
                partial: String::new(),
                pending: Vec::new(),
                event: event.clone(),
                info: AgentInfo {
                    agent_session: req.resume.clone(),
                    model: req.model.clone(),
                    ..AgentInfo::default()
                },
                tasks: Vec::new(),
            },
        );
        let _sent = daemon.events.send(HostMsg::Agent(event));
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let table = self.clone();
        let daemon = daemon.clone();
        let resumed = req.resume.clone();
        tokio::spawn(async move {
            let pump = Pump { session, table: table.clone(), events: daemon.events.clone() };
            if let Some(id) = resumed {
                pump.seed_past(&id).await;
            }
            if let Some(stderr) = stderr {
                tokio::spawn(async move {
                    let mut lines = BufReader::new(stderr).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        tracing::debug!(%session, "claude stderr: {line}");
                    }
                });
            }
            let (Some(stdin), Some(stdout)) = (stdin, stdout) else {
                tracing::error!(%session, "claude spawned without pipes");
                pump.ended(&daemon, &mut child).await;
                return;
            };
            pump.run(stdin, stdout, cmd_rx).await;
            pump.ended(&daemon, &mut child).await;
        });
        Ok(summary)
    }

    /// Send the human's next prompt.
    ///
    /// # Errors
    ///
    /// When `session` is not a driven agent.
    pub fn say(
        &self,
        session: SessionId,
        text: String,
        images: Vec<Image>,
    ) -> Result<(), DrivenError> {
        let largest = images.iter().map(|i| i.data.len()).max().unwrap_or(0);
        if images.len() > slopty_proto::agent::IMAGES_MAX
            || largest > slopty_proto::agent::IMAGE_BYTES_MAX
        {
            return Err(DrivenError::Pictures(images.len(), largest));
        }
        self.send(session, Cmd::Say { text, images })
    }

    /// Answer a permission request.
    ///
    /// # Errors
    ///
    /// When `session` is not a driven agent.
    pub fn answer(&self, answer: AgentAnswer) -> Result<(), DrivenError> {
        self.send(answer.session, Cmd::Answer(answer))
    }

    /// Stop the running turn.
    ///
    /// # Errors
    ///
    /// When `session` is not a driven agent.
    pub fn interrupt(&self, session: SessionId) -> Result<(), DrivenError> {
        self.send(session, Cmd::Interrupt)
    }

    /// Switch the agent's model and/or permission mode in place.
    ///
    /// # Errors
    ///
    /// When `session` is not a driven agent.
    pub fn set(
        &self,
        session: SessionId,
        model: Option<String>,
        permission_mode: Option<String>,
    ) -> Result<(), DrivenError> {
        self.send(session, Cmd::Set { model, permission_mode })
    }

    /// Close the agent: stdin closes, the process gets [`EXIT_GRACE`], then it is killed. The
    /// session ends through the pump, which broadcasts `SessionClosed`.
    ///
    /// # Errors
    ///
    /// When `session` is not a driven agent.
    pub fn close(&self, session: SessionId) -> Result<(), DrivenError> {
        self.send(session, Cmd::Close)
    }

    fn send(&self, session: SessionId, cmd: Cmd) -> Result<(), DrivenError> {
        let table = self.inner.lock();
        let sent = table.get(&session).map(|entry| entry.cmd.send(cmd));
        drop(table);
        match sent {
            Some(Ok(())) => Ok(()),
            Some(Err(_)) | None => Err(DrivenError::Unknown(session)),
        }
    }
}

/// Where an agent runs when the client names no directory: the daemon's home.
fn default_cwd() -> String {
    std::env::var("HOME").unwrap_or_else(|_unset| "/".to_owned())
}

/// The `claude` invocation: the binary named by [`CLAUDE_BIN_ENV`], else `claude` through the
/// user's interactive login shell, which is what resolves an alias or `~/.claude/local` as the
/// "+ agent" terminal does.
fn launch(req: &OpenAgent, cwd: &str) -> Command {
    let args = stream::arguments(req.resume.as_deref(), req.model.as_deref());
    let mut command = if let Some(bin) = std::env::var_os(CLAUDE_BIN_ENV) {
        let mut c = Command::new(bin);
        c.args(&args);
        c
    } else {
        let shell = std::env::var("SHELL").unwrap_or_else(|_unset| "/bin/zsh".to_owned());
        let words = ["exec", "claude"].into_iter().chain(args.iter().map(String::as_str));
        let mut c = Command::new(shell);
        c.arg("-lic").arg(stream::shell_line(words));
        c
    };
    command
        .current_dir(cwd)
        .env_remove("CLAUDECODE")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    command
}

/// The per-agent task's handle on the table and the clients.
struct Pump {
    session: SessionId,
    table: Driven,
    events: broadcast::Sender<HostMsg>,
}

impl Pump {
    async fn run(
        &self,
        mut stdin: tokio::process::ChildStdin,
        stdout: tokio::process::ChildStdout,
        mut cmds: mpsc::UnboundedReceiver<Cmd>,
    ) {
        let mut lines = BufReader::new(stdout).lines();
        let mut fold = Fold::default();
        // Retune requests in flight, by request id: what to record when the agent acks.
        let mut asked: HashMap<String, Ask> = HashMap::new();
        let mut set_seq = 0_u64;
        let mut partial_dirty = false;
        let mut partial_tick = tokio::time::interval(PARTIAL_EVERY);
        partial_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                line = lines.next_line() => {
                    let Ok(Some(line)) = line else { break };
                    let Some(event) = stream::parse(&line) else {
                        tracing::debug!(session = %self.session, "claude said: {line}");
                        continue;
                    };
                    if let stream::Event::ControlAck { request_id, error } = &event
                        && let Some(ask) = asked.remove(request_id)
                    {
                        match error {
                            None => self.acked(ask),
                            Some(e) => tracing::warn!(session = %self.session, %e, ?ask, "retune refused"),
                        }
                    }
                    for update in fold.apply(event, now_millis()) {
                        // Streamed text is coalesced to the tick; its end (the text became an
                        // entry) goes at once, so no client shows it twice beside the entry.
                        let flush = match &update {
                            Update::Partial(text) if text.is_empty() => Some(true),
                            Update::Partial(_) => Some(false),
                            _ => None,
                        };
                        self.publish(update, &fold);
                        match flush {
                            Some(true) => {
                                partial_dirty = false;
                                self.send_partial();
                            }
                            Some(false) => partial_dirty = true,
                            None => {}
                        }
                    }
                }
                cmd = cmds.recv() => {
                    let Some(cmd) = cmd else { break };
                    let line = match cmd {
                        Cmd::Say { text, images } => {
                            self.said(&text, images.len());
                            Some(stream::user_message(&text, &images))
                        }
                        Cmd::Answer(answer) => {
                            let line = fold.answer(
                                &answer.request,
                                answer.allowed,
                                answer.message.as_deref(),
                                &answer.answers,
                                answer.always,
                            );
                            if line.is_some() {
                                self.answered(&answer.request);
                            }
                            line
                        }
                        Cmd::Interrupt => Some(stream::interrupt(&format!("slopty-{}", now_millis()))),
                        Cmd::Set { model, permission_mode } => {
                            let mut lines = Vec::new();
                            for ask in model
                                .map(Ask::Model)
                                .into_iter()
                                .chain(permission_mode.map(Ask::PermissionMode))
                            {
                                set_seq = set_seq.wrapping_add(1);
                                let id = format!("slopty-set-{set_seq}");
                                lines.push(match &ask {
                                    Ask::Model(m) => stream::set_model(&id, m),
                                    Ask::PermissionMode(m) => stream::set_permission_mode(&id, m),
                                });
                                asked.insert(id, ask);
                            }
                            (!lines.is_empty()).then(|| lines.join("\n"))
                        }
                        Cmd::Close => {
                            let _closed = stdin.shutdown().await;
                            tokio::time::sleep(EXIT_GRACE).await;
                            break;
                        }
                    };
                    if let Some(mut line) = line {
                        line.push('\n');
                        if let Err(e) = stdin.write_all(line.as_bytes()).await {
                            tracing::warn!(session = %self.session, error = %e, "claude stdin");
                            break;
                        }
                    }
                }
                _ = partial_tick.tick(), if partial_dirty => {
                    partial_dirty = false;
                    self.send_partial();
                }
            }
        }
    }

    /// Broadcast the streamed text as the table holds it now.
    fn send_partial(&self) {
        let text = self.table.inner.lock().get(&self.session).map(|e| e.partial.clone());
        if let Some(text) = text {
            let _sent = self.events.send(HostMsg::AgentPartial { session: self.session, text });
        }
    }

    /// The human's prompt is shown at once, before the agent replays it; the replay is then
    /// the same entry again, so it is dropped (see [`Pump::publish`]).
    fn said(&self, text: &str, images: usize) {
        let entry = TranscriptEntry {
            at: Some(now_millis()),
            body: slopty_proto::agent::TranscriptBody::User {
                text: text.to_owned(),
                images: u32::try_from(images).unwrap_or(u32::MAX),
            },
        };
        self.append(vec![entry]);
        self.status(AgentStatus::Working, Some(slopty_agent::truncate(text)));
    }

    fn answered(&self, request: &str) {
        let mut table = self.table.inner.lock();
        if let Some(entry) = table.get_mut(&self.session) {
            entry.pending.retain(|p| p.id != request);
        }
        drop(table);
        self.status(AgentStatus::Working, None);
    }

    fn publish(&self, update: Update, fold: &Fold) {
        match update {
            Update::Entries(entries) => {
                // The prompt the host wrote is replayed by the agent; it was shown when sent.
                let entries: Vec<TranscriptEntry> =
                    entries.into_iter().filter(|e| !self.is_replay(e)).collect();
                if !entries.is_empty() {
                    self.append(entries);
                }
            }
            Update::Partial(text) => {
                let mut table = self.table.inner.lock();
                if let Some(entry) = table.get_mut(&self.session) {
                    entry.partial = text;
                }
            }
            Update::Status { status, detail } => {
                let agent_session = fold.init().map(|i| i.session_id.clone());
                if let Some(id) = agent_session {
                    let mut table = self.table.inner.lock();
                    if let Some(entry) = table.get_mut(&self.session) {
                        entry.event.agent_session = Some(id);
                    }
                }
                self.status(status, detail);
            }
            Update::Permission(request) => {
                let mut table = self.table.inner.lock();
                if let Some(entry) = table.get_mut(&self.session) {
                    entry.pending.push(request.clone());
                }
                drop(table);
                let _sent =
                    self.events.send(HostMsg::AgentPermission { session: self.session, request });
            }
            Update::Turn(result) => {
                tracing::info!(
                    session = %self.session,
                    ok = result.ok,
                    turns = result.turns,
                    cost_usd = result.cost_usd,
                    "turn ended"
                );
                self.info(|info| {
                    info.turns = result.turns;
                    info.cost_micro_usd = micro_usd(result.cost_usd);
                    if !result.session_id.is_empty() {
                        info.agent_session = Some(result.session_id.clone());
                    }
                });
            }
            Update::Init(init) => self.info(|info| {
                info.agent_session = Some(init.session_id.clone()).filter(|s| !s.is_empty());
                info.model = Some(init.model.clone()).filter(|m| !m.is_empty());
                info.permission_mode = Some(init.permission_mode.clone()).filter(|m| !m.is_empty());
                info.slash_commands.clone_from(&init.slash_commands);
            }),
            Update::Task(task) => {
                let mut table = self.table.inner.lock();
                if let Some(entry) = table.get_mut(&self.session) {
                    match entry.tasks.iter_mut().find(|t| t.call == task.call) {
                        Some(seen) => *seen = task.clone(),
                        None => entry.tasks.push(task.clone()),
                    }
                }
                drop(table);
                let _sent = self.events.send(HostMsg::AgentTask { session: self.session, task });
            }
            Update::Usage(usage) => self.info(|info| info.usage = Some(usage.clone())),
            Update::Context(context) => self.info(|info| info.context = Some(context)),
            Update::Model(model) => self.info(|info| info.model = Some(model.clone())),
            Update::PermissionMode(mode) => {
                self.info(|info| info.permission_mode = Some(mode.clone()));
            }
        }
    }

    /// The agent acknowledged a retune: the model is recorded as asked (the next assistant
    /// record names the full one); the mode comes back as a status record, so nothing yet.
    fn acked(&self, ask: Ask) {
        match ask {
            Ask::Model(model) => self.info(|info| info.model = Some(model.clone())),
            Ask::PermissionMode(_) => {}
        }
    }

    /// Change the agent's info and, when that changed anything, tell every client and keep
    /// the status event's `agent_session` in step.
    fn info(&self, change: impl FnOnce(&mut AgentInfo)) {
        let mut table = self.table.inner.lock();
        let Some(entry) = table.get_mut(&self.session) else { return };
        let before = entry.info.clone();
        change(&mut entry.info);
        if entry.info == before {
            return;
        }
        entry.event.agent_session.clone_from(&entry.info.agent_session);
        let info = entry.info.clone();
        drop(table);
        let _sent = self.events.send(HostMsg::AgentInfo { session: self.session, info });
    }

    /// A user entry equal to the last one shown (the agent replaying the prompt just sent).
    fn is_replay(&self, entry: &TranscriptEntry) -> bool {
        use slopty_proto::agent::TranscriptBody;
        let TranscriptBody::User { text, images } = &entry.body else { return false };
        let table = self.table.inner.lock();
        table.get(&self.session).and_then(|e| e.entries.back()).is_some_and(|last| {
            matches!(&last.body, TranscriptBody::User { text: shown, images: pictures }
                if shown == text && pictures == images)
        })
    }

    /// A resumed conversation's past, read from its transcript off the runtime and shown
    /// before the agent says anything new, so the card does not open empty.
    async fn seed_past(&self, id: &str) {
        let cwd = {
            let table = self.table.inner.lock();
            table.get(&self.session).and_then(|e| e.summary.cwd.clone())
        };
        let Some(cwd) = cwd else { return };
        let id = id.to_owned();
        let read = tokio::task::spawn_blocking(move || {
            let home = std::path::PathBuf::from(default_cwd());
            slopty_agent::discover::conversation(
                &home,
                std::path::Path::new(&cwd),
                &id,
                KEEP_ENTRIES,
            )
        })
        .await;
        match read {
            Ok(entries) if !entries.is_empty() => {
                tracing::info!(session = %self.session, entries = entries.len(), "resumed past");
                self.append(entries);
            }
            Ok(_none) => {}
            Err(e) => tracing::warn!(session = %self.session, error = %e, "transcript read"),
        }
    }

    fn append(&self, entries: Vec<TranscriptEntry>) {
        let mut table = self.table.inner.lock();
        let Some(entry) = table.get_mut(&self.session) else { return };
        for e in &entries {
            if entry.entries.len() >= KEEP_ENTRIES {
                entry.entries.pop_front();
            }
            entry.entries.push_back(e.clone());
        }
        drop(table);
        let update = TranscriptUpdate { session: self.session, reset: false, entries };
        let _sent = self.events.send(HostMsg::Transcript(update));
    }

    fn status(&self, status: AgentStatus, detail: Option<String>) {
        let event = {
            let mut table = self.table.inner.lock();
            let Some(entry) = table.get_mut(&self.session) else { return };
            let was_blocked = matches!(entry.event.status, AgentStatus::Blocked(_));
            let attention = match status {
                AgentStatus::Blocked(BlockReason::IdlePrompt) => false,
                AgentStatus::Blocked(_) => !was_blocked,
                AgentStatus::Done => true,
                _ => false,
            };
            if entry.event.status == status && entry.event.detail == detail {
                return;
            }
            entry.event.status = status;
            entry.event.detail = detail;
            entry.event.attention = attention;
            let event = entry.event.clone();
            drop(table);
            event
        };
        let _sent = self.events.send(HostMsg::Agent(event));
    }

    /// The process is gone (or being closed): kill what is left, drop the table entry and
    /// tell every client the session ended.
    async fn ended(&self, daemon: &Daemon, child: &mut Child) {
        let status = match tokio::time::timeout(EXIT_GRACE, child.wait()).await {
            Ok(Ok(status)) => status.code().unwrap_or(-1),
            _timeout_or_error => {
                let _killed = child.kill().await;
                -1
            }
        };
        tracing::info!(session = %self.session, status, "claude exited");
        let removed = self.table.inner.lock().remove(&self.session);
        if let Some(mut entry) = removed {
            entry.event.status = AgentStatus::None;
            entry.event.attention = false;
            let _sent = self.events.send(HostMsg::Agent(entry.event));
        }
        let _sent = self
            .events
            .send(HostMsg::SessionClosed { session: self.session, reason: CloseReason::Exited });
        for delta in daemon.canvas.remove_session(self.session, ClientId::nil()) {
            let _sent = self.events.send(HostMsg::Canvas(delta));
        }
    }
}

/// A retune in flight, keyed by its request id until the agent acks it.
#[derive(Debug)]
enum Ask {
    Model(String),
    PermissionMode(String),
}

/// Dollars to millionths of a dollar, saturating; a negative or NaN estimate is zero.
fn micro_usd(usd: f64) -> u64 {
    let micro = (usd * 1_000_000.0).round();
    if micro.is_finite() && micro > 0.0 {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "checked finite and positive; f64 to u64 within 2^53 is exact"
        )]
        {
            micro.min(9_007_199_254_740_992.0) as u64
        }
    } else {
        0
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}
