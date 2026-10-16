//! Claude Code on the worker, driven the way Claude's own desktop app drives it: the IO half of
//! the adapter (`slopty_agent::claude::driven` is the codec).
//!
//! The person's own `claude` runs as `claude -p` speaking stream-json both ways with the control
//! protocol ([`slopty_agent::claude::args`]), in ptyd's custody ([`custody`]), one task per
//! running Claude Code carrying its lines (`driven`).
//!
//! - **The program.** `claude` is found as the person's terminal finds it (on the daemon's `PATH`,
//!   else their login shell's: [`crate::facts::installed`]) and runs with that `PATH` and the
//!   variables the desktop app gives it ([`slopty_agent::claude::ENV`]). A managed launcher takes
//!   the same arguments and hands them on. Nothing is signed in and no credential is read.
//! - **Named before it speaks.** A new thread's session id is its start's intent id, given with
//!   `--session-id`, so the thread ([`slopty_agent::claude::thread_of`]) is answered at once.
//! - **Custody.** Claude Code outlives the worker: the next worker attaches its session again,
//!   reads the thread again from the session's transcript, then what Claude Code wrote meanwhile,
//!   and asks it again for what it still waits on (`initialize`).
//! - **Goes on by message.** A thread whose Claude Code ended (it exited, failed, or was ended) is
//!   exited and resumable: its next message starts Claude Code again on the session (`--resume`),
//!   the thread read again from the transcript first, so the session has one writer.
//! - **Once.** A start is acted on once per intent id.
//! - **Beside the conversation** ([`settings`]): a `!` command runs in the chat's folder and goes
//!   to Claude Code as the person's message, a permission rule is written into its scope's file,
//!   and the person's status line command runs as the TUI runs it.
//! - **A fork at a turn** keeps the conversation through that turn: Claude Code resumes the session
//!   at that turn's last record into a new one (`--resume-session-at`).

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use slopty_agent::claude::driven::{ARGS_FACT, CUSTODY_FACT, Driven, Ran};
use slopty_agent::claude::history::Out;
use slopty_agent::claude::{self as agent, Session};
use slopty_core::{SessionId, WallMs};
use slopty_proto::thread::wire::{Expanded, Intent, Outcome, Start};
use slopty_proto::thread::{
    Action, AgentId, Answerer, AskId, ContentRef, Delivery, Drive, Fork, IntentId, ItemId,
    Liveness, Phase, Request, RequestState, RuleChange, SurfaceEvent, ThreadId, ThreadMeta,
    ThreadState, TurnId,
};
use slopty_proto::visual::{AppAnswer, AppAsk};
use slopty_pty::SpawnSpec;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use self::custody::Custody;
use self::driven::{Task, read_again};
use super::{Host, Seated};
use crate::facts::Installed;

pub mod custody;
mod driven;
pub mod settings;

/// Where a thread's task says its Claude Code has gone, or the worker has, with the asks it did
/// not take.
type Ended = mpsc::UnboundedSender<(ThreadId, Vec<ThreadAsk>)>;

/// Why a thread whose Claude Code an earlier worker left is no longer running.
const LOST: &str = "Claude Code ended while the worker was away";

/// What a client asks of the driven Claude Code threads.
#[derive(Debug)]
enum Ask {
    Start {
        id: IntentId,
        start: Box<Start>,
        seated: Option<Box<Seated>>,
        reply: oneshot::Sender<Outcome>,
    },
    Fork {
        thread: ThreadId,
        id: IntentId,
        after: Option<TurnId>,
        reply: oneshot::Sender<Outcome>,
    },
    Thread {
        thread: ThreadId,
        ask: ThreadAsk,
    },
    /// The thread's agent ends, its session kept.
    Close {
        thread: ThreadId,
        done: oneshot::Sender<()>,
    },
    /// The whole of a clipped text or picture of a thread.
    Expand {
        thread: ThreadId,
        content: ContentRef,
        reply: oneshot::Sender<Expanded>,
    },
    /// A page's ask of its MCP App, through the thread's Claude Code.
    App {
        thread: ThreadId,
        ask: AppAsk,
        reply: oneshot::Sender<AppAnswer>,
    },
}

/// What a client asks of one thread.
#[derive(Debug)]
enum ThreadAsk {
    Send {
        text: String,
        attachments: Vec<String>,
        intent: IntentId,
        delivery: Delivery,
    },
    /// Slopty's reports for the agent, as a message of their own.
    Report {
        text: String,
    },
    Withdraw {
        intent: IntentId,
    },
    Edit {
        intent: IntentId,
        text: String,
        attachments: Vec<String>,
    },
    Promote {
        intent: IntentId,
    },
    Move {
        intent: IntentId,
        before: Option<IntentId>,
    },
    Interrupt,
    /// Stop one background task: a command, a monitor, a subagent's run.
    StopTask {
        task: String,
    },
    /// Move call `item`, or every blocking one, to the background.
    Background {
        item: Option<String>,
    },
    /// Run `!` command `command` for intent `intent`, then hand it over.
    Shell {
        command: String,
        intent: IntentId,
    },
    /// What `!` command `intent` did, for Claude Code; answered unless `quiet`.
    ShellDone {
        ran: Ran,
        intent: IntentId,
        quiet: bool,
    },
    /// Put the files back to before turn `turn`.
    Rewind {
        turn: TurnId,
    },
    /// Write a permission rule into its scope's file, then list the rules again.
    Rule {
        change: RuleChange,
    },
    /// The person acted in a mod's surface.
    Surface {
        surface: String,
        element: String,
        event: SurfaceEvent,
    },
    /// A page's ask of its MCP App.
    App {
        ask: AppAsk,
        reply: oneshot::Sender<AppAnswer>,
    },
    Answer {
        ask: AskId,
        choice: String,
        message: Option<String>,
        by: Answerer,
    },
    SetModel {
        model: String,
    },
    SetMode {
        mode: String,
    },
    SetEffort {
        effort: String,
    },
    SetOption {
        option: String,
        choice: String,
    },
    /// End Claude Code now, its session kept.
    Close,
}

