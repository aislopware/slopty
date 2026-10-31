//! An agent's GPU visual in a session's inspector on the simulator (a phone's sheet taken to the
//! whole screen, an iPad's pane): the shaders draw on the surface path there as on the Mac, each
//! frame an `IOSurface`-backed BGRA buffer from a CoreVideo pool that wgpu renders into through
//! the Metal texture cache and the window paints as a surface.

use std::time::Duration;

use serde_json::json;
use slopty_e2e::harness::{Stack, measure, slopty_json};
use slopty_e2e::visual;

use crate::compact::{PHONE_BELOW, STEP, simulator, tap};

/// Two shaders, one animating and one still ([`visual::SCRIPT`]), painted as two BGRA surfaces,
/// each an `IOSurface` its shader's last pass drew into, held to the rules the Mac's are
/// ([`visual::hold_to_its_rules`]): at time 0 under the self-test's held Reduce Motion the render
/// holds the script's colours where it put them; the animating one draws while motion runs and
/// stands once held; the still one keeps its one frame.
#[tokio::test]
#[ignore = "live: cargo xtask e2e ios"]
async fn an_animated_shader_draws_on_the_surface_path_in_the_simulator() {
    let prompt = "Show the shaders";
    let turns = json!({ prompt: [{ "name": "show_visual", "arguments": {
        "title": visual::TITLE, "script": visual::SCRIPT,
    }}]})
    .to_string();
    let mut stack = Stack::launch_on_simulator_with_stand_in(
        "e2e-ios-visuals",
        simulator(),
        &[("STUB_TURNS", &turns)],
    )
    .await
    .unwrap();
    let drv = &mut stack.driver;
    let dump = drv.wait_for("the worker", STEP, |d| d.status == "connected").await.unwrap();
    let phone = dump.window.width < PHONE_BELOW;
    let device = if phone { "phone" } else { "pad" };
    let (address, cli) = (stack.server.address().to_owned(), stack.path("cli"));
    let folder = stack.path("repo");
    std::fs::create_dir_all(&folder).unwrap();
    let folder = folder.to_string_lossy().into_owned();
    let start = ["agent", "start", "e2e-ios-visuals", "--folder", &folder, prompt];
    let started = slopty_json(&address, &cli, &start, b"").await.unwrap();
    let agent = started["agent"].as_str().expect("an agent started").to_owned();
    let since = std::time::Instant::now();
    loop {
        // A read may meet the worker between two links: read again, until the deadline.
        let args = ["agent", "read", "--thread", agent.as_str()];
        let read = match slopty_json(&address, &cli, &args, b"").await {
            Ok(read) if read.to_string().contains("Done: Show the shaders") => break,
            Ok(read) => read.to_string(),
            Err(e) => format!("{e:#}"),
        };
        assert!(since.elapsed() < STEP * 3, "the thread never answered: {read}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Open the chat (from Chats on a phone, from the sidebar's list on an iPad), then its
    // inspector: a sheet taken to the whole screen on a phone, a pane ⌘I summons on an iPad.
    let drv = &mut stack.driver;
    let listed = drv
        .wait_for("the chat listed", STEP, |d| {
            d.a11y
                .iter()
                .any(|n| n.label.as_deref() == Some(prompt) || n.value.as_deref() == Some(prompt))
        })
        .await
        .unwrap();
    let row = listed
        .a11y
        .iter()
        .find(|n| n.label.as_deref() == Some(prompt) || n.value.as_deref() == Some(prompt))
        .map(|n| n.bounds)
        .unwrap();
    drv.ui_tap(row[0] + row[2] / 2.0, row[1] + row[3] / 2.0).await.unwrap();
    drv.wait_for("the session's terminal", STEP, |d| d.a11y.iter().any(|n| n.role == "Terminal"))
        .await
        .unwrap();
    if phone {
        tap(drv, "Button", prompt).await;
        tap(drv, "Button", "Expand inspector").await;
    } else {
        drv.keys("cmd-i").await.unwrap();
    }
    let shown = visual::settled(drv, STEP).await.unwrap_or_else(|e| panic!("{e:#}"));
    measure(&format!("ios-shader-first-frames {device}: first painted [{}]", shown.first));
    let dir = stack.dir.path().to_path_buf();
    let drv = &mut stack.driver;
    let name = format!("ios-{device}-shader");
    let held = visual::hold_to_its_rules(drv, &shown, &dir, &name, STEP)
        .await
        .unwrap_or_else(|e| panic!("{e:#}"));
    measure(&format!("ios-shader-moved {device}: {held}"));
    stack.shutdown().await;
}
