//! The Slopty app shell, shared by the macOS and iOS apps.
//!
//! Connects to every paired host at once and shows one host's canvas at a time (the switcher
//! in the top bar, ⌘⌥→/←): every terminal that host knows about, on one plane shared by all its
//! clients. Networking runs on a tokio runtime thread; GPUI owns the main thread. The two talk
//! through channels only. The platform binaries set up logging, the runtime and the GPUI
//! application, then call [`open_workspace`].

#![forbid(unsafe_code)]

mod e2e;
pub mod hosts;
pub mod net;
pub mod settings;

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
pub use hosts::actions::{AddHost, ForgetHost, NextHost, PrevHost};
use hosts::{HostSlot, HostStatus};
pub use settings::actions::OpenSettings;
use slopty_client::LinkEvent;
use slopty_core::SessionId;
use slopty_net::EndpointId;
use slopty_proto::HostMsg;
use slopty_settings::{Loaded, Settings};
use slopty_theme::{Theme, alpha};
use slopty_ui::a11y::{key_name, tab_stop};
use slopty_ui::canvas::{
    AddWindow, CanvasEvent, CanvasView, FitAll, KeyTarget, NewAgent, NewNote, NewTerminal,
    NextAttention, OpenPalette,
};
use slopty_ui::colors::{hsla, hsla_alpha};
use slopty_ui::kit;
use slopty_ui::screen::{ScreenView, Sticky};
use slopty_ui::settings_editor::{SettingsEditor, SettingsEditorEvent};
use slopty_ui::terminal::TerminalView;

/// Height of the top bar (the titlebar area; traffic lights sit at its left on macOS).
const TOP_BAR: f32 = 38.0;
/// Space reserved for the traffic lights.
#[cfg(target_os = "macos")]
const LEADING_INSET: f32 = 78.0;
/// No traffic lights on iOS; the safe area is added from the window's insets.
#[cfg(not(target_os = "macos"))]
const LEADING_INSET: f32 = 12.0;
/// Keyboard shortcut hints make sense where there is a keyboard with a ⌘ key.
const SHORTCUT_HINTS: bool = cfg!(target_os = "macos");
/// Below this window width (points) the top bar is laid out for a phone.
const NARROW_BAR: f32 = 600.0;
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
/// How long a settings notice (parse error, unknown keys) replaces the status text.
const NOTICE_FOR: std::time::Duration = std::time::Duration::from_secs(6);
/// Nothing heard from the host for this long is shown as a warning (keep-alives run every 5 s).
const SILENCE_WARN: std::time::Duration = std::time::Duration::from_secs(8);
/// Nothing heard for this long and the connection is given up so the reconnect loop takes
/// over: a restarted host is back in ~1 s instead of after the transport's 45 s idle timeout.
/// Three missed keep-alives, the same bar noq uses to abandon a path.
const SILENCE_DROP: std::time::Duration = std::time::Duration::from_secs(15);

/// The pairing panel: shown until this installation knows a host, and on "Add host…".
#[derive(Debug)]
struct Pairing {
    /// Where the ticket is typed or pasted.
    ticket: Entity<InputState>,
    /// A pairing attempt is in flight.
    busy: bool,
    /// Why the last attempt failed.
    error: Option<String>,
}

/// The window's root view.
#[derive(Debug)]
pub struct Workspace {
    /// Every paired host, each with its own link and canvas, in switcher order.
    hosts: Vec<HostSlot>,
    /// The host whose canvas is shown.
    active: Option<EndpointId>,
    /// The host switcher is open.
    switcher: bool,
    /// A physical keyboard is attached (polled with the settings; hides the key bar).
    hardware_keyboard: bool,
    theme: Theme,
    /// The user's `settings.toml` as last loaded (defaults when absent or broken).
    settings: Settings,
    /// The window's appearance is dark (`theme.appearance = "system"` follows it).
    window_dark: bool,
    /// A transient message shown in place of the status (with the sequence that clears it).
    notice: Option<(u64, String)>,
    notice_seq: u64,
    subscriptions: Vec<gpui::Subscription>,
    pairing: Option<Pairing>,
    /// Networking runtime; connects and pairing run there.
    runtime: tokio::runtime::Handle,
    /// The window, for focusing a canvas from a task or a banner.
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

    /// Drop the editor and hand the keyboard back to the canvas.
    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_editor.take().is_none() {
            return;
        }
        if let Some(canvas) = self.active_canvas() {
            let handle = canvas.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
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

    /// Take a (re)loaded settings file: log what was odd about it, show it in the bar for a
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
        self.settings = loaded.settings;
        self.rebuild_theme(cx);
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
        let canvases: Vec<_> = self.hosts.iter().filter_map(|h| h.canvas.clone()).collect();
        for canvas in canvases {
            canvas.update(cx, |c, cx| c.set_theme(theme.clone(), cx));
        }
        if let Some(editor) = &self.settings_editor {
            editor.update(cx, |e, cx| e.set_theme(theme.clone(), cx));
        }
        self.theme = theme;
        cx.refresh_windows();
        cx.notify();
    }

