//! The panes beside each session kept across launches (`docs/decisions/layout.md`, "No
//! permanent right column"). A session's view comes back as it was when the app quit: its shells
//! on the same terminals (they live on in their machine), its inspector, files, pages, changes and
//! remote windows, and the pane that had the keyboard. Where each stood is not kept: the views
//! dock again at the session's right and the shells split beside it. A session's panes come back
//! the first time it is shown, once the app is connected; a terminal that ended meanwhile does
//! not.

use gpui::{App, AppContext as _, Context, Window};

use super::panes::Pane;
use super::{RightSurface, Shell};
use crate::settings::{SavedSurface, SavedTabs};
use crate::terminal::panel::TerminalPanel;

/// How many sessions' panes are kept.
const KEPT: usize = 500;

impl Shell {
    /// The panes of the view `key` as kept, in reading order; `None` when it keeps none.
    pub(super) fn kept_tabs(&self, key: &str, cx: &App) -> Option<SavedTabs> {
        // The new-session canvas's shells live while the app runs.
        if key.starts_with(crate::state::CANVAS_PANEL_PREFIX) {
            return None;
        }
        let panes = self.panes.get(key)?;
        let focused = panes.focused();
        let mut saved = SavedTabs::default();
        for pane in panes.panes() {
            if let Some(kept) = self.kept_pane(pane, cx) {
                if pane == focused {
                    saved.active = Some(saved.tabs.len());
                }
                saved.tabs.push(kept);
            }
        }
        (!saved.tabs.is_empty()).then_some(saved)
    }

    /// What opens `pane` again; `None` for one that is not kept.
    pub(super) fn kept_pane(&self, pane: Pane, cx: &App) -> Option<SavedSurface> {
        Some(match pane {
            Pane::Session => return None,
            Pane::Shell(n) => {
                let id = self.pane_shells.get(&n)?.read(cx).pane_terminal_id()?.to_owned();
                SavedSurface::Terminal { id }
            }
            Pane::View(RightSurface::File(id)) => {
                let path = self.file_surface_paths.get(&id)?.clone();
                let chat = self.file_surfaces.get(&id)?.read(cx).chat_id().to_owned();
                SavedSurface::File { chat, path }
            }
            Pane::View(RightSurface::Browser(id)) => {
                let url = self.browsers.get(&id)?.read(cx).url()?.to_owned();
                SavedSurface::Browser { url }
            }
            Pane::View(RightSurface::Diff(id)) => {
                self.diffs.get(&id)?;
                SavedSurface::Diff
            }
            Pane::View(RightSurface::Screen(id)) => {
                let (screen, title) = self.screen_surface_chosen(id, cx)?;
                SavedSurface::Screen { screen, title }
            }
            Pane::View(RightSurface::Inspector) => SavedSurface::Inspector,
            Pane::View(RightSurface::Files) => SavedSurface::Files,
            // An agent's page lives while it shows it.
            Pane::View(RightSurface::Page(_)) => return None,
        })
    }

    /// Keep the panes of every view shown in this run once they changed, after the frame; those
    /// not shown yet stay as the last run left them.
    pub(super) fn keep_tabs(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        let keys: Vec<String> = self.panes.keys().cloned().collect();
        for key in keys {
            match self.kept_tabs(&key, cx) {
                Some(saved) if self.settings.session_tabs.get(&key) != Some(&saved) => {
                    self.settings.session_tabs.insert(key, saved);
                    changed = true;
                }
                Some(_) => {}
                None => changed |= self.settings.session_tabs.remove(&key).is_some(),
            }
        }
        if !changed {
            return;
        }
        while self.settings.session_tabs.len() > KEPT {
            let Some(key) = self.settings.session_tabs.keys().next().cloned() else { break };
            self.settings.session_tabs.remove(&key);
        }
        cx.spawn(async move |this, cx| {
            this.update(cx, |shell, cx| shell.schedule_save(cx)).ok();
        })
        .detach();
    }

    /// Bring back the panes the shown session's view had when the app last ran, once it is
    /// connected and knows the session; a view given panes in this run keeps its own.
    pub(super) fn restore_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_chat.is_empty() || self.unrestored_tabs.is_empty() {
            return;
        }
        let key = self.panel_key(cx);
        if !self.unrestored_tabs.contains_key(&key) {
            return;
        }
        if self.panes.get(&key).is_some_and(|panes| panes.len() > 1) {
            self.unrestored_tabs.remove(&key);
            return;
        }
        // A terminal or a remote window opens through the connection; the rest wait only for
        // the chat.
        let ready = {
            let state = self.state.read(cx);
            let connects = self.unrestored_tabs[&key].tabs.iter().any(|tab| {
                matches!(tab, SavedSurface::Terminal { .. } | SavedSurface::Screen { .. })
            });
            (!connects || state.engine().is_some()) && state.chats.iter().any(|chat| chat.id == key)
        };
        let Some(saved) = ready.then(|| self.unrestored_tabs.remove(&key)).flatten() else {
            return;
        };
        let mut front = None;
        for (at, surface) in saved.tabs.into_iter().enumerate() {
            // Each comes back beside the session: the views docked at its right in one column,
            // the shells split beside the terminal.
            self.panes_mut(cx).focus(Pane::Session);
            let made = self.restore_pane(&key, surface, window, cx);
            if saved.active == Some(at) {
                front = made;
            }
        }
        self.panes_mut(cx).focus(front.unwrap_or(Pane::Session));
        self.pane_keyboard_pending = true;
        cx.notify();
    }

    /// Open `surface` again beside the session of the view `key`, the shown one: the pane it
    /// made.
    pub(super) fn restore_pane(
        &mut self,
        key: &str,
        surface: SavedSurface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Pane> {
        match surface {
            SavedSurface::Terminal { id } => {
                let state = self.state.clone();
                let shell = cx.new(|cx| TerminalPanel::new(state, cx));
                shell.update(cx, |shell, cx| shell.reopen_tab_for_selected(id, cx))?;
                Some(Pane::Shell(self.add_shell_beside(key, shell, cx)))
            }
            SavedSurface::File { chat, path } => {
                let owner = self
                    .link_owner(&chat, cx)
                    .unwrap_or_else(|| (chat.clone(), self.state.clone()));
                self.add_file_surface_at(owner, path, None, window, cx);
                self.front_view(cx).map(Pane::View)
            }
            SavedSurface::Browser { url } => {
                self.add_browser_surface(Some(url), window, cx);
                Some(Pane::View(RightSurface::Browser(self.browser_seq)))
            }
            SavedSurface::Diff => {
                self.add_diff_surface(window, cx);
                Some(Pane::View(RightSurface::Diff(self.diff_seq)))
            }
            SavedSurface::Screen { screen, title } => {
                Some(Pane::View(self.reopen_screen_surface(screen, title, cx)))
            }
            SavedSurface::Inspector => {
                self.open_view(RightSurface::Inspector, cx);
                Some(Pane::View(RightSurface::Inspector))
            }
            SavedSurface::Files => {
                self.ensure_explorer(window, cx);
                self.open_view(RightSurface::Files, cx);
                Some(Pane::View(RightSurface::Files))
            }
        }
    }
}
