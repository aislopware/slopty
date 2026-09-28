//! Clipboard sync, the worker's half: announce a change, never push it.
//!
//! - **Watch only while wanted.** The pasteboard is read only while some client said it wants the
//!   worker's changes ([`Clipboard::watch`]); [`Clipboard::watched`] wakes the poller. A change
//!   made while nobody watched is not announced when someone starts to: it was made on the worker,
//!   not by anyone at a client.
//! - **Read only when reads are free.** macOS asks the person before a program reads the general
//!   pasteboard unless they allowed it ([`Access`]), and a poll is no paste of theirs, so while
//!   reads are not free nothing is read or announced (`docs/decisions/platform.md`, "The worker's
//!   pasteboard alert"). A paste from a client still writes: writing never asks.
//! - **Announce.** [`Clipboard::poll`] turns a change into an [`Offer`]: plain text of at most
//!   [`INLINE_CLIP_BYTES`] inline, every other representation listed with its size and digest and
//!   kept here for [`Clipboard::fetch`]. Concealed and transient contents are skipped.
//! - **Paste.** A client's offer is only recorded ([`Clipboard::offered`]). It reaches the
//!   pasteboard when that client's paste chord arrives ([`Clipboard::paste`]): inline text at once,
//!   anything else once fetched ([`Clipboard::supply`], [`Clipboard::write_incoming`]).
//! - **Break echoes.** Every write carries [`ORIGIN_TYPE`], naming who the contents came from; the
//!   `changeCount` a write leaves is remembered and never announced, contents whose origin is this
//!   worker are never announced, and contents whose digest matches what was last written or
//!   announced are not announced again (the backstop for a peer that drops the origin, as Universal
//!   Clipboard does).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use bytes::Bytes;
use parking_lot::Mutex;
use slopty_input::pasteboard::{
    Board, CONCEALED_TYPE, Item, ORIGIN_TYPE, TRANSIENT_TYPE, board_type,
};
use slopty_platform::pasteboard_access::Access as ReadAccess;
use slopty_proto::transfer::{
    ClipFormat, ClipItem, Hash, INLINE_CLIP_BYTES, Offer, Peer, origin_bytes, parse_origin,
};
use tokio::sync::watch;

/// One client connection (its QUIC `stable_id`): a client that reconnects is a new link, and the
/// old one ending must not take the new one's watch with it.
pub type Link = usize;

/// Largest representation read off the pasteboard or fetched for it: a Retina screenshot as
/// TIFF is tens of megabytes; anything past this is a file, not a paste.
pub const MAX_REP_BYTES: u64 = 64 << 20;

/// The digest clipboard sync names contents by.
#[must_use]
pub fn digest(bytes: &[u8]) -> Hash {
    blake3::hash(bytes).into()
}

/// `path` as a `file://` URL, the bytes of a [`ClipFormat::FileUrls`] item.
#[must_use]
fn file_url(path: &Path) -> String {
    use std::fmt::Write as _;
    use std::os::unix::ffi::OsStrExt as _;
    let mut url = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            url.push(char::from(b));
        } else {
            let _infallible = write!(url, "%{b:02X}");
        }
    }
    url
}

/// How reading a board's contents goes for this process, asked before every poll.
pub trait Access {
    /// Whether a read the person did not make goes through, and if not, why.
    fn access(&self) -> ReadAccess;
}

/// The general pasteboard asks as macOS says; a named one always lets this process read it.
#[cfg(target_os = "macos")]
impl Access for slopty_input::pasteboard::MacBoard {
    fn access(&self) -> ReadAccess {
        if self.name().is_some() {
            ReadAccess::Allowed
        } else {
            slopty_platform::pasteboard_access::general()
        }
    }
}

/// Nothing to read, and nothing that asks.
impl Access for slopty_input::pasteboard::Unsupported {
    fn access(&self) -> ReadAccess {
        ReadAccess::Allowed
    }
}

/// What a client's paste chord needs before it can go to the window.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Paste {
    /// The pasteboard holds what the client copied (or it never offered anything): send the
    /// chord.
    Ready,
    /// Fetch these representations of offer `generation` from the client first, then
    /// [`Clipboard::write_incoming`].
    Fetch {
        /// The client's offer.
        generation: u64,
        /// Representations to fetch.
        formats: Vec<ClipFormat>,
    },
}

