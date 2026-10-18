//! What a thread's agent does besides the conversation, as its TUI shows it.
//!
//! Its background tasks' figures and the notices they end with, each call's running time and the
//! words it ended with, the turn's live line, the goal it works towards, its passing
//! notifications and next prompt it guesses, its permission rules, its status line, its subagent
//! kinds, its context by category, its MCP servers, and the surfaces its mods draw.
//!
//! Every shape here is agent-neutral and open where agents differ: Claude Code's stream-json
//! drive fills them (`docs/decisions/agents.md`, "Claude Code's TUI, read from its doors"), and
//! an agent that says none of it leaves them empty. Each arrives by its own [`super::Action`] so
//! a ticking figure never resends an item or a list.

use serde::{Deserialize, Serialize};
use slopty_core::WallMs;

use super::{Clipped, ItemId, Retry};

/// A background task's figures so far ([`super::BackgroundTask::usage`], [`TaskNote::usage`]).
///
/// From the agent's `task_progress` or the notice it ended with. The TUI's agents list reads
/// "↓ 290.5k tokens" and "Done (12 tool uses · 41k tokens · 2m 5s)" from these.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct TaskUsage {
    /// Tokens it took.
    pub tokens: u64,
    /// Tool calls it made.
    pub tool_uses: u64,
    /// How long it has run, paused time left out.
    pub duration_ms: u64,
}

/// A background task's notice, as a row of its own in the transcript ([`super::ItemBody::Task`]).
///
/// The TUI's `● Agent "x" finished · 2m 5s`, `● Monitor event: … · lane stall watch`,
/// `● Background command "y" failed with exit code 2`. Linked to its task ([`Self::task`]), which
/// the tray lists, and to the call that started it when the agent names one ([`Self::call`]): a
/// monitor's events name none, and are rows all the same.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TaskNote {
    /// The task, by the agent's id ([`super::BackgroundTask::id`]).
    pub task: String,
    /// What kind of task: [`super::BackgroundTask::kind`]'s words.
    pub kind: String,
    /// What it says of the task. Open: [`TaskNote::COMPLETED`], [`TaskNote::FAILED`],
    /// [`TaskNote::KILLED`], [`TaskNote::WAITING`], [`TaskNote::EVENT`], [`TaskNote::EXPIRED`].
    pub status: String,
    /// The task's description, muted on the right of the row ("lane stall watch").
    pub title: String,
    /// The row's words, as the TUI words them: "Agent \"Look\" is waiting on its own background
    /// work", "Monitor event: STALL lane WATCH idle 20 min", "Background command \"x\" completed
    /// (exit code 0)". The client adds " · {duration}" from [`Self::usage`].
    pub summary: String,
    /// What opens under the row: a monitor's event body, an agent's report or its interim
    /// result; `None` when there is nothing more.
    pub body: Option<Clipped>,
    /// The call that started the task, when the agent names it.
    pub call: Option<ItemId>,
    /// The task's figures as it ended or paused.
    pub usage: Option<TaskUsage>,
    /// A command's exit code, when it ended with one.
    pub exit_code: Option<i32>,
    /// The resources its result linked (an MCP task's `resource_link`s).
    pub links: Vec<crate::visual::ResourceLink>,
}

impl TaskNote {
    /// It finished.
    pub const COMPLETED: &'static str = "completed";
    /// A monitor's event: one of many, the task runs on.
    pub const EVENT: &'static str = "event";
    /// A monitor's watch ran out of time.
    pub const EXPIRED: &'static str = "expired";
    /// It failed.
    pub const FAILED: &'static str = "failed";
    /// It was stopped: by the person, by the agent, or by the machine.
    pub const KILLED: &'static str = "killed";
    /// An agent that stopped with background work of its own still running: its result may be
    /// interim, and the same task may notify again.
    pub const WAITING: &'static str = "waiting";
}

