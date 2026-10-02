//! Claude Code, observed, on the worker: the IO half of the adapter
//! (`slopty_agent::observed` is the codec).
//!
//! It runs beside today's conversation path and changes nothing of it. It hears what every
//! client hears (the daemon's broadcast: each agent's status as the tracker merged it, the
//! permission prompts held), and what the hooks and the mod said of each session
//! ([`Sources::seen`]). A task per terminal session running Claude Code reads its transcripts
//! on the blocking pool, at once after a hook and on a [`TICK`] between, and puts what the codec
//! makes of it all into the [`Host`]: a thread per Claude Code session, one per subagent.
//!
//! A session's thread is begun when its own id is known (its first hook, or the transcript
//! file's name), since that id names the thread. Until then, a Claude Code the tracker sees in
//! the terminal (one started by hand, idle at its prompt) has a provisional thread named by the
//! terminal, removed once the session's own begins or the agent goes, since it never was a
//! session. A thread the host holds from before (a worker restart) starts over and is read
//! again from the transcript, under a new epoch. When the terminal moves to another Claude Code
//! session (`/clear`, `/resume`), the old thread is left exited and resumable, and the new one
//! begins. A thread declares `approvals` once a hook has been heard in its terminal.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slopty_agent::conversation::{Part, Transcripts};
use slopty_agent::observed::{self, Observed, Out};
use slopty_core::{SessionId, WallMs};
use slopty_proto::WorkerMsg;
use slopty_proto::agent::{AgentEvent, AgentKind, AgentStatus};
use slopty_proto::conversation::PermissionEvent;
use slopty_proto::thread::wire::{EXPANDED_CHARS, Expanded};
use slopty_proto::thread::{Action, ContentRef, Liveness, Phase, Status, ThreadState};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use super::Host;
use crate::conversation::Seen;

/// How often a session's transcripts are read between hooks: answers and thinking arrive with
/// no hook of their own.
pub const TICK: Duration = Duration::from_millis(250);

/// Where the daemon keeps what an observed session needs.
pub trait Sources: Send + Sync {
    /// Where the session's conversation is written.
    fn sources(&self, session: SessionId) -> crate::orchestrate::Sources;
    /// What the hooks and the mod said of the session, as it changes.
    fn seen(&self, session: SessionId) -> watch::Receiver<Seen>;
}

/// What a session's task hears.
#[derive(Debug)]
enum Input {
    Status(Box<AgentEvent>),
    Permission(PermissionEvent),
    /// The terminal's working directory, as its shell last said.
    Cwd(String),
    /// The terminal's title, where Claude Code names the session.
    Title(String),
    /// The whole of a clipped text or picture, asked through a [`Driver`].
    Expand(ContentRef, oneshot::Sender<Expanded>),
}

/// What is asked of the observed sessions beyond what the daemon's events tell them. Cheap to
/// clone.
#[derive(Clone, Debug)]
pub struct Driver(mpsc::UnboundedSender<(SessionId, Input)>);

/// Where a [`Driver`]'s asks wait for [`spawn`].
#[derive(Debug)]
pub struct Asks(mpsc::UnboundedReceiver<(SessionId, Input)>);

impl Driver {
    /// A driver, and the asks [`spawn`] takes from it.
    #[must_use]
    pub fn channel() -> (Self, Asks) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self(tx), Asks(rx))
    }

    /// The whole of `content`, clipped in a thread observed in terminal `session`:
    /// [`Expanded::Gone`] when its transcript no longer has it, or nothing is observed there.
    pub async fn expand(&self, session: SessionId, content: ContentRef) -> Expanded {
        let (tx, rx) = oneshot::channel();
        if self.0.send((session, Input::Expand(content, tx))).is_err() {
            return Expanded::Gone;
        }
        rx.await.unwrap_or(Expanded::Gone)
    }
}

