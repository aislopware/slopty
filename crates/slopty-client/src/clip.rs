//! The clipboard, this client's half.
//!
//! [`ClipCache`] holds a worker's clipboard as this client sees it: the representations of the
//! worker's latest offer that were fetched, or are on their way. It fetches ahead of any paste
//! what the worker knows the size of, under a budget ([`prefetch_budget`]), so an ordinary paste
//! of that is answered from memory; the rest is fetched when something pastes it, since finding
//! out its size would make the worker read it whole. When something pastes, the
//! pasteboard asks for the bytes on the main thread and must have them before it returns;
//! [`ClipCache::fetch`] gives them from here, asking the worker once and waiting a bounded time
//! for the answer, which the link's tasks put here ([`ClipCache::on_control`], [`ClipCache::fill`])
//! from a `Data` message or a bulk stream.
//!
//! `ClipSync` (Apple only) keeps this client's pasteboard in step with the workers: it announces
//! this client's clipboard, relays one worker's to another, and puts a worker's offer on the
//! pasteboard as promises.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use slopty_net::ClientMsg;
use slopty_proto::transfer::{ClipMsg, Hash, INLINE_CLIP_BYTES, Offer, RepRef, Source};
use tokio::sync::mpsc;

#[cfg(target_vendor = "apple")]
mod local;
#[cfg(target_vendor = "apple")]
pub use local::{
    Answer, ClipFiles, ClipSync, LinkNow, Place, ShellPaste, file_url, file_url_path,
    file_url_paths, provider, relay,
};

/// Most a link fetches ahead of a paste for one offer.
pub const PREFETCH_MAX: u64 = 8 << 20;

/// How much of the link's time one offer's prefetch may take: its budget is what the link
/// carries in this long.
pub const PREFETCH_WINDOW: Duration = Duration::from_millis(250);

/// The largest clipboard representation this client takes: past it, a paste is a file
/// transfer.
pub const MAX_CLIP_BYTES: u64 = 256 << 20;

/// The digest clipboard sync names contents by.
#[must_use]
pub fn digest(bytes: &[u8]) -> Hash {
    *blake3::hash(bytes).as_bytes()
}

/// What one offer may fetch ahead of a paste.
///
/// That is [`PREFETCH_MAX`], or what a path of round trip `rtt` and congestion window `cwnd`
/// bytes carries in [`PREFETCH_WINDOW`], whichever is less. Nothing is known of a path yet: the
/// whole [`PREFETCH_MAX`].
#[must_use]
pub fn prefetch_budget(path: Option<(Duration, u64)>) -> u64 {
    let Some((rtt, cwnd)) = path.filter(|(rtt, _)| !rtt.is_zero()) else { return PREFETCH_MAX };
    let window = PREFETCH_WINDOW.as_micros();
    let carried = u128::from(cwnd).saturating_mul(window).checked_div(rtt.as_micros());
    carried.map_or(PREFETCH_MAX, |c| u64::try_from(c).unwrap_or(u64::MAX).min(PREFETCH_MAX))
}

/// The answer to a fetch.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Fetched {
    /// The bytes.
    Data(Vec<u8>),
    /// Past the fetch's cap, with its size.
    TooBig(u64),
    /// Gone: the offer moved on, the link went, or no answer came in time.
    Gone,
}

#[derive(Debug)]
enum Slot {
    /// Asked for, with the cap it was asked with.
    Asked(Option<u64>),
    Ready(Vec<u8>),
    TooBig(u64),
    Gone,
}

#[derive(Debug, Default)]
struct State {
    /// The worker's latest offer.
    source: Option<Source>,
    slots: HashMap<RepRef, Slot>,
    /// When bytes of a representation last arrived on its stream: a paste waits on while they
    /// keep coming.
    heard: HashMap<RepRef, Instant>,
    /// The link is gone: nothing more is asked, and nothing more will answer.
    closed: bool,
}

/// How a cache learns its link's prefetch budget, asked at every offer.
pub type Budget = Box<dyn Fn() -> u64 + Send + Sync>;

/// The fetched representations of a worker's latest offer.
pub struct ClipCache {
    state: Mutex<State>,
    changed: Condvar,
    out: mpsc::Sender<ClientMsg>,
    budget: Budget,
}

