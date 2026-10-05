//! Folder tiles in the headless workspace: what they ask the worker for, the keys that walk
//! them, a file opened beside one, a path opened as what it is, a drop that goes up into one,
//! and the iOS ways out and in: a row lifted out to another app, and the Files picker.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{ExternalPaths, FileDropEvent};
use slopty_client::clip::Fetched;
use slopty_client::remote::Remote;
use slopty_client::xfer::XferError;
use slopty_core::{WallMs, XferId};
use slopty_proto::folder::{FolderEntry, Listing};
use slopty_proto::orchestration::FileKind;
use slopty_proto::transfer::{Dest, RepRef, XferMsg};

use super::*;

fn entry(name: &str, kind: FileKind) -> FolderEntry {
    FolderEntry {
        name: name.to_owned(),
        kind,
        link: false,
        hidden: name.starts_with('.'),
        size: 120,
        items: (kind == FileKind::Dir).then_some(2),
        modified_ms: WallMs::from_millis(1_700_000_000_000),
    }
}

fn listed(dir: &str, entries: Vec<FolderEntry>) -> Listing {
    let total = u32::try_from(entries.len()).unwrap();
    Listing::Listed { dir: dir.to_owned(), entries, total }
}

/// The paths the workspace asked `fake` to list.
fn asked(fake: &mut Fake) -> Vec<String> {
    fake.drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::ListFolder { path } => Some(path),
            _ => None,
        })
        .collect()
}

fn answer(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &Fake,
    path: &str,
    listing: &Listing,
) {
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| v.folder_listed(key, path, listing, cx));
    cx.run_until_parked();
}

fn folder_path(view: &Entity<WorkspaceView>, cx: &VisualTestContext, tile: TileRef) -> String {
    view.read_with(cx, |v, _| match v.item(tile).map(|i| &i.kind) {
        Some(ItemKind::Folder { path }) => path.clone(),
        other => panic!("not a folder: {other:?}"),
    })
}

fn selected(view: &Entity<WorkspaceView>, cx: &VisualTestContext, tile: TileRef) -> String {
    view.read_with(cx, |v, cx| {
        v.folder(tile.item).and_then(|f| f.read(cx).selected().map(|e| e.name.clone()))
    })
    .unwrap_or_default()
}

/// A folder tile asks for its listing, takes the keyboard when focused, walks its rows by key
/// (folders first), browses into a folder with ↩ (the item follows, the worker is asked
/// again), goes back up with ⌫ to the folder it left, and opens a file with ↩ as a file tile
/// right of it.
#[gpui::test]
fn a_folder_tile_is_keyed_through_and_opens_a_file_beside_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let tile = arrives(&view, cx, &studio, ItemKind::Folder { path: "/w/proj".into() }, 1);
    assert_eq!(asked(&mut studio), ["/w/proj"], "the tile asks for what is in it");
    let nodes = tree(cx);
    assert!(nodes.iter().any(|n| n.is("Heading", Some("folder proj"))), "{nodes:#?}");
    let top = listed(
        "/w/proj",
        vec![
            entry("src", FileKind::Dir),
            entry(".env", FileKind::File),
            entry("README.md", FileKind::File),
        ],
    );
    answer(&view, cx, &studio, "/w/proj", &top);
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    let keyed = cx.update(|window, cx| {
        view.read(cx).folder(tile.item).is_some_and(|f| f.read(cx).focused(window))
    });
    assert!(keyed, "the folder has the keyboard");
    assert_eq!(asked(&mut studio), ["/w/proj"], "and looks again when focused");
    let nodes = tree(cx);
    for name in ["src", ".env", "README.md"] {
        assert!(nodes.iter().any(|n| n.is("ListBoxOption", Some(name))), "{name}: {nodes:#?}");
    }
    assert_eq!(selected(&view, cx, tile), "src", "the first row is selected");

    cx.simulate_keystrokes("down down up");
    assert_eq!(selected(&view, cx, tile), ".env");
    cx.simulate_keystrokes("up enter");
    cx.run_until_parked();
    assert_eq!(folder_path(&view, cx, tile), "/w/proj/src", "the item moved into the folder");
    let sent = studio.drain();
    let moved = ItemOp::SetFolder { id: tile.item, path: "/w/proj/src".into() };
    assert!(sent.contains(&ClientMsg::Items(moved)), "{sent:#?}");
    assert!(sent.contains(&ClientMsg::ListFolder { path: "/w/proj/src".into() }), "{sent:#?}");

    let src = listed(
        "/w/proj/src",
        vec![entry("lib.rs", FileKind::File), entry("main.rs", FileKind::File)],
    );
    answer(&view, cx, &studio, "/w/proj/src", &src);
    cx.simulate_keystrokes("backspace");
    cx.run_until_parked();
    assert_eq!(folder_path(&view, cx, tile), "/w/proj", "⌫ goes up");
    assert_eq!(asked(&mut studio), ["/w/proj"]);
    answer(&view, cx, &studio, "/w/proj", &top);
    assert_eq!(selected(&view, cx, tile), "src", "on the folder it came from");

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    answer(&view, cx, &studio, "/w/proj/src", &src);
    studio.drain();
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    let opened: Vec<String> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Items(ItemOp::Add(Item { kind: ItemKind::File { path }, .. })) => Some(path),
            _ => None,
        })
        .collect();
    assert_eq!(opened, ["/w/proj/src/main.rs"], "a file tile for the selected file");
    let file = focused(&view, cx).expect("the file tile is focused");
    let (file_at, folder_at) =
        view.read_with(cx, |v, _| (v.layout().position(file), v.layout().position(tile)));
    let (file_at, folder_at) = (file_at.unwrap(), folder_at.unwrap());
    assert_eq!(file_at.column.checked_sub(folder_at.column), Some(1), "right of the folder");
    assert_eq!(folder_path(&view, cx, tile), "/w/proj/src", "the folder stays where it was");
}

