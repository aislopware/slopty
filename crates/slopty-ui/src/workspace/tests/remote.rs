//! The clipboard, files and ports in the headless workspace: what is sent to the worker when,
//! and what the tile shows.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::{ExternalPaths, FileDropEvent};
use slopty_client::remote::Remote;
use slopty_client::tunnel::Forward;
use slopty_core::XferId;
use slopty_platform::pasteboard::{Memory, Pasteboard, TEXT_UTI};
use slopty_proto::orchestration::Port;
use slopty_proto::transfer::{ClipItem, ClipMsg, Dest, Offer, Peer, XferMsg};

use super::*;

/// What the workspace asked of a worker's [`Remote`].
#[derive(Debug)]
enum Call {
    Upload(XferId, Vec<PathBuf>, Dest),
    Cancel(XferId),
    SendClip(u64, String, Vec<u8>),
    Forward(u16),
}

/// Records the calls; serves a worker port here `offset` ports up, as a client whose ports
/// are partly taken would. Its copied files are [`WORKER_FILES`], and a download of one
/// writes a file of that name whose text is the path.
#[derive(Debug)]
struct Recorder(mpsc::UnboundedSender<Call>, u16);

/// The `public.file-url` of the files a worker copied.
const WORKER_FILES: &[u8] = b"file:///Users/w/a%20b.txt\nfile:///Users/w/c.txt";

impl Remote for Recorder {
    fn upload(&self, xfer: XferId, files: Vec<PathBuf>, dest: Dest) {
        self.0.send(Call::Upload(xfer, files, dest)).unwrap();
    }

    fn cancel(&self, xfer: XferId) {
        self.0.send(Call::Cancel(xfer)).unwrap();
    }

    fn download(&self, path: String, into: PathBuf) -> Result<Vec<PathBuf>, String> {
        let name = path.rsplit('/').next().ok_or("no name")?;
        let file = into.join(name);
        std::fs::write(&file, &path).map_err(|e| e.to_string())?;
        Ok(vec![file])
    }

    fn clip_data(&self, _generation: u64, uti: &str, _wait: Duration) -> Option<Vec<u8>> {
        match uti {
            "public.png" => Some(b"PNG".to_vec()),
            "public.file-url" => Some(WORKER_FILES.to_vec()),
            _ => None,
        }
    }

    fn send_clip(&self, generation: u64, uti: String, bytes: Vec<u8>) {
        self.0.send(Call::SendClip(generation, uti, bytes)).unwrap();
    }

