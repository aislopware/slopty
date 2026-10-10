//! Folder tiles in the workspace: their views, the listings asked for them, and a path opened as
//! whatever it turns out to be.
//!
//! A path whose kind the workspace cannot tell (⌘-click on a path in a running shell, "Open" on a
//! path typed into the palette) is asked of its worker as a folder first: a listing opens a
//! folder tile with it, anything else a file tile. A path that names a line, or ends in `/`,
//! says what it is and opens at once.
//!
//! Files cross to and from other apps on iOS here too: a row an iPad lifts out of a folder tile,
//! or a path a shell printed, is offered as the worker's file ([`WorkspaceView::drag_offers`]),
//! and the Files picker ([`FilesAsk`]) uploads what is picked in it and saves what a row brings
//! down.

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, AppContext as _, Context, Pixels, Point};
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_core::ItemId;
use slopty_platform::file_drop::Dropped;
use slopty_platform::file_drop::out::{self, Fetch, Offer};
use slopty_proto::ClientMsg;
use slopty_proto::folder::{After, FsOp, FsOutcome, Listing};
use slopty_proto::items::{Item, ItemKind, ItemOp};

use super::WorkspaceView;
use crate::folder::{FolderView, FolderViewEvent, UploadFromFiles};

/// What the Files picker is asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum FilesAsk {
    /// Files picked there go up to this tile, as a drop on it.
    Import(TileRef),
    /// Photos and videos picked from Photos go up to this tile, as a drop on it. On the Mac,
    /// whose open panel reaches the Photos library itself, it is the open panel.
    Photos(TileRef),
    /// This worker file or folder comes down, and is saved where the person chooses.
    Export {
        /// The worker it is on.
        worker: WorkerKey,
        /// Its path there.
        path: String,
    },
}

/// Takes the Files picker's asks in place of the system's picker, which no test may show.
#[derive(Clone)]
pub(super) struct FilesSeam(pub Rc<dyn Fn(&FilesAsk)>);

impl gpui::Global for FilesSeam {}

impl WorkspaceView {
    /// A folder tile for `path` on `key`: one already at that folder is focused, else a new
    /// one opens right of the focus. Its item.
    pub fn open_folder_on(
        &mut self,
        key: WorkerKey,
        path: &str,
        cx: &mut Context<Self>,
    ) -> Option<ItemId> {
        let path = folder_path(path);
        let existing = self.workers.get(&key).and_then(|w| {
            w.doc.items().find_map(|i| match &i.kind {
                ItemKind::Folder { path: p } if *p == path => Some(i.id),
                _ => None,
            })
        });
        if let Some(id) = existing {
            self.go_to(id, cx);
            return Some(id);
        }
        self.workers.get(&key)?;
        let item = Item {
            id: ItemId::new(),
            kind: ItemKind::Folder { path: path.clone() },
            name: None,
            facts: std::collections::BTreeMap::new(),
        };
        let id = item.id;
        tracing::info!(%id, %path, "open folder");
        self.propose(key, ItemOp::Add(item), cx);
        cx.notify();
        Some(id)
    }

    /// Open `path` on `key` as what it is: a folder tile for a directory, else a file tile
    /// landing on `line`. A path spelled as either opens at once; any other is asked of the
    /// worker first, and opens when it answers ([`Self::folder_listed`]).
    pub fn open_path_on(
        &mut self,
        key: WorkerKey,
        path: &str,
        line: Option<u32>,
        cx: &mut Context<Self>,
    ) {
        if path.ends_with('/') {
            self.open_folder_on(key, path, cx);
            return;
        }
        let known_file = self.workers.get(&key).is_some_and(|w| {
            w.doc.items().any(|i| matches!(&i.kind, ItemKind::File { path: p } if p == path))
        });
        let linked = self.workers.get(&key).is_some_and(|w| w.link.is_some());
        if line.is_some() || known_file || !linked {
            self.open_file_on(Some(key), path, line, cx);
            return;
        }
        self.probes.push((key, path.to_owned(), line));
        self.send(key, ClientMsg::ListFolder { path: path.to_owned() });
    }

