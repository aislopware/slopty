//! pi, driven over its RPC mode, on the worker: the IO half of the adapter
//! (`slopty_agent::pi::driven` is the codec).
//!
//! A thread is started by the person ([`Pi::start`], `ThreadRequest::Start` for agent `pi`): the
//! worker runs the person's own `pi` in the thread's directory as `pi --mode rpc` with Slopty's
//! gate ([`slopty_agent::pi`]), on a session named by the start's intent id, which pi keeps in
//! its own session directory beside the person's others. One task per running pi carries its
//! records both ways. What a client asks of the thread ([`Pi::decide`]: a message, an
//! interrupt, an answer, a model, a compaction) goes to that task.
//!
//! - **The program.** pi is found as the person's terminal finds it: on the daemon's `PATH`, else
//!   on their login shell's ([`crate::facts::installed`]), and runs with that `PATH`, so the `node`
//!   it starts with is theirs too. It runs with the daemon's environment, so it reaches its
//!   providers with the person's own settings; Slopty signs nothing in and passes only the flags
//!   that loosen nothing ([`slopty_agent::pi::checked`]).
//! - **One writer.** A session has at most one pi here: a thread's next pi starts only once the
//!   last one is reaped.
//! - **Fail closed.** The gate lets a call run only on an allow, and only an answer the gate's
//!   dialog offers goes to it. When the worker goes, pi's stdin closes and pi ends; a call waiting
//!   at the gate then never runs.
//! - **Exited, and resumed.** A thread whose pi is gone (it exited, or the worker restarted) is
//!   exited and resumable. The next message starts pi again on the same session, and the thread is
//!   read again from the session's entries before the message goes, since the session is the record
//!   and the thread a cache of it.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use slopty_agent::pi::driven::{Answered, Driven};
use slopty_agent::pi::rpc::{self, Command, Entries, Incoming, Request, State, Stats};
use slopty_core::WallMs;
use slopty_proto::thread::wire::{Intent, Outcome, Start};
use slopty_proto::thread::{
    Action, AgentId, Answerer, AskId, Cap, Delivery, Drive, IntentId, Liveness, Phase,
    RequestState, ThreadId, ThreadState,
};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use super::Host;
use crate::facts::Installed;

/// How long pi has to end once its stdin closes, before it is killed.
pub const SHUTDOWN: Duration = Duration::from_secs(5);

/// The lines of pi's stderr kept, to say why it ended when it failed.
const STDERR_LINES: usize = 8;

/// What a client asks of the driven pi threads.
#[derive(Debug)]
enum Ask {
    Start { id: IntentId, start: Box<Start>, reply: oneshot::Sender<Outcome> },
    Thread { thread: ThreadId, ask: ThreadAsk },
}

/// What a client asks of one thread.
#[derive(Debug)]
enum ThreadAsk {
    Send { text: String, intent: IntentId },
    Interrupt,
    Answer { ask: AskId, choice: String, message: Option<String>, by: Answerer },
    SetModel { model: String },
    Compact,
}

/// Where clients' asks go to the driven pi threads. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Pi(mpsc::UnboundedSender<Ask>);

/// Where a [`Pi`]'s asks wait for [`spawn`].
#[derive(Debug)]
pub struct Asks(mpsc::UnboundedReceiver<Ask>);

