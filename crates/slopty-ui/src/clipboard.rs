//! The clipboard shared with the workers: this client's pasteboard announced to the worker a
//! remote tile belongs to, and a worker's announcements put on this client's pasteboard as
//! promises.
//!
//! Announce, never push: an [`Offer`] lists what the clipboard holds, with plain text of a
//! clipboard's worth inline, and the rest is fetched when something pastes it. Echoes are broken
//! three ways: every write here carries [`ORIGIN_TYPE`] and is never announced back, the change
//! count a write produced is remembered and skipped, and contents whose digest matches what a
//! worker just announced are not announced to it again.
//!
//! Copied files are the exception to announcing. Their URLs name files on one machine, so a paste
//! of them moves the files by a transfer ([`ClipSync::files`]). An offer of files carries their
//! URLs alone, one per line: the worker never writes them, and the offer replaces the text the
//! client copied before, which a paste would otherwise write over the files.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use slopty_client::layout::WorkerKey;
use slopty_core::ClientId;
use slopty_platform::pasteboard::{
    CONCEALED_UTI, FILE_URL_UTI, ORIGIN_TYPE, Pasteboard, Provide, TEXT_UTI, TRANSIENT_UTI, Write,
};
use slopty_proto::transfer::{ClipItem, Hash, INLINE_CLIP_BYTES, Offer, Peer};

/// The representations synced, richest first. File URLs name files on one machine only; files
/// move by a transfer instead.
pub const SYNCED: [&str; 5] = ["public.png", "public.tiff", "public.rtf", "public.html", TEXT_UTI];

/// Files on the clipboard, for a paste that moves them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ClipFiles {
    /// Files on this device, copied here.
    Here(Vec<PathBuf>),
    /// Files a worker copied: their URLs are its offer `generation`'s `public.file-url`.
    Worker {
        /// The worker they are on.
        worker: WorkerKey,
        /// Its offer.
        generation: u64,
    },
}

/// A worker's offer of files, and the change count its write here left.
#[derive(Clone, Copy, Debug)]
struct WorkerFiles {
    worker: WorkerKey,
    generation: u64,
    count: i64,
}

/// The clipboard's contents as last read, and what was announced of them.
#[derive(Debug)]
struct Held {
    /// The change count they were read at.
    count: i64,
    /// The offer made of them; `None` for contents never to be announced (Slopty's own write,
    /// a concealed password, nothing synced).
    offer: Option<Offer>,
    /// Their bytes, to answer a fetch.
    reps: Vec<(String, Vec<u8>)>,
}

/// The pasteboard and the bookkeeping that keeps it in step with the workers.
pub struct ClipSync {
    board: Rc<dyn Pasteboard>,
    /// The change count this client's own last write produced.
    wrote: Option<i64>,
    /// This client's offers so far.
    generation: u64,
    held: Option<Held>,
    /// The generation each worker was last told.
    told: HashMap<WorkerKey, u64>,
    /// Digests a worker announced last: the same contents coming back are not announced again.
    heard: Vec<Hash>,
    /// The files a worker's offer put here last.
    worker_files: Option<WorkerFiles>,
}

impl std::fmt::Debug for ClipSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipSync")
            .field("generation", &self.generation)
            .field("wrote", &self.wrote)
            .finish_non_exhaustive()
    }
}

/// The digest an offer carries.
#[must_use]
pub fn digest(bytes: &[u8]) -> Hash {
    *blake3::hash(bytes).as_bytes()
}

impl ClipSync {
    /// Keep `board` in step.
    #[must_use]
    pub fn new(board: Rc<dyn Pasteboard>) -> Self {
        Self {
            board,
            wrote: None,
            generation: 0,
            held: None,
            told: HashMap::new(),
            heard: Vec::new(),
            worker_files: None,
        }
    }

    /// The pasteboard.
    #[must_use]
    pub fn board(&self) -> &Rc<dyn Pasteboard> {
        &self.board
    }

