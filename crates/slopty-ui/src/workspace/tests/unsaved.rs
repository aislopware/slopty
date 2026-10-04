//! Hot exit in the workspace: a file tile's unsaved edit is kept on this device as it is typed,
//! let go once saved or closed for good, and laid back over the tile after a restart; a kept
//! edit whose tile went gets a tile again.

use slopty_client::unsaved::{Store, Unsaved};
use slopty_proto::file::{FileRead, WriteResult};

use super::*;

const PATH: &str = "/w/notes.txt";

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = Store::new(dir.path().join("unsaved"));
    (dir, store)
}

fn read(text: &str, modified_ms: u64) -> FileRead {
    FileRead::Text {
        text: text.to_owned(),
        size: 8,
        modified_ms: WallMs::from_millis(modified_ms),
        final_newline: true,
        editorconfig: Vec::new(),
    }
}

/// The store as the app finds it when it starts again: the same directory, a new process.
fn relaunched(dir: &tempfile::TempDir) -> Store {
    Store::new(dir.path().join("unsaved"))
}

/// The texts kept now.
fn kept(store: &Store) -> Vec<String> {
    store.all().into_iter().map(|u| u.text).collect()
}

/// A tile on `studio` showing [`PATH`], read and focused.
fn file_tile(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, studio: &Fake) -> TileRef {
    let tile = arrives(view, cx, studio, ItemKind::File { path: PATH.to_owned() }, 1);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.file_read(key, PATH, &read("# Notes", 1_000), cx);
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    tile
}

/// The first keystroke's edit is kept at once, and the ones after it within
/// [`super::super::unsaved`]'s throttle; once the worker has written the file, the backup goes.
#[gpui::test]
fn an_edit_is_kept_as_it_is_typed_and_let_go_once_saved(cx: &mut TestAppContext) {
    let (_dir, store) = store();
    let (view, cx) = workspace(cx);
    view.update(cx, |v, cx| v.set_unsaved_store(store.clone(), cx));
    cx.run_until_parked();
    let studio = connect(&view, cx, 1, "studio");
    file_tile(&view, cx, &studio);
    assert!(kept(&store).is_empty(), "nothing to keep while clean");

    cx.simulate_input("x");
    cx.run_until_parked();
    assert_eq!(kept(&store), ["x# Notes"], "kept at once");
    cx.simulate_input("y");
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    assert_eq!(kept(&store), ["xy# Notes"], "and again within the throttle");

    cx.simulate_keystrokes("cmd-s");
    cx.run_until_parked();
    assert_eq!(kept(&store), ["xy# Notes"], "kept until the disk has it");
    let saved = WriteResult::Saved { size: 10, modified_ms: WallMs::from_millis(2_000) };
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.file_written(key, PATH, &saved, cx));
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    assert!(kept(&store).is_empty(), "saved: let go");
}

/// A tile closed with an edit keeps it while ⌘Z can bring the tile back, and lets it go once
/// the tile is closed for good. Quitting writes what is left there and then.
#[gpui::test]
fn an_edit_closed_for_good_is_let_go(cx: &mut TestAppContext) {
    let (_dir, store) = store();
    let (view, cx) = workspace(cx);
    view.update(cx, |v, cx| v.set_unsaved_store(store.clone(), cx));
    cx.run_until_parked();
    let studio = connect(&view, cx, 1, "studio");
    file_tile(&view, cx, &studio);
    cx.simulate_input("x");
    cx.run_until_parked();
    cx.simulate_input("z");
    view.update_in(cx, |v, _w, cx| v.keep_unsaved_now(cx));
    assert_eq!(kept(&store), ["xz# Notes"], "written now, not after the throttle");

    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    assert_eq!(kept(&store), ["xz# Notes"], "⌘Z could still bring it back");
    cx.executor().advance_clock(UNDO_CLOSE);
    cx.run_until_parked();
    assert!(kept(&store).is_empty(), "closed for good");
}

