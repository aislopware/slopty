//! A person's day in the app, walked to find what it shows wrongly, late or not at all: the
//! GUI twin of the CLI's `a_person_s_day_across_two_workers` (`apps/slopty-cli/tests/projects.rs`,
//! `.research/readiness-w-2026-10-12b.md`).
//!
//! A real server, two real workers ("studio", the app's own, and "linux-box") and the app; every
//! agent is `slopty-stub-claude` and `gh` a stand-in, so nobody's agent or forge is reached. The
//! orchestrator's own tool calls (`task_start`, a task's `task_report`) are made with the
//! `slopty` CLI, the verbs its tools send: the stand-in on one machine cannot tell an
//! orchestrator from a task's agent by its environment.
//!
//! The walk does not stop at the first thing it finds. Each miss is printed as `FINDING …` and
//! kept, each wait timed as `MEASURE …`, each state rendered to the artifacts as `day-*.png` for
//! review, and the test fails once at its end with the whole list
//! (`.research/readiness-a-2026-10-12.md`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use slopty_e2e::harness::{ProjectStack, artifacts_dir};
use slopty_e2e::snapshot::{MAC_TOLERANCE as TOLERANCE, assert_matches};
use slopty_e2e::{A11yNode, Command, Driver, Dump, ProjectInfo, frame_text};

/// How long one step may take before it is a finding: a round trip, an agent starting, a
/// clone, the app hearing of it.
const STEP: Duration = Duration::from_secs(15);
/// The renders' window.
const WINDOW: (f32, f32) = (1280.0, 800.0);
/// Where the work comes from, as the clones name it: `.invalid` never resolves, so a machine
/// that cannot reach the forge fails at once and no network is asked.
const FORGE_URL: &str = "https://forge.invalid/o/demo.git";
/// The goal the person writes.
const GOAL: &str = "Ship the Linux part and the Mac part";
/// What the Linux task's agent asks, and the answer the person picks on the board.
const QUESTION: &str = "Which file holds the Linux part?";
const ANSWER: &str = "linux.txt";

/// What the walk found.
#[derive(Default)]
struct Findings(Vec<String>);

impl Findings {
    /// `holds`, or `what` is found.
    fn check(&mut self, holds: bool, what: &str) -> bool {
        if !holds {
            eprintln!("FINDING {what}");
            self.0.push(what.to_owned());
        }
        holds
    }
}

/// `done` within `within`, polled, without failing: what it gave, if it did.
async fn soon<T>(within: Duration, mut done: impl AsyncFnMut() -> Option<T>) -> Option<T> {
    let waited = tokio::time::timeout(within, async {
        loop {
            if let Some(found) = done().await {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
    waited.await.ok()
}

/// The app's dump once `done` holds of it, within `within`; none (no panic) when it never does.
async fn seen(driver: &mut Driver, within: Duration, done: impl Fn(&Dump) -> bool) -> Option<Dump> {
    soon(within, async || driver.dump().await.ok().filter(|d| done(d))).await
}

/// Say how long `what` took.
fn measure(what: &str, since: Instant) {
    eprintln!("MEASURE {what}: {:?}", since.elapsed());
}

/// The first node labelled `label`, with `role` when given.
fn node<'a>(d: &'a Dump, role: Option<&str>, label: &str) -> Option<&'a A11yNode> {
    d.a11y.iter().find(|n| {
        role.is_none_or(|r| n.role == r) && n.label.as_deref() == Some(label) && n.bounds[2] > 0.0
    })
}

/// The first button whose label holds `words`.
fn button_with<'a>(d: &'a Dump, words: &str) -> Option<&'a A11yNode> {
    d.a11y
        .iter()
        .find(|n| n.role == "Button" && n.label.as_deref().is_some_and(|l| l.contains(words)))
}

/// Click the middle of `n`.
async fn click(driver: &mut Driver, node: &A11yNode) {
    let [left, top, wide, high] = node.bounds;
    driver.click(left + wide / 2.0, top + high / 2.0).await.unwrap();
}

/// What the window says, in words.
fn words(d: &Dump) -> String {
    frame_text(&d.a11y)
}

/// Render the window to the artifacts as `day-<name>.png`, for review.
async fn shot(driver: &mut Driver, name: &str) {
    let dir = artifacts_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("day-{name}.png"));
    if let Ok(frame) = driver.render(&path).await {
        std::fs::write(dir.join(format!("day-{name}.txt")), frame_text(&frame.a11y)).unwrap();
    }
}

