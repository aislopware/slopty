use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};

use slopty_core::{ClientId, WorkerId};

use super::*;

/// A pasteboard in memory: items of bytes and promises, as `NSPasteboard` holds them.
#[derive(Default)]
struct Fake {
    count: AtomicIsize,
    items: Mutex<Vec<Item>>,
    provide: Mutex<Option<Provide>>,
    /// Run once inside the next `write`, after the contents land and before it returns.
    during_write: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Run once inside the next read of a representation, after its bytes are taken.
    during_read: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Reading asks the person first, as the general pasteboard does until they allow it.
    asks: AtomicBool,
    /// Reads of a representation's bytes so far, by type.
    reads: Mutex<Vec<String>>,
}

impl Access for Fake {
    fn access(&self) -> ReadAccess {
        if self.asks.load(Ordering::SeqCst) { ReadAccess::NotAskedYet } else { ReadAccess::Allowed }
    }
}

impl Fake {
    /// Another app copied `items`: it cleared the pasteboard, then put them on.
    fn copy_items(&self, items: &[&[(&str, &[u8])]]) {
        self.clear();
        self.put(items);
    }

    /// Another app copied one item of `reps`.
    fn copy(&self, reps: &[(&str, &[u8])]) {
        self.copy_items(&[reps]);
    }

    /// `clearContents`: the board is empty and its count moves.
    fn clear(&self) {
        self.items.lock().clear();
        *self.provide.lock() = None;
        self.count.fetch_add(1, Ordering::SeqCst);
    }

    /// `writeObjects:` after a clear: the contents land under the count the clear left.
    fn put(&self, items: &[&[(&str, &[u8])]]) {
        *self.items.lock() = items
            .iter()
            .map(|reps| {
                Item::data(reps.iter().map(|(t, b)| ((*t).to_owned(), b.to_vec())).collect())
            })
            .collect();
    }

    fn reads_of(&self, kind: &str) -> usize {
        self.reads.lock().iter().filter(|t| *t == kind).count()
    }

    /// What a paste in an app on the worker gets: the bytes, asking a promise's provider.
    fn paste(&self, item: usize, kind: &str) -> Option<Vec<u8>> {
        self.data(item, kind)
    }

    fn bytes(&self, item: usize, kind: &str) -> Option<Vec<u8>> {
        let promised = {
            let items = self.items.lock();
            let entry = items.get(item)?;
            if let Some((_t, b)) = entry.data.iter().find(|(t, _b)| t == kind) {
                return Some(b.clone());
            }
            let promised = entry.promised.iter().any(|t| t == kind);
            drop(items);
            promised
        };
        let provide = self.provide.lock().clone().filter(|_| promised)?;
        provide(item, kind)
    }
}

impl Board for Fake {
    fn change_count(&self) -> isize {
        self.count.load(Ordering::SeqCst)
    }

    fn items(&self) -> Vec<Vec<String>> {
        let items = self.items.lock();
        items
            .iter()
            .map(|i| {
                i.data.iter().map(|(t, _b)| t.clone()).chain(i.promised.iter().cloned()).collect()
            })
            .collect()
    }

    fn data(&self, item: usize, kind: &str) -> Option<Vec<u8>> {
        self.reads.lock().push(kind.to_owned());
        let bytes = self.bytes(item, kind);
        let hook = self.during_read.lock().take();
        if let Some(hook) = hook {
            hook();
        }
        bytes
    }

    fn write(&self, items: &[Item], provide: Option<Provide>) -> Option<isize> {
        *self.items.lock() = items.to_vec();
        *self.provide.lock() = provide;
        let count = self.count.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
        let hook = self.during_write.lock().take();
        if let Some(hook) = hook {
            hook();
        }
        Some(count)
    }
}

fn next_link() -> Link {
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A pasteboard as a Mac that has been up a while has it: its count far from zero, and
/// whatever was last copied still on it.
fn board() -> Fake {
    let board = Fake::default();
    board.count.store(7_301, Ordering::SeqCst);
    board.put(&[&[(&text(), b"copied before this process started")]]);
    board
}

fn clip() -> Clipboard<Fake> {
    Clipboard::new(board(), Peer::Worker(WorkerId::new()))
}

fn text() -> String {
    board_type(ClipFormat::Text)
}

fn png() -> String {
    board_type(ClipFormat::Png)
}

fn inline(kind: ClipFormat, bytes: &[u8]) -> Rep {
    Rep {
        kind: ClipType::Format(kind),
        size: Some(bytes.len() as u64),
        hash: Some(digest(bytes)),
        inline: Some(bytes.to_vec()),
    }
}

fn listed(kind: ClipType, bytes: &[u8]) -> Rep {
    Rep { kind, size: Some(bytes.len() as u64), hash: Some(digest(bytes)), inline: None }
}

fn client_offer(generation: u64, age_ms: u64, items: Vec<Vec<Rep>>) -> Offer {
    Offer {
        origin: Peer::Client(ClientId::nil()),
        generation,
        age_ms,
        concealed: false,
        items: items.into_iter().map(|reps| ClipEntry { reps }).collect(),
    }
}

fn texts(offer: &Offer) -> Vec<Option<&[u8]>> {
    offer.reps().map(|(_, r)| r.inline.as_deref()).collect()
}

/// Every message sent to a client, in order.
fn sink() -> (Sink, Arc<Mutex<Vec<ClipMsg>>>) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&sent);
    (Arc::new(move |msg| log.lock().push(msg)), sent)
}

