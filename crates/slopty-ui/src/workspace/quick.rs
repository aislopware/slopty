//! The quick terminal as the workspace keeps it: one shell, a tile like any other, drawn in a
//! panel that a system-wide chord slides down from the top of the screen.
//!
//! The first time it is asked for, a shell opens on the worker of the shell used last, in that
//! shell's directory (else on the worker a new tile would go to), and the focus stays where it
//! was. Its item is the quick terminal from then on: its `TerminalView` is drawn in the
//! panel ([`crate::quick_terminal`]) and its tile says where it went, as a tile popped out into
//! a window of its own does. Showing and hiding move the panel, never the session, and nothing
//! reaches the worker. A shell that ended is put away with the panel; the next show opens a
//! new one. The Mac's only: iPadOS has no window over other apps
//! (`docs/decisions/ui.md`, "A quick terminal slides down from the top of the screen").

use std::time::Instant;

use gpui::{
    AppContext as _, Bounds, Context, SharedString, TitlebarOptions, WindowBackgroundAppearance,
    WindowBounds, WindowHandle, WindowKind, WindowOptions, px, size,
};
use slopty_client::layout::WorkerKey;
use slopty_core::{ItemId, SessionId};
use slopty_proto::items::{ItemKind, ItemOp};
use slopty_proto::terminal::SessionState;

use super::WorkspaceView;
pub use crate::quick_terminal::QuickConfig;
use crate::quick_terminal::{Body, QuickTerminalView};

/// The palette's name for [`super::ToggleQuickTerminal`].
pub const TOGGLE_QUICK_TERMINAL: &str = "Toggle quick terminal";
/// What the quick terminal's tile says while its shell is in the panel.
pub(crate) const IN_QUICK_TERMINAL: &str = "In the quick terminal";
/// The panel before a worker is there to open a shell on.
const NO_WORKER: &str = "No worker to open a shell on";
/// The panel's window, as the system lists it.
const WINDOW_TITLE: &str = "Quick terminal";

/// Where a toggle of the quick terminal comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QuickToggle {
    /// Its chord, from any app: a panel shown behind another app's window comes to the front,
    /// and one in front goes away.
    Chord,
    /// The palette's command, run in the workspace's window, which holds the keyboard then: a
    /// panel shown goes away.
    Command,
}

/// The quick terminal's shell and panel.
#[derive(Default)]
pub(super) struct Quick {
    /// Its item, once its shell arrived.
    item: Option<ItemId>,
    /// The worker a shell was asked of for it, until its item arrives.
    pending: Option<WorkerKey>,
    /// The panel, made the first time it is shown and kept.
    window: Option<WindowHandle<QuickTerminalView>>,
    config: QuickConfig,
}

impl std::fmt::Debug for Quick {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Quick")
            .field("item", &self.item)
            .field("pending", &self.pending)
            .field("open", &self.window.is_some())
            .field("config", &self.config)
            .finish()
    }
}

impl Quick {
    /// Whether `item` is the quick terminal's.
    pub(super) fn holds(&self, item: ItemId) -> bool {
        self.item == Some(item)
    }
}

impl WorkspaceView {
    /// Where the quick terminal sits and whether it hides on losing the keyboard.
    pub fn set_quick_terminal(&mut self, config: QuickConfig, cx: &mut Context<Self>) {
        if self.quick.config == config {
            return;
        }
        self.quick.config = config;
        self.refresh_quick(cx);
    }

    /// Whether the quick terminal is offered: on the Mac, whose panels float over other apps.
    #[must_use]
    pub const fn quick_terminal_offered() -> bool {
        cfg!(target_os = "macos")
    }

