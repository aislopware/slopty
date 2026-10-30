// Derived from zeron (MIT, github.com/zeronsh/zeron @ 3b2f780), see THIRD_PARTY_NOTICES.md
//! Agent-side wire types: harness identity, tool calls, checklists and the questions an agent asks.

use serde::{Deserialize, Serialize};

// An agent is named, not enumerated: any agent a worker offers (Claude Code, Codex, pi, any ACP
// agent by its registry's name) is a harness, so the type is an open, interned name. The known
// names are constants, and matches on them keep a fallback arm.
/// An agent harness, by its wire name (`claude-code`, `codex`, an ACP agent's registry name).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct HarnessId(&'static str);

impl HarnessId {
    /// Google's Antigravity agent over ACP.
    pub const ANTIGRAVITY: Self = Self("antigravity");
    /// Anthropic's Claude Code.
    pub const CLAUDE_CODE: Self = Self("claude-code");
    /// The Codex agent.
    pub const CODEX: Self = Self("codex");
    /// Cursor's agent, driven over ACP.
    pub const CURSOR: Self = Self("cursor");
    /// Cognition's Devin agent, driven over ACP (`devin acp`).
    pub const DEVIN: Self = Self("devin");
    /// xAI's Grok Build agent, driven over ACP (`grok agent stdio`).
    pub const GROK: Self = Self("grok");
    /// Nous Research's Hermes Agent, driven over ACP (`hermes acp`).
    pub const HERMES: Self = Self("hermes");
    /// The names known at build time, the rest interned as they are first seen.
    const KNOWN: [Self; 10] = [
        Self::CLAUDE_CODE,
        Self::CODEX,
        Self::CURSOR,
        Self::DEVIN,
        Self::GROK,
        Self::HERMES,
        Self::PI,
        Self::OPENCODE,
        Self::ANTIGRAVITY,
        Self::MOCK,
    ];
    /// Test harness; never shown in production pickers.
    pub const MOCK: Self = Self("mock");
    /// SST's opencode agent.
    pub const OPENCODE: Self = Self("opencode");
    /// The Pi coding agent (pi.dev), driven over its native JSONL RPC protocol.
    pub const PI: Self = Self("pi");

    /// The harness named `name`: one of the constants, or a name interned for the life of the
    /// process (agents are few, so the names kept stay few).
    pub fn named(name: &str) -> Self {
        static SEEN: parking_lot::Mutex<Vec<&'static str>> = parking_lot::Mutex::new(Vec::new());
        if let Some(known) = Self::KNOWN.iter().find(|known| known.0 == name) {
            return *known;
        }
        let mut seen = SEEN.lock();
        let kept = if let Some(kept) = seen.iter().find(|kept| **kept == name) {
            *kept
        } else {
            let kept: &'static str = Box::leak(name.to_owned().into_boxed_str());
            seen.push(kept);
            kept
        };
        drop(seen);
        Self(kept)
    }

    /// Its wire name.
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Debug for HarnessId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HarnessId({})", self.0)
    }
}

impl std::fmt::Display for HarnessId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl Serialize for HarnessId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0)
    }
}

impl<'de> Deserialize<'de> for HarnessId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = std::borrow::Cow::<'de, str>::deserialize(deserializer)?;
        if name.is_empty() {
            return Err(serde::de::Error::custom("an empty harness name"));
        }
        Ok(Self::named(&name))
    }
}

/// How hard an agent thinks before it answers, from least to most.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningLevel {
    /// The least thinking a harness offers.
    Minimal,
    /// Low effort.
    Low,
    /// Medium effort.
    Medium,
    /// High effort.
    High,
    /// Extra-high effort.
    XHigh,
    /// The most a model's own setting offers.
    Max,
    /// Past the maximum, where a harness offers more.
    Ultra,
    /// xhigh + harness-specific setting.
    Ultracode,
    /// Prompt-prefix driven (Claude).
    Ultrathink,
}

/// What an agent's commands may touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxLevel {
    /// They read and never write.
    ReadOnly,
    /// They write inside the workspace only.
    WorkspaceWrite,
    /// They write anywhere, without a sandbox.
    DangerFullAccess,
}

