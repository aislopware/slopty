//! The Slopty app shell, shared by the macOS and iOS apps.
//!
//! Connects to every added worker at once and shows all of them in one workspace: each
//! worker's items are tiles in this device's layout, side by side with the others'.
//! Networking runs on a tokio runtime thread; GPUI owns the main thread. The two talk through
//! channels only. The platform binaries set up logging, the runtime and the GPUI application,
//! then call [`open_workspace`].

#![forbid(unsafe_code)]

mod e2e;
pub mod net;
mod server;
pub mod settings;
pub mod workers;

use std::rc::Rc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, WindowOptions, div, px,
};
use gpui_kit::component::Root;
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
pub use settings::actions::OpenSettings;
use slopty_client::LinkEvent;
use slopty_client::layout::WorkerKey;
use slopty_core::{SessionId, WorkerId};
use slopty_proto::WorkerMsg;
use slopty_settings::{Loaded, Settings};
use slopty_theme::{Density, Spacing, Theme, Typography};
use slopty_ui::a11y::tab_stop;
use slopty_ui::colors::hsla;
use slopty_ui::kit::{self, ButtonKind};
use slopty_ui::screen::{ScreenView, Sticky};
use slopty_ui::settings_editor::{SettingsEditor, SettingsEditorEvent};
use slopty_ui::terminal::TerminalView;
use slopty_ui::workspace::{
    HostActions, KeyTarget, MenuEntry, MenuGroup, MenuRun, WorkerLink, WorkerStatus,
    WorkspaceEvent, WorkspaceView,
};
pub use workers::actions::{AddWorker, ConnectServer, DisconnectServer};
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
    ("⌃", "", None),
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

/// Where a key sits on a row wide enough to spread the bar out (an iPad's): the keys the soft
/// keyboard lacks at the left, the arrows in the middle, the symbols and the word keys (Copy,
/// Paste, Find) at the right, as a hardware keyboard's own groups sit apart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum KeyGroup {
    /// Esc, Tab and the sticky modifiers.
    Lead,
    /// The four arrows.
    Arrows,
    /// Shell symbols, then the word keys.
    Trail,
}

/// The group `label`'s key belongs to.
fn key_group(label: &str) -> KeyGroup {
    match label {
        "Esc" | "Tab" | "⌃" | "⌘" => KeyGroup::Lead,
        "←" | "↑" | "↓" | "→" => KeyGroup::Arrows,
        _ => KeyGroup::Trail,
    }
}

/// How wide a row of `labels`' caps is, a gap between each and at its ends.
fn key_row_width<'a>(labels: impl IntoIterator<Item = &'a str>, spacing: Spacing) -> f32 {
    let (count, caps) = labels
        .into_iter()
        .fold((0.0_f32, 0.0_f32), |(n, w), label| (n + 1.0, w + cap_width(label, spacing)));
    2.0_f32.mul_add(spacing.xs, (count - 1.0).max(0.0).mul_add(spacing.xs, caps))
}

