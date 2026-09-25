//! Every state of the app worth looking at, rendered by the app's own renderer and held as a
//! golden: the first run, the panels that add a worker, a workspace of columns in both themes,
//! the overview, the palette, the settings, the empty workspace, an agent that needs the
//! human, a remote tile, an upload and a forwarded port.
//!
//! A golden passes or fails on its numbers. The tolerance is blind to a word of chrome text
//! (`docs/decisions/ui.md`), so each scenario also asserts the chrome it shows through the
//! accessibility tree, where a word appearing or going is a string comparison.

use std::time::Duration;

use slopty_e2e::harness::artifacts_dir;
use slopty_e2e::snapshot::assert_matches;
use slopty_e2e::{Command, Driver, Dump, Stack};

/// How long a worker round trip may take.
const STEP: Duration = Duration::from_secs(20);
/// The renders' window: the size the other app goldens use.
const WINDOW: (f32, f32) = (900.0, 600.0);
/// Fraction of pixels allowed to differ from a golden.
const TOLERANCE: f64 = 0.01;

fn gated() -> bool {
    if std::env::var_os("SLOPTY_APP_E2E").is_none() {
        eprintln!("skipped: set SLOPTY_APP_E2E=1 (or run `cargo xtask e2e app`)");
        return false;
    }
    true
}

/// Wait until nothing moves: two dumps a frame apart place every tile alike, so a spring
/// that is still running cannot end up in a golden.
async fn settled(drv: &mut Driver) -> Dump {
    let deadline = tokio::time::Instant::now().checked_add(STEP);
    let mut last = drv.dump().await.unwrap();
    loop {
        tokio::time::sleep(Duration::from_millis(120)).await;
        let next = drv.dump().await.unwrap();
        let still = |a: &[f32; 4], b: &[f32; 4]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.5);
        let same = next.items.len() == last.items.len()
            && next.items.iter().zip(&last.items).all(|(a, b)| still(&a.bounds, &b.bounds));
        if same || deadline.is_none_or(|d| tokio::time::Instant::now() > d) {
            return next;
        }
        last = next;
    }
}

/// Render the frame as it rests and hold it against `golden/<name>.png`.
async fn golden(drv: &mut Driver, stack_dir: &std::path::Path, name: &str) {
    settled(drv).await;
    let frame = drv.render(&stack_dir.join(format!("{name}.png"))).await.unwrap();
    assert_matches(name, &frame, TOLERANCE, &artifacts_dir()).unwrap();
}

/// The labels of every node with `role`, in tree order.
fn labels(d: &Dump, role: &str) -> Vec<String> {
    d.a11y.iter().filter(|n| n.role == role).filter_map(|n| n.label.clone()).collect()
}

/// The first shell, connected and prompted.
async fn first_shell(drv: &mut Driver) -> Dump {
    drv.wait_for("the first shell with a prompt", STEP, |d| {
        d.status == "connected"
            && d.focus.as_deref() == Some("terminal")
            && d.terminals.iter().any(|t| t.rows.iter().any(|r| !r.is_empty()))
    })
    .await
    .unwrap()
}

