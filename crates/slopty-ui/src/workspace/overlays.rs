//! The overlays over the strip: the command palette (and its find in every tile), and the
//! picker of a worker's windows.

use gpui::{AppContext as _, Context, Entity, SharedString, Window};
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_core::SessionId;
use slopty_proto::ClientMsg;
use slopty_proto::items::ItemKind;
use slopty_proto::screen::{DisplayInfo, WindowInfo};
use slopty_proto::terminal::TermRequest;

use super::actions::{FindEverywhere, ListWorkers, OpenFile, OpenPalette};
use super::agents::{agent_status_text, needs_human};
use super::tile::kind_name;
use super::{AGENT_COMMAND, WorkspaceView};
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

    /// Every line the palette offers: the sessions to go to (agents waiting on the human
    /// first), the file cards and named tiles, the last few commands of the shell a "run"
    /// would go to, then every action, then the app's own.
    #[must_use]
    pub fn palette_lines(&self, cx: &Context<Self>) -> Vec<PaletteItem> {
        let mut items: Vec<PaletteItem> = self
            .session_rows(cx)
            .into_iter()
            .map(|row| {
                PaletteItem::session(&row.title, &row.status.unwrap_or_default(), row.session)
            })
            .collect();
        // Every file card, and every other tile the human named: a name is a wish to find it
        // again.
        for tile in self.reading_order() {
            let Some(item) = self.item(tile) else { continue };
            match &item.kind {
                ItemKind::Terminal { .. } => {}
                ItemKind::File { .. } => {
                    items.push(PaletteItem::item(
                        &self.card_title(tile, item, cx),
                        "file",
                        item.id,
                    ));
                }
                ItemKind::Window { .. } | ItemKind::Display { .. } | ItemKind::Note { .. } => {
                    if let Some(name) = item.name.as_deref() {
                        items.push(PaletteItem::item(name, kind_name(item), item.id));
                    }
                }
            }
        }
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
        let palette = cx.new(|cx| CommandPalette::new(items, theme, window, cx));
        self.show_palette(palette, window, cx);
    }

    /// "List workers": the palette with a line per worker, its state on the right.
    pub fn list_workers(&mut self, _: &ListWorkers, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            return;
        }
        let lines = self
            .workers
            .iter()
            .map(|(key, w)| PaletteItem::worker(&w.name, &w.status.text(), *key))
            .collect();
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
            p.seed(&seed, window, cx);
            p
        });
        self.show_palette(palette, window, cx);
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
            ItemKind::Note { .. } | ItemKind::Window { .. } | ItemKind::Display { .. } => None,
        };
        needle.unwrap_or_default()
    }

    pub(super) fn show_palette(
        &mut self,
        palette: Entity<CommandPalette>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.palette_return = window.focused(cx);
        self.menu = None;
        self.subscriptions.push(cx.subscribe(&palette, |this, _palette, event, cx| {
            if let PaletteEvent::Changed(text) = event {
                if this.find_needle.is_some() {
                    this.find_changed(text, cx);
                } else {
                    this.palette_changed(text);
                }
                return;
            }
            this.palette = None;
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
                PaletteEvent::Run(PaletteRun::OpenFile { path, line }) => {
                    let path = this.absolute_in_active_shell(path);
                    this.open_file_on(None, &path, *line, cx);
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
        }));
        self.pending_focus_palette = true;
        self.palette = Some(palette);
        cx.notify();
    }

    /// The find-everywhere field changed: every live shell is asked for the needle (one hit
    /// each is enough: the count is what the line says); the notes and file cards are counted
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
                    let lines: Vec<SharedString> = text.lines().map(SharedString::from).collect();
                    (crate::file::find_hits(&lines, &needle).len(), PaletteRun::Item(id))
                }
                ItemKind::File { .. } => {
                    let Some(view) = self.files.get(&id) else { continue };
                    let total = crate::file::find_hits(view.read(cx).lines(), &needle).len();
                    (total, PaletteRun::FindInFile { item: id, needle: needle.clone() })
                }
                ItemKind::Window { .. } | ItemKind::Display { .. } => continue,
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
                    PaletteItem::hits(&self.card_title(tile, item, cx), *total, run.clone())
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

    pub(super) fn show_picker(
        &mut self,
        key: WorkerKey,
        windows: Vec<WindowInfo>,
        displays: Vec<DisplayInfo>,
        cx: &mut Context<Self>,
    ) {
        let theme = self.theme.clone();
        let sessions = self.session_rows(cx);
        let picker = cx.new(|cx| WindowPicker::new(sessions, windows, displays, theme, cx));
        self.subscriptions.push(cx.subscribe(&picker, move |this, _picker, event, cx| {
            match event {
                PickerEvent::Pick { target, title, .. } => {
                    this.add_screen_item(key, *target, title.clone(), cx);
                }
                PickerEvent::Jump(session) => this.reveal_session(*session, cx),
                PickerEvent::Dismiss => {}
            }
            this.picker = None;
            // The jump focuses its terminal; every other outcome hands focus back.
            this.pending_focus_self = !matches!(event, PickerEvent::Jump(_));
            cx.notify();
        }));
        self.pending_focus_picker = true;
        self.picker = Some((key, picker));
    }

    /// The terminal sessions for the picker and the palette: agents waiting on the human
    /// first, then other agents, then plain shells; ties in reading order.
    pub(super) fn session_rows(&self, cx: &Context<Self>) -> Vec<SessionRow> {
        let order = self.reading_order();
        let mut rows: Vec<(u8, usize, SessionRow)> = order
            .iter()
            .enumerate()
            .filter_map(|(at, tile)| {
                let item = self.item(*tile)?;
                let ItemKind::Terminal { session } = item.kind else { return None };
                let agent = self.agent_state(session);
                let needs_you = agent.is_some_and(needs_human);
                let rank = match agent {
                    _ if needs_you => 0,
                    Some(_) => 1,
                    None => 2,
                };
                let row = SessionRow {
                    session,
                    title: self.card_title(*tile, item, cx),
                    status: agent.map(agent_status_text),
                    needs_you,
                };
                Some((rank, at, row))
            })
            .collect();
        rows.sort_by_key(|(rank, at, _)| (*rank, *at));
        rows.into_iter().map(|(_, _, row)| row).collect()
    }
}
