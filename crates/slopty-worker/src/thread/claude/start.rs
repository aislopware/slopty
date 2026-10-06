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
//!   is refused while a Claude Code runs that session already: one this worker observes, or one
//!   Claude Code's own registry of its live sessions lists at that moment
//!   ([`slopty_agent::roster`]), as the person's `claude` in another terminal app is. A session has
//!   one writer. No other argument is taken from a client.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use slopty_core::SessionId;
use slopty_proto::thread::wire::{Outcome, Start};
use slopty_proto::thread::{AgentId, Drive, Fork, IntentId, ThreadId, TurnId};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use super::Driver;
use crate::thread::terminals::Terminals;
use crate::thread::{Host, Seated};

/// What is asked of the starts.
#[derive(Debug)]
struct Ask {
    id: IntentId,
    what: What,
    reply: oneshot::Sender<Outcome>,
}

/// What a start begins.
#[derive(Debug)]
enum What {
    /// A thread as the start says, at a server task's seat when it has one.
    Start(Box<Start>, Option<Box<Seated>>),
    /// A thread branched off this one through this turn, or all of it.
    Fork { from: ThreadId, after: Option<TurnId> },
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
        if self.0.send(Ask { id, what: What::Start(Box::new(start), None), reply }).is_err() {
            return refused("Claude Code threads are not started here");
        }
        outcome.await.unwrap_or_else(|_| refused("the Claude Code starts stopped"))
    }

    /// Start a server task's thread as `start` says, once per seat: Claude Code runs in a
    /// terminal opened under the seat, so the worker gives it the seat's hooks, tools and
    /// variables as it does any terminal's, and the role as its system prompt's addition
    /// (`--append-system-prompt`).
    pub async fn start_seated(&self, start: Start, seated: Seated) -> Outcome {
        self.start_at(seated.intent(), start, seated).await
    }

    /// Start Claude Code for intent `id` as `start` says, once, at `seated`'s seat: a seated
    /// thread's session taken up again (`--resume`) runs where it was started.
    pub async fn start_at(&self, id: IntentId, start: Start, seated: Seated) -> Outcome {
        let (reply, outcome) = oneshot::channel();
        if self
            .0
            .send(Ask { id, what: What::Start(Box::new(start), Some(Box::new(seated))), reply })
            .is_err()
        {
            return refused("Claude Code threads are not started here");
        }
        outcome.await.unwrap_or_else(|_| refused("the Claude Code starts stopped"))
    }

    /// Branch a new thread off the whole of `from`'s conversation for intent `id`, once: Claude
    /// Code resumes it into a new one (`--fork-session`) in a terminal of its own.
    pub async fn fork(&self, from: ThreadId, id: IntentId, after: Option<TurnId>) -> Outcome {
        let (reply, outcome) = oneshot::channel();
        if self.0.send(Ask { id, what: What::Fork { from, after }, reply }).is_err() {
            return refused("Claude Code threads are not started here");
        }
        outcome.await.unwrap_or_else(|_| refused("the Claude Code starts stopped"))
    }
}

/// Take the asks of a [`Starter`]: each opens the person's `claude` in one of `terminals`.
///
/// `driver` observes it into `host`. `claude` is looked for on `path` alone when it is given.
/// `registry` is Claude Code's registry of its live sessions
/// ([`slopty_agent::roster::sessions_dir`]), read again before each resume.
pub fn spawn(
    host: Host,
    driver: Driver,
    terminals: Arc<dyn Terminals>,
    path: Option<OsString>,
    registry: PathBuf,
    Asks(mut asks): Asks,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(Ask { id, what, reply }) = asks.recv().await {
            let outcome = if let Some(first) = host.started(id) {
                first
            } else {
                let claude = Claude {
                    driver: &driver,
                    terminals: terminals.as_ref(),
                    path: &path,
                    registry: &registry,
                };
                let outcome = match &what {
                    What::Start(start, seated) => {
                        begin(&host, &claude, id, start, seated.as_deref()).await
                    }
                    What::Fork { from, after } => fork(&host, &claude, *from, *after).await,
                };
                host.record_start(id, outcome)
            };
            let _gone = reply.send(outcome);
        }
    })
}

