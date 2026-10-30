//! Claude Code in Slopty's terminals, each session read as a thread (`docs/decisions/agents.md`,
//! "TUI-first").
//!
//! Every Claude Code in one of the worker's terminals is the person's own TUI, typed there by
//! hand or started by Slopty (`slopty_agent::claude::Tui`). It is found from outside: the
//! terminal's foreground process, which the daemon reads on its tick ([`Claude::tick`]), and
//! the record each live Claude Code keeps of itself, which names the session it writes now
//! (`slopty_agent::claude::sessions::live_of`, for the process or the real client a managed
//! launcher runs as its child); for one Slopty started, its session is known from the start
//! ([`Claude::begun`]) and its hooks name the next. The session's transcript and its subagents'
//! files are followed as the kernel reports their writes, read by the conversation decoder and
//! mapped onto one thread per session (`slopty_agent::claude::history`), which names the
//! terminal ([`ThreadMeta::TERMINAL_FACT`]) while the session runs there. A `/clear` or
//! `/resume` in the TUI moves the terminal to the next session: its thread begins, and the one
//! before ends. Nothing of the terminal's screen is read, and nothing is typed here.
//!
//! - **The phase** follows the session's hooks once one is heard ([`Claude::hook`],
//!   `slopty_agent::hooks`): at work, waiting on the person at a dialog, done or failed (its `Stop`
//!   ends the turn, since the records that close it come after), compacting. The transcript is read
//!   before each hook is applied, so a turn's end never shows before the turn. A session with none
//!   (typed by hand) takes it from its transcript (a turn open is at work, one closed is done) and
//!   from its record, which says when a dialog holds it (`Live::waiting`): waiting on the person
//!   then, for the call the transcript has open where the dialog is about one, and back to what the
//!   transcript says once the record no longer waits. Hooks are exact, so a session that has them
//!   keeps them over its record, and its record over its transcript. An Esc fires no hook, so the
//!   transcript's interrupt says it for both.
//! - **Its prompt box** comes from Slopty's mod in a Claude Code Slopty started
//!   (`slopty_agent::claude::draft`): each draft it relays, with the pictures pasted into it read
//!   from Claude Code's own store, is the thread's [`slopty_proto::thread::Draft`], apart from the
//!   hooks and their order. The prompts queued behind a turn come from the transcript, for any
//!   session; one queued before its Claude Code began was another run's.
//! - **Ended** when its Claude Code ends (its process gone, its terminal closed or exited) or its
//!   `SessionEnd` hook says so: exited and resumable, the terminal no longer named.
//! - **A worker that starts again** finds the sessions again at its first ticks and reads their
//!   threads again from their transcripts; a thread that names a terminal where no Claude Code runs
//!   by then has ended.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use slopty_agent::claude::draft::{self, Pictures};
use slopty_agent::claude::history::{End, History, Out, asks_for};
use slopty_agent::claude::sessions::{self, Live};
use slopty_agent::conversation::{Body, Change, Transcripts};
use slopty_agent::hooks::{Hook, Ran, Signal};
use slopty_core::{SessionId, WallMs};
use slopty_proto::thread::wire::Expanded;
use slopty_proto::thread::{
    Action, AgentId, ContentRef, Drive, Liveness, Phase, ThreadId, ThreadMeta, ThreadState,
    TurnState, Wait,
};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use super::Host;

pub mod held;

/// Where the Claude Codes run, as the observer reads them.
#[derive(Clone, Debug)]
pub struct Place {
    /// The person's home, where Claude Code keeps its records.
    pub home: PathBuf,
    /// The `PATH` an agent's program is looked for on (a test's own); the person's when
    /// `None`.
    pub path: Option<OsString>,
    /// The worker's data directory, where the files an agent loads are written (pi's tools).
    pub data: PathBuf,
}

/// What the daemon's tick saw in the foreground of one terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Foreground {
    /// A Claude Code: its process, and where it runs.
    Claude {
        /// The foreground process: Claude Code, or a managed launcher that runs it as its child.
        pid: u32,
        /// Its working directory, when the platform said.
        cwd: Option<PathBuf>,
    },
    /// Another program.
    Other,
    /// The platform would not say: nothing is concluded.
    Unknown,
}

/// The observer of every Claude Code in the worker's terminals. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Claude {
    asks: mpsc::UnboundedSender<Ask>,
    host: Host,
    place: Arc<Place>,
    stores: Stores,
}

/// Where each thread's prompt box keeps its pasted pictures, while it shows one: the store a
/// draft picture's bytes are read from ([`Claude::expand`]).
type Stores = Arc<Mutex<HashMap<ThreadId, PathBuf>>>;

#[derive(Debug)]
enum Ask {
    Tick(Vec<(SessionId, Foreground)>),
    Hook { terminal: SessionId, at_us: u64, payload: String },
    Begun { terminal: SessionId, native: String, cwd: String, version: String },
    Row { terminal: SessionId, thread: ThreadId },
}

impl Claude {
    /// Observe every Claude Code the ticks name, as threads of `host`.
    #[must_use]
    pub fn spawn(host: Host, place: Place) -> (Self, JoinHandle<()>) {
        let (asks, taken) = mpsc::unbounded_channel();
        let place = Arc::new(place);
        let stores = Stores::default();
        let watch = Watch {
            host: host.clone(),
            place: Arc::clone(&place),
            stores: Arc::clone(&stores),
            terms: HashMap::new(),
            ticks: 0,
        };
        let task = tokio::spawn(watch.run(taken));
        (Self { asks, host, place, stores }, task)
    }

    /// What every terminal running a program shows in its foreground now; a terminal not named
    /// has ended (or its program has).
    pub fn tick(&self, seen: Vec<(SessionId, Foreground)>) {
        let _gone = self.asks.send(Ask::Tick(seen));
    }

    /// One of Claude Code's hooks, relayed from terminal `terminal` as it started at `at_us`
    /// (microseconds since the epoch), its JSON `payload`. The token was checked.
    pub fn hook(&self, terminal: SessionId, at_us: u64, payload: String) {
        let _gone = self.asks.send(Ask::Hook { terminal, at_us, payload });
    }

