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

/// The follow-ups of [`a_long_session_reads_calm_from_its_top_to_its_end`], each the prompt the
/// person types, what plays for it (a recording of `slopty-agent`'s by name, or an answer in
/// words alone written for the session: `None`, its own folder under the stack's root), and the
/// words its last turn ends on: the background command's end is a turn of its own.
const SESSION: [(&str, Option<&str>, &str); 4] = [
    ("What did you find?", None, "run `cargo test` in CI."),
    ("Make beta loud in notes.txt", Some("edit"), "done"),
    ("Run the slow check in the background", Some("background"), "It finished."),
    ("Sum it up for the PR", None, "Nothing is left open."),
];

/// What the agent answers, in words alone, to [`SESSION`]'s prompts that play no recording: the
/// markdown a real answer holds (bold, inline code, a list, a fenced block, a numbered list).
fn session_answer(prompt: &str) -> &'static str {
    if prompt.starts_with("What") {
        "Here's what the survey turned up:\n\n\
         - **`main.rs`** is the only source file: three lines, and it prints `hello`.\n\
         - `ls missing-file` failed as planned; nothing else did.\n\
         - The background check finished cleanly, exit 0.\n\n\
         The notes I wrote are in `notes.md`:\n\n\
         ```markdown\n# Notes\nmain.rs prints hello\n```\n\n\
         Next I'd add a test for the greeting, then run `cargo test` in CI."
    } else {
        "Ready for review. The PR:\n\n\
         1. Surveys the crate and records what it found in `notes.md`.\n\
         2. Makes `beta` loud in `notes.txt` (`BETA`), one line changed.\n\
         3. Runs the slow check in the background; it woke and passed.\n\n\
         Nothing is left open."
    }
}

/// A recorded turn of words alone in `folder`, as `STUB_REPLAY` reads it: `prompt`, then
/// `answer`, as Claude Code writes them.
fn answered_turn(folder: &Path, prompt: &str, answer: &str) {
    const ID: &str = "00000000-0000-4000-8000-0000000000";
    const SESSION_ID: &str = "00000000-0000-4000-8000-000000000001";
    let record = |n: u32, parent: Option<u32>, kind: &str, message: Value| {
        let mut line = serde_json::json!({
            "cwd": "/work", "entrypoint": "sdk-cli", "gitBranch": "HEAD", "isSidechain": false,
            "message": message, "parentUuid": parent.map(|p| format!("{ID}{p:02}")),
            "promptId": format!("{ID}02"), "sessionId": SESSION_ID,
            "timestamp": format!("2026-10-20T06:00:{n:02}.000Z"), "type": kind,
            "userType": "external", "uuid": format!("{ID}{n:02}"), "version": "2.1.301",
        });
        if kind == "assistant" {
            line["requestId"] = serde_json::json!(format!("req_{n:02}"));
        }
        line
    };
    let said = serde_json::json!({
        "content": [{ "type": "text", "text": answer }], "id": "msg_01",
        "model": "claude-haiku-5-5", "role": "assistant", "stop_reason": "end_turn",
        "type": "message", "usage": { "input_tokens": 2, "output_tokens": 120 },
    });
    let lines = [
        record(3, None, "user", serde_json::json!({ "role": "user", "content": prompt })),
        record(4, Some(3), "assistant", said),
    ];
    std::fs::create_dir_all(folder).unwrap();
    let transcript = lines.iter().fold(String::new(), |mut text, line| {
        use std::fmt::Write as _;
        writeln!(text, "{line}").unwrap();
        text
    });
    std::fs::write(folder.join("transcript.jsonl"), transcript).unwrap();
    std::fs::write(folder.join("hooks.jsonl"), "").unwrap();
}

/// Whether the thread, as read, rests with the turn `prompt` began and every turn after it ended,
/// the last agent's words `last`.
fn rests_after(read: &Value, prompt: &str, last: &str) -> bool {
    let Some(turns) = read["turns"].as_array() else { return false };
    let asked = turns.iter().position(|turn| {
        turn["entries"].as_array().is_some_and(|entries| {
            entries.iter().any(|e| e["type"] == "user" && e["text"].as_str() == Some(prompt))
        })
    });
    let said = turns.iter().rev().find_map(|turn| {
        turn["entries"].as_array()?.iter().rev().find(|e| e["type"] == "agent")?["text"].as_str()
    });
    asked.is_some_and(|at| turns[at..].iter().all(|turn| !turn["ended_ms"].is_null()))
        && said.is_some_and(|said| said.trim_end().ends_with(last))
        && read["phase"] == "done"
}