/// Where clients' asks go to the driven Claude Code threads. Cheap to clone.
///
/// A stop and an end go straight to the running thread's task ([`Running`]): the served loop
/// may be busy starting another thread (finding the install takes up to seconds), and a stop
/// must not wait behind it.
#[derive(Clone, Debug)]
pub struct Claude(mpsc::UnboundedSender<Ask>, Running);

/// Where a [`Claude`]'s asks wait for [`spawn`], and where its own go back in.
#[derive(Debug)]
pub struct Asks(mpsc::UnboundedReceiver<Ask>, mpsc::UnboundedSender<Ask>, Running);

/// The task of each thread's Claude Code while it runs, the session's one writer: shared by the
/// served loop, which starts and ends them, and the handles, which stop them at once.
type Running = Arc<parking_lot::Mutex<HashMap<ThreadId, mpsc::UnboundedSender<ThreadAsk>>>>;

/// Where the driven Claude Codes run and keep what they keep.
#[derive(Clone, Debug)]
pub struct Place {
    /// The worker's data directory: each Claude Code's standard error goes under it.
    pub data_dir: PathBuf,
    /// Where ptyd listens.
    pub ptyd: PathBuf,
    /// Where `claude` is looked for alone, when given; else as the person's terminal finds it.
    pub path: Option<OsString>,
    /// The home Claude Code keeps its sessions under (`~/.claude`).
    pub home: PathBuf,
}

impl Claude {
    /// A handle, and the asks [`spawn`] takes from it.
    #[must_use]
    pub fn channel() -> (Self, Asks) {
        let (tx, rx) = mpsc::unbounded_channel();
        let running = Running::default();
        (Self(tx.clone(), Arc::clone(&running)), Asks(rx, tx, running))
    }

    /// `ask` straight to `thread`'s running task, past the served loop; `ask` back when no task
    /// runs it.
    fn at_once(&self, thread: ThreadId, ask: ThreadAsk) -> Result<(), ThreadAsk> {
        let task = self.1.lock().get(&thread).cloned();
        match task {
            Some(task) => task.send(ask).map_err(|mpsc::error::SendError(ask)| ask),
            None => Err(ask),
        }
    }

    async fn asked(&self, ask: impl FnOnce(oneshot::Sender<Outcome>) -> Ask) -> Outcome {
        let (reply, outcome) = oneshot::channel();
        if self.0.send(ask(reply)).is_err() {
            return refused("Claude Code threads are not served here");
        }
        outcome.await.unwrap_or_else(|_| refused("the Claude Code threads stopped"))
    }

    /// Start a Claude Code thread for intent `id` once, as `start` says: a repeat of the id gets
    /// the first outcome.
    pub async fn start(&self, id: IntentId, start: Box<Start>) -> Outcome {
        self.asked(|reply| Ask::Start { id, start, seated: None, reply }).await
    }

    /// Start a seated thread (an agent another started) as `start` says, once per seat: Claude
    /// Code runs with the seat's variables and Slopty's tools, and the system text as its system
    /// prompt's addition, kept with the thread's flags for every later run.
    pub async fn start_seated(&self, start: Box<Start>, seated: Seated) -> Outcome {
        let (id, seated) = (seated.intent(), Some(Box::new(seated)));
        self.asked(|reply| Ask::Start { id, start, seated, reply }).await
    }

    /// Branch a new thread off the whole of `thread`'s session for intent `id`, once: Claude
    /// Code resumes it into a new session (`--fork-session`).
    pub async fn fork(&self, thread: ThreadId, id: IntentId, after: Option<TurnId>) -> Outcome {
        self.asked(|reply| Ask::Fork { thread, id, after, reply }).await
    }

    /// End thread `thread`'s Claude Code, its session kept to take up again.
    pub async fn close(&self, thread: ThreadId) {
        if self.at_once(thread, ThreadAsk::Close).is_ok() {
            return;
        }
        let (done, closed) = oneshot::channel();
        if self.0.send(Ask::Close { thread, done }).is_ok() {
            let _done = closed.await;
        }
    }

    /// The whole of `content`, clipped in `thread`.
    pub async fn expand(&self, thread: ThreadId, content: ContentRef) -> Expanded {
        let (reply, body) = oneshot::channel();
        if self.0.send(Ask::Expand { thread, content, reply }).is_err() {
            return Expanded::Gone;
        }
        body.await.unwrap_or(Expanded::Gone)
    }

    /// What a page shown in `thread` asks of its MCP App (`resources/read`, `tools/call`),
    /// through the session's Claude Code (`mcp_read_resource`, `mcp_call`): refused while it does
    /// not run.
    pub async fn app(&self, thread: ThreadId, ask: AppAsk) -> AppAnswer {
        let (reply, answer) = oneshot::channel();
        if self.0.send(Ask::App { thread, ask, reply }).is_err() {
            return AppAnswer::Refused("Claude Code threads are not served here".to_owned());
        }
        answer.await.unwrap_or_else(|_| AppAnswer::Refused("Claude Code did not answer".to_owned()))
    }

    /// Hand Slopty's reports, worded `text`, to `thread`'s Claude Code: a message of their own,
    /// which takes the thread up again when its Claude Code ended.
    pub fn report(&self, thread: ThreadId, text: String) -> Outcome {
        let ask = Ask::Thread { thread, ask: ThreadAsk::Report { text } };
        if self.0.send(ask).is_err() {
            return refused("Claude Code threads are not served here");
        }
        Outcome::Done
    }

