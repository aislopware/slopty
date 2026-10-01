//! Drags from this Mac onto a remote display, in the real app against a real worker: the app's
//! drop destination is handed each step of the drag as the platform's would hand it
//! (`Command::DragOver`, `DragDrop`, `DragLeave`), the files go up into the drag's landing on the
//! worker as it hovers, and the worker carries the drag on the drawn display
//! (`SLOPTY_SYNTHETIC_SCREEN`). The worker records its drops (`SLOPTY_DND_RECORD`): no helper
//! starts and no system drag runs there, a scripted one answers each step with the operation the
//! test chose, and each release writes what the landing held at that moment. So the test owns
//! everything it drives and judges the drop on digests, never on a picture.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use slopty_e2e::{Command, Driver, Dump, Stack};

use super::gallery::{STEP, first_shell};

/// The worker's switch to its drawn screen.
const SYNTHETIC: (&str, &str) = ("SLOPTY_SYNTHETIC_SCREEN", "1");
/// The worker's switch to recording its drops (`apps/slopty-worker/src/dnd.rs`, `RECORD`).
const RECORD: &str = "SLOPTY_DND_RECORD";
/// The window the tiles are laid out in.
const WINDOW: (f32, f32) = (900.0, 600.0);
/// The display's label, as `synthetic::DISPLAY` names it.
const DISPLAY: &str = "Remote display 1";
/// The first frame's wait: an encoder and a decoder session to build.
const FIRST_FRAMES: Duration = Duration::from_secs(90);
/// How often a file or the worker's answer is looked for.
const POLL: Duration = Duration::from_millis(20);
/// The big file's size: many chunks, so it is still going up for a while after the drag enters.
const BIG: usize = 24 << 20;

/// A stack whose worker draws its screen and records every drop it is handed in `record`,
/// answering `op` (`copy` or `none`) from every target, with the drawn display in a tile.
async fn launch(record: &Path, op: &str) -> (Stack, (f32, f32)) {
    let recording = format!("{}:{op}", record.display());
    let mut stack =
        Stack::launch_with("e2e-worker", &[SYNTHETIC, (RECORD, &recording)]).await.unwrap();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;
    drv.ok(&Command::AddDisplay).await.unwrap();
    let dump = drv
        .wait_for("the drawn display's frames", FIRST_FRAMES, |d| {
            d.screens.first().is_some_and(|s| s.frames >= 30)
        })
        .await
        .unwrap();
    let at = middle(&dump);
    (stack, at)
}

/// The middle of the display's picture, in window points.
fn middle(dump: &Dump) -> (f32, f32) {
    let node = dump
        .a11y_node("Image", Some(DISPLAY))
        .unwrap_or_else(|| panic!("no picture: {:#?}", dump.a11y));
    let [x, y, w, h] = node.bounds;
    (x + w / 2.0, y + h / 2.0)
}

/// A small text file and a big one of bytes that do not repeat, under `dir`.
fn sources(dir: &Path) -> Vec<PathBuf> {
    let note = dir.join("note.txt");
    std::fs::write(&note, b"the quick brown fox\n").unwrap();
    let big = dir.join("frames.bin");
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let bytes: Vec<u8> = std::iter::repeat_with(|| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state.to_le_bytes()[0]
    })
    .take(BIG)
    .collect();
    std::fs::write(&big, bytes).unwrap();
    vec![note, big]
}

/// Carry the drag of `paths` to `at` until the worker's answer is `op`; the drag and how long
/// the answer took from the first step.
async fn hover(drv: &mut Driver, paths: &[&Path], at: (f32, f32), op: &str) -> (String, Duration) {
    let start = Instant::now();
    let mut last = None;
    while start.elapsed() < STEP {
        let (now, drag) = drv.drag_over(paths, at.0, at.1).await.unwrap();
        if now == op
            && let Some(drag) = drag
        {
            return (drag, start.elapsed());
        }
        last = Some(now);
        tokio::time::sleep(POLL).await;
    }
    panic!("the drag over the display never read {op}: last {last:?}");
}

/// The record's line for `drag`: each file the landing held at the release, by name, with its
/// BLAKE3 digest.
async fn released(record: &Path, drag: &str) -> Vec<(String, String)> {
    let start = Instant::now();
    let want = format!("release drag={drag}");
    while start.elapsed() < STEP {
        let text = std::fs::read_to_string(record).unwrap_or_default();
        let line = text.lines().find_map(|l| {
            l.strip_prefix(&want).filter(|rest| rest.is_empty() || rest.starts_with(' '))
        });
        if let Some(entries) = line {
            return entries
                .split_whitespace()
                .filter_map(|entry| entry.split_once('='))
                .map(|(name, digest)| (name.to_owned(), digest.to_owned()))
                .collect();
        }
        tokio::time::sleep(POLL).await;
    }
    panic!("no release of {drag} recorded: {:?}", std::fs::read_to_string(record));
}

