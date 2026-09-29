//! The bell's inbox, kept as a mailbox: what waits on the human and what finished while they
//! looked away, with the history of what was already read.
//!
//! Two views. *Unread* lists the agents waiting (*Needs you*) and the long commands that ended
//! unwatched and are still badged on their headers (*Finished*); *All* keeps every command the
//! inbox ever took, newest first, read or not. A row is two lines on the navigator's rhythm:
//! the command or the agent's words with its age on the right, which swaps for a mark-read
//! button under the pointer; then what the section heading does not already say (a failed
//! command's exit), how long it took, its tile, worker and directory. Reading is the header's badge
//! going: a row marked read, "Mark all read", or its tile looked at. The history holds the
//! last [`LOG_MAX`] commands.
//!
//! An agent that waits on a yes or no held for this client ([`approvals`]) gets "Deny" and
//! "Allow" at the end of its row's second line, which answer it where it is.

pub(super) mod approvals;

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, Div, ElementId, InteractiveElement as _, IntoElement as _, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use slopty_client::layout::WorkerKey;
use slopty_core::SessionId;
use slopty_proto::conversation::Verdict;
use slopty_theme::{Theme, alpha};

use super::agents::{agent_ask_line, agent_status_word};
use super::{Finished, WorkspaceView};
use crate::a11y::tab_stop;
use crate::colors::hsla;
use crate::icons::{IconName, IconSize, Status, icon, status_mark};
use crate::kit::{self, meta, tabular};
use crate::palette::{Plate, age_label, quiet_line, section_heading};

/// The popover's width, in points.
const INBOX_W: f32 = 360.0;

/// The most the list shows before it scrolls, in points.
const INBOX_MAX_H: f32 = 480.0;

/// How many finished commands the history keeps: more than a day of long builds, and a bound.
pub(super) const LOG_MAX: usize = 200;

/// What the empty inbox says.
pub(super) const ALL_CAUGHT_UP: &str = "You're all caught up";

/// What an inbox that has never held anything adds: what lands in it.
pub(super) const INBOX_HOLDS: &str =
    "Agents that need you and long commands that finish while you look away land here.";

/// The button that reads every unread row.
pub(super) const MARK_ALL_READ: &str = "Mark all read";

/// The button that allows a waiting agent's call.
pub(super) const ALLOW: &str = "Allow";

/// The button that refuses it.
pub(super) const DENY: &str = "Deny";

/// One finished command as the history keeps it: what ran, where, and when it ended.
#[derive(Debug)]
struct Logged {
    seq: u64,
    session: SessionId,
    worker: Option<WorkerKey>,
    cwd: Option<String>,
    done: Finished,
    at: Instant,
}

/// The inbox's own state: the history, newest last, and which view is up.
#[derive(Debug, Default)]
pub(super) struct Inbox {
    log: VecDeque<Logged>,
    /// The last command's number: a row's identity in the history.
    seq: u64,
    /// *All* rather than *Unread*.
    all: bool,
    /// The fill under the view that is up, which slides between *Unread* and *All*.
    plate: Plate,
    /// The permission prompts this client may answer from here.
    approvals: approvals::Approvals,
}

/// One of the inbox's rows, before it is drawn.
struct Row {
    id: String,
    status: Status,
    /// The first line: the command or the agent's words.
    what: String,
    /// A status word the section heading does not already say, in the status's tone, leading
    /// the second line: "Exit 1". A command that ended well and an agent that waits say
    /// nothing: *Finished* and *Needs you* over them already do, and the mark leads the row.
    word: Option<String>,
    /// The second line's words before the directory: how long it took, the tile, the worker.
    meta: String,
    cwd: Option<String>,
    age: Option<Duration>,
    /// Still unread: bright, and the mark-read button swaps in under the pointer.
    unread: bool,
    go: Go,
    /// The prompt "Deny" and "Allow" answer: its session and ask.
    approval: Option<(SessionId, u64)>,
}

