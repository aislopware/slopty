//! A Claude Code thread started from a client: `ThreadRequest::Start` for agent `claude-code`.
//!
//! The person's own `claude`, unmodified, runs in one of the worker's terminals, and the thread
//! is the observed one that terminal makes.
//!
//! - **The program.** `claude` is found as the person's terminal finds it (on the daemon's `PATH`,
//!   else their login shell's: [`crate::facts::installed`]) and runs with that `PATH`. The worker
//!   opens it the way it opens any `claude` (`Worker::open`): with the hook relay, the mod and
//!   Slopty's tools when it has a server. Nothing is signed in and no credential is read: a Claude
//!   Code that is not signed in says so in its own terminal.
//! - **Named before it speaks.** The session id is chosen here and given with `--session-id`, so
//!   the thread ([`slopty_agent::observed::thread_of`]) is begun as soon as the terminal opens and
//!   the start is answered with it; the hooks and the transcript then fill it in. The first message
//!   is Claude Code's own initial prompt on its command line, after `--`, never keys typed into its
//!   TUI, and is marked with the start's intent once the transcript shows it.
//! - **The TUI is the agent.** The terminal is the person's: they can type into it at any time, and
//!   closing it ends the agent.
//! - **Once.** A start is acted on once per intent id: starts are taken one at a time, and a repeat
//!   gets the first outcome back.
//! - **Resumed.** A start whose arguments are `--resume <id>` takes an exited thread's session up
//!   again ([`slopty_agent::resume::resumed`]): Claude Code goes on under the same id, so the
//!   observer begins the same thread in the new terminal and reads it again from the transcript. It
//!   is refused while a Claude Code runs that session already: a session has one writer. No other
//!   argument is taken from a client.

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;

use slopty_proto::thread::wire::{Outcome, Start};
use slopty_proto::thread::{AgentId, Drive, IntentId};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use super::Driver;
use crate::thread::Host;
use crate::thread::terminals::Terminals;

/// What is asked of the starts.
#[derive(Debug)]
struct Ask {
    id: IntentId,
    start: Start,
    reply: oneshot::Sender<Outcome>,
}

/// Starts Claude Code threads. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Starter(mpsc::UnboundedSender<Ask>);

/// Where a [`Starter`]'s asks wait for [`spawn`].
#[derive(Debug)]
pub struct Asks(mpsc::UnboundedReceiver<Ask>);

impl Starter {
    /// A handle, and the asks [`spawn`] takes from it.
    #[must_use]
    pub fn channel() -> (Self, Asks) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self(tx), Asks(rx))
    }

    /// Start the thread of intent `id` as `start` says, once.
    pub async fn start(&self, id: IntentId, start: Start) -> Outcome {
        let (reply, outcome) = oneshot::channel();
        if self.0.send(Ask { id, start, reply }).is_err() {
            return refused("Claude Code threads are not started here");
        }
        outcome.await.unwrap_or_else(|_| refused("the Claude Code starts stopped"))
    }
}

/// Take the asks of a [`Starter`]: each opens the person's `claude` in one of `terminals` and has
/// `driver` observe it into `host`. `claude` is looked for on `path` alone when it is given.
pub fn spawn(
    host: Host,
    driver: Driver,
    terminals: Arc<dyn Terminals>,
    path: Option<OsString>,
    Asks(mut asks): Asks,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(Ask { id, start, reply }) = asks.recv().await {
            let outcome = if let Some(first) = host.started(id) {
                first
            } else {
                let outcome = begin(&host, &driver, terminals.as_ref(), path.clone(), id, &start);
                host.record_start(id, outcome.await)
            };
            let _gone = reply.send(outcome);
        }
    })
}

async fn begin(
    host: &Host,
    driver: &Driver,
    terminals: &dyn Terminals,
    path: Option<OsString>,
    id: IntentId,
    start: &Start,
) -> Outcome {
    if !start.agent.is(AgentId::CLAUDE_CODE) {
        return refused(&format!("{} is not Claude Code", start.agent.0));
    }
    if start.drive.as_ref().is_some_and(|d| !d.is(Drive::OBSERVED)) {
        return refused("Claude Code runs in its own terminal, observed");
    }
    let (model, prompt) = (start.model.as_deref(), start.prompt.as_deref());
    let (args, native) = match start.args.as_slice() {
        [] => slopty_agent::resume::started(model, prompt),
        [flag, session] if flag == slopty_agent::resume::RESUME_FLAG => {
            let Some(args) = slopty_agent::resume::resumed(session, model, prompt) else {
                return refused(&format!("{session} is no Claude Code session"));
            };
            if runs(host, session) {
                return refused("Claude Code runs this session already");
            }
            (args, session.clone())
        }
        _ => return refused("Claude Code takes no arguments from a start but --resume <id>"),
    };
    if !Path::new(&start.cwd).is_dir() {
        return refused(&format!("There is no folder {} here", start.cwd));
    }
    let Some(claude) = crate::facts::installed("claude", path).await else {
        return refused("Claude Code is not installed");
    };
    let command =
        std::iter::once(claude.program.to_string_lossy().into_owned()).chain(args).collect();
    let env = vec![("PATH".to_owned(), claude.path.to_string_lossy().into_owned())];
    let terminal = match terminals.open(command, start.cwd.clone(), env).await {
        Ok(terminal) => terminal,
        Err(e) => return refused(&format!("Its terminal did not open: {e}")),
    };
    tracing::info!(%id, %terminal, native, cwd = start.cwd, "started Claude Code");
    let Some(thread) = driver.begin(terminal, native, start.cwd.clone()).await else {
        return refused("Claude Code started, but nothing observes it here");
    };
    if let Some(prompt) = start.prompt.as_deref().filter(|p| !p.trim().is_empty()) {
        host.typed(thread, id, prompt);
    }
    Outcome::Started { thread }
}

/// Whether a live Claude Code holds session `native`'s thread here.
fn runs(host: &Host, native: &str) -> bool {
    let thread = slopty_agent::observed::thread_of(native);
    host.state(thread)
        .is_some_and(|(state, _)| state.status.liveness == slopty_proto::thread::Liveness::Live)
}

fn refused(reason: &str) -> Outcome {
    Outcome::Refused { reason: reason.to_owned() }
}