/// The name and BLAKE3 digest of each of `paths`, in the record's order (by name).
fn digests(paths: &[PathBuf]) -> Vec<(String, String)> {
    let mut named: Vec<(String, String)> = paths
        .iter()
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, blake3::hash(&std::fs::read(p).unwrap()).to_hex().to_string())
        })
        .collect();
    named.sort();
    named
}

/// Wait until `path` holds a whole file of `size` bytes: its partial renamed into place.
async fn whole(path: &Path, size: usize) {
    let start = Instant::now();
    while start.elapsed() < STEP {
        if std::fs::metadata(path).is_ok_and(|m| usize::try_from(m.len()) == Ok(size)) {
            return;
        }
        tokio::time::sleep(POLL).await;
    }
    panic!("{} never landed whole", path.display());
}

/// Wait until nothing is left at `path`; how long it took.
async fn gone(path: &Path) -> Duration {
    let start = Instant::now();
    while start.elapsed() < STEP {
        if !path.exists() {
            return start.elapsed();
        }
        tokio::time::sleep(POLL).await;
    }
    panic!("{} is still there: {:?}", path.display(), walk(path));
}

/// Everything under `dir`.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        }
        out.push(path);
    }
    out
}

/// Files dragged from this Mac onto the drawn display go up into the drag's landing while it
/// hovers, the worker's target answers copy and the badge says so, and the drop is let go on
/// the worker only once every file is whole there: the record of the release holds each file
/// with the digest of its source here.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn files_dropped_on_a_display_are_whole_on_the_worker_when_it_lets_go() {
    let scratch = tempfile::tempdir().unwrap();
    let record = scratch.path().join("record");
    let files = sources(scratch.path());
    let (mut stack, at) = launch(&record, "copy").await;
    let drv = &mut stack.driver;
    let paths: Vec<&Path> = files.iter().map(PathBuf::as_path).collect();

    // The client takes a drop for a copy until the worker says otherwise: the first step reads
    // copy whatever the worker answers, and the refusal below is what proves its word arrives.
    let (drag, _first) = hover(drv, &paths, at, "copy").await;
    let dropped = Instant::now();
    assert!(drv.drag_drop(at.0, at.1).await.unwrap(), "the drop is taken");
    let held = released(&record, &drag).await;
    let release = dropped.elapsed();
    println!(
        "MEASURE drop on a display: drop → release with {} MiB whole {:.1} ms",
        BIG >> 20,
        release.as_secs_f64() * 1e3,
    );
    assert_eq!(held, digests(&files), "the landing at the release");
    stack.shutdown().await;
}

/// A worker whose every target refuses: its word reaches the badge, so the drop is refused here
/// and slides back, and the worker never lets go. Neither that drag's landing nor one left before
/// its drop stays on the worker, whole files and partial ones alike.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_refused_or_left_drag_leaves_nothing_on_the_worker() {
    let scratch = tempfile::tempdir().unwrap();
    let record = scratch.path().join("record");
    let files = sources(scratch.path());
    let (mut stack, at) = launch(&record, "none").await;
    let drops = stack.dir.path().join("drops");
    let drv = &mut stack.driver;
    let paths: Vec<&Path> = files.iter().map(PathBuf::as_path).collect();

    let (refused, answered) = hover(drv, &paths, at, "none").await;
    println!("MEASURE refusing target: first step → none {:.1} ms", answered.as_secs_f64() * 1e3);
    let landing = drops.join(&refused);
    whole(&landing.join("note.txt"), 20).await;
    assert!(!drv.drag_drop(at.0, at.1).await.unwrap(), "refused here: it slides back");
    let cleared = gone(&landing).await;
    println!("MEASURE refused drop: its landing gone {:.1} ms after", cleared.as_secs_f64() * 1e3);

    let (left, _answered) = hover(drv, &paths, at, "none").await;
    assert_ne!(left, refused, "a drag of its own");
    let landing = drops.join(&left);
    whole(&landing.join("note.txt"), 20).await;
    drv.ok(&Command::DragLeave).await.unwrap();
    let cleared = gone(&landing).await;
    println!("MEASURE left drag: its landing gone {:.1} ms after", cleared.as_secs_f64() * 1e3);

    assert!(std::fs::read_to_string(&record).unwrap_or_default().is_empty(), "never let go");
    stack.shutdown().await;
}
