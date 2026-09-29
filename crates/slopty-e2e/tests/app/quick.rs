//! The quick terminal in the real app: the palette's command opens one shell on the worker and
//! shows it in a panel of its own, its tile saying where it went; showing and hiding it again
//! opens no other and keeps the session. Each show is timed by the app from the command to the
//! first frame holding it on the glass, and logged as `quick terminal shown` (MEASUREMENTS,
//! "Quick terminal: chord to glass"). The self-test registers no system-wide chord and its
//! panel takes no keyboard, so the machine's keyboard stays with whoever is using it.
//!
//! The keyboard's side is not run live: a panel that takes the keyboard takes it from the
//! person at this Mac, and the dump carries no panel state to judge it by. The GPUI tests hold
//! it instead (`cargo nextest run -p slopty-ui -E 'test(/workspace::tests::quick/)'`): a show
//! gives the shell the keyboard, losing it hides the panel unless "Hide when unfocused" is
//! off, the chord brings a panel shown behind another window to the front and hides one in
//! front, and the palette's command, run in the workspace's window, hides a shown panel.

use std::time::Duration;

use slopty_e2e::{Driver, Dump, Stack};

use super::gallery::{STEP, first_shell};

/// Shows and hides after the first: enough shows for a median.
const TOGGLES: usize = 16;
/// Longer than the slide in (240 ms) or out (160 ms), so each show starts from hidden.
const SETTLE: Duration = Duration::from_millis(400);

/// "Toggle quick terminal" from the palette.
async fn toggle(drv: &mut Driver) {
    drv.keys("cmd-shift-p").await.unwrap();
    drv.wait_for("the palette", STEP, |d| d.a11y_node("Dialog", Some("Commands")).is_some())
        .await
        .unwrap();
    drv.type_text("toggle quick terminal").await.unwrap();
    drv.wait_for("its line", STEP, |d| {
        d.a11y.iter().any(|n| {
            n.role == "ListBoxOption"
                && n.label.as_deref().is_some_and(|l| l.starts_with("Toggle quick terminal"))
        })
    })
    .await
    .unwrap();
    drv.keys("enter").await.unwrap();
    drv.wait_for("the palette closed", STEP, |d| d.a11y_node("Dialog", Some("Commands")).is_none())
        .await
        .unwrap();
}

/// The sessions of the terminal tiles, sorted.
fn sessions(dump: &Dump) -> Vec<String> {
    let mut out: Vec<String> = dump
        .items
        .iter()
        .filter(|i| i.kind == "terminal")
        .filter_map(|i| i.session.clone())
        .collect();
    out.sort();
    out
}

fn says_where(dump: &Dump) -> bool {
    dump.a11y
        .iter()
        .any(|n| n.label.as_deref().is_some_and(|l| l.contains("In the quick terminal")))
}

#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_quick_terminal_keeps_one_shell_across_shows() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let drv = &mut stack.driver;
    first_shell(drv).await;

    toggle(drv).await;
    let dump = drv
        .wait_for("the quick shell, its tile saying where it went", STEP, |d| {
            sessions(d).len() == 2 && says_where(d)
        })
        .await
        .unwrap();
    let kept = sessions(&dump);
    assert_eq!(dump.focus.as_deref(), Some("terminal"), "the workspace's focus stayed put");

    for _ in 0..TOGGLES {
        tokio::time::sleep(SETTLE).await;
        toggle(drv).await;
    }
    tokio::time::sleep(SETTLE).await;
    let dump = drv.dump().await.unwrap();
    assert_eq!(sessions(&dump), kept, "no second shell, the first kept");
    assert!(says_where(&dump), "{:#?}", dump.a11y);
    stack.shutdown().await;
}
