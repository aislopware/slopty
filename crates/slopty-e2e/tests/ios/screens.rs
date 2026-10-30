//! A session on the simulator (`cargo xtask e2e ios --sim iphone|ipad`), each state a golden in
//! light and in dark, `ios-<device>-compact-<state>-*`: the session is its terminal, the stand-in
//! Claude Code's TUI, which takes what a finger types; its inspector reads the turn, a sheet over
//! the session on a phone and a pane beside it on an iPad; and a shell of the session's worker
//! and the files of its folder open from it.

use std::time::Duration;

use slopty_e2e::harness::{Stack, slopty_json};
use slopty_e2e::{A11yNode, Driver, Dump};

use crate::compact::{PHONE_BELOW, SETTLE, STEP, chats_page, goldens, saying, simulator, tap};

/// What the session is first asked, its title, and what a finger then types into its TUI.
const FIRST: &str = "Say hello";
const NEXT: &str = "Carry on";

/// The chat's row of `d`: a button whose label holds `words`, the prompt its title comes from.
fn row<'a>(d: &'a Dump, words: &str) -> Option<&'a A11yNode> {
    d.a11y
        .iter()
        .find(|n| n.role == "Button" && n.label.as_deref().is_some_and(|l| l.contains(words)))
}

/// Tap the middle of `node`.
async fn tap_node(drv: &mut Driver, node: &A11yNode) {
    let [left, top, width, height] = node.bounds;
    drv.ui_tap(width.mul_add(0.5, left), height.mul_add(0.5, top)).await.unwrap();
}

/// Wait for `ready`, and on a timeout fail with what the device said.
async fn shown(drv: &mut Driver, what: &str, ready: impl Fn(&Dump) -> bool) -> Dump {
    match drv.wait_for(what, STEP, ready).await {
        Ok(dump) => dump,
        Err(e) => {
            let dump = drv.dump().await.unwrap();
            panic!("{e:#}\nthe device said:\n{}", slopty_e2e::frame_text(&dump.a11y));
        }
    }
}

/// Open the chat whose title holds `title`: from the Sessions page on a phone (back to it first,
/// from a chat open), from the sidebar's list on an iPad.
async fn open_chat(drv: &mut Driver, phone: bool, title: &str) {
    if phone {
        let dump = drv.dump().await.unwrap();
        if !chats_page(&dump) {
            tap(drv, "Button", "Back to Sessions").await;
        }
        let what = format!("{title:?} on the Sessions page");
        let listed = shown(drv, &what, |d| chats_page(d) && row(d, title).is_some()).await;
        tap_node(drv, row(&listed, title).unwrap()).await;
    } else {
        let what = format!("{title:?} in the sidebar");
        let listed = shown(drv, &what, |d| row(d, title).is_some()).await;
        tap_node(drv, row(&listed, title).unwrap()).await;
    }
}

/// A stack on the booted simulator with the stand-in Claude Code, its worker connected, and
/// whether the device is a phone.
async fn device(machine: &str) -> (Stack, bool) {
    let mut stack = Stack::launch_on_simulator_with_claude(machine, simulator()).await.unwrap();
    let dump =
        stack.driver.wait_for("the worker", STEP, |d| d.status == "connected").await.unwrap();
    let phone = dump.window.width < PHONE_BELOW;
    (stack, phone)
}

/// Start the stand-in in `folder` on `machine` with `prompt`, one word of the CLI's each; the
/// agent's id.
async fn start(stack: &Stack, machine: &str, folder: &std::path::Path, prompt: &str) -> String {
    let (address, cli) = (stack.server.address().to_owned(), stack.path("cli"));
    let folder = folder.to_string_lossy().into_owned();
    // After `--`, so a word of the prompt that starts with a dash (`-rf`) stays the prompt's.
    let mut args = vec!["agent", "start", machine, "--folder", &folder, "--"];
    args.extend(prompt.split(' '));
    let started = slopty_json(&address, &cli, &args, b"").await.unwrap();
    started["agent"].as_str().expect("an agent started").to_owned()
}

