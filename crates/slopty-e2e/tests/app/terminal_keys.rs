//! A shell's own keys in a shell split beside the session (⌘D): the terminal's commands are bound
//! in the app's keymap, inside the shell's. Its clear is ⌥⌘K, which holds there; ⌘K is the app's in
//! every session, a plain shell's too (`docs/decisions/layout.md`, "The picker and the palette are
//! one keyboard door to everything"), and leaves what the shell shows alone.

#![cfg(target_os = "macos")]

use std::time::Duration;

use slopty_e2e::Stack;

/// How long a worker round trip may take.
const STEP: Duration = Duration::from_secs(30);

/// ⌘K over the split's shell takes the keyboard to the app's door and clears nothing; Esc gives
/// it back. ⌥⌘K clears what the shell shows, the shell at its prompt again with the keyboard.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_terminal_s_clear_is_option_command_k() {
    let mut stack = Stack::launch_at_home("e2e-keys").await.unwrap();
    let drv = &mut stack.driver;
    drv.wait_for("the worker", STEP, |d| d.status == "connected").await.unwrap();
    drv.keys("cmd-d").await.unwrap();
    drv.wait_for("the split's shell with the keyboard", STEP, |d| {
        d.terminals.iter().any(|t| t.at_prompt) && d.focused.starts_with("terminal:")
    })
    .await
    .unwrap();
    drv.type_text("echo key-mark-$((6*7))\n").await.unwrap();
    drv.wait_for("the shell's answer", STEP, |d| !d.lines_containing("key-mark-42").is_empty())
        .await
        .unwrap();
    drv.keys("cmd-k").await.unwrap();
    let away = drv
        .wait_for("the keyboard at the app's door", STEP, |d| !d.focused.starts_with("terminal:"))
        .await
        .unwrap();
    assert!(!away.lines_containing("key-mark-42").is_empty(), "⌘K cleared the shell");
    drv.keys("escape").await.unwrap();
    drv.wait_for("the keyboard back in the shell", STEP, |d| d.focused.starts_with("terminal:"))
        .await
        .unwrap();
    drv.keys("alt-cmd-k").await.unwrap();
    drv.wait_for("the screen cleared, the keyboard still the shell's", STEP, |d| {
        d.lines_containing("key-mark-42").is_empty() && d.focused.starts_with("terminal:")
    })
    .await
    .unwrap();
    stack.shutdown().await;
}
