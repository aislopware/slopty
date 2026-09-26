//! The bell's inbox, kept as a mailbox: what waits on the human and what finished while they
//! looked away, with the history of what was already read.
//!
//! Two views. *Unread* lists the agents waiting (*Needs you*) and the long commands that ended
//! unwatched and are still badged on their headers (*Finished*); *All* keeps every command the
//! inbox ever took, newest first, read or not. A row is two lines: the command or the agent's
//! words, then its worker and directory. On the right an age, which swaps for a mark-read
//! button under the pointer. Reading is the header's badge going: a row marked read, "Mark all
//! read", or its tile looked at. The history holds the last [`LOG_MAX`] commands.

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
use slopty_theme::{Theme, alpha};

use super::agents::agent_status_text;
use super::{Finished, WorkspaceView};
use crate::a11y::tab_stop;
use crate::colors::hsla;
use crate::icons::{IconName, IconSize, Status, icon, status_mark};
use crate::kit::tabular;
use crate::palette::{age_label, mono_family, section_heading};

/// The popover's width, in points.
const INBOX_W: f32 = 360.0;

/// The most the list shows before it scrolls, in points.
const INBOX_MAX_H: f32 = 480.0;

/// How many finished commands the history keeps: more than a day of long builds, and a bound.
pub(super) const LOG_MAX: usize = 200;

/// What the empty inbox says.
pub(super) const ALL_CAUGHT_UP: &str = "You're all caught up";

/// The button that reads every unread row.
pub(super) const MARK_ALL_READ: &str = "Mark all read";

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
}

/// One of the inbox's rows, before it is drawn.
struct Row {
    id: String,
    status: Status,
    /// The first line: the command or the agent's words.
    what: String,
    /// The second line's words before the directory: the outcome, the worker.
    meta: String,
    cwd: Option<String>,
    age: Option<Duration>,
    /// Still unread: bright, and the mark-read button swaps in under the pointer.
    unread: bool,
    go: Go,
}

/// Where a row goes when clicked.
#[derive(Clone, Copy)]
enum Go {
    Waiting(super::agents::Waiting),
    Session(SessionId),
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

    /// Whether the inbox shows its history rather than only what is unread.
    #[must_use]
    pub const fn inbox_shows_all(&self) -> bool {
        self.inbox.all
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
        let status = match done.exit {
            Some(0) | None => Status::Done,
            Some(_) => Status::Failed,
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
            meta: join(&[&done.label(), worker]),
            cwd: logged.cwd.as_deref().map(super::tile::cwd_tail),
            age: Some(now.saturating_duration_since(logged.at)),
            unread,
            go: Go::Session(logged.session),
        }
    }

    /// An agent waiting on the human: its words first, then its tile, worker and directory.
    fn waiting_inbox_row(&self, waiting: super::agents::Waiting, cx: &gpui::App) -> Row {
        let session = waiting.session;
        let what = self
            .agent_state(session)
            .map(agent_status_text)
            .filter(|w| !w.is_empty())
            .unwrap_or_else(|| "Needs you".to_owned());
        let title = waiting.tile.and_then(|t| Some(self.card_title(t, self.item(t)?, cx)));
        let worker = self.workers.get(&waiting.worker).map(|w| w.name.as_str()).unwrap_or_default();
        Row {
            id: format!("inbox-waiting-{session}"),
            status: Status::NeedsYou,
            what,
            meta: join(&[title.as_deref().unwrap_or_default(), worker]),
            cwd: self.summary(session).and_then(|s| s.cwd.as_deref()).map(super::tile::cwd_tail),
            age: None,
            unread: true,
            go: Go::Waiting(waiting),
        }
    }

    /// The popover's panel.
    pub(super) fn render_inbox(&self, cx: &Context<Self>) -> gpui::AnyElement {
        let panel = self.inbox_panel(cx);
        crate::kit::fade_in(panel, "inbox-fade", cx)
    }

