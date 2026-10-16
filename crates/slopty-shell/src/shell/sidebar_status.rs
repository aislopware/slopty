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
//! - **The hover card.** Hovering a row shows where it runs (project @ machine, folder, branch),
//!   which agent and model, how long it has stood as it is, and its running subagents.

use chrono::{DateTime, Local, Utc};
use gpui::{AnyElement, AnyView, App, Context, IntoElement, Render, SharedString, Window, div, prelude::*, px};
use slopty_view_proto::{Chat, ChatIndicator, Session, SessionStatus, SessionWait};

use crate::icons::{self, icon};
use crate::state::{AppState, SESSION_STALE_MS, format_time_ago};
use crate::theme::Theme;

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

/// What a row's hover card says, line by line.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct ChatCard {
    pub title: SharedString,
    /// "project @ machine", or "@ machine" for a chat with no project.
    pub place: Option<SharedString>,
    /// Whether its machine answers now.
    pub online: bool,
    /// Its folder, its middle elided when long.
    pub folder: Option<SharedString>,
    pub branch: Option<SharedString>,
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
    let branch = crate::change_requests::conversation_branch(chat, &state.spaces)
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
        .map(str::to_owned);
    let agent = chat.config.as_ref().map(|config| {
        let mut words = vec![super::harness_updates::agent_name(config.harness).to_owned()];
        words.extend(config.model.clone().filter(|model| !model.is_empty()));
        words.extend(config.reasoning.map(|level| crate::pickers::reasoning_label(level).to_owned()));
        words.join(" \u{b7} ")
    });
    let subagents = state.running_subagents_for(chat_id, now);
    Some(ChatCard {
        title: crate::transcript::single_line(chat.title.as_deref().unwrap_or("New session")).into(),
        place: place.map(SharedString::from),
        online: state.device_online(&chat.device_id, now),
        folder: folder.map(SharedString::from),
        branch: branch.map(SharedString::from),
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

/// How long a chat has stood as it is, for its hover card.
fn status_words(chat: &Chat, status: ChatIndicator, session: Option<&Session>, now: DateTime<Utc>) -> Option<String> {
    let session = live(session, now);
    let since = session.and_then(|s| s.status_since.or(s.started_at));
    let ago = |at: DateTime<Utc>| format_time_ago(at, now);
    let last = chat.last_message_at.unwrap_or(chat.created_at);
    Some(match status {
        ChatIndicator::Working => match since.map(ago) {
            Some(age) if age != "now" => format!("Working for {age}"),
            _ => "Working".to_owned(),
        },
        ChatIndicator::AwaitingInput => match since {
            Some(since) => {
                format!("Waiting on you since {}", since.with_timezone(&Local).format("%H:%M"))
            }
            None => "Waiting on you".to_owned(),
        },
        ChatIndicator::Errored => format!("Failed {} ago", ago(last)).replace("now ago", "just now"),
        ChatIndicator::Completed | ChatIndicator::Idle if waiting_on_itself(session, now) => {
            match since.map(ago) {
                Some(age) if age != "now" => format!("Waiting on its own work for {age}"),
                _ => "Waiting on its own work".to_owned(),
            }
        }
        ChatIndicator::Completed => match ago(last).as_str() {
            "now" => "Done just now".to_owned(),
            age => format!("Done {age} ago"),
        },
        ChatIndicator::Idle => {
            let stopped = session.is_some_and(|s| s.status == SessionStatus::Stopped);
            match (stopped, ago(last).as_str()) {
                (true, "now") => "Stopped just now".to_owned(),
                (true, age) => format!("Stopped {age} ago"),
                (false, "now") => "Active just now".to_owned(),
                (false, age) => format!("Last active {age} ago"),
            }
        }
    })
}

/// The `.tooltip(..)` that shows chat `chat_id`'s card, built from the state when it opens.
pub(super) fn hover_card(
    state: gpui::Entity<AppState>,
    chat_id: String,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |_, cx| {
        let card = chat_card(state.read(cx), &chat_id, Utc::now()).unwrap_or_default();
        cx.new(|_| ChatHoverCard { card }).into()
    }
}

/// The card's width: room for a long title and a folder without wrapping.
const CARD_WIDTH: f32 = 280.0;

/// The hover card: zeron's frosted popup card (12 corners, the popup plane) with the session's
/// title over one line per fact, each led by its glyph.
struct ChatHoverCard {
    card: ChatCard,
}

impl Render for ChatHoverCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).for_popup();
        let card = &self.card;
        let line = |glyph: &'static str, words: SharedString, theme: &Theme| -> AnyElement {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .min_h(px(20.0))
                .child(icon(glyph).size(px(13.0)).flex_none().text_color(theme.text_muted))
                .child(div().flex_1().min_w_0().truncate().child(words))
                .into_any_element()
        };
        // What a screen reader says of the card: its lines in order, one per line.
        let spoken = [
            &card.place,
            &card.folder,
            &card.branch,
            &card.agent,
            &card.status,
            &card.subagents,
        ]
        .into_iter()
        .flatten()
        .map(SharedString::as_ref)
        .collect::<Vec<&str>>()
        .join("\n");
        let mut lines: Vec<AnyElement> = Vec::new();
        if let Some(place) = card.place.clone() {
            let dot = div()
                .size(px(6.0))
                .flex_none()
                .rounded_full()
                .bg(if card.online { theme.success } else { crate::theme::ink(0.24) });
            lines.push(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .min_h(px(20.0))
                    .child(icon(icons::REMOTE_SERVER).size(px(13.0)).flex_none().text_color(theme.text_muted))
                    .child(div().flex_1().min_w_0().truncate().child(place))
                    .child(dot)
                    .into_any_element(),
            );
        }
        if let Some(folder) = card.folder.clone() {
            lines.push(line(icons::FOLDER, folder, &theme));
        }
        if let Some(branch) = card.branch.clone() {
            lines.push(line(icons::GIT_BRANCH, branch, &theme));
        }
        if let Some(agent) = card.agent.clone() {
            lines.push(line(icons::BOT, agent, &theme));
        }
        if let Some(status) = card.status.clone() {
            lines.push(line(icons::CLOCK_CIRCLE, status, &theme));
        }
        if let Some(subagents) = card.subagents.clone() {
            lines.push(line(icons::LIST, subagents, &theme));
        }
        let body = div()
            .id("sidebar-hover-card")
            .debug_selector(|| "sidebar-hover-card".into())
            .role(gpui::Role::Tooltip)
            .aria_label(card.title.clone())
            .aria_value(spoken)
            .w(px(CARD_WIDTH))
            .p(px(12.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(theme.border)
            .bg(crate::popover::surface_bg(&theme))
            .when(!theme.is_frost(), |el| el.shadow_lg())
            .flex()
            .flex_col()
            .gap(px(6.0))
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(theme.text_muted)
            .child(
                div()
                    .text_size(crate::typography::ui_rems(13.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .line_clamp(2)
                    .child(card.title.clone()),
            )
            .children(lines);
        // A gap from the pointer, then the card.
        div().pt(px(6.0)).child(crate::frost::frosted(12.0, crate::frost::MENU_BLUR, body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

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

    impl Render for SidebarHost {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let (shell, cx) = sidebar(cx, crate::settings::SidebarOrganization::InOneList);
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
