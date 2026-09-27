//! The conversations recorded under `slopty-agent`'s fixtures, decoded by the worker's own
//! decoder into the events a follower is sent, and a held permission prompt to answer.

use std::path::Path;

use slopty_core::SessionId;
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
        }),
        suggestions: vec![Suggestion {
            grant: Grant::Rules {
                behavior: "allow".into(),
                rules: vec!["Bash(npm test:*)".into()],
            },
            destination: Some("localSettings".into()),
        }],
        mode: None,
        asked_ms: 0,
        until_ms: 600_000,
    }
}
