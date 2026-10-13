//! Coding agents: what starts one, how it stands, and where its work lands
//! (`docs/decisions/projects.md`).
//!
//! What an agent is doing travels as its thread's row ([`crate::thread::wire::ThreadRow`]);
//! the worker's hook tracker that feeds the row keeps its own vocabulary off the wire. An agent
//! that starts another ([`crate::orchestration::Verb::AgentStart`]) is its parent: the child's
//! row says so ([`PARENT_FACT`]), so every device draws the same tree.

use serde::{Deserialize, Serialize};
use slopty_core::{SessionId, WallMs, WorkerId};

use crate::thread::{AgentId, Phase, ThreadId, Wait};

/// The fact on a thread's row ([`crate::thread::wire::ThreadRow::facts`]) naming the seat it
/// was started at, so the server knows the row whatever terminal it has.
pub const SEAT_FACT: &str = "slopty.seat";
/// The fact on a thread's row naming the thread of the agent that started it.
pub const PARENT_FACT: &str = "slopty.parent";
/// The most bytes of words one [`crate::orchestration::Verb::AgentTell`] carries.
pub const TELL_MAX: usize = 16 * 1024;
/// The most agents one [`crate::orchestration::Verb::AgentWait`] names.
pub const WAIT_MAX: usize = 64;

/// What [`crate::orchestration::Verb::AgentStart`] asks for.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AgentStart {
    /// The machine it runs on.
    pub worker: WorkerId,
    /// The folder it works in: absolute, or `~/…`. The machine's clone of `repo` when absent.
    pub folder: Option<String>,
    /// What it is told first.
    pub prompt: String,
    /// Which agent; Claude Code when absent.
    pub agent: Option<AgentId>,
    /// The model, by the agent's own id; its default when absent.
    pub model: Option<String>,
    /// How hard it thinks, by the agent's own name; its default when absent.
    pub effort: Option<String>,
    /// A fresh git worktree of the folder's clone to work in, by this name (its branch is
    /// `worktree-<name>`); the folder itself when absent.
    pub worktree: Option<String>,
    /// A remote repository's address: cloned onto the machine first when it has no clone of it.
    pub repo: Option<String>,
}

/// One agent as the fleet has it ([`crate::orchestration::Outcome::Agents`]).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AgentEntry {
    /// Its thread: the id every agent verb takes.
    pub id: ThreadId,
    /// The agent that started it, when one did.
    pub parent: Option<ThreadId>,
    /// The machine it runs on.
    pub worker: WorkerId,
    /// Which agent.
    pub agent: AgentId,
    /// What its thread is about.
    pub title: String,
    /// The folder it works in.
    pub folder: Option<String>,
    /// The branch it works on, when the server knows it.
    pub branch: Option<String>,
    /// Where it stands, as its harness says.
    pub phase: Phase,
    /// What it waits on, while it waits.
    pub wait: Option<Wait>,
    /// Its program has ended.
    pub ended: bool,
    /// What it asks the person: its first open request's title.
    pub asks: Option<String>,
    /// The last line it wrote.
    pub last: Option<String>,
    /// When it entered its phase, by its worker's clock.
    pub since_ms: WallMs,
    /// It needs someone, has ended, or has come to rest after a turn: what
    /// [`crate::orchestration::Verb::AgentWait`] waits for. An agent idle before its first
    /// turn ended is starting, not at rest.
    pub rests: bool,
}

/// Where an agent's work lands, as its status line names it: the worktree it runs in.
///
/// Its branch's pull request is its thread's, which the worker reads from the forge
/// ([`crate::thread::wire::ThreadRow::pull`]), never what the status line says of one.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct AgentBranch {
    /// The terminal session the agent runs in.
    pub session: SessionId,
    /// The worktree, when the agent runs in one Claude Code made (`--worktree`).
    pub worktree: Option<Worktree>,
}

/// A worktree Claude Code made for a session.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Worktree {
    /// Its name.
    pub name: String,
    /// Where it is.
    pub path: String,
    /// Its branch, when git made it.
    pub branch: Option<String>,
    /// The directory the session started in.
    pub original_cwd: String,
    /// The branch that directory was on, when git made the worktree.
    pub original_branch: Option<String>,
}
