//! `WorkspaceView`: every worker's items, tiled.
//!
//! One view for every worker this client reaches: each worker's item registry is mirrored
//! ([`slopty_client::ItemDoc`]) and each item is a tile in this device's tiling
//! ([`slopty_client::layout::Tiling`]): projects, each with its tabs, each tab its panes tiled
//! edge to edge. The registry is the worker's; the arrangement is this device's alone.
//!
//! * [`actions`] — the actions, the key table and the palette lines.
//! * `workers` — connecting, losing and forgetting a worker; the sync that follows.
//! * `commands` — what the actions do.
//! * `agents` — coding agents in terminals: badges, banners, "needs you".
//! * `browsers` — web pages in tiles: opening them, and serving their ports here.
//! * `folders` — folders in tiles, and a path opened as whatever it turns out to be.
//! * `overlays` — the command palette, find in every tile, the window picker.
//! * `toast` — the one-line notices: undo close, what needs the person off screen.
//! * [`remote`] — the clipboard shared with the workers, files dropped on tiles, forwarded ports.
//! * `area` — the tab on show: its panes and their tiles, a header's drag, the start page.
//! * `panes` — a tab's panes at their rectangles and the sashes between them.
//! * `title_tabs` — the title bar's tabs of the project on show.
//! * `tile` — one tile's chrome and body.
//! * `titlebar` — the bar across the top: where the focused work is, the notices, the bell.
//! * `navigator` — the workers and their tiles, down the left, and the filter over them.
//! * `machines` — what each machine says of itself and what can be done to it, from its row.
//! * `rollup` — what a folded worker or a workspace tab adds up to; the navigator's second line.
//! * `readouts` — what the app says of itself at the title bar's end, while it has something.
//! * `approvals` — "Allow" and "Deny" for an agent that waits, from its row or its note.
//! * `turns` — agents' turns that ended unread, which the bell counts.
//! * `projects` — the server's projects, each board shown in its orchestrator's tile.
//!
//! The navigator and the title bar are views of their own (`ChromeView`),
//! drawn cached: a terminal's echo draws the terminal and the panes around it, never the
//! chrome, which draws again only when the workspace itself changes or its own clock ticks.

mod about;
pub mod actions;
mod agent_screens;
mod agent_start;
mod agents;
mod approval_cards;
mod approvals;
mod area;
pub mod attention;
mod authors;
mod breadcrumb;
mod browsers;
mod commands;
mod context_menus;
mod desktop;
mod faces;
mod facts;
mod folder_typing;
mod folders;
mod foot;
mod grouping;
mod handoffs;
mod kept_items;
mod machine_remove;
mod machines;
mod navigator;
mod overlays;
mod panes;
mod popout;
mod presence;
mod preview;
mod project_lines;
mod project_search;
mod projects;
mod pull_review;
mod readouts;
pub mod remote;
mod restore;
mod reviews;
mod rollup;
mod seating;
mod secure;
mod settings_page;
mod starting;
mod tab_look;
mod tabs;
mod tile;
mod tile_strip;
mod title_tabs;
mod titlebar;
mod toast;
mod turns;
mod unsaved;
mod workers;
mod worktrees;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

pub use actions::*;
pub use agents::{banner_title, program_banner};
#[cfg(test)]
pub(crate) use area::{ADD_WORKER, NEW_AGENT, NO_WORKERS, NO_WORKERS_NEXT};
use gpui::{
    App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, Pixels, SharedString,
    StyleRefinement, Subscription, Task, WeakEntity, Window,
};
pub use machines::HostActions;
pub use navigator::NAVIGATOR_CTX;
pub use projects::worker_key;
pub(crate) use rollup::META_SEPARATOR;
use slopty_client::ItemDoc;
use slopty_client::layout::{Navigator, Saved, TileRef, Tiling, TilingConfig, WorkerKey};
use slopty_client::relay::RelayWatch;
use slopty_core::{ClientId, ItemId, SessionId};
use slopty_proto::ClientMsg;
use slopty_proto::items::{Item, ItemOp};
use slopty_proto::screen::CaptureTarget;
use slopty_proto::server::WorkerCaps;
use slopty_proto::terminal::SessionSummary;
use slopty_theme::Theme;
#[cfg(test)]
pub(crate) use tile::{
    ATTACHING, CLOSE_TILE, MUTE, OPENING, PAUSED, READING, RECONNECTING, SESSION_ENDED, TAKE,
    TAKE_OVER,
};
pub use tile::{COPY_COMMAND, cwd_tail, file_title};
pub(crate) use worktrees::{REMOVE_WORKTREE, worktree_root};

/// Chrome words the modules keep to themselves, for the sentence-case check: a waiting
/// badge's description and what "+" is called.
#[cfg(test)]
pub(crate) const CHROME_WORDS: [&str; 2] = [agents::SHOW_PROMPT, titlebar::NEW];
pub use titlebar::titlebar_height;
use tokio::sync::mpsc;

use crate::file::FileView;
use crate::folder::FolderView;
use crate::palette::{CommandPalette, PaletteItem};
use crate::picker::WindowPicker;
use crate::screen::{ScreenFactory, ScreenView};
use crate::terminal::TerminalView;

/// How long a shell command or an agent's turn has to run before its end, unwatched, is worth a
/// mark: shorter ones end before the person has looked away.
pub const SLOW_COMMAND: Duration = Duration::from_secs(30);

/// How long a shell command runs before its tile marks it running: a quick one ends before the
/// mark would be read, and a mark that flickers on for it is noise.
pub const RUNNING_AFTER: Duration = Duration::from_secs(3);

/// How long a closed tile's notice offers it back, and how long a closed shell that was
/// running something (a command, a program, an agent) keeps its session for ⌘Z.
const UNDO_CLOSE: Duration = Duration::from_secs(5);

/// How long a closed shell idle at its prompt keeps its session, to come back whole: it runs
/// nothing meanwhile.
const IDLE_SHELL_KEPT: Duration = Duration::from_mins(10);

/// How many closed tiles ⌘Z and the palette's "Reopen" can bring back, the latest first.
const CLOSED_KEPT: usize = 20;

/// How long a remote window or display may sit off screen before its stream is let go: long
/// enough for a glance at the next column and back, short enough that a stream nobody sees
/// does not keep a worker encoding for long.
const STREAM_GRACE: Duration = Duration::from_secs(5);

/// How long the layout waits after its last change before it is written to disk.
const SAVE_AFTER: Duration = Duration::from_millis(500);

/// The longest refresh of a screen the app draws on (60 Hz): a typed key's echo is awaited for
/// its round trip and this much more before the working marks step on without it.
const ECHO_SLACK: Duration = Duration::from_nanos(16_666_667);

/// A region of the frame drawn as a view of its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Region {
    Navigator,
    /// The navigator's rows, inside its panel: the list's wheel builds this view alone,
    /// not the panel with its filter field.
    NavigatorRows,
    /// The navigator folded to a glyph per project, where it is hidden or lays over the panes:
    /// a view of its own, so it stays while the panel opens over it.
    Rail,
    Titlebar,
    /// The title bar's tabs, inside it: a working mark's step builds this view alone, not
    /// the bar with its breadcrumb and its readouts.
    TitleTabs,
    /// The bar along the window's foot ([`foot`]).
    Foot,
    /// The facts and controls of a tab's one tile, in the title bar ([`tile_strip`]): news for
    /// its header builds this view and the panes, not the bar.
    TileStrip,
}

/// One region of the workspace's chrome as a view of its own, so the frame can draw it
/// cached. It holds nothing: it draws the workspace's region, which it reads and never writes
/// ([`crate::draw`]), and is drawn again only when notified, by the workspace changing
/// ([`Chrome::notify`]) or by a clock of its own (an age, a turn's time, the frame time).
struct ChromeView {
    workspace: WeakEntity<WorkspaceView>,
    region: Region,
    /// How many times it has drawn: the proof that an echo leaves it be.
    #[cfg(test)]
    renders: usize,
}

impl gpui::Render for ChromeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        #[cfg(test)]
        {
            self.renders = self.renders.saturating_add(1);
        }
        let region = self.region;
        if let Some(workspace) = self.workspace.upgrade() {
            let projects = Rc::clone(&workspace.read(cx).frame_projects);
            projects.hold(false, cx);
        }
        crate::draw::build(&self.workspace, window, cx, |workspace, window, cx| {
            workspace.render_region(region, window, cx)
        })
    }
}

/// The tab on show as a view of its own, built from the workspace, which it reads and never
/// writes ([`crate::draw`]). What only changes the panes (a working mark's turn, the pointer
/// over a tile, a body's own news) builds this view alone: the workspace's own notify is news
/// of a change for the chrome and the titles. What the area drew is kept for it in
/// [`area::Drawn`].
struct AreaHost {
    workspace: WeakEntity<WorkspaceView>,
    /// How many times it has built: the proof that a pane's own news builds it alone.
    #[cfg(test)]
    builds: usize,
}

impl gpui::Render for AreaHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        #[cfg(test)]
        {
            self.builds = self.builds.saturating_add(1);
        }
        let host = cx.entity_id();
        if let Some(workspace) = self.workspace.upgrade() {
            let projects = Rc::clone(&workspace.read(cx).frame_projects);
            projects.hold(false, cx);
        }
        crate::draw::build(&self.workspace, window, cx, |workspace, window, cx| {
            workspace.render_area(host, window, cx)
        })
    }
}

/// The chrome's views.
struct Chrome {
    navigator: Entity<ChromeView>,
    nav_rows: Entity<ChromeView>,
    /// The navigator folded to its glyphs ([`Region::Rail`]).
    rail: Entity<ChromeView>,
    titlebar: Entity<ChromeView>,
    title_tabs: Entity<ChromeView>,
    foot: Entity<ChromeView>,
    tile_strip: Entity<ChromeView>,
}

impl Chrome {
    fn new(cx: &mut Context<WorkspaceView>) -> Self {
        use gpui::AppContext as _;
        let workspace = cx.weak_entity();
        let mut view = |region| {
            let workspace = workspace.clone();
            cx.new(|_| ChromeView {
                workspace,
                region,
                #[cfg(test)]
                renders: 0,
            })
        };
        Self {
            navigator: view(Region::Navigator),
            nav_rows: view(Region::NavigatorRows),
            rail: view(Region::Rail),
            titlebar: view(Region::Titlebar),
            title_tabs: view(Region::TitleTabs),
            foot: view(Region::Foot),
            tile_strip: view(Region::TileStrip),
        }
    }

    /// Draw every region again: the workspace changed, and any of them may show it.
    fn notify(&self, cx: &mut App) {
        for view in self.ids() {
            App::notify(cx, view);
        }
    }

    /// The views.
    fn ids(&self) -> [gpui::EntityId; 7] {
        [
            &self.navigator,
            &self.nav_rows,
            &self.rail,
            &self.titlebar,
            &self.title_tabs,
            &self.foot,
            &self.tile_strip,
        ]
        .map(Entity::entity_id)
    }
}

