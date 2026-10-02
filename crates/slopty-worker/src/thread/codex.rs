//! Codex, shared, on the worker: the IO half of the adapter (`slopty_agent::codex` is the
//! codec).
//!
//! The worker is one more client of the person's Codex app-server daemon, beside the Codex
//! TUI: it joins the daemon's control socket, a WebSocket on a Unix socket, follows every
//! thread the daemon has loaded or starts, and puts what each says into the [`Host`]. What a
//! client asks of a Codex thread goes back as the JSON-RPC Codex's own TUI would send: an
//! approval's answer, a turn, a steer, an interrupt. Nothing is typed into the TUI.
//!
//! A thread is followed by `thread/resume`, which brings back what Codex holds of it and keeps
//! this connection subscribed, so the thread stays loaded while the worker runs. A thread is
//! resumable only once its first turn has written it, so one that is not yet is tried again at
//! its next status change, which the daemon tells every client. When the daemon is not there,
//! or goes, the worker tries again every [`RETRY`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::Value;
use slopty_agent::codex::protocol::{self as p, Method, RequestId, ServerNotification};
use slopty_agent::codex::rpc::{self, Incoming};
use slopty_agent::codex::shared::{Send, Shared};
use slopty_core::WallMs;
use slopty_proto::thread::{
    Action, Answerer, AskId, Delivery, IntentId, Liveness, Phase, Status, ThreadId, ThreadState,
};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

use super::Host;

/// How long the worker waits before it tries the daemon again.
pub const RETRY: Duration = Duration::from_secs(2);

/// The name Slopty gives itself to the app-server.
const CLIENT: &str = "slopty";

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
    Answer { thread: ThreadId, ask: AskId, choice: String, by: Answerer },
    Send { thread: ThreadId, text: String, delivery: Delivery, intent: IntentId },
    Interrupt { thread: ThreadId },
}

/// What is asked of the Codex threads. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Codex(mpsc::UnboundedSender<Ask>);

/// Where a [`Codex`]'s asks wait for [`spawn`].
#[derive(Debug)]
pub struct Asks(mpsc::UnboundedReceiver<Ask>);

impl Codex {
    /// A handle, and the asks [`spawn`] takes from it.
    #[must_use]
    pub fn channel() -> (Self, Asks) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self(tx), Asks(rx))
    }

    /// Answer request `ask` of `thread` with `choice`, as `by`. The answer is the JSON-RPC
    /// Codex's TUI would send; the first one Codex hears settles the request.
    pub fn answer(&self, thread: ThreadId, ask: AskId, choice: String, by: Answerer) {
        let _gone = self.0.send(Ask::Answer { thread, ask, choice, by });
    }

    /// Send `text` to `thread` as intent `intent`.
    pub fn send(&self, thread: ThreadId, text: String, delivery: Delivery, intent: IntentId) {
        let _gone = self.0.send(Ask::Send { thread, text, delivery, intent });
    }

    /// Stop the turn under way in `thread`.
    pub fn interrupt(&self, thread: ThreadId) {
        let _gone = self.0.send(Ask::Interrupt { thread });
    }
}

/// Follow every thread of the daemon at `socket` into `host`, and act on the asks of its
/// [`Codex`].
pub fn spawn(host: Host, socket: PathBuf, Asks(mut asks): Asks) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match connect(&socket).await {
                Ok(ws) => {
                    let mut session = Session::new(host.clone(), ws);
                    match session.run(&mut asks).await {
                        Ended::Asks => return,
                        Ended::Link(why) => {
                            tracing::debug!(socket = %socket.display(), "Codex went: {why}");
                        }
                    }
                    session.exited();
                }
                Err(why) => tracing::trace!(socket = %socket.display(), "no Codex: {why}"),
            }
            // What is asked while Codex is not there is not kept: its intent was answered.
            loop {
                tokio::select! {
                    () = tokio::time::sleep(RETRY) => break,
                    ask = asks.recv() => if ask.is_none() { return },
                }
            }
        }
    })
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
    Resume { native: String },
    Turn { thread: ThreadId },
}

/// A followed thread.
struct Followed {
    id: ThreadId,
    shared: Shared,
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
}

impl Session {
    fn new(host: Host, ws: Socket) -> Self {
        Self {
            host,
            ws,
            next: 0,
            waiting: HashMap::new(),
            threads: HashMap::new(),
            native: HashMap::new(),
            resuming: HashMap::new(),
        }
    }

    async fn run(&mut self, asks: &mut mpsc::UnboundedReceiver<Ask>) -> Ended {
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
        loop {
            let ended = tokio::select! {
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
            };
            if let Some(why) = ended {
                return Ended::Link(why);
            }
        }
    }

