//! The conversations recorded under `slopty-agent`'s fixtures, decoded by the worker's own
//! decoder into the events a follower is sent, and a held permission prompt to answer.

use std::path::Path;

use slopty_core::{SessionId, WallMs};
use slopty_proto::conversation::{
    BashDetail, Change, Clipped, ConversationEvent, Grant, PermissionPrompt, ShellStatus,
    Suggestion, ToolDetail,
};

/// What a follower of the recorded session `name` is sent: its every thread from the start,
/// then `Current`.
pub fn events(name: &str) -> Vec<ConversationEvent> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../slopty-agent/tests/fixtures/conversation")
        .join(name);
    let mut files = vec![dir.join("transcript.jsonl")];
    if let Ok(agents) = std::fs::read_dir(dir.join("subagents")) {
        let mut agents: Vec<_> = agents.map(|e| e.unwrap().path()).collect();
        agents.sort();
        files.extend(agents);
    }
    let mut decoder = slopty_agent::conversation::Conversation::default();
    let mut changes = vec![Change::Reset { thread: None }];
    for file in files {
        let mut tail = slopty_agent::transcript::Tail::default();
        changes.extend(decoder.read(&mut tail, &file).unwrap());
    }
    vec![ConversationEvent::Changes(changes), ConversationEvent::Current]
}

/// A 1600 × 1000 PNG's header, base64 as a transcript holds a picture: enough for the decoder
/// to describe it, which is all a face needs until it asks for the bytes.
pub const PICTURE: &str = "iVBORw0KGgoAAAANSUhEUgAABkAAAAPoCAYAAAA=";

