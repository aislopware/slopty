//! The stream-json stand-in `claude` over pipes, as the driven adapter runs it: it answers
//! `initialize`, echoes each message by its uuid, asks before a tool runs, and keeps the
//! conversation where `--resume` finds it.

#[cfg(test)]
mod claude_stream {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::path::Path;
    use std::process::{Child, ChildStdin, Command, Stdio};
    use std::sync::mpsc::{self, Receiver};
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};

    const PATIENCE: Duration = Duration::from_secs(20);

    struct Driven {
        child: Child,
        stdin: ChildStdin,
        lines: Receiver<Value>,
        seen: Vec<Value>,
    }

    impl Driven {
        fn start(home: &Path, work: &Path, args: &[&str]) -> Self {
            let stub = slopty_testkit::bins::bin(
                env!("CARGO_BIN_EXE_slopty-stub-claude"),
                "slopty-stub-claude",
            );
            let mut command = Command::new(stub);
            slopty_testkit::env::scrub(&mut command, home);
            command
                .args([
                    "-p",
                    "--input-format",
                    "stream-json",
                    "--output-format",
                    "stream-json",
                    "--verbose",
                    "--permission-prompt-tool=stdio",
                    "--replay-user-messages",
                ])
                .args(args)
                .current_dir(work)
                .env("STUB_RECORD", home.join("record.json"))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit());
            let mut child = command.spawn().expect("the stand-in starts");
            let stdin = child.stdin.take().expect("its stdin");
            let stdout = child.stdout.take().expect("its stdout");
            let (tx, lines) = mpsc::channel();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { return };
                    let value =
                        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line}"));
                    if tx.send(value).is_err() {
                        return;
                    }
                }
            });
            Self { child, stdin, lines, seen: Vec::new() }
        }

        fn send(&mut self, value: &Value) {
            writeln!(self.stdin, "{value}").expect("the stand-in reads");
            self.stdin.flush().expect("flushed");
        }

        /// The first line from now on that `wanted` takes.
        fn until(&mut self, what: &str, wanted: impl Fn(&Value) -> bool) -> Value {
            let deadline = Instant::now().checked_add(PATIENCE).expect("a deadline");
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                let line = self.lines.recv_timeout(left).unwrap_or_else(|e| {
                    panic!("no {what} within {PATIENCE:?} ({e}); saw {:#?}", self.seen)
                });
                self.seen.push(line.clone());
                if wanted(&line) {
                    return line;
                }
            }
        }

        fn user(&mut self, uuid: &str, text: &str) {
            self.send(&json!({
                "type": "user", "message": { "role": "user", "content": [{ "type": "text", "text": text }] },
                "parent_tool_use_id": null, "session_id": "", "uuid": uuid,
            }));
        }
    }

    impl Drop for Driven {
        fn drop(&mut self) {
            drop(self.child.kill());
            drop(self.child.wait());
        }
    }

    fn kind(line: &Value) -> &str {
        line.get("type").and_then(Value::as_str).unwrap_or_default()
    }

    #[test]
    fn the_stand_in_speaks_claude_codes_control_protocol() {
        let root = std::env::temp_dir().join(format!("claude-stream-{}", std::process::id()));
        let (home, work) = (root.join("home"), root.join("work"));
        std::fs::create_dir_all(&work).expect("a work directory");
        let session = "11111111-2222-4333-8444-555555555555";
        let mut driven = Driven::start(&home, &work, &["--session-id", session]);

        driven.send(&json!({ "type": "control_request", "request_id": "init1", "request": { "subtype": "initialize" } }));
        let init = driven.until("the initialize answer", |l| kind(l) == "control_response");
        assert_eq!(init.pointer("/response/request_id"), Some(&json!("init1")));
        assert_eq!(init.pointer("/response/response/session_state"), Some(&json!("idle")));
        assert_eq!(init.pointer("/response/pending_permission_requests"), Some(&json!([])));

        let first = "aaaaaaaa-0000-4000-8000-000000000001";
        driven.user(first, "say hello");
        let echo = driven
            .until("the echo", |l| kind(l) == "user" && l.get("isReplay") == Some(&json!(true)));
        assert_eq!(echo.get("uuid"), Some(&json!(first)), "echoed by its own uuid");
        let result = driven.until("the result", |l| kind(l) == "result");
        assert_eq!(result.get("result"), Some(&json!("Done: say hello")));
        assert_eq!(result.get("is_error"), Some(&json!(false)));
        let done = driven.until("completed", |l| kind(l) == "command_lifecycle");
        assert_eq!(done.get("state"), Some(&json!("completed")));
        assert_eq!(done.get("command_uuid"), Some(&json!(first)));

        driven.user("aaaaaaaa-0000-4000-8000-000000000002", "bash touch made.txt");
        let asked = driven.until("the permission prompt", |l| kind(l) == "control_request");
        assert_eq!(asked.pointer("/request/subtype"), Some(&json!("can_use_tool")));
        assert_eq!(asked.pointer("/request/tool_name"), Some(&json!("Bash")));
        // A worker that reattached asks again and is handed the prompt still open.
        driven.send(&json!({ "type": "control_request", "request_id": "init2", "request": { "subtype": "initialize" } }));
        let again = driven.until("the second initialize", |l| kind(l) == "control_response");
        assert_eq!(
            again.pointer("/response/response/session_state"),
            Some(&json!("requires_action"))
        );
        assert_eq!(again.pointer("/response/pending_permission_requests/0"), Some(&asked));
        let id = asked.get("request_id").cloned().expect("a request id");
        driven.send(&json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": id, "response": { "behavior": "allow" } },
        }));
        let ran = driven.until("the tool's result", |l| {
            kind(l) == "user" && l.pointer("/message/content/0/type") == Some(&json!("tool_result"))
        });
        assert_eq!(ran.pointer("/message/content/0/content"), Some(&json!("ran touch made.txt")));
        let result = driven.until("the second result", |l| kind(l) == "result");
        assert_eq!(result.get("result"), Some(&json!("Done: ran it.")));

        let kept = std::fs::read_dir(home.join(".claude/projects"))
            .expect("a projects directory")
            .filter_map(Result::ok)
            .map(|dir| dir.path().join(format!("{session}.jsonl")))
            .find(|file| file.exists())
            .expect("the conversation is kept where Claude Code keeps it");
        let records = std::fs::read_to_string(kept).expect("readable");
        assert!(records.lines().any(|l| l.contains(first)), "the first message is in it");
        let record: Value = serde_json::from_str(
            &std::fs::read_to_string(home.join("record.json")).expect("a record"),
        )
        .expect("JSON");
        assert!(
            record["argv"]
                .as_array()
                .expect("argv")
                .iter()
                .any(|a| a == "--permission-prompt-tool=stdio"),
            "the record holds the arguments: {record}"
        );
        drop(driven);
        drop(std::fs::remove_dir_all(root));
    }
}
