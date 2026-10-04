//! The clipboard, files and ports in the headless workspace: what is sent to the worker when,
//! and what the tile shows.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::{ExternalPaths, FileDropEvent};
use slopty_client::clip::Fetched;
use slopty_client::remote::Remote;
use slopty_client::tunnel::Forward;
use slopty_client::xfer::XferError;
use slopty_core::XferId;
use slopty_platform::pasteboard::{Memory, Pasteboard, TEXT_UTI};
use slopty_platform::web::WebEvent;
use slopty_proto::orchestration::Port;
use slopty_proto::transfer::{
    ClipEntry, ClipFormat, ClipMsg, ClipType, Dest, Offer, Peer, Rep, RepRef, TunnelRefusal,
    XferMsg,
};

use super::*;
use crate::conversation::attach::Attachment;

/// What the workspace asked of a worker's [`Remote`].
#[derive(Debug)]
enum Call {
    Upload(XferId, Vec<PathBuf>, Dest),
    /// An upload an earlier run began, taken up again.
    UploadAgain(XferId, Vec<PathBuf>, Dest),
    Cancel(XferId),
    SendClip(RepRef, Fetched, bool),
    Forward(u16),
    Proxy,
    #[cfg_attr(not(target_os = "macos"), expect(dead_code, reason = "a drag out is the Mac's"))]
    WatchDragOut(slopty_proto::drag::DragId),
}

/// Records the calls; serves a worker port here `offset` ports up, as a client whose ports
/// are partly taken would. Its copied files are [`WORKER_FILES`], one item each, its pictures
/// `PNG`, and a download of one writes a file of that name whose text is the path.
#[derive(Debug)]
struct Recorder(mpsc::UnboundedSender<Call>, u16);

/// Where a [`Recorder`] serves its worker's network.
const PROXY: u16 = 1080;

/// The `public.file-url` of each file a worker copied.
const WORKER_FILES: [&[u8]; 2] = [b"file:///Users/w/a%20b.txt", b"file:///Users/w/c.txt"];

impl Remote for Recorder {
    fn upload(&self, xfer: XferId, files: Vec<PathBuf>, dest: Dest, again: bool) {
        let call = if again { Call::UploadAgain } else { Call::Upload };
        self.0.send(call(xfer, files, dest)).unwrap();
    }

    fn cancel(&self, xfer: XferId) {
        self.0.send(Call::Cancel(xfer)).unwrap();
    }

    fn download(&self, ask: slopty_client::xfer::Download) -> Result<Vec<PathBuf>, XferError> {
        let (path, into) = (ask.path, ask.into);
        let name =
            path.rsplit('/').next().ok_or_else(|| XferError::Worker("no name".to_owned()))?;
        let file = into.join(name);
        std::fs::write(&file, &path).map_err(|e| XferError::Worker(e.to_string()))?;
        Ok(vec![file])
    }

    fn clip_fetch(&self, rep: &RepRef, _max: Option<u64>, _wait: Duration) -> Fetched {
        match rep.kind {
            ClipType::Format(ClipFormat::Png) => Fetched::Data(b"PNG".to_vec()),
            ClipType::Format(ClipFormat::FileUrls) => WORKER_FILES
                .get(usize::from(rep.item))
                .map_or(Fetched::Gone, |url| Fetched::Data(url.to_vec())),
            _ => Fetched::Gone,
        }
    }

    fn send_clip(&self, rep: RepRef, answer: Fetched, urgent: bool) {
        self.0.send(Call::SendClip(rep, answer, urgent)).unwrap();
    }

    fn forward(&self, port: u16) -> Option<u16> {
        self.0.send(Call::Forward(port)).unwrap();
        port.checked_add(self.1)
    }

    fn proxy(&self) -> Option<u16> {
        self.0.send(Call::Proxy).unwrap();
        Some(PROXY)
    }

    fn refusal(&self, host: &str, _port: u16) -> Option<TunnelRefusal> {
        (host == "nowhere.internal").then_some(TunnelRefusal::Unresolved)
    }

    fn watch_drag_out(&self, shared: &Arc<slopty_client::dnd::out::Shared>) {
        self.0.send(Call::WatchDragOut(shared.drag())).unwrap();
    }
}

/// A worker connected with a recording [`Remote`], and a pasteboard in memory.
fn connect_remote(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
) -> (Fake, mpsc::UnboundedReceiver<Call>, Rc<Memory>) {
    connect_remote_at(view, cx, 0)
}

/// [`connect_remote`], the worker's ports served here `offset` ports up.
fn connect_remote_at(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    offset: u16,
) -> (Fake, mpsc::UnboundedReceiver<Call>, Rc<Memory>) {
    let board = Rc::new(Memory::default());
    let shared: Rc<dyn Pasteboard> = Rc::<Memory>::clone(&board);
    view.update_in(cx, |v, _window, _cx| v.set_pasteboard(shared));
    let (fake, recorded) = link_remote(view, cx, 7, "studio", offset);
    (fake, recorded, board)
}

/// Worker `name` under key `seed` connected with a recording [`Remote`].
fn link_remote(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    seed: u128,
    name: &str,
    offset: u16,
) -> (Fake, mpsc::UnboundedReceiver<Call>) {
    let (tx, rx) = mpsc::channel(256);
    let (calls, recorded) = mpsc::unbounded_channel();
    let me = ClientId::new();
    let key = WorkerKey::new(seed);
    let factory: ScreenFactory =
        Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
    view.update_in(cx, |v, _window, cx| {
        v.add_worker(key, name.to_owned(), cx);
        let remote: Arc<dyn Remote> = Arc::new(Recorder(calls, offset));
        let link = WorkerLink { me, out: tx, open_screen: factory, remote: Some(remote) };
        v.connect_worker(key, link, hello(name, Vec::new()), cx);
        v.apply_sync(key, ItemSync::Snapshot { version: 0, items: Vec::new() }, cx);
    });
    cx.run_until_parked();
    let mut fake = Fake { key, me, rx };
    fake.drain();
    (fake, recorded)
}

/// What was pasted into `shell`.
fn pasted(sent: Vec<ClientMsg>, shell: SessionId) -> Vec<String> {
    sent.into_iter()
        .filter_map(|m| match m {
            ClientMsg::Term { session, req: TermRequest::Paste { text, .. } }
                if session == shell =>
            {
                Some(text)
            }
            _ => None,
        })
        .collect()
}

/// ⌘V in `shell`'s terminal.
fn paste_in(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, shell: SessionId) {
    view.update_in(cx, |v, window, cx| {
        let terminal = v.terminal(shell).expect("attached").clone();
        terminal.update(cx, |t, cx| t.paste_clipboard(&crate::terminal::Paste, window, cx));
    });
    cx.run_until_parked();
}

/// The offer of worker `generation`'s copied files, announced to this client.
fn worker_copied_files(generation: u64) -> Offer {
    let item = |url: &[u8]| ClipEntry {
        reps: vec![Rep {
            kind: ClipType::Format(ClipFormat::FileUrls),
            size: Some(url.len() as u64),
            hash: Some(crate::clipboard::digest(url)),
            inline: None,
        }],
    };
    worker_offer(generation, WORKER_FILES.iter().map(|url| item(url)).collect())
}

/// Worker offer `generation` of `items`.
fn worker_offer(generation: u64, items: Vec<ClipEntry>) -> Offer {
    Offer {
        origin: Peer::Worker(slopty_core::WorkerId::new()),
        generation,
        age_ms: 0,
        concealed: false,
        items,
    }
}

/// One item of `reps`.
fn entry(reps: Vec<Rep>) -> ClipEntry {
    ClipEntry { reps }
}

/// A representation listed by its digest, fetched when pasted.
fn listed(format: ClipFormat, bytes: &[u8]) -> Rep {
    Rep {
        kind: ClipType::Format(format),
        size: Some(bytes.len() as u64),
        hash: Some(crate::clipboard::digest(bytes)),
        inline: None,
    }
}

/// ⌘V in a shell with files copied here sends them to the shell's directory and types their
/// paths once there, as a drop does; the worker hears the files' URLs as the clipboard.
#[gpui::test]
fn files_copied_here_paste_into_a_shell_as_a_drop(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (mut studio, mut calls, board) = connect_remote(&view, cx);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a b.txt");
    std::fs::write(&file, b"abc").unwrap();
    let url = format!("file://{}", dir.path().join("a%20b.txt").display());
    board.copy_files(&[&url]);
    let shell = SessionId::new();
    opens(&view, cx, &studio, shell, studio.me, 1);
    let offer = offers(&studio.drain()).pop().expect("announced on focus");
    assert_eq!(offer.items.len(), 1, "one item a file");
    assert_eq!(
        offer.items[0].reps[0].kind,
        ClipType::Format(ClipFormat::FileUrls),
        "the URLs alone"
    );

    paste_in(&view, cx, shell);
    let Call::Upload(xfer, files, dest) = calls.try_recv().expect("an upload") else {
        panic!("an upload")
    };
    assert_eq!((files, dest), (vec![file], Dest::SessionCwd(shell)));
    assert!(pasted(studio.drain(), shell).is_empty(), "nothing typed yet");
    let paths = vec!["/Users/w/proj/a b.txt".to_owned()];
    view.update_in(cx, |v, _window, cx| v.xfer_message(XferMsg::Finished { xfer, paths }, cx));
    cx.run_until_parked();
    assert_eq!(pasted(studio.drain(), shell), ["'/Users/w/proj/a b.txt' "]);
}

