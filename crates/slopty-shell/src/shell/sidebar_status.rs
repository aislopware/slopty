//! The session list's model (`docs/decisions/layout.md`, "The session list is stable, and its
//! marks carry the attention"): the groups the sidebar draws, their rows in order, each row's
//! quiet hint, and what a row's hover card says.
//!
//! - **Groups.** Rows group by project: the repository across its checkouts and machines
//!   (`slopty_view_proto::view::project_key`), or the folder of a session with no project. The
//!   groups stand in the order of their names. Inside a group rows keep the order their sessions
//!   started in, the newest first (`docs/decisions/ui.md`), so a row never moves on its own: not
//!   when its state changes, not when it answers. Its state is its mark (`marks`), a need's
//!   glyph naming the need; the need's words are its hover card's and a screen reader's, never a
//!   line of the row (`docs/decisions/layout.md`, "A state is a glyph").
//! - **Inactive.** A session nobody has touched for [`INACTIVE_AFTER_HOURS`] that calls for
//!   nothing (at rest, or done) folds into Inactive at the foot, shut at first. Opened, it shows
//!   its first [`INACTIVE_FIRST`] rows, then "Show N more", which adds [`INACTIVE_PAGE`] at a
//!   time, as the Archived shelf does; the open chat always shows.
//! - **The hint.** A row is one line. After its title a quiet hint says only what tells it from
//!   the rows beside it: its machine while the list spans machines, its branch while its
//!   project's rows stand on more than one, and its project in Inactive, where projects mix.
//! - **Cached.** The row of a session whose machine is out of reach stays where it is, dimmed and
//!   marked as cached: what it shows is the last word heard from there.
//! - **The hover card** says what the row has no room for: where it runs, the agent and model,
//!   how long it has stood as it is, what it needs in the agent's words, the last line the agent
//!   wrote, and the subagents it has running.
//! - **The jump.** The key to the next session that needs the person walks every session that
//!   waits on them, then each that failed, then each finished unseen, across every project and
//!   machine, each kind oldest first ([`needing_you`]).

use chrono::{DateTime, Local, Utc};
use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px};
use slopty_view_proto::{Chat, ChatIndicator, Session, SessionStatus, SessionWait};

use super::marks::Mark;
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

/// How long a session that calls for nothing rests untouched before it folds into Inactive
/// (Amp's 72 h).
pub(super) const INACTIVE_AFTER_HOURS: i64 = 72;

/// The Inactive group's key.
pub(super) const INACTIVE: &str = "inactive";

/// The groups the sidebar opens with shut, by their keys: Inactive (Amp's).
pub(super) fn shut_at_first() -> std::collections::HashSet<String> {
    std::collections::HashSet::from([INACTIVE.to_owned()])
}

/// Rows Inactive shows before "Show more", as the Archived shelf's first page.
pub(super) const INACTIVE_FIRST: usize = 10;

/// Rows each "Show more" adds, as the Archived shelf's page.
pub(super) const INACTIVE_PAGE: usize = 25;

/// The Inactive group's header.
const INACTIVE_LABEL: &str = "Inactive";

/// The group of a session with neither project nor folder.
const NO_PROJECT: &str = "No project";

/// One group of the list: a project, a folder, or Inactive.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Group {
    /// Its key among the sidebar's groups: `project:…`, `folder:…`, or [`INACTIVE`].
    pub key: String,
    /// Its header.
    pub label: SharedString,
    pub rows: Vec<Row>,
    /// Rows that wait behind "Show more" (Inactive's alone, [`page`]).
    pub hidden: usize,
}

/// One row of the list.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Row {
    pub chat: Chat,
    /// Its display status, seen folded in (`AppState::display_status_for`).
    pub status: ChatIndicator,
    pub mark: Mark,
    /// The quiet hint after its title, when something tells the row from those beside it.
    pub hint: Option<SharedString>,
    /// Its machine is out of reach: the row shows the last word heard from there.
    pub cached: bool,
}

/// Why `device` turns this device away, in its own words (the login badge's tooltip); `None`
/// while it lets this device in. The badge retries and never asks for a credential.
pub(super) fn turned_away<'a>(state: &'a AppState, device: &str) -> Option<&'a str> {
    state.devices.iter().find(|d| d.id == device)?.turned_away.as_deref()
}

/// Whether `device` is out of reach: not heard lately, or turning this device away. Its rows
/// show the last word heard from there.
pub(super) fn out_of_reach(state: &AppState, device: &str, now: DateTime<Utc>) -> bool {
    !state.device_online(device, now) || turned_away(state, device).is_some()
}

/// What the sidebar narrows its list to: a project, a machine, both or neither (the machine is
/// a filter, not a level: `docs/decisions/layout.md`, "The session list is stable").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Filter<'a> {
    /// A space id: its whole project, every checkout on every machine
    /// (`AppState::project_filter`).
    pub project: Option<&'a str>,
    /// A device id.
    pub machine: Option<&'a str>,
}

impl Filter<'_> {
    /// Whether `chat` passes the filter's machine; its project is `AppState::project_filter`'s.
    pub(super) fn on_machine(&self, chat: &Chat) -> bool {
        self.machine.is_none_or(|machine| chat.device_id == machine)
    }
}

