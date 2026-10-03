//! Any ACP agent, driven over the Agent Client Protocol, on the worker: the IO half of the
//! adapter (`slopty_agent::acp::driven` is the codec).
//!
//! A thread is started by the person ([`Acp::start`], `ThreadRequest::Start` for agent
//! `acp:<name>`): the worker runs the person's own program for that agent, as the registry
//! starts it ([`slopty_agent::acp::registry`]), in the thread's directory, and speaks ACP to it
//! on its stdio. One task per running agent carries its messages both ways. What a client asks
//! of the thread ([`Acp::decide`]: a message, an interrupt, an answer, a mode, a model) goes to
//! that task.
//!
//! - **The program.** It is found as the person's terminal finds it: on the daemon's `PATH`, else
//!   on their login shell's ([`crate::facts::installed`]), and runs with that `PATH` and the
//!   daemon's environment, so it reaches its provider with the person's own settings. Slopty signs
//!   nothing in, and a start's own arguments are refused: an agent's command line is the registry's
//!   or the person's settings', never a client's.
//! - **One writer.** A session has at most one agent here: a thread's next agent starts only once
//!   the last one is reaped.
//! - **Fail closed.** A permission request is a request on the thread, answered only with what the
//!   agent offers. A request of any other kind is refused, so a call that needs the client's file
//!   system or terminal does not run. When the worker goes, the agent's stdin closes and it ends; a
//!   call waiting on the person then never runs.
//! - **One message at a time.** ACP takes no message while a turn runs, so one sent then waits on
//!   the worker ([`Cap::QUEUE`]) and goes once the turn ends.
//! - **Exited, and resumed.** A thread whose agent is gone (it exited, or the worker restarted) is
//!   exited, resumable when the agent can load its sessions (`loadSession`). The next message
//!   starts the agent again and loads the session, which replays it: the thread is read again from
//!   the agent's own record before the message goes.

