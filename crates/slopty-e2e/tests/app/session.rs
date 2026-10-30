//! A session is its terminal, on a real stack (`docs/decisions/ui.md`, "A session is its
//! terminal, its inspector beside it"): an agent started in a project through the CLI is the
//! stand-in Claude Code (`slopty-stub-claude`) running as its own TUI in a terminal of the
//! worker. Opened, the session's main column is that terminal, holding the keyboard, and the
//! inspector ⌘I opens beside it reads the session's turn from Claude Code's transcript: the prompt,
//! the answer, and the turn's call of Slopty's `machines` tool folded to a line, which a press
//! opens whole, its result shown in full. Judged by the accessibility tree, the terminal's rows
//! and the goldens, in light and in dark.

#![cfg(target_os = "macos")]

use slopty_e2e::harness::ServedApp;
use slopty_e2e::{A11yNode, Dump};

use crate::served::{
    SETTLE, STEP, goldens, middle, open_thread, says, seen, show_inspector, start, thread_until,
};

/// The worker's name, as the CLI names the machine.
const MACHINE: &str = "session";

/// What the session is asked: the stand-in calls `machines` for it (`STUB_TURNS`), then answers.
const PROMPT: &str = "List the machines";

/// What only the `machines` call's whole result says: each machine's liveness.
const RESULT_WORDS: &str = "liveness";

/// The press that shows a call's output past its first lines, named with the output's size.
const FULL_OUTPUT: &str = "Show full output";

/// The terminal the session runs in, once it shows the stand-in's answer.
fn answered_terminal(dump: &Dump) -> Option<&slopty_e2e::TerminalInfo> {
    dump.terminals
        .iter()
        .find(|t| t.lines.iter().any(|line| line.contains(&format!("Done: {PROMPT}"))))
}

/// Where the session's terminal ends on the right: what lies past it is the right pane's.
fn terminal_right(dump: &Dump) -> f32 {
    dump.a11y.iter().find(|n| n.role == "Terminal").map_or(0.0, |n| n.bounds[0] + n.bounds[2])
}

/// Whether a node of the right pane, past the terminal, says `words`.
fn inspector_says(dump: &Dump, words: &str) -> bool {
    let edge = terminal_right(dump) - 1.0;
    dump.a11y.iter().any(|n| {
        n.bounds[0] >= edge
            && [&n.label, &n.value].into_iter().flatten().any(|text| text.contains(words))
    })
}

/// A press in the inspector that opens more of the turn: its work folded behind "Worked for",
/// a group's summary, the call's own line, or its output past the first lines, the first of them
/// not pressed yet.
fn opener<'a>(dump: &'a Dump, pressed: &[String]) -> Option<&'a A11yNode> {
    let edge = terminal_right(dump) - 1.0;
    dump.a11y.iter().find(|n| {
        n.role == "Button"
            && n.bounds[0] >= edge
            && n.label.as_deref().is_some_and(|label| {
                !pressed.iter().any(|p| p == label)
                    && (label.starts_with("Worked for ")
                        || label.to_lowercase().contains("machines")
                        || label.starts_with("Called ")
                        || label.starts_with("Ran ")
                        || label.starts_with(FULL_OUTPUT))
            })
    })
}

/// The session's terminal is its main column and holds the keyboard; its inspector reads the
/// turn from the transcript, the call folded to a line until a press opens it whole.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_session_is_its_terminal_and_its_inspector_opens_a_call_whole() {
    let turns =
        serde_json::json!({ PROMPT: [{ "name": "machines", "arguments": {} }] }).to_string();
    let mut stack = ServedApp::launch_with(MACHINE, &[], &[("STUB_TURNS", &turns)]).await.unwrap();
    let repo = stack.path("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let agent = start(&stack, MACHINE, &repo, PROMPT).await;
    thread_until(&stack, &agent, "answered", |read| says(read, &format!("Done: {PROMPT}"))).await;

    let renders = stack.path("");
    let drv = &mut stack.driver;
    open_thread(drv, 1).await;
    show_inspector(drv).await;
    // The terminal shows the TUI's own answer and holds the keyboard; the inspector beside it
    // reads the same turn from the transcript.
    let shown = seen(drv, "the session's terminal and its inspector", STEP, |d| {
        answered_terminal(d).is_some()
            && d.focused.starts_with("terminal:")
            && inspector_says(d, &format!("Done: {PROMPT}"))
    })
    .await;
    let terminal = answered_terminal(&shown).unwrap();
    assert_eq!(
        shown.focused,
        format!("terminal:{}", terminal.session),
        "the session's own terminal holds the keyboard"
    );
    let words = slopty_e2e::frame_text(&shown.a11y);
    assert!(inspector_says(&shown, PROMPT), "the inspector holds the prompt:\n{words}");
    assert!(!inspector_says(&shown, RESULT_WORDS), "the call is folded to its line:\n{words}");
    // The pointer off the window: no hover or tooltip in the renders.
    drv.ok(&slopty_e2e::Command::Move { x: -1.0, y: -1.0 }).await.unwrap();
    tokio::time::sleep(SETTLE).await;
    goldens(drv, &renders, "session-inspector", &[]).await;

    // Pressed open, step by step, the call shows its whole result.
    let mut pressed: Vec<String> = Vec::new();
    let opened = loop {
        let dump = drv.dump().await.unwrap();
        if inspector_says(&dump, RESULT_WORDS) {
            break dump;
        }
        let next = opener(&dump, &pressed).cloned();
        let Some(next) = next else {
            panic!(
                "nothing opens the call further; the window said:\n{}",
                slopty_e2e::frame_text(&dump.a11y)
            );
        };
        assert!(pressed.len() < 4, "the call never opened whole after {pressed:?}");
        pressed.push(next.label.clone().unwrap_or_default());
        let (x, y) = middle(&next);
        drv.click(x, y).await.unwrap();
        tokio::time::sleep(SETTLE).await;
    };
    assert!(
        pressed.iter().any(|label| label.to_lowercase().contains("machines")),
        "the call's own line opened it: {pressed:?}\n{}",
        slopty_e2e::frame_text(&opened.a11y)
    );
    drv.ok(&slopty_e2e::Command::Move { x: -1.0, y: -1.0 }).await.unwrap();
    tokio::time::sleep(SETTLE).await;
    // The result's size counts the facts of the machine the run is on (its address, load,
    // memory, displays), so the press that shows the rest, named with that size, is left out.
    goldens(drv, &renders, "session-inspector-call", &[FULL_OUTPUT]).await;
    stack.shutdown().await;
}