/// Where a row goes when clicked.
#[derive(Clone, Copy)]
enum Go {
    Waiting(super::agents::Waiting),
    Session(SessionId),
}

/// Now, in milliseconds since the Unix epoch: the clock a worker stamps its times with.
pub(super) fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

impl WorkspaceView {
    /// What the bell counts: agents waiting and commands finished unwatched.
    #[must_use]
    pub fn inbox_count(&self) -> usize {
        self.needs_you_count().saturating_add(self.finished.len())
    }

    /// Keep a command that finished unwatched in the history, as it was when it ended.
    pub(super) fn log_finished(&mut self, session: SessionId, done: &Finished) {
        let worker = self.worker_of_session(session);
        let cwd = self.summary(session).and_then(|s| s.cwd.clone());
        let inbox = &mut self.inbox;
        inbox.seq = inbox.seq.wrapping_add(1);
        if inbox.log.len() >= LOG_MAX {
            inbox.log.pop_front();
        }
        let (seq, at, done) = (inbox.seq, Instant::now(), done.clone());
        inbox.log.push_back(Logged { seq, session, worker, cwd, done, at });
    }

    /// Show the history (`true`) or only what is unread.
    pub fn show_inbox_all(&mut self, all: bool, cx: &mut Context<Self>) {
        self.inbox.all = all;
        cx.notify();
    }

    /// Mark `session`'s finished command read: its badge goes, its row stays in *All*.
    pub fn mark_read(&mut self, session: SessionId, cx: &mut Context<Self>) {
        if self.finished.remove(&session).is_some() {
            cx.notify();
        }
    }

    /// Mark every finished command read. An agent waiting stays: it is read by answering it.
    pub fn mark_all_read(&mut self, cx: &mut Context<Self>) {
        if !self.finished.is_empty() {
            self.finished.clear();
            cx.notify();
        }
    }

    /// The history's rows, newest first: only the unread ones unless `all`. A command is
    /// unread while its session's badge is up and no later command there has replaced it.
    fn finished_rows(&self, all: bool) -> Vec<Row> {
        let now = Instant::now();
        let mut seen = std::collections::HashSet::new();
        self.inbox
            .log
            .iter()
            .rev()
            .filter_map(|logged| {
                let latest = seen.insert(logged.session);
                let unread = latest && self.finished.contains_key(&logged.session);
                (all || unread).then(|| self.finished_row(logged, unread, now))
            })
            .collect()
    }

    fn finished_row(&self, logged: &Logged, unread: bool, now: Instant) -> Row {
        let done = &logged.done;
        let (status, word) = match done.exit {
            Some(0) | None => (Status::Done, None),
            Some(code) => (Status::Failed, Some(format!("Exit {code}"))),
        };
        let command = done.command.lines().next().unwrap_or_default().trim();
        let what = if command.is_empty() { "Command".to_owned() } else { command.to_owned() };
        let worker = logged
            .worker
            .and_then(|k| self.workers.get(&k))
            .map(|w| w.name.as_str())
            .unwrap_or_default();
        // An unread row is its session's one; the history may hold several of a session.
        let id = if unread {
            format!("inbox-finished-{}", logged.session)
        } else {
            format!("inbox-read-{}", logged.seq)
        };
        Row {
            id,
            status,
            what,
            word,
            meta: join(&[&kit::duration(done.elapsed), worker]),
            cwd: logged
                .cwd
                .as_deref()
                .map(|cwd| super::tile::cwd_tail(cwd, logged.worker.and_then(|w| self.home_of(w)))),
            age: Some(now.saturating_duration_since(logged.at)),
            unread,
            go: Go::Session(logged.session),
            approval: None,
        }
    }

