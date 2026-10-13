//! The iOS app on the simulator, driven through its own test socket.
//!
//! Live (`#[ignore]`), run by `cargo xtask e2e ios [--sim iphone|ipad]`: the daemons
//! run on the Mac as for the app self-test, the app runs in the booted simulator named by
//! `SLOPTY_SIM_UDID` and binds its socket on the shared file system. The phone's anatomy (its
//! Sessions page, a chat over the whole screen) is `compact`; its frames are compared with a
//! golden per device, `ios-phone-*.png` or `ios-pad-*.png`.

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

    /// A made-up Claude Code session of `turns` turns, each the person's prompt and a long
    /// answer, as its transcript keeps them: long enough that the first prompt is far above
    /// the tail on a phone.
    fn long_session(turns: usize) -> String {
        use serde_json::json;
        let mut out = String::new();
        let mut parent: Option<String> = None;
        let mut n = 0_u64;
        let mut push = |mut record: serde_json::Value| {
            n = n.saturating_add(1);
            let uuid = format!("00000000-0000-4000-9000-{n:012}");
            record["uuid"] = json!(uuid);
            record["parentUuid"] = parent.clone().map_or(serde_json::Value::Null, |p| json!(p));
            record["timestamp"] = json!(format!("2026-10-05T09:{:02}:{:02}.000Z", n / 60, n % 60));
            record["sessionId"] = json!("s1");
            record["isSidechain"] = json!(false);
            out.push_str(&record.to_string());
            out.push('\n');
            parent = Some(uuid);
        };
        for turn in 0..turns {
            push(json!({ "type": "user", "message": { "role": "user",
                "content": format!("Step {turn}: tighten the parser's error path") } }));
            let answer = (0..6)
                .map(|p| {
                    format!("Paragraph {p} of turn {turn}: the span is kept and the cursor reset.")
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            push(json!({ "type": "assistant", "message": {
                "id": format!("msg_{turn}"), "role": "assistant", "model": "claude-opus-5-5",
                "content": [{ "type": "text", "text": answer }], "stop_reason": "end_turn",
                "usage": { "input_tokens": 12, "output_tokens": 420 } } }));
        }
        out
    }

    /// A note's "Deny" pressed while the app is not running: the system launches it in the
    /// background and hands it the press before any window, so the press is answered with no
    /// window, through a link of its own to the server its settings name. The held relay gets
    /// its answer and the thread's request is gone. Pressed again on the prompt now gone, the
    /// press says so in a note in place of the one pressed.
    ///
    /// simctl delivers pushes but cannot press a note's button, so the press is handed in at
    /// launch through the e2e build's seam ([`slopty_e2e::LAUNCH_TAP_ENV`]), along the path the
    /// system's launching response takes.
    #[tokio::test]
    #[ignore = "live: cargo xtask e2e ios"]
    async fn a_killed_app_answers_a_note_s_deny_with_no_window() {
        use serde_json::json;
        use slopty_e2e::harness::slopty_json;

        let mut stack = Stack::launch_on_simulator("e2e-ios-cold", simulator()).await.unwrap();
        let dump = stack
            .driver
            .wait_for("the first shell with a prompt", STEP, |d| {
                d.status == "connected"
                    && d.terminals.iter().any(|t| t.rows.iter().any(|r| !r.is_empty()))
            })
            .await
            .unwrap();
        let session = dump.terminals[0].session.clone();
        // Notes allowed with no prompt, so the press's own note can be shown.
        stack.driver.push_register(&[0; 32]).await.unwrap();

        let transcript = stack.path("projects").join("s1.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, long_session(1)).unwrap();
        let transcript = transcript.to_string_lossy().into_owned();
        let start = json!({
            "hook_event_name": "SessionStart", "source": "startup", "session_id": "s1",
            "transcript_path": transcript, "cwd": stack.path("home"),
        });
        let started = stack.relay_hook(&session, &[], &start).unwrap().wait().await.unwrap();
        assert!(started.success(), "the relay ran");
        let ask = json!({
            "hook_event_name": "PermissionRequest", "session_id": "s1",
            "transcript_path": transcript, "cwd": stack.path("home"),
            "tool_name": "Bash", "tool_input": { "command": "rm -rf build" },
        });
        let mut held = stack.relay_hook(&session, &[], &ask).unwrap();

        let term = format!("e2e-ios-cold/{session}");
        let cli = stack.path("cli");
        let address = stack.server.address().to_owned();
        let read_thread = async || {
            let args = ["agent", "read", "--term", term.as_str()];
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
        let held_on: slopty_core::WorkerId = read["worker"].as_str().unwrap().parse().unwrap();
        // Spelled as `slopty_platform::notify::info` keys a note, which builds on Apple only.
        let pressed = json!({
            "id": "e2e-cold",
            "action": "deny",
            "info": {
                "worker": held_on.as_uuid().as_u128().to_string(),
                "session": session,
                "ask": ask,
            },
        })
        .to_string();

        // Killed, then launched by the press.
        let launched = std::time::Instant::now();
        stack.relaunch_on_simulator(&[(slopty_e2e::LAUNCH_TAP_ENV, &pressed)]).await.unwrap();
        let status = tokio::time::timeout(STEP, held.wait()).await;
        let status = status.expect("the held relay got its answer").unwrap();
        println!(
            "MEASURE ios-cold: a Deny pressed on a killed app reached the held relay within {:.0} ms",
            launched.elapsed().as_secs_f64() * 1e3
        );
        assert!(status.success(), "the relay ends well on an answer: {status}");
        let read = read_thread().await;
        assert_eq!(read["requests"].as_array().map(Vec::len), Some(0), "{read}");

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

/// The phone's anatomy: its Sessions page, a chat over the whole screen.
#[cfg(test)]
#[path = "ios/compact.rs"]
mod compact;