    /// `key` listed `path`: a path asked to learn what it is opens now, every folder tile at
    /// that path shows the listing, and the folder step completes a path typed in it.
    pub fn folder_listed(
        &mut self,
        key: WorkerKey,
        path: &str,
        listing: &Listing,
        cx: &mut Context<Self>,
    ) {
        self.typed_folder_listed(key, path, listing, cx);
        if let Some(at) = self.probes.iter().position(|(k, p, _)| *k == key && p == path) {
            let (_, asked, line) = self.probes.remove(at);
            match listing {
                Listing::Listed { dir, .. } => {
                    // Its view is made now and given the answer at hand, not asked again.
                    let id = self.open_folder_on(key, dir, cx);
                    if let Some(id) = id.filter(|id| !self.folders.contains_key(id)) {
                        self.make_folder(key, id, dir, cx);
                    }
                    if let Some(view) = id.and_then(|id| self.folders.get(&id)).cloned() {
                        view.update(cx, |v, cx| {
                            v.take_request();
                            v.set_listing(dir, listing.clone(), cx);
                        });
                    }
                }
                Listing::NotFolder | Listing::Missing { .. } => {
                    self.open_file_on(Some(key), &asked, line, cx);
                }
            }
        }
        let views: Vec<_> = self
            .workers
            .get(&key)
            .into_iter()
            .flat_map(|w| w.doc.items())
            .filter_map(|item| self.folders.get(&item.id))
            .cloned()
            .collect();
        for view in views {
            view.update(cx, |v, cx| v.set_listing(path, listing.clone(), cx));
        }
        // An item that named its folder by `~` takes the folder's own path, so going up from it
        // works from a path this client can read.
        if let Listing::Listed { dir, .. } = listing
            && dir != path
        {
            let moved: Vec<ItemId> = self
                .workers
                .get(&key)
                .into_iter()
                .flat_map(|w| w.doc.items())
                .filter(|i| matches!(&i.kind, ItemKind::Folder { path: p } if p == path))
                .map(|i| i.id)
                .collect();
            for id in moved {
                if let Some(view) = self.folders.get(&id).cloned() {
                    // Taken as the answer for the new path too: nothing more to ask.
                    view.update(cx, |v, cx| {
                        v.set_path(dir, cx);
                        v.take_request();
                        v.set_listing(dir, listing.clone(), cx);
                    });
                }
                self.propose(key, ItemOp::SetFolder { id, path: dir.clone() }, cx);
            }
        }
        cx.notify();
    }

    /// Views for folder items that have none, each item's path given to its view, and the
    /// listings the views want asked of their linked workers; the views whose items are gone go.
    /// Each linked worker follows the set of its folder tiles' directories and lists one again
    /// when its entries change.
    pub(super) fn reconcile_folders(&mut self, cx: &mut Context<Self>) {
        let folders: Vec<(WorkerKey, ItemId, String)> = self
            .items()
            .filter_map(|(key, item)| match &item.kind {
                ItemKind::Folder { path } => Some((key, item.id, path.clone())),
                _ => None,
            })
            .collect();
        self.folders.retain(|id, _| folders.iter().any(|(_, f, _)| f == id));
        for (key, w) in &mut self.workers {
            if w.link.is_none() {
                continue;
            }
            let mut paths: Vec<String> =
                folders.iter().filter(|(k, ..)| k == key).map(|(.., p)| p.clone()).collect();
            paths.sort_unstable();
            paths.dedup();
            if paths != w.watched_folders {
                w.watched_folders.clone_from(&paths);
                w.send(ClientMsg::WatchFolders { paths });
            }
        }
        for (key, id, path) in folders {
            let Some(w) = self.workers.get(&key) else { continue };
            if w.link.is_none() {
                continue;
            }
            let home = w.home.clone();
            let view = match self.folders.get(&id) {
                Some(view) => view.clone(),
                None => self.make_folder(key, id, &path, cx),
            };
            view.update(cx, |v, cx| {
                v.set_home(home);
                v.set_path(&path, cx);
            });
            // Drawn while the window draws, its notify reaches no observer.
            self.folder_changed(id, cx);
            self.list_folder(key, id, cx);
        }
    }

