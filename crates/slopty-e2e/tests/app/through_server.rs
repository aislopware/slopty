//! The app as it is normally used: at its first run, pointed at a server from Settings → Devices,
//! it finds its workers in the directory and dials each one itself (`harness::ServerFleet`): worker
//! `near` on loopback, and worker `far` behind a relay shaped like the tailnet
//! (`harness::TAILNET`), at the address the server lists for it.
//!
//! `far` is then killed and restarted twice: once as soon as the app shows it down, which its own
//! link finds again; once after the server has called it unreachable and the app holds it, where
//! the server listing it online again is what wakes the dial. Each is measured and bounded.

use std::time::{Duration, Instant};

use slopty_e2e::harness::{ServerFleet, TAILNET};
use slopty_e2e::{A11yNode, Driver, Dump};

/// A round trip over the shaped link, the directory arriving.
const STEP: Duration = Duration::from_secs(30);
/// A killed worker must show down within this: the app's silence bar is three missed
/// keep-alives, 3 s.
const DOWN_BOUND: Duration = Duration::from_secs(5);
/// A restarted worker must be connected again within this, whichever path finds it.
const BACK_BOUND: Duration = Duration::from_secs(2);
/// The server's lease ends 5 s after a worker goes silent; this leaves the app's dial the time
/// to have failed and the server's word the time to arrive.
const HELD_BOUND: Duration = Duration::from_secs(15);
const NEAR: &str = "studio";
const FAR: &str = "remote";

fn status<'d>(d: &'d Dump, name: &str) -> Option<&'d str> {
    d.workers.iter().find(|w| w.name == name).map(|w| w.status.as_str())
}

fn connected(d: &Dump, name: &str) -> bool {
    status(d, name) == Some("connected")
}

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

/// The server's word on worker `name`.
async fn liveness(fleet: &ServerFleet, name: &str) -> String {
    let directory = fleet.directory().await.unwrap();
    let all = directory.as_array().cloned().unwrap_or_default();
    let entry = all.iter().find(|w| w["name"] == name);
    entry.and_then(|w| w["liveness"].as_str()).unwrap_or_default().to_owned()
}

/// The server's address typed into Add device on "A server" and connected: both workers come
/// from the directory, nobody adds them; far, killed, shows down and comes back within bounds,
/// both while the app shows it down and after the server holds it unreachable.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_app_finds_its_workers_through_the_server() {
    let started = Instant::now();
    let mut fleet = ServerFleet::launch(NEAR, FAR, TAILNET).await.unwrap();
    let address = fleet.server.address().to_owned();
    let drv = &mut fleet.driver;

    drv.keys("cmd-,").await.unwrap();
    click(drv, "Tab", "Devices").await;
    click(drv, "Button", "Connect server").await;
    drv.wait_for("Add device", STEP, |d| node(d, "Dialog", "Add device").is_some()).await.unwrap();
    drv.type_text(&address).await.unwrap();
    click(drv, "Button", "Connect").await;
    let entered = Instant::now();
    drv.wait_for("both workers from the directory, connected", STEP, |d| {
        connected(d, NEAR) && connected(d, FAR)
    })
    .await
    .unwrap();
    println!(
        "MEASURE through-server: both workers connected {:.2} s after the server's address was \
         entered",
        entered.elapsed().as_secs_f64()
    );

    // Killed, then restarted the moment the app shows it down: the app's own link finds it.
    fleet.far.kill_worker().await;
    let killed = Instant::now();
    fleet
        .driver
        .wait_for("far shown down", STEP, |d| !connected(d, FAR) && connected(d, NEAR))
        .await
        .unwrap();
    let down = killed.elapsed();
    println!("MEASURE through-server: far shown down {:.1} s after the kill", down.as_secs_f64());
    fleet.far.restart_worker().await.unwrap();
    let restarted = Instant::now();
    fleet.driver.wait_for("far back", STEP, |d| connected(d, FAR)).await.unwrap();
    let back = restarted.elapsed();
    println!(
        "MEASURE through-server: far connected again {:.2} s after a restart while shown down",
        back.as_secs_f64()
    );
    assert!(down <= DOWN_BOUND, "shown down after {down:?}, over {DOWN_BOUND:?}");
    assert!(back <= BACK_BOUND, "back after {back:?}, over {BACK_BOUND:?}");

    // Killed until the server calls it unreachable and the app holds it; restarted, the server
    // lists it online and that wakes the dial.
    fleet.far.kill_worker().await;
    fleet
        .driver
        .wait_for("far held as unreachable", HELD_BOUND, |d| status(d, FAR) == Some("unreachable"))
        .await
        .unwrap();
    assert_eq!(liveness(&fleet, FAR).await, "unreachable");
    fleet.far.restart_worker().await.unwrap();
    let restarted = Instant::now();
    fleet.driver.wait_for("far back from the hold", STEP, |d| connected(d, FAR)).await.unwrap();
    let back = restarted.elapsed();
    println!(
        "MEASURE through-server: far connected again {:.2} s after a restart while held \
         unreachable; whole run {:.1} s",
        back.as_secs_f64(),
        started.elapsed().as_secs_f64()
    );
    assert!(back <= BACK_BOUND, "back from the hold after {back:?}, over {BACK_BOUND:?}");
    assert_eq!(liveness(&fleet, FAR).await, "online");
    fleet.shutdown().await;
}
