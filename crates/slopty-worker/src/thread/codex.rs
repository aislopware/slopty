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
//! its next status change, and on a timer, since Codex tells a status change only to the
//! connections subscribed to the thread. When the daemon is not there,
//! or goes, the worker tries again every [`RETRY`].
//!
//! A thread that has rested for [`REST`], with no turn under way, nothing asked or held, nobody
//! following it and no TUI on it, is let go (`thread/unsubscribe`), so Codex can unload it with
//! its MCP servers. It stays in the table as it was. A follow ([`Codex::wake`]), anything asked of
//! it, or Codex saying it works on it again takes it up again with `thread/resume`, and what was
//! asked meanwhile goes once it is followed.
//!
//! What a thread says is Codex's word, read again where it may have gone stale: a thread followed
//! from a turn under way, whose earlier items Codex left out, is read whole (`thread/read`) once
//! that turn ends; one with a turn under way that has said nothing for [`QUIET`] is read again,
//! and hosted again as Codex has it if the turn ended unheard; and one the host keeps at work
//! that Codex no longer has loaded is taken up again as the worker joins Codex.
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
//! A message that cannot go, or that Codex refuses (its turn or its steer), is held in the
//! thread's list saying why, for the person to send again, change or take back: the mapping's
//! queue never holds those, so every list it shows keeps them.
//!
//! A thread Codex closes, and every thread when Codex goes, ends as Codex left it: what it asked
//! withdrawn, its turn under way stopped, exited and resumable. So does each thread an earlier
//! worker left at work when no Codex answers as this one starts; one that answers has them read
//! again instead. A thread left exited still takes a message, which takes it up again.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::Value;
use slopty_agent::attach::Attached;
use slopty_agent::codex::apps;
use slopty_agent::codex::daemon::{self, Daemon};
use slopty_agent::codex::protocol::{self as p, Method, RequestId, ServerNotification};
use slopty_agent::codex::rpc::{self, Incoming};
use slopty_agent::codex::shared::{Send, Setting, Settings, Shared};
use slopty_agent::queue::Queue;
use slopty_core::WallMs;
use slopty_proto::thread::wire::{Outcome, PastSession, Start};
use slopty_proto::thread::{
    Action, AgentId, Answerer, AskId, Delivery, Drive, Fork, IntentId, ItemBody, Link, Pending,
    PendingState, Phase, ThreadId, ThreadState, TurnId, TurnState,
};
use slopty_proto::visual::{AppAnswer, AppAsk};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

use super::{Host, Seated};

/// Why a message to a Codex thread did not go: the person's Codex holds no such thread.
const NOT_HELD: &str = "Codex no longer holds this thread, so the message did not go";

/// Why a message to a Codex thread is held again: Codex refused the turn or the steer that
/// carried it, in the words that follow.
pub const REFUSED: &str = "Codex didn't take the message";

/// How long the worker waits before it tries the daemon again.
pub const RETRY: Duration = Duration::from_secs(2);

/// How long a message written while Codex's app-server is away waits for it to come back.
///
/// An app-server that restarts, as an update restarts it, is back and has taken the thread up
/// again within seconds; past half a minute the person had better know the message did not go
/// than wonder where it is.
pub const HOLD_FOR: Duration = Duration::from_secs(30);

/// Why a message written while Codex's app-server was away did not go ([`HOLD_FOR`]).
pub const NOT_BACK: &str =
    "Codex's app-server didn't come back within half a minute, so the message did not go";

/// Why an answer given while Codex's app-server was away did not go: the request it answered
/// ended with the app-server.
pub const REQUEST_ENDED: &str =
    "Codex's request ended when its app-server stopped, so the answer did not go";

/// The name Slopty gives itself to the app-server.
const CLIENT: &str = "slopty";

/// How long Codex's daemon start may take: it answers once the app-server does.
pub const DAEMON_WAIT: Duration = Duration::from_secs(20);

/// How long a thread rests, with nobody following it, before the worker lets Codex unload it.
pub const REST: Duration = Duration::from_mins(10);

/// How long a turn under way may say nothing before its thread is read again (`thread/read`).
///
/// Its rest when that is shorter. Codex tells a connection only what it is subscribed to, so a
/// turn whose end went unheard would otherwise say it works for good.
pub const QUIET: Duration = Duration::from_secs(90);

/// How often a thread Codex could not take up again is tried again, [`RETRY`] apart.
const RESUME_TRIES: u8 = 10;

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

/// What a client asks of a Codex thread.
#[derive(Debug)]
enum Ask {
    Answer {
        thread: ThreadId,
        ask: AskId,
        choice: String,
        by: Answerer,
        /// The intent that gave it, to say under should it not go.
        intent: IntentId,
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
        attachments: Vec<String>,
    },
    Promote {
        thread: ThreadId,
        intent: IntentId,
    },
    Move {
        thread: ThreadId,
        intent: IntentId,
        before: Option<IntentId>,
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
    Fork {
        thread: ThreadId,
        id: IntentId,
        /// The last turn the new thread shares; `None` for all of them.
        after: Option<TurnId>,
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
    /// What a page shown in the thread asks of its MCP App.
    App {
        thread: ThreadId,
        ask: AppAsk,
        reply: oneshot::Sender<AppAnswer>,
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
            | Self::Move { thread, .. }
            | Self::Interrupt { thread }
            | Self::Fork { thread, .. }
            | Self::Review { thread, .. }
            | Self::Set { thread, .. }
            | Self::App { thread, .. }
            | Self::Wake { thread } => Some(*thread),
            // A thread let go is not taken up again to be closed.
            Self::Start { .. } | Self::Sessions { .. } | Self::Close { .. } => None,
        }
    }
}

/// How the person's own `codex` is launched to bring its daemon up: the `PATH` it is looked
/// for on alone, when given.
#[derive(Clone, Debug)]
pub struct Launch {
    path: Option<OsString>,
}

impl Launch {
    /// `codex` found on `path` alone when it is given, else as the person's terminal finds it.
    #[must_use]
    pub const fn new(path: Option<OsString>) -> Self {
        Self { path }
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
}

/// What the daemon's start says back to the loop that launched it.
enum Said {
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

