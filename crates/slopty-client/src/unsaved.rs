//! Edits not yet saved, kept on this device so a quit or a crash loses none of them (hot exit).
//!
//! A file tile's text lives in its editor until it is saved to its worker. The UI puts each
//! dirty tile's whole text here, with the version of the file the edit started from, within
//! moments of each change, and takes it away once the tile is clean again: saved, reloaded, or
//! closed for good. When the app starts again, a tile that opens on the same file on the same
//! worker takes the text back, marked unsaved, and the version it started from tells whether
//! the disk moved on meanwhile.
//!
//! A backup is one per file tile, keyed by its worker and its item: two tiles on one file (two
//! clients opened it at once, or ⌘Z brought one back beside another) keep an edit each, and
//! after a restart each goes back to its own tile. The item is the worker's, so the key
//! outlives the app; a backup whose item is gone (closed elsewhere) is opened again in a tile
//! when the app next starts. One backup is one file, `<blake3 of worker and item>.json`,
//! replaced whole by `slopty_platform::fs::replace` (a temporary file ordered on the device
//! ahead of its rename), so a crash mid-write leaves the one before. The directory is the
//! user's alone (0700) and so is each file (0600): an edit may hold a secret. This is how Zed and
//! VS Code keep theirs (whole text and the base version, conflicts found at the next save), with VS
//! Code's file per document rather than Zed's rows in the layout's database, which lost edits whose
//! tile was not in the layout (`docs/decisions/ui.md`, "An unsaved edit survives a quit or a
//! crash").

use std::collections::HashMap;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{fs, io};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use slopty_core::{ItemId, WallMs};

use crate::layout::WorkerKey;

/// The suffix of the temporary file `slopty_platform::fs::replace` writes a backup to: one left
/// behind was cut short by a crash.
const TORN: &str = "slopty-tmp";

/// One file tile's edit not yet on disk.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Unsaved {
    /// The worker the file is on.
    pub worker: WorkerKey,
    /// The file tile it was typed in: the worker's item, which outlives the app.
    pub item: ItemId,
    /// Its path there.
    pub path: String,
    /// The editor's text, without the final newline the file ends with when `newline`.
    pub text: String,
    /// The file ends with a newline.
    pub newline: bool,
    /// The modification time of the version the edit started from; `None` for a file that was
    /// not there, or not text.
    pub base_modified_ms: Option<WallMs>,
    /// The disk had already moved on under the edit ("Changed on disk"): it comes back as a
    /// conflict whatever the disk holds by then.
    pub conflict: bool,
    /// When the edit was last kept: how long a backup whose worker never came back has waited.
    pub kept_ms: WallMs,
}

/// The backups in one directory. Clones share it, and its order: of two changes to one file,
/// the one numbered later stands, whichever thread gets there first.
#[derive(Clone, Debug)]
pub struct Store {
    dir: PathBuf,
    /// Each backup's own lock, holding the number of the last change made to it. A change to
    /// one file waits only on another change to that file, never on a write of another.
    order: Arc<Mutex<HashMap<PathBuf, Arc<Mutex<u64>>>>>,
}

