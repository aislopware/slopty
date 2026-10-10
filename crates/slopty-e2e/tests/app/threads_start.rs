//! An agent's thread started from the palette in the real app: the machine's facts say which
//! agents it can start, "New agent…" passes over the agent step when it has one, the folder step
//! offers the shell's folder first, the new thread's own composer takes the first message (here
//! none), and the agent the machine starts opens in its terminal's tile, on the thread face. Claude
//! Code is `slopty-stub-claude`, first on the worker's `PATH`; no real agent runs.

use std::time::Duration;

use slopty_e2e::harness::ProjectStack;
use slopty_e2e::{Driver, Dump};

/// A server round trip, the agent's terminal opening, the app hearing of it.
const STEP: Duration = Duration::from_secs(30);
/// The palette's one start, as it begins.
const LINE: &str = "New agent\u{2026}";
/// The new thread's composer, which writes its first message.
const FIELD: &str = "Message";

/// Wait until the server says the worker has Claude Code: the palette offers what the
/// worker's facts list, and a worker lists it once its facts are gathered.
async fn claude_installed(stack: &ProjectStack) {
    let started = tokio::time::Instant::now();
    loop {
        let workers = stack.slopty(&["workers"]).await.unwrap();
        let listed = workers.as_array().into_iter().flatten();
        if listed.into_iter().any(|w| w["facts"]["agents"]["claude"].is_string()) {
            return;
        }
        assert!(started.elapsed() < STEP, "Claude Code on the worker: {workers}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Whether the palette's first line, the one ↩ runs, is "New agent…".
fn offered(d: &Dump) -> bool {
    d.a11y
        .iter()
        .find(|n| n.role == "ListBoxOption")
        .and_then(|n| n.label.as_deref())
        .is_some_and(|l| l.starts_with(LINE))
}

fn palette_up(d: &Dump) -> bool {
    d.a11y_node("Dialog", Some("Commands")).is_some()
}

/// Open the palette and type `LINE` until it offers it: the agents come with the server's
/// answer about the worker's facts, which each opening asks again.
async fn palette_offering(drv: &mut Driver) {
    let started = tokio::time::Instant::now();
    loop {
        drv.keys("cmd-shift-p").await.unwrap();
        drv.wait_for("the palette", STEP, palette_up).await.unwrap();
        drv.type_text("new agent\u{2026}").await.unwrap();
        let shown = drv.wait_for(LINE, Duration::from_secs(2), offered).await;
        if shown.is_ok() {
            return;
        }
        assert!(started.elapsed() < STEP, "the palette never offered {LINE}");
        drv.keys("escape").await.unwrap();
        drv.wait_for("the palette closed", STEP, |d| !palette_up(d)).await.unwrap();
    }
}

/// "New agent…" from the palette asks the folder (one agent and one machine, so neither step),
/// and ↩ there opens the new thread's tile on its composer, for its first message; ↩ on it
/// left empty starts Claude Code bare in the focused shell's folder. The agent's terminal takes
/// that tile, on its thread face with the keyboard in the composer, and no thread tile opens;
/// the shell's tile is still there.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_palette_start_opens_the_agents_terminal_on_its_thread() {
    let mut stack = ProjectStack::launch("studio").await.unwrap();
    claude_installed(&stack).await;
    let drv = &mut stack.driver;
    palette_offering(drv).await;
    drv.keys("enter").await.unwrap();
    drv.wait_for("the folder step", STEP, |d| {
        let options = || d.a11y.iter().filter(|n| n.role == "ListBoxOption");
        options().next().is_some()
            && options().all(|n| n.label.as_deref().is_none_or(|l| !l.starts_with(LINE)))
    })
    .await
    .unwrap();
    drv.keys("enter").await.unwrap();
    drv.wait_for("the palette closed", STEP, |d| !palette_up(d)).await.unwrap();
    drv.wait_for("the first message's field", STEP, |d| {
        d.a11y.iter().any(|n| {
            n.role == "MultilineTextInput" && n.focused && n.label.as_deref() == Some(FIELD)
        })
    })
    .await
    .unwrap();
    drv.keys("enter").await.unwrap();
    let dump = drv
        .wait_for("the agent's terminal on its thread", STEP, |d| {
            d.items.iter().filter(|i| i.kind == "terminal").count() == 2
                && d.focused.starts_with("thread:")
        })
        .await
        .unwrap();
    assert_eq!(dump.notice, None, "a start that went through says nothing");
    assert!(dump.items.iter().all(|i| i.kind != "thread"), "no thread tile: {:?}", dump.items);
    stack.shutdown().await;
}