/// Observe every Claude Code session the daemon's `events` speak of, into `host`, and answer
/// the `asks` of its [`Driver`].
pub fn spawn(
    host: Host,
    mut events: broadcast::Receiver<WorkerMsg>,
    sources: Arc<dyn Sources>,
    Asks(mut asks): Asks,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut sessions: HashMap<SessionId, mpsc::UnboundedSender<Input>> = HashMap::new();
        let mut cwds: HashMap<SessionId, String> = HashMap::new();
        let mut titles: HashMap<SessionId, String> = HashMap::new();
        loop {
            let heard = tokio::select! {
                heard = events.recv() => heard,
                // An ask of a session observed nowhere is dropped, and its asker hears so.
                Some((session, input)) = asks.recv() => {
                    if let Some(tx) = sessions.get(&session)
                        && tx.send(input).is_err()
                    {
                        sessions.remove(&session);
                    }
                    continue;
                }
            };
            let (session, input) = match heard {
                Ok(WorkerMsg::Agent(event)) if event.kind == AgentKind::ClaudeCode => {
                    (event.session, Input::Status(Box::new(event)))
                }
                Ok(WorkerMsg::Permission(event)) => (event.session(), Input::Permission(event)),
                Ok(WorkerMsg::SessionClosed { session, .. }) => {
                    sessions.remove(&session);
                    cwds.remove(&session);
                    titles.remove(&session);
                    continue;
                }
                Ok(
                    WorkerMsg::SessionOpened { summary, .. } | WorkerMsg::SessionChanged(summary),
                ) => {
                    if let Some(tx) = sessions.get(&summary.id) {
                        let _gone = tx.send(Input::Title(summary.title.clone()));
                    }
                    titles.insert(summary.id, summary.title);
                    let Some(cwd) = summary.cwd else { continue };
                    cwds.insert(summary.id, cwd.clone());
                    if let Some(tx) = sessions.get(&summary.id) {
                        let _gone = tx.send(Input::Cwd(cwd));
                    }
                    continue;
                }
                Ok(_) => continue,
                // A status missed is told again with the next change; a prompt missed is in
                // the TUI's own dialog.
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::debug!(missed, "the observed sessions missed events");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => return,
            };
            let tx = sessions.entry(session).or_insert_with(|| {
                let (tx, rx) = mpsc::unbounded_channel();
                if let Some(cwd) = cwds.get(&session) {
                    let _queued = tx.send(Input::Cwd(cwd.clone()));
                }
                if let Some(title) = titles.get(&session) {
                    let _queued = tx.send(Input::Title(title.clone()));
                }
                tokio::spawn(observe(host.clone(), session, Arc::clone(&sources), rx));
                tx
            });
            if tx.send(input).is_err() {
                sessions.remove(&session);
            }
        }
    })
}

/// One terminal session's Claude Code, until the session goes.
async fn observe(
    host: Host,
    session: SessionId,
    sources: Arc<dyn Sources>,
    mut inputs: mpsc::UnboundedReceiver<Input>,
) {
    let mut seen = sources.seen(session);
    let mut watching = true;
    let mut on = Session {
        host,
        terminal: session,
        sources,
        observed: None,
        transcripts: Some(Transcripts::default()),
        status: None,
        cwd: String::new(),
        title: String::new(),
        hooks: 0,
    };
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            input = inputs.recv() => match input {
                None => {
                    on.forget_provisional();
                    return;
                }
                Some(Input::Status(event)) => on.status(*event).await,
                Some(Input::Cwd(cwd)) => {
                    if let Some(observed) = on.observed.as_mut() {
                        take(&on.host, observed.cwd(&cwd));
                    }
                    on.cwd = cwd;
                }
                Some(Input::Title(title)) => {
                    if let Some(observed) = on.observed.as_mut() {
                        take(&on.host, observed.title(&title));
                    }
                    on.title = title;
                }
                Some(Input::Permission(event)) => {
                    if let Some(observed) = on.observed.as_mut() {
                        let outs = observed.permission(&event);
                        take(&on.host, outs);
                    }
                }
                Some(Input::Expand(content, reply)) => {
                    let _gone = reply.send(on.expand(&content).await);
                }
            },
            changed = seen.changed(), if watching => match changed {
                Ok(()) => {
                    let now = seen.borrow_and_update().clone();
                    on.seen(&now).await;
                }
                Err(_) => watching = false,
            },
            _ = tick.tick() => {
                on.read().await;
                let board = seen.borrow().live.clone();
                if let Some(observed) = on.observed.as_mut() {
                    take(&on.host, observed.live(&board, Instant::now(), WallMs::now()));
                }
            }
        }
    }
}

struct Session {
    host: Host,
    terminal: SessionId,
    sources: Arc<dyn Sources>,
    observed: Option<Observed>,
    /// `None` while a read has them on the blocking pool.
    transcripts: Option<Transcripts>,
    /// The last status, to tell a thread begun after it.
    status: Option<AgentEvent>,
    /// The terminal's working directory.
    cwd: String,
    /// The terminal's title.
    title: String,
    hooks: u64,
}

impl Session {
    async fn status(&mut self, event: AgentEvent) {
        let native = event.agent_session.clone().or_else(|| self.native_from_file());
        match native {
            Some(native) => self.begin(&native),
            None if event.status == AgentStatus::None => self.forget_provisional(),
            None if self.observed.is_none() => {
                let observed = Observed::provisional("", self.terminal, &self.cwd, WallMs::now());
                self.start(observed);
            }
            None => {}
        }
        if let Some(observed) = self.observed.as_mut() {
            take(&self.host, observed.status(&event));
        }
        self.status = Some(event);
        self.read().await;
    }

