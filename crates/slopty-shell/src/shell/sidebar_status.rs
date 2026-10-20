//! Slopty's own: what a session row says about where its agent stands, after Claude desktop's
//! Code sidebar (values and behaviour only).
//!
//! - **Groups.** "By status" sorts rows into Needs you, Working and Done, in that order. A row
//!   changes group only when its agent's phase changes, and inside a group rows keep the order
//!   their sessions started in, so nothing moves on its own while the person reads. A finished
//!   chat nobody has touched for [`INACTIVE_AFTER_HOURS`] folds into Inactive, last and shut at
//!   first (Amp's "Inactive last 72h"), so Done holds the work of the last days.
//! - **Reasons.** A row that waits says why, from what the agent holds open: "Waiting for your
//!   answer", "Plan ready for review", "Needs your approval · run cargo test"; an agent waiting
//!   on its own work says "Background work", or "1 subagent running" for a subagent it left at
//!   work; one summarising its context says so.
//! - **Times.** A row's time says how long it has stood as it is ([`row_since`]): at work since
//!   its turn began, waiting since it started to, at rest since its last answer.
//! - **Unread weight.** A finished result nobody has seen sets its title in the heavier weight.
//! - **The hover card's facts.** zeron's session hover card (`session_hover_card`) also says
//!   where a row runs (project @ machine), which agent and model, how long it has stood as it is,
//!   and how many subagents it has running.
//! - **Show more.** A group shows its first [`REST_FIRST`] rows at rest, then "Show N more",
//!   which adds [`REST_PAGE`] at a time, as desktop's groups and the Archived shelf do. A row that
//!   needs the person or is at work always shows, and so does the open chat.

use chrono::{DateTime, Local, Utc};
use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px};
use slopty_view_proto::{Chat, ChatIndicator, Session, SessionStatus, SessionWait};

use crate::icons::{self, icon};
use crate::state::{AppState, SESSION_STALE_MS, format_time_ago, running_subagents};
use crate::theme::Theme;

/// An agent as the hover card names it.
fn agent_name(harness: slopty_view_proto::HarnessId) -> &'static str {
    use slopty_view_proto::HarnessId;
    match harness {
        HarnessId::CLAUDE_CODE => "Claude Code",
        HarnessId::CODEX => "Codex",
        HarnessId::PI => "pi",
        // Any other agent by its harness's name.
        other => other.as_str(),
    }
}

/// The groups "By status" sorts rows into, in the order they show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum StatusGroup {
    /// Waiting on the person, or failed.
    NeedsYou,
    /// At work, or waiting on its own work.
    Working,
    /// At rest: finished, stopped, or idle.
    Done,
    /// At rest, and untouched for [`INACTIVE_AFTER_HOURS`].
    Inactive,
}

/// slopty: how long a finished chat rests untouched before it folds into Inactive (Amp's 72 h).
pub(super) const INACTIVE_AFTER_HOURS: i64 = 72;

impl StatusGroup {
    /// The group the row of `chat`, with display status `status` and session `session`, sits in.
    pub(super) fn of(
        status: ChatIndicator,
        chat: &Chat,
        session: Option<&Session>,
        now: DateTime<Utc>,
    ) -> Self {
        match Self::phase(status, session, now) {
            Self::Done if inactive(chat, now) => Self::Inactive,
            group => group,
        }
    }

    /// The group of a row's phase alone: Needs you, Working or Done.
    fn phase(status: ChatIndicator, session: Option<&Session>, now: DateTime<Utc>) -> Self {
        match status {
            ChatIndicator::AwaitingInput | ChatIndicator::Errored => Self::NeedsYou,
            ChatIndicator::Working => Self::Working,
            ChatIndicator::Completed | ChatIndicator::Idle => {
                if waiting_on_itself(session, now) {
                    Self::Working
                } else {
                    Self::Done
                }
            }
        }
    }

    /// The group's key among the sidebar's groups.
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::NeedsYou => "status:needs-you",
            Self::Working => "status:working",
            Self::Done => "status:done",
            Self::Inactive => "status:inactive",
        }
    }

    /// The group's header.
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::NeedsYou => "Needs you",
            Self::Working => "Working",
            Self::Done => "Done",
            Self::Inactive => "Inactive",
        }
    }
}

/// Whether `chat` has gone untouched (no message, else since it started) for
/// [`INACTIVE_AFTER_HOURS`].
fn inactive(chat: &Chat, now: DateTime<Utc>) -> bool {
    let touched = chat.last_message_at.unwrap_or(chat.created_at);
    now.signed_duration_since(touched) >= chrono::TimeDelta::hours(INACTIVE_AFTER_HOURS)
}

/// What the Inactive group says while no chat rests in it.
pub(super) const NO_INACTIVE: &str = "No inactive chats";

/// Whether a row with display status `status` and session `session` is at rest (Done): the rows
/// a group pages behind "Show more".
pub(super) fn at_rest(status: ChatIndicator, session: Option<&Session>, now: DateTime<Utc>) -> bool {
    StatusGroup::phase(status, session, now) == StatusGroup::Done
}

/// Rows at rest a sidebar group shows before "Show more", as the Archived shelf's first page.
pub(super) const REST_FIRST: usize = 10;

/// Rows at rest each "Show more" adds, as the Archived shelf's page.
pub(super) const REST_PAGE: usize = 25;

/// Keep of a group's `rows`, in order, those it draws while it shows `shown` rows at rest
/// (`at_rest`): every row not at rest, the one `kept` (the open chat), and the first `shown` at
/// rest. How many wait behind "Show more".
pub(super) fn page_at_rest<T>(
    rows: &mut Vec<T>,
    shown: usize,
    at_rest: impl Fn(&T) -> bool,
    kept: impl Fn(&T) -> bool,
) -> usize {
    let before = rows.len();
    let mut seen = 0_usize;
    rows.retain(|row| {
        if !at_rest(row) || kept(row) {
            return true;
        }
        seen = seen.saturating_add(1);
        seen <= shown
    });
    before.saturating_sub(rows.len())
}

/// The "Show N more" line under a sidebar group whose `hidden` rows wait, `height` tall, drawn as
/// the Archived shelf draws its own.
pub(super) fn show_more_row(
    id: SharedString,
    hidden: usize,
    height: f32,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let id_text = id.to_string();
    let words = SharedString::from(format!("Show {} more", hidden.min(REST_PAGE)));
    div()
        .id(id)
        .debug_selector(move || id_text)
        .h(px(height))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        // slopty: its words at the rows' title edge, past the one leading slot.
        .gap(px(super::SIDEBAR_LEADING_GAP))
        .px(px(Theme::SPACE_SM))
        .rounded(px(8.0))
        .text_size(crate::typography::ui_rems(13.0))
        .text_color(theme.text_muted.opacity(0.55))
        .cursor_pointer()
        .role(gpui::Role::Button)
        .aria_label(words.clone())
        .hover(|s| s.bg(theme.glass_hover()).text_color(theme.text))
        .child(
            div()
                .size(px(super::SIDEBAR_LEADING_SLOT))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(icon(icons::PLUS).size(px(14.0)).flex_none()),
        )
        .child(words)
}

