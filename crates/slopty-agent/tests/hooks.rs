//! Claude Code's hook payloads as Slopty's relay hears them, captured from a real Claude Code
//! beside the conversation fixtures (`tests/fixtures/conversation/<scenario>/hooks.jsonl`, written
//! by `cargo xtask fixtures claude` with its sink registered for the relay's events). Every
//! scenario's payloads are read in order through [`Hook::signal`] and pinned by one snapshot; a
//! snapshot that moves after a recapture is the payloads moving under the session's status.

#[cfg(test)]
mod hooks {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use serde_json::Value;
    use slopty_agent::hooks::{EVENTS, Hook, Signal};

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/conversation")
    }

    /// Every recorded scenario, by name.
    fn scenarios() -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(fixtures())
            .expect("the fixtures")
            .map(|e| e.expect("an entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A scenario's payloads, as Claude Code handed them to the hook, in the order it ran them.
    fn payloads(name: &str) -> Vec<Value> {
        let text =
            std::fs::read_to_string(fixtures().join(name).join("hooks.jsonl")).expect("hooks");
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let record: Value = serde_json::from_str(l).expect("a record");
                record.get("input").cloned().expect("the payload")
            })
            .collect()
    }

    /// The session id the scenario's main transcript names.
    fn session(name: &str) -> String {
        let text = std::fs::read_to_string(fixtures().join(name).join("transcript.jsonl"))
            .expect("a transcript");
        text.lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find_map(|r| r.get("sessionId").and_then(Value::as_str).map(str::to_owned))
            .expect("a session id")
    }

    /// What each payload says of the session, scenario by scenario: an event Slopty's relay is
    /// registered for, of the session the transcript is, read as a status. A call put to the
    /// person asks for permission and says what it would do, and a session that ends says so.
    #[test]
    fn hooks_say_the_status() {
        let mut said = BTreeMap::new();
        for name in scenarios() {
            let session = session(&name);
            let mut lines = Vec::new();
            let mut signals = Vec::new();
            for payload in payloads(&name) {
                let hook = Hook::parse(&payload.to_string()).expect("a payload Slopty reads");
                assert!(EVENTS.contains(&hook.event.as_str()), "{name}: {}", hook.event);
                assert_eq!(hook.session_id.as_deref(), Some(session.as_str()), "{name}");
                let transcript = hook.transcript_path.as_deref().unwrap_or_default();
                let jsonl = Path::new(transcript).extension().is_some_and(|e| e == "jsonl");
                assert!(jsonl, "{name}: {transcript}");
                let signal = hook.signal();
                if hook.event == "PermissionRequest" {
                    assert!(
                        matches!(&signal, Signal::Asks { kind: "permission", text } if !text.is_empty()),
                        "{name}: {signal:?}"
                    );
                }
                let tool = hook.tool_name.as_deref().map(|t| format!(" {t}")).unwrap_or_default();
                lines.push(format!("{}{tool}: {signal:?}", hook.event));
                signals.push(signal);
            }
            assert_eq!(signals.first(), Some(&Signal::Began), "{name} begins");
            assert_eq!(signals.last(), Some(&Signal::Ended), "{name} ends");
            said.insert(name, lines);
        }
        insta::assert_yaml_snapshot!(said);
    }
}
