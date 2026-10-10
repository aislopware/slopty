//! The overlays over the panes: the command palette (and its find in every tile), and the
//! picker of a worker's windows.

use std::collections::HashSet;

use gpui::{App, AppContext as _, Context, Entity, Window};
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_core::SessionId;
use slopty_proto::ClientMsg;
use slopty_proto::items::ItemKind;
use slopty_proto::screen::{DisplayInfo, WindowInfo};
use slopty_proto::thread::wire::ThreadRow;

use super::WorkspaceView;
use super::actions::{OpenCommands, OpenFile, OpenFolder, OpenPalette, StartThread};
use super::agents::{agent_mark_of, agent_status_text, needs_human};
use crate::kit::find::Query;
use crate::palette::{self, CommandPalette, PaletteEvent, PaletteItem, PaletteRun};
use crate::picker::{PickerEvent, SessionRow, WindowPicker};

/// How much of an agent's first prompt, and of its last answer, the palette searches: enough
/// for what a task is about, bounded for a transcript that pasted a log.
const ABOUT_CHARS: usize = 2_000;

/// How many threads worked in lately an empty search lists.
const RECENT_THREADS: usize = 5;

/// What the empty field of the search of everything says (⌘K, ⌘⇧P).
pub(super) const SEARCH_EVERYTHING: &str = "Search tiles, threads, files and commands";

/// What the empty field of the search of files says (⌘P).
pub(super) const SEARCH_FILES: &str = "Search files";

/// What the search of files says when nothing is found.
const NO_FILE_MATCHES: &str = "No file matches";

impl WorkspaceView {
    /// Every tile in reading order: project by project, tab by tab, pane by pane.
    pub(super) fn reading_order(&self) -> Vec<TileRef> {
        self.layout.tiles().collect()
    }

    /// Where `tile` stands in reading order ([`Self::reading_order`]).
    pub(super) fn reading_rank(&self, tile: TileRef) -> Option<usize> {
        self.layout.tiles().position(|t| t == tile)
    }

    /// `worker`'s name for a line about one of its tiles, when more than one worker is known
    /// and so the name tells them apart.
    fn worker_label(&self, worker: WorkerKey) -> Option<String> {
        (self.workers.len() > 1)
            .then(|| self.workers.get(&worker).map(|w| w.name.clone()))
            .flatten()
    }

