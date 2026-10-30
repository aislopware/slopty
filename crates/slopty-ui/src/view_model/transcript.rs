//! A thread as the shell's transcript: the person's messages as user entries, each turn's answer as
//! one assistant entry whose parts are its text, reasoning, tool calls and notices.
//!
//! A text the worker cut reads whole once its whole has come ([`Look::whole`]), and says how much
//! is still to come until then. A picture goes by a path of its own ([`image_path`]), which the
//! interface reads back through the thread's link to it. So does the script of a visual a call
//! showed ([`visual_path`]), drawn natively after the call.
//!
//! Beside the turns go a few entries no one wrote: a compaction's row, its summary under it
//! ([`MessagePart::Note`]), a divider for a subagent's news ([`MessagePart::Marker`]), and the
//! words one agent told another, as from that agent. A failure
//! reads in one grammar: what happened and the one thing to do, the agent's own words kept behind
//! it ([`error_words`]).

use std::collections::HashMap;

use slopty_proto::thread::detail::{Clipped, Patch};
use slopty_proto::thread::{
    BackgroundTask, Compaction, ContentRef, Image, Item, ItemBody, Notice, TaskNote, ThreadId,
    ThreadState, ToolCall, ToolDetail, ToolState, Turn, TurnId, TurnState, kind,
};
use slopty_view_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry, ToolDiffStat};

pub use super::facts::ChildRun;
use super::facts::{note_row, page_card, task_part, tool_facts};

