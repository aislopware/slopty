//! A long sidebar group on a real stack: twelve chats the stand-in Claude Code finished leave the
//! Done group with its first ten rows and "Show N more", which shows the rest when pressed
//! (`docs/decisions/ui.md`, "The sidebar groups by status", Show more). Judged by the
//! accessibility tree.

use slopty_e2e::Dump;
use slopty_e2e::harness::ServedApp;

use crate::served::{STEP, TURN, middle, seen, start, thread_until};

/// The worker's name, as the CLI names the machine.
const MACHINE: &str = "paging";

/// Chats started: more than a group's first page of ten, whichever of them is open.
const CHATS: usize = 12;

/// The most that wait behind Show more: twelve less the first page, the open chat among them.
const BEHIND: usize = 2;

/// The sidebar's "Show N more" line, and its N.
fn show_more(dump: &Dump) -> Option<(&slopty_e2e::A11yNode, usize)> {
    dump.a11y.iter().find_map(|n| {
        let label = n.label.as_deref()?;
        let count = label.strip_prefix("Show ")?.strip_suffix(" more")?.parse().ok()?;
        (n.role == "Button").then_some((n, count))
    })
}

#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_long_group_shows_more_on_request() {
    let mut stack = ServedApp::launch(MACHINE).await.unwrap();
    let repo = std::fs::canonicalize(stack.path("repo")).unwrap();
    for n in 0..CHATS {
        let agent = start(&stack, MACHINE, &repo, &format!("Say hello {n}")).await;
        thread_until(&stack, &agent, "answered", |read| read.to_string().contains("Done:")).await;
    }
    let drv = &mut stack.driver;
    drv.ok(&slopty_e2e::Command::Resize { width: 1320.0, height: 1100.0 }).await.unwrap();

    let paged =
        seen(drv, "Done's first page and Show more", TURN, |d| show_more(d).is_some()).await;
    let (line, hidden) = show_more(&paged).unwrap();
    assert!((1..=BEHIND).contains(&hidden), "the rows past ten at rest wait: {hidden}");
    let (x, y) = middle(line);
    drv.click(x, y).await.unwrap();
    seen(drv, "every row, and no Show more", STEP, |d| show_more(d).is_none()).await;
}
