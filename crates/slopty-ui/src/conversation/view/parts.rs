//! The face's chrome around the list: the bar over a subagent's thread, the prompt rail, the
//! way back to the newest row, the task card, the line about the last permission prompt, the
//! permission card and the composer.

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Bounds, Context, ElementId, Focusable as _,
    InteractiveElement as _, IntoElement as _, ParentElement as _, PathBuilder, Pixels,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, canvas, div, point, px,
};
use gpui_kit::component::input::{Input, Textarea};
use slopty_proto::conversation::{ThreadId, Verdict};
use slopty_theme::{Rgb, Theme};

use super::ConversationView;
use super::entries::READING;
use crate::colors::hsla;
use crate::conversation::approval::{self, Outcome};
use crate::conversation::rows;
use crate::icons::{IconName, Status};
use crate::kit::{self, ButtonKind};

impl ConversationView {
    /// Over a subagent's thread: the way back, and whose thread it is.
    pub(super) fn thread_bar(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let ThreadId::Agent(id) = &self.thread else { return None };
        let theme = &self.theme;
        let s = theme.surfaces;
        let (name, kind) = self.model.subagent(id);
        let name = name.unwrap_or_else(|| format!("Subagent {id}"));
        let back = kit::icon_button_at(
            theme,
            "thread-back",
            IconName::ArrowLeft,
            "Back to the conversation",
            self.zoom,
        )
        .on_click(cx.listener(|this, _ev, _w, cx| this.open_thread(ThreadId::Main, cx)));
        Some(
            div()
                .id("thread-bar")
                .debug_selector(|| "thread-bar".to_owned())
                .role(Role::Navigation)
                .aria_label(SharedString::from(format!("Subagent {name}")))
                .flex_none()
                .w_full()
                .h(self.z(theme.density.row))
                .flex()
                .items_center()
                .gap(self.z(theme.spacing.xs))
                .px(self.z(theme.spacing.xs))
                .border_b_1()
                .border_color(hsla(s.border_subtle))
                .text_size(self.z(theme.typography.small()))
                .child(back)
                .child(div().text_color(hsla(s.text_muted)).child("Conversation"))
                .child(self.icon(IconName::ChevronRight, s.text_muted))
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(SharedString::from(name)),
                )
                .children(
                    kind.map(|k| div().text_color(hsla(s.text_muted)).child(SharedString::from(k))),
                )
                .into_any_element(),
        )
    }

    /// A tick per prompt down the right edge, at its place in the list: a click goes there,
    /// the pointer on one shows its words. Only with two prompts or more.
    pub(super) fn rail(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let prompts: Vec<(usize, String)> =
            rows::prompts(&self.rows).map(|(ix, id)| (ix, id.to_owned())).collect();
        if prompts.len() < 2 {
            return None;
        }
        let theme = &self.theme;
        let s = theme.surfaces;
        let total = self.rows.len().max(1);
        let thread = self.model.thread(&self.thread);
        let ticks = prompts.into_iter().enumerate().map(|(n, (ix, id))| {
            #[expect(clippy::cast_precision_loss, reason = "a position on screen")]
            let at = ix as f32 / total as f32;
            let words = thread
                .and_then(|t| t.entry(&id))
                .and_then(|e| match &e.body {
                    slopty_proto::conversation::Body::Prompt(p) => Some(p.text.text.clone()),
                    _ => None,
                })
                .map(|t| super::entries::first_line(&t))
                .unwrap_or_default();
            let hint = SharedString::from(words.chars().take(80).collect::<String>());
            let hint_theme = std::rc::Rc::new(self.theme.clone());
            let selector = format!("rail-{n}");
            div()
                .id(ElementId::Name(SharedString::from(selector.clone())))
                .debug_selector(move || selector)
                .role(Role::Button)
                .aria_label(SharedString::from(format!("Prompt {}: {hint}", n.saturating_add(1))))
                .absolute()
                .top(gpui::relative(at))
                .right_0()
                .w(self.z(theme.spacing.md))
                .h(self.z(theme.spacing.sm))
                .flex()
                .items_center()
                .justify_end()
                .cursor_pointer()
                .group("tick")
                .child(
                    div()
                        .w(self.z(theme.spacing.sm))
                        .h(px(2.0))
                        .rounded_full()
                        .bg(hsla(s.border))
                        .group_hover("tick", |el| {
                            el.bg(hsla(s.accent_fill)).w(self.z(theme.spacing.md))
                        }),
                )
                .tooltip(move |_window, cx| {
                    let hint = hint.clone();
                    let theme = std::rc::Rc::clone(&hint_theme);
                    cx.new(|_| kit::Hint::new(hint, "", theme)).into()
                })
                .on_click(cx.listener(move |this, _ev, _w, cx| this.scroll_to_row(ix, cx)))
        });
        Some(
            div()
                .debug_selector(|| "prompt-rail".to_owned())
                .absolute()
                .top(self.z(theme.spacing.md))
                .bottom(self.z(theme.spacing.md))
                .right(self.z(theme.spacing.xxs))
                .w(self.z(theme.spacing.md))
                .children(ticks)
                .into_any_element(),
        )
    }

    /// Over the list once the reader scrolled up: back to the newest row.
    pub(super) fn latest_pill(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let pill = div()
            .id("conversation-latest")
            .debug_selector(|| "conversation-latest".to_owned())
            .role(Role::Button)
            .aria_label("Jump to latest")
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .px(self.z(theme.spacing.sm))
            .py(self.z(theme.spacing.xxs))
            .rounded(self.z(theme.radii.md))
            .text_size(self.z(theme.typography.small()))
            .text_color(hsla(s.text_secondary))
            .cursor_pointer()
            .hover(move |el| el.text_color(hsla(s.text)))
            .child(self.icon(IconName::ArrowDown, s.text_secondary))
            .child("Latest");
        let pill = crate::a11y::tab_stop(kit::elevate(pill, theme), s.accent)
            .on_click(cx.listener(|this, _ev, _w, cx| this.to_latest(cx)));
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom(self.z(theme.spacing.md))
            .flex()
            .justify_center()
            .child(kit::fade_in(pill, "latest", cx))
            .into_any_element()
    }

    /// Everything under the list, on the reading column.
    pub(super) fn foot(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let (spacing, zoom) = (self.theme.spacing, self.zoom);
        let z = |v: f32| px(v * zoom);
        let tasks = self.tasks_card(cx);
        let settled = self.settled_line(cx);
        let ask = match self.approvals.prompt() {
            Some(_) => self.approval_card(cx),
            None => self.composer_box(window, cx),
        };
        div()
            .flex_none()
            .w_full()
            .flex()
            .justify_center()
            .px(z(spacing.inset()))
            .pb(z(spacing.md))
            .child(
                div()
                    .w_full()
                    .max_w(z(READING))
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(z(spacing.sm))
                    .children(tasks)
                    .children(settled)
                    .child(ask),
            )
            .into_any_element()
    }

    /// The agent's task list, while any task is open: one line (how many are done, the one in
    /// progress) that opens to all of them.
    fn tasks_card(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let tasks = self.model.thread(&self.thread)?.tasks();
        if tasks.is_empty() || tasks.iter().all(|t| t.status == "completed") {
            return None;
        }
        let theme = &self.theme;
        let s = theme.surfaces;
        let done = tasks.iter().filter(|t| t.status == "completed").count();
        let current = tasks
            .iter()
            .find(|t| t.status == "in_progress")
            .or_else(|| tasks.iter().find(|t| t.status == "pending"));
        let open = self.tasks_open;
        let head = crate::a11y::tab_stop(
            kit::tabular(div())
                .id("tasks-head")
                .debug_selector(|| "tasks-head".to_owned())
                .role(Role::Button)
                .aria_label(SharedString::from(format!("Tasks: {done} of {} done", tasks.len())))
                .aria_expanded(open)
                .flex()
                .items_center()
                .gap(self.z(theme.spacing.sm))
                .h(self.z(theme.density.row))
                .px(self.z(theme.spacing.sm))
                .cursor_pointer()
                .child(self.icon(IconName::ListTodo, s.text_secondary))
                .child(
                    div()
                        .text_color(hsla(s.text_secondary))
                        .child(SharedString::from(format!("{done} of {}", tasks.len()))),
                )
                .children(current.map(|t| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(hsla(s.text))
                        .child(SharedString::from(t.subject.clone()))
                }))
                .child(self.icon(
                    if open { IconName::ChevronDown } else { IconName::ChevronUp },
                    s.text_muted,
                )),
            s.accent,
        )
        .on_click(cx.listener(|this, _ev, _w, cx| {
            this.tasks_open = !this.tasks_open;
            cx.notify();
        }));
        Some(
            div()
                .id("tasks")
                .debug_selector(|| "tasks".to_owned())
                .role(Role::Group)
                .aria_label("Tasks")
                .flex()
                .flex_col()
                .rounded(self.z(theme.radii.md))
                .border_1()
                .border_color(hsla(s.border_subtle))
                .bg(hsla(s.panel))
                .text_size(self.z(theme.typography.small()))
                .child(head)
                .when(open, |el| {
                    el.child(
                        div()
                            .px(self.z(theme.spacing.sm))
                            .pb(self.z(theme.spacing.sm))
                            .child(self.task_lines(tasks, false)),
                    )
                })
                .into_any_element(),
        )
    }

    /// How the last permission prompt ended; when the terminal took it back, the way there.
    fn settled_line(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let (tool, outcome) = self.approvals.last()?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let (icon, tone) = match outcome {
            Outcome::Answered(Verdict::Deny { .. }) | Outcome::Elsewhere(Verdict::Deny { .. }) => {
                (IconName::CircleX, s.text_muted)
            }
            Outcome::Answered(_) | Outcome::Elsewhere(_) => (IconName::CircleCheck, s.success),
            Outcome::Released => (IconName::SquareTerminal, s.warn),
            Outcome::Withdrawn => (IconName::CirclePause, s.text_muted),
        };
        let text = SharedString::from(outcome.text(tool));
        let go = outcome.in_terminal().then(|| {
            kit::button(theme, "show-terminal", "Show the terminal", ButtonKind::Link).on_click(
                cx.listener(|_this, _ev, _w, cx| {
                    cx.emit(crate::conversation::FaceEvent::ShowTerminal);
                }),
            )
        });
        Some(
            div()
                .id("permission-settled")
                .debug_selector(|| "permission-settled".to_owned())
                .role(Role::Status)
                .aria_label(text.clone())
                .flex()
                .items_center()
                .gap(self.z(theme.spacing.sm))
                .px(self.z(theme.spacing.sm))
                .text_size(self.z(theme.typography.small()))
                .text_color(hsla(s.text_secondary))
                .child(self.icon(icon, tone))
                .child(div().flex_1().min_w_0().child(text))
                .children(go)
                .into_any_element(),
        )
    }

    /// The held permission prompt, in the composer's place: what the call would do, and the
    /// three answers, "always" saying what it grants.
    fn approval_card(&self, cx: &Context<Self>) -> AnyElement {
        let Some(prompt) = self.approvals.prompt().cloned() else {
            return div().into_any_element();
        };
        let theme = self.theme.clone();
        let s = theme.surfaces;
        let answering = self.approvals.answering().cloned();
        let call = slopty_proto::conversation::ToolCall {
            name: prompt.tool.clone(),
            detail: prompt.detail.clone(),
            result: None,
        };
        let title = crate::conversation::tools::title(&call, &[]);
        let subject = title.subject.clone().unwrap_or_else(|| title.verb.clone());
        let preview = self.tool_body(
            &slopty_proto::conversation::Entry {
                id: format!("ask-{}", prompt.ask),
                at_ms: prompt.asked_ms,
                body: slopty_proto::conversation::Body::Tool(Box::new(call.clone())),
            },
            &call,
            rows::Level::Full,
            cx,
        );
        let grants = approval::grants(&prompt.suggestions);
        let mode = prompt
            .mode
            .as_deref()
            .filter(|m| *m != "default")
            .map(|m| approval::mode_label(m).to_owned());
        let busy = answering.is_some();
        let allow = kit::button(&theme, "allow-once", "Allow once", ButtonKind::Primary)
            .when(busy, |el| el.opacity(slopty_theme::alpha::PRESSED))
            .on_click(cx.listener(|this, _ev, _w, cx| this.answer(Verdict::Allow, cx)));
        let always = (!prompt.suggestions.is_empty()).then(|| {
            kit::button(&theme, "allow-always", "Always allow", ButtonKind::Secondary)
                .when(busy, |el| el.opacity(slopty_theme::alpha::PRESSED))
                .on_click(cx.listener(|this, _ev, _w, cx| this.answer(Verdict::AllowAlways, cx)))
        });
        let deny = if self.deny_open {
            kit::button(&theme, "deny-send", "Deny", ButtonKind::Secondary)
                .on_click(cx.listener(|this, _ev, _w, cx| this.deny(cx)))
        } else {
            kit::button(&theme, "deny", "Deny", ButtonKind::Ghost).on_click(cx.listener(
                |this, _ev, window, cx| {
                    this.deny_open = true;
                    this.deny.update(cx, |d, cx| d.focus(window, cx));
                    cx.notify();
                },
            ))
        };
        let reason = self
            .deny_open
            .then(|| div().w_full().child(Input::new(&self.deny).aria_label("Why, for Claude")));
        let grants_line = (!grants.is_empty()).then(|| {
            div()
                .debug_selector(|| "always-grants".to_owned())
                .flex()
                .flex_col()
                .gap(self.z(theme.spacing.xxs))
                .text_size(self.z(theme.typography.meta()))
                .text_color(hsla(s.text_muted))
                .child("Always allow adds")
                .children(grants.iter().map(|g| {
                    div().text_color(hsla(s.text_secondary)).child(SharedString::from(g.clone()))
                }))
        });
        let wait = Self::wait_left(&prompt);
        div()
            .id("approval")
            .debug_selector(|| "approval".to_owned())
            .role(Role::AlertDialog)
            .aria_label(SharedString::from(format!("Claude wants to use {}", prompt.tool)))
            .flex()
            .flex_col()
            .gap(self.z(theme.spacing.sm))
            .p(self.z(theme.spacing.md))
            .rounded(self.z(theme.radii.md))
            .border_1()
            .border_color(hsla(s.warn_fill))
            .bg(hsla(s.panel))
            .text_size(self.z(theme.typography.small()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(self.z(theme.spacing.sm))
                    .child(crate::icons::status_icon(
                        &theme,
                        Status::NeedsYou,
                        self.z(theme.typography.icon()),
                        hsla(s.warn),
                    ))
                    .child(
                        div()
                            .text_color(hsla(s.text))
                            .font_weight(gpui::FontWeight(slopty_theme::Typography::STRONG_WEIGHT))
                            .child(SharedString::from(format!("Allow {}?", prompt.tool))),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_color(hsla(s.text_secondary))
                            .child(SharedString::from(subject)),
                    )
                    .child(div().flex_1())
                    .children(mode.map(|m| {
                        div()
                            .text_size(self.z(theme.typography.meta()))
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(m))
                    })),
            )
            .children(preview.map(|p| div().max_h(self.z(320.0)).overflow_hidden().child(p)))
            .children(grants_line)
            .children(reason)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(self.z(theme.spacing.sm))
                    .child(allow)
                    .children(always)
                    .child(deny)
                    .child(div().flex_1())
                    .children(wait.map(|w| {
                        kit::tabular(div())
                            .text_size(self.z(theme.typography.meta()))
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(w))
                    })),
            )
            .into_any_element()
    }

    /// How long until the worker hands the prompt back to the terminal, by the gap the worker
    /// gave when it asked.
    fn wait_left(prompt: &slopty_proto::conversation::PermissionPrompt) -> Option<String> {
        let window = prompt.until_ms.checked_sub(prompt.asked_ms)?;
        let minutes = window / 60_000;
        (minutes > 0).then(|| format!("The terminal asks in {minutes} min"))
    }

    /// The field that types into the agent's terminal, with the density and the way to send
    /// (or, while the agent works and nothing is typed, to stop it).
    fn composer_box(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let theme = self.theme.clone();
        let s = theme.surfaces;
        let focused = self.composer.focus_handle(cx).is_focused(window);
        let empty = self.draft(cx).trim().is_empty();
        let stop = self.working() && empty;
        let (icon, label) =
            if stop { (IconName::Square, "Stop") } else { (IconName::ArrowUp, "Send") };
        let send =
            div()
                .id("composer-send")
                .debug_selector(move || {
                    if stop { "composer-stop".to_owned() } else { "composer-send".to_owned() }
                })
                .role(Role::Button)
                .aria_label(label)
                .flex_none()
                .size(self.z(kit::icon_button_side(&theme)))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .cursor_pointer()
                .map(|el| {
                    if stop || !empty { el.bg(hsla(s.accent_fill)) } else { el.bg(hsla(s.raised)) }
                })
                .child(
                    crate::icons::icon(
                        &theme,
                        icon,
                        crate::icons::IconSize::Inline,
                        hsla(if stop || !empty { s.accent_ink } else { s.text_muted }),
                    )
                    .size(self.z(theme.typography.icon())),
                );
        let send = crate::a11y::tab_stop(send, s.accent).on_click(cx.listener(
            move |this, _ev, window, cx| {
                if stop {
                    this.interrupt(cx);
                } else {
                    this.submit(window, cx);
                }
            },
        ));
        let density = div()
            .id("density")
            .debug_selector(|| "density".to_owned())
            .role(Role::Button)
            .aria_label(SharedString::from(format!("Density: {}", self.density.label())))
            .px(self.z(theme.spacing.xs))
            .rounded(self.z(theme.radii.xs))
            .text_size(self.z(theme.typography.meta()))
            .text_color(hsla(s.text_muted))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla(s.raised)).text_color(hsla(s.text_secondary)))
            .child(self.density.label());
        let density = crate::a11y::tab_stop(density, s.accent)
            .on_click(cx.listener(|this, _ev, _w, cx| this.cycle_density(cx)));
        div()
            .debug_selector(|| "composer".to_owned())
            .flex()
            .flex_col()
            .rounded(self.z(theme.radii.md))
            .border_1()
            .border_color(hsla(if focused { s.accent } else { s.border }))
            .bg(hsla(s.panel))
            .child(
                div()
                    .px(self.z(theme.spacing.xs))
                    .pt(self.z(theme.spacing.xs))
                    .text_size(self.z(theme.typography.ui_size))
                    .child(
                        Textarea::new(&self.composer)
                            .appearance(false)
                            .bordered(false)
                            .aria_label("Message"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(self.z(theme.spacing.xs))
                    .px(self.z(theme.spacing.xs))
                    .pb(self.z(theme.spacing.xs))
                    .child(density)
                    .child(div().flex_1())
                    .child(send),
            )
            .into_any_element()
    }
}