    /// What comes of intent `id` on the driven Claude Code thread `state`, from `by` when it
    /// answers: done when it went to Claude Code (or to one started again for it), and why not
    /// when it did not. Meant to run once per id, as [`Host::intent`] runs its decision.
    pub fn decide(&self, state: &ThreadState, id: IntentId, ask: &Intent, by: Answerer) -> Outcome {
        let mut thread = state.meta.id;
        let live = state.status.liveness == Liveness::Live;
        let goes_on = matches!(state.status.liveness, Liveness::Exited { resumable: true });
        let subagent = state.meta.origin == ThreadMeta::SUBAGENT;
        let asked = match ask {
            Intent::Send { text, attachments, .. }
                if text.trim().is_empty() && attachments.is_empty() =>
            {
                return refused("There is nothing to send");
            }
            Intent::Send { .. } if subagent => return refused("A subagent takes no message"),
            Intent::Send { text, attachments, delivery } => {
                if let Err(why) = super::attach::check(attachments) {
                    return refused(&why);
                }
                let (text, attachments, delivery) = (text.clone(), attachments.clone(), *delivery);
                ThreadAsk::Send { text, attachments, intent: id, delivery }
            }
            Intent::Withdraw { pending }
            | Intent::Edit { pending, .. }
            | Intent::Promote { pending }
            | Intent::Move { pending, .. }
                if !state.pending.iter().any(|p| p.intent == *pending) =>
            {
                return refused("That message is not waiting");
            }
            Intent::Withdraw { pending } => ThreadAsk::Withdraw { intent: *pending },
            Intent::Move { pending, before } => {
                ThreadAsk::Move { intent: *pending, before: *before }
            }
            Intent::Edit { pending, text, attachments } => {
                if let Err(why) = super::attach::check(attachments) {
                    return refused(&why);
                }
                let (text, attachments) = (text.clone(), attachments.clone());
                ThreadAsk::Edit { intent: *pending, text, attachments }
            }
            Intent::Promote { pending } => ThreadAsk::Promote { intent: *pending },
            Intent::Interrupt => {
                // An agent that is gone wakes only by a message. One at rest has nothing left to
                // stop, so a second stop, or one that crossed the turn's end, is done.
                if !live {
                    return refused("Claude Code is not running");
                }
                let phase = &state.status.phase;
                if !matches!(phase, Phase::Working | Phase::NeedsYou | Phase::Waiting) {
                    return Outcome::Done;
                }
                if subagent {
                    // The subagent's run, stopped in the session that runs it.
                    let Some(parent) = state.meta.parent.as_ref() else {
                        return refused("This subagent's session is not known");
                    };
                    thread = parent.thread;
                    ThreadAsk::StopTask { task: state.meta.native.clone() }
                } else {
                    ThreadAsk::Interrupt
                }
            }
            Intent::StopTask { task } => {
                if !state.tasks.iter().any(|t| t.id == *task && t.is_live()) {
                    return refused(&format!("There is no running task {task}"));
                }
                if let Some(parent) = state.meta.parent.as_ref().filter(|_| subagent) {
                    thread = parent.thread;
                }
                ThreadAsk::StopTask { task: task.clone() }
            }
            Intent::Background { item } => {
                if let Some(item) = item
                    && !state.items.iter().any(|i| i.id == *item)
                {
                    return refused(&format!("There is no call {}", item.0));
                }
                ThreadAsk::Background { item: item.as_ref().map(|i| i.0.clone()) }
            }
            Intent::Shell { .. } if subagent => return refused("A subagent takes no command"),
            Intent::Shell { command } => {
                if command.trim().is_empty() {
                    return refused("There is no command to run");
                }
                ThreadAsk::Shell { command: command.clone(), intent: id }
            }
            Intent::Rewind { turn } => {
                if !state.turns.iter().any(|t| t.id == *turn && t.input.is_some()) {
                    return refused(&format!("There is no turn {} to go back to", turn.0));
                }
                ThreadAsk::Rewind { turn: *turn }
            }
            Intent::Rule(change) => {
                if let Err(why) = settings::checked(change) {
                    return refused(&why);
                }
                ThreadAsk::Rule { change: change.clone() }
            }
            Intent::Surface { surface, element, event } => {
                if !state.surfaces.iter().any(|s| s.id == *surface) {
                    return refused(&format!("There is no surface {surface}"));
                }
                ThreadAsk::Surface {
                    surface: surface.clone(),
                    element: element.clone(),
                    event: event.clone(),
                }
            }
            Intent::Answer { ask, choice, message } => {
                let Some(request) = state.requests.iter().find(|r| r.id == *ask) else {
                    return refused(&format!("There is no request {}", ask.0));
                };
                match &request.state {
                    RequestState::Open => {}
                    RequestState::Answered { .. } => return Outcome::Done,
                    RequestState::Released | RequestState::Withdrawn => {
                        return refused("Claude Code no longer asks this");
                    }
                }
                let offered = request.options.iter().any(|o| o.id == *choice);
                // A dialog or a form also takes its result as a JSON object, or is put away.
                let form = request.kind == Request::ELICITATION
                    || request.kind.starts_with(Request::DIALOG);
                let filled =
                    serde_json::from_str::<serde_json::Value>(choice).is_ok_and(|v| v.is_object());
                let put_away = [Request::CANCEL, Request::DECLINE].contains(&choice.as_str());
                if !offered && request.questions.is_empty() && !(form && (filled || put_away)) {
                    return refused(&format!("There is no answer {choice}"));
                }
                ThreadAsk::Answer {
                    ask: ask.clone(),
                    choice: choice.clone(),
                    message: message.clone(),
                    by,
                }
            }
            Intent::SetModel { model } => {
                if !state.meta.models.iter().any(|m| m.id == *model) {
                    return refused(&format!("There is no model {model} here"));
                }
                ThreadAsk::SetModel { model: model.clone() }
            }
            Intent::SetEffort { effort } => {
                if !state.meta.efforts.iter().any(|e| e.id == *effort) {
                    return refused(&format!("The model has no effort {effort}"));
                }
                ThreadAsk::SetEffort { effort: effort.clone() }
            }
            Intent::SetMode { mode } => {
                if !state.meta.modes.iter().any(|m| m.id == *mode) {
                    return refused(&format!("Claude Code has no mode {mode}"));
                }
                ThreadAsk::SetMode { mode: mode.clone() }
            }
            Intent::SetOption { option, choice } => {
                let offered = state
                    .meta
                    .models
                    .iter()
                    .flat_map(|m| &m.options)
                    .any(|o| o.id == *option && o.choices.iter().any(|c| c.id == *choice));
                if !offered {
                    return refused(&format!("The model offers no {option} {choice}"));
                }
                ThreadAsk::SetOption { option: option.clone(), choice: choice.clone() }
            }
            Intent::End => {
                if !live {
                    return refused("Claude Code is not running");
                }
                ThreadAsk::Close
            }
            other => return other.unsupported(),
        };
        // What is held is the thread's record, answered whether Claude Code runs or not; a
        // message or a setting takes up a thread whose Claude Code ended.
        let held = matches!(
            asked,
            ThreadAsk::Withdraw { .. }
                | ThreadAsk::Edit { .. }
                | ThreadAsk::Promote { .. }
                | ThreadAsk::Move { .. }
                | ThreadAsk::Rule { .. }
        );
        let takes_up = matches!(
            asked,
            ThreadAsk::Send { .. }
                | ThreadAsk::Shell { .. }
                | ThreadAsk::SetModel { .. }
                | ThreadAsk::SetMode { .. }
                | ThreadAsk::SetEffort { .. }
        ) && goes_on;
        if !live && !held && !takes_up {
            return refused("Claude Code is not running");
        }
        let asked = if matches!(
            asked,
            ThreadAsk::Interrupt | ThreadAsk::StopTask { .. } | ThreadAsk::Close
        ) {
            match self.at_once(thread, asked) {
                Ok(()) => return Outcome::Done,
                Err(asked) => asked,
            }
        } else {
            asked
        };
        if self.0.send(Ask::Thread { thread, ask: asked }).is_err() {
            return refused("Claude Code threads are not served here");
        }
        Outcome::Done
    }
}

