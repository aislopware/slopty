//! `WorkspaceView`: every worker this client reaches, and what the shell (`slopty-shell`) is
//! fed from them through the view model ([`crate::view_model`]).
//!
//! One model for every worker: each worker's item registry is mirrored
//! ([`slopty_client::ItemDoc`]), with its sessions, its link and its thread hub. It draws
//! nothing of its own: the shell draws, and the live terminals, windows and previews it hosts
//! are this model's views ([`crate::view_model::live`]).
//!
//! * `workers` — connecting, losing and forgetting a worker; the sync that follows.
//! * `agents` — coding agents in terminals: badges, banners, "needs you".
//! * [`remote`] — the clipboard shared with the workers, files to and from them, forwarded ports.
//! * `approvals` — "Allow" and "Deny" for an agent that waits, from its note.
//! * `turns` — agents' turns that ended unread.
//! * [`attention`] — what reaches the person as a note, and where.

mod agent_start;
mod agents;
mod approvals;
pub mod attention;
mod commands;
mod desktop;
mod faces;
mod handoffs;
mod keys;
mod presence;
pub mod remote;
#[cfg(test)]
mod rig;
mod server_calls;
mod turns;
mod view_model;
mod workers;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

pub use agents::{banner_title, program_banner};
use gpui::{Context, Entity, EventEmitter, Task};
pub use handoffs::OFFER_FOR;
pub use keys::worker_key;
use slopty_client::relay::RelayWatch;
use slopty_client::{ItemDoc, ItemRef, WorkerKey};
use slopty_core::{ClientId, ItemId, SessionId};
use slopty_proto::ClientMsg;
use slopty_proto::items::{Item, ItemOp};
use slopty_proto::screen::CaptureTarget;
use slopty_proto::server::WorkerCaps;
use slopty_proto::terminal::SessionSummary;
use slopty_terminal::TerminalView;
use slopty_theme::Theme;
use tokio::sync::mpsc;
pub use view_model::{
    AddAnswer, AddChoice, AddDevice, AddFailure, AddFix, AddKind, AddMark, AddProgress, AddStep,
    Found, FoundNode, Look,
};

use crate::screen::{ScreenFactory, ScreenView};

/// How long a shell command or an agent's turn has to run before its end, unwatched, is worth a
/// mark: shorter ones end before the person has looked away.
pub const SLOW_COMMAND: Duration = Duration::from_secs(30);

