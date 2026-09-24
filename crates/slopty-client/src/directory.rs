//! The worker directory the server keeps, as this client last heard it
//! (`docs/decisions/topology.md`).
//!
//! The server lists the workers; the client dials each one directly, so the directory decides
//! *whom* to dial and *where*, never what goes over the link. With the server unreachable the
//! last directory stands (degraded mode): every cached worker is dialled at its cached
//! address, so terminals and streams keep going while only the list stops changing.
//!
//! Pure: [`Directory::apply`] takes what the server said and returns what changed; the link
//! that feeds it is [`crate::server`].

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use slopty_core::WorkerId;
use slopty_net::HostAddr;
use slopty_net::endpoint::WORKER_PORT;
use slopty_proto::server::{Event, FromServer, Liveness, WorkerInfo};

/// File name of the cached directory inside the client's data directory.
pub const CACHE_FILE: &str = "directory.json";

/// Where the link to the server stands.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum ServerState {
    /// No server is configured.
    #[default]
    Off,
    /// Dialling, before the first answer.
    Linking,
    /// Welcomed.
    Linked {
        /// The server's name.
        name: String,
    },
    /// The last attempt failed or the link dropped; redialling, with the reason.
    Unreachable {
        /// Why.
        why: String,
    },
}

/// What one message from the server changed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Change {
    /// A worker the directory did not have.
    Listed(WorkerId),
    /// A worker went online, unreachable or gone.
    Liveness {
        /// Which.
        worker: WorkerId,
        /// Before.
        was: Liveness,
        /// Now.
        now: Liveness,
    },
    /// A worker's name or address changed.
    Moved(WorkerId),
    /// A worker a full directory no longer lists.
    Unlisted(WorkerId),
    /// Something happened on a worker.
    Event(Event),
}

/// Whether to dial a worker now, and where.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Dial {
    /// Dial this address.
    At(HostAddr),
    /// The server says the worker is not there; wait until it comes back online.
    Hold(Liveness),
    /// The directory does not list it (a worker added by address, or none at all).
    Unlisted,
}

/// The directory and the state of the link that keeps it.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Directory {
    server: ServerState,
    workers: BTreeMap<WorkerId, WorkerInfo>,
}

impl Directory {
    /// A directory of `workers` (a cache read at launch), before the server has answered.
    #[must_use]
    pub fn cached(workers: Vec<WorkerInfo>) -> Self {
        Self {
            server: ServerState::Linking,
            workers: workers.into_iter().map(|w| (w.worker, w)).collect(),
        }
    }

    /// Where the link stands.
    #[must_use]
    pub const fn server(&self) -> &ServerState {
        &self.server
    }

    /// The link moved on.
    pub fn set_server(&mut self, state: ServerState) {
        self.server = state;
    }

    /// Whether the server is answering right now.
    #[must_use]
    pub const fn linked(&self) -> bool {
        matches!(self.server, ServerState::Linked { .. })
    }

    /// A server is configured but not answering: the cached directory stands.
    #[must_use]
    pub const fn degraded(&self) -> bool {
        matches!(self.server, ServerState::Linking | ServerState::Unreachable { .. })
    }

    /// Every listed worker, by id.
    pub fn workers(&self) -> impl Iterator<Item = &WorkerInfo> {
        self.workers.values()
    }

    /// One worker.
    #[must_use]
    pub fn get(&self, worker: WorkerId) -> Option<&WorkerInfo> {
        self.workers.get(&worker)
    }

    /// Forget every worker (the server was disconnected).
    pub fn clear(&mut self) -> Vec<WorkerId> {
        std::mem::take(&mut self.workers).into_keys().collect()
    }

    /// Take one message from the server.
    pub fn apply(&mut self, msg: FromServer) -> Vec<Change> {
        match msg {
            FromServer::Directory(list) => {
                let listed: std::collections::HashSet<WorkerId> =
                    list.iter().map(|w| w.worker).collect();
                let mut changes: Vec<Change> = self
                    .workers
                    .keys()
                    .filter(|id| !listed.contains(id))
                    .map(|id| Change::Unlisted(*id))
                    .collect();
                self.workers.retain(|id, _| listed.contains(id));
                for info in list {
                    changes.extend(self.upsert(info));
                }
                changes
            }
            FromServer::Worker(info) => self.upsert(info),
            FromServer::Event(event) => vec![Change::Event(event)],
            FromServer::Welcome { .. }
            | FromServer::Refused(_)
            | FromServer::Request { .. }
            | FromServer::Reply { .. } => Vec::new(),
        }
    }