/// `git -C dir args…` with nobody's config but `extra`, its output trimmed; panics on failure.
fn git(dir: &Path, extra: &[&str], args: &[&str]) -> String {
    let mut all = vec!["-c", "user.name=Mira", "-c", "user.email=mira@localhost"];
    all.extend_from_slice(extra);
    all.extend_from_slice(args);
    let ran = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(&all)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(ran.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&ran.stderr));
    String::from_utf8_lossy(&ran.stdout).trim().to_owned()
}

/// A home's git config reaching the forge at `forge` by [`FORGE_URL`], and the file Claude Code
/// keeps beside it.
fn reach_forge(home: &Path, forge: &Path) {
    std::fs::create_dir_all(home).unwrap();
    let config = format!("[url \"file://{}\"]\n\tinsteadOf = {FORGE_URL}\n", forge.display());
    std::fs::write(home.join(".gitconfig"), config).unwrap();
}

/// A stand-in `gh` in `dir` that notes each call in `log` and answers a pull request opened.
fn stand_in_gh(dir: &Path, log: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let view = r#"{"number":1,"url":"https://forge.invalid/o/demo/pull/1","title":"Ship",
"state":"OPEN","isDraft":false,"headRefName":"goal","headRefOid":"0123",
"baseRefName":"main","reviewDecision":"","mergeable":"MERGEABLE",
"mergeStateStatus":"CLEAN","statusCheckRollup":[]}"#;
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> \"{log}\"\ncase \"$1 $2\" in\n\
         'pr list') echo '[]' ;;\n\
         'pr create') echo 'https://forge.invalid/o/demo/pull/1' ;;\n\
         'pr view') echo '{view}' ;;\n\
         *) echo \"unexpected: $*\" >&2; exit 2 ;;\nesac\n",
        log = log.display(),
        view = view.replace('\n', ""),
    );
    let gh = dir.join("gh");
    std::fs::write(&gh, script).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    gh
}

/// A hook's payload: its event and `rest`.
fn hook(event: &str, rest: &Value) -> Value {
    let mut payload = json!({ "hook_event_name": event });
    if let (Some(payload), Some(rest)) = (payload.as_object_mut(), rest.as_object()) {
        payload.extend(rest.clone());
    }
    payload
}

/// The project the app mirrors as `id`.
fn project<'a>(d: &'a Dump, id: &str) -> Option<&'a ProjectInfo> {
    d.projects.iter().find(|p| p.id == id)
}

/// Task `task`'s state as the app shows it, in lower case (`blocked`, `done`, `merged`).
fn state_of(d: &Dump, id: &str, task: u32) -> Option<String> {
    project(d, id)?.tasks.iter().find(|(t, _)| *t == task).map(|(_, s)| s.to_lowercase())
}

/// Show project `id`'s board in its orchestrator's tile, as the person would: its terminal
/// brought forward, then ⌘⇧J, once. Whether it shows.
async fn to_board(driver: &mut Driver, id: &str) -> bool {
    let d = driver.dump().await.unwrap();
    let Some(p) = project(&d, id) else { return false };
    if p.shown {
        return true;
    }
    if let Some(orchestrator) = p.orchestrator.clone() {
        driver.reveal(&orchestrator).await.unwrap();
    }
    driver.keys("cmd-shift-j").await.unwrap();
    seen(driver, Duration::from_secs(6), |d| project(d, id).is_some_and(|p| p.shown))
        .await
        .is_some()
}

/// Open "New goal" from the title bar, write `goal`, move the folder to `folder` when given,
/// and create it. What the folder came filled with; none when the sheet never opened.
async fn new_goal(
    driver: &mut Driver,
    found: &mut Findings,
    goal: &str,
    folder: Option<&str>,
) -> Option<String> {
    let d = driver.dump().await.unwrap();
    let Some(open) = node(&d, Some("Button"), "New goal").cloned() else {
        found.check(false, "the title bar has no New goal");
        return None;
    };
    click(driver, &open).await;
    let opened = Instant::now();
    let sheet = seen(driver, STEP, |d| node(d, None, "Folder").is_some()).await;
    if !found.check(sheet.is_some(), "New goal opens no sheet with a folder") {
        return None;
    }
    measure("New goal to its sheet", opened);
    let sheet = sheet.unwrap_or_default();
    let filled = node(&sheet, None, "Folder").and_then(|n| n.value.clone()).unwrap_or_default();
    let keyed = sheet.a11y.iter().any(|n| n.focused && n.label.as_deref() == Some("Goal"));
    found.check(keyed, "the goal field does not have the keyboard as the sheet opens");
    if !keyed && let Some(field) = node(&sheet, None, "Goal").cloned() {
        click(driver, &field).await;
    }
    driver.type_text(goal).await.unwrap();
    if let Some(folder) = folder
        && let Some(field) = node(&sheet, None, "Folder").cloned()
    {
        click(driver, &field).await;
        driver.keys("cmd-a").await.unwrap();
        driver.type_text(folder).await.unwrap();
    }
    shot(driver, &format!("goal-sheet-{}", folder.map_or("filled", |_| "typed"))).await;
    let d = driver.dump().await.unwrap();
    if let Some(create) = node(&d, Some("Button"), "Create").cloned() {
        click(driver, &create).await;
    } else {
        found.check(false, "the goal sheet has no Create");
        driver.keys("enter").await.unwrap();
    }
    Some(filled)
}