    /// Read the pasteboard again if it changed since it was last read.
    fn refresh(&mut self, me: ClientId) {
        let count = self.board.change_count();
        if self.held.as_ref().is_some_and(|h| h.count == count) {
            return;
        }
        let types = self.board.types();
        let has = |uti: &str| types.iter().any(|t| t == uti);
        let skip = self.wrote == Some(count)
            || has(ORIGIN_TYPE)
            || has(CONCEALED_UTI)
            || has(TRANSIENT_UTI);
        let urls = if skip { Vec::new() } else { self.board.file_urls() };
        let reps: Vec<(String, Vec<u8>)> = if skip {
            Vec::new()
        } else if !urls.is_empty() {
            vec![(FILE_URL_UTI.to_owned(), urls.join("\n").into_bytes())]
        } else {
            SYNCED
                .iter()
                .filter(|uti| has(uti))
                .filter_map(|uti| self.board.data(uti).map(|bytes| ((*uti).to_owned(), bytes)))
                .collect()
        };
        let items: Vec<ClipItem> = reps
            .iter()
            .map(|(uti, bytes)| ClipItem {
                uti: uti.clone(),
                size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                hash: digest(bytes),
                inline: (uti == TEXT_UTI && bytes.len() <= INLINE_CLIP_BYTES)
                    .then(|| bytes.clone()),
            })
            .collect();
        let echo = !items.is_empty() && items.iter().all(|i| self.heard.contains(&i.hash));
        let offer = (!items.is_empty() && !echo).then(|| {
            self.generation = self.generation.saturating_add(1);
            Offer { origin: Peer::Client(me), generation: self.generation, items }
        });
        tracing::debug!(count, skip, echo, offered = offer.is_some(), "clipboard read");
        self.held = Some(Held { count, offer, reps });
    }

    /// What `worker` should hear of this client's clipboard now: its offer, when the clipboard
    /// holds something the worker was not told yet. Sent when a remote tile of that worker
    /// takes the keyboard, and ahead of a paste into one of its windows.
    pub fn offer_for(&mut self, worker: WorkerKey, me: ClientId) -> Option<Offer> {
        self.refresh(me);
        let offer = self.held.as_ref()?.offer.as_ref()?;
        if self.told.get(&worker) == Some(&offer.generation) {
            return None;
        }
        self.told.insert(worker, offer.generation);
        Some(offer.clone())
    }

    /// The files the clipboard holds, for a paste that moves them: files copied here, or the
    /// files of the worker offer this client wrote last, while its write is still there.
    /// Reads the clipboard, so only for a paste.
    pub fn files(&self) -> Option<ClipFiles> {
        let count = self.board.change_count();
        if let Some(files) = self.worker_files.filter(|f| f.count == count) {
            return Some(ClipFiles::Worker { worker: files.worker, generation: files.generation });
        }
        if self.wrote == Some(count) || self.board.types().iter().any(|t| t == ORIGIN_TYPE) {
            return None;
        }
        let paths: Vec<PathBuf> =
            self.board.file_urls().iter().filter_map(|url| file_url_path(url)).collect();
        (!paths.is_empty()).then_some(ClipFiles::Here(paths))
    }

    /// The bytes of `uti` in this client's offer `generation`, for a worker's fetch; `None`
    /// once the clipboard has moved on.
    #[must_use]
    pub fn answer(&self, generation: u64, uti: &str) -> Option<Vec<u8>> {
        let held = self.held.as_ref()?;
        if held.offer.as_ref()?.generation != generation || self.board.change_count() != held.count
        {
            return None;
        }
        held.reps.iter().find(|(t, _)| t == uti).map(|(_, bytes)| bytes.clone())
    }

    /// Worker `from`'s clipboard changed: put its offer here, inline text now and the rest as
    /// promises that `provide` keeps. Contents this clipboard already holds are left alone. Files
    /// it copied are remembered for a paste ([`Self::files`]); what goes on the pasteboard for
    /// them is only what else the offer holds, their names as text.
    pub fn receive(&mut self, from: WorkerKey, offer: &Offer, provide: Provide) {
        let items: Vec<&ClipItem> =
            offer.items.iter().filter(|i| SYNCED.contains(&i.uti.as_str())).collect();
        let files = offer.items.iter().any(|i| i.uti == FILE_URL_UTI);
        if items.is_empty() && !files {
            return;
        }
        self.heard = items.iter().map(|i| i.hash).collect();
        let same = !files
            && self.held.as_ref().filter(|h| h.count == self.board.change_count()).is_some_and(
                |h| {
                    items
                        .iter()
                        .all(|i| h.reps.iter().any(|(t, b)| *t == i.uti && digest(b) == i.hash))
                },
            );
        if same {
            tracing::debug!(generation = offer.generation, "clipboard already holds it");
            return;
        }
        let mut write = Write { origin: origin(offer), ..Write::default() };
        for item in items {
            match &item.inline {
                Some(bytes) => write.data.push((item.uti.clone(), bytes.clone())),
                None => write.promised.push(item.uti.clone()),
            }
        }
        if !write.promised.is_empty() {
            write.provide = Some(provide);
        }
        let count = self.board.write(write);
        self.wrote = Some(count);
        self.held = Some(Held { count, offer: None, reps: Vec::new() });
        self.worker_files =
            files.then_some(WorkerFiles { worker: from, generation: offer.generation, count });
        tracing::debug!(generation = offer.generation, count, files, "clipboard from a worker");
    }

