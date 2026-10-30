//! The phone's anatomy on the simulator (`cargo xtask e2e ios --sim iphone|ipad`): a phone
//! opens on its Sessions page (the large title, the chat list, the capsule and the tab bar, with no
//! sidebar foot). Before any project the capsule adds one, and the phone lands on the project's
//! page under a navigation bar (a close, none of the desktop's sidebar and history cluster),
//! whose close goes back. After it, "New session" puts the picker of projects and agents over
//! Sessions, and a tap outside it puts it away. An iPad keeps the shell's columns, the sidebar
//! beside the chat. Each state is a golden in light and in dark, `ios-<device>-compact-*`.

use std::path::Path;
use std::time::Duration;

use slopty_e2e::harness::{Simulator, Stack, artifacts_dir};
use slopty_e2e::snapshot::{PixelRect, assert_matches_apart};
use slopty_e2e::{A11yNode, Command, Driver, Dump, WindowInfo};

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
    let window = drv.dump().await.unwrap().window;
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
        let mut pixels: Vec<PixelRect> = Vec::new();
        // In words, only those nodes, each by its own box: a band across the window would also
        // drop whatever node is centred in it (the sidebar's footer, level with a Copy), and the
        // words would hang on where the bands fall.
        let mut words: Vec<PixelRect> = Vec::new();
        for n in &frame.a11y {
            let Some(label) = n.label.as_deref() else { continue };
            let [x, y, w, h] = n.bounds;
            let own = [px(x - 4.0), px(y - 4.0), px(w + 8.0), px(h + 8.0)];
            if label.contains("Worked for ") {
                pixels.push(own);
                words.push(own);
            } else if n.role == "Button" && label == "Copy message" {
                pixels.push([0, px(y - 4.0), frame.image.width(), px(h + 8.0)]);
                words.push(own);
            }
        }
        // Pixels alone: what a band hides has no node, and a node centred in one (the splitter
        // on the seam) says the same in every run.
        pixels.extend(
            inspector_bands(&frame.a11y, window)
                .into_iter()
                .map(|[x, y, w, h]| [px(x), px(y), px(w), px(h)]),
        );
        assert_matches_apart(&name, &frame, TOLERANCE, &artifacts_dir(), (&pixels, &words))
            .unwrap();
    }
    drv.ok(&Command::Appearance { dark: false }).await.unwrap();
}

/// The inspector's bands between its words, across its width, in points: where its lines that
/// name a moment of the run sit (a message's time beside its Copy, a turn's done line), which the
/// iOS tree has no node for. The inspector is the phone's sheet, else what lies right of the
/// session's terminal and down to the window's foot (an iPad's pane, beside the terminal and the
/// shell under it); none while it is shut.
fn inspector_bands(a11y: &[A11yNode], window: WindowInfo) -> Vec<[f32; 4]> {
    let sheet = a11y.iter().find(|n| n.role == "Dialog" && n.label.as_deref() == Some("Inspector"));
    let (window_width, window_height) = (window.width, window.height);
    let pane = || {
        let [x, y, width, _] = a11y.iter().find(|n| n.role == "Terminal")?.bounds;
        let right = x + width;
        (right < window_width - 1.0).then_some([right, y, window_width - right, window_height - y])
    };
    let Some([left, top, width, height]) = sheet.map(|n| n.bounds).or_else(pane) else {
        return Vec::new();
    };
    let inside = |n: &&A11yNode| {
        let [x, y, ..] = n.bounds;
        x >= left - 0.5 && x < left + width && y >= top - 0.5 && y < top + height
    };
    let mut words: Vec<(f32, f32)> = a11y
        .iter()
        .filter(|n| matches!(n.role.as_str(), "Paragraph" | "Heading"))
        .filter(inside)
        .map(|n| (n.bounds[1], n.bounds[1] + n.bounds[3]))
        .collect();
    words.sort_by(|a, b| a.0.total_cmp(&b.0));
    let ends = words.iter().map(|&(_, bottom)| bottom);
    let starts = words.iter().skip(1).map(|&(start, _)| start).chain(std::iter::once(top + height));
    ends.zip(starts)
        .filter(|(end, start)| start > end)
        .map(|(end, start)| [left, end, width, start - end])
        .collect()
}

/// The Sessions page is up.
pub fn chats_page(d: &Dump) -> bool {
    d.a11y_node("Group", Some("Sessions")).is_some()
        && d.a11y_node("TabList", Some("Tabs")).is_some()
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
        // An iPad keeps the columns: no Sessions page, the sidebar's list beside the chat.
        let dump = drv.dump().await.unwrap();
        assert!(!chats_page(&dump), "{:#?}", dump.a11y);
        goldens(drv, &dir, device, "columns").await;
        touch_targets(drv, "pad columns").await;
        stack.shutdown().await;
        return;
    }

    let dump = drv.wait_for("the Sessions page", STEP, chats_page).await.unwrap();
    // The page keeps clear of the status bar and the home indicator.
    let tabs = dump.a11y_node("TabList", Some("Tabs")).map(|n| n.bounds).unwrap();
    assert!(tabs[1] + tabs[3] <= dump.window.height + 0.5, "{tabs:?} in {:?}", dump.window);
    for tab in ["Sessions", "Settings", "Search"] {
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
    drv.wait_for("Sessions with its project", STEP, chats_page).await.unwrap();

    // "New session" puts its picker over Sessions: the project, and the agent whose TUI starts
    // there. None of the desktop's sidebar and history cluster, and no page of its own.
    tap(drv, "Button", "New session").await;
    let picker = |d: &Dump| d.a11y_node("TabList", Some("Agent")).is_some();
    let page = drv.wait_for("the new-session picker", STEP, picker).await.unwrap();
    assert!(chats_page(&page), "over Sessions: {:#?}", page.a11y);
    assert!(page.a11y_node("Group", Some("Navigation")).is_none(), "{:#?}", page.a11y);
    let start =
        |d: &Dump| d.a11y.iter().any(|n| n.label.as_deref() == Some("Start Claude Code in home"));
    assert!(start(&page), "the project, to start Claude Code in: {:#?}", page.a11y);
    for desktop in ["Toggle left sidebar", "Back", "Forward"] {
        assert!(page.a11y_node("Button", Some(desktop)).is_none(), "{desktop}: {:#?}", page.a11y);
    }
    goldens(drv, &dir, device, "new-session").await;

    // A tap outside it, between the status bar and its card, puts it away: Sessions as it was.
    let field = page.a11y_node("MultilineTextInput", None).map(|n| n.bounds).unwrap();
    drv.ui_tap(page.window.width / 2.0, field[1] / 2.0).await.unwrap();
    drv.wait_for("the Sessions page again", STEP, |d| chats_page(d) && !picker(d)).await.unwrap();

    // Settings from the tab bar, and back from its page to Sessions.
    tap(drv, "Tab", "Settings").await;
    drv.wait_for("the settings", STEP, |d| d.a11y_node("Group", Some("Settings")).is_some())
        .await
        .unwrap();
    tap(drv, "Button", "Back").await;
    drv.wait_for("Sessions after the settings", STEP, chats_page).await.unwrap();
    stack.shutdown().await;
}

/// A node of `d` whose label or value holds `words`, whatever its role.
pub fn saying<'a>(d: &'a Dump, words: &str) -> Option<&'a A11yNode> {
    let holds = |s: Option<&str>| s.is_some_and(|s| s.contains(words));
    d.a11y.iter().find(|n| holds(n.label.as_deref()) || holds(n.value.as_deref()))
}
