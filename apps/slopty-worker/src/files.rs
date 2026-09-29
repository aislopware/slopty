//! The files a client reads, writes and watches, answered on tasks of their own: a read, a
//! quick-open walk or a look at the watched files touches the disk, and none of it may hold up a
//! terminal's echo on the same connection. A text too big for the control stream goes on a
//! bulk stream after its announcement (`slopty_worker::file::announce`).

use std::collections::HashMap;

use slopty_core::{ClientId, WallMs};
use slopty_net::{Connection, NetError, WorkerMsg};
use slopty_proto::file::{FileRead, WriteResult};
use slopty_proto::transfer::{BulkHeader, Purpose};
use slopty_worker::file::Rewrite;
use tokio::sync::{mpsc, watch};

/// The daemon's handoffs, which say whether a save goes in place.
type Handoffs = std::sync::Arc<parking_lot::Mutex<slopty_worker::handoff::Handoffs>>;

/// Paths the palette's quick open is answered with at most.
const FILES_LISTED: usize = 8;
/// How often the files behind a client's file tiles are looked at for a change.
const FILES_PERIOD: std::time::Duration = std::time::Duration::from_millis(1000);

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
    };
    tracing::info!(%client, %path, kind, "read file");
    let modified_ms = match &read {
        FileRead::Streamed { modified_ms, .. } => *modified_ms,
        _ => WallMs::ZERO,
    };
    if out.send(WorkerMsg::File { path: path.clone(), read }).await.is_err() {
        return false;
    }
    if let Some((xfer, text)) = stream {
        let header = BulkHeader {
            xfer,
            purpose: Purpose::FileText,
            name: String::new(),
            size: text.len() as u64,
            mtime_ms: modified_ms,
            mode: 0,
            offset: 0,
        };
        let sent = async {
            let mut send = slopty_net::streams::open_bulk(conn, header).await?;
            send.write_all(text.as_bytes()).await.map_err(|e| NetError::stream(&e))?;
            send.finish().map_err(|e| NetError::stream(&e))
        };
        // The client's link says the read broke; the connection's own end is its loop's to see.
        if let Err(e) = sent.await {
            tracing::info!(%client, %path, error = %e, "file text not sent");
        }
    }
    true
}

/// Save a file tile and answer how it went. Every watcher of the file, the writer's own
/// included, then hears the new contents from its next look ([`watch`]). A file a waiting edit
/// shows is written in place, since the program waiting on it may hold it open.
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
    /// A file tile's save ([`write`]).
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
    let paths = if query.is_empty() {
        Vec::new()
    } else {
        let (dir, needle) = (root.clone(), query.clone());
        tokio::task::spawn_blocking(move || {
            let dir = slopty_worker::file::expand_home(std::path::Path::new(&dir));
            slopty_worker::find::matching(&dir, &needle, FILES_LISTED)
        })
        .await
        .unwrap_or_default()
    };
    let _sent = out.send(WorkerMsg::FoundFiles { root, query, paths }).await;
}

/// Watch the files behind a client's file tiles: each list `lists` holds replaces the last, and a
/// file whose stamp moves is read and sent again. Ends with the connection.
pub async fn watch(
    client: ClientId,
    conn: Connection,
    out: mpsc::Sender<WorkerMsg>,
    mut lists: watch::Receiver<Vec<String>>,
) {
    let mut watched: HashMap<String, Option<(u64, u128)>> = HashMap::new();
    let mut tick = tokio::time::interval(FILES_PERIOD);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            changed = lists.changed() => {
                if changed.is_err() {
                    return;
                }
                let paths = lists.borrow_and_update().clone();
                watched = restamp(std::mem::take(&mut watched), paths).await;
                tracing::debug!(%client, files = watched.len(), "watch files");
            }
            _ = tick.tick(), if !watched.is_empty() => {
                if !poll(client, &conn, &out, &mut watched).await {
                    return;
                }
            }
        }
    }
}

/// The new watch list: a path kept keeps its stamp; a new one is stamped as it is now (the
/// tile's own read shows that state).
async fn restamp(
    mut old: HashMap<String, Option<(u64, u128)>>,
    paths: Vec<String>,
) -> HashMap<String, Option<(u64, u128)>> {
    let (kept, new): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| old.contains_key(p));
    let mut watched: HashMap<_, _> =
        kept.into_iter().filter_map(|p| old.remove_entry(&p)).collect();
    let stamped = tokio::task::spawn_blocking(move || {
        new.into_iter()
            .map(|path| {
                let stamp = slopty_worker::file::stamp(std::path::Path::new(&path));
                (path, stamp)
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    watched.extend(stamped);
    watched
}

/// Look at every watched file; one whose stamp moved is read and sent again. `false` when the
/// writer is gone.
async fn poll(
    client: ClientId,
    conn: &Connection,
    out: &mpsc::Sender<WorkerMsg>,
    watched: &mut HashMap<String, Option<(u64, u128)>>,
) -> bool {
    let paths: Vec<String> = watched.keys().cloned().collect();
    let stamps = tokio::task::spawn_blocking(move || {
        paths
            .into_iter()
            .map(|path| {
                let stamp = slopty_worker::file::stamp(std::path::Path::new(&path));
                (path, stamp)
            })
            .collect::<Vec<_>>()
    })
    .await;
    let Ok(stamps) = stamps else { return true };
    for (path, stamp) in stamps {
        let Some(seen) = watched.get_mut(&path) else { continue };
        if *seen == stamp {
            continue;
        }
        *seen = stamp;
        if !send_file(client, conn, out, path).await {
            return false;
        }
    }
    true
}
