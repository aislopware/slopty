//! Threads for the tests: the conversations recorded under `slopty-agent`'s fixtures, mapped by
//! the worker's own observed adapter into the thread a follower mirrors.

use std::collections::BTreeMap;

use slopty_core::WallMs;
use slopty_proto::thread::{AgentId, Drive, ThreadId, ThreadMeta, ThreadState};

/// A thread with nothing in it.
pub(crate) fn empty() -> ThreadState {
    ThreadState::new(ThreadMeta {
        modes: Vec::new(),
        efforts: Vec::new(),
        id: ThreadId::new(),
        agent: AgentId::named(AgentId::CLAUDE_CODE),
        agent_version: String::new(),
        native: "s1".to_owned(),
        cwd: "/w".to_owned(),
        title: "Fix the build".to_owned(),
        terminal: None,
        parent: None,
        origin: ThreadMeta::PERSON.to_owned(),
        forked_from: None,
        drive: Drive::named(Drive::OBSERVED),
        caps: Vec::new(),
        models: Vec::new(),
        facts: BTreeMap::new(),
        created_ms: WallMs::ZERO,
    })
}

/// What the worker's transcript decoder reads from the recorded session `name` under
/// `slopty-agent`'s fixtures: its main transcript, then each subagent's.
fn changes(name: &str) -> Vec<slopty_agent::conversation::Change> {
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
    let mut decoder = slopty_agent::conversation::Conversation::default();
    let mut changes = vec![slopty_agent::conversation::Change::Reset { thread: None }];
    for file in files {
        let mut tail = slopty_agent::transcript::Tail::default();
        changes.extend(decoder.read(&mut tail, &file).expect("a recorded transcript"));
    }
    changes
}

/// The main thread of the recorded session `name`, as the worker hosts it.
pub(crate) fn thread(name: &str) -> ThreadState {
    let mut observed =
        slopty_agent::observed::Observed::new("s1", "2.1.0", None, "/w", WallMs::ZERO);
    let main = observed.main();
    let mut outs = observed.drain();
    outs.extend(observed.transcript(&changes(name), &[]));
    let mut state = empty();
    for out in outs {
        match out {
            slopty_agent::observed::Out::Begin(meta) if meta.id == main => {
                state = ThreadState::new(*meta);
            }
            slopty_agent::observed::Out::Actions(id, actions) if id == main => {
                for action in &actions {
                    state.apply(action);
                }
            }
            _ => {}
        }
    }
    state
}