/// A click opens a row as ↩ does; the header's arrow goes up; and at the root there is no way
/// up to offer.
#[gpui::test]
fn a_click_opens_a_row_and_the_header_goes_up(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let tile = arrives(&view, cx, &studio, ItemKind::Folder { path: "/w".into() }, 1);
    let top = listed("/w", vec![entry("docs", FileKind::Dir), entry("a.txt", FileKind::File)]);
    answer(&view, cx, &studio, "/w", &top);
    studio.drain();
    let row = cx.debug_bounds("folder-row-0").expect("the first row is drawn");
    cx.simulate_click(row.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(folder_path(&view, cx, tile), "/w/docs", "a click on a folder browses into it");
    answer(&view, cx, &studio, "/w/docs", &listed("/w/docs", Vec::new()));
    let nodes = tree(cx);
    assert!(nodes.iter().any(|n| n.is("Button", Some("Enclosing folder"))), "{nodes:#?}");
    let up = cx.debug_bounds(selector("up", tile.item)).expect("the way up");
    cx.simulate_click(up.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(folder_path(&view, cx, tile), "/w");

    let root = arrives(&view, cx, &studio, ItemKind::Folder { path: "/".into() }, 2);
    answer(&view, cx, &studio, "/", &listed("/", vec![entry("w", FileKind::Dir)]));
    view.update_in(cx, |v, _w, cx| v.focus_tile(root, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("up", root.item)).is_none(), "nothing above the root");
}

/// A path whose kind the workspace cannot tell is asked of the worker as a folder first: a
/// listing opens a folder tile, a file there a file tile. A path that says it is a folder opens
/// one at once, and one that names a line opens as a file.
#[gpui::test]
fn a_path_opens_as_what_it_is(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let kinds = |view: &Entity<WorkspaceView>, cx: &VisualTestContext| {
        view.read_with(cx, |v, _| v.items().map(|(_, i)| i.kind.clone()).collect::<Vec<_>>())
    };
    view.update_in(cx, |v, _w, cx| v.open_path_on(key, "/w/proj", None, cx));
    cx.run_until_parked();
    assert_eq!(asked(&mut studio), ["/w/proj"], "asked, nothing opened yet");
    assert_eq!(kinds(&view, cx), Vec::<ItemKind>::new());
    answer(&view, cx, &studio, "/w/proj", &listed("/w/proj", vec![entry("a", FileKind::File)]));
    assert_eq!(kinds(&view, cx), [ItemKind::Folder { path: "/w/proj".into() }]);
    assert!(asked(&mut studio).is_empty(), "the answer at hand fills the new tile");

    view.update_in(cx, |v, _w, cx| v.open_path_on(key, "/w/proj/a", None, cx));
    cx.run_until_parked();
    assert_eq!(asked(&mut studio), ["/w/proj/a"]);
    answer(&view, cx, &studio, "/w/proj/a", &Listing::NotFolder);
    assert!(kinds(&view, cx).contains(&ItemKind::File { path: "/w/proj/a".into() }));

    view.update_in(cx, |v, _w, cx| {
        v.open_path_on(key, "/w/other/", None, cx);
        v.open_path_on(key, "/w/proj/b.rs", Some(3), cx);
    });
    cx.run_until_parked();
    let kinds = kinds(&view, cx);
    assert!(kinds.contains(&ItemKind::Folder { path: "/w/other".into() }), "{kinds:?}");
    assert!(kinds.contains(&ItemKind::File { path: "/w/proj/b.rs".into() }), "{kinds:?}");
}

/// A file tile whose machine went away before the text came says it opens when the machine
/// is back, not that it is still reading; the read the next link brings fills it.
#[gpui::test]
fn a_file_whose_machine_went_away_unread_says_it_waits_for_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.open_file_on(Some(key), "/w/notes.md", None, cx));
    cx.run_until_parked();
    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("lost".into()), cx);
    });
    cx.run_until_parked();
    let said = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, cx| {
            v.files.values().map(|f| f.read(cx).summary(cx)).collect::<Vec<_>>()
        })
    };
    assert_eq!(said(cx), [crate::file::OPENS_WHEN_BACK]);

    let read = slopty_proto::file::FileRead::Text {
        text: "# Notes".to_owned(),
        size: 8,
        modified_ms: WallMs::from_millis(1_000),
        final_newline: true,
        editorconfig: Vec::new(),
    };
    view.update_in(cx, |v, _w, cx| v.file_read(key, "/w/notes.md", &read, cx));
    cx.run_until_parked();
    let now = said(cx);
    assert!(now.first().is_some_and(|s| s.starts_with("1 line")), "{now:?}");
}

