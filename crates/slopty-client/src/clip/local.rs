//! This client's pasteboard in step with the workers: its clipboard announced to the worker a
//! remote tile belongs to, one worker's clipboard relayed to another, and a worker's
//! announcements put on this client's pasteboard as promises.
//!
//! Announce, never push: an [`Offer`] lists every item and every type the clipboard holds, with
//! small text and file URLs inline, and the rest is read off the pasteboard when a worker fetches
//! it ([`ClipSync::answer`]). A secret (concealed or transient) is announced by type alone and
//! read only for a paste.
//!
//! Reading the pasteboard can ask the person first ([`Pasteboard::reads_ask`]: iOS always, macOS
//! until they allow this app), so when a tile takes the keyboard the clipboard is read only when
//! reads are free ([`ClipSync::focus_offer`]); otherwise it waits for their paste
//! ([`ClipSync::paste_offer`]).
//!
//! A worker's offer written here is relayed as it is to the other workers
//! ([`ClipSync::offer_for`]), origin kept, and their fetches are answered by fetching from the
//! worker it came from ([`Answer::From`]). So text or a picture copied on one worker pastes into
//! another worker's window, as one machine's clipboard would.
//!
//! Echoes are broken three ways: every write here carries [`ORIGIN_TYPE`] and is never announced
//! as this client's, the change count a write produced is remembered, and contents whose inline
//! digests match what a worker just announced are not announced to it again.
//!
//! Copied files are the exception to announcing: their URLs name files on one machine, so a
//! paste of them into a worker moves the files by a transfer ([`ClipSync::files`]). Where this
//! device shows a worker's home as a place of its own (a File Provider domain in Finder,
//! [`Place`]), a worker's copied files go on this pasteboard as their URLs there, so a paste
//! into Finder or any app here takes them, each fetched from the worker as it is read.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slopty_core::ClientId;
use slopty_platform::pasteboard::{
    CONCEALED_UTI, Capped, FILE_URL_UTI, ORIGIN_TYPE, Pasteboard, Provide, TRANSIENT_UTI, Write,
    WriteItem, carried, clip_type, format_of, uti_of_type, writable,
};
use slopty_proto::transfer::{
    ClipEntry, ClipFormat, ClipType, Hash, INLINE_CLIP_BYTES, MAX_CLIP_ITEMS, Offer, Peer, Rep,
    RepRef, Source, origin_bytes,
};
use tokio::sync::watch;

use super::{Fetched, MAX_CLIP_BYTES, digest};
use crate::layout::WorkerKey;
use crate::remote::Remote;

/// Files on the clipboard, for a paste that moves them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ClipFiles {
    /// Files on this device, copied here.
    Here(Vec<PathBuf>),
    /// Files a worker copied: each one's `public.file-url` representation in its offer.
    Worker {
        /// The worker they are on.
        worker: WorkerKey,
        /// Each file's URL, as a representation to fetch.
        urls: Vec<RepRef>,
    },
}

/// What a paste into a shell finds on the clipboard.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ShellPaste {
    /// Text, which the view reads itself.
    Text,
    /// Files, pasted as a drop.
    Files(ClipFiles),
    /// A picture and no text: it goes to the worker's pasteboard ahead of the chord, with this
    /// offer first when the worker has not heard it.
    Picture {
        /// The offer, for the control stream ahead of the chord.
        offer: Option<Offer>,
    },
}

/// Where the answer to a worker's fetch comes from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Answer {
    /// This client's pasteboard, read now.
    Here(Fetched),
    /// The worker whose offer this client relayed: fetch it from there.
    From(WorkerKey),
}

/// Where a worker's home shows on this device: the root of its place in the file manager (its
/// File Provider domain in Finder), whose files are the worker's, fetched as they are read.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Place {
    /// The worker's home, as its hello said.
    pub home: String,
    /// Where its place is rooted on this device.
    pub root: PathBuf,
}

