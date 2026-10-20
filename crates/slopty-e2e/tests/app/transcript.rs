//! The transcript of a real turn on the real app: a stand-in Claude Code (`slopty-stub-claude`)
//! plays a recorded Claude Code conversation (`STUB_REPLAY`, `slopty_testkit::replay`) as its
//! own turn on the stack's worker, so the shell draws what Claude Code really writes: tool calls
//! grouped under one summary, a task list, a subagent, and a permission request in the
//! composer. Every state is a golden in light and in dark, and its words are read from the
//! accessibility tree.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use slopty_e2e::harness::ServedApp;
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

/// What the `tools` turn's groups say of their calls, each found by its first words: the agent's
/// words between calls end a group, a directory listed is counted apart from commands (as the
/// TUI's group line counts it), a failed call is counted as failed whatever its kind, the task
/// list's update after the subagent's card is a group of its own, and the tool search is
/// housekeeping that shows no chip, as in Claude Code's TUI.
/// What the turn's calls add up to over every group's summary, as the summaries count them
/// ("Ran 2 commands · searched 2 times · 1 failed", "Ran 1 command · edited 1 file +2 −0"):
/// commands run, searches, failures and files edited. Where the groups split follows where the
/// recorded agent spoke between calls, which a new recording may move.
const CALLS: [(&str, u32); 4] = [("command", 3), ("searched", 2), ("failed", 1), ("edited", 1)];

/// The counts `summary` (one group's sentence: "Ran 2 commands (1 failed), searched 2 times and
/// edited 1 file") gives for each of [`CALLS`]' kinds.
fn counted(summary: &str) -> [u32; 4] {
    let mut counts = [0_u32; 4];
    let clauses = summary.replace(" (", ", ").replace(')', "").replace(" and ", ", ");
    for part in clauses.split(", ") {
        let number = part.split_whitespace().find_map(|w| w.parse::<u32>().ok()).unwrap_or(0);
        for (slot, (kind, _)) in counts.iter_mut().zip(CALLS) {
            if part.contains(kind) {
                *slot = (*slot).saturating_add(number);
            }
        }
    }
    counts
}

/// What the recorded agent worked on.
const MAIN_RS: &str = "fn main() {\n    println!(\"hello\");\n}\n";

/// Render the window in light and in dark against `golden/<name>-<appearance>.png`, the done
/// line's moment left out ([`crate::served::goldens`]); the frame's words, as the light render
/// said them.
async fn goldens(drv: &mut Driver, renders: &Path, name: &str) -> String {
    // The pointer off the transcript, so no message's hover line shows; the strip's controls are
    // always mounted, so no row is masked.
    drv.ok(&Command::Move { x: 10.0, y: 10.0 }).await.unwrap();
    crate::served::goldens_with(drv, renders, name, &[], &[]).await
}

/// The first node of `dump`'s accessibility tree with role `role` whose label holds `words`.
fn labelled<'a>(dump: &'a Dump, role: &str, words: &str) -> Option<&'a A11yNode> {
    dump.a11y
        .iter()
        .find(|n| n.role == role && n.label.as_deref().is_some_and(|l| l.contains(words)))
}