/// A long session reads calm from its top to its end, the way a real one goes: the `tools`
/// survey (tasks, searches, commands, a subagent, a file written), the agent's findings in
/// markdown, an edit, a command run in the background, and a summary for the PR. Each settled
/// turn is its prompt, one "Worked for" divider and its answer, so the page reads as a
/// conversation, not a log; its end and its top are goldens in light and in dark.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_long_session_reads_calm_from_its_top_to_its_end() {
    let mut stack = ServedApp::launch_rooted(MACHINE, &[], |root| {
        let mut replays = serde_json::Map::new();
        replays.insert(TOOLS.to_owned(), Value::from("tools"));
        for (n, (prompt, recording, _)) in SESSION.iter().enumerate() {
            let played = recording.map_or_else(
                || root.join(format!("answer-{n}")).to_string_lossy().into_owned(),
                str::to_owned,
            );
            replays.insert((*prompt).to_owned(), Value::from(played));
        }
        vec![("STUB_REPLAY".to_owned(), Value::Object(replays).to_string())]
    })
    .await
    .unwrap();
    for (n, (prompt, recording, _)) in SESSION.iter().enumerate() {
        if recording.is_none() {
            answered_turn(&stack.path(&format!("answer-{n}")), prompt, session_answer(prompt));
        }
    }
    let work = stack.path("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("main.rs"), MAIN_RS).unwrap();
    std::fs::write(work.join("notes.txt"), "alpha\nbeta\ngamma\ndelta\n").unwrap();
    let renders = stack.path("");
    let folder = work.to_string_lossy().into_owned();

    let agent = start(&stack, &folder, TOOLS).await;
    thread_until(&stack, &agent, "surveyed", |read| rests_after(read, TOOLS, "done")).await;
    for (prompt, _, last) in SESSION {
        crate::served::tell(&mut stack, prompt).await;
        thread_until(&stack, &agent, prompt, |read| rests_after(read, prompt, last)).await;
    }

    let drv = &mut stack.driver;
    drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    drv.keys("cmd-1").await.unwrap();
    // The last turn's history arrives and its divider folds; nothing moves after.
    tokio::time::sleep(SETTLE.saturating_mul(2)).await;
    let end = goldens(drv, &renders, "transcript-session").await;
    let mut missing: Vec<String> = Vec::new();
    if !end.contains("Nothing is left open.") {
        missing.push("the session's end shows its last answer".to_owned());
    }
    // The last turn did no work: it ends on its done line, and no turn's work stays open.
    if labelled(&drv.dump().await.unwrap(), "Button", "Hide work").is_some() {
        missing.push("no settled turn's work stays open by itself at the end".to_owned());
    }

    // Its top: the survey's prompt, its folded work and the findings under it.
    // The wheel over the transcript's column, rolled up past the session's top.
    for _ in 0..30 {
        drv.scroll(WINDOW.0 * 0.6, WINDOW.1 / 2.0, 0.0, 400.0).await.unwrap();
    }
    tokio::time::sleep(SETTLE).await;
    let top = goldens(drv, &renders, "transcript-session-top").await;
    if !top.contains(TOOLS) {
        missing.push(format!("the session's top shows its first prompt, {TOOLS:?}"));
    }
    // Every settled turn folds its work behind one "Worked for" divider: the page reads as a
    // conversation, not a log.
    let shown = drv.dump().await.unwrap();
    let dividers: Vec<&str> = shown
        .a11y
        .iter()
        .filter(|n| n.role == "Button")
        .filter_map(|n| n.label.as_deref())
        .filter(|l| {
            l.starts_with("Worked for ") || l.starts_with("Show work") || l.starts_with("Hide work")
        })
        .collect();
    if !dividers.first().is_some_and(|l| l.starts_with("Worked for ")) {
        missing.push(format!("the survey folds its work behind \"Worked for\", not {dividers:?}"));
    }
    if !dividers.iter().all(|l| l.starts_with("Worked for ")) {
        missing.push(format!("every settled turn's divider says how long it worked: {dividers:?}"));
    }
    assert!(missing.is_empty(), "the frames do not say:\n- {}", missing.join("\n- "));

    stack.shutdown().await;
}
