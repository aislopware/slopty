//! Every state of the app worth looking at, rendered by the app's own renderer and held as a
//! golden: the first run, the panels that add a worker, a workspace of panes in both themes,
//! the palette, the settings, the "…" menu, the empty workspace, an agent that
//! needs the human (on its tile and in the navigator), a remote tile, an upload and a forwarded
//! port, a failed command block, and a tile kept nowhere whose worker is away after a relaunch;
//! the first run, the navigator, the failed block and the away tile dark as well.
//!
//! A golden passes or fails on its numbers. The tolerance is blind to a word of chrome text
//! (`docs/decisions/ui.md`), so each scenario also asserts the chrome it shows through the
//! accessibility tree, where a word appearing or going is a string comparison.

use std::time::Duration;

use slopty_e2e::harness::artifacts_dir;
use slopty_e2e::snapshot::{
    MAC_TOLERANCE as TOLERANCE, PixelRect, assert_matches, assert_matches_masked,
};
use slopty_e2e::{Command, Driver, Dump, Stack};

/// How long a worker round trip may take.
pub const STEP: Duration = Duration::from_secs(20);
/// The renders' window: the size the other app goldens use.
const WINDOW: (f32, f32) = (900.0, 600.0);
/// Where the pointer rests before a golden: the title bar's top-left corner, left of the window
/// buttons, over nothing that answers a hover.
pub const PARK: (f32, f32) = (1.0, 1.0);
/// Where a golden that shows a time pins the readouts ([`Command::PinClock`]): 2026-10-04 at
/// 09:00 UTC, in Unix milliseconds.
pub const PINNED_AT: u64 = 1_791_104_400_000;

/// Wait until nothing moves, every link has its round trip and every shell at a prompt has its
/// caret there: two dumps a frame apart place every tile alike, so a spring still running
/// cannot end up in a golden, nor a readout yet to land, nor the block a
/// command ran under.
pub async fn settled(drv: &mut Driver) -> Dump {
    drv.wait_for("the first round trip", STEP, Dump::rtt_sampled).await.unwrap();
    drv.wait_for("the carets at their prompts", STEP, Dump::prompts_settled).await.unwrap();
    at_rest(drv).await
}

