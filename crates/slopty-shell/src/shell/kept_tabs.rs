//! slopty: the right pane's surface tabs kept across launches. Each chat's pane comes back as it
//! was when the app quit, its tabs in their order and the one in front: its terminals on the
//! same sessions (they live on in their machine), its files, pages, diffs,
//! side chats and remote windows. A chat's tabs come back the first time it is shown, once the
//! app is connected; a terminal that ended meanwhile does not.

use gpui::{App, Context, Window};

use super::{RightSurface, Shell};
use crate::settings::{SavedSurface, SavedTabs};

/// How many chats' tabs are kept.
const KEPT: usize = 500;

impl Shell {
    /// The tabs of the pane `key` as kept; `None` when it keeps none.
    pub(super) fn kept_tabs(&self, key: &str, cx: &App) -> Option<SavedTabs> {
        let front = self.panels.get(key).right_active;
        let mut saved = SavedTabs::default();
        for surface in self.right_tabs.get(key)? {
            if let Some(kept) = self.kept_surface(key, *surface, cx) {
                if *surface == front {
                    saved.active = Some(saved.tabs.len());
                }
                saved.tabs.push(kept);
            }
        }
        (!saved.tabs.is_empty()).then_some(saved)
    }

    /// What opens `surface` of the pane `key` again; `None` for one that is not kept.
    pub(super) fn kept_surface(&self, key: &str, surface: RightSurface, cx: &App) -> Option<SavedSurface> {
        Some(match surface {
            RightSurface::Terminal(tab) => {
                let panel = self.right_terminal.as_ref()?.read(cx);
                let id = panel.terminal_id(key, tab)?.to_owned();
                SavedSurface::Terminal { id }
            }
            RightSurface::File(id) => {
                let path = self.file_surface_paths.get(&id)?.clone();
                let chat = self.file_surfaces.get(&id)?.read(cx).chat_id().to_owned();
                SavedSurface::File { chat, path }
            }
            RightSurface::Browser(id) => {
                let url = self.browsers.get(&id)?.read(cx).url()?.to_owned();
                SavedSurface::Browser { url }
            }
            RightSurface::Diff(id) => {
                self.diffs.get(&id)?;
                SavedSurface::Diff
            }
            RightSurface::SideChat(id) => {
                let chat = self.side_chats.get(&id)?.state.read(cx).selected_chat.clone()?;
                SavedSurface::SideChat { chat }
            }
            RightSurface::Screen(id) => {
                let (screen, title) = self.screen_surface_chosen(id, cx)?;
                SavedSurface::Screen { screen, title }
            }
            RightSurface::Picker | RightSurface::Subagent(_) => return None,
        })
    }

    /// Keep the tabs of every pane shown in this run once they changed, after the frame; those
    /// not shown yet stay as the last run left them.
    pub(super) fn keep_tabs(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        let keys: Vec<String> = self.right_tabs.keys().cloned().collect();
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

    /// Bring back the tabs the shown chat's pane had when the app last ran, once it is
    /// connected and knows the chat; a pane given tabs in this run keeps its own.
    pub(super) fn restore_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_chat.is_empty() || self.unrestored_tabs.is_empty() {
            return;
        }
        let key = self.panel_key(cx);
        if !self.unrestored_tabs.contains_key(&key) {
            return;
        }
        if self.right_tabs.get(&key).is_some_and(|tabs| !tabs.is_empty()) {
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
            (!connects || state.engine().is_some())
                && state.chats.iter().any(|chat| chat.id == key)
        };
        let Some(saved) = ready.then(|| self.unrestored_tabs.remove(&key)).flatten() else {
            return;
        };
        let open = self.panels.get(&key).changes_open;
        let mut made = Vec::new();
        for surface in saved.tabs {
            made.push(self.restore_surface(&key, surface, window, cx));
        }
        // Opening a file opens the pane; it stays as the person left it.
        self.panels.update(&key, |panels| panels.changes_open = open);
        let front = saved.active.and_then(|at| made.get(at).copied().flatten());
        if let Some(front) = front.or_else(|| made.iter().rev().find_map(|made| *made)) {
            self.set_right_active(front, cx);
        }
        cx.notify();
    }

    /// Open `surface` again in the pane `key`, the shown chat's: the tab it made.
    pub(super) fn restore_surface(
        &mut self,
        key: &str,
        surface: SavedSurface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<RightSurface> {
        match surface {
            SavedSurface::Terminal { id } => {
                let panel = self.right_terminal_panel(cx);
                let tab = panel.update(cx, |panel, cx| {
                    panel.set_open(true, cx);
                    panel.reopen_tab_for_selected(id, cx)
                })?;
                let surface = RightSurface::Terminal(tab);
                self.right_tabs.entry(key.to_owned()).or_default().push(surface);
                Some(surface)
            }
            SavedSurface::File { chat, path } => {
                let owner = self
                    .link_owner(&chat, cx)
                    .unwrap_or_else(|| (chat.clone(), self.state.clone()));
                self.add_file_surface_at(owner, path, None, window, cx);
                Some(self.panels.get(key).right_active)
            }
            SavedSurface::Browser { url } => {
                self.add_browser_surface(Some(url), window, cx);
                Some(RightSurface::Browser(self.browser_seq))
            }
            SavedSurface::Diff => {
                self.add_diff_surface(window, cx);
                Some(RightSurface::Diff(self.diff_seq))
            }
            SavedSurface::SideChat { chat } => {
                let row = self.state.read(cx).chats.iter().find(|row| row.id == chat).cloned()?;
                self.open_side_chat(row, key.to_owned(), cx);
                self.side_chats.iter().find_map(|(id, side)| {
                    (side.state.read(cx).selected_chat.as_deref() == Some(chat.as_str()))
                        .then_some(RightSurface::SideChat(*id))
                })
            }
            SavedSurface::Screen { screen, title } => {
                Some(self.reopen_screen_surface(screen, title, cx))
            }
        }
    }
}