    /// Slopty started Claude Code `version` in `terminal` on session `native`, in `cwd`: it is
    /// followed from now, before any tick or hook says so.
    pub fn begun(&self, terminal: SessionId, native: String, cwd: String, version: String) {
        let _gone = self.asks.send(Ask::Begun { terminal, native, cwd, version });
    }

    /// Slopty started another agent's TUI in `terminal`, whose thread `thread` is a row only:
    /// it ends with the terminal's program.
    pub fn row(&self, terminal: SessionId, thread: ThreadId) {
        let _gone = self.asks.send(Ask::Row { terminal, thread });
    }

    /// The whole of `content`, clipped in `thread`, read from its session's records; a picture
    /// pasted into its prompt box, from Claude Code's store of them.
    pub async fn expand(&self, thread: ThreadId, content: ContentRef) -> Expanded {
        if let Some(name) = draft::image_name(&content).map(str::to_owned) {
            let Some(store) = self.stores.lock().get(&thread).cloned() else {
                return Expanded::Gone;
            };
            let read =
                move || draft::read_picture(&store, &name).map_or(Expanded::Gone, Expanded::Bytes);
            return tokio::task::spawn_blocking(read).await.unwrap_or(Expanded::Gone);
        }
        let Some(root) = root_of(&self.host, thread) else { return Expanded::Gone };
        let home = self.place.home.clone();
        tokio::task::spawn_blocking(move || {
            sessions::expand(&home, Path::new(&root.cwd), &root.native, &content)
        })
        .await
        .unwrap_or(Expanded::Gone)
    }
}

/// Whether `state` is a thread of a Claude Code session.
#[must_use]
pub fn is_claude(state: &ThreadState) -> bool {
    state.meta.agent.is(AgentId::CLAUDE_CODE)
}

/// How many ticks after the observer starts a thread that names a terminal with no Claude Code
/// is left as it is: the worker's terminals are being taken back from ptyd meanwhile.
const SETTLE_TICKS: u32 = 4;

/// How many ticks a session Slopty started may go without its Claude Code in the foreground
/// before it is taken to have ended: a launcher takes a moment to start it.
const START_TICKS: u32 = 8;

/// The observer's own task: every terminal's Claude Code.
struct Watch {
    host: Host,
    place: Arc<Place>,
    stores: Stores,
    terms: HashMap<SessionId, Term>,
    ticks: u32,
}

/// What is known of one terminal's Claude Code.
#[derive(Default)]
struct Term {
    /// Its process, once a tick saw it.
    pid: Option<u32>,
    /// The session followed now.
    session: Option<Followed>,
    /// Sessions that ended here while the process lives, which its record may still name for
    /// a moment.
    ended: HashSet<String>,
    /// When the last hook taken started, in microseconds since the epoch.
    hooked_us: u64,
    /// When the last draft of its prompt box taken started: drafts keep their own order, apart
    /// from the hooks'.
    drafted_us: u64,
    /// When its Claude Code began, once known: a prompt queued before was another run's.
    since: Option<WallMs>,
    /// Ticks since the session began with no Claude Code seen in the foreground.
    unseen: u32,
    /// Another agent's thread that is a row only, ended with the terminal's program.
    row: Option<ThreadId>,
    /// Whether a tick has named the terminal: one opened since a tick was read is not in it.
    seen: bool,
    /// The version of its Claude Code, once known: a session its hook begins after `/clear`
    /// runs in the same process.
    version: String,
}

/// A session followed on its own task, which ends on [`Say::End`] or once the observer goes.
struct Followed {
    native: String,
    thread: ThreadId,
    says: mpsc::UnboundedSender<Say>,
    /// What its record last said it waits on the person for, as the session was told.
    waits: Option<Wait>,
}

/// What a followed session's task is told.
#[derive(Debug)]
enum Say {
    /// A hook said this of the session, at this time, in the turn of this prompt (its
    /// `prompt_id`, which a `Stop` ends).
    Signal(Signal, WallMs, Option<String>),
    /// A call of the session ended, its hook saying how long its run took, at this time.
    Ran(Ran, WallMs),
    /// Its record says its name now.
    Named(String),
    /// Word of its start names the version of the Claude Code that runs it.
    Version(String),
    /// Its record says it waits on the person for this (`None`: no longer), since this time.
    Waits(Option<Wait>, WallMs),
    /// Slopty's mod says its prompt box holds `text`, its pasted pictures under the temporary
    /// root `tmpdir`, at this time.
    Draft { text: String, tmpdir: Option<String>, at: WallMs },
    /// The session ended here.
    End(WallMs),
}

/// How a session to follow is known.
struct Begin {
    terminal: SessionId,
    native: String,
    cwd: String,
    version: String,
    transcript: PathBuf,
    /// When the Claude Code that runs it began, when known.
    since: Option<WallMs>,
}

impl Watch {
    async fn run(mut self, mut asks: mpsc::UnboundedReceiver<Ask>) {
        while let Some(ask) = asks.recv().await {
            match ask {
                Ask::Tick(seen) => self.ticked(seen),
                Ask::Hook { terminal, at_us, payload } => self.hooked(terminal, at_us, &payload),
                Ask::Begun { terminal, native, cwd, version } => {
                    let term = self.terms.entry(terminal).or_default();
                    let since = Some(WallMs::now());
                    term.since = since;
                    // Its first hook can come before word of the start: the session is
                    // followed already, by the path the hook named, and takes the version.
                    if term.session.as_ref().is_some_and(|f| f.native == native) {
                        if !version.is_empty() {
                            term.version.clone_from(&version);
                            self.say(terminal, Say::Version(version));
                        }
                        continue;
                    }
                    let transcript =
                        sessions::transcript(&self.place.home, Path::new(&cwd), &native);
                    self.follow(Begin { terminal, native, cwd, version, transcript, since });
                }
                Ask::Row { terminal, thread } => {
                    self.terms.entry(terminal).or_default().row = Some(thread);
                }
            }
        }
    }