/// Things the surrounding app reacts to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WorkspaceEvent {
    /// A terminal rang its bell.
    Bell(SessionId),
    /// A coding agent's thread came to need the human (an approval, a question), as its
    /// worker's table says, or the server's ladder for a worker not linked here. Led by a
    /// server, its notices say this instead ([`attention::Attention::server_led`]).
    Attention(slopty_proto::thread::ThreadId),
    /// A program in this session asked for a desktop notification (`OSC 9`, `OSC 777`): this
    /// client's own moment, never the server's.
    Program(SessionId),
    /// How many agents are waiting on the human right now, across every worker (the Dock
    /// badge).
    NeedsYou(usize),
    /// A note's "Allow" or "Deny" found no prompt to answer while the app was away: the app
    /// says `why` in a note of its own about `route`'s session.
    Unanswered {
        /// The note's agent.
        route: attention::Route,
        /// What to say.
        why: &'static str,
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
}

/// Where the phone key bar sends its keys (see [`WorkspaceView::active_key_target`]).
#[derive(Clone, Debug)]
pub enum KeyTarget {
    /// A terminal: keys go through the grid and the predictor.
    Terminal(Entity<TerminalView>),
    /// A remote window: keys are injected on the worker.
    Screen(Entity<ScreenView>),
}

/// Where a worker's link stands.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum WorkerStatus {
    /// First attempt in progress.
    Connecting,
    /// Link up.
    Connected,
    /// Link up but nothing heard for this many seconds (keep-alives come every second).
    Silent(u64),
    /// Link up, but the device was away or the path moved under it: a probe is out, and the
    /// tiles are set back until it answers.
    Checking,
    /// The probe went unanswered and a new link is being dialled at once: the tiles keep what
    /// they showed, set back, until it lands.
    Relinking,
    /// Link lost or the attempt failed; retrying, with the reason.
    Reconnecting(String),
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
                format!("silent {}", crate::kit::duration(Duration::from_secs(*secs)))
            }
            Self::Checking => "checking…".to_owned(),
            Self::Reconnecting(why) => format!("{why}; reconnecting…"),
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

    /// Whether what its tiles show may be stale: its link is being checked or dialled again
    /// after a resume. They show it set back, not hidden.
    #[must_use]
    pub const fn in_doubt(&self) -> bool {
        matches!(self, Self::Checking | Self::Relinking)
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
/// and goes while the registry mirror (and so the tiles) stays.
struct Worker {
    name: String,
    status: WorkerStatus,
    link: Option<WorkerLink>,
    /// Since when it has been out of reach on this client's clock: added and not yet linked,
    /// or since its link dropped; `None` while linked. Its tiles say for how long.
    away_since: Option<Duration>,
    /// How many links have come up to it: the self-test's proof that a relink landed.
    links: u64,
    doc: ItemDoc,
    sessions: HashMap<SessionId, SessionSummary>,
    rtt: Option<Duration>,
    /// How the tailnet carries the link, once the worker has said (never on a loopback or LAN
    /// link), and since when it has been on a DERP relay.
    relay: RelayWatch,
    /// Draws again once a DERP path has held long enough to be said ([`RelayWatch::due`]).
    relay_due: Option<Task<()>>,
    /// Its home directory as its hello said, so a path under it reads `~/…`; `None` until a
    /// link has said, or when the daemon has none.
    home: Option<String>,
    /// Where its `settings.toml` is, as its hello said; `None` until a link has said, or when
    /// the daemon has none.
    settings: Option<String>,
    /// What it can do, as its link or the server's directory last said.
    caps: Option<WorkerCaps>,
    /// Its one-minute load average, as its link or the server last said.
    load: Option<f32>,
    /// The paths the worker was last asked to watch for its file tiles, sorted.
    watched: Vec<String>,
    /// The directories the worker was last asked to watch for its folder tiles, sorted.
    watched_folders: Vec<String>,
    /// A `List` is in flight to name restored window items.
    titles_requested: bool,
    /// A `List` is in flight for the picker.
    picker_wanted: bool,
    /// The next listing adds its first display straight away (the self-test socket's way to
    /// put a remote display in the workspace without the picker).
    display_wanted: bool,
    /// Streams requested but not yet `Opened`, by target.
    pending_opens: HashMap<CaptureTarget, ItemId>,
    /// The shells asked of it on this link and not come yet, and where each goes
    /// ([`tabs::Opening`]).
    openings: tabs::Openings,
    /// Tiles this client opened by a drop on a pane, not come yet: where each lands.
    dropped: HashMap<ItemId, slopty_client::layout::Drop>,
    /// Remote tiles whose open the worker refused on this link, and why: not asked again
    /// until the next link or a retry.
    failed_opens: HashMap<ItemId, slopty_proto::screen::ScreenFailure>,
    /// Remote tiles whose view came over a link that has gone: it shows its last picture, set
    /// back, until a stream opened on the next link has one of its own.
    stale_screens: HashSet<ItemId>,
    /// Streams opened on this link for a stale tile, waiting for their first picture to take
    /// its place, so the tile never shows an empty frame between the two.
    fresh_screens: HashMap<ItemId, Entity<ScreenView>>,
    /// The display tile streamed from a display this worker made for this device.
    sized: Option<desktop::Sized>,
    /// The first snapshot since the link came up has not been applied yet.
    awaiting_snapshot: bool,
    /// What the human did to this worker's items while it was out of reach (or before its
    /// first snapshot): replayed over the next snapshot and sent, in order.
    queued: Vec<ClientMsg>,
    /// The edits its programs wait on; kept across links, since the worker asks again under
    /// the same number after a reconnect.
    handoffs: slopty_client::handoff::Handoffs,
    /// The changes to its files this client asked for, until it answers each.
    fs_ops: slopty_client::folders::FsOps,
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
            away_since: None,
            links: 0,
            doc: ItemDoc::default(),
            sessions: HashMap::new(),
            rtt: None,
            relay: RelayWatch::default(),
            relay_due: None,
            home: None,
            settings: None,
            caps: None,
            load: None,
            watched: Vec::new(),
            watched_folders: Vec::new(),
            titles_requested: false,
            picker_wanted: false,
            display_wanted: false,
            pending_opens: HashMap::new(),
            openings: tabs::Openings::new(),
            dropped: HashMap::new(),
            failed_opens: HashMap::new(),
            stale_screens: HashSet::new(),
            fresh_screens: HashMap::new(),
            sized: None,
            awaiting_snapshot: false,
            queued: Vec::new(),
            handoffs: slopty_client::handoff::Handoffs::default(),
            fs_ops: slopty_client::folders::FsOps::default(),
        }
    }

    /// Whether the worker can hear this client right now.
    const fn is_linked(&self) -> bool {
        self.link.is_some()
    }

    /// Send `msg` if the worker is linked; otherwise it is lost. For what only means anything
    /// now (a read, a key, a stream request): see [`Self::send_or_queue`] for what must land.
    fn send(&self, msg: ClientMsg) -> bool {
        let Some(link) = &self.link else {
            tracing::debug!(kind = msg.kind(), "worker down; not sent");
            return false;
        };
        if let Err(e) = link.out.try_send(msg) {
            tracing::warn!(error = %e, "outbound queue");
        }
        true
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

/// A shell command that finished while nobody was looking: what the header badge says.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Finished {
    /// What was typed.
    pub command: String,
    /// Its exit status, when the shell said.
    pub exit: Option<u8>,
    /// How long it ran.
    pub elapsed: Duration,
}

impl Finished {
    /// The badge text: "Done · 3.2 s", "Exit 1 · 1m 4s", in [`crate::kit::duration`]'s words.
    #[must_use]
    pub fn label(&self) -> String {
        let took = crate::kit::duration(self.elapsed);
        match self.exit {
            Some(0) | None => format!("Done · {took}"),
            Some(code) => format!("Exit {code} · {took}"),
        }
    }
}

/// What a menu row does when clicked.
pub type MenuRun = Rc<dyn Fn(&mut Window, &mut App)>;

/// The sections of the titlebar's menus, in their order, a hairline between each.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum MenuGroup {
    /// A phone's "…": the focused tile's own rows, which its header holds on a wider screen.
    Tile,
    /// The breadcrumb's: the projects, or a repository's checkouts.
    Places,
    /// A phone's title: the panes of the tab on show.
    Panes,
    /// A phone's title: the project's tabs.
    Tabs,
    /// "+": the worker a new tile goes to, where there are several.
    Target,
    /// "+": what a new tile can be.
    Tiles,
    /// Where to go and what to see: the palette, the stream stats.
    Navigation,
    /// The settings.
    Settings,
    /// The server and the workers: connecting, adding, updating, waking.
    Connections,
    /// Taking something away for good: forgetting a machine.
    Removal,
}

/// An entry of the titlebar's "…" menu the app adds (settings, the server, adding a worker).
#[derive(Clone)]
pub struct MenuEntry {
    /// The section it is listed in.
    pub group: MenuGroup,
    /// What the row says.
    pub label: SharedString,
    /// Muted text on the right ("connected", "⌘,").
    pub detail: SharedString,
    /// What a click does.
    pub run: MenuRun,
}

impl std::fmt::Debug for MenuEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MenuEntry").field("label", &self.label).finish_non_exhaustive()
    }
}

/// What the field open in a tile's header edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    /// The tile's name.
    Name,
    /// A page's address.
    Address,
    /// The name of the tile's project ("Name this project…").
    Project,
}

/// The field open in a tile's header: its name, or a page's address.
struct Rename {
    tile: TileRef,
    field: Field,
    input: Entity<gpui_kit::component::input::InputState>,
    /// Who had the keyboard before the field: it goes back there after ↩ or Esc.
    return_to: Option<FocusHandle>,
    _subscription: Subscription,
}

/// A tile taken off, one of the last [`CLOSED_KEPT`], until ⌘Z or the palette puts it back
/// where it was. A live shell's session runs on for a while, so its rows come back untouched.
struct ClosedTile {
    tile: TileRef,
    item: Item,
    /// Where it was in the layout, to put it back there.
    at: Option<slopty_client::layout::Pos>,
    /// The session kept alive for the wait, for a live shell.
    session: Option<SessionId>,
    /// A file tile's editor, kept while the notice is up: its edit is not on the worker's
    /// disk, so a tile taken back then comes back with the buffer it closed with.
    file: Option<Entity<FileView>>,
    seq: u64,
    /// What its header said, for the palette's "Reopen" line.
    title: String,
    /// Its notice is up.
    offered: bool,
    /// For a plain shell: what a new shell takes once its session has ended.
    shell: Option<Reshell>,
    /// For an agent's terminal: its thread, taken up again once its session has ended.
    thread: Option<slopty_proto::thread::ThreadId>,
}

impl ClosedTile {
    /// Whether it still holds a live thing: a running session or an editor.
    #[cfg(test)]
    const fn holds(&self) -> bool {
        self.session.is_some() || self.file.is_some()
    }
}

/// A closed plain shell, as a new shell stands in for it once its session has ended.
struct Reshell {
    /// Its directory when it closed.
    cwd: Option<String>,
    /// The name the person gave it.
    name: Option<String>,
}