/// Whether `state` is a Claude Code thread, which this worker drives.
#[must_use]
pub fn is_claude(state: &ThreadState) -> bool {
    state.meta.agent.is(AgentId::CLAUDE_CODE)
}

/// Serve the driven Claude Code threads in `host`, run where `place` says, taking the asks of
/// their [`Claude`].
///
/// First every thread an earlier worker left with Claude Code in ptyd's custody is taken up
/// again; one whose Claude Code ptyd no longer holds is left exited and resumable.
pub fn spawn(host: Host, place: Place, Asks(mut asks, again, running): Asks) -> JoinHandle<()> {
    tokio::spawn(async move {
        let (ended_tx, mut ended) = mpsc::unbounded_channel();
        let custody = Custody::new(place.ptyd.clone());
        let mut served =
            Served { host, custody, place, launcher: None, running, ended: ended_tx, again };
        served.take_up().await;
        loop {
            tokio::select! {
                ask = asks.recv() => match ask {
                    Some(Ask::Start { id, start, seated, reply }) => {
                        let outcome =
                            Box::pin(served.begin(id, &start, seated.map(|s| *s))).await;
                        let _gone = reply.send(outcome);
                    }
                    Some(Ask::Fork { thread, id, after, reply }) => {
                        let outcome = served.fork(thread, id, after).await;
                        let _gone = reply.send(outcome);
                    }
                    Some(Ask::Thread { thread, ask }) => served.route(thread, vec![ask]).await,
                    Some(Ask::Close { thread, done }) => {
                        let task = served.running.lock().get(&thread).cloned();
                        if let Some(task) = task {
                            let _gone = task.send(ThreadAsk::Close);
                        }
                        let _gone = done.send(());
                    }
                    Some(Ask::Expand { thread, content, reply }) => served.expand(thread, content, reply),
                    Some(Ask::App { thread, ask, reply }) => served.app(thread, ask, reply),
                    None => return,
                },
                Some((thread, left)) = ended.recv() => {
                    {
                        let mut running = served.running.lock();
                        if running.get(&thread).is_some_and(mpsc::UnboundedSender::is_closed) {
                            running.remove(&thread);
                        }
                    }
                    if !left.is_empty() {
                        served.route(thread, left).await;
                    }
                }
            }
        }
    })
}

/// The driven Claude Code threads, as [`spawn`] serves them.
struct Served {
    host: Host,
    custody: Custody,
    place: Place,
    /// Found once it is found; looked for again until then.
    launcher: Option<Installed>,
    /// The task of each thread's Claude Code while it runs: the session's one writer.
    running: Running,
    /// Told by a task when its Claude Code has gone.
    ended: Ended,
    /// Where what the served threads do on their own comes back in (a `!` command that ended).
    again: mpsc::UnboundedSender<Ask>,
}

impl Served {
    async fn launcher(&mut self) -> Result<Installed, String> {
        if let Some(launcher) = &self.launcher {
            return Ok(launcher.clone());
        }
        let found = crate::facts::installed("claude", self.place.path.clone())
            .await
            .ok_or_else(|| "Claude Code is not installed".to_owned())?;
        self.launcher = Some(found.clone());
        Ok(found)
    }

    /// Take up again every thread whose Claude Code an earlier worker left in ptyd's custody;
    /// let go of what ptyd holds that no thread names; leave exited any thread left running
    /// whose Claude Code is gone.
    async fn take_up(&self) {
        let held = match self.custody.sessions().await {
            Ok(held) => held,
            Err(why) => {
                tracing::warn!("the Claude Codes in ptyd's keeping are not known: {why}");
                Vec::new()
            }
        };
        let threads = self.host.visit(|s| {
            let root = is_claude(s) && s.meta.origin != ThreadMeta::SUBAGENT;
            let custody = s.meta.facts.get(CUSTODY_FACT).and_then(|c| c.parse::<SessionId>().ok());
            let live = s.status.liveness == Liveness::Live;
            (root && (live || custody.is_some())).then_some((s.meta.id, custody))
        });
        let mut claimed = Vec::new();
        for (thread, custody) in threads {
            let info = custody.and_then(|id| held.iter().find(|s| s.id == id));
            match info {
                Some(info) => {
                    claimed.push(info.id);
                    self.reattach(thread, info.id, info.pid, info.exited.is_none()).await;
                }
                None => lost(&self.host, thread),
            }
        }
        for orphan in held.iter().filter(|s| !claimed.contains(&s.id)) {
            tracing::info!(session = %orphan.id, "a piped session no thread names: closed");
            self.custody.close(orphan.id).await;
        }
    }

