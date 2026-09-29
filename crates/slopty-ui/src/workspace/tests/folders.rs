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
    assert!(kinds(&view, cx).is_empty());
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

/// Records the uploads the workspace starts.
#[derive(Debug)]
struct Uploads(mpsc::UnboundedSender<(XferId, Dest)>);

impl Remote for Uploads {
    fn upload(&self, xfer: XferId, _files: Vec<PathBuf>, dest: Dest) {
        self.0.send((xfer, dest)).unwrap();
    }

    fn cancel(&self, _xfer: XferId) {}

    /// Writes a file named as the path's last part, whose text is the path.
    fn download(&self, path: String, into: PathBuf) -> Result<Vec<PathBuf>, XferError> {
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

/// The Files picker is a palette command on iOS and nowhere else.
#[test]
fn the_files_picker_is_offered_on_ios_only() {
    use crate::folder::{SAVE_TO_FILES, UPLOAD_FROM_FILES, files_palette_items};
    let bindings = key_bindings();
    assert!(files_palette_items(false, &bindings).is_empty());
    let labels: Vec<String> =
        files_palette_items(true, &bindings).into_iter().map(|i| i.label).collect();
    assert_eq!(labels, [UPLOAD_FROM_FILES, SAVE_TO_FILES]);
    let listed: Vec<String> = palette_items()
        .into_iter()
        .map(|i| i.label)
        .filter(|l| l == UPLOAD_FROM_FILES || l == SAVE_TO_FILES)
        .collect();
    assert_eq!(listed.len(), if cfg!(target_os = "ios") { 2 } else { 0 }, "{listed:?}");
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

    let note = arrives(&view, cx, &studio, ItemKind::Note { text: "n".into() }, 2);
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
