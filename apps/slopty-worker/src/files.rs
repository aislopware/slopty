//! The files a client reads, writes, watches and changes, answered on tasks of their own: a
//! read, a quick-open walk, a folder op or a look at the watched files touches the disk, and
//! none of it may hold up a terminal's echo on the same connection. A text or picture too big
//! for the control stream goes on a bulk stream after its announcement
//! (`slopty_worker::file::announce`). The watched files are followed on the kernel's events
//! (`slopty_worker::fswatch`).

use slopty_core::{ClientId, WallMs};
use slopty_net::{Connection, NetError, WorkerMsg};
use slopty_proto::RequestId;
use slopty_proto::file::{FileRead, WriteResult};
use slopty_proto::folder::{After, FsOp, FsOutcome, Listing};
use slopty_proto::git::{GitOp, GitOutcome};
use slopty_proto::transfer::{BulkHeader, Purpose};
use slopty_worker::file::Rewrite;
use tokio::sync::{mpsc, watch};

/// The daemon's handoffs, which say whether a save goes in place.
type Handoffs = std::sync::Arc<parking_lot::Mutex<slopty_worker::handoff::Handoffs>>;

/// Paths the palette's quick open is answered with at most.
const FILES_LISTED: usize = 8;

/// Read `path` and send what is there; `false` when the writer is gone.
///
/// A large text is announced on the control stream and then written to its bulk stream before
/// this returns, so the watch sends one file's text at a time and a file rewritten faster than
/// the link carries it is sent as it is when the last send ends, not once per change.
pub async fn send_file(
    client: ClientId,
    conn: &Connection,
    out: &mpsc::Sender<WorkerMsg>,
    path: String,
) -> bool {
    let target = path.clone();
    let read = tokio::task::spawn_blocking(move || {
        slopty_worker::file::read(std::path::Path::new(&target))
    })
    .await
    .unwrap_or_else(|_| FileRead::Missing { error: "read failed".to_owned() });
    let (read, stream) = slopty_worker::file::announce(read);
    let kind = match &read {
        FileRead::Text { .. } => "text",
        FileRead::Streamed { .. } => "streamed",
        FileRead::Binary { .. } => "binary",
        FileRead::Missing { .. } => "missing",
        FileRead::Absent { .. } => "absent",
        FileRead::TooLarge { .. } => "too large",
        FileRead::Media { .. } => "media",
    };
    tracing::info!(%client, %path, kind, "read file");
    let modified_ms = match &read {
        FileRead::Streamed { modified_ms, .. } => *modified_ms,
        _ => WallMs::ZERO,
    };
    if out.send(WorkerMsg::File { path: path.clone(), read }).await.is_err() {
        return false;
    }
    if let Some((xfer, body)) = stream {
        let header = BulkHeader {
            xfer,
            purpose: Purpose::FileBody,
            name: String::new(),
            size: body.len() as u64,
            mtime_ms: modified_ms,
            mode: 0,
            offset: 0,
        };
        let sent = async {
            let mut send = slopty_net::streams::open_bulk(conn, header).await?;
            send.write_all(&body).await.map_err(|e| NetError::stream(&e))?;
            send.finish().map_err(|e| NetError::stream(&e))
        };
        // The client's link says the read broke; the connection's own end is its loop's to see.
        if let Err(e) = sent.await {
            tracing::info!(%client, %path, error = %e, "file body not sent");
        }
    }
    true
}

/// Save a file tile and answer how it went. Every watcher of the file, the writer's own
/// included, then hears the new contents from the save's own events ([`watch()`]). A file a waiting
/// edit shows is written in place, since the program waiting on it may hold it open.
pub async fn write(
    handoffs: &Handoffs,
    client: ClientId,
    out: &mpsc::Sender<WorkerMsg>,
    path: String,
    text: Vec<u8>,
    base_modified_ms: Option<WallMs>,
) {
    let target = path.clone();
    let bytes = text.len();
    let how = if handoffs.lock().editing(&path) { Rewrite::InPlace } else { Rewrite::Replace };
    let result = tokio::task::spawn_blocking(move || {
        slopty_worker::file::write(std::path::Path::new(&target), &text, base_modified_ms, how)
    })
    .await
    .unwrap_or_else(|_| WriteResult::Failed { error: "write failed".to_owned() });
    let outcome = match &result {
        WriteResult::Saved { .. } => "saved",
        WriteResult::Conflict { .. } => "conflict",
        WriteResult::Failed { .. } => "failed",
    };
    tracing::info!(%client, %path, bytes, outcome, "write file");
    let _sent = out.send(WorkerMsg::Written { path, result }).await;
}

