//! The composer: a card at the foot of the column.
//!
//! Its top row says where the agent works (the checkout, its branch, what the thread changed
//! there, which opens the review); then the menu, the chips, the
//! field; then a row with the way to attach, the model with the agent's mark, the mode, and at
//! the right the one solid: send, or stop while a turn runs and nothing is typed. While the
//! agent's own TUI holds the session, a strip saying so stands in its place.

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Div, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div,
};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::{Sizable as _, Size};
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{Cap, Delivery};

use super::{ThreadView, ThreadViewEvent, agent_name};
use crate::colors::hsla;
use crate::icons::{IconName, IconSize};
use crate::kit::{self, ButtonKind};

/// What the composer says before anything is typed: what it takes, and its two menus.
pub(super) const PLACEHOLDER: &str = "Ask, build, / for commands, @ for references\u{2026}";

/// Where the agent works, as the composer's top row reads it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(super) struct Whereabouts {
    /// The checkout's folder name.
    pub checkout: Option<String>,
    /// Its branch, when the worker says.
    pub branch: Option<String>,
    /// Lines added and removed over the thread.
    pub added: u32,
    /// And removed.
    pub removed: u32,
}

/// The fact that names how far a thread's sandbox reaches (Codex's), as the worker sets it.
const SANDBOX: &str = "sandbox";

/// The context and each of the plan's rate windows, one line each: "Context 34% of 200k",
/// "Five hour 41%".
pub(super) fn meter_words(meters: &slopty_proto::thread::Meters) -> Vec<String> {
    let context = super::context_used(meters).map(|u| match meters.context_window {
        Some(window) => format!("Context {} of {}", share(u), super::tokens(window)),
        None => format!("Context {}", share(u)),
    });
    let limits = meters.limits.iter().map(|l| {
        let used = f64::from(l.used_bp) / 100.0;
        format!("{} {used:.0}%", sentence(&l.name))
    });
    context.into_iter().chain(limits).collect()
}

/// A share of the context in whole percent; under one, "<1%", since a few tokens in use
/// are not none.
pub(super) fn share(percent: f64) -> String {
    if percent < 0.5 { "<1%".to_owned() } else { format!("{percent:.0}%") }
}

/// The model the thread runs as the chip says it: one name, as people say it, from the
/// agent's name for it or else its id ("Canned", never "Canned/Canned").
pub(super) fn model_said(meters: &slopty_proto::thread::Meters) -> Option<String> {
    [meters.model.as_deref(), meters.model_id.as_deref()]
        .into_iter()
        .flatten()
        .find(|m| !m.trim().is_empty())
        .map(|m| crate::conversation::figures::spoken_model(m).0)
}

/// Where the person changes a thread's mode when Slopty cannot: in the agent's own TUI, for an
/// agent that has one. Slopty never cycles a mode key for them.
pub(super) fn mode_hint(meta: &slopty_proto::thread::ThreadMeta) -> Option<String> {
    (!meta.can(Cap::SET_MODE) && meta.can(Cap::LIVE_TUI))
        .then(|| format!("Change the mode in {}'s own terminal", agent_name(&meta.agent)))
}

/// The fact that names a thread's branch, as the workers set it.
const BRANCH: &str = "branch";

/// The last part of `cwd`, the checkout's name; none for an empty one.
pub(super) fn checkout(cwd: &str) -> Option<String> {
    let name = cwd.trim_end_matches('/').rsplit('/').next().unwrap_or_default();
    (!name.is_empty()).then(|| name.to_owned())
}