impl Place {
    /// The URLs here of a worker's `public.file-url` representation (its own `file://` URLs,
    /// one a line): each file's URL in this place. `None` when any of them is outside the
    /// worker's home, which the place does not hold, or is no file URL at all.
    #[must_use]
    pub fn urls_of(&self, bytes: &[u8]) -> Option<Vec<u8>> {
        let text = std::str::from_utf8(bytes).ok()?;
        let mut urls = Vec::new();
        for url in text.lines().filter(|line| !line.is_empty()) {
            let path = file_url_path(url)?;
            let under = slopty_proto::folder::under_home(&self.home, path.to_str()?)?;
            let mut here = file_url(&self.root.join(under));
            if url.ends_with('/') && !here.ends_with('/') {
                here.push('/');
            }
            urls.push(here);
        }
        (!urls.is_empty()).then(|| urls.join("\n").into_bytes())
    }
}

/// A worker's link as it is now: whichever connection is up, `None` while none is.
pub type LinkNow = watch::Receiver<Option<Arc<dyn Remote>>>;

type Key = (u16, ClipType);

/// The clipboard's contents as last read, and what was announced of them.
#[derive(Debug)]
struct Held {
    /// The change count they were read at.
    count: i64,
    /// The offer made of them; `None` for contents never to be announced (Slopty's own write, a
    /// copy that names nothing that travels).
    offer: Option<Offer>,
    /// When they were copied.
    copied: Instant,
    /// What was read of them.
    read: HashMap<Key, Vec<u8>>,
}

/// The worker offer this client wrote last.
#[derive(Debug)]
struct Wrote {
    /// The change count the write left.
    count: i64,
    from: WorkerKey,
    offer: Offer,
    /// When it arrived.
    at: Instant,
}

/// The pasteboard and the bookkeeping that keeps it in step with the workers.
pub struct ClipSync {
    board: Rc<dyn Pasteboard>,
    /// This client's offers so far.
    generation: u64,
    held: Option<Held>,
    wrote: Option<Wrote>,
    /// The offer each worker was last told.
    told: HashMap<WorkerKey, Source>,
    /// Digests a worker announced last: the same contents coming back are not announced again.
    heard: HashSet<Hash>,
    /// The change count last seen, and when: a copy is placed at the time its count was first
    /// seen.
    seen: Option<(i64, Instant)>,
    /// Each worker's link now, which its promises here fetch over.
    links: HashMap<WorkerKey, watch::Sender<Option<Arc<dyn Remote>>>>,
    /// When [`ClipSync::tick`] last looked.
    ticked: Option<Instant>,
    /// The change count a focus left unread because reading would ask the person.
    deferred: Option<i64>,
}

/// How often [`ClipSync::tick`] reads the change count.
const TICK: Duration = Duration::from_millis(100);

impl std::fmt::Debug for ClipSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipSync")
            .field("generation", &self.generation)
            .field("held", &self.held.as_ref().map(|h| h.count))
            .field("wrote", &self.wrote.as_ref().map(|w| w.count))
            .finish_non_exhaustive()
    }
}

/// Where this client's offer generations start. The app keeps its client id across launches,
/// so a relaunch must not reuse a generation a worker still holds from the last run: the wall
/// clock in microseconds, with a count for two made within one.
fn first_generation() -> u64 {
    static MADE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX));
    let made = MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    micros.wrapping_shl(12).wrapping_add(made & 0xfff)
}

/// Milliseconds from `then` to `now`.
fn age(then: Instant, now: Instant) -> u64 {
    u64::try_from(now.saturating_duration_since(then).as_millis()).unwrap_or(u64::MAX)
}

impl ClipSync {
    /// Keep `board` in step.
    #[must_use]
    pub fn new(board: Rc<dyn Pasteboard>) -> Self {
        Self {
            board,
            generation: first_generation(),
            held: None,
            wrote: None,
            told: HashMap::new(),
            heard: HashSet::new(),
            seen: None,
            links: HashMap::new(),
            ticked: None,
            deferred: None,
        }
    }

    /// The pasteboard.
    #[must_use]
    pub fn board(&self) -> &Rc<dyn Pasteboard> {
        &self.board
    }

    /// Note the change count as it is `now`, which never asks the person: a copy made since is
    /// placed at the first time its count was seen. Called often, so a copy's age is known when
    /// it is announced.
    pub fn observe(&mut self, now: Instant) {
        let count = self.board.change_count();
        if self.seen.is_none_or(|(seen, _)| seen != count) {
            self.seen = Some((count, now));
        }
    }

