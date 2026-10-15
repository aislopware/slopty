//! A thread as the shell's transcript: the person's messages as user entries, each turn's answer as
//! one assistant entry whose parts are its text, reasoning, tool calls and notices.
//!
//! A text the worker cut reads whole once its whole has come ([`Look::whole`]), and says how much
//! is still to come until then. A picture goes by a path of its own ([`image_path`]), which the
//! interface reads back through the thread's link to it.

use std::collections::HashMap;

use slopty_proto::thread::detail::{Clipped, Patch};
use slopty_proto::thread::{
    ContentRef, Image, IntentId, ItemBody, Notice, Request, ThreadId, ThreadState, ToolCall,
    ToolDetail, ToolState, Turn, TurnId, TurnState, kind,
};
use slopty_view_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry, ToolDiffStat};
use slopty_view_proto::{UserInputAnswer, UserInputQuestion};

/// What a thread's transcript is read with beyond its state: the machine it is on, and what the
/// workspace knows of the threads around it, of the wholes of cut texts, and of what the shell
/// itself sent.
pub struct Look<'a> {
    device: &'a str,
    child_running: Box<dyn Fn(ThreadId) -> Option<bool> + 'a>,
    named: ByIntent<'a>,
    said: ByIntent<'a>,
    whole: Whole<'a>,
}

/// Words the workspace keeps by the intent that sent them.
type ByIntent<'a> = Box<dyn Fn(IntentId) -> Option<String> + 'a>;

/// The whole of a cut text, once it came.
type Whole<'a> = Box<dyn Fn(&ContentRef) -> Option<String> + 'a>;

impl std::fmt::Debug for Look<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Look").field("device", &self.device).finish_non_exhaustive()
    }
}

impl<'a> Look<'a> {
    /// A thread on `device`, read with nothing else known.
    #[must_use]
    pub fn new(device: &'a str) -> Self {
        Self {
            device,
            child_running: Box::new(|_| None),
            named: Box::new(|_| None),
            said: Box::new(|_| None),
            whole: Box::new(|_| None),
        }
    }

    /// Whether a subagent's thread still runs, for its spawn chip, wherever it runs.
    #[must_use]
    pub fn children(mut self, running: impl Fn(ThreadId) -> Option<bool> + 'a) -> Self {
        self.child_running = Box::new(running);
        self
    }

    /// The id the shell gave a message it sent, by the intent that sent it, so its echo gives
    /// way to it.
    #[must_use]
    pub fn named(mut self, named: impl Fn(IntentId) -> Option<String> + 'a) -> Self {
        self.named = Box::new(named);
        self
    }

    /// The words the shell sent by an intent, its chips as the person wrote them, which the
    /// agent was given in its own words.
    #[must_use]
    pub fn said(mut self, said: impl Fn(IntentId) -> Option<String> + 'a) -> Self {
        self.said = Box::new(said);
        self
    }

    /// The whole of a cut text, once it came.
    #[must_use]
    pub fn whole(mut self, whole: impl Fn(&ContentRef) -> Option<String> + 'a) -> Self {
        self.whole = Box::new(whole);
        self
    }

    /// `text` as it reads: whole when it is, or its whole came; else what was kept and how
    /// much more there is.
    fn words(&self, text: &Clipped) -> String {
        let Some(full) = &text.full else { return text.text.clone() };
        if let Some(whole) = (self.whole)(full) {
            return whole;
        }
        let lines = u32::try_from(text.text.lines().count()).unwrap_or(u32::MAX);
        let chars = u32::try_from(text.text.chars().count()).unwrap_or(u32::MAX);
        let more = match text.lines.saturating_sub(lines) {
            0 => format!("… {} more characters", text.chars.saturating_sub(chars)),
            1 => "… 1 more line".to_owned(),
            n => format!("… {n} more lines"),
        };
        format!("{}\n\n{more}", text.text)
    }
}

