//! What Slopty's Claude Code mod ([`crate::claude_mod`]) tells the worker, and the live blocks
//! it makes: the answer, thinking and tool input as the model writes them, ahead of the
//! transcript.
//!
//! The mod posts batches ([`Batch`]) of events ([`ModEvent`]). An event this build does not
//! know is [`ModEvent::Other`]; one it knows whose fields moved is [`ModEvent::Malformed`]. A
//! session is heard only after its `hello` passes [`gate`], and [`admit`] keeps what the worker
//! makes of it, event by event ([`Trust`]): a Claude Code the mod was verified against is heard
//! as before, a newer release of the same line is heard provisionally, and the first event of a
//! known kind it sends in a shape this build cannot read drops the mod for that session.
//!
//! Two halves keep the blocks. The worker keeps one [`Board`] per session, fed by the events:
//! the blocks in flight and those that stopped a moment ago. Each observed thread keeps an
//! [`Overlay`]: what it has shown of the board, which [`Overlay::update`] brings up to date as
//! [`Live`] starts and appends, and which the transcript settles ([`Overlay::settle`]): a text
//! block when an answer with the same text is upserted in its thread, a thinking block when
//! thinking is, a tool block when the call with its id is. A block the transcript never
//! matched goes [`SETTLE_GRACE`] after its step stopped, or when the board lets it go.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;
use slopty_core::WallMs;

use crate::claude_mod::{MOD_CLAUDE_VERSIONS, MOD_PROTOCOL};
use crate::conversation::{Body, Change, Clipped, Live, LiveId, LiveKind, Meters, ThreadId};

/// How long after its step stopped a block the transcript has not settled is dropped.
pub const SETTLE_GRACE: Duration = Duration::from_secs(5);

/// How long the board keeps a stopped block, for a follower that comes late in the turn.
pub const KEEP_STOPPED: Duration = Duration::from_secs(30);

/// How long the board keeps a block that never stopped (the agent died mid-answer).
pub const KEEP_SILENT: Duration = Duration::from_secs(120);

/// How much older than the moment a follower first saw a block its entry may say it is, in
/// ms: the transcript's clock is Claude Code's, and it stamps a record when it writes it.
const ENTRY_SLACK: Duration = Duration::from_secs(2);

/// One request from the mod.
#[derive(Debug, Deserialize)]
pub struct Batch {
    /// `SLOPTY_SESSION`: the terminal session the agent runs in.
    #[serde(default)]
    pub session: Option<String>,
    /// The events, oldest first, undecoded.
    #[serde(default)]
    pub events: Vec<Value>,
}

impl Batch {
    /// The events, decoded.
    #[must_use]
    pub fn decoded(&self) -> Vec<ModEvent> {
        self.events.iter().map(ModEvent::decode).collect()
    }
}

/// One event from the mod.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "kind")]
pub enum ModEvent {
    /// `session.start`: the mod says who it is.
    #[serde(rename = "hello")]
    Hello(Hello),
    /// A model request starts.
    #[serde(rename = "step.start")]
    StepStart(Step),
    /// Answer text.
    #[serde(rename = "text")]
    Text {
        /// Where.
        #[serde(flatten)]
        at: At,
        /// The next piece.
        text: String,
    },
    /// Thinking.
    #[serde(rename = "thinking")]
    Thinking {
        /// Where.
        #[serde(flatten)]
        at: At,
        /// The next piece.
        text: String,
    },
    /// A tool call starts.
    #[serde(rename = "tool")]
    Tool {
        /// Where.
        #[serde(flatten)]
        at: At,
        /// Its `tool_use_id`.
        id: String,
        /// The tool.
        name: String,
    },
    /// More of a tool call's input JSON.
    #[serde(rename = "input")]
    Input {
        /// Where.
        #[serde(flatten)]
        at: At,
        /// The next piece.
        json: String,
    },
    /// The model request ended.
    #[serde(rename = "stop")]
    Stop(Step),
    /// The turn ended.
    #[serde(rename = "turn.complete")]
    TurnComplete {
        /// Which.
        #[serde(rename = "turnId")]
        turn: String,
    },
    /// The session's context.
    #[serde(rename = "measure")]
    Measure(Measure),
    /// Claude Code's own command list and model aliases.
    #[serde(rename = "catalog")]
    Catalog(Catalog),
    /// `session.end`.
    #[serde(rename = "bye")]
    Bye,
    /// An event of a kind this build reads (named), in a shape it cannot: the Claude Code under
    /// the mod moved a field the mod passes on.
    #[serde(skip)]
    Malformed(String),
    /// Anything else: an event of a kind this build does not use.
    #[serde(other)]
    Other,
}