    /// Look at the change count, at most every 100 ms, which never asks the person: a copy's
    /// time is noted ([`ClipSync::observe`]). Whether the clipboard changed since it was last
    /// read, written or left unread, so that a worker whose tile has the keyboard hears of it.
    pub fn tick(&mut self, now: Instant) -> bool {
        if self.ticked.is_some_and(|at| now.saturating_duration_since(at) < TICK) {
            return false;
        }
        self.ticked = Some(now);
        self.observe(now);
        let Some((count, _)) = self.seen else { return false };
        self.held.as_ref().is_none_or(|h| h.count != count)
            && self.wrote.as_ref().is_none_or(|w| w.count != count)
            && self.deferred != Some(count)
    }

    /// When the contents at `count` were copied: the first time their count was seen, else now.
    fn copied(&self, count: i64, now: Instant) -> Instant {
        self.seen.filter(|(seen, _)| *seen == count).map_or(now, |(_, at)| at)
    }

    /// The worker offer this client wrote, while the pasteboard still holds it.
    fn relaying(&self) -> Option<&Wrote> {
        let count = self.board.change_count();
        self.wrote.as_ref().filter(|w| w.count == count)
    }

    /// Read the pasteboard again if it changed since it was last read.
    fn refresh(&mut self, me: ClientId, now: Instant) {
        let count = self.board.change_count();
        if self.held.as_ref().is_some_and(|h| h.count == count) {
            return;
        }
        let board = Rc::clone(&self.board);
        self.hold(board.as_ref(), count, me, now);
    }

    /// A paste the person made where reading the clipboard asks them first (the system's paste
    /// button on iOS): `pasted` holds what the clipboard holds now, so it stands for a read of
    /// it, and the paste that follows reads nothing more.
    pub fn pasted(&mut self, pasted: &dyn Pasteboard, me: ClientId) {
        let count = self.board.change_count();
        self.hold(pasted, count, me, Instant::now());
    }

    /// Take `board`'s contents as the clipboard's at change `count`: every item's types, and
    /// the small text and file URLs among them, inline while they fit.
    fn hold(&mut self, board: &dyn Pasteboard, count: i64, me: ClientId, now: Instant) {
        let copied = self.copied(count, now);
        if self.wrote.as_ref().is_some_and(|w| w.count == count) {
            self.held = Some(Held { count, offer: None, copied, read: HashMap::new() });
            return;
        }
        let items = board.items();
        // A copy clears the pasteboard, which moves the count, and puts its contents on after
        // under that same count: an empty board may be a copy half done, read again next time.
        if items.is_empty() {
            self.held = None;
            return;
        }
        let has = |uti: &str| items.iter().flatten().any(|t| t == uti);
        if has(ORIGIN_TYPE) {
            self.held = Some(Held { count, offer: None, copied, read: HashMap::new() });
            return;
        }
        let concealed = has(CONCEALED_UTI) || has(TRANSIENT_UTI);
        let mut budget = if concealed { 0 } else { INLINE_CLIP_BYTES };
        let mut read = HashMap::new();
        let mut entries = Vec::new();
        for (types, n) in items.iter().take(MAX_CLIP_ITEMS).zip(0_u16..) {
            let mut reps = Vec::new();
            for uti in types.iter().filter(|t| format_of(t).is_some() || carried(t)) {
                let kind = clip_type(uti);
                let small = kind.is(ClipFormat::Text) || kind.is(ClipFormat::FileUrls);
                let cap = u64::try_from(budget).unwrap_or(u64::MAX);
                let bytes = (small && budget > 0)
                    .then(|| board.item_data_within(usize::from(n), uti, cap))
                    .flatten();
                let rep = match bytes {
                    Some(Capped::Data(bytes)) => {
                        budget = budget.saturating_sub(bytes.len());
                        let rep = Rep {
                            kind: kind.clone(),
                            size: Some(u64::try_from(bytes.len()).unwrap_or(u64::MAX)),
                            hash: Some(digest(&bytes)),
                            inline: Some(bytes.clone()),
                        };
                        read.insert((n, kind), bytes);
                        rep
                    }
                    // Past what is left inline: its size is known, its bytes are read again
                    // when a worker fetches them, and nothing more is read now.
                    Some(Capped::TooBig(size)) => {
                        budget = 0;
                        Rep { kind, size: Some(size), hash: None, inline: None }
                    }
                    None => Rep { kind, size: None, hash: None, inline: None },
                };
                reps.push(rep);
            }
            entries.push(ClipEntry { reps });
        }
        let keys: HashSet<Hash> =
            entries.iter().flat_map(|e| &e.reps).filter_map(|r| r.hash).collect();
        let echo = !keys.is_empty() && keys.is_subset(&self.heard);
        let travels = entries.iter().any(|e| !e.reps.is_empty());
        let offer = (travels && !echo).then(|| {
            self.generation = self.generation.saturating_add(1);
            Offer {
                origin: Peer::Client(me),
                generation: self.generation,
                age_ms: 0,
                concealed,
                items: entries,
            }
        });
        tracing::debug!(count, echo, offered = offer.is_some(), "clipboard read");
        self.held = Some(Held { count, offer, copied, read });
    }

