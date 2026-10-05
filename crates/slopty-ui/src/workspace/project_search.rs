//! Search in files, as the workspace holds it: which worker and directory it searches, the
//! surface kept while hidden, and the matches it opens as file tiles.
//!
//! ⌘⇧F (or the palette's "Search in files") searches the directory the focused tile is about:
//! a shell's repository, else its directory; the repository of a shell a file tile sits under,
//! else the file's folder; a folder tile's folder. With none of those it takes the shell a
//! "run" would go to, then the worker's home. The field starts on the focused tile's own find
//! needle when it has one. The surface is kept when it closes, its search stopped, so it opens
//! on the same directory as it was left; another directory or worker gets a new one.
//!
//! With its scope chip on the open tiles, the same query goes to every tile here: each shell
//! is asked for one hit (its count is what the row says), and the file tiles and notes are
//! counted here, where their text is.

use std::collections::HashMap;

use gpui::{AppContext as _, Context, Entity, FocusHandle, Window};
use slopty_client::layout::WorkerKey;
use slopty_core::{ItemId, SessionId};
use slopty_proto::ClientMsg;
use slopty_proto::items::ItemKind;
use slopty_proto::search::SearchEvent;
use slopty_proto::terminal::TermRequest;

use super::WorkspaceView;
use crate::kit::find::Query;
use crate::search::{ProjectSearch, ProjectSearchEvent, TileHit, TileOpen};

/// The surface and what it hands back to when it closes.
#[derive(Debug, Default)]
pub(super) struct Surface {
    /// The worker it searches on, and the surface, shown or kept.
    kept: Option<(WorkerKey, Entity<ProjectSearch>)>,
    shown: bool,
    /// What had the keyboard when it opened.
    back_to: Option<FocusHandle>,
    focus_pending: bool,
    /// The open tiles' search: the pattern the shells were asked, and what each tile holds.
    tiles: TileFind,
}

/// A search of the open tiles under way.
#[derive(Debug, Default)]
struct TileFind {
    /// The pattern the shells were asked ([`Query::source`]), which their answers name.
    asked: Option<String>,
    /// Each tile's count, and how its row goes to it.
    hits: HashMap<ItemId, (u32, TileOpen)>,
}

impl Surface {
    /// How many tiles the open tiles' search holds a count for.
    #[cfg(test)]
    pub(super) fn tile_hits(&self) -> usize {
        self.tiles.hits.len()
    }
}

/// The palette's name for [`super::actions::SearchInFiles`].
pub(super) const SEARCH_IN_FILES: &str = "Search in files";

impl WorkspaceView {
    /// ⌘⇧F: search the files of the directory the focused tile is about, on its worker, or
    /// the open tiles.
    pub fn search_in_files(
        &mut self,
        _: &super::actions::SearchInFiles,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.context_worker() else { return };
        let root = self.search_root(key);
        let seed = self.active_query(cx).needle;
        if !self.search.shown {
            self.search.back_to = window.focused(cx);
        }
        let kept =
            self.search.kept.as_ref().filter(|(k, view)| *k == key && view.read(cx).root() == root);
        let view = if let Some((_, view)) = kept {
            view.clone()
        } else {
            // Another directory or worker: the old surface's search stops, through its own
            // subscription, and a new surface takes its place.
            if let Some((_, old)) = self.search.kept.take() {
                old.update(cx, ProjectSearch::stop);
            }
            let place = self.search_place(key, &root);
            let theme = self.theme.clone();
            let view = cx.new(|cx| ProjectSearch::new(&root, &place, theme, window, cx));
            cx.subscribe(&view, move |this, _view, event, cx| match event {
                ProjectSearchEvent::Send(msg) => this.send(key, msg.clone()),
                ProjectSearchEvent::Open { path, line } => {
                    this.close_search(false, cx);
                    this.open_file_on(Some(key), path, Some(*line), cx);
                }
                ProjectSearchEvent::FindTiles(query) => this.find_tiles(query, cx),
                ProjectSearchEvent::OpenTile { open, query } => {
                    this.close_search(false, cx);
                    this.open_tile_found(*open, query.clone(), cx);
                }
                ProjectSearchEvent::Dismiss => this.close_search(true, cx),
            })
            .detach();
            self.search.kept = Some((key, view.clone()));
            view
        };
        view.update(cx, |v, cx| {
            v.seed(&seed, window, cx);
            v.resume(cx);
        });
        self.menu = None;
        self.nav.open = false;
        self.search.shown = true;
        self.search.focus_pending = true;
        cx.notify();
    }