    /// An agent waiting on the human: what it asks first (what the tool does where the hook
    /// named only the tool, never a bare "Bash"), then its tile, worker and directory, and how
    /// long it has waited, from the worker's stamp so a reconnect keeps it.
    fn waiting_inbox_row(
        &self,
        waiting: super::agents::Waiting,
        now_ms: u64,
        cx: &gpui::App,
    ) -> Row {
        let session = waiting.session;
        let agent = self.agent_state(session);
        // What it asks, under the heading that already says it waits; its state only when it
        // asks nothing in particular.
        let what = agent
            .and_then(|a| agent_ask_line(a).or_else(|| Some(agent_status_word(a))))
            .filter(|w| !w.is_empty())
            .unwrap_or_else(|| Status::NeedsYou.label().to_owned());
        let title = waiting.tile.and_then(|t| Some(self.tile_title(self.item(t)?, cx)));
        let worker = self.workers.get(&waiting.worker).map(|w| w.name.as_str()).unwrap_or_default();
        let age = agent
            .map(|a| a.since_ms)
            .filter(|since| !since.is_zero())
            .map(|since| Duration::from_millis(now_ms.saturating_sub(since.as_millis())));
        Row {
            id: format!("inbox-waiting-{session}"),
            status: Status::NeedsYou,
            what,
            word: None,
            meta: join(&[title.as_deref().unwrap_or_default(), worker]),
            cwd: self.session_tail(session),
            age,
            unread: true,
            go: Go::Waiting(waiting),
            approval: self.approval(session).map(|prompt| (session, prompt.ask)),
        }
    }

    /// The popover's panel, dropping the base unit from the bell as it fades in.
    pub(super) fn render_inbox(&self, cx: &Context<Self>) -> gpui::AnyElement {
        let panel = self.inbox_panel(cx);
        kit::slide_fade(panel, "inbox-fade", -self.theme.spacing.xs, kit::Pace::Fade, cx)
    }

    fn inbox_panel(&self, cx: &Context<Self>) -> gpui::Stateful<Div> {
        let theme = &self.theme;
        let all = self.inbox.all;
        let now_ms = wall_ms();
        let waiting: Vec<Row> =
            self.drawn_waiting.iter().map(|w| self.waiting_inbox_row(*w, now_ms, cx)).collect();
        let finished = self.finished_rows(all);
        let mut list: Vec<gpui::AnyElement> = Vec::new();
        if !waiting.is_empty() {
            list.push(section(theme, "inbox-needs-you", "Needs you").into_any_element());
            list.extend(waiting.into_iter().map(|row| self.inbox_row(row, cx)));
        }
        if !finished.is_empty() {
            list.push(section(theme, "inbox-finished", "Finished").into_any_element());
            list.extend(finished.into_iter().map(|row| self.inbox_row(row, cx)));
        }
        let empty = list.is_empty().then(|| {
            // Unread emptied by reading is a quiet line; an inbox that has never held anything
            // says what it is for.
            if self.inbox.log.is_empty() {
                never_held(theme).into_any_element()
            } else {
                quiet_line(theme, "inbox-empty", ALL_CAUGHT_UP).into_any_element()
            }
        });
        kit::elevate(div(), theme)
            .id("inbox")
            .debug_selector(|| "inbox".to_owned())
            .role(Role::Dialog)
            .aria_label("Inbox")
            .occlude()
            // Flush with the title bar's hairline: a gap there showed the tile header's pill
            // through it as a sliver, and the shadow already lifts the popover.
            .w(px(INBOX_W))
            .flex()
            .flex_col()
            .rounded(px(theme.radii.lg))
            .overflow_hidden()
            .text_size(px(theme.typography.ui_size))
            .font_family(theme.typography.ui_family.clone())
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .child(self.inbox_head(cx))
            .child(
                div()
                    .id("inbox-list")
                    .flex()
                    .flex_col()
                    .max_h(px(INBOX_MAX_H))
                    .overflow_y_scroll()
                    .pb(px(theme.spacing.xs))
                    .children(list)
                    .children(empty),
            )
    }