    /// The view of folder item `id` on `worker`.
    fn make_folder(
        &mut self,
        worker: WorkerKey,
        id: ItemId,
        path: &str,
        cx: &mut Context<Self>,
    ) -> gpui::Entity<FolderView> {
        let theme = self.theme.clone();
        let view = cx.new(|cx| FolderView::new(id, worker, path, theme, cx));
        cx.subscribe(&view, move |this, _view, event, cx| {
            match event {
                FolderViewEvent::Ask(msg) => this.send(worker, msg.clone()),
                FolderViewEvent::Op(op) => this.fs_op(worker, op.clone(), cx),
                FolderViewEvent::Browse(path) => this.browse_folder(worker, id, path, cx),
                FolderViewEvent::OpenFile(path) => this.open_file_beside(id, path, cx),
                FolderViewEvent::DragOut(path) => {
                    this.drag_out(worker, path, cx);
                }
                FolderViewEvent::UploadHere => {
                    if let Some(tile) = this.tile_of(id) {
                        this.ask_files(&FilesAsk::Import(tile), cx);
                    }
                }
                FolderViewEvent::RemoveWorktree(root) => {
                    this.remove_worktree_at(worker, root, cx);
                }
                FolderViewEvent::NewShell(dir) => {
                    this.open_session_on(worker, Some(dir.clone()), Vec::new(), None, cx);
                }
                FolderViewEvent::SaveToFiles { path } => {
                    let ask = FilesAsk::Export { worker, path: path.clone() };
                    this.ask_files(&ask, cx);
                }
            }
            cx.notify();
        })
        .detach();
        cx.observe(&view, move |this, _view, cx| this.folder_changed(id, cx)).detach();
        self.folders.insert(id, view.clone());
        view
    }

    /// Folder item `id` moved to `path`: the registry hears it, and the worker is asked what is
    /// there.
    fn browse_folder(&mut self, worker: WorkerKey, id: ItemId, path: &str, cx: &mut Context<Self>) {
        let moved = self
            .tile_of(id)
            .and_then(|t| self.item(t))
            .is_some_and(|item| !matches!(&item.kind, ItemKind::Folder { path: p } if p == path));
        if moved {
            self.propose(worker, ItemOp::SetFolder { id, path: path.to_owned() }, cx);
        }
        self.list_folder(worker, id, cx);
    }

    /// Ask `worker` for what folder tile `id` wants listed, if it wants anything.
    pub(super) fn list_folder(&self, worker: WorkerKey, id: ItemId, cx: &mut Context<Self>) {
        let Some(view) = self.folders.get(&id) else { return };
        if self.workers.get(&worker).is_none_or(|w| w.link.is_none()) {
            return;
        }
        if let Some(path) = view.update(cx, |v, _| v.take_request()) {
            self.send(worker, ClientMsg::ListFolder { path });
        }
    }

    /// Ask again for what folder tile `id` shows: it took the keyboard, or files went up into
    /// it.
    pub(super) fn refresh_folder(&self, id: ItemId, cx: &mut Context<Self>) {
        let Some(tile) = self.tile_of(id) else { return };
        if let Some(view) = self.folders.get(&id) {
            view.update(cx, |v, _| v.refresh());
        }
        self.list_folder(tile.worker, id, cx);
    }

    /// Ask `worker` to change its files as `op` says; out of reach, nothing is asked and the
    /// person is told.
    pub(super) fn fs_op(&mut self, worker: WorkerKey, op: FsOp, cx: &mut Context<Self>) {
        let Some(w) = self.workers.get_mut(&worker) else { return };
        if !w.is_linked() {
            let name = w.name.clone();
            self.show_notice(format!("{name} is out of reach, so nothing was changed"), cx);
            return;
        }
        let msg = w.fs_ops.ask(op);
        self.send(worker, msg);
    }