/// The subagent's card, "Subagent Count lines": a click opens its run in place (a group
/// "Transcript of Count lines" under it), Escape folds it, and its Open drills into the run, the
/// breadcrumb "… › Count lines" over it, a bar saying it runs inside its parent where the
/// composer was, and Escape leads back. What the frames fail to say goes to `missing`.
async fn subagent_card(drv: &mut Driver, renders: &Path, missing: &mut Vec<String>) {
    const CARD: &str = "Subagent Count lines";
    const RUN: &str = "Transcript of Count lines";
    let shown = drv.dump().await.unwrap();
    let Some(card) = shown.a11y_node("Button", Some(CARD)).cloned() else {
        missing.push(format!("the card {CARD:?}"));
        return;
    };
    // Settled, it says what its run answered ("4"), never a blank line.
    if !card.value.as_deref().is_some_and(|value| value.starts_with("4 · ")) {
        missing.push(format!("the settled card says its run's answer: {:?}", card.value));
    }
    let [x, y, w, h] = card.bounds;
    drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
    if let Err(e) = drv
        .wait_for("the run open in place", STEP, |d| d.a11y_node("Group", Some(RUN)).is_some())
        .await
    {
        // The wait's own error names why: no group, or a dump that bailed on a stale frame.
        missing.push(format!("a click on the card opens {RUN:?} in place: {e:#}"));
        return;
    }
    tokio::time::sleep(SETTLE).await;
    goldens(drv, renders, "transcript-subagent-open").await;
    drv.keys("escape").await.unwrap();
    if drv
        .wait_for("the run folded", STEP, |d| d.a11y_node("Group", Some(RUN)).is_none())
        .await
        .is_err()
    {
        missing.push("Escape folds the subagent's run".to_owned());
    }
    // Open, on the card's hover: the run as the session view, under its breadcrumb.
    let shown = drv.dump().await.unwrap();
    let card = shown.a11y_node("Button", Some(CARD)).cloned().unwrap_or(card);
    let [x, y, w, h] = card.bounds;
    drv.ok(&Command::Move { x: x + w / 2.0, y: y + h / 2.0 }).await.unwrap();
    let Ok(hovered) = drv
        .wait_for("Open subagent", STEP, |d| d.a11y_node("Button", Some("Open subagent")).is_some())
        .await
    else {
        missing.push("the card's hover offers Open subagent".to_owned());
        return;
    };
    let [x, y, w, h] = hovered.a11y_node("Button", Some("Open subagent")).unwrap().bounds;
    drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
    let drilled = drv
        .wait_for("the breadcrumb", STEP, |d| {
            d.a11y.iter().any(|n| {
                n.role == "Navigation"
                    && n.label.as_deref().is_some_and(|l| l.ends_with("› Count lines"))
            })
        })
        .await;
    match drilled {
        Ok(drilled) => {
            if !drilled.a11y.iter().any(|n| {
                n.role == "Status"
                    && n.label.as_deref().is_some_and(|l| l.starts_with("Runs inside"))
            }) {
                missing.push("the drilled-in run says it runs inside its parent".to_owned());
            }
            tokio::time::sleep(SETTLE).await;
            goldens(drv, renders, "transcript-subagent-drilled").await;
            drv.keys("escape").await.unwrap();
            if drv
                .wait_for("back in the parent", STEP, |d| {
                    d.a11y_node("Button", Some(CARD)).is_some()
                        && !d.a11y.iter().any(|n| {
                            n.role == "Navigation"
                                && n.label.as_deref().is_some_and(|l| l.ends_with("› Count lines"))
                        })
                })
                .await
                .is_err()
            {
                missing.push("Escape leads back to the parent's chat".to_owned());
            }
        }
        Err(_) => missing.push("Open drills into the subagent's run".to_owned()),
    }
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
/// end: settled, the turn is its prompt, one "Worked for" divider and its answer, and its finished
/// task list leaves no tray. The divider opens on the work: its calls fold under a summary on
/// each side of the subagent's card, counting them by kind ("Ran 2 commands (1 failed)", the glob
/// and grep as searches; then the file written), and the subagent the turn ran is a card that
/// opens in place and drills in; a click on a summary opens its group. The turn ends with what
/// it changed, and ⌘F finds words in the thread, counted and stepped through. Then the `permission`
/// recording, held at its first request: the composer asks for the command, which it names.
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
    // What the frames fail to say, gathered so every state is still drawn and judged.
    let mut missing: Vec<String> = Vec::new();

    // The settled turn reads as its prompt, one divider and its answer (Amp's settled turn): its
    // work folds behind "Worked for", which a click opens. A finished task list on an ended turn
    // leaves no tray; the work's own Todo row keeps the record.
    if words.contains("2/2") {
        missing.push("a finished task list leaves its tray once the turn ends".to_owned());
    }
    let folded = drv.dump().await.unwrap();
    let summary = if let Some(divider) = labelled(&folded, "Button", "Worked for ") {
        let closed = folded.a11y.len();
        let [x, y, w, h] = divider.bounds;
        drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
        if drv.wait_for("the turn's work open", STEP, |d| d.a11y.len() > closed).await.is_ok() {
            tokio::time::sleep(SETTLE).await;
            goldens(drv, &renders, "transcript-turn-work").await;
        } else {
            missing.push("a click on the divider opens the turn's work".to_owned());
        }
        drv.dump().await.unwrap()
    } else {
        missing.push("the settled turn folds its work behind a \"Worked for\" divider".to_owned());
        folded
    };

    // A summary for the calls on each side of the subagent's card, counted by kind: the
    // commands (one failed), the glob and the grep and the task list before it, a command and the
    // file written after it.
    let groups: Vec<String> = summary
        .a11y
        .iter()
        .filter(|n| n.role == "Button")
        .filter_map(|n| n.label.clone())
        .filter(|l| l.starts_with("Ran "))
        .collect();
    let total = groups.iter().map(|g| counted(g)).fold([0_u32; 4], |mut sum, counts| {
        for (s, c) in sum.iter_mut().zip(counts) {
            *s = (*s).saturating_add(c);
        }
        sum
    });
    for ((kind, want), got) in CALLS.into_iter().zip(total) {
        if got != want {
            missing
                .push(format!("the groups' summaries count {want} {kind} (they say {groups:?})"));
        }
    }
    if labelled(&summary, "Button", "Todo 2/2 done").is_none() {
        missing.push("a group's summary says \"Todo 2/2 done\"".to_owned());
    }
    let group = labelled(&summary, "Button", "Ran ");
    // The subagent the turn ran: a card in the turn's work named for its task, no longer a row
    // of the sidebar.
    if !slopty_e2e::frame_text(&summary.a11y).contains("Count lines") {
        missing.push("the subagent's card names its task, Count lines".to_owned());
    }
    if labelled(&summary, "Button", "Subagent Count lines").is_none() {
        missing.push("the transcript holds the subagent's card".to_owned());
    }
    if labelled(&summary, "Button", "finished subagent").is_some() {
        missing.push("the sidebar holds no subagent rows".to_owned());
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

    // The subagent's card opens its run in place, Escape folds it, and its Open drills into the
    // run under a breadcrumb that leads back.
    let drv = &mut stack.driver;
    subagent_card(drv, &renders, &mut missing).await;

    // What the turn changed closes it: the one file it wrote, with its line counts.
    let drv = &mut stack.driver;
    let summary = drv.dump().await.unwrap();
    if labelled(&summary, "Group", "notes.md +2 −0").is_none() {
        missing.push("the turn's changes card names its one file, notes.md +2 −0".to_owned());
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
    // A run the card step left open (it opened past its wait) is folded first, so find's golden
    // is the turn as it reads folded, not a failed step's leftover.
    let shown = drv.dump().await.unwrap();
    if shown.a11y_node("Group", Some("Transcript of Count lines")).is_some() {
        drv.keys("escape").await.unwrap();
        let folded = drv
            .wait_for("the run folded before find", STEP, |d| {
                d.a11y_node("Group", Some("Transcript of Count lines")).is_none()
            })
            .await;
        if folded.is_err() {
            missing.push("Escape folds the subagent's run before find".to_owned());
        }
    }
    drv.keys("cmd-f").await.unwrap();
    drv.type_text("the").await.unwrap();
    if let Ok(found) = drv.wait_for("the find tally", STEP, |d| tally(d).is_some()).await {
        let (at, total) = tally(&found).unwrap_or_default();
        if at != 1 || total == 0 {
            missing.push(format!("find lands on the first match (it says {at} of {total})"));
        }
        // Its golden is the turn as the steps before left it: after a step that failed, that is
        // the failure's leftover (a group or a run still open), not the turn to hold find to.
        if missing.is_empty() {
            goldens(drv, &renders, "transcript-find").await;
        } else {
            println!("transcript-find: no golden, a step before failed");
        }
        drv.keys("enter").await.unwrap();
        let next = if total > 1 { 2 } else { 1 };
        if drv.wait_for("find steps on", STEP, |d| tally(d) == Some((next, total))).await.is_err() {
            missing.push(format!("Return steps find to {next} of {total}"));
        }
        drv.keys("escape").await.unwrap();
        if drv
            .wait_for("find closed", STEP, |d| labelled(d, "Search", "Find in chat").is_none())
            .await
            .is_err()
        {
            missing.push("Escape closes find".to_owned());
        }
    } else {
        let shown = drv.dump().await.unwrap();
        let said = slopty_e2e::frame_text(&shown.a11y);
        // Find reaches the window its transcript draws in, key or not (`transcript/find.rs`):
        // say whether it was key all the same.
        let key = shown.a11y.iter().any(|n| n.role == "Window" && n.focused);
        missing.push(format!(
            "⌘F opens find with a tally (the window key: {key}); the frame said:\n{said}"
        ));
    }

    // The `permission` turn stops at its first request, which the composer asks, naming the
    // command.
    let asked = start(&stack, &folder, PERMISSION).await;
    let asking = |read: &Value| read["requests"].as_array().is_some_and(|r| !r.is_empty());
    thread_until(&stack, &asked, "asks the person", asking).await;
    // The new thread's row in the sidebar opens it.
    let drv = &mut stack.driver;
    let row = drv
        .wait_for("the new thread's row", STEP, |d| labelled(d, "Button", PERMISSION).is_some())
        .await
        .unwrap();
    let [x, y, w, h] = labelled(&row, "Button", PERMISSION).unwrap().bounds;
    drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
    // The pointer leaves the row, whose hover card says since when the chat waits: a moment of
    // the run.
    drv.ok(&Command::Move { x: WINDOW.0 / 2.0, y: 60.0 }).await.unwrap();
    tokio::time::sleep(SETTLE.saturating_mul(2)).await;
    let words = goldens(drv, &renders, "transcript-question").await;
    if !words.contains("touch refused.txt") {
        missing.push("the request in the composer names its command".to_owned());
    }
    assert!(missing.is_empty(), "the frames do not say:\n- {}", missing.join("\n- "));

    stack.shutdown().await;
}
