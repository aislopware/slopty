use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use parking_lot::Mutex;
use slopty_core::{WorkerId, XferId};
use slopty_platform::pasteboard::{FILE_URL_UTI, Memory, TEXT_UTI};
use slopty_proto::transfer::{Dest, parse_origin};

use super::*;
use crate::xfer::XferError;

fn worker_offer(generation: u64, items: Vec<Vec<Rep>>) -> Offer {
    Offer {
        origin: Peer::Worker(WorkerId::new()),
        generation,
        age_ms: 0,
        concealed: false,
        items: items.into_iter().map(|reps| ClipEntry { reps }).collect(),
    }
}

fn text_rep(text: &str) -> Rep {
    Rep {
        kind: ClipType::Format(ClipFormat::Text),
        size: Some(text.len() as u64),
        hash: Some(digest(text.as_bytes())),
        inline: Some(text.as_bytes().to_vec()),
    }
}

fn lazy(kind: ClipType) -> Rep {
    Rep { kind, size: None, hash: None, inline: None }
}

fn setup() -> (Rc<Memory>, ClipSync, ClientId, WorkerKey) {
    let board = Rc::new(Memory::default());
    let shared: Rc<dyn Pasteboard> = Rc::<Memory>::clone(&board);
    (board, ClipSync::new(shared), ClientId::new(), WorkerKey::new(1))
}

fn noop() -> Provide {
    Arc::new(|_, _| None)
}

/// A worker's link, as far as clipboard sync uses it: fetches answered from `bytes`, and every
/// answer this client sends recorded.
#[derive(Debug, Default)]
struct FakeRemote {
    bytes: Mutex<HashMap<RepRef, Vec<u8>>>,
    fetched: Mutex<Vec<(RepRef, Option<u64>)>>,
    sent: Mutex<Vec<(RepRef, Fetched, bool)>>,
}

impl Remote for FakeRemote {
    fn upload(&self, _xfer: XferId, _files: Vec<PathBuf>, _dest: Dest) {}

    fn cancel(&self, _xfer: XferId) {}

    fn download(
        &self,
        _path: String,
        _into: PathBuf,
        _shown_at: Option<PathBuf>,
    ) -> Result<Vec<PathBuf>, XferError> {
        Ok(Vec::new())
    }

    fn clip_fetch(&self, rep: &RepRef, max: Option<u64>, _wait: Duration) -> Fetched {
        self.fetched.lock().push((rep.clone(), max));
        self.bytes.lock().get(rep).cloned().map_or(Fetched::Gone, Fetched::Data)
    }

    fn send_clip(&self, rep: RepRef, answer: Fetched, urgent: bool) {
        self.sent.lock().push((rep, answer, urgent));
    }

    fn forward(&self, _port: u16) -> Option<u16> {
        None
    }
}

#[test]
fn a_copy_here_is_announced_once_per_worker_with_small_text_inline() {
    let (board, mut sync, me, studio) = setup();
    assert!(sync.offer_for(studio, me).is_none(), "an empty clipboard says nothing");
    let png = vec![9_u8; INLINE_CLIP_BYTES + 1];
    board.copy(&[
        ("public.png", &png),
        (TEXT_UTI, b"hello"),
        ("com.apple.webarchive", b"archive"),
        ("dyn.ah62d4rv4gk8zuxnykk", b"dynamic"),
    ]);
    let offer = sync.offer_for(studio, me).unwrap();
    let kinds: Vec<&ClipType> = offer.reps().map(|(_, r)| &r.kind).collect();
    assert_eq!(
        kinds,
        [
            &ClipType::Format(ClipFormat::Png),
            &ClipType::Format(ClipFormat::Text),
            &ClipType::Apple("com.apple.webarchive".to_owned()),
        ],
        "every type in the copying app's order, a dynamic one left behind"
    );
    let reps: Vec<&Rep> = offer.reps().map(|(_, r)| r).collect();
    assert_eq!(reps[1].inline.as_deref(), Some(&b"hello"[..]));
    assert_eq!((reps[0].size, reps[0].inline.as_ref()), (None, None), "a picture is lazy");
    assert_eq!(board.reads(), 1, "only the text was read");
    assert!(sync.offer_for(studio, me).is_none(), "told already");
    assert!(sync.offer_for(WorkerKey::new(2), me).is_some(), "the other worker was not");
    let picture = offer.rep_ref(0, ClipType::Format(ClipFormat::Png));
    assert_eq!(sync.answer(&picture, None), Answer::Here(Fetched::Data(png.clone())));
    let cap = Some(1024);
    assert_eq!(sync.answer(&picture, cap), Answer::Here(Fetched::TooBig(png.len() as u64)));
    board.copy(&[(TEXT_UTI, b"newer")]);
    assert_eq!(sync.answer(&picture, None), Answer::Here(Fetched::Gone), "the clipboard moved on");
}

