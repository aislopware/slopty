//! What a thread does besides its conversation, as the shell's dock and tray read it.
//!
//! Its background work beside the child chats it started ([`tray`]); the turn's live line, the
//! goal, notifications, mode, context and mods' surfaces ([`dock`]); and its permission rules
//! ([`rules`]).
//!
//! Pure: the workspace hands in a thread's state and the rows of the threads it started, and
//! publishes what comes out (`workspace::view_model::work`).

use slopty_proto::thread::wire::ThreadRow;
use slopty_proto::thread::{
    BackgroundTask, Cap, ItemBody, Notice, Phase, Rule, ThreadId, ThreadState, TurnState, Usage,
};
use slopty_view_doc::{
    ActivityLine, ChatDock, ContextBreakdown, ContextSlice, DockCan, GoalLine, MainRow,
    McpServerRow, ModSurface, ModeChip, ModeChoice, NoteLine, PermissionRuleRow, PermissionRules,
    RetryLine, RuleScope, SideLine, StatusSpan, SubagentKindRow, TurnTokens, WorkCounts, WorkRow,
    WorkTray, tokens_short,
};

use super::transcript::at;

/// A chat another agent started under this thread, as its row says it: Claude Code's own
/// subagent of a call, or an agent started through Slopty's verbs.
#[derive(Clone, Copy, Debug)]
pub struct Child<'a> {
    /// Its row in its worker's table.
    pub row: &'a ThreadRow,
}

/// The tray's row id of the agent's task `task`.
#[must_use]
pub fn task_row(task: &str) -> String {
    format!("task:{task}")
}

/// The tray's row id of the child chat `chat`.
#[must_use]
pub fn chat_row(chat: &str) -> String {
    format!("chat:{chat}")
}

/// A background task's kind as the tray's, and its noun.
#[must_use]
pub fn kind_of(kind: &str) -> (&str, &'static str) {
    match kind {
        BackgroundTask::SHELL => (WorkRow::COMMAND, "command"),
        BackgroundTask::AGENT => (WorkRow::SUBAGENT, "subagent"),
        BackgroundTask::MONITOR => (WorkRow::MONITOR, "monitor"),
        BackgroundTask::WORKFLOW => (WorkRow::WORKFLOW, "workflow"),
        BackgroundTask::MCP => (WorkRow::MCP, "MCP task"),
        BackgroundTask::REMOTE => (WorkRow::REMOTE, "remote agent"),
        BackgroundTask::TEAMMATE => (WorkRow::TEAMMATE, "teammate"),
        other => (other, "background task"),
    }
}

/// A task's state in the tray's words.
#[must_use]
pub fn state_words(state: &str) -> String {
    match state {
        BackgroundTask::RUNNING => "running",
        BackgroundTask::PENDING => "queued",
        BackgroundTask::PAUSED => "paused",
        BackgroundTask::WAITING => "waiting",
        BackgroundTask::COMPLETED => "done",
        BackgroundTask::FAILED => "error",
        BackgroundTask::KILLED => "stopped",
        NEEDS_YOU => "needs you",
        other => other,
    }
    .to_owned()
}

/// A child chat's state when it waits on the person.
const NEEDS_YOU: &str = "needs-you";

/// Figures in words, time left out: "12 tool uses · 41k tokens".
#[must_use]
pub fn figures(tool_uses: Option<u64>, tokens: Option<u64>) -> String {
    let mut out = Vec::new();
    match tool_uses {
        Some(1) => out.push("1 tool use".to_owned()),
        Some(n) if n > 0 => out.push(format!("{n} tool uses")),
        _ => {}
    }
    if let Some(tokens) = tokens.filter(|n| *n > 0) {
        out.push(format!("{} tokens", tokens_short(tokens)));
    }
    out.join(" · ")
}

/// A child chat's state, as a task's would say it.
const fn child_state(row: &ThreadRow) -> &'static str {
    match row.status.phase {
        Phase::Working => BackgroundTask::RUNNING,
        Phase::Waiting => BackgroundTask::WAITING,
        Phase::NeedsYou => NEEDS_YOU,
        Phase::Failed => BackgroundTask::FAILED,
        Phase::Stopped => BackgroundTask::KILLED,
        Phase::Idle | Phase::Done => BackgroundTask::COMPLETED,
    }
}