/// An open name (a mode, an item's kind) in words, sentence case: `acceptEdits` and
/// `AcceptEdits` read "Accept edits", `skill-loaded` "Skill loaded".
pub(super) fn sentence(mode: &str) -> String {
    let mut words = String::with_capacity(mode.len().saturating_add(4));
    let mut prev: Option<char> = None;
    for c in mode.trim().chars() {
        match c {
            '-' | '_' | ' ' => {
                if !words.ends_with(' ') && !words.is_empty() {
                    words.push(' ');
                }
            }
            c if c.is_uppercase() => {
                if prev.is_some_and(|p| p.is_lowercase() || p.is_ascii_digit())
                    && !words.ends_with(' ')
                {
                    words.push(' ');
                }
                words.extend(c.to_lowercase());
            }
            c => words.push(c),
        }
        prev = Some(c);
    }
    let mut chars = words.trim_end().chars();
    chars.next().map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
}

impl ThreadView {
    /// Who holds the session, when the thread can move between Slopty and the agent's own
    /// TUI ([`Cap::HANDOFF`]): `Some(true)` while the TUI does, `Some(false)` while Slopty
    /// drives it.
    pub(super) fn tui_holds(&self, cx: &App) -> Option<bool> {
        let meta = &self.state(cx)?.meta;
        meta.can(Cap::HANDOFF).then_some(meta.terminal.is_some())
    }

    /// Whether this client's `intent` is on its way and not yet answered.
    fn moving(&self, cx: &App, intent: &Intent) -> bool {
        self.hub
            .read(cx)
            .threads()
            .unshown(self.thread)
            .any(|s| s.outcome.is_none() && s.intent == *intent)
    }

    /// Where the agent works: the checkout, its branch, and what the thread changed.
    pub(super) fn where_it_works(&self, cx: &App) -> Whereabouts {
        let threads = self.hub.read(cx).threads();
        let row = threads.rows().rows.get(&self.thread);
        let state = self.state(cx);
        let branch = state
            .and_then(|st| st.meta.facts.get(BRANCH))
            .or_else(|| row.and_then(|r| r.facts.get(BRANCH)))
            .filter(|b| !b.is_empty())
            .cloned();
        let changed = row.map(|r| r.changed).unwrap_or_default();
        Whereabouts {
            checkout: state.and_then(|st| checkout(&st.meta.cwd)),
            branch,
            added: changed.added,
            removed: changed.removed,
        }
    }

