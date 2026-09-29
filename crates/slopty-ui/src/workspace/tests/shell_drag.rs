//! A path a shell printed, lifted out to another app by a held touch, as a folder row is.

use std::path::PathBuf;

use slopty_client::clip::Fetched;
use slopty_client::remote::Remote;
use slopty_client::xfer::XferError;
use slopty_core::XferId;
use slopty_proto::transfer::{Dest, RepRef};

use super::*;

/// A worker whose every download is a file named as the path's last part, holding the path.
#[derive(Debug)]
struct Echoes;

impl Remote for Echoes {
    fn upload(&self, _xfer: XferId, _files: Vec<PathBuf>, _dest: Dest) {}

    fn cancel(&self, _xfer: XferId) {}

    fn download(&self, path: String, into: PathBuf) -> Result<Vec<PathBuf>, XferError> {
        let file = into
            .join(path.rsplit('/').next().ok_or_else(|| XferError::Worker("no name".to_owned()))?);
        std::fs::write(&file, &path).map_err(|e| XferError::Worker(e.to_string()))?;
        Ok(vec![file])
    }

    fn clip_fetch(&self, _rep: &RepRef, _max: Option<u64>, _wait: Duration) -> Fetched {
        Fetched::Gone
    }

    fn send_clip(&self, _rep: RepRef, _answer: Fetched, _urgent: bool) {}

    fn forward(&self, _port: u16) -> Option<u16> {
        None
    }
}

/// A touch held on a path a shell printed offers the worker's file, made absolute against the
/// shell's directory, a path printed with a trailing `/` as a folder; plain text offers
/// nothing, and neither does the path while the palette is open.
#[gpui::test]
fn a_path_a_shell_printed_is_offered_under_a_held_touch(cx: &mut TestAppContext) {
    use slopty_platform::file_drop::out::FOLDER_UTI;
    let (view, cx) = workspace(cx);
    let (tx, rx) = mpsc::channel(256);
    let (me, key) = (ClientId::new(), WorkerKey::new(5));
    let factory: ScreenFactory =
        Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
    view.update_in(cx, |v, _window, cx| {
        v.add_worker(key, "studio".to_owned(), cx);
        let link = WorkerLink { me, out: tx, open_screen: factory, remote: Some(Arc::new(Echoes)) };
        v.connect_worker(key, link, hello("studio", Vec::new()), cx);
        v.apply_sync(key, ItemSync::Snapshot { version: 0, items: Vec::new() }, cx);
    });
    cx.run_until_parked();
    let studio = Fake { key, me, rx };
    let session = SessionId::new();
    opens_in(&view, cx, &studio, session, me, 1, Some("/w/app"));
    let lines = [
        "error in src/main.rs:12:5 here",
        "built into target/debug/",
        "nothing to see",
        "sourced ~/.zshrc",
    ];
    view.update_in(cx, |v, _w, cx| v.term_event(session, frame(&lines), cx));
    cx.run_until_parked();
    let metrics = view
        .read_with(cx, |v, cx| v.terminal(session).and_then(|t| t.read(cx).metrics()))
        .expect("the shell was laid out");
    let cell = |col: u16, row: u16| {
        point(
            metrics.origin.x + metrics.cell_width * (f32::from(col) + 0.5),
            metrics.origin.y + metrics.line_height * (f32::from(row) + 0.5),
        )
    };
    let offers = |cx: &mut VisualTestContext, at| view.read_with(cx, |v, cx| v.drag_offers(at, cx));

    let file = offers(cx, cell(12, 0));
    let [file] = file.as_slice() else { panic!("one offer: {file:?}") };
    assert_eq!((file.name.as_str(), file.folder), ("main.rs", false));
    let tmp = tempfile::tempdir().unwrap();
    let landed = file.fetch_under(tmp.path()).unwrap();
    assert_eq!(std::fs::read_to_string(landed).unwrap(), "/w/app/src/main.rs", "made absolute");

    let folder = offers(cx, cell(14, 1));
    let [folder] = folder.as_slice() else { panic!("one offer: {folder:?}") };
    assert_eq!((folder.name.as_str(), folder.type_identifier()), ("debug", FOLDER_UTI));

    let home = offers(cx, cell(10, 3));
    let [home] = home.as_slice() else { panic!("one offer: {home:?}") };
    let landed = home.fetch_under(tmp.path()).unwrap();
    assert_eq!(std::fs::read_to_string(landed).unwrap(), "~/.zshrc", "left for the worker");

    assert!(offers(cx, cell(2, 2)).is_empty(), "plain text is no file");
    view.update_in(cx, |v, window, cx| v.open_palette(&OpenPalette, window, cx));
    cx.run_until_parked();
    assert!(offers(cx, cell(12, 0)).is_empty(), "the palette is over the tiles");
}