    /// What `worker` should hear of the clipboard now, reading it if it must: this client's
    /// offer, or another worker's it relays, when the worker was not told it yet. Its `age_ms`
    /// is the time since the copy.
    pub fn offer_for(&mut self, worker: WorkerKey, me: ClientId) -> Option<Offer> {
        let now = Instant::now();
        let offer = if let Some(wrote) = self.relaying() {
            if wrote.from == worker {
                return None;
            }
            Offer { age_ms: age(wrote.at, now), ..wrote.offer.clone() }
        } else {
            self.refresh(me, now);
            let held = self.held.as_ref()?;
            Offer { age_ms: age(held.copied, now), ..held.offer.clone()? }
        };
        if self.told.get(&worker) == Some(&offer.source()) {
            return None;
        }
        self.told.insert(worker, offer.source());
        Some(offer)
    }

    /// What `worker` should hear when a tile of it takes the keyboard. Where reading the
    /// clipboard would ask the person first, it waits for their paste, unless the clipboard
    /// holds what this client wrote, which it knows without reading.
    pub fn focus_offer(&mut self, worker: WorkerKey, me: ClientId) -> Option<Offer> {
        if self.relaying().is_none() && self.board.reads_ask() {
            self.deferred = Some(self.board.change_count());
            return None;
        }
        self.offer_for(worker, me)
    }

    /// What `worker` should hear ahead of a paste the person made into one of its tiles: the
    /// paste is their intent to read the clipboard.
    pub fn paste_offer(&mut self, worker: WorkerKey, me: ClientId) -> Option<Offer> {
        self.offer_for(worker, me)
    }

    /// What a paste into a shell of `worker` finds on the clipboard. Files first: they go as a
    /// drop. Then a picture with no text: a program on the worker reads it off the worker's
    /// pasteboard (Claude Code), so it goes there ahead of the chord, with the offer when the
    /// worker has not heard it. A picture the worker itself offered is there already. Anything
    /// else is text, which the view reads itself.
    pub fn shell_paste(&mut self, worker: WorkerKey, me: ClientId) -> ShellPaste {
        if let Some(files) = self.files() {
            return ShellPaste::Files(files);
        }
        let reps: Vec<ClipType> = match self.relaying() {
            Some(wrote) if wrote.from == worker => return ShellPaste::Text,
            Some(wrote) => wrote.offer.reps().map(|(_, r)| r.kind.clone()).collect(),
            None => {
                self.refresh(me, Instant::now());
                let Some(offer) = self.held.as_ref().and_then(|h| h.offer.as_ref()) else {
                    return ShellPaste::Text;
                };
                offer.reps().map(|(_, r)| r.kind.clone()).collect()
            }
        };
        let picture = reps.iter().any(ClipType::is_picture);
        if !picture || reps.iter().any(|k| k.is(ClipFormat::Text)) {
            return ShellPaste::Text;
        }
        ShellPaste::Picture { offer: self.paste_offer(worker, me) }
    }

    /// The files the clipboard holds, for a paste that moves them: files copied here, or the
    /// files of the worker offer this client wrote last, while its write is still there.
    /// Reads the clipboard, so only for a paste.
    pub fn files(&self) -> Option<ClipFiles> {
        if let Some(wrote) = self.relaying() {
            let urls: Vec<RepRef> = wrote
                .offer
                .reps()
                .filter(|(_, r)| r.kind.is(ClipFormat::FileUrls))
                .map(|(n, r)| wrote.offer.rep_ref(n, r.kind.clone()))
                .collect();
            return (!urls.is_empty()).then_some(ClipFiles::Worker { worker: wrote.from, urls });
        }
        if self.board.types().iter().any(|t| t == ORIGIN_TYPE) {
            return None;
        }
        let paths: Vec<PathBuf> =
            self.board.file_urls().iter().filter_map(|url| file_url_path(url)).collect();
        (!paths.is_empty()).then_some(ClipFiles::Here(paths))
    }