/// Files a worker copied, pasted into a shell of that worker, are typed where they are; into
/// a shell of another worker, they come down here and go up to it, and what came down goes.
#[gpui::test]
fn files_a_worker_copied_paste_into_its_shell_or_travel_to_another(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (mut studio, mut studio_calls, _board) = connect_remote(&view, cx);
    let (mut laptop, mut laptop_calls) = link_remote(&view, cx, 8, "laptop", 0);
    let key = studio.key;
    view.update_in(cx, |v, _window, cx| {
        v.clip_message(key, ClipMsg::Offer(worker_copied_files(5)), cx);
    });
    cx.run_until_parked();
    // A test build has no Finder location for a worker: said once, the first time.
    #[cfg(target_os = "macos")]
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some(
            "Files copied on studio paste into shells, not Finder: studio has no location in \
             Finder here"
        )
    );
    let here = SessionId::new();
    opens(&view, cx, &studio, here, studio.me, 1);
    studio.drain();
    paste_in(&view, cx, here);
    assert_eq!(pasted(studio.drain(), here), ["'/Users/w/a b.txt' /Users/w/c.txt "]);
    assert!(studio_calls.try_recv().is_err(), "nothing moved");

    let there = SessionId::new();
    opens(&view, cx, &laptop, there, laptop.me, 1);
    laptop.drain();
    paste_in(&view, cx, there);
    let Call::Upload(xfer, files, dest) = laptop_calls.try_recv().expect("an upload") else {
        panic!("an upload")
    };
    assert_eq!(dest, Dest::SessionCwd(there));
    let names: Vec<_> = files.iter().map(|f| f.file_name().unwrap().to_owned()).collect();
    assert_eq!(names, ["a b.txt", "c.txt"], "in the order copied");
    assert_eq!(std::fs::read_to_string(&files[0]).unwrap(), "/Users/w/a b.txt", "brought down");
    let scratch = files[0].parent().unwrap().parent().unwrap().to_owned();
    let paths = vec!["/Users/l/a b.txt".to_owned(), "/Users/l/c.txt".to_owned()];
    view.update_in(cx, |v, _window, cx| v.xfer_message(XferMsg::Finished { xfer, paths }, cx));
    cx.run_until_parked();
    assert_eq!(pasted(laptop.drain(), there), ["'/Users/l/a b.txt' /Users/l/c.txt "]);
    assert!(!scratch.exists(), "what came down for it is gone");
}

/// ⌘V of files into a remote window stages them on its worker, with no notice, and lets the
/// window's held chord go once they are there; files on the window's own worker let it go at
/// once.
#[gpui::test]
fn files_pasted_into_a_window_are_staged_before_the_chord_goes(cx: &mut TestAppContext) {
    use gpui::AppContext as _;
    use slopty_proto::screen::{CaptureTarget, Quality};

    use crate::clipboard::ClipFiles;
    use crate::screen::{Opened, ScreenView};

    let (view, cx) = workspace(cx);
    let (studio, mut calls, _board) = connect_remote(&view, cx);
    let window =
        arrives(&view, cx, &studio, ItemKind::Window { window: slopty_core::WindowId(9) }, 1);
    let (out, _screen_rx) = mpsc::channel(64);
    let screen = cx.update(|_window, cx| {
        cx.new(|cx| {
            let opened = Opened {
                stream: StreamId(3),
                target: CaptureTarget::Window(slopty_core::WindowId(9)),
                size: (800, 600),
                quality: Quality::default(),
            };
            let handle = slopty_client::ScreenHandle::detached(StreamId(3));
            ScreenView::new(opened, handle, out, Theme::default(), cx)
        })
    });
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("shot.png");
    std::fs::write(&file, b"png").unwrap();
    let hold = |cx: &mut VisualTestContext, files: ClipFiles| {
        screen.update(cx, |v, cx| {
            v.set_paste_hook(Rc::new(move || crate::screen::PasteAhead {
                offer: None,
                files: Some(files.clone()),
            }));
            v.press(gpui::Keystroke::parse("cmd-v").unwrap(), cx);
        });
        screen.downgrade()
    };
    let weak = hold(cx, ClipFiles::Here(vec![file.clone()]));
    view.update_in(cx, |v, _window, cx| {
        v.paste_files_in_window(window, &weak, ClipFiles::Here(vec![file.clone()]), cx);
    });
    let Call::Upload(xfer, files, dest) = calls.try_recv().expect("an upload") else {
        panic!("an upload")
    };
    assert_eq!((files, dest), (vec![file.clone()], Dest::Staging));
    assert!(screen.read_with(cx, |v, _| v.paste_held()), "the chord waits");
    let paths = vec!["/Users/w/.slopty/drop/x/shot.png".to_owned()];
    view.update_in(cx, |v, _window, cx| v.xfer_message(XferMsg::Finished { xfer, paths }, cx));
    cx.run_until_parked();
    assert!(!screen.read_with(cx, |v, _| v.paste_held()), "and goes once they are there");
    assert_eq!(view.read_with(cx, |v, _| v.toast_text()), None, "a paste says nothing");

    let on_worker = ClipFiles::Worker { worker: studio.key, urls: Vec::new() };
    let weak = hold(cx, on_worker.clone());
    view.update_in(cx, |v, _window, cx| v.paste_files_in_window(window, &weak, on_worker, cx));
    assert!(calls.try_recv().is_err(), "nothing to send");
    assert!(!screen.read_with(cx, |v, _| v.paste_held()));
}

fn watches(sent: &[ClientMsg]) -> Vec<bool> {
    sent.iter()
        .filter_map(|m| match m {
            ClientMsg::Clip(ClipMsg::Watch(on)) => Some(*on),
            _ => None,
        })
        .collect()
}

fn offers(sent: &[ClientMsg]) -> Vec<Offer> {
    sent.iter()
        .filter_map(|m| match m {
            ClientMsg::Clip(ClipMsg::Offer(offer)) => Some(offer.clone()),
            _ => None,
        })
        .collect()
}

/// The worker's clipboard is wanted exactly while one of its terminals or windows has the
/// keyboard and the app is frontmost; the moment it becomes wanted, the worker hears this
/// client's clipboard, once.
#[gpui::test]
fn the_worker_clipboard_is_watched_only_while_its_tile_has_the_keyboard(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (mut studio, _calls, board) = connect_remote(&view, cx);
    board.copy(&[(TEXT_UTI, b"copied here")]);
    let shell = SessionId::new();
    let tile = opens(&view, cx, &studio, shell, studio.me, 1);
    let sent = studio.drain();
    assert_eq!(watches(&sent), [true], "a shell of the worker took the keyboard");
    let offer = offers(&sent);
    assert_eq!(offer.len(), 1, "{sent:?}");
    assert_eq!(offer[0].items[0].reps[0].inline.as_deref(), Some(&b"copied here"[..]));
    assert_eq!(offer[0].origin, Peer::Client(studio.me));

    view.update_in(cx, |v, window, cx| v.new_note(&NewNote, window, cx));
    cx.run_until_parked();
    let sent = studio.drain();
    assert_eq!(watches(&sent), [false], "a file tile is not the worker's");

    view.update_in(cx, |v, _window, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    let sent = studio.drain();
    assert_eq!(watches(&sent), [true]);
    assert!(offers(&sent).is_empty(), "nothing new to announce");

    view.update_in(cx, |v, _window, cx| v.set_app_active(false, cx));
    cx.run_until_parked();
    assert_eq!(watches(&studio.drain()), [false], "the app went to the back");
    board.copy(&[(TEXT_UTI, b"copied elsewhere")]);
    view.update_in(cx, |v, _window, cx| v.set_app_active(true, cx));
    cx.run_until_parked();
    let sent = studio.drain();
    assert_eq!(watches(&sent), [true]);
    assert_eq!(offers(&sent).len(), 1, "what was copied meanwhile is announced");
    assert_eq!(view.read_with(cx, |v, _| v.watching()), [studio.key]);
}

/// With the clipboard not shared with a worker, by the settings' per-worker switch, the worker
/// is told at once that its clipboard is no longer wanted, its copies land nowhere here, a fetch
/// of this client's clipboard is refused, and a paste carries nothing of it. Shared again, it
/// is wanted again and hears this clipboard.
#[gpui::test]
fn a_worker_the_clipboard_is_not_shared_with_neither_hears_nor_gives_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (mut studio, _calls, board) = connect_remote(&view, cx);
    board.copy(&[(TEXT_UTI, b"copied here")]);
    let shell = SessionId::new();
    opens(&view, cx, &studio, shell, studio.me, 1);
    let mine = offers(&studio.drain()).pop().expect("announced on focus");
    let share = |cx: &mut VisualTestContext, on: bool| {
        let mut sharing = slopty_settings::ClipboardSettings::default();
        sharing.workers.insert("studio".to_owned(), on);
        view.update_in(cx, |v, _window, cx| v.set_clipboard_sharing(sharing, cx));
        cx.run_until_parked();
    };

    share(cx, false);
    let sent = studio.drain();
    assert_eq!(watches(&sent), [false], "no longer wanted: {sent:?}");
    assert_eq!(offers(&sent), Vec::<Offer>::new());
    let key = studio.key;
    let hi = Rep { inline: Some(b"from the worker".to_vec()), ..listed(ClipFormat::Text, b"x") };
    let theirs = worker_offer(3, vec![entry(vec![hi])]);
    view.update_in(cx, |v, _window, cx| v.clip_message(key, ClipMsg::Offer(theirs), cx));
    assert_eq!(board.data(TEXT_UTI).as_deref(), Some(&b"copied here"[..]), "theirs stays theirs");
    let rep = mine.rep_ref(0, ClipType::Format(ClipFormat::Text));
    let fetch = ClipMsg::Fetch { rep, max: None, urgent: true };
    view.update_in(cx, |v, _window, cx| v.clip_message(key, fetch, cx));
    let refused = |m: &ClientMsg| matches!(m, ClientMsg::Clip(ClipMsg::Unavailable { .. }));
    let sent = studio.drain();
    assert!(sent.iter().any(refused), "mine is not handed over: {sent:?}");
    let hook = view.read_with(cx, |v, _| v.paste_hook(key)).expect("a remote tile");
    let ahead = hook();
    assert!(ahead.offer.is_none() && ahead.files.is_none(), "a paste carries none of it");
    board.copy(&[(TEXT_UTI, b"copied again")]);
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(offers(&studio.drain()).is_empty(), "nor does a new copy");

    share(cx, true);
    let sent = studio.drain();
    assert_eq!(watches(&sent), [true], "wanted again");
    assert_eq!(offers(&sent).len(), 1, "and it hears this clipboard: {sent:?}");
}