/// Read `agent`'s thread until `ready` holds; the read that held.
async fn read_until(
    stack: &Stack,
    agent: &str,
    what: &str,
    ready: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let (address, cli) = (stack.server.address().to_owned(), stack.path("cli"));
    let since = std::time::Instant::now();
    loop {
        let args = ["agent", "read", "--thread", agent, "--activity"];
        let read = slopty_json(&address, &cli, &args, b"").await.unwrap();
        if ready(&read) {
            return read;
        }
        assert!(since.elapsed() < STEP.saturating_mul(3), "{what}: {read}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Whether a node past the session's terminal, its right pane or its sheet, says `words`.
fn inspector_says(dump: &Dump, words: &str) -> bool {
    let terminal = dump.a11y.iter().find(|n| n.role == "Terminal").map(|n| n.bounds);
    let sheet = dump.a11y_node("Dialog", Some("Inspector")).map(|sheet| sheet.bounds);
    dump.a11y.iter().any(|n| {
        let [left, top] = [n.bounds[0], n.bounds[1]];
        let beside = terminal
            .is_none_or(|[x, y, width, height]| left >= x + width - 1.0 || top >= y + height - 1.0);
        let inside = sheet.is_some_and(|[x, y, width, height]| {
            left >= x && top >= y && left <= x + width && top <= y + height
        });
        n.role != "Terminal"
            && (beside || inside)
            && [&n.label, &n.value].into_iter().flatten().any(|text| text.contains(words))
    })
}

/// A session on the device is its terminal, the stand-in's TUI, with nothing over, under or
/// beside it: a finger taps it and types the next prompt into the TUI, which answers. The
/// inspector reads the turn once a finger opens it: on a phone a sheet the navigation bar's
/// Inspector puts up over the session and its close puts away, on an iPad the pane beside it
/// that the titlebar's Inspector opens.
#[tokio::test]
#[ignore = "live: cargo xtask e2e ios"]
async fn a_session_is_its_terminal_and_shows_its_inspector() {
    const MACHINE: &str = "e2e-ios-session";
    let (mut stack, phone) = device(MACHINE).await;
    let dir = stack.dir.path().to_path_buf();
    let device = if phone { "phone" } else { "pad" };
    let folder = stack.path("repo");
    std::fs::create_dir_all(&folder).unwrap();
    let agent = start(&stack, MACHINE, &folder, FIRST).await;
    let said = |words: &'static str| {
        move |r: &serde_json::Value| r.to_string().contains(&format!("Done: {words}"))
    };
    read_until(&stack, &agent, "the agent never answered", said(FIRST)).await;

    let drv = &mut stack.driver;
    open_chat(drv, phone, FIRST).await;
    let session = shown(drv, "the session's terminal", |d| {
        d.a11y.iter().any(|n| n.role == "Terminal")
            && d.lines_containing(&format!("Done: {FIRST}")).len() == 1
    })
    .await;
    assert!(
        session.a11y.iter().all(|n| n.role != "MultilineTextInput"),
        "no field beside the terminal: {:?}",
        session.a11y.iter().filter(|n| n.role == "MultilineTextInput").collect::<Vec<_>>()
    );
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &dir, device, "session").await;

    // A finger taps the terminal and types the next prompt into the TUI.
    let [left, top, width, height] =
        session.a11y.iter().find(|n| n.role == "Terminal").map(|n| n.bounds).unwrap();
    drv.ui_tap(left + width / 2.0, top + height / 2.0).await.unwrap();
    shown(drv, "the terminal holding the keyboard", |d| d.focused.starts_with("terminal:")).await;
    drv.ui_insert_text(NEXT).await.unwrap();
    drv.keys("enter").await.unwrap();
    read_until(&stack, &agent, "the typed words never reached the TUI", said(NEXT)).await;

    let drv = &mut stack.driver;
    // On a phone a sheet a finger opens; on an iPad a pane ⌘I summons beside the session.
    if phone {
        tap(drv, "Button", "Show inspector").await;
    } else {
        drv.keys("cmd-i").await.unwrap();
    }
    shown(drv, "the inspector reading the turn", |d| {
        (!phone || d.a11y_node("Dialog", Some("Inspector")).is_some())
            && inspector_says(d, &format!("Done: {NEXT}"))
    })
    .await;
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &dir, device, "inspector").await;
    if phone {
        tap(drv, "Button", "Close inspector").await;
        shown(drv, "the session again", |d| {
            d.a11y_node("Dialog", Some("Inspector")).is_none()
                && d.a11y_node("Button", Some("Show inspector")).is_some()
        })
        .await;
    }
    stack.shutdown().await;
}

