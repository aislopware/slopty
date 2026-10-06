//! The Slopty app shell, shared by the macOS and iOS apps.
//!
//! Connects to every added worker at once and shows all of them in one workspace: each
//! worker's items are tiles in this device's layout, side by side with the others'.
//! Networking runs on a tokio runtime thread; GPUI owns the main thread. The two talk through
//! channels only. The platform binaries set up logging, the runtime and the GPUI application,
//! then call [`open_workspace`].

#![forbid(unsafe_code)]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

#[cfg_attr(
    not(feature = "e2e"),
    expect(
        dead_code,
        reason = "only the e2e build serves the self-test socket; linted in every build"
    )
)]
mod e2e;
mod editors;
pub mod finder;
mod hangs;
mod invite;
pub mod menus;
pub mod net;
mod presence;
mod server;
pub mod settings;
pub mod ssh;
pub mod this_mac;
#[cfg(target_os = "macos")]
mod update;
pub mod window;
pub mod workers;

use std::rc::Rc;

pub use finder::actions::ShowWorkersInFinder;
use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnimationExt as _, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, WindowOptions, div, px,
};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
pub use invite::actions::ConnectDevice;
pub use invite::open_link;
pub use settings::actions::{OpenAbout, OpenKeyboardShortcuts, OpenSettings};
use slopty_client::LinkEvent;
use slopty_client::layout::WorkerKey;
use slopty_core::{SessionId, WorkerId};
use slopty_platform::notify::{Notifier, Tap};
use slopty_proto::WorkerMsg;
use slopty_proto::server::Liveness;
use slopty_proto::terminal::{Progress, ProgressState};
use slopty_settings::{Loaded, Settings};
use slopty_theme::{Density, Spacing, Theme, Typography};
use slopty_ui::a11y::tab_stop;
use slopty_ui::colors::hsla;
use slopty_ui::kit::{self, ButtonKind};
use slopty_ui::screen::{ScreenView, Sticky};
use slopty_ui::settings_editor::{SettingsEditor, SettingsEditorEvent};
use slopty_ui::terminal::{TerminalView, TerminalViewEvent};
use slopty_ui::workspace::attention::Attention;
use slopty_ui::workspace::{
    Finished, HostActions, KeyTarget, MenuEntry, MenuGroup, MenuRun, WorkerLink, WorkerStatus,
    WorkspaceEvent, WorkspaceView,
};
pub use ssh::actions::{UpdateAllWorkers, UpdateServer};
pub use window::actions::{Minimize, OpenHelp, ShowWindow, Zoom};
pub use window::{HELP_URL, show as show_main_window};
pub use workers::actions::{AddWorker, ConnectServer, CopyTailnetGrant};
use workers::{Hearing, Tick, WorkerSlot};

/// A finger drives this build: the key bar the soft keyboard lacks (Esc, Tab, Control, arrows,
/// shell symbols), a Paste where the Mac has ⌘V, full-width primary actions.
pub(crate) const TOUCH: bool = cfg!(target_os = "ios");

/// The key bar is for a touch platform typing on glass: a hardware keyboard has every key on
/// it, so the row hides while one is attached and comes back when it is unplugged.
const fn key_bar_visible(touch_platform: bool, hardware_keyboard: bool) -> bool {
    touch_platform && !hardware_keyboard
}
/// The key bar's height: a finger's target, whatever density the rest of the chrome is at,
/// since the bar is only ever on glass.
const KEY_BAR_H: f32 = Density::TOUCH.hit;
/// Half a point: a scroll offset this close to an end is at it.
const AT_END: f32 = 0.5;
/// The add-worker panel's width: a line of help, the tailnet's rows and an address field
/// beside its button, not a document.
const ADD_PANEL_W: f32 = 440.0;
/// From this width the address field and its action share a row: a tablet's, a Mac's. Under
/// it (a phone, an iPad's Split View column) the action is a thumb's full width under the field.
/// Chosen by the room, not by the input: a 440 pt Connect button under its field on a 1032 pt
/// iPad was the phone's stack.
const FIELD_ROW_FROM: f32 = 600.0;
/// This device can ask Tailscale what is on the tailnet: not on iOS, where no app can read it.
const LISTS_TAILNET: bool = !cfg!(target_os = "ios");
/// What the panel says where it cannot look on the tailnet, so its absence has a reason.
const UNLISTED: &str = "This device cannot list your tailnet, so type an address.";
/// The machine panel's line where this device can add none (an iPhone, an iPad).
const FROM_A_MAC: &str = "Machines are added from a Mac.";
/// How, under it.
const FROM_A_MAC_HOW: &str = "On a Mac, Use this Mac shares it, and Install over SSH adds a Mac or \
                              Linux machine you reach. They show here once your server lists them.";
/// The address field's height, and its button's beside it: T3 Code's field (`h-9`) under a
/// pointer, a finger's target on glass.
const FIELD_H: f32 = if TOUCH { Density::TOUCH.hit } else { 36.0 };

/// Whether a hardware keyboard is attached. The e2e build lets `SLOPTY_HARDWARE_KEYBOARD=0|1`
/// decide instead of the platform: on the simulator `GameController` reports the Mac's keyboard
/// about a second after launch, and a golden must not depend on which side of that poll the
/// frame landed on.
fn hardware_keyboard_attached() -> bool {
    #[cfg(feature = "e2e")]
    if let Some(forced) = std::env::var_os("SLOPTY_HARDWARE_KEYBOARD") {
        return forced == "1";
    }
    slopty_platform::hardware_keyboard_attached()
}

/// Link events applied per foreground turn at most (see the link loop).
const LINK_BATCH: usize = 256;
/// How long an answer sent from a note is given to leave the link before the system hears the
/// app is done with the tap, and the grace that woke the app for it goes.
const ANSWER_FLUSH: std::time::Duration = std::time::Duration::from_secs(2);

/// The key bar's keys: label, GPUI key name, and the character it types (`None` for
/// non-printing keys). In the order a phone shows them before the row scrolls: the keys the
/// soft keyboard has no way to type come first, the punctuation it only hides after, then
/// paging and the rarer modifiers.
///
/// The modifiers are the glyphs iPadOS's own shortcut sheet shows (⌃ ⌥ ⌘), square caps; the
/// paging keys are a keyboard's legends (`PgUp`, `PgDn`), since ⇞ and ⇟ draw too small to read.
/// The row spreads into its groups from an 11-inch iPad (820 pt) and scrolls a little on the
/// narrowest (744 pt). ⌥ is Alt as Meta for the next key (readline's and Emacs' chords). Home
/// and End are ⌘← and ⌘→, which the armed ⌘ already makes.
const BAR_KEYS: [(&str, &str, Option<&str>); 15] = [
    ("Esc", "escape", None),
    ("Tab", "tab", None),
    (CONTROL_CAP, "", None),
    ("←", "left", None),
    ("↑", "up", None),
    ("↓", "down", None),
    ("→", "right", None),
    ("~", "~", Some("~")),
    ("|", "|", Some("|")),
    ("/", "/", Some("/")),
    ("-", "-", Some("-")),
    ("PgUp", "pageup", None),
    ("PgDn", "pagedown", None),
    (ALT_CAP, "alt", None),
    ("⌘", "cmd", None),
];
/// The key bar's sticky Control.
const CONTROL_CAP: &str = "⌃";
/// The key bar's sticky Alt, sent as Meta.
const ALT_CAP: &str = "⌥";
/// Where the arrows end in [`BAR_KEYS`]: the clipboard key follows them.
const ARROWS_END: usize = 7;

/// A key cap's height: the bar's, a base unit clear of its edges.
fn cap_side(spacing: Spacing) -> f32 {
    2.0_f32.mul_add(-spacing.xs, KEY_BAR_H)
}

/// A key cap's width: square for a symbol, a word's a small step wider each side. Fixed, never
/// grown: an iPad's spare width stays spare rather than stretching "|" to 100 pt, and a phone's
/// row scrolls rather than crowds.
fn cap_width(label: &str, spacing: Spacing) -> f32 {
    let side = cap_side(spacing);
    if label.chars().count() > 1 { 2.0_f32.mul_add(spacing.sm, side) } else { side }
}

/// Where a key sits on a row wide enough for all of it (an iPad's): one leading run of groups,
/// as iOS's own input-assistant bar has, with the word keys (Copy, Paste, Find) trailing. Spread
/// in three islands the arrows floated alone in the middle, far from both hands' keys.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum KeyGroup {
    /// Esc, Tab and the sticky modifiers.
    Lead,
    /// The four arrows.
    Arrows,
    /// Shell symbols.
    Symbols,
    /// The word keys, at the row's trailing end.
    Words,
}

/// The group `label`'s key belongs to.
fn key_group(label: &str) -> KeyGroup {
    match label {
        "Esc" | "Tab" | CONTROL_CAP | ALT_CAP | "⌘" => KeyGroup::Lead,
        "←" | "↑" | "↓" | "→" | "PgUp" | "PgDn" => KeyGroup::Arrows,
        word if word.chars().count() > 1 => KeyGroup::Words,
        _ => KeyGroup::Symbols,
    }
}

/// How wide a row of `labels`' caps is, a gap between each and at its ends.
fn key_row_width<'a>(labels: impl IntoIterator<Item = &'a str>, spacing: Spacing) -> f32 {
    let (count, caps) = labels
        .into_iter()
        .fold((0.0_f32, 0.0_f32), |(n, w), label| (n + 1.0, w + cap_width(label, spacing)));
    2.0_f32.mul_add(spacing.xs, (count - 1.0).max(0.0).mul_add(spacing.xs, caps))
}

/// How wide the row of `labels`' caps is laid out as one run of groups: [`key_row_width`] with
/// a step between the groups where a gap between caps was. At most three breaks: the lead, the
/// arrows, the symbols, then the word keys trailing.
fn spread_width<'a>(labels: impl IntoIterator<Item = &'a str>, spacing: Spacing) -> f32 {
    3.0_f32.mul_add(spacing.md - spacing.xs, key_row_width(labels, spacing))
}

/// How far a row of `labels`' caps scrolls in `width`: none where it spreads out, else what
/// its one line runs past the edge. Known from the caps' fixed widths before any layout.
fn key_row_overflow<'a>(
    labels: impl IntoIterator<Item = &'a str> + Clone,
    width: f32,
    spacing: Spacing,
) -> f32 {
    if spread_width(labels.clone(), spacing) > width {
        (key_row_width(labels, spacing) - width).max(0.0)
    } else {
        0.0
    }
}

/// Which ends of the key row fade, as `(leading, trailing)`: the leading one once the row is
/// scrolled off its start, the trailing one while keys remain past the edge. `scrolled` is how
/// far the row is scrolled in and `max` how far it can be (zero when it fits); an offset
/// kept from a wider row counts as the end, where layout clamps it.
fn key_bar_fades(scrolled: f32, max: f32) -> (bool, bool) {
    let scrolled = scrolled.clamp(0.0, max);
    (scrolled > AT_END, scrolled < max - AT_END)
}
/// The key bar over a remote window: ⌘ joins ⌃ (an IDE lives on chords), the shell
/// punctuation goes.
const SCREEN_BAR_KEYS: [(&str, &str, Option<&str>); 9] = [
    ("Esc", "escape", None),
    ("Tab", "tab", None),
    (CONTROL_CAP, "", None),
    ("⌘", "cmd", None),
    ("←", "left", None),
    ("↑", "up", None),
    ("↓", "down", None),
    ("→", "right", None),
    ("/", "/", Some("/")),
];
/// What the panel over the workspace is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Panel {
    /// Connect to a server, whose directory lists the machines: the first run, and a move to
    /// another server.
    Server,
    /// Add a machine to the server: this Mac, one over SSH, or one the tailnet found that the
    /// server does not list.
    Worker,
}

/// The server address field's example: a small always-on box, by name or by its tailnet IP.
const SERVER_EXAMPLE: &str = "home-server or 100.64.0.1";
/// What the server address field takes, as its label names it.
const SERVER_FIELD: &str = "Server address";

/// The address field's label: "Or type an address" only under rows to press, where "or" has a
/// first way to follow; else what the field takes.
fn address_label(search: Option<&Search>) -> &'static str {
    if search.is_some_and(|s| !s.offers(Panel::Server).is_empty()) {
        "Or type an address"
    } else {
        SERVER_FIELD
    }
}

/// The panel that connects to a server or adds a machine: the page of the first run, and a
/// dialog on "Connect to another server" or "Add a machine".
#[derive(Debug)]
struct Adding {
    /// Which of the two.
    mode: Panel,
    /// The first run's page, the whole window, for as long as it is up: there is nothing to go
    /// back to, so it has no Cancel.
    page: bool,
    /// Where the server's address is typed or pasted.
    address: Entity<InputState>,
    /// A connection attempt is in flight.
    busy: bool,
    /// Why the last attempt failed.
    error: Option<String>,
    /// Where the look on the tailnet stands; `None` where none is made (iOS, where no app can
    /// read Tailscale, and a server's panel while a server is set).
    search: Option<Search>,
    /// "Use this Mac" under way: its checklist stands in for the rest.
    this_mac: Option<this_mac::Flow>,
    /// "Use this Mac" asks which server first: several answered, or none could be looked for.
    /// The address field then takes it, empty for one on this Mac.
    asking: Option<Asking>,
    /// "Install on a machine over SSH": its form, then its steps, stand in for the address.
    ssh: Option<ssh::Sheet>,
    /// The panel's own focus, where a machine's panel (no field to type in) keeps the
    /// keyboard, so Esc and the palette reach it.
    focus: gpui::FocusHandle,
    /// Return in the address field acts on what it holds; it goes with the panel.
    _enter: gpui::Subscription,
}

/// Why "Use this Mac" asks for the server.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Asking {
    /// Several servers answered on the tailnet.
    Several,
    /// No look was possible: Tailscale is not up here, or this device cannot read it.
    Blind,
}

impl Asking {
    /// What the panel says above the field.
    const fn words(self) -> &'static str {
        match self {
            Self::Several => "Several servers answered. Pick one, or type its address.",
            Self::Blind => {
                "No tailnet to look for a server on. Type its address, or leave it empty to run one on this Mac."
            }
        }
    }
}

impl Adding {
    /// Whether an input method is composing in one of the panel's fields: Esc is its own then.
    fn composing(&self, cx: &App) -> bool {
        self.address.read(cx).is_composing() || self.ssh.as_ref().is_some_and(|s| s.composing(cx))
    }
}

/// What the tailnet offers the panel: a server to connect to, or a worker to install over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Host {
    /// A server; pressing it connects to it.
    Server,
    /// A worker the server does not list; pressing it installs this build there, registered
    /// with the server.
    Worker,
}
/// A node that answered on the tailnet, by name and tailnet IP, and what it said.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Offer {
    /// Its `MagicDNS` name.
    name: String,
    /// Its tailnet IP, which the panel dials.
    at: String,
    /// Its tailnet tags, which a grant for it names.
    tags: Vec<String>,
    /// What it answered: ready, on another build, or turning this device away.
    answer: slopty_net::discover::Answer,
}

impl From<slopty_net::discover::Found> for Offer {
    fn from(found: slopty_net::discover::Found) -> Self {
        Self {
            name: found.name,
            at: found.addr.ip().to_string(),
            tags: found.tags,
            answer: found.answer.unwrap_or(slopty_net::discover::Answer::Ready),
        }
    }
}

impl Offer {
    /// The grant that lets this device in as a client of it: by its tags when it has any, else
    /// by its address.
    fn grant(&self) -> String {
        let dst: Vec<&str> = if self.tags.is_empty() {
            vec![self.at.as_str()]
        } else {
            self.tags.iter().map(String::as_str).collect()
        };
        server::grant_to(&dst)
    }
}

/// The panel's look on the tailnet for servers and workers.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Search {
    /// Probing the tailnet's nodes.
    Looking,
    /// These answered, each kind best first.
    Answered {
        /// Nodes that answered as a server.
        servers: Vec<Offer>,
        /// Nodes that answered as a worker the server does not list.
        workers: Vec<Offer>,
        /// This Mac's Tailscale is up: an empty answer is the tailnet's, not a look never made.
        running: bool,
    },
}

impl Search {
    /// The rows a panel in `mode` offers: a server's panel the servers, a machine's the
    /// workers the server does not list.
    fn offers(&self, mode: Panel) -> Vec<(Host, &Offer)> {
        let Self::Answered { servers, workers, .. } = self else { return Vec::new() };
        match mode {
            Panel::Server => servers.iter().map(|o| (Host::Server, o)).collect(),
            Panel::Worker => workers.iter().map(|o| (Host::Worker, o)).collect(),
        }
    }

    /// The servers that answered ready, once the look is done; `None` while it looks or where
    /// it could not look.
    fn ready_servers(&self) -> Option<Vec<&Offer>> {
        match self {
            Self::Answered { servers, running: true, .. } => Some(
                servers
                    .iter()
                    .filter(|o| o.answer == slopty_net::discover::Answer::Ready)
                    .collect(),
            ),
            Self::Looking | Self::Answered { .. } => None,
        }
    }

    /// What the panel in `mode` says while it has nothing to offer.
    fn words(&self, mode: Panel) -> Option<&'static str> {
        match self {
            Self::Looking => Some(LOOKING),
            Self::Answered { running: false, .. } => {
                self.offers(mode).is_empty().then_some(NOT_RUNNING)
            }
            Self::Answered { .. } => self.offers(mode).is_empty().then_some(match mode {
                Panel::Server => NO_SERVER_FOUND,
                Panel::Worker => NO_MACHINE_FOUND,
            }),
        }
    }

    /// What the line under the scan's row says: what to do about it. Nothing while it looks.
    const fn next_step(&self, mode: Panel) -> Option<&'static str> {
        match (self, mode) {
            (Self::Looking, _) => None,
            (Self::Answered { running: false, .. }, _) => {
                Some("Start it, or type an address on your VPN.")
            }
            (Self::Answered { .. }, Panel::Server) => {
                Some("Run the Slopty server on any machine there, then scan again.")
            }
            (Self::Answered { .. }, Panel::Worker) => {
                Some("Install Slopty on a machine there, then scan again.")
            }
        }
    }
}

/// The panel's word while it probes the tailnet.
const LOOKING: &str = "Looking on your tailnet\u{2026}";
/// The scan's row when no server on the tailnet answered.
const NO_SERVER_FOUND: &str = "No Slopty server found yet";
/// The scan's row when no machine on the tailnet answered.
const NO_MACHINE_FOUND: &str = "No machine found yet";
/// The scan's way to look again.
const SCAN_AGAIN: &str = "Scan again";
/// The scan's section, as its label says it.
const ON_TAILNET: &str = "On your tailnet";
/// The panel's word when this Mac has no tailnet to look on: Tailscale is off or absent.
const NOT_RUNNING: &str = "Tailscale is not running on this Mac";
/// The first run's foot: why there is no pairing code, key or password to find.
const TAILNET_NOTE: &str =
    "Tailscale or your VPN encrypts every link, so there is nothing to pair.";

/// The window's root view.
#[derive(Debug)]
pub struct Workspace {
    /// Every worker, each with its own link.
    workers: Vec<WorkerSlot>,
    /// Where the key bar sends its keys, while it can show: followed from the view's changes
    /// so the build reads no view.
    key_target: Option<KeyTarget>,
    /// Times it was built, for the tests.
    #[cfg(test)]
    renders: usize,
    /// The server's worker directory as last heard (the cache while it does not answer).
    directory: slopty_client::directory::Directory,
    /// The server in use, if any.
    server: Option<server::ServerSlot>,
    /// Bumped whenever the server changes, so a late event from the old link is dropped.
    server_generation: u64,
    /// What the cached directory should hold, for its one writer ([`server::write_cache`]).
    directory_cache: tokio::sync::watch::Sender<server::Cache>,
    /// Every worker's tiles, in one layout.
    view: Entity<WorkspaceView>,
    /// A physical keyboard is attached (polled with the settings; hides the key bar).
    hardware_keyboard: bool,
    /// The system asks for motion to be reduced (polled with the settings).
    reduce_motion: bool,
    theme: Theme,
    /// The user's `settings.toml` as last loaded (defaults when absent or broken).
    settings: Settings,
    /// The window's appearance is dark (`theme.appearance = "system"` follows it).
    window_dark: bool,
    /// The workspace view's events and changes, heard for as long as the app runs.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "held so they live; only the tests count them")
    )]
    subscriptions: Vec<gpui::Subscription>,
    /// The main window's watches (its appearance, its activation, where it stands), made again
    /// with each window ([`window::open`]).
    window_subscriptions: Vec<gpui::Subscription>,
    /// The system's Reduce Motion heard as it changes ([`watch_reduce_motion`]).
    motion_watch: Option<slopty_platform::motion::Watch>,
    /// The system's wakes, unlocks, returns to the front and path changes ([`watch_resumes`]).
    resume_watch: Option<slopty_platform::resume::Watch>,
    adding: Option<Adding>,
    /// The code a phone or iPad scans for the server, while it is up.
    inviting: Option<invite::Invite>,
    /// Networking runtime; connects run there.
    runtime: tokio::runtime::Handle,
    /// The window, for focusing from a task or a banner.
    window: Option<gpui::AnyWindowHandle>,
    /// Where `settings.toml` lives (the data directory's).
    settings_path: std::path::PathBuf,
    /// Its stamp as last loaded or written here, for the watcher.
    settings_seen: settings::Seen,
    /// The in-app settings editor while it is open.
    settings_editor: Option<Entity<SettingsEditor>>,
    /// What the editor asks for, heard while it is open.
    settings_editor_events: Option<gpui::Subscription>,
    /// The settings dialog just closed, drawn for the moment it takes to fade away.
    settings_leaving: Option<Entity<SettingsEditor>>,
    /// Focus the editor's field on the next frame (it needs a frame to exist).
    pending_focus_editor: bool,
    /// The self-test's stand-in for iPad Split View and Stage Manager: the app laid out in
    /// this size at the window's top left. A UIKit window cannot be resized from inside the
    /// app, and the layout only needs the size it is given to be the size it lays out in.
    split_view: Option<gpui::Size<gpui::Pixels>>,
    /// Where the key row is scrolled to.
    key_bar_scroll: ScrollHandle,
    /// What "Use this Mac" does to this machine; `None` where it is not offered.
    this_mac: Option<Rc<dyn this_mac::Host>>,
    /// Runs of it so far, so an answer for one left behind is dropped.
    this_mac_runs: u64,
    /// This Mac's worker, as its `doctor` last said: once the server lists it, "Add a
    /// machine"'s row for this Mac says so.
    this_mac_worker: Option<WorkerId>,
    /// What installs and updates a worker over SSH; `None` where it is not offered.
    deployer: Option<Rc<dyn ssh::Deployer>>,
    /// Runs of the SSH sheet so far, so an answer for one left behind is dropped.
    ssh_runs: u64,
    /// Updates from the tiles of a worker on a different build, by host.
    updates: ssh::Updating,
    /// This Mac's workers brought to this build unasked this launch: once each.
    updated_unasked: std::collections::HashSet<WorkerId>,
    /// The server being brought to this build ([`Workspace::update_server`]).
    server_update: Option<gpui::Task<()>>,
    /// What reaches the system's notifications while the app is not in front.
    attention: Attention,
    /// The terminals whose finished commands [`Self::attention`] hears and whose progress the
    /// Dock shows, by session.
    heard_terminals: std::collections::HashMap<SessionId, Heard>,
    /// What the Dock tile's bar shows, so a terminal's redraw sets it only when it changes.
    dock_progress: Option<slopty_platform::dock::DockProgress>,
    /// Where the person is, as the server is told it.
    presenting: presence::Presenting,
    /// How the worker loops dial: over the network, or a test's stand-in.
    dial: Dialer,
    /// The time iOS grants after the app leaves the screen, held until it returns, so the links
    /// stay up for what arrives just after the phone is pocketed.
    #[cfg(target_os = "ios")]
    grace: Option<slopty_platform::notify::BackgroundGrace>,
    /// The time iOS grants an app woken in the background by a note's "Allow" or "Deny", held
    /// until the answer is out ([`WorkspaceEvent::TapsSettled`] and [`ANSWER_FLUSH`] after),
    /// so it is not suspended while the answer waits for its link.
    #[cfg(target_os = "ios")]
    answer_grace: Option<slopty_platform::notify::BackgroundGrace>,
    /// Tells the system the taps are done, and on iOS lets the answer grace go, once the
    /// answers have had time to leave.
    answer_flush: Option<gpui::Task<()>>,
    /// The system's paste button over the key bar's Paste, made with the first key bar, so a
    /// paste there needs no permission alert.
    #[cfg(target_os = "ios")]
    paste_key: Option<Rc<slopty_ui::paste_key::PasteKey>>,
}

/// A terminal the app follows: its finished commands and notes, and its redraws, which carry
/// its progress report (a report has no event of its own).
struct Heard {
    _events: [gpui::Subscription; 2],
    /// The report last read, so a redraw that did not change it costs one comparison.
    progress: Progress,
}

impl std::fmt::Debug for Heard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Heard").field("progress", &self.progress).finish_non_exhaustive()
    }
}

impl Workspace {
    /// The shell round `view`: no worker yet, the default theme until the settings are applied.
    /// `settings_seen` is the file's stamp from before it was read, so an edit in between is
    /// still seen; `directory_cache` feeds the cache's one writer ([`server::write_cache`]).
    fn new(
        view: Entity<WorkspaceView>,
        runtime: tokio::runtime::Handle,
        settings_path: std::path::PathBuf,
        settings_seen: settings::Seen,
        directory_cache: tokio::sync::watch::Sender<server::Cache>,
        cx: &mut Context<Self>,
    ) -> Self {
        let events = cx.subscribe(&view, |ws: &mut Self, _view, event, cx| match event {
            // The badge is the inbox's count, which `Self::look` follows.
            WorkspaceEvent::NeedsYou(_) => {}
            // As a Mac app's alert: heard only while the human is elsewhere. Led by a server,
            // its notice sounds it instead, so a moment never sounds twice.
            WorkspaceEvent::Attention(_session) => {
                let active = cx.active_window().is_some();
                if !ws.attention.server_led() && settings::alerts(&ws.settings, active) {
                    alert();
                }
            }
            // A program's own notification is this client's, server or not.
            WorkspaceEvent::Program(_session) => {
                if settings::alerts(&ws.settings, cx.active_window().is_some()) {
                    alert();
                }
            }
            // A bell while the human is elsewhere is an alert; in front of the window the
            // view's own flash is enough.
            WorkspaceEvent::Bell(_session) => {
                if settings::alerts(&ws.settings, cx.active_window().is_some()) {
                    alert();
                }
            }
            WorkspaceEvent::Unanswered { route, why } => {
                let title = ws.view.read(cx).route_title(*route);
                ws.attention.unanswered(*route, title, why);
            }
            WorkspaceEvent::ClipboardShared { worker, share } => {
                ws.keep_clipboard_shared(*worker, *share, cx);
            }
            WorkspaceEvent::TapsSettled => ws.answers_out(cx),
        });
        // The key bar follows the focused tile: the app's own build is drawn again only when
        // the tile it sends keys to moves, never for the rest of the view's news.
        let changes = cx.observe(&view, |ws, _view, cx| {
            ws.look(cx);
            ws.presence_changed(cx);
            if ws.follow_key_target(cx) {
                cx.notify();
            }
        });
        let this_mac = this_mac::native(&runtime);
        let dial = network_dialer(runtime.clone());
        let deployer = ssh::native(&runtime);
        let this = Self {
            workers: Vec::new(),
            key_target: None,
            #[cfg(test)]
            renders: 0,
            directory: slopty_client::directory::Directory::default(),
            server: None,
            server_generation: 0,
            directory_cache,
            view,
            hardware_keyboard: hardware_keyboard_attached(),
            reduce_motion: slopty_platform::reduce_motion(),
            theme: Theme::default(),
            settings: Settings::default(),
            window_dark: true,
            subscriptions: vec![events, changes],
            window_subscriptions: Vec::new(),
            motion_watch: None,
            resume_watch: None,
            adding: None,
            inviting: None,
            runtime,
            window: None,
            settings_path,
            settings_seen,
            settings_editor: None,
            settings_editor_events: None,
            settings_leaving: None,
            pending_focus_editor: false,
            split_view: None,
            key_bar_scroll: ScrollHandle::new(),
            this_mac,
            this_mac_runs: 0,
            this_mac_worker: None,
            deployer,
            ssh_runs: 0,
            updates: ssh::Updating::new(),
            updated_unasked: std::collections::HashSet::new(),
            server_update: None,
            attention: Attention::new(Rc::new(slopty_platform::notify::Memory::default())),
            heard_terminals: std::collections::HashMap::new(),
            dock_progress: None,
            presenting: presence::Presenting::default(),
            dial,
            #[cfg(target_os = "ios")]
            grace: None,
            #[cfg(target_os = "ios")]
            answer_grace: None,
            answer_flush: None,
            #[cfg(target_os = "ios")]
            paste_key: None,
        };
        this.publish_updates(cx);
        this.repoint_this_mac(cx);
        this.ask_this_mac_worker(cx);
        Self::watch_presence(cx);
        this
    }

    /// Post notes through `notifier` from now on (the system's in the app).
    fn set_notifier(&mut self, notifier: Rc<dyn Notifier>) {
        self.attention = Attention::new(notifier);
    }

    /// The workspace changed: hand the attention what it follows, and hear every terminal's
    /// finished commands.
    fn look(&mut self, cx: &mut Context<Self>) {
        let view = self.view.read(cx);
        let look = view.attention_look();
        let terminals = view.terminal_views();
        self.attention.look(&look);
        let before = self.heard_terminals.len();
        self.heard_terminals.retain(|session, _| terminals.iter().any(|(s, _)| s == session));
        let mut changed = self.heard_terminals.len() != before;
        for (session, terminal) in terminals {
            if self.heard_terminals.contains_key(&session) {
                continue;
            }
            let heard =
                cx.subscribe(&terminal, move |ws: &mut Self, _terminal, event, cx| match event {
                    TerminalViewEvent::CommandFinished { command, exit, elapsed } => {
                        let done =
                            Finished { command: command.clone(), exit: *exit, elapsed: *elapsed };
                        ws.command_finished(session, &done, cx);
                    }
                    TerminalViewEvent::Notification { title, body } => {
                        ws.program_note(session, title, body, cx);
                    }
                    _ => {}
                });
            let redrawn = cx.observe(&terminal, move |ws: &mut Self, terminal, cx| {
                let progress = terminal.read(cx).state().progress();
                if let Some(heard) = ws.heard_terminals.get_mut(&session)
                    && heard.progress != progress
                {
                    heard.progress = progress;
                    ws.show_progress();
                }
            });
            let progress = terminal.read(cx).state().progress();
            changed |= progress.state != ProgressState::None;
            self.heard_terminals.insert(session, Heard { _events: [heard, redrawn], progress });
        }
        if changed {
            self.show_progress();
        }
    }

