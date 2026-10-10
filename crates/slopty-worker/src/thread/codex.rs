//! Codex, shared, on the worker: the IO half of the adapter (`slopty_agent::codex` is the
//! codec).
//!
//! The worker is one more client of the person's Codex app-server daemon, beside the Codex
//! TUI: it joins the daemon's control socket, a WebSocket on a Unix socket, follows every
//! thread the daemon has loaded or starts, and puts what each says into the [`Host`]. What a
//! client asks of a Codex thread goes back as the JSON-RPC Codex's own TUI would send: an
//! approval's answer, a turn, a steer, an interrupt, a switch of the model, its reasoning effort
//! or the approval policy (`thread/settings/update`, for the thread's next turns, the TUI's
//! included). Nothing is typed into the TUI. The models a thread can switch to are Codex's own
//! list (`model/list`), asked once as the worker joins. A thread's commands are its skills
//! (`skills/list`, asked as it is followed and again on `skills/changed`), and its background
//! work is the commands Codex left running (`thread/backgroundTerminals/list`, asked as it is
//! followed and whenever a call or a turn ends).
//!
//! A thread is followed by `thread/resume`, which brings back what Codex holds of it and keeps
//! this connection subscribed, so the thread stays loaded while the worker runs. A thread is
//! resumable only once its first turn has written it, so one that is not yet is tried again at
//! its next status change, which the daemon tells every client. When the daemon is not there,
//! or goes, the worker tries again every [`RETRY`].
//!
//! A thread that has rested for [`REST`], with no turn under way, nothing asked or held, nobody
//! following it and no TUI on it, is let go (`thread/unsubscribe`), so Codex can unload it with
//! its MCP servers. It stays in the table as it was. A follow ([`Codex::wake`]), anything asked of
//! it, or Codex saying it works on it again takes it up again with `thread/resume`, and what was
//! asked meanwhile goes once it is followed.
//!
//! A thread is started from a client ([`Codex::start`], `ThreadRequest::Start` for agent `codex`)
//! as Codex's own TUI starts one: `thread/start` in the thread's folder, with nothing set but the
//! model the person picked, so the policies are their Codex configuration's. The connection that
//! starts a thread is subscribed to it, so it is followed from Codex's answer, and the start's
//! prompt goes as its first turn, marked with the start's intent. The TUI can join it as it joins
//! any thread of the daemon. A start that names a thread (`resume <thread>`, Codex's own words)
//! takes that thread up again with `thread/resume`, so it is followed under the same id.
//!
//! A start while no daemon runs brings the person's daemon up as Codex publishes it for remote
//! clients: `codex app-server daemon start`, with the person's own `codex` found as their login
//! shell finds it ([`Launch`]). The start waits while it runs and goes on once the daemon answers;
//! a second start meanwhile starts nothing more. Where there is no `codex`, or Codex says why the
//! daemon did not start, the start is refused in those words, and nothing tries again by itself.
//! Only a start does this: a worker alone never starts Codex.
//!
//! A message queued while a turn runs is held by the thread's mapping ([`Shared::send`]) and goes
//! as the next turn once Codex says the turn ended; it can be changed or taken back until then.
//! A request with nothing to answer in the GUI (a secret, a permissions grant, a page to open) is
//! handed to Codex's own TUI ([`Codex::release`]): `codex resume <thread>` in one of the worker's
//! terminals, in the thread's folder, which joins the running daemon as one more client. The
//! thread names that terminal while it runs, so a client shows it. With no daemon running it is
//! refused: only a start brings the daemon up.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::Value;
use slopty_agent::attach::Attached;
use slopty_agent::codex::daemon::{self, Daemon};
use slopty_agent::codex::protocol::{self as p, Method, RequestId, ServerNotification};
use slopty_agent::codex::rpc::{self, Incoming};
use slopty_agent::codex::shared::{Send, Setting, Settings, Shared};
use slopty_agent::queue::Queue;
use slopty_core::{SessionId, WallMs};
use slopty_proto::thread::wire::{Outcome, PastSession, Start};
use slopty_proto::thread::{
    Action, AgentId, Answerer, AskId, Delivery, Drive, Fork, IntentId, ItemBody, Link, Liveness,
    Pending, PendingState, Phase, Status, ThreadId, ThreadState, TurnId,
};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

use super::terminals::Terminals;
use super::{Host, Seated};

/// Why a message to a Codex thread did not go: the person's Codex holds no such thread.
const NOT_HELD: &str = "Codex no longer holds this thread, so the message did not go";

/// How long the worker waits before it tries the daemon again.
pub const RETRY: Duration = Duration::from_secs(2);

/// The name Slopty gives itself to the app-server.
const CLIENT: &str = "slopty";

/// How long Codex's daemon start may take: it answers once the app-server does.
pub const DAEMON_WAIT: Duration = Duration::from_secs(20);

/// How long a thread rests, with nobody following it, before the worker lets Codex unload it.
pub const REST: Duration = Duration::from_mins(10);

/// Why a start is refused where the person has no `codex`.
pub const NO_CODEX: &str = "Codex isn't installed on this machine";

/// Why a start is refused where the worker launches nothing of Codex's.
pub const NOT_RUNNING: &str = "Codex's app-server isn't running on this machine";

/// Why a start is refused when Codex went before it answered.
pub const WENT: &str = "Codex's app-server stopped before it answered";

/// Why a start is refused when Codex's daemon did not start, in Codex's `words`.
#[must_use]
pub fn daemon_failed(words: &str) -> String {
    format!("Codex's app-server didn't start: {words}")
}

/// Where the daemon of `codex_home` listens:
/// `$CODEX_HOME/app-server-control/app-server-control.sock`.
#[must_use]
pub fn socket_of(codex_home: &Path) -> PathBuf {
    codex_home.join("app-server-control").join("app-server-control.sock")
}

/// The person's `CODEX_HOME`: the variable, else `~/.codex`.
#[must_use]
pub fn codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
}

/// Why a request is not handed to Codex's own TUI while no daemon runs.
pub const NO_TUI: &str = "Codex isn't running on this machine, so its own terminal can't join";

/// Where a fork cuts the thread.
#[derive(Clone, Copy, Debug)]
enum Cut {
    /// Through this turn, or the whole thread.
    Through(Option<TurnId>),
    /// Before this turn: an edit from there.
    Before(TurnId),
}

/// What a client asks of a Codex thread.
#[derive(Debug)]
enum Ask {
    Answer {
        thread: ThreadId,
        ask: AskId,
        choice: String,
        by: Answerer,
    },
    Send {
        thread: ThreadId,
        text: String,
        attachments: Vec<String>,
        delivery: Delivery,
        intent: IntentId,
    },
    Withdraw {
        thread: ThreadId,
        intent: IntentId,
    },
    Edit {
        thread: ThreadId,
        intent: IntentId,
        text: String,
    },
    Promote {
        thread: ThreadId,
        intent: IntentId,
    },
    Interrupt {
        thread: ThreadId,
    },
    Start {
        id: IntentId,
        start: Box<Start>,
        seated: Option<Box<Seated>>,
        reply: oneshot::Sender<Outcome>,
    },
    Release {
        thread: ThreadId,
        id: IntentId,
        reply: oneshot::Sender<Outcome>,
    },
    Fork {
        thread: ThreadId,
        id: IntentId,
        cut: Cut,
        reply: oneshot::Sender<Outcome>,
    },
    Sessions {
        cwd: String,
        limit: u32,
        reply: oneshot::Sender<Result<Vec<PastSession>, String>>,
    },
    /// Codex's own reviewer over the changes of commit `sha`, in the thread.
    Review {
        thread: ThreadId,
        sha: String,
        title: String,
        reply: oneshot::Sender<Outcome>,
    },
    /// Someone follows the thread: Codex loads it again if it was let go.
    Wake {
        thread: ThreadId,
    },
    /// A server task's thread is settled: its turn is stopped and Codex lets it go, its
    /// thread kept to take up again, and its seat forgotten.
    Close {
        thread: ThreadId,
        done: oneshot::Sender<()>,
    },
    /// Switch a setting of the thread's for its next turns.
    Set {
        thread: ThreadId,
        setting: Setting,
    },
}

impl Ask {
    /// The thread it is about, when it is about one.
    const fn thread(&self) -> Option<ThreadId> {
        match self {
            Self::Answer { thread, .. }
            | Self::Send { thread, .. }
            | Self::Withdraw { thread, .. }
            | Self::Edit { thread, .. }
            | Self::Promote { thread, .. }
            | Self::Interrupt { thread }
            | Self::Release { thread, .. }
            | Self::Fork { thread, .. }
            | Self::Review { thread, .. }
            | Self::Set { thread, .. }
            | Self::Wake { thread } => Some(*thread),
            // A thread let go is not taken up again to be closed.
            Self::Start { .. } | Self::Sessions { .. } | Self::Close { .. } => None,
        }
    }
}

/// How the person's own `codex` is launched for a start or a request: the `PATH` it is looked
/// for on alone, when given, and the worker's terminals its TUI opens in, when there are any.
#[derive(Clone)]
pub struct Launch {
    terminals: Option<Arc<dyn Terminals>>,
    path: Option<OsString>,
}

impl std::fmt::Debug for Launch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Launch").field("path", &self.path).finish_non_exhaustive()
    }
}

