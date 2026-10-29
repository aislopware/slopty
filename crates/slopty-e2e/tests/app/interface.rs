//! The shell on a real stack: the app's one window, with the stack's worker as a
//! device and its threads as chats, served from the workspace (`slopty_ui::view_model`).

use slopty_e2e::harness::artifacts_dir;
use slopty_e2e::snapshot::{MAC_TOLERANCE as TOLERANCE, assert_matches, foreground_fraction};
use slopty_e2e::{Command, Stack};

/// The shell's main window as it first opens.
const WINDOW: (f32, f32) = (1320.0, 880.0);

/// The app comes up as the shell: its sidebar and its new-session canvas, drawn and named for a
/// screen reader, in light and dark, on its opaque planes and frosted
/// over the render's fixed backdrop.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_interface_comes_up_on_the_stack() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    for (golden, dark, frosted) in [
        ("interface", false, false),
        ("interface-dark", true, false),
        ("interface-frosted", false, true),
        ("interface-frosted-dark", true, true),
    ] {
        let path = stack.path(&format!("{golden}.png"));
        stack.driver.ok(&Command::Appearance { dark }).await.unwrap();
        stack.driver.ok(&Command::Surface { frosted }).await.unwrap();
        let frame = stack.driver.render(&path).await.unwrap();
        println!("A11Y {golden} {}", slopty_e2e::frame_text(&frame.a11y));
        let fg = foreground_fraction(&frame);
        assert!(fg > 0.002, "{golden}: frame is blank ({fg:.4} foreground)");
        assert_matches(golden, &frame, TOLERANCE, &artifacts_dir()).unwrap();
    }
}

// The palette and New project over a stand-in agent's thread: what its agent said is found in
// the palette, the palette's Recent Files are there on its first paint, and a repository is
// cloned into the open folder as a new project.

/// How long a worker round trip may take.
const STEP: std::time::Duration = std::time::Duration::from_secs(30);
/// The worker's name, as the CLI names the machine.
const MACHINE: &str = "interface";
/// What the stand-in agent is asked; it answers `Done: <prompt>` (`STUB_TURNS`).
const PROMPT: &str = "Tidy the greeting";

/// Run git in `dir` with nothing of the developer's configuration.
fn git(dir: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(["-c", "user.name=Slopty", "-c", "user.email=e2e@slopty.invalid"])
        .args(["-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A stack whose stand-in agent answers [`PROMPT`], with a thread of it started in `root/repo`, a
/// repository with a commit and a change since.
async fn agent_stack() -> slopty_e2e::harness::ServedApp {
    let stack = slopty_e2e::harness::ServedApp::launch_rooted(MACHINE, &[], |_root| {
        vec![("STUB_TURNS".to_owned(), serde_json::json!({ PROMPT: [] }).to_string())]
    })
    .await
    .unwrap();
    let repo = stack.path("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/main.rs"), "fn main() {}\n").unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "First"]);
    std::fs::write(repo.join("src/main.rs"), "fn main() {\n    println!(\"hi\");\n}\n").unwrap();
    std::fs::write(repo.join("NOTES.md"), "Notes, new.\n").unwrap();
    let folder = repo.to_string_lossy().into_owned();
    let mut start = vec!["agent", "start", MACHINE, "--folder", &folder];
    start.extend(PROMPT.split(' '));
    let started = stack.slopty(&start).await.unwrap();
    let agent = started["agent"].as_str().unwrap_or_default().to_owned();
    assert!(!agent.is_empty(), "an agent started: {started}");
    // Its reply is in its thread once the stand-in's turn ended.
    let since = std::time::Instant::now();
    loop {
        let read = stack.slopty(&["agent", "read", "--thread", &agent]).await;
        if read.as_ref().is_ok_and(|read| read.to_string().contains("Done:")) {
            break;
        }
        assert!(since.elapsed() < STEP, "the agent's reply read back: {read:?}");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    stack
}

/// A node whose label holds `words`.
fn labelled<'a>(dump: &'a slopty_e2e::Dump, words: &str) -> Option<&'a slopty_e2e::A11yNode> {
    dump.a11y.iter().find(|n| n.label.as_deref().is_some_and(|label| label.contains(words)))
}