/// Things the surrounding app reacts to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum WorkspaceEvent {
    /// A terminal rang its bell.
    Bell(SessionId),
    /// A coding agent's thread came to need the human (an approval, a question), as its
    /// worker's table says, or the server's ladder for a worker not linked here. Led by a
    /// server, its notices say this instead ([`attention::Attention::server_led`]).
    Attention(slopty_proto::thread::ThreadId),
    /// A program in this session asked for a desktop notification (`OSC 9`, `OSC 777`), or a
    /// status record of its came to wait on the person (`OSC 7501`): this client's own moment,
    /// never the server's.
    Program(SessionId),
    /// How many agents are waiting on the human right now, across every worker (the Dock
    /// badge).
    NeedsYou(usize),
    /// A note's "Allow", "Deny" or pick found no prompt to answer, or was not taken, while the
    /// app was away: the app says `why` in a note of its own about `route`'s session.
    Unanswered {
        /// The note's agent.
        route: attention::Route,
        /// What to say: the app's words, or those of whoever turned the answer down.
        why: String,
    },
    /// The person stopped or started sharing the clipboard with a machine: the app keeps it in
    /// the settings by the machine's name.
    ClipboardShared {
        /// The machine.
        worker: WorkerKey,
        /// Shared now, or not.
        share: bool,
    },
    /// Every note's "Allow" or "Deny" tapped so far was sent, or was said to have found
    /// nothing: an app woken in the background to send one may be let go once it is out.
    TapsSettled,
    /// Settings → Devices asked to add a device or follow a server: the app takes the run with
    /// [`WorkspaceView::take_add_device`].
    AddDevice,
    /// Settings → Devices asked what is on the tailnet: the app takes the look with
    /// [`WorkspaceView::take_look`].
    LookOnTailnet,
    /// Settings → Devices pressed one of the actions the app offers for a machine
    /// ([`WorkspaceView::set_device_actions`]), or for the server when `worker` is `None`.
    DeviceAction {
        /// The machine, or `None` for the server.
        worker: Option<WorkerKey>,
        /// The action's id, as the app named it.
        action: String,
    },
    /// Something to tell the person (a paste refused, an upload that landed elsewhere), or
    /// something that went wrong: the app shows it as a toast.
    Notice {
        /// What to say.
        text: String,
        /// It went wrong.
        failure: bool,
    },
    /// A page a shell on `worker` handed over, held back: a notice saying `text` with an Open
    /// button that opens `url`, for [`OFFER_FOR`]. A page on the worker's `loopback` opens at
    /// the port that forwards it here when Open is pressed
    /// ([`WorkspaceView::open_loopback_page`]).
    Offer {
        /// The worker whose shell asked.
        worker: WorkerKey,
        /// Its handoff, which the worker may take back.
        id: slopty_proto::handoff::HandoffId,
        /// Who wants to open which host, and why it was held back.
        text: String,
        /// What Open opens.
        url: String,
        /// Whether `url` is on the worker's own loopback.
        loopback: bool,
    },
    /// A path a person ⌘-clicked in a shell opened for `chat`: the shell opens `target` (a
    /// `file://` link, its line as `#L12`) in that chat's Files pane.
    OpenFile {
        /// The chat the shell was opened for.
        chat: String,
        /// The file, absolute on the shell's machine.
        target: String,
    },
    /// A block or a selection attached in a shell opened for `chat`: the shell puts `text` in
    /// that chat's draft, at the caret.
    Attach {
        /// The chat the shell was opened for.
        chat: String,
        /// The context, fenced and named after its command.
        text: String,
    },
    /// The worker took handoff `id` back: its notice goes.
    Withdrawn {
        /// The worker.
        worker: WorkerKey,
        /// The handoff.
        id: slopty_proto::handoff::HandoffId,
    },
}

/// Where a worker's link stands.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum WorkerStatus {
    /// First attempt in progress.
    Connecting,
    /// Link up.
    Connected,
    /// Link up but no pong for this many seconds (pings go every 2 s, `slopty_net::liveness`).
    Silent(u64),
    /// Link up, but the device was away or the path moved under it: a probe is out, and the
    /// tiles are set back until it answers.
    Checking,
    /// The probe went unanswered and a new link is being dialled at once: the tiles keep what
    /// they showed, set back, until it lands.
    Relinking,
    /// Link lost or the attempt failed; retrying, with the reason.
    Reconnecting(String),
    /// Nothing answered at its address: it is asleep, off the tailnet, or its worker stopped.
    /// Still dialled.
    NoAnswer,
    /// Its address names nothing here: the name does not resolve. Still dialled.
    NoSuchHost,
    /// It answered and turned this device away by its address, which its `[network] allow`
    /// ranges do not hold: this device's address as that path shows it, when it can be told.
    /// Still dialled, as the file is read again when it changes.
    Refused(Option<String>),
    /// The server says the worker went quiet; dialled again when it is back online.
    Unreachable,
    /// The server has not heard from the worker for long enough to presume it gone.
    Gone,
    /// The worker, or the server listing it, turned this device away: the tailnet policy grants
    /// it no role there. Still retried, as a policy change can grant it.
    NotGranted,
    /// The worker runs a different build, whose wire this one cannot read: said with both
    /// builds and what updates it, and dialled again only after a long wait or a nudge.
    NeedsUpdate(slopty_client::update::UpdateNotice),
}