#[test]
fn nothing_is_read_or_announced_while_nobody_watches() {
    let c = clip();
    let a = next_link();
    let now = Instant::now();
    c.board().copy(&[(&text(), b"one")]);
    assert_eq!(c.poll(now), None, "nobody watches");
    let mut interest = c.interest();
    assert_eq!(*interest.borrow_and_update(), Interest::Idle);
    c.watch(a, true);
    assert!(interest.has_changed().unwrap());
    assert_eq!(*interest.borrow_and_update(), Interest::Watched);
    assert_eq!(c.poll(now), None, "a change made while unwatched is not announced");
    c.board().copy(&[(&text(), b"two")]);
    let offer = c.poll(now).expect("a change while watched");
    assert_eq!(texts(&offer), [Some(&b"two"[..])]);
    assert_eq!(c.poll(now), None, "announced once");
    c.forget(a);
    assert_eq!(*interest.borrow_and_update(), Interest::Idle);
    c.board().copy(&[(&text(), b"three")]);
    assert_eq!(c.poll(now), None);
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
    assert_eq!(c.poll(Instant::now()), None);
    assert!(c.board().reads.lock().is_empty(), "the contents were not read");
    let offer = client_offer(1, 0, vec![vec![inline(ClipFormat::Text, b"typed")]]);
    if c.offered(a, offer, Instant::now()) {
        c.mirror(a);
    }
    assert_eq!(c.paste(a, PasteKind::Window), Paste::Ready);
    assert_eq!(c.board().data(0, &text()).unwrap(), b"typed", "a paste still writes");
    c.board().asks.store(false, Ordering::SeqCst);
    c.board().copy(&[(&text(), b"allowed now")]);
    let offer = c.poll(Instant::now()).expect("reads are free again");
    assert_eq!(texts(&offer), [Some(&b"allowed now"[..])]);
}

/// Another process's copy clears the pasteboard, which moves its count, and puts the new
/// contents on after, under that same count. A poll that lands in between finds the board
/// empty; the contents are still announced once they are there.
#[test]
fn contents_put_on_after_a_poll_saw_the_board_cleared_are_announced() {
    let c = clip();
    c.watch(next_link(), true);
    c.board().clear();
    assert_eq!(c.poll(Instant::now()), None, "nothing on the board yet");
    c.board().put(&[&[(&text(), b"landed")]]);
    let offer = c.poll(Instant::now()).expect("the contents that landed under the clear's count");
    assert_eq!(texts(&offer), [Some(&b"landed"[..])]);
    assert_eq!(c.poll(Instant::now()), None, "announced once");
}

/// A poll reads the types and small text and nothing else: a 200 MB picture is listed by type
/// with no size, and read off the pasteboard only by a fetch, which a cap turns away.
#[test]
fn a_big_copy_reads_only_its_types_until_fetched() {
    let c = clip();
    c.watch(next_link(), true);
    let picture = vec![7_u8; 200 << 20];
    c.board().copy(&[(&png(), &picture), (&text(), b"a caption")]);
    let offer = c.poll(Instant::now()).unwrap();
    assert_eq!(c.board().reads_of(&png()), 0, "the picture was not read");
    let reps: Vec<&Rep> = offer.reps().map(|(_, r)| r).collect();
    assert_eq!(reps[0].kind, ClipType::Format(ClipFormat::Png), "the copying app's order");
    assert_eq!((reps[0].size, reps[0].hash, reps[0].inline.as_ref()), (None, None, None));
    assert_eq!(reps[1].inline.as_deref(), Some(&b"a caption"[..]));

    let rep = offer.rep_ref(0, ClipType::Format(ClipFormat::Png));
    assert_eq!(c.fetch(&rep, Some(8 << 20)), Fetched::TooBig(200 << 20), "past the cap");
    assert_eq!(c.board().reads_of(&png()), 1);
    let Fetched::Data(bytes) = c.fetch(&rep, None) else { panic!("a paste takes it whole") };
    assert_eq!(bytes.len(), picture.len());
    assert_eq!(c.board().reads_of(&png()), 2, "nothing was kept from the capped read");
    assert!(matches!(c.fetch(&rep, None), Fetched::Data(_)));
    assert_eq!(c.board().reads_of(&png()), 3, "too big to keep for the offer");

    c.board().copy(&[(&png(), b"\x89PNG small")]);
    let offer = c.poll(Instant::now()).unwrap();
    let small = offer.rep_ref(0, ClipType::Format(ClipFormat::Png));
    for _ in 0..2 {
        assert_eq!(c.fetch(&small, None), Fetched::Data(Bytes::from_static(b"\x89PNG small")));
    }
    assert_eq!(c.board().reads_of(&png()), 4, "a small one is read once and kept");
}

/// Text past what rides inline is measured at the poll, not kept: its bytes are read again
/// when fetched.
#[test]
fn long_text_is_measured_at_a_poll_and_not_kept() {
    let c = clip();
    c.watch(next_link(), true);
    let long = vec![b'a'; INLINE_CLIP_BYTES + 1];
    c.board().copy(&[(&text(), &long)]);
    let offer = c.poll(Instant::now()).unwrap();
    let rep = &offer.items[0].reps[0];
    assert_eq!((rep.size, rep.hash, rep.inline.as_ref()), (Some(long.len() as u64), None, None));
    let text_ref = offer.rep_ref(0, ClipType::Format(ClipFormat::Text));
    assert_eq!(c.fetch(&text_ref, None), Fetched::Data(Bytes::from(long)));
    assert_eq!(c.board().reads_of(&text()), 2, "read at the poll, and again by the fetch");
}