mod task;

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use slopty_agent::acp::driven::{self, Session};
use slopty_agent::acp::registry;
use slopty_core::WallMs;
use slopty_proto::thread::wire::{Intent, Outcome, Start};
use slopty_proto::thread::{
    Answerer, AskId, Cap, Delivery, Drive, IntentId, Liveness, Phase, RequestState, ThreadId,
    ThreadState,
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use self::task::{Opening, Task, start_agent};
use super::Host;

/// How long an agent has to end once its stdin closes, before it is killed.
pub const SHUTDOWN: Duration = Duration::from_secs(5);

/// The person's own ACP agents, as their settings name them: a name and its command line
/// ([`registry::registry`]). Read at each start, so an edit takes effect without a restart.
pub type Own = Arc<dyn Fn() -> BTreeMap<String, Vec<String>> + Send + Sync>;

/// Where a thread's task says its agent has ended and is reaped, with the asks it did not take.
type Ended = mpsc::UnboundedSender<(ThreadId, Vec<ThreadAsk>)>;

/// What a client asks of the ACP threads.
#[derive(Debug)]
enum Ask {
    Start { id: IntentId, start: Box<Start>, reply: oneshot::Sender<Outcome> },
    Thread { thread: ThreadId, ask: ThreadAsk },
}

/// What a client asks of one thread.
#[derive(Debug)]
enum ThreadAsk {
    Send { text: String, intent: IntentId },
    Withdraw { intent: IntentId },
    Edit { intent: IntentId, text: String },
    Interrupt,
    Answer { ask: AskId, choice: String, by: Answerer },
    SetMode { mode: String },
    SetModel { model: String },
}

/// Where clients' asks go to the ACP threads. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Acp(mpsc::UnboundedSender<Ask>);

/// Where an [`Acp`]'s asks wait for [`spawn`].
#[derive(Debug)]
pub struct Asks(mpsc::UnboundedReceiver<Ask>);

impl Acp {
    /// A handle, and the asks [`spawn`] takes from it.
    #[must_use]
    pub fn channel() -> (Self, Asks) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self(tx), Asks(rx))
    }

    /// Start an ACP thread for intent `id` once, as `start` says: a repeat of the id gets the
    /// first outcome.
    pub async fn start(&self, id: IntentId, start: Box<Start>) -> Outcome {
        let (reply, outcome) = oneshot::channel();
        if self.0.send(Ask::Start { id, start, reply }).is_err() {
            return refused("ACP threads are not served here");
        }
        outcome.await.unwrap_or_else(|_| refused("the ACP threads stopped"))
    }

    /// What comes of intent `id` on the ACP thread `state`, from `by` when it answers: done when
    /// it went to the agent (or to an agent started again for it), and why not when it did not.
    /// Meant to run once per id, as [`Host::intent`] runs its decision.
    pub fn decide(
        &self,
        state: &ThreadState,
        intent: IntentId,
        ask: &Intent,
        by: Answerer,
    ) -> Outcome {
        let live = state.status.liveness == Liveness::Live;
        // An ACP permission's answer carries no words: a reason given with it goes as the
        // person's next message, queued behind the turn under way.
        let reason = match ask {
            Intent::Answer { message: Some(why), .. } if !why.trim().is_empty() => {
                Some(ThreadAsk::Send { text: why.trim().to_owned(), intent })
            }
            _ => None,
        };
        let asked = match ask {
            Intent::Send { delivery: Delivery::Steer, .. } => {
                return Outcome::Unsupported { cap: Cap::named(Cap::STEER) };
            }
            Intent::Send { attachments, .. } if !attachments.is_empty() => {
                return refused("Files are not sent to an ACP agent yet");
            }
            Intent::Send { text, .. } if text.trim().is_empty() => {
                return refused("There is nothing to send");
            }
            Intent::Send { .. } if !live && !driven::resumable(&state.meta) => {
                return refused("The agent cannot take this session up again; start a new thread");
            }
            Intent::Send { text, .. } => ThreadAsk::Send { text: text.clone(), intent },
            Intent::Withdraw { pending } | Intent::Edit { pending, .. }
                if !state.pending.iter().any(|p| p.intent == *pending) =>
            {
                return refused("That message is not waiting");
            }
            Intent::Withdraw { pending } => ThreadAsk::Withdraw { intent: *pending },
            Intent::Edit { pending, text } => {
                ThreadAsk::Edit { intent: *pending, text: text.clone() }
            }
            Intent::Interrupt => {
                if !live || !matches!(state.status.phase, Phase::Working | Phase::NeedsYou) {
                    return refused("The agent is not working");
                }
                ThreadAsk::Interrupt
            }
            Intent::Answer { ask, choice, .. } => {
                let Some(request) = state.requests.iter().find(|r| r.id == *ask) else {
                    return refused(&format!("There is no request {}", ask.0));
                };
                match &request.state {
                    RequestState::Open => {}
                    RequestState::Answered { .. } => return Outcome::Done,
                    RequestState::Released | RequestState::Withdrawn => {
                        return refused("The agent no longer asks this");
                    }
                }
                if !request.options.iter().any(|o| o.id == *choice) {
                    return refused(&format!("There is no answer {choice}"));
                }
                ThreadAsk::Answer { ask: ask.clone(), choice: choice.clone(), by }
            }
            Intent::Release { .. } => {
                return refused("An ACP agent has no prompt of its own while Slopty drives it");
            }
            Intent::SetModel { model } => {
                if !state.meta.models.iter().any(|m| m.id == *model) {
                    return refused(&format!("There is no model {model} here"));
                }
                ThreadAsk::SetModel { model: model.clone() }
            }
            Intent::SetMode { mode } => ThreadAsk::SetMode { mode: mode.clone() },
            other => return Outcome::Unsupported { cap: Cap::named(other.needs()) },
        };
        if !live && !matches!(asked, ThreadAsk::Send { .. }) {
            return refused("The agent is not running");
        }
        let thread = state.meta.id;
        for ask in std::iter::once(asked).chain(reason) {
            if self.0.send(Ask::Thread { thread, ask }).is_err() {
                return refused("ACP threads are not served here");
            }
        }
        Outcome::Done
    }
}

/// Whether `state` is the thread of an ACP agent, which this worker drives.
#[must_use]
pub fn is_acp(state: &ThreadState) -> bool {
    slopty_agent::acp::name_of(&state.meta.agent).is_some()
}