impl ModEvent {
    /// Decode one event: [`ModEvent::Malformed`] when its kind is one this build reads and the
    /// rest does not decode, [`ModEvent::Other`] when it is no event this build reads.
    #[must_use]
    pub fn decode(value: &Value) -> Self {
        Self::deserialize(value).unwrap_or_else(|_shape| {
            match value.get("kind").and_then(Value::as_str) {
                Some(kind) => Self::Malformed(kind.to_owned()),
                None => Self::Other,
            }
        })
    }
}

/// The mod's `hello`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Hello {
    /// [`MOD_PROTOCOL`] of the mod that sent it.
    pub protocol: u32,
    /// Claude Code's version (`2.1.283`).
    pub claude: String,
    /// Claude Code's session id.
    #[serde(default, rename = "sessionId")]
    pub session_id: Option<String>,
}

/// A model request.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Step {
    /// The turn.
    #[serde(rename = "turnId")]
    pub turn: String,
    /// The request within the turn.
    #[serde(rename = "step")]
    pub index: u32,
    /// The subagent it belongs to; `None` for the session's own.
    #[serde(default, rename = "agentId")]
    pub agent: Option<String>,
}

impl Step {
    /// The thread it writes to.
    #[must_use]
    pub fn thread(&self) -> ThreadId {
        self.agent.clone().map_or(ThreadId::Main, ThreadId::Agent)
    }
}

/// A content block of a model request.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct At {
    /// The request.
    #[serde(flatten)]
    pub step: Step,
    /// The block within its answer.
    pub block: u32,
}

impl At {
    /// The block's id on the wire.
    #[must_use]
    pub fn id(&self) -> LiveId {
        LiveId { turn: self.step.turn.clone(), step: self.step.index, block: self.block }
    }
}

/// What Claude Code lists for the person: its slash commands (`$.command.list()`) and the
/// aliases `/model` takes (the `/config` menu's `model` row).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Catalog {
    /// Every command the person can run now, in the typeahead's order.
    #[serde(default)]
    pub commands: Vec<CommandInfo>,
    /// The model aliases, in the menu's order.
    #[serde(default)]
    pub models: Vec<String>,
    /// The alias in use.
    #[serde(default)]
    pub model: Option<String>,
}

/// One slash command as Claude Code lists it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct CommandInfo {
    /// What the person runs it by, without the slash.
    pub name: String,
    /// The typeahead's line for it.
    #[serde(default)]
    pub description: String,
    /// Where it comes from: `builtin`, `plugin`, `user` (the person's or the project's own
    /// file) or `mcp` (an MCP server's prompt).
    #[serde(default)]
    pub source: String,
}

/// `session.measure`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
pub struct Measure {
    /// The context window.
    #[serde(default)]
    pub context: Option<Context>,
}

/// The context window, as measured.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct Context {
    /// Share in use, 0 to 100.
    pub percent: f64,
    /// The window, in tokens.
    pub window: u64,
}

impl Measure {
    /// `meters` with what this measured: the context, sooner than the status line has it. The
    /// model and the rate limits stay the status line's.
    #[must_use]
    pub fn onto(&self, meters: Option<Meters>) -> Meters {
        let mut meters = meters.unwrap_or_default();
        if let Some(context) = self.context {
            meters.context_used_pct = Some(context.percent);
            meters.context_window = Some(context.window);
        }
        meters
    }
}