/// The line a sidebar group with no rows shows under its header (only Inactive stands empty):
/// its key, its height with the list's gap above it, and the line, quiet as "Show more". A shut
/// group's line is drawn for its open motion but says nothing to a screen reader.
pub(super) fn empty_group_row(
    collapse_key: &str,
    open: bool,
    theme: &Theme,
) -> (String, f32, AnyElement) {
    let height = super::sidebar_row_height(false);
    let id = format!("sidebar-group-empty-{collapse_key}");
    let element = div()
        .id(SharedString::from(id.clone()))
        .debug_selector(move || id)
        .h(px(height))
        .flex_none()
        .flex()
        .items_center()
        // slopty: at the rows' title edge.
        .pl(px(Theme::SPACE_SM + super::SIDEBAR_LEADING_SLOT + super::SIDEBAR_LEADING_GAP))
        .pr(px(Theme::SPACE_SM))
        .text_size(crate::typography::ui_rems(13.0))
        .text_color(theme.text_muted.opacity(0.55))
        .when(open, |line| line.role(gpui::Role::Status).aria_label(NO_INACTIVE))
        .child(NO_INACTIVE)
        .into_any_element();
    (format!("empty:{collapse_key}"), height + super::SIDEBAR_LIST_GAP, element)
}

impl super::Shell {
    /// The "Show more" line under the sidebar group of `collapse_key`, when `hidden` rows wait:
    /// its key, its height with the list's gap above it, and the line, which shows another page.
    pub(super) fn render_group_more(
        &self,
        collapse_key: &str,
        hidden: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<(String, f32, AnyElement)> {
        if hidden == 0 {
            return None;
        }
        // As the Archived shelf's: a row's height.
        let height = super::sidebar_row_height(false);
        let key = collapse_key.to_owned();
        let id = SharedString::from(format!("sidebar-group-more-{collapse_key}"));
        let element = show_more_row(id, hidden, height, theme)
            .on_click(cx.listener(move |this, _, _, cx| {
                let shown = this.sidebar_group_shown.get(&key).copied().unwrap_or(REST_FIRST);
                this.sidebar_group_shown.insert(key.clone(), shown.saturating_add(REST_PAGE));
                cx.notify();
            }))
            .into_any_element();
        Some((format!("more:{collapse_key}"), height + super::SIDEBAR_LIST_GAP, element))
    }
}

/// `session`, unless it has gone quiet past the staleness window (its machine stopped saying).
fn live(session: Option<&Session>, now: DateTime<Utc>) -> Option<&Session> {
    session.filter(|s| now.signed_duration_since(s.updated_at).num_milliseconds() <= SESSION_STALE_MS)
}

/// Order `rows` for "By status": by group, then the latest-started session first. A row's place
/// follows only its group and when it started, never its last activity, so a republish or a new
/// message never moves it.
pub(super) fn order_by_status(rows: &mut [(ChatIndicator, Chat)], state: &AppState, now: DateTime<Utc>) {
    rows.sort_by(|(left_status, left), (right_status, right)| {
        let left_group = StatusGroup::of(*left_status, left, state.session_for(&left.id), now);
        let right_group = StatusGroup::of(*right_status, right, state.session_for(&right.id), now);
        left_group
            .cmp(&right_group)
            .then_with(|| right.created_at.cmp(&left.created_at))
            .then_with(|| left.id.cmp(&right.id))
    });
}

/// The chats the "Next chat that needs you" key walks: every chat that waits on the person,
/// across projects and machines, in the order "By status" draws them, then each finished one
/// nobody has seen.
pub(super) fn needing_you(state: &AppState, now: DateTime<Utc>) -> Vec<String> {
    let mut rows: Vec<(ChatIndicator, Chat)> =
        state.overview_chats(now).into_iter().map(|(status, chat)| (status, chat.clone())).collect();
    order_by_status(&mut rows, state, now);
    let group = |status: ChatIndicator, chat: &Chat| StatusGroup::of(status, chat, state.session_for(&chat.id), now);
    let needs = rows.iter().filter(|(status, chat)| group(*status, chat) == StatusGroup::NeedsYou);
    let unseen = rows.iter().filter(|(status, chat)| {
        matches!(group(*status, chat), StatusGroup::Done | StatusGroup::Inactive) && chat.unseen()
    });
    needs.chain(unseen).map(|(_, chat)| chat.id.clone()).collect()
}

/// Whether the visible chats run on machines of more than one kind, so a row's machine glyph
/// tells them apart. slopty: two Macs would wear the one laptop on every row, which tells
/// nothing apart; the hover card names the machine.
pub(super) fn span_machines(state: &AppState) -> bool {
    let kind = |device: &str| {
        state.devices.iter().find(|known| known.id == device).map(|known| super::spaces::device_glyph(&known.platform))
    };
    let mut kinds = state.visible_chats().filter_map(|chat| kind(&chat.device_id));
    let first = kinds.next();
    kinds.any(|glyph| Some(glyph) != first)
}

/// slopty: what tells apart the rows of `chats` that share a title, by chat id: the project,
/// when the rows of one title are not all in one, else the machine, when they are not all on
/// one; a title no other row has, or rows nothing tells apart, have none. Two "hello" rows
/// otherwise read as one chat twice.
pub(super) fn title_hints<'a>(
    state: &AppState,
    chats: impl IntoIterator<Item = &'a Chat>,
) -> std::collections::HashMap<String, SharedString> {
    let rows = chats.into_iter().map(|chat| TitledRow {
        id: chat.id.clone(),
        title: crate::transcript::single_line(chat.title.as_deref().unwrap_or("New chat")).trim().to_lowercase(),
        project: state.space_for_chat(chat).map(|space| space.display_name().to_owned()),
        machine: state.device_name(&chat.device_id).map(str::to_owned),
    });
    hints(&rows.collect::<Vec<_>>())
}

/// A sidebar row as [`title_hints`] reads it.
struct TitledRow {
    id: String,
    title: String,
    project: Option<String>,
    machine: Option<String>,
}

impl TitledRow {
    fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }

    fn machine(&self) -> Option<&str> {
        self.machine.as_deref()
    }
}

/// [`title_hints`] over rows already read.
fn hints(rows: &[TitledRow]) -> std::collections::HashMap<String, SharedString> {
    let mut by_title: std::collections::HashMap<&str, Vec<&TitledRow>> = std::collections::HashMap::new();
    for row in rows {
        by_title.entry(row.title.as_str()).or_default().push(row);
    }
    let mut hints = std::collections::HashMap::new();
    for same in by_title.values().filter(|same| same.len() > 1) {
        let differ = |of: fn(&TitledRow) -> Option<&str>| same.iter().any(|row| of(row) != of(same[0]));
        let pick: fn(&TitledRow) -> Option<&str> = if differ(TitledRow::project) {
            TitledRow::project
        } else if differ(TitledRow::machine) {
            TitledRow::machine
        } else {
            continue;
        };
        for row in same {
            if let Some(hint) = pick(row) {
                hints.insert(row.id.clone(), SharedString::from(hint.to_owned()));
            }
        }
    }
    hints
}

