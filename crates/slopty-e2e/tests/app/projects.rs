//! A project's board in the real app, following a real server: the orchestrator and a task's
//! agent are `slopty-stub-claude` started by the server on a real worker, the project and its
//! tasks are made and moved with the `slopty` CLI as an orchestrator's tools would, and the app
//! shows it all in the orchestrator's tile. No real Claude Code runs.

use std::os::unix::fs::PermissionsExt as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use slopty_e2e::harness::{ProjectStack, artifacts_dir};
use slopty_e2e::snapshot::{MAC_TOLERANCE as TOLERANCE, assert_matches};
use slopty_e2e::{Command, Dump, ProjectInfo};

/// A server round trip, an agent starting, the app hearing of it.
const STEP: Duration = Duration::from_secs(30);
/// The renders' window: the board and the agent's tile beside it.
const WINDOW: (f32, f32) = (1280.0, 800.0);
const PROJECT: &str = "board";

fn project(d: &Dump) -> Option<&ProjectInfo> {
    d.projects.iter().find(|p| p.id == PROJECT)
}

fn term(opened: &Value) -> String {
    opened["term"].as_str().unwrap_or_else(|| panic!("a terminal: {opened}")).to_owned()
}

fn session_of(term: &str) -> String {
    term.rsplit_once('/').map_or(term, |(_, s)| s).to_owned()
}

/// The session a started task's terminal runs, from the task `slopty task start` printed.
fn started_session(task: &Value) -> String {
    session_of(&term(task))
}

/// Make a task that waits to be started: the start a task makes is refused while a task it
/// depends on is not done, and the task is kept, as an orchestrator's would be. Then `deps`
/// are what it depends on for good: none, or the ones named.
async fn planned(stack: &ProjectStack, title: &str, extra: &[&str], deps: &[&str]) {
    let mut start = vec!["task", "start", "--project", PROJECT, "--title", title];
    start.extend_from_slice(extra);
    start.extend_from_slice(&["--depends-on", "1"]);
    let refused = stack.slopty(&start).await.expect_err("a start that waits on task 1");
    assert!(refused.to_string().contains("did not start"), "{refused:#}");
    let made = stack.slopty(&["project", "status", PROJECT]).await.unwrap();
    let id = made["tasks"]
        .as_array()
        .and_then(|tasks| tasks.iter().rev().find(|t| t["title"] == title))
        .and_then(|t| t["task"].as_u64())
        .unwrap_or_else(|| panic!("{title} kept: {made}"))
        .to_string();
    let mut update = vec!["task", "update", "--project", PROJECT, "--task", &id];
    if deps.is_empty() {
        update.push("--no-dependencies");
    }
    for dep in deps {
        update.extend_from_slice(&["--depends-on", dep]);
    }
    stack.slopty(&update).await.unwrap();
}

