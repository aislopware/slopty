//! The server's state files.
//!
//! They hold the last-known info of every worker, so a restarted server lists them (as gone)
//! before they re-register, the words on their way to the agents (`DeliveryStore`), and the
//! phones it pushes to ([`PushStore`]).
//!
//! Each file is replaced whole ([`slopty_platform::fs::replace`]), so a crash leaves the old
//! one or the new one. One that does not parse is set aside under a name of its own and read
//! as empty; one that cannot be read at all stops the server, which would otherwise write over
//! it.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use slopty_proto::server::WorkerInfo;
use tokio::sync::watch;

use crate::deliver::Kept;
use crate::hub::PushKept;

/// The file's name in the server's data directory.
pub const FILE: &str = "workers.json";
/// The phones the server pushes to, beside [`FILE`].
pub const PUSH_FILE: &str = "push.json";
/// The reports on their way to the agents, beside [`FILE`].
pub const DELIVERIES_FILE: &str = "deliveries.json";
/// How long a keeper waits after a change for more before it writes: an agent's burst of
/// hooks costs one write.
pub const SETTLE: Duration = Duration::from_millis(250);

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
    /// aside as `workers.json.bad-<ms>` and read as none, so a bad write costs the list, not
    /// the server.
    ///
    /// # Errors
    /// The file is there and cannot be read, or cannot be set aside.
    pub async fn load(&self) -> io::Result<Vec<WorkerInfo>> {
        load(&self.path).await
    }

    /// Replace the file with `workers` (`slopty_platform::fs::replace`, on a blocking thread),
    /// so a crash leaves the old list or the new one.
    pub async fn save(&self, workers: &[WorkerInfo]) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(&workers).map_err(io::Error::other)?;
        write(&self.path, json).await
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

/// The reports on their way to the agents ([`DELIVERIES_FILE`]): what waits for each and
/// what was sent and not yet handed over, so a server that restarts, as every update does,
/// loses none and sends the outstanding again.
#[derive(Clone, Debug)]
pub(crate) struct DeliveryStore {
    path: PathBuf,
}

impl DeliveryStore {
    /// The store in `dir` (created on the first save).
    #[must_use]
    pub(crate) fn in_dir(dir: &Path) -> Self {
        Self { path: dir.join(DELIVERIES_FILE) }
    }

    /// Its path.
    #[must_use]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// What it holds: nothing when there is no file. One that does not parse is set aside as
    /// `deliveries.json.bad-<ms>` and read as nothing.
    ///
    /// # Errors
    /// The file is there and cannot be read, or cannot be set aside.
    pub(crate) async fn load(&self) -> io::Result<Kept> {
        load(&self.path).await
    }

    /// Replace the file with `kept`.
    ///
    /// # Errors
    /// The file cannot be written.
    pub(crate) async fn save(&self, kept: &Kept) -> io::Result<()> {
        write(&self.path, serde_json::to_vec(kept).map_err(io::Error::other)?).await
    }
}

/// The phones the server pushes to ([`PUSH_FILE`]).
#[derive(Clone, Debug)]
pub struct PushStore {
    path: PathBuf,
}

impl PushStore {
    /// The store in `dir` (created on the first save).
    #[must_use]
    pub fn in_dir(dir: &Path) -> Self {
        Self { path: dir.join(PUSH_FILE) }
    }

    /// Its devices' path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The phones it holds, with the notes they show and the take-backs they are owed: none
    /// when there is no file. One that does not parse is set aside as `push.json.bad-<ms>` and
    /// read as none.
    ///
    /// # Errors
    /// The file is there and cannot be read, or cannot be set aside.
    pub async fn load(&self) -> io::Result<PushKept> {
        load(&self.path).await
    }

    /// Replace the file with `kept`.
    ///
    /// # Errors
    /// The file cannot be written.
    pub async fn save(&self, kept: &PushKept) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(kept).map_err(io::Error::other)?;
        write(&self.path, json).await
    }

    /// Save every change `changes` says until its sender goes away; changes that arrive during
    /// a write collapse into the latest.
    pub async fn keep(self, mut changes: watch::Receiver<PushKept>) {
        while changes.changed().await.is_ok() {
            let kept = changes.borrow_and_update().clone();
            if let Err(e) = self.save(&kept).await {
                tracing::warn!(path = %self.path.display(), error = %e, "phones not saved");
            }
        }
    }
}

/// What the file at `path` holds: the default when there is none. A file that does not parse
/// is set aside as `<name>.bad-<ms>`, a name no other file had, and read as the default, so a
/// bad write costs its contents, not the server, and every bad file is kept.
async fn load<T: DeserializeOwned + Default>(path: &Path) -> io::Result<T> {
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => return Err(e),
    };
    match serde_json::from_slice(&bytes) {
        Ok(state) => Ok(state),
        Err(e) => {
            let aside = aside(path).await;
            tracing::warn!(
                path = %path.display(), error = %e, aside = %aside.display(),
                "state does not parse; set aside, starting empty"
            );
            tokio::fs::rename(path, &aside).await?;
            Ok(T::default())
        }
    }
}