    /// `key` answered the change to its files numbered `request`. A trash is said with the
    /// way to put it back; a change refused or failed says why. A change done needs no word:
    /// the folder's tile lists it, the entry selected.
    pub fn fs_done(
        &mut self,
        key: WorkerKey,
        request: slopty_proto::RequestId,
        outcome: FsOutcome,
        cx: &mut Context<Self>,
    ) {
        let Some((op, outcome)) =
            self.workers.get_mut(&key).and_then(|w| w.fs_ops.done(request, outcome))
        else {
            return;
        };
        self.folders_heard(key, &op, matches!(outcome, FsOutcome::Done { .. }), cx);
        let words = slopty_client::folders::sentence(&op, &outcome);
        match (&op, &outcome) {
            (FsOp::Trash { path: was }, FsOutcome::Done { path }) => {
                let back = FsOp::Move { from: path.clone(), to: was.clone() };
                self.show_trashed(key, words, back, cx);
            }
            (_, FsOutcome::Done { .. }) => {}
            (_, FsOutcome::Refused(_)) => self.show_notice(words, cx),
            (_, FsOutcome::Failed { .. }) => self.show_failure(words, cx),
        }
    }

    /// Every folder tile of `key` hears how `op` went: `done`, or refused and drawn as it was.
    pub(super) fn folders_heard(
        &self,
        key: WorkerKey,
        op: &FsOp,
        done: bool,
        cx: &mut Context<Self>,
    ) {
        for view in self.folders_of(key) {
            view.update(cx, |v, cx| v.answered(op, done, cx));
        }
    }

    /// The folder tiles of `key`.
    fn folders_of(&self, key: WorkerKey) -> Vec<gpui::Entity<FolderView>> {
        self.workers
            .get(&key)
            .into_iter()
            .flat_map(|w| w.doc.items())
            .filter_map(|item| self.folders.get(&item.id))
            .cloned()
            .collect()
    }

    /// `key` sent the page of `path` after `after`: every folder tile at `path` takes it.
    pub fn folder_page(
        &self,
        key: WorkerKey,
        path: &str,
        after: &After,
        listing: &Listing,
        cx: &mut Context<Self>,
    ) {
        for view in self.folders_of(key) {
            view.update(cx, |v, cx| v.set_page(path, after, listing.clone(), cx));
        }
    }

    /// A file opened from folder tile `id`: its tile goes beside the folder's by the room rule.
    fn open_file_beside(&mut self, id: ItemId, path: &str, cx: &mut Context<Self>) {
        let Some(tile) = self.tile_of(id) else { return };
        self.layout.focus(tile);
        self.open_file_on(Some(tile.worker), path, None, cx);
    }
}

impl WorkspaceView {
    /// "Upload from Files…" with the keyboard anywhere: the Files picker's files go up to the
    /// focused tile as a drop on it would (a shell's directory, a folder, a window's staging).
    pub fn upload_from_files(
        &mut self,
        _: &UploadFromFiles,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let takes = |kind: &ItemKind| {
            matches!(
                kind,
                ItemKind::Terminal { .. }
                    | ItemKind::Folder { .. }
                    | ItemKind::Window { .. }
                    | ItemKind::Display { .. }
            )
        };
        match self.focused().filter(|tile| self.item(*tile).is_some_and(|i| takes(&i.kind))) {
            Some(tile) => self.ask_files(&FilesAsk::Import(tile), cx),
            None => self.show_notice("Focus a shell or a folder to upload to".to_owned(), cx),
        }
    }

    /// Ask the Files picker, or the seam that stands in for it. Only iOS has the picker; a Mac
    /// asks its open panel for what goes up and its save panel for where a download goes.
    pub(super) fn ask_files(&mut self, ask: &FilesAsk, cx: &mut Context<Self>) {
        if let Some(seam) = cx.try_global::<FilesSeam>().cloned() {
            (seam.0)(ask);
            return;
        }
        #[cfg(target_os = "ios")]
        match ask {
            FilesAsk::Import(tile) => self.pick_files(*tile, cx),
            FilesAsk::Photos(tile) => self.pick_photos(*tile, cx),
            FilesAsk::Export { worker, path } => self.save_to_files(*worker, path, cx),
        }
        #[cfg(not(target_os = "ios"))]
        match ask {
            FilesAsk::Import(tile) | FilesAsk::Photos(tile) => Self::open_files(*tile, cx),
            FilesAsk::Export { worker, path, .. } => {
                self.bring_down_as(*worker, path.clone(), super::remote::Bringing::Download, cx);
            }
        }
    }