/// A session made up to show the work a turn does beyond words, laid out in `dir` as Claude
/// Code keeps it: a prompt with a pasted picture, the model's thinking, a plan the person
/// approved, a task list under way, a `Read` of a screenshot, and a build started in the
/// background that is still printing. Returns what a follower is sent: the conversation, the
/// build's last lines, `Current`.
pub fn work(dir: &Path) -> Vec<ConversationEvent> {
    use serde_json::{Value, json};
    let main = dir.join("s1.jsonl");
    let tasks = dir.join("tasks");
    std::fs::create_dir_all(&tasks).unwrap();
    let output = tasks.join("b1.output");
    std::fs::write(
        &output,
        "   Compiling slopty-theme v0.1.0\n   Compiling slopty-ui v0.1.0\n   Compiling slopty v0.1.0\n",
    )
    .unwrap();
    let picture = json!({
        "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": PICTURE },
    });
    let mut parent = Value::Null;
    let mut records = Vec::new();
    let mut push = |uuid: &str, second: u32, mut record: Value| {
        record["uuid"] = json!(uuid);
        record["parentUuid"] = parent.clone();
        record["timestamp"] = json!(format!("2026-09-27T04:00:{second:02}.000Z"));
        record["sessionId"] = json!("s1");
        parent = json!(uuid);
        records.push(record.to_string());
    };
    let assistant = |content: Value| {
        json!({ "type": "assistant", "message": {
            "role": "assistant", "model": "claude-opus-5-5", "content": [content],
        }})
    };
    let result = |id: &str, content: Value, structured: Value| {
        json!({ "type": "user", "message": { "role": "user", "content": [{
            "type": "tool_result", "tool_use_id": id, "content": content,
        }]}, "toolUseResult": structured })
    };
    push(
        "u1",
        0,
        json!({ "type": "user", "message": { "role": "user", "content": [
            picture,
            { "type": "text", "text": "The header clips its chips on a narrow window, as here." },
        ]}}),
    );
    push(
        "a1",
        9,
        assistant(json!({ "type": "thinking", "thinking":
        "The chips never shrink, so the title takes the squeeze.\nThe header should give way \
         from the right: the model first, then the context."})),
    );
    let plan = "# Let the header's chips give way\n\n1. Measure the chips before the title.\n\
                2. Drop the model chip first, then the context.\n3. Keep the changes chip last.";
    push(
        "a2",
        12,
        assistant(json!({
            "type": "tool_use", "id": "t1", "name": "ExitPlanMode", "input": { "plan": plan },
        })),
    );
    push("r1", 30, result("t1", json!("User has approved your plan."), json!({ "plan": plan })));
    let todos = json!([
        { "content": "Measure the chips", "activeForm": "Measuring the chips", "status": "completed" },
        { "content": "Drop chips from the right", "activeForm": "Dropping chips", "status": "in_progress" },
        { "content": "Check a phone-width window", "activeForm": "Checking", "status": "pending" },
    ]);
    push(
        "a3",
        31,
        assistant(json!({
            "type": "tool_use", "id": "t2", "name": "TodoWrite", "input": { "todos": todos },
        })),
    );
    push("r2", 31, result("t2", json!("Todos have been modified successfully."), json!({})));
    push(
        "a4",
        40,
        assistant(json!({
            "type": "tool_use", "id": "t3", "name": "Read",
            "input": { "file_path": "/work/shots/header.png" },
        })),
    );
    push(
        "r3",
        41,
        result(
            "t3",
            json!([picture]),
            json!({
                "type": "image", "file": { "base64": PICTURE, "type": "image/png" },
            }),
        ),
    );
    push(
        "a5",
        50,
        assistant(json!({
            "type": "tool_use", "id": "t4", "name": "Bash", "input": {
                "command": "cargo build --release", "description": "Build the release binary",
                "run_in_background": true,
            },
        })),
    );
    let started = format!(
        "Command running in background with ID: b1. Output is being written to: {}. You will be \
         notified when it completes.",
        output.display()
    );
    push(
        "r4",
        51,
        result(
            "t4",
            json!(started),
            json!({
                "stdout": "", "stderr": "", "interrupted": false, "backgroundTaskId": "b1",
            }),
        ),
    );
    push(
        "a6",
        55,
        assistant(json!({ "type": "text", "text":
        "The build runs in the background; the chips give way from the right now." })),
    );
    std::fs::write(&main, [records.join("\n"), String::new()].join("\n")).unwrap();
    let mut transcripts = slopty_agent::conversation::Transcripts::default();
    let mut changes = vec![Change::Reset { thread: None }];
    changes.extend(transcripts.read(&main, &[]));
    vec![
        ConversationEvent::Changes(changes),
        ConversationEvent::Output(transcripts.outputs()),
        ConversationEvent::Current,
    ]
}