    /// Replace the status text with `text` for [`NOTICE_FOR`].
    fn show_notice(&mut self, text: String, cx: &mut Context<Self>) {
        self.notice_seq = self.notice_seq.wrapping_add(1);
        let seq = self.notice_seq;
        self.notice = Some((seq, text));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NOTICE_FOR).await;
            let _cleared = this.update(cx, |ws, cx| {
                if ws.notice.as_ref().is_some_and(|(s, _)| *s == seq) {
                    ws.notice = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn slot(&self, id: EndpointId) -> Option<&HostSlot> {
        self.hosts.iter().find(|h| h.id == id)
    }

    fn slot_mut(&mut self, id: EndpointId) -> Option<&mut HostSlot> {
        self.hosts.iter_mut().find(|h| h.id == id)
    }

    fn active_slot(&self) -> Option<&HostSlot> {
        self.active.and_then(|id| self.slot(id))
    }

    /// The canvas on show, if its host is connected.
    fn active_canvas(&self) -> Option<Entity<CanvasView>> {
        self.active_slot().and_then(|h| h.canvas.clone())
    }

    /// Agents waiting on the human across every host (the pill and the Dock badge).
    fn needs_you_total(&self) -> usize {
        self.hosts.iter().fold(0, |n, h| n.saturating_add(h.needs_you))
    }

    fn refresh_badge(&self) {
        slopty_platform::set_badge(self.needs_you_total());
    }

    /// What the bar says about the host on show.
    fn status_text(&self) -> String {
        match self.active_slot() {
            Some(host) => host.status.text(),
            None if self.hosts.is_empty() => "not paired".to_owned(),
            None => String::new(),
        }
    }

    /// The host whose canvas holds `session` (a banner names only the session).
    fn host_of_session(&self, session: SessionId, cx: &App) -> Option<EndpointId> {
        self.hosts
            .iter()
            .find(|h| h.canvas.as_ref().is_some_and(|c| c.read(cx).has_session(session)))
            .map(|h| h.id)
    }

    /// A banner for `session` was activated: switch to whichever host holds it and reveal it.
    /// The banner's tag is only the session UUID, so the host is found by asking each canvas
    /// `has_session`.
    fn notification_response(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(host) = self.host_of_session(session, cx) else { return };
        if self.active != Some(host) {
            self.activate(host, window, cx);
        }
        if let Some(canvas) = self.active_canvas() {
            canvas.update(cx, |c, cx| c.notification_response(session, cx));
        }
    }

    /// Start (or refresh) a host: a slot in the switcher and a connect loop of its own.
    fn add_host(&mut self, id: EndpointId, name: String, cx: &mut Context<Self>) {
        if let Some(slot) = self.slot_mut(id) {
            slot.name = name;
            if slot.status == HostStatus::NeedsPairing {
                slot.status = HostStatus::Connecting;
            }
            cx.notify();
            return;
        }
        self.hosts.push(HostSlot::new(id, name));
        if self.active.is_none() {
            self.active = Some(id);
        }
        self.spawn_host_loop(id, cx);
        cx.notify();
    }

    /// Show `id`'s canvas and give it the keyboard.
    fn activate(&mut self, id: EndpointId, window: &mut Window, cx: &mut Context<Self>) {
        if self.slot(id).is_none() {
            return;
        }
        self.active = Some(id);
        self.switcher = false;
        if let Some(canvas) = self.active_canvas() {
            let handle = canvas.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    /// Show the host `steps` places after the current one, wrapping.
    fn step_host(&mut self, steps: isize, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.hosts.len();
        if n == 0 {
            return;
        }
        let current =
            self.active.and_then(|id| self.hosts.iter().position(|h| h.id == id)).unwrap_or(0);
        let len = isize::try_from(n).unwrap_or(isize::MAX);
        let at = isize::try_from(current).unwrap_or(0);
        let next = at.saturating_add(steps).rem_euclid(len);
        if let Some(id) = usize::try_from(next).ok().and_then(|i| self.hosts.get(i)).map(|h| h.id) {
            self.activate(id, window, cx);
        }
    }

    /// Drop a host: the pairing, the link and the slot. The next host takes the stage; with
    /// none left the pairing panel returns.
    fn forget_host(&mut self, id: EndpointId, window: &mut Window, cx: &mut Context<Self>) {
        match net::forget_host(id) {
            Ok(_removed) => {}
            Err(e) => {
                self.show_notice(format!("forget host: {e:#}"), cx);
                return;
            }
        }
        let Some(at) = self.hosts.iter().position(|h| h.id == id) else { return };
        let slot = self.hosts.remove(at);
        if let Some(link) = slot.link.as_ref().and_then(std::sync::Weak::upgrade) {
            link.abandon("host forgotten");
        }
        self.switcher = false;
        if self.active == Some(id) {
            self.active = None;
            if let Some(next) = self.hosts.get(at.min(self.hosts.len().saturating_sub(1))) {
                let next = next.id;
                self.activate(next, window, cx);
            }
        }
        if self.hosts.is_empty() {
            self.show_pairing(window, cx);
        }
        self.refresh_badge();
        cx.notify();
    }

    /// Jump to the next agent needing the human: on the host on show if it has one, else on
    /// the first host that does.
    fn next_attention(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let on_show = self.active_slot().filter(|h| h.needs_you > 0).map(|h| h.id);
        let target = on_show.or_else(|| self.hosts.iter().find(|h| h.needs_you > 0).map(|h| h.id));
        let Some(id) = target else { return };
        if self.active != Some(id) {
            self.activate(id, window, cx);
        }
        if let Some(canvas) = self.active_canvas() {
            canvas.update(cx, |c, cx| c.next_attention(&NextAttention, window, cx));
        }
    }

    /// Connect to `id` and keep it connected: each drop (host restart, network change,
    /// silence) is retried with a capped backoff; the loop ends when the host is forgotten.
    fn spawn_host_loop(&self, id: EndpointId, cx: &Context<Self>) {
        let handle = self.runtime.clone();
        cx.spawn(async move |this, cx| {
            let mut failures: u32 = 0;
            loop {
                let Ok(true) = this.update(cx, |ws, _cx| ws.slot(id).is_some()) else { break };
                let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
                handle.spawn(async move {
                    let _sent = ready_tx.send(net::connect_to(id).await);
                });
                let outcome = ready_rx.await;
                let Ok(Ok(connected)) = outcome else {
                    let status = match outcome {
                        Ok(Err(net::ConnectError::NotPaired)) => HostStatus::NeedsPairing,
                        Ok(Err(e)) => HostStatus::Reconnecting(e.to_string()),
                        Ok(Ok(_)) | Err(_) => {
                            HostStatus::Reconnecting("connection task died".to_owned())
                        }
                    };
                    failures = failures.saturating_add(1);
                    let delay = retry_delay(failures);
                    let _set = this.update(cx, |ws, cx| {
                        if let Some(slot) = ws.slot_mut(id) {
                            slot.status = status;
                        }
                        cx.notify();
                    });
                    cx.background_executor().timer(delay).await;
                    continue;
                };
                failures = 0;
                let net::Connected { me, ack, sender, mut events, link } = connected;
                let link = std::sync::Arc::new(link);
                let screen_link = std::sync::Arc::clone(&link);
                let open_screen: slopty_ui::screen::ScreenFactory =
                    std::sync::Arc::new(move |stream, codec| screen_link.screen(stream, codec));
                let weak_link = std::sync::Arc::downgrade(&link);
                let Ok(Some(canvas)) = this.update(cx, |ws, cx| {
                    let theme = ws.theme.clone();
                    let sessions = ack.sessions.clone();
                    let canvas = cx.new(|cx| {
                        let mut canvas =
                            CanvasView::new(me, sender, sessions, open_screen, theme, cx);
                        canvas.extend_palette(app_palette_items());
                        // Under the self-test a frame is a step, not a moment: a camera still
                        // flying when `dump` runs would report where it was passing through.
                        // The tests assert the destination.
                        #[cfg(feature = "e2e")]
                        canvas.set_animation(false);
                        canvas
                    });
                    let events = cx.subscribe(&canvas, move |ws, _canvas, event, cx| match event {
                        CanvasEvent::Zoom(z) => {
                            if let Some(slot) = ws.slot_mut(id) {
                                slot.zoom = *z;
                            }
                            cx.notify();
                        }
                        CanvasEvent::NeedsYou(n) => {
                            if let Some(slot) = ws.slot_mut(id) {
                                slot.needs_you = *n;
                            }
                            ws.refresh_badge();
                            cx.notify();
                        }
                        CanvasEvent::Attention(_session) => {
                            slopty_platform::attention();
                            slopty_platform::bounce();
                        }
                        // A bell while the human is elsewhere is an alert; in front of the
                        // window the view's own flash is enough.
                        CanvasEvent::Bell(_session) => {
                            if settings::bell_alerts(&ws.settings, cx.active_window().is_some())
                            {
                                slopty_platform::attention();
                                slopty_platform::bounce();
                            }
                        }
                        CanvasEvent::Notice(text) => ws.show_notice(text.clone(), cx),
                    });
                    // The chrome follows the active item (key bar target), so every canvas
                    // change re-renders it; the workspace is a few labels, so this is cheap.
                    let changes = cx.observe(&canvas, |_ws, _canvas, cx| cx.notify());
                    let slot = ws.slot_mut(id)?;
                    if let Some((camera, active)) = slot.resume.take() {
                        canvas.update(cx, |c, cx| c.resume_at(camera, active, cx));
                    }
                    slot.subscriptions = vec![events, changes];
                    slot.canvas = Some(canvas.clone());
                    slot.needs_you = 0;
                    slot.link = Some(weak_link);
                    slot.name.clone_from(&ack.name);
                    slot.status = HostStatus::Connected;
                    ws.refresh_badge();
                    let on_show = ws.active == Some(id);
                    if let (true, Some(window)) = (on_show, ws.window) {
                        let focus = canvas.clone();
                        cx.defer(move |cx| {
                            let _focused = window.update(cx, move |_root, window, cx| {
                                let handle = focus.read(cx).focus_handle(cx);
                                window.focus(&handle, cx);
                            });
                        });
                    }
                    cx.notify();
                    Some(canvas)
                }) else {
                    break;
                };
                // Once a second: RTT for the top bar and the predictors, and a liveness
                // check. QUIC keep-alives make the host send something every few seconds;
                // when the received datagram count stops moving the host is gone or
                // unreachable, and the bar says so long before the transport's idle timeout
                // drops the connection.
                let rtt_link = std::sync::Arc::downgrade(&link);
                let rtt_canvas = canvas.clone();
                let rtt_workspace = this.clone();
                cx.spawn(async move |cx| {
                    let mut heard = (0_u64, std::time::Instant::now());
                    loop {
                        cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                        let Some(link) = rtt_link.upgrade() else { break };
                        let rtt = link.rtt();
                        let relayed = link.relayed();
                        let received = link.received_datagrams();
                        if received != heard.0 {
                            heard = (received, std::time::Instant::now());
                        }
                        let gap = heard.1.elapsed();
                        let silent = (gap >= SILENCE_WARN).then_some(gap);
                        tracing::debug!(host = %id.fmt_short(), paths = %link.paths(), received, "link paths");
                        if gap >= SILENCE_DROP {
                            tracing::warn!(host = %id.fmt_short(), ?gap, paths = %link.paths(), "host silent; reconnecting");
                            link.abandon("host silent");
                            break;
                        }
                        rtt_canvas.update(cx, |c, cx| c.set_rtt(rtt, cx));
                        let _set = rtt_workspace.update(cx, |ws, cx| {
                            if let Some(slot) = ws.slot_mut(id) {
                                slot.rtt = rtt;
                                slot.relayed = relayed;
                                slot.silent = silent;
                            }
                            cx.notify();
                        });
                    }
                })
                .detach();
                let mut first_snapshot = true;
                tracing::debug!(host = %id.fmt_short(), sessions = ack.sessions.len(), "link up; pumping events");
                // One foreground turn per frame, not per event: a flood of terminal frames
                // from many sessions would otherwise run a GPUI update (and, when the window
                // is drawn from the update, a whole frame) for each of them. The first event
                // after a quiet spell is applied at once; while events keep coming they are
                // applied in one update per nominal frame.
                let pace = frame_nominal();
                let mut last_apply: Option<std::time::Instant> = None;
                while let Some(first) = events.recv().await {
                    if let Some(due) = last_apply.and_then(|at| at.checked_add(pace)) {
                        let wait = due.saturating_duration_since(std::time::Instant::now());
                        if !wait.is_zero() {
                            cx.background_executor().timer(wait).await;
                        }
                    }
                    let mut batch = Vec::with_capacity(LINK_BATCH);
                    batch.push(first);
                    while batch.len() < LINK_BATCH
                        && let Ok(next) = events.try_recv()
                    {
                        batch.push(next);
                    }
                    last_apply = Some(std::time::Instant::now());
                    let disconnected = cx.update(|cx| {
                        batch.into_iter().fold(false, |down, event| {
                            down | apply_link_event(&this, &canvas, id, &mut first_snapshot, event, cx)
                        })
                    });
                    if disconnected {
                        break;
                    }
                }
                // Dropping the link closes the connection; the endpoint stays for the retry.
                drop(link);
                cx.background_executor().timer(retry_delay(0)).await;
            }
        })
        .detach();
    }

    /// Show the pairing panel (idempotent).
    fn show_pairing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pairing.is_some() {
            return;
        }
        let ticket = cx.new(|cx| {
            InputState::new(window, cx).placeholder("sloptypair… (from `slopty host ticket`)")
        });
        self.subscriptions.push(cx.subscribe(&ticket, |this, _input, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.pair(cx);
            }
        }));
        ticket.update(cx, |input, cx| input.focus(window, cx));
        self.pairing = Some(Pairing { ticket, busy: false, error: None });
        self.switcher = false;
        cx.notify();
    }

    /// Close the pairing panel (only offered while some host is paired).
    fn cancel_pairing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.hosts.is_empty() {
            return;
        }
        self.pairing = None;
        if let Some(id) = self.active {
            self.activate(id, window, cx);
        }
        cx.notify();
    }

    /// Put the clipboard's text into the ticket field.
    fn paste_ticket(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pairing) = &self.pairing else { return };
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
        pairing.ticket.update(cx, |input, cx| input.set_value(text.trim().to_owned(), window, cx));
        self.pair(cx);
    }