/// The cut texts of `state` its transcript reads, whose wholes are worth asking for: the
/// person's messages, the agent's words, its reasoning and its notices.
pub fn cut(state: &ThreadState) -> impl Iterator<Item = &ContentRef> {
    state.items.iter().filter_map(|item| match &item.body {
        ItemBody::User(message) => message.text.full.as_ref(),
        ItemBody::Text(text) | ItemBody::Reasoning(text) => text.full.as_ref(),
        ItemBody::Notice(notice) => notice.text.full.as_ref(),
        _ => None,
    })
}

/// Where a picture of `thread`'s item `item` is read back from: the `index`th of its pictures,
/// named by its digest and kind.
#[must_use]
pub fn image_path(thread: ThreadId, item: &str, index: usize, image: &Image) -> String {
    let ext = image.media_type.rsplit('/').next().filter(|e| !e.is_empty()).unwrap_or("png");
    let digest: String = image.digest.chars().take(12).collect();
    format!("{IMAGE_SCHEME}{thread}/{item}/{index}/{digest}.{ext}")
}

/// How a path of [`image_path`] begins.
pub const IMAGE_SCHEME: &str = "slopty-image:";

/// The thread, item and index an [`image_path`] names.
#[must_use]
pub fn image_of(path: &str) -> Option<(ThreadId, &str, usize)> {
    let rest = path.strip_prefix(IMAGE_SCHEME)?;
    let mut parts = rest.split('/');
    let thread = parts.next()?.parse().ok()?;
    let item = parts.next()?;
    let index = parts.next()?.parse().ok()?;
    Some((thread, item, index))
}

/// How the shell carries pictures with a message: a trailer of their paths after its words.
const IMAGES_TRAILER: &str = "Attached images (local files — open them to view):";

/// `text` with `thread`'s pictures of item `item` after it, as the shell reads them.
fn with_images(text: String, thread: ThreadId, item: &str, images: &[Image]) -> String {
    if images.is_empty() || text.contains(IMAGES_TRAILER) {
        return text;
    }
    let paths: Vec<String> = images
        .iter()
        .enumerate()
        .map(|(ix, image)| format!("- {}", image_path(thread, item, ix, image)))
        .collect();
    let body = if text.trim().is_empty() { "Attached images".to_owned() } else { text };
    format!("{body}\n\n{IMAGES_TRAILER}\n{}", paths.join("\n"))
}

