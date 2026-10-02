//! A hang of the app's main thread is filed like a crash: the report lands in the app's crash
//! directory, where `slopty crashes` lists it, and names the work that held the thread.

use std::time::{Duration, Instant};

use slopty_crash::Kind;
use slopty_e2e::{Command, Stack};

/// Past the monitor's poll (2 s) and the idle that seals the hang's interval, with room for a
/// loaded machine.
const WAIT: Duration = Duration::from_secs(20);

/// The self-test holds the main thread for 600 ms once the app has drawn its first frame; a
/// report of it follows, as long as the hold and naming the command's task.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_hang_of_the_main_thread_is_filed_like_a_crash() {
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let crashes = slopty_crash::crash_dir(&stack.path("app"));
    stack.driver.ok(&Command::HoldMain { ms: 600 }).await.unwrap();
    let asked = Instant::now();
    let (hang, stall_ms, cause) = loop {
        let hang = slopty_crash::reports_with(&crashes, None).into_iter().find_map(|report| {
            let Kind::Hang { stall_ms, cause, .. } = &report.kind else { return None };
            let (stall_ms, cause) = (*stall_ms, cause.clone());
            Some((report, stall_ms, cause))
        });
        if let Some(hang) = hang {
            break hang;
        }
        assert!(asked.elapsed() < WAIT, "no hang report in {}", crashes.display());
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert!(stall_ms >= 550, "as long as the hold: {}", hang.headline());
    assert!(cause.contains("slopty-app/src/e2e.rs"), "the command's task: {}", hang.headline());
    assert_eq!(hang.process, slopty_e2e::harness::APP);
    eprintln!("MEASURE {} ({})", hang.headline(), hang.path.display());
    stack.shutdown().await;
}
