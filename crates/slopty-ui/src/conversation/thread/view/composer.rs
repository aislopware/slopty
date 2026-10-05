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
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::{Sizable as _, Size};
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{BackgroundTask, Cap, Delivery};
use slopty_theme::{Rgb, Theme};

use super::{ThreadView, ThreadViewEvent, agent_name};
use crate::colors::hsla;
use crate::icons::{IconName, IconSize};
use crate::kit::{self, ButtonKind};

/// What the composer says before anything is typed, while the thread has not said its agent.
pub(super) const PLACEHOLDER: &str = "Ask\u{2026}";

/// What the composer says before anything is typed: an invitation naming the agent, "Ask
/// Claude Code…". What it takes ("/" and "@") is taught by the "+" menu, not by the field.
pub(super) fn placeholder(agent: &slopty_proto::thread::AgentId) -> String {
    format!("Ask {}\u{2026}", super::agent_label(agent))
}

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

/// The context and each of the plan's rate windows, one line each, at `now`: "Context 34% of
/// 200k", "Five hour 41%".
pub(super) fn meter_words(
    meters: &slopty_proto::thread::Meters,
    now: slopty_core::WallMs,
) -> Vec<String> {
    let context = super::context_used(meters).map(|u| match meters.context_window {
        Some(window) => format!("Context {} of {}", share(u), super::tokens(window)),
        None => format!("Context {}", share(u)),
    });
    let limits = meters.limits.iter().map(move |l| {
        let used = f64::from(l.used_bp) / 100.0;
        let resets = l
            .resets_ms
            .and_then(|at| crate::conversation::figures::stamp(at, now))
            .map(|at| format!(" \u{b7} resets {at}"))
            .unwrap_or_default();
        format!("{} {used:.0}%{resets}", sentence(&l.name))
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
    pub(super) fn moving(&self, cx: &App, intent: &Intent) -> bool {
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

    /// A small fact in the composer's toolbar, `name` its id's tail: its mark where it has one,
    /// then its words.
    fn fact(&self, name: &str, icon: Option<IconName>, words: String) -> Div {
        let theme = &self.theme;
        let s = theme.surfaces;
        div()
            .flex_none()
            .min_w_0()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .children(icon.map(|icon| {
                crate::icons::icon(theme, icon, IconSize::Inline, hsla(s.text_muted))
                    .size(self.z(theme.typography.small()))
            }))
            .child(kit::fit_label(format!("thread-fact-{name}"), words, theme).fixed())
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
        let place = div()
            .min_w_0()
            .flex_shrink_1()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.sm))
            // The checkout by its name alone: a folder's glyph before it said nothing its name
            // and its place do not. The branch keeps its glyph, the one cue that tells a
            // branch's name from a folder's.
            .children(here.checkout.map(|c| self.fact("checkout", None, c).flex_shrink_1()))
            .children(
                here.branch
                    .map(|b| self.fact("branch", Some(IconName::GitBranch), b).flex_shrink_1()),
            );
        // Where it works opens the commit sheet on that repository.
        let place = if self.repo(cx).is_some() {
            crate::a11y::tab_stop(
                place
                    .id("thread-git")
                    .debug_selector(|| "thread-git".to_owned())
                    .role(Role::Button)
                    .aria_label("Commit")
                    .px(self.z(theme.spacing.xs))
                    .rounded(self.z(theme.radii.xs))
                    .cursor_pointer()
                    .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text_secondary)))
                    .active(move |el| el.bg(hsla(s.pressed)))
                    .on_click(cx.listener(|this, _ev, window, cx| this.open_commit(window, cx))),
                s.accent,
            )
            .into_any_element()
        } else {
            place.into_any_element()
        };
        div()
            .debug_selector(|| "thread-where".to_owned())
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .text_size(self.z(theme.typography.small()))
            .text_color(hsla(s.text_muted))
            .child(place)
            .children(self.pull_chip(cx))
            .children(changes)
            .children(self.screen_chip(cx))
    }

    /// The branch's pull request, once this client has heard of it: its number in the tone of
    /// where it stands, which opens the commit sheet with the pull request at its head.
    fn pull_chip(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let repo = self.repo(cx)?;
        let hub = self.hub.read(cx);
        let pull = hub.git().repo(&repo)?.pull.status()?;
        let words = crate::conversation::thread::git::standing_words(pull);
        let tone = crate::conversation::thread::commit::standing_tone(theme, pull.standing());
        Some(
            crate::a11y::tab_stop(
                div()
                    .id("thread-pull")
                    .debug_selector(|| "thread-pull".to_owned())
                    .role(Role::Button)
                    .aria_label(SharedString::from(format!(
                        "Pull request {}, {words}",
                        pull.number
                    )))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(self.z(theme.spacing.xxs))
                    .px(self.z(theme.spacing.xs))
                    .rounded(self.z(theme.radii.xs))
                    .cursor_pointer()
                    .hover(move |el| el.bg(hsla(s.hover)))
                    .child(
                        crate::icons::icon(
                            theme,
                            IconName::GitPullRequest,
                            IconSize::Inline,
                            hsla(tone),
                        )
                        .size(self.z(theme.typography.small())),
                    )
                    .child(
                        kit::tabular(div()).child(SharedString::from(format!("#{}", pull.number))),
                    )
                    .on_click(cx.listener(|this, _ev, window, cx| this.open_commit(window, cx))),
                s.accent,
            )
            .into_any_element(),
        )
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
        let chip = self
            .chip("thread-model", format!("Model, {name}"))
            .child(kit::fit_label("thread-model-name", name, theme).fixed());
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

    /// The effort chip: how hard the model thinks, as the agent names it, by the label of the
    /// level it published when one matches; a menu of the levels when it can switch
    /// ([`Cap::SET_EFFORT`] and a level to switch to). An agent with the door but no levels
    /// for its model (a pi model that does not reason) shows no chip.
    fn effort_chip(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let state = self.state(cx)?;
        let switch = state.meta.can(Cap::SET_EFFORT) && !state.meta.efforts.is_empty();
        let effort = state.meters.effort.as_deref().map(str::trim).filter(|e| !e.is_empty());
        let named = effort.map(|e| {
            state
                .meta
                .efforts
                .iter()
                .find(|known| known.id == e || known.label == e)
                .map_or_else(|| sentence(e), |known| known.label.clone())
        });
        if named.is_none() && state.meta.can(Cap::SET_EFFORT) && !switch {
            return None;
        }
        let words = named.or_else(|| switch.then(|| "Effort".to_owned()))?;
        let chip =
            self.chip("thread-effort", format!("Effort, {words}")).child(SharedString::from(words));
        if !switch {
            return Some(chip.role(Role::Label).into_any_element());
        }
        Some(
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
                    .on_click(cx.listener(|this, _ev, _w, cx| this.toggle_efforts(cx))),
                s.accent,
            )
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
        let words = super::tray::tasks_words(tasks);
        // A mark only where it says something: running, or failed. Work that finished is said
        // by its words.
        let mark = if tasks.iter().any(BackgroundTask::is_running) {
            Some(self.spinner(true))
        } else if tasks.iter().any(|t| t.state == BackgroundTask::FAILED) {
            Some(self.icon(IconName::CircleAlert, s.error))
        } else {
            None
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
                    .children(mark)
                    .child(kit::Rolling::new(
                        "thread-tasks-chip-figure",
                        words,
                        self.z(theme.typography.small()),
                    ))
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
    /// hint: quiet until asked. A press opens the meter's panel in the tray.
    fn meter(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let meters = &self.state(cx)?.meters;
        let used = super::context_used(meters);
        if used.is_none() && meters.limits.is_empty() {
            return None;
        }
        let hint = meter_words(meters, crate::clock::now(cx)).join("\n");
        let hint_theme = theme.clone();
        Some(
            crate::a11y::tab_stop(
                self.chip("thread-meter", hint.replace('\n', ", "))
                    .role(Role::Button)
                    .aria_expanded(self.meter_open)
                    .cursor_pointer()
                    .when(self.meter_open, |el| el.bg(hsla(s.hover)))
                    .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
                    .on_click(cx.listener(|this, _ev, _w, cx| {
                        this.meter_open = !this.meter_open;
                        cx.notify();
                    }))
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .children(used.map(|u| {
                        context_ring(
                            theme,
                            "thread-meter-ring",
                            u,
                            theme.typography.small() * self.zoom,
                        )
                    }))
                    .children(used.map(|u| {
                        kit::Rolling::new(
                            "thread-meter-figure",
                            share(u),
                            self.z(theme.typography.small()),
                        )
                    }))
                    .map(kit::hint_timing)
                    .tooltip(move |_window, cx| {
                        let theme = std::rc::Rc::new(hint_theme.clone());
                        cx.new(|_| kit::Hint::new(hint.clone(), "", theme)).into()
                    }),
                s.accent,
            )
            .into_any_element(),
        )
    }

    /// "Interrupt and send" beside the queue's send, while a turn runs on an agent that takes
    /// no message mid-turn but can be stopped ([`Cap::INTERRUPT`] and [`Cap::QUEUE`] without
    /// [`Cap::STEER`]): the turn stops, then the message goes ([`Delivery::Interrupt`]).
    fn interrupt_send(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let meta = &self.state(cx)?.meta;
        let offered = meta.can(Cap::INTERRUPT) && meta.can(Cap::QUEUE) && !meta.can(Cap::STEER);
        let typed = !self.composer.read(cx).value().trim().is_empty();
        let ready = offered && typed && self.working(cx) && !self.composing.editing();
        ready.then(|| {
            self.button("thread-interrupt-send", "Interrupt and send", ButtonKind::Ghost)
                .on_click(cx.listener(|this, _ev, window, cx| {
                    this.submit(Delivery::Interrupt, window, cx);
                }))
                .into_any_element()
        })
    }

    /// The one solid: stop the turn while one runs and nothing is typed; otherwise what ↵ will
    /// do with the draft, in its glyph and its name: Update a waiting message being changed,
    /// Steer into the turn under way, Queue after it (an agent that takes no steer), or Send.
    fn send_button(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let stopping = self.hub.read(cx).threads().stopping(self.thread);
        let empty = self.composer.read(cx).value().trim().is_empty();
        let working = self.working(cx);
        let stop = working && empty && !stopping && !self.composing.editing();
        let (id, icon, label) = if stop {
            ("thread-stop", IconName::Square, "Stop")
        } else if self.composing.editing() {
            ("thread-send", IconName::Check, "Update")
        } else if !working {
            ("thread-send", IconName::ArrowUp, "Send")
        } else if self.send_now(cx) == Delivery::Queue {
            ("thread-send", IconName::Clock, "Queue")
        } else {
            ("thread-send", IconName::ArrowRight, "Steer")
        };
        let hint_theme = std::rc::Rc::new(theme.clone());
        let hint: SharedString = if stop && self.goal_goes_on(cx) {
            format!("{label}. {}", super::goal::GOES_ON).into()
        } else {
            label.into()
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
            )
            .map(kit::hint_timing)
            .tooltip(move |_window, cx| {
                cx.new(|_| kit::Hint::new(hint.clone(), "", std::rc::Rc::clone(&hint_theme))).into()
            });
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
        let el = if capped {
            let r = self.z(theme.radii.lg);
            el.rounded_bl(r).rounded_br(r)
        } else {
            el.rounded(self.z(theme.radii.lg))
        };
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
            .key_context(super::COMPOSER_CTX)
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
                    .children(self.limit_strip(cx))
                    .children(self.goal_strip(cx))
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
                                .text_size(self.z(theme.typography.prose()))
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
                    .when(!editing, |el| el.child(self.add_button(cx)))
                    .children(self.model_chip(cx))
                    .children(self.effort_chip(cx))
                    .children(self.mode_chip(cx))
                    .children(self.tasks_chip(cx))
                    .child(self.where_facts(cx))
                    .children(self.meter(cx))
                    .children(self.handoff_button(cx))
                    .children(self.interrupt_send(cx))
                    .child(self.send_button(cx)),
            )
            .into_any_element()
    }
}

impl ThreadView {
    /// The "+" at the foot's start and the menu it opens: attach files, then the two menus
    /// the field takes, "/" for commands and "@" for files and symbols, so the field's own
    /// words can be an invitation rather than a lesson.
    fn add_button(&self, cx: &Context<Self>) -> AnyElement {
        let menu = self.add_open.then(|| self.add_menu(cx));
        div()
            .relative()
            .flex_none()
            .child(self.icon_button(ADD_BUTTON, IconName::Plus, ADD_LABEL).on_click(cx.listener(
                |this, _ev, _w, cx| {
                    this.add_open = !this.add_open;
                    cx.notify();
                },
            )))
            .children(menu)
            .into_any_element()
    }

    /// The "+" menu, hung above the button.
    fn add_menu(&self, cx: &Context<Self>) -> AnyElement {
        let this = cx.entity().downgrade();
        let draft = self.draft(cx);
        let mut menu = kit::Menu::new();
        let to = this.clone();
        menu.push(kit::MenuItem::new("files", ATTACH_FILES, move |_w, cx| {
            let _gone = to.update(cx, |this, cx| {
                this.add_open = false;
                cx.emit(ThreadViewEvent::PickFiles);
                cx.notify();
            });
        }));
        if !draft.starts_with('/') {
            let to = this.clone();
            menu.push(
                kit::MenuItem::new("commands", COMMANDS, move |window, cx| {
                    let _gone = to.update(cx, |this, cx| {
                        this.add_open = false;
                        let (text, caret) = with_command(&this.draft(cx));
                        this.set_draft(&text, caret, window, cx);
                        this.focus(window, cx);
                    });
                })
                .detail("/"),
            );
        }
        let to = this.clone();
        menu.push(
            kit::MenuItem::new("mentions", MENTIONS, move |window, cx| {
                let _gone = to.update(cx, |this, cx| {
                    this.add_open = false;
                    let caret = this.composer.read(cx).cursor();
                    let (text, caret) = with_mention(&this.draft(cx), caret);
                    this.set_draft(&text, caret, window, cx);
                    this.focus(window, cx);
                });
            })
            .detail("@"),
        );
        let panel = kit::MenuPanel::new(
            ADD_MENU,
            ADD_LABEL,
            std::rc::Rc::new(menu),
            &self.theme,
            move |window, cx| {
                let _gone = this.update(cx, |this, cx| {
                    this.add_open = false;
                    this.focus(window, cx);
                    cx.notify();
                });
            },
        );
        gpui::deferred(gpui::anchored().anchor(gpui::Anchor::BottomLeft).child(panel))
            .with_priority(crate::palette::Layer::Submenu.priority())
            .into_any_element()
    }
}

/// The selector of the composer's "+".
pub(crate) const ADD_BUTTON: &str = "thread-attach";

/// The selector of the menu the "+" opens.
pub(crate) const ADD_MENU: &str = "thread-add-menu";

/// What the "+" is called.
const ADD_LABEL: &str = "Add to the message";

/// The "+" menu's row that picks files to send.
pub(crate) const ATTACH_FILES: &str = "Attach files\u{2026}";

/// The "+" menu's row that starts a command, as typing "/" does.
pub(crate) const COMMANDS: &str = "Commands";

/// The "+" menu's row that names a file or a symbol, as typing "@" does.
pub(crate) const MENTIONS: &str = "Files and symbols";

/// `draft` with a command begun at its start, and the caret after the "/": the agent reads a
/// command only from a message's start, and what was written stays as the command's words.
fn with_command(draft: &str) -> (String, usize) {
    let text = if draft.is_empty() { "/".to_owned() } else { format!("/ {draft}") };
    (text, 1)
}

/// `draft` with a mention begun at byte `caret`, set off from a word before it by a space, and
/// the caret after the "@".
fn with_mention(draft: &str, caret: usize) -> (String, usize) {
    let caret =
        (0..=caret.min(draft.len())).rev().find(|&at| draft.is_char_boundary(at)).unwrap_or(0);
    let (before, after) = draft.split_at(caret);
    let lead = if before.is_empty() || before.ends_with(char::is_whitespace) { "" } else { " " };
    let text = format!("{before}{lead}@{after}");
    (text, caret.saturating_add(lead.len()).saturating_add(1))
}

/// The tone the share of the context window in use is drawn in: warn past 80 %, error past
/// 95 %.
#[must_use]
pub(super) fn context_tone(theme: &Theme, used_pct: f64) -> Rgb {
    let s = theme.surfaces;
    match used_pct {
        p if p >= 95.0 => s.error,
        p if p >= 80.0 => s.warn,
        _ => s.text_secondary,
    }
}

/// The share of the context window in use as a ring, `side` points round: the whole track drawn
/// in the muted ink at a tint, the used arc over it in the tone the share calls for (warn past
/// 80 %, error past 95 %). On the hairline the track all but vanished, and a lone arc beside the
/// stop button read as a spinner; a closed ring reads as a gauge.
///
/// It is the kit's ring ([`kit::progress::ring`]): a true arc with round ends on the soft
/// track, gliding to a new share and turning back from where it is drawn; under `id`, one per
/// place it shows.
#[must_use]
pub(super) fn context_ring(
    theme: &Theme,
    id: impl Into<gpui::ElementId>,
    used_pct: f64,
    side: f32,
) -> AnyElement {
    #[expect(clippy::cast_possible_truncation, reason = "a share on screen")]
    let share = (used_pct / 100.0).clamp(0.0, 1.0) as f32;
    kit::progress::ring(theme, id, share, context_tone(theme, used_pct), px(side))
}

#[cfg(test)]
mod tests {
    use super::{checkout, meter_words, sentence, with_command, with_mention};

    /// The "+" menu begins a command at the message's start and a mention at the caret, set
    /// off from the word before it, with the caret after the sign each time.
    #[test]
    fn the_add_menu_begins_a_command_or_a_mention() {
        assert_eq!(with_command(""), ("/".to_owned(), 1));
        assert_eq!(with_command("fix it"), ("/ fix it".to_owned(), 1));
        assert_eq!(with_mention("", 0), ("@".to_owned(), 1));
        assert_eq!(with_mention("look at", 7), ("look at @".to_owned(), 9));
        assert_eq!(with_mention("look ", 5), ("look @".to_owned(), 6));
        assert_eq!(with_mention("ab", 1), ("a @b".to_owned(), 3));
        assert_eq!(
            with_mention("é", 1),
            ("@é".to_owned(), 1),
            "a caret inside a character backs off"
        );
    }

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
        assert_eq!(
            meter_words(&meters, slopty_core::WallMs::now()),
            ["Context 34% of 200k", "Five hour 41%"]
        );
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