/// Records the uploads the workspace starts.
#[derive(Debug)]
struct Uploads(mpsc::UnboundedSender<(XferId, Dest)>);

impl Remote for Uploads {
    fn upload(&self, xfer: XferId, _files: Vec<PathBuf>, dest: Dest, _again: bool) {
        self.0.send((xfer, dest)).unwrap();
    }

    fn cancel(&self, _xfer: XferId) {}

    /// Writes a file named as the path's last part, whose text is the path.
    fn download(&self, ask: slopty_client::xfer::Download) -> Result<Vec<PathBuf>, XferError> {
        let (path, into) = (ask.path, ask.into);
        let name =
            path.rsplit('/').next().ok_or_else(|| XferError::Worker("no name".to_owned()))?;
        let file = into.join(name);
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

/// Worker "studio" linked with an [`Uploads`] remote: the fake, and the uploads it is asked for.
fn link_uploads(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
) -> (Fake, mpsc::UnboundedReceiver<(XferId, Dest)>) {
    let (tx, rx) = mpsc::channel(256);
    let (calls, uploads) = mpsc::unbounded_channel();
    let key = WorkerKey::new(3);
    let me = ClientId::new();
    view.update_in(cx, |v, _window, cx| {
        v.add_worker(key, "studio".to_owned(), cx);
        let remote: Arc<dyn Remote> = Arc::new(Uploads(calls));
        let factory: ScreenFactory =
            Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
        let link = WorkerLink { me, out: tx, open_screen: factory, remote: Some(remote) };
        v.connect_worker(key, link, hello("studio", Vec::new()), cx);
        v.apply_sync(key, ItemSync::Snapshot { version: 0, items: Vec::new() }, cx);
    });
    cx.run_until_parked();
    (Fake { key, me, rx }, uploads)
}

/// A touch held on a folder tile's row offers that row to another app as the worker's file,
/// a folder as a folder; the path bar offers nothing. The offer brings the file down only
/// when it is asked for, into the directory it is given.
#[gpui::test]
fn a_row_under_a_held_touch_is_offered_as_the_workers_file(cx: &mut TestAppContext) {
    use slopty_platform::file_drop::out::{DATA_UTI, FOLDER_UTI};
    let (view, cx) = workspace(cx);
    let (studio, _uploads) = link_uploads(&view, cx);
    let tile = arrives(&view, cx, &studio, ItemKind::Folder { path: "/w/proj".into() }, 1);
    let rows = vec![entry("src", FileKind::Dir), entry("a.txt", FileKind::File)];
    answer(&view, cx, &studio, "/w/proj", &listed("/w/proj", rows));
    let offers = |cx: &mut VisualTestContext, selector: &'static str| {
        let at = cx.debug_bounds(selector).expect("drawn").center();
        view.read_with(cx, |v, cx| v.drag_offers(at, cx))
    };
    let file = offers(cx, "folder-row-1");
    let [file] = file.as_slice() else { panic!("one offer: {file:?}") };
    assert_eq!((file.name.as_str(), file.type_identifier()), ("a.txt", DATA_UTI));
    let folder = offers(cx, "folder-row-0");
    assert_eq!(folder.first().map(|o| (o.folder, o.type_identifier())), Some((true, FOLDER_UTI)));
    assert!(offers(cx, "folder-crumb-0").is_empty(), "the path bar is not a file");
    let tile_at = view.read_with(cx, |v, _| v.tile_bounds(tile)).expect("drawn");
    let below = point(tile_at.center().x, tile_at.bottom() - px(2.0));
    assert!(view.read_with(cx, |v, cx| v.drag_offers(below, cx)).is_empty(), "under the rows");

    let tmp = tempfile::tempdir().unwrap();
    let landed = file.fetch_under(tmp.path()).unwrap();
    assert_eq!(landed.file_name(), Some("a.txt".as_ref()));
    assert_eq!(std::fs::read_to_string(landed).unwrap(), "/w/proj/a.txt", "the worker's file");
}

/// Upload and download are palette commands everywhere: named for the Files picker on iOS,
/// plainly on a Mac, where the open panel and a picked folder stand in for it.
#[test]
fn upload_and_download_are_offered_on_every_device() {
    use crate::folder::{DOWNLOAD, SAVE_TO_FILES, UPLOAD, UPLOAD_FROM_FILES, files_palette_items};
    let bindings = key_bindings();
    let labels = |ios| -> Vec<String> {
        files_palette_items(ios, &bindings).into_iter().map(|i| i.label).collect()
    };
    assert_eq!(labels(true), [UPLOAD_FROM_FILES, SAVE_TO_FILES]);
    assert_eq!(labels(false), [UPLOAD, DOWNLOAD]);
    let here = labels(cfg!(target_os = "ios"));
    let listed: Vec<String> =
        palette_items().into_iter().map(|i| i.label).filter(|l| here.contains(l)).collect();
    assert_eq!(listed, here);
}

/// "Upload from Files…" on a folder tile asks the Files picker (here its seam) for files to
/// upload there, and what it hands over goes up into the folder, which is listed again once
/// they are there, the picked files deleted here. "Save to Files…" asks it to save the
/// selected row. With the keyboard on a tile that takes no files, nothing is asked.
#[gpui::test]
fn the_files_picker_is_asked_through_its_seam_and_its_files_go_up(cx: &mut TestAppContext) {
    use slopty_platform::file_drop::{Landing, root};

    use crate::workspace::folders::{FilesAsk, FilesSeam};
    let (view, cx) = workspace(cx);
    let asks = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&asks);
    cx.update(|_window, cx| {
        cx.set_global(FilesSeam(Rc::new(move |ask: &FilesAsk| {
            seen.borrow_mut().push(ask.clone());
        })));
    });
    let (mut studio, mut uploads) = link_uploads(&view, cx);
    let key = studio.key;
    let tile = arrives(&view, cx, &studio, ItemKind::Folder { path: "/w/in".into() }, 1);
    answer(&view, cx, &studio, "/w/in", &listed("/w/in", vec![entry("a.txt", FileKind::File)]));
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    studio.drain();

