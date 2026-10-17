//! The shell's Changes and Files surfaces and its settings pages on a real stack: a thread of a
//! stand-in Claude Code (`slopty-stub-claude`) working in a git repository on the stack's
//! worker, the repository's uncommitted changes in the right pane, its tree in the files column,
//! and the Agents, Appearance, Shortcuts, Permissions, Devices and Archived pages. Every state is a
//! golden in light and in dark.

use std::path::Path;
use std::time::Duration;

use slopty_e2e::harness::ServedApp;
use slopty_e2e::{A11yNode, Command, Driver, Dump};

/// How long a worker round trip may take.
const STEP: Duration = Duration::from_secs(30);
/// The shell's main window as it first opens.
const WINDOW: (f32, f32) = (1320.0, 880.0);
/// Time for an answer the accessibility tree cannot show (a diff, a listing) to arrive and draw.
const SETTLE: Duration = Duration::from_secs(3);
/// The worker's name, as the CLI names the machine.
const MACHINE: &str = "surfaces";

/// What the agent is asked to do.
const PROMPT: &str = "Tidy the greeting";

/// The repository's first commit, then what the agent has changed since.
const MAIN_BEFORE: &str = "fn main() {\n    println!(\"hello\");\n}\n";
const MAIN_AFTER: &str =
    "fn main() {\n    let name = \"surfaces\";\n    println!(\"hello, {name}\");\n}\n";
const README: &str = "# Surfaces\n\nA repository an agent works in.\n";
const NOTES: &str = "The agent's notes, new and not yet committed.\n";
/// The project's permission rules (`.claude/settings.json`): one allowed, one always asked, one
/// denied.
const RULES: &str = r#"{"permissions":{"allow":["Bash(cargo test:*)"],"ask":["Bash(git push:*)"],"deny":["Bash(rm -rf:*)"]}}"#;

/// Run git in `repo` with nothing of the developer's configuration.
fn git(repo: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(["-c", "user.name=Slopty", "-c", "user.email=e2e@slopty.invalid"])
        .args(["-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", "2026-10-13T09:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-10-13T09:00:00Z")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A repository with one commit, a file changed since and a file not yet added.
fn repository(repo: &Path) {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("README.md"), README).unwrap();
    std::fs::write(repo.join("src/main.rs"), MAIN_BEFORE).unwrap();
    git(repo, &["init", "-q"]);
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "First"]);
    std::fs::write(repo.join("src/main.rs"), MAIN_AFTER).unwrap();
    std::fs::write(repo.join("NOTES.md"), NOTES).unwrap();
}

