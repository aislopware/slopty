//! The files a client reads and watches, answered on tasks of their own: a read, a quick-open
//! walk or a look at the watched files touches the disk, and none of it may hold up a terminal's
//! echo on the same connection.

use std::collections::HashMap;

use slopty_core::ClientId;
use slopty_net::WorkerMsg;
use slopty_proto::file::FileRead;
use tokio::sync::{mpsc, watch};

/// Paths the palette's quick open is answered with at most.
const FILES_LISTED: usize = 8;
/// How often the files behind a client's file cards are looked at for a change.
const FILES_PERIOD: std::time::Duration = std::time::Duration::from_millis(1000);

/// Read `path` and send what is there; `false` when the writer is gone.
pub async fn send_file(client: ClientId, out: &mpsc::Sender<WorkerMsg>, path: String) -> bool {
    let target = path.clone();
    let read = tokio::task::spawn_blocking(move || {
        slopty_worker::file::read(std::path::Path::new(&target))
    })
    .await
    .unwrap_or_else(|_| FileRead::Missing { error: "read failed".to_owned() });
    let kind = match &read {
        FileRead::Text { .. } => "text",
        FileRead::Binary { .. } => "binary",
        FileRead::Missing { .. } => "missing",
    };
    tracing::info!(%client, %path, kind, "read file");
    out.send(WorkerMsg::File { path, read }).await.is_ok()
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

/// Watch the files behind a client's file cards: each list `lists` holds replaces the last, and a
/// file whose stamp moves is read and sent again. Ends with the connection.
pub async fn watch(
    client: ClientId,
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
                if !poll(client, &out, &mut watched).await {
                    return;
                }
            }
        }
    }
}

/// The new watch list: a path kept keeps its stamp; a new one is stamped as it is now (the
/// card's own read shows that state).
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
        if !send_file(client, out, path).await {
            return false;
        }
    }
    true
}
