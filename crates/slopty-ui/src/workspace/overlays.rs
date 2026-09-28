//! The overlays over the strip: the command palette (and its find in every tile), and the
//! picker of a worker's windows.

use gpui::{AppContext as _, Context, Entity, Window};
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_core::SessionId;
use slopty_proto::ClientMsg;
use slopty_proto::items::ItemKind;
use slopty_proto::screen::{DisplayInfo, WindowInfo};
use slopty_proto::terminal::TermRequest;

use super::actions::{FindEverywhere, ListWorkers, OpenFile, OpenFolder, OpenPalette};
use super::agents::{agent_status_text, needs_human};
use super::tile::kind_icon;
use super::{AGENT_COMMAND, WorkspaceView};
use crate::icons::Status;
use crate::palette::{self, CommandPalette, PaletteEvent, PaletteItem, PaletteRun};
use crate::picker::{PickerEvent, SessionRow, WindowPicker};

/// How many of the run-target shell's last commands the palette offers to run again.
const RERUN_LINES: usize = 5;

impl WorkspaceView {
    /// Every tile in reading order: workspace by workspace, column by column, top to bottom.
    pub(super) fn reading_order(&self) -> Vec<TileRef> {
        let mut tiles: Vec<(slopty_client::layout::Pos, TileRef)> =
            self.layout.tiles().filter_map(|t| self.layout.position(t).map(|p| (p, t))).collect();
        tiles.sort_by_key(|(p, _)| (p.workspace, p.column, p.tile));
        tiles.into_iter().map(|(_, t)| t).collect()
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
                None => super::navigator::slow_rtt(w.rtt).unwrap_or_default(),
            };
            PaletteItem::worker(&w.name, &detail, *key).with_status(health.map(|(mark, _)| mark))
        })
    }

    /// Every line the palette offers: the sessions to go to (agents waiting on the human
    /// first), every other tile, the workers, the last few commands of the shell a "run" would
    /// go to, then every action, then the app's own. The palette groups them into its sections.
    #[must_use]
    pub fn palette_lines(&self, cx: &Context<Self>) -> Vec<PaletteItem> {
        let mut items: Vec<PaletteItem> = self
            .session_rows(cx)
            .into_iter()
            .map(|row| {
                let icon = if row.status.is_some() {
                    crate::icons::IconName::Bot
                } else {
                    crate::icons::IconName::SquareTerminal
                };
                PaletteItem::session(&row.title, row.session)
                    .with_icon(icon)
                    .with_status(row.mark)
                    .on_worker(row.worker)
                    .in_dir(row.cwd)
                    .aged(row.age)
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
            let title = self.tile_title(tile, item, cx);
            let line = PaletteItem::item(&title, kind_icon(item, false), item.id)
                .placed(self.tile_place(item, cx));
            items.push(line.on_worker(self.worker_label(tile.worker)));
        }
        items.extend(self.worker_lines());
        // The shell a "run" would go to: its last few commands, to run again.
        if let Some(shell) = self.run_target()
            && let Some(view) = self.terminals.get(&shell)
        {
            let state = view.read(cx).state();
            items.extend(
                state.recent_commands(RERUN_LINES).iter().map(|c| PaletteItem::rerun(c, shell)),
            );
        }
        items.extend(super::actions::palette_items());
        items.extend(self.palette_extra.iter().cloned());
        items
    }

    /// ⌘⇧P: the command palette over whatever has the keyboard; the choice runs once it is
    /// gone and the focus is back.
    pub fn open_palette(&mut self, _: &OpenPalette, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            return;
        }
        let items = self.palette_lines(cx);
        let theme = self.theme.clone();
        let palette = cx.new(|cx| {
            let mut p = CommandPalette::new(items, theme, window, cx);
            p.set_brief(true);
            p
        });
        self.show_palette(palette, window, cx);
    }

    /// "List workers": the palette with a line per worker, its state on the right.
    pub fn list_workers(&mut self, _: &ListWorkers, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            return;
        }
        let lines = self.worker_lines().collect();
        let theme = self.theme.clone();
        let palette = cx.new(|cx| CommandPalette::new(lines, theme, window, cx));
        self.show_palette(palette, window, cx);
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

    /// "Open a file": the palette, its field ready for a path on the focused tile's worker.
    pub fn open_file_palette(&mut self, _: &OpenFile, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            return;
        }
        let items = self.palette_lines(cx);
        let theme = self.theme.clone();
        let seed = self.active_cwd().map_or_else(|| "~/".to_owned(), |cwd| format!("{cwd}/"));
        let palette = cx.new(|cx| {
            let mut p = CommandPalette::new(items, theme, window, cx);
            p.set_brief(true);
            p.seed(&seed, window, cx);
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
        self.open_file_palette(&OpenFile, window, cx);
    }

    /// ⌘⇧F: the palette as a find in every tile.
    pub fn find_everywhere(
        &mut self,
        _: &FindEverywhere,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.palette.is_some() {
            return;
        }
        let theme = self.theme.clone();
        let seed = match self.active_needle(cx) {
            own if own.is_empty() => self.last_find.clone(),
            own => own,
        };
        let palette = cx.new(|cx| CommandPalette::find(&seed, theme, window, cx));
        self.find_needle = Some(String::new());
        self.find_hits.clear();
        self.show_palette(palette, window, cx);
        if !seed.is_empty() {
            self.find_changed(&seed, cx);
        }
    }

    /// The focused tile's own find-bar needle, to start a find in every tile from.
    fn active_needle(&self, cx: &gpui::App) -> String {
        let Some(tile) = self.focused() else { return String::new() };
        let Some(active) = self.item(tile) else { return String::new() };
        let needle = match &active.kind {
            ItemKind::Terminal { session } => self
                .terminals
                .get(session)
                .and_then(|v| v.read(cx).search_needle().map(str::to_owned)),
            ItemKind::File { .. } => self
                .files
                .get(&active.id)
                .and_then(|v| v.read(cx).search_needle().map(str::to_owned)),
            ItemKind::Note { .. }
            | ItemKind::Window { .. }
            | ItemKind::Display { .. }
            | ItemKind::Browser { .. }
            | ItemKind::Folder { .. } => None,
        };
        needle.unwrap_or_default()
    }

    /// Keep drawing a dismissed palette for as long as its way out takes, then drop it; a
    /// palette opened meanwhile draws over it and is not dropped with it.
    fn let_palette_leave(&mut self, palette: Entity<CommandPalette>, cx: &mut Context<Self>) {
        let during = palette.update(cx, CommandPalette::leave);
        if during.is_zero() {
            return;
        }
        self.palette_leaving = Some(palette.clone());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(during).await;
            let _gone = this.update(cx, |this, cx| {
                if this.palette_leaving.as_ref() == Some(&palette) {
                    this.palette_leaving = None;
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
        // The palette is where the human went next: a drawer or an overlaid navigator over the
        // strip would only sit between it and what it goes to.
        self.nav.open = false;
        let chords = self.hardware_keyboard;
        let phone_below = self.layout.config().phone_below;
        palette.update(cx, |p, _| {
            p.set_chords(chords);
            p.set_sheet_below(phone_below);
        });
        cx.subscribe(&palette, |this, palette, event, cx| {
            if let PaletteEvent::Changed(text) = event {
                if this.find_needle.is_some() {
                    this.find_changed(text, cx);
                } else {
                    this.palette_changed(text);
                }
                return;
            }
            this.palette = None;
            this.let_palette_leave(palette, cx);
            this.find_needle = None;
            this.find_hits.clear();
            match event {
                PaletteEvent::Run(PaletteRun::Action(action)) => {
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
                            this.open_file_on(Some(key), &path, *line, cx);
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
                        let command = vec![AGENT_COMMAND.to_owned()];
                        let title = Some(AGENT_COMMAND.to_owned());
                        this.open_session_on(key, Some(cwd.clone()), command, title, cx);
                    }
                }
                PaletteEvent::Run(PaletteRun::FindIn { session, needle }) => {
                    this.palette_return = None;
                    this.reveal_session(*session, cx);
                    this.pending_find = Some((*session, needle.clone()));
                }
                PaletteEvent::Run(PaletteRun::Rerun { session, command }) => {
                    this.palette_return = None;
                    this.reveal_session(*session, cx);
                    if let Some(view) = this.terminals.get(session).cloned() {
                        let command = command.clone();
                        view.update(cx, |v, cx| v.run_text(command, cx));
                    }
                }
                PaletteEvent::Run(PaletteRun::FindInFile { item, needle }) => {
                    this.palette_return = None;
                    this.go_to(*item, cx);
                    this.pending_find_file = Some((*item, needle.clone()));
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

    /// The find-everywhere field changed: every live shell is asked for the needle (one hit
    /// each is enough: the count is what the line says); the notes and file tiles are counted
    /// here, where their text is.
    fn find_changed(&mut self, text: &str, cx: &mut Context<Self>) {
        let needle = text.trim().to_owned();
        self.find_needle = Some(needle.clone());
        self.find_hits.clear();
        if needle.is_empty() {
            self.refresh_find_lines(cx);
            return;
        }
        needle.clone_into(&mut self.last_find);
        for tile in self.reading_order() {
            let Some(item) = self.item(tile) else { continue };
            let id = item.id;
            let (total, run) = match &item.kind {
                ItemKind::Terminal { session } => {
                    if !self.terminals.contains_key(session) {
                        continue;
                    }
                    self.send(
                        tile.worker,
                        ClientMsg::Term {
                            session: *session,
                            req: TermRequest::Search {
                                needle: needle.clone(),
                                max: 1,
                                regex: false,
                            },
                        },
                    );
                    continue;
                }
                ItemKind::Note { text } => {
                    (crate::file::hit_lines(text, &needle).len(), PaletteRun::Item(id))
                }
                ItemKind::File { .. } => {
                    let Some(view) = self.files.get(&id) else { continue };
                    let total = crate::file::hit_lines(&view.read(cx).text(cx), &needle).len();
                    (total, PaletteRun::FindInFile { item: id, needle: needle.clone() })
                }
                ItemKind::Window { .. }
                | ItemKind::Display { .. }
                | ItemKind::Browser { .. }
                | ItemKind::Folder { .. } => {
                    continue;
                }
            };
            if let Ok(total) = u32::try_from(total) {
                self.find_hits.insert(id, (total, run));
            }
        }
        self.refresh_find_lines(cx);
    }

    /// A shell answered the find-everywhere needle: its line says how many hits it holds.
    pub(super) fn find_answered(
        &mut self,
        session: SessionId,
        needle: &str,
        total: u32,
        cx: &mut Context<Self>,
    ) {
        if self.find_needle.as_deref() != Some(needle) || needle.is_empty() {
            return;
        }
        if !self.terminals.contains_key(&session) {
            return;
        }
        let Some(tile) = self.tile_of_session(session) else { return };
        let run = PaletteRun::FindIn { session, needle: needle.to_owned() };
        self.find_hits.insert(tile.item, (total, run));
        self.refresh_find_lines(cx);
    }

    /// The find-everywhere lines: the tiles with a hit, in reading order, as they are known.
    fn refresh_find_lines(&self, cx: &mut Context<Self>) {
        if self.find_needle.is_none() {
            return;
        }
        let lines: Vec<PaletteItem> = self
            .reading_order()
            .into_iter()
            .filter_map(|tile| {
                let (total, run) = self.find_hits.get(&tile.item)?;
                let item = self.item(tile)?;
                (*total > 0).then(|| {
                    PaletteItem::hits(&self.tile_title(tile, item, cx), *total, run.clone())
                })
            })
            .collect();
        if let Some(palette) = &self.palette {
            palette.update(cx, |p, cx| p.set_lines(lines, cx));
        }
    }

    /// The palette's field changed: a word worth a lookup is asked of the context worker's
    /// files under the focused shell's directory, or the worker's home.
    fn palette_changed(&self, text: &str) {
        if let Some(query) = palette::files_query(text)
            && let Some(key) = self.context_worker()
        {
            let root = self.active_cwd().unwrap_or_else(|| "~".to_owned());
            self.send(key, ClientMsg::FindFiles { root, query: query.to_owned() });
        }
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
        let sessions = self.session_rows(cx);
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
        let sessions = self.session_rows(cx);
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
            this.picker = None;
            // The jump focuses its terminal; every other outcome hands focus back.
            this.pending_focus_self = !matches!(event, PickerEvent::Jump(_));
            cx.notify();
        })
        .detach();
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
    pub(super) fn session_rows(&self, cx: &Context<Self>) -> Vec<SessionRow> {
        // Wall clock, as the worker stamped the start: the summary may be relayed long after.
        let now_ms = super::inbox::wall_ms();
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
                    title: self.tile_title(*tile, item, cx),
                    status: agent.map(agent_status_text),
                    needs_you,
                    mark: agent.and_then(Status::of_agent),
                    worker: self.worker_label(tile.worker),
                    cwd: self.session_tail(session),
                    age: summary.and_then(|s| session_age(s.started_ms)),
                };
                Some((rank, row))
            })
            .collect();
        // Stable: tiles never focused keep their reading order.
        rows.sort_by_key(|(rank, _)| *rank);
        rows.into_iter().map(|(_, row)| row).collect()
    }
}