/// The worker's clipboard state over pasteboard `B`.
#[derive(Debug)]
pub struct Clipboard<B> {
    board: B,
    /// This worker, the origin of what it announces.
    me: Peer,
    state: Mutex<State>,
    watched: watch::Sender<bool>,
}

#[derive(Debug, Default)]
struct State {
    /// Links that want the worker's changes now.
    watchers: HashSet<Link>,
    /// The `changeCount` last handled: polled, written, or current when watching began.
    seen: isize,
    /// The last announced offer's generation.
    generation: u64,
    /// What the last offer announced, for fetches.
    offered: Option<(u64, Vec<(ClipFormat, Bytes)>)>,
    /// Digests of what was last announced or written: contents among them are an echo.
    last: HashSet<Hash>,
    /// Each link's last offer.
    incoming: HashMap<Link, Incoming>,
}

#[derive(Debug)]
struct Incoming {
    offer: Offer,
    fetched: HashMap<ClipFormat, Vec<u8>>,
    written: bool,
}

impl Incoming {
    /// Representations the paste still waits for.
    fn missing(&self) -> Vec<ClipFormat> {
        self.offer
            .items
            .iter()
            .filter(|i| writable(i.format) && i.inline.is_none() && i.size <= MAX_REP_BYTES)
            .filter(|i| !self.fetched.contains_key(&i.format))
            .map(|i| i.format)
            .collect()
    }
}

impl State {
    /// Add or remove a watcher: whether anyone watched before, and whether anyone does now.
    fn set_watch(&mut self, client: Link, on: bool) -> (bool, bool) {
        let was = !self.watchers.is_empty();
        if on {
            self.watchers.insert(client);
        } else {
            self.watchers.remove(&client);
        }
        (was, !self.watchers.is_empty())
    }

    /// Whether `count` is a change to look at.
    fn unseen(&self, count: isize) -> bool {
        !self.watchers.is_empty() && self.seen != count
    }

    /// Announce `reps` as the next offer from `me`, unless they are what was last announced or
    /// written.
    fn announce(&mut self, me: Peer, reps: Vec<(ClipFormat, Bytes)>) -> Option<Offer> {
        let (_format, key) = reps.first()?;
        if self.last.contains(&digest(key)) {
            return None;
        }
        self.generation = self.generation.wrapping_add(1);
        let items = reps
            .iter()
            .map(|(format, bytes)| ClipItem {
                format: *format,
                size: bytes.len() as u64,
                hash: digest(bytes),
                inline: (*format == ClipFormat::Text && bytes.len() <= INLINE_CLIP_BYTES)
                    .then(|| bytes.to_vec()),
            })
            .collect::<Vec<_>>();
        self.last = items.iter().map(|i| i.hash).collect();
        self.offered = Some((self.generation, reps));
        Some(Offer { origin: me, generation: self.generation, items })
    }

    fn fetch(&self, generation: u64, format: ClipFormat) -> Option<Bytes> {
        let (current, reps) = self.offered.as_ref()?;
        if *current != generation {
            return None;
        }
        reps.iter().find(|(f, _b)| *f == format).map(|(_f, b)| b.clone())
    }

    /// What `client`'s paste needs; `None` when its offer is whole and still to be written.
    fn paste(&mut self, client: Link) -> Option<Paste> {
        let Some(inc) = self.incoming.get_mut(&client) else { return Some(Paste::Ready) };
        if inc.written {
            return Some(Paste::Ready);
        }
        // The same contents are there already (an echo of the worker's own offer).
        if inc.offer.items.first().is_some_and(|i| self.last.contains(&i.hash)) {
            inc.written = true;
            return Some(Paste::Ready);
        }
        let formats = inc.missing();
        (!formats.is_empty()).then_some(Paste::Fetch { generation: inc.offer.generation, formats })
    }

    fn supply(
        &mut self,
        client: Link,
        generation: u64,
        format: ClipFormat,
        bytes: Vec<u8>,
    ) -> bool {
        let Some(inc) = self.incoming.get_mut(&client) else { return false };
        if inc.offer.generation != generation {
            return false;
        }
        let listed = inc.offer.items.iter().find(|i| i.format == format);
        if listed.is_none_or(|i| i.hash != digest(&bytes)) {
            tracing::debug!(%client, ?format, "clipboard data that does not match its offer; dropped");
        } else {
            inc.fetched.insert(format, bytes);
        }
        inc.missing().is_empty()
    }

