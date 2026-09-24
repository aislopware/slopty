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
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    WeakEntity, Window, WindowOptions, div, px,
};
use gpui_kit::component::Root;
use gpui_kit::component::input::{Input, InputEvent, InputState};
pub use settings::actions::OpenSettings;
use slopty_client::LinkEvent;
use slopty_client::layout::WorkerKey;
use slopty_core::{SessionId, WorkerId};
use slopty_proto::HostMsg;
use slopty_settings::{Loaded, Settings};
use slopty_theme::Theme;
use slopty_ui::a11y::{key_name, tab_stop};
use slopty_ui::colors::hsla;
use slopty_ui::kit;
use slopty_ui::screen::{ScreenView, Sticky};
use slopty_ui::settings_editor::{SettingsEditor, SettingsEditorEvent};
use slopty_ui::terminal::TerminalView;
use slopty_ui::workspace::{
    KeyTarget, MenuEntry, WorkerLink, WorkerStatus, WorkspaceEvent, WorkspaceView,
};
pub use workers::actions::{AddWorker, ConnectServer, DisconnectServer};
use workers::{WorkerSlot, retry_delay};

/// A row of keys the soft keyboard lacks (Esc, Tab, Control, arrows, shell symbols).
const KEY_BAR: bool = cfg!(target_os = "ios");

/// The key bar is for a touch platform typing on glass: a hardware keyboard has every key on
/// it, so the row hides while one is attached and comes back when it is unplugged.
const fn key_bar_visible(touch_platform: bool, hardware_keyboard: bool) -> bool {
    touch_platform && !hardware_keyboard
}
/// Key bar height in points.
const KEY_BAR_H: f32 = 40.0;

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
/// non-printing keys).
const BAR_KEYS: [(&str, &str, Option<&str>); 12] = [
    ("esc", "escape", None),
    ("tab", "tab", None),
    ("⌃", "", None),
    ("⌘", "cmd", None),
    ("←", "left", None),
    ("↑", "up", None),
    ("↓", "down", None),
    ("→", "right", None),
    ("-", "-", Some("-")),
    ("/", "/", Some("/")),
    ("|", "|", Some("|")),
    ("~", "~", Some("~")),
];
/// The key bar over a remote window: ⌘ joins ⌃ (an IDE lives on chords), the shell
/// punctuation goes.
const SCREEN_BAR_KEYS: [(&str, &str, Option<&str>); 9] = [
    ("esc", "escape", None),
    ("tab", "tab", None),
    ("⌃", "", None),
    ("⌘", "cmd", None),
    ("←", "left", None),
    ("↑", "up", None),
    ("↓", "down", None),
    ("→", "right", None),
    ("/", "/", Some("/")),
];
/// Nothing heard from the worker for this long is shown as a warning (keep-alives run every
/// 5 s).
const SILENCE_WARN: std::time::Duration = std::time::Duration::from_secs(8);
/// Nothing heard for this long and the connection is given up so the reconnect loop takes
/// over: a restarted worker is back in ~1 s instead of after the transport's 45 s idle
/// timeout. Three missed keep-alives, the same bar noq uses to abandon a path.
const SILENCE_DROP: std::time::Duration = std::time::Duration::from_secs(15);

/// What the panel over the workspace connects to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Panel {
    /// A server, whose directory lists the workers: the usual way in.
    Server,
    /// One worker by address, for a setup without a server.
    Worker,
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
}

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
    /// The in-app settings editor while it is open.
    settings_editor: Option<Entity<SettingsEditor>>,
    /// Focus the editor's field on the next frame (it needs a frame to exist).
    pending_focus_editor: bool,
}

impl Workspace {
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

    /// The editor's Save: a text that parses is written and applied at once (the watcher
    /// would see it within a second anyway); one that does not stays in the editor with
    /// the reason under it.
    fn save_settings(
        &mut self,
        text: &str,
        editor: &Entity<SettingsEditor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match settings::save(&self.settings_path, text) {
            Ok(loaded) => {
                tracing::info!(path = %self.settings_path.display(), "settings saved");
                self.apply_loaded(loaded, cx);
                self.close_settings(window, cx);
            }
            Err(error) => editor.update(cx, |e, cx| e.set_error(error, cx)),
        }
    }

