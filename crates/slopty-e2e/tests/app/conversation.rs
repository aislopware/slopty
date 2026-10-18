//! The conversation holds still where the person looks, on a real stack: a stand-in Claude Code
//! (`slopty-stub-claude`) answers with long words, then runs a foreground command (`wait`) that
//! Stop ends, and another that ends on its own; a code block's Copy turns to Copied in place;
//! terminal output in an ```` ```ansi ```` fence draws in its colours, no escape shown. Each is
//! judged by numbers from the accessibility tree: where the person's own message sits before and
//! after, where the Copy button sits. (What a copy holds is the shell's unit tests': GPUI copies
//! to the general pasteboard, which no test reads.)

#![cfg(target_os = "macos")]

use slopty_e2e::harness::ServedApp;
use slopty_e2e::{A11yNode, Driver, Dump};

use crate::served::{
    SETTLE, STEP, goldens, hover, middle, node, open_thread, says, seen, start, tell, thread_until,
};

/// The worker's name, as the CLI names the machine.
const MACHINE: &str = "conversation";

/// A stack with the stand-in as it comes.
async fn stack() -> ServedApp {
    ServedApp::launch(MACHINE).await.unwrap()
}

/// The person's own message `words`, as the transcript names it.
fn said<'a>(dump: &'a Dump, words: &str) -> Option<&'a A11yNode> {
    node(dump, "Paragraph", words)
}

/// Where the person's message `words` sits now, once it is drawn and the frame settled.
async fn y_of(drv: &mut Driver, words: &str) -> f32 {
    tokio::time::sleep(SETTLE).await;
    let dump =
        seen(drv, &format!("the message {words:?}"), STEP, |d| said(d, words).is_some()).await;
    said(&dump, words).map(|n| n.bounds[1]).unwrap_or_default()
}

/// A reply long enough to pin the transcript to its bottom: what sits above the last row moves
/// whenever the last row's height does.
fn long_reply() -> String {
    use std::fmt::Write as _;
    let mut reply = "Notes:\n\n".to_owned();
    for n in 1..=24 {
        write!(reply, "Paragraph {n}: the reply runs on so the chat scrolls.\n\n").unwrap();
    }
    reply
}

/// Stop and a turn's end move nothing in the chat: with the transcript pinned to its bottom, the
/// person's message above a running command sits at the same y, within a pixel, while the
/// command runs, once Stop ended it, and across a turn that ends on its own. The person's own
/// message is in the accessibility tree, a group labelled with its words.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn stop_and_a_turns_end_leave_the_chat_still() {
    let mut stack = stack().await;
    let repo = stack.path("repo");
    let agent = start(&stack, MACHINE, &repo, &long_reply()).await;
    thread_until(&stack, &agent, "answered", |r| says(r, "Done: Notes:")).await;

    // Stop: the command runs, then Stop ends it.
    tell(&mut stack, "wait 60").await;
    let drv = &mut stack.driver;
    seen(drv, "the Stop button", STEP, |d| node(d, "Button", "Stop").is_some()).await;
    let running = y_of(drv, "wait 60").await;
    let dump = drv.dump().await.unwrap();
    let stop = node(&dump, "Button", "Stop").cloned().unwrap();
    let (x, y) = middle(&stop);
    drv.click(x, y).await.unwrap();
    seen(drv, "the turn stopped", STEP, |d| node(d, "Button", "Stop").is_none()).await;
    let stopped = y_of(drv, "wait 60").await;
    println!("MEASURE stop-no-shift: the message at {running:.1} running, {stopped:.1} stopped");
    assert!((running - stopped).abs() <= 1.0, "Stop moved the chat: {running} → {stopped}");
    // The pointer off the window: Send takes Stop's place under it, and its tooltip, shown once
    // the pointer rests long enough, is no part of the golden.
    let drv = &mut stack.driver;
    drv.ok(&slopty_e2e::Command::Move { x: -1.0, y: -1.0 }).await.unwrap();
    seen(drv, "no tooltip", STEP, |d| d.a11y.iter().all(|n| n.role != "Tooltip")).await;
    let renders = stack.path("");
    goldens(&mut stack.driver, &renders, "composer-stop-no-shift", &[]).await;

    // A turn that ends on its own.
    tell(&mut stack, "wait 3").await;
    let drv = &mut stack.driver;
    seen(drv, "the Stop button", STEP, |d| node(d, "Button", "Stop").is_some()).await;
    let working = y_of(drv, "wait 3").await;
    thread_until(&stack, &agent, "ended the wait", |r| says(r, "Done: waited.")).await;
    let drv = &mut stack.driver;
    seen(drv, "the turn ended", STEP, |d| node(d, "Button", "Stop").is_none()).await;
    let ended = y_of(drv, "wait 3").await;
    println!("MEASURE end-no-shift: the message at {working:.1} working, {ended:.1} ended");
    assert!((working - ended).abs() <= 1.0, "a turn's end moved the chat: {working} → {ended}");
    stack.shutdown().await;
}