    /// Hide the surface, its search stopped. The keyboard goes back where it was, unless a
    /// match is opening, whose tile takes it.
    fn close_search(&mut self, give_back: bool, cx: &mut Context<Self>) {
        if !self.search.shown {
            return;
        }
        self.search.shown = false;
        if let Some((_, view)) = &self.search.kept {
            view.update(cx, ProjectSearch::stop);
        }
        self.search.tiles = TileFind::default();
        if !give_back {
            self.search.back_to = None;
        }
        cx.notify();
    }

    /// A page or the end of a text search from `key`, for the surface searching there.
    pub fn search_event(&self, key: WorkerKey, event: SearchEvent, cx: &mut Context<Self>) {
        if let Some((k, view)) = &self.search.kept
            && *k == key
        {
            view.update(cx, |v, cx| v.apply(event, cx));
        }
    }

    /// The surface while it is shown, for the frame to draw over the strip.
    pub(super) fn search_drawn(&self) -> Option<Entity<ProjectSearch>> {
        self.search.kept.as_ref().filter(|_| self.search.shown).map(|(_, view)| view.clone())
    }

    /// The surface's search, shown or kept, for the tests to read.
    #[cfg(test)]
    pub(super) fn search_view(&self) -> Option<Entity<ProjectSearch>> {
        self.search.kept.as_ref().map(|(_, view)| view.clone())
    }