    /// A tick: each Claude Code's session as its record names it, and every one that ended.
    fn ticked(&mut self, seen: Vec<(SessionId, Foreground)>) {
        self.ticks = self.ticks.saturating_add(1);
        let named: HashSet<SessionId> = seen.iter().map(|(id, _)| *id).collect();
        let absent: Vec<SessionId> =
            self.terms.keys().filter(|id| !named.contains(id)).copied().collect();
        for terminal in absent {
            let Some(term) = self.terms.get_mut(&terminal) else { continue };
            term.unseen = term.unseen.saturating_add(1);
            if term.seen || term.unseen > START_TICKS {
                tracing::debug!(%terminal, "an agent's terminal ended");
                if let Some(row) = term.row.take() {
                    ended(&self.host, row, WallMs::now());
                }
                self.end(terminal, WallMs::now());
                self.terms.remove(&terminal);
            }
        }
        for (terminal, fg) in seen {
            if let Some(term) = self.terms.get_mut(&terminal) {
                term.seen = true;
            }
            match fg {
                Foreground::Claude { pid, cwd } => self.claude_at(terminal, pid, cwd.as_deref()),
                Foreground::Other => self.other_at(terminal),
                Foreground::Unknown => {}
            }
        }
        if self.ticks == SETTLE_TICKS {
            self.sweep();
        }
    }

    /// A Claude Code runs in the foreground of `terminal` as `pid`, in `cwd`.
    fn claude_at(&mut self, terminal: SessionId, pid: u32, cwd: Option<&Path>) {
        let term = self.terms.entry(terminal).or_default();
        term.pid = Some(pid);
        term.seen = true;
        term.unseen = 0;
        let Some(live) = record_of(&self.place.home, pid) else { return };
        if live.started_at.is_some() {
            term.since = live.started_at;
        }
        let current = term.session.as_ref().map(|f| f.native.as_str());
        if current == Some(live.session.as_str()) {
            self.told(terminal, &live);
            return;
        }
        if term.ended.contains(&live.session) {
            return;
        }
        let cwd = live.cwd.clone().or_else(|| cwd.map(|c| c.to_string_lossy().into_owned()));
        let Some(cwd) = cwd else { return };
        let transcript = sessions::transcript(&self.place.home, Path::new(&cwd), &live.session);
        let version = live.version.clone().unwrap_or_default();
        tracing::info!(%terminal, session = live.session, "a Claude Code's session, by its record");
        let (native, since) = (live.session.clone(), live.started_at);
        self.follow(Begin { terminal, native, cwd, version, transcript, since });
        self.told(terminal, &live);
    }

    /// Tell the session followed in `terminal` what its record `live` says: its name, and what
    /// it waits on when that changed.
    fn told(&mut self, terminal: SessionId, live: &Live) {
        let followed = self.terms.get_mut(&terminal).and_then(|t| t.session.as_mut());
        let Some(followed) = followed.filter(|f| f.native == live.session) else { return };
        if let Some(name) = &live.name {
            let _gone = followed.says.send(Say::Named(name.clone()));
        }
        let wait = live.waiting();
        if followed.waits == wait {
            return;
        }
        followed.waits.clone_from(&wait);
        let at = live.status_at.unwrap_or_else(WallMs::now);
        let _gone = followed.says.send(Say::Waits(wait, at));
    }

    /// Another program is in the foreground of `terminal`: its Claude Code ended, unless its
    /// process still lives (an editor it opened holds the terminal).
    fn other_at(&mut self, terminal: SessionId) {
        let Some(term) = self.terms.get_mut(&terminal) else { return };
        if term.session.is_none() {
            return;
        }
        let ended = if let Some(pid) = term.pid {
            !i32::try_from(pid).is_ok_and(crate::ports::alive)
        } else {
            term.unseen = term.unseen.saturating_add(1);
            term.unseen > START_TICKS
        };
        if ended {
            tracing::info!(%terminal, "a Claude Code ended");
            self.end(terminal, WallMs::now());
            if let Some(term) = self.terms.get_mut(&terminal) {
                term.pid = None;
                term.ended.clear();
            }
        }
    }

    /// End every thread an earlier worker left naming a terminal where no Claude Code was found.
    fn sweep(&self) {
        let followed: HashSet<ThreadId> = self
            .terms
            .values()
            .flat_map(|t| t.session.as_ref().map(|f| f.thread).into_iter().chain(t.row))
            .collect();
        let left = self.host.visit(|s| {
            (s.meta.terminal().is_some() && !followed.contains(&s.meta.id)).then_some(s.meta.id)
        });
        for thread in left {
            tracing::info!(%thread, "a Claude Code thread whose terminal no longer runs it ended");
            ended(&self.host, thread, WallMs::now());
        }
    }