/// The project made since `before` held these, and its id.
async fn made_project(driver: &mut Driver, before: &[String]) -> Option<String> {
    let d = seen(driver, STEP, |d| d.projects.iter().any(|p| !before.contains(&p.id))).await?;
    d.projects.iter().find(|p| !before.contains(&p.id)).map(|p| p.id.clone())
}

/// Walk ↓ on the shown board until it stands on `task`; whether it did.
async fn pick(driver: &mut Driver, id: &str, task: u32) -> bool {
    let want = task.to_string();
    for _ in 0..12 {
        let d = driver.dump().await.unwrap();
        if project(&d, id).and_then(|p| p.picked.as_deref()) == Some(want.as_str()) {
            return true;
        }
        driver.keys("down").await.unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
    false
}

/// The worktree a stand-in agent was started in, from what it recorded: its directory and the
/// `--worktree` it was given.
async fn worktree_of(record: &Path) -> Option<PathBuf> {
    soon(STEP, async || {
        let seen: Value = serde_json::from_slice(&std::fs::read(record).ok()?).ok()?;
        let argv: Vec<&str> = seen["argv"].as_array()?.iter().filter_map(Value::as_str).collect();
        let named = argv.iter().enumerate().find_map(|(at, arg)| {
            arg.strip_prefix("--worktree=").or_else(|| {
                (*arg == "--worktree").then(|| argv.get(at.saturating_add(1)).copied())?
            })
        })?;
        let cwd = PathBuf::from(seen["cwd"].as_str()?);
        let tree = cwd.join(".claude/worktrees").join(named);
        tree.exists().then_some(tree)
    })
    .await
}

/// The transcript of the Linux task's one turn, as Claude Code writes it: the brief, a Write of
/// `file`, its result and the answer.
fn turn_transcript(file: &Path) -> String {
    let path = file.to_string_lossy();
    let lines = [
        json!({ "type": "user", "timestamp": "2026-10-12T10:00:00.000Z",
            "message": { "role": "user", "content": "Write linux.txt." } }),
        json!({ "type": "assistant", "timestamp": "2026-10-12T10:00:02.000Z",
            "message": { "role": "assistant", "content": [
                { "type": "tool_use", "id": "toolu_w1", "name": "Write",
                  "input": { "file_path": path, "content": "done\n" } }] } }),
        json!({ "type": "user", "timestamp": "2026-10-12T10:00:03.000Z",
            "message": { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "toolu_w1",
                  "content": format!("File created successfully at: {path}") }] },
            "toolUseResult": { "type": "create", "filePath": path, "content": "done\n",
                "structuredPatch": [] } }),
        json!({ "type": "assistant", "timestamp": "2026-10-12T10:00:05.000Z",
            "message": { "role": "assistant", "stop_reason": "end_turn", "content": [
                { "type": "text", "text": "Wrote linux.txt." }] } }),
    ];
    lines.iter().fold(String::new(), |mut out, line| {
        out.push_str(&line.to_string());
        out.push('\n');
        out
    })
}

