//! pi's RPC records, held to what the pinned build was recorded saying (`tests/fixtures/pi`).
//!
//! Written from pi's documentation: `rpc.md`, `rpc-commands.md`, `json.md`,
//! `rpc-extension-ui.md` and `message-types.md`.
//!
//! - **Framing.** One JSON object per record, ended by LF alone ([`record`], [`line()`]): a generic
//!   line reader that also splits on U+2028 would cut a record in two.
//! - **To pi**, a [`Request`]: a [`Command`] with the id its response repeats. The answer to a
//!   dialog is one too, under the dialog's id, and gets no response.
//! - **From pi**, an [`Incoming`]: a command's [`Response`], a session event, or a dialog an
//!   extension opened ([`UiRequest`]). A record of a kind this does not know is
//!   [`Incoming::Other`], never an error, so a newer pi degrades rather than breaks.
//! - **The gate.** A `select` whose title is the gate's JSON is a [`GateAsk`]; [`allow`] and
//!   [`deny`] are its answers.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::GATE_PROTOCOL;

/// The record in `line`, read as pi frames it: one JSON object, its LF (and a CR before it)
/// stripped.
///
/// # Errors
///
/// When the line is not a record this knows the shape of.
pub fn record(line: &[u8]) -> serde_json::Result<Incoming> {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    serde_json::from_slice(line)
}

/// `request` as one record for pi's stdin, ended by its LF.
///
/// # Errors
///
/// Never for these types; serde's signature has it.
pub fn line(request: &Request) -> serde_json::Result<Vec<u8>> {
    let mut out = serde_json::to_vec(request)?;
    out.push(b'\n');
    Ok(out)
}

/// What goes to pi.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Request {
    /// The id the response repeats; for a dialog's answer, the dialog's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// What is asked.
    #[serde(flatten)]
    pub command: Command,
}

/// When a prompt sent while pi works goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamingBehavior {
    /// After the current assistant turn's tool calls, before the next model call.
    Steer,
    /// Once pi has nothing else to do.
    FollowUp,
}

/// A picture sent with a message.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ImageContent {
    /// Always `image`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Base64.
    pub data: String,
    /// Its media type.
    #[serde(rename = "mimeType")]
    pub mime_type: String,
}

/// A command, by its `type`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// A message from the person; while pi works it needs a [`StreamingBehavior`].
    Prompt {
        /// What it says.
        message: String,
        /// Pictures with it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageContent>,
        /// When it goes, while pi works.
        #[serde(default, rename = "streamingBehavior", skip_serializing_if = "Option::is_none")]
        streaming_behavior: Option<StreamingBehavior>,
    },
    /// A message into the run under way.
    Steer {
        /// What it says.
        message: String,
        /// Pictures with it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageContent>,
    },
    /// A message for when the run is over.
    FollowUp {
        /// What it says.
        message: String,
        /// Pictures with it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageContent>,
    },
    /// Stop the run, answered once pi is idle.
    Abort,
    /// Take back every queued message.
    ClearQueue,
    /// The session's state.
    GetState,
    /// Use another model.
    SetModel {
        /// Its provider.
        provider: String,
        /// Its id.
        #[serde(rename = "modelId")]
        model_id: String,
    },
    /// Every model pi can use.
    GetAvailableModels,
    /// How hard the model thinks.
    SetThinkingLevel {
        /// `off` to `max`.
        level: String,
    },
    /// Compact the context.
    Compact {
        /// What the summary should keep.
        #[serde(default, rename = "customInstructions", skip_serializing_if = "Option::is_none")]
        custom_instructions: Option<String>,
    },
    /// The session's entries after `since`, or all of them.
    GetEntries {
        /// The last entry id held.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<String>,
    },
    /// Tokens, cost and the context window's use.
    GetSessionStats,
    /// Name the session.
    SetSessionName {
        /// The name.
        name: String,
    },
    /// The answer to a dialog, under its id.
    ExtensionUiResponse {
        /// The option chosen or the text entered.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        /// A confirmation's answer.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confirmed: Option<bool>,
        /// Dismissed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cancelled: Option<bool>,
    },
    /// A command this does not know.
    #[serde(other)]
    Other,
}