/// `state`'s transcript, as `look` reads it.
#[must_use]
pub fn entries(state: &ThreadState, look: &Look<'_>) -> Vec<SessionMessageEntry> {
    let device = look.device;
    let thread = state.meta.id;
    let mut out: Vec<SessionMessageEntry> = Vec::new();
    // The turn each entry of `out` answers, for an answer's own.
    let mut turns: Vec<Option<TurnId>> = Vec::new();
    // The answer being gathered: its turn, and its entry.
    let mut answer: Option<(TurnId, SessionMessageEntry)> = None;
    let last_tasks = state
        .items
        .iter()
        .rposition(|item| matches!(&item.body, ItemBody::Tool(call) if call.kind == kind::TASKS));
    // A task's text by its id, as a task call before named it: a change to a task names it by id
    // alone.
    let mut named: Named<'_> = HashMap::new();
    for (ix, item) in state.items.iter().enumerate() {
        let id = item.id.0.clone();
        let at = at(item.at_ms);
        if let ItemBody::User(message) = &item.body {
            // The id the shell gave the message, where it sent it, so its echo gives way.
            let shown = message.intent.and_then(&look.named).unwrap_or_else(|| id.clone());
            // The person's own words where the shell sent them, its chips and pictures as the
            // shell holds them; else what the agent was given, and its pictures.
            let text = message.intent.and_then(&look.said).unwrap_or_else(|| {
                with_images(look.words(&message.text), thread, &id, &message.images)
            });
            if let Some((turn, entry)) = answer.take() {
                out.push(entry);
                turns.push(Some(turn));
            }
            out.push(SessionMessageEntry {
                parts: vec![MessagePart::Text { id: format!("{shown}:text"), text }],
                id: shown,
                role: MessageRole::User,
                created_at: at,
                device_id: device.to_owned(),
                status: None,
                continuation_of: None,
                duration_ms: None,
            });
            turns.push(None);
            continue;
        }
        let mut parts = Vec::new();
        if let ItemBody::Tool(call) = &item.body {
            if let Some(ToolDetail::Tasks { steps, .. }) = &call.detail {
                for step in steps.iter().filter(|step| !step.text.is_empty()) {
                    if let Some(task) = &step.id {
                        named.insert(task, &step.text);
                    }
                }
            }
            // The last task list is the thread's own as it stands, whole.
            let plan = (Some(ix) == last_tasks).then_some(state.plan.as_ref()).flatten();
            parts.push(tool(thread, id.clone(), call, plan, &named, &look.child_running));
            parts.extend(call.images.iter().enumerate().map(|(n, image)| MessagePart::Image {
                id: format!("{id}:image:{n}"),
                path: image_path(thread, &id, n, image),
                name: format!("Image {}", n.saturating_add(1)),
                mime_type: image.media_type.clone(),
            }));
        } else if let Some(part) = part(&id, &item.body, look) {
            parts.push(part);
        }
        for part in parts {
            match &mut answer {
                Some((turn, entry)) if *turn == item.turn => entry.parts.push(part),
                _ => {
                    if let Some((turn, entry)) = answer.take() {
                        out.push(entry);
                        turns.push(Some(turn));
                    }
                    let turn = state.turns.iter().find(|t| t.id == item.turn);
                    answer = Some((item.turn, assistant(&id, at, device, turn, part)));
                }
            }
        }
    }
    if let Some((turn, entry)) = answer {
        out.push(entry);
        turns.push(Some(turn));
    }
    failures(state, &turns, &mut out);
    plan_part(state, last_tasks.is_some(), device, &mut out);
    out
}

/// Each failed turn's error, at the end of its last answer, unless a notice there already says
/// it.
fn failures(state: &ThreadState, turns: &[Option<TurnId>], out: &mut [SessionMessageEntry]) {
    for turn in &state.turns {
        let TurnState::Failed { error, .. } = &turn.state else { continue };
        let Some(last) = turns.iter().rposition(|t| *t == Some(turn.id)) else { continue };
        let said = turns
            .iter()
            .zip(out.iter())
            .filter(|(t, _)| **t == Some(turn.id))
            .flat_map(|(_, entry)| &entry.parts)
            .any(|part| matches!(part, MessagePart::Error { message, .. } if message.trim() == error.trim()));
        if said {
            continue;
        }
        if let Some(entry) = out.get_mut(last) {
            let id = format!("{}:error", entry.id);
            entry.parts.push(MessagePart::Error { id, message: error.clone() });
        }
    }
}

/// The agent's task list where no call of its wrote it (Codex's plan, an ACP agent's): a list
/// at the end of its last answer, which the shell's checklist reads.
fn plan_part(state: &ThreadState, written: bool, device: &str, out: &mut Vec<SessionMessageEntry>) {
    let Some(plan) = state.plan.as_ref().filter(|plan| !plan.steps.is_empty()) else { return };
    if written {
        return;
    }
    let part = MessagePart::Tool {
        id: format!("plan:{}", state.meta.id),
        call: slopty_view_proto::ToolCall::Todo { items: todo_items(&plan.steps, &Named::new()) },
        is_error: false,
        resolved: true,
        output: None,
        diff: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: None,
        subagent_ref: None,
        subagent_status: None,
        subagent_tail: None,
    };
    let at = state.items.last().map_or(0, |item| at(item.at_ms));
    if let Some(entry) = tail(out, &format!("plan:{}", state.meta.id), at, device) {
        entry.parts.push(part);
    }
}

/// A task's text by its id.
type Named<'a> = HashMap<&'a str, &'a str>;