impl Launch {
    /// `codex` found on `path` alone when it is given, else as the person's terminal finds it.
    #[must_use]
    pub const fn new(path: Option<OsString>) -> Self {
        Self { terminals: None, path }
    }

    /// Codex's TUI opened in `terminals`.
    #[must_use]
    pub fn with_terminals(mut self, terminals: Arc<dyn Terminals>) -> Self {
        self.terminals = Some(terminals);
        self
    }

    /// Bring the person's daemon up with Codex's own `codex app-server daemon start`: the
    /// socket it listens on, or why not, in words for the person.
    async fn daemon_start(&self) -> Result<PathBuf, String> {
        let codex = crate::facts::installed("codex", self.path.clone())
            .await
            .ok_or_else(|| NO_CODEX.to_owned())?;
        let mut command = tokio::process::Command::new(&codex.program);
        command
            .args(daemon::START)
            .env("PATH", &codex.path)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        // The daemon runs every thread's commands with its environment: it claims no terminal.
        for var in slopty_agent::codex::shared::TERMINAL_ENV {
            command.env_remove(var);
        }
        let ran = tokio::time::timeout(DAEMON_WAIT, command.output())
            .await
            .map_err(|elapsed| {
                daemon_failed(&format!("no answer in {} s ({elapsed})", DAEMON_WAIT.as_secs()))
            })?
            .map_err(|e| daemon_failed(&e.to_string()))?;
        let stdout = String::from_utf8_lossy(&ran.stdout);
        let stderr = String::from_utf8_lossy(&ran.stderr);
        match daemon::read(ran.status.success(), &stdout, &stderr) {
            Daemon::Started { socket } => Ok(socket),
            Daemon::Failed { message } => Err(daemon_failed(&message)),
        }
    }

    /// Open the person's own `codex resume <native>` in `cwd`: its terminal, or why not.
    async fn open(&self, native: &str, cwd: String) -> Result<SessionId, String> {
        let terminals =
            self.terminals.as_ref().ok_or_else(|| "there are no terminals here".to_owned())?;
        let codex = crate::facts::installed("codex", self.path.clone())
            .await
            .ok_or_else(|| NO_CODEX.to_owned())?;
        let command = vec![
            codex.program.to_string_lossy().into_owned(),
            "resume".to_owned(),
            native.to_owned(),
        ];
        let env = vec![("PATH".to_owned(), codex.path.to_string_lossy().into_owned())];
        terminals.open(command, cwd, env).await
    }
}

/// What something launched of Codex's says back to the loop that launched it.
enum Said {
    /// Its TUI opened on `thread` for intent `id`, or why not.
    Opened { thread: ThreadId, id: IntentId, opened: Result<SessionId, String> },
    /// The TUI's program ended.
    Closed { thread: ThreadId, session: SessionId },
    /// The daemon's start came to its socket, or to why not.
    Daemon(Result<PathBuf, String>),
}

/// What is asked of the Codex threads. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Codex(mpsc::UnboundedSender<Ask>);

/// Where a [`Codex`]'s asks wait for [`spawn`], and how long a thread rests before it is let go.
#[derive(Debug)]
pub struct Asks {
    asks: mpsc::UnboundedReceiver<Ask>,
    rest: Duration,
}

impl Asks {
    /// A thread is let go once it has rested `rest` ([`REST`] unless said).
    #[must_use]
    pub const fn rest_after(mut self, rest: Duration) -> Self {
        self.rest = rest;
        self
    }
}

impl Codex {
    /// A handle, and the asks [`spawn`] takes from it.
    #[must_use]
    pub fn channel() -> (Self, Asks) {
        let (tx, asks) = mpsc::unbounded_channel();
        (Self(tx), Asks { asks, rest: REST })
    }

    /// Answer request `ask` of `thread` with `choice`, as `by`. The answer is the JSON-RPC
    /// Codex's TUI would send; the first one Codex hears settles the request.
    pub fn answer(&self, thread: ThreadId, ask: AskId, choice: String, by: Answerer) {
        let _gone = self.0.send(Ask::Answer { thread, ask, choice, by });
    }

    /// Send `text` and the files at `attachments` to `thread` as intent `intent`.
    pub fn send(
        &self,
        thread: ThreadId,
        text: String,
        attachments: Vec<String>,
        delivery: Delivery,
        intent: IntentId,
    ) {
        let _gone = self.0.send(Ask::Send { thread, text, attachments, delivery, intent });
    }

    /// Branch a new thread off `thread` through turn `after`, or the whole of it, for intent
    /// `id`, once: started when Codex has made it and it is followed here.
    pub async fn fork(&self, thread: ThreadId, id: IntentId, after: Option<TurnId>) -> Outcome {
        self.cut(thread, id, Cut::Through(after)).await
    }

    /// Branch a new thread off `thread` with its turns before `turn` and none from it on, for
    /// intent `id`, once (`thread/fork` with `beforeTurnId`): an edit from that turn. The new
    /// thread says it came from the turn before it, or from the thread with no turn shared.
    pub async fn rewind(&self, thread: ThreadId, id: IntentId, turn: TurnId) -> Outcome {
        self.cut(thread, id, Cut::Before(turn)).await
    }

    async fn cut(&self, thread: ThreadId, id: IntentId, cut: Cut) -> Outcome {
        let (reply, outcome) = oneshot::channel();
        if self.0.send(Ask::Fork { thread, id, cut, reply }).is_err() {
            return refused("Codex threads are not served here".to_owned());
        }
        outcome.await.unwrap_or_else(|_| refused("the Codex threads stopped".to_owned()))
    }

    /// Ask Codex's own reviewer for the changes commit `sha` makes, in `thread`, as the
    /// person's turn (`review/start`, inline), named `title`: done once Codex has begun it.
    pub async fn review(&self, thread: ThreadId, sha: String, title: String) -> Outcome {
        let (reply, outcome) = oneshot::channel();
        if self.0.send(Ask::Review { thread, sha, title, reply }).is_err() {
            return refused("Codex threads are not served here".to_owned());
        }
        outcome.await.unwrap_or_else(|_| refused("the Codex threads stopped".to_owned()))
    }

    /// Codex's threads in folder `cwd`, the latest first, at most `limit`; why not, in words.
    ///
    /// # Errors
    ///
    /// When Codex's daemon is not running, or did not answer.
    pub async fn sessions(&self, cwd: String, limit: u32) -> Result<Vec<PastSession>, String> {
        let (reply, listed) = oneshot::channel();
        if self.0.send(Ask::Sessions { cwd, limit, reply }).is_err() {
            return Err("Codex threads are not served here".to_owned());
        }
        listed.await.unwrap_or_else(|_| Err("the Codex threads stopped".to_owned()))
    }

    /// Take back the message `intent` queued in `thread`.
    pub fn withdraw(&self, thread: ThreadId, intent: IntentId) {
        let _gone = self.0.send(Ask::Withdraw { thread, intent });
    }

    /// Make the message `intent` queued in `thread` say `text`.
    pub fn edit(&self, thread: ThreadId, intent: IntentId, text: String) {
        let _gone = self.0.send(Ask::Edit { thread, intent, text });
    }

    /// Send the message `intent` queued in `thread` now, into the turn under way.
    pub fn promote(&self, thread: ThreadId, intent: IntentId) {
        let _gone = self.0.send(Ask::Promote { thread, intent });
    }

    /// Switch `thread`'s `setting` for its next turns, the TUI's included
    /// (`thread/settings/update`).
    pub fn set(&self, thread: ThreadId, setting: Setting) {
        let _gone = self.0.send(Ask::Set { thread, setting });
    }

    /// Stop the turn under way in `thread`.
    pub fn interrupt(&self, thread: ThreadId) {
        let _gone = self.0.send(Ask::Interrupt { thread });
    }

    /// Someone follows `thread`: Codex takes it up again if the worker let it go.
    pub fn wake(&self, thread: ThreadId) {
        let _gone = self.0.send(Ask::Wake { thread });
    }

    /// Hand `thread`'s requests to Codex's own TUI for intent `id`, once: done when it runs on
    /// the thread in one of the worker's terminals, opened now unless it already was.
    pub async fn release(&self, thread: ThreadId, id: IntentId) -> Outcome {
        let (reply, outcome) = oneshot::channel();
        if self.0.send(Ask::Release { thread, id, reply }).is_err() {
            return refused("Codex threads are not served here".to_owned());
        }
        outcome.await.unwrap_or_else(|_| refused("the Codex threads stopped".to_owned()))
    }

    /// Start the thread of intent `id` as `start` says, once: its first turn is `start`'s
    /// prompt, sent as that intent.
    pub async fn start(&self, id: IntentId, start: Start) -> Outcome {
        let (reply, outcome) = oneshot::channel();
        if self.0.send(Ask::Start { id, start: Box::new(start), seated: None, reply }).is_err() {
            return refused("Codex threads are not served here".to_owned());
        }
        outcome.await.unwrap_or_else(|_| refused("the Codex threads stopped".to_owned()))
    }

    /// End thread `thread` for its settled task: its turn is stopped and Codex lets it go.
    pub async fn close(&self, thread: ThreadId) {
        let (done, closed) = oneshot::channel();
        if self.0.send(Ask::Close { thread, done }).is_ok() {
            let _done = closed.await;
        }
    }