/// The workspace.
pub struct WorkspaceView {
    /// The theme in use: the settings' with ⌘=/⌘- applied to the terminal text.
    theme: Theme,
    /// The settings' theme, as the app last set it.
    base_theme: Theme,
    /// Points ⌘=/⌘- moved the terminal text by.
    font_delta: f32,
    /// The projects, their tabs and the panes they tile.
    layout: Tiling,
    /// The navigator's width, its grouping and whether it shows, kept with the tiling.
    navigator: Navigator,
    /// The sash being dragged.
    panes: panes::Panes,
    /// The remote windows' pane sizes when the sash was pressed, to ask them to follow once it
    /// is let go.
    sash_before: Vec<(ItemId, (f32, f32))>,
    /// Where the title bar's tabs are scrolled.
    title_scroll: gpui::ScrollHandle,
    /// The title tabs that closed and still fold away.
    title_closing: title_tabs::Closing,
    /// The file tile open as a preview, which the next file opened in passing replaces.
    preview: preview::Preview,
    /// The foot bar's own state.
    foot: foot::Foot,
    /// The title tabs as last drawn: a header's news draws them again only where it changed
    /// what they say.
    title_tabs_drawn: std::cell::RefCell<Vec<title_tabs::TitleTab>>,
    /// The layout's clock starts here.
    epoch: Instant,
    /// A tick runs while a worker is out of reach, so its tiles' "Reconnecting for …" keeps
    /// time; it stops once every worker is linked.
    away_ticking: bool,
    /// The layout's clock held at a time, for tests that compare two frames drawn at one instant.
    #[cfg(test)]
    held_clock: Option<Duration>,
    /// The round trip the readouts (the navigator, the bar, the palette) show for every linked
    /// worker in place of its link's, pinned by the e2e harness so a golden never carries the
    /// machine's live timing. The predictors and the dump keep the link's own.
    pinned_rtt: Option<Duration>,
    /// Whether moves animate. Off under the self-test, where a frame is a step.
    animate: bool,
    /// GPUI's Reduce Motion flag as last read: on, nothing springs whatever [`Self::animate`]
    /// says. Read at every drawing, so the flag alone decides.
    reduced: bool,
    workers: BTreeMap<WorkerKey, Worker>,
    // Per-item views. Item and session ids are UUIDv7, unique across workers, so one map
    // each serves every worker.
    terminals: HashMap<SessionId, Entity<TerminalView>>,
    screens: HashMap<ItemId, Entity<ScreenView>>,
    files: HashMap<ItemId, Entity<FileView>>,
    folders: HashMap<ItemId, Entity<FolderView>>,
    /// Paths asked of a worker to learn whether they are folders before a tile opens for them
    /// ([`WorkspaceView::open_path_on`]), with the line a file lands on.
    probes: Vec<(WorkerKey, String, Option<u32>)>,
    browsers: HashMap<ItemId, Entity<crate::browser::BrowserView>>,
    /// The link each browser tile's port is served by: a new link serves it anew.
    browser_links: HashMap<ItemId, std::sync::Weak<dyn slopty_client::remote::Remote>>,
    /// The line a file tile opened at, for a view not made yet.
    file_focus: HashMap<ItemId, u32>,
    /// A registry or a link changed since the file tiles and pages were last matched
    /// to the items: the next frame matches them. Only then, since a frame comes with every
    /// terminal and video update, and the matching walks every item.
    items_dirty: bool,
    /// Who needs the human, worked out when the workspace changes, for everything drawn until
    /// the next change.
    drawn_waiting: Vec<agents::Waiting>,
    /// The threads that need the human whose rows speak for them, worked out with
    /// `drawn_waiting`.
    drawn_thread_waits: Vec<faces::ThreadWait>,
    /// Where ⌘⇧A's last step stood on the attention ladder.
    attention_at: Option<usize>,
    /// The starts made here: the last one, what "New agent…" lists first, and each agent's
    /// chips, where its next draft begins.
    starts: slopty_client::starts::Starts,
    /// Where they are kept, once the app says.
    starts_file: Option<std::path::PathBuf>,
    /// Their write under way: the next waits on it, so the latest lands last.
    starts_writing: Option<Task<()>>,
    /// The session step that is up: its agent and machine, and what the machine said for it.
    sessions_asked: Option<agent_start::SessionsAsked>,
    /// The folders each machine's agents' past sessions ran in, as it last listed them.
    past_places: HashMap<WorkerKey, Vec<agent_start::PastPlace>>,
    /// The folder step that is up.
    folder_step: Option<agent_start::FolderStep>,
    /// The asks for past sessions with no words still out, by machine and agent (every agent's
    /// when `None`): one at a time each ([`Self::ask_past_places`]).
    listing: HashSet<(WorkerKey, Option<slopty_proto::thread::AgentId>)>,
    /// The tiles of threads on their way.
    starting: starting::Starts,
    /// Each worker's items kept on this device.
    kept_items: kept_items::KeptItems,
    /// The tiles, the faces or the links changed since the faces were last brought in step
    /// with them: the next frame does it ([`Self::sync_faces`] makes a face with the window).
    faces_dirty: bool,
    /// The number each unnamed tile that reads like an earlier one of its worker carries after
    /// its title ("Terminal 2"), worked out in the frame after [`Self::titles_dirty`].
    twins: HashMap<ItemId, u32>,
    /// Every item's derived title, as the twins were last worked out from.
    derived: HashMap<ItemId, String>,
    /// The placed files and folders whose title another placed one of their kind shares, on
    /// any machine, worked out with the twins: only their place tells them apart in the
    /// navigator ([`Self::nav_tile_meta`]).
    alike: HashSet<ItemId>,
    /// Every item's place ([`Self::tile_place`]), worked out with the titles.
    places: HashMap<ItemId, Option<String>>,
    /// A title may have changed since the twins were worked out: the workspace changed (its
    /// own notify), a shell's command started or ended, a window was named. A frame another
    /// tile causes (an echo, a video frame) changes no title, and works nothing out.
    titles_dirty: bool,
    /// What the area drew, for the handlers to read ([`area::Drawn`]).
    drawn: Rc<area::Drawn>,
    /// The navigator and the title bar, each a view of its own.
    chrome: Chrome,
    /// The area, a view of its own so a sash drag is not the chrome's news.
    area_host: Entity<AreaHost>,
    /// How many changes the workspace took and how many times the titles were worked out: the
    /// proof that a frame of motion is neither.
    #[cfg(test)]
    counts: (usize, usize),
    /// How many times the workspace was built: the proof that the keyboard moving is not.
    #[cfg(test)]
    renders: usize,
    /// What the panes and the chrome show of the shells, streams and faces ([`facts`]).
    facts: facts::Facts,
    /// How often and how lately each project was gone to on this device, for the palette's
    /// ranking; saved with the layout.
    frecency: slopty_client::groups::Frecency,
    /// The project the focus was last in, so moving among one project's tiles is one visit.
    last_project: Option<slopty_client::layout::GroupKey>,
    /// The projects as the frame being drawn groups them, worked out once a draw.
    frame_projects: Rc<grouping::FrameProjects>,
    /// Holds the working marks' steps while a typed key waits for its echo.
    _keys: Subscription,
    /// Window titles the picker or a listing gave (the registry stores ids).
    titles: HashMap<ItemId, String>,
    /// The quiet line about the server in the titlebar ("server offline"), if any.
    server_status: Option<SharedString>,
    /// Long shell commands that finished unwatched, by session.
    /// What ended while nobody looked, by what it is about: a terminal's command or agent turn,
    /// or the turn of a thread with no terminal ([`attention::About`]).
    finished: HashMap<attention::About, Finished>,
    slow_command: Duration,
    /// How long a command runs before its tile says so ([`RUNNING_AFTER`]).
    running_after: Duration,
    /// Tiles by item, least recent first: one moves to the end when it arrives and when it is
    /// focused. The "run in shell" target is the last shell here; the palette lists tiles from
    /// the end.
    recency: Vec<ItemId>,
    /// Holds the device out of idle sleep while an agent works.
    awake: Option<Task<()>>,
    /// Remote tiles off screen since when; their streams go after [`STREAM_GRACE`].
    unseen: HashMap<ItemId, Instant>,
    /// How long a remote tile may be off screen before its stream goes ([`STREAM_GRACE`]).
    stream_grace: Duration,
    /// Remote tiles whose streams were let go for being off screen.
    parked: HashSet<ItemId>,
    picker: Option<(WorkerKey, Entity<WindowPicker>)>,
    /// The picker just dismissed, drawn for the moment it takes to fade away.
    picker_leaving: Option<Entity<WindowPicker>>,
    palette: Option<Entity<CommandPalette>>,
    /// A dismissed palette still drawing its way out, dropped once that has played.
    palette_leaving: Option<Entity<CommandPalette>>,
    /// Search in files: the surface, shown or kept, and where the keyboard goes back to.
    search: project_search::Surface,
    pending_find: Option<(SessionId, crate::kit::find::Query)>,
    pending_find_file: Option<(ItemId, crate::kit::find::Query)>,
    rename: Option<Rename>,
    rename_return: Option<FocusHandle>,
    pending_focus_rename: bool,
    palette_return: Option<FocusHandle>,
    palette_action: Option<Box<dyn gpui::Action>>,
    /// The actions the open palette leaves out: nothing where the keyboard was answers them.
    palette_hidden: HashSet<std::any::TypeId>,
    /// What the app does for a line or a button of ours (wake a worker, dial it now), run on
    /// the next frame, where there is a window to run it in.
    pending_runs: Vec<MenuRun>,
    /// The tailnet policy grant that lets this device in, as the app words it: the away pill
    /// copies it for a worker whose policy turns the device away.
    tailnet_grant: Option<SharedString>,
    palette_extra: Vec<PaletteItem>,
    /// A keyboard is there to press chords on: always on a Mac, only when one is attached on
    /// a phone or tablet.
    hardware_keyboard: bool,
    pending_focus_palette: bool,
    /// Which titlebar menu is open.
    menu: Option<titlebar::MenuKind>,
    /// The bar's menu was opened by a key, so it arrives whole in its first frame.
    menu_keyed: bool,
    /// The bar's menu just closed, drawn for the moment it takes to fade away.
    menu_leaving: Option<titlebar::MenuKind>,
    /// Where a press opened the menu, which then hangs there rather than from a button.
    menu_at: Option<gpui::Point<Pixels>>,
    /// What a press on a tile or a project opened, while it shows.
    context_menu: Option<context_menus::ContextMenu>,
    /// The worker "+" chose for the next new tile, where there are several; the focused tile's
    /// worker otherwise.
    new_on: Option<WorkerKey>,
    /// Where the bar's buttons that hang a menu were last laid out.
    anchors: titlebar::Anchors,
    /// The navigator's state for this run (its width and whether it docks are the layout's).
    nav: navigator::NavState,
    /// The title bar's readouts' own state: the frame time's clock, the popovers, a release.
    readouts: readouts::Readouts,
    /// What the app lets the person do to each machine, and how one is added.
    machines: machines::Machines,
    /// The confirm of a machine's removal, while it is open.
    machine_remove: Option<machine_remove::Asking>,
    /// The permission prompts this client may answer from a row or a note.
    approvals: approvals::Approvals,
    /// The settings, as a page in the panes' place while they are up ([`settings_page`]).
    settings: Option<Entity<crate::settings_editor::SettingsEditor>>,
    /// Agents' turns under way, and the ones that ended unread.
    turns: turns::Turns,
    /// The app's rows in the "…" menu.
    more_entries: Vec<MenuEntry>,
    /// The app's rows in the server readout's menu.
    server_entries: Vec<MenuEntry>,
    show_stats: bool,
    /// The remote desktops' own state: this device's display key, the wait of a display
    /// following its tile, the system shortcuts.
    desktop: desktop::Desktop,
    toast: Option<toast::Toast>,
    closed: Vec<ClosedTile>,
    closed_seq: u64,
    /// A header, a tab or a row pressed, and where it would land once it moves.
    drag: Option<area::Drag>,
    /// Where the title strip and the navigator take a drop, as they were last drawn.
    drop_spots: Rc<area::DropSpots>,
    /// How much narrower than the window the workspace was laid out in the last frame: none,
    /// unless the app gives it less (an iPad's Split View, as the self-test sets it).
    width_inset: f32,
    /// A timer is out to park the streams of remote tiles off screen.
    park_pending: bool,
    /// A terminal to focus on the next frame.
    pending_focus: Option<SessionId>,
    /// A pane is zoomed: whether the docked navigator was shown before, to put it back on the
    /// way out.
    zoom_hold: Option<bool>,
    /// Agent terminals' conversation faces.
    faces: faces::Faces,
    /// The server's projects and their boards.
    projects: projects::ProjectsState,
    /// The number the next `OpenSession` goes under, for its answer to name.
    next_open: std::cell::Cell<slopty_proto::RequestId>,
    /// A file tile whose editor takes the keyboard on the next frame.
    pending_focus_file: Option<ItemId>,
    /// A folder tile that takes the keyboard on the next frame.
    pending_focus_folder: Option<ItemId>,
    /// A review tile, by its thread, that takes the keyboard on the next frame.
    pending_focus_review: Option<slopty_proto::thread::ThreadId>,
    /// The review tiles: which are open, which this client asked for.
    reviews: reviews::Reviews,
    /// The worktrees asked to go and not yet answered.
    worktrees: worktrees::Asked,
    /// Text on its way to a composer not made yet.
    quotes: workers::Quotes,
    pending_focus_picker: bool,
    pending_focus_self: bool,
    /// What held the keyboard a moment is gone: it goes back where the focused tile keeps it
    /// ([`Self::return_keyboard`]) in the next frame.
    pending_return: bool,
    /// The title bar's empty span is pressed: a move drags the window.
    title_press: bool,
    /// What the title bar asked of the window, kept by a test.
    #[cfg(test)]
    window_asks: Vec<titlebar::WindowAsk>,
    /// The keyboard lost (what held it left the frame) comes back to an open modal, else to
    /// where the focused tile keeps it.
    focus_lost: Option<Subscription>,
    /// Where the layout is saved (`layout.json` in the client's data directory), if anywhere.
    layout_path: Option<std::path::PathBuf>,
    /// What was last written there.
    layout_saved: Option<Saved>,
    /// What the last run left beside its layout that this one puts back as it can.
    restore: restore::Restore,
    save_pending: bool,
    /// Workers whose empty registry was given a shell this run.
    given_shell: HashSet<WorkerKey>,
    /// Workers whose given shell has not arrived yet, and the tile that had the focus when it
    /// was asked for: it goes back there when the shell lands.
    given_pending: HashMap<WorkerKey, Option<TileRef>>,
    /// The clipboard kept in step with the workers; `None` where the platform has none here.
    clip: Option<Rc<std::cell::RefCell<crate::clipboard::ClipSync>>>,
    /// The app is frontmost.
    app_active: bool,
    /// Remote tiles shown in windows of their own.
    popouts: popout::PopOuts,
    /// The window's title as last set: the project on show ([`Self::retitle_window`]).
    window_title: String,
    /// A thread held open from a navigator row's press until its tile's own view takes it.
    warm: Option<navigator::Warm>,
    /// Workers told this client wants their clipboard.
    watching: HashSet<WorkerKey>,
    /// Which workers the clipboard is shared with, as the settings say.
    clip_sharing: slopty_settings::ClipboardSettings,
    /// The workers it is not shared with, by key: read by the paste hooks as they run, so a
    /// change holds for the views made before it.
    clip_unshared: Rc<std::cell::RefCell<HashSet<WorkerKey>>>,
    /// Agents' pull requests, programs waiting on file tiles, and the shell told it has the
    /// focus.
    handoff: handoffs::HandoffState,
    /// The file tiles' unsaved edits, kept on this device; `None` where nothing is kept (a
    /// test without a store).
    kept: Option<unsaved::Kept>,
    /// Slopty's mark over the empty workspace.
    empty_mark: Entity<about::Mark>,
    /// Uploads in flight.
    uploads: HashMap<slopty_core::XferId, remote::Upload>,
    /// Downloads in flight, and the ledger that keeps transfers across a relaunch.
    transfers: remote::transfers::Transfers,
    /// The landing of the drop being handed to the tiles, for the tile that takes it to upload
    /// from and delete once the upload ends.
    drop_landing: Option<std::path::PathBuf>,
    /// The tile a drag of files from elsewhere is over, which says where they would land.
    files_over: Option<TileRef>,
    /// The workspace is a phone's this frame: its bar is the focused tile's, and tiles have no
    /// header of their own.
    phone: bool,
    /// Where a drag of worker files out of the app goes: a system drag, unless the self-test
    /// keeps the promises itself.
    #[cfg(target_os = "macos")]
    drag_sink: Option<remote::DragSink>,
    /// The drags out of a worker's app this window carried on.
    #[cfg(target_os = "macos")]
    drags_out: remote::DragsOut,
    /// Forwarded ports, by session.
    ports: HashMap<SessionId, Vec<slopty_client::tunnel::Forward>>,
    /// Secure keyboard entry, held while the focused tile takes a password.
    secure: secure::Secure,
    focus: FocusHandle,
}