impl Pi {
    /// A handle, and the asks [`spawn`] takes from it.
    #[must_use]
    pub fn channel() -> (Self, Asks) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self(tx), Asks(rx))
    }

    /// Start a pi thread for intent `id` once, as `start` says: a repeat of the id gets the
    /// first outcome.
    pub async fn start(&self, id: IntentId, start: Box<Start>) -> Outcome {
        let (reply, outcome) = oneshot::channel();
        let ask = Ask::Start { id, start, reply };
        if self.0.send(ask).is_err() {
            return refused("pi threads are not served here");
        }
        outcome.await.unwrap_or_else(|_| refused("the pi threads stopped"))
    }

    /// What comes of intent `id` on the driven pi thread `state`, from `by` when it answers:
    /// done when it went to pi (or to a pi started again for it), and why not when it did not.
    /// Meant to run once per id, as [`Host::intent`] runs its decision.
    pub fn decide(
        &self,
        state: &ThreadState,
        intent: IntentId,
        ask: &Intent,
        by: Answerer,
    ) -> Outcome {
        let thread = state.meta.id;
        let live = state.status.liveness == Liveness::Live;
        let asked = match ask {
            Intent::Send { delivery: Delivery::Queue, .. } => {
                return Outcome::Unsupported { cap: Cap::named(Cap::QUEUE) };
            }
            Intent::Send { text, .. } if text.trim().is_empty() => {
                return refused("There is nothing to send");
            }
            Intent::Send { text, .. } => ThreadAsk::Send { text: text.clone(), intent },
            Intent::Interrupt => {
                if !live || !matches!(state.status.phase, Phase::Working | Phase::NeedsYou) {
                    return refused("pi is not working");
                }
                ThreadAsk::Interrupt
            }
            Intent::Answer { ask, choice, message } => {
                let Some(request) = state.requests.iter().find(|r| r.id == *ask) else {
                    return refused(&format!("There is no request {}", ask.0));
                };
                match &request.state {
                    RequestState::Open => {}
                    RequestState::Answered { .. } => return Outcome::Done,
                    RequestState::Released | RequestState::Withdrawn => {
                        return refused("pi no longer asks this");
                    }
                }
                // Only what the dialog offers goes; a question with nothing offered takes words.
                let offered = request.options.iter().any(|o| o.id == *choice);
                let written = request.options.is_empty() && !request.questions.is_empty();
                if !offered && !written {
                    return refused(&format!("There is no answer {choice}"));
                }
                ThreadAsk::Answer {
                    ask: ask.clone(),
                    choice: choice.clone(),
                    message: message.clone(),
                    by,
                }
            }
            Intent::Release { .. } => {
                return refused("pi has no prompt of its own while Slopty drives it");
            }
            Intent::SetModel { model } => {
                if !state.meta.models.iter().any(|m| m.id == *model) {
                    return refused(&format!("There is no model {model} here"));
                }
                ThreadAsk::SetModel { model: model.clone() }
            }
            Intent::Compact => ThreadAsk::Compact,
            other => return Outcome::Unsupported { cap: Cap::named(other.needs()) },
        };
        if !live && !matches!(asked, ThreadAsk::Send { .. }) {
            return refused("pi is not running");
        }
        if self.0.send(Ask::Thread { thread, ask: asked }).is_err() {
            return refused("pi threads are not served here");
        }
        Outcome::Done
    }
}

/// Whether `state` is a pi thread this worker drives.
#[must_use]
pub fn is_driven(state: &ThreadState) -> bool {
    state.meta.agent.is(AgentId::PI) && state.meta.drive.is(Drive::DRIVEN)
}

/// Serve the driven pi threads in `host`, with the gate written under `data_dir`, taking the
/// asks of their [`Pi`].
///
/// pi is looked for on `path` alone when it is given, else as the person's terminal finds it
/// ([`crate::facts::installed`]). The threads a pi of an earlier worker ran are told exited
/// first: that pi ended with it.
pub fn spawn(
    host: Host,
    data_dir: PathBuf,
    path: Option<OsString>,
    Asks(mut asks): Asks,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        for thread in host.threads() {
            let Some((state, _)) = host.state(thread) else { continue };
            if is_driven(&state) && state.status.liveness == Liveness::Live {
                host.apply(thread, slopty_agent::pi::driven::gone(&state, WallMs::now()));
            }
        }
        let (ended_tx, mut ended) = mpsc::unbounded_channel();
        let mut served = Served {
            host,
            data_dir,
            path,
            launcher: None,
            running: HashMap::new(),
            ended: ended_tx,
        };
        loop {
            tokio::select! {
                ask = asks.recv() => match ask {
                    Some(Ask::Start { id, start, reply }) => {
                        let outcome = served.begin(id, &start).await;
                        let _gone = reply.send(outcome);
                    }
                    Some(Ask::Thread { thread, ask }) => served.route(thread, vec![ask]).await,
                    None => return,
                },
                Some((thread, left)) = ended.recv() => {
                    if served.running.get(&thread).is_some_and(mpsc::UnboundedSender::is_closed) {
                        served.running.remove(&thread);
                    }
                    if !left.is_empty() {
                        served.route(thread, left).await;
                    }
                }
            }
        }
    })
}

