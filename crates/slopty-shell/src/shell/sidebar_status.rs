//! Slopty's own: what a session row says about where its agent stands, after Claude desktop's
//! Code sidebar (values and behaviour only).
//!
//! - **Groups.** "By status" sorts rows into Needs you, Working and Done, in that order. A row
//!   changes group only when its agent's phase changes, and inside a group rows keep the order
//!   their sessions started in, so nothing moves on its own while the person reads.
//! - **Reasons.** A row that waits says why, from what the agent holds open: "Waiting for your
//!   answer", "Plan ready for review", "Needs your approval · run cargo test"; an agent waiting
//!   on its own work says "Waiting · 4m"; one summarising its context says so.
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
use crate::state::{AppState, SESSION_STALE_MS, format_time_ago};
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
}

impl StatusGroup {
    /// The group a row with display status `status` and session `session` sits in.
    pub(super) fn of(status: ChatIndicator, session: Option<&Session>, now: DateTime<Utc>) -> Self {
        match status {
            ChatIndicator::AwaitingInput | ChatIndicator::Errored => Self::NeedsYou,
            ChatIndicator::Working => Self::Working,
            ChatIndicator::Completed | ChatIndicator::Idle => {
                if live(session, now).is_some_and(|s| s.status == SessionStatus::Waiting) {
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
        }
    }

    /// The group's header.
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::NeedsYou => "Needs you",
            Self::Working => "Working",
            Self::Done => "Done",
        }
    }
}

/// Whether a row with display status `status` and session `session` is at rest (Done): the rows
/// a group pages behind "Show more".
pub(super) fn at_rest(status: ChatIndicator, session: Option<&Session>, now: DateTime<Utc>) -> bool {
    StatusGroup::of(status, session, now) == StatusGroup::Done
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
        .gap(px(10.0))
        .px(px(Theme::SPACE_SM))
        .rounded(px(6.0))
        .text_size(crate::typography::ui_rems(13.0))
        .text_color(theme.text_muted.opacity(0.55))
        .cursor_pointer()
        .role(gpui::Role::Button)
        .aria_label(words.clone())
        .hover(|s| s.bg(theme.glass_hover()).text_color(theme.text))
        .child(icon(icons::PLUS).size(px(14.0)).flex_none())
        .child(words)
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
        // As the Archived shelf's: a compact row's height, else 36.
        let height = if self.settings.sidebar_compact {
            super::sidebar_row_height(true, true, false, false)
        } else {
            36.0
        };
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
        let left_group = StatusGroup::of(*left_status, state.session_for(&left.id), now);
        let right_group = StatusGroup::of(*right_status, state.session_for(&right.id), now);
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
    let group = |status: ChatIndicator, chat: &Chat| StatusGroup::of(status, state.session_for(&chat.id), now);
    let needs = rows.iter().filter(|(status, chat)| group(*status, chat) == StatusGroup::NeedsYou);
    let unseen =
        rows.iter().filter(|(status, chat)| group(*status, chat) == StatusGroup::Done && chat.unseen());
    needs.chain(unseen).map(|(_, chat)| chat.id.clone()).collect()
}

/// Whether the visible chats run on more than one machine, so a row's machine glyph tells them
/// apart.
pub(super) fn span_machines(state: &AppState) -> bool {
    let mut devices = state.visible_chats().map(|chat| chat.device_id.as_str());
    let first = devices.next();
    devices.any(|device| Some(device) != first)
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
        ChatIndicator::Completed | ChatIndicator::Idle => {
            let session = session.filter(|s| s.status == SessionStatus::Waiting)?;
            Some(match session.status_since.map(|since| format_time_ago(since, now)) {
                Some(age) if age != "now" => format!("Waiting \u{b7} {age}"),
                _ => "Waiting".to_owned(),
            })
        }
    }
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