    /// Gather every terminal's progress report into the Dock tile's bar.
    fn show_progress(&mut self) {
        let reports = self.heard_terminals.values().map(|heard| heard.progress);
        let progress = slopty_platform::dock::DockProgress::gather(reports);
        if progress != self.dock_progress {
            self.dock_progress = progress;
            slopty_platform::dock::set_progress(progress);
        }
    }

    /// A program in `session` asked for a desktop notification (OSC 9 / 777 / 99): it notifies
    /// while the app is away, under the tile's name.
    fn program_note(&mut self, session: SessionId, title: &str, body: &str, cx: &Context<Self>) {
        let view = self.view.read(cx);
        let Some((route, tile)) = view.attention_route(session) else { return };
        let (title, body) = slopty_ui::workspace::program_banner(Some(&tile), title, body);
        self.attention.program(route, title, body);
    }

    /// A shell command ended in `session`: a long one notifies while the app is away.
    fn command_finished(&mut self, session: SessionId, done: &Finished, cx: &Context<Self>) {
        let view = self.view.read(cx);
        let Some((route, title)) = view.attention_route(session) else { return };
        let slow = view.slow_command();
        self.attention.command_finished(route, title, done, slow);
    }

    /// The app came to the front or left it: back in front, it says once a run that
    /// notifications are off when a note went unsaid meanwhile. On iOS, leaving holds the
    /// background grace.
    fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        self.attention.set_active(active);
        if active && self.attention.unsaid_while_off() {
            self.show_notice(slopty_ui::workspace::attention::NOTES_OFF.to_owned(), cx);
        }
        #[cfg(target_os = "ios")]
        {
            self.grace = if active {
                None
            } else {
                slopty_platform::notify::BackgroundGrace::begin("Slopty keeps its links")
            };
        }
    }

    /// ⌘, / "Settings…" / the palette: the file's text (the commented defaults when there is
    /// none) in the in-app editor. A second ask while it is open just refocuses it.
    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_focus_editor = true;
        if self.settings_editor.is_some() {
            cx.notify();
            return;
        }
        let text = settings::editable_text(&self.settings_path);
        let path = self.settings_path.display().to_string();
        let theme = self.theme.clone();
        let editor = cx.new(|cx| SettingsEditor::new(&text, &path, theme, window, cx));
        editor.update(cx, |e, cx| e.set_palette_words(app_palette_items(), cx));
        self.settings_editor_events =
            Some(cx.subscribe_in(&editor, window, |this, editor, event, window, cx| match event {
                SettingsEditorEvent::Apply(text) => {
                    this.write_settings(text, editor, cx);
                }
                SettingsEditorEvent::Save(text) => {
                    if this.write_settings(text, editor, cx) {
                        this.close_settings(window, cx);
                    }
                }
                SettingsEditorEvent::Dismiss => this.close_settings(window, cx),
            }));
        self.settings_editor = Some(editor);
        cx.notify();
    }

    /// The settings, open on `section`'s page.
    fn open_settings_at(
        &mut self,
        section: slopty_ui::settings_form::schema::Section,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_settings(window, cx);
        if let Some(editor) = &self.settings_editor {
            editor.update(cx, |e, cx| e.show_section(section, window, cx));
        }
    }

    /// A change from the settings form, or the file's face saved: a text that parses is written
    /// and applied at once, and the watcher takes it as seen; one that does not stays in the
    /// editor with the reason under it. Returns whether it was written.
    fn write_settings(
        &mut self,
        text: &str,
        editor: &Entity<SettingsEditor>,
        cx: &mut Context<Self>,
    ) -> bool {
        match settings::save(&self.settings_path, text, &mut self.settings_seen) {
            Ok(loaded) => {
                tracing::info!(path = %self.settings_path.display(), "settings saved");
                self.apply_loaded(loaded, cx);
                true
            }
            Err(error) => {
                editor.update(cx, |e, cx| e.set_error(error, cx));
                false
            }
        }
    }

    /// Drop the editor and hand the keyboard back to the focused tile at once; the dialog
    /// fades out where it stands for its way out, then goes.
    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_editor_events = None;
        let Some(editor) = self.settings_editor.take() else { return };
        let during = editor.update(cx, SettingsEditor::leave);
        if !during.is_zero() {
            self.settings_leaving = Some(editor.clone());
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(during).await;
                let _gone = this.update(cx, |this, cx| {
                    if this.settings_leaving.as_ref() == Some(&editor) {
                        this.settings_leaving = None;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        self.view.update(cx, |view, cx| view.return_keyboard(window, cx));
        cx.notify();
    }

    /// The system's Reduce Motion setting changed. What moves reads it as it is drawn, and a
    /// view drawn from the last frame read it then: every window is drawn again from scratch.
    /// GPUI's own animations (gpui-kit's among them) follow GPUI's flag, which is set with it.
    fn set_reduce_motion(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.reduce_motion != on {
            tracing::info!(on, "reduce motion");
            self.reduce_motion = on;
            slopty_ui::icons::motion_setting_changed(cx);
            self.view.update(cx, WorkspaceView::motion_setting_changed);
            cx.set_reduce_motion(on);
            cx.refresh_windows();
        }
    }

    /// A keyboard was attached or removed: show or hide the key bar and the palette's chords.
    /// Take the view's key target where the key bar can show. Whether it moved to another
    /// tile (or came or went), which is all the app's own build shows of the view.
    fn follow_key_target(&mut self, cx: &App) -> bool {
        let target = key_bar_visible(TOUCH, self.hardware_keyboard)
            .then(|| self.view.read(cx).active_key_target())
            .flatten();
        let id = |t: &KeyTarget| match t {
            KeyTarget::Terminal(e) => e.entity_id(),
            KeyTarget::Screen(e) => e.entity_id(),
        };
        let moved = self.key_target.as_ref().map(id) != target.as_ref().map(id);
        self.key_target = target;
        moved
    }

    fn set_hardware_keyboard(&mut self, attached: bool, cx: &mut Context<Self>) {
        if self.hardware_keyboard != attached {
            tracing::info!(attached, "hardware keyboard");
            self.hardware_keyboard = attached;
            self.view.update(cx, |v, cx| v.set_hardware_keyboard(attached, cx));
            self.follow_key_target(cx);
            cx.notify();
        }
    }

    /// Take a (re)loaded settings file: log what was odd about it, show it as a toast for a
    /// few seconds, and rebuild the theme and the keys.
    ///
    /// A file that does not parse changes nothing: the settings last applied stay (the
    /// defaults, on a launch), and no chord is taken from other apps on its account.
    fn apply_loaded(&mut self, mut loaded: Loaded, cx: &mut Context<Self>) {
        if let Some(error) = &loaded.error {
            tracing::error!(%error, "settings ignored");
            self.show_failure(format!("Settings: {error}"), cx);
            self.rebuild_theme(cx);
            return;
        }
        let keymap = keymap_for(&loaded.settings.keys);
        loaded.warnings.extend(keymap.diagnostics().iter().cloned());
        for warning in &loaded.warnings {
            tracing::warn!(%warning, "settings");
        }
        if let Some(first) = loaded.warnings.first() {
            let more = loaded.warnings.len().saturating_sub(1);
            let text = if more == 0 {
                format!("Settings: {first}")
            } else {
                format!("Settings: {first} (+{more} more)")
            };
            self.show_notice(text, cx);
        }
        let server = loaded.settings.client.server.clone();
        let sharing = loaded.settings.clipboard.clone();
        slopty_ui::file::open_with::set_link(loaded.settings.client.editor.clone(), cx);
        self.settings = loaded.settings;
        self.view.update(cx, |v, cx| v.set_clipboard_sharing(sharing, cx));
        self.rebuild_theme(cx);
        self.apply_keymap(keymap, cx);
        // The app's palette lines show their chords from the keymap just bound.
        let palette = app_palette(cx);
        self.view.update(cx, |v, _| v.extend_palette(palette));
        self.set_server(server, None, cx);
        self.refresh_menu(cx);
        // A server taken out of the file by hand leaves nothing to show: the first run again.
        if self.server.is_none() && self.adding.is_none() {
            self.show_first_run(cx);
        }
    }

    /// The first run's page, from where no window is at hand; nothing before the window is up,
    /// whose launch shows it then.
    fn show_first_run(&self, cx: &Context<Self>) {
        let Some(window) = self.window else { return };
        cx.spawn(async move |this, cx| {
            let _shown = cx.update_window(window, |_root, window, cx| {
                this.update(cx, |ws, cx| {
                    if ws.server.is_none() && ws.adding.is_none() {
                        ws.show_add_worker(Panel::Server, window, cx);
                    }
                })
            });
        })
        .detach();
    }

    /// Bind `keymap` in place of the keys bound now, unless it binds the same, and show the
    /// menus' chords from it: the "…" menu's, and the menu bar's, whose key equivalents AppKit
    /// runs itself. The palette reads it when it opens.
    fn apply_keymap(&self, keymap: slopty_ui::keymap::Keymap, cx: &mut Context<Self>) {
        if keymap.binds_as(&slopty_ui::keymap::current()) {
            return;
        }
        tracing::info!(said = keymap.diagnostics().len(), "keys rebound");
        slopty_ui::keymap::install(keymap, cx);
        rebuild_app_menus(cx);
        self.refresh_menu(cx);
    }

    /// The window turned dark or light.
    fn set_window_dark(&mut self, dark: bool, cx: &mut Context<Self>) {
        if self.window_dark != dark {
            self.window_dark = dark;
            self.rebuild_theme(cx);
        }
    }

    /// Derive the theme from the settings and the window, and push it everywhere.
    fn rebuild_theme(&mut self, cx: &mut Context<Self>) {
        let theme = settings::theme_for(&self.settings, self.window_dark);
        if theme == self.theme {
            return;
        }
        // gpui-kit widgets (inputs, Markdown) read gpui-kit's theme: keep it on
        // the same tokens.
        kit::sync(&theme, cx);
        self.view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        if let Some(editor) = &self.settings_editor {
            editor.update(cx, |e, cx| e.set_theme(theme.clone(), cx));
        }
        self.theme = theme;
        cx.refresh_windows();
        cx.notify();
    }

    /// A word for the human, as a toast over the workspace.
    fn show_notice(&self, text: String, cx: &mut Context<Self>) {
        self.view.update(cx, |v, cx| v.show_notice(text, cx));
    }

    /// Something that failed, which stays until dismissed.
    fn show_failure(&self, text: String, cx: &mut Context<Self>) {
        self.view.update(cx, |v, cx| v.show_failure(text, cx));
    }

    fn slot(&self, id: WorkerId) -> Option<&WorkerSlot> {
        self.workers.iter().find(|w| w.id == id)
    }

    fn slot_mut(&mut self, id: WorkerId) -> Option<&mut WorkerSlot> {
        self.workers.iter_mut().find(|w| w.id == id)
    }

    /// A note was tapped: its tile comes forward, on whichever worker it lives. A note's
    /// "Allow" or "Deny" may have woken the app in the background: the system is told the tap
    /// is done only once the answer is out ([`Self::answers_out`]), and on iOS the app holds
    /// the grace the system grants until then too.
    fn open_notification(&mut self, tap: &Tap, cx: &mut Context<Self>) {
        if slopty_ui::workspace::attention::answers(tap) {
            self.answer_flush = None;
            #[cfg(target_os = "ios")]
            if self.answer_grace.is_none() {
                self.answer_grace =
                    slopty_platform::notify::BackgroundGrace::begin("Slopty sends your answer");
            }
        }
        self.view.update(cx, |v, cx| v.open_notification(tap, cx));
    }

    /// Every tapped answer is out, or said to have found nothing: once the last has had
    /// [`ANSWER_FLUSH`] to leave the link, the system hears the taps are done and the grace
    /// held for them goes.
    fn answers_out(&mut self, cx: &Context<Self>) {
        self.answer_flush = Some(cx.spawn(async |ws, cx| {
            cx.background_executor().timer(ANSWER_FLUSH).await;
            slopty_platform::notify::taps_finished();
            #[cfg(target_os = "ios")]
            let _gone = ws.update(cx, |ws, _cx| ws.answer_grace = None);
            #[cfg(not(target_os = "ios"))]
            let _unused = ws;
        }));
    }

    /// Start (or refresh) a worker the directory lists: its tiles wait in the workspace and a
    /// connect loop of its own brings them to life.
    fn add_worker(&mut self, id: WorkerId, name: String, cx: &mut Context<Self>) {
        if let Some(slot) = self.slot_mut(id) {
            slot.name.clone_from(&name);
            let key = slot.key;
            self.view.update(cx, |v, cx| v.add_worker(key, name, cx));
            return;
        }
        let slot = WorkerSlot::new(id, name.clone());
        let key = slot.key;
        self.workers.push(slot);
        self.view.update(cx, |v, cx| v.add_worker(key, name, cx));
        self.spawn_worker_loop(id, cx);
        self.refresh_menu(cx);
    }

    /// Ask the server to forget a worker that is not online (`Verb::ForgetWorker`; it refuses
    /// one that is). It leaves when the directory unlists it, its pages with it.
    fn forget_worker(&self, id: WorkerId, cx: &mut Context<Self>) {
        let name = self.directory.get(id).map_or_else(|| id.to_string(), |w| w.name.clone());
        let Some(caller) = self.server_caller() else {
            self.show_failure(
                format!("Could not forget {name}: the server does not answer yet"),
                cx,
            );
            return;
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.runtime.spawn(async move {
            let _gone = tx.send(
                caller.call(slopty_proto::orchestration::Verb::ForgetWorker { worker: id }).await,
            );
        });
        cx.spawn(async move |this, cx| {
            let Ok(outcome) = rx.await else { return };
            let _gone = this.update(cx, |ws, cx| match outcome {
                slopty_proto::orchestration::Outcome::Error { message, .. } => {
                    ws.show_failure(format!("Could not forget {name}: {message}"), cx);
                }
                _forgotten => ws.show_notice(format!("Forgot {name}"), cx),
            });
        })
        .detach();
    }

    /// A forgotten worker's pages lose their cookies and storage with the tiles that held them,
    /// which go as this change is handled: `WebKit` deletes the store once it lets go of their
    /// last page (`slopty_platform::web::forget`).
    fn forget_pages(key: WorkerKey) {
        slopty_platform::web::forget(key.value(), |gone| {
            if let Err(why) = gone {
                tracing::warn!(%why, "a forgotten worker's pages kept their store");
            }
        });
    }

    /// Drop a worker's slot: its link, and its tiles.
    fn drop_slot(&mut self, id: WorkerId, cx: &mut Context<Self>) {
        let Some(at) = self.workers.iter().position(|w| w.id == id) else { return };
        let slot = self.workers.remove(at);
        if let Some(link) = slot.link.as_ref().and_then(std::sync::Weak::upgrade) {
            link.abandon("worker dropped");
        }
        slot.wake();
        self.view.update(cx, |v, cx| v.remove_worker(slot.key, cx));
        self.refresh_menu(cx);
        cx.notify();
    }

    /// The app's rows in the titlebar's "…" menu: the settings, then the server and adding a
    /// worker. Forgetting a worker is the hosts popover's, beside the worker it forgets.
    fn refresh_menu(&self, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let mut entries = Vec::new();
        let bindings = app_key_bindings();
        let hint = |action: &dyn gpui::Action| {
            if cfg!(target_os = "macos") {
                slopty_ui::palette::keys_for(action, &bindings)
            } else {
                String::new()
            }
        };
        {
            let this = this.clone();
            entries.push(MenuEntry {
                group: MenuGroup::Settings,
                label: "Settings".into(),
                detail: hint(&OpenSettings).into(),
                run: Rc::new(move |window, cx| {
                    let _gone = this.update(cx, |ws, cx| ws.open_settings(window, cx));
                }),
            });
        }
        {
            let this = this.clone();
            let label = if self.server.is_some() {
                "Connect to another server"
            } else {
                "Connect to a server"
            };
            let entry = MenuEntry {
                group: MenuGroup::Connections,
                label: label.into(),
                detail: self
                    .server_address()
                    .map(|a| a.host().to_owned())
                    .unwrap_or_default()
                    .into(),
                run: Rc::new(move |window, cx| {
                    let _gone =
                        this.update(cx, |ws, cx| ws.show_add_worker(Panel::Server, window, cx));
                }),
            };
            entries.push(entry);
        }
        entries.push(MenuEntry {
            group: MenuGroup::Connections,
            label: "Add a machine\u{2026}".into(),
            detail: hint(&AddWorker).into(),
            run: Rc::new(move |window, cx| {
                let _gone = this.update(cx, |ws, cx| ws.show_add_worker(Panel::Worker, window, cx));
            }),
        });
        self.view.update(cx, |v, cx| v.set_more_menu(entries, cx));
        let server_menu = Self::server_menu(cx);
        self.view.update(cx, |v, _cx| v.set_server_menu(server_menu));
        self.refresh_hosts(cx);
    }

    /// What the server's readout offers while the server is offline: try it now, or connect
    /// to another.
    fn server_menu(cx: &Context<Self>) -> Vec<MenuEntry> {
        let this = cx.entity().downgrade();
        let again = this.clone();
        vec![
            MenuEntry {
                group: MenuGroup::Connections,
                label: "Retry now".into(),
                detail: SharedString::default(),
                run: Rc::new(move |_window, cx| {
                    let _gone = again.update(cx, |ws, _cx| ws.resume_server());
                }),
            },
            MenuEntry {
                group: MenuGroup::Connections,
                label: "Connect to another server".into(),
                detail: SharedString::default(),
                run: Rc::new(move |window, cx| {
                    let _gone =
                        this.update(cx, |ws, cx| ws.show_add_worker(Panel::Server, window, cx));
                }),
            },
        ]
    }

    /// What a machine's menu in the navigator can do to each worker: dial it now, wake one the
    /// server can send a magic packet to (the palette offers that too), and forget one the
    /// server lists as not online; and its way to add a worker.
    fn refresh_hosts(&self, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let hosts = self
            .workers
            .iter()
            .map(|slot| {
                let id = slot.id;
                let connect: MenuRun = {
                    let this = this.clone();
                    Rc::new(move |_window, cx| {
                        let _gone = this.update(cx, |ws, _cx| ws.connect_now(id));
                    })
                };
                let away = self.directory.get(id).is_some_and(|w| w.liveness != Liveness::Online);
                let forget = away.then(|| {
                    let this = this.clone();
                    let run: MenuRun = Rc::new(move |_window, cx| {
                        let _gone = this.update(cx, |ws, cx| ws.forget_worker(id, cx));
                    });
                    run
                });
                let wake = self.directory.can_wake(id).then(|| {
                    let this = this.clone();
                    let run: MenuRun = Rc::new(move |_window, cx| {
                        let _gone = this.update(cx, |ws, cx| ws.wake_worker(id, cx));
                    });
                    run
                });
                (slot.key, HostActions { connect: Some(connect), forget, wake })
            })
            .collect();
        let add: MenuRun = Rc::new(move |window, cx| {
            let _gone = this.update(cx, |ws, cx| ws.show_add_worker(Panel::Worker, window, cx));
        });
        self.view.update(cx, |v, cx| {
            v.set_tailnet_grant(Some(server::client_grant()));
            v.set_host_actions(hosts, Some(add), cx);
        });
    }

    /// Something may have killed the links: the device was away, the app came back to the
    /// front, or the path moved. Each live link is probed at once and a dead one dialled again
    /// at once ([`workers::Probe`]); a worker between links is dialled now. On a path change the
    /// connections migrate to the new path first (QUIC's own migration, which the probe then
    /// tests), so a link that survives it is never dialled again. The server link is probed
    /// the same way.
    pub(crate) fn resume(&self, resume: slopty_platform::resume::Resume) {
        tracing::info!(
            resume = resume.name(),
            workers = self.workers.len(),
            "resume; probing the links"
        );
        if resume.moved() {
            net::path_changed();
        }
        self.resume_server();
        for slot in &self.workers {
            slot.resume(resume);
        }
    }

    /// Dial `id` now rather than at the end of its backoff.
    fn connect_now(&self, id: WorkerId) {
        if let Some(slot) = self.slot(id) {
            slot.wake();
        }
    }

    /// Connect to `id` and keep it connected: each drop (worker restart, network change,
    /// silence) is redialled on the shared backoff ([`slopty_net::redial`]); the loop ends
    /// when the worker is dropped.
    /// While the server says the worker is away the loop waits for it to come back online, for
    /// a wake (a resume, a click), or for [`server::HOLD_RETRY`] in case the server is the one
    /// that cannot see it, and then dials once. A
    /// worker on another build changes only when someone updates it, so it is asked again after
    /// [`slopty_net::redial::WRONG_BUILD`], or at once when something wakes the loop.
    fn spawn_worker_loop(&self, id: WorkerId, cx: &Context<Self>) {
        let view = self.view.clone();
        cx.spawn(async move |this, cx| {
            let mut redial = slopty_net::redial::Redial::default();
            let mut held = false;
            // A link a probe found dead after a resume, whose tiles still show what it showed,
            // set back, until the next link lands or fails (make before break).
            let mut replacing = false;
            loop {
                let Ok(Some(plan)) = this.update(cx, |ws, _cx| ws.plan(id)) else {
                    break;
                };
                let server::Plan { key, wake, address, hold } = plan;
                if let Some(status) = hold
                    && !held
                {
                    if std::mem::take(&mut replacing) {
                        view.update(cx, |v, cx| {
                            v.threads_unlinked(key, cx);
                            v.disconnect_worker(key, status.clone(), cx);
                        });
                    }
                    view.update(cx, |v, cx| v.set_worker_status(key, status, cx));
                    // Woken (back online, the server went away, a resume, a click on the
                    // worker) or timed out: either way it is dialled once before the next hold,
                    // since only the direct dial can tell whether the server's word still holds.
                    let _woken = wait_or_wake(cx, &wake, server::HOLD_RETRY).await;
                    held = true;
                    continue;
                }
                held = false;
                // Not listed yet: there is nowhere to dial until the directory says where.
                let Some(address) = address else {
                    view.update(cx, |v, cx| v.set_worker_status(key, WorkerStatus::Connecting, cx));
                    let _woken = wait_or_wake(cx, &wake, server::HOLD_RETRY).await;
                    continue;
                };
                let Ok(dialing) =
                    this.update(cx, move |ws, cx| (ws.dial.clone().0)(id, address, cx))
                else {
                    break;
                };
                let connected = match dialing.await {
                    Ok(connected) => connected,
                    Err(failed) => {
                        let wrong_build = matches!(failed, net::DialFailed::WrongBuild(_));
                        let Ok(status) = this.update(cx, |ws, cx| {
                            if let net::DialFailed::WrongBuild(notice) = &failed {
                                ws.heard_other_build(id, notice, cx);
                            }
                            ws.failure_status(id, failed)
                        }) else {
                            break;
                        };
                        let delay = if wrong_build {
                            slopty_net::redial::WRONG_BUILD
                        } else {
                            redial.next(std::time::Instant::now())
                        };
                        if std::mem::take(&mut replacing) {
                            view.update(cx, |v, cx| {
                            v.threads_unlinked(key, cx);
                            v.disconnect_worker(key, status.clone(), cx);
                        });
                        }
                        view.update(cx, |v, cx| v.set_worker_status(key, status, cx));
                        wait_or_wake(cx, &wake, delay).await;
                        continue;
                    }
                };
                redial.linked(std::time::Instant::now());
                let net::Connected { me, mut ack, sender, mut events, link } = connected;
                pin_grants(&mut ack.caps);
                hold_uploads(&link);
                let link = std::sync::Arc::new(link);
                let screen_link = std::sync::Arc::clone(&link);
                let open_screen: slopty_ui::screen::ScreenFactory =
                    std::sync::Arc::new(move |stream, codec| screen_link.screen(stream, codec));
                let weak_link = std::sync::Arc::downgrade(&link);
                let remote = Some(link.remote());
                let worker_link = WorkerLink { me, out: sender, open_screen, remote };
                let name = ack.name.clone();
                let home = ack.home.clone();
                let sessions = ack.sessions.len();
                let (resume_tx, mut resumes) = tokio::sync::mpsc::unbounded_channel();
                let replaced = std::mem::take(&mut replacing);
                // The slot is checked and the tiles connected in one step: a worker forgotten
                // while the dial was in flight must not come back as a tile holding this link.
                // A link replacing one a probe found dead lets the old one's views go in the same
                // step, so the frame that stops showing them shows this link's.
                let alive = this.update(cx, |ws, cx| {
                    let Some(key) = workers::adopt(&mut ws.workers, id, weak_link, name.clone())
                    else {
                        return false;
                    };
                    if let Some(slot) = ws.workers.iter_mut().find(|w| w.id == id) {
                        slot.resume = Some(resume_tx);
                    }
                    ws.view.update(cx, |v, cx| {
                        if replaced {
                            v.threads_unlinked(key, cx);
                            v.disconnect_worker(key, WorkerStatus::Relinking, cx);
                        }
                        v.connect_worker(key, worker_link, ack, cx);
                        editors::tell_editor_home(key, &name, &home, cx);
                        v.threads_linked(key, cx);
                    });
                    ws.refresh_menu(cx);
                    ws.update_linked(id, cx);
                    true
                });
                if !matches!(alive, Ok(true)) {
                    link.abandon("worker dropped");
                    break;
                }
                // Twice a keep-alive: RTT for the bar and the predictors, and whether the
                // worker is heard (`workers::Hearing`). The bar says "silent" long before the
                // transport's idle timeout, and past the drop bar the link is given up so the
                // redial takes over. A wake while the link is up is the server saying the worker
                // is back or moved: a link that is silent then is the dead one, given up at once.
                // A resume (the Mac woke, the path moved) probes the link at once rather than
                // waiting for its silence: an answer keeps it, none has it dialled again at once
                // while its tiles keep what they show (`relink`).
                let rtt_link = std::sync::Arc::downgrade(&link);
                let rtt_view = view.clone();
                let rtt_wake = std::sync::Arc::clone(&wake);
                let relink = std::sync::Arc::new(tokio::sync::Notify::new());
                let relink_asked = std::sync::Arc::clone(&relink);
                cx.spawn(async move |cx| {
                    let mut hearing = Hearing::new(std::time::Instant::now());
                    // The handshake has measured the path already: the predictors draw from the
                    // first key, not from the first tick 500 ms on.
                    if let Some(link) = rtt_link.upgrade() {
                        let rtt = link.rtt();
                        rtt_view.update(cx, |v, cx| v.set_rtt(key, rtt, cx));
                    }
                    let mut listening = true;
                    loop {
                        let (woken, resumed) = tokio::select! {
                            woken = wait_or_wake(cx, &rtt_wake, workers::HEARING_TICK) => (woken, None),
                            resume = resumes.recv(), if listening => {
                                listening = resume.is_some();
                                (false, resume)
                            }
                        };
                        if let Some(resume) = resumed {
                            // The link ended before the resume reached it: the connect loop is
                            // in its backoff, and the resume says to dial now.
                            let Some(link) = rtt_link.upgrade() else {
                                rtt_wake.notify_one();
                                break;
                            };
                            if probe(cx, &link, resume, &rtt_view, key).await {
                                // Resumes that came while it ran are answered by this one.
                                while resumes.try_recv().is_ok() {}
                                hearing = Hearing::new(std::time::Instant::now());
                                continue;
                            }
                            tracing::warn!(worker = %id, resume = resume.name(), path = %link.path(), "no answer after a resume; relinking");
                            // The pump relinks at once. If the link ended while the probe ran,
                            // the pump is gone and the connect loop waits out its backoff
                            // instead: the wake cuts that short. A wake left over when the
                            // pump took the relink costs at most one dial without backoff; a
                            // new link starts heard, so it is not given up for it.
                            relink_asked.notify_one();
                            rtt_wake.notify_one();
                            break;
                        }
                        let Some(link) = rtt_link.upgrade() else {
                            if woken {
                                // The wake was for the connect loop, which is past this link.
                                rtt_wake.notify_one();
                            }
                            break;
                        };
                        let rtt = link.rtt();
                        let received = link.received_datagrams();
                        tracing::debug!(worker = %id, path = %link.path(), received, "link path");
                        let heard = hearing.sample(received, std::time::Instant::now());
                        let status = match heard.tick(woken) {
                            Tick::Show(status) => status,
                            Tick::Ping(status) => {
                                link.ping();
                                status
                            }
                            Tick::GiveUp { at_once } => {
                                tracing::warn!(worker = %id, ?heard, woken, path = %link.path(), "worker silent; reconnecting");
                                link.abandon("worker silent");
                                if at_once {
                                    rtt_wake.notify_one();
                                }
                                break;
                            }
                        };
                        // Both notify only when what the bar shows changed.
                        rtt_view.update(cx, |v, cx| {
                            v.set_rtt(key, rtt, cx);
                            v.set_worker_status(key, status, cx);
                        });
                    }
                })
                .detach();
                tracing::debug!(worker = %id, sessions, "link up; pumping events");
                // One foreground update per batch, not per event: whatever arrived while the
                // last batch was applied goes in the next one, so a flood from many sessions
                // costs one update (and GPUI draws at most once per display frame anyway).
                loop {
                    let first = tokio::select! {
                        first = events.recv() => first,
                        () = relink.notified() => {
                            replacing = true;
                            None
                        }
                    };
                    let Some(first) = first else { break };
                    let arrived = std::time::Instant::now();
                    let mut batch = Vec::with_capacity(LINK_BATCH);
                    batch.push(first);
                    while batch.len() < LINK_BATCH
                        && let Ok(next) = events.try_recv()
                    {
                        batch.push(next);
                    }
                    let disconnected = cx.update(|cx| {
                        cx.set_global(slopty_ui::terminal::LinkArrival(arrived));
                        batch.into_iter().fold(false, |down, event| {
                            down | apply_link_event(&this, &view, key, event, cx)
                        })
                    });
                    if disconnected {
                        break;
                    }
                }
                // Dropping the link closes the connection; the endpoint stays for the retry.
                if replacing {
                    // Dialled again at once; its tiles stay, set back, until that lands.
                    link.abandon("no answer to a probe after a resume");
                    drop(link);
                    view.update(cx, |v, cx| v.set_worker_status(key, WorkerStatus::Relinking, cx));
                    continue;
                }
                drop(link);
                wait_or_wake(cx, &wake, redial.next(std::time::Instant::now())).await;
            }
        })
        .detach();
    }

    /// Show the panel, connecting to a server or adding a machine; an open panel switches to
    /// `mode` and keeps what was typed, leaving this Mac's checklist if it was up. With no
    /// server set there is nothing to add a machine to, so the panel connects to one first.
    fn show_add_worker(&mut self, mode: Panel, window: &mut Window, cx: &mut Context<Self>) {
        let mode = if self.server.is_none() { Panel::Server } else { mode };
        if let Some(adding) = &mut self.adding {
            let left = adding.this_mac.take().is_some()
                | adding.ssh.take().is_some()
                | adding.asking.take().is_some();
            if adding.mode != mode {
                adding.mode = mode;
                adding.error = None;
                cx.notify();
            } else if left {
                cx.notify();
            }
            Self::focus_panel(adding, window, cx);
            return;
        }
        let address = cx.new(|cx| InputState::new(window, cx).placeholder(SERVER_EXAMPLE));
        let enter = cx.subscribe_in(&address, window, |this, _input, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.enter_address(window, cx);
            }
        });
        // A server's panel looks while no server is set; a machine's always, for the workers
        // the server does not list.
        let search = (LISTS_TAILNET && (mode == Panel::Worker || self.server.is_none()))
            .then_some(Search::Looking);
        let looking = search.is_some();
        self.adding = Some(Adding {
            mode,
            page: self.server.is_none(),
            address,
            busy: false,
            error: None,
            search,
            this_mac: None,
            asking: None,
            ssh: None,
            focus: cx.focus_handle(),
            _enter: enter,
        });
        if let Some(adding) = &self.adding {
            Self::focus_panel(adding, window, cx);
        }
        if looking {
            self.look_on_tailnet(window, cx);
        }
        self.ask_this_mac_worker(cx);
        cx.notify();
    }

    /// Ask this Mac's worker who it is, for "Add a machine" to leave this Mac out once the
    /// server lists it.
    fn ask_this_mac_worker(&self, cx: &Context<Self>) {
        let Some(host) = self.this_mac.clone() else { return };
        let doctor = host.doctor();
        cx.spawn(async move |this, cx| {
            let worker = doctor.await.map(|d| d.worker);
            let _gone = this.update(cx, |ws, cx| {
                if ws.this_mac_worker != worker {
                    ws.this_mac_worker = worker;
                    ws.tell_editor_machines(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The server lists this Mac's worker already.
    fn this_mac_listed_here(&self) -> bool {
        self.this_mac_worker.is_some_and(|id| self.directory.get(id).is_some())
    }

    /// Give the panel the keyboard: the server's address field, or a machine's panel itself.
    fn focus_panel(adding: &Adding, window: &mut Window, cx: &mut Context<Self>) {
        if adding.mode == Panel::Server {
            adding.address.update(cx, |input, cx| input.focus(window, cx));
        } else {
            window.focus(&adding.focus, cx);
        }
    }

    /// "Scan again": look on the tailnet once more, the section saying so meanwhile.
    fn scan_again(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(adding) = &mut self.adding else { return };
        if adding.search.is_none() || adding.search == Some(Search::Looking) {
            return;
        }
        adding.search = Some(Search::Looking);
        self.look_on_tailnet(window, cx);
        cx.notify();
    }

    /// Look for servers and workers on the tailnet, saying so, and offer those that answer.
    /// Connecting stays the person's call.
    fn look_on_tailnet(&self, window: &Window, cx: &Context<Self>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.runtime.spawn(async move {
            let _sent = tx.send(net::find_on_tailnet().await);
        });
        cx.spawn_in(window, async move |this, cx| {
            let found = rx.await.unwrap_or_default();
            let _updated = this.update_in(cx, |ws, window, cx| ws.offer_found(found, window, cx));
        })
        .detach();
    }

    /// The tailnet answered with `found`: each server, and each worker the server does not
    /// list, is a row to press, and on a server's panel the best server has its address in the
    /// field unless the person has moved on (typed something, or connected). Nothing answering
    /// says so. A "Use this Mac" that waited for the look goes on.
    fn offer_found(&mut self, found: net::Tailnet, window: &mut Window, cx: &mut Context<Self>) {
        // A worker the server lists is reached through it already.
        let listed: Vec<std::net::IpAddr> = self
            .directory
            .workers()
            .filter_map(|w| w.address.parse::<std::net::SocketAddr>().ok())
            .map(|a| a.ip())
            .collect();
        let Some(adding) = &mut self.adding else { return };
        if adding.search != Some(Search::Looking) {
            return;
        }
        let servers: Vec<Offer> = found.servers.into_iter().map(Offer::from).collect();
        let workers: Vec<Offer> = found
            .workers
            .into_iter()
            .filter(|w| !listed.contains(&w.addr.ip()))
            .map(Offer::from)
            .collect();
        let best = (adding.mode == Panel::Server)
            .then(|| servers.iter().find(|o| o.answer == slopty_net::discover::Answer::Ready))
            .flatten();
        let empty = adding.address.read(cx).value().trim().is_empty();
        if let Some(best) = best.filter(|_| empty && !adding.busy) {
            let at = best.at.clone();
            adding.address.update(cx, |input, cx| input.set_value(at, window, cx));
        }
        adding.search = Some(Search::Answered { servers, workers, running: found.running });
        let waited =
            adding.this_mac.as_ref().is_some_and(|f| f.server == this_mac::Server::Looking);
        if waited {
            self.use_this_mac(window, cx);
        }
        cx.notify();
    }

    /// What pressing a node the tailnet found does, by what it answered: one that turns this
    /// device away puts the grant that lets it in on the clipboard; a server on another build
    /// opens the server's SSH sheet on it, to put this build there; any other server is
    /// connected to. A worker the server does not list opens the SSH sheet on its host, to put
    /// this build there registered with the server.
    fn press_found(
        &mut self,
        host: Host,
        offer: &Offer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use slopty_net::discover::Answer;
        match (&offer.answer, host) {
            (_, Host::Worker) => self.open_ssh_at(&offer.name, window, cx),
            (Answer::NotGranted, Host::Server) => self.copy_grant_text(offer.grant(), cx),
            (Answer::OtherBuild(_), Host::Server) => {
                self.show_add_worker(Panel::Server, window, cx);
                self.open_ssh_at(&offer.name, window, cx);
            }
            (Answer::Ready, Host::Server) => self.connect_found(&offer.at, window, cx),
        }
    }

    /// Connect to a server the tailnet found at `at`, as typed into the field: a failure is
    /// reported there with its address. Asked for by "Use this Mac", it joins that server.
    fn connect_found(&mut self, at: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(adding) = &mut self.adding else { return };
        if adding.busy {
            return;
        }
        adding.address.update(cx, |input, cx| input.set_value(at.to_owned(), window, cx));
        self.enter_address(window, cx);
    }

    /// Close the panel (only offered while there is somewhere else to be).
    fn cancel_add_worker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.welcome() {
            return;
        }
        self.adding = None;
        self.view.update(cx, |view, cx| view.return_keyboard(window, cx));
        cx.notify();
    }

    /// Put the clipboard's text into the address field and go.
    fn paste_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(adding) = &self.adding else { return };
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
        adding.address.update(cx, |input, cx| input.set_value(text.trim().to_owned(), window, cx));
        self.enter_address(window, cx);
    }

    /// The panel's report: the attempt ended with `error`, or it is still going.
    fn panel_failed(&mut self, error: String, cx: &mut Context<Self>) {
        if let Some(p) = &mut self.adding {
            p.busy = false;
            p.error = Some(error);
        }
        cx.notify();
    }

    /// The address field's Return, or its button: connect to the server it names; while "Use
    /// this Mac" asks, use this Mac with it, an empty field starting one here.
    fn enter_address(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(adding) = &mut self.adding else { return };
        if adding.busy || adding.mode != Panel::Server {
            return;
        }
        let address = adding.address.read(cx).value().trim().to_owned();
        if adding.asking.is_some() {
            let serve = if address.is_empty() {
                this_mac::Serve::Here
            } else {
                match slopty_net::HostAddr::parse_with_port(
                    &address,
                    slopty_net::endpoint::SERVER_PORT,
                ) {
                    Ok(address) => this_mac::Serve::Join(address),
                    Err(e) => return self.panel_failed(e.to_string(), cx),
                }
            };
            adding.asking = None;
            adding.error = None;
            return self.install_this_mac_with(serve, false, window, cx);
        }
        if address.is_empty() {
            return;
        }
        adding.busy = true;
        adding.error = None;
        cx.notify();
        self.connect_from_panel(&address, cx);
    }

    /// Prove the server answers, then save it as `[client] server` and keep its link.
    fn connect_from_panel(&mut self, typed: &str, cx: &mut Context<Self>) {
        let address =
            match slopty_net::HostAddr::parse_with_port(typed, slopty_net::endpoint::SERVER_PORT) {
                Ok(address) => address,
                Err(e) => return self.panel_failed(e.to_string(), cx),
            };
        let (tx, rx) = tokio::sync::oneshot::channel();
        let dial = address.clone();
        self.runtime.spawn(async move {
            let _sent = tx.send(net::link_server(&dial).await);
        });
        cx.spawn(async move |this, cx| {
            let outcome = rx.await;
            let _updated = this.update(cx, |ws, cx| match outcome {
                Ok(Ok(link)) => {
                    let name = link.name.clone();
                    if let Err(e) = ws.save_server(Some(&address)) {
                        link.close();
                        return ws.panel_failed(e, cx);
                    }
                    if let Some(deployer) = &ws.deployer {
                        deployer.register_here(ws.server_address(), &address);
                    }
                    ws.adding = None;
                    ws.show_notice(format!("Connected to {name}"), cx);
                    ws.set_server(Some(address), Some(link), cx);
                    ws.refresh_menu(cx);
                    cx.notify();
                }
                Ok(Err(e)) => ws.panel_failed(format!("{e:#}"), cx),
                Err(_dropped) => ws.panel_failed("connection task died".to_owned(), cx),
            });
        })
        .detach();
    }

    /// "Use this Mac", from the panel's row or a failed line's "Try again": decide the
    /// server, install, then read the worker's `doctor` as the panel's checklist. A run already
    /// looking, installing or waiting for the directory is left to finish.
    fn use_this_mac(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.install_this_mac(false, window, cx);
    }

    /// The server "Use this Mac" registers this Mac with, as far as it is decided: the one this
    /// app or this Mac's worker is set to, else the one Ready server the tailnet answered with,
    /// else one started here. `Err` while the tailnet is still being looked on, or when the
    /// person must say: several answered, or no look could be made.
    fn this_mac_serve(&self) -> Result<this_mac::Serve, Option<Asking>> {
        let set = self.server_address().or(self.settings.worker.server.as_ref());
        if let Some(server) = set {
            return Ok(this_mac::Serve::Join(server.clone()));
        }
        let search = self.adding.as_ref().and_then(|a| a.search.as_ref());
        match search {
            Some(Search::Looking) => Err(None),
            Some(search) => match search.ready_servers().as_deref() {
                Some([]) => Ok(this_mac::Serve::Here),
                // A tailnet IP always parses; were it not to, the person says which.
                Some([one]) => slopty_net::HostAddr::parse_with_port(
                    &one.at,
                    slopty_net::endpoint::SERVER_PORT,
                )
                .ok()
                .map(this_mac::Serve::Join)
                .ok_or(Some(Asking::Several)),
                Some(_) => Err(Some(Asking::Several)),
                None => Err(Some(Asking::Blind)),
            },
            None => Err(Some(Asking::Blind)),
        }
    }

    /// [`Self::use_this_mac`], going on where it ends sessions when `end_sessions`: the
    /// person's "Update anyway" after the last try said so.
    fn install_this_mac(
        &mut self,
        end_sessions: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.this_mac.is_none() {
            return;
        }
        if self.adding.is_none() {
            self.show_add_worker(Panel::Worker, window, cx);
        }
        let decided = self.this_mac_serve();
        let Some(adding) = &mut self.adding else { return };
        if adding.this_mac.as_ref().is_some_and(this_mac::Flow::busy)
            && adding.this_mac.as_ref().is_none_or(|f| f.server != this_mac::Server::Looking)
        {
            return;
        }
        adding.ssh = None;
        match decided {
            Ok(serve) => self.install_this_mac_with(serve, end_sessions, window, cx),
            Err(None) => {
                self.this_mac_runs = self.this_mac_runs.wrapping_add(1);
                adding.asking = None;
                adding.this_mac = Some(this_mac::Flow::looking(self.this_mac_runs));
                cx.notify();
            }
            Err(Some(why)) => {
                adding.this_mac = None;
                adding.asking = Some(why);
                adding.mode = Panel::Server;
                adding.error = None;
                adding.address.update(cx, |input, cx| input.focus(window, cx));
                cx.notify();
            }
        }
    }

    /// Install this Mac against `serve`: the server first when it starts here, then the worker.
    fn install_this_mac_with(
        &mut self,
        serve: this_mac::Serve,
        end_sessions: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(host) = self.this_mac.clone() else { return };
        let Some(adding) = &mut self.adding else { return };
        self.this_mac_runs = self.this_mac_runs.wrapping_add(1);
        let run = self.this_mac_runs;
        let install = host.install(&serve, end_sessions);
        adding.ssh = None;
        adding.asking = None;
        adding.this_mac = Some(this_mac::Flow::installing(run, serve));
        cx.spawn_in(window, async move |this, cx| {
            let outcome = install.await;
            let _gone = this
                .update_in(cx, |ws, window, cx| ws.this_mac_installed(run, outcome, window, cx));
        })
        .detach();
        self.read_this_app(run, cx);
        cx.notify();
    }

    /// Read the app's own lines of run `run`'s checklist: its notifications, whether it opens at
    /// login and its place in Finder.
    fn read_this_app(&self, run: u64, cx: &Context<Self>) {
        let Some(host) = self.this_mac.clone() else { return };
        let (notes, login, finder) = (host.notes(), host.login(), host.finder());
        cx.spawn(async move |this, cx| {
            let (notes, login, finder) = (notes.await, login.await, finder.await);
            let _gone = this.update(cx, |ws, cx| {
                if let Some(flow) = ws.this_mac_flow(run) {
                    flow.notes = Some(notes);
                    flow.login = Some(login);
                    flow.finder = Some(finder);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Back from this Mac's checklist to the panel it was opened from.
    fn leave_this_mac(&mut self, cx: &mut Context<Self>) {
        if let Some(adding) = &mut self.adding {
            adding.this_mac = None;
            adding.asking = None;
        }
        cx.notify();
    }

    /// Run `run` of this Mac's flow, if it is still the one on screen.
    fn this_mac_flow(&mut self, run: u64) -> Option<&mut this_mac::Flow> {
        self.adding.as_mut()?.this_mac.as_mut().filter(|flow| flow.run == run)
    }

    /// The install ended: this app follows the server the worker registers with, and the
    /// worker is asked how it stands; or the checklist says why it failed.
    fn this_mac_installed(
        &mut self,
        run: u64,
        outcome: Result<slopty_net::HostAddr, this_mac::Stopped>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(flow) = self.this_mac_flow(run) else { return };
        match outcome {
            Ok(server) => {
                flow.worker = this_mac::Worker::Starting;
                if self.server_address() != Some(&server) {
                    if let Err(e) = self.save_server(Some(&server)) {
                        self.show_failure(format!("Settings: {e}"), cx);
                    }
                    self.set_server(Some(server), None, cx);
                    self.refresh_menu(cx);
                }
                self.read_this_mac(run, false, window, cx);
            }
            Err(this_mac::Stopped::Failed(why)) => flow.worker = this_mac::Worker::Failed(why),
            Err(this_mac::Stopped::EndsSessions(plan)) => {
                flow.worker = this_mac::Worker::EndsSessions(plan);
            }
            Err(this_mac::Stopped::Misplaced) => flow.worker = this_mac::Worker::Misplaced,
            Err(this_mac::Stopped::Server(why)) => flow.server = this_mac::Server::Failed(why),
        }
        cx.notify();
    }

    /// Ask the worker's `doctor` until it answers or [`this_mac::ATTEMPTS`] run out, starting
    /// it again first when `restart`.
    fn read_this_mac(&mut self, run: u64, restart: bool, window: &Window, cx: &Context<Self>) {
        let Some(host) = self.this_mac.clone() else { return };
        let Some(flow) = self.this_mac_flow(run) else { return };
        flow.reading = true;
        cx.spawn_in(window, async move |this, cx| {
            if restart {
                host.restart().await;
            }
            let mut doctor = None;
            for attempt in 0..this_mac::ATTEMPTS {
                if attempt > 0 {
                    cx.background_executor().timer(this_mac::RETRY).await;
                }
                doctor = host.doctor().await;
                if doctor.is_some() {
                    break;
                }
            }
            let _gone =
                this.update_in(cx, |ws, window, cx| ws.this_mac_read(run, doctor, window, cx));
        })
        .detach();
    }

    /// The worker answered with `doctor` (or never did): the checklist shows it, and once it
    /// may stream and take input the flow waits for the server to list it.
    fn this_mac_read(
        &mut self,
        run: u64,
        doctor: Option<this_mac::Doctor>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(flow) = self.this_mac_flow(run) else { return };
        flow.reading = false;
        flow.worker = doctor.map_or(this_mac::Worker::Silent, this_mac::Worker::Up);
        if flow.worker.ready() && !flow.listing {
            self.wait_this_mac_listed(run, window, cx);
        }
        cx.notify();
    }

    /// Look, at most [`this_mac::LISTED_TRIES`] times, for the server's directory to list this
    /// Mac's worker; while it does not yet, the worker's own word on its link is read again, so
    /// the Server line says why.
    fn wait_this_mac_listed(&mut self, run: u64, window: &Window, cx: &Context<Self>) {
        let Some(host) = self.this_mac.clone() else { return };
        let Some(flow) = self.this_mac_flow(run) else { return };
        let this_mac::Worker::Up(doctor) = &flow.worker else { return };
        let worker = doctor.worker;
        flow.listing = true;
        flow.error = None;
        cx.spawn_in(window, async move |this, cx| {
            for attempt in 0..this_mac::LISTED_TRIES {
                if attempt > 0 {
                    cx.background_executor().timer(this_mac::RETRY).await;
                    if let Some(doctor) = host.doctor().await {
                        let _gone = this.update(cx, |ws, cx| {
                            if let Some(flow) = ws.this_mac_flow(run) {
                                flow.worker = this_mac::Worker::Up(doctor);
                                cx.notify();
                            }
                        });
                    }
                }
                match this.update(cx, |ws, _cx| ws.directory.get(worker).is_some()) {
                    Ok(true) => {
                        let _gone = this.update(cx, |ws, cx| ws.this_mac_listed(run, cx));
                        return;
                    }
                    Ok(false) => {}
                    Err(_gone) => return,
                }
            }
            let _gone = this.update(cx, |ws, cx| ws.this_mac_unlisted(run, cx));
        })
        .detach();
    }

    /// The directory never listed this Mac: the checklist says so, with the worker's log.
    fn this_mac_unlisted(&mut self, run: u64, cx: &mut Context<Self>) {
        let logs =
            slopty_platform::service::Session::native().logs(slopty_platform::service::WORKER);
        let Some(flow) = self.this_mac_flow(run) else { return };
        flow.listing = false;
        flow.error = Some(format!("{} Its log is {logs}", this_mac::NOT_LISTED));
        cx.notify();
    }

    /// The server lists this Mac: the panel closes onto its workspace, with a word when the
    /// tailnet does not reach it yet; or, while the app's own lines still have something for
    /// the person, it stays open on them, ready, until they are done.
    ///
    /// Slopty opens at login from then on, so it is up to say when an agent needs the person
    /// after a restart; unless the person turned that off in System Settings, which it leaves.
    fn this_mac_listed(&mut self, run: u64, cx: &mut Context<Self>) {
        let Some(flow) = self.this_mac_flow(run) else { return };
        flow.listing = false;
        flow.listed = true;
        if let this_mac::Worker::Up(d) = &flow.worker {
            self.this_mac_worker = Some(d.worker);
        }
        self.open_at_login(run, true, cx);
        cx.notify();
    }

    /// "Done" at the end of this Mac's flow: the panel gives way to the workspace.
    fn close_this_mac(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.adding = None;
        self.view.update(cx, |view, cx| view.return_keyboard(window, cx));
        cx.notify();
    }

    /// A phone or iPad's way in, from the add panel: the panel gives way to the code.
    fn connect_device_from_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.adding = None;
        self.show_invite(window, cx);
    }

    /// A missing line's button: its pane of System Settings, or the install again.
    fn this_mac_fix(&mut self, fix: this_mac::Fix, window: &mut Window, cx: &mut Context<Self>) {
        match fix {
            this_mac::Fix::Open(place) => {
                if let Some(host) = &self.this_mac {
                    host.open(place);
                }
            }
            this_mac::Fix::AllowNotes => self.allow_notes(cx),
            this_mac::Fix::OpenAtLogin => {
                let run = self.adding.as_ref().and_then(|a| a.this_mac.as_ref()).map(|f| f.run);
                if let Some(run) = run {
                    self.open_at_login(run, false, cx);
                }
            }
            this_mac::Fix::Retry => self.use_this_mac(window, cx),
            this_mac::Fix::EndSessions => self.install_this_mac(true, window, cx),
            this_mac::Fix::MoveToApplications => self.move_to_applications(window, cx),
        }
    }

    /// Open Slopty at login, and say on run `run`'s line how it stands after; `if_off` only when
    /// it is off, so a login item the person turned off in System Settings stays off.
    fn open_at_login(&self, run: u64, if_off: bool, cx: &Context<Self>) {
        let Some(host) = self.this_mac.clone() else { return };
        cx.spawn(async move |this, cx| {
            let was = if if_off { host.login().await } else { this_mac::Login::Off };
            let now = match was {
                this_mac::Login::Off => host.open_at_login().await,
                other => Ok(other),
            };
            let _gone = this.update(cx, |ws, cx| {
                let now = now.unwrap_or_else(|why| {
                    ws.show_notice(format!("Slopty could not open at login: {why}"), cx);
                    this_mac::Login::Off
                });
                if let Some(flow) = ws.this_mac_flow(run) {
                    flow.login = Some(now);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The Notifications line's "Allow": the system asks the person, and the line says how they
    /// answered.
    fn allow_notes(&self, cx: &Context<Self>) {
        let Some(host) = self.this_mac.clone() else { return };
        let Some(run) = self.adding.as_ref().and_then(|a| a.this_mac.as_ref()).map(|f| f.run)
        else {
            return;
        };
        let asked = host.ask_notes();
        cx.spawn(async move |this, cx| {
            let notes = asked.await;
            let _gone = this.update(cx, |ws, cx| {
                if let Some(flow) = ws.this_mac_flow(run) {
                    flow.notes = Some(notes);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Copy Slopty into Applications and open it from there, this copy quitting: the checklist
    /// goes on in the new one.
    fn move_to_applications(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(host) = self.this_mac.clone() else { return };
        let Some(run) = self.adding.as_ref().and_then(|a| a.this_mac.as_ref()).map(|f| f.run)
        else {
            return;
        };
        if let Some(flow) = self.this_mac_flow(run) {
            flow.worker = this_mac::Worker::Installing;
        }
        let moved = host.move_to_applications();
        cx.spawn_in(window, async move |this, cx| {
            let moved = moved.await;
            let _gone = this.update(cx, |ws, cx| match moved {
                Ok(to) => {
                    tracing::info!(to = %to.display(), "moved to Applications; opening it there");
                    cx.set_restart_path(to);
                    cx.restart();
                }
                Err(why) => {
                    if let Some(flow) = ws.this_mac_flow(run) {
                        flow.worker =
                            this_mac::Worker::Failed(format!("Could not move Slopty: {why}"));
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// At launch: this Mac's worker services run a copy of Slopty that is gone (it was moved,
    /// or ran from a download a restart took away), so they are pointed at this one.
    fn repoint_this_mac(&self, cx: &Context<Self>) {
        let Some(host) = self.this_mac.clone() else { return };
        let repointed = host.repoint();
        cx.spawn(async move |this, cx| {
            let Some(done) = repointed.await else { return };
            let said = match done {
                Ok(()) => "This Mac shares from this copy of Slopty now".to_owned(),
                Err(why) => why,
            };
            let _gone = this.update(cx, |ws, cx| ws.show_notice(said, cx));
        })
        .detach();
    }

    /// The app is in front again, perhaps from System Settings: the app's own lines are read
    /// again, and a worker short of ready is asked again, started again first when Screen
    /// Recording was what it lacked.
    fn this_mac_activated(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(flow) = self.adding.as_ref().and_then(|a| a.this_mac.as_ref()) else { return };
        self.read_this_app(flow.run, cx);
        if !flow.rereads() {
            return;
        }
        let (run, restart) = (flow.run, flow.restarts());
        self.read_this_mac(run, restart, window, cx);
        cx.notify();
    }

    /// Keep the person's word on sharing the clipboard with `worker` in the settings file, by
    /// its name; what could not be written is said, and the workspace keeps it until the app
    /// quits.
    fn keep_clipboard_shared(&mut self, worker: WorkerKey, share: bool, cx: &mut Context<Self>) {
        let Some(name) = self.workers.iter().find(|w| w.key == worker).map(|w| w.name.clone())
        else {
            return;
        };
        let text = settings::editable_text(&self.settings_path);
        let kept = settings::with_clipboard_shared(&text, &name, share)
            .and_then(|text| settings::save(&self.settings_path, &text, &mut self.settings_seen));
        match kept {
            Ok(loaded) => self.settings = loaded.settings,
            Err(e) => self.show_failure(format!("Settings: {e}"), cx),
        }
    }

    /// Write `[client] server` into `settings.toml`, the rest of the file untouched, and take
    /// it as loaded so the watcher sees nothing new.
    fn save_server(&mut self, server: Option<&slopty_net::HostAddr>) -> Result<(), String> {
        let text = settings::editable_text(&self.settings_path);
        let text = slopty_settings::with_server(&text, slopty_settings::ServerOf::Client, server)?;
        let loaded = settings::save(&self.settings_path, &text, &mut self.settings_seen)?;
        self.settings = loaded.settings;
        Ok(())
    }

    /// Whether the panel is the whole window: the first run's page, with nothing to go back
    /// to, so no workspace behind it and no way to dismiss it.
    fn welcome(&self) -> bool {
        self.adding.as_ref().is_some_and(|a| a.page)
    }

    /// The way in: the app's name, a heading and a line on what it is, what the tailnet
    /// answered as a list to press, the address with its one primary action, and the other ways
    /// in as quiet links: the other panel, and on a Mac "Use this Mac", whose
    /// checklist then stands in for the list and the address.
    ///
    /// On the first run it is the page, a third of the way down the content surface over a foot
    /// that says why there is nothing to pair. Later ("Add a worker…", "Connect to a server…")
    /// it is a dialog over the workspace, closed by Cancel, Esc or a click outside it. From
    /// [`FIELD_ROW_FROM`] wide the field and its action share a row; narrower, the action is a
    /// thumb's full width under it. On touch the field ends in a Paste, since glass has no ⌘V,
    /// and where the tailnet cannot be listed a line says so.
    fn add_worker_panel(
        &self,
        adding: &Adding,
        window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let (spacing, radii, ty) = (theme.spacing, theme.radii, &theme.typography);
        let button = |id, text, kind| kit::button(theme, id, text, kind);
        // Each blurb fits one line of a 402 pt phone.
        let (title, blurb) = match adding.mode {
            Panel::Server => {
                ("Connect to a server", "A server on your tailnet or VPN lists your machines.")
            }
            Panel::Worker => ("Add a machine", "A Mac or Linux machine on your tailnet or VPN."),
        };
        let flow = adding.this_mac.as_ref();
        let sheet = adding.ssh.as_ref();
        let asking = adding.asking;
        let in_flow = flow.is_some() || sheet.is_some() || asking.is_some();
        // A machine's panel with no way to add one here: no Mac to share, no SSH and no tailnet
        // to list. It says where machines are added instead of offering an address it has no
        // field for.
        let from_a_mac = adding.mode == Panel::Worker
            && !in_flow
            && self.this_mac.is_none()
            && self.deployer.is_none()
            && adding.search.is_none();
        let blurb = if from_a_mac { FROM_A_MAC } else { blurb };
        // The checklist, the SSH sheet and the question have headings of their own, and their
        // link goes back to the panel they came from.
        let back = match adding.mode {
            Panel::Server => "Connect to a server instead",
            Panel::Worker => "Add a machine another way",
        };
        let welcome = self.welcome();
        // The first run asks where to work before anything about servers: this Mac, or a
        // server that already lists machines (`docs/decisions/ui.md`, "The first run
        // asks where to work"). A device with no Mac to share has one way, the server.
        let choosing =
            welcome && adding.mode == Panel::Server && !in_flow && self.this_mac.is_some();
        let (title, blurb) = match (flow, sheet, asking) {
            (Some(_), ..) => (this_mac::TITLE, this_mac::BLURB),
            (None, Some(sheet), _) => (sheet.heading(), sheet.blurb()),
            (None, None, Some(asking)) => (this_mac::TITLE, asking.words()),
            (None, None, None) if choosing => (CHOOSE_TITLE, CHOOSE_BLURB),
            (None, None, None) => (title, blurb),
        };
        // The page leads with the app's mark over its heading, as Raycast's and Linear's first
        // screens do; a dialog over the workspace needs no sign.
        let brand = welcome.then(|| kit::brand(theme, None));
        let roles = theme.roles();
        let heading = div()
            .id("add-worker-title")
            .debug_selector(|| "add-worker-title".to_owned())
            .role(Role::Heading)
            .aria_label(title)
            .map(|el| {
                if welcome {
                    kit::typed(el, roles.first_run, 1.0)
                } else {
                    el.text_size(px(ty.title()))
                        .font_weight(gpui::FontWeight(Typography::STRONG_WEIGHT))
                }
            })
            .text_color(hsla(s.text))
            .child(title);
        let intro = div().flex().flex_col().gap(px(spacing.xs)).child(heading).child(
            kit::typed(div(), roles.chrome, 1.0)
                .id("add-worker-blurb")
                .debug_selector(|| "add-worker-blurb".to_owned())
                .text_color(hsla(s.text_secondary))
                .child(blurb),
        );
        let tailnet =
            adding.search.as_ref().map(|search| self.tailnet_list(search, adding.mode, cx));
        let field_label = address_label(adding.search.as_ref());
        let unlisted = (!LISTS_TAILNET && adding.mode == Panel::Server).then(|| {
            kit::meta(div(), theme)
                .debug_selector(|| "add-worker-unlisted".to_owned())
                .child(UNLISTED)
        });
        let status = match (&adding.error, adding.busy) {
            (Some(e), _) => Some((e.clone(), s.error)),
            (None, true) => Some(("Connecting…".to_owned(), s.text_muted)),
            (None, false) => None,
        };
        let safe = window.insets().effective();
        let room = f32::from(self.frame_size(window).width - safe.left - safe.right);
        let stacked = room < FIELD_ROW_FROM;
        let go = if asking.is_some() { "Continue" } else { "Connect" };
        let go = button("add", go, ButtonKind::Primary)
            .h(px(FIELD_H))
            .when(stacked, gpui::Styled::w_full)
            .on_click(cx.listener(|this, _ev, window, cx| this.enter_address(window, cx)));
        let paste = TOUCH.then(|| {
            button("paste-address", "Paste", ButtonKind::Link)
                .on_click(cx.listener(|this, _ev, window, cx| this.paste_address(window, cx)))
        });
        // A field is a well in the page, not a box drawn round one: the raised fill, no
        // hairline, and its caret the only sign of focus (T3 Code, Geist).
        let address = div()
            .id("add-worker-field")
            .debug_selector(|| "add-worker-field".to_owned())
            .flex_1()
            .min_w_0()
            .h(px(FIELD_H))
            .flex()
            .items_center()
            // The field's own pad and this put its text on the tailnet rows' glyphs.
            .pl(px(spacing.xs))
            .rounded(px(radii.sm))
            .map(|el| kit::field(el, theme))
            .text_size(px(ty.ui_size))
            .child(
                Input::new(&adding.address)
                    .appearance(false)
                    .aria_label(SERVER_FIELD)
                    .when_some(paste, Input::suffix),
            );
        let entry = if stacked {
            div().flex().flex_col().gap(px(spacing.sm)).child(address.w_full()).child(go)
        } else {
            div().flex().items_center().gap(px(spacing.sm)).child(address).child(go)
        };
        let entry = div()
            .flex()
            .flex_col()
            .gap(px(spacing.sm))
            .child(panel_label(
                theme,
                "add-worker-label",
                if asking.is_some() { SERVER_FIELD } else { field_label },
            ))
            .child(entry)
            .when_some(status, |el, (text, tone)| {
                el.child(
                    kit::meta(div(), theme)
                        .id("add-worker-status")
                        .role(Role::Status)
                        .aria_label(SharedString::from(text.clone()))
                        .text_color(hsla(tone))
                        .child(SharedString::from(text)),
                )
            });
        // At the body's size, as Cancel beside it is: one size for what can be pressed. Only a
        // flow has somewhere to go back to.
        let switch = in_flow.then(|| {
            button("panel-switch", back, ButtonKind::Link).on_click(cx.listener(
                |this, _ev, _window, cx| {
                    this.leave_this_mac(cx);
                    this.leave_ssh(cx);
                },
            ))
        });
        // This Mac and a machine over SSH are rows to press as the tailnet's are, in one frame,
        // not links under the field. This Mac is the likeliest first step with nothing set up
        // yet, so it leads: on the server's panel it runs or joins the server too, which then
        // offers the servers found, the address and a server over SSH; on a machine's panel it
        // shares a worker over SSH beside it.
        let serving = adding.mode == Panel::Server;
        // Listed already, the row stays: it is the way back to this Mac's checklist.
        let this_mac_entry = (self.this_mac.is_some() && !in_flow).then(|| {
            this_mac_row(theme, self.this_mac_listed_here(), choosing)
                .on_click(cx.listener(|this, _ev, window, cx| this.use_this_mac(window, cx)))
        });
        let ssh_entry = (!in_flow).then(|| self.ssh_row(cx)).flatten();
        // The first run's page sets its groups 24 pt apart and their parts 12 pt; a dialog
        // keeps to its own tighter steps.
        let (apart, within) =
            if welcome { (spacing.xl, spacing.md) } else { (spacing.lg, spacing.sm) };
        let group =
            |id: &'static str, label_id: &'static str, label: &'static str, rows: Vec<_>| {
                (!rows.is_empty()).then(|| {
                    let frame =
                        kit::card(theme).flex().flex_col().p(px(spacing.xxs)).children(rows);
                    div()
                        .id(id)
                        .flex()
                        .flex_col()
                        .gap(px(within))
                        .child(panel_label(theme, label_id, label))
                        .child(frame)
                })
            };
        let (use_this_mac, set_up_server) = if choosing {
            // The row is the choice, its words its own: no label over it.
            let mac = this_mac_entry.map(|row| {
                div()
                    .id("add-worker-this-mac")
                    .child(kit::card(theme).flex().flex_col().p(px(spacing.xxs)).child(row))
            });
            (
                mac,
                group(
                    "add-worker-server",
                    "add-worker-server-label",
                    SET_UP_SERVER_LABEL,
                    ssh_entry.into_iter().collect(),
                ),
            )
        } else if serving {
            (
                group(
                    "add-worker-this-mac",
                    "add-worker-this-mac-label",
                    THIS_MAC_LABEL,
                    this_mac_entry.into_iter().collect(),
                ),
                group(
                    "add-worker-server",
                    "add-worker-server-label",
                    SET_UP_SERVER_LABEL,
                    ssh_entry.into_iter().collect(),
                ),
            )
        } else {
            let label = if ssh_entry.is_some() { SET_UP_LABEL } else { THIS_MAC_LABEL };
            let rows = this_mac_entry.into_iter().chain(ssh_entry).collect();
            (group("add-worker-this-mac", "add-worker-this-mac-label", label, rows), None)
        };
        let cancel = (!welcome).then(|| {
            button("cancel-add", "Cancel", ButtonKind::Ghost)
                .on_click(cx.listener(|this, _ev, window, cx| this.cancel_add_worker(window, cx)))
        });
        let ways = div().flex().flex_col().items_start().gap(px(spacing.xs)).children(switch);
        let aside = div().flex().items_start().child(ways).child(div().flex_1()).children(cancel);
        // The question "Use this Mac" asks is the servers found and the address alone.
        let mut sections: Vec<gpui::AnyElement> = Vec::new();
        if asking.is_some() {
            sections.extend(tailnet);
            sections.push(entry.into_any_element());
        } else if choosing {
            // The other choice: a server that lists machines already, what it is in one line,
            // then the servers the tailnet answered with and the address. What the link needs
            // is the list's own line and the page's foot.
            let head = div()
                .flex()
                .flex_col()
                .child(
                    kit::typed(div(), roles.action, 1.0)
                        .id("add-worker-connect-title")
                        .role(Role::Heading)
                        .aria_label(CONNECT_TITLE)
                        .text_color(hsla(s.text))
                        .child(CONNECT_TITLE),
                )
                .child(
                    kit::typed(div(), roles.chrome, 1.0)
                        .text_color(hsla(s.text_secondary))
                        .child(CONNECT_LINE),
                );
            let connect = div()
                .id("add-worker-connect")
                .debug_selector(|| "add-worker-connect".to_owned())
                .flex()
                .flex_col()
                .gap(px(within))
                .child(head)
                .children(tailnet)
                .children(unlisted)
                .child(entry);
            sections.extend(use_this_mac.map(IntoElement::into_any_element));
            sections.push(connect.into_any_element());
            sections.extend(set_up_server.map(IntoElement::into_any_element));
        } else if !in_flow {
            sections.extend(use_this_mac.map(IntoElement::into_any_element));
            sections.extend(tailnet);
            sections.extend(unlisted.map(IntoElement::into_any_element));
            if serving {
                sections.push(entry.into_any_element());
            }
            sections.extend(set_up_server.map(IntoElement::into_any_element));
        }
        // A phone or iPad joins the server, not as a machine, but this is where a person
        // looks for a way to bring one in.
        let phone = (!in_flow && !serving).then(|| {
            phone_row(theme).on_click(
                cx.listener(|this, _ev, window, cx| this.connect_device_from_panel(window, cx)),
            )
        });
        let phone = group(
            "add-worker-phone",
            "add-worker-phone-label",
            PHONE_LABEL,
            phone.into_iter().collect(),
        );
        if from_a_mac {
            let how = kit::meta(div(), theme)
                .debug_selector(|| "add-worker-from-a-mac".to_owned())
                .child(FROM_A_MAC_HOW);
            let other = entry_row(
                theme,
                "connect-another-server",
                slopty_ui::icons::Symbol::Link,
                "Connect to another server",
                "One that lists other machines",
            )
            .on_click(cx.listener(|this, _ev, window, cx| {
                this.show_add_worker(Panel::Server, window, cx);
            }));
            let rows = kit::card(theme).flex().flex_col().p(px(spacing.xxs)).child(other);
            sections.push(how.into_any_element());
            sections.push(rows.into_any_element());
        }
        sections.extend(phone.map(IntoElement::into_any_element));
        let checklist = flow.map(|flow| self.this_mac_checklist(flow, cx));
        let ssh_sheet = sheet.map(|sheet| self.ssh_sheet(sheet, cx));
        let panel = div()
            .id("add-worker")
            .debug_selector(|| "add-worker".to_owned())
            .track_focus(&adding.focus)
            .occlude()
            // It gives up height to a short window rather than run past it: its body scrolls,
            // and its intro and its way back stay in view.
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(apart))
            .w(px(ADD_PANEL_W))
            .max_w_full()
            .font_family(ty.ui_family.clone())
            .when(!welcome, |el| {
                kit::elevate(el, theme)
                    .p(px(spacing.xl))
                    .mb(px(spacing.xl))
                    .rounded(px(radii.lg))
                    .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
                    .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                        if this.adding.as_ref().is_some_and(|a| a.composing(cx)) {
                            return;
                        }
                        this.cancel_add_worker(window, cx);
                        cx.stop_propagation();
                    }))
                    // A machine's panel has no field whose Esc this hears: its own focus does.
                    .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                        let own = this.adding.as_ref().is_some_and(|a| a.focus.is_focused(window));
                        if own && ev.keystroke.key == "escape" {
                            this.cancel_add_worker(window, cx);
                            cx.stop_propagation();
                        }
                    }))
            })
            // The intro stands a step further from what follows than the sections do apart.
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(spacing.lg))
                    .mb(px(spacing.sm))
                    .children(brand)
                    .child(intro),
            )
            // Where the tailnet cannot be listed, the line saying so stands in the list's place;
            // this Mac's checklist stands in for both and the address.
            .child(
                div()
                    .id("add-worker-body")
                    .flex()
                    .flex_col()
                    .gap(px(apart))
                    .min_h_0()
                    .overflow_y_scroll()
                    .when_some(checklist, gpui::ParentElement::child)
                    .when_some(ssh_sheet, gpui::ParentElement::child)
                    .children(sections),
            )
            .child(aside);
        if !welcome {
            // The scrim dims in as the dialog fades in where it stands (the palette and the
            // keyboard summon it, so it does not travel), both on the overlay's pace, and under
            // Reduce Motion both are there at once.
            let panel = kit::fade_in(panel, "add-worker-in", cx);
            let backdrop = kit::backdrop(theme, window)
                .id("add-worker-backdrop")
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev, window, cx| {
                        this.cancel_add_worker(window, cx);
                        cx.stop_propagation();
                    }),
                )
                .child(panel);
            if !kit::motion(cx) {
                return backdrop.into_any_element();
            }
            let dim = kit::scrim(theme);
            return backdrop
                .with_animation("add-worker-scrim", kit::Pace::Fade.animation(), move |el, t| {
                    el.bg(gpui::Hsla { a: dim.a * t, ..dim })
                })
                .into_any_element();
        }
        // Not a dialog over an app that does nothing yet: the page itself, on the content
        // surface the tiles' bodies take. Its block sits a third of the way down the room over
        // the foot, where the eye starts; spacers rather than a percentage pad, which would
        // resolve against the width. The foot stays on the bottom safe edge, as on the Mac; only
        // the block's room gives way to a keyboard, so the block rises with it and the note stays
        // put under the keys rather than floating mid-screen.
        let insets = window.insets();
        let keyboard = (insets.ime.bottom - insets.safe_area.bottom).max(px(0.0));
        let foot = div()
            .id("add-worker-foot")
            .debug_selector(|| "add-worker-foot".to_owned())
            .flex_none()
            .w(px(ADD_PANEL_W))
            .max_w_full()
            .pb(px(spacing.lg))
            .child(kit::meta(div(), theme).child(TAILNET_NOTE));
        div()
            .id("welcome")
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(safe.top)
            .pb(insets.safe_area.bottom)
            // A phone on its side has the island at one end: the page keeps clear of it.
            .pl(safe.left + px(spacing.lg))
            .pr(safe.right + px(spacing.lg))
            .bg(hsla(theme.content()))
            .font_family(ty.ui_family.clone())
            .child(
                div()
                    .id("welcome-room")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .pb(keyboard)
                    .child(div().flex_grow(1.0))
                    .child(panel)
                    .child(div().flex_grow(2.0)),
            )
            .child(foot)
            .into_any_element()
    }

    /// What the tailnet answered: each server and worker a row to press, in one list a tone step
    /// off the page, under a label. While it looks, or when nothing answered, one quiet line
    /// says so and what to do, with no step and no label (the line names the tailnet itself):
    /// only what can be chosen stands on a step, and a status on one with a magnifier read as
    /// a search field to type in.
    fn tailnet_list(&self, search: &Search, mode: Panel, cx: &Context<Self>) -> gpui::AnyElement {
        use slopty_ui::icons::{IconSize, Status, Symbol, icon, status_icon};
        let theme = &self.theme;
        let (s, spacing, ty) = (theme.surfaces, theme.spacing, &theme.typography);
        let section = div().id("add-worker-tailnet").flex().flex_col().gap(px(spacing.sm));
        let label = panel_label(theme, "add-worker-tailnet-label", ON_TAILNET);
        // Nothing to offer: the section still stands, its one row saying what the scan found
        // and offering to look again, a line under it on what to do about it.
        if let Some(words) = search.words(mode) {
            let size = px(ty.small());
            let mark = match search {
                Search::Looking => {
                    status_icon(theme, Status::Running, size, hsla(s.text_muted)).into_any_element()
                }
                Search::Answered { running: false, .. } => {
                    icon(theme, Symbol::WifiSlash, IconSize::Inline, hsla(s.text_muted))
                        .size(size)
                        .into_any_element()
                }
                Search::Answered { .. } => {
                    icon(theme, Symbol::Magnifyingglass, IconSize::Inline, hsla(s.text_muted))
                        .size(size)
                        .into_any_element()
                }
            };
            let step = search.next_step(mode);
            let again = (*search != Search::Looking).then(|| {
                kit::button(theme, "add-worker-scan", SCAN_AGAIN, ButtonKind::Secondary)
                    .on_click(cx.listener(|this, _ev, window, cx| this.scan_again(window, cx)))
            });
            let row = kit::row(theme, kit::Row::One)
                .id("add-worker-search")
                .debug_selector(|| "add-worker-search".to_owned())
                .role(Role::Status)
                .aria_label(words)
                .when_some(step, gpui::StatefulInteractiveElement::aria_description)
                .gap(px(spacing.sm))
                .child(div().flex_none().child(mark))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(ty.ui_size))
                        .text_color(hsla(s.text_secondary))
                        .child(words),
                )
                .children(again);
            let frame = kit::card(theme).flex().flex_col().p(px(spacing.xxs)).child(row);
            let step = step.map(|step| kit::meta(div(), theme).child(step));
            return section.child(label).child(frame).children(step).into_any_element();
        }
        let rows = search.offers(mode).into_iter().enumerate().map(|(ix, (host, offer))| {
            let pressed = offer.clone();
            found_row(theme, ix, host, offer).on_click(cx.listener(move |this, _ev, window, cx| {
                this.press_found(host, &pressed, window, cx);
            }))
        });
        // A card: the rows' radius plus the pad round them, so the corners nest.
        let frame = kit::card(theme).flex().flex_col().p(px(spacing.xxs)).children(rows);
        section.child(label).child(frame).into_any_element()
    }

    /// This Mac's checklist: a line for each thing the worker needs, marked as its `doctor`
    /// reads it, the missing ones with the button that fixes them, in one list a tone step off
    /// the page as the tailnet's rows are; under it, the add under way or why it failed.
    fn this_mac_checklist(&self, flow: &this_mac::Flow, cx: &Context<Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let (s, spacing) = (theme.surfaces, theme.spacing);
        let session = slopty_platform::service::Session::native();
        let logs = session.logs(slopty_platform::service::WORKER);
        let server_logs = session.logs(slopty_platform::service::SERVER);
        let all: Vec<this_mac::Line> = this_mac::checklist(flow, &logs, &server_logs)
            .into_iter()
            .chain(this_mac::app_lines(flow))
            .collect();
        // What a line that holds says of itself waits under Details: the checklist says what
        // is ready and what to do, and the versions and names are a press away.
        let details = self.this_mac_details(flow, &all, cx);
        let lines = all.into_iter().map(|line| self.this_mac_line(line, cx));
        let frame = kit::card(theme)
            .id("this-mac-checklist")
            .debug_selector(|| "this-mac-checklist".to_owned())
            .role(Role::List)
            .aria_label(this_mac::TITLE)
            .flex()
            .flex_col()
            .p(px(spacing.xxs))
            .children(lines);
        let status = match (&flow.error, flow.listing) {
            (Some(why), _) => Some((why.clone(), s.error)),
            (None, true) => {
                Some(("Waiting for the server to list this Mac\u{2026}".to_owned(), s.text_muted))
            }
            (None, false) if flow.listed => Some((flow.ready_words().to_owned(), s.text_muted)),
            (None, false) => None,
        };
        let run = flow.run;
        let again = flow.error.is_some().then(|| {
            kit::button(theme, "this-mac-list-again", "Try again", ButtonKind::Link).on_click(
                cx.listener(move |this, _ev, window, cx| {
                    this.wait_this_mac_listed(run, window, cx);
                }),
            )
        });
        let done = flow.listed.then(|| {
            kit::button(theme, "this-mac-done", "Done", ButtonKind::Primary)
                .on_click(cx.listener(|this, _ev, window, cx| this.close_this_mac(window, cx)))
        });
        // The end of the flow is where a phone joins: this Mac's server is set by now.
        let phone = flow.listed.then(|| {
            let row = phone_row(theme).on_click(
                cx.listener(|this, _ev, window, cx| this.connect_device_from_panel(window, cx)),
            );
            kit::card(theme).flex().flex_col().p(px(spacing.xxs)).child(row)
        });
        div()
            .flex()
            .flex_col()
            .gap(px(spacing.sm))
            .child(frame)
            .child(details)
            .when_some(status, |el, (text, tone)| {
                el.child(
                    kit::meta(div(), theme)
                        .id("this-mac-status")
                        .debug_selector(|| "this-mac-status".to_owned())
                        .role(Role::Status)
                        .aria_label(SharedString::from(text.clone()))
                        .flex()
                        .items_center()
                        .gap(px(spacing.sm))
                        .child(div().text_color(hsla(tone)).child(SharedString::from(text)))
                        .children(again),
                )
            })
            .children(phone)
            .when_some(done, |el, done| el.child(div().flex().justify_end().child(done)))
            .into_any_element()
    }

    /// The checklist's Details: a link that opens and closes them, then, open, what each line
    /// that holds says of itself and the app's version and build, quiet, a line each.
    fn this_mac_details(
        &self,
        flow: &this_mac::Flow,
        lines: &[this_mac::Line],
        cx: &Context<Self>,
    ) -> gpui::Div {
        let theme = &self.theme;
        let spacing = theme.spacing;
        let open = flow.details;
        let run = flow.run;
        let toggle = kit::button(
            theme,
            "this-mac-details",
            if open { HIDE_DETAILS } else { SHOW_DETAILS },
            ButtonKind::Link,
        )
        .aria_expanded(open)
        .on_click(cx.listener(move |this, _ev, _window, cx| {
            if let Some(flow) = this.this_mac_flow(run) {
                flow.details = !flow.details;
            }
            cx.notify();
        }));
        let facts = open.then(|| {
            let (version, build) = slopty_ui::settings_form::schema::about();
            let held = lines
                .iter()
                .filter(|line| line.mark == this_mac::Mark::Ok)
                .map(|line| format!("{} \u{b7} {}", line.check.title(), line.detail));
            let app = format!("Slopty {version} \u{b7} {build}");
            div()
                .id("this-mac-facts")
                .debug_selector(|| "this-mac-facts".to_owned())
                .role(Role::List)
                .aria_label(SHOW_DETAILS)
                .flex()
                .flex_col()
                .children(held.chain(std::iter::once(app)).map(|fact| {
                    kit::typed(kit::meta(div(), theme), theme.roles().metadata, 1.0)
                        .child(SharedString::from(fact))
                }))
        });
        div().flex().flex_col().items_start().gap(px(spacing.xs)).child(toggle).children(facts)
    }

    /// One line of this Mac's checklist: its mark and its name, then, while it does not hold,
    /// what it waits on or what to do, and a missing line's button. A line that holds is its
    /// name alone; what it says of itself is under Details ([`Self::this_mac_details`]).
    fn this_mac_line(&self, line: this_mac::Line, cx: &Context<Self>) -> gpui::Stateful<gpui::Div> {
        use slopty_ui::icons::status_mark;
        let theme = &self.theme;
        let s = theme.surfaces;
        let status = line.status();
        let check = line.check;
        let fix = line.fix.map(|fix| {
            kit::button(theme, check.fix_id(), fix.label(), ButtonKind::Secondary).on_click(
                cx.listener(move |this, _ev, window, cx| this.this_mac_fix(fix, window, cx)),
            )
        });
        let muted = line.mark == this_mac::Mark::Unknown;
        let holds = line.mark == this_mac::Mark::Ok;
        // A line's detail wraps rather than cut off a path or a reason, so the line grows from
        // a two-line row's height instead of holding it. The fix stands beside the title and
        // the detail together, centred on them, so a line with a button is as tall as one
        // without.
        kit::inset_x(div(), theme)
            .id(gpui::ElementId::Name(format!("this-mac-{}", check.slug()).into()))
            .debug_selector(move || format!("this-mac-{}", check.slug()))
            .role(Role::ListItem)
            .aria_label(check.title())
            .aria_description(SharedString::from(line.detail.clone()))
            .flex()
            .items_start()
            .gap(px(theme.spacing.sm))
            .min_h(px(if holds { kit::Row::One } else { kit::Row::Two }.height(theme)))
            .py(px(theme.spacing.xs))
            .when(holds, gpui::Styled::items_center)
            .child(status_mark(theme, Some(status), 1.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .min_w_0()
                            .text_size(px(theme.typography.ui_size))
                            .font_weight(gpui::FontWeight(Typography::MEDIUM_WEIGHT))
                            .text_color(hsla(if muted { s.text_secondary } else { s.text }))
                            .child(check.title()),
                    )
                    .when(!holds, |el| {
                        el.child(kit::meta(div(), theme).child(SharedString::from(line.detail)))
                    }),
            )
            .when_some(fix, |el, fix| el.child(div().flex_none().self_center().child(fix)))
    }

    /// Esc, Tab, sticky Control, arrows and the shell symbols a phone keyboard hides; shown
    /// above the keyboard inset while a terminal or a remote window is active.
    ///
    /// The bar is the body's own surface under a hairline, so it reads as the tile's input row
    /// and its caps as plates on it, with no hairline of their own. Its row scrolls where it
    /// overflows, and an end with keys past it fades out.
    fn key_bar(&self, target: &KeyTarget, window: &Window, cx: &Context<Self>) -> gpui::AnyElement {
        let safe = window.insets().effective();
        let width = f32::from(self.frame_size(window).width - safe.left - safe.right);
        let (row, overflow) = match target {
            KeyTarget::Terminal(terminal) => self.terminal_key_bar(terminal, width, cx),
            KeyTarget::Screen(screen) => self.screen_key_bar(screen, width, cx),
        };
        // The extent comes from the caps, not the last layout, so a bar just shown, turned or
        // resized fades right on its first frame.
        let scrolled = -f32::from(self.key_bar_scroll.offset().x);
        let (leading, trailing) = key_bar_fades(scrolled, overflow);
        let ends = gpui::EdgeFade::x(px(self.theme.spacing.lg)).depth(gpui::Edges {
            left: f32::from(leading),
            right: f32::from(trailing),
            ..gpui::Edges::default()
        });
        // The caps fade per pixel; the bar's surface is outside the fade.
        div()
            .w_full()
            .bg(hsla(self.theme.content()))
            .border_t(kit::HAIR)
            .border_color(hsla(self.theme.surfaces.border))
            .child(gpui::edge_fade(row, ends))
            .into_any_element()
    }

    /// The size the app lays itself out in: the window's, or the self-test's stand-in for
    /// Split View.
    fn frame_size(&self, window: &Window) -> gpui::Size<gpui::Pixels> {
        self.split_view.unwrap_or_else(|| window.viewport_size())
    }

    /// The key bar's row with `keys` in it, and how far it scrolls ([`key_row_overflow`]).
    /// Where they all fit in `width` (an iPad), one leading run of their groups a step apart,
    /// the word keys trailing ([`key_group`]); where they do not (a phone), one line in the
    /// order given that scrolls sideways.
    fn key_row_of(
        &self,
        keys: Vec<(&'static str, gpui::AnyElement)>,
        width: f32,
    ) -> (gpui::Stateful<gpui::Div>, f32) {
        let spacing = self.theme.spacing;
        let row = self.key_row();
        let overflow = key_row_overflow(keys.iter().map(|(label, _)| *label), width, spacing);
        if overflow > 0.0 {
            return (row.children(keys.into_iter().map(|(_, key)| key)), overflow);
        }
        let (mut lead, mut arrows, mut symbols, mut words) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for (label, key) in keys {
            match key_group(label) {
                KeyGroup::Lead => lead.push(key),
                KeyGroup::Arrows => arrows.push(key),
                KeyGroup::Symbols => symbols.push(key),
                KeyGroup::Words => words.push(key),
            }
        }
        let group = |keys: Vec<gpui::AnyElement>| {
            div().flex().flex_none().items_center().gap(px(spacing.xs)).children(keys)
        };
        let run = [lead, arrows, symbols].into_iter().filter(|g| !g.is_empty()).map(group);
        let spread = row
            .debug_selector(|| "key-bar-spread".to_owned())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(spacing.md))
                    .debug_selector(|| "key-bar-lead".to_owned())
                    .children(run),
            )
            .child(div().flex_1())
            .child(group(words).debug_selector(|| "key-bar-trail".to_owned()));
        (spread, 0.0)
    }

    /// The key bar's row, before its keys: one line that scrolls sideways.
    fn key_row(&self) -> gpui::Stateful<gpui::Div> {
        let spacing = self.theme.spacing;
        div()
            .id("key-bar")
            .h(px(KEY_BAR_H))
            .w_full()
            .flex()
            .items_center()
            .overflow_x_scroll()
            .track_scroll(&self.key_bar_scroll)
            .px(px(spacing.xs))
            .gap(px(spacing.xs))
            .font_family(self.theme.typography.ui_family.clone())
    }

    /// A key cap of the bar: raised off the bar ([`kit::raised`]), `pressed` while held, the
    /// accent fill with its ink when `lit` (armed or toggled on). A word ("Esc", "Paste")
    /// is set small, as a keyboard sets its word keys; a glyph at the title size, so an arrow
    /// reads at a glance on a 36 pt cap.
    fn key_cap(&self, id: String, label: &str, lit: bool) -> gpui::Stateful<gpui::Div> {
        let s = &self.theme.surfaces;
        let (spacing, ty) = (self.theme.spacing, &self.theme.typography);
        let word = label.chars().count() > 1;
        div()
            .id(SharedString::from(id))
            .flex_none()
            .w(px(cap_width(label, spacing)))
            .h(px(cap_side(spacing)))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(self.theme.radii.sm))
            .text_size(px(if word { ty.small() } else { ty.title() }))
            .font_weight(gpui::FontWeight(Typography::MEDIUM_WEIGHT))
            .map(|el| {
                if lit {
                    kit::solid(el, &self.theme)
                } else {
                    kit::raised(el.text_color(hsla(s.text)), &self.theme)
                }
            })
            .when(!lit, |el| el.active(|el| el.bg(hsla(s.pressed))))
    }

    /// One key of the bar; `lit` draws it armed.
    fn bar_key(
        &self,
        id: String,
        label: &'static str,
        lit: bool,
        on_click: impl Fn(&mut Window, &mut App) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        let key = self
            .key_cap(id, label, lit)
            .role(Role::Button)
            .aria_label(SharedString::from(key_label(label, lit)))
            .child(SharedString::from(label));
        tab_stop(key, self.theme.surfaces.focus)
            .on_click(move |_ev, window, cx| on_click(window, cx))
    }

    /// The system's paste button, made once a key bar can show and made again for a theme
    /// that changed; hidden until a Paste cap drawn this frame puts it over itself.
    #[cfg(target_os = "ios")]
    fn ready_paste_key(&mut self, window: &Window, cx: &Context<Self>) {
        let stale = self.paste_key.as_ref().is_some_and(|key| !key.drawn_as(&self.theme));
        if stale || (self.paste_key.is_none() && key_bar_visible(TOUCH, self.hardware_keyboard)) {
            let workspace = self.view.downgrade();
            self.paste_key =
                slopty_ui::paste_key::PasteKey::new(window, &self.theme, workspace, cx)
                    .map(Rc::new);
        }
        if let Some(key) = &self.paste_key {
            key.hide();
        }
    }

    /// What puts the system's paste button over a Paste cap: an element as large as the cap,
    /// whose paint places the button while the cap is wholly in the row's view.
    #[cfg(target_os = "ios")]
    fn paste_key_spot(&self) -> Option<gpui::AnyElement> {
        let key = Rc::clone(self.paste_key.as_ref()?);
        let spot = gpui::canvas(
            |_bounds, _window, _cx| {},
            move |bounds, (), window, _cx| key.place(bounds, window.content_mask().bounds),
        );
        Some(spot.absolute().inset_0().into_any_element())
    }

    /// The bar over a remote window: chords and arrows, copy and paste through the worker.
    fn screen_key_bar(
        &self,
        screen: &Entity<ScreenView>,
        width: f32,
        cx: &Context<Self>,
    ) -> (gpui::AnyElement, f32) {
        let view = screen.read(cx);
        let (control, command) = (view.sticky(Sticky::Control), view.sticky(Sticky::Command));
        let mut keys: Vec<(&'static str, gpui::AnyElement)> = Vec::new();
        for (label, key, typed) in SCREEN_BAR_KEYS {
            let sticky = match key {
                "" => Some(Sticky::Control),
                "cmd" => Some(Sticky::Command),
                _ => None,
            };
            let lit = matches!(sticky, Some(Sticky::Control) if control)
                || matches!(sticky, Some(Sticky::Command) if command);
            let target = screen.clone();
            let el = self.bar_key(format!("skey-{label}"), label, lit, move |_window, cx| {
                target.update(cx, |v, cx| match sticky {
                    Some(which) => {
                        let on = !v.sticky(which);
                        v.set_sticky(which, on, cx);
                    }
                    None => v.press(
                        gpui::Keystroke {
                            modifiers: gpui::Modifiers::default(),
                            key: key.to_owned(),
                            key_char: typed.map(str::to_owned),
                        },
                        cx,
                    ),
                });
            });
            keys.push((label, el.into_any_element()));
        }
        let target = screen.clone();
        let copy = self.bar_key("skey-copy".to_owned(), "Copy", false, move |_w, cx| {
            target.update(cx, ScreenView::copy_key);
        });
        keys.push(("Copy", copy.into_any_element()));
        let target = screen.clone();
        let paste = self.bar_key("skey-paste".to_owned(), "Paste", false, move |_w, cx| {
            target.update(cx, ScreenView::paste_key);
        });
        #[cfg(target_os = "ios")]
        let paste = paste.children(self.paste_key_spot());
        keys.push(("Paste", paste.into_any_element()));
        let (row, overflow) = self.key_row_of(keys, width);
        (row.into_any_element(), overflow)
    }

    fn terminal_key_bar(
        &self,
        terminal: &Entity<TerminalView>,
        width: f32,
        cx: &Context<Self>,
    ) -> (gpui::AnyElement, f32) {
        let focus = self.theme.surfaces.focus;
        let view = terminal.read(cx);
        let (armed, armed_command) = (view.sticky_control(), view.sticky_command());
        let armed_alt = view.sticky_alt();
        let (has_selection, finding) = (view.selection().is_some(), view.finding());
        let mut keys: Vec<(&'static str, gpui::AnyElement)> =
            Vec::with_capacity(BAR_KEYS.len().saturating_add(2));
        for (label, key, typed) in BAR_KEYS {
            let is_control = key.is_empty();
            let is_command = key == "cmd";
            let is_alt = key == "alt";
            let lit =
                (is_control && armed) || (is_command && armed_command) || (is_alt && armed_alt);
            let target = terminal.clone();
            let el = self.bar_key(format!("key-{label}"), label, lit, move |_window, cx| {
                target.update(cx, |t, cx| {
                    if is_control {
                        let on = !t.sticky_control();
                        t.set_sticky_control(on, cx);
                    } else if is_alt {
                        let on = !t.sticky_alt();
                        t.set_sticky_alt(on, cx);
                    } else if is_command {
                        let on = !t.sticky_command();
                        t.set_sticky_command(on, cx);
                    } else {
                        t.press(
                            gpui::Keystroke {
                                modifiers: gpui::Modifiers::default(),
                                key: key.to_owned(),
                                key_char: typed.map(str::to_owned),
                            },
                            cx,
                        );
                    }
                });
            });
            keys.push((label, el.into_any_element()));
        }
        // The phone has no ⌘C/⌘V: while text is selected the bar offers copy, otherwise paste.
        let target = terminal.clone();
        let clip_label = if has_selection { "Copy" } else { "Paste" };
        let clipboard = self
            .key_cap("key-clipboard".to_owned(), clip_label, has_selection)
            .role(Role::Button)
            .aria_label(clip_label)
            .child(clip_label);
        // Copy is this app's own; only a paste reads another's clipboard.
        #[cfg(target_os = "ios")]
        let clipboard =
            clipboard.children((!has_selection).then(|| self.paste_key_spot()).flatten());
        let clipboard = tab_stop(clipboard, focus).on_click(move |_ev, window, cx| {
            target.update(cx, |t, cx| {
                if has_selection {
                    // Copy, then the key reads "Paste" again.
                    t.copy(&slopty_ui::terminal::Copy, window, cx);
                    t.clear_selection(cx);
                } else {
                    t.paste_clipboard(&slopty_ui::terminal::Paste, window, cx);
                }
            });
        });
        // Right after the arrows: on a phone it is in view before the row scrolls.
        keys.insert(ARROWS_END.min(keys.len()), (clip_label, clipboard.into_any_element()));
        // No ⌘F either: "Find" opens the search bar, or closes it while it is open.
        let target = terminal.clone();
        let find = self
            .key_cap("key-find".to_owned(), "Find", finding)
            .role(Role::Button)
            .aria_label(if finding { "Close find" } else { "Find" })
            .child("Find");
        let find = tab_stop(find, focus).on_click(move |_ev, window, cx| {
            target.update(cx, |t, cx| {
                if finding {
                    t.close_find(&slopty_ui::terminal::CloseFind, window, cx);
                } else {
                    t.find(&slopty_ui::terminal::Find, window, cx);
                }
            });
        });
        keys.push(("Find", find.into_any_element()));
        let (row, overflow) = self.key_row_of(keys, width);
        (row.into_any_element(), overflow)
    }
}

/// How a worker loop dials worker `id`, at an address when the directory has one.
#[derive(Clone)]
struct Dialer(Rc<DialFn>);

type DialFn = dyn Fn(
    WorkerId,
    slopty_net::HostAddr,
    &mut App,
) -> gpui::Task<Result<net::Connected, net::DialFailed>>;

impl std::fmt::Debug for Dialer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Dialer")
    }
}

/// Dial over the network, on `runtime`, where the transport lives ([`net::connect_to`]).
fn network_dialer(runtime: tokio::runtime::Handle) -> Dialer {
    Dialer(Rc::new(move |id, address, cx| {
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        runtime.spawn(async move {
            let _sent = ready_tx.send(net::connect_to(id, address).await);
        });
        cx.foreground_executor().spawn(async move {
            ready_rx
                .await
                .unwrap_or_else(|_| Err(net::DialFailed::Other("connection task died".to_owned())))
        })
    }))
}

/// Wait `delay`, or less if `wake` is notified first; whether it was.
async fn wait_or_wake(
    cx: &gpui::AsyncApp,
    wake: &tokio::sync::Notify,
    delay: std::time::Duration,
) -> bool {
    let timer = cx.background_executor().timer(delay);
    tokio::select! {
        () = wake.notified() => true,
        () = timer => false,
    }
}

/// Probe `link` after `resume`: a QUIC PING, and the link alive the moment anything arrives
/// from the worker, within the probe's deadline ([`workers::Probe`]). Its worker's tiles are
/// set back ([`WorkerStatus::Checking`]) from when the probe says, and come back with the
/// answer.
async fn probe(
    cx: &mut gpui::AsyncApp,
    link: &slopty_client::WorkerLink,
    resume: slopty_platform::resume::Resume,
    view: &Entity<WorkspaceView>,
    key: WorkerKey,
) -> bool {
    let plan = workers::Probe::new(resume, link.rtt());
    let before = link.received_datagrams();
    let started = std::time::Instant::now();
    link.ping();
    let mut doubted = false;
    loop {
        let waited = started.elapsed();
        if link.received_datagrams() != before {
            if doubted {
                view.update(cx, |v, cx| v.set_worker_status(key, WorkerStatus::Connected, cx));
            }
            tracing::info!(resume = resume.name(), ?waited, "link answered after a resume");
            return true;
        }
        if waited >= plan.deadline {
            return false;
        }
        if !doubted && waited >= plan.doubt_after {
            doubted = true;
            view.update(cx, |v, cx| v.set_worker_status(key, WorkerStatus::Checking, cx));
        }
        cx.background_executor().timer(workers::PROBE_POLL).await;
    }
}

/// A key-bar key as a screen reader names it; an armed modifier says so.
fn key_label(label: &str, lit: bool) -> String {
    let name = key_spoken(label);
    if lit { format!("{name}, armed") } else { name.to_owned() }
}

/// A key-bar key's name for a screen reader: what the cap shows, spelled out.
fn key_spoken(label: &str) -> &str {
    match label {
        "Esc" => "Escape",
        CONTROL_CAP => "Control",
        ALT_CAP => "Alt",
        "⌘" => "Command",
        "PgUp" => "Page up",
        "PgDn" => "Page down",
        "←" => "Left arrow",
        "↑" => "Up arrow",
        "↓" => "Down arrow",
        "→" => "Right arrow",
        "-" => "Minus",
        "/" => "Slash",
        "|" => "Pipe",
        "~" => "Tilde",
        word => word,
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.renders = self.renders.saturating_add(1);
        }
        // Notch / Dynamic Island, home indicator and the soft keyboard on iOS; zero on macOS.
        // The workspace keeps the top and the sides clear itself.
        let insets = window.insets().effective();
        #[cfg(target_os = "ios")]
        self.ready_paste_key(window, cx);
        let key_bar = self.key_target.clone().map(|target| self.key_bar(&target, window, cx));
        let surfaces = self.theme.surfaces;
        let band = if key_bar.is_some() { self.theme.content() } else { surfaces.canvas };
        if std::mem::take(&mut self.pending_focus_editor)
            && let Some(editor) = self.settings_editor.clone()
        {
            editor.update(cx, |e, cx| e.focus(window, cx));
            // Focus moved while the window draws, which asks for no frame: a view already drawn
            // in this one (a shell's caret) would keep the old focus. The next frame is asked
            // for, and GPUI builds again the views whose focus answers changed.
            let editor = editor.entity_id();
            window.defer(cx, move |_window, cx| App::notify(cx, editor));
        }
        let settings_editor =
            self.settings_editor.clone().or_else(|| self.settings_leaving.clone());
        let welcome = self.welcome();
        if let Some(sheet) = self.adding.as_mut().and_then(|a| a.ssh.as_mut()) {
            sheet.follow_secure(window, cx);
        }
        let adding = self.adding.as_ref().map(|adding| self.add_worker_panel(adding, window, cx));
        let inviting = self.inviting.as_ref().map(|invite| self.invite_dialog(invite, window, cx));
        let root = match self.split_view {
            Some(size) => div().w(size.width).h(size.height),
            None => div().size_full(),
        };
        root.relative()
            .flex()
            .flex_col()
            .bg(hsla(surfaces.canvas))
            .on_action(cx.listener(|this, _: &AddWorker, window, cx| {
                this.show_add_worker(Panel::Worker, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ConnectServer, window, cx| {
                this.show_add_worker(Panel::Server, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ConnectDevice, window, cx| {
                this.show_invite(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CopyTailnetGrant, _window, cx| this.copy_grant(cx)))
            .on_action(
                cx.listener(|this, _: &UpdateAllWorkers, _window, cx| this.update_all_workers(cx)),
            )
            .on_action(cx.listener(|this, _: &UpdateServer, _window, cx| this.update_server(cx)))
            .on_action(cx.listener(|this, _: &ShowWorkersInFinder, _window, cx| {
                this.show_workers_in_finder(cx);
            }))
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .on_action(cx.listener(|this, _: &OpenKeyboardShortcuts, window, cx| {
                let keyboard = slopty_ui::settings_form::schema::Section::Keyboard;
                this.open_settings_at(keyboard, window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenAbout, window, cx| {
                let about = slopty_ui::settings_form::schema::Section::About;
                this.open_settings_at(about, window, cx);
            }))
            .when(!welcome, |el| {
                el.child(div().flex_1().w_full().min_h_0().child(self.view.clone()))
                    .when_some(key_bar, |el, bar| {
                        el.child(div().w_full().pl(insets.left).pr(insets.right).child(bar))
                    })
                    // The home indicator's band continues what is above it: the key bar on the
                    // body's surface, else the strip's ground, `canvas`.
                    .child(div().w_full().h(insets.bottom).bg(hsla(band)))
            })
            .when_some(adding, gpui::ParentElement::child)
            .when_some(inviting, gpui::ParentElement::child)
            .when_some(settings_editor, gpui::ParentElement::child)
    }
}

/// Apply one event from a worker's link to the workspace; `true` when the link is gone.
fn apply_link_event(
    this: &WeakEntity<Workspace>,
    view: &Entity<WorkspaceView>,
    key: WorkerKey,
    event: LinkEvent,
    cx: &mut App,
) -> bool {
    tracing::trace!(?event, "link event");
    match event {
        LinkEvent::Term { session, event } => {
            view.update(cx, |v, cx| v.term_event(session, event, cx));
        }
        LinkEvent::Control(WorkerMsg::Term { session, event }) => {
            view.update(cx, |v, cx| v.term_event(session, event, cx));
        }
        LinkEvent::Control(WorkerMsg::Items(sync)) => {
            view.update(cx, |v, cx| v.apply_sync(key, sync, cx));
        }
        LinkEvent::Control(
            WorkerMsg::SessionOpened { summary, .. } | WorkerMsg::SessionChanged(summary),
        ) => {
            view.update(cx, |v, cx| v.session_opened(key, summary, cx));
        }
        LinkEvent::Control(WorkerMsg::Failed { message, .. }) => {
            view.update(cx, |v, cx| v.open_failed(key, &message, cx));
        }
        LinkEvent::Control(WorkerMsg::Load(load)) => {
            view.update(cx, |v, cx| v.set_worker_load(key, load, cx));
        }
        LinkEvent::Control(WorkerMsg::SessionClosed { session, .. }) => {
            view.update(cx, |v, cx| v.session_closed(session, cx));
        }
        LinkEvent::Control(WorkerMsg::Screen(event)) => {
            view.update(cx, |v, cx| v.screen_event(key, event, cx));
        }
        LinkEvent::Control(WorkerMsg::File { path, read }) => {
            view.update(cx, |v, cx| v.file_read(key, &path, &read, cx));
        }
        LinkEvent::Control(WorkerMsg::Written { path, result }) => {
            view.update(cx, |v, cx| v.file_written(key, &path, &result, cx));
        }
        LinkEvent::Control(WorkerMsg::FoundFiles { root, query, paths, notice }) => {
            view.update(cx, |v, cx| {
                v.files_found(&root, &query, &paths, cx);
                v.threads_found(key, &root, &query, &paths, cx);
                if let Some(notice) = notice {
                    v.show_notice(notice, cx);
                }
            });
        }
        LinkEvent::Control(WorkerMsg::Folder { path, listing }) => {
            view.update(cx, |v, cx| v.folder_listed(key, &path, &listing, cx));
        }
        LinkEvent::Control(WorkerMsg::Search(event)) => {
            view.update(cx, |v, cx| v.search_event(key, event, cx));
        }
        LinkEvent::Control(WorkerMsg::Clip(msg)) => {
            view.update(cx, |v, cx| v.clip_message(key, msg, cx));
        }
        LinkEvent::Control(WorkerMsg::Xfer(msg)) => {
            view.update(cx, |v, cx| v.xfer_message(msg, cx));
        }
        LinkEvent::Control(WorkerMsg::Path(path)) => {
            view.update(cx, |v, cx| v.set_link_path(key, path, cx));
        }
        LinkEvent::Control(WorkerMsg::Caps(mut caps)) => {
            pin_grants(&mut caps);
            view.update(cx, |v, cx| v.set_worker_caps(key, caps, cx));
        }
        LinkEvent::Ports { session, forwards } => {
            view.update(cx, |v, cx| v.ports_changed(session, forwards, cx));
        }
        LinkEvent::XferFailed { xfer, error } => {
            view.update(cx, |v, cx| v.xfer_failed(xfer, &error.to_string(), cx));
        }
        LinkEvent::Handoff { event, received } => {
            view.update(cx, |v, cx| v.handoff_event(key, event, received, cx));
        }
        LinkEvent::Control(WorkerMsg::AgentBranch(branch)) => {
            view.update(cx, |v, cx| v.agent_branch(branch, cx));
        }
        LinkEvent::Control(WorkerMsg::Threads(frame)) => {
            view.update(cx, |v, cx| v.thread_table(key, &frame, cx));
        }
        LinkEvent::Control(WorkerMsg::IntentDone(done)) => {
            view.update(cx, |v, cx| v.thread_done(key, &done, cx));
        }
        LinkEvent::Thread { thread, frame } => {
            view.update(cx, |v, cx| v.thread_frame(key, thread, frame, cx));
        }
        LinkEvent::Control(WorkerMsg::ThreadHits(hits)) => {
            view.update(cx, |v, cx| v.thread_hits(key, hits, cx));
        }
        LinkEvent::Control(WorkerMsg::Authors(authors)) => {
            view.update(cx, |v, cx| v.thread_authors(key, authors, cx));
        }
        LinkEvent::Control(WorkerMsg::GitDone { request, outcome }) => {
            view.update(cx, |v, cx| v.git_done(key, request, outcome, cx));
        }
        LinkEvent::Control(WorkerMsg::FolderPage { path, after, listing }) => {
            view.update(cx, |v, cx| v.folder_page(key, &path, &after, &listing, cx));
        }
        LinkEvent::Control(WorkerMsg::FsDone { request, outcome }) => {
            view.update(cx, |v, cx| v.fs_done(key, request, outcome, cx));
        }
        LinkEvent::Control(WorkerMsg::Sessions(past)) => {
            view.update(cx, |v, cx| v.past_sessions(key, past, cx));
        }
        // The handshake's ack was read when the link connected; the tick pings to draw a
        // restarted worker's reset, so the pong carries nothing; the app's link forwards
        // ports itself (`LinkEvent::Ports`), and hands a handoff on stamped with when it was
        // read (`LinkEvent::Handoff`).
        LinkEvent::Control(
            WorkerMsg::HelloAck(_)
            | WorkerMsg::Pong { .. }
            | WorkerMsg::Ports { .. }
            | WorkerMsg::Handoff(_),
        ) => {}
        LinkEvent::Disconnected(why) => {
            let status = WorkerStatus::Reconnecting(format!("disconnected: {why}"));
            view.update(cx, |v, cx| {
                v.threads_unlinked(key, cx);
                v.disconnect_worker(key, status, cx);
            });
            let _dropped = this.update(cx, |ws, _cx| {
                if let Some(slot) = ws.workers.iter_mut().find(|w| w.key == key) {
                    slot.link = None;
                }
            });
            return true;
        }
    }
    false
}

/// The display period the frame probe measures against: `SLOPTY_FRAME_HZ` when set (the
/// simulator runs at 60 Hz whatever the device it imitates), else 60 Hz on macOS and 120 Hz on
/// iOS.
fn frame_nominal() -> std::time::Duration {
    std::env::var("SLOPTY_FRAME_HZ")
        .ok()
        .and_then(|hz| hz.parse::<f64>().ok())
        .filter(|hz| hz.is_finite() && *hz > 0.0)
        .map_or_else(slopty_ui::frames::default_nominal, |hz| {
            std::time::Duration::from_secs_f64(1.0 / hz)
        })
}

/// A label over a part of the panel, at the meta size its status lines share, so the block
/// under the heading keeps to two sizes: the body's and the meta's.
fn panel_label(theme: &Theme, selector: &'static str, text: &'static str) -> gpui::Div {
    kit::meta(div(), theme).flex_none().debug_selector(move || selector.to_owned()).child(text)
}

/// A server or a worker the tailnet found, as a row to press, the palette's: its kind's glyph,
/// its name over what it is and its address, and a chevron that says the press goes on.
fn found_row(theme: &Theme, ix: usize, host: Host, offer: &Offer) -> gpui::Stateful<gpui::Div> {
    use slopty_net::discover::Answer;
    use slopty_ui::icons::{IconSize, Symbol, icon};
    let s = theme.surfaces;
    let (glyph, what, verb) = match host {
        Host::Server => (Symbol::ServerRack, "Server", "Connect to"),
        Host::Worker => (Symbol::Display, "Machine", "Add"),
    };
    // What it answered decides what the press does, and the row says that.
    let (verb, said) = match &offer.answer {
        Answer::Ready => (verb.to_owned(), offer.at.clone()),
        Answer::NotGranted => {
            ("Copy the tailnet grant for".to_owned(), "needs a tailnet grant".to_owned())
        }
        Answer::OtherBuild(_) => {
            ("Install this build on".to_owned(), "runs a different build".to_owned())
        }
    };
    let glyph_size = px(theme.typography.icon());
    let row = kit::row(theme, kit::Row::Two)
        .id(("add-worker-found", ix))
        .debug_selector(move || format!("add-worker-found-{ix}"))
        .role(Role::Button)
        .aria_label(SharedString::from(format!("{verb} {}", offer.name)))
        .aria_description(SharedString::from(format!("{what}, {said}")))
        .rounded(px(theme.radii.sm))
        .cursor_pointer()
        // On the list's card: the pointer washes a row one step up.
        .map(kit::eased)
        .hover(move |el| el.bg(hsla(s.hover)))
        .active(move |el| el.bg(hsla(s.pressed)))
        .child(icon(theme, glyph, IconSize::Inline, hsla(s.text_muted)).size(glyph_size))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(theme.typography.ui_size))
                        .font_weight(gpui::FontWeight(Typography::MEDIUM_WEIGHT))
                        .text_color(hsla(s.text))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(SharedString::from(offer.name.clone())),
                )
                .child(
                    kit::tabular(kit::meta(div(), theme))
                        .child(SharedString::from(format!("{what} \u{b7} {said}"))),
                ),
        )
        .child(
            icon(theme, Symbol::ChevronRight, IconSize::Inline, hsla(s.text_muted))
                .size(glyph_size),
        );
    tab_stop(row, s.focus)
}

/// The label over this Mac's row on the add panels.
const THIS_MAC_LABEL: &str = "On this Mac";
/// The link that opens this Mac's checklist details.
const SHOW_DETAILS: &str = "Show details";
/// The same link once they are open.
const HIDE_DETAILS: &str = "Hide details";
/// The first run's heading: where to work comes before any server.
const CHOOSE_TITLE: &str = "Choose where to work";
/// The line under it: what the choice is about, in one line of a 402 pt phone.
const CHOOSE_BLURB: &str = "Run your agents and shells here or on your other machines.";
/// The first run's other choice: a server that lists machines already.
const CONNECT_TITLE: &str = "Connect to an existing server";
/// What it is, in one line of a 402 pt phone.
const CONNECT_LINE: &str = "Reach the machines a Slopty server already lists.";
/// The label over this Mac's row and the SSH row together.
const SET_UP_LABEL: &str = "Set up a machine";
/// The label over the machine panel's row for a phone or iPad.
const PHONE_LABEL: &str = "On a phone or iPad";
/// The label over the server panel's rows: the server on this Mac, or over SSH.
const SET_UP_SERVER_LABEL: &str = "Set up the server";

/// This Mac as a row to press, drawn as a found worker's is: the Mac's glyph, what pressing
/// does over what follows, and the chevron that says the press goes on to a checklist.
fn this_mac_row(theme: &Theme, listed: bool, choosing: bool) -> gpui::Stateful<gpui::Div> {
    use slopty_ui::icons::Symbol;
    if choosing {
        // The first run's choice says what it does in a line at the body's size.
        let line = kit::typed(div(), theme.roles().chrome, 1.0)
            .text_color(hsla(theme.surfaces.text_secondary))
            .child(this_mac::CHOICE);
        return entry_row_saying(theme, "use-this-mac", Symbol::Display, this_mac::TITLE, line);
    }
    let meta = if listed { this_mac::ROW_META_LISTED } else { this_mac::ROW_META };
    entry_row(theme, "use-this-mac", Symbol::Display, this_mac::TITLE, meta)
}

/// A phone or iPad's way in as a row to press: the code it scans, opened from the add panel.
fn phone_row(theme: &Theme) -> gpui::Stateful<gpui::Div> {
    use slopty_ui::icons::Symbol;
    entry_row(theme, "connect-device", Symbol::Iphone, invite::TITLE, invite::ROW_META)
}

/// A way to add a worker that goes on to a sheet of its own, as a row to press drawn as a found
/// worker's is: its glyph, what pressing does over what follows, and the chevron that says the
/// press goes on.
fn entry_row(
    theme: &Theme,
    id: &'static str,
    glyph: slopty_ui::icons::Symbol,
    title: &'static str,
    meta: &'static str,
) -> gpui::Stateful<gpui::Div> {
    entry_row_saying(theme, id, glyph, title, kit::meta(div(), theme).child(meta))
}

/// [`entry_row`] with `line` as its second line.
fn entry_row_saying(
    theme: &Theme,
    id: &'static str,
    glyph: slopty_ui::icons::Symbol,
    title: &'static str,
    line: gpui::Div,
) -> gpui::Stateful<gpui::Div> {
    use slopty_ui::icons::{IconSize, Symbol, icon};
    let s = theme.surfaces;
    let glyph_size = px(theme.typography.icon());
    let row = kit::row(theme, kit::Row::Two)
        .id(id)
        .debug_selector(move || id.to_owned())
        .role(Role::Button)
        .aria_label(title)
        .rounded(px(theme.radii.sm))
        .cursor_pointer()
        // On the list's card: the pointer washes a row one step up.
        .map(kit::eased)
        .hover(move |el| el.bg(hsla(s.hover)))
        .active(move |el| el.bg(hsla(s.pressed)))
        .child(icon(theme, glyph, IconSize::Inline, hsla(s.text_muted)).size(glyph_size))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(theme.typography.ui_size))
                        .font_weight(gpui::FontWeight(Typography::MEDIUM_WEIGHT))
                        .text_color(hsla(s.text))
                        .child(title),
                )
                .child(line),
        )
        .child(
            icon(theme, Symbol::ChevronRight, IconSize::Inline, hsla(s.text_muted))
                .size(glyph_size),
        );
    tab_stop(row, s.focus)
}

/// The app's own commands, bound outside any view's context: its rows of the keymap's table
/// (`[keys.app]`), which lives in `slopty_ui::keymap` beside the rest.
fn app_commands() -> Vec<slopty_ui::keymap::Command> {
    use slopty_ui::keymap::app_command;
    let mut commands = vec![
        app_command("open_settings", OpenSettings, &["cmd-,"]),
        app_command("about", OpenAbout, &[]),
        app_command("add_worker", AddWorker, &["cmd-shift-h"]),
        app_command("connect_server", ConnectServer, &[]),
        app_command("connect_device", ConnectDevice, &[]),
        app_command("copy_tailnet_grant", CopyTailnetGrant, &[]),
        app_command("update_all_workers", UpdateAllWorkers, &[]),
        app_command("update_server", UpdateServer, &[]),
    ];
    if finder::OFFERED {
        commands.push(app_command("show_workers_in_finder", ShowWorkersInFinder, &[]));
    }
    commands
}

/// The app's commands and the rest of the table, with `keys` over them.
fn keymap_for(keys: &slopty_settings::KeySettings) -> slopty_ui::keymap::Keymap {
    slopty_ui::keymap::Keymap::new(keys, app_commands())
}

/// The app's own bindings in effect.
fn app_key_bindings() -> Vec<gpui::KeyBinding> {
    slopty_ui::keymap::current().bindings(|scope| scope == slopty_ui::keymap::Scope::App)
}

/// The palette's way to the keyboard shortcuts: the Mac's Help menu has it, an iPad with a
/// keyboard its menus, and a phone or a closed menu bar only this.
const KEYBOARD_SHORTCUTS: &str = "Keyboard shortcuts";

/// The palette's way to the version and the build: Settings › About.
const ABOUT: &str = "About Slopty";

/// [`app_palette_items`] and the line that opens a tile in the person's editor, named for it
/// ([`editors::palette_line`]).
fn app_palette(cx: &App) -> Vec<slopty_ui::palette::PaletteItem> {
    let mut items = app_palette_items();
    items.extend(editors::palette_line(cx));
    items
}

/// The app's lines for the command palette, after the workspace's.
fn app_palette_items() -> Vec<slopty_ui::palette::PaletteItem> {
    let bindings = app_key_bindings();
    let item = |label: &str, action: Box<dyn gpui::Action>| {
        slopty_ui::palette::PaletteItem::new(label, action, &bindings)
    };
    let mut items = vec![
        item("Open settings", Box::new(OpenSettings)),
        item(KEYBOARD_SHORTCUTS, Box::new(OpenKeyboardShortcuts)),
        item(ABOUT, Box::new(OpenAbout)),
        item("Connect to a server", Box::new(ConnectServer)),
        item(invite::TITLE, Box::new(ConnectDevice)),
        item(server::COPY_GRANT, Box::new(CopyTailnetGrant)),
        item("Add a machine\u{2026}", Box::new(AddWorker)),
        item(ssh::UPDATE_ALL, Box::new(UpdateAllWorkers)),
        item(server::UPDATE_SERVER, Box::new(UpdateServer)),
    ];
    if finder::OFFERED {
        items.push(item(finder::TITLE, Box::new(ShowWorkersInFinder)));
    }
    items
}

/// Where this device keeps its layout: beside the settings, in the client's data directory.
fn layout_path() -> std::path::PathBuf {
    slopty_platform::dirs::data_dir().join("layout.json")
}

/// What builds the app's menu bar, kept so a rebinding rebuilds it.
struct AppMenus(Rc<dyn Fn() -> Vec<gpui::Menu>>);

impl gpui::Global for AppMenus {}

/// Show the menus `build` makes in the menu bar, and again whenever the keys are rebound.
///
/// A menu's key equivalents are the chords bound when it is set, and AppKit runs them before
/// any binding sees the key, so a menu built once would keep a chord the file took away.
pub fn set_app_menus(cx: &mut App, build: impl Fn() -> Vec<gpui::Menu> + 'static) {
    cx.set_global(AppMenus(Rc::new(build)));
    rebuild_app_menus(cx);
}

/// The menu bar again, from the keys bound now.
fn rebuild_app_menus(cx: &App) {
    if let Some(build) = cx.try_global::<AppMenus>().map(|menus| Rc::clone(&menus.0)) {
        cx.set_menus(build());
    }
}

/// The person's alert: the sound chosen in System Settings and a bounce of the Dock icon.
#[cfg(not(feature = "e2e"))]
fn alert() {
    slopty_platform::attention();
    slopty_platform::bounce();
}

/// The e2e build only notes the alert: tests make no sound on this Mac and never take its Dock.
#[cfg(feature = "e2e")]
fn alert() {
    tracing::info!("alert");
}

/// In the e2e build, the worker's macOS grants as the harness names them
/// (`slopty_e2e::WORKER_GRANTS_ENV`), in place of what this machine granted the worker's
/// binary: a render must not depend on the machine it is drawn on.
#[cfg(feature = "e2e")]
fn pin_grants(caps: &mut slopty_proto::server::WorkerCaps) {
    let Some(grants) = std::env::var_os(slopty_e2e::WORKER_GRANTS_ENV) else {
        return;
    };
    let grants = grants.to_string_lossy();
    caps.can_capture = slopty_e2e::granted(&grants, slopty_e2e::SCREEN_RECORDING);
    caps.can_inject = slopty_e2e::granted(&grants, slopty_e2e::ACCESSIBILITY);
}

/// A worker's grants are what it reports, outside the e2e build.
#[cfg(not(feature = "e2e"))]
const fn pin_grants(_caps: &mut slopty_proto::server::WorkerCaps) {}

/// In the e2e build, every upload on `link` held before its first byte when the harness asks
/// (`slopty_e2e::HOLD_UPLOADS_ENV`): how far one got by a given frame is up to the machine.
#[cfg(feature = "e2e")]
fn hold_uploads(link: &slopty_client::WorkerLink) {
    if std::env::var_os(slopty_e2e::HOLD_UPLOADS_ENV).is_some() {
        link.hold_uploads(true);
    }
}

/// Uploads are never held outside the e2e build.
#[cfg(not(feature = "e2e"))]
const fn hold_uploads(_link: &slopty_client::WorkerLink) {}

/// Whether this launch is `cargo xtask e2e`'s, driven over its socket.
#[cfg(feature = "e2e")]
#[must_use]
pub fn self_test() -> bool {
    std::env::var_os(slopty_e2e::SOCKET_ENV).is_some()
}

/// Whether this launch is `cargo xtask e2e`'s: never, in a build without the self-test.
#[cfg(not(feature = "e2e"))]
#[must_use]
pub const fn self_test() -> bool {
    false
}

/// How long after the window opens the symbols drawn so far are written down for the next
/// launch's prewarm: the first frames, and the tiles that draw once their workers link.
const SYMBOLS_DRAWN_WITHIN: std::time::Duration = std::time::Duration::from_secs(3);

/// Open the workspace window and start a link loop per added worker on `handle`'s runtime.
/// Call once from inside the GPUI application callback, after `gpui_kit::init`.
///
/// # Errors
///
/// When the window cannot be opened.
pub fn open_workspace(
    cx: &mut App,
    handle: tokio::runtime::Handle,
    options: impl Fn(&App) -> WindowOptions + 'static,
) -> anyhow::Result<()> {
    let options: window::MakeOptions = Rc::new(options);
    // The self-test's window keeps the size its test asks for on any screen, a CI Mac's short
    // one too (`slopty_platform::asked_size`); before the window is made, which AppKit fits.
    #[cfg(feature = "e2e")]
    if std::env::var_os(slopty_e2e::SOCKET_ENV).is_some() && cfg!(target_os = "macos") {
        let kept = slopty_platform::asked_size::keep_asked_sizes(c"GPUIWindow");
        tracing::info!(kept, "the self-test's windows keep the size they ask for");
    }
    // The chrome's symbols, drawn on background threads while the window is made: the first
    // symbol of a process loads the system's catalogue, 40–70 ms the first frame must not wait
    // for (docs/MEASUREMENTS.md, "SF Symbols as masks"). Those the last launch's first frames
    // drew, and no more, written down a few seconds after its window opened (below).
    let symbols = slopty_platform::dirs::data_dir().join("symbols");
    slopty_ui::icons::prewarm(&Theme::default(), symbols.clone());
    // VideoToolbox's first decoder session costs 150–400 ms; pay it before any worker is
    // dialed.
    slopty_client::warm_up_decoder();
    // ⌘Q drops no view and no tap, so the hotkey mode a focused remote tile pushed is popped
    // here, as the app terminates. macOS would revert it as the process exits anyway
    // (`CarbonEvents.h`); popping it first leaves nothing to that.
    #[cfg(target_os = "macos")]
    cx.on_app_quit(|_cx| {
        if slopty_platform::system_keys::hotkeys_back_on() {
            tracing::info!("system shortcuts back on this Mac as the app quits");
        }
        async {}
    })
    .detach();
    if let Err(e) = slopty_ui::fonts::install(cx) {
        tracing::error!(error = %e, "bundled fonts");
    }
    // Every window and the terminal draw text at the weight it is set in: AppKit's smoothing
    // would thicken light text on dark by up to 15 % ink (docs/decisions/ui.md).
    cx.set_text_smoothing(gpui::TextSmoothing::Antialiased);
    // The table's defaults until the settings are read, below, lay their `[keys]` over it.
    slopty_ui::keymap::install(keymap_for(&slopty_settings::KeySettings::default()), cx);
    // gpui-kit widgets follow their own theme; put it on the tokens now, and again once the
    // window's appearance is known, below.
    kit::sync(&Theme::default(), cx);
    slopty_ui::frames::install(cx, frame_nominal());
    let settings_path = slopty_settings::path();
    let settings_seen = settings::Seen::of(&settings_path);
    let loaded = Settings::load(&settings_path);
    // The self-test gives each stack a data directory of its own, so a test starts from an
    // empty layout and a relaunch within it puts its layout back as a person's would.
    let read = slopty_ui::workspace::read_layout(&layout_path());
    let unreadable = read.is_err();
    let view = cx.new(|cx| {
        let mut view = WorkspaceView::new(Theme::default(), read.ok().flatten(), cx);
        if unreadable {
            view.show_notice(slopty_ui::workspace::LAYOUT_SET_ASIDE.to_owned(), cx);
        }
        view.extend_palette(app_palette_items());
        view.set_layout_path(layout_path());
        // Beside the layout: the file tiles' edits not yet saved, taken back after a quit or a
        // crash.
        view.set_unsaved_store(
            slopty_client::unsaved::Store::new(slopty_platform::dirs::data_dir().join("unsaved")),
            cx,
        );
        // And the transfers in flight, taken up again at the next launch.
        view.set_transfer_ledger(
            slopty_platform::dirs::data_dir().join(slopty_client::xfer::ledger::FILE),
            cx,
        );
        // Each worker's threads, so an agent's thread draws in its first frame.
        view.set_thread_cache(slopty_platform::dirs::data_dir().join("threads"));
        // Each worker's items, so a cold launch draws its tiles before it is linked.
        view.set_item_cache(slopty_platform::dirs::data_dir().join("items"));
        view.set_pasteboard(pasteboard());
        view.set_hardware_keyboard(hardware_keyboard_attached(), cx);
        #[cfg(feature = "e2e")]
        view.set_animation(false);
        view
    });
    let (directory_cache, cache_writes) = tokio::sync::watch::channel(server::Cache::Remove);
    // The self-test's workers are its own, never the system's.
    #[cfg(target_os = "macos")]
    if !self_test() {
        handle.spawn(finder::follow(cache_writes.clone()));
    }
    handle.spawn(server::write_cache(server::cache_path(), cache_writes));
    let mut tapped = slopty_platform::notify::taps();
    let notifier = notifier();
    let workspace = cx.new(|cx| {
        let mut ws =
            Workspace::new(view, handle, settings_path, settings_seen, directory_cache, cx);
        ws.set_notifier(notifier);
        ws
    });
    // What the file tiles hold unsaved is written as the app quits, before anything goes.
    let quitting = workspace.clone();
    cx.on_app_quit(move |cx| {
        quitting.update(cx, |ws, cx| {
            ws.view.update(cx, |v, cx| {
                v.keep_unsaved_now(cx);
                // Where the windows stand changed a moment ago, maybe: written before the wait.
                v.save_layout_now();
            });
        });
        async {}
    })
    .detach();
    let window = window::open(&workspace, options(cx), Some(loaded), cx)?;
    window::install(&workspace, options, cx);
    let drawn = cx.background_executor().timer(SYMBOLS_DRAWN_WITHIN);
    cx.background_spawn(async move {
        drawn.await;
        slopty_ui::icons::remember(&symbols);
    })
    .detach();
    // GPUI's own animations hold still as the system asks, as Slopty's do, and follow the
    // setting as the system says it changed ([`Workspace::set_reduce_motion`]).
    cx.set_reduce_motion(slopty_platform::reduce_motion());
    watch_reduce_motion(&workspace, cx);
    watch_resumes(&workspace, &slopty_platform::resume::System, cx);
    hangs::watch(cx);
    // A self-test reads no network.
    #[cfg(target_os = "macos")]
    if !self_test() {
        update::watch(&workspace, cx);
    }
    // Settings' "Open at login" is the system's login item; a self-test leaves it alone.
    #[cfg(target_os = "macos")]
    if !self_test() {
        cx.set_global(slopty_ui::settings_form::LoginItem {
            read: std::sync::Arc::new(slopty_platform::login::status),
            set: std::sync::Arc::new(slopty_platform::login::set),
            open_settings: Rc::new(slopty_platform::login::open_settings),
        });
    }
    watch_settings(workspace.clone(), cx);
    // A tapped note brings the app forward on its tile, on whichever worker it lives. The tap
    // that launched the app waited for `taps` above and arrives first, once the window is up.
    let for_notifications = workspace.clone();
    cx.spawn(async move |cx| {
        while let Some(tap) = tapped.recv().await {
            // "Allow" and "Deny" answer where the note is, leaving the app where it was.
            let verdict = matches!(
                tap.action.as_deref(),
                Some(slopty_platform::notify::ALLOW | slopty_platform::notify::DENY)
            );
            cx.update(|cx| {
                if !verdict {
                    // The window comes back to show the tile, if it was closed.
                    window::show(cx);
                }
                let Some(window) = for_notifications.read(cx).window else { return };
                let _handled = window.update(cx, |_root, _window, cx| {
                    for_notifications.update(cx, |ws, cx| ws.open_notification(&tap, cx));
                });
            });
        }
    })
    .detach();
    // A link the system opened Slopty with ([`invite`]): the person's Camera read a code.
    let mut links = invite::links();
    let for_links = workspace.clone();
    cx.spawn(async move |cx| {
        while let Some(url) = links.recv().await {
            cx.update(|cx| {
                let Some(window) = for_links.read(cx).window else { return };
                let _handled = window.update(cx, |_root, window, cx| {
                    for_links.update(cx, |ws, cx| ws.open_link(&url, window, cx));
                });
            });
        }
    })
    .detach();
    // The server's (cached) directory has been read by the settings above; with no server set,
    // the first run's page.
    window.update(cx, |_root, window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.refresh_menu(cx);
            if ws.server.is_none() {
                ws.show_add_worker(Panel::Server, window, cx);
            }
        });
    })?;
    // The self-test socket, for `cargo xtask e2e app`; never set for a normal launch.
    #[cfg(feature = "e2e")]
    if let Some(socket) = std::env::var_os(slopty_e2e::SOCKET_ENV) {
        let runtime = workspace.read(cx).runtime.clone();
        e2e::serve(socket.into(), workspace, window.into(), &runtime, cx);
    }
    Ok(())
}

/// Where notes go: the system's notification centre, or, under the self-test, nowhere a person
/// would see them (its window is never in front, so everything would notify).
fn notifier() -> Rc<dyn Notifier> {
    if self_test() {
        Rc::new(slopty_platform::notify::Memory::default())
    } else {
        Rc::new(slopty_platform::notify::System::new())
    }
}

/// The pasteboard the clipboard is shared through: the general one, or the one named by
/// `SLOPTY_PASTEBOARD` (the self-tests', so no test touches the human's clipboard).
#[cfg(target_os = "macos")]
fn pasteboard() -> Rc<dyn slopty_platform::pasteboard::Pasteboard> {
    use slopty_platform::pasteboard::MacPasteboard;
    match std::env::var("SLOPTY_PASTEBOARD") {
        Ok(name) if !name.is_empty() => {
            tracing::info!(%name, "clipboard on a named pasteboard");
            Rc::new(MacPasteboard::named(&name))
        }
        _ => Rc::new(MacPasteboard::general()),
    }
}

/// [`pasteboard`] on iOS: `UIPasteboard`, the app's own named one under the self-test. A
/// self-test that names none still gets one of its own: the simulator's general pasteboard
/// follows the Mac's.
#[cfg(target_os = "ios")]
fn pasteboard() -> Rc<dyn slopty_platform::pasteboard::Pasteboard> {
    use slopty_platform::pasteboard::IosPasteboard;
    let named =
        std::env::var("SLOPTY_PASTEBOARD").ok().filter(|name| !name.is_empty()).or_else(|| {
            self_test().then(|| format!("com.aislopware.slopty.self-test.{}", std::process::id()))
        });
    if let Some(board) = named.as_deref().and_then(IosPasteboard::named) {
        tracing::info!(name = ?named, "clipboard on a named pasteboard");
        return Rc::new(board);
    }
    Rc::new(IosPasteboard::general())
}

/// Follow the system's Reduce Motion as it changes: the watch hands each change to the app.
fn watch_reduce_motion(workspace: &Entity<Workspace>, cx: &mut App) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let watch = slopty_platform::motion::watch_reduce_motion(move |on| {
        let _closed = tx.send(on);
    });
    workspace.update(cx, |ws, _cx| ws.motion_watch = Some(watch));
    let workspace = workspace.downgrade();
    cx.spawn(async move |cx| {
        while let Some(on) = rx.recv().await {
            if workspace.update(cx, |ws, cx| ws.set_reduce_motion(on, cx)).is_err() {
                return;
            }
        }
    })
    .detach();
}

/// Hear what may have killed the links under the app (the Mac waking, an unlock, a return to
/// the front, a path change) from `source`, on the main thread ([`Workspace::resume`]).
fn watch_resumes(
    workspace: &Entity<Workspace>,
    source: &dyn slopty_platform::resume::Source,
    cx: &mut App,
) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let watch = source.watch(std::sync::Arc::new(move |resume| {
        let _closed = tx.send(resume);
    }));
    workspace.update(cx, |ws, _cx| ws.resume_watch = Some(watch));
    let workspace = workspace.downgrade();
    cx.spawn(async move |cx| {
        while let Some(resume) = rx.recv().await {
            if workspace.update(cx, |ws, _cx| ws.resume(resume)).is_err() {
                return;
            }
        }
    })
    .detach();
}

/// Reload `settings.toml` whenever its stamp changes (see [`settings`] for why this polls),
/// and notice a hardware keyboard coming or going on the same tick. The app's own saves are
/// already applied and seen ([`settings::save`]), so they are not reloaded.
fn watch_settings(workspace: Entity<Workspace>, cx: &App) {
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(settings::POLL).await;
            let keyboard = hardware_keyboard_attached();
            workspace.update(cx, |ws, cx| {
                ws.set_hardware_keyboard(keyboard, cx);
                if ws.settings_seen.changed(&ws.settings_path) {
                    tracing::info!(path = %ws.settings_path.display(), "settings changed; reloading");
                    let loaded = Settings::load(&ws.settings_path);
                    ws.apply_loaded(loaded, cx);
                    // An open dialog follows the file, so it never writes a value back over it.
                    if let Some(editor) = ws.settings_editor.clone() {
                        let text = settings::editable_text(&ws.settings_path);
                        editor.update(cx, |e, cx| e.follow_file(text, cx));
                    }
                }
            });
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext, point, size};

    use super::*;

    #[test]
    fn the_key_bar_is_for_glass_without_a_keyboard() {
        assert!(key_bar_visible(true, false));
        assert!(!key_bar_visible(true, true));
        assert!(!key_bar_visible(false, false));
        assert!(!key_bar_visible(false, true));
    }

    /// A cap's words are sentence case ("Esc", not "esc"), and a screen reader hears each key
    /// by name, never its glyph, with an armed modifier saying so.
    #[test]
    fn every_key_is_spoken_by_name_and_shown_in_sentence_case() {
        for (label, ..) in BAR_KEYS.iter().chain(&SCREEN_BAR_KEYS) {
            let spoken = key_spoken(label);
            assert!(spoken.chars().next().is_some_and(char::is_uppercase), "{label}: {spoken}");
            assert!(spoken.chars().count() > 1, "{label} is named, not drawn");
            assert!(!label.starts_with(char::is_lowercase), "{label} is sentence case");
        }
        assert_eq!(key_label(CONTROL_CAP, true), "Control, armed");
        assert_eq!(key_label(ALT_CAP, false), "Alt");
        assert_eq!(key_label("Esc", false), "Escape");
    }

    /// A key row that fits has no fade; one that runs past the edge fades where keys remain,
    /// the leading end once it is scrolled off the start.
    #[test]
    fn the_key_row_fades_where_keys_run_past_the_edge() {
        assert_eq!(key_bar_fades(0.0, 0.0), (false, false), "a row that fits");
        assert_eq!(key_bar_fades(0.0, 180.0), (false, true), "at the start, more to come");
        assert_eq!(key_bar_fades(90.0, 180.0), (true, true), "keys past both ends");
        assert_eq!(key_bar_fades(179.8, 180.0), (true, false), "at the end");
        assert_eq!(key_bar_fades(300.0, 180.0), (true, false), "kept from a wider row");
    }

    /// A phone's row knows it runs past the edge before it is laid out, so its first frame
    /// already fades the trailing end; an iPad's spreads out and never scrolls.
    #[test]
    fn the_key_row_knows_its_overflow_before_layout() {
        let spacing = Theme::default().spacing;
        let terminal = || BAR_KEYS.iter().map(|(label, ..)| *label).chain(["Paste", "Find"]);
        let phone = key_row_overflow(terminal(), 402.0, spacing);
        let line = key_row_width(terminal(), spacing);
        assert!((phone - (line - 402.0)).abs() < f32::EPSILON, "{phone} of {line}");
        assert_eq!(key_bar_fades(0.0, phone), (false, true));
        assert!(key_row_overflow(terminal(), 820.0, spacing).abs() < f32::EPSILON, "an iPad");
    }

    /// The caps keep their widths: the terminal's row runs past a 402 pt phone, so it scrolls,
    /// and fits an 11-inch iPad (820 pt) with room to spare rather than stretching to it.
    #[test]
    fn the_key_caps_keep_their_width() {
        let spacing = Theme::default().spacing;
        let labels = BAR_KEYS.iter().map(|(label, ..)| *label).chain(["Paste", "Find"]);
        let row = key_row_width(labels, spacing);
        assert!(row > 402.0 && row < 820.0, "{row}");
        assert!((cap_width("|", spacing) - cap_side(spacing)).abs() < f32::EPSILON, "square");
        assert!(cap_side(spacing) < KEY_BAR_H, "a cap sits inside its bar");
    }

    /// Every bar fits an 11-inch iPad (820 pt) as one run of groups with its word keys
    /// trailing (a remote window's the narrowest, 744 pt, too), and a phone's (402 pt) never
    /// does, so it scrolls.
    #[test]
    fn every_bar_spreads_on_an_ipad_and_scrolls_on_a_phone() {
        let spacing = Theme::default().spacing;
        let terminal = || BAR_KEYS.iter().map(|(label, ..)| *label).chain(["Paste", "Find"]);
        let screen = || SCREEN_BAR_KEYS.iter().map(|(label, ..)| *label).chain(["Copy", "Paste"]);
        for (row, ipad) in
            [(spread_width(terminal(), spacing), 820.0), (spread_width(screen(), spacing), 744.0)]
        {
            assert!(row <= ipad && row > 402.0, "{row}");
        }
        assert_eq!(key_group(CONTROL_CAP), KeyGroup::Lead, "the soft keyboard's lack");
        assert_eq!(key_group("PgUp"), KeyGroup::Arrows, "paging moves as the arrows do");
        assert_eq!(key_group("Find"), KeyGroup::Words);
        assert_eq!(key_group("|"), KeyGroup::Symbols);
    }

    /// The terminal's key row, drawn from a shell's caps at the window's width.
    struct KeyRow(Entity<Workspace>);

    impl Render for KeyRow {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let width = f32::from(window.viewport_size().width);
            let ws = self.0.read(cx);
            let labels = BAR_KEYS.iter().map(|(label, ..)| *label).chain(["Paste", "Find"]);
            let keys = labels
                .map(|label| {
                    let id = format!("key-{label}");
                    let selector = id.clone();
                    let cap = ws.key_cap(id, label, false).debug_selector(move || selector);
                    (label, cap.child(label).into_any_element())
                })
                .collect();
            div().size_full().child(ws.key_row_of(keys, width).0)
        }
    }

    /// On an iPad the key bar is one bar: Esc, Tab and the modifiers, then the arrows, then the
    /// symbols, one leading run a step apart, the word keys at the trailing end; the arrows no
    /// longer float alone in the middle. On a phone it is one line in the given order.
    #[gpui::test]
    fn an_ipad_key_bar_is_one_leading_run_with_the_word_keys_trailing(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ws = workspace(cx, &runtime, &dir);
        let (_row, cx) = cx.add_window_view(|_, _| KeyRow(ws));
        let spacing = Theme::default().spacing;
        cx.simulate_resize(size(px(1032.0), px(200.0)));
        cx.run_until_parked();
        let at = |cx: &mut VisualTestContext, label: &str| {
            // A debug selector is looked up by a `'static` name; a test's few are leaked.
            let selector: &'static str = Box::leak(format!("key-{label}").into_boxed_str());
            cx.debug_bounds(selector).unwrap_or_else(|| panic!("{label} drawn"))
        };
        let gap = |cx: &mut VisualTestContext, before: &str, after: &str| {
            f32::from(at(cx, after).left() - at(cx, before).right())
        };
        assert!((f32::from(at(cx, "Esc").left()) - spacing.xs).abs() < 0.5, "leading");
        assert!((gap(cx, "Tab", CONTROL_CAP) - spacing.xs).abs() < 0.5, "caps in a group");
        assert!((gap(cx, ALT_CAP, "⌘") - spacing.xs).abs() < 0.5, "the modifiers together");
        assert!((gap(cx, "⌘", "←") - spacing.md).abs() < 0.5, "the arrows follow the lead");
        assert!((gap(cx, "→", "PgUp") - spacing.xs).abs() < 0.5, "paging with the arrows");
        assert!((gap(cx, "PgDn", "~") - spacing.md).abs() < 0.5, "the symbols follow them");
        assert!((f32::from(at(cx, "Find").right()) - (1032.0 - spacing.xs)).abs() < 0.5);
        assert!(gap(cx, "-", "Paste") > 200.0, "the word keys trail: {}", gap(cx, "-", "Paste"));

        cx.simulate_resize(size(px(402.0), px(200.0)));
        cx.run_until_parked();
        assert!(cx.debug_bounds("key-bar-spread").is_none(), "a phone's row scrolls");
        assert!((gap(cx, "→", "~") - spacing.xs).abs() < 0.5, "in the given order");
    }

    /// The shell in a headless window with the panel up: with `set_up`, a server and one of
    /// its workers, so it is the dialog that adds a machine over the workspace; else the first
    /// run's page. The runtime and the directory
    /// hold what the shell's tasks and settings file need for the test's length.
    pub(crate) fn shell<'a>(
        cx: &'a mut TestAppContext,
        runtime: &tokio::runtime::Runtime,
        dir: &tempfile::TempDir,
        set_up: bool,
    ) -> (Entity<Workspace>, &'a mut VisualTestContext) {
        let ws = workspace(cx, runtime, dir);
        if set_up {
            ws.update(cx, |ws, _cx| {
                let hub = slopty_net::HostAddr::new("hub", slopty_net::endpoint::SERVER_PORT);
                ws.server = Some(server::ServerSlot::stand_in(hub));
                ws.workers.push(WorkerSlot::new(WorkerId::new(), "studio".to_owned()));
            });
        }
        let root = ws.clone();
        let (_root, cx) =
            cx.add_window_view(move |window, cx| gpui_kit::component::Root::new(root, window, cx));
        cx.update(|window, cx| {
            ws.update(cx, |ws, cx| ws.show_add_worker(Panel::Worker, window, cx));
        });
        cx.run_until_parked();
        (ws, cx)
    }

    /// The server's directory lists `worker` as `name`, as a server would send it.
    pub(crate) fn list(
        ws: &Entity<Workspace>,
        cx: &mut TestAppContext,
        worker: WorkerId,
        name: &str,
    ) {
        use slopty_proto::server::{FromServer, Liveness, Os, WorkerCaps, WorkerInfo};
        let info = WorkerInfo {
            worker,
            name: name.to_owned(),
            address: "100.64.0.2:45550".to_owned(),
            liveness: Liveness::Online,
            caps: WorkerCaps::bare(Os::MacOs),
            load: 0.0,
            last_seen_ms: slopty_core::WallMs::ZERO,
        };
        let listing = FromServer::Worker(info);
        ws.update(cx, |ws, cx| {
            ws.server_event(slopty_client::server::ServerEvent::Message(Box::new(listing)), cx);
        });
        cx.run_until_parked();
    }

    /// Closing the main window leaves the workspace running; asking for the window again (the
    /// Dock icon, ⌘N, Window ▸ Slopty) opens a new one on the same workspace, where the last one
    /// stood; with a window up, it only comes to the front.
    #[gpui::test]
    fn a_closed_window_opens_again_on_the_same_workspace(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ws = workspace(cx, &runtime, &dir);
        let options: window::MakeOptions = Rc::new(|_| WindowOptions::default());
        cx.update(|cx| {
            window::open(&ws, options(cx), None, cx).expect("the window");
            window::install(&ws, options, cx);
        });
        cx.run_until_parked();
        let first = ws.read_with(cx, |ws, _| ws.window).expect("shown");
        let frame = slopty_client::layout::WindowFrame {
            display: None,
            x: 50.0,
            y: 60.0,
            width: 900.0,
            height: 700.0,
            fullscreen: false,
        };
        ws.update(cx, |ws, cx| ws.view.update(cx, |v, cx| v.set_window_frame(Some(frame), cx)));
        cx.update(|cx| first.update(cx, |_root, window, _cx| window.remove_window())).unwrap();
        cx.run_until_parked();
        assert!(cx.update(|cx| cx.windows().is_empty()), "closed");

        cx.update(window::show);
        cx.run_until_parked();
        let windows = cx.update(|cx| cx.windows());
        let second = ws.read_with(cx, |ws, _| ws.window).expect("shown again");
        assert_eq!(windows, [second], "one window, the new one");
        assert_ne!(second, first);
        let size = cx
            .update(|cx| second.update(cx, |_r, window, _cx| window.window_bounds().get_bounds()))
            .expect("open")
            .size;
        #[expect(clippy::cast_possible_truncation, reason = "whole points")]
        let size = (f32::from(size.width) as i32, f32::from(size.height) as i32);
        assert_eq!(size, (900, 700), "where the last one stood");

        cx.update(window::show);
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| cx.windows()), [second], "brought to the front, not opened");
    }

    /// A deployer for a test that only opens the SSH sheet: nothing is ever deployed.
    #[derive(Debug)]
    struct NoDeploys;

    impl ssh::Deployer for NoDeploys {
        fn deploy(
            &self,
            _to: &ssh::Target,
            _server: slopty_deploy::Server,
            _said: ssh::Said,
            _events: tokio::sync::mpsc::UnboundedSender<slopty_deploy::Event>,
        ) -> this_mac::Pending<Result<slopty_deploy::Deployed, slopty_deploy::Failure>> {
            panic!("this test deploys nothing")
        }

        fn remember(&self, _worker: WorkerId, _to: &ssh::Target) {}

        fn target_of(&self, _worker: WorkerId) -> Option<ssh::Target> {
            None
        }
    }

    /// A worker the tailnet found that the server does not list is a row on the machine's
    /// panel; pressing it opens the SSH sheet on its host, to install this build there
    /// registered with the server (`ssh::tests` checks the sheet's host field).
    #[gpui::test]
    fn a_worker_the_server_does_not_list_opens_the_install_on_its_host(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, true);
        cx.simulate_resize(size(px(900.0), px(800.0)));
        ws.update(cx, |ws, _cx| ws.deployer = Some(Rc::new(NoDeploys)));
        let offer = Offer {
            name: "mini.tail1234.ts.net".to_owned(),
            at: "100.64.0.7".to_owned(),
            tags: Vec::new(),
            answer: slopty_net::discover::Answer::OtherBuild("0.0.1".to_owned()),
        };
        cx.update(|window, cx| {
            ws.update(cx, |ws, cx| ws.press_found(Host::Worker, &offer, window, cx));
        });
        cx.run_until_parked();
        let sheet = ws.read_with(cx, |ws, _cx| ws.adding.as_ref().is_some_and(|a| a.ssh.is_some()));
        assert!(sheet && cx.debug_bounds("ssh-form").is_some(), "the sheet, drawn");
    }

    /// Where no machine can be added (an iPhone or iPad: no Mac to share, no SSH, no tailnet to
    /// list), the machine's panel says machines are added from a Mac, with no line about an
    /// address it has no field for, and offers another server instead, whose panel takes one.
    #[gpui::test]
    fn a_device_that_adds_no_machine_says_they_are_added_from_a_mac(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, true);
        cx.simulate_resize(size(px(402.0), px(800.0)));
        cx.update(|window, cx| {
            ws.update(cx, |ws, cx| {
                // As on iOS: no Mac to share, no SSH, and nothing lists the tailnet.
                ws.this_mac = None;
                ws.deployer = None;
                ws.show_add_worker(Panel::Worker, window, cx);
                if let Some(adding) = &mut ws.adding {
                    adding.search = None;
                }
                cx.notify();
            });
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("add-worker-from-a-mac").is_some(), "where machines are added");
        assert!(cx.debug_bounds("add-worker-unlisted").is_none(), "no address to type here");
        assert!(cx.debug_bounds("add-worker-field").is_none());
        let other = cx.debug_bounds("connect-another-server").expect("another server");
        cx.simulate_click(other.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        let mode = ws.read_with(cx, |ws, _| ws.adding.as_ref().map(|a| a.mode));
        assert_eq!(mode, Some(Panel::Server), "the server's panel");
        assert!(cx.debug_bounds("add-worker-field").is_some(), "with its address");
        assert!(cx.debug_bounds("add-worker-from-a-mac").is_none());
    }

    /// Both panels lead with this Mac, the likeliest first step; the server's panel then takes
    /// an address or sets up a server over SSH, and the machine's panel installs one over SSH,
    /// with no address to type: a machine joins through the server, never by its address.
    #[gpui::test]
    fn the_server_panel_offers_this_mac_and_a_server_to_set_up(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, true);
        cx.simulate_resize(size(px(900.0), px(800.0)));
        let host: Rc<dyn this_mac::Host> = StandIn::answering(None);
        ws.update(cx, |ws, _cx| {
            ws.deployer = Some(Rc::new(NoDeploys));
            ws.this_mac = Some(host);
        });
        let shown = |cx: &mut VisualTestContext, panel: Panel| {
            cx.update(|window, cx| ws.update(cx, |ws, cx| ws.show_add_worker(panel, window, cx)));
            cx.run_until_parked();
            ["use-this-mac", "serve-over-ssh", "install-over-ssh", "add-worker-field"]
                .map(|row| cx.debug_bounds(row).is_some())
        };
        assert_eq!(shown(cx, Panel::Server), [true, true, false, true], "the server's rows");
        assert_eq!(shown(cx, Panel::Worker), [true, false, true, false], "the worker's rows");
    }

    /// The shell with no window, no worker and no panel.
    pub(crate) fn workspace(
        cx: &mut TestAppContext,
        runtime: &tokio::runtime::Runtime,
        dir: &tempfile::TempDir,
    ) -> Entity<Workspace> {
        cx.update(|cx| {
            gpui_kit::init(cx);
            slopty_ui::keymap::install(keymap_for(&slopty_settings::KeySettings::default()), cx);
        });
        let path = dir.path().join("settings.toml");
        let (cache, _writes) = tokio::sync::watch::channel(server::Cache::Remove);
        let handle = runtime.handle().clone();
        let view = cx.new(|cx| {
            let mut view = WorkspaceView::new(Theme::default(), None, cx);
            view.set_animation(false);
            view
        });
        let seen = settings::Seen::of(&path);
        cx.new(|cx| Workspace::new(view, handle, path, seen, cache, cx))
    }

    /// The first run's block sits a third of the way down the room over its foot on every
    /// device, which a percentage pad (resolved against the width) did not: 42 % on the Mac,
    /// 12 % on a phone. The foot says why there is nothing to pair, on the block's left edge.
    #[gpui::test]
    fn the_first_run_sits_a_third_down_over_its_foot_on_every_device(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, false);
        assert!(ws.read_with(cx, |ws, _| ws.welcome()));
        for (w, h) in [(900.0, 600.0), (1280.0, 800.0), (1024.0, 1366.0), (402.0, 874.0)] {
            cx.simulate_resize(size(px(w), px(h)));
            cx.run_until_parked();
            let panel = cx.debug_bounds("add-worker").expect("the panel is drawn");
            let foot = cx.debug_bounds("add-worker-foot").expect("the foot is drawn");
            let above = f32::from(panel.top());
            let below = f32::from(foot.top() - panel.bottom());
            assert!(
                2.0_f32.mul_add(-above, below).abs() < 1.0,
                "{w}×{h}: {above} above, {below} below"
            );
            assert!((f32::from(foot.left() - panel.left())).abs() < 0.5, "one left edge");
            assert!(f32::from(foot.bottom()) <= h, "{w}×{h}: the foot is on the page");
        }
    }

    /// Over the workspace the panel is a dialog, and it has no foot: the note is the first
    /// run's. From a tablet's width the field and its action share a row, the field taking the
    /// room; under it (a phone, a Split View column) the action is the field's width under it,
    /// on the first run and in the dialog alike, whatever the input.
    #[gpui::test]
    fn the_field_and_its_action_share_a_row_from_a_tablets_width(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, true);
        cx.update(|window, cx| {
            ws.update(cx, |ws, cx| ws.show_add_worker(Panel::Server, window, cx));
        });
        cx.simulate_resize(size(px(900.0), px(600.0)));
        cx.run_until_parked();
        assert!(cx.debug_bounds("add-worker-foot").is_none(), "a dialog has no foot");
        assert!(cx.debug_bounds("app-brand").is_none(), "nor the app's name");
        let sizes = [
            (900.0, 600.0, false),
            (560.0, 800.0, false),
            (1024.0, 1366.0, true),
            (402.0, 874.0, true),
        ];
        for (w, h, welcome) in sizes {
            if welcome && !ws.read_with(cx, |ws, _| ws.welcome()) {
                cx.update(|window, cx| {
                    ws.update(cx, |ws, cx| {
                        ws.server = None;
                        ws.adding = None;
                        ws.show_add_worker(Panel::Server, window, cx);
                    });
                });
            }
            cx.simulate_resize(size(px(w), px(h)));
            cx.run_until_parked();
            assert_eq!(ws.read_with(cx, |ws, _| ws.welcome()), welcome);
            let field = cx.debug_bounds("add-worker-field").expect("the field");
            let go = cx.debug_bounds("add").expect("its action");
            if w >= FIELD_ROW_FROM {
                assert!((f32::from(field.top() - go.top())).abs() < 0.5, "{w}: {field:?} {go:?}");
                assert!((f32::from(field.size.height - go.size.height)).abs() < 0.5, "one height");
                assert!(field.right() < go.left(), "the action after the field");
                assert!(field.size.width > go.size.width * 3.0, "the field takes the room");
            } else {
                assert!(field.bottom() < go.top(), "{w}: the action under the field");
                assert!((f32::from(field.size.width - go.size.width)).abs() < 0.5, "full width");
            }
        }
    }

    /// The dialog fades in where it stands as its scrim dims in, with no travel, and so under
    /// Reduce Motion: in place on its first frame either way.
    #[gpui::test]
    fn the_dialog_fades_in_where_it_stands(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, true);
        cx.simulate_resize(size(px(900.0), px(600.0)));
        let first_top = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                ws.update(cx, |ws, cx| {
                    ws.cancel_add_worker(window, cx);
                });
            });
            cx.run_until_parked();
            cx.update(|window, cx| {
                ws.update(cx, |ws, cx| ws.show_add_worker(Panel::Worker, window, cx));
            });
            cx.run_until_parked();
            f32::from(cx.debug_bounds("add-worker").expect("the dialog is drawn").top())
        };
        cx.update(|_window, cx| cx.set_reduce_motion(true));
        let still = first_top(cx);
        assert!(
            600.0_f32.mul_add(-kit::MODAL_ANCHOR, still).abs() < 0.5,
            "in place at once: {still}"
        );
        cx.update(|_window, cx| cx.set_reduce_motion(false));
        let fading = first_top(cx);
        assert!((fading - still).abs() < 0.5, "no travel on its first frame: {fading}");
    }

    /// A change the workspace view hears of builds that view, not the app's root over it.
    #[gpui::test]
    fn the_root_is_not_built_for_the_views_news(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, true);
        let view = ws.read_with(cx, |ws, _| ws.view.clone());
        cx.update(|window, cx| ws.update(cx, |ws, cx| ws.cancel_add_worker(window, cx)));
        cx.run_until_parked();
        // The panel's going is still settling into the view: news the root does draw.
        view.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        let before = ws.read_with(cx, |ws, _| ws.renders);
        for _ in 0..3 {
            view.update(cx, |_, cx| cx.notify());
            cx.run_until_parked();
        }
        assert_eq!(ws.read_with(cx, |ws, _| ws.renders), before, "the root replayed");
    }

    /// ⌘, focuses the settings editor as the root draws: the frame that follows is the one a
    /// window drawn from scratch shows, not the last one's with the old focus in it.
    #[gpui::test]
    fn the_settings_editor_taking_the_keyboard_leaves_no_stale_frame(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, true);
        // The dialog lands at once, so the frame holds still to be judged.
        cx.update(|_window, cx| cx.set_reduce_motion(true));
        cx.update(|window, cx| ws.update(cx, |ws, cx| ws.cancel_add_worker(window, cx)));
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-,");
        cx.run_until_parked();
        assert!(ws.read_with(cx, |ws, _| ws.settings_editor.is_some()), "the editor is open");
        let quads = |window: &Window| {
            let mut lines: Vec<String> = window
                .painted_quads()
                .iter()
                .map(|q| {
                    format!(
                        "{:?} {:?} {:?} {:?}",
                        q.bounds, q.background, q.border_color, q.border_widths
                    )
                })
                .collect();
            lines.sort_unstable();
            lines
        };
        let shown = cx.update(|window, _cx| quads(window));
        let scratch = cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            quads(window)
        });
        let only_shown: Vec<&String> = shown.iter().filter(|l| !scratch.contains(l)).collect();
        assert!(only_shown.is_empty(), "painted with the old focus: {only_shown:#?}");
    }

    /// "About Slopty", in the app menu and the palette, opens the settings on their About page,
    /// where the version and the build are: no panel of its own says them again.
    #[gpui::test]
    fn about_slopty_opens_the_settings_on_their_about_page(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, true);
        cx.update(|window, cx| ws.update(cx, |ws, cx| ws.cancel_add_worker(window, cx)));
        cx.run_until_parked();
        cx.dispatch_action(OpenAbout);
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| slopty_ui::a11y::tree(window));
        let about = tree.iter().find(|n| n.is("Group", Some("About"))).expect("the About page");
        let said = about.description.as_deref().unwrap_or_default();
        assert!(said.contains(env!("CARGO_PKG_VERSION")), "{said}");
        assert!(app_palette_items().iter().any(|item| item.label == ABOUT), "the palette's line");
    }

    /// The Keyboard page names the app's own commands as its palette does ("Update all
    /// machines"), not by their names in the file.
    #[gpui::test]
    fn the_keyboard_page_names_the_app_s_commands_in_the_palette_s_words(cx: &mut TestAppContext) {
        use slopty_ui::settings_form::schema::Section;
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, true);
        cx.update(|window, cx| ws.update(cx, |ws, cx| ws.cancel_add_worker(window, cx)));
        cx.update(|window, cx| ws.update(cx, |ws, cx| ws.open_settings(window, cx)));
        cx.run_until_parked();
        let tab = format!("settings-section-{}", Section::Keyboard.index());
        let at = cx.debug_bounds(Box::leak(tab.into_boxed_str())).expect("the Keyboard tab");
        cx.simulate_click(at.center(), gpui::Modifiers::none());
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| slopty_ui::a11y::tree(window));
        let listed = |label: &str| tree.iter().any(|n| n.is("ListItem", Some(label)));
        assert!(listed(ssh::UPDATE_ALL), "{tree:#?}");
        assert!(listed(finder::TITLE) == finder::OFFERED, "{tree:#?}");
        assert!(!listed("Update all workers"), "not its name in the file");
    }

    /// Esc while an input method composes in the address field is the input method's: the
    /// panel stays. Once the word is committed, Esc closes it.
    #[gpui::test]
    fn esc_mid_word_leaves_the_panel_open(cx: &mut TestAppContext) {
        use gpui::EntityInputHandler as _;
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, true);
        cx.update(|window, cx| {
            ws.update(cx, |ws, cx| ws.show_add_worker(Panel::Server, window, cx));
        });
        cx.run_until_parked();
        let address = ws.read_with(cx, |ws, _| ws.adding.as_ref().map(|a| a.address.clone()));
        let Some(address) = address else { panic!("the panel is open") };
        cx.update(|window, cx| {
            address.update(cx, |field, cx| {
                field.focus(window, cx);
                field.replace_and_mark_text_in_range(None, "s", Some(1..1), window, cx);
            });
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(ws.read_with(cx, |ws, _| ws.adding.is_some()), "the input method took Esc");
        cx.update(|window, cx| {
            address.update(cx, |field, cx| field.replace_text_in_range(None, "s", window, cx));
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(ws.read_with(cx, |ws, _| ws.adding.is_none()), "Esc closed the panel");
    }

    /// The first run is the server's panel whichever was asked for: with no server there is
    /// nothing to add a machine to. While it looks on the tailnet it says so; each server that
    /// answered is a row to connect to under the blurb and over the field, the best one's
    /// address in the field; when none did it says that, and with no tailnet here it says so.
    /// The first run carries the app's name over its heading.
    #[gpui::test]
    fn the_panel_names_its_host_and_the_server_the_tailnet_found(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, false);
        let example = |cx: &mut VisualTestContext| {
            ws.read_with(cx, |ws, cx| {
                ws.adding
                    .as_ref()
                    .map(|a| a.address.read(cx).presentation().placeholder().to_string())
            })
        };
        assert_eq!(example(cx).as_deref(), Some(SERVER_EXAMPLE));
        assert_eq!(
            ws.read_with(cx, |ws, _| ws.adding.as_ref().map(|a| a.mode)),
            Some(Panel::Server)
        );

        // The look is on screen from the start, then each server that answered is a row.
        let words = |cx: &mut VisualTestContext| {
            cx.run_until_parked();
            let shown = cx.debug_bounds("add-worker-search").is_some();
            let search =
                ws.read_with(cx, |ws, _| ws.adding.as_ref().and_then(|a| a.search.clone()));
            search
                .as_ref()
                .and_then(|s| s.words(Panel::Server))
                .filter(|_| shown)
                .map(str::to_owned)
        };
        let looking = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| {
            ws.update(cx, |ws, cx| {
                if let Some(adding) = &mut ws.adding {
                    adding.search = Some(Search::Looking);
                }
                cx.notify();
            });
        };
        looking(&ws, cx);
        assert_eq!(words(cx).as_deref(), Some(LOOKING));
        let found = |name: &str, last: u8| slopty_net::discover::Found {
            name: name.to_owned(),
            addr: std::net::SocketAddr::from(([100, 64, 0, last], 7_000)),
            tags: Vec::new(),
            answer: Some(slopty_net::discover::Answer::Ready),
        };
        cx.update(|window, cx| {
            let servers = vec![found("home-server", 1), found("office-server", 2)];
            let answered = net::Tailnet { servers, workers: Vec::new(), running: true };
            ws.update(cx, |ws, cx| ws.offer_found(answered, window, cx));
        });
        cx.run_until_parked();
        let first = cx.debug_bounds("add-worker-found-0").expect("a row to connect to");
        let second = cx.debug_bounds("add-worker-found-1").expect("every server a row");
        assert!(first.bottom() <= second.top(), "best first");
        let field = cx.debug_bounds("add-worker-blurb").expect("the blurb stays");
        assert!(field.bottom() <= first.top(), "under what the panel is for");
        let brand = cx.debug_bounds("app-brand").expect("the first run carries the name");
        assert!(brand.bottom() <= field.top(), "over the heading");
        let mark = cx.debug_bounds("app-mark").expect("led by the app's mark");
        assert!((f32::from(mark.size.width) - kit::BRAND_MARK).abs() < 0.5, "{mark:?}");
        assert!(mark.left() <= brand.left() + px(0.5), "its name beside it: {mark:?} {brand:?}");
        let label = cx.debug_bounds("add-worker-label").expect("the field's label");
        assert!(second.bottom() <= label.top(), "under the rows");
        let named =
            ws.read_with(cx, |ws, _| ws.adding.as_ref().map(|a| address_label(a.search.as_ref())));
        assert_eq!(named, Some("Or type an address"), "the other way, under the rows");
        let address = cx.debug_bounds("add-worker-field").expect("the field");
        assert!(second.bottom() <= address.top(), "the rows over the field");
        assert_eq!(words(cx), None, "done looking");
        let typed = ws.read_with(cx, |ws, cx| {
            ws.adding.as_ref().map(|a| a.address.read(cx).value().to_string())
        });
        assert_eq!(typed.as_deref(), Some("100.64.0.1"), "the best one's address, ready");

        // Nothing answering says so, and leaves what is in the field; with no tailnet to look
        // on, it says that instead.
        looking(&ws, cx);
        cx.update(|window, cx| {
            let empty = net::Tailnet { running: true, ..net::Tailnet::default() };
            ws.update(cx, |ws, cx| ws.offer_found(empty, window, cx));
        });
        assert_eq!(words(cx).as_deref(), Some(NO_SERVER_FOUND));
        cx.run_until_parked();
        let row = cx.debug_bounds("add-worker-search").expect("the scan's row");
        let again = cx.debug_bounds("add-worker-scan").expect("its way to look again");
        assert!(row.contains(&again.center()), "at the row's end: {row:?} {again:?}");
        let label = cx.debug_bounds("add-worker-tailnet-label").expect("the section's label");
        assert!(label.bottom() <= row.top(), "a section, headed");
        cx.simulate_click(again.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert_eq!(words(cx).as_deref(), Some(LOOKING), "looking again");
        cx.update(|window, cx| {
            let empty = net::Tailnet { running: true, ..net::Tailnet::default() };
            ws.update(cx, |ws, cx| ws.offer_found(empty, window, cx));
        });
        let label =
            ws.read_with(cx, |ws, _| ws.adding.as_ref().map(|a| address_label(a.search.as_ref())));
        assert_eq!(label, Some("Server address"), "no \"or\" with nothing before it");
        looking(&ws, cx);
        cx.update(|window, cx| {
            ws.update(cx, |ws, cx| ws.offer_found(net::Tailnet::default(), window, cx));
        });
        assert_eq!(words(cx).as_deref(), Some(NOT_RUNNING));
        let typed = ws.read_with(cx, |ws, cx| {
            ws.adding.as_ref().map(|a| a.address.read(cx).value().to_string())
        });
        assert_eq!(typed.as_deref(), Some("100.64.0.1"));
    }

    /// The server's panel offers only the servers the tailnet found, the field keeping to them;
    /// a machine's panel offers only the workers its server does not list yet, each a way to
    /// install this build there.
    #[gpui::test]
    fn each_panel_offers_its_own_kind_of_node(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, false);
        let found = |name: &str, last: u8| slopty_net::discover::Found {
            name: name.to_owned(),
            addr: std::net::SocketAddr::from(([100, 64, 0, last], 7_001)),
            tags: Vec::new(),
            answer: Some(slopty_net::discover::Answer::Ready),
        };
        let answer = |ws: &Entity<Workspace>, tailnet: net::Tailnet, cx: &mut VisualTestContext| {
            ws.update(cx, |ws, _cx| {
                if let Some(adding) = &mut ws.adding {
                    adding.search = Some(Search::Looking);
                }
            });
            cx.update(|window, cx| ws.update(cx, |ws, cx| ws.offer_found(tailnet, window, cx)));
            cx.run_until_parked();
        };
        let tailnet = net::Tailnet {
            servers: vec![found("home-server", 1)],
            workers: vec![found("mac-studio", 2), found("macbook", 3)],
            running: true,
        };
        let offers = |cx: &mut VisualTestContext| {
            ws.read_with(cx, |ws, _| {
                let adding = ws.adding.as_ref()?;
                let search = adding.search.as_ref()?;
                Some(
                    search
                        .offers(adding.mode)
                        .iter()
                        .map(|(h, o)| (*h, o.name.clone()))
                        .collect::<Vec<_>>(),
                )
            })
        };
        answer(&ws, tailnet.clone(), cx);
        assert_eq!(offers(cx), Some(vec![(Host::Server, "home-server".to_owned())]));
        assert!(cx.debug_bounds("add-worker-found-1").is_none(), "one row");

        // Workers alone: nothing goes in a server's field, and no "nothing answered".
        let workers_only =
            net::Tailnet { servers: Vec::new(), workers: tailnet.workers.clone(), running: true };
        cx.update(|window, cx| {
            ws.update(cx, |ws, cx| {
                if let Some(adding) = &ws.adding {
                    adding.address.update(cx, |input, cx| input.set_value("", window, cx));
                }
            });
        });
        answer(&ws, workers_only, cx);
        let field = ws.read_with(cx, |ws, cx| {
            ws.adding.as_ref().map(|a| a.address.read(cx).value().to_string())
        });
        assert_eq!(field.as_deref(), Some(""), "a server's field takes no worker");
        assert_eq!(offers(cx), Some(Vec::new()));

        // With a server, a machine's panel offers the workers it does not list.
        let listed = WorkerId::new();
        cx.update(|window, cx| {
            ws.update(cx, |ws, cx| {
                ws.server = Some(server::ServerSlot::stand_in(slopty_net::HostAddr::new(
                    "hub",
                    slopty_net::endpoint::SERVER_PORT,
                )));
                ws.adding = None;
                ws.show_add_worker(Panel::Worker, window, cx);
            });
        });
        // `list` puts it at 100.64.0.2, where the tailnet found mac-studio.
        list(&ws, cx, listed, "mac-studio");
        answer(&ws, tailnet, cx);
        assert_eq!(offers(cx), Some(vec![(Host::Worker, "macbook".to_owned())]), "unlisted only");
        assert!(cx.debug_bounds("add-worker-found-0").is_some(), "a row to press");
    }

    /// A node that turned this device away, or answered on another build, is a row that says
    /// so rather than nothing on the tailnet: the field takes the best node that is ready, a
    /// refused one's press puts the grant for that node on the clipboard, by its tags or else its
    /// address, and says where it goes.
    #[gpui::test]
    fn a_node_that_needs_a_grant_shows_and_copies_its_grant(cx: &mut TestAppContext) {
        use slopty_net::discover::{Answer, Found};
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, false);
        cx.update(|window, cx| {
            ws.update(cx, |ws, cx| ws.show_add_worker(Panel::Server, window, cx));
        });
        let found = |name: &str, last: u8, tags: &[&str], answer: Answer| Found {
            name: name.to_owned(),
            addr: std::net::SocketAddr::from(([100, 64, 0, last], 7_000)),
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            answer: Some(answer),
        };
        let tailnet = net::Tailnet {
            servers: vec![
                found("hub", 1, &["tag:slopty-server"], Answer::NotGranted),
                found("old-hub", 2, &[], Answer::OtherBuild("0.0.9+wire.0badf00d".to_owned())),
                found("home", 3, &[], Answer::Ready),
            ],
            workers: Vec::new(),
            running: true,
        };
        cx.update(|window, cx| ws.update(cx, |ws, cx| ws.offer_found(tailnet, window, cx)));
        cx.run_until_parked();
        let field = ws.read_with(cx, |ws, cx| {
            ws.adding.as_ref().map(|a| a.address.read(cx).value().to_string())
        });
        assert_eq!(field.as_deref(), Some("100.64.0.3"), "the field takes the one that is ready");
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        let said = cx.update(|window, _cx| slopty_ui::a11y::tree(window));
        for (label, description) in [
            ("Copy the tailnet grant for hub", "Server, needs a tailnet grant"),
            ("Install this build on old-hub", "Server, runs a different build"),
            ("Connect to home", "Server, 100.64.0.3"),
        ] {
            let node = said.iter().find(|n| n.label.as_deref() == Some(label));
            let node = node.unwrap_or_else(|| panic!("no row {label:?} in {said:#?}"));
            assert_eq!(node.description.as_deref(), Some(description), "{label}");
        }

        {
            let (row, dst) = ("add-worker-found-0", "tag:slopty-server");
            let at = cx.debug_bounds(row).expect("the refused node's row");
            cx.simulate_click(at.center(), gpui::Modifiers::none());
            cx.run_until_parked();
            let copied = cx.read_from_clipboard().and_then(|item| item.text()).unwrap_or_default();
            let grant: serde_json::Value = serde_json::from_str(&copied).unwrap();
            assert_eq!(grant["dst"], serde_json::json!([dst]), "{row}");
            assert_eq!(grant["app"][slopty_tailnet::policy::CAP][0]["roles"][0], "client");
            let told = ws.read_with(cx, |ws, cx| ws.view.read(cx).toast_text());
            assert!(told.is_some_and(|t| t.contains("Access controls")), "where it goes");
        }
        let busy = ws.read_with(cx, |ws, _| ws.adding.as_ref().map(|a| a.busy));
        assert_eq!(busy, Some(false), "a refused node is not dialled");
    }

    /// Over the workspace the panel is a dialog: Esc closes it, and so does a click outside
    /// it, while a click inside it does not.
    #[gpui::test]
    fn the_dialog_closes_on_esc_or_a_click_outside(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, true);
        cx.simulate_resize(size(px(900.0), px(600.0)));
        cx.run_until_parked();
        assert!(ws.read_with(cx, |ws, _| ws.adding.is_some() && !ws.welcome()));
        cx.simulate_keystrokes("escape");
        assert!(ws.read_with(cx, |ws, _| ws.adding.is_none()), "Esc closes it");

        let reopen = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                ws.update(cx, |ws, cx| ws.show_add_worker(Panel::Worker, window, cx));
            });
            cx.run_until_parked();
            cx.debug_bounds("add-worker").expect("the dialog is drawn")
        };
        let panel = reopen(cx);
        let inside = point(panel.left() + px(2.0), panel.top() + px(2.0));
        cx.simulate_click(inside, gpui::Modifiers::none());
        assert!(ws.read_with(cx, |ws, _| ws.adding.is_some()), "a click inside keeps it");
        let outside = point(panel.left(), panel.bottom() + px(20.0));
        cx.simulate_click(outside, gpui::Modifiers::none());
        assert!(ws.read_with(cx, |ws, _| ws.adding.is_none()), "a click outside closes it");
    }

    /// A stand-in for this Mac: it records what the flow asked of it and answers with the
    /// `doctor` it is given, so no test installs an agent, prompts or opens System Settings.
    #[derive(Debug, Default)]
    struct StandIn {
        asked: std::cell::RefCell<Vec<String>>,
        doctor: std::cell::RefCell<Option<this_mac::Doctor>>,
        /// What the next installs stop on, first first; an install with none left is done.
        stops: std::cell::RefCell<std::collections::VecDeque<this_mac::Stopped>>,
        /// How notifications stand; allowed when unset.
        notes: std::cell::Cell<Option<this_mac::Alerts>>,
        /// Slopty's place in Finder; a build with no extension when unset.
        finder: std::cell::RefCell<Option<finder::Step>>,
        /// Whether Slopty opens at login; off when unset.
        login: std::cell::Cell<Option<this_mac::Login>>,
    }

    impl StandIn {
        fn answering(doctor: Option<this_mac::Doctor>) -> Rc<Self> {
            Rc::new(Self { doctor: std::cell::RefCell::new(doctor), ..Self::default() })
        }

        fn ask(&self, what: String) {
            self.asked.borrow_mut().push(what);
        }

        fn asked(&self) -> Vec<String> {
            std::mem::take(&mut *self.asked.borrow_mut())
        }
    }

    impl this_mac::Host for StandIn {
        fn install(
            &self,
            serve: &this_mac::Serve,
            end_sessions: bool,
        ) -> this_mac::Pending<Result<slopty_net::HostAddr, this_mac::Stopped>> {
            let ends = if end_sessions { " end_sessions" } else { "" };
            match serve {
                this_mac::Serve::Here => self.ask(format!("install here{ends}")),
                this_mac::Serve::Join(at) => self.ask(format!("install join {at}{ends}")),
            }
            let done = self.stops.borrow_mut().pop_front().map_or_else(|| Ok(serve.address()), Err);
            Box::pin(async move { done })
        }

        fn doctor(&self) -> this_mac::Pending<Option<this_mac::Doctor>> {
            self.ask("doctor".to_owned());
            let doctor = self.doctor.borrow().clone();
            Box::pin(async move { doctor })
        }

        fn restart(&self) -> this_mac::Pending<()> {
            self.ask("restart".to_owned());
            Box::pin(async {})
        }

        fn open(&self, place: this_mac::Place) {
            self.ask(format!("open {place:?}"));
        }

        fn notes(&self) -> this_mac::Pending<this_mac::Alerts> {
            let notes = self.notes.get().unwrap_or(this_mac::Alerts::Allowed);
            Box::pin(async move { notes })
        }

        fn ask_notes(&self) -> this_mac::Pending<this_mac::Alerts> {
            self.ask("ask notes".to_owned());
            self.notes.set(Some(this_mac::Alerts::Allowed));
            Box::pin(async { this_mac::Alerts::Allowed })
        }

        fn finder(&self) -> this_mac::Pending<finder::Step> {
            let step = self.finder.borrow().clone().unwrap_or(finder::Step::Unsigned);
            Box::pin(async move { step })
        }

        fn login(&self) -> this_mac::Pending<this_mac::Login> {
            let login = self.login.get().unwrap_or(this_mac::Login::Off);
            Box::pin(async move { login })
        }

        fn open_at_login(&self) -> this_mac::Pending<Result<this_mac::Login, String>> {
            self.ask("open at login".to_owned());
            let now = match self.login.get() {
                Some(this_mac::Login::Blocked) => this_mac::Login::Blocked,
                _ => this_mac::Login::On,
            };
            self.login.set(Some(now));
            Box::pin(async move { Ok(now) })
        }

        fn move_to_applications(&self) -> this_mac::Pending<Result<std::path::PathBuf, String>> {
            self.ask("move".to_owned());
            Box::pin(async { Err("the test moves nothing".to_owned()) })
        }

        fn repoint(&self) -> this_mac::Pending<Option<Result<(), String>>> {
            self.ask("repoint".to_owned());
            Box::pin(async { None })
        }
    }

    /// A worker's `doctor` with every grant made, linked to its server.
    fn green() -> this_mac::Doctor {
        this_mac::Doctor {
            worker: WorkerId::new(),
            server: Some(slopty_proto::ctl::LinkState::Linked),
            version: "0.3.0".to_owned(),
            screen_recording: true,
            accessibility: true,
            tailnet: this_mac::Tailnet::Reachable("mac-studio.tail1234.ts.net".to_owned()),
            battery: false,
        }
    }

    /// The flow on screen, if any.
    fn flow(ws: &Entity<Workspace>, cx: &VisualTestContext) -> Option<this_mac::Flow> {
        ws.read_with(cx, |ws, _| ws.adding.as_ref().and_then(|a| a.this_mac.clone()))
    }

    /// "Use this Mac" on the first run waits for the look on the tailnet; with no server
    /// answering it starts one here and installs the worker against it, then shows the
    /// worker's `doctor` as the checklist: the missing grant's button opens its own pane. Back
    /// from System Settings the worker is started again and asked again; once it may stream
    /// and take input the flow waits for the server to list it. Its end says this Mac is
    /// ready, with a phone's way in, and Done gives way to the workspace. After that "Add a
    /// machine" keeps this Mac's row as the way back to its checklist, beside a phone's.
    #[gpui::test]
    fn this_mac_runs_the_server_then_waits_for_it_to_list_the_worker(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, false);
        cx.simulate_resize(size(px(900.0), px(700.0)));
        let lacking = this_mac::Doctor { screen_recording: false, ..green() };
        let host = StandIn::answering(Some(lacking.clone()));
        let shared: Rc<dyn this_mac::Host> = Rc::<StandIn>::clone(&host);
        ws.update(cx, |ws, cx| {
            ws.this_mac = Some(shared);
            cx.notify();
        });
        cx.run_until_parked();
        assert!(ws.read_with(cx, |ws, _| ws.welcome()), "the first run");

        let entry = cx.debug_bounds("use-this-mac").expect("the entry, a row to press");
        let field = cx.debug_bounds("add-worker-field").expect("the address");
        assert!(entry.bottom() <= field.top(), "a place to start, over the address to type");
        cx.simulate_click(entry.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(host.asked().is_empty(), "nothing installed while the tailnet is looked on");
        assert_eq!(flow(&ws, cx).map(|f| f.server), Some(this_mac::Server::Looking));
        assert!(cx.debug_bounds("this-mac-checklist").is_some(), "the checklist is up");

        let none = net::Tailnet { running: true, ..net::Tailnet::default() };
        cx.update(|window, cx| ws.update(cx, |ws, cx| ws.offer_found(none, window, cx)));
        cx.run_until_parked();
        assert_eq!(host.asked(), ["install here", "doctor"], "a server here, then the worker");
        let here = this_mac::Serve::Here.address();
        assert_eq!(ws.read_with(cx, |ws, _| ws.server_address().cloned()), Some(here));
        assert!(cx.debug_bounds("add-worker-field").is_none(), "in the address's place");
        assert!(cx.debug_bounds("use-this-mac").is_none(), "the entry has done its part");
        assert!(cx.debug_bounds("this-mac-fix-accessibility").is_none(), "granted: no button");
        let open = cx.debug_bounds("this-mac-fix-screen").expect("Screen Recording's button");
        cx.simulate_click(open.center(), gpui::Modifiers::none());
        assert_eq!(host.asked(), ["open Privacy(ScreenRecording)"], "its own pane");

        // Granted in System Settings; the app comes back to the front.
        let worker = lacking.worker;
        *host.doctor.borrow_mut() = Some(this_mac::Doctor { screen_recording: true, ..lacking });
        cx.update(|window, cx| ws.update(cx, |ws, cx| ws.this_mac_activated(window, cx)));
        cx.run_until_parked();
        assert_eq!(host.asked(), ["restart", "doctor"], "started again to see the grant");
        assert_eq!(flow(&ws, cx).map(|f| f.listing), Some(true), "waiting for the directory");

        list(&ws, cx, worker, "mac-studio");
        cx.executor().advance_clock(this_mac::RETRY);
        cx.run_until_parked();
        assert!(host.asked().contains(&"open at login".to_owned()), "finished: open at login");
        assert_eq!(flow(&ws, cx).and_then(|f| f.login), Some(this_mac::Login::On));
        let status = ws.read_with(cx, |ws, _| ws.adding.as_ref().map(|a| a.this_mac.is_some()));
        assert_eq!(status, Some(true), "the flow ends on its checklist");
        assert_eq!(flow(&ws, cx).map(|f| f.ready_words()), Some(this_mac::READY));
        assert!(cx.debug_bounds("connect-device").is_some(), "where a phone joins");
        let done = cx.debug_bounds("this-mac-done").expect("Done");
        cx.simulate_click(done.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(ws.read_with(cx, |ws, _| ws.adding.is_none()), "the page gave way to this Mac");

        // Listed now, the row is the way back to this Mac's checklist.
        cx.dispatch_action(AddWorker);
        cx.run_until_parked();
        assert!(cx.debug_bounds("install-over-ssh").is_some(), "a machine's panel");
        assert!(cx.debug_bounds("use-this-mac").is_some(), "this Mac's checklist, again");
        let phone = cx.debug_bounds("connect-device").expect("a phone's way in");
        cx.simulate_click(phone.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        let shown = ws.read_with(cx, |ws, _| (ws.adding.is_none(), ws.inviting.is_some()));
        assert_eq!(shown, (true, true), "the panel gives way to the code");
    }

    /// This Mac's checklist says what is ready in a line each and what is not over what to
    /// do; what a ready line says of itself (the worker's version, this Mac's name on the
    /// tailnet) and the app's build wait under Details, which opens and closes.
    #[gpui::test]
    fn the_checklist_is_concise_and_its_details_open_on_request(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, false);
        cx.simulate_resize(size(px(900.0), px(800.0)));
        let lacking = this_mac::Doctor { screen_recording: false, ..green() };
        let host = StandIn::answering(Some(lacking));
        let shared: Rc<dyn this_mac::Host> = Rc::<StandIn>::clone(&host);
        ws.update(cx, |ws, cx| {
            ws.this_mac = Some(shared);
            cx.notify();
        });
        cx.run_until_parked();
        let title = cx.debug_bounds("add-worker-title");
        assert!(title.is_some_and(|t| t.size.height >= px(32.0)), "26/32 heading: {title:?}");
        let entry = cx.debug_bounds("use-this-mac").expect("the entry");
        cx.simulate_click(entry.center(), gpui::Modifiers::none());
        let none = net::Tailnet { running: true, ..net::Tailnet::default() };
        cx.update(|window, cx| ws.update(cx, |ws, cx| ws.offer_found(none, window, cx)));
        cx.run_until_parked();
        let ready = cx.debug_bounds("this-mac-accessibility").expect("a line that holds");
        let missing = cx.debug_bounds("this-mac-screen").expect("a line that does not");
        assert!(ready.size.height < missing.size.height, "{ready:?} is its name alone");
        assert!(cx.debug_bounds("this-mac-facts").is_none(), "the details start closed");
        let details = cx.debug_bounds("this-mac-details").expect("the way to them");
        cx.simulate_click(details.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(cx.debug_bounds("this-mac-facts").is_some(), "open on request");
        assert_eq!(flow(&ws, cx).map(|f| f.details), Some(true));
        let details = cx.debug_bounds("this-mac-details").expect("the way back");
        cx.simulate_click(details.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(cx.debug_bounds("this-mac-facts").is_none(), "and closed again");
    }

    /// The app's own lines: with notifications, opening at login and Slopty's place in Finder
    /// all switched off in System Settings, finishing leaves the login item off, and each line's
    /// button opens its place there; back from there with all turned on, every line says so.
    /// Notifications never asked for are asked for from the line's "Allow", and a login item
    /// merely off is turned on from its line's "Turn on".
    #[gpui::test]
    fn this_mac_checks_what_the_app_lacks_and_reads_it_again(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, false);
        cx.simulate_resize(size(px(900.0), px(800.0)));
        let ready = green();
        let host = StandIn::answering(Some(ready.clone()));
        host.notes.set(Some(this_mac::Alerts::Denied));
        *host.finder.borrow_mut() = Some(finder::Step::SwitchOn);
        host.login.set(Some(this_mac::Login::Blocked));
        let shared: Rc<dyn this_mac::Host> = Rc::<StandIn>::clone(&host);
        ws.update(cx, |ws, cx| {
            ws.this_mac = Some(shared);
            cx.notify();
        });
        cx.run_until_parked();
        let entry = cx.debug_bounds("use-this-mac").expect("the entry");
        cx.simulate_click(entry.center(), gpui::Modifiers::none());
        let none = net::Tailnet { running: true, ..net::Tailnet::default() };
        cx.update(|window, cx| ws.update(cx, |ws, cx| ws.offer_found(none, window, cx)));
        cx.run_until_parked();
        assert_eq!(host.asked(), ["install here", "doctor"]);
        list(&ws, cx, ready.worker, "mac-studio");
        cx.executor().advance_clock(this_mac::RETRY);
        cx.run_until_parked();
        assert_eq!(flow(&ws, cx).map(|f| f.listed), Some(true), "listed, the flow's end");
        let listing = host.asked();
        assert!(!listing.contains(&"open at login".to_owned()), "turned off there, it stays off");

        let notes = cx.debug_bounds("this-mac-fix-notifications").expect("notifications' button");
        cx.simulate_click(notes.center(), gpui::Modifiers::none());
        let login = cx.debug_bounds("this-mac-fix-login").expect("Login Items' button");
        cx.simulate_click(login.center(), gpui::Modifiers::none());
        let switch = cx.debug_bounds("this-mac-fix-finder").expect("Finder's button");
        cx.simulate_click(switch.center(), gpui::Modifiers::none());
        let places = ["open Notifications", "open LoginItems", "open FileProviders"];
        assert_eq!(host.asked(), places, "their places");

        // Turned on in System Settings; the app comes back to the front.
        host.notes.set(Some(this_mac::Alerts::Allowed));
        *host.finder.borrow_mut() = Some(finder::Step::Open(dir.path().to_path_buf()));
        host.login.set(Some(this_mac::Login::On));
        cx.update(|window, cx| ws.update(cx, |ws, cx| ws.this_mac_activated(window, cx)));
        cx.run_until_parked();
        let marks = |cx: &mut VisualTestContext| {
            flow(&ws, cx).map(|f| this_mac::app_lines(&f).map(|l| (l.mark, l.fix)))
        };
        let on = (this_mac::Mark::Ok, None);
        assert_eq!(marks(cx), Some([on, on, on]), "read again: all on");

        // Never asked: "Allow" asks, and the answer is the line's.
        let done = cx.debug_bounds("this-mac-done").expect("Done");
        cx.simulate_click(done.center(), gpui::Modifiers::none());
        host.notes.set(Some(this_mac::Alerts::Unasked));
        pick(cx, "use-this-mac");
        let allow = cx.debug_bounds("this-mac-fix-notifications").expect("Allow");
        cx.simulate_click(allow.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(host.asked().contains(&"ask notes".to_owned()), "the system asks the person");
        assert_eq!(flow(&ws, cx).and_then(|f| f.notes), Some(this_mac::Alerts::Allowed));

        // Off, but not refused: "Turn on" opens Slopty at login.
        host.login.set(Some(this_mac::Login::Off));
        cx.update(|window, cx| ws.update(cx, |ws, cx| ws.this_mac_activated(window, cx)));
        cx.run_until_parked();
        let _read = host.asked();
        let turn_on = cx.debug_bounds("this-mac-fix-login").expect("Turn on");
        cx.simulate_click(turn_on.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert_eq!(host.asked(), ["open at login"], "registered");
        assert_eq!(flow(&ws, cx).and_then(|f| f.login), Some(this_mac::Login::On));
    }

    /// "Add a machine…", then its row `row`.
    fn pick(cx: &mut VisualTestContext, row: &'static str) {
        cx.dispatch_action(AddWorker);
        cx.run_until_parked();
        let at = cx.debug_bounds(row).unwrap_or_else(|| panic!("the {row} row"));
        cx.simulate_click(at.center(), gpui::Modifiers::none());
        cx.run_until_parked();
    }

    /// With several servers answering, or none to look on, "Use this Mac" asks for the server
    /// with the one address field; empty, it starts one here. A server set already is joined
    /// with nothing asked.
    #[gpui::test]
    fn this_mac_asks_which_server_when_it_cannot_tell(cx: &mut TestAppContext) {
        use slopty_net::discover::Found;
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, false);
        cx.simulate_resize(size(px(900.0), px(700.0)));
        let host = StandIn::answering(None);
        let shared: Rc<dyn this_mac::Host> = Rc::<StandIn>::clone(&host);
        ws.update(cx, |ws, _cx| ws.this_mac = Some(shared));
        let found = |name: &str, ip: &str| Found {
            name: name.to_owned(),
            addr: format!("{ip}:45560").parse().unwrap(),
            tags: Vec::new(),
            answer: None,
        };
        let two = net::Tailnet {
            servers: vec![found("hub", "100.64.0.1"), found("box", "100.64.0.2")],
            workers: Vec::new(),
            running: true,
        };
        cx.update(|window, cx| {
            ws.update(cx, |ws, cx| {
                ws.offer_found(two, window, cx);
                ws.use_this_mac(window, cx);
            });
        });
        cx.run_until_parked();
        let asking = ws.read_with(cx, |ws, _| ws.adding.as_ref().and_then(|a| a.asking));
        assert_eq!(asking, Some(Asking::Several));
        assert!(host.asked().is_empty(), "nothing installed before it is told");
        assert!(cx.debug_bounds("add-worker-field").is_some(), "the address to say it in");
        let typed = ws.read_with(cx, |ws, cx| {
            ws.adding.as_ref().map(|a| a.address.read(cx).value().to_string())
        });
        let typed = typed.unwrap_or_default();
        assert_eq!(typed, "100.64.0.1", "the best one offered");
        cx.update(|window, cx| {
            ws.update(cx, |ws, cx| {
                if let Some(adding) = &ws.adding {
                    adding.address.update(cx, |i, cx| i.set_value(String::new(), window, cx));
                }
                ws.enter_address(window, cx);
            });
        });
        cx.run_until_parked();
        assert_eq!(host.asked(), ["install here", "doctor"], "empty starts one here");

        cx.update(|window, cx| {
            ws.update(cx, |ws, cx| {
                ws.leave_this_mac(cx);
                ws.use_this_mac(window, cx);
            });
        });
        cx.run_until_parked();
        let here = this_mac::Serve::Here.address();
        assert_eq!(host.asked().first(), Some(&format!("install join {here}")), "its own server");
    }

    /// A failed install is the checklist's first line, red with a way to try again; back to
    /// the panel leaves the checklist for the panel's rows. The checklist is taller than a
    /// short window has room for, so it scrolls and the way back stays in view.
    #[gpui::test]
    fn a_failed_install_offers_another_try(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, true);
        cx.simulate_resize(size(px(900.0), px(700.0)));
        let host = StandIn::answering(None);
        let failed = this_mac::Stopped::Failed("launchctl bootstrap failed".to_owned());
        host.stops.borrow_mut().push_back(failed);
        let shared: Rc<dyn this_mac::Host> = Rc::<StandIn>::clone(&host);
        ws.update(cx, |ws, _cx| ws.this_mac = Some(shared));
        pick(cx, "use-this-mac");
        assert_eq!(host.asked(), ["install join hub:45560"], "the server set already");
        assert_eq!(
            flow(&ws, cx).map(|f| f.worker),
            Some(this_mac::Worker::Failed("launchctl bootstrap failed".to_owned())),
            "the reason, kept"
        );
        assert!(cx.debug_bounds("this-mac-fix-running").is_some(), "a way to try again");
        let back = cx.debug_bounds("panel-switch").expect("the way back");
        assert!(back.bottom() <= px(700.0), "the checklist scrolls; the way back stays: {back:?}");
        cx.simulate_click(back.center(), gpui::Modifiers::none());
        assert!(cx.debug_bounds("this-mac-checklist").is_none(), "left");
        assert!(cx.debug_bounds("use-this-mac").is_some(), "the rows are back");
    }

    /// An install that would end this Mac's sessions stops before changing anything and
    /// says so, with "Update anyway" as the one way on; one run from outside Applications
    /// offers the move instead.
    #[gpui::test]
    fn this_mac_asks_before_it_ends_sessions_or_installs_from_a_download(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, true);
        cx.simulate_resize(size(px(900.0), px(700.0)));
        let plan = "slopty-ptyd restarts, ending the 2 sessions it holds".to_owned();
        let host = StandIn::answering(None);
        host.stops
            .borrow_mut()
            .extend([this_mac::Stopped::EndsSessions(plan.clone()), this_mac::Stopped::Misplaced]);
        let shared: Rc<dyn this_mac::Host> = Rc::<StandIn>::clone(&host);
        ws.update(cx, |ws, _cx| ws.this_mac = Some(shared));
        let worker = |cx: &mut VisualTestContext| flow(&ws, cx).map(|f| f.worker);
        pick(cx, "use-this-mac");
        assert_eq!(host.asked(), ["install join hub:45560"]);
        assert_eq!(worker(cx), Some(this_mac::Worker::EndsSessions(plan)));
        let lines = flow(&ws, cx).map(|f| this_mac::checklist(&f, "", "")).expect("the flow");
        assert_eq!(lines[0].fix, Some(this_mac::Fix::EndSessions));
        assert!(lines[0].detail.contains("ending the 2 sessions"), "{}", lines[0].detail);
        let anyway = cx.debug_bounds("this-mac-fix-running").expect("Update anyway");
        cx.simulate_click(anyway.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert_eq!(host.asked(), ["install join hub:45560 end_sessions"], "the press is the yes");
        assert_eq!(worker(cx), Some(this_mac::Worker::Misplaced));
        let lines = flow(&ws, cx).map(|f| this_mac::checklist(&f, "", "")).expect("the flow");
        assert_eq!(lines[0].fix.map(this_mac::Fix::label), Some("Move to Applications"));
        let move_it = cx.debug_bounds("this-mac-fix-running").expect("the move");
        cx.simulate_click(move_it.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert_eq!(host.asked(), ["move"]);
        assert_eq!(
            worker(cx),
            Some(this_mac::Worker::Failed(
                "Could not move Slopty: the test moves nothing".to_owned()
            )),
            "a move that fails says why, and the app stays"
        );
    }

    /// The keyboard shortcuts are a palette line on every device, not only in a Mac's Help menu.
    #[test]
    fn the_palette_offers_the_keyboard_shortcuts() {
        let offered = app_palette_items().iter().any(|item| item.label == KEYBOARD_SHORTCUTS);
        assert!(offered);
    }

    /// This Mac and a machine over SSH are rows of "Add a machine…", which is the palette's
    /// one line for them.
    #[test]
    fn the_palette_adds_a_machine_in_one_line() {
        let labels: Vec<_> = app_palette_items().into_iter().map(|item| item.label).collect();
        assert!(labels.iter().any(|l| l == "Add a machine\u{2026}"), "{labels:?}");
        let rows = [this_mac::TITLE, ssh::TITLE];
        assert!(!labels.iter().any(|l| rows.contains(&l.as_str())), "rows only: {labels:?}");
    }

    /// The palette offers the workers in Finder on a Mac, under a command the keymap can bind.
    #[test]
    fn the_palette_offers_the_workers_in_finder_on_a_mac() {
        let offered = app_palette_items().iter().any(|item| item.label == finder::TITLE);
        assert_eq!(offered, finder::OFFERED);
        let bindable = app_commands().iter().any(|c| c.name() == "show_workers_in_finder");
        assert_eq!(bindable, finder::OFFERED);
    }

    /// A dial a worker answers from another build shows as its status, with both builds and the
    /// command that updates it, and the loop does not dial it again on the backoff: only after
    /// the long wait, or when something wakes it.
    #[gpui::test]
    fn a_worker_on_another_build_is_asked_again_only_after_the_long_wait(cx: &mut TestAppContext) {
        use slopty_client::update::{Of, UpdateNotice};
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ws = workspace(cx, &runtime, &dir);
        let notice = UpdateNotice {
            of: Of::Worker,
            host: "mini".to_owned(),
            peer: "0.0.9+wire.0badf00d".to_owned(),
        };
        let dials = Rc::new(std::cell::Cell::new(0_u32));
        let (counted, said) = (Rc::clone(&dials), notice.clone());
        ws.update(cx, |ws, _cx| {
            ws.dial = Dialer(Rc::new(move |_id, _address, _cx| {
                counted.set(counted.get().saturating_add(1));
                gpui::Task::ready(Err(net::DialFailed::WrongBuild(said.clone())))
            }));
        });
        let id = WorkerId::new();
        list(&ws, cx, id, "mini");
        assert_eq!(dials.get(), 1);
        let status = ws.read_with(cx, |ws, cx| {
            ws.view.read(cx).workers().map(|(_, _, status)| status.clone()).next()
        });
        assert_eq!(status, Some(WorkerStatus::NeedsUpdate(notice)));

        cx.executor().advance_clock(slopty_net::redial::MAX.saturating_mul(10));
        cx.run_until_parked();
        assert_eq!(dials.get(), 1, "not dialled again on the backoff");
        cx.executor().advance_clock(slopty_net::redial::WRONG_BUILD);
        cx.run_until_parked();
        assert_eq!(dials.get(), 2, "asked again after the long wait");
        ws.update(cx, |ws, _cx| ws.connect_now(id));
        cx.run_until_parked();
        assert_eq!(dials.get(), 3, "or at once when woken");
        ws.update(cx, |ws, cx| ws.drop_slot(id, cx));
        cx.run_until_parked();
    }

    /// A worker the server calls away is not dialled on every backoff, only after the hold;
    /// but a resume (or any wake) dials it at once, once, rather than starting the hold over.
    #[gpui::test]
    fn a_wake_dials_a_held_worker_once_rather_than_holding_again(cx: &mut TestAppContext) {
        use slopty_proto::server::WorkerInfo;
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ws = workspace(cx, &runtime, &dir);
        let dials = Rc::new(std::cell::Cell::new(0_u32));
        let counted = Rc::clone(&dials);
        let id = WorkerId::new();
        ws.update(cx, |ws, _cx| {
            ws.dial = Dialer(Rc::new(move |_id, _address, _cx| {
                counted.set(counted.get().saturating_add(1));
                gpui::Task::ready(Err(net::DialFailed::Other("no answer".to_owned())))
            }));
            ws.directory = slopty_client::directory::Directory::cached(vec![WorkerInfo {
                worker: id,
                name: "box".to_owned(),
                address: "100.64.0.2:45550".to_owned(),
                liveness: Liveness::Unreachable,
                caps: slopty_proto::server::WorkerCaps::bare(slopty_proto::server::Os::MacOs),
                load: 0.0,
                last_seen_ms: slopty_core::WallMs::ZERO,
            }]);
            ws.directory.set_server(slopty_client::directory::ServerState::Linked {
                name: "hub".to_owned(),
                link: 1,
            });
        });
        ws.update(cx, |ws, cx| ws.add_worker(id, "box".to_owned(), cx));
        cx.run_until_parked();
        assert_eq!(dials.get(), 0, "held on the server's word");

        ws.update(cx, |ws, _cx| ws.resume(slopty_platform::resume::Resume::Woke));
        cx.run_until_parked();
        assert_eq!(dials.get(), 1, "dialled at once on the resume");
        cx.executor().advance_clock(slopty_net::redial::MAX);
        cx.run_until_parked();
        assert_eq!(dials.get(), 1, "held again after that dial, not on the backoff");
        cx.executor().advance_clock(server::HOLD_RETRY);
        cx.run_until_parked();
        assert_eq!(dials.get(), 2, "and tried again after the hold");
        ws.update(cx, |ws, cx| ws.drop_slot(id, cx));
        cx.run_until_parked();
    }

    /// A settings file read again rebinds the keys at once: the app's own command runs on the
    /// file's chord and no longer on its default, and on its default again once the file drops
    /// the line. A chord taken from another command is said in one notice naming both.
    #[gpui::test]
    fn saved_keys_rebind_at_once(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, false);
        let load = |cx: &mut VisualTestContext, text: &str| {
            ws.update(cx, |ws, cx| ws.apply_loaded(Settings::parse(text), cx));
            cx.run_until_parked();
        };
        let press = |cx: &mut VisualTestContext, keys: &str| {
            cx.update(|window, cx| ws.update(cx, |ws, cx| ws.close_settings(window, cx)));
            // The dialog's dim holds the pointer while it fades out.
            cx.executor().advance_clock(kit::Pace::Exit.duration());
            cx.run_until_parked();
            let field = cx.debug_bounds("add-worker-field").expect("the panel's field");
            cx.simulate_click(field.center(), gpui::Modifiers::none());
            cx.simulate_keystrokes(keys);
            cx.run_until_parked();
            ws.read_with(cx, |ws, _| ws.settings_editor.is_some())
        };
        assert!(press(cx, "cmd-,"), "the default");

        load(cx, "[keys.app]\nopen_settings = \"cmd-;\"\n");
        assert!(!press(cx, "cmd-,"), "the default no longer");
        assert!(press(cx, "cmd-;"), "the file's chord");

        load(cx, "[keys.app]\nopen_settings = \"cmd-shift-h\"\n");
        let said = ws.read_with(cx, |ws, cx| ws.view.read(cx).toast_text());
        assert_eq!(
            said.as_deref(),
            Some(
                "Settings: ⇧⌘H runs `app.open_settings` now, no longer `file.toggle_replace` \
                 (+1 more)"
            ),
            "it takes the file's replace key and the app's add worker"
        );
        assert!(press(cx, "cmd-shift-h"), "the file's command wins the chord");

        load(cx, "");
        assert!(press(cx, "cmd-,"), "the default again");
        assert!(!press(cx, "cmd-;"));
    }

    /// The menu bar is built again whenever the keys are rebound, from the keys bound then:
    /// AppKit runs a menu item's key equivalent itself, so a menu built once would keep running
    /// New Shell on ⌘T after the file moved it to ⌘Y.
    #[gpui::test]
    fn the_menu_bar_follows_a_rebinding(cx: &mut TestAppContext) {
        use slopty_ui::workspace::NewTerminal;
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ws = workspace(cx, &runtime, &dir);
        let built = Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = Rc::clone(&built);
        cx.update(|cx| {
            set_app_menus(cx, move || {
                let keys = slopty_ui::keymap::current().label_of(&NewTerminal).to_owned();
                seen.borrow_mut().push(keys);
                vec![
                    gpui::Menu::new("File")
                        .items([gpui::MenuItem::action("New Shell", NewTerminal)]),
                ]
            });
        });
        let file = "[keys.workspace]\nnew_terminal = \"cmd-y\"\n";
        ws.update(cx, |ws, cx| ws.apply_loaded(Settings::parse(file), cx));
        ws.update(cx, |ws, cx| ws.apply_loaded(Settings::parse(file), cx));
        assert_eq!(*built.borrow(), ["⌘T", "⌘Y"], "built at first, then once for the change");
        let menus = cx.update(|cx| cx.get_menus()).unwrap_or_default();
        assert_eq!(menus.iter().map(|m| m.name.to_string()).collect::<Vec<_>>(), ["File"]);
        // The item's key equivalent as AppKit's menu takes it: the action's first binding
        // that holds in the workspace (`gpui_macos`'s `create_menu_item`).
        let equivalent = cx.update(|cx| {
            let mut context = gpui::KeyContext::new_with_defaults();
            context.add("Workspace");
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            keymap
                .bindings_for_action(&NewTerminal)
                .find(|b| b.predicate().is_none_or(|p| p.eval(std::slice::from_ref(&context))))
                .map(|b| b.keystrokes().iter().map(|k| k.inner().unparse()).collect::<String>())
        });
        assert_eq!(equivalent.as_deref(), Some("cmd-y"), "no ⌘T left for the menu to take");
    }

    /// A settings file that does not parse changes no key: the keymap stays as last applied,
    /// not the defaults', and the file's trouble is said.
    #[gpui::test]
    fn a_broken_file_keeps_the_keys(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, false);
        let load = |cx: &mut VisualTestContext, text: &str| {
            ws.update(cx, |ws, cx| ws.apply_loaded(Settings::parse(text), cx));
            cx.run_until_parked();
        };
        let said =
            |cx: &mut VisualTestContext| ws.read_with(cx, |ws, cx| ws.view.read(cx).toast_text());
        let note_keys = || {
            let keymap = slopty_ui::keymap::current();
            let ix = keymap.find(slopty_ui::keymap::Scope::Workspace, "new_note");
            keymap.chords(ix.unwrap_or_default()).to_vec()
        };

        load(cx, "[keys.workspace]\nnew_note = \"cmd-alt-n\"\n");
        assert_eq!(note_keys(), ["alt-cmd-n"], "the file's keys");
        load(cx, "[keys.workspace\nnew_note = ");
        assert!(said(cx).is_some_and(|t| t.starts_with("Settings: ")), "{:?}", said(cx));
        assert_eq!(note_keys(), ["alt-cmd-n"], "the keys last applied");
    }

    /// A change from the settings form writes the file and applies it with the dialog still
    /// open; a text that does not parse is not written and the dialog says why; the file's face
    /// saved is written, applied and closed.
    #[gpui::test]
    fn a_settings_change_applies_with_the_dialog_still_open(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let (ws, cx) = shell(cx, &runtime, &dir, false);
        cx.update(|window, cx| ws.update(cx, |ws, cx| ws.open_settings(window, cx)));
        cx.run_until_parked();
        let editor = ws.read_with(cx, |ws, _| ws.settings_editor.clone()).expect("the dialog");
        let send = |cx: &mut VisualTestContext, event: SettingsEditorEvent| {
            editor.update(cx, |_, cx| cx.emit(event));
            cx.run_until_parked();
        };
        let open =
            |cx: &mut VisualTestContext| ws.read_with(cx, |ws, _| ws.settings_editor.is_some());

        send(cx, SettingsEditorEvent::Apply("[font]\nligatures = false\n".to_owned()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[font]\nligatures = false\n");
        assert!(!ws.read_with(cx, |ws, _| ws.settings.font.ligatures), "applied");
        assert!(open(cx), "the dialog stays open");

        send(cx, SettingsEditorEvent::Apply("[font]\nligatures = 3\n".to_owned()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[font]\nligatures = false\n");
        assert!(editor.read_with(cx, |e, _| e.error().is_some()), "the dialog says why");
        assert!(open(cx), "and stays open");

        send(cx, SettingsEditorEvent::Save("[font]\nligatures = true\n".to_owned()));
        assert!(ws.read_with(cx, |ws, _| ws.settings.font.ligatures), "saved and applied");
        assert!(!open(cx), "and closed");
    }
}
