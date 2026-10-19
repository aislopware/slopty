//! The races a person meets while an agent works, on a real stack: a stand-in Claude Code
//! (`slopty-stub-claude`) with Orca's faults, driven from the app as the person drives it. A
//! message sent while a turn runs goes once, after it; Stop lands on every rung of its ladder
//! (Claude Code acknowledges and goes on, or ignores its `SIGTERM` too); Claude Code dying mid-turn
//! with a grandchild still holding its output ends the turn with why; and a turn whose worker
//! went silent stops reading as working once its lease is old, then reads live again when the
//! worker is back. Each is judged on numbers: what the stand-in heard (`STUB_RECORD`), the
//! thread as the CLI reads it, and the app's accessibility tree.

#![cfg(target_os = "macos")]

use std::time::{Duration, Instant};

use serde_json::Value;
use slopty_e2e::harness::ServedApp;
use slopty_e2e::{A11yNode, Dump};

use crate::served::{
    STEP, click, heard, middle, node, open_thread, says, seen, start, tell, thread_until,
};

/// The worker's name, as the CLI names the machine.
const MACHINE: &str = "races";

/// The longest a stop may take when Claude Code acknowledges it and goes on: the driver's
/// acknowledgement wait and end wait, the process's end, and a read of the app.
const ACKED_STOP: Duration = Duration::from_secs(12);
/// The longest a stop may take when Claude Code answers nothing and ignores `SIGTERM`: ptyd's
/// grace before its `SIGKILL` on top of the above.
const DEAF_STOP: Duration = Duration::from_secs(20);
/// The longest the thread may take to read Claude Code's death while a grandchild holds its
/// output open (40 s here).
const DEATH: Duration = Duration::from_secs(15);
/// The worker's seam: its control streams stall while the file it names exists.
const STALL_ENV: &str = "SLOPTY_E2E_STALL_CONTROL";
/// How long a session's word lasts in the shell (`SESSION_STALE_MS`), and a margin for the
/// relink and a read of the app.
const STALE: Duration = Duration::from_mins(1);

/// A stack whose stand-in records what it heard at `<root>/heard.json`, with `faults` set.
async fn stack(faults: &[(&str, &str)]) -> ServedApp {
    let faults: Vec<(String, String)> =
        faults.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
    ServedApp::launch_rooted(MACHINE, &[], move |root| {
        let mut env = faults;
        env.push((
            "STUB_RECORD".to_owned(),
            root.join("heard.json").to_string_lossy().into_owned(),
        ));
        env
    })
    .await
    .unwrap()
}

/// The user messages the stand-in heard, each as its text, in order.
fn user_messages(record: &Value) -> Vec<String> {
    record["received"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|line| match line {
            Value::String(text) => serde_json::from_str::<Value>(text).ok(),
            other => Some(other.clone()),
        })
        .filter(|line| line["type"] == "user")
        .map(|line| {
            let content = &line["message"]["content"];
            content.as_str().map_or_else(|| content.to_string(), str::to_owned)
        })
        .collect()
}

/// The last turn's state, as the thread read says it.
fn last_turn(read: &Value) -> Value {
    read["turns"].as_array().and_then(|t| t.last()).map_or(Value::Null, |t| t["state"].clone())
}

/// Whether the last turn reads stopped.
fn interrupted(read: &Value) -> bool {
    last_turn(read).to_string().to_lowercase().contains("interrupted")
}

/// The Stop button, when the composer shows one.
fn stop_button(dump: &Dump) -> Option<&A11yNode> {
    node(dump, "Button", "Stop")
}

/// Open the first thread, wait for its turn to show Stop, press it, and time until the worker
/// reads the turn stopped, which must be within `within`; the chat offers Stop no more by then.
async fn press_stop(stack: &mut ServedApp, agent: &str, within: Duration) -> (Duration, Value) {
    let drv = &mut stack.driver;
    open_thread(drv, 1).await;
    let dump = seen(drv, "the Stop button", STEP, |d| stop_button(d).is_some()).await;
    let (x, y) = stop_button(&dump).map(middle).unwrap_or_default();
    let pressed = Instant::now();
    drv.click(x, y).await.unwrap();
    let read = thread_until(stack, agent, "read its turn stopped", interrupted).await;
    let took = pressed.elapsed();
    assert!(took <= within, "the turn read stopped {took:?} after the press: {read}");
    seen(&mut stack.driver, "Stop gone", STEP, |d| stop_button(d).is_none()).await;
    (took, read)
}

