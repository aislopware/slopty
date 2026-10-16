//! The intents a thread was asked, by id, and how each went: a client resends what it never
//! heard back on under the same id, and the repeat gets the first outcome without being acted
//! on again.
//!
//! A thread keeps the last [`KEPT`] ids. They are kept on disk beside its log (`intents`, one
//! framed record each), so a worker restart does not act on a resend twice either; the file is
//! written anew with only the kept ones when it has grown to twice that.

use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use bytes::BytesMut;
use serde::{Deserialize, Serialize};
use slopty_proto::codec;
use slopty_proto::thread::IntentId;
use slopty_proto::thread::wire::Outcome;

/// Intent ids a thread remembers.
pub const KEPT: usize = 512;

#[derive(Serialize, Deserialize)]
struct Record {
    id: IntentId,
    outcome: Outcome,
}

/// One thread's answered intents.
#[derive(Debug)]
pub struct Intents {
    path: PathBuf,
    order: VecDeque<IntentId>,
    outcomes: HashMap<IntentId, Outcome>,
    file: File,
    on_disk: usize,
}

impl Intents {
    /// The intents recorded at `path`, or none.
    ///
    /// # Errors
    ///
    /// When the file cannot be opened for appending.
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut intents = Self {
            path: path.to_owned(),
            order: VecDeque::new(),
            outcomes: HashMap::new(),
            file: OpenOptions::new().create(true).append(true).open(path)?,
            on_disk: 0,
        };
        let bytes = std::fs::read(path)?;
        let mut rest = BytesMut::from(bytes.as_slice());
        while let Ok(Some(body)) = codec::try_take(&mut rest) {
            let Ok(record) = codec::decode_body::<Record>(&body) else { break };
            intents.remember(record.id, record.outcome);
            intents.on_disk = intents.on_disk.saturating_add(1);
        }
        if !rest.is_empty() {
            intents.rewrite()?;
        }
        Ok(intents)
    }

    /// How `id` went, when it was answered.
    #[must_use]
    pub fn outcome(&self, id: &IntentId) -> Option<&Outcome> {
        self.outcomes.get(id)
    }

    /// Record how `id` went.
    ///
    /// # Errors
    ///
    /// When it cannot be written; it is remembered anyway.
    pub fn record(&mut self, id: IntentId, outcome: Outcome) -> io::Result<()> {
        let frame =
            codec::encode(&Record { id, outcome: outcome.clone() }).map_err(io::Error::other)?;
        self.remember(id, outcome);
        self.file.write_all(&frame)?;
        self.on_disk = self.on_disk.saturating_add(1);
        if self.on_disk >= KEPT.saturating_mul(2) {
            self.rewrite()?;
        }
        Ok(())
    }

    fn remember(&mut self, id: IntentId, outcome: Outcome) {
        if self.outcomes.insert(id, outcome).is_none() {
            self.order.push_back(id);
        }
        while self.order.len() > KEPT {
            if let Some(gone) = self.order.pop_front() {
                self.outcomes.remove(&gone);
            }
        }
    }

    fn rewrite(&mut self) -> io::Result<()> {
        let mut body = Vec::new();
        for id in &self.order {
            if let Some(outcome) = self.outcomes.get(id) {
                let record = Record { id: *id, outcome: outcome.clone() };
                body.extend_from_slice(&codec::encode(&record).map_err(io::Error::other)?);
            }
        }
        let staging = self.path.with_extension("staging");
        std::fs::write(&staging, body)?;
        std::fs::rename(&staging, &self.path)?;
        self.file = OpenOptions::new().append(true).open(&self.path)?;
        self.on_disk = self.order.len();
        Ok(())
    }
}
