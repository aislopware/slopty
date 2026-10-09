use serde_json::json;

use super::*;

const MAIN: ThreadId = ThreadId::Main;

fn line(record: &Value) -> String {
    format!("{record}\n")
}

fn user(uuid: &str, parent: Option<&str>, content: &Value) -> Value {
    json!({
        "type": "user", "uuid": uuid, "parentUuid": parent,
        "timestamp": "2026-09-27T03:15:25.849Z",
        "message": { "role": "user", "content": content },
    })
}

fn assistant(uuid: &str, parent: Option<&str>, blocks: &Value) -> Value {
    json!({
        "type": "assistant", "uuid": uuid, "parentUuid": parent,
        "timestamp": "2026-09-27T03:15:26.000Z",
        "message": { "role": "assistant", "content": blocks },
    })
}

fn call(uuid: &str, parent: Option<&str>, id: &str, name: &str, input: &Value) -> Value {
    assistant(
        uuid,
        parent,
        &json!([{ "type": "tool_use", "id": id, "name": name, "input": input }]),
    )
}

fn result(uuid: &str, parent: Option<&str>, id: &str, text: &str, structured: &Value) -> Value {
    let mut record =
        user(uuid, parent, &json!([{ "type": "tool_result", "tool_use_id": id, "content": text }]));
    record["toolUseResult"] = structured.clone();
    record
}

fn tool<'a>(conversation: &'a Conversation, thread: &ThreadId, id: &str) -> &'a ToolCall {
    let entry = conversation.entries(thread).iter().find(|e| e.id == id).expect("the call");
    match &entry.body {
        Body::Tool(call) => call,
        other => panic!("not a call: {other:?}"),
    }
}

/// A result written in a later read than its call updates the same entry, which comes back
/// as an upsert of that id; one that arrives first waits for its call.
#[test]
fn a_result_pairs_with_its_call_whenever_it_arrives() {
    let mut c = Conversation::default();
    let changes =
        c.ingest(&MAIN, &call("a1", None, "t1", "Bash", &json!({"command": "cargo test"})));
    assert!(
        matches!(changes.as_slice(), [Change::Upsert { entry, .. }] if entry.id == "t1"),
        "{changes:?}"
    );
    let ToolDetail::Bash(bash) = &tool(&c, &MAIN, "t1").detail else { panic!("bash") };
    assert_eq!(bash.status, ShellStatus::Running);

    let structured = json!({"stdout": "ok\n", "stderr": "", "interrupted": false});
    let changes = c.ingest(&MAIN, &result("u1", Some("a1"), "t1", "ok", &structured));
    let [Change::Upsert { entry, .. }] = changes.as_slice() else { panic!("{changes:?}") };
    assert_eq!(entry.id, "t1", "the call's entry, updated");
    assert_eq!(c.entries(&MAIN).len(), 1, "a result makes no entry of its own");
    let ran = tool(&c, &MAIN, "t1");
    let ToolDetail::Bash(bash) = &ran.detail else { panic!("bash") };
    assert_eq!((bash.status, bash.exit_code), (ShellStatus::Done, Some(0)));
    assert_eq!(bash.stdout.as_ref().map(|s| s.text.as_str()), Some("ok\n"));
    let outcome = ran.result.as_ref().expect("result");
    assert_eq!((outcome.status, &outcome.text), (ResultStatus::Ok, &None), "stdout says it all");

    // Before its call: parked, then applied.
    let mut c = Conversation::default();
    assert_eq!(
        c.ingest(
            &MAIN,
            &result("u2", None, "t2", "Exit code 3\nboom", &json!("Error: Exit code 3"))
        ),
        Vec::<Change>::new()
    );
    let mut late = result("u2", None, "t2", "Exit code 3\nboom", &json!("Error: Exit code 3"));
    late["message"]["content"][0]["is_error"] = json!(true);
    let mut c = Conversation::default();
    c.ingest(&MAIN, &late);
    c.ingest(&MAIN, &call("a2", None, "t2", "Bash", &json!({"command": "false"})));
    let failed = tool(&c, &MAIN, "t2");
    let ToolDetail::Bash(bash) = &failed.detail else { panic!("bash") };
    assert_eq!((bash.status, bash.exit_code), (ShellStatus::Failed, Some(3)));
    assert_eq!(failed.result.as_ref().map(|r| r.status), Some(ResultStatus::Error));
    assert_eq!(
        failed.result.as_ref().and_then(|r| r.text.as_ref()).map(|t| t.text.as_str()),
        Some("Exit code 3\nboom"),
        "an error keeps the text the model saw"
    );
}