    /// `client`'s offer as one pasteboard item stamped with its origin, and the digests of what
    /// it holds; `None` when it was written already or nothing of it is here.
    fn take_incoming(&mut self, client: Link) -> Option<(Item, HashSet<Hash>)> {
        let inc = self.incoming.get_mut(&client)?;
        if std::mem::replace(&mut inc.written, true) {
            return None;
        }
        let mut reps: Item = Vec::new();
        let mut hashes = HashSet::new();
        for item in &inc.offer.items {
            if !writable(item.format) {
                continue;
            }
            if let Some(bytes) = item.inline.clone().or_else(|| inc.fetched.remove(&item.format)) {
                hashes.insert(item.hash);
                reps.push((board_type(item.format), bytes));
            }
        }
        if reps.is_empty() {
            return None;
        }
        reps.push((ORIGIN_TYPE.to_owned(), origin_bytes(inc.offer.origin, inc.offer.generation)));
        Some((reps, hashes))
    }
}

impl<B: Board + Access> Clipboard<B> {
    /// Sync over `board` on behalf of `me`.
    pub fn new(board: B, me: Peer) -> Self {
        Self { board, me, state: Mutex::default(), watched: watch::Sender::new(false) }
    }

    /// The pasteboard.
    pub const fn board(&self) -> &B {
        &self.board
    }

    /// Whether any client watches, now and on every change.
    #[must_use]
    pub fn watched(&self) -> watch::Receiver<bool> {
        self.watched.subscribe()
    }

    /// Whether `client` wants the worker's changes.
    #[must_use]
    pub fn is_watching(&self, client: Link) -> bool {
        self.state.lock().watchers.contains(&client)
    }

    /// `client` wants the worker's changes (`on`), or no longer does. Watching starts from the
    /// pasteboard as it is.
    pub fn watch(&self, client: Link, on: bool) {
        let (was, now) = self.state.lock().set_watch(client, on);
        if now && !was {
            let count = self.board.change_count();
            self.state.lock().seen = count;
        }
        self.watched.send_if_modified(|w| std::mem::replace(w, now) != now);
    }

    /// `client` is gone: it watches nothing and its offer is void.
    pub fn forget(&self, client: Link) {
        self.watch(client, false);
        self.state.lock().incoming.remove(&client);
    }

    /// How reading the pasteboard goes now, for the doctor.
    pub fn access(&self) -> ReadAccess {
        self.board.access()
    }

    /// The pasteboard's change since the last poll or write, as an offer to announce; `None`
    /// when nobody watches, reads would ask the person, nothing changed, or the change is not
    /// to be announced.
    pub fn poll(&self) -> Option<Offer> {
        if self.state.lock().watchers.is_empty() || !self.board.access().reads_freely() {
            return None;
        }
        let count = self.board.change_count();
        if !self.state.lock().unseen(count) {
            return None;
        }
        let types = self.board.types();
        // A copy clears the pasteboard, which moves the count, and puts the new contents on
        // after, under that same count. An empty board may be a copy half done, so its count
        // is left unseen and read again on the next poll.
        if types.is_empty() {
            return None;
        }
        self.state.lock().seen = count;
        if types.iter().any(|t| t == CONCEALED_TYPE || t == TRANSIENT_TYPE) {
            return None;
        }
        if types.iter().any(|t| t == ORIGIN_TYPE)
            && self
                .board
                .data(ORIGIN_TYPE)
                .and_then(|b| parse_origin(&b))
                .is_some_and(|(peer, _)| peer == self.me)
        {
            return None;
        }
        let reps = self.read(&types);
        self.state.lock().announce(self.me, reps)
    }

    /// The representations of the pasteboard's first item clipboard sync carries, richest
    /// first; file URLs are every item's, one per line.
    fn read(&self, types: &[String]) -> Vec<(ClipFormat, Bytes)> {
        let mut reps = Vec::new();
        for format in ClipFormat::ALL {
            let kind = board_type(format);
            let bytes = if format == ClipFormat::FileUrls {
                let urls = self.board.file_urls();
                if urls.is_empty() {
                    continue;
                }
                urls.join("\n").into_bytes()
            } else if types.contains(&kind) {
                let Some(bytes) = self.board.data(&kind) else { continue };
                bytes
            } else {
                continue;
            };
            if bytes.len() as u64 <= MAX_REP_BYTES {
                reps.push((format, Bytes::from(bytes)));
            }
        }
        reps
    }