/// Rest the pointer on the point `at` names once it stands still (the same in two dumps
/// running), until `shown` holds. A control that moves from under a resting pointer while the
/// window settles loses its hover and its dwell, so the pointer follows it to where it now
/// stands; the dump that shows it is returned.
async fn rest_on(
    drv: &mut slopty_e2e::Driver,
    what: &str,
    at: impl Fn(&slopty_e2e::Dump) -> Option<(f32, f32)>,
    shown: impl Fn(&slopty_e2e::Dump) -> bool,
) -> slopty_e2e::Dump {
    let since = std::time::Instant::now();
    loop {
        let mut last = None;
        let still = drv
            .wait_for(&format!("{what} standing still"), STEP, |d| {
                let now = at(d);
                let same = now.is_some() && now == last;
                last = now;
                same
            })
            .await
            .unwrap();
        let point = at(&still).expect("found above");
        drv.ok(&Command::Move { x: point.0, y: point.1 }).await.unwrap();
        let dump = drv.wait_for(what, STEP, |d| shown(d) || at(d) != Some(point)).await.unwrap();
        if shown(&dump) {
            return dump;
        }
        assert!(since.elapsed() < STEP.saturating_mul(2), "{what}: the control never stood still");
    }
}

/// The palette's Threads tab finds a thread by its agent's words, which no title holds: its row
/// says who said them.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_palette_finds_a_thread_by_what_its_agent_said() {
    let mut stack = agent_stack().await;
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    drv.keys("cmd-k").await.unwrap();
    drv.keys("cmd-2").await.unwrap();
    drv.type_text("Done:").await.unwrap();
    let found = drv
        .wait_for("the thread by its agent's words", STEP, |d| {
            labelled(d, "Agent: Done: Tidy the greeting").is_some()
        })
        .await
        .unwrap();
    let row = labelled(&found, "Agent: Done:").unwrap();
    assert_eq!(row.role, "Button", "{}", slopty_e2e::frame_text(&found.a11y));
    drv.keys("escape").await.unwrap();
    stack.shutdown().await;
}

/// ⌘K over a thread lists its workspace's recently changed files on the palette's first paint,
/// or within a few frames of it; the time is a measurement (`docs/MEASUREMENTS.md`).
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_palette_lists_recent_files_at_once() {
    let mut stack = agent_stack().await;
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    drv.keys("cmd-1").await.unwrap();
    // The thread open, its workspace known, before the palette is asked.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let mut took = Vec::new();
    for _ in 0..10 {
        let since = std::time::Instant::now();
        drv.keys("cmd-k").await.unwrap();
        let first = drv.dump().await.unwrap();
        let at_once = labelled(&first, "NOTES.md").is_some();
        drv.wait_for("Recent Files", STEP, |d| labelled(d, "NOTES.md").is_some()).await.unwrap();
        took.push((since.elapsed(), at_once));
        // Closed again before the next ⌘K, which would otherwise close it.
        drv.keys("escape").await.unwrap();
        drv.wait_for("the palette closed", STEP, |d| labelled(d, "NOTES.md").is_none())
            .await
            .unwrap();
    }
    let first_dumps = took.iter().filter(|(_, at_once)| *at_once).count();
    let mut times: Vec<_> = took.iter().map(|(time, _)| time.as_secs_f64() * 1e3).collect();
    times.sort_by(f64::total_cmp);
    println!(
        "MEASURE palette Recent Files: in the first dump {first_dumps}/10, p50 {:.0} ms max {:.0} ms",
        times[times.len() / 2],
        times[times.len() - 1]
    );
    assert!(times[times.len() / 2] < 1000.0, "Recent Files within a second: {times:?}");
    stack.shutdown().await;
}