    /// Redeem the ticket in the field on the runtime; report back to the panel.
    fn pair(&mut self, cx: &mut Context<Self>) {
        let Some(pairing) = &mut self.pairing else { return };
        if pairing.busy {
            return;
        }
        let ticket = pairing.ticket.read(cx).value().trim().to_owned();
        if ticket.is_empty() {
            return;
        }
        pairing.busy = true;
        pairing.error = None;
        cx.notify();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.runtime.spawn(async move {
            let _sent = tx.send(net::pair_host(&ticket).await);
        });
        cx.spawn(async move |this, cx| {
            let outcome = rx.await;
            let _updated = this.update(cx, |ws, cx| {
                match outcome {
                    Ok(Ok(net::Paired { id, name })) => {
                        ws.pairing = None;
                        ws.show_notice(format!("paired with {name}"), cx);
                        ws.add_host(id, name, cx);
                        ws.active = Some(id);
                    }
                    Ok(Err(e)) => {
                        if let Some(p) = &mut ws.pairing {
                            p.busy = false;
                            p.error = Some(format!("{e:#}"));
                        }
                    }
                    Err(_dropped) => {
                        if let Some(p) = &mut ws.pairing {
                            p.busy = false;
                            p.error = Some("pairing task died".to_owned());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn pairing_panel(&self, pairing: &Pairing, cx: &Context<Self>) -> impl IntoElement {
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
        div()
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
                    .id("pairing-title")
                    .role(Role::Heading)
                    .aria_label("Pair with a host")
                    .text_size(px(theme.typography.title()))
                    .text_color(hsla(s.text))
                    .child("Pair with a host"),
            )
            .child(
                div()
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .child("On the host Mac run `slopty host ticket`, then paste the ticket here."),
            )
            .child(Input::new(&pairing.ticket).aria_label("Pairing ticket"))
            .child(
                div()
                    .flex()
                    .gap(px(spacing.sm))
                    .items_center()
                    .child(
                        button("pair", "Pair", true)
                            .on_click(cx.listener(|this, _ev, _window, cx| this.pair(cx))),
                    )
                    .child(button("paste-ticket", "Paste & pair", false).on_click(
                        cx.listener(|this, _ev, window, cx| this.paste_ticket(window, cx)),
                    ))
                    .when(!self.hosts.is_empty(), |row| {
                        row.child(button("cancel-pairing", "Cancel", false).on_click(
                            cx.listener(|this, _ev, window, cx| this.cancel_pairing(window, cx)),
                        ))
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(theme.typography.small()))
                            .text_color(hsla(if pairing.error.is_some() {
                                s.error
                            } else {
                                s.text_muted
                            }))
                            .child(SharedString::from(match (&pairing.error, pairing.busy) {
                                (Some(e), _) => e.clone(),
                                (None, true) => "pairing…".to_owned(),
                                (None, false) => String::new(),
                            })),
                    ),
            )
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

    /// `narrow`: a phone-width window; the readouts (RTT, zoom) go so every button fits.
    fn top_bar(
        &self,
        safe_top: gpui::Pixels,
        narrow: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let (spacing, radii) = (theme.spacing, theme.radii);
        // Bar text is the small step of the scale.
        let ui = theme.typography.small();
        let active = self.active_slot();
        let zoom = active.map_or(1.0, |h| h.zoom);
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "0–400")]
        let zoom_pct = (zoom * 100.0).round().max(0.0) as u32;
        let (rtt, relayed, silent) =
            active.map_or((None, None, None), |h| (h.rtt, h.relayed, h.silent));
        let label = move |text: String| {
            div()
                .flex_none()
                .text_size(px(ui))
                .text_color(hsla(s.text_muted))
                .child(SharedString::from(text))
        };
        let hint_theme = Rc::new(theme.clone());
        let button = move |id: &'static str, text: &'static str, hint: &'static str| {
            let label = text.trim_start_matches("+ ");
            let pill = div()
                .id(id)
                .role(Role::Button)
                .aria_label(label)
                .flex_none()
                .px(px(if narrow { spacing.xs } else { spacing.sm }))
                .py(px(spacing.xs))
                .rounded(px(radii.sm))
                .text_size(px(ui))
                .text_color(hsla(s.text))
                .hover(move |el| el.bg(hsla(s.panel)))
                .active(move |el| el.bg(hsla(s.raised)))
                .cursor_pointer()
                .child(SharedString::from(text))
                .when(SHORTCUT_HINTS, |el| {
                    let theme = Rc::clone(&hint_theme);
                    el.tooltip(move |_window, cx| {
                        cx.new(|_| kit::Hint::new(label, hint, Rc::clone(&theme))).into()
                    })
                });
            tab_stop(pill, s.accent)
        };
        let new_button = button("new-terminal", "+ shell", "⌘T").on_click(cx.listener(
            |this, _ev, window, cx| {
                if let Some(canvas) = this.active_canvas() {
                    canvas.update(cx, |c, cx| c.new_terminal(&NewTerminal, window, cx));
                }
            },
        ));
        let agent_button =
            button("new-agent", "+ agent", "⌘⇧T").on_click(cx.listener(|this, _ev, window, cx| {
                if let Some(canvas) = this.active_canvas() {
                    canvas.update(cx, |c, cx| c.new_agent(&NewAgent, window, cx));
                }
            }));
        let note_button =
            button("new-note", "+ note", "⌘⇧N").on_click(cx.listener(|this, _ev, window, cx| {
                if let Some(canvas) = this.active_canvas() {
                    canvas.update(cx, |c, cx| c.new_note(&NewNote, window, cx));
                }
            }));
        let window_button = button("add-window", "+ window", "⌘O").on_click(cx.listener(
            |this, _ev, window, cx| {
                if let Some(canvas) = this.active_canvas() {
                    canvas.update(cx, |c, cx| c.add_window(&AddWindow, window, cx));
                }
            },
        ));
        // Every action by name: the phone's way to what has no button, and the Mac's index.
        let commands_button = button("commands", "⋯", "⌘⇧P").aria_label("Commands").on_click(
            cx.listener(|this, _ev, window, cx| {
                if let Some(canvas) = this.active_canvas() {
                    canvas.update(cx, |c, cx| c.open_palette(&OpenPalette, window, cx));
                }
            }),
        );
        let fit_button =
            button("fit-all", "fit", "⌘1").on_click(cx.listener(|this, _ev, window, cx| {
                if let Some(canvas) = this.active_canvas() {
                    canvas.update(cx, |c, cx| c.fit_all(&FitAll, window, cx));
                }
            }));
        // Agents waiting on the human, on any host: a warm pill with the count; a tap goes to
        // the next one (switching host when the one on show has none).
        let total = self.needs_you_total();
        let needs_you = (total > 0).then(|| {
            let warm = s.warn;
            let text =
                if total == 1 { "1 needs you".to_owned() } else { format!("{total} need you") };
            let pill = div()
                .id("needs-you")
                .role(Role::Button)
                .aria_label(SharedString::from(text.clone()))
                .flex_none()
                .px(px(spacing.sm))
                .py(px(spacing.xxs))
                .rounded(px(radii.xs))
                .text_size(px(ui))
                .text_color(hsla(warm))
                .bg(hsla_alpha(warm, alpha::FAINT))
                .hover(move |el| el.bg(hsla_alpha(warm, alpha::TINT)))
                .cursor_pointer()
                .child(SharedString::from(text))
                .when(SHORTCUT_HINTS, |el| {
                    let theme = Rc::new(self.theme.clone());
                    el.tooltip(move |_window, cx| {
                        cx.new(|_| kit::Hint::new("Go to the next one", "⌘⇧A", Rc::clone(&theme)))
                            .into()
                    })
                });
            tab_stop(pill, s.accent)
                .on_click(cx.listener(|this, _ev, window, cx| this.next_attention(window, cx)))
        });
        // The host on show, with its status dot; a click opens the switcher.
        let dot = active.map_or(s.text_muted, |h| status_color(&self.theme, &h.status));
        let host_name = active.map_or_else(|| "Slopty".to_owned(), |h| h.name.clone());
        let host_label = active.map_or_else(
            || "Hosts".to_owned(),
            |h| format!("Host {}, {}", h.name, h.status.text()),
        );
        let host_button = div()
            .id("host-switcher")
            .role(Role::Button)
            .aria_label(SharedString::from(host_label))
            .flex_shrink(1.0)
            // A phone's bar keeps the tap target even when the buttons crowd it.
            .min_w(px(if narrow { 96.0 } else { 0.0 }))
            .flex()
            .items_center()
            .gap(px(spacing.sm))
            .px(px(spacing.sm))
            .py(px(spacing.xs))
            .rounded(px(radii.sm))
            .hover(move |el| el.bg(hsla(s.panel)))
            .active(move |el| el.bg(hsla(s.raised)))
            .cursor_pointer()
            .child(div().flex_none().size(px(spacing.sm)).rounded_full().bg(hsla(dot)))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(theme.typography.ui_size))
                    .text_color(hsla(s.text))
                    .child(SharedString::from(host_name)),
            )
            .when(!self.hosts.is_empty(), |el| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(px(theme.typography.caption()))
                        .text_color(hsla(s.text_muted))
                        .child("▾"),
                )
            })
            .on_click(cx.listener(|this, _ev, _window, cx| {
                this.switcher = !this.switcher;
                cx.notify();
            }));
        let host_button = tab_stop(host_button, s.accent);
        div()
            .h(px(TOP_BAR) + safe_top)
            .pt(safe_top)
            .w_full()
            .flex()
            .items_center()
            .gap(px(if narrow { spacing.sm } else { spacing.md }))
            .pl(px(LEADING_INSET))
            .pr(px(spacing.md))
            .bg(hsla(s.canvas))
            .border_b_1()
            .border_color(hsla(s.border))
            .font_family(self.theme.typography.ui_family.clone())
            // The host name gives way first: a phone's bar must keep every button.
            .child(host_button)
            // Narrow: the status lives in the switcher rows; only a notice claims bar space.
            .child(match &self.notice {
                Some((_, text)) => label(text.clone()).text_color(hsla(s.warn)),
                None if narrow => label(String::new()),
                None => label(self.status_text()),
            })
            .child(div().flex_1())
            .child(match (silent, rtt, relayed) {
                (Some(gap), _, _) => {
                    label(format!("host silent {}s", gap.as_secs())).text_color(hsla(s.warn))
                }
                (None, _, _) if narrow => label(String::new()),
                (None, Some(d), Some(true)) => {
                    label(format!("{:.1} ms via relay", d.as_secs_f64() * 1e3))
                }
                (None, Some(d), _) => label(format!("{:.1} ms", d.as_secs_f64() * 1e3)),
                (None, None, _) => label(String::new()),
            })
            .when_some(needs_you, gpui::ParentElement::child)
            .when(!narrow, |bar| bar.child(label(format!("{zoom_pct}%"))))
            .child(fit_button)
            .child(new_button)
            .child(agent_button)
            .child(note_button)
            // A phone-wide bar has no room for both: the palette lists "Add a window".
            .when(!narrow, |bar| bar.child(window_button))
            .child(commands_button)
    }
}

impl Workspace {
    /// The host switcher: one row per host (status dot, name, state, "forget"), then
    /// "Add host…". Anchored under the host name; a click anywhere else closes it.
    fn switcher(&self, safe_top: gpui::Pixels, cx: &Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let (spacing, radii) = (theme.spacing, theme.radii);
        let ui = theme.typography.ui_size;
        let small = theme.typography.small();
        let mut panel = div()
            .id("host-switcher-panel")
            .occlude()
            .w(px(320.0))
            .max_w_full()
            .flex()
            .flex_col()
            .py(px(spacing.xs))
            .rounded(px(radii.md))
            .bg(hsla(s.panel))
            .border_1()
            .border_color(hsla(s.border))
            .shadow_sm()
            .font_family(self.theme.typography.ui_family.clone())
            .on_mouse_down(gpui::MouseButton::Left, |_ev, _window, cx| cx.stop_propagation());
        for host in &self.hosts {
            let id = host.id;
            let on_show = self.active == Some(id);
            let color = status_color(&self.theme, &host.status);
            let row = div()
                .id(SharedString::from(format!("host-{id}")))
                .role(Role::Button)
                .aria_label(SharedString::from(format!("{}, {}", host.name, host.status.text())))
                .flex()
                .items_center()
                .gap(px(spacing.sm))
                .px(px(spacing.md))
                .py(px(spacing.sm))
                .cursor_pointer()
                .hover(move |el| el.bg(hsla(s.raised)))
                .active(move |el| el.bg(hsla(s.overlay)))
                .when(on_show, |el| el.bg(hsla_alpha(s.accent, alpha::FAINT)))
                .child(div().flex_none().size(px(spacing.sm)).rounded_full().bg(hsla(color)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_size(px(ui))
                                .text_color(hsla(s.text))
                                .child(SharedString::from(host.name.clone())),
                        )
                        .child(
                            div()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_size(px(small))
                                .text_color(hsla(s.text_muted))
                                .child(SharedString::from(host.status.text())),
                        ),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("forget-{id}")))
                        .role(Role::Button)
                        .aria_label(SharedString::from(format!("Forget {}", host.name)))
                        .flex_none()
                        .px(px(spacing.xs))
                        .py(px(spacing.xxs))
                        .rounded(px(radii.xs))
                        .text_size(px(small))
                        .text_color(hsla(s.text_muted))
                        .hover(move |el| el.text_color(hsla(s.error)))
                        .child("forget")
                        .on_click(cx.listener(move |this, _ev, window, cx| {
                            cx.stop_propagation();
                            this.forget_host(id, window, cx);
                        })),
                )
                .on_click(cx.listener(move |this, _ev, window, cx| this.activate(id, window, cx)));
            panel = panel.child(tab_stop(row, s.accent));
        }
        let add_host = div()
            .id("add-host")
            .role(Role::Button)
            .aria_label("Add host")
            .px(px(spacing.md))
            .py(px(spacing.sm))
            .text_size(px(ui))
            .text_color(hsla(s.accent))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla(s.raised)))
            .child(SharedString::from(if SHORTCUT_HINTS {
                "Add host…  ⌘⇧H".to_owned()
            } else {
                "Add host…".to_owned()
            }));
        panel = panel.child(div().h(px(1.0)).my(px(spacing.xs)).bg(hsla(s.border))).child(
            tab_stop(add_host, s.accent).on_click(cx.listener(|this, _ev, window, cx| {
                this.switcher = false;
                this.show_pairing(window, cx);
            })),
        );
        div()
            .id("host-switcher-backdrop")
            .absolute()
            .inset_0()
            .pt(px(TOP_BAR) + safe_top)
            .pl(px(LEADING_INSET - spacing.sm))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _ev, _window, cx| {
                    this.switcher = false;
                    cx.notify();
                }),
            )
            .child(panel)
    }
}