/// Every item of a copy is offered as its own item: a Photos copy of two pictures, a Finder
/// copy of files.
#[test]
fn a_multi_item_copy_keeps_its_items() {
    let (board, mut sync, me, studio) = setup();
    board.copy_items(&[&[("public.jpeg", b"one")], &[("public.heic", b"two"), (TEXT_UTI, b"b")]]);
    let offer = sync.offer_for(studio, me).unwrap();
    assert_eq!(offer.items.len(), 2);
    let second = offer.rep_ref(1, ClipType::Apple("public.heic".to_owned()));
    assert_eq!(sync.answer(&second, None), Answer::Here(Fetched::Data(b"two".to_vec())));
}

/// Another app's copy clears the pasteboard, then puts its contents on under the count
/// the clear left. A read in between finds it empty; the contents are offered once there.
#[test]
fn contents_put_on_after_a_read_saw_the_board_cleared_are_offered() {
    let (board, mut sync, me, studio) = setup();
    board.clear();
    assert!(sync.offer_for(studio, me).is_none(), "nothing on it yet");
    board.put(&[(TEXT_UTI, b"landed")]);
    let offer = sync.offer_for(studio, me).expect("the contents under the clear's count");
    assert_eq!(offer.items[0].reps[0].inline.as_deref(), Some(&b"landed"[..]));
}

/// A copy's age is the time since its change count was first seen, so the worker can tell
/// whose copy is the latest.
#[test]
fn an_offer_says_how_long_ago_the_copy_was_made() {
    let (board, mut sync, me, studio) = setup();
    board.copy(&[(TEXT_UTI, b"earlier")]);
    let then = Instant::now().checked_sub(Duration::from_secs(5)).unwrap();
    sync.observe(then);
    sync.observe(Instant::now());
    let offer = sync.offer_for(studio, me).unwrap();
    assert!(offer.age_ms >= 5_000, "{}", offer.age_ms);
}

#[test]
fn a_worker_offer_lands_as_text_and_promises_and_is_never_announced_back() {
    let (board, mut sync, me, studio) = setup();
    let asked = Arc::new(AtomicUsize::new(0));
    let log = Arc::clone(&asked);
    let provide: Provide = Arc::new(move |item, uti: &str| {
        log.fetch_add(1, Ordering::Relaxed);
        (item == 0 && uti == "public.png").then(|| vec![1, 2, 3, 4])
    });
    let offer = worker_offer(
        7,
        vec![
            vec![lazy(ClipType::Format(ClipFormat::Png)), text_rep("from the worker")],
            vec![lazy(ClipType::Apple("com.adobe.pdf".to_owned()))],
        ],
    );
    sync.receive(studio, &offer, provide, None);
    assert_eq!(board.data(TEXT_UTI).as_deref(), Some(&b"from the worker"[..]));
    assert_eq!(board.promised(), ["public.png"]);
    assert_eq!(board.items()[1], ["com.adobe.pdf"], "the second item");
    assert_eq!(asked.load(Ordering::Relaxed), 0, "fetched only on paste");
    assert_eq!(board.data("public.png"), Some(vec![1, 2, 3, 4]));
    assert_eq!(asked.load(Ordering::Relaxed), 1);
    let stamp = board.data(ORIGIN_TYPE).unwrap();
    let (peer, generation) = parse_origin(&stamp).unwrap();
    assert!(matches!(peer, Peer::Worker(_)) && generation == 7, "{peer:?} {generation}");
    assert!(sync.offer_for(studio, me).is_none(), "not offered back to the worker it came from");
}

