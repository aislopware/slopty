//! Each agent wears its owner's mark where it is listed: Claude Code the Claude spark, Codex
//! the Blossom, pi its cells and an ACP agent the neutral glyph, each in the ink of its row's
//! words, leading its row at rest; while it works or waits on the person, that state leads
//! the row instead.
//!
//! The one golden where a mark's size or centring shows, light and dark: a Claude Code agent
//! waiting on the person, another at work, and a Codex, a pi and an `opencode` thread at rest,
//! each its own tile, in the docked navigator. Beside that navigator the component preview
//! then makes the design-system pair, `design-system` and `design-system-dark`. The agents are
//! stand-ins: hooks handed to the worker's control socket for Claude Code, recordings for the rest
//! ([`crate::showcase::Threads`]).

use slopty_e2e::{Command, Dump};

use crate::gallery::{PARK, PINNED_AT, STEP, golden};
use crate::showcase::{Threads, start_thread};

/// Whether the navigator's rows lead as they should, named for a screen reader: an agent at
/// rest with its owner's mark, one that works or waits on the person with that state, which a
/// row's lead says once (`docs/decisions/ui.md`, "A tab's lead says its state once").
fn marked(d: &Dump) -> bool {
    ["Codex", "pi", "Needs you", "Working"]
        .iter()
        .all(|name| d.a11y_node("Image", Some(name)).is_some())
}

#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn each_agent_wears_its_mark_in_the_navigator() {
    let mut t = Threads::begin().await;
    for agent in ["Codex", "pi", "opencode"] {
        assert!(start_thread(&mut t, agent).await.is_some(), "{agent}'s thread opens a tile");
    }
    // Claude Code in the shell asks the person; in a second terminal it works.
    let asks = t.shell.clone();
    let drv = &mut t.stack.driver;
    let before: Vec<String> =
        drv.dump().await.unwrap().terminals.into_iter().map(|t| t.session).collect();
    drv.open(&["sh", "-c", "exec cat"], 1).await.unwrap();
    let dump = drv
        .wait_for("a second terminal", STEP, |d| d.terminals.len() > before.len())
        .await
        .unwrap();
    let works = dump
        .terminals
        .into_iter()
        .map(|t| t.session)
        .find(|s| !before.contains(s))
        .expect("the second terminal");
    let stack = &t.stack;
    for (session, event, fields) in [
        (&asks, "SessionStart", r#","source":"startup""#),
        (&works, "SessionStart", r#","source":"startup""#),
        (&asks, "UserPromptSubmit", r#","prompt":"Run the tests""#),
        (&works, "UserPromptSubmit", r#","prompt":"Tidy the imports""#),
        (&asks, "PermissionRequest", r#","tool_name":"Bash""#),
    ] {
        stack.play_hook(session, event, fields).await.unwrap();
    }
    let drv = &mut t.stack.driver;
    drv.wait_for("one agent asking, one at work", STEP, |d| {
        d.terminal(&asks).is_some_and(|t| t.agent.as_deref() == Some("needs-you"))
            && d.terminal(&works).is_some_and(|t| t.agent.as_deref() == Some("working"))
    })
    .await
    .unwrap();
    drv.ok(&Command::PinClock { at_ms: Some(PINNED_AT) }).await.unwrap();
    drv.ok(&Command::Resize { width: 1280.0, height: 800.0 }).await.unwrap();
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    let dump = drv
        .wait_for("the navigator docked, each row marked", STEP, |d| {
            d.a11y_node("Navigation", Some("Navigator")).is_some() && marked(d)
        })
        .await
        .unwrap();
    // The place no longer names the agent in words: the mark says it.
    let named = dump.a11y.iter().filter_map(|n| n.label.as_deref());
    assert!(!named.into_iter().any(|l| l.contains("Claude Code \u{b7}")), "{:#?}", dump.a11y);
    let dir = t.stack.dir.path().to_path_buf();
    golden(drv, &dir, "agent-marks").await;
    t.stack.set_appearance("dark").unwrap();
    let drv = &mut t.stack.driver;
    drv.wait_for("the dark theme", STEP, |d| d.dark).await.unwrap();
    golden(drv, &dir, "agent-marks-dark").await;

    // The design system on one page beside this navigator, both themes: every surface over its
    // samples and every control in its states, with a toast up (`crate::components`).
    drv.ok(&Command::Resize { width: DESIGN.0, height: DESIGN.1 }).await.unwrap();
    drv.keys("cmd-shift-p").await.unwrap();
    drv.wait_for("the palette", STEP, |d| d.a11y_node("Dialog", Some("Commands")).is_some())
        .await
        .unwrap();
    drv.type_text(PREVIEW).await.unwrap();
    drv.wait_for("its line", STEP, |d| {
        d.a11y.iter().any(|n| {
            n.role == "ListBoxOption" && n.label.as_deref().is_some_and(|l| l.starts_with(PREVIEW))
        })
    })
    .await
    .unwrap();
    drv.keys("enter").await.unwrap();
    drv.ok(&Command::Move { x: PARK.0, y: PARK.1 }).await.unwrap();
    drv.wait_for("the preview beside the navigator", STEP, |d| {
        d.a11y_node("Group", Some(PREVIEW)).is_some() && marked(d)
    })
    .await
    .unwrap();
    golden(drv, &dir, "design-system-dark").await;
    t.stack.set_appearance("light").unwrap();
    let drv = &mut t.stack.driver;
    drv.wait_for("the light theme", STEP, |d| !d.dark).await.unwrap();
    golden(drv, &dir, "design-system").await;
    t.end().await;
}

/// The design-system pair's window: room for the navigator, the thread, the board and the
/// review side by side, and the controls' column.
const DESIGN: (f32, f32) = (1600.0, 1000.0);
/// The page's name in the palette and to a screen reader.
const PREVIEW: &str = "Component preview";