/// The types of each item an offer lists, in order.
fn kinds(offer: &Offer) -> Vec<Vec<ClipType>> {
    offer.items.iter().map(|i| i.reps.iter().map(|r| r.kind.clone()).collect()).collect()
}

/// A copy lands on the pasteboard a type at a time, all under the one count its clear left:
/// `NSPasteboard` moves the count with ownership, not with each type written, and promises no
/// more. A poll that finds more types under a count it announced announces the whole copy
/// again, so an offer read half written never stands.
#[test]
fn a_copy_that_grows_under_its_count_is_announced_whole() {
    let c = clip();
    c.watch(next_link(), true);
    let (png, text) = (png(), text());
    c.board().clear();
    c.board().put(&[&[(&png, b"\x89PNG picture")]]);
    let half = c.poll(Instant::now()).expect("what is there so far");
    assert_eq!(kinds(&half), [[ClipType::Format(ClipFormat::Png)]]);
    c.board().put(&[&[
        (&png, b"\x89PNG picture"),
        (&text, b"copied"),
        ("com.example.own", b"own"),
    ]]);
    let whole = c.poll(Instant::now()).expect("the rest of the copy, under the same count");
    assert_eq!(
        kinds(&whole),
        [[
            ClipType::Format(ClipFormat::Png),
            ClipType::Format(ClipFormat::Text),
            ClipType::Apple("com.example.own".to_owned()),
        ]]
    );
    assert!(whole.generation > half.generation, "it replaces the half");
    assert_eq!(texts(&whole)[1], Some(&b"copied"[..]));
    assert_eq!(c.poll(Instant::now()), None, "announced once more, not again");
    let rep = whole.rep_ref(0, ClipType::Format(ClipFormat::Png));
    assert_eq!(c.fetch(&rep, None), Fetched::Data(Bytes::from_static(b"\x89PNG picture")));
}

/// A poll whose read of the copy's text sees more types land meanwhile announces nothing then:
/// the copy is still being written. The next poll announces it whole.
#[test]
fn a_copy_still_being_written_as_it_is_read_waits_for_the_next_poll() {
    let c = Arc::new(clip());
    c.watch(next_link(), true);
    c.board().clear();
    c.board().put(&[&[(&text(), b"copied")]]);
    let hook = {
        let (weak, png, text) = (Arc::downgrade(&c), png(), text());
        move || {
            if let Some(c) = weak.upgrade() {
                c.board().put(&[&[(&text, b"copied"), (&png, b"\x89PNG picture")]]);
            }
        }
    };
    *c.board().during_read.lock() = Some(Box::new(hook));
    assert_eq!(c.poll(Instant::now()), None, "read while it grew");
    let whole = c.poll(Instant::now()).expect("the whole copy");
    assert_eq!(
        kinds(&whole),
        [[ClipType::Format(ClipFormat::Text), ClipType::Format(ClipFormat::Png)]]
    );
    assert_eq!(c.poll(Instant::now()), None, "once");
}

/// An item whose types have not landed yet is a copy half done: nothing is announced for it,
/// and its types are announced once they are there.
#[test]
fn an_item_with_no_types_yet_is_announced_once_they_land() {
    let c = clip();
    c.watch(next_link(), true);
    c.board().clear();
    c.board().put(&[&[]]);
    assert_eq!(c.poll(Instant::now()), None, "nothing to offer yet");
    c.board().put(&[&[(&text(), b"late")]]);
    let offer = c.poll(Instant::now()).expect("its types landed");
    assert_eq!(texts(&offer), [Some(&b"late"[..])]);
}

/// Someone copies while a fetch reads: what was read is not served under the offer before.
#[test]
fn a_copy_landing_during_a_fetch_is_not_served_under_the_old_offer() {
    let c = Arc::new(clip());
    c.watch(next_link(), true);
    c.board().copy(&[(&png(), b"\x89PNG first")]);
    let offer = c.poll(Instant::now()).unwrap();
    let hook = {
        let weak = Arc::downgrade(&c);
        move || {
            if let Some(c) = weak.upgrade() {
                c.board().copy(&[(&png(), b"\x89PNG second")]);
            }
        }
    };
    *c.board().during_read.lock() = Some(Box::new(hook));
    let rep = offer.rep_ref(0, ClipType::Format(ClipFormat::Png));
    assert_eq!(c.fetch(&rep, None), Fetched::Unavailable);
}

/// The first look after this process starts finds contents copied at a time nobody knows:
/// older than any client's copy, which goes on the pasteboard and pastes, however long ago it
/// was made.
#[test]
fn contents_found_at_start_lose_to_any_clients_copy() {
    let c = clip();
    let a = next_link();
    c.attach(a, sink().0);
    c.watch(a, true);
    let now = Instant::now();
    let offer =
        client_offer(1, 3_600_000, vec![vec![inline(ClipFormat::Text, b"copied an hour ago")]]);
    assert!(c.offered(a, offer, now), "mirrored");
    assert!(c.mirror(a));
    assert_eq!(c.paste(a, PasteKind::Window), Paste::Ready);
    assert_eq!(c.board().data(0, &text()).unwrap(), b"copied an hour ago");
}

