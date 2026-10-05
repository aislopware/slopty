//! The tray on the composer's top edge: what waits on the person, the plan, the edits of the
//! last turn, the messages waiting to go, and the work in the background, stacked and parted
//! by hairlines.
//!
//! A request whose call is on screen is answered on the call's own card ([`Placement`]); the
//! tray carries a copy of it only while that card is scrolled away, with the way back to it,
//! and always carries a request that belongs to no call. There it is a card of its own above
//! the rest, answered as one decision ([`super::decision`]).

use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, Div, ElementId, FollowMode, InteractiveElement as _,
    IntoElement as _, LiveRegion as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, relative,
};
use slopty_proto::thread::detail::ExecStatus;
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{BackgroundTask, Cap, Delivery, Drive, ItemId, Request};

use super::composer::sentence;
use super::{PEEK_LINES, ThreadView, ThreadViewEvent, tail};
use crate::colors::hsla;
use crate::conversation::thread::activity::{Activity, Asked, Edit, STEP_DONE};
use crate::conversation::thread::hub::Refusal;
use crate::conversation::thread::questions;
use crate::conversation::thread::rows::Row;
use crate::icons::Symbol;
use crate::kit::{self, ButtonKind};

/// The widest an answer's reach is drawn on its button, in ems of its text.
const SCOPE_EMS: f32 = 14.0;

/// What a frame drew from the list's scroll, to know when a scroll changes it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) struct Marks {
    /// The row of the request on show's call, and where the request is answered.
    pub asked: Option<(usize, Placement)>,
    /// The list is scrolled up from its newest row.
    pub down: bool,
}

/// Where the request on show is answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) enum Placement {
    /// On its call's card, which is on screen.
    Inline,
    /// In the tray, its call's card scrolled away above.
    Above,
    /// In the tray, its call's card scrolled away below.
    Below,
    /// In the tray: it belongs to no call on show.
    #[default]
    Tray,
}

impl ThreadView {
    /// The row of the call `item` is about, when the list shows it as a row of its own.
    pub(super) fn call_row(&self, item: &ItemId) -> Option<usize> {
        self.rows.iter().position(|r| matches!(r, Row::Tool { item: i } if i == item))
    }

    /// Where the call in row `row` stands against the viewport, as the last layout left it.
    /// A row the list has not laid out yet, past its top while it follows the newest row, is
    /// the row that just came in at its foot: on screen. Else a call that asks as it arrives
    /// would be answered in the tray for one frame and jump onto its card in the next.
    fn placement_of(&self, row: usize) -> Placement {
        match (self.list.item_is_above_viewport(row), self.list.item_is_below_viewport(row)) {
            (Some(true), _) => Placement::Above,
            (_, Some(true)) => Placement::Below,
            (Some(false), Some(false)) => Placement::Inline,
            _ if self.list.is_following_tail() && row >= self.list.logical_scroll_top().item_ix => {
                Placement::Inline
            }
            _ => Placement::Tray,
        }
    }