    cx.dispatch_action(crate::folder::UploadFromFiles);
    assert_eq!(*asks.borrow(), [FilesAsk::Import(tile)], "the picker is asked");
    let mut landing = Landing::new(&root(), 1, (0.0, 0.0)).unwrap();
    let picked = landing.dir().join("report.pdf");
    std::fs::write(&picked, b"pdf").unwrap();
    let dropped = landing.resolve(Ok(picked)).expect("the one file");
    view.update_in(cx, |v, _w, cx| v.files_picked(tile, dropped, cx));
    let (xfer, dest) = uploads.try_recv().expect("an upload");
    assert_eq!(dest, Dest::Path("/w/in".into()), "into the folder");
    let paths = vec!["/w/in/report.pdf".to_owned()];
    view.update_in(cx, |v, _window, cx| v.xfer_message(XferMsg::Finished { xfer, paths }, cx));
    cx.run_until_parked();
    assert_eq!(asked(&mut studio), ["/w/in"], "listed again with the file in it");
    assert!(!landing.dir().exists(), "the picked copies are gone once they are up");

    cx.dispatch_action(crate::folder::SaveToFiles);
    let saved = FilesAsk::Export { worker: key, path: "/w/in/a.txt".into(), folder: false };
    assert_eq!(asks.borrow().last(), Some(&saved), "the selected row is saved");