/// A message the person sends while a turn runs waits for it, then goes once: the stand-in hears
/// it once, after the running turn's prompt, answers it in a turn of its own, and the chat shows
/// the person's message once.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_message_sent_mid_turn_goes_once_after_it() {
    const FIRST: &str = "slow 30";
    const MORE: &str = "Say more";
    let mut stack = stack(&[]).await;
    let (repo, record) = (stack.path("repo"), stack.path("heard.json"));
    let agent = start(&stack, MACHINE, &repo, FIRST).await;
    let drv = &mut stack.driver;
    open_thread(drv, 1).await;
    seen(drv, "the turn under way", STEP, |d| stop_button(d).is_some()).await;

    tell(&mut stack, MORE).await;
    thread_until(&stack, &agent, "answered the second message", |r| says(r, "Done: Say more"))
        .await;
    let heard = user_messages(&heard(&record));
    let first = heard.iter().position(|m| m.contains(FIRST));
    let more: Vec<usize> =
        heard.iter().enumerate().filter(|(_, m)| m.contains(MORE)).map(|(at, _)| at).collect();
    assert_eq!(more.len(), 1, "sent once: {heard:?}");
    assert!(first.is_some_and(|f| f < more[0]), "after the turn's prompt: {heard:?}");
    let shown = seen(&mut stack.driver, "the reply", STEP, |d| {
        d.a11y.iter().any(|n| n.label.as_deref().is_some_and(|l| l.contains("Done: Say more")))
    })
    .await;
    let mine =
        shown.a11y.iter().filter(|n| n.role == "Paragraph" && n.label.as_deref() == Some(MORE));
    assert_eq!(mine.count(), 1, "shown once:\n{}", slopty_e2e::frame_text(&shown.a11y));
    stack.shutdown().await;
}

/// Stop's first rungs: Claude Code acknowledges the stop and goes on with its turn. Once the turn
/// had its end wait, the process is ended; the turn reads stopped within [`ACKED_STOP`], Stop is
/// gone, and the next message starts Claude Code again on the same conversation.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn stop_lands_when_claude_code_acknowledges_and_goes_on() {
    let mut stack = stack(&[("STUB_ACK_WITHOUT_ENDING", "1")]).await;
    let repo = stack.path("repo");
    let agent = start(&stack, MACHINE, &repo, "slow 300").await;
    let (took, _read) = press_stop(&mut stack, &agent, ACKED_STOP).await;
    println!("MEASURE stop-acked: the turn read stopped {} ms after the press", took.as_millis());

    tell(&mut stack, "Say hello").await;
    thread_until(&stack, &agent, "answered after the stop", |r| says(r, "Done: Say hello")).await;
    stack.shutdown().await;
}

/// Stop's last rung: a Claude Code that answers no interrupt and ignores `SIGTERM` is still
/// ended, by ptyd's `SIGKILL` after its grace; the turn reads stopped within [`DEAF_STOP`] and
/// Stop is gone.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn stop_lands_when_claude_code_ignores_sigterm() {
    let mut stack =
        stack(&[("STUB_IGNORE_SIGTERM", "1"), ("STUB_SLOW_INTERRUPT_MS", "60000")]).await;
    let repo = stack.path("repo");
    let agent = start(&stack, MACHINE, &repo, "slow 300").await;
    let (took, _read) = press_stop(&mut stack, &agent, DEAF_STOP).await;
    println!(
        "MEASURE stop-sigterm-ignored: the turn read stopped {} ms after the press",
        took.as_millis()
    );
    stack.shutdown().await;
}