    /// The request on show, among those waiting.
    pub(super) fn shown_waiting<'a>(&self, cx: &'a App) -> Option<&'a Request> {
        let state = self.state(cx)?;
        let bar = Activity::of(self.hub.read(cx).threads(), self.thread, state);
        let waiting: Vec<&Asked<'_>> = bar.waiting().collect();
        let at = self.asked_at.min(waiting.len().saturating_sub(1));
        waiting.get(at).map(|asked| asked.request)
    }

    /// What the list's scroll says now: where the request on show is answered, and whether
    /// the way down shows. Questions are always the tray's: the questionnaire is one, kept
    /// there.
    pub(super) fn read_marks(&self, cx: &App) -> Marks {
        let asked = self
            .shown_waiting(cx)
            .filter(|request| request.questions.is_empty())
            .and_then(|request| request.item.as_ref())
            .and_then(|item| self.call_row(item))
            .map(|row| (row, self.placement_of(row)));
        Marks { asked, down: self.away_from_newest() }
    }

    /// Whether the way down shows: the list does not follow its newest row, and is not known
    /// to stand at its end. Rows the list has not measured leave the end unknown, as on a
    /// long thread scrolled up from its newest row: the way down shows then too, rather than
    /// only once every row above has been measured.
    fn away_from_newest(&self) -> bool {
        !self.list.is_following_tail() && self.list.is_scrolled_to_end() != Some(true)
    }

    /// Draw again once the list's layout moved what the frame drew from it: the request's
    /// place, the way down. Reads only the list, so a frame that changes neither costs no
    /// second one.
    pub(super) fn recheck_marks(&self, cx: &mut Context<Self>) {
        let was = self.marks.get();
        let now = Marks {
            asked: was.asked.map(|(row, _)| (row, self.placement_of(row))),
            down: self.away_from_newest(),
        };
        if now != was {
            #[cfg(test)]
            self.marks_moved.set(self.marks_moved.get().saturating_add(1));
            cx.notify();
        }
    }

    /// Whether the request on show is answered on the card of the call in row `row`.
    pub(super) fn answered_inline(&self, row: usize) -> bool {
        self.marks.get().asked == Some((row, Placement::Inline))
    }

    /// Count what came since the list left its newest row, and once the count has held for
    /// [`TELL_AFTER`], let the way down announce it: a run of arrivals is said once, when it
    /// settles, not once per row.
    ///
    /// It goes by whether the list follows its newest row, not by the way down's mark: right
    /// after rows come the list has not laid them out, and the mark reads as at the end.
    pub(super) fn count_unseen(&mut self, cx: &Context<Self>) {
        if self.list.is_following_tail() {
            self.unseen_from = None;
            self.told = 0;
            self.telling = None;
            return;
        }
        let items = self.state(cx).map_or(0, |state| state.items.len());
        let new = items.saturating_sub(*self.unseen_from.get_or_insert(items));
        if self.telling.as_ref().is_some_and(|(waiting, _)| *waiting == new) {
            return;
        }
        if new == self.told {
            self.telling = None;
            return;
        }
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TELL_AFTER).await;
            let _gone = this.update(cx, |this, cx| {
                this.told = new;
                this.telling = None;
                cx.notify();
            });
        });
        self.telling = Some((new, task));
    }

    /// The way down to the newest row, over the list's foot while it is scrolled up: a round
    /// chevron, or a pill saying how much is new once anything came. What it says is
    /// announced politely once the count settles.
    pub(super) fn down_button(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let new = self.unseen_from.map_or(0, |from| {
            self.state(cx).map_or(0, |state| state.items.len()).saturating_sub(from)
        });
        let control = self.z(theme.density.control);
        // At the list's right edge, in the gutter beside the column where it has one: over the
        // text, it would sit on whatever the newest rows draw there.
        div()
            .absolute()
            .bottom(self.z(theme.spacing.md))
            .right(self.z(theme.spacing.sm))
            .child(crate::a11y::tab_stop(
                div()
                    .id("thread-down")
                    .debug_selector(|| "thread-down".to_owned())
                    .role(Role::Button)
                    .aria_label("Scroll to the newest")
                    .when(self.told > 0, |el| {
                        el.aria_live(gpui::accesskit::Live::Polite).aria_value(SharedString::from(
                            format!("{} below", new_words(self.told)),
                        ))
                    })
                    .h(control)
                    .min_w(control)
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(self.z(theme.spacing.xxs))
                    .when(new > 0, |el| {
                        el.pl(self.z(theme.spacing.sm)).pr(self.z(theme.spacing.xs))
                    })
                    .rounded_full()
                    .border(kit::hair(theme))
                    .border_color(hsla(s.border))
                    .bg(hsla(s.elevated))
                    .cursor_pointer()
                    .hover(move |el| el.bg(hsla(s.hover)))
                    .when(new > 0, |el| {
                        let size = self.z(theme.typography.small());
                        el.child(
                            div()
                                .debug_selector(|| "thread-down-count".to_owned())
                                .flex()
                                .items_center()
                                .gap(self.z(theme.spacing.xxs))
                                .text_size(size)
                                .text_color(hsla(s.text_secondary))
                                .child(kit::Rolling::new(
                                    "thread-down-figure",
                                    new_figure(new),
                                    size,
                                ))
                                .child(NEW_TAIL),
                        )
                    })
                    .child(self.icon(Symbol::ChevronDown, s.text_secondary))
                    .on_click(cx.listener(|this, _ev, _w, cx| {
                        this.list.scroll_to_end();
                        this.list.set_follow_mode(FollowMode::Tail);
                        cx.notify();
                    })),
                s.focus,
            ))
            .into_any_element()
    }

    /// The tray, when anything waits in it, and whether it ends tucked behind the composer's
    /// top edge.
    ///
    /// A request put to the person is a card of its own, a full step of space above the rest:
    /// a decision never reads as another way to send the message under it. The rest (the
    /// plan, the queue, the work in the background) is the composer's head, tucked behind its
    /// top edge, or a card of its own where there is no composer (a subagent's thread).
    ///
    /// A request on show stands whole; what else waits stands at most a share of the window's
    /// height `window_h` ([`super::TRAY`], [`super::TRAY_ASKING`] under a request) and scrolls
    /// past that, so the thread above keeps its share of the tile however much waits.
    pub(super) fn activity_bar(
        &self,
        tucked: bool,
        window_h: gpui::Pixels,
        cx: &Context<Self>,
    ) -> Option<(AnyElement, bool)> {
        let hub = self.hub.read(cx);
        let state = self.state(cx)?;
        let bar = Activity::of(hub.threads(), self.thread, state);
        let theme = &self.theme;
        let s = theme.surfaces;
        // What waits on the person stands whole; the rest, under it, scrolls past `max`.
        let waiting: Vec<&Asked<'_>> = bar.waiting().collect();
        let at = self.asked_at.min(waiting.len().saturating_sub(1));
        let placement = self.marks.get().asked.map_or(Placement::Tray, |(_, p)| p);
        let request = waiting
            .get(at)
            .filter(|_| placement != Placement::Inline)
            .map(|current| self.request_card(current.request, at, waiting.len(), placement, cx));
        // Each kind of thing stands in a group of its own, a base unit of space between groups,
        // so a row never reads as belonging to the group above it. Space, not a rule: rules
        // between rows read as a stack of boxes.
        let mut groups: Vec<Vec<AnyElement>> = Vec::new();
        groups.push(hub.refusals(self.thread).map(|r| self.refusal_line(r, cx)).collect());
        groups.push(
            bar.asked
                .iter()
                .filter(|a| a.answered.is_some())
                .map(|a| self.answered_line(a, cx))
                .collect(),
        );
        groups.push(bar.plan.map(|plan| self.plan_section(plan, cx)).into_iter().collect());
        if !bar.edited.is_empty() {
            groups.push(vec![self.edited_section(&bar.edited, cx)]);
        }
        let paused = bar.queue.iter().any(|q| q.stopped).then(|| self.paused_line());
        groups.push(
            paused
                .into_iter()
                .chain(
                    bar.queue
                        .iter()
                        .map(|q| self.queued_line(q, bar.can_withdraw, bar.can_promote, cx)),
                )
                .collect(),
        );
        if !bar.background.is_empty() {
            groups.push(vec![self.background_section(&bar.background)]);
        }
        if self.tasks_open && !bar.tasks.is_empty() {
            let mut tasks = vec![self.tasks_head(cx)];
            tasks.extend(bar.tasks.iter().map(|task| self.task_line(task, bar.can_stop, cx)));
            groups.push(tasks);
        }
        if self.meter_open {
            groups.push(vec![self.meter_panel(state, cx)]);
        }
        let mut sections: Vec<AnyElement> = Vec::new();
        for group in groups.into_iter().filter(|g| !g.is_empty()) {
            if !sections.is_empty() {
                sections.push(div().flex_none().h(self.z(theme.spacing.xs)).into_any_element());
            }
            sections.extend(group);
        }
        if sections.is_empty() && request.is_none() {
            return None;
        }
        let parted = request.is_some();
        let max = window_h * if parted { super::TRAY_ASKING } else { super::TRAY };
        let rest = (!sections.is_empty()).then(|| {
            div()
                .id("thread-activity-rest")
                .debug_selector(|| "thread-activity-rest".to_owned())
                .w_full()
                .flex()
                .flex_col()
                .max_h(max)
                .overflow_y_scroll()
                .children(sections)
        });
        let radius = self.z(theme.radii.lg);
        let request = request.map(|request| {
            // The request takes the keyboard by a press anywhere on it, or by Tab to one of
            // its answers; then, and only then, ⌘↵ and ⌘⌫ answer it. A press that a field or
            // a control inside it took (a question's "Other" field) keeps the keyboard there:
            // the card's press comes after theirs, and taking it back would leave the field
            // deaf to what is typed.
            div()
                .id("thread-decision")
                .debug_selector(|| "thread-decision".to_owned())
                .key_context(crate::conversation::REQUEST_CTX)
                .track_focus(&self.request_focus)
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _ev, window, cx| {
                        if !this.request_focus.contains_focused(window, cx) {
                            this.request_focus.focus(window, cx);
                        }
                    }),
                )
                .w_full()
                .min_h_0()
                .flex()
                .flex_col()
                .rounded(radius)
                .border(kit::hair(theme))
                .border_color(hsla(s.border))
                .bg(hsla(s.elevated))
                .map(|el| kit::rests(el, theme, true, true))
                .overflow_hidden()
                .mb(self.z(theme.spacing.md))
                .child(request)
        });
        // Over the composer the tray is the card's head: as wide, its corners the composer's,
        // and the composer's top edge the one hairline between them, so one outline holds both
        // and no row reads as cut by the composer. Alone, it is a card of its own. Inside, its
        // kinds of thing stand apart by the quieter hairline. It is raised as the composer is,
        // on the same surface: a wash would sink it into the page in light.
        let radius = self.z(if tucked { theme.radii.lg } else { theme.radii.md });
        let rest = rest.map(|rest| {
            div()
                .id("thread-activity")
                .debug_selector(|| "thread-activity".to_owned())
                .role(Role::Group)
                .aria_label("Activity")
                .min_h_0()
                .flex()
                .flex_col()
                .rounded_tl(radius)
                .rounded_tr(radius)
                .border_t(kit::hair(theme))
                .border_l(kit::hair(theme))
                .border_r(kit::hair(theme))
                .when(!tucked, |el| {
                    el.border_b(kit::hair(theme))
                        .rounded_bl(radius)
                        .rounded_br(radius)
                        .mb(self.z(theme.spacing.xs))
                })
                .border_color(hsla(s.border))
                .bg(hsla(s.elevated))
                .map(|el| kit::rests(el, theme, true, !tucked))
                .overflow_hidden()
                .child(rest)
        });
        let tucked = tucked && rest.is_some();
        Some((
            div()
                .w_full()
                .min_h_0()
                .flex()
                .flex_col()
                .children(request)
                .children(rest)
                .into_any_element(),
            tucked,
        ))
    }

    /// One of the tray's lines: a row's height, the chrome's edge, its parts a base unit
    /// apart.
    fn section(&self) -> Div {
        let spacing = self.theme.spacing;
        div()
            .w_full()
            .flex()
            .items_center()
            .gap(self.z(spacing.xs))
            .px(self.z(spacing.sm))
            .min_h(self.z(kit::Row::One.height(&self.theme)))
            .text_size(self.z(self.theme.typography.small()))
    }

    /// A tray row of two lines: `head` at a row's height, and under it, past the mark's slot,
    /// `under` in the code face and the quiet tone (a command's last line while it runs), cut
    /// to one line. Output never runs inline after a title in body text.
    fn two_lines(&self, head: Div, under: Option<String>) -> Div {
        let theme = &self.theme;
        let s = theme.surfaces;
        let spacing = theme.spacing;
        div()
            .w_full()
            .flex()
            .flex_col()
            .px(self.z(spacing.sm))
            .text_size(self.z(theme.typography.small()))
            .child(head.min_h(self.z(kit::Row::One.height(theme))))
            .children(under.map(|line| {
                div()
                    .w_full()
                    .pl(self.z(super::TOOL_ROW + spacing.xs))
                    .pb(self.z(spacing.xs))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .font_family(self.mono())
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(line))
            }))
    }

    /// The head of the work in the background, opened from its chip: what it is, how much of
    /// it runs or how it ended, and the way to fold it.
    fn tasks_head(&self, cx: &Context<Self>) -> AnyElement {
        let s = self.theme.surfaces;
        // The count is the composer's chip's, which opened this and stays beside it as the
        // way back: said twice, a step apart, it read as two things.
        self.section()
            .id("thread-tasks-head")
            .debug_selector(|| "thread-tasks-head".to_owned())
            .role(Role::Button)
            .aria_label("In the background")
            .aria_expanded(true)
            .cursor_pointer()
            .text_color(hsla(s.text_secondary))
            .child(self.slot())
            .child(div().flex_none().child("In the background"))
            .child(div().flex_1())
            .child(self.icon(Symbol::ChevronDown, s.text_muted))
            .on_click(cx.listener(|this, _ev, _w, cx| {
                this.tasks_open = false;
                cx.notify();
            }))
            .into_any_element()
    }

    /// Something the worker turned down that the thread would not show: what and why, until
    /// the person lets it go.
    fn refusal_line(&self, refusal: &Refusal, cx: &Context<Self>) -> AnyElement {
        let s = self.theme.surfaces;
        let id = refusal.id;
        self.section()
            .id(ElementId::Name(format!("refused-{id}").into()))
            .debug_selector(move || format!("refused-{id}"))
            .role(Role::Alert)
            .aria_label(SharedString::from(refusal.words.clone()))
            .text_color(hsla(s.text_secondary))
            .child(self.slot().child(self.icon(Symbol::ExclamationmarkTriangle, s.error)))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .whitespace_normal()
                    .child(SharedString::from(refusal.words.clone())),
            )
            // Going back with the files was refused (another thread edits the same folder):
            // going back without them is the next thing to try.
            .when_some(
                match refusal.intent {
                    Some(Intent::Rewind { turn, files: true }) => Some(turn),
                    _ => None,
                },
                |el, turn| {
                    el.child(
                        self.button(
                            format!("refused-no-files-{id}"),
                            "Without the files",
                            ButtonKind::Ghost,
                        )
                        .on_click(cx.listener(move |this, _ev, _w, cx| {
                            this.dismiss(id, cx);
                            let seed =
                                this.state(cx).and_then(|st| super::branch::message_of(st, turn));
                            this.start(Intent::Rewind { turn, files: false }, seed, cx);
                        })),
                    )
                },
            )
            .child(
                self.icon_button(format!("refused-dismiss-{id}"), Symbol::Xmark, "Dismiss")
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.dismiss(id, cx))),
            )
            .into_any_element()
    }

    fn answered_line(&self, asked: &Asked<'_>, cx: &Context<Self>) -> AnyElement {
        let s = self.theme.surfaces;
        let choice = asked.answered.and_then(|sent| match &sent.intent {
            Intent::Answer { choice, .. } => Some(choice.clone()),
            Intent::Release { .. } => self
                .state(cx)
                .and_then(|st| release_words(&st.meta))
                .or_else(|| Some("Answer in the terminal".to_owned())),
            _ => None,
        });
        let request = asked.request;
        let label = choice
            .map(|c| {
                request
                    .options
                    .iter()
                    .find(|o| o.id == c)
                    .map_or_else(|| questions::words(&request.questions, &c), |o| o.label.clone())
            })
            .unwrap_or_default();
        let id = request.id.0.clone();
        self.section()
            .debug_selector(move || format!("answered-{id}"))
            .text_color(hsla(s.text_muted))
            .child(self.slot().child(self.icon(Symbol::Checkmark, s.text_muted)))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(request.title.clone())),
            )
            .child(div().flex_1())
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(label)),
            )
            .into_any_element()
    }

    /// The way back to the agent's own prompt: an agent whose prompt runs in a terminal takes
    /// the request back there, and one whose own TUI can join the thread beside Slopty
    /// ([`Cap::LIVE_TUI`] with no terminal yet: Codex) has it opened on the thread first. Its
    /// one solid when nothing else answers it here. The terminal comes into view.
    pub(super) fn release_button(
        &self,
        request: &Request,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let words = release_words(&self.state(cx)?.meta)?;
        let only_way = request.options.is_empty() && request.questions.is_empty();
        let kind = if only_way { ButtonKind::Primary } else { ButtonKind::Ghost };
        let release = request.id.clone();
        Some(
            self.answer_button(format!("release-{}", release.0), (words, None), kind)
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    let _id = this.intent(Intent::Release { ask: release.clone() }, cx);
                    this.show_terminal(cx);
                }))
                .into_any_element(),
        )
    }

    /// A button of a request's: its words, then how far it reaches, muted. A rule can be long
    /// ("cargo test -p atlas-api refresh"): it gives way to an ellipsis at [`SCOPE_EMS`], so
    /// the answers keep one row and the button's hint and a screen reader still say all of it.
    pub(super) fn answer_button(
        &self,
        id: String,
        (words, scope): (String, Option<String>),
        kind: ButtonKind,
    ) -> gpui::Stateful<Div> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let label = super::decision::answer_label(&words, scope.as_deref());
        let scope = scope.map(|scope| {
            div()
                .min_w_0()
                .max_w(self.z(theme.typography.small() * SCOPE_EMS))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_color(hsla(s.text_muted))
                .child(SharedString::from(scope))
        });
        self.button_frame(id, label, kind)
            .gap(self.z(theme.spacing.xs))
            .px(self.z(theme.spacing.sm))
            .child(div().flex_none().child(SharedString::from(words)))
            .children(scope)
    }

    /// The request on show, in the tray: what it asks, its answers under a hairline, "2 of 5"
    /// with the way to the others, and the way back to its call when that is scrolled away.
    /// An approval is answered only by a press, so a stray key cannot answer it; questions are
    /// a questionnaire, which the keyboard walks once it is in it.
    fn request_card(
        &self,
        request: &Request,
        at: usize,
        of: usize,
        placement: Placement,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let release = self.release_button(request, cx);
        let asking = self.asking.as_ref().filter(|a| *a.ask() == request.id);
        // A plan put to the person is read on its card in the thread: the tray names it and
        // keeps the way to it and the answers, not a second copy of its words.
        let carded = request.kind == Request::PLAN
            && request.item.as_ref().and_then(|item| self.call_row(item)).is_some();
        let (title, counter) = match asking {
            Some(asking) => {
                let header = asking.questions().current(cx).and_then(|q| q.header.clone());
                let progress = asking.questions().state().read(cx).progress();
                let counter = (progress.total() > 1)
                    .then(|| format!("Question {} of {}", progress.current(), progress.total()));
                (
                    header
                        .filter(|h| !h.trim().is_empty())
                        .unwrap_or_else(|| "Question".to_owned()),
                    counter,
                )
            }
            None if carded => ("Plan ready".to_owned(), None),
            None => (request.title.clone(), None),
        };
        let text =
            request.text.as_ref().filter(|_| !carded).map(|t| t.text.clone()).or_else(|| {
                asking
                    .is_none()
                    .then(|| request.questions.first().map(|q| q.text.clone()))
                    .flatten()
            });
        // A command is code; anything else the agent says is a sentence.
        let code = request.kind == Request::APPROVAL;
        let scroll = match placement {
            Placement::Above => Some(("Scroll \u{2191}", -1_isize)),
            Placement::Below => Some(("Scroll \u{2193}", 1)),
            Placement::Inline | Placement::Tray => None,
        };
        let back = scroll.zip(request.item.clone()).and_then(|((label, _), item)| {
            let row = self.call_row(&item)?;
            Some(
                self.button("asked-scroll", label, ButtonKind::Ghost)
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        // Revealing leaves the tail, or the next layout snaps back to it.
                        this.list.pause_following_tail();
                        this.list.scroll_to_reveal_item(row);
                        cx.notify();
                    }))
                    .into_any_element(),
            )
        });
        let stepper = (of > 1).then(|| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(self.z(theme.spacing.xxs))
                .child(
                    self.icon_button("asked-prev", Symbol::ChevronUp, "Previous request").on_click(
                        cx.listener(move |this, _ev, _w, cx| this.step_asked(-1, of, cx)),
                    ),
                )
                .child(
                    kit::tabular(div())
                        .text_size(self.z(theme.typography.small()))
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(format!("{} of {of}", at.saturating_add(1)))),
                )
                .child(
                    self.icon_button("asked-next", Symbol::ChevronDown, "Next request")
                        .on_click(cx.listener(move |this, _ev, _w, cx| this.step_asked(1, of, cx))),
                )
        });
        let id = request.id.0.clone();
        // The way back to the agent's own prompt stands on the title's line, apart from the
        // answers: it is not one of them, and a row of five wrapped. A request with nothing to
        // answer here has only that line. A questionnaire keeps its buttons together.
        let (inline, body) = if let Some(asking) = asking {
            let mut answers: Vec<AnyElement> = self.deny_row(request, cx).map_or_else(
                || {
                    super::decision::arrange(&request.options)
                        .front
                        .into_iter()
                        .map(|(choice, kind)| {
                            self.choice_button(request, choice, kind, cx).into_any_element()
                        })
                        .collect()
                },
                |row| vec![row],
            );
            answers.extend(release);
            let body = div()
                .min_h_0()
                .flex()
                .flex_col()
                .px(self.z(theme.spacing.md))
                .pb(self.z(theme.spacing.md))
                .child(asking.questions().element(theme, answers))
                .into_any_element();
            (None, Some(body))
        } else {
            let body = self.decision(request, None, cx).map(|decision| {
                div()
                    .flex_none()
                    .px(self.z(theme.spacing.md))
                    .pb(self.z(theme.spacing.md))
                    .child(decision)
                    .into_any_element()
            });
            (release, body)
        };
        div()
            .id(ElementId::Name(format!("request-{id}").into()))
            .debug_selector(move || format!("request-{id}"))
            .role(Role::Dialog)
            .aria_label(SharedString::from(if carded {
                "Plan ready".to_owned()
            } else {
                request.title.clone()
            }))
            .w_full()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                self.section()
                    .flex_none()
                    .min_h(self.z(theme.density.header))
                    .px(self.z(theme.spacing.md))
                    .child(self.needs_you())
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .map(|el| kit::typed(el, theme.roles().task_title, self.zoom))
                            .text_color(hsla(s.text))
                            .child(SharedString::from(title)),
                    )
                    .children(counter.map(|c| {
                        kit::tabular(div())
                            .flex_none()
                            .text_size(self.z(theme.typography.small()))
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(c))
                    }))
                    .children(back)
                    .children(stepper)
                    .children(inline),
            )
            .children(text.map(|t| {
                // What it asks is read before it is answered: at the chrome's size, a full
                // step of space above the answers.
                // Where the room is short it scrolls, and the answers under it stay.
                let el = div()
                    .id("request-text")
                    .min_h_0()
                    .overflow_y_scroll()
                    .mx(self.z(theme.spacing.md))
                    .mb(self.z(theme.spacing.md))
                    .whitespace_normal()
                    .map(|el| kit::typed(el, theme.roles().chrome, self.zoom));
                if code {
                    el.px(self.z(theme.spacing.sm))
                        .py(self.z(theme.spacing.xs))
                        .rounded(self.z(theme.radii.sm))
                        .bg(hsla(s.panel))
                        .font_family(self.mono())
                        .text_color(hsla(s.text))
                } else {
                    el.text_color(hsla(s.text_secondary))
                }
                .child(SharedString::from(tail(&t, PEEK_LINES)))
            }))
            .children(body)
            .into_any_element()
    }

    fn plan_section(&self, plan: &slopty_proto::thread::Plan, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let done = plan.steps.iter().filter(|st| st.status == STEP_DONE).count();
        let total = plan.steps.len();
        let open = self.plan_open;
        let head = self
            .section()
            .id("thread-plan")
            .debug_selector(|| "thread-plan".to_owned())
            .role(Role::Button)
            .aria_label(SharedString::from(format!("Plan, {done} of {total} done")))
            .aria_expanded(open)
            .cursor_pointer()
            .text_color(hsla(s.text_secondary))
            .child(self.slot().child(self.icon(Symbol::Checklist, s.text_muted)))
            .child(div().flex_none().child("Plan"))
            .child(
                kit::tabular(div())
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(format!("{done} of {total} done"))),
            )
            .child(div().flex_1())
            .child(
                self.icon(if open { Symbol::ChevronDown } else { Symbol::ChevronUp }, s.text_muted),
            )
            .on_click(cx.listener(|this, _ev, _w, cx| {
                this.plan_open = !this.plan_open;
                cx.notify();
            }));
        let steps = open.then(|| {
            div().w_full().flex().flex_col().pb(self.z(theme.spacing.xs)).children(
                plan.steps.iter().map(|step| {
                    let (icon, tone) = match step.status.as_str() {
                        STEP_DONE => (Symbol::CheckmarkCircle, s.text_muted),
                        "in_progress" => (Symbol::CircleInsetFilled, s.text),
                        _ => (Symbol::Circle, s.text_muted),
                    };
                    self.section()
                        .text_color(hsla(tone))
                        .child(self.slot().child(self.icon(icon, tone)))
                        .child(
                            div()
                                .min_w_0()
                                .whitespace_normal()
                                .child(SharedString::from(step.text.clone())),
                        )
                }),
            )
        });
        div().w_full().flex().flex_col().child(head).children(steps).into_any_element()
    }

    fn edited_section(
        &self,
        edited: &[crate::conversation::thread::activity::Edited],
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let (added, removed) = edited.iter().fold((0_u32, 0_u32), |(a, r), e| {
            (a.saturating_add(e.added), r.saturating_add(e.removed))
        });
        let words = match edited {
            [one] => format!("Edits \u{b7} {}", one.path.rsplit('/').next().unwrap_or(&one.path)),
            many => format!("Edits \u{b7} {} files", many.len()),
        };
        let thread = self.thread;
        self.section()
            .id("thread-edited")
            .debug_selector(|| "thread-edited".to_owned())
            .role(Role::Group)
            .aria_label(SharedString::from(words.clone()))
            .text_color(hsla(s.text_secondary))
            .child(self.slot().child(self.icon(Symbol::Pencil, s.text_muted)))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(words)),
            )
            .when(added > 0 || removed > 0, |el| el.child(kit::separator(theme)))
            .children(kit::changes(theme, added, removed))
            .child(div().flex_1())
            .child(self.button("thread-review", "Review", ButtonKind::Ghost).on_click(cx.listener(
                move |_this, _ev, _w, cx| {
                    cx.emit(ThreadViewEvent::Review { thread });
                },
            )))
            .into_any_element()
    }

    /// Over a queue the person's stop holds: it goes on with their next message.
    fn paused_line(&self) -> AnyElement {
        let s = self.theme.surfaces;
        let words = "Queue paused \u{b7} sends after your next message";
        self.section()
            .id("thread-queue-paused")
            .debug_selector(|| "thread-queue-paused".to_owned())
            .role(Role::Status)
            .aria_label(SharedString::from(words.replace('\u{b7}', ",")))
            .text_size(self.z(self.theme.typography.small()))
            .text_color(hsla(s.text_muted))
            .child(self.slot().child(self.icon(Symbol::PauseCircle, s.text_muted)))
            .child(SharedString::from(words))
            .into_any_element()
    }

    /// A waiting message: its first line, where it is (when one held for later goes, or what
    /// it waits on), and the ways to change it, send it now or take it back while the worker
    /// holds it.
    fn queued_line(
        &self,
        queued: &crate::conversation::thread::activity::Queued,
        can_change: bool,
        can_promote: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let s = self.theme.surfaces;
        let pending = queued.intent;
        let refused = match &queued.edit {
            Some(Edit::Refused { intent, text, reason }) => Some((*intent, text.clone(), reason)),
            Some(Edit::Sending) | None => None,
        };
        let state = match (&queued.held, queued.on_worker, queued.withdrawing, &refused) {
            (_, _, true, _) => Some("Taking back".to_owned()),
            (.., Some((_, _, reason))) => Some(format!("Not changed: {reason}")),
            // The line over the queue says it once for all of them.
            (Some(_), ..) if queued.stopped => None,
            (Some(why), ..) => Some(why.clone()),
            (None, false, ..) if queued.delivery == Delivery::Interrupt => {
                Some("Stopping the turn to send".to_owned())
            }
            (None, false, ..) if !queued.delivery.is_kept() => Some("Sending".to_owned()),
            (None, ..) => super::later::when_words(queued.delivery, crate::clock::now(cx)),
        };
        let scheduled = queued.delivery.is_kept();
        let open = can_change && queued.on_worker && !queued.withdrawing;
        let editable = open && !queued.going && !self.composing.editing();
        // The words the composer takes: a refused change's, so they are not lost.
        let words = refused.as_ref().map_or_else(|| queued.text.clone(), |(_, t, _)| t.clone());
        let first = queued_words(queued);
        let files = queued.attachments.len();
        let with_words = !kit::first_line(&queued.text).is_empty();
        let mut said = first.clone();
        if files > 0 && with_words {
            let files = u64::try_from(files).unwrap_or(u64::MAX);
            said = format!("{said}, {}", kit::count(files, "file", "files"));
        }
        if let Some(st) = &state {
            said = format!("{said}, {st}");
        }
        self.section()
            .id(ElementId::Name(format!("queued-{pending}").into()))
            .debug_selector(move || format!("queued-{pending}"))
            .role(Role::ListItem)
            .aria_label(SharedString::from(said))
            .text_color(hsla(s.text_secondary))
            .child(self.slot().child(self.icon(Symbol::Clock, s.text_muted)))
            .child(kit::fit_label(format!("queued-words-{pending}"), first, &self.theme))
            .when(files > 0 && with_words, |el| {
                el.child(
                    div()
                        .debug_selector(move || format!("queued-files-{pending}"))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(self.z(self.theme.spacing.xxs))
                        .text_size(self.z(self.theme.typography.small()))
                        .text_color(hsla(s.text_muted))
                        .child(self.icon(Symbol::Paperclip, s.text_muted))
                        .child(SharedString::from(files.to_string())),
                )
            })
            .children(state.map(|st| {
                div()
                    .flex_none()
                    .max_w(relative(0.5))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(self.z(self.theme.typography.small()))
                    .text_color(hsla(if refused.is_some() { s.warn } else { s.text_muted }))
                    .child(SharedString::from(st))
            }))
            .when(editable, |el| {
                el.child(
                    self.icon_button(format!("edit-{pending}"), Symbol::Pencil, "Edit").on_click(
                        cx.listener(move |this, _ev, window, cx| {
                            this.start_edit(pending, &words, window, cx);
                        }),
                    ),
                )
            })
            .when_some(refused.map(|(intent, ..)| intent), |el, intent| {
                el.child(
                    self.icon_button(format!("edit-dismiss-{pending}"), Symbol::Xmark, "Dismiss")
                        .on_click(cx.listener(move |this, _ev, _w, cx| this.dismiss(intent, cx))),
                )
            })
            .when(open && (scheduled || queued.stopped) && can_promote && !queued.going, |el| {
                el.child(
                    self.icon_button(format!("promote-{pending}"), Symbol::ArrowUp, "Send now")
                        .on_click(cx.listener(move |this, _ev, _w, cx| {
                            let _id = this.intent(Intent::Promote { pending }, cx);
                        })),
                )
            })
            .when(open && queued.edit.is_none(), |el| {
                el.child(
                    self.icon_button(format!("withdraw-{pending}"), Symbol::Xmark, "Take back")
                        .on_click(cx.listener(move |this, _ev, _w, cx| {
                            let _id = this.intent(Intent::Withdraw { pending }, cx);
                        })),
                )
            })
            .into_any_element()
    }

    /// The commands run in the background, each with how it stands and its last line.
    fn background_section(
        &self,
        background: &[crate::conversation::thread::activity::Background<'_>],
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let lines = background.iter().map(|bg| {
            let state = match bg.detail.status {
                ExecStatus::Running => "Running".to_owned(),
                ExecStatus::Done => bg.detail.duration_ms.map_or_else(
                    || "Done".to_owned(),
                    |ms| format!("Done \u{b7} {}", kit::duration(Duration::from_millis(ms))),
                ),
                ExecStatus::Failed => "Failed".to_owned(),
                ExecStatus::Interrupted => "Stopped".to_owned(),
            };
            let running = matches!(bg.detail.status, ExecStatus::Running);
            let last = bg.output.and_then(|o| o.lines().rev().find(|l| !l.trim().is_empty()));
            let label = SharedString::from(format!("{}: {state}", bg.title));
            let head = div()
                .w_full()
                .flex()
                .items_center()
                .gap(self.z(theme.spacing.xs))
                .child(self.slot().child(if running {
                    self.spinner(true)
                } else {
                    self.icon(Symbol::Terminal, s.text_muted)
                }))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(SharedString::from(bg.title.to_owned())),
                )
                .child(
                    kit::tabular(div())
                        .flex_none()
                        .text_size(self.z(theme.typography.small()))
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(state)),
                );
            self.two_lines(head, last.filter(|_| running).map(|l| l.trim().to_owned()))
                .id(ElementId::Name(format!("background-{}", bg.item.0).into()))
                .debug_selector({
                    let id = bg.item.0.clone();
                    move || format!("background-{id}")
                })
                .role(Role::Status)
                .aria_label(label)
                .text_color(hsla(s.text_secondary))
        });
        div()
            .id("thread-background")
            .role(Role::List)
            .aria_label("In the background")
            .w_full()
            .flex()
            .flex_col()
            .children(lines)
            .into_any_element()
    }

    /// The meter's panel: a line for the context and each of the plan's windows with when it
    /// resets, and "Compact context" where the agent compacts through
    /// Slopty's door ([`Cap::COMPACT`]). Compacting is the person's press, never Slopty's.
    fn meter_panel(
        &self,
        state: &slopty_proto::thread::ThreadState,
        cx: &Context<Self>,
    ) -> AnyElement {
        let s = self.theme.surfaces;
        let meters = &state.meters;
        let lines = super::composer::meter_words(meters, crate::clock::now(cx));
        let compact = super::composing::compacts(state).then(|| {
            self.button("thread-compact", "Compact context", ButtonKind::Ghost).on_click(
                cx.listener(|this, _ev, _w, cx| {
                    let _id = this.intent(Intent::Compact, cx);
                    this.meter_open = false;
                    cx.notify();
                }),
            )
        });
        let label = SharedString::from(lines.join(", "));
        self.section()
            .id("thread-meter-panel")
            .debug_selector(|| "thread-meter-panel".to_owned())
            .role(Role::Group)
            .aria_label(label)
            .items_start()
            .py(self.z(self.theme.spacing.xs))
            .text_color(hsla(s.text_secondary))
            .child(self.slot().child(self.icon(Symbol::InfoCircle, s.text_muted)))
            .child(
                kit::tabular(div())
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .children(lines.into_iter().map(|l| div().child(SharedString::from(l)))),
            )
            .children(compact)
            .into_any_element()
    }

    /// One piece of background work in the panel: its kind, what it is, the end of what it
    /// printed, how it stands and for how long, and the way to stop it while it runs.
    fn task_line(&self, task: &BackgroundTask, can_stop: bool, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let id = task.id.clone();
        let running = task.is_running();
        let stopping = self.hub.read(cx).threads().unshown(self.thread).any(|sent| {
            matches!(&sent.intent, Intent::StopTask { task: t } if *t == id) && !sent.failed()
        });
        let mark = if running {
            self.spinner(true)
        } else {
            let icon = if task.kind == BackgroundTask::AGENT {
                Symbol::RectangleSplit3x1
            } else {
                Symbol::Terminal
            };
            self.icon(icon, s.text_muted)
        };
        let last = task
            .output
            .as_ref()
            .and_then(|o| o.text.lines().rev().find(|l| !l.trim().is_empty()))
            .map(|l| l.trim().to_owned());
        let took = (!task.started_ms.is_zero()).then(|| {
            let end = task.ended_ms.unwrap_or_else(slopty_core::WallMs::now);
            kit::duration(Duration::from_secs(end.millis_since(task.started_ms) / 1_000))
        });
        let state = if stopping { "Stopping".to_owned() } else { sentence(&task.state) };
        let standing = took.map_or_else(|| state.clone(), |t| format!("{state} \u{b7} {t}"));
        let tag = id.clone();
        let label = SharedString::from(format!("{}: {state}", task.title));
        let head = div()
            .w_full()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .child(self.slot().child(mark))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(task.title.clone())),
            )
            .child(
                kit::tabular(div())
                    .flex_none()
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(standing)),
            )
            .when(running && can_stop && !stopping, |el| {
                el.child(
                    self.button(format!("stop-task-{id}"), "Stop", ButtonKind::Ghost).on_click(
                        cx.listener(move |this, _ev, _w, cx| {
                            let _id = this.intent(Intent::StopTask { task: id.clone() }, cx);
                        }),
                    ),
                )
            });
        self.two_lines(head, last.filter(|_| running))
            .id(ElementId::Name(format!("task-{tag}").into()))
            .debug_selector(move || format!("task-{tag}"))
            .role(Role::Status)
            .aria_label(label)
            .text_color(hsla(s.text_secondary))
            .into_any_element()
    }
}