/// New project's "Clone repository…" clones a repository into the folder open in the picker, on
/// the picked machine, and the clone becomes the project the window lands in.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn new_project_clones_a_repository_into_the_open_folder() {
    let mut stack = agent_stack().await;
    // A repository to clone from: bare, with one commit pushed into it.
    let origin = stack.path("origin.git");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "-q", "--bare"]);
    let seed = stack.path("seed");
    std::fs::create_dir_all(&seed).unwrap();
    std::fs::write(seed.join("README.md"), "# Origin\n").unwrap();
    git(&seed, &["init", "-q"]);
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-q", "-m", "First"]);
    git(&seed, &["push", "-q", &origin.to_string_lossy(), "main"]);
    // The worker clones only from a remote: git's own daemon serves the origin on loopback.
    let daemon = GitDaemon::serve(&stack.path("")).await;
    let url = format!("git://127.0.0.1:{}/origin.git", daemon.port);

    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    // New project: the machine, then Home, then the row after its folders.
    drv.keys("cmd-shift-n").await.unwrap();
    drv.type_text(MACHINE).await.unwrap();
    drv.keys("enter").await.unwrap();
    drv.keys("enter").await.unwrap();
    let shown =
        drv.wait_for("the clone row", STEP, |d| labelled(d, "Clone repository…").is_some()).await;
    let [x, y, w, h] = labelled(&shown.unwrap(), "Clone repository…").unwrap().bounds;
    drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
    drv.type_text(&url).await.unwrap();
    drv.wait_for("the clone it makes", STEP, |d| labelled(d, "Clone origin").is_some())
        .await
        .unwrap();
    drv.keys("enter").await.unwrap();

    let clone = stack.path("home").join("origin");
    let since = std::time::Instant::now();
    while !clone.join("README.md").exists() {
        assert!(since.elapsed() < STEP, "the clone at {}", clone.display());
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    // The window lands in the clone's project, on a new session there: the thread it showed is
    // left behind (Back returns to it), and nothing says the clone failed.
    let landed = stack
        .driver
        .wait_for("a new session in the clone", STEP, |d| {
            d.a11y_node("Button", Some("Back")).is_some()
                && d.a11y_node("Button", Some("Open in…")).is_none()
        })
        .await
        .unwrap();
    assert!(
        labelled(&landed, "Couldn't clone").is_none(),
        "{}",
        slopty_e2e::frame_text(&landed.a11y)
    );
    drop(daemon);
    stack.shutdown().await;
}

/// `git daemon` serving every repository under a folder on a loopback port, ended when dropped.
struct GitDaemon {
    child: std::process::Child,
    port: u16,
}

impl GitDaemon {
    async fn serve(base: &std::path::Path) -> Self {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        // The daemon itself, not `git daemon`, whose wrapper would outlive nothing it started.
        let exec = std::process::Command::new("git").arg("--exec-path").output().unwrap();
        let exec = String::from_utf8(exec.stdout).unwrap();
        let child =
            std::process::Command::new(std::path::Path::new(exec.trim()).join("git-daemon"))
                .args(["--reuseaddr", "--export-all", "--listen=127.0.0.1"])
                .arg(format!("--port={port}"))
                .arg(format!("--base-path={}", base.display()))
                .arg(base)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .spawn()
                .unwrap();
        let since = std::time::Instant::now();
        while tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err() {
            assert!(since.elapsed() < STEP, "git daemon on {port}");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        Self { child, port }
    }
}

impl Drop for GitDaemon {
    fn drop(&mut self) {
        if let Err(gone) = self.child.kill().and_then(|()| self.child.wait().map(drop)) {
            eprintln!("git daemon already ended: {gone}");
        }
    }
}

// The sidebar's server menu and a row's hover card, held to their words and their renders.

/// Render the window in light and in dark and hold each against `golden/<name>-<appearance>`,
/// a finished turn's moment left out ([`crate::served::goldens`]); the window is light again
/// after.
async fn goldens_both(drv: &mut slopty_e2e::Driver, dir: &std::path::Path, name: &str) {
    crate::served::goldens(drv, dir, name, &[]).await;
}

/// The server pill's menu names the link and offers what to do about servers and machines:
/// Devices opens Settings → Devices.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_server_menu_offers_devices_and_a_server_change() {
    let mut stack = Stack::launch("e2e-server-menu").await.unwrap();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    let drv = &mut stack.driver;
    drv.wait_for("the worker", STEP, |d| d.status == "connected").await.unwrap();
    let pill =
        drv.wait_for("the server pill", STEP, |d| labelled(d, "Server: ").is_some()).await.unwrap();
    let [x, y, w, h] = labelled(&pill, "Server: ").unwrap().bounds;
    drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
    let menu = drv
        .wait_for("the server menu", STEP, |d| {
            d.a11y_node("MenuItem", Some("Devices")).is_some()
                && d.a11y_node("MenuItem", Some("Change server")).is_some()
        })
        .await
        .unwrap();
    // Every row sits inside the footer's width, above the pill.
    let rows: Vec<[f32; 4]> = ["Devices", "Change server"]
        .iter()
        .map(|row| menu.a11y_node("MenuItem", Some(row)).unwrap().bounds)
        .collect();
    assert!(rows.iter().all(|r| r[1] + r[3] <= y + 0.5), "{rows:?} above {y}");
    assert!(rows.iter().all(|r| r[3] >= 24.0), "zeron's row height: {rows:?}");
    goldens_both(drv, &dir, "sidebar-server-menu").await;
    let [x, y, w, h] = menu.a11y_node("MenuItem", Some("Devices")).unwrap().bounds;
    drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
    drv.wait_for("Settings → Devices", STEP, |d| {
        d.a11y_node("Group", Some("Settings")).is_some()
    })
    .await
    .unwrap();
    stack.shutdown().await;
}