    /// Where the answer to a worker's fetch of `rep`, capped at `max` and at
    /// [`MAX_CLIP_BYTES`], comes from: this client's pasteboard, read now unless it was read
    /// already, while it still holds the offer; or the worker whose offer this client relayed.
    /// Past the cap it is `TooBig`, and none of it is kept.
    #[must_use]
    pub fn answer(&self, rep: &RepRef, max: Option<u64>) -> Answer {
        if let Some(wrote) = self.relaying().filter(|w| w.offer.source() == rep.source) {
            return Answer::From(wrote.from);
        }
        let count = self.board.change_count();
        let Some(held) = self.held.as_ref().filter(|h| h.count == count) else {
            return Answer::Here(Fetched::Gone);
        };
        if held.offer.as_ref().is_none_or(|o| o.rep(rep).is_none()) {
            return Answer::Here(Fetched::Gone);
        }
        let cap = max.map_or(MAX_CLIP_BYTES, |m| m.min(MAX_CLIP_BYTES));
        let key = (rep.item, rep.kind.clone());
        let read = match held.read.get(&key) {
            Some(bytes) => Some(Capped::Data(bytes.clone())),
            None => self.board.item_data_within(usize::from(rep.item), uti_of_type(&rep.kind), cap),
        };
        Answer::Here(match read {
            None => Fetched::Gone,
            Some(Capped::TooBig(size)) => Fetched::TooBig(size),
            Some(Capped::Data(bytes)) => {
                let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                if size > cap { Fetched::TooBig(size) } else { Fetched::Data(bytes) }
            }
        })
    }

    /// Worker `from`'s clipboard changed: put its offer here, every item, inline bytes now and
    /// the rest as promises that `provide` keeps. A secret goes on marked concealed and transient,
    /// so no clipboard manager here keeps it. Contents this clipboard already holds are left
    /// alone. Nothing that names a file goes on as it is ([`writable`]): a worker's path names
    /// whatever sits there on this machine. A worker's file in its home goes on as its URL in
    /// the worker's `place` here, when this device shows one, which a paste in Finder takes as
    /// a file fetched as it is read ([`Place::urls_of`]). Its files also come by a paste into a
    /// worker, which moves them ([`ClipSync::files`]).
    pub fn receive(
        &mut self,
        from: WorkerKey,
        offer: &Offer,
        provide: Provide,
        place: Option<&Place>,
    ) {
        let inline: HashSet<(u16, &ClipType, Hash)> = offer
            .reps()
            .filter_map(|(n, r)| r.inline.as_ref().map(|b| (n, &r.kind, digest(b))))
            .collect();
        let all_inline = offer.reps().all(|(_, r)| r.inline.is_some());
        let count = self.board.change_count();
        let same = all_inline
            && !inline.is_empty()
            && self.held.as_ref().filter(|h| h.count == count).is_some_and(|h| {
                let ours: HashSet<(u16, &ClipType, Hash)> =
                    h.read.iter().map(|((n, k), b)| (*n, k, digest(b))).collect();
                inline.is_subset(&ours)
            });
        self.heard = inline.iter().map(|(_, _, h)| *h).collect();
        if same {
            tracing::debug!(generation = offer.generation, "clipboard already holds it");
            return;
        }
        let mut items = Vec::new();
        for entry in &offer.items {
            let mut item = WriteItem::default();
            for rep in &entry.reps {
                let uti = uti_of_type(&rep.kind);
                if uti == FILE_URL_UTI {
                    let here = place.zip(rep.inline.as_deref()).and_then(|(p, b)| p.urls_of(b));
                    if let Some(here) = here {
                        item.data.push((FILE_URL_UTI.to_owned(), here));
                    }
                    continue;
                }
                if !writable(uti, rep.inline.as_deref()) {
                    continue;
                }
                match &rep.inline {
                    Some(bytes) => item.data.push((uti.to_owned(), bytes.clone())),
                    None => item.promised.push(uti.to_owned()),
                }
            }
            if !item.data.is_empty() || !item.promised.is_empty() {
                items.push(item);
            }
        }
        // The origin rides on the first item, so there is one even when nothing else can go.
        if items.is_empty() {
            items.push(WriteItem::default());
        }
        if offer.concealed
            && let Some(first) = items.first_mut()
        {
            first.data.push((CONCEALED_UTI.to_owned(), Vec::new()));
            first.data.push((TRANSIENT_UTI.to_owned(), Vec::new()));
        }
        let promised = items.iter().any(|i| !i.promised.is_empty());
        let write = Write {
            origin: origin_bytes(offer.origin, offer.generation),
            items,
            provide: promised.then_some(provide),
        };
        let count = self.board.write(write);
        let now = Instant::now();
        self.held = Some(Held { count, offer: None, copied: now, read: HashMap::new() });
        self.wrote = Some(Wrote { count, from, offer: offer.clone(), at: now });
        tracing::debug!(generation = offer.generation, count, "clipboard from a worker");
    }