/// What the agent says of one call beside the call itself ([`super::Action::CallNoted`]).
///
/// How long it has run, the background task it became, a retry of a subagent's request, the
/// words it ended with, and its header where the agent's own UI words it better than its kind.
/// The TUI's "Running… (12s)", "Running in the background", "Interrupted", "exit code 2",
/// "Denied by auto mode classifier · reason", "Read report.pdf · pages 1-3".
///
/// A note leaves what it does not say as the last one said it: a running time said after the
/// task keeps the task, and an ending said once stays.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct CallNote {
    /// How long it had run when the agent last said, in ms; a client ticks on from there.
    pub elapsed_ms: Option<u64>,
    /// When the agent last said so.
    pub at_ms: WallMs,
    /// The background task it started or became.
    pub task: Option<String>,
    /// A subagent's request being tried again, inside this call.
    pub retry: Option<Retry>,
    /// How it ended, in the agent's words, when its state alone does not say ("Interrupted",
    /// "exit code 2", "Denied by auto mode classifier · rm is destructive", "Running in the
    /// background").
    pub said: Option<String>,
    /// The call's header in the agent's own UI words, where they say more than its kind's
    /// ("Read report.pdf · pages 1-3", "Update(src/lib.rs)", "Cron(*/5 * * * *: check CI)").
    #[serde(default)]
    pub title: Option<String>,
}

impl CallNote {
    /// This note over `had`, the one said before it: what this one leaves unsaid stays as
    /// `had` said it.
    #[must_use]
    pub fn over(self, had: &Self) -> Self {
        Self {
            task: self.task.or_else(|| had.task.clone()),
            said: self.said.or_else(|| had.said.clone()),
            title: self.title.or_else(|| had.title.clone()),
            ..self
        }
    }
}

/// The turn under way as the TUI's spinner line shows it ([`super::Action::ActivitySet`]).
///
/// "Cogitating… (1m 5s · ↓ 2.3k tokens · thinking some more)". The verb and the elapsed time
/// are the client's own; the rest is the agent's.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Activity {
    /// Output tokens the model has written in the turn so far, as the agent estimates them
    /// (the TUI's "↓ N tokens").
    pub tokens: u64,
    /// Since when the model has been thinking, while it is.
    pub thinking_since: Option<WallMs>,
    /// How long its last burst of thinking took ("thought for 12s").
    pub thought_ms: Option<u64>,
    /// What it is doing, in a phrase: the agent's own summary of the turn, the hook it runs
    /// ("Running `PreToolUse` hook"), or the task in progress; `None` for the client's own verb.
    pub phrase: Option<String>,
    /// Since when the model's reply, while one streams, has said nothing new: no token since.
    /// `None` while no reply streams (a tool runs, the turn waits on the person). The TUI's
    /// spinner reddens from 10 s of it and says the API has not answered.
    #[serde(default)]
    pub quiet_since: Option<WallMs>,
}

/// The goal the agent works towards until a check says it is met (Claude Code's `/goal`):
/// the TUI's "◎ /goal active (5h)" ([`super::Action::GoalSet`]).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Goal {
    /// What has to hold for it to be met.
    pub condition: String,
    /// The checks made so far.
    pub iterations: u32,
    /// When it was set.
    pub set_ms: WallMs,
    /// The session's tokens when it was set, so a client says what it has cost since.
    pub tokens_at_start: u64,
    /// What the last check said, when one has run.
    pub last_reason: Option<String>,
}

/// A passing notification of the agent's, the TUI's right-hand notification row ("3 MCP servers
/// failed · /mcp", "Context low"): one per key, the newest kept ([`super::Action::Notified`]).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AgentNote {
    /// Its key: a later notification of the same key takes its place.
    pub key: String,
    /// What it says.
    pub text: String,
    /// Open, as the agent says: `low`, `medium`, `high`, `immediate`.
    pub priority: String,
    /// A colour the agent names for it (`warning`, `error`), when it does.
    pub color: Option<String>,
    /// When it came.
    pub at_ms: WallMs,
    /// When it stops showing, when the agent says; a client hides it then.
    pub until_ms: Option<WallMs>,
}

impl AgentNote {
    /// The most notifications a thread keeps, newest last.
    pub const KEPT: usize = 16;
}