/// A copy made on the worker while no client was linked is placed at when the last one left:
/// a client's copy since then is newer, one from before is older.
#[test]
fn a_copy_made_while_no_client_was_linked_is_placed_when_the_last_left() {
    let c = clip();
    let a = next_link();
    c.attach(a, sink().0);
    c.observe(Instant::now());
    c.forget(a);
    let left = Instant::now();
    c.board().copy(&[(&text(), b"copied while nobody was linked")]);
    let back = left.checked_add(Duration::from_secs(10)).unwrap();
    let b = next_link();
    c.attach(b, sink().0);
    c.observe(back);
    let before = client_offer(1, 20_000, vec![vec![inline(ClipFormat::Text, b"before")]]);
    assert!(!c.offered(b, before, back), "copied before the last client left");
    let since = client_offer(2, 5_000, vec![vec![inline(ClipFormat::Text, b"since")]]);
    assert!(c.offered(b, since, back), "copied after it left");
}

/// A restarted worker keeps its id, so its offers must not start again at a generation its
/// clients still hold.
#[test]
fn a_restarted_workers_first_offer_is_a_new_generation() {
    let me = Peer::Worker(WorkerId::new());
    let first_offer = || {
        let c = Clipboard::new(board(), me);
        c.watch(next_link(), true);
        c.board().copy(&[(&text(), b"after a start")]);
        c.poll(Instant::now()).unwrap().generation
    };
    assert_ne!(first_offer(), first_offer());
}

#[test]
fn a_fetch_after_the_board_moved_is_unavailable() {
    let c = clip();
    c.watch(next_link(), true);
    c.board().copy(&[(&png(), b"\x89PNG"), (&text(), b"hi")]);
    let offer = c.poll(Instant::now()).unwrap();
    let rep = offer.rep_ref(0, ClipType::Format(ClipFormat::Png));
    let rtf = offer.rep_ref(0, ClipType::Format(ClipFormat::Rtf));
    assert_eq!(c.fetch(&rtf, None), Fetched::Unavailable, "never listed");
    // Another app copies, and nobody has polled yet: the count alone says the offer is gone.
    c.board().copy(&[(&png(), b"another picture")]);
    assert_eq!(c.fetch(&rep, None), Fetched::Unavailable);
    let newer = c.poll(Instant::now()).unwrap();
    assert!(newer.generation > offer.generation);
    assert_eq!(c.fetch(&rep, None), Fetched::Unavailable, "an older generation");
    let now = newer.rep_ref(0, ClipType::Format(ClipFormat::Png));
    assert_eq!(c.fetch(&now, None), Fetched::Data(Bytes::from_static(b"another picture")));
}

/// Every item of a copy, and every type it holds, is announced in order and fetched back by
/// item: a Finder copy's files, an app's private type, a PDF. Dynamic types, old names and the
/// markers are left behind.
#[test]
fn every_item_and_apple_type_round_trips() {
    let c = clip();
    c.watch(next_link(), true);
    let url = board_type(ClipFormat::FileUrls);
    c.board().copy_items(&[
        &[(&url, b"file:///tmp/a.txt"), (&text(), b"a.txt"), ("dyn.ah62d4rv4gu8y", b"x")],
        &[(&url, b"file:///tmp/b.pdf"), ("com.adobe.pdf", b"%PDF"), ("NSStringPboardType", b"b")],
        &[("com.apple.keynote.slide", b"private")],
    ]);
    let offer = c.poll(Instant::now()).unwrap();
    let kinds: Vec<Vec<ClipType>> =
        offer.items.iter().map(|i| i.reps.iter().map(|r| r.kind.clone()).collect()).collect();
    let apple = |s: &str| ClipType::Apple(s.to_owned());
    assert_eq!(
        kinds,
        [
            vec![ClipType::Format(ClipFormat::FileUrls), ClipType::Format(ClipFormat::Text)],
            vec![ClipType::Format(ClipFormat::FileUrls), apple("com.adobe.pdf")],
            vec![apple("com.apple.keynote.slide")],
        ]
    );
    assert_eq!(offer.items[1].reps[0].inline.as_deref(), Some(&b"file:///tmp/b.pdf"[..]));
    for (item, kind, bytes) in [
        (1, apple("com.adobe.pdf"), &b"%PDF"[..]),
        (2, apple("com.apple.keynote.slide"), b"private"),
        (0, ClipType::Format(ClipFormat::Text), b"a.txt"),
    ] {
        let rep = offer.rep_ref(item, kind);
        assert_eq!(c.fetch(&rep, None), Fetched::Data(Bytes::copy_from_slice(bytes)));
    }

    // The same shape from a client lands as the same items.
    let a = next_link();
    let theirs = client_offer(
        3,
        0,
        vec![
            vec![inline(ClipFormat::Text, b"first")],
            vec![listed(apple("com.adobe.pdf"), b"%PDF client")],
        ],
    );
    let (tell, sent) = sink();
    c.attach(a, tell);
    assert!(c.offered(a, theirs.clone(), Instant::now()));
    assert!(c.mirror(a));
    let items = c.board().items();
    assert_eq!(items.len(), 2, "{items:?}");
    assert!(items[1].iter().any(|t| t == "com.adobe.pdf"), "{items:?}");
    let rep = theirs.rep_ref(1, apple("com.adobe.pdf"));
    // A paste on the worker asks the promise, which fetches from the client.
    let c = Arc::new(c);
    let pasting = {
        let c = Arc::clone(&c);
        std::thread::spawn(move || c.board().paste(1, "com.adobe.pdf"))
    };
    let deadline = Instant::now().checked_add(Duration::from_secs(10)).unwrap();
    while sent.lock().is_empty() {
        assert!(Instant::now() < deadline, "the promise asks the client");
        std::thread::yield_now();
    }
    assert_eq!(
        sent.lock().as_slice(),
        [ClipMsg::Fetch { rep: rep.clone(), max: None, urgent: true }]
    );
    c.supply(a, &rep, b"%PDF client".to_vec());
    assert_eq!(pasting.join().unwrap().as_deref(), Some(&b"%PDF client"[..]));
    assert_eq!(c.board().paste(0, &text()).as_deref(), Some(&b"first"[..]));
}

