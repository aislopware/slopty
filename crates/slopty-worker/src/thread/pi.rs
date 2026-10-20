//! pi, driven over its RPC mode, on the worker: the IO half of the adapter
//! (`slopty_agent::pi::driven` is the codec).
//!
//! A thread is started by the person ([`Pi::start`], `ThreadRequest::Start` for agent `pi`): the
//! worker runs the person's own `pi` in the thread's directory as `pi --mode rpc` with Slopty's
//! gate ([`slopty_agent::pi`]), on a session named by the start's intent id, which pi keeps in
//! its own session directory beside the person's others. One task per running pi carries its
//! records both ways. What a client asks of the thread ([`Pi::decide`]: a message, an
//! interrupt, an answer, a model) goes to that task.
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
//!   exited and resumable. The next message starts pi again on the same session, with the flags it
//!   was started with, and the thread is read again from the session's entries before the message
//!   goes, since the session is the record and the thread a cache of it.

pub mod driven;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use slopty_agent::pi::driven::Driven;
use slopty_agent::pi::sessions;
use slopty_agent::queue::Queue;
use slopty_core::WallMs;
use slopty_proto::thread::wire::{Intent, Outcome, PastSession, Start};
use slopty_proto::thread::{
    Action, AgentId, Answerer, AskId, Delivery, Drive, Fork, IntentId, Liveness, Phase,
    RequestState, ThreadId, ThreadState, TurnId,
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use self::driven::{Ready, Task, start_pi};
use super::{Host, Seated};
use crate::facts::Installed;

/// How long pi has to end once its stdin closes, before it is killed.
pub const SHUTDOWN: Duration = Duration::from_secs(5);

/// Where a thread's pi says it has ended and is reaped, with the asks it did not take.
type Ended = mpsc::UnboundedSender<(ThreadId, Vec<ThreadAsk>)>;

/// What a client asks of the driven pi threads.
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
    /// A server task's thread is settled: its agent ends, its session kept, and its seat is
    /// forgotten.
    Close {
        thread: ThreadId,
        done: oneshot::Sender<()>,
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
    /// Take back the message held for `intent`.
    Withdraw {
        intent: IntentId,
    },
    /// Make the message held for `intent` say `text`, with the files at `attachments`.
    Edit {
        intent: IntentId,
        text: String,
        attachments: Vec<String>,
    },
    /// Send the message held for `intent` now.
    Promote {
        intent: IntentId,
    },
    /// Move the message held for `intent` to just before `before`, or last.
    Move {
        intent: IntentId,
        before: Option<IntentId>,
    },
    Interrupt,
    Answer {
        ask: AskId,
        choice: String,
        message: Option<String>,
        by: Answerer,
    },
    SetModel {
        model: String,
    },
    SetEffort {
        effort: String,
    },
    /// How far the gate lets calls through without asking ([`slopty_agent::pi::modes`]).
    SetMode {
        mode: String,
    },
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
        let ask = Ask::Start { id, start, seated: None, reply };
        if self.0.send(ask).is_err() {
            return refused("pi threads are not served here");
        }
        outcome.await.unwrap_or_else(|_| refused("the pi threads stopped"))
    }

    /// End thread `thread`'s agent for its settled task, its session kept to take up again.
    pub async fn close(&self, thread: ThreadId) {
        let (done, closed) = oneshot::channel();
        if self.0.send(Ask::Close { thread, done }).is_ok() {
            let _done = closed.await;
        }
    }

    /// Start a seated thread (an agent another started) as `start` says, once per seat: pi runs
    /// with the seat's variables, Slopty's gate registers Slopty's tools among its MCP servers
    /// ([`slopty_agent::pi::TOOLS_ENV`]), and the system text goes as pi's own addition to its
    /// system prompt (`--append-system-prompt`), kept with the thread's flags for every later
    /// run.
    pub async fn start_seated(&self, start: Box<Start>, seated: Seated) -> Outcome {
        let (reply, outcome) = oneshot::channel();
        let (id, seated) = (seated.intent(), Some(Box::new(seated)));
        if self.0.send(Ask::Start { id, start, seated, reply }).is_err() {
            return refused("pi threads are not served here");
        }
        outcome.await.unwrap_or_else(|_| refused("the pi threads stopped"))
    }

    /// Branch a new thread off the whole of `thread`'s session for intent `id`, once: pi, run
    /// for the new thread, copies the session (`--fork`).
    pub async fn fork(&self, thread: ThreadId, id: IntentId, after: Option<TurnId>) -> Outcome {
        let (reply, outcome) = oneshot::channel();
        if self.0.send(Ask::Fork { thread, id, after, reply }).is_err() {
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
            Intent::Send { text, attachments, .. }
                if text.trim().is_empty() && attachments.is_empty() =>
            {
                return refused("There is nothing to send");
            }
            Intent::Send { text, attachments, delivery } => {
                if let Err(why) = super::attach::check(attachments) {
                    return refused(&why);
                }
                let (text, attachments, delivery) = (text.clone(), attachments.clone(), *delivery);
                ThreadAsk::Send { text, attachments, intent, delivery }
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
                // One that is gone wakes only by a message. One at rest has nothing left to
                // stop: done, not refused, so a stop never fails for having come late.
                if !live {
                    return refused("pi is not running");
                }
                if !matches!(state.status.phase, Phase::Working | Phase::NeedsYou) {
                    return Outcome::Done;
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
            Intent::SetModel { model } => {
                if !state.meta.models.iter().any(|m| m.id == *model) {
                    return refused(&format!("There is no model {model} here"));
                }
                ThreadAsk::SetModel { model: model.clone() }
            }
            Intent::SetEffort { effort } => {
                if !state.meta.efforts.iter().any(|e| e.id == *effort) {
                    return refused(&format!("The model has no thinking level {effort}"));
                }
                ThreadAsk::SetEffort { effort: effort.clone() }
            }
            Intent::SetMode { mode } => {
                if !slopty_agent::pi::modes::known(mode) {
                    return refused(&format!("pi's gate has no mode {mode}"));
                }
                ThreadAsk::SetMode { mode: mode.clone() }
            }
            other => return other.unsupported(),
        };
        // What is held is the thread's record, answered whether pi runs or not.
        let held = matches!(
            asked,
            ThreadAsk::Send { .. }
                | ThreadAsk::Withdraw { .. }
                | ThreadAsk::Edit { .. }
                | ThreadAsk::Promote { .. }
                | ThreadAsk::Move { .. }
        );
        if !live && !held {
            return refused("pi is not running");
        }
        if self.0.send(Ask::Thread { thread, ask: asked }).is_err() {
            return refused("pi threads are not served here");
        }
        Outcome::Done
    }
}

/// Whether `state` is a pi thread, which this worker drives.
#[must_use]
pub fn is_pi(state: &ThreadState) -> bool {
    state.meta.agent.is(AgentId::PI)
}

/// Serve the driven pi threads in `host`, with the gate written under `data_dir`, taking the
/// asks of their [`Pi`].
///
/// pi is looked for on `path` alone when it is given, else as the person's terminal finds it
/// ([`crate::facts::installed`]). A thread a pi of an earlier worker drove is told exited
/// first, since that pi ended with it.
pub fn spawn(
    host: Host,
    data_dir: PathBuf,
    path: Option<OsString>,
    Asks(mut asks): Asks,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let (ended_tx, mut ended) = mpsc::unbounded_channel();
        let mut served = Served {
            host: host.clone(),
            data_dir,
            path,
            launcher: None,
            running: HashMap::new(),
            ended: ended_tx,
        };
        // Only the threads left live are opened, by their heads.
        let live =
            host.visit(|s| (is_pi(s) && s.status.liveness == Liveness::Live).then_some(s.meta.id));
        for thread in live {
            host.update(thread, |state| (slopty_agent::pi::driven::gone(state, WallMs::now()), ()));
        }
        loop {
            tokio::select! {
                ask = asks.recv() => match ask {
                    Some(Ask::Start { id, start, seated, reply }) => {
                        let outcome = served.begin(id, &start, seated.map(|s| *s)).await;
                        let _gone = reply.send(outcome);
                    }
                    Some(Ask::Fork { thread, id, after, reply }) => {
                        let outcome = served.fork(thread, id, after).await;
                        let _gone = reply.send(outcome);
                    }
                    Some(Ask::Thread { thread, ask }) => served.route(thread, vec![ask]).await,
                    Some(Ask::Close { thread, done }) => {
                        // Its task ends its agent once nothing can ask it more.
                        served.running.remove(&thread);
                        let _gone = done.send(());
                    }
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
    /// The task of each thread's pi while it runs: the session's one writer.
    running: HashMap<ThreadId, mpsc::UnboundedSender<ThreadAsk>>,
    /// Told by a task when its pi has ended.
    ended: Ended,
}

impl Served {
    async fn launcher(&mut self) -> Result<Launcher, String> {
        if let Some(launcher) = &self.launcher {
            return Ok(launcher.clone());
        }
        let gate = slopty_agent::pi::install(&self.data_dir)
            .map_err(|e| format!("Couldn't write Slopty's gate for pi: {e}"))?;
        let pi = crate::facts::installed("pi", self.path.clone())
            .await
            .ok_or_else(|| "pi is not installed".to_owned())?;
        let launcher = Launcher { pi, gate };
        self.launcher = Some(launcher.clone());
        Ok(launcher)
    }

    /// Start the thread of intent `id` as `start` says, once. A start that names one of pi's
    /// sessions (`--session <id>`) takes it up again: in the thread this worker keeps of it when
    /// there is one, else in a new thread read from the session's entries.
    async fn begin(&mut self, id: IntentId, start: &Start, seated: Option<Seated>) -> Outcome {
        if let Some(outcome) = self.host.started(id) {
            return outcome;
        }
        let system =
            seated.as_ref().and_then(|s| s.system.clone()).filter(|r| !r.trim().is_empty());
        if let Some(mode) = start.mode.as_deref().filter(|m| !slopty_agent::pi::modes::known(m)) {
            return self.host.record_start(id, refused(&format!("pi's gate has no mode {mode}")));
        }
        if let Err(why) = super::attach::check(&start.attachments) {
            return self.host.record_start(id, refused(&why));
        }
        // The level goes ahead of the first message, so its first turn thinks as chosen.
        let effort = start.effort.clone().map(|effort| ThreadAsk::SetEffort { effort });
        let prompt = start.prompt.clone().filter(|p| !p.trim().is_empty());
        let prompt = prompt.or_else(|| (!start.attachments.is_empty()).then(String::new));
        let send = prompt.map(|text| ThreadAsk::Send {
            text,
            attachments: start.attachments.clone(),
            intent: id,
            delivery: Delivery::Steer,
        });
        // The gate's level goes first too, so no call of the first turn asks more than it.
        let mode = start.mode.clone().map(|mode| ThreadAsk::SetMode { mode });
        let first: Vec<ThreadAsk> = mode.into_iter().chain(effort).chain(send).collect();
        let resumed = sessions::resumed(&start.args);
        let (session, own) = match resumed {
            Some((session, rest)) => (session.to_owned(), rest),
            None => (id.to_string(), start.args.as_slice()),
        };
        if resumed.is_some() {
            let thread = slopty_agent::pi::driven::thread_of(&session);
            if self.host.state(thread).is_some_and(|(s, _)| is_pi(&s)) {
                if self.running.contains_key(&thread) {
                    self.route(thread, first).await;
                } else {
                    self.drive(thread, first).await;
                }
                return self.host.record_start(id, Outcome::Started { thread });
            }
        }
        let found = self.launcher().await;
        let mut begun = None;
        // What is refused is refused once, as what is started is started once.
        let outcome = self.host.start(id, || {
            if start.drive.as_ref().is_some_and(|d| !d.is(Drive::DRIVEN)) {
                return Err("pi is only driven over RPC".to_owned());
            }
            let mut args = slopty_agent::pi::checked(own)?.to_vec();
            // The system text is the thread's own flag, kept with the others for every later run.
            args.extend(
                system.iter().flat_map(|r| ["--append-system-prompt".to_owned(), r.clone()]),
            );
            if !Path::new(&start.cwd).is_dir() {
                return Err(format!("There is no folder {} here", start.cwd));
            }
            let launch = found.clone()?;
            let version = launch.pi.version.clone().unwrap_or_default();
            let (mut driven, mut actions) =
                Driven::new(&session, &version, &start.cwd, WallMs::now());
            actions.extend(driven.started_with(&args));
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
        if let Some(seated) = seated {
            self.host.seated(thread, &seated);
        }
        if let Some((provider, model)) = start.model.as_deref().and_then(|m| m.split_once('/')) {
            args.extend(["--provider".to_owned(), provider.to_owned()]);
            args.extend(["--model".to_owned(), model.to_owned()]);
        }
        // A session taken up again is read from its entries before the first message goes.
        let ready = if resumed.is_some() { Ready::AfterEntries(first) } else { Ready::Now(first) };
        self.run(&launch, thread, driven, &args, ready);
        outcome
    }

    /// Branch a new thread off the whole of `from`'s session for intent `id`, once: pi, run for
    /// the new thread on a session of the intent's id, copies `from`'s first (`--fork`), and the
    /// thread is read from the copy's entries.
    async fn fork(&mut self, from: ThreadId, id: IntentId, after: Option<TurnId>) -> Outcome {
        if let Some(outcome) = self.host.started(id) {
            return outcome;
        }
        let Some((state, _)) = self.host.state(from).filter(|(s, _)| is_pi(s)) else {
            return self.host.record_start(id, refused("There is no such pi thread here"));
        };
        let found = self.launcher().await;
        let session = id.to_string();
        let mut begun = None;
        let outcome = self.host.start(id, || {
            let turn = crate::thread::fork::whole(&state, after, "pi")?;
            let launch = found.clone()?;
            let version = launch.pi.version.clone().unwrap_or_default();
            let (mut driven, mut actions) =
                Driven::new(&session, &version, &state.meta.cwd, WallMs::now());
            let args = kept_args(&state.meta);
            actions.extend(driven.started_with(&args));
            actions.extend(driven.forked(Fork { thread: from, turn }));
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
        // Only this first run copies the session; every later one opens the copy by its id.
        let args = [args, sessions::fork_args(&state.meta.native).to_vec()].concat();
        self.run(&launch, thread, driven, &args, Ready::AfterEntries(Vec::new()));
        outcome
    }

    /// `asks` for `thread`: to its pi while one runs. Else a message takes the thread up again,
    /// driven; anything else is dropped, its intent already answered.
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
        let Some((state, _)) = self.host.state(thread).filter(|(s, _)| is_pi(s)) else {
            return;
        };
        let left = self.held_at_rest(thread, &state, left);
        let (sends, dropped): (Vec<_>, Vec<_>) =
            left.into_iter().partition(|ask| matches!(ask, ThreadAsk::Send { .. }));
        for ask in &dropped {
            tracing::debug!(%thread, ?ask, "an ask of a pi that is not running");
        }
        if sends.is_empty() {
            return;
        }
        self.drive(thread, sends).await;
    }

    /// `asks` of `thread`'s queue while no pi runs: a message taken back or changed in the
    /// record, and one sent now made a message that takes the thread up again. The other asks.
    fn held_at_rest(
        &self,
        thread: ThreadId,
        state: &ThreadState,
        asks: Vec<ThreadAsk>,
    ) -> Vec<ThreadAsk> {
        let mut queue = Queue::of(&state.pending);
        let mut shown = None;
        let mut left = Vec::new();
        for ask in asks {
            match ask {
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
                        left.push(ThreadAsk::Send {
                            text: held.text,
                            attachments: held.attachments,
                            intent,
                            delivery: Delivery::Steer,
                        });
                    }
                }
                ask => left.push(ask),
            }
        }
        if let Some(shown) = shown {
            self.host.apply(thread, vec![shown]);
        }
        left
    }

    /// Drive `thread`'s pi again, read again from its session, then send `sends`.
    async fn drive(&mut self, thread: ThreadId, sends: Vec<ThreadAsk>) {
        let Some((state, _)) = self.host.state(thread) else { return };
        let launch = match self.launcher().await {
            Ok(launch) => launch,
            Err(why) => {
                tracing::warn!(%thread, "pi could not start: {why}");
                let (mut driven, _) = driven_of(&state);
                self.host.apply(thread, driven.exited(Some(&why), WallMs::now()));
                return;
            }
        };
        // Checked again: the flags are the thread's own, kept in its log.
        let args = kept_args(&state.meta);
        let (driven, _) = driven_of(&state);
        self.run(&launch, thread, driven, &args, Ready::AfterEntries(sends));
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
        let seat = self.host.seated_of(thread).map(|seated| self.host.env_of(&seated));
        let env = with_tools(seat.unwrap_or_default(), self.host.tools_cli().as_deref());
        match start_pi(launch, &native, &cwd, args, &env) {
            Ok((stdin, pi)) => {
                let (tx, rx) = mpsc::unbounded_channel();
                let task = Task::new(self.host.clone(), thread, driven, stdin);
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

/// The flags `meta`'s pi was started with, checked again: they are read from the log.
fn kept_args(meta: &slopty_proto::thread::ThreadMeta) -> Vec<String> {
    let args = slopty_agent::pi::driven::args_of(meta);
    match slopty_agent::pi::checked(&args) {
        Ok(args) => args.to_vec(),
        Err(why) => {
            tracing::warn!(thread = %meta.id, "a kept flag is refused: {why}");
            Vec::new()
        }
    }
}

/// The codec of the thread `state`, as it stands, read again from nothing but for the
/// messages it holds.
fn driven_of(state: &ThreadState) -> (Driven, Vec<Action>) {
    let (mut driven, actions) = Driven::of(&state.meta, WallMs::now());
    *driven.queue() = Queue::of(&state.pending);
    (driven, actions)
}

fn refused(reason: &str) -> Outcome {
    Outcome::Refused { reason: reason.to_owned() }
}

/// pi's sessions in folder `cwd`, kept under pi's directory `agent`
/// ([`sessions::agent_dir`]), the last written first, at most `limit`.
///
/// Each is named by its file and read only at its two ends ([`sessions::READ`]).
///
/// # Errors
///
/// When the folder's sessions cannot be listed, in words.
pub async fn sessions(agent: &Path, cwd: &str, limit: u32) -> Result<Vec<PastSession>, String> {
    let dir = sessions::folder(agent, cwd);
    let mut listing = match tokio::fs::read_dir(&dir).await {
        Ok(listing) => listing,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("Couldn't list pi's sessions: {e}")),
    };
    let mut found = Vec::new();
    while let Some(entry) = listing.next_entry().await.map_err(|e| e.to_string())? {
        let name = entry.file_name();
        let Some(id) = name.to_str().and_then(sessions::id_of) else { continue };
        let Ok(meta) = entry.metadata().await else { continue };
        if meta.is_file() {
            found.push((meta.modified().ok(), meta.len(), id.to_owned(), entry.path()));
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.2.cmp(&b.2)));
    found.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    let mut past = Vec::with_capacity(found.len());
    for (modified, size, id, path) in found {
        let title = match ends(&path, size).await {
            Ok((head, tail)) => sessions::title(&head, &tail),
            Err(e) => {
                tracing::debug!(path = %path.display(), "a pi session could not be read: {e}");
                None
            }
        };
        past.push(sessions::past(&id, cwd, title, modified.map(WallMs::of)));
    }
    Ok(past)
}

/// The first and last [`sessions::READ`] bytes of the `size`-byte file at `path`, as text; the
/// tail is empty when the head holds it all.
async fn ends(path: &Path, size: u64) -> std::io::Result<(String, String)> {
    use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
    let mut file = tokio::fs::File::open(path).await?;
    let mut head = Vec::new();
    (&mut file).take(sessions::READ).read_to_end(&mut head).await?;
    let mut tail = Vec::new();
    if size > sessions::READ {
        file.seek(std::io::SeekFrom::Start(
            size.saturating_sub(sessions::READ).max(sessions::READ),
        ))
        .await?;
        file.take(sessions::READ).read_to_end(&mut tail).await?;
    }
    Ok((String::from_utf8_lossy(&head).into_owned(), String::from_utf8_lossy(&tail).into_owned()))
}

/// What a thread's pi runs with: its seat's variables (`env`, none for the person's own), and
/// Slopty's tools for the gate to register ([`slopty_agent::pi::TOOLS_ENV`]) where Slopty's CLI
/// (`cli`) is found.
fn with_tools(env: Vec<(String, String)>, cli: Option<&str>) -> Vec<(String, String)> {
    let mut env = env;
    if let Some(cli) = cli {
        let tools = slopty_agent::pi::tools(cli, &env);
        env.push((slopty_agent::pi::TOOLS_ENV.to_owned(), tools.to_string()));
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pi runs with its seat's variables, and with Slopty's tools for the gate wherever
    /// Slopty's CLI was found, the person's own pi too.
    #[test]
    fn a_pi_runs_with_its_seat_and_slopty_tools() {
        let seat = vec![("SLOPTY_SESSION".to_owned(), slopty_core::SessionId::new().to_string())];
        assert_eq!(with_tools(seat.clone(), None), seat, "no tools without the CLI");
        let env = with_tools(seat.clone(), Some("/bin/slopty"));
        assert_eq!(env[..1], seat);
        let (name, tools) = &env[1];
        assert_eq!(name, slopty_agent::pi::TOOLS_ENV);
        let tools: serde_json::Value = serde_json::from_str(tools).unwrap_or_default();
        assert_eq!(tools, slopty_agent::pi::tools("/bin/slopty", &seat));
        let own = with_tools(Vec::new(), Some("/bin/slopty"));
        assert_eq!(own.len(), 1, "the person's own pi gets the tools: {own:?}");
        assert_eq!(tools, slopty_agent::pi::tools("/bin/slopty", &seat));
    }
}
