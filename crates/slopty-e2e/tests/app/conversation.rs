//! The conversation face of an agent's terminal, rendered by the app over recorded Claude Code
//! sessions that reach it the way a live one would: the transcripts laid out as Claude Code
//! keeps them, and `SessionStart`, the status line and `PermissionRequest` played through the
//! real relay (`slopty hook`, a child of the test). The worker decodes them, streams the
//! conversation to the app once ⌘J shows the face, and holds the permission prompt for it.
//! Nothing is typed into a shell and no agent runs.
//!
//! What the model writes before the transcript has it comes from Slopty's Claude Code mod: its
//! recorded events are posted to the worker's mod socket as the mod posts them.
//!
//! Goldens: the face with a held prompt over an edit's diff, light and dark; a subagent's own
//! thread; the face on a phone-width window, where it is the default; and a step the model is
//! still writing.

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
            has(d, "AlertDialog", "Claude wants to run a command")
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

/// A settled turn reads as its prompt, one line of what it did and its answer, with the files
/// it changed; a subagent's card opens its own thread, under a bar that names it and leads
/// back.
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
    // Settled: the prompt, the turn folded to its figures, the answer and the files it changed.
    drv.wait_for("the settled turn", STEP, |d| {
        labels(d, "Button").iter().any(|l| l.starts_with("Worked for 35 s"))
            && has(d, "List", "Changed files")
    })
    .await
    .unwrap();
    golden(drv, &dir, "conversation-settled").await;
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
        has(d, "AlertDialog", "Claude wants to run a command")
    })
    .await
    .unwrap();
    golden(drv, &dir, "conversation-phone").await;
    stack.shutdown().await;
}

/// The recorded mod session `name`: its batches, each put in `session`, and its transcript's
/// records with their stamps taken out, since an entry stamped long before the follower saw
/// the block it settles is an older one.
fn recorded_mod(name: &str, session: &str) -> (Vec<Value>, Vec<String>) {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../slopty-agent/tests/fixtures/mod").join(name);
    let batches = std::fs::read_to_string(dir.join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| {
            let mut batch: Value = serde_json::from_str(line).unwrap();
            batch["session"] = Value::String(session.to_owned());
            batch
        })
        .collect();
    let records = std::fs::read_to_string(dir.join("transcript.jsonl"))
        .unwrap()
        .lines()
        .map(|line| {
            let mut record: Value = serde_json::from_str(line).unwrap();
            record.as_object_mut().unwrap().remove("timestamp");
            record.to_string()
        })
        .collect();
    (batches, records)
}