/// A client's offer goes onto the pasteboard as soon as it arrives, unless the worker's own
/// copy is newer: the latest copy wins, on the worker's clock. A paste chord re-writes the
/// paster's clipboard when another client's is there.
#[test]
fn the_focused_clients_copy_is_mirrored_unless_the_workers_is_newer() {
    let c = clip();
    let (a, b) = (next_link(), next_link());
    let t0 = Instant::now();
    let at = |ms: u64| t0.checked_add(Duration::from_millis(ms)).unwrap();
    c.attach(a, sink().0);
    c.attach(b, sink().0);
    c.watch(a, true);

    // The worker's person copies at 1 s.
    c.board().copy(&[(&text(), b"worker copy")]);
    assert!(c.poll(at(1_000)).is_some());
    // A copy made at 0.5 s reaches the worker at 1.5 s: older, so the worker's stays.
    let old = client_offer(1, 1_000, vec![vec![inline(ClipFormat::Text, b"older client copy")]]);
    assert!(!c.offered(a, old, at(1_500)));
    assert_eq!(c.board().data(0, &text()).unwrap(), b"worker copy");
    assert_eq!(c.paste(a, PasteKind::Window), Paste::Ready, "the worker's newer copy pastes");
    assert_eq!(c.board().data(0, &text()).unwrap(), b"worker copy");

    // A copy made at 1.9 s: newer, mirrored at once.
    let new = client_offer(2, 100, vec![vec![inline(ClipFormat::Text, b"client copy")]]);
    assert!(c.offered(a, new.clone(), at(2_000)));
    assert!(c.mirror(a));
    assert_eq!(c.board().data(0, &text()).unwrap(), b"client copy");
    assert!(!c.offered(a, new, at(2_100)), "the same offer again writes nothing");
    assert_eq!(c.poll(at(2_200)), None, "the mirror is not announced back");

    // A second client's newer copy takes the pasteboard; each paste pastes the paster's own.
    let other = client_offer(1, 0, vec![vec![inline(ClipFormat::Text, b"other client")]]);
    assert!(c.offered(b, other, at(3_000)));
    assert!(c.mirror(b));
    assert_eq!(c.board().data(0, &text()).unwrap(), b"other client");
    assert_eq!(c.paste(a, PasteKind::Window), Paste::Ready);
    assert_eq!(c.board().data(0, &text()).unwrap(), b"client copy");
    assert_eq!(c.paste(b, PasteKind::Window), Paste::Ready);
    assert_eq!(c.board().data(0, &text()).unwrap(), b"other client");

    // While nobody watches, the count alone is read, and a change still counts as the newest.
    c.watch(a, false);
    c.board().copy(&[(&text(), b"unwatched worker copy")]);
    c.observe(at(4_000));
    let stale = client_offer(3, 2_000, vec![vec![inline(ClipFormat::Text, b"stale")]]);
    assert!(!c.offered(a, stale, at(4_500)), "copied at 2.5 s, before the worker's at 4 s");
}

/// A secret from a client is never mirrored. The paste chord fetches it, writes it marked
/// concealed and transient, and it is cleared after its time or once the client's clipboard
/// moves on, unless something else was copied over it.
#[test]
fn a_concealed_copy_is_written_only_by_a_paste_and_cleared_after() {
    let c = clip();
    let a = next_link();
    c.attach(a, sink().0);
    c.board().copy(&[(&text(), b"before")]);
    c.observe(Instant::now());
    let secret = |generation| Offer {
        concealed: true,
        ..client_offer(
            generation,
            0,
            vec![vec![Rep {
                kind: ClipType::Format(ClipFormat::Text),
                size: None,
                hash: None,
                inline: None,
            }]],
        )
    };
    let later = Instant::now().checked_add(Duration::from_millis(5)).unwrap();
    assert!(!c.offered(a, secret(1), later), "never mirrored");
    assert_eq!(c.board().data(0, &text()).unwrap(), b"before");
    let Paste::Fetch(reps) = c.paste(a, PasteKind::Window) else { panic!("fetched by the paste") };
    let rep = reps[0].clone();
    assert!(c.supply(a, &rep, b"hunter2".to_vec()));
    let now = Instant::now();
    assert!(c.write_incoming(a, now));
    let items = c.board().items();
    assert!(items[0].iter().any(|t| t == CONCEALED_TYPE), "{items:?}");
    assert!(items[0].iter().any(|t| t == TRANSIENT_TYPE), "{items:?}");
    assert_eq!(c.board().data(0, &text()).unwrap(), b"hunter2");
    assert_eq!(c.paste(a, PasteKind::Window), Paste::Ready, "pasted again as it is");

    c.expire(now.checked_add(Duration::from_secs(59)).unwrap());
    assert_eq!(c.board().data(0, &text()).unwrap(), b"hunter2", "not yet");
    c.expire(now.checked_add(CONCEALED_FOR).unwrap());
    assert!(c.board().items().is_empty(), "cleared after its time");

    // Written again, then the client copies something else: the secret goes with it.
    assert!(!c.offered(a, secret(2), Instant::now()));
    let Paste::Fetch(reps) = c.paste(a, PasteKind::Window) else { panic!("fetched again") };
    assert!(c.supply(a, &reps[0], b"hunter3".to_vec()));
    assert!(c.write_incoming(a, Instant::now()));
    let next = client_offer(3, 0, vec![vec![inline(ClipFormat::Text, b"plain")]]);
    assert!(c.offered(a, next, Instant::now()));
    assert!(c.board().items().is_empty() || c.board().data(0, &text()).unwrap() != b"hunter3");

    // A secret someone copied over on the worker is not the client's to clear.
    assert!(!c.offered(a, secret(4), Instant::now()));
    let Paste::Fetch(reps) = c.paste(a, PasteKind::Window) else { panic!("fetched") };
    assert!(c.supply(a, &reps[0], b"hunter4".to_vec()));
    assert!(c.write_incoming(a, Instant::now()));
    c.board().copy(&[(&text(), b"copied over it")]);
    c.forget(a);
    assert_eq!(c.board().data(0, &text()).unwrap(), b"copied over it");
}