/// A save, a folder op, a git op, or the end of a waiting edit, in the order its client sent
/// them: a file saved and then moved is moved with what was saved, and one saved and then
/// committed is committed as saved.
#[derive(Debug)]
pub enum Save {
    /// A file tile's save ([`write()`]).
    File {
        /// Absolute path on the worker.
        path: String,
        /// The whole new text.
        text: Vec<u8>,
        /// The version the edit started from.
        base_modified_ms: Option<WallMs>,
    },
    /// The person is done with a waiting edit: heard only once the saves before it are on disk,
    /// so the program that waited reads what was saved.
    Edited(slopty_proto::handoff::HandoffReply),
    /// A folder tile's change ([`fs_op`]).
    Fs {
        /// The client's number for it.
        request: RequestId,
        /// What to do.
        op: FsOp,
    },
    /// The person's commit sheet asking of a folder's repository ([`git_op`]).
    Git {
        /// The client's number for it.
        request: RequestId,
        /// A folder in the repository.
        repo: String,
        /// What to do.
        op: GitOp,
    },
}

/// Take one client's saves and edit ends in order until the connection goes and the last one
/// sent is taken. One at a time: a second save of a file never overtakes the first.
pub async fn save_in_order(
    handoffs: Handoffs,
    worker: slopty_worker::Worker,
    client: ClientId,
    out: mpsc::Sender<WorkerMsg>,
    mut saves: mpsc::UnboundedReceiver<Save>,
) {
    while let Some(save) = saves.recv().await {
        match save {
            Save::File { path, text, base_modified_ms } => {
                write(&handoffs, client, &out, path, text, base_modified_ms).await;
            }
            Save::Edited(reply) => handoffs.lock().replied(client, reply),
            Save::Fs { request, op } => fs_op(client, &out, request, op).await,
            // A status, a commit, a review of the changes or a worktree's removal is the
            // disk's, in order with the saves around it; a push or a pull request waits on the
            // network, so it runs beside them, after what came before.
            Save::Git {
                request,
                repo,
                op:
                    op @ (GitOp::Status
                    | GitOp::Commit { .. }
                    | GitOp::Changes { .. }
                    | GitOp::RemoveWorktree),
            } => {
                git_op(&worker, client, &out, request, repo, op).await;
            }
            Save::Git { request, repo, op } => {
                let (out, worker) = (out.clone(), worker.clone());
                tokio::spawn(async move { git_op(&worker, client, &out, request, repo, op).await });
            }
        }
    }
}

/// The directory of every terminal running on `worker`.
async fn terminal_dirs(worker: &slopty_worker::Worker) -> Vec<std::path::PathBuf> {
    worker
        .summaries()
        .await
        .into_iter()
        .filter(|s| matches!(s.state, slopty_proto::terminal::SessionState::Running))
        .filter_map(|s| s.cwd)
        .map(|cwd| slopty_worker::file::expand_home(std::path::Path::new(&cwd)))
        .collect()
}

/// Make, move or trash an entry for a folder tile and answer how it went. The folder tiles that
/// show the folders it changed hear of it from their watch ([`watch_folders`]).
pub async fn fs_op(client: ClientId, out: &mpsc::Sender<WorkerMsg>, request: RequestId, op: FsOp) {
    let what = format!("{op:?}");
    let outcome = tokio::task::spawn_blocking(move || slopty_worker::fsop::apply(&op))
        .await
        .unwrap_or_else(|_| FsOutcome::Failed { error: "folder op failed".to_owned() });
    tracing::info!(%client, request, op = %what, ?outcome, "folder op");
    let _sent = out.send(WorkerMsg::FsDone { request, outcome }).await;
}

/// Do a git op in a folder's repository for the person's commit sheet and answer how it went.
/// A worktree's removal is told where `worker`'s live terminals are, so none loses its folder.
pub async fn git_op(
    worker: &slopty_worker::Worker,
    client: ClientId,
    out: &mpsc::Sender<WorkerMsg>,
    request: RequestId,
    repo: String,
    op: GitOp,
) {
    let what = match &op {
        GitOp::Status => "status",
        GitOp::Commit { .. } => "commit",
        GitOp::Push => "push",
        GitOp::PullRequest { .. } => "pull request",
        GitOp::PullStatus => "pull request status",
        GitOp::Merge { .. } => "merge",
        GitOp::Changes { .. } => "changes",
        GitOp::RemoveWorktree => "remove worktree",
        GitOp::Branches => "branches",
        GitOp::PullComments { .. } => "pull request comments",
        GitOp::Worktrees => "worktrees",
        GitOp::Scripts => "run scripts",
        GitOp::FileDiff { .. } => "file diff",
        GitOp::Blob { .. } => "blob",
        GitOp::PullReview { .. } => "pull request review",
    };
    let terminals = if matches!(op, GitOp::RemoveWorktree | GitOp::Worktrees) {
        terminal_dirs(worker).await
    } else {
        Vec::new()
    };
    let outcome = slopty_worker::repo::commit::apply(
        &slopty_worker::repo::commit::Programs::here(),
        &repo,
        op,
        &terminals,
    )
    .await;
    let done = matches!(outcome, GitOutcome::Done(_));
    tracing::info!(%client, request, %repo, op = what, done, "git op");
    let _sent = out.send(WorkerMsg::GitDone { request, outcome }).await;
}