    /// Start a server task's thread as `start` says, once per seat: Codex loads it with the
    /// seat's variables in its commands and Slopty's tools among its MCP servers
    /// ([`slopty_agent::codex::shared::seated`]), and the role as its developer instructions
    /// (`developerInstructions`). Taken up again after Codex let it go, it is loaded the same.
    pub async fn start_seated(&self, start: Start, seated: Seated) -> Outcome {
        let (reply, outcome) = oneshot::channel();
        let (id, seated) = (seated.intent(), Some(Box::new(seated)));
        if self.0.send(Ask::Start { id, start: Box::new(start), seated, reply }).is_err() {
            return refused("Codex threads are not served here".to_owned());
        }
        outcome.await.unwrap_or_else(|_| refused("the Codex threads stopped".to_owned()))
    }
}

const fn refused(reason: String) -> Outcome {
    Outcome::Refused { reason }
}

/// Intent `id` for `thread` came to `outcome`, noted once: the outcome it first had when it was
/// noted before.
fn once(host: &Host, thread: ThreadId, id: IntentId, outcome: Outcome) -> Outcome {
    host.intent(thread, id, |_| (outcome.clone(), Vec::new())).unwrap_or(outcome)
}

/// What a start of a Codex thread asks of Codex.
enum Begin {
    /// A new thread (`thread/start`).
    New(Box<p::ThreadStartParams>),
    /// Thread `native` taken up again (`thread/resume`).
    Resume(String),
}

/// What `start` asks of Codex, or why it is refused.
fn checked(start: &Start) -> Result<Begin, String> {
    if !start.agent.is(AgentId::CODEX) {
        return Err(format!("{} is not Codex", start.agent.0));
    }
    if start.drive.as_ref().is_some_and(|d| !d.is(Drive::SHARED)) {
        return Err("A Codex thread is shared with Codex's own TUI".to_owned());
    }
    if let Some(native) = slopty_agent::codex::shared::resumed(&start.args) {
        if start.prompt.as_ref().is_some_and(|p| !p.trim().is_empty())
            || !start.attachments.is_empty()
        {
            return Err(
                "A Codex thread taken up again takes its next message from the composer".to_owned()
            );
        }
        if start.mode.is_some() || start.effort.is_some() {
            return Err(
                "A Codex thread taken up again keeps its settings; switch them in its composer"
                    .to_owned(),
            );
        }
        return Ok(Begin::Resume(native.to_owned()));
    }
    if !start.args.is_empty() {
        return Err("Codex takes no arguments from a start but resume <thread>".to_owned());
    }
    if !Path::new(&start.cwd).is_dir() {
        return Err(format!("There is no folder {} here", start.cwd));
    }
    super::attach::check(&start.attachments)?;
    let params = slopty_agent::codex::shared::start(
        &start.cwd,
        start.model.as_deref(),
        start.mode.as_deref(),
        start.effort.as_deref(),
    )?;
    Ok(Begin::New(Box::new(params)))
}

/// Whether Codex refused a resume because the thread is archived: "session `<id>` is archived",
/// or its hint to run `codex unarchive`. Codex says so only in words, so they are read as T3
/// Code reads them; any other refusal stays one.
fn archived(message: &str) -> bool {
    let message = message.to_lowercase();
    let words: Vec<&str> = message.split_whitespace().collect();
    message.contains("codex unarchive")
        || words.windows(4).any(|w| {
            w.first() == Some(&"session")
                && w.get(2) == Some(&"is")
                && w.get(3).is_some_and(|last| last.trim_end_matches(['.', ',', ';']) == "archived")
        })
}

/// Each start in `held` refused with `reason`, once, and told to whoever waits on it.
fn refuse_held(host: &Host, held: &mut Vec<Ask>, reason: &str) {
    for ask in held.drain(..) {
        if let Ask::Start { id, reply, .. } = ask {
            let _gone = reply.send(host.record_start(id, refused(reason.to_owned())));
        }
    }
}

/// Where the daemon's start stands while no daemon answers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Bringing {
    /// No start asked for it.
    Idle,
    /// Codex's start runs.
    Starting,
    /// Codex said its daemon is up: it is joined at once.
    Up,
}

/// Follow every thread of the daemon at `socket` into `host`, and act on the asks of its
/// [`Codex`].
///
/// A start while no daemon runs brings it up, and Codex's own TUI is opened on a thread,
/// through `launch`, where it is given; without it a start then is refused.
pub fn spawn(
    host: Host,
    socket: PathBuf,
    launch: Option<Launch>,
    Asks { mut asks, rest }: Asks,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut socket = socket;
        let (said, mut hear) = mpsc::unbounded_channel();
        let mut open = Opened { launch, said, running: HashMap::new(), waiting: HashMap::new() };
        // The starts waiting on the daemon's start.
        let mut held: Vec<Ask> = Vec::new();
        let mut bringing = Bringing::Idle;
        loop {
            match connect(&socket).await {
                Ok(ws) => {
                    bringing = Bringing::Idle;
                    let mut session = Session::new(host.clone(), ws, open, rest);
                    session.held = std::mem::take(&mut held);
                    let ended = session.run(&mut asks, &mut hear).await;
                    session.exited();
                    session.abandon(WENT);
                    open = session.into_tuis();
                    match ended {
                        Ended::Asks => return,
                        Ended::Link(why) => {
                            tracing::debug!(socket = %socket.display(), "Codex went: {why}");
                        }
                    }
                }
                Err(why) if bringing == Bringing::Up => {
                    bringing = Bringing::Idle;
                    let reason = daemon_failed(&format!("its socket didn't answer: {why}"));
                    refuse_held(&host, &mut held, &reason);
                }
                Err(why) => tracing::trace!(socket = %socket.display(), "no Codex: {why}"),
            }
            // What is asked while Codex is not there is not kept: its intent was answered. A
            // start waits on the daemon's start, and a request handed to the TUI is refused.
            loop {
                tokio::select! {
                    () = tokio::time::sleep(RETRY) => break,
                    ask = asks.recv() => match ask {
                        Some(Ask::Start { id, start, seated, reply }) => {
                            if let Some(first) = host.started(id) {
                                let _gone = reply.send(first);
                                continue;
                            }
                            held.push(Ask::Start { id, start, seated, reply });
                            if bringing != Bringing::Idle {
                                continue;
                            }
                            let Some(launch) = open.launch.clone() else {
                                refuse_held(&host, &mut held, NOT_RUNNING);
                                continue;
                            };
                            bringing = Bringing::Starting;
                            let said = open.said.clone();
                            tokio::spawn(async move {
                                let _gone = said.send(Said::Daemon(launch.daemon_start().await));
                            });
                        }
                        Some(Ask::Release { thread, id, reply }) => {
                            let _gone = reply.send(open.refuse(&host, thread, id, NO_TUI));
                        }
                        Some(Ask::Fork { thread, id, reply, .. }) => {
                            let _gone = reply.send(once(&host, thread, id, refused(NOT_RUNNING.to_owned())));
                        }
                        Some(Ask::Sessions { reply, .. }) => {
                            let _gone = reply.send(Err(NOT_RUNNING.to_owned()));
                        }
                        Some(_) => {}
                        None => return,
                    },
                    Some(heard) = hear.recv() => match heard {
                        Said::Daemon(Ok(up)) => {
                            if up != socket {
                                tracing::info!(
                                    was = %socket.display(), now = %up.display(),
                                    "Codex's daemon listens elsewhere"
                                );
                                socket = up;
                            }
                            bringing = Bringing::Up;
                            break;
                        }
                        Said::Daemon(Err(why)) => {
                            bringing = Bringing::Idle;
                            refuse_held(&host, &mut held, &why);
                        }
                        Said::Closed { thread, session } => {
                            if open.running.get(&thread) == Some(&session) {
                                open.running.remove(&thread);
                            }
                        }
                        // Opened as Codex went: the TUI runs all the same, and the thread names
                        // it once Codex is followed again.
                        Said::Opened { thread, id, opened: Ok(session) } => {
                            open.running.insert(thread, session);
                            let _done = open.settle(&host, thread, id, Outcome::Done);
                        }
                        Said::Opened { thread, id, opened: Err(why) } => {
                            let reason = format!("Codex's own terminal didn't open: {why}");
                            let _refused = open.refuse(&host, thread, id, &reason);
                        }
                    },
                }
            }
        }
    })
}

/// The TUIs opened on threads, and the opens under way.
struct Opened {
    launch: Option<Launch>,
    /// Where an open, a TUI's end or the daemon's start is said.
    said: mpsc::UnboundedSender<Said>,
    /// The terminal Codex's TUI runs in on each thread.
    running: HashMap<ThreadId, SessionId>,
    /// Who waits on each open under way.
    waiting: HashMap<IntentId, Vec<oneshot::Sender<Outcome>>>,
}

impl Opened {
    /// Intent `id` for `thread` refused with `reason`, once; told to whoever waits on it.
    fn refuse(&mut self, host: &Host, thread: ThreadId, id: IntentId, reason: &str) -> Outcome {
        self.settle(host, thread, id, refused(reason.to_owned()))
    }

    /// Intent `id` for `thread` came to `outcome`, noted once and told to whoever waits on it;
    /// the outcome it first had when it was noted before.
    fn settle(&mut self, host: &Host, thread: ThreadId, id: IntentId, outcome: Outcome) -> Outcome {
        let outcome = host.intent(thread, id, |_| (outcome.clone(), Vec::new())).unwrap_or(outcome);
        for reply in self.waiting.remove(&id).unwrap_or_default() {
            let _gone = reply.send(outcome.clone());
        }
        outcome
    }
}

