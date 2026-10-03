//! A driven pi: `pi --mode rpc` with Slopty's gate, its records carried both ways by one task
//! until it ends, the worker goes, or the session is handed to pi's TUI once it rests.

use std::collections::HashMap;
use std::process::Stdio;

use slopty_agent::pi::driven::{Answered, Driven};
use slopty_agent::pi::rpc::{self, Command, Entries, Incoming, Request, State, Stats};
use slopty_core::WallMs;
use slopty_proto::thread::{Action, IntentId, ThreadId, ThreadState};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::{Ended, Launcher, Next, SHUTDOWN, ThreadAsk, driven_of};
use crate::thread::Host;

/// The lines of pi's stderr kept, to say why it ended when it failed.
const STDERR_LINES: usize = 8;

/// A running pi, but for its stdin: the process, its stdout, and the last of its stderr once it
/// closes.
pub(super) struct Process {
    child: Child,
    stdout: ChildStdout,
    stderr: JoinHandle<Vec<String>>,
}

/// Run pi on session `session` in `cwd`, with the thread's own `args`.
pub(super) fn start_pi(
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
pub(super) enum Ready {
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
pub(super) struct Task {
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
    /// The person handed the session to pi's TUI: this pi ends once it rests.
    handoff: bool,
}

impl Task {
    /// The task for `thread`'s pi, writing to `stdin`, mapping with `driven`.
    pub(super) fn new(host: Host, thread: ThreadId, driven: Driven, stdin: ChildStdin) -> Self {
        Self {
            host,
            thread,
            driven,
            stdin: Some(stdin),
            next: 0,
            expect: HashMap::new(),
            held: None,
            lost: None,
            handoff: false,
        }
    }

    /// Whether this pi is done with: lost, or handed off and at rest.
    const fn done(&self) -> bool {
        self.lost.is_some() || (self.handoff && self.held.is_none() && self.driven.rests())
    }
}

impl Task {
    /// Serve the thread until pi goes or the worker does; then reap pi, tell the thread, and
    /// hand back what was asked and not taken.
    pub(super) async fn serve(
        mut self,
        pi: Process,
        mut asks: mpsc::UnboundedReceiver<ThreadAsk>,
        ready: Ready,
        ended: Ended,
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
        while !self.done() {
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
        // Handed off: the TUI takes the session from here, and the thread is read from it.
        if self.handoff && !worker_gone && self.lost.is_none() {
            stderr.abort();
            let _gone = ended.send((self.thread, left, Next::Tui));
            return;
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
        let _gone = ended.send((self.thread, left, Next::Rest));
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
            ThreadAsk::Send { text, attachments, intent } => {
                let attached = crate::thread::attach::read(&attachments).await;
                let command = self.driven.send(&text, &attached, intent);
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
            ThreadAsk::Handoff => self.handoff = true,
            // Slopty holds the session already.
            ThreadAsk::TakeBack => {}
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