    let note = arrives(&view, cx, &studio, ItemKind::File { path: "/w/n.md".into() }, 2);
    view.update_in(cx, |v, window, cx| {
        v.focus_tile(note, cx);
        v.upload_from_files(&crate::folder::UploadFromFiles, window, cx);
    });
    assert_eq!(asks.borrow().len(), 2, "a note takes no files: nothing more is asked");
}

/// Files dropped on a folder tile go up into its folder, and once they are there the folder
/// is listed again.
#[gpui::test]
fn a_drop_on_a_folder_goes_up_into_it_and_lists_it_again(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let (tx, rx) = mpsc::channel(256);
    let (calls, mut uploads) = mpsc::unbounded_channel();
    let key = WorkerKey::new(3);
    let me = ClientId::new();
    view.update_in(cx, |v, _window, cx| {
        v.add_worker(key, "studio".to_owned(), cx);
        let remote: Arc<dyn Remote> = Arc::new(Uploads(calls));
        let factory: ScreenFactory =
            Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
        let link = WorkerLink { me, out: tx, open_screen: factory, remote: Some(remote) };
        v.connect_worker(key, link, hello("studio", Vec::new()), cx);
        v.apply_sync(key, ItemSync::Snapshot { version: 0, items: Vec::new() }, cx);
    });
    cx.run_until_parked();
    let mut studio = Fake { key, me, rx };
    let tile = arrives(&view, cx, &studio, ItemKind::Folder { path: "/w/in".into() }, 1);
    answer(&view, cx, &studio, "/w/in", &listed("/w/in", Vec::new()));
    studio.drain();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("report.pdf");
    std::fs::write(&file, b"pdf").unwrap();

    let at = view.read_with(cx, |v, _| v.tile_bounds(tile)).expect("drawn").center();
    cx.simulate_event(FileDropEvent::Entered {
        position: at,
        paths: ExternalPaths(std::iter::once(file).collect()),
    });
    cx.simulate_event(FileDropEvent::Pending { position: at });
    cx.simulate_event(FileDropEvent::Submit { position: at });
    cx.run_until_parked();
    let (xfer, dest) = uploads.try_recv().expect("an upload");
    assert_eq!(dest, Dest::Path("/w/in".into()), "into the folder");

    let paths = vec!["/w/in/report.pdf".to_owned()];
    view.update_in(cx, |v, _window, cx| v.xfer_message(XferMsg::Finished { xfer, paths }, cx));
    cx.run_until_parked();
    assert_eq!(asked(&mut studio), ["/w/in"], "listed again with the file in it");
    assert_eq!(view.read_with(cx, |v, _| v.toast_text()), None, "it kept its name: no word");

    // The same name again: it lands beside the first under the next free name, which is said.
    let other = dir.path().join("notes.md");
    std::fs::write(&other, b"md").unwrap();
    let file = dir.path().join("report.pdf");
    view.update_in(cx, |v, _window, cx| v.drop_files(tile, &[file, other], cx));
    let (xfer, _dest) = uploads.try_recv().expect("another upload");
    let paths = vec!["/w/in/notes.md".to_owned(), "/w/in/report 2.pdf".to_owned()];
    view.update_in(cx, |v, _window, cx| v.xfer_message(XferMsg::Finished { xfer, paths }, cx));
    cx.run_until_parked();
    let said = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(said.as_deref(), Some("report 2.pdf (report.pdf was there already)"));
}

