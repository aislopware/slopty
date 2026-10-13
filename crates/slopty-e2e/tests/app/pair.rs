//! Two clients on one worker, each driven through its own test socket (`Stack::launch_pair`):
//! the stack's app (`a`) and a second app process with its own data directory and identity,
//! following the same server.
//!
//! The terminal dock (⌘J) of the same place is the same shell on both: what is typed on either
//! runs once, in the order it was typed, and shows on both. A client that dies leaves the other's
//! shell live.

use std::time::Duration;

use slopty_e2e::harness::Stack;
use slopty_e2e::{Driver, Dump};

/// How long a worker round trip (open a shell, run a command) may take.
const STEP: Duration = Duration::from_secs(30);

/// The session of the dock's shell, once it is at its prompt.
fn shell(d: &Dump) -> Option<String> {
    d.terminals.iter().find(|t| t.at_prompt).map(|t| t.session.clone())
}

/// ⌘J's shell on `drv`, at its prompt; its session.
async fn dock_shell(drv: &mut Driver, who: &str) -> String {
    drv.wait_for(&format!("{who}: the worker"), STEP, |d| d.status == "connected").await.unwrap();
    drv.keys("cmd-j").await.unwrap();
    let d = drv
        .wait_for(&format!("{who}: the dock's shell at its prompt"), STEP, |d| {
            shell(d).is_some() && d.focused.starts_with("terminal:")
        })
        .await
        .unwrap();
    shell(&d).unwrap()
}

/// Type `line` and Return on `drv`, then wait until `on` shows its output line.
async fn run_seen(drv: &mut Driver, on: &mut Driver, line: &str, shown: &str) {
    drv.type_text(line).await.unwrap();
    drv.keys("enter").await.unwrap();
    on.wait_for(&format!("{shown} on the other client"), STEP, |d| {
        d.terminals.iter().flat_map(|t| &t.lines).any(|l| l.trim() == shown)
    })
    .await
    .unwrap();
}

/// The dock of the same place opens the same shell on both clients; a command typed on either
/// runs there and shows on both, and their output stands in the order they were typed. With B
/// killed, A's shell still answers.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn the_dock_s_shell_is_one_shell_on_both_clients() {
    let mut pair = Stack::launch_pair("e2e-pair").await.unwrap();
    let a_session = dock_shell(&mut pair.stack.driver, "a").await;
    let b_session = dock_shell(&mut pair.b.driver, "b").await;
    assert_eq!(a_session, b_session, "one shell, not one each");

    let (a, b) = (&mut pair.stack.driver, &mut pair.b.driver);
    run_seen(a, b, "echo typed-on-$((40+2))", "typed-on-42").await;
    run_seen(b, a, "echo typed-on-$((50+1))", "typed-on-51").await;
    let d = a.dump().await.unwrap();
    let lines: Vec<&str> = d.terminals.iter().flat_map(|t| &t.lines).map(|l| l.trim()).collect();
    let first = lines.iter().position(|l| *l == "typed-on-42").unwrap();
    let second = lines.iter().position(|l| *l == "typed-on-51").unwrap();
    assert!(first < second, "in the order typed: {lines:?}");

    let mut child = pair.b.child.take().expect("b runs here");
    child.kill().await.unwrap();
    let a = &mut pair.stack.driver;
    a.type_text("echo after-b-$((60+3))").await.unwrap();
    a.keys("enter").await.unwrap();
    a.wait_for("a's shell still answers", STEP, |d| {
        d.terminals.iter().flat_map(|t| &t.lines).any(|l| l.trim() == "after-b-63")
    })
    .await
    .unwrap();
    pair.stack.shutdown().await;
}