impl ConversationView {
    /// What the tile's header says about the session, at the chrome's scale `k`: the lines
    /// the conversation changed, how full the context window is, and the model.
    #[must_use]
    pub fn header_chips(&self, k: f32) -> Vec<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let size = px(theme.typography.small() * k);
        let chip = |id: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .flex_none()
                .flex()
                .items_center()
                .gap(px(theme.spacing.xxs * k))
                .text_size(size)
                .whitespace_nowrap()
        };
        let mut out = Vec::new();
        let (added, removed) = self.model.changed_lines();
        if added > 0 || removed > 0 {
            out.push(
                kit::tabular(chip("chip-changes"))
                    .aria_label(SharedString::from(format!(
                        "{added} lines added, {removed} removed"
                    )))
                    .child(
                        div()
                            .text_color(hsla(s.success))
                            .child(SharedString::from(format!("+{added}"))),
                    )
                    .child(
                        div()
                            .text_color(hsla(s.error))
                            .child(SharedString::from(format!("\u{2212}{removed}"))),
                    )
                    .into_any_element(),
            );
        }
        let meters = self.model.meters();
        if let Some(used) = meters.and_then(|m| m.context_used_pct) {
            let words = SharedString::from(format!("Context {used:.0}% used"));
            let hint = words.clone();
            let hint_theme = std::rc::Rc::new(theme.clone());
            out.push(
                chip("chip-context")
                    .aria_label(words)
                    .tooltip(move |_window, cx| {
                        let (hint, theme) = (hint.clone(), std::rc::Rc::clone(&hint_theme));
                        cx.new(|_| kit::Hint::new(hint, "", theme)).into()
                    })
                    .text_color(hsla(s.text_muted))
                    .child(context_ring(theme, used, theme.typography.small() * k))
                    .child(SharedString::from(format!("{used:.0}%")))
                    .into_any_element(),
            );
        }
        if let Some(model) = meters.and_then(|m| m.model.clone()) {
            out.push(
                chip("chip-model")
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(model))
                    .into_any_element(),
            );
        }
        out
    }
}

