//! The phone's anatomy on the simulator (`cargo xtask e2e ios --sim iphone|ipad`): a phone
//! opens on its Chats page (the large title, the chat list, the "New chat" capsule and
//! the tab bar, with no sidebar foot), "New chat" takes the whole screen for the new chat under
//! a navigation bar (its title and a close button, none of the desktop's sidebar and history
//! cluster), and its close goes back. An iPad keeps the shell's columns, the sidebar beside the
//! chat. Each state is a golden in light and in dark, `ios-<device>-compact-*`.

use std::path::Path;
use std::time::Duration;

use slopty_e2e::harness::{Simulator, Stack, artifacts_dir};
use slopty_e2e::snapshot::{PixelRect, assert_matches_masked};
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

/// How far above the safe area a chat's composer row may end, points: its own padding and the
/// mode row's, never a keyboard's band.
const REACHES: f32 = 64.0;

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
async fn goldens(drv: &mut Driver, dir: &Path, device: &str, state: &str) {
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
fn chats_page(d: &Dump) -> bool {
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
    goldens(drv, &dir, device, "chats").await;

    // "New chat" takes the screen for the new chat, its composer ready, under the phone's
    // navigation bar: its title and a close, none of the desktop's sidebar and history cluster.
    tap(drv, "Button", "New chat").await;
    let page = drv.wait_for("the new chat", STEP, |d| !chats_page(d)).await.unwrap();
    let bar = page.a11y_node("Group", Some("Navigation")).map(|n| n.bounds);
    let [_, bar_top, _, bar_height] = bar.unwrap_or_else(|| panic!("{:#?}", page.a11y));
    assert!(page.a11y_node("Heading", Some("New chat")).is_some(), "{:#?}", page.a11y);
    let close = page.a11y_node("Button", Some("Close")).map(|n| n.bounds).unwrap();
    assert!(close[1] >= bar_top && close[1] + close[3] <= bar_top + bar_height + 0.5, "{close:?}");
    for desktop in ["Toggle left sidebar", "Back", "Forward"] {
        assert!(page.a11y_node("Button", Some(desktop)).is_none(), "{desktop}: {:#?}", page.a11y);
    }
    goldens(drv, &dir, device, "new-chat").await;

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
fn saying<'a>(d: &'a Dump, words: &str) -> Option<&'a slopty_e2e::A11yNode> {
    let holds = |s: Option<&str>| s.is_some_and(|s| s.contains(words));
    d.a11y.iter().find(|n| holds(n.label.as_deref()) || holds(n.value.as_deref()))
}

