//! The host's clipboard as this client sees it: the representations of the host's latest offer
//! that were fetched, or are on their way.
//!
//! The UI puts promises on the local pasteboard for what the host announced. When something
//! pastes, the pasteboard asks for the bytes on the main thread and must have them before it
//! returns; [`ClipCache::wait`] gives them from here, asking the host once and waiting a bounded
//! time for the answer, which the link's tasks put here ([`ClipCache::fill`]) from a `Data`
//! message or a bulk stream.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use slopty_net::ClientMsg;
use slopty_proto::transfer::{ClipMsg, INLINE_CLIP_BYTES, Offer};
use tokio::sync::mpsc;

/// Representations up to this size are fetched as soon as the host announces them, so a paste
/// finds them here; bigger ones are fetched when something pastes them.
pub const PREFETCH_BYTES: u64 = 1 << 20;

/// The largest clipboard representation this client takes: past it, a paste is a file
/// transfer.
pub const MAX_CLIP_BYTES: u64 = 256 << 20;

#[derive(Debug)]
enum Slot {
    Asked,
    Ready(Vec<u8>),
    Gone,
}

#[derive(Debug, Default)]
struct State {
    /// The host's latest offer.
    generation: Option<u64>,
    slots: HashMap<(u64, String), Slot>,
}

/// The fetched representations of the host's latest offer.
#[derive(Debug)]
pub struct ClipCache {
    state: Mutex<State>,
    changed: Condvar,
    out: mpsc::Sender<ClientMsg>,
}

impl ClipCache {
    /// A cache that asks the host on `out`.
    #[must_use]
    pub fn new(out: mpsc::Sender<ClientMsg>) -> Self {
        Self { state: Mutex::default(), changed: Condvar::new(), out }
    }

    /// The host announced `offer`: the older offers' bytes go, the inline ones are here at
    /// once, and the small ones are asked for now.
    pub fn offer(&self, offer: &Offer) {
        let mut state = self.state.lock();
        state.generation = Some(offer.generation);
        state.slots.retain(|(generation, _), _| *generation == offer.generation);
        for item in &offer.items {
            let key = (offer.generation, item.uti.clone());
            if let Some(bytes) = &item.inline {
                state.slots.insert(key, Slot::Ready(bytes.clone()));
            } else if item.size <= PREFETCH_BYTES && !state.slots.contains_key(&key) {
                state.slots.insert(key, Slot::Asked);
                self.ask(offer.generation, &item.uti);
            }
        }
        drop(state);
        self.changed.notify_all();
    }

    fn ask(&self, generation: u64, uti: &str) {
        let fetch = ClipMsg::Fetch { generation, uti: uti.to_owned() };
        if let Err(e) = self.out.try_send(ClientMsg::Clip(fetch)) {
            tracing::debug!(error = %e, "clipboard fetch not sent");
        }
    }

    /// Bytes of `uti` in offer `generation` arrived.
    pub fn fill(&self, generation: u64, uti: String, bytes: Vec<u8>) {
        let mut state = self.state.lock();
        if state.generation.is_some_and(|g| g != generation) {
            return;
        }
        state.slots.insert((generation, uti), Slot::Ready(bytes));
        drop(state);
        self.changed.notify_all();
    }

    /// Offer `generation` is gone on the host: anyone waiting on it stops.
    pub fn gone(&self, generation: u64) {
        let mut state = self.state.lock();
        for ((g, _), slot) in &mut state.slots {
            if *g == generation {
                *slot = Slot::Gone;
            }
        }
        drop(state);
        self.changed.notify_all();
    }

    /// A clipboard message for this cache. Returns `true` when it was only the cache's
    /// business (data, or an offer withdrawn), so the UI need not hear it.
    pub fn on_control(&self, msg: &ClipMsg) -> bool {
        match msg {
            ClipMsg::Data { generation, uti, bytes } => {
                self.fill(*generation, uti.clone(), bytes.clone());
                true
            }
            ClipMsg::Unavailable { generation } => {
                self.gone(*generation);
                true
            }
            ClipMsg::Offer(offer) => {
                self.offer(offer);
                false
            }
            ClipMsg::Watch(_) | ClipMsg::Fetch { .. } => false,
        }
    }