    /// Representation `format` of the worker's offer `generation`; `None` when that offer is not
    /// the current one (the pasteboard changed again) or never listed it.
    #[must_use]
    pub fn fetch(&self, generation: u64, format: ClipFormat) -> Option<Bytes> {
        self.state.lock().fetch(generation, format)
    }

    /// `client`'s clipboard changed to `offer`; it reaches the pasteboard on that client's
    /// next paste.
    pub fn offered(&self, client: Link, offer: Offer) {
        let incoming = Incoming { offer, fetched: HashMap::new(), written: false };
        self.state.lock().incoming.insert(client, incoming);
    }

    /// `client` pressed its paste chord: what must happen before the chord goes on. When the
    /// offer is whole already it is written here.
    pub fn paste(&self, client: Link) -> Paste {
        let plan = self.state.lock().paste(client);
        plan.unwrap_or_else(|| {
            let _written = self.write_incoming(client);
            Paste::Ready
        })
    }

    /// Bytes of representation `format` of `client`'s offer `generation` arrived. `true` once
    /// every representation the paste waits for is here.
    pub fn supply(
        &self,
        client: Link,
        generation: u64,
        format: ClipFormat,
        bytes: Vec<u8>,
    ) -> bool {
        self.state.lock().supply(client, generation, format, bytes)
    }

    /// Put `client`'s offer on the pasteboard, with what of it is here (inline text and what was
    /// fetched). `false` when there was nothing to write or the write failed.
    pub fn write_incoming(&self, client: Link) -> bool {
        let taken = self.state.lock().take_incoming(client);
        let Some((reps, hashes)) = taken else { return false };
        self.commit(&[reps], hashes)
    }

    /// Put `paths` on the pasteboard as file URLs, one item each, so ⌘V in Finder or an app
    /// pastes the files. Not announced: they are the clients' own files.
    pub fn write_files(&self, paths: &[impl AsRef<Path>]) -> bool {
        let url = board_type(ClipFormat::FileUrls);
        let generation = self.state.lock().generation;
        let mut items: Vec<Item> =
            paths.iter().map(|p| vec![(url.clone(), file_url(p.as_ref()).into_bytes())]).collect();
        let Some(first) = items.first_mut() else { return false };
        first.push((ORIGIN_TYPE.to_owned(), origin_bytes(self.me, generation)));
        self.commit(&items, HashSet::new())
    }

    fn commit(&self, items: &[Item], hashes: HashSet<Hash>) -> bool {
        // Known before the write lands: a poll that reads the new contents before `write`
        // returns must take them for this write, not a copy to announce back.
        let before = std::mem::replace(&mut self.state.lock().last, hashes);
        let Some(count) = self.board.write(items) else {
            tracing::warn!("pasteboard write failed");
            self.state.lock().last = before;
            return false;
        };
        self.state.lock().seen = count;
        true
    }
}

