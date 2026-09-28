//! The server's one state file: the last-known info of every worker, so a restarted server
//! lists them (as gone) before they re-register.

use std::io;
use std::path::{Path, PathBuf};

use slopty_proto::server::WorkerInfo;
use tokio::sync::watch;

/// The file's name in the server's data directory.
pub const FILE: &str = "workers.json";

/// Where the server's state lives.
///
/// That is `server` in the platform's data directory (`slopty_platform::dirs::data_dir`:
/// `$SLOPTY_DATA_DIR`, `~/Library/Application Support/Slopty` on macOS, the XDG data home on
/// Linux); the `server` level lets a worker and a server share one data directory, as a dev
/// setup does.
#[must_use]
pub fn default_data_dir() -> PathBuf {
    slopty_platform::dirs::data_dir().join("server")
}

/// The state file of one data directory.
#[derive(Clone, Debug)]
pub struct Store {
    path: PathBuf,
}

impl Store {
    /// The store in `dir` (created on the first save).
    #[must_use]
    pub fn in_dir(dir: &Path) -> Self {
        Self { path: dir.join(FILE) }
    }

    /// Its path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The workers it holds: none when there is no file. A file that does not parse is set
    /// aside as `workers.json.bad` and read as none, so a bad write costs the list, not the
    /// server.
    pub async fn load(&self) -> Vec<WorkerInfo> {
        let bytes = match tokio::fs::read(&self.path).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => {
                tracing::warn!(path = %self.path.display(), error = %e, "state unreadable");
                return Vec::new();
            }
        };
        match serde_json::from_slice(&bytes) {
            Ok(workers) => workers,
            Err(e) => {
                let aside = self.path.with_extension("json.bad");
                tracing::warn!(
                    path = %self.path.display(), error = %e, aside = %aside.display(),
                    "state does not parse; starting empty"
                );
                let _moved = tokio::fs::rename(&self.path, &aside).await;
                Vec::new()
            }
        }
    }

    /// Replace the file with `workers` (`slopty_platform::fs::replace`, on a blocking thread),
    /// so a crash leaves the old list or the new one.
    pub async fn save(&self, workers: &[WorkerInfo]) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(workers).map_err(io::Error::other)?;
        let path = self.path.clone();
        let saved = tokio::task::spawn_blocking(move || {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            slopty_platform::fs::replace(&path, &json)
        });
        saved.await.map_err(io::Error::other)?
    }

    /// Save every snapshot `changes` publishes until its sender goes away. Snapshots that
    /// arrive during a write collapse into the latest.
    pub async fn keep(self, mut changes: watch::Receiver<Vec<WorkerInfo>>) {
        while changes.changed().await.is_ok() {
            let workers = changes.borrow_and_update().clone();
            if let Err(e) = self.save(&workers).await {
                tracing::warn!(path = %self.path.display(), error = %e, "state not saved");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use slopty_core::WorkerId;
    use slopty_proto::server::Liveness;

    use super::*;
    use crate::hub::Hub;
    use crate::hub::tests::{caps, registration};

    #[tokio::test]
    async fn a_restarted_server_lists_the_workers_it_knew_as_gone() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::in_dir(&dir.path().join("server"));
        assert!(store.load().await.is_empty(), "no file yet");

        let hub = Hub::new("server".to_owned(), Vec::new());
        let keeper = tokio::spawn(store.clone().keep(hub.persisted()));
        let worker = WorkerId::new();
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let lease =
            hub.register(registration(worker, Vec::new()), [10, 0, 0, 2].into(), tx).unwrap();
        drop(lease);
        let expected = hub.directory();
        drop(hub);
        keeper.await.unwrap();

        let loaded = store.load().await;
        assert_eq!(loaded, expected, "the file holds the last state");
        let restarted = Hub::new("server".to_owned(), loaded);
        let listed = restarted.directory();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].worker, worker);
        assert_eq!(listed[0].liveness, Liveness::Gone);
        assert_eq!(listed[0].address, "10.0.0.2:45550");
        assert_eq!(listed[0].caps, caps());
        let left: Vec<_> = std::fs::read_dir(dir.path().join("server"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, [FILE], "the temporary is renamed");
    }

    #[tokio::test]
    async fn a_corrupt_file_is_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::in_dir(dir.path());
        std::fs::write(store.path(), b"{ not json").unwrap();
        assert!(store.load().await.is_empty());
        assert!(dir.path().join("workers.json.bad").exists());
        assert!(!store.path().exists());
    }
}
