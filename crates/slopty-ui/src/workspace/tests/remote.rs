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
/// are partly taken would.
#[derive(Debug)]
struct Recorder(mpsc::UnboundedSender<Call>, u16);

impl Remote for Recorder {
    fn upload(&self, xfer: XferId, files: Vec<PathBuf>, dest: Dest) {
        self.0.send(Call::Upload(xfer, files, dest)).unwrap();
    }

    fn cancel(&self, xfer: XferId) {
        self.0.send(Call::Cancel(xfer)).unwrap();
    }

    fn download(&self, _path: String, _into: PathBuf) -> Result<Vec<PathBuf>, String> {
        Err("not in this test".to_owned())
    }

    fn clip_data(&self, _generation: u64, uti: &str, _wait: Duration) -> Option<Vec<u8>> {
        (uti == "public.png").then(|| b"PNG".to_vec())
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
    let (tx, rx) = mpsc::channel(256);
    let (calls, recorded) = mpsc::unbounded_channel();
    let me = ClientId::new();
    let key = WorkerKey::new(7);
    let factory: ScreenFactory =
        Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
    let board = Rc::new(Memory::default());
    let shared: Rc<dyn Pasteboard> = Rc::<Memory>::clone(&board);
    view.update_in(cx, |v, _window, cx| {
        v.set_pasteboard(shared);
        v.add_worker(key, "studio".to_owned(), cx);
        let remote: Arc<dyn Remote> = Arc::new(Recorder(calls, offset));
        let link = WorkerLink { me, out: tx, open_screen: factory, remote: Some(remote) };
        v.connect_worker(key, "studio".to_owned(), link, Vec::new(), cx);
        v.apply_sync(key, ItemSync::Snapshot { version: 0, items: Vec::new() }, cx);
    });
    cx.run_until_parked();
    let mut fake = Fake { key, me, rx };
    fake.drain();
    (fake, recorded, board)
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
    let Some(ClientMsg::Clip(ClipMsg::Offer(offer))) = hook() else { panic!("an offer to paste") };
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
            ClipItem { uti: "public.png".to_owned(), size: 3, hash: [2; 32], inline: None },
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