    /// The head: the Unread and All views, and "Mark all read" while anything is unread. Their
    /// words sit on the edge grid; their fills a base unit in, as the rows' do.
    fn inbox_head(&self, cx: &Context<Self>) -> Div {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let pad = spacing.inset() - spacing.xs;
        let all = self.inbox.all;
        let unread = self.inbox_count();
        let tab = |id: &'static str, label: &'static str, on: bool, count: Option<usize>| {
            let el = div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .role(Role::Tab)
                .aria_label(label)
                .flex()
                .items_center()
                .gap(px(spacing.xs))
                .px(px(pad))
                .py(px(spacing.xxs))
                .rounded(px(theme.radii.sm))
                .cursor_pointer()
                .text_size(px(theme.typography.small()))
                .when(on, |el| self.inbox.plate.mark(el.text_color(hsla(s.text)), id))
                .when(!on, |el| {
                    el.text_color(hsla(s.text_muted))
                        .hover(move |el| el.bg(hsla(s.raised)).text_color(hsla(s.text)))
                })
                .child(label)
                .children(count.filter(|n| *n > 0).map(|n| {
                    tabular(div().text_color(hsla(s.text_muted)))
                        .child(SharedString::from(n.to_string()))
                }));
            tab_stop(el, s.accent)
        };
        let mark_all = (!self.finished.is_empty()).then(|| {
            let el = div()
                .id("inbox-mark-all")
                .debug_selector(|| "inbox-mark-all".to_owned())
                .role(Role::Button)
                .aria_label(MARK_ALL_READ)
                .flex_none()
                .px(px(pad))
                .py(px(spacing.xxs))
                .rounded(px(theme.radii.sm))
                .cursor_pointer()
                .text_size(px(theme.typography.small()))
                .text_color(hsla(s.text_secondary))
                .hover(move |el| el.bg(hsla(s.raised)).text_color(hsla(s.text)))
                .child(MARK_ALL_READ);
            tab_stop(el, s.accent).on_click(cx.listener(|this, _ev, _w, cx| this.mark_all_read(cx)))
        });
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(spacing.xxs))
            .px(px(spacing.xs))
            .py(px(spacing.xs))
            .border_b_1()
            .border_color(hsla(s.border_subtle))
            .child(self.inbox.plate.under(theme))
            .child(
                tab("inbox-unread", "Unread", !all, Some(unread))
                    .on_click(cx.listener(|this, _ev, _w, cx| this.show_inbox_all(false, cx))),
            )
            .child(
                tab("inbox-all", "All", all, None)
                    .on_click(cx.listener(|this, _ev, _w, cx| this.show_inbox_all(true, cx))),
            )
            .child(div().flex_1())
            .children(mark_all)
    }

    /// One two-line row on the navigator's rhythm: the status mark beside the first line,
    /// the words with the age at the right edge (the mark-read button in its place under the
    /// pointer), then the meta line. Clicked, it goes there and the inbox closes.
    fn inbox_row(&self, row: Row, cx: &Context<Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let (first_h, second_h) = super::navigator::line_heights(theme);
        let group = SharedString::from(row.id.clone());
        let said = row.word.clone().unwrap_or_else(|| row.status.label().to_owned());
        let label = SharedString::from(join(&[
            &row.what,
            &said,
            &row.meta,
            row.cwd.as_deref().unwrap_or_default(),
        ]));
        let ink = if row.unread { s.text } else { s.text_secondary };
        let tone = if row.unread { row.status.tone(theme) } else { s.text_muted };
        let mark = status_mark(theme, Some(row.status), 1.0)
            .when(!row.unread, |el| el.opacity(alpha::STRONG));
        let unread_session = match row.go {
            Go::Session(session) if row.unread => Some(session),
            Go::Session(_) | Go::Waiting(_) => None,
        };
        let mark_read = unread_session.map(|session| {
            let id = format!("{}-read", row.id);
            let selector = id.clone();
            let el = div()
                .id(ElementId::Name(id.into()))
                .debug_selector(move || selector)
                .role(Role::Button)
                .aria_label("Mark read")
                .absolute()
                .right_0()
                .top_0()
                .h(px(first_h))
                .flex()
                .items_center()
                .justify_center()
                .w(px(theme.typography.icon_large()))
                .rounded(px(theme.radii.xs))
                .cursor_pointer()
                .invisible()
                .group_hover(group.clone(), gpui::Styled::visible)
                .hover(move |el| el.bg(hsla(s.overlay)))
                .child(icon(theme, IconName::Check, IconSize::Inline, hsla(s.text_secondary)));
            tab_stop(el, s.accent).on_click(cx.listener(move |this, _ev, _w, cx| {
                cx.stop_propagation();
                this.mark_read(session, cx);
            }))
        });
        let has_read = mark_read.is_some();
        let (word_id, age_id) = (format!("{}-word", row.id), format!("{}-age", row.id));
        let age = row.age.map(|age| {
            meta(tabular(div()), theme)
                .debug_selector(move || age_id)
                .flex_none()
                .when(has_read, |el| el.group_hover(group.clone(), gpui::Styled::invisible))
                .child(SharedString::from(age_label(age)))
        });
        let go = row.go;
        let first = div()
            .relative()
            .h(px(first_h))
            .line_height(px(first_h))
            .flex()
            .items_center()
            .min_w_0()
            .gap(px(spacing.xs))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_color(hsla(ink))
                    .child(SharedString::from(row.what)),
            )
            .children(age)
            .children(mark_read);
        let word = row.word.map(|word| {
            div()
                .debug_selector(move || word_id)
                .flex_none()
                .text_color(hsla(tone))
                .child(SharedString::from(word))
        });
        let answers = row.approval.map(|(session, ask)| self.approval_buttons(session, ask, cx));
        let place = join(&[&row.meta, row.cwd.as_deref().unwrap_or_default()]);
        let separated = word.is_some() && !place.is_empty();
        let second = meta(div(), theme)
            .h(px(second_h))
            .line_height(px(second_h))
            .flex()
            .items_center()
            .min_w_0()
            .gap(px(spacing.xs))
            .overflow_hidden()
            .whitespace_nowrap()
            .children(word)
            .when(separated, |el| el.child("\u{b7}"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(SharedString::from(place)),
            )
            .children(answers);
        let el = kit::row(theme, kit::Row::Two)
            .id(ElementId::Name(row.id.clone().into()))
            .debug_selector(move || row.id)
            .group(group)
            .role(Role::Button)
            .aria_label(label)
            // The fill a base unit in from the popover's edges; the mark on the edge grid.
            .mx(px(spacing.xs))
            .px(px(spacing.inset() - spacing.xs))
            .gap(px(spacing.xs))
            .items_center()
            .rounded(px(theme.radii.sm))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla(s.raised)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_start()
                    .gap(px(spacing.xs))
                    .child(div().h(px(first_h)).flex().items_center().child(mark))
                    .child(div().flex_1().min_w_0().flex().flex_col().child(first).child(second)),
            );
        tab_stop(el, s.accent)
            .on_click(cx.listener(move |this, _ev, _w, cx| {
                this.menu = None;
                match go {
                    Go::Waiting(waiting) => this.go_to_waiting(waiting, cx),
                    Go::Session(session) => this.reveal_session(session, cx),
                }
            }))
            .into_any_element()
    }
}

