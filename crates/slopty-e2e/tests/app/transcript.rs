//! The inspector reads a real Claude Code session on the real app: conversations Claude Code
//! recorded (`slopty-agent`'s fixtures, `tests/fixtures/conversation`) are planted as a session
//! in Claude Code's own store on the stack's worker, taken up from the project's History by the
//! stand-in TUI (`slopty-stub-claude`, `--resume <id>`) in the session's terminal, and the
//! inspector beside it draws what Claude Code really wrote, in the order it wrote it: tool calls
//! grouped under one summary that opens only on a press, a task list, a subagent's card that
//! opens in place and drills in, and the files changed (`docs/decisions/inspector.md`). Every
//! state is a golden in light and in dark, and its words are read from the accessibility tree.

use std::path::Path;

use slopty_e2e::{A11yNode, Command, Driver, Dump};

use crate::planted::{
    Turn, answered, inspector, inspector_words, last_answer, plant, resume, stack, terminal_right,
    thread_of,
};
use crate::served::{SETTLE, STEP, WINDOW, all_holding, middle, says, thread_until};

/// The worker's name, as the CLI names the machine.
const MACHINE: &str = "transcript";

/// The planted sessions' ids, as Claude Code names a session.
const SURVEY: &str = "3b8f2c41-6d0e-4a7b-9c15-2e4f6a8b0d13";
const LONG: &str = "7e1d4b2a-9c3f-4e8d-a6b5-0f2c4d6e8a91";

/// The first words of the `tools` recording's prompt: tasks made and worked, a glob, a grep,
/// three commands (one fails, one runs in the background), a file written, a subagent that reads.
const TOOLS: &str = "Do these steps in order, using the named tools";

/// What the `tools` turn's calls add up to over every group's summary and every lone call's line
/// ("Ran 2 commands (1 failed) and searched 2 times", "Run sleep 1; echo bg-finished", "Write
/// notes.md"): commands run, searches, failures and files edited. Where the groups split follows
/// where the recorded agent spoke between calls, which a new recording may move.
const CALLS: [(&str, u32); 4] = [("command", 3), ("searched", 2), ("failed", 1), ("edited", 1)];

/// The counts `summary` (one group's sentence: "Ran 2 commands (1 failed), searched 2 times and
/// edited 1 file", or a lone call's line: "Run …", "Write …") gives for each of [`CALLS`]' kinds.
fn counted(summary: &str) -> [u32; 4] {
    // A lone call's own line: one command run, or one file written.
    if summary.starts_with("Run ") {
        return [1, 0, 0, 0];
    }
    if summary.starts_with("Write ") {
        return [0, 0, 0, 1];
    }
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

/// The first node of the inspector with role `role` whose label holds `words`.
fn labelled<'a>(dump: &'a Dump, role: &str, words: &str) -> Option<&'a A11yNode> {
    inspector(dump)
        .into_iter()
        .find(|n| n.role == role && n.label.as_deref().is_some_and(|l| l.contains(words)))
}

/// Render the window in light and in dark against `golden/<name>-<appearance>.png`, the pointer
/// off it and the done line's moment left out ([`crate::served::goldens`]).
async fn goldens(drv: &mut Driver, renders: &Path, name: &str) {
    drv.ok(&Command::Move { x: -1.0, y: -1.0 }).await.unwrap();
    tokio::time::sleep(SETTLE).await;
    crate::served::goldens_with(drv, renders, name, &[], &[]).await;
}

/// The inspector's buttons that would fold a whole turn ("Worked for 4s", "Show work"): none,
/// since a turn reads as its steps (`docs/decisions/inspector.md`, "Stable above all").
fn dividers(dump: &Dump) -> Vec<String> {
    inspector(dump)
        .into_iter()
        .filter(|n| n.role == "Button")
        .filter_map(|n| n.label.clone())
        .filter(|l| {
            l.starts_with("Worked for ") || l.starts_with("Show work") || l.starts_with("Hide work")
        })
        .collect()
}

