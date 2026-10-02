//! Search in files, as the workspace holds it: which worker and directory it searches, the
//! surface kept while hidden, and the matches it opens as file tiles.
//!
//! ⌥⌘F (or the palette's "Search in files") searches the directory the focused tile is about:
//! a shell's repository, else its directory; the repository of a shell a file tile sits under,
//! else the file's folder; a folder tile's folder. With none of those it takes the shell a
//! "run" would go to, then the worker's home. The field starts on the focused tile's own find
//! needle when it has one. The surface is kept when it closes, its search stopped, so it opens
//! on the same directory as it was left; another directory or worker gets a new one.

use gpui::{AppContext as _, Context, Entity, FocusHandle, Window};
use slopty_client::layout::WorkerKey;
use slopty_proto::items::ItemKind;
use slopty_proto::search::SearchEvent;

use super::WorkspaceView;
use crate::search::{ProjectSearch, ProjectSearchEvent};

/// The surface and what it hands back to when it closes.
#[derive(Debug, Default)]
pub(super) struct Surface {
    /// The worker it searches on, and the surface, shown or kept.
    kept: Option<(WorkerKey, Entity<ProjectSearch>)>,
    shown: bool,
    /// What had the keyboard when it opened.
    back_to: Option<FocusHandle>,
    focus_pending: bool,
}

/// The palette's name for [`super::actions::SearchInFiles`].
pub(super) const SEARCH_IN_FILES: &str = "Search in files";

impl WorkspaceView {
    /// ⌥⌘F: search the files of the directory the focused tile is about, on its worker.
    pub fn search_in_files(
        &mut self,
        _: &super::actions::SearchInFiles,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.context_worker() else { return };
        let root = self.search_root(key);
        let seed = self.active_needle(cx);
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
            ItemKind::Folder { path } => Some(path.clone()),
            ItemKind::Note { .. }
            | ItemKind::Window { .. }
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
}