type Socket = WebSocketStream<UnixStream>;

/// Join the daemon at `socket`. Its path in `CODEX_HOME` is a symlink to a socket in a short
/// directory, since a socket's own path must fit 104 bytes; the target is joined for the
/// same reason.
async fn connect(socket: &Path) -> Result<Socket, String> {
    let target = tokio::fs::canonicalize(socket).await.map_err(|e| e.to_string())?;
    let stream = UnixStream::connect(&target).await.map_err(|e| e.to_string())?;
    let (ws, _response) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .map_err(|e| e.to_string())?;
    Ok(ws)
}

/// Why a session ended.
enum Ended {
    /// The asks' handle is gone: the worker is shutting down.
    Asks,
    /// The connection went.
    Link(String),
}

/// What a request of ours is waiting to hear.
#[derive(Debug)]
enum Waiting {
    Initialize,
    Loaded,
    Resume {
        native: String,
    },
    Turn {
        thread: ThreadId,
    },
    /// A thread started for intent `id`, whose first turn is `prompt`, at a server task's
    /// seat when it has one.
    Start {
        id: IntentId,
        prompt: Option<String>,
        attachments: Vec<String>,
        seated: Option<Box<Seated>>,
    },
    /// A thread branched off `from` through `turn` for intent `id`.
    Fork {
        id: IntentId,
        from: ThreadId,
        turn: Option<TurnId>,
    },
    /// A review begun, for whoever asked.
    Review {
        reply: oneshot::Sender<Outcome>,
    },
    /// The threads of a folder, for whoever asked.
    List {
        reply: oneshot::Sender<Result<Vec<PastSession>, String>>,
    },
    /// Codex thread `native`, let go.
    Unsubscribe {
        native: String,
    },
    /// The models Codex offers, the pages before this one gathered.
    Models {
        gathered: Vec<p::Model>,
    },
    /// A switch of `thread`'s settings.
    Settings {
        thread: ThreadId,
        update: Box<p::ThreadSettingsUpdateParams>,
    },
    /// The goal Codex holds for Codex thread `native`.
    Goal {
        native: String,
    },
    /// Codex thread `native`, archived, put back so it can be taken up again.
    Unarchive {
        native: String,
    },
    /// The skills of Codex thread `native`'s folder, its composer's commands.
    Skills {
        native: String,
    },
    /// The commands Codex left running in the background on thread `native`, the pages before
    /// this one gathered.
    Terminals {
        native: String,
        gathered: Vec<p::ThreadBackgroundTerminal>,
    },
}

/// A followed thread.
struct Followed {
    id: ThreadId,
    shared: Shared,
    /// When Codex or a client last said anything of it.
    heard: Instant,
}

/// One connection to the daemon.
struct Session {
    host: Host,
    ws: Socket,
    /// Codex's own TUIs on the threads.
    tuis: Opened,
    next: i64,
    waiting: HashMap<i64, Waiting>,
    /// Followed threads, by Codex's id.
    threads: HashMap<String, Followed>,
    /// Codex's id of each followed thread.
    native: HashMap<ThreadId, String>,
    /// Threads being resumed, or to resume at their next status change.
    resuming: HashMap<String, bool>,
    /// Threads put back from Codex's archive to be taken up again: each is tried once.
    unarchived: HashSet<String>,
    /// Whether the handshake is done and the threads Codex holds are known, so a thread can be
    /// started or asked anything.
    ready: bool,
    /// Asks of a thread, and starts, asked before it was.
    held: Vec<Ask>,
    /// Who waits on each start Codex has not answered yet.
    starting: HashMap<IntentId, Vec<oneshot::Sender<Outcome>>>,
    /// The starts that take each Codex thread up again, answered once it is followed.
    reopening: HashMap<String, Vec<IntentId>>,
    /// The account's rate-limit windows as Codex last said them, for each thread followed
    /// from now on.
    limits: Option<p::RateLimitSnapshot>,
    /// The call that started each subagent's thread, by the subagent's thread, whether or not
    /// it is followed yet.
    parents: HashMap<ThreadId, Link>,
    /// Who waits on each fork Codex has not answered yet, and the thread it branches off.
    forking: HashMap<IntentId, (ThreadId, Vec<oneshot::Sender<Outcome>>)>,
    /// How long a thread rests, with nobody following it, before it is let go.
    rest: Duration,
    /// The threads let go: Codex may have unloaded them, and they are taken up again when
    /// someone follows or asks.
    unloaded: HashSet<String>,
    /// What was asked of each thread let go, sent once it is followed again.
    waking: HashMap<String, Vec<Ask>>,
    /// The models Codex offers, once it said, for each thread followed from now on.
    models: Option<Vec<p::Model>>,
    /// The messages each thread being read again held, held again once it is followed.
    requeue: HashMap<String, Queue<Vec<Attached>>>,
}

/// What a server task's thread is loaded with: the seat's variables and Slopty's tools.
fn tools(host: &Host, seated: &Seated) -> std::collections::BTreeMap<String, Value> {
    slopty_agent::codex::shared::seated(&host.env_of(seated), seated.relay.as_deref())
}

impl Session {
    fn new(host: Host, ws: Socket, tuis: Opened, rest: Duration) -> Self {
        Self {
            host,
            ws,
            tuis,
            next: 0,
            waiting: HashMap::new(),
            threads: HashMap::new(),
            native: HashMap::new(),
            resuming: HashMap::new(),
            unarchived: HashSet::new(),
            ready: false,
            held: Vec::new(),
            starting: HashMap::new(),
            reopening: HashMap::new(),
            limits: None,
            parents: HashMap::new(),
            forking: HashMap::new(),
            rest,
            unloaded: HashSet::new(),
            waking: HashMap::new(),
            models: None,
            requeue: HashMap::new(),
        }
    }

    /// Every start not answered is refused with `reason`: Codex went before it answered.
    fn abandon(&mut self, reason: &str) {
        let held = std::mem::take(&mut self.held).into_iter().filter_map(|ask| match ask {
            Ask::Start { id, reply, .. } => Some((id, vec![reply])),
            _ => None,
        });
        self.reopening.clear();
        for (id, (thread, waiting)) in self.forking.drain() {
            let outcome = once(&self.host, thread, id, refused(reason.to_owned()));
            for reply in waiting {
                let _gone = reply.send(outcome.clone());
            }
        }
        let starting: Vec<_> = self.starting.drain().chain(held).collect();
        for (id, waiting) in starting {
            let outcome = self.host.record_start(id, refused(reason.to_owned()));
            for reply in waiting {
                let _gone = reply.send(outcome.clone());
            }
        }
    }

    /// What it held of Codex's TUIs, for the next connection.
    fn into_tuis(self) -> Opened {
        self.tuis
    }

    async fn run(
        &mut self,
        asks: &mut mpsc::UnboundedReceiver<Ask>,
        hear: &mut mpsc::UnboundedReceiver<Said>,
    ) -> Ended {
        let init = p::InitializeParams {
            capabilities: Some(p::InitializeCapabilities {
                experimental_api: Some(true),
                ..p::InitializeCapabilities::default()
            }),
            client_info: p::ClientInfo {
                name: CLIENT.to_owned(),
                title: Some("Slopty".to_owned()),
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
        };
        if let Err(why) = self.request(&init, Waiting::Initialize).await {
            return Ended::Link(why);
        }
        let mut sweep = tokio::time::interval(
            self.rest.checked_div(4).unwrap_or(self.rest).max(Duration::from_millis(50)),
        );
        sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let ended = tokio::select! {
                _ = sweep.tick() => self.let_go().await.err(),
                frame = self.ws.next() => match frame {
                    Some(Ok(Message::Text(text))) => self.heard(&text).await.err(),
                    Some(Ok(Message::Close(_))) | None => Some("closed".to_owned()),
                    Some(Ok(_)) => None,
                    Some(Err(e)) => Some(e.to_string()),
                },
                ask = asks.recv() => match ask {
                    Some(ask) => self.ask(ask).await.err(),
                    None => return Ended::Asks,
                },
                Some(tui) = hear.recv() => {
                    self.tui(tui);
                    None
                }
            };
            if let Some(why) = ended {
                return Ended::Link(why);
            }
        }
    }

    /// Every followed thread, and every one let go, is left exited and resumable: Codex is not
    /// there to say more.
    fn exited(&self) {
        let unloaded =
            self.unloaded.iter().map(|native| slopty_agent::codex::shared::thread_of(native));
        for thread in self.threads.values().map(|f| f.id).chain(unloaded) {
            let status = Status {
                phase: Phase::Idle,
                wait: None,
                liveness: Liveness::Exited { resumable: true },
                since_ms: WallMs::now(),
            };
            self.host.apply(thread, vec![Action::Status(status)]);
        }
    }

    /// Let Codex unload each thread that has rested [`Session::rest`] with nobody following it
    /// and no TUI on it: its MCP servers and whatever else Codex keeps for a loaded thread go.
    /// The thread stays in the table, and is taken up again once someone follows or asks.
    async fn let_go(&mut self) -> Result<(), String> {
        let now = Instant::now();
        let resting: Vec<String> = self
            .threads
            .iter()
            .filter(|(_, f)| now.saturating_duration_since(f.heard) >= self.rest)
            .filter(|(_, f)| f.shared.rests())
            .filter(|(_, f)| {
                self.host.followers(f.id) == 0 && !self.tuis.running.contains_key(&f.id)
            })
            .map(|(native, _)| native.clone())
            .collect();
        for native in resting {
            self.threads.remove(&native);
            self.unloaded.insert(native.clone());
            let params = p::ThreadUnsubscribeParams { thread_id: native.clone() };
            self.request(&params, Waiting::Unsubscribe { native }).await?;
        }
        Ok(())
    }