/// A key-bar key as a screen reader names it; an armed modifier says so.
fn key_label(label: &str, lit: bool) -> String {
    let name = key_name(label);
    if lit { format!("{name}, armed") } else { name.to_owned() }
}

/// The dot colour for a host's link state.
const fn status_color(theme: &Theme, status: &HostStatus) -> slopty_theme::Rgb {
    match status {
        HostStatus::Connected => theme.surfaces.success,
        HostStatus::Connecting | HostStatus::Reconnecting(_) => theme.surfaces.warn,
        HostStatus::NeedsPairing => theme.surfaces.error,
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The frame's draw starts here; the probe element at the end of the tree closes it.
        slopty_ui::frames::begin(cx);
        let surfaces = &self.theme.surfaces;
        // Notch / Dynamic Island, home indicator and the soft keyboard on iOS; zero on macOS.
        let insets = window.insets().effective();
        // A phone in portrait; the bar drops its readouts so every button stays reachable.
        let narrow = window.viewport_size().width < px(NARROW_BAR);
        let canvas = self.active_canvas();
        let body = match (canvas, &self.pairing) {
            (_, Some(pairing)) => div()
                .flex_1()
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .p(px(self.theme.spacing.lg))
                .child(self.pairing_panel(pairing, cx)),
            (Some(canvas), None) => div().flex_1().w_full().child(canvas),
            (None, None) => div()
                .flex_1()
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(self.theme.typography.ui_size))
                .text_color(hsla(surfaces.text_muted))
                .child(SharedString::from(self.status_text())),
        };
        let key_bar = key_bar_visible(KEY_BAR, self.hardware_keyboard)
            .then(|| self.active_canvas()?.read(cx).active_key_target())
            .flatten()
            .map(|target| self.key_bar(&target, cx));
        if std::mem::take(&mut self.pending_focus_editor)
            && let Some(editor) = self.settings_editor.clone()
        {
            editor.update(cx, |e, cx| e.focus(window, cx));
        }
        let settings_editor = self.settings_editor.clone();
        let switcher = self.switcher.then(|| self.switcher(insets.top, cx));
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(hsla(surfaces.canvas))
            .on_action(cx.listener(|this, _: &NextHost, window, cx| this.step_host(1, window, cx)))
            .on_action(cx.listener(|this, _: &PrevHost, window, cx| this.step_host(-1, window, cx)))
            .on_action(cx.listener(|this, _: &AddHost, window, cx| this.show_pairing(window, cx)))
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ForgetHost, window, cx| {
                if let Some(id) = this.active {
                    this.forget_host(id, window, cx);
                }
            }))
            .child(self.top_bar(insets.top, narrow, cx))
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .flex()
                    .flex_col()
                    .pl(insets.left)
                    .pr(insets.right)
                    .pb(insets.bottom)
                    .child(body)
                    .when_some(key_bar, gpui::ParentElement::child),
            )
            .when_some(switcher, gpui::ParentElement::child)
            .when_some(settings_editor, gpui::ParentElement::child)
            .child(slopty_ui::frames::probe())
    }
}

