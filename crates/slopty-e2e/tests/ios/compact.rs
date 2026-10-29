//! The phone's anatomy on the simulator (`cargo xtask e2e ios --sim iphone|ipad`): a phone
//! opens on its Chats page (the large title, the chat list, the capsule and the tab bar, with no
//! sidebar foot). Before any project the capsule adds one; after it, "New session" takes the whole
//! screen for the new chat under a navigation bar (its title and a close button, none of the
//! desktop's sidebar and history cluster), and its close goes back. An iPad keeps the shell's
//! columns, the sidebar beside the chat. Each state is a golden in light and in dark,
//! `ios-<device>-compact-*`.

use std::path::Path;
use std::time::Duration;

use slopty_e2e::harness::{Simulator, Stack, artifacts_dir};
use slopty_e2e::snapshot::{PixelRect, assert_matches_masked};
use slopty_e2e::{Command, Driver, Dump};

/// Per-step wait.
pub const STEP: Duration = Duration::from_secs(30);
/// A viewport narrower than this is a phone's (the shell's `PHONE_BELOW`).
pub const PHONE_BELOW: f32 = 700.0;
/// As the other simulator goldens: five times the noise between two runs.
const TOLERANCE: f64 = 0.0005;
/// Long enough for a page's entrance to come to rest.
pub const SETTLE: Duration = Duration::from_millis(600);

/// The booted simulator `cargo xtask e2e ios` installed the app on.
pub fn simulator() -> Simulator {
    let udid = std::env::var("SLOPTY_SIM_UDID").expect("SLOPTY_SIM_UDID (a booted simulator)");
    let bundle_id = std::env::var("SLOPTY_SIM_BUNDLE_ID").expect("SLOPTY_SIM_BUNDLE_ID");
    Simulator { udid, bundle_id }
}

/// Tap the middle of the `role` node labelled `label`, once it is drawn.
pub async fn tap(drv: &mut Driver, role: &str, label: &str) {
    let what = format!("the {role} {label}");
    let d = drv.wait_for(&what, STEP, |d| d.a11y_node(role, Some(label)).is_some()).await.unwrap();
    let [left, top, width, height] = d.a11y_node(role, Some(label)).map(|n| n.bounds).unwrap();
    drv.ui_tap(left + width / 2.0, top + height / 2.0).await.unwrap();
}

/// Apple's least touch target, points a side.
const TOUCH_TARGET: f32 = 44.0;

/// Say which buttons `state` draws smaller than a finger's [`TOUCH_TARGET`], by their hit
/// boxes as the accessibility tree has them: the numbers the touch-target audit reads.
async fn touch_targets(drv: &mut Driver, state: &str) {
    let dump = drv.dump().await.unwrap();
    let buttons: Vec<_> = dump.a11y.iter().filter(|n| n.role == "Button").collect();
    let small: Vec<String> = buttons
        .iter()
        .filter(|n| n.bounds[2].min(n.bounds[3]) < TOUCH_TARGET)
        .map(|n| {
            let [_, _, w, h] = n.bounds;
            format!("{} {w:.0}x{h:.0}", n.label.as_deref().unwrap_or("(unnamed)"))
        })
        .collect();
    println!(
        "MEASURE touch-targets {state}: {} of {} buttons under {TOUCH_TARGET} pt: {}",
        small.len(),
        buttons.len(),
        small.join(", ")
    );
}

/// Render the frame in light and in dark against `golden/ios-<device>-compact-<state>-*.png`;
/// light again after.
pub async fn goldens(drv: &mut Driver, dir: &Path, device: &str, state: &str) {
    for (dark, appearance) in [(false, "light"), (true, "dark")] {
        drv.ok(&Command::Appearance { dark }).await.unwrap();
        tokio::time::sleep(SETTLE).await;
        let name = format!("ios-{device}-compact-{state}-{appearance}");
        let frame = drv.render(&dir.join(format!("{name}.png"))).await.unwrap();
        println!("A11Y {name}\n{}", slopty_e2e::frame_text(&frame.a11y));
        // A finished turn's done line, and a reply's row with its Copy (on touch it stays shown),
        // name when they happened in the person's time: facts of the run, left out of the
        // golden. The reply's row is masked across the window, its date left of the Copy.
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixels")]
        let px = |v: f32| (v * frame.scale).max(0.0).round() as u32;
        let masks: Vec<PixelRect> = frame
            .a11y
            .iter()
            .filter_map(|n| {
                let label = n.label.as_deref()?;
                let [x, y, w, h] = n.bounds;
                if label.contains("Worked for ") {
                    Some([px(x - 4.0), px(y - 4.0), px(w + 8.0), px(h + 8.0)])
                } else if n.role == "Button" && label == "Copy message" {
                    Some([0, px(y - 4.0), frame.image.width(), px(h + 8.0)])
                } else {
                    None
                }
            })
            .collect();
        assert_matches_masked(&name, &frame, TOLERANCE, &artifacts_dir(), &masks).unwrap();
    }
    drv.ok(&Command::Appearance { dark: false }).await.unwrap();
}

/// The Chats page is up.
pub fn chats_page(d: &Dump) -> bool {
    d.a11y_node("Group", Some("Chats")).is_some() && d.a11y_node("TabList", Some("Tabs")).is_some()
}

