//! The whole texts and pictures behind clipped content, kept in memory by recency under a byte
//! budget, so an expansion opened again draws without asking the worker.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use slopty_proto::thread::ContentRef;
use slopty_proto::thread::wire::Expanded;

/// The bytes the blobs hold at most, together.
pub const BLOB_BUDGET: usize = 64 << 20;

/// Expanded content by where it came from, the least recently used going first.
#[derive(Clone, Debug)]
pub struct Blobs {
    held: HashMap<ContentRef, Arc<Expanded>>,
    /// Oldest use first. Each key is in it once.
    order: VecDeque<ContentRef>,
    bytes: usize,
    budget: usize,
}

impl Default for Blobs {
    fn default() -> Self {
        Self::with_budget(BLOB_BUDGET)
    }
}

/// What one expansion weighs.
const fn weight(body: &Expanded) -> usize {
    match body {
        Expanded::Text(text) => text.len(),
        Expanded::Bytes(bytes) => bytes.len(),
        Expanded::Gone => 0,
    }
}

impl Blobs {
    /// Blobs that hold at most `budget` bytes.
    #[must_use]
    pub fn with_budget(budget: usize) -> Self {
        Self { held: HashMap::new(), order: VecDeque::new(), bytes: 0, budget }
    }

    /// The bytes held.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    /// The expansion of `content`, made the most recent.
    pub fn get(&mut self, content: &ContentRef) -> Option<Arc<Expanded>> {
        let found = Arc::clone(self.held.get(content)?);
        self.touch(content);
        Some(found)
    }

    /// Keep `body` as the expansion of `content`, letting the least recent go to stay within
    /// the budget. One larger than the whole budget is not kept.
    pub fn put(&mut self, content: ContentRef, body: Expanded) {
        let size = weight(&body);
        if size > self.budget {
            return;
        }
        if let Some(old) = self.held.insert(content.clone(), Arc::new(body)) {
            self.bytes = self.bytes.saturating_sub(weight(&old));
            self.touch(&content);
        } else {
            self.order.push_back(content);
        }
        self.bytes = self.bytes.saturating_add(size);
        while self.bytes > self.budget {
            let Some(oldest) = self.order.pop_front() else { break };
            if let Some(gone) = self.held.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(weight(&gone));
            }
        }
    }

    fn touch(&mut self, content: &ContentRef) {
        if let Some(at) = self.order.iter().position(|c| c == content) {
            self.order.remove(at);
        }
        self.order.push_back(content.clone());
    }
}