/// While the model writes a step, the face shows it after the thread's last entry in a lighter
/// tone: the answer as it grows and the tool call being prepared. When the step stops and the
/// transcript has its entries, they take the live blocks' place.
#[tokio::test]
async fn a_step_being_written_shows_live_until_the_transcript_settles_it() {
    if !gated() {
        return;
    }
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    let dir = stack.dir.path().to_path_buf();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    let session = dump.terminals[0].session.clone();
    let main = stack.path("projects").join("s1.jsonl");
    std::fs::create_dir_all(main.parent().unwrap()).unwrap();
    std::fs::write(&main, "").unwrap();
    let start = json!({
        "hook_event_name": "SessionStart", "source": "startup", "session_id": "s1",
        "transcript_path": main, "cwd": stack.path("home"),
    });
    let done = stack.relay_hook(&session, &[], &start).unwrap().wait().await.unwrap();
    assert!(done.success(), "the relay ran");
    stack.driver.keys("cmd-j").await.unwrap();
    stack
        .driver
        .wait_for("the conversation", STEP, |d| has(d, "Group", "Conversation"))
        .await
        .unwrap();

    // The first step as the model writes it: its answer, then the Bash call's input.
    let (batches, records) = recorded_mod("bash", &session);
    let stop = |b: &Value| b["events"].as_array().unwrap().iter().any(|e| e["kind"] == "stop");
    let first_stop = batches.iter().position(stop).unwrap();
    for batch in &batches[..first_stop] {
        assert_eq!(stack.post_mod(batch).await.unwrap(), 204, "{batch}");
    }
    let writing = "Writing: Let me run it.";
    let drv = &mut stack.driver;
    drv.wait_for("the live step", STEP, |d| {
        has(d, "Article", writing) && has(d, "Status", "Preparing Bash")
    })
    .await
    .unwrap();
    assert!(labels(&drv.dump().await.unwrap(), "Article").iter().all(|l| l == writing));
    golden(drv, &dir, "conversation-live").await;

    // The step stops and the transcript gets the answer and the call: both settle, into the
    // turn's fold now that the agent has no turn going.
    assert_eq!(stack.post_mod(&batches[first_stop]).await.unwrap(), 204);
    let result = records.iter().position(|l| l.contains(r#""type":"tool_result""#)).unwrap();
    let mut transcript = records[..result].join("\n");
    transcript.push('\n');
    std::fs::write(&main, transcript).unwrap();
    let dump = stack
        .driver
        .wait_for("the step settled", STEP, |d| {
            // The fold carries the turn's figures from the transcript: the model and what it
            // wrote.
            labels(d, "Button")
                .iter()
                .any(|l| l == "Worked \u{b7} 1 step \u{b7} Haiku 4.5 \u{b7} 20 tokens")
                && !has(d, "Article", writing)
                && !has(d, "Status", "Preparing Bash")
        })
        .await
        .unwrap();
    assert!(
        !labels(&dump, "Article").iter().any(|l| l.starts_with("Writing: ")),
        "{:#?}",
        dump.a11y
    );
    stack.shutdown().await;
}

/// A long session made up for measuring: `turns` turns, each a prompt, an answer in Markdown
/// with a list and a fenced block, a command with its output, an edit with its diff and a
/// read, every assistant record carrying a model and its usage as Claude Code writes them.
fn synthetic_session(turns: usize) -> String {
    struct Log {
        out: String,
        n: u64,
        parent: Option<String>,
    }
    impl Log {
        fn push(&mut self, mut record: Value) -> String {
            self.n = self.n.saturating_add(1);
            let uuid = format!("00000000-0000-4000-9000-{:012}", self.n);
            let secs = self.n.saturating_mul(2);
            let stamp = format!(
                "2026-09-27T{:02}:{:02}:{:02}.000Z",
                4_u64.saturating_add(secs / 3_600),
                (secs / 60) % 60,
                secs % 60
            );
            record["uuid"] = Value::String(uuid.clone());
            record["parentUuid"] = self.parent.clone().map_or(Value::Null, Value::String);
            record["timestamp"] = Value::String(stamp);
            record["sessionId"] = Value::String("s1".to_owned());
            record["isSidechain"] = Value::Bool(false);
            self.out.push_str(&record.to_string());
            self.out.push('\n');
            self.parent = Some(uuid.clone());
            uuid
        }

        fn assistant(&mut self, content: &Value) {
            let usage = json!({
                "input_tokens": 12, "cache_read_input_tokens": 41_000,
                "cache_creation_input_tokens": 800, "output_tokens": 420,
            });
            self.push(json!({
                "type": "assistant",
                "message": {
                    "id": format!("msg_{}", self.n), "role": "assistant",
                    "model": "claude-opus-5-5", "content": [content], "usage": usage,
                    "stop_reason": "tool_use",
                },
            }));
        }
    }
    let mut log = Log { out: String::new(), n: 0, parent: None };
    for turn in 0..turns {
        log.push(json!({
            "type": "user",
            "message": { "role": "user", "content": format!(
                "Step {turn}: tighten the parser's error path and run the tests again"
            )},
        }));
        log.assistant(&json!({ "type": "thinking", "thinking": format!(
            "The parser returns early on turn {turn}; the error path loses the span."
        )}));
        log.assistant(&json!({ "type": "text", "text": format!(
            "Turn {turn}. The failure comes from **two** places:\n\n\
             - `parse_header` drops the span when the line is empty\n\
             - `recover` retries without resetting the cursor\n\n\
             ```rust\nfn recover(&mut self) -> Result<(), Error> {{\n    self.cursor = self.mark;\n    self.next()\n}}\n```\n\n\
             I will fix both and run the suite."
        )}));
        let bash = format!("toolu_b{turn}");
        log.assistant(&json!({
            "type": "tool_use", "id": bash, "name": "Bash",
            "input": { "command": "cargo test -p parser", "description": "Run the parser tests" },
        }));
        let stdout = (0..30).map(|i| format!("test case_{i:02} ... ok")).collect::<Vec<_>>();
        log.push(json!({
            "type": "user",
            "message": { "role": "user", "content": [{
                "type": "tool_result", "tool_use_id": bash, "content": stdout.join("\n"),
                "is_error": false,
            }]},
            "toolUseResult": { "stdout": stdout.join("\n"), "stderr": "", "interrupted": false },
        }));
        let edit = format!("toolu_e{turn}");
        log.assistant(&json!({
            "type": "tool_use", "id": edit, "name": "Edit",
            "input": {
                "file_path": "/work/src/parser.rs",
                "old_string": "self.next()", "new_string": "self.cursor = self.mark;\nself.next()",
            },
        }));
        log.push(json!({
            "type": "user",
            "message": { "role": "user", "content": [{
                "type": "tool_result", "tool_use_id": edit, "content": "The file was updated.",
            }]},
            "toolUseResult": {
                "filePath": "/work/src/parser.rs",
                "structuredPatch": [{
                    "oldStart": 40, "oldLines": 6, "newStart": 40, "newLines": 8,
                    "lines": [
                        "     fn recover(&mut self) -> Result<(), Error> {",
                        "-        self.next()",
                        "+        self.cursor = self.mark;",
                        "+        self.next()",
                        "     }",
                        " ",
                        "+    /// The span of the line being read.",
                        "     fn span(&self) -> Span {",
                    ],
                }],
            },
        }));
        let read = format!("toolu_r{turn}");
        log.assistant(&json!({
            "type": "tool_use", "id": read, "name": "Read",
            "input": { "file_path": "/work/src/lexer.rs" },
        }));
        log.push(json!({
            "type": "user",
            "message": { "role": "user", "content": [{
                "type": "tool_result", "tool_use_id": read, "content": "1\tuse std::str;",
            }]},
            "toolUseResult": { "file": { "startLine": 1, "numLines": 120, "totalLines": 120 } },
        }));
        log.assistant(&json!({ "type": "text", "text": format!(
            "Done with turn {turn}: the tests pass and the span survives an empty line."
        )}));
    }
    log.out
}

/// The face over a long conversation while the model writes an answer, the frame-time case
/// behind `docs/MEASUREMENTS.md` ("the conversation face under a streaming answer"). Runs only
/// with `SLOPTY_SMOOTH_E2E=1`, alone, since other tests' frames would be counted.
///
/// (h) the list following the tail while an answer grows by a piece every 16 ms, as Slopty's
/// Claude Code mod reports it; (i) the same with the reader panning the history at 120
/// events per second.
#[tokio::test]
async fn the_face_draws_a_streaming_answer_within_a_frame() {
    if std::env::var_os("SLOPTY_SMOOTH_E2E").is_none() {
        eprintln!("skipped: set SLOPTY_SMOOTH_E2E=1 (and SLOPTY_APP_E2E=1)");
        return;
    }
    let run = std::time::Duration::from_secs(5);
    let mut stack = Stack::launch("e2e-worker").await.unwrap();
    stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
    let dump = first_shell(&mut stack.driver).await;
    let session = dump.terminals[0].session.clone();
    let main = stack.path("projects").join("s1.jsonl");
    std::fs::create_dir_all(main.parent().unwrap()).unwrap();
    std::fs::write(&main, synthetic_session(80)).unwrap();
    let start = json!({
        "hook_event_name": "SessionStart", "source": "startup", "session_id": "s1",
        "transcript_path": main, "cwd": stack.path("home"),
    });
    let done = stack.relay_hook(&session, &[], &start).unwrap().wait().await.unwrap();
    assert!(done.success(), "the relay ran");
    stack.driver.keys("cmd-j").await.unwrap();
    stack
        .driver
        .wait_for("the conversation", STEP, |d| {
            has(d, "Group", "Conversation") && labels(d, "Article").len() > 4
        })
        .await
        .unwrap();

    let (batches, _) = recorded_mod("bash", &session);
    for batch in &batches[..3] {
        assert_eq!(stack.post_mod(batch).await.unwrap(), 204, "{batch}");
    }
    let turn = batches[1]["events"][0]["turnId"].clone();
    let piece = |text: &str| {
        json!({ "session": session, "events": [{
            "kind": "text", "block": 0, "step": 0, "turnId": turn,
            "model": "claude-haiku-4-5-20251001", "text": text,
        }]})
    };
    let words = [
        "The ",
        "parser ",
        "now ",
        "keeps ",
        "the ",
        "span ",
        "through ",
        "`recover`, ",
        "and ",
        "an ",
        "empty ",
        "line ",
        "reads ",
        "as ",
        "one.\n\n",
        "- ",
        "fixed ",
        "`parse_header`\n",
        "- ",
        "reset ",
        "the ",
        "cursor\n\n",
    ];
    assert_eq!(stack.post_mod(&piece("Writing ")).await.unwrap(), 204);
    stack
        .driver
        .wait_for("the live answer", STEP, |d| {
            labels(d, "Article").iter().any(|l| l.starts_with("Writing: "))
        })
        .await
        .unwrap();

    let region = stack
        .driver
        .dump()
        .await
        .unwrap()
        .a11y_node("Group", Some("Conversation"))
        .expect("the face")
        .bounds;
    let (x, y) = (region[0] + region[2] / 2.0, region[1] + region[3] / 2.0);
    for (scenario, pan) in [
        ("(h) face, following a streaming answer", false),
        ("(i) face, panning while it streams", true),
    ] {
        stack.driver.frames_reset().await.unwrap();
        let begin = tokio::time::Instant::now();
        let mut n = 0_usize;
        while begin.elapsed() < run {
            let word = words[n % words.len()];
            assert_eq!(stack.post_mod(&piece(word)).await.unwrap(), 204);
            if pan {
                let dy = if (n / 60).is_multiple_of(2) { 40.0 } else { -40.0 };
                stack.driver.scroll(x, y, 0.0, dy, false).await.unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(8)).await;
                stack.driver.scroll(x, y, 0.0, dy, false).await.unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(8)).await;
            } else {
                tokio::time::sleep(std::time::Duration::from_millis(16)).await;
            }
            n = n.saturating_add(1);
        }
        let frames = stack.driver.dump().await.unwrap().frames;
        println!("MEASURE {scenario}: {}", frames.row());
        assert!(frames.frames >= 100, "too few frames to judge: {frames:?}");
    }
    stack.shutdown().await;
}