/// A name beside `path` for a file set aside, taken by nothing yet.
async fn aside(path: &Path) -> PathBuf {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut n = 0_u32;
    loop {
        let suffix = if n == 0 { String::new() } else { format!("-{n}") };
        let candidate = path.with_file_name(format!("{name}.bad-{ms}{suffix}"));
        if !tokio::fs::try_exists(&candidate).await.unwrap_or(false) {
            return candidate;
        }
        n = n.saturating_add(1);
    }
}

/// Replace the file at `path` with `json` (`slopty_platform::fs::replace`, on a blocking
/// thread), so a crash leaves the old contents or the new.
async fn write(path: &Path, json: Vec<u8>) -> io::Result<()> {
    let path = path.to_owned();
    let saved = tokio::task::spawn_blocking(move || {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        slopty_platform::fs::replace(&path, &json)
    });
    saved.await.map_err(io::Error::other)?
}

#[cfg(test)]
mod tests {
    use slopty_core::WorkerId;
    use slopty_proto::server::Liveness;
    use tokio::sync::mpsc;

    use super::*;
    use crate::hub::Hub;
    use crate::hub::tests::{caps, registration};

    #[tokio::test]
    async fn a_restarted_server_lists_the_workers_it_knew_as_gone() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::in_dir(&dir.path().join("server"));
        assert!(store.load().await.unwrap().is_empty(), "no file yet");

        let hub = Hub::new("server".to_owned(), Vec::new());
        let keeper = tokio::spawn(store.clone().keep(hub.persisted()));
        let worker = WorkerId::new();
        let (tx, _rx) = mpsc::channel(8);
        let lease =
            hub.register(registration(worker, Vec::new()), [10, 0, 0, 2].into(), tx).unwrap();
        drop(lease);
        let expected = hub.directory();
        drop(hub);
        keeper.await.unwrap();

        let loaded = store.load().await.unwrap();
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

    /// The phones a server pushes to outlive it.
    #[tokio::test]
    async fn the_phones_outlive_the_server() {
        use slopty_core::ClientId;
        use slopty_proto::push::PushDevice;

        let dir = tempfile::tempdir().unwrap();
        let store = PushStore::in_dir(&dir.path().join("server"));
        assert_eq!(store.load().await.unwrap(), PushKept::default(), "no file yet");
        let hub = Hub::new("server".to_owned(), Vec::new());
        let keeper = tokio::spawn(store.clone().keep(hub.keep_phones(PushKept::default())));
        let (tx, _rx) = mpsc::channel(8);
        let link = hub.number_link();
        let seated = hub.seat(link, "phone".to_owned(), tx);
        let client = ClientId::new();
        let device = PushDevice {
            token: "ab".repeat(32),
            key: [3; 32],
            sandbox: false,
            topic: "dev.aislopware.slopty".to_owned(),
            quiet_ms: 30_000,
        };
        hub.push_device(link, client, Some(device.clone()));
        drop(seated);
        drop(hub);
        keeper.await.unwrap();
        let loaded = store.load().await.unwrap();
        assert_eq!(loaded.devices().get(&client), Some(&device), "the phone is kept");
    }

    fn set_aside(dir: &Path, name: &str) -> Vec<Vec<u8>> {
        let mut bad: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap())
            .filter(|e| e.file_name().to_string_lossy().starts_with(&format!("{name}.bad-")))
            .map(|e| {
                (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap())
            })
            .collect();
        bad.sort();
        bad.into_iter().map(|(_, bytes)| bytes).collect()
    }

    #[tokio::test]
    async fn a_corrupt_file_is_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::in_dir(dir.path());
        std::fs::write(store.path(), b"{ not json").unwrap();
        assert_eq!(store.load().await.unwrap(), Vec::<WorkerInfo>::new());
        assert_eq!(set_aside(dir.path(), FILE), [b"{ not json".to_vec()]);
        assert!(!store.path().exists());
    }

    /// A state file the server cannot read is not a missing one: the server does not start,
    /// rather than write an empty one over it.
    #[tokio::test]
    async fn an_unreadable_state_file_stops_the_server_instead_of_being_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("server");
        std::fs::create_dir_all(data_dir.join(DELIVERIES_FILE)).unwrap();
        let config = crate::Config {
            name: "server".to_owned(),
            quic: "127.0.0.1:0".parse().unwrap(),
            data_dir: data_dir.clone(),
            admission: slopty_net::admission::Admission::default(),
            push: crate::PushConfig::Off,
        };
        let started = crate::Server::start(config).await;
        let Err(crate::ServerError::State { path, .. }) = started else {
            panic!("started over an unreadable file")
        };
        assert_eq!(path, data_dir.join(DELIVERIES_FILE));
        assert!(data_dir.join(DELIVERIES_FILE).is_dir(), "left as it was");
    }
}
