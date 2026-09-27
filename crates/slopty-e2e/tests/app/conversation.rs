//! The conversation face of an agent's terminal, rendered by the app over recorded Claude Code
//! sessions that reach it the way a live one would: the transcripts laid out as Claude Code
//! keeps them, and `SessionStart`, the status line and `PermissionRequest` played through the
//! real relay (`slopty hook`, a child of the test). The worker decodes them, streams the
//! conversation to the app once ⌘J shows the face, and holds the permission prompt for it.
//! Nothing is typed into a shell and no agent runs.
//!
//! Goldens: the face with a held prompt over an edit's diff, light and dark; a subagent's own
//! thread; and the face on a phone-width window, where it is the default.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use slopty_e2e::{Command, Dump, Stack};

use crate::gallery::{STEP, first_shell, gated, golden};

/// The face's renders: room for the list, the approval card and the header's chips.
const WINDOW: (f32, f32) = (1000.0, 720.0);
/// An iPhone's portrait width in points, with the height of one.
const PHONE: (f32, f32) = (393.0, 852.0);

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../slopty-agent/tests/fixtures/conversation")
        .join(name)
}

/// Lay the recorded session `name` under the run's directory as Claude Code keeps it (the
/// main transcript and its subagents' beside it), then start it in `session` through the
/// relay and give it a status line (the model and the context in use). Returns the main
/// transcript's path.
async fn start_recorded(stack: &Stack, session: &str, name: &str) -> String {
    let main = stack.path("projects").join("s1.jsonl");
    let subagents = slopty_agent::conversation::subagents_dir(&main);
    std::fs::create_dir_all(&subagents).unwrap();
    std::fs::copy(fixture(name).join("transcript.jsonl"), &main).unwrap();
    if let Ok(agents) = std::fs::read_dir(fixture(name).join("subagents")) {
        for agent in agents {
            let agent = agent.unwrap().path();
            std::fs::copy(&agent, subagents.join(agent.file_name().unwrap())).unwrap();
        }
    }
    let transcript = main.to_string_lossy().into_owned();
    let start = json!({
        "hook_event_name": "SessionStart", "source": "startup", "session_id": "s1",
        "transcript_path": transcript, "cwd": stack.path("home"),
    });
    let done = stack.relay_hook(session, &[], &start).unwrap().wait().await.unwrap();
    assert!(done.success(), "the relay ran");
    let status = json!({
        "session_id": "s1", "transcript_path": transcript,
        "model": { "id": "claude-opus-5-5", "display_name": "Opus 5.5" },
        "context_window": { "context_window_size": 200_000, "used_percentage": 34 },
    });
    let line = stack.relay_hook(session, &["statusline", "--command", "true"], &status).unwrap();
    assert!(line.wait_with_output().await.unwrap().status.success(), "the status line ran");
    transcript
}

/// The first `PermissionRequest` of the recorded `permission` session, asked in the session
/// whose transcript is `transcript`.
fn permission_request(transcript: &str) -> Value {
    let hooks = std::fs::read_to_string(fixture("permission").join("hooks.jsonl")).unwrap();
    let mut ask = hooks
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|l| l["input"].clone())
        .find(|input| input["hook_event_name"] == "PermissionRequest")
        .unwrap();
    ask["transcript_path"] = Value::String(transcript.to_owned());
    ask["session_id"] = Value::String("s1".to_owned());
    ask
}

/// The labels of the nodes with `role`.
fn labels(d: &Dump, role: &str) -> Vec<String> {
    d.a11y.iter().filter(|n| n.role == role).filter_map(|n| n.label.clone()).collect()
}

fn has(d: &Dump, role: &str, label: &str) -> bool {
    d.a11y_node(role, Some(label)).is_some()
}

