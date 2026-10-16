//! The transcript of a real turn on the real app: a stand-in Claude Code (`slopty-stub-claude`)
//! plays a recorded Claude Code conversation (`STUB_REPLAY`, `slopty_testkit::replay`) as its
//! own turn on the stack's worker, so the shell draws what Claude Code really writes: tool calls
//! grouped under one summary, a task list, a subagent, and a permission request in the
//! composer. Every state is a golden in light and in dark, and its words are read from the
//! accessibility tree.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use slopty_e2e::harness::{ServedApp, artifacts_dir};
use slopty_e2e::snapshot::{MAC_TOLERANCE as TOLERANCE, assert_matches};
use slopty_e2e::{A11yNode, Command, Driver, Dump};

/// How long a worker round trip may take.
const STEP: Duration = Duration::from_secs(30);
/// How long a recorded turn may take to play, streamed step by step over the drive.
const TURN: Duration = Duration::from_secs(90);
/// The shell's main window as it first opens.
const WINDOW: (f32, f32) = (1320.0, 880.0);
/// Time for what the accessibility tree cannot show (a fold's tween, a row's reveal) to settle.
const SETTLE: Duration = Duration::from_secs(2);
/// The worker's name, as the CLI names the machine.
const MACHINE: &str = "transcript";

/// The prompt that plays the `tools` recording: tasks made and worked, a glob, a grep, three
/// commands (one fails, one runs in the background), a file written, a subagent that reads.
const TOOLS: &str = "Survey the files";
/// The prompt that plays the `permission` recording: three commands, each asked for first.
const PERMISSION: &str = "Touch three files";

/// What the `tools` turn's group says of its calls, as zeron counts them.
const GROUP: [&str; 6] = [
    "Ran 3 commands",
    "edited 1 file",
    "searched 2 times",
    "updated todos",
    "called 1 tool",
    "1 failed",
];

/// What the recorded agent worked on.
const MAIN_RS: &str = "fn main() {\n    println!(\"hello\");\n}\n";

/// Render the window in light and in dark and hold each against `golden/<name>-<appearance>.png`;
/// the window is light again after. The frame's words, as the light render said them.
async fn goldens(drv: &mut Driver, renders: &Path, name: &str) -> String {
    let mut words = String::new();
    for (dark, appearance) in [(false, "light"), (true, "dark")] {
        drv.ok(&Command::Appearance { dark }).await.unwrap();
        // The appearance's first frame, and its hover and fade tweens settled.
        tokio::time::sleep(Duration::from_millis(600)).await;
        let golden = format!("{name}-{appearance}");
        let frame = drv.render(&renders.join(format!("{golden}.png"))).await.unwrap();
        let text = slopty_e2e::frame_text(&frame.a11y);
        println!("A11Y {golden}\n{text}");
        if !dark {
            words = text;
        }
        assert_matches(&golden, &frame, TOLERANCE, &artifacts_dir()).unwrap();
    }
    drv.ok(&Command::Appearance { dark: false }).await.unwrap();
    words
}

/// The first node of `dump`'s accessibility tree with role `role` whose label holds `words`.
fn labelled<'a>(dump: &'a Dump, role: &str, words: &str) -> Option<&'a A11yNode> {
    dump.a11y
        .iter()
        .find(|n| n.role == role && n.label.as_deref().is_some_and(|l| l.contains(words)))
}