    /// A hook from `terminal`: one older than the last taken is passed over, since Claude Code
    /// runs them all at once.
    fn hooked(&mut self, terminal: SessionId, at_us: u64, payload: &str) {
        let Some(hook) = Hook::parse(payload) else {
            tracing::debug!(%terminal, "a hook that is no hook's JSON");
            return;
        };
        let signal = hook.signal();
        if let Signal::Draft { text, tmpdir } = signal {
            self.drafted(terminal, at_us, hook.session_id.as_deref(), text, tmpdir);
            return;
        }
        // How long a call ran stands whatever order its hook came in: the age rule below is the
        // status's alone, and parallel calls end at once.
        let ours = self
            .terms
            .get(&terminal)
            .and_then(|t| t.session.as_ref())
            .is_some_and(|f| hook.session_id.as_deref() == Some(f.native.as_str()));
        if let Some(ran) = hook.ran().filter(|_| ours) {
            self.say(terminal, Say::Ran(ran, wall_of(at_us)));
        }
        let term = self.terms.entry(terminal).or_default();
        if at_us < term.hooked_us {
            tracing::debug!(%terminal, event = hook.event, "a hook older than the last, passed over");
            return;
        }
        term.hooked_us = at_us;
        let at = wall_of(at_us);
        let native = hook.session_id.clone().filter(|s| slopty_agent::claude::is_session_id(s));
        let current = term.session.as_ref().map(|f| f.native.clone());
        match (&signal, native) {
            // The session began here: a new one after `/clear`, or the one taken up.
            (Signal::Began, Some(native)) if current.as_deref() != Some(native.as_str()) => {
                term.ended.remove(&native);
                let cwd = hook.cwd.clone().unwrap_or_default();
                let transcript = hook.transcript_path.clone().map_or_else(
                    || sessions::transcript(&self.place.home, Path::new(&cwd), &native),
                    PathBuf::from,
                );
                tracing::info!(%terminal, session = native, source = ?hook.source, "a Claude Code's session, by its hook");
                let (version, since) = (term.version.clone(), term.since);
                self.follow(Begin { terminal, native, cwd, version, transcript, since });
                self.say(terminal, Say::Signal(Signal::Began, at, hook.prompt_id));
            }
            (Signal::Ended, Some(native)) if current.as_deref() == Some(native.as_str()) => {
                tracing::info!(%terminal, session = native, reason = ?hook.reason, "a Claude Code's session ended");
                self.end(terminal, at);
                if let Some(term) = self.terms.get_mut(&terminal) {
                    term.ended.insert(native);
                }
            }
            // A session no tick has named yet (its record unread): its hook names its files.
            (_, Some(native)) if current.is_none() && hook.transcript_path.is_some() => {
                let cwd = hook.cwd.clone().unwrap_or_default();
                let transcript = hook.transcript_path.map(PathBuf::from).unwrap_or_default();
                let (version, since) = (term.version.clone(), term.since);
                self.follow(Begin { terminal, native, cwd, version, transcript, since });
                self.say(terminal, Say::Signal(signal, at, hook.prompt_id));
            }
            (_, Some(native)) if current.as_deref() != Some(native.as_str()) => {
                tracing::debug!(%terminal, session = native, event = hook.event, "a hook of a session not followed here");
            }
            _ => self.say(terminal, Say::Signal(signal, at, hook.prompt_id)),
        }
    }

    /// What Slopty's mod relayed from `terminal` of session `session`'s prompt box, as the relay
    /// started at `at_us`: told the session followed there when it is that one, unless a later
    /// draft was told already.
    fn drafted(
        &mut self,
        terminal: SessionId,
        at_us: u64,
        session: Option<&str>,
        text: String,
        tmpdir: Option<String>,
    ) {
        let Some(term) = self.terms.get_mut(&terminal) else { return };
        let ours = term.session.as_ref().is_some_and(|f| Some(f.native.as_str()) == session);
        if !ours || at_us < term.drafted_us {
            tracing::debug!(%terminal, ?session, "a draft of a session not followed here, or late");
            return;
        }
        term.drafted_us = at_us;
        let at = wall_of(at_us);
        self.say(terminal, Say::Draft { text, tmpdir, at });
    }

    /// Follow `at`'s session in its terminal, in place of the one there before.
    fn follow(&mut self, at: Begin) {
        let terminal = at.terminal;
        self.end(terminal, WallMs::now());
        let (native, version) = (at.native.clone(), at.version.clone());
        let thread = slopty_agent::claude::thread_of(&native);
        let (says, told) = mpsc::unbounded_channel();
        let reading =
            read(self.host.clone(), self.place.home.clone(), Arc::clone(&self.stores), at, told);
        drop(tokio::spawn(reading));
        let followed = Followed { native, thread, says, waits: None };
        let term = self.terms.entry(terminal).or_default();
        if !version.is_empty() {
            term.version = version;
        }
        term.session = Some(followed);
    }

    /// End the session followed in `terminal`, if one is, at `at`.
    fn end(&mut self, terminal: SessionId, at: WallMs) {
        let Some(followed) = self.terms.get_mut(&terminal).and_then(|t| t.session.take()) else {
            return;
        };
        let _gone = followed.says.send(Say::End(at));
    }

    fn say(&self, terminal: SessionId, say: Say) {
        if let Some(followed) = self.terms.get(&terminal).and_then(|t| t.session.as_ref()) {
            let _gone = followed.says.send(say);
        }
    }
}

/// The wall time `at_us` microseconds after the epoch.
fn wall_of(at_us: u64) -> WallMs {
    let ms = Duration::from_micros(at_us).as_millis();
    WallMs::from_millis(u64::try_from(ms).unwrap_or(u64::MAX))
}

/// The record Claude Code `pid` keeps of itself, or the one its child keeps when `pid` is a
/// managed launcher that runs the real client beneath it.
fn record_of(home: &Path, pid: u32) -> Option<Live> {
    if let Some(live) = sessions::live_of(home, pid) {
        return Some(live);
    }
    let pid = i32::try_from(pid).ok()?;
    let children = crate::ports::child_pids(pid);
    let below = children.iter().flat_map(|child| crate::ports::child_pids(*child));
    children
        .iter()
        .copied()
        .chain(below)
        .filter_map(|child| u32::try_from(child).ok())
        .find_map(|child| sessions::live_of(home, child))
}

/// How often a followed session's files are looked at besides the kernel's word: the output
/// of its background commands, and a volume that cannot report a write.
const LOOK: Duration = Duration::from_secs(1);

