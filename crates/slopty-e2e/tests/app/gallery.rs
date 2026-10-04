//! Every state of the app worth looking at, rendered by the app's own renderer and held as a
//! golden: the first run, the panels that add a worker, a workspace of columns in both themes,
//! the overview, the palette, the settings, the "…" menu, the empty workspace, an agent that
//! needs the human (on its tile and in the navigator), a remote tile, an upload and a forwarded
//! port, and a failed command block; the first run, the navigator and the failed block dark as
//! well, and the navigator once under Increase Contrast.
//!
//! A golden passes or fails on its numbers. The tolerance is blind to a word of chrome text
//! (`docs/decisions/ui.md`), so each scenario also asserts the chrome it shows through the
//! accessibility tree, where a word appearing or going is a string comparison.

use std::time::Duration;

use slopty_e2e::harness::artifacts_dir;
use slopty_e2e::snapshot::{MAC_TOLERANCE as TOLERANCE, assert_matches};
use slopty_e2e::{Command, Driver, Dump, Stack};

/// How long a worker round trip may take.
pub const STEP: Duration = Duration::from_secs(20);
/// The renders' window: the size the other app goldens use.
const WINDOW: (f32, f32) = (900.0, 600.0);
/// Where the pointer rests before a golden: the title bar's top-left corner, left of the window
/// buttons, over nothing that answers a hover. The bottom-left corner is the status bar, which
/// shows the round trip under the pointer.
const PARK: (f32, f32) = (1.0, 1.0);

