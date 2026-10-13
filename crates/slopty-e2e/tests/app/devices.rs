//! Settings → Devices on a real stack: the server's row offers the code a phone or iPad scans,
//! which carries this Mac's tailnet name for a server on this Mac.
//!
//! The code and the address under it carry the stack's own server port, so the dialog is held
//! to its accessibility tree, and its render is kept for a design review
//! (`target/e2e/artifacts/settings-devices-invite.png`) rather than held to a golden.

use std::time::Duration;

use slopty_e2e::harness::artifacts_dir;
use slopty_e2e::{A11yNode, Driver, Dump, Stack, TAILNET_STATUS_ENV};

/// How long a worker round trip may take.
const STEP: Duration = Duration::from_secs(30);
/// This Mac on the stand-in tailnet.
const TAILNET: &str = r#"{"BackendState":"Running","Self":{"ID":"n1","HostName":"mac-studio",
"DNSName":"mac-studio.tail1234.ts.net.","OS":"macOS","TailscaleIPs":["100.64.0.3"],
"Online":true,"UserID":2},"Peer":null}"#;

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
    drv.render(&artifacts_dir().join("settings-devices-invite.png")).await.unwrap();

    click(drv, "Button", "Done").await;
    drv.wait_for("the code closed", STEP, |d| {
        node(d, "Dialog", "Connect a phone or iPad").is_none()
    })
    .await
    .unwrap();
    stack.shutdown().await;
}
