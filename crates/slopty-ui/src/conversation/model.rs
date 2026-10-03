//! A followed conversation as this client holds it.
//!
//! It keeps the threads the worker streamed, the blocks the model is writing, the meters, the
//! texts expanded on request, what background commands printed, the pictures fetched (by
//! digest) and the messages sent from the composer that the transcript has not recorded yet.
//!
//! Nothing here draws. [`Model::apply`] takes the worker's events in order and says what
//! changed; [`crate::conversation::rows`] turns a thread into the list's rows.
//!
//! **A replay is swapped in whole.** Following a session (again, after the face was hidden)
//! and a new transcript (`/clear`, `/resume`) both start with [`Change::Reset`] of every
//! thread, then the conversation as it stands, then [`ConversationEvent::Current`]. The
//! replay is built aside and swapped in at `Current`. Entry ids are stable across reads, so
//! the rows before and after the swap diff to nothing where nothing changed, and the list keeps
//! the reader where they were: a toggle does not scroll the conversation.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use slopty_core::WallMs;
use slopty_proto::conversation::{
    Body, Change, Clipped, ConversationEvent, Entry, Image, Live, LiveId, LiveKind, Meters, Origin,
    Output, Part, SlashCommand, Task, TextRef, ThreadId, Turn,
};

/// One thread: its entries in order, its task list and, for a subagent, the call that
/// started it.
#[derive(Clone, Debug, Default)]
pub struct Thread {
    entries: Vec<Entry>,
    /// Each entry's position in `entries`, by id.
    index: HashMap<String, usize>,
    /// How many times each entry changed since it arrived: a row's revision.
    revs: HashMap<String, u64>,
    tasks: Vec<Task>,
    origin: Option<Origin>,
    /// What each turn took, by the prompt that opened it.
    turns: HashMap<String, Turn>,
}

impl Thread {
    /// The entries, oldest first.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The thread's task list, as its last `TaskCreate`/`TaskUpdate`/`TodoWrite` left it.
    #[must_use]
    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    /// For a subagent, the call that started it.
    #[must_use]
    pub const fn origin(&self) -> Option<&Origin> {
        self.origin.as_ref()
    }

    /// How many times the entry `id` changed since it arrived.
    #[must_use]
    pub fn rev(&self, id: &str) -> u64 {
        self.revs.get(id).copied().unwrap_or(0)
    }

    /// The entry `id`.
    #[must_use]
    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries.get(*self.index.get(id)?)
    }

    /// Where the entry `id` is in [`Self::entries`].
    #[must_use]
    pub fn position(&self, id: &str) -> Option<usize> {
        self.index.get(id).copied()
    }

    /// The figures of the turn the prompt `id` opened (empty for work before any prompt).
    #[must_use]
    pub fn turn(&self, prompt: &str) -> Option<&Turn> {
        self.turns.get(prompt)
    }

    /// The newest turn's figures: the one a working agent is on.
    #[must_use]
    pub fn last_turn(&self) -> Option<&Turn> {
        let prompt = self.entries.iter().rev().find_map(|e| match e.body {
            Body::Prompt(_) => Some(e.id.as_str()),
            _ => None,
        });
        self.turns.get(prompt.unwrap_or_default())
    }

    fn upsert(&mut self, entry: Entry) {
        match self.index.get(&entry.id).and_then(|&ix| self.entries.get_mut(ix)) {
            Some(old) if *old == entry => {}
            Some(old) => {
                let rev = self.revs.entry(entry.id.clone()).or_default();
                *rev = rev.saturating_add(1);
                *old = entry;
            }
            None => {
                self.index.insert(entry.id.clone(), self.entries.len());
                self.entries.push(entry);
            }
        }
    }

    fn remove(&mut self, id: &str) {
        self.turns.remove(id);
        if self.index.remove(id).is_none() {
            return;
        }
        self.revs.remove(id);
        self.entries.retain(|e| e.id != id);
        self.index = self.entries.iter().enumerate().map(|(ix, e)| (e.id.clone(), ix)).collect();
    }
}