/// Follow one session: its thread begun (again), its files read as they grow, what its hooks
/// say applied, until it ends.
async fn read(
    host: Host,
    home: PathBuf,
    stores: Stores,
    at: Begin,
    mut heard: mpsc::UnboundedReceiver<Say>,
) {
    let terminal = at.terminal;
    let mut reader = Reader::begin(&host, &home, stores, at);
    // The follower takes each list sent after it starts, the first one too.
    let (paths, lists) = watch::channel(Vec::new());
    let mut files = crate::fswatch::follow(lists, crate::fswatch::Limits::default());
    paths.send_replace(reader.paths());
    let mut look = tokio::time::interval(LOOK);
    look.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            changed = files.next() => if changed.is_none() { break },
            _ = look.tick() => {}
            say = heard.recv() => match say {
                None => break,
                Some(Say::End(at)) => {
                    let Some(back) = reader.read_off(&host).await else { return };
                    reader = back;
                    reader.end(&host, at);
                    return;
                }
                Some(Say::Draft { text, tmpdir, at }) => {
                    let Some(back) = reader.draft_off(&host, text, tmpdir, at).await else { return };
                    reader = back;
                    continue;
                }
                Some(say) => {
                    // A hook comes once Claude Code wrote what it says of: the transcript is
                    // read first, so a turn's end never shows before the turn, a dialog its
                    // record says is open finds the call it is about, and a call's run time
                    // lands on the call.
                    if matches!(say, Say::Signal(..) | Say::Ran(..) | Say::Waits(..)) {
                        let Some(back) = reader.read_off(&host).await else { return };
                        reader = back;
                    }
                    reader.say(&host, say);
                    continue;
                }
            },
        }
        let Some(back) = reader.read_off(&host).await else { return };
        reader = back;
        paths.send_if_modified(|now| {
            let next = reader.paths();
            let moved = *now != next;
            *now = next;
            moved
        });
    }
    tracing::debug!(%terminal, "a session's follower stopped");
}

/// One session's thread as its records and hooks make it.
struct Reader {
    history: History,
    transcripts: Transcripts,
    main: PathBuf,
    cwd: String,
    home: PathBuf,
    /// Whether a hook was heard: the phase follows the hooks then, not the record or the
    /// transcript.
    hooked: bool,
    /// What the record says the session waits on the person for ([`Say::Waits`]), in its own
    /// words: the transcript leaves the phase alone until the record no longer does, and the
    /// call it writes meanwhile words the wait.
    asked: Option<Wait>,
    /// The calls the transcript has written with no result yet, the latest last, while no hook
    /// is heard: what a dialog the record says is open is about.
    open: Vec<Open>,
    /// Its prompt box's pictures, as Claude Code stores them.
    pictures: Pictures,
    /// Where each thread's box keeps its pictures, which this one's tells.
    stores: Stores,
    /// A draft one of whose pictures was not there yet, looked for again.
    relook: Option<Relook>,
}

/// A draft looked for again, its picture not yet stored when it came.
struct Relook {
    text: String,
    tmpdir: Option<String>,
    at: WallMs,
    /// When it is looked for next.
    next: tokio::time::Instant,
    /// How many more looks it gets.
    left: u8,
}

/// How many times a draft whose picture is not there is looked for again, a [`LOOK`] apart: a
/// placeholder recalled from another session names a picture this one never stored.
const RELOOKS: u8 = 3;

/// A call written with no result yet.
struct Open {
    thread: slopty_agent::conversation::ThreadId,
    id: String,
    name: String,
    input: serde_json::Value,
}

/// The most calls kept open at once: a session's dialogs are about its latest.
const OPEN_CALLS: usize = 64;

/// Claude Code's tool that asks the person a question, and the one that asks them to approve a
/// plan: a dialog about either is a question or a plan, whatever the record calls it.
const QUESTION_TOOL: &str = "AskUserQuestion";
const PLAN_TOOL: &str = "ExitPlanMode";

/// The wait kinds a dialog is said by (`slopty_proto::thread::Wait`).
const PERMISSION: &str = "permission";
const QUESTION: &str = "question";
const PLAN: &str = "plan";

impl Reader {
    /// Begin `at`'s thread: the one held of its session taken up again from nothing, else a new
    /// one, naming its terminal.
    fn begin(host: &Host, home: &Path, stores: Stores, at: Begin) -> Self {
        let Begin { terminal, native, cwd, version, transcript, since } = at;
        let thread = slopty_agent::claude::thread_of(&native);
        let now = WallMs::now();
        let mut history = match host.meta(thread) {
            Some((mut meta, _status)) => {
                // Read, not driven: nothing to switch to from here.
                meta.drive = Drive::named(Drive::OBSERVED);
                History::of(meta)
            }
            None => History::new(&native, &version, &cwd, now),
        };
        take(host, history.drain());
        if !version.is_empty() {
            take(host, history.version(&version));
        }
        take(host, history.fact(ThreadMeta::TERMINAL_FACT, Some(&terminal.to_string())));
        take(host, history.status(Phase::Idle, None, Liveness::Live, now));
        if let Some(since) = since {
            take(host, history.run_began(since));
        }
        let uid = rustix::process::getuid().as_raw();
        let pictures = Pictures::new(uid, Path::new(&cwd), &native);
        Self {
            history,
            transcripts: Transcripts::default(),
            main: transcript,
            cwd,
            home: home.to_path_buf(),
            hooked: false,
            asked: None,
            open: Vec::new(),
            pictures,
            stores,
            relook: None,
        }
    }

    /// Take what the prompt box holds now ([`Self::draft`]), off the runtime's threads: its
    /// pictures are files. `None` when that panicked.
    async fn draft_off(
        mut self,
        host: &Host,
        text: String,
        tmpdir: Option<String>,
        at: WallMs,
    ) -> Option<Self> {
        let host = host.clone();
        tokio::task::spawn_blocking(move || {
            self.relook = None;
            self.draft(&host, text, tmpdir, at, RELOOKS);
            self
        })
        .await
        .inspect_err(|e| tracing::warn!(error = %e, "a prompt box's draft was not read"))
        .ok()
    }

