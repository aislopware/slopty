//! The agent's prompt box, mirrored live in the inspector beside its terminal, on a real stack:
//! the stand-in Claude Code (`slopty-stub-claude`) loads Slopty's mod and relays its box as the
//! person types (`docs/decisions/claude-code.md`, "The inspector mirrors the TUI's prompt box
//! through a mod of Slopty's"), and records a prompt typed behind a turn under way as Claude Code
//! queues it. The inspector shows the draft with its picture's placeholder and thumbnail, and a
//! press on the thumbnail opens the picture large; sent from the terminal, the box is empty and
//! the mirror goes; a prompt typed behind the next turn shows queued until it runs. Every word is
//! typed in the terminal: the inspector types nothing.

#![cfg(target_os = "macos")]

use slopty_e2e::harness::{ServedApp, pasteboard_name};
use slopty_e2e::{Command, Driver, Dump};
use slopty_platform::pasteboard::MacPasteboard;

use crate::served::{
    SETTLE, STEP, click, goldens, middle, node, open_thread, says, seen, start, thread_until,
    type_in_terminal,
};

/// The worker's name, as the CLI names the machine.
const MACHINE: &str = "prompt";
/// The turn the session starts with, held until Esc.
const HELD: &str = "hold the release";
/// The draft's words, before the picture pasted after them.
const DRAFT: &str = "Look at ";
/// The draft as the box shows it, the picture as its placeholder.
const SHOWN: &str = "Look at [Image #1]";
/// A second turn, held so a prompt can queue behind it.
const HELD_AGAIN: &str = "hold the build";
/// What the person types behind the held turn.
const QUEUED: &str = "then run the tests";
/// The pasteboard's type for plain text.
const TEXT: &str = "public.utf8-plain-text";

/// A small picture to paste: a gradient, so its thumbnail reads as a picture in the render.
fn picture() -> Vec<u8> {
    let (width, height) = (96_u32, 64_u32);
    let shade = |v: u32, of: u32| {
        let step = v.saturating_mul(180).checked_div(of).unwrap_or_default();
        u8::try_from(step.saturating_add(40)).unwrap_or(u8::MAX)
    };
    let image = image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([shade(x, width), shade(y, height), 210, 255])
    });
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).unwrap();
    png.into_inner()
}

/// Whether the inspector shows the prompt box at all.
fn mirrored(dump: &Dump) -> bool {
    node(dump, "Group", "Prompt box").is_some()
}

/// Give the session's terminal the keyboard again, from wherever it went.
async fn terminal_holds_the_keyboard(drv: &mut Driver) {
    let dump = drv.dump().await.unwrap();
    if dump.focused.starts_with("terminal:") {
        return;
    }
    let terminal = dump.a11y.iter().find(|n| n.role == "Terminal").expect("the terminal");
    let (x, y) = middle(terminal);
    drv.click(x, y).await.unwrap();
    seen(drv, "the terminal holding the keyboard", STEP, |d| d.focused.starts_with("terminal:"))
        .await;
}

/// A draft shows its words with a pasted picture's placeholder drawn apart and its thumbnail,
/// and the thumbnail opens the picture in the lightbox; a prompt typed behind a turn under way
/// shows queued under the timeline; once the TUI sends what the box holds, the mirror goes.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_inspector_mirrors_the_tuis_prompt_box() {
    let mut stack = ServedApp::launch(MACHINE).await.unwrap();
    let repo = stack.path("repo");
    let pasted = stack.path("screenshot.png");
    std::fs::write(&pasted, picture()).unwrap();
    let board = pasteboard_name(stack.dir.path(), "app");
    let renders = stack.path("");
    let agent = start(&stack, MACHINE, &repo, HELD).await;
    let drv = &mut stack.driver;
    open_thread(drv, 1).await;

    // Stopped, the turn says so and no more: the frame holds nothing that moves or tells the
    // time, and the box nothing yet.
    drv.keys("escape").await.unwrap();
    seen(drv, "the turn stopped", STEP, |d| {
        d.a11y.iter().any(|n| n.role == "Status" && n.value.as_deref() == Some("interrupted"))
    })
    .await;
    assert!(!mirrored(&drv.dump().await.unwrap()), "an empty box shows nothing");

    // A draft with a picture pasted after its words: the words, the placeholder, the thumbnail.
    type_in_terminal(drv, DRAFT).await;
    let path = pasted.to_string_lossy().into_owned();
    MacPasteboard::named(&board).copy(&[(TEXT, path.as_bytes())]);
    drv.keys("cmd-v").await.unwrap();
    seen(drv, "the draft and its picture", STEP, |d| {
        node(d, "Paragraph", SHOWN).is_some() && node(d, "Button", "Image #1").is_some()
    })
    .await;
    drv.ok(&Command::Move { x: 1300.0, y: 60.0 }).await.unwrap();
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &renders, "prompt-box", &[]).await;

    // The thumbnail opens the picture large; Esc closes it.
    click(drv, "Button", "Image #1").await;
    seen(drv, "the picture large", STEP, |d| node(d, "Dialog", "Image #1").is_some()).await;
    drv.keys("escape").await.unwrap();
    seen(drv, "the lightbox closed", STEP, |d| node(d, "Dialog", "Image #1").is_none()).await;

    // Sent from the terminal, the box is empty and the mirror goes.
    terminal_holds_the_keyboard(drv).await;
    drv.keys("enter").await.unwrap();
    let answered = format!("Done: {SHOWN}");
    thread_until(&stack, &agent, "took the draft", |r| says(r, &answered)).await;
    let drv = &mut stack.driver;
    seen(drv, "the mirror gone", STEP, |d| !mirrored(d)).await;

    // Typed behind a turn under way, a prompt waits in the TUI's queue, and the inspector says
    // so under the timeline, with no draft and no picture.
    type_in_terminal(drv, HELD_AGAIN).await;
    drv.keys("enter").await.unwrap();
    type_in_terminal(drv, QUEUED).await;
    drv.keys("enter").await.unwrap();
    let queued = format!("Queued: {QUEUED}");
    let shown =
        seen(drv, "the queued prompt", STEP, |d| node(d, "ListItem", &queued).is_some()).await;
    let section = node(&shown, "Group", "Prompt box").expect("the prompt box").bounds;
    let turns: Vec<[f32; 4]> =
        shown.a11y.iter().filter(|n| n.role == "Article").map(|n| n.bounds).collect();
    let [_, y, _, h] = *turns.last().expect("the timeline's turns");
    assert!(section[1] >= y + h - 1.0, "under the timeline: {section:?} after a turn at {y}+{h}");
    let [left, top, width, _] = section;
    let in_box = |n: &&slopty_e2e::A11yNode| {
        n.bounds[0] >= left && n.bounds[0] < left + width && n.bounds[1] >= top
    };
    assert!(!shown.a11y.iter().filter(in_box).any(|n| n.role == "Paragraph"), "queued, no draft");
    assert!(node(&shown, "Button", "Image #1").is_none(), "the sent picture gone from the box");

    // Esc stops the turn, the queued prompt goes to the agent, and the box holds nothing.
    drv.keys("escape").await.unwrap();
    let done = format!("Done: {QUEUED}");
    thread_until(&stack, &agent, "took the queued prompt", |r| says(r, &done)).await;
    seen(&mut stack.driver, "the box empty", STEP, |d| !mirrored(d)).await;

    stack.shutdown().await;
}