#[test]
fn the_same_contents_coming_back_unstamped_are_not_echoed() {
    let (board, mut sync, me, studio) = setup();
    sync.receive(studio, &worker_offer(1, vec![vec![text_rep("ping")]]), noop(), None);
    // Universal Clipboard delivers the worker's copy again, without Slopty's stamp.
    board.copy(&[(TEXT_UTI, b"ping")]);
    assert!(sync.offer_for(studio, me).is_none(), "same digest: an echo");
    board.copy(&[(TEXT_UTI, b"pong")]);
    assert!(sync.offer_for(studio, me).is_some(), "new contents are announced");
}

/// A worker's copy (G1): written here, it is offered as it is to the other workers, origin
/// kept, and their fetches are answered by fetching from the worker it came from, through that
/// worker's link.
#[test]
fn a_worker_offer_is_relayed_to_another_worker_and_fetched_through_the_first() {
    let (_board, mut sync, me, studio) = setup();
    let laptop = WorkerKey::new(2);
    let offer = worker_offer(
        4,
        vec![vec![text_rep("copied on the studio"), lazy(ClipType::Format(ClipFormat::Png))]],
    );
    sync.receive(studio, &offer, noop(), None);
    let relayed = sync.offer_for(laptop, me).expect("offered to the other worker");
    assert_eq!((relayed.origin, relayed.generation), (offer.origin, 4), "origin kept");
    assert_eq!(relayed.items, offer.items);
    assert!(sync.offer_for(laptop, me).is_none(), "told once");
    assert!(sync.focus_offer(WorkerKey::new(3), me).is_some(), "no read needed to relay");

    let picture = offer.rep_ref(0, ClipType::Format(ClipFormat::Png));
    assert_eq!(sync.answer(&picture, None), Answer::From(studio));

    // The relay itself, as the UI runs it: fetch from the studio, answer the laptop.
    let (from, to) = (Arc::new(FakeRemote::default()), Arc::new(FakeRemote::default()));
    from.bytes.lock().insert(picture.clone(), b"\x89PNG".to_vec());
    relay(&*from, &*to, picture.clone(), Some(1 << 20), true, Duration::from_secs(1));
    assert_eq!(from.fetched.lock().as_slice(), [(picture.clone(), Some(1 << 20))]);
    assert_eq!(to.sent.lock().as_slice(), [(picture, Fetched::Data(b"\x89PNG".to_vec()), true)]);

    // A picture relayed this way pastes into the laptop's shell ahead of the chord; into the
    // studio's own shell it is there already.
    let photo = worker_offer(5, vec![vec![lazy(ClipType::Format(ClipFormat::Png))]]);
    sync.receive(studio, &photo, noop(), None);
    let ShellPaste::Picture { offer: Some(ahead) } = sync.shell_paste(laptop, me) else {
        panic!("a picture from another worker goes ahead of the chord")
    };
    assert_eq!(ahead.source(), photo.source());
    assert_eq!(sync.shell_paste(studio, me), ShellPaste::Text);
}

/// Where reading the clipboard asks first, taking a tile's focus reads nothing and offers
/// nothing; the paste the person makes reads it and offers it.
#[test]
fn a_focus_read_waits_for_a_paste_when_reads_ask() {
    let board = Rc::new(Memory::asking());
    let shared: Rc<dyn Pasteboard> = Rc::<Memory>::clone(&board);
    let (mut sync, me, studio) = (ClipSync::new(shared), ClientId::new(), WorkerKey::new(1));
    board.copy(&[(TEXT_UTI, b"typed here")]);
    assert!(sync.focus_offer(studio, me).is_none());
    assert_eq!(board.reads(), 0, "not read on focus");
    let offer = sync.paste_offer(studio, me).expect("a paste reads it");
    assert_eq!(offer.items[0].reps[0].inline.as_deref(), Some(&b"typed here"[..]));
    assert_eq!(board.reads(), 1);

    board.copy(&[(TEXT_UTI, b"copied while focused")]);
    let now = Instant::now();
    assert!(sync.tick(now), "the clipboard moved while the tile has the keyboard");
    assert!(sync.focus_offer(studio, me).is_none(), "still not read");
    let later = now.checked_add(Duration::from_secs(1)).unwrap();
    assert!(!sync.tick(later), "left unread, not asked again");
    board.set_asks(false);
    board.copy(&[(TEXT_UTI, b"allowed")]);
    assert!(sync.tick(later.checked_add(Duration::from_secs(1)).unwrap()));
    assert!(sync.focus_offer(studio, me).is_some(), "reads are free now");
    assert!(!sync.tick(later.checked_add(Duration::from_secs(2)).unwrap()), "read already");
}