/// The first run: one way forward, and nothing else on the screen to choose from.
#[tokio::test]
async fn the_first_run_offers_one_way_in() {
    if !gated() {
        return;
    }
    let mut stack = Stack::launch_first_run("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    drv.wait_for("the connect panel", STEP, |d| d.adding).await.unwrap();
    golden(drv, &dir, "first-run").await;
    let dump = drv.dump().await.unwrap();
    assert_eq!(labels(&dump, "Heading"), ["Connect to a server"], "{:#?}", dump.a11y);
    // The way in and the other way in; no menu, no tile, no column marks to wonder about.
    let buttons = labels(&dump, "Button");
    for absent in ["Open", "More", "Columns"] {
        assert!(!buttons.iter().any(|b| b == absent), "{absent} on the first run: {buttons:?}");
    }
    assert!(buttons.iter().any(|b| b == "Connect"), "{buttons:?}");
    assert!(buttons.iter().any(|b| b == "Add a worker by address instead"), "{buttons:?}");

    let switch = dump
        .a11y_node("Button", Some("Add a worker by address instead"))
        .expect("the switch")
        .bounds;
    drv.click(switch[0] + switch[2] / 2.0, switch[1] + switch[3] / 2.0).await.unwrap();
    let dump = drv
        .wait_for("the add-worker panel", STEP, |d| {
            d.a11y_node("Heading", Some("Add a worker")).is_some()
        })
        .await
        .unwrap();
    assert!(labels(&dump, "Button").iter().any(|b| b == "Add"), "{:#?}", dump.a11y);
    // The pointer leaves the link it clicked, so the golden holds the panel at rest and not
    // the link's hover.
    drv.ok(&Command::Move { x: 1.0, y: WINDOW.1 - 1.0 }).await.unwrap();
    golden(drv, &dir, "add-worker").await;

    stack.add_worker().await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    assert!(!dump.adding, "the panel goes with the first worker: {dump:#?}");
    stack.shutdown().await;
}

/// Three columns with the shell focused, the overview, the palette and the settings, then
/// the same workspace dark.
#[tokio::test]
async fn a_workspace_of_columns_in_both_themes() {
    if !gated() {
        return;
    }
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;

    drv.keys("cmd-shift-n").await.unwrap();
    drv.wait_for("the note", STEP, |d| d.item("note").is_some()).await.unwrap();
    drv.type_text("Release\n\n- [x] build the bundle\n- [ ] notarise\n- [ ] tag").await.unwrap();
    drv.open(&["cat"], 1).await.unwrap();
    // The note commits its text on a timer; its title follows the first line.
    drv.wait_for("a third column, the note titled", STEP, |d| {
        d.items.len() == 3 && d.a11y_node("Heading", Some("note Release · 1/3")).is_some()
    })
    .await
    .unwrap();
    drv.keys("cmd-alt-left").await.unwrap();
    drv.keys("cmd-alt-left").await.unwrap();
    let dump = drv
        .wait_for("the first shell focused", STEP, |d| {
            d.items.iter().any(|i| i.active && i.pos[1] == 0)
        })
        .await
        .unwrap();
    assert_eq!(dump.workspace, "Workspace 1");
    golden(drv, &dir, "workspace").await;

    // A desktop-sized window leaves the strip its room, so the navigator docks beside it.
    drv.ok(&Command::Resize { width: 1280.0, height: 800.0 }).await.unwrap();
    drv.wait_for("the navigator docked", STEP, |d| {
        d.a11y_node("Navigation", Some("Navigator")).is_some()
    })
    .await
    .unwrap();
    golden(drv, &dir, "workspace-navigator").await;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    drv.wait_for("the navigator gone again", STEP, |d| {
        d.a11y_node("Navigation", Some("Navigator")).is_none()
    })
    .await
    .unwrap();

    drv.keys("cmd-alt-o").await.unwrap();
    drv.wait_for("the overview", STEP, |d| d.overview).await.unwrap();
    golden(drv, &dir, "overview").await;
    drv.keys("cmd-alt-o").await.unwrap();
    drv.wait_for("the overview closed", STEP, |d| !d.overview).await.unwrap();

    drv.keys("cmd-shift-p").await.unwrap();
    drv.wait_for("the palette", STEP, |d| d.a11y_node("Dialog", Some("Commands")).is_some())
        .await
        .unwrap();
    golden(drv, &dir, "palette").await;
    drv.keys("escape").await.unwrap();
    drv.wait_for("the palette closed", STEP, |d| d.a11y_node("Dialog", Some("Commands")).is_none())
        .await
        .unwrap();

    drv.keys("cmd-,").await.unwrap();
    drv.wait_for("the settings", STEP, |d| d.a11y_node("Dialog", Some("Settings")).is_some())
        .await
        .unwrap();
    golden(drv, &dir, "settings").await;
    drv.keys("escape").await.unwrap();
    drv.wait_for("the settings closed", STEP, |d| {
        d.a11y_node("Dialog", Some("Settings")).is_none()
    })
    .await
    .unwrap();

    stack.set_appearance("dark").unwrap();
    let drv = &mut stack.driver;
    drv.wait_for("the dark theme", STEP, |d| d.dark).await.unwrap();
    golden(drv, &dir, "workspace-dark").await;
    drv.keys("cmd-shift-p").await.unwrap();
    drv.wait_for("the palette", STEP, |d| d.a11y_node("Dialog", Some("Commands")).is_some())
        .await
        .unwrap();
    golden(drv, &dir, "palette-dark").await;
    stack.shutdown().await;
}

/// A worker with nothing open: the strip says how to begin, once.
#[tokio::test]
async fn the_empty_workspace_says_how_to_begin() {
    if !gated() {
        return;
    }
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;
    drv.keys("cmd-w").await.unwrap();
    drv.wait_for("the take-back offer to pass", Duration::from_secs(30), |d| {
        d.items.is_empty() && d.notice.is_none()
    })
    .await
    .unwrap();
    golden(drv, &dir, "empty-workspace").await;
    stack.shutdown().await;
}

/// An agent blocked on a permission: the tile's ring and badge, and the bar's count.
#[tokio::test]
async fn an_agent_that_needs_you_says_so_on_its_tile_and_in_the_bar() {
    if !gated() {
        return;
    }
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    let session = dump.terminals[0].session.clone();
    stack.driver.open(&["cat"], 1).await.unwrap();
    stack.driver.wait_for("a second column", STEP, |d| d.items.len() == 2).await.unwrap();
    stack.play_hook(&session, "PermissionRequest", r#","tool_name":"Bash""#).await.unwrap();
    let drv = &mut stack.driver;
    let dump = drv
        .wait_for("the agent blocked", STEP, |d| {
            d.terminal(&session)
                .is_some_and(|t| t.agent.as_deref() == Some("blocked:permission:Bash"))
                && d.a11y_node("Button", Some("1 needs you")).is_some()
        })
        .await
        .unwrap();
    assert!(dump.a11y_node("Button", Some("1 needs you")).is_some(), "{:#?}", dump.a11y);
    golden(drv, &dir, "agent-needs-you").await;
    stack.shutdown().await;
}

/// A port a shell listens on, and a file on its way up: the chip and the upload on the
/// tiles they belong to.
#[tokio::test]
async fn a_forwarded_port_and_an_upload_show_on_their_tiles() {
    if !gated() {
        return;
    }
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let work = stack.path("drop-here");
    std::fs::create_dir_all(&work).unwrap();
    // Sparse: a file big enough to still be on its way when the frame is drawn, that costs
    // the disk nothing to make.
    let source = stack.path("disk.img");
    std::fs::File::create(&source).unwrap().set_len(2 << 30).unwrap();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let dump = crate::tests::shell_in(drv, &work).await;
    let shell = dump.item("terminal").unwrap().clone();

    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let listen = port.to_string();
    drv.keys("cmd-t").await.unwrap();
    drv.wait_for("a second shell with a prompt", STEP, |d| {
        d.terminals.len() == 2 && d.terminals.iter().all(|t| t.rows.iter().any(|r| !r.is_empty()))
    })
    .await
    .unwrap();
    drv.type_text(&format!("nc -l {listen}")).await.unwrap();
    drv.keys("enter").await.unwrap();
    drv.wait_for("the port forwarded", STEP, |d| {
        d.terminals.iter().any(|t| t.ports.iter().any(|p| p[0] == port && p[1] != 0))
    })
    .await
    .unwrap();
    drv.keys("cmd-alt-left").await.unwrap();
    let dump = drv
        .wait_for("the shell focused", STEP, |d| {
            d.items.iter().any(|i| i.id == shell.id && i.active)
        })
        .await
        .unwrap();
    let tile = dump.items.iter().find(|i| i.id == shell.id).unwrap().bounds;
    drv.drop_files(&[source.as_path()], tile[0] + tile[2] / 2.0, tile[1] + tile[3] / 2.0)
        .await
        .unwrap();
    let dump = drv
        .wait_for("the upload under way", STEP, |d| {
            d.a11y_node("Button", Some("Cancel upload")).is_some()
        })
        .await
        .unwrap();
    assert!(
        dump.a11y.iter().any(|n| n.role == "Link"
            && n.label.as_deref() == Some(&*format!("Open port {port} in the browser"))),
        "{:#?}",
        dump.a11y
    );
    let frame = drv.render(&dir.join("transfers.png")).await.unwrap();
    assert_matches("transfers", &frame, TOLERANCE, &artifacts_dir()).unwrap();
    let cancel = drv.dump().await.unwrap();
    if let Some(node) = cancel.a11y_node("Button", Some("Cancel upload")) {
        let [x, y, w, h] = node.bounds;
        drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
    }
    stack.shutdown().await;
}

/// A remote window before its first frame: the chrome a picture sits in, around the
/// placeholder. The window id names no window, so nothing is captured and nothing needs the
/// grant; a real picture would be this machine's screen, which has no place in a golden.
#[tokio::test]
async fn a_remote_window_waits_in_its_chrome() {
    if !gated() {
        return;
    }
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;
    drv.ok(&Command::PickWindow { window: u32::MAX, title: "Safari".to_owned() }).await.unwrap();
    let dump = drv
        .wait_for("the window tile", STEP, |d| d.item("window").is_some_and(|i| i.active))
        .await
        .unwrap();
    assert!(dump.a11y_node("Heading", Some("window Safari")).is_some(), "{:#?}", dump.a11y);
    golden(drv, &dir, "remote-window").await;
    stack.shutdown().await;
}
