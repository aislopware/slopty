//! A session's turn holds still where the person looks in its inspector, on a real stack: a
//! stand-in Claude Code (`slopty-stub-claude`) answers in its terminal, and the inspector beside
//! it reads the answer from the transcript. A code block's Copy turns to Copied in place;
//! terminal output in an ```` ```ansi ```` fence draws in its colours, no escape shown; a link
//! shows where it goes on hover. Each is judged by numbers from the accessibility tree: where the
//! Copy button sits. (What a copy holds is the shell's unit tests': GPUI copies to the general
//! pasteboard, which no test reads.) Stop is the TUI's (Esc), so the inspector has none.

#![cfg(target_os = "macos")]

use slopty_e2e::harness::ServedApp;
use slopty_e2e::{A11yNode, Driver, Dump};

use crate::served::{
    SETTLE, STEP, goldens, hover, middle, node, open_thread, says, seen, start, thread_until,
};

/// The worker's name, as the CLI names the machine.
const MACHINE: &str = "conversation";

/// A stack with the stand-in as it comes.
async fn stack() -> ServedApp {
    ServedApp::launch(MACHINE).await.unwrap()
}

/// Open the one session once the app holds its finished turn, and wait until it reads as seen:
/// its row says nothing more, whether or not the guest's window is the active one, so a golden
/// of it never catches the row before or after the look.
async fn open_read(drv: &mut Driver) {
    seen(drv, "the finished session's row", STEP, |d| node(d, "Status", "Done").is_some()).await;
    open_thread(drv, 1).await;
    seen(drv, "the session read", STEP, |d| node(d, "Status", "Done").is_none()).await;
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
    open_read(drv).await;
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
    open_read(drv).await;
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
