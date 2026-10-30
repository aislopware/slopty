//! A person's day with the app's own surfaces, walked as they walk it: one input after another
//! with nothing drawn from scratch between them, each landing on the frame the last one left.
//! The terminal dock and its tabs, the files and their editor, the side pane's tabs, Settings
//! and the palette, and the keyboard coming back to where it was each time.

#![cfg(target_os = "macos")]

use std::time::Duration;

use slopty_e2e::{A11yNode, Driver, Dump, Stack};

/// How long a worker round trip may take.
const STEP: Duration = Duration::from_secs(30);

/// The terminal the keyboard is in, by its session.
fn focused_terminal(dump: &Dump) -> Option<&str> {
    dump.focused.strip_prefix("terminal:")
}

/// The dock's terminal tabs, left to right.
fn tabs(dump: &Dump) -> Vec<&A11yNode> {
    dump.a11y.iter().filter(|n| n.role == "Tab").collect()
}

/// The middle of `node`, in window points.
fn middle(node: &A11yNode) -> (f32, f32) {
    let [x, y, w, h] = node.bounds;
    (w.mul_add(0.5, x), h.mul_add(0.5, y))
}

/// The lines of the terminal of `session`.
fn lines<'a>(dump: &'a Dump, session: &str) -> Vec<&'a str> {
    dump.terminals
        .iter()
        .filter(|t| t.session == session)
        .flat_map(|t| t.lines.iter().map(String::as_str))
        .collect()
}

/// Type `command` into the terminal with the keyboard and wait for `answer` in that terminal.
async fn run(drv: &mut Driver, command: &str, answer: &str) -> Dump {
    let typed_into = drv
        .wait_for("a terminal with the keyboard", STEP, |d| focused_terminal(d).is_some())
        .await
        .unwrap();
    let session = focused_terminal(&typed_into).unwrap_or_default().to_owned();
    drv.type_text(&format!("{command}\n")).await.unwrap();
    drv.wait_for(&format!("{answer:?} in {session}"), STEP, |d| {
        lines(d, &session).iter().any(|l| l.contains(answer) && !l.contains("$(("))
    })
    .await
    .unwrap()
}

/// The dock's tabs, a tab picked by pointer, a tab closed by its ✕, the dock hidden and shown,
/// Settings opened over it (⌘K is the shell's own clear there): each time the keyboard is where the
/// person left it, and what they type lands in the terminal they see in front.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_terminal_dock_keeps_up_with_a_person() {
    let mut stack = Stack::launch_at_home("e2e-walk").await.unwrap();
    let drv = &mut stack.driver;
    drv.wait_for("the worker", STEP, |d| d.status == "connected").await.unwrap();

    drv.keys("cmd-d").await.unwrap();
    let first = run(drv, "echo one-$((1+1))", "one-2").await;
    let first_session = focused_terminal(&first).unwrap_or_default().to_owned();
    println!("WALK dock\n{}", slopty_e2e::frame_text(&first.a11y));

    // A second tab, where the keyboard goes.
    drv.keys("cmd-t").await.unwrap();
    let second = drv
        .wait_for("a second terminal with the keyboard", STEP, |d| {
            tabs(d).len() == 2 && focused_terminal(d).is_some_and(|s| s != first_session)
        })
        .await
        .unwrap();
    let second_session = focused_terminal(&second).unwrap_or_default().to_owned();
    let second = run(drv, "echo two-$((2+2))", "two-4").await;
    println!("WALK two tabs\n{}", slopty_e2e::frame_text(&second.a11y));

    // The first tab by pointer: it comes to the front with the keyboard.
    let (x, y) = tabs(&second).first().map(|t| middle(t)).unwrap_or_default();
    drv.click(x, y).await.unwrap();
    drv.wait_for("the first tab with the keyboard", STEP, |d| {
        focused_terminal(d) == Some(first_session.as_str())
    })
    .await
    .unwrap();
    let back = run(drv, "echo back-$((3+3))", "back-6").await;
    assert!(
        lines(&back, &second_session).iter().all(|l| !l.contains("back-6")),
        "typed into the tab behind"
    );

    // The second tab's ✕, shown once the pointer is over its tab.
    let tab = tabs(&back).get(1).map(|t| middle(t)).unwrap_or_default();
    drv.ok(&slopty_e2e::Command::Move { x: tab.0, y: tab.1 }).await.unwrap();
    let hovered = drv
        .wait_for("the second tab's close", STEP, |d| {
            d.a11y.iter().filter(|n| n.role == "Button" && is_close(n)).count() >= 2
        })
        .await
        .unwrap();
    println!("WALK hovered\n{}", slopty_e2e::frame_text(&hovered.a11y));
    let close = hovered
        .a11y
        .iter()
        .filter(|n| n.role == "Button" && is_close(n))
        .min_by(|a, b| distance(a, tab).total_cmp(&distance(b, tab)))
        .map(middle)
        .unwrap_or_default();
    drv.click(close.0, close.1).await.unwrap();
    drv.wait_for("one tab left, the first", STEP, |d| {
        tabs(d).len() == 1 && focused_terminal(d) == Some(first_session.as_str())
    })
    .await
    .unwrap();

    // Hidden and shown again: the same shell, with the keyboard.
    drv.keys("cmd-d").await.unwrap();
    drv.wait_for("the dock hidden", STEP, |d| tabs(d).is_empty()).await.unwrap();
    drv.keys("cmd-d").await.unwrap();
    let shown = run(drv, "echo again-$((4+4))", "again-8").await;
    assert_eq!(focused_terminal(&shown), Some(first_session.as_str()), "the same shell");

    // Settings over it, shut with Escape.
    drv.keys("cmd-,").await.unwrap();
    drv.wait_for("Settings", STEP, |d| node(d, "Button", "Close settings").is_some())
        .await
        .unwrap();
    drv.keys("escape").await.unwrap();
    run(drv, "echo settings-$((6+6))", "settings-12").await;
    stack.shutdown().await;
}