/// Where reading the clipboard asks the person first (iOS), a tile taking the keyboard reads
/// nothing of it: the worker's clipboard still comes here, and this one goes to the worker only
/// when a paste into its window reads it.
#[gpui::test]
fn a_clipboard_that_asks_is_read_only_for_a_paste(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (mut studio, _calls, _unused) = connect_remote(&view, cx);
    let board = Rc::new(Memory::asking());
    let shared: Rc<dyn Pasteboard> = Rc::<Memory>::clone(&board);
    view.update_in(cx, |v, _window, _cx| v.set_pasteboard(shared));
    board.copy(&[(TEXT_UTI, b"copied on the phone")]);
    let shell = SessionId::new();
    opens(&view, cx, &studio, shell, studio.me, 1);
    let sent = studio.drain();
    assert_eq!(watches(&sent), [true], "the worker's clipboard is still wanted here");
    assert!(offers(&sent).is_empty(), "{sent:?}");
    assert_eq!(board.reads(), 0, "nothing read on focus");

    let hook = view.read_with(cx, |v, _| v.paste_hook(studio.key)).unwrap();
    let Some(ClientMsg::Clip(ClipMsg::Offer(offer))) = hook().offer else {
        panic!("an offer to paste")
    };
    assert_eq!(offer.items[0].reps[0].inline.as_deref(), Some(&b"copied on the phone"[..]));
    assert!(board.reads() > 0, "read for the paste");
}

/// The system's paste button hands over the clipboard with no prompt, and that paste reads
/// nothing more of it: its text goes into the focused shell, and a picture with no text goes
/// to the worker as this client's offer ahead of the picture chord.
#[gpui::test]
fn a_paste_through_the_system_button_reads_the_clipboard_no_further(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (mut studio, _calls, _unused) = connect_remote(&view, cx);
    let board = Rc::new(Memory::asking());
    let shared: Rc<dyn Pasteboard> = Rc::<Memory>::clone(&board);
    view.update_in(cx, |v, _window, _cx| v.set_pasteboard(shared));
    let shell = SessionId::new();
    opens(&view, cx, &studio, shell, studio.me, 1);
    let _before = studio.drain();
    let mut tap = |data: &[(&str, &[u8])]| {
        board.copy(data);
        let tapped = Memory::default();
        tapped.copy(data);
        view.update_in(cx, |v, _window, cx| v.paste_made(&tapped, cx));
        cx.run_until_parked();
    };

    tap(&[(TEXT_UTI, b"copied on the phone")]);
    assert_eq!(pasted(studio.drain(), shell), ["copied on the phone"]);

    tap(&[("public.png", b"PNG")]);
    let sent = studio.drain();
    let offer = offers(&sent);
    assert_eq!(offer.len(), 1, "{sent:?}");
    assert_eq!(offer[0].items[0].reps[0].kind, ClipType::Format(ClipFormat::Png));
    let chord = |m: &ClientMsg| matches!(m, ClientMsg::Term { session, req: TermRequest::PastePicture(_) } if *session == shell);
    assert!(sent.iter().any(chord), "{sent:?}");
    assert_eq!(board.reads(), 0, "the clipboard itself was never read");
}

/// The worker's announcement lands here as text and promises; its fetch of this client's
/// offer is answered with the bytes, and a fetch of a stale offer with `Unavailable`.
#[gpui::test]
fn offers_land_as_promises_and_fetches_are_answered(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (mut studio, mut calls, board) = connect_remote(&view, cx);
    let hi = Rep { inline: Some(b"hi".to_vec()), ..listed(ClipFormat::Text, b"hi") };
    let offer = worker_offer(3, vec![entry(vec![listed(ClipFormat::Png, b"PNG"), hi])]);
    let key = studio.key;
    view.update_in(cx, |v, _window, cx| v.clip_message(key, ClipMsg::Offer(offer), cx));
    assert_eq!(board.data(TEXT_UTI).as_deref(), Some(&b"hi"[..]));
    assert_eq!(board.data("public.png").as_deref(), Some(&b"PNG"[..]), "fetched on paste");

    board.copy(&[(TEXT_UTI, b"mine")]);
    let shell = SessionId::new();
    opens(&view, cx, &studio, shell, studio.me, 1);
    let mine = offers(&studio.drain()).pop().expect("announced on focus");
    let rep = mine.rep_ref(0, ClipType::Format(ClipFormat::Text));
    let fetch = ClipMsg::Fetch { rep: rep.clone(), max: None, urgent: true };
    view.update_in(cx, |v, _window, cx| v.clip_message(key, fetch, cx));
    match calls.try_recv().unwrap() {
        Call::SendClip(sent, answer, urgent) => {
            assert_eq!((sent, answer, urgent), (rep, Fetched::Data(b"mine".to_vec()), true));
        }
        other => panic!("{other:?}"),
    }
    let old = Offer { generation: mine.generation.wrapping_add(5), ..mine };
    let stale = old.rep_ref(0, ClipType::Format(ClipFormat::Text));
    let fetch = ClipMsg::Fetch { rep: stale.clone(), max: None, urgent: false };
    view.update_in(cx, |v, _window, cx| v.clip_message(key, fetch, cx));
    match calls.try_recv().unwrap() {
        Call::SendClip(sent, answer, _) => assert_eq!((sent, answer), (stale, Fetched::Gone)),
        other => panic!("{other:?}"),
    }
}

/// A copy on one worker reaches another worker whose tile takes the keyboard next: its offer
/// goes on as it came, origin kept, and that worker's fetch of the picture is answered with the
/// first worker's bytes, fetched through the first worker's link. Before, the second worker
/// heard nothing, and ⌘V in its window pasted its own old clipboard.
#[gpui::test]
fn a_copy_on_one_worker_is_relayed_to_the_next_one_focused(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (studio, _studio_calls, _board) = connect_remote(&view, cx);
    let (mut laptop, mut laptop_calls) = link_remote(&view, cx, 8, "laptop", 0);
    let hi = Rep { inline: Some(b"hi".to_vec()), ..listed(ClipFormat::Text, b"hi") };
    let copied = worker_offer(9, vec![entry(vec![listed(ClipFormat::Png, b"PNG"), hi])]);
    let studio_key = studio.key;
    let offer = copied.clone();
    view.update_in(cx, |v, _window, cx| v.clip_message(studio_key, ClipMsg::Offer(offer), cx));

    let there = SessionId::new();
    opens(&view, cx, &laptop, there, laptop.me, 1);
    let sent = laptop.drain();
    let relayed = offers(&sent).pop().expect("the studio's copy, relayed to the laptop");
    assert_eq!((relayed.origin, relayed.generation), (copied.origin, 9), "origin kept");
    assert_eq!(relayed.items, copied.items);

    let picture = copied.rep_ref(0, ClipType::Format(ClipFormat::Png));
    let fetch = ClipMsg::Fetch { rep: picture.clone(), max: None, urgent: true };
    let laptop_key = laptop.key;
    view.update_in(cx, |v, _window, cx| v.clip_message(laptop_key, fetch, cx));
    cx.run_until_parked();
    match laptop_calls.try_recv().expect("the laptop is answered") {
        Call::SendClip(rep, answer, urgent) => {
            assert_eq!((rep, answer, urgent), (picture, Fetched::Data(b"PNG".to_vec()), true));
        }
        other => panic!("{other:?}"),
    }
}