/// A worker's secret is announced by type alone: nothing read, nothing inline.
#[test]
fn a_workers_secret_is_offered_without_bytes() {
    let c = clip();
    c.watch(next_link(), true);
    c.board().copy(&[(&text(), b"hunter2"), (CONCEALED_TYPE, b"")]);
    let offer = c.poll(Instant::now()).unwrap();
    assert!(offer.concealed);
    assert_eq!(texts(&offer), [None], "only the text, by type");
    assert_eq!(c.board().reads_of(&text()), 0);
    c.board().copy(&[(&text(), b"soon gone"), (TRANSIENT_TYPE, b"")]);
    assert!(c.poll(Instant::now()).unwrap().concealed);
}

/// The worker's own write is not announced back: not the change it made, not contents that
/// name this worker as their origin, not contents it just wrote under another count.
#[test]
fn echoes_are_broken_three_ways() {
    let c = clip();
    let a = next_link();
    c.watch(a, true);
    let offer = client_offer(1, 0, vec![vec![inline(ClipFormat::Text, b"from the client")]]);
    assert!(c.offered(a, offer, Instant::now()));
    assert!(c.mirror(a));
    assert_eq!(c.board().data(0, &text()).unwrap(), b"from the client");
    assert_eq!(c.poll(Instant::now()), None, "the write's own change count");

    // A client on this very Mac puts the worker's announced contents back, stamped with the
    // worker as their origin.
    c.board().copy(&[(&text(), b"worker text")]);
    let announced = c.poll(Instant::now()).unwrap();
    let origin = origin_bytes(announced.origin, announced.generation);
    c.board().copy(&[(&text(), b"worker text"), (ORIGIN_TYPE, &origin)]);
    assert_eq!(c.poll(Instant::now()), None, "origin names this worker");

    // Universal Clipboard delivers the same text again with no origin.
    c.board().copy(&[(&text(), b"worker text")]);
    assert_eq!(c.poll(Instant::now()), None, "same digest as announced");
    c.board().copy(&[(&text(), b"something else")]);
    assert!(c.poll(Instant::now()).is_some());
}

/// A shell's paste of a picture fetches the picture before the chord goes on, then writes it
/// once, stamped with its origin; file URLs are never written. Bytes that do not match their
/// digest are dropped, and the paste goes on without them.
#[test]
fn a_picture_paste_fetches_the_picture_then_writes_it_once() {
    let c = clip();
    let a = next_link();
    let picture = b"\x89PNG picture".to_vec();
    let offer = |generation| {
        client_offer(
            generation,
            0,
            vec![vec![
                listed(ClipType::Format(ClipFormat::FileUrls), b"file:///x"),
                listed(ClipType::Format(ClipFormat::Png), &picture),
                inline(ClipFormat::Text, b"hi"),
            ]],
        )
    };
    let png_ref = offer(7).rep_ref(0, ClipType::Format(ClipFormat::Png));
    let _mirror = c.offered(a, offer(7), Instant::now());
    let Paste::Fetch(reps) = c.paste(a, PasteKind::Picture) else { panic!("a fetch first") };
    assert_eq!(reps, std::slice::from_ref(&png_ref), "a file URL is not pasted");
    let other = offer(6).rep_ref(0, ClipType::Format(ClipFormat::Png));
    assert!(!c.supply(a, &other, picture.clone()), "another offer's data");
    assert!(c.supply(a, &png_ref, picture.clone()));
    assert!(c.write_incoming(a, Instant::now()));
    assert_eq!(c.board().data(0, &png()).unwrap(), picture);
    assert_eq!(c.board().data(0, &text()).unwrap(), b"hi");
    let (peer, generation) = parse_origin(&c.board().data(0, ORIGIN_TYPE).unwrap()).unwrap();
    assert_eq!((peer, generation), (Peer::Client(ClientId::nil()), 7), "stamped with its origin");
    assert_eq!(c.paste(a, PasteKind::Picture), Paste::Ready, "written once");
    assert_eq!(
        c.paste(next_link(), PasteKind::Window),
        Paste::Ready,
        "a client that offered nothing"
    );

    let _mirror = c.offered(a, offer(8), Instant::now());
    let Paste::Fetch(reps) = c.paste(a, PasteKind::Picture) else { panic!("a fetch first") };
    assert!(c.supply(a, &reps[0], b"tampered".to_vec()), "dropped; nothing more is coming");
    assert!(c.write_incoming(a, Instant::now()));
    assert_eq!(c.board().data(0, &png()), None, "the tampered picture is not pasted");
    assert_eq!(c.board().data(0, &text()).unwrap(), b"hi");
}