/// A code block's Copy says Copied in place, the button and the block unmoved; terminal output
/// in an ```` ```ansi ```` fence draws in its colours, no escape shown.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn code_copies_in_place_and_ansi_draws_in_colour() {
    let reply = "Output:\n\n```ansi\n\\x1b[1;32m✓\\x1b[0m 12 passed \\x1b[2m(0.4s)\\x1b[0m\n\
                 \\x1b[31merror\\x1b[0m: \\x1b[1mcannot find\\x1b[0m `x` in this scope\n\
                 \\x1b[38;5;208mwarning\\x1b[0m: unused import\n```";
    let mut stack = stack().await;
    let repo = stack.path("repo");
    let agent = start(&stack, MACHINE, &repo, reply).await;
    thread_until(&stack, &agent, "answered", |r| says(r, "Done: Output:")).await;

    let renders = stack.path("");
    let drv = &mut stack.driver;
    open_thread(drv, 1).await;
    tokio::time::sleep(SETTLE).await;
    let shown =
        seen(drv, "the code's Copy", STEP, |d| node(d, "Button", "Copy code").is_some()).await;
    // The person's own words keep what they typed; the reply's block shows no escape.
    let words = slopty_e2e::frame_text(crate::served::replies(&shown));
    assert!(!words.contains("\\x1b") && !words.contains("[1;32m"), "no escape shows:\n{words}");
    goldens(drv, &renders, "code-ansi", &[]).await;

    let dump = drv.dump().await.unwrap();
    let copy = node(&dump, "Button", "Copy code").cloned().unwrap();
    hover(drv, &copy).await;
    let (x, y) = middle(&copy);
    drv.click(x, y).await.unwrap();
    let copied = seen(drv, "Copied", STEP, |d| node(d, "Button", "Copied").is_some()).await;
    let after = node(&copied, "Button", "Copied").cloned().unwrap();
    println!("MEASURE code-copy-no-shift: Copy at {:?}, Copied at {:?}", copy.bounds, after.bounds);
    let moved =
        copy.bounds.iter().zip(after.bounds).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
    assert!(
        moved <= 1.0,
        "Copied moved or resized the button: {:?} → {:?}",
        copy.bounds,
        after.bounds
    );
    // Back to Copy once it has said so, still in place.
    let back = seen(drv, "Copy again", STEP, |d| node(d, "Button", "Copy code").is_some()).await;
    let again = node(&back, "Button", "Copy code").cloned().unwrap();
    assert!(
        copy.bounds.iter().zip(again.bounds).all(|(a, b)| (a - b).abs() <= 1.0),
        "Copy came back elsewhere: {:?} → {:?}",
        copy.bounds,
        again.bounds
    );
    // The block as it rests once Copy is back, the pointer gone from it: a pointer still on the
    // button shows its tooltip and the reply's hover line only until an appearance's change
    // draws the window again, so the light render and the dark one would hold different moments.
    drv.ok(&slopty_e2e::Command::Move { x: 1300.0, y: 60.0 }).await.unwrap();
    seen(drv, "the tooltip gone", STEP, |d| d.a11y.iter().all(|n| n.role != "Tooltip")).await;
    goldens(drv, &renders, "code-copy-no-shift", &[]).await;
    stack.shutdown().await;
}

/// A link in a reply shows where it goes when the pointer rests on it: the destination card,
/// the address in full as local text (nothing is fetched), under the link, in light and dark.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_link_shows_where_it_goes_on_hover() {
    const URL: &str = "https://example.com/docs/guide/getting-started";
    let reply = format!("Read [the guide]({URL}) before the first run.");
    let mut stack = stack().await;
    let repo = stack.path("repo");
    let agent = start(&stack, MACHINE, &repo, &reply).await;
    thread_until(&stack, &agent, "answered", |r| says(r, "Done: Read")).await;

    let renders = stack.path("");
    let drv = &mut stack.driver;
    open_thread(drv, 1).await;
    tokio::time::sleep(SETTLE).await;
    let shown = seen(drv, "the link", STEP, |d| link(d).is_some()).await;
    let at = link(&shown).cloned().unwrap();
    hover(drv, &at).await;
    // The card shows after the tooltip's delay and its fade. The pointer on the reply shows its
    // hover line, whose stamp is the run's moment: that line is left out.
    tokio::time::sleep(SETTLE).await;
    crate::served::goldens_with(drv, &renders, "link-preview", &[], &["Copy message"]).await;
    stack.shutdown().await;
}

/// The reply's link "the guide", the last one drawn (the person's own message holds it too).
fn link(dump: &Dump) -> Option<&A11yNode> {
    dump.a11y
        .iter()
        .rev()
        .find(|n| n.role == "Link" && n.label.as_deref().is_some_and(|l| l.contains("the guide")))
}