impl std::fmt::Debug for ClipCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipCache").field("state", &self.state).finish_non_exhaustive()
    }
}

impl ClipCache {
    /// A cache that asks the worker on `out`, fetching ahead within what `budget` says.
    #[must_use]
    pub fn new(out: mpsc::Sender<ClientMsg>, budget: Budget) -> Self {
        Self { state: Mutex::default(), changed: Condvar::new(), out, budget }
    }

    /// The worker announced `offer`: the older offers' bytes go, the inline ones are here at
    /// once, and those whose size the worker knows are fetched ahead while they fit the budget.
    /// One of unknown size waits for a paste: finding out its size would make the worker read
    /// it whole, a 200 MB copy included. A secret is never fetched ahead.
    pub fn offer(&self, offer: &Offer) {
        let mut left = if offer.concealed { 0 } else { (self.budget)() };
        let mut state = self.state.lock();
        state.source = Some(offer.source());
        state.slots.clear();
        state.heard.clear();
        for (n, rep) in offer.reps() {
            let key = offer.rep_ref(n, rep.kind.clone());
            if let Some(bytes) = &rep.inline {
                state.slots.insert(key, Slot::Ready(bytes.clone()));
                continue;
            }
            if offer.concealed || state.closed {
                continue;
            }
            if let Some(size) = rep.size.filter(|size| *size <= left)
                && self.ask(&key, None, false)
            {
                left = left.saturating_sub(size);
                state.slots.insert(key, Slot::Asked(None));
            }
        }
        drop(state);
        self.changed.notify_all();
    }

    /// Ask the worker for `rep`; whether the fetch went out.
    fn ask(&self, rep: &RepRef, max: Option<u64>, urgent: bool) -> bool {
        let fetch = ClipMsg::Fetch { rep: rep.clone(), max, urgent };
        match self.out.try_send(ClientMsg::Clip(fetch)) {
            Ok(()) => true,
            Err(e) => {
                tracing::debug!(error = %e, "clipboard fetch not sent");
                false
            }
        }
    }

    /// The link is gone: whatever waits on it stops now, and later waits answer from what
    /// arrived, without asking.
    pub fn close(&self) {
        let mut state = self.state.lock();
        state.closed = true;
        for slot in state.slots.values_mut() {
            if matches!(slot, Slot::Asked(_)) {
                *slot = Slot::Gone;
            }
        }
        drop(state);
        self.changed.notify_all();
    }

    /// Bytes of `rep` arrived.
    pub fn fill(&self, rep: RepRef, bytes: Vec<u8>) {
        let mut state = self.state.lock();
        if state.source.is_some_and(|s| s != rep.source) {
            return;
        }
        state.heard.remove(&rep);
        state.slots.insert(rep, Slot::Ready(bytes));
        drop(state);
        self.changed.notify_all();
    }

    /// Bytes of `rep` are arriving on its stream: a paste waiting on it waits on while they keep
    /// coming.
    pub fn receiving(&self, rep: &RepRef) {
        self.state.lock().heard.insert(rep.clone(), Instant::now());
        self.changed.notify_all();
    }

    /// `rep` is past `size`, past the cap of the fetch this answers. A fetch asked since with a
    /// larger cap, or none, still waits for its own answer, so it is left alone.
    pub fn too_big(&self, rep: &RepRef, size: u64) {
        let mut state = self.state.lock();
        if let Some(slot) = state.slots.get_mut(rep)
            && let Slot::Asked(cap) = slot
            && size > cap.unwrap_or(MAX_CLIP_BYTES)
        {
            *slot = Slot::TooBig(size);
        }
        drop(state);
        self.changed.notify_all();
    }

    /// `rep` will not come (its stream was cut, or it was past [`MAX_CLIP_BYTES`]).
    pub fn lost(&self, rep: &RepRef) {
        let mut state = self.state.lock();
        state.heard.remove(rep);
        if let Some(slot) = state.slots.get_mut(rep) {
            *slot = Slot::Gone;
        }
        drop(state);
        self.changed.notify_all();
    }