/// Render the window in light and in dark against `golden/<name>-<appearance>.png`, the done
/// line's moment left out ([`crate::served::goldens`]); the window is light again after.
async fn goldens(drv: &mut Driver, renders: &Path, name: &str) {
    crate::served::goldens(drv, renders, name, &[]).await;
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

/// A stand-in agent's thread in a repository with changes: ⌘1 selects it, ⌘R opens the right
/// pane, whose Diffs surface shows the working tree's two changed files with their hunks; ⌘E
/// opens the files column on the thread's folder, and the titlebar has no TUI switch: an agent
/// is driven, never shown as a terminal. Then ⌘, opens Settings, whose Providers page
/// lists the machine's agents and the autonomy setting, its Appearance and Shortcuts pages,
/// Permissions with the project's rules as Claude Code reads them, Devices with Add
/// device's dialog, and Archived with the thread archived by ⌘⇧A, which Unarchive brings back.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn changes_files_and_settings_show_the_threads_work() {
    // The stand-in takes its prompt's turn as Claude Code driven does, so its thread has a turn
    // to show.
    let mut stack = ServedApp::launch_rooted(MACHINE, &[], |_root| {
        vec![("STUB_TURNS".to_owned(), serde_json::json!({ PROMPT: [] }).to_string())]
    })
    .await
    .unwrap();
    let repo = stack.path("repo");
    repository(&repo);
    // The project's own permission rules, which Settings → Permissions reads through the agent.
    std::fs::create_dir_all(repo.join(".claude")).unwrap();
    std::fs::write(repo.join(".claude/settings.json"), RULES).unwrap();
    let renders = stack.path("");
    let folder = repo.to_string_lossy().into_owned();
    let mut start = vec!["agent", "start", MACHINE, "--folder", &folder];
    start.extend(PROMPT.split(' '));
    let started = stack.slopty(&start).await.unwrap();
    let agent = started["agent"].as_str().unwrap_or_default().to_owned();
    assert!(!agent.is_empty(), "an agent started: {started}");

    // The worker has the agent's thread, with its prompt, once the driven stand-in took it; the
    // shell's sidebar then lists it.
    let listed = std::time::Instant::now();
    loop {
        let read = stack.slopty(&["agent", "read", "--thread", &agent]).await;
        if read.as_ref().is_ok_and(|read| read.to_string().contains(PROMPT)) {
            break;
        }
        assert!(listed.elapsed() < STEP, "the agent's thread read back: {read:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    drv.keys("cmd-1").await.unwrap();

    // The right pane's surface list, then the working tree's diff: its two files, its hunks.
    drv.keys("cmd-r").await.unwrap();
    click(drv, "Button", "Diffs").await;
    drv.wait_for("the diff surface", STEP, |d| node(d, "Button", "Diffs").is_none()).await.unwrap();
    // The worker's first diff of the tree.
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &renders, "surface-files-changes").await;

    // The files column on the thread's folder, beside the diff.
    drv.keys("cmd-e").await.unwrap();
    drv.wait_for("the files column", STEP, |d| node(d, "Button", "Hide files panel").is_some())
        .await
        .unwrap();
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &renders, "surface-files-tree").await;
    let tree = drv.dump().await.unwrap();
    assert!(
        node(&tree, "Button", "Claude Code's TUI beside the chat").is_none(),
        "no TUI switch:\n{}",
        slopty_e2e::frame_text(&tree.a11y)
    );

    // Settings: the Agents page (Providers, as zeron names it) with the autonomy setting, then
    // Appearance.
    drv.keys("cmd-,").await.unwrap();
    click(drv, "Tab", "Providers").await;
    drv.wait_for("the autonomy setting", STEP, |d| node(d, "Button", "Autonomy: Auto").is_some())
        .await
        .unwrap();
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &renders, "settings-agents").await;
    click(drv, "Tab", "Appearance").await;
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &renders, "settings-appearance").await;
    // The theme menu marks the default theme, and a row off its default (the frame's opaque
    // glass) offers Reset, and only that row.
    let page = drv.dump().await.unwrap();
    assert!(
        node(&page, "Button", "Reset Glass to default").is_some()
            && node(&page, "Button", "Reset Reduce motion to default").is_none(),
        "Reset only off the default:\n{}",
        slopty_e2e::frame_text(&page.a11y)
    );
    click(drv, "Button", "Light theme: Slopty Light").await;
    drv.wait_for("the theme menu", STEP, |d| node(d, "MenuItemRadio", "Slopty Light").is_some())
        .await
        .unwrap();
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &renders, "settings-appearance-defaults").await;
    drv.keys("escape").await.unwrap();
    // The menu fades out; a frame mid-fade is not one a scratch frame draws.
    tokio::time::sleep(SETTLE).await;
    drv.wait_for("the theme menu closed", STEP, |d| {
        node(d, "MenuItemRadio", "Slopty Light").is_none()
    })
    .await
    .unwrap();

    // Shortcuts: every binding by what it does.
    click(drv, "Tab", "Shortcuts").await;
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &renders, "settings-shortcuts").await;

    // Permissions: the folder's rules as Claude Code reads them, by behaviour and by where each
    // comes from.
    click(drv, "Tab", "Permissions").await;
    let rules = drv
        .wait_for("the project's rules", STEP, |d| {
            d.a11y.iter().any(|n| n.label.as_deref().is_some_and(|l| l.contains("cargo test")))
        })
        .await;
    if let Err(e) = &rules {
        let shown = drv.dump().await.unwrap();
        panic!("{e:#}\nPermissions said:\n{}", slopty_e2e::frame_text(&shown.a11y));
    }
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &renders, "settings-permissions").await;

    // Devices: this Mac's worker, then Add device's dialog, which a self-test has no ssh for.
    click(drv, "Tab", "Devices").await;
    drv.wait_for("the add row", STEP, |d| node(d, "Button", "Add device").is_some()).await.unwrap();
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &renders, "settings-devices").await;
    click(drv, "Button", "Add device").await;
    drv.wait_for("the dialog", STEP, |d| node(d, "Dialog", "Add device").is_some()).await.unwrap();
    goldens(drv, &renders, "settings-devices-add").await;
    drv.keys("escape").await.unwrap();
    drv.wait_for("the dialog gone", STEP, |d| node(d, "Dialog", "Add device").is_none())
        .await
        .unwrap();

    // Archived: the thread archived from the sidebar's shortcut is listed there, and comes
    // back with Unarchive.
    drv.keys("escape").await.unwrap();
    drv.keys("cmd-1").await.unwrap();
    drv.keys("cmd-shift-a").await.unwrap();
    drv.keys("cmd-,").await.unwrap();
    click(drv, "Tab", "Archived chats").await;
    drv.wait_for("the archived thread", STEP, |d| node(d, "Button", "Unarchive").is_some())
        .await
        .unwrap();
    tokio::time::sleep(SETTLE).await;
    // The archive's toast lasts a while of its own: it stays out of the golden.
    crate::served::goldens(drv, &renders, "settings-archived", &["Archived \u{201c}"]).await;
    click(drv, "Button", "Unarchive").await;
    drv.wait_for("the thread back", STEP, |d| node(d, "Button", "Unarchive").is_none())
        .await
        .unwrap();

    stack.shutdown().await;
}

