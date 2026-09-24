//! Clipboard sync, the worker's half: announce a change, never push it.
//!
//! - **Watch only while wanted.** The pasteboard is read only while some client said it wants the
//!   worker's changes ([`Clipboard::watch`]); [`Clipboard::watched`] wakes the poller. A change
//!   made while nobody watched is not announced when someone starts to: it was made on the worker,
//!   not by anyone at a client.
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
use slopty_input::pasteboard::{Board, CONCEALED_TYPE, Item, ORIGIN_TYPE, Rep, TRANSIENT_TYPE};
use slopty_proto::transfer::{
    ClipItem, Hash, INLINE_CLIP_BYTES, Offer, Peer, origin_bytes, parse_origin,
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

/// `path` as a `file://` URL, the bytes of a `public.file-url` item.
#[must_use]
pub fn file_url(path: &Path) -> String {
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
        utis: Vec<String>,
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
    offered: Option<(u64, Vec<(String, Bytes)>)>,
    /// Digests of what was last announced or written: contents among them are an echo.
    last: HashSet<Hash>,
    /// Each link's last offer.
    incoming: HashMap<Link, Incoming>,
}

#[derive(Debug)]
struct Incoming {
    offer: Offer,
    fetched: HashMap<String, Vec<u8>>,
    written: bool,
}

impl Incoming {
    /// Representations the paste still waits for.
    fn missing(&self) -> Vec<String> {
        self.offer
            .items
            .iter()
            .filter(|i| writable(&i.uti) && i.inline.is_none() && i.size <= MAX_REP_BYTES)
            .filter(|i| !self.fetched.contains_key(&i.uti))
            .map(|i| i.uti.clone())
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

    /// Whether `count` is a change to look at, marking it seen.
    fn advance(&mut self, count: isize) -> bool {
        !self.watchers.is_empty() && std::mem::replace(&mut self.seen, count) != count
    }

    /// Announce `reps` as the next offer from `me`, unless they are what was last announced or
    /// written.
    fn announce(&mut self, me: Peer, reps: Vec<(String, Bytes)>) -> Option<Offer> {
        let (_uti, key) = reps.first()?;
        if self.last.contains(&digest(key)) {
            return None;
        }
        self.generation = self.generation.wrapping_add(1);
        let text = Rep::Text.uti();
        let items = reps
            .iter()
            .map(|(uti, bytes)| ClipItem {
                uti: uti.clone(),
                size: bytes.len() as u64,
                hash: digest(bytes),
                inline: (*uti == text && bytes.len() <= INLINE_CLIP_BYTES).then(|| bytes.to_vec()),
            })
            .collect::<Vec<_>>();
        self.last = items.iter().map(|i| i.hash).collect();
        self.offered = Some((self.generation, reps));
        Some(Offer { origin: me, generation: self.generation, items })
    }

    fn fetch(&self, generation: u64, uti: &str) -> Option<Bytes> {
        let (current, reps) = self.offered.as_ref()?;
        if *current != generation {
            return None;
        }
        reps.iter().find(|(t, _b)| t == uti).map(|(_t, b)| b.clone())
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
        let utis = inc.missing();
        (!utis.is_empty()).then_some(Paste::Fetch { generation: inc.offer.generation, utis })
    }

    fn supply(&mut self, client: Link, generation: u64, uti: &str, bytes: Vec<u8>) -> bool {
        let Some(inc) = self.incoming.get_mut(&client) else { return false };
        if inc.offer.generation != generation {
            return false;
        }
        let listed = inc.offer.items.iter().find(|i| i.uti == uti);
        if listed.is_none_or(|i| i.hash != digest(&bytes)) {
            tracing::debug!(%client, uti, "clipboard data that does not match its offer; dropped");
        } else {
            inc.fetched.insert(uti.to_owned(), bytes);
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
            if !writable(&item.uti) {
                continue;
            }
            if let Some(bytes) = item.inline.clone().or_else(|| inc.fetched.remove(&item.uti)) {
                hashes.insert(item.hash);
                reps.push((item.uti.clone(), bytes));
            }
        }
        if reps.is_empty() {
            return None;
        }
        reps.push((ORIGIN_TYPE.to_owned(), origin_bytes(inc.offer.origin, inc.offer.generation)));
        Some((reps, hashes))
    }
}

impl<B: Board> Clipboard<B> {
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

    /// The pasteboard's change since the last poll or write, as an offer to announce; `None`
    /// when nobody watches, nothing changed, or the change is not to be announced.
    pub fn poll(&self) -> Option<Offer> {
        if self.state.lock().watchers.is_empty() {
            return None;
        }
        let count = self.board.change_count();
        if !self.state.lock().advance(count) {
            return None;
        }
        let types = self.board.types();
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
    fn read(&self, types: &[String]) -> Vec<(String, Bytes)> {
        let mut reps = Vec::new();
        for rep in Rep::ALL {
            let uti = rep.uti();
            let bytes = if rep == Rep::FileUrl {
                let urls = self.board.file_urls();
                if urls.is_empty() {
                    continue;
                }
                urls.join("\n").into_bytes()
            } else if types.contains(&uti) {
                let Some(bytes) = self.board.data(&uti) else { continue };
                bytes
            } else {
                continue;
            };
            if bytes.len() as u64 <= MAX_REP_BYTES {
                reps.push((uti, Bytes::from(bytes)));
            }
        }
        reps
    }

    /// Representation `uti` of the worker's offer `generation`; `None` when that offer is not the
    /// current one (the pasteboard changed again) or never listed it.
    #[must_use]
    pub fn fetch(&self, generation: u64, uti: &str) -> Option<Bytes> {
        self.state.lock().fetch(generation, uti)
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

    /// Bytes of representation `uti` of `client`'s offer `generation` arrived. `true` once
    /// every representation the paste waits for is here.
    pub fn supply(&self, client: Link, generation: u64, uti: &str, bytes: Vec<u8>) -> bool {
        self.state.lock().supply(client, generation, uti, bytes)
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
        let url = Rep::FileUrl.uti();
        let generation = self.state.lock().generation;
        let mut items: Vec<Item> =
            paths.iter().map(|p| vec![(url.clone(), file_url(p.as_ref()).into_bytes())]).collect();
        let Some(first) = items.first_mut() else { return false };
        first.push((ORIGIN_TYPE.to_owned(), origin_bytes(self.me, generation)));
        self.commit(&items, HashSet::new())
    }

    fn commit(&self, items: &[Item], hashes: HashSet<Hash>) -> bool {
        let Some(count) = self.board.write(items) else {
            tracing::warn!("pasteboard write failed");
            return false;
        };
        let mut st = self.state.lock();
        st.seen = count;
        st.last = hashes;
        true
    }
}

/// Whether a representation a client offers can go on the worker's pasteboard as it is: a
/// client's file URLs name files on the client, which travel as a transfer instead.
fn writable(uti: &str) -> bool {
    Rep::of(uti).is_some_and(|rep| rep != Rep::FileUrl)
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
    }

    impl Fake {
        /// Another app copied `reps`.
        fn copy(&self, reps: &[(&str, &[u8])]) {
            let item = reps.iter().map(|(t, b)| ((*t).to_owned(), b.to_vec())).collect();
            *self.items.lock() = vec![item];
            self.count.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl Board for Fake {
        fn change_count(&self) -> isize {
            self.count.load(Ordering::SeqCst)
        }

        fn types(&self) -> Vec<String> {
            let items = self.items.lock();
            items.first().map(|i| i.iter().map(|(t, _b)| t.clone()).collect()).unwrap_or_default()
        }

        fn data(&self, uti: &str) -> Option<Vec<u8>> {
            let items = self.items.lock();
            items.first()?.iter().find(|(t, _b)| t == uti).map(|(_t, b)| b.clone())
        }

        fn file_urls(&self) -> Vec<String> {
            let url = Rep::FileUrl.uti();
            let urls = |items: &[Item]| -> Vec<String> {
                let found = items.iter().filter_map(|i| i.iter().find(|(t, _b)| *t == url));
                found.map(|(_t, b)| String::from_utf8_lossy(b).into_owned()).collect()
            };
            urls(&self.items.lock())
        }

        fn write(&self, items: &[Item]) -> Option<isize> {
            *self.items.lock() = items.to_vec();
            Some(self.count.fetch_add(1, Ordering::SeqCst).wrapping_add(1))
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
        Rep::Text.uti()
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

    #[test]
    fn an_offer_inlines_small_text_and_lists_the_rest_for_fetching() {
        let c = clip();
        c.watch(next_link(), true);
        let big = vec![b'x'; INLINE_CLIP_BYTES + 1];
        let png = Rep::Png.uti();
        c.board().copy(&[(&text(), &big), (&png, b"\x89PNG"), ("com.example.private", b"p")]);
        let offer = c.poll().unwrap();
        let utis: Vec<&str> = offer.items.iter().map(|i| i.uti.as_str()).collect();
        assert_eq!(utis, [png.as_str(), text().as_str()], "richest first, unknown types left out");
        assert!(offer.items.iter().all(|i| i.inline.is_none()), "text over the cap is fetched");
        assert_eq!(offer.items[1].size, big.len() as u64);
        assert_eq!(offer.items[1].hash, digest(&big));
        assert_eq!(c.fetch(offer.generation, &png).unwrap(), &b"\x89PNG"[..]);
        assert_eq!(c.fetch(offer.generation, "public.rtf"), None, "never listed");
        c.board().copy(&[(&text(), b"newer")]);
        let newer = c.poll().unwrap();
        assert!(newer.generation > offer.generation);
        assert_eq!(c.fetch(offer.generation, &png), None, "the old offer is gone");
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
                uti: text(),
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
        let png = Rep::Png.uti();
        let picture = b"\x89PNG picture".to_vec();
        let url = Rep::FileUrl.uti();
        c.offered(
            a,
            Offer {
                origin: Peer::Client(ClientId::nil()),
                generation: 7,
                items: vec![
                    ClipItem { uti: url, size: 9, hash: digest(b"file:///x"), inline: None },
                    ClipItem {
                        uti: png.clone(),
                        size: picture.len() as u64,
                        hash: digest(&picture),
                        inline: None,
                    },
                    ClipItem {
                        uti: text(),
                        size: 2,
                        hash: digest(b"hi"),
                        inline: Some(b"hi".to_vec()),
                    },
                ],
            },
        );
        let Paste::Fetch { generation, utis } = c.paste(a) else { panic!("a fetch first") };
        assert_eq!((generation, utis), (7, vec![png.clone()]), "a client's file URL is not pasted");
        assert!(!c.supply(a, 7, &png, b"tampered".to_vec()), "a digest mismatch is dropped");
        assert!(!c.supply(a, 6, &png, picture.clone()), "another offer's data");
        assert!(c.supply(a, 7, &png, picture.clone()));
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

    #[test]
    fn staged_files_go_on_as_file_urls() {
        let c = clip();
        c.watch(next_link(), true);
        assert!(c.write_files(&["/tmp/a b.txt", "/tmp/ü"]));
        assert_eq!(c.board().file_urls(), ["file:///tmp/a%20b.txt", "file:///tmp/%C3%BC"]);
        assert_eq!(c.poll(), None, "the worker's own write");
    }
}