/// Resting on a thread's row floats zeron's session card out beside it: where it runs (its
/// project and machine), how long it has stood as it is, and its agent; a press puts it away.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_rows_hover_card_names_where_it_runs() {
    let mut stack = agent_stack().await;
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    // The row's title, the leftmost node holding the prompt's words (the sidebar's).
    let title_of = |d: &slopty_e2e::Dump| {
        d.a11y
            .iter()
            .filter(|n| n.label.as_deref().is_some_and(|label| label.contains(PROMPT)))
            .min_by(|a, b| a.bounds[0].total_cmp(&b.bounds[0]))
            .map(|n| n.bounds)
    };
    let card = rest_on(
        drv,
        "the hover card",
        |d| title_of(d).map(|t| (t[0] + 4.0, t[1] + t[3] / 2.0)),
        |d| d.a11y_node("Tooltip", None).is_some(),
    )
    .await;
    let title = title_of(&card).unwrap();
    let node = card.a11y_node("Tooltip", None).unwrap();
    let words = format!(
        "{} {}",
        node.label.as_deref().unwrap_or_default(),
        node.value.as_deref().unwrap_or_default()
    );
    assert!(words.contains(PROMPT), "its title: {words}");
    assert!(words.contains(&format!("@ {MACHINE}")), "where it runs: {words}");
    assert!(words.contains("Done") || words.contains("Active"), "its age: {words}");
    let [card_x, card_y, _, card_h] = node.bounds;
    println!("MEASURE hover card at {card_x:.0},{card_y:.0}, {card_h:.0} tall; row {title:?}");
    assert!(card_x > title[0] + title[2], "beside the row, not over it: {card_x} vs {title:?}");
    assert!(card_h >= 60.0, "whole: {card_h}");
    // A press on the row has the pointer's intent: the card goes.
    drv.click(title[0] + 4.0, title[1] + title[3] / 2.0).await.unwrap();
    drv.wait_for("the card gone", STEP, |d| d.a11y_node("Tooltip", None).is_none()).await.unwrap();
    stack.shutdown().await;
}

// The window's own keys and Open in, on a stand-in agent's thread: a closed tab comes back with
// ⌘⇧T, and Open in hands the thread's folder to an editor through the app's one open seam, which
// a run points at a file (`SLOPTY_E2E_OPEN_LOG`) so nothing is launched.

/// The tab-close buttons in `dump`: one per tab in the right pane, each named for its tab.
fn tab_closes(dump: &slopty_e2e::Dump) -> Vec<[f32; 4]> {
    dump.a11y
        .iter()
        .filter(|n| {
            n.role == "Button" && n.label.as_deref().is_some_and(|l| l.starts_with("Close "))
        })
        .map(|n| n.bounds)
        .collect()
}

/// ⌘T, from the session's terminal, opens the right pane's new-tab menu; a Changes tab closed
/// comes back, in front, with ⌘⇧T.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_closed_tab_comes_back_with_cmd_shift_t() {
    let mut stack = agent_stack().await;
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    drv.keys("cmd-1").await.unwrap();
    drv.keys("cmd-t").await.unwrap();
    let picker = drv
        .wait_for("the pane's surfaces", STEP, |d| {
            d.a11y.iter().any(|n| n.role == "MenuItem" && n.label.as_deref() == Some("Changes"))
        })
        .await
        .unwrap();
    let diffs = picker
        .a11y
        .iter()
        .find(|n| n.role == "MenuItem" && n.label.as_deref() == Some("Changes"))
        .unwrap()
        .bounds;
    drv.click(diffs[0] + diffs[2] / 2.0, diffs[1] + diffs[3] / 2.0).await.unwrap();
    let open = drv.wait_for("the Changes tab", STEP, |d| !tab_closes(d).is_empty()).await.unwrap();
    let tabs = tab_closes(&open).len();
    let [x, y, w, h] = *tab_closes(&open).last().unwrap();
    drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
    drv.wait_for("the tab closed", STEP, |d| tab_closes(d).len() == tabs - 1).await.unwrap();
    drv.keys("cmd-shift-t").await.unwrap();
    let back = drv.wait_for("the tab back", STEP, |d| tab_closes(d).len() == tabs).await.unwrap();
    println!("A11Y reopened {}", slopty_e2e::frame_text(&back.a11y));
    stack.shutdown().await;
}

