//! The files a client reads, writes and watches, answered on tasks of their own: a read, a
//! quick-open walk or a look at the watched files touches the disk, and none of it may hold up a
//! terminal's echo on the same connection. A text or picture too big for the control stream goes
//! on a bulk stream after its announcement (`slopty_worker::file::announce`). The watched files are
//! followed on the kernel's events (`slopty_worker::fswatch`).

use slopty_core::{ClientId, WallMs};
use slopty_net::{Connection, NetError, WorkerMsg};
use slopty_proto::file::{FileRead, WriteResult};
use slopty_proto::folder::Listing;
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

/// A save, or the end of a waiting edit, in the order its client sent them.
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
}

/// Take one client's saves and edit ends in order until the connection goes and the last one
/// sent is taken. One at a time: a second save of a file never overtakes the first.
pub async fn save_in_order(
    handoffs: Handoffs,
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
        }
    }
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
