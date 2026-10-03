//! "Save a copy…" on a file tile: the worker's file comes down whole to the path the save panel
//! gives (the test platform's, which shows nothing).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use slopty_client::clip::Fetched;
use slopty_client::remote::Remote;
use slopty_client::xfer::XferError;
use slopty_core::{WallMs, XferId};
use slopty_proto::file::{FILE_BYTES, FileRead, INLINE_FILE_BYTES};
use slopty_proto::transfer::{Dest, RepRef};

use super::*;

/// A worker's files by path; a download writes one's bytes under its name, and is recorded.
#[derive(Debug)]
struct Files {
    held: HashMap<String, Vec<u8>>,
    asked: mpsc::UnboundedSender<String>,
}

impl Remote for Files {
    fn upload(&self, _xfer: XferId, _files: Vec<PathBuf>, _dest: Dest) {}

    fn cancel(&self, _xfer: XferId) {}

    fn download(
        &self,
        path: String,
        into: PathBuf,
        _shown_at: Option<PathBuf>,
    ) -> Result<Vec<PathBuf>, XferError> {
        self.asked.send(path.clone()).map_err(|e| XferError::Worker(e.to_string()))?;
        let bytes = self
            .held
            .get(&path)
            .ok_or_else(|| XferError::Worker("No such file or directory".to_owned()))?;
        let file = into
            .join(path.rsplit('/').next().ok_or_else(|| XferError::Worker("no name".to_owned()))?);
        std::fs::write(&file, bytes).map_err(|e| XferError::Worker(e.to_string()))?;
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

/// A worker whose downloads `files` serves.
fn link_files(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, files: Arc<Files>) -> Fake {
    let (tx, rx) = mpsc::channel(256);
    let (me, key) = (ClientId::new(), WorkerKey::new(9));
    let factory: ScreenFactory =
        Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
    view.update_in(cx, |v, _window, cx| {
        v.add_worker(key, "studio".to_owned(), cx);
        let remote: Arc<dyn Remote> = files;
        let link = WorkerLink { me, out: tx, open_screen: factory, remote: Some(remote) };
        v.connect_worker(key, link, hello("studio", Vec::new()), cx);
        v.apply_sync(key, ItemSync::Snapshot { version: 0, items: Vec::new() }, cx);
    });
    cx.run_until_parked();
    let mut fake = Fake { key, me, rx };
    fake.drain();
    fake
}

/// A text past the inline limit and a binary file past the cap a tile opens are each saved
/// as exactly the worker's bytes, to the path chosen, with nothing left beside it: the tile
/// that shows no editor takes the palette's action too. A file the worker cannot send says so
/// and leaves nothing at the path.
#[gpui::test]
fn a_file_tiles_copy_is_saved_whole_where_the_panel_says(cx: &mut TestAppContext) {
    let text: Vec<u8> = "fn main() {}\n".repeat(INLINE_FILE_BYTES / 8).into_bytes();
    assert!(text.len() > INLINE_FILE_BYTES);
    let binary: Vec<u8> =
        (0..FILE_BYTES.saturating_add(1)).map(|n| u8::try_from(n % 251).unwrap()).collect();
    let held = HashMap::from([
        ("/w/src/main.rs".to_owned(), text.clone()),
        ("/w/out/core.bin".to_owned(), binary.clone()),
    ]);
    let (asked, mut downloads) = mpsc::unbounded_channel();
    let files = Arc::new(Files { held, asked });
    let (view, cx) = workspace(cx);
    let studio = link_files(&view, cx, files);
    let here = tempfile::tempdir().unwrap();

    let mut version = 0_u64;
    let mut save = |path: &str, read: FileRead, as_name: &str, cx: &mut VisualTestContext| {
        version = version.saturating_add(1);
        let tile = arrives(&view, cx, &studio, ItemKind::File { path: path.to_owned() }, version);
        let key = studio.key;
        view.update_in(cx, |v, _w, cx| {
            v.file_read(key, path, &read, cx);
            v.focus_tile(tile, cx);
        });
        cx.run_until_parked();
        cx.dispatch_action(SaveCopy);
        assert!(cx.did_prompt_for_new_path(), "the save panel is asked");
        let dest = here.path().join(as_name);
        let chosen = dest.clone();
        cx.simulate_new_path_selection(move |_downloads| Some(chosen));
        cx.run_until_parked();
        dest
    };

    let shown = FileRead::Text {
        text: String::from_utf8(text.clone()).unwrap().trim_end_matches('\n').to_owned(),
        size: text.len() as u64,
        modified_ms: WallMs::from_millis(1),
        final_newline: true,
        editorconfig: Vec::new(),
    };
    let dest = save("/w/src/main.rs", shown, "main copy.rs", cx);
    assert_eq!(std::fs::read(&dest).unwrap(), text, "the text, past the inline limit");
    let dest = save("/w/out/core.bin", FileRead::TooLarge { size: FILE_BYTES + 1 }, "core.bin", cx);
    assert!(std::fs::read(&dest).unwrap() == binary, "the binary, past the cap, byte for byte");
    let left: Vec<_> =
        std::fs::read_dir(here.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(left.len(), 2, "no staging is left beside them: {left:?}");

    let gone = FileRead::Missing { error: "No such file or directory".to_owned() };
    let dest = save("/w/gone.txt", gone, "gone.txt", cx);
    assert!(!dest.exists(), "nothing lands for a file the worker cannot send");
    let said = view.read_with(cx, |v, _| v.toast_texts());
    assert!(
        said.iter().any(|t| t == "gone.txt was not saved: No such file or directory"),
        "{said:?}"
    );
    let asked: Vec<String> = std::iter::from_fn(|| downloads.try_recv().ok()).collect();
    assert_eq!(asked, ["/w/src/main.rs", "/w/out/core.bin", "/w/gone.txt"]);
}

/// "Download…" on a folder tile on a Mac asks the save panel where its selected entry goes, on
/// its name, and brings it down there whole, saying where it went; one the worker cannot send
/// is named with why, and nothing lands.
#[gpui::test]
fn download_brings_the_selected_entry_where_the_save_panel_says(cx: &mut TestAppContext) {
    use slopty_proto::folder::{FolderEntry, Listing};
    use slopty_proto::orchestration::FileKind;

    let held = HashMap::from([("/w/in/report.pdf".to_owned(), b"%PDF-1.7".to_vec())]);
    let (asked, mut downloads) = mpsc::unbounded_channel();
    let (view, cx) = workspace(cx);
    let studio = link_files(&view, cx, Arc::new(Files { held, asked }));
    let tile = arrives(&view, cx, &studio, ItemKind::Folder { path: "/w/in".into() }, 1);
    let row = |name: &str| FolderEntry {
        name: name.to_owned(),
        kind: FileKind::File,
        link: false,
        hidden: false,
        size: 8,
        items: None,
        modified_ms: WallMs::from_millis(1),
    };
    let entries = vec![row("gone.txt"), row("report.pdf")];
    let listing = Listing::Listed { dir: "/w/in".to_owned(), entries, total: 2 };
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.folder_listed(key, "/w/in", &listing, cx);
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    let here = tempfile::tempdir().unwrap();
    let download = |cx: &mut VisualTestContext| {
        cx.dispatch_action(crate::folder::SaveToFiles);
        assert!(cx.did_prompt_for_new_path(), "the save panel is asked");
        let dest = here.path().join("chosen.pdf");
        let chosen = dest.clone();
        cx.simulate_new_path_selection(move |_downloads| Some(chosen));
        cx.run_until_parked();
        dest
    };

    let gone = download(cx);
    assert!(!gone.exists(), "nothing lands for an entry the worker cannot send");
    let said = view.read_with(cx, |v, _| v.toast_texts());
    let refused = "gone.txt was not downloaded: No such file or directory";
    assert!(said.iter().any(|t| t == refused), "{said:?}");

    cx.simulate_keystrokes("down");
    let dest = download(cx);
    assert_eq!(std::fs::read(&dest).unwrap(), b"%PDF-1.7", "the worker's bytes, renamed");
    let said = view.read_with(cx, |v, _| v.toast_texts());
    assert!(said.iter().any(|t| *t == format!("Downloaded {}", dest.display())), "{said:?}");
    let left: Vec<_> =
        std::fs::read_dir(here.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(left.len(), 1, "no staging is left beside it: {left:?}");
    let asked: Vec<String> = std::iter::from_fn(|| downloads.try_recv().ok()).collect();
    assert_eq!(asked, ["/w/in/gone.txt", "/w/in/report.pdf"]);
}