/// What comes from pi, by its `type`.
#[derive(Clone, PartialEq, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Incoming {
    /// A command's answer.
    Response(Response),
    /// A dialog or a notice an extension opened.
    ExtensionUiRequest(UiRequest),
    /// A low-level run began.
    AgentStart,
    /// A low-level run ended; more may follow.
    AgentEnd {
        /// Whether pi retries it.
        #[serde(default, rename = "willRetry")]
        will_retry: bool,
    },
    /// pi has nothing more to do on its own.
    AgentSettled,
    /// An assistant response and its tool calls began.
    TurnStart,
    /// They ended.
    TurnEnd {
        /// The assistant's message.
        message: Option<Message>,
    },
    /// A message began.
    MessageStart {
        /// As it stands.
        message: Message,
    },
    /// The assistant's message grew.
    MessageUpdate {
        /// What changed.
        #[serde(rename = "assistantMessageEvent")]
        event: AssistantEvent,
        /// The response's usage so far.
        #[serde(default)]
        usage: Option<Usage>,
    },
    /// A message is whole: authoritative.
    MessageEnd {
        /// The message.
        message: Message,
    },
    /// A tool call began to run.
    ToolExecutionStart {
        /// The call.
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        /// Its tool.
        #[serde(rename = "toolName")]
        tool_name: String,
        /// Its arguments.
        #[serde(default)]
        args: Value,
    },
    /// A running call's latest partial result.
    ToolExecutionUpdate {
        /// The call.
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        /// The result so far.
        #[serde(default, rename = "partialResult")]
        partial_result: Option<ToolOutput>,
    },
    /// A call ended.
    ToolExecutionEnd {
        /// The call.
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        /// What it gave.
        result: ToolOutput,
        /// Whether it failed, was blocked or was aborted.
        #[serde(default, rename = "isError")]
        is_error: bool,
    },
    /// The queued messages, whole.
    QueueUpdate {
        /// Steering messages.
        #[serde(default)]
        steering: Vec<String>,
        /// Follow-up messages.
        #[serde(default, rename = "followUp")]
        follow_up: Vec<String>,
    },
    /// The session's name changed; none when it was cleared.
    SessionInfoChanged {
        /// The name.
        #[serde(default)]
        name: Option<String>,
    },
    /// The thinking level changed.
    ThinkingLevelChanged {
        /// The level.
        level: String,
    },
    /// Compaction began.
    CompactionStart {
        /// `manual`, `threshold` or `overflow`.
        reason: String,
    },
    /// Compaction ended.
    CompactionEnd {
        /// Why it ran.
        reason: String,
        /// Whether it was aborted.
        #[serde(default)]
        aborted: bool,
        /// Why it failed, when it did.
        #[serde(default, rename = "errorMessage")]
        error_message: Option<String>,
        /// What it came to, when it worked.
        #[serde(default)]
        result: Option<Compacted>,
    },
    /// A failed model call is retried.
    AutoRetryStart {
        /// Which attempt.
        attempt: u32,
        /// Out of how many.
        #[serde(rename = "maxAttempts")]
        max_attempts: u32,
        /// How long pi waits before it, in ms.
        #[serde(default, rename = "delayMs")]
        delay_ms: Option<u64>,
        /// What failed.
        #[serde(rename = "errorMessage")]
        error_message: String,
    },
    /// Retrying ended.
    AutoRetryEnd {
        /// Whether it worked.
        success: bool,
        /// Why not, at the end.
        #[serde(default, rename = "finalError")]
        final_error: Option<String>,
    },
    /// An extension's handler threw.
    ExtensionError {
        /// The handler's event.
        event: String,
        /// What it threw.
        error: String,
    },
    /// A record this does not know.
    #[serde(other)]
    Other,
}

/// A command's answer.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize)]
pub struct Response {
    /// The command's id, when it had one.
    #[serde(default)]
    pub id: Option<String>,
    /// The command's `type`.
    pub command: String,
    /// Whether it was taken.
    pub success: bool,
    /// What it returns.
    #[serde(default)]
    pub data: Option<Value>,
    /// Why it was not taken.
    #[serde(default)]
    pub error: Option<String>,
}

/// What a `prompt`, `steer` or `follow_up` came to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Disposition {
    /// A run began for it.
    Started,
    /// It waits in the queue.
    Queued,
    /// An extension's command took it; no run follows.
    Handled,
}