/// What opening the person's `claude` takes: the driver that observes it, the terminals it
/// runs in, and the `PATH` it is looked for on alone when there is one.
struct Claude<'a> {
    driver: &'a Driver,
    terminals: &'a dyn Terminals,
    path: &'a Option<OsString>,
    /// Claude Code's registry of its live sessions.
    registry: &'a Path,
}

async fn begin(
    host: &Host,
    claude: &Claude<'_>,
    id: IntentId,
    start: &Start,
    seated: Option<&Seated>,
) -> Outcome {
    if !start.agent.is(AgentId::CLAUDE_CODE) {
        return refused(&format!("{} is not Claude Code", start.agent.0));
    }
    if start.drive.as_ref().is_some_and(|d| !d.is(Drive::OBSERVED)) {
        return refused("Claude Code runs in its own terminal, observed");
    }
    let resume = match resumed_of(&start.args) {
        Ok(resume) => resume,
        Err(why) => return refused(&why),
    };
    if let Err(why) = crate::thread::attach::check(&start.attachments) {
        return refused(&why);
    }
    // Claude Code's TUI takes a file by its path, a picture's too, as it would one pasted.
    let prompt = slopty_agent::attach::with_paths(
        start.prompt.as_deref().unwrap_or_default(),
        start.attachments.iter().map(String::as_str),
    );
    let prompt = Some(prompt).filter(|p| !p.trim().is_empty());
    let (model, prompt) = (start.model.as_deref(), prompt.as_deref());
    let (mut args, native) = match resume {
        None => slopty_agent::resume::started(model, prompt),
        Some(session) => {
            let Some(args) = slopty_agent::resume::resumed(session, model, prompt) else {
                return refused(&format!("{session} is no Claude Code session"));
            };
            if runs(host, session) {
                return refused("Claude Code runs this session already");
            }
            if held(claude.registry, session).await {
                return refused(HELD_ELSEWHERE);
            }
            (args, session.to_owned())
        }
    };
    if let Some(mode) = start.mode.as_deref() {
        if !slopty_agent::resume::startable_mode(mode) {
            return refused(&format!("Claude Code starts in no mode {mode}"));
        }
        args.splice(0..0, [slopty_agent::resume::PERMISSION_MODE.to_owned(), mode.to_owned()]);
    }
    if let Some(effort) = start.effort.as_deref() {
        if !slopty_agent::resume::startable_effort(effort) {
            return refused(&format!("Claude Code takes no effort {effort}"));
        }
        args.splice(0..0, [slopty_agent::resume::EFFORT_FLAG.to_owned(), effort.to_owned()]);
    }
    if !Path::new(&start.cwd).is_dir() {
        return refused(&format!("There is no folder {} here", start.cwd));
    }
    if let Some(role) = seated.and_then(|s| s.role.as_deref()).filter(|r| !r.trim().is_empty()) {
        args.insert(0, format!("--append-system-prompt={role}"));
    }
    let at = seated.map(|seated| (seated.seat, host.env_of(seated)));
    let thread = match open(claude, args, native, &start.cwd, at).await {
        Ok(thread) => thread,
        Err(why) => return refused(&why),
    };
    if let Some(seated) = seated {
        host.seated(thread, seated);
    }
    if let Some(prompt) = prompt {
        host.typed(thread, id, prompt);
    }
    Outcome::Started { thread }
}

/// The session a client's start asks Claude Code to take up again in [`Start::args`]
/// (`--resume <id>`), when it asks; why not, in words, for any other argument. The mode, the
/// effort and the model are [`Start`]'s own.
fn resumed_of(args: &[String]) -> Result<Option<&str>, String> {
    match args {
        [] => Ok(None),
        [flag, session] if flag == slopty_agent::resume::RESUME_FLAG => Ok(Some(session)),
        _ => Err("Claude Code takes no arguments from a start but --resume <id>".to_owned()),
    }
}