    fn forward(&self, port: u16) -> Option<u16> {
        self.0.send(Call::Forward(port)).unwrap();
        port.checked_add(self.1)
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
        v.connect_worker(key, name.to_owned(), link, Vec::new(), cx);
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
            ClientMsg::Term { session, req: TermRequest::Paste(text) } if session == shell => {
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
    Offer {
        origin: Peer::Worker(slopty_core::WorkerId::new()),
        generation,
        items: vec![ClipItem {
            uti: "public.file-url".to_owned(),
            size: WORKER_FILES.len() as u64,
            hash: crate::clipboard::digest(WORKER_FILES),
            inline: None,
        }],
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
    assert_eq!(offer.items.len(), 1);
    assert_eq!(offer.items[0].uti, "public.file-url", "the URLs alone");

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
    view.update_in(cx, |v, _window, _cx| {
        v.clip_message(key, ClipMsg::Offer(worker_copied_files(5)));
    });
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
    assert_eq!(view.read_with(cx, WorkspaceView::toast_text), None, "a paste says nothing");

    let on_worker = ClipFiles::Worker { worker: studio.key, generation: 2 };
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
    assert_eq!(offer[0].items[0].inline.as_deref(), Some(&b"copied here"[..]));
    assert_eq!(offer[0].origin, Peer::Client(studio.me));

    view.update_in(cx, |v, window, cx| v.new_note(&NewNote, window, cx));
    cx.run_until_parked();
    let sent = studio.drain();
    assert_eq!(watches(&sent), [false], "a note is not the worker's");

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
    assert_eq!(offer.items[0].inline.as_deref(), Some(&b"copied on the phone"[..]));
    assert!(board.reads() > 0, "read for the paste");
}

/// The worker's announcement lands here as text and promises; its fetch of this client's
/// offer is answered with the bytes, and a fetch of a stale offer with `Unavailable`.
#[gpui::test]
fn offers_land_as_promises_and_fetches_are_answered(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (mut studio, mut calls, board) = connect_remote(&view, cx);
    let offer = Offer {
        origin: Peer::Worker(slopty_core::WorkerId::new()),
        generation: 3,
        items: vec![
            ClipItem {
                uti: "public.png".to_owned(),
                size: 3,
                hash: crate::clipboard::digest(b"PNG"),
                inline: None,
            },
            ClipItem {
                uti: TEXT_UTI.to_owned(),
                size: 2,
                hash: crate::clipboard::digest(b"hi"),
                inline: Some(b"hi".to_vec()),
            },
        ],
    };
    let key = studio.key;
    view.update_in(cx, |v, _window, _cx| v.clip_message(key, ClipMsg::Offer(offer)));
    assert_eq!(board.data(TEXT_UTI).as_deref(), Some(&b"hi"[..]));
    assert_eq!(board.data("public.png").as_deref(), Some(&b"PNG"[..]), "fetched on paste");

    board.copy(&[(TEXT_UTI, b"mine")]);
    let shell = SessionId::new();
    opens(&view, cx, &studio, shell, studio.me, 1);
    let mine = offers(&studio.drain()).pop().expect("announced on focus");
    let fetch = ClipMsg::Fetch { generation: mine.generation, uti: TEXT_UTI.to_owned() };
    view.update_in(cx, |v, _window, _cx| v.clip_message(key, fetch));
    match calls.try_recv().unwrap() {
        Call::SendClip(generation, uti, bytes) => {
            assert_eq!(
                (generation, uti.as_str(), bytes.as_slice()),
                (mine.generation, TEXT_UTI, &b"mine"[..])
            );
        }
        other => panic!("{other:?}"),
    }
    let stale =
        ClipMsg::Fetch { generation: mine.generation.wrapping_add(5), uti: TEXT_UTI.to_owned() };
    view.update_in(cx, |v, _window, _cx| v.clip_message(key, stale));
    let sent = studio.drain();
    assert!(matches!(sent.as_slice(), [ClientMsg::Clip(ClipMsg::Unavailable { .. })]), "{sent:?}");
}

/// A worker link that serves one clipboard representation and counts the fetches.
#[derive(Debug)]
struct Serves(&'static [u8], Arc<std::sync::atomic::AtomicUsize>);

impl Remote for Serves {
    fn upload(&self, _xfer: XferId, _files: Vec<PathBuf>, _dest: Dest) {}

    fn cancel(&self, _xfer: XferId) {}

    fn download(&self, _path: String, _into: PathBuf) -> Result<Vec<PathBuf>, String> {
        Err("clipboard only".to_owned())
    }

    fn clip_data(&self, _generation: u64, _uti: &str, _wait: Duration) -> Option<Vec<u8>> {
        self.1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(self.0.to_vec())
    }

    fn send_clip(&self, _generation: u64, _uti: String, _bytes: Vec<u8>) {}

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
    let offer = Offer {
        origin: Peer::Worker(slopty_core::WorkerId::new()),
        generation: 3,
        items: vec![ClipItem {
            uti: "public.png".to_owned(),
            size: 3,
            hash: crate::clipboard::digest(b"PNG"),
            inline: None,
        }],
    };
    view.update_in(cx, |v, _window, _cx| v.clip_message(key, ClipMsg::Offer(offer)));
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
            v.connect_worker(key, "studio".to_owned(), link, Vec::new(), cx);
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

    let paths = vec!["/Users/me/work/my notes.txt".to_owned(), "/tmp/b".to_owned()];
    view.update_in(cx, |v, _window, cx| v.xfer_message(XferMsg::Finished { xfer, paths }, cx));
    cx.run_until_parked();
    let pasted: Vec<String> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Term { session, req: TermRequest::Paste(text) } if session == shell => {
                Some(text)
            }
            _ => None,
        })
        .collect();
    assert_eq!(pasted, ["'/Users/me/work/my notes.txt' /tmp/b "], "one bracketable paste");
    assert!(cx.debug_bounds(selector("upload", tile.item)).is_none(), "done: the progress goes");

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

/// A drop on a note sends nothing; a drop on a remote window goes to the worker's staging.
#[gpui::test]
fn a_drop_goes_where_the_tile_can_take_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (studio, mut calls, _board) = connect_remote(&view, cx);
    let note = arrives(&view, cx, &studio, ItemKind::Note { text: "n".to_owned() }, 1);
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

/// A shell's listening ports show as quiet chips on its tile; one served on another port here
/// says so on the chip and in a notice; the palette lists them.
#[gpui::test]
fn forwarded_ports_show_on_the_shell_tile(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (studio, _calls, _board) = connect_remote(&view, cx);
    let shell = SessionId::new();
    let tile = opens(&view, cx, &studio, shell, studio.me, 1);
    let port = |number| Port { number, pid: 2, process: "vite".to_owned(), session: Some(shell) };
    let forwards = vec![
        Forward { port: port(5173), local: Some(5173) },
        Forward { port: port(8080), local: Some(8081) },
    ];
    view.update_in(cx, |v, _window, cx| v.ports_changed(shell, forwards, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("port-5173", tile.item)).is_some(), "a chip per port");
    assert!(cx.debug_bounds(selector("port-8080", tile.item)).is_some());
    assert!(cx.debug_bounds(selector("port-out-5173", tile.item)).is_some(), "and its arrow");
    let notice = view.read_with(cx, WorkspaceView::toast_text);
    assert_eq!(notice.as_deref(), Some("Port 8080 is taken here; forwarded on 8081"));
    // The number opens the page in a tile on the shell's worker, named by the worker's port:
    // each client serves it where it can.
    let mut studio = studio;
    studio.drain();
    let chip = cx.debug_bounds(selector("port-8080", tile.item)).unwrap();
    cx.simulate_click(chip.center(), Modifiers::none());
    cx.run_until_parked();
    let opened: Vec<String> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Items(ItemOp::Upsert(Item { kind: ItemKind::Browser { url }, .. })) => {
                Some(url)
            }
            _ => None,
        })
        .collect();
    assert_eq!(opened, ["http://localhost:8080/"]);
    view.update_in(cx, |v, _window, cx| v.ports_changed(shell, Vec::new(), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("port-5173", tile.item)).is_none(), "gone with the server");
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
        sleeping: false,
        name: None,
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
