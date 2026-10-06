//! The clipboard between this client and its workers, over the run's own named pasteboards (the
//! human's is never touched): a copy on one worker made ready to paste in another's window, a
//! copy of several items arriving as its items, and the focused client's copy on the worker's
//! pasteboard for anything there that reads it (`pbpaste`, a menu's Paste).
//!
//! Nothing is clicked or typed into a remote window: the worker's own pasteboard is where each
//! paste there would read, and that is what the tests read.

#![cfg(target_os = "macos")]

use std::time::{Duration, Instant};

use slopty_e2e::harness::{SecondWorker, pasteboard_name};
use slopty_e2e::{Command, Driver, Dump, Stack};
use slopty_platform::pasteboard::{MacPasteboard, Pasteboard as _};

use super::gallery::STEP;

const TEXT: &str = "public.utf8-plain-text";
/// The second worker's switch to its drawn screen: a window to stream without capturing.
const SYNTHETIC: (&str, &str) = ("SLOPTY_SYNTHETIC_SCREEN", "1");
/// The drawn screen's first window, as `synthetic::WINDOWS` lists it.
const EDITOR: (u32, &str) = (7001, "Synthetic editor");

/// The stack's worker watched from a shell with the keyboard, and the watch in effect: a copy
/// the worker makes before it hears the watch is, by design, never announced, and the keys'
/// echo comes back behind the watch on the one control stream.
async fn watched(drv: &mut Driver) {
    drv.wait_for("the worker's clipboard watched", STEP, |d| {
        d.status == "connected"
            && d.focus.as_deref() == Some("terminal")
            && d.workers.iter().any(|w| w.clipboard_watched)
    })
    .await
    .unwrap();
    drv.type_text("clipboard-watched").await.unwrap();
    drv.wait_for("the keys behind the watch echoed", STEP, |d| {
        !d.lines_containing("clipboard-watched").is_empty()
    })
    .await
    .unwrap();
    drv.keys("ctrl-u").await.unwrap();
}