/// A password manager's copy is offered by type alone: nothing read, nothing inline, and read
/// only when a worker's paste fetches it.
#[test]
fn a_concealed_copy_is_offered_without_bytes() {
    let (board, mut sync, me, studio) = setup();
    board.copy(&[(TEXT_UTI, b"hunter2"), (CONCEALED_UTI, b"")]);
    let offer = sync.offer_for(studio, me).expect("offered, as a secret");
    assert!(offer.concealed);
    assert_eq!(offer.reps().count(), 1, "the marker is not a representation");
    let rep = &offer.items[0].reps[0];
    assert_eq!((rep.size, rep.hash, rep.inline.as_ref()), (None, None, None));
    assert_eq!(board.reads(), 0, "not read");
    let text = offer.rep_ref(0, ClipType::Format(ClipFormat::Text));
    assert_eq!(sync.answer(&text, None), Answer::Here(Fetched::Data(b"hunter2".to_vec())));

    board.copy(&[(TEXT_UTI, b"otp"), (TRANSIENT_UTI, b"")]);
    assert!(sync.offer_for(studio, me).unwrap().concealed);

    // A worker's secret lands here marked, so no clipboard manager keeps it.
    let secret = Offer {
        concealed: true,
        ..worker_offer(2, vec![vec![lazy(ClipType::Format(ClipFormat::Text))]])
    };
    sync.receive(studio, &secret, noop(), None);
    assert!(board.types().iter().any(|t| t == CONCEALED_UTI), "{:?}", board.types());
}

/// Files copied here are offered as their URLs, one item each, and named for a paste; a
/// worker's copied files are named by where they are, while its write is what the clipboard
/// holds.
#[test]
fn copied_files_are_offered_as_urls_and_named_for_a_paste() {
    let (board, mut sync, me, studio) = setup();
    board.copy(&[(TEXT_UTI, b"older text")]);
    assert!(sync.offer_for(studio, me).is_some());
    board.copy_files(&["file:///Users/me/a%20b.txt", "file:///tmp/c"]);
    let offer = sync.offer_for(studio, me).expect("the files replace the text");
    let kinds: Vec<&ClipType> = offer.reps().map(|(_, r)| &r.kind).collect();
    assert_eq!(kinds, [&ClipType::Format(ClipFormat::FileUrls); 2]);
    assert_eq!(offer.items[1].reps[0].inline.as_deref(), Some(&b"file:///tmp/c"[..]));
    let here = vec![PathBuf::from("/Users/me/a b.txt"), PathBuf::from("/tmp/c")];
    assert_eq!(sync.files(), Some(ClipFiles::Here(here)));

    let url = |n: u8| Rep {
        kind: ClipType::Format(ClipFormat::FileUrls),
        size: Some(9),
        hash: Some([n; 32]),
        inline: None,
    };
    let theirs = worker_offer(4, vec![vec![url(1), text_rep("a.txt")], vec![url(2)]]);
    sync.receive(studio, &theirs, noop(), None);
    let urls = vec![
        theirs.rep_ref(0, ClipType::Format(ClipFormat::FileUrls)),
        theirs.rep_ref(1, ClipType::Format(ClipFormat::FileUrls)),
    ];
    assert_eq!(sync.files(), Some(ClipFiles::Worker { worker: studio, urls }));
    assert_eq!(board.item_data(0, TEXT_UTI).as_deref(), Some(&b"a.txt"[..]), "the names as text");
    assert!(
        board.items().iter().flatten().all(|t| t != FILE_URL_UTI),
        "the worker's paths are not put here: {:?}",
        board.items()
    );
    board.copy(&[(TEXT_UTI, b"moved on")]);
    assert_eq!(sync.files(), None);
}