    /// Drop the editor and hand the keyboard back to the workspace.
    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_editor.take().is_none() {
            return;
        }
        let handle = self.view.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    /// A keyboard was attached or removed: show or hide the key bar.
    fn set_hardware_keyboard(&mut self, attached: bool, cx: &mut Context<Self>) {
        if self.hardware_keyboard != attached {
            tracing::info!(attached, "hardware keyboard");
            self.hardware_keyboard = attached;
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
            self.show_notice(format!("settings: {error}"), cx);
        } else if let Some(first) = loaded.warnings.first() {
            let more = loaded.warnings.len().saturating_sub(1);
            let text = if more == 0 {
                format!("settings: {first}")
            } else {
                format!("settings: {first} (+{more} more)")
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
            self.show_notice(format!("forget worker: {e:#}"), cx);
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

    /// The app's rows in the titlebar's "…" menu: settings, adding a worker, and forgetting
    /// each one.
    fn refresh_menu(&self, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let mut entries = Vec::new();
        let hint = |hint: &'static str| if cfg!(target_os = "macos") { hint } else { "" };
        {
            let this = this.clone();
            entries.push(MenuEntry {
                label: "Settings".into(),
                detail: hint("⌘,").into(),
                run: Rc::new(move |window, cx| {
                    let _gone = this.update(cx, |ws, cx| ws.open_settings(window, cx));
                }),
            });
        }
        {
            let this = this.clone();
            let entry = match self.server_address() {
                Some(address) => MenuEntry {
                    label: "Disconnect from the server".into(),
                    detail: address.host().to_owned().into(),
                    run: Rc::new(move |window, cx| {
                        let _gone = this.update(cx, |ws, cx| ws.disconnect_server(window, cx));
                    }),
                },
                None => MenuEntry {
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
        {
            let this = this.clone();
            entries.push(MenuEntry {
                label: "Add a worker".into(),
                detail: hint("⌘⇧H").into(),
                run: Rc::new(move |window, cx| {
                    let _gone =
                        this.update(cx, |ws, cx| ws.show_add_worker(Panel::Worker, window, cx));
                }),
            });
        }
        for slot in self.workers.iter().filter(|s| s.added) {
            let (this, id) = (this.clone(), slot.id);
            entries.push(MenuEntry {
                label: format!("Forget {}", slot.name).into(),
                detail: SharedString::default(),
                run: Rc::new(move |window, cx| {
                    let _gone = this.update(cx, |ws, cx| ws.forget_worker(id, window, cx));
                }),
            });
        }
        self.view.update(cx, |v, cx| v.set_more_menu(entries, cx));
    }

    /// Connect to `id` and keep it connected: each drop (worker restart, network change,
    /// silence) is retried with a capped backoff; the loop ends when the worker is dropped.
    /// While the server says the worker is away the loop waits for it to come back online
    /// (or for [`server::HOLD_RETRY`], in case the server is the one that cannot see it).
    fn spawn_worker_loop(&self, id: WorkerId, cx: &Context<Self>) {
        let handle = self.runtime.clone();
        let view = self.view.clone();
        cx.spawn(async move |this, cx| {
            let mut failures: u32 = 0;
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
                    let why = match outcome {
                        Ok(Err(why)) => why,
                        Ok(Ok(_)) | Err(_) => "connection task died".to_owned(),
                    };
                    let Ok(status) = this.update(cx, |ws, _cx| ws.failure_status(id, why)) else {
                        break;
                    };
                    failures = failures.saturating_add(1);
                    let delay = retry_delay(failures);
                    view.update(cx, |v, cx| v.set_worker_status(key, status, cx));
                    wait_or_wake(cx, &wake, delay).await;
                    continue;
                };
                failures = 0;
                let net::Connected { me, ack, sender, mut events, link } = connected;
                let link = std::sync::Arc::new(link);
                let screen_link = std::sync::Arc::clone(&link);
                let open_screen: slopty_ui::screen::ScreenFactory =
                    std::sync::Arc::new(move |stream, codec| screen_link.screen(stream, codec));
                let weak_link = std::sync::Arc::downgrade(&link);
                let worker_link = WorkerLink { me, out: sender, open_screen };
                let name = ack.name.clone();
                let sessions = ack.sessions.clone();
                view.update(cx, |v, cx| v.connect_worker(key, name.clone(), worker_link, sessions, cx));
                let alive = this.update(cx, |ws, cx| {
                    let Some(slot) = ws.slot_mut(id) else { return false };
                    slot.link = Some(weak_link);
                    slot.name = name;
                    ws.refresh_menu(cx);
                    true
                });
                if !matches!(alive, Ok(true)) {
                    break;
                }
                // Once a second: RTT for the bar and the predictors, and a liveness check.
                // QUIC keep-alives make the worker send something every few seconds; when the
                // received datagram count stops moving the worker is gone or unreachable, and
                // the bar says so long before the transport's idle timeout drops the
                // connection.
                let rtt_link = std::sync::Arc::downgrade(&link);
                let rtt_view = view.clone();
                cx.spawn(async move |cx| {
                    let mut heard = (0_u64, std::time::Instant::now());
                    loop {
                        cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                        let Some(link) = rtt_link.upgrade() else { break };
                        let rtt = link.rtt();
                        let received = link.received_datagrams();
                        if received != heard.0 {
                            heard = (received, std::time::Instant::now());
                        }
                        let gap = heard.1.elapsed();
                        tracing::debug!(worker = %id, path = %link.path(), received, "link path");
                        if gap >= SILENCE_DROP {
                            tracing::warn!(worker = %id, ?gap, path = %link.path(), "worker silent; reconnecting");
                            link.abandon("worker silent");
                            break;
                        }
                        let status = if gap >= SILENCE_WARN {
                            WorkerStatus::Silent(gap.as_secs())
                        } else {
                            WorkerStatus::Connected
                        };
                        // Both notify only when what the bar shows changed.
                        rtt_view.update(cx, |v, cx| {
                            v.set_rtt(key, rtt, cx);
                            v.set_worker_status(key, status, cx);
                        });
                    }
                })
                .detach();
                tracing::debug!(worker = %id, sessions = ack.sessions.len(), "link up; pumping events");
                // One foreground update per batch, not per event: whatever arrived while the
                // last batch was applied goes in the next one, so a flood from many sessions
                // costs one update (and GPUI draws at most once per display frame anyway).
                #[cfg(feature = "e2e")]
                let mut paced = e2e::Pacer::new(frame_nominal());
                while let Some(first) = events.recv().await {
                    #[cfg(feature = "e2e")]
                    paced.wait(cx).await;
                    let mut batch = Vec::with_capacity(LINK_BATCH);
                    batch.push(first);
                    while batch.len() < LINK_BATCH
                        && let Ok(next) = events.try_recv()
                    {
                        batch.push(next);
                    }
                    let disconnected = cx.update(|cx| {
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
                wait_or_wake(cx, &wake, retry_delay(0)).await;
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
                cx.notify();
            }
            return;
        }
        let address =
            cx.new(|cx| InputState::new(window, cx).placeholder("studio, 100.64.0.3 or host:port"));
        self.subscriptions.push(cx.subscribe(&address, |this, _input, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.add_from_panel(cx);
            }
        }));
        address.update(cx, |input, cx| input.focus(window, cx));
        self.adding = Some(Adding { mode, address, busy: false, error: None });
        cx.notify();
    }

    /// Close the panel (only offered while there is somewhere else to be).
    fn cancel_add_worker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workers.is_empty() && self.server.is_none() {
            return;
        }
        self.adding = None;
        let handle = self.view.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
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
        let text = slopty_settings::with_client_server(&text, server)?;
        let loaded = settings::save(&self.settings_path, &text)?;
        self.settings = loaded.settings;
        Ok(())
    }

    /// Stop using the server: the setting is cleared and its workers leave.
    fn disconnect_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.server.is_none() {
            return;
        }
        if let Err(e) = self.save_server(None) {
            self.show_notice(format!("settings: {e}"), cx);
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

    fn add_worker_panel(&self, adding: &Adding, cx: &Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let (spacing, radii) = (theme.spacing, theme.radii);
        let button = move |id: &'static str, text: &'static str, accent: bool| {
            let pill = div()
                .id(id)
                .role(Role::Button)
                .aria_label(text)
                .px(px(spacing.md))
                .py(px(spacing.xs))
                .rounded(px(radii.sm))
                .text_size(px(theme.typography.ui_size))
                .text_color(hsla(if accent { s.accent_fg } else { s.text }))
                .bg(hsla(if accent { s.accent } else { s.raised }))
                .when(!accent, |el| el.hover(move |el| el.bg(hsla(s.overlay))))
                .cursor_pointer()
                .child(text);
            tab_stop(pill, s.accent)
        };
        let (title, blurb, field, go, paste, other, other_mode) = match adding.mode {
            Panel::Server => (
                "Connect to a server",
                "The server lists your workers; terminals and screens still go to each worker \
                 directly. Its Tailscale name, LAN name or IP, with :port unless it listens on \
                 45560. Slopty does not encrypt: the tailnet or VPN is the boundary.",
                "Server address",
                "Connect",
                "Paste & connect",
                "Add a worker by address instead",
                Panel::Worker,
            ),
            Panel::Worker => (
                "Add a worker",
                "For a setup without a server. Its Tailscale name, LAN name or IP, with :port \
                 unless it listens on 45550. Slopty does not encrypt: the tailnet or VPN is the \
                 boundary.",
                "Worker address",
                "Add",
                "Paste & add",
                "Connect to a server instead",
                Panel::Server,
            ),
        };
        let can_cancel = !self.workers.is_empty() || self.server.is_some();
        let switch = div()
            .id("panel-switch")
            .role(Role::Button)
            .aria_label(other)
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text_muted))
            .hover(move |el| el.text_color(hsla(s.text)))
            .cursor_pointer()
            .child(other)
            .on_click(cx.listener(move |this, _ev, window, cx| {
                this.show_add_worker(other_mode, window, cx);
            }));
        div()
            .id("add-worker")
            .occlude()
            .flex()
            .flex_col()
            .gap(px(spacing.md))
            .w(px(420.0))
            .max_w_full()
            .p(px(spacing.xl))
            .rounded(px(radii.md))
            .bg(hsla(s.panel))
            .border_1()
            .border_color(hsla(s.border))
            .child(
                div()
                    .id("add-worker-title")
                    .role(Role::Heading)
                    .aria_label(title)
                    .text_size(px(theme.typography.title()))
                    .text_color(hsla(s.text))
                    .child(title),
            )
            .child(
                div()
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .child(blurb),
            )
            .child(Input::new(&adding.address).aria_label(field))
            .child(
                div()
                    .flex()
                    .gap(px(spacing.sm))
                    .items_center()
                    .child(
                        button("add", go, true).on_click(
                            cx.listener(|this, _ev, _window, cx| this.add_from_panel(cx)),
                        ),
                    )
                    .child(button("paste-address", paste, false).on_click(
                        cx.listener(|this, _ev, window, cx| this.paste_address(window, cx)),
                    ))
                    .when(can_cancel, |row| {
                        row.child(button("cancel-add", "Cancel", false).on_click(
                            cx.listener(|this, _ev, window, cx| this.cancel_add_worker(window, cx)),
                        ))
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(theme.typography.small()))
                            .text_color(hsla(if adding.error.is_some() {
                                s.error
                            } else {
                                s.text_muted
                            }))
                            .child(SharedString::from(match (&adding.error, adding.busy) {
                                (Some(e), _) => e.clone(),
                                (None, true) => "connecting…".to_owned(),
                                (None, false) => String::new(),
                            })),
                    ),
            )
            .child(tab_stop(switch, s.accent))
    }

    /// Esc, Tab, sticky Control, arrows and the shell symbols a phone keyboard hides; shown
    /// above the keyboard inset while a terminal is active.
    fn key_bar(&self, target: &KeyTarget, cx: &Context<Self>) -> gpui::AnyElement {
        match target {
            KeyTarget::Terminal(terminal) => self.terminal_key_bar(terminal, cx),
            KeyTarget::Screen(screen) => self.screen_key_bar(screen, cx),
        }
    }

    /// A key cap of the bar: `raised` on the `panel` bar, `overlay` while pressed, the accent
    /// with its foreground when `lit` (armed or toggled on).
    fn key_cap(&self, id: String, lit: bool, text_size: f32) -> gpui::Stateful<gpui::Div> {
        let s = &self.theme.surfaces;
        let pressed = if lit { s.accent } else { s.overlay };
        div()
            .id(SharedString::from(id))
            .flex_1()
            .h(px(KEY_BAR_H - self.theme.spacing.sm))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(self.theme.radii.sm))
            .text_size(px(text_size))
            .text_color(hsla(if lit { s.accent_fg } else { s.text }))
            .bg(hsla(if lit { s.accent } else { s.raised }))
            .active(move |el| el.bg(hsla(pressed)))
    }

    /// One key of the bar; `lit` draws it armed.
    fn bar_key(
        &self,
        id: String,
        label: &'static str,
        lit: bool,
        on_click: impl Fn(&mut Window, &mut App) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        let accent = self.theme.surfaces.accent;
        let key = self
            .key_cap(id, lit, self.theme.typography.ui_size)
            .role(Role::Button)
            .aria_label(SharedString::from(key_label(label, lit)))
            .child(SharedString::from(label));
        tab_stop(key, accent).on_click(move |_ev, window, cx| on_click(window, cx))
    }

    /// The bar over a remote window: chords and arrows, copy and paste through the host.
    fn screen_key_bar(&self, screen: &Entity<ScreenView>, cx: &Context<Self>) -> gpui::AnyElement {
        let s = &self.theme.surfaces;
        let view = screen.read(cx);
        let (control, command) = (view.sticky(Sticky::Control), view.sticky(Sticky::Command));
        let mut bar = div()
            .h(px(KEY_BAR_H))
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .px(px(self.theme.spacing.xs))
            .gap(px(self.theme.spacing.xs))
            .bg(hsla(s.panel))
            .border_t_1()
            .border_color(hsla(s.border))
            .font_family(self.theme.typography.ui_family.clone());
        for (label, key, typed) in SCREEN_BAR_KEYS {
            let sticky = match key {
                "" => Some(Sticky::Control),
                "cmd" => Some(Sticky::Command),
                _ => None,
            };
            let lit = matches!(sticky, Some(Sticky::Control) if control)
                || matches!(sticky, Some(Sticky::Command) if command);
            let target = screen.clone();
            bar =
                bar.child(self.bar_key(format!("skey-{label}"), label, lit, move |_window, cx| {
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
                }));
        }
        let target = screen.clone();
        bar = bar.child(self.bar_key("skey-copy".to_owned(), "copy", false, move |_w, cx| {
            target.update(cx, ScreenView::copy_key);
        }));
        let target = screen.clone();
        bar = bar.child(self.bar_key("skey-paste".to_owned(), "paste", false, move |_w, cx| {
            target.update(cx, ScreenView::paste_key);
        }));
        bar.into_any_element()
    }

    fn terminal_key_bar(
        &self,
        terminal: &Entity<TerminalView>,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let s = &self.theme.surfaces;
        let armed = terminal.read(cx).sticky_control();
        let armed_command = terminal.read(cx).sticky_command();
        let has_selection = terminal.read(cx).selection().is_some();
        let mut bar = div()
            .h(px(KEY_BAR_H))
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .px(px(self.theme.spacing.xs))
            .gap(px(self.theme.spacing.xs))
            .bg(hsla(s.panel))
            .border_t_1()
            .border_color(hsla(s.border))
            .font_family(self.theme.typography.ui_family.clone());
        let ui = self.theme.typography.ui_size;
        let small = self.theme.typography.small();
        for (label, key, typed) in BAR_KEYS {
            let is_control = key.is_empty();
            let is_command = key == "cmd";
            let lit = (is_control && armed) || (is_command && armed_command);
            let target = terminal.clone();
            let key_el = self
                .key_cap(format!("key-{label}"), lit, ui)
                .role(Role::Button)
                .aria_label(SharedString::from(key_label(label, lit)))
                .child(SharedString::from(label));
            bar = bar.child(tab_stop(key_el, s.accent).on_click(move |_ev, _window, cx| {
                target.update(cx, |t, cx| {
                    if is_control {
                        let on = !t.sticky_control();
                        t.set_sticky_control(on, cx);
                    } else if is_command {
                        let on = !t.sticky_command();
                        t.set_sticky_command(on, cx);
                    } else {
                        t.bar_key(
                            gpui::Keystroke {
                                modifiers: gpui::Modifiers::default(),
                                key: key.to_owned(),
                                key_char: typed.map(str::to_owned),
                            },
                            cx,
                        );
                    }
                });
            }));
        }
        // The phone has no ⌘C/⌘V: while text is selected the bar offers copy, otherwise paste.
        let target = terminal.clone();
        let clipboard = self
            .key_cap("key-clipboard".to_owned(), has_selection, small)
            .role(Role::Button)
            .aria_label(if has_selection { "Copy" } else { "Paste" })
            .child(if has_selection { "copy" } else { "paste" });
        bar = bar.child(tab_stop(clipboard, s.accent).on_click(move |_ev, window, cx| {
            target.update(cx, |t, cx| {
                if has_selection {
                    // Copy, then the key reads "paste" again.
                    t.copy(&slopty_ui::terminal::Copy, window, cx);
                    t.clear_selection(cx);
                } else {
                    t.paste_clipboard(&slopty_ui::terminal::Paste, window, cx);
                }
            });
        }));
        // No ⌘F either: "find" opens the search bar, or closes it while it is open.
        let target = terminal.clone();
        let finding = terminal.read(cx).finding();
        let find = self
            .key_cap("key-find".to_owned(), finding, small)
            .role(Role::Button)
            .aria_label(if finding { "Close find" } else { "Find" })
            .child("find");
        bar = bar.child(tab_stop(find, s.accent).on_click(move |_ev, window, cx| {
            target.update(cx, |t, cx| {
                if finding {
                    t.close_find(&slopty_ui::terminal::CloseFind, window, cx);
                } else {
                    t.find(&slopty_ui::terminal::Find, window, cx);
                }
            });
        }));
        bar.into_any_element()
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
    let name = key_name(label);
    if lit { format!("{name}, armed") } else { name.to_owned() }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The frame's draw starts here; the probe element at the end of the tree closes it.
        slopty_ui::frames::begin(cx);
        let surfaces = &self.theme.surfaces;
        // Notch / Dynamic Island, home indicator and the soft keyboard on iOS; zero on macOS.
        // The workspace keeps the top and the sides clear itself.
        let insets = window.insets().effective();
        let key_bar = key_bar_visible(KEY_BAR, self.hardware_keyboard)
            .then(|| self.view.read(cx).active_key_target())
            .flatten()
            .map(|target| self.key_bar(&target, cx));
        if std::mem::take(&mut self.pending_focus_editor)
            && let Some(editor) = self.settings_editor.clone()
        {
            editor.update(cx, |e, cx| e.focus(window, cx));
        }
        let settings_editor = self.settings_editor.clone();
        let adding = self.adding.as_ref().map(|adding| {
            kit::backdrop(&self.theme)
                .id("add-worker-backdrop")
                .flex()
                .items_center()
                .justify_center()
                .p(px(self.theme.spacing.lg))
                .child(self.add_worker_panel(adding, cx))
        });
        div()
            .size_full()
            .relative()
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
            .child(div().flex_1().w_full().min_h_0().child(self.view.clone()))
            .when_some(key_bar, |el, bar| el.child(div().w_full().px(insets.left).child(bar)))
            .child(div().w_full().h(insets.bottom))
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
        LinkEvent::Control(HostMsg::Term { session, event }) => {
            view.update(cx, |v, cx| v.term_event(session, event, cx));
        }
        LinkEvent::Control(HostMsg::Items(sync)) => {
            view.update(cx, |v, cx| v.apply_sync(key, sync, cx));
        }
        LinkEvent::Control(HostMsg::SessionOpened(summary)) => {
            view.update(cx, |v, cx| v.session_opened(key, summary, cx));
        }
        LinkEvent::Control(HostMsg::SessionClosed { session, .. }) => {
            view.update(cx, |v, cx| v.session_closed(session, cx));
        }
        LinkEvent::Control(HostMsg::Screen(event)) => {
            view.update(cx, |v, cx| v.screen_event(key, event, cx));
        }
        LinkEvent::Control(HostMsg::Agent(event)) => {
            view.update(cx, |v, cx| v.agent_event(event, cx));
        }
        LinkEvent::Control(HostMsg::File { path, read }) => {
            view.update(cx, |v, cx| v.file_read(key, &path, &read, cx));
        }
        LinkEvent::Control(HostMsg::FoundFiles { root, query, paths }) => {
            view.update(cx, |v, cx| v.files_found(&root, &query, &paths, cx));
        }
        LinkEvent::Control(HostMsg::HooksInstalled { ok, message }) => {
            view.update(cx, |v, cx| {
                v.show_notice(message, cx);
                if !ok {
                    // The offer stays on the header so it can be tried again.
                    v.hooks_offer_failed(key, cx);
                }
            });
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

/// The app's own bindings, outside any view's context.
fn app_key_bindings() -> Vec<gpui::KeyBinding> {
    vec![
        gpui::KeyBinding::new("cmd-,", OpenSettings, None),
        gpui::KeyBinding::new("cmd-shift-h", AddWorker, None),
    ]
}

/// The app's lines for the command palette, after the workspace's.
fn app_palette_items() -> Vec<slopty_ui::palette::PaletteItem> {
    let bindings = app_key_bindings();
    let item = |label: &str, action: Box<dyn gpui::Action>| {
        slopty_ui::palette::PaletteItem::new(label, action, &bindings)
    };
    vec![
        item("Open settings", Box::new(OpenSettings)),
        item("Connect to a server", Box::new(ConnectServer)),
        item("Disconnect from the server", Box::new(DisconnectServer)),
        item("Add a worker", Box::new(AddWorker)),
    ]
}

/// Where this device keeps its layout: beside the settings, in the client's data directory.
fn layout_path() -> std::path::PathBuf {
    slopty_settings::data_dir().join("layout.json")
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
        #[cfg(feature = "e2e")]
        view.set_animation(false);
        view
    });
    let workspace = cx.new(|cx| {
        let events = cx.subscribe(&view, |ws: &mut Workspace, _view, event, cx| match event {
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
        Workspace {
            workers: Vec::new(),
            directory: slopty_client::directory::Directory::default(),
            server: None,
            server_generation: 0,
            view: view.clone(),
            hardware_keyboard: hardware_keyboard_attached(),
            theme: Theme::default(),
            settings: Settings::default(),
            window_dark: true,
            subscriptions: vec![events, changes],
            adding: None,
            runtime: handle,
            window: None,
            settings_path: settings_path.clone(),
            settings_editor: None,
            pending_focus_editor: false,
        }
    });
    let root_view = workspace.clone();
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
            ws.window_dark = dark;
            ws.apply_loaded(loaded, cx);
        });
        cx.new(|cx| Root::new(root_view, window, cx))
    })?;
    watch_settings(settings_path, workspace.clone(), cx);
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

/// Reload `settings.toml` whenever its stamp changes (see [`settings`] for why this polls),
/// and notice a hardware keyboard coming or going on the same tick.
fn watch_settings(path: std::path::PathBuf, workspace: Entity<Workspace>, cx: &App) {
    cx.spawn(async move |cx| {
        let mut last = settings::Stamp::of(&path);
        loop {
            cx.background_executor().timer(settings::POLL).await;
            let keyboard = hardware_keyboard_attached();
            workspace.update(cx, |ws, cx| ws.set_hardware_keyboard(keyboard, cx));
            let now = settings::Stamp::of(&path);
            if now == last {
                continue;
            }
            last = now;
            tracing::info!(path = %path.display(), "settings changed; reloading");
            let loaded = Settings::load(&path);
            workspace.update(cx, |ws, cx| ws.apply_loaded(loaded, cx));
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
    use super::key_bar_visible;

    #[test]
    fn the_key_bar_is_for_glass_without_a_keyboard() {
        assert!(key_bar_visible(true, false));
        assert!(!key_bar_visible(true, true));
        assert!(!key_bar_visible(false, false));
        assert!(!key_bar_visible(false, true));
    }
}