    /// The prompt box holds `text` since `at`, its pictures under the temporary root `tmpdir`:
    /// the thread's draft, with each picture stored by now, and the draft looked for again
    /// (`looks` more times) while one is not.
    fn draft(&mut self, host: &Host, text: String, tmpdir: Option<String>, at: WallMs, looks: u8) {
        let (draft, missing) = self.pictures.draft(&text, tmpdir.as_deref(), at);
        let thread = self.history.main();
        match (draft.as_ref().is_some_and(|d| !d.images.is_empty()), self.pictures.dir()) {
            (true, Some(store)) => drop(self.stores.lock().insert(thread, store.to_path_buf())),
            _ => drop(self.stores.lock().remove(&thread)),
        }
        take(host, self.history.draft(draft));
        self.relook = (missing && looks > 0).then(|| Relook {
            text,
            tmpdir,
            at,
            next: tokio::time::Instant::now()
                .checked_add(LOOK)
                .unwrap_or_else(tokio::time::Instant::now),
            left: looks.saturating_sub(1),
        });
    }

    /// The files to follow: the main transcript, and every subagent file found so far.
    fn paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self
            .transcripts
            .read_files()
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let main = self.main.to_string_lossy().into_owned();
        if !paths.contains(&main) {
            paths.insert(0, main);
        }
        let agents = slopty_agent::conversation::subagents_dir(&self.main);
        paths.push(agents.to_string_lossy().into_owned());
        paths
    }

    /// Read what the files gained, off the runtime's threads. `None` when the reading panicked.
    async fn read_off(mut self, host: &Host) -> Option<Self> {
        let host = host.clone();
        tokio::task::spawn_blocking(move || {
            self.read(&host);
            self
        })
        .await
        .inspect_err(|e| tracing::warn!(error = %e, "a session's transcript read failed"))
        .ok()
    }

    fn read(&mut self, host: &Host) {
        if let Some(relook) = self.relook.take_if(|r| r.next <= tokio::time::Instant::now()) {
            self.draft(host, relook.text, relook.tmpdir, relook.at, relook.left);
        }
        let changes = self.transcripts.read(&self.main, &[]);
        let outputs = self.transcripts.outputs();
        if changes.is_empty() && outputs.is_empty() {
            return;
        }
        if !self.hooked {
            calls(&mut self.open, &changes);
        }
        take(host, self.history.transcript(&changes, &outputs));
        take(host, self.phase_from_transcript());
        take(host, self.reworded());
    }

    /// What the transcript says of the phase: the whole of it with no hooks, an interrupt
    /// alone with them (it fires none).
    fn phase_from_transcript(&mut self) -> Vec<Out> {
        let Some(turn) = self.history.last_turn() else { return Vec::new() };
        let (state, started, ended) = (turn.state.clone(), turn.started_ms, turn.ended_ms);
        let status = self.history.phase().cloned();
        let at_work =
            status.as_ref().is_some_and(|s| matches!(s.phase, Phase::Working | Phase::NeedsYou));
        let since = status.as_ref().map_or(WallMs::ZERO, |s| s.since_ms);
        let phase = match state {
            TurnState::Interrupted if at_work && ended.is_some_and(|e| e >= since) => {
                Phase::Stopped
            }
            _ if self.hooked || self.asked.is_some() => return Vec::new(),
            state => phase_of(&state),
        };
        let at = ended.unwrap_or(started);
        if let Some(had) = status.filter(|had| had.phase == phase) {
            // The same phase keeps its time and whether its agent runs, but for a later turn's
            // end: a turn the record alone shows (a task's notice) ended then.
            let ends = matches!(phase, Phase::Done | Phase::Failed | Phase::Stopped);
            if !ends || had.since_ms >= at {
                return Vec::new();
            }
            return self.history.status(phase, None, had.liveness, at);
        }
        self.history.status(phase, None, Liveness::Live, at)
    }

    fn say(&mut self, host: &Host, say: Say) {
        let outs = match say {
            Say::Signal(signal, at, prompt) => {
                self.hooked = true;
                self.asked = None;
                self.open.clear();
                self.signal(signal, prompt.as_deref(), at)
            }
            Say::Ran(ran, at) => self.history.ran(&ran, at),
            Say::Waits(wait, at) => self.waits(wait, at),
            Say::Named(name) => self.history.named(&name),
            Say::Version(version) => self.history.version(&version),
            Say::Draft { text, tmpdir, at } => {
                self.draft(host, text, tmpdir, at, RELOOKS);
                Vec::new()
            }
            Say::End(at) => {
                self.end(host, at);
                Vec::new()
            }
        };
        take(host, outs);
    }

    /// What the record says the session waits on since `at`, for a session none of whose hooks
    /// is heard: waiting on the person while it does, and what the transcript says once it no
    /// longer does.
    fn waits(&mut self, wait: Option<Wait>, at: WallMs) -> Vec<Out> {
        let live = Liveness::Live;
        if self.hooked {
            return Vec::new();
        }
        match wait {
            Some(wait) => {
                self.asked = Some(wait.clone());
                let wait = self.worded(wait);
                self.history.status(Phase::NeedsYou, Some(wait), live, at)
            }
            None if self.asked.take().is_some() => {
                let phase =
                    self.history.last_turn().map_or(Phase::Idle, |turn| phase_of(&turn.state));
                self.history.status(phase, None, live, at)
            }
            None => Vec::new(),
        }
    }

    /// The record's wait said anew of the call the transcript has open now, since the dialog
    /// opened: Claude Code writes a call a moment after its dialog opens, and the record may be
    /// read between. A call's result leaves the wait as it was said.
    fn reworded(&mut self) -> Vec<Out> {
        let Some(asked) = self.asked.clone() else { return Vec::new() };
        if self.open.is_empty() {
            return Vec::new();
        }
        let wait = self.worded(asked);
        let Some(status) = self.history.phase() else { return Vec::new() };
        // An interruption the transcript shows ends the dialog, whatever the record still says.
        if status.phase != Phase::NeedsYou || status.wait.as_ref() == Some(&wait) {
            return Vec::new();
        }
        let since = status.since_ms;
        self.history.status(Phase::NeedsYou, Some(wait), Liveness::Live, since)
    }

    /// `wait` said of the call it is about ([`worded`]): the latest open, since a dialog holds
    /// the session until it is answered.
    fn worded(&self, wait: Wait) -> Wait {
        worded(self.open.last(), wait)
    }

    /// What hook `signal` heard at `at` says, of the turn of `prompt` (the hook's `prompt_id`):
    /// a `Stop` ends that turn, whenever the transcript shows it.
    fn signal(&mut self, signal: Signal, prompt: Option<&str>, at: WallMs) -> Vec<Out> {
        let live = Liveness::Live;
        match signal {
            Signal::Began => self.history.status(Phase::Idle, None, live, at),
            Signal::Working => self.history.status(Phase::Working, None, live, at),
            Signal::Asks { kind, text } => {
                let wait = Wait { kind: kind.to_owned(), text };
                self.history.status(Phase::NeedsYou, Some(wait), live, at)
            }
            // The turn of the prompt it names ends with its Stop, before or after the records
            // that close it.
            Signal::Done => {
                let mut outs = self.history.turn_ended(prompt, &End::Complete, at);
                outs.extend(self.history.status(Phase::Done, None, live, at));
                outs
            }
            Signal::Failed(_) => {
                let mut outs = self.history.turn_ended(prompt, &End::Failed(None), at);
                outs.extend(self.history.status(Phase::Failed, None, live, at));
                outs
            }
            Signal::Compacting(on) => self.history.compacting(on, at),
            // A draft goes its own way ([`Say::Draft`]).
            Signal::Ended | Signal::Nothing | Signal::Draft { .. } => Vec::new(),
        }
    }

    /// The session ended here at `at`: exited and resumable, its terminal no longer named.
    fn end(&mut self, host: &Host, at: WallMs) {
        self.relook = None;
        drop(self.stores.lock().remove(&self.history.main()));
        let mut outs = self.history.gone(at);
        outs.extend(self.history.fact(ThreadMeta::TERMINAL_FACT, None));
        take(host, outs);
        tracing::debug!(cwd = self.cwd, home = %self.home.display(), "a session's thread ended");
    }
}

