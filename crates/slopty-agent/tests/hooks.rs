//! Hook payloads captured from a real Claude Code (`tests/fixtures/conversation/*/hooks.jsonl`,
//! each line the `input` the hook got and the `output` it printed): what the relay forwards
//! keeps what the conversation face reads, and the permission decisions this crate prints are
//! the ones that Claude Code acted on during the capture (the `permission` scenario checks the
//! files the allowed and refused commands left).

#[cfg(test)]
mod hooks {
    use std::path::Path;

    use serde_json::{Value, json};
    use slopty_agent::permission::Decision;
    use slopty_agent::{HOOK_EVENTS, HOOK_JSON_BUDGET, Hook};

    fn records(scenario: &str) -> Vec<Value> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/conversation")
            .join(scenario)
            .join("hooks.jsonl");
        let text = std::fs::read_to_string(path).expect("fixture");
        text.lines().map(|line| serde_json::from_str(line).expect("json")).collect()
    }

    /// The forwarded form of one captured payload.
    fn forwarded(input: &Value) -> Hook {
        let hook = Hook::parse(&input.to_string()).expect("a hook").trimmed();
        let line = serde_json::to_string(&hook).expect("json");
        assert!(line.len() < 2 * HOOK_JSON_BUDGET, "{} bytes", line.len());
        Hook::parse(&line).expect("reads back")
    }

    fn inputs(scenario: &str, event: &str) -> Vec<Value> {
        records(scenario)
            .into_iter()
            .map(|r| r["input"].clone())
            .filter(|input| input["hook_event_name"] == event)
            .collect()
    }

    #[test]
    fn every_captured_event_is_one_the_relay_registers() {
        for scenario in ["edit", "tools", "interrupt", "compact", "permission"] {
            for record in records(scenario) {
                let event = record["input"]["hook_event_name"].as_str().expect("event");
                assert!(HOOK_EVENTS.contains(&event), "{scenario}: {event}");
            }
        }
    }

    /// A tool's result keeps its response and how long it ran.
    #[test]
    fn a_tool_result_keeps_its_response_and_duration() {
        for input in inputs("tools", "PostToolUse") {
            let hook = forwarded(&input);
            assert!(hook.duration_ms.is_some(), "{input}");
            assert!(hook.tool_response.is_some(), "{input}");
            assert!(hook.tool_use_id.is_some());
        }
        let bash = inputs("tools", "PostToolUse")
            .into_iter()
            .map(|input| forwarded(&input))
            .find(|h| {
                h.tool_input.as_ref().and_then(|i| i["command"].as_str())
                    == Some("printf 'one\\ntwo\\n'")
            })
            .expect("the printf call");
        assert_eq!(
            bash.tool_response.as_ref().map(|r| r["stdout"].clone()),
            Some(json!("one\ntwo"))
        );
        let edit = inputs("edit", "PostToolUse")
            .into_iter()
            .map(|input| forwarded(&input))
            .find(|h| h.tool_name.as_deref() == Some("Edit"))
            .expect("the edit");
        let response = edit.tool_response.expect("response");
        assert!(response.get("structuredPatch").is_some(), "the diff stays");
        assert!(response.get("originalFile").is_none(), "the whole file before it goes");
    }

    /// A subagent's start and stop name it, its type and, on stop, its transcript and last words.
    #[test]
    fn a_subagent_is_named_with_its_transcript() {
        let start = forwarded(inputs("tools", "SubagentStart").first().expect("start"));
        let stop = forwarded(inputs("tools", "SubagentStop").first().expect("stop"));
        assert_eq!(start.agent_id, stop.agent_id);
        assert!(start.agent_id.is_some());
        assert_eq!(stop.agent_type.as_deref(), Some("general-purpose"));
        let transcript = stop.agent_transcript_path.expect("path");
        assert!(
            transcript.ends_with(&format!(
                "/subagents/agent-{}.jsonl",
                stop.agent_id.unwrap_or_default()
            )),
            "{transcript}"
        );
        assert!(stop.last_assistant_message.is_some_and(|m| !m.is_empty()));
    }

    /// The task list's events carry the task.
    #[test]
    fn the_task_events_carry_the_task() {
        let created: Vec<Hook> = inputs("tools", "TaskCreated").iter().map(forwarded).collect();
        let subjects: Vec<&str> =
            created.iter().filter_map(|h| h.task_subject.as_deref()).collect();
        assert_eq!(subjects, ["Survey files", "Write notes"]);
        assert!(created.iter().all(|h| h.task_id.is_some()));
        assert_eq!(inputs("tools", "TaskCompleted").len(), 2);
    }

    /// Compaction says what started it and, after, the summary.
    #[test]
    fn compaction_carries_its_trigger_and_summary() {
        let pre = forwarded(inputs("compact", "PreCompact").first().expect("pre"));
        assert_eq!(pre.trigger.as_deref(), Some("manual"));
        assert!(pre.custom_instructions.is_some());
        let post = forwarded(inputs("compact", "PostCompact").first().expect("post"));
        assert_eq!(post.trigger.as_deref(), Some("manual"));
        assert!(post.compact_summary.is_some_and(|s| !s.is_empty()));
    }

    /// The decisions this crate prints are the ones the capture fed Claude Code for the same
    /// requests, which it acted on: the refused command left no file, the allowed ones did, and
    /// "always" handed back what Claude Code suggested.
    #[test]
    fn a_permission_decision_is_the_output_claude_code_acted_on() {
        let requests: Vec<Value> = records("permission")
            .into_iter()
            .filter(|r| r["input"]["hook_event_name"] == "PermissionRequest")
            .collect();
        assert_eq!(requests.len(), 3, "refused, allowed, always");
        for record in requests {
            let hook = forwarded(&record["input"]);
            assert!(hook.tool_use_id.is_none(), "a permission request names no call");
            let command = hook
                .tool_input
                .as_ref()
                .and_then(|i| i["command"].as_str())
                .unwrap_or_default()
                .to_owned();
            let decision = if command.contains("refused") {
                Decision::Deny { message: "Refused by the fixture.".to_owned(), interrupt: false }
            } else if command.contains("always") {
                let suggested =
                    hook.permission_suggestions.as_ref().and_then(Value::as_array).cloned();
                Decision::AllowAlways { updated_permissions: suggested.expect("suggestions") }
            } else {
                Decision::Allow
            };
            assert_eq!(decision.hook_output().as_ref(), Some(&record["output"]), "{command}");
        }
    }
}
