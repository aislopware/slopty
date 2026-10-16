//! The shell's live surfaces on a real stack: a shell of the stack's worker in the terminal dock
//! (⌘J), drawn by Slopty's own terminal in the dock's chrome, and a window of the worker's Mac as
//! a right-pane surface, chosen from the machine's list, then taking the whole window. Each state
//! is a golden in light and in dark.

use std::path::Path;
use std::time::Duration;

use slopty_e2e::harness::artifacts_dir;
use slopty_e2e::snapshot::{MAC_TOLERANCE as TOLERANCE, assert_matches};
use slopty_e2e::{A11yNode, Button, Command, Driver, Dump, Stack};

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

/// The foot's "N lines below" pill.
fn lines_below(dump: &Dump) -> Option<&A11yNode> {
    dump.a11y.iter().find(|n| {
        n.role == "Button" && n.label.as_deref().is_some_and(|l| l.contains("lines below"))
    })
}

/// `value` is `want` points, give or take half a point.
fn about(value: f32, want: f32) -> bool {
    (value - want).abs() <= 0.5
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

/// What floats over the dock's terminal is the shell's own: a program's progress (`OSC 9;4`) on
/// its tab, the lines-below pill (zeron's 30 pt jump pill), the find field (a 24 pt surface
/// field), the right-click menu (the shell's menu rows, one height) and the dim trailer a program
/// that ended leaves. Each decides on its numbers, and each state is a golden in both appearances.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_terminal_s_floats_wear_the_shell() {
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

    // A program's progress shows on its tab, as a share a screen reader reads, and goes when the
    // program clears it.
    drv.type_text("printf '\\e]9;4;1;40\\a'").await.unwrap();
    drv.keys("enter").await.unwrap();
    let dump = drv
        .wait_for("the tab's progress", STEP, |d| {
            node(d, "ProgressIndicator", "Progress").is_some()
        })
        .await
        .unwrap();
    let progress = node(&dump, "ProgressIndicator", "Progress").unwrap();
    assert_eq!(progress.value.as_deref(), Some("40 %"), "{progress:?}");
    drv.type_text("printf '\\e]9;4;0\\a'").await.unwrap();
    drv.keys("enter").await.unwrap();
    drv.wait_for("the progress cleared", STEP, |d| {
        node(d, "ProgressIndicator", "Progress").is_none() && d.prompts_settled()
    })
    .await
    .unwrap();

    // Scrolled up into its history, the foot counts the lines below on zeron's jump pill.
    drv.type_text("seq 1 300").await.unwrap();
    drv.keys("enter").await.unwrap();
    drv.wait_for("the count", STEP, |d| {
        !d.lines_containing("300").is_empty() && d.prompts_settled()
    })
    .await
    .unwrap();
    let dump = drv.dump().await.unwrap();
    let [x, y, w, h] = dump.a11y.iter().find(|n| n.role == "Terminal").map(|n| n.bounds).unwrap();
    drv.scroll(x + w / 2.0, y + h / 2.0, 0.0, 40.0).await.unwrap();
    let dump =
        drv.wait_for("the lines-below pill", STEP, |d| lines_below(d).is_some()).await.unwrap();
    let pill = lines_below(&dump).unwrap();
    assert!(about(pill.bounds[3], 30.0), "the jump pill is 30 tall: {pill:?}");
    goldens(drv, &renders, "surface-live-terminal-scrolled").await;
    let [px_, py, pw, ph] = pill.bounds;
    drv.click(px_ + pw / 2.0, py + ph / 2.0).await.unwrap();
    drv.wait_for("back at the live end", STEP, |d| lines_below(d).is_none()).await.unwrap();

    // ⌘F opens the find bar: its field is a surface's 24 pt field, its words find their hits.
    drv.keys("cmd-f").await.unwrap();
    let dump = drv
        .wait_for("the find field", STEP, |d| node(d, "SearchInput", "Find").is_some())
        .await
        .unwrap();
    let field = node(&dump, "SearchInput", "Find").unwrap();
    assert!(field.bounds[3] <= 24.5, "the field sits in a 24 pt frame: {field:?}");
    drv.type_text("29").await.unwrap();
    drv.wait_for("the tally", STEP, |d| {
        d.a11y.iter().any(|n| {
            n.label.as_deref() == Some("Matches")
                && n.value.as_deref().is_some_and(|v| !v.is_empty() && !v.starts_with('0'))
        })
    })
    .await
    .unwrap();
    goldens(drv, &renders, "surface-live-terminal-find").await;
    drv.keys("escape").await.unwrap();
    drv.wait_for("the find bar closed", STEP, |d| node(d, "SearchInput", "Find").is_none())
        .await
        .unwrap();

    // A right click opens the shell's menu: its rows are the shell's menu rows, one height.
    drv.ok(&Command::Click { x: x + w / 2.0, y: y + h / 2.0, button: Button::Right, count: 1 })
        .await
        .unwrap();
    let dump =
        drv.wait_for("the menu", STEP, |d| d.a11y.iter().any(|n| n.role == "Menu")).await.unwrap();
    let rows: Vec<&A11yNode> = dump.a11y.iter().filter(|n| n.role == "MenuItem").collect();
    assert!(rows.len() >= 3, "{rows:?}");
    let height = rows[0].bounds[3];
    assert!((26.0..=32.0).contains(&height), "a menu row of the shell's: {height}");
    assert!(rows.iter().all(|r| about(r.bounds[3], height)), "one row height: {rows:?}");
    goldens(drv, &renders, "surface-live-terminal-menu").await;
    drv.keys("escape").await.unwrap();
    drv.wait_for("the menu closed", STEP, |d| !d.a11y.iter().any(|n| n.role == "Menu"))
        .await
        .unwrap();

    // A shell that exits leaves zeron's dim trailer with its status.
    drv.type_text("exit 3").await.unwrap();
    drv.keys("enter").await.unwrap();
    drv.wait_for("the exit trailer", STEP, |d| {
        d.a11y.iter().any(|n| n.label.as_deref() == Some("[process exited 3]"))
    })
    .await
    .unwrap();
    goldens(drv, &renders, "surface-live-terminal-exited").await;
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