/// A task list's steps as the shell's checklist, a step of no text of its own named by its id
/// (`named`).
fn todo_items(
    steps: &[slopty_proto::thread::Step],
    named: &Named<'_>,
) -> Vec<slopty_view_proto::TodoItem> {
    steps
        .iter()
        .map(|step| {
            let status = slopty_view_proto::TodoStatus::parse(&step.status);
            let text = match step.id.as_deref().and_then(|task| named.get(task)) {
                Some(text) if step.text.is_empty() => (*text).to_owned(),
                _ => step.text.clone(),
            };
            slopty_view_proto::TodoItem::new(text, status)
        })
        .collect()
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

/// A turn's answer, begun with its first part; a failed turn's error goes at its end
/// ([`failures`]).
fn assistant(
    id: &str,
    at: i64,
    device: &str,
    turn: Option<&Turn>,
    first: MessagePart,
) -> SessionMessageEntry {
    let status = turn.map(|turn| match &turn.state {
        TurnState::Active => MessageStatus::Streaming,
        TurnState::Complete | TurnState::Failed { .. } => MessageStatus::Complete,
        TurnState::Interrupted => MessageStatus::Aborted,
    });
    let duration_ms = turn.and_then(|turn| {
        let ended = turn.ended_ms?;
        i64::try_from(ended.as_millis().saturating_sub(turn.started_ms.as_millis())).ok()
    });
    SessionMessageEntry {
        id: format!("answer:{id}"),
        role: MessageRole::Assistant,
        parts: vec![first],
        created_at: at,
        device_id: device.to_owned(),
        status,
        continuation_of: None,
        duration_ms,
    }
}

/// An item of the agent's other than a tool call as a part of its answer; nothing for what the
/// shell draws no part of.
fn part(id: &str, body: &ItemBody, look: &Look<'_>) -> Option<MessagePart> {
    let id = id.to_owned();
    Some(match body {
        ItemBody::Text(text) => MessagePart::Text { id, text: look.words(text) },
        ItemBody::Reasoning(text) => MessagePart::Reasoning { id, text: look.words(text) },
        ItemBody::Notice(notice) => return notice_part(id, notice, look),
        ItemBody::Tool(_)
        | ItemBody::User(_)
        | ItemBody::Compaction(_)
        | ItemBody::Review { .. }
        | ItemBody::Extra { .. } => return None,
    })
}

/// A notice as the shell draws it: a failure in red, what a command printed or the agent said
/// as words, and nothing for a stop, which the answer already says it was.
fn notice_part(id: String, notice: &Notice, look: &Look<'_>) -> Option<MessagePart> {
    let text = look.words(&notice.text);
    match notice.kind.as_str() {
        Notice::INTERRUPTED => None,
        Notice::INFO | Notice::COMMAND | Notice::REWOUND => Some(MessagePart::Text { id, text }),
        _ => Some(MessagePart::Error { id, message: text }),
    }
}

/// A tool call of `thread` as the shell's tool part. Its whole output, as the thread holds it,
/// is fetched by `{thread}/{item}` when the summary leaves some out, and an edit's diff by
/// `{thread}/{item}.diff` ([`tool_diff`]), as zeron's engine names them. The thread's task list
/// as it stands (`plan`) is the last task call's checklist.
fn tool(
    thread: ThreadId,
    id: String,
    call: &ToolCall,
    plan: Option<&slopty_proto::thread::Plan>,
    named: &Named<'_>,
    child_running: &dyn Fn(ThreadId) -> Option<bool>,
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
        // A task list: the thread's own as it stands for the last call, else what the call
        // wrote.
        (Some(ToolDetail::Tasks { steps, .. }), _) => slopty_view_proto::ToolCall::Todo {
            items: todo_items(plan.map_or(steps.as_slice(), |plan| plan.steps.as_slice()), named),
        },
        (_, kind::TASKS) => slopty_view_proto::ToolCall::Todo {
            items: plan.map(|plan| todo_items(&plan.steps, named)).unwrap_or_default(),
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
        .filter(|out| out.is_clipped() || output.as_deref() != Some(out.text.trim()))
        .map(|_| format!("{thread}/{id}"));
    let diff_ref = diffable(call, input.as_ref()).then(|| format!("{thread}/{id}.diff"));
    MessagePart::Tool {
        is_error: matches!(call.state, ToolState::Failed | ToolState::Rejected),
        resolved: call.state.is_final(),
        output,
        diff: None,
        output_ref,
        id,
        call: decoded,
        output_bytes: call.output.as_ref().map(|out| u64::from(out.chars)),
        diff_ref,
        diff_stats,
        subagent_ref: child.map(|child| child.to_string()),
        subagent_status,
        subagent_tail: None,
    }
}

/// An edit's or a write's diff as the shell draws one: the lines its hunks cover, before and
/// after.
///
/// `whole` is the whole diff's text when its hunks were cut and it came. An edit with no hunks
/// shows what its call replaced, a write what it wrote. `None` for any other call, and for one
/// with nothing to show.
#[must_use]
pub fn tool_diff(call: &ToolCall, whole: Option<&str>) -> Option<slopty_view_proto::ToolDiff> {
    let input: Option<serde_json::Value> = serde_json::from_str(&call.input.text).ok();
    let text = |key: &str| {
        input
            .as_ref()
            .and_then(|i| i.get(key))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let (path, patch, fallback) = match &call.detail {
        Some(ToolDetail::Edit(edit)) => {
            let replaced = text("new_string").map(|new| (text("old_string"), new));
            (edit.path.clone(), &edit.patch, replaced)
        }
        Some(ToolDetail::Write(write)) => {
            (write.path.clone(), &write.patch, text("content").map(|new| (None, new)))
        }
        _ => return None,
    };
    let lines: Vec<String> = match whole {
        Some(whole) => hunk_lines(whole),
        None => patch_lines(patch),
    };
    if lines.is_empty() {
        let (old_text, new_text) = fallback?;
        return Some(slopty_view_proto::ToolDiff { path, old_text, new_text });
    }
    let side = |keep: char| {
        let mut out = String::new();
        for line in &lines {
            let mut chars = line.chars();
            let mark = chars.next();
            if mark == Some(' ') || mark == Some(keep) {
                out.push_str(chars.as_str());
                out.push('\n');
            }
        }
        out
    };
    let old_text = side('-');
    Some(slopty_view_proto::ToolDiff {
        path,
        old_text: (!old_text.is_empty()).then_some(old_text),
        new_text: side('+'),
    })
}

/// Whether [`tool_diff`] has something to show for `call`, without building it: an edit or a
/// write with hunks, or with what it replaced or wrote in its input.
fn diffable(call: &ToolCall, input: Option<&serde_json::Value>) -> bool {
    let given =
        |key: &str| input.and_then(|i| i.get(key)).is_some_and(serde_json::Value::is_string);
    match &call.detail {
        Some(ToolDetail::Edit(edit)) => !edit.patch.hunks.is_empty() || given("new_string"),
        Some(ToolDetail::Write(write)) => !write.patch.hunks.is_empty() || given("content"),
        _ => false,
    }
}

/// A patch's hunks' lines, each with its mark.
fn patch_lines(patch: &Patch) -> Vec<String> {
    patch.hunks.iter().flat_map(|hunk| hunk.lines.iter().cloned()).collect()
}

/// A unified diff's hunk lines, each with its mark: what follows each `@@` header, its file
/// headers and notes left out.
fn hunk_lines(diff: &str) -> Vec<String> {
    let mut in_hunk = false;
    let mut out = Vec::new();
    for line in diff.lines() {
        if line.starts_with("@@") {
            in_hunk = true;
            continue;
        }
        if line.starts_with("diff --git") {
            in_hunk = false;
            continue;
        }
        if in_hunk && line.starts_with([' ', '+', '-']) {
            out.push(line.to_owned());
        } else if in_hunk && line.is_empty() {
            out.push(" ".to_owned());
        }
    }
    out
}