/// Click `node` and let what it opens settle.
async fn press(drv: &mut Driver, node: &A11yNode) {
    let (x, y) = middle(node);
    drv.click(x, y).await.unwrap();
    tokio::time::sleep(SETTLE).await;
}

/// Whether the subagent's run stands open in place: its brief ("Task: …") is a row of the trace.
fn brief_shown(dump: &Dump) -> bool {
    inspector(dump).into_iter().any(|n| {
        n.role == "Paragraph" && n.label.as_deref().is_some_and(|l| l.starts_with("Task: "))
    })
}

/// The subagent's row, "Subagent Count lines": a click splices its run's steps in under it, its
/// brief first, and another takes them out; its Open drills in, the run as the inspector's own
/// timeline under its task's name, whose way back leads to the session's. What the frames fail
/// to say goes to `missing`.
async fn subagent_card(drv: &mut Driver, renders: &Path, missing: &mut Vec<String>) {
    const CARD: &str = "Subagent Count lines";
    let shown = drv.dump().await.unwrap();
    let Some(card) = labelled(&shown, "Button", CARD).cloned() else {
        missing.push(format!("the row {CARD:?}"));
        return;
    };
    // Settled, it says what its run answered ("4"), then its figures, never a blank line.
    if !card.value.as_deref().is_some_and(|value| value == "4" || value.starts_with("4 · ")) {
        missing.push(format!("the settled row says its run's answer: {:?}", card.value));
    }
    if brief_shown(&shown) {
        missing.push("the run stays shut until a click".to_owned());
    }
    press(drv, &card).await;
    if drv.wait_for("its run in place", STEP, brief_shown).await.is_err() {
        missing.push("a click on the row splices its run in under it".to_owned());
        return;
    }
    goldens(drv, renders, "inspector-subagent-open").await;
    let card = labelled(&drv.dump().await.unwrap(), "Button", CARD).cloned().unwrap_or(card);
    press(drv, &card).await;
    if brief_shown(&drv.dump().await.unwrap()) {
        missing.push("a second click takes the subagent's run out".to_owned());
    }
    // Open, on the row's hover: the run as the inspector's timeline, under its task's name.
    let (x, y) = middle(&card);
    drv.ok(&Command::Move { x, y }).await.unwrap();
    let Ok(hovered) = drv
        .wait_for("Open subagent", STEP, |d| d.a11y_node("Button", Some("Open subagent")).is_some())
        .await
    else {
        missing.push("the card's hover offers Open subagent".to_owned());
        return;
    };
    press(drv, hovered.a11y_node("Button", Some("Open subagent")).unwrap()).await;
    let drilled = drv
        .wait_for("the run's own timeline", STEP, |d| {
            labelled(d, "Heading", "Count lines").is_some()
                && labelled(d, "Button", "Back to the session").is_some()
        })
        .await;
    let Ok(drilled) = drilled else {
        missing.push("Open shows the subagent's run under its task's name".to_owned());
        return;
    };
    goldens(drv, renders, "inspector-subagent-drilled").await;
    press(drv, labelled(&drilled, "Button", "Back to the session").unwrap()).await;
    if drv
        .wait_for("back in the parent", STEP, |d| labelled(d, "Button", CARD).is_some())
        .await
        .is_err()
    {
        missing.push("the way back leads to the session's timeline".to_owned());
    }
}