/// What a thread's transcript is read with beyond its state: the machine it is on, and what the
/// workspace knows of the threads around it, of the wholes of cut texts, and of what the shell
/// itself sent.
pub struct Look<'a> {
    device: &'a str,
    host: Option<&'a str>,
    child_run: Box<dyn Fn(ThreadId) -> Option<ChildRun> + 'a>,
    chat_of: Box<dyn Fn(ThreadId) -> String + 'a>,
    whole: Whole<'a>,
}

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
            host: None,
            child_run: Box::new(|_| None),
            chat_of: Box::new(|thread| thread.to_string()),
            whole: Box::new(|_| None),
        }
    }

    /// The name of the machine the thread runs on, which a failure there names.
    #[must_use]
    pub const fn host(mut self, name: &'a str) -> Self {
        self.host = Some(name);
        self
    }

    /// A subagent's run, for its spawn card, wherever it runs: whether it still runs, what it
    /// is doing, what it asks of the person.
    #[must_use]
    pub fn children(mut self, run: impl Fn(ThreadId) -> Option<ChildRun> + 'a) -> Self {
        self.child_run = Box::new(run);
        self
    }

    /// The id the shell knows a thread's chat by, where it is not the thread's own.
    #[must_use]
    pub fn chats(mut self, chat_of: impl Fn(ThreadId) -> String + 'a) -> Self {
        self.chat_of = Box::new(chat_of);
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

/// slopty: the file a call read, by its last name (its read's path, else `file_path` or `path`
/// in its input): what a picture it returned is called in the chat.
fn image_file_name(call: &ToolCall) -> Option<String> {
    let input: Option<serde_json::Value> = serde_json::from_str(&call.input.text).ok();
    let path = match &call.detail {
        Some(ToolDetail::Read(read)) => read.path.as_str(),
        _ => ["file_path", "path"].iter().find_map(|key| input.as_ref()?.get(*key)?.as_str())?,
    };
    let name = path.rsplit(['/', '\\']).next()?.trim();
    (!name.is_empty()).then(|| super::text::escaped(name))
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

/// Where the script of the visual shown by `thread`'s call `item` is read back from.
///
/// The shell reads its files beside it: `{stem}-assets.json` (their names) and
/// `{stem}-asset-{name}` (each one), `stem` being this path without `-visual.luau`
/// (`slopty_shell::visuals`).
#[must_use]
pub fn visual_path(thread: ThreadId, item: &str) -> String {
    format!("{VISUAL_SCHEME}{thread}/{item}{VISUAL_SCRIPT}")
}

/// How a path of [`visual_path`] begins.
pub const VISUAL_SCHEME: &str = "slopty-visual:";

/// How a visual's script path ends, as the shell reads it.
const VISUAL_SCRIPT: &str = "-visual.luau";

/// What a path beside a visual's script names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisualRead<'a> {
    /// The script.
    Script,
    /// The names of its files, as a JSON list.
    Assets,
    /// One of its files, by the name the script gives it.
    Asset(&'a str),
}

/// The thread, call and part of a visual a path of [`visual_path`] (or one beside it) names.
#[must_use]
pub fn visual_of(path: &str) -> Option<(ThreadId, &str, VisualRead<'_>)> {
    let (thread, rest) = path.strip_prefix(VISUAL_SCHEME)?.split_once('/')?;
    let thread = thread.parse().ok()?;
    let (item, read) = if let Some(item) = rest.strip_suffix(VISUAL_SCRIPT) {
        (item, VisualRead::Script)
    } else if let Some(item) = rest.strip_suffix("-assets.json") {
        (item, VisualRead::Assets)
    } else {
        let (item, name) = rest.split_once("-asset-")?;
        (item, VisualRead::Asset(name))
    };
    (!item.is_empty()).then_some((thread, item, read))
}

/// How the shell carries pictures with a message: a trailer of their paths after its words.
const IMAGES_TRAILER: &str = "Attached images (local files — open them to view):";

/// `text` with `thread`'s pictures of item `item` after it, as the shell reads them.
fn with_images(text: String, thread: ThreadId, item: &str, images: &[Image]) -> String {
    if images.is_empty() {
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

/// The entries being made: each with the turn it answers, and the answer being gathered.
struct Made {
    out: Vec<SessionMessageEntry>,
    /// The turn each entry of `out` answers, for an answer's own.
    turns: Vec<Option<TurnId>>,
    /// The answer being gathered: its turn, and its entry.
    answer: Option<(TurnId, SessionMessageEntry)>,
}

impl Made {
    /// The answer being gathered goes in.
    fn flush(&mut self) {
        if let Some((turn, entry)) = self.answer.take() {
            self.out.push(entry);
            self.turns.push(Some(turn));
        }
    }

    /// An entry of no turn's answer goes in, after the answer being gathered.
    fn own(&mut self, entry: SessionMessageEntry) {
        self.flush();
        self.out.push(entry);
        self.turns.push(None);
    }
}

/// `state`'s transcript, as `look` reads it.
#[must_use]
pub fn entries(state: &ThreadState, look: &Look<'_>) -> Vec<SessionMessageEntry> {
    let device = look.device;
    let thread = state.meta.id;
    let mut made = Made { out: Vec::new(), turns: Vec::new(), answer: None };
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
        match &item.body {
            ItemBody::User(message) => {
                // The person's words as the agent was given them, and its pictures.
                let words = marked(message.command.as_deref(), look.words(&message.text));
                let text = with_images(words, thread, &id, &message.images);
                // slopty: a plan sent to be implemented reads as the plan's own card under the
                // message, as the TUI boxes it ("Plan to implement").
                let plan = text
                    .strip_prefix(IMPLEMENT_PLAN)
                    .map(str::trim)
                    .filter(|plan| !plan.is_empty())
                    .map(str::to_owned);
                let text = if plan.is_some() {
                    IMPLEMENT_PLAN.trim_end_matches(':').to_owned()
                } else {
                    text
                };
                let parts = vec![MessagePart::Text { id: format!("{id}:text"), text }];
                made.own(own_entry(id.clone(), MessageRole::User, parts, at, device));
                if let Some(plan) = plan {
                    let part = MessagePart::Text { id: format!("plan:implement:{id}"), text: plan };
                    let mut entry = own_entry(
                        format!("plan-card:implement:{id}"),
                        MessageRole::User,
                        vec![part],
                        at,
                        device,
                    );
                    entry.status = Some(MessageStatus::Complete);
                    made.own(entry);
                }
            }
            // What Slopty handed the agent from the person and other agents: an agent's words
            // as from it, a child's news as a quiet line.
            ItemBody::Notice(notice) if notice.kind == Notice::REPORT => {
                for entry in report(&id, &look.words(&notice.text), at, device) {
                    made.own(entry);
                }
            }
            ItemBody::Compaction(compaction) => {
                // slopty: a quiet row whose summary opens under it.
                let note = slopty_view_doc::NoteRow {
                    kind: "compaction".to_owned(),
                    text: compacted(compaction),
                    tone: slopty_view_doc::NoteLine::INFO.to_owned(),
                    detail: compaction
                        .summary
                        .as_ref()
                        .map(|summary| look.words(summary))
                        .filter(|summary| !summary.trim().is_empty()),
                    ..slopty_view_doc::NoteRow::default()
                };
                let part = MessagePart::Note { id: id.clone(), note: Box::new(note) };
                made.own(own_entry(id.clone(), MessageRole::System, vec![part], at, device));
            }
            body => {
                let mut parts = Vec::new();
                if let ItemBody::Tool(call) = body
                    && let Some(ToolDetail::Tasks { steps, .. }) = &call.detail
                {
                    for step in steps.iter().filter(|step| !step.text.is_empty()) {
                        if let Some(task) = &step.id {
                            named.insert(task, &step.text);
                        }
                    }
                }
                // slopty: a message the agent sends the person (Claude Code's brief tools) is
                // its words, as the TUI draws it with no call around it.
                let brief = match body {
                    ItemBody::Tool(call) => brief_words(call),
                    _ => None,
                };
                // The TUI shows nothing of some calls: tool discovery, the agents list, and a
                // task list's every change but the last, which is the list as it stands.
                if let Some(text) = brief {
                    parts.push(MessagePart::Text { id: format!("{id}:brief"), text });
                } else if let ItemBody::Tool(call) = body
                    && !HIDDEN_TOOLS.contains(&call.name.as_str())
                    && (call.kind != kind::TASKS || Some(ix) == last_tasks)
                {
                    // The last task list is the thread's own as it stands, whole.
                    let plan = (Some(ix) == last_tasks).then_some(state.plan.as_ref()).flatten();
                    let child = call.child.and_then(|child| (look.child_run)(child));
                    let mut part = tool(thread, id.clone(), call, plan, &named, child.as_ref());
                    if let MessagePart::Tool { facts, .. } = &mut part {
                        *facts =
                            tool_facts(&id, item.at_ms, call, state, child.as_ref()).map(Box::new);
                        // slopty: its start is said from its turn's, which its row's tooltip says.
                        if let Some(time) = facts.as_mut().and_then(|facts| facts.time.as_mut()) {
                            time.turn_started = turn_start(state, item.turn);
                        }
                    }
                    parts.push(part);
                    // slopty: the files it changed, as the TUI lists them under a command.
                    parts.extend(shell_edits(thread, &id, call));
                    // slopty: a picture of a file read is named by the file; any other by its
                    // place among the call's pictures.
                    let file = image_file_name(call);
                    parts.extend(call.images.iter().enumerate().map(|(n, image)| {
                        let name = match &file {
                            Some(file) if call.images.len() == 1 => file.clone(),
                            _ => format!("Image {}", n.saturating_add(1)),
                        };
                        MessagePart::Image {
                            id: format!("{id}:image:{n}"),
                            path: image_path(thread, &id, n, image),
                            name,
                            mime_type: image.media_type.clone(),
                        }
                    }));
                    // A visual it showed, once its script passed the check: drawn natively after
                    // the call, as zeron folds its `visual` part into the turn.
                    if let Some(ToolDetail::Visual(visual)) = &call.detail
                        && call.state == ToolState::Completed
                    {
                        parts.push(MessagePart::Visual {
                            id: format!("{id}:visual"),
                            path: visual_path(thread, &id),
                            title: visual.title.clone(),
                        });
                    }
                    // A page it shows, from its first words on: the card shows what has come.
                    if let Some(card) = page_card(thread, &id, call, &state.meta.cwd) {
                        parts.push(MessagePart::Page { id: card.id.clone(), card: Box::new(card) });
                    }
                } else if let ItemBody::Task(note) = body
                    && !said_by_its_card(note, item, state)
                {
                    let words = |text: &Clipped| look.words(text);
                    let task = task_part(note, state, &words, &look.chat_of);
                    parts.push(MessagePart::Task { id: id.clone(), task: Box::new(task) });
                } else if let ItemBody::Notice(notice) = body
                    && let Some(note) =
                        note_row(notice, look.words(&notice.text), &id, item.turn, at, state)
                {
                    parts.push(MessagePart::Note { id: id.clone(), note: Box::new(note) });
                } else if let Some(mut part) = part(&id, body, look) {
                    if let MessagePart::Reasoning { thought_ms, .. } = &mut part {
                        *thought_ms = thought_for(state, ix);
                    }
                    parts.push(part);
                }
                for part in parts {
                    match &mut made.answer {
                        Some((turn, entry)) if *turn == item.turn => entry.parts.push(part),
                        _ => {
                            made.flush();
                            let turn = state.turns.iter().find(|t| t.id == item.turn);
                            made.answer = Some((item.turn, assistant(&id, at, device, turn, part)));
                        }
                    }
                }
            }
        }
    }
    made.flush();
    let Made { mut out, turns, .. } = made;
    // slopty: what the TUI folds of task notices, and each turn's figures on its prompt. Subagents
    // started together are the shell's to head with one line, live (`transcript/subagents.rs`).
    for entry in &mut out {
        if entry.role == MessageRole::Assistant {
            entry.parts = super::folds::fold_tasks(std::mem::take(&mut entry.parts));
        }
    }
    super::folds::turn_roots(state, &mut out, &turns);
    failures(state, look, &turns, &mut out);
    plan_part(state, last_tasks.is_some(), device, &mut out);
    out
}

/// slopty: the person's words as the TUI's row says them: a slash command (a skill run as one
/// too) as `/name args`, a shell command as `! command`.
fn marked(command: Option<&str>, words: String) -> String {
    let Some(command) = command.map(str::trim).filter(|c| !c.is_empty()) else { return words };
    let after = |mark: String| {
        if words.trim().is_empty() { mark } else { format!("{mark} {}", words.trim_start()) }
    };
    if command == "!" {
        return after("!".to_owned());
    }
    after(format!("/{}", command.trim_start_matches('/')))
}

/// An entry no turn's answer holds: the person's message, a divider, a card.
fn own_entry(
    id: String,
    role: MessageRole,
    parts: Vec<MessagePart>,
    at: i64,
    device: &str,
) -> SessionMessageEntry {
    SessionMessageEntry {
        id,
        role,
        parts,
        created_at: at,
        device_id: device.to_owned(),
        status: None,
        continuation_of: None,
        duration_ms: None,
    }
}

/// A compaction as its divider says it: how many tokens became the summary, when the agent said.
fn compacted(compaction: &Compaction) -> String {
    match compaction.before_tokens {
        Some(tokens) => {
            format!("Compacted · {} tokens → summary", slopty_view_doc::tokens_short(tokens))
        }
        None => "Compacted · context → summary".to_owned(),
    }
}

/// How a report's block names an agent's words: `Agent <name> (<thread>) says (an agent, not the
/// person):`, as `slopty-server`'s deliveries frame them.
const AGENT_SAYS: &str = " says (an agent, not the person):";

/// What Slopty handed an agent with a turn (its `<slopty-reports>` frame), as the person reads
/// it: each agent's words as a message from that agent (the shell names it in bold), the
/// person's own as theirs, and what came of other agents' work as one quiet line each. A report
/// whose frame does not read stays as the agent got it.
fn report(item: &str, text: &str, at: i64, device: &str) -> Vec<SessionMessageEntry> {
    let mut blocks: Vec<(String, Vec<&str>)> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("<slopty-reports") || trimmed == "</slopty-reports>" {
            continue;
        }
        if let Some(body) = line.strip_prefix("  ")
            && let Some((_, lines)) = blocks.last_mut()
        {
            lines.push(body);
            continue;
        }
        if trimmed.is_empty() || (trimmed.starts_with('(') && trimmed.ends_with("are read)")) {
            continue;
        }
        blocks.push((trimmed.to_owned(), Vec::new()));
    }
    if blocks.is_empty() {
        let part = MessagePart::Text { id: format!("{item}:report"), text: text.to_owned() };
        return vec![own_entry(
            format!("{item}:report"),
            MessageRole::User,
            vec![part],
            at,
            device,
        )];
    }
    let mut out = Vec::new();
    for (n, (head, lines)) in blocks.into_iter().enumerate() {
        let id = format!("{item}:report:{n}");
        let body = lines.join("\n");
        let entry = if head == "The person says:" {
            let part = MessagePart::Text { id: format!("{id}:text"), text: body };
            own_entry(id, MessageRole::User, vec![part], at, device)
        } else if let Some(who) =
            head.strip_prefix("Agent ").and_then(|rest| rest.strip_suffix(AGENT_SAYS))
        {
            let (name, thread) = match who.rsplit_once(" (") {
                Some((name, thread)) => (name, thread.trim_end_matches(')')),
                None => (who, who),
            };
            // zeron's attribution header, which its transcript shows as "Message from <name>".
            let text = format!(
                "[Message from Slopty chat {name} ({thread}). Reply to it with the Slopty \
                 `send_message` tool, chat {thread}.]\n\n{body}"
            );
            let part = MessagePart::Text { id: format!("{id}:text"), text };
            own_entry(id, MessageRole::User, vec![part], at, device)
        } else {
            let said = std::iter::once(head.as_str()).chain(lines).collect::<Vec<_>>().join(" ");
            let marker =
                MessagePart::Marker { id: id.clone(), text: super::text::line(&said, 200) };
            own_entry(id, MessageRole::System, vec![marker], at, device)
        };
        out.push(entry);
    }
    out
}

/// Each failed turn's error, at the end of its last answer, unless a notice there already says
/// it.
fn failures(
    state: &ThreadState,
    look: &Look<'_>,
    turns: &[Option<TurnId>],
    out: &mut [SessionMessageEntry],
) {
    for turn in &state.turns {
        let TurnState::Failed { error, .. } = &turn.state else { continue };
        let Some(last) = turns.iter().rposition(|t| *t == Some(turn.id)) else { continue };
        let says = |part: &MessagePart| match part {
            MessagePart::Error { message, detail, .. } => {
                detail.as_deref().unwrap_or(message).trim() == error.trim()
                    || message.trim() == error.trim()
            }
            _ => false,
        };
        let said = turns
            .iter()
            .zip(out.iter())
            .filter(|(t, _)| **t == Some(turn.id))
            .any(|(_, entry)| entry.parts.iter().any(says));
        if said {
            continue;
        }
        if let Some(entry) = out.get_mut(last) {
            let id = format!("{}:error", entry.id);
            let (message, detail) = error_words(TURN_FAILED, error, look.host, None);
            entry.parts.push(MessagePart::Error { id, message, detail });
        }
    }
}

/// The kind [`error_words`] gives a failed turn with no notice of its own.
const TURN_FAILED: &str = "turn-failed";

/// A failure of kind `kind` (a [`Notice`]'s, or `turn-failed`) in one grammar.
///
/// What happened, on which machine where one is named (`host`), then the one thing to do; and
/// the agent's own words (`raw`) kept behind it. A kind it does not know reads as the agent
/// said it.
#[must_use]
pub fn error_words(
    kind: &str,
    raw: &str,
    host: Option<&str>,
    retrying: Option<&slopty_proto::thread::Retry>,
) -> (String, Option<String>) {
    let on = host.map(|host| format!(" on {host}")).unwrap_or_default();
    // A Claude Code too old to drive keeps the worker's words, which the shell draws as a
    // banner naming the machine and both versions.
    if slopty_shell::transcript::claude_too_old(raw).is_some() {
        return (raw.trim().to_owned(), None);
    }
    // slopty: where the agent's own first line says what happened and what to do, in the TUI's
    // words (a spend limit, a sign-in its organization refuses, a start it refused, a context
    // that no longer fits), that line leads, its machine after it.
    if let Some(head) = agent_headline(raw).filter(|_| retrying.is_none()) {
        let words = match host {
            Some(host) => format!("{head} (on {host})"),
            None => head.to_owned(),
        };
        let raw = raw.trim();
        return (words, (raw != head).then(|| raw.to_owned()));
    }
    let words = match kind {
        Notice::API_ERROR => match retrying {
            Some(retry) => {
                let of = retry.max.map(|max| format!(" of {max}")).unwrap_or_default();
                format!(
                    "The model's service returned an error{on}. Trying again (attempt {}{of})…",
                    retry.attempt
                )
            }
            // slopty: a sign-in Claude Code lacks is the person's to fix where it runs; Slopty
            // never signs in for them.
            None if signed_out(raw) => {
                format!("The agent is not signed in{on}. Sign it in there, then try again.")
            }
            None => format!("The model's service returned an error{on}. Try again."),
        },
        Notice::LIMIT => {
            "The plan's usage limit was reached. Send a message to try again once it resets."
                .to_owned()
        }
        Notice::DECLINED => {
            "The agent's own safety check turned this step down. Say what you meant more \
             plainly, then send it again."
                .to_owned()
        }
        Notice::HOOK => format!("A hook stopped this step{on}. Fix the hook, then try again."),
        TURN_FAILED => format!("The turn failed{on}. Try again."),
        // slopty: an end of Claude Code's is said whole by the worker, what happened then what
        // to do; its machine goes after what happened, which "that machine" then names.
        Notice::ENDED => {
            let raw = raw.trim();
            let words = match (host, raw.rsplit_once(". ")) {
                (Some(host), Some((happened, then))) => format!("{happened} (on {host}). {then}"),
                (Some(host), None) => format!("{} (on {host}).", raw.trim_end_matches('.')),
                (None, _) => raw.to_owned(),
            };
            return (words, None);
        }
        _ => return (raw.to_owned(), None),
    };
    let raw = raw.trim();
    (words, (!raw.is_empty()).then(|| raw.to_owned()))
}

/// How Claude Code's own error lines begin where they say what happened and what to do, as its
/// TUI shows them: spend and usage limits, an organization's sign-in policies, a cloud
/// provider's credentials, a start it refused, a context that no longer fits.
const AGENT_HEADLINES: &[&str] = &[
    "Claude Code refused to start",
    "Context limit reached",
    "You've hit your",
    "You're out of usage credits",
    "Your org is out of usage",
    "Your seat type doesn't include",
    "This service is disabled for your org",
    "Your usage allocation has been disabled",
    "Your group's usage limit",
    "API Error: Usage credits required",
    "Your ANTHROPIC_API_KEY belongs to a disabled organization",
    "Your organization has disabled",
    "Could not refresh your login",
    "AWS credentials expired or invalid",
    "AWS authentication failed",
    "Google Cloud credentials expired or invalid",
    "Google Cloud authentication failed",
    "Microsoft Foundry authentication failed",
    "Gateway refused the request",
];

/// The agent's own first line, where it is one of [`AGENT_HEADLINES`].
fn agent_headline(raw: &str) -> Option<&str> {
    let first = raw.trim().lines().next()?.trim();
    AGENT_HEADLINES.iter().any(|head| first.starts_with(head)).then_some(first)
}

/// Whether an agent's error says it is not signed in, or its key or login is refused: Claude
/// Code's "Not logged in · Please run /login", "Invalid API key · …", "OAuth token revoked · …".
fn signed_out(raw: &str) -> bool {
    let lower = raw.to_lowercase();
    [
        "not logged in",
        "/login",
        "invalid api key",
        "oauth token",
        "login expired",
        "authentication_failed",
        "authentication required",
        "invalid auth token",
    ]
    .iter()
    .any(|said| lower.contains(said))
}

/// The agent's task list where no call in its transcript wrote it, as its task store holds it: a
/// list at the end of its last answer, which the shell's checklist reads.
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
        facts: None,
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

/// When turn `turn` of `state` began, in ms since the epoch, when its agent said.
fn turn_start(state: &ThreadState, turn: TurnId) -> Option<i64> {
    // Turns come in their order, so a search finds one at once; any other order still finds it.
    let ix = state
        .turns
        .binary_search_by_key(&turn, |t| t.id)
        .ok()
        .or_else(|| state.turns.iter().position(|t| t.id == turn))?;
    Some(at(state.turns.get(ix)?.started_ms)).filter(|ms| *ms != 0)
}

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

/// `why` an intent was turned down, said at the end of the transcript.
pub fn refused(why: &str, at: i64, device: &str, entries: &mut Vec<SessionMessageEntry>) {
    let id = format!("refused:{at}");
    let part = MessagePart::Error { id: id.clone(), message: why.to_owned(), detail: None };
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

/// The tools whose calls the TUI shows nothing of: discovering deferred tools, listing the
/// agents. A chip for each was noise.
const HIDDEN_TOOLS: [&str; 2] = ["ToolSearch", "ListAgents"];

/// How a message that hands the agent a plan to carry out begins: the one Slopty sends after a
/// plan approved with its context cleared, as Claude Code's TUI sends it.
pub const IMPLEMENT_PLAN: &str = "Implement the following plan:";

/// What a call of Claude Code's brief tools says to the person, as its words: the message, or
/// a file's caption, then the files it attached.
fn brief_words(call: &ToolCall) -> Option<String> {
    if !matches!(call.name.as_str(), "SendUserMessage" | "SendUserFile") {
        return None;
    }
    let input: serde_json::Value = serde_json::from_str(&call.input.text).ok()?;
    let said = ["message", "caption", "text"]
        .iter()
        .find_map(|key| input.get(*key)?.as_str())
        .map(str::trim)
        .unwrap_or_default();
    let files: Vec<String> = ["attachments", "files", "file_paths"]
        .iter()
        .filter_map(|key| input.get(*key)?.as_array())
        .flatten()
        .filter_map(|file| {
            file.as_str().or_else(|| file.get("path")?.as_str()).map(|p| format!("- `{p}`"))
        })
        .chain(
            input.get("file_path").and_then(serde_json::Value::as_str).map(|p| format!("- `{p}`")),
        )
        .collect();
    let mut text = said.to_owned();
    if !files.is_empty() {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&files.join("\n"));
    }
    (!text.trim().is_empty()).then_some(text)
}

/// Whether `body` is a tool call the transcript shows a chip for: tests pick their call by it.
#[cfg(test)]
pub(crate) fn shown_call(body: &ItemBody) -> bool {
    matches!(body, ItemBody::Tool(call)
        if !HIDDEN_TOOLS.contains(&call.name.as_str()) && call.kind != kind::TASKS)
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

/// How long the model thought before the reasoning at `ix` of `state`'s items was written, as
/// the TUI's "Thought for 12s": from the item before it in its turn (or the turn's start) to
/// it, once something follows it or its turn ended; `None` while it may still go on, or where
/// a time is unknown.
/// Whether `note` is a subagent's end that its own card already says: one that lands in the turn
/// of the call that started it, whose card shows its answer and how it ended. A subagent left
/// running that ends in a later turn keeps its notice, which says when it came back.
fn said_by_its_card(note: &TaskNote, item: &Item, state: &ThreadState) -> bool {
    note.kind == BackgroundTask::AGENT
        && note.call.as_ref().is_some_and(|call| {
            state.items.iter().any(|started| &started.id == call && started.turn == item.turn)
        })
}

fn thought_for(state: &ThreadState, ix: usize) -> Option<u64> {
    let item = state.items.get(ix)?;
    let turn = state.turns.iter().find(|turn| turn.id == item.turn)?;
    let followed = state.items.get(ix.saturating_add(1)).is_some_and(|next| next.turn == item.turn);
    if !followed && turn.state == TurnState::Active {
        return None;
    }
    let before = ix
        .checked_sub(1)
        .and_then(|prev| state.items.get(prev))
        .filter(|prev| prev.turn == item.turn)
        .map_or(turn.started_ms, |prev| prev.at_ms);
    let (from, to) = (before.as_millis(), item.at_ms.as_millis());
    (from > 0 && to > from).then(|| to.saturating_sub(from))
}

/// An item of the agent's other than a tool call as a part of its answer; nothing for what the
/// shell draws no part of.
fn part(id: &str, body: &ItemBody, look: &Look<'_>) -> Option<MessagePart> {
    let id = id.to_owned();
    Some(match body {
        ItemBody::Text(text) => MessagePart::Text { id, text: look.words(text) },
        ItemBody::Reasoning(text) => {
            MessagePart::Reasoning { id, text: look.words(text), thought_ms: None }
        }
        ItemBody::Notice(notice) => return notice_part(id, notice, look),
        // The person's messages, calls and compactions are entries or parts of their own.
        ItemBody::Tool(_)
        | ItemBody::User(_)
        | ItemBody::Compaction(_)
        | ItemBody::Review { .. }
        | ItemBody::Task(_)
        | ItemBody::Extra { .. } => return None,
    })
}

/// A notice as the shell draws it: a failure in red, in one grammar ([`error_words`]), what a
/// command printed or the agent said as words, and nothing for a stop, which the answer already
/// says it was.
fn notice_part(id: String, notice: &Notice, look: &Look<'_>) -> Option<MessagePart> {
    let text = look.words(&notice.text);
    match notice.kind.as_str() {
        Notice::INTERRUPTED => None,
        // A Claude Code too old to drive is said as a notice; the shell draws it as a banner.
        Notice::INFO if slopty_shell::transcript::claude_too_old(&text).is_some() => {
            Some(MessagePart::Error { id, message: text, detail: None })
        }
        Notice::INFO | Notice::COMMAND | Notice::REWOUND => Some(MessagePart::Text { id, text }),
        kind => {
            let (message, detail) = error_words(kind, &text, look.host, notice.retry.as_ref());
            Some(MessagePart::Error { id, message, detail })
        }
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
    child_run: Option<&ChildRun>,
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
            // A file written whole with no diff said (a new file's, before its result) added
            // every line it wrote, never "+0 −0".
            let unsaid = write.patch.added == 0 && write.patch.removed == 0;
            let added = if unsaid && write.created != Some(false) {
                write.lines
            } else {
                write.patch.added
            };
            diff_stats = Some(vec![ToolDiffStat {
                path: write.path.clone(),
                additions: u64::from(added),
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
    let subagent_status = child.map(|_| match child_run.map(|run| run.running) {
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
        facts: None,
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
    Some(sides(path, &lines))
}

/// slopty: the part id of file `n` a command changed, under its call `item`'s part: drawn as an
/// edit's chip, its diff fetched by `{thread}/{id}.diff` ([`shell_edit_of`]).
fn shell_edit_id(item: &str, n: usize) -> String {
    format!("{item}{SHELL_EDIT}{n}")
}

/// What marks a part as a file a command changed ([`shell_edit_id`]).
const SHELL_EDIT: &str = ":edit:";

/// The call and the index of the file a command changed that part `id` shows.
#[must_use]
pub fn shell_edit_of(id: &str) -> Option<(&str, usize)> {
    let (item, n) = id.rsplit_once(SHELL_EDIT)?;
    Some((item, n.parse().ok()?)).filter(|(item, _)| !item.is_empty())
}

/// slopty: the diff of a file a command changed, as an edit's is drawn; `whole` as for
/// [`tool_diff`]. `None` when it has no lines to show.
#[must_use]
pub fn shell_edit_diff(
    edit: &slopty_proto::thread::detail::ShellEdit,
    whole: Option<&str>,
) -> Option<slopty_view_proto::ToolDiff> {
    let lines: Vec<String> = match whole {
        Some(whole) => hunk_lines(whole),
        None => patch_lines(&edit.patch),
    };
    (!lines.is_empty()).then(|| sides(edit.path.clone(), &lines))
}

/// A diff's `lines` (each with its mark) as the before and after of the file at `path`.
fn sides(path: String, lines: &[String]) -> slopty_view_proto::ToolDiff {
    let side = |keep: char| {
        let mut out = String::new();
        for line in lines {
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
    slopty_view_proto::ToolDiff {
        path,
        old_text: (!old_text.is_empty()).then_some(old_text),
        new_text: side('+'),
    }
}

/// slopty: the files a command changed, each as an edit's chip after the command's own, and
/// what the agent says of the list (more files, a diff it could not take) as a quiet row: the
/// TUI's `bashEditDiff`, drawn by the parts the shell already draws for edits.
fn shell_edits(thread: ThreadId, item: &str, call: &ToolCall) -> Vec<MessagePart> {
    let Some(ToolDetail::Exec(slopty_proto::thread::detail::ExecDetail {
        edits: Some(edits), ..
    })) = &call.detail
    else {
        return Vec::new();
    };
    let mut parts: Vec<MessagePart> = edits
        .files
        .iter()
        .enumerate()
        .map(|(n, edit)| {
            let id = shell_edit_id(item, n);
            let decoded = if edit.created {
                slopty_view_proto::ToolCall::WriteFile { path: edit.path.clone(), content: None }
            } else {
                slopty_view_proto::ToolCall::EditFile {
                    path: edit.path.clone(),
                    old_string: None,
                    new_string: None,
                }
            };
            let has_lines = !edit.patch.hunks.is_empty() || edit.patch.full.is_some();
            MessagePart::Tool {
                diff_ref: has_lines.then(|| format!("{thread}/{id}.diff")),
                diff_stats: Some(vec![ToolDiffStat {
                    path: edit.path.clone(),
                    additions: u64::from(edit.patch.added),
                    deletions: u64::from(edit.patch.removed),
                }]),
                id,
                call: decoded,
                is_error: false,
                resolved: true,
                output: edit.deleted.then(|| "Deleted".to_owned()),
                diff: None,
                output_ref: None,
                output_bytes: None,
                subagent_ref: None,
                subagent_status: None,
                facts: None,
            }
        })
        .collect();
    let mut said = Vec::new();
    if edits.more > 0 {
        let files = if edits.more == 1 { "file" } else { "files" };
        said.push(format!("{} more {files} changed", edits.more));
    }
    if edits.unavailable {
        said.push("Couldn't take the full diff".to_owned());
    }
    if edits.skipped {
        said.push("A git step moved the working tree, so no diff was taken".to_owned());
    } else if edits.moves_tree {
        said.push(
            "A git step may have moved the working tree: a change listed may be its".to_owned(),
        );
    }
    if edits.shared {
        said.push(
            "Another command ran in this repository at the same time: a change may be its"
                .to_owned(),
        );
    }
    if let Some((first, rest)) = said.split_first() {
        let note = slopty_view_doc::NoteRow {
            kind: "edits".to_owned(),
            text: first.clone(),
            tone: slopty_view_doc::NoteLine::INFO.to_owned(),
            detail: (!rest.is_empty()).then(|| rest.join("\n")),
            ..slopty_view_doc::NoteRow::default()
        };
        parts.push(MessagePart::Note { id: format!("{item}:edits"), note: Box::new(note) });
    }
    parts
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