    /// Take up `thread`'s Claude Code, session `session` (child `pid`) in ptyd, `running` while
    /// its child lives: the thread read again from the transcript, then what Claude Code wrote
    /// while no worker held it.
    async fn reattach(&self, thread: ThreadId, session: SessionId, pid: u32, running: bool) {
        let Some((state, _)) = self.host.state(thread) else { return };
        let taken = match self.custody.take(session, pid).await {
            Ok(taken) => taken,
            Err(why) => {
                tracing::warn!(%thread, "its Claude Code could not be taken up: {why}");
                lost(&self.host, thread);
                return;
            }
        };
        self.custody.read(session).await;
        let records = match self.records(&state.meta).await {
            Ok(records) => records,
            Err(why) => {
                // Kept as it is rather than read again from nothing: its Claude Code is ended,
                // and its next message tries the transcript again.
                say(&self.host, thread, "unread", &format!("{UNREAD}: {why}"));
                self.custody.close(session).await;
                lost(&self.host, thread);
                return;
            }
        };
        let (driven, outs) = read_again(&state, &records, running);
        take(&self.host, outs);
        tracing::info!(%thread, %session, "a Claude Code taken up again");
        self.serve(thread, driven, session, taken, Vec::new());
    }

    /// The records of `meta`'s session, as Claude Code wrote them, or why they cannot be read.
    async fn records(&self, meta: &ThreadMeta) -> Result<Vec<serde_json::Value>, String> {
        let (home, cwd, native) =
            (self.place.home.clone(), PathBuf::from(&meta.cwd), meta.native.clone());
        let read =
            tokio::task::spawn_blocking(move || agent::sessions::records(&home, &cwd, &native))
                .await;
        let why = match read {
            Ok(Ok(records)) => return Ok(records),
            Ok(Err(e)) => e.to_string(),
            Err(e) => e.to_string(),
        };
        tracing::warn!(thread = %meta.id, "its transcript could not be read: {why}");
        Err(why)
    }

    /// Start the thread of intent `id` as `start` says, once. A start that names one of Claude
    /// Code's sessions (`--resume <id>`) takes it up again: in the thread this worker keeps of
    /// it when there is one, else in a new thread read from the session's transcript.
    async fn begin(&mut self, id: IntentId, start: &Start, seated: Option<Seated>) -> Outcome {
        if let Some(outcome) = self.host.started(id) {
            return outcome;
        }
        if let Err(why) = checked(start) {
            return self.host.record_start(id, refused(&why));
        }
        // A Claude Code too old to drive is not started at all: the person is told to update it.
        if let Ok(launch) = self.launcher().await
            && let Some(why) = launch.version.as_deref().and_then(agent::too_old)
        {
            return self.host.record_start(id, refused(&why));
        }
        let resumed = match resumed_of(&start.args) {
            Ok(resumed) => resumed.map(str::to_owned),
            Err(why) => return self.host.record_start(id, refused(&why)),
        };
        let prompt = start.prompt.clone().filter(|p| !p.trim().is_empty());
        let prompt = prompt.or_else(|| (!start.attachments.is_empty()).then(String::new));
        let first: Vec<ThreadAsk> = prompt
            .map(|text| ThreadAsk::Send {
                text,
                attachments: start.attachments.clone(),
                intent: id,
                delivery: Delivery::Steer,
            })
            .into_iter()
            .collect();
        if let Some(native) = &resumed {
            let thread = agent::thread_of(native);
            if self.host.state(thread).is_some_and(|(s, _)| is_claude(&s)) {
                self.route(thread, first).await;
                return self.host.record_start(id, Outcome::Started { thread });
            }
        }
        let native = resumed.clone().unwrap_or_else(|| id.to_string());
        let mut kept =
            agent::kept(start.model.as_deref(), start.mode.as_deref(), start.effort.as_deref());
        if let Some(seated) = &seated {
            kept = seat_args(kept, seated);
        }
        let mut begun = None;
        let outcome = self.host.start(id, || {
            let mut driven = Driven::new(&native, &start.cwd, WallMs::now());
            let mut outs = driven.drain();
            outs.extend(driven.started_with(&kept));
            let meta = driven.meta().clone();
            begun = Some((driven, outs));
            Ok(meta)
        });
        let (Outcome::Started { thread }, Some((driven, outs))) = (&outcome, begun) else {
            return outcome;
        };
        let thread = *thread;
        take(&self.host, outs);
        if let Some(seated) = &seated {
            self.host.seated(thread, seated);
        }
        let session = match resumed {
            Some(native) => Session::Resume(native),
            None => Session::New(native),
        };
        if matches!(session, Session::Resume(_)) {
            // A session taken up again is read from its transcript before the first message.
            // A new thread: one whose transcript cannot be read loses nothing shown, and
            // says why it starts empty.
            let Some((state, _)) = self.host.state(thread) else { return outcome };
            let read = self.records(&state.meta).await;
            let (driven, outs) = read_again(&state, read.as_deref().unwrap_or_default(), false);
            take(&self.host, outs);
            if let Err(why) = read {
                say(&self.host, thread, "unread", &format!("{UNREAD}: {why}"));
            }
            self.run(thread, driven, session, first).await;
        } else {
            self.run(thread, driven, session, first).await;
        }
        outcome
    }