/// After a restart, the tile on the same file on the same worker takes its kept edit back,
/// unsaved; and a kept edit whose tile went meanwhile gets a tile of its own again.
#[gpui::test]
fn a_kept_edit_comes_back_on_its_tile_or_on_a_new_one(cx: &mut TestAppContext) {
    let (dir, store) = store();
    let key = WorkerKey::new(1);
    // The worker's registry still holds the first file's tile, not the second's.
    let item = Item {
        id: ItemId::new(),
        kind: ItemKind::File { path: PATH.to_owned() },
        name: None,
        facts: BTreeMap::new(),
    };
    let kept_edit = |id: ItemId, path: &str, text: &str| Unsaved {
        worker: key,
        item: id,
        path: path.to_owned(),
        text: text.to_owned(),
        newline: true,
        base_modified_ms: Some(WallMs::from_millis(1_000)),
        conflict: false,
        kept_ms: WallMs::now(),
    };
    store.put(&kept_edit(item.id, PATH, "draft # Notes"), 1).expect("put");
    store.put(&kept_edit(ItemId::new(), "/w/orphan.txt", "lost tile"), 2).expect("put");
    let store = relaunched(&dir);
    let (view, cx) = workspace(cx);
    view.update(cx, |v, cx| v.set_unsaved_store(store.clone(), cx));
    cx.run_until_parked();
    let tile = TileRef { worker: key, item: item.id };
    let (tx, mut rx) = mpsc::channel(256);
    let factory: ScreenFactory =
        Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
    view.update_in(cx, |v, _w, cx| {
        v.add_worker(key, "studio".to_owned(), cx);
        let link = WorkerLink { me: ClientId::new(), out: tx, open_screen: factory, remote: None };
        v.connect_worker(key, link, hello("studio", Vec::new()), cx);
        v.apply_sync(key, ItemSync::Snapshot { version: 1, items: vec![item] }, cx);
    });
    cx.run_until_parked();
    let sent: Vec<ClientMsg> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    let reopened = sent.iter().filter(|m| {
        matches!(m, ClientMsg::Items(ItemOp::Add(Item { kind: ItemKind::File { path }, .. }))
            if path == "/w/orphan.txt")
    });
    assert_eq!(reopened.count(), 1, "the edit whose tile went has a tile again: {sent:?}");

    view.update_in(cx, |v, _w, cx| {
        v.file_read(key, PATH, &read("# Notes", 1_000), cx);
        v.file_read(key, "/w/orphan.txt", &read("", 1_000), cx);
    });
    cx.run_until_parked();
    let texts = view.read_with(cx, |v, cx| {
        v.items()
            .filter_map(|(_, i)| v.file(i.id))
            .map(|f| (f.read(cx).path().to_owned(), f.read(cx).text(cx), f.read(cx).dirty()))
            .collect::<Vec<_>>()
    });
    assert!(texts.contains(&(PATH.to_owned(), "draft # Notes".to_owned(), true)), "{texts:?}");
    assert!(
        texts.contains(&("/w/orphan.txt".to_owned(), "lost tile".to_owned(), true)),
        "{texts:?}"
    );
    assert!(cx.debug_bounds(selector("unsaved", tile.item)).is_some(), "the header says so");
    settle(cx);
    let backups = store.all();
    assert_eq!(backups.len(), 2, "one each, the moved one no longer under its gone tile");
    for kept in backups {
        let shown = view.read_with(cx, |v, _| v.file(kept.item).is_some());
        assert!(shown, "{} is kept under the tile that shows it", kept.path);
    }
}

/// The backup of [`PATH`] on worker 1, if one is kept.
fn backup(store: &Store) -> Option<Unsaved> {
    store.all().into_iter().find(|u| u.path == PATH)
}

/// Let every pass run: the throttle's wait included.
fn settle(cx: &VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(200));
    }
    cx.run_until_parked();
}

/// A second tile on the same file, read and clean.
fn second_tile(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    studio: &Fake,
    version: u64,
) -> TileRef {
    let tile = arrives(view, cx, studio, ItemKind::File { path: PATH.to_owned() }, version);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.file_read(key, PATH, &read("# Notes", 1_000), cx));
    cx.run_until_parked();
    tile
}

/// Two tiles on one file, one with an edit and one clean: the clean one lets nothing go, in a
/// pass or in the flush as the app quits.
#[gpui::test]
fn a_clean_tile_keeps_the_backup_of_another_tile_on_its_file(cx: &mut TestAppContext) {
    let (_dir, store) = store();
    let (view, cx) = workspace(cx);
    view.update(cx, |v, cx| v.set_unsaved_store(store.clone(), cx));
    cx.run_until_parked();
    let studio = connect(&view, cx, 1, "studio");
    file_tile(&view, cx, &studio);
    cx.simulate_input("x");
    settle(cx);
    let other = second_tile(&view, cx, &studio, 2);
    view.update_in(cx, |v, _w, cx| v.focus_tile(other, cx));
    settle(cx);
    assert_eq!(kept(&store), ["x# Notes"], "a pass keeps it");
    view.update_in(cx, |v, _w, cx| v.keep_unsaved_now(cx));
    assert_eq!(kept(&store), ["x# Notes"], "and so does the flush on quit");
}