/// The phase a turn in `state` leaves the session in.
const fn phase_of(state: &TurnState) -> Phase {
    match state {
        TurnState::Active => Phase::Working,
        TurnState::Complete => Phase::Done,
        TurnState::Interrupted => Phase::Stopped,
        TurnState::Failed { .. } => Phase::Failed,
    }
}

/// End `thread`, which no follower holds: exited and resumable, naming no terminal.
fn ended(host: &Host, thread: ThreadId, at: WallMs) {
    host.update(thread, |state| {
        let mut meta = state.meta.clone();
        let mut actions = Vec::new();
        if meta.facts.remove(ThreadMeta::TERMINAL_FACT).is_some() {
            actions.push(Action::Meta(Box::new(meta)));
        }
        // The prompt box and the queue were its TUI's, gone with it.
        if state.draft.is_some() {
            actions.push(Action::DraftSet(None));
        }
        if !state.queued.is_empty() {
            actions.push(Action::QueuedSet(Vec::new()));
        }
        if state.status.liveness == Liveness::Live {
            let phase = match state.status.phase {
                Phase::Working | Phase::NeedsYou | Phase::Waiting => Phase::Stopped,
                other => other,
            };
            let liveness = Liveness::Exited { resumable: true };
            let status = slopty_proto::thread::Status { phase, wait: None, liveness, since_ms: at };
            actions.push(Action::Status(status));
        }
        (actions, ())
    });
}

/// Keep in `open` the calls `changes` leave open, the latest last, at most [`OPEN_CALLS`].
fn calls(open: &mut Vec<Open>, changes: &[Change]) {
    for change in changes {
        match change {
            Change::Upsert { thread, entry } => {
                let Body::Tool(call) = &entry.body else { continue };
                open.retain(|o| o.id != entry.id);
                if call.result.is_none() {
                    let input = slopty_agent::conversation::call_input(call);
                    let (thread, id, name) = (thread.clone(), entry.id.clone(), call.name.clone());
                    open.push(Open { thread, id, name, input });
                }
            }
            Change::Remove { id, .. } => open.retain(|o| o.id != *id),
            Change::Reset { thread: Some(thread) } => open.retain(|o| o.thread != *thread),
            Change::Reset { thread: None } => open.clear(),
            Change::Tasks { .. } | Change::Turn { .. } | Change::Queued(_) => {}
        }
    }
    let over = open.len().saturating_sub(OPEN_CALLS);
    drop(open.drain(..over));
}

/// `wait` said of the call `open` it is about, when the record says a permission or a question
/// and the transcript has a call open: a question or a plan when the call asks one, whatever
/// the record calls it. A dialog of Claude Code's own (input) is said in the record's words.
fn worded(open: Option<&Open>, wait: Wait) -> Wait {
    let Some(open) = open else { return wait };
    let kind = match (open.name.as_str(), wait.kind.as_str()) {
        (QUESTION_TOOL, PERMISSION | QUESTION) => QUESTION,
        (PLAN_TOOL, PERMISSION) => PLAN,
        (_, PERMISSION) => PERMISSION,
        _ => return wait,
    };
    Wait { kind: kind.to_owned(), text: asks_for(&open.name, &open.input) }
}

/// The session thread `thread` is (or is a subagent of).
fn root_of(host: &Host, thread: ThreadId) -> Option<ThreadMeta> {
    let mut at = thread;
    for _ in 0..16 {
        let (meta, _) = host.meta(at)?;
        if meta.origin != ThreadMeta::SUBAGENT {
            return Some(meta);
        }
        at = meta.parent?.thread;
    }
    None
}

/// Put what the mapping made into the host: a thread begun anew if the host holds it already.
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

#[cfg(test)]
mod tests {
    use slopty_proto::thread::{ItemId, Link};

    use super::*;

