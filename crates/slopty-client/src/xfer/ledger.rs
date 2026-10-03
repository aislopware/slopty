//! The transfers in flight, kept in the client's data directory across a relaunch.
//!
//! An app that quits or crashes mid-transfer takes each up at its next launch: an upload begun
//! again under its id, every file from what the worker holds of it, and a download fetched again
//! to where it was going.
//!
//! Only transfers that still mean something to a new run are kept: files dropped on a shell or
//! a folder, and a worker's file brought down to a place here the person chose. A paste, a
//! drag over a remote window or an attachment ends with the run that started it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use slopty_core::XferId;
use slopty_proto::transfer::Dest;

use crate::layout::{TileRef, WorkerKey};

/// The ledger's file in the client's data directory.
pub const FILE: &str = "transfers.json";

/// One transfer in flight.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kept {
    /// The transfer it began as.
    pub xfer: XferId,
    /// The worker at the other end.
    pub worker: WorkerKey,
    /// Which way it goes, and what.
    pub way: Way,
}

/// Which way a kept transfer goes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Way {
    /// Files from here to the worker.
    Up {
        /// The tile they were dropped on.
        tile: TileRef,
        /// The files and folders here.
        files: Vec<PathBuf>,
        /// Where they go there.
        dest: Dest,
        /// Bytes in them.
        total: u64,
        /// Where they wait here when they are a copy made for the upload, deleted once it ends.
        scratch: Option<PathBuf>,
    },
    /// The worker's file or folder to a place here.
    Down {
        /// Its path on the worker.
        source: String,
        /// Where it lands here.
        dest: PathBuf,
    },
}

/// Every transfer in flight, in the order they began.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    /// The transfers.
    pub kept: Vec<Kept>,
}

impl Ledger {
    /// The ledger at `path`; empty when there is none, or it cannot be read (logged: a ledger
    /// another build wrote is not read, by rule).
    #[must_use]
    pub fn load(path: &Path) -> Self {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "transfer ledger unreadable");
                return Self::default();
            }
        };
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!(path = %path.display(), error = %e, "transfer ledger ignored");
            Self::default()
        })
    }

    /// Write it to `path` whole, so a crash leaves the old one or the new
    /// (`slopty_platform::fs::replace`); with nothing in flight, the file goes.
    ///
    /// # Errors
    ///
    /// When the file cannot be written or removed.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if self.kept.is_empty() {
            return match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        slopty_platform::fs::replace(path, &json)
    }

    /// Keep `kept`, in place of a transfer of the same id.
    pub fn keep(&mut self, kept: Kept) {
        self.end(kept.xfer);
        self.kept.push(kept);
    }

    /// Transfer `xfer` ended, however it did. Whether it was kept.
    pub fn end(&mut self, xfer: XferId) -> bool {
        let before = self.kept.len();
        self.kept.retain(|k| k.xfer != xfer);
        self.kept.len() != before
    }
}

#[cfg(test)]
mod tests {
    use slopty_core::{ItemId, SessionId};

    use super::*;

    fn up(xfer: XferId) -> Kept {
        let worker = WorkerKey::new(7);
        Kept {
            xfer,
            worker,
            way: Way::Up {
                tile: TileRef { worker, item: ItemId::new() },
                files: vec![PathBuf::from("/Users/me/report.pdf")],
                dest: Dest::SessionCwd(SessionId::new()),
                total: 1_200,
                scratch: None,
            },
        }
    }

    /// What is kept comes back as it was at the next launch; a transfer that ends leaves it,
    /// and the file goes with the last one. No file, or one that cannot be read, is an empty
    /// ledger.
    #[test]
    fn transfers_kept_come_back_and_the_file_goes_with_the_last() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        assert_eq!(Ledger::load(&path), Ledger::default(), "no file yet");

        let (a, b) = (XferId::new(), XferId::new());
        let down = Kept {
            xfer: b,
            worker: WorkerKey::new(9),
            way: Way::Down {
                source: "~/build/out.tar".to_owned(),
                dest: PathBuf::from("/Users/me/Downloads/out.tar"),
            },
        };
        let mut ledger = Ledger::default();
        ledger.keep(up(a));
        ledger.keep(down.clone());
        ledger.keep(up(a));
        assert_eq!(ledger.kept.len(), 2, "kept once per transfer");
        ledger.save(&path).unwrap();
        assert_eq!(Ledger::load(&path), ledger);

        assert!(ledger.end(a));
        assert!(!ledger.end(a), "ended once");
        ledger.save(&path).unwrap();
        assert_eq!(Ledger::load(&path).kept, [down]);
        assert!(ledger.end(b));
        ledger.save(&path).unwrap();
        assert!(!path.exists(), "nothing in flight, no file");
        ledger.save(&path).unwrap();

        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(Ledger::load(&path), Ledger::default());
    }
}