    async fn seen(&mut self, seen: &Seen) {
        if let Some(observed) = self.observed.as_mut() {
            if seen.hooks > 0 {
                take(&self.host, observed.hooked());
            }
            if let Some(meters) = &seen.meters {
                take(&self.host, observed.meters(meters));
            }
            take(&self.host, observed.live(&seen.live, Instant::now(), WallMs::now()));
        }
        if seen.hooks != self.hooks {
            self.hooks = seen.hooks;
            self.read().await;
        }
    }

    /// The session id the transcript's file name says, for a session no hook spoke for.
    fn native_from_file(&self) -> Option<String> {
        let main = self.sources.sources(self.terminal).main?;
        main.file_stem().map(|s| s.to_string_lossy().into_owned())
    }

    /// Observe Claude Code session `native` from now on, unless it already is.
    fn begin(&mut self, native: &str) {
        if self.observed.as_ref().is_some_and(|o| o.meta().native == native) {
            return;
        }
        self.forget_provisional();
        if let Some(old) = self.observed.take() {
            let exited = Status {
                phase: Phase::Idle,
                wait: None,
                liveness: Liveness::Exited { resumable: true },
                since_ms: WallMs::now(),
            };
            self.host.apply(old.main(), vec![Action::Status(exited)]);
        }
        let observed = Observed::new(native, "", Some(self.terminal), &self.cwd, WallMs::now());
        self.start(observed);
        self.transcripts = Some(Transcripts::default());
    }

    /// Put `observed` in the host, told what the session has heard so far.
    fn start(&mut self, mut observed: Observed) {
        take(&self.host, observed.drain());
        take(&self.host, observed.title(&self.title));
        if self.hooks > 0 {
            take(&self.host, observed.hooked());
        }
        if let Some(status) = &self.status {
            take(&self.host, observed.status(status));
        }
        self.observed = Some(observed);
    }

    /// Remove the provisional thread, if that is what is observed: it never was a session.
    fn forget_provisional(&mut self) {
        let Some(old) = self.observed.take_if(|o| o.is_provisional()) else { return };
        if let Err(e) = self.host.remove(old.main()) {
            tracing::warn!(thread = %old.main(), "a provisional thread could not go: {e}");
        }
    }

    /// Read what the transcripts gained, on the blocking pool, and take it in.
    async fn read(&mut self) {
        let sources = self.sources.sources(self.terminal);
        let Some(main) = sources.main else { return };
        if self.observed.as_ref().is_none_or(Observed::is_provisional)
            && let Some(native) = stem(&main)
        {
            self.begin(&native);
        }
        let Some(mut transcripts) = self.transcripts.take() else { return };
        let subagents = sources.subagents;
        let read = tokio::task::spawn_blocking(move || {
            let changes = transcripts.read(&main, &subagents);
            let outputs = transcripts.outputs();
            (transcripts, changes, outputs)
        })
        .await;
        let Ok((transcripts, changes, outputs)) = read else {
            self.transcripts = Some(Transcripts::default());
            return;
        };
        self.transcripts = Some(transcripts);
        if changes.is_empty() && outputs.is_empty() {
            return;
        }
        if let Some(observed) = self.observed.as_mut() {
            take(&self.host, observed.transcript(&changes, &outputs));
        }
    }
}

impl Session {
    /// The whole of `content`, read from the transcripts on the blocking pool.
    async fn expand(&mut self, content: &ContentRef) -> Expanded {
        let Some((thread, at)) = observed::text_ref(content) else { return Expanded::Gone };
        let Some(transcripts) = self.transcripts.take() else { return Expanded::Gone };
        let read = tokio::task::spawn_blocking(move || {
            let body = if matches!(at.part, Part::Image { .. }) {
                transcripts.image(&thread, &at).map(Expanded::Bytes)
            } else {
                transcripts.full_text(&thread, &at).map(|text| Expanded::Text(clip(text)))
            };
            (transcripts, body.unwrap_or(Expanded::Gone))
        })
        .await;
        let Ok((transcripts, body)) = read else {
            self.transcripts = Some(Transcripts::default());
            return Expanded::Gone;
        };
        self.transcripts = Some(transcripts);
        body
    }
}

/// `text` cut at [`EXPANDED_CHARS`].
fn clip(mut text: String) -> String {
    if let Some((at, _)) = text.char_indices().nth(EXPANDED_CHARS) {
        text.truncate(at);
    }
    text
}

fn stem(path: &Path) -> Option<String> {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).filter(|s| !s.is_empty())
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
                    tracing::warn!(thread = %id, "an observed thread could not begin: {e}");
                }
            }
            Out::Actions(thread, actions) => {
                host.apply(thread, actions);
            }
        }
    }
}