/// Which ends of the key row fade, as `(leading, trailing)`: the leading one once the row is
/// scrolled off its start, the trailing one while keys remain past the edge. `scrolled` is how
/// far the row is scrolled in and `max` how far it can be (zero when it fits).
fn key_bar_fades(scrolled: f32, max: f32) -> (bool, bool) {
    (scrolled > AT_END, scrolled < max - AT_END)
}
/// The key bar over a remote window: ⌘ joins ⌃ (an IDE lives on chords), the shell
/// punctuation goes.
const SCREEN_BAR_KEYS: [(&str, &str, Option<&str>); 9] = [
    ("Esc", "escape", None),
    ("Tab", "tab", None),
    ("⌃", "", None),
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

    /// The line under [`Self::words`]: what to do about it. Nothing while it looks.
    const fn next_step(&self, mode: Panel) -> Option<&'static str> {
        match (self, mode) {
            (Self::Looking, _) => None,
            (Self::Answered { running: false, .. }, _) => {
                Some("Start it here, or type an address on your VPN.")
            }
            (Self::Answered { .. }, Panel::Server) => {
                Some("Start the Slopty server on a machine there, or type its address.")
            }
            (Self::Answered { .. }, Panel::Worker) => {
                Some("Start the Slopty worker on a Mac there, or type its address.")
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
    theme: Theme,
    /// The user's `settings.toml` as last loaded (defaults when absent or broken).
    settings: Settings,
    /// The window's appearance is dark (`theme.appearance = "system"` follows it).
    window_dark: bool,
    subscriptions: Vec<gpui::Subscription>,
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
    /// Focus the editor's field on the next frame (it needs a frame to exist).
    pending_focus_editor: bool,
    /// The self-test's stand-in for iPad Split View and Stage Manager: the app laid out in
    /// this size at the window's top left. A UIKit window cannot be resized from inside the
    /// app, and the layout only needs the size it is given to be the size it lays out in.
    split_view: Option<gpui::Size<gpui::Pixels>>,
    /// Where the key row is scrolled to, and how far it can go (from its last layout).
    key_bar_scroll: ScrollHandle,
    /// The key row's fades as last drawn, `(leading, trailing)` ([`key_bar_fades`]).
    key_bar_fades: (bool, bool),
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
            WorkspaceEvent::NeedsYou(n) => slopty_platform::set_badge(*n),
            WorkspaceEvent::Attention(_session) => {
                slopty_platform::attention();
                slopty_platform::bounce();
            }
            // A bell while the human is elsewhere is an alert; in front of the window the
            // view's own flash is enough.
            WorkspaceEvent::Bell(_session) => {
                if settings::bell_alerts(&ws.settings, cx.active_window().is_some()) {
                    slopty_platform::attention();
                    slopty_platform::bounce();
                }
            }
        });
        // The key bar follows the focused tile: a workspace change re-renders the shell,
        // which is a key bar and the overlays.
        let changes = cx.observe(&view, |_ws, _view, cx| cx.notify());
        Self {
            workers: Vec::new(),
            directory: slopty_client::directory::Directory::default(),
            server: None,
            server_generation: 0,
            directory_cache,
            view,
            hardware_keyboard: hardware_keyboard_attached(),
            theme: Theme::default(),
            settings: Settings::default(),
            window_dark: true,
            subscriptions: vec![events, changes],
            adding: None,
            runtime,
            window: None,
            settings_path,
            settings_seen,
            settings_editor: None,
            pending_focus_editor: false,
            split_view: None,
            key_bar_scroll: ScrollHandle::new(),
            key_bar_fades: (false, false),
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
        self.subscriptions.push(cx.subscribe_in(
            &editor,
            window,
            |this, editor, event, window, cx| match event {
                SettingsEditorEvent::Save(text) => this.save_settings(text, editor, window, cx),
                SettingsEditorEvent::OpenExternally => {
                    open_settings_file(cx);
                    this.close_settings(window, cx);
                }
                SettingsEditorEvent::Dismiss => this.close_settings(window, cx),
            },
        ));
        self.settings_editor = Some(editor);
        cx.notify();
    }

    /// The editor's Save: a text that parses is written and applied at once, and the watcher
    /// takes it as seen; one that does not stays in the editor with the reason under it.
    fn save_settings(
        &mut self,
        text: &str,
        editor: &Entity<SettingsEditor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match settings::save(&self.settings_path, text, &mut self.settings_seen) {
            Ok(loaded) => {
                tracing::info!(path = %self.settings_path.display(), "settings saved");
                self.apply_loaded(loaded, cx);
                self.close_settings(window, cx);
            }
            Err(error) => editor.update(cx, |e, cx| e.set_error(error, cx)),
        }
    }

    /// Drop the editor and hand the keyboard back to the focused tile.
    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_editor.take().is_none() {
            return;
        }
        self.view.update(cx, |view, cx| view.return_keyboard(window, cx));
        cx.notify();
    }

    /// A keyboard was attached or removed: show or hide the key bar and the palette's chords.
    fn set_hardware_keyboard(&mut self, attached: bool, cx: &mut Context<Self>) {
        if self.hardware_keyboard != attached {
            tracing::info!(attached, "hardware keyboard");
            self.hardware_keyboard = attached;
            self.view.update(cx, |v, _| v.set_hardware_keyboard(attached));
            cx.notify();
        }
    }

    /// Take a (re)loaded settings file: log what was odd about it, show it as a toast for a
    /// few seconds, and rebuild the theme.
    fn apply_loaded(&mut self, loaded: Loaded, cx: &mut Context<Self>) {
        for warning in &loaded.warnings {
            tracing::warn!(%warning, "settings");
        }
        if let Some(error) = &loaded.error {
            tracing::error!(%error, "settings ignored");
            self.show_notice(format!("Settings: {error}"), cx);
        } else if let Some(first) = loaded.warnings.first() {
            let more = loaded.warnings.len().saturating_sub(1);
            let text = if more == 0 {
                format!("Settings: {first}")
            } else {
                format!("Settings: {first} (+{more} more)")
            };
            self.show_notice(text, cx);
        }
        // A file that did not parse keeps the server in use rather than dropping it.
        let server = loaded.error.is_none().then(|| loaded.settings.client.server.clone());
        self.settings = loaded.settings;
        self.rebuild_theme(cx);
        if let Some(server) = server {
            self.set_server(server, None, cx);
            self.refresh_menu(cx);
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

    fn slot(&self, id: WorkerId) -> Option<&WorkerSlot> {
        self.workers.iter().find(|w| w.id == id)
    }

    fn slot_mut(&mut self, id: WorkerId) -> Option<&mut WorkerSlot> {
        self.workers.iter_mut().find(|w| w.id == id)
    }

    /// A banner for `session` was activated: whichever worker runs it, its tile is revealed.
    fn notification_response(
        &self,
        session: SessionId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.view.update(cx, |v, cx| v.notification_response(session, cx));
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
            self.drop_slot(id, cx);
        }
        if self.workers.is_empty() && self.server.is_none() {
            self.show_add_worker(Panel::Server, window, cx);
        }
        self.refresh_menu(cx);
        cx.notify();
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

    /// What the status bar's hosts popover can do to each worker: dial it now, and forget
    /// one added by address; and its way to add a worker.
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
                (slot.key, HostActions { connect: Some(connect), forget })
            })
            .collect();
        let add: MenuRun = Rc::new(move |window, cx| {
            let _gone = this.update(cx, |ws, cx| ws.show_add_worker(Panel::Worker, window, cx));
        });
        self.view.update(cx, |v, cx| v.set_host_actions(hosts, Some(add), cx));
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
    /// (or for [`server::HOLD_RETRY`], in case the server is the one that cannot see it).
    fn spawn_worker_loop(&self, id: WorkerId, cx: &Context<Self>) {
        let handle = self.runtime.clone();
        let view = self.view.clone();
        cx.spawn(async move |this, cx| {
            let mut redial = slopty_net::redial::Redial::default();
            let mut held = false;
            loop {
                let Ok(Some(plan)) = this.update(cx, |ws, _cx| ws.plan(id)) else {
                    break;
                };
                let server::Plan { key, wake, address, hold } = plan;
                if let Some(status) = hold
                    && !held
                {
                    view.update(cx, |v, cx| v.set_worker_status(key, status, cx));
                    // Woken: back online, or the server went away. Timed out: try it anyway.
                    held = !wait_or_wake(cx, &wake, server::HOLD_RETRY).await;
                    continue;
                }
                held = false;
                let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
                handle.spawn(async move {
                    let _sent = ready_tx.send(net::connect_to(id, address).await);
                });
                let outcome = ready_rx.await;
                let Ok(Ok(connected)) = outcome else {
                    let failed = match outcome {
                        Ok(Err(failed)) => failed,
                        Ok(Ok(_)) | Err(_) => {
                            net::DialFailed::Other("connection task died".to_owned())
                        }
                    };
                    let Ok(status) = this.update(cx, |ws, _cx| ws.failure_status(id, failed))
                    else {
                        break;
                    };
                    let delay = redial.next(std::time::Instant::now());
                    view.update(cx, |v, cx| v.set_worker_status(key, status, cx));
                    wait_or_wake(cx, &wake, delay).await;
                    continue;
                };
                redial.linked(std::time::Instant::now());
                let net::Connected { me, ack, sender, mut events, link } = connected;
                let link = std::sync::Arc::new(link);
                let screen_link = std::sync::Arc::clone(&link);
                let open_screen: slopty_ui::screen::ScreenFactory =
                    std::sync::Arc::new(move |stream, codec| screen_link.screen(stream, codec));
                let weak_link = std::sync::Arc::downgrade(&link);
                let remote = Some(link.remote());
                let worker_link = WorkerLink { me, out: sender, open_screen, remote };
                let name = ack.name.clone();
                let sessions = ack.sessions.len();
                // The slot is checked and the tiles connected in one step: a worker forgotten
                // while the dial was in flight must not come back as a tile holding this link.
                let alive = this.update(cx, |ws, cx| {
                    let Some(key) = workers::adopt(&mut ws.workers, id, weak_link, name.clone())
                    else {
                        return false;
                    };
                    ws.view.update(cx, |v, cx| v.connect_worker(key, worker_link, ack, cx));
                    ws.refresh_menu(cx);
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
                let rtt_link = std::sync::Arc::downgrade(&link);
                let rtt_view = view.clone();
                let rtt_wake = std::sync::Arc::clone(&wake);
                cx.spawn(async move |cx| {
                    let mut hearing = Hearing::new(std::time::Instant::now());
                    // The handshake has measured the path already: the predictors draw from the
                    // first key, not from the first tick 500 ms on.
                    if let Some(link) = rtt_link.upgrade() {
                        let rtt = link.rtt();
                        rtt_view.update(cx, |v, cx| v.set_rtt(key, rtt, cx));
                    }
                    loop {
                        let woken = wait_or_wake(cx, &rtt_wake, workers::HEARING_TICK).await;
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
                while let Some(first) = events.recv().await {
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
                drop(link);
                wait_or_wake(cx, &wake, redial.next(std::time::Instant::now())).await;
            }
        })
        .detach();
    }

    /// Show the panel, connecting to a server or adding a worker; an open panel switches to
    /// `mode` and keeps what was typed.
    fn show_add_worker(&mut self, mode: Panel, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(adding) = &mut self.adding {
            if adding.mode != mode {
                adding.mode = mode;
                adding.error = None;
                adding.address.update(cx, |input, cx| {
                    input.set_placeholder(mode.example(), window, cx);
                });
                cx.notify();
            }
            return;
        }
        let address = cx.new(|cx| InputState::new(window, cx).placeholder(mode.example()));
        self.subscriptions.push(cx.subscribe(&address, |this, _input, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.add_from_panel(cx);
            }
        }));
        address.update(cx, |input, cx| input.focus(window, cx));
        // A server's panel looks while no server is set; a worker's always, for the workers
        // not yet added.
        let search = (!cfg!(target_os = "ios") && (mode == Panel::Worker || self.server.is_none()))
            .then_some(Search::Looking);
        let looking = search.is_some();
        self.adding = Some(Adding { mode, address, busy: false, error: None, search });
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
                Ok(Err(e)) => ws.panel_failed(format!("{e:#}"), cx),
                Err(_dropped) => ws.panel_failed("connection task died".to_owned(), cx),
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
    /// answered as a list to press, the address with its one primary action, and the other way
    /// in as a quiet link.
    ///
    /// On the first run it is the page, a third of the way down the content surface over a foot
    /// that says why there is nothing to pair. Later ("Add a worker…", "Connect to a server…")
    /// it is a dialog over the workspace, closed by Cancel, Esc or a click outside it. On the
    /// Mac the field and its action share a row; on touch the field ends in a Paste, since the
    /// phone has no ⌘V, and the action is a thumb's full width under it.
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
        let (title, blurb, field, go, other, other_mode) = match adding.mode {
            Panel::Server => (
                "Connect to a server",
                "A server on your tailnet or VPN lists your workers.",
                "Server address",
                "Connect",
                "Add a worker by address instead",
                Panel::Worker,
            ),
            Panel::Worker => (
                "Add a worker",
                "A Mac running the Slopty worker, on your tailnet or VPN.",
                "Worker address",
                "Add",
                "Connect to a server instead",
                Panel::Server,
            ),
        };
        let welcome = self.welcome();
        // The page names the app over its heading, as a sign on the door does; a dialog over
        // the workspace needs no sign.
        let wordmark = welcome.then(|| wordmark(theme));
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
        let status = match (&adding.error, adding.busy) {
            (Some(e), _) => Some((e.clone(), s.error)),
            (None, true) => Some(("Connecting…".to_owned(), s.text_muted)),
            (None, false) => None,
        };
        let go = button("add", go, ButtonKind::Primary)
            .h(px(FIELD_H))
            .when(TOUCH, gpui::Styled::w_full)
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
        let entry = if TOUCH {
            div().flex().flex_col().gap(px(spacing.sm)).child(address.w_full()).child(go)
        } else {
            div().flex().items_center().gap(px(spacing.sm)).child(address).child(go)
        };
        let entry = div()
            .flex()
            .flex_col()
            .gap(px(spacing.sm))
            // Under the tailnet's list the field is the other way; alone it needs no label.
            .when(tailnet.is_some(), |el| el.child(kit::label(theme, "Or type an address")))
            .child(entry)
            .when_some(status, |el, (text, tone)| {
                el.child(
                    div()
                        .id("add-worker-status")
                        .role(Role::Status)
                        .aria_label(SharedString::from(text.clone()))
                        .text_size(px(ty.small()))
                        .text_color(hsla(tone))
                        .child(SharedString::from(text)),
                )
            });
        let switch = button("panel-switch", other, ButtonKind::Link)
            .text_size(px(ty.small()))
            .on_click(cx.listener(move |this, _ev, window, cx| {
                this.show_add_worker(other_mode, window, cx);
            }));
        let cancel = (!welcome).then(|| {
            button("cancel-add", "Cancel", ButtonKind::Ghost)
                .on_click(cx.listener(|this, _ev, window, cx| this.cancel_add_worker(window, cx)))
        });
        let aside =
            div().flex().items_center().child(switch).child(div().flex_1()).children(cancel);
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
                    .children(wordmark)
                    .child(intro),
            )
            .children(tailnet)
            .child(entry)
            .child(aside);
        if !welcome {
            return kit::backdrop(theme, window)
                .id("add-worker-backdrop")
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev, window, cx| {
                        this.cancel_add_worker(window, cx);
                        cx.stop_propagation();
                    }),
                )
                .child(panel)
                .into_any_element();
        }
        // Not a dialog over an app that does nothing yet: the page itself, on the content
        // surface the tiles' bodies take. Its block sits a third of the way down the room over
        // the foot, where the eye starts; spacers rather than a percentage pad, which would
        // resolve against the width. The safe area includes a phone's keyboard, so the block
        // rises with it.
        let safe = window.insets().effective();
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
            .pb(safe.bottom)
            // A phone on its side has the island at one end: the page keeps clear of it.
            .pl(safe.left + px(spacing.lg))
            .pr(safe.right + px(spacing.lg))
            .bg(hsla(theme.content()))
            .font_family(ty.ui_family.clone())
            .child(div().flex_grow(1.0))
            .child(panel)
            .child(div().flex_grow(2.0))
            .child(foot)
            .into_any_element()
    }

    /// What the tailnet answered: each server and worker a row to press, under a label, in
    /// one framed list; while it looks, or when nothing answered, the same frame holds one
    /// row saying so and what to do, so the page does not jump as the look ends.
    fn tailnet_list(&self, search: &Search, mode: Panel, cx: &Context<Self>) -> gpui::AnyElement {
        use slopty_ui::icons::{IconName, IconSize, Status, icon, status_icon};
        let theme = &self.theme;
        let s = theme.surfaces;
        // The rows' radius plus the pad round them, so the corners nest.
        let frame = div()
            .id("add-worker-tailnet")
            .flex()
            .flex_col()
            .p(px(theme.spacing.xxs))
            .rounded(px(theme.radii.md))
            .border_1()
            .border_color(hsla(s.border));
        let glyph = px(theme.typography.icon());
        if let Some(words) = search.words(mode) {
            let mark = match search {
                Search::Looking => status_icon(theme, Status::Running, glyph, hsla(s.text_muted)),
                Search::Answered { running, .. } => {
                    let name = if *running { IconName::Search } else { IconName::WifiOff };
                    icon(theme, name, IconSize::Inline, hsla(s.text_muted))
                        .size(glyph)
                        .into_any_element()
                }
            };
            let row = kit::row(theme, kit::Row::Two)
                .id("add-worker-search")
                .debug_selector(|| "add-worker-search".to_owned())
                .role(Role::Status)
                .aria_label(words)
                .child(mark)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(px(theme.typography.ui_size))
                                .text_color(hsla(s.text_secondary))
                                .child(words),
                        )
                        .children(
                            search.next_step(mode).map(|step| kit::meta(div(), theme).child(step)),
                        ),
                );
            return frame.child(row).into_any_element();
        }
        let rows = search.offers(mode).into_iter().enumerate().map(|(ix, (host, offer))| {
            let target = offer.at.clone();
            found_row(theme, ix, host, offer).on_click(cx.listener(move |this, _ev, window, cx| {
                this.connect_found(host, &target, window, cx);
            }))
        });
        div()
            .flex()
            .flex_col()
            .gap(px(theme.spacing.sm))
            .child(kit::label(theme, "On your tailnet"))
            .child(frame.children(rows))
            .into_any_element()
    }

    /// Esc, Tab, sticky Control, arrows and the shell symbols a phone keyboard hides; shown
    /// above the keyboard inset while a terminal or a remote window is active.
    ///
    /// The bar is the body's own surface under a hairline, so it reads as the tile's input row
    /// and its caps as plates on it; on the canvas a cap needed a hairline of its own to be
    /// seen. Its row scrolls where it overflows, and an end with keys past it fades out.
    fn key_bar(
        &mut self,
        target: &KeyTarget,
        window: &mut Window,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let safe = window.insets().effective();
        let width = f32::from(self.frame_size(window).width - safe.left - safe.right);
        let row = match target {
            KeyTarget::Terminal(terminal) => self.terminal_key_bar(terminal, width, cx),
            KeyTarget::Screen(screen) => self.screen_key_bar(screen, width, cx),
        };
        // The offset is the one this frame scrolled to; the extent is the last layout's, so a
        // bar just shown, turned or resized is looked at again once it is laid out.
        let (leading, trailing) = self.measured_fades();
        self.key_bar_fades = (leading, trailing);
        cx.on_next_frame(window, |this, _window, cx| {
            if this.measured_fades() != this.key_bar_fades {
                cx.notify();
            }
        });
        let solid = hsla(self.theme.content());
        let fade = |toward: f32| {
            div().absolute().top_0().bottom_0().w(px(self.theme.spacing.lg)).bg(
                gpui::linear_gradient(
                    toward,
                    gpui::linear_color_stop(gpui::Hsla { a: 0.0, ..solid }, 0.0),
                    gpui::linear_color_stop(solid, 1.0),
                ),
            )
        };
        div()
            .relative()
            .w_full()
            .bg(solid)
            .border_t_1()
            .border_color(hsla(self.theme.surfaces.border))
            .child(row)
            .when(leading, |el| {
                el.child(fade(270.0).left_0().debug_selector(|| "key-bar-fade-leading".to_owned()))
            })
            .when(trailing, |el| {
                el.child(fade(90.0).right_0().debug_selector(|| "key-bar-fade-trailing".to_owned()))
            })
            .into_any_element()
    }

    /// The size the app lays itself out in: the window's, or the self-test's stand-in for
    /// Split View.
    fn frame_size(&self, window: &Window) -> gpui::Size<gpui::Pixels> {
        self.split_view.unwrap_or_else(|| window.viewport_size())
    }

    /// [`key_bar_fades`] for the key row as it is scrolled now.
    fn measured_fades(&self) -> (bool, bool) {
        let scrolled = -f32::from(self.key_bar_scroll.offset().x);
        key_bar_fades(scrolled, f32::from(self.key_bar_scroll.max_offset().x))
    }

    /// The key bar's row with `keys` in it. Where they all fit in `width` (an iPad), their
    /// groups spread along it ([`key_group`]); where they do not (a phone), one line in the
    /// order given that scrolls sideways.
    fn key_row_of(
        &self,
        keys: Vec<(&'static str, gpui::AnyElement)>,
        width: f32,
    ) -> gpui::Stateful<gpui::Div> {
        let spacing = self.theme.spacing;
        let row = self.key_row();
        if key_row_width(keys.iter().map(|(label, _)| *label), spacing) > width {
            return row.children(keys.into_iter().map(|(_, key)| key));
        }
        let group = || div().flex().items_center().gap(px(spacing.xs));
        let (mut lead, mut arrows, mut symbols, mut words) =
            (group(), group(), Vec::new(), Vec::new());
        for (label, key) in keys {
            match key_group(label) {
                KeyGroup::Lead => lead = lead.child(key),
                KeyGroup::Arrows => arrows = arrows.child(key),
                KeyGroup::Trail if label.chars().count() > 1 => words.push(key),
                KeyGroup::Trail => symbols.push(key),
            }
        }
        row.debug_selector(|| "key-bar-spread".to_owned())
            .child(lead.flex_1().debug_selector(|| "key-group-lead".to_owned()))
            .child(arrows.flex_none().debug_selector(|| "key-group-arrows".to_owned()))
            .child(
                group()
                    .flex_1()
                    .justify_end()
                    .debug_selector(|| "key-group-trail".to_owned())
                    .children(symbols)
                    .children(words),
            )
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
            .text_color(hsla(if lit { s.accent_ink } else { s.text }))
            .bg(hsla(if lit { s.accent_fill } else { s.raised }))
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

    /// The bar over a remote window: chords and arrows, copy and paste through the worker.
    fn screen_key_bar(
        &self,
        screen: &Entity<ScreenView>,
        width: f32,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
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
        keys.push(("Paste", paste.into_any_element()));
        self.key_row_of(keys, width).into_any_element()
    }

    fn terminal_key_bar(
        &self,
        terminal: &Entity<TerminalView>,
        width: f32,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
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
        self.key_row_of(keys, width).into_any_element()
    }
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

/// A key-bar key as a screen reader names it; an armed modifier says so.
fn key_label(label: &str, lit: bool) -> String {
    let name = key_spoken(label);
    if lit { format!("{name}, armed") } else { name.to_owned() }
}

/// A key-bar key's name for a screen reader: what the cap shows, spelled out.
fn key_spoken(label: &str) -> &str {
    match label {
        "Esc" => "Escape",
        "⌃" => "Control",
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
        // The frame's draw starts here; the probe element at the end of the tree closes it.
        slopty_ui::frames::begin(cx);
        // Notch / Dynamic Island, home indicator and the soft keyboard on iOS; zero on macOS.
        // The workspace keeps the top and the sides clear itself.
        let insets = window.insets().effective();
        let key_bar = key_bar_visible(TOUCH, self.hardware_keyboard)
            .then(|| self.view.read(cx).active_key_target())
            .flatten()
            .map(|target| self.key_bar(&target, window, cx));
        let surfaces = self.theme.surfaces;
        let band = if key_bar.is_some() { self.theme.content() } else { surfaces.canvas };
        if std::mem::take(&mut self.pending_focus_editor)
            && let Some(editor) = self.settings_editor.clone()
        {
            editor.update(cx, |e, cx| e.focus(window, cx));
        }
        let settings_editor = self.settings_editor.clone();
        let welcome = self.welcome();
        // A page in a browser tile is a native view over everything GPUI draws: it hides
        // under the app's own dialogs as it does under the workspace's.
        let covered = welcome || settings_editor.is_some() || self.adding.is_some();
        self.view.update(cx, |v, cx| {
            v.set_covered(covered, cx);
            // The key bar takes the status bar's row above the keyboard.
            v.set_key_bar_shown(key_bar.is_some(), cx);
        });
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
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
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
            .child(slopty_ui::frames::probe())
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
        LinkEvent::Control(WorkerMsg::SessionOpened(summary)) => {
            view.update(cx, |v, cx| v.session_opened(key, summary, cx));
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
        LinkEvent::Control(WorkerMsg::FoundFiles { root, query, paths }) => {
            view.update(cx, |v, cx| v.files_found(&root, &query, &paths, cx));
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
            view.update(cx, |v, _cx| v.clip_message(key, msg));
        }
        LinkEvent::Control(WorkerMsg::Xfer(msg)) => {
            view.update(cx, |v, cx| v.xfer_message(msg, cx));
        }
        LinkEvent::Control(WorkerMsg::Path(path)) => {
            view.update(cx, |v, cx| v.set_link_path(key, path, cx));
        }
        LinkEvent::Control(WorkerMsg::Caps(caps)) => {
            view.update(cx, |v, cx| v.set_worker_caps(key, caps, cx));
        }
        LinkEvent::Ports { session, forwards } => {
            view.update(cx, |v, cx| v.ports_changed(session, forwards, cx));
        }
        LinkEvent::XferFailed { xfer, error } => {
            view.update(cx, |v, cx| v.xfer_failed(xfer, &error, cx));
        }
        LinkEvent::Conversation { session, event } => {
            view.update(cx, |v, cx| v.conversation_event(session, event, cx));
        }
        LinkEvent::Control(WorkerMsg::Permission(event)) => {
            view.update(cx, |v, cx| v.permission_event(event, cx));
        }
        LinkEvent::Control(_) => {}
        LinkEvent::Disconnected(why) => {
            let status = WorkerStatus::Reconnecting(format!("disconnected: {why}"));
            view.update(cx, |v, cx| v.disconnect_worker(key, status, cx));
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

/// The app's name where it names itself: the word alone, at the strong weight in the secondary
/// tone. A mark on an accent tile read as a web page's logo over a form.
fn wordmark(theme: &Theme) -> impl IntoElement {
    div()
        .id("app-brand")
        .debug_selector(|| "app-brand".to_owned())
        .role(Role::Label)
        .aria_label(APP_NAME)
        .text_size(px(theme.typography.ui_size))
        .font_weight(gpui::FontWeight(Typography::STRONG_WEIGHT))
        .text_color(hsla(theme.surfaces.text_secondary))
        .child(APP_NAME)
}

/// What the app is called where it names itself.
const APP_NAME: &str = "Slopty";

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
        .hover(move |el| el.bg(hsla(s.raised)))
        .active(move |el| el.bg(hsla(s.overlay)))
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

/// The app's own bindings, outside any view's context.
fn app_key_bindings() -> Vec<gpui::KeyBinding> {
    vec![
        gpui::KeyBinding::new("cmd-,", OpenSettings, None),
        gpui::KeyBinding::new("cmd-shift-h", AddWorker, None),
    ]
}

/// The app's lines for the command palette, after the workspace's.
fn app_palette_items() -> Vec<slopty_ui::palette::PaletteItem> {
    use slopty_ui::icons::IconName;

    let bindings = app_key_bindings();
    let item = |label: &str, icon: IconName, action: Box<dyn gpui::Action>| {
        slopty_ui::palette::PaletteItem::new(label, icon, action, &bindings)
    };
    vec![
        item("Open settings", IconName::Settings, Box::new(OpenSettings)),
        item("Connect to a server", IconName::Link, Box::new(ConnectServer)),
        item("Disconnect from the server", IconName::Unplug, Box::new(DisconnectServer)),
        item("Add a worker", IconName::Plus, Box::new(AddWorker)),
    ]
}

/// Where this device keeps its layout: beside the settings, in the client's data directory.
fn layout_path() -> std::path::PathBuf {
    slopty_settings::data_dir().join("layout.json")
}

/// Whether this launch is `cargo xtask e2e`'s, driven over its socket.
#[must_use]
pub fn self_test() -> bool {
    std::env::var_os(slopty_e2e::SOCKET_ENV).is_some()
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
    options: WindowOptions,
) -> anyhow::Result<()> {
    // VideoToolbox's first decoder session costs 150–400 ms; pay it before any worker is
    // dialed.
    slopty_client::warm_up_decoder();
    if let Err(e) = slopty_ui::fonts::install(cx) {
        tracing::error!(error = %e, "bundled fonts");
    }
    cx.bind_keys(slopty_ui::workspace::key_bindings());
    cx.bind_keys(slopty_ui::terminal::key_bindings());
    cx.bind_keys(app_key_bindings());
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
        view.set_pasteboard(pasteboard());
        view.set_hardware_keyboard(hardware_keyboard_attached());
        #[cfg(feature = "e2e")]
        view.set_animation(false);
        view
    });
    let (directory_cache, cache_writes) = tokio::sync::watch::channel(server::Cache::Remove);
    handle.spawn(server::write_cache(server::cache_path(), cache_writes));
    let workspace = cx
        .new(|cx| Workspace::new(view, handle, settings_path, settings_seen, directory_cache, cx));
    let root_view = workspace.clone();
    // Terminals and remote desktops stay at full rate while another app has the keyboard: a
    // second display is watched while typing elsewhere.
    // The self-test's window comes up in front but takes no keyboard: its keys arrive over the
    // socket, and the machine's keyboard belongs to whoever is using it.
    let options = WindowOptions {
        inactive_frame_interval: None,
        focus: options.focus && !self_test(),
        ..options
    };
    let window = cx.open_window(options, move |window, cx| {
        // The theme follows the window's appearance while `theme.appearance = "system"`.
        let observed = root_view.clone();
        let subscription = window.observe_window_appearance(move |window, cx| {
            let dark = settings::is_dark(window.appearance());
            observed.update(cx, |ws, cx| ws.set_window_dark(dark, cx));
        });
        let dark = settings::is_dark(window.appearance());
        root_view.update(cx, |ws, cx| {
            ws.subscriptions.push(subscription);
            // A worker's clipboard is watched only while this app is frontmost.
            let activation = cx.observe_window_activation(window, |ws, window, cx| {
                let active = window.is_window_active();
                ws.view.update(cx, |v, cx| v.set_app_active(active, cx));
            });
            ws.subscriptions.push(activation);
            ws.window_dark = dark;
            ws.apply_loaded(loaded, cx);
        });
        cx.new(|cx| Root::new(root_view, window, cx))
    })?;
    watch_settings(workspace.clone(), cx);
    // A tap on an agent banner brings the app and that session forward, on whichever worker
    // the session lives.
    let for_notifications = workspace.clone();
    cx.on_system_notification_response(move |response, cx| {
        let Ok(session) = response.tag.parse::<SessionId>() else { return };
        cx.activate(true);
        let _handled = window.update(cx, |_root, window, cx| {
            for_notifications.update(cx, |ws, cx| {
                ws.notification_response(session, window, cx);
            });
        });
    });
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
            ws.window = Some(window.window_handle());
            ws.view.update(cx, |_v, cx| WorkspaceView::accept_dropped_files(window, cx));
            for worker in known {
                ws.add_worker(worker.worker_id, worker.name, true, cx);
            }
            ws.refresh_menu(cx);
            if ws.workers.is_empty() && ws.server.is_none() {
                ws.show_add_worker(Panel::Server, window, cx);
            } else {
                let handle = ws.view.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
        });
    })?;
    // The self-test socket, for `cargo xtask e2e app`; never set for a normal launch.
    if let Some(socket) = std::env::var_os(slopty_e2e::SOCKET_ENV) {
        // The accessibility tree is built only while a screen reader asks for it; a test
        // asks up front so `dump.a11y` has it.
        #[cfg(feature = "e2e")]
        window.update(cx, |_root, window, _cx| window.set_a11y_active(true))?;
        let runtime = workspace.read(cx).runtime.clone();
        e2e::serve(socket.into(), workspace, window.into(), &runtime, cx);
    }
    Ok(())
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
            std::env::var_os(slopty_e2e::SOCKET_ENV)
                .map(|_| format!("com.aislopware.slopty.self-test.{}", std::process::id()))
        });
    if let Some(board) = named.as_deref().and_then(IosPasteboard::named) {
        tracing::info!(name = ?named, "clipboard on a named pasteboard");
        return Rc::new(board);
    }
    Rc::new(IosPasteboard::general())
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
        assert_eq!(key_label("⌃", true), "Control, armed");
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

    /// Where the row fits (an iPad) its keys spread in three groups: what the soft keyboard
    /// lacks at the left, the arrows in the middle, the symbols and the word keys at the right.
    #[test]
    fn a_wide_key_bar_spreads_its_groups() {
        let spacing = Theme::default().spacing;
        let group = |labels: &[&str]| labels.iter().map(|l| key_group(l)).collect::<Vec<_>>();
        assert!(group(&["Esc", "Tab", "⌃", "⌘"]).iter().all(|g| *g == KeyGroup::Lead));
        assert!(group(&["←", "↑", "↓", "→"]).iter().all(|g| *g == KeyGroup::Arrows));
        let trail = group(&["~", "|", "/", "-", "Copy", "Paste", "Find"]);
        assert!(trail.iter().all(|g| *g == KeyGroup::Trail));
        let terminal = BAR_KEYS.iter().map(|(label, ..)| *label).chain(["Paste", "Find"]);
        let screen = SCREEN_BAR_KEYS.iter().map(|(label, ..)| *label).chain(["Copy", "Paste"]);
        for row in [key_row_width(terminal, spacing), key_row_width(screen, spacing)] {
            assert!(row <= 744.0, "every bar spreads on the narrowest iPad: {row}");
        }
        assert!(key_row_width(["Esc"], spacing) > 0.0, "one key is a row");
    }

    /// The shell in a headless window with the add-worker panel up, `worker` ones known so
    /// it is a dialog over the workspace, else the first run. The runtime and the directory
    /// hold what the shell's tasks and settings file need for the test's length.
    fn shell<'a>(
        cx: &'a mut TestAppContext,
        runtime: &tokio::runtime::Runtime,
        dir: &tempfile::TempDir,
        worker: bool,
    ) -> (Entity<Workspace>, &'a mut VisualTestContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.bind_keys(app_key_bindings());
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
        let ws = cx.new(|cx| {
            let mut ws = Workspace::new(view, handle, path, seen, cache, cx);
            if worker {
                ws.workers.push(WorkerSlot::new(WorkerId::new(), "studio".to_owned(), true));
            }
            ws
        });
        let root = ws.clone();
        let (_root, cx) = cx.add_window_view(move |window, cx| Root::new(root, window, cx));
        cx.update(|window, cx| {
            ws.update(cx, |ws, cx| ws.show_add_worker(Panel::Worker, window, cx));
        });
        cx.run_until_parked();
        (ws, cx)
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
    /// run's. On the Mac the field and its action share a row, the field taking the room.
    #[gpui::test]
    fn the_field_and_its_action_share_a_row(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (_ws, cx) = shell(cx, &runtime, &dir, true);
        cx.simulate_resize(size(px(900.0), px(600.0)));
        cx.run_until_parked();
        assert!(cx.debug_bounds("add-worker-foot").is_none(), "a dialog has no foot");
        assert!(cx.debug_bounds("app-brand").is_none(), "nor the app's name");
        let field = cx.debug_bounds("add-worker-field").expect("the field");
        let go = cx.debug_bounds("add").expect("its action");
        assert!((f32::from(field.top() - go.top())).abs() < 0.5, "{field:?} {go:?}");
        assert!((f32::from(field.size.height - go.size.height)).abs() < 0.5, "one height");
        assert!(field.right() < go.left(), "the action after the field");
        assert!(field.size.width > go.size.width * 3.0, "the field takes the room");
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
}