impl std::fmt::Debug for WorkspaceView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceView")
            .field("workers", &self.workers.len())
            .field("tiles", &self.layout.tiles().count())
            .finish_non_exhaustive()
    }
}

impl EventEmitter<WorkspaceEvent> for WorkspaceView {}

impl Focusable for WorkspaceView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl WorkspaceView {
    /// An empty workspace, arranged as `saved` left it (the layout of the last run on this
    /// device), its tiles waiting for their workers.
    pub fn new(theme: Theme, saved: Option<Saved>, cx: &mut Context<Self>) -> Self {
        let layout = match saved.clone() {
            Some(saved) => Tiling::restore(saved.tiling, tiling_config()),
            None => Tiling::new(tiling_config()),
        };
        let navigator = saved.as_ref().map(|s| s.navigator.clone()).unwrap_or_default();
        // A terminal's own change (an echo) is not the workspace's, and a pane's own news
        // notifies the area's view instead ([`AreaHost`]): neither comes here.
        cx.observe_self(Self::changed).detach();
        let this = cx.weak_entity();
        let keys = cx.intercept_keystrokes(move |event, window, cx| {
            if event.keystroke.modifiers.platform {
                return;
            }
            let until = this.upgrade().and_then(|this| this.read(cx).echo_awaited(window, cx));
            if let Some(until) = until {
                crate::icons::hold_steps(cx, until);
            }
        });
        let empty_mark =
            gpui::AppContext::new(cx, |_| about::Mark::new(theme.clone(), "empty", false));
        Self {
            base_theme: theme.clone(),
            font_delta: 0.0,
            theme,
            layout,
            navigator,
            panes: panes::Panes::default(),
            sash_before: Vec::new(),
            title_scroll: gpui::ScrollHandle::new(),
            title_closing: title_tabs::Closing::default(),
            preview: preview::Preview::default(),
            foot: foot::Foot::default(),
            title_tabs_drawn: std::cell::RefCell::default(),
            epoch: Instant::now(),
            away_ticking: false,
            #[cfg(test)]
            held_clock: None,
            pinned_rtt: None,
            animate: true,
            reduced: cx.reduce_motion(),
            workers: BTreeMap::new(),
            terminals: HashMap::new(),
            screens: HashMap::new(),
            files: HashMap::new(),
            folders: HashMap::new(),
            probes: Vec::new(),
            browsers: HashMap::new(),
            browser_links: HashMap::new(),
            file_focus: HashMap::new(),
            items_dirty: true,
            drawn_waiting: Vec::new(),
            drawn_thread_waits: Vec::new(),
            attention_at: None,
            starts: slopty_client::starts::Starts::default(),
            starts_file: None,
            starts_writing: None,
            sessions_asked: None,
            past_places: HashMap::new(),
            folder_step: None,
            listing: HashSet::new(),
            starting: starting::Starts::default(),
            kept_items: kept_items::KeptItems::default(),
            faces_dirty: true,
            twins: HashMap::new(),
            derived: HashMap::new(),
            alike: HashSet::new(),
            places: HashMap::new(),
            titles_dirty: true,
            drawn: Rc::default(),
            chrome: Chrome::new(cx),
            area_host: {
                let workspace = cx.weak_entity();
                gpui::AppContext::new(cx, |_| AreaHost {
                    workspace,
                    #[cfg(test)]
                    builds: 0,
                })
            },
            #[cfg(test)]
            counts: (0, 0),
            #[cfg(test)]
            renders: 0,
            facts: facts::Facts::default(),
            frecency: saved.as_ref().map(|s| s.frecency.clone()).unwrap_or_default(),
            last_project: None,
            frame_projects: Rc::default(),
            _keys: keys,
            titles: HashMap::new(),
            server_status: None,
            finished: HashMap::new(),
            slow_command: SLOW_COMMAND,
            running_after: RUNNING_AFTER,
            recency: Vec::new(),
            awake: None,
            unseen: HashMap::new(),
            stream_grace: STREAM_GRACE,
            parked: HashSet::new(),
            picker: None,
            picker_leaving: None,
            palette: None,
            palette_leaving: None,
            search: project_search::Surface::default(),
            pending_find: None,
            pending_find_file: None,
            rename: None,
            rename_return: None,
            pending_focus_rename: false,
            palette_return: None,
            palette_action: None,
            palette_hidden: HashSet::new(),
            pending_runs: Vec::new(),
            tailnet_grant: None,
            palette_extra: Vec::new(),
            hardware_keyboard: true,
            pending_focus_palette: false,
            menu: None,
            menu_keyed: false,
            menu_leaving: None,
            menu_at: None,
            context_menu: None,
            new_on: None,
            anchors: titlebar::Anchors::default(),
            nav: navigator::NavState::default(),
            readouts: readouts::Readouts::default(),
            machines: machines::Machines::default(),
            machine_remove: None,
            approvals: approvals::Approvals::default(),
            settings: None,
            turns: turns::Turns::default(),
            more_entries: Vec::new(),
            server_entries: Vec::new(),
            show_stats: false,
            desktop: desktop::Desktop::default(),
            toast: None,
            closed: Vec::new(),
            closed_seq: 0,
            drag: None,
            drop_spots: Rc::default(),
            width_inset: 0.0,
            park_pending: false,
            pending_focus: None,
            zoom_hold: None,
            faces: faces::Faces::default(),
            projects: projects::ProjectsState {
                looked: saved
                    .iter()
                    .flat_map(|s| &s.looked)
                    .map(|l| {
                        let looked = crate::project::recap::Looked { seq: l.seq, at_ms: l.at_ms };
                        (l.project.clone(), looked)
                    })
                    .collect(),
                ..projects::ProjectsState::default()
            },
            next_open: std::cell::Cell::new(1),
            pending_focus_file: None,
            pending_focus_folder: None,
            pending_focus_review: None,
            reviews: reviews::Reviews::default(),
            worktrees: worktrees::Asked::default(),
            quotes: workers::Quotes::default(),
            pending_focus_picker: false,
            pending_focus_self: false,
            pending_return: false,
            title_press: false,
            #[cfg(test)]
            window_asks: Vec::new(),
            focus_lost: None,
            layout_path: None,
            restore: restore::Restore::of(saved.as_ref()),
            layout_saved: saved,
            save_pending: false,
            given_shell: HashSet::new(),
            given_pending: HashMap::new(),
            clip: None,
            app_active: true,
            popouts: popout::PopOuts::default(),
            window_title: String::new(),
            warm: None,
            watching: HashSet::new(),
            clip_sharing: slopty_settings::ClipboardSettings::default(),
            clip_unshared: Rc::default(),
            handoff: handoffs::HandoffState::default(),
            kept: None,
            empty_mark,
            uploads: HashMap::new(),
            transfers: remote::transfers::Transfers::default(),
            drop_landing: None,
            files_over: None,
            phone: false,
            #[cfg(target_os = "macos")]
            drag_sink: None,
            #[cfg(target_os = "macos")]
            drags_out: remote::DragsOut::default(),
            ports: HashMap::new(),
            secure: secure::secure(),
            focus: cx.focus_handle(),
        }
    }