/// What a child chat asks of the person now, in a line, while it does.
#[must_use]
pub fn child_asks(row: &ThreadRow) -> Option<String> {
    let card = row.requests.first()?;
    let title = card.title.trim();
    Some(if title.is_empty() { "Needs your answer".to_owned() } else { title.to_owned() })
}

/// `state`'s background work beside the chats it started (`children`), as the tray lists it.
/// `chat_of` names a thread by the id the shell knows its chat by.
#[must_use]
pub fn tray(
    state: &ThreadState,
    children: &[Child<'_>],
    chat_of: &dyn Fn(ThreadId) -> String,
) -> WorkTray {
    let can_stop_task = state.meta.can(Cap::STOP_TASK);
    let model_of = |agent_type: Option<&str>| {
        let agent_type = agent_type?;
        state.agents.iter().find(|kind| kind.name == agent_type).and_then(|kind| kind.model.clone())
    };
    let mut rows: Vec<WorkRow> = Vec::new();
    for task in &state.tasks {
        let (kind, noun) = kind_of(&task.kind);
        let child = task.child.and_then(|id| children.iter().find(|c| c.row.id == id));
        let needs_you = child.and_then(|c| child_asks(c.row));
        let usage = task.usage;
        let live = task.is_live();
        rows.push(WorkRow {
            id: task_row(&task.id),
            kind: kind.to_owned(),
            noun: noun.to_owned(),
            title: task.title.clone(),
            agent_type: task.agent_type.clone(),
            model: model_of(task.agent_type.as_deref()),
            state: if needs_you.is_some() { NEEDS_YOU.to_owned() } else { task.state.clone() },
            state_words: state_words(if needs_you.is_some() { NEEDS_YOU } else { &task.state }),
            live,
            wakes: live && (task.awaited || task.kind != BackgroundTask::SHELL),
            awaited: task.awaited,
            ambient: task.ambient,
            needs_you,
            step: task.step.clone().or_else(|| child.and_then(|c| c.row.doing.clone())),
            tokens: usage.map(|u| u.tokens).filter(|n| *n > 0),
            tool_uses: usage.map(|u| u.tool_uses).filter(|n| *n > 0),
            duration_ms: usage.map(|u| u.duration_ms).filter(|n| *n > 0),
            figures: figures(usage.map(|u| u.tool_uses), usage.map(|u| u.tokens)),
            started_at: at(task.started_ms),
            ended_at: task.ended_ms.map(at),
            outcome: task.outcome.clone(),
            output: task.output.as_ref().map(|out| out.text.clone()).filter(|t| !t.is_empty()),
            timeout_ms: task.timeout_ms,
            persistent: task.persistent,
            call_part_id: task.item.as_ref().map(|item| item.0.clone()),
            chat_id: task.child.map(chat_of),
            parent: task.parent.as_deref().map(task_row),
            depth: 0,
            can_stop: live && can_stop_task,
        });
    }
    // The chats it started that no task stands for.
    for child in children {
        if state.tasks.iter().any(|task| task.child == Some(child.row.id)) {
            continue;
        }
        let row = child.row;
        let needs_you = child_asks(row);
        let state_now = if needs_you.is_some() { NEEDS_YOU } else { child_state(row) };
        let live = matches!(row.status.phase, Phase::Working | Phase::Waiting | Phase::NeedsYou);
        let started = row.began_ms.filter(|_| live).unwrap_or(row.created_ms);
        let chat = chat_of(row.id);
        rows.push(WorkRow {
            id: chat_row(&chat),
            kind: WorkRow::SUBAGENT.to_owned(),
            noun: "subagent".to_owned(),
            title: row.title.clone(),
            agent_type: row.facts.get("agent_type").cloned(),
            model: row.meters.model.clone(),
            state: state_now.to_owned(),
            state_words: state_words(state_now),
            live,
            wakes: live,
            awaited: false,
            ambient: false,
            needs_you,
            step: row.doing.clone().or_else(|| row.last_line.clone()).filter(|_| live),
            tokens: None,
            tool_uses: None,
            duration_ms: None,
            figures: String::new(),
            started_at: at(started),
            ended_at: (!live).then(|| at(row.updated_ms)),
            outcome: None,
            output: None,
            timeout_ms: None,
            persistent: false,
            call_part_id: row.parent.as_ref().map(|link| link.item.0.clone()),
            chat_id: Some(chat),
            parent: None,
            depth: 0,
            can_stop: live && row.caps.iter().any(|cap| cap.0 == Cap::INTERRUPT),
        });
    }
    // One row per piece of work: a task the thread names twice (its start and its stream) is
    // counted once, as the tray lists it once.
    let rows = distinct(rows);
    let counts = counts(&rows);
    let ordered = order(&rows);
    WorkTray {
        main: main_row(state),
        summary: counts.summary(),
        can_stop_all: ordered.iter().any(|row| row.can_stop),
        counts,
        rows: ordered,
    }
}

/// `rows` with one row per id, the last word on each kept where the first stood.
fn distinct(rows: Vec<WorkRow>) -> Vec<WorkRow> {
    let mut out: Vec<WorkRow> = Vec::with_capacity(rows.len());
    for row in rows {
        match out.iter_mut().find(|kept| kept.id == row.id) {
            Some(kept) => *kept = row,
            None => out.push(row),
        }
    }
    out
}

/// The live rows by kind, housekeeping left out.
fn counts(rows: &[WorkRow]) -> WorkCounts {
    let mut counts = WorkCounts::default();
    for row in rows.iter().filter(|row| row.live && !row.ambient) {
        let slot = match row.kind.as_str() {
            WorkRow::COMMAND => &mut counts.commands,
            WorkRow::MONITOR => &mut counts.monitors,
            WorkRow::SUBAGENT => &mut counts.subagents,
            WorkRow::WORKFLOW => &mut counts.workflows,
            WorkRow::MCP => &mut counts.mcp,
            WorkRow::REMOTE => &mut counts.remote,
            WorkRow::TEAMMATE => &mut counts.teammates,
            _ => &mut counts.other,
        };
        *slot = slot.saturating_add(1);
        if row.needs_you.is_some() {
            counts.needs_you = counts.needs_you.saturating_add(1);
        }
    }
    counts
}

/// The rows in the tray's order: what needs the person, then what is at work by when it began,
/// then what ended, the latest first and [`WorkTray::ENDED_KEPT`] at most; each row's own work
/// after it, one deeper.
fn order(rows: &[WorkRow]) -> Vec<WorkRow> {
    let rank =
        |row: &WorkRow| u8::from(row.needs_you.is_none()).saturating_add(u8::from(!row.live));
    let mut top: Vec<&WorkRow> = rows
        .iter()
        .filter(|row| row.parent.as_ref().is_none_or(|p| !rows.iter().any(|r| r.id == *p)))
        .collect();
    top.sort_by(|a, b| {
        rank(a).cmp(&rank(b)).then_with(|| {
            if a.live {
                a.started_at.cmp(&b.started_at)
            } else {
                b.ended_at.unwrap_or(b.started_at).cmp(&a.ended_at.unwrap_or(a.started_at))
            }
        })
    });
    let mut ended = 0_usize;
    top.retain(|row| {
        if row.live {
            return true;
        }
        ended = ended.saturating_add(1);
        ended <= WorkTray::ENDED_KEPT
    });
    let mut out = Vec::with_capacity(rows.len());
    for row in top {
        place(row, 0, rows, &mut out);
    }
    out
}

/// `row` at `depth`, then the rows it started, one deeper.
fn place(row: &WorkRow, depth: u32, rows: &[WorkRow], out: &mut Vec<WorkRow>) {
    if out.iter().any(|placed| placed.id == row.id) || depth > 8 {
        return;
    }
    let mut placed = row.clone();
    placed.depth = depth;
    out.push(placed);
    for child in rows.iter().filter(|r| r.parent.as_deref() == Some(row.id.as_str())) {
        place(child, depth.saturating_add(1), rows, out);
    }
}

/// The turn under way, if any: when it began.
fn turn_started(state: &ThreadState) -> Option<i64> {
    state
        .turns
        .last()
        .filter(|turn| turn.state == TurnState::Active)
        .map(|turn| at(turn.started_ms))
        .filter(|ms| *ms > 0)
}

/// The chat's own agent as the agents list's first row.
fn main_row(state: &ThreadState) -> MainRow {
    let working = state.status.phase == Phase::Working;
    MainRow {
        working,
        step: state.activity.as_ref().and_then(|a| a.phrase.clone()).filter(|_| working),
        tokens: state.activity.as_ref().map(|a| a.tokens).filter(|n| *n > 0 && working),
        started_at: turn_started(state),
    }
}

/// What a failed request is, in the TUI's first words: "API error", "Overloaded", a limit by
/// its name.
fn retry_what(text: &str) -> String {
    let lower = text.to_lowercase();
    if lower.starts_with("no response from the api after") {
        text.trim().to_owned()
    } else if lower.contains("overloaded") || lower.contains("529") {
        "Overloaded".to_owned()
    } else if lower.contains("rate limit") || lower.contains("429") {
        "Rate limited".to_owned()
    } else if lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("no response")
    {
        "No response from the API".to_owned()
    } else {
        "API error".to_owned()
    }
}

/// The status page the TUI points at when an overload persists, from the third attempt.
const STATUS_PAGE: &str = "If it persists, check status.claude.com.";

/// A retry notice as the line under way says it.
#[must_use]
pub fn retry_line(notice: &Notice, at_ms: i64) -> Option<RetryLine> {
    let retry = notice.retry?;
    let what = retry_what(&notice.text.text);
    let overloaded = what == "Overloaded";
    Some(RetryLine {
        attempt: retry.attempt,
        max: retry.max,
        at: retry
            .in_ms
            .and_then(|ms| i64::try_from(ms).ok())
            .map(|ms| at_ms.saturating_add(ms))
            .filter(|_| at_ms > 0),
        what,
        hint: (overloaded && retry.attempt >= 3).then(|| STATUS_PAGE.to_owned()),
    })
}

/// The request the turn under way is trying again, while it is: its last notice with a retry,
/// nothing of the agent's after it.
fn retrying(state: &ThreadState) -> Option<RetryLine> {
    let turn = state.turns.last().filter(|turn| turn.state == TurnState::Active)?;
    let last = state.items.iter().rev().find(|item| item.turn == turn.id)?;
    match &last.body {
        ItemBody::Notice(notice) if notice.kind == Notice::API_ERROR => {
            retry_line(notice, at(last.at_ms))
        }
        _ => None,
    }
}

/// The task in progress of the agent's list, and the one after it.
fn todo_now(state: &ThreadState) -> (Option<String>, Option<String>) {
    let Some(plan) = &state.plan else { return (None, None) };
    let now = plan.steps.iter().position(|step| step.status == "in_progress");
    let doing = now.and_then(|ix| plan.steps.get(ix)).map(|step| step.text.clone());
    let next = plan
        .steps
        .iter()
        .skip(now.map_or(0, |ix| ix.saturating_add(1)))
        .find(|step| step.status == "pending")
        .map(|step| step.text.clone());
    (doing, next)
}

/// The session's tokens as far as `state` holds its turns.
fn session_tokens(state: &ThreadState) -> u64 {
    state.turns.iter().fold(0_u64, |sum, turn| sum.saturating_add(turn.usage.tokens()))
}

/// A permission mode's chip.
fn mode_chip(state: &ThreadState) -> Option<ModeChip> {
    let id = state.meters.mode.clone()?;
    let choices: Vec<ModeChoice> = state
        .meta
        .modes
        .iter()
        .map(|mode| ModeChoice {
            id: mode.id.clone(),
            label: mode.label.clone(),
            description: mode.description.clone(),
            recommended: mode.id == RECOMMENDED_MODE,
        })
        .collect();
    let label = choices
        .iter()
        .find(|mode| mode.id == id)
        .map_or_else(|| humane(&id), |mode| mode.label.clone());
    // The recommended default says its name quietly; only a mode the person chose that asks less,
    // or plans, stands out.
    // Don't ask refuses what was not allowed beforehand: the safer side, so it is quiet; only
    // a mode that asks nothing and lets everything through is loud.
    let tone = match id.as_str() {
        "bypassPermissions" => ModeChip::DANGER,
        "plan" => ModeChip::PLAN,
        "dontAsk" | "acceptEdits" => ModeChip::ACCEPT,
        _ => ModeChip::QUIET,
    };
    let words = if tone == ModeChip::QUIET { String::new() } else { format!("{label} on") };
    Some(ModeChip { id, label, words, tone: tone.to_owned(), choices })
}

/// The mode Claude Code starts in when nothing chooses another, and the one it recommends: auto,
/// where a judge asks only about what looks risky
/// (`slopty_agent::claude::offers`, its built-in mode).
const RECOMMENDED_MODE: &str = "auto";

/// An agent's camel-cased name for people: `acceptEdits` → "Accept edits".
fn humane(id: &str) -> String {
    let mut out = String::new();
    for (ix, c) in id.chars().enumerate() {
        if ix == 0 {
            out.extend(c.to_uppercase());
        } else if c.is_uppercase() {
            out.push(' ');
            out.extend(c.to_lowercase());
        } else if c == '-' || c == '_' {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

/// A share of a window, 0 to 100.
fn percent(part: u64, whole: u64) -> u8 {
    let share = part.saturating_mul(100).checked_div(whole).unwrap_or(0);
    u8::try_from(share.min(100)).unwrap_or(100)
}

/// How near the context is to full, when it is worth a word: the TUI warns 20 points before
/// the agent compacts on its own, or before the window is full when it does not.
const WARN_WITHIN: u8 = 20;

/// The context window, by category when the agent counted them.
fn context(state: &ThreadState) -> Option<ContextBreakdown> {
    let (tokens, window, categories, auto) = match &state.context {
        Some(ctx) => (ctx.tokens, ctx.window, ctx.categories.as_slice(), ctx.auto_compact_bp),
        None => (state.meters.context_tokens?, state.meters.context_window?, [].as_slice(), None),
    };
    let used = percent(tokens, window);
    let auto_compact_percent = auto.map(|bp| u8::try_from((bp / 100).min(100)).unwrap_or(100));
    let (words, urgent) = if let Some(at) = auto_compact_percent {
        let until = at.saturating_sub(used);
        ((until <= WARN_WITHIN).then(|| format!("{until}% until auto-compact")), false)
    } else {
        let left = 100_u8.saturating_sub(used);
        let low = left <= WARN_WITHIN && window > 0;
        (
            low.then(|| {
                format!("Context low ({left}% remaining) · Run /compact to compact & continue")
            }),
            low,
        )
    };
    let share = |n: u64| {
        u32::try_from(n.saturating_mul(10_000).checked_div(window).unwrap_or(0)).unwrap_or(u32::MAX)
    };
    let last_turn =
        state.turns.iter().rev().find(|turn| turn.ended_ms.is_some()).map(|turn| TurnTokens {
            input: turn.usage.get(Usage::INPUT),
            output: turn.usage.get(Usage::OUTPUT),
            cache_read: turn.usage.get(Usage::CACHE_READ),
            cache_write: turn.usage.get(Usage::CACHE_WRITE),
        });
    Some(ContextBreakdown {
        tokens,
        window,
        used_percent: used,
        categories: categories
            .iter()
            .map(|c| ContextSlice {
                name: c.name.clone(),
                tokens: c.tokens,
                share_bp: share(c.tokens),
                free: c.free,
                color: c.color.clone(),
            })
            .collect(),
        auto_compact_percent,
        words,
        urgent,
        last_turn,
    })
}

/// A notification's tone, from its colour or its priority.
fn note_tone(color: Option<&str>, priority: &str) -> &'static str {
    match color {
        Some(c) if c.contains("error") || c.contains("red") => NoteLine::ERROR,
        Some(c) if c.contains("warn") || c.contains("yellow") || c.contains("orange") => {
            NoteLine::WARNING
        }
        _ if priority == "immediate" || priority == "high" => NoteLine::WARNING,
        _ => NoteLine::INFO,
    }
}

/// What the agent waits on once its turn ended with work running: the turn footer's line.
fn waiting_words(state: &ThreadState, counts: &WorkCounts) -> Option<String> {
    if state.status.phase != Phase::Waiting || counts.total() == 0 {
        return None;
    }
    let agents = counts.subagents.saturating_add(counts.workflows).saturating_add(counts.teammates);
    if agents > 0 {
        let noun = if agents == 1 { "subagent" } else { "subagents" };
        return Some(format!("Waiting for {agents} {noun} to finish"));
    }
    Some(format!("{} still running", counts.summary()))
}

/// `state`'s dock, at `now_ms`; `counts` is its tray's.
#[must_use]
pub fn dock(state: &ThreadState, counts: &WorkCounts, now_ms: i64) -> ChatDock {
    let working = state.status.phase == Phase::Working;
    let (doing, next) = todo_now(state);
    let activity = working.then(|| {
        let live = state.activity.clone().unwrap_or_default();
        ActivityLine {
            started_at: turn_started(state),
            tokens: live.tokens,
            thinking_since: live.thinking_since.map(at).filter(|ms| *ms > 0),
            thought_ms: live.thought_ms,
            effort: state.meters.effort.clone(),
            phrase: live.phrase.or(doing),
            next,
            retry: retrying(state),
            quiet_since: live.quiet_since.map(at).filter(|ms| *ms > 0),
        }
    });
    let goal = state.goal.as_ref().map(|goal| GoalLine {
        condition: goal.condition.clone(),
        set_at: at(goal.set_ms),
        iterations: goal.iterations,
        tokens: (!state.older)
            .then(|| session_tokens(state).saturating_sub(goal.tokens_at_start))
            .filter(|n| *n > 0),
        last_reason: goal.last_reason.clone(),
    });
    let notes = state
        .notes
        .iter()
        .filter(|note| note.until_ms.is_none_or(|until| at(until) > now_ms))
        .map(|note| NoteLine {
            key: note.key.clone(),
            text: note.text.clone(),
            tone: note_tone(note.color.as_deref(), &note.priority).to_owned(),
            priority: note.priority.clone(),
            at: at(note.at_ms),
            until: note.until_ms.map(at),
        })
        .collect();
    let surfaces = state
        .surfaces
        .iter()
        .filter(|s| s.until_ms.is_none_or(|until| at(until) > now_ms))
        .map(|s| ModSurface {
            id: s.id.clone(),
            kind: s.kind.clone(),
            plugin: s.plugin.clone(),
            title: s.title.clone(),
            text: s.text.clone(),
            tree: serde_json::from_str(&s.tree).unwrap_or(serde_json::Value::Null),
            until: s.until_ms.map(at),
        })
        .collect();
    let can = |cap: &str| state.meta.can(cap);
    ChatDock {
        activity,
        goal,
        notes,
        suggestion: state.suggestion.clone().filter(|s| !s.trim().is_empty()),
        status_line: state.status_line.as_deref().map(status_spans).unwrap_or_default(),
        mode: mode_chip(state),
        waiting: waiting_words(state, counts),
        context: context(state),
        subagent_kinds: state
            .agents
            .iter()
            .map(|kind| SubagentKindRow {
                name: kind.name.clone(),
                description: kind.description.clone(),
                model: kind.model.clone(),
            })
            .collect(),
        surfaces,
        side: state
            .side
            .iter()
            .map(|s| SideLine {
                id: s.intent.to_string(),
                question: s.question.clone(),
                answer: s.answer.clone(),
                failed: s.failed.clone(),
                at: at(s.at_ms),
            })
            .collect(),
        can: DockCan {
            stop_work: can(Cap::STOP_TASK),
            background: can(Cap::BACKGROUND),
            shell: can(Cap::SHELL),
            rewind: can(Cap::REWIND),
            fork: can(Cap::FORK),
            rules: can(Cap::RULES),
            surfaces: can(Cap::SURFACES),
            set_mode: can(Cap::SET_MODE) && !state.meta.modes.is_empty(),
            side: can(Cap::SIDE),
            mcp_servers: can(Cap::MCP_SERVERS),
        },
        servers: state
            .servers
            .iter()
            .map(|server| McpServerRow {
                name: server.name.clone(),
                status: server.status.clone(),
                error: server.error.clone(),
                source: server.source.clone(),
                reconnects: server.reconnects(),
            })
            .collect(),
    }
}

/// The scopes a rule can be added to, the narrowest first.
fn scopes() -> Vec<RuleScope> {
    [
        (Rule::LOCAL, "This project, this machine", ".claude/settings.local.json"),
        (Rule::PROJECT, "This project, shared", ".claude/settings.json"),
        (Rule::USER, "You, every project", "~/.claude/settings.json"),
    ]
    .into_iter()
    .map(|(id, label, file)| RuleScope {
        id: id.to_owned(),
        label: label.to_owned(),
        file: file.to_owned(),
    })
    .collect()
}

/// Where a rule comes from, for people.
fn source_label(source: &str) -> String {
    match source {
        Rule::LOCAL => "This project, this machine",
        Rule::PROJECT => "This project, shared",
        Rule::USER => "You, every project",
        "session" => "This session",
        "cliArg" => "How it was started",
        "policySettings" => "Your organisation",
        "flagSettings" => "Its flags",
        "command" => "A command",
        other => return humane(other),
    }
    .to_owned()
}

/// `state`'s permission rules, as the chat `chat` reads them.
#[must_use]
pub fn rules(state: &ThreadState, chat: &str) -> PermissionRules {
    let rules = state.rules.as_ref();
    PermissionRules {
        chat_id: chat.to_owned(),
        known: rules.is_some(),
        rules: rules
            .map(|rules| {
                rules
                    .rules
                    .iter()
                    .map(|rule| PermissionRuleRow {
                        behavior: rule.behavior.clone(),
                        rule: rule.text.clone(),
                        source: rule.source.clone(),
                        source_label: source_label(&rule.source),
                        description: rule.description.clone(),
                        editable: rule.editable,
                        in_effect: rule.in_effect,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        directories: rules.map(|rules| rules.directories.clone()).unwrap_or_default(),
        scopes: if state.meta.can(Cap::RULES) { scopes() } else { Vec::new() },
    }
}

/// A status line's text, its ANSI styles read into spans; escapes it does not style by (a
/// link, a title) are dropped.
#[must_use]
pub fn status_spans(text: &str) -> Vec<StatusSpan> {
    let mut spans: Vec<StatusSpan> = Vec::new();
    let mut style = StatusSpan::default();
    let mut chars = text.chars().peekable();
    let mut run = String::new();
    let flush = |run: &mut String, style: &StatusSpan, spans: &mut Vec<StatusSpan>| {
        if run.is_empty() {
            return;
        }
        let text = std::mem::take(run);
        match spans.last_mut() {
            Some(last) if StatusSpan { text: String::new(), ..last.clone() } == *style => {
                last.text.push_str(&text);
            }
            _ => spans.push(StatusSpan { text, ..style.clone() }),
        }
    };
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if c != '\r' && (c == '\n' || !c.is_control()) {
                run.push(c);
            }
            continue;
        }
        match chars.next() {
            Some('[') => {
                let mut params = String::new();
                let mut end = None;
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        end = Some(c);
                        break;
                    }
                    params.push(c);
                }
                if end == Some('m') {
                    flush(&mut run, &style, &mut spans);
                    sgr(&params, &mut style);
                }
            }
            // An OSC (a link, a title) runs to BEL or ST; its text is not drawn.
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    flush(&mut run, &style, &mut spans);
    spans
}

/// One SGR sequence's parameters applied to `style`.
fn sgr(params: &str, style: &mut StatusSpan) {
    let codes: Vec<u16> = params
        .split([';', ':'])
        .map(|p| if p.is_empty() { 0 } else { p.parse().unwrap_or(0) })
        .collect();
    let codes = if codes.is_empty() { vec![0] } else { codes };
    let mut ix = 0;
    while let Some(&code) = codes.get(ix) {
        ix = ix.saturating_add(1);
        match code {
            0 => *style = StatusSpan::default(),
            1 => style.bold = true,
            2 => style.dim = true,
            3 => style.italic = true,
            4 => style.underline = true,
            22 => {
                style.bold = false;
                style.dim = false;
            }
            23 => style.italic = false,
            24 => style.underline = false,
            30..=37 => style.fg = Some(format!("ansi:{}", code.saturating_sub(30))),
            90..=97 => style.fg = Some(format!("ansi:{}", code.saturating_sub(82))),
            40..=47 => style.bg = Some(format!("ansi:{}", code.saturating_sub(40))),
            100..=107 => style.bg = Some(format!("ansi:{}", code.saturating_sub(92))),
            39 => style.fg = None,
            49 => style.bg = None,
            38 | 48 => {
                let colour = match codes.get(ix) {
                    Some(5) => {
                        let n = codes.get(ix.saturating_add(1)).copied();
                        ix = ix.saturating_add(2);
                        n.map(|n| format!("ansi:{}", n.min(255)))
                    }
                    Some(2) => {
                        let rgb: Vec<u16> =
                            codes.iter().skip(ix.saturating_add(1)).take(3).copied().collect();
                        ix = ix.saturating_add(4);
                        match rgb.as_slice() {
                            [r, g, b] => Some(format!(
                                "#{:02x}{:02x}{:02x}",
                                (*r).min(255),
                                (*g).min(255),
                                (*b).min(255)
                            )),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                if code == 38 {
                    style.fg = colour;
                } else {
                    style.bg = colour;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