    /// Put the keyboard where the surface's opening or closing left it owed.
    pub(super) fn settle_search_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.search.focus_pending)
            && let Some(view) = self.search_drawn()
        {
            view.update(cx, |v, cx| v.focus(window, cx));
        }
        if !self.search.shown
            && let Some(back) = self.search.back_to.take()
        {
            window.focus(&back, cx);
        }
    }

    /// The directory a search from here looks in: what the focused tile is about, else the
    /// shell a "run" would go to, else `key`'s home.
    pub(super) fn search_root(&self, key: WorkerKey) -> String {
        let about = |session| {
            let summary = self.summary(session)?;
            summary.repo.clone().or_else(|| summary.cwd.clone())
        };
        let focused = self.focused().filter(|t| t.worker == key).and_then(|t| self.item(t));
        let from_tile = focused.and_then(|item| match &item.kind {
            ItemKind::Terminal { session } => about(*session),
            ItemKind::File { path } => self.repo_holding(key, path).or_else(|| {
                path.rsplit_once('/')
                    .map(|(dir, _)| if dir.is_empty() { "/" } else { dir }.to_owned())
            }),
            ItemKind::Folder { path } | ItemKind::Changes { path } => Some(path.clone()),
            ItemKind::Window { .. }
            | ItemKind::Display { .. }
            | ItemKind::Browser { .. }
            | ItemKind::Review { .. }
            | ItemKind::Thread { .. } => None,
        });
        from_tile
            .or_else(|| {
                let shell = self.run_target()?;
                (self.worker_of_session(shell) == Some(key)).then(|| about(shell)).flatten()
            })
            .unwrap_or_else(|| "~".to_owned())
    }

    /// The repository of one of `key`'s shells that `path` is in, the deepest if several.
    fn repo_holding(&self, key: WorkerKey, path: &str) -> Option<String> {
        let worker = self.workers.get(&key)?;
        worker
            .sessions
            .values()
            .filter_map(|s| s.repo.as_deref())
            .filter(|repo| {
                path.strip_prefix(repo.trim_end_matches('/'))
                    .is_some_and(|rest| rest.starts_with('/'))
            })
            .max_by_key(|repo| repo.len())
            .map(str::to_owned)
    }

    /// Where `root` on `key` is, as the surface says it: under the worker's home as `~/…`,
    /// and the worker named once there are several.
    fn search_place(&self, key: WorkerKey, root: &str) -> String {
        let home = self.home_of(key).map(|h| h.trim_end_matches('/')).filter(|h| !h.is_empty());
        let shown = match home.and_then(|h| root.strip_prefix(h)) {
            Some("") => "~".to_owned(),
            Some(rest) if rest.starts_with('/') => format!("~{rest}"),
            _ => root.to_owned(),
        };
        match self.workers.get(&key).filter(|_| self.workers.len() > 1) {
            Some(worker) => format!("{shown} on {}", worker.name),
            None => shown,
        }
    }

    /// Find `query` in every open tile: each live shell is asked for one hit, and the file
    /// tiles and notes are counted here.
    fn find_tiles(&mut self, query: &Query, cx: &mut Context<Self>) {
        let asked = query.source();
        self.search.tiles = TileFind { asked: asked.clone(), hits: HashMap::new() };
        let Some(source) = asked else {
            self.show_tile_hits(cx);
            return;
        };
        // A pattern that does not compile matches nothing here; the shells say the same.
        let matcher = query.matcher().ok().flatten();
        let lines = |text: &str| {
            matcher.as_ref().map_or(0, |m| {
                crate::file::find::rows(text, &crate::file::find::matches(text, m)).len()
            })
        };
        for tile in self.reading_order() {
            let Some(item) = self.item(tile) else { continue };
            let id = item.id;
            let (total, open) = match &item.kind {
                ItemKind::Terminal { session } => {
                    if self.terminals.contains_key(session) {
                        let req =
                            TermRequest::Search { needle: source.clone(), max: 1, regex: true };
                        self.send(tile.worker, ClientMsg::Term { session: *session, req });
                    }
                    continue;
                }
                ItemKind::File { .. } => {
                    let Some(view) = self.files.get(&id) else { continue };
                    (lines(&view.read(cx).text(cx)), TileOpen::File(id))
                }
                ItemKind::Window { .. }
                | ItemKind::Display { .. }
                | ItemKind::Browser { .. }
                | ItemKind::Folder { .. }
                | ItemKind::Review { .. }
                | ItemKind::Changes { .. }
                | ItemKind::Thread { .. } => continue,
            };
            if let Ok(total) = u32::try_from(total) {
                self.search.tiles.hits.insert(id, (total, open));
            }
        }
        self.show_tile_hits(cx);
    }

    /// A shell answered a search: when it was the open tiles' search, its row says how many
    /// hits it holds.
    pub(super) fn tile_answered(
        &mut self,
        session: SessionId,
        needle: &str,
        total: u32,
        cx: &mut Context<Self>,
    ) {
        if self.search.tiles.asked.as_deref() != Some(needle) {
            return;
        }
        let Some(tile) = self.tile_of_session(session) else { return };
        self.search.tiles.hits.insert(tile.item, (total, TileOpen::Session(session)));
        self.show_tile_hits(cx);
    }

    /// The tiles with a hit, in reading order, as far as they are known, to the surface.
    fn show_tile_hits(&self, cx: &mut Context<Self>) {
        let Some((_, view)) = &self.search.kept else { return };
        let hits: Vec<TileHit> = self
            .reading_order()
            .into_iter()
            .filter_map(|tile| {
                let (total, open) = self.search.tiles.hits.get(&tile.item)?;
                let item = self.item(tile)?;
                (*total > 0).then(|| TileHit {
                    title: self.tile_title(item),
                    total: *total,
                    open: *open,
                })
            })
            .collect();
        view.update(cx, |v, cx| v.set_tiles(hits, cx));
    }

    /// Go to a tile the open tiles' search found `query` in, its find bar open on it.
    fn open_tile_found(&mut self, open: TileOpen, query: Query, cx: &mut Context<Self>) {
        match open {
            TileOpen::Session(session) => {
                self.reveal_session(session, cx);
                self.pending_find = Some((session, query));
            }
            TileOpen::File(item) => {
                self.go_to(item, cx);
                self.pending_find_file = Some((item, query));
            }
        }
    }
}