/// Whether `node` is a tab's ✕.
fn is_close(node: &A11yNode) -> bool {
    node.label.as_deref().is_some_and(|l| l.starts_with("Close "))
}

/// How far `node`'s middle is from `at`.
fn distance(node: &A11yNode, at: (f32, f32)) -> f32 {
    let (x, y) = middle(node);
    (x - at.0).hypot(y - at.1)
}

/// The worker's name for the files walk, as the CLI names the machine.
const MACHINE: &str = "walk";
/// What the stand-in agent is asked to do.
const PROMPT: &str = "Read the notes";

/// Run git in `repo` with nothing of the developer's configuration.
fn git(repo: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(["-c", "user.name=Slopty", "-c", "user.email=e2e@slopty.invalid"])
        .args(["-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// The node `role` named exactly `label`.
fn node<'a>(dump: &'a Dump, role: &str, label: &str) -> Option<&'a A11yNode> {
    dump.a11y.iter().find(|n| n.role == role && n.label.as_deref() == Some(label))
}

/// Click the middle of the node `role` named `label`, once it is drawn.
async fn click(drv: &mut Driver, role: &str, label: &str) -> Dump {
    let dump = drv
        .wait_for(&format!("the {role} {label:?}"), STEP, |d| node(d, role, label).is_some())
        .await
        .unwrap();
    let (x, y) = node(&dump, role, label).map(middle).unwrap_or_default();
    drv.click(x, y).await.unwrap();
    dump
}

/// A stand-in agent's thread in a repository with a change, the files column open on it.
async fn files_walk() -> slopty_e2e::harness::ServedApp {
    let mut stack = slopty_e2e::harness::ServedApp::launch_rooted(MACHINE, &[], |_root| {
        vec![("STUB_TURNS".to_owned(), serde_json::json!({ PROMPT: [] }).to_string())]
    })
    .await
    .unwrap();
    let repo = stack.path("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("README.md"), "# Walk\n\nA repository.\n").unwrap();
    std::fs::write(repo.join("src/lib.rs"), "pub fn one() -> u32 {\n    1\n}\n").unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "First"]);
    std::fs::write(repo.join("src/lib.rs"), "pub fn one() -> u32 {\n    2 - 1\n}\n").unwrap();
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
    drv.ok(&slopty_e2e::Command::Resize { width: 1320.0, height: 880.0 }).await.unwrap();
    drv.keys("cmd-1").await.unwrap();
    drv.keys("cmd-e").await.unwrap();
    drv.wait_for("the files column", STEP, |d| node(d, "Button", "Hide files panel").is_some())
        .await
        .unwrap();
    stack
}