/// Claude Code dies mid-turn while a process it left holds its output open for 40 s: the thread
/// does not wait on that output. Within [`DEATH`] the turn reads failed with why, the chat no
/// longer offers Stop and says the turn failed, and its details say Claude Code's own last words.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn claude_code_dying_mid_turn_ends_the_turn_with_why() {
    let mut stack = stack(&[("STUB_CRASH_MID_TURN", "1"), ("STUB_GRANDCHILD", "40")]).await;
    let repo = stack.path("repo");
    let began = Instant::now();
    let agent = start(&stack, MACHINE, &repo, "Say hello").await;
    let read = thread_until(&stack, &agent, "read the turn failed", |r| {
        r["turns"].as_array().and_then(|t| t.last()).is_some_and(|turn| {
            turn["state"] == "failed"
                && turn["error"].as_str().is_some_and(|e| e.contains("crashed mid-turn"))
        })
    })
    .await;
    let failed = began.elapsed();
    println!("MEASURE agent-death: the turn read failed {} ms after the start", failed.as_millis());
    assert!(failed < DEATH, "not held by the grandchild: {failed:?}: {read}");
    let drv = &mut stack.driver;
    open_thread(drv, 1).await;
    // The chat says the turn failed, in the person's words, and keeps why behind its details.
    seen(drv, "the failure", STEP, |d| {
        stop_button(d).is_none()
            && d.a11y.iter().any(|n| {
                n.role == "Alert" && n.label.as_deref().is_some_and(|l| l.contains("failed"))
            })
    })
    .await;
    click(drv, "Button", "Show details").await;
    let shown = seen(drv, "why, in the failure's details", STEP, |d| {
        d.a11y.iter().any(|n| {
            n.label.as_deref().is_some_and(|l| l.contains("crashed mid-turn"))
                || n.value.as_deref().is_some_and(|v| v.contains("crashed mid-turn"))
        })
    })
    .await;
    println!("A11Y agent-death\n{}", slopty_e2e::frame_text(&shown.a11y));
    stack.shutdown().await;
}

/// A turn whose worker goes silent (its control streams stall while its connection lives) stops
/// reading as working once its lease is older than the shell's window: the chat's row no longer
/// says Working. When the worker is back, the lease moves again and the row says Working again,
/// and Stop still lands.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_turn_whose_lease_goes_stale_stops_reading_as_working() {
    let stall_dir = tempfile::tempdir().unwrap();
    let stall = stall_dir.path().join("stall");
    let stall_path = stall.to_string_lossy().into_owned();
    let mut stack = stack(&[(STALL_ENV, &stall_path)]).await;
    let repo = stack.path("repo");
    let agent = start(&stack, MACHINE, &repo, "wait 600").await;
    let drv = &mut stack.driver;
    open_thread(drv, 1).await;
    let running = seen(drv, "the turn under way", STEP, |d| stop_button(d).is_some()).await;
    assert!(
        working(&running),
        "the chat's row says Working (its status names itself; a node with no role is no node \
         of the tree):\n{}",
        slopty_e2e::frame_text(&running.a11y)
    );

    std::fs::write(&stall, b"").unwrap();
    let stalled = Instant::now();
    let shown = seen(drv, "the row no longer working", STALE, |d| !working(d)).await;
    let stale = stalled.elapsed();
    println!(
        "MEASURE lease-stale: the row stopped saying Working {} ms after the stall",
        stale.as_millis()
    );
    assert!(
        stale >= Duration::from_secs(40),
        "not before its lease ran out: {stale:?}\n{}",
        slopty_e2e::frame_text(&shown.a11y)
    );

    std::fs::remove_file(&stall).unwrap();
    seen(drv, "the row working again", STEP, working).await;
    let read = thread_until(&stack, &agent, "still working", |r| r["phase"] != Value::Null).await;
    assert!(!interrupted(&read), "the turn ran on: {read}");
    let (took, _read) = press_stop(&mut stack, &agent, ACKED_STOP).await;
    println!(
        "MEASURE lease-stale-stop: the turn read stopped {} ms after the press",
        took.as_millis()
    );
    stack.shutdown().await;
}

/// Whether the sidebar says a chat is working.
fn working(dump: &Dump) -> bool {
    dump.a11y.iter().any(|n| n.label.as_deref() == Some("Working"))
}
