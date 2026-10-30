//! The Slopty app shell, shared by the macOS and iOS apps.
//!
//! Connects to every worker its server lists at once and keeps their links, threads,
//! terminals and screens in one workspace, which feeds the shell's window (`slopty-shell`).
//! Networking runs on a tokio runtime thread; GPUI owns the main thread. The two talk through
//! channels only. The platform binaries set up logging, the runtime and the GPUI application,
//! then call [`open_workspace`].

#![forbid(unsafe_code)]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

mod add_device;
mod devices;
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
mod interface;
mod invite;
pub mod net;
mod presence;
#[cfg(any(target_os = "ios", test))]
mod push;
mod server;
pub mod settings;
pub mod ssh;
pub mod this_mac;
pub mod workers;

use std::rc::Rc;

use gpui::{App, AppContext as _, Context, Entity, WeakEntity};
pub use invite::open_link;
use slopty_client::{LinkEvent, WorkerKey};
use slopty_core::{SessionId, WorkerId};
use slopty_platform::notify::Notifier;
use slopty_proto::WorkerMsg;
use slopty_proto::terminal::{Progress, ProgressState};
use slopty_settings::{Loaded, Settings};
use slopty_terminal::TerminalViewEvent;
use slopty_theme::Theme;
use slopty_ui::workspace::attention::Attention;
use slopty_ui::workspace::{Finished, WorkerLink, WorkerStatus, WorkspaceEvent, WorkspaceView};
use workers::{Supervisor, Tick, WorkerSlot};

/// What a forgotten worker's confirmation says, as a success toast.
fn forgot(name: &str) -> String {
    format!("Forgot {name}")
}

/// Link events applied per foreground turn at most (see the link loop).
const LINK_BATCH: usize = 256;