/// A long made-up session of `turns` turns, laid out in `dir` as Claude Code keeps it, the one
/// the app's frame probe draws: each turn a prompt, thinking, a Markdown answer with a list and
/// a fenced block, a 30-line test run, an edit with its diff, a read and a closing line, every
/// assistant record with a model and usage. Returns what a follower is sent.
pub fn long(dir: &Path, turns: usize) -> Vec<ConversationEvent> {
    use serde_json::{Value, json};
    let main = dir.join("s1.jsonl");
    let mut records = Vec::new();
    let mut parent = Value::Null;
    let mut n = 0_u32;
    let mut push = |mut record: Value| {
        n = n.saturating_add(1);
        let uuid = format!("r{n}");
        let (minutes, seconds) = (n / 60 % 60, n % 60);
        record["uuid"] = json!(uuid);
        record["parentUuid"] = parent.clone();
        record["timestamp"] = json!(format!("2026-09-27T04:{minutes:02}:{seconds:02}.000Z"));
        record["sessionId"] = json!("s1");
        parent = json!(uuid);
        records.push(record.to_string());
    };
    let assistant = |content: Value| {
        let usage = json!({
            "input_tokens": 12, "cache_read_input_tokens": 41_000,
            "cache_creation_input_tokens": 800, "output_tokens": 420,
        });
        json!({ "type": "assistant", "message": {
            "role": "assistant", "model": "claude-opus-5-5", "content": [content], "usage": usage,
        }})
    };
    let result = |id: &str, content: Value, structured: Value| {
        json!({ "type": "user", "message": { "role": "user", "content": [{
            "type": "tool_result", "tool_use_id": id, "content": content,
        }]}, "toolUseResult": structured })
    };
    for turn in 0..turns {
        push(json!({ "type": "user", "message": { "role": "user", "content": format!(
            "Step {turn}: tighten the parser's error path and run the tests again"
        )}}));
        push(assistant(json!({ "type": "thinking", "thinking": format!(
            "The parser returns early on turn {turn}; the error path loses the span."
        )})));
        push(assistant(json!({ "type": "text", "text": format!(
            "Turn {turn}. The failure comes from **two** places:\n\n\
             - `parse_header` drops the span when the line is empty\n\
             - `recover` retries without resetting the cursor\n\n\
             ```rust\nfn recover(&mut self) -> Result<(), Error> {{\n    self.cursor = self.mark;\n    self.next()\n}}\n```\n\n\
             I will fix both and run the suite."
        )})));
        let bash = format!("toolu_b{turn}");
        push(assistant(json!({
            "type": "tool_use", "id": bash, "name": "Bash",
            "input": { "command": "cargo test -p parser", "description": "Run the parser tests" },
        })));
        let stdout = (0..30).map(|i| format!("test case_{i:02} ... ok")).collect::<Vec<_>>();
        let stdout = stdout.join("\n");
        push(result(
            &bash,
            json!(stdout),
            json!({ "stdout": stdout, "stderr": "", "interrupted": false }),
        ));
        let edit = format!("toolu_e{turn}");
        push(assistant(json!({
            "type": "tool_use", "id": edit, "name": "Edit",
            "input": {
                "file_path": "/work/src/parser.rs",
                "old_string": "self.next()", "new_string": "self.cursor = self.mark;\nself.next()",
            },
        })));
        push(result(
            &edit,
            json!("The file was updated."),
            json!({
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
            }),
        ));
        let read = format!("toolu_r{turn}");
        push(assistant(json!({
            "type": "tool_use", "id": read, "name": "Read",
            "input": { "file_path": "/work/src/lexer.rs" },
        })));
        push(result(
            &read,
            json!("1\tuse std::str;"),
            json!({ "file": { "startLine": 1, "numLines": 120, "totalLines": 120 } }),
        ));
        push(assistant(json!({ "type": "text", "text": format!(
            "Done with turn {turn}: the tests pass and the span survives an empty line."
        )})));
    }
    std::fs::write(&main, [records.join("\n"), String::new()].join("\n")).unwrap();
    let mut transcripts = slopty_agent::conversation::Transcripts::default();
    let mut changes = vec![Change::Reset { thread: None }];
    changes.extend(transcripts.read(&main, &[]));
    vec![ConversationEvent::Changes(changes), ConversationEvent::Current]
}

/// A recorded session applied as a follower receives it.
pub fn scenario(name: &str) -> super::model::Model {
    let mut model = super::model::Model::default();
    for event in events(name) {
        model.apply(event);
    }
    model
}

/// Claude Code asking to run `npm test` in `session`, with a rule to grant always.
pub fn bash_prompt(session: SessionId, ask: u64) -> PermissionPrompt {
    let command = "npm test";
    PermissionPrompt {
        session,
        ask,
        tool: "Bash".into(),
        detail: ToolDetail::Bash(BashDetail {
            command: Clipped { text: command.into(), lines: 1, chars: 8, full: None },
            description: Some("Run the tests".into()),
            background: false,
            task_id: None,
            status: ShellStatus::Running,
            exit_code: None,
            stdout: None,
            stderr: None,
            output_file: None,
            finished_ms: None,
        }),
        suggestions: vec![Suggestion {
            grant: Grant::Rules {
                behavior: "allow".into(),
                rules: vec!["Bash(npm test:*)".into()],
            },
            destination: Some("localSettings".into()),
        }],
        mode: None,
        asked_ms: WallMs::ZERO,
        until_ms: WallMs::from_millis(600_000),
    }
}