impl super::Shell {
    /// The key to the next chat that needs the person ([`needing_you`]): the one after the open
    /// chat, round the end. Whether there was one to open.
    pub(super) fn open_next_needing_you(&mut self, cx: &mut Context<Self>) -> bool {
        let (order, selected) = {
            let state = self.state.read(cx);
            (needing_you(state, crate::clock::utc(cx)), state.selected_chat.clone())
        };
        let Some(next) = super::tabs::cycle_target(&order, selected.as_deref(), true) else {
            return false;
        };
        if selected.as_deref() != Some(next.as_str()) {
            self.open_chat(next, cx);
        }
        true
    }
}

/// Why a row with display status `status` stands as it does, when that is worth a line: what a
/// waiting agent waits on, or that it summarises its context. `None` for a row whose status word
/// says it all.
pub(super) fn reason(status: ChatIndicator, session: Option<&Session>, now: DateTime<Utc>) -> Option<String> {
    let session = live(session, now);
    let wait = session.and_then(|s| s.waiting_on.as_ref());
    match status {
        ChatIndicator::AwaitingInput => Some(awaiting_words(wait)),
        ChatIndicator::Working => wait
            .filter(|wait| wait.kind == SessionWait::COMPACTING)
            .map(|_| "Compacting its context".to_owned()),
        ChatIndicator::Errored => None,
        // slopty: the row's time says for how long (`row_since`), so the line says only what.
        ChatIndicator::Completed | ChatIndicator::Idle => {
            let session = session.filter(|_| waiting_on_itself(session, now))?;
            Some(match (session.status, running_subagents(Some(session), now)) {
                (SessionStatus::Waiting, _) | (_, 0) => "Background work".to_owned(),
                (_, 1) => "1 subagent running".to_owned(),
                (_, n) => format!("{n} subagents running"),
            })
        }
    }
}

/// slopty: since when a row has stood as it is, which its time says: a turn at work since it
/// began, an agent waiting (on the person or on its own work) since it started to, and a row at
/// rest since its last answer. A turn's time is the agent's, never the last answer's: a chat at
/// work again after a day said "1d".
pub(super) fn row_since(
    status: ChatIndicator,
    chat: &Chat,
    session: Option<&Session>,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    let at_rest = chat.last_message_at.unwrap_or(chat.created_at);
    let session = live(session, now);
    let since = match status {
        ChatIndicator::Working => session.and_then(|s| s.started_at.or(s.status_since)),
        ChatIndicator::AwaitingInput => session.and_then(|s| s.status_since),
        ChatIndicator::Completed | ChatIndicator::Idle if waiting_on_itself(session, now) => {
            session.and_then(|s| s.status_since)
        }
        ChatIndicator::Completed | ChatIndicator::Idle | ChatIndicator::Errored => None,
    };
    since.unwrap_or(at_rest)
}

/// What an agent waiting on the person waits for, in its words.
fn awaiting_words(wait: Option<&SessionWait>) -> String {
    let Some(wait) = wait else { return "Awaiting input".to_owned() };
    let summary = wait
        .summary
        .as_deref()
        .map(crate::transcript::single_line)
        .filter(|summary| !summary.trim().is_empty());
    match wait.kind.as_str() {
        SessionWait::QUESTION => "Waiting for your answer".to_owned(),
        SessionWait::PLAN => "Plan ready for review".to_owned(),
        SessionWait::APPROVAL => match summary {
            Some(summary) => format!("Needs your approval \u{b7} {summary}"),
            None => "Needs your approval".to_owned(),
        },
        _ => summary.unwrap_or_else(|| "Awaiting input".to_owned()),
    }
}

/// A finished result nobody has seen: its title takes the heavier weight.
pub(super) fn unread(status: ChatIndicator) -> bool {
    status == ChatIndicator::Completed
}

/// The agent at rest waiting on its own work (a background task, a wakeup, a subagent it left
/// running), not on the person.
pub(super) fn waiting_on_itself(session: Option<&Session>, now: DateTime<Utc>) -> bool {
    live(session, now).is_some_and(|s| {
        s.status == SessionStatus::Waiting
            || (s.status != SessionStatus::Working
                && s.status != SessionStatus::AwaitingInput
                && running_subagents(Some(s), now) > 0)
    })
}

/// What a row's hover card says beyond zeron's, line by line.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct ChatCard {
    /// "project @ machine", or "@ machine" for a chat with no project.
    pub place: Option<SharedString>,
    /// Its folder, its middle elided when long.
    pub folder: Option<SharedString>,
    /// "Claude Code · opus · High"
    pub agent: Option<SharedString>,
    /// "Working for 3m", "Waiting on you since 14:02", "Done 5m ago"
    pub status: Option<SharedString>,
    /// "2 subagents running"
    pub subagents: Option<SharedString>,
}

/// The longest folder the card shows whole.
const FOLDER_CHARS: usize = 44;

/// `path` within [`FOLDER_CHARS`], its middle given up first: the root and the leaf say the most.
fn elide_middle(path: &str) -> String {
    let chars: Vec<char> = path.chars().collect();
    if chars.len() <= FOLDER_CHARS {
        return path.to_owned();
    }
    let keep = FOLDER_CHARS - 1;
    let head = keep / 3;
    let tail = keep - head;
    let mut out: String = chars[..head].iter().collect();
    out.push('\u{2026}');
    out.extend(&chars[chars.len() - tail..]);
    out
}

/// The hover card of chat `chat_id`'s row, as things stand at `now`; `None` for a chat not here.
pub(super) fn chat_card(state: &AppState, chat_id: &str, now: DateTime<Utc>) -> Option<ChatCard> {
    let chat = state.chats.iter().find(|chat| chat.id == chat_id)?;
    let status = state.display_status_for(chat, now);
    let session = state.session_for(chat_id);
    let device = state.device_name(&chat.device_id);
    let project = state.space_for_chat(chat).map(|space| space.display_name().to_owned());
    let place = match (project, device) {
        (Some(project), Some(device)) => Some(format!("{project} @ {device}")),
        (Some(project), None) => Some(project),
        (None, Some(device)) => Some(format!("@ {device}")),
        (None, None) => None,
    };
    let folder = chat
        .source_context
        .as_ref()
        .map(|context| context.cwd.as_str())
        .or(chat.cwd.as_deref())
        .filter(|cwd| !cwd.is_empty())
        .map(elide_middle);
    let agent = chat.config.as_ref().map(|config| {
        let mut words = vec![agent_name(config.harness).to_owned()];
        words.extend(config.model.clone().filter(|model| !model.is_empty()));
        words.extend(config.reasoning.map(|level| crate::pickers::reasoning_label(level).to_owned()));
        words.join(" \u{b7} ")
    });
    let subagents = state.running_subagents_for(chat_id, now);
    Some(ChatCard {
        place: place.map(SharedString::from),
        folder: folder.map(SharedString::from),
        agent: agent.map(SharedString::from),
        status: status_words(chat, status, session, now).map(SharedString::from),
        subagents: (subagents > 0).then(|| {
            SharedString::from(if subagents == 1 {
                "1 subagent running".to_owned()
            } else {
                format!("{subagents} subagents running")
            })
        }),
    })
}