    /// A dialog the record says is open is said of the call it is about: a question or a plan
    /// when the call asks one, whatever the record calls it, a permission in the call's words;
    /// one of Claude Code's own, or with no call open, in the record's words.
    #[test]
    fn a_dialog_is_said_of_the_call_it_is_about() {
        let open = |name: &str, input: serde_json::Value| Open {
            thread: slopty_agent::conversation::ThreadId::Main,
            id: "toolu_1".to_owned(),
            name: name.to_owned(),
            input,
        };
        let said = |kind: &str| Wait { kind: kind.to_owned(), text: "the record's".to_owned() };
        let question = serde_json::json!({ "questions": [{ "question": "Which one?" }] });
        let asks = open(QUESTION_TOOL, question);
        let words = |kind: &str, call: &Open| Wait {
            kind: kind.to_owned(),
            text: asks_for(&call.name, &call.input),
        };
        assert_eq!(worded(Some(&asks), said(PERMISSION)), words(QUESTION, &asks));
        assert_eq!(worded(Some(&asks), said(QUESTION)), words(QUESTION, &asks));
        let plan = open(PLAN_TOOL, serde_json::json!({ "plan": "Do it" }));
        assert_eq!(worded(Some(&plan), said(PERMISSION)), words(PLAN, &plan));
        let bash = open("Bash", serde_json::json!({ "command": "ls" }));
        assert_eq!(worded(Some(&bash), said(PERMISSION)), words(PERMISSION, &bash));
        assert_eq!(worded(Some(&bash), said("input")), said("input"));
        assert_eq!(worded(None, said(PERMISSION)), said(PERMISSION));
        assert_ne!(words(QUESTION, &asks).text, "the record's", "the call's own words");
    }

    /// The calls open are the ones a change leaves without a result, the latest last: a call
    /// read again moves to the end, one whose result came or that was removed goes, a thread's
    /// file started over drops that thread's, every file started over drops them all, and only
    /// the latest [`OPEN_CALLS`] are kept.
    #[test]
    fn the_calls_open_are_those_without_a_result() {
        use slopty_agent::conversation::{
            Clipped, Entry, ResultStatus, ThreadId as Conversation, ToolCall, ToolDetail,
            ToolResult,
        };
        let clipped = |text: &str| Clipped {
            text: text.to_owned(),
            lines: 1,
            chars: u32::try_from(text.len()).unwrap(),
            full: None,
        };
        let done = || ToolResult {
            status: ResultStatus::Ok,
            text: None,
            at_ms: WallMs::ZERO,
            images: Vec::new(),
            said: None,
        };
        let call = |thread: &Conversation, id: &str, ended: bool| Change::Upsert {
            thread: thread.clone(),
            entry: Entry {
                id: id.to_owned(),
                at_ms: WallMs::ZERO,
                body: Body::Tool(Box::new(ToolCall {
                    name: PLAN_TOOL.to_owned(),
                    input: clipped("{}"),
                    detail: ToolDetail::Plan { plan: clipped("Do it") },
                    result: ended.then(done),
                    title: None,
                })),
            },
        };
        let ids = |open: &[Open]| open.iter().map(|o| o.id.clone()).collect::<Vec<_>>();
        let (main, agent) = (Conversation::Main, Conversation::Agent("a1".to_owned()));
        let mut open = Vec::new();
        calls(&mut open, &[call(&main, "t1", false), call(&agent, "t2", false)]);
        calls(&mut open, &[call(&main, "t3", false)]);
        assert_eq!(ids(&open), ["t1", "t2", "t3"]);
        calls(&mut open, &[call(&main, "t1", false)]);
        assert_eq!(ids(&open), ["t2", "t3", "t1"], "read again, it is the latest");
        calls(&mut open, &[call(&main, "t3", true)]);
        assert_eq!(ids(&open), ["t2", "t1"], "its result came");
        calls(&mut open, &[Change::Remove { thread: main.clone(), id: "t1".to_owned() }]);
        assert_eq!(ids(&open), ["t2"], "removed");
        calls(&mut open, &[call(&main, "t4", false), Change::Reset { thread: Some(agent) }]);
        assert_eq!(ids(&open), ["t4"], "the subagent's file started over");
        calls(&mut open, &[Change::Reset { thread: None }]);
        assert!(open.is_empty(), "every file started over");
        let many: Vec<Change> = (0..OPEN_CALLS.saturating_add(2))
            .map(|n| call(&main, &format!("m{n}"), false))
            .collect();
        calls(&mut open, &many);
        assert_eq!(open.len(), OPEN_CALLS);
        assert_eq!(open.first().map(|o| o.id.as_str()), Some("m2"), "the oldest go first");
    }

    /// A subagent's session is the thread at the top of its chain; a chain that ends before a
    /// session, or a thread not kept, has none.
    #[test]
    fn a_subagent_belongs_to_the_session_at_the_top_of_its_chain() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::open(dir.path(), crate::thread::log::Limits::default()).unwrap();
        let meta = |origin: &str, parent: Option<ThreadId>| ThreadMeta {
            id: ThreadId::new(),
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            agent_version: String::new(),
            native: ThreadId::new().to_string(),
            cwd: "/work".to_owned(),
            title: String::new(),
            parent: parent.map(|thread| Link { thread, item: ItemId("toolu_1".to_owned()) }),
            origin: origin.to_owned(),
            drive: Drive::named(Drive::OBSERVED),
            facts: std::collections::BTreeMap::new(),
            created_ms: WallMs::from_millis(1),
        };
        let kept = |meta: ThreadMeta| {
            let id = meta.id;
            host.create(meta).unwrap();
            id
        };
        let session = kept(meta(ThreadMeta::PERSON, None));
        let agent = kept(meta(ThreadMeta::SUBAGENT, Some(session)));
        let nested = kept(meta(ThreadMeta::SUBAGENT, Some(agent)));
        let root = |thread| root_of(&host, thread).map(|meta| meta.id);
        assert_eq!(root(session), Some(session));
        assert_eq!(root(agent), Some(session));
        assert_eq!(root(nested), Some(session));
        let orphan = kept(meta(ThreadMeta::SUBAGENT, Some(ThreadId::new())));
        assert_eq!(root(orphan), None, "its parent is not kept");
        let loose = kept(meta(ThreadMeta::SUBAGENT, None));
        assert_eq!(root(loose), None, "it names no parent");
        assert_eq!(root(ThreadId::new()), None, "not kept");
    }
}