/// A worker link that serves one clipboard representation and counts the fetches.
#[derive(Debug)]
struct Serves(&'static [u8], Arc<std::sync::atomic::AtomicUsize>);

impl Remote for Serves {
    fn upload(&self, _xfer: XferId, _files: Vec<PathBuf>, _dest: Dest, _again: bool) {}

    fn cancel(&self, _xfer: XferId) {}

    fn download(&self, _ask: slopty_client::xfer::Download) -> Result<Vec<PathBuf>, XferError> {
        Err(XferError::Worker("clipboard only".to_owned()))
    }

    fn clip_fetch(&self, _rep: &RepRef, _max: Option<u64>, _wait: Duration) -> Fetched {
        self.1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Fetched::Data(self.0.to_vec())
    }

    fn send_clip(&self, _rep: RepRef, _answer: Fetched, _urgent: bool) {}

    fn forward(&self, _port: u16) -> Option<u16> {
        None
    }
}

/// A worker's promise here follows its link: pasted while the link is down it gives nothing at
/// once; after the link comes back it is fetched over the new link, never the dead one it was
/// made on; and bytes that are not what was offered (a restarted worker's same generation)
/// are not pasted.
#[gpui::test]
fn a_promise_is_fetched_over_the_link_the_worker_has_now(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (studio, _calls, board) = connect_remote(&view, cx);
    let key = studio.key;
    let offer = worker_offer(3, vec![entry(vec![listed(ClipFormat::Png, b"PNG")])]);
    view.update_in(cx, |v, _window, cx| v.clip_message(key, ClipMsg::Offer(offer), cx));
    view.update_in(cx, |v, _window, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("lost".into()), cx);
    });
    assert_eq!(board.data("public.png"), None, "no link, no bytes, no wait");

    let relink = |serves: &'static [u8], cx: &mut VisualTestContext| {
        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let remote: Arc<dyn Remote> = Arc::new(Serves(serves, Arc::clone(&asked)));
        let (tx, _rx) = mpsc::channel(256);
        let factory: ScreenFactory =
            Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
        let link =
            WorkerLink { me: studio.me, out: tx, open_screen: factory, remote: Some(remote) };
        view.update_in(cx, |v, _window, cx| {
            v.connect_worker(key, link, hello("studio", Vec::new()), cx);
        });
        asked
    };
    let asked = relink(b"PNG", cx);
    assert_eq!(board.data("public.png").as_deref(), Some(&b"PNG"[..]));
    assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 1, "over the new link");

    let asked = relink(b"a restarted worker's", cx);
    assert_eq!(board.data("public.png"), None, "not the offered bytes");
    assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 1);
}

/// A picture pasted into an agent's thread composer goes up to a directory of its own on the
/// worker, with a chip in the composer from the paste until the message goes; the draft never
/// holds its path, and nothing is sent until the message is, its text followed by the landed
/// path. A file dropped on the thread, or picked with the attach button, is attached the same
/// way.
#[gpui::test]
fn a_picture_pasted_into_the_composer_stays_a_chip_until_sent(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (mut studio, mut calls, _board) = connect_remote(&view, cx);
    let session = SessionId::new();
    let tile = opens(&view, cx, &studio, session, studio.me, 1);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    agent_thread(&view, cx, studio.key, session);
    let face = view.read_with(cx, |v, _| v.thread_face(session).cloned()).expect("its thread");
    cx.simulate_input("look at");
    studio.drain();

    let png: Vec<u8> = b"\x89PNG".iter().copied().chain(std::iter::repeat_n(7, 196)).collect();
    let image = gpui::Image::from_bytes(gpui::ImageFormat::Png, png.clone());
    cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_image(&image)));
    cx.simulate_keystrokes("cmd-v");
    cx.run_until_parked();
    let Call::Upload(xfer, files, dest) = calls.try_recv().expect("an upload") else {
        panic!("an upload");
    };
    assert_eq!(dest, Dest::Attachment, "never the session's working tree");
    let [file] = files.as_slice() else { panic!("one file: {files:?}") };
    assert_eq!(file.file_name().unwrap(), "pasted-image.png");
    assert_eq!(std::fs::read(file).unwrap(), png, "the pasted bytes");
    let chips = |cx: &mut VisualTestContext| {
        face.read_with(cx, |f, _| {
            f.attachments().iter().map(Attachment::progress).collect::<Vec<_>>()
        })
    };
    assert_eq!(chips(cx), ["\u{2191} 0%"], "a chip while it uploads");
    assert!(cx.debug_bounds("composer-attachment").is_some(), "drawn in the composer");
    assert_eq!(
        face.read_with(cx, crate::conversation::thread::ThreadView::draft),
        "look at",
        "nothing typed yet"
    );

    view.update_in(cx, |v, _window, cx| {
        v.xfer_message(XferMsg::Progress { xfer, done: png.len() as u64 / 2 }, cx);
    });
    cx.run_until_parked();
    assert_eq!(chips(cx), ["\u{2191} 50%"]);
    let landed = "/Users/me/.slopty/drop/x/pasted-image.png".to_owned();
    view.update_in(cx, |v, _window, cx| {
        v.xfer_message(XferMsg::Finished { xfer, paths: vec![landed.clone()] }, cx);
    });
    cx.run_until_parked();
    assert_eq!(chips(cx).len(), 1, "the chip stays once it landed");
    assert!(cx.debug_bounds("composer-attachment-progress").is_none(), "done going up");
    assert_eq!(
        face.read_with(cx, crate::conversation::thread::ThreadView::draft),
        "look at",
        "no path in the draft"
    );
    assert!(!file.exists(), "the scratch copy here goes with the upload");
    let sent = |studio: &mut Fake| -> Vec<String> {
        use slopty_proto::thread::wire::{Intent, ThreadRequest};
        studio
            .drain()
            .into_iter()
            .filter_map(|m| match m {
                ClientMsg::Thread(ThreadRequest::Intent {
                    intent: Intent::Send { text, .. },
                    ..
                }) => Some(text),
                _ => None,
            })
            .collect()
    };
    assert!(sent(&mut studio).is_empty(), "the chip waits for the message");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(sent(&mut studio), [format!("look at {landed}")], "the path goes with it");
    assert!(chips(cx).is_empty(), "the chip went with it");

    let dir = tempfile::tempdir().unwrap();
    let dropped = dir.path().join("design.png");
    std::fs::write(&dropped, b"png").unwrap();
    view.update_in(cx, |v, _window, cx| v.drop_files(tile, std::slice::from_ref(&dropped), cx));
    cx.run_until_parked();
    let Call::Upload(xfer, files, dest) = calls.try_recv().expect("an upload") else {
        panic!("an upload");
    };
    assert_eq!((files, dest), (vec![dropped], Dest::Attachment), "a drop on the thread attaches");
    assert_eq!(face.read_with(cx, |f, _| f.attachments()[0].name.clone()), "design.png");

    // The chip is the one place the upload is said, and the way to take it off the draft.
    assert!(cx.debug_bounds(selector("upload", tile.item)).is_none(), "no header pill as well");
    let remove = cx.debug_bounds("composer-attachment-remove").expect("the chip's way off");
    cx.simulate_click(remove.center(), Modifiers::none());
    cx.run_until_parked();
    assert!(chips(cx).is_empty(), "the chip goes at once");
    assert!(matches!(calls.try_recv(), Ok(Call::Cancel(c)) if c == xfer), "and the upload stops");
    let nodes = tree(cx);
    assert!(!nodes.iter().any(|n| n.is("Button", Some("Cancel upload"))), "{nodes:#?}");

    // The attach button asks for the system's picker, for this tile.
    let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = std::rc::Rc::clone(&asked);
    cx.update(|_, cx| {
        cx.set_global(crate::workspace::folders::FilesSeam(std::rc::Rc::new(
            move |ask: &crate::workspace::folders::FilesAsk| sink.borrow_mut().push(ask.clone()),
        )));
    });
    let clip = cx.debug_bounds("thread-attach").expect("the attach button");
    cx.simulate_click(clip.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(*asked.borrow(), [crate::workspace::folders::FilesAsk::Import(tile)]);
}