    fn inbox_panel(&self, cx: &Context<Self>) -> gpui::Stateful<Div> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let all = self.inbox.all;
        let waiting: Vec<Row> =
            self.drawn_waiting.iter().map(|w| self.waiting_inbox_row(*w, cx)).collect();
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
        let empty = list.is_empty();
        div()
            .id("inbox")
            .debug_selector(|| "inbox".to_owned())
            .role(Role::Dialog)
            .aria_label("Inbox")
            .occlude()
            .w(px(INBOX_W))
            .flex()
            .flex_col()
            .rounded(px(theme.radii.md))
            .bg(hsla(s.panel))
            .border_1()
            .border_color(hsla(s.border))
            .shadow_sm()
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
                    .when(empty, |el| el.child(caught_up(theme))),
            )
    }

    /// The head: the Unread and All views, and "Mark all read" while anything is unread.
    fn inbox_head(&self, cx: &Context<Self>) -> Div {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
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
                .px(px(spacing.sm))
                .py(px(spacing.xxs))
                .rounded(px(theme.radii.sm))
                .cursor_pointer()
                .text_size(px(theme.typography.small()))
                .when(on, |el| el.bg(hsla(s.overlay)).text_color(hsla(s.text)))
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
                .px(px(spacing.xs))
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
            .px(px(spacing.sm))
            .py(px(spacing.xs))
            .border_b_1()
            .border_color(hsla(s.border_subtle))
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

    /// One two-line row: the status slot, the words and the age over the worker and the
    /// directory. Clicked, it goes there and the inbox closes.
    fn inbox_row(&self, row: Row, cx: &Context<Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let group = SharedString::from(row.id.clone());
        let label = SharedString::from(join(&[
            &row.what,
            &row.meta,
            row.cwd.as_deref().unwrap_or_default(),
        ]));
        let ink = if row.unread { s.text } else { s.text_secondary };
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
                .top_0()
                .right_0()
                .flex()
                .items_center()
                .justify_center()
                .size(px(theme.typography.icon_large()))
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
        let age = row.age.map(|age| {
            tabular(
                div()
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .when(has_read, |el| el.group_hover(group.clone(), gpui::Styled::invisible)),
            )
            .child(SharedString::from(age_label(age)))
        });
        let go = row.go;
        let second = div()
            .flex()
            .items_center()
            .min_w_0()
            .gap(px(spacing.xs))
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text_muted))
            .child(
                div().flex_none().whitespace_nowrap().child(SharedString::from(row.meta.clone())),
            )
            .when(!row.meta.is_empty() && row.cwd.is_some(), |el| el.child("·"))
            .children(row.cwd.map(|cwd| {
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .font_family(mono_family(theme))
                    .child(SharedString::from(cwd))
            }));
        let el = div()
            .id(ElementId::Name(row.id.clone().into()))
            .debug_selector({
                let id = row.id.clone();
                move || id
            })
            .group(group)
            .role(Role::Button)
            .aria_label(label)
            .flex_none()
            .mx(px(spacing.xs))
            .px(px(spacing.xs))
            .py(px(spacing.xs))
            .flex()
            .items_start()
            .gap(px(spacing.sm))
            .rounded(px(theme.radii.sm))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla(s.raised)))
            .child(mark)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(spacing.xxs))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(hsla(ink))
                            .child(SharedString::from(row.what)),
                    )
                    .child(second),
            )
            .child(
                div()
                    .relative()
                    .flex_none()
                    .min_w(px(theme.typography.icon_large()))
                    .h(px(theme.typography.icon_large()))
                    .flex()
                    .items_center()
                    .justify_end()
                    .children(age)
                    .children(mark_read),
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

/// Parts of a line joined by a middle dot, the empty ones left out.
fn join(parts: &[&str]) -> String {
    parts.iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join(" · ")
}

/// A section's heading, as every list draws one.
fn section(theme: &Theme, selector: &'static str, text: &'static str) -> gpui::Stateful<Div> {
    section_heading(theme, selector.into(), text).debug_selector(move || selector.to_owned())
}

/// The empty inbox: a mark and one line.
fn caught_up(theme: &Theme) -> gpui::Stateful<Div> {
    let s = &theme.surfaces;
    div()
        .id("inbox-empty")
        .debug_selector(|| "inbox-empty".to_owned())
        .role(Role::Status)
        .aria_label(ALL_CAUGHT_UP)
        .flex()
        .flex_col()
        .items_center()
        .gap(px(theme.spacing.sm))
        .py(px(theme.spacing.xl))
        .text_color(hsla(s.text_muted))
        .child(icon(theme, IconName::Inbox, IconSize::Large, hsla(s.text_muted)))
        .child(ALL_CAUGHT_UP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_joins_what_it_has() {
        assert_eq!(join(&["Done · 3.2 s", "studio"]), "Done · 3.2 s · studio");
        assert_eq!(join(&["", "studio", ""]), "studio");
    }
}