/// A folder says where it is once: in its path bar, whose first crumb's words stand on the
/// rows' icons, and not again beside its title in the header.
#[gpui::test]
fn a_folder_says_where_it_is_once_on_the_rows_edge(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let tile = arrives(&view, cx, &studio, ItemKind::Folder { path: "/w/project".into() }, 1);
    let rows = vec![entry("src", FileKind::Dir), entry(".env", FileKind::File)];
    answer(&view, cx, &studio, "/w/project", &listed("/w/project", rows));
    assert!(cx.debug_bounds(selector("place", tile.item)).is_none(), "no place in the header");
    let theme = view.read_with(cx, |v, _| v.theme.clone());
    let crumb = cx.debug_bounds("folder-crumb-0").expect("the path bar");
    let row = cx.debug_bounds("folder-row-0").expect("a row");
    let words = crumb.left() + px(theme.spacing.xxs);
    let icon = row.left() + px(theme.spacing.inset() - crate::palette::list_pad(&theme));
    assert!(
        (words - icon).abs() < px(0.5),
        "crumb's words at {words:?}, the rows' icons at {icon:?}"
    );
}

/// The directories the workspace last asked `fake` to follow.
fn followed(fake: &mut Fake) -> Option<Vec<String>> {
    fake.drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::WatchFolders { paths } => Some(paths),
            _ => None,
        })
        .next_back()
}

/// A folder tile's directory is followed by its worker, and a listing the worker sends
/// unasked (its entries changed on disk) shows at once. The selected entry stays selected, and
/// when it is the one that went, the selection stays at its row. The last tile gone, nothing is
/// followed.
#[gpui::test]
fn a_folder_tile_follows_its_directory_and_keeps_its_place(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let tile = arrives(&view, cx, &studio, ItemKind::Folder { path: "/w/proj".into() }, 1);
    assert_eq!(followed(&mut studio), Some(vec!["/w/proj".to_owned()]), "followed");
    let files = |names: &[&str]| {
        listed("/w/proj", names.iter().map(|n| entry(n, FileKind::File)).collect())
    };
    answer(&view, cx, &studio, "/w/proj", &files(&["a.rs", "b.rs", "c.rs"]));
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    cx.simulate_keystrokes("down");
    assert_eq!(selected(&view, cx, tile), "b.rs");

    answer(&view, cx, &studio, "/w/proj", &files(&["a.rs", "aa.rs", "b.rs", "c.rs"]));
    let nodes = tree(cx);
    assert!(nodes.iter().any(|n| n.is("ListBoxOption", Some("aa.rs"))), "{nodes:#?}");
    assert_eq!(selected(&view, cx, tile), "b.rs", "the selection follows its entry");

    answer(&view, cx, &studio, "/w/proj", &files(&["a.rs", "aa.rs", "c.rs"]));
    assert_eq!(selected(&view, cx, tile), "c.rs", "the entry went: the row where it was");
    answer(&view, cx, &studio, "/w/proj", &files(&["a.rs"]));
    assert_eq!(selected(&view, cx, tile), "a.rs", "clamped to the last row");
    assert_eq!(followed(&mut studio), None, "the same set is not sent again");

    view.update_in(cx, |v, _w, cx| {
        let by = ClientId::new();
        v.apply_sync(
            studio.key,
            ItemSync::Delta { version: 2, by, op: ItemOp::Remove(tile.item) },
            cx,
        );
    });
    cx.run_until_parked();
    assert_eq!(followed(&mut studio), Some(Vec::new()), "nothing followed once the tile goes");
}

