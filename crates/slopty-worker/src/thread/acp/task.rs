//! A driven ACP agent: its messages carried both ways by one task until it ends or the worker
//! goes.

use std::collections::{HashMap, VecDeque};
use std::process::Stdio;

use serde::Serialize;
use slopty_agent::acp::driven::{self, Answered, Cancelled, Session, Switch};
use slopty_agent::acp::rpc::{self, Incoming};
use slopty_agent::acp::schema as acp;
use slopty_core::WallMs;
use slopty_proto::thread::{Action, IntentId, ThreadId, ThreadState};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::{Ended, Launcher, SHUTDOWN, ThreadAsk};
use crate::thread::Host;

/// The lines of the agent's stderr kept, to say why it ended when it failed.
const STDERR_LINES: usize = 8;

/// A running agent, but for its stdin: the process, its stdout, and the last of its stderr once
/// it closes.
pub(super) struct Process {
    child: Child,
    stdout: ChildStdout,
    stderr: JoinHandle<Vec<String>>,
}

/// Run the agent `launch` says in `cwd`, for `thread` (what its log lines are told by), with
/// `env` over the worker's own.
pub(super) fn start_agent(
    launch: &Launcher,
    cwd: &str,
    thread: String,
    env: &[(String, String)],
) -> Result<(ChildStdin, Process), String> {
    let mut command = tokio::process::Command::new(&launch.program);
    command
        .args(&launch.args)
        .envs(env.iter().map(|(n, v)| (n, v)))
        .current_dir(cwd)
        .env("PATH", &launch.path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let program = launch.program.display().to_string();
    let mut child = command.spawn().map_err(|e| format!("{program} could not start: {e}"))?;
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err(format!("{program}'s pipes could not be opened"));
    };
    let stderr = tokio::spawn(async move {
        let mut kept = VecDeque::with_capacity(STDERR_LINES);
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(%thread, "acp agent: {line}");
            if kept.len() == STDERR_LINES {
                kept.pop_front();
            }
            kept.push_back(line);
        }
        kept.into()
    });
    Ok((stdin, Process { child, stdout, stderr }))
}

/// How a thread's session is opened once the agent is initialized.
#[derive(Clone, Debug)]
pub(super) enum Opening {
    /// A new session, switched to `model` when the agent offers it.
    New { model: Option<String> },
    /// The thread's own session loaded again, which replays it.
    Load,
    /// A fork of the agent's session `from`, loaded once made when the agent can load it, which
    /// replays what it shares with `from`.
    Fork { from: String },
}

/// What an answer to one of our requests is read as.
#[derive(Clone, Debug)]
enum Expect {
    Initialize,
    New,
    Load,
    Fork,
    /// A prompt, sent as this intent.
    Prompt,
    Mode(String),
    Option,
}

/// One running agent and its thread.
pub(super) struct Task {
    host: Host,
    thread: ThreadId,
    session: Session,
    /// The agent's stdin, until it is closed to end the agent.
    stdin: Option<ChildStdin>,
    next: u64,
    expect: HashMap<String, Expect>,
    opening: Option<Opening>,
    /// Asks held until the session is open.
    held: Option<Vec<ThreadAsk>>,
    /// What a session being loaded replays, kept from the thread until the load is answered:
    /// a load that fails leaves the thread as it was.
    replay: Option<Vec<Action>>,
    /// Why the thread cannot go on with this agent, once it cannot.
    lost: Option<String>,
}

impl Task {
    /// The task for `thread`'s agent, writing to `stdin`, mapping with `session`; `first` goes
    /// once the session is opened as `opening` says.
    pub(super) fn new(
        host: Host,
        thread: ThreadId,
        session: Session,
        stdin: ChildStdin,
        opening: Opening,
        first: Vec<ThreadAsk>,
    ) -> Self {
        Self {
            host,
            thread,
            session,
            stdin: Some(stdin),
            next: 0,
            expect: HashMap::new(),
            opening: Some(opening),
            held: Some(first),
            replay: None,
            lost: None,
        }
    }

