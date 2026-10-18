//! The iOS app on the simulator, driven through its own test socket.
//!
//! Live (`#[ignore]`), run by `cargo xtask e2e ios [--sim iphone|ipad]`: the daemons
//! run on the Mac as for the app self-test, the app runs in the booted simulator named by
//! `SLOPTY_SIM_UDID` and binds its socket on the shared file system. The phone's anatomy (its
//! Chats page, a chat over the whole screen) is `compact`; its frames are compared with a
//! golden per device, `ios-phone-*.png` or `ios-pad-*.png`. What a chat asks and opens there (a
//! question, an approval, a shell, the files, an iPad turned to landscape) is `screens`. A finger
//! over a page card, as UIKit delivers it, is `pages`.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slopty_e2e::harness::{Simulator, Stack};

    /// Per-step wait.
    const STEP: Duration = Duration::from_secs(30);

    /// The booted simulator `cargo xtask e2e ios` installed the app on.
    fn simulator() -> Simulator {
        let udid = std::env::var("SLOPTY_SIM_UDID").expect("SLOPTY_SIM_UDID (a booted simulator)");
        let bundle_id = std::env::var("SLOPTY_SIM_BUNDLE_ID").expect("SLOPTY_SIM_BUNDLE_ID");
        Simulator { udid, bundle_id }
    }

    /// A note's "Deny" pressed while the app is not running: the system launches it in the
    /// background and hands it the press before any window, so the press is answered with no
    /// window, through a link of its own to the server its settings name. The thread's request
    /// (a Claude Code thread, the stand-in, asking to run a command) is answered and gone.
    /// Pressed again on the prompt now gone, the press says so in a note in place of the one
    /// pressed.
    ///
    /// simctl delivers pushes but cannot press a note's button, so the press is handed in at
    /// launch through the e2e build's seam ([`slopty_e2e::LAUNCH_TAP_ENV`]), along the path the
    /// system's launching response takes.
    #[tokio::test]
    #[ignore = "live: cargo xtask e2e ios"]
    async fn a_killed_app_answers_a_note_s_deny_with_no_window() {
        use serde_json::json;
        use slopty_e2e::harness::slopty_json;

        let mut stack =
            Stack::launch_on_simulator_with_claude("e2e-ios-cold", simulator()).await.unwrap();
        // Notes allowed with no prompt, so the press's own note can be shown.
        stack.driver.push_register(&[0; 32]).await.unwrap();

        let cli = stack.path("cli");
        let address = stack.server.address().to_owned();
        let folder = stack.path("repo");
        std::fs::create_dir_all(&folder).unwrap();
        let folder = folder.to_string_lossy().into_owned();
        let start = ["agent", "start", "e2e-ios-cold", "--folder", &folder, "bash", "rm -rf build"];
        let started = slopty_json(&address, &cli, &start, b"").await.unwrap();
        let agent = started["agent"].as_str().expect("an agent started").to_owned();
        let read_thread = async || {
            let args = ["agent", "read", "--thread", agent.as_str(), "--activity"];
            slopty_json(&address, &cli, &args, b"").await.unwrap()
        };
        let mut read = read_thread().await;
        let waited = std::time::Instant::now();
        while read["requests"][0]["ask"].as_str().is_none() {
            assert!(waited.elapsed() < STEP, "the thread holds no request: {read}");
            tokio::time::sleep(Duration::from_millis(100)).await;
            read = read_thread().await;
        }
        let ask = read["requests"][0]["ask"].as_str().unwrap().to_owned();
        let thread = read["thread"].as_str().unwrap_or(&agent).to_owned();
        // Spelled as `slopty_platform::notify::info` keys a note, which builds on Apple only.
        let pressed = json!({
            "id": "e2e-cold",
            "action": "deny",
            "info": { "thread": thread, "ask": ask },
        })
        .to_string();

        // Killed, then launched by the press.
        let launched = std::time::Instant::now();
        stack.relaunch_on_simulator(&[(slopty_e2e::LAUNCH_TAP_ENV, &pressed)]).await.unwrap();
        let mut read = read_thread().await;
        while read["requests"].as_array().is_none_or(|r| !r.is_empty()) {
            assert!(launched.elapsed() < STEP, "the request still waits: {read}");
            tokio::time::sleep(Duration::from_millis(50)).await;
            read = read_thread().await;
        }
        println!(
            "MEASURE ios-cold: a Deny pressed on a killed app answered the thread within {:.0} ms",
            launched.elapsed().as_secs_f64() * 1e3
        );

        // The same press on the prompt now gone: said in a note in its place.
        stack.relaunch_on_simulator(&[(slopty_e2e::LAUNCH_TAP_ENV, &pressed)]).await.unwrap();
        let waited = std::time::Instant::now();
        loop {
            let notes = stack.driver.delivered().await.unwrap();
            if notes
                .iter()
                .any(|n| n.id == "e2e-cold" && n.title == "That prompt is no longer waiting")
            {
                break;
            }
            assert!(waited.elapsed() < STEP, "no note says the prompt is gone: {notes:?}");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        stack.shutdown().await;
    }
}

/// The phone's anatomy: its Chats page, a chat over the whole screen.
#[cfg(test)]
#[path = "ios/compact.rs"]
mod compact;
/// A finger over a page card, as UIKit delivers it.
#[cfg(test)]
#[path = "ios/pages.rs"]
mod pages;
/// What a chat asks and opens on the device: a question, an approval, a shell, the files.
#[cfg(test)]
#[path = "ios/screens.rs"]
mod screens;