/// Files dropped on a shell go up to its directory; the tile shows how far the upload got,
/// quietly; when the worker has them all, their quoted paths are typed into the shell as one
/// paste and the progress goes.
#[gpui::test]
fn a_drop_on_a_shell_uploads_shows_progress_and_types_the_quoted_paths(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (mut studio, mut calls, _board) = connect_remote(&view, cx);
    let shell = SessionId::new();
    let tile = opens(&view, cx, &studio, shell, studio.me, 1);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("my notes.txt");
    std::fs::write(&file, vec![b'x'; 1000]).unwrap();
    studio.drain();

    let bounds = view.read_with(cx, |v, _| v.tile_bounds(tile)).expect("drawn");
    let at = bounds.center();
    cx.simulate_event(FileDropEvent::Entered {
        position: at,
        paths: ExternalPaths(std::iter::once(file.clone()).collect()),
    });
    cx.simulate_event(FileDropEvent::Pending { position: at });
    cx.simulate_event(FileDropEvent::Submit { position: at });
    cx.run_until_parked();
    let Call::Upload(xfer, files, dest) = calls.try_recv().expect("an upload") else {
        panic!("an upload");
    };
    assert_eq!((files, dest), (vec![file], Dest::SessionCwd(shell)));

    view.update_in(cx, |v, _window, cx| v.xfer_message(XferMsg::Progress { xfer, done: 420 }, cx));
    cx.run_until_parked();
    let label = view.read_with(cx, |v, _| v.upload_on(tile).map(|(_, u)| u.label()));
    assert_eq!(label.as_deref(), Some("\u{2191} 42%"));
    assert!(cx.debug_bounds(selector("upload", tile.item)).is_some(), "the tile shows it");
    let header = cx.debug_bounds(selector("title", tile.item)).expect("the header");
    let line = cx.debug_bounds(selector("upload-progress", tile.item)).expect("a progress line");
    assert!((f32::from(line.size.height) - 2.0).abs() < 0.01, "2 pt: {line:?}");
    assert!(line.bottom() <= header.bottom() && line.bottom() >= header.bottom() - px(1.5));
    let share = f32::from(line.size.width) / f32::from(header.size.width);
    assert!((share - 0.42).abs() < 0.02, "along the header as far as it got: {share}");

    let paths = vec!["/Users/me/work/my notes.txt".to_owned(), "/tmp/b".to_owned()];
    view.update_in(cx, |v, _window, cx| v.xfer_message(XferMsg::Finished { xfer, paths }, cx));
    cx.run_until_parked();
    let pasted: Vec<String> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Term { session, req: TermRequest::Paste { text, .. } }
                if session == shell =>
            {
                Some(text)
            }
            _ => None,
        })
        .collect();
    assert_eq!(pasted, ["'/Users/me/work/my notes.txt' /tmp/b "], "one bracketable paste");
    assert!(cx.debug_bounds(selector("upload", tile.item)).is_none(), "done: the progress goes");
    assert!(cx.debug_bounds(selector("upload-progress", tile.item)).is_none(), "and its line");

    // A second drop is cancelled from its progress pill.
    let other = dir.path().join("b.bin");
    std::fs::write(&other, b"b").unwrap();
    view.update_in(cx, |v, _window, cx| v.drop_files(tile, &[other], cx));
    let Call::Upload(second, ..) = calls.try_recv().unwrap() else { panic!("an upload") };
    cx.run_until_parked();
    let pill = cx.debug_bounds(selector("upload", tile.item)).expect("the pill");
    cx.simulate_click(pill.center(), Modifiers::none());
    cx.run_until_parked();
    assert!(matches!(calls.try_recv().unwrap(), Call::Cancel(x) if x == second));
    assert!(view.read_with(cx, |v, _| v.upload_on(tile).is_none()));
}

/// An upload outlives its worker's link. While the worker is away the tile keeps its progress
/// and its cancel, which reaches the upload through the remote it went by; on the next link the
/// worker's word on it types the paths into the shell. One whose worker stays away past the
/// wait for a link ends, said in a notice.
#[gpui::test]
fn an_upload_outlives_its_workers_link(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (studio, mut calls, _board) = connect_remote(&view, cx);
    let key = studio.key;
    let shell = SessionId::new();
    let tile = opens(&view, cx, &studio, shell, studio.me, 1);
    let dir = tempfile::tempdir().unwrap();
    let file = |name: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, b"x").unwrap();
        path
    };
    let away = |cx: &mut VisualTestContext| {
        view.update_in(cx, |v, _window, cx| {
            v.disconnect_worker(key, WorkerStatus::Reconnecting("lost".into()), cx);
        });
        cx.run_until_parked();
    };
    let relink = |cx: &mut VisualTestContext| {
        let (tx, rx) = mpsc::channel(256);
        let (calls, recorded) = mpsc::unbounded_channel();
        let remote: Arc<dyn Remote> = Arc::new(Recorder(calls, 0));
        let factory: ScreenFactory =
            Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
        let link =
            WorkerLink { me: studio.me, out: tx, open_screen: factory, remote: Some(remote) };
        let ack = hello("studio", vec![summary(shell, None)]);
        view.update_in(cx, |v, _window, cx| v.connect_worker(key, link, ack, cx));
        cx.run_until_parked();
        (rx, recorded)
    };

    view.update_in(cx, |v, _window, cx| v.drop_files(tile, &[file("a.txt")], cx));
    let Call::Upload(first, ..) = calls.try_recv().expect("an upload") else { panic!("upload") };
    away(cx);
    let held = view.read_with(cx, |v, _| v.upload_on(tile).map(|(x, u)| (x, u.label())));
    assert_eq!(held, Some((first, "\u{2191} 0%".to_owned())), "kept while the worker is away");
    assert!(cx.debug_bounds(selector("upload", tile.item)).is_some(), "the tile still shows it");

    let (mut out, mut calls) = relink(cx);
    let paths = vec!["/Users/me/a.txt".to_owned()];
    view.update_in(cx, |v, _window, cx| {
        v.xfer_message(XferMsg::Progress { xfer: first, done: 1 }, cx);
        v.xfer_message(XferMsg::Finished { xfer: first, paths }, cx);
    });
    cx.run_until_parked();
    let sent: Vec<ClientMsg> = std::iter::from_fn(|| out.try_recv().ok()).collect();
    assert_eq!(pasted(sent, shell), ["/Users/me/a.txt "], "typed over the next link");
    assert!(view.read_with(cx, |v, _| v.upload_on(tile).is_none()));

    // Cancelled from its pill while the worker is away: the remote it went by hears it.
    view.update_in(cx, |v, _window, cx| v.drop_files(tile, &[file("b.txt")], cx));
    let Call::Upload(second, ..) = calls.try_recv().expect("an upload") else { panic!("upload") };
    away(cx);
    let pill = cx.debug_bounds(selector("upload", tile.item)).expect("the pill");
    cx.simulate_click(pill.center(), Modifiers::none());
    cx.run_until_parked();
    assert!(matches!(calls.try_recv(), Ok(Call::Cancel(x)) if x == second), "the upload stops");
    assert!(view.read_with(cx, |v, _| v.upload_on(tile).is_none()));

    // A worker that stays away past the wait: the upload has ended on its link too.
    let (_out, mut calls) = relink(cx);
    view.update_in(cx, |v, _window, cx| v.drop_files(tile, &[file("c.txt")], cx));
    let Call::Upload(..) = calls.try_recv().expect("an upload") else { panic!("upload") };
    away(cx);
    let short = slopty_client::xfer::RELINK_WAIT.checked_sub(Duration::from_secs(1)).unwrap();
    cx.executor().advance_clock(short);
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.upload_on(tile).is_some()), "still waiting");
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.upload_on(tile).is_none()), "ended");
    let notice = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(notice.as_deref(), Some("c.txt did not reach studio: the machine went away"));
}

/// A drop on a note sends nothing; a drop on a remote window goes to the worker's staging.
#[gpui::test]
fn a_drop_goes_where_the_tile_can_take_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (studio, mut calls, _board) = connect_remote(&view, cx);
    let note = arrives(&view, cx, &studio, ItemKind::File { path: "/w/n.md".to_owned() }, 1);
    let window =
        arrives(&view, cx, &studio, ItemKind::Window { window: slopty_core::WindowId(9) }, 2);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("shot.png");
    std::fs::write(&file, b"png").unwrap();
    view.update_in(cx, |v, _window, cx| {
        v.drop_files(note, std::slice::from_ref(&file), cx);
        v.drop_files(window, std::slice::from_ref(&file), cx);
    });
    let Call::Upload(_, _, dest) = calls.try_recv().unwrap() else { panic!("an upload") };
    assert_eq!(dest, Dest::Staging);
    assert!(calls.try_recv().is_err(), "the note took nothing");
}

