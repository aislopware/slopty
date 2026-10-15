//! A thread as the shell's transcript: the person's messages as user entries, each turn's answer as
//! one assistant entry whose parts are its text, reasoning, tool calls and notices.

use slopty_proto::thread::{
    IntentId, ItemBody, Notice, Request, ThreadId, ThreadState, ToolCall, ToolDetail, ToolState,
    Turn, TurnId, TurnState, kind,
};
use slopty_view_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry, ToolDiffStat};
use slopty_view_proto::{UserInputAnswer, UserInputQuestion};

/// `state`'s transcript, its entries written on `device`.
///
/// `child_running` says whether a subagent's thread still runs, for its spawn chip; `named`
/// gives the id the shell gave a message it sent, by the intent that sent it.
#[must_use]
pub fn entries(
    state: &ThreadState,
    device: &str,
    child_running: impl Fn(ThreadId) -> Option<bool>,
    named: impl Fn(IntentId) -> Option<String>,
) -> Vec<SessionMessageEntry> {
    let mut out: Vec<SessionMessageEntry> = Vec::new();
    // The answer being gathered: its turn, and its entry.
    let mut answer: Option<(TurnId, SessionMessageEntry)> = None;
    for item in &state.items {
        let id = item.id.0.clone();
        let at = at(item.at_ms);
        if let ItemBody::User(message) = &item.body {
            // The id the shell gave the message, where it sent it, so its echo gives way.
            let id = message.intent.and_then(&named).unwrap_or(id);
            if let Some((_, entry)) = answer.take() {
                out.push(entry);
            }
            out.push(SessionMessageEntry {
                parts: vec![MessagePart::Text {
                    id: format!("{id}:text"),
                    text: message.text.text.clone(),
                }],
                id,
                role: MessageRole::User,
                created_at: at,
                device_id: device.to_owned(),
                status: None,
                continuation_of: None,
                duration_ms: None,
            });
            continue;
        }
        let Some(part) = part(state.meta.id, &id, &item.body, &child_running) else { continue };
        match &mut answer {
            Some((turn, entry)) if *turn == item.turn => entry.parts.push(part),
            _ => {
                if let Some((_, entry)) = answer.take() {
                    out.push(entry);
                }
                let turn = state.turns.iter().find(|t| t.id == item.turn);
                answer = Some((item.turn, assistant(&id, at, device, turn, part)));
            }
        }
    }
    if let Some((_, entry)) = answer {
        out.push(entry);
    }
    out
}

/// The first wall time an item may truly carry (2000-01-01): anything before it is a time its
/// agent never said, as a resumed session's history may come with none.
const FIRST_TIME_MS: u64 = 946_684_800_000;

/// An item's wall time as an entry's, in ms since the epoch; `0`, which the shell shows as no
/// time, for a time its agent never said.
#[must_use]
pub fn at(time: slopty_core::WallMs) -> i64 {
    let ms = time.as_millis();
    if ms < FIRST_TIME_MS {
        return 0;
    }
    i64::try_from(ms).unwrap_or(i64::MAX)
}

/// Answers still streaming in `entries` read as finished when their thread is not `live`.
///
/// A turn a stale cache, or an agent that went without ending it, left open is over, and its
/// tool groups stay closed instead of opening and then closing as the worker catches up.
pub fn settle(entries: &mut [SessionMessageEntry], live: bool) {
    if live {
        return;
    }
    for entry in entries {
        if entry.status == Some(MessageStatus::Streaming) {
            entry.status = Some(MessageStatus::Complete);
        }
    }
}

/// `state`'s open requests as the shell's question panel takes them.
///
/// They go on the answer the agent is giving: the last entry when it is the agent's, else an
/// entry of their own after the person's message. A request is asked where the person looks,
/// whatever it is: an approval, a question, a plan, Claude Code's own dialog at start.
pub fn asks(state: &ThreadState, device: &str, entries: &mut Vec<SessionMessageEntry>) {
    let parts: Vec<MessagePart> =
        state.requests.iter().filter(|request| request.is_open()).map(input).collect();
    let Some(first) = state.requests.iter().find(|request| request.is_open()) else { return };
    let at = at(first.opened_ms);
    if let Some(entry) = tail(entries, &format!("ask:{}", first.id.0), at, device) {
        entry.parts.extend(parts);
    }
}