    // ----- reading ---------------------------------------------------------------------------

    /// The tiling (read only; tests and the self-test dump).
    #[must_use]
    pub const fn layout(&self) -> &Tiling {
        &self.layout
    }

    /// The navigator's width, grouping and whether it shows.
    #[must_use]
    pub(super) const fn navigator(&self) -> &Navigator {
        &self.navigator
    }

    /// Change the navigator's width, grouping or whether it shows; saved with the tiling.
    pub(super) fn set_navigator(&mut self, navigator: Navigator) {
        self.navigator = navigator;
    }

    /// The focused tile.
    #[must_use]
    pub fn focused(&self) -> Option<TileRef> {
        self.layout.focused()
    }

    /// The focused tile's item id.
    #[must_use]
    pub fn active_item(&self) -> Option<ItemId> {
        self.focused().map(|t| t.item)
    }

    /// An item, whichever worker holds it.
    #[must_use]
    pub fn item(&self, tile: TileRef) -> Option<&Item> {
        self.workers.get(&tile.worker)?.doc.get(tile.item)
    }

    /// Every item of every worker, with its worker, in worker then creation order.
    pub fn items(&self) -> impl Iterator<Item = (WorkerKey, &Item)> {
        self.workers.iter().flat_map(|(key, w)| w.doc.items().map(move |i| (*key, i)))
    }

    /// The tile showing `item`, whichever worker holds it.
    #[must_use]
    pub fn tile_of(&self, item: ItemId) -> Option<TileRef> {
        self.workers
            .iter()
            .find(|(_, w)| w.doc.get(item).is_some())
            .map(|(key, _)| TileRef { worker: *key, item })
    }