/// A shell's listening ports are counted in the status bar, not on its tile's header; one
/// served on another port here says so in a notice; the count lists them to open.
#[gpui::test]
fn forwarded_ports_are_counted_and_listed_off_the_tile(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (studio, _calls, _board) = connect_remote(&view, cx);
    let shell = SessionId::new();
    let _tile = opens(&view, cx, &studio, shell, studio.me, 1);
    let port = |number| Port { number, pid: 2, process: "vite".to_owned(), session: Some(shell) };
    let forwards = vec![
        Forward { port: port(5173), local: Some(5173) },
        Forward { port: port(8080), local: Some(8081) },
    ];
    view.update_in(cx, |v, _window, cx| v.ports_changed(shell, forwards, cx));
    cx.run_until_parked();
    let notice = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(notice.as_deref(), Some("Port 8080 is taken here; forwarded on 8081"));
    // Its line opens the page in a tile on the shell's worker, named by the worker's port:
    // each client serves it where it can.
    let mut studio = studio;
    studio.drain();
    let count = cx.debug_bounds("status-ports").expect("the status bar counts them");
    cx.simulate_click(count.center(), Modifiers::none());
    cx.run_until_parked();
    cx.simulate_input("8080 in a tile");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let opened: Vec<String> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Items(ItemOp::Add(Item { kind: ItemKind::Browser { url }, .. })) => {
                Some(url)
            }
            _ => None,
        })
        .collect();
    assert_eq!(opened, ["http://localhost:8080/"]);
    view.update_in(cx, |v, _window, cx| v.ports_changed(shell, Vec::new(), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("status-ports").is_none(), "gone with the server");
}

/// Two clients of one worker show one browser item, the worker's address; each loads it from
/// the local port it serves the worker's port on, and asks its own link for that port.
#[gpui::test]
fn a_browser_item_is_the_worker_s_address_served_at_each_client_s_own_port(
    cx: &mut TestAppContext,
) {
    let url = "http://localhost:5173/app";
    let item = Item {
        id: ItemId::new(),
        kind: ItemKind::Browser { url: url.to_owned() },
        name: None,
        facts: BTreeMap::new(),
    };
    let mut loaded = Vec::new();
    for offset in [0, 1] {
        let (view, cx) = workspace(cx);
        let (studio, mut calls, _board) = connect_remote_at(&view, cx, offset);
        let items = vec![item.clone()];
        view.update_in(cx, |v, _window, cx| {
            v.apply_sync(studio.key, ItemSync::Snapshot { version: 1, items }, cx);
        });
        cx.run_until_parked();
        let (named, local) = view.read_with(cx, |v, cx| {
            let b = v.browser(item.id).map(|b| b.read(cx));
            (b.map(|b| b.url().to_owned()), b.and_then(|b| b.local_url().map(str::to_owned)))
        });
        assert_eq!(named.as_deref(), Some(url), "the item keeps the worker's address");
        loaded.push(local);
        let mut forwards = Vec::new();
        while let Ok(call) = calls.try_recv() {
            if let Call::Forward(port) = call {
                forwards.push(port);
            }
        }
        assert_eq!(forwards, [5173], "asked once, for the worker's port");
    }
    assert_eq!(
        loaded,
        [
            Some("http://localhost:5173/app".to_owned()),
            Some("http://localhost:5174/app".to_owned())
        ]
    );
}

/// A page on a host only the worker names waits for the worker's link, then loads as it is
/// through the worker's proxy, asked of the link once and never forwarded; a load the worker
/// could not reach says why.
#[gpui::test]
fn a_page_on_any_other_host_loads_through_the_workers_proxy(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (studio, mut calls, _board) = connect_remote(&view, cx);
    let page = |url: &str, seq, cx: &mut VisualTestContext| {
        let kind = ItemKind::Browser { url: url.to_owned() };
        let tile = arrives(&view, cx, &studio, kind, seq);
        view.read_with(cx, |v, _| v.browser(tile.item).cloned()).expect("a page view")
    };
    let admin = page("http://db-admin:8080/", 1, cx);
    let local = admin.read_with(cx, |b, _| b.local_url().map(str::to_owned));
    assert_eq!(local.as_deref(), Some("http://db-admin:8080/"), "loaded as it is");
    let mut asked = Vec::new();
    while let Ok(call) = calls.try_recv() {
        asked.push(call);
    }
    assert!(matches!(asked[..], [Call::Proxy]), "the proxy, and no forward: {asked:?}");

    let nowhere = page("http://nowhere.internal/", 2, cx);
    cx.update(|window, cx| {
        nowhere.update(cx, |p, cx| {
            p.native_event(
                WebEvent::Failed("A server with the specified hostname could not be found.".into()),
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
    let failed = nowhere.read_with(cx, |b, _| b.page().failed.clone());
    assert_eq!(failed.as_deref(), Some("the worker finds no host named nowhere.internal"));
}

/// A drop's landing (where the platform received files an app promised) lives exactly as long
/// as something uploads from it: a drop on a note deletes it at once, and a drop on a window
/// uploads from it and deletes it once the upload is over.
#[gpui::test]
fn a_drops_landing_goes_once_nothing_uploads_from_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (studio, mut calls, _board) = connect_remote(&view, cx);
    let note = arrives(&view, cx, &studio, ItemKind::File { path: "/w/n.md".to_owned() }, 1);
    let window =
        arrives(&view, cx, &studio, ItemKind::Window { window: slopty_core::WindowId(9) }, 2);
    let landing = |name: &str| {
        let root = slopty_platform::file_drop::root();
        let dir = root.join(format!("{}-test-{name}-{}", std::process::id(), XferId::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("shot.png");
        std::fs::write(&file, b"png").unwrap();
        (dir, file)
    };

    let (on_note, file) = landing("note");
    view.update_in(cx, |v, _window, cx| {
        v.drop_landing = Some(on_note.clone());
        v.drop_files(note, &[file], cx);
    });
    cx.run_until_parked();
    assert!(!on_note.exists(), "a note takes nothing, and the landing goes");
    assert!(calls.try_recv().ok().is_none(), "nothing was sent");

    let (on_window, file) = landing("window");
    view.update_in(cx, |v, _window, cx| {
        v.drop_landing = Some(on_window.clone());
        v.drop_files(window, &[file], cx);
    });
    cx.run_until_parked();
    let Call::Upload(xfer, ..) = calls.try_recv().unwrap() else { panic!("an upload") };
    let scratch =
        view.read_with(cx, |v, _| v.upload_on(window).and_then(|(_, u)| u.scratch.clone()));
    assert_eq!(scratch.as_ref(), Some(&on_window), "the upload holds the landing");
    assert!(on_window.exists(), "kept while the files go up");
    view.update_in(cx, |v, _window, cx| {
        v.xfer_message(XferMsg::Finished { xfer, paths: vec!["/tmp/shot.png".to_owned()] }, cx);
    });
    cx.run_until_parked();
    assert!(!on_window.exists(), "gone once the upload is over");
}

/// A composer that has the keyboard as the window becomes active starts its caret from a focus
/// listener, which runs after the frame is painted: the frame after is drawn as from scratch,
/// and so is the chip of a file dropped on the thread.
#[gpui::test]
fn a_caret_started_by_focus_is_drawn_as_from_scratch(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let (studio, _calls, _board) = connect_remote(&view, cx);
    let session = SessionId::new();
    let tile = opens(&view, cx, &studio, session, studio.me, 1);
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(session) }, cx);
        v.focus_tile(tile, cx);
    });
    agent_thread(&view, cx, studio.key, session);
    cx.update(|window, _cx| window.activate_window());
    cx.run_until_parked();
    let stale = cx.update(|window, cx| crate::retained::stale(window, cx, 12));
    assert!(stale.is_none(), "the caret: {}", stale.unwrap_or_default());

    let dir = tempfile::tempdir().unwrap();
    let dropped = dir.path().join("screen-recording.mov");
    std::fs::write(&dropped, b"mov").unwrap();
    view.update_in(cx, |v, _window, cx| v.drop_files(tile, std::slice::from_ref(&dropped), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("composer-attachment").is_some(), "the chip");
    let stale = cx.update(|window, cx| crate::retained::stale(window, cx, 12));
    assert!(stale.is_none(), "the chip: {}", stale.unwrap_or_default());
}

/// A worker's window tile streaming at `stream`, drawn.
#[cfg(target_os = "macos")]
fn streaming(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &Fake,
    stream: StreamId,
) -> TileRef {
    use slopty_proto::screen::VideoCodec;
    let window = slopty_core::WindowId(9);
    let tile = arrives(view, cx, fake, ItemKind::Window { window }, 1);
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        let opened = ScreenEvent::Opened {
            stream,
            target: CaptureTarget::Window(window),
            codec: VideoCodec::Hevc,
            width: 1600,
            height: 1000,
            scale: 2.0,
            stripes: Vec::new(),
        };
        v.screen_event(key, opened, cx);
    });
    cx.run_until_parked();
    tile
}

/// A drag from this Mac carrying `board`, for a copy, tagged `own` when this window began it.
#[cfg(target_os = "macos")]
fn carried(board: &dyn Pasteboard, own: Option<u64>) -> crate::workspace::remote::Carried<'_> {
    use slopty_proto::drag::DragOps;
    crate::workspace::remote::Carried { board, allowed: DragOps::COPY, own }
}

/// The drag steps sent to a worker.
#[cfg(target_os = "macos")]
fn drag_steps(sent: Vec<ClientMsg>) -> Vec<slopty_proto::drag::DragInput> {
    sent.into_iter()
        .filter_map(|m| match m {
            ClientMsg::Screen(ScreenRequest::Input {
                input: slopty_proto::screen::ScreenInput::Drag(d),
                ..
            }) => Some(d),
            _ => None,
        })
        .collect()
}