    /// Show the system's open panel; what is picked (files and folders) goes up to `tile` as a
    /// drop on it. The files are the person's own, so nothing is deleted after.
    #[cfg(not(target_os = "ios"))]
    fn open_files(tile: TileRef, cx: &Context<Self>) {
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await
                && !paths.is_empty()
            {
                let _gone = this.update(cx, |this, cx| this.drop_files(tile, &paths, cx));
            }
        })
        .detach();
    }

    /// Show the Files picker; what is picked goes up to `tile`.
    #[cfg(target_os = "ios")]
    fn pick_files(&mut self, tile: TileRef, cx: &mut Context<Self>) {
        if !slopty_platform::file_drop::picker::import(Self::picked_to(tile, cx)) {
            self.show_failure("The Files picker could not be shown".to_owned(), cx);
        }
    }

    /// Show the Photos picker; the photos and videos picked go up to `tile`, as files do.
    #[cfg(target_os = "ios")]
    fn pick_photos(&mut self, tile: TileRef, cx: &mut Context<Self>) {
        if !slopty_platform::file_drop::picker::import_photos(Self::picked_to(tile, cx)) {
            self.show_failure("The Photos picker could not be shown".to_owned(), cx);
        }
    }

    /// Where a picker's files land: up to `tile` as a drop on it ([`Self::files_picked`]), or,
    /// with the workspace gone, their landing discarded.
    #[cfg(target_os = "ios")]
    fn picked_to(tile: TileRef, cx: &Context<Self>) -> Rc<dyn Fn(Dropped)> {
        let (view, app) = (cx.entity().downgrade(), cx.to_async());
        Rc::new(move |dropped: Dropped| {
            let mut app = app.clone();
            let landing = dropped.landing.clone();
            if view.update(&mut app, |v, cx| v.files_picked(tile, dropped, cx)).is_err()
                && let Some(landing) = &landing
            {
                slopty_platform::file_drop::discard(landing);
            }
        })
    }

    /// Files the Files picker handed over, landed as a drop's: they go up to `tile` as a drop
    /// on it, and the landing goes when the upload ends. A file that did not come is named.
    pub fn files_picked(&mut self, tile: TileRef, dropped: Dropped, cx: &mut Context<Self>) {
        if !dropped.failed.is_empty() {
            self.show_failure_at(tile, format!("Not sent: {}", dropped.failed.join("; ")), cx);
        }
        if dropped.paths.is_empty() {
            return;
        }
        tracing::info!(%tile.item, files = dropped.paths.len(), "picked files go up");
        self.drop_landing = dropped.landing;
        self.drop_files(tile, &dropped.paths, cx);
    }

    /// "Save to Files": the Files picker asks for a folder, and the worker's `path` comes down
    /// straight into it ([`Self::save_into`]), listed with its progress and its stop.
    #[cfg(target_os = "ios")]
    fn save_to_files(&mut self, worker: WorkerKey, path: &str, cx: &mut Context<Self>) {
        if self.remote(worker).is_none() {
            self.show_notice("The machine is away; nothing was saved".to_owned(), cx);
            return;
        }
        let (view, mut app) = (cx.entity().downgrade(), cx.to_async());
        let source = path.to_owned();
        let chosen = Box::new(move |folder: Option<slopty_platform::file_drop::picker::Scoped>| {
            let Some(folder) = folder else { return };
            let dir = folder.path().to_path_buf();
            let held: Box<dyn std::any::Any> = Box::new(folder);
            let _gone = view.update(&mut app, |v, cx| v.save_into(worker, source, &dir, held, cx));
        });
        if !slopty_platform::file_drop::picker::choose_folder(chosen) {
            self.show_failure("The Files picker could not be shown".to_owned(), cx);
        }
    }

    /// Bring the worker's `source` down into the folder `dir` here, under a name no entry there
    /// has (`name (2).ext` beside one that has it), as a listed download with its stop; `held`
    /// (the folder's security scope) is kept until the download stops writing there.
    #[cfg_attr(
        not(any(target_os = "ios", test)),
        expect(dead_code, reason = "a folder chosen in Files is iOS's; the Mac tests it")
    )]
    pub(super) fn save_into(
        &mut self,
        worker: WorkerKey,
        source: String,
        dir: &Path,
        held: Box<dyn std::any::Any>,
        cx: &mut Context<Self>,
    ) {
        let name = super::remote::worker_name(&source);
        let dest = slopty_platform::web::unique_path(dir, name, Path::exists);
        tracing::info!(%source, dest = %dest.display(), "save to Files");
        let versions = slopty_client::xfer::Versions::new();
        let xfer = slopty_core::XferId::new();
        let down = super::remote::transfers::Down { worker, xfer, source, dest, versions };
        self.bring_down_holding(down, super::remote::Bringing::Files, Some(held), cx);
    }

    /// The worker files under `at` (window points) that a touch held there lifts out to another
    /// app: the row of a folder tile, or a path a shell printed (made absolute against its
    /// directory), as the worker's file. None under anything else, or while the palette is
    /// over the tiles.
    #[must_use]
    pub fn drag_offers(&self, at: Point<Pixels>, cx: &App) -> Vec<Offer> {
        if self.palette.is_some() {
            return Vec::new();
        }
        let Some((tile, true)) = self.under(at) else { return Vec::new() };
        let under = match self.item(tile).map(|i| &i.kind) {
            Some(ItemKind::Folder { .. }) => {
                self.folders.get(&tile.item).and_then(|view| view.read(cx).path_at(at))
            }
            Some(&ItemKind::Terminal { session }) => self
                .terminal(session)
                .and_then(|view| view.read(cx).path_at(at))
                .map(|(path, folder)| (self.absolute_in_session(session, &path), folder)),
            _ => None,
        };
        let Some((path, folder)) = under else { return Vec::new() };
        self.offer(tile.worker, &path, folder).into_iter().collect()
    }

    /// Let files be dragged out of the window to other apps, each found by
    /// [`Self::drag_offers`] under the touch that lifts it. Once, with the app's window.
    #[cfg(target_os = "ios")]
    pub fn offer_drags(window: &gpui::Window, cx: &Context<Self>) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Ok(RawWindowHandle::UiKit(handle)) =
            HasWindowHandle::window_handle(window).map(|h| h.as_raw())
        else {
            return;
        };
        let (view, app) = (cx.entity().downgrade(), cx.to_async());
        let pick: out::Pick = Rc::new(move |x, y| {
            #[expect(clippy::cast_possible_truncation, reason = "points in a window")]
            let at = gpui::point(gpui::px(x as f32), gpui::px(y as f32));
            view.read_with(&app, |v, cx| v.drag_offers(at, cx)).unwrap_or_default()
        });
        out::offer(handle.ui_view, pick);
    }

    /// The worker's file at `path` offered to another app, brought down only when it is asked
    /// for; none while the worker is away.
    fn offer(&self, worker: WorkerKey, path: &str, folder: bool) -> Option<Offer> {
        let remote = self.workers.get(&worker).and_then(|w| w.link.as_ref()?.remote.clone())?;
        let source = path.to_owned();
        let fetch: Fetch = Arc::new(move |into: &Path| {
            let landed = remote
                .download(slopty_client::xfer::Download::new(
                    slopty_core::XferId::new(),
                    source.clone(),
                    into.to_path_buf(),
                ))
                .map_err(|e| e.to_string())?;
            out::landed_top(&landed, into)
        });
        Offer::of(path, folder, fetch)
    }
}

/// A folder's path as its item keeps it: without a trailing `/`, except the root's.
fn folder_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() && path.starts_with('/') { "/".to_owned() } else { trimmed.to_owned() }
}