/// When words sent to a working agent reach it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SteeringMode {
    /// Steer delivered at the next step boundary within the live turn.
    StepBoundary,
    /// Steer delivered only between turns.
    TurnBoundary,
}

/// A decoded tool invocation, reduced to the fields each kind renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ToolCall {
    /// A shell command.
    Exec {
        /// The command line, whole.
        command: String,
    },
    /// A file read.
    ReadFile {
        /// The file read.
        path: String,
    },
    /// A file written whole.
    WriteFile {
        /// The file written.
        path: String,
        /// Full content; STRIPPED by the render-parts policy before entering the doc.
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<String>,
    },
    /// A file edited in place.
    EditFile {
        /// The file edited.
        path: String,
        /// The text replaced, when the harness says.
        #[serde(skip_serializing_if = "Option::is_none")]
        old_string: Option<String>,
        /// The text put in its place, when the harness says.
        #[serde(skip_serializing_if = "Option::is_none")]
        new_string: Option<String>,
    },
    /// A patch applied.
    ApplyPatch {
        /// The file patched, when the patch names one.
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// A text search through files.
    Search {
        /// What it searches for.
        pattern: String,
        /// Where it searches, when not the workspace.
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// A file search by name pattern.
    Glob {
        /// The pattern.
        pattern: String,
    },
    /// A web page fetched.
    WebFetch {
        /// The page's address.
        url: String,
        /// What the agent asked of the page, when it said.
        #[serde(skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    /// A web search.
    WebSearch {
        /// What it searched for.
        query: String,
    },
    /// The agent's checklist, set again.
    Todo {
        /// The checklist, in order.
        #[serde(default)]
        items: Vec<TodoItem>,
    },
    /// A tool of an MCP server.
    Mcp {
        /// The server.
        server: String,
        /// The tool.
        tool: String,
        /// The call's input.
        #[serde(skip_serializing_if = "Option::is_none")]
        input: Option<serde_json::Value>,
    },
    /// Any other tool.
    Unknown {
        /// The tool's name.
        name: String,
        /// The call's input.
        #[serde(skip_serializing_if = "Option::is_none")]
        input: Option<serde_json::Value>,
    },
}

impl ToolCall {
    /// A subagent SPAWN call — the `Agent[: <description>]` naming convention
    /// every driver decodes its spawn tool into (claude/codex `Task`, cursor
    /// `task`, grok `spawn_subagent`, opencode `task`). This is the single
    /// genus gate for subagent binding: tagged subagent traffic may only ever
    /// stamp a ref/status onto a spawn call, so a driver keying bug can never
    /// turn an ordinary Run/Read chip into a spawn chip (2026-08-20: claude's
    /// background-shell `task_notification` did exactly that — the chip
    /// linked to a never-created doc and opened an empty panel).
    pub fn is_subagent_spawn(&self) -> bool {
        let name = match self {
            Self::Unknown { name, .. } => name,
            Self::Mcp { tool, .. } => tool,
            _ => return false,
        };
        name == "Agent" || name.starts_with("Agent: ")
    }

    /// The model a subagent SPAWN was given, when the spawn named one.
    ///
    /// Read off the spawn's own input rather than the session's picked model:
    /// a spawn may override it per child (claude's `Agent` takes `model`, grok
    /// `spawn_subagent` a `model_id`), and two chips spawned in one turn can
    /// legitimately name different models. `None` means the spawn didn't say —
    /// the child inherits the parent's model, which the chip already implies,
    /// so nothing is rendered rather than guessing a name.
    ///
    /// Only ever answers for [`is_subagent_spawn`](Self::is_subagent_spawn)
    /// calls: an ordinary tool with a stray `model` argument is not a spawn.
    pub fn subagent_model(&self) -> Option<&str> {
        if !self.is_subagent_spawn() {
            return None;
        }
        let input = match self {
            Self::Unknown { input, .. } | Self::Mcp { input, .. } => input.as_ref()?,
            _ => return None,
        };
        SUBAGENT_MODEL_KEYS
            .iter()
            .find_map(|key| input.get(key).and_then(serde_json::Value::as_str))
            .map(str::trim)
            .filter(|model| !model.is_empty())
    }
}

/// Spawn-input keys that carry a child model, in precedence order. Drivers
/// disagree on the spelling, so the lookup is by key set, not by harness —
/// a new adapter naming it any of these needs no code change here.
const SUBAGENT_MODEL_KEYS: [&str; 4] = ["model", "modelId", "model_id", "subagent_model"];

/// Where one checklist item stands. Claude's `TodoWrite`, opencode and ACP plans
/// distinguish the item being worked on from those still waiting; Codex and
/// Cursor only report done / not done.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TodoStatus {
    /// Not started.
    #[default]
    Pending,
    /// Being worked on.
    InProgress,
    /// Done.
    Completed,
}

impl TodoStatus {
    /// Decode a harness status string. Anything unrecognised (including
    /// `cancelled`) is `Pending`: not finished, not being worked on.
    pub fn parse(raw: &str) -> Self {
        match raw {
            "completed" | "complete" | "done" => Self::Completed,
            "in_progress" | "inProgress" | "in-progress" | "active" => Self::InProgress,
            _ => Self::Pending,
        }
    }
}

/// One checklist entry.
///
/// `done` is the original wire field and stays authoritative for completion,
/// so docs written before `status` existed (and readers that ignore it) keep
/// working. `status` is additive and written only when it adds information —
/// i.e. for an in-progress item; a missing or unreadable value derives from
/// `done`. Build items with [`TodoItem::new`] to keep the two consistent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoItem {
    /// The item's words.
    pub text: String,
    /// Whether it is done.
    pub done: bool,
    /// Where it stands, written only for an item in progress.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_todo_status"
    )]
    pub status: Option<TodoStatus>,
}

