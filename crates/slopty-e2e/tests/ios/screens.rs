//! What a chat asks and opens on the simulator (`cargo xtask e2e ios --sim iphone|ipad`), each a
//! golden in light and in dark, `ios-<device>-compact-<state>-*`: a question of Claude Code's (the
//! stand-in's `AskUserQuestion`) and an approval of a command, each answered at its card by a tap;
//! a shell of the chat's worker that runs what is typed; and the files of the chat's folder.

use std::time::Duration;

use slopty_e2e::harness::{Stack, slopty_json};
use slopty_e2e::{A11yNode, Driver, Dump};

use crate::compact::{PHONE_BELOW, SETTLE, STEP, chats_page, goldens, saying, simulator, tap};

/// The stand-in's question (`ask`): "Which color?", Red or Blue.
const QUESTION: &str = "ask";
/// The stand-in's command asked about before it runs (`bash <cmd>`).
const APPROVAL: &str = "bash rm -rf build";

/// The button of `d` labelled exactly `label`.
fn button<'a>(d: &'a Dump, label: &str) -> Option<&'a A11yNode> {
    d.a11y.iter().find(|n| n.role == "Button" && n.label.as_deref() == Some(label))
}

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

/// Open the chat whose title holds `title`: from the Chats page on a phone (back to it first,
/// from a chat open), from the sidebar's list on an iPad.
async fn open_chat(drv: &mut Driver, phone: bool, title: &str) {
    if phone {
        let dump = drv.dump().await.unwrap();
        if !chats_page(&dump) {
            tap(drv, "Button", "Back to Chats").await;
        }
        let what = format!("{title:?} on the Chats page");
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

/// Whether a thread's read holds a request waiting on the person.
fn asking(read: &serde_json::Value) -> bool {
    read["requests"].as_array().is_some_and(|r| !r.is_empty())
}

/// A question and an approval wait at their cards on the device, and a tap answers each: the
/// stand-in asks "Which color?" (`AskUserQuestion`), whose card offers Red and Blue, and a tap on
/// Red answers it; then a command it wants to run (`rm -rf build`) is asked about, whose card
/// offers Allow and Deny, and a tap on Deny refuses it.
#[tokio::test]
#[ignore = "live: cargo xtask e2e ios"]
async fn a_question_and_an_approval_are_answered_at_their_cards() {
    const MACHINE: &str = "e2e-ios-ask";
    let (mut stack, phone) = device(MACHINE).await;
    let dir = stack.dir.path().to_path_buf();
    let device = if phone { "phone" } else { "pad" };
    let folder = stack.path("repo");
    std::fs::create_dir_all(&folder).unwrap();

    let asked = start(&stack, MACHINE, &folder, QUESTION).await;
    read_until(&stack, &asked, "the question never came", asking).await;
    let drv = &mut stack.driver;
    open_chat(drv, phone, QUESTION).await;
    shown(drv, "the question's card", |d| {
        button(d, "Red").is_some() && button(d, "Blue").is_some()
    })
    .await;
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &dir, device, "question").await;
    tap(drv, "Button", "Red").await;
    let since = std::time::Instant::now();
    loop {
        let read = read_until(&stack, &asked, "", |_| true).await;
        if !asking(&read) {
            break;
        }
        // A pick that waits for its send goes with Return.
        if since.elapsed() > Duration::from_secs(3) {
            stack.driver.keys("enter").await.unwrap();
        }
        assert!(since.elapsed() < STEP, "the card's tap answered it: {read}");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let said = |r: &serde_json::Value| r.to_string().contains("Done: noted.");
    read_until(&stack, &asked, "the answer never reached it", said).await;

    let approval = start(&stack, MACHINE, &folder, APPROVAL).await;
    read_until(&stack, &approval, "the approval never came", asking).await;
    let drv = &mut stack.driver;
    open_chat(drv, phone, "rm -rf build").await;
    shown(drv, "the approval's card", |d| {
        button(d, "Allow").is_some() && button(d, "Deny").is_some() && saying(d, "rm -rf").is_some()
    })
    .await;
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &dir, device, "approval").await;
    tap(drv, "Button", "Deny").await;
    read_until(&stack, &approval, "the tap on Deny answered nothing", |r| !asking(r)).await;
    stack.shutdown().await;
}

/// A shell of the chat's worker and the files of its folder open from the chat on the device, a
/// finger's reach away: on a phone from its navigation bar ("Show terminal", and "Show files" as
/// a page over the chat that leads back to it), on an iPad the files from the titlebar's "Show
/// files panel" and the shell from ⌘J. The shell runs what is typed; the files list the folder's
/// own.
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
    shown(drv, "the chat's composer", |d| d.a11y.iter().any(|n| n.role == "MultilineTextInput"))
        .await;

    if phone {
        tap(drv, "Button", "Show terminal").await;
    } else {
        drv.keys("cmd-j").await.unwrap();
    }
    shown(drv, "a shell at its prompt", |d| {
        d.terminals.iter().any(|t| t.at_prompt) && d.prompts_settled()
    })
    .await;
    drv.ui_insert_text("echo phone surface").await.unwrap();
    drv.keys("enter").await.unwrap();
    shown(drv, "the echo", |d| {
        d.lines_containing("phone surface").len() >= 2 && d.prompts_settled()
    })
    .await;
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &dir, device, "terminal").await;
    if phone {
        tap(drv, "Button", "Hide terminal").await;
    } else {
        drv.keys("cmd-j").await.unwrap();
    }

    if phone {
        tap(drv, "Button", "Show files").await;
    } else {
        tap(drv, "Button", "Show files panel").await;
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
        // The files are a page over the chat, whose back leads to it.
        tap(drv, "Button", "Back to Chat").await;
        shown(drv, "the chat again", |d| d.a11y_node("Button", Some("Back to Chats")).is_some())
            .await;
    } else {
        tap(drv, "Button", "Hide files panel").await;
    }
    stack.shutdown().await;
}