/// `why` an intent was turned down, said at the end of the transcript.
pub fn refused(why: &str, at: i64, device: &str, entries: &mut Vec<SessionMessageEntry>) {
    let id = format!("refused:{at}");
    let part = MessagePart::Error { id: id.clone(), message: why.to_owned() };
    if let Some(entry) = tail(entries, &id, at, device) {
        entry.parts.push(part);
    }
}

/// The agent's last answer when the transcript ends on it, else a new one with id `id`; `None`
/// never, as the list is not empty after.
fn tail<'a>(
    entries: &'a mut Vec<SessionMessageEntry>,
    id: &str,
    at: i64,
    device: &str,
) -> Option<&'a mut SessionMessageEntry> {
    let ends_on_answer = entries.last().is_some_and(|entry| entry.role == MessageRole::Assistant);
    if !ends_on_answer {
        entries.push(SessionMessageEntry {
            id: id.to_owned(),
            role: MessageRole::Assistant,
            parts: Vec::new(),
            created_at: at,
            device_id: device.to_owned(),
            status: None,
            continuation_of: None,
            duration_ms: None,
        });
    }
    entries.last_mut()
}

/// The id of the one question of a request that offers its choices.
const CHOICE: &str = "choice";

/// What a request asks, as the panel heads it.
fn header(request: &Request) -> String {
    match request.kind.as_str() {
        Request::APPROVAL | Request::RETRY => "Approval",
        Request::PLAN => "Plan",
        Request::QUESTION => "Question",
        Request::ELICITATION => "Form",
        _ => "Input",
    }
    .to_owned()
}

/// A choice as the panel offers it: its label, and how far it reaches when it reaches beyond
/// this once.
fn choice_label(choice: &slopty_proto::thread::Choice) -> String {
    match &choice.scope {
        Some(scope) if !scope.trim().is_empty() => format!("{} ({scope})", choice.label),
        _ => choice.label.clone(),
    }
}

/// An open request as the shell's input part.
///
/// A request that offers choices and asks no questions (an approval, a plan, Claude Code's trust
/// dialog) takes one of them on a click, headed by its own title; one that offers none takes
/// words; an agent's questions keep their own headers and the words one may add.
fn input(request: &Request) -> MessagePart {
    let questions = if request.questions.is_empty() && !request.options.is_empty() {
        let title = request.title.trim();
        let text = request.text.as_ref().map(|text| text.text.trim()).unwrap_or_default();
        vec![UserInputQuestion {
            id: CHOICE.to_owned(),
            header: if title.is_empty() { header(request) } else { title.to_owned() },
            question: text.to_owned(),
            options: request.options.iter().map(choice_label).collect(),
            multi_select: false,
            prefill: None,
            multiline: false,
            choices_only: true,
        }]
    } else if request.questions.is_empty() {
        let mut question = request.title.clone();
        if let Some(text) = request.text.as_ref().filter(|text| !text.text.trim().is_empty()) {
            question = format!("{question}\n\n{}", text.text);
        }
        vec![UserInputQuestion {
            id: CHOICE.to_owned(),
            header: header(request),
            question,
            options: Vec::new(),
            multi_select: false,
            prefill: None,
            multiline: true,
            choices_only: false,
        }]
    } else {
        request
            .questions
            .iter()
            .enumerate()
            .map(|(ix, question)| UserInputQuestion {
                id: ix.to_string(),
                header: question.header.clone().unwrap_or_else(|| header(request)),
                question: question.text.clone(),
                options: question.options.iter().map(|o| o.label.clone()).collect(),
                multi_select: question.multi_select,
                prefill: None,
                multiline: false,
                choices_only: false,
            })
            .collect()
    };
    MessagePart::Input {
        id: format!("ask:{}", request.id.0),
        request_id: request.id.0.clone(),
        questions,
        resolved: false,
    }
}