    /// End thread `thread` for its settled task: its turn is stopped, Codex lets it go, and its
    /// seat is forgotten. A thread let go already has nothing to end.
    async fn close(&mut self, thread: ThreadId) -> Result<(), String> {
        let Some(native) = self.native.get(&thread).cloned() else { return Ok(()) };
        if let Some(params) = self.followed(thread).and_then(|f| f.shared.interrupt()) {
            self.request(&params, Waiting::Turn { thread }).await?;
        }
        if self.threads.remove(&native).is_some() {
            self.unloaded.insert(native.clone());
            let params = p::ThreadUnsubscribeParams { thread_id: native.clone() };
            self.request(&params, Waiting::Unsubscribe { native }).await?;
        }
        Ok(())
    }

    /// `ask`, held for a thread Codex could not take up again, answered with `reason` where
    /// someone waits on it; what was answered already is only noted.
    fn unanswered(&mut self, ask: Ask, reason: &str) {
        match ask {
            Ask::Fork { thread, id, reply, .. } => {
                let _gone = reply.send(once(&self.host, thread, id, refused(reason.to_owned())));
            }
            Ask::Release { thread, id, reply } => {
                let _gone = reply.send(self.tuis.refuse(&self.host, thread, id, reason));
            }
            Ask::Send { thread, text, attachments, delivery, intent } => {
                let pending =
                    Pending { intent, text, attachments, delivery, state: PendingState::Sending };
                self.not_sent(thread, pending, reason);
            }
            other => tracing::debug!(?other, "an ask of a Codex thread that did not come back"),
        }
    }

    async fn send(&mut self, text: String) -> Result<(), String> {
        self.ws.send(Message::text(text)).await.map_err(|e| e.to_string())
    }

    async fn request<M: Method>(&mut self, params: &M, waiting: Waiting) -> Result<(), String> {
        self.next = self.next.saturating_add(1);
        let id = self.next;
        let frame = rpc::request(&RequestId::Integer(id), params).map_err(|e| e.to_string())?;
        self.waiting.insert(id, waiting);
        self.send(frame).await
    }

    async fn heard(&mut self, text: &str) -> Result<(), String> {
        let incoming = match rpc::read(text) {
            Ok(incoming) => incoming,
            Err(e) => {
                tracing::debug!("Codex said what Slopty does not read: {e}");
                return Ok(());
            }
        };
        let now = WallMs::now();
        match incoming {
            Incoming::Answer { id, outcome } => {
                let RequestId::Integer(id) = id else { return Ok(()) };
                let Some(waiting) = self.waiting.remove(&id) else { return Ok(()) };
                self.answered(waiting, outcome).await?;
            }
            Incoming::Request { id, thread, request } => {
                // A thread not followed here is asked of its other clients as well.
                if let Some(followed) = thread.and_then(|native| self.threads.get_mut(&native)) {
                    followed.heard = Instant::now();
                    let actions = followed.shared.request(&id, &request, now);
                    self.host.apply(followed.id, actions);
                }
            }
            Incoming::UnknownRequest { id, method } => {
                let refusal = rpc::refuse(
                    &id,
                    rpc::METHOD_NOT_FOUND,
                    &format!("Slopty does not take {method}"),
                );
                self.send(refusal).await?;
            }
            Incoming::Notification { thread, note } => self.note(thread, &note, now).await?,
            Incoming::UnknownNotification { .. } => {}
        }
        Ok(())
    }