    /// A line per worker: its name, with its round trip on the right while its link is up,
    /// else a mark and a word for what is wrong.
    pub(super) fn worker_lines(&self) -> impl Iterator<Item = PaletteItem> + '_ {
        self.workers.iter().map(|(key, w)| {
            let health = super::navigator::worker_health(&w.status);
            let detail = match health {
                Some((_, word)) => palette::sentence_case(word),
                None => super::navigator::slow_rtt(self.shown_rtt(w)).unwrap_or_default(),
            };
            let form = w.caps.as_ref().map(|caps| caps.form);
            PaletteItem::worker(&w.name, &detail, *key)
                .with_icon(crate::icons::machine(form))
                .with_status(health.map(|(mark, _)| mark))
        })
    }

    /// "Wake `<worker>`" for each worker the app can wake from sleep.
    fn wake_lines(&self) -> impl Iterator<Item = PaletteItem> + '_ {
        self.workers
            .iter()
            .filter(|(key, _)| self.host_actions(**key).is_some_and(|a| a.wake.is_some()))
            .map(|(key, w)| PaletteItem::wake(&w.name, *key))
    }

    /// Run the app's wake for `worker`, on the next frame.
    fn wake_worker(&mut self, worker: WorkerKey, cx: &mut Context<Self>) {
        if let Some(run) = self.host_actions(worker).and_then(|a| a.wake.clone()) {
            self.pending_runs.push(run);
            cx.notify();
        }
    }

    /// Every line the palette offers: the sessions to go to (agents waiting on the human
    /// first), every other tile, the workers, the last few commands of the shell a "run" would
    /// go to, then every action, then the app's own. The palette groups them into its sections.
    #[must_use]
    pub fn palette_lines(&self, cx: &Context<Self>) -> Vec<PaletteItem> {
        let mut items: Vec<PaletteItem> = self
            .session_rows()
            .into_iter()
            .map(|row| {
                PaletteItem::session(&row.title, row.session)
                    .with_icon(row.lead)
                    .with_status(row.mark)
                    .on_worker(row.worker)
                    .in_dir(row.cwd)
                    .aged(row.age)
                    .about(self.agent_about(row.session))
            })
            .collect();
        // Every other tile, by the title its header shows, as the navigator lists them: a
        // tile the palette cannot find is one the human has to hunt for by eye.
        let mut others: Vec<TileRef> = self.reading_order();
        others.sort_by_key(|t| self.recency_rank(*t));
        for tile in others {
            let Some(item) = self.item(tile) else { continue };
            if matches!(item.kind, ItemKind::Terminal { .. }) {
                continue;
            }
            let title = self.tile_title(item);
            let line = PaletteItem::item(&title, self.kind_glyph(item), item.id)
                .placed(self.tile_place(item));
            items.push(line.on_worker(self.worker_label(tile.worker)));
        }
        items.extend(self.project_rows());
        items.extend(self.project_lines());
        items.extend(self.worker_lines());
        items.extend(self.wake_lines());
        items.extend(self.closed_lines());
        items.extend(
            super::actions::palette_items().into_iter().map(|line| self.sound_line(line, cx)),
        );
        items.extend(self.attach_lines(cx));
        items.extend(self.group_lines());
        items.extend(self.scope_lines());
        items.extend(self.pin_lines());
        items.extend(self.clipboard_lines());
        items.extend(self.settings_lines());
        items.extend(self.remove_lines());
        // What the focused remote tile can do beyond its header, only while one has the focus.
        items.extend(self.screen_lines(cx));
        items.extend(self.palette_extra.iter().cloned());
        items
    }

    /// `line`, or "Unmute sound" in its place while it is ⌘⇧M's and the focused stream is
    /// muted: the line says what picking it does.
    fn sound_line(&self, line: PaletteItem, cx: &Context<Self>) -> PaletteItem {
        let mute = run_action(&line).is_some_and(|a| a.partial_eq(&super::actions::ToggleMute));
        if !mute || !self.active_screen().is_some_and(|v| v.read(cx).muted()) {
            return line;
        }
        PaletteItem { label: super::actions::UNMUTE_SOUND.to_owned(), ..line }
    }

    /// What an agent's session is about, for the palette to find it by: its thread's title and
    /// the last line its agent wrote, as its worker's table says. "the login redirect" then
    /// finds the agent working on it, whatever its tile is named.
    fn agent_about(&self, session: SessionId) -> Option<String> {
        let thread = self.session_thread(session)?;
        let clip = |text: &str| text.chars().take(ABOUT_CHARS).collect::<String>();
        let about: Vec<String> = self
            .thread_named(thread)
            .into_iter()
            .chain(self.thread_line(thread).map(str::to_owned))
            .map(|text| clip(&text))
            .collect();
        (!about.is_empty()).then(|| about.join("\n"))
    }

    /// The palette's lines that apply where the keyboard is ([`Self::palette_lines`]): an
    /// action goes unless something on the way from the focused element up to the window
    /// answers it, as the dispatch tree last drawn says. What it leaves out is kept, so the
    /// lines given again while the palette is open leave out the same.
    pub fn offered_lines(&mut self, window: &Window, cx: &Context<Self>) -> Vec<PaletteItem> {
        let from = window
            .focused(cx)
            .filter(|h| self.focus.contains(h, window))
            .unwrap_or_else(|| self.focus.clone());
        let mut hidden = HashSet::new();
        let lines = self
            .palette_lines(cx)
            .into_iter()
            .filter(|line| {
                let Some(action) = run_action(line) else { return true };
                let answered = window.is_action_available_in(action, &from);
                if !answered {
                    hidden.insert(action.as_any().type_id());
                }
                answered
            })
            .collect();
        self.palette_hidden = hidden;
        lines
    }

    /// A pick that nothing where the keyboard went back to answers any more (what had it
    /// changed while the palette was open): said in a notice, never dropped without a word.
    pub(super) fn say_unavailable(&mut self, action: &dyn gpui::Action, cx: &mut Context<Self>) {
        let label = self
            .palette_lines(cx)
            .into_iter()
            .find(|line| run_action(line).is_some_and(|a| a.partial_eq(action)))
            .map_or_else(|| "That command".to_owned(), |line| line.label);
        tracing::info!(action = action.name(), "a palette pick nothing answers");
        self.show_notice(format!("{label} does not apply here"), cx);
    }

    /// The last step of "New agent…": the thread's tile opens at once, its field for the first
    /// message taking the keyboard. Once it goes, it is what each step lists first next time
    /// ([`Self::start_went`]).
    pub(super) fn start_thread_action(
        &mut self,
        start: &StartThread,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_start(start.clone(), window, cx);
    }

    /// ⌘K: search everything over whatever has the keyboard, the threads worked in lately
    /// listed before anything is typed; the choice runs once it is gone and the focus is back.
    pub fn open_palette(&mut self, _: &OpenPalette, window: &mut Window, cx: &mut Context<Self>) {
        self.open_search("", window, cx);
    }

    /// ⌘⇧P: the same search with `>` typed, so it lists the commands alone.
    pub fn open_commands(&mut self, _: &OpenCommands, window: &mut Window, cx: &mut Context<Self>) {
        self.open_search(">", window, cx);
    }

    /// The workspace's own palette, its field starting at `typed`.
    fn open_search(&mut self, typed: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            return;
        }
        let items = self.offered_lines(window, cx);
        let recent = self.recent_thread_lines(cx);
        let theme = self.theme.clone();
        let palette = cx.new(|cx| {
            let mut p = CommandPalette::search(items, SEARCH_EVERYTHING, theme, window, cx);
            p.set_brief(true, cx);
            p.set_recent(recent, cx);
            p.set_live(true);
            if !typed.is_empty() {
                p.seed(typed, window, cx);
            }
            p
        });
        self.show_palette(palette, window, cx);
    }

    /// The threads worked in lately across the linked machines, newest first, that no tile
    /// shows: an empty search lists them so going back to one is a key away. A subagent's
    /// thread is its parent's, so it is left out.
    pub(super) fn recent_thread_lines(&self, cx: &App) -> Vec<PaletteItem> {
        let shown: HashSet<_> = self
            .layout
            .tiles()
            .filter_map(|tile| match self.item(tile).map(|i| &i.kind) {
                Some(ItemKind::Thread { thread }) => Some(*thread),
                Some(ItemKind::Terminal { session }) => self.session_thread(*session),
                _ => None,
            })
            .collect();
        let named = self.workers.values().filter(|w| w.link.is_some()).count() > 1;
        let mut rows: Vec<(WorkerKey, &ThreadRow)> = self
            .workers
            .keys()
            .filter_map(|key| Some((*key, self.held_hub(*key)?)))
            .flat_map(|(key, hub)| {
                hub.read(cx).threads().rows().rows.values().map(move |row| (key, row))
            })
            .filter(|(_, row)| row.parent.is_none() && !shown.contains(&row.id))
            .collect();
        rows.sort_by_key(|(_, row)| std::cmp::Reverse(row.updated_ms));
        rows.into_iter()
            .take(RECENT_THREADS)
            .map(|(key, row)| {
                let title = self.thread_title(row.id);
                let place =
                    row.cwd.as_deref().map(|cwd| super::tile::cwd_tail(cwd, self.home_of(key)));
                let mut line =
                    PaletteItem::recent_thread(&title, Some(row.agent.0.as_str()), place, row.id);
                if named {
                    line.worker = self.workers.get(&key).map(|w| w.name.clone());
                }
                line
            })
            .collect()
    }

    /// Focus `worker`'s first tile in reading order; one with no tile gets a shell, and one
    /// this client cannot reach says so.
    pub fn go_to_worker(&mut self, worker: WorkerKey, cx: &mut Context<Self>) {
        if let Some(tile) = self.reading_order().into_iter().find(|t| t.worker == worker) {
            self.focus_tile(tile, cx);
            return;
        }
        let Some(w) = self.workers.get(&worker) else { return };
        if w.link.is_none() {
            let text = format!("{} is {}", w.name, w.status.text());
            self.show_notice(text, cx);
            return;
        }
        self.open_session_on(worker, None, Vec::new(), None, cx);
    }

    /// ⌘P, "Open file…": the palette over the files alone. What is typed is looked up among the
    /// files under the focused tile's directory on its machine (or its home), best first; a
    /// path typed from `/`, `~` or `.` is opened as it is spelled. With nothing typed it lists
    /// the files open in tiles, the latest used first.
    pub fn open_file_palette(&mut self, _: &OpenFile, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            return;
        }
        let mut open: Vec<(usize, PaletteItem)> = self
            .layout
            .tiles()
            .filter_map(|tile| {
                let item = self.item(tile)?;
                let ItemKind::File { .. } = item.kind else { return None };
                let line =
                    PaletteItem::item(&self.tile_title(item), self.kind_glyph(item), item.id)
                        .placed(self.tile_place(item))
                        .on_worker(self.worker_label(tile.worker));
                Some((self.recency_rank(tile), line))
            })
            .collect();
        open.sort_by_key(|(rank, _)| *rank);
        let items = open.into_iter().map(|(_, line)| line).collect();
        let theme = self.theme.clone();
        let palette = cx.new(|cx| {
            let mut p = CommandPalette::search(items, SEARCH_FILES, theme, window, cx);
            p.set_empty(NO_FILE_MATCHES, cx);
            p
        });
        self.show_palette(palette, window, cx);
    }

    /// "Open folder…": the palette, its field holding the focused shell's directory (or the
    /// worker's home), so ↩ opens it and a few keys go elsewhere.
    pub fn open_folder_palette(
        &mut self,
        _: &OpenFolder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.palette.is_some() {
            return;
        }
        let items = self.offered_lines(window, cx);
        let theme = self.theme.clone();
        let seed = self.active_cwd().map_or_else(|| "~/".to_owned(), |cwd| format!("{cwd}/"));
        let palette = cx.new(|cx| {
            let mut p = CommandPalette::new(items, theme, window, cx);
            p.set_brief(true, cx);
            p.set_live(true);
            p.seed(&seed, window, cx);
            p
        });
        self.show_palette(palette, window, cx);
    }

    /// The focused tile's own find-bar query, to start a search from.
    pub(super) fn active_query(&self, cx: &App) -> Query {
        let Some(tile) = self.focused() else { return Query::default() };
        let Some(active) = self.item(tile) else { return Query::default() };
        let query = match &active.kind {
            ItemKind::Terminal { session } => {
                self.terminals.get(session).and_then(|v| v.read(cx).search_query().cloned())
            }
            ItemKind::File { .. } => {
                self.files.get(&active.id).and_then(|v| v.read(cx).query().cloned())
            }
            ItemKind::Window { .. }
            | ItemKind::Display { .. }
            | ItemKind::Browser { .. }
            | ItemKind::Folder { .. }
            | ItemKind::Review { .. }
            | ItemKind::Changes { .. }
            | ItemKind::Thread { .. } => None,
        };
        query.unwrap_or_default()
    }

    /// Keep drawing a dismissed palette for as long as its way out takes, then drop it; a
    /// palette opened meanwhile draws over it and is not dropped with it.
    fn let_palette_leave(&mut self, palette: Entity<CommandPalette>, cx: &mut Context<Self>) {
        let during = palette.update(cx, CommandPalette::leave);
        self.keep_leaving(palette, during, |this| &mut this.palette_leaving, cx);
    }

    /// Close the picker, drawing it for as long as its way out takes.
    pub(super) fn let_picker_leave(&mut self, cx: &mut Context<Self>) {
        if let Some((_, picker)) = self.picker.take() {
            let during = picker.update(cx, WindowPicker::leave);
            self.keep_leaving(picker, during, |this| &mut this.picker_leaving, cx);
        }
    }

    /// Keep `it` in `slot`, drawn as it leaves, for `during`, then drop it; one put there
    /// meanwhile is not dropped with it. Nothing is kept for no time at all.
    pub(super) fn keep_leaving<T: Clone + PartialEq + 'static>(
        &mut self,
        it: T,
        during: std::time::Duration,
        slot: fn(&mut Self) -> &mut Option<T>,
        cx: &Context<Self>,
    ) {
        if during.is_zero() {
            return;
        }
        *slot(self) = Some(it.clone());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(during).await;
            let _gone = this.update(cx, |this, cx| {
                let kept = slot(this);
                if kept.as_ref() == Some(&it) {
                    *kept = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn show_palette(
        &mut self,
        palette: Entity<CommandPalette>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.palette_return = window.focused(cx);
        self.menu = None;
        // A palette opens on an empty field: no ask of the last one's is waited on or shown.
        self.faces.search = super::faces::ThreadSearch::default();
        // The palette is where the human went next: a phone's home or an overlaid navigator
        // over the panes would only sit between it and what it goes to.
        self.nav.open = false;
        let chords = self.hardware_keyboard;
        let phone_below = self.layout.config().phone_below;
        // What the keyboard opens arrives whole; the pointer's open fades in.
        let pointer = !window.last_input_was_keyboard();
        palette.update(cx, |p, _| {
            p.set_chords(chords);
            p.set_sheet_below(phone_below);
            p.set_fades_in(pointer);
        });
        cx.subscribe(&palette, |this, palette, event, cx| {
            if let PaletteEvent::Changed(text) = event {
                this.palette_changed(&palette, text, cx);
                return;
            }
            this.palette = None;
            this.let_palette_leave(palette, cx);
            match event {
                PaletteEvent::Run(PaletteRun::Action(action)) => {
                    // Run from where the keyboard was, once it is back there.
                    this.palette_action = Some(action.boxed_clone());
                }
                PaletteEvent::Run(PaletteRun::Session(session)) => {
                    // The terminal takes the keyboard, not whoever had it before.
                    this.palette_return = None;
                    this.reveal_session(*session, cx);
                }
                PaletteEvent::Run(PaletteRun::Item(item)) => {
                    this.palette_return = None;
                    this.go_to(*item, cx);
                }
                PaletteEvent::Run(PaletteRun::Worker(worker)) => {
                    this.palette_return = None;
                    this.go_to_worker(*worker, cx);
                }
                PaletteEvent::Run(PaletteRun::Wake(worker)) => this.wake_worker(*worker, cx),
                PaletteEvent::Run(PaletteRun::OpenUrl(url)) => {
                    tracing::info!(%url, "opening a forwarded port");
                    slopty_platform::open_url(url);
                }
                PaletteEvent::Run(PaletteRun::OpenInTile(url)) => {
                    this.palette_return = None;
                    this.open_browser(None, url, cx);
                }
                PaletteEvent::Run(PaletteRun::OpenFile { path, line, found }) => {
                    let path = this.absolute_in_active_shell(path);
                    if let Some(key) = this.context_worker() {
                        if *found {
                            // Picked on purpose: kept, not a preview.
                            let _shown = this.show_file(Some(key), &path, *line, cx);
                        } else {
                            this.open_path_on(key, &path, *line, cx);
                        }
                    }
                }
                PaletteEvent::Run(PaletteRun::OpenFolder { path }) => {
                    this.palette_return = None;
                    let path = this.absolute_in_active_shell(path);
                    if let Some(key) = this.context_worker() {
                        this.open_folder_on(key, &path, cx);
                    }
                }
                PaletteEvent::Run(PaletteRun::OpenShell { cwd }) => {
                    if let Some(key) = this.context_worker() {
                        this.open_session_on(key, Some(cwd.clone()), Vec::new(), None, cx);
                    }
                }
                PaletteEvent::Run(PaletteRun::OpenAgent { cwd }) => {
                    if let Some(key) = this.context_worker() {
                        let agent = this.agent_for(key);
                        match agent {
                            Some(agent) => this.start_thread(key, agent, cwd.clone(), None, cx),
                            None => this.show_notice(super::agent_start::NO_AGENT.to_owned(), cx),
                        }
                    }
                }
                PaletteEvent::Run(PaletteRun::Reopen(closing)) => {
                    this.palette_return = None;
                    this.take_back(Some(*closing), cx);
                }
                PaletteEvent::Run(PaletteRun::Project(project)) => {
                    this.palette_return = None;
                    this.open_project(project, cx);
                }
                PaletteEvent::Run(PaletteRun::Group(group)) => {
                    this.palette_return = None;
                    this.go_to_group(group, cx);
                }
                PaletteEvent::Run(PaletteRun::Thread { thread, turn }) => {
                    this.palette_return = None;
                    let opens = crate::authorship::Opens { thread: *thread, turn: *turn };
                    this.open_thread_at(opens, cx);
                }
                PaletteEvent::Dismiss | PaletteEvent::Changed(_) => {}
            }
            cx.notify();
        })
        .detach();
        self.pending_focus_palette = true;
        self.palette = Some(palette);
        cx.notify();
    }

    /// The palette's field changed: a word worth a lookup is asked of the context worker's
    /// files under the focused shell's directory, or the worker's home; and the workspace's
    /// own palette asks every linked worker's threads for the words ([`Self::ask_threads`]), the
    /// session step its machine's past prompts ([`Self::ask_sessions`]), and the folder step the
    /// folder a typed path is in ([`Self::ask_typed_folder`]).
    fn palette_changed(
        &mut self,
        palette: &Entity<CommandPalette>,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(query) = palette::files_query(text)
            && let Some(key) = self.context_worker()
        {
            let root = self.active_cwd().unwrap_or_else(|| "~".to_owned());
            self.send(key, ClientMsg::FindFiles { root, query: query.to_owned() });
        }
        if palette.read(cx).is_live() {
            self.ask_threads(text, cx);
        }
        self.ask_sessions(palette, text, cx);
        self.ask_typed_folder(palette, text);
    }

    /// A worker found files for the palette's text: they are its `Open <path>` lines.
    pub fn files_found(&self, root: &str, query: &str, paths: &[String], cx: &mut Context<Self>) {
        if let Some(palette) = &self.palette {
            palette.update(cx, |p, cx| p.set_found(root, query, paths, cx));
        }
    }

    /// ⌘O asked `key` for its windows: the picker shows at once with the sessions, a row
    /// standing where the windows will be, so the key never seems to do nothing while the
    /// worker answers. A picker already up stays as it is.
    pub(super) fn show_picker_loading(&mut self, key: WorkerKey, cx: &mut Context<Self>) {
        if self.picker.is_some() {
            return;
        }
        let theme = self.theme.clone();
        let sessions = self.session_rows();
        let picker = cx.new(|cx| WindowPicker::loading(sessions, theme, cx));
        self.watch_picker(key, &picker, cx);
    }

    /// `key`'s listing arrived for the picker: it fills the one waiting for it, or opens one.
    pub(super) fn show_picker(
        &mut self,
        key: WorkerKey,
        windows: Vec<WindowInfo>,
        displays: Vec<DisplayInfo>,
        cx: &mut Context<Self>,
    ) {
        if let Some((waiting, picker)) = &self.picker
            && *waiting == key
            && picker.read(cx).is_loading()
        {
            picker.update(cx, |p, cx| p.set_listing(windows, displays, cx));
            return;
        }
        let theme = self.theme.clone();
        let sessions = self.session_rows();
        let picker = cx.new(|cx| WindowPicker::new(sessions, windows, displays, theme, cx));
        self.watch_picker(key, &picker, cx);
    }

    fn watch_picker(
        &mut self,
        key: WorkerKey,
        picker: &Entity<WindowPicker>,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe(picker, move |this, _picker, event, cx| {
            match event {
                PickerEvent::Pick { target, title, .. } => {
                    this.add_screen_item(key, *target, title.clone(), cx);
                }
                PickerEvent::Jump(session) => this.reveal_session(*session, cx),
                PickerEvent::Dismiss => {}
            }
            // A listing still on its way no longer has a picker to fill; it must not open one.
            if let Some(w) = this.workers.get_mut(&key) {
                w.picker_wanted = false;
            }
            this.let_picker_leave(cx);
            // The jump focuses its terminal; every other outcome hands the keyboard back where
            // the focused tile keeps it.
            this.pending_return = !matches!(event, PickerEvent::Jump(_));
            cx.notify();
        })
        .detach();
        let chords = self.hardware_keyboard;
        picker.update(cx, |p, _| p.set_chords(chords));
        self.pending_focus_picker = true;
        self.picker = Some((key, picker.clone()));
        cx.notify();
    }

    /// Where tile `tile` stands in the lists that go to a tile: the latest used first, the one
    /// already focused last, since nobody goes where they are.
    pub(super) fn recency_rank(&self, tile: TileRef) -> usize {
        if self.focused() == Some(tile) {
            return usize::MAX;
        }
        let back = self.recency.iter().rev().position(|id| *id == tile.item);
        back.unwrap_or(usize::MAX - 1)
    }

    /// The terminal sessions for the picker and the palette: agents waiting on the human
    /// first, then by [`Self::recency_rank`].
    pub(super) fn session_rows(&self) -> Vec<SessionRow> {
        // Wall clock, as the worker stamped the start: the summary may be relayed long after.
        let now_ms = super::turns::wall_ms();
        let session_age = |started_ms: u64| {
            (started_ms > 0)
                .then(|| std::time::Duration::from_millis(now_ms.saturating_sub(started_ms)))
        };
        let order = self.reading_order();
        let mut rows: Vec<((bool, usize), SessionRow)> = order
            .iter()
            .filter_map(|tile| {
                let item = self.item(*tile)?;
                let ItemKind::Terminal { session } = item.kind else { return None };
                let agent = self.agent_state(session);
                let needs_you = agent.is_some_and(needs_human);
                let rank = (!needs_you, self.recency_rank(*tile));
                let summary = self.summary(session);
                let row = SessionRow {
                    session,
                    title: self.tile_title(item),
                    status: agent.map(agent_status_text),
                    needs_you,
                    lead: self.kind_glyph(item),
                    mark: agent.map(agent_mark_of),
                    worker: self.worker_label(tile.worker),
                    cwd: self.session_tail(session),
                    age: summary.and_then(|s| session_age(s.started_ms.as_millis())),
                };
                Some((rank, row))
            })
            .collect();
        // Stable: tiles never focused keep their reading order.
        rows.sort_by_key(|(rank, _)| *rank);
        rows.into_iter().map(|(_, row)| row).collect()
    }
}

/// The action a palette line dispatches, when it is one.
fn run_action(line: &PaletteItem) -> Option<&dyn gpui::Action> {
    match &line.run {
        PaletteRun::Action(action) => Some(action.as_ref()),
        _ => None,
    }
}