    /// Every followed thread is left exited and resumable: Codex is not there to say more.
    fn exited(&self) {
        for followed in self.threads.values() {
            let status = Status {
                phase: Phase::Idle,
                wait: None,
                liveness: Liveness::Exited { resumable: true },
                since_ms: WallMs::now(),
            };
            self.host.apply(followed.id, vec![Action::Status(status)]);
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
            }
            (Waiting::Initialize, Err(e)) => return Err(format!("initialize: {e}")),
            (Waiting::Loaded, Ok(result)) => {
                let loaded = rpc::response::<p::ThreadLoadedListParams>(result)
                    .map_err(|e| e.to_string())?;
                for native in loaded.data {
                    self.resume(native).await?;
                }
            }
            (Waiting::Resume { native }, Ok(result)) => {
                self.resuming.remove(&native);
                match rpc::response::<p::ThreadResumeParams>(result) {
                    Ok(resumed) => self.follow(&resumed.thread),
                    Err(e) => tracing::debug!(%native, "a Codex thread's resume did not read: {e}"),
                }
            }
            (Waiting::Resume { native }, Err(e)) => {
                // Not written yet: tried again at its next status change.
                tracing::trace!(%native, "a Codex thread is not resumable yet: {e}");
                self.resuming.insert(native, false);
            }
            (Waiting::Turn { thread }, Err(e)) => {
                tracing::debug!(%thread, "Codex refused a turn: {e}");
            }
            (Waiting::Loaded | Waiting::Turn { .. }, _) => {}
        }
        Ok(())
    }

    /// Follow Codex thread `native` from now on, unless it is already.
    async fn resume(&mut self, native: String) -> Result<(), String> {
        if self.threads.contains_key(&native) || self.resuming.get(&native) == Some(&true) {
            return Ok(());
        }
        self.resuming.insert(native.clone(), true);
        let params =
            p::ThreadResumeParams { thread_id: native.clone(), ..p::ThreadResumeParams::default() };
        self.request(&params, Waiting::Resume { native }).await
    }

    /// Host Codex thread `thread`, as it now stands.
    fn follow(&mut self, thread: &p::Thread) {
        let (shared, actions) = Shared::new(thread, None);
        let id = shared.meta().id;
        let begun = if self.host.holds(id) {
            self.host.reset(id, ThreadState::new(shared.meta().clone())).map(|_| ())
        } else {
            self.host.create(shared.meta().clone()).map(|_| ())
        };
        if let Err(e) = begun {
            tracing::warn!(thread = %id, "a Codex thread could not begin: {e}");
            return;
        }
        self.host.apply(id, actions);
        self.native.insert(id, thread.id.clone());
        self.threads.insert(thread.id.clone(), Followed { id, shared });
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
        let Some(native) = thread else { return Ok(()) };
        match self.threads.get_mut(&native) {
            Some(followed) => {
                let actions = followed.shared.notification(note, now);
                if !actions.is_empty() {
                    self.host.apply(followed.id, actions);
                }
            }
            None if matches!(note, ServerNotification::ThreadStatusChanged(_)) => {
                self.resume(native).await?;
            }
            None => {}
        }
        Ok(())
    }

    async fn ask(&mut self, ask: Ask) -> Result<(), String> {
        match ask {
            Ask::Answer { thread, ask, choice, by } => {
                let Some(followed) = self.followed(thread) else { return Ok(()) };
                let Some((id, result)) = followed.shared.answer(&ask, &choice, by) else {
                    tracing::debug!(%thread, ask = %ask.0, "no such Codex request or choice");
                    return Ok(());
                };
                let frame = rpc::answer(&id, &result).map_err(|e| e.to_string())?;
                self.send(frame).await
            }
            Ask::Send { thread, text, delivery, intent } => {
                let Some(followed) = self.followed(thread) else { return Ok(()) };
                match followed.shared.send(&text, delivery, intent) {
                    Send::Start(params) => {
                        self.request(params.as_ref(), Waiting::Turn { thread }).await
                    }
                    Send::Steer(params) => {
                        self.request(params.as_ref(), Waiting::Turn { thread }).await
                    }
                }
            }
            Ask::Interrupt { thread } => {
                let Some(params) = self.followed(thread).and_then(|f| f.shared.interrupt()) else {
                    return Ok(());
                };
                self.request(&params, Waiting::Turn { thread }).await
            }
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
    state.meta.agent.is(slopty_proto::thread::AgentId::CODEX)
        && state.meta.drive.is(slopty_proto::thread::Drive::SHARED)
}