/// A block the model is writing, not in the transcript yet.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LiveBlock {
    /// The thread it is shown at the end of.
    pub thread: ThreadId,
    /// Answer, thinking or a tool call's input.
    pub kind: LiveKind,
    /// What has arrived of it.
    pub text: String,
}

/// A message typed into the agent from the composer, until its prompt is in the transcript.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Pending {
    /// What was sent.
    pub text: String,
    /// It was sent while the agent worked: Claude Code holds it until the turn ends.
    pub queued: bool,
    /// When it was sent, in ms since the Unix epoch by this client's clock.
    pub sent_ms: WallMs,
}

/// What [`Model::apply`] changed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[expect(clippy::struct_excessive_bools, reason = "independent flags a caller reads apart")]
pub struct Applied {
    /// A thread's entries or tasks: the rows are built again.
    pub threads: bool,
    /// A live block started or went.
    pub live: bool,
    /// A live block grew.
    pub grew: bool,
    /// The meters.
    pub meters: bool,
    /// The conversation as it stands has just arrived in full, the first time or again.
    pub current: bool,
    /// A background command's output, or a picture's bytes: the rows that show them are
    /// drawn again.
    pub media: bool,
    /// The slash commands, or the paths an `@` query found: the composer's menu may show them.
    pub menu: bool,
}

/// A picture's bytes, as far as this client has them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Picture {
    /// Asked for; not here yet.
    Asked,
    /// Here.
    Here(Arc<[u8]>),
    /// The worker has none to send: gone from the transcript, or too large.
    Missing,
}

/// A text's place in [`Model`]'s map of expanded texts: its thread, record and part.
fn text_key(thread: &ThreadId, reference: &TextRef) -> String {
    let thread = match thread {
        ThreadId::Main => "main",
        ThreadId::Agent(id) => id.as_str(),
    };
    let part = match &reference.part {
        Part::Block { index } => format!("block:{index}"),
        Part::Input { tool_use_id, field } => format!("input:{tool_use_id}:{field}"),
        Part::Result { tool_use_id } => format!("result:{tool_use_id}"),
        Part::Stdout => "stdout".to_owned(),
        Part::Stderr => "stderr".to_owned(),
        Part::Patch => "patch".to_owned(),
        Part::Image { tool_use_id, index } => {
            format!("image:{}:{index}", tool_use_id.as_deref().unwrap_or_default())
        }
        Part::Output { tool_use_id } => format!("output:{tool_use_id}"),
    };
    format!("{thread}/{}/{part}", reference.record)
}

/// Whether `entry` is the prompt `pending` sent: its words, or its slash command and words.
fn records(entry: &Entry, pending: &Pending) -> bool {
    let Body::Prompt(prompt) = &entry.body else { return false };
    let sent = pending.text.trim();
    let recorded = prompt.text.text.trim();
    match &prompt.command {
        Some(command) if command == "!" => sent.strip_prefix('!').map(str::trim) == Some(recorded),
        Some(command) => {
            let (name, args) = sent.split_once(char::is_whitespace).unwrap_or((sent, ""));
            name == command && args.trim() == recorded
        }
        None => sent == recorded,
    }
}

/// A followed session's conversation.
#[derive(Debug, Default)]
pub struct Model {
    threads: BTreeMap<ThreadId, Thread>,
    /// A replay being built, swapped in at `Current`.
    replay: Option<BTreeMap<ThreadId, Thread>>,
    /// The conversation as it stood has arrived in full at least once.
    current: bool,
    meters: Option<Meters>,
    expanded: HashMap<String, Option<Clipped>>,
    live: BTreeMap<LiveId, LiveBlock>,
    pending: Vec<Pending>,
    /// What each background command printed last, by thread and call.
    outputs: HashMap<ThreadId, HashMap<String, Output>>,
    /// Pictures by digest.
    pictures: HashMap<String, Picture>,
    /// The digest each picture asked for was asked by, by its place.
    asked: HashMap<String, String>,
    /// The slash commands the agent takes.
    commands: Vec<SlashCommand>,
    /// The last `@` query answered, and the paths it found.
    found: Option<(String, Vec<String>)>,
}