    /// Branch a new thread off `from`'s session for intent `id`, once: Claude Code, run for the
    /// new thread on a session of the intent's id, resumes `from`'s into it (`--fork-session`),
    /// all of it, or through turn `after` (`--resume-session-at` that turn's last record).
    async fn fork(&mut self, from: ThreadId, id: IntentId, after: Option<TurnId>) -> Outcome {
        if let Some(outcome) = self.host.started(id) {
            return outcome;
        }
        let Some((state, _)) = self.host.state(from).filter(|(s, _)| is_claude(s)) else {
            return self.host.record_start(id, refused("There is no such Claude Code thread here"));
        };
        let last = state.last_turn().map(|t| t.id);
        // A turn before the last: the record the next turn's prompt follows is that turn's last.
        let at = match after.filter(|after| Some(*after) != last) {
            Some(after) => {
                let records = match self.records(&state.meta).await {
                    Ok(records) => records,
                    Err(why) => {
                        let why = format!("Its transcript could not be read: {why}");
                        return self.host.record_start(id, refused(&why));
                    }
                };
                match resume_at(&state, after, &records) {
                    Ok(at) => Some(at),
                    Err(why) => return self.host.record_start(id, refused(&why)),
                }
            }
            None => None,
        };
        let native = id.to_string();
        let mut begun = None;
        let outcome = self.host.start(id, || {
            let turn = if at.is_some() { after } else { last };
            // Claude Code writes the conversation with its first message: before it there is none.
            if turn.is_none() {
                return Err("There is nothing to fork yet".to_owned());
            }
            let mut driven = Driven::new(&native, &state.meta.cwd, WallMs::now());
            let mut outs = driven.drain();
            outs.extend(driven.started_with(&kept_args(&state.meta)));
            outs.extend(driven.forked(Fork { thread: from, turn }));
            let meta = driven.meta().clone();
            begun = Some((driven, outs));
            Ok(meta)
        });
        let (Outcome::Started { thread }, Some((driven, outs))) = (&outcome, begun) else {
            return outcome;
        };
        let thread = *thread;
        take(&self.host, outs);
        let session = Session::Fork { from: state.meta.native.clone(), id: native, at };
        self.run(thread, driven, session, Vec::new()).await;
        outcome
    }