/// Nothing a worker sends that names a file goes on this pasteboard, however it is spelled:
/// a file URL as an Apple type, Finder's node, a link whose scheme is `file`, inline or
/// promised. An offer of nothing else still goes on, as its origin alone.
#[test]
fn a_workers_file_names_never_go_on_this_pasteboard() {
    let (board, mut sync, _me, studio) = setup();
    let apple = |s: &str| ClipType::Apple(s.to_owned());
    let inline = |kind: ClipType, bytes: &[u8]| Rep {
        kind,
        size: Some(bytes.len() as u64),
        hash: Some(digest(bytes)),
        inline: Some(bytes.to_vec()),
    };
    let key = b"file:///Users/me/.ssh/id_ed25519";
    let theirs = worker_offer(
        6,
        vec![
            vec![
                text_rep("id_ed25519"),
                inline(apple("public.file-url"), key),
                lazy(apple("com.apple.finder.node")),
                inline(apple("public.url"), key),
            ],
            vec![lazy(apple("public.url"))],
        ],
    );
    let link = FakeRemote::default();
    let promised = theirs.rep_ref(1, apple("public.url"));
    link.bytes.lock().insert(promised, b"file:///etc/passwd".to_vec());
    let link: Arc<dyn Remote> = Arc::new(link);
    let (_tx, now) = watch::channel(Some(link));
    sync.receive(studio, &theirs, provider(now, &theirs, Duration::from_secs(1)), None);
    let types: Vec<String> = board.items().into_iter().flatten().collect();
    for bad in [FILE_URL_UTI, "com.apple.finder.node"] {
        assert!(types.iter().all(|t| t != bad), "{bad}: {types:?}");
    }
    assert_eq!(board.item_data(0, "public.url"), None, "a file link inline is not written");
    assert_eq!(board.item_data(1, "public.url"), None, "nor one that a promise turns up");

    let only_files = worker_offer(7, vec![vec![lazy(ClipType::Format(ClipFormat::FileUrls))]]);
    sync.receive(studio, &only_files, noop(), None);
    assert_eq!(board.items().len(), 1, "one item, for the origin");
    assert!(board.types().iter().any(|t| t == ORIGIN_TYPE));
    assert!(matches!(sync.files(), Some(ClipFiles::Worker { .. })), "named for a paste");
}

/// A worker's fetch reads this pasteboard capped: at its own cap, else at the most a client
/// takes, so a copy past it answers `TooBig` without being kept.
#[test]
fn a_workers_fetch_reads_no_more_than_its_cap() {
    #[derive(Default)]
    struct Capping {
        board: Memory,
        caps: std::cell::RefCell<Vec<u64>>,
    }
    impl Pasteboard for Capping {
        fn change_count(&self) -> i64 {
            self.board.change_count()
        }

        fn items(&self) -> Vec<Vec<String>> {
            self.board.items()
        }

        fn item_data(&self, item: usize, uti: &str) -> Option<Vec<u8>> {
            self.board.item_data(item, uti)
        }

        fn item_data_within(&self, item: usize, uti: &str, max: u64) -> Option<Capped> {
            self.caps.borrow_mut().push(max);
            self.board.item_data_within(item, uti, max)
        }

        fn write(&self, write: Write) -> i64 {
            self.board.write(write)
        }
    }
    let board = Rc::new(Capping::default());
    let mut sync = ClipSync::new(Rc::<Capping>::clone(&board));
    let (me, studio) = (ClientId::new(), WorkerKey::new(1));
    board.board.copy(&[("public.png", &[7; 64])]);
    let offer = sync.offer_for(studio, me).unwrap();
    let png = offer.rep_ref(0, ClipType::Format(ClipFormat::Png));
    assert_eq!(sync.answer(&png, None), Answer::Here(Fetched::Data(vec![7; 64])));
    assert_eq!(sync.answer(&png, Some(10)), Answer::Here(Fetched::TooBig(64)));
    assert_eq!(*board.caps.borrow(), [MAX_CLIP_BYTES, 10]);
}