    /// Serve the thread until the agent goes or the worker does; then reap the agent, tell the
    /// thread, and hand back what was asked and not taken.
    pub(super) async fn serve(
        mut self,
        agent: Process,
        mut asks: mpsc::UnboundedReceiver<ThreadAsk>,
        ended: Ended,
    ) {
        self.request("initialize", &driven::initialize(), Expect::Initialize).await;
        let Process { mut child, stdout, stderr } = agent;
        let mut lines = BufReader::new(stdout).split(b'\n');
        let mut worker_gone = false;
        while self.lost.is_none() {
            tokio::select! {
                line = lines.next_segment() => match line {
                    Ok(Some(line)) => self.heard(&line).await,
                    Ok(None) | Err(_) => break,
                },
                ask = asks.recv() => {
                    let Some(ask) = ask else {
                        worker_gone = true;
                        break;
                    };
                    match self.held.as_mut() {
                        Some(held) => held.push(ask),
                        None => self.ask(ask).await,
                    }
                }
            }
        }
        // Closing stdin asks the agent to end; one that does not in time is killed.
        drop(self.stdin.take());
        let status = if let Ok(status) = tokio::time::timeout(SHUTDOWN, child.wait()).await {
            status.ok()
        } else {
            let _killed = child.start_kill();
            child.wait().await.ok()
        };
        asks.close();
        let mut left = self.held.take().unwrap_or_default();
        while let Ok(ask) = asks.try_recv() {
            left.push(ask);
        }
        let failed = !worker_gone && status.is_some_and(|s| !s.success());
        let why = if let Some(lost) = self.lost.take() {
            stderr.abort();
            Some(lost)
        } else if failed {
            let tail = tokio::time::timeout(SHUTDOWN, stderr).await.ok().and_then(Result::ok);
            let last =
                tail.and_then(|lines| lines.into_iter().rev().find(|l| !l.trim().is_empty()));
            Some(last.map_or_else(
                || "The agent ended".to_owned(),
                |line| format!("The agent ended: {line}"),
            ))
        } else {
            stderr.abort();
            None
        };
        self.abandon_replay();
        let exited = self.session.exited(why.as_deref(), WallMs::now());
        self.host.apply(self.thread, exited);
        // An agent that failed would fail again on what it was not given: the thread says why,
        // and the next message the person sends tries again.
        if why.is_some() && !left.is_empty() {
            tracing::debug!(thread = %self.thread, dropped = left.len(), "asks of an agent that failed");
            left.clear();
        }
        let _gone = ended.send((self.thread, left));
    }

    fn apply(&mut self, actions: Vec<Action>) {
        if let Some(replay) = self.replay.as_mut() {
            replay.extend(actions);
        } else if !actions.is_empty() {
            self.host.apply(self.thread, actions);
        }
    }

    /// Drop what a load replayed, and go on from the thread as the host holds it.
    fn abandon_replay(&mut self) {
        if self.replay.take().is_some()
            && let Some((state, _)) = self.host.state(self.thread)
        {
            self.session = Session::of(&state, WallMs::now());
        }
    }

    async fn write(&mut self, line: serde_json::Result<Vec<u8>>) {
        let line = match line {
            Ok(line) => line,
            Err(e) => {
                tracing::warn!(thread = %self.thread, "a message for the agent could not be written: {e}");
                return;
            }
        };
        let Some(stdin) = self.stdin.as_mut() else { return };
        if let Err(e) = stdin.write_all(&line).await {
            tracing::debug!(thread = %self.thread, "the agent's stdin is closed: {e}");
        }
    }

    /// Request `method` with `params` under a fresh id, its answer read as `expect`.
    async fn request(&mut self, method: &str, params: &impl Serialize, expect: Expect) {
        self.next = self.next.saturating_add(1);
        self.expect.insert(self.next.to_string(), expect);
        self.write(rpc::request(self.next, method, params)).await;
    }