impl WorkerStatus {
    /// Short text for the bar and the menus.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::Connecting | Self::Relinking => "reconnecting…".to_owned(),
            Self::Connected => "connected".to_owned(),
            Self::Silent(secs) => {
                format!("silent {}", slopty_kit::kit::duration(Duration::from_secs(*secs)))
            }
            Self::Checking => "checking…".to_owned(),
            Self::Reconnecting(why) => format!("{why}; reconnecting…"),
            Self::NoAnswer => "not answering".to_owned(),
            Self::NoSuchHost => "not found".to_owned(),
            Self::Refused(_) => "closed to this device by its allow ranges".to_owned(),
            Self::Unreachable => "unreachable".to_owned(),
            Self::Gone => "gone".to_owned(),
            Self::NotGranted => "closed to this device by the tailnet policy".to_owned(),
            Self::NeedsUpdate(_) => "runs a different build".to_owned(),
        }
    }

    /// Whether the worker is reachable right now: a link being checked still is, as far as
    /// anything knows.
    #[must_use]
    pub const fn is_up(&self) -> bool {
        matches!(self, Self::Connected | Self::Checking)
    }
}

/// A live connection to one worker: who this client is there and where its messages go.
#[derive(Clone)]
pub struct WorkerLink {
    /// This client's id on that worker's wire.
    pub me: ClientId,
    /// The control stream.
    pub out: mpsc::Sender<ClientMsg>,
    /// Opens a stream view for an `Opened` screen.
    pub open_screen: ScreenFactory,
    /// Files and clipboard bytes to and from the worker; `None` in a test without them.
    pub remote: Option<std::sync::Arc<dyn slopty_client::remote::Remote>>,
}

impl std::fmt::Debug for WorkerLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerLink").field("me", &self.me).finish_non_exhaustive()
    }
}

/// One worker as the workspace keeps it: its registry, its sessions and its link, which comes
/// and goes while the registry mirror stays.
struct Worker {
    name: String,
    status: WorkerStatus,
    link: Option<WorkerLink>,
    /// The link's control stream with what waits for room in it; `Some` while linked.
    outbox: Option<crate::outbox::Outbox>,
    /// Since when it has been out of reach on this client's clock: added and not yet linked,
    /// or since its link dropped; `None` while linked.
    away_since: Option<Duration>,
    /// How many links have come up to it: the self-test's proof that a relink landed.
    links: u64,
    doc: ItemDoc,
    sessions: HashMap<SessionId, SessionSummary>,
    rtt: Option<Duration>,
    /// How the tailnet carries the link, once the worker has said (never on a loopback or LAN
    /// link), and since when it has been on a DERP relay.
    relay: RelayWatch,
    /// Its home directory as its hello said, so a path under it reads `~/…`; `None` until a
    /// link has said, or when the daemon has none.
    home: Option<String>,
    /// Where this client reaches it (its tailnet name or address, no port), as the app dials
    /// it; `None` until the app says.
    address: Option<String>,
    /// Where its `settings.toml` is, as its hello said; `None` until a link has said, or when
    /// the daemon has none.
    settings: Option<String>,
    /// What it can do, as its link or the server's directory last said.
    caps: Option<WorkerCaps>,
    /// It answers on this wire from an older build: what updates it, beside a link that works
    /// ([`WorkspaceView::set_worker_behind`]).
    behind: Option<slopty_client::update::UpdateNotice>,
    /// Its one-minute load average, as its link or the server last said.
    load: Option<f32>,
    /// A `List` is in flight to name window items.
    titles_requested: bool,
    /// Streams requested but not yet `Opened`, by target.
    pending_opens: HashMap<CaptureTarget, ItemId>,
    /// Windows whose open the worker refused on this link, and why: not asked again until the
    /// next link or a retry.
    failed_opens: HashMap<ItemId, slopty_proto::screen::ScreenFailure>,
    /// Windows whose stream the worker ended unasked on this link ([`workers::Stopped`]):
    /// asked for again at once, and shown as stopped when it ends again soon after.
    stopped: HashMap<ItemId, workers::Stopped>,
    /// Windows whose view came over a link that has gone: it shows its last picture, set back,
    /// until a stream opened on the next link has one of its own.
    stale_screens: HashSet<ItemId>,
    /// Streams opened on this link for a stale window, waiting for their first picture to take
    /// its place.
    fresh_screens: HashMap<ItemId, Entity<ScreenView>>,
    /// The first snapshot since the link came up has not been applied yet.
    awaiting_snapshot: bool,
    /// What the human did to this worker's items while it was out of reach (or before its
    /// first snapshot): replayed over the next snapshot and sent, in order.
    queued: Vec<ClientMsg>,
    /// The changes to its files this client asked for, until it answers each.
    fs_ops: slopty_client::folders::FsOps,
    /// The display it streams for this device's key, when a desktop of it shows one.
    sized: Option<desktop::Sized>,
    /// Where its curtain stands for this device.
    curtain: desktop::Curtain,
    /// The pages its shells handed over, as `slopty_client::handoff` keeps them.
    handoffs: slopty_client::handoff::Handoffs,
}