/// Text past what rides inline is measured, not kept: a worker's fetch reads it again.
#[test]
fn long_text_is_measured_and_not_kept() {
    let (board, mut sync, me, studio) = setup();
    let long = vec![b'a'; INLINE_CLIP_BYTES + 1];
    board.copy(&[(TEXT_UTI, &long)]);
    let offer = sync.offer_for(studio, me).unwrap();
    let rep = &offer.items[0].reps[0];
    assert_eq!((rep.size, rep.hash, rep.inline.as_ref()), (Some(long.len() as u64), None, None));
    let text = offer.rep_ref(0, ClipType::Format(ClipFormat::Text));
    assert_eq!(sync.answer(&text, None), Answer::Here(Fetched::Data(long)));
    assert_eq!(board.reads(), 2, "read when announced, and again for the fetch");
}

/// The app keeps its client id across launches, so a relaunch's offers start at a new
/// generation.
#[test]
fn a_relaunched_clients_first_offer_is_a_new_generation() {
    let first_offer = || {
        let (board, mut sync, _me, studio) = setup();
        board.copy(&[(TEXT_UTI, b"after a launch")]);
        sync.offer_for(studio, ClientId::nil()).unwrap().generation
    };
    assert_ne!(first_offer(), first_offer());
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

/// A worker's copied files go on this pasteboard as their URLs in the worker's place here
/// (its File Provider domain), escaped as Finder writes them, a folder still a folder: a paste
/// in Finder takes them from there. One outside the worker's home, which the place does not
/// hold, goes on as nothing, and a paste into a worker still moves them all.
#[test]
fn a_workers_files_go_on_as_their_place_here() {
    let (board, mut sync, _me, studio) = setup();
    let place = Place {
        home: "/Users/dev".to_owned(),
        root: PathBuf::from("/Users/me/Library/CloudStorage/Slopty-studio"),
    };
    let url = |text: &str| Rep {
        kind: ClipType::Format(ClipFormat::FileUrls),
        size: Some(text.len() as u64),
        hash: Some(digest(text.as_bytes())),
        inline: Some(text.as_bytes().to_vec()),
    };
    let theirs = worker_offer(
        3,
        vec![
            vec![text_rep("a b.txt"), url("file:///Users/dev/src/a%20b.txt")],
            vec![url("file:///Users/dev/proj/")],
            vec![text_rep("hosts"), url("file:///etc/hosts")],
        ],
    );
    sync.receive(studio, &theirs, noop(), Some(&place));
    let at = |item: usize| board.item_data(item, FILE_URL_UTI).map(String::from_utf8);
    assert_eq!(
        at(0),
        Some(Ok("file:///Users/me/Library/CloudStorage/Slopty-studio/src/a%20b.txt".to_owned()))
    );
    assert_eq!(
        at(1),
        Some(Ok("file:///Users/me/Library/CloudStorage/Slopty-studio/proj/".to_owned()))
    );
    assert_eq!(at(2), None, "outside the home: not in the place");
    assert_eq!(board.item_data(2, TEXT_UTI), Some(b"hosts".to_vec()), "its name still goes on");
    assert!(board.types().iter().any(|t| t == ORIGIN_TYPE), "never announced back");
    let Some(ClipFiles::Worker { worker, urls }) = sync.files() else { panic!("worker files") };
    assert_eq!((worker, urls.len()), (studio, 3), "a paste into a worker moves them all");
    let back = file_url_paths(&place.urls_of(b"file:///Users/dev/%C3%BC/x%23y").unwrap());
    assert_eq!(back, [place.root.join("ü/x#y")], "escaped and read back whole");
    assert_eq!(place.urls_of(b"file:///Users/dev/../root/x"), None, "a path that climbs");
}
