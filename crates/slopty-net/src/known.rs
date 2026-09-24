//! The client's installation id and the workers it has added, as `workers.json` in its data dir.
//!
//! A worker is keyed by its [`WorkerId`] (from `HelloAck`), never by its address: the address
//! is only where it was last reached, and a worker that moves keeps its canvas and its row.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use slopty_core::{ClientId, WorkerId};

use crate::NetError;
use crate::addr::HostAddr;

/// File name inside the client's data directory.
pub const FILE_NAME: &str = "workers.json";

/// A worker this client has added.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct KnownWorker {
    /// Where to reach it.
    pub address: HostAddr,
    /// Display name from its last `HelloAck`.
    pub name: String,
    /// Its identity from `HelloAck`.
    pub worker_id: WorkerId,
}

#[derive(Serialize, Deserialize, Default)]
struct File {
    client_id: Option<ClientId>,
    #[serde(default)]
    workers: Vec<KnownWorker>,
}

/// This installation's id and its workers, in the order they were added.
#[derive(Debug)]
pub struct KnownWorkers {
    path: PathBuf,
    client: ClientId,
    workers: Vec<KnownWorker>,
}

impl KnownWorkers {
    /// `workers.json` inside `data_dir`, loaded or created.
    pub fn open_in(data_dir: &Path) -> Result<Self, NetError> {
        Self::open(&data_dir.join(FILE_NAME))
    }

    /// Load `path`, or create it with a fresh client id.
    pub fn open(path: &Path) -> Result<Self, NetError> {
        let file: File = match std::fs::read(path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|e| NetError::Store(e.to_string()))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => File::default(),
            Err(e) => return Err(NetError::Store(e.to_string())),
        };
        let fresh = file.client_id.is_none();
        let me = Self {
            path: path.to_path_buf(),
            client: file.client_id.unwrap_or_default(),
            workers: file.workers,
        };
        if fresh {
            me.save()?;
        }
        Ok(me)
    }

    /// This installation's id.
    #[must_use]
    pub const fn client(&self) -> ClientId {
        self.client
    }

    /// Every worker, in the order added.
    #[must_use]
    pub fn workers(&self) -> &[KnownWorker] {
        &self.workers
    }

    /// The worker with this id.
    #[must_use]
    pub fn get(&self, id: WorkerId) -> Option<&KnownWorker> {
        self.workers.iter().find(|w| w.worker_id == id)
    }

    /// The one worker whose name, address or id starts with `needle` (case-insensitive).
    #[must_use]
    pub fn find(&self, needle: &str) -> Option<&KnownWorker> {
        let needle = needle.to_lowercase();
        let mut hits = self.workers.iter().filter(|w| {
            w.name.to_lowercase().starts_with(&needle)
                || w.address.to_string().to_lowercase().starts_with(&needle)
                || w.worker_id.to_string().starts_with(&needle)
        });
        let first = hits.next()?;
        hits.next().is_none().then_some(first)
    }

    /// Add a worker, or update the one with its id. An entry at the same address under another
    /// id is a reinstalled worker and goes: that address now answers with the new id.
    pub fn remember(&mut self, worker: KnownWorker) -> Result<(), NetError> {
        let (id, address) = (worker.worker_id, worker.address.clone());
        if let Some(same) = self.workers.iter_mut().find(|w| w.worker_id == id) {
            *same = worker;
        } else {
            self.workers.push(worker);
        }
        self.workers.retain(|w| w.worker_id == id || w.address != address);
        self.save()
    }

    /// Forget a worker; whether it was known.
    pub fn forget(&mut self, id: WorkerId) -> Result<bool, NetError> {
        let before = self.workers.len();
        self.workers.retain(|w| w.worker_id != id);
        let removed = self.workers.len() != before;
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    fn save(&self) -> Result<(), NetError> {
        let store = |e: std::io::Error| NetError::Store(e.to_string());
        let file = File { client_id: Some(self.client), workers: self.workers.clone() };
        let json = serde_json::to_vec_pretty(&file).map_err(|e| NetError::Store(e.to_string()))?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(store)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &json).map_err(store)?;
        std::fs::rename(&tmp, &self.path).map_err(store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker(name: &str, address: &str) -> KnownWorker {
        KnownWorker {
            address: address.parse().unwrap(),
            name: name.to_owned(),
            worker_id: WorkerId::new(),
        }
    }

    #[test]
    fn workers_persist_in_order_and_are_found_by_name_address_or_id() {
        let dir = tempfile::tempdir().unwrap();
        let studio = worker("Mac Studio", "mac-studio.tail1234.ts.net");
        let laptop = worker("MacBook", "100.64.0.7:45551");
        let client = {
            let mut me = KnownWorkers::open_in(dir.path()).unwrap();
            me.remember(studio.clone()).unwrap();
            me.remember(laptop.clone()).unwrap();
            me.client()
        };
        let me = KnownWorkers::open_in(dir.path()).unwrap();
        assert_eq!(me.client(), client, "the id survives");
        assert_eq!(me.workers(), [studio.clone(), laptop.clone()]);
        assert_eq!(me.find("mac s"), Some(&studio));
        assert_eq!(me.find("100.64"), Some(&laptop));
        // Ids made in the same millisecond share their UUIDv7 time prefix; the whole id is unique.
        assert_eq!(me.find(&laptop.worker_id.to_string()), Some(&laptop));
        assert_eq!(me.find("mac"), None, "two workers start with mac");
        assert_eq!(me.find("zzz"), None);
        assert_eq!(me.get(studio.worker_id), Some(&studio));
    }

    #[test]
    fn a_worker_is_keyed_by_its_id_and_a_reinstall_replaces_its_address() {
        let dir = tempfile::tempdir().unwrap();
        let mut me = KnownWorkers::open_in(dir.path()).unwrap();
        let studio = worker("Studio", "studio");
        me.remember(studio.clone()).unwrap();
        let moved = KnownWorker { address: "100.64.0.9".parse().unwrap(), ..studio.clone() };
        me.remember(moved.clone()).unwrap();
        assert_eq!(me.workers(), [moved], "same id, new address: one row");
        let reinstalled = worker("Studio", "100.64.0.9");
        me.remember(reinstalled.clone()).unwrap();
        assert_eq!(
            me.workers(),
            std::slice::from_ref(&reinstalled),
            "a new id at the old address replaces it"
        );
        assert!(!me.forget(studio.worker_id).unwrap(), "already gone");
        assert!(me.forget(reinstalled.worker_id).unwrap());
        assert!(KnownWorkers::open_in(dir.path()).unwrap().workers().is_empty(), "saved");
    }

    #[test]
    fn a_corrupt_file_is_an_error_not_a_fresh_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep").join(FILE_NAME);
        let fresh = KnownWorkers::open(&path).unwrap();
        assert!(path.exists(), "a fresh id is written at once");
        assert_eq!(KnownWorkers::open(&path).unwrap().client(), fresh.client());
        std::fs::write(&path, b"{not json").unwrap();
        KnownWorkers::open(&path).unwrap_err();
    }
}