/// Poll `read` off the async runtime (a promised type is fetched from its owner while the read
/// waits) until it gives `want`, for at most [`STEP`]; what it gave last.
async fn read_until<T, F>(read: F, want: &T) -> Option<T>
where
    T: PartialEq + Send + Sync + Clone + 'static,
    F: Fn() -> Option<T> + Send + Sync + Clone + 'static,
{
    let started = Instant::now();
    loop {
        let got = tokio::task::spawn_blocking(read.clone()).await.unwrap();
        if got.as_ref() == Some(want) || started.elapsed() > STEP {
            return got;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn watching(d: &Dump, name: &str) -> bool {
    d.workers.iter().any(|w| w.name == name && w.clipboard_watched)
}

/// Text copied on one worker, while its shell has the keyboard, reaches this client; when a
/// window of a second worker is focused, that worker's pasteboard holds it, so a paste in the
/// window pastes it. The copy is relayed through this client and fetched from the first worker
/// when the second's pasteboard is read.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn text_copied_on_one_worker_is_ready_to_paste_in_another_workers_window() {
    let mut stack = Stack::launch("e2e-clip-relay").await.unwrap();
    let a_name = pasteboard_name(stack.dir.path(), "worker");
    let app_name = pasteboard_name(stack.dir.path(), "app");
    watched(&mut stack.driver).await;
    let link = slopty_shape::Link::CLEAR;
    let second =
        SecondWorker::launch_env("remote", link, &stack.server, &[SYNTHETIC]).await.unwrap();
    let b_name = second.pasteboard();
    let drv = &mut stack.driver;
    let d = drv
        .wait_for("both workers connected, a shell on each", STEP, |d| {
            d.workers.iter().filter(|w| w.status == "connected").count() == 2
                && d.items.iter().any(|i| i.worker == "remote" && i.kind == "terminal")
        })
        .await
        .unwrap();
    let first = d.workers.iter().find(|w| w.name != "remote").map(|w| w.name.clone()).unwrap();
    drv.wait_for("the first worker still watched", STEP, |d| watching(d, &first)).await.unwrap();

    let text = format!("relayed between workers {}", std::process::id());
    let (a, app) = (MacPasteboard::named(&a_name), MacPasteboard::named(&app_name));
    a.copy(&[(TEXT, text.as_bytes())]);
    let reader = app_name.clone();
    let here = read_until(move || MacPasteboard::named(&reader).text(), &text).await;
    assert_eq!(here.as_deref(), Some(&*text), "the first worker's copy reached this client");

    let session = d
        .items
        .iter()
        .find(|i| i.worker == "remote" && i.kind == "terminal")
        .and_then(|i| i.session.clone())
        .unwrap();
    drv.reveal(&session).await.unwrap();
    drv.ok(&Command::PickWindow { window: EDITOR.0, title: EDITOR.1.to_owned() }).await.unwrap();
    drv.wait_for("the second worker's window focused and watched", STEP, |d| {
        d.items.iter().any(|i| i.worker == "remote" && i.kind == "window" && i.active)
            && watching(d, "remote")
    })
    .await
    .unwrap();
    let reader = b_name.clone();
    let there = read_until(move || MacPasteboard::named(&reader).text(), &text).await;
    assert_eq!(there.as_deref(), Some(&*text), "on the second worker's pasteboard");

    stack.shutdown().await;
    second.shutdown().await;
    a.release();
    app.release();
}

/// A copy of several items on the worker (Finder's several files, a note app's several
/// entries) arrives here as the same items, in order, each with its own text.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_multi_item_copy_arrives_as_its_items() {
    let mut stack = Stack::launch("e2e-clip-items").await.unwrap();
    let worker_name = pasteboard_name(stack.dir.path(), "worker");
    let app_name = pasteboard_name(stack.dir.path(), "app");
    watched(&mut stack.driver).await;
    let (worker, app) = (MacPasteboard::named(&worker_name), MacPasteboard::named(&app_name));
    let (one, two) = (format!("first {}", std::process::id()), "second".to_owned());
    worker.copy_items(&[&[(TEXT, one.as_bytes())], &[(TEXT, two.as_bytes())]]);
    let reader = app_name.clone();
    let items = read_until(
        move || {
            let app = MacPasteboard::named(&reader);
            let texts: Vec<String> = (0..app.items().len())
                .filter_map(|i| app.item_data(i, TEXT))
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .collect();
            Some(texts)
        },
        &vec![one.clone(), two.clone()],
    )
    .await;
    assert_eq!(items, Some(vec![one, two]), "two items, in order: {:?}", app.items());
    stack.shutdown().await;
    worker.release();
    app.release();
}

/// What this client copied is on the worker's pasteboard once a tile of the worker takes the
/// focus, for anything there that reads the pasteboard rather than pastes with ⌘V: `pbpaste`,
/// a menu's Paste, a program reading it itself. Copied while a new note had the focus (a file
/// tile: the worker was not watched), it is mirrored as the shell takes the focus back.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_focused_clients_copy_is_on_the_workers_pasteboard() {
    let mut stack = Stack::launch("e2e-clip-mirror").await.unwrap();
    let worker_name = pasteboard_name(stack.dir.path(), "worker");
    let app_name = pasteboard_name(stack.dir.path(), "app");
    let drv = &mut stack.driver;
    watched(drv).await;
    drv.keys("cmd-shift-n").await.unwrap();
    drv.wait_for("a note focused, the worker no longer watched", STEP, |d| {
        d.items.iter().any(|i| i.kind == "file" && i.active)
            && d.workers.iter().all(|w| !w.clipboard_watched)
    })
    .await
    .unwrap();
    let (worker, app) = (MacPasteboard::named(&worker_name), MacPasteboard::named(&app_name));
    let text = format!("copied on this client {}", std::process::id());
    app.copy(&[(TEXT, text.as_bytes())]);

    drv.keys("cmd-alt-left").await.unwrap();
    drv.wait_for("the shell focused and watched again", STEP, |d| {
        d.items.iter().any(|i| i.kind == "terminal" && i.active)
            && d.workers.iter().any(|w| w.clipboard_watched)
    })
    .await
    .unwrap();
    let reader = worker_name.clone();
    let there = read_until(move || MacPasteboard::named(&reader).text(), &text).await;
    assert_eq!(there.as_deref(), Some(&*text), "the worker's pasteboard holds it");
    stack.shutdown().await;
    worker.release();
    app.release();
}
