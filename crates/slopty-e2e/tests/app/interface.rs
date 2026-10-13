//! The shell on a real stack: the app's one window, with the stack's worker as a
//! device and its threads as chats, served from the workspace (`slopty_ui::view_model`).

use slopty_e2e::harness::artifacts_dir;
use slopty_e2e::snapshot::{MAC_TOLERANCE as TOLERANCE, assert_matches, foreground_fraction};
use slopty_e2e::{Command, Stack};

/// The shell's main window as it first opens.
const WINDOW: (f32, f32) = (1320.0, 880.0);

/// The app comes up as the shell: its sidebar, its new-chat canvas and its composer,
/// drawn and named for a screen reader.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_interface_comes_up_on_the_stack() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let path = stack.path("interface.png");
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let frame = drv.render(&path).await.unwrap();
    println!("A11Y {}", slopty_e2e::frame_text(&frame.a11y));
    let fg = foreground_fraction(&frame);
    assert!(fg > 0.002, "frame is blank ({fg:.4} foreground)");
    assert_matches("interface", &frame, TOLERANCE, &artifacts_dir()).unwrap();
}