/// A recorded Claude Code session reads in the inspector as a trace of what it did. The `tools`
/// recording, taken up from History: settled, the turn is its prompt, then its steps and words in
/// the order the record has them, with no divider folding the turn, and its finished task list
/// leaves Now saying nothing. Its calls fold under a summary between the words the agent said,
/// counting them by kind ("Ran 2 commands (1 failed)", the glob and grep as searches), and a call
/// alone between them shows itself (the background command, the file written); the subagent the
/// turn ran is a row that opens in place and drills in; a click on a summary opens its group.
/// Changes counts the file the session wrote, and its card names it.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_recorded_session_reads_in_the_inspector() {
    let (mut stack, _work) =
        stack(MACHINE, |home, work| plant(home, work, SURVEY, &[Turn::Recorded("tools")])).await;
    let renders = stack.path("");
    resume(&mut stack.driver, TOOLS).await;
    let end = last_answer("tools");
    thread_until(&stack, &thread_of(SURVEY), "took up the recording", |read| answered(read, &end))
        .await;
    let drv = &mut stack.driver;
    // The thread's history arrives and draws; the turn is over, so nothing moves after.
    tokio::time::sleep(SETTLE.saturating_mul(2)).await;
    goldens(drv, &renders, "inspector-turn").await;
    let folded = drv.dump().await.unwrap();
    let words = inspector_words(&folded);
    // What the frames fail to say, gathered so every state is still drawn and judged.
    let mut missing: Vec<String> = Vec::new();
    if !words.contains(TOOLS) {
        missing.push(format!("the inspector's timeline holds the prompt:\n{words}"));
    }
    // A finished task list on an ended turn leaves no list in Now; the work's own Todo row
    // keeps the record.
    if labelled(&folded, "ListItem", "Done: Survey files").is_some() {
        missing.push("a finished task list leaves Now once the turn ends".to_owned());
    }
    // The changes the session made: its line counts them, and its card names the one file it
    // wrote, a press from its diff; Esc closes the card.
    match labelled(&folded, "Button", "Changes: 1 file, +2").cloned() {
        Some(line) => {
            press(drv, &line).await;
            let card = drv
                .wait_for("the Changes card", STEP, |d| {
                    labelled(d, "Button", "Open the diff of notes.md").is_some()
                })
                .await;
            if card.is_err() {
                missing
                    .push("the Changes card names notes.md, the file the session wrote".to_owned());
            }
            drv.keys("escape").await.unwrap();
            if drv
                .wait_for("the card closed", STEP, |d| {
                    labelled(d, "Button", "Open the diff of notes.md").is_none()
                })
                .await
                .is_err()
            {
                missing.push("Esc closes the Changes card".to_owned());
            }
        }
        None => {
            missing.push(format!("the Changes line counts the file the session wrote:\n{words}"));
        }
    }

    // No divider folds the settled turn: its steps read in the record's order.
    if !dividers(&folded).is_empty() {
        missing.push(format!("no divider folds the turn: {:?}", dividers(&folded)));
    }
    let summary = folded;

    // A summary for the calls between the agent's words, counted by kind; a call alone between
    // them shows itself ("Run sleep 1; echo bg-finished", "Write notes.md") and counts as one.
    let groups: Vec<String> = inspector(&summary)
        .into_iter()
        .filter(|n| n.role == "Button")
        .filter_map(|n| n.label.clone())
        .filter(|l| l.starts_with("Ran ") || l.starts_with("Run ") || l.starts_with("Write "))
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
    if !all_holding(&summary, "Button", "finished subagent").is_empty() {
        missing.push("the sidebar holds no subagent rows".to_owned());
    }
    // The group opens on a click: its calls are drawn under the summary.
    if let Some(group) = labelled(&summary, "Button", "Ran ").cloned() {
        let closed = summary.a11y.len();
        press(drv, &group).await;
        drv.wait_for("the group open", STEP, |d| d.a11y.len() > closed).await.unwrap();
        goldens(drv, &renders, "inspector-turn-open").await;
    }

    // The subagent's card opens its run in place, folds again, and its Open shows the run as the
    // inspector's own timeline, whose Back leads to the parent's.
    subagent_card(drv, &renders, &mut missing).await;
    assert!(missing.is_empty(), "the frames do not say:\n- {}", missing.join("\n- "));
    stack.shutdown().await;
}