    /// The chord, or the palette's command (`from`): show the quick terminal, or put it away
    /// when it is shown. The chord puts it away only while it has the keyboard (or never takes
    /// it), and otherwise brings it to the front. `asked` is when it was asked for.
    pub fn toggle_quick_terminal(
        &mut self,
        asked: Instant,
        from: QuickToggle,
        cx: &mut Context<Self>,
    ) {
        let keyboard = self.quick.config.keyboard;
        let hide = self.quick.window.is_some_and(|handle| {
            handle
                .update(cx, |q, window, _cx| {
                    q.is_shown()
                        && (from == QuickToggle::Command || window.is_window_active() || !keyboard)
                })
                .unwrap_or(false)
        });
        if hide {
            self.hide_quick_terminal(cx);
        } else {
            self.show_quick_terminal(asked, cx);
        }
    }

    /// Slide the quick terminal in, opening its shell first when it has none.
    pub fn show_quick_terminal(&mut self, asked: Instant, cx: &mut Context<Self>) {
        if !Self::quick_terminal_offered() {
            return;
        }
        if self.quick_session().is_none() && self.quick.pending.is_none() {
            self.open_quick_shell(cx);
        }
        let Some(handle) = self.quick_window(cx) else { return };
        let body = self.quick_body();
        let _gone = handle.update(cx, |q, window, cx| {
            q.set_body(body, window, cx);
            q.show(asked, window, cx);
        });
    }

    /// Slide the quick terminal away; its shell stays as it is.
    pub fn hide_quick_terminal(&self, cx: &mut Context<Self>) {
        if let Some(handle) = self.quick.window {
            let _gone = handle.update(cx, |q, window, cx| q.hide(window, cx));
        }
    }

    /// The quick terminal's panel, when it has been made.
    #[must_use]
    pub fn quick_terminal_window(&self) -> Option<gpui::AnyWindowHandle> {
        self.quick.window.map(Into::into)
    }

    /// The quick terminal's live session: its item's, while the shell runs.
    fn quick_session(&self) -> Option<SessionId> {
        let tile = self.tile_of(self.quick.item?)?;
        let ItemKind::Terminal { session } = self.item(tile)?.kind else { return None };
        let running = self.summary(session).is_some_and(|s| s.state == SessionState::Running);
        running.then_some(session)
    }

    /// Ask for the quick terminal's shell: on the worker of the shell used last, in its
    /// directory, else where a new tile would go. The focus stays where it is.
    fn open_quick_shell(&mut self, cx: &mut Context<Self>) {
        let linked = |key: &WorkerKey| self.workers.get(key).is_some_and(super::Worker::is_linked);
        let last = self
            .run_target()
            .filter(|s| self.quick_session() != Some(*s))
            .and_then(|s| self.worker_of_session(s).filter(linked).map(|key| (key, s)))
            .map(|(key, s)| (key, self.summary(s).and_then(|s| s.cwd.clone())));
        let Some((key, cwd)) =
            last.or_else(|| self.context_worker().filter(linked).map(|k| (k, None)))
        else {
            return;
        };
        self.quick.pending = Some(key);
        self.given_pending.insert(key, self.focused());
        self.open_session_on(key, cwd, Vec::new(), None, cx);
    }

    /// An item this client asked for arrived on `key`: the quick terminal's shell, when one
    /// was asked of that worker for it.
    pub(super) fn quick_arrived(&mut self, key: WorkerKey, item: ItemId) {
        let terminal = self
            .workers
            .get(&key)
            .and_then(|w| w.doc.get(item))
            .is_some_and(|i| matches!(i.kind, ItemKind::Terminal { .. }));
        if terminal && self.quick.pending == Some(key) {
            self.quick.pending = None;
            self.quick.item = Some(item);
        }
    }