/// How long a transfer's toast may stand: as long as any transfer takes.
const TRANSFER_LASTS: std::time::Duration = std::time::Duration::from_hours(24);

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
    /// Every worker's items, links, threads, terminals and screens.
    view: Entity<WorkspaceView>,
    /// The system asks for motion to be reduced (polled with the settings).
    reduce_motion: bool,
    /// Each transfer in flight, standing as a toast with its stop.
    transfer_toasts: std::collections::HashMap<slopty_core::XferId, u64>,
    theme: Theme,
    /// The user's `settings.toml` as last loaded (defaults when absent or broken).
    settings: Settings,
    /// The workspace view's events and changes, heard for as long as the app runs.
    _subscriptions: Vec<gpui::Subscription>,
    /// The main window's watches (its appearance, its activation, where it stands), made again
    /// with each window (`interface::open`).
    window_subscriptions: Vec<gpui::Subscription>,
    /// What the shell watches, for what the app itself tells it: the server it follows
    /// (`server::tell_server_presence`).
    interface_feed: Option<std::sync::Arc<slopty_ui::view_model::Feed>>,
    /// The system's Reduce Motion heard as it changes ([`watch_reduce_motion`]).
    motion_watch: Option<slopty_platform::motion::Watch>,
    /// The system's wakes, unlocks, returns to the front and path changes ([`watch_resumes`]).
    resume_watch: Option<slopty_platform::resume::Watch>,
    /// The person leaving this Mac: its lock, its sleep, another session
    /// (`Workspace::watch_leaving`).
    #[cfg(target_os = "macos")]
    leaving_watch: Option<slopty_platform::away::Watch>,
    /// Networking runtime; connects run there.
    runtime: tokio::runtime::Handle,
    /// The window, for focusing from a task or a banner.
    window: Option<gpui::AnyWindowHandle>,
    /// Where `settings.toml` lives (the data directory's).
    settings_path: std::path::PathBuf,
    /// Its stamp as last loaded or written here, for the watcher.
    settings_seen: settings::Seen,
    /// What "Use this Mac" does to this machine; `None` where it is not offered.
    this_mac: Option<Rc<dyn this_mac::Host>>,
    /// This Mac's worker, as its `doctor` last said: once the server lists it, "Add a
    /// machine"'s row for this Mac says so.
    this_mac_worker: Option<WorkerId>,
    /// This Mac's worker's `doctor` has answered, so [`Self::this_mac_worker`] says.
    this_mac_asked: bool,
    /// Wakes Add device's This Mac run when the app comes back to the front.
    this_mac_wake: Option<tokio::sync::mpsc::UnboundedSender<()>>,
    /// What installs and updates a worker over SSH; `None` where it is not offered.
    deployer: Option<Rc<dyn ssh::Deployer>>,
    /// Updates of machines on a different build, by host.
    updates: ssh::Updating,
    /// The machines being removed, by worker ([`Self::remove_worker`]).
    removals: ssh::Removing,
    /// The machine whose Remove was pressed once: pressed again, it goes.
    confirm_remove: Option<WorkerId>,
    /// This Mac's name on the tailnet, as a code for a server on this Mac carries it
    /// (`invite`); looked up as such a server is set.
    tailnet_name: Option<String>,
    /// This Mac's workers brought to this build unasked this launch: once each.
    updated_unasked: std::collections::HashSet<WorkerId>,
    /// The machines "Update all" found away: each is updated once it answers again on an
    /// older build ([`Workspace::update_all_workers`]).
    update_when_back: std::collections::HashSet<WorkerId>,
    /// "Update all" updates the server first, and the workers once its run has ended.
    workers_after_server: bool,
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
    /// stay up for the transfers under way when the phone is pocketed.
    #[cfg(target_os = "ios")]
    grace: Option<slopty_platform::notify::BackgroundGrace>,
    /// What the server may push to while the phone is pocketed, and when it stops listening.
    #[cfg(target_os = "ios")]
    pushing: push::Pushing,
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
        // The notice of each page held back, by its worker and handoff, so one taken back goes.
        let mut offered: std::collections::HashMap<
            (WorkerKey, slopty_proto::handoff::HandoffId),
            u64,
        > = std::collections::HashMap::new();
        let events = cx.subscribe(&view, move |ws: &mut Self, _view, event, cx| match event {
            // The badge is the inbox's count, which `Self::look` follows.
            WorkspaceEvent::NeedsYou(_) => {}
            // The Dock bounces while the human is elsewhere; the shell's "Input required" chime
            // is its sound. Led by a server, its notice bounces it instead, so a moment never
            // says it twice.
            WorkspaceEvent::Attention(_session) => {
                let active = cx.active_window().is_some();
                if !ws.attention.server_led() && settings::alerts(&ws.settings, active) {
                    bounce();
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
            WorkspaceEvent::ClipboardShared { worker, share } => {
                ws.keep_clipboard_shared(*worker, *share, cx);
            }
            WorkspaceEvent::AddDevice => ws.add_device(cx),
            WorkspaceEvent::LookOnTailnet => ws.look_on_tailnet_for_devices(cx),
            WorkspaceEvent::DeviceAction { worker, action } => {
                ws.device_action(*worker, action, cx);
            }
            WorkspaceEvent::Notice { text, failure: false } => {
                slopty_shell::toast::info(cx, text.clone());
            }
            WorkspaceEvent::Notice { text, failure: true } => {
                slopty_shell::toast::error(cx, text.clone());
            }
            WorkspaceEvent::Offer { worker, id, text, url, loopback } => {
                let (url, key, loopback) = (url.clone(), *worker, *loopback);
                let view = ws.view.downgrade();
                let open = move |cx: &mut App| {
                    if !loopback {
                        cx.open_url(&url);
                    } else if let Some(view) = view.upgrade() {
                        view.update(cx, |v, cx| v.open_loopback_page(key, &url, cx));
                    }
                };
                let lasts = slopty_ui::workspace::OFFER_FOR;
                if let Some(toast) =
                    slopty_shell::toast::offer(cx, text.clone(), "Open", lasts, open)
                {
                    offered.retain(|_, shown| slopty_shell::toast::showing(cx, *shown));
                    offered.insert((*worker, *id), toast);
                }
            }
            WorkspaceEvent::OpenFile { chat, target } => {
                let (chat, target) = (chat.clone(), target.clone());
                // Once the press that asked has been dispatched: the shell opens it in the window.
                cx.defer(move |cx| {
                    if !slopty_shell::live::open_file(&chat, &target, cx) {
                        slopty_shell::toast::error(cx, "Couldn't open the file");
                    }
                });
            }
            WorkspaceEvent::OpenLink { chat, url } => {
                let (chat, url) = (chat.clone(), url.clone());
                // As a file: once the keys that chose it have been dispatched. With no window
                // up to take it, the person's browser does.
                cx.defer(move |cx| {
                    if !slopty_shell::live::open_link(&chat, &url, cx) {
                        cx.open_url(&url);
                    }
                });
            }
            WorkspaceEvent::Withdrawn { worker, id } => {
                if let Some(toast) = offered.remove(&(*worker, *id)) {
                    slopty_shell::toast::withdraw(cx, toast);
                }
            }
        });
        let changes = cx.observe(&view, |ws, _view, cx| {
            ws.look(cx);
            ws.presence_changed(cx);
            ws.show_transfers(cx);
        });
        let this_mac = this_mac::native(&runtime);
        let dial = network_dialer(runtime.clone());
        let deployer = ssh::native(&runtime);
        #[cfg_attr(
            not(target_os = "macos"),
            expect(unused_mut, reason = "only a Mac watches the person leave")
        )]
        let mut this = Self {
            workers: Vec::new(),
            directory: slopty_client::directory::Directory::default(),
            server: None,
            server_generation: 0,
            directory_cache,
            view,
            reduce_motion: slopty_platform::reduce_motion(),
            transfer_toasts: std::collections::HashMap::new(),
            theme: Theme::default(),
            settings: Settings::default(),
            _subscriptions: vec![events, changes],
            window_subscriptions: Vec::new(),
            interface_feed: None,
            motion_watch: None,
            resume_watch: None,
            #[cfg(target_os = "macos")]
            leaving_watch: None,
            runtime,
            window: None,
            settings_path,
            settings_seen,
            this_mac,
            this_mac_worker: None,
            this_mac_asked: false,
            this_mac_wake: None,
            deployer,
            updates: ssh::Updating::new(),
            removals: ssh::Removing::new(),
            confirm_remove: None,
            tailnet_name: None,
            updated_unasked: std::collections::HashSet::new(),
            update_when_back: std::collections::HashSet::new(),
            workers_after_server: false,
            server_update: None,
            attention: Attention::new(Rc::new(slopty_platform::notify::Memory::default())),
            heard_terminals: std::collections::HashMap::new(),
            dock_progress: None,
            presenting: presence::Presenting::default(),
            dial,
            #[cfg(target_os = "ios")]
            grace: None,
            #[cfg(target_os = "ios")]
            pushing: push::Pushing::default(),
        };
        this.publish_device_actions(cx);
        this.repoint_this_mac(cx);
        this.ask_this_mac_worker(cx);
        Self::watch_presence(cx);
        #[cfg(target_os = "macos")]
        this.watch_leaving(cx);
        #[cfg(target_os = "ios")]
        Self::watch_push_tokens(cx);
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
                        let done = Finished {
                            command: command.clone(),
                            exit: *exit,
                            elapsed: *elapsed,
                            turn: None,
                        };
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
    /// while the app is away, under its terminal's name.
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
    /// notifications are off when a note went unsaid meanwhile. On iOS, leaving stops the
    /// link's notices at once and holds the background grace for the transfers under way;
    /// either way the server hears again what it may push to.
    fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        // Notes post only while away, and their switch (Settings → Notifications) is changed
        // only in front: leaving is when it is read.
        if !active {
            self.attention.set_enabled(slopty_shell::settings::current(cx).notifications_enabled);
        }
        self.attention.set_active(active);
        if active && self.attention.unsaid_while_off() {
            Self::show_notice(slopty_ui::workspace::attention::NOTES_OFF.to_owned(), cx);
        }
        // Back in front, the notes a push put up while away are the system's to list.
        if active {
            cx.spawn(async move |this, cx| {
                let shown = slopty_platform::notify::delivered().await;
                let ids: Vec<String> = shown.into_iter().map(|d| d.id).collect();
                let _gone = this.update(cx, |ws, _cx| ws.attention.delivered(&ids));
            })
            .detach();
        }
        #[cfg(target_os = "ios")]
        {
            self.grace = if active {
                None
            } else {
                slopty_platform::notify::BackgroundGrace::begin("Slopty keeps its links")
            };
            self.set_listening(active);
            self.tell_phone(cx);
            if active && self.server_caller().is_some() {
                Self::look_at_notes(cx);
            }
        }
    }

    /// The system's Reduce Motion setting changed. What moves reads it as it is drawn, and a
    /// view drawn from the last frame read it then: every window is drawn again from scratch.
    /// GPUI's own animations (gpui-kit's among them) follow GPUI's flag, which is set with it.
    fn set_reduce_motion(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.reduce_motion != on {
            tracing::info!(on, "reduce motion");
            self.reduce_motion = on;
            cx.set_reduce_motion(on);
            cx.refresh_windows();
        }
    }

    /// Take a (re)loaded settings file: log what was odd about it, say it, and rebuild the
    /// theme.
    ///
    /// A file that does not parse changes nothing: the settings last applied stay (the
    /// defaults, on a launch).
    fn apply_loaded(&mut self, loaded: Loaded, cx: &mut Context<Self>) {
        if let Some(error) = &loaded.error {
            tracing::error!(%error, "settings ignored");
            Self::show_failure(
                format!("Couldn't read settings.toml, so nothing changed: {error}"),
                cx,
            );
            self.rebuild_theme(cx);
            return;
        }
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
            Self::show_notice(text, cx);
        }
        let server = loaded.settings.network.server.clone();
        let sharing = loaded.settings.clipboard.clone();
        let keys = loaded.settings.keys.clone();
        let (_bound, wrong) =
            slopty_terminal::bindings(|name| keys.get("terminal", name).map(|c| c.0.clone()), cx);
        if let Some(first) = wrong.first() {
            Self::show_notice(format!("Settings: {first}"), cx);
        }
        let keys_moved = self.settings.keys != keys;
        self.settings = loaded.settings;
        if keys_moved {
            bind_terminal_keys(&cx.entity(), cx);
        }
        self.view.update(cx, |v, cx| v.set_clipboard_sharing(sharing, cx));
        self.rebuild_theme(cx);
        self.set_server(server, None, cx);
    }

    /// Derive the theme from the shell's look and the settings, and push it everywhere.
    fn rebuild_theme(&mut self, cx: &mut Context<Self>) {
        let theme = interface::theme(&self.settings, cx);
        if theme == self.theme {
            return;
        }
        self.view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        self.theme = theme;
        cx.refresh_windows();
        cx.notify();
    }

    /// Each file going up to a worker or coming down from one stands as a toast while it goes,
    /// with a Stop that ends it; the toast goes when the transfer does. Its end, when it says
    /// anything, is the workspace's notice.
    fn show_transfers(&mut self, cx: &mut Context<Self>) {
        let rows = self.view.read(cx).transfer_rows(std::time::Instant::now());
        let going: std::collections::HashSet<_> = rows.iter().map(|row| row.xfer).collect();
        self.transfer_toasts.retain(|xfer, toast| {
            let goes = going.contains(xfer);
            if !goes {
                slopty_shell::toast::withdraw(cx, *toast);
            }
            goes
        });
        for row in rows {
            let text = if row.up {
                format!("Sending {} to {}", row.name, row.machine)
            } else {
                format!("Bringing {} from {}", row.name, row.machine)
            };
            // How far it is, moved in place on the toast already up.
            let text =
                if row.words.is_empty() { text } else { format!("{text} \u{b7} {}", row.words) };
            if let Some(&toast) = self.transfer_toasts.get(&row.xfer) {
                slopty_shell::toast::update(cx, toast, text, row.fraction);
                continue;
            }
            let (view, xfer) = (self.view.downgrade(), row.xfer);
            let stop = move |cx: &mut App| {
                let _gone = view.update(cx, |v, cx| v.cancel_transfer(xfer, cx));
            };
            if let Some(toast) = slopty_shell::toast::offer_progress(
                cx,
                text,
                "Stop",
                TRANSFER_LASTS,
                row.fraction,
                stop,
            ) {
                self.transfer_toasts.insert(xfer, toast);
            }
        }
    }

    /// A word for the human, as a toast.
    fn show_notice(text: String, cx: &mut Context<Self>) {
        tracing::info!(%text, "notice");
        slopty_shell::toast::info(cx, text);
    }

    /// Something that failed, as a toast.
    fn show_failure(text: String, cx: &mut Context<Self>) {
        tracing::warn!(%text, "failure");
        slopty_shell::toast::error(cx, text);
    }

    fn slot(&self, id: WorkerId) -> Option<&WorkerSlot> {
        self.workers.iter().find(|w| w.id == id)
    }

    fn slot_mut(&mut self, id: WorkerId) -> Option<&mut WorkerSlot> {
        self.workers.iter_mut().find(|w| w.id == id)
    }

    /// Start (or refresh) a worker the directory lists: its items wait in the workspace and a
    /// connect loop of its own brings them to life.
    fn add_worker(&mut self, id: WorkerId, name: String, cx: &mut Context<Self>) {
        // Where it is dialled, which an editor's SSH remote names too.
        let address = self.directory.get(id).map(|w| w.address.clone());
        if let Some(slot) = self.slot_mut(id) {
            slot.name.clone_from(&name);
            let key = slot.key;
            self.view.update(cx, |v, cx| {
                v.add_worker(key, name, cx);
                if let Some(address) = &address {
                    v.set_worker_address(key, address, cx);
                }
            });
            return;
        }
        let slot = WorkerSlot::new(id, name.clone());
        let key = slot.key;
        self.workers.push(slot);
        self.view.update(cx, |v, cx| {
            v.add_worker(key, name, cx);
            if let Some(address) = &address {
                v.set_worker_address(key, address, cx);
            }
        });
        self.spawn_worker_loop(id, cx);
    }

    /// Ask the server to forget a worker that is not online (`Verb::ForgetWorker`; it refuses
    /// one that is). It leaves when the directory unlists it, its pages with it.
    fn forget_worker(&self, id: WorkerId, cx: &mut Context<Self>) {
        let name = self.directory.get(id).map_or_else(|| id.to_string(), |w| w.name.clone());
        let Some(caller) = self.server_caller() else {
            Self::show_failure(
                format!("Couldn't forget {name}: the server does not answer yet"),
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
            let _gone = this.update(cx, |_, cx| match outcome {
                slopty_proto::orchestration::Outcome::Error { message, .. } => {
                    Self::show_failure(format!("Couldn't forget {name}: {message}"), cx);
                }
                _forgotten => slopty_shell::toast::success(cx, forgot(&name)),
            });
        })
        .detach();
    }

    /// A forgotten worker's pages lose their cookies and storage with the items that held them,
    /// which go as this change is handled: `WebKit` deletes the store once it lets go of their
    /// last page (`slopty_platform::web::forget`).
    fn forget_pages(key: WorkerKey) {
        slopty_platform::web::forget(key.value(), |gone| {
            if let Err(why) = gone {
                tracing::warn!(%why, "a forgotten worker's pages kept their store");
            }
        });
    }

    /// Drop a worker's slot: its link, and its items.
    fn drop_slot(&mut self, id: WorkerId, cx: &mut Context<Self>) {
        let Some(at) = self.workers.iter().position(|w| w.id == id) else { return };
        let slot = self.workers.remove(at);
        if let Some(link) = slot.link.as_ref().and_then(std::sync::Weak::upgrade) {
            link.abandon("worker dropped");
        }
        slot.wake();
        self.view.update(cx, |v, cx| v.remove_worker(slot.key, cx));
        cx.notify();
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
            // A link a probe found dead after a resume, whose views still show what they showed,
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
                        if wait_or_wake(cx, &wake, delay).await {
                            // Something says to try now (a resume, the worker back or moved,
                            // an update): the schedule starts again from its first wait.
                            redial.reset();
                        }
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
                let build = ack.caps.build.clone();
                let sessions = ack.sessions.len();
                let (resume_tx, mut resumes) = tokio::sync::mpsc::unbounded_channel();
                let replaced = std::mem::take(&mut replacing);
                // The slot is checked and the items connected in one step: a worker forgotten
                // while the dial was in flight must not come back as an item holding this link.
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
                    ws.update_linked(id, cx);
                    ws.heard_build(id, &build, true, cx);
                    true
                });
                if !matches!(alive, Ok(true)) {
                    link.abandon("worker dropped");
                    break;
                }
                // The link's supervisor (`workers::Supervisor`), every half second: the RTT for
                // the bar and the predictors, a ping down the control stream every two seconds,
                // and the link judged by its pongs, not by the datagrams beside them, which flow
                // under a stream that has stopped. One ping unanswered shows the link silent; two
                // relink it at once, its views kept, set back, until the new link lands (`relink`).
                // A wake while the link is up is the server saying the worker is back or moved: a
                // link in doubt then is the dead one, relinked at once.
                // A resume (the Mac woke, the path moved) probes the link at once rather than
                // waiting for its silence: an answer keeps it, none relinks it the same way.
                let rtt_link = std::sync::Arc::downgrade(&link);
                let rtt_view = view.clone();
                let rtt_wake = std::sync::Arc::clone(&wake);
                let relink = std::sync::Arc::new(tokio::sync::Notify::new());
                let relink_asked = std::sync::Arc::clone(&relink);
                cx.spawn(async move |cx| {
                    let mut supervisor = Supervisor::new(std::time::Instant::now(), link_pongs(&rtt_link));
                    // The handshake has measured the path already: the predictors draw from the
                    // first key, not from the first look 500 ms on.
                    if let Some(link) = rtt_link.upgrade() {
                        let rtt = link.rtt();
                        rtt_view.update(cx, |v, cx| v.set_rtt(key, rtt, cx));
                    }
                    let mut listening = true;
                    loop {
                        let (woken, resumed) = tokio::select! {
                            woken = wait_or_wake(cx, &rtt_wake, slopty_net::liveness::LOOK_EVERY) => (woken, None),
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
                            // A pong that comes while the probe runs is the stream's own word.
                            let _queued = link.ping_stream();
                            if probe(cx, &link, resume, &rtt_view, key).await {
                                // Resumes that came while it ran are answered by this one.
                                while resumes.try_recv().is_ok() {}
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
                        let pongs = link.pongs();
                        tracing::trace!(worker = %id, path = %link.path(), pongs, stream_rtt = ?link.stream_rtt(), "link look");
                        let look = supervisor.look(pongs, woken, std::time::Instant::now());
                        if look.ping_stream && !link.ping_stream() {
                            tracing::debug!(worker = %id, "control stream queue full; ping skipped");
                        }
                        let status = match look.tick {
                            Tick::Show(status) => status,
                            Tick::Ping(status) => {
                                link.ping();
                                status
                            }
                            Tick::Relink => {
                                tracing::warn!(worker = %id, pongs, woken, path = %link.path(), "the worker's control stream stopped answering; relinking");
                                relink_asked.notify_one();
                                rtt_wake.notify_one();
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
                        cx.set_global(slopty_terminal::LinkArrival(arrived));
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
                    // Dialled again at once; its items stay, set back, until that lands.
                    link.abandon("its control stream stopped answering");
                    drop(link);
                    view.update(cx, |v, cx| v.set_worker_status(key, WorkerStatus::Relinking, cx));
                    continue;
                }
                drop(link);
                if wait_or_wake(cx, &wake, redial.next(std::time::Instant::now())).await {
                    redial.reset();
                }
            }
        })
        .detach();
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
                    cx.notify();
                }
                ws.this_mac_asked = true;
                ws.tell_local_device();
            });
        })
        .detach();
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
            let _gone = this.update(cx, |_, cx| Self::show_notice(said, cx));
        })
        .detach();
    }

    /// Keep the person's word on sharing the clipboard with `worker` in the settings file, by
    /// its worker id; what could not be written is said, and the workspace keeps it until the
    /// app quits.
    fn keep_clipboard_shared(&mut self, worker: WorkerKey, share: bool, cx: &mut Context<Self>) {
        let Some(id) = self.workers.iter().find(|w| w.key == worker).map(|w| w.id) else {
            return;
        };
        let text = settings::editable_text(&self.settings_path);
        let kept = settings::with_clipboard_shared(&text, id, share)
            .and_then(|text| settings::save(&self.settings_path, &text, &mut self.settings_seen));
        match kept {
            Ok(loaded) => self.settings = loaded.settings,
            Err(e) => Self::show_failure(
                format!("Couldn't save clipboard sharing to settings.toml: {e}"),
                cx,
            ),
        }
    }

    /// Write `[network] server` into `settings.toml`, the rest of the file untouched, and take
    /// it as loaded so the watcher sees nothing new.
    fn save_server(&mut self, server: Option<&slopty_net::HostAddr>) -> Result<(), String> {
        let text = settings::editable_text(&self.settings_path);
        let text = slopty_settings::with_server(&text, server)?;
        let loaded = settings::save(&self.settings_path, &text, &mut self.settings_seen)?;
        self.settings = loaded.settings;
        Ok(())
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

/// How often `link`'s control stream has answered, 0 for a link gone.
fn link_pongs(link: &std::sync::Weak<slopty_client::WorkerLink>) -> u64 {
    link.upgrade().map_or(0, |link| link.pongs())
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
/// from the worker, within the probe's deadline ([`workers::Probe`]). Its worker's views are
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
    // The executor's clock, which a test moves by hand.
    let started = cx.background_executor().now();
    link.ping();
    let mut doubted = false;
    loop {
        let waited = cx.background_executor().now().saturating_duration_since(started);
        let answered = link.received_datagrams() != before;
        match plan.step(waited, answered, doubted) {
            workers::ProbeStep::Answered => {
                if doubted {
                    view.update(cx, |v, cx| v.set_worker_status(key, WorkerStatus::Connected, cx));
                }
                tracing::info!(resume = resume.name(), ?waited, "link answered after a resume");
                return true;
            }
            workers::ProbeStep::GiveUp => return false,
            workers::ProbeStep::Doubt => {
                doubted = true;
                view.update(cx, |v, cx| v.set_worker_status(key, WorkerStatus::Checking, cx));
            }
            workers::ProbeStep::Wait => {}
        }
        cx.background_executor().timer(workers::PROBE_POLL).await;
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
    if let LinkEvent::Control(msg) = &event
        && view.update(cx, |v, _| v.surface_heard(key, msg))
    {
        return false;
    }
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
        LinkEvent::Control(WorkerMsg::SessionOpened { request, summary }) => {
            let session = summary.id;
            view.update(cx, |v, cx| {
                v.session_opened(key, summary, cx);
                v.live_session_opened(key, request, session, cx);
            });
        }
        LinkEvent::Control(WorkerMsg::SessionChanged(summary)) => {
            view.update(cx, |v, cx| v.session_opened(key, summary, cx));
        }
        LinkEvent::Control(WorkerMsg::Failed { request, message, .. }) => {
            view.update(cx, |v, cx| v.live_open_failed(key, request, &message, cx));
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
        LinkEvent::Handoff { event, received } => {
            view.update(cx, |v, cx| v.handoff_event(key, event, received, cx));
        }
        LinkEvent::XferFailed { xfer, error } => {
            view.update(cx, |v, cx| v.xfer_failed(xfer, &error.to_string(), cx));
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
        LinkEvent::Control(WorkerMsg::GitDone { request, outcome }) => {
            view.update(cx, |v, cx| v.git_done(key, request, outcome, cx));
        }
        // The handshake's ack was read when the link connected; the tick pings to draw a
        // restarted worker's reset, so the pong carries nothing; the app's link forwards
        // ports itself (`LinkEvent::Ports`); a handoff comes as `LinkEvent::Handoff`. A file,
        // folder, search or git answer nobody on the surfaces asked for and the old
        // workspace's thread news have no reader.
        LinkEvent::Control(
            WorkerMsg::HelloAck(_)
            | WorkerMsg::Pong { .. }
            | WorkerMsg::Ports { .. }
            | WorkerMsg::Handoff(_)
            | WorkerMsg::File { .. }
            | WorkerMsg::Written { .. }
            | WorkerMsg::FoundFiles { .. }
            | WorkerMsg::FoundFolders { .. }
            | WorkerMsg::RepoMoved { .. }
            | WorkerMsg::Folder { .. }
            | WorkerMsg::FolderPage { .. }
            | WorkerMsg::Search(_)
            | WorkerMsg::FsDone { .. }
            | WorkerMsg::RepoCloning { .. }
            | WorkerMsg::RepoCloned { .. }
            | WorkerMsg::Sessions(_)
            | WorkerMsg::SettingUp { .. },
        ) => {}
        LinkEvent::Disconnected(why) => {
            tracing::info!(worker = ?key, %why, "link ended");
            let status = WorkerStatus::Reconnecting(server::DROPPED.to_owned());
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
        .map_or_else(slopty_kit::frames::default_nominal, |hz| {
            std::time::Duration::from_secs_f64(1.0 / hz)
        })
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

/// The person's alert with no sound of its own, for a moment the shell already chimes: a bounce
/// of the Dock icon.
#[cfg(not(feature = "e2e"))]
fn bounce() {
    slopty_platform::bounce();
}

/// The e2e build only notes the bounce, as [`alert`].
#[cfg(feature = "e2e")]
fn bounce() {
    tracing::info!("bounce");
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

/// The Nerd symbol face a prompt's powerline and devicon glyphs fall to, registered beside the
/// shell's Geist faces before the window opens (`docs/decisions/ui.md`, "The symbol face is
/// registered at startup"). A refusal leaves those glyphs to the system's fallbacks, and says so.
fn install_symbol_face(cx: &App) {
    if let Err(error) = slopty_kit::fonts::install(cx) {
        tracing::warn!(%error, "the symbol face did not register; prompt glyphs fall to the system's");
    }
}

/// Open the app's window and start a link loop per added worker on `handle`'s runtime.
///
/// The window is the shell on the workspace (`interface`). Call once from inside the
/// GPUI application callback, after `gpui_kit::init`.
pub fn open_workspace(cx: &mut App, handle: tokio::runtime::Handle) {
    // The self-test's window keeps the size its test asks for on any screen, a CI Mac's short
    // one too (`slopty_platform::asked_size`); before the window is made, which AppKit fits.
    #[cfg(feature = "e2e")]
    if std::env::var_os(slopty_e2e::SOCKET_ENV).is_some() && cfg!(target_os = "macos") {
        let kept = slopty_platform::asked_size::keep_asked_sizes(c"GPUIWindow");
        tracing::info!(kept, "the self-test's windows keep the size they ask for");
    }
    // VideoToolbox's first decoder session costs 150–400 ms; pay it before any worker is
    // dialed.
    slopty_client::warm_up_decoder();
    // ⌘Q drops no view and no tap, so the hotkey mode a focused remote window pushed is popped
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
    slopty_kit::frames::install(cx, frame_nominal());
    install_symbol_face(cx);
    let settings_path = slopty_settings::path();
    let settings_seen = settings::Seen::of(&settings_path);
    let loaded = Settings::load(&settings_path);
    let view = cx.new(|cx| {
        let mut view = WorkspaceView::new(Theme::default());
        // The transfers in flight, taken up again at the next launch.
        view.set_transfer_ledger(
            slopty_platform::dirs::data_dir().join(slopty_client::xfer::ledger::FILE),
            cx,
        );
        // Each worker's threads, so an agent's thread draws in its first frame. Not `threads`:
        // that is this Mac's worker's own, which reads every folder in it as a thread's log.
        view.set_thread_cache(slopty_platform::dirs::data_dir().join("thread-cache"));
        view
    });
    let (directory_cache, cache_writes) = tokio::sync::watch::channel(server::Cache::Remove);
    // The self-test's workers are its own, never the system's.
    #[cfg(target_os = "macos")]
    if !self_test() {
        handle.spawn(finder::follow(cache_writes.clone()));
    }
    handle.spawn(server::write_cache(server::cache_path(), cache_writes));
    let runtime = handle.clone();
    let mut tapped = slopty_platform::notify::taps();
    let notifier = notifier();
    let workspace = cx.new(|cx| {
        let mut ws =
            Workspace::new(view, handle, settings_path, settings_seen, directory_cache, cx);
        ws.set_notifier(notifier);
        ws
    });
    bind_terminal_keys(&workspace, cx);
    let window = interface::open(&workspace, runtime, loaded, cx);
    // Every draw of the window, timed for the stream's overlay and the self-test's numbers.
    let _timed = window.update(cx, |_, window, cx| slopty_kit::frames::watch(window, cx).detach());
    // GPUI's own animations hold still as the system asks, as Slopty's do, and follow the
    // setting as the system says it changed ([`Workspace::set_reduce_motion`]).
    cx.set_reduce_motion(slopty_platform::reduce_motion());
    watch_reduce_motion(&workspace, cx);
    watch_resumes(&workspace, &slopty_platform::resume::System, cx);
    hangs::watch(cx);
    watch_settings(workspace.clone(), cx);
    // A tapped note brings the app forward on the session it is about: a note answers nothing
    // (TUI-first), the person answers in the session's TUI. The tap that launched the app
    // waited for `taps` above and arrives first, once the window is up.
    cx.spawn(async move |cx| {
        while let Some(tap) = tapped.recv().await {
            tracing::debug!(id = tap.id, "note opened");
            cx.update(|cx| {
                // The window comes back to show it, if it was closed.
                slopty_shell::activate_main_window(cx);
                if let Some(chat) = WorkspaceView::tapped_chat(&tap) {
                    slopty_shell::open_chat(chat, cx);
                }
            });
        }
    })
    .detach();
    // A link the system opened Slopty with ([`invite`]): the person's Camera read a code.
    let mut links = invite::links();
    let shell = window;
    cx.spawn(async move |cx| {
        while let Some(url) = links.recv().await {
            cx.update(|cx| invite::open_in(&url, shell, cx));
        }
    })
    .detach();
    // The self-test socket, for `cargo xtask e2e app`; never set for a normal launch.
    #[cfg(feature = "e2e")]
    if let Some(socket) = std::env::var_os(slopty_e2e::SOCKET_ENV) {
        // The window counts as looked at, in front or not: a guest's other apps decide which
        // window is in front, so a session open in it read as seen on one run and unread on
        // the next. A test of the OS's own word sets `SLOPTY_E2E_AWAY`.
        if std::env::var_os("SLOPTY_E2E_AWAY").is_none() {
            slopty_shell::presence::hold(cx);
        }
        let runtime = workspace.read(cx).runtime.clone();
        e2e::serve(socket.into(), workspace, window, &runtime, cx);
    }
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
/// `SLOPTY_PASTEBOARD` (the self-tests', so no test touches the human's clipboard). A named
/// one is GPUI's clipboard too, so every copy and paste in the app goes where the sharing
/// looks; called as the window is made, before anything is copied.
#[cfg(target_os = "macos")]
fn pasteboard() -> Rc<dyn slopty_platform::pasteboard::Pasteboard> {
    use slopty_platform::pasteboard::MacPasteboard;
    match std::env::var("SLOPTY_PASTEBOARD") {
        Ok(name) if !name.is_empty() => {
            tracing::info!(%name, "clipboard on a named pasteboard");
            gpui_platform::set_clipboard_pasteboard(&name);
            Rc::new(MacPasteboard::named(&name))
        }
        _ => Rc::new(MacPasteboard::general()),
    }
}

/// [`pasteboard`] on iOS: `UIPasteboard`, the app's own named one under the self-test, GPUI's
/// clipboard too. A self-test that names none still gets one of its own: the simulator's
/// general pasteboard follows the Mac's.
#[cfg(target_os = "ios")]
fn pasteboard() -> Rc<dyn slopty_platform::pasteboard::Pasteboard> {
    use slopty_platform::pasteboard::IosPasteboard;
    let named =
        std::env::var("SLOPTY_PASTEBOARD").ok().filter(|name| !name.is_empty()).or_else(|| {
            self_test().then(|| format!("com.aislopware.slopty.self-test.{}", std::process::id()))
        });
    if let Some(name) = named
        && let Some(board) = IosPasteboard::named(&name)
    {
        tracing::info!(%name, "clipboard on a named pasteboard");
        gpui_platform::set_clipboard_pasteboard(&name);
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
            #[cfg(target_os = "macos")]
            let heard = workspace.update(cx, |ws, cx| {
                ws.resume(resume);
                ws.person_back(resume, cx);
            });
            #[cfg(not(target_os = "macos"))]
            let heard = workspace.update(cx, |ws, _cx| ws.resume(resume));
            if heard.is_err() {
                return;
            }
        }
    })
    .detach();
}

/// Reload `settings.toml` whenever its stamp changes (see [`settings`] for why this polls).
/// The app's own saves are
/// already applied and seen ([`settings::save`]), so they are not reloaded.
/// The terminal's keys as the settings file's `[keys.terminal]` sets them, bound last in the
/// shell's keymap each time it is bound.
fn bind_terminal_keys(workspace: &Entity<Workspace>, cx: &mut App) {
    let workspace = workspace.downgrade();
    slopty_shell::live::keys(cx, move |cx| {
        let keys = workspace.upgrade().map(|ws| ws.read(cx).settings.keys.clone());
        let keys = keys.unwrap_or_default();
        let (bindings, _warned) =
            slopty_terminal::bindings(|name| keys.get("terminal", name).map(|c| c.0.clone()), cx);
        cx.bind_keys(bindings);
    });
}

fn watch_settings(workspace: Entity<Workspace>, cx: &App) {
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(settings::POLL).await;
            workspace.update(cx, |ws, cx| {
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

#[cfg(test)]
mod tests {
    use gpui::{IntoElement, Render, TestAppContext, VisualTestContext, Window};
    use slopty_proto::server::Liveness;

    use super::*;

    /// A window's root that draws nothing: the app's own state lives in the workspace.
    struct Blank;

    impl Render for Blank {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            gpui::Empty
        }
    }

    /// The workspace beside a headless window: with `set_up`, a server and one of its workers.
    /// The runtime and the directory hold what the workspace's tasks and settings file need for
    /// the test's length.
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
        let (_root, cx) = cx.add_window_view(|_, _| Blank);
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

    /// The shell with no window, no worker and no panel.
    pub(crate) fn workspace(
        cx: &mut TestAppContext,
        runtime: &tokio::runtime::Runtime,
        dir: &tempfile::TempDir,
    ) -> Entity<Workspace> {
        cx.update(gpui_kit::init);
        let path = dir.path().join("settings.toml");
        let (cache, _writes) = tokio::sync::watch::channel(server::Cache::Remove);
        let handle = runtime.handle().clone();
        let view = cx.new(|_| WorkspaceView::new(Theme::default()));
        let seen = settings::Seen::of(&path);
        cx.new(|cx| Workspace::new(view, handle, path, seen, cache, cx))
    }

    /// A worker the server forgot is said as done, with the check a confirmation wears, and a
    /// refusal as a failure; a block sent to a chat not open names the chat, not the shell.
    #[gpui::test]
    fn forgetting_a_worker_is_a_success_and_a_refusal_a_failure(cx: &mut TestAppContext) {
        use slopty_proto::orchestration::{ErrorCode, Outcome, Verb};
        use slopty_shell::toast::{self, ToastKind};
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = shell(cx, &runtime, &dir, false);
        let hub = slopty_net::HostAddr::new("hub", slopty_net::endpoint::SERVER_PORT);
        let (slot, mut queue) = server::ServerSlot::answered_by(hub);
        ws.update(cx, |ws, _cx| ws.server = Some(slot));
        let refused = Outcome::Error { code: ErrorCode::Conflict, message: "it is online".into() };
        for (outcome, kind) in [(Outcome::Done, ToastKind::Success), (refused, ToastKind::Error)] {
            let id = WorkerId::new();
            ws.update(cx, |ws, cx| ws.forget_worker(id, cx));
            let (verb, answer) = runtime.block_on(async {
                loop {
                    if let Some(next) = queue.try_next() {
                        break next;
                    }
                    tokio::task::yield_now().await;
                }
            });
            assert_eq!(verb, Verb::ForgetWorker { worker: id });
            let _sent = answer.send(outcome);
            runtime.block_on(async {
                for _ in 0..8 {
                    tokio::task::yield_now().await;
                }
            });
            cx.run_until_parked();
            let shown = cx.update(|_, cx| toast::shown(cx));
            let (said, words) = shown.last().expect("a toast");
            assert_eq!(*said, kind, "{words}");
            if kind == ToastKind::Success {
                assert_eq!(words.as_ref(), forgot(&id.to_string()));
            }
        }
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
    /// but a wake (a resume, or the server listing it online again) dials it at once, once,
    /// rather than starting the hold over.
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
        cx.executor().advance_clock(slopty_net::redial::MAX);
        cx.run_until_parked();
        assert_eq!(dials.get(), 2, "held again");
        list(&ws, cx, id, "box");
        assert_eq!(dials.get(), 3, "dialled at once when the server lists it online");
        ws.update(cx, |ws, cx| ws.drop_slot(id, cx));
        cx.run_until_parked();
    }
}