/// slopty: when `at` was, in the person's time on the system's clock: "2:05 PM" today, "Oct 16,
/// 2:05 PM" before, the year too past this one.
pub(crate) fn absolute_time(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let (at, now) = (at.with_timezone(&Local), now.with_timezone(&Local));
    // The system's clock, as the transcript's times read.
    let clock = crate::transcript::clock_format();
    let format = if at.date_naive() == now.date_naive() {
        clock.to_owned()
    } else if chrono::Datelike::year(&at) == chrono::Datelike::year(&now) {
        format!("%b %-d, {clock}")
    } else {
        format!("%b %-d %Y, {clock}")
    };
    at.format(&format).to_string()
}

/// How long a chat has stood as it is, for its hover card.
fn status_words(chat: &Chat, status: ChatIndicator, session: Option<&Session>, now: DateTime<Utc>) -> Option<String> {
    let session = live(session, now);
    let since = session.and_then(|s| s.status_since.or(s.started_at));
    let ago = |at: DateTime<Utc>| format_time_ago(at, now);
    let last = chat.last_message_at.unwrap_or(chat.created_at);
    // slopty: a relative time also says when, which "2d ago" alone never does.
    let when = |words: String| format!("{words} \u{b7} {}", absolute_time(last, now));
    Some(match status {
        ChatIndicator::Working => match since.map(ago) {
            Some(age) if age != "now" => format!("Working for {age}"),
            _ => "Working".to_owned(),
        },
        ChatIndicator::AwaitingInput => match since {
            Some(since) => {
                format!("Waiting on you since {}", since.with_timezone(&Local).format(crate::transcript::clock_format()))
            }
            None => "Waiting on you".to_owned(),
        },
        ChatIndicator::Errored => {
            when(format!("Failed {} ago", ago(last)).replace("now ago", "just now"))
        }
        ChatIndicator::Completed | ChatIndicator::Idle if waiting_on_itself(session, now) => {
            match since.map(ago) {
                Some(age) if age != "now" => format!("Waiting on its own work for {age}"),
                _ => "Waiting on its own work".to_owned(),
            }
        }
        ChatIndicator::Completed => match ago(last).as_str() {
            "now" => "Done just now".to_owned(),
            age => when(format!("Done {age} ago")),
        },
        ChatIndicator::Idle => {
            let stopped = session.is_some_and(|s| s.status == SessionStatus::Stopped);
            match (stopped, ago(last).as_str()) {
                (true, "now") => "Stopped just now".to_owned(),
                (true, age) => when(format!("Stopped {age} ago")),
                (false, "now") => "Active just now".to_owned(),
                (false, age) => when(format!("Last active {age} ago")),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    /// A relative time also says when: the hour today, the date before, the year past this one.
    #[test]
    fn a_time_says_when() {
        let now = Local::now();
        let local = |at: chrono::DateTime<Local>| at.with_timezone(&Utc);
        let clock = crate::transcript::clock_format();
        assert_eq!(absolute_time(local(now), local(now)), now.format(clock).to_string());
        let before = now - TimeDelta::days(400);
        assert_eq!(
            absolute_time(local(before), local(now)),
            before.format(&format!("%b %-d %Y, {clock}")).to_string()
        );
    }

    fn session(chat: &str, status: SessionStatus, now: DateTime<Utc>) -> Session {
        Session {
            last_completed_turn: None,
            chat_id: chat.into(),
            device_id: "studio".into(),
            status,
            started_at: Some(now - TimeDelta::minutes(5)),
            updated_at: now,
            running_subagents: 0,
            status_since: Some(now - TimeDelta::minutes(4)),
            waiting_on: None,
        }
    }

    fn wait(kind: &str, summary: Option<&str>) -> Option<SessionWait> {
        Some(SessionWait { kind: kind.into(), summary: summary.map(str::to_owned) })
    }

    // The reason line says what a waiting agent waits on, in the person's words.
    #[test]
    fn a_waiting_row_says_why() {
        let now = Utc::now();
        let mut held = session("a", SessionStatus::AwaitingInput, now);
        held.waiting_on = wait(SessionWait::APPROVAL, Some("run cargo test"));
        assert_eq!(
            reason(ChatIndicator::AwaitingInput, Some(&held), now).as_deref(),
            Some("Needs your approval \u{b7} run cargo test")
        );
        held.waiting_on = wait(SessionWait::QUESTION, None);
        assert_eq!(reason(ChatIndicator::AwaitingInput, Some(&held), now).as_deref(), Some("Waiting for your answer"));
        held.waiting_on = wait(SessionWait::PLAN, None);
        assert_eq!(reason(ChatIndicator::AwaitingInput, Some(&held), now).as_deref(), Some("Plan ready for review"));
        held.waiting_on = None;
        assert_eq!(reason(ChatIndicator::AwaitingInput, Some(&held), now).as_deref(), Some("Awaiting input"));
        let mut busy = session("a", SessionStatus::Working, now);
        assert_eq!(reason(ChatIndicator::Working, Some(&busy), now), None, "the status word says it");
        busy.waiting_on = wait(SessionWait::COMPACTING, None);
        assert_eq!(reason(ChatIndicator::Working, Some(&busy), now).as_deref(), Some("Compacting its context"));
        let resting = session("a", SessionStatus::Waiting, now);
        assert_eq!(reason(ChatIndicator::Idle, Some(&resting), now).as_deref(), Some("Background work"));
        let done = session("a", SessionStatus::Done, now);
        assert_eq!(reason(ChatIndicator::Completed, Some(&done), now), None);
        let mut stale = resting.clone();
        stale.updated_at = now - TimeDelta::minutes(2);
        assert_eq!(reason(ChatIndicator::Idle, Some(&stale), now), None, "its machine went quiet");
    }

    // A row's time says how long it has stood as it is: a row at work since its turn began, not
    // since its last answer a day before; a waiting row since it started to wait; a row at rest
    // since its last answer.
    #[test]
    fn a_rows_time_is_how_long_it_has_stood_as_it_is() {
        let now = Utc::now();
        let chat: Chat = serde_json::from_value(serde_json::json!({
            "id": "a", "deviceId": "local", "archived": false, "createdAt": now - TimeDelta::days(2),
            "lastMessageAt": now - TimeDelta::hours(16),
        }))
        .unwrap();
        let since = |status, session: &Session| row_since(status, &chat, Some(session), now);
        let busy = session("a", SessionStatus::Working, now);
        assert_eq!(since(ChatIndicator::Working, &busy), now - TimeDelta::minutes(5), "its turn's time");
        let held = session("a", SessionStatus::AwaitingInput, now);
        assert_eq!(since(ChatIndicator::AwaitingInput, &held), now - TimeDelta::minutes(4));
        let resting = session("a", SessionStatus::Waiting, now);
        assert_eq!(since(ChatIndicator::Idle, &resting), now - TimeDelta::minutes(4));
        let done = session("a", SessionStatus::Done, now);
        assert_eq!(since(ChatIndicator::Completed, &done), now - TimeDelta::hours(16), "its last answer");
        assert_eq!(row_since(ChatIndicator::Errored, &chat, None, now), now - TimeDelta::hours(16));
    }

    // A subagent its agent left at work keeps the row at work on its own: in Working, saying so;
    // a row at work or waiting on the person says what it does itself, and a machine gone quiet
    // says nothing.
    #[test]
    fn a_subagent_left_running_keeps_its_row_at_work() {
        let now = Utc::now();
        let chat: Chat = serde_json::from_value(serde_json::json!({
            "id": "a", "deviceId": "local", "archived": false, "createdAt": now - TimeDelta::hours(1),
        }))
        .unwrap();
        let mut done = session("a", SessionStatus::Done, now);
        done.running_subagents = 2;
        assert!(waiting_on_itself(Some(&done), now));
        assert_eq!(StatusGroup::of(ChatIndicator::Completed, &chat, Some(&done), now), StatusGroup::Working);
        assert_eq!(reason(ChatIndicator::Completed, Some(&done), now).as_deref(), Some("2 subagents running"));
        done.running_subagents = 1;
        assert_eq!(reason(ChatIndicator::Idle, Some(&done), now).as_deref(), Some("1 subagent running"));
        let mut busy = session("a", SessionStatus::Working, now);
        busy.running_subagents = 1;
        assert!(!waiting_on_itself(Some(&busy), now), "at work itself");
        assert_eq!(reason(ChatIndicator::Working, Some(&busy), now), None);
        let mut stale = done.clone();
        stale.updated_at = now - TimeDelta::minutes(2);
        assert!(!waiting_on_itself(Some(&stale), now));
        assert_eq!(reason(ChatIndicator::Completed, Some(&stale), now), None, "its machine went quiet");
    }

    // Rows of one title say what tells them apart: the project when theirs differ, else the
    // machine; a title of its own, or rows nothing tells apart, say nothing more.
    #[test]
    fn rows_of_one_title_say_what_tells_them_apart() {
        let row = |id: &str, title: &str, project: Option<&str>, machine: &str| TitledRow {
            id: id.into(),
            title: title.into(),
            project: project.map(str::to_owned),
            machine: Some(machine.into()),
        };
        let hinted = hints(&[
            row("a", "hello", Some("slopty"), "studio"),
            row("b", "hello", Some("better-update"), "studio"),
            row("c", "fix the lexer", Some("slopty"), "studio"),
            row("d", "tidy", Some("slopty"), "studio"),
            row("e", "tidy", Some("slopty"), "laptop"),
            row("f", "again", Some("slopty"), "studio"),
            row("g", "again", Some("slopty"), "studio"),
        ]);
        assert_eq!(hinted.get("a").map(SharedString::as_ref), Some("slopty"));
        assert_eq!(hinted.get("b").map(SharedString::as_ref), Some("better-update"));
        assert_eq!(hinted.get("c"), None, "a title of its own");
        assert_eq!(hinted.get("d").map(SharedString::as_ref), Some("studio"), "one project, two machines");
        assert_eq!(hinted.get("e").map(SharedString::as_ref), Some("laptop"));
        assert!(!hinted.contains_key("f") && !hinted.contains_key("g"), "nothing tells them apart");
    }

    // Rows sit in Needs you, Working, Done; a waiting-on-itself agent is still at work; a
    // finished chat untouched for 72 hours is Inactive, and only a finished one.
    #[test]
    fn rows_group_by_phase() {
        let now = Utc::now();
        let resting = session("a", SessionStatus::Waiting, now);
        let fresh: Chat = serde_json::from_value(serde_json::json!({
            "id": "a", "deviceId": "local", "archived": false, "createdAt": now - TimeDelta::hours(100),
            "lastMessageAt": now - TimeDelta::hours(71),
        }))
        .unwrap();
        let of = |status, session| StatusGroup::of(status, &fresh, session, now);
        assert_eq!(of(ChatIndicator::AwaitingInput, None), StatusGroup::NeedsYou);
        assert_eq!(of(ChatIndicator::Errored, None), StatusGroup::NeedsYou);
        assert_eq!(of(ChatIndicator::Working, None), StatusGroup::Working);
        assert_eq!(of(ChatIndicator::Idle, Some(&resting)), StatusGroup::Working);
        assert_eq!(of(ChatIndicator::Completed, None), StatusGroup::Done);
        assert_eq!(of(ChatIndicator::Idle, None), StatusGroup::Done);
        let mut stale = fresh.clone();
        stale.last_message_at = Some(now - TimeDelta::hours(INACTIVE_AFTER_HOURS));
        let of = |status, session| StatusGroup::of(status, &stale, session, now);
        assert_eq!(of(ChatIndicator::Idle, None), StatusGroup::Inactive);
        assert_eq!(of(ChatIndicator::Completed, None), StatusGroup::Inactive);
        assert_eq!(of(ChatIndicator::AwaitingInput, None), StatusGroup::NeedsYou, "waits, so never");
        assert_eq!(of(ChatIndicator::Working, None), StatusGroup::Working, "at work, so never");
        assert!(at_rest(ChatIndicator::Idle, None, now), "paged as Done is");
        assert!(
            StatusGroup::NeedsYou < StatusGroup::Working
                && StatusGroup::Working < StatusGroup::Done
                && StatusGroup::Done < StatusGroup::Inactive
        );
    }

    /// A sidebar alone, drawn from a shell with no engine.
    struct SidebarHost(gpui::Entity<super::super::Shell>);

    impl gpui::Render for SidebarHost {
        fn render(&mut self, _: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.0.update(cx, |shell, cx| {
                div().w(px(300.0)).h(px(900.0)).child(shell.render_chat_sidebar(&Theme::default(), cx))
            })
        }
    }

    fn chat(id: &str, minutes_ago: i64, now: DateTime<Utc>, unseen: bool) -> Chat {
        let created = now - TimeDelta::minutes(minutes_ago);
        serde_json::from_value(serde_json::json!({
            "id": id, "title": id, "deviceId": "local", "archived": false,
            "createdAt": created,
            "lastMessageAt": now - TimeDelta::seconds(30),
            "lastSeenAt": if unseen { now - TimeDelta::minutes(5) } else { now },
        }))
        .unwrap()
    }

    /// Four chats as Claude desktop's sidebar names them: one at work, one holding an approval,
    /// one asking, one done and unread.
    fn four(now: DateTime<Utc>) -> (Vec<Chat>, Vec<Session>) {
        let chats = vec![
            chat("working", 40, now, false),
            chat("approval", 30, now, false),
            chat("question", 20, now, false),
            chat("finished", 10, now, true),
        ];
        let mut approval = session("approval", SessionStatus::AwaitingInput, now);
        approval.waiting_on = wait(SessionWait::APPROVAL, Some("run cargo test"));
        let mut question = session("question", SessionStatus::AwaitingInput, now);
        question.waiting_on = wait(SessionWait::QUESTION, None);
        let sessions = vec![
            session("working", SessionStatus::Working, now),
            approval,
            question,
            session("finished", SessionStatus::Done, now),
        ];
        (chats, sessions)
    }

    fn sidebar(
        cx: &mut gpui::TestAppContext,
    ) -> (gpui::Entity<super::super::Shell>, &mut gpui::VisualTestContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let path = dir.path().to_path_buf();
        let (host, cx) = cx.add_window_view(move |_, cx| {
            SidebarHost(cx.new(|cx| {
                let state = cx.new(|_| AppState::new());
                let shell = super::super::Shell::new(
                    state,
                    crate::state::EngineBootConfig {
                        data_dir: path.clone(),
                        default_harness: slopty_view_proto::HarnessId::MOCK,
                    },
                    cx,
                );
                let now = Utc::now();
                let (chats, sessions) = four(now);
                shell.state.update(cx, |state, _| {
                    state.workspace_scope = Some(slopty_view_proto::WorkspaceScope::Local);
                    state.local_device_id = Some("local".into());
                    state.chats = chats;
                    state.apply_sessions(sessions);
                });
                shell
            }))
        });
        std::mem::forget(dir);
        let shell = host.read_with(cx, |host, _| host.0.clone());
        (shell, cx)
    }

    /// The key walks the chats that need the person, latest started first, then the finished
    /// one nobody has seen, and comes round again; the chat at work is never one.
    #[gpui::test]
    fn the_needs_you_key_walks_the_waiting_then_the_unseen(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = sidebar(cx);
        let order = shell.read_with(cx, |shell, cx| needing_you(shell.state.read(cx), Utc::now()));
        assert_eq!(order, ["question", "approval", "finished"]);
        let mut opened = Vec::new();
        for _ in 0..4 {
            shell.update(cx, |shell, cx| assert!(shell.open_next_needing_you(cx)));
            opened.push(shell.read_with(cx, |shell, cx| shell.state.read(cx).selected_chat.clone().unwrap()));
        }
        assert_eq!(opened, ["question", "approval", "finished", "question"]);
        // With nobody waiting, the key opens nothing.
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| state.chats.retain(|chat| chat.id == "working"));
            assert!(!shell.open_next_needing_you(cx));
        });
    }

    /// A row draws its machine's glyph only while the visible chats span more than one kind of
    /// machine.
    #[gpui::test]
    fn the_machine_glyph_shows_only_across_machines(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = sidebar(cx);
        let device = |id: &str, platform: &str| -> slopty_view_proto::Device {
            serde_json::from_value(serde_json::json!({ "id": id, "name": id, "platform": platform }))
                .unwrap()
        };
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.devices = vec![device("local", "macos"), device("studio", "linux")];
            });
        });
        redraw(cx);
        assert!(cx.debug_bounds("chat-question-machine").is_none(), "one machine: nothing to tell apart");
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                state.devices.push(device("laptop", "macos"));
                if let Some(chat) = state.chats.iter_mut().find(|chat| chat.id == "working") {
                    chat.device_id = "laptop".into();
                }
                cx.notify();
            });
        });
        redraw(cx);
        assert!(cx.debug_bounds("chat-question-machine").is_none(), "two Macs: the same glyph on every row");
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                if let Some(chat) = state.chats.iter_mut().find(|chat| chat.id == "working") {
                    chat.device_id = "studio".into();
                }
                cx.notify();
            });
        });
        redraw(cx);
        assert!(cx.debug_bounds("chat-question-machine").is_some(), "two machines: each row says its own");
        assert!(cx.debug_bounds("chat-working-machine").is_some());
    }

    fn top(cx: &mut gpui::VisualTestContext, id: &str) -> f32 {
        let selector: &'static str = Box::leak(format!("chat-{id}").into_boxed_str());
        f32::from(cx.debug_bounds(selector).unwrap_or_else(|| panic!("row {id}")).origin.y)
    }

    fn redraw(cx: &mut gpui::VisualTestContext) {
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
    }

    // Grouped by status: Needs you over Working over Done, each waiting row with its reason, the
    // unread result in the heavier weight; and ten republishes with fresh activity move nothing.
    #[gpui::test]
    fn by_status_groups_say_why_and_never_reorder_on_republish(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = sidebar(cx);
        redraw(cx);
        let needs = [top(cx, "approval"), top(cx, "question")];
        let (working, finished) = (top(cx, "working"), top(cx, "finished"));
        assert!(needs.iter().all(|y| *y < working), "Needs you first: {needs:?} {working}");
        assert!(working < finished, "Working before Done");
        // Within Needs you: the latest started first.
        assert!(needs[1] < needs[0], "question started after approval");
        for id in ["approval", "question"] {
            let selector: &'static str = Box::leak(format!("chat-reason-{id}").into_boxed_str());
            assert!(cx.debug_bounds(selector).is_some(), "{id} says why");
        }
        assert!(cx.debug_bounds("chat-reason-working").is_none());
        assert!(cx.debug_bounds("chat-reason-finished").is_none());
        let before: Vec<f32> = ["approval", "question", "working", "finished"].map(|id| top(cx, id)).into();
        for round in 0..10 {
            let now = Utc::now() + TimeDelta::seconds(round);
            shell.update(cx, |shell, cx| {
                shell.state.update(cx, |state, cx| {
                    let (_, mut sessions) = four(now);
                    for session in &mut sessions {
                        session.updated_at = now;
                    }
                    // Fresh activity on the oldest row, which would lead a recency sort.
                    if let Some(chat) = state.chats.iter_mut().find(|chat| chat.id == "approval") {
                        chat.last_message_at = Some(now);
                    }
                    state.apply_sessions(sessions);
                    cx.notify();
                });
            });
            redraw(cx);
            let after: Vec<f32> =
                ["approval", "question", "working", "finished"].map(|id| top(cx, id)).into();
            assert_eq!(before, after, "republish {round} moved a row");
        }
    }

    // slopty: a row draws one mark before its title (opencode's avatar slot): a result nobody
    // has read its done mark, with no agent's mark beside it and no dot on a corner; a row at
    // work or waiting its own; a row read and at rest its agent's mark. Every title starts at one
    // edge.
    #[gpui::test]
    fn a_row_draws_one_mark_before_its_title(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = sidebar(cx);
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                for chat in &mut state.chats {
                    chat.config = Some(
                        serde_json::from_value(
                            serde_json::json!({ "harness": "claude-code", "sandbox": "workspace-write" }),
                        )
                        .expect("a chat config"),
                    );
                }
                cx.notify();
            });
        });
        redraw(cx);
        let ids = ["working", "approval", "question", "finished"];
        for id in ids {
            let mark: &'static str = Box::leak(format!("chat-mark-{id}").into_boxed_str());
            let agent: &'static str = Box::leak(format!("chat-agent-{id}").into_boxed_str());
            assert!(cx.debug_bounds(mark).is_some(), "{id} says its state");
            assert!(cx.debug_bounds(agent).is_none(), "{id}: one mark, not the agent's beside it");
        }
        let left = |cx: &mut gpui::VisualTestContext, id: &str| {
            let title: &'static str = Box::leak(format!("chat-title-{id}").into_boxed_str());
            cx.debug_bounds(title).unwrap_or_else(|| panic!("{id}'s title")).left()
        };
        let edge = left(cx, "working");
        assert!(ids.iter().all(|id| left(cx, id) == edge), "every title at one edge");
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                if let Some(chat) = state.chats.iter_mut().find(|chat| chat.id == "finished") {
                    chat.last_seen_at = chat.last_message_at;
                }
                cx.notify();
            });
        });
        redraw(cx);
        assert!(cx.debug_bounds("chat-mark-finished").is_none(), "read, at rest: no state to say");
        assert!(cx.debug_bounds("chat-agent-finished").is_some(), "its agent's mark");
        assert_eq!(left(cx, "finished"), edge, "in the same slot");
    }

    // A finished chat opened with its jump key stops saying "Done" on the next frames the window
    // shows, with the accessibility tree built on every frame (a Mac with an assistant running,
    // where view retention replays what was not redrawn): the row's tree and paint are what a
    // frame from scratch draws.
    #[gpui::test]
    fn a_finished_chat_opened_reads_seen_with_the_tree_kept(cx: &mut gpui::TestAppContext) {
        use super::super::retention_tests::{DESKTOP, frames, launch, stale};
        let (shell, cx) = launch(cx, DESKTOP);
        cx.update(|window, _| {
            window.activate_window();
            window.set_a11y_active(true);
        });
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                if let Some(chat) = state.chats.iter_mut().find(|chat| chat.id == "b") {
                    chat.last_seen_at = None;
                }
                cx.notify();
            });
        });
        let tick = std::time::Duration::from_millis(16);
        frames(cx, tick, 120);
        let says_done = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, _| {
                let tree = window.a11y_tree().expect("the tree built");
                tree.nodes.iter().any(|(_, node)| {
                    node.role() == gpui::accesskit::Role::Status && node.label() == Some("Done")
                })
            })
        };
        assert!(says_done(cx), "unseen, it says so");
        assert_eq!(stale(cx), None, "before it opens");
        frames(cx, tick, 120);
        cx.simulate_keystrokes("cmd-2");
        frames(cx, tick, 120);
        let opened = shell.read_with(cx, |shell, cx| shell.state.read(cx).selected_chat.clone());
        assert_eq!(opened.as_deref(), Some("b"), "the jump key opened it");
        assert!(!says_done(cx), "seen, the row's tree says so on the frames shown");
        assert_eq!(stale(cx), None, "and paints it");
        // Every view watching the state hears the open chat seen, as the sidebar's pane does
        // through the shell: a mark made while a frame drew reached none of them.
        let heard = std::rc::Rc::new(std::cell::Cell::new(None::<bool>));
        let state = shell.read_with(cx, |shell, _| shell.state.clone());
        let _watch = cx.update(|_, cx| {
            let heard = heard.clone();
            cx.observe(&state, move |state, cx| {
                heard.set(Some(state.read(cx).chats.iter().any(|chat| chat.id == "b" && chat.unseen())));
            })
        });
        let answer = |cx: &mut gpui::VisualTestContext, round: i64| {
            shell.update(cx, |shell, cx| {
                shell.state.update(cx, |state, cx| {
                    if let Some(chat) = state.chats.iter_mut().find(|chat| chat.id == "b") {
                        chat.last_message_at = Some(Utc::now() + TimeDelta::seconds(round));
                        chat.last_seen_at = None;
                    }
                    cx.notify();
                });
            });
        };
        let seen = |cx: &mut gpui::VisualTestContext| {
            shell.read_with(cx, |shell, cx| shell.state.read(cx).chats.iter().any(|chat| chat.id == "b" && !chat.unseen()))
        };
        // The chat open, a new answer arrives in it (or its server sends the row back before
        // its seen mark is round): seen as it arrives.
        for round in 1..=3 {
            answer(cx, round);
            frames(cx, tick, 120);
            assert!(seen(cx), "round {round}: the open chat seen again");
            assert_eq!(heard.get(), Some(false), "round {round}: every watcher heard it seen");
            assert!(!says_done(cx), "round {round}: the row's tree says so");
            assert_eq!(stale(cx), None, "round {round}: and paints it");
        }
        // An answer arriving while the window is behind another stays unseen, and the row says
        // so; the window back in front, it is seen.
        cx.deactivate_window();
        frames(cx, tick, 120);
        answer(cx, 4);
        frames(cx, tick, 120);
        assert!(!seen(cx), "nobody looked");
        assert!(says_done(cx), "the row says it");
        cx.update(|window, _| window.activate_window());
        frames(cx, tick, 120);
        assert!(seen(cx), "looked at again");
        assert_eq!(heard.get(), Some(false), "every watcher heard it seen");
        assert!(!says_done(cx), "the row's tree says so");
        assert_eq!(stale(cx), None, "and paints it");
    }

    // The server menu offers what to do about servers and machines, Connect a phone only while
    // the server shows its code; Devices opens Settings → Devices.
    #[gpui::test]
    fn the_server_menu_has_rows(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = sidebar(cx);
        cx.update(|window, _| window.activate_window());
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.connectivity.server = Some(slopty_view_proto::ServerPresence {
                    name: "studio".into(),
                    online: true,
                    actions: Vec::new(),
                    invite: None,
                });
            });
        });
        redraw(cx);
        let trigger = cx.debug_bounds("user-menu").expect("the server pill").center();
        cx.simulate_click(trigger, gpui::Modifiers::default());
        redraw(cx);
        assert!(cx.debug_bounds("user-menu-devices").is_some());
        assert!(cx.debug_bounds("user-menu-change-server").is_some());
        assert!(cx.debug_bounds("user-menu-connect-phone").is_none(), "no code to show");
        let devices = cx.debug_bounds("user-menu-devices").unwrap().center();
        cx.simulate_click(devices, gpui::Modifiers::default());
        redraw(cx);
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.route, super::super::Route::Settings(super::super::SettingsSection::Devices));
        });
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                if let Some(server) = state.connectivity.server.as_mut() {
                    server.invite = Some(slopty_view_proto::ServerInvite::default());
                }
            });
            shell.set_route(super::super::Route::Chat, cx);
        });
        redraw(cx);
        let trigger = cx.debug_bounds("user-menu").expect("the server pill").center();
        cx.simulate_click(trigger, gpui::Modifiers::default());
        redraw(cx);
        assert!(cx.debug_bounds("user-menu-connect-phone").is_some(), "the code is offered");
    }

    // Every row wears its machine's kind, in each layout, and names the machine; a chat whose
    // machine the app does not know wears none rather than a guess.
    #[gpui::test]
    fn every_row_says_which_machine_it_runs_on(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = sidebar(cx);
        let device = |id: &str, name: &str, platform: &str| -> slopty_view_proto::Device {
            serde_json::from_value(serde_json::json!({
                "id": id, "name": name, "platform": platform, "lastSeenAt": null,
            }))
            .unwrap()
        };
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.devices = vec![device("local", "studio", "macos"), device("box", "box", "linux")];
                if let Some(chat) = state.chats.iter_mut().find(|chat| chat.id == "working") {
                    chat.device_id = "box".into();
                }
                if let Some(chat) = state.chats.iter_mut().find(|chat| chat.id == "finished") {
                    chat.device_id = "gone".into();
                }
            });
        });
        redraw(cx);
        for id in ["approval", "question", "working"] {
            let selector: &'static str = Box::leak(format!("chat-{id}-machine").into_boxed_str());
            assert!(cx.debug_bounds(selector).is_some(), "{id} wears its machine");
        }
        assert!(cx.debug_bounds("chat-finished-machine").is_none(), "an unknown machine is not guessed");
        assert_eq!(super::super::spaces::device_glyph("linux"), crate::icons::REMOTE_SERVER);
        assert_eq!(super::super::spaces::device_glyph("macos"), crate::icons::LAPTOP);
        assert_eq!(super::super::spaces::device_glyph("ios"), crate::icons::SMARTPHONE);
    }

    /// A group shows its first rows at rest and keeps the rest behind "Show more"; a row that
    /// needs the person or works, and the open chat, always show.
    #[test]
    fn a_group_pages_its_rows_at_rest() {
        // (id, at rest): every seventh needs the person; the last is the open chat.
        let mut rows: Vec<(usize, bool)> = (0..30).map(|id| (id, id % 7 != 0)).collect();
        let hidden = page_at_rest(&mut rows, REST_FIRST, |row| row.1, |row| row.0 == 29);
        assert_eq!(hidden, 14, "25 at rest: ten shown, the open chat, fourteen behind");
        assert_eq!(rows.iter().filter(|row| !row.1).count(), 5, "every row not at rest shows");
        assert!(rows.iter().any(|row| row.0 == 29), "the open chat shows");
        let mut few: Vec<(usize, bool)> = (0..REST_FIRST).map(|id| (id, true)).collect();
        assert_eq!(page_at_rest(&mut few, REST_FIRST, |row| row.1, |_| false), 0, "nothing to page");
    }

    /// A shell window with `count` chats at rest in its sidebar, newest first: c00, c01…
    fn shell_with_chats(
        cx: &mut gpui::TestAppContext,
        count: usize,
    ) -> (gpui::Entity<super::super::Shell>, &mut gpui::VisualTestContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
            crate::composer::init(cx, crate::settings::ComposerSendBehavior::default());
            super::super::apply_keymap(
                cx,
                &crate::settings::KeymapConfig::default(),
                crate::settings::ComposerSendBehavior::default(),
            );
        });
        let path = dir.path().to_path_buf();
        let (shell, cx) = cx.add_window_view(move |_, cx| {
            let state = cx.new(|_| AppState::new());
            let shell = super::super::Shell::new(
                state,
                crate::state::EngineBootConfig {
                    data_dir: path.clone(),
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            );
            crate::motion::set_reduced_motion(cx, true);
            let now = Utc::now();
            let chats = (0..count).map(|n| chat(&format!("c{n:02}"), i64::try_from(n).unwrap(), now, false));
            shell.state.update(cx, |state, _| {
                state.connection = slopty_view_proto::view::ConnectionStatus::Ready;
                state.workspace_scope = Some(slopty_view_proto::WorkspaceScope::Local);
                state.local_device_id = Some("local".into());
                state.chats = chats.collect();
            });
            shell
        });
        std::mem::forget(dir);
        (shell, cx)
    }

    fn shown(cx: &mut gpui::VisualTestContext, id: &str) -> bool {
        let selector: &'static str = Box::leak(format!("chat-{id}").into_boxed_str());
        cx.debug_bounds(selector).is_some()
    }

    /// Done shows its first ten rows, then "Show 4 more"; pressed, it shows them all and no keys
    /// skip a row it does not draw. The open chat shows wherever it falls.
    #[gpui::test]
    fn a_long_group_shows_more_on_request(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = shell_with_chats(cx, 14);
        redraw(cx);
        const MORE: &str = "sidebar-group-more-status:status:done";
        assert!(shown(cx, "c09") && !shown(cx, "c10"), "the first page");
        assert!(cx.debug_bounds(MORE).is_some(), "Show more under the first page");
        let order = shell.update(cx, |shell, cx| shell.sidebar_visible_order(cx));
        assert_eq!(order.len(), REST_FIRST, "keys reach only the rows drawn: {order:?}");
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| state.selected_chat = Some("c12".into()));
        });
        redraw(cx);
        assert!(shown(cx, "c12") && !shown(cx, "c11"), "the open chat shows");

        let more = cx.debug_bounds(MORE).expect("Show more").center();
        cx.simulate_click(more, gpui::Modifiers::default());
        redraw(cx);
        assert!((10..14).all(|n| shown(cx, &format!("c{n:02}"))), "every row now");
        assert!(cx.debug_bounds(MORE).is_none(), "nothing left behind it");
        let order = shell.update(cx, |shell, cx| shell.sidebar_visible_order(cx));
        assert_eq!(order.len(), 14, "{order:?}");
    }

    /// Holding the jump keys' modifier numbers the first rows with their keys; let go, the
    /// numbers go; the key opens its row.
    #[gpui::test]
    fn holding_the_modifier_numbers_the_rows(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = shell_with_chats(cx, 3);
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        redraw(cx);
        assert!(cx.update(|window, _| window.is_window_active()), "the window is the active one");
        assert!(cx.debug_bounds("jump-hint-c00").is_none(), "no numbers before the key is held");
        let held = if crate::keys::MOD_IS_COMMAND { // slopty
            gpui::Modifiers { platform: true, ..gpui::Modifiers::default() }
        } else {
            gpui::Modifiers { control: true, ..gpui::Modifiers::default() }
        };
        cx.simulate_modifiers_change(held);
        assert!(shell.read_with(cx, |shell, _| shell.jump_hints), "the held key is heard");
        redraw(cx);
        for id in ["jump-hint-c00", "jump-hint-c01", "jump-hint-c02"] {
            assert!(cx.debug_bounds(id).is_some(), "{id} while held");
        }
        cx.simulate_modifiers_change(gpui::Modifiers::default());
        redraw(cx);
        assert!(cx.debug_bounds("jump-hint-c00").is_none(), "gone once let go");
        cx.simulate_keystrokes(&crate::settings::platform_combo("mod-2"));
        let open = shell.read_with(cx, |shell, cx| shell.state.read(cx).selected_chat.clone());
        assert_eq!(open.as_deref(), Some("c01"), "the second row's key opens it");
    }

    #[test]
    fn a_long_folder_gives_up_its_middle() {
        let short = "/Users/me/src/slopty";
        assert_eq!(elide_middle(short), short);
        let long = "/Users/someone/Workspace/oss/a-very-long-project-name/crates/slopty-shell";
        let elided = elide_middle(long);
        assert_eq!(elided.chars().count(), FOLDER_CHARS);
        assert!(elided.starts_with("/Users/some") && elided.ends_with("crates/slopty-shell"), "{elided}");
        assert!(elided.contains('\u{2026}'));
    }
}
