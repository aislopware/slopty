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

/// A long thread: `turns` settled turns of `calls` commands each, every one with a 2 KiB output,
/// between a message and a 1 KiB answer; the last turn still under way.
pub(crate) fn long(turns: u32, calls: u32) -> ThreadState {
    use slopty_proto::thread::detail::{ExecDetail, ExecStatus};
    use slopty_proto::thread::{
        Changed, Clipped, Item, ItemBody, ItemId, ToolCall, ToolDetail, ToolState, Turn, TurnId,
        TurnState, Usage, UserMessage, kind,
    };
    let mut state = empty();
    let output = "   Compiling slopty-ui v0.1.0 (/w/crates/slopty-ui)\n".repeat(40);
    let answer = "The build passes now. ".repeat(48);
    let at = |turn: u32, step: u32| {
        WallMs::from_millis(u64::from(turn).saturating_mul(60_000).saturating_add(u64::from(step)))
    };
    for t in 1..=turns {
        let last = t == turns;
        state.turns.push(Turn {
            id: TurnId(t),
            input: Some(ItemId(format!("u{t}"))),
            state: if last { TurnState::Active } else { TurnState::Complete },
            started_ms: at(t, 0),
            ended_ms: (!last).then(|| at(t, 55_000)),
            usage: Usage::default(),
            models: Vec::new(),
            changed: Changed::default(),
            before: None,
            after: None,
        });
        let item = |id: String, step: u32, body| Item {
            id: ItemId(id),
            turn: TurnId(t),
            at_ms: at(t, step),
            body,
        };
        state.items.push(item(
            format!("u{t}"),
            0,
            ItemBody::User(UserMessage {
                text: Clipped::whole("Run the tests and fix what fails"),
                images: Vec::new(),
                command: None,
                intent: None,
            }),
        ));
        for c in 0..calls {
            state.items.push(item(
                format!("t{t}.{c}"),
                c.saturating_add(1),
                ItemBody::Tool(Box::new(ToolCall {
                    name: "Bash".to_owned(),
                    kind: kind::EXEC.to_owned(),
                    title: "cargo test".to_owned(),
                    input: Clipped::whole(r#"{"command":"cargo test"}"#),
                    state: ToolState::Completed,
                    output: Some(Clipped::whole(&output)),
                    images: Vec::new(),
                    detail: Some(ToolDetail::Exec(ExecDetail {
                        command: Clipped::whole("cargo test"),
                        description: None,
                        cwd: None,
                        background: false,
                        task: None,
                        status: ExecStatus::Done,
                        exit_code: Some(0),
                        stderr: None,
                        duration_ms: Some(1_200),
                    })),
                    child: None,
                    ended_ms: Some(at(t, c.saturating_add(2))),
                })),
            ));
        }
        state.items.push(item(
            format!("a{t}"),
            calls.saturating_add(2),
            ItemBody::Text(Clipped::whole(&answer)),
        ));
    }
    state
}