/// Open Claude Code on a new conversation branched off the whole of thread `from`'s, in a
/// terminal of its own, and observe it as a thread that says where it came from.
async fn fork(host: &Host, claude: &Claude<'_>, from: ThreadId, after: Option<TurnId>) -> Outcome {
    let Some((state, _)) = host.state(from).filter(|(s, _)| s.meta.agent.is(AgentId::CLAUDE_CODE))
    else {
        return refused("There is no such Claude Code thread here");
    };
    let turn = match crate::thread::fork::whole(&state, after, "Claude Code") {
        Ok(turn) => turn,
        Err(why) => return refused(&why),
    };
    // Claude Code writes the conversation with its first message: before it there is none.
    if turn.is_none() {
        return refused("There is nothing to fork yet");
    }
    let Some((args, native)) = slopty_agent::resume::forked(&state.meta.native) else {
        return refused(&format!("{} is no Claude Code session", state.meta.native));
    };
    let cwd = state.meta.cwd.clone();
    let thread = match open(claude, args, native, &cwd, None).await {
        Ok(thread) => thread,
        Err(why) => return refused(&why),
    };
    host.forked(thread, Fork { thread: from, turn });
    Outcome::Started { thread }
}

/// Open the person's `claude` with `args` in folder `cwd`, in a terminal of its own, and begin
/// the thread of its conversation `native`; why not, in words. `at` a server task's seat, the
/// terminal opens under the seat's id with every variable of the seat.
async fn open(
    Claude { driver, terminals, path, .. }: &Claude<'_>,
    args: Vec<String>,
    native: String,
    cwd: &str,
    at: Option<(SessionId, Vec<(String, String)>)>,
) -> Result<ThreadId, String> {
    let installed = crate::facts::installed("claude", (*path).clone())
        .await
        .ok_or_else(|| "Claude Code is not installed".to_owned())?;
    let command =
        std::iter::once(installed.program.to_string_lossy().into_owned()).chain(args).collect();
    let mut env = vec![("PATH".to_owned(), installed.path.to_string_lossy().into_owned())];
    let opened = match at {
        Some((seat, seat_env)) => {
            env.extend(seat_env);
            terminals.open_at(seat, command, cwd.to_owned(), env).await
        }
        None => terminals.open(command, cwd.to_owned(), env).await,
    };
    let terminal = opened.map_err(|e| format!("Its terminal did not open: {e}"))?;
    tracing::info!(%terminal, native, cwd, "started Claude Code");
    driver
        .begin(terminal, native, cwd.to_owned())
        .await
        .ok_or_else(|| "Claude Code started, but nothing observes it here".to_owned())
}

/// Whether a live Claude Code holds session `native`'s thread here.
fn runs(host: &Host, native: &str) -> bool {
    let thread = slopty_agent::observed::thread_of(native);
    host.state(thread)
        .is_some_and(|(state, _)| state.status.liveness == slopty_proto::thread::Liveness::Live)
}

/// Why a resume of a session a Claude Code this worker does not observe holds is refused.
pub const HELD_ELSEWHERE: &str = "Claude Code runs this session in another terminal";

/// Whether a live Claude Code holds session `native`, as `registry` says it now.
async fn held(registry: &Path, native: &str) -> bool {
    let (registry, native) = (registry.to_owned(), native.to_owned());
    tokio::task::spawn_blocking(move || {
        let listed = slopty_agent::roster::registered(&registry, crate::ports::alive);
        slopty_agent::roster::holder(&listed, &native).is_some()
    })
    .await
    .unwrap_or(false)
}

fn refused(reason: &str) -> Outcome {
    Outcome::Refused { reason: reason.to_owned() }
}