/// Wait until the worker says Claude Code is installed: an agent's task goes only to a worker
/// known to have it, and a worker says so once its facts are gathered.
async fn claude_installed(stack: &ProjectStack) {
    let started = tokio::time::Instant::now();
    loop {
        let workers = stack.slopty(&["workers"]).await.unwrap();
        let listed = workers.as_array().into_iter().flatten();
        if listed.into_iter().any(|w| w["facts"]["agents"]["claude"].is_string()) {
            return;
        }
        assert!(started.elapsed() < STEP, "Claude Code on the worker: {workers}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Render the window as `name` and hold it to its golden.
async fn golden(stack: &mut ProjectStack, name: &str) {
    let frame = stack.driver.render(&stack.path(&format!("{name}.png"))).await.unwrap();
    assert_matches(name, &frame, TOLERANCE, &artifacts_dir()).unwrap();
}

/// The server's project, with an orchestrator and six tasks one level under it in five states,
/// one of them run by an agent the server started, one whose verifier broke and one waiting to
/// merge, reaches the app; ⇧⌘J turns the orchestrator's tile to its board, the lenses are keys,
/// ↓↓↩ opens the task's agent, and a change on the server moves the board while it shows.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_project_board_follows_its_orchestration() {
    let mut stack = ProjectStack::launch("studio").await.unwrap();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let repo = stack.path("repo").to_string_lossy().into_owned();

    let orchestrator = term(&stack.slopty(&["agent", "spawn", "--cwd", &repo]).await.unwrap());
    let orchestrator_session = session_of(&orchestrator);
    stack
        .slopty(&[
            "project",
            "create",
            PROJECT,
            "--title",
            "Ship the project board",
            "--repo",
            "slopty",
            "--verifier",
            "cargo gate",
            "--orchestrator",
            &orchestrator,
        ])
        .await
        .unwrap();
    claude_installed(&stack).await;
    let started = stack
        .slopty(&[
            "task",
            "start",
            "--project",
            PROJECT,
            "--title",
            "Mirror the server's projects",
            "--cwd",
            &repo,
        ])
        .await
        .unwrap();
    planned(&stack, "Draw the lanes", &[], &[]).await;
    planned(&stack, "Hold it to goldens", &["--read-only"], &["1"]).await;
    planned(&stack, "Write the decision", &[], &[]).await;
    planned(&stack, "Check the wire goldens", &[], &[]).await;
    planned(&stack, "Mirror the merge queue", &[], &[]).await;
    let update = |task: &'static str, more: &'static [&'static str]| {
        let mut args = vec!["task", "update", "--project", PROJECT, "--task", task];
        args.extend_from_slice(more);
        args
    };
    stack
        .slopty(&update(
            "1",
            &["--branch", "slopty/board/1", "--status", "Reading the snapshot parts"],
        ))
        .await
        .unwrap();
    stack
        .slopty(&update("2", &["--state", "blocked", "--status", "Which lanes come first?"]))
        .await
        .unwrap();
    stack
        .slopty(&update(
            "4",
            &[
                "--state",
                "done",
                "--passed",
                "--summary",
                "gate passed",
                "--head",
                "4a7aa6d",
                "--base",
                "c08d4c1",
            ],
        ))
        .await
        .unwrap();
    stack.slopty(&update("4", &["--state", "merged"])).await.unwrap();
    // A verifier that broke stays on its task until it is judged again, and one that passed
    // waits on the person to merge.
    stack
        .slopty(&update(
            "5",
            &[
                "--failed",
                "--summary",
                "   Compiling slopty-proto\nerror[E0063]: missing field `push` in initializer\n  --> crates/slopty-proto/tests/golden_project.rs:88:5\nerror: could not compile `slopty-proto`",
                "--head",
                "9c1e2f3",
                "--base",
                "c08d4c1",
            ],
        ))
        .await
        .unwrap();
    stack
        .slopty(&update(
            "6",
            &["--state", "done", "--passed", "--head", "e5b0d17", "--base", "c08d4c1"],
        ))
        .await
        .unwrap();

    // The first task's own shell is a tile too: its agent is the terminal its start named.
    let agent_session = started_session(&started);
    let d = stack
        .driver
        .wait_for("the project and its agent in the app", STEP, |d| {
            let agent =
                d.items.iter().any(|i| i.session.as_deref() == Some(agent_session.as_str()));
            project(d)
                .is_some_and(|p| p.tasks.len() == 6 && p.lanes.iter().any(|(l, _)| l == "merged"))
                && agent
        })
        .await
        .unwrap();
    let info = project(&d).unwrap();
    assert_eq!(info.orchestrator.as_deref(), Some(orchestrator_session.as_str()));
    assert!(!info.shown, "the terminal is the default");

    stack.driver.reveal(&orchestrator_session).await.unwrap();
    stack.driver.keys("cmd-shift-j").await.unwrap();
    let project_focus = format!("project:{PROJECT}");
    stack
        .driver
        .wait_for("the board with the keyboard", STEP, |d| {
            project(d).is_some_and(|p| p.shown) && d.focused == project_focus
        })
        .await
        .unwrap();
    golden(&mut stack, "project-tree").await;

    stack.driver.keys("2").await.unwrap();
    stack
        .driver
        .wait_for("the lanes", STEP, |d| {
            project(d).and_then(|p| p.lens.clone()).as_deref() == Some("Board")
        })
        .await
        .unwrap();
    golden(&mut stack, "project-lanes").await;
    stack.set_appearance("dark").unwrap();
    stack.driver.wait_for("the dark theme", STEP, |d| d.dark).await.unwrap();
    golden(&mut stack, "project-lanes-dark").await;
    stack.set_appearance("light").unwrap();
    stack.driver.wait_for("the light theme", STEP, |d| !d.dark).await.unwrap();

    stack.driver.keys("3").await.unwrap();
    stack
        .driver
        .wait_for("the timeline", STEP, |d| {
            project(d).and_then(|p| p.lens.clone()).as_deref() == Some("Timeline")
        })
        .await
        .unwrap();
    golden(&mut stack, "project-timeline").await;

    // A change on the server moves the board while it shows.
    stack.driver.keys("1").await.unwrap();
    stack.slopty(&update("3", &["--state", "failed", "--note", "no golden yet"])).await.unwrap();
    stack
        .driver
        .wait_for("task 3 failed", STEP, |d| {
            project(d).is_some_and(|p| p.lanes.iter().any(|(l, t)| l == "failed" && t == &[3]))
        })
        .await
        .unwrap();

    // ↓ stands on the orchestrator, ↓ again on task 1, ↩ opens its agent: its tile is the
    // active one and has the keyboard, on its terminal or its conversation, whichever it shows.
    stack.driver.keys("down down enter").await.unwrap();
    let d = stack
        .driver
        .wait_for("task 1's agent with the keyboard", STEP, |d| {
            let active = d
                .items
                .iter()
                .any(|i| i.active && i.session.as_deref() == Some(agent_session.as_str()));
            let board = d.focused.starts_with("project:");
            active && !board && !matches!(d.focused.as_str(), "workspace" | "none")
        })
        .await
        .unwrap();
    assert_eq!(project(&d).and_then(|p| p.picked.clone()).as_deref(), Some("1"));

    stack.shutdown().await;
}

/// How long the live task's agent works before the renders: past a minute, which is when its
/// time shows, and short of the second.
const AT_WORK: Duration = Duration::from_secs(75);

/// A live task as the board shows it: its agent at work for over a minute, its pull request's
/// own checks failing as the worker's `gh` reports them, and its reviewer on the forge asking
/// for changes. Its row says the time, the board offers Fix CI and Address comments, and its
/// card draws the pipeline and where it runs. `gh` is a stand-in on the worker's `PATH`; no
/// forge is asked.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_live_task_shows_its_checks_its_time_and_its_next_steps() {
    let mut stack = ProjectStack::launch("studio").await.unwrap();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let gh = stack.path("programs").join("gh");
    let listed = r#"[{"name":"clippy (macos)","bucket":"fail"},{"name":"test (macos)","bucket":"pass"},{"name":"test (linux)","bucket":"pass"},{"name":"golden","bucket":"pass"}]"#;
    // `gh pr checks` ends 1 once a check failed, with its JSON all the same.
    std::fs::write(&gh, format!("#!/bin/sh\nprintf '%s' '{listed}'\nexit 1\n")).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let repo = stack.path("repo").to_string_lossy().into_owned();
    let tree = stack.path("repo/.claude/worktrees/slopty-board-1");
    std::fs::create_dir_all(&tree).unwrap();

    let orchestrator = term(&stack.slopty(&["agent", "spawn", "--cwd", &repo]).await.unwrap());
    let orchestrator_session = session_of(&orchestrator);
    stack
        .slopty(&[
            "project",
            "create",
            PROJECT,
            "--title",
            "Ship the project board",
            "--repo",
            "slopty",
            "--verifier",
            "cargo gate",
            "--orchestrator",
            &orchestrator,
        ])
        .await
        .unwrap();
    let hooks = serde_json::json!([
        { "hook_event_name": "SessionStart", "source": "startup" },
        { "hook_event_name": "UserPromptSubmit", "prompt": "Read the checks with gh" },
        {
            "hook_event_name": "Statusline",
            "worktree": {
                "name": "slopty-board-1",
                "path": tree.to_string_lossy(),
                "branch": "worktree-slopty-board-1",
                "original_cwd": repo,
                "original_branch": "main",
            },
            // As the relay posts the status line's, in the wire's own shape.
            "pr": {
                "number": 42,
                "url": "https://github.com/aislopware/slopty/pull/42",
                "review": "ChangesRequested",
                "merge_request": false,
            },
        },
    ]);
    let env = format!("STUB_HOOKS={hooks}");
    claude_installed(&stack).await;
    let started = stack
        .slopty(&[
            "task",
            "start",
            "--project",
            PROJECT,
            "--title",
            "Read a pull request's checks",
            "--cwd",
            &repo,
            "--env",
            &env,
        ])
        .await
        .unwrap();
    let agent_session = started_session(&started);
    planned(&stack, "Draw the pipeline row", &[], &[]).await;

    // The server reads the checks on its own round, and counts the agent's time from its
    // prompt.
    let started = tokio::time::Instant::now();
    let failing = loop {
        let status = stack.slopty(&["project", "status", PROJECT]).await.unwrap();
        let task = &status["tasks"][0];
        if task["checks"]["state"] == "failing" && task["at_work_since_ms"].is_u64() {
            break status;
        }
        assert!(started.elapsed() < STEP, "checks and time on the server: {status}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    assert_eq!(failing["tasks"][0]["checks"]["failing"], serde_json::json!(["clippy (macos)"]));
    let since = failing["tasks"][0]["at_work_since_ms"].as_u64().unwrap();
    let at_work = || {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        now.saturating_sub(Duration::from_millis(since))
    };
    let kept = failing["timeline"].as_array().map_or(0, Vec::len);
    let d = stack
        .driver
        .wait_for("the checks and the agent in the app", STEP, |d| {
            let agent =
                d.items.iter().any(|i| i.session.as_deref() == Some(agent_session.as_str()));
            project(d).is_some_and(|p| p.timeline >= kept) && agent
        })
        .await
        .unwrap();
    assert!(project(&d).is_some_and(|p| p.tasks.len() == 2));

    stack.driver.reveal(&orchestrator_session).await.unwrap();
    stack.driver.keys("cmd-shift-j").await.unwrap();
    stack
        .driver
        .wait_for("the board", STEP, |d| project(d).is_some_and(|p| p.shown))
        .await
        .unwrap();
    // Time shows from a minute at work, and says "1m" until the second.
    tokio::time::sleep(AT_WORK.saturating_sub(at_work())).await;
    let picked = |want: &'static str| {
        move |d: &Dump| project(d).and_then(|p| p.lens.clone()).as_deref() == Some(want)
    };
    // The board's tile takes the keys again, whatever the agent's own tile did meanwhile.
    stack.driver.reveal(&orchestrator_session).await.unwrap();
    stack
        .driver
        .wait_for("the board with the keyboard", STEP, |d| d.focused.starts_with("project:"))
        .await
        .unwrap();
    // Each render follows a lens turned to, so it is a frame drawn after the minute.
    stack.driver.keys("2").await.unwrap();
    stack.driver.wait_for("the lanes", STEP, picked("Board")).await.unwrap();
    golden(&mut stack, "project-live-lanes").await;
    stack.driver.keys("1").await.unwrap();
    stack.driver.wait_for("the tree", STEP, picked("Tree")).await.unwrap();
    golden(&mut stack, "project-live-tree").await;
    assert!(at_work() < Duration::from_secs(118), "rendered within its first 2 minutes at work");

    stack.shutdown().await;
}