/// Wait until nothing moves: two dumps a frame apart place every tile alike.
async fn at_rest(drv: &mut Driver) -> Dump {
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

/// [`golden`] drawn at `scale` device pixels to the point whatever display the window is on:
/// the Retina picture, which a 1x runner draws offscreen afresh.
pub async fn golden_at(drv: &mut Driver, stack_dir: &std::path::Path, name: &str, scale: f32) {
    settled(drv).await;
    let frame = drv.render_at(&stack_dir.join(format!("{name}.png")), scale).await.unwrap();
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

/// The first run asks where to work: this Mac, or a server that lists machines already, and
/// nothing else on the screen is there to choose from. Once connected, its workers come, and
/// "Add a machine" installs one rather than taking an address, keeps this Mac's row as the way
/// back to its checklist, and offers a phone's way in.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_first_run_offers_one_way_in() {
    // This Mac's entry is on the page as a user meets it: the stand-in behind it installs
    // nothing, and this case never presses it.
    let mut stack = Stack::launch_first_run_with("e2e-worker", |server, root| {
        vec![(slopty_e2e::THIS_MAC_ENV.to_owned(), this_mac_report(true, server, root))]
    })
    .await
    .unwrap();
    let dir = stack.dir.path().to_path_buf();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    // The look on the tailnet (the harness's empty one) has ended, so the golden does not
    // depend on which side of it the frame landed.
    drv.wait_for("the connect panel, done looking", STEP, |d| {
        d.adding && d.a11y_node("Status", Some("No Slopty server found yet")).is_some()
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
    assert_eq!(
        labels(&dump, "Heading"),
        ["Choose where to work", "Connect to an existing server"],
        "{:#?}",
        dump.a11y
    );
    // The way in and the other way in; no menu and no tile to wonder about.
    let buttons = labels(&dump, "Button");
    for absent in ["Open", "More"] {
        assert!(!buttons.iter().any(|b| b == absent), "{absent} on the first run: {buttons:?}");
    }
    assert!(buttons.iter().any(|b| b == "Connect"), "{buttons:?}");
    assert!(buttons.iter().any(|b| b == THIS_MAC), "{buttons:?}");
    assert!(!buttons.iter().any(|b| b.contains("by address")), "{buttons:?}");

    stack.connect_server().await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    assert!(!dump.adding, "the page goes once the server's worker connects: {dump:#?}");

    let drv = &mut stack.driver;
    drv.keys("cmd-shift-h").await.unwrap();
    let dump = drv
        .wait_for("the add-a-machine dialog, done looking", STEP, |d| {
            d.a11y_node("Heading", Some("Add a machine")).is_some()
                && d.a11y_node("Status", Some("No machine found yet")).is_some()
        })
        .await
        .unwrap();
    let buttons = labels(&dump, "Button");
    assert!(!buttons.iter().any(|b| b == "Connect" || b == "Add"), "no address: {buttons:?}");
    assert!(buttons.iter().any(|b| b == THIS_MAC), "this Mac's checklist, again: {buttons:?}");
    assert!(buttons.iter().any(|b| b == "Connect a phone or iPad"), "{buttons:?}");
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    golden(drv, &dir, "add-worker").await;
    stack.shutdown().await;
}

/// The first run's way to make this Mac a worker, and its checklist's heading.
const THIS_MAC: &str = "Use this Mac";

/// This Mac's worker as the stand-in reports it: the run's worker under `root`, answering,
/// linked to `server`, with Accessibility granted, Screen Recording as `screen_recording`
/// says, and on the tailnet. A server "started here" is `server`.
fn this_mac_report(
    screen_recording: bool,
    server: &slopty_e2e::harness::ServerDaemon,
    root: &std::path::Path,
) -> String {
    let worker = slopty_e2e::harness::worker_id(root).and_then(|id| id.parse().ok());
    let health = slopty_proto::ctl::Health {
        worker: worker.unwrap_or_default(),
        server: Some(slopty_proto::ctl::ServerHealth {
            address: server.address().to_owned(),
            link: slopty_proto::ctl::LinkState::Linked,
        }),
        exe: "/Applications/Slopty.app/Contents/MacOS/slopty-worker".to_owned(),
        caps: slopty_proto::server::WorkerCaps {
            can_capture: screen_recording,
            can_inject: true,
            build: "0.1.0".to_owned(),
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
        turns: 0,
        uptime_secs: 1,
    };
    serde_json::to_string(&health).unwrap()
}

/// "Use this Mac" on the first run, with no server answering on the tailnet, starts one here
/// and turns the page into the worker's own checklist: running, the server linked, one grant
/// missing with the button to its pane, the other granted, the tailnet reached; then the app's
/// own lines, notifications allowed, not yet opening at login, and a build with no Finder
/// extension. The stand-in behind it installs nothing and registers no login item: the server
/// "started here" is the run's own.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn this_mac_walks_its_checklist() {
    let mut stack = Stack::launch_first_run_with("e2e-worker", |server, root| {
        vec![(slopty_e2e::THIS_MAC_ENV.to_owned(), this_mac_report(false, server, root))]
    })
    .await
    .unwrap();
    let dir = stack.dir.path().to_path_buf();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let dump = drv
        .wait_for("the connect panel, done looking", STEP, |d| {
            d.adding && d.a11y_node("Status", Some("No Slopty server found yet")).is_some()
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
        [
            "Running",
            "Server",
            "Screen Recording",
            "Accessibility",
            "Reachable on your tailnet",
            "Notes on your phone",
            "Notifications",
            "Open at login",
            "Finder",
        ],
        "{:#?}",
        dump.a11y
    );
    let fixes = labels(&dump, "Button").into_iter().filter(|b| b == "Open settings").count();
    assert_eq!(fixes, 1, "one grant missing, one way to it: {:#?}", dump.a11y);
    assert!(dump.adding, "the checklist waits on the grant: {dump:#?}");
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

/// `git` in `repo`, as Mira, which must succeed.
fn git(repo: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=Mira", "-c", "user.email=mira@localhost"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// A shell in `project`, then "Review changes" from the palette: the review of its working
/// tree, in a tile of its own.
async fn review_changes(drv: &mut Driver, project: &std::path::Path) {
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;
    drv.type_text(&format!("cd '{}' && pwd", project.display())).await.unwrap();
    drv.keys("enter").await.unwrap();
    drv.wait_for("the shell in the project", STEP, |d| {
        d.lines_containing("project").iter().any(|r| r.trim().ends_with("/project"))
    })
    .await
    .unwrap();
    drv.keys("cmd-shift-p").await.unwrap();
    drv.wait_for("the palette", STEP, |d| d.a11y_node("Dialog", Some("Commands")).is_some())
        .await
        .unwrap();
    drv.type_text("Review changes").await.unwrap();
    drv.keys("enter").await.unwrap();
}

/// A folder's changes that are more than lines, reviewed with "Review changes": a picture
/// redrawn shows its two sides beside each other, a file moved in its folder says where it was,
/// a script made executable says so in its head, and a file too large to cut says its size with
/// the press that opens it whole.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_review_shows_pictures_moves_modes_and_large_files_for_what_they_are() {
    use std::os::unix::fs::PermissionsExt as _;

    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let project = dir.join("project");
    std::fs::create_dir_all(project.join("art")).unwrap();
    let mark = |hue: [u8; 3]| {
        image::RgbImage::from_fn(96, 64, move |x, y| {
            let edge = x < 8 || y < 8 || x >= 88 || y >= 56;
            image::Rgb(if edge { [236, 236, 232] } else { hue })
        })
    };
    let logo = project.join("art/logo.png");
    mark([64, 150, 96]).save_with_format(&logo, image::ImageFormat::Png).unwrap();
    std::fs::write(project.join("art/credits.txt"), "Drawn by Mira\n").unwrap();
    std::fs::write(project.join("deploy.sh"), "#!/bin/sh\ncargo xtask deploy\n").unwrap();
    git(&project, &["init", "-q", "-b", "main"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "the mark"]);
    // The changes: the mark redrawn, the credits renamed, the script made executable, and a
    // log past what a review cuts into hunks.
    mark([70, 110, 190]).save_with_format(&logo, image::ImageFormat::Png).unwrap();
    // Another name, not another case of it: a Mac's disk takes `CREDITS.txt` for the same file.
    std::fs::rename(project.join("art/credits.txt"), project.join("art/authors.txt")).unwrap();
    let script = project.join("deploy.sh");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(project.join("capture.log"), "frame 0042 presented\n".repeat(220_000)).unwrap();

    let drv = &mut stack.driver;
    review_changes(drv, &project).await;
    let dump = drv
        .wait_for("the review of the folder's changes", STEP, |d| {
            d.a11y_node("Image", Some("After: art/logo.png")).is_some()
        })
        .await
        .unwrap();
    assert!(dump.a11y_node("Image", Some("Before: art/logo.png")).is_some(), "{:#?}", dump.a11y);
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    golden(drv, &dir, "review-sides").await;
    stack.shutdown().await;
}

/// The forge's review of the branch's pull request, on the lines it is about: a thread still
/// open hangs under its line with its reply, and one on code changed since waits at the file's
/// end saying so, each with the way to its page. `gh` is a stand-in on the worker's `PATH`
/// that answers the pull request and its threads in gh's own words; no forge is asked.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_review_hangs_the_forges_threads_on_their_lines() {
    use std::os::unix::fs::PermissionsExt as _;

    let programs = tempfile::tempdir().unwrap();
    let viewed = serde_json::json!({
        "number": 42,
        "url": "https://github.com/aislopware/slopty/pull/42",
        "title": "Retry the refresh with one key",
        "state": "OPEN",
        "isDraft": false,
        "headRefName": "feature",
        "headRefOid": "0123abcd",
        "baseRefName": "main",
        "reviewDecision": "CHANGES_REQUESTED",
        "mergeable": "MERGEABLE",
        "mergeStateStatus": "BLOCKED",
        "statusCheckRollup": [],
    });
    let note = |login: &str, body: &str, at: u32| {
        serde_json::json!({
            "author": { "login": login }, "body": body,
            "url": format!("https://github.com/aislopware/slopty/pull/42#discussion_r{at}"),
        })
    };
    let thread = |line: u32, outdated: bool, notes: Vec<serde_json::Value>| {
        serde_json::json!({
            "isResolved": false, "isOutdated": outdated, "path": "src/refresh.rs", "line": line,
            "comments": { "totalCount": notes.len(), "nodes": notes },
        })
    };
    let threads = serde_json::json!({ "data": { "repository": { "pullRequest": {
        "reviews": { "nodes": [{
            "author": { "login": "ana" }, "body": "Close. The retry needs a pause between tries.",
            "state": "CHANGES_REQUESTED",
            "url": "https://github.com/aislopware/slopty/pull/42#pullrequestreview-1",
        }] },
        "reviewThreads": { "totalCount": 2, "nodes": [
            thread(3, false, vec![
                note("ana", "Three tries with no pause between them will meet the rate limit.", 1),
                note("mira", "Agreed: a jittered backoff, capped at two seconds.", 2),
            ]),
            thread(2, true, vec![note("lin", "Name the key for what it guards.", 3)]),
        ] },
    } } } });
    std::fs::write(programs.path().join("view.json"), viewed.to_string()).unwrap();
    std::fs::write(programs.path().join("threads.json"), threads.to_string()).unwrap();
    let gh = programs.path().join("gh");
    let script = format!(
        "#!/bin/sh\ncase \"$1 $2\" in\n\
         'pr view') cat '{dir}/view.json' ;;\n\
         'api graphql') cat '{dir}/threads.json' ;;\n\
         *) echo \"unexpected: $*\" >&2; exit 2 ;;\n\
         esac\n",
        dir = programs.path().display()
    );
    std::fs::write(&gh, script).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", programs.path().display());
    let mut stack = Stack::launch_with("e2e-worker", &[("PATH", &path)]).await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let project = dir.join("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    let before = "pub fn refresh(client: &Client, token: &Token) -> Result<Token> {\n    \
                  let fresh = client.post(\"/refresh\", token)?;\n    Ok(fresh)\n}\n";
    std::fs::write(project.join("src/refresh.rs"), before).unwrap();
    git(&project, &["init", "-q", "-b", "feature"]);
    git(&project, &["remote", "add", "origin", "https://github.com/aislopware/slopty.git"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "refresh tokens"]);
    let after = "pub fn refresh(client: &Client, token: &Token) -> Result<Token> {\n    \
                 let key = IdempotencyKey::new();\n    \
                 let fresh = retry(3, || client.post_with(\"/refresh\", token, &key))?;\n    \
                 Ok(fresh)\n}\n";
    std::fs::write(project.join("src/refresh.rs"), after).unwrap();

    let drv = &mut stack.driver;
    review_changes(drv, &project).await;
    let said = "ana: Three tries with no pause between them will meet the rate limit.";
    let dump = drv
        .wait_for("the forge's threads on the diff", STEP, |d| {
            d.a11y_node("Comment", Some(said)).is_some()
        })
        .await
        .unwrap();
    let outdated = dump.a11y_node("Comment", Some("lin: Name the key for what it guards."));
    assert!(outdated.is_some(), "the thread on code changed since: {:#?}", dump.a11y);
    assert_eq!(labels(&dump, "Link").iter().filter(|l| *l == "Open on the forge").count(), 2);
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    golden(drv, &dir, "review-forge").await;
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

/// Two panes, the shell focused among its pane's tabs, the palette and the settings, then
/// the same workspace dark.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_workspace_of_panes_in_both_themes() {
    let mut stack = Stack::launch_at_home("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let home = std::path::PathBuf::from(stack.home().unwrap());
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;

    // A Markdown checklist at home, opened on its preview: like the shell, it names no folder.
    let release = home.join("RELEASE.md");
    let checklist = "# Release\n\n- [x] build the bundle\n- [ ] notarise\n- [ ] tag\n";
    std::fs::write(&release, checklist).unwrap();
    drv.open_file(&release.display().to_string(), None).await.unwrap();
    drv.wait_for("the checklist, previewed", STEP, |d| {
        d.item("file").and_then(|i| i.file.as_ref()).is_some_and(|f| f.previewing)
    })
    .await
    .unwrap();
    drv.open(&["cat"], 1).await.unwrap();
    drv.wait_for("a third tile", STEP, |d| d.items.len() == 3).await.unwrap();
    // No room at this size for a pane beside the shell, so all three are its pane's tabs:
    // ⌥⇧⌘→ moves `cat` out to a pane of its own on the right, ⌥⌘← goes back to the left pane,
    // which shows the checklist, and ⌥⌘[ its tab before, the shell.
    drv.keys("cmd-alt-shift-right").await.unwrap();
    drv.wait_for("two panes", STEP, |d| d.panes_on_show() == 2).await.unwrap();
    drv.keys("cmd-alt-left").await.unwrap();
    drv.keys("cmd-alt-[").await.unwrap();
    let dump = drv
        .wait_for("the first shell focused", STEP, |d| {
            d.items
                .iter()
                .any(|i| i.active && i.kind == "terminal" && i.pane == [0] && i.bounds[2] > 0.0)
        })
        .await
        .unwrap();
    assert_eq!(dump.project, "e2e-worker", "a shell at home leaves the worker's name");
    golden(drv, &dir, "workspace").await;

    // A desktop-sized window leaves the panes their room, so the navigator docks beside them.
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
    drv.wait_for("the settings", STEP, |d| d.a11y_node("Group", Some("Settings")).is_some())
        .await
        .unwrap();
    golden(drv, &dir, "settings").await;
    drv.keys("escape").await.unwrap();
    drv.wait_for("the settings closed", STEP, |d| d.a11y_node("Group", Some("Settings")).is_none())
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
    drv.keys("cmd-,").await.unwrap();
    drv.wait_for("the settings", STEP, |d| d.a11y_node("Group", Some("Settings")).is_some())
        .await
        .unwrap();
    golden(drv, &dir, "settings-dark").await;
    stack.shutdown().await;
}

/// A morning after an update: the app relaunched with its worker down and none of its items
/// kept on this device (an older build's cache does not read). The tile still stands where it
/// was, with the worker's name and the pill saying it is reconnecting, never a gap. In both
/// themes.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_tile_kept_nowhere_says_its_worker_is_away() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let app_dir = dir.join("app");
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let shell = first_shell(&mut stack.driver).await;
    let tile = shell.item("terminal").expect("the shell's tile").id.clone();
    // The layout is written a moment after the shell's tile lands, and the items with it.
    let laid_out = || {
        std::fs::read_to_string(app_dir.join("layout.json")).is_ok_and(|l| l.contains(&tile))
            && app_dir.join("items").exists()
    };
    let deadline = tokio::time::Instant::now() + STEP;
    while !laid_out() {
        assert!(tokio::time::Instant::now() < deadline, "the layout and the items written");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    stack.kill_app().await.unwrap();
    stack.kill_worker().await.unwrap();
    std::fs::remove_dir_all(app_dir.join("items")).unwrap();
    // The app shows the server's word once the server has noticed, and its own dial's before:
    // which one the frame holds was a race, lost on a slow runner (CI e2e run 37359580819).
    // The server is waited for, as a worker that died a while ago is met.
    stack.server_lists_worker("unreachable", STEP).await.unwrap();
    stack.relaunch_app_unlinked().await.unwrap();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let away = |d: &Dump| {
        d.a11y_node("Group", Some("e2e-worker")).is_some()
            && d.a11y_node("Status", Some("e2e-worker is unreachable")).is_some()
    };
    drv.wait_for("the tile standing with its worker away", STEP, away).await.unwrap();
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    at_rest(drv).await;
    let frame = drv.render(&dir.join("tile-away.png")).await.unwrap();
    assert_matches("tile-away", &frame, TOLERANCE, &artifacts_dir()).unwrap();
    stack.set_appearance("dark").unwrap();
    let drv = &mut stack.driver;
    drv.wait_for("the dark theme", STEP, |d| d.dark).await.unwrap();
    at_rest(drv).await;
    let frame = drv.render(&dir.join("tile-away-dark.png")).await.unwrap();
    assert_matches("tile-away-dark", &frame, TOLERANCE, &artifacts_dir()).unwrap();
    stack.shutdown().await;
}

/// A worker with nothing open: the workspace says how to begin, once.
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

/// The blocked agent's thread holds what it asks: the tray saying what the tool does, with the
/// way to answer in the terminal, its one answer when the hook offers none to press here. The
/// worker puts it there a grace after the hook, so a golden taken before it would hold a
/// thread that has not caught up.
fn asked_in_its_thread(d: &Dump) -> bool {
    d.a11y_node("Dialog", Some("Wants to run a command")).is_some()
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
        stack.driver.wait_for("a second shell", STEP, |d| d.terminals.len() == 2).await.unwrap();
    // It came in as a tab of the first's pane, there being no room beside it at this size:
    // ⌥⇧⌘→ moves it out to a pane of its own, so both agents are drawn.
    stack.driver.keys("cmd-alt-shift-right").await.unwrap();
    stack
        .driver
        .wait_for("two panes", STEP, |d| {
            d.items.len() == 2 && d.items.iter().all(|i| i.bounds[2] > 0.0)
        })
        .await
        .unwrap();
    let second = dump
        .terminals
        .iter()
        .find(|t| t.session != session)
        .map(|t| t.session.clone())
        .expect("the second shell");
    stack.play_hook(&second, "SessionStart", r#","source":"startup""#).await.unwrap();
    // An agent's tile is announced by its agent.
    let heading = format!("Claude Code {TITLED_AGENT}");
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
            d.terminal(&session).is_some_and(|t| t.agent.as_deref() == Some("needs-you"))
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
/// not that it needs the person; and its status line's worktree rides on its header, its
/// branch in the hint. Played through the worker's control socket and the real relay. (Its
/// pull request is its thread's row's, which the worker reads from the forge.)
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_paused_agent_says_what_it_waits_on_and_wears_its_worktree() {
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
            d.terminal(&session).is_some_and(|t| t.agent.as_deref() == Some("waiting"))
                && d.a11y.iter().any(|n| n.label.as_deref() == Some("Waiting on cargo test"))
        })
        .await
        .unwrap();
    let status = serde_json::json!({
        "session_id": slopty_e2e::harness::agent_session(&session),
        "transcript_path": stack.transcript_path(&session),
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
        .wait_for("the worktree on the header", STEP, |d| {
            d.a11y_node("Label", Some("Worktree fix-build on worktree-fix-build")).is_some()
        })
        .await
        .unwrap();
    assert!(
        !dump.a11y.iter().any(|n| n.label.as_deref() == Some("1 new")),
        "a paused turn asks nothing of the person: {:#?}",
        dump.a11y
    );
    golden(drv, &dir, "agent-waiting-worktree").await;
    stack.shutdown().await;
}

/// A port a shell listens on, and a file on its way up: the port counted in the title bar,
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
            d.lines_containing("~/drop-here %").len() == 1
        })
        .await
        .unwrap();
    let shell = dump.item("terminal").unwrap().clone();

    let port = steady_port();
    let listen = port.to_string();
    drv.keys("cmd-shift-t").await.unwrap();
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
    // ⌘⇧T put the second on a tab of its own: back to the first's.
    drv.keys("cmd-[").await.unwrap();
    let dump = drv
        .wait_for("the shell focused", STEP, |d| {
            d.items.iter().any(|i| i.id == shell.id && i.active)
        })
        .await
        .unwrap();
    let tile = dump.items.iter().find(|i| i.id == shell.id).unwrap().bounds;
    // The worker held still while the frame is drawn, so it has taken no byte of the upload
    // and the tile says 0% on every run: a running worker would have taken some, and a
    // different share each time.
    let worker = stack.worker_pid().expect("the worker's pid").to_string();
    signal("-STOP", &worker);
    let drv = &mut stack.driver;
    drv.drop_files(&[source.as_path()], tile[0] + tile[2] / 2.0, tile[1] + tile[3] / 2.0)
        .await
        .unwrap();
    let dump = drv
        .wait_for("the upload under way", STEP, |d| {
            d.a11y_node("Button", Some("Stop upload")).is_some()
        })
        .await
        .unwrap();
    assert!(dump.a11y_node("Button", Some("1 port")).is_some(), "{:#?}", dump.a11y);
    let frame = drv.render(&dir.join("transfers.png")).await.unwrap();
    signal("-CONT", &worker);
    assert_matches("transfers", &frame, TOLERANCE, &artifacts_dir()).unwrap();
    let cancel = drv.dump().await.unwrap();
    if let Some(node) = cancel.a11y_node("Button", Some("Stop upload")) {
        let [x, y, w, h] = node.bounds;
        drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
    }
    stack.shutdown().await;
}

/// Send `signal` (`-STOP`, `-CONT`) to process `pid`.
fn signal(signal: &str, pid: &str) {
    let sent = std::process::Command::new("/bin/kill").args([signal, pid]).status().unwrap();
    assert!(sent.success(), "kill {signal} {pid}: {sent}");
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
    // Pinned, the clock stands still, so the failed command says it took no time in every run.
    // It sleeps first, so the app sees it running in a frame of its own: a command over between
    // two frames is never seen running, and says no time at all.
    drv.ok(&Command::PinClock { at_ms: Some(PINNED_AT) }).await.unwrap();
    drv.type_text("echo fine").await.unwrap();
    drv.keys("enter").await.unwrap();
    drv.type_text("sleep 0.3; ls /e2e-missing").await.unwrap();
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

/// Two shells in one pane, there being no room for a pane each at this size: the header is a
/// tab row, a tab per shell with its slot and its title, the shown one on its body's surface.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_pane_of_two_draws_its_tab_row() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;
    drv.open(&["cat"], 1).await.unwrap();
    let dump = drv
        .wait_for("one pane of two tabs", STEP, |d| {
            d.items.len() == 2
                && d.items.iter().all(|i| i.same_pane(&d.items[0]))
                && d.items.iter().find(|i| i.active).is_some_and(|i| d.pane_tabs(i).len() == 2)
        })
        .await
        .unwrap();
    assert!(dump.a11y.iter().all(|n| n.label.as_deref() != Some("2/2")), "{:#?}", dump.a11y);
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    golden(drv, &dir, "pane-tabs").await;
    stack.shutdown().await;
}

/// A window as macOS 26 tiles it to half a screen, 756 × 900, holds two panes, one above the
/// other, with the navigator opened over them, since docked it would leave them a phone's
/// width. Then the window at the least size it takes, 375 × 480, its one pane a phone's.
/// Nothing in either may run past its edges
/// (`docs/decisions/ui.md`, "How surfaces adapt to their room").
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_half_screen_and_the_least_window_keep_their_chrome_whole() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: 756.0, height: 900.0 }).await.unwrap();
    first_shell(drv).await;
    drv.open(&["cat"], 1).await.unwrap();
    drv.wait_for("a second pane", STEP, |d| d.panes_on_show() == 2).await.unwrap();
    at_rest(drv).await;
    drv.keys("cmd-b").await.unwrap();
    drv.wait_for("the navigator over the panes", STEP, |d| {
        d.a11y_node("Navigation", Some("Navigator")).is_some()
    })
    .await
    .unwrap();
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    golden(drv, &dir, "half-screen").await;

    drv.keys("cmd-b").await.unwrap();
    drv.wait_for("the navigator put away", STEP, |d| {
        d.a11y_node("Navigation", Some("Navigator")).is_none()
    })
    .await
    .unwrap();
    drv.ok(&Command::Resize { width: 375.0, height: 480.0 }).await.unwrap();
    let dump = drv
        .wait_for("the least window", STEP, |d| (d.window.width - 375.0).abs() < 1.0)
        .await
        .unwrap();
    assert!((dump.window.height - 480.0).abs() < 1.0, "{:?}", dump.window);
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    golden(drv, &dir, "window-minimum").await;
    stack.shutdown().await;
}

/// This Mac on the tailnet as its Tailscale would describe it, with no peer to list.
const TAILNET_NAMING_THIS_MAC: &str = r#"{"BackendState":"Running","Self":{"ID":"n1","HostName":"mac-studio","DNSName":"mac-studio.tail1234.ts.net.","OS":"macOS","TailscaleIPs":["100.64.0.7"]},"Peer":null}"#;

/// The node labelled `label` with `role`, as device pixels.
fn pixels(dump: &Dump, role: &str, label: &str) -> PixelRect {
    let node =
        dump.a11y_node(role, Some(label)).unwrap_or_else(|| panic!("{label}: {:#?}", dump.a11y));
    let scale = dump.window.scale;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "window pixels")]
    node.bounds.map(|points| (points * scale).round().max(0.0) as u32)
}

/// "Connect a phone or iPad" from the palette: the server listens on this Mac's loopback, so
/// the code carries this Mac's name on the tailnet with the server's port, in both themes.
/// The port is the run's own, so the code and the address under it are left out of the
/// comparison; the dialog round them is held to the golden.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_code_for_a_phone_names_this_mac_on_the_tailnet() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    std::fs::write(stack.path("tailnet.json"), TAILNET_NAMING_THIS_MAC).unwrap();
    let port = stack.server.address().rsplit(':').next().unwrap().to_owned();
    let reached = format!("mac-studio.tail1234.ts.net:{port}");
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    first_shell(drv).await;
    drv.keys("cmd-shift-p").await.unwrap();
    drv.wait_for("the palette", STEP, |d| d.a11y_node("Dialog", Some("Commands")).is_some())
        .await
        .unwrap();
    drv.type_text("phone or ipad").await.unwrap();
    drv.wait_for("the line", STEP, |d| {
        d.a11y.iter().any(|n| {
            n.role == "ListBoxOption"
                && n.label.as_deref().is_some_and(|l| l.starts_with("Connect a phone or iPad"))
        })
    })
    .await
    .unwrap();
    drv.keys("enter").await.unwrap();
    let code = format!("Code for {reached}");
    let d = drv
        .wait_for("the code for this Mac's name", STEP, |d| {
            d.a11y_node("Dialog", Some("Connect a phone or iPad")).is_some()
                && d.a11y_node("Image", Some(&code)).is_some()
        })
        .await
        .unwrap();
    assert!(d.a11y_node("Label", Some(&reached)).is_some(), "{:#?}", d.a11y);
    assert_eq!(labels(&d, "Button").iter().filter(|b| *b == "Copy link").count(), 1);
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    for (name, dark) in [("invite", false), ("invite-dark", true)] {
        if dark {
            stack.set_appearance("dark").unwrap();
            stack.driver.wait_for("the dark theme", STEP, |d| d.dark).await.unwrap();
        }
        let drv = &mut stack.driver;
        let d = settled(drv).await;
        let masks = [pixels(&d, "Image", &code), pixels(&d, "Label", &reached)];
        let frame = drv.render(&dir.join(format!("{name}.png"))).await.unwrap();
        assert_matches_masked(name, &frame, TOLERANCE, &artifacts_dir(), &masks).unwrap();
    }
    let drv = &mut stack.driver;
    drv.keys("escape").await.unwrap();
    drv.wait_for("the dialog closed", STEP, |d| {
        d.a11y_node("Dialog", Some("Connect a phone or iPad")).is_none()
    })
    .await
    .unwrap();
    stack.shutdown().await;
}