/// A tile closed with an edit keeps its backup while ⌘Z can bring it back, whatever another
/// tile on the file does meanwhile; closed for good, it lets go only of its own edit, never of
/// one made since in a tile opened again.
#[gpui::test]
fn closing_for_good_lets_go_only_of_that_tiles_edit(cx: &mut TestAppContext) {
    let (_dir, store) = store();
    let (view, cx) = workspace(cx);
    view.update(cx, |v, cx| v.set_unsaved_store(store.clone(), cx));
    cx.run_until_parked();
    let studio = connect(&view, cx, 1, "studio");
    file_tile(&view, cx, &studio);
    cx.simulate_input("x");
    settle(cx);
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    let again = second_tile(&view, cx, &studio, 3);
    settle(cx);
    assert_eq!(kept(&store), ["x# Notes"], "⌘Z can still bring the closed edit back");

    view.update_in(cx, |v, _w, cx| v.focus_tile(again, cx));
    cx.run_until_parked();
    cx.simulate_input("y");
    cx.executor().advance_clock(UNDO_CLOSE);
    settle(cx);
    assert_eq!(kept(&store), ["y# Notes"], "the closed edit goes, the new one stays");
}

/// A save answered after the text moved on moves the backup's base with it: back after a
/// restart over the file as saved, the edit is no conflict.
#[gpui::test]
fn a_save_under_an_edit_moves_the_backups_base(cx: &mut TestAppContext) {
    let (_dir, store) = store();
    let (view, cx) = workspace(cx);
    view.update(cx, |v, cx| v.set_unsaved_store(store.clone(), cx));
    cx.run_until_parked();
    let studio = connect(&view, cx, 1, "studio");
    file_tile(&view, cx, &studio);
    cx.simulate_input("x");
    cx.simulate_keystrokes("cmd-s");
    cx.simulate_input("y");
    settle(cx);
    let saved = WriteResult::Saved { size: 9, modified_ms: WallMs::from_millis(2_000) };
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.file_written(key, PATH, &saved, cx));
    settle(cx);
    let kept = backup(&store).expect("still unsaved: the y");
    assert_eq!(kept.text, "xy# Notes");
    assert_eq!(kept.base_modified_ms, Some(WallMs::from_millis(2_000)), "based on the save");
    assert!(!kept.conflict);
}