    /// Offer `source` is gone on the worker: anyone waiting on it stops.
    pub fn gone(&self, source: Source) {
        let mut state = self.state.lock();
        for (rep, slot) in &mut state.slots {
            if rep.source == source {
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
            ClipMsg::Data { rep, bytes } => {
                self.fill(rep.clone(), bytes.clone());
                true
            }
            ClipMsg::TooBig { rep, size } => {
                self.too_big(rep, *size);
                true
            }
            ClipMsg::Unavailable { source } => {
                self.gone(*source);
                true
            }
            ClipMsg::Offer(offer) => {
                self.offer(offer);
                false
            }
            ClipMsg::Watch(_) | ClipMsg::Fetch { .. } => false,
        }
    }

    /// Representation `rep`, capped at `max`: from the cache, else asked for at once (a paste
    /// waits on it) and waited on for `wait`, and for as long again after each chunk of its
    /// stream, so a big one on a slow link is not given up while it moves. Blocks the calling
    /// thread; the answer arrives on the link's tasks, never this thread.
    #[must_use]
    pub fn fetch(&self, rep: &RepRef, max: Option<u64>, wait: Duration) -> Fetched {
        let asked = Instant::now();
        let over = |size: u64| max.is_some_and(|m| size > m) || size > MAX_CLIP_BYTES;
        let mut state = self.state.lock();
        loop {
            match state.slots.get(rep) {
                Some(Slot::Ready(bytes)) => {
                    let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                    return if over(size) {
                        Fetched::TooBig(size)
                    } else {
                        Fetched::Data(bytes.clone())
                    };
                }
                Some(Slot::TooBig(size)) if over(*size) => return Fetched::TooBig(*size),
                Some(Slot::Gone) => return Fetched::Gone,
                Some(Slot::Asked(asked)) if asked.is_none_or(|a| max.is_some_and(|m| m <= a)) => {}
                Some(Slot::TooBig(_) | Slot::Asked(_)) | None => {
                    if state.closed || !self.ask(rep, max, true) {
                        return Fetched::Gone;
                    }
                    state.slots.insert(rep.clone(), Slot::Asked(max));
                }
            }
            let since = state.heard.get(rep).map_or(asked, |heard| (*heard).max(asked));
            let Some(deadline) = since.checked_add(wait) else { return Fetched::Gone };
            if self.changed.wait_until(&mut state, deadline).timed_out()
                && state.heard.get(rep).is_none_or(|heard| *heard <= since)
            {
                drop(state);
                tracing::debug!(item = rep.item, "clipboard fetch timed out");
                return Fetched::Gone;
            }
        }
    }
}

/// How an answer to a worker's fetch travels: inline on the control stream when it fits.
#[must_use]
pub const fn fits_inline(len: usize) -> bool {
    len <= INLINE_CLIP_BYTES
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use slopty_core::WorkerId;
    use slopty_proto::transfer::{ClipEntry, ClipFormat, ClipType, Peer, Rep};

    use super::*;

    fn offer(generation: u64, reps: Vec<Rep>) -> Offer {
        Offer {
            origin: Peer::Worker(WorkerId::new()),
            generation,
            age_ms: 0,
            concealed: false,
            items: reps.into_iter().map(|r| ClipEntry { reps: vec![r] }).collect(),
        }
    }

    fn rep(format: ClipFormat, size: Option<u64>, inline: Option<&[u8]>) -> Rep {
        Rep { kind: ClipType::Format(format), size, hash: None, inline: inline.map(<[u8]>::to_vec) }
    }

    fn cache(budget: u64) -> (ClipCache, mpsc::Receiver<ClientMsg>) {
        let (tx, rx) = mpsc::channel(64);
        (ClipCache::new(tx, Box::new(move || budget)), rx)
    }

    fn fetches(rx: &mut mpsc::Receiver<ClientMsg>) -> Vec<(u16, Option<u64>, bool)> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|m| match m {
                ClientMsg::Clip(ClipMsg::Fetch { rep, max, urgent }) => {
                    Some((rep.item, max, urgent))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_budget_is_what_the_link_carries_in_a_quarter_second() {
        assert_eq!(prefetch_budget(None), PREFETCH_MAX, "an unknown path");
        let slow = prefetch_budget(Some((Duration::from_millis(50), 100_000)));
        assert_eq!(slow, 500_000, "2 MB/s for 250 ms");
        assert_eq!(prefetch_budget(Some((Duration::from_millis(1), 10 << 20))), PREFETCH_MAX);
        assert_eq!(prefetch_budget(Some((Duration::ZERO, 1))), PREFETCH_MAX);
    }

    /// Inline text is here at once; known sizes that fit are fetched ahead, and nothing of
    /// unknown size is, which would make the worker read it whole. A paste fetches the rest
    /// whole.
    #[test]
    fn prefetch_stays_in_its_budget() {
        let (cache, mut rx) = cache(1_000);
        let o = offer(
            3,
            vec![
                rep(ClipFormat::Text, Some(2), Some(b"hi")),
                rep(ClipFormat::Rtf, Some(600), None),
                rep(ClipFormat::Tiff, Some(5_000), None),
                rep(ClipFormat::Html, Some(300), None),
                rep(ClipFormat::Png, None, None),
                rep(ClipFormat::Rtf, Some(200), None),
            ],
        );
        cache.offer(&o);
        assert_eq!(
            fetches(&mut rx),
            [(1, None, false), (3, None, false)],
            "rtf and html fit; the tiff does not, the png is of unknown size, and the last rtf is past what is left"
        );
        let text = o.rep_ref(0, ClipType::Format(ClipFormat::Text));
        assert_eq!(cache.fetch(&text, None, Duration::ZERO), Fetched::Data(b"hi".to_vec()));
        let rtf = o.rep_ref(1, ClipType::Format(ClipFormat::Rtf));
        cache.fill(rtf.clone(), vec![b'r'; 600]);
        assert_eq!(cache.fetch(&rtf, None, Duration::ZERO), Fetched::Data(vec![b'r'; 600]));
        assert!(fetches(&mut rx).is_empty(), "prefetched bytes answer from memory");

        let png = o.rep_ref(4, ClipType::Format(ClipFormat::Png));
        assert_eq!(cache.fetch(&png, None, Duration::ZERO), Fetched::Gone, "timed out");
        assert_eq!(fetches(&mut rx), [(4, None, true)], "a paste's fetch is urgent and whole");
    }

    /// A capped fetch's answer that lands after a paste asked for the same representation
    /// whole leaves the paste waiting for its own answer.
    #[test]
    fn a_paste_during_a_capped_fetch_gets_the_whole() {
        let (cache, mut rx) = cache(0);
        let cache = Arc::new(cache);
        let o = offer(9, vec![rep(ClipFormat::Png, None, None)]);
        cache.offer(&o);
        let png = o.rep_ref(0, ClipType::Format(ClipFormat::Png));
        assert_eq!(cache.fetch(&png, Some(100), Duration::ZERO), Fetched::Gone, "asked, capped");
        let pasting = {
            let (cache, png) = (Arc::clone(&cache), png.clone());
            std::thread::spawn(move || cache.fetch(&png, None, Duration::from_secs(10)))
        };
        while !matches!(cache.state.lock().slots.get(&png), Some(Slot::Asked(None))) {
            std::thread::yield_now();
        }
        cache.too_big(&png, 5_000);
        cache.fill(png, vec![7; 5_000]);
        assert_eq!(pasting.join().unwrap(), Fetched::Data(vec![7; 5_000]));
        assert_eq!(fetches(&mut rx), [(0, Some(100), true), (0, None, true)]);
    }

    /// A paste waits on for as long as its bytes keep arriving, past the wait for them to start.
    #[test]
    fn a_paste_waits_on_while_its_bytes_keep_coming() {
        let (cache, _rx) = cache(0);
        let cache = Arc::new(cache);
        let o = offer(10, vec![rep(ClipFormat::Png, None, None)]);
        cache.offer(&o);
        let png = o.rep_ref(0, ClipType::Format(ClipFormat::Png));
        let wait = Duration::from_millis(200);
        let pasting = {
            let (cache, png) = (Arc::clone(&cache), png.clone());
            std::thread::spawn(move || cache.fetch(&png, None, wait))
        };
        let started = Instant::now();
        let mut next = started;
        while started.elapsed() < wait * 4 {
            if Instant::now() >= next {
                cache.receiving(&png);
                next = Instant::now().checked_add(wait / 4).unwrap();
            }
            std::thread::yield_now();
        }
        assert!(!pasting.is_finished(), "still waiting while bytes come");
        cache.fill(png, vec![1; 3]);
        assert_eq!(pasting.join().unwrap(), Fetched::Data(vec![1; 3]));
    }

    /// A secret is never fetched ahead: a paste fetches it.
    #[test]
    fn a_secret_is_fetched_only_by_a_paste() {
        let (cache, mut rx) = cache(PREFETCH_MAX);
        let o = Offer { concealed: true, ..offer(1, vec![rep(ClipFormat::Text, None, None)]) };
        cache.offer(&o);
        assert!(fetches(&mut rx).is_empty());
        let text = o.rep_ref(0, ClipType::Format(ClipFormat::Text));
        assert_eq!(cache.fetch(&text, None, Duration::ZERO), Fetched::Gone);
        assert_eq!(fetches(&mut rx), [(0, None, true)]);
    }

    #[test]
    fn a_paste_asks_once_and_waits_for_the_answer_from_another_thread() {
        let (cache, mut rx) = cache(0);
        let cache = Arc::new(cache);
        let o = offer(4, vec![rep(ClipFormat::Tiff, Some(MAX_CLIP_BYTES), None)]);
        cache.offer(&o);
        let tiff = o.rep_ref(0, ClipType::Format(ClipFormat::Tiff));
        let filler = Arc::clone(&cache);
        let key = tiff.clone();
        let answer = std::thread::spawn(move || {
            while filler.state.lock().slots.is_empty() {
                std::thread::yield_now();
            }
            filler.fill(key, vec![7; 3]);
        });
        let got = cache.fetch(&tiff, None, Duration::from_secs(10));
        answer.join().unwrap();
        assert_eq!(got, Fetched::Data(vec![7; 3]));
        assert_eq!(fetches(&mut rx), [(0, None, true)]);
    }

    #[test]
    fn a_withdrawn_or_superseded_offer_answers_nothing_and_does_not_hang() {
        let (cache, _rx) = cache(PREFETCH_MAX);
        let o = offer(5, vec![rep(ClipFormat::Png, Some(10), None)]);
        cache.offer(&o);
        let png = o.rep_ref(0, ClipType::Format(ClipFormat::Png));
        cache.gone(o.source());
        assert_eq!(cache.fetch(&png, None, Duration::from_secs(5)), Fetched::Gone);
        cache.offer(&offer(6, Vec::new()));
        cache.fill(png.clone(), vec![1]);
        assert_eq!(cache.fetch(&png, None, Duration::from_millis(10)), Fetched::Gone, "stale data");
    }

    /// A paste waiting on a link that goes stops then, not at its deadline; one on a link that
    /// is gone answers at once from what arrived, and asks nothing.
    #[test]
    fn a_paste_on_a_dead_link_answers_at_once() {
        let (cache, mut rx) = cache(0);
        let cache = Arc::new(cache);
        let o = offer(
            8,
            vec![
                rep(ClipFormat::Tiff, Some(MAX_CLIP_BYTES), None),
                rep(ClipFormat::Rtf, Some(1), None),
            ],
        );
        cache.offer(&o);
        let (tiff, rtf) = (
            o.rep_ref(0, ClipType::Format(ClipFormat::Tiff)),
            o.rep_ref(1, ClipType::Format(ClipFormat::Rtf)),
        );
        cache.fill(rtf.clone(), vec![5]);
        let closer = Arc::clone(&cache);
        let close = std::thread::spawn(move || {
            while closer.state.lock().slots.len() < 2 {
                std::thread::yield_now();
            }
            closer.close();
        });
        let started = Instant::now();
        assert_eq!(cache.fetch(&tiff, None, Duration::from_secs(30)), Fetched::Gone);
        close.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        assert_eq!(fetches(&mut rx), [(0, None, true)]);

        let png = o.rep_ref(0, ClipType::Format(ClipFormat::Png));
        assert_eq!(cache.fetch(&png, None, Duration::from_secs(30)), Fetched::Gone);
        assert_eq!(cache.fetch(&rtf, None, Duration::from_secs(30)), Fetched::Data(vec![5]));
        assert!(fetches(&mut rx).is_empty(), "a closed link is asked nothing");

        let (tx, rx) = mpsc::channel(8);
        let orphan = ClipCache::new(tx, Box::new(|| 0));
        drop(rx);
        let started = Instant::now();
        assert_eq!(orphan.fetch(&png, None, Duration::from_secs(30)), Fetched::Gone);
        assert!(started.elapsed() < Duration::from_secs(5), "an unsendable fetch is not waited on");
    }
}