/// Wait until nothing moves, every link has its round trip and every shell at a prompt has its
/// caret there: two dumps a frame apart place every tile alike, so a spring still running
/// cannot end up in a golden, nor a status bar whose readout is yet to land, nor the block a
/// command ran under.
pub async fn settled(drv: &mut Driver) -> Dump {
    drv.wait_for("the first round trip", STEP, Dump::rtt_sampled).await.unwrap();
    drv.wait_for("the carets at their prompts", STEP, Dump::prompts_settled).await.unwrap();
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
pub async fn golden(drv: &mut Driver, stack_dir: &std::path::Path, name: &str) {
    settled(drv).await;
    let frame = drv.render(&stack_dir.join(format!("{name}.png"))).await.unwrap();
    assert_matches(name, &frame, TOLERANCE, &artifacts_dir()).unwrap();
}

/// The labels of every node with `role`, in tree order.
fn labels(d: &Dump, role: &str) -> Vec<String> {
    d.a11y.iter().filter(|n| n.role == role).filter_map(|n| n.label.clone()).collect()
}

/// The first shell, connected and prompted.
pub async fn first_shell(drv: &mut Driver) -> Dump {
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
#[ignore = "live: cargo xtask e2e app"]
async fn the_first_run_offers_one_way_in() {
    // This Mac's entry is on the page as a user meets it: the stand-in behind it installs
    // nothing, and this case never presses it.
    let report = this_mac_report(true);
    let env = [(slopty_e2e::THIS_MAC_ENV, report.as_str())];
    let mut stack = Stack::launch_first_run_with("e2e-worker", &env).await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    // The look on the tailnet (the harness's empty one) has ended, so the golden does not
    // depend on which side of it the frame landed.
    drv.wait_for("the connect panel, done looking", STEP, |d| {
        d.adding && d.a11y_node("Status", Some("Nothing answered on your tailnet")).is_some()
    })
    .await
    .unwrap();
    golden(drv, &dir, "first-run").await;
    stack.set_appearance("dark").unwrap();
    let drv = &mut stack.driver;
    drv.wait_for("the dark theme", STEP, |d| d.dark).await.unwrap();
    golden(drv, &dir, "first-run-dark").await;
    stack.set_appearance("light").unwrap();
    let drv = &mut stack.driver;
    drv.wait_for("the light theme", STEP, |d| !d.dark).await.unwrap();
    let dump = drv.dump().await.unwrap();
    assert_eq!(labels(&dump, "Heading"), ["Connect to a server"], "{:#?}", dump.a11y);
    // The way in and the other way in; no menu, no tile, no column marks to wonder about.
    let buttons = labels(&dump, "Button");
    for absent in ["Open", "More", "Columns"] {
        assert!(!buttons.iter().any(|b| b == absent), "{absent} on the first run: {buttons:?}");
    }
    assert!(buttons.iter().any(|b| b == "Connect"), "{buttons:?}");
    assert!(buttons.iter().any(|b| b == "Add a machine by address instead"), "{buttons:?}");
    assert!(buttons.iter().any(|b| b == THIS_MAC), "{buttons:?}");

    let switch = dump
        .a11y_node("Button", Some("Add a machine by address instead"))
        .expect("the switch")
        .bounds;
    drv.click(switch[0] + switch[2] / 2.0, switch[1] + switch[3] / 2.0).await.unwrap();
    let dump = drv
        .wait_for("the add-worker panel", STEP, |d| {
            d.a11y_node("Heading", Some("Add a machine")).is_some()
        })
        .await
        .unwrap();
    assert!(labels(&dump, "Button").iter().any(|b| b == "Add"), "{:#?}", dump.a11y);
    // The pointer leaves the link it clicked, so the golden holds the panel at rest and not
    // the link's hover.
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    golden(drv, &dir, "add-worker").await;

    stack.add_worker().await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    assert!(!dump.adding, "the panel goes with the first worker: {dump:#?}");
    stack.shutdown().await;
}

/// The first run's way to make this Mac a worker, and its checklist's heading.
const THIS_MAC: &str = "Use this Mac";

/// This Mac's worker as the stand-in reports it: answering, with Accessibility granted, Screen
/// Recording as `screen_recording` says, and on the tailnet.
fn this_mac_report(screen_recording: bool) -> String {
    let health = slopty_proto::ctl::Health {
        version: "0.1.0".to_owned(),
        exe: "/Applications/Slopty.app/Contents/MacOS/slopty-worker".to_owned(),
        caps: slopty_proto::server::WorkerCaps {
            can_capture: screen_recording,
            can_inject: true,
            ..slopty_proto::server::WorkerCaps::bare(slopty_proto::server::Os::MacOs)
        },
        listen: "[::]:45550".to_owned(),
        allow: Vec::new(),
        tailscale: slopty_proto::ctl::Tailscale::Up {
            node: "studio.tail1234.ts.net".to_owned(),
            ip: Some(std::net::IpAddr::from([100, 64, 0, 3])),
        },
        pasteboard: slopty_proto::ctl::PasteboardAccess::Allowed,
        clients: 0,
        sessions: 0,
        uptime_secs: 1,
    };
    serde_json::to_string(&health).unwrap()
}

/// "Use this Mac as a worker" on the first run turns the page into the worker's own
/// checklist: running, one grant missing with the button to its pane, the other granted, the
/// tailnet reached. The stand-in behind it installs nothing and adds nothing.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn this_mac_walks_its_checklist() {
    let report = this_mac_report(false);
    let env = [(slopty_e2e::THIS_MAC_ENV, report.as_str())];
    let mut stack = Stack::launch_first_run_with("e2e-worker", &env).await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let dump = drv
        .wait_for("the connect panel, done looking", STEP, |d| {
            d.adding && d.a11y_node("Status", Some("Nothing answered on your tailnet")).is_some()
        })
        .await
        .unwrap();
    let entry = dump.a11y_node("Button", Some(THIS_MAC)).expect("this Mac's entry").bounds;
    drv.click(entry[0] + entry[2] / 2.0, entry[1] + entry[3] / 2.0).await.unwrap();
    let dump = drv
        .wait_for("the checklist, read", STEP, |d| {
            d.a11y_node("List", Some(THIS_MAC)).is_some()
                && d.a11y_node("Button", Some("Open settings")).is_some()
        })
        .await
        .unwrap();
    assert_eq!(labels(&dump, "Heading"), [THIS_MAC], "{:#?}", dump.a11y);
    let items = labels(&dump, "ListItem");
    assert_eq!(
        items,
        ["Running", "Screen Recording", "Accessibility", "Reachable on your tailnet"],
        "{:#?}",
        dump.a11y
    );
    let fixes = labels(&dump, "Button").into_iter().filter(|b| b == "Open settings").count();
    assert_eq!(fixes, 1, "one grant missing, one way to it: {:#?}", dump.a11y);
    assert!(dump.adding && dump.workers.is_empty(), "nothing was added: {dump:#?}");
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    golden(drv, &dir, "this-mac").await;
    stack.shutdown().await;
}

/// A file past what a tile edits says so in its body, with the two ways to read it in a
/// terminal on its worker instead.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_file_too_large_to_edit_says_where_to_read_it() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let project = dir.join("project");
    std::fs::create_dir_all(&project).unwrap();
    // Sparse: past the cap, at no cost to the disk.
    let file = project.join("capture.log");
    std::fs::File::create(&file).unwrap().set_len(40 << 20).unwrap();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;
    drv.ok(&Command::OpenFile { path: file.display().to_string(), line: None }).await.unwrap();
    let dump = drv
        .wait_for("the file's notice", STEP, |d| {
            d.a11y_node("Button", Some("Open in editor")).is_some()
        })
        .await
        .unwrap();
    assert!(dump.a11y_node("Button", Some("Open in pager")).is_some(), "{:#?}", dump.a11y);
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    golden(drv, &dir, "file-too-large").await;
    stack.shutdown().await;
}