impl WorkspaceView {
    /// "Deny" and "Allow" for `session`'s prompt `ask`, as quiet text buttons the height of a
    /// row's second line: the answer goes where the row is, without going to the agent.
    fn approval_buttons(&self, session: SessionId, ask: u64, cx: &Context<Self>) -> Div {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let (_, second_h) = super::navigator::line_heights(theme);
        let button = |id: String, label: &'static str, ink| {
            let selector = id.clone();
            let el = div()
                .id(ElementId::Name(id.into()))
                .debug_selector(move || selector)
                .role(Role::Button)
                .aria_label(label)
                .flex_none()
                .h(px(second_h))
                .px(px(theme.spacing.xs))
                .flex()
                .items_center()
                .rounded(px(theme.radii.xs))
                .cursor_pointer()
                .text_color(hsla(ink))
                .hover(move |el| el.bg(hsla(s.overlay)))
                .child(label);
            tab_stop(el, s.accent)
        };
        let deny = button(format!("inbox-deny-{session}"), DENY, s.text_secondary).on_click(
            cx.listener(move |this, _ev, _w, cx| {
                cx.stop_propagation();
                let verdict = Verdict::Deny { message: String::new(), interrupt: false };
                this.answer_approval(session, ask, verdict, cx);
            }),
        );
        let allow = button(format!("inbox-allow-{session}"), ALLOW, s.accent).on_click(
            cx.listener(move |this, _ev, _w, cx| {
                cx.stop_propagation();
                this.answer_approval(session, ask, Verdict::Allow, cx);
            }),
        );
        div().flex_none().flex().items_center().gap(px(theme.spacing.xxs)).child(deny).child(allow)
    }
}