/// A background command stays running after its result, and the task notification that
/// comes turns later finishes it, with the exit code from its summary.
#[test]
fn a_background_command_finishes_on_its_notice() {
    let mut c = Conversation::default();
    let input = json!({"command": "sleep 1; false", "run_in_background": true});
    c.ingest(&MAIN, &call("a1", None, "t1", "Bash", &input));
    let structured = json!({"stdout": "", "stderr": "", "backgroundTaskId": "b1"});
    c.ingest(&MAIN, &result("u1", Some("a1"), "t1", "Command running in background", &structured));
    let ToolDetail::Bash(bash) = &tool(&c, &MAIN, "t1").detail else { panic!("bash") };
    assert_eq!((bash.status, bash.task_id.as_deref()), (ShellStatus::Running, Some("b1")));

    let prompt = "<task-notification>\n<task-id>b1</task-id>\n<tool-use-id>t1</tool-use-id>\n\
                  <output-file>/tmp/b1.output</output-file>\n<status>completed</status>\n\
                  <summary>Background command \"x\" completed (exit code 1)</summary>\n</task-notification>";
    let notice = json!({
        "type": "attachment", "uuid": "n1", "parentUuid": "u1",
        "attachment": { "type": "queued_command", "commandMode": "task-notification", "prompt": prompt },
    });
    let changes = c.ingest(&MAIN, &notice);
    assert!(
        matches!(changes.as_slice(), [Change::Upsert { entry, .. }] if entry.id == "t1"),
        "{changes:?}"
    );
    let ToolDetail::Bash(bash) = &tool(&c, &MAIN, "t1").detail else { panic!("bash") };
    assert_eq!((bash.status, bash.exit_code), (ShellStatus::Failed, Some(1)));
    assert_eq!(bash.output_file.as_deref(), Some("/tmp/b1.output"));

    // The same notice as a user record (what an interrupt that backgrounds a command writes).
    let killed = prompt.replace("completed</status>", "killed</status>");
    c.ingest(&MAIN, &user("n2", Some("n1"), &json!(killed)));
    let ToolDetail::Bash(bash) = &tool(&c, &MAIN, "t1").detail else { panic!("bash") };
    assert_eq!(bash.status, ShellStatus::Killed);
    assert_eq!(c.entries(&MAIN).len(), 1, "a notice is not a prompt");
}

/// Sidechain records go to their agent's thread whether they come from the main file or the
/// agent's own; the thread knows the call that started it.
#[test]
fn a_subagent_talks_in_its_own_thread() {
    let mut c = Conversation::default();
    c.ingest(
        &MAIN,
        &call(
            "a1",
            None,
            "t1",
            "Agent",
            &json!({"description": "Count", "prompt": "Count lines", "subagent_type": "Explore"}),
        ),
    );
    let mut side = assistant("s1", None, &json!([{"type": "text", "text": "4"}]));
    side["isSidechain"] = json!(true);
    side["agentId"] = json!("ag1");
    c.ingest(&MAIN, &side);
    let agent = ThreadId::Agent("ag1".to_owned());
    assert_eq!(c.entries(&agent).len(), 1);
    assert_eq!(c.entries(&MAIN).len(), 1, "the main thread holds only the call");

    let file = Path::new("/p/session/subagents/agent-ag2.jsonl");
    let hinted = thread_of(file);
    assert_eq!(hinted, ThreadId::Agent("ag2".to_owned()));
    let meta = json!({"type": "agent_metadata", "toolUseId": "t2", "agentType": "Plan", "description": "Plan it"});
    c.ingest(&hinted, &meta);
    c.ingest(&hinted, &user("s2", None, &json!("Make a plan")));
    let state = c.snapshot();
    let ag2 = state.iter().find(|t| t.id == hinted).expect("thread");
    assert_eq!(ag2.origin.as_ref().and_then(|o| o.tool_use_id.as_deref()), Some("t2"));
    assert!(matches!(&ag2.entries[0].body, Body::Prompt(p) if p.text.text == "Make a plan"));

    let structured = json!({"status": "completed", "agentId": "ag1", "agentType": "Explore",
        "content": [{"type": "text", "text": "4"}], "totalTokens": 900, "totalToolUseCount": 1, "totalDurationMs": 1200});
    c.ingest(&MAIN, &result("u1", Some("a1"), "t1", "[Subagent hand-back] 4", &structured));
    let ToolDetail::Agent(detail) = &tool(&c, &MAIN, "t1").detail else { panic!("agent") };
    assert_eq!(detail.agent_id.as_deref(), Some("ag1"));
    assert_eq!(detail.status, AgentRun::Completed);
    assert_eq!(
        detail.report.as_ref().map(|r| r.text.as_str()),
        Some("4"),
        "the report, not the hand-back wrapper"
    );
    assert_eq!(
        (detail.tokens, detail.tool_uses, detail.duration_ms),
        (Some(900), Some(1), Some(1200))
    );
    assert_eq!(thread_of(Path::new("/p/abc.jsonl")), MAIN);
    assert_eq!(thread_of(Path::new("/p/agent-x.jsonl")), MAIN, "only under subagents/");
}

/// Prose keeps its head, a log its tail; both say how long the whole was, and the reference
/// they carry finds the whole again in the transcript.
#[test]
fn a_long_text_is_clipped_and_can_be_had_whole() {
    let long = (1..=1000).fold(String::new(), |mut text, n| {
        let _written = writeln!(text, "line {n}");
        text
    });
    let prompt = user("p1", None, &json!(long));
    let structured = json!({"stdout": long, "stderr": ""});
    let jsonl = [
        line(&prompt),
        line(&call("a1", Some("p1"), "t1", "Bash", &json!({"command": "seq"}))),
        line(&result("u1", Some("a1"), "t1", "…", &structured)),
    ]
    .concat();
    let mut c = Conversation::default();
    c.ingest_jsonl(&MAIN, &jsonl);
    let Body::Prompt(p) = &c.entries(&MAIN)[0].body else { panic!("prompt") };
    assert_eq!((p.text.lines, p.text.text.lines().count()), (1000, PROSE.lines));
    assert!(p.text.text.starts_with("line 1\n"));
    let whole = full_text(&jsonl, p.text.full.as_ref().expect("clipped")).expect("found");
    assert_eq!(whole, long);

    let ToolDetail::Bash(bash) = &tool(&c, &MAIN, "t1").detail else { panic!("bash") };
    let stdout = bash.stdout.as_ref().expect("stdout");
    assert!(stdout.is_clipped());
    assert!(stdout.text.ends_with("line 1000"), "a log keeps its end: {:?}", stdout.text);
    assert_eq!(stdout.text.lines().count(), OUTPUT.lines);
    assert_eq!(
        full_text(&jsonl, stdout.full.as_ref().expect("ref")).as_deref(),
        Some(long.as_str())
    );

    // A single line longer than the cap is cut inside it, at a character.
    let wide = "ở".repeat(OUTPUT.chars * 2);
    let head = Clipped::head(&wide, OUTPUT, None);
    assert_eq!(head.text.chars().count(), OUTPUT.chars + 1, "the cut and its ellipsis");
    let tail = Clipped::tail(&wide, OUTPUT, None);
    assert_eq!(tail.text.chars().count(), OUTPUT.chars + 1);
    assert_eq!(Clipped::head("short", OUTPUT, None).text, "short");
}