/// A picture opened as a file shows fitted in its tile, decoded by the platform, and says what
/// it is to a screen reader.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_picture_shows_fitted_in_its_tile() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let project = dir.join("project");
    std::fs::create_dir_all(&project).unwrap();
    // A wide picture in soft bands, larger than the tile, with no extension to go by.
    let picture = image::RgbImage::from_fn(1600, 900, |x, y| {
        let band = |v: u32, of: u32| u8::try_from(60 + v * 150 / of).unwrap_or(u8::MAX);
        image::Rgb([band(x, 1600), band(y, 900), band(1600 - x, 1600)])
    });
    let file = project.join("screenshot");
    picture.save_with_format(&file, image::ImageFormat::Png).unwrap();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;
    drv.ok(&Command::OpenFile { path: file.display().to_string(), line: None }).await.unwrap();
    let dump = drv
        .wait_for("the picture, decoded", STEP, |d| {
            labels(d, "Image").iter().any(|l| l.starts_with("image/png, 1600 × 900"))
        })
        .await
        .unwrap();
    assert!(dump.a11y_node("Status", None).is_none_or(|n| n.label.as_deref() != Some("Reading…")));
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    golden(drv, &dir, "file-picture").await;
    stack.shutdown().await;
}

/// A PDF opened as a file shows its pages, each as wide as the tile, drawn by the platform.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_pdf_shows_its_pages() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let project = dir.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let text = (0..40_u32).fold(String::new(), |mut text, line| {
        let y = 720_u32.saturating_sub(line.saturating_mul(16));
        text.push_str("BT /F1 11 Tf 72 ");
        text.push_str(&y.to_string());
        text.push_str(" Td (Line ");
        text.push_str(&line.to_string());
        text.push_str(" of the report, set in Helvetica.) Tj ET ");
        text
    });
    let heading =
        "BT /F1 24 Tf 72 760 Td (Quarterly report) Tj ET q 0.2 0.5 0.3 rg 72 744 468 3 re f Q";
    let file = project.join("report.pdf");
    std::fs::write(&file, pdf(&[&format!("{heading} {text}"), &text])).unwrap();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;
    drv.ok(&Command::OpenFile { path: file.display().to_string(), line: None }).await.unwrap();
    drv.wait_for("the first page", STEP, |d| labels(d, "Image").iter().any(|l| l == "Page 1 of 2"))
        .await
        .unwrap();
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    golden(drv, &dir, "file-pdf").await;
    stack.shutdown().await;
}