/// The list as it stands at `now`, narrowed to `filter`; and the soonest moment after `now` it
/// reads otherwise of time alone (a row folding into Inactive, a session going quiet), for the
/// sidebar's clock. A machine going out of reach is the state's own transition
/// (`AppState::next_clock_transition`).
pub(super) fn list(
    state: &AppState,
    filter: Filter<'_>,
    now: DateTime<Utc>,
) -> (Vec<Group>, Option<DateTime<Utc>>) {
    let mut chats = state.sidebar_chats(now, filter.project);
    chats.retain(|(_, chat)| filter.on_machine(chat));
    let mut machines: Vec<&str> = chats.iter().map(|(_, chat)| chat.device_id.as_str()).collect();
    machines.sort_unstable();
    machines.dedup();
    let spans_machines = machines.len() > 1;
    let mut next: Option<DateTime<Utc>> = None;
    let mut note = |at: DateTime<Utc>| {
        if at > now && next.is_none_or(|next| at < next) {
            next = Some(at);
        }
    };
    let mut groups: Vec<Group> = Vec::new();
    let mut inactive: Vec<(Row, SharedString)> = Vec::new();
    for (status, chat) in chats {
        let session = state.session_for(&chat.id);
        if let Some(session) = session {
            // `age_ms > SESSION_STALE_MS` first holds one millisecond past it.
            note(session.updated_at + chrono::TimeDelta::milliseconds(SESSION_STALE_MS + 1));
        }
        let mark = Mark::of(status, session, now);
        let row = Row {
            chat: chat.clone(),
            status,
            cached: out_of_reach(state, &chat.device_id, now),
            mark,
            hint: None,
        };
        let (key, label) = place(state, chat);
        let folds_at = touched(chat) + chrono::TimeDelta::hours(INACTIVE_AFTER_HOURS);
        if matches!(row.mark, Mark::AtRest | Mark::DoneUnseen) && folds_at <= now {
            inactive.push((row, label));
            continue;
        }
        note(folds_at);
        match groups.iter_mut().find(|group| group.key == key) {
            Some(group) => group.rows.push(row),
            None => groups.push(Group { key, label, rows: vec![row], hidden: 0 }),
        }
    }
    groups.sort_by(|left, right| {
        left.label.to_lowercase().cmp(&right.label.to_lowercase()).then_with(|| left.key.cmp(&right.key))
    });
    for group in &mut groups {
        group.rows.sort_by(|left, right| started(&left.chat, &right.chat));
        hints(state, &mut group.rows, spans_machines, None);
    }
    if !inactive.is_empty() {
        inactive.sort_by(|(left, _), (right, _)| started(&left.chat, &right.chat));
        let projects: Vec<SharedString> = inactive.iter().map(|(_, label)| label.clone()).collect();
        let mut rows: Vec<Row> = inactive.into_iter().map(|(row, _)| row).collect();
        hints(state, &mut rows, spans_machines, Some(&projects));
        groups.push(Group { key: INACTIVE.to_owned(), label: INACTIVE_LABEL.into(), rows, hidden: 0 });
    }
    (groups, next)
}

/// Keep of `group`'s rows those it draws while it shows `shown`: the first `shown`, and the
/// open chat `selected` wherever it falls; the rest wait behind "Show more" ([`Group::hidden`]).
pub(super) fn page(group: &mut Group, shown: usize, selected: Option<&str>) {
    let before = group.rows.len();
    let mut kept = 0_usize;
    group.rows.retain(|row| {
        kept = kept.saturating_add(1);
        kept <= shown || selected == Some(row.chat.id.as_str())
    });
    group.hidden = before.saturating_sub(group.rows.len());
}

/// The rows of a shelf whose `chats`, in its order, mix projects (Archived): each with its
/// hint as the list's rows have theirs, its project first while the shelf spans more
/// than one.
pub(super) fn shelf(state: &AppState, chats: Vec<Chat>, now: DateTime<Utc>) -> Vec<Row> {
    let mut machines: Vec<&str> = chats.iter().map(|chat| chat.device_id.as_str()).collect();
    machines.sort_unstable();
    machines.dedup();
    let spans_machines = machines.len() > 1;
    let projects: Vec<SharedString> = chats.iter().map(|chat| place(state, chat).1).collect();
    let mut rows: Vec<Row> = chats
        .iter()
        .map(|chat| {
            let status = state.display_status_for(chat, now);
            Row {
                chat: chat.clone(),
                status,
                mark: Mark::of(status, state.session_for(&chat.id), now),
                hint: None,
                cached: out_of_reach(state, &chat.device_id, now),
            }
        })
        .collect();
    hints(state, &mut rows, spans_machines, Some(&projects));
    rows
}

/// The group `chat` sits in, by key and header: its project, else its folder.
fn place(state: &AppState, chat: &Chat) -> (String, SharedString) {
    if let Some(space) = state.space_for_chat(chat) {
        let name = state.representative_space(space).display_name().to_owned();
        return (format!("project:{}", slopty_view_proto::view::project_key(space)), name.into());
    }
    let folder = chat
        .source_context
        .as_ref()
        .map(|context| context.cwd.as_str())
        .or(chat.cwd.as_deref())
        .map(|cwd| cwd.trim_end_matches(['/', '\\']))
        .filter(|cwd| !cwd.is_empty());
    match folder {
        Some(folder) => {
            let leaf = folder.rsplit(['/', '\\']).next().filter(|leaf| !leaf.is_empty()).unwrap_or(folder);
            (format!("folder:{folder}"), SharedString::from(leaf.to_owned()))
        }
        None => ("project:".to_owned(), NO_PROJECT.into()),
    }
}

/// The order sessions started in, the newest first; ties by id, so the order is total.
fn started(left: &Chat, right: &Chat) -> std::cmp::Ordering {
    super::spaces::compare_sidebar_chats(left, right)
}

/// When `chat` was last touched: its last message, else its start.
fn touched(chat: &Chat) -> DateTime<Utc> {
    chat.last_message_at.unwrap_or(chat.created_at)
}

/// Give each of a group's `rows` the hint that tells it from the others: its project
/// (`projects`, a shelf's or Inactive's, while they are not all one), its machine while the list
/// spans machines, and its branch while the group's rows stand on more than one.
fn hints(state: &AppState, rows: &mut [Row], spans_machines: bool, projects: Option<&[SharedString]>) {
    let mut branches: Vec<&str> = rows.iter().filter_map(|row| branch_of(&row.chat)).collect();
    branches.sort_unstable();
    branches.dedup();
    let spans_branches = branches.len() > 1;
    let projects = projects.filter(|projects| projects.iter().any(|project| project != &projects[0]));
    let lines: Vec<Option<SharedString>> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let mut parts: Vec<&str> = Vec::new();
            if let Some(project) = projects.and_then(|projects| projects.get(index)) {
                parts.push(project.as_ref());
            }
            if spans_machines && let Some(machine) = state.device_name(&row.chat.device_id) {
                parts.push(machine);
            }
            if spans_branches && let Some(branch) = branch_of(&row.chat) {
                parts.push(branch);
            }
            (!parts.is_empty()).then(|| SharedString::from(parts.join(" \u{b7} ")))
        })
        .collect();
    for (row, line) in rows.iter_mut().zip(lines) {
        row.hint = line;
    }
}

