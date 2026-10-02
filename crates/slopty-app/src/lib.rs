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
pub mod finder;
mod hangs;
pub mod net;
mod presence;
mod server;
pub mod settings;
pub mod ssh;
pub mod this_mac;
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
pub use settings::actions::{OpenKeyboardShortcuts, OpenSettings};
use slopty_client::LinkEvent;
use slopty_client::layout::WorkerKey;
use slopty_core::{SessionId, WorkerId};
use slopty_platform::notify::{Notifier, Tap};
use slopty_proto::WorkerMsg;
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
pub use ssh::actions::InstallOverSsh;
pub use this_mac::actions::UseThisMac;
pub use window::actions::{Minimize, OpenHelp, ShowWindow, Zoom};
pub use window::{HELP_URL, show as show_main_window};
pub use workers::actions::{AddWorker, ConnectServer, DisconnectServer};
use workers::{Hearing, Tick, WorkerSlot};

/// A finger drives this build: the key bar the soft keyboard lacks (Esc, Tab, Control, arrows,
/// shell symbols), a Paste where the Mac has ⌘V, full-width primary actions.
pub(crate) const TOUCH: bool = cfg!(target_os = "ios");

/// The waits before each ask to delete a forgotten worker's page store: at once, then over
/// about eight seconds.
const FORGET_PAGES_WAITS: [std::time::Duration; 6] = [
    std::time::Duration::ZERO,
    std::time::Duration::from_millis(250),
    std::time::Duration::from_millis(500),
    std::time::Duration::from_secs(1),
    std::time::Duration::from_secs(2),
    std::time::Duration::from_secs(4),
];

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

/// The key bar's keys: label, GPUI key name, and the character it types (`None` for
/// non-printing keys). In the order a phone shows them before the row scrolls: the keys the
/// soft keyboard has no way to type come first, the punctuation it only hides after.
const BAR_KEYS: [(&str, &str, Option<&str>); 12] = [
    ("Esc", "escape", None),
    ("Tab", "tab", None),
    ("Ctrl", "", None),
    ("←", "left", None),
    ("↑", "up", None),
    ("↓", "down", None),
    ("→", "right", None),
    ("~", "~", Some("~")),
    ("|", "|", Some("|")),
    ("/", "/", Some("/")),
    ("-", "-", Some("-")),
    ("⌘", "cmd", None),
];
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
        "Esc" | "Tab" | "Ctrl" | "⌘" => KeyGroup::Lead,
        "←" | "↑" | "↓" | "→" => KeyGroup::Arrows,
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
    ("Ctrl", "", None),
    ("⌘", "cmd", None),
    ("←", "left", None),
    ("↑", "up", None),
    ("↓", "down", None),
    ("→", "right", None),
    ("/", "/", Some("/")),
];
/// What the panel over the workspace connects to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Panel {
    /// A server, whose directory lists the workers: the usual way in.
    Server,
    /// One worker by address, for a setup without a server.
    Worker,
}

impl Panel {
    /// The address field's example: the kind of host this panel is for, by name or by its
    /// tailnet IP. A server is usually a small always-on box, a worker a Mac someone works on.
    const fn example(self) -> &'static str {
        match self {
            Self::Server => "home-server or 100.64.0.1",
            Self::Worker => "mac-studio or 100.64.0.3",
        }
    }

    /// What the address field takes, as its label names it.
    const fn field(self) -> &'static str {
        match self {
            Self::Server => "Server address",
            Self::Worker => "Worker address",
        }
    }

    /// The address field's label: "Or type an address" only under rows to press, where "or"
    /// has a first way to follow; else what the field takes.
    fn address_label(self, search: Option<&Search>) -> &'static str {
        if search.is_some_and(|s| !s.offers(self).is_empty()) {
            "Or type an address"
        } else {
            self.field()
        }
    }
}

/// The panel that connects to a server or adds a worker by address: shown until this
/// installation reaches some worker, and on "Connect to a server" or "Add a worker".
#[derive(Debug)]
struct Adding {
    /// Which of the two.
    mode: Panel,
    /// Where the address is typed or pasted.
    address: Entity<InputState>,
    /// A connection attempt is in flight.
    busy: bool,
    /// Why the last attempt failed.
    error: Option<String>,
    /// Where the look on the tailnet stands; `None` where none is made (iOS, where no app can
    /// read Tailscale, and a server's panel while a server is set).
    search: Option<Search>,
    /// "Use this Mac as a worker" under way: its checklist stands in for the address.
    this_mac: Option<this_mac::Flow>,
    /// "Install on a machine over SSH": its form, then its steps, stand in for the address.
    ssh: Option<ssh::Sheet>,
    /// Return in the address field adds what it holds; it goes with the panel.
    _enter: gpui::Subscription,
}

impl Adding {
    /// Whether an input method is composing in one of the panel's fields: Esc is its own then.
    fn composing(&self, cx: &App) -> bool {
        self.address.read(cx).is_composing() || self.ssh.as_ref().is_some_and(|s| s.composing(cx))
    }
}

/// What the tailnet offers the panel: a server to connect to, or a worker to add.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Host {
    /// A server; pressing it connects to it.
    Server,
    /// A worker; pressing it adds it.
    Worker,
}

/// A node that answered on the tailnet, by name and tailnet IP.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Offer {
    /// Its `MagicDNS` name.
    name: String,
    /// Its tailnet IP, which the panel dials.
    at: String,
}

impl From<slopty_net::discover::Found> for Offer {
    fn from(found: slopty_net::discover::Found) -> Self {
        Self { name: found.name, at: found.addr.ip().to_string() }
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
        /// Nodes that answered as a worker not yet added.
        workers: Vec<Offer>,
        /// This Mac's Tailscale is up: an empty answer is the tailnet's, not a look never made.
        running: bool,
    },
}

impl Search {
    /// The rows a panel in `mode` offers, in order: the servers (a server's panel only), then
    /// the workers, since a tailnet without a server still has its workers to add.
    fn offers(&self, mode: Panel) -> Vec<(Host, &Offer)> {
        let Self::Answered { servers, workers, .. } = self else { return Vec::new() };
        let servers = servers.iter().filter(|_| mode == Panel::Server).map(|o| (Host::Server, o));
        servers.chain(workers.iter().map(|o| (Host::Worker, o))).collect()
    }

    /// What the panel in `mode` says while it has nothing to offer.
    fn words(&self, mode: Panel) -> Option<&'static str> {
        match self {
            Self::Looking => Some(LOOKING),
            Self::Answered { running: false, .. } => {
                self.offers(mode).is_empty().then_some(NOT_RUNNING)
            }
            Self::Answered { .. } => self.offers(mode).is_empty().then_some(NOTHING_ANSWERED),
        }
    }

    /// What follows [`Self::words`] on its line: what to do about it. Nothing while it looks.
    const fn next_step(&self, mode: Panel) -> Option<&'static str> {
        match (self, mode) {
            (Self::Looking, _) => None,
            (Self::Answered { running: false, .. }, _) => {
                Some("Start it, or type an address on your VPN.")
            }
            (Self::Answered { .. }, Panel::Server) => {
                Some("Start the Slopty server on a machine there.")
            }
            (Self::Answered { .. }, Panel::Worker) => {
                Some("Start the Slopty worker on a Mac or Linux machine there.")
            }
        }
    }
}

