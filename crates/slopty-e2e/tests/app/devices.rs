//! Settings → Devices on a real stack: the server's row offers the code a phone or iPad scans,
//! which carries this Mac's tailnet name for a server on this Mac; and Add device's look on the
//! tailnet and a run that fails.
//!
//! The code and the address under it carry the stack's own server port, so the invite's golden
//! is held outside them, in pixels and in words. Every state is a golden in light and in dark.

use std::path::Path;
use std::time::Duration;

use slopty_e2e::harness::artifacts_dir;
use slopty_e2e::snapshot::{MAC_TOLERANCE as TOLERANCE, PixelRect, assert_matches_apart};
use slopty_e2e::{A11yNode, Command, Driver, Dump, Stack, TAILNET_STATUS_ENV};

/// How long a worker round trip may take.
const STEP: Duration = Duration::from_secs(30);
/// Time for what the accessibility tree cannot show (a look on the tailnet) to arrive and draw.
const SETTLE: Duration = Duration::from_secs(3);
/// This Mac on the stand-in tailnet.
const TAILNET: &str = r#"{"BackendState":"Running","Self":{"ID":"n1","HostName":"mac-studio",
"DNSName":"mac-studio.tail1234.ts.net.","OS":"macOS","TailscaleIPs":["100.64.0.3"],
"Online":true,"UserID":2},"Peer":null}"#;
/// A machine no resolver knows (`.invalid` is reserved for that), for a run that fails.
const NO_HOST: &str = "nohost.invalid";

/// The node `role` named `label` in `dump`'s accessibility tree.
fn node<'a>(dump: &'a Dump, role: &str, label: &str) -> Option<&'a A11yNode> {
    dump.a11y.iter().find(|n| n.role == role && n.label.as_deref() == Some(label))
}

/// Click the middle of the node `role` named `label`, once it is drawn.
async fn click(drv: &mut Driver, role: &str, label: &str) {
    let what = format!("the {role} {label}");
    let dump = drv.wait_for(&what, STEP, |d| node(d, role, label).is_some()).await.unwrap();
    let [x, y, w, h] = node(&dump, role, label).map(|n| n.bounds).unwrap_or_default();
    drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
}

/// The device-pixel rectangles of the nodes `masked` picks, on a frame drawn at `scale`.
fn masks(a11y: &[A11yNode], scale: f32, masked: impl Fn(&A11yNode) -> bool) -> Vec<PixelRect> {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixels")]
    let px = |v: f32| (v * scale).max(0.0).round() as u32;
    a11y.iter()
        .filter(|n| masked(n))
        .map(|n| {
            let [x, y, w, h] = n.bounds;
            [px(x), px(y), px(w), px(h)]
        })
        .collect()
}

/// Render the window in light and in dark and hold each against `golden/<name>-<appearance>.png`,
/// outside the nodes `masked` picks; the window is light again after.
async fn goldens(
    drv: &mut Driver,
    renders: &Path,
    name: &str,
    masked: impl Fn(&A11yNode) -> bool + Copy,
) {
    std::fs::create_dir_all(renders).unwrap();
    for (dark, appearance) in [(false, "light"), (true, "dark")] {
        drv.ok(&Command::Appearance { dark }).await.unwrap();
        // The appearance's first frame, and its hover and fade tweens settled.
        tokio::time::sleep(Duration::from_millis(600)).await;
        let golden = format!("{name}-{appearance}");
        let frame = drv.render(&renders.join(format!("{golden}.png"))).await.unwrap();
        println!("A11Y {golden}\n{}", slopty_e2e::frame_text(&frame.a11y));
        let hidden = masks(&frame.a11y, frame.scale, masked);
        assert_matches_apart(&golden, &frame, TOLERANCE, &artifacts_dir(), (&hidden, &hidden))
            .unwrap();
    }
    drv.ok(&Command::Appearance { dark: false }).await.unwrap();
}