/// Apply one event from a host link to the workspace; `true` when the link is gone.
fn apply_link_event(
    this: &WeakEntity<Workspace>,
    canvas: &Entity<CanvasView>,
    id: EndpointId,
    first_snapshot: &mut bool,
    event: LinkEvent,
    cx: &mut App,
) -> bool {
    tracing::trace!(?event, "link event");
    match event {
        LinkEvent::Term { session, event } => {
            canvas.update(cx, |c, cx| c.term_event(session, event, cx));
        }
        LinkEvent::Control(HostMsg::Canvas(sync)) => {
            let is_snapshot = matches!(sync, slopty_proto::canvas::CanvasSync::Snapshot { .. });
            canvas.update(cx, |c, cx| c.apply_sync(sync, cx));
            if is_snapshot && *first_snapshot {
                *first_snapshot = false;
                // First run: an empty canvas gets one shell so there is
                // something to type into. Otherwise bring the existing
                // layout into view.
                let window = this.update(cx, |ws, _cx| ws.window).ok().flatten();
                if let Some(window) = window {
                    let _opened = window.update(cx, |_root, window, cx| {
                        canvas.update(cx, |c, cx| {
                            if c.is_empty() {
                                c.new_terminal(&NewTerminal, window, cx);
                            } else {
                                c.fit_when_painted();
                            }
                        });
                    });
                }
            }
        }
        LinkEvent::Control(HostMsg::SessionOpened(summary)) => {
            canvas.update(cx, |c, cx| c.session_opened(summary, cx));
        }
        LinkEvent::Control(HostMsg::SessionClosed { session, .. }) => {
            canvas.update(cx, |c, cx| c.session_closed(session, cx));
        }
        LinkEvent::Control(HostMsg::Term { session, event }) => {
            canvas.update(cx, |c, cx| c.term_event(session, event, cx));
        }
        LinkEvent::Control(HostMsg::Screen(event)) => {
            canvas.update(cx, |c, cx| c.screen_event(event, cx));
        }
        LinkEvent::Control(HostMsg::Agent(event)) => {
            canvas.update(cx, |c, cx| c.agent_event(event, cx));
        }
        LinkEvent::Control(HostMsg::File { path, read }) => {
            canvas.update(cx, |c, cx| c.file_read(&path, &read, cx));
        }
        LinkEvent::Control(HostMsg::FoundFiles { root, query, paths }) => {
            canvas.update(cx, |c, cx| c.files_found(&root, &query, &paths, cx));
        }
        LinkEvent::Control(HostMsg::HooksInstalled { ok, message }) => {
            let _shown = this.update(cx, |ws, cx| ws.show_notice(message, cx));
            if !ok {
                // The offer stays on the title bar so it can be tried again.
                canvas.update(cx, CanvasView::hooks_offer_failed);
            }
        }
        LinkEvent::Control(_) => {}
        LinkEvent::Disconnected(why) => {
            let _set = this.update(cx, |ws, cx| {
                if let Some(slot) = ws.slot_mut(id) {
                    let place = slot.canvas.as_ref().map(|c| {
                        let c = c.read(cx);
                        (c.camera(), c.active_item())
                    });
                    let status = HostStatus::Reconnecting(format!("disconnected: {why}"));
                    slot.disconnect(status, place);
                }
                ws.refresh_badge();
                cx.notify();
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
        gpui::KeyBinding::new("cmd-alt-right", NextHost, None),
        gpui::KeyBinding::new("cmd-alt-left", PrevHost, None),
        gpui::KeyBinding::new("cmd-shift-h", AddHost, None),
    ]
}

/// The app's lines for the command palette, after the canvas's.
fn app_palette_items() -> Vec<slopty_ui::palette::PaletteItem> {
    let bindings = app_key_bindings();
    let item = |label: &str, action: Box<dyn gpui::Action>| {
        slopty_ui::palette::PaletteItem::new(label, action, &bindings)
    };
    vec![
        item("Open settings", Box::new(OpenSettings)),
        item("Next host", Box::new(NextHost)),
        item("Previous host", Box::new(PrevHost)),
        item("Add a host", Box::new(AddHost)),
    ]
}

/// Open the workspace window and start the host link loop on `handle`'s runtime. Call once
/// from inside the GPUI application callback, after `gpui_kit::init`.
///
/// # Errors
///
/// When the window cannot be opened.
pub fn open_workspace(
    cx: &mut App,
    handle: tokio::runtime::Handle,
    options: WindowOptions,
) -> anyhow::Result<()> {
    // VideoToolbox's first decoder session costs 150–400 ms; pay it before any host is dialed.
    slopty_client::warm_up_decoder();
    if let Err(e) = slopty_ui::fonts::install(cx) {
        tracing::error!(error = %e, "bundled fonts");
    }
    cx.bind_keys(slopty_ui::canvas::key_bindings());
    cx.bind_keys(slopty_ui::terminal::key_bindings());
    cx.bind_keys(app_key_bindings());
    // gpui-kit widgets follow their own theme; put it on the tokens now, and again once the
    // window's appearance is known, below.
    kit::sync(&Theme::default(), cx);
    slopty_ui::frames::install(cx, frame_nominal());
    let settings_path = slopty_settings::path();
    let loaded = Settings::load(&settings_path);
    let workspace = cx.new(|_cx| Workspace {
        hosts: Vec::new(),
        active: None,
        switcher: false,
        hardware_keyboard: hardware_keyboard_attached(),
        theme: Theme::default(),
        settings: Settings::default(),
        window_dark: true,
        notice: None,
        notice_seq: 0,
        subscriptions: Vec::new(),
        pairing: None,
        runtime: handle,
        window: None,
        settings_path: settings_path.clone(),
        settings_editor: None,
        pending_focus_editor: false,
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
    // A tap on an agent banner brings the app and that session forward, on whichever host
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
    // Every paired host gets a link now; with none, the pairing panel.
    let known = match net::known_hosts() {
        Ok(known) => known,
        Err(e) => {
            tracing::error!(error = %e, "pairing store");
            Vec::new()
        }
    };
    window.update(cx, |_root, window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.window = Some(window.window_handle());
            for (id, host) in known {
                ws.add_host(id, host.name, cx);
            }
            if ws.hosts.is_empty() {
                ws.show_pairing(window, cx);
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

/// Backoff between connection attempts: 1 s after a drop, doubling per failure, capped.
fn retry_delay(failures: u32) -> std::time::Duration {
    std::time::Duration::from_secs((1_u64 << failures.min(4)).min(10))
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