/// The most a worker out of reach holds for its return: far more than a human closes, names
/// and writes in a sitting, and a bound all the same.
const QUEUE_MAX: usize = 1024;

impl Worker {
    fn new(name: String) -> Self {
        Self {
            name,
            status: WorkerStatus::Connecting,
            link: None,
            outbox: None,
            away_since: None,
            links: 0,
            doc: ItemDoc::default(),
            sessions: HashMap::new(),
            rtt: None,
            relay: RelayWatch::default(),
            home: None,
            address: None,
            settings: None,
            caps: None,
            behind: None,
            load: None,
            titles_requested: false,
            pending_opens: HashMap::new(),
            failed_opens: HashMap::new(),
            stopped: HashMap::new(),
            stale_screens: HashSet::new(),
            fresh_screens: HashMap::new(),
            awaiting_snapshot: false,
            queued: Vec::new(),
            fs_ops: slopty_client::folders::FsOps::default(),
            sized: None,
            curtain: desktop::Curtain::default(),
            handoffs: slopty_client::handoff::Handoffs::default(),
        }
    }

    /// Whether the worker can hear this client right now.
    const fn is_linked(&self) -> bool {
        self.link.is_some()
    }

    /// Send `msg` if the worker is linked: into the control stream now, or behind what waits
    /// for room in it, in order. Nothing is dropped on a full stream but a message the next of
    /// its kind outdates (a watch set, a size; [`crate::outbox::hold_control`]). False when the
    /// worker is down, when it is lost: for what only means anything now (a read, a key, a
    /// stream request). See [`Self::send_or_queue`] for what must land even then.
    fn send(&self, msg: ClientMsg) -> bool {
        let Some(outbox) = self.outbox.as_ref().filter(|_| self.link.is_some()) else {
            tracing::debug!(kind = msg.kind(), "worker down; not sent");
            return false;
        };
        outbox.send(msg)
    }

    /// Send what the human did to the worker's items (an item op, a session closed): now if
    /// the worker is linked and has sent its snapshot, else when it next has, replayed over
    /// that snapshot so the tile closed or the page moved meanwhile stays as it was left.
    fn send_or_queue(&mut self, msg: ClientMsg) {
        if self.is_linked() && !self.awaiting_snapshot {
            self.send(msg);
            return;
        }
        if self.queued.len() >= QUEUE_MAX {
            tracing::warn!(kind = msg.kind(), "worker away too long; oldest queued op dropped");
            self.queued.remove(0);
        }
        self.queued.push(msg);
    }
}

/// A shell command, or an agent's turn, that finished while nobody was looking: what the
/// header badge says.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Finished {
    /// What was typed, or what the agent said of its turn.
    pub command: String,
    /// Its exit status, when the shell said.
    pub exit: Option<u8>,
    /// How long it ran.
    pub elapsed: Duration,
    /// The agent's turn it is, as its worker's table says it ended unread; none for a shell
    /// command.
    pub turn: Option<AgentTurn>,
}

/// An agent's turn that ended unread ([`Finished::turn`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AgentTurn {
    /// The thread it is a turn of.
    pub thread: slopty_proto::thread::ThreadId,
    /// Which turn.
    pub turn: slopty_proto::thread::TurnId,
    /// It ended while this device heard its worker, so a note may tell of it; one found
    /// unread in a worker's first table was told by whoever heard it end.
    pub fresh: bool,
}

