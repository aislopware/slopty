//! The tray on the composer's top edge: what waits on the person, the plan, the edits of the
//! last turn, the messages waiting to go, and the work in the background, stacked and parted
//! by hairlines.
//!
//! A request whose call is on screen is answered on the call's own card ([`Placement`]); the
//! tray carries a copy of it only while that card is scrolled away, with the way back to it,
//! and always carries a request that belongs to no call. An approval's answers are quiet,
//! each with a small mark of what it means, and the one plain allow is the single solid.

use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, Div, ElementId, FollowMode, FontWeight, InteractiveElement as _,
    IntoElement as _, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, div, relative,
};
use slopty_proto::thread::detail::ExecStatus;
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{Choice, Effect, ItemId, Request};
use slopty_theme::{Rgb, Theme, Typography};

use super::{PEEK_LINES, ThreadView, ThreadViewEvent, tail};
use crate::colors::hsla;
use crate::conversation::thread::activity::{Activity, Asked, Edit, STEP_DONE};
use crate::conversation::thread::questions;
use crate::conversation::thread::rows::Row;
use crate::icons::IconName;
use crate::kit::{self, ButtonKind};

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
    fn placement_of(&self, row: usize) -> Placement {
        match (self.list.item_is_above_viewport(row), self.list.item_is_below_viewport(row)) {
            (Some(true), _) => Placement::Above,
            (_, Some(true)) => Placement::Below,
            (Some(false), Some(false)) => Placement::Inline,
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
        Marks { asked, down: self.list.is_scrolled_to_end() == Some(false) }
    }

    /// Draw again once the list's layout moved what the frame drew from it: the request's
    /// place, the way down. Reads only the list, so a frame that changes neither costs no
    /// second one.
    pub(super) fn recheck_marks(&self, cx: &mut Context<Self>) {
        let was = self.marks.get();
        let now = Marks {
            asked: was.asked.map(|(row, _)| (row, self.placement_of(row))),
            down: self.list.is_scrolled_to_end() == Some(false),
        };
        if now != was {
            cx.notify();
        }
    }

    /// Whether the request on show is answered on the card of the call in row `row`.
    pub(super) fn answered_inline(&self, row: usize) -> bool {
        self.marks.get().asked == Some((row, Placement::Inline))
    }

    /// The round way down to the newest row, over the list's foot while it is scrolled up.
    pub(super) fn down_button(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
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
                    .size(self.z(theme.density.control))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .border_color(hsla(s.border))
                    .bg(hsla(s.elevated))
                    .cursor_pointer()
                    .hover(move |el| el.bg(hsla(s.raised)))
                    .child(self.icon(IconName::ChevronDown, s.text_secondary))
                    .on_click(cx.listener(|this, _ev, _w, cx| {
                        this.list.scroll_to_end();
                        this.list.set_follow_mode(FollowMode::Tail);
                        cx.notify();
                    })),
                s.accent,
            ))
            .into_any_element()
    }

    /// The tray, when anything waits in it: tucked behind the composer's top edge, or a card
    /// of its own where there is no composer (a subagent's thread).
    ///
    /// A request on show stands whole; what else waits stands at most `max` tall and scrolls
    /// past that, so the thread above keeps its share of the tile however much waits.
    pub(super) fn activity_bar(
        &self,
        tucked: bool,
        max: gpui::Pixels,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
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
        let mut sections: Vec<AnyElement> = Vec::new();
        for asked in bar.asked.iter().filter(|a| a.answered.is_some()) {
            sections.push(self.answered_line(asked));
        }
        if let Some(plan) = bar.plan {
            sections.push(self.plan_section(plan, cx));
        }
        if !bar.edited.is_empty() {
            sections.push(self.edited_section(&bar.edited, cx));
        }
        for queued in &bar.queue {
            sections.push(self.queued_line(queued, bar.can_withdraw, cx));
        }
        if !bar.background.is_empty() {
            sections.push(self.background_section(&bar.background));
        }
        for task in &bar.tasks {
            sections.push(self.task_line(task, bar.can_stop, cx));
        }
        if sections.is_empty() && request.is_none() {
            return None;
        }
        let parted = request.is_some();
        let rest = (!sections.is_empty()).then(|| {
            div()
                .id("thread-activity-rest")
                .w_full()
                .flex()
                .flex_col()
                .max_h(max)
                .overflow_y_scroll()
                .when(parted, |el| el.border_t_1().border_color(hsla(s.border_subtle)))
                .children(sections.into_iter().enumerate().map(|(ix, section)| {
                    div()
                        .w_full()
                        .when(ix > 0, |el| el.border_t_1().border_color(hsla(s.border_subtle)))
                        .child(section)
                }))
        });
        let radius = self.z(theme.radii.md);
        Some(
            div()
                .id("thread-activity")
                .debug_selector(|| "thread-activity".to_owned())
                .role(Role::Group)
                .aria_label("Activity")
                .flex()
                .flex_col()
                .rounded_tl(radius)
                .rounded_tr(radius)
                .border_t_1()
                .border_l_1()
                .border_r_1()
                // A sheet tucked behind the composer: inset by the composer's corner, so its
                // square foot meets the composer's straight top edge.
                .when(tucked, |el| el.mx(self.z(theme.radii.lg)))
                .when(!tucked, |el| {
                    el.border_b_1().rounded_bl(radius).rounded_br(radius).mb(self.z(theme.spacing.xs))
                })
                .border_color(hsla(s.border))
                .bg(hsla(s.raised))
                .overflow_hidden()
                .children(request)
                .children(rest)
                .into_any_element(),
        )
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

    fn answered_line(&self, asked: &Asked<'_>) -> AnyElement {
        let s = self.theme.surfaces;
        let choice = asked.answered.and_then(|sent| match &sent.intent {
            Intent::Answer { choice, .. } => Some(choice.clone()),
            Intent::Release { .. } => Some("Answer in the terminal".to_owned()),
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
            .child(self.slot().child(self.icon(IconName::Check, s.text_muted)))
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

    /// The answers `request` offers as buttons, with the way back to the agent's own prompt
    /// where it has one in a terminal; a request that offers nothing here (a secret Codex
    /// keeps to its own terminal) makes that way its one solid.
    pub(super) fn answer_buttons(&self, request: &Request, cx: &Context<Self>) -> Vec<AnyElement> {
        let ask = request.id.clone();
        let mut answers: Vec<AnyElement> = Vec::new();
        for (choice, kind) in request.options.iter().zip(answer_kinds(&request.options)) {
            let (ask, id) = (ask.clone(), choice.id.clone());
            answers.push(
                self.answer_button(
                    format!("answer-{}-{}", ask.0, choice.id),
                    answer_label(choice),
                    kind,
                    answer_mark(choice, kind, &self.theme),
                )
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    this.answer(ask.clone(), id.clone(), cx);
                }))
                .into_any_element(),
            );
        }
        // Only an agent whose own prompt runs in a terminal can take the request back there.
        if self.state(cx).is_some_and(|st| st.meta.terminal.is_some()) {
            let only_way = request.options.is_empty() && request.questions.is_empty();
            let kind = if only_way { ButtonKind::Primary } else { ButtonKind::Ghost };
            let release = ask;
            answers.push(
                self.answer_button(
                    format!("release-{}", release.0),
                    "Answer in the terminal".to_owned(),
                    kind,
                    Some((IconName::SquareTerminal, None)),
                )
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    let _id = this.intent(Intent::Release { ask: release.clone() }, cx);
                }))
                .into_any_element(),
            );
        }
        answers
    }

    /// A button of a request's: its small mark (in its tone, or the button's own), then its
    /// words.
    fn answer_button(
        &self,
        id: String,
        label: String,
        kind: ButtonKind,
        mark: Option<(IconName, Option<Rgb>)>,
    ) -> gpui::Stateful<Div> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let ink = match kind {
            ButtonKind::Primary => s.solid_ink,
            ButtonKind::Secondary => s.text,
            ButtonKind::Ghost | ButtonKind::Link => s.text_secondary,
        };
        let mark = mark.map(|(icon, tone)| {
            crate::icons::icon(
                theme,
                icon,
                crate::icons::IconSize::Inline,
                hsla(tone.unwrap_or(ink)),
            )
            .size(self.z(theme.typography.small()))
        });
        self.button_frame(id, label.clone(), kind)
            .gap(self.z(theme.spacing.xs))
            .px(self.z(theme.spacing.sm))
            .children(mark)
            .child(SharedString::from(label))
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
        let answers = self.answer_buttons(request, cx);
        let asking = self.asking.as_ref().filter(|a| *a.ask() == request.id);
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
            None => (request.title.clone(), None),
        };
        let text = request.text.as_ref().map(|t| t.text.clone()).or_else(|| {
            asking.is_none().then(|| request.questions.first().map(|q| q.text.clone())).flatten()
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
                    self.icon_button("asked-prev", IconName::ChevronUp, "Previous request")
                        .on_click(
                            cx.listener(move |this, _ev, _w, cx| this.step_asked(-1, of, cx)),
                        ),
                )
                .child(
                    kit::tabular(div())
                        .text_size(self.z(theme.typography.meta()))
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(format!("{} of {of}", at.saturating_add(1)))),
                )
                .child(
                    self.icon_button("asked-next", IconName::ChevronDown, "Next request")
                        .on_click(cx.listener(move |this, _ev, _w, cx| this.step_asked(1, of, cx))),
                )
        });
        let id = request.id.0.clone();
        let body = match asking {
            Some(asking) => div()
                .px(self.z(theme.spacing.md))
                .pb(self.z(theme.spacing.md))
                .child(asking.questions().element(theme, answers))
                .into_any_element(),
            None => self.answers_row(answers).into_any_element(),
        };
        div()
            .id(ElementId::Name(format!("request-{id}").into()))
            .debug_selector(move || format!("request-{id}"))
            .role(Role::Dialog)
            .aria_label(SharedString::from(request.title.clone()))
            .w_full()
            .flex()
            .flex_col()
            .child(
                self.section()
                    .min_h(self.z(theme.density.header))
                    .px(self.z(theme.spacing.md))
                    .child(self.spinner(true))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                            .text_color(hsla(s.text))
                            .child(SharedString::from(title)),
                    )
                    .children(counter.map(|c| {
                        kit::tabular(div())
                            .flex_none()
                            .text_size(self.z(theme.typography.meta()))
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(c))
                    }))
                    .children(back)
                    .children(stepper),
            )
            .children(text.map(|t| {
                let el = div()
                    .mx(self.z(theme.spacing.md))
                    .mb(self.z(theme.spacing.sm))
                    .whitespace_normal()
                    .text_size(self.z(theme.typography.small()));
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
            .child(body)
            .into_any_element()
    }

    /// A request's answers in a row of their own under a hairline, the quiet ones first and
    /// the solid last, at the right.
    pub(super) fn answers_row(&self, answers: Vec<AnyElement>) -> Div {
        let theme = &self.theme;
        div()
            .w_full()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_end()
            .gap(self.z(theme.spacing.xxs))
            .p(self.z(theme.spacing.xs))
            .border_t_1()
            .border_color(hsla(theme.surfaces.border_subtle))
            .children(answers)
    }

    fn plan_section(&self, plan: &slopty_proto::thread::Plan, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let done = plan.steps.iter().filter(|st| st.status == STEP_DONE).count();
        let total = plan.steps.len();
        let open = self.plan_open;
        let head =
            self.section()
                .id("thread-plan")
                .role(Role::Button)
                .aria_label(SharedString::from(format!("Plan, {done} of {total} done")))
                .aria_expanded(open)
                .cursor_pointer()
                .text_color(hsla(s.text_secondary))
                .child(self.slot().child(self.icon(IconName::ListTodo, s.text_muted)))
                .child(div().flex_none().child("Plan"))
                .child(
                    kit::tabular(div())
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(format!("{done} of {total} done"))),
                )
                .child(div().flex_1())
                .child(self.icon(
                    if open { IconName::ChevronDown } else { IconName::ChevronUp },
                    s.text_muted,
                ))
                .on_click(cx.listener(|this, _ev, _w, cx| {
                    this.plan_open = !this.plan_open;
                    cx.notify();
                }));
        let steps = open.then(|| {
            div().w_full().flex().flex_col().pb(self.z(theme.spacing.xs)).children(
                plan.steps.iter().map(|step| {
                    let (icon, tone) = match step.status.as_str() {
                        STEP_DONE => (IconName::CircleCheck, s.text_muted),
                        "in_progress" => (IconName::CircleDot, s.text),
                        _ => (IconName::Circle, s.text_muted),
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
            .child(self.slot().child(self.icon(IconName::FilePen, s.text_muted)))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(words)),
            )
            .when(added > 0 || removed > 0, |el| el.child(kit::separator(theme)))
            .children(kit::changes_at(theme, added, removed, self.zoom))
            .child(div().flex_1())
            .child(self.button("thread-review", "Review", ButtonKind::Ghost).on_click(cx.listener(
                move |_this, _ev, _w, cx| {
                    cx.emit(ThreadViewEvent::Review { thread });
                },
            )))
            .into_any_element()
    }

    /// A waiting message: its first line, where it is, and the ways to change it or take it
    /// back while the worker holds it.
    fn queued_line(
        &self,
        queued: &crate::conversation::thread::activity::Queued,
        can_change: bool,
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
            (Some(why), ..) => Some(why.clone()),
            (None, false, ..) => Some("Sending".to_owned()),
            (None, true, false, None) => None,
        };
        let open = can_change && queued.on_worker && !queued.withdrawing;
        let editable = open && !queued.going && !self.composing.editing();
        // The words the composer takes: a refused change's, so they are not lost.
        let words = refused.as_ref().map_or_else(|| queued.text.clone(), |(_, t, _)| t.clone());
        self.section()
            .debug_selector(move || format!("queued-{pending}"))
            .text_color(hsla(s.text_secondary))
            .child(self.slot().child(self.icon(IconName::Clock, s.text_muted)))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(kit::first_line(&queued.text).to_owned())),
            )
            .children(state.map(|st| {
                div()
                    .flex_none()
                    .max_w(relative(0.5))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(self.z(self.theme.typography.meta()))
                    .text_color(hsla(if refused.is_some() { s.warn } else { s.text_muted }))
                    .child(SharedString::from(st))
            }))
            .when(editable, |el| {
                el.child(
                    self.icon_button(format!("edit-{pending}"), IconName::Pencil, "Edit").on_click(
                        cx.listener(move |this, _ev, window, cx| {
                            this.start_edit(pending, &words, window, cx);
                        }),
                    ),
                )
            })
            .when_some(refused.map(|(intent, ..)| intent), |el, intent| {
                el.child(
                    self.icon_button(format!("edit-dismiss-{pending}"), IconName::X, "Dismiss")
                        .on_click(cx.listener(move |this, _ev, _w, cx| this.dismiss(intent, cx))),
                )
            })
            .when(open && queued.edit.is_none(), |el| {
                el.child(
                    self.icon_button(format!("withdraw-{pending}"), IconName::X, "Take back")
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
            self.section()
                .id(ElementId::Name(format!("background-{}", bg.item.0).into()))
                .debug_selector({
                    let id = bg.item.0.clone();
                    move || format!("background-{id}")
                })
                .role(Role::Status)
                .aria_label(label)
                .text_color(hsla(s.text_secondary))
                .child(self.slot().child(if running {
                    self.spinner(true)
                } else {
                    self.icon(IconName::Terminal, s.text_muted)
                }))
                .child(div().flex_none().child(SharedString::from(bg.title.to_owned())))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .font_family(self.mono())
                        .text_size(self.z(theme.typography.meta()))
                        .text_color(hsla(s.text_muted))
                        .children(last.map(|l| SharedString::from(l.trim().to_owned()))),
                )
                .child(
                    kit::tabular(div())
                        .flex_none()
                        .text_size(self.z(theme.typography.meta()))
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(state)),
                )
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

    fn task_line(
        &self,
        task: &slopty_proto::thread::BackgroundTask,
        can_stop: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let s = self.theme.surfaces;
        let id = task.id.clone();
        let stopping = self.hub.read(cx).threads().unshown(self.thread).any(|sent| {
            matches!(&sent.intent, Intent::StopTask { task: t } if *t == id) && !sent.failed()
        });
        self.section()
            .text_color(hsla(s.text_secondary))
            .child(self.slot().child(self.spinner(true)))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(task.title.clone())),
            )
            .when(stopping, |el| el.child(div().text_color(hsla(s.text_muted)).child("Stopping")))
            .when(can_stop && !stopping, |el| {
                el.child(
                    self.button(format!("stop-task-{id}"), "Stop", ButtonKind::Ghost).on_click(
                        cx.listener(move |this, _ev, _w, cx| {
                            let _id = this.intent(Intent::StopTask { task: id.clone() }, cx);
                        }),
                    ),
                )
            })
            .into_any_element()
    }
}

/// How each answer's button reads: the first plain allow leads, the one solid; the rest are
/// quiet. An answer that reaches beyond this once ("always", a rule) never leads, so the card
/// never leads the person to a standing grant.
fn answer_kinds(options: &[Choice]) -> Vec<ButtonKind> {
    let lead = options.iter().position(|c| c.effect == Effect::Allow && c.scope.is_none());
    (0..options.len())
        .map(|ix| if Some(ix) == lead { ButtonKind::Primary } else { ButtonKind::Ghost })
        .collect()
}

/// An answer's words on its button, with how far it reaches when its words do not say it
/// already: "Always allow · Bash(cargo test:*)", but "Always allow" for an `always` scope.
fn answer_label(choice: &Choice) -> String {
    match choice.scope.as_deref().map(str::trim).filter(|scope| !scope.is_empty()) {
        Some(scope) if !choice.label.to_lowercase().contains(&scope.to_lowercase()) => {
            format!("{} \u{b7} {scope}", choice.label)
        }
        _ => choice.label.clone(),
    }
}

/// An answer's small mark: a check for a yes and a cross for a no, in their tones on a quiet
/// button and in the solid's own ink on the solid; none for an answer to a question.
fn answer_mark(
    choice: &Choice,
    kind: ButtonKind,
    theme: &Theme,
) -> Option<(IconName, Option<Rgb>)> {
    let s = theme.surfaces;
    let quiet = kind != ButtonKind::Primary;
    match choice.effect {
        Effect::Allow => Some((IconName::Check, quiet.then_some(s.success))),
        Effect::Deny => Some((IconName::X, quiet.then_some(s.error))),
        Effect::Answer => None,
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::thread::{Choice, Effect};

    use super::{answer_kinds, answer_label};
    use crate::kit::ButtonKind;

    /// An answer that reaches beyond this once says how far, unless its words already do.
    #[test]
    fn a_scoped_answer_says_how_far_it_reaches() {
        let choice = |label: &str, scope: Option<&str>| Choice {
            id: "a".to_owned(),
            label: label.to_owned(),
            effect: Effect::Allow,
            scope: scope.map(str::to_owned),
            stops: false,
        };
        assert_eq!(answer_label(&choice("Allow", None)), "Allow");
        assert_eq!(
            answer_label(&choice("Always allow", Some("Bash(cargo test:*)"))),
            "Always allow \u{b7} Bash(cargo test:*)"
        );
        assert_eq!(answer_label(&choice("Always Allow", Some("always"))), "Always Allow");
    }

    /// An ACP agent may offer "always" first: the plain allow still leads as the one solid,
    /// and every other answer is quiet, so the card never leads the person to a standing grant.
    #[test]
    fn a_standing_grant_never_leads_the_card() {
        let choice = |effect, scope: Option<&str>| Choice {
            id: "a".to_owned(),
            label: "a".to_owned(),
            effect,
            scope: scope.map(str::to_owned),
            stops: false,
        };
        let offered = [
            choice(Effect::Allow, Some("always")),
            choice(Effect::Allow, None),
            choice(Effect::Deny, Some("always")),
            choice(Effect::Deny, None),
        ];
        assert_eq!(
            answer_kinds(&offered),
            [ButtonKind::Ghost, ButtonKind::Primary, ButtonKind::Ghost, ButtonKind::Ghost]
        );
    }
}