    fn upsert(&mut self, info: WorkerInfo) -> Vec<Change> {
        let id = info.worker;
        let mut changes = Vec::new();
        match self.workers.get(&id) {
            None => changes.push(Change::Listed(id)),
            Some(old) => {
                if old.liveness != info.liveness {
                    changes.push(Change::Liveness {
                        worker: id,
                        was: old.liveness,
                        now: info.liveness,
                    });
                }
                if old.name != info.name || old.address != info.address {
                    changes.push(Change::Moved(id));
                }
            }
        }
        self.workers.insert(id, info);
        changes
    }

    /// Whether to dial `worker` now. While the server answers, its word on liveness holds; while
    /// it does not, every cached worker is dialled at its last address.
    #[must_use]
    pub fn dial(&self, worker: WorkerId) -> Dial {
        let Some(info) = self.workers.get(&worker) else { return Dial::Unlisted };
        if self.linked() && info.liveness != Liveness::Online {
            return Dial::Hold(info.liveness);
        }
        match HostAddr::parse_with_port(&info.address, WORKER_PORT) {
            Ok(addr) => Dial::At(addr),
            Err(e) => {
                tracing::warn!(%worker, address = %info.address, error = %e, "directory address");
                Dial::Unlisted
            }
        }
    }

    /// Write the directory to `path` for the next launch without the server: to a temporary
    /// file beside it, flushed, then renamed over it, so a crash leaves the old one or the new.
    ///
    /// # Errors
    ///
    /// When the file cannot be written.
    pub fn save(&self, path: &Path, server: &HostAddr) -> std::io::Result<()> {
        let cache =
            Cache { server: server.to_string(), workers: self.workers.values().cloned().collect() };
        let json = serde_json::to_vec_pretty(&cache).map_err(std::io::Error::other)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        {
            use std::io::Write as _;
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(&json)?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, path)
    }

    /// The workers cached at `path` for `server`; none when the file is absent, unreadable, or
    /// was written for another server.
    #[must_use]
    pub fn load(path: &Path, server: &HostAddr) -> Vec<WorkerInfo> {
        let Ok(bytes) = std::fs::read(path) else { return Vec::new() };
        match serde_json::from_slice::<Cache>(&bytes) {
            Ok(cache) if cache.server == server.to_string() => cache.workers,
            Ok(_other) => Vec::new(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cached directory ignored");
                Vec::new()
            }
        }
    }
}

/// The cache file.
#[derive(Serialize, Deserialize)]
struct Cache {
    /// The server it came from, as `host:port`.
    server: String,
    /// Its workers as last heard.
    workers: Vec<WorkerInfo>,
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use slopty_proto::server::{Os, WorkerCaps};

    use super::*;

    fn info(worker: WorkerId, address: &str, liveness: Liveness) -> WorkerInfo {
        WorkerInfo {
            worker,
            name: "studio".to_owned(),
            address: address.to_owned(),
            liveness,
            caps: WorkerCaps {
                os: Os::MacOs,
                os_version: "26.5".to_owned(),
                arch: "aarch64".to_owned(),
                cpus: 8,
                memory: 1 << 34,
                encoders: Vec::new(),
                displays: Vec::new(),
                agents: Vec::new(),
                can_capture: true,
                can_inject: true,
                load: 0.5,
                version: "0".to_owned(),
            },
            last_seen_ms: 0,
        }
    }

    fn linked() -> Directory {
        let mut d = Directory::default();
        d.set_server(ServerState::Linked { name: "server".to_owned() });
        d
    }

