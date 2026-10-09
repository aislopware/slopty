//! Claude Code, observed, on the worker: the IO half of the adapter
//! (`slopty_agent::observed` is the codec).
//!
//! It hears what every client hears (the daemon's broadcast: each agent's status as the tracker
//! merged it), the permission prompts the daemon holds and settles, in order
//! ([`Driver::permission`]), and what the hooks and the mod said of each session
//! ([`Sources::seen`]). A task per terminal session running Claude Code reads its transcripts
//! on the blocking pool, at once after a hook and on a [`TICK`] between, and puts what the codec
//! makes of it all into the [`Host`]: a thread per Claude Code session, one per subagent.
//!
//! A session's thread is begun when its own id is known (its first hook, or the transcript
//! file's name), since that id names the thread. Until then, a Claude Code the tracker sees in
//! the terminal (one started by hand, idle at its prompt) has a provisional thread named by the
//! terminal, removed once the session's own begins or the agent goes, since it never was a
//! session. One Slopty started ([`start`]) is begun at once ([`Driver::begin`]) under the session
//! id it was given, so it needs no provisional thread. A thread the host holds from before (a
//! worker restart) starts over and is read again from the transcript, under a new epoch. When the
//! terminal moves to another Claude Code session (`/clear`, `/resume`), the old thread is left
//! exited and resumable, and the new one begins. A thread declares `approvals` once a hook has been
//! heard in its terminal.
//!
//! A permission prompt held for a terminal opens a thread there when none is observed yet (the
//! session's own when its id is known, else the terminal's provisional one), so the prompt is a
//! request on a thread from the first hook. The prompts held and not yet settled are told again
//! to whichever thread begins after them, provisional or the session's own. A block with no
//! prompt held for it (nobody followed the thread when the agent asked) is asked in the agent's
//! own terminal, looked at on each tick ([`Observed::waited`]).
//!
//! Two terminals whose Claude Codes run on one session id at once (a `--resume` of a session
//! still running elsewhere, or a stand-in that names every session alike) are told apart: the
//! first to claim the id keeps the session's thread until its agent goes, and the other observes
//! a thread of its own ([`observed::thread_in`]), so no thread moves between terminals.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use slopty_agent::conversation::{Part, Transcripts};
use slopty_agent::observed::{self, Observed, Out};
use slopty_agent::status::{AgentEvent, AgentStatus};
use slopty_core::{SessionId, WallMs};
use slopty_proto::WorkerMsg;
use slopty_proto::conversation::{PermissionEvent, PermissionPrompt};
use slopty_proto::thread::wire::{EXPANDED_CHARS, Expanded};
use slopty_proto::thread::{Action, ContentRef, Liveness, Phase, Status, ThreadId, ThreadState};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use super::Host;
use crate::conversation::Seen;

pub mod start;

/// How often a session's transcripts are read between hooks: answers and thinking arrive with
/// no hook of their own.
pub const TICK: Duration = Duration::from_millis(250);

/// Where the daemon keeps what an observed session needs.
pub trait Sources: Send + Sync {
    /// Where the session's conversation is written.
    fn sources(&self, session: SessionId) -> crate::orchestrate::Sources;
    /// What the hooks and the mod said of the session, as it changes.
    fn seen(&self, session: SessionId) -> watch::Receiver<Seen>;
    /// Every agent's status as the daemon's table holds it now: told again when reports were
    /// missed, since a session's last word may be among them.
    fn standing(&self) -> Vec<AgentEvent>;
    /// Whether terminal `session` is still open: what is kept of one whose close was missed
    /// is let go.
    fn open(&self, session: SessionId) -> bool;
}