/// How to run pi: the person's program, the `PATH` it runs with, its version, and the gate.
#[derive(Clone, Debug)]
struct Launcher {
    pi: Installed,
    gate: PathBuf,
}

/// The driven pi threads, as [`spawn`] serves them.
struct Served {
    host: Host,
    data_dir: PathBuf,
    path: Option<OsString>,
    /// Found once it is found; looked for again until then.
    launcher: Option<Launcher>,
    /// Each running pi's task, by its thread.
    running: HashMap<ThreadId, mpsc::UnboundedSender<ThreadAsk>>,
    /// Told by a task when its pi has ended and is reaped, with the asks it did not take.
    ended: mpsc::UnboundedSender<(ThreadId, Vec<ThreadAsk>)>,
}

impl Served {
    async fn launcher(&mut self) -> Result<Launcher, String> {
        if let Some(launcher) = &self.launcher {
            return Ok(launcher.clone());
        }
        let gate = slopty_agent::pi::install(&self.data_dir)
            .map_err(|e| format!("Slopty's gate for pi could not be written: {e}"))?;
        let pi = crate::facts::installed("pi", self.path.clone())
            .await
            .ok_or_else(|| "pi is not installed".to_owned())?;
        let launcher = Launcher { pi, gate };
        self.launcher = Some(launcher.clone());
        Ok(launcher)
    }

    /// Start the thread of intent `id` as `start` says, once.
    async fn begin(&mut self, id: IntentId, start: &Start) -> Outcome {
        if let Some(outcome) = self.host.started(id) {
            return outcome;
        }
        let found = self.launcher().await;
        let session = id.to_string();
        let mut begun = None;
        // What is refused is refused once, as what is started is started once.
        let outcome = self.host.start(id, || {
            if start.drive.as_ref().is_some_and(|d| !d.is(Drive::DRIVEN)) {
                return Err("pi is only driven over RPC".to_owned());
            }
            let args = slopty_agent::pi::checked(&start.args)?.to_vec();
            if !Path::new(&start.cwd).is_dir() {
                return Err(format!("There is no folder {} here", start.cwd));
            }
            let launch = found.clone()?;
            let version = launch.pi.version.clone().unwrap_or_default();
            let (driven, actions) = Driven::new(&session, &version, &start.cwd, WallMs::now());
            let meta = driven.meta().clone();
            begun = Some((driven, actions, args, launch));
            Ok(meta)
        });
        let (Outcome::Started { thread }, Some((driven, actions, args, launch))) =
            (&outcome, begun)
        else {
            return outcome;
        };
        let thread = *thread;
        self.host.apply(thread, actions);
        let mut args = args;
        if let Some((provider, model)) = start.model.as_deref().and_then(|m| m.split_once('/')) {
            args.extend(["--provider".to_owned(), provider.to_owned()]);
            args.extend(["--model".to_owned(), model.to_owned()]);
        }
        let first = start
            .prompt
            .clone()
            .filter(|p| !p.trim().is_empty())
            .map(|text| ThreadAsk::Send { text, intent: id });
        self.run(&launch, thread, driven, &args, Ready::Now(first.into_iter().collect()));
        outcome
    }

