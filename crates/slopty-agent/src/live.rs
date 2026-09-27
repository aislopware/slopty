//! What Slopty's Claude Code mod ([`crate::claude_mod`]) tells the worker, and the live blocks
//! it makes: the answer, thinking and tool input as the model writes them, ahead of the
//! transcript.
//!
//! The mod posts batches ([`Batch`]) of events ([`ModEvent`]), which are decoded leniently: an
//! event this build does not know, or one whose fields moved, is [`ModEvent::Other`]. A session
//! is heard only after its `hello` passes [`gate`].
//!
//! Two halves keep the blocks. The worker keeps one [`Board`] per session, fed by the events:
//! the blocks in flight and those that stopped a moment ago. Each follower keeps an
//! [`Overlay`]: what it has shown of the board, which [`Overlay::update`] brings up to date as
//! [`Live`] starts and appends, and which the transcript settles ([`Overlay::settle`]): a text
//! block when an answer with the same text is upserted in its thread, a thinking block when
//! thinking is, a tool block when the call with its id is. A block the transcript never
//! matched goes [`SETTLE_GRACE`] after its step stopped, or when the board lets it go.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;
use slopty_proto::conversation::{Body, Change, Clipped, Live, LiveId, LiveKind, Meters, ThreadId};

use crate::claude_mod::{MOD_CLAUDE_VERSIONS, MOD_PROTOCOL};

/// How long after its step stopped a block the transcript has not settled is dropped.
pub const SETTLE_GRACE: Duration = Duration::from_secs(5);

/// How long the board keeps a stopped block, for a follower that comes late in the turn.
pub const KEEP_STOPPED: Duration = Duration::from_secs(30);

/// How long the board keeps a block that never stopped (the agent died mid-answer).
pub const KEEP_SILENT: Duration = Duration::from_secs(120);

/// How much older than the moment a follower first saw a block its entry may say it is, in
/// ms: the transcript's clock is Claude Code's, and it stamps a record when it writes it.
const ENTRY_SLACK_MS: u64 = 2_000;

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
    /// The session's context and cost.
    #[serde(rename = "measure")]
    Measure(Measure),
    /// `session.end`.
    #[serde(rename = "bye")]
    Bye,
    /// Anything else: an event this build does not use, or one whose shape it does not know.
    #[serde(other)]
    Other,
}

impl ModEvent {
    /// Decode one event; [`ModEvent::Other`] when it is not one this build reads.
    #[must_use]
    pub fn decode(value: &Value) -> Self {
        Self::deserialize(value).unwrap_or(Self::Other)
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

/// `session.measure`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
pub struct Measure {
    /// The context window.
    #[serde(default)]
    pub context: Option<Context>,
    /// The session's cost.
    #[serde(default)]
    pub cost: Option<Cost>,
}

/// The context window, as measured.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct Context {
    /// Share in use, 0 to 100.
    pub percent: f64,
    /// The window, in tokens.
    pub window: u64,
}

/// The session's cost so far.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct Cost {
    /// In US dollars, as Claude Code estimates it.
    pub usd: f64,
}

impl Measure {
    /// `meters` with what this measured: the context and the cost, sooner than the status
    /// line has them. The model and the rate limits stay the status line's.
    #[must_use]
    pub fn onto(&self, meters: Option<Meters>) -> Meters {
        let mut meters = meters.unwrap_or_default();
        if let Some(context) = self.context {
            meters.context_used_pct = Some(context.percent);
            meters.context_window = Some(context.window);
        }
        if let Some(cost) = self.cost {
            meters.cost_usd = Some(cost.usd);
        }
        meters
    }
}

/// Why a `hello` was not trusted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A mod of another protocol.
    Protocol(u32),
    /// A Claude Code the mod was not verified against.
    Version(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Protocol(p) => write!(f, "mod protocol {p}, not {MOD_PROTOCOL}"),
            Self::Version(v) => {
                write!(f, "Claude Code {v} is not one the mod was verified against")
            }
        }
    }
}

/// Whether a `hello` makes the session's mod trusted: this mod's protocol, on a Claude Code
/// in [`MOD_CLAUDE_VERSIONS`].
///
/// # Errors
///
/// Why not.
pub fn gate(hello: &Hello) -> Result<(), Refusal> {
    if hello.protocol != MOD_PROTOCOL {
        return Err(Refusal::Protocol(hello.protocol));
    }
    if !MOD_CLAUDE_VERSIONS.contains(&hello.claude.as_str()) {
        return Err(Refusal::Version(hello.claude.clone()));
    }
    Ok(())
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
    /// When this follower first saw it, in ms since the Unix epoch: an entry stamped well
    /// before that is an older one.
    since_ms: u64,
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

    /// What to send for this follower to see `board` as it is at `now` (`wall_ms` since the
    /// Unix epoch): new blocks and what the others grew by, and a clear for each block the
    /// board let go. A block whose step stopped before this follower first saw it is left to
    /// the transcript.
    pub fn update(&mut self, board: &Board, now: Instant, wall_ms: u64) -> Vec<Live> {
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
                    since_ms: wall_ms,
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
                let fresh = entry.at_ms == 0
                    || entry.at_ms.saturating_add(ENTRY_SLACK_MS) >= shown.since_ms;
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