/// A PDF of letter pages, one content stream each, with Helvetica as `F1`.
#[expect(clippy::arithmetic_side_effects, reason = "a fixture's small object numbers and offsets")]
fn pdf(pages: &[&str]) -> Vec<u8> {
    let kids: Vec<String> = (0..pages.len()).map(|n| format!("{} 0 R", 3 + 2 * n)).collect();
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len()),
    ];
    for (n, content) in pages.iter().enumerate() {
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 \
             << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >> /Contents {} 0 R >>",
            4 + 2 * n
        ));
        objects.push(format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len() + 1));
    }
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (n, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", n + 1).as_bytes());
    }
    let xref = out.len();
    let count = objects.len() + 1;
    out.extend_from_slice(format!("xref\n0 {count}\n0000000000 65535 f \n").as_bytes());
    for at in offsets {
        out.extend_from_slice(format!("{at:010} 00000 n \n").as_bytes());
    }
    let trailer = format!("trailer\n<< /Size {count} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n");
    out.extend_from_slice(trailer.as_bytes());
    out
}

/// Three columns with the shell focused, the overview, the palette and the settings, then
/// the same workspace dark.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_workspace_of_columns_in_both_themes() {
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
        d.items.len() == 3 && d.a11y_node("Heading", Some("note Release")).is_some()
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
    assert_eq!(dump.workspace, "e2e-worker", "a shell at home leaves the worker's name");
    golden(drv, &dir, "workspace").await;

    // A desktop-sized window leaves the strip its room, so the navigator docks beside it.
    drv.ok(&Command::Resize { width: 1280.0, height: 800.0 }).await.unwrap();
    drv.wait_for("the navigator docked", STEP, |d| {
        d.a11y_node("Navigation", Some("Navigator")).is_some()
    })
    .await
    .unwrap();
    golden(drv, &dir, "workspace-navigator").await;
    // Increase Contrast as the app's own setting stands it in: this Mac's is never touched.
    drv.ok(&Command::Contrast { increased: true }).await.unwrap();
    golden(drv, &dir, "workspace-navigator-contrast").await;
    drv.ok(&Command::Contrast { increased: false }).await.unwrap();
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

    let dump = drv.dump().await.unwrap();
    let [x, y, w, h] = dump.a11y_node("Button", Some("More")).expect("the menu button").bounds;
    drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
    drv.wait_for("the menu", STEP, |d| d.a11y_node("Menu", None).is_some()).await.unwrap();
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    golden(drv, &dir, "more-menu").await;
    drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
    drv.wait_for("the menu closed", STEP, |d| d.a11y_node("Menu", None).is_none()).await.unwrap();
    // Off "…", so the shot shows the bar at rest rather than the button under the pointer.
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();

    stack.set_appearance("dark").unwrap();
    let drv = &mut stack.driver;
    drv.wait_for("the dark theme", STEP, |d| d.dark).await.unwrap();
    golden(drv, &dir, "workspace-dark").await;
    drv.ok(&Command::Resize { width: 1280.0, height: 800.0 }).await.unwrap();
    drv.wait_for("the navigator docked", STEP, |d| {
        d.a11y_node("Navigation", Some("Navigator")).is_some()
    })
    .await
    .unwrap();
    golden(drv, &dir, "workspace-navigator-dark").await;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    drv.wait_for("the navigator gone again", STEP, |d| {
        d.a11y_node("Navigation", Some("Navigator")).is_none()
    })
    .await
    .unwrap();
    drv.keys("cmd-shift-p").await.unwrap();
    drv.wait_for("the palette", STEP, |d| d.a11y_node("Dialog", Some("Commands")).is_some())
        .await
        .unwrap();
    golden(drv, &dir, "palette-dark").await;
    drv.keys("escape").await.unwrap();
    drv.wait_for("the palette closed", STEP, |d| d.a11y_node("Dialog", Some("Commands")).is_none())
        .await
        .unwrap();
    drv.keys("cmd-alt-o").await.unwrap();
    drv.wait_for("the overview", STEP, |d| d.overview).await.unwrap();
    golden(drv, &dir, "overview-dark").await;
    drv.keys("cmd-alt-o").await.unwrap();
    drv.wait_for("the overview closed", STEP, |d| !d.overview).await.unwrap();
    drv.keys("cmd-,").await.unwrap();
    drv.wait_for("the settings", STEP, |d| d.a11y_node("Dialog", Some("Settings")).is_some())
        .await
        .unwrap();
    golden(drv, &dir, "settings-dark").await;
    stack.shutdown().await;
}