/// A person's day in the app across two machines (the module's doc).
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_person_s_day_in_the_app_across_two_workers() {
    let mut found = Findings::default();
    let tools = tempfile::tempdir().unwrap();
    let gh_log = tools.path().join("gh.log");
    let gh = stand_in_gh(tools.path(), &gh_log);
    let mut stack = ProjectStack::launch_with("studio", &[("gh", &gh)], &[]).await.unwrap();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let root = std::fs::canonicalize(stack.path("")).unwrap();

    // The forge and the studio's clone of it, in the studio's home as `~/demo`.
    let seed = root.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &[], &["init", "-q", "-b", "main"]);
    std::fs::write(seed.join("a.txt"), "one\n").unwrap();
    git(&seed, &[], &["add", "."]);
    git(&seed, &[], &["commit", "-q", "-m", "first"]);
    git(&root, &[], &["clone", "-q", "--bare", "seed", "forge.git"]);
    let forge = root.join("forge.git");
    let reach = format!("url.file://{}.insteadOf={FORGE_URL}", forge.display());
    let studio_home = root.join("home");
    git(&studio_home, &["-c", &reach], &["clone", "-q", FORGE_URL, "demo"]);
    reach_forge(&studio_home, &forge);
    std::fs::write(studio_home.join(".claude.json"), "{}").unwrap();
    let studio_clone = studio_home.join("demo");
    let person_main = git(&studio_clone, &[], &["rev-parse", "main"]);

    // A Linux box beside it, whose task's agent asks the person a question as it starts and,
    // once answered, ends a turn that wrote a file. Its home reaches no forge yet: its first
    // start fails late, at the clone.
    let linux_home = root.join("linux-home");
    std::fs::create_dir_all(&linux_home).unwrap();
    std::fs::write(linux_home.join(".claude.json"), "{}").unwrap();
    let transcript = root.join("linux-transcript.jsonl");
    std::fs::write(&transcript, "").unwrap();
    let said = json!({ "transcript_path": transcript });
    let question = json!({
        "questions": [{
            "question": QUESTION,
            "header": "File",
            "options": [{ "label": ANSWER }, { "label": "work.txt" }],
            "multiSelect": false,
        }],
    });
    let mut asked = said.clone();
    asked["tool_name"] = json!("AskUserQuestion");
    asked["tool_input"] = question;
    let hooks = json!([
        hook("SessionStart", &json!({ "source": "startup", "transcript_path": transcript })),
        hook("PermissionRequest", &asked),
        hook("PostToolUse", &asked),
    ]);
    let turn_gate = root.join("linux-turn");
    let later =
        json!([hook("Stop", &json!({ "stop_hook_active": false, "transcript_path": transcript }))]);
    let linux_record = root.join("linux-task.json");
    let (hooks, later) = (hooks.to_string(), later.to_string());
    let (gate, record) =
        (turn_gate.to_string_lossy().into_owned(), linux_record.to_string_lossy().into_owned());
    let added = Instant::now();
    let mut linux = stack
        .add_worker(
            "linux-box",
            &linux_home,
            &[
                ("STUB_HOOKS", &hooks),
                ("STUB_LATER", &later),
                ("STUB_LATER_AFTER", &gate),
                ("STUB_RECORD", &record),
            ],
        )
        .await
        .unwrap();
    let both = seen(&mut stack.driver, STEP, |d| {
        ["studio", "linux-box"]
            .iter()
            .all(|n| d.workers.iter().any(|w| w.name == *n && w.status == "connected"))
    })
    .await;
    found.check(both.is_some(), "the app never shows both machines connected");
    measure("a second machine added, to connected in the app", added);
    shot(&mut stack.driver, "two-machines").await;

    // The goal, from the GUI, in the folder the sheet comes filled with: the focused shell's.
    let before: Vec<String> =
        stack.driver.dump().await.unwrap().projects.iter().map(|p| p.id.clone()).collect();
    let filled = new_goal(&mut stack.driver, &mut found, GOAL, None).await.unwrap_or_default();
    eprintln!("the goal sheet came filled with the folder {filled:?}");
    let in_repo = filled.trim_end_matches('/').ends_with("demo");
    found.check(
        in_repo,
        &format!("New goal comes filled with {filled:?}, no repository: its tasks cannot start"),
    );
    let goal_made = Instant::now();
    let outcome = seen(&mut stack.driver, STEP, |d| {
        d.notice.is_some() || d.projects.iter().any(|p| !before.contains(&p.id))
    })
    .await
    .unwrap_or_default();
    measure("New goal's Create in a folder with no repository, to its outcome", goal_made);
    let made = outcome.projects.iter().any(|p| !before.contains(&p.id));
    let agents = outcome.items.iter().filter(|i| i.session.is_some()).count();
    eprintln!(
        "a goal with no repository: made {made}, notice {:?}, {agents} terminals",
        outcome.notice
    );
    found.check(
        made || outcome.notice.is_none(),
        &format!(
            "a goal in a folder with no repository starts its orchestrator, then is refused in \
             git's words ({:?}); the agent stays",
            outcome.notice
        ),
    );
    shot(&mut stack.driver, "goal-refused").await;

    // A project whose orchestrator works where no repository is (made by the CLI, as the GUI
    // refuses one): a task it starts is refused as unplaced, and the board says why in words.
    let home = studio_home.to_string_lossy().into_owned();
    let spawned = stack.slopty(&["agent", "spawn", "--cwd", &home]).await.unwrap();
    let term = spawned["term"].as_str().unwrap_or_default().to_owned();
    stack
        .slopty(&[
            "project",
            "create",
            "loose",
            "--title",
            "No repository",
            "--repo",
            "demo",
            "--target",
            "main",
            "--orchestrator",
            &term,
        ])
        .await
        .unwrap();
    let refused =
        stack.slopty(&["task", "start", "--project", "loose", "--title", "The Linux part"]).await;
    let why = refused.as_ref().err().map(|e| format!("{e:#}")).unwrap_or_default();
    eprintln!("a task in a project with no repository: {why}");
    found.check(
        why.contains("has not said yet"),
        &format!("the start was not refused as unplaced: {refused:?}"),
    );
    found
        .check(to_board(&mut stack.driver, "loose").await, "the loose project's board never shows");
    let shown =
        seen(&mut stack.driver, Duration::from_secs(5), |d| words(d).contains("has not said yet"))
            .await;
    found.check(shown.is_some(), "a task refused as unplaced says nothing on the board");
    shot(&mut stack.driver, "unplaced").await;

    // The goal again, in the clone.
    let before: Vec<String> =
        stack.driver.dump().await.unwrap().projects.iter().map(|p| p.id.clone()).collect();
    new_goal(&mut stack.driver, &mut found, GOAL, Some("~/demo")).await;
    let goal_made = Instant::now();
    let Some(owned) = made_project(&mut stack.driver, &before).await else {
        panic!("the goal in ~/demo made no project: {:#?}", found.0);
    };
    let id = owned.as_str();
    measure("New goal's Create to its project in the app", goal_made);
    let board = seen(&mut stack.driver, STEP, |d| project(d, id).is_some_and(|p| p.shown)).await;
    found.check(board.is_some(), "a new goal's board does not show by itself");
    measure("New goal's Create to its board", goal_made);
    let stale = board.as_ref().and_then(|d| d.notice.clone());
    found.check(
        stale.is_none(),
        &format!("the last goal's refusal still shows over the new goal's board: {stale:?}"),
    );
    let known = soon(STEP, async || {
        let status = stack.slopty(&["project", "status", id]).await.ok()?;
        status["project"]["repo_id"]["root"].is_string().then_some(())
    })
    .await;
    found.check(known.is_some(), "the project never learns its repository");
    shot(&mut stack.driver, "board-new").await;

    // The orchestrator starts a task on each machine. The Linux one fails late: its machine
    // cannot reach the forge.
    let asked_at = Instant::now();
    let linux_task = stack
        .slopty(&[
            "task",
            "start",
            "--project",
            id,
            "--title",
            "The Linux part",
            "--brief",
            "Write linux.txt.",
            "--worker",
            "linux-box",
        ])
        .await;
    found.check(linux_task.is_ok(), &format!("the Linux task is refused: {linux_task:?}"));
    let studio_task = stack
        .slopty(&[
            "task",
            "start",
            "--project",
            id,
            "--title",
            "The Mac part",
            "--brief",
            "Write mac.txt.",
            "--worker",
            "studio",
        ])
        .await;
    found.check(studio_task.is_ok(), &format!("the Mac task is refused: {studio_task:?}"));
    let filled =
        seen(&mut stack.driver, STEP, |d| project(d, id).is_some_and(|p| p.tasks.len() == 2)).await;
    found.check(filled.is_some(), "the board never shows both tasks");
    measure("two task starts to both on the board", asked_at);
    let said_why =
        seen(&mut stack.driver, STEP, |d| words(d).contains("Clone on linux-box failed")).await;
    found.check(said_why.is_some(), "the late start failure is not said on the board");
    measure("the late start failure to words on the board", asked_at);
    let lane = said_why.as_ref().and_then(|d| {
        project(d, id)?.lanes.iter().find(|(_, t)| t.contains(&1)).map(|(l, _)| l.clone())
    });
    eprintln!("a start that failed stands in the lane {lane:?}");
    found.check(
        lane.as_deref().is_none_or(|l| !l.to_lowercase().contains("next")),
        &format!("a start that failed waits in {lane:?}, as if it were queued"),
    );
    shot(&mut stack.driver, "late-failure").await;
    // The board with the start that failed: its mark, why, and "Start again" (the golden).
    let path = stack.path("project-start-failed.png");
    if let Ok(frame) = stack.driver.render(&path).await {
        let held = assert_matches("project-start-failed", &frame, TOLERANCE, &artifacts_dir());
        found.check(held.is_ok(), &format!("the failed start's board moved: {held:?}"));
    }

    // The person makes the box reach the forge and starts the task again from the board.
    reach_forge(&linux_home, &forge);
    let d = stack.driver.dump().await.unwrap();
    let again = ["Start again", "Start elsewhere", "Retry", "Start"]
        .iter()
        .find_map(|w| button_with(&d, w).cloned());
    let restarted = Instant::now();
    if let Some(again) = again {
        eprintln!("the board starts it again with {:?}", again.label);
        click(&mut stack.driver, &again).await;
    } else {
        found.check(false, "a task whose start failed shows no way to start again");
        if pick(&mut stack.driver, id, 1).await {
            stack.driver.keys("s").await.unwrap();
        } else {
            found.check(false, "↓ never stands on the failed task");
        }
    }
    let going = soon(STEP, async || std::fs::metadata(&linux_record).ok()).await;
    found.check(going.is_some(), "started again from the board, the Linux task's agent never runs");
    measure("a start again from the board to its agent running", restarted);

    // The Linux task's question waits on the board, and is answered there.
    let question_up = seen(&mut stack.driver, STEP, |d| button_with(d, ANSWER).is_some()).await;
    if found
        .check(question_up.is_some(), "the Linux task's question is never answerable on the board")
    {
        measure("a start again to its question on the board", restarted);
        shot(&mut stack.driver, "question").await;
        let d = question_up.unwrap_or_default();
        found.check(
            words(&d).contains(QUESTION),
            "the board shows the answers but not the question",
        );
        if let Some(answer) = button_with(&d, ANSWER).cloned() {
            let answered = Instant::now();
            click(&mut stack.driver, &answer).await;
            let heard = soon(STEP, async || {
                let seen: Value =
                    serde_json::from_slice(&std::fs::read(&linux_record).ok()?).ok()?;
                let asked =
                    seen["hooks"].as_array()?.iter().find(|h| h["event"] == "PermissionRequest")?;
                asked["outputs"].to_string().contains("allow").then_some(())
            })
            .await;
            found.check(heard.is_some(), "the board's answer never reaches the agent");
            measure("the board's answer to the agent", answered);
            let off = seen(&mut stack.driver, STEP, |d| {
                state_of(d, id, 1).is_some_and(|s| s != "blocked")
                    && button_with(d, ANSWER).is_none()
            })
            .await;
            found.check(off.is_some(), "answered, the card still waits on the person");
        }
    } else if pick(&mut stack.driver, id, 1).await {
        shot(&mut stack.driver, "no-question").await;
    }

    // Its worker restarts mid-task, right after the answer.
    let restart_at = Instant::now();
    linux.restart_worker().await.unwrap();
    let away_or_back = seen(&mut stack.driver, STEP, |d| {
        d.workers.iter().any(|w| w.name == "linux-box" && w.status == "connected")
    })
    .await;
    found.check(away_or_back.is_some(), "the box's worker restarted never reads connected again");
    measure("the box's worker restarted, to connected in the app", restart_at);
    let d = stack.driver.dump().await.unwrap();
    found.check(
        state_of(&d, id, 1).is_some_and(|s| s != "failed"),
        "its worker restarted, the Linux task reads failed",
    );

    // The Linux agent's turn writes linux.txt and ends; its tile lists the file in its activity
    // bar, whose Review opens the change.
    let linux_tree = worktree_of(&linux_record).await;
    found.check(linux_tree.is_some(), "the Linux task's agent runs in no worktree");
    if let Some(tree) = &linux_tree {
        let file = tree.join("linux.txt");
        std::fs::write(&file, "done\n").unwrap();
        std::fs::write(&transcript, turn_transcript(&file)).unwrap();
    }
    std::fs::write(&turn_gate, "").unwrap();
    let opened = Instant::now();
    if pick(&mut stack.driver, id, 1).await {
        stack.driver.keys("enter").await.unwrap();
    } else {
        found.check(false, "↓ never stands on the Linux task's card");
    }
    let listed = seen(&mut stack.driver, STEP, |d| {
        d.a11y
            .iter()
            .any(|n| n.label.as_deref().is_some_and(|l| l.contains("Review") && l.contains(ANSWER)))
    })
    .await;
    found.check(
        listed.is_some(),
        "the Linux agent's tile never lists linux.txt in its activity bar",
    );
    measure("the agent's tile opened, to its edited files listed", opened);
    shot(&mut stack.driver, "edited-files").await;
    if let Some(d) = listed {
        let row = d
            .a11y
            .iter()
            .find(|n| {
                n.label.as_deref().is_some_and(|l| l.contains("Review") && l.contains(ANSWER))
            })
            .cloned();
        if let Some(row) = row {
            let reviewing = Instant::now();
            click(&mut stack.driver, &row).await;
            let review = seen(&mut stack.driver, STEP, |d| {
                d.items.iter().any(|i| i.kind == "review" && i.active)
            })
            .await;
            found.check(review.is_some(), "Review from the edited files opens no review");
            let shows = seen(&mut stack.driver, STEP, |d| words(d).contains(ANSWER)).await;
            found.check(shows.is_some(), "the review does not show linux.txt");
            measure("the edited file's Review to its review", reviewing);
            shot(&mut stack.driver, "review").await;
        }
    }

    // Both commit and report; the board shows both ready.
    if let Some(tree) = &linux_tree {
        git(tree, &[], &["add", "."]);
        git(tree, &[], &["commit", "-q", "-m", "linux"]);
    }
    let studio_tree = soon(STEP, async || {
        let listed = git(&studio_clone, &[], &["worktree", "list", "--porcelain"]);
        listed
            .lines()
            .filter_map(|l| l.strip_prefix("worktree "))
            .map(PathBuf::from)
            .find(|p| p.to_string_lossy().ends_with("-2"))
    })
    .await;
    found.check(studio_tree.is_some(), "the Mac task's agent runs in no worktree of the clone");
    if let Some(tree) = &studio_tree {
        std::fs::write(tree.join("mac.txt"), "done\n").unwrap();
        git(tree, &[], &["add", "."]);
        git(tree, &[], &["commit", "-q", "-m", "mac"]);
    }
    let reported = Instant::now();
    for (task, tree) in [(1, &linux_tree), (2, &studio_tree)] {
        let branch = tree.as_ref().map(|t| git(t, &[], &["branch", "--show-current"]));
        let task = task.to_string();
        let mut args =
            vec!["task", "report", "--project", id, "--task", task.as_str(), "--note", "Wrote it."];
        if let Some(branch) = &branch {
            args.extend_from_slice(&["--branch", branch.as_str()]);
        }
        let took = stack.slopty(&args).await;
        found.check(took.is_ok(), &format!("task {task}'s report is refused: {took:?}"));
    }
    let ready = seen(&mut stack.driver, STEP * 2, |d| {
        [1, 2].iter().all(|t| state_of(d, id, *t).is_some_and(|s| s == "done"))
    })
    .await;
    found.check(ready.is_some(), "the board never shows both tasks done");
    measure("both reports to both done on the board", reported);
    found.check(to_board(&mut stack.driver, id).await, "the board cannot be brought back");
    shot(&mut stack.driver, "both-ready").await;

    // Merged from the board, one after the other.
    for task in [1, 2] {
        let merging = Instant::now();
        if !pick(&mut stack.driver, id, task).await {
            found.check(false, &format!("↓ never stands on task {task}'s card to merge it"));
            continue;
        }
        let d = stack.driver.dump().await.unwrap();
        if let Some(merge) = node(&d, Some("Button"), "Merge").cloned() {
            click(&mut stack.driver, &merge).await;
        } else {
            found.check(false, &format!("task {task}'s card offers no Merge button; m merges"));
            stack.driver.keys("m").await.unwrap();
        }
        let merged = seen(&mut stack.driver, STEP * 2, |d| {
            state_of(d, id, task).is_some_and(|s| s == "merged")
        })
        .await;
        found.check(merged.is_some(), &format!("task {task} never shows merged"));
        measure(&format!("Merge of task {task} to merged on the board"), merging);
    }
    let main_now = git(&studio_clone, &[], &["rev-parse", "main"]);
    found.check(main_now == person_main, "the person's main moved");
    shot(&mut stack.driver, "merged").await;

    // The goal lands as a pull request, from the board's head.
    let d = stack.driver.dump().await.unwrap();
    match node(&d, Some("Button"), "Open pull request").cloned() {
        Some(open) => {
            let landing = Instant::now();
            click(&mut stack.driver, &open).await;
            let pulled = seen(&mut stack.driver, STEP, |d| words(d).contains("Pull request")).await;
            found.check(pulled.is_some(), "the board never shows the goal's pull request");
            measure("Open pull request to its line on the board", landing);
            let asked = std::fs::read_to_string(&gh_log).unwrap_or_default();
            found.check(
                asked.contains("pr create"),
                &format!("gh was never asked to open it: {asked}"),
            );
        }
        None => {
            found.check(false, "the finished goal's board offers no Open pull request");
        }
    }
    shot(&mut stack.driver, "landed").await;

    // The same state on a second device.
    let second = Instant::now();
    let (app2, mut driver2) = stack.add_app("app2").await.unwrap();
    driver2.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let mine = project(&stack.driver.dump().await.unwrap(), id).cloned().unwrap_or_default();
    let same = seen(&mut driver2, STEP, |d| {
        project(d, id).is_some_and(|p| p.tasks == mine.tasks && p.lanes == mine.lanes)
    })
    .await;
    found.check(
        same.is_some(),
        &format!("a second device never shows the same board: {:?}", mine.lanes),
    );
    measure("a second device started, to the same board", second);
    shot(&mut driver2, "second-device").await;
    ProjectStack::quit_app(app2, driver2).await;

    linux.shutdown().await;
    stack.shutdown().await;
    assert!(found.0.is_empty(), "the day found {} things: {:#?}", found.0.len(), found.0);
}

