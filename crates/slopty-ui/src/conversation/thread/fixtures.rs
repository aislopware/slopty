//! Threads for the tests: the conversations recorded under `slopty-agent`'s fixtures, rebuilt by
//! the worker's own Claude Code history into the thread a follower mirrors.

use std::collections::BTreeMap;

use slopty_core::WallMs;
use slopty_proto::thread::{AgentId, Drive, ThreadId, ThreadMeta, ThreadState};

/// A thread with nothing in it.
pub(crate) fn empty() -> ThreadState {
    ThreadState::new(ThreadMeta {
        id: ThreadId::new(),
        agent: AgentId::named(AgentId::CLAUDE_CODE),
        agent_version: String::new(),
        native: "s1".to_owned(),
        cwd: "/w".to_owned(),
        title: "Fix the build".to_owned(),
        parent: None,
        origin: ThreadMeta::PERSON.to_owned(),
        drive: Drive::named(Drive::DRIVEN),
        facts: BTreeMap::new(),
        created_ms: WallMs::ZERO,
    })
}

/// The records of the recorded session `name` under `slopty-agent`'s fixtures, as Claude Code
/// wrote them: its main transcript's, then each subagent's.
fn records(name: &str) -> Vec<serde_json::Value> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../slopty-agent/tests/fixtures/conversation")
        .join(name);
    let mut files = vec![dir.join("transcript.jsonl")];
    if let Ok(agents) = std::fs::read_dir(dir.join("subagents")) {
        let mut agents: Vec<_> =
            agents.map(|e| e.expect("a subagent's transcript").path()).collect();
        agents.sort();
        files.extend(agents);
    }
    files
        .iter()
        .flat_map(|file| {
            let text = std::fs::read_to_string(file).expect("a recorded transcript");
            text.lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str(line).expect("a record"))
                .collect::<Vec<serde_json::Value>>()
        })
        .collect()
}

/// The main thread of the recorded session `name`, as the worker rebuilds it from its record.
pub(crate) fn thread(name: &str) -> ThreadState {
    slopty_agent::claude::history::replay(&records(name))
}