/// A stand-in agent's thread in the repository, selected, with the files column open on its
/// folder.
async fn files_column() -> ServedApp {
    let mut stack = ServedApp::launch_rooted(MACHINE, &[], |_root| {
        vec![("STUB_TURNS".to_owned(), serde_json::json!({ PROMPT: [] }).to_string())]
    })
    .await
    .unwrap();
    let repo = stack.path("repo");
    repository(&repo);
    let folder = repo.to_string_lossy().into_owned();
    let mut start = vec!["agent", "start", MACHINE, "--folder", &folder];
    start.extend(PROMPT.split(' '));
    let started = stack.slopty(&start).await.unwrap();
    let agent = started["agent"].as_str().unwrap_or_default().to_owned();
    assert!(!agent.is_empty(), "an agent started: {started}");
    let listed = std::time::Instant::now();
    loop {
        let read = stack.slopty(&["agent", "read", "--thread", &agent]).await;
        if read.as_ref().is_ok_and(|read| read.to_string().contains(PROMPT)) {
            break;
        }
        assert!(listed.elapsed() < STEP, "the agent's thread read back: {read:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    drv.keys("cmd-1").await.unwrap();
    drv.keys("cmd-e").await.unwrap();
    drv.wait_for("the files column", STEP, |d| node(d, "Button", "Hide files panel").is_some())
        .await
        .unwrap();
    tokio::time::sleep(SETTLE).await;
    stack
}

/// A file picked in the files column opens in the editor beside the chat: its folder unfolds,
/// and the file's text shows with its name on the editor's tab, in light and dark.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_file_picked_in_the_tree_opens_in_the_editor() {
    let mut stack = files_column().await;
    let renders = stack.path("");
    let drv = &mut stack.driver;
    click(drv, "TreeItem", "src").await;
    click(drv, "TreeItem", "main.rs").await;
    let opened = drv
        .wait_for("the file in the editor", STEP, |d| {
            d.a11y.iter().any(|n| {
                n.role != "TreeItem" && n.label.as_deref().is_some_and(|l| l.ends_with("main.rs"))
            })
        })
        .await;
    if let Err(e) = opened {
        let shown = drv.dump().await.unwrap();
        panic!("{e:#}\nthe app said:\n{}", slopty_e2e::frame_text(&shown.a11y));
    }
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &renders, "surface-files-editor").await;
    stack.shutdown().await;
}