impl Model {
    /// Take one event from the conversation stream.
    pub fn apply(&mut self, event: ConversationEvent) -> Applied {
        let mut applied = Applied::default();
        match event {
            ConversationEvent::Changes(changes) => {
                for change in changes {
                    self.change(change, &mut applied);
                }
            }
            ConversationEvent::Current => {
                if let Some(replay) = self.replay.take() {
                    self.threads = replay;
                }
                self.current = true;
                applied.threads = true;
                applied.current = true;
                self.drop_recorded();
                // A request on the stream before this one is not answered on this one.
                self.pictures.retain(|_, p| *p != Picture::Asked);
                self.asked.clear();
                // A new transcript's commands are new calls: the old outputs have no row.
                let threads = &self.threads;
                self.outputs.retain(|thread, calls| {
                    let Some(t) = threads.get(thread) else { return false };
                    calls.retain(|call, _| t.entry(call).is_some());
                    !calls.is_empty()
                });
            }
            ConversationEvent::Meters(meters) => {
                applied.meters = self.meters.as_ref() != Some(&meters);
                self.meters = Some(meters);
            }
            ConversationEvent::Expanded { thread, reference, text } => {
                self.expanded.insert(text_key(&thread, &reference), text);
                applied.threads = true;
            }
            ConversationEvent::Live(blocks) => {
                for block in blocks {
                    self.live_block(block, &mut applied);
                }
            }
            ConversationEvent::Output(outputs) => {
                for output in outputs {
                    let calls = self.outputs.entry(output.thread.clone()).or_default();
                    calls.insert(output.call.clone(), output);
                }
                applied.media = true;
            }
            ConversationEvent::Image { thread, reference, blob } => {
                let Some(digest) = self.asked.remove(&text_key(&thread, &reference)) else {
                    return applied;
                };
                let picture = match blob {
                    Some(blob) if blob.digest == digest => Picture::Here(Arc::from(blob.data)),
                    _ => Picture::Missing,
                };
                self.pictures.insert(digest, picture);
                applied.media = true;
            }
            ConversationEvent::Commands(commands) => {
                applied.menu = self.commands != commands;
                self.commands = commands;
            }
            ConversationEvent::Found { query, paths } => {
                self.found = Some((query, paths));
                applied.menu = true;
            }
        }
        applied
    }

    /// The slash commands the agent takes, as the worker last listed them.
    #[must_use]
    pub fn commands(&self) -> &[SlashCommand] {
        &self.commands
    }

    /// The paths the worker found for `query`, once it answered that query.
    #[must_use]
    pub fn found(&self, query: &str) -> Option<&[String]> {
        self.found.as_ref().filter(|(q, _)| q == query).map(|(_, paths)| paths.as_slice())
    }

    /// What the background command `call` of `thread` printed last.
    #[must_use]
    pub fn output(&self, thread: &ThreadId, call: &str) -> Option<&Output> {
        self.outputs.get(thread)?.get(call)
    }

    /// A picture's bytes, as far as they came.
    #[must_use]
    pub fn picture(&self, digest: &str) -> Option<&Picture> {
        self.pictures.get(digest)
    }

    /// Note that `image` of `thread` is being asked for; `false` when it is here, missing or
    /// asked already (by this place or another with the same digest).
    pub fn want(&mut self, thread: &ThreadId, image: &Image) -> bool {
        if self.pictures.contains_key(&image.digest) {
            return false;
        }
        self.pictures.insert(image.digest.clone(), Picture::Asked);
        self.asked.insert(text_key(thread, &image.at), image.digest.clone());
        true
    }

    fn change(&mut self, change: Change, applied: &mut Applied) {
        if change == (Change::Reset { thread: None }) {
            // The stream starts over: what follows up to `Current` is the whole conversation,
            // built aside so the one on screen stays until it is complete.
            self.replay = Some(BTreeMap::new());
            self.live.clear();
            applied.live = true;
            return;
        }
        let threads = self.replay.as_mut().unwrap_or(&mut self.threads);
        match change {
            Change::Upsert { thread, entry } => {
                threads.entry(thread).or_default().upsert(entry);
            }
            Change::Remove { thread, id } => {
                if let Some(t) = threads.get_mut(&thread) {
                    t.remove(&id);
                }
            }
            Change::Tasks { thread, tasks } => {
                threads.entry(thread).or_default().tasks = tasks;
            }
            Change::Turn { thread, turn } => {
                threads.entry(thread).or_default().turns.insert(turn.prompt.clone(), turn);
            }
            Change::Reset { thread: Some(thread) } => {
                threads.remove(&thread);
            }
            Change::Reset { thread: None } => {}
        }
        if self.replay.is_none() {
            applied.threads = true;
            self.drop_recorded();
        }
    }

