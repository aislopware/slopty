//! Any agent that speaks the Agent Client Protocol (ACP v1), driven through one adapter.
//!
//! It serves the long tail beside the three agents with adapters of their own: Gemini CLI,
//! Copilot, Cursor, `OpenCode`, Goose and the rest of the registry.
//!
//! - [`registry`]: which agents there are and how each is started, an open list.
//! - [`rpc`]: the framing, JSON-RPC on the agent's stdio, in the protocol's own types.
//! - [`driven`]: the codec, sans-IO, from what the agent says to the thread model and from what a
//!   client asks to what goes to the agent.
//!
//! Slopty is the ACP client. It offers the agent no file system and no terminal of its own, so
//! the agent works with its own tools and asks before it acts (`session/request_permission`);
//! a request of any other kind is refused. It never signs an agent in: one that asks to be is
//! left as it is, with the reason.

pub mod driven;
pub mod registry;
pub mod rpc;

/// The protocol's own types, for the worker that carries them.
pub use agent_client_protocol_schema::v1 as schema;
use slopty_proto::thread::AgentId;

/// What names an ACP agent's threads: `acp:<name>`.
pub const AGENT_PREFIX: &str = AgentId::ACP_PREFIX;

/// The agent of an ACP agent named `name`.
#[must_use]
pub fn agent_id(name: &str) -> AgentId {
    AgentId::acp(name)
}

/// The name of the ACP agent `agent` is, when it is one.
#[must_use]
pub fn name_of(agent: &AgentId) -> Option<&str> {
    agent.acp_name()
}