/// The turns of [`a_long_session_reads_calm_from_its_top_to_its_end`], as the session went: the
/// `tools` survey, findings in markdown (bold, inline code, a list, a fenced block), an edit, a
/// command run in the background whose end is a turn of its own, and a summary for the PR in a
/// numbered list.
const SESSION: [Turn; 5] = [
    Turn::Recorded("tools"),
    Turn::Said(
        "What did you find?",
        "Here's what the survey turned up:\n\n\
         - **`main.rs`** is the only source file: three lines, and it prints `hello`.\n\
         - `ls missing-file` failed as planned; nothing else did.\n\
         - The background check finished cleanly, exit 0.\n\n\
         The notes I wrote are in `notes.md`:\n\n\
         ```markdown\n# Notes\nmain.rs prints hello\n```\n\n\
         Next I'd add a test for the greeting, then run `cargo test` in CI.",
    ),
    Turn::Recorded("edit"),
    Turn::Recorded("background"),
    Turn::Said(
        "Sum it up for the PR",
        "Ready for review. The PR:\n\n\
         1. Surveys the crate and records what it found in `notes.md`.\n\
         2. Makes `beta` loud in `notes.txt` (`BETA`), one line changed.\n\
         3. Runs the slow check in the background; it woke and passed.\n\n\
         Nothing is left open.",
    ),
];

/// A long session reads calm in the inspector from its top to its end, the way a real one goes
/// ([`SESSION`]). Each turn is its prompt, then its steps and words in the record's order, every
/// group folded to its summary until pressed; its end and its top are goldens in light and in
/// dark.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_long_session_reads_calm_from_its_top_to_its_end() {
    let (mut stack, _work) = stack(MACHINE, |home, work| plant(home, work, LONG, &SESSION)).await;
    let renders = stack.path("");
    resume(&mut stack.driver, TOOLS).await;
    thread_until(&stack, &thread_of(LONG), "took up the session", |read| {
        says(read, "Nothing is left open.")
    })
    .await;
    let drv = &mut stack.driver;
    // The last turn's history arrives and draws; nothing moves after.
    tokio::time::sleep(SETTLE.saturating_mul(2)).await;
    goldens(drv, &renders, "inspector-session").await;
    let end = drv.dump().await.unwrap();
    let mut missing: Vec<String> = Vec::new();
    if !inspector_words(&end).contains("Nothing is left open.") {
        missing.push("the session's end shows its last answer".to_owned());
    }
    // No turn folds behind a divider, and nothing opened by itself.
    if !dividers(&end).is_empty() {
        missing.push(format!("no divider folds a turn: {:?}", dividers(&end)));
    }

    // Its top: the survey's prompt, its folded work and the findings under it. The wheel over
    // the inspector's timeline, rolled up past the session's top.
    let over = terminal_right(&end) + (WINDOW.0 - terminal_right(&end)) / 2.0;
    for _ in 0..30 {
        drv.scroll(over, WINDOW.1 * 0.6, 0.0, 400.0).await.unwrap();
    }
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &renders, "inspector-session-top").await;
    let top = drv.dump().await.unwrap();
    if !inspector_words(&top).contains(TOOLS) {
        missing.push(format!("the session's top shows its first prompt, {TOOLS:?}"));
    }
    // The survey's steps follow its prompt, folded to their summaries, with no divider over them.
    let words = inspector_words(&top);
    let prompt_at = words.find(TOOLS);
    let first_group = words.find("Ran ");
    if !matches!((prompt_at, first_group), (Some(prompt), Some(group)) if prompt < group) {
        missing.push(format!("the survey's first group reads under its prompt:\n{words}"));
    }
    if !dividers(&top).is_empty() {
        missing.push(format!("no divider folds a turn: {:?}", dividers(&top)));
    }
    assert!(missing.is_empty(), "the frames do not say:\n- {}", missing.join("\n- "));
    stack.shutdown().await;
}
