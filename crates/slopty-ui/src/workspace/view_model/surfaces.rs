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

mod changes;
mod files;
mod live;
mod settings;

use std::collections::HashMap;
use std::time::Duration;

use futures::FutureExt as _;
use gpui::{AsyncApp, Context, Task, WeakEntity};
use slopty_client::layout::WorkerKey;
use slopty_proto::file::{FileRead, WriteResult};
use slopty_proto::folder::{FsOp, FsOutcome, Listing};
use slopty_proto::git::{GitOp, GitOutcome};
use slopty_proto::search::{SearchEvent, SearchQuery, SearchRequest};
use slopty_proto::{ClientMsg, RequestId, WorkerMsg};
use slopty_view_rpc::{RpcError, RpcReply, methods};
use tokio::sync::{mpsc, oneshot};

use super::param;
use crate::workspace::WorkspaceView;

/// The first number the surfaces give a request: far past the counters every other asker on a
/// link starts from one, so an answer is never taken for another's.
const FIRST_REQUEST: RequestId = 1 << 48;

/// How long a worker has to answer a surface before the call fails.
const ANSWER_WITHIN: Duration = Duration::from_secs(30);

/// The paths a find answered, relative to its root.
type Found = Vec<String>;

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
    finds: HashMap<(WorkerKey, String, String), Vec<oneshot::Sender<Found>>>,
    /// The sides of the recent diffs' files, for their highlighting.
    changes: changes::Kept,
    /// What the files surfaces keep between calls: pictures being read in chunks, the folders
    /// a watch follows.
    files: files::Kept,
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
}

/// A call's answer as a value.
fn value<T: serde::Serialize>(answer: &T) -> Result<RpcReply, RpcError> {
    RpcReply::value(answer)
}

/// The worker went quiet or away before it answered.
fn unanswered() -> RpcError {
    RpcError::Failed("the machine did not answer".to_owned())
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
        let params = params.clone();
        let answer = match method {
            methods::WATCH_CHECKOUT_DIFFS => self.watch_diffs(&params, cx),
            methods::GET_CHECKOUT_DIFF => self.checkout_diff(&params, cx),
            methods::GET_CHECKOUT_FILE_DIFF_TEXT => self.file_diff_text(params, cx),
            methods::LIST_BRANCHES => self.list_branches(&params, cx),
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
            _ => return None,
        };
        Some(answer)
    }

    /// A worker's message, heard before the workspace's own handling: an answer to a surface's
    /// request is taken. Whether it was the surfaces' alone, so nothing else is to hear it.
    pub fn surface_heard(&mut self, key: WorkerKey, msg: &WorkerMsg) -> bool {
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
                for tx in surfaces.reads.remove(&(key, path.clone())).unwrap_or_default() {
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
                for tx in surfaces.listings.remove(&(key, path.clone())).unwrap_or_default() {
                    let _gone = tx.send(listing.clone());
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

    /// Read the file `path` on `key`.
    fn surface_read(&mut self, key: WorkerKey, path: String) -> oneshot::Receiver<FileRead> {
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

    /// The worker the interface's device id names, from a call's `targetDeviceId`.
    fn surface_device(&self, params: &serde_json::Value) -> Option<WorkerKey> {
        param(params, "targetDeviceId").and_then(|device| self.view_worker(device))
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