/// Why a `hello` was not trusted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A mod of another protocol.
    Protocol(u32),
    /// A Claude Code of a line the mod was not verified on.
    Version(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Protocol(p) => write!(f, "mod protocol {p}, not {MOD_PROTOCOL}"),
            Self::Version(v) => {
                write!(f, "Claude Code {v} is of no line the mod was verified on")
            }
        }
    }
}

/// How far the worker hears a session's mod.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// Its `hello` named a Claude Code the mod was verified against ([`MOD_CLAUDE_VERSIONS`]).
    Verified,
    /// Its `hello` named an unrecorded release of a verified line (`2.1.x`): heard while every
    /// event of a kind this build reads decodes.
    Provisional,
    /// Its `hello` was refused ([`Refusal`]); a later one may pass.
    Refused,
    /// It was provisional and sent an event this build could not read: not heard again in this
    /// session, whatever it says.
    Dropped,
}

impl Trust {
    /// Whether the mod's events are used.
    #[must_use]
    pub const fn heard(self) -> bool {
        matches!(self, Self::Verified | Self::Provisional)
    }
}

/// How trusted a `hello` makes the session's mod: this mod's protocol, on a Claude Code in
/// [`MOD_CLAUDE_VERSIONS`] ([`Trust::Verified`]) or of the same `major.minor` line as one
/// ([`Trust::Provisional`]).
///
/// # Errors
///
/// Why not.
pub fn gate(hello: &Hello) -> Result<Trust, Refusal> {
    if hello.protocol != MOD_PROTOCOL {
        return Err(Refusal::Protocol(hello.protocol));
    }
    if MOD_CLAUDE_VERSIONS.contains(&hello.claude.as_str()) {
        return Ok(Trust::Verified);
    }
    let line = |version: &str| {
        let mut parts = version.split('.');
        let (major, minor, patch) = (parts.next()?, parts.next()?, parts.next()?);
        let number = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        (number(major) && number(minor) && number(patch) && parts.next().is_none())
            .then(|| (major.to_owned(), minor.to_owned()))
    };
    match line(&hello.claude) {
        Some(theirs) if MOD_CLAUDE_VERSIONS.iter().any(|v| line(v).as_ref() == Some(&theirs)) => {
            Ok(Trust::Provisional)
        }
        _ => Err(Refusal::Version(hello.claude.clone())),
    }
}

/// What [`admit`] made of one event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admitted {
    /// Use it: the mod is heard.
    Use,
    /// Leave it: the mod is not heard (yet, or any more), or it says nothing to use.
    Skip,
    /// A `hello` made the mod heard, this far.
    Trusted(Trust),
    /// A `hello` was refused, for this reason; `again` when the mod was refused already.
    Refused {
        /// Why.
        why: Refusal,
        /// It was refused before.
        again: bool,
    },
    /// A provisional mod sent this kind of event in a shape this build cannot read, and is
    /// dropped for the session.
    Dropped(String),
}

/// Take `event` into the session's `trust` (`None` before any `hello`), and say what to do.
///
/// A worker falls back to the transcript, the hooks and the status line for a session
/// whose mod is never heard, says no `hello`, or is dropped.
pub fn admit(trust: &mut Option<Trust>, event: &ModEvent) -> Admitted {
    match event {
        ModEvent::Hello(_) if *trust == Some(Trust::Dropped) => Admitted::Skip,
        ModEvent::Hello(hello) => match gate(hello) {
            Ok(heard) => {
                *trust = Some(heard);
                Admitted::Trusted(heard)
            }
            Err(why) => {
                let again = *trust == Some(Trust::Refused);
                *trust = Some(Trust::Refused);
                Admitted::Refused { why, again }
            }
        },
        ModEvent::Malformed(kind) if *trust == Some(Trust::Provisional) => {
            *trust = Some(Trust::Dropped);
            Admitted::Dropped(kind.clone())
        }
        ModEvent::Malformed(_) | ModEvent::Other => Admitted::Skip,
        _ if trust.is_some_and(Trust::heard) => Admitted::Use,
        _ => Admitted::Skip,
    }
}