    fn live_block(&mut self, block: Live, applied: &mut Applied) {
        match block {
            Live::Start { thread, id, kind } => {
                self.live.insert(id, LiveBlock { thread, kind, text: String::new() });
                applied.live = true;
            }
            Live::Append { id, text } => {
                if let Some(block) = self.live.get_mut(&id) {
                    block.text.push_str(&text);
                    applied.grew = true;
                }
            }
            Live::Clear { id } => {
                applied.live |= self.live.remove(&id).is_some();
            }
        }
    }

    /// Messages the transcript now records are no longer pending: the oldest pending one
    /// whose prompt arrived, and every one before it (Claude Code runs them in order).
    fn drop_recorded(&mut self) {
        let Some(main) = self.threads.get(&ThreadId::Main) else { return };
        let recorded = self.pending.iter().rposition(|pending| {
            main.entries.iter().rev().take(64).any(|entry| records(entry, pending))
        });
        if let Some(last) = recorded {
            self.pending.drain(..=last);
        }
    }

    /// A message the composer sent. `queued` when the agent was working.
    pub fn sent(&mut self, text: String, queued: bool, sent_ms: WallMs) {
        self.pending.push(Pending { text, queued, sent_ms });
    }

    /// Forget pending messages sent before `before_ms` that never showed up (a slash command
    /// Claude Code ran without recording a prompt). Returns whether any went.
    pub fn expire_pending(&mut self, before_ms: WallMs) -> bool {
        let was = self.pending.len();
        self.pending.retain(|p| p.sent_ms >= before_ms);
        was != self.pending.len()
    }

    /// Messages sent and not yet in the transcript, oldest first.
    #[must_use]
    pub fn pending(&self) -> &[Pending] {
        &self.pending
    }

    /// Whether the conversation as it stood has arrived in full.
    #[must_use]
    pub const fn is_current(&self) -> bool {
        self.current
    }

    /// A thread, if the worker sent anything of it.
    #[must_use]
    pub fn thread(&self, id: &ThreadId) -> Option<&Thread> {
        self.threads.get(id)
    }

    /// Every thread, the main one first, then subagents by id.
    pub fn threads(&self) -> impl Iterator<Item = (&ThreadId, &Thread)> {
        self.threads.iter()
    }

    /// The status line's meters, once the worker sent them.
    #[must_use]
    pub const fn meters(&self) -> Option<&Meters> {
        self.meters.as_ref()
    }

    /// What [`ConversationEvent::Expanded`] brought for a clipped text, once it came.
    #[must_use]
    pub fn expanded(&self, thread: &ThreadId, reference: &TextRef) -> Option<Expanded<'_>> {
        self.expanded
            .get(&text_key(thread, reference))
            .map(|text| text.as_ref().map_or(Expanded::Gone, Expanded::Whole))
    }

    /// The live blocks of `thread`, in the order they are shown.
    pub fn live(&self, thread: &ThreadId) -> impl Iterator<Item = (&LiveId, &LiveBlock)> {
        let thread = thread.clone();
        self.live.iter().filter(move |(_, b)| b.thread == thread)
    }

    /// One live block.
    #[must_use]
    pub fn live_block_at(&self, id: &LiveId) -> Option<&LiveBlock> {
        self.live.get(id)
    }

    /// Lines added and removed by every edit and write in every thread: the header's chip.
    #[must_use]
    pub fn changed_lines(&self) -> (u32, u32) {
        self.threads.values().flat_map(|t| &t.entries).fold((0, 0), |(a, r), entry| {
            let (da, dr) = crate::conversation::rows::entry_changes(entry);
            (a.saturating_add(da), r.saturating_add(dr))
        })
    }

    /// Who subagent `id` is: its description and its type, from its thread's origin, else
    /// from the `Agent` call that started it.
    #[must_use]
    pub fn subagent(&self, id: &str) -> (Option<String>, Option<String>) {
        let origin =
            self.threads.get(&ThreadId::Agent(id.to_owned())).and_then(|t| t.origin.as_ref());
        let call = self.threads.values().flat_map(|t| &t.entries).find_map(|e| match &e.body {
            Body::Tool(call) => match &call.detail {
                slopty_proto::conversation::ToolDetail::Agent(a)
                    if a.agent_id.as_deref() == Some(id) =>
                {
                    Some(a)
                }
                _ => None,
            },
            _ => None,
        });
        let description = origin
            .and_then(|o| o.description.clone())
            .or_else(|| call.and_then(|c| c.description.clone()));
        let kind = origin
            .and_then(|o| o.agent_type.clone())
            .or_else(|| call.and_then(|c| c.agent_type.clone()));
        (description, kind)
    }
}