    async fn answered(
        &mut self,
        waiting: Waiting,
        outcome: Result<Value, rpc::RpcError>,
    ) -> Result<(), String> {
        match (waiting, outcome) {
            (Waiting::Initialize, Ok(_)) => {
                self.send(rpc::notification("initialized")).await?;
                let list = p::ThreadLoadedListParams::default();
                self.request(&list, Waiting::Loaded).await?;
                let models = p::ModelListParams::default();
                self.request(&models, Waiting::Models { gathered: Vec::new() }).await?;
            }
            (Waiting::Initialize, Err(e)) => return Err(format!("initialize: {e}")),
            (Waiting::Loaded, Ok(result)) => {
                let loaded = rpc::response::<p::ThreadLoadedListParams>(result)
                    .map_err(|e| e.to_string())?;
                self.let_go_before(&loaded.data);
                for native in loaded.data {
                    self.resume(native).await?;
                }
                self.joined().await?;
            }
            (Waiting::Loaded, Err(e)) => {
                tracing::debug!("Codex listed no loaded threads: {e}");
                self.joined().await?;
            }
            (Waiting::Resume { native }, Ok(result)) => {
                self.resuming.remove(&native);
                match rpc::response::<p::ThreadResumeParams>(result) {
                    Ok(resumed) => {
                        self.unarchived.remove(&native);
                        self.follow(&resumed.thread).await?;
                        let settings = Settings {
                            approval: resumed.approval_policy,
                            sandbox: &resumed.sandbox,
                            model: &resumed.model,
                            effort: resumed.reasoning_effort.as_deref(),
                        };
                        self.settled(&resumed.thread.id, &settings);
                        // A revert that undid the turn a message was held for leaves nothing
                        // to end, so what it held goes now.
                        let next = self.threads.get_mut(&native).and_then(|followed| {
                            followed.shared.next_queued().map(|next| (followed.id, next))
                        });
                        if let Some((thread, (params, taken))) = next {
                            self.host.apply(thread, taken);
                            self.request(params.as_ref(), Waiting::Turn { thread }).await?;
                        }
                        for ask in self.waking.remove(&native).unwrap_or_default() {
                            self.ask(ask).await?;
                        }
                        let goal = p::ThreadGoalGetParams { thread_id: native.clone() };
                        self.request(&goal, Waiting::Goal { native }).await?;
                    }
                    Err(e) => {
                        tracing::debug!(%native, "a Codex thread's resume did not read: {e}");
                        self.reopened(&native, &format!("Codex's answer did not read: {e}"));
                    }
                }
            }
            // Archived outside Slopty: put back from the archive and taken up again, once.
            (Waiting::Resume { native }, Err(e))
                if archived(&e.message) && self.unarchived.insert(native.clone()) =>
            {
                tracing::info!(%native, "a Codex thread was archived: unarchived to resume it");
                let params = p::ThreadUnarchiveParams { thread_id: native.clone() };
                self.request(&params, Waiting::Unarchive { native }).await?;
            }
            (Waiting::Resume { native }, Err(e)) => {
                // Not written yet: tried again at its next status change.
                tracing::trace!(%native, "a Codex thread is not resumable yet: {e}");
                self.unresumed(native, &e.to_string());
            }
            (Waiting::Unarchive { native }, Ok(_)) => {
                self.resuming.remove(&native);
                self.resume(native).await?;
            }
            (Waiting::Unarchive { native }, Err(e)) => {
                tracing::debug!(%native, "Codex did not unarchive a thread: {e}");
                self.unresumed(native, &e.to_string());
            }
            (Waiting::Goal { native }, Ok(result)) => {
                let read = rpc::response::<p::ThreadGoalGetParams>(result);
                match (read, self.threads.get(&native)) {
                    (Ok(read), Some(followed)) => {
                        if let Some(goal) = read.goal {
                            let goal = slopty_agent::codex::shared::goal_of(&goal);
                            self.host.apply(followed.id, vec![Action::GoalSet(Some(goal))]);
                        }
                    }
                    (Err(e), _) => tracing::debug!(%native, "Codex's goal did not read: {e}"),
                    (Ok(_), None) => {}
                }
            }
            (Waiting::Goal { native }, Err(e)) => {
                // A Codex built without goals.
                tracing::trace!(%native, "Codex gave no goal: {e}");
            }
            (Waiting::Turn { thread }, Err(e)) => {
                tracing::debug!(%thread, "Codex refused a turn: {e}");
            }
            (Waiting::Start { id, prompt, attachments, seated }, Ok(result)) => {
                let outcome = match rpc::response::<p::ThreadStartParams>(result) {
                    Ok(started) => {
                        if !self.threads.contains_key(&started.thread.id) {
                            self.follow(&started.thread).await?;
                        }
                        let settings = Settings {
                            approval: started.approval_policy,
                            sandbox: &started.sandbox,
                            model: &started.model,
                            effort: started.reasoning_effort.as_deref(),
                        };
                        self.settled(&started.thread.id, &settings);
                        match self.threads.get(&started.thread.id) {
                            Some(followed) => {
                                if let Some(seated) = seated.as_deref() {
                                    self.host.seated(followed.id, seated);
                                }
                                Outcome::Started { thread: followed.id }
                            }
                            None => refused("Codex's thread could not be kept here".to_owned()),
                        }
                    }
                    Err(e) => refused(format!("Codex's answer did not read: {e}")),
                };
                self.answer_start(id, outcome.clone());
                let first = prompt.or_else(|| (!attachments.is_empty()).then(String::new));
                if let (Outcome::Started { thread }, Some(text)) = (outcome, first) {
                    let ask = Ask::Send {
                        thread,
                        text,
                        attachments,
                        delivery: Delivery::Queue,
                        intent: id,
                    };
                    self.ask(ask).await?;
                }
            }
            (Waiting::Start { id, .. }, Err(e)) => {
                self.answer_start(id, refused(format!("Codex did not start the thread: {e}")));
            }
            (Waiting::Fork { id, from, turn }, Ok(result)) => {
                let outcome = match rpc::response::<p::ThreadForkParams>(result) {
                    Ok(forked) => {
                        if !self.threads.contains_key(&forked.thread.id) {
                            self.follow(&forked.thread).await?;
                        }
                        let settings = Settings {
                            approval: forked.approval_policy,
                            sandbox: &forked.sandbox,
                            model: &forked.model,
                            effort: forked.reasoning_effort.as_deref(),
                        };
                        self.settled(&forked.thread.id, &settings);
                        match self.threads.get(&forked.thread.id).map(|f| f.id) {
                            Some(thread) => {
                                self.host.forked(thread, Fork { thread: from, turn });
                                Outcome::Started { thread }
                            }
                            None => refused("Codex's fork could not be kept here".to_owned()),
                        }
                    }
                    Err(e) => refused(format!("Codex's answer did not read: {e}")),
                };
                self.answer_fork(id, outcome);
            }
            (Waiting::Fork { id, .. }, Err(e)) => {
                self.answer_fork(id, refused(format!("Codex did not fork the thread: {e}")));
            }
            (Waiting::Review { reply }, Ok(_)) => {
                let _gone = reply.send(Outcome::Done);
            }
            (Waiting::Review { reply }, Err(e)) => {
                let _gone = reply.send(refused(format!("Codex did not begin the review: {e}")));
            }
            (Waiting::List { reply }, Ok(result)) => {
                let listed = rpc::response::<p::ThreadListParams>(result)
                    .map(|listed| {
                        listed.data.iter().map(slopty_agent::codex::shared::past).collect()
                    })
                    .map_err(|e| format!("Codex's answer did not read: {e}"));
                let _gone = reply.send(listed);
            }
            (Waiting::List { reply }, Err(e)) => {
                let _gone = reply.send(Err(format!("Codex did not list its threads: {e}")));
            }
            (Waiting::Unsubscribe { native }, Ok(_)) => {
                tracing::debug!(%native, "a rested Codex thread is let go");
            }
            // Still subscribed, so still followed.
            (Waiting::Unsubscribe { native }, Err(e)) => {
                tracing::debug!(%native, "Codex kept a thread it was asked to let go: {e}");
                self.unloaded.remove(&native);
                self.resume(native).await?;
            }
            (Waiting::Models { mut gathered }, Ok(result)) => {
                match rpc::response::<p::ModelListParams>(result) {
                    Ok(page) => {
                        gathered.extend(page.data);
                        if let Some(cursor) = page.next_cursor {
                            let next =
                                p::ModelListParams { cursor: Some(cursor), ..Default::default() };
                            return self.request(&next, Waiting::Models { gathered }).await;
                        }
                        for followed in self.threads.values_mut() {
                            let actions = followed.shared.models(&gathered);
                            if !actions.is_empty() {
                                self.host.apply(followed.id, actions);
                            }
                        }
                        self.models = Some(gathered);
                    }
                    Err(e) => tracing::debug!("Codex's models did not read: {e}"),
                }
            }
            // Without the models a thread offers no switch of model or effort.
            (Waiting::Models { .. }, Err(e)) => tracing::debug!("Codex listed no models: {e}"),
            (Waiting::Settings { thread, update }, Ok(_)) => {
                if let Some(followed) = self.followed(thread) {
                    let actions = followed.shared.switched(&update);
                    if !actions.is_empty() {
                        self.host.apply(thread, actions);
                    }
                }
            }
            (Waiting::Settings { thread, .. }, Err(e)) => {
                if let Some(followed) = self.followed(thread) {
                    let actions = followed.shared.unswitched(&e.to_string(), WallMs::now());
                    self.host.apply(thread, actions);
                }
            }
            (Waiting::Skills { native }, Ok(result)) => {
                match rpc::response::<p::SkillsListParams>(result) {
                    Ok(said) => {
                        if let Some(followed) = self.threads.get_mut(&native) {
                            let actions = followed.shared.skills(&said);
                            self.host.apply(followed.id, actions);
                        }
                    }
                    Err(e) => tracing::debug!(%native, "Codex's skills did not read: {e}"),
                }
            }
            // Without its skills a thread offers no commands.
            (Waiting::Skills { native }, Err(e)) => {
                tracing::debug!(%native, "Codex listed no skills: {e}");
            }
            (Waiting::Terminals { native, mut gathered }, Ok(result)) => {
                match rpc::response::<p::ThreadBackgroundTerminalsListParams>(result) {
                    Ok(page) => {
                        gathered.extend(page.data);
                        let Some(followed) = self.threads.get_mut(&native) else { return Ok(()) };
                        if let Some(cursor) = page.next_cursor {
                            let next = p::ThreadBackgroundTerminalsListParams {
                                cursor: Some(cursor),
                                ..followed.shared.terminals_list()
                            };
                            return self
                                .request(&next, Waiting::Terminals { native, gathered })
                                .await;
                        }
                        let actions = followed.shared.terminals(&gathered, WallMs::now());
                        if !actions.is_empty() {
                            self.host.apply(followed.id, actions);
                        }
                    }
                    Err(e) => tracing::debug!(%native, "Codex's background work did not read: {e}"),
                }
            }
            // The tray keeps what it last showed.
            (Waiting::Terminals { native, .. }, Err(e)) => {
                tracing::debug!(%native, "Codex listed no background work: {e}");
            }
            (Waiting::Turn { .. }, _) => {}
        }
        Ok(())
    }

    /// Ask Codex for the skills of followed thread `native`'s folder.
    async fn ask_skills(&mut self, native: &str) -> Result<(), String> {
        let Some(followed) = self.threads.get(native) else { return Ok(()) };
        let params = followed.shared.skills_list();
        self.request(&params, Waiting::Skills { native: native.to_owned() }).await
    }

    /// Ask Codex for the commands it left running in the background on followed thread
    /// `native`.
    async fn ask_terminals(&mut self, native: &str) -> Result<(), String> {
        let Some(followed) = self.threads.get(native) else { return Ok(()) };
        let params = followed.shared.terminals_list();
        let waiting = Waiting::Terminals { native: native.to_owned(), gathered: Vec::new() };
        self.request(&params, waiting).await
    }

    /// Follow Codex thread `native` from now on, unless it is already.
    async fn resume(&mut self, native: String) -> Result<(), String> {
        if self.threads.contains_key(&native) || self.resuming.get(&native) == Some(&true) {
            return Ok(());
        }
        self.resuming.insert(native.clone(), true);
        let thread = slopty_agent::codex::shared::thread_of(&native);
        let seated = self.host.seated_of(thread);
        let params = p::ThreadResumeParams {
            thread_id: native.clone(),
            config: Some(
                seated
                    .as_ref()
                    .map_or_else(slopty_agent::codex::shared::unclaimed, |s| tools(&self.host, s)),
            ),
            developer_instructions: seated.and_then(|s| s.role),
            ..p::ThreadResumeParams::default()
        };
        self.request(&params, Waiting::Resume { native }).await
    }

    /// Codex thread `native` could not be taken up again, for `why`: what waited on it is
    /// refused, and it is tried again at its next status change.
    fn unresumed(&mut self, native: String, why: &str) {
        let reason = format!("Codex couldn't take the thread up again: {why}");
        self.reopened(&native, &reason);
        if let Some(held) = self.waking.remove(&native) {
            for ask in held {
                self.unanswered(ask, &reason);
            }
            self.unloaded.insert(native.clone());
        }
        self.resuming.insert(native, false);
    }

    /// The handshake is done and Codex's threads are known: what was asked meanwhile goes.
    async fn joined(&mut self) -> Result<(), String> {
        self.ready = true;
        for ask in std::mem::take(&mut self.held) {
            self.ask(ask).await?;
        }
        Ok(())
    }

    /// Every Codex thread the host keeps that is not among `loaded`, the threads Codex holds
    /// now, was let go before this link (by the worker before it restarted, or by Codex): it
    /// counts as let go, so a follow or a message takes it up again (`thread/resume`) rather
    /// than finding nothing to go to.
    fn let_go_before(&mut self, loaded: &[String]) {
        let kept = self.host.visit(|state| {
            let native = &state.meta.native;
            (is_shared(state) && !native.is_empty() && !loaded.contains(native))
                .then(|| (state.meta.id, native.clone()))
        });
        for (thread, native) in kept {
            if self.threads.contains_key(&native) {
                continue;
            }
            self.native.insert(thread, native.clone());
            self.unloaded.insert(native);
        }
    }

    /// A message for `thread` that could not go, for `reason`: it is held in the thread's
    /// list of messages on their way, saying why, for the person to send again, rather than
    /// lost.
    fn not_sent(&self, thread: ThreadId, pending: Pending, reason: &str) {
        let Some((state, _)) = self.host.state(thread) else {
            tracing::debug!(%thread, "a message for a thread the worker does not hold");
            return;
        };
        let mut list = state.pending;
        list.retain(|p| p.intent != pending.intent);
        list.push(Pending { state: PendingState::Held { reason: reason.to_owned() }, ..pending });
        self.host.apply(thread, vec![Action::PendingSet(list)]);
    }