    async fn ask(&mut self, ask: ThreadAsk) {
        let now = WallMs::now();
        match ask {
            ThreadAsk::Send { text, attachments, intent } => {
                // The person speaking again lets what their stop held go, ahead of this.
                let released = self.session.release_queue();
                if self.session.running() || released {
                    let actions = self.session.queue(intent, &text, attachments, false);
                    self.apply(actions);
                    self.next().await;
                } else {
                    self.prompt(&text, &attachments, intent).await;
                }
            }
            ThreadAsk::Interrupting { text, attachments, intent } => {
                if self.session.release_queue() {
                    self.apply(vec![self.session.pending()]);
                }
                if self.session.running() {
                    let actions = self.session.queue(intent, &text, attachments, true);
                    self.apply(actions);
                    self.cancel(now).await;
                } else {
                    self.prompt(&text, &attachments, intent).await;
                }
            }
            ThreadAsk::Withdraw { intent } => {
                let actions = self.session.withdraw(intent).unwrap_or_default();
                self.apply(actions);
            }
            ThreadAsk::Edit { intent, text } => {
                let actions = self.session.edit(intent, &text).unwrap_or_default();
                self.apply(actions);
            }
            ThreadAsk::Interrupt => {
                let held = self.session.hold_queue();
                self.apply(held);
                self.cancel(now).await;
            }
            ThreadAsk::Answer { ask, choice, by } => {
                let Some(Answered { id, response, actions }) =
                    self.session.answer(&ask, &choice, by, now)
                else {
                    tracing::debug!(thread = %self.thread, ask = ask.0, "an answer the agent no longer takes");
                    return;
                };
                self.write(rpc::result(&id, &response)).await;
                self.apply(actions);
            }
            ThreadAsk::SetMode { mode } => {
                let switch = self.session.set_mode(&mode);
                self.switch(switch, Expect::Mode(mode)).await;
            }
            ThreadAsk::SetModel { model } => {
                let switch = self.session.set_model(&model);
                self.switch(switch, Expect::Option).await;
            }
            ThreadAsk::SetEffort { effort } => {
                let switch = self.session.set_effort(&effort);
                self.switch(switch, Expect::Option).await;
            }
        }
    }

    /// Send `switch`, a mode's answer read as `mode`.
    /// Send the next message held, once no turn is under way and no stop holds it.
    async fn next(&mut self) {
        if let Some((next, actions)) = self.session.next_queued() {
            self.apply(actions);
            self.prompt(&next.text, &next.attachments, next.intent).await;
        }
    }

    /// Cancel the turn under way, when one is, and answer what it still asked as cancelled.
    async fn cancel(&mut self, now: WallMs) {
        let Some(Cancelled { notification, answers, actions }) = self.session.cancel(now) else {
            return;
        };
        self.write(rpc::notification("session/cancel", &notification)).await;
        for (id, response) in answers {
            self.write(rpc::result(&id, &response)).await;
        }
        self.apply(actions);
    }

    async fn switch(&mut self, switch: Option<Switch>, mode: Expect) {
        match switch {
            Some(Switch::Mode(request)) => self.request("session/set_mode", &request, mode).await,
            Some(Switch::Option(request)) => {
                self.request("session/set_config_option", &request, Expect::Option).await;
            }
            None => tracing::debug!(thread = %self.thread, "a switch the agent does not offer"),
        }
    }

    /// Prompt with `text` and the files at `attachments`, read as they go.
    async fn prompt(&mut self, text: &str, attachments: &[String], intent: IntentId) {
        let attached = crate::thread::attach::read(attachments).await;
        let (request, actions) = self.session.prompt(text, &attached, intent, WallMs::now());
        self.apply(actions);
        self.request("session/prompt", &request, Expect::Prompt).await;
    }

    async fn heard(&mut self, line: &[u8]) {
        let message = match rpc::incoming(line) {
            Ok(message) => message,
            Err(e) => {
                tracing::debug!(thread = %self.thread, "the agent wrote what is no message: {e}");
                return;
            }
        };
        let now = WallMs::now();
        match message {
            Incoming::Response { id, outcome } => {
                let Some(expect) = self.expect.remove(&rpc::id_text(&id)) else {
                    tracing::debug!(thread = %self.thread, "an answer to nothing asked");
                    return;
                };
                self.answered(expect, outcome, now).await;
            }
            Incoming::Request { id, method, params } => {
                if method == "session/request_permission" {
                    match self.session.permission(&id, &params, now) {
                        Ok(actions) => self.apply(actions),
                        Err(error) => self.write(rpc::error(&id, error)).await,
                    }
                } else {
                    // No file system, no terminal: what the agent did not ask through a
                    // permission it does not get.
                    tracing::debug!(thread = %self.thread, method, "a request of the agent's refused");
                    self.write(rpc::error(&id, acp::Error::method_not_found())).await;
                }
            }
            Incoming::Notification { method, params } => {
                if method == "session/update" {
                    let actions = self.session.update(&params, now);
                    self.apply(actions);
                }
            }
        }
    }