impl Finished {
    /// The badge text: "Done · 3.2 s", "Exit 1 · 1m 4s", in [`slopty_kit::kit::duration`]'s words.
    #[must_use]
    pub fn label(&self) -> String {
        let took = slopty_kit::kit::duration(self.elapsed);
        match self.exit {
            Some(0) | None => format!("Done · {took}"),
            Some(code) => format!("Exit {code} · {took}"),
        }
    }
}

/// The workspace.
pub struct WorkspaceView {
    /// The theme the live views are drawn in.
    theme: Theme,
    /// The workspace's clock starts here.
    epoch: Instant,
    /// The workspace's clock held at a time, for tests that compare two moments at one instant.
    #[cfg(test)]
    held_clock: Option<Duration>,
    /// The round trip said for every linked worker in place of its link's, pinned by the e2e
    /// harness so a golden never carries the machine's live timing.
    pinned_rtt: Option<Duration>,
    workers: BTreeMap<WorkerKey, Worker>,
    // Per-item views. Item and session ids are UUIDv7, unique across workers, so one map
    // each serves every worker.
    terminals: HashMap<SessionId, Entity<TerminalView>>,
    screens: HashMap<ItemId, Entity<ScreenView>>,
    /// Window items' titles, as the worker's listing named them.
    titles: HashMap<ItemId, String>,
    /// What the server link says of itself, while it has something to say.
    server_status: Option<gpui::SharedString>,
    /// Sessions' and agents' ends that finished while nobody looked, by what they are about.
    finished: HashMap<attention::About, Finished>,
    /// How long a command has to run before its end is worth a mark.
    slow_command: Duration,
    /// Agents' approvals: the taps and answers on their way.
    approvals: approvals::Approvals,
    /// Agents' turns that ended unread.
    turns: turns::Turns,
    /// Each worker's thread hub.
    faces: faces::Faces,
    /// The shell, fed from here ([`crate::view_model`]).
    view_model: view_model::Bridge,
    /// The way to the server, while the app is linked to one.
    server: Option<slopty_client::server::ServerCaller>,
    /// The number the next `OpenSession` goes under, for its answer to name.
    next_open: std::cell::Cell<slopty_proto::RequestId>,
    /// The clipboard kept in step with the workers; `None` where the platform has none here.
    clip: Option<Rc<std::cell::RefCell<crate::clipboard::ClipSync>>>,
    /// The app is frontmost.
    app_active: bool,
    /// Workers told this client wants their clipboard.
    watching: HashSet<WorkerKey>,
    /// The worker whose live terminal or window has the shell window's keyboard.
    clip_focus: Option<WorkerKey>,
    /// Looks at the clipboard every [`remote::CLIP_TICK`].
    clip_tick: Option<Task<()>>,
    /// Which workers the clipboard is shared with, as the settings say.
    clip_sharing: slopty_settings::ClipboardSettings,
    /// The workers it is not shared with, by key: read by the paste hooks as they run, so a
    /// change holds for the views made before it.
    clip_unshared: Rc<std::cell::RefCell<HashSet<WorkerKey>>>,
    /// Uploads in flight.
    uploads: HashMap<slopty_core::XferId, remote::Upload>,
    /// Downloads in flight, and the ledger that keeps transfers across a relaunch.
    transfers: remote::transfers::Transfers,
    /// Forwarded ports, by session.
    ports: HashMap<SessionId, Vec<slopty_client::tunnel::Forward>>,
    /// Keeps the workspace's clock ticking while a worker is out of reach.
    awake: Option<Task<()>>,
    /// What Settings → Devices offers to do to each machine, as the app says.
    device_actions: HashMap<WorkerKey, Vec<slopty_view_proto::DeviceAction>>,
    /// What the remote windows and desktops get beyond their pictures ([`desktop`]).
    desktop: desktop::Desktop,
    /// The shell's window, whose keyboard the live views hold.
    keyboard_window: Option<gpui::AnyWindowHandle>,
    /// Where a test's drags out go instead of the system's drag.
    #[cfg(target_os = "macos")]
    drag_sink: Option<remote::DragSink>,
}

impl std::fmt::Debug for WorkspaceView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceView")
            .field("workers", &self.workers.len())
            .finish_non_exhaustive()
    }
}

