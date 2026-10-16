//! The interface's right-pane surfaces and settings pages, answered from the workspace: the
//! calls the shell's changes, files, terminals, screens and settings make of the view model
//! ([`crate::view_model`]).
//!
//! A call is the surfaces' when [`WorkspaceView::view_surface_call`] gives its answer, a value
//! or a watch's stream; the rest go to the chats' calls, and what nobody answers is an unknown
//! method.
//!
//! Each answer is asked of the worker the call names, through its link's own verbs (a folder
//! listed, a file read or written, a git op, an entry moved or trashed, a text search), and its
//! reply comes back through [`WorkspaceView::surface_heard`]. A git op, a change to the files and
//! a search carry a number of the surfaces' own; a read, a listing and a find are told apart by
//! their path, and the workspace's own tiles hear them too.
//!
//! A watch asks nothing while nothing changes. The worker follows what the watches show on the
//! file system's events: the folders of a tree and of the documents read
//! (`ClientMsg::WatchFolders`, answered by a `WorkerMsg::Folder` unasked) and the repositories of
//! a status or a diff (`ClientMsg::WatchRepos`, answered by `WorkerMsg::RepoMoved`), each set
//! whole and sent again when it changes or the link comes back
//! ([`WorkspaceView::surfaces_relinked`]), when what the link missed is asked again once.

mod actions;
mod adding;
mod catalog;
mod changes;
mod clone;
mod composer;
mod devices;

pub use adding::{
    AddAnswer, AddChoice, AddDevice, AddFailure, AddFix, AddKind, AddMark, AddProgress, AddStep,
    Found, FoundNode, Look,
};
mod files;
mod live;
mod projects;
mod settings;
mod sidebar;

use std::collections::{BTreeMap, HashMap};
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;
use std::time::Duration;

use futures::FutureExt as _;
use gpui::{AsyncApp, Context, Task, WeakEntity};
use slopty_client::WorkerKey;
use slopty_proto::file::{FileRead, WriteResult};
use slopty_proto::folder::{FsOp, FsOutcome, Listing};
use slopty_proto::git::{GitOp, GitOutcome};
use slopty_proto::search::{SearchEvent, SearchQuery, SearchRequest};
use slopty_proto::{ClientMsg, RequestId, WorkerMsg};
use slopty_view_rpc::{RpcError, RpcReply, methods};
use tokio::sync::{Notify, mpsc, oneshot};

use super::param;
use crate::workspace::WorkspaceView;

/// The first number the surfaces give a request: far past the counters every other asker on a
/// link starts from one, so an answer is never taken for another's.
const FIRST_REQUEST: RequestId = 1 << 48;

/// How long a worker has to answer a surface before the call fails.
const ANSWER_WITHIN: Duration = Duration::from_secs(30);

/// The paths a find answered, relative to its root.
type FoundPaths = Vec<String>;

/// A worker's home, the folders it found for a new project, and whether it still indexes.
type FoundFolders = (String, Vec<slopty_proto::folder::FolderHit>, bool);

/// What the surfaces wait on from the workers.
#[derive(Default)]
pub(super) struct Surfaces {
    /// The last number given a request.
    last: RequestId,
    /// Git ops on their way, by worker and number.
    git: HashMap<(WorkerKey, RequestId), oneshot::Sender<GitOutcome>>,
    /// Changes to the files on their way.
    fs: HashMap<(WorkerKey, RequestId), oneshot::Sender<FsOutcome>>,
    /// Text searches running, each event sent on as it comes.
    searches: HashMap<(WorkerKey, RequestId), mpsc::UnboundedSender<SearchEvent>>,
    /// Reads waiting, by worker and path as asked.
    reads: HashMap<(WorkerKey, String), Vec<oneshot::Sender<FileRead>>>,
    /// Saves waiting, by worker and path as asked.
    writes: HashMap<(WorkerKey, String), Vec<oneshot::Sender<WriteResult>>>,
    /// Listings waiting, by worker and path as asked.
    listings: HashMap<(WorkerKey, String), Vec<oneshot::Sender<Listing>>>,
    /// Finds waiting, by worker, root and query as asked.
    finds: HashMap<(WorkerKey, String, String), Vec<oneshot::Sender<FoundPaths>>>,
    /// New project's folder finds waiting, by worker and query: the worker's home and the
    /// folders.
    folder_finds: HashMap<(WorkerKey, String), Vec<oneshot::Sender<FoundFolders>>>,
    /// The sides of the recent diffs' files, for their highlighting.
    changes: changes::Kept,
    /// What the files surfaces keep between calls: pictures being read in chunks, the folders
    /// a watch follows.
    files: files::Kept,
    /// The threads the person archived.
    archived: devices::Filed,
    /// The agents the person switched off: the pickers offer them nowhere.
    agents_off: devices::Filed,
    /// The Add device runs under way.
    adding: adding::Adding,
    /// The projects the person added.
    projects: projects::Projects,
    /// The person's own project actions, and the worktree actions waiting for their chats.
    actions: actions::Actions,
    /// The files being attached to a message.
    uploads: composer::Uploads,
    /// The model and effort the person picked for each chat.
    configs: composer::Configs,
    /// The attached files read back, while they are read.
    read_back: composer::ReadBack,
    /// The sidebar's pins and sections.
    sidebar: sidebar::Sidebar,
    /// The last number given a watch.
    last_watch: u64,
    /// The repositories the status and diff watches follow, by worker and folder as named,
    /// with each waiting watch's number and bell.
    repos: BTreeMap<(WorkerKey, String), Vec<Bell>>,
    /// The folders whose repositories each worker was last told to follow.
    repos_sent: HashMap<WorkerKey, Vec<String>>,
    /// The clones on their way ([`clone`]).
    clones: clone::Clones,
}