/// A shell of the session's worker and the files of its folder open from the session on the
/// device: on an iPad the shell split beside the session (⌘D), which a phone, one session at a
/// time, never splits; the files on a phone from the navigation bar's "Show files", a page over
/// the session that leads back to it, and on an iPad summoned beside the session (⌘E) and closed
/// by a finger on their pane's ✕. The shell runs what is typed; the files list the folder's own.
#[tokio::test]
#[ignore = "live: cargo xtask e2e ios"]
async fn a_chats_shell_and_its_files_open_on_the_device() {
    const MACHINE: &str = "e2e-ios-surfaces";
    let (mut stack, phone) = device(MACHINE).await;
    let dir = stack.dir.path().to_path_buf();
    let device = if phone { "phone" } else { "pad" };
    let folder = stack.path("repo");
    std::fs::create_dir_all(folder.join("src")).unwrap();
    std::fs::write(folder.join("README.md"), "# Lexer\n\nTokens for the notes language.\n")
        .unwrap();
    std::fs::write(
        folder.join("src/lexer.rs"),
        "pub fn lex(text: &str) -> usize {\n    text.len()\n}\n",
    )
    .unwrap();
    let agent = start(&stack, MACHINE, &folder, "Say hello").await;
    let said = |r: &serde_json::Value| r.to_string().contains("Done: Say hello");
    read_until(&stack, &agent, "the agent never answered", said).await;
    let title = "Say hello";
    let drv = &mut stack.driver;
    open_chat(drv, phone, title).await;
    shown(drv, "the session's terminal", |d| d.a11y.iter().any(|n| n.role == "Terminal")).await;

    if !phone {
        drv.keys("cmd-d").await.unwrap();
        shown(drv, "a shell beside the session at its prompt", |d| {
            d.a11y.iter().filter(|n| n.role == "Terminal").count() == 2
                && d.terminals.iter().any(|t| t.at_prompt)
                && d.prompts_settled()
        })
        .await;
        drv.ui_insert_text("echo pad surface").await.unwrap();
        drv.keys("enter").await.unwrap();
        shown(drv, "the echo", |d| {
            d.lines_containing("pad surface").len() >= 2 && d.prompts_settled()
        })
        .await;
        tokio::time::sleep(SETTLE).await;
        goldens(drv, &dir, device, "terminal").await;
        // ⌘W closes the shell's pane, the one with the keyboard.
        drv.keys("cmd-w").await.unwrap();
        shown(drv, "the session alone", |d| {
            d.a11y.iter().filter(|n| n.role == "Terminal").count() == 1
        })
        .await;
    }

    if phone {
        tap(drv, "Button", "Show files").await;
    } else {
        drv.keys("cmd-e").await.unwrap();
    }
    shown(drv, "the folder's files", |d| {
        (!phone || d.a11y_node("Heading", Some("Files")).is_some())
            && saying(d, "README.md").is_some()
            && (saying(d, "src").is_some() || saying(d, "lexer.rs").is_some())
    })
    .await;
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &dir, device, "files").await;
    if phone {
        // The files are a page over the session, whose back leads to it.
        tap(drv, "Button", "Back to Session").await;
        shown(drv, "the session again", |d| {
            d.a11y_node("Button", Some("Back to Sessions")).is_some()
        })
        .await;
    } else {
        tap(drv, "Button", "Close Files").await;
        shown(drv, "the session alone", |d| d.a11y_node("Group", Some("Files")).is_none()).await;
    }
    stack.shutdown().await;
}