/// Serve the ACP threads in `host`, taking the asks of their [`Acp`].
///
/// An agent's program is looked for on `path` alone when it is given, else as the person's
/// terminal finds it ([`crate::facts::installed`]); the agents are the registry's and the
/// person's `own`. A thread an agent of an earlier worker drove is told exited first, since that
/// agent ended with it.
pub fn spawn(host: Host, path: Option<OsString>, own: Own, Asks(mut asks): Asks) -> JoinHandle<()> {
    tokio::spawn(async move {
        let (ended_tx, mut ended) = mpsc::unbounded_channel();
        let mut served =
            Served { host: host.clone(), path, own, running: HashMap::new(), ended: ended_tx };
        for thread in host.threads() {
            let Some((state, _)) = host.state(thread) else { continue };
            if is_acp(&state) && state.status.liveness == Liveness::Live {
                host.apply(thread, driven::gone(&state, WallMs::now()));
            }
        }
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

/// How to run an agent: the person's program, the `PATH` it runs with, and its arguments.
#[derive(Clone, Debug)]
struct Launcher {
    program: PathBuf,
    path: OsString,
    args: Vec<String>,
}

/// The ACP threads, as [`spawn`] serves them.
struct Served {
    host: Host,
    path: Option<OsString>,
    own: Own,
    /// The task of each thread's agent while it runs: the session's one writer.
    running: HashMap<ThreadId, mpsc::UnboundedSender<ThreadAsk>>,
    /// Told by a task when its agent has ended.
    ended: Ended,
}

impl Served {
    /// How to run the agent named `name`, or why it cannot be.
    async fn launcher(&self, name: &str) -> Result<Launcher, String> {
        // The settings are a file: read off the runtime.
        let own = Arc::clone(&self.own);
        let own = tokio::task::spawn_blocking(move || own()).await.unwrap_or_default();
        let agent = registry::find(name, &own)
            .ok_or_else(|| format!("No ACP agent {name} is known here"))?;
        let found = crate::facts::installed(&agent.program, self.path.clone())
            .await
            .ok_or_else(|| format!("{} is not installed", agent.program))?;
        Ok(Launcher { program: found.program, path: found.path, args: agent.args })
    }

    /// Start the thread of intent `id` as `start` says, once.
    async fn begin(&mut self, id: IntentId, start: &Start) -> Outcome {
        if let Some(outcome) = self.host.started(id) {
            return outcome;
        }
        let name = slopty_agent::acp::name_of(&start.agent).unwrap_or_default().to_owned();
        let found = self.launcher(&name).await;
        let mut begun = None;
        // What is refused is refused once, as what is started is started once.
        let outcome = self.host.start(id, || {
            if name.is_empty() {
                return Err(format!("{} is no ACP agent", start.agent.0));
            }
            if start.drive.as_ref().is_some_and(|d| !d.is(Drive::DRIVEN)) {
                return Err("An ACP agent is only driven".to_owned());
            }
            if !start.args.is_empty() {
                return Err(
                    "An ACP agent takes no arguments from a start; name its command line in the settings"
                        .to_owned(),
                );
            }
            if !Path::new(&start.cwd).is_dir() {
                return Err(format!("There is no folder {} here", start.cwd));
            }
            let launch = found.clone()?;
            let agent = slopty_agent::acp::agent_id(&name);
            let (session, actions) =
                Session::new(agent, driven::thread_of(id), &start.cwd, WallMs::now());
            let meta = session.meta().clone();
            begun = Some((session, actions, launch));
            Ok(meta)
        });
        let (Outcome::Started { thread }, Some((session, actions, launch))) = (&outcome, begun)
        else {
            return outcome;
        };
        let thread = *thread;
        self.host.apply(thread, actions);
        let first = start
            .prompt
            .clone()
            .filter(|p| !p.trim().is_empty())
            .map(|text| ThreadAsk::Send { text, intent: id });
        let opening = Opening::New { model: start.model.clone() };
        self.run(&launch, thread, session, opening, first.into_iter().collect());
        outcome
    }

    /// `asks` for `thread`: to its agent while one runs. Else a message takes the thread up
    /// again, loading its session; anything else is dropped, its intent already answered.
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
        for ask in &dropped {
            tracing::debug!(%thread, ?ask, "an ask of an ACP agent that is not running");
        }
        if sends.is_empty() {
            return;
        }
        let Some((state, _)) = self.host.state(thread).filter(|(s, _)| is_acp(s)) else {
            return;
        };
        let session = Session::of(&ThreadState::new(state.meta.clone()), WallMs::now());
        let name = slopty_agent::acp::name_of(&state.meta.agent).unwrap_or_default().to_owned();
        match self.launcher(&name).await {
            Ok(launch) => self.run(&launch, thread, session, Opening::Load, sends),
            Err(why) => {
                tracing::warn!(%thread, "an ACP agent could not start: {why}");
                let mut session = session;
                self.host.apply(thread, session.exited(Some(&why), WallMs::now()));
            }
        }
    }

    /// Run the agent for `thread`, opening its session as `opening` says, and serve it until it
    /// ends; `first` goes once the session is open.
    fn run(
        &mut self,
        launch: &Launcher,
        thread: ThreadId,
        session: Session,
        opening: Opening,
        first: Vec<ThreadAsk>,
    ) {
        let mut session = session;
        let cwd = session.meta().cwd.clone();
        match start_agent(launch, &cwd, thread) {
            Ok((stdin, agent)) => {
                let (tx, rx) = mpsc::unbounded_channel();
                let task = Task::new(self.host.clone(), thread, session, stdin, opening, first);
                tokio::spawn(task.serve(agent, rx, self.ended.clone()));
                self.running.insert(thread, tx);
            }
            Err(why) => {
                tracing::warn!(%thread, "{why}");
                self.host.apply(thread, session.exited(Some(&why), WallMs::now()));
            }
        }
    }
}

fn refused(reason: &str) -> Outcome {
    Outcome::Refused { reason: reason.to_owned() }
}