/// Open in, from the chat's titlebar, hands the thread's folder to the editor picked: on this
/// Mac as `open -a <app> <folder>`, on another machine as the editor's SSH link to it.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn open_in_hands_the_folder_to_an_editor() {
    const MACHINE: &str = "e2e-open-in";
    let scratch = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(scratch.path()).unwrap();
    let programs = root.join("programs");
    let home = root.join("home");
    let repo = root.join("my project");
    for dir in [&programs, &home, &repo] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::os::unix::fs::symlink(
        slopty_e2e::harness::stub_claude().await.unwrap(),
        programs.join("claude"),
    )
    .unwrap();
    let log = root.join("opened.log");
    let path = format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", programs.display());
    let turns = serde_json::json!({ PROMPT: [] }).to_string();
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("PATH", path.as_str()),
        ("STUB_TURNS", turns.as_str()),
        ("SLOPTY_E2E_OPEN_LOG", log.to_str().unwrap()),
    ];
    let mut stack = Stack::launch_with(MACHINE, &env).await.unwrap();
    let folder = repo.to_string_lossy().into_owned();
    let mut start = vec!["agent", "start", MACHINE, "--folder", &folder];
    start.extend(PROMPT.split(' '));
    let started =
        slopty_e2e::harness::slopty_json(stack.server.address(), &root.join("cli"), &start, b"")
            .await
            .unwrap();
    assert!(started["agent"].as_str().is_some_and(|id| !id.is_empty()), "{started}");

    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    // The thread reaches the sidebar once the worker publishes it: open its row when it is there.
    let open_in = |d: &slopty_e2e::Dump| {
        d.a11y.iter().any(|n| n.role == "Button" && n.label.as_deref() == Some("Open in…"))
    };
    let since = std::time::Instant::now();
    let titlebar = loop {
        drv.keys("cmd-1").await.unwrap();
        let dump = drv.dump().await.unwrap();
        if open_in(&dump) {
            break dump;
        }
        assert!(
            since.elapsed() < STEP,
            "the thread's Open in: {}",
            slopty_e2e::frame_text(&dump.a11y)
        );
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    };
    let [x, y, w, h] = titlebar
        .a11y
        .iter()
        .find(|n| n.role == "Button" && n.label.as_deref() == Some("Open in…"))
        .unwrap()
        .bounds;
    drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
    let editors = ["Finder", "Visual Studio Code", "Cursor", "Zed"];
    let menu = drv
        .wait_for("the editors", STEP, |d| {
            d.a11y.iter().any(|n| {
                n.role == "MenuItem" && n.label.as_deref().is_some_and(|l| editors.contains(&l))
            })
        })
        .await;
    let menu = match menu {
        Ok(menu) => menu,
        Err(error) => {
            // What the window showed instead, for the review.
            let shown = artifacts_dir().join("open-in-no-menu.png");
            let frame = drv.render(&shown).await.unwrap();
            panic!(
                "{error:#}\nthe frame ({}) said:\n{}",
                shown.display(),
                slopty_e2e::frame_text(&frame.a11y)
            );
        }
    };
    let offered: Vec<_> = menu
        .a11y
        .iter()
        .filter(|n| {
            n.role == "MenuItem" && n.label.as_deref().is_some_and(|l| editors.contains(&l))
        })
        .collect();
    let picked =
        offered.iter().find(|n| n.label.as_deref() == Some("Finder")).unwrap_or(&offered[0]);
    let app = picked.label.clone().unwrap();
    let [x, y, w, h] = picked.bounds;
    drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
    let since = std::time::Instant::now();
    let opened = loop {
        let opened = std::fs::read_to_string(&log).unwrap_or_default();
        if !opened.is_empty() {
            break opened;
        }
        assert!(since.elapsed() < STEP, "nothing was opened");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    let line = opened.lines().next().unwrap();
    println!("MEASURE open-in: {line}");
    let local = format!("open -a {app} {folder}");
    let remote = folder.replace(' ', "%20");
    assert!(line == local || line.ends_with(&remote), "{line:?} opens {folder:?} in {app}");
    assert_eq!(opened.lines().count(), 1, "one open: {opened}");
    stack.shutdown().await;
}