    /// The bytes of `uti` in offer `generation`: from the cache, else asked for and waited on
    /// for at most `wait`. `None` when the offer is gone or the host did not answer in time.
    /// Blocks the calling thread; the answer arrives on the link's tasks, never this thread.
    #[must_use]
    pub fn wait(&self, generation: u64, uti: &str, wait: Duration) -> Option<Vec<u8>> {
        let deadline = Instant::now().checked_add(wait)?;
        let key = (generation, uti.to_owned());
        let mut state = self.state.lock();
        if !state.slots.contains_key(&key) {
            state.slots.insert(key.clone(), Slot::Asked);
            self.ask(generation, uti);
        }
        loop {
            match state.slots.get(&key) {
                Some(Slot::Ready(bytes)) => return Some(bytes.clone()),
                Some(Slot::Gone) | None => return None,
                Some(Slot::Asked) => {}
            }
            if self.changed.wait_until(&mut state, deadline).timed_out() {
                drop(state);
                tracing::debug!(generation, uti, "clipboard fetch timed out");
                return None;
            }
        }
    }
}

/// How an answer to a host's fetch travels: inline on the control stream when it fits.
#[must_use]
pub const fn fits_inline(len: usize) -> bool {
    len <= INLINE_CLIP_BYTES
}

#[cfg(test)]
mod tests {
    use slopty_core::ClientId;
    use slopty_proto::transfer::{ClipItem, Peer};

    use super::*;

    fn offer(generation: u64, items: Vec<ClipItem>) -> Offer {
        Offer { origin: Peer::Client(ClientId::new()), generation, items }
    }

    fn item(uti: &str, size: u64, inline: Option<&[u8]>) -> ClipItem {
        ClipItem { uti: uti.to_owned(), size, hash: [0; 32], inline: inline.map(<[u8]>::to_vec) }
    }

    fn fetches(rx: &mut mpsc::Receiver<ClientMsg>) -> Vec<(u64, String)> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|m| match m {
                ClientMsg::Clip(ClipMsg::Fetch { generation, uti }) => Some((generation, uti)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn inline_text_is_here_at_once_and_small_pictures_are_asked_for_on_the_offer() {
        let (tx, mut rx) = mpsc::channel(8);
        let cache = ClipCache::new(tx);
        cache.offer(&offer(
            3,
            vec![
                item("public.utf8-plain-text", 2, Some(b"hi")),
                item("public.png", 900, None),
                item("public.tiff", PREFETCH_BYTES + 1, None),
            ],
        ));
        assert_eq!(fetches(&mut rx), [(3, "public.png".to_owned())], "the big one waits");
        let text = cache.wait(3, "public.utf8-plain-text", Duration::ZERO);
        assert_eq!(text.as_deref(), Some(&b"hi"[..]));
        cache.fill(3, "public.png".to_owned(), vec![1, 2]);
        assert_eq!(cache.wait(3, "public.png", Duration::ZERO), Some(vec![1, 2]));
    }

    #[test]
    fn a_paste_asks_once_and_waits_for_the_answer_from_another_thread() {
        let (tx, mut rx) = mpsc::channel(8);
        let cache = std::sync::Arc::new(ClipCache::new(tx));
        cache.offer(&offer(4, vec![item("public.tiff", PREFETCH_BYTES + 1, None)]));
        let filler = std::sync::Arc::clone(&cache);
        let answer = std::thread::spawn(move || {
            while filler.state.lock().slots.is_empty() {
                std::thread::yield_now();
            }
            filler.fill(4, "public.tiff".to_owned(), vec![7; 3]);
        });
        let got = cache.wait(4, "public.tiff", Duration::from_secs(10));
        answer.join().unwrap();
        assert_eq!(got, Some(vec![7; 3]));
        assert_eq!(fetches(&mut rx), [(4, "public.tiff".to_owned())]);
    }

    #[test]
    fn a_withdrawn_or_superseded_offer_answers_nothing_and_does_not_hang() {
        let (tx, _rx) = mpsc::channel(8);
        let cache = ClipCache::new(tx);
        cache.offer(&offer(5, vec![item("public.png", 10, None)]));
        cache.gone(5);
        assert_eq!(cache.wait(5, "public.png", Duration::from_secs(5)), None);
        cache.offer(&offer(6, Vec::new()));
        cache.fill(5, "public.png".to_owned(), vec![1]);
        assert_eq!(cache.wait(5, "public.png", Duration::from_millis(10)), None, "stale data");
    }
}