/// A promise nobody answers gives up after its wait, and one whose client is gone gives up at
/// once.
#[test]
fn a_promise_whose_client_is_gone_answers_nothing_at_once() {
    let c = Arc::new(clip());
    let a = next_link();
    c.attach(a, sink().0);
    let picture = b"\x89PNG".to_vec();
    let offer = client_offer(1, 0, vec![vec![listed(ClipType::Format(ClipFormat::Png), &picture)]]);
    assert!(c.offered(a, offer, Instant::now()));
    assert!(c.mirror(a));
    let pasting = {
        let c = Arc::clone(&c);
        std::thread::spawn(move || c.board().paste(0, &png()))
    };
    let deadline = Instant::now().checked_add(Duration::from_secs(10)).unwrap();
    while !pasting.is_finished() && Instant::now() < deadline {
        c.forget(a);
        std::thread::yield_now();
    }
    assert_eq!(pasting.join().unwrap(), None);
}

/// A poll that reads the worker's own write before `write` has returned takes it for that
/// write, not for a copy to announce back to the clients.
#[test]
fn a_poll_inside_the_workers_own_write_announces_nothing() {
    let c = Arc::new(clip());
    let (a, watcher) = (next_link(), next_link());
    c.watch(watcher, true);
    let offer = client_offer(3, 0, vec![vec![inline(ClipFormat::Text, b"mine")]]);
    assert!(c.offered(a, offer, Instant::now()));
    let polled = Arc::new(Mutex::new(None));
    let hook = {
        let (weak, polled) = (Arc::downgrade(&c), Arc::clone(&polled));
        move || *polled.lock() = weak.upgrade().map(|c| c.poll(Instant::now()))
    };
    *c.board().during_write.lock() = Some(Box::new(hook));
    assert!(c.mirror(a));
    assert_eq!(*polled.lock(), Some(None), "polled mid-write, and nothing announced");
    assert_eq!(c.poll(Instant::now()), None, "nor after");
}

/// A poll inside the write of a client's promises, which carry no inline digest to match,
/// takes them for that write.
#[test]
fn a_poll_inside_a_write_of_promises_announces_nothing() {
    let c = Arc::new(clip());
    let (a, watcher) = (next_link(), next_link());
    c.watch(watcher, true);
    c.attach(a, sink().0);
    let offer = client_offer(
        4,
        0,
        vec![vec![listed(ClipType::Format(ClipFormat::Png), b"\x89PNG from the client")]],
    );
    assert!(c.offered(a, offer, Instant::now()));
    let polled = Arc::new(Mutex::new(None));
    let hook = {
        let (weak, polled) = (Arc::downgrade(&c), Arc::clone(&polled));
        move || *polled.lock() = weak.upgrade().map(|c| c.poll(Instant::now()))
    };
    *c.board().during_write.lock() = Some(Box::new(hook));
    assert!(c.mirror(a));
    assert_eq!(*polled.lock(), Some(None), "polled mid-write, and nothing announced");
    assert_eq!(c.poll(Instant::now()), None, "nor after");
    let items = c.board().items();
    assert!(items[0].iter().any(|t| t == AUTO_GENERATED_TYPE), "{items:?}");
}

/// Nothing a client sends that names a file goes on the pasteboard, however it is spelled: a
/// file URL as an Apple type, Finder's node, a link whose scheme is `file`, whether it came
/// inline or through a promise.
#[test]
fn a_clients_file_names_never_go_on_the_pasteboard() {
    let c = clip();
    let a = next_link();
    c.attach(a, sink().0);
    let apple = |s: &str| ClipType::Apple(s.to_owned());
    let link = |bytes: &[u8]| Rep {
        kind: apple("public.url"),
        size: Some(bytes.len() as u64),
        hash: Some(digest(bytes)),
        inline: Some(bytes.to_vec()),
    };
    let key = b"file:///Users/me/.ssh/id_ed25519";
    let offer = client_offer(
        5,
        0,
        vec![
            vec![
                inline(ClipFormat::Text, b"id_ed25519"),
                listed(apple("public.file-url"), key),
                listed(apple("com.apple.finder.node"), b"node"),
                link(key),
            ],
            vec![listed(apple("public.url"), b"file:///etc/passwd")],
            vec![link(b"https://example.com")],
        ],
    );
    assert!(c.offered(a, offer.clone(), Instant::now()));
    assert!(c.mirror(a));
    let items = c.board().items();
    assert_eq!(items.len(), 3, "{items:?}");
    for bad in ["public.file-url", "com.apple.finder.node"] {
        assert!(items.iter().flatten().all(|t| t != bad), "{bad}: {items:?}");
    }
    assert!(!items[0].iter().any(|t| t == "public.url"), "a file link inline: {items:?}");
    assert_eq!(c.board().data(2, "public.url").unwrap(), b"https://example.com");
    // The promised link turns out to name a file: the promise answers nothing.
    let promised = offer.rep_ref(1, apple("public.url"));
    c.supply(a, &promised, b"file:///etc/passwd".to_vec());
    assert_eq!(c.board().paste(1, "public.url"), None);
}