impl Response {
    /// What a prompt came to, when this answers one.
    #[must_use]
    pub fn disposition(&self) -> Option<Disposition> {
        let data = self.data.as_ref()?.get("disposition")?;
        Disposition::deserialize(data).ok()
    }
}

/// A dialog or a notice an extension opened, under the id its answer carries.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize)]
pub struct UiRequest {
    /// Its id.
    pub id: String,
    /// What it is.
    #[serde(flatten)]
    pub method: UiMethod,
}

/// An extension's dialog or notice, by its `method`.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize)]
#[serde(tag = "method", rename_all = "camelCase")]
pub enum UiMethod {
    /// Choose one of `options`.
    Select {
        /// What is asked.
        title: String,
        /// The answers.
        #[serde(default)]
        options: Vec<String>,
        /// When pi gives up waiting, in milliseconds.
        #[serde(default)]
        timeout: Option<u64>,
    },
    /// Yes or no.
    Confirm {
        /// What is asked.
        title: String,
        /// More of it.
        #[serde(default)]
        message: Option<String>,
        /// When pi gives up waiting, in milliseconds.
        #[serde(default)]
        timeout: Option<u64>,
    },
    /// A line of text.
    Input {
        /// What is asked.
        title: String,
        /// What the empty field shows.
        #[serde(default)]
        placeholder: Option<String>,
        /// When pi gives up waiting, in milliseconds.
        #[serde(default)]
        timeout: Option<u64>,
    },
    /// Lines of text.
    Editor {
        /// What is asked.
        title: String,
        /// The text to start from.
        #[serde(default)]
        prefill: Option<String>,
        /// When pi gives up waiting, in milliseconds.
        #[serde(default)]
        timeout: Option<u64>,
    },
    /// A notice; nothing answers it.
    Notify {
        /// What it says.
        message: String,
        /// `info`, `warning` or `error`; `info` when it is not given.
        #[serde(default, rename = "notifyType")]
        notify_type: Option<String>,
    },
    /// Something not asked of the person (a status, a widget, a title), or a method this does
    /// not know.
    #[serde(other)]
    Other,
}

/// The gate asking about a tool call: a [`UiMethod::Select`] whose title is the gate's JSON.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize)]
pub struct GateAsk {
    /// The gate's protocol, [`GATE_PROTOCOL`].
    pub gate: String,
    /// The call's id, as the assistant's message and the tool events have it.
    pub call: String,
    /// The call that made it, for a call another tool made.
    #[serde(default)]
    pub parent: Option<String>,
    /// Its tool.
    pub tool: String,
    /// Its arguments.
    #[serde(default)]
    pub input: Value,
    /// What the tool says of itself: `readOnlyHint`, `destructiveHint` and the like.
    #[serde(default)]
    pub hints: Value,
}

impl UiRequest {
    /// The gate's ask, when this is one.
    #[must_use]
    pub fn gate(&self) -> Option<GateAsk> {
        let UiMethod::Select { title, .. } = &self.method else { return None };
        serde_json::from_str::<GateAsk>(title).ok().filter(|ask| ask.gate == GATE_PROTOCOL)
    }
}

/// The answer that lets gate ask `ask` run.
#[must_use]
pub fn allow(ask: &str) -> Request {
    answer(ask, "allow".to_owned())
}

/// The answer that blocks gate ask `ask`, telling the model `reason` when there is one.
#[must_use]
pub fn deny(ask: &str, reason: Option<&str>) -> Request {
    let value = match reason.map(str::trim).filter(|r| !r.is_empty()) {
        Some(reason) => format!("deny\n{reason}"),
        None => "deny".to_owned(),
    };
    answer(ask, value)
}

/// The answer `value` to dialog `ask`: the option chosen, or the text written.
#[must_use]
pub fn answer(ask: &str, value: String) -> Request {
    Request {
        id: Some(ask.to_owned()),
        command: Command::ExtensionUiResponse {
            value: Some(value),
            confirmed: None,
            cancelled: None,
        },
    }
}