    #[test]
    fn liveness_moves_are_reported_once_each() {
        let id = WorkerId::new();
        let mut d = linked();
        let listed =
            d.apply(FromServer::Directory(vec![info(id, "100.64.0.3:45550", Liveness::Online)]));
        assert_eq!(listed, vec![Change::Listed(id)]);
        assert_eq!(d.dial(id), Dial::At(HostAddr::new("100.64.0.3", 45550)));

        let down = d.apply(FromServer::Worker(info(id, "100.64.0.3:45550", Liveness::Unreachable)));
        assert_eq!(
            down,
            vec![Change::Liveness {
                worker: id,
                was: Liveness::Online,
                now: Liveness::Unreachable
            }]
        );
        assert_eq!(d.dial(id), Dial::Hold(Liveness::Unreachable), "no dial while it is away");
        let gone = d.apply(FromServer::Worker(info(id, "100.64.0.3:45550", Liveness::Gone)));
        assert!(matches!(gone.as_slice(), [Change::Liveness { now: Liveness::Gone, .. }]));
        assert_eq!(d.dial(id), Dial::Hold(Liveness::Gone));

        let back = d.apply(FromServer::Worker(info(id, "100.64.0.9:45550", Liveness::Online)));
        assert_eq!(
            back,
            vec![
                Change::Liveness { worker: id, was: Liveness::Gone, now: Liveness::Online },
                Change::Moved(id),
            ],
            "it came back from somewhere else"
        );
        assert_eq!(d.dial(id), Dial::At(HostAddr::new("100.64.0.9", 45550)), "reattach there");
        assert!(
            d.apply(FromServer::Worker(info(id, "100.64.0.9:45550", Liveness::Online))).is_empty()
        );
    }

    #[test]
    fn a_full_directory_unlists_what_it_leaves_out() {
        let (a, b) = (WorkerId::new(), WorkerId::new());
        let mut d = linked();
        d.apply(FromServer::Directory(vec![
            info(a, "10.0.0.1:45550", Liveness::Online),
            info(b, "10.0.0.2:45550", Liveness::Online),
        ]));
        let changes =
            d.apply(FromServer::Directory(vec![info(b, "10.0.0.2:45550", Liveness::Gone)]));
        assert_eq!(
            changes,
            vec![
                Change::Unlisted(a),
                Change::Liveness { worker: b, was: Liveness::Online, now: Liveness::Gone },
            ]
        );
        assert_eq!(d.dial(a), Dial::Unlisted);
        assert_eq!(d.workers().count(), 1);
    }

    #[test]
    fn without_the_server_the_cache_is_dialled_whatever_it_last_said() {
        let (up, away) = (WorkerId::new(), WorkerId::new());
        let mut d = Directory::cached(vec![
            info(up, "10.0.0.1:45550", Liveness::Online),
            info(away, "[fd7a:115c:a1e0::2]:45550", Liveness::Gone),
        ]);
        assert!(d.degraded() && !d.linked());
        assert_eq!(d.dial(up), Dial::At(HostAddr::new("10.0.0.1", 45550)));
        assert_eq!(
            d.dial(away),
            Dial::At(HostAddr::new("fd7a:115c:a1e0::2", 45550)),
            "the server's word is stale, so try it"
        );
        d.set_server(ServerState::Linked { name: "s".to_owned() });
        assert_eq!(d.dial(away), Dial::Hold(Liveness::Gone), "once it answers, it holds");
        d.set_server(ServerState::Unreachable { why: "timed out".to_owned() });
        assert!(d.degraded());
        assert_eq!(d.dial(away), Dial::At(HostAddr::new("fd7a:115c:a1e0::2", 45550)));
    }

    #[test]
    fn events_pass_through() {
        let worker = WorkerId::new();
        let session = slopty_core::SessionId::new();
        let event = Event::SessionClosed { worker, session };
        assert_eq!(linked().apply(FromServer::Event(event.clone())), vec![Change::Event(event)]);
    }

    #[test]
    fn the_cache_round_trips_for_its_own_server_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CACHE_FILE);
        let server = HostAddr::new("studio", 45560);
        let id = WorkerId::new();
        let mut d = linked();
        d.apply(FromServer::Directory(vec![info(id, "10.0.0.1:45550", Liveness::Online)]));
        d.save(&path, &server).unwrap();
        let back = Directory::load(&path, &server);
        assert_eq!(back, vec![info(id, "10.0.0.1:45550", Liveness::Online)]);
        assert!(Directory::load(&path, &HostAddr::new("other", 45560)).is_empty());
        assert!(Directory::load(&dir.path().join("absent.json"), &server).is_empty());
        std::fs::write(&path, b"{").unwrap();
        assert!(Directory::load(&path, &server).is_empty(), "a broken cache costs the cache");
    }
}