/// Whether a representation a client offers can go on the worker's pasteboard as it is: a
/// client's file URLs name files on the client, which travel as a transfer instead.
fn writable(format: ClipFormat) -> bool {
    format != ClipFormat::FileUrls
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicIsize, Ordering};

    use slopty_core::{ClientId, WorkerId};

    use super::*;

    /// A pasteboard in memory: one item, or several file URLs.
    #[derive(Default)]
    struct Fake {
        count: AtomicIsize,
        items: Mutex<Vec<Item>>,
        /// Run once inside the next `write`, after the contents land and before it returns.
        during_write: Mutex<Option<Box<dyn FnOnce() + Send>>>,
        /// Reading asks the person first, as the general pasteboard does until they allow it.
        asks: std::sync::atomic::AtomicBool,
        /// Reads of the contents so far.
        reads: std::sync::atomic::AtomicUsize,
    }

    impl Access for Fake {
        fn access(&self) -> ReadAccess {
            if self.asks.load(Ordering::SeqCst) {
                ReadAccess::NotAskedYet
            } else {
                ReadAccess::Allowed
            }
        }
    }

    impl Fake {
        /// Another app copied `reps`: it cleared the pasteboard, then put them on.
        fn copy(&self, reps: &[(&str, &[u8])]) {
            self.clear();
            self.put(reps);
        }

        /// `clearContents`: the board is empty and its count moves.
        fn clear(&self) {
            self.items.lock().clear();
            self.count.fetch_add(1, Ordering::SeqCst);
        }

        /// `writeObjects:` after a clear: the contents land under the count the clear left.
        fn put(&self, reps: &[(&str, &[u8])]) {
            let item = reps.iter().map(|(t, b)| ((*t).to_owned(), b.to_vec())).collect();
            *self.items.lock() = vec![item];
        }
    }

    impl Board for Fake {
        fn change_count(&self) -> isize {
            self.count.load(Ordering::SeqCst)
        }

        fn types(&self) -> Vec<String> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            let items = self.items.lock();
            items.first().map(|i| i.iter().map(|(t, _b)| t.clone()).collect()).unwrap_or_default()
        }

        fn data(&self, kind: &str) -> Option<Vec<u8>> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            let items = self.items.lock();
            items.first()?.iter().find(|(t, _b)| t == kind).map(|(_t, b)| b.clone())
        }

        fn file_urls(&self) -> Vec<String> {
            let url = board_type(ClipFormat::FileUrls);
            let urls = |items: &[Item]| -> Vec<String> {
                let found = items.iter().filter_map(|i| i.iter().find(|(t, _b)| *t == url));
                found.map(|(_t, b)| String::from_utf8_lossy(b).into_owned()).collect()
            };
            urls(&self.items.lock())
        }

        fn write(&self, items: &[Item]) -> Option<isize> {
            *self.items.lock() = items.to_vec();
            let count = self.count.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
            let hook = self.during_write.lock().take();
            if let Some(hook) = hook {
                hook();
            }
            Some(count)
        }
    }

    fn next_link() -> Link {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }

    fn clip() -> Clipboard<Fake> {
        Clipboard::new(Fake::default(), Peer::Worker(WorkerId::new()))
    }

    fn text() -> String {
        board_type(ClipFormat::Text)
    }

    #[test]
    fn nothing_is_read_or_announced_while_nobody_watches() {
        let c = clip();
        let a = next_link();
        c.board().copy(&[(&text(), b"one")]);
        assert_eq!(c.poll(), None, "nobody watches");
        let mut watched = c.watched();
        assert!(!*watched.borrow_and_update());
        c.watch(a, true);
        assert!(watched.has_changed().unwrap() && *watched.borrow_and_update());
        assert_eq!(c.poll(), None, "a change made while unwatched is not announced");
        c.board().copy(&[(&text(), b"two")]);
        let offer = c.poll().expect("a change while watched");
        assert_eq!(offer.items[0].inline.as_deref(), Some(&b"two"[..]));
        assert_eq!(c.poll(), None, "announced once");
        c.forget(a);
        assert!(!*watched.borrow_and_update());
        c.board().copy(&[(&text(), b"three")]);
        assert_eq!(c.poll(), None);
    }

    /// While reading the pasteboard would raise macOS's paste alert, a change is neither read
    /// nor announced, and a client's paste is still written; once reads are allowed the
    /// clipboard is announced again.
    #[test]
    fn nothing_is_read_while_reads_would_ask_the_person() {
        let c = clip();
        let a = next_link();
        c.watch(a, true);
        c.board().asks.store(true, Ordering::SeqCst);
        assert_eq!(c.access(), ReadAccess::NotAskedYet);
        c.board().copy(&[(&text(), b"secret")]);
        let before = c.board().reads.load(Ordering::SeqCst);
        assert_eq!(c.poll(), None);
        assert_eq!(c.board().reads.load(Ordering::SeqCst), before, "the contents were not read");
        let offer = Offer {
            origin: Peer::Client(ClientId::nil()),
            generation: 1,
            items: vec![ClipItem {
                format: ClipFormat::Text,
                size: 5,
                hash: digest(b"typed"),
                inline: Some(b"typed".to_vec()),
            }],
        };
        c.offered(a, offer);
        assert_eq!(c.paste(a), Paste::Ready);
        assert_eq!(c.board().data(&text()).unwrap(), b"typed", "a paste still writes");
        c.board().asks.store(false, Ordering::SeqCst);
        c.board().copy(&[(&text(), b"allowed now")]);
        let offer = c.poll().expect("reads are free again");
        assert_eq!(offer.items[0].inline.as_deref(), Some(&b"allowed now"[..]));
    }

    /// Another process's copy clears the pasteboard, which moves its count, and puts the new
    /// contents on after, under that same count. A poll that lands in between finds the board
    /// empty; the contents are still announced once they are there.
    #[test]
    fn contents_put_on_after_a_poll_saw_the_board_cleared_are_announced() {
        let c = clip();
        c.watch(next_link(), true);
        c.board().clear();
        assert_eq!(c.poll(), None, "nothing on the board yet");
        c.board().put(&[(&text(), b"landed")]);
        let offer = c.poll().expect("the contents that landed under the clear's count");
        assert_eq!(offer.items[0].inline.as_deref(), Some(&b"landed"[..]));
        assert_eq!(c.poll(), None, "announced once");
    }

    #[test]
    fn an_offer_inlines_small_text_and_lists_the_rest_for_fetching() {
        let c = clip();
        c.watch(next_link(), true);
        let big = vec![b'x'; INLINE_CLIP_BYTES + 1];
        let png = board_type(ClipFormat::Png);
        c.board().copy(&[(&text(), &big), (&png, b"\x89PNG"), ("com.example.private", b"p")]);
        let offer = c.poll().unwrap();
        let formats: Vec<ClipFormat> = offer.items.iter().map(|i| i.format).collect();
        assert_eq!(formats, [ClipFormat::Png, ClipFormat::Text], "richest first, unknown left out");
        assert!(offer.items.iter().all(|i| i.inline.is_none()), "text over the cap is fetched");
        assert_eq!(offer.items[1].size, big.len() as u64);
        assert_eq!(offer.items[1].hash, digest(&big));
        assert_eq!(c.fetch(offer.generation, ClipFormat::Png).unwrap(), &b"\x89PNG"[..]);
        assert_eq!(c.fetch(offer.generation, ClipFormat::Rtf), None, "never listed");
        c.board().copy(&[(&text(), b"newer")]);
        let newer = c.poll().unwrap();
        assert!(newer.generation > offer.generation);
        assert_eq!(c.fetch(offer.generation, ClipFormat::Png), None, "the old offer is gone");
    }

    #[test]
    fn secrets_and_transient_contents_are_skipped() {
        let c = clip();
        c.watch(next_link(), true);
        c.board().copy(&[(&text(), b"hunter2"), (CONCEALED_TYPE, b"")]);
        assert_eq!(c.poll(), None);
        c.board().copy(&[(&text(), b"soon gone"), (TRANSIENT_TYPE, b"")]);
        assert_eq!(c.poll(), None);
    }

    /// The worker's own write is not announced back: not the change it made, not contents that
    /// name this worker as their origin, not contents it just wrote under another count.
    #[test]
    fn echoes_are_broken_three_ways() {
        let c = clip();
        let a = next_link();
        c.watch(a, true);
        let offer = |generation, body: &[u8]| Offer {
            origin: Peer::Client(ClientId::nil()),
            generation,
            items: vec![ClipItem {
                format: ClipFormat::Text,
                size: body.len() as u64,
                hash: digest(body),
                inline: Some(body.to_vec()),
            }],
        };
        c.offered(a, offer(1, b"from the client"));
        assert_eq!(c.paste(a), Paste::Ready);
        assert_eq!(c.board().data(&text()).unwrap(), b"from the client");
        assert_eq!(c.poll(), None, "the write's own change count");

        // A client on this very Mac puts the worker's announced contents back, stamped with the
        // worker as their origin.
        c.board().copy(&[(&text(), b"worker text")]);
        let announced = c.poll().unwrap();
        let origin = origin_bytes(announced.origin, announced.generation);
        c.board().copy(&[(&text(), b"worker text"), (ORIGIN_TYPE, &origin)]);
        assert_eq!(c.poll(), None, "origin names this worker");

        // Universal Clipboard delivers the same text again with no origin.
        c.board().copy(&[(&text(), b"worker text")]);
        assert_eq!(c.poll(), None, "same digest as announced");
        c.board().copy(&[(&text(), b"something else")]);
        assert!(c.poll().is_some());
    }

    #[test]
    fn a_paste_fetches_what_was_not_inline_then_writes_it_once() {
        let c = clip();
        let a = next_link();
        let png = board_type(ClipFormat::Png);
        let picture = b"\x89PNG picture".to_vec();
        c.offered(
            a,
            Offer {
                origin: Peer::Client(ClientId::nil()),
                generation: 7,
                items: vec![
                    ClipItem {
                        format: ClipFormat::FileUrls,
                        size: 9,
                        hash: digest(b"file:///x"),
                        inline: None,
                    },
                    ClipItem {
                        format: ClipFormat::Png,
                        size: picture.len() as u64,
                        hash: digest(&picture),
                        inline: None,
                    },
                    ClipItem {
                        format: ClipFormat::Text,
                        size: 2,
                        hash: digest(b"hi"),
                        inline: Some(b"hi".to_vec()),
                    },
                ],
            },
        );
        let Paste::Fetch { generation, formats } = c.paste(a) else { panic!("a fetch first") };
        assert_eq!((generation, formats), (7, vec![ClipFormat::Png]), "a file URL is not pasted");
        let png_format = ClipFormat::Png;
        assert!(!c.supply(a, 7, png_format, b"tampered".to_vec()), "a digest mismatch is dropped");
        assert!(!c.supply(a, 6, png_format, picture.clone()), "another offer's data");
        assert!(c.supply(a, 7, png_format, picture.clone()));
        assert!(c.write_incoming(a));
        assert_eq!(c.board().data(&png).unwrap(), picture);
        assert_eq!(c.board().data(&text()).unwrap(), b"hi");
        let (peer, generation) = parse_origin(&c.board().data(ORIGIN_TYPE).unwrap()).unwrap();
        assert_eq!(
            (peer, generation),
            (Peer::Client(ClientId::nil()), 7),
            "stamped with its origin"
        );
        assert_eq!(c.paste(a), Paste::Ready, "written once");
        assert!(!c.write_incoming(a));
        assert_eq!(c.paste(next_link()), Paste::Ready, "a client that offered nothing");
    }

    /// A poll that reads the worker's own write before `write` has returned takes it for that
    /// write, not for a copy to announce back to the clients.
    #[test]
    fn a_poll_inside_the_workers_own_write_announces_nothing() {
        let c = std::sync::Arc::new(clip());
        let (a, watcher) = (next_link(), next_link());
        c.watch(watcher, true);
        c.offered(
            a,
            Offer {
                origin: Peer::Client(ClientId::nil()),
                generation: 3,
                items: vec![ClipItem {
                    format: ClipFormat::Text,
                    size: 4,
                    hash: digest(b"mine"),
                    inline: Some(b"mine".to_vec()),
                }],
            },
        );
        let polled = std::sync::Arc::new(Mutex::new(None));
        let hook = {
            let (weak, polled) = (std::sync::Arc::downgrade(&c), std::sync::Arc::clone(&polled));
            move || *polled.lock() = weak.upgrade().map(|c| c.poll())
        };
        *c.board().during_write.lock() = Some(Box::new(hook));
        assert!(c.write_incoming(a));
        assert_eq!(*polled.lock(), Some(None), "polled mid-write, and nothing announced");
        assert_eq!(c.poll(), None, "nor after");
    }

    /// A client that copied files offers only their URLs. That offer takes the place of its
    /// earlier one, whose text a paste would otherwise write over the staged files, and it
    /// writes nothing itself: the files come as a transfer.
    #[test]
    fn a_clients_copied_files_replace_its_offer_and_write_nothing() {
        let c = clip();
        let a = next_link();
        let offer = |generation, format, body: &[u8], inline: bool| Offer {
            origin: Peer::Client(ClientId::nil()),
            generation,
            items: vec![ClipItem {
                format,
                size: body.len() as u64,
                hash: digest(body),
                inline: inline.then(|| body.to_vec()),
            }],
        };
        c.offered(a, offer(1, ClipFormat::Text, b"older text", true));
        assert!(c.write_files(&["/tmp/staged.txt"]));
        c.offered(a, offer(2, ClipFormat::FileUrls, b"file:///Users/me/a.txt", false));
        assert_eq!(c.paste(a), Paste::Ready, "nothing to fetch");
        assert_eq!(c.board().file_urls(), ["file:///tmp/staged.txt"], "the staged files stay");
        assert_eq!(c.board().data(&text()), None);
    }

    #[test]
    fn staged_files_go_on_as_file_urls() {
        let c = clip();
        c.watch(next_link(), true);
        assert!(c.write_files(&["/tmp/a b.txt", "/tmp/ü"]));
        assert_eq!(c.board().file_urls(), ["file:///tmp/a%20b.txt", "file:///tmp/%C3%BC"]);
        assert_eq!(c.poll(), None, "the worker's own write");
    }
}