/// A diff longer than the cap keeps its first lines and still counts every change.
#[test]
fn a_long_diff_keeps_its_counts() {
    let lines: Vec<String> = (0..1000).map(|n| format!("+added {n}")).collect();
    let structured = json!({"type": "update", "structuredPatch": [
        {"oldStart": 1, "oldLines": 1, "newStart": 1, "newLines": 1000, "lines": lines},
        {"oldStart": 2000, "oldLines": 1, "newStart": 3000, "newLines": 0, "lines": ["-gone"]},
    ]});
    let jsonl = [
        line(&call(
            "a1",
            None,
            "t1",
            "Write",
            &json!({"file_path": "/w/big.txt", "content": "x\ny"}),
        )),
        line(&result("u1", Some("a1"), "t1", "updated", &structured)),
    ]
    .concat();
    let mut c = Conversation::default();
    c.ingest_jsonl(&MAIN, &jsonl);
    let ToolDetail::Write(write) = &tool(&c, &MAIN, "t1").detail else { panic!("write") };
    assert_eq!((write.kind, write.lines), (WriteKind::Overwrite, 2));
    let patch = &write.patch;
    assert_eq!((patch.added, patch.removed), (1000, 1));
    assert_eq!(patch.hunks.len(), 1, "the second hunk is past the cap");
    assert_eq!(patch.clipped_lines, 601);
    let whole = full_text(&jsonl, patch.full.as_ref().expect("ref")).expect("patch");
    assert!(whole.starts_with("@@ -1,1 +1,1000 @@\n+added 0\n"));
    assert!(whole.ends_with("@@ -2000,1 +3000,0 @@\n-gone\n"));
}

/// A file made whole comes back with an empty diff, and counts every line it wrote as added,
/// so the turn says what it changed.
#[test]
fn a_created_file_counts_its_lines_as_added() {
    let created = json!({"type": "create", "filePath": "/w/notes.md", "content": "# Notes\nhello",
        "originalFile": null, "structuredPatch": []});
    let jsonl = [
        line(&call(
            "a1",
            None,
            "t1",
            "Write",
            &json!({"file_path": "/w/notes.md", "content": "# Notes\nhello"}),
        )),
        line(&result("u1", Some("a1"), "t1", "created", &created)),
    ]
    .concat();
    let mut c = Conversation::default();
    c.ingest_jsonl(&MAIN, &jsonl);
    let ToolDetail::Write(write) = &tool(&c, &MAIN, "t1").detail else { panic!("write") };
    assert_eq!((write.kind, write.lines), (WriteKind::Create, 2));
    assert_eq!((write.patch.added, write.patch.removed), (2, 0));
    assert!(write.patch.hunks.is_empty(), "no diff to show");
}

/// An edit's hunks are headed from the file as it was, as git heads them, in the clipped hunks
/// and in the whole patch alike.
#[test]
fn an_edits_hunks_name_what_they_are_in() {
    let original = "use std::io;\n\nimpl Client {\n    fn send(&self) {\n        a();\n        b();\n        c();\n    }\n}\n";
    let structured = json!({"filePath": "/w/client.rs", "originalFile": original, "structuredPatch": [
        {"oldStart": 1, "oldLines": 1, "newStart": 1, "newLines": 1, "lines": ["-use std::io;", "+use std::fmt;"]},
        {"oldStart": 5, "oldLines": 3, "newStart": 5, "newLines": 3, "lines": ["         a();", "-        b();", "+        d();", "         c();"]},
    ]});
    let jsonl = [
        line(&call(
            "a1",
            None,
            "t1",
            "Edit",
            &json!({"file_path": "/w/client.rs", "old_string": "b();", "new_string": "d();"}),
        )),
        line(&result("u1", Some("a1"), "t1", "updated", &structured)),
    ]
    .concat();
    let mut c = Conversation::default();
    c.ingest_jsonl(&MAIN, &jsonl);
    let ToolDetail::Edit(edit) = &tool(&c, &MAIN, "t1").detail else { panic!("edit") };
    let headings: Vec<Option<&str>> =
        edit.patch.hunks.iter().map(|h| h.heading.as_deref()).collect();
    assert_eq!(headings, [None, Some("impl Client {")], "{:#?}", edit.patch);
    let whole =
        full_text(&jsonl, &TextRef { record: "u1".to_owned(), part: Part::Patch }).expect("patch");
    assert!(whole.contains("@@ -1,1 +1,1 @@\n-use"), "{whole}");
    assert!(whole.contains("@@ -5,3 +5,3 @@ impl Client {\n"), "{whole}");
}