/// `open .` in a worker's shell hands its folder over as a path ending in `/`: it opens as a
/// folder tile, not a file tile, and the worker hears it was taken.
#[gpui::test]
fn a_folder_a_shell_hands_over_opens_as_a_folder_tile(cx: &mut TestAppContext) {
    use slopty_proto::handoff::{EditFile, HandoffEvent, HandoffReply};
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    studio.drain();
    let key = studio.key;
    let edit =
        EditFile { id: 1, session: None, path: "/w/proj/".to_owned(), line: None, wait: false };
    view.update_in(cx, |v, _w, cx| {
        v.handoff_event(key, HandoffEvent::Edit(edit), Instant::now(), cx);
    });
    cx.run_until_parked();
    let sent = studio.drain();
    let added = sent.iter().any(|m| {
        matches!(m, ClientMsg::Items(ItemOp::Add(Item { kind: ItemKind::Folder { path }, .. }))
            if path == "/w/proj")
    });
    assert!(added, "a folder tile at /w/proj: {sent:?}");
    let taken = sent.iter().any(|m| matches!(m, ClientMsg::Handoff(HandoffReply::Taken { id: 1 })));
    assert!(taken, "the worker hears it was taken: {sent:?}");
    let files = view.read_with(cx, |v, _| v.files.len());
    assert_eq!(files, 0, "no file tile");
}

/// The folder's own changes, keyed: ⌘⇧N names a new folder in a field over the rows, and ↩
/// asks the worker to make it, the new folder selected once the folder lists it; "Rename or
/// move…" writes a new name in the row, and a path in it moves the entry; ⌘⌫ trashes the
/// selected entry, and the notice's "Put back" moves it back from the trash. A change refused
/// says why, Esc puts a field away asking nothing, and a change whose answer went with the
/// link says so.
#[gpui::test]
fn a_folder_makes_renames_and_trashes_its_entries(cx: &mut TestAppContext) {
    use slopty_proto::folder::{FsOp, FsOutcome, FsRefusal};

    use crate::folder::Fate;

    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let tile = arrives(&view, cx, &studio, ItemKind::Folder { path: "/w".into() }, 1);
    let top = listed("/w", vec![entry("docs", FileKind::Dir), entry("a.txt", FileKind::File)]);
    answer(&view, cx, &studio, "/w", &top);
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    studio.drain();
    let ops = |fake: &mut Fake| -> Vec<(slopty_proto::RequestId, FsOp)> {
        fake.drain()
            .into_iter()
            .filter_map(|m| match m {
                ClientMsg::FsOp { request, op } => Some((request, op)),
                _ => None,
            })
            .collect()
    };
    let done = |cx: &mut VisualTestContext, request, outcome| {
        view.update_in(cx, |v, _w, cx| v.fs_done(key, request, outcome, cx));
        cx.run_until_parked();
    };

    cx.simulate_keystrokes("cmd-shift-n");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds(selector("folder-new", tile.item)).is_some(),
        "the field over the rows"
    );
    cx.simulate_input("planz");
    cx.simulate_keystrokes("backspace");
    cx.simulate_input("s");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let made = ops(&mut studio);
    let [(request, op)] = made.as_slice() else { panic!("one op: {made:?}") };
    assert_eq!(*op, FsOp::MakeDir { parent: "/w".into(), name: "plans".into() });
    let drawn_made = |cx: &mut VisualTestContext| {
        cx.debug_bounds(selector("folder-made-0", tile.item)).is_some()
    };
    assert!(drawn_made(cx), "drawn as made at once");
    done(cx, *request, FsOutcome::Done { path: "/w/plans".into() });
    assert!(drawn_made(cx), "and still, until the folder lists it");
    let with_plans = listed(
        "/w",
        vec![
            entry("docs", FileKind::Dir),
            entry("plans", FileKind::Dir),
            entry("a.txt", FileKind::File),
        ],
    );
    assert_eq!(folder_path(&view, cx, tile), "/w", "↩ in the field opens nothing");
    answer(&view, cx, &studio, "/w", &with_plans);
    assert_eq!(selected(&view, cx, tile), "plans", "the new folder, selected");
    assert!(!drawn_made(cx), "one row for it, the listed one");
    let fate = |cx: &mut VisualTestContext, name: &str| {
        view.read_with(cx, |v, cx| v.folder(tile.item).and_then(|f| f.read(cx).fate(name)))
    };

    cx.simulate_keystrokes("end");
    cx.dispatch_action(crate::folder::RenameSelected);
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("docs/a.txt");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let moved = ops(&mut studio);
    let [(request, op)] = moved.as_slice() else { panic!("one op: {moved:?}") };
    assert_eq!(*op, FsOp::Move { from: "/w/a.txt".into(), to: "/w/docs/a.txt".into() });
    assert_eq!(fate(cx, "a.txt"), Some(Fate::Leaving), "set back on its way out");
    let clash = FsOutcome::Refused(FsRefusal::Clash { path: "/w/docs/a.txt".into() });
    done(cx, *request, clash);
    let said = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(said.as_deref(), Some("Something named “a.txt” is already there"));
    assert_eq!(fate(cx, "a.txt"), None, "refused: drawn as it was");

    cx.dispatch_action(crate::folder::RenameSelected);
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(ops(&mut studio).is_empty(), "Esc asks nothing");

    cx.simulate_keystrokes("cmd-backspace");
    cx.run_until_parked();
    let trashed = ops(&mut studio);
    let [(request, op)] = trashed.as_slice() else { panic!("one op: {trashed:?}") };
    assert_eq!(*op, FsOp::Trash { path: "/w/a.txt".into() });
    assert_eq!(fate(cx, "a.txt"), Some(Fate::Leaving));
    done(cx, *request, FsOutcome::Done { path: "/home/.Trash/a.txt".into() });
    let said = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(said.as_deref(), Some("Moved “a.txt” to the Trash"));
    let put_back = cx.debug_bounds("toast-put-back").expect("the way back");
    cx.simulate_click(put_back.center(), Modifiers::none());
    cx.run_until_parked();
    let back = ops(&mut studio);
    let [(request, op)] = back.as_slice() else { panic!("one op: {back:?}") };
    assert_eq!(*op, FsOp::Move { from: "/home/.Trash/a.txt".into(), to: "/w/a.txt".into() });

    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("lost".into()), cx);
    });
    cx.run_until_parked();
    let said = view.read_with(cx, |v, _| v.toast_text());
    let unknown = "studio went out of reach before it said whether it could move “a.txt”";
    assert_eq!(said.as_deref(), Some(unknown), "the answer went with the link");
    done(cx, *request, FsOutcome::Failed { error: "late".into() });
    let said = view.read_with(cx, |v, _| v.toast_text());
    assert_eq!(said.as_deref(), Some(unknown), "an answer nobody waits on says nothing");
}