/// A phone's chat holds the dock and the agent's subagents as the Mac's does: a stand-in Claude
/// Code leaves a command running in the background and runs a subagent. Tapped from Chats,
/// the chat takes the screen with its footer counting the command and the subagent's card in the
/// transcript; a tap on the card drills into the subagent's run under its breadcrumb, which leads
/// back, and a screenshot pasted into its composer is staged there. An iPad shows the same chat
/// beside its sidebar, picked from the list.
#[tokio::test]
#[ignore = "live: cargo xtask e2e ios"]
async fn a_phones_chat_holds_the_dock_and_a_subagents_card() {
    use slopty_e2e::harness::slopty_json;

    let mut stack =
        Stack::launch_on_simulator_with_claude("e2e-ios-dock", simulator()).await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let drv = &mut stack.driver;
    let dump = drv.wait_for("the worker", STEP, |d| d.status == "connected").await.unwrap();
    let phone = dump.window.width < PHONE_BELOW;
    let (address, cli) = (stack.server.address().to_owned(), stack.path("cli"));
    let folder = stack.path("repo");
    std::fs::create_dir_all(&folder).unwrap();
    let folder = folder.to_string_lossy().into_owned();
    let prompt = "background sleep 600";
    let start = ["agent", "start", "e2e-ios-dock", "--folder", &folder, prompt];
    let started = slopty_json(&address, &cli, &start, b"").await.unwrap();
    let agent = started["agent"].as_str().expect("an agent started").to_owned();
    let read = async |words: &str| {
        let since = std::time::Instant::now();
        loop {
            let args = ["agent", "read", "--thread", agent.as_str()];
            let read = slopty_json(&address, &cli, &args, b"").await.unwrap();
            if read.to_string().contains(words) {
                return;
            }
            assert!(since.elapsed() < STEP * 3, "the thread never said {words:?}: {read}");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    };
    read("it runs in the background").await;

    let drv = &mut stack.driver;
    if !phone {
        // An iPad's chat is the Mac's, beside the sidebar: picked from its list, its footer
        // counting the command.
        let listed = drv
            .wait_for("the chat listed", STEP, |d| {
                d.a11y.iter().any(|n| n.role == "Button" && n.label.as_deref() == Some(prompt))
            })
            .await
            .unwrap();
        let row = listed.a11y_node("Button", Some(prompt)).map(|n| n.bounds).unwrap();
        drv.ui_tap(row[0] + row[2] / 2.0, row[1] + row[3] / 2.0).await.unwrap();
        let chat = drv
            .wait_for("the chat with its footer", STEP, |d| {
                d.a11y.iter().any(|n| n.role == "MultilineTextInput")
                    && saying(d, "1 command").is_some()
            })
            .await;
        if let Err(e) = chat {
            let shown = drv.dump().await.unwrap();
            panic!("{e:#}\nthe iPad said:\n{}", slopty_e2e::frame_text(&shown.a11y));
        }
        tokio::time::sleep(SETTLE).await;
        goldens(drv, &dir, "pad", "chat").await;
        touch_targets(drv, "pad chat").await;
        stack.shutdown().await;
        return;
    }
    let sessions = drv
        .wait_for("the chat listed", STEP, |d| chats_page(d) && saying(d, prompt).is_some())
        .await
        .unwrap();
    let [left, top, width, height] = saying(&sessions, prompt).unwrap().bounds;
    drv.ui_tap(width.mul_add(0.5, left), height.mul_add(0.5, top)).await.unwrap();
    // The person asks for a subagent from the phone's composer: the CLI's `agent tell` reaches
    // Claude Code as another agent's report, which it is not asked to act on alone.
    let composer = drv
        .wait_for("the chat's composer", STEP, |d| {
            d.a11y.iter().any(|n| n.role == "MultilineTextInput")
        })
        .await
        .unwrap();
    let [left, top, width, height] =
        composer.a11y.iter().find(|n| n.role == "MultilineTextInput").unwrap().bounds;
    drv.ui_tap(width.mul_add(0.5, left), height.mul_add(0.5, top)).await.unwrap();
    drv.ui_insert_text("agent").await.unwrap();
    let typed = drv
        .wait_for("the draft's Send", STEP, |d| {
            d.a11y_node("Button", Some("Send message")).is_some()
        })
        .await
        .unwrap();
    let [left, top, width, height] =
        typed.a11y_node("Button", Some("Send message")).unwrap().bounds;
    drv.ui_tap(width.mul_add(0.5, left), height.mul_add(0.5, top)).await.unwrap();
    read("Done: the agent is back.").await;
    let chat = drv
        .wait_for("the chat with its card and its footer", STEP, |d| {
            d.a11y_node("Button", Some("Subagent Look")).is_some()
                && saying(d, "1 command").is_some()
        })
        .await;
    let chat = match chat {
        Ok(chat) => chat,
        Err(e) => {
            let shown = drv.dump().await.unwrap();
            panic!("{e:#}\nthe phone said:\n{}", slopty_e2e::frame_text(&shown.a11y));
        }
    };
    tokio::time::sleep(SETTLE).await;
    // The send let the composer go, so the page reaches the safe area again: no keyboard's band
    // left below the composer, whatever keyboard frame UIKit last reported.
    let sent = drv.dump().await.unwrap();
    let floor = sent.window.height - sent.window.safe_bottom;
    let lowest = sent
        .a11y
        .iter()
        .filter(|n| n.role == "Button" || n.role == "MultilineTextInput")
        .map(|n| n.bounds[1] + n.bounds[3])
        .fold(0.0, f32::max);
    println!(
        "MEASURE phone-after-send: the composer's last control ends at {lowest:.1} pt, the safe \
         area at {floor:.1} (window {:.1}, safe bottom {:.1}, keyboard {:.1}, focus {} {})",
        sent.window.height,
        sent.window.safe_bottom,
        sent.window.keyboard_bottom,
        sent.focused,
        sent.focus_path
    );
    assert!(
        floor - lowest <= REACHES,
        "after a send the page stops {:.1} pt above the safe area:\n{}",
        floor - lowest,
        slopty_e2e::frame_text(&sent.a11y)
    );
    goldens(drv, &dir, "phone", "chat").await;
    let card = chat.a11y_node("Button", Some("Subagent Look")).map(|n| n.bounds).unwrap();
    assert!(card[0] >= 0.0 && card[0] + card[2] <= chat.window.width + 0.5, "{card:?} on screen");

    // A tap on the card drills in: the subagent's run is the session view, under its breadcrumb.
    drv.ui_tap(card[0] + card[2] / 2.0, card[1] + card[3] / 2.0).await.unwrap();
    let drilled = drv
        .wait_for("the subagent's run", STEP, |d| {
            d.a11y.iter().any(|n| {
                n.role == "Navigation" && n.label.as_deref().is_some_and(|l| l.ends_with("› Look"))
            })
        })
        .await;
    if let Err(e) = drilled {
        let shown = drv.dump().await.unwrap();
        panic!("{e:#}\nthe phone said:\n{}", slopty_e2e::frame_text(&shown.a11y));
    }
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &dir, "phone", "subagent").await;
    let back = drv.dump().await.unwrap();
    let link = back
        .a11y
        .iter()
        .find(|n| n.role == "Link" && n.label.as_deref().is_some_and(|l| l.starts_with("Back to ")))
        .map(|n| n.bounds);
    let [x, y, w, h] = link.unwrap_or_else(|| panic!("the way back: {:#?}", back.a11y));
    drv.ui_tap(x + w / 2.0, y + h / 2.0).await.unwrap();
    drv.wait_for("the parent's chat again", STEP, |d| {
        d.a11y_node("Button", Some("Subagent Look")).is_some()
    })
    .await
    .unwrap();

    // A screenshot pasted into the composer is staged as its attachment, its chip in the draft
    // where the caret stood (zeron's inline mention, `[Image 1](slopty-image:1)`), which the
    // thumbnail strip then leaves out.
    let composer = drv.dump().await.unwrap();
    let [left, top, width, height] =
        composer.a11y.iter().find(|n| n.role == "MultilineTextInput").unwrap().bounds;
    drv.ui_tap(width.mul_add(0.5, left), height.mul_add(0.5, top)).await.unwrap();
    drv.clipboard_image("image/png", &screenshot()).await.unwrap();
    drv.keys("cmd-v").await.unwrap();
    let staged = drv
        .wait_for("the pasted picture staged", STEP, |d| {
            d.a11y.iter().any(|n| {
                n.role == "MultilineTextInput"
                    && n.value.as_deref().is_some_and(|v| v.contains("](slopty-image:"))
            })
        })
        .await;
    if let Err(e) = staged {
        let shown = drv.dump().await.unwrap();
        panic!("{e:#}\nthe phone said:\n{}", slopty_e2e::frame_text(&shown.a11y));
    }
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &dir, "phone", "paste").await;
    stack.shutdown().await;
}

/// A small picture, as a screenshot pasted from the clipboard would be: two bands of the
/// brand's green over a pale ground, the same bytes every run.
fn screenshot() -> Vec<u8> {
    let picture = image::RgbaImage::from_fn(96, 64, |_, y| {
        if y < 20 {
            image::Rgba([46, 160, 90, 255])
        } else if y < 40 {
            image::Rgba([228, 242, 233, 255])
        } else {
            image::Rgba([30, 110, 64, 255])
        }
    });
    let mut png = std::io::Cursor::new(Vec::new());
    picture.write_to(&mut png, image::ImageFormat::Png).unwrap();
    png.into_inner()
}