/// The server on this Mac is reached by this Mac's tailnet name: its row's "Connect a phone"
/// shows the code with that address under it, and Done closes it.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_server_s_code_carries_this_mac_s_tailnet_name() {
    let dir = tempfile::tempdir().unwrap();
    let tailnet = dir.path().join("tailnet.json");
    std::fs::write(&tailnet, TAILNET).unwrap();
    let mut stack =
        Stack::launch_with("e2e-invite", &[(TAILNET_STATUS_ENV, tailnet.to_str().unwrap())])
            .await
            .unwrap();
    let port = stack.server.address().rsplit_once(':').map(|(_, port)| port.to_owned()).unwrap();
    let address = format!("mac-studio.tail1234.ts.net:{port}");
    let renders = artifacts_dir().join("renders");
    let drv = &mut stack.driver;
    drv.wait_for("the worker", STEP, |d| d.status == "connected").await.unwrap();

    drv.keys("cmd-,").await.unwrap();
    click(drv, "Tab", "Devices").await;
    click(drv, "Button", "Connect a phone or iPad").await;
    let shown = drv
        .wait_for("the code", STEP, |d| {
            node(d, "Dialog", "Connect a phone or iPad").is_some()
                && node(d, "Image", "The server's code").is_some()
        })
        .await
        .unwrap();
    assert!(
        shown.a11y.iter().any(|n| n.label.as_deref() == Some(&*address)
            || n.value.as_deref() == Some(&*address)),
        "the address under the code: {}",
        slopty_e2e::frame_text(&shown.a11y)
    );
    // The code and its address carry the run's port; the dialog around them is held whole.
    let address_ref = address.as_str();
    goldens(drv, &renders, "settings-devices-invite", move |n| {
        n.label.as_deref() == Some("The server's code")
            || n.label.as_deref() == Some(address_ref)
            || n.value.as_deref() == Some(address_ref)
    })
    .await;

    click(drv, "Button", "Done").await;
    drv.wait_for("the code closed", STEP, |d| {
        node(d, "Dialog", "Connect a phone or iPad").is_none()
    })
    .await
    .unwrap();
    stack.shutdown().await;
}

/// Add device on a tailnet with no Slopty server on it says so under "A server"; "Over SSH" to a
/// machine no resolver knows fails in the error strip, which names it, and the dialog offers to
/// try again.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn add_device_says_what_the_tailnet_holds_and_why_a_run_failed() {
    let dir = tempfile::tempdir().unwrap();
    let tailnet = dir.path().join("tailnet.json");
    std::fs::write(&tailnet, TAILNET).unwrap();
    let mut stack =
        Stack::launch_with("e2e-add-device", &[(TAILNET_STATUS_ENV, tailnet.to_str().unwrap())])
            .await
            .unwrap();
    let renders = artifacts_dir().join("renders");
    let drv = &mut stack.driver;
    drv.wait_for("the worker", STEP, |d| d.status == "connected").await.unwrap();

    drv.keys("cmd-,").await.unwrap();
    click(drv, "Tab", "Devices").await;
    click(drv, "Button", "Add device").await;
    drv.wait_for("the dialog", STEP, |d| node(d, "Dialog", "Add device").is_some()).await.unwrap();

    // A server: the look on the stand-in tailnet finds none, and says so.
    click(drv, "Tab", "A server").await;
    drv.wait_for("A server chosen", STEP, |d| node(d, "Button", "Connect").is_some())
        .await
        .unwrap();
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &renders, "settings-devices-add-server", |_| false).await;

    // Over SSH to a machine that resolves nowhere: the run fails, and the strip says why.
    click(drv, "Tab", "Over SSH").await;
    drv.wait_for("Over SSH chosen", STEP, |d| node(d, "Button", "Install").is_some())
        .await
        .unwrap();
    drv.type_text(NO_HOST).await.unwrap();
    click(drv, "Button", "Install").await;
    let failed = drv
        .wait_for("the run failed", Duration::from_secs(60), |d| {
            d.a11y.iter().any(|n| n.role == "Alert") && node(d, "Button", "Try again").is_some()
        })
        .await
        .unwrap();
    let strip = failed.a11y.iter().find(|n| n.role == "Alert").unwrap();
    let [_, _, width, height] = strip.bounds;
    assert!(height >= 40.0 && width >= 200.0, "the strip holds its words: {:?}", strip.bounds);
    tokio::time::sleep(Duration::from_millis(600)).await;
    goldens(drv, &renders, "settings-devices-add-failed", |_| false).await;

    drv.keys("escape").await.unwrap();
    drv.wait_for("the dialog gone", STEP, |d| node(d, "Dialog", "Add device").is_none())
        .await
        .unwrap();
    stack.shutdown().await;
}