/// The panel's word while it probes the tailnet.
const LOOKING: &str = "Looking on your tailnet\u{2026}";
/// The panel's word when nothing on the tailnet answered as a server or a worker.
const NOTHING_ANSWERED: &str = "Nothing answered on your tailnet";
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
    /// What the view was last told: whether the key bar shows. Kept here so the build compares
    /// without reading the view, which would build the app's root again with every change the
    /// view hears of.
    told: bool,
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
    /// The system's contrast setting, which the chrome is derived for.
    contrast: slopty_theme::Contrast,
    /// The system's Increase Contrast heard as it changes ([`watch_increase_contrast`]).
    contrast_watch: Option<slopty_platform::motion::Watch>,
    /// The system's wakes, unlocks, returns to the front and path changes ([`watch_resumes`]).
    resume_watch: Option<slopty_platform::resume::Watch>,
    adding: Option<Adding>,
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
    /// Focus the editor's field on the next frame (it needs a frame to exist).
    pending_focus_editor: bool,
    /// The self-test's stand-in for iPad Split View and Stage Manager: the app laid out in
    /// this size at the window's top left. A UIKit window cannot be resized from inside the
    /// app, and the layout only needs the size it is given to be the size it lays out in.
    split_view: Option<gpui::Size<gpui::Pixels>>,
    /// Where the key row is scrolled to.
    key_bar_scroll: ScrollHandle,
    /// What "Use this Mac as a worker" does to this machine; `None` where it is not offered.
    this_mac: Option<Rc<dyn this_mac::Host>>,
    /// Runs of it so far, so an answer for one left behind is dropped.
    this_mac_runs: u64,
    /// What installs and updates a worker over SSH; `None` where it is not offered.
    deployer: Option<Rc<dyn ssh::Deployer>>,
    /// Runs of the SSH sheet so far, so an answer for one left behind is dropped.
    ssh_runs: u64,
    /// Updates from the tiles of a worker on a different build, by host.
    updates: ssh::Updating,
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
                if !ws.attention.server_led() && settings::agent_alerts(&ws.settings, active) {
                    alert();
                }
            }
            // A program's own notification is this client's, server or not.
            WorkspaceEvent::Program(_session) => {
                if settings::agent_alerts(&ws.settings, cx.active_window().is_some()) {
                    alert();
                }
            }
            // A bell while the human is elsewhere is an alert; in front of the window the
            // view's own flash is enough.
            WorkspaceEvent::Bell(_session) => {
                if settings::bell_alerts(&ws.settings, cx.active_window().is_some()) {
                    alert();
                }
            }
            WorkspaceEvent::Unanswered { route, why } => {
                let title = ws.view.read(cx).route_title(*route);
                ws.attention.unanswered(*route, title, why);
            }
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
            told: false,
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
            contrast: slopty_theme::Contrast::Standard,
            contrast_watch: None,
            resume_watch: None,
            adding: None,
            runtime,
            window: None,
            settings_path,
            settings_seen,
            settings_editor: None,
            settings_editor_events: None,
            pending_focus_editor: false,
            split_view: None,
            key_bar_scroll: ScrollHandle::new(),
            this_mac,
            this_mac_runs: 0,
            deployer,
            ssh_runs: 0,
            updates: ssh::Updating::new(),
            attention: Attention::new(Rc::new(slopty_platform::notify::Memory::default())),
            heard_terminals: std::collections::HashMap::new(),
            dock_progress: None,
            presenting: presence::Presenting::default(),
            dial,
            #[cfg(target_os = "ios")]
            grace: None,
            #[cfg(target_os = "ios")]
            paste_key: None,
        };
        this.publish_updates(cx);
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

    /// The app came to the front or left it. On iOS, leaving holds the background grace.
    fn set_active(&mut self, active: bool) {
        self.attention.set_active(active);
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
        let editor = cx.new(|cx| {
            SettingsEditor::new(&text, &path, cfg!(target_os = "macos"), theme, window, cx)
        });
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
                SettingsEditorEvent::OpenExternally => {
                    open_settings_file(cx);
                    this.close_settings(window, cx);
                }
                SettingsEditorEvent::Dismiss => this.close_settings(window, cx),
            }));
        self.settings_editor = Some(editor);
        cx.notify();
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

    /// Drop the editor and hand the keyboard back to the focused tile.
    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_editor_events = None;
        if self.settings_editor.take().is_none() {
            return;
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
            self.show_notice(format!("Settings: {error}"), cx);
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
        self.settings = loaded.settings;
        self.view.update(cx, |v, cx| v.set_clipboard_sharing(sharing, cx));
        self.rebuild_theme(cx);
        self.apply_keymap(keymap, cx);
        // The app's palette lines show their chords from the keymap just bound.
        self.view.update(cx, |v, _| v.extend_palette(app_palette_items()));
        self.set_server(server, None, cx);
        self.refresh_menu(cx);
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

    /// The system's Increase Contrast was turned on or off.
    fn set_contrast(&mut self, contrast: slopty_theme::Contrast, cx: &mut Context<Self>) {
        if self.contrast != contrast {
            tracing::info!(?contrast, "contrast");
            self.contrast = contrast;
            self.rebuild_theme(cx);
        }
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
        let theme = settings::theme_for(&self.settings, self.window_dark, self.contrast);
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

    fn slot(&self, id: WorkerId) -> Option<&WorkerSlot> {
        self.workers.iter().find(|w| w.id == id)
    }

    fn slot_mut(&mut self, id: WorkerId) -> Option<&mut WorkerSlot> {
        self.workers.iter_mut().find(|w| w.id == id)
    }

    /// A note was tapped: its tile comes forward, on whichever worker it lives.
    fn open_notification(&self, tap: &Tap, cx: &mut Context<Self>) {
        self.view.update(cx, |v, cx| v.open_notification(tap, cx));
    }

    /// Start (or refresh) a worker: its tiles wait in the workspace and a connect loop of its
    /// own brings them to life. `added` marks one added by address, which stays without the
    /// server.
    fn add_worker(&mut self, id: WorkerId, name: String, added: bool, cx: &mut Context<Self>) {
        if let Some(slot) = self.slot_mut(id) {
            slot.name.clone_from(&name);
            slot.added |= added;
            let key = slot.key;
            self.view.update(cx, |v, cx| v.add_worker(key, name, cx));
            return;
        }
        let slot = WorkerSlot::new(id, name.clone(), added);
        let key = slot.key;
        self.workers.push(slot);
        self.view.update(cx, |v, cx| v.add_worker(key, name, cx));
        self.spawn_worker_loop(id, cx);
        self.refresh_menu(cx);
    }

    /// Forget a worker added by address: its entry in the store, then the slot. With none
    /// left the panel returns.
    fn forget_worker(&mut self, id: WorkerId, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(e) = net::forget_worker(id) {
            self.show_notice(format!("Could not forget the worker: {e:#}"), cx);
            return;
        }
        if self.directory.get(id).is_some() {
            if let Some(slot) = self.slot_mut(id) {
                slot.added = false;
            }
        } else {
            let key = self.slot_mut(id).map(|slot| slot.key);
            self.drop_slot(id, cx);
            if let Some(key) = key {
                Self::forget_pages(key, cx);
            }
        }
        if self.workers.is_empty() && self.server.is_none() {
            self.show_add_worker(Panel::Server, window, cx);
        }
        self.refresh_menu(cx);
        cx.notify();
    }

    /// A forgotten worker's pages lose their cookies and storage once the tiles that held them
    /// have gone. `WebKit` lets go of a store a little after its last view, so a refusal is
    /// asked again, a few times over some seconds.
    fn forget_pages(key: WorkerKey, cx: &Context<Self>) {
        cx.spawn(async move |_this, cx| {
            let mut why = String::new();
            for wait in FORGET_PAGES_WAITS {
                cx.background_executor().timer(wait).await;
                let (said, heard) = tokio::sync::oneshot::channel();
                // An app gone drops `said` unheard, which ends the asking.
                cx.update(|_cx| {
                    slopty_platform::web::forget(key.value(), move |gone| {
                        let _heard = said.send(gone);
                    });
                });
                match heard.await {
                    Ok(Ok(())) => return,
                    Ok(Err(e)) => why = e,
                    Err(_dropped) => return,
                }
            }
            tracing::warn!(%why, "a forgotten worker's pages kept their store");
        })
        .detach();
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
            let entry = match self.server_address() {
                Some(address) => MenuEntry {
                    group: MenuGroup::Connections,
                    label: "Disconnect from the server".into(),
                    detail: address.host().to_owned().into(),
                    run: Rc::new(move |window, cx| {
                        let _gone = this.update(cx, |ws, cx| ws.disconnect_server(window, cx));
                    }),
                },
                None => MenuEntry {
                    group: MenuGroup::Connections,
                    label: "Connect to a server".into(),
                    detail: SharedString::default(),
                    run: Rc::new(move |window, cx| {
                        let _gone =
                            this.update(cx, |ws, cx| ws.show_add_worker(Panel::Server, window, cx));
                    }),
                },
            };
            entries.push(entry);
        }
        entries.push(MenuEntry {
            group: MenuGroup::Connections,
            label: "Add a worker".into(),
            detail: hint(&AddWorker).into(),
            run: Rc::new(move |window, cx| {
                let _gone = this.update(cx, |ws, cx| ws.show_add_worker(Panel::Worker, window, cx));
            }),
        });
        self.view.update(cx, |v, cx| v.set_more_menu(entries, cx));
        self.refresh_hosts(cx);
    }

    /// What the status bar's hosts popover can do to each worker: dial it now, wake one the
    /// server can send a magic packet to (the palette offers that too), and forget one added
    /// by address; and its way to add a worker.
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
                let forget = slot.added.then(|| {
                    let this = this.clone();
                    let run: MenuRun = Rc::new(move |window, cx| {
                        let _gone = this.update(cx, |ws, cx| ws.forget_worker(id, window, cx));
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
        self.view.update(cx, |v, cx| v.set_host_actions(hosts, Some(add), cx));
    }

    /// Something may have killed the links: the device was away, the app came back to the
    /// front, or the path moved. Each live link is probed at once and a dead one dialled again
    /// at once ([`workers::Probe`]); a worker between links is dialled now. On a path change the
    /// connections migrate to the new path first (QUIC's own migration, which the probe then
    /// tests), so a link that survives it is never dialled again.
    pub(crate) fn resume(&self, resume: slopty_platform::resume::Resume) {
        tracing::info!(
            resume = resume.name(),
            workers = self.workers.len(),
            "resume; probing the links"
        );
        if resume.moved() {
            net::path_changed();
        }
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
    /// While the server says the worker is away the loop waits for it to come back online
    /// (or for [`server::HOLD_RETRY`], in case the server is the one that cannot see it). A
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
                    // Woken: back online, or the server went away. Timed out: try it anyway.
                    held = !wait_or_wake(cx, &wake, server::HOLD_RETRY).await;
                    continue;
                }
                held = false;
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
                            if wrong_build {
                                ws.update_still_wrong(id, cx);
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

    /// Show the panel, connecting to a server or adding a worker; an open panel switches to
    /// `mode` and keeps what was typed, leaving this Mac's checklist if it was up.
    fn show_add_worker(&mut self, mode: Panel, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(adding) = &mut self.adding {
            let left = adding.this_mac.take().is_some() | adding.ssh.take().is_some();
            if adding.mode != mode {
                adding.mode = mode;
                adding.error = None;
                adding.address.update(cx, |input, cx| {
                    input.set_placeholder(mode.example(), window, cx);
                });
            }
            if left || adding.mode != mode {
                cx.notify();
            }
            return;
        }
        let address = cx.new(|cx| InputState::new(window, cx).placeholder(mode.example()));
        let enter = cx.subscribe(&address, |this, _input, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.add_from_panel(cx);
            }
        });
        address.update(cx, |input, cx| input.focus(window, cx));
        // A server's panel looks while no server is set; a worker's always, for the workers
        // not yet added.
        let search = (LISTS_TAILNET && (mode == Panel::Worker || self.server.is_none()))
            .then_some(Search::Looking);
        let looking = search.is_some();
        self.adding = Some(Adding {
            mode,
            address,
            busy: false,
            error: None,
            search,
            this_mac: None,
            ssh: None,
            _enter: enter,
        });
        if looking {
            self.look_on_tailnet(window, cx);
        }
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

    /// The tailnet answered with `found`: each server and each worker not yet here is a row to
    /// press, and the best one of the panel's own kind has its address in the field unless the
    /// person has moved on (typed something, or connected). Nothing answering says so.
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
        let best = match adding.mode {
            Panel::Server => servers.first(),
            Panel::Worker => workers.first(),
        };
        let empty = adding.address.read(cx).value().trim().is_empty();
        if let Some(best) = best.filter(|_| empty && !adding.busy) {
            let at = best.at.clone();
            adding.address.update(cx, |input, cx| input.set_value(at, window, cx));
        }
        adding.search = Some(Search::Answered { servers, workers, running: found.running });
        cx.notify();
    }

    /// Connect to a server the tailnet found, or add a worker it found, at `at`: a worker turns
    /// the panel into the worker's, so a failure is reported there with its address.
    fn connect_found(&mut self, host: Host, at: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(adding) = &mut self.adding else { return };
        if adding.busy {
            return;
        }
        let mode = match host {
            Host::Server => Panel::Server,
            Host::Worker => Panel::Worker,
        };
        if adding.mode != mode {
            adding.mode = mode;
            adding.error = None;
            adding.address.update(cx, |input, cx| {
                input.set_placeholder(mode.example(), window, cx);
            });
        }
        adding.address.update(cx, |input, cx| input.set_value(at.to_owned(), window, cx));
        self.add_from_panel(cx);
    }

    /// Close the panel (only offered while there is somewhere else to be).
    fn cancel_add_worker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workers.is_empty() && self.server.is_none() {
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
        self.add_from_panel(cx);
    }

    /// The panel's report: the attempt ended with `error`, or it is still going.
    fn panel_failed(&mut self, error: String, cx: &mut Context<Self>) {
        if let Some(p) = &mut self.adding {
            p.busy = false;
            p.error = Some(error);
        }
        cx.notify();
    }

    /// Connect to the address in the field on the runtime, as the panel's mode says; report
    /// back to the panel.
    fn add_from_panel(&mut self, cx: &mut Context<Self>) {
        let Some(adding) = &mut self.adding else { return };
        if adding.busy {
            return;
        }
        let address = adding.address.read(cx).value().trim().to_owned();
        if address.is_empty() {
            return;
        }
        let mode = adding.mode;
        adding.busy = true;
        adding.error = None;
        cx.notify();
        match mode {
            Panel::Server => self.connect_from_panel(&address, cx),
            Panel::Worker => self.add_worker_from_panel(address, cx),
        }
    }

    fn add_worker_from_panel(&self, address: String, cx: &Context<Self>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.runtime.spawn(async move {
            let _sent = tx.send(net::add_worker(&address).await);
        });
        cx.spawn(async move |this, cx| {
            let outcome = rx.await;
            let _updated = this.update(cx, |ws, cx| match outcome {
                Ok(Ok(net::Added { id, name })) => {
                    ws.adding = None;
                    ws.show_notice(format!("Added {name}"), cx);
                    ws.add_worker(id, name, true, cx);
                    cx.notify();
                }
                Ok(Err(e)) => match e.downcast_ref::<net::OtherBuild>() {
                    // Not a dead end: the way on is installing this build there.
                    Some(other) => ws.install_this_build_at(other.0.host.clone(), cx),
                    None => ws.panel_failed(format!("{e:#}"), cx),
                },
                Err(_dropped) => ws.panel_failed("connection task died".to_owned(), cx),
            });
        })
        .detach();
    }

    /// The panel's worker answered on another build: the SSH sheet opens with its host filled
    /// in, to install this build over it.
    fn install_this_build_at(&mut self, host: String, cx: &Context<Self>) {
        if let Some(adding) = &mut self.adding {
            adding.busy = false;
        }
        let Some(window) = self.window else { return };
        cx.spawn(async move |this, cx| {
            let _opened = cx.update_window(window, |_root, window, cx| {
                this.update(cx, |ws, cx| ws.open_ssh_at(&host, window, cx))
            });
        })
        .detach();
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

    /// "Use this Mac as a worker", from the panel, the palette or a failed line's "Try again":
    /// install the worker's services, then read its `doctor` as the panel's checklist. A run
    /// already installing or adding is left to finish.
    fn use_this_mac(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(host) = self.this_mac.clone() else { return };
        if self.adding.is_none() {
            self.show_add_worker(Panel::Worker, window, cx);
        }
        let Some(adding) = &mut self.adding else { return };
        if adding
            .this_mac
            .as_ref()
            .is_some_and(|f| f.adding || f.worker == this_mac::Worker::Installing)
        {
            return;
        }
        self.this_mac_runs = self.this_mac_runs.wrapping_add(1);
        let run = self.this_mac_runs;
        adding.ssh = None;
        adding.this_mac = Some(this_mac::Flow::installing(run));
        let install = host.install();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = install.await;
            let _gone = this
                .update_in(cx, |ws, window, cx| ws.this_mac_installed(run, outcome, window, cx));
        })
        .detach();
        cx.notify();
    }

    /// Back from this Mac's checklist to the panel it was opened from.
    fn leave_this_mac(&mut self, cx: &mut Context<Self>) {
        if let Some(adding) = &mut self.adding {
            adding.this_mac = None;
        }
        cx.notify();
    }

    /// Run `run` of this Mac's flow, if it is still the one on screen.
    fn this_mac_flow(&mut self, run: u64) -> Option<&mut this_mac::Flow> {
        self.adding.as_mut()?.this_mac.as_mut().filter(|flow| flow.run == run)
    }

    /// The install ended: ask the worker how it stands, or say why it failed.
    fn this_mac_installed(
        &mut self,
        run: u64,
        outcome: Result<(), String>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(flow) = self.this_mac_flow(run) else { return };
        match outcome {
            Ok(()) => {
                flow.worker = this_mac::Worker::Starting;
                self.read_this_mac(run, false, window, cx);
            }
            Err(why) => flow.worker = this_mac::Worker::Failed(why),
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

    /// The worker answered with `doctor` (or never did): the checklist shows it, and a worker
    /// that may stream and take input is added.
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
        if flow.worker.ready() && !flow.adding {
            self.add_this_mac(run, window, cx);
        }
        cx.notify();
    }

    /// Add this Mac over loopback.
    fn add_this_mac(&mut self, run: u64, window: &Window, cx: &Context<Self>) {
        let Some(host) = self.this_mac.clone() else { return };
        let Some(flow) = self.this_mac_flow(run) else { return };
        flow.adding = true;
        flow.error = None;
        let add = host.add(this_mac::LOOPBACK);
        cx.spawn_in(window, async move |this, cx| {
            let outcome = add.await;
            let _gone =
                this.update_in(cx, |ws, window, cx| ws.this_mac_added(run, outcome, window, cx));
        })
        .detach();
    }

    /// This Mac was added: the panel closes onto its workspace, with a word when the tailnet
    /// does not reach it yet. A failure stays on the checklist.
    fn this_mac_added(
        &mut self,
        run: u64,
        outcome: Result<net::Added, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(flow) = self.this_mac_flow(run) else { return };
        match outcome {
            Ok(net::Added { id, name }) => {
                let reached = match &flow.worker {
                    this_mac::Worker::Up(d) => {
                        matches!(d.tailnet, this_mac::Tailnet::Reachable(_))
                    }
                    _ => true,
                };
                self.adding = None;
                self.show_notice(
                    if reached {
                        format!("Added {name}")
                    } else {
                        format!("Added {name}. Only this Mac reaches it until Tailscale is up.")
                    },
                    cx,
                );
                self.add_worker(id, name, true, cx);
                self.view.update(cx, |view, cx| view.return_keyboard(window, cx));
            }
            Err(why) => {
                flow.adding = false;
                flow.error = Some(why);
            }
        }
        cx.notify();
    }

    /// A missing line's button: its pane of System Settings, or the install again.
    fn this_mac_fix(&mut self, fix: this_mac::Fix, window: &mut Window, cx: &mut Context<Self>) {
        match fix {
            this_mac::Fix::Open(pane) => {
                if let Some(host) = &self.this_mac {
                    host.open(pane);
                }
            }
            this_mac::Fix::Retry => self.use_this_mac(window, cx),
        }
    }

    /// The app is in front again, perhaps from System Settings: a worker short of ready is
    /// asked again, and started again first when Screen Recording was what it lacked.
    fn this_mac_activated(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(flow) = self.adding.as_ref().and_then(|a| a.this_mac.as_ref()) else { return };
        if !flow.rereads() {
            return;
        }
        let (run, restart) = (flow.run, flow.restarts());
        self.read_this_mac(run, restart, window, cx);
        cx.notify();
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

    /// Stop using the server: the setting is cleared and its workers leave.
    fn disconnect_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.server.is_none() {
            return;
        }
        if let Err(e) = self.save_server(None) {
            self.show_notice(format!("Settings: {e}"), cx);
            return;
        }
        self.set_server(None, None, cx);
        self.show_notice("Disconnected from the server".to_owned(), cx);
        if self.workers.is_empty() {
            self.show_add_worker(Panel::Server, window, cx);
        }
        self.refresh_menu(cx);
        cx.notify();
    }

    /// Whether the panel is the whole window: nothing to go back to, so no workspace behind
    /// it and no way to dismiss it. The first run, and after the last worker is forgotten.
    const fn welcome(&self) -> bool {
        self.adding.is_some() && self.workers.is_empty() && self.server.is_none()
    }

    /// The way in: the app's name, a heading and a line on what it is, what the tailnet
    /// answered as a list to press, the address with its one primary action, and the other ways
    /// in as quiet links: the other panel, and on a Mac "Use this Mac as a worker", whose
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
        let field = adding.mode.field();
        let (title, blurb, go, other, other_mode) = match adding.mode {
            Panel::Server => (
                "Connect to a server",
                "A server on your tailnet or VPN lists your workers.",
                "Connect",
                "Add a worker by address instead",
                Panel::Worker,
            ),
            Panel::Worker => (
                "Add a worker",
                "A Mac or Linux worker on your tailnet or VPN.",
                "Add",
                "Connect to a server instead",
                Panel::Server,
            ),
        };
        let flow = adding.this_mac.as_ref();
        let sheet = adding.ssh.as_ref();
        // The checklist and the SSH sheet have headings of their own, and their link goes back
        // to the panel they came from, named as that panel's own link names it.
        let back = match adding.mode {
            Panel::Server => "Connect to a server instead",
            Panel::Worker => "Add a worker by address instead",
        };
        let (title, blurb, other) = match (flow, sheet) {
            (Some(_), _) => (this_mac::TITLE, this_mac::BLURB, back),
            (None, Some(sheet)) => (sheet.heading(), sheet.blurb(), back),
            (None, None) => (title, blurb, other),
        };
        let welcome = self.welcome();
        // The page leads with the app's mark over its heading, as Raycast's and Linear's first
        // screens do; a dialog over the workspace needs no sign.
        let brand = welcome.then(|| kit::brand(theme, None));
        let heading = div()
            .id("add-worker-title")
            .role(Role::Heading)
            .aria_label(title)
            .text_size(px(if welcome { ty.display() } else { ty.title() }))
            .font_weight(gpui::FontWeight(Typography::STRONG_WEIGHT))
            .text_color(hsla(s.text))
            .child(title);
        let intro = div().flex().flex_col().gap(px(spacing.xs)).child(heading).child(
            div()
                .id("add-worker-blurb")
                .debug_selector(|| "add-worker-blurb".to_owned())
                .text_size(px(ty.ui_size))
                .text_color(hsla(s.text_secondary))
                .child(blurb),
        );
        let tailnet =
            adding.search.as_ref().map(|search| self.tailnet_list(search, adding.mode, cx));
        let field_label = adding.mode.address_label(adding.search.as_ref());
        let unlisted = (!LISTS_TAILNET).then(|| {
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
        let go = button("add", go, ButtonKind::Primary)
            .h(px(FIELD_H))
            .when(stacked, gpui::Styled::w_full)
            .on_click(cx.listener(|this, _ev, _window, cx| this.add_from_panel(cx)));
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
            .bg(hsla(s.raised))
            .text_size(px(ty.ui_size))
            .child(
                Input::new(&adding.address)
                    .appearance(false)
                    .aria_label(field)
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
            .child(panel_label(theme, "add-worker-label", field_label))
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
        // At the body's size, as Cancel beside it is: one size for what can be pressed.
        let in_flow = flow.is_some() || sheet.is_some();
        let switch = button("panel-switch", other, ButtonKind::Link).on_click(cx.listener(
            move |this, _ev, window, cx| {
                if in_flow {
                    this.leave_this_mac(cx);
                    this.leave_ssh(cx);
                } else {
                    this.show_add_worker(other_mode, window, cx);
                }
            },
        ));
        // This Mac and a machine over SSH are more places to add, so they are rows to press as
        // the tailnet's are, in one frame, not links under the switch, where the likeliest first
        // step for a single Mac sat at the page's foot dressed as a way aside.
        // The server panel's rows set up a server, here or over SSH; the worker panel's add
        // this Mac as a worker, or set one up over SSH.
        // This Mac as a worker is the likeliest first step with nothing set up yet, so it leads
        // both panels. The server panel then offers to set up a server, here or over SSH; the
        // worker panel, a worker over SSH.
        let serving = adding.mode == Panel::Server;
        let this_mac_entry = (self.this_mac.is_some() && !in_flow).then(|| {
            this_mac_row(theme)
                .on_click(cx.listener(|this, _ev, window, cx| this.use_this_mac(window, cx)))
        });
        let ssh_entry = (!in_flow).then(|| self.ssh_row(cx)).flatten();
        let serve_entry = (serving && !in_flow).then(|| self.serve_here_row(cx)).flatten();
        let group =
            |id: &'static str, label_id: &'static str, label: &'static str, rows: Vec<_>| {
                (!rows.is_empty()).then(|| {
                    let frame = div()
                        .flex()
                        .flex_col()
                        .p(px(spacing.xxs))
                        .rounded(px(radii.md))
                        .bg(hsla(s.raised))
                        .children(rows);
                    div()
                        .id(id)
                        .flex()
                        .flex_col()
                        .gap(px(spacing.sm))
                        .child(panel_label(theme, label_id, label))
                        .child(frame)
                })
            };
        let (use_this_mac, set_up_server) = if serving {
            let server_rows = serve_entry.into_iter().chain(ssh_entry).collect();
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
                    server_rows,
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
        let ways = div().flex().flex_col().items_start().gap(px(spacing.xs)).child(switch);
        let aside = div().flex().items_start().child(ways).child(div().flex_1()).children(cancel);
        let checklist = flow.map(|flow| self.this_mac_checklist(flow, cx));
        let ssh_sheet = sheet.map(|sheet| self.ssh_sheet(sheet, cx));
        let panel = div()
            .id("add-worker")
            .debug_selector(|| "add-worker".to_owned())
            .occlude()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(spacing.lg))
            .w(px(ADD_PANEL_W))
            .max_w_full()
            .font_family(ty.ui_family.clone())
            .when(!welcome, |el| {
                kit::elevate(el, theme)
                    .p(px(spacing.xl))
                    .rounded(px(radii.lg))
                    .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
                    .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                        if this.adding.as_ref().is_some_and(|a| a.composing(cx)) {
                            return;
                        }
                        this.cancel_add_worker(window, cx);
                        cx.stop_propagation();
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
            .when_some(checklist, gpui::ParentElement::child)
            .when_some(ssh_sheet, gpui::ParentElement::child)
            .when(!in_flow, |el| {
                el.children(tailnet)
                    .children(unlisted)
                    .children(use_this_mac)
                    .children(set_up_server)
                    .child(entry)
            })
            .child(aside);
        if !welcome {
            // The scrim dims in as the dialog rises the base unit's half into place, both on
            // the overlay's pace, and under Reduce Motion both are there at once.
            let panel = kit::slide_fade(panel, "add-worker-rise", spacing.xs, kit::Pace::Fade, cx);
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
        use slopty_ui::icons::{IconName, IconSize, Status, icon, status_icon};
        let theme = &self.theme;
        let (s, spacing, ty) = (theme.surfaces, theme.spacing, &theme.typography);
        let section = div().id("add-worker-tailnet").flex().flex_col().gap(px(spacing.sm));
        if let Some(words) = search.words(mode) {
            let mark = px(ty.small());
            let mark = match search {
                Search::Looking => {
                    Some(status_icon(theme, Status::Running, mark, hsla(s.text_muted)))
                }
                Search::Answered { running: false, .. } => Some(
                    icon(theme, IconName::WifiOff, IconSize::Inline, hsla(s.text_muted))
                        .size(mark)
                        .into_any_element(),
                ),
                Search::Answered { .. } => None,
            };
            let step = search.next_step(mode);
            let said = step.map_or_else(|| words.to_owned(), |step| format!("{words}. {step}"));
            // Announced as the words, the step its description, shown as one run of text.
            let line = kit::meta(div(), theme)
                .id("add-worker-search")
                .debug_selector(|| "add-worker-search".to_owned())
                .role(Role::Status)
                .aria_label(words)
                .when_some(step, gpui::StatefulInteractiveElement::aria_description)
                .flex()
                .items_center()
                .gap(px(spacing.xs))
                .children(mark)
                .child(div().flex_1().min_w_0().child(said));
            return section.child(line).into_any_element();
        }
        let rows = search.offers(mode).into_iter().enumerate().map(|(ix, (host, offer))| {
            let target = offer.at.clone();
            found_row(theme, ix, host, offer).on_click(cx.listener(move |this, _ev, window, cx| {
                this.connect_found(host, &target, window, cx);
            }))
        });
        // The rows' radius plus the pad round them, so the corners nest.
        let frame = div()
            .flex()
            .flex_col()
            .p(px(spacing.xxs))
            .rounded(px(theme.radii.md))
            .bg(hsla(s.raised))
            .children(rows);
        let label = panel_label(theme, "add-worker-tailnet-label", "On your tailnet");
        section.child(label).child(frame).into_any_element()
    }

    /// This Mac's checklist: a line for each thing the worker needs, marked as its `doctor`
    /// reads it, the missing ones with the button that fixes them, in one list a tone step off
    /// the page as the tailnet's rows are; under it, the add under way or why it failed.
    fn this_mac_checklist(&self, flow: &this_mac::Flow, cx: &Context<Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let (s, spacing) = (theme.surfaces, theme.spacing);
        let logs =
            slopty_platform::service::Session::native().logs(slopty_platform::service::WORKER);
        let lines = this_mac::checklist(&flow.worker, &logs)
            .into_iter()
            .map(|line| self.this_mac_line(line, cx));
        let frame = div()
            .id("this-mac-checklist")
            .debug_selector(|| "this-mac-checklist".to_owned())
            .role(Role::List)
            .aria_label(this_mac::TITLE)
            .flex()
            .flex_col()
            .p(px(spacing.xxs))
            .rounded(px(theme.radii.md))
            .bg(hsla(s.raised))
            .children(lines);
        let status = match (&flow.error, flow.adding) {
            (Some(why), _) => Some((why.clone(), s.error)),
            (None, true) => Some(("Adding this Mac\u{2026}".to_owned(), s.text_muted)),
            (None, false) => None,
        };
        let again = flow.error.is_some().then(|| {
            let run = flow.run;
            kit::button(theme, "this-mac-add-again", "Try again", ButtonKind::Link).on_click(
                cx.listener(move |this, _ev, window, cx| this.add_this_mac(run, window, cx)),
            )
        });
        div()
            .flex()
            .flex_col()
            .gap(px(spacing.sm))
            .child(frame)
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
            .into_any_element()
    }

    /// One line of this Mac's checklist: its mark, its name over what it is for or what to do,
    /// and a missing line's button.
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
        // A line's detail wraps rather than cut off a path or a reason, so the line grows from
        // a two-line row's height instead of holding it.
        kit::inset_x(div(), theme)
            .id(gpui::ElementId::Name(format!("this-mac-{}", check.slug()).into()))
            .debug_selector(move || format!("this-mac-{}", check.slug()))
            .role(Role::ListItem)
            .aria_label(check.title())
            .aria_description(SharedString::from(line.detail.clone()))
            .flex()
            .items_start()
            .gap(px(theme.spacing.sm))
            .min_h(px(kit::Row::Two.height(theme)))
            .py(px(theme.spacing.xs))
            .child(status_mark(theme, Some(status), 1.0))
            .child(
                // The fix sits on the title's row, so the detail below takes the full width.
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .justify_between()
                            .gap(px(theme.spacing.sm))
                            .child(
                                div()
                                    .min_w_0()
                                    .text_size(px(theme.typography.ui_size))
                                    .font_weight(gpui::FontWeight(Typography::MEDIUM_WEIGHT))
                                    .text_color(hsla(if muted { s.text_secondary } else { s.text }))
                                    .child(check.title()),
                            )
                            .children(fix),
                    )
                    .child(kit::meta(div(), theme).child(SharedString::from(line.detail))),
            )
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
            .border_t_1()
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

    /// A key cap of the bar: a plate of `raised` with no hairline, `overlay` while pressed, the
    /// accent fill with its ink when `lit` (armed or toggled on). A word ("Esc", "Paste") is
    /// set small, as a keyboard sets its word keys; a glyph at the title size, so an arrow
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
                    el.text_color(hsla(s.text)).bg(hsla(s.raised))
                }
            })
            .when(!lit, |el| el.active(|el| el.bg(hsla(s.overlay))))
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
        tab_stop(key, self.theme.surfaces.accent)
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
        let accent = self.theme.surfaces.accent;
        let view = terminal.read(cx);
        let (armed, armed_command) = (view.sticky_control(), view.sticky_command());
        let (has_selection, finding) = (view.selection().is_some(), view.finding());
        let mut keys: Vec<(&'static str, gpui::AnyElement)> =
            Vec::with_capacity(BAR_KEYS.len().saturating_add(2));
        for (label, key, typed) in BAR_KEYS {
            let is_control = key.is_empty();
            let is_command = key == "cmd";
            let lit = (is_control && armed) || (is_command && armed_command);
            let target = terminal.clone();
            let el = self.bar_key(format!("key-{label}"), label, lit, move |_window, cx| {
                target.update(cx, |t, cx| {
                    if is_control {
                        let on = !t.sticky_control();
                        t.set_sticky_control(on, cx);
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
        let clipboard = tab_stop(clipboard, accent).on_click(move |_ev, window, cx| {
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
        let find = tab_stop(find, accent).on_click(move |_ev, window, cx| {
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
    Option<slopty_net::HostAddr>,
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
        "Ctrl" => "Control",
        "⌘" => "Command",
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
        let settings_editor = self.settings_editor.clone();
        let welcome = self.welcome();
        // The key bar takes the status bar's row above the keyboard. Told only a change: an
        // update while the window draws would build everything that read the workspace again.
        let shown = key_bar.is_some();
        if self.told != shown {
            self.told = shown;
            self.view.update(cx, |v, cx| v.set_key_bar_shown(shown, cx));
        }
        let adding = self.adding.as_ref().map(|adding| self.add_worker_panel(adding, window, cx));
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
            .on_action(cx.listener(|this, _: &DisconnectServer, window, cx| {
                this.disconnect_server(window, cx);
            }))
            .on_action(cx.listener(|this, _: &UseThisMac, window, cx| {
                this.use_this_mac(window, cx);
            }))
            .on_action(cx.listener(|this, _: &InstallOverSsh, window, cx| {
                this.open_ssh(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowWorkersInFinder, _window, cx| {
                this.show_workers_in_finder(cx);
            }))
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .on_action(cx.listener(|this, _: &OpenKeyboardShortcuts, window, cx| {
                this.open_settings(window, cx);
                if let Some(editor) = &this.settings_editor {
                    let keyboard = slopty_ui::settings_form::schema::Section::Keyboard;
                    editor.update(cx, |e, cx| e.show_section(keyboard, window, cx));
                }
            }))
            .when(!welcome, |el| {
                el.child(div().flex_1().w_full().min_h_0().child(self.view.clone()))
                    .when_some(key_bar, |el, bar| {
                        el.child(div().w_full().pl(insets.left).pr(insets.right).child(bar))
                    })
                    // The home indicator's band continues the bar above it: the key bar on the
                    // body's surface, else the status bar on `canvas`.
                    .child(div().w_full().h(insets.bottom).bg(hsla(band)))
            })
            .when_some(adding, gpui::ParentElement::child)
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
        LinkEvent::Control(WorkerMsg::Agent(event)) => {
            view.update(cx, |v, cx| v.agent_event(event, cx));
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
        LinkEvent::Control(WorkerMsg::HooksInstalled { ok, message }) => {
            view.update(cx, |v, cx| {
                v.show_notice(message, cx);
                if !ok {
                    // The offer stays on the header so it can be tried again.
                    v.hooks_offer_failed(key, cx);
                }
            });
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
        LinkEvent::Conversation { session, event } => {
            view.update(cx, |v, cx| v.conversation_event(session, event, cx));
        }
        LinkEvent::Control(WorkerMsg::Permission(event)) => {
            view.update(cx, |v, cx| v.permission_event(event, cx));
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
    use slopty_ui::icons::{IconName, IconSize, icon};
    let s = theme.surfaces;
    let (glyph, what, verb) = match host {
        Host::Server => (IconName::Server, "Server", "Connect to"),
        Host::Worker => (IconName::Monitor, "Worker", "Add"),
    };
    let glyph_size = px(theme.typography.icon());
    let row = kit::row(theme, kit::Row::Two)
        .id(("add-worker-found", ix))
        .debug_selector(move || format!("add-worker-found-{ix}"))
        .role(Role::Button)
        .aria_label(SharedString::from(format!("{verb} {}", offer.name)))
        .rounded(px(theme.radii.sm))
        .cursor_pointer()
        // On the list's `raised` step: the pointer washes a row one step up.
        .hover(move |el| el.bg(hsla(s.overlay)))
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
                        .child(SharedString::from(format!("{what} \u{b7} {}", offer.at))),
                ),
        )
        .child(
            icon(theme, IconName::ChevronRight, IconSize::Inline, hsla(s.text_muted))
                .size(glyph_size),
        );
    tab_stop(row, s.accent)
}

/// The label over this Mac's row on the add panels.
const THIS_MAC_LABEL: &str = "On this Mac";
/// The label over this Mac's row and the SSH row together.
const SET_UP_LABEL: &str = "Set up a worker";
/// The label over the server panel's rows: the server on this Mac, or over SSH.
const SET_UP_SERVER_LABEL: &str = "Set up the server";

/// This Mac as a row to press, drawn as a found worker's is: the Mac's glyph, what pressing
/// does over what follows, and the chevron that says the press goes on to a checklist.
fn this_mac_row(theme: &Theme) -> gpui::Stateful<gpui::Div> {
    use slopty_ui::icons::IconName;
    entry_row(theme, "use-this-mac", IconName::Monitor, this_mac::TITLE, this_mac::ROW_META)
}

/// A way to add a worker that goes on to a sheet of its own, as a row to press drawn as a found
/// worker's is: its glyph, what pressing does over what follows, and the chevron that says the
/// press goes on.
fn entry_row(
    theme: &Theme,
    id: &'static str,
    glyph: slopty_ui::icons::IconName,
    title: &'static str,
    meta: &'static str,
) -> gpui::Stateful<gpui::Div> {
    use slopty_ui::icons::{IconName, IconSize, icon};
    let s = theme.surfaces;
    let glyph_size = px(theme.typography.icon());
    let row = kit::row(theme, kit::Row::Two)
        .id(id)
        .debug_selector(move || id.to_owned())
        .role(Role::Button)
        .aria_label(title)
        .rounded(px(theme.radii.sm))
        .cursor_pointer()
        // On the list's `raised` step: the pointer washes a row one step up.
        .hover(move |el| el.bg(hsla(s.overlay)))
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
                .child(kit::meta(div(), theme).child(meta)),
        )
        .child(
            icon(theme, IconName::ChevronRight, IconSize::Inline, hsla(s.text_muted))
                .size(glyph_size),
        );
    tab_stop(row, s.accent)
}

/// The app's own commands, bound outside any view's context: its rows of the keymap's table
/// (`[keys.app]`), which lives in `slopty_ui::keymap` beside the rest.
fn app_commands() -> Vec<slopty_ui::keymap::Command> {
    use slopty_ui::keymap::app_command;
    let mut commands = vec![
        app_command("open_settings", OpenSettings, &["cmd-,"]),
        app_command("add_worker", AddWorker, &["cmd-shift-h"]),
        app_command("connect_server", ConnectServer, &[]),
        app_command("disconnect_server", DisconnectServer, &[]),
    ];
    if this_mac::OFFERED {
        commands.push(app_command("use_this_mac", UseThisMac, &[]));
    }
    if ssh::OFFERED {
        commands.push(app_command("install_over_ssh", InstallOverSsh, &[]));
    }
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

/// The app's lines for the command palette, after the workspace's.
fn app_palette_items() -> Vec<slopty_ui::palette::PaletteItem> {
    use slopty_ui::icons::IconName;

    let bindings = app_key_bindings();
    let item = |label: &str, icon: IconName, action: Box<dyn gpui::Action>| {
        slopty_ui::palette::PaletteItem::new(label, icon, action, &bindings)
    };
    let mut items = vec![
        item("Open settings", IconName::Settings, Box::new(OpenSettings)),
        item("Connect to a server", IconName::Link, Box::new(ConnectServer)),
        item("Disconnect from the server", IconName::Unplug, Box::new(DisconnectServer)),
        item("Add a worker", IconName::Plus, Box::new(AddWorker)),
    ];
    if this_mac::OFFERED {
        items.push(item(this_mac::TITLE, IconName::Monitor, Box::new(UseThisMac)));
    }
    if ssh::OFFERED {
        items.push(item(ssh::TITLE, IconName::Terminal, Box::new(InstallOverSsh)));
    }
    if finder::OFFERED {
        items.push(item(finder::TITLE, IconName::FolderOpen, Box::new(ShowWorkersInFinder)));
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
    // Under the self-test a frame is a step, not a moment: the layout lands at once so a
    // `dump` reads where things went, not where they were passing through. Each run starts
    // from an empty layout there, so no test depends on the last one's.
    let saved = if cfg!(feature = "e2e") {
        None
    } else {
        slopty_ui::workspace::read_layout(&layout_path())
    };
    let view = cx.new(|cx| {
        let mut view = WorkspaceView::new(Theme::default(), saved, cx);
        view.extend_palette(app_palette_items());
        view.set_layout_path(layout_path());
        // Beside the layout: the file tiles' edits not yet saved, taken back after a quit or a
        // crash.
        view.set_unsaved_store(
            slopty_client::unsaved::Store::new(slopty_platform::dirs::data_dir().join("unsaved")),
            cx,
        );
        // Each worker's threads, so an agent's thread draws in its first frame.
        view.set_thread_cache(slopty_platform::dirs::data_dir().join("threads"));
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
    // GPUI's own animations hold still as the system asks, as Slopty's do, and follow the
    // setting as the system says it changed ([`Workspace::set_reduce_motion`]).
    cx.set_reduce_motion(slopty_platform::reduce_motion());
    watch_reduce_motion(&workspace, cx);
    // A self-test draws the standard chrome whatever this Mac's setting, so its frames compare.
    if !self_test() {
        watch_increase_contrast(&workspace, cx);
    }
    watch_resumes(&workspace, &slopty_platform::resume::System, cx);
    hangs::watch(cx);
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
    // Every worker added by address gets a link now, and the server's (cached) directory has
    // been read by the settings above; with neither, the panel.
    let known = match net::known_workers() {
        Ok(known) => known,
        Err(e) => {
            tracing::error!(error = %e, "known workers");
            Vec::new()
        }
    };
    window.update(cx, |_root, window, cx| {
        workspace.update(cx, |ws, cx| {
            for worker in known {
                ws.add_worker(worker.worker_id, worker.name, true, cx);
            }
            ws.refresh_menu(cx);
            if ws.workers.is_empty() && ws.server.is_none() {
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

/// Derive the chrome for the system's contrast setting, and again each time it changes.
fn watch_increase_contrast(workspace: &Entity<Workspace>, cx: &mut App) {
    let contrast = |on: bool| {
        if on { slopty_theme::Contrast::Increased } else { slopty_theme::Contrast::Standard }
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let watch = slopty_platform::motion::watch_increase_contrast(move |on| {
        let _closed = tx.send(on);
    });
    let now = contrast(slopty_platform::motion::increase_contrast());
    workspace.update(cx, |ws, cx| {
        ws.contrast_watch = Some(watch);
        ws.set_contrast(now, cx);
    });
    let workspace = workspace.downgrade();
    cx.spawn(async move |cx| {
        while let Some(on) = rx.recv().await {
            if workspace.update(cx, |ws, cx| ws.set_contrast(contrast(on), cx)).is_err() {
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

/// The editor's "Open in editor" button: the file in the default `.toml` editor, written with
/// the commented defaults first when there is none.
fn open_settings_file(cx: &App) {
    let path = slopty_settings::path();
    match Settings::init(&path) {
        Ok(true) => tracing::info!(path = %path.display(), "wrote default settings"),
        Ok(false) => {}
        Err(e) => tracing::warn!(path = %path.display(), error = %e, "write default settings"),
    }
    cx.open_with_system(&path);
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
        assert_eq!(key_label("Ctrl", true), "Control, armed");
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
        assert!(key_row_overflow(terminal(), 744.0, spacing).abs() < f32::EPSILON, "an iPad");
    }

    /// The caps keep their widths: the terminal's row runs past a 402 pt phone, so it scrolls,
    /// and fits the narrowest iPad (744 pt) with room to spare rather than stretching to it.
    #[test]
    fn the_key_caps_keep_their_width() {
        let spacing = Theme::default().spacing;
        let labels = BAR_KEYS.iter().map(|(label, ..)| *label).chain(["Paste", "Find"]);
        let row = key_row_width(labels, spacing);
        assert!(row > 402.0 && row < 744.0, "{row}");
        assert!((cap_width("|", spacing) - cap_side(spacing)).abs() < f32::EPSILON, "square");
        assert!(cap_side(spacing) < KEY_BAR_H, "a cap sits inside its bar");
    }

    /// Every bar fits the narrowest iPad (744 pt) as one run of groups with its word keys
    /// trailing, and a phone's (402 pt) never does, so it scrolls.
    #[test]
    fn every_bar_spreads_on_an_ipad_and_scrolls_on_a_phone() {
        let spacing = Theme::default().spacing;
        let terminal = || BAR_KEYS.iter().map(|(label, ..)| *label).chain(["Paste", "Find"]);
        let screen = || SCREEN_BAR_KEYS.iter().map(|(label, ..)| *label).chain(["Copy", "Paste"]);
        for row in [spread_width(terminal(), spacing), spread_width(screen(), spacing)] {
            assert!(row <= 744.0 && row > 402.0, "{row}");
        }
        assert_eq!(key_group("Ctrl"), KeyGroup::Lead, "a word, but the soft keyboard's lack");
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
        assert!((gap(cx, "Tab", "Ctrl") - spacing.xs).abs() < 0.5, "caps in a group");
        assert!((gap(cx, "⌘", "←") - spacing.md).abs() < 0.5, "the arrows follow the lead");
        assert!((gap(cx, "→", "~") - spacing.md).abs() < 0.5, "the symbols follow the arrows");
        assert!((f32::from(at(cx, "Find").right()) - (1032.0 - spacing.xs)).abs() < 0.5);
        assert!(gap(cx, "-", "Paste") > 200.0, "the word keys trail: {}", gap(cx, "-", "Paste"));

        cx.simulate_resize(size(px(402.0), px(200.0)));
        cx.run_until_parked();
        assert!(cx.debug_bounds("key-bar-spread").is_none(), "a phone's row scrolls");
        assert!((gap(cx, "→", "~") - spacing.xs).abs() < 0.5, "in the given order");
    }

    /// The shell in a headless window with the add-worker panel up, `worker` ones known so
    /// it is a dialog over the workspace, else the first run. The runtime and the directory
    /// hold what the shell's tasks and settings file need for the test's length.
    pub(crate) fn shell<'a>(
        cx: &'a mut TestAppContext,
        runtime: &tokio::runtime::Runtime,
        dir: &tempfile::TempDir,
        worker: bool,
    ) -> (Entity<Workspace>, &'a mut VisualTestContext) {
        let ws = workspace(cx, runtime, dir);
        if worker {
            ws.update(cx, |ws, _cx| {
                ws.workers.push(WorkerSlot::new(WorkerId::new(), "studio".to_owned(), true));
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
            _server: Option<slopty_deploy::Server>,
            _events: tokio::sync::mpsc::UnboundedSender<slopty_deploy::Event>,
        ) -> this_mac::Pending<Result<slopty_deploy::Deployed, slopty_deploy::Failure>> {
            panic!("this test deploys nothing")
        }

        fn add(&self, _address: &str) -> this_mac::Pending<Result<net::Added, String>> {
            panic!("this test adds nothing")
        }

        fn remember(&self, _worker: WorkerId, _to: &ssh::Target) {}

        fn target_of(&self, _worker: WorkerId) -> Option<ssh::Target> {
            None
        }
    }

    /// An address that answers as another build is no dead end in the panel: the add says so
    /// as an [`net::OtherBuild`], which opens the SSH sheet on that host to install this build
    /// there, the panel no longer busy (`ssh::tests` checks the sheet's host field).
    #[gpui::test]
    fn a_worker_on_another_build_opens_the_install_on_its_host(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, true);
        let window = cx.update(|window, _cx| window.window_handle());
        ws.update(cx, |ws, _cx| {
            ws.window = Some(window);
            ws.deployer = Some(Rc::new(NoDeploys));
        });
        let notice = slopty_client::update::UpdateNotice {
            of: slopty_client::update::Of::Worker,
            host: "mini.local".to_owned(),
            peer: "0.0.1".to_owned(),
        };
        let failed = anyhow::Error::new(net::OtherBuild(notice));
        let other = failed.downcast_ref::<net::OtherBuild>().expect("typed through anyhow");
        assert!(failed.to_string().contains(&other.0.detail()), "{failed}");
        let host = other.0.host.clone();
        ws.update(cx, |ws, cx| {
            if let Some(adding) = &mut ws.adding {
                adding.busy = true;
            }
            ws.install_this_build_at(host, cx);
        });
        cx.run_until_parked();
        let (busy, sheet) = ws.read_with(cx, |ws, _cx| {
            let adding = ws.adding.as_ref().expect("the panel is up");
            (adding.busy, adding.ssh.is_some())
        });
        assert!(!busy, "no longer waiting on the add");
        assert!(sheet && cx.debug_bounds("ssh-form").is_some(), "the sheet, drawn");
    }

    /// Both panels lead with this Mac as a worker, the likeliest first step; the server panel
    /// then offers to set up a server, here or over SSH, and the worker panel a worker over SSH.
    #[gpui::test]
    fn the_server_panel_offers_this_mac_and_a_server_to_set_up(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, true);
        cx.simulate_resize(size(px(900.0), px(800.0)));
        let host = Rc::new(StandIn {
            asked: std::cell::RefCell::default(),
            doctor: std::cell::RefCell::new(None),
            added: WorkerId::new(),
        });
        ws.update(cx, |ws, _cx| {
            ws.deployer = Some(Rc::new(NoDeploys));
            ws.this_mac = Some(host);
        });
        let shown = |cx: &mut VisualTestContext, panel: Panel| {
            cx.update(|window, cx| ws.update(cx, |ws, cx| ws.show_add_worker(panel, window, cx)));
            cx.run_until_parked();
            ["serve-here", "serve-over-ssh", "use-this-mac", "install-over-ssh"]
                .map(|row| cx.debug_bounds(row).is_some())
        };
        assert_eq!(shown(cx, Panel::Server), [true, true, true, false], "the server's rows");
        assert_eq!(shown(cx, Panel::Worker), [false, false, true, true], "the worker's rows");
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
            if welcome {
                ws.update(cx, |ws, _cx| ws.workers.clear());
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

    /// The dialog rises into place as its scrim dims in; under Reduce Motion it is in place on
    /// its first frame.
    #[gpui::test]
    fn the_dialog_rises_into_place_unless_motion_is_reduced(cx: &mut TestAppContext) {
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
        // The test runs on this Mac, whose own Reduce Motion the kit reads as well.
        if cx.update(|_window, cx| kit::motion(cx)) {
            let rising = first_top(cx);
            assert!(rising > still + 0.5, "below its place on its first frame: {rising}");
        }
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

    /// Esc while an input method composes in the address field is the input method's: the
    /// panel stays. Once the word is committed, Esc closes it.
    #[gpui::test]
    fn esc_mid_word_leaves_the_panel_open(cx: &mut TestAppContext) {
        use gpui::EntityInputHandler as _;
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, true);
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

    /// Each panel's field gives an example of its own kind of host, and switching panels
    /// switches it. While it looks on the tailnet it says so; each server that answered is a row
    /// to connect to under the blurb and over the field, the best one's address in the field;
    /// when none did it says that, and with no tailnet here it says so. The first run carries
    /// the app's name over its heading.
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
        assert_eq!(example(cx).as_deref(), Some(Panel::Worker.example()));
        cx.update(|window, cx| {
            ws.update(cx, |ws, cx| ws.show_add_worker(Panel::Server, window, cx));
        });
        cx.run_until_parked();
        assert_eq!(example(cx).as_deref(), Some(Panel::Server.example()));
        assert_ne!(Panel::Server.example(), Panel::Worker.example());

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
        let named = ws.read_with(cx, |ws, _| {
            ws.adding.as_ref().map(|a| a.mode.address_label(a.search.as_ref()))
        });
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
        assert_eq!(words(cx).as_deref(), Some(NOTHING_ANSWERED));
        let label = ws.read_with(cx, |ws, _| {
            ws.adding.as_ref().map(|a| a.mode.address_label(a.search.as_ref()))
        });
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

    /// A tailnet with workers and no server still has a way in: the first run offers each
    /// worker that answered as a row after the servers, the field keeping to servers, and a
    /// worker's own panel offers only the workers. Pressing one adds it at once, the panel
    /// turning into the worker's so a failure is told beside its address.
    #[gpui::test]
    fn the_first_run_offers_the_workers_the_tailnet_found(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, false);
        cx.update(|window, cx| {
            ws.update(cx, |ws, cx| ws.show_add_worker(Panel::Server, window, cx));
        });
        let found = |name: &str, last: u8| slopty_net::discover::Found {
            name: name.to_owned(),
            addr: std::net::SocketAddr::from(([100, 64, 0, last], 7_001)),
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
            workers: vec![found("mac-studio", 3), found("macbook", 4)],
            running: true,
        };
        answer(&ws, tailnet.clone(), cx);
        let rows = |cx: &mut VisualTestContext| {
            ["add-worker-found-0", "add-worker-found-1", "add-worker-found-2", "add-worker-found-3"]
                .into_iter()
                .filter(|row| cx.debug_bounds(row).is_some())
                .count()
        };
        assert_eq!(rows(cx), 3, "the server, then both workers");
        let offers = ws.read_with(cx, |ws, _| {
            let adding = ws.adding.as_ref().unwrap();
            let search = adding.search.as_ref().unwrap();
            search.offers(adding.mode).iter().map(|(h, o)| (*h, o.name.clone())).collect::<Vec<_>>()
        });
        assert_eq!(
            offers,
            [
                (Host::Server, "home-server".to_owned()),
                (Host::Worker, "mac-studio".to_owned()),
                (Host::Worker, "macbook".to_owned()),
            ]
        );

        // Workers alone: nothing goes in a server's field, and no "nothing answered".
        let workers_only =
            net::Tailnet { servers: Vec::new(), workers: tailnet.workers.clone(), running: true };
        let field = |cx: &mut VisualTestContext| {
            ws.read_with(cx, |ws, cx| {
                ws.adding.as_ref().map(|a| a.address.read(cx).value().to_string())
            })
        };
        cx.update(|window, cx| {
            ws.update(cx, |ws, cx| {
                if let Some(adding) = &ws.adding {
                    adding.address.update(cx, |input, cx| input.set_value("", window, cx));
                }
            });
        });
        answer(&ws, workers_only, cx);
        assert_eq!(rows(cx), 2);
        assert_eq!(field(cx).as_deref(), Some(""), "a server's field takes no worker");
        assert!(cx.debug_bounds("add-worker-search").is_none(), "something did answer");

        // A worker's panel offers the workers, the best one ready in its field.
        cx.update(|window, cx| {
            ws.update(cx, |ws, cx| ws.show_add_worker(Panel::Worker, window, cx));
        });
        answer(&ws, tailnet, cx);
        assert_eq!(rows(cx), 2, "no server in a worker's panel");
        assert_eq!(field(cx).as_deref(), Some("100.64.0.3"));

        // A press adds that worker straight away, from a server's panel too.
        cx.update(|window, cx| {
            ws.update(cx, |ws, cx| ws.show_add_worker(Panel::Server, window, cx));
        });
        cx.run_until_parked();
        let second = cx.debug_bounds("add-worker-found-2").expect("the second worker's row");
        cx.simulate_click(second.center(), gpui::Modifiers::none());
        let (mode, busy) =
            ws.read_with(cx, |ws, _| ws.adding.as_ref().map(|a| (a.mode, a.busy)).unwrap());
        assert_eq!((mode, busy), (Panel::Worker, true), "adding it, in the worker's panel");
        assert_eq!(field(cx).as_deref(), Some("100.64.0.4"));
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
    #[derive(Debug)]
    struct StandIn {
        asked: std::cell::RefCell<Vec<String>>,
        doctor: std::cell::RefCell<Option<this_mac::Doctor>>,
        added: WorkerId,
    }

    impl StandIn {
        fn ask(&self, what: String) {
            self.asked.borrow_mut().push(what);
        }

        fn asked(&self) -> Vec<String> {
            std::mem::take(&mut *self.asked.borrow_mut())
        }
    }

    impl this_mac::Host for StandIn {
        fn install(&self) -> this_mac::Pending<Result<(), String>> {
            self.ask("install".to_owned());
            Box::pin(async { Ok(()) })
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

        fn add(&self, address: &str) -> this_mac::Pending<Result<net::Added, String>> {
            self.ask(format!("add {address}"));
            let added = net::Added { id: self.added, name: "mac-studio".to_owned() };
            Box::pin(async move { Ok(added) })
        }

        fn open(&self, pane: this_mac::Pane) {
            self.ask(format!("open {pane:?}"));
        }
    }

    /// "Use this Mac as a worker" on the first run installs through the host, then shows the
    /// worker's `doctor` as the checklist: the missing grant's button opens its own pane. Back
    /// from System Settings the worker is started again and asked again, and once it may
    /// stream and take input this Mac is added over loopback and the panel gives way to its
    /// workspace.
    #[gpui::test]
    fn this_mac_installs_then_is_added_once_its_doctor_is_green(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, false);
        cx.simulate_resize(size(px(900.0), px(700.0)));
        let studio = this_mac::Tailnet::Reachable("mac-studio.tail1234.ts.net".to_owned());
        let lacking = this_mac::Doctor {
            version: "0.3.0".to_owned(),
            screen_recording: false,
            accessibility: true,
            tailnet: studio,
        };
        let host = Rc::new(StandIn {
            asked: std::cell::RefCell::default(),
            doctor: std::cell::RefCell::new(Some(lacking.clone())),
            added: WorkerId::new(),
        });
        let shared: Rc<dyn this_mac::Host> = Rc::<StandIn>::clone(&host);
        ws.update(cx, |ws, cx| {
            ws.this_mac = Some(shared);
            cx.notify();
        });
        cx.run_until_parked();
        assert!(ws.read_with(cx, |ws, _| ws.welcome()), "the first run");

        let entry = cx.debug_bounds("use-this-mac").expect("the entry, a row to press");
        let field = cx.debug_bounds("add-worker-field").expect("the address");
        let switch = cx.debug_bounds("panel-switch").expect("the other way in");
        assert!(entry.bottom() <= field.top(), "a place to add, over the address to type");
        assert!(entry.bottom() <= switch.top(), "not a second link under the way aside");
        cx.simulate_click(entry.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert_eq!(host.asked(), ["install", "doctor"], "installed, then asked");
        assert!(cx.debug_bounds("this-mac-checklist").is_some(), "the checklist is up");
        assert!(cx.debug_bounds("add-worker-field").is_none(), "in the address's place");
        assert!(cx.debug_bounds("use-this-mac").is_none(), "the entry has done its part");
        assert!(cx.debug_bounds("this-mac-fix-accessibility").is_none(), "granted: no button");
        let open = cx.debug_bounds("this-mac-fix-screen").expect("Screen Recording's button");
        cx.simulate_click(open.center(), gpui::Modifiers::none());
        assert_eq!(host.asked(), ["open ScreenRecording"], "its own pane");

        // Granted in System Settings; the app comes back to the front.
        *host.doctor.borrow_mut() = Some(this_mac::Doctor { screen_recording: true, ..lacking });
        cx.update(|window, cx| ws.update(cx, |ws, cx| ws.this_mac_activated(window, cx)));
        cx.run_until_parked();
        assert_eq!(
            host.asked(),
            ["restart", "doctor", "add 127.0.0.1"],
            "started again to see the grant, then added over loopback"
        );
        let (adding, added) = ws.read_with(cx, |ws, _| {
            (ws.adding.is_some(), ws.workers.iter().any(|w| w.id == host.added && w.added))
        });
        assert!(!adding && added, "the panel gave way to this Mac's workspace");
    }

    /// A failed install is the checklist's first line, red with a way to try again; back to
    /// the panel leaves the checklist for the address.
    #[gpui::test]
    fn a_failed_install_offers_another_try(cx: &mut TestAppContext) {
        #[derive(Debug)]
        struct Refusing;
        impl this_mac::Host for Refusing {
            fn install(&self) -> this_mac::Pending<Result<(), String>> {
                Box::pin(async { Err("launchctl bootstrap failed".to_owned()) })
            }

            fn doctor(&self) -> this_mac::Pending<Option<this_mac::Doctor>> {
                Box::pin(async { None })
            }

            fn restart(&self) -> this_mac::Pending<()> {
                Box::pin(async {})
            }

            fn add(&self, _address: &str) -> this_mac::Pending<Result<net::Added, String>> {
                Box::pin(async { Err("no".to_owned()) })
            }

            fn open(&self, _pane: this_mac::Pane) {}
        }
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, true);
        cx.simulate_resize(size(px(900.0), px(700.0)));
        ws.update(cx, |ws, _cx| ws.this_mac = Some(Rc::new(Refusing)));
        cx.dispatch_action(UseThisMac);
        cx.run_until_parked();
        let worker = ws.read_with(cx, |ws, _| {
            ws.adding.as_ref().and_then(|a| a.this_mac.as_ref()).map(|f| f.worker.clone())
        });
        assert_eq!(
            worker,
            Some(this_mac::Worker::Failed("launchctl bootstrap failed".to_owned())),
            "the reason, kept"
        );
        assert!(cx.debug_bounds("this-mac-fix-running").is_some(), "a way to try again");
        let back = cx.debug_bounds("panel-switch").expect("the way back");
        cx.simulate_click(back.center(), gpui::Modifiers::none());
        assert!(cx.debug_bounds("this-mac-checklist").is_none(), "left");
        assert!(cx.debug_bounds("add-worker-field").is_some(), "the address is back");
    }

    /// The palette offers it on a Mac.
    #[test]
    fn the_palette_offers_this_mac_on_a_mac() {
        let offered = app_palette_items().iter().any(|item| item.label == this_mac::TITLE);
        assert_eq!(offered, this_mac::OFFERED);
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
        ws.update(cx, |ws, cx| ws.add_worker(id, "mini".to_owned(), true, cx));
        cx.run_until_parked();
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