/// The agent at rest waiting on its own work (a background task, a wakeup), not on the person.
pub(super) fn waiting_on_itself(session: Option<&Session>, now: DateTime<Utc>) -> bool {
    live(session, now).is_some_and(|s| s.status == SessionStatus::Waiting)
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
        assert_eq!(reason(ChatIndicator::Idle, Some(&resting), now).as_deref(), Some("Waiting \u{b7} 4m"));
        let done = session("a", SessionStatus::Done, now);
        assert_eq!(reason(ChatIndicator::Completed, Some(&done), now), None);
        let mut stale = resting.clone();
        stale.updated_at = now - TimeDelta::minutes(2);
        assert_eq!(reason(ChatIndicator::Idle, Some(&stale), now), None, "its machine went quiet");
    }

    // Rows sit in Needs you, Working, Done; a waiting-on-itself agent is still at work.
    #[test]
    fn rows_group_by_phase() {
        let now = Utc::now();
        let resting = session("a", SessionStatus::Waiting, now);
        assert_eq!(StatusGroup::of(ChatIndicator::AwaitingInput, None, now), StatusGroup::NeedsYou);
        assert_eq!(StatusGroup::of(ChatIndicator::Errored, None, now), StatusGroup::NeedsYou);
        assert_eq!(StatusGroup::of(ChatIndicator::Working, None, now), StatusGroup::Working);
        assert_eq!(StatusGroup::of(ChatIndicator::Idle, Some(&resting), now), StatusGroup::Working);
        assert_eq!(StatusGroup::of(ChatIndicator::Completed, None, now), StatusGroup::Done);
        assert_eq!(StatusGroup::of(ChatIndicator::Idle, None, now), StatusGroup::Done);
        assert!(StatusGroup::NeedsYou < StatusGroup::Working && StatusGroup::Working < StatusGroup::Done);
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
        organization: crate::settings::SidebarOrganization,
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
                let mut shell = super::super::Shell::new(
                    state,
                    crate::state::EngineBootConfig {
                        data_dir: path.clone(),
                        default_harness: slopty_view_proto::HarnessId::MOCK,
                    },
                    cx,
                );
                shell.settings.sidebar_organization = organization;
                shell.settings.sidebar_compact = true;
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
        let (shell, cx) = sidebar(cx, crate::settings::SidebarOrganization::ByProject);
        let order = shell.read_with(cx, |shell, cx| needing_you(shell.state.read(cx), Utc::now()));
        assert_eq!(order, ["question", "approval", "finished"], "whatever the sidebar's organisation");
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

    /// A row draws its machine's glyph only while the visible chats span more than one machine.
    #[gpui::test]
    fn the_machine_glyph_shows_only_across_machines(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = sidebar(cx, crate::settings::SidebarOrganization::ByStatus);
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
        let (shell, cx) = sidebar(cx, crate::settings::SidebarOrganization::ByStatus);
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

    // The server menu offers what to do about servers and machines, Connect a phone only while
    // the server shows its code; Devices opens Settings → Devices.
    #[gpui::test]
    fn the_server_menu_has_rows(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = sidebar(cx, crate::settings::SidebarOrganization::ByStatus);
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
        let (shell, cx) = sidebar(cx, crate::settings::SidebarOrganization::ByStatus);
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
        for (compact, label) in [(true, false), (false, true), (false, false)] {
            shell.update(cx, |shell, cx| {
                shell.settings.sidebar_compact = compact;
                shell.settings.sidebar_show_project_label = label;
                cx.notify();
            });
            redraw(cx);
            for id in ["approval", "question", "working"] {
                let selector: &'static str = Box::leak(format!("chat-{id}-machine").into_boxed_str());
                assert!(cx.debug_bounds(selector).is_some(), "{id} wears its machine ({compact}, {label})");
            }
            assert!(cx.debug_bounds("chat-finished-machine").is_none(), "an unknown machine is not guessed");
        }
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

    /// A shell window, its sidebar by status and `compact` or not, with `count` chats at rest,
    /// newest first: c00, c01…
    fn shell_with_chats(
        cx: &mut gpui::TestAppContext,
        count: usize,
        compact: bool,
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
            let mut shell = super::super::Shell::new(
                state,
                crate::state::EngineBootConfig {
                    data_dir: path.clone(),
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            );
            crate::motion::set_reduced_motion(cx, true);
            shell.settings.sidebar_organization = crate::settings::SidebarOrganization::ByStatus;
            shell.settings.sidebar_compact = compact;
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
        let (shell, cx) = shell_with_chats(cx, 14, true);
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
        let (shell, cx) = shell_with_chats(cx, 3, false);
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
