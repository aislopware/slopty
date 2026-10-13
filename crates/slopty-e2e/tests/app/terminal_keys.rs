//! A shell's own keys in the terminal dock (⌘J): the terminal's commands are bound in the app's
//! keymap, inside the shell's, and in the terminal they hold over the shell's own chord for the
//! same keys: ⌘K clears the screen there rather than opening the command palette.

#![cfg(target_os = "macos")]

use std::time::Duration;

use slopty_e2e::Stack;

/// How long a worker round trip may take.
const STEP: Duration = Duration::from_secs(30);

/// ⌘K in the dock's shell clears what it shows, the shell at its prompt again, and opens no
/// palette.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_terminal_s_own_keys_hold_in_the_dock() {
    let mut stack = Stack::launch_at_home("e2e-keys").await.unwrap();
    let drv = &mut stack.driver;
    drv.wait_for("the worker", STEP, |d| d.status == "connected").await.unwrap();
    drv.keys("cmd-j").await.unwrap();
    drv.wait_for("the dock's shell with the keyboard", STEP, |d| {
        d.terminals.iter().any(|t| t.at_prompt) && d.focused.starts_with("terminal:")
    })
    .await
    .unwrap();
    drv.type_text("echo key-mark-$((6*7))\n").await.unwrap();
    drv.wait_for("the shell's answer", STEP, |d| !d.lines_containing("key-mark-42").is_empty())
        .await
        .unwrap();
    drv.keys("cmd-k").await.unwrap();
    drv.wait_for("the screen cleared, the keyboard still the shell's", STEP, |d| {
        d.lines_containing("key-mark-42").is_empty() && d.focused.starts_with("terminal:")
    })
    .await
    .unwrap();
    stack.shutdown().await;
}
