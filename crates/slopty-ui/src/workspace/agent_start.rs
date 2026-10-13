//! What a machine can start: the agents its own link says are installed there, Claude Code,
//! Codex, pi and every ACP agent alike, so a machine reached with no server offers all it has.

use slopty_client::WorkerKey;
use slopty_proto::thread::AgentId;

use super::WorkspaceView;

impl WorkspaceView {
    /// The agents `key` can start a thread of while its link is up: those its link says are
    /// installed there, each once.
    pub(super) fn startable_on(&self, key: WorkerKey) -> Vec<AgentId> {
        let Some(w) = self.workers.get(&key).filter(|w| w.link.is_some()) else {
            return Vec::new();
        };
        let mut out: Vec<AgentId> = Vec::new();
        for agent in w.caps.iter().flat_map(|c| &c.agents).map(|a| a.agent.clone()) {
            if !out.contains(&agent) {
                out.push(agent);
            }
        }
        out
    }
}