    /// `asks` for `thread`: to its Claude Code while one runs. Else a message takes the thread
    /// up again; a setting is kept for the next run; anything else is dropped, its intent
    /// already answered.
    async fn route(&mut self, thread: ThreadId, asks: Vec<ThreadAsk>) {
        let mut left = Vec::new();
        let asks = self.beside(thread, asks);
        for ask in asks {
            let Some(task) = self.running.lock().get(&thread).cloned() else {
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
        self.running.lock().remove(&thread);
        let Some((state, _)) = self.host.state(thread).filter(|(s, _)| is_claude(s)) else {
            return;
        };
        let mut sends = Vec::new();
        let mut kept = kept_args(&state.meta);
        let mut queue = slopty_agent::queue::Queue::of(&state.pending);
        let mut shown = None;
        for ask in left {
            match ask {
                ThreadAsk::Send { .. } | ThreadAsk::Report { .. } | ThreadAsk::ShellDone { .. } => {
                    sends.push(ask);
                }
                ThreadAsk::App { reply, .. } => {
                    let _gone =
                        reply.send(AppAnswer::Refused("Claude Code is not running".to_owned()));
                }
                ThreadAsk::Withdraw { intent } => shown = queue.withdraw(intent).or(shown),
                ThreadAsk::Edit { intent, text, attachments } => {
                    shown = queue.edit(intent, &text, attachments, ()).or(shown);
                }
                ThreadAsk::Move { intent, before } => {
                    shown = queue.move_to(intent, before).or(shown);
                }
                ThreadAsk::Promote { intent } => {
                    if let Some((held, ())) = queue.take(intent) {
                        queue.release();
                        shown = Some(queue.shown());
                        sends.push(ThreadAsk::Send {
                            text: held.text,
                            attachments: held.attachments,
                            intent,
                            delivery: Delivery::Steer,
                        });
                    }
                }
                ThreadAsk::SetModel { model } => kept = with_flag(kept, agent::MODEL_FLAG, &model),
                ThreadAsk::SetMode { mode } => {
                    kept = with_flag(kept, agent::PERMISSION_MODE_FLAG, &mode);
                }
                ThreadAsk::SetEffort { effort } => {
                    kept = with_flag(kept, agent::EFFORT_FLAG, &effort);
                }
                other => tracing::debug!(%thread, ?other, "an ask of a Claude Code not running"),
            }
        }
        if let Some(shown) = shown {
            self.host.apply(thread, vec![shown]);
        }
        if kept != kept_args(&state.meta) {
            let json = serde_json::to_string(&kept).unwrap_or_default();
            self.host.update(thread, |state| {
                let mut meta = state.meta.clone();
                meta.facts.insert(ARGS_FACT.to_owned(), json);
                (vec![Action::Meta(Box::new(meta))], ())
            });
        }
        if sends.is_empty() || state.meta.origin == ThreadMeta::SUBAGENT {
            return;
        }
        // Read again from the transcript, the thread goes on in a Claude Code of its own.
        let Some((state, _)) = self.host.state(thread) else { return };
        let records = match self.records(&state.meta).await {
            Ok(records) => records,
            Err(why) => {
                // Kept as it is rather than read again from nothing: the messages wait in its
                // queue, and the next one tries the transcript again.
                let mut queue = slopty_agent::queue::Queue::of(&state.pending);
                for send in sends {
                    if let ThreadAsk::Send { text, attachments, intent, .. } = send {
                        let _shown = queue.hold(intent, &text, attachments, (), false);
                    }
                }
                self.host.apply(thread, vec![queue.shown()]);
                say(&self.host, thread, "unread", &format!("{UNREAD}: {why}"));
                return;
            }
        };
        let (driven, outs) = read_again(&state, &records, false);
        take(&self.host, outs);
        self.run(thread, driven, Session::Resume(state.meta.native.clone()), sends).await;
    }

    /// Run Claude Code for `thread` on `session`, with the thread's own flags, and serve it
    /// until it ends; `first`, what it is asked once it runs.
    async fn run(
        &mut self,
        thread: ThreadId,
        driven: Driven,
        session: Session,
        first: Vec<ThreadAsk>,
    ) {
        let mut driven = driven;
        let launch = match self.launcher().await {
            Ok(launch) => match launch.version.as_deref().and_then(agent::too_old) {
                Some(why) => Err(why),
                None => Ok(launch),
            },
            Err(why) => Err(why),
        };
        let launch = match launch {
            Ok(launch) => launch,
            Err(why) => {
                take(&self.host, driven.exited(Some(&why), WallMs::now()));
                return;
            }
        };
        let meta = driven.meta().clone();
        let mut command = vec![launch.program.to_string_lossy().into_owned()];
        command.extend(agent::args(&session, &kept_args(&meta)));
        let mut env = vec![("PATH".to_owned(), launch.path.to_string_lossy().into_owned())];
        env.extend(agent::ENV.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())));
        if let Some(seated) = self.host.seated_of(thread) {
            env.extend(self.host.env_of(&seated));
        }
        let id = SessionId::new();
        let dir = self.place.data_dir.join("claude");
        let stderr = dir.join(format!("{id}.log"));
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(dir = %dir.display(), "Claude Code's error files have no folder: {e}");
        }
        let spec = SpawnSpec {
            command,
            cwd: Some(PathBuf::from(&meta.cwd)),
            env,
            size: slopty_proto::terminal::TermSize::default(),
        };
        let taken = match self.custody.spawn(id, spec, stderr.clone()).await {
            Ok(taken) => taken,
            Err(why) => {
                tracing::warn!(%thread, "Claude Code could not start: {why}");
                let why = format!("Claude Code could not start: {why}");
                take(&self.host, driven.exited(Some(&why), WallMs::now()));
                return;
            }
        };
        tracing::info!(%thread, session = %id, "started Claude Code");
        let id_text = id.to_string();
        take(&self.host, driven.fact(CUSTODY_FACT, Some(&id_text)));
        take(&self.host, driven.branch(branch_in(Path::new(&meta.cwd)).as_deref()));
        self.serve(thread, driven, id, taken, first);
    }

    /// Serve `thread`'s Claude Code, session `session` in ptyd, over `taken`.
    fn serve(
        &self,
        thread: ThreadId,
        driven: Driven,
        session: SessionId,
        taken: custody::Taken,
        first: Vec<ThreadAsk>,
    ) {
        let socket = std::os::unix::net::UnixStream::from(taken.socket);
        let socket =
            socket.set_nonblocking(true).and_then(|()| tokio::net::UnixStream::from_std(socket));
        let socket = match socket {
            Ok(socket) => socket,
            Err(e) => {
                tracing::warn!(%thread, "Claude Code's socket could not be read: {e}");
                return;
            }
        };
        let stderr = self.place.data_dir.join("claude").join(format!("{session}.log"));
        let task = Task::new(
            self.host.clone(),
            thread,
            driven,
            self.custody.clone(),
            (session, stderr, self.place.home.clone()),
        );
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(task.serve(socket, taken.backlog, first, rx, self.ended.clone()));
        self.running.lock().insert(thread, tx);
    }

    /// What of `asks` the worker does itself beside Claude Code: a `!` command runs on a task of
    /// its own and comes back by [`Self::again`] once it ended; a rule is written into its
    /// scope's file, then the rules listed again where Claude Code runs. The rest goes on.
    fn beside(&self, thread: ThreadId, asks: Vec<ThreadAsk>) -> Vec<ThreadAsk> {
        let mut rest = Vec::new();
        for ask in asks {
            match ask {
                ThreadAsk::Shell { command, intent } => self.shell(thread, command, intent),
                ThreadAsk::Rule { change } => {
                    let Some((state, _)) = self.host.state(thread) else { continue };
                    let cwd = PathBuf::from(&state.meta.cwd);
                    if let Err(why) = settings::change_rule(&self.place.home, &cwd, &change) {
                        tracing::warn!(%thread, "a permission rule was not written: {why}");
                        let said = format!("The rule was not written: {why}");
                        say(&self.host, thread, "rule", &said);
                        continue;
                    }
                    rest.push(ThreadAsk::Rule { change });
                }
                other => rest.push(other),
            }
        }
        rest
    }

    /// Run `!` command `command` of `thread` for intent `intent` on a task of its own; what it
    /// did comes back as [`ThreadAsk::ShellDone`].
    fn shell(&self, thread: ThreadId, command: String, intent: IntentId) {
        let Some((state, _)) = self.host.state(thread) else { return };
        let (host, again, home) = (self.host.clone(), self.again.clone(), self.place.home.clone());
        let cwd = PathBuf::from(&state.meta.cwd);
        tokio::spawn(async move {
            let ran = settings::run_shell(&host, thread, &cwd, &command, intent).await;
            let quiet = !settings::answers_bang(&home, &cwd);
            let ask = ThreadAsk::ShellDone { ran, intent, quiet };
            let _gone = again.send(Ask::Thread { thread, ask });
        });
    }

    /// A page's ask of its app, to the session's Claude Code where it runs.
    fn app(&self, thread: ThreadId, ask: AppAsk, reply: oneshot::Sender<AppAnswer>) {
        let root = root_of(&self.host, thread).map_or(thread, |meta| meta.id);
        let task = self.running.lock().get(&root).cloned();
        match task {
            Some(task) => {
                if let Err(mpsc::error::SendError(ThreadAsk::App { reply, .. })) =
                    task.send(ThreadAsk::App { ask, reply })
                {
                    let _gone = reply.send(AppAnswer::Refused("Claude Code ended".to_owned()));
                }
            }
            None => {
                let _gone = reply.send(AppAnswer::Refused("Claude Code is not running".to_owned()));
            }
        }
    }

    /// Answer `reply` with the whole of `content`, clipped in `thread`, read from its session's
    /// transcript on the blocking pool.
    fn expand(&self, thread: ThreadId, content: ContentRef, reply: oneshot::Sender<Expanded>) {
        let Some(root) = root_of(&self.host, thread) else {
            let _gone = reply.send(Expanded::Gone);
            return;
        };
        let home = self.place.home.clone();
        drop(tokio::task::spawn_blocking(move || {
            let cwd = PathBuf::from(&root.cwd);
            let body = agent::sessions::expand(&home, &cwd, &root.native, &content);
            let _gone = reply.send(body);
        }));
    }
}