    /// The tile showing `session`.
    #[must_use]
    pub fn tile_of_session(&self, session: SessionId) -> Option<TileRef> {
        self.workers.iter().find_map(|(key, w)| {
            w.doc.item_for_session(session).map(|i| TileRef { worker: *key, item: i.id })
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

    /// Where a tile was drawn last frame, in window points.
    #[must_use]
    pub fn tile_bounds(&self, tile: TileRef) -> Option<Bounds<Pixels>> {
        self.drawn.placed.borrow().iter().find(|(t, _)| *t == tile).map(|(_, b)| *b)
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

    /// The file tiles, for tests and the self-test dump.
    #[must_use]
    pub fn file(&self, id: ItemId) -> Option<&Entity<FileView>> {
        self.files.get(&id)
    }

    /// The folder tiles, for tests and the self-test dump.
    #[must_use]
    pub fn folder(&self, id: ItemId) -> Option<&Entity<FolderView>> {
        self.folders.get(&id)
    }

    /// The agent at work in `session` as its thread's row says, in the status vocabulary and
    /// in one short line; `None` with no agent there.
    #[must_use]
    pub fn agent(&self, session: SessionId) -> Option<(crate::icons::Status, String)> {
        let agent = self.agent_state(session)?;
        Some((agents::agent_mark_of(agent), agents::agent_status_text(agent)))
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

    /// Whether the palette is up.
    #[cfg(test)]
    #[must_use]
    const fn palette_open(&self) -> bool {
        self.palette.is_some()
    }

    /// The badge a session's last long command left, if the tile has not been looked at since.
    #[must_use]
    pub fn finished(&self, session: SessionId) -> Option<&Finished> {
        self.finished.get(&attention::About::Session(session))
    }

    /// Whether moves animate. The self-test turns this off so a dump right after an action
    /// sees where things landed, not where they were passing through.
    pub const fn set_animation(&mut self, on: bool) {
        self.animate = on;
    }

    /// GPUI's Reduce Motion flag changed: the springs, and every view, follow it.
    pub fn motion_setting_changed(&mut self, cx: &mut Context<Self>) {
        self.reduced = cx.reduce_motion();
        cx.notify();
    }

    /// Whether a keyboard is attached, so the palette prints chords that can be pressed.
    pub fn set_hardware_keyboard(&mut self, attached: bool, cx: &mut Context<Self>) {
        if self.hardware_keyboard != attached {
            self.hardware_keyboard = attached;
            cx.notify();
        }
    }

    /// Lines the app adds to the palette after the workspace's own (settings, workers).
    pub fn extend_palette(&mut self, items: Vec<PaletteItem>) {
        self.palette_extra = items;
    }

    /// The app's rows in the titlebar's "…" menu.
    pub fn set_more_menu(&mut self, entries: Vec<MenuEntry>, cx: &mut Context<Self>) {
        self.more_entries = entries;
        cx.notify();
    }

    /// The app's rows in the server readout's menu, while the server is offline: try it again
    /// now, connect to another.
    pub fn set_server_menu(&mut self, entries: Vec<MenuEntry>) {
        self.server_entries = entries;
    }

    /// Save the layout to `path` whenever it changes (debounced).
    pub fn set_layout_path(&mut self, path: std::path::PathBuf) {
        self.layout_path = Some(path);
    }

    // ----- plumbing --------------------------------------------------------------------------

    /// The worker a new session, note or lookup goes to: the focused tile's, else the first
    /// one that is up, else the first known.
    fn context_worker(&self) -> Option<WorkerKey> {
        self.focused()
            .map(|t| t.worker)
            .filter(|w| self.workers.get(w).is_some_and(|w| w.link.is_some()))
            .or_else(|| self.workers.iter().find(|(_, w)| w.link.is_some()).map(|(k, _)| *k))
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

    /// The time on the layout's clock.
    fn now(&self) -> Duration {
        #[cfg(test)]
        if let Some(held) = self.held_clock {
            return held;
        }
        self.epoch.elapsed()
    }

    /// The instant the layout's clock stands at: what the chrome's own motion (the navigator's
    /// plate) moves by, so it keeps step with the springs.
    fn clock_instant(&self) -> Instant {
        self.epoch.checked_add(self.now()).unwrap_or(self.epoch)
    }

    /// Hold the layout's clock at `at` since the workspace was made, or let it run (`None`).
    #[cfg(test)]
    pub(crate) const fn hold_clock(&mut self, at: Option<Duration>) {
        self.held_clock = at;
    }

    /// Something in the layout may have changed: save it once things settle.
    fn layout_touched(&mut self, cx: &Context<Self>) {
        if self.layout_path.is_none() || self.save_pending {
            return;
        }
        self.save_pending = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_AFTER).await;
            let write = this
                .update(cx, |this, _cx| {
                    this.save_pending = false;
                    let saved = this.to_save();
                    if this.layout_saved.as_ref() == Some(&saved) {
                        return None;
                    }
                    this.layout_saved = Some(saved.clone());
                    this.layout_path.clone().map(|path| (path, saved))
                })
                .ok()
                .flatten();
            if let Some((path, saved)) = write {
                cx.background_executor().spawn(async move { write_layout(&path, &saved) }).await;
            }
        })
        .detach();
    }

    /// Swap the theme everywhere: every terminal (which re-fits its grid to the new font on
    /// its next frame), every window's chrome, every file tile and the picker.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.base_theme = theme;
        self.apply_font(cx);
    }

    /// The settings' theme with the terminal text moved by ⌘=/⌘-.
    fn apply_font(&mut self, cx: &mut Context<Self>) {
        let mut theme = self.base_theme.clone();
        let size = theme.typography.mono_size + self.font_delta;
        theme.typography.mono_size = size.clamp(commands::FONT_MIN, commands::FONT_MAX);
        self.apply_theme(theme, cx);
    }

    fn apply_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme == theme {
            return;
        }
        for view in self.terminals.values() {
            view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        }
        for view in self.screens.values() {
            view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        }
        for view in self.files.values() {
            view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        }
        for view in self.folders.values() {
            view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        }
        for view in self.browsers.values() {
            view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        }
        self.set_threads_theme(&theme, cx);
        for view in self.projects.views.values() {
            view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        }
        if let Some((_, picker)) = &self.picker {
            picker.update(cx, |p, cx| p.set_theme(theme.clone(), cx));
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
        self.theme_marks(cx);
        cx.notify();
    }

    /// The theme in use (terminal size included).
    #[must_use]
    pub const fn theme(&self) -> &Theme {
        &self.theme
    }

    /// How long a key typed now waits for its echo, at most: the round trip to the focused
    /// terminal's worker and a refresh. `None` unless a terminal here has the keyboard.
    fn echo_awaited(&self, window: &Window, cx: &App) -> Option<Instant> {
        let terminal = self.active_terminal()?;
        if !terminal.read(cx).focus_handle(cx).is_focused(window) {
            return None;
        }
        let rtt = self.focused().and_then(|t| self.rtt(t.worker)).unwrap_or_default();
        cx.background_executor().now().checked_add(rtt.saturating_add(ECHO_SLACK))
    }

    /// Draw one region of the chrome, for its view.
    fn render_region(
        &self,
        region: Region,
        window: &Window,
        cx: &crate::draw::Draw<'_, Self>,
    ) -> gpui::AnyElement {
        match region {
            Region::Navigator => self.render_navigator_region(window, cx),
            Region::NavigatorRows => self.render_navigator_rows(window, cx),
            Region::Rail => self.navigator_rail(cx),
            Region::Titlebar => self.render_titlebar(window, cx),
            Region::Foot => self.render_foot(window, cx),
            Region::TileStrip => self.render_tile_strip(cx),
            Region::TitleTabs => {
                let tabs = self.title_tabs();
                let at = match self.landing() {
                    Some(area::Landing::Strip(at)) => Some(*at),
                    _ => None,
                };
                // A closed tab folds away on the layout's clock, among the project's own tabs.
                let closing = self.layout.shown_project().map(|project| {
                    let mut hasher = std::hash::DefaultHasher::new();
                    std::hash::Hash::hash(project.home(), &mut hasher);
                    let clock = title_tabs::Clock {
                        project: std::hash::Hasher::finish(&hasher),
                        now: self.clock_instant(),
                        moves: self.animate && crate::kit::motion(cx),
                    };
                    (&self.title_closing, clock)
                });
                // The tab on show is its one tile's title, which its field names in place.
                let field = self.lone_tile().and_then(|tile| {
                    let named = self
                        .rename
                        .as_ref()
                        .is_some_and(|r| r.tile == tile && r.field != Field::Address);
                    named.then(|| self.rename_field(tile, tile.item)).flatten()
                });
                let drops = title_tabs::Drops { spots: &self.drop_spots, at, closing, field };
                let drawn =
                    title_tabs::render(&self.theme, &tabs, &self.title_scroll, drops, window, cx);
                *self.title_tabs_drawn.borrow_mut() = tabs;
                drawn
            }
        }
    }

    /// The workspace changed (its own notify; never a frame of motion, see [`AreaHost`]): the
    /// chrome may show it and a title may follow it, and who needs the human and which
    /// worker's clipboard is wanted follow it at once, and so do the approvals asked of the
    /// workers. The faces follow in the next frame.
    fn changed(&mut self, cx: &mut Context<Self>) {
        #[cfg(test)]
        {
            self.counts.0 = self.counts.0.saturating_add(1);
        }
        self.titles_dirty = true;
        self.faces_dirty = true;
        self.projects.dirty = true;
        self.prune_facts();
        self.drawn_waiting = self.needs_you();
        self.drawn_thread_waits = self.threads_waiting();
        self.sync_clipboard_watch();
        self.sync_focus_report();
        self.sync_approvals(cx);
        self.light_marks(cx);
        self.chrome.notify(cx);
    }

    /// How many times each region of the chrome has drawn: the navigator (its panel and its
    /// rows together), the title bar.
    #[cfg(test)]
    fn chrome_renders(&self, cx: &App) -> [usize; 2] {
        let chrome = &self.chrome;
        let renders = |view: &Entity<ChromeView>| view.read(cx).renders;
        [
            renders(&chrome.navigator).saturating_add(renders(&chrome.nav_rows)),
            renders(&chrome.titlebar),
        ]
    }

    /// The length of every map and list the workspace keeps per tile, session or worker, by
    /// name: what closing everything that was opened must bring back to where it was.
    #[cfg(test)]
    fn footprint(&self) -> Vec<(&'static str, usize)> {
        let per_worker = |f: fn(&Worker) -> usize| self.workers.values().map(f).sum::<usize>();
        vec![
            ("workers", self.workers.len()),
            ("workers.sessions", per_worker(|w| w.sessions.len())),
            ("workers.watched", per_worker(|w| w.watched.len())),
            ("workers.watched_folders", per_worker(|w| w.watched_folders.len())),
            ("workers.pending_opens", per_worker(|w| w.pending_opens.len())),
            ("workers.openings", per_worker(|w| w.openings.len())),
            ("workers.queued", per_worker(|w| w.queued.len())),
            ("terminals", self.terminals.len()),
            ("screens", self.screens.len()),
            ("files", self.files.len()),
            ("folders", self.folders.len()),
            ("probes", self.probes.len()),
            ("browsers", self.browsers.len()),
            ("browser_links", self.browser_links.len()),
            ("file_focus", self.file_focus.len()),
            ("drawn_waiting", self.drawn_waiting.len()),
            ("drawn_thread_waits", self.drawn_thread_waits.len()),
            ("twins", self.twins.len()),
            ("derived", self.derived.len()),
            ("places", self.places.len()),
            ("titles", self.titles.len()),
            ("finished", self.finished.len()),
            ("recency", self.recency.len()),
            ("unseen", self.unseen.len()),
            ("parked", self.parked.len()),
            ("on_screen", self.drawn.on_screen.borrow().len()),
            ("tile_hits", self.search.tile_hits()),
            ("palette_extra", self.palette_extra.len()),
            ("more_entries", self.more_entries.len()),
            // The closed list itself is kept on purpose, up to `CLOSED_KEPT`; what must not
            // outlive the closing is what an entry still holds.
            ("closed_holding", self.closed.iter().filter(|c| c.holds()).count()),
            ("placed", self.drawn.placed.borrow().len()),
            ("given_shell", self.given_shell.len()),
            ("handed", self.drawn.handed.borrow().len()),
            ("given_pending", self.given_pending.len()),
            ("watching", self.watching.len()),
            ("uploads", self.uploads.len()),
            ("ports", self.ports.len()),
            ("nav.folded", self.nav.folded.len()),
            ("faces.chosen", self.faces.chosen.len()),
            ("faces.focus", self.faces.focus.len()),
            ("faces.drafts", self.faces.drafts.len()),
        ]
        .into_iter()
        .chain(self.project_sizes())
        .chain(self.facts.lens())
        .chain(self.handoff.sizes())
        .collect()
    }

    /// A shell changed, and its facts are copied again ([`facts::ShellFacts`]). Most of what a
    /// shell changes (its grid, its cursor) is its own; the rest is news only for what shows it.
    /// A command that starts or ends retitles the shell and renumbers its twins at once, for its
    /// navigator row and its header; a title its program sets is the chrome's news when the
    /// tile's title follows it; how the last command ended marks the tile, its row and its tab;
    /// who sizes the PTY is its header's.
    fn terminal_changed(&mut self, session: SessionId, cx: &mut Context<Self>) {
        self.follow_secure_input(cx);
        let Some((was, now)) = self.copy_shell(session, cx) else { return };
        let navigator = self.chrome.nav_rows.entity_id();
        // A command that starts or ends renames its shell and its twins.
        if was.running != now.running {
            self.number_twins();
            App::notify(cx, navigator);
            self.header_news(session, cx);
            // Still running at the threshold, the tile and its row say so: the readouts' clock
            // moves then, as well as each second ([`Self::keep_time`]).
            if now.running.is_some() {
                self.keep_time(cx);
                let after = self.running_after;
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(after).await;
                    let _gone = this.update(cx, |this, cx| {
                        if this.shell(session).is_some_and(|s| s.running.is_some()) {
                            this.tick_readouts(cx);
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
        } else if was.last != now.last {
            App::notify(cx, navigator);
        }
        if was.title != now.title {
            self.retitled(session, cx);
        }
        if (was.exit, was.failure_in_view) != (now.exit, now.failure_in_view) {
            cx.notify();
        } else if was.driving != now.driving {
            self.panes_news(cx);
        }
    }

    /// `session`'s header changed, news for the area. A title tab is named and marked by the
    /// tiles of its tab, so a tile of the project on show is the title tabs' news too where it
    /// changed what they say. On a
    /// phone the bar is the focused tile's header, so a change to that tile's title, kind or
    /// state is the bar's news as well; any other tile's is not.
    fn header_news(&self, session: SessionId, cx: &mut App) {
        self.panes_news(cx);
        let tile = self.tile_of_session(session);
        let shown = self.layout.shown_index();
        if tile.and_then(|t| self.layout.position(t)).is_some_and(|p| Some(p.project) == shown)
            && *self.title_tabs_drawn.borrow() != self.title_tabs()
        {
            App::notify(cx, self.chrome.title_tabs.entity_id());
        }
        if self.phone && tile.is_some_and(|t| self.focused() == Some(t)) {
            App::notify(cx, self.chrome.titlebar.entity_id());
        }
    }
}

impl WorkspaceView {
    /// Tab from the workspace itself (nothing else focused) enters the keyboard ring; inside
    /// a terminal or a text field Tab is theirs. ↵ there on an empty workspace runs its first
    /// way to begin, as the palette's selected row would.
    fn key_down(&mut self, ev: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) {
            return;
        }
        if crate::a11y::cycle(ev, window, cx) {
            cx.stop_propagation();
        } else if ev.keystroke.key == "enter"
            && !ev.keystroke.modifiers.modified()
            && self.bare()
            && !self.workers.is_empty()
        {
            cx.stop_propagation();
            self.start_here(window, cx);
        }
    }

    /// Esc closes the bar's open menu before the key reaches the tile that has the keyboard
    /// (a shell would take it as its own), as a menu bar's menu closes on it.
    fn menu_key(&mut self, ev: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.is_some() && ev.keystroke.key == "escape" && !ev.keystroke.modifiers.modified()
        {
            cx.stop_propagation();
            self.dismiss_menu(window, cx);
        }
    }

    /// After a ring step: a remote window with no tab stop around it kept the keys, so the
    /// workspace itself takes them — ⌃Tab is always the way out of a window, whose chords are
    /// all the worker's while it has the focus.
    fn leave_screen(&self, window: &mut Window, cx: &mut Context<Self>) {
        let on_screen = self
            .active_screen()
            .is_some_and(|screen| screen.read(cx).focus_handle(cx).is_focused(window));
        if on_screen {
            window.focus(&self.focus, cx);
        }
    }

    /// Focus asked for since the last frame, now that there is a window to give it in. Given
    /// here, so the views drawn after this one show it in this frame.
    fn give_pending_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.pending_focus.take() {
            // A tile showing its project's board takes the keyboard in the board, one showing
            // its thread in its composer.
            if let Some(board) = self.board_view(session).filter(|_| self.board_shown(session)) {
                board.clone().update(cx, |v, cx| v.focus(window, cx));
            } else if let Some(thread) = self.thread_face(session).cloned() {
                thread.update(cx, |v, cx| v.focus(window, cx));
            } else if let Some(view) = self.terminals.get(&session) {
                let handle = view.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
        }
        if let Some((session, query)) = self.pending_find.take()
            && let Some(view) = self.terminals.get(&session).cloned()
        {
            // After this frame: the tile is drawn and focused first, then its find bar takes
            // the keyboard.
            window.defer(cx, move |window, cx| {
                view.update(cx, |v, cx| v.find_with(&query, window, cx));
            });
        }
        if let Some((item, query)) = self.pending_find_file.take()
            && let Some(view) = self.files.get(&item).cloned()
        {
            window.defer(cx, move |window, cx| {
                view.update(cx, |v, cx| v.find_with(&query, window, cx));
            });
        }
        if std::mem::take(&mut self.pending_focus_picker)
            && let Some((_, picker)) = &self.picker
        {
            let handle = picker.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        if std::mem::take(&mut self.pending_return) {
            self.return_keyboard(window, cx);
        }
        // A page's next dialog may have taken the keyboard since its last one gave it back.
        if std::mem::take(&mut self.pending_focus_self)
            && !self.browsers.values().any(|b| b.read(cx).dialog_has_keyboard(window, cx))
        {
            window.focus(&self.focus, cx);
        }
        if std::mem::take(&mut self.pending_focus_palette)
            && let Some(palette) = &self.palette
        {
            let handle = palette.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        if std::mem::take(&mut self.pending_focus_rename)
            && let Some(rename) = &self.rename
        {
            rename.input.update(cx, |input, cx| {
                input.focus(window, cx);
                input.select_all(window, cx);
            });
        }
        if self.rename.is_none()
            && let Some(handle) = self.rename_return.take()
        {
            window.focus(&handle, cx);
        }
        if self.palette.is_none()
            && (self.palette_return.is_some() || self.palette_action.is_some())
        {
            // Nothing had the keyboard, or what had it is gone: the workspace takes it, so a
            // pick still runs from where its own actions are answered.
            let handle = self
                .palette_return
                .take()
                .filter(|h| self.focus.contains(h, window))
                .unwrap_or_else(|| self.focus.clone());
            window.focus(&handle, cx);
            if let Some(action) = self.palette_action.take() {
                // Once this frame is done, from the element that had the keyboard. A pick that
                // nothing there answers any more says so, rather than doing nothing.
                cx.defer_in(window, move |this, window, cx| {
                    if window.is_action_available(action.as_ref(), cx) {
                        window.dispatch_action(action, cx);
                    } else {
                        this.say_unavailable(action.as_ref(), cx);
                    }
                });
            }
        }
        self.settle_search_focus(window, cx);
        for run in std::mem::take(&mut self.pending_runs) {
            cx.defer_in(window, move |_this, window, cx| run(window, cx));
        }
        if let Some(id) = self.pending_focus_file.take()
            && let Some(view) = self.files.get(&id)
        {
            view.update(cx, |v, cx| v.focus(window, cx));
        }
        if let Some(id) = self.pending_focus_folder.take()
            && let Some(view) = self.folders.get(&id)
        {
            view.update(cx, |v, cx| v.focus(window, cx));
        }
        if let Some(thread) = self.pending_focus_review.take() {
            self.focus_review(thread, window, cx);
        }
        self.settle_starting_focus(window, cx);
    }
}

impl WorkspaceView {
    /// The window is called by the project on show, as a document window is by its
    /// document: the Window menu, Mission Control, cycling the windows by key and the screen
    /// reader tell two windows apart by it, where every one was the app's name. Set only when it
    /// changes.
    fn retitle_window(&mut self, window: &mut Window) {
        let title = self.project_name();
        if title != self.window_title {
            window.set_window_title(&title);
            self.window_title = title;
        }
    }
}

impl gpui::Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        use gpui::prelude::FluentBuilder as _;
        use gpui::{
            InteractiveElement as _, ParentElement as _, StatefulInteractiveElement as _,
            Styled as _,
        };

        if self.focus_lost.is_none() {
            // Once the frame that lost it is done: a focus moved inside the draw's focus phase
            // schedules no frame, so the new holder would not be drawn as holding it.
            self.focus_lost = Some(cx.on_focus_lost(window, |_this, window, cx| {
                cx.defer_in(window, |this, window, cx| {
                    if window.focused(cx).is_none() && !crate::a11y::reclaim(window, cx) {
                        this.return_keyboard(window, cx);
                    }
                });
            }));
        }

        #[cfg(test)]
        {
            self.renders = self.renders.saturating_add(1);
        }
        self.frame_projects.hold(true, cx);
        // A project's group can change only with the workspace or a fact, which both leave
        // the titles to work out.
        if self.titles_dirty && self.rehome_projects() {
            self.layout_touched(cx);
        }
        self.retitle_window(window);
        self.phone = self.width(window) < self.layout.config().phone_below;
        self.reduced = cx.reduce_motion();
        if std::mem::take(&mut self.items_dirty) {
            self.reconcile_files(window, cx);
            self.reconcile_browsers(cx);
            self.reconcile_reviews();
        }
        if std::mem::take(&mut self.faces_dirty) {
            self.sync_faces(window, cx);
        }
        self.settle_reviews(cx);
        self.settle_quotes(window, cx);
        self.settle_agent_screens(cx);
        self.settle_going(cx);
        self.settle_review_writers(cx);
        self.sync_projects(window, cx);
        self.keep_zoom(cx);
        self.give_pending_focus(window, cx);
        self.follow_secure_input(cx);
        self.follow_sized_displays(window, cx);
        self.arm_system_keys(window, cx);
        if std::mem::take(&mut self.titles_dirty) {
            self.number_twins();
            #[cfg(test)]
            {
                self.counts.1 = self.counts.1.saturating_add(1);
            }
        }
        // First: a docked navigator narrows the title bar and the area.
        self.place_navigator(window);
        self.sync_settings_aside(cx);
        if self.nav.drawn.is_some() {
            self.ensure_navigator_filter(window, cx);
            self.settle_navigator_filter(window, cx);
        }
        self.serve_browsers(cx);
        let area = gpui::IntoElement::into_any_element(self.area_host.clone());
        let menu = self.render_menu(window, cx);
        // What is leaving is drawn under the menu and what is live over it: a palette fading
        // out where a menu then opens must not take the menu's clicks.
        let picker = self.picker.as_ref().map(|(_, p)| p.clone());
        let palette = self.palette.clone();
        let picker_leaving = picker.is_none().then(|| self.picker_leaving.clone()).flatten();
        let palette_leaving = palette.is_none().then(|| self.palette_leaving.clone()).flatten();
        let mut key_context = gpui::KeyContext::new_with_defaults();
        key_context.add("Workspace");
        if self.page_keys(window, cx) {
            key_context.add("Page");
        }
        if self.closing_offered() {
            key_context.add(CLOSING_CTX);
        }
        let root = gpui::div()
            .id("workspace")
            .debug_selector(|| "workspace".to_owned())
            .key_context(key_context)
            .track_focus(&self.focus)
            .role(gpui::accesskit::Role::Group)
            .aria_label("Workspace")
            .on_key_down(cx.listener(Self::key_down))
            .capture_key_down(cx.listener(Self::menu_key))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(crate::colors::hsla(self.theme.surfaces.ground));
        let applies = self.applies();
        let root = Self::register_layout_actions(root, applies, cx);
        // Global: whatever has the focus.
        let root = root
            .on_action(cx.listener(Self::new_terminal))
            .on_action(cx.listener(Self::new_agent))
            .on_action(cx.listener(Self::start_agent))
            .on_action(cx.listener(Self::split_right))
            .on_action(cx.listener(Self::split_down))
            .on_action(cx.listener(Self::tab_terminal))
            .on_action(cx.listener(|this, a: &ShowTab, _w, cx| this.show_tab(a.id, cx)))
            .on_action(cx.listener(Self::new_note))
            .on_action(cx.listener(Self::add_window))
            .on_action(cx.listener(Self::open_file_palette))
            .on_action(cx.listener(Self::open_folder_palette))
            .on_action(cx.listener(Self::open_url_palette))
            .on_action(cx.listener(Self::list_ports))
            .on_action(cx.listener(Self::next_attention))
            .on_action(cx.listener(Self::show_needs_you))
            .on_action(cx.listener(Self::filter_navigator))
            .on_action(cx.listener(Self::open_palette))
            .on_action(cx.listener(Self::open_commands))
            .on_action(cx.listener(Self::edit_address))
            .on_action(cx.listener(Self::search_in_files))
            .on_action(cx.listener(Self::start_thread_action))
            .on_action(cx.listener(Self::new_agent_of))
            .on_action(cx.listener(Self::new_agent_on))
            .on_action(cx.listener(Self::new_project))
            .on_action(cx.listener(Self::new_project_of))
            .on_action(cx.listener(Self::new_project_on))
            .on_action(cx.listener(Self::start_orchestrator))
            .on_action(cx.listener(Self::resume_past_session))
            .on_action(cx.listener(Self::resume_session))
            .on_action(cx.listener(Self::review_pull))
            .on_action(cx.listener(Self::review_pull_in))
            .on_action(cx.listener(Self::review_pull_number))
            .on_action(cx.listener(Self::remove_machine))
            .on_action(cx.listener(Self::group_navigator_by))
            .on_action(cx.listener(Self::scope_to))
            .on_action(cx.listener(Self::pin_to_project))
            .on_action(cx.listener(Self::name_project))
            .on_action(cx.listener(Self::share_clipboard))
            .on_action(cx.listener(Self::edit_machine_settings));
        // Only while they apply to the focus ([`actions::Applies`]).
        let root = root
            .when(applies.tile, |el| {
                el.on_action(cx.listener(Self::close_item))
                    .on_action(cx.listener(Self::rename_item))
            })
            .when(applies.terminal, |el| el.on_action(cx.listener(Self::start_project)))
            .when(applies.agent, |el| {
                el.on_action(cx.listener(Self::switch_face))
                    .on_action(cx.listener(Self::make_orchestrator))
            })
            .when(applies.undo, |el| el.on_action(cx.listener(Self::undo_close)))
            .when(applies.tabs, |el| {
                el.on_action(cx.listener(Self::other_tabs))
                    .on_action(cx.listener(Self::close_other_tabs))
            })
            .when(applies.projects, |el| {
                el.on_action(cx.listener(Self::move_to_project))
                    .on_action(cx.listener(Self::move_to_project_of))
            })
            .when(applies.changes, |el| {
                el.on_action(cx.listener(Self::review_changes))
                    .on_action(cx.listener(Self::remove_merged))
            })
            .when(applies.worktree, |el| el.on_action(cx.listener(Self::remove_worktree)))
            .when(applies.streams, |el| el.on_action(cx.listener(Self::toggle_stats)))
            .when(applies.screen, |el| {
                el.on_action(cx.listener(Self::toggle_mute))
                    .on_action(cx.listener(Self::type_clipboard))
                    .on_action(cx.listener(Self::toggle_system_keys))
                    .on_action(cx.listener(Self::toggle_trackpad))
                    .on_action(cx.listener(Self::toggle_remote_gestures))
            })
            .when(applies.display, |el| el.on_action(cx.listener(Self::toggle_sized_display)))
            .when(applies.terminal || applies.file || applies.page, |el| {
                el.on_action(cx.listener(Self::find_in_active))
            })
            .when(applies.page, |el| {
                el.on_action(cx.listener(Self::page_back))
                    .on_action(cx.listener(Self::page_forward))
                    .on_action(cx.listener(Self::reload_page))
            })
            .when(applies.upload, |el| el.on_action(cx.listener(Self::upload_from_files)))
            .when(applies.file, |el| el.on_action(cx.listener(Self::save_copy)));
        root
            // Esc in the name field: the input's own action, taken here so the field closes
            // without a change and the workspace has the keyboard.
            .capture_action(cx.listener(
                |this, _: &gpui_kit::component::input::Escape, _window, cx| {
                    // Unless an input method composes in it: its Esc cancels the word.
                    if this.rename.as_ref().is_some_and(|r| !r.input.read(cx).is_composing()) {
                        this.finish_rename(false, true, cx);
                        cx.stop_propagation();
                    }
                },
            ))
            .on_action(cx.listener(|this, _: &FocusNext, window, cx| {
                crate::a11y::step(true, window, cx);
                this.leave_screen(window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusPrev, window, cx| {
                crate::a11y::step(false, window, cx);
                this.leave_screen(window, cx);
            }))
            .on_action(cx.listener(Self::toggle_navigator))
            .on_action(cx.listener(Self::toggle_navigator_lens))
            .child(Self::measure_width(cx))
            .child(Self::render_follow(cx))
            .child(self.render_frame(area, window, cx))
            .when_some(picker_leaving, gpui::ParentElement::child)
            .when_some(palette_leaving, gpui::ParentElement::child)
            .children(menu)
            .when_some(picker, gpui::ParentElement::child)
            .when_some(palette, gpui::ParentElement::child)
            .children(self.search_drawn())
            .children(self.render_project_sheet(window, cx))
            .children(self.render_remove_machine(window, cx))
    }
}

impl WorkspaceView {
    /// How wide the workspace is: the window's width less what the app kept of it last frame.
    /// A difference, not last frame's width: the window's width is this frame's, so a resize
    /// is seen at once and never lays the panes out for the old size.
    pub(super) fn width(&self, window: &Window) -> f32 {
        f32::from(window.viewport_size().width) - self.width_inset
    }

    /// Takes how much of the window's width the workspace was laid out in; a change draws one
    /// more frame at the new width, as the area's own measure does. Read while the window
    /// draws, and written only once it is done, and only when it changed: a write while it
    /// draws would build every view that read the workspace again.
    fn measure_width(cx: &Context<Self>) -> gpui::AnyElement {
        use gpui::{IntoElement as _, Styled as _};
        let entity = cx.entity();
        gpui::canvas(
            move |bounds, window, cx| {
                let inset = f32::from(window.viewport_size().width - bounds.size.width);
                if (entity.read(cx).width_inset - inset).abs() > f32::EPSILON {
                    cx.defer(move |cx| {
                        entity.update(cx, |this, cx| {
                            this.width_inset = inset;
                            cx.notify();
                        });
                    });
                }
            },
            |_bounds, (), _window, _cx| {},
        )
        .absolute()
        .inset_0()
        .into_any_element()
    }

    /// The frame: the navigator the window's full height on the left, docked beside the rest
    /// or laid over it (or the rail in its place), and the title bar over the area to its
    /// right, the area running to the window's bottom edge. The chrome's regions are their own
    /// views, drawn cached at the sizes laid out here.
    fn render_frame(
        &self,
        area: gpui::AnyElement,
        window: &Window,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        use gpui::{IntoElement as _, ParentElement as _, Styled as _, px};
        let safe = window.insets().effective();
        // Not cached, as the docked navigator is not: a retained view is drawn again from last
        // frame around a nested view notified alone (the title tabs' marks stepping), where a
        // cached one is built again whole.
        let titlebar = gpui::div()
            .w_full()
            .flex_none()
            .h(px(titlebar_height(&self.theme)) + safe.top)
            .flex()
            .child(self.chrome.titlebar.clone());
        let navigator = self.chrome.navigator.clone();
        // The rail draws no project rows to drop on.
        if self.nav.drawn.is_none() {
            self.drop_spots.projects.borrow_mut().clear();
        }
        let (docked, rail, over, handle) = match self.nav.drawn {
            Some(navigator::Mode::Docked) => {
                let width = px(self.navigator_width());
                let handle = Self::render_handle(width, cx);
                // Not cached, as in `navigator_over`. Its trailing edge is a sash line, as
                // every edge between the chrome and the panes is, laid over its last point so
                // it takes no room; the handle lies over it.
                let edge = gpui::div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right_0()
                    .w(crate::kit::HAIR)
                    .bg(crate::colors::hsla(self.theme.surfaces.sash));
                let column = gpui::div()
                    .relative()
                    .flex_none()
                    .h_full()
                    .w(width)
                    .flex()
                    .child(navigator)
                    .child(edge);
                (Some(column.into_any_element()), None, None, Some(handle))
            }
            // Over the panes, the rail stays under it, so the panes do not move as it opens.
            Some(mode) => {
                let over = self.navigator_over(mode, navigator, window, cx);
                (None, self.render_rail(), Some(over), None)
            }
            None => (None, self.render_rail(), None, None),
        };
        // The settings take the panes' place, and the foot bar goes with them.
        let page = self.render_settings_page();
        let area = page.unwrap_or(area);
        // Cached, as the title bar is not: nothing in it is a view notified alone.
        let foot = self.foot_drawn().then(|| {
            let height = px(self.foot_height());
            self.chrome
                .foot
                .clone()
                .cached(StyleRefinement::default().flex_none().w_full().h(height))
        });
        // The panes meet the frame's edges and each other on the one ground.
        let middle =
            gpui::div().relative().flex_1().min_h_0().w_full().flex().children(rail).child(
                gpui::div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(area),
            );
        gpui::div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .children(docked)
            .child(
                gpui::div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(titlebar)
                    .child(middle)
                    .children(foot),
            )
            .children(handle)
            .children(over)
            .into_any_element()
    }

    /// The navigator laid over the frame in `mode`, on the scrim, which a click closes it by.
    ///
    /// It enters as a sheet does: the panel slides in from the leading edge while the scrim
    /// comes up under it, on the sheet's time and the drawer's curve. Under Reduce Motion, or in
    /// a headless frame, it is there at once. Only the entry moves; it leaves at once.
    fn navigator_over(
        &self,
        mode: navigator::Mode,
        panel: Entity<ChromeView>,
        window: &Window,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        use gpui::{
            Animation, AnimationExt as _, InteractiveElement as _, IntoElement as _, MouseButton,
            ParentElement as _, Styled as _, px,
        };
        let safe = window.insets().effective();
        // A phone's drawer floats as iOS 26's sidebar does: inset from the safe area's top and
        // leading edges and from the window's bottom, its corners rounded, over a lighter scrim.
        // Laid over a wider frame, the panel meets the window's edges and runs under the
        // leading safe area, which its rows clear.
        let floats = mode == navigator::Mode::Drawer;
        let inset = px(self.theme.spacing.sm);
        let width = if floats {
            px(self.navigator_panel_width(mode, window))
        } else {
            px(self.navigator_panel_width(mode, window)) + safe.left
        };
        // Not cached: a cached view is built again whenever a view in it is, and a scroll of
        // the rows' view inside it then rebuilt the panel and its filter field.
        let panel = gpui::div().h_full().w(width).flex().child(panel);
        let handle = (mode == navigator::Mode::Overlay).then(|| Self::render_handle(width, cx));
        let sheet = || {
            Animation::new(slopty_theme::Motion::DEFAULT.sheet).with_easing(crate::kit::drawer())
        };
        let moves = self.animate && crate::kit::motion(cx);
        let dim = if floats {
            crate::kit::aside_scrim(&self.theme)
        } else {
            crate::kit::scrim(&self.theme)
        };
        let scrim = gpui::div().absolute().inset_0().bg(dim);
        let scrim = if moves {
            scrim
                .with_animation("navigator-scrim", sheet(), gpui::Styled::opacity)
                .into_any_element()
        } else {
            scrim.into_any_element()
        };
        let (top, bottom, left) = if floats {
            (safe.top + inset, inset, safe.left + inset)
        } else {
            (px(0.0), px(0.0), px(0.0))
        };
        let drawn = gpui::div()
            .debug_selector(|| "navigator-sheet".to_owned())
            .absolute()
            .top(top)
            .bottom(bottom)
            .left(left)
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .child(panel);
        let drawn = if moves {
            // From wholly past the leading edge, its shadow with it.
            drawn
                .with_animation("navigator-slide", sheet(), move |el, t| {
                    el.left(left - (width + left) * (1.0 - t))
                })
                .into_any_element()
        } else {
            drawn.into_any_element()
        };
        let away = gpui::div()
            .id("navigator-away")
            .debug_selector(|| "navigator-away".to_owned())
            .absolute()
            .inset_0()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, _w, cx| {
                    this.nav.open = false;
                    cx.notify();
                }),
            )
            .child(scrim)
            .child(drawn)
            .children(handle);
        match mode {
            // A touch device's workspace ends above the home indicator's band (and the key
            // bar, when it shows), which the app draws below it. Laid over the frame, on a
            // phone or an iPad, the panel is drawn after everything and unclipped, down to the
            // window's bottom edge, so it and its scrim cover the band; the workspace starts
            // at the window's top.
            navigator::Mode::Drawer | navigator::Mode::Overlay => {
                gpui::deferred(away.bottom_auto().h(window.viewport_size().height))
                    .into_any_element()
            }
            navigator::Mode::Docked => away.into_any_element(),
        }
    }
}

/// The tiling's constants for this device: touch's on iOS (an iPad's panes at the touch
/// minimum, and Split View below 900 pt drawn as a phone), a pointer's on the Mac.
const fn tiling_config() -> TilingConfig {
    if cfg!(target_os = "ios") { TilingConfig::TOUCH } else { TilingConfig::POINTER }
}

/// Write `saved` to `path` atomically; a failure is logged, never fatal: the layout is this
/// device's convenience, and the next change tries again.
fn write_layout(path: &std::path::Path, saved: &Saved) {
    let bytes = match serde_json::to_vec_pretty(saved) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!(error = %e, "layout serialise");
            return;
        }
    };
    if let Err(e) = slopty_platform::fs::replace(path, &bytes) {
        tracing::warn!(path = %path.display(), error = %e, "layout save");
    }
}

/// What the app says when the last layout could not be read and was set aside.
pub const LAYOUT_SET_ASIDE: &str =
    "The last layout could not be read; it was kept as layout.json.bad";

/// A layout file that does not read as this build's: set aside as `<path>.bad`, so the next
/// save does not overwrite it, and said to the person.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LayoutUnreadable;

/// Read the layout a previous run saved at `path`: none when there is no file.
///
/// One that does not read (another build's, or broken) is renamed to `<path>.bad`, replacing
/// an older one, so it costs an arrangement, never the app, and is still there to look at.
/// There is no reading of an older shape: the layout is this device's convenience.
///
/// # Errors
///
/// [`LayoutUnreadable`] when the file is there and does not read.
pub fn read_layout(path: &std::path::Path) -> Result<Option<Saved>, LayoutUnreadable> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "layout read");
            return Err(LayoutUnreadable);
        }
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|e| {
        let bad = path.with_extension("json.bad");
        tracing::warn!(path = %path.display(), error = %e, "layout parse: set aside");
        if let Err(e) = std::fs::rename(path, &bad) {
            tracing::warn!(path = %bad.display(), error = %e, "layout set aside");
        }
        LayoutUnreadable
    })
}

#[cfg(test)]
mod tests;