/// A worker with nothing open: the strip says how to begin, once.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_empty_workspace_says_how_to_begin() {
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

/// The blocked agent's thread holds what it asks: the tray naming the tool, with the way to
/// answer in the terminal. The worker puts it there a grace after the hook, so a golden taken
/// before it would hold a thread that has not caught up.
fn asked_in_its_thread(d: &Dump) -> bool {
    d.a11y_node("Dialog", Some("Bash")).is_some()
        && d.a11y_node("Button", Some("Answer in the terminal")).is_some()
}

/// The title the second agent's TUI gives itself, as Claude Code titles a session by its task.
const TITLED_AGENT: &str = "Fix the login redirect";

/// An agent blocked on a permission beside a second one at rest that has titled itself: the
/// waiting tile's pill and the bar's count, each agent named once, the second by its own
/// title rather than "Claude Code 2".
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn an_agent_that_needs_you_says_so_on_its_tile_and_in_the_bar() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    let session = dump.terminals[0].session.clone();
    // The second program sets its own title (OSC 0) and then waits, as a TUI would.
    let titled = format!("printf '\\033]0;{TITLED_AGENT}\\007'; exec cat");
    stack.driver.open(&["sh", "-c", &titled], 1).await.unwrap();
    let dump =
        stack.driver.wait_for("a second column", STEP, |d| d.terminals.len() == 2).await.unwrap();
    let second = dump
        .terminals
        .iter()
        .find(|t| t.session != session)
        .map(|t| t.session.clone())
        .expect("the second shell");
    stack.play_hook(&second, "SessionStart", r#","source":"startup""#).await.unwrap();
    let heading = format!("terminal {TITLED_AGENT}");
    stack
        .driver
        .wait_for("the second agent, titled", STEP, |d| {
            d.terminal(&second).is_some_and(|t| t.agent.as_deref() == Some("idle"))
                && d.a11y_node("Heading", Some(&heading)).is_some()
        })
        .await
        .unwrap();
    stack.play_hook(&session, "PermissionRequest", r#","tool_name":"Bash""#).await.unwrap();
    let drv = &mut stack.driver;
    let dump = drv
        .wait_for("the agent blocked", STEP, |d| {
            d.terminal(&session)
                .is_some_and(|t| t.agent.as_deref() == Some("blocked:permission:Bash"))
                && d.a11y_node("Status", Some("1 new")).is_some()
                && asked_in_its_thread(d)
        })
        .await
        .unwrap();
    // The bell's badge is the one count of what waits.
    assert!(dump.a11y_node("Status", Some("1 new")).is_some(), "{:#?}", dump.a11y);
    golden(drv, &dir, "agent-needs-you").await;
    // The navigator names the waiting agent in its row, beside the tile's own mark.
    drv.ok(&Command::Resize { width: 1280.0, height: 800.0 }).await.unwrap();
    drv.wait_for("the navigator docked", STEP, |d| {
        d.a11y_node("Navigation", Some("Navigator")).is_some()
    })
    .await
    .unwrap();
    golden(drv, &dir, "agent-needs-you-navigator").await;
    stack.shutdown().await;
}

/// An agent whose turn ended with a background command still running says what it waits on,
/// not that it needs the person; and its status line's pull request and worktree ride on its
/// header: the request's number toned by its review, a click from its page, and the
/// worktree's name beside it. Played through the worker's control socket and the real relay.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_paused_agent_says_what_it_waits_on_and_wears_its_pull_request() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    let session = dump.terminals[0].session.clone();
    stack.play_hook(&session, "SessionStart", r#","source":"startup""#).await.unwrap();
    let running = r#","background_tasks":[{"id":"b1","type":"shell","status":"running","description":"cargo test"}],"session_crons":[]"#;
    stack.play_hook(&session, "Stop", running).await.unwrap();
    stack
        .driver
        .wait_for("the agent waiting on its task", STEP, |d| {
            d.terminal(&session).is_some_and(|t| t.agent.as_deref() == Some("waiting:1:0"))
                && d.a11y.iter().any(|n| n.label.as_deref() == Some("Waiting on cargo test"))
        })
        .await
        .unwrap();
    let status = serde_json::json!({
        "session_id": slopty_e2e::harness::agent_session(&session),
        "transcript_path": stack.transcript_path(),
        "model": { "id": "claude-opus-5-5", "display_name": "Opus 5.5" },
        "pr": {
            "number": 1234,
            "url": "https://github.com/aislopware/slopty/pull/1234",
            "review_state": "approved",
        },
        "worktree": {
            "name": "fix-build",
            "path": stack.path("home").join(".claude/worktrees/fix-build"),
            "branch": "worktree-fix-build",
            "original_cwd": stack.path("home"),
            "original_branch": "main",
        },
    });
    let line = stack.relay_hook(&session, &["statusline", "--command", "true"], &status).unwrap();
    assert!(line.wait_with_output().await.unwrap().status.success(), "the status line ran");
    let drv = &mut stack.driver;
    let dump = drv
        .wait_for("the pull request on the header", STEP, |d| {
            d.a11y_node("Link", Some("Pull request 1234, approved")).is_some()
                && d.a11y_node("Label", Some("Worktree fix-build on worktree-fix-build")).is_some()
        })
        .await
        .unwrap();
    assert!(
        !dump.a11y.iter().any(|n| n.label.as_deref() == Some("1 new")),
        "a paused turn asks nothing of the person: {:#?}",
        dump.a11y
    );
    golden(drv, &dir, "agent-waiting-pull-request").await;
    stack.shutdown().await;
}

/// A port a shell listens on, and a file on its way up: the port counted in the status bar,
/// the upload on the tile it belongs to.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_forwarded_port_and_an_upload_show_where_they_belong() {
    // A home of the run's own: the shells name the directory under it from `~`, so no
    // temporary path of the machine's is in the render.
    let mut stack = Stack::launch_at_home("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    std::fs::create_dir_all(stack.path("home").join("drop-here")).unwrap();
    // Sparse: a file big enough to still be on its way when the frame is drawn, that costs
    // the disk nothing to make.
    let source = stack.path("disk.img");
    std::fs::File::create(&source).unwrap().set_len(2 << 30).unwrap();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;
    drv.type_text("cd drop-here").await.unwrap();
    drv.keys("enter").await.unwrap();
    let dump = drv
        .wait_for("the shell in the drop directory", STEP, |d| {
            d.rows_containing("~/drop-here %").len() == 1
        })
        .await
        .unwrap();
    let shell = dump.item("terminal").unwrap().clone();

    let port = steady_port();
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
    assert!(dump.a11y_node("Button", Some("1 port")).is_some(), "{:#?}", dump.a11y);
    let frame = drv.render(&dir.join("transfers.png")).await.unwrap();
    assert_matches("transfers", &frame, TOLERANCE, &artifacts_dir()).unwrap();
    let cancel = drv.dump().await.unwrap();
    if let Some(node) = cancel.a11y_node("Button", Some("Cancel upload")) {
        let [x, y, w, h] = node.bounds;
        drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
    }
    stack.shutdown().await;
}

/// The first port of a fixed run that is free here, with the one after it: the app forwards a
/// port its own machine holds (as this one's `nc` does) on the next free one, and a golden
/// shows both. Free, as it is on a quiet machine, it is the same number every run.
fn steady_port() -> u16 {
    let free = |port: u16| {
        ["127.0.0.1", "0.0.0.0"].iter().all(|ip| std::net::TcpListener::bind((*ip, port)).is_ok())
    };
    (47_310_u16..47_410)
        .step_by(10)
        .find(|p| free(*p) && free(p.saturating_add(1)))
        .expect("a free pair of ports in 47310..47410")
}

/// A remote window before its first frame: the chrome a picture sits in, around the
/// placeholder. The window id names no window, so nothing is captured and nothing needs the
/// grant; a real picture would be this machine's screen, which has no place in a golden.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_remote_window_waits_in_its_chrome() {
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

/// A command that failed, under the pointer: its block wears the error bar down the left edge
/// and the faint wash, and says how it ended and how long it ran, with "…" for its menu. The
/// harness's driver types a command that succeeds and one that fails; the pointer rests on the
/// failed one's output.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_failed_command_block_says_so_under_the_pointer() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(&mut stack.driver).await;
    let drv = &mut stack.driver;
    drv.type_text("echo fine").await.unwrap();
    drv.keys("enter").await.unwrap();
    drv.type_text("ls /e2e-missing").await.unwrap();
    drv.keys("enter").await.unwrap();
    let failure = "No such file or directory";
    let dump = drv
        .wait_for("the failure and the prompt after it", STEP, |d| {
            d.terminals.first().is_some_and(|t| {
                let at = t.rows.iter().position(|r| r.contains(failure));
                at.is_some_and(|at| usize::from(t.cursor[1]) > at)
            })
        })
        .await
        .unwrap();
    let term = &dump.terminals[0];
    let row = term.rows.iter().position(|r| r.contains(failure)).unwrap();
    let [x, y, w, h] = dump.a11y_node("Terminal", None).expect("the grid").bounds;
    // The grid's rows share its height inside the inset (12 pt a side): the middle of the
    // failure's row, clear of the text.
    let line = (h - 24.0) / f32::from(term.size[1]);
    let at = line.mul_add(f32::from(u16::try_from(row).unwrap()) + 0.5, y + 12.0);
    drv.ok(&Command::Move { x: w.mul_add(0.6, x), y: at }).await.unwrap();
    let dump = drv
        .wait_for("the block's facts", STEP, |d| {
            d.a11y_node("Button", Some("Block actions")).is_some()
        })
        .await
        .unwrap();
    assert!(dump.a11y_node("Button", Some("Block actions")).is_some(), "{:#?}", dump.a11y);
    golden(drv, &dir, "terminal-failed-block").await;
    stack.set_appearance("dark").unwrap();
    let drv = &mut stack.driver;
    drv.wait_for("the dark block", STEP, |d| {
        d.dark && d.a11y_node("Button", Some("Block actions")).is_some()
    })
    .await
    .unwrap();
    golden(drv, &dir, "terminal-failed-block-dark").await;
    stack.shutdown().await;
}

/// Two shells in one tabbed column: the header is a tab row, a tab per shell with its slot and
/// its title, the shown one on its body's surface.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_tabbed_column_draws_its_tab_row() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;
    drv.open(&["cat"], 1).await.unwrap();
    drv.wait_for("a second column", STEP, |d| d.items.len() == 2).await.unwrap();
    drv.keys("cmd-[").await.unwrap();
    drv.keys("cmd-alt-t").await.unwrap();
    let dump = drv
        .wait_for("one tabbed column", STEP, |d| {
            d.items.len() == 2
                && d.items.iter().all(|i| i.pos[1] == 0)
                && labels(d, "Tab").len() == 2
        })
        .await
        .unwrap();
    assert!(dump.a11y.iter().all(|n| n.label.as_deref() != Some("2/2")), "{:#?}", dump.a11y);
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    golden(drv, &dir, "tabbed-column").await;
    stack.shutdown().await;
}
