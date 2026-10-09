//! The server's state files.
//!
//! They hold the last-known info of every worker, so a restarted server lists them (as gone)
//! before they re-register, every project whole ([`ProjectStore`]), the reports on their way to
//! the agents (`DeliveryStore`), and the phones it pushes to with this install's key for the
//! relay ([`PushStore`]).
//!
//! Each file is replaced whole ([`slopty_platform::fs::replace`]), so a crash leaves the old
//! one or the new one. One that does not parse is set aside under a name of its own and read
//! as empty; one that cannot be read at all stops the server, which would otherwise write over
//! it.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use slopty_proto::server::WorkerInfo;
use slopty_push::relay::InstallKey;
use tokio::sync::{mpsc, watch};

use crate::deliver::Kept;
use crate::hub::Devices;
use crate::project::{Keep, ProjectsFile};

/// The file's name in the server's data directory.
pub const FILE: &str = "workers.json";
/// The projects snapshot's name, beside [`FILE`].
pub const PROJECTS_FILE: &str = "projects.json";
/// The log of project changes past the snapshot, beside it.
pub const PROJECTS_LOG: &str = "projects.log";
/// The phones the server pushes to, beside [`FILE`].
pub const PUSH_FILE: &str = "push.json";
/// This install's key for the relay, beside [`FILE`], readable by its owner only.
pub const PUSH_KEY: &str = "push.key";
/// The reports on their way to the agents, beside [`FILE`].
pub const DELIVERIES_FILE: &str = "deliveries.json";
/// How long the projects keeper waits after a change for more before it writes: an agent's
/// burst of hooks costs one write.
pub const PROJECTS_SETTLE: Duration = Duration::from_millis(250);
/// How far the log grows before the snapshot is written again and the log emptied.
pub const LOG_COMPACT_BYTES: usize = 8 << 20;

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

/// The projects of one data directory: a snapshot ([`PROJECTS_FILE`]) and the changes made
/// since, one JSON line each ([`PROJECTS_LOG`]).
///
/// A change is appended as it is made, after a burst settles ([`PROJECTS_SETTLE`]), so keeping
/// it costs its own bytes and never a copy of every project under the hub's lock. The keeper
/// holds its own replica, built from the changes, and writes it as the new snapshot when the
/// log has grown past [`LOG_COMPACT_BYTES`] and when it stops; the snapshot names the last
/// change it holds, so a log line it holds already is skipped when read.
#[derive(Clone, Debug)]
pub struct ProjectStore {
    path: PathBuf,
    log: PathBuf,
}

/// One line of the log: the change numbered `n`.
#[derive(Deserialize)]
struct Logged {
    n: u64,
    kept: Keep,
}

/// [`Logged`] as it is written.
#[derive(Serialize)]
struct Logging<'k> {
    n: u64,
    kept: &'k Keep,
}

impl ProjectStore {
    /// The store in `dir` (created on the first save).
    #[must_use]
    pub fn in_dir(dir: &Path) -> Self {
        Self { path: dir.join(PROJECTS_FILE), log: dir.join(PROJECTS_LOG) }
    }

    /// Its snapshot's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Its log's path.
    #[must_use]
    pub fn log_path(&self) -> &Path {
        &self.log
    }

