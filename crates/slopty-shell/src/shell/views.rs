//! The views summoned beside a session's terminal (`docs/decisions/layout.md`, "No permanent
//! right column"): the session's files and a file of them, its changes, a web page, its
//! inspector, a remote window or desktop, and a page an agent showed. Each is a pane of the
//! session's own view ([`super::panes`]), docked at the session's right when it opens; what it
//! shows, what it is called, where its keyboard goes and what closing it lets go of are here.

use super::panes::Pane;
use super::*;

impl Shell {
    /// The view with the keyboard in the shown session's panes.
    pub(super) fn front_view(&self, cx: &App) -> Option<RightSurface> {
        match self.panes_now(cx).focused() {
            Pane::View(view) => Some(view),
            _ => None,
        }
    }

    /// Whether `view` is a pane of the shown session's view.
    pub(super) fn view_open(&self, view: RightSurface, cx: &App) -> bool {
        self.panes_now(cx).contains(Pane::View(view))
    }

    /// Whether the inspector has the keyboard: on a phone, its sheet is up.
    pub(super) fn inspector_shown(&self, cx: &App) -> bool {
        self.front_view(cx) == Some(RightSurface::Inspector)
    }

    /// The views laid out this frame, which draw: the shown session's, through a zoom only the
    /// pane zoomed, and on a phone only the page in front. None before the app is `ready` or
    /// off a session's page.
    pub(super) fn laid_views(&self, ready: bool, cx: &mut App) -> Vec<RightSurface> {
        if !ready || !matches!(self.route, Route::Chat) || self.active_chat.is_empty() {
            return Vec::new();
        }
        if self.compact() && self.compact_sessions_up(cx) {
            return Vec::new();
        }
        let panes = self.panes_now(cx);
        if self.compact() || panes.zoomed().is_some() {
            return match panes.zoomed().unwrap_or_else(|| panes.focused()) {
                Pane::View(view) => vec![view],
                _ => Vec::new(),
            };
        }
        panes.views()
    }

    /// Open `view` beside the session shown, docked at its right and focused, its keyboard
    /// given once it draws; already open, it is focused where it stands.
    pub(super) fn open_view(&mut self, view: RightSurface, cx: &mut Context<Self>) {
        if self.active_chat.is_empty() {
            return;
        }
        let share = self.view_share();
        self.panes_mut(cx).dock(Pane::View(view), share);
        self.pane_keyboard_pending = true;
        self.wake_view(view, cx);
        cx.notify();
    }

    /// `view` came forward: what it shows loads, a file counts as opened, a remote window takes
    /// its stream's keys, and Files reveals the file in front.
    pub(super) fn wake_view(&mut self, view: RightSurface, cx: &mut Context<Self>) {
        match view {
            RightSurface::File(id) => {
                if let Some(file) = self.file_surfaces.get(&id).cloned() {
                    file.update(cx, |file, cx| file.ensure_loaded(cx));
                    if let Some(path) = self.file_surface_paths.get(&id).cloned() {
                        let chat_id = file.read(cx).chat_id().to_owned();
                        self.note_recent_file(&chat_id, &path);
                    }
                }
            }
            RightSurface::Diff(id) => {
                if let Some(changes) = self.diffs.get(&id).cloned() {
                    changes.update(cx, |changes, cx| changes.ensure_content(cx));
                }
            }
            RightSurface::Screen(id) => self.focus_screen_surface(id, cx),
            RightSurface::Files => {
                if let Some(files) = self.files.get(&self.panel_key(cx)).cloned() {
                    files.update(cx, |files, cx| files.ensure_loaded(cx));
                }
            }
            RightSurface::Browser(_) | RightSurface::Inspector | RightSurface::Page(_) => {}
        }
        self.sync_explorer_selection(cx);
    }

    /// ⌘I and the inspector's buttons: summoned as any view is. On a phone, whose inspector is
    /// a sheet over the session, it goes when put away rather than staying behind it.
    pub(super) fn toggle_inspector(&mut self, cx: &mut Context<Self>) {
        // A plain terminal has nothing to inspect.
        if !self.inspector_shown(cx)
            && !crate::inspector::inspects(self.state.read(cx), &self.active_chat)
        {
            return;
        }
        if self.compact() && self.inspector_shown(cx) {
            self.put_away_view(RightSurface::Inspector, cx);
        } else {
            self.summon_view(RightSurface::Inspector, cx);
        }
    }

