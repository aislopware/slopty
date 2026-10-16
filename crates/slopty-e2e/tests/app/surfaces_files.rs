//! The shell's Changes and Files surfaces and its settings pages on a real stack: a thread of a
//! stand-in Claude Code (`slopty-stub-claude`) working in a git repository on the stack's
//! worker, the repository's uncommitted changes in the right pane, its tree in the files column,
//! and the Agents, Appearance, Devices and Archived pages. Every state is a golden in light and
//! in dark.

use std::path::Path;
use std::time::Duration;

use slopty_e2e::harness::{ServedApp, artifacts_dir};
use slopty_e2e::snapshot::{MAC_TOLERANCE as TOLERANCE, assert_matches};
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

/// Render the window in light and in dark and hold each against `golden/<name>-<appearance>.png`;
/// the window is light again after.
async fn goldens(drv: &mut Driver, renders: &Path, name: &str) {
    for (dark, appearance) in [(false, "light"), (true, "dark")] {
        drv.ok(&Command::Appearance { dark }).await.unwrap();
        // The appearance's first frame, and its hover and fade tweens settled.
        tokio::time::sleep(Duration::from_millis(600)).await;
        let golden = format!("{name}-{appearance}");
        let frame = drv.render(&renders.join(format!("{golden}.png"))).await.unwrap();
        println!("A11Y {golden}\n{}", slopty_e2e::frame_text(&frame.a11y));
        assert_matches(&golden, &frame, TOLERANCE, &artifacts_dir()).unwrap();
    }
    drv.ok(&Command::Appearance { dark: false }).await.unwrap();
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
/// lists the machine's agents and the autonomy setting, its Appearance page, Devices with Add
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
    click(drv, "Tab", "Archived sessions").await;
    drv.wait_for("the archived thread", STEP, |d| node(d, "Button", "Unarchive").is_some())
        .await
        .unwrap();
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &renders, "settings-archived").await;
    click(drv, "Button", "Unarchive").await;
    drv.wait_for("the thread back", STEP, |d| node(d, "Button", "Unarchive").is_none())
        .await
        .unwrap();

    stack.shutdown().await;
}
