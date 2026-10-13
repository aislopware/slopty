//! The shell's Changes and Files surfaces and its settings pages on a real stack: a thread of a
//! stand-in Claude Code (`slopty-stub-claude`) working in a git repository on the stack's
//! worker, the repository's uncommitted changes in the right pane, its tree in the files column,
//! and the Agents and Appearance pages. Every state is a golden in light and in dark.

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
/// opens the files column on the thread's folder. Then ⌘, opens Settings, whose Providers page
/// lists the machine's agents and the autonomy setting, and its Appearance page.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn changes_files_and_settings_show_the_threads_work() {
    let mut stack = ServedApp::launch(MACHINE).await.unwrap();
    let repo = stack.path("repo");
    repository(&repo);
    let renders = stack.path("");
    let folder = repo.to_string_lossy().into_owned();
    let started = stack
        .slopty(&["agent", "start", MACHINE, "--folder", &folder, "Tidy", "the", "greeting"])
        .await
        .unwrap();
    assert!(started["agent"].is_string(), "an agent started: {started}");

    // The server lists the agent once its worker has started it; the shell's sidebar then has
    // its thread.
    let listed = std::time::Instant::now();
    while !stack.slopty(&["agents"]).await.unwrap().to_string().contains("Tidy the greeting") {
        assert!(listed.elapsed() < STEP, "the agent listed");
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

    stack.shutdown().await;
}