/// How many columns of the band 2 to 4 points above `rect` (points, at `scale`) differ from the
/// colour 6 points above it, against how many were read: a ring drawn round the control fills
/// the band; without one, the band is the toolbar's own ground.
fn ring_band(image: &image::RgbaImage, rect: [f32; 4], scale: f32) -> (usize, usize) {
    let [x, y, w, _] = rect;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixels")]
    let px = |v: f32| (v * scale).max(0.0) as u32;
    // Clear of the corners, whose curve leaves the band.
    let (x0, x1) = (px(x + 8.0), px(x + w - 8.0).min(image.width()));
    let (band, ground) = (px(y - 3.0), px(y - 6.0));
    let differs = (x0..x1)
        .filter(|&col| {
            let (ring, under) = (image.get_pixel(col, band).0, image.get_pixel(col, ground).0);
            ring.iter().zip(under).take(3).any(|(one, other)| one.abs_diff(other) > 48)
        })
        .count();
    (differs, x1.saturating_sub(x0) as usize)
}

/// The files column's hidden-files toggle, the stop the walk below looks for.
const TOGGLE: &str = "Show hidden files";

/// Tab walks into the files column and reaches its controls, each wearing the shell's ring: 2
/// points of the accent 2 points clear of the control, drawn only while it has the keyboard, in
/// light and in dark (`docs/decisions/ui.md`, "Every pane control is a keyboard stop with one
/// ring"). The renders go to the artifacts for review.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_tab_walks_the_files_pane_with_a_ring() {
    let mut stack = files_column().await;
    let drv = &mut stack.driver;

    // The toggle at rest: no ring round it.
    let artifacts = slopty_e2e::harness::artifacts_dir();
    std::fs::create_dir_all(&artifacts).unwrap();
    let rest = drv.render(&artifacts.join("files-pane-ring-rest.png")).await.unwrap();
    let toggle = rest
        .a11y
        .iter()
        .find(|n| n.role == "Button" && n.label.as_deref() == Some(TOGGLE))
        .map(|n| n.bounds)
        .expect("the files column's hidden-files toggle");
    let (lit, read) = ring_band(&rest.image, toggle, rest.scale);
    assert!(lit * 10 <= read, "no ring at rest: {lit} of {read}");

    // Tab walks the window's stops until the files column's toggle has the keyboard; each stop
    // it passes in the column is one too.
    let mut walked = Vec::new();
    let mut reached = false;
    for _ in 0..120 {
        drv.keys("tab").await.unwrap();
        let dump = drv.dump().await.unwrap();
        let Some(focused) = dump.a11y.iter().find(|n| n.focused) else { continue };
        let label = focused.label.clone().unwrap_or_default();
        walked.push(format!("{} {label}", focused.role));
        if focused.role == "Button" && label == TOGGLE {
            reached = true;
            break;
        }
    }
    assert!(reached, "Tab reached the files column's toggle; it walked:\n{}", walked.join("\n"));

    for (dark, appearance) in [(false, "light"), (true, "dark")] {
        drv.ok(&Command::Appearance { dark }).await.unwrap();
        tokio::time::sleep(Duration::from_millis(600)).await;
        let frame =
            drv.render(&artifacts.join(format!("files-pane-ring-{appearance}.png"))).await.unwrap();
        let focused = frame
            .a11y
            .iter()
            .find(|n| n.focused)
            .unwrap_or_else(|| panic!("the toggle keeps the keyboard in {appearance}"));
        assert_eq!(focused.label.as_deref(), Some(TOGGLE));
        let (lit, read) = ring_band(&frame.image, focused.bounds, frame.scale);
        assert!(
            read > 0 && lit * 10 >= read * 8,
            "the ring is drawn round the toggle in {appearance}: {lit} of {read} columns"
        );
    }
    drv.ok(&Command::Appearance { dark: false }).await.unwrap();
    stack.shutdown().await;
}
