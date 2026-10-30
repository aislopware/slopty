//! The clipboard between this client and its worker while a shell split beside the session (⌘D)
//! has the keyboard, over the run's own named pasteboards (the person's is never touched): a copy
//! on the worker reaches this client, items and all, this client's copy is on the worker's
//! pasteboard for anything there that reads it (`pbpaste`, a menu's Paste), and the worker is
//! no longer watched once the keyboard leaves the shell.
//!
//! Nothing is typed into the worker's programs to copy: the worker's own pasteboard is where a
//! copy there would land, and that is what the tests write and read.

#![cfg(target_os = "macos")]

use std::time::{Duration, Instant};

use slopty_e2e::harness::pasteboard_name;
use slopty_e2e::{Driver, Dump, Stack};
use slopty_platform::pasteboard::{MacPasteboard, Pasteboard as _};

/// How long a worker round trip may take.
const STEP: Duration = Duration::from_secs(30);
const TEXT: &str = "public.utf8-plain-text";

/// Some worker's clipboard is watched by this client.
fn watched(d: &Dump) -> bool {
    d.workers.iter().any(|w| w.clipboard_watched)
}

/// ⌘D's shell at its prompt with the keyboard, its worker watched, and the watch in effect: a
/// copy the worker makes before it hears the watch is, by design, never announced, and the
/// keys' echo comes back behind the watch on the one control stream.
async fn shell_watched(drv: &mut Driver) {
    drv.wait_for("the worker", STEP, |d| d.status == "connected").await.unwrap();
    drv.keys("cmd-d").await.unwrap();
    drv.wait_for("the split's shell with the keyboard, its worker watched", STEP, |d| {
        d.terminals.iter().any(|t| t.at_prompt) && d.focused.starts_with("terminal:") && watched(d)
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

/// The texts of each item on the pasteboard `name`, in order.
fn item_texts(name: &str) -> Vec<String> {
    let board = MacPasteboard::named(name);
    (0..board.items().len())
        .filter_map(|i| board.item_data(i, TEXT))
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .collect()
}

/// With the split's shell holding the keyboard, a copy of several items on its worker (Finder's
/// several files, a note app's several entries) arrives here as the same items, in order, and
/// what this client copies then is on the worker's pasteboard. Closing the shell's pane (⌘W)
/// takes the keyboard from the shell, and the worker is no longer watched.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_split_shell_shares_the_clipboard_with_its_worker() {
    let mut stack = Stack::launch_at_home("e2e-clip").await.unwrap();
    let worker_name = pasteboard_name(stack.dir.path(), "worker");
    let app_name = pasteboard_name(stack.dir.path(), "app");
    shell_watched(&mut stack.driver).await;
    let (worker, app) = (MacPasteboard::named(&worker_name), MacPasteboard::named(&app_name));

    let (one, two) = (format!("first {}", std::process::id()), "second".to_owned());
    worker.copy_items(&[&[(TEXT, one.as_bytes())], &[(TEXT, two.as_bytes())]]);
    let reader = app_name.clone();
    let items =
        read_until(move || Some(item_texts(&reader)), &vec![one.clone(), two.clone()]).await;
    assert_eq!(items, Some(vec![one, two]), "two items, in order: {:?}", app.items());

    let text = format!("copied on this client {}", std::process::id());
    app.copy(&[(TEXT, text.as_bytes())]);
    let reader = worker_name.clone();
    let there = read_until(move || MacPasteboard::named(&reader).text(), &text).await;
    assert_eq!(there.as_deref(), Some(&*text), "the worker's pasteboard holds it");

    let drv = &mut stack.driver;
    drv.keys("cmd-w").await.unwrap();
    drv.wait_for("the keyboard away from the shell, nothing watched", STEP, |d| {
        !d.focused.starts_with("terminal:") && !watched(d)
    })
    .await
    .unwrap();

    stack.shutdown().await;
    worker.release();
    app.release();
}