/// A task whose machine cannot reach the forge fails its start after its steps, and the board
/// says so where it is read: under *Failed*, the clone's failure as the row's second line, and
/// "Start again" on the row. The orchestrator works in the studio's clone; the task goes to a
/// second machine whose home reaches no forge. Held as `project-start-failed`.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_start_that_failed_says_why_on_the_board() {
    let mut stack = ProjectStack::launch("studio").await.unwrap();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let root = std::fs::canonicalize(stack.path("")).unwrap();
    let seed = root.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &[], &["init", "-q", "-b", "main"]);
    std::fs::write(seed.join("a.txt"), "one\n").unwrap();
    git(&seed, &[], &["add", "."]);
    git(&seed, &[], &["commit", "-q", "-m", "first"]);
    git(&root, &[], &["clone", "-q", "--bare", "seed", "forge.git"]);
    let forge = root.join("forge.git");
    let reach = format!("url.file://{}.insteadOf={FORGE_URL}", forge.display());
    let home = root.join("home");
    git(&home, &["-c", &reach], &["clone", "-q", FORGE_URL, "demo"]);
    reach_forge(&home, &forge);
    let linux_home = root.join("linux-home");
    let linux = stack.add_worker("linux-box", &linux_home, &[]).await.unwrap();

    let clone = home.join("demo").to_string_lossy().into_owned();
    let spawned = stack.slopty(&["agent", "spawn", "--cwd", &clone]).await.unwrap();
    let orchestrator = spawned["term"].as_str().unwrap().to_owned();
    stack
        .slopty(&[
            "project",
            "create",
            "board",
            "--title",
            "Ship the Linux part",
            "--repo",
            &clone,
            "--target",
            "main",
            "--orchestrator",
            &orchestrator,
        ])
        .await
        .unwrap();
    let known = soon(STEP, async || {
        let status = stack.slopty(&["project", "status", "board"]).await.ok()?;
        status["project"]["repo_id"]["root"].is_string().then_some(())
    })
    .await;
    assert!(known.is_some(), "the project learns its repository");
    stack
        .slopty(&[
            "task",
            "start",
            "--project",
            "board",
            "--title",
            "The Linux part",
            "--worker",
            "linux-box",
        ])
        .await
        .unwrap();
    let said = seen(&mut stack.driver, STEP, |d| {
        project(d, "board")
            .is_some_and(|p| p.lanes.iter().any(|(l, t)| l.to_lowercase() == "failed" && t == &[1]))
    })
    .await;
    assert!(said.is_some(), "the failed start stands in Failed");
    assert!(to_board(&mut stack.driver, "board").await, "the board shows");
    let d = seen(&mut stack.driver, STEP, |d| button_with(d, "Start again").is_some())
        .await
        .expect("Start again on the row");
    assert!(words(&d).contains("Clone on linux-box failed"), "{}", words(&d));
    stack.driver.ok(&Command::PinClock { at_ms: Some(PINNED_AT) }).await.unwrap();
    stack.driver.ok(&Command::Move { x: 1.0, y: 1.0 }).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let frame = stack.driver.render(&stack.path("project-start-failed.png")).await.unwrap();
    assert_matches("project-start-failed", &frame, TOLERANCE, &artifacts_dir()).unwrap();
    linux.shutdown().await;
    stack.shutdown().await;
}

/// Where a golden that shows a time pins the readouts: 2026-10-04 at 09:00 UTC.
const PINNED_AT: u64 = 1_791_104_400_000;