/// The branch `chat` stands on, as Slopty's own git read of its checkout says, trimmed and never
/// empty. The chat's own `branch` is never read: it may tell an older state of the checkout.
pub(super) fn branch_of(chat: &Chat) -> Option<&str> {
    chat.source_context
        .as_ref()
        .map(|source| source.branch.trim())
        .filter(|branch| !branch.is_empty())
}

/// The sessions the "Next session that needs you" key walks, by chat id: every one that waits
/// on the person, then each that failed, then each finished unseen, across every project and
/// machine; each kind oldest first, by when it began to call.
pub(super) fn needing_you(state: &AppState, now: DateTime<Utc>) -> Vec<String> {
    let mut calls: Vec<(u8, DateTime<Utc>, &Chat)> = state
        .overview_chats(now)
        .into_iter()
        .filter_map(|(status, chat)| {
            let session = state.session_for(&chat.id);
            let mark = Mark::of(status, session, now);
            let since = match mark {
                Mark::NeedsYou(_) => session.and_then(|session| session.status_since),
                Mark::Failed | Mark::DoneUnseen => None,
                Mark::AtWork | Mark::AtRest => return None,
            };
            Some((mark.urgency(), since.unwrap_or_else(|| touched(chat)), chat))
        })
        .collect();
    calls.sort_by(|left, right| {
        right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)).then_with(|| left.2.id.cmp(&right.2.id))
    });
    calls.into_iter().map(|(_, _, chat)| chat.id.clone()).collect()
}

impl super::Shell {
    /// The key to the next session that needs the person ([`needing_you`]): the one after the
    /// open one, round the end. Whether there was one to open.
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

/// The "Show N more" line under Inactive while `hidden` rows wait, `height` tall, drawn as the
/// Archived shelf draws its own.
fn show_more_row(id: SharedString, hidden: usize, height: f32, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    let id_text = id.to_string();
    let words = SharedString::from(format!("Show {} more", hidden.min(INACTIVE_PAGE)));
    div()
        .id(id)
        .debug_selector(move || id_text)
        .h(px(height))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        // At the rows' title edge, past the one leading slot.
        .gap(px(super::sidebar::SIDEBAR_LEADING_GAP))
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
                .size(px(super::sidebar::SIDEBAR_LEADING_SLOT))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(icon(icons::PLUS).size(px(14.0)).flex_none()),
        )
        .child(words)
}

impl super::Shell {
    /// The "Show more" line under the group `key`, when `hidden` rows wait: its key, its height
    /// with the list's gap above it, and the line, which shows another page.
    pub(super) fn render_group_more(
        &self,
        key: &str,
        hidden: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<(String, f32, AnyElement)> {
        if hidden == 0 {
            return None;
        }
        // As the Archived shelf's: a row's height.
        let height = super::sidebar::SIDEBAR_ROW_HEIGHT;
        let shown_key = key.to_owned();
        let id = SharedString::from(format!("sidebar-group-more-{key}"));
        let element = show_more_row(id, hidden, height, theme)
            .on_click(cx.listener(move |this, _, _, cx| {
                let shown = this.sidebar_group_shown.get(&shown_key).copied().unwrap_or(INACTIVE_FIRST);
                this.sidebar_group_shown.insert(shown_key.clone(), shown.saturating_add(INACTIVE_PAGE));
                cx.notify();
            }))
            .into_any_element();
        Some((format!("more:{key}"), height + super::SIDEBAR_LIST_GAP, element))
    }
}

/// `session`, unless it has gone quiet past the staleness window (its machine stopped saying).
fn live(session: Option<&Session>, now: DateTime<Utc>) -> Option<&Session> {
    session.filter(|s| now.signed_duration_since(s.updated_at).num_milliseconds() <= SESSION_STALE_MS)
}

/// What a row's hover card says beyond zeron's, line by line.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ChatCard {
    /// "project @ machine", or "@ machine" for a chat with no project.
    pub place: Option<SharedString>,
    /// Its folder, its middle elided when long.
    pub folder: Option<SharedString>,
    /// "Claude Code · opus · High"
    pub agent: Option<SharedString>,
    /// "Working for 3m", "Waiting on you since 14:02", "Done 5m ago", "Cached · studio is out of
    /// reach"
    pub status: SharedString,
    /// What it needs of the person, its glyph and its agent's words (`marks::need`).
    pub need: Option<super::marks::Need>,
    /// The last line the agent wrote.
    pub last: Option<SharedString>,
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
    let cached = out_of_reach(state, &chat.device_id, now);
    let need = match Mark::of(status, session, now) {
        Mark::NeedsYou(need) if !cached => Some(need),
        _ => None,
    };
    let last = chat
        .last_message_preview
        .as_deref()
        .map(crate::transcript::single_line)
        .filter(|line| !line.trim().is_empty())
        .map(SharedString::from);
    let subagents = state.running_subagents_for(chat_id, now);
    let status = if cached {
        // What the row shows is the last word from there, and why.
        let machine = device.unwrap_or("its machine");
        match turned_away(state, &chat.device_id) {
            Some(why) => format!("Cached \u{b7} {machine} is {why}"),
            None => format!("Cached \u{b7} {machine} is out of reach"),
        }
    } else {
        status_words(chat, status, session, now)
    };
    Some(ChatCard {
        place: place.map(SharedString::from),
        folder: folder.map(SharedString::from),
        agent: agent.map(SharedString::from),
        status: status.into(),
        need,
        last,
        subagents: (subagents > 0 && !cached).then(|| {
            SharedString::from(if subagents == 1 {
                "1 subagent running".to_owned()
            } else {
                format!("{subagents} subagents running")
            })
        }),
    })
}

