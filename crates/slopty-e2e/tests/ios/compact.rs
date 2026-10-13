//! The phone's anatomy on the simulator (`cargo xtask e2e ios --sim iphone|ipad`): a phone
//! opens on its Sessions page (the large title, the session list, the "New session" capsule and
//! the tab bar), "New session" takes the whole screen for the new chat, and the chat's sidebar
//! button goes back. An iPad keeps the shell's columns, the sidebar beside the chat. Each state is
//! a golden in light and in dark, `ios-<device>-compact-*`.

use std::path::Path;
use std::time::Duration;

use slopty_e2e::harness::{Simulator, Stack, artifacts_dir};
use slopty_e2e::snapshot::assert_matches;
use slopty_e2e::{Command, Driver, Dump};

/// Per-step wait.
const STEP: Duration = Duration::from_secs(30);
/// A viewport narrower than this is a phone's (the shell's `PHONE_BELOW`).
const PHONE_BELOW: f32 = 700.0;
/// As the other simulator goldens: five times the noise between two runs.
const TOLERANCE: f64 = 0.0005;
/// Long enough for a page's entrance to come to rest.
const SETTLE: Duration = Duration::from_millis(600);

/// The booted simulator `cargo xtask e2e ios` installed the app on.
fn simulator() -> Simulator {
    let udid = std::env::var("SLOPTY_SIM_UDID").expect("SLOPTY_SIM_UDID (a booted simulator)");
    let bundle_id = std::env::var("SLOPTY_SIM_BUNDLE_ID").expect("SLOPTY_SIM_BUNDLE_ID");
    Simulator { udid, bundle_id }
}

/// Tap the middle of the `role` node labelled `label`, once it is drawn.
async fn tap(drv: &mut Driver, role: &str, label: &str) {
    let what = format!("the {role} {label}");
    let d = drv.wait_for(&what, STEP, |d| d.a11y_node(role, Some(label)).is_some()).await.unwrap();
    let [left, top, width, height] = d.a11y_node(role, Some(label)).map(|n| n.bounds).unwrap();
    drv.ui_tap(left + width / 2.0, top + height / 2.0).await.unwrap();
}

/// Render the frame in light and in dark against `golden/ios-<device>-compact-<state>-*.png`;
/// light again after.
async fn goldens(drv: &mut Driver, dir: &Path, device: &str, state: &str) {
    for (dark, appearance) in [(false, "light"), (true, "dark")] {
        drv.ok(&Command::Appearance { dark }).await.unwrap();
        tokio::time::sleep(SETTLE).await;
        let name = format!("ios-{device}-compact-{state}-{appearance}");
        let frame = drv.render(&dir.join(format!("{name}.png"))).await.unwrap();
        println!("A11Y {name}\n{}", slopty_e2e::frame_text(&frame.a11y));
        assert_matches(&name, &frame, TOLERANCE, &artifacts_dir()).unwrap();
    }
    drv.ok(&Command::Appearance { dark: false }).await.unwrap();
}

/// The Sessions page is up.
fn sessions_page(d: &Dump) -> bool {
    d.a11y_node("Group", Some("Sessions")).is_some()
        && d.a11y_node("TabList", Some("Tabs")).is_some()
}

#[tokio::test]
#[ignore = "live: cargo xtask e2e ios"]
async fn a_phone_opens_on_its_sessions_and_a_chat_takes_the_screen() {
    let mut stack = Stack::launch_on_simulator("e2e-ios-worker", simulator()).await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let drv = &mut stack.driver;
    let dump = drv.wait_for("the worker", STEP, |d| d.status == "connected").await.unwrap();
    let phone = dump.window.width < PHONE_BELOW;
    let device = if phone { "phone" } else { "pad" };

    if !phone {
        // An iPad keeps the columns: no Sessions page, the sidebar's list beside the chat.
        let dump = drv.dump().await.unwrap();
        assert!(!sessions_page(&dump), "{:#?}", dump.a11y);
        goldens(drv, &dir, device, "columns").await;
        stack.shutdown().await;
        return;
    }

    let dump = drv.wait_for("the Sessions page", STEP, sessions_page).await.unwrap();
    // The page keeps clear of the status bar and the home indicator.
    let tabs = dump.a11y_node("TabList", Some("Tabs")).map(|n| n.bounds).unwrap();
    assert!(tabs[1] + tabs[3] <= dump.window.height + 0.5, "{tabs:?} in {:?}", dump.window);
    for tab in ["Sessions", "Settings", "Search"] {
        assert!(dump.a11y_node("Tab", Some(tab)).is_some(), "{tab}: {:#?}", dump.a11y);
    }
    goldens(drv, &dir, device, "sessions").await;

    // "New session" takes the screen for the new chat, its composer ready.
    tap(drv, "Button", "New session").await;
    drv.wait_for("the new chat", STEP, |d| !sessions_page(d)).await.unwrap();
    goldens(drv, &dir, device, "new-session").await;

    // The chat's sidebar button goes back to Sessions.
    tap(drv, "Button", "Toggle left sidebar").await;
    drv.wait_for("the Sessions page again", STEP, sessions_page).await.unwrap();

    // Settings from the tab bar, and back from its page to Sessions.
    tap(drv, "Tab", "Settings").await;
    drv.wait_for("the settings", STEP, |d| d.a11y_node("Group", Some("Settings")).is_some())
        .await
        .unwrap();
    tap(drv, "Button", "Back").await;
    drv.wait_for("Sessions after the settings", STEP, sessions_page).await.unwrap();
    stack.shutdown().await;
}