impl std::fmt::Debug for Surfaces {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Surfaces")
            .field("git", &self.git.len())
            .field("fs", &self.fs.len())
            .field("searches", &self.searches.len())
            .field("reads", &self.reads.len())
            .finish_non_exhaustive()
    }
}

impl Surfaces {
    /// A fresh number for a request.
    const fn next(&mut self) -> RequestId {
        self.last =
            if self.last < FIRST_REQUEST { FIRST_REQUEST } else { self.last.wrapping_add(1) };
        self.last
    }

    /// A fresh number for a watch.
    const fn next_watch(&mut self) -> u64 {
        self.last_watch = self.last_watch.wrapping_add(1);
        self.last_watch
    }

    /// The link to `key` dropped and came back: what was asked on the old one is answered
    /// never, so its askers hear so now, not after [`ANSWER_WITHIN`].
    fn forget_asked(&mut self, key: WorkerKey) {
        self.git.retain(|(k, _), _| *k != key);
        self.fs.retain(|(k, _), _| *k != key);
        self.searches.retain(|(k, _), _| *k != key);
        self.reads.retain(|(k, _), _| *k != key);
        self.writes.retain(|(k, _), _| *k != key);
        self.listings.retain(|(k, _), _| *k != key);
        self.finds.retain(|(k, ..), _| *k != key);
        self.folder_finds.retain(|(k, _), _| *k != key);
        self.clones.forget(key);
    }
}

/// A watch waiting on a repository: its number, and the bell rung when the repository moved.
type Bell = (u64, Rc<Notify>);

/// A watch's stream to the shell, which says when the shell lets it go: its other end, from
/// [`watched`], then resolves.
struct Watched<S> {
    inner: S,
    _gone: futures::channel::oneshot::Sender<()>,
}