impl Store {
    /// The backups under `dir`, made when the first is written.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, order: Arc::default() }
    }

    /// Where the backup of file tile `item` on `worker` is kept.
    fn file(&self, worker: WorkerKey, item: ItemId) -> PathBuf {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&worker.value().to_be_bytes());
        hasher.update(item.as_uuid().as_bytes());
        self.dir.join(format!("{}.json", hasher.finalize().to_hex()))
    }

    /// The lock of backup `file`, made the first time it is changed.
    fn slot(&self, file: &Path) -> Arc<Mutex<u64>> {
        Arc::clone(self.order.lock().entry(file.to_owned()).or_default())
    }

    /// Keep `unsaved` as change `seq` to its file's backup, over any before it. `Ok(false)`
    /// when a later change to it was already made, and this one changes nothing.
    ///
    /// # Errors
    ///
    /// When the directory cannot be made or the backup cannot be written whole.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the file's lock is held through the write, so a change numbered earlier never lands after"
    )]
    pub fn put(&self, unsaved: &Unsaved, seq: u64) -> io::Result<bool> {
        let to = self.file(unsaved.worker, unsaved.item);
        let bytes = serde_json::to_vec(unsaved).map_err(io::Error::other)?;
        let slot = self.slot(&to);
        let mut last = slot.lock();
        if *last > seq {
            return Ok(false);
        }
        *last = seq;
        fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.dir)?;
        let new = !to.exists();
        // The one way this codebase replaces a file: ordered on the device, never a full
        // flush of the drive's cache, which costs 3.4 times as much for a small backup
        // (`docs/MEASUREMENTS.md`, "keeping an unsaved edit").
        slopty_platform::fs::replace(&to, &bytes)?;
        if new {
            // Later writes keep the mode; the directory is the user's alone meanwhile.
            fs::set_permissions(&to, fs::Permissions::from_mode(0o600))?;
        }
        Ok(true)
    }

    /// Forget the backup of file tile `item` on `worker`, if there is one, as change `seq` to
    /// it.
    /// `Ok(false)` when a later change to it was already made, and this one changes nothing.
    ///
    /// # Errors
    ///
    /// When it is there and cannot be removed.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the file's lock is held through the removal, as through a write"
    )]
    pub fn remove(&self, worker: WorkerKey, item: ItemId, seq: u64) -> io::Result<bool> {
        let file = self.file(worker, item);
        let slot = self.slot(&file);
        let mut last = slot.lock();
        if *last > seq {
            return Ok(false);
        }
        *last = seq;
        match fs::remove_file(file) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(true),
        }
    }

    /// Every backup kept. One cut short by a crash mid-write is dropped: the one it was
    /// replacing, if any, stands. One that does not read (damaged outside the app) is left as it
    /// is, and not offered.
    #[must_use]
    pub fn all(&self) -> Vec<Unsaved> {
        let Ok(entries) = fs::read_dir(&self.dir) else { return Vec::new() };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            match path.extension().and_then(|e| e.to_str()) {
                Some("json") => {
                    if let Some(unsaved) = read(&path) {
                        out.push(unsaved);
                    } else {
                        tracing::warn!(path = %path.display(), "a backup that does not read");
                    }
                }
                Some(TORN) => {
                    let _gone = fs::remove_file(&path);
                }
                _ => {}
            }
        }
        out.sort_by(|a, b| (a.worker, &a.path, a.item).cmp(&(b.worker, &b.path, b.item)));
        out
    }
}