    /// The answer `outcome` to a request read as `expect`.
    async fn answered(
        &mut self,
        expect: Expect,
        outcome: Result<serde_json::Value, acp::Error>,
        now: WallMs,
    ) {
        match (expect, outcome) {
            (Expect::Initialize, Ok(value)) => {
                let response = match serde_json::from_value::<acp::InitializeResponse>(value) {
                    Ok(response) => response,
                    Err(e) => {
                        self.lost = Some(format!("The agent's greeting did not read: {e}"));
                        return;
                    }
                };
                match self.session.initialized(&response) {
                    Ok(actions) => self.apply(actions),
                    Err(why) => {
                        self.lost = Some(why);
                        return;
                    }
                }
                self.open().await;
            }
            (Expect::New, Ok(value)) => {
                match serde_json::from_value::<acp::NewSessionResponse>(value) {
                    Ok(response) => {
                        let options = response.config_options.as_deref();
                        let actions = self.session.opened(
                            Some(&response.session_id),
                            response.modes.as_ref(),
                            options,
                            now,
                        );
                        self.apply(actions);
                        self.opened().await;
                    }
                    Err(e) => self.lost = Some(format!("The agent's session did not read: {e}")),
                }
            }
            (Expect::Load, Ok(value)) => {
                match serde_json::from_value::<acp::LoadSessionResponse>(value) {
                    Ok(response) => {
                        let options = response.config_options.as_deref();
                        let mut actions = self.replay.take().unwrap_or_default();
                        actions.extend(self.session.opened(
                            None,
                            response.modes.as_ref(),
                            options,
                            now,
                        ));
                        // Started over empty and told again through the host, which puts back
                        // what only the worker knows: which intent sent each message.
                        let empty = ThreadState::new(self.session.meta().clone());
                        if let Err(e) = self.host.reset(self.thread, empty) {
                            tracing::warn!(thread = %self.thread, "an ACP thread could not be read again: {e}");
                        }
                        self.apply(actions);
                        self.opened().await;
                    }
                    Err(e) => self.lost = Some(format!("The agent's session did not read: {e}")),
                }
            }
            (Expect::Fork, Ok(value)) => {
                match serde_json::from_value::<acp::ForkSessionResponse>(value) {
                    Ok(response) => {
                        let options = response.config_options.as_deref();
                        let actions = self.session.opened(
                            Some(&response.session_id),
                            response.modes.as_ref(),
                            options,
                            now,
                        );
                        self.apply(actions);
                        if self.session.resumable() {
                            self.opening = Some(Opening::Load);
                            self.open().await;
                        } else {
                            self.opened().await;
                        }
                    }
                    Err(e) => self.lost = Some(format!("The agent's fork did not read: {e}")),
                }
            }
            (Expect::Initialize | Expect::New | Expect::Load | Expect::Fork, Err(error)) => {
                self.lost = Some(driven::said(&error));
            }
            (Expect::Prompt, outcome) => {
                let actions = self.session.prompted(outcome.as_ref(), now);
                self.apply(actions);
                self.next().await;
            }
            (Expect::Mode(mode), Ok(_)) => {
                let actions = self.session.mode_set(&mode);
                self.apply(actions);
            }
            (Expect::Option, Ok(value)) => {
                match serde_json::from_value::<acp::SetSessionConfigOptionResponse>(value) {
                    Ok(response) => {
                        let actions = self.session.options_set(&response.config_options);
                        self.apply(actions);
                    }
                    Err(e) => tracing::debug!(thread = %self.thread, "an option's answer: {e}"),
                }
            }
            (Expect::Mode(_) | Expect::Option, Err(error)) => {
                tracing::debug!(thread = %self.thread, "a switch refused: {}", driven::said(&error));
            }
        }
    }

    /// The agent is initialized: open the session as the task was asked to.
    async fn open(&mut self) {
        match self.opening.take() {
            Some(Opening::New { model }) => {
                let request = self.session.session_new();
                self.request("session/new", &request, Expect::New).await;
                self.opening = Some(Opening::New { model });
            }
            Some(Opening::Load) => {
                // Read again from nothing: the agent replays the session before it answers.
                let empty = ThreadState::new(self.session.meta().clone());
                self.session = Session::of(&empty, WallMs::now());
                match self.session.session_load() {
                    Some(request) => {
                        self.replay = Some(Vec::new());
                        self.request("session/load", &request, Expect::Load).await;
                    }
                    None => {
                        self.lost = Some("The agent cannot take this session up again".to_owned());
                    }
                }
            }
            Some(Opening::Fork { from }) => {
                let request = self.session.session_fork(&from);
                self.request("session/fork", &request, Expect::Fork).await;
            }
            None => {}
        }
    }