#[tokio::test]
#[ignore = "live: cargo xtask e2e ios"]
async fn a_phone_opens_on_its_chats_and_one_takes_the_screen() {
    let mut stack = Stack::launch_on_simulator("e2e-ios-worker", simulator()).await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let drv = &mut stack.driver;
    let dump = drv.wait_for("the worker", STEP, |d| d.status == "connected").await.unwrap();
    let phone = dump.window.width < PHONE_BELOW;
    let device = if phone { "phone" } else { "pad" };

    if !phone {
        // An iPad keeps the columns: no Chats page, the sidebar's list beside the chat.
        let dump = drv.dump().await.unwrap();
        assert!(!chats_page(&dump), "{:#?}", dump.a11y);
        goldens(drv, &dir, device, "columns").await;
        touch_targets(drv, "pad columns").await;
        stack.shutdown().await;
        return;
    }

    let dump = drv.wait_for("the Chats page", STEP, chats_page).await.unwrap();
    // The page keeps clear of the status bar and the home indicator.
    let tabs = dump.a11y_node("TabList", Some("Tabs")).map(|n| n.bounds).unwrap();
    assert!(tabs[1] + tabs[3] <= dump.window.height + 0.5, "{tabs:?} in {:?}", dump.window);
    for tab in ["Chats", "Settings", "Search"] {
        assert!(dump.a11y_node("Tab", Some(tab)).is_some(), "{tab}: {:#?}", dump.a11y);
    }
    // Settings is a tab, so the desktop's sidebar foot (the server pill, the gear) is gone.
    assert!(dump.a11y_node("Button", Some("Settings")).is_none(), "{:#?}", dump.a11y);
    // Before any project the capsule offers the desktop canvas's step, never a new chat that
    // has nowhere to go.
    assert!(dump.a11y_node("Button", Some("New session")).is_none(), "{:#?}", dump.a11y);
    goldens(drv, &dir, device, "chats").await;

    // It opens the project picker: the worker, its Home, and ⌘⏎ adds the folder open there.
    // The phone lands on the project's new chat, as the Mac does.
    tap(drv, "Button", "Add a project").await;
    drv.wait_for("the device step", STEP, |d| saying(d, "e2e-ios-worker").is_some()).await.unwrap();
    drv.type_text("e2e-ios-worker").await.unwrap();
    drv.keys("enter").await.unwrap();
    drv.wait_for("the locations", STEP, |d| d.a11y_node("Button", Some("Home")).is_some())
        .await
        .unwrap();
    drv.keys("enter").await.unwrap();
    drv.wait_for("Home's folders", STEP, |d| saying(d, "Clone repository").is_some())
        .await
        .unwrap();
    drv.keys("cmd-enter").await.unwrap();
    let landed = drv.wait_for("the project's new chat", STEP, |d| !chats_page(d)).await;
    if let Err(e) = landed {
        let shown = drv.dump().await.unwrap();
        panic!("{e:#}\nthe phone said:\n{}", slopty_e2e::frame_text(&shown.a11y));
    }
    tap(drv, "Button", "Close").await;
    drv.wait_for("Chats with its project", STEP, chats_page).await.unwrap();

    // "New session" takes the screen for the new session, under the phone's
    // navigation bar: its title and a close, none of the desktop's sidebar and history cluster.
    tap(drv, "Button", "New session").await;
    let page = drv.wait_for("the new session", STEP, |d| !chats_page(d)).await.unwrap();
    let bar = page.a11y_node("Group", Some("Navigation")).map(|n| n.bounds);
    let [_, bar_top, _, bar_height] = bar.unwrap_or_else(|| panic!("{:#?}", page.a11y));
    assert!(page.a11y_node("Heading", Some("New session")).is_some(), "{:#?}", page.a11y);
    let close = page.a11y_node("Button", Some("Close")).map(|n| n.bounds).unwrap();
    assert!(close[1] >= bar_top && close[1] + close[3] <= bar_top + bar_height + 0.5, "{close:?}");
    for desktop in ["Toggle left sidebar", "Back", "Forward"] {
        assert!(page.a11y_node("Button", Some(desktop)).is_none(), "{desktop}: {:#?}", page.a11y);
    }
    goldens(drv, &dir, device, "new-session").await;

    // Its close goes back to Chats.
    tap(drv, "Button", "Close").await;
    drv.wait_for("the Chats page again", STEP, chats_page).await.unwrap();

    // Settings from the tab bar, and back from its page to Chats.
    tap(drv, "Tab", "Settings").await;
    drv.wait_for("the settings", STEP, |d| d.a11y_node("Group", Some("Settings")).is_some())
        .await
        .unwrap();
    tap(drv, "Button", "Back").await;
    drv.wait_for("Chats after the settings", STEP, chats_page).await.unwrap();
    stack.shutdown().await;
}

/// A node of `d` whose label or value holds `words`, whatever its role.
pub fn saying<'a>(d: &'a Dump, words: &str) -> Option<&'a slopty_e2e::A11yNode> {
    let holds = |s: Option<&str>| s.is_some_and(|s| s.contains(words));
    d.a11y.iter().find(|n| holds(n.label.as_deref()) || holds(n.value.as_deref()))
}