/// The choice and words an [`Intent::Answer`](slopty_proto::thread::wire::Intent::Answer) to
/// `request` carries for the panel's `answers`; why not, when they answer nothing it offers.
///
/// A question's answers go as its agent takes them
/// ([`Answer::choice`](slopty_proto::thread::detail::Answer::choice)); any other request takes
/// the choice picked, or, for words of one's own, its plain deny with them as the reason, or the
/// words themselves where it offers nothing to pick.
pub fn answer(
    request: &Request,
    answers: &[UserInputAnswer],
) -> Result<(String, Option<String>), String> {
    use slopty_proto::thread::detail::Answer;
    if !request.questions.is_empty() {
        let given: Vec<Answer> = request
            .questions
            .iter()
            .enumerate()
            .filter_map(|(ix, question)| {
                let labels = &answers.iter().find(|a| a.question_id == ix.to_string())?.labels;
                Some(Answer { question: question.text.clone(), answer: labels.join(Answer::JOIN) })
            })
            .collect();
        if given.len() != request.questions.len() {
            return Err("Answer every question".to_owned());
        }
        return Ok((Answer::choice(&request.questions, &given), None));
    }
    let picked = answers
        .iter()
        .find(|a| a.question_id == CHOICE)
        .and_then(|a| a.labels.first())
        .map(|label| label.trim().to_owned())
        .filter(|label| !label.is_empty())
        .ok_or_else(|| "Pick an answer".to_owned())?;
    if let Some(choice) = request.options.iter().find(|c| choice_label(c) == picked) {
        return Ok((choice.id.clone(), None));
    }
    if request.options.is_empty() {
        return Ok((picked, None));
    }
    let deny = slopty_proto::thread::plain_deny(&request.options)
        .ok_or_else(|| format!("{picked} is not one of its answers"))?;
    Ok((deny.id.clone(), Some(picked)))
}

/// A turn's answer, begun with its first part.
fn assistant(
    id: &str,
    at: i64,
    device: &str,
    turn: Option<&Turn>,
    first: MessagePart,
) -> SessionMessageEntry {
    let mut parts = vec![first];
    let status = turn.map(|turn| match &turn.state {
        TurnState::Active => MessageStatus::Streaming,
        TurnState::Complete => MessageStatus::Complete,
        TurnState::Interrupted => MessageStatus::Aborted,
        TurnState::Failed { error, .. } => {
            parts.push(MessagePart::Error { id: format!("{id}:error"), message: error.clone() });
            MessageStatus::Complete
        }
    });
    let duration_ms = turn.and_then(|turn| {
        let ended = turn.ended_ms?;
        i64::try_from(ended.as_millis().saturating_sub(turn.started_ms.as_millis())).ok()
    });
    SessionMessageEntry {
        id: format!("answer:{id}"),
        role: MessageRole::Assistant,
        parts,
        created_at: at,
        device_id: device.to_owned(),
        status,
        continuation_of: None,
        duration_ms,
    }
}

/// An item of `thread`'s agent as a part of its answer; nothing for what the shell draws no
/// part of.
fn part(
    thread: ThreadId,
    id: &str,
    body: &ItemBody,
    child_running: &impl Fn(ThreadId) -> Option<bool>,
) -> Option<MessagePart> {
    let id = id.to_owned();
    Some(match body {
        ItemBody::Text(text) => MessagePart::Text { id, text: text.text.clone() },
        ItemBody::Reasoning(text) => MessagePart::Reasoning { id, text: text.text.clone() },
        ItemBody::Tool(call) => tool(thread, id, call, child_running),
        ItemBody::Notice(notice) => return notice_part(id, notice),
        ItemBody::User(_)
        | ItemBody::Compaction(_)
        | ItemBody::Review { .. }
        | ItemBody::Extra { .. } => return None,
    })
}

/// A notice as the shell draws it: a failure in red, what a command printed or the agent said
/// as words, and nothing for a stop, which the answer already says it was.
fn notice_part(id: String, notice: &Notice) -> Option<MessagePart> {
    let text = notice.text.text.clone();
    match notice.kind.as_str() {
        Notice::INTERRUPTED => None,
        Notice::INFO | Notice::COMMAND | Notice::REWOUND => Some(MessagePart::Text { id, text }),
        _ => Some(MessagePart::Error { id, message: text }),
    }
}