impl TodoItem {
    /// An item at `status`, its `done` and `status` agreeing.
    pub fn new(text: impl Into<String>, status: TodoStatus) -> Self {
        Self {
            text: text.into(),
            done: status == TodoStatus::Completed,
            status: (status == TodoStatus::InProgress).then_some(status),
        }
    }

    /// Effective status: `done` wins, then the explicit status, else pending.
    pub fn status(&self) -> TodoStatus {
        if self.done { TodoStatus::Completed } else { self.status.unwrap_or_default() }
    }
}

/// A status this build does not know (a future `blocked`, say) must not fail
/// the whole tool call — the part would vanish from the transcript. Treat it
/// as absent so the item falls back to `done`.
fn lenient_todo_status<'de, D>(deserializer: D) -> Result<Option<TodoStatus>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|v| serde_json::from_value(v).ok()))
}

/// A file modification carried inline on a tool result (ACP
/// `ToolCallContent::Diff`). `old_text: None` means a new file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDiff {
    /// The file changed.
    pub path: String,
    /// Its text before, none for a new file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_text: Option<String>,
    /// Its text after.
    pub new_text: String,
}

/// A request an agent waits on the person for: a question, an approval, a plan or a dialog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQuestion {
    /// The request's id within its turn.
    pub id: String,
    /// Its title: an agent question's header, an approval's tool.
    pub header: String,
    /// What it asks, in the agent's words.
    pub question: String,
    /// The answers it offers, by label.
    pub options: Vec<String>,
    /// The request's kind, as the thread names it: `approval`, `plan`, `question`,
    /// `elicitation`, `retry`, `dialog:<kind>`, or one Slopty does not know.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// The command an approval would run, whole; none for any other request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drivers spell the key differently; the chip must not care which one
    /// spawned the child. Non-spawns never answer, whatever they carry.
    #[test]
    fn subagent_model_reads_every_spelling_and_only_off_a_spawn() {
        let spawn = |input: serde_json::Value| ToolCall::Unknown {
            name: "Agent: scan".into(),
            input: Some(input),
        };
        for key in SUBAGENT_MODEL_KEYS {
            let call = spawn(serde_json::json!({ key: "haiku" }));
            assert_eq!(call.subagent_model(), Some("haiku"), "key {key}");
        }
        // An MCP-shaped spawn (cursor routes its `task` through MCP) too.
        assert_eq!(
            ToolCall::Mcp {
                server: "s".into(),
                tool: "Agent: scan".into(),
                input: Some(serde_json::json!({ "model": "sonnet" })),
            }
            .subagent_model(),
            Some("sonnet")
        );
        // Not a spawn: the name gate wins over the key.
        assert_eq!(
            ToolCall::Unknown {
                name: "Bash".into(),
                input: Some(serde_json::json!({ "model": "haiku" })),
            }
            .subagent_model(),
            None
        );
        // A spawn that named nothing usable inherits — nothing to render.
        assert_eq!(spawn(serde_json::json!({ "model": " " })).subagent_model(), None);
        assert_eq!(spawn(serde_json::json!({ "prompt": "x" })).subagent_model(), None);
        assert_eq!(ToolCall::Unknown { name: "Agent".into(), input: None }.subagent_model(), None);
        // Non-string values are not names.
        assert_eq!(spawn(serde_json::json!({ "model": 5 })).subagent_model(), None);
    }

    #[test]
    fn harness_id_uses_kebab_case() {
        assert_eq!(serde_json::to_string(&HarnessId::CLAUDE_CODE).unwrap(), "\"claude-code\"");
    }
}