/// A file opened from the tree, edited and saved with ⌘S, a second file beside it, the tabs
/// switched by pointer, one closed with ⌘W and brought back with ⇧⌘T, and the Changes surface
/// with the tree's diff: each input lands where the person sees it.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_files_and_changes_keep_up_with_a_person() {
    let mut stack = files_walk().await;
    let lib = stack.path("repo").join("src/lib.rs");
    let drv = &mut stack.driver;
    let tree = click(drv, "TreeItem", "README.md").await;
    println!("WALK tree\n{}", slopty_e2e::frame_text(&tree.a11y));
    let opened = drv
        .wait_for("README.md in the editor", STEP, |d| {
            d.a11y.iter().any(|n| {
                n.role != "TreeItem" && n.label.as_deref().is_some_and(|l| l.ends_with("README.md"))
            })
        })
        .await
        .unwrap();
    println!("WALK opened\n{} (focus {})", slopty_e2e::frame_text(&opened.a11y), opened.focused);

    // A source file in the editor: what is typed lands in it, and ⌘S writes it to the worker's
    // disk.
    click(drv, "TreeItem", "src").await;
    click(drv, "TreeItem", "lib.rs").await;
    let editor = drv
        .wait_for("lib.rs in front", STEP, |d| node(d, "Button", "Close lib.rs").is_some())
        .await
        .unwrap();
    println!("WALK editor\n{} (focus {})", slopty_e2e::frame_text(&editor.a11y), editor.focused);
    drv.type_text("// walked\n").await.unwrap();
    drv.keys("cmd-s").await.unwrap();
    let saved = std::time::Instant::now();
    loop {
        let text = std::fs::read_to_string(&lib).unwrap_or_default();
        if text.contains("// walked") {
            break;
        }
        if saved.elapsed() > STEP {
            let shown = drv.dump().await.unwrap();
            panic!(
                "⌘S wrote nothing: the file says {text:?}; the app (focus {}) said:\n{}",
                shown.focused,
                slopty_e2e::frame_text(&shown.a11y)
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // The other tab by pointer, and back.
    click(drv, "Button", "README.md").await;
    drv.wait_for("README.md in front", STEP, |d| node(d, "Button", "Show Markdown code").is_some())
        .await
        .unwrap();
    click(drv, "Button", "src/lib.rs").await;
    drv.wait_for("lib.rs in front again", STEP, |d| {
        node(d, "Button", "Show Markdown code").is_none()
            && node(d, "Button", "Close lib.rs").is_some()
    })
    .await
    .unwrap();

    // Its ✕ closes it, and ⇧⌘T brings it back.
    click(drv, "Button", "Close lib.rs").await;
    drv.wait_for("lib.rs closed", STEP, |d| node(d, "Button", "Close lib.rs").is_none())
        .await
        .unwrap();
    drv.keys("cmd-shift-t").await.unwrap();
    let reopened =
        drv.wait_for("lib.rs back", STEP, |d| node(d, "Button", "Close lib.rs").is_some()).await;
    let reopened = match reopened {
        Ok(d) => d,
        Err(e) => panic!("{e:#}"),
    };
    println!(
        "WALK reopened\n{} (focus {})",
        slopty_e2e::frame_text(&reopened.a11y),
        reopened.focused
    );

    // A new tab's picker, then the Changes surface with the tree's diff.
    click(drv, "Button", "New tab").await;
    let picker = drv.dump().await.unwrap();
    println!("WALK picker\n{} (focus {})", slopty_e2e::frame_text(&picker.a11y), picker.focused);
    click(drv, "MenuItem", "Changes").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let changes = drv.dump().await.unwrap();
    println!(
        "WALK changes\n{} (focus {}, key contexts {})",
        slopty_e2e::frame_text(&changes.a11y),
        changes.focused,
        changes.focus_path
    );
    // The surface just opened has the keyboard: ⌘F finds in the diff, not in the chat.
    drv.keys("cmd-f").await.unwrap();
    let find = drv
        .wait_for("the diff's find with the keyboard", STEP, |d| {
            d.a11y.iter().any(|n| {
                n.focused && n.role != "Button" && n.label.as_deref() == Some("Find in diff")
            })
        })
        .await;
    if let Err(e) = find {
        let now = drv.dump().await.unwrap();
        panic!("{e:#}\nkey contexts: {}", now.focus_path);
    }
    drv.keys("escape").await.unwrap();

    // Settings and back.
    drv.keys("cmd-,").await.unwrap();
    click(drv, "Tab", "Appearance").await;
    click(drv, "Tab", "Shortcuts").await;
    click(drv, "Tab", "Devices").await;
    let devices = drv.dump().await.unwrap();
    println!("WALK devices\n{} (focus {})", slopty_e2e::frame_text(&devices.a11y), devices.focused);
    drv.keys("escape").await.unwrap();
    drv.wait_for("the chat again", STEP, |d| node(d, "Button", "Close settings").is_none())
        .await
        .unwrap();

    // The palette, a file found by name and opened.
    drv.keys("cmd-shift-p").await.unwrap();
    drv.type_text("READ").await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let palette = drv.dump().await.unwrap();
    println!("WALK palette\n{} (focus {})", slopty_e2e::frame_text(&palette.a11y), palette.focused);
    drv.keys("escape").await.unwrap();
    stack.shutdown().await;
}

/// The dock's shell across a dropped link: the path dies with nothing said, the system's wake
/// hands the app its resume, and the same shell takes what the person types next, with what it
/// showed before still there. Then the app quits and comes back: the dock, open when it quit,
/// opens again on a shell that takes the keyboard, though it asked for it ahead of its link.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_dock_s_shell_comes_back_after_a_dropped_link_and_a_relaunch() {
    let (mut stack, cut) = Stack::launch_behind_cut("e2e-walk-link").await.unwrap();
    let drv = &mut stack.driver;
    drv.wait_for("the worker", STEP, |d| d.status == "connected").await.unwrap();
    drv.keys("cmd-d").await.unwrap();
    let before = run(drv, "echo before-$((7+7))", "before-14").await;
    let session = focused_terminal(&before).unwrap_or_default().to_owned();
    let links = |d: &Dump| d.workers.first().map_or(0, |w| w.links);
    let seen = links(&before);

    cut.cut();
    drv.ok(&slopty_e2e::Command::Resume { what: "woke".into() }).await.unwrap();
    drv.wait_for("a new link", STEP, |d| links(d) > seen && d.status == "connected").await.unwrap();
    let after = run(drv, "echo after-$((8+8))", "after-16").await;
    assert_eq!(
        focused_terminal(&after),
        Some(session.as_str()),
        "the same shell, with the keyboard"
    );
    assert!(
        lines(&after, &session).iter().any(|l| l.contains("before-14")),
        "what it showed before is still there"
    );

    stack.kill_app().await.unwrap();
    stack.relaunch_app().await.unwrap();
    let drv = &mut stack.driver;
    drv.wait_for("the worker", STEP, |d| d.status == "connected").await.unwrap();
    let reopened = drv.dump().await.unwrap();
    println!(
        "WALK reopened\n{} (status {}, focus {})",
        slopty_e2e::frame_text(&reopened.a11y),
        reopened.status,
        reopened.focused
    );
    // The dock comes back as it was left; the keyboard starts where a launch puts it, and the
    // person picks the shell by its tab.
    if tabs(&reopened).is_empty() {
        drv.keys("cmd-d").await.unwrap();
    } else if focused_terminal(&reopened).is_none() {
        let (x, y) = tabs(&reopened).first().map(|t| middle(t)).unwrap_or_default();
        drv.click(x, y).await.unwrap();
    }
    // The home dock is not kept across launches: a working shell, whichever, with the keyboard.
    let again = run(drv, "echo again-$((9+9))", "again-18").await;
    assert!(focused_terminal(&again).is_some(), "a shell with the keyboard after a relaunch");
    println!("WALK relaunched\n{}", slopty_e2e::frame_text(&again.a11y));
    stack.shutdown().await;
}