    /// The session is open: the model a start asked for, then what was held for it.
    async fn opened(&mut self) {
        if let Some(Opening::New { model: Some(model) }) = self.opening.take()
            && let Some(Switch::Option(request)) = self.session.set_model(&model)
        {
            self.request("session/set_config_option", &request, Expect::Option).await;
        }
        for ask in self.held.take().unwrap_or_default() {
            self.ask(ask).await;
        }
    }
}

/// How long a run of an agent started to list its sessions has to answer.
const LIST_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// The agent `launch` says, run in `cwd` for this alone: its sessions there, as it lists them
/// (`session/list`), at most `limit`, the latest first; why not, in words. The run ends once
/// they are listed.
pub(super) async fn list(
    launch: &Launcher,
    agent: &slopty_proto::thread::AgentId,
    cwd: &str,
    limit: u32,
) -> Result<Vec<slopty_proto::thread::wire::PastSession>, String> {
    let (stdin, Process { mut child, stdout, stderr }) =
        start_agent(launch, cwd, format!("sessions in {cwd}"), &[])?;
    let mut asking = Asking { stdin, lines: BufReader::new(stdout).split(b'\n'), next: 0 };
    let listed = tokio::time::timeout(LIST_WAIT, asking.sessions(agent, cwd, limit)).await;
    drop(asking);
    if tokio::time::timeout(SHUTDOWN, child.wait()).await.is_err() {
        let _killed = child.start_kill();
        let _reaped = child.wait().await;
    }
    stderr.abort();
    listed.unwrap_or_else(|_| Err("The agent did not list its sessions in time".to_owned()))
}

/// An agent asked one thing at a time, its answers read in turn.
struct Asking {
    stdin: ChildStdin,
    lines: tokio::io::Split<BufReader<ChildStdout>>,
    next: u64,
}

impl Asking {
    async fn sessions(
        &mut self,
        agent: &slopty_proto::thread::AgentId,
        cwd: &str,
        limit: u32,
    ) -> Result<Vec<slopty_proto::thread::wire::PastSession>, String> {
        let greeting: acp::InitializeResponse =
            self.ask("initialize", &driven::initialize()).await?;
        if greeting.protocol_version != driven::PROTOCOL {
            return Err(format!("The agent speaks ACP version {}", greeting.protocol_version));
        }
        let can = &greeting.agent_capabilities;
        if can.session_capabilities.list.is_none() {
            return Err("The agent keeps no list of its sessions".to_owned());
        }
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        let mut sessions = Vec::new();
        let mut cursor = None;
        while sessions.len() < limit {
            let page: acp::ListSessionsResponse =
                self.ask("session/list", &driven::list(cwd, cursor.take())).await?;
            let room = limit.saturating_sub(sessions.len());
            sessions.extend(
                page.sessions.iter().take(room).map(|s| driven::past(agent, s, can.load_session)),
            );
            match page.next_cursor {
                Some(next) if !page.sessions.is_empty() => cursor = Some(next),
                _ => break,
            }
        }
        Ok(sessions)
    }

    /// Ask `method` with `params`: its answer, read as `T`, or why not. What the agent asks
    /// meanwhile is refused, and what it tells is passed over.
    async fn ask<T: serde::de::DeserializeOwned>(
        &mut self,
        method: &str,
        params: &impl Serialize,
    ) -> Result<T, String> {
        self.next = self.next.saturating_add(1);
        let line = rpc::request(self.next, method, params).map_err(|e| e.to_string())?;
        self.stdin.write_all(&line).await.map_err(|e| format!("The agent went: {e}"))?;
        let want = self.next.to_string();
        loop {
            let line = match self.lines.next_segment().await {
                Ok(Some(line)) => line,
                Ok(None) => return Err("The agent ended before it answered".to_owned()),
                Err(e) => return Err(format!("The agent went: {e}")),
            };
            match rpc::incoming(&line) {
                Ok(Incoming::Response { id, outcome }) if rpc::id_text(&id) == want => {
                    let value = outcome.map_err(|e| driven::said(&e))?;
                    return serde_json::from_value(value)
                        .map_err(|e| format!("The agent's answer did not read: {e}"));
                }
                Ok(Incoming::Request { id, .. }) => {
                    let refusal = rpc::error(&id, acp::Error::method_not_found());
                    if let Ok(refusal) = refusal {
                        let _gone = self.stdin.write_all(&refusal).await;
                    }
                }
                Ok(_) | Err(_) => {}
            }
        }
    }
}