impl<S: futures::Stream + Unpin> futures::Stream for Watched<S> {
    type Item = S::Item;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

/// A watch's channel to the shell: where its frames go, the reply that carries them, and what
/// resolves once the shell lets the stream go.
///
/// The last is fused on its own: a bare `oneshot::Receiver` says it is terminated as soon as
/// its sender is dropped, so a `select!` would skip it and never hear the shell let go.
fn watched() -> (
    futures::channel::mpsc::UnboundedSender<serde_json::Value>,
    RpcReply,
    futures::future::Fuse<futures::channel::oneshot::Receiver<()>>,
) {
    let (tx, rx) = futures::channel::mpsc::unbounded::<serde_json::Value>();
    let (gone_tx, gone) = futures::channel::oneshot::channel();
    let stream = Watched { inner: rx, _gone: gone_tx };
    (tx, RpcReply::Stream(Box::pin(stream)), gone.fuse())
}

/// A call's answer as a value.
fn value<T: serde::Serialize>(answer: &T) -> Result<RpcReply, RpcError> {
    RpcReply::value(answer)
}

/// What a call says when its worker went quiet or away before it answered.
const UNANSWERED: &str = "the machine did not answer";

/// The worker went quiet or away before it answered.
fn unanswered() -> RpcError {
    RpcError::Failed(UNANSWERED.to_owned())
}

/// Whether `error` is the worker not answering, which says nothing of what it would have.
fn is_unanswered(error: &RpcError) -> bool {
    matches!(error, RpcError::Failed(said) if said == UNANSWERED)
}

/// Wait for a worker's answer on `rx`, at most [`ANSWER_WITHIN`].
async fn within<T>(cx: &AsyncApp, rx: oneshot::Receiver<T>) -> Result<T, RpcError> {
    let timer = cx.background_executor().timer(ANSWER_WITHIN);
    futures::select_biased! {
        answer = rx.fuse() => answer.map_err(|dropped| {
            tracing::debug!(%dropped, "a surface's request went unanswered");
            unanswered()
        }),
        () = timer.fuse() => Err(unanswered()),
    }
}

/// The workspace is gone: the app is closing.
fn gone(_: impl std::fmt::Debug) -> RpcError {
    RpcError::Closed
}

/// Run `ask` on the workspace and wait for the worker's answer it returns.
async fn ask<T>(
    this: &WeakEntity<WorkspaceView>,
    cx: &mut AsyncApp,
    ask: impl FnOnce(&mut WorkspaceView) -> oneshot::Receiver<T>,
) -> Result<T, RpcError> {
    let rx = this.update(cx, |view, _| ask(view)).map_err(gone)?;
    within(cx, rx).await
}

impl WorkspaceView {
    /// The answer to `method` when a surface makes it; `None` for every other call.
    pub(super) fn view_surface_call(
        &mut self,
        method: &str,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<RpcReply, RpcError>>> {
        if let Some(answer) = self.live_surface_call(method, params, cx) {
            return Some(answer);
        }
        if let Some(answer) = self.composer_call(method, params, cx) {
            return Some(answer);
        }
        let params = params.clone();
        let answer = match method {
            methods::WATCH_CHECKOUT_DIFFS => self.watch_diffs(&params, cx),
            methods::GET_CHECKOUT_DIFF => self.checkout_diff(&params, cx),
            methods::GET_CHECKOUT_FILE_DIFF_TEXT => self.file_diff_text(params, cx),
            methods::LIST_BRANCHES => self.list_branches(&params, cx),
            methods::LIST_FOLDERS => self.list_folders(&params, cx),
            methods::LIST_DRIVES => self.list_drives(&params, cx),
            methods::SEARCH_HOME_FOLDERS => self.search_home_folders(&params, cx),
            methods::LIST_WORKSPACE_DIRECTORY => self.list_directory(params, cx),
            methods::SEARCH_WORKSPACE_FILES => self.search_files(params, cx),
            methods::SEARCH_WORKSPACE_CONTENT => self.search_content(params, cx),
            methods::WARM_WORKSPACE_SEARCH => Task::ready(value(&serde_json::json!({
                "state": slopty_view_proto::WorkspaceSearchIndexState::Ready,
            }))),
            methods::READ_WORKSPACE_FILE => self.read_file(params, cx),
            methods::READ_WORKSPACE_IMAGE => self.read_image(params, cx),
            methods::WRITE_WORKSPACE_FILE => self.write_file(params, cx),
            methods::MOVE_WORKSPACE_ENTRY => self.move_entry(params, cx),
            methods::DELETE_WORKSPACE_ENTRY => self.delete_entry(params, cx),
            methods::WATCH_WORKSPACE_FILES => self.watch_files(&params, cx),
            methods::WATCH_WORKSPACE_GIT_STATUS => self.watch_git_status(&params, cx),
            methods::LIST_HARNESSES
            | methods::SET_HARNESS_ENABLED
            | methods::INSTALL_HARNESS
            | methods::CANCEL_INSTALL
            | methods::WATCH_HARNESS_UPDATES
            | methods::CHECK_HARNESS_UPDATES
            | methods::GET_AGENT_SETTINGS
            | methods::SET_AGENT_SETTINGS => self.settings_call(method, &params, cx)?,
            methods::ADD_DEVICE => self.add_device(&params, cx),
            methods::ANSWER_ADD_DEVICE => Task::ready(self.answer_add_device(&params)),
            methods::LOOK_ON_TAILNET => self.look_on_tailnet(cx),
            methods::DEVICE_ACTION => Task::ready(self.device_action(&params, cx)),
            methods::CLONE_REPO => self.clone_repo(&params, cx),
            _ => return None,
        };
        Some(answer)
    }

    /// A worker's message, heard before the workspace's own handling: an answer to a surface's
    /// request is taken. Whether it was the surfaces' alone, so nothing else is to hear it.
    pub fn surface_heard(&mut self, key: WorkerKey, msg: &WorkerMsg) -> bool {
        if self.clone_heard(key, msg) {
            return true;
        }
        let surfaces = &mut self.view_model.surfaces;
        match msg {
            WorkerMsg::GitDone { request, outcome } => {
                surfaces.git.remove(&(key, *request)).is_some_and(|tx| {
                    let _gone = tx.send(outcome.clone());
                    true
                })
            }
            WorkerMsg::FsDone { request, outcome } => {
                surfaces.fs.remove(&(key, *request)).is_some_and(|tx| {
                    let _gone = tx.send(outcome.clone());
                    true
                })
            }
            WorkerMsg::Search(event) => {
                let id = (key, event.id());
                let Some(tx) = surfaces.searches.get(&id) else { return false };
                let done = !matches!(event, SearchEvent::Hits { .. });
                if tx.send(event.clone()).is_err() || done {
                    surfaces.searches.remove(&id);
                }
                true
            }
            WorkerMsg::File { path, read } => {
                let asked = surfaces.reads.remove(&(key, path.clone())).unwrap_or_default();
                // slopty: a file nobody asked for changed; a page showing it reloads (M).
                #[cfg(target_vendor = "apple")]
                if asked.is_empty() {
                    self.view_model.pages.file_changed(key, path, read);
                }
                for tx in asked {
                    let _gone = tx.send(read.clone());
                }
                false
            }
            WorkerMsg::Written { path, result } => {
                for tx in surfaces.writes.remove(&(key, path.clone())).unwrap_or_default() {
                    let _gone = tx.send(result.clone());
                }
                false
            }
            WorkerMsg::Folder { path, listing } => {
                let asked = surfaces.listings.remove(&(key, path.clone())).unwrap_or_default();
                if asked.is_empty() {
                    self.folder_moved(key, path, listing);
                }
                for tx in asked {
                    let _gone = tx.send(listing.clone());
                }
                false
            }
            WorkerMsg::RepoMoved { repo } => {
                for (_, bell) in surfaces.repos.get(&(key, repo.clone())).into_iter().flatten() {
                    bell.notify_one();
                }
                true
            }
            WorkerMsg::FoundFolders { query, home, folders, indexing } => {
                for tx in surfaces.folder_finds.remove(&(key, query.clone())).unwrap_or_default() {
                    let _gone = tx.send((home.clone(), folders.clone(), *indexing));
                }
                false
            }
            WorkerMsg::FoundFiles { root, query, paths, .. } => {
                let asked = (key, root.clone(), query.clone());
                for tx in surfaces.finds.remove(&asked).unwrap_or_default() {
                    let _gone = tx.send(paths.clone());
                }
                false
            }
            _ => false,
        }
    }

    /// Send `msg` to `key`; whether it went.
    fn surface_send(&self, key: WorkerKey, msg: ClientMsg) -> bool {
        self.workers.get(&key).is_some_and(|worker| worker.send(msg))
    }

    /// Have `bell` rung when any of `repos` moves, in place of what the watch `id` followed.
    fn follow_repos(&mut self, id: u64, bell: &Rc<Notify>, repos: &[(WorkerKey, String)]) {
        let mut keys = self.unfollow_repos_of(id);
        for (key, repo) in repos {
            let watches = self.view_model.surfaces.repos.entry((*key, repo.clone())).or_default();
            watches.push((id, Rc::clone(bell)));
            keys.push(*key);
        }
        keys.sort_unstable();
        keys.dedup();
        for key in keys {
            self.send_repos(key);
        }
    }

    /// The watch `id` follows no repository any more.
    fn unfollow_repos(&mut self, id: u64) {
        for key in self.unfollow_repos_of(id) {
            self.send_repos(key);
        }
    }

    /// Take the watch `id` off every repository; the workers whose repositories it followed.
    fn unfollow_repos_of(&mut self, id: u64) -> Vec<WorkerKey> {
        let mut keys = Vec::new();
        self.view_model.surfaces.repos.retain(|(key, _), watches| {
            let before = watches.len();
            watches.retain(|(watch, _)| *watch != id);
            if watches.len() != before {
                keys.push(*key);
            }
            !watches.is_empty()
        });
        keys
    }

    /// Tell `key` the folders whose repositories the watches follow there, when that moved.
    fn send_repos(&mut self, key: WorkerKey) {
        let surfaces = &self.view_model.surfaces;
        let paths: Vec<String> = surfaces
            .repos
            .keys()
            .filter(|(k, _)| *k == key)
            .map(|(_, repo)| repo.clone())
            .collect();
        // A link told nothing yet follows nothing.
        if surfaces.repos_sent.get(&key).map_or(paths.is_empty(), |sent| *sent == paths) {
            return;
        }
        if self.surface_send(key, ClientMsg::WatchRepos { paths: paths.clone() }) {
            self.view_model.surfaces.repos_sent.insert(key, paths);
        }
    }

    /// The link to `key` came up again: the watches' sets go to the new link, and each watch
    /// looks once at what the old one may have missed.
    pub(in crate::workspace) fn surfaces_relinked(&mut self, key: WorkerKey) {
        let surfaces = &mut self.view_model.surfaces;
        surfaces.forget_asked(key);
        surfaces.repos_sent.remove(&key);
        for ((..), watches) in surfaces.repos.iter().filter(|((k, _), _)| *k == key) {
            for (_, bell) in watches {
                bell.notify_one();
            }
        }
        self.send_repos(key);
        self.trees_relinked(key);
    }

    /// Ask `op` of the repository holding `repo` on `key`.
    fn surface_git(
        &mut self,
        key: WorkerKey,
        repo: String,
        op: GitOp,
    ) -> oneshot::Receiver<GitOutcome> {
        let (tx, rx) = oneshot::channel();
        let request = self.view_model.surfaces.next();
        if self.surface_send(key, ClientMsg::Git { request, repo, op }) {
            self.view_model.surfaces.git.insert((key, request), tx);
        }
        rx
    }

    /// Change the files on `key`.
    fn surface_fs(&mut self, key: WorkerKey, op: FsOp) -> oneshot::Receiver<FsOutcome> {
        let (tx, rx) = oneshot::channel();
        let request = self.view_model.surfaces.next();
        if self.surface_send(key, ClientMsg::FsOp { request, op }) {
            self.view_model.surfaces.fs.insert((key, request), tx);
        }
        rx
    }

    /// Read the file `path` on `key` (slopty: a page reads its changed file again (M)).
    pub(super) fn surface_read(
        &mut self,
        key: WorkerKey,
        path: String,
    ) -> oneshot::Receiver<FileRead> {
        let (tx, rx) = oneshot::channel();
        if self.surface_send(key, ClientMsg::ReadFile { path: path.clone() }) {
            self.view_model.surfaces.reads.entry((key, path)).or_default().push(tx);
        }
        rx
    }

    /// Save `text` as the file `path` on `key`, refused when the file on disk is newer than
    /// `base`.
    fn surface_write(
        &mut self,
        key: WorkerKey,
        path: String,
        text: String,
        base: Option<slopty_core::WallMs>,
    ) -> oneshot::Receiver<WriteResult> {
        let (tx, rx) = oneshot::channel();
        let msg = ClientMsg::WriteFile { path: path.clone(), text, base_modified_ms: base };
        if self.surface_send(key, msg) {
            self.view_model.surfaces.writes.entry((key, path)).or_default().push(tx);
        }
        rx
    }

    /// List the folder `path` on `key`.
    fn surface_list(&mut self, key: WorkerKey, path: String) -> oneshot::Receiver<Listing> {
        let (tx, rx) = oneshot::channel();
        if self.surface_send(key, ClientMsg::ListFolder { path: path.clone() }) {
            self.view_model.surfaces.listings.entry((key, path)).or_default().push(tx);
        }
        rx
    }

    /// The paths under `root` on `key` that `query` matches, best first.
    fn surface_find(
        &mut self,
        key: WorkerKey,
        root: String,
        query: String,
    ) -> oneshot::Receiver<Vec<String>> {
        let (tx, rx) = oneshot::channel();
        let msg = ClientMsg::FindFiles { root: root.clone(), query: query.clone() };
        if self.surface_send(key, msg) {
            self.view_model.surfaces.finds.entry((key, root, query)).or_default().push(tx);
        }
        rx
    }

    /// The folders a new project on `key` may be for `query`, at most `limit`.
    fn surface_find_folders(
        &mut self,
        key: WorkerKey,
        query: String,
        limit: u16,
    ) -> oneshot::Receiver<FoundFolders> {
        let (tx, rx) = oneshot::channel();
        if self.surface_send(key, ClientMsg::FindFolders { query: query.clone(), limit }) {
            self.view_model.surfaces.folder_finds.entry((key, query)).or_default().push(tx);
        }
        rx
    }

    /// Start a text search under `root` on `key`: its number and its events.
    fn surface_search(
        &mut self,
        key: WorkerKey,
        root: String,
        query: SearchQuery,
    ) -> (RequestId, mpsc::UnboundedReceiver<SearchEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let id = self.view_model.surfaces.next();
        if self.surface_send(key, ClientMsg::Search(SearchRequest::Start { id, root, query })) {
            self.view_model.surfaces.searches.insert((key, id), tx);
        }
        (id, rx)
    }

    /// Stop the text search `id` on `key`.
    fn surface_stop_search(&mut self, key: WorkerKey, id: RequestId) {
        if self.view_model.surfaces.searches.remove(&(key, id)).is_some() {
            let _sent = self.surface_send(key, ClientMsg::Search(SearchRequest::Stop { id }));
        }
    }

    /// The worker a call names: its `targetDeviceId`, else this Mac's own worker, which the
    /// shell leaves unnamed.
    fn surface_device(&self, params: &serde_json::Value) -> Option<WorkerKey> {
        match param(params, "targetDeviceId") {
            Some(device) => self.view_worker(device),
            None => self
                .view_model
                .feed
                .as_ref()
                .and_then(|feed| feed.local_worker())
                .and_then(|device| self.view_worker(&device)),
        }
    }

    /// Where a call's workspace is: its worker and the folder its paths are under. A chat's is
    /// where its thread works; a space's is its folder; `checkoutPath` overrides the folder.
    fn surface_place(
        &self,
        params: &serde_json::Value,
        cx: &gpui::App,
    ) -> Result<(WorkerKey, String), RpcError> {
        let place = if let Some(chat) = param(params, "chatId") {
            self.chat_place(chat, cx)
        } else {
            param(params, "spaceId").and_then(|space| self.view_space(space))
        };
        let (key, root) = place.ok_or_else(|| {
            RpcError::BadParams("no machine or folder for this workspace".to_owned())
        })?;
        Ok((key, param(params, "checkoutPath").map_or(root, str::to_owned)))
    }

    /// The worker and folder of the chat `chat`: a minted chat's folder, else where its thread
    /// works (the shell roots a chat's files there and joins its paths to it).
    fn chat_place(&self, chat: &str, cx: &gpui::App) -> Option<(WorkerKey, String)> {
        if let Some(minted) = self.view_model.minted.get(chat) {
            return Some((minted.worker, minted.cwd.clone()));
        }
        let (key, thread) = self.chat_thread_in(chat, cx)?;
        let hub = self.held_hub(key)?.read(cx);
        let row = hub.threads().rows().rows.get(&thread)?;
        let cwd = row.cwd.as_deref().filter(|cwd| !cwd.is_empty());
        Some((key, cwd.or_else(|| crate::view_model::rows::folder(row))?.to_owned()))
    }

    /// The thread a chat shows, and its worker, as [`Self::chat_thread`] finds it.
    fn chat_thread_in(
        &self,
        chat: &str,
        cx: &gpui::App,
    ) -> Option<(WorkerKey, slopty_proto::thread::ThreadId)> {
        let thread = self
            .view_model
            .aliases
            .iter()
            .find(|(_, alias)| alias.as_str() == chat)
            .map(|(thread, _)| *thread)
            .or_else(|| chat.parse().ok())?;
        Some((self.worker_of_thread(thread, cx)?, thread))
    }
}

/// `rel` under `root`, `/` between them: `root` itself for an empty `rel`, `rel` itself when it is
/// absolute.
fn join(root: &str, rel: &str) -> String {
    if absolute(rel) {
        return rel.to_owned();
    }
    let rel = rel.trim_matches('/');
    if rel.is_empty() {
        return root.to_owned();
    }
    if root.is_empty() {
        return rel.to_owned();
    }
    format!("{}/{rel}", root.trim_end_matches('/'))
}

/// Whether `path` is absolute on the worker: from its root or its home.
fn absolute(path: &str) -> bool {
    path.starts_with('/') || path == "~" || path.starts_with("~/")
}