    /// Host Codex thread `thread`, as it now stands.
    async fn follow(&mut self, thread: &p::Thread) -> Result<(), String> {
        let terminal = self.tuis.running.get(&slopty_agent::codex::shared::thread_of(&thread.id));
        let (shared, actions) = Shared::new(thread, terminal.copied());
        let id = shared.meta().id;
        let begun = if self.host.holds(id) {
            self.host.reset(id, ThreadState::new(shared.meta().clone())).map(|_| ())
        } else {
            self.host.create(shared.meta().clone()).map(|_| ())
        };
        if let Err(e) = begun {
            tracing::warn!(thread = %id, "a Codex thread could not begin: {e}");
            return Ok(());
        }
        self.native.insert(id, thread.id.clone());
        self.adopt(id, &actions);
        self.host.apply(id, actions);
        self.threads.insert(thread.id.clone(), Followed { id, shared, heard: Instant::now() });
        let mut late = Vec::new();
        if let Some(followed) = self.threads.get_mut(&thread.id) {
            if let Some(limits) = &self.limits {
                late.extend(followed.shared.rate_limits(limits));
            }
            if let Some(models) = &self.models {
                late.extend(followed.shared.models(models));
            }
            if let Some(queued) = self.requeue.remove(&thread.id) {
                late.extend(followed.shared.queue().requeue(queued));
            }
            if let Some(link) = self.parents.get(&id) {
                late.extend(followed.shared.adopted(link.clone()));
            }
        }
        if !late.is_empty() {
            self.host.apply(id, late);
        }
        for start in self.reopening.remove(&thread.id).unwrap_or_default() {
            self.answer_start(start, Outcome::Started { thread: id });
        }
        self.ask_skills(&thread.id).await?;
        self.ask_terminals(&thread.id).await
    }

    /// The starts that took Codex thread `native` up again, refused with `reason`.
    fn reopened(&mut self, native: &str, reason: &str) {
        for start in self.reopening.remove(native).unwrap_or_default() {
            self.answer_start(start, refused(reason.to_owned()));
        }
    }

    /// Codex's thread `native` runs with `settings`, as its start, resume or fork said.
    fn settled(&mut self, native: &str, settings: &Settings<'_>) {
        if let Some(followed) = self.threads.get_mut(native) {
            let actions = followed.shared.settings(settings);
            if !actions.is_empty() {
                self.host.apply(followed.id, actions);
            }
        }
    }

    /// The subagents `parent`'s `actions` started: each child thread is linked to its call,
    /// now if it is followed, else once it is.
    fn adopt(&mut self, parent: ThreadId, actions: &[Action]) {
        for action in actions {
            let (Action::ItemStarted(item)
            | Action::ItemUpdated(item)
            | Action::ItemCompleted(item)) = action
            else {
                continue;
            };
            let ItemBody::Tool(call) = &item.body else { continue };
            let Some(child) = call.child else { continue };
            let link = Link { thread: parent, item: item.id.clone() };
            if self.parents.get(&child) == Some(&link) {
                continue;
            }
            self.parents.insert(child, link.clone());
            let Some(native) = self.native.get(&child) else { continue };
            if let Some(followed) = self.threads.get_mut(native) {
                let adopted = followed.shared.adopted(link);
                if !adopted.is_empty() {
                    self.host.apply(child, adopted);
                }
            }
        }
    }

    async fn note(
        &mut self,
        thread: Option<String>,
        note: &ServerNotification,
        now: WallMs,
    ) -> Result<(), String> {
        if let ServerNotification::ThreadStarted(started) = note {
            return self.resume(started.thread.id.clone()).await;
        }
        if let ServerNotification::AccountRateLimitsUpdated(updated) = note {
            // The account's, not a thread's: every thread on it hears it.
            self.limits = Some(updated.rate_limits.clone());
            for followed in self.threads.values_mut() {
                let actions = followed.shared.rate_limits(&updated.rate_limits);
                if !actions.is_empty() {
                    self.host.apply(followed.id, actions);
                }
            }
            return Ok(());
        }
        if let ServerNotification::SkillsChanged(_) = note {
            // Codex names no thread: every followed one asks again for its folder's.
            let natives: Vec<String> = self.threads.keys().cloned().collect();
            for native in natives {
                self.ask_skills(&native).await?;
            }
            return Ok(());
        }
        let Some(native) = thread else { return Ok(()) };
        // A client rewrote the thread's history (`thread/revert`): it is read again whole, its
        // held messages kept.
        if matches!(note, ServerNotification::ThreadReverted(_))
            && let Some(mut followed) = self.threads.remove(&native)
        {
            self.requeue.insert(native.clone(), followed.shared.queue().take_all());
            return self.resume(native).await;
        }
        match self.threads.get_mut(&native) {
            Some(followed) => {
                followed.heard = Instant::now();
                let id = followed.id;
                let actions = followed.shared.notification(note, now);
                let next = followed.shared.next_queued();
                self.adopt(id, &actions);
                if !actions.is_empty() {
                    self.host.apply(id, actions);
                }
                // The turn under way ended: the message held for it goes as the next.
                if let Some((params, taken)) = next {
                    self.host.apply(id, taken);
                    self.request(params.as_ref(), Waiting::Turn { thread: id }).await?;
                }
                // Codex says nothing when a command left in the background exits, so its list
                // is read again whenever a call or a turn ends.
                if slopty_agent::codex::shared::terminals_due(note) {
                    self.ask_terminals(&native).await?;
                }
            }
            // A thread let go is taken up again only when it works: its unloading is no news.
            None if self.unloaded.contains(&native) => {
                if let ServerNotification::ThreadStatusChanged(changed) = note
                    && matches!(changed.status, p::ThreadStatus::Active { .. })
                {
                    self.unloaded.remove(&native);
                    self.resume(native).await?;
                }
            }
            None if matches!(note, ServerNotification::ThreadStatusChanged(_)) => {
                self.resume(native).await?;
            }
            None => {}
        }
        Ok(())
    }

    /// Start intent `id` came to `outcome`: noted once, and told to whoever waits on it.
    fn answer_start(&mut self, id: IntentId, outcome: Outcome) {
        let outcome = self.host.record_start(id, outcome);
        for reply in self.starting.remove(&id).unwrap_or_default() {
            let _gone = reply.send(outcome.clone());
        }
    }

    /// Fork intent `id` came to `outcome`: noted once on the thread it branched off, and told to
    /// whoever waits on it.
    fn answer_fork(&mut self, id: IntentId, outcome: Outcome) {
        let Some((from, waiting)) = self.forking.remove(&id) else { return };
        let outcome = once(&self.host, from, id, outcome);
        for reply in waiting {
            let _gone = reply.send(outcome.clone());
        }
    }