/// The answer to confirmation `ask`.
#[must_use]
pub fn confirm(ask: &str, confirmed: bool) -> Request {
    Request {
        id: Some(ask.to_owned()),
        command: Command::ExtensionUiResponse {
            value: None,
            confirmed: Some(confirmed),
            cancelled: None,
        },
    }
}

/// A message, by its `role`.
#[derive(Clone, PartialEq, Debug, Deserialize)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum Message {
    /// The person's.
    User {
        /// What it says.
        content: UserContent,
    },
    /// The model's.
    Assistant {
        /// Its blocks.
        #[serde(default)]
        content: Vec<Content>,
        /// The model that wrote it.
        #[serde(default)]
        model: Option<String>,
        /// Its provider.
        #[serde(default)]
        provider: Option<String>,
        /// What it cost.
        #[serde(default)]
        usage: Option<Usage>,
        /// Why it ended: `stop`, `toolUse`, `length`, `error`, `aborted`, or `pending` while it
        /// streams.
        #[serde(default, rename = "stopReason")]
        stop_reason: Option<String>,
        /// What went wrong, when it did.
        #[serde(default, rename = "errorMessage")]
        error_message: Option<String>,
    },
    /// A tool call's result.
    ToolResult {
        /// The call.
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        /// Its tool.
        #[serde(rename = "toolName")]
        tool_name: String,
        /// What it gave.
        #[serde(default)]
        content: Vec<Content>,
        /// Whether it failed.
        #[serde(default, rename = "isError")]
        is_error: bool,
    },
    /// The prompt and tools pi declares, or a role this does not know.
    #[serde(other)]
    Other,
}

/// What the person's message says: one string, or blocks.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    /// Plain text.
    Text(String),
    /// Blocks.
    Blocks(Vec<Content>),
}

impl UserContent {
    /// Its text, the blocks' joined.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Blocks(blocks) => text_of(blocks),
        }
    }
}

/// The text of `blocks`, joined by blank lines.
#[must_use]
pub fn text_of(blocks: &[Content]) -> String {
    let texts: Vec<&str> = blocks
        .iter()
        .filter_map(|block| match block {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    texts.join("\n\n")
}

/// A content block, by its `type`.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Content {
    /// Text.
    Text {
        /// It.
        text: String,
    },
    /// The model's thinking.
    Thinking {
        /// It, empty when redacted.
        #[serde(default)]
        thinking: String,
    },
    /// A picture.
    Image {
        /// Base64.
        data: String,
        /// Its media type.
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    /// A tool call.
    ToolCall {
        /// Its id.
        id: String,
        /// Its tool.
        name: String,
        /// Its arguments.
        #[serde(default)]
        arguments: Value,
    },
    /// A block this does not know.
    #[serde(other)]
    Other,
}

/// A change to the assistant's message as it streams, by its `type`. Deltas append to block
/// `content_index`; an end replaces the block with its whole content.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantEvent {
    /// A text block began.
    TextStart {
        /// The block.
        #[serde(rename = "contentIndex")]
        content_index: u32,
    },
    /// Text to append.
    TextDelta {
        /// The block.
        #[serde(rename = "contentIndex")]
        content_index: u32,
        /// The text.
        delta: String,
    },
    /// A text block is whole.
    TextEnd {
        /// The block.
        #[serde(rename = "contentIndex")]
        content_index: u32,
        /// All of it.
        content: String,
    },
    /// A thinking block began.
    ThinkingStart {
        /// The block.
        #[serde(rename = "contentIndex")]
        content_index: u32,
    },
    /// Thinking to append.
    ThinkingDelta {
        /// The block.
        #[serde(rename = "contentIndex")]
        content_index: u32,
        /// The text.
        delta: String,
    },
    /// A thinking block is whole.
    ThinkingEnd {
        /// The block.
        #[serde(rename = "contentIndex")]
        content_index: u32,
        /// All of it.
        content: String,
    },
    /// A tool call began.
    ToolcallStart {
        /// The block.
        #[serde(rename = "contentIndex")]
        content_index: u32,
        /// The call's id.
        id: String,
        /// Its tool.
        #[serde(rename = "toolName")]
        tool_name: String,
    },
    /// Arguments to append, as JSON text.
    ToolcallDelta {
        /// The block.
        #[serde(rename = "contentIndex")]
        content_index: u32,
        /// The text.
        delta: String,
    },
    /// A tool call is whole.
    ToolcallEnd {
        /// The block.
        #[serde(rename = "contentIndex")]
        content_index: u32,
        /// The call.
        #[serde(rename = "toolCall")]
        tool_call: Content,
    },
    /// An event this does not know, or one the loop turns into a message's start or end.
    #[serde(other)]
    Other,
}

/// A tool's result, partial or whole.
#[derive(Clone, PartialEq, Eq, Debug, Default, Deserialize)]
pub struct ToolOutput {
    /// What the model is given.
    #[serde(default)]
    pub content: Vec<Content>,
    /// The tool's structured result, when it declares one.
    #[serde(default, rename = "structuredContent")]
    pub structured: Option<Value>,
}

/// Tokens and cost of a response.
#[derive(Clone, Copy, PartialEq, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// Input tokens.
    #[serde(default)]
    pub input: u64,
    /// Output tokens.
    #[serde(default)]
    pub output: u64,
    /// Input read from the cache.
    #[serde(default)]
    pub cache_read: u64,
    /// Input written to the cache.
    #[serde(default)]
    pub cache_write: u64,
    /// Reasoning tokens, when the provider says.
    #[serde(default)]
    pub reasoning: Option<u64>,
    /// All of them.
    #[serde(default)]
    pub total_tokens: u64,
    /// What they cost, in dollars.
    #[serde(default)]
    pub cost: Cost,
}

