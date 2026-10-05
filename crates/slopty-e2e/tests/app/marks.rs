//! Each agent wears its owner's mark where it is listed: Claude Code the Claude spark, Codex
//! the Blossom, pi its cells and an ACP agent the neutral glyph, each in the ink of its row's
//! words, leading the row whatever the agent is doing, while how it is doing ends the row.
//!
//! The one golden where a mark's size or centring shows, light and dark: a Claude Code agent
//! waiting on the person, another at work, and a Codex, a pi and an `opencode` thread at rest,
//! each its own tile, in the docked navigator. The agents are stand-ins: hooks handed to the
//! worker's control socket for Claude Code, recordings for the rest
//! ([`crate::showcase::Threads`]).

use slopty_e2e::{Command, Dump};

use crate::gallery::{PARK, PINNED_AT, STEP, golden};
use crate::showcase::{Threads, start_thread};

/// Whether the navigator's rows lead with each agent's mark, named for a screen reader.
fn marked(d: &Dump) -> bool {
    ["Claude Code", "Codex", "pi"].iter().all(|name| d.a11y_node("Image", Some(name)).is_some())
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
    t.end().await;
}