    async fn ask(&mut self, ask: Ask) -> Result<(), String> {
        // Which threads Codex let go is known only once it listed what it holds.
        if !self.ready && ask.thread().is_some() {
            self.held.push(ask);
            return Ok(());
        }
        if let Some(thread) = ask.thread()
            && let Some(native) = self.native.get(&thread).cloned()
        {
            if let Some(followed) = self.threads.get_mut(&native) {
                followed.heard = Instant::now();
            } else if self.unloaded.remove(&native) || self.waking.contains_key(&native) {
                // Let go: held until Codex has taken the thread up again.
                let held = self.waking.entry(native.clone()).or_default();
                if !matches!(ask, Ask::Wake { .. }) {
                    held.push(ask);
                }
                return self.resume(native).await;
            }
        }
        match ask {
            Ask::Wake { .. } => Ok(()),
            Ask::Close { thread, done } => {
                let result = self.close(thread).await;
                let _gone = done.send(());
                result
            }
            Ask::Fork { thread, id, cut, reply } => {
                if let Some(first) = self.host.outcome(thread, id) {
                    let _gone = reply.send(first);
                    return Ok(());
                }
                if let Some((_, waiting)) = self.forking.get_mut(&id) {
                    waiting.push(reply);
                    return Ok(());
                }
                let params = match self.followed(thread).map(|f| match cut {
                    Cut::Through(after) => f.shared.fork(after),
                    Cut::Before(turn) => f.shared.fork_before(turn),
                }) {
                    Some(Ok(params)) => params,
                    Some(Err(why)) => {
                        let _gone = reply.send(once(&self.host, thread, id, refused(why)));
                        return Ok(());
                    }
                    None => {
                        let why = refused("Codex does not hold this thread".to_owned());
                        let _gone = reply.send(once(&self.host, thread, id, why));
                        return Ok(());
                    }
                };
                self.forking.insert(id, (thread, vec![reply]));
                // A fork of the whole thread shares every turn it has now; one before a turn, the
                // turns before it.
                let state = self.host.state(thread).map(|(s, _)| s);
                let turn = match cut {
                    Cut::Through(Some(after)) => Some(after),
                    Cut::Through(None) => state.and_then(|s| s.last_turn().map(|t| t.id)),
                    Cut::Before(turn) => state.and_then(|s| {
                        s.turns.iter().take_while(|t| t.id != turn).last().map(|t| t.id)
                    }),
                };
                self.request(&params, Waiting::Fork { id, from: thread, turn }).await
            }
            Ask::Review { thread, sha, title, reply } => {
                let params = match self.followed(thread).map(|f| f.shared.review(&sha, &title)) {
                    Some(Ok(params)) => params,
                    Some(Err(why)) => {
                        let _gone = reply.send(refused(why));
                        return Ok(());
                    }
                    None => {
                        let _gone =
                            reply.send(refused("Codex does not hold this thread".to_owned()));
                        return Ok(());
                    }
                };
                self.request(&params, Waiting::Review { reply }).await
            }
            Ask::Sessions { cwd, limit, reply } => {
                let params = slopty_agent::codex::shared::list(&cwd, limit);
                self.request(&params, Waiting::List { reply }).await
            }
            Ask::Start { id, start, seated, reply } => {
                if let Some(first) = self.host.started(id) {
                    let _gone = reply.send(first);
                    return Ok(());
                }
                if let Some(waiting) = self.starting.get_mut(&id) {
                    waiting.push(reply);
                    return Ok(());
                }
                if !self.ready {
                    self.held.push(Ask::Start { id, start, seated, reply });
                    return Ok(());
                }
                self.starting.insert(id, vec![reply]);
                match checked(&start) {
                    Ok(Begin::New(mut params)) => {
                        if let Some(seated) = seated.as_deref() {
                            params.config = Some(tools(&self.host, seated));
                            params.developer_instructions.clone_from(&seated.role);
                        }
                        let prompt = start.prompt.filter(|p| !p.trim().is_empty());
                        let attachments = start.attachments;
                        let waiting = Waiting::Start { id, prompt, attachments, seated };
                        self.request(params.as_ref(), waiting).await
                    }
                    Ok(Begin::Resume(native)) => {
                        if let Some(thread) = self.threads.get(&native).map(|f| f.id) {
                            self.answer_start(id, Outcome::Started { thread });
                            return Ok(());
                        }
                        self.reopening.entry(native.clone()).or_default().push(id);
                        self.resume(native).await
                    }
                    Err(reason) => {
                        self.answer_start(id, refused(reason));
                        Ok(())
                    }
                }
            }
            Ask::Answer { thread, ask, choice, by } => {
                let Some(followed) = self.followed(thread) else { return Ok(()) };
                let Some((id, result)) = followed.shared.answer(&ask, &choice, by) else {
                    tracing::debug!(%thread, ask = %ask.0, "no such Codex request or choice");
                    return Ok(());
                };
                let frame = rpc::answer(&id, &result).map_err(|e| e.to_string())?;
                self.send(frame).await
            }
            Ask::Send { thread, text, attachments, delivery, intent } => {
                if !self.native.contains_key(&thread) {
                    let pending = Pending {
                        intent,
                        text,
                        attachments,
                        delivery,
                        state: PendingState::Sending,
                    };
                    self.not_sent(thread, pending, NOT_HELD);
                    return Ok(());
                }
                let attached = crate::thread::attach::read(&attachments).await;
                let Some(followed) = self.followed(thread) else { return Ok(()) };
                match followed.shared.send(&text, attached, delivery, intent) {
                    Send::Start(params) => {
                        self.request(params.as_ref(), Waiting::Turn { thread }).await
                    }
                    Send::Steer(params) => {
                        self.request(params.as_ref(), Waiting::Turn { thread }).await
                    }
                    Send::Held(actions) => {
                        self.host.apply(thread, actions);
                        // What a stop held goes again at rest, ahead of this message.
                        let next = self.followed(thread).and_then(|f| f.shared.next_queued());
                        let Some((params, taken)) = next else { return Ok(()) };
                        self.host.apply(thread, taken);
                        self.request(params.as_ref(), Waiting::Turn { thread }).await
                    }
                }
            }
            Ask::Withdraw { thread, intent } => {
                let taken = self.followed(thread).and_then(|f| f.shared.queue().withdraw(intent));
                if let Some(shown) = taken {
                    self.host.apply(thread, vec![shown]);
                }
                Ok(())
            }
            Ask::Edit { thread, intent, text } => {
                let changed =
                    self.followed(thread).and_then(|f| f.shared.queue().edit(intent, &text));
                if let Some(shown) = changed {
                    self.host.apply(thread, vec![shown]);
                }
                Ok(())
            }
            Ask::Promote { thread, intent } => {
                let Some(followed) = self.followed(thread) else { return Ok(()) };
                let Some((send, actions)) = followed.shared.promote(intent) else { return Ok(()) };
                self.host.apply(thread, actions);
                match send {
                    Send::Start(params) => {
                        self.request(params.as_ref(), Waiting::Turn { thread }).await
                    }
                    Send::Steer(params) => {
                        self.request(params.as_ref(), Waiting::Turn { thread }).await
                    }
                    Send::Held(actions) => {
                        self.host.apply(thread, actions);
                        Ok(())
                    }
                }
            }
            Ask::Release { thread, id, reply } => {
                self.release(thread, id, reply);
                Ok(())
            }
            Ask::Interrupt { thread } => {
                let Some((params, held)) = self.followed(thread).and_then(|f| f.shared.stop())
                else {
                    return Ok(());
                };
                if !held.is_empty() {
                    self.host.apply(thread, held);
                }
                self.request(&params, Waiting::Turn { thread }).await
            }
            Ask::Set { thread, setting } => {
                let Some(followed) = self.followed(thread) else { return Ok(()) };
                match followed.shared.switch(&setting) {
                    Ok(update) => {
                        let waiting =
                            Waiting::Settings { thread, update: Box::new(update.clone()) };
                        self.request(&update, waiting).await
                    }
                    Err(why) => {
                        let actions = followed.shared.unswitched(&why, WallMs::now());
                        self.host.apply(thread, actions);
                        Ok(())
                    }
                }
            }
        }
    }

    /// Hand `thread`'s requests to Codex's own TUI for intent `id`: done at once when it runs
    /// there already, else once it is opened, off this connection's loop.
    fn release(&mut self, thread: ThreadId, id: IntentId, reply: oneshot::Sender<Outcome>) {
        if let Some(first) = self.host.outcome(thread, id) {
            let _gone = reply.send(first);
            return;
        }
        if let Some(waiting) = self.tuis.waiting.get_mut(&id) {
            waiting.push(reply);
            return;
        }
        self.tuis.waiting.insert(id, vec![reply]);
        if self.tuis.running.contains_key(&thread) {
            self.tuis.settle(&self.host, thread, id, Outcome::Done);
            return;
        }
        let (Some(launch), Some(followed)) = (self.tuis.launch.clone(), self.followed(thread))
        else {
            let reason = "Codex's own terminal can't be opened here for this thread";
            self.tuis.refuse(&self.host, thread, id, reason);
            return;
        };
        let (native, cwd) =
            (followed.shared.meta().native.clone(), followed.shared.meta().cwd.clone());
        let said = self.tuis.said.clone();
        tokio::spawn(async move {
            let opened = launch.open(&native, cwd).await;
            if let (Ok(session), Some(terminals)) = (&opened, &launch.terminals) {
                let (session, ended, said) = (*session, terminals.exited(*session), said.clone());
                tokio::spawn(async move {
                    ended.await;
                    let _gone = said.send(Said::Closed { thread, session });
                });
            }
            let _gone = said.send(Said::Opened { thread, id, opened });
        });
    }

    /// What a TUI opened on a thread says. A daemon's start that answers once the daemon was
    /// joined anyway is done with.
    fn tui(&mut self, said: Said) {
        let (thread, terminal) = match said {
            Said::Daemon(_) => return,
            Said::Opened { thread, id, opened: Ok(session) } => {
                tracing::info!(%thread, %session, "Codex's own terminal opened");
                self.tuis.running.insert(thread, session);
                self.tuis.settle(&self.host, thread, id, Outcome::Done);
                (thread, Some(session))
            }
            Said::Opened { thread, id, opened: Err(why) } => {
                let reason = format!("Codex's own terminal didn't open: {why}");
                self.tuis.refuse(&self.host, thread, id, &reason);
                return;
            }
            Said::Closed { thread, session } => {
                if self.tuis.running.get(&thread) != Some(&session) {
                    return;
                }
                self.tuis.running.remove(&thread);
                (thread, None)
            }
        };
        let named = self.followed(thread).map(|f| f.shared.set_terminal(terminal));
        if let Some(actions) = named.filter(|a| !a.is_empty()) {
            self.host.apply(thread, actions);
        }
    }

    fn followed(&mut self, thread: ThreadId) -> Option<&mut Followed> {
        let native = self.native.get(&thread)?;
        self.threads.get_mut(native)
    }
}

/// Whether `state`'s thread is a Codex thread this adapter answers for.
#[must_use]
pub fn is_shared(state: &ThreadState) -> bool {
    state.meta.agent.is(AgentId::CODEX) && state.meta.drive.is(Drive::SHARED)
}

#[cfg(test)]
mod tests {
    use super::archived;

    /// Only Codex's words for an archived thread unarchive it: an archived workspace, a missing
    /// thread or an unrelated path under the archive stay refusals.
    #[test]
    fn only_an_archived_thread_is_unarchived() {
        assert!(archived("session saved-thread is archived"));
        assert!(archived("Session 019a-77 is archived."));
        assert!(archived("Run `codex unarchive saved-thread` to unarchive it first."));
        for other in [
            "thread not found",
            "no rollout found",
            "authentication failed",
            "workspace is archived",
            "permission denied reading archived_sessions/saved-thread",
        ] {
            assert!(!archived(other), "{other}");
        }
    }
}