    /// A worker's link is new: it has heard nothing yet.
    pub fn forget(&mut self, worker: WorkerKey) {
        self.told.remove(&worker);
    }
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

/// The paths a `public.file-url` representation names: its URLs, one per line.
#[must_use]
pub fn file_url_paths(bytes: &[u8]) -> Vec<PathBuf> {
    String::from_utf8_lossy(bytes).lines().filter_map(file_url_path).collect()
}

/// The [`ORIGIN_TYPE`] payload: who wrote, and which generation.
fn origin(offer: &Offer) -> Vec<u8> {
    slopty_proto::transfer::origin_bytes(offer.origin, offer.generation)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use slopty_core::WorkerId;
    use slopty_platform::pasteboard::Memory;

    use super::*;

    fn worker_offer(generation: u64, items: Vec<ClipItem>) -> Offer {
        Offer { origin: Peer::Worker(WorkerId::new()), generation, items }
    }

    fn text_item(text: &str) -> ClipItem {
        ClipItem {
            uti: TEXT_UTI.to_owned(),
            size: text.len() as u64,
            hash: digest(text.as_bytes()),
            inline: Some(text.as_bytes().to_vec()),
        }
    }

    fn setup() -> (Rc<Memory>, ClipSync, ClientId, WorkerKey) {
        let board = Rc::new(Memory::default());
        let shared: Rc<dyn Pasteboard> = Rc::<Memory>::clone(&board);
        let sync = ClipSync::new(shared);
        (board, sync, ClientId::new(), WorkerKey::new(1))
    }

    #[test]
    fn a_copy_here_is_announced_once_per_worker_with_small_text_inline() {
        let (board, mut sync, me, studio) = setup();
        assert!(sync.offer_for(studio, me).is_none(), "an empty clipboard says nothing");
        let png = vec![9_u8; INLINE_CLIP_BYTES + 1];
        board.copy(&[
            (TEXT_UTI, b"hello"),
            ("public.png", &png),
            ("public.file-url", b"file:///x"),
        ]);
        let offer = sync.offer_for(studio, me).unwrap();
        let utis: Vec<_> = offer.items.iter().map(|i| i.uti.as_str()).collect();
        assert_eq!(utis, ["public.png", TEXT_UTI], "richest first, no file URLs");
        assert_eq!(offer.items[1].inline.as_deref(), Some(&b"hello"[..]));
        assert!(offer.items[0].inline.is_none(), "a picture is fetched");
        assert_eq!(offer.items[0].hash, digest(&png));
        assert!(sync.offer_for(studio, me).is_none(), "told already");
        assert!(sync.offer_for(WorkerKey::new(2), me).is_some(), "the other worker was not");
        assert_eq!(sync.answer(offer.generation, "public.png"), Some(png));
        board.copy(&[(TEXT_UTI, b"newer")]);
        assert_eq!(sync.answer(offer.generation, "public.png"), None, "the clipboard moved on");
    }

    #[test]
    fn a_worker_offer_lands_as_text_and_promises_and_is_never_announced_back() {
        let (board, mut sync, me, studio) = setup();
        let png = ClipItem { uti: "public.png".to_owned(), size: 4, hash: [1; 32], inline: None };
        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let log = Arc::clone(&asked);
        let provide: Provide = Arc::new(move |uti: &str| {
            log.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            (uti == "public.png").then(|| vec![1, 2, 3, 4])
        });
        sync.receive(studio, &worker_offer(7, vec![png, text_item("from the worker")]), provide);
        assert_eq!(board.data(TEXT_UTI).as_deref(), Some(&b"from the worker"[..]));
        assert_eq!(board.promised(), ["public.png"]);
        assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 0, "fetched only on paste");
        assert_eq!(board.data("public.png"), Some(vec![1, 2, 3, 4]));
        assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 1);
        let stamp = board.data(ORIGIN_TYPE).unwrap();
        let (peer, generation) = slopty_proto::transfer::parse_origin(&stamp).unwrap();
        assert!(matches!(peer, Peer::Worker(_)) && generation == 7, "{peer:?} {generation}");
        assert!(sync.offer_for(studio, me).is_none(), "our own write is not an announcement");
    }

    #[test]
    fn the_same_contents_coming_back_unstamped_are_not_echoed() {
        let (board, mut sync, me, studio) = setup();
        let noop: Provide = Arc::new(|_: &str| None);
        sync.receive(studio, &worker_offer(1, vec![text_item("ping")]), noop);
        // Universal Clipboard delivers the worker's copy again, without Slopty's stamp.
        board.copy(&[(TEXT_UTI, b"ping")]);
        assert!(sync.offer_for(studio, me).is_none(), "same digest: an echo");
        board.copy(&[(TEXT_UTI, b"pong")]);
        assert!(sync.offer_for(studio, me).is_some(), "new contents are announced");
    }

    /// Files copied here are offered as their URLs alone and named for a paste; a worker's
    /// copied files are named by where they are, while its write is what the clipboard holds.
    #[test]
    fn copied_files_are_offered_as_urls_and_named_for_a_paste() {
        let (board, mut sync, me, studio) = setup();
        board.copy(&[(TEXT_UTI, b"older text")]);
        assert!(sync.offer_for(studio, me).is_some());
        board.copy_files(&["file:///Users/me/a%20b.txt", "file:///tmp/c"]);
        let offer = sync.offer_for(studio, me).expect("the files replace the text");
        let utis: Vec<_> = offer.items.iter().map(|i| i.uti.as_str()).collect();
        assert_eq!(utis, [FILE_URL_UTI]);
        assert_eq!(
            sync.answer(offer.generation, FILE_URL_UTI).as_deref(),
            Some(&b"file:///Users/me/a%20b.txt\nfile:///tmp/c"[..])
        );
        let here = vec![PathBuf::from("/Users/me/a b.txt"), PathBuf::from("/tmp/c")];
        assert_eq!(sync.files(), Some(ClipFiles::Here(here)));

        let noop: Provide = Arc::new(|_: &str| None);
        let url = ClipItem { uti: FILE_URL_UTI.to_owned(), size: 9, hash: [3; 32], inline: None };
        sync.receive(studio, &worker_offer(4, vec![url, text_item("a.txt")]), noop);
        assert_eq!(board.data(TEXT_UTI).as_deref(), Some(&b"a.txt"[..]), "the names as text");
        assert_eq!(sync.files(), Some(ClipFiles::Worker { worker: studio, generation: 4 }));
        assert!(sync.offer_for(studio, me).is_none(), "our own write");
        board.copy(&[(TEXT_UTI, b"moved on")]);
        assert_eq!(sync.files(), None);
    }

    #[test]
    fn file_urls_become_paths() {
        assert_eq!(file_url_path("file:///tmp/%C3%BC%20x"), Some(PathBuf::from("/tmp/ü x")));
        assert_eq!(file_url_path("file://localhost/tmp/a"), Some(PathBuf::from("/tmp/a")));
        assert_eq!(file_url_path("https://example.com/a"), None);
        assert_eq!(file_url_path("file:///tmp/%zz"), None);
        assert_eq!(
            file_url_paths(b"file:///a\nfile:///b%2Fc"),
            [PathBuf::from("/a"), PathBuf::from("/b/c")]
        );
    }

    #[test]
    fn a_password_or_a_transient_copy_is_never_announced() {
        let (board, mut sync, me, studio) = setup();
        board.copy(&[(TEXT_UTI, b"hunter2"), (CONCEALED_UTI, b"")]);
        assert!(sync.offer_for(studio, me).is_none());
        board.copy(&[(TEXT_UTI, b"otp"), (TRANSIENT_UTI, b"")]);
        assert!(sync.offer_for(studio, me).is_none());
    }
}