fn read(path: &Path) -> Option<Unsaved> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = Store::new(dir.path().join("unsaved"));
        (dir, store)
    }

    fn edit(worker: u128, item: ItemId, path: &str, text: &str) -> Unsaved {
        Unsaved {
            worker: WorkerKey::new(worker),
            item,
            path: path.to_owned(),
            text: text.to_owned(),
            newline: true,
            base_modified_ms: Some(WallMs::from_millis(1_000)),
            conflict: false,
            kept_ms: WallMs::from_millis(5_000),
        }
    }

    /// A backup reads back whole; a second from the same tile replaces it; another tile on the
    /// same file, or the same item on another worker, is another backup; one forgotten is gone.
    #[test]
    fn a_backup_is_one_per_tile_on_its_worker() {
        let (_dir, store) = store();
        let (tile, twin) = (ItemId::new(), ItemId::new());
        assert!(store.all().is_empty(), "nothing before the first");
        store.put(&edit(1, tile, "/r/a.rs", "one"), 1).expect("put");
        store.put(&edit(1, tile, "/r/a.rs", "two"), 2).expect("put");
        store.put(&edit(1, twin, "/r/a.rs", "beside"), 3).expect("put");
        store.put(&edit(2, tile, "/r/a.rs", "elsewhere"), 4).expect("put");
        let texts = |store: &Store| {
            let mut texts: Vec<String> = store.all().into_iter().map(|u| u.text).collect();
            texts.sort();
            texts
        };
        assert_eq!(texts(&store), ["beside", "elsewhere", "two"]);
        store.remove(WorkerKey::new(1), tile, 5).expect("remove");
        store.remove(WorkerKey::new(1), tile, 6).expect("a second remove is nothing");
        assert_eq!(texts(&store), ["beside", "elsewhere"]);
    }

    /// A change that reaches the store after a later one to the same backup (a write on its way
    /// off the UI thread, overtaken by the one made as the app quits) changes nothing.
    #[test]
    fn a_change_overtaken_by_a_later_one_is_dropped() {
        let (_dir, store) = store();
        let tile = ItemId::new();
        let shared = store.clone();
        shared.put(&edit(1, tile, "/r/a.rs", "newer"), 5).expect("put");
        store.put(&edit(1, tile, "/r/a.rs", "older"), 4).expect("put");
        store.remove(WorkerKey::new(1), tile, 3).expect("remove");
        assert_eq!(store.all(), [edit(1, tile, "/r/a.rs", "newer")]);
    }

    /// A crash mid-write leaves the backup before it: the cut-short one is dropped. One that
    /// does not read is not offered.
    #[test]
    fn a_write_cut_short_leaves_the_backup_before_it() {
        let (_dir, store) = store();
        let kept = edit(1, ItemId::new(), "/r/a.rs", "kept");
        store.put(&kept, 1).expect("put");
        let file = store.file(kept.worker, kept.item);
        let name = file.file_name().and_then(|n| n.to_str()).expect("a name");
        let torn = store.dir.join(format!(".{name}.1-2-3.{TORN}"));
        fs::write(torn, b"{\"worker\":").expect("a torn write");
        fs::write(store.dir.join("junk.json"), b"not json").expect("junk");
        assert_eq!(store.all(), [kept]);
        let mut left: Vec<String> = fs::read_dir(&store.dir)
            .expect("dir")
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|name| Path::new(name).extension().is_none_or(|e| e != "json"))
            .collect();
        left.sort();
        assert!(left.is_empty(), "the torn write cleared: {left:?}");
    }

    /// What keeping one edit costs off the UI thread, by size: its JSON, then its write synced
    /// three ways (`F_FULLFSYNC`, as `File::sync_all` is on Apple platforms; the barrier
    /// `slopty_platform::fs::replace` uses; none), and [`Store::put`] as it is. Median of
    /// several rounds, in ms.
    #[test]
    #[ignore = "timing: cargo nextest run -p slopty-client --release --run-ignored only put_cost --no-capture"]
    fn put_cost() {
        use std::io::Write as _;
        use std::time::{Duration, Instant};

        const PARTIAL: &str = "partial";

        fn median(rounds: usize, mut f: impl FnMut()) -> f64 {
            let mut took: Vec<Duration> = std::iter::repeat_with(|| {
                let t = Instant::now();
                f();
                t.elapsed()
            })
            .take(rounds)
            .collect();
            took.sort();
            took.get(rounds / 2).map_or(0.0, |d| d.as_secs_f64() * 1e3)
        }

        let (dir, store) = store();
        let scratch = dir.path().join("scratch.json");
        let mut seq = 0_u64;
        for (size, rounds) in [(4_usize << 10, 40), (1 << 20, 20), (16 << 20, 7)] {
            let line = "fn line() -> u32 { 1 } // padding to a source-like width\n";
            let mut text = line.repeat(size / line.len() + 1);
            text.truncate(size);
            let unsaved = edit(1, ItemId::new(), "/r/big.rs", &text);
            let bytes = serde_json::to_vec(&unsaved).expect("json");
            let json = median(rounds, || {
                std::hint::black_box(serde_json::to_vec(&unsaved).expect("json"));
            });
            let full = median(rounds, || {
                let partial = scratch.with_extension(PARTIAL);
                let mut out = fs::File::create(&partial).expect("create");
                out.write_all(&bytes).expect("write");
                out.sync_all().expect("sync");
                fs::rename(&partial, &scratch).expect("rename");
            });
            let barrier = median(rounds, || {
                slopty_platform::fs::replace(&scratch, &bytes).expect("replace");
            });
            let none = median(rounds, || {
                let partial = scratch.with_extension(PARTIAL);
                fs::write(&partial, &bytes).expect("write");
                fs::rename(&partial, &scratch).expect("rename");
            });
            let put = median(rounds, || {
                seq += 1;
                store.put(&unsaved, seq).expect("put");
            });
            println!(
                "{:>8} B: json {json:.2} ms; write + F_FULLFSYNC {full:.2}, + barrier and \
                 directory fsync {barrier:.2}, unsynced {none:.2}; Store::put {put:.2}",
                text.len()
            );
        }
    }

    /// Only the user reads a backup: the directory is made 0700 and every file 0600, whatever
    /// the umask.
    #[test]
    fn a_backup_is_the_users_alone() {
        use std::os::unix::fs::PermissionsExt as _;

        let (_dir, store) = store();
        let tile = ItemId::new();
        store.put(&edit(1, tile, "/r/a.rs", "secret"), 1).expect("put");
        let mode = |p: &Path| fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&store.dir), 0o700);
        let file = store.file(WorkerKey::new(1), tile);
        assert_eq!(mode(&file), 0o600);
    }
}