/// A side question the person asked the agent, and its answer.
///
/// Asked by [`super::wire::Intent::Side`] and told by [`super::Action::Side`]: answered from the
/// conversation so far, without tools, outside the conversation and beside the turn under way
/// (Claude Code's `/btw`).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SideExchange {
    /// The intent that asked it.
    pub intent: super::IntentId,
    /// The question.
    pub question: String,
    /// The answer, once it came; `None` while the agent thinks, or when it failed.
    pub answer: Option<String>,
    /// Why no answer came, when none will.
    pub failed: Option<String>,
    /// When it was asked.
    pub at_ms: WallMs,
}

impl SideExchange {
    /// The most exchanges a thread keeps, newest last: as many as Claude Code keeps.
    pub const KEPT: usize = 20;

    /// Whether the agent is still at it.
    #[must_use]
    pub const fn pending(&self) -> bool {
        self.answer.is_none() && self.failed.is_none()
    }
}

/// The agent's permission rules as its own `/permissions` lists them, read from it
/// ([`super::Action::RulesSet`]) and changed by [`super::wire::Intent::Rule`].
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Rules {
    /// The rules, in the agent's order.
    pub rules: Vec<Rule>,
    /// The folders it may work in beside its own.
    pub directories: Vec<String>,
}

/// One permission rule.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Rule {
    /// What it does to a call it matches: [`Rule::ALLOW`], [`Rule::DENY`], [`Rule::ASK`].
    pub behavior: String,
    /// The rule as the agent writes it (`Bash(cargo test:*)`).
    pub text: String,
    /// Where it comes from. Open: [`Rule::USER`], [`Rule::PROJECT`], [`Rule::LOCAL`], and the
    /// agent's own others (`session`, `policySettings`, `cliArg`, `command`).
    pub source: String,
    /// What it means, in the agent's words, when it says.
    pub description: Option<String>,
    /// Whether [`super::wire::Intent::Rule`] can remove it: it lives in one of the scopes the
    /// worker writes ([`Rule::USER`], [`Rule::PROJECT`], [`Rule::LOCAL`]).
    pub editable: bool,
    /// Whether it is in effect, or overridden by another.
    pub in_effect: bool,
}

impl Rule {
    /// Calls it matches run without asking.
    pub const ALLOW: &'static str = "allow";
    /// Calls it matches are asked about.
    pub const ASK: &'static str = "ask";
    /// Calls it matches are refused.
    pub const DENY: &'static str = "deny";
    /// The project's settings on this machine alone (`.claude/settings.local.json`).
    pub const LOCAL: &'static str = "localSettings";
    /// The project's own settings, shared with the repository (`.claude/settings.json`).
    pub const PROJECT: &'static str = "projectSettings";
    /// The person's settings on this machine, for every project (`~/.claude/settings.json`).
    pub const USER: &'static str = "userSettings";
}

/// A kind of subagent the agent can start, as it lists them: what the composer's `@agent`
/// offers ([`super::Action::AgentsSet`]).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SubagentKind {
    /// Its name, as the agent calls it (`general-purpose`, a custom agent's).
    pub name: String,
    /// What it is for.
    pub description: String,
    /// The model it runs on, when it names one.
    pub model: Option<String>,
}

/// The context window by category, as the agent counts it (Claude Code's `/context`):
/// [`super::Action::ContextSet`]. The meters keep the totals; this is the breakdown.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct ContextUsage {
    /// Each category, in the agent's order.
    pub categories: Vec<ContextCategory>,
    /// Tokens in the context.
    pub tokens: u64,
    /// The window.
    pub window: u64,
    /// The share of the window at which it compacts on its own, in hundredths of a percent;
    /// `None` when it does not.
    pub auto_compact_bp: Option<u32>,
}

/// One of the agent's MCP servers, as the agent last said it ([`super::Action::McpServersSet`]).
///
/// What the context card lists, with a reconnect for one that is not connected and an enable
/// or disable for any ([`super::wire::Intent::McpReconnect`], [`super::wire::Intent::McpEnable`]).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct McpServer {
    /// Its name, as the agent's config names it.
    pub name: String,
    /// Where it stands. Open: [`McpServer::CONNECTED`], [`McpServer::PENDING`],
    /// [`McpServer::FAILED`], [`McpServer::NEEDS_AUTH`], [`McpServer::DISABLED`].
    pub status: String,
    /// Why it failed, in the agent's words, when it did.
    pub error: Option<String>,
    /// Where it is configured, in the agent's words: `user`, `project`, `local`, `plugin`.
    pub source: Option<String>,
}