    /// Answer request `ask` of `thread` with `choice`, as `by`, for intent `intent`. The answer
    /// is the JSON-RPC Codex's TUI would send; the first one Codex hears settles the request.
    /// Given while Codex's app-server is away, it goes nowhere and the thread says so
    /// ([`REQUEST_ENDED`]).
    pub fn answer(
        &self,
        thread: ThreadId,
        ask: AskId,
        choice: String,
        by: Answerer,
        intent: IntentId,
    ) {
        let _gone = self.0.send(Ask::Answer { thread, ask, choice, by, intent });
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
        let (reply, outcome) = oneshot::channel();
        if self.0.send(Ask::Fork { thread, id, after, reply }).is_err() {
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

    /// What a page shown in `thread` asks of its MCP App, through Codex's own
    /// `mcpServer/resource/read` and `mcpServer/tool/call`.
    pub async fn app(&self, thread: ThreadId, ask: AppAsk) -> AppAnswer {
        let (reply, answer) = oneshot::channel();
        if self.0.send(Ask::App { thread, ask, reply }).is_err() {
            return AppAnswer::Refused("Codex threads are not served here".to_owned());
        }
        answer.await.unwrap_or_else(|_| AppAnswer::Refused("the Codex threads stopped".to_owned()))
    }

    /// Take back the message `intent` queued in `thread`.
    pub fn withdraw(&self, thread: ThreadId, intent: IntentId) {
        let _gone = self.0.send(Ask::Withdraw { thread, intent });
    }

    /// Make the message `intent` queued in `thread` say `text`, with the files at `attachments`.
    pub fn edit(&self, thread: ThreadId, intent: IntentId, text: String, attachments: Vec<String>) {
        let _gone = self.0.send(Ask::Edit { thread, intent, text, attachments });
    }

    /// Send the message `intent` queued in `thread` now, into the turn under way.
    pub fn promote(&self, thread: ThreadId, intent: IntentId) {
        let _gone = self.0.send(Ask::Promote { thread, intent });
    }

    /// Move the message `intent` queued in `thread` to just before `before`, or last.
    pub fn move_queued(&self, thread: ThreadId, intent: IntentId, before: Option<IntentId>) {
        let _gone = self.0.send(Ask::Move { thread, intent, before });
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

    /// Start a seated thread (an agent another started) as `start` says, once per seat: Codex loads
    /// it with the seat's variables in its commands and Slopty's tools among its MCP servers
    /// ([`slopty_agent::codex::shared::seated`]), the system text as its developer instructions
    /// (`developerInstructions`), the approval policy the start's mode names, and a sandbox that
    /// writes only in its workspace. Taken up again after Codex let it go, it is loaded the same.
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

/// A message kept while Codex's app-server is away, and since when.
struct Parked {
    at: Instant,
    ask: Ask,
}

/// Say of each message in `parked` kept for [`HOLD_FOR`] by `now` that it did not go.
fn expire(host: &Host, parked: &mut Vec<Parked>, now: Instant) {
    let (late, kept): (Vec<Parked>, Vec<Parked>) =
        parked.drain(..).partition(|p| now.duration_since(p.at) >= HOLD_FOR);
    *parked = kept;
    for Parked { ask, .. } in late {
        message_not_sent(host, ask, NOT_BACK);
    }
}

/// The message `ask` (a send, or an edit of one waiting) that did not go, for `reason`: held in
/// its thread's list for the person to send again ([`not_sent`]).
fn message_not_sent(host: &Host, ask: Ask, reason: &str) {
    match ask {
        Ask::Send { thread, text, attachments, delivery, intent } => {
            let pending =
                Pending { intent, text, attachments, delivery, state: PendingState::Sending };
            not_sent(host, thread, pending, reason);
        }
        Ask::Edit { thread, intent, text, attachments } => {
            let Some((state, _)) = host.state(thread) else { return };
            let Some(was) = state.pending.into_iter().find(|p| p.intent == intent) else {
                return;
            };
            not_sent(host, thread, Pending { text, attachments, ..was }, reason);
        }
        other => tracing::debug!(?other, "not a message"),
    }
}

/// The answer `choice` to request `ask` of `thread`, given as intent `intent` while Codex's
/// app-server was away: its request ended with it, so the answer is held in the thread's list
/// as words of the choice's label, for the person to send as a message should they still mean
/// it ([`REQUEST_ENDED`]).
fn answer_not_sent(host: &Host, thread: ThreadId, (ask, choice): (&AskId, &str), intent: IntentId) {
    let Some((state, _)) = host.state(thread) else { return };
    let label = state
        .requests
        .iter()
        .find(|r| r.id == *ask)
        .and_then(|r| r.options.iter().find(|o| o.id == choice))
        .map_or_else(|| choice.to_owned(), |o| o.label.clone());
    let pending = Pending {
        intent,
        text: label,
        attachments: Vec::new(),
        delivery: Delivery::Steer,
        state: PendingState::Sending,
    };
    not_sent(host, thread, pending, REQUEST_ENDED);
}

/// A message for `thread` that could not go, for `reason`: it is held in the thread's list of
/// messages on their way, saying why, for the person to send again, rather than lost.
fn not_sent(host: &Host, thread: ThreadId, pending: Pending, reason: &str) {
    let Some((state, _)) = host.state(thread) else {
        tracing::debug!(%thread, "a message for a thread the worker does not hold");
        return;
    };
    let mut list = state.pending;
    list.retain(|p| p.intent != pending.intent);
    list.push(Pending { state: PendingState::Held { reason: reason.to_owned() }, ..pending });
    host.apply(thread, vec![Action::PendingSet(list)]);
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
/// A start while no daemon runs brings it up through `launch`, where it is given; without it a
/// start then is refused.
pub fn spawn(
    host: Host,
    socket: PathBuf,
    launch: Option<Launch>,
    Asks { mut asks, rest }: Asks,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut socket = socket;
        let (said, mut hear) = mpsc::unbounded_channel();
        let open = Opened { launch, said };
        // The starts waiting on the daemon's start.
        let mut held: Vec<Ask> = Vec::new();
        // The messages written while no daemon answers, waiting for one ([`HOLD_FOR`]).
        let mut parked: Vec<Parked> = Vec::new();
        let mut bringing = Bringing::Idle;
        // Until the first try at the daemon: what an earlier worker left at work is read again
        // from Codex once it answers, and ends as it was left when it does not.
        let mut first = true;
        loop {
            let joined = connect(&socket).await;
            if std::mem::take(&mut first) && joined.is_err() {
                cut_short_at_work(&host);
            }
            match joined {
                Ok(ws) => {
                    bringing = Bringing::Idle;
                    let mut session = Session::new(host.clone(), ws, rest);
                    session.held = std::mem::take(&mut held);
                    expire(&host, &mut parked, Instant::now());
                    session.held.extend(std::mem::take(&mut parked).into_iter().map(|p| p.ask));
                    let ended = session.run(&mut asks, &mut hear).await;
                    session.exited();
                    let at = Instant::now();
                    parked.extend(session.abandon(WENT).into_iter().map(|ask| Parked { at, ask }));
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
            // start waits on the daemon's start. A message waits for Codex to come back, for a
            // while ([`HOLD_FOR`]), and an answer goes nowhere, as its request ended
            // with Codex.
            loop {
                tokio::select! {
                    () = tokio::time::sleep(RETRY) => {
                        expire(&host, &mut parked, Instant::now());
                        break;
                    }
                    ask = asks.recv() => match ask {
                        Some(ask @ (Ask::Send { .. } | Ask::Edit { .. })) => {
                            parked.push(Parked { at: Instant::now(), ask });
                        }
                        Some(Ask::Answer { thread, ask, choice, intent, .. }) => {
                            answer_not_sent(&host, thread, (&ask, &choice), intent);
                        }
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
                        Some(Ask::Fork { thread, id, reply, .. }) => {
                            let _gone = reply.send(once(&host, thread, id, refused(NOT_RUNNING.to_owned())));
                        }
                        Some(Ask::Sessions { reply, .. }) => {
                            let _gone = reply.send(Err(NOT_RUNNING.to_owned()));
                        }
                        Some(Ask::App { reply, .. }) => {
                            let _gone = reply.send(AppAnswer::Refused(NOT_RUNNING.to_owned()));
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
                    },
                }
            }
        }
    })
}

/// How Codex's daemon is brought up, and where its start is said.
struct Opened {
    launch: Option<Launch>,
    said: mpsc::UnboundedSender<Said>,
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
    /// A turn, a steer or an interrupt of `thread`; for a message, the message as it waited,
    /// held again should Codex refuse it ([`REFUSED`]).
    Turn {
        thread: ThreadId,
        went: Option<Pending>,
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
    /// A page's read of its app's resource `uri`, for whoever asked.
    AppRead {
        uri: String,
        reply: oneshot::Sender<AppAnswer>,
    },
    /// A page's call of its app's tool, for whoever asked.
    AppCall {
        reply: oneshot::Sender<AppAnswer>,
    },
    /// Codex thread `native`, let go.
    Unsubscribe {
        native: String,
    },
    /// Codex thread `native` read again whole, as Codex has it now.
    Reread {
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
    /// The thread stamped alive while this connection follows it.
    lease: super::host::Lease,
}

/// One connection to the daemon.
struct Session {
    host: Host,
    ws: Socket,
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
    /// The threads followed from a turn already under way whose earlier items Codex did not
    /// give: read again whole once that turn ends.
    partial: HashSet<String>,
    /// The threads being read again ([`Waiting::Reread`]).
    rereading: HashSet<String>,
    /// The threads Codex could not take up again yet, with how often that was tried and when
    /// to try next.
    retries: HashMap<String, (u8, Instant)>,
}

/// What a thread is loaded with: its seat's variables (none for the person's own) and Slopty's
/// tools.
fn tools(host: &Host, seated: Option<&Seated>) -> std::collections::BTreeMap<String, Value> {
    let env = seated.map(|seated| host.env_of(seated)).unwrap_or_default();
    slopty_agent::codex::shared::seated(&env, host.tools_cli().as_deref())
}

impl Session {
    fn new(host: Host, ws: Socket, rest: Duration) -> Self {
        Self {
            host,
            ws,
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
            partial: HashSet::new(),
            rereading: HashSet::new(),
            retries: HashMap::new(),
        }
    }

    /// Every start not answered is refused with `reason`: Codex went before it answered.
    /// What waited on this link when it went: starts and forks refused for `reason`, answers
    /// said not to have gone ([`REQUEST_ENDED`]); the messages, those queued behind a turn in
    /// each thread's own queue among them, to wait for the next link ([`HOLD_FOR`]). One the
    /// person's stop held stays held, saying so, for them to send again.
    fn abandon(&mut self, reason: &str) -> Vec<Ask> {
        let waiting = std::mem::take(&mut self.held)
            .into_iter()
            .chain(std::mem::take(&mut self.waking).into_values().flatten());
        let mut held = Vec::new();
        let mut messages = Vec::new();
        let queues = self
            .threads
            .values_mut()
            .map(|f| (f.id, f.shared.queue().take_all()))
            .chain(
                self.requeue
                    .drain()
                    .map(|(native, q)| (slopty_agent::codex::shared::thread_of(&native), q)),
            )
            .collect::<Vec<_>>();
        for (thread, queue) in queues {
            for (pending, _attached) in queue.into_held() {
                if pending.stopped() {
                    not_sent(&self.host, thread, pending, Pending::STOPPED);
                    continue;
                }
                let Pending { intent, text, attachments, delivery, .. } = pending;
                messages.push(Ask::Send { thread, text, attachments, delivery, intent });
            }
        }
        for ask in waiting {
            match ask {
                Ask::Start { id, reply, .. } => held.push((id, vec![reply])),
                ask @ (Ask::Send { .. } | Ask::Edit { .. }) => messages.push(ask),
                Ask::Answer { thread, ask, choice, intent, .. } => {
                    answer_not_sent(&self.host, thread, (&ask, &choice), intent);
                }
                other => self.unanswered(other, reason),
            }
        }
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
        messages
    }

    async fn run(
        &mut self,
        asks: &mut mpsc::UnboundedReceiver<Ask>,
        hear: &mut mpsc::UnboundedReceiver<Said>,
    ) -> Ended {
        let init = p::InitializeParams {
            capabilities: Some(apps::capabilities()),
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
            let retry = self.next_retry();
            let ended = tokio::select! {
                _ = sweep.tick() => self.let_go().await.err(),
                () = sleep_until(retry), if retry.is_some() => self.retry_due().await.err(),
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
                // A daemon's start that answers once the daemon was joined anyway is done with.
                Some(Said::Daemon(_)) = hear.recv() => None,
            };
            if let Some(why) = ended {
                return Ended::Link(why);
            }
        }
    }

    /// Every followed thread, and every one let go, is left exited and resumable, as Codex left
    /// it: Codex is not there to say more, so a turn under way is stopped and what it asked
    /// withdrawn ([`cut_short`]).
    fn exited(&self) {
        for followed in self.threads.values() {
            followed.lease.stop();
        }
        let unloaded =
            self.unloaded.iter().map(|native| slopty_agent::codex::shared::thread_of(native));
        for thread in self.threads.values().map(|f| f.id).chain(unloaded) {
            cut_short(&self.host, thread);
        }
    }

    /// Let Codex unload each thread that has rested [`Session::rest`] with nobody following it
    /// and no TUI on it: its MCP servers and whatever else Codex keeps for a loaded thread go.
    /// The thread stays in the table, and is taken up again once someone follows or asks.
    async fn let_go(&mut self) -> Result<(), String> {
        let now = Instant::now();
        let quiet: Vec<String> = self
            .threads
            .iter()
            .filter(|(_, f)| f.shared.current().is_some())
            .filter(|(_, f)| now.saturating_duration_since(f.heard) >= QUIET.min(self.rest))
            .map(|(native, _)| native.clone())
            .collect();
        for native in quiet {
            if let Some(followed) = self.threads.get_mut(&native) {
                followed.heard = now;
            }
            self.reread(native).await?;
        }
        let resting: Vec<String> = self
            .threads
            .iter()
            .filter(|(_, f)| now.saturating_duration_since(f.heard) >= self.rest)
            .filter(|(_, f)| f.shared.rests())
            .filter(|(_, f)| self.host.followers(f.id) == 0)
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

    /// When the next thread Codex could not take up again is due to be tried again.
    fn next_retry(&self) -> Option<Instant> {
        self.retries.values().map(|(_, at)| *at).min()
    }

    /// Try again each thread Codex could not take up again whose moment has come; one followed
    /// since needs no more tries. A try's answer schedules the next ([`Self::unresumed`]).
    async fn retry_due(&mut self) -> Result<(), String> {
        let threads = &self.threads;
        self.retries.retain(|native, _| !threads.contains_key(native));
        let now = Instant::now();
        let due: Vec<String> = self
            .retries
            .iter()
            .filter(|(_, (_, at))| *at <= now)
            .map(|(native, _)| native.clone())
            .collect();
        for native in due {
            // Not again until this try is answered.
            if let Some((_, at)) = self.retries.get_mut(&native) {
                *at = now.checked_add(self.rest).unwrap_or(now);
            }
            self.resume(native).await?;
        }
        Ok(())
    }

    /// End thread `thread` for its settled task: its turn is stopped, Codex lets it go, and its
    /// seat is forgotten. A thread let go already has nothing to end.
    async fn close(&mut self, thread: ThreadId) -> Result<(), String> {
        let Some(native) = self.native.get(&thread).cloned() else { return Ok(()) };
        if let Some(params) = self.followed(thread).and_then(|f| f.shared.interrupt()) {
            self.request(&params, Waiting::Turn { thread, went: None }).await?;
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
    fn unanswered(&self, ask: Ask, reason: &str) {
        match ask {
            Ask::Fork { thread, id, reply, .. } => {
                let _gone = reply.send(once(&self.host, thread, id, refused(reason.to_owned())));
            }
            ask @ (Ask::Send { .. } | Ask::Edit { .. }) => {
                message_not_sent(&self.host, ask, reason);
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
                tracing::warn!("Codex said what Slopty does not read: {e}");
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
                let followed = self.let_go_before(&loaded.data);
                for native in loaded.data {
                    // A message the worker kept while Codex was away waits for its thread's
                    // resume, rather than finding it not followed yet.
                    let thread = slopty_agent::codex::shared::thread_of(&native);
                    if self.host.holds(thread) && !self.threads.contains_key(&native) {
                        self.native.insert(thread, native.clone());
                        self.waking.entry(native.clone()).or_default();
                    }
                    self.resume(native).await?;
                }
                for native in followed {
                    // What is asked of it meanwhile waits for it, as for any thread let go.
                    self.waking.entry(native.clone()).or_default();
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
                        // Followed meanwhile (its own start answered first): what was heard since
                        // stands, rather than the snapshot this answer took before it.
                        if !self.threads.contains_key(&native) {
                            self.follow(&resumed.thread).await?;
                        }
                        let settings = Settings {
                            approval: resumed.approval_policy,
                            sandbox: &resumed.sandbox,
                            model: &resumed.model,
                            effort: resumed.reasoning_effort.as_deref(),
                        };
                        self.settled(&resumed.thread.id, &settings);
                        // A revert that undid the turn a message was held for leaves nothing
                        // to end, so what it held goes now.
                        self.start_queued(&native).await?;
                        for ask in self.waking.remove(&native).unwrap_or_default() {
                            self.ask(ask).await?;
                        }
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
            (Waiting::Reread { native }, Ok(result)) => {
                match rpc::response::<p::ThreadReadParams>(result) {
                    Ok(read) => self.reread_answered(native, &read.thread).await?,
                    Err(e) => {
                        tracing::debug!(%native, "Codex's read of a thread did not parse: {e}");
                        self.rereading.remove(&native);
                    }
                }
            }
            (Waiting::Reread { native }, Err(e)) => {
                tracing::debug!(%native, "Codex did not read a thread again: {e}");
                self.rereading.remove(&native);
            }
            (Waiting::Unarchive { native }, Ok(_)) => {
                self.resuming.remove(&native);
                self.resume(native).await?;
            }
            (Waiting::Unarchive { native }, Err(e)) => {
                tracing::debug!(%native, "Codex did not unarchive a thread: {e}");
                self.unresumed(native, &e.to_string());
            }
            // A message Codex refused is held in the thread's list saying so, for the person to
            // send again or take back, rather than gone.
            (Waiting::Turn { thread, went }, Err(e)) => {
                tracing::debug!(%thread, "Codex refused a turn: {e}");
                if let Some(went) = went {
                    self.not_sent(thread, went, &format!("{REFUSED}: {}", e.message));
                }
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
                            None => refused("Couldn't keep Codex's thread here".to_owned()),
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
                            None => refused("Couldn't keep Codex's fork here".to_owned()),
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
            (Waiting::AppRead { uri, reply }, Ok(result)) => {
                let answer = match rpc::response::<p::McpResourceReadParams>(result) {
                    Ok(said) => apps::read(&uri, &said),
                    Err(e) => AppAnswer::Refused(format!("Codex's answer did not read: {e}")),
                };
                let _gone = reply.send(answer);
            }
            (Waiting::AppCall { reply }, Ok(result)) => {
                let answer = match rpc::response::<p::McpServerToolCallParams>(result) {
                    Ok(said) => apps::called(said),
                    Err(e) => AppAnswer::Refused(format!("Codex's answer did not read: {e}")),
                };
                let _gone = reply.send(answer);
            }
            (Waiting::AppRead { reply, .. } | Waiting::AppCall { reply }, Err(e)) => {
                let _gone = reply.send(AppAnswer::Refused(format!("Codex said: {e}")));
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
            config: Some(tools(&self.host, seated.as_ref())),
            developer_instructions: seated.and_then(|s| s.system),
            ..p::ThreadResumeParams::default()
        };
        self.request(&params, Waiting::Resume { native }).await
    }

    /// Read Codex thread `native` again whole (`thread/read`), unless it already is.
    async fn reread(&mut self, native: String) -> Result<(), String> {
        if !self.rereading.insert(native.clone()) {
            return Ok(());
        }
        let params = p::ThreadReadParams { include_turns: Some(true), thread_id: native.clone() };
        self.request(&params, Waiting::Reread { native }).await
    }

    /// Codex thread `native` as Codex has it now, read again: a turn still under way leaves it
    /// as it is; else it is hosted again as Codex has it, its held messages kept, and one Codex
    /// no longer has loaded is let go, since nothing more of it reaches this connection.
    async fn reread_answered(&mut self, native: String, thread: &p::Thread) -> Result<(), String> {
        self.rereading.remove(&native);
        let running = matches!(thread.status, p::ThreadStatus::Active { .. })
            && thread.turns.last().is_some_and(|t| matches!(t.status, p::TurnStatus::InProgress));
        if running && !self.partial.contains(&native) {
            return Ok(());
        }
        if let Some(mut followed) = self.threads.remove(&native) {
            self.requeue.insert(native.clone(), followed.shared.queue().take_all());
        }
        self.partial.remove(&native);
        self.follow(thread).await?;
        if matches!(thread.status, p::ThreadStatus::NotLoaded) {
            return self.unload(native).await;
        }
        // The turn it held messages for has ended: the next goes, as after a resume.
        self.start_queued(&native).await
    }

    /// Followed Codex thread `native` is no longer loaded for this connection (Codex closed or
    /// unloaded it): it ends as Codex left it ([`cut_short`]) and counts as let go, so a follow
    /// or an ask takes it up again. What it held behind its turn goes now that no turn is under
    /// way: the thread is taken up again for it at once.
    async fn unload(&mut self, native: String) -> Result<(), String> {
        let Some(mut followed) = self.threads.remove(&native) else { return Ok(()) };
        followed.lease.stop();
        cut_short(&self.host, followed.id);
        let queued = followed.shared.queue().take_all();
        if queued.is_empty() {
            self.unloaded.insert(native);
            return Ok(());
        }
        self.requeue.insert(native.clone(), queued);
        self.resume(native).await
    }

    /// The next message followed thread `native` holds, sent as its turn once none is under way.
    async fn start_queued(&mut self, native: &str) -> Result<(), String> {
        let next = self
            .threads
            .get_mut(native)
            .and_then(|followed| followed.shared.next_queued().map(|next| (followed.id, next)));
        let Some((thread, (params, taken))) = next else { return Ok(()) };
        let intent = params.client_user_message_id.as_deref().and_then(|id| id.parse().ok());
        let went = self.listed(thread, intent);
        self.put(thread, taken);
        self.request(params.as_ref(), Waiting::Turn { thread, went }).await
    }

    /// Message `went` of followed thread `thread`, sent: into the turn under way or as a turn of
    /// its own, leaving the thread's list, or held behind the turn under way. One for a thread
    /// not followed is held in the list saying it did not go ([`NOT_HELD`]).
    async fn send_message(&mut self, thread: ThreadId, went: Pending) -> Result<(), String> {
        let attached = crate::thread::attach::read(&went.attachments).await;
        let Some(followed) = self.followed(thread) else {
            self.not_sent(thread, went, NOT_HELD);
            return Ok(());
        };
        let intent = went.intent;
        match followed.shared.send(&went.text, attached, went.delivery, intent) {
            Send::Start(params) => {
                self.gone_from_list(thread, intent);
                self.request(params.as_ref(), Waiting::Turn { thread, went: Some(went) }).await
            }
            Send::Steer(params) => {
                self.gone_from_list(thread, intent);
                self.request(params.as_ref(), Waiting::Turn { thread, went: Some(went) }).await
            }
            Send::Held(actions) => {
                self.put(thread, actions);
                // What a stop held goes again at rest, ahead of this message.
                let Some(native) = self.native.get(&thread).cloned() else { return Ok(()) };
                self.start_queued(&native).await
            }
        }
    }

    /// Message `intent` of `thread`, as its list shows it.
    fn listed(&self, thread: ThreadId, intent: Option<IntentId>) -> Option<Pending> {
        let intent = intent?;
        self.host
            .update(thread, |s| {
                (Vec::new(), s.pending.iter().find(|p| p.intent == intent).cloned())
            })
            .flatten()
    }

    /// The message `intent` of `thread` held in its list here, said not to have gone
    /// ([`kept_here`]), which Codex's queue never held: changed in the list by `change`, given
    /// the list and where it stands. Nothing when no such message is held here.
    fn held_here(
        &self,
        thread: ThreadId,
        intent: IntentId,
        change: impl FnOnce(&mut Vec<Pending>, usize),
    ) {
        self.host.update(thread, |state| {
            let Some(at) = state.pending.iter().position(|p| p.intent == intent && kept_here(p))
            else {
                return (Vec::new(), ());
            };
            let mut list = state.pending.clone();
            change(&mut list, at);
            (vec![Action::PendingSet(list)], ())
        });
    }

    /// Put what the codec made of `thread` into the host. A list of messages it shows is its
    /// queue alone: the messages held here, said not to have gone ([`kept_here`]), stay in it
    /// ahead of the queue, for the person to send again or take back.
    fn put(&self, thread: ThreadId, mut actions: Vec<Action>) {
        if actions.iter().any(|a| matches!(a, Action::PendingSet(_))) {
            let kept: Vec<Pending> = self
                .host
                .update(thread, |s| {
                    (Vec::new(), s.pending.iter().filter(|p| kept_here(p)).cloned().collect())
                })
                .unwrap_or_default();
            for action in &mut actions {
                let Action::PendingSet(list) = action else { continue };
                let missing: Vec<Pending> = kept
                    .iter()
                    .filter(|k| !list.iter().any(|p| p.intent == k.intent))
                    .cloned()
                    .collect();
                list.splice(0..0, missing);
            }
        }
        self.host.apply(thread, actions);
    }

    /// Codex thread `native` could not be taken up again, for `why`: what waited on it is
    /// refused, and it is tried again at its next status change and every [`RETRY`], up to
    /// [`RESUME_TRIES`] times. One the host still holds at work ends as Codex left it unsaid,
    /// its turn stopped, once the last try failed too, rather than working for good.
    fn unresumed(&mut self, native: String, why: &str) {
        let reason = format!("Codex couldn't take the thread up again: {why}");
        self.reopened(&native, &reason);
        if !self.threads.contains_key(&native) {
            // Codex tells a status change only to the connections subscribed to it, so one not
            // written yet (a subagent's first turn) is tried again on a timer of its own.
            let now = Instant::now();
            let tries = self.retries.entry(native.clone()).or_insert((0, now));
            tries.0 = tries.0.saturating_add(1);
            tries.1 = now.checked_add(RETRY).unwrap_or(now);
            if tries.0 > RESUME_TRIES {
                self.retries.remove(&native);
                let thread = slopty_agent::codex::shared::thread_of(&native);
                if self.host.update(thread, |state| (Vec::new(), at_work(state))) == Some(true) {
                    cut_short(&self.host, thread);
                }
            }
        }
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
    ///
    /// Those someone follows now are returned, to take up again at once: a client watching a
    /// thread while Codex restarts sees it go on, not stand still until it is followed again. So
    /// are those the host still holds at work (a turn under way, a request open): Codex no longer
    /// runs them, so what they say was left by a link that missed their end, and taking them up
    /// again reads them as Codex has them. They are let go again once they rest.
    fn let_go_before(&mut self, loaded: &[String]) -> Vec<String> {
        let kept = self.host.visit(|state| {
            let native = &state.meta.native;
            (is_shared(state) && !native.is_empty() && !loaded.contains(native))
                .then(|| (state.meta.id, native.clone(), at_work(state)))
        });
        let mut followed = Vec::new();
        for (thread, native, at_work) in kept {
            if self.threads.contains_key(&native) {
                continue;
            }
            self.native.insert(thread, native.clone());
            if at_work || self.host.followers(thread) > 0 {
                followed.push(native);
            } else {
                self.unloaded.insert(native);
            }
        }
        followed
    }

    /// A message for `thread` that could not go, for `reason`: it is held in the thread's
    /// list of messages on their way, saying why, for the person to send again, rather than
    /// lost.
    fn not_sent(&self, thread: ThreadId, pending: Pending, reason: &str) {
        not_sent(&self.host, thread, pending, reason);
    }

    /// Message `intent` of `thread` goes now: it leaves the thread's list of messages on their
    /// way, where it stood when it was queued behind a turn on a link that went.
    fn gone_from_list(&self, thread: ThreadId, intent: IntentId) {
        let Some((state, _)) = self.host.state(thread) else { return };
        if state.pending.iter().any(|p| p.intent == intent) {
            let list = state.pending.into_iter().filter(|p| p.intent != intent).collect();
            self.host.apply(thread, vec![Action::PendingSet(list)]);
        }
    }

    /// Host Codex thread `thread`, as it now stands.
    async fn follow(&mut self, thread: &p::Thread) -> Result<(), String> {
        let under_way = thread.turns.iter().any(|t| {
            matches!(t.status, p::TurnStatus::InProgress)
                && t.items_view.is_some_and(|v| v != p::TurnItemsView::Full)
        });
        if under_way {
            self.partial.insert(thread.id.clone());
        }
        let (shared, actions) = Shared::new(thread);
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
        // Followed again (a try that Codex took at last, say): let go no more.
        self.unloaded.remove(&thread.id);
        self.adopt(id, &actions);
        self.host.apply(id, actions);
        let lease = self.host.lease(id);
        let followed = Followed { id, shared, heard: Instant::now(), lease };
        self.threads.insert(thread.id.clone(), followed);
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
            self.put(id, late);
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
        // Codex closed the thread: nothing more of it reaches this connection until it is taken
        // up again, so it ends as Codex left it.
        if matches!(note, ServerNotification::ThreadClosed(_)) {
            return self.unload(native).await;
        }
        if matches!(note, ServerNotification::TurnCompleted(_)) && self.partial.remove(&native) {
            self.reread(native.clone()).await?;
        }
        match self.threads.get_mut(&native) {
            Some(followed) => {
                followed.heard = Instant::now();
                let id = followed.id;
                let actions = followed.shared.notification(note, now);
                self.adopt(id, &actions);
                if !actions.is_empty() {
                    self.put(id, actions);
                }
                // The turn under way ended: the message held for it goes as the next.
                self.start_queued(&native).await?;
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
            Ask::Fork { thread, id, after, reply } => {
                if let Some(first) = self.host.outcome(thread, id) {
                    let _gone = reply.send(first);
                    return Ok(());
                }
                if let Some((_, waiting)) = self.forking.get_mut(&id) {
                    waiting.push(reply);
                    return Ok(());
                }
                let mut params = match self.followed(thread).map(|f| f.shared.fork(after)) {
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
                params.config.get_or_insert_default().extend(tools(&self.host, None));
                self.forking.insert(id, (thread, vec![reply]));
                // A fork of the whole thread shares every turn it has now.
                let turn = after.or_else(|| {
                    self.host.state(thread).and_then(|(s, _)| s.last_turn().map(|t| t.id))
                });
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
            Ask::App { thread, ask, reply } => {
                let Some(native) = self.native.get(&thread).cloned() else {
                    let why = "Codex does not hold this thread".to_owned();
                    let _gone = reply.send(AppAnswer::Refused(why));
                    return Ok(());
                };
                match apps::request(&native, ask) {
                    Ok(apps::Request::Read(params)) => {
                        let uri = params.uri.clone();
                        self.request(&params, Waiting::AppRead { uri, reply }).await
                    }
                    Ok(apps::Request::Call(params)) => {
                        self.request(&params, Waiting::AppCall { reply }).await
                    }
                    Err(why) => {
                        let _gone = reply.send(AppAnswer::Refused(why));
                        Ok(())
                    }
                }
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
                        let config = params.config.get_or_insert_default();
                        config.extend(tools(&self.host, seated.as_deref()));
                        if let Some(seated) = seated.as_deref() {
                            params.developer_instructions.clone_from(&seated.system);
                            // A server task writes only in its workspace, at every level.
                            params.sandbox = Some(p::SandboxMode::WorkspaceWrite);
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
            Ask::Answer { thread, ask, choice, by, .. } => {
                let Some(followed) = self.followed(thread) else { return Ok(()) };
                let Some((id, result)) = followed.shared.answer(&ask, &choice, by) else {
                    tracing::debug!(%thread, ask = %ask.0, "no such Codex request or choice");
                    return Ok(());
                };
                let frame = rpc::answer(&id, &result).map_err(|e| e.to_string())?;
                self.send(frame).await
            }
            Ask::Send { thread, text, attachments, delivery, intent } => {
                let went =
                    Pending { intent, text, attachments, delivery, state: PendingState::Sending };
                if !self.native.contains_key(&thread) {
                    self.not_sent(thread, went, NOT_HELD);
                    return Ok(());
                }
                self.send_message(thread, went).await
            }
            Ask::Move { thread, intent, before } => {
                let moved =
                    self.followed(thread).and_then(|f| f.shared.queue().move_to(intent, before));
                match moved {
                    Some(shown) => self.put(thread, vec![shown]),
                    None => self.held_here(thread, intent, |list, at| {
                        let moved = list.remove(at);
                        let to = before
                            .and_then(|before| list.iter().position(|p| p.intent == before))
                            .unwrap_or(list.len());
                        list.insert(to, moved);
                    }),
                }
                Ok(())
            }
            Ask::Withdraw { thread, intent } => {
                let taken = self.followed(thread).and_then(|f| f.shared.queue().withdraw(intent));
                match taken {
                    Some(shown) => self.put(thread, vec![shown]),
                    None => self.held_here(thread, intent, |list, at| {
                        list.remove(at);
                    }),
                }
                Ok(())
            }
            Ask::Edit { thread, intent, text, attachments } => {
                let attached = crate::thread::attach::read(&attachments).await;
                let changed = self.followed(thread).and_then(|f| {
                    f.shared.queue().edit(intent, &text, attachments.clone(), attached)
                });
                match changed {
                    Some(shown) => self.put(thread, vec![shown]),
                    None => self.held_here(thread, intent, |list, at| {
                        if let Some(held) = list.get_mut(at) {
                            held.text = text;
                            held.attachments = attachments;
                        }
                    }),
                }
                Ok(())
            }
            Ask::Promote { thread, intent } => {
                let listed = self.listed(thread, Some(intent));
                let Some(followed) = self.followed(thread) else { return Ok(()) };
                let Some((send, actions)) = followed.shared.promote(intent) else {
                    // Held here, said not to have gone: it goes now, as the person's steer.
                    let Some(held) = listed.filter(kept_here) else { return Ok(()) };
                    self.held_here(thread, intent, |list, at| {
                        list.remove(at);
                    });
                    let went = Pending { delivery: Delivery::Steer, ..held };
                    return self.send_message(thread, went).await;
                };
                self.put(thread, actions);
                let went = listed.map(|p| Pending { delivery: Delivery::Steer, ..p });
                match send {
                    Send::Start(params) => {
                        self.request(params.as_ref(), Waiting::Turn { thread, went }).await
                    }
                    Send::Steer(params) => {
                        self.request(params.as_ref(), Waiting::Turn { thread, went }).await
                    }
                    Send::Held(actions) => {
                        self.put(thread, actions);
                        Ok(())
                    }
                }
            }
            Ask::Interrupt { thread } => {
                let Some((params, held)) = self.followed(thread).and_then(|f| f.shared.stop())
                else {
                    return Ok(());
                };
                if !held.is_empty() {
                    self.put(thread, held);
                }
                self.request(&params, Waiting::Turn { thread, went: None }).await
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

    fn followed(&mut self, thread: ThreadId) -> Option<&mut Followed> {
        let native = self.native.get(&thread)?;
        self.threads.get_mut(native)
    }
}

/// Until `at`, or for good with none.
async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Whether `pending` is held in its thread's list by the worker, said not to have gone
/// ([`not_sent`]): Codex's queue holds only what waits behind a turn, and what the person's
/// stop holds ([`Pending::STOPPED`]).
fn kept_here(pending: &Pending) -> bool {
    matches!(pending.state, PendingState::Held { .. }) && !pending.stopped()
}

/// `thread`, which Codex no longer runs for this worker, ended as Codex left it: what it asked
/// withdrawn, its calls and its turn under way stopped, exited and resumable.
fn cut_short(host: &Host, thread: ThreadId) {
    host.update(thread, |state| (slopty_agent::driven::cut_short(state, true, WallMs::now()), ()));
}

/// Each Codex thread the host keeps at work, ended as Codex left it unsaid ([`cut_short`]): an
/// earlier worker left it so, and no Codex answers to read it again from.
fn cut_short_at_work(host: &Host) {
    for thread in host.visit(|s| (is_shared(s) && at_work(s)).then_some(s.meta.id)) {
        cut_short(host, thread);
    }
}

/// Whether `state` says its agent is at work: a turn under way, a request open, or a phase
/// that only a running thread has.
fn at_work(state: &ThreadState) -> bool {
    matches!(state.status.phase, Phase::Working | Phase::NeedsYou | Phase::Waiting)
        || state.last_turn().is_some_and(|t| t.state == TurnState::Active)
        || state.open_requests().next().is_some()
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
