//! Whose terminal history is compressed next.
//!
//! An idle session compresses its scrollback in bounded steps on its own actor thread
//! ([`crate::session`]). Sessions take turns, one at a time across the worker, so compression
//! never takes more than a core however many sessions fall quiet together, and the session
//! holding the most memory goes first: when several wait, that is where a turn frees the most.

use std::sync::{Arc, LazyLock};

use parking_lot::Mutex;
use slopty_core::SessionId;
use tokio::sync::oneshot;

/// The worker's turns.
#[derive(Debug, Default)]
pub struct Compressor {
    queue: Mutex<Queue>,
}

#[derive(Debug, Default)]
struct Queue {
    /// A turn is out.
    busy: bool,
    waiting: Vec<Waiter>,
}

#[derive(Debug)]
struct Waiter {
    id: SessionId,
    resident_bytes: u64,
    grant: oneshot::Sender<Turn>,
}

/// A session's turn to compress, until it drops it.
#[derive(Debug)]
pub struct Turn {
    /// `None` once the turn was never taken, so dropping it hands nothing on.
    compressor: Option<Arc<Compressor>>,
}

/// The worker's one set of turns.
static SHARED: LazyLock<Arc<Compressor>> = LazyLock::new(Arc::default);

impl Compressor {
    /// The turns every session in this worker takes.
    #[must_use]
    pub fn shared() -> Arc<Self> {
        Arc::clone(&SHARED)
    }

    /// Ask for a turn for session `id`, whose terminal holds `resident_bytes`. The turn comes
    /// on the receiver: at once when no one has one, otherwise when it is this session's go,
    /// the largest waiting first. Dropping the receiver withdraws the request; asking again
    /// replaces it.
    pub fn ask(self: &Arc<Self>, id: SessionId, resident_bytes: u64) -> oneshot::Receiver<Turn> {
        let (grant, turn) = oneshot::channel();
        let mut queue = self.queue.lock();
        queue.waiting.retain(|w| w.id != id && !w.grant.is_closed());
        if queue.busy {
            queue.waiting.push(Waiter { id, resident_bytes, grant });
        } else if grant.send(Turn { compressor: Some(Arc::clone(self)) }).map_err(disarm).is_ok() {
            queue.busy = true;
        }
        turn
    }

    /// The turn that was out is done: hand it to the largest waiter still waiting.
    fn release(self: Arc<Self>) {
        let mut queue = self.queue.lock();
        while let Some(at) = largest(&queue.waiting) {
            let waiter = queue.waiting.swap_remove(at);
            let turn = Turn { compressor: Some(Arc::clone(&self)) };
            if waiter.grant.send(turn).map_err(disarm).is_ok() {
                return;
            }
        }
        queue.busy = false;
    }
}

/// Where the largest waiter stands.
fn largest(waiting: &[Waiter]) -> Option<usize> {
    waiting.iter().enumerate().max_by_key(|(_, w)| w.resident_bytes).map(|(at, _)| at)
}

/// A turn that could not be handed over: it was never out, so it hands nothing on.
fn disarm(mut turn: Turn) {
    turn.compressor = None;
}

impl Drop for Turn {
    fn drop(&mut self) {
        if let Some(compressor) = self.compressor.take() {
            compressor.release();
        }
    }
}

#[cfg(test)]
mod tests;