/// How long a count of new rows holds before the way down announces it, so a run of
/// arrivals is said once.
pub(super) const TELL_AFTER: Duration = Duration::from_millis(700);

/// What the way down says of `n` new rows: "3 new", capped at "99+ new".
pub(super) fn new_words(n: usize) -> String {
    format!("{} {NEW_TAIL}", new_figure(n))
}

/// The word after the way down's figure: "3 new" is one phrase that starts with the figure,
/// so its word is lower case.
const NEW_TAIL: &str = "new";

/// The most new rows the way down counts one by one.
const NEW_MOST: usize = 99;

/// The figure of [`new_words`]: "3", "99+".
fn new_figure(n: usize) -> String {
    if n > NEW_MOST { format!("{NEW_MOST}+") } else { n.to_string() }
}

/// How the work in the background stands, in a few words: how much runs while any does,
/// else how it ended ("1 finished", "2 finished · 1 failed"), never "in the background" for
/// work that is over.
pub(super) fn tasks_words<'a>(tasks: impl IntoIterator<Item = &'a BackgroundTask>) -> String {
    let (mut running, mut finished, mut failed, mut stopped, mut ended) = (0, 0, 0, 0, 0_usize);
    for task in tasks {
        let n = match task.state.as_str() {
            BackgroundTask::RUNNING => &mut running,
            BackgroundTask::COMPLETED => &mut finished,
            BackgroundTask::FAILED => &mut failed,
            BackgroundTask::KILLED => &mut stopped,
            _ => &mut ended,
        };
        *n = n.saturating_add(1);
    }
    if running > 0 {
        return format!("{running} running");
    }
    let parts: Vec<String> =
        [(finished, "finished"), (failed, "failed"), (stopped, "stopped"), (ended, "ended")]
            .into_iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, word)| format!("{n} {word}"))
            .collect();
    parts.join(" \u{b7} ")
}