    /// The projects it holds: the snapshot (none when there is none; one that does not parse
    /// is set aside as `projects.json.bad-<ms>`) with every change the log holds past it. A
    /// line cut short at the log's end is a write a crash stopped, and is passed over; a bad
    /// line anywhere else sets the log aside as `projects.log.bad-<ms>`, keeping what came
    /// before it.
    ///
    /// # Errors
    /// A file is there and cannot be read, or cannot be set aside.
    pub async fn load(&self) -> io::Result<ProjectsFile> {
        let mut file: ProjectsFile = load(&self.path).await?;
        let text = match tokio::fs::read(&self.log).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(file),
            Err(e) => return Err(e),
        };
        let mut lines = text.split(|b| *b == b'\n').filter(|l| !l.is_empty()).peekable();
        while let Some(line) = lines.next() {
            let last = lines.peek().is_none();
            match serde_json::from_slice::<Logged>(line) {
                Ok(logged) if logged.n <= file.through => {}
                Ok(logged) if logged.n == file.through.saturating_add(1) => {
                    file.apply(&logged.kept);
                }
                Ok(_) | Err(_) if last && !text.ends_with(b"\n") => break,
                Ok(_) | Err(_) => {
                    let aside = aside(&self.log).await;
                    tracing::warn!(
                        path = %self.log.display(), aside = %aside.display(), through = file.through,
                        "projects log breaks off; set aside, keeping the changes before"
                    );
                    tokio::fs::rename(&self.log, &aside).await?;
                    break;
                }
            }
        }
        Ok(file)
    }

    /// Write `projects` as the snapshot, compact, and empty the log: everything it held is in
    /// the snapshot now.
    ///
    /// # Errors
    /// The snapshot could not be written; the log is left as it was then.
    pub async fn save(&self, projects: &ProjectsFile) -> io::Result<()> {
        write(&self.path, serde_json::to_vec(projects).map_err(io::Error::other)?).await?;
        match tokio::fs::remove_file(&self.log).await {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Keep every change `changes` brings (`crate::Hub::keep_projects`), starting from `file`
    /// as it was loaded, until the hub stops sending them: appended to the log once a burst
    /// settles, the snapshot written again when the log has grown and at the end.
    pub async fn keep(self, mut file: ProjectsFile, mut changes: mpsc::UnboundedReceiver<Keep>) {
        // What was loaded becomes the snapshot, so the log starts empty.
        self.saved(&file).await;
        let mut logged = 0_usize;
        while let Some(first) = changes.recv().await {
            tokio::time::sleep(PROJECTS_SETTLE).await;
            let mut lines = Vec::new();
            let mut next = Some(first);
            while let Some(kept) = next.take().or_else(|| changes.try_recv().ok()) {
                let n = file.through.saturating_add(1);
                match serde_json::to_vec(&Logging { n, kept: &kept }) {
                    Ok(line) => {
                        lines.extend(line);
                        lines.push(b'\n');
                    }
                    Err(e) => tracing::warn!(error = %e, "a project change not kept"),
                }
                file.apply(&kept);
            }
            logged = logged.saturating_add(lines.len());
            if let Err(e) = append(&self.log, lines).await {
                tracing::warn!(path = %self.log.display(), error = %e, "project changes not kept");
            }
            if logged > LOG_COMPACT_BYTES {
                self.saved(&file).await;
                logged = 0;
            }
        }
        self.saved(&file).await;
    }

    async fn saved(&self, file: &ProjectsFile) {
        if let Err(e) = self.save(file).await {
            tracing::warn!(path = %self.path.display(), error = %e, "projects not saved");
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

/// The phones the server pushes to ([`PUSH_FILE`]) and this install's key for the relay
/// ([`PUSH_KEY`]), made on first use and never written again.
#[derive(Clone, Debug)]
pub struct PushStore {
    path: PathBuf,
    key: PathBuf,
}

impl PushStore {
    /// The store in `dir` (created on the first save).
    #[must_use]
    pub fn in_dir(dir: &Path) -> Self {
        Self { path: dir.join(PUSH_FILE), key: dir.join(PUSH_KEY) }
    }

    /// Its devices' path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The phones it holds: none when there is no file. One that does not parse is set aside
    /// as `push.json.bad-<ms>` and read as none.
    ///
    /// # Errors
    /// The file is there and cannot be read, or cannot be set aside.
    pub async fn load(&self) -> io::Result<Devices> {
        load(&self.path).await
    }

    /// Replace the file with `devices`.
    ///
    /// # Errors
    /// The file cannot be written.
    pub async fn save(&self, devices: &Devices) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(devices).map_err(io::Error::other)?;
        write(&self.path, json).await
    }

    /// Save every change `changes` says until its sender goes away; changes that arrive during
    /// a write collapse into the latest.
    pub async fn keep(self, mut changes: watch::Receiver<Devices>) {
        while changes.changed().await.is_ok() {
            let devices = changes.borrow_and_update().clone();
            if let Err(e) = self.save(&devices).await {
                tracing::warn!(path = %self.path.display(), error = %e, "phones not saved");
            }
        }
    }

    /// This install's key: the one kept, else a new one, kept from now on where only this
    /// user reads it. One that is not a key is set aside as `push.key.bad-<ms>` and replaced,
    /// which costs the relay's binding of each phone to it a slot.
    ///
    /// # Errors
    /// The file cannot be read, set aside or written, or the system's random source fails.
    pub async fn install_key(&self) -> io::Result<InstallKey> {
        match tokio::fs::read(&self.key).await {
            Ok(bytes) => {
                if let Ok(bytes) = <[u8; 32]>::try_from(bytes.as_slice()) {
                    return Ok(InstallKey::from_bytes(&bytes));
                }
                let aside = aside(&self.key).await;
                tracing::warn!(aside = %aside.display(), "the relay key is not one; set aside");
                tokio::fs::rename(&self.key, &aside).await?;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let key = InstallKey::generate().map_err(io::Error::other)?;
        let (path, bytes) = (self.key.clone(), key.to_bytes());
        let made = tokio::task::spawn_blocking(move || {
            use std::io::Write as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut file =
                std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
            file.write_all(&bytes)?;
            file.sync_all()
        });
        made.await.map_err(io::Error::other)??;
        Ok(key)
    }
}

/// Append `bytes` to the file at `path` and flush them to the disk, on a blocking thread.
async fn append(path: &Path, bytes: Vec<u8>) -> io::Result<()> {
    use std::io::Write as _;
    let path = path.to_owned();
    let appended = tokio::task::spawn_blocking(move || {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        file.write_all(&bytes)?;
        file.sync_data()
    });
    appended.await.map_err(io::Error::other)?
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

    /// The phones a server pushes to outlive it, and its key for the relay is made once,
    /// readable by its owner only, and the same on every start; one that is not a key is set
    /// aside for a new one.
    #[tokio::test]
    async fn the_phones_and_the_relay_key_outlive_the_server() {
        use std::os::unix::fs::PermissionsExt as _;

        use slopty_core::ClientId;
        use slopty_proto::push::PushDevice;

        let dir = tempfile::tempdir().unwrap();
        let store = PushStore::in_dir(&dir.path().join("server"));
        assert!(store.load().await.unwrap().is_empty(), "no file yet");
        let hub = Hub::new("server".to_owned(), Vec::new());
        let keeper = tokio::spawn(store.clone().keep(hub.keep_devices(Devices::new())));
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
        assert_eq!(loaded.get(&client), Some(&device), "the phone is kept");

        let key = store.install_key().await.unwrap();
        let path = dir.path().join("server").join(PUSH_KEY);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "only its owner reads it");
        assert_eq!(store.install_key().await.unwrap().public(), key.public(), "the same key");
        std::fs::write(&path, b"short").unwrap();
        let new = store.install_key().await.unwrap();
        assert_ne!(new.public(), key.public(), "one that is no key is replaced");
        let aside = std::fs::read_dir(dir.path().join("server"))
            .unwrap()
            .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().contains(".bad-"))
            .count();
        assert_eq!(aside, 1, "and set aside");
    }

    /// Projects outlive the server: its keeper writes them after each change, and a server
    /// started again on the same directory has every project whole, timeline and all.
    #[tokio::test]
    async fn projects_survive_a_restart() {
        use slopty_proto::orchestration::{Outcome, Verb};
        use slopty_proto::project::{LimitsChange, ProjectId, TaskSpec};

        let dir = tempfile::tempdir().unwrap();
        let config = || crate::Config {
            name: "server".to_owned(),
            quic: "127.0.0.1:0".parse().unwrap(),
            mcp: "127.0.0.1:0".parse().unwrap(),
            data_dir: dir.path().join("server"),
            admission: slopty_net::admission::Admission::default(),
            push: crate::PushConfig::Off,
        };
        let id = ProjectId::new("slopty").unwrap();
        let status = |hub: Hub| {
            let id = id.clone();
            async move {
                let verb = Verb::ProjectStatus { project: id, since: Some(0), timeout_ms: 0 };
                match hub.dispatch(verb).await {
                    Outcome::Project(status) => *status,
                    other => panic!("{other:?}"),
                }
            }
        };
        let server = crate::Server::start(config()).await.unwrap();
        let made = server
            .hub()
            .dispatch(Verb::ProjectCreate {
                project: id.clone(),
                title: "Projects".to_owned(),
                repo: "~/src/slopty".to_owned(),
                target: "main".to_owned(),
                verifier: Some("cargo gate".to_owned()),
                push: false,
                orchestrator: None,
                limits: LimitsChange { review: Some(2) },
                metadata: Some(r#"{"goal":"open"}"#.to_owned()),
                members: Vec::new(),
            })
            .await;
        assert!(matches!(made, Outcome::Project(_)), "{made:?}");
        let spec = TaskSpec {
            title: "Store".to_owned(),
            brief: "Keep it.".to_owned(),
            ..TaskSpec::default()
        };
        let task = Verb::TaskCreate { project: id.clone(), spec: Box::new(spec) };
        assert!(matches!(server.hub().dispatch(task).await, Outcome::Task(_)));
        let store = ProjectStore::in_dir(&dir.path().join("server"));
        let written = async {
            while store.load().await.unwrap().projects.first().is_none_or(|r| r.tasks.is_empty()) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), written)
            .await
            .expect("the keeper writes the change without a shutdown");
        let before = status(server.hub().clone()).await;
        server.shutdown().await;

        let again = crate::Server::start(config()).await.unwrap();
        assert_eq!(status(again.hub().clone()).await, before);
        again.shutdown().await;
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

    /// A file that does not parse is set aside under a name of its own, every time: a second
    /// bad file never writes over the first.
    #[tokio::test]
    async fn every_corrupt_projects_file_is_kept_aside() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProjectStore::in_dir(dir.path());
        for bad in [&b"{ not json"[..], b"[1, 2"] {
            std::fs::write(store.path(), bad).unwrap();
            assert_eq!(store.load().await.unwrap().projects, Vec::<crate::project::Record>::new());
            assert!(!store.path().exists());
        }
        let mut kept = set_aside(dir.path(), PROJECTS_FILE);
        kept.sort();
        assert_eq!(kept, [b"[1, 2".to_vec(), b"{ not json".to_vec()]);
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

    /// A projects file the server cannot read is not a missing one: the server does not start,
    /// rather than write an empty one over it.
    #[tokio::test]
    async fn an_unreadable_projects_file_stops_the_server_instead_of_being_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("server");
        std::fs::create_dir_all(data_dir.join(PROJECTS_FILE)).unwrap();
        let config = crate::Config {
            name: "server".to_owned(),
            quic: "127.0.0.1:0".parse().unwrap(),
            mcp: "127.0.0.1:0".parse().unwrap(),
            data_dir: data_dir.clone(),
            admission: slopty_net::admission::Admission::default(),
            push: crate::PushConfig::Off,
        };
        let started = crate::Server::start(config).await;
        let Err(crate::ServerError::State { path, .. }) = started else {
            panic!("started over an unreadable file")
        };
        assert_eq!(path, data_dir.join(PROJECTS_FILE));
        assert!(data_dir.join(PROJECTS_FILE).is_dir(), "left as it was");
    }

    /// Changes a hub made: a project, then a task in it, then that task noted, and two
    /// terminals watched, one of them then no longer.
    fn changes() -> (Vec<Keep>, ProjectsFile) {
        use std::collections::HashSet;

        use slopty_core::{SessionId, WallMs, WorkerId};
        use slopty_proto::orchestration::TermRef;
        use slopty_proto::project::{LimitsChange, ProjectId, TaskChange, TaskId, TaskSpec};

        use crate::project::{Caller, Drove, NewProject, Projects, Running, Watched};

        let now = WallMs::from_millis(1_790_000_000_000);
        let id = ProjectId::new("slopty").unwrap();
        let none = HashSet::new();
        let running = Running { terminals: &none, agents: &none, starting: &[] };
        let mut p = Projects::default();
        let new = NewProject {
            id: id.clone(),
            title: "Projects".to_owned(),
            repo: "~/src/slopty".to_owned(),
            target: "main".to_owned(),
            verifier: None,
            push: false,
            orchestrator: None,
            limits: LimitsChange::default(),
            metadata: None,
            members: Vec::new(),
        };
        let mut all = p.create(new, &running, now).unwrap().1;
        let spec = TaskSpec { title: "Store".to_owned(), ..TaskSpec::default() };
        all.extend(p.create_task(&id, spec, now).unwrap().1);
        let note = TaskChange { note: Some("kept".to_owned()), ..TaskChange::default() };
        all.extend(p.update_task(&id, TaskId(1), note, Caller::Person, now).unwrap().1);
        let mut kept: Vec<Keep> =
            all.into_iter().map(|c| Keep::Project(Box::new(c.kept))).collect();
        let watched = |drove| Watched {
            term: TermRef { worker: WorkerId::new(), session: SessionId::new() },
            locked: true,
            drove,
        };
        let (kept_on, gone) = (watched(Some(Drove::Opened { by: None })), watched(None));
        kept.extend([Keep::Watch(gone), Keep::Watch(kept_on), Keep::Unwatch(gone.term.session)]);
        let through = u64::try_from(kept.len()).unwrap();
        (kept, p.file(vec![kept_on], through))
    }

    fn line(n: usize, kept: &Keep) -> Vec<u8> {
        let n = u64::try_from(n).unwrap();
        let mut line = serde_json::to_vec(&Logging { n, kept }).unwrap();
        line.push(b'\n');
        line
    }

    /// The log goes on from the snapshot: a line the snapshot holds is passed over, a line a
    /// crash cut short at the end is passed over, and a bad line before the end sets the log
    /// aside keeping every change before it.
    #[tokio::test]
    async fn the_log_replays_past_the_snapshot_and_survives_a_torn_or_bad_line() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProjectStore::in_dir(dir.path());
        let (kept, whole) = changes();
        let mut first = ProjectsFile::default();
        first.apply(&kept[0]);
        store.save(&first).await.unwrap();
        let mut log: Vec<u8> = kept.iter().enumerate().flat_map(|(i, k)| line(i + 1, k)).collect();
        log.extend(b"{\"n\": 99, \"kept\": {\"proj");
        std::fs::write(store.log_path(), &log).unwrap();
        assert_eq!(store.load().await.unwrap(), whole, "the torn line is passed over");
        assert!(store.log_path().exists(), "and the log kept");

        let mut log: Vec<u8> =
            kept.iter().enumerate().take(2).flat_map(|(i, k)| line(i + 1, k)).collect();
        log.extend(b"not json\n");
        log.extend(line(3, &kept[2]));
        std::fs::write(store.log_path(), &log).unwrap();
        let loaded = store.load().await.unwrap();
        assert_eq!(loaded.through, 2, "the changes before the bad line");
        assert!(!store.log_path().exists());
        assert_eq!(set_aside(dir.path(), PROJECTS_LOG), [log]);
    }

    /// The keeper appends each change as a line and, when it stops, writes the snapshot and
    /// empties the log; what it wrote reads back as the model had it.
    #[tokio::test]
    async fn the_keeper_appends_each_change_then_compacts_when_it_stops() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProjectStore::in_dir(dir.path());
        let (kept, whole) = changes();
        let (tx, rx) = mpsc::unbounded_channel();
        let keeper = tokio::spawn(store.clone().keep(ProjectsFile::default(), rx));
        for k in &kept {
            tx.send(k.clone()).unwrap();
        }
        let appended = async {
            loop {
                let text = tokio::fs::read(store.log_path()).await.unwrap_or_default();
                if String::from_utf8_lossy(&text).lines().count() == kept.len() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), appended).await.expect("one line each");
        assert_eq!(store.load().await.unwrap(), whole, "the snapshot and its log");
        drop(tx);
        keeper.await.unwrap();
        assert!(!store.log_path().exists(), "compacted");
        assert_eq!(store.load().await.unwrap(), whole);
    }
}