/// A block on the board.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// Its thread.
    pub thread: ThreadId,
    /// What it is.
    pub kind: LiveKind,
    /// All of it so far.
    pub text: String,
    /// When its step stopped.
    pub stopped: Option<Instant>,
    /// When it last grew.
    pub touched: Instant,
}

/// One session's blocks in flight, as the mod reports them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Board {
    blocks: BTreeMap<LiveId, Block>,
}

impl Board {
    /// The blocks.
    #[must_use]
    pub const fn blocks(&self) -> &BTreeMap<LiveId, Block> {
        &self.blocks
    }

    /// Take in one event, at `now`; whether the blocks changed.
    pub fn apply(&mut self, event: &ModEvent, now: Instant) -> bool {
        let changed = match event {
            ModEvent::Text { at, text } => self.grow(at, LiveKind::Text, text, now),
            ModEvent::Thinking { at, text } => self.grow(at, LiveKind::Thinking, text, now),
            ModEvent::Tool { at, id, name } => {
                let kind = LiveKind::Tool { id: id.clone(), name: name.clone() };
                self.grow(at, kind, "", now)
            }
            ModEvent::Input { at, json } => match self.blocks.get_mut(&at.id()) {
                Some(block) => {
                    block.text.push_str(json);
                    block.touched = now;
                    true
                }
                None => false,
            },
            ModEvent::Stop(step) => {
                self.stop(now, |id| (&id.turn, id.step) == (&step.turn, step.index))
            }
            ModEvent::TurnComplete { turn } => self.stop(now, |id| id.turn == *turn),
            ModEvent::Bye => {
                let had = !self.blocks.is_empty();
                self.blocks.clear();
                had
            }
            ModEvent::Hello(_)
            | ModEvent::StepStart(_)
            | ModEvent::Measure(_)
            | ModEvent::Catalog(_)
            | ModEvent::Malformed(_)
            | ModEvent::Other => false,
        };
        let before = self.blocks.len();
        self.blocks.retain(|_, block| match block.stopped {
            Some(at) => now.saturating_duration_since(at) < KEEP_STOPPED,
            None => now.saturating_duration_since(block.touched) < KEEP_SILENT,
        });
        changed || self.blocks.len() != before
    }

    fn grow(&mut self, at: &At, kind: LiveKind, text: &str, now: Instant) -> bool {
        let block = self.blocks.entry(at.id()).or_insert_with(|| Block {
            thread: at.step.thread(),
            kind,
            text: String::new(),
            stopped: None,
            touched: now,
        });
        block.text.push_str(text);
        block.touched = now;
        true
    }

    fn stop(&mut self, now: Instant, which: impl Fn(&LiveId) -> bool) -> bool {
        let mut changed = false;
        for (_, block) in self.blocks.iter_mut().filter(|(id, _)| which(id)) {
            if block.stopped.is_none() {
                block.stopped = Some(now);
                changed = true;
            }
        }
        changed
    }
}

/// A block as one follower was shown it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Shown {
    thread: ThreadId,
    kind: LiveKind,
    text: String,
    /// When this follower saw its step stop.
    stopped: Option<Instant>,
    /// When this follower first saw it: an entry stamped well before that is an older one.
    since_ms: WallMs,
}

/// What one follower has been shown of a session's board.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overlay {
    shown: BTreeMap<LiveId, Shown>,
    /// Blocks cleared while the board still has them: never shown again.
    settled: BTreeSet<LiveId>,
}