/// Records of kinds the decoder does not know, fields it has never seen, lines that are not
/// JSON and tools it has no detail for are all passed over or kept generically.
#[test]
fn what_it_does_not_know_is_passed_over() {
    let jsonl = [
        "not json at all\n".to_owned(),
        line(&json!({"type": "file-history-snapshot", "snapshot": {}})),
        line(&json!({"type": "brand-new-kind", "uuid": "x1", "whatever": [1, 2]})),
        line(&json!({"type": "user", "uuid": "u0", "message": {"role": "user", "content": 42}})),
        line(&json!({"type": "summary", "summary": "old"})),
        line(&call("a1", None, "t1", "FutureTool", &json!({"alpha": 1, "beta": "two"}))),
        line(&call("a2", Some("a1"), "t2", "mcp__github__create_issue", &json!({"title": "t"}))),
        line(&assistant("a3", Some("a2"), &json!([{"type": "hologram", "data": 1}, {"type": "text", "text": "hi", "extra": true}]))),
        "{\"type\":\"user\",\"uuid\":\"tor".to_owned(),
    ]
    .concat();
    let mut c = Conversation::default();
    c.ingest_jsonl(&MAIN, &jsonl);
    let entries = c.entries(&MAIN);
    assert_eq!(entries.len(), 3, "{entries:#?}");
    let other = tool(&c, &MAIN, "t1");
    assert!(
        matches!(&other.detail, ToolDetail::Other { input } if input.text == r#"{"alpha":1,"beta":"two"}"#)
    );
    let mcp = tool(&c, &MAIN, "t2");
    assert!(
        matches!(&mcp.detail, ToolDetail::Mcp(m) if m.server == "github" && m.tool == "create_issue")
    );
    assert_eq!(entries[2].id, "a3:1", "the block's own index");
}

/// A prompt whose parent is an earlier record (a rewind) abandons what came after it.
#[test]
fn a_branch_abandons_what_followed_its_fork() {
    let mut c = Conversation::default();
    c.ingest(&MAIN, &user("p1", None, &json!("first")));
    c.ingest(&MAIN, &assistant("a1", Some("p1"), &json!([{"type": "text", "text": "one"}])));
    c.ingest(&MAIN, &user("p2", Some("a1"), &json!("second")));
    c.ingest(&MAIN, &call("a2", Some("p2"), "t1", "Read", &json!({"file_path": "/f"})));
    let changes = c.ingest(&MAIN, &user("p3", Some("a1"), &json!("second, reworded")));
    let removed: Vec<&str> = changes
        .iter()
        .filter_map(|ch| match ch {
            Change::Remove { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(removed, ["t1", "p2"]);
    let ids: Vec<&str> = c.entries(&MAIN).iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["p1", "a1:0", "p3:rewound", "p3"], "the new branch opens with its marker");
    assert!(matches!(c.entries(&MAIN)[2].body, Body::Rewound { dropped: 2 }));
    let turns: Vec<&str> = c.turns(&MAIN).iter().map(|t| t.prompt.as_str()).collect();
    assert_eq!(turns, ["p1", "p3"], "the abandoned prompt's turn goes with it");
    // A result for the abandoned call finds nothing to update.
    assert_eq!(
        c.ingest(&MAIN, &result("u9", Some("a2"), "t1", "x", &json!({}))),
        Vec::<Change>::new()
    );
    // A record read twice (a tail that went back) changes nothing.
    assert_eq!(
        c.ingest(&MAIN, &user("p3", Some("a1"), &json!("second, reworded"))),
        Vec::<Change>::new()
    );
}

/// Esc, slash commands and their output, bash mode, compaction with its summary.
#[test]
fn the_turns_markers_are_entries() {
    let jsonl = [
        line(&user("p1", None, &json!([{"type": "text", "text": "[Request interrupted by user for tool use]"}]))),
        line(&user("p2", Some("p1"), &json!("[Request interrupted by user]"))),
        line(&user("p3", Some("p2"), &json!("<command-name>/model</command-name>\n<command-message>model</command-message>\n<command-args>opus</command-args>"))),
        line(&user("p4", Some("p3"), &json!("<local-command-stdout>Set model to \u{1b}[1mOpus\u{1b}[22m</local-command-stdout>"))),
        line(&user("p5", Some("p4"), &json!("<local-command-stdout></local-command-stdout>"))),
        line(&user("p6", Some("p5"), &json!("<bash-input>ls</bash-input>"))),
        line(&json!({"type": "system", "subtype": "compact_boundary", "uuid": "c1", "parentUuid": null,
            "logicalParentUuid": "p6", "compactMetadata": {"trigger": "auto", "preTokens": 150_000}})),
        line(&json!({"type": "user", "uuid": "c2", "parentUuid": "c1", "isCompactSummary": true,
            "message": {"role": "user", "content": "Summary: things happened"}})),
        line(&json!({"type": "user", "uuid": "c3", "parentUuid": "c2", "isMeta": true,
            "message": {"role": "user", "content": "<local-command-caveat>x</local-command-caveat>"}})),
        line(&json!({"type": "system", "subtype": "api_error", "uuid": "e1", "parentUuid": "c3",
            "error": {"message": "overloaded"}})),
        line(&json!({"type": "system", "subtype": "stop_hook_summary", "uuid": "e2", "parentUuid": "e1"})),
    ]
    .concat();
    let mut c = Conversation::default();
    c.ingest_jsonl(&MAIN, &jsonl);
    let bodies: Vec<&Body> = c.entries(&MAIN).iter().map(|e| &e.body).collect();
    assert!(matches!(bodies[0], Body::Interrupted { during_tool: true }), "{bodies:#?}");
    assert!(matches!(bodies[1], Body::Interrupted { during_tool: false }));
    assert!(
        matches!(bodies[2], Body::Prompt(p) if p.command.as_deref() == Some("/model") && p.text.text == "opus")
    );
    assert!(
        matches!(bodies[3], Body::Note(n) if n.kind == NoteKind::Command && n.text.text == "Set model to Opus")
    );
    assert!(
        matches!(bodies[4], Body::Prompt(p) if p.command.as_deref() == Some("!") && p.text.text == "ls")
    );
    let Body::Compact(compact) = bodies[5] else { panic!("compact") };
    assert_eq!((compact.trigger.as_deref(), compact.pre_tokens), (Some("auto"), Some(150_000)));
    assert_eq!(compact.summary.as_ref().map(|s| s.text.as_str()), Some("Summary: things happened"));
    assert!(
        matches!(bodies[6], Body::Note(n) if n.kind == NoteKind::ApiError && n.text.text == "overloaded")
    );
    assert_eq!(bodies.len(), 7);
}

/// The task tools keep a list; `TodoWrite` replaces it whole.
#[test]
fn the_task_tools_keep_the_list() {
    let mut c = Conversation::default();
    c.ingest(&MAIN, &call("a1", None, "t1", "TaskCreate", &json!({"subject": "Survey"})));
    let changes = c.ingest(
        &MAIN,
        &result(
            "u1",
            Some("a1"),
            "t1",
            "Task #1 created",
            &json!({"task": {"id": "1", "subject": "Survey"}}),
        ),
    );
    assert!(
        changes.iter().any(|ch| matches!(ch, Change::Tasks { tasks, .. } if tasks.len() == 1)),
        "{changes:?}"
    );
    c.ingest(
        &MAIN,
        &call(
            "a2",
            Some("u1"),
            "t2",
            "TaskUpdate",
            &json!({"taskId": "1", "status": "in_progress"}),
        ),
    );
    c.ingest(&MAIN, &result("u2", Some("a2"), "t2", "Updated", &json!({"success": true, "taskId": "1", "updatedFields": ["status"], "statusChange": {"from": "pending", "to": "in_progress"}})));
    assert_eq!(
        c.tasks(&MAIN),
        [Task { id: "1".into(), subject: "Survey".into(), status: "in_progress".into() }]
    );
    let ToolDetail::TaskUpdate(update) = &tool(&c, &MAIN, "t2").detail else { panic!("update") };
    assert_eq!(
        (update.from.as_deref(), update.to.as_deref()),
        (Some("pending"), Some("in_progress"))
    );
    // A failed update changes nothing.
    c.ingest(
        &MAIN,
        &call("a3", Some("u2"), "t3", "TaskUpdate", &json!({"taskId": "1", "status": "deleted"})),
    );
    let mut failed = result("u3", Some("a3"), "t3", "no", &json!({}));
    failed["message"]["content"][0]["is_error"] = json!(true);
    c.ingest(&MAIN, &failed);
    assert_eq!(c.tasks(&MAIN).len(), 1);

    let todos = json!({"todos": [{"content": "a", "status": "completed"}, {"content": "b", "status": "pending"}]});
    c.ingest(&MAIN, &call("a4", Some("u3"), "t4", "TodoWrite", &todos));
    c.ingest(&MAIN, &result("u4", Some("a4"), "t4", "ok", &json!({"newTodos": []})));
    let subjects: Vec<&str> = c.tasks(&MAIN).iter().map(|t| t.subject.as_str()).collect();
    assert_eq!(subjects, ["a", "b"]);
}

/// Questions, plans and the web tools have details of their own.
#[test]
fn questions_plans_and_the_web_are_typed() {
    let mut c = Conversation::default();
    let ask = json!({"questions": [{"question": "Which?", "header": "Pick", "multiSelect": false,
        "options": [{"label": "A", "description": "a"}, {"label": "B", "description": "b"}]}]});
    c.ingest(&MAIN, &call("a1", None, "t1", "AskUserQuestion", &ask));
    c.ingest(
        &MAIN,
        &result("u1", Some("a1"), "t1", "answered", &json!({"answers": {"Which?": "B"}})),
    );
    let ToolDetail::Question(q) = &tool(&c, &MAIN, "t1").detail else { panic!("question") };
    let labels: Vec<&str> = q.questions[0].options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(labels, ["A", "B"]);
    assert_eq!(q.questions[0].options[1].description.as_deref(), Some("b"));
    assert_eq!(q.answers, [Answer { question: "Which?".into(), answer: "B".into() }]);

    c.ingest(&MAIN, &call("a2", Some("u1"), "t2", "ExitPlanMode", &json!({"plan": "1. Do it"})));
    assert!(
        matches!(&tool(&c, &MAIN, "t2").detail, ToolDetail::Plan { plan } if plan.text == "1. Do it")
    );

    c.ingest(
        &MAIN,
        &call(
            "a3",
            Some("a2"),
            "t3",
            "WebFetch",
            &json!({"url": "https://x.dev", "prompt": "sum"}),
        ),
    );
    c.ingest(&MAIN, &result("u3", Some("a3"), "t3", "page", &json!({"code": 200, "bytes": 5120})));
    assert!(
        matches!(&tool(&c, &MAIN, "t3").detail, ToolDetail::WebFetch(f) if f.code == Some(200) && f.bytes == Some(5120))
    );
    c.ingest(&MAIN, &call("a4", Some("u3"), "t4", "WebSearch", &json!({"query": "rust"})));
    let found =
        json!({"results": [{"tool_use_id": "s", "content": [{"url": "a"}, {"url": "b"}]}, "text"]});
    c.ingest(&MAIN, &result("u4", Some("a4"), "t4", "links", &found));
    let ToolDetail::WebSearch(search) = &tool(&c, &MAIN, "t4").detail else { panic!("search") };
    assert_eq!(search.results, Some(2));
    let links: Vec<(&str, &str)> =
        search.links.iter().map(|l| (l.title.as_str(), l.url.as_str())).collect();
    assert_eq!(links, [("a", "a"), ("b", "b")], "an untitled page is named by its address");
}

/// A truncated main file is a new conversation; a subagent's only resets its thread.
#[test]
fn a_file_that_shrank_starts_over() {
    let dir = tempfile::tempdir().expect("tempdir");
    let main = dir.path().join("s.jsonl");
    let agent = dir.path().join("s").join("subagents").join("agent-q.jsonl");
    std::fs::create_dir_all(agent.parent().expect("dir")).expect("mkdir");
    std::fs::write(&main, line(&user("p1", None, &json!("hello there")))).expect("write");
    let mut side = user("s1", None, &json!("brief"));
    side["isSidechain"] = json!(true);
    side["agentId"] = json!("q");
    std::fs::write(&agent, line(&side)).expect("write");
    let (mut c, mut main_tail, mut agent_tail) =
        (Conversation::default(), Tail::default(), Tail::default());
    // The prompt and the turn it opens.
    assert_eq!(c.read(&mut main_tail, &main).expect("read").len(), 2);
    assert_eq!(c.read(&mut agent_tail, &agent).expect("read").len(), 2);
    assert!(c.read(&mut main_tail, &main).expect("read").is_empty(), "nothing new");

    std::fs::write(&agent, "").expect("truncate");
    let changes = c.read(&mut agent_tail, &agent).expect("read");
    assert_eq!(changes, [Change::Reset { thread: Some(ThreadId::Agent("q".into())) }]);
    assert_eq!(c.entries(&MAIN).len(), 1, "the main thread stays");
    std::fs::write(&main, "").expect("truncate");
    let changes = c.read(&mut main_tail, &main).expect("read");
    assert_eq!(changes, [Change::Reset { thread: None }]);
    assert_eq!(c.threads().count(), 0);
}

#[test]
fn stamps_are_read_as_utc_milliseconds() {
    assert_eq!(parse_ms("1970-01-01T00:00:00Z"), Some(WallMs::ZERO));
    assert_eq!(parse_ms("2026-09-27T03:15:25.849Z"), Some(WallMs::from_millis(1_790_478_925_849)));
    assert_eq!(parse_ms("2024-02-29T23:59:59.5Z"), Some(WallMs::from_millis(1_709_251_199_500)));
    assert_eq!(parse_ms("2026-13-01T00:00:00Z"), None);
    assert_eq!(parse_ms("2026-09-27T03:15:25+02:00"), None, "only UTC is written");
    assert_eq!(parse_ms("yesterday"), None);
}

/// Parallel calls: each result names its own call's record as its parent, not the newest
/// record, and abandons nothing (Claude Code 2.1.283 writes them so).
#[test]
fn the_results_of_parallel_calls_branch_nothing() {
    let mut c = Conversation::default();
    c.ingest(&MAIN, &call("a1", None, "t1", "Read", &json!({"file_path": "/a"})));
    c.ingest(&MAIN, &call("a2", Some("a1"), "t2", "Read", &json!({"file_path": "/b"})));
    let first = c.ingest(&MAIN, &result("u1", Some("a1"), "t1", "a", &json!({})));
    let second = c.ingest(&MAIN, &result("u2", Some("a2"), "t2", "b", &json!({})));
    assert!(
        first.iter().chain(&second).all(|ch| matches!(ch, Change::Upsert { .. })),
        "{first:?} {second:?}"
    );
    assert!(
        c.entries(&MAIN).iter().all(|e| matches!(&e.body, Body::Tool(t) if t.result.is_some()))
    );
    assert_eq!(c.entries(&MAIN).len(), 2);
}

/// An assistant record with a model and usage, as Claude Code writes one per content block of a
/// request (every block of the request repeats the request's usage).
fn answered(uuid: &str, message: &str, blocks: &Value, usage: &Value, stop: Option<&str>) -> Value {
    let mut record = assistant(uuid, None, blocks);
    record["message"]["id"] = json!(message);
    record["message"]["model"] = json!("claude-opus-5-5");
    record["message"]["usage"] = usage.clone();
    record["message"]["stop_reason"] = json!(stop);
    record
}

/// A turn counts each request once, however many records it wrote, and adds up its tokens; it
/// knows its model, the context its last request carried, why the model stopped, the mode the
/// prompt was sent in, and when Claude Code closed it.
#[test]
fn a_turn_adds_up_its_requests() {
    let mut c = Conversation::default();
    let mut prompt = user("p1", None, &json!("go"));
    prompt["permissionMode"] = json!("plan");
    c.ingest(&MAIN, &prompt);
    let first = json!({
        "input_tokens": 10, "cache_read_input_tokens": 1_000, "cache_creation_input_tokens": 200,
        "output_tokens": 50, "output_tokens_details": {"thinking_tokens": 20},
    });
    c.ingest(
        &MAIN,
        &answered("a1", "m1", &json!([{"type": "thinking", "thinking": "hm"}]), &first, None),
    );
    let tool_use =
        json!([{"type": "tool_use", "id": "t1", "name": "Read", "input": {"file_path": "/f"}}]);
    c.ingest(&MAIN, &answered("a2", "m1", &tool_use, &first, Some("tool_use")));
    c.ingest(&MAIN, &result("u1", Some("a2"), "t1", "x", &json!({})));
    let second = json!({
        "input_tokens": 5, "cache_read_input_tokens": 1_300, "cache_creation_input_tokens": 0,
        "output_tokens": 30,
    });
    let changes = c.ingest(
        &MAIN,
        &answered(
            "a3",
            "m1",
            &json!([{"type": "text", "text": "done"}]),
            &second,
            Some("end_turn"),
        ),
    );
    assert!(
        changes.iter().any(|ch| matches!(ch, Change::Turn { turn, .. } if turn.prompt == "p1"))
    );
    let [turn] = c.turns(&MAIN) else { panic!("one turn: {:?}", c.turns(&MAIN)) };
    assert_eq!(turn.requests, 2, "a user record between them makes two requests of one id");
    assert_eq!(
        turn.usage,
        Usage { input: 15, cache_read: 2_300, cache_write: 200, output: 80, thinking: 20 }
    );
    assert_eq!(turn.context_tokens, Some(1_305));
    assert_eq!(turn.models, ["claude-opus-5-5"]);
    assert_eq!((turn.stop.as_deref(), turn.mode.as_deref()), (Some("end_turn"), Some("plan")));
    assert_eq!(turn.ended_ms, None);
    let mut end = json!({
        "type": "system", "subtype": "stop_hook_summary", "uuid": "s1",
        "timestamp": "2026-09-27T03:15:40.000Z", "hookErrors": [], "preventedContinuation": false,
    });
    c.ingest(&MAIN, &end);
    assert_eq!(c.turns(&MAIN)[0].ended_ms, parse_ms("2026-09-27T03:15:40.000Z"));
    assert_eq!(c.entries(&MAIN).len(), 4, "a clean stop says nothing");
    end["uuid"] = json!("s2");
    end["hookErrors"] = json!(["lint failed: 2 warnings"]);
    c.ingest(&MAIN, &end);
    let Some(Entry { body: Body::Note(note), .. }) = c.entries(&MAIN).last() else { panic!() };
    assert_eq!((note.kind, note.text.text.as_str()), (NoteKind::Hook, "lint failed: 2 warnings"));
}

/// An API error Claude Code retries says which attempt comes next and when.
#[test]
fn an_api_error_says_when_it_retries() {
    let mut c = Conversation::default();
    c.ingest(
        &MAIN,
        &json!({
            "type": "system", "subtype": "api_error", "uuid": "e1",
            "timestamp": "2026-09-27T03:15:40.000Z",
            "error": {"error": {"type": "overloaded_error", "message": "Overloaded"}},
            "retryInMs": 1_084.6, "retryAttempt": 2, "maxRetries": 10,
        }),
    );
    let [Entry { body: Body::Note(note), .. }] = c.entries(&MAIN) else { panic!() };
    assert_eq!(note.text.text, "Overloaded");
    assert_eq!(note.retry, Some(Retry { attempt: 2, max: 10, in_ms: 1_085 }));
}

/// A background command's result says where it writes, and the queued notice Claude Code
/// writes the moment it ends finishes it, stamped, before the model has taken the notice in;
/// the attachment that follows changes nothing more.
#[test]
fn a_background_command_is_finished_by_its_queued_notice() {
    let mut c = Conversation::default();
    let input = json!({"command": "npm run build", "run_in_background": true});
    c.ingest(&MAIN, &call("a1", None, "t1", "Bash", &input));
    let text = "Command running in background with ID: b1. Output is being written to: \
                /private/tmp/claude-501/-w/s1/tasks/b1.output. You will be notified when it \
                completes.";
    let structured = json!({"stdout": "", "stderr": "", "backgroundTaskId": "b1"});
    c.ingest(&MAIN, &result("u1", Some("a1"), "t1", text, &structured));
    let ToolDetail::Bash(bash) = &tool(&c, &MAIN, "t1").detail else { panic!("bash") };
    assert_eq!(bash.output_file.as_deref(), Some("/private/tmp/claude-501/-w/s1/tasks/b1.output"));
    assert_eq!(c.background().map(|(_, e)| e.id.as_str()).collect::<Vec<_>>(), ["t1"]);

    let notice = "<task-notification>\n<task-id>b1</task-id>\n<tool-use-id>t1</tool-use-id>\n\
                  <status>completed</status>\n<summary>Background command \"build\" completed \
                  (exit code 0)</summary>\n</task-notification>";
    let queued = json!({
        "type": "queue-operation", "operation": "enqueue", "content": notice,
        "timestamp": "2026-09-27T03:16:25.000Z",
    });
    let changes = c.ingest(&MAIN, &queued);
    assert!(matches!(changes.as_slice(), [Change::Upsert { entry, .. }] if entry.id == "t1"));
    let ToolDetail::Bash(bash) = &tool(&c, &MAIN, "t1").detail else { panic!("bash") };
    assert_eq!(bash.status, ShellStatus::Done);
    assert_eq!(bash.finished_ms, parse_ms("2026-09-27T03:16:25.000Z"));
    let attached = json!({
        "type": "attachment", "uuid": "n1", "parentUuid": "u1",
        "timestamp": "2026-09-27T03:16:30.000Z",
        "attachment": { "commandMode": "task-notification", "prompt": notice },
    });
    assert!(c.ingest(&MAIN, &attached).is_empty(), "the same notice again changes nothing");
}

/// A background subagent's notice brings its report and its figures from `<usage>`.
#[test]
fn a_background_subagent_reports_its_figures() {
    let mut c = Conversation::default();
    let input = json!({"description": "Survey", "prompt": "look", "run_in_background": true});
    c.ingest(&MAIN, &call("a1", None, "t1", "Agent", &input));
    let launched = json!({"isAsync": true, "status": "async_launched", "agentId": "x1"});
    c.ingest(&MAIN, &result("u1", Some("a1"), "t1", "launched", &launched));
    let notice = "<task-notification>\n<task-id>x1</task-id>\n<tool-use-id>t1</tool-use-id>\n\
                  <status>completed</status>\n<result>Found 3 places.</result>\n\
                  <usage>total_tokens: 1200\ntool_uses: 4\nduration_ms: 3000</usage>\n\
                  </task-notification>";
    c.ingest(&MAIN, &user("n1", Some("u1"), &json!(notice)));
    let ToolDetail::Agent(agent) = &tool(&c, &MAIN, "t1").detail else { panic!("agent") };
    assert_eq!(agent.status, AgentRun::Completed);
    assert_eq!(agent.report.as_ref().map(|r| r.text.as_str()), Some("Found 3 places."));
    assert_eq!(
        (agent.tokens, agent.tool_uses, agent.duration_ms),
        (Some(1200), Some(4), Some(3000))
    );
}

fn picture() -> (Vec<u8>, Value) {
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    bytes.extend(64_u32.to_be_bytes());
    bytes.extend(48_u32.to_be_bytes());
    let data = data_encoding::BASE64.encode(&bytes);
    let block = json!({
        "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": data },
    });
    (bytes, block)
}

/// A picture pasted with words is on its prompt, described and never carried; one pasted
/// alone is a prompt of its own.
#[test]
fn a_pasted_picture_is_on_its_prompt() {
    let (bytes, block) = picture();
    let mut c = Conversation::default();
    c.ingest(&MAIN, &user("p1", None, &json!([block, { "type": "text", "text": "what is this" }])));
    c.ingest(&MAIN, &user("p2", Some("p1"), &json!([block])));
    let prompts: Vec<&Prompt> = c
        .entries(&MAIN)
        .iter()
        .filter_map(|e| match &e.body {
            Body::Prompt(p) => Some(p),
            _ => None,
        })
        .collect();
    let [with_words, alone] = prompts.as_slice() else { panic!("{prompts:?}") };
    assert_eq!(with_words.text.text, "what is this");
    let [image] = with_words.images.as_slice() else { panic!() };
    assert_eq!((image.width, image.height, image.media_type.as_str()), (64, 48, "image/png"));
    assert_eq!(image.digest, blake3::hash(&bytes).to_hex().to_string());
    assert_eq!(
        image.at,
        TextRef { record: "p1".to_owned(), part: Part::Image { tool_use_id: None, index: 0 } }
    );
    assert_eq!(alone.text.text, "");
    assert_eq!(alone.images.len(), 1);
    assert_eq!(c.turns(&MAIN).len(), 2, "each opens a turn");
    let jsonl =
        line(&user("p1", None, &json!([block, { "type": "text", "text": "what is this" }])));
    assert_eq!(image_bytes(&jsonl, &image.at), Some(bytes));
}

/// A screenshot a tool returned is on its result, with no empty text beside it.
#[test]
fn a_tools_picture_is_on_its_result() {
    let (_, block) = picture();
    let mut c = Conversation::default();
    c.ingest(&MAIN, &call("a1", None, "t1", "mcp__browser__screenshot", &json!({})));
    let record = user(
        "u1",
        Some("a1"),
        &json!([{ "type": "tool_result", "tool_use_id": "t1", "content": [block] }]),
    );
    c.ingest(&MAIN, &record);
    let result = tool(&c, &MAIN, "t1").result.as_ref().expect("result");
    assert_eq!(result.text, None);
    let [image] = result.images.as_slice() else { panic!("{result:?}") };
    assert_eq!(image.at.part, Part::Image { tool_use_id: Some("t1".to_owned()), index: 0 });
    assert!(image_bytes(&line(&record), &image.at).is_some());
}

/// A notebook's cell edited, as Claude Code 2.1.295 did it (the `notebook` capture): a replaced
/// cell is an edit of the notebook whose patch takes its old source to its new one, an
/// inserted cell's source is all added; and the same call put to the person shows its new
/// source before any result says what it replaced.
#[test]
fn a_notebook_edit_is_an_edit_of_its_cell() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/conversation/notebook/transcript.jsonl");
    let jsonl = std::fs::read_to_string(path).expect("transcript");
    let mut c = Conversation::default();
    c.ingest_jsonl(&MAIN, &jsonl);
    let edits: Vec<&EditDetail> = c
        .entries(&MAIN)
        .iter()
        .filter_map(|e| match &e.body {
            Body::Tool(call) if call.name == "NotebookEdit" => match &call.detail {
                ToolDetail::Edit(edit) => Some(edit),
                other => panic!("an edit: {other:?}"),
            },
            _ => None,
        })
        .collect();
    let [replaced, inserted] = edits.as_slice() else { panic!("two: {edits:?}") };
    assert_eq!(replaced.path, "/work/nb.ipynb");
    let lines = |e: &EditDetail| -> Vec<String> {
        e.patch.hunks.iter().flat_map(|h| h.lines.clone()).collect()
    };
    assert_eq!(lines(replaced), ["-print(1)", "+print(2)"]);
    assert_eq!((replaced.patch.added, replaced.patch.removed), (1, 1));
    assert_eq!(lines(inserted), ["+print(3)"]);

    let asked = proposed(
        "NotebookEdit",
        &json!({"cell_id": "a1", "edit_mode": "replace", "new_source": "print(2)",
            "notebook_path": "/work/nb.ipynb"}),
    );
    let ToolDetail::Edit(asked) = asked else { panic!("an edit") };
    assert_eq!(
        (asked.path.as_str(), lines(&asked)),
        ("/work/nb.ipynb", vec!["+print(2)".to_owned()])
    );
}
