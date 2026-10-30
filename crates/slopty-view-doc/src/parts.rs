// Derived from zeron (MIT, github.com/zeronsh/zeron @ 3b2f780), see THIRD_PARTY_NOTICES.md
//! A transcript entry's parts as the view model serves them ([`MessagePart`]), and the line
//! counts a tool part carries in place of its diff ([`ToolDiffStat`]).

use serde::{Deserialize, Serialize};
use slopty_view_proto::{ToolCall, UserInputQuestion};

/// One file's added and removed lines, which a tool part carries in place of its diff: a diff
/// runs to tens of KB an edit, so the whole of it is fetched behind the part's `diff_ref`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDiffStat {
    /// The file.
    pub path: String,
    /// The lines added.
    pub additions: u64,
    /// The lines removed.
    pub deletions: u64,
}

/// How far an answer of the agent's has come.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageStatus {
    /// The agent is still writing it.
    Streaming,
    /// The agent finished it.
    Complete,
    /// It stopped short: the turn was interrupted or failed.
    Aborted,
}

/// Lifecycle of a spawned subagent, carried on its spawn chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SubagentStatus {
    /// It is still at work.
    Running,
    /// It finished.
    Done,
    /// It failed.
    Failed,
}

/// One rendered part of an assistant message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MessagePart {
    /// The agent's prose.
    Text {
        /// The part's id.
        id: String,
        /// What it says, in Markdown.
        text: String,
    },
    /// An image the agent made or showed.
    #[serde(rename_all = "camelCase")]
    Image {
        /// The part's id.
        id: String,
        /// Where it is on the machine the session runs on.
        path: String,
        /// Its file name.
        name: String,
        /// Its type, as `image/png`.
        mime_type: String,
    },
    /// A visual the agent drew (`show_visual`): a Luau script on the session's machine at
    /// `path`, drawn natively.
    Visual {
        /// The part's id.
        id: String,
        /// The script.
        path: String,
        /// What the agent called it.
        title: String,
    },
    /// The model's thinking.
    #[serde(rename_all = "camelCase")]
    Reasoning {
        /// The part's id.
        id: String,
        /// What it thought.
        text: String,
        /// How long the model thought, once known, for its group's "thought for 12s".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thought_ms: Option<u64>,
    },
    /// A tool call and what came of it.
    #[serde(rename_all = "camelCase")]
    Tool {
        /// The part's id: the call's.
        id: String,
        /// What the agent called, with its input.
        call: ToolCall,
        /// The call failed.
        #[serde(default)]
        is_error: bool,
        /// Its result came.
        #[serde(default)]
        resolved: bool,
        /// The output's head as the thread holds it (a running command's
        /// tail); the whole is behind `output_ref` when this is cut.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        /// The key (`{chatId}/{partId}`) the whole output is fetched by.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_ref: Option<String>,
        /// Full-output byte length, so the UI can say "Show full output (12 KB)".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_bytes: Option<u64>,
        /// The key (`{chatId}/{partId}.diff`) the whole diff is fetched by, as JSON.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff_ref: Option<String>,
        /// Each changed file's line counts.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff_stats: Option<Vec<ToolDiffStat>>,
        /// The transcript a subagent's spawn opens: the spawn card is the only way to it, as
        /// there is no list of a session's subagents to ask.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_ref: Option<String>,
        /// How the spawned subagent stands, apart from `resolved`: the spawn's own result can
        /// come while the subagent still runs. While it runs its transcript is watched; once
        /// done or failed it is read once.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_status: Option<SubagentStatus>,
        /// The call's facts: its line in the TUI's words, what came of it, its timer, the tray
        /// row of the task it became, and a subagent's progress on its spawn card.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        facts: Option<Box<crate::part_kinds::ToolFacts>>,
    },
    /// Questions the agent put to the person, answered in its TUI.
    #[serde(rename_all = "camelCase")]
    Input {
        /// The part's id.
        id: String,
        /// The agent's id for the request.
        request_id: String,
        /// What it asked.
        questions: Vec<UserInputQuestion>,
        /// The person answered.
        #[serde(default)]
        resolved: bool,
    },
    /// A failure the transcript shows.
    Error {
        /// The part's id.
        id: String,
        /// The failure in Slopty's words.
        message: String,
        /// What the agent or its machine said, word for word, behind the copy.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// A one-line divider between turns that no one wrote: "Compacted · 143k tokens →
    /// summary", "ui-lane came to rest".
    Marker {
        /// The part's id.
        id: String,
        /// The divider's words.
        text: String,
    },
    /// A background task's notice, a quiet row linked to its task's tray row
    /// ([`crate::part_kinds::TaskRow::row`], `task:{id}`): an agent finished or waits on its
    /// own work, a command exited, a monitor's event. Its part id is the thread item's.
    Task {
        /// The part's id.
        id: String,
        /// The row.
        task: Box<crate::part_kinds::TaskRow>,
    },
    /// A page shown inline by the call before it: `show_page`, an MCP App's UI, an `.html` or
    /// `.svg` file. Its part id is `{call part id}:page`; the app hosts its view by
    /// [`crate::page_card::PageCard::page`].
    Page {
        /// The part's id.
        id: String,
        /// The card.
        card: Box<crate::page_card::PageCard>,
    },
    /// A quiet row of the agent's own that is no failure: "Allowed cargo test", "Saved 2
    /// memories", a hook's word, a request tried again with its countdown.
    Note {
        /// The part's id.
        id: String,
        /// The row.
        note: Box<crate::part_kinds::NoteRow>,
    },
}

impl MessagePart {
    /// The part's id.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Text { id, .. }
            | Self::Image { id, .. }
            | Self::Visual { id, .. }
            | Self::Reasoning { id, .. }
            | Self::Tool { id, .. }
            | Self::Input { id, .. }
            | Self::Error { id, .. }
            | Self::Marker { id, .. }
            | Self::Task { id, .. }
            | Self::Page { id, .. }
            | Self::Note { id, .. } => id,
        }
    }

    /// About how many bytes the part holds: its text, or its serialized payload.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        fn json(value: &impl Serialize) -> usize {
            serde_json::to_vec(value).map_or(0, |bytes| bytes.len())
        }
        let optional = |text: Option<&str>| text.map_or(0, str::len);
        let lengths: [usize; 4] = match self {
            Self::Text { text, .. } | Self::Reasoning { text, .. } | Self::Marker { text, .. } => {
                [text.len(), 0, 0, 0]
            }
            Self::Tool { call, output, diff_stats, .. } => {
                [json(call), optional(output.as_deref()), diff_stats.as_ref().map_or(0, json), 0]
            }
            Self::Input { questions, .. } => [json(questions), 0, 0, 0],
            Self::Image { id, path, name, mime_type } => {
                [id.len(), path.len(), name.len(), mime_type.len()]
            }
            Self::Visual { id, path, title } => [id.len(), path.len(), title.len(), 0],
            Self::Error { message, detail, .. } => {
                [message.len(), optional(detail.as_deref()), 0, 0]
            }
            Self::Task { task, .. } => {
                [task.text.len(), task.title.len(), optional(task.body.as_deref()), 0]
            }
            Self::Page { card, .. } => [json(card), 0, 0, 0],
            Self::Note { note, .. } => [note.text.len(), optional(note.detail.as_deref()), 0, 0],
        };
        lengths.iter().sum()
    }
}