/// A write that failed is not taken as done: the flush as the app quits writes it again.
#[cfg(unix)]
#[gpui::test]
fn a_failed_write_is_written_again_on_quit(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt as _;

    let (dir, store) = store();
    let (view, cx) = workspace(cx);
    view.update(cx, |v, cx| v.set_unsaved_store(store.clone(), cx));
    cx.run_until_parked();
    let studio = connect(&view, cx, 1, "studio");
    file_tile(&view, cx, &studio);
    let unsaved = dir.path().join("unsaved");
    std::fs::create_dir_all(&unsaved).expect("dir");
    std::fs::set_permissions(&unsaved, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    cx.simulate_input("x");
    settle(cx);
    std::fs::set_permissions(&unsaved, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    assert!(kept(&store).is_empty(), "the write failed");
    view.update_in(cx, |v, _w, cx| v.keep_unsaved_now(cx));
    assert_eq!(kept(&store), ["x# Notes"], "written on quit");
}

/// A write that failed (the disk full for a moment) is tried again on its own, with no edit
/// after it and no quit, so a crash after the disk recovers still finds the edit. A few
/// failures in a row are said, once.
#[cfg(unix)]
#[gpui::test]
fn a_failed_write_is_tried_again_without_another_edit(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt as _;

    let (dir, store) = store();
    let (view, cx) = workspace(cx);
    view.update(cx, |v, cx| v.set_unsaved_store(store.clone(), cx));
    cx.run_until_parked();
    let studio = connect(&view, cx, 1, "studio");
    file_tile(&view, cx, &studio);
    let unsaved = dir.path().join("unsaved");
    std::fs::create_dir_all(&unsaved).expect("dir");
    std::fs::set_permissions(&unsaved, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    cx.simulate_input("x");
    // The person moves on to a shell: no caret blinks in a file tile, and nothing changes in it.
    let shell = opens_in(&view, cx, &studio, SessionId::new(), ClientId::new(), 2, None);
    view.update_in(cx, |v, _w, cx| v.focus_tile(shell, cx));
    settle(cx);
    assert!(kept(&store).is_empty(), "the write failed");
    let toasts = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.toast_texts());
    let not_kept = super::super::unsaved::NOT_KEPT;
    assert!(!toasts(cx).iter().any(|t| t.starts_with(not_kept)), "one failure is a hiccup");
    // Each time the notice comes up, over a minute of failing passes.
    let mut shown = Vec::new();
    for _ in 0..60 {
        cx.executor().advance_clock(Duration::from_secs(1));
        settle(cx);
        shown.push(toasts(cx).iter().any(|t| t.starts_with(not_kept)));
    }
    let comes_up = shown.windows(2).filter(|w| w == &[false, true]).count();
    assert_eq!(comes_up, 1, "a few failures in a row are said, once: {shown:?}");
    std::fs::set_permissions(&unsaved, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    cx.executor().advance_clock(Duration::from_secs(60));
    settle(cx);
    assert_eq!(kept(&store), ["x# Notes"], "tried again, and kept");
}

/// Two tiles on one file, each with its own edit (two clients opened the file at the same
/// moment, or ⌘Z brought a closed one back beside a new one): both edits are kept, neither laid
/// over the other.
#[gpui::test]
fn two_tiles_on_one_file_keep_both_edits(cx: &mut TestAppContext) {
    let (_dir, store) = store();
    let (view, cx) = workspace(cx);
    view.update(cx, |v, cx| v.set_unsaved_store(store.clone(), cx));
    cx.run_until_parked();
    let studio = connect(&view, cx, 1, "studio");
    file_tile(&view, cx, &studio);
    cx.simulate_input("a");
    settle(cx);
    let other = second_tile(&view, cx, &studio, 2);
    view.update_in(cx, |v, _w, cx| v.focus_tile(other, cx));
    cx.run_until_parked();
    cx.simulate_input("b");
    settle(cx);
    let mut texts = kept(&store);
    texts.sort();
    assert_eq!(texts, ["a# Notes", "b# Notes"], "a pass keeps both");
    view.update_in(cx, |v, _w, cx| v.keep_unsaved_now(cx));
    let mut texts = kept(&store);
    texts.sort();
    assert_eq!(texts, ["a# Notes", "b# Notes"], "and so does the flush on quit");
}

/// After a restart, two tiles on one file each take back their own kept edit.
#[gpui::test]
fn two_tiles_on_one_file_each_take_their_own_edit_back(cx: &mut TestAppContext) {
    let (dir, store) = store();
    let key = WorkerKey::new(1);
    let file_item = || Item {
        id: ItemId::new(),
        kind: ItemKind::File { path: PATH.to_owned() },
        name: None,
        facts: BTreeMap::new(),
    };
    let (first, second) = (file_item(), file_item());
    for (seq, (item, text)) in [(&first, "a# Notes"), (&second, "b# Notes")].into_iter().enumerate()
    {
        let unsaved = Unsaved {
            worker: key,
            item: item.id,
            path: PATH.to_owned(),
            text: text.to_owned(),
            newline: true,
            base_modified_ms: Some(WallMs::from_millis(1_000)),
            conflict: false,
            kept_ms: WallMs::now(),
        };
        store.put(&unsaved, u64::try_from(seq).unwrap_or(0)).expect("put");
    }
    let store = relaunched(&dir);
    let (view, cx) = workspace(cx);
    view.update(cx, |v, cx| v.set_unsaved_store(store.clone(), cx));
    cx.run_until_parked();
    let (tx, _rx) = mpsc::channel(256);
    let factory: ScreenFactory =
        Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
    let items = vec![first.clone(), second.clone()];
    view.update_in(cx, |v, _w, cx| {
        v.add_worker(key, "studio".to_owned(), cx);
        let link = WorkerLink { me: ClientId::new(), out: tx, open_screen: factory, remote: None };
        v.connect_worker(key, link, hello("studio", Vec::new()), cx);
        v.apply_sync(key, ItemSync::Snapshot { version: 1, items }, cx);
    });
    cx.run_until_parked();
    view.update_in(cx, |v, _w, cx| v.file_read(key, PATH, &read("# Notes", 1_000), cx));
    settle(cx);
    let text_of = |id: ItemId| {
        view.read_with(cx, |v, cx| v.file(id).map(|f| (f.read(cx).text(cx), f.read(cx).dirty())))
    };
    assert_eq!(text_of(first.id), Some(("a# Notes".to_owned(), true)));
    assert_eq!(text_of(second.id), Some(("b# Notes".to_owned(), true)));
    let mut texts = kept(&store);
    texts.sort();
    assert_eq!(texts, ["a# Notes", "b# Notes"], "both still kept");
}