/// When `at` was, in the person's time on the system's clock: "2:05 PM" today, "Oct 16,
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
fn status_words(chat: &Chat, status: ChatIndicator, session: Option<&Session>, now: DateTime<Utc>) -> String {
    let session = live(session, now);
    let since = session.and_then(|s| s.status_since.or(s.started_at));
    let ago = |at: DateTime<Utc>| format_time_ago(at, now);
    let last = touched(chat);
    // A relative time also says when, which "2d ago" alone never does.
    let when = |words: String| format!("{words} \u{b7} {}", absolute_time(last, now));
    match status {
        ChatIndicator::Working
            if session
                .and_then(|s| s.waiting_on.as_ref())
                .is_some_and(|wait| wait.kind == SessionWait::COMPACTING) =>
        {
            "Compacting its context".to_owned()
        }
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
        ChatIndicator::Completed | ChatIndicator::Idle if super::marks::works_on_its_own(session, now) => {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::marks::Need;
    use crate::loaders::status_mark::MarkState;
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
            device_id: "local".into(),
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

    /// Chat `id` in space `space` (none for a session with no project), started `started` minutes
    /// before `now` and last touched `touched` minutes before it, seen unless `unseen`.
    fn chat(id: &str, space: Option<&str>, started: i64, touched: i64, now: DateTime<Utc>, unseen: bool) -> Chat {
        let last = now - TimeDelta::minutes(touched);
        serde_json::from_value(serde_json::json!({
            "id": id, "title": id, "deviceId": "local", "archived": false,
            "spaceId": space,
            "createdAt": now - TimeDelta::minutes(started),
            "lastMessageAt": last,
            "lastSeenAt": if unseen { last - TimeDelta::seconds(1) } else { last },
        }))
        .unwrap()
    }

    fn space(id: &str, name: &str, repository: Option<&str>, device: &str) -> slopty_view_proto::Space {
        serde_json::from_value(serde_json::json!({
            "id": id, "deviceId": device, "path": format!("/src/{name}"), "name": name,
            "repositoryId": repository, "createdAt": Utc::now() - TimeDelta::days(30),
        }))
        .unwrap()
    }

    fn device(id: &str, seen: Option<DateTime<Utc>>) -> slopty_view_proto::Device {
        serde_json::from_value(serde_json::json!({ "id": id, "name": id, "platform": "macos", "lastSeenAt": seen }))
            .unwrap()
    }

    /// Two projects, one of them checked out twice (its clones are one project), and a session
    /// with no project in a folder of its own.
    fn state(now: DateTime<Utc>) -> AppState {
        let mut state = AppState::new();
        state.local_device_id = Some("local".into());
        state.spaces = vec![
            space("s-zed", "zed", None, "local"),
            space("s-app", "app", Some("repo-app"), "local"),
            space("s-app-2", "app-worktree", Some("repo-app"), "local"),
        ];
        let mut loose = chat("loose", None, 50, 1, now, false);
        loose.cwd = Some("/tmp/scratch/".into());
        state.chats = vec![
            chat("app-new", Some("s-app-2"), 10, 1, now, false),
            chat("zed-old", Some("s-zed"), 60, 1, now, false),
            chat("app-old", Some("s-app"), 40, 1, now, true),
            loose,
            chat("zed-new", Some("s-zed"), 5, 1, now, false),
        ];
        state
    }

    fn keys(groups: &[Group]) -> Vec<(String, Vec<String>)> {
        groups
            .iter()
            .map(|group| (group.label.to_string(), group.rows.iter().map(|row| row.chat.id.clone()).collect()))
            .collect()
    }

    /// Rows group by project (a repository's checkouts are one), a session with no project by its
    /// folder, the groups by name; inside a group rows keep the order their sessions started in,
    /// the newest first, and no state, answer or republish moves one.
    #[test]
    fn rows_group_by_project_in_the_order_their_sessions_started() {
        let now = Utc::now();
        let mut state = state(now);
        let order = |state: &AppState| keys(&list(state, Filter::default(), now).0);
        let expected = vec![
            ("app".to_owned(), vec!["app-new".to_owned(), "app-old".to_owned()]),
            ("scratch".to_owned(), vec!["loose".to_owned()]),
            ("zed".to_owned(), vec!["zed-new".to_owned(), "zed-old".to_owned()]),
        ];
        assert_eq!(order(&state), expected);
        let (groups, _) = list(&state, Filter::default(), now);
        assert_eq!(groups[0].rows[1].mark, Mark::DoneUnseen, "its state is its mark");
        // Every row's state changes, the oldest answers: nothing moves.
        state.apply_sessions(vec![
            session("zed-old", SessionStatus::Working, now),
            session("app-new", SessionStatus::AwaitingInput, now),
            session("loose", SessionStatus::Waiting, now),
        ]);
        state.chats.iter_mut().for_each(|chat| chat.last_message_at = Some(now));
        assert_eq!(order(&state), expected);
        // A session started now leads its project.
        state.chats.push(chat("app-newest", Some("s-app"), 0, 0, now, false));
        assert_eq!(order(&state)[0].1, ["app-newest", "app-new", "app-old"]);
        // The project filter narrows the list to one project, its clones in it.
        let app = Filter { project: Some("s-app"), machine: None };
        assert_eq!(keys(&list(&state, app, now).0)[0].1, ["app-newest", "app-new", "app-old"]);
        assert_eq!(list(&state, app, now).0.len(), 1);
    }

    /// A session nobody touched for 72 hours folds into Inactive at the foot while it calls for
    /// nothing: at rest or done. One that needs the person, failed or works stays where it lives;
    /// the list says when the next one folds.
    #[test]
    fn a_session_untouched_for_three_days_folds_into_inactive_while_it_calls_for_nothing() {
        let now = Utc::now();
        let mut state = state(now);
        let days = |n: i64| n * 24 * 60;
        state.chats = vec![
            chat("rested", Some("s-zed"), days(5), days(3), now, false),
            chat("unseen", Some("s-zed"), days(5), days(4), now, true),
            chat("asking", Some("s-zed"), days(5), days(4), now, false),
            chat("busy", Some("s-zed"), days(5), days(4), now, false),
            chat("fresh", Some("s-app"), days(5), days(3) - 30, now, false),
        ];
        let mut asking = session("asking", SessionStatus::AwaitingInput, now);
        asking.waiting_on = wait(SessionWait::QUESTION, None);
        state.apply_sessions(vec![asking, session("busy", SessionStatus::Working, now)]);
        let (groups, next) = list(&state, Filter::default(), now);
        assert_eq!(
            keys(&groups),
            [
                ("app".to_owned(), vec!["fresh".to_owned()]),
                ("zed".to_owned(), vec!["asking".to_owned(), "busy".to_owned()]),
                ("Inactive".to_owned(), vec!["rested".to_owned(), "unseen".to_owned()]),
            ]
        );
        assert_eq!(groups[2].key, INACTIVE);
        let fresh = state.chats.iter().find(|chat| chat.id == "fresh").unwrap();
        let folds = fresh.last_message_at.unwrap() + TimeDelta::hours(INACTIVE_AFTER_HOURS);
        assert!(next.is_some_and(|next| next <= folds), "the clock wakes when it folds: {next:?}");
        let (groups, _) = list(&state, Filter::default(), folds);
        assert!(groups.iter().any(|group| group.key == INACTIVE && group.rows.iter().any(|row| row.chat.id == "fresh")));
    }

    /// Inactive pages its rows as the Archived shelf does, the open chat shown wherever it falls.
    #[test]
    fn inactive_pages_its_rows_with_the_open_chat_kept() {
        let now = Utc::now();
        let rows: Vec<Row> = (0..30)
            .map(|n| Row {
                chat: chat(&format!("c{n:02}"), None, 10_000 - n, 9_000, now, false),
                status: ChatIndicator::Idle,
                mark: Mark::AtRest,
                hint: None,
                cached: false,
            })
            .collect();
        let mut group = Group { key: INACTIVE.into(), label: "Inactive".into(), rows, hidden: 0 };
        page(&mut group, INACTIVE_FIRST, Some("c29"));
        assert_eq!(group.rows.len(), INACTIVE_FIRST + 1, "its first page and the open chat");
        assert_eq!(group.hidden, 30 - INACTIVE_FIRST - 1);
        assert_eq!(group.rows.last().map(|row| row.chat.id.as_str()), Some("c29"));
    }

    /// The hint says only what tells a row from those beside it: its machine while the list
    /// spans machines, its branch while its project's rows stand on more than one, its project
    /// where projects mix (Inactive, Archived); with nothing to tell, a row says its title alone.
    /// Narrowed to one machine, the list holds that machine's rows alone.
    #[test]
    fn the_hint_says_only_what_tells_rows_apart() {
        let now = Utc::now();
        let mut state = state(now);
        let hint = |state: &AppState, id: &str| {
            let (groups, _) = list(state, Filter::default(), now);
            groups.into_iter().flat_map(|group| group.rows).find(|row| row.chat.id == id).and_then(|row| row.hint)
        };
        assert_eq!(hint(&state, "zed-old"), None, "one machine, one branch: the title alone");
        state.devices = vec![device("local", Some(now)), device("studio", Some(now))];
        state.chats.iter_mut().find(|chat| chat.id == "zed-new").unwrap().device_id = "studio".into();
        assert_eq!(hint(&state, "zed-new").as_deref(), Some("studio"));
        assert_eq!(hint(&state, "zed-old").as_deref(), Some("local"));
        // The machine is a filter, not a level: narrowed to one, the list holds its rows alone,
        // in their projects, and no row says the machine every row shares.
        let studio = Filter { project: None, machine: Some("studio") };
        let (groups, _) = list(&state, studio, now);
        assert_eq!(keys(&groups), [("zed".to_owned(), vec!["zed-new".to_owned()])]);
        assert_eq!(groups[0].rows[0].hint, None, "one machine shown: no machine said");
        let both = Filter { project: Some("s-app"), machine: Some("studio") };
        assert!(list(&state, both, now).0.is_empty(), "a project with nothing on the machine shows nothing");
        state.chats.iter_mut().find(|chat| chat.id == "zed-new").unwrap().device_id = "local".into();
        let context = |branch: &str| -> slopty_view_proto::ConversationSourceContext {
            serde_json::from_value(serde_json::json!({
                "checkoutId": "k", "repoRoot": "/src/app", "cwd": "/src/app", "branch": branch,
                "observedAt": now,
            }))
            .unwrap()
        };
        for (id, branch) in [("app-old", "main"), ("app-new", "lexer")] {
            state.chats.iter_mut().find(|chat| chat.id == id).unwrap().source_context = Some(context(branch));
        }
        assert_eq!(hint(&state, "app-new").as_deref(), Some("lexer"), "two branches in one project");
        assert_eq!(hint(&state, "zed-old"), None, "its own project stands on one");
        let rows = shelf(&state, state.chats.clone(), now);
        let said: Vec<Option<&str>> = rows.iter().map(|row| row.hint.as_deref()).collect();
        assert_eq!(
            said,
            [Some("app \u{b7} lexer"), Some("zed"), Some("app \u{b7} main"), Some("scratch"), Some("zed")],
            "a shelf of projects says each, and the branches it spans"
        );
    }

    /// A session whose machine is out of reach keeps its row, cached: its card says so, and asks
    /// nothing in its name; back in reach, it is live again.
    #[test]
    fn a_row_whose_machine_is_out_of_reach_is_cached() {
        let now = Utc::now();
        let mut state = state(now);
        state.devices = vec![device("local", Some(now)), device("studio", Some(now - TimeDelta::minutes(5)))];
        state.chats.iter_mut().find(|chat| chat.id == "zed-new").unwrap().device_id = "studio".into();
        let mut asking = session("zed-new", SessionStatus::AwaitingInput, now);
        asking.waiting_on = wait(SessionWait::APPROVAL, Some("run cargo test"));
        state.apply_sessions(vec![asking]);
        let cached = |state: &AppState| {
            let (groups, _) = list(state, Filter::default(), now);
            groups.into_iter().flat_map(|group| group.rows).filter(|row| row.cached).map(|row| row.chat.id).collect::<Vec<_>>()
        };
        assert_eq!(cached(&state), ["zed-new"], "its row stays, cached");
        let card = chat_card(&state, "zed-new", now).unwrap();
        assert_eq!(card.status.as_ref(), "Cached \u{b7} studio is out of reach");
        assert_eq!(card.need, None, "no need said from a machine out of reach");
        state.devices[1].last_seen_at = Some(now);
        assert!(cached(&state).is_empty());
        let card = chat_card(&state, "zed-new", now).unwrap();
        let need = card.need.expect("its need, live again");
        assert_eq!((need.glyph, need.words.as_ref()), (MarkState::Approval, "Needs approval \u{b7} run cargo test"));
    }

    /// The card says what the row has no room for: the need in the agent's words, with the glyph
    /// its row wears for it, and the last line it wrote; what an agent at work or on its own work
    /// does is its status.
    #[test]
    fn the_card_says_the_need_and_the_last_words() {
        let now = Utc::now();
        let mut state = state(now);
        let mut asking = session("zed-old", SessionStatus::AwaitingInput, now);
        asking.waiting_on = wait(SessionWait::QUESTION, None);
        state.apply_sessions(vec![asking.clone()]);
        state.chats.iter_mut().find(|chat| chat.id == "zed-old").unwrap().last_message_preview =
            Some("Which accent reads best\non glass?".into());
        let card = chat_card(&state, "zed-old", now).unwrap();
        assert_eq!(card.need, Some(Need { glyph: MarkState::Question, words: "Has a question".into() }));
        assert_eq!(card.last.as_deref(), Some("Which accent reads best on glass?"));
        assert!(card.status.starts_with("Waiting on you since"), "{}", card.status);
        asking.waiting_on = wait(SessionWait::APPROVAL, Some("run cargo test"));
        state.apply_sessions(vec![asking.clone()]);
        let need = chat_card(&state, "zed-old", now).unwrap().need.unwrap();
        assert_eq!((need.glyph, need.words.as_ref()), (MarkState::Approval, "Needs approval \u{b7} run cargo test"));
        asking.waiting_on = None;
        state.apply_sessions(vec![asking]);
        assert_eq!(chat_card(&state, "zed-old", now).unwrap().need.map(|need| need.words), Some("Needs input".into()));
        let mut busy = session("zed-old", SessionStatus::Working, now);
        busy.waiting_on = wait(SessionWait::COMPACTING, None);
        state.apply_sessions(vec![busy]);
        let compacting = chat_card(&state, "zed-old", now).unwrap();
        assert_eq!((compacting.status.as_ref(), compacting.need), ("Compacting its context", None));
        state.apply_sessions(vec![session("zed-old", SessionStatus::Waiting, now)]);
        let resting = chat_card(&state, "zed-old", now).unwrap();
        assert_eq!((resting.status.as_ref(), resting.need), ("Waiting on its own work for 4m", None));
        let rested = chat_card(&state, "zed-new", now).unwrap();
        assert_eq!((rested.need, rested.last), (None, None));
    }

    /// The jump walks every session that needs the person, then each that failed, then each
    /// finished unseen, each kind oldest first; one at work or at rest is never one.
    #[test]
    fn the_jump_walks_needs_you_then_failed_then_unseen_oldest_first() {
        let now = Utc::now();
        let mut state = state(now);
        let since = |chat: &str, minutes: i64| {
            let mut asking = session(chat, SessionStatus::AwaitingInput, now);
            asking.status_since = Some(now - TimeDelta::minutes(minutes));
            asking
        };
        state.chats.push(chat("done-late", Some("s-zed"), 3, 1, now, true));
        state.apply_sessions(vec![since("zed-new", 2), since("loose", 9), session("zed-old", SessionStatus::Working, now)]);
        // `app-old` finished unseen 1 minute ago, `done-late` too; a tie goes by id.
        assert_eq!(needing_you(&state, now), ["loose", "zed-new", "app-old", "done-late"]);
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

    /// A sidebar over [`state`]'s chats, one of them asking, one at work.
    fn sidebar(cx: &mut gpui::TestAppContext) -> (gpui::Entity<super::super::Shell>, &mut gpui::VisualTestContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let path = dir.path().to_path_buf();
        let (host, cx) = cx.add_window_view(move |_, cx| {
            SidebarHost(cx.new(|cx| {
                let now = Utc::now();
                let state = cx.new(|_| state(now));
                let shell = super::super::Shell::new(
                    state,
                    crate::state::EngineBootConfig {
                        data_dir: path.clone(),
                        default_harness: slopty_view_proto::HarnessId::MOCK,
                    },
                    cx,
                );
                crate::motion::set_reduced_motion(cx, true);
                shell.state.update(cx, |state, _| {
                    state.workspace_scope = Some(slopty_view_proto::WorkspaceScope::Local);
                    let mut asking = session("zed-old", SessionStatus::AwaitingInput, now);
                    asking.waiting_on = wait(SessionWait::QUESTION, None);
                    state.apply_sessions(vec![asking, session("app-new", SessionStatus::Working, now)]);
                    for chat in &mut state.chats {
                        chat.config = Some(
                            serde_json::from_value(serde_json::json!({ "harness": "claude-code", "sandbox": "workspace-write" }))
                                .expect("a chat config"),
                        );
                    }
                });
                shell
            }))
        });
        std::mem::forget(dir);
        let shell = host.read_with(cx, |host, _| host.0.clone());
        (shell, cx)
    }

    fn redraw(cx: &mut gpui::VisualTestContext) {
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
    }

    fn bounds(cx: &mut gpui::VisualTestContext, selector: &str) -> Option<gpui::Bounds<gpui::Pixels>> {
        cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
    }

    /// A row is one line, about 28 pt: the program's mark leads, the title follows with its quiet
    /// hint, the state's mark trails in a slot kept while at rest, so every title starts and ends
    /// at one edge.
    #[gpui::test]
    fn a_row_is_one_line_of_program_title_and_mark(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = sidebar(cx);
        redraw(cx);
        let ids = ["zed-old", "zed-new", "app-old", "app-new", "loose"];
        let mut edges = Vec::new();
        for id in ids {
            let row = bounds(cx, &format!("chat-{id}")).unwrap_or_else(|| panic!("{id}'s row"));
            assert_eq!(f32::from(row.size.height), super::super::sidebar::SIDEBAR_ROW_HEIGHT, "{id}: one line");
            let program = bounds(cx, &format!("chat-agent-{id}")).expect("its program's mark");
            let title = bounds(cx, &format!("chat-title-{id}")).expect("its title");
            let slot = bounds(cx, &format!("chat-status-{id}")).expect("its mark's slot");
            assert!(program.right() <= title.left() && title.right() <= slot.left(), "{id}: program, title, mark");
            edges.push((title.left(), slot.left()));
        }
        assert!(edges.iter().all(|edge| *edge == edges[0]), "every title at one edge: {edges:?}");
        for (id, marked) in [("zed-old", true), ("app-new", true), ("app-old", true), ("zed-new", false), ("loose", false)] {
            assert_eq!(bounds(cx, &format!("chat-mark-{id}")).is_some(), marked, "{id}'s mark");
        }
        // Across two machines each row's hint says its own, after its title on the same line.
        assert!(bounds(cx, "chat-hint-zed-new").is_none(), "one machine: nothing to tell apart");
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                let now = Utc::now();
                state.devices = vec![device("local", Some(now)), device("studio", Some(now))];
                state.chats.iter_mut().find(|chat| chat.id == "zed-new").unwrap().device_id = "studio".into();
                cx.notify();
            });
        });
        redraw(cx);
        let hint = bounds(cx, "chat-hint-zed-new").expect("its machine, after its title");
        let title = bounds(cx, "chat-title-zed-new").unwrap();
        let slot = bounds(cx, "chat-status-zed-new").unwrap();
        assert!(title.left() < hint.left() && hint.right() <= slot.left(), "{title:?} {hint:?} {slot:?}");
        let row = bounds(cx, "chat-zed-new").unwrap();
        assert_eq!(f32::from(row.size.height), super::super::sidebar::SIDEBAR_ROW_HEIGHT, "still one line");
    }

    // A screen reader hears a waiting row say "Needs you", then what it needs in its
    // agent's words, as the status's description; no node of the row is named by those words,
    // which stay off the row as its glyph says them (`docs/decisions/layout.md`, "A state is a
    // glyph").
    #[gpui::test]
    fn a_waiting_rows_need_is_its_status_description(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = sidebar(cx);
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                let now = Utc::now();
                let mut asking = session("zed-old", SessionStatus::AwaitingInput, now);
                asking.waiting_on = wait(SessionWait::QUESTION, None);
                let mut held = session("zed-new", SessionStatus::AwaitingInput, now);
                held.waiting_on = wait(SessionWait::APPROVAL, Some("run cargo test"));
                state.apply_sessions(vec![asking, held]);
                cx.notify();
            });
        });
        for _ in 0..3 {
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.set_a11y_active(true);
                window.refresh();
                window.draw(cx).clear(cx);
            });
        }
        let nodes: Vec<(gpui::Role, String, String)> = cx.update(|window, _| {
            let tree = window.a11y_tree().expect("a tree");
            tree.nodes
                .iter()
                .map(|(_, node)| {
                    let label = node.label().unwrap_or_default().to_owned();
                    (node.role(), label, node.description().unwrap_or_default().to_owned())
                })
                .collect()
        });
        let words = "Needs approval \u{b7} run cargo test";
        let said = |description: &str| {
            nodes.iter().any(|(role, label, said)| {
                *role == gpui::Role::Status && label == "Needs you" && said == description
            })
        };
        assert!(said(words), "the approval: {nodes:?}");
        assert!(said("Has a question"), "the question: {nodes:?}");
        assert!(
            !nodes.iter().any(|(_, label, _)| label.starts_with("Needs approval") || label == "Has a question"),
            "no node named by a need's words: {nodes:?}"
        );
    }

    /// A row's state changing never moves it: an asking row done, a done row asking, the one at
    /// work at rest.
    #[gpui::test]
    fn a_rows_state_changing_never_moves_it(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = sidebar(cx);
        redraw(cx);
        let ids = ["app-new", "app-old", "loose", "zed-new", "zed-old"];
        let tops = |cx: &mut gpui::VisualTestContext| -> Vec<f32> {
            ids.iter().map(|id| f32::from(bounds(cx, &format!("chat-{id}")).unwrap().origin.y)).collect()
        };
        let before = tops(cx);
        assert!(before.windows(2).all(|pair| pair[0] < pair[1]), "top to bottom: {before:?}");
        for round in 0..4_usize {
            shell.update(cx, |shell, cx| {
                shell.state.update(cx, |state, cx| {
                    let now = Utc::now() + TimeDelta::seconds(i64::try_from(round).unwrap());
                    let statuses = [SessionStatus::Done, SessionStatus::AwaitingInput, SessionStatus::Working];
                    let sessions = ids
                        .iter()
                        .enumerate()
                        .map(|(index, id)| session(id, statuses[(index + round) % 3], now))
                        .collect();
                    state.apply_sessions(sessions);
                    for chat in &mut state.chats {
                        chat.last_message_at = Some(now);
                    }
                    cx.notify();
                });
            });
            redraw(cx);
            assert_eq!(tops(cx), before, "round {round} moved a row");
        }
    }

    /// A shut project wears its loudest row's mark and how many rows it holds, and a screen reader
    /// hears both; its rows leave the keys' order. Opened, its rows come back.
    #[gpui::test]
    fn a_shut_project_wears_its_loudest_mark_and_a_count(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = sidebar(cx);
        cx.update(|window, _| {
            window.activate_window();
            window.set_a11y_active(true);
        });
        redraw(cx);
        let (zed, app) = ("project:s-zed", "project:repo:repo-app");
        assert!(bounds(cx, &format!("sidebar-group-mark-{zed}")).is_none(), "open, its rows say it");
        for key in [zed, app] {
            let header = bounds(cx, &format!("sidebar-group-{key}")).expect("its header");
            cx.simulate_click(header.center(), gpui::Modifiers::default());
            redraw(cx);
            assert!(bounds(cx, &format!("sidebar-group-mark-{key}")).is_some(), "{key}: its loudest mark");
            assert!(bounds(cx, &format!("sidebar-group-count-{key}")).is_some(), "{key}: its count");
        }
        // Past the fold's motion, a shut group builds no rows.
        cx.executor().advance_clock(std::time::Duration::from_millis(400));
        redraw(cx);
        assert!(bounds(cx, "chat-zed-old").is_none() && bounds(cx, "chat-app-old").is_none(), "shut");
        let heard = |cx: &mut gpui::VisualTestContext, label: &str| {
            cx.update(|window, _| {
                let tree = window.a11y_tree().expect("the tree built");
                tree.nodes.iter().any(|(_, node)| node.label() == Some(label))
            })
        };
        // Needs you over at rest; done unseen over at work.
        assert!(heard(cx, "zed, 2, Needs you"), "zed's header says what it wears");
        assert!(heard(cx, "app, 2, Done"), "app's header says what it wears");
        let order = shell.update(cx, |shell, cx| shell.sidebar_visible_order(cx));
        assert_eq!(order, ["loose"], "a shut group's rows take no key");
        let header = bounds(cx, &format!("sidebar-group-{zed}")).unwrap();
        cx.simulate_click(header.center(), gpui::Modifiers::default());
        redraw(cx);
        assert!(bounds(cx, "chat-zed-old").is_some(), "open again");
        assert!(bounds(cx, &format!("sidebar-group-mark-{zed}")).is_none(), "its rows say it again");
    }

    /// The key walks the sessions that need the person, then the one finished unseen, and comes
    /// round again; with nobody waiting it opens nothing.
    #[gpui::test]
    fn the_needs_you_key_walks_the_waiting_then_the_unseen(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = sidebar(cx);
        let mut opened = Vec::new();
        for _ in 0..3 {
            shell.update(cx, |shell, cx| assert!(shell.open_next_needing_you(cx)));
            opened.push(shell.read_with(cx, |shell, cx| shell.state.read(cx).selected_chat.clone().unwrap()));
        }
        assert_eq!(opened, ["zed-old", "app-old", "zed-old"]);
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| state.chats.retain(|chat| chat.id == "app-new"));
            assert!(!shell.open_next_needing_you(cx));
        });
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
        let slot = shell.update(cx, |shell, cx| shell.sidebar_visible_order(cx)).iter().position(|id| id == "b");
        let key = format!("cmd-{}", slot.expect("b's row") + 1);
        cx.simulate_keystrokes(&key);
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

    /// A shell window with `count` chats at rest in its sidebar, the newest first: c00, c01…
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
            crate::composer::init(cx);
            super::super::apply_keymap(cx, &crate::settings::KeymapConfig::default());
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
            let count = i64::try_from(count).unwrap();
            let chats = (0..count).map(|n| chat(&format!("c{n:02}"), None, n, 1, now, false));
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
        let held = if crate::keys::MOD_IS_COMMAND {
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
        assert!(cx.debug_bounds("chat-mark-c00").is_none() && cx.debug_bounds("chat-status-c00").is_none(), "the key in the mark's place");
        cx.simulate_modifiers_change(gpui::Modifiers::default());
        redraw(cx);
        assert!(cx.debug_bounds("jump-hint-c00").is_none(), "gone once let go");
        cx.simulate_keystrokes(&crate::settings::platform_combo("mod-2"));
        let open = shell.read_with(cx, |shell, cx| shell.state.read(cx).selected_chat.clone());
        assert_eq!(open.as_deref(), Some("c01"), "the second row's key opens it");
    }

    /// Inactive, opened, shows its first page, then "Show N more"; pressed, every row, and no key
    /// skips a row it does not draw.
    #[gpui::test]
    fn inactive_shows_more_on_request(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = shell_with_chats(cx, 14);
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                for chat in &mut state.chats {
                    chat.last_message_at = Some(Utc::now() - TimeDelta::days(4));
                    chat.last_seen_at = chat.last_message_at;
                }
            });
            shell.sidebar_collapsed_groups.remove(INACTIVE);
        });
        redraw(cx);
        let more = format!("sidebar-group-more-{INACTIVE}");
        assert!(bounds(cx, "chat-c09").is_some() && bounds(cx, "chat-c10").is_none(), "the first page");
        let order = shell.update(cx, |shell, cx| shell.sidebar_visible_order(cx));
        assert_eq!(order.len(), INACTIVE_FIRST, "keys reach only the rows drawn: {order:?}");
        let more = bounds(cx, &more).expect("Show more").center();
        cx.simulate_click(more, gpui::Modifiers::default());
        redraw(cx);
        assert!((10..14).all(|n| bounds(cx, &format!("chat-c{n:02}")).is_some()), "every row now");
        assert_eq!(shell.update(cx, |shell, cx| shell.sidebar_visible_order(cx)).len(), 14);
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