/// Answer one request on `stream`: the page at `/`, which names its icon, and the icon.
fn serve_page(mut stream: std::net::TcpStream, icon: &[u8]) {
    use std::io::{BufRead as _, BufReader, Write as _};
    let mut line = String::new();
    if BufReader::new(&stream).read_line(&mut line).is_err() {
        return;
    }
    let path = line.split_whitespace().nth(1).unwrap_or("/");
    let (kind, body): (&str, Vec<u8>) = if path.starts_with("/icon.png") {
        ("image/png", icon.to_vec())
    } else {
        (
            "text/html",
            b"<!doctype html><html><head><title>Icon page</title>\
              <link rel=\"icon\" href=\"/icon.png\"></head><body>An icon.</body></html>"
                .to_vec(),
        )
    };
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    // A browser that gave up on the page closed its end; nothing is left to answer.
    if let Err(gone) = stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(&body)) {
        eprintln!("the page's answer was not read: {gone}");
    }
}

/// A page that names its icon shows it on its tab: the web view reads it inside the page, and
/// the tab's icon is the page's, no longer the default globe.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_pages_icon_shows_on_its_tab() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let icon = {
        let red = image::RgbaImage::from_pixel(32, 32, image::Rgba([220, 40, 40, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        red.write_to(&mut png, image::ImageFormat::Png).unwrap();
        png.into_inner()
    };
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            serve_page(stream, &icon);
        }
    });
    let mut stack = agent_stack().await;
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    drv.keys("cmd-1").await.unwrap();
    drv.wait_for("the thread open", STEP, |d| d.a11y_node("Button", Some("Open in…")).is_some())
        .await
        .unwrap();
    // The right pane's new-tab menu (⌘T, from the session's terminal), its Browser, and the
    // page's address.
    drv.keys("cmd-t").await.unwrap();
    let picker = drv
        .wait_for("the pane's picker", STEP, |d| d.a11y_node("MenuItem", Some("Browser")).is_some())
        .await
        .unwrap();
    let [x, y, w, h] = picker.a11y_node("MenuItem", Some("Browser")).unwrap().bounds;
    drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    drv.type_text(&format!("http://127.0.0.1:{port}/")).await.unwrap();
    drv.keys("enter").await.unwrap();
    let shown = drv
        .wait_for("the page's icon on its tab", STEP, |d| {
            d.a11y_node("Image", Some("Page icon")).is_some()
        })
        .await
        .unwrap();
    let [_, _, w, h] = shown.a11y_node("Image", Some("Page icon")).unwrap().bounds;
    assert!((w - 14.0).abs() < 1.0 && (h - 14.0).abs() < 1.0, "the strip's 14 pt glyph: {w}x{h}");
    stack.shutdown().await;
}

/// A control's tooltip names its action and key, centred under it clear of the pointer
/// (`docs/decisions/ui.md`, "Tooltips name the action and its key"): the chat's files toggle.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_files_toggles_tooltip_names_its_key_under_it() {
    let mut stack = agent_stack().await;
    let dir = stack.dir.path().to_path_buf();
    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    drv.keys("cmd-1").await.unwrap();
    let toggle = |d: &slopty_e2e::Dump| {
        d.a11y
            .iter()
            .find(|n| n.role == "Button" && n.label.as_deref() == Some("Show files panel"))
            .map(|n| n.bounds)
    };
    // The chat's reply is in: its titlebar has every control it will have.
    let middle = |d: &slopty_e2e::Dump| {
        labelled(d, &format!("Done: {PROMPT}"))?;
        toggle(d).map(|[x, y, w, h]| (x + w / 2.0, y + h / 2.0))
    };
    let tip = rest_on(drv, "its tooltip", middle, |d| d.a11y_node("Tooltip", None).is_some()).await;
    let [x, y, w, h] = toggle(&tip).unwrap();
    let pointer = (x + w / 2.0, y + h / 2.0);
    let words = slopty_e2e::frame_text(&tip.a11y);
    assert!(words.contains("Show files panel") && words.contains("⌘E"), "{words}");
    let [tx, ty, tw, _] = tip.a11y_node("Tooltip", None).unwrap().bounds;
    assert!(ty >= y + h + 3.5, "under the control, the pointer clear of it: {ty} vs {}", y + h);
    // Centred under the control, or held 8 px inside the window where centring would cross it.
    let held = (tx - 8.0).abs() <= 0.5 || (tx + tw - (WINDOW.0 - 8.0)).abs() <= 0.5;
    assert!(((tx + tw / 2.0) - pointer.0).abs() <= 1.0 || held, "centred: {tx} {tw}");
    goldens_both(drv, &dir, "tooltip-files-toggle").await;
    stack.shutdown().await;
}