/// What the way back to the agent's own prompt says: the terminal its prompt runs in, or the
/// agent's own TUI opened on the thread; `None` where it has neither.
fn release_words(meta: &slopty_proto::thread::ThreadMeta) -> Option<String> {
    let joins = meta.can(Cap::LIVE_TUI) && meta.drive.is(Drive::SHARED);
    if joins {
        return Some(format!("Answer in {}", super::agent_name(&meta.agent)));
    }
    meta.terminal.is_some().then(|| "Answer in the terminal".to_owned())
}

/// What a waiting message's line says of it: its first line, or the names of its files when it
/// is files alone.
fn queued_words(queued: &crate::conversation::thread::activity::Queued) -> String {
    let first = kit::first_line(&queued.text);
    if !first.is_empty() || queued.attachments.is_empty() {
        return first.to_owned();
    }
    let names: Vec<&str> =
        queued.attachments.iter().map(|p| p.rsplit('/').next().unwrap_or(p)).collect();
    names.join(", ")
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::thread::BackgroundTask as T;

    use super::{new_words, tasks_words};

    /// The way down counts new rows to 99, then says "99+ new".
    #[test]
    fn a_count_of_new_rows_stops_at_99() {
        assert_eq!(new_words(3), "3 new");
        assert_eq!(new_words(99), "99 new");
        assert_eq!(new_words(100), "99+ new");
    }

    /// Work in the background says how much runs while any does, else how it ended, so a
    /// finished task never reads as still running.
    #[test]
    fn background_work_says_what_is_true() {
        let task = |state: &str| T {
            id: String::new(),
            kind: T::SHELL.to_owned(),
            title: String::new(),
            state: state.to_owned(),
            item: None,
            output: None,
            started_ms: WallMs::from_millis(0),
            ended_ms: None,
        };
        let words =
            |states: &[&str]| tasks_words(&states.iter().map(|s| task(s)).collect::<Vec<_>>());
        assert_eq!(words(&[T::RUNNING, T::COMPLETED]), "1 running");
        assert_eq!(words(&[T::COMPLETED]), "1 finished");
        assert_eq!(words(&[T::COMPLETED, T::FAILED, T::COMPLETED]), "2 finished \u{b7} 1 failed");
        assert_eq!(words(&[T::KILLED]), "1 stopped");
        assert_eq!(words(&["lost"]), "1 ended");
    }
}
