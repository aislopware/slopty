//! The agents on the fleet as a tree, each with its parent: what `agents` and `agent_wait`
//! answer, for JSON and for a person.

use std::collections::HashMap;

use serde::Serialize;
use slopty_core::{WallMs, WorkerId};
use slopty_proto::agent::AgentEntry;
use slopty_proto::thread::ThreadId;

use super::agents::phase_key;

/// One agent, for JSON.
#[derive(Debug, Serialize)]
pub struct AgentEntryView<'a> {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
    worker: String,
    agent: &'a str,
    title: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    folder: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<&'a str>,
    phase: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    wait: Option<&'a str>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    ended: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    last: Option<&'a str>,
    /// It needs someone or has come to rest.
    rests: bool,
    since_ms: WallMs,
}

/// Agents as they stand, for JSON.
#[derive(Debug, Serialize)]
pub struct AgentsView<'a> {
    agents: Vec<AgentEntryView<'a>>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    timed_out: bool,
}

/// `agents` as they stand, `timed_out` when a wait ran out first, for JSON; each worker by
/// its name where `names` knows it.
#[must_use]
pub fn agents<'a, S: std::hash::BuildHasher>(
    agents: &'a [AgentEntry],
    timed_out: bool,
    names: &HashMap<WorkerId, String, S>,
) -> AgentsView<'a> {
    let agents = agents
        .iter()
        .map(|a| AgentEntryView {
            id: a.id.to_string(),
            parent: a.parent.map(|p| p.to_string()),
            worker: names.get(&a.worker).cloned().unwrap_or_else(|| a.worker.to_string()),
            agent: &a.agent.0,
            title: &a.title,
            folder: a.folder.as_deref(),
            branch: a.branch.as_deref(),
            phase: phase_key(a.phase),
            wait: a.wait.as_ref().map(|w| w.text.as_str()),
            ended: a.ended,
            last: a.last.as_deref(),
            rests: a.rests,
            since_ms: a.since_ms,
        })
        .collect();
    AgentsView { agents, timed_out }
}

/// `agents` as a tree for a person: each under the agent that started it.
#[must_use]
pub fn agents_text<S: std::hash::BuildHasher>(
    agents: &[AgentEntry],
    names: &HashMap<WorkerId, String, S>,
) -> String {
    let known: Vec<ThreadId> = agents.iter().map(|a| a.id).collect();
    let mut out = String::new();
    let tops = agents.iter().filter(|a| a.parent.is_none_or(|p| !known.contains(&p)));
    for top in tops {
        line(&mut out, top, agents, names, 0);
    }
    out
}

fn line<S: std::hash::BuildHasher>(
    out: &mut String,
    a: &AgentEntry,
    all: &[AgentEntry],
    names: &HashMap<WorkerId, String, S>,
    depth: usize,
) {
    let worker = names.get(&a.worker).cloned().unwrap_or_else(|| a.worker.to_string());
    let phase = if a.ended { "ended" } else { phase_key(a.phase) };
    let title = if a.title.trim().is_empty() { "(untitled)" } else { a.title.trim() };
    let fields = [a.id.to_string(), phase.to_owned(), worker, title.to_owned()];
    out.push_str(&"  ".repeat(depth));
    out.push_str(&fields.join("  "));
    out.push('\n');
    // A tree is never deeper than its agents; a loop in the parents stops there.
    if depth >= all.len() {
        return;
    }
    for child in all.iter().filter(|c| c.parent == Some(a.id)) {
        line(out, child, all, names, depth.saturating_add(1));
    }
}