/// A drag over a remote window's body is the worker's: it enters with what it carries, its
/// files start up into the drag's landing and its big data goes up beside them. Off the body
/// it is GPUI's again: the worker's drag ends and its upload stops. A drop the worker says did
/// not land says why, and a drag over a remote tile carrying nothing a remote app takes is
/// refused there.
#[cfg(target_os = "macos")]
#[gpui::test]
fn a_drag_over_a_remote_body_is_the_workers_and_elsewhere_gpuis(cx: &mut TestAppContext) {
    use slopty_platform::file_drop::Over;
    use slopty_proto::drag::{DragEvent, DragInput, DragOp};
    use slopty_proto::transfer::{INLINE_CLIP_BYTES, Source};

    use crate::workspace::remote::DropIn;
    let (view, cx) = workspace(cx);
    let (mut studio, mut calls, _board) = connect_remote(&view, cx);
    let tile = streaming(&view, cx, &studio, StreamId(1));
    studio.drain();
    let bounds = view.read_with(cx, |v, _| v.tile_bounds(tile)).expect("drawn");
    let body = bounds.center();
    let header = point(bounds.center().x, bounds.origin.y + px(4.0));
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("shot.png");
    std::fs::write(&file, b"png").unwrap();
    let url = format!("file://{}", file.display());
    let picture = vec![3_u8; INLINE_CLIP_BYTES + 1];
    let drag_board = Memory::default();
    drag_board.copy_items(&[&[("public.file-url", url.as_bytes())], &[("public.png", &picture)]]);
    let mut state = DropIn::default();
    let over = |state: &mut DropIn, p, cx: &mut VisualTestContext| {
        view.update_in(cx, |v, _w, cx| v.drag_over(state, p, carried(&drag_board, None), cx))
    };

    assert_eq!(over(&mut state, header, cx), Over::Local, "the header is GPUI's");
    assert_eq!(over(&mut state, body, cx), Over::Remote(DragOp::Copy));
    let drag = state.drag().expect("the worker's drag");
    let steps = drag_steps(studio.drain());
    let [DragInput::Enter { drag: entered, items, .. }] = steps.as_slice() else {
        panic!("{steps:?}")
    };
    assert_eq!((*entered, items.len()), (drag, 2));
    let Some(Call::Upload(xfer, files, Dest::Drag(to))) = calls.try_recv().ok() else {
        panic!("the files go up at once")
    };
    assert_eq!((files, to), (vec![file], drag));
    let Some(Call::SendClip(rep, Fetched::Data(bytes), false)) = calls.try_recv().ok() else {
        panic!("the picture goes up beside them, in bulk")
    };
    assert_eq!((rep.source, rep.item, bytes.len()), (Source::Drag(drag), 1, picture.len()));

    assert_eq!(over(&mut state, body, cx), Over::Remote(DragOp::Copy));
    assert!(drag_steps(studio.drain()).is_empty(), "a still drag sends nothing");
    assert_eq!(over(&mut state, header, cx), Over::Local, "off the body");
    assert_eq!(drag_steps(studio.drain()), [DragInput::Leave { drag }]);
    assert!(matches!(calls.try_recv(), Ok(Call::Cancel(x)) if x == xfer), "its upload stops");

    assert_eq!(over(&mut state, body, cx), Over::Remote(DragOp::Copy), "back on");
    let again = state.drag().expect("a new drag");
    assert_ne!(again, drag);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        let error = Some("another drag is crossing this worker".to_owned());
        let event = DragEvent::Ended { drag: again, op: DragOp::None, error };
        v.screen_event(key, ScreenEvent::Drag { stream: StreamId(1), event }, cx);
    });
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |v, _| v.toast_text()).as_deref(),
        Some("The drop did not land: another drag is crossing this worker")
    );

    let empty = Memory::default();
    empty.copy(&[("dyn.ah62d4rv4gu8y", b"x")]);
    let mut fresh = DropIn::default();
    let refused =
        view.update_in(cx, |v, _w, cx| v.drag_over(&mut fresh, body, carried(&empty, None), cx));
    assert_eq!(refused, Over::Remote(DragOp::None), "nothing a remote app takes");
    assert_eq!(fresh.drag(), None);
}

/// A drag out the tile hands over goes on as this Mac's drag, and the worker's link is told of
/// it first, so the catch reaches it off the main thread where a target's read of its data
/// waits.
#[cfg(target_os = "macos")]
#[gpui::test]
fn a_drag_out_going_on_here_is_heard_by_the_link(cx: &mut TestAppContext) {
    use slopty_client::dnd::out::{Outgoing, Shared};
    use slopty_proto::drag::{DragId, DragItem, FileMeta};
    let (view, cx) = workspace(cx);
    let (studio, mut calls, _board) = connect_remote(&view, cx);
    let parked = Rc::new(std::cell::Cell::new(0_usize));
    let sink = Rc::clone(&parked);
    view.update_in(cx, |v, _w, _cx| {
        v.set_drag_sink(Rc::new(move |promises| {
            sink.set(promises.len());
            true
        }));
    });
    let drag = DragId::new();
    let file = FileMeta {
        name: "a.txt".to_owned(),
        size: 1,
        folder: false,
        mode: 0o644,
        mtime_ms: WallMs::ZERO,
        path: Some("/Users/w/a.txt".to_owned()),
    };
    let items = vec![DragItem { file: Some(file), promised: None, reps: Vec::new() }];
    let shared = Arc::new(Shared::new(Outgoing::began(drag, items)));
    let began = view.update_in(cx, |v, _w, _cx| v.drag_out_of(studio.key, &shared));
    assert!(began);
    assert_eq!(parked.get(), 1, "the file goes on as a promise");
    assert!(matches!(calls.try_recv(), Ok(Call::WatchDragOut(d)) if d == drag));
}

/// A file dragged out of a folder tile is fetched when the drop asks for it: a fetch that fails
/// says so with why, rather than leaving only the Finder's bare error, and one that lands says
/// nothing. A machine that is away is said at once and nothing is dragged.
#[cfg(target_os = "macos")]
#[gpui::test]
fn a_drag_out_that_fails_says_why(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (studio, _calls, _board) = connect_remote(&view, cx);
    let parked = Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = Rc::clone(&parked);
    view.update_in(cx, |v, _w, _cx| {
        v.set_drag_sink(Rc::new(move |promises| {
            sink.borrow_mut().extend(promises);
            true
        }));
    });
    let toast = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.toast_text());
    let key = studio.key;
    assert!(view.update_in(cx, |v, _w, cx| v.drag_out(key, "/Users/w/a.txt", cx)));
    let promise = parked.borrow_mut().pop().expect("the file goes on as a promise");
    let dir = tempfile::tempdir().unwrap();
    let not_a_folder = dir.path().join("plain");
    std::fs::write(&not_a_folder, b"x").unwrap();

    (promise.keep)(&dir.path().join("a.txt")).unwrap();
    cx.run_until_parked();
    assert_eq!(toast(cx), None, "a drop that lands says nothing");
    (promise.keep)(&not_a_folder.join("a.txt")).unwrap_err();
    cx.run_until_parked();
    let said = toast(cx).unwrap_or_default();
    assert!(said.starts_with("a.txt was not dragged out: "), "{said}");

    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("lost".into()), cx);
    });
    assert!(!view.update_in(cx, |v, _w, cx| v.drag_out(key, "/Users/w/a.txt", cx)));
    cx.run_until_parked();
    assert_eq!(toast(cx).as_deref(), Some("studio is away; a.txt was not dragged out"));
    assert_eq!(parked.borrow().len(), 0, "nothing is dragged");
}