/// Answer a page of a folder past its first.
pub async fn folder_page(
    client: ClientId,
    out: mpsc::Sender<WorkerMsg>,
    path: String,
    after: After,
) {
    let (dir, from) = (std::path::PathBuf::from(&path), after.clone());
    let listing =
        tokio::task::spawn_blocking(move || slopty_worker::listing::folder_page(&dir, Some(&from)))
            .await
            .unwrap_or_else(|_| Listing::Missing { error: "list failed".to_owned() });
    tracing::info!(%client, %path, after = %after.name, "folder page");
    let _sent = out.send(WorkerMsg::FolderPage { path, after, listing }).await;
}

/// Answer a quick-open query under `root`.
pub async fn find(out: mpsc::Sender<WorkerMsg>, root: String, query: String) {
    let answer = if query.is_empty() {
        slopty_worker::find::Answer::default()
    } else {
        let (dir, needle) = (root.clone(), query.clone());
        tokio::task::spawn_blocking(move || {
            let dir = slopty_worker::file::expand_home(std::path::Path::new(&dir));
            slopty_worker::find::matching(&dir, &needle, FILES_LISTED)
        })
        .await
        .unwrap_or_default()
    };
    let slopty_worker::find::Answer { paths, notice } = answer;
    let _sent = out.send(WorkerMsg::FoundFiles { root, query, paths, notice }).await;
}

/// Watch the files behind a client's file tiles: each list `lists` holds replaces the last, and a
/// file that changes on disk is read and sent again, once per change however many writes it
/// took. A send finishes before the next starts, so a file that changes during one is sent
/// once more as it stands when that one ends. Ends with the connection.
pub async fn watch(
    client: ClientId,
    conn: Connection,
    out: mpsc::Sender<WorkerMsg>,
    lists: watch::Receiver<Vec<String>>,
) {
    let span = tracing::debug_span!("watch files", %client);
    let mut changes = span.in_scope(|| {
        slopty_worker::fswatch::follow(lists, slopty_worker::fswatch::Limits::default())
    });
    while let Some(paths) = changes.next().await {
        for path in paths {
            if !send_file(client, &conn, &out, path).await {
                return;
            }
        }
    }
}

/// Watch the folders behind a client's folder tiles: each list `lists` holds replaces the last,
/// and a folder whose entries change on disk is listed and sent again as `WorkerMsg::Folder`.
/// Ends with the connection.
pub async fn watch_folders(
    client: ClientId,
    out: mpsc::Sender<WorkerMsg>,
    lists: watch::Receiver<Vec<String>>,
) {
    let span = tracing::debug_span!("watch folders", %client);
    let mut changes = span.in_scope(|| {
        slopty_worker::fswatch::follow_folders(lists, slopty_worker::fswatch::Limits::default())
    });
    while let Some(paths) = changes.next().await {
        for path in paths {
            let dir = std::path::PathBuf::from(&path);
            let listing = tokio::task::spawn_blocking(move || slopty_worker::listing::folder(&dir))
                .await
                .unwrap_or_else(|_| Listing::Missing { error: "list failed".to_owned() });
            tracing::debug!(%client, %path, "folder changed");
            if out.send(WorkerMsg::Folder { path, listing }).await.is_err() {
                return;
            }
        }
    }
}

/// Clone `url` into `into` (absolute, or `~/…`) for the person, as `ClientMsg::CloneRepo` asks:
/// how far it has come is told as it moves, a step lost when the client is behind, and how it
/// went once it is done. The clones the server asks for share `cloner`'s turns.
pub async fn clone_repo(
    cloner: slopty_worker::repo::cloning::Cloner,
    client: ClientId,
    out: mpsc::Sender<WorkerMsg>,
    request: RequestId,
    (url, into): (String, String),
) {
    use slopty_proto::cloning::{CloneOutcome, ClonedRepo};
    use slopty_worker::repo::cloning::{Missed, Progress};

    let outcome = match slopty_worker::changes::git() {
        None => CloneOutcome::Refused { why: "git is not on this worker".to_owned() },
        Some(git) => {
            let dest = slopty_worker::file::expand_home(std::path::Path::new(&into));
            let told = out.clone();
            let progress = move |p: Progress| {
                let step = WorkerMsg::RepoCloning { request, phase: p.phase, percent: p.percent };
                let _behind = told.try_send(step);
            };
            match cloner.clone_into(git, &url, &dest, progress).await {
                Ok((path, repo)) => CloneOutcome::Cloned(ClonedRepo {
                    path: path.to_string_lossy().into_owned(),
                    repo,
                }),
                Err(Missed::Refused(why)) => CloneOutcome::Refused { why },
                Err(Missed::Failed(said)) => CloneOutcome::Failed { said },
            }
        }
    };
    let cloned = matches!(outcome, CloneOutcome::Cloned(_));
    tracing::info!(%client, request, %url, %into, cloned, "clone");
    let _sent = out.send(WorkerMsg::RepoCloned { request, outcome }).await;
}