/// Wait until the agent's thread, as the worker keeps it (`slopty agent read`), is `ready`.
async fn thread_until(stack: &ServedApp, agent: &str, what: &str, ready: impl Fn(&Value) -> bool) {
    let since = std::time::Instant::now();
    loop {
        let read = stack.slopty(&["agent", "read", "--thread", agent]).await;
        if read.as_ref().is_ok_and(&ready) {
            return;
        }
        assert!(since.elapsed() < TURN, "the agent's thread {what}: {read:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Start the stand-in on `folder` with `prompt`; the agent's id.
async fn start(stack: &ServedApp, folder: &str, prompt: &str) -> String {
    let mut start = vec!["agent", "start", MACHINE, "--folder", folder];
    start.extend(prompt.split(' '));
    let started = stack.slopty(&start).await.unwrap();
    let agent = started["agent"].as_str().unwrap_or_default().to_owned();
    assert!(!agent.is_empty(), "an agent started: {started}");
    agent
}

/// A recorded Claude Code turn reads as zeron draws it. The `tools` recording, played to its
/// end: its calls fold under one summary that counts them by kind ("Ran 3 commands", the
/// glob and grep as searches, the file written, the one that failed), the task list's tray
/// counts what is done, and the subagent the turn ran is a chip; a click on the summary opens
/// the group. The turn ends with what it changed, and ⌘F finds words in the thread, counted
/// and stepped through. Then the `permission` recording, held at its first request: the composer
/// asks for the command, which it names.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_recorded_turn_reads_as_zeron_draws_it() {
    let replays = serde_json::json!({ TOOLS: "tools", PERMISSION: "permission" }).to_string();
    let mut stack =
        ServedApp::launch_with(MACHINE, &[], &[("STUB_REPLAY", &replays)]).await.unwrap();
    let work = stack.path("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("main.rs"), MAIN_RS).unwrap();
    let renders = stack.path("");
    let folder = work.to_string_lossy().into_owned();

    // The whole `tools` turn, through its answer and its `Stop`.
    let tools = start(&stack, &folder, TOOLS).await;
    thread_until(&stack, &tools, "is done", |read| read["phase"] == "done").await;

    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    drv.keys("cmd-1").await.unwrap();
    // The thread's history arrives and draws; the turn is over, so nothing moves after.
    tokio::time::sleep(SETTLE.saturating_mul(2)).await;
    let words = goldens(drv, &renders, "transcript-turn").await;
    let summary = drv.dump().await.unwrap();
    // What the frames fail to say, gathered so every state is still drawn and judged.
    let mut missing: Vec<String> = Vec::new();

    // One summary for the turn's calls, counted by kind: the three commands (one failed), the
    // file written, the glob and the grep, the task list and the tool search.
    let group = labelled(&summary, "Button", "Ran 3 commands");
    let label = group.and_then(|g| g.label.clone()).unwrap_or_default();
    for said in GROUP {
        if !label.contains(said) {
            missing.push(format!("the group's summary says {said:?} (it says {label:?})"));
        }
    }
    // The task list's tray: both of the turn's tasks done.
    if !words.contains("2/2") {
        missing.push("the todo tray counts 2/2".to_owned());
    }
    // The subagent the turn ran: a chip named for its task, and a row under its agent.
    if !words.contains("Count lines") {
        missing.push("the subagent's chip names its task, Count lines".to_owned());
    }
    if labelled(&summary, "Button", "Show 1 finished subagents").is_none() {
        missing.push("the sidebar holds the finished subagent".to_owned());
    }

    // The group opens on a click: its calls are drawn under the summary.
    if let Some(group) = group {
        let closed = summary.a11y.len();
        let [x, y, w, h] = group.bounds;
        drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
        drv.wait_for("the group open", STEP, |d| d.a11y.len() > closed).await.unwrap();
        tokio::time::sleep(SETTLE).await;
        goldens(drv, &renders, "transcript-turn-open").await;
    }

    // What the turn changed closes it: the one file it wrote, with its line counts.
    let drv = &mut stack.driver;
    let summary = drv.dump().await.unwrap();
    if labelled(&summary, "Button", "1 file changed").is_none() {
        missing.push("the turn's changes row says 1 file changed".to_owned());
    }
    if labelled(&summary, "Button", "Open diff").is_none() {
        missing.push("the turn's changes row offers Open diff".to_owned());
    }

    // Find in the thread: ⌘F opens the field, the words are counted where they read and the
    // first is current ("1 of N"); Return steps on, wrapping; Escape closes it.
    let tally = |d: &Dump| -> Option<(usize, usize)> {
        let label = labelled(d, "Status", " of ")?.label.clone()?;
        let (at, total) = label.split_once(" of ")?;
        Some((at.trim().parse().ok()?, total.trim().parse().ok()?))
    };
    drv.keys("cmd-f").await.unwrap();
    drv.type_text("the").await.unwrap();
    match drv.wait_for("the find tally", STEP, |d| tally(d).is_some()).await {
        Ok(found) => {
            let (at, total) = tally(&found).unwrap_or_default();
            if at != 1 || total == 0 {
                missing.push(format!("find lands on the first match (it says {at} of {total})"));
            }
            goldens(drv, &renders, "transcript-find").await;
            drv.keys("enter").await.unwrap();
            let next = if total > 1 { 2 } else { 1 };
            if drv
                .wait_for("find steps on", STEP, |d| tally(d) == Some((next, total)))
                .await
                .is_err()
            {
                missing.push(format!("Return steps find to {next} of {total}"));
            }
            drv.keys("escape").await.unwrap();
            if drv
                .wait_for("find closed", STEP, |d| {
                    labelled(d, "Search", "Find in thread").is_none()
                })
                .await
                .is_err()
            {
                missing.push("Escape closes find".to_owned());
            }
        }
        Err(_) => missing.push("⌘F opens find with a tally".to_owned()),
    }

    // The `permission` turn stops at its first request, which the composer asks, naming the
    // command.
    let asked = start(&stack, &folder, PERMISSION).await;
    let asking = |read: &Value| read["requests"].as_array().is_some_and(|r| !r.is_empty());
    thread_until(&stack, &asked, "asks the person", asking).await;
    // The sidebar lists threads in the order they started: the second row is the new one.
    let drv = &mut stack.driver;
    drv.keys("cmd-2").await.unwrap();
    tokio::time::sleep(SETTLE.saturating_mul(2)).await;
    let words = goldens(drv, &renders, "transcript-question").await;
    if !words.contains("touch refused.txt") {
        missing.push("the request in the composer names its command".to_owned());
    }
    assert!(missing.is_empty(), "the frames do not say:\n- {}", missing.join("\n- "));

    stack.shutdown().await;
}