/// The last record of turn `after` of `state`, as `records` (its session's) hold it: the one the
/// next turn's prompt follows.
fn resume_at(
    state: &ThreadState,
    after: TurnId,
    records: &[serde_json::Value],
) -> Result<String, String> {
    if !state.turns.iter().any(|t| t.id == after) {
        return Err(format!("There is no turn {} to fork from", after.0));
    }
    let next = state
        .turns
        .iter()
        .filter(|t| t.id > after)
        .min_by_key(|t| t.id)
        .and_then(|t| t.input.as_ref())
        .ok_or("The turn after it is not known")?;
    let prompt = next.0.split_once(':').map_or(next.0.as_str(), |(record, _)| record);
    records
        .iter()
        .find(|r| r.get("uuid").and_then(serde_json::Value::as_str) == Some(prompt))
        .and_then(|r| r.get("parentUuid").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .ok_or_else(|| "That turn's place in the session is not in its transcript".to_owned())
}

/// The session thread `thread` is (or is a subagent of).
fn root_of(host: &Host, thread: ThreadId) -> Option<ThreadMeta> {
    let mut at = thread;
    for _ in 0..16 {
        let (state, _) = host.state(at)?;
        if state.meta.origin != ThreadMeta::SUBAGENT {
            return Some(state.meta);
        }
        at = state.meta.parent?.thread;
    }
    None
}

/// What is wrong with `start` for Claude Code, if anything.
fn checked(start: &Start) -> Result<(), String> {
    if !start.agent.is(AgentId::CLAUDE_CODE) {
        return Err(format!("{} is not Claude Code", start.agent.0));
    }
    if start.drive.as_ref().is_some_and(|d| !d.is(Drive::DRIVEN)) {
        return Err("Claude Code is only driven".to_owned());
    }
    if let Some(mode) =
        start.mode.as_deref().filter(|m| !slopty_agent::claude::offers::startable_mode(m))
    {
        return Err(format!("Claude Code starts in no mode {mode}"));
    }
    if let Some(effort) =
        start.effort.as_deref().filter(|e| !slopty_agent::claude::offers::startable_effort(e))
    {
        return Err(format!("Claude Code takes no effort {effort}"));
    }
    super::attach::check(&start.attachments)?;
    if !Path::new(&start.cwd).is_dir() {
        return Err(format!("There is no folder {} here", start.cwd));
    }
    Ok(())
}

/// The session a client's start asks Claude Code to take up again in [`Start::args`]
/// (`--resume <id>`), when it asks; why not, in words, for any other argument.
fn resumed_of(args: &[String]) -> Result<Option<&str>, String> {
    match args {
        [] => Ok(None),
        [flag, session] if flag == agent::RESUME_FLAG && agent::is_session_id(session) => {
            Ok(Some(session))
        }
        _ => Err("Claude Code takes no arguments from a start but --resume <id>".to_owned()),
    }
}

/// `kept` with a seat's own: its system text as the system prompt's addition, and Slopty's
/// tools where Slopty's CLI is found.
fn seat_args(kept: Vec<String>, seated: &Seated) -> Vec<String> {
    let mut kept = kept;
    if let Some(system) = seated.system.as_deref().filter(|s| !s.trim().is_empty()) {
        kept.push(format!("--append-system-prompt={system}"));
    }
    match seated.relay.as_deref() {
        Some(relay) => slopty_agent::mcp::with_mcp(kept, relay),
        None => kept,
    }
}

/// The flags `meta`'s Claude Code was started with, as its thread keeps them.
fn kept_args(meta: &ThreadMeta) -> Vec<String> {
    meta.facts.get(ARGS_FACT).and_then(|json| serde_json::from_str(json).ok()).unwrap_or_default()
}

/// `kept` with `flag` given `value`, in place of any value it had.
fn with_flag(kept: Vec<String>, flag: &str, value: &str) -> Vec<String> {
    let prefix = format!("{flag}=");
    let mut kept: Vec<String> = kept.into_iter().filter(|a| !a.starts_with(&prefix)).collect();
    kept.push(format!("{prefix}{value}"));
    kept
}

/// The branch checked out in the repository `cwd` is in, if it is in one.
fn branch_in(cwd: &Path) -> Option<String> {
    cwd.ancestors().find(|dir| dir.join(".git").exists()).and_then(crate::repo::branch_of)
}

/// `thread`, whose Claude Code is gone with nothing to say how it ended, ended as it was left:
/// what it asked withdrawn, its turn under way stopped, exited and resumable.
fn lost(host: &Host, thread: ThreadId) {
    tracing::info!(%thread, "{LOST}");
    host.update(thread, |state| {
        let mut actions = slopty_agent::driven::cut_short(state, true, WallMs::now());
        let mut meta = state.meta.clone();
        if meta.facts.remove(CUSTODY_FACT).is_some() {
            actions.push(Action::Meta(Box::new(meta)));
        }
        (actions, ())
    });
}

/// What a thread says when its transcript cannot be read.
const UNREAD: &str = "Its transcript could not be read, so the thread was kept as it was";

/// Say `text` on `thread`, a notice of the worker's own, its id from `kind` and the time.
fn say(host: &Host, thread: ThreadId, kind: &str, text: &str) {
    let Some((state, _)) = host.state(thread) else { return };
    let now = WallMs::now();
    let notice = slopty_proto::thread::Item {
        id: ItemId(format!("{kind}-{}", now.as_millis())),
        turn: state.last_turn().map_or(TurnId::BEFORE, |t| t.id),
        at_ms: now,
        body: slopty_proto::thread::ItemBody::Notice(slopty_proto::thread::Notice::new(
            slopty_proto::thread::Notice::INFO,
            slopty_proto::thread::Clipped::whole(text),
        )),
    };
    host.apply(thread, vec![Action::ItemCompleted(notice)]);
}

/// Put what the codec made into the host: a thread begun anew if the host holds it already.
fn take(host: &Host, outs: Vec<Out>) {
    for out in outs {
        match out {
            Out::Begin(meta) => {
                let id = meta.id;
                let begun = if host.holds(id) {
                    host.reset(id, ThreadState::new(*meta)).map(|_| ())
                } else {
                    host.create(*meta).map(|_| ())
                };
                if let Err(e) = begun {
                    tracing::warn!(thread = %id, "a Claude Code thread could not begin: {e}");
                }
            }
            Out::Actions(thread, actions) => {
                host.apply(thread, actions);
            }
        }
    }
}

fn refused(reason: &str) -> Outcome {
    Outcome::Refused { reason: reason.to_owned() }
}