    /// Take `view` away from the session shown without remembering it to open again: a phone's
    /// inspector sheet put away.
    pub(super) fn put_away_view(&mut self, view: RightSurface, cx: &mut Context<Self>) {
        let key = self.panel_key(cx);
        self.drop_view(view, &key, cx);
    }

    /// Files by its key (⌘E): the session's explorer, made the first time.
    pub(super) fn summon_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_chat.is_empty() {
            return;
        }
        self.ensure_explorer(window, cx);
        self.summon_view(RightSurface::Files, cx);
    }

    /// Changes by its key (⌘R): the session's changes already beside it, or new ones, where its
    /// folder has git.
    pub(super) fn summon_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self
            .panes_now(cx)
            .views()
            .into_iter()
            .find(|view| matches!(view, RightSurface::Diff(_)));
        match shown {
            Some(view) => self.summon_view(view, cx),
            None if self.space_git_detected(cx) => self.show_changes(window, cx),
            None => {}
        }
    }

    /// The session's changes beside it with the keyboard: those open, or new ones.
    pub(super) fn show_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_chat.is_empty() {
            return;
        }
        let shown = self
            .panes_now(cx)
            .views()
            .into_iter()
            .find(|view| matches!(view, RightSurface::Diff(_)));
        let view = match shown {
            Some(view) => view,
            None => {
                let changes = cx.new(|cx| Changes::new(self.state.clone(), cx));
                self.register_diff_surface(changes, window, cx);
                RightSurface::Diff(self.diff_seq)
            }
        };
        self.open_view(view, cx);
    }

    /// Close `view` as the person asks it (its header's ✕, ⌘W): a file with unsaved edits asks
    /// first and closes once they are saved or let go; the rest close now and can be opened
    /// again (⇧⌘T).
    pub(super) fn close_view(&mut self, view: RightSurface, cx: &mut Context<Self>) {
        let key = self.panel_key(cx);
        if let RightSurface::File(id) = view
            && let Some(file) = self.file_surfaces.get(&id).cloned()
        {
            match file.update(cx, |file, cx| file.prepare_close(cx)) {
                FilesCloseDisposition::Allow => self.complete_file_close(view, &key, cx),
                FilesCloseDisposition::Pending | FilesCloseDisposition::Blocked => {
                    // Its question shows in its pane, which keeps the keyboard.
                    self.pending_file_closes.insert(view);
                    self.panes_mut(cx).focus(Pane::View(view));
                    self.pane_keyboard_pending = true;
                    cx.notify();
                }
            }
            return;
        }
        self.remember_closed_tab(&key, view, cx); // ⇧⌘T opens it again
        self.drop_view(view, &key, cx);
    }

    /// A file's close went through (saved, or its edits let go): its pane closes.
    pub(super) fn complete_file_close(
        &mut self,
        view: RightSurface,
        key: &str,
        cx: &mut Context<Self>,
    ) {
        self.remember_closed_tab(key, view, cx); // ⇧⌘T opens it again
        self.pending_file_closes.remove(&view);
        self.drop_view(view, key, cx);
    }

    /// Take `view` out of the panes of `key`, letting go of what it showed; its sibling takes its
    /// room and, on screen, the keyboard.
    pub(super) fn drop_view(&mut self, view: RightSurface, key: &str, cx: &mut Context<Self>) {
        if let Some(panes) = self.panes.get_mut(key) {
            panes.close(Pane::View(view));
        }
        self.forget_view(view, key, cx);
        if key == self.panel_key(cx) {
            self.pane_keyboard_pending = true;
        }
        cx.notify();
    }

    /// Let go of what `view` of the panes of `key` showed: a page's browser closes, a diff stops
    /// watching, a file's editor goes, a remote window stops streaming, an agent's page goes back
    /// to its card, Files stops following the tree.
    pub(super) fn forget_view(&mut self, view: RightSurface, key: &str, cx: &mut Context<Self>) {
        self.view_handles.remove(&view);
        match view {
            RightSurface::File(id) => {
                self.file_surfaces.remove(&id);
                self.file_surface_paths.remove(&id);
                self.file_surface_subs.remove(&id);
                self.file_surface_keys.retain(|_, value| *value != id);
                self.pending_file_closes.remove(&view);
            }
            RightSurface::Browser(id) => {
                if let Some(browser) = self.browsers.remove(&id) {
                    browser.update(cx, |browser, cx| browser.close(cx));
                }
                self.browser_subs.remove(&id);
            }
            RightSurface::Diff(id) => {
                // Dropping the entity tears down its diff watch.
                self.diffs.remove(&id);
                self.diff_subs.remove(&id);
            }
            RightSurface::Screen(id) => self.close_screen_surface(id, cx),
            RightSurface::Page(id) => self.close_page_pane(id, cx),
            RightSurface::Files => {
                if let Some(files) = self.files.get(key).cloned() {
                    files.update(cx, |files, cx| files.suspend_tree_interactions(cx));
                }
            }
            RightSurface::Inspector => {}
        }
    }

    /// `view`'s pane: what it shows inside a focus of its own, so a press that leaves the
    /// keyboard nowhere inside gives the view its keyboard as its key would ([`Self::focus_view`]:
    /// a diff its own keys, a file its editor), and the pane focused has it. The copy key there
    /// copies a transcript's selection (the inspector's) once nothing inside took it.
    pub(super) fn view_pane(
        &mut self,
        view: RightSurface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let handle = self.view_handle(view, cx);
        let held = handle.clone();
        div()
            .size_full()
            .track_focus(&handle)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    if !held.contains_focused(window, cx) {
                        this.focus_view(view, window, cx);
                    }
                }),
            )
            .on_key_down(|event, _, cx| navigation_focus::copy_transcript_selection(event, cx))
            .child(self.view_content(view, window, cx))
            .into_any_element()
    }

    /// What `view`'s pane shows. Files keeps its git state live only while it is laid out.
    pub(super) fn view_content(
        &mut self,
        view: RightSurface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let body = match view {
            RightSurface::File(id) => match self.file_surfaces.get(&id).cloned() {
                Some(file) => {
                    file.update(cx, |file, cx| file.ensure_loaded(cx));
                    file.into_any_element()
                }
                None => Empty.into_any_element(),
            },
            RightSurface::Diff(id) => match self.diffs.get(&id).cloned() {
                Some(changes) => {
                    // Idempotent — also covers a view kept open across launches.
                    changes.update(cx, |changes, cx| changes.ensure_content(cx));
                    // The diff's options (scope, base, fold all) stand over it.
                    let controls =
                        changes.update(cx, |changes, cx| changes.render_header_controls(cx));
                    div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .child(crate::surface_chrome::toolbar(&theme).child(controls))
                        .child(div().flex_1().min_h_0().child(changes))
                        .into_any_element()
                }
                None => Empty.into_any_element(),
            },
            RightSurface::Browser(id) => self
                .browsers
                .get(&id)
                .cloned()
                .map_or_else(|| Empty.into_any_element(), IntoElement::into_any_element),
            RightSurface::Inspector => self.inspector.clone().into_any_element(),
            RightSurface::Screen(id) => self.render_screen_surface(id, cx),
            RightSurface::Page(id) => self.render_page_pane(id, cx),
            RightSurface::Files => match self.files.get(&self.panel_key(cx)).cloned() {
                Some(files) => {
                    files.update(cx, |files, cx| {
                        files.ensure_loaded(cx);
                        files.ensure_git_status(cx);
                    });
                    self.sync_explorer_selection(cx);
                    files.into_any_element()
                }
                None => {
                    // A view kept open across launches has its place, not its explorer yet.
                    cx.defer_in(window, |shell, window, cx| shell.ensure_explorer(window, cx));
                    Empty.into_any_element()
                }
            },
        };
        div().size_full().bg(theme.panel_bg()).child(body).into_any_element()
    }

    /// Give `view` the keyboard: a file its editor, Files its tree, a page its address while it
    /// has none, the changes their own keys, a remote window its stream; the rest, their pane.
    pub(super) fn focus_view(
        &mut self,
        view: RightSurface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match view {
            RightSurface::File(id) => {
                if let Some(file) = self.file_surfaces.get(&id).cloned() {
                    file.update(cx, |file, cx| file.focus_editor(window, cx));
                    return;
                }
            }
            RightSurface::Files => {
                if let Some(files) = self.files.get(&self.panel_key(cx)).cloned() {
                    files.update(cx, |files, cx| files.focus_explorer(window, cx));
                    return;
                }
            }
            RightSurface::Browser(id) => {
                if let Some(browser) = self.browsers.get(&id).cloned() {
                    if browser.read(cx).url().is_none() {
                        browser.update(cx, |browser, cx| browser.focus_address(window, cx));
                    } else {
                        window.focus(&browser.focus_handle(cx), cx);
                    }
                    return;
                }
            }
            RightSurface::Diff(id) => {
                if let Some(changes) = self.diffs.get(&id).cloned() {
                    changes.update(cx, |changes, cx| changes.take_keyboard(cx));
                }
            }
            RightSurface::Screen(id) => self.focus_screen_surface(id, cx),
            RightSurface::Inspector | RightSurface::Page(_) => {}
        }
        let handle = self.view_handle(view, cx);
        if !handle.contains_focused(window, cx) {
            window.focus(&handle, cx);
        }
    }

    /// The focus `view`'s pane holds, around what it shows: where its keyboard goes when it has
    /// none of its own, and how the shell knows the pane holds the keyboard.
    pub(super) fn view_handle(
        &mut self,
        view: RightSurface,
        cx: &mut Context<Self>,
    ) -> FocusHandle {
        self.view_handles.entry(view).or_insert_with(|| cx.focus_handle()).clone()
    }

    /// What `view`'s pane header names: its glyph and its title.
    pub(super) fn view_identity(
        &self,
        view: RightSurface,
        cx: &App,
    ) -> (&'static str, SharedString) {
        match view {
            RightSurface::Files => (icons::FOLDER_TREE, "Files".into()),
            RightSurface::File(id) => {
                let title = self
                    .file_surface_paths
                    .get(&id)
                    .map_or_else(|| SharedString::from("File"), |path| workspace_file_title(path));
                (icons::DOCUMENT, title)
            }
            // The pane's name; its scope is the picker's.
            RightSurface::Diff(id) => (
                icons::FILE_DIFF,
                self.diffs
                    .get(&id)
                    .map_or_else(|| "Changes".into(), |changes| changes.read(cx).tab_title()),
            ),
            // Steps in time, as a trace reads; the eye is the inbox's.
            RightSurface::Inspector => (icons::SORT_BY_TIME, "Inspector".into()),
            RightSurface::Browser(id) => (
                icons::GLOBE,
                self.browsers
                    .get(&id)
                    .map_or_else(|| "Browser".into(), |browser| browser.read(cx).title()),
            ),
            RightSurface::Screen(id) => {
                let chosen = self.screen_surface_chosen(id, cx).map(|(screen, _)| screen);
                let title =
                    self.screen_surface_title(id, cx).unwrap_or_else(|| "Remote window".into());
                (screen_chip_glyph(chosen.as_deref()), title)
            }
            RightSurface::Page(id) => {
                (icons::GLOBE, self.page_pane_title(id, cx).unwrap_or_else(|| "Page".into()))
            }
        }
    }

    /// The session's explorer, made the first time its Files is asked for: its events reach the
    /// shell, and a file picked in it opens as a pane of its own.
    pub(super) fn ensure_explorer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_chat.is_empty() {
            return;
        }
        let key = self.panel_key(cx);
        if self.files.contains_key(&key) {
            return;
        }
        let files = cx.new(|cx| {
            FilesSurface::new_explorer(
                self.state.clone(),
                self.active_chat.clone(),
                self.settings.files_show_all,
                cx,
            )
        });
        let owner = key.clone();
        let sub =
            cx.subscribe_in(&files, window, move |this: &mut Self, source, event, window, cx| {
                match event {
                    FilesEvent::HoldMutation { origin, paths } => {
                        let surfaces = this
                            .files
                            .values()
                            .chain(this.file_surfaces.values())
                            .filter(|s| s.read(cx).shares_workspace(origin))
                            .cloned()
                            .collect::<Vec<_>>();
                        for surface in surfaces {
                            surface.update(cx, |files, cx| files.hold_mutation(paths.clone(), cx));
                        }
                    }
                    FilesEvent::Mutate(intent)
                        if this.accepts_file_navigation(&owner, source, cx) =>
                    {
                        this.start_file_mutation(source.clone(), intent.clone(), cx);
                    }
                    FilesEvent::MutateMany(intents)
                        if this.accepts_file_navigation(&owner, source, cx) =>
                    {
                        this.start_file_mutations(source.clone(), intents.clone(), cx);
                    }
                    FilesEvent::OpenFile(path)
                        if this.accepts_file_navigation(&owner, source, cx) =>
                    {
                        this.add_file_surface(path.clone(), window, cx);
                    }
                    FilesEvent::OpenWebLink(activation) => {
                        if let crate::markdown::render::LinkOutcome::External(url) =
                            this.activate_session_link(activation, window, cx)
                        {
                            cx.open_url(&url);
                        }
                    }
                    FilesEvent::ShowAllFilesChanged(show_all) => {
                        this.set_files_show_all(*show_all, cx)
                    }
                    _ => cx.notify(),
                }
            });
        self.files.insert(key.clone(), files);
        self.files_subs.insert(key, sub);
    }
}