/// Which terminal holds each Claude Code session id it observes, so a second terminal on the
/// same id is told apart from it.
type Claims = Arc<parking_lot::Mutex<HashMap<String, SessionId>>>;

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
    /// Slopty started Claude Code in the terminal on session `native`, in `cwd`: its thread
    /// begins now, before the agent says anything, and is answered.
    Begin {
        native: String,
        cwd: String,
        reply: oneshot::Sender<Option<ThreadId>>,
    },
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

    /// A prompt Claude Code asks in terminal `session` is held for the people who can answer it,
    /// or one held is settled: it opens or settles a request on the session's thread. One held
    /// where nothing is observed yet starts observing it. Told in the order the holds decide.
    pub fn permission(&self, event: PermissionEvent) {
        let _gone = self.0.send((event.session(), Input::Permission(event)));
    }

    /// Slopty started Claude Code in terminal `session` on session `native`, in `cwd`: observe
    /// it from now on, its thread begun at once. The thread, or `None` when nothing observes.
    pub async fn begin(&self, session: SessionId, native: String, cwd: String) -> Option<ThreadId> {
        let (reply, rx) = oneshot::channel();
        self.0.send((session, Input::Begin { native, cwd, reply })).ok()?;
        rx.await.ok().flatten()
    }
}

/// Observe every Claude Code session the daemon's agent reports (`heard`) speak of, into
/// `host`, its title and folder as its `events` say, and answer the `asks` of its [`Driver`].
pub fn spawn(
    host: Host,
    mut events: broadcast::Receiver<WorkerMsg>,
    mut heard: broadcast::Receiver<AgentEvent>,
    sources: Arc<dyn Sources>,
    Asks(mut asks): Asks,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut on = Observers {
            host,
            sources,
            sessions: HashMap::new(),
            cwds: HashMap::new(),
            titles: HashMap::new(),
            claims: Claims::default(),
        };
        loop {
            tokio::select! {
                report = heard.recv() => match report {
                    Ok(event) => on.send(event.session, Input::Status(Box::new(event))),
                    // A session's last status may be among those missed, and no change may
                    // follow it: the table's are told again.
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::debug!(missed, "the observed sessions missed agent reports");
                        on.told_again();
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                heard_of = events.recv() => match heard_of {
                    Ok(WorkerMsg::SessionClosed { session, .. }) => on.closed(session),
                    Ok(
                        WorkerMsg::SessionOpened { summary, .. } | WorkerMsg::SessionChanged(summary),
                    ) => {
                        if let Some(tx) = on.sessions.get(&summary.id) {
                            let _gone = tx.send(Input::Title(summary.title.clone()));
                        }
                        on.titles.insert(summary.id, summary.title);
                        let Some(cwd) = summary.cwd else { continue };
                        on.cwds.insert(summary.id, cwd.clone());
                        if let Some(tx) = on.sessions.get(&summary.id) {
                            let _gone = tx.send(Input::Cwd(cwd));
                        }
                    }
                    Ok(_) => {}
                    // A title or folder missed is told again with the next change; a close
                    // missed would keep what is held of its session for good, so every session
                    // is looked at.
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::debug!(missed, "the observed sessions missed events");
                        on.closed_unheard();
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                // An ask of a session observed nowhere is dropped, and its asker hears so; a
                // begin, or a prompt held, is what starts observing it.
                Some((session, input)) = asks.recv() => {
                    if matches!(input, Input::Begin { .. } | Input::Permission(_)) {
                        on.send(session, input);
                    } else if let Some(tx) = on.sessions.get(&session)
                        && tx.send(input).is_err()
                    {
                        on.sessions.remove(&session);
                    }
                }
            }
        }
    })
}

/// The observed sessions' tasks, and what the daemon's events said of each terminal.
struct Observers {
    host: Host,
    sources: Arc<dyn Sources>,
    sessions: HashMap<SessionId, mpsc::UnboundedSender<Input>>,
    cwds: HashMap<SessionId, String>,
    titles: HashMap<SessionId, String>,
    claims: Claims,
}

impl Observers {
    /// Tell `session`'s task `input`, starting one when none observes it.
    fn send(&mut self, session: SessionId, input: Input) {
        let tx = self.sessions.entry(session).or_insert_with(|| {
            let (tx, rx) = mpsc::unbounded_channel();
            if let Some(cwd) = self.cwds.get(&session) {
                let _queued = tx.send(Input::Cwd(cwd.clone()));
            }
            if let Some(title) = self.titles.get(&session) {
                let _queued = tx.send(Input::Title(title.clone()));
            }
            let claims = Arc::clone(&self.claims);
            let sources = Arc::clone(&self.sources);
            tokio::spawn(observe(self.host.clone(), session, sources, claims, rx));
            tx
        });
        if tx.send(input).is_err() {
            self.sessions.remove(&session);
        }
    }