/// What the worker sent for a clipped text asked for whole.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Expanded<'a> {
    /// All of it.
    Whole(&'a Clipped),
    /// The transcript no longer has it.
    Gone,
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::conversation::{Prompt, ResultStatus, ToolCall, ToolDetail, ToolResult};

    use super::*;

    fn clipped(text: &str) -> Clipped {
        Clipped { text: text.to_owned(), lines: 1, chars: 1, full: None }
    }

    fn prompt(id: &str, text: &str, command: Option<&str>) -> Entry {
        Entry {
            id: id.to_owned(),
            at_ms: WallMs::from_millis(1),
            body: Body::Prompt(Prompt {
                text: clipped(text),
                images: Vec::new(),
                command: command.map(str::to_owned),
            }),
        }
    }

    fn text(id: &str, words: &str) -> Entry {
        Entry { id: id.to_owned(), at_ms: WallMs::from_millis(2), body: Body::Text(clipped(words)) }
    }

    fn upsert(entry: Entry) -> Change {
        Change::Upsert { thread: ThreadId::Main, entry }
    }

    fn ids(model: &Model) -> Vec<String> {
        model
            .thread(&ThreadId::Main)
            .map(|t| t.entries().iter().map(|e| e.id.clone()).collect())
            .unwrap_or_default()
    }

    /// A replay (following again, a new transcript) is built aside: until `Current` the
    /// conversation on screen stays whole, and then the new one takes its place at once.
    #[test]
    fn a_replay_replaces_the_conversation_only_when_it_is_whole() {
        let mut model = Model::default();
        model.apply(ConversationEvent::Changes(vec![
            Change::Reset { thread: None },
            upsert(prompt("p1", "hello", None)),
        ]));
        assert!(ids(&model).is_empty(), "nothing shows before the first Current");
        model.apply(ConversationEvent::Current);
        assert_eq!(ids(&model), ["p1"]);

        let applied = model.apply(ConversationEvent::Changes(vec![
            Change::Reset { thread: None },
            upsert(prompt("p1", "hello", None)),
        ]));
        assert!(!applied.threads, "a replay in progress changes nothing on screen");
        assert_eq!(ids(&model), ["p1"], "the old conversation stays while the new one arrives");
        model.apply(ConversationEvent::Changes(vec![upsert(text("t1", "hi"))]));
        assert_eq!(ids(&model), ["p1"]);
        let applied = model.apply(ConversationEvent::Current);
        assert!(applied.threads && applied.current);
        assert_eq!(ids(&model), ["p1", "t1"]);
        assert_eq!(model.thread(&ThreadId::Main).map(|t| t.rev("p1")), Some(0), "unchanged");
    }

    /// An upsert replaces the entry in place and bumps its revision; one equal to what is
    /// held changes nothing. A removal closes the gap.
    #[test]
    fn an_upsert_replaces_in_place_and_a_removal_closes_the_gap() {
        let mut model = Model::default();
        model.apply(ConversationEvent::Current);
        let call = |result: Option<ToolResult>| Entry {
            id: "toolu_1".to_owned(),
            at_ms: WallMs::from_millis(3),
            body: Body::Tool(Box::new(ToolCall {
                name: "Bash".to_owned(),
                detail: ToolDetail::Other { input: clipped("{}") },
                result,
            })),
        };
        model.apply(ConversationEvent::Changes(vec![
            upsert(prompt("p1", "go", None)),
            upsert(call(None)),
            upsert(text("t1", "done")),
        ]));
        model.apply(ConversationEvent::Changes(vec![upsert(call(Some(ToolResult {
            status: ResultStatus::Ok,
            text: None,
            at_ms: WallMs::from_millis(4),
            images: Vec::new(),
        })))]));
        assert_eq!(ids(&model), ["p1", "toolu_1", "t1"], "the result lands on its call");
        let main = model.thread(&ThreadId::Main).unwrap();
        assert_eq!(main.rev("toolu_1"), 1);
        model.apply(ConversationEvent::Changes(vec![upsert(text("t1", "done"))]));
        assert_eq!(model.thread(&ThreadId::Main).unwrap().rev("t1"), 0, "the same text again");
        model.apply(ConversationEvent::Changes(vec![Change::Remove {
            thread: ThreadId::Main,
            id: "toolu_1".to_owned(),
        }]));
        assert_eq!(ids(&model), ["p1", "t1"]);
        assert_eq!(
            model.thread(&ThreadId::Main).unwrap().entry("t1").map(|e| e.at_ms),
            Some(WallMs::from_millis(2))
        );
    }

    /// A message sent from the composer shows until the transcript records its prompt:
    /// words as words, a slash command by its name and arguments, a shell line by its `!`.
    #[test]
    fn a_sent_message_waits_until_its_prompt_is_recorded() {
        let mut model = Model::default();
        model.apply(ConversationEvent::Current);
        model.sent("first".to_owned(), false, WallMs::from_millis(10));
        model.sent("/compact keep it short".to_owned(), true, WallMs::from_millis(11));
        model.sent("!ls".to_owned(), true, WallMs::from_millis(12));
        assert_eq!(model.pending().len(), 3);
        model.apply(ConversationEvent::Changes(vec![upsert(prompt("p1", "first", None))]));
        assert_eq!(model.pending().len(), 2);
        model.apply(ConversationEvent::Changes(vec![upsert(prompt(
            "p2",
            "keep it short",
            Some("/compact"),
        ))]));
        assert_eq!(model.pending().iter().map(|p| p.text.as_str()).collect::<Vec<_>>(), ["!ls"]);
        model.apply(ConversationEvent::Changes(vec![upsert(prompt("p3", "ls", Some("!")))]));
        assert_eq!(model.pending(), []);
        model.sent("/help".to_owned(), false, WallMs::from_millis(20));
        assert!(!model.expire_pending(WallMs::from_millis(20)), "not older than the bound");
        assert!(
            model.expire_pending(WallMs::from_millis(21)),
            "a command that never shows up goes"
        );
    }

    /// Live blocks start, grow and clear by id; a replay clears them all.
    #[test]
    fn live_blocks_grow_and_clear() {
        let mut model = Model::default();
        model.apply(ConversationEvent::Current);
        let id = LiveId { turn: "t".to_owned(), step: 0, block: 0 };
        model.apply(ConversationEvent::Live(vec![
            Live::Start { thread: ThreadId::Main, id: id.clone(), kind: LiveKind::Text },
            Live::Append { id: id.clone(), text: "Good ".to_owned() },
            Live::Append { id: id.clone(), text: "morning".to_owned() },
        ]));
        assert_eq!(model.live_block_at(&id).map(|b| b.text.as_str()), Some("Good morning"));
        assert_eq!(model.live(&ThreadId::Agent("a".to_owned())).count(), 0);
        model.apply(ConversationEvent::Live(vec![Live::Clear { id: id.clone() }]));
        assert!(model.live_block_at(&id).is_none());
        model.apply(ConversationEvent::Live(vec![Live::Start {
            thread: ThreadId::Main,
            id: id.clone(),
            kind: LiveKind::Thinking,
        }]));
        model.apply(ConversationEvent::Changes(vec![Change::Reset { thread: None }]));
        assert!(model.live_block_at(&id).is_none(), "a replay starts with nothing live");
    }

    /// An expanded text is kept by its thread and reference, and a text the transcript lost
    /// is remembered as lost.
    #[test]
    fn an_expanded_text_is_found_by_its_reference() {
        let mut model = Model::default();
        let reference = TextRef { record: "r".to_owned(), part: Part::Stdout };
        assert!(model.expanded(&ThreadId::Main, &reference).is_none());
        model.apply(ConversationEvent::Expanded {
            thread: ThreadId::Main,
            reference: reference.clone(),
            text: Some(clipped("all of it")),
        });
        assert_eq!(
            model.expanded(&ThreadId::Main, &reference),
            Some(Expanded::Whole(&clipped("all of it")))
        );
        assert!(model.expanded(&ThreadId::Agent("x".to_owned()), &reference).is_none());
        let gone = TextRef { record: "r".to_owned(), part: Part::Patch };
        model.apply(ConversationEvent::Expanded {
            thread: ThreadId::Main,
            reference: gone.clone(),
            text: None,
        });
        assert_eq!(model.expanded(&ThreadId::Main, &gone), Some(Expanded::Gone));
    }

    /// A picture is asked for once, whatever draws it again, and kept by its digest once its
    /// bytes come; a reply with other bytes, or none, marks it missing rather than asking on.
    #[test]
    fn a_picture_is_asked_for_once_and_kept_by_its_digest() {
        use slopty_proto::conversation::{Blob, Image};
        let image = |digest: &str, record: &str| Image {
            digest: digest.to_owned(),
            media_type: "image/png".to_owned(),
            bytes: 3,
            width: 1,
            height: 1,
            at: TextRef {
                record: record.to_owned(),
                part: Part::Image { tool_use_id: None, index: 0 },
            },
        };
        let mut model = Model::default();
        let shot = image("d1", "u1");
        assert!(model.want(&ThreadId::Main, &shot));
        assert!(!model.want(&ThreadId::Main, &shot), "asked already");
        assert!(!model.want(&ThreadId::Main, &image("d1", "u2")), "the same bytes elsewhere");
        assert!(matches!(model.picture("d1"), Some(Picture::Asked)));
        let applied = model.apply(ConversationEvent::Image {
            thread: ThreadId::Main,
            reference: shot.at.clone(),
            blob: Some(Blob { digest: "d1".to_owned(), data: vec![1, 2, 3] }),
        });
        assert!(applied.media);
        assert!(matches!(model.picture("d1"), Some(Picture::Here(b)) if b[..] == [1, 2, 3]));

        let other = image("d2", "u3");
        assert!(model.want(&ThreadId::Main, &other));
        model.apply(ConversationEvent::Image {
            thread: ThreadId::Main,
            reference: other.at.clone(),
            blob: Some(Blob { digest: "not d2".to_owned(), data: vec![9] }),
        });
        assert!(matches!(model.picture("d2"), Some(Picture::Missing)));
        let unasked = model.apply(ConversationEvent::Image {
            thread: ThreadId::Main,
            reference: image("d3", "u4").at,
            blob: None,
        });
        assert!(!unasked.media && model.picture("d3").is_none(), "a reply nobody asked for");
    }

    /// A background command's last lines are kept by its call, the newest over the last, and
    /// go with the call when a replay no longer has it.
    #[test]
    fn a_background_commands_lines_follow_its_call() {
        let dir = tempfile::tempdir().unwrap();
        let mut model = Model::default();
        for event in crate::conversation::fixtures::work(dir.path()) {
            model.apply(event);
        }
        let tail = model.output(&ThreadId::Main, "t4").map(|o| o.tail.text.clone());
        assert_eq!(
            tail.as_deref().and_then(|t| t.lines().last()),
            Some("   Compiling slopty v0.1.0")
        );
        model.apply(ConversationEvent::Changes(vec![Change::Reset { thread: None }]));
        model.apply(ConversationEvent::Current);
        assert!(model.output(&ThreadId::Main, "t4").is_none(), "its call is gone");
    }
}