    /// In the composer's place while the agent's own TUI holds the session: where it is, and
    /// the way to take it back once it rests.
    fn held_strip(&self, capped: bool, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let name = self.state(cx).map_or("the agent", |st| agent_name(&st.meta.agent));
        let taking = self.moving(cx, &Intent::TakeBack);
        div()
            .id("thread-held")
            .debug_selector(|| "thread-held".to_owned())
            .role(Role::Status)
            .w_full()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .px(self.z(theme.spacing.md))
            .py(self.z(theme.spacing.sm))
            .map(|el| self.shell(el, capped))
            .text_size(self.z(theme.typography.small()))
            .text_color(hsla(s.text_secondary))
            .child(self.icon(IconName::SquareTerminal, s.text_muted))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .child(SharedString::from(format!("In {name}'s own terminal now"))),
            )
            .child(if taking {
                div()
                    .flex_none()
                    .text_color(hsla(s.text_muted))
                    .child("Taking it back once it rests")
                    .into_any_element()
            } else {
                self.button("thread-take-back", "Take back", ButtonKind::Secondary)
                    .on_click(cx.listener(|this, _ev, _w, cx| {
                        let _id = this.intent(Intent::TakeBack, cx);
                    }))
                    .into_any_element()
            })
            .into_any_element()
    }

    /// The composer's way to hand the session to the agent's own TUI, while Slopty drives it.
    fn handoff_button(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if self.tui_holds(cx) != Some(false) {
            return None;
        }
        if self.moving(cx, &Intent::Handoff) {
            return Some(div().child("Handing over once it rests").into_any_element());
        }
        Some(
            self.button("thread-handoff", "Continue in the terminal", ButtonKind::Ghost)
                .on_click(cx.listener(|this, _ev, _w, cx| {
                    let _id = this.intent(Intent::Handoff, cx);
                }))
                .into_any_element(),
        )
    }

    /// A small fact in the composer's top row: its mark, then its words.
    fn fact(&self, icon: IconName, words: String) -> Div {
        let theme = &self.theme;
        let s = theme.surfaces;
        div()
            .flex_none()
            .min_w_0()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .child(
                crate::icons::icon(theme, icon, IconSize::Inline, hsla(s.text_muted))
                    .size(self.z(theme.typography.small())),
            )
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(words)),
            )
    }

    /// Where the thread works, in the composer's toolbar after its chips: the checkout, its
    /// branch, and what the thread changed (which opens the review). It takes the room between
    /// the chips and the meter and gives it up first, so the controls never shift.
    ///
    /// A row of its own over the field made the shell two strata; folded into the toolbar the
    /// composer is one shell with no internal line.
    ///
    /// No mark of the agent at work: the thread's own working line says it in words, with
    /// how long, and an unlabelled ring in the card's corner said nothing more.
    fn where_facts(&self, cx: &Context<Self>) -> Div {
        let theme = &self.theme;
        let s = theme.surfaces;
        let here = self.where_it_works(cx);
        let thread = self.thread;
        let changes = kit::changes(theme, here.added, here.removed).map(|counts| {
            div()
                .id("thread-changes")
                .debug_selector(|| "thread-changes".to_owned())
                .role(Role::Button)
                .aria_label("Review the changes")
                .flex_none()
                .px(self.z(theme.spacing.xs))
                .rounded(self.z(theme.radii.xs))
                .cursor_pointer()
                .hover(move |el| el.bg(hsla(s.hover)))
                .active(move |el| el.bg(hsla(s.pressed)))
                .child(counts)
                .on_click(cx.listener(move |_this, _ev, _w, cx| {
                    cx.emit(ThreadViewEvent::Review { thread });
                }))
        });
        div()
            .debug_selector(|| "thread-where".to_owned())
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.sm))
            .px(self.z(theme.spacing.xs))
            .text_size(self.z(theme.typography.small()))
            .text_color(hsla(s.text_muted))
            .children(here.checkout.map(|c| self.fact(IconName::Folder, c).flex_shrink_1()))
            .children(here.branch.map(|b| self.fact(IconName::GitBranch, b).flex_shrink_1()))
            .children(changes)
    }

    /// A quiet chip in the composer's foot: the model, the mode.
    fn chip(&self, id: &'static str, label: String) -> gpui::Stateful<Div> {
        let theme = &self.theme;
        let s = theme.surfaces;
        div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .aria_label(SharedString::from(label))
            .flex_none()
            .h(self.z(theme.density.control))
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .px(self.z(theme.spacing.sm))
            .rounded(self.z(theme.radii.sm))
            .text_size(self.z(theme.typography.small()))
            .text_color(hsla(s.text_secondary))
    }

    /// The model chip: the agent's mark and the model's name; a menu of the agent's models
    /// when it can switch.
    fn model_chip(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let state = self.state(cx)?;
        let switch = state.meta.can(Cap::SET_MODEL) && !state.meta.models.is_empty();
        let name =
            model_said(&state.meters).unwrap_or_else(|| agent_name(&state.meta.agent).to_owned());
        let mark = self.agent_mark(Some(&state.meta.agent), false, s.text_secondary);
        let chip = self.chip("thread-model", format!("Model, {name}")).child(mark).child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(SharedString::from(name)),
        );
        Some(if switch {
            crate::a11y::tab_stop(
                chip.role(Role::Button)
                    .cursor_pointer()
                    .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
                    .child(
                        crate::icons::icon(
                            theme,
                            IconName::ChevronDown,
                            IconSize::Inline,
                            hsla(s.text_muted),
                        )
                        .size(self.z(theme.typography.small())),
                    )
                    .on_click(cx.listener(|this, _ev, _w, cx| this.toggle_models(cx))),
                s.accent,
            )
            .into_any_element()
        } else {
            chip.role(Role::Label).into_any_element()
        })
    }

    /// The mode chip: the permission mode the agent says it is in; a menu of the modes it
    /// publishes when it can switch ([`Cap::SET_MODE`]).
    ///
    /// Where the agent says how far its sandbox reaches (Codex's), that follows: "On request ·
    /// Workspace write".
    fn mode_chip(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let state = self.state(cx)?;
        let switch = state.meta.can(Cap::SET_MODE) && !state.meta.modes.is_empty();
        let mode = state.meters.mode.as_deref().map(str::trim).filter(|m| !m.is_empty());
        // The agent's own name for the mode it is in, where it published one.
        let named = mode.map(|m| {
            state
                .meta
                .modes
                .iter()
                .find(|known| known.id == m || known.label == m)
                .map_or_else(|| sentence(m), |known| known.label.clone())
        });
        let named = named.or_else(|| switch.then(|| "Mode".to_owned()));
        let sandbox = state.meta.facts.get(SANDBOX).map(|f| f.trim()).filter(|f| !f.is_empty());
        let words: Vec<String> = named.into_iter().chain(sandbox.map(sentence)).collect();
        if words.is_empty() {
            return None;
        }
        let words = words.join(" \u{b7} ");
        let chip =
            self.chip("thread-mode", format!("Mode, {words}")).child(SharedString::from(words));
        if switch {
            return Some(
                crate::a11y::tab_stop(
                    chip.role(Role::Button)
                        .cursor_pointer()
                        .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
                        .child(
                            crate::icons::icon(
                                theme,
                                IconName::ChevronDown,
                                IconSize::Inline,
                                hsla(s.text_muted),
                            )
                            .size(self.z(theme.typography.small())),
                        )
                        .on_click(cx.listener(|this, _ev, _w, cx| this.toggle_modes(cx))),
                    s.accent,
                )
                .into_any_element(),
            );
        }
        let hint = mode_hint(&state.meta).map(|hint| (hint, self.theme.clone()));
        Some(
            chip.role(Role::Label)
                .when_some(hint, |el, (hint, theme)| {
                    kit::hint_timing(el).tooltip(move |_window, cx| {
                        let theme = std::rc::Rc::new(theme.clone());
                        cx.new(|_| kit::Hint::new(hint.clone(), "", theme)).into()
                    })
                })
                .into_any_element(),
        )
    }

    /// The effort chip: how hard the model thinks, as the agent names it.
    fn effort_chip(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let effort = self.state(cx)?.meters.effort.clone().filter(|e| !e.trim().is_empty())?;
        let words = sentence(&effort);
        Some(
            self.chip("thread-effort", format!("Effort, {words}"))
                .role(Role::Label)
                .child(
                    crate::icons::icon(
                        &self.theme,
                        IconName::Brain,
                        IconSize::Inline,
                        hsla(self.theme.surfaces.text_muted),
                    )
                    .size(self.z(self.theme.typography.small())),
                )
                .child(SharedString::from(words))
                .into_any_element(),
        )
    }

    /// The background chip, while the agent lists background work: how much still runs, and
    /// the panel of it on a press.
    fn tasks_chip(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let tasks = &self.state(cx)?.tasks;
        if tasks.is_empty() {
            return None;
        }
        let running = tasks.iter().filter(|t| t.is_running()).count();
        let words = if running > 0 {
            format!("{running} running")
        } else {
            format!("{} in the background", tasks.len())
        };
        let mark = if running > 0 {
            self.spinner(true)
        } else {
            self.icon(IconName::Activity, s.text_muted)
        };
        let open = self.tasks_open;
        Some(
            crate::a11y::tab_stop(
                self.chip("thread-tasks", words.clone())
                    .role(Role::Button)
                    .aria_expanded(open)
                    .cursor_pointer()
                    .when(open, |el| el.bg(hsla(s.hover)))
                    .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
                    .child(mark)
                    .child(SharedString::from(words))
                    .on_click(cx.listener(|this, _ev, _w, cx| {
                        this.tasks_open = !this.tasks_open;
                        cx.notify();
                    })),
                s.accent,
            )
            .into_any_element(),
        )
    }

    /// How full the context is, a ring and its share, with the plan's rate windows in its
    /// hint: quiet until asked.
    fn meter(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let meters = &self.state(cx)?.meters;
        let used = super::context_used(meters);
        if used.is_none() && meters.limits.is_empty() {
            return None;
        }
        let hint = meter_words(meters).join("\n");
        let hint_theme = theme.clone();
        Some(
            self.chip("thread-meter", hint.replace('\n', ", "))
                .role(Role::Label)
                .text_size(self.z(theme.typography.small()))
                .text_color(hsla(s.text_muted))
                .children(used.map(|u| {
                    crate::conversation::view::context_ring(
                        theme,
                        u,
                        theme.typography.small() * self.zoom,
                    )
                }))
                .children(used.map(|u| SharedString::from(share(u))))
                .map(kit::hint_timing)
                .tooltip(move |_window, cx| {
                    let theme = std::rc::Rc::new(hint_theme.clone());
                    cx.new(|_| kit::Hint::new(hint.clone(), "", theme)).into()
                })
                .into_any_element(),
        )
    }

    /// The one solid: send what is typed, or stop the turn while one runs and nothing is.
    fn send_button(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let stopping = self.hub.read(cx).threads().stopping(self.thread);
        let empty = self.composer.read(cx).value().trim().is_empty();
        let stop = self.working(cx) && empty && !stopping && !self.composing.editing();
        let (id, icon, label) = if stop {
            ("thread-stop", IconName::Square, "Stop")
        } else {
            ("thread-send", IconName::ArrowUp, "Send")
        };
        let el = div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .role(Role::Button)
            .aria_label(label)
            .flex_none()
            .size(self.z(theme.density.control))
            .flex()
            .items_center()
            .justify_center()
            .rounded(self.z(theme.radii.sm))
            .cursor_pointer()
            .child(
                crate::icons::icon(theme, icon, IconSize::Inline, hsla(s.solid_ink))
                    .size(self.z(theme.typography.icon())),
            );
        let el =
            kit::solid_pressable(el, theme).on_click(cx.listener(move |this, _ev, window, cx| {
                if stop {
                    this.interrupt(cx);
                } else {
                    let delivery = this.send_now(cx);
                    this.submit(delivery, window, cx);
                }
            }));
        // A long press (a right click on the Mac) sends after the turn under way, as ⌘↵ does:
        // the one way to queue with no keyboard.
        let queues = !stop && self.state(cx).is_some_and(|st| st.meta.can(Cap::QUEUE));
        let el = el.when(queues, |el| {
            el.on_aux_click(cx.listener(|this, _ev, window, cx| {
                this.submit(Delivery::Queue, window, cx);
            }))
        });
        crate::a11y::tab_stop(el, s.accent).into_any_element()
    }

    /// The composer's shell, which what stands in its place shares: the floating surface on
    /// the resting elevation inside one hairline, all its corners round, or with the tray as
    /// its head only the foot's (the tray then takes the rim's top edge in dark).
    pub(super) fn shell<E: gpui::Styled>(&self, el: E, capped: bool) -> E {
        let theme = &self.theme;
        let s = theme.surfaces;
        let r = self.z(theme.radii.lg);
        let el = if capped { el.rounded_bl(r).rounded_br(r) } else { el.rounded(r) };
        let el = el.border(kit::hair(theme)).border_color(hsla(s.border)).bg(hsla(s.elevated));
        kit::rests(el, theme, !capped, true)
    }

    /// The composer card. While the agent's own TUI holds the session, where it is instead.
    /// `capped`: the tray stands on it as its head, so its top corners are square and its top
    /// edge is the quieter hairline between the two.
    pub(super) fn composer_box(&self, capped: bool, cx: &Context<Self>) -> AnyElement {
        if self.tui_holds(cx) == Some(true) {
            return self.held_strip(capped, cx);
        }
        if let Some(strip) = self.exited_strip(capped, cx) {
            return strip;
        }
        let theme = &self.theme;
        let s = theme.surfaces;
        let editing = self.composing.editing();
        let view = cx.weak_entity();
        div()
            .id("thread-composer")
            .debug_selector(|| "thread-composer".to_owned())
            .w_full()
            .flex()
            .flex_col()
            .map(|el| self.shell(el, capped))
            // Under the tray, the one line in the shell: the quieter hairline, where the
            // tray's head meets the field.
            .when(capped, |el| el.border_t_0().child(kit::rule(theme, s.border_subtle)))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(self.z(theme.spacing.xs))
                    .px(self.z(theme.spacing.md))
                    .pt(self.z(theme.spacing.sm))
                    .text_size(self.z(theme.typography.title()))
                    .children(self.exited_line(cx))
                    .children(self.menu_section(cx))
                    .children(self.notice_strip())
                    .children(self.editing_strip(cx))
                    .children(self.attachment_chips(cx))
                    .child(
                        // The kit sizes a field in fixed points (its pads, its line): the
                        // least of them, and the words and lines at the zoom, so a miniature
                        // in the overview draws the field as small as the rest.
                        div().py(self.z(theme.spacing.xs)).child(
                            Textarea::new(&self.composer)
                                .with_size(Size::XSmall)
                                .appearance(false)
                                .bordered(false)
                                .text_size(self.z(theme.typography.title()))
                                .line_height(gpui::relative(theme.typography.prose_line_height))
                                .aria_label("Message")
                                .on_paste(move |item, _window, cx| {
                                    view.update(cx, |v, cx| v.paste_attachment(item, cx))
                                        .unwrap_or(false)
                                }),
                        ),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(self.z(theme.spacing.xxs))
                    .px(self.z(theme.spacing.sm))
                    .pb(self.z(theme.spacing.sm))
                    .pt(self.z(theme.spacing.xs))
                    .when(!editing, |el| {
                        el.child(
                            self.icon_button("thread-attach", IconName::Plus, "Attach files")
                                .on_click(cx.listener(|this, _ev, _w, cx| this.pick_files(cx))),
                        )
                    })
                    .children(self.model_chip(cx))
                    .children(self.effort_chip(cx))
                    .children(self.mode_chip(cx))
                    .children(self.tasks_chip(cx))
                    .child(self.where_facts(cx))
                    .children(self.meter(cx))
                    .children(self.handoff_button(cx))
                    .child(self.send_button(cx)),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{checkout, meter_words, sentence};

    /// The meter's hint names the context and each rate window as the agent names it.
    #[test]
    fn the_meter_names_the_context_and_each_window() {
        let meters = slopty_proto::thread::Meters {
            context_tokens: Some(68_000),
            context_window: Some(200_000),
            limits: vec![slopty_proto::thread::Limit {
                name: "five-hour".to_owned(),
                used_bp: 4_100,
                resets_ms: None,
            }],
            ..slopty_proto::thread::Meters::default()
        };
        assert_eq!(meter_words(&meters), ["Context 34% of 200k", "Five hour 41%"]);
    }

    /// A mode reads as words in sentence case, however the agent spells it.
    #[test]
    fn a_mode_reads_as_words() {
        assert_eq!(sentence("AcceptEdits"), "Accept edits");
        assert_eq!(sentence("acceptEdits"), "Accept edits");
        assert_eq!(sentence("bypass-permissions"), "Bypass permissions");
        assert_eq!(sentence("plan"), "Plan");
        assert_eq!(sentence("read_only"), "Read only");
    }

    /// The checkout reads as its folder's name, a trailing slash or not.
    #[test]
    fn a_checkout_reads_as_its_folders_name() {
        assert_eq!(checkout("/Users/me/src/slopty/").as_deref(), Some("slopty"));
        assert_eq!(checkout("/Users/me/src/slopty").as_deref(), Some("slopty"));
        assert_eq!(checkout(""), None);
    }
}