/// An agent working through an edit asks to run a command: the face shows the turn it is on,
/// the edit's diff in it, and the prompt in the composer's place with its three answers; the
/// header says how many lines changed, how full the context is and which model it is. The
/// same, dark.
#[tokio::test]
async fn the_face_holds_a_prompt_over_the_turn_it_interrupts() {
    if !gated() {
        return;
    }
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    let session = dump.terminals[0].session.clone();
    let transcript = start_recorded(&stack, &session, "edit").await;
    stack.driver.keys("cmd-j").await.unwrap();
    stack
        .driver
        .wait_for("the conversation", STEP, |d| {
            has(d, "Group", "Conversation") && !labels(d, "Article").is_empty()
        })
        .await
        .unwrap();
    let _held = stack.relay_hook(&session, &[], &permission_request(&transcript)).unwrap();
    let drv = &mut stack.driver;
    let dump = drv
        .wait_for("the prompt held for the face", STEP, |d| {
            has(d, "AlertDialog", "Claude wants to use Bash")
        })
        .await
        .unwrap();
    for answer in ["Allow once", "Always allow", "Deny"] {
        assert!(labels(&dump, "Button").iter().any(|l| l == answer), "{answer}: {:#?}", dump.a11y);
    }
    golden(drv, &dir, "conversation").await;
    stack.set_appearance("dark").unwrap();
    let drv = &mut stack.driver;
    drv.wait_for("the dark theme", STEP, |d| d.dark).await.unwrap();
    golden(drv, &dir, "conversation-dark").await;
    stack.shutdown().await;
}

/// A subagent's card opens its own thread, under a bar that names it and leads back.
#[tokio::test]
async fn a_subagent_has_a_thread_of_its_own() {
    if !gated() {
        return;
    }
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    let session = dump.terminals[0].session.clone();
    start_recorded(&stack, &session, "tools").await;
    let drv = &mut stack.driver;
    drv.keys("cmd-j").await.unwrap();
    drv.wait_for("the conversation", STEP, |d| has(d, "Group", "Conversation")).await.unwrap();
    // Every step shown: the settled turn opens, the subagent's card with it.
    drv.keys("ctrl-o ctrl-o").await.unwrap();
    let dump = drv
        .wait_for("the subagent's card", STEP, |d| {
            d.a11y
                .iter()
                .any(|n| n.label.as_deref().is_some_and(|l| l.starts_with("Subagent Count lines")))
        })
        .await
        .unwrap();
    let card = dump
        .a11y
        .iter()
        .find(|n| n.label.as_deref().is_some_and(|l| l.starts_with("Subagent Count lines")))
        .unwrap();
    let [x, y, w, h] = card.bounds;
    drv.click(x + w / 2.0, y + h / 2.0).await.unwrap();
    drv.wait_for("the subagent's thread", STEP, |d| has(d, "Navigation", "Subagent Count lines"))
        .await
        .unwrap();
    golden(drv, &dir, "conversation-subagent").await;
    stack.shutdown().await;
}

/// On a phone-width window an agent's tile opens on its conversation, with no key pressed,
/// and a prompt the agent asks takes the composer's place there too.
#[tokio::test]
async fn a_phone_opens_on_the_conversation() {
    if !gated() {
        return;
    }
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    stack.driver.ok(&Command::Resize { width: PHONE.0, height: PHONE.1 }).await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    let session = dump.terminals[0].session.clone();
    let transcript = start_recorded(&stack, &session, "edit").await;
    stack
        .driver
        .wait_for("the conversation, unasked", STEP, |d| {
            has(d, "Group", "Conversation") && !labels(d, "Article").is_empty()
        })
        .await
        .unwrap();
    let _held = stack.relay_hook(&session, &[], &permission_request(&transcript)).unwrap();
    let drv = &mut stack.driver;
    drv.wait_for("the prompt held for the face", STEP, |d| {
        has(d, "AlertDialog", "Claude wants to use Bash")
    })
    .await
    .unwrap();
    golden(drv, &dir, "conversation-phone").await;
    stack.shutdown().await;
}