impl EventEmitter<WorkspaceEvent> for WorkspaceView {}

/// The fact a shell opened for a chat carries: the chat's id.
pub(crate) const CHAT_FACT: &str = "chat";

impl WorkspaceView {
    /// An empty workspace, its workers yet to come.
    #[must_use]
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            epoch: Instant::now(),
            #[cfg(test)]
            held_clock: None,
            pinned_rtt: None,
            workers: BTreeMap::new(),
            terminals: HashMap::new(),
            screens: HashMap::new(),
            titles: HashMap::new(),
            server_status: None,
            finished: HashMap::new(),
            slow_command: SLOW_COMMAND,
            approvals: approvals::Approvals::default(),
            turns: turns::Turns::default(),
            faces: faces::Faces::default(),
            view_model: view_model::Bridge::default(),
            server: None,
            next_open: std::cell::Cell::new(1),
            clip: None,
            app_active: true,
            watching: HashSet::new(),
            clip_focus: None,
            clip_tick: None,
            clip_sharing: slopty_settings::ClipboardSettings::default(),
            clip_unshared: Rc::default(),
            uploads: HashMap::new(),
            transfers: remote::transfers::Transfers::default(),
            ports: HashMap::new(),
            awake: None,
            device_actions: HashMap::new(),
            desktop: desktop::Desktop::default(),
            keyboard_window: None,
            #[cfg(target_os = "macos")]
            drag_sink: None,
        }
    }

    // ----- reading ---------------------------------------------------------------------------

    /// An item, whichever worker holds it.
    #[must_use]
    pub fn item(&self, tile: ItemRef) -> Option<&Item> {
        self.workers.get(&tile.worker)?.doc.get(tile.item)
    }

    /// Every item of every worker, with its worker, in worker then creation order.
    pub fn items(&self) -> impl Iterator<Item = (WorkerKey, &Item)> {
        self.workers.iter().flat_map(|(key, w)| w.doc.items().map(move |i| (*key, i)))
    }

    /// The tile showing `item`, whichever worker holds it.
    #[must_use]
    pub fn tile_of(&self, item: ItemId) -> Option<ItemRef> {
        self.workers
            .iter()
            .find(|(_, w)| w.doc.get(item).is_some())
            .map(|(key, _)| ItemRef { worker: *key, item })
    }

    /// The tile showing `session`.
    #[must_use]
    pub fn tile_of_session(&self, session: SessionId) -> Option<ItemRef> {
        self.workers.iter().find_map(|(key, w)| {
            w.doc.item_for_session(session).map(|i| ItemRef { worker: *key, item: i.id })
        })
    }

    /// The worker running `session`.
    #[must_use]
    pub fn worker_of_session(&self, session: SessionId) -> Option<WorkerKey> {
        self.workers
            .iter()
            .find(|(_, w)| {
                w.sessions.contains_key(&session) || w.doc.item_for_session(session).is_some()
            })
            .map(|(key, _)| *key)
    }

    /// Number of items across every worker.
    #[must_use]
    pub fn len(&self) -> usize {
        self.workers.values().map(|w| w.doc.len()).sum()
    }

    /// True when no worker has anything.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The terminal view for `session`, if it has one.
    #[must_use]
    pub fn terminal(&self, session: SessionId) -> Option<&Entity<TerminalView>> {
        self.terminals.get(&session)
    }

    /// The stream view of one item, if it has one open.
    #[must_use]
    pub fn screen(&self, item: ItemId) -> Option<&Entity<ScreenView>> {
        self.screens.get(&item)
    }

    /// This client's id on `worker`'s wire, while linked.
    #[must_use]
    pub fn me(&self, worker: WorkerKey) -> Option<ClientId> {
        self.workers.get(&worker)?.link.as_ref().map(|l| l.me)
    }

    /// The workers, their names and states, in key order.
    pub fn workers(&self) -> impl Iterator<Item = (WorkerKey, &str, &WorkerStatus)> {
        self.workers.iter().map(|(key, w)| (*key, w.name.as_str(), &w.status))
    }

    /// Link RTT of one worker, sampled once a second while connected.
    #[must_use]
    pub fn rtt(&self, worker: WorkerKey) -> Option<Duration> {
        self.workers.get(&worker)?.rtt
    }

    /// How many links have come up to `worker` since the app started.
    #[must_use]
    pub fn links(&self, worker: WorkerKey) -> u64 {
        self.workers.get(&worker).map_or(0, |w| w.links)
    }

    /// The badge a session's last long command left, if the tile has not been looked at since.
    #[must_use]
    pub fn finished(&self, session: SessionId) -> Option<&Finished> {
        self.finished.get(&attention::About::Session(session))
    }

    /// The worker a new session or lookup goes to: the first one that is up, else the first
    /// known.
    fn context_worker(&self) -> Option<WorkerKey> {
        self.workers
            .iter()
            .find(|(_, w)| w.link.is_some())
            .map(|(k, _)| *k)
            .or_else(|| self.workers.keys().next().copied())
    }

    fn send(&self, worker: WorkerKey, msg: ClientMsg) {
        if let Some(w) = self.workers.get(&worker) {
            w.send(msg);
        }
    }

    /// Send to the worker running `session`.
    fn send_session(&self, session: SessionId, msg: ClientMsg) {
        if let Some(worker) = self.worker_of_session(session) {
            self.send(worker, msg);
        }
    }

    /// Apply an item op here at once and propose it to its worker, now or, while the worker
    /// is out of reach, once it is back.
    fn propose(&mut self, worker: WorkerKey, op: ItemOp, cx: &mut Context<Self>) {
        let Some(w) = self.workers.get_mut(&worker) else { return };
        let change = w.doc.apply_op(&op, true);
        w.send_or_queue(ClientMsg::Items(op));
        self.item_changed(worker, change, cx);
    }

    /// Close `session` on `worker` for good, now or once the worker is back: a shell closed
    /// while its worker was away must not keep running there unseen.
    fn close_session_on(&mut self, worker: WorkerKey, session: SessionId) {
        if let Some(w) = self.workers.get_mut(&worker) {
            w.send_or_queue(ClientMsg::Term {
                session,
                req: slopty_proto::terminal::TermRequest::Close,
            });
        }
    }

    /// The time on the workspace's clock.
    fn now(&self) -> Duration {
        #[cfg(test)]
        if let Some(held) = self.held_clock {
            return held;
        }
        self.epoch.elapsed()
    }

    /// Swap the theme everywhere: every live terminal (which re-fits its grid to the new font
    /// on its next frame) and window.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme == theme {
            return;
        }
        for view in self.terminals.values() {
            view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        }
        for view in self.screens.values() {
            view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        }
        if self.theme.terminal != theme.terminal {
            // Every shell hears the new colours; the worker applies the driver's.
            let colors = theme.terminal.wire();
            for session in self.terminals.keys() {
                self.send_session(
                    *session,
                    ClientMsg::Term {
                        session: *session,
                        req: slopty_proto::terminal::TermRequest::Colors(colors),
                    },
                );
            }
        }
        self.theme = theme;
        cx.notify();
    }

    /// Something to tell the person (a paste refused, an upload that landed elsewhere), handed
    /// to the app as [`WorkspaceEvent::Notice`].
    pub fn show_notice(text: String, cx: &mut Context<Self>) {
        tracing::info!(%text, "notice");
        cx.emit(WorkspaceEvent::Notice { text, failure: false });
    }

    /// Something that went wrong, said as [`Self::show_notice`] says a notice.
    pub fn show_failure(text: String, cx: &mut Context<Self>) {
        tracing::warn!(%text, "failure");
        cx.emit(WorkspaceEvent::Notice { text, failure: true });
    }

    /// `key`'s name, as its link or the server says it.
    pub(crate) fn worker_name(&self, key: WorkerKey) -> String {
        self.workers.get(&key).map(|w| w.name.clone()).unwrap_or_default()
    }

    /// The theme in use (terminal size included).
    #[must_use]
    pub const fn theme(&self) -> &Theme {
        &self.theme
    }
}