    /// `asks` for `thread`: to its pi while one runs; else a message takes the thread up again,
    /// and anything else is dropped, its intent already answered.
    async fn route(&mut self, thread: ThreadId, asks: Vec<ThreadAsk>) {
        let mut left = Vec::new();
        for ask in asks {
            let Some(task) = self.running.get(&thread) else {
                left.push(ask);
                continue;
            };
            if let Err(mpsc::error::SendError(ask)) = task.send(ask) {
                left.push(ask);
            }
        }
        if left.is_empty() {
            return;
        }
        self.running.remove(&thread);
        let (sends, dropped): (Vec<_>, Vec<_>) =
            left.into_iter().partition(|ask| matches!(ask, ThreadAsk::Send { .. }));
        for ask in dropped {
            tracing::debug!(%thread, ?ask, "an ask of a pi that is not running");
        }
        if sends.is_empty() {
            return;
        }
        let Some((state, _)) = self.host.state(thread).filter(|(s, _)| is_driven(s)) else {
            return;
        };
        let launch = match self.launcher().await {
            Ok(launch) => launch,
            Err(why) => {
                tracing::warn!(%thread, "pi could not start: {why}");
                let (mut driven, _) = driven_of(&state);
                self.host.apply(thread, driven.exited(Some(&why), WallMs::now()));
                return;
            }
        };
        let (driven, _) = driven_of(&state);
        self.run(&launch, thread, driven, &[], Ready::AfterEntries(sends));
    }

    /// Run pi for `thread` with its own `args`, and serve it until it ends.
    fn run(
        &mut self,
        launch: &Launcher,
        thread: ThreadId,
        driven: Driven,
        args: &[String],
        ready: Ready,
    ) {
        let mut driven = driven;
        let (native, cwd) = (driven.meta().native.clone(), driven.meta().cwd.clone());
        match start_pi(launch, &native, &cwd, args) {
            Ok((stdin, pi)) => {
                let (tx, rx) = mpsc::unbounded_channel();
                let task = Task {
                    host: self.host.clone(),
                    thread,
                    driven,
                    stdin: Some(stdin),
                    next: 0,
                    expect: HashMap::new(),
                    held: None,
                    lost: None,
                };
                tokio::spawn(task.serve(pi, rx, ready, self.ended.clone()));
                self.running.insert(thread, tx);
            }
            Err(why) => {
                tracing::warn!(%thread, "{why}");
                self.host.apply(thread, driven.exited(Some(&why), WallMs::now()));
            }
        }
    }
}

/// The codec of the thread `state`, as it was started.
fn driven_of(state: &ThreadState) -> (Driven, Vec<Action>) {
    let meta = &state.meta;
    Driven::new(&meta.native, &meta.agent_version, &meta.cwd, meta.created_ms)
}

/// A running pi, but for its stdin: the process, its stdout, and the last of its stderr once it
/// closes.
struct Process {
    child: Child,
    stdout: ChildStdout,
    stderr: JoinHandle<Vec<String>>,
}

/// Run pi on session `session` in `cwd`, with the thread's own `args`.
fn start_pi(
    launch: &Launcher,
    session: &str,
    cwd: &str,
    args: &[String],
) -> Result<(ChildStdin, Process), String> {
    let mut command = tokio::process::Command::new(&launch.pi.program);
    command
        .args(slopty_agent::pi::args(&launch.gate, session, args))
        .current_dir(cwd)
        .env("PATH", &launch.pi.path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|e| format!("pi could not start: {e}"))?;
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err("pi's pipes could not be opened".to_owned());
    };
    let session = session.to_owned();
    let stderr = tokio::spawn(async move {
        let mut kept = std::collections::VecDeque::with_capacity(STDERR_LINES);
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(%session, "pi: {line}");
            if kept.len() == STDERR_LINES {
                kept.pop_front();
            }
            kept.push_back(line);
        }
        kept.into()
    });
    Ok((stdin, Process { child, stdout, stderr }))
}

/// When a thread's task takes its first asks.
enum Ready {
    /// At once.
    Now(Vec<ThreadAsk>),
    /// Once the thread is read again from the session's entries.
    AfterEntries(Vec<ThreadAsk>),
}

/// What a command's response is read as.
#[derive(Clone, Copy, Debug)]
enum Expect {
    State,
    Models,
    Stats,
    Entries,
    /// A message, sent as this intent.
    Prompt(IntentId),
}