impl Overlay {
    /// Whether nothing is shown.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shown.is_empty()
    }

    /// What to send for this follower to see `board` as it is at `now` (`wall` by the wall
    /// clock): new blocks and what the others grew by, and a clear for each block the
    /// board let go. A block whose step stopped before this follower first saw it is left to
    /// the transcript.
    pub fn update(&mut self, board: &Board, now: Instant, wall: WallMs) -> Vec<Live> {
        let mut out = Vec::new();
        let gone: Vec<LiveId> =
            self.shown.keys().filter(|id| !board.blocks.contains_key(*id)).cloned().collect();
        for id in gone {
            self.shown.remove(&id);
            out.push(Live::Clear { id });
        }
        self.settled.retain(|id| board.blocks.contains_key(id));
        for (id, block) in &board.blocks {
            // A block that stopped before this follower saw it is the transcript's by now.
            let late = block.stopped.is_some() && !self.shown.contains_key(id);
            if late || self.settled.contains(id) {
                continue;
            }
            let shown = self.shown.entry(id.clone()).or_insert_with(|| {
                out.push(Live::Start {
                    thread: block.thread.clone(),
                    id: id.clone(),
                    kind: block.kind.clone(),
                });
                Shown {
                    thread: block.thread.clone(),
                    kind: block.kind.clone(),
                    text: String::new(),
                    stopped: None,
                    since_ms: wall,
                }
            });
            if let Some(more) = block.text.get(shown.text.len()..).filter(|more| !more.is_empty()) {
                out.push(Live::Append { id: id.clone(), text: more.to_owned() });
                shown.text.push_str(more);
            }
            if block.stopped.is_some() && shown.stopped.is_none() {
                shown.stopped = Some(now);
            }
        }
        out
    }

    /// Clear what the transcript's `changes` settle. Sent after those changes, so a client
    /// never shows a gap between the live block and its entry. Bring the overlay up to date
    /// first ([`Overlay::update`]), so a block is compared whole.
    pub fn settle(&mut self, changes: &[Change]) -> Vec<Live> {
        let mut cleared = Vec::new();
        for change in changes {
            let Change::Upsert { thread, entry } = change else { continue };
            let matched = self.shown.iter().find(|(_, shown)| {
                let fresh = entry.at_ms.is_zero()
                    || entry.at_ms.saturating_add(ENTRY_SLACK) >= shown.since_ms;
                shown.thread == *thread
                    && match (&shown.kind, &entry.body) {
                        (LiveKind::Text, Body::Text(text)) => fresh && same_text(&shown.text, text),
                        (LiveKind::Thinking, Body::Thinking(_)) => fresh,
                        (LiveKind::Tool { id, .. }, Body::Tool(_)) => *id == entry.id,
                        _ => false,
                    }
            });
            if let Some(id) = matched.map(|(id, _)| id.clone()) {
                self.shown.remove(&id);
                cleared.push(id);
            }
        }
        self.clear(cleared)
    }

    /// Clear the blocks whose step stopped [`SETTLE_GRACE`] ago with no entry to settle them.
    pub fn expire(&mut self, now: Instant) -> Vec<Live> {
        let late: Vec<LiveId> = self
            .shown
            .iter()
            .filter(|(_, s)| {
                s.stopped.is_some_and(|at| now.saturating_duration_since(at) >= SETTLE_GRACE)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &late {
            self.shown.remove(id);
        }
        self.clear(late)
    }

    /// Clear everything shown: the conversation started over.
    pub fn clear_all(&mut self) -> Vec<Live> {
        let all: Vec<LiveId> = std::mem::take(&mut self.shown).into_keys().collect();
        self.clear(all)
    }

    fn clear(&mut self, ids: Vec<LiveId>) -> Vec<Live> {
        ids.into_iter()
            .map(|id| {
                self.settled.insert(id.clone());
                Live::Clear { id }
            })
            .collect()
    }
}

/// Whether the transcript's answer is the live text: the answer begins with it, but for the
/// whitespace at its ends (the mod's last pieces may not have come yet), or, clipped, the live
/// text begins with its head.
fn same_text(live: &str, entry: &Clipped) -> bool {
    let live = live.trim();
    if live.is_empty() {
        return false;
    }
    if entry.is_clipped() {
        let head = entry.text.trim_end_matches('…').trim();
        !head.is_empty() && (live.starts_with(head) || head.starts_with(live))
    } else {
        entry.text.trim().starts_with(live)
    }
}

#[cfg(test)]
mod tests;