/// A promise waits on for as long as its bytes keep arriving, past the wait for them to start.
#[test]
fn a_promise_waits_on_while_the_bytes_keep_coming() {
    let wait = Duration::from_millis(200);
    let c = Arc::new(Clipboard::waiting(board(), Peer::Worker(WorkerId::new()), wait));
    let a = next_link();
    let (tell, sent) = sink();
    c.attach(a, tell);
    let picture = b"\x89PNG slow".to_vec();
    let offer = client_offer(1, 0, vec![vec![listed(ClipType::Format(ClipFormat::Png), &picture)]]);
    assert!(c.offered(a, offer.clone(), Instant::now()));
    assert!(c.mirror(a));
    let pasting = {
        let c = Arc::clone(&c);
        std::thread::spawn(move || c.board().paste(0, &png()))
    };
    while sent.lock().is_empty() {
        std::thread::yield_now();
    }
    let rep = offer.rep_ref(0, ClipType::Format(ClipFormat::Png));
    let started = Instant::now();
    let mut next = started;
    while started.elapsed() < wait * 4 {
        if Instant::now() >= next {
            c.receiving(&rep);
            next = Instant::now().checked_add(wait / 4).unwrap();
        }
        std::thread::yield_now();
    }
    assert!(!pasting.is_finished(), "still waiting while bytes come");
    c.supply(a, &rep, picture.clone());
    assert_eq!(pasting.join().unwrap(), Some(picture));
}

/// A client that copied files offers only their URLs. That offer takes the place of its
/// earlier one, whose text a paste would otherwise write over the staged files, and it
/// writes nothing itself: the files come as a transfer.
#[test]
fn a_clients_copied_files_replace_its_offer_and_write_nothing() {
    let c = clip();
    let a = next_link();
    let older = client_offer(1, 0, vec![vec![inline(ClipFormat::Text, b"older text")]]);
    assert!(c.offered(a, older, Instant::now()));
    assert!(c.mirror(a));
    assert!(c.write_files(&["/tmp/staged.txt"], Instant::now()));
    let files = client_offer(
        2,
        0,
        vec![vec![listed(ClipType::Format(ClipFormat::FileUrls), b"file:///Users/me/a.txt")]],
    );
    let later = Instant::now().checked_add(Duration::from_millis(5)).unwrap();
    if c.offered(a, files, later) {
        assert!(!c.mirror(a), "nothing of it can be written");
    }
    assert_eq!(c.paste(a, PasteKind::Window), Paste::Ready, "nothing to fetch");
    let url = board_type(ClipFormat::FileUrls);
    assert_eq!(
        c.board().data(0, &url).unwrap(),
        b"file:///tmp/staged.txt",
        "the staged files stay"
    );
    assert_eq!(c.board().data(0, &text()), None);
}

#[test]
fn staged_files_go_on_as_file_urls() {
    let c = clip();
    c.watch(next_link(), true);
    assert!(c.write_files(&["/tmp/a b.txt", "/tmp/ü"], Instant::now()));
    let url = board_type(ClipFormat::FileUrls);
    assert_eq!(c.board().data(0, &url).unwrap(), b"file:///tmp/a%20b.txt");
    assert_eq!(c.board().data(1, &url).unwrap(), b"file:///tmp/%C3%BC");
    assert_eq!(c.poll(Instant::now()), None, "the worker's own write");
}

/// A program's read gets what the worker holds as it already has it: a client's copy while that
/// client watches, the worker's own once a poll read it while anyone does, never a secret and
/// never more than asked for.
#[test]
fn a_program_reads_the_shared_text_only_while_it_is_shared() {
    let c = clip();
    let (a, b) = (next_link(), next_link());
    c.attach(a, sink().0);
    c.attach(b, sink().0);
    let copy = client_offer(1, 0, vec![vec![inline(ClipFormat::Text, b"client copy")]]);
    assert!(c.offered(a, copy, Instant::now()));
    assert!(c.mirror(a));
    assert_eq!(c.shared_text_within(1024), None, "nobody shares");
    assert!(!ForSessions::shares(&c), "so the attributes list no clipboard");
    c.watch(b, true);
    assert!(ForSessions::shares(&c), "b shares");
    assert_eq!(c.shared_text_within(1024), None, "the copy is a's, and a does not share");
    c.watch(a, true);
    assert_eq!(c.shared_text_within(1024).as_deref(), Some("client copy"));
    assert_eq!(c.shared_text_within(4), None, "past the bound");

    c.board().copy(&[(&text(), b"worker copy")]);
    assert!(c.poll(Instant::now()).is_some());
    c.watch(a, false);
    assert_eq!(c.shared_text_within(1024).as_deref(), Some("worker copy"), "b still shares");
    c.watch(b, false);
    assert_eq!(c.shared_text_within(1024), None);

    c.watch(a, true);
    let mut secret = client_offer(2, 0, vec![vec![inline(ClipFormat::Text, b"hunter2")]]);
    secret.concealed = true;
    let _mirrored = c.offered(a, secret, Instant::now());
    assert_eq!(c.paste(a, PasteKind::Window), Paste::Ready);
    assert_eq!(c.board().data(0, &text()).unwrap(), b"hunter2");
    assert_eq!(c.shared_text_within(1024), None, "a secret is pasted, never read");
}

/// Text the pasteboard only promises is not fetched for a read: the read answers nothing at
/// once, and the client is never asked.
#[test]
fn a_read_of_promised_text_never_waits_or_fetches() {
    let c = Clipboard::waiting(board(), Peer::Worker(WorkerId::new()), Duration::from_secs(60));
    let a = next_link();
    let (to_a, sent) = sink();
    c.attach(a, to_a);
    c.watch(a, true);
    let big = vec![b'x'; 100];
    let offer = client_offer(1, 0, vec![vec![listed(ClipType::Format(ClipFormat::Text), &big)]]);
    assert!(c.offered(a, offer, Instant::now()));
    assert!(c.mirror(a));
    let asked = Instant::now();
    assert_eq!(c.shared_text_within(1024), None);
    assert!(asked.elapsed() < Duration::from_secs(1));
    assert!(sent.lock().is_empty(), "{:?}", sent.lock());
}