#[cfg(test)]
mod todo_item_tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn legacy_items_without_status_derive_from_done() {
        let done: TodoItem = serde_json::from_value(json!({ "text": "a", "done": true })).unwrap();
        let open: TodoItem = serde_json::from_value(json!({ "text": "b", "done": false })).unwrap();
        assert_eq!(done.status(), TodoStatus::Completed);
        assert_eq!(open.status(), TodoStatus::Pending);
        assert_eq!(open.status, None);
    }

    #[test]
    fn only_in_progress_adds_a_field_so_other_docs_are_unchanged() {
        let pending = serde_json::to_value(TodoItem::new("a", TodoStatus::Pending)).unwrap();
        let done = serde_json::to_value(TodoItem::new("b", TodoStatus::Completed)).unwrap();
        let active = serde_json::to_value(TodoItem::new("c", TodoStatus::InProgress)).unwrap();
        assert_eq!(pending, json!({ "text": "a", "done": false }));
        assert_eq!(done, json!({ "text": "b", "done": true }));
        assert_eq!(active, json!({ "text": "c", "done": false, "status": "inProgress" }));
    }

    #[test]
    fn unknown_status_falls_back_to_done_instead_of_failing_the_call() {
        let call: ToolCall = serde_json::from_value(json!({
            "kind": "todo",
            "items": [
                { "text": "a", "done": false, "status": "blocked" },
                { "text": "b", "done": true, "status": 7 },
            ]
        }))
        .unwrap();
        let ToolCall::Todo { items } = call else {
            panic!("expected a todo call");
        };
        assert_eq!(items[0].status(), TodoStatus::Pending);
        assert_eq!(items[1].status(), TodoStatus::Completed);
    }

    #[test]
    fn done_outranks_a_stale_status() {
        let item = TodoItem { text: "a".into(), done: true, status: Some(TodoStatus::InProgress) };
        assert_eq!(item.status(), TodoStatus::Completed);
    }

    #[test]
    fn harness_status_strings_decode() {
        assert_eq!(TodoStatus::parse("completed"), TodoStatus::Completed);
        assert_eq!(TodoStatus::parse("in_progress"), TodoStatus::InProgress);
        assert_eq!(TodoStatus::parse("inProgress"), TodoStatus::InProgress);
        assert_eq!(TodoStatus::parse("pending"), TodoStatus::Pending);
        assert_eq!(TodoStatus::parse("cancelled"), TodoStatus::Pending);
        assert_eq!(TodoStatus::parse(""), TodoStatus::Pending);
    }
}

#[cfg(test)]
mod harness_id_tests {
    use super::HarnessId;

    /// A harness reads and writes as its wire name: a known one is its constant, any other is
    /// kept by its name, the same value each time it is read; an empty name is no harness.
    #[test]
    fn a_harness_is_its_name() {
        let read = |text: &str| serde_json::from_str::<HarnessId>(text);
        assert_eq!(read("\"claude-code\"").unwrap(), HarnessId::CLAUDE_CODE);
        assert_eq!(serde_json::to_string(&HarnessId::OPENCODE).unwrap(), "\"opencode\"");
        let gemini = read("\"gemini\"").unwrap();
        assert_eq!(gemini.as_str(), "gemini");
        assert_eq!(gemini, HarnessId::named("gemini"));
        assert!(std::ptr::eq(gemini.as_str(), HarnessId::named("gemini").as_str()), "interned");
        read("\"\"").unwrap_err();
    }
}