/// One running pi and its thread.
struct Task {
    host: Host,
    thread: ThreadId,
    driven: Driven,
    /// pi's stdin, until it is closed to end pi.
    stdin: Option<ChildStdin>,
    next: u64,
    expect: HashMap<String, Expect>,
    /// Asks held until the thread is read again.
    held: Option<Vec<ThreadAsk>>,
    /// Why the thread cannot go on with this pi, once it cannot.
    lost: Option<String>,
}

impl Task {
    /// Serve the thread until pi goes or the worker does; then reap pi, tell the thread, and
    /// hand back what was asked and not taken.
    async fn serve(
        mut self,
        pi: Process,
        mut asks: mpsc::UnboundedReceiver<ThreadAsk>,
        ready: Ready,
        ended: mpsc::UnboundedSender<(ThreadId, Vec<ThreadAsk>)>,
    ) {
        match ready {
            Ready::Now(first) => {
                self.ask_for(Command::GetState, Some(Expect::State)).await;
                self.ask_for(Command::GetAvailableModels, Some(Expect::Models)).await;
                for ask in first {
                    self.ask(ask).await;
                }
            }
            Ready::AfterEntries(held) => {
                self.held = Some(held);
                self.ask_for(Command::GetEntries { since: None }, Some(Expect::Entries)).await;
            }
        }
        let Process { mut child, stdout, stderr } = pi;
        let mut lines = BufReader::new(stdout).split(b'\n');
        let mut worker_gone = false;
        loop {
            let deadline = self.driven.deadline();
            let expiry = async {
                match deadline.and_then(WallMs::to_system) {
                    Some(at) => {
                        let wait =
                            at.duration_since(std::time::SystemTime::now()).unwrap_or_default();
                        tokio::time::sleep(wait).await;
                    }
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                line = lines.next_segment() => match line {
                    Ok(Some(line)) => {
                        self.heard(&line).await;
                        if self.lost.is_some() {
                            break;
                        }
                    }
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
                () = expiry => {
                    let actions = self.driven.expire(WallMs::now());
                    self.apply(actions);
                }
            }
        }
        // Closing stdin asks pi to end; one that does not in time is killed.
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
            Some(last.map_or_else(|| "pi ended".to_owned(), |line| format!("pi ended: {line}")))
        } else {
            stderr.abort();
            None
        };
        let exited = self.driven.exited(why.as_deref(), WallMs::now());
        self.host.apply(self.thread, exited);
        let _gone = ended.send((self.thread, left));
    }

    fn apply(&self, actions: Vec<Action>) {
        if !actions.is_empty() {
            self.host.apply(self.thread, actions);
        }
    }

    async fn write(&mut self, request: &Request) {
        let line = match rpc::line(request) {
            Ok(line) => line,
            Err(e) => {
                tracing::warn!(thread = %self.thread, "a command for pi could not be written: {e}");
                return;
            }
        };
        let Some(stdin) = self.stdin.as_mut() else { return };
        if let Err(e) = stdin.write_all(&line).await {
            tracing::debug!(thread = %self.thread, "pi's stdin is closed: {e}");
        }
    }

    /// Send `command` under a fresh id, its response read as `expect`.
    async fn ask_for(&mut self, command: Command, expect: Option<Expect>) {
        self.next = self.next.saturating_add(1);
        let id = format!("slopty-{}", self.next);
        if let Some(expect) = expect {
            self.expect.insert(id.clone(), expect);
        }
        self.write(&Request { id: Some(id), command }).await;
    }

    async fn ask(&mut self, ask: ThreadAsk) {
        match ask {
            ThreadAsk::Send { text, intent } => {
                let command = self.driven.send(&text, intent);
                self.ask_for(command, Some(Expect::Prompt(intent))).await;
            }
            ThreadAsk::Interrupt => {
                if let Some(command) = self.driven.interrupt() {
                    self.ask_for(command, None).await;
                }
            }
            ThreadAsk::Answer { ask, choice, message, by } => {
                let answered =
                    self.driven.answer(&ask, &choice, message.as_deref(), by, WallMs::now());
                let Some(Answered { requests, actions }) = answered else {
                    tracing::debug!(thread = %self.thread, ask = ask.0, "an answer pi no longer takes");
                    return;
                };
                for request in requests {
                    match request.id {
                        Some(_) => self.write(&request).await,
                        None => self.ask_for(request.command, None).await,
                    }
                }
                self.apply(actions);
            }
            ThreadAsk::SetModel { model } => {
                if let Some(command) = Driven::set_model(&model) {
                    self.ask_for(command, None).await;
                    self.ask_for(Command::GetState, Some(Expect::State)).await;
                }
            }
            ThreadAsk::Compact => {
                self.ask_for(Command::Compact { custom_instructions: None }, None).await;
            }
        }
    }

    async fn heard(&mut self, line: &[u8]) {
        let record = match rpc::record(line) {
            Ok(record) => record,
            Err(e) => {
                tracing::debug!(thread = %self.thread, "pi wrote what is no record: {e}");
                return;
            }
        };
        let now = WallMs::now();
        if let Incoming::Response(response) = &record
            && let Some(expect) = response.id.as_ref().and_then(|id| self.expect.remove(id))
        {
            if response.success {
                let data = response.data.clone().unwrap_or_default();
                match expect {
                    Expect::State => {
                        if let Ok(state) = serde_json::from_value::<State>(data) {
                            let actions = self.driven.state(&state);
                            self.apply(actions);
                        }
                    }
                    Expect::Models => {
                        let models = data.get("models").cloned().unwrap_or_default();
                        if let Ok(models) = serde_json::from_value::<Vec<rpc::Model>>(models) {
                            let actions = self.driven.models(&models);
                            self.apply(actions);
                        }
                    }
                    Expect::Stats => {
                        if let Ok(stats) = serde_json::from_value::<Stats>(data) {
                            let actions = self.driven.stats(&stats);
                            self.apply(actions);
                        }
                    }
                    Expect::Entries => self.read_again(data, now).await,
                    Expect::Prompt(_) => {}
                }
                return;
            }
            match expect {
                Expect::Prompt(intent) => self.driven.unsent(intent),
                // Without the session's record the thread cannot go on where it was.
                Expect::Entries => {
                    let why = response.error.as_deref().unwrap_or("no reason given");
                    self.lost = Some(format!("pi did not give the session back: {why}"));
                    return;
                }
                Expect::State | Expect::Models | Expect::Stats => {}
            }
        }
        let settled = matches!(record, Incoming::AgentSettled);
        let actions = self.driven.incoming(&record, now);
        self.apply(actions);
        if settled {
            self.ask_for(Command::GetSessionStats, Some(Expect::Stats)).await;
        }
    }

    /// The thread read again from the session's `entries`, then what was held for it.
    async fn read_again(&mut self, entries: serde_json::Value, now: WallMs) {
        if let Some((state, _)) = self.host.state(self.thread) {
            let (mut driven, mut actions) = driven_of(&state);
            match serde_json::from_value::<Entries>(entries) {
                Ok(entries) => actions.extend(driven.entries(&entries, now)),
                Err(e) => tracing::warn!(thread = %self.thread, "pi's entries did not read: {e}"),
            }
            // Started over empty and told again through the host, which puts back what only the
            // worker knows: which intent sent each message, each turn's snapshots.
            let empty = ThreadState::new(driven.meta().clone());
            if let Err(e) = self.host.reset(self.thread, empty) {
                tracing::warn!(thread = %self.thread, "a pi thread could not be read again: {e}");
            }
            self.apply(actions);
            self.driven = driven;
        }
        self.ask_for(Command::GetState, Some(Expect::State)).await;
        self.ask_for(Command::GetAvailableModels, Some(Expect::Models)).await;
        for ask in self.held.take().unwrap_or_default() {
            self.ask(ask).await;
        }
    }
}

fn refused(reason: &str) -> Outcome {
    Outcome::Refused { reason: reason.to_owned() }
}