/// Parts of a line joined by a middle dot, the empty ones left out.
fn join(parts: &[&str]) -> String {
    parts.iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join(" · ")
}

/// A section's heading, as every list draws one.
fn section(theme: &Theme, selector: &'static str, text: &'static str) -> gpui::Stateful<Div> {
    section_heading(theme, selector.into(), text).debug_selector(move || selector.to_owned())
}

/// An inbox that has never held anything: the line, and what lands here, on the edge grid.
fn never_held(theme: &Theme) -> gpui::Stateful<Div> {
    let s = &theme.surfaces;
    kit::inset_x(div(), theme)
        .id("inbox-empty")
        .debug_selector(|| "inbox-empty".to_owned())
        .role(Role::Status)
        .aria_label(ALL_CAUGHT_UP)
        .flex()
        .flex_col()
        .gap(px(theme.spacing.xxs))
        .py(px(theme.spacing.md))
        .child(div().text_color(hsla(s.text_secondary)).child(ALL_CAUGHT_UP))
        .child(meta(div(), theme).debug_selector(|| "inbox-holds".to_owned()).child(INBOX_HOLDS))
}

#[cfg(test)]
impl WorkspaceView {
    /// Whether the inbox shows its history rather than only what is unread.
    #[must_use]
    pub(super) const fn inbox_shows_all(&self) -> bool {
        self.inbox.all
    }

    /// Where the inbox's view plate was drawn in the last frame.
    pub(super) fn inbox_plate(&self) -> Option<gpui::Bounds<gpui::Pixels>> {
        self.inbox.plate.drawn()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_joins_what_it_has() {
        assert_eq!(join(&["Done · 3.2 s", "studio"]), "Done · 3.2 s · studio");
        assert_eq!(join(&["", "studio", ""]), "studio");
    }

    /// The inbox's words and the status words down its right edge are chrome, so sentence
    /// case, as `kit`'s `chrome_text_is_sentence_case` holds everywhere else.
    #[test]
    fn the_inbox_words_are_sentence_case() {
        use crate::icons::Status;
        let statuses = [
            Status::Idle,
            Status::Working,
            Status::NeedsYou,
            Status::Done,
            Status::Failed,
            Status::Away,
        ];
        let words = [ALL_CAUGHT_UP, INBOX_HOLDS, MARK_ALL_READ, ALLOW, DENY]
            .into_iter()
            .chain(statuses.map(Status::label));
        for text in words {
            let mut chars = text.chars();
            assert!(chars.next().is_some_and(char::is_uppercase), "starts lowercase: {text:?}");
            assert!(!chars.any(char::is_uppercase), "title case: {text:?}");
        }
    }
}