/// What a compaction came to.
#[derive(Clone, PartialEq, Eq, Debug, Default, Deserialize)]
pub struct Compacted {
    /// The summary the session goes on from.
    #[serde(default)]
    pub summary: Option<String>,
    /// Context tokens before.
    #[serde(default, rename = "tokensBefore")]
    pub tokens_before: Option<u64>,
    /// Context tokens after, as pi estimates them.
    #[serde(default, rename = "estimatedTokensAfter")]
    pub estimated_tokens_after: Option<u64>,
}

/// What a response cost, in dollars.
#[derive(Clone, Copy, PartialEq, Debug, Default, Deserialize)]
pub struct Cost {
    /// All of it.
    #[serde(default)]
    pub total: f64,
}

/// A model, as `get_state` and `get_available_models` give it.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    /// Its id.
    pub id: String,
    /// Its name.
    #[serde(default)]
    pub name: Option<String>,
    /// Its provider.
    pub provider: String,
    /// Its context window, in tokens.
    #[serde(default)]
    pub context_window: Option<u64>,
    /// Whether it thinks.
    #[serde(default)]
    pub reasoning: bool,
}

/// `get_state`'s answer.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    /// The model in use.
    #[serde(default)]
    pub model: Option<Model>,
    /// How hard it thinks.
    #[serde(default)]
    pub thinking_level: Option<String>,
    /// Whether a run is under way.
    #[serde(default)]
    pub is_streaming: bool,
    /// The session's file; none for an ephemeral one.
    #[serde(default)]
    pub session_file: Option<String>,
    /// The session's id.
    pub session_id: String,
    /// Its name.
    #[serde(default)]
    pub session_name: Option<String>,
}

/// `get_session_stats`'s answer, as far as Slopty reads it.
#[derive(Clone, Copy, PartialEq, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    /// What the session cost, in dollars.
    #[serde(default)]
    pub cost: f64,
    /// The context window's use now.
    #[serde(default)]
    pub context_usage: Option<ContextUsage>,
}

/// The context window's use.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    /// Tokens in it; none just after a compaction.
    #[serde(default)]
    pub tokens: Option<u64>,
    /// Its size.
    pub context_window: u64,
}

/// `get_entries`' answer: the session's entries in the order they were appended.
#[derive(Clone, PartialEq, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entries {
    /// The entries.
    pub entries: Vec<Entry>,
    /// The entry the session is at.
    #[serde(default)]
    pub leaf_id: Option<String>,
}

/// A session entry.
#[derive(Clone, PartialEq, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Its id, a durable cursor.
    pub id: String,
    /// The entry before it on its branch.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// What it is: `message`, `model_change`, `compaction` and the like.
    #[serde(rename = "type")]
    pub kind: String,
    /// The message of a `message` entry.
    #[serde(default)]
    pub message: Option<Message>,
}