    /// Terminal `session` closed: its task ends, and what was kept of it goes.
    fn closed(&mut self, session: SessionId) {
        self.sessions.remove(&session);
        self.cwds.remove(&session);
        self.titles.remove(&session);
    }

    /// Every status the table holds, told again: to the sessions observed, and to any other
    /// whose agent is there, as a status heard would start it.
    fn told_again(&mut self) {
        for event in self.sources.standing() {
            if self.sessions.contains_key(&event.session) || event.status != AgentStatus::None {
                self.send(event.session, Input::Status(Box::new(event)));
            }
        }
    }

    /// Each terminal no longer open, whose close went unheard, closed.
    fn closed_unheard(&mut self) {
        let mut known: Vec<SessionId> = self.sessions.keys().copied().collect();
        known.extend(self.cwds.keys().chain(self.titles.keys()));
        known.sort_unstable();
        known.dedup();
        for session in known {
            if !self.sources.open(session) {
                self.closed(session);
            }
        }
    }
}

/// One terminal session's Claude Code, until the session goes.
async fn observe(
    host: Host,
    session: SessionId,
    sources: Arc<dyn Sources>,
    claims: Claims,
    mut inputs: mpsc::UnboundedReceiver<Input>,
) {
    let mut seen = sources.seen(session);
    let mut watching = true;
    let mut on = Session {
        host,
        terminal: session,
        sources,
        claims,
        held: Vec::new(),
        observed: None,
        transcripts: Some(Transcripts::default()),
        status: None,
        cwd: String::new(),
        hooked_cwd: false,
        commands_for: None,
        title: String::new(),
        hooks: 0,
        meters: seen.borrow().meters.clone(),
        silent_since: None,
    };
    // The hook that brought the session here was heard before this watched for changes.
    let heard = seen.borrow().cwd.clone();
    if let Some(cwd) = heard {
        on.cwd = cwd;
        on.hooked_cwd = true;
    }
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            input = inputs.recv() => match input {
                None => {
                    on.forget_provisional();
                    on.unclaim();
                    return;
                }
                Some(Input::Status(event)) => on.status(*event).await,
                // The agent's own word on where it works, once a hook said it, outranks the
                // terminal's.
                Some(Input::Cwd(cwd)) if !on.hooked_cwd => {
                    if let Some(observed) = on.observed.as_mut() {
                        take(&on.host, observed.cwd(&cwd));
                    }
                    on.cwd = cwd;
                }
                Some(Input::Cwd(_)) => {}
                Some(Input::Title(title)) => {
                    if let Some(observed) = on.observed.as_mut() {
                        take(&on.host, observed.title(&title));
                    }
                    on.title = title;
                }
                Some(Input::Permission(event)) => on.permission(&event).await,
                Some(Input::Expand(content, reply)) => on.expand(&content, reply),
                Some(Input::Begin { native, cwd, reply }) => {
                    if on.cwd.is_empty() {
                        on.cwd = cwd;
                    }
                    on.begin(&native);
                    // Slopty opened it with the hook relay: until a hook speaks, it may be held
                    // at a dialog of its own.
                    if on.hooks == 0 {
                        on.silent_since = Some(Instant::now());
                    }
                    let _gone = reply.send(on.observed.as_ref().map(Observed::main));
                    on.read().await;
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
                    take(&on.host, observed.waited(WallMs::now()));
                }
                on.silence();
            }
        }
    }
}