    /// A worker's link came up (`remote`) or went (`None`): the worker has heard nothing over
    /// it yet, and what it offered before is fetched over it from now on.
    pub fn relink(&mut self, worker: WorkerKey, remote: Option<Arc<dyn Remote>>) {
        self.told.remove(&worker);
        self.links.entry(worker).or_insert_with(|| watch::Sender::new(None)).send_replace(remote);
    }

    /// `worker`'s link, followed as it drops and comes back.
    pub fn link(&mut self, worker: WorkerKey) -> LinkNow {
        self.links.entry(worker).or_insert_with(|| watch::Sender::new(None)).subscribe()
    }
}

/// What keeps the promises of worker offer `offer`: a fetch over the worker's link at the time
/// of the paste, waiting at most `wait`.
///
/// A paste while the link is down gets nothing at once. Bytes other than the ones the offer
/// listed are not pasted: a restarted worker counts its offers from 1 again, so a generation
/// alone does not name the contents.
#[must_use]
pub fn provider(link: LinkNow, offer: &Offer, wait: Duration) -> Provide {
    let offer = offer.clone();
    Arc::new(move |item: usize, uti: &str| {
        let rep = offer.rep_ref(u16::try_from(item).ok()?, clip_type(uti));
        let listed = offer.rep(&rep)?.hash;
        let remote = link.borrow().clone()?;
        let Fetched::Data(bytes) = remote.clip_fetch(&rep, None, wait) else { return None };
        if listed.is_some_and(|h| h != digest(&bytes)) {
            tracing::debug!(item, uti, "clipboard bytes not the ones offered; dropped");
            return None;
        }
        // A link promised without its bytes may turn out to name a file.
        writable(uti, Some(&bytes)).then_some(bytes)
    })
}

/// Answer worker `to`'s fetch of `rep` (capped at `max`, `urgent` when a paste waits on it)
/// with what worker `from`, whose offer it is, sends, waiting at most `wait`. Blocks: run it off
/// the main thread.
pub fn relay(
    from: &dyn Remote,
    to: &dyn Remote,
    rep: RepRef,
    max: Option<u64>,
    urgent: bool,
    wait: Duration,
) {
    let got = from.clip_fetch(&rep, max, wait);
    to.send_clip(rep, got, urgent);
}

/// The path a `file://` URL names, percent escapes undone; `None` for any other URL.
#[must_use]
pub fn file_url_path(url: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt as _;
    let rest = url.strip_prefix("file://")?;
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    if !path.starts_with('/') {
        return None;
    }
    let mut bytes = Vec::with_capacity(path.len());
    let mut it = path.bytes();
    while let Some(b) = it.next() {
        if b == b'%' {
            let hex = [it.next()?, it.next()?];
            let hex = std::str::from_utf8(&hex).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
        } else {
            bytes.push(b);
        }
    }
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// `path` as a `file://` URL: every byte but a letter, a digit and `/-._~` escaped.
#[must_use]
pub fn file_url(path: &std::path::Path) -> String {
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

/// The paths a `public.file-url` representation names: its URLs, one per line.
#[must_use]
pub fn file_url_paths(bytes: &[u8]) -> Vec<PathBuf> {
    String::from_utf8_lossy(bytes).lines().filter_map(file_url_path).collect()
}

#[cfg(test)]
mod tests;
