//! The shell's live surfaces on a real stack: a shell of the stack's worker in the terminal dock
//! (⌘J), drawn by Slopty's own terminal in the dock's chrome, and a window of the worker's Mac as
//! a right-pane surface, chosen from the machine's list, then taking the whole window. Each state
//! is a golden in light and in dark.

use std::path::Path;
use std::time::Duration;

use slopty_e2e::harness::artifacts_dir;
use slopty_e2e::snapshot::{MAC_TOLERANCE as TOLERANCE, assert_matches};
use slopty_e2e::{A11yNode, Command, Driver, Dump, Stack};

/// How long a worker round trip may take.
const STEP: Duration = Duration::from_secs(30);
/// The shell's main window as it first opens.
const WINDOW: (f32, f32) = (1320.0, 880.0);

/// Render the window in light and in dark and hold each against `golden/<name>-<appearance>.png`;
/// the window is light again after.
async fn goldens(drv: &mut Driver, renders: &Path, name: &str) {
    for (dark, appearance) in [(false, "light"), (true, "dark")] {
        drv.ok(&Command::Appearance { dark }).await.unwrap();
        drv.wait_for("the carets at their prompts", STEP, Dump::prompts_settled).await.unwrap();
        let golden = format!("{name}-{appearance}");
        let frame = drv.render(&renders.join(format!("{golden}.png"))).await.unwrap();
        println!("A11Y {golden}\n{}", slopty_e2e::frame_text(&frame.a11y));
        assert_matches(&golden, &frame, TOLERANCE, &artifacts_dir()).unwrap();
    }
    drv.ok(&Command::Appearance { dark: false }).await.unwrap();
}

/// The node `role` named `label` in `dump`'s accessibility tree.
fn node<'a>(dump: &'a Dump, role: &str, label: &str) -> Option<&'a A11yNode> {
    dump.a11y.iter().find(|n| n.role == role && n.label.as_deref() == Some(label))
}

/// Click the middle of the button named `label`, once it is drawn.
async fn click_button(drv: &mut Driver, label: &str) {
    let what = format!("the button {label}");
    let dump = drv.wait_for(&what, STEP, |d| node(d, "Button", label).is_some()).await.unwrap();
    let [x, y, w, h] = node(&dump, "Button", label).map(|n| n.bounds).unwrap_or_default();
    drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
}

/// ⌘J opens the dock on the new-session canvas with a shell of the stack's worker in it. It is
/// Slopty's terminal of a worker session (the workspace lists it, with its rows), it takes the
/// keyboard, and what is typed runs there.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_terminal_dock_holds_a_shell_of_the_worker() {
    let mut stack = Stack::launch_at_home("e2e-worker").await.unwrap();
    let renders = stack.path("");
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    drv.wait_for("the worker", STEP, |d| d.status == "connected").await.unwrap();

    drv.keys("cmd-j").await.unwrap();
    drv.wait_for("a shell at its prompt", STEP, |d| {
        d.terminals.iter().any(|t| t.at_prompt) && d.prompts_settled()
    })
    .await
    .unwrap();

    drv.type_text("echo live surface").await.unwrap();
    drv.keys("enter").await.unwrap();
    drv.wait_for("the echo", STEP, |d| {
        d.lines_containing("live surface").len() >= 2 && d.prompts_settled()
    })
    .await
    .unwrap();

    goldens(drv, &renders, "surface-live-terminal").await;
    stack.shutdown().await;
}

/// The cases that capture a window, which needs the Screen Recording grant
/// (`cargo xtask e2e app --screen-recording`).
mod screen_recording {
    use super::*;

    /// The right pane's picker offers a remote window; its list holds the window the test opened
    /// on the worker's Mac, and choosing it shows its stream as a surface, which the pane's
    /// expand button then makes the whole window.
    #[tokio::test]
    #[ignore = "live: cargo xtask e2e app --screen-recording"]
    async fn a_remote_window_is_a_right_pane_surface_and_takes_the_window() {
        let mut stack = Stack::launch_at_home("e2e-worker").await.unwrap();
        let idle = stack.start_idle_window().await.unwrap();
        let title = idle.title().to_owned();
        let renders = stack.path("");
        let drv = &mut stack.driver;
        drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        drv.wait_for("the worker", STEP, |d| d.status == "connected").await.unwrap();

        drv.keys("cmd-r").await.unwrap();
        click_button(drv, "Remote window").await;
        click_button(drv, &title).await;
        drv.wait_for("the window's stream", STEP, |d| !d.screens.is_empty()).await.unwrap();
        // The picture's first frame, so the surface shows it rather than its wait.
        tokio::time::sleep(Duration::from_secs(1)).await;
        goldens(drv, &renders, "surface-live-screen").await;

        click_button(drv, "Expand panel").await;
        drv.wait_for("the takeover", STEP, |d| node(d, "Button", "Collapse panel").is_some())
            .await
            .unwrap();
        goldens(drv, &renders, "surface-live-screen-takeover").await;

        idle.hide().unwrap();
        stack.shutdown().await;
    }
}