    /// What the panel holds now: the shell, or why there is none.
    fn quick_body(&self) -> Body {
        let waiting = |line: String, detail: Option<String>| {
            Body::Waiting(line.into(), detail.map(SharedString::from))
        };
        if let Some(key) = self.quick.pending {
            return waiting(format!("Opening a shell on {}\u{2026}", self.worker_name(key)), None);
        }
        let Some(tile) = self.quick.item.and_then(|item| self.tile_of(item)) else {
            let detail = self.workers.is_empty().then(|| "Add one from the palette".to_owned());
            return waiting(NO_WORKER.to_owned(), detail);
        };
        let session = match self.item(tile).map(|i| &i.kind) {
            Some(ItemKind::Terminal { session }) => *session,
            _ => return waiting(NO_WORKER.to_owned(), None),
        };
        if let Some(view) = self.terminals.get(&session) {
            return Body::Terminal(view.clone());
        }
        let name = self.worker_name(tile.worker);
        match self.workers.get(&tile.worker).filter(|w| !w.is_linked()) {
            Some(w) => waiting(format!("{name} is {}", w.status.text()), None),
            None => waiting(format!("Attaching to {name}\u{2026}"), None),
        }
    }

    /// The panel, opened hidden the first time it is wanted.
    fn quick_window(&mut self, cx: &mut Context<Self>) -> Option<WindowHandle<QuickTerminalView>> {
        if let Some(handle) = self.quick.window {
            return Some(handle);
        }
        // Along the top of the main screen until the first show puts it under the pointer.
        let display = cx.primary_display();
        let screen = display.as_ref().map_or_else(
            || Bounds::new(gpui::Point::default(), size(px(1280.0), px(800.0))),
            |d| d.visible_bounds(),
        );
        let config = self.quick.config;
        let height = config.height;
        let bounds = Bounds::new(
            screen.origin,
            size(screen.size.width, (screen.size.height * height).max(px(1.0))),
        );
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(WINDOW_TITLE.into()),
                appears_transparent: true,
                traffic_light_position: None,
            }),
            focus: false,
            show: false,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            display_id: display.map(|d| d.id()),
            window_background: WindowBackgroundAppearance::Transparent,
            // A shell is watched while another app has the keyboard.
            inactive_frame_interval: None,
            ..WindowOptions::default()
        };
        let workspace = cx.entity();
        let theme = self.theme.clone();
        let body = self.quick_body();
        let opened = cx.open_window(options, |window, cx| {
            cx.new(|cx| {
                let mut view = QuickTerminalView::new(theme, body, config, window, cx);
                view.keep(cx.observe_in(&workspace, window, |q, ws, window, cx| {
                    let ended = ws.update(cx, Self::quick_followed);
                    let (theme, body, config) = {
                        let ws = ws.read(cx);
                        (ws.theme.clone(), ws.quick_body(), ws.quick.config)
                    };
                    q.configure(&theme, config, cx);
                    q.set_body(body, window, cx);
                    if ended {
                        q.hide(window, cx);
                    }
                }));
                view
            })
        });
        match opened {
            Ok(handle) => {
                self.quick.window = Some(handle);
                Some(handle)
            }
            Err(e) => {
                tracing::warn!(error = %e, "the quick terminal's panel");
                None
            }
        }
    }

    /// Keep the quick terminal's item with the workspace: forget it once its tile is gone, and
    /// put away a shell that ended. Returns whether it just ended.
    fn quick_followed(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(item) = self.quick.item else { return false };
        let Some(tile) = self.tile_of(item) else {
            self.quick.item = None;
            return false;
        };
        let Some(ItemKind::Terminal { session }) = self.item(tile).map(|i| i.kind.clone()) else {
            self.quick.item = None;
            return false;
        };
        let ended =
            self.summary(session).is_some_and(|s| matches!(s.state, SessionState::Exited { .. }));
        if ended {
            self.quick.item = None;
            self.propose(tile.worker, ItemOp::Remove(item), cx);
        }
        ended
    }

    /// Hand the panel the workspace's latest: a new theme, the settings, a shell attached.
    fn refresh_quick(&self, cx: &mut Context<Self>) {
        let Some(handle) = self.quick.window else { return };
        let (theme, body, config) = (self.theme.clone(), self.quick_body(), self.quick.config);
        let _gone = handle.update(cx, |q, window, cx| {
            q.configure(&theme, config, cx);
            q.set_body(body, window, cx);
        });
    }
}