struct Session {
    host: Host,
    terminal: SessionId,
    sources: Arc<dyn Sources>,
    claims: Claims,
    /// The prompts held for the terminal and not yet settled.
    held: Vec<PermissionPrompt>,
    observed: Option<Observed>,
    /// `None` while a read has them on the blocking pool.
    transcripts: Option<Transcripts>,
    /// The last status, to tell a thread begun after it.
    status: Option<AgentEvent>,
    /// The agent's working directory: its hooks', else the terminal's.
    cwd: String,
    /// A hook said the working directory.
    hooked_cwd: bool,
    /// The thread and folder the slash commands were last read for.
    commands_for: Option<(ThreadId, String)>,
    /// The terminal's title.
    title: String,
    hooks: u64,
    /// The status line's latest meters, for a thread begun after they came.
    meters: Option<slopty_proto::conversation::Meters>,
    /// When Slopty opened the Claude Code here, while no hook has spoken since
    /// ([`observed::UNHEARD`]).
    silent_since: Option<Instant>,
}

impl Session {
    async fn status(&mut self, event: AgentEvent) {
        let native = event.agent_session.clone().or_else(|| self.native_from_file());
        match native {
            Some(native) => {
                self.begin(&native);
                // The agent went: its session's id is free for another terminal to take up.
                if event.status == AgentStatus::None {
                    self.unclaim();
                }
            }
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
        // A hook that names the folder the terminal already reported still outranks the
        // terminal from now on.
        if let Some(cwd) = &seen.cwd {
            self.hooked_cwd = true;
            if *cwd != self.cwd {
                cwd.clone_into(&mut self.cwd);
                if let Some(observed) = self.observed.as_mut() {
                    take(&self.host, observed.cwd(cwd));
                }
            }
        }
        if let Some(observed) = self.observed.as_mut() {
            if seen.hooks > 0 {
                take(&self.host, observed.hooked());
            }
            if let Some(meters) = &seen.meters {
                take(&self.host, observed.meters(meters));
            }
            if let Some(catalog) = &seen.catalog {
                take(&self.host, observed.catalog(catalog));
            }
        }
        self.meters.clone_from(&seen.meters);
        if let Some(observed) = self.observed.as_mut() {
            take(&self.host, observed.live(&seen.live, Instant::now(), WallMs::now()));
        }
        if seen.hooks != self.hooks {
            self.hooks = seen.hooks;
            self.silent_since = None;
            self.read().await;
        }
    }

    /// A Claude Code Slopty opened here that no hook has spoken for in [`observed::UNHEARD`]
    /// is held at a dialog of its own: its thread says it asks in the terminal, once.
    fn silence(&mut self) {
        let Some(since) = self.silent_since else { return };
        if since.elapsed() < observed::UNHEARD {
            return;
        }
        self.silent_since = None;
        if let Some(observed) = self.observed.as_mut() {
            tracing::info!(terminal = %self.terminal, "Claude Code is silent at its start");
            take(&self.host, observed.unheard(WallMs::now()));
        }
    }

    /// A prompt held for the terminal, or settled, or a call auto mode declined. One held, or a
    /// decline, while no thread is observed here opens one: the session's own when its id is
    /// known, else the terminal's provisional one.
    async fn permission(&mut self, event: &PermissionEvent) {
        match event {
            PermissionEvent::Asked(prompt) => self.held.push((**prompt).clone()),
            PermissionEvent::Settled { ask, .. } => self.held.retain(|p| p.ask != *ask),
            PermissionEvent::Declined(_) => {}
        }
        if let Some(observed) = self.observed.as_mut() {
            take(&self.host, observed.permission(event));
            return;
        }
        if matches!(event, PermissionEvent::Settled { .. }) {
            return;
        }
        let known = self.status.as_ref().and_then(|s| s.agent_session.clone());
        // Begun now, the thread is told the prompts held, this one with them.
        if let Some(native) = known.or_else(|| self.native_from_file()) {
            self.begin(&native);
        } else {
            let observed = Observed::provisional("", self.terminal, &self.cwd, WallMs::now());
            self.start(observed);
        }
        self.read().await;
        if let (PermissionEvent::Declined(_), Some(observed)) = (event, self.observed.as_mut()) {
            take(&self.host, observed.permission(event));
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
            // Its agent back on the same session holds the id again, unless another took it.
            let _held = self.claim(native);
            return;
        }
        self.forget_provisional();
        self.unclaim();
        if let Some(old) = self.observed.take() {
            let exited = Status {
                phase: Phase::Idle,
                wait: None,
                liveness: Liveness::Exited { resumable: true },
                since_ms: WallMs::now(),
            };
            self.host.apply(old.main(), vec![Action::Status(exited)]);
        }
        let now = WallMs::now();
        let observed = if self.claim(native) {
            Observed::new(native, "", Some(self.terminal), &self.cwd, now)
        } else {
            tracing::info!(terminal = %self.terminal, native, "a session id another terminal runs");
            Observed::beside(native, "", self.terminal, &self.cwd, now)
        };
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
        if let Some(meters) = &self.meters {
            take(&self.host, observed.meters(meters));
        }
        if let Some(status) = &self.status {
            take(&self.host, observed.status(status));
        }
        for prompt in &self.held {
            let asked = PermissionEvent::Asked(Box::new(prompt.clone()));
            take(&self.host, observed.permission(&asked));
        }
        self.observed = Some(observed);
    }

    /// Claim session id `native` for this terminal: whether no other terminal holds it.
    fn claim(&self, native: &str) -> bool {
        let mut claims = self.claims.lock();
        match claims.get(native) {
            Some(holder) if *holder != self.terminal => false,
            _ => {
                claims.insert(native.to_owned(), self.terminal);
                true
            }
        }
    }

    /// Let go of the session id this terminal holds, if it holds one.
    fn unclaim(&self) {
        let Some(native) = self.observed.as_ref().map(|o| o.meta().native.clone()) else {
            return;
        };
        let mut claims = self.claims.lock();
        if claims.get(&native) == Some(&self.terminal) {
            claims.remove(&native);
        }
    }

    /// Remove the provisional thread, if that is what is observed: it never was a session.
    fn forget_provisional(&mut self) {
        let Some(old) = self.observed.take_if(|o| o.is_provisional()) else { return };
        if let Err(e) = self.host.remove(old.main()) {
            tracing::warn!(thread = %old.main(), "a provisional thread could not go: {e}");
        }
    }

    /// Tell the thread the person's and the project's own slash commands in its folder, read on
    /// the blocking pool once for each thread and folder. Claude Code's whole list comes from
    /// the mod where it is heard ([`Observed::catalog`]); these give it their argument hints,
    /// and stand alone where it is not.
    async fn offer_commands(&mut self) {
        let Some(main) = self.observed.as_ref().map(Observed::main) else { return };
        let wanted = (main, self.cwd.clone());
        if self.commands_for.as_ref() == Some(&wanted) {
            return;
        }
        self.commands_for = Some(wanted);
        let cwd = PathBuf::from(&self.cwd);
        let home = slopty_platform::dirs::home();
        let listed = tokio::task::spawn_blocking(move || {
            if cwd.as_os_str().is_empty() {
                Vec::new()
            } else {
                slopty_agent::commands::custom(&home, &cwd)
            }
        })
        .await;
        let Ok(listed) = listed else { return };
        if let Some(observed) = self.observed.as_mut().filter(|o| o.main() == main) {
            take(&self.host, observed.commands(&listed));
        }
    }

    /// Read what the transcripts gained, on the blocking pool, and take it in.
    async fn read(&mut self) {
        self.offer_commands().await;
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
    /// Answer `reply` with the whole of `content`: the record's line, as the transcripts'
    /// index places it ([`Transcripts::locate`]), read on the blocking pool while the session
    /// goes on with its thread.
    fn expand(&self, content: &ContentRef, reply: oneshot::Sender<Expanded>) {
        let located = observed::text_ref(content).and_then(|(thread, at)| {
            let image = matches!(at.part, Part::Image { .. });
            Some((self.transcripts.as_ref()?.locate(&thread, &at), image))
        });
        let Some((located, image)) = located else {
            let _gone = reply.send(Expanded::Gone);
            return;
        };
        drop(tokio::task::spawn_blocking(move || {
            let body = if image {
                located.image().map(Expanded::Bytes)
            } else {
                located.text().map(|text| Expanded::Text(clip(text)))
            };
            let _gone = reply.send(body.unwrap_or(Expanded::Gone));
        }));
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