/// The share of the context window in use as a ring, `side` points round: the track in the
/// quiet hairline, the used arc in the tone the share calls for (warn past 80 %, error past
/// 95 %).
#[must_use]
pub fn context_ring(theme: &Theme, used_pct: f64, side: f32) -> AnyElement {
    let s = theme.surfaces;
    let tone: Rgb = match used_pct {
        p if p >= 95.0 => s.error,
        p if p >= 80.0 => s.warn,
        _ => s.text_secondary,
    };
    let (track, arc) = (hsla(s.border), hsla(tone));
    #[expect(clippy::cast_possible_truncation, reason = "a share on screen")]
    let share = (used_pct / 100.0).clamp(0.0, 1.0) as f32;
    canvas(
        |_bounds, _window, _cx| {},
        move |bounds: Bounds<Pixels>, (), window, _cx| {
            let width = (bounds.size.width.min(bounds.size.height) * 0.16).max(px(1.5));
            let r = (bounds.size.width.min(bounds.size.height) - width) / 2.0;
            let c = bounds.center();
            let at = |t: f32| {
                let a = t.mul_add(std::f32::consts::TAU, -std::f32::consts::FRAC_PI_2);
                point(c.x + r * a.cos(), c.y + r * a.sin())
            };
            let stroke = |from: f32, to: f32| {
                let mut path = PathBuilder::stroke(width);
                let steps = 48_u16;
                path.move_to(at(from));
                for step in 1..=steps {
                    let t = f32::from(step) / f32::from(steps);
                    path.line_to(at((to - from).mul_add(t, from)));
                }
                path.build().ok()
            };
            if let Some(path) = stroke(0.0, 1.0) {
                window.paint_path(path, track);
            }
            if share > 0.0
                && let Some(path) = stroke(0.0, share)
            {
                window.paint_path(path, arc);
            }
        },
    )
    .size(px(side))
    .flex_none()
    .into_any_element()
}