/// A folder past a page comes a page at a time: the rows near the end of those listed ask for
/// the next, which joins them; a relist keeps as many pages as were wanted.
#[gpui::test]
fn a_long_folder_comes_a_page_at_a_time(cx: &mut TestAppContext) {
    use slopty_proto::folder::After;

    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let tile = arrives(&view, cx, &studio, ItemKind::Folder { path: "/w".into() }, 1);
    let page = |from: usize, to: usize| -> Vec<FolderEntry> {
        (from..to).map(|n| entry(&format!("f{n:05}.txt"), FileKind::File)).collect()
    };
    let first = Listing::Listed { dir: "/w".into(), entries: page(0, 30), total: 50 };
    answer(&view, cx, &studio, "/w", &first);
    let pages = |fake: &mut Fake| -> Vec<After> {
        fake.drain()
            .into_iter()
            .filter_map(|m| match m {
                ClientMsg::FolderPage { after, .. } => Some(after),
                _ => None,
            })
            .collect()
    };
    let asked = pages(&mut studio);
    let last = After { folder: false, name: "f00029.txt".into() };
    assert_eq!(asked, std::slice::from_ref(&last), "the rows drawn reach the end: the next page");
    let rest = Listing::Listed { dir: "/w".into(), entries: page(30, 50), total: 50 };
    view.update_in(cx, |v, _w, cx| v.folder_page(key, "/w", &last, &rest, cx));
    cx.run_until_parked();
    let shown =
        view.read_with(cx, |v, cx| v.folder(tile.item).map_or(0, |f| f.read(cx).entries().len()));
    assert_eq!(shown, 50, "the page joins the rows");
    assert!(pages(&mut studio).is_empty(), "every entry is here");
}