/// A drag out of a worker's app that comes back over a tile of that worker names the worker's
/// own files by their paths there: nothing goes up, and the drop is taken as it is, with no
/// promise called in. Any other drag over the tile reads its pasteboard and sends its files up.
#[cfg(target_os = "macos")]
#[gpui::test]
fn a_drag_out_back_over_its_worker_names_its_files_there(cx: &mut TestAppContext) {
    use slopty_client::dnd::out::{Outgoing, Shared};
    use slopty_platform::file_drop::{Over, Taken};
    use slopty_proto::drag::{DragId, DragInput, DragItem, DragOp, FileMeta};

    use crate::workspace::remote::DropIn;
    let (view, cx) = workspace(cx);
    let (mut studio, mut calls, _board) = connect_remote(&view, cx);
    let tile = streaming(&view, cx, &studio, StreamId(1));
    studio.drain();
    view.update_in(cx, |v, _w, _cx| v.set_drag_sink(Rc::new(|_promises| true)));
    let file = FileMeta {
        name: "a.txt".to_owned(),
        size: 1,
        folder: false,
        mode: 0o644,
        mtime_ms: WallMs::ZERO,
        path: Some("/Users/w/a.txt".to_owned()),
    };
    let own = DragItem { file: Some(file), promised: None, reps: Vec::new() };
    let shared = Arc::new(Shared::new(Outgoing::began(DragId::new(), vec![own.clone()])));
    assert!(view.update_in(cx, |v, _w, _cx| v.drag_out_of(studio.key, &shared)));
    while calls.try_recv().is_ok() {}
    let body = view.read_with(cx, |v, _| v.tile_bounds(tile)).expect("drawn").center();
    let dir = tempfile::tempdir().unwrap();
    let here = dir.path().join("here.txt");
    std::fs::write(&here, b"here").unwrap();
    let board = Memory::default();
    board.copy_files(&[&slopty_client::dnd::file_url(&here).unwrap()]);
    let over = |state: &mut DropIn, own, cx: &mut VisualTestContext| {
        view.update_in(cx, |v, _w, cx| v.drag_over(state, body, carried(&board, own), cx))
    };

    let mut state = DropIn::default();
    assert_eq!(over(&mut state, Some(1), cx), Over::Remote(DragOp::Copy));
    let steps = drag_steps(studio.drain());
    let [DragInput::Enter { items, .. }] = steps.as_slice() else { panic!("{steps:?}") };
    assert_eq!(items, &[own], "the worker's own file, by its path there");
    assert!(calls.try_recv().is_err(), "nothing goes up");
    let taken = view.update_in(cx, |v, _w, cx| v.drag_dropped(&mut state, body, 1, cx));
    assert_eq!(taken, Taken::AsIs, "no promise called in");
    let steps = drag_steps(studio.drain());
    assert!(matches!(steps.as_slice(), [DragInput::Drop { promised, .. }] if promised.is_empty()));

    let mut other = DropIn::default();
    assert_eq!(over(&mut other, Some(7), cx), Over::Remote(DragOp::Copy), "a drag of no tag here");
    let steps = drag_steps(studio.drain());
    let Some(DragInput::Enter { items, .. }) = steps.last() else { panic!("{steps:?}") };
    let named = items[0].file.as_ref().map(|f| (f.name.as_str(), f.path.is_none()));
    assert_eq!(named, Some(("here.txt", true)), "this Mac's file, to go up");
    assert!(matches!(calls.try_recv(), Ok(Call::Upload(..))), "and it goes up");
}

/// Every transfer in flight is on the status bar's list, both ways: an upload with how far it
/// got, its rate and its time left, a download as it begins; the bar counts them together, and
/// a transfer's stop on the list stops it. The list closes once nothing is left in it.
#[cfg(target_os = "macos")]
#[gpui::test]
fn the_transfers_list_shows_both_ways_and_stops_one(cx: &mut TestAppContext) {
    use crate::workspace::remote::Bringing;
    use crate::workspace::remote::transfers::Down;
    let (view, cx) = workspace(cx);
    let (studio, mut calls, _board) = connect_remote(&view, cx);
    let key = studio.key;
    let shell = SessionId::new();
    let tile = opens(&view, cx, &studio, shell, studio.me, 1);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("big.bin");
    std::fs::write(&file, vec![b'x'; 1000]).unwrap();
    view.update_in(cx, |v, _window, cx| v.drop_files(tile, std::slice::from_ref(&file), cx));
    let Call::Upload(up, ..) = calls.try_recv().expect("an upload") else { panic!("upload") };
    view.update_in(cx, |v, _window, cx| {
        v.xfer_message(XferMsg::Progress { xfer: up, done: 420 }, cx);
    });
    cx.executor().advance_clock(Duration::from_secs(2));
    view.update_in(cx, |v, _window, cx| {
        v.xfer_message(XferMsg::Progress { xfer: up, done: 820 }, cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("status-transfers").is_none(), "the focused tile's own says it");

    let down = XferId::new();
    let dest = dir.path().join("out.txt");
    let rows = view.update_in(cx, |v, _window, cx| {
        let source = "~/out.txt".to_owned();
        let versions = slopty_client::xfer::Versions::new();
        let asked = Down { worker: key, xfer: down, source, dest: dest.clone(), versions };
        v.bring_down(asked, Bringing::Download, cx);
        v.transfer_rows(cx.background_executor().now())
    });
    let said: Vec<(bool, &str, &str, &str)> = rows
        .iter()
        .map(|r| (r.up, r.name.as_str(), r.machine.as_str(), r.words.as_str()))
        .collect();
    assert_eq!(
        said,
        [
            (true, "big.bin", "studio", "82% \u{b7} 410 B/s \u{b7} 1 s left"),
            (false, "out.txt", "studio", "Starting"),
        ]
    );
    assert_eq!(rows.first().and_then(|r| r.fraction), Some(0.82));
    cx.run_until_parked();
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "~/out.txt", "it landed");
    let notice = view.read_with(cx, |v, _| v.toast_text()).unwrap_or_default();
    assert!(notice.starts_with("Downloaded ") && notice.ends_with("out.txt"), "{notice}");

    // Another tile focused: the bar counts the upload, and its list stops it.
    let other = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    view.update_in(cx, |v, _window, cx| v.focus_tile(other, cx));
    cx.run_until_parked();
    let button = cx.debug_bounds("status-transfers").expect("the bar counts it");
    let listed = view.read_with(cx, |v, cx| v.transfer_rows(cx.background_executor().now()).len());
    assert_eq!(listed, 1, "the download landed: only the upload is left");
    cx.simulate_click(button.center(), Modifiers::none());
    cx.run_until_parked();
    let row = cx.debug_bounds(format!("transfers-row-{up}").leak()).expect("listed");
    cx.simulate_mouse_move(row.center(), None, Modifiers::none());
    cx.run_until_parked();
    let stop = cx.debug_bounds(format!("transfers-cancel-{up}").leak()).expect("its stop");
    cx.simulate_click(stop.center(), Modifiers::none());
    cx.run_until_parked();
    assert!(matches!(calls.try_recv(), Ok(Call::Cancel(x)) if x == up), "the upload stops");
    assert!(cx.debug_bounds("transfers").is_none(), "nothing left: the list closes");
    assert!(cx.debug_bounds("status-transfers").is_none());
}

/// Transfers in flight are kept in the ledger and taken up at the next launch: listed as
/// waiting for their machine until it links, then an upload goes on from what the worker holds
/// and a download to where it was going; each leaves the ledger as it ends, and the file goes
/// with the last.
#[cfg(target_os = "macos")]
#[gpui::test]
fn transfers_in_flight_are_taken_up_at_the_next_launch(cx: &mut TestAppContext) {
    use slopty_client::xfer::ledger::{Kept, Ledger, Way};
    let (view, cx) = workspace(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(slopty_client::xfer::ledger::FILE);
    let file = dir.path().join("a.txt");
    std::fs::write(&file, b"abc").unwrap();
    let key = WorkerKey::new(7);
    let (up, down) = (XferId::new(), XferId::new());
    let shell = SessionId::new();
    let tile = TileRef { worker: key, item: ItemId::new() };
    let dest = dir.path().join("out.txt");
    let left = Ledger {
        kept: vec![
            Kept {
                xfer: up,
                worker: key,
                way: Way::Up {
                    tile,
                    files: vec![file.clone()],
                    dest: Dest::SessionCwd(shell),
                    total: 3,
                    scratch: None,
                },
            },
            Kept {
                xfer: down,
                worker: key,
                way: Way::Down {
                    source: "~/out.txt".to_owned(),
                    dest: dest.clone(),
                    versions: slopty_client::xfer::Versions::new(),
                },
            },
        ],
    };
    left.save(&path).unwrap();

    view.update_in(cx, |v, _window, cx| v.set_transfer_ledger(path.clone(), cx));
    cx.run_until_parked();
    let rows = view.update_in(cx, |v, _w, cx| v.transfer_rows(cx.background_executor().now()));
    let words: Vec<&str> = rows.iter().map(|r| r.words.as_str()).collect();
    assert_eq!(words, ["Waiting for the machine", "Waiting for the machine"]);

    let (studio, mut calls, _board) = connect_remote(&view, cx);
    assert_eq!(studio.key, key);
    cx.run_until_parked();
    let Ok(Call::UploadAgain(again, files, to)) = calls.try_recv() else { panic!("taken up") };
    assert_eq!((again, files, to), (up, vec![file.clone()], Dest::SessionCwd(shell)));
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "~/out.txt", "the download landed");
    assert_eq!(Ledger::load(&path).kept.iter().map(|k| k.xfer).collect::<Vec<_>>(), [up]);

    let paths = vec!["/Users/w/a.txt".to_owned()];
    view.update_in(cx, |v, _window, cx| v.xfer_message(XferMsg::Finished { xfer: up, paths }, cx));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.toast_text()).as_deref(), Some("a.txt reached studio"));
    assert!(!path.exists(), "nothing in flight: the ledger goes");

    // A drop on a shell is kept as it starts; a paste or a drag would not be.
    let tile = opens(&view, cx, &studio, shell, studio.me, 1);
    view.update_in(cx, |v, _window, cx| v.drop_files(tile, std::slice::from_ref(&file), cx));
    let Ok(Call::Upload(fresh, ..)) = calls.try_recv() else { panic!("an upload") };
    cx.run_until_parked();
    assert_eq!(Ledger::load(&path).kept.iter().map(|k| k.xfer).collect::<Vec<_>>(), [fresh]);
}