impl McpServer {
    /// Connected, its tools offered.
    pub const CONNECTED: &'static str = "connected";
    /// Turned off by the person.
    pub const DISABLED: &'static str = "disabled";
    /// It did not connect, or the connection dropped.
    pub const FAILED: &'static str = "failed";
    /// It waits on the person signing in.
    pub const NEEDS_AUTH: &'static str = "needs-auth";
    /// Still connecting.
    pub const PENDING: &'static str = "pending";

    /// Whether a reconnect is worth offering: it is on and not connected.
    #[must_use]
    pub fn reconnects(&self) -> bool {
        self.status != Self::CONNECTED && self.status != Self::DISABLED
    }
}

/// One category of the context ("System prompt", "Messages", "Free space").
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ContextCategory {
    /// Its name, as the agent names it.
    pub name: String,
    /// Its tokens.
    pub tokens: u64,
    /// Whether it is room left rather than room used.
    pub free: bool,
    /// The colour the agent draws it in, by the agent's name for it, when it says.
    pub color: Option<String>,
}

/// Something one of the agent's mods (its plugins' UI) draws ([`super::Action::SurfaceSet`]).
///
/// Rendered with the client's own components: a band over the composer, a toast, a segment of the
/// footer, a pane, a log line. No mod is special: its content is the mod's plain-data element
/// tree, and what the person does in it goes back by [`super::wire::Intent::Surface`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Surface {
    /// Its id, stable while it shows.
    pub id: String,
    /// Where it shows. Open: [`Surface::BAND`], [`Surface::TOAST`], [`Surface::STATUS`],
    /// [`Surface::PANE`], [`Surface::LOG`].
    pub kind: String,
    /// The mod that draws it.
    pub plugin: String,
    /// Its title, for a pane's tab or a band's heading.
    pub title: Option<String>,
    /// Its words, for a toast, a status segment or a log line.
    pub text: Option<String>,
    /// Its element tree as the mod renders it, as JSON: nodes `{type, props, children}` of
    /// `Box`, `Text`, `Button`, `Input`, `Select`, `Link`, `Code`, `Markdown`, `Svg`, each
    /// pressable or editable one with an `id` the intent names. Empty for a text-only surface.
    pub tree: String,
    /// When it stops showing, for a toast.
    pub until_ms: Option<WallMs>,
}

impl Surface {
    /// A band above the composer.
    pub const BAND: &'static str = "band";
    /// A line of the mod's log.
    pub const LOG: &'static str = "log";
    /// A pane of its own, a tab of the right pane.
    pub const PANE: &'static str = "pane";
    /// A segment of the footer under the composer.
    pub const STATUS: &'static str = "status";
    /// A toast.
    pub const TOAST: &'static str = "toast";
}

/// What the person did in a mod's surface ([`super::wire::Intent::Surface`]).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum SurfaceEvent {
    /// Pressed a button.
    Press,
    /// Changed an input's text to this.
    Input(String),
    /// Chose this of a select's options.
    Select(String),
    /// Closed the surface.
    Close,
}

/// A change to one of the agent's permission rules ([`super::wire::Intent::Rule`]), written to
/// the settings file of its scope with the agent's own precedence; credentials are never read
/// or written.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RuleChange {
    /// Added, rather than removed.
    pub add: bool,
    /// [`Rule::ALLOW`], [`Rule::DENY`] or [`Rule::ASK`].
    pub behavior: String,
    /// The rule (`Bash(npm test:*)`).
    pub rule: String,
    /// [`Rule::USER`], [`Rule::PROJECT`] or [`Rule::LOCAL`].
    pub scope: String,
}

/// The files a rewind ([`super::wire::Intent::Rewind`]) changed, as they were just before it.
///
/// The worker keeps them, and [`super::wire::Intent::Unrewind`] puts them back.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Rewound {
    /// The turn whose start the files went back to.
    pub turn: super::TurnId,
    /// What names the files kept before, in git or not.
    pub tree: super::TreeRef,
    /// When.
    pub at_ms: WallMs,
}