/// A tool call of `thread` as the shell's tool part. Its whole output, as the thread holds it,
/// is fetched by `{thread}/{item}` when the summary leaves some out.
fn tool(
    thread: ThreadId,
    id: String,
    call: &ToolCall,
    child_running: &impl Fn(ThreadId) -> Option<bool>,
) -> MessagePart {
    let input: Option<serde_json::Value> = serde_json::from_str(&call.input.text).ok();
    let text = |key: &str| {
        input
            .as_ref()
            .and_then(|i| i.get(key))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let mut diff_stats = None;
    let decoded = match (&call.detail, call.kind.as_str()) {
        (Some(ToolDetail::Exec(exec)), _) => {
            slopty_view_proto::ToolCall::Exec { command: exec.command.text.clone() }
        }
        (Some(ToolDetail::Read(read)), _) => {
            slopty_view_proto::ToolCall::ReadFile { path: read.path.clone() }
        }
        (Some(ToolDetail::Edit(edit)), _) => {
            diff_stats = Some(vec![ToolDiffStat {
                path: edit.path.clone(),
                additions: u64::from(edit.patch.added),
                deletions: u64::from(edit.patch.removed),
            }]);
            slopty_view_proto::ToolCall::EditFile {
                path: edit.path.clone(),
                old_string: text("old_string"),
                new_string: text("new_string"),
            }
        }
        (Some(ToolDetail::Write(write)), _) => {
            diff_stats = Some(vec![ToolDiffStat {
                path: write.path.clone(),
                additions: u64::from(write.patch.added),
                deletions: u64::from(write.patch.removed),
            }]);
            slopty_view_proto::ToolCall::WriteFile {
                path: write.path.clone(),
                content: text("content"),
            }
        }
        (Some(ToolDetail::Search(search)), _) => slopty_view_proto::ToolCall::Search {
            pattern: search.pattern.clone(),
            path: search.path.clone(),
        },
        (Some(ToolDetail::Fetch(fetch)), _) => slopty_view_proto::ToolCall::WebFetch {
            url: fetch.url.clone(),
            prompt: fetch.prompt.clone(),
        },
        (Some(ToolDetail::WebSearch(search)), _) => {
            slopty_view_proto::ToolCall::WebSearch { query: search.query.clone() }
        }
        (Some(ToolDetail::Mcp(mcp)), _) => slopty_view_proto::ToolCall::Mcp {
            server: mcp.server.clone(),
            tool: mcp.tool.clone(),
            input: input.clone(),
        },
        (Some(ToolDetail::Agent(agent)), _) => slopty_view_proto::ToolCall::Unknown {
            name: agent
                .description
                .as_ref()
                .map_or_else(|| "Agent".to_owned(), |about| format!("Agent: {about}")),
            input: input.clone(),
        },
        (_, kind::AGENT) => slopty_view_proto::ToolCall::Unknown {
            name: format!("Agent: {}", call.title),
            input: input.clone(),
        },
        _ => slopty_view_proto::ToolCall::Unknown { name: call.name.clone(), input: input.clone() },
    };
    let child = call.child.filter(|_| decoded.is_subagent_spawn());
    let subagent_status = child.map(|child| match child_running(child) {
        Some(true) => slopty_view_doc::SubagentStatus::Running,
        _ if call.state == ToolState::Failed => slopty_view_doc::SubagentStatus::Failed,
        _ => slopty_view_doc::SubagentStatus::Done,
    });
    let output =
        call.output.as_ref().and_then(|out| slopty_view_doc::summarize_tool_output(&out.text));
    let output_ref = call
        .output
        .as_ref()
        .filter(|out| output.as_deref() != Some(out.text.trim()))
        .map(|_| format!("{thread}/{id}"));
    MessagePart::Tool {
        is_error: matches!(call.state, ToolState::Failed | ToolState::Rejected),
        resolved: call.state.is_final(),
        output,
        diff: None,
        output_ref,
        id,
        call: decoded,
        output_bytes: call.output.as_ref().map(|out| u64::from(out.chars)),
        diff_ref: None,
        diff_stats,
        subagent_ref: child.map(|child| child.to_string()),
        subagent_status,
        subagent_tail: None,
    }
}
