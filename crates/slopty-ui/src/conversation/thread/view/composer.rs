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
use crate::conversation::thread::activity::{self, Edited};
use crate::icons::{IconSize, Symbol};
use crate::kit::{self, ButtonKind};

/// What the composer says before anything is typed, while the thread has not said its agent.
pub(super) const PLACEHOLDER: &str = "Ask\u{2026}";

/// What the composer says before anything is typed: an invitation naming the agent, "Ask
/// Claude Code…". What it takes ("/" and "@") is taught by the "+" menu, not by the field.
pub(super) fn placeholder(agent: &slopty_proto::thread::AgentId) -> String {
    format!("Ask {}\u{2026}", super::agent_label(agent))
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

    /// What the thread changed, `(added, removed)` lines over the thread, as the worker's
    /// table counts them.
    fn changed(&self, cx: &App) -> (u32, u32) {
        let threads = self.hub.read(cx).threads();
        let changed = threads.rows().rows.get(&self.thread).map(|r| r.changed).unwrap_or_default();
        (changed.added, changed.removed)
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
            .map(|el| self.shell(el, capped, false))
            .text_size(self.z(theme.typography.small()))
            .text_color(hsla(s.text_secondary))
            .child(self.icon(Symbol::Terminal, s.text_muted))
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
            self.button("thread-handoff", HANDOFF, ButtonKind::Ghost)
                .on_click(cx.listener(|this, _ev, _w, cx| {
                    let _id = this.intent(Intent::Handoff, cx);
                }))
                .into_any_element(),
        )
    }

    /// What the thread changed, in the foot, a request on show or not: the edits are the
    /// thread's, and a permission's card asks only its own question.
    fn changes(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let edited = self.state(cx).map(activity::edited).unwrap_or_default();
        self.changes_chip("thread-changes", self.changed(cx), &edited, cx)
    }

    /// What the thread changed, `(added, removed)` lines, as counts that open the review;
    /// `None` when nothing changed. Its id is `id`.
    ///
    /// The review's one door over the composer, so it never hangs on the counts: files the
    /// last turn `edited` with no line counted (a file created whole, whose result carries no
    /// diff) are said under a pencil, by one file's name or how many.
    pub(super) fn changes_chip(
        &self,
        id: &'static str,
        (added, removed): (u32, u32),
        edited: &[Edited],
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let thread = self.thread;
        let counted = kit::changes(theme, added, removed);
        let named = counted.is_none();
        let counts = counted.or_else(|| {
            let words = match edited {
                [] => return None,
                [one] => one.path.rsplit('/').next().unwrap_or(&one.path).to_owned(),
                many => format!("{} files", many.len()),
            };
            // The pencil tells an edited file's name from the checkout's beside it.
            Some(
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(self.z(theme.spacing.xs))
                    .text_color(hsla(s.text_secondary))
                    .child(
                        crate::icons::icon(
                            theme,
                            Symbol::Pencil,
                            IconSize::Inline,
                            hsla(s.text_secondary),
                        )
                        .size(self.z(theme.typography.icon())),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(SharedString::from(words)),
                    ),
            )
        });
        counts.map(|counts| {
            crate::a11y::tab_stop(
                div()
                    .id(id)
                    .debug_selector(move || id.to_owned())
                    .role(Role::Button)
                    .aria_label(REVIEW_CHANGES)
                    .flex_none()
                    // A name gives up width before the place does; counts never.
                    .when(named, |el| el.flex_shrink_1().min_w_0())
                    .flex()
                    .items_center()
                    .px(self.z(theme.spacing.xs))
                    .rounded(self.z(theme.radii.xs))
                    .cursor_pointer()
                    .text_size(self.z(theme.typography.small()))
                    .hover(move |el| el.bg(hsla(s.hover)))
                    .active(move |el| el.bg(hsla(s.pressed)))
                    .child(counts)
                    .on_click(cx.listener(move |_this, _ev, _w, cx| {
                        cx.emit(ThreadViewEvent::Review { thread });
                    })),
                s.focus,
            )
            .into_any_element()
        })
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
        let glyph = crate::icons::GitGlyph::of_pull(pull.standing());
        let glyph_ink = glyph.state_ink(theme).unwrap_or(tone);
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
                        crate::icons::icon(theme, glyph, IconSize::Inline, hsla(glyph_ink))
                            .size(self.z(theme.typography.icon())),
                    )
                    .child(
                        kit::tabular(div()).child(SharedString::from(format!("#{}", pull.number))),
                    )
                    .on_click(cx.listener(|this, _ev, window, cx| this.open_commit(window, cx))),
                s.focus,
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

    /// Where an empty thread works, before its model: "studio · slopty", the question over
    /// the composer asking what to do there. Once the thread has rows its tile's header says it.
    fn place_chip(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let (_, place) = self.hero(cx)?;
        let theme = &self.theme;
        let words = match &place.folder {
            Some(folder) if place.worktree => format!(
                "{}{}new worktree of {folder}",
                place.machine,
                crate::workspace::META_SEPARATOR
            ),
            Some(folder) => {
                format!("{}{}{folder}", place.machine, crate::workspace::META_SEPARATOR)
            }
            None => place.machine.clone(),
        };
        Some(
            self.chip("thread-place", place.said())
                .role(Role::Label)
                .text_color(hsla(theme.surfaces.text_muted))
                .child(kit::fit_label("thread-place-words", words, theme))
                .into_any_element(),
        )
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
        // The model by its name alone: the agent's mark leads the tile's header, and beside
        // "Opus 5.5" it only said again whose model it is.
        let chip = self
            .chip("thread-model", format!("Model, {name}"))
            .child(kit::fit_label("thread-model-name", name, theme).fixed());
        Some(if switch {
            crate::a11y::tab_stop(
                chip.role(Role::Button)
                    .cursor_pointer()
                    .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
                    .child(
                        crate::icons::Drawn::disclosure(theme, Symbol::ChevronDown)
                            .slot(self.z(IconSize::Inline.slot(theme)), hsla(s.text_muted)),
                    )
                    .on_click(cx.listener(|this, _ev, _w, cx| this.toggle_models(cx))),
                s.focus,
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
        // The agent's default mode goes unsaid, and so does a mode it names none of: the "+"
        // menu holds the switch then ([`Self::add_menu`]).
        let named = named.filter(|_| !mode.is_some_and(is_default));
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
                            crate::icons::Drawn::disclosure(theme, Symbol::ChevronDown)
                                .slot(self.z(IconSize::Inline.slot(theme)), hsla(s.text_muted)),
                        )
                        .on_click(cx.listener(|this, _ev, _w, cx| this.toggle_modes(cx))),
                    s.focus,
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
        // The default level goes unsaid, and so does a level it names none of: the "+" menu
        // holds the switch then ([`Self::add_menu`]).
        let words = named.filter(|_| !effort.is_some_and(is_default))?;
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
                        crate::icons::Drawn::disclosure(theme, Symbol::ChevronDown)
                            .slot(self.z(IconSize::Inline.slot(theme)), hsla(s.text_muted)),
                    )
                    .on_click(cx.listener(|this, _ev, _w, cx| this.toggle_efforts(cx))),
                s.focus,
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
            Some(self.icon(Symbol::ExclamationmarkTriangle, s.error))
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
                s.focus,
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
                    // The ring alone says a context well within its window; from half full its
                    // share is read too, and from 80 % it takes the warning tone.
                    .children(used.filter(|u| *u >= FIGURE_FROM).map(|u| {
                        div().debug_selector(|| "thread-meter-figure".to_owned()).child(
                            kit::Rolling::new(
                                "thread-meter-figure",
                                share(u),
                                self.z(theme.typography.small()),
                            ),
                        )
                    }))
                    .map(kit::hint_timing)
                    .tooltip(move |_window, cx| {
                        let theme = std::rc::Rc::new(hint_theme.clone());
                        cx.new(|_| kit::Hint::new(hint.clone(), "", theme)).into()
                    }),
                s.focus,
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
            self.button("thread-interrupt-send", INTERRUPT_SEND, ButtonKind::Ghost)
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
            ("thread-stop", Symbol::StopFill, "Stop")
        } else if self.composing.editing() {
            ("thread-send", Symbol::Checkmark, "Update")
        } else if !working {
            ("thread-send", Symbol::ArrowUp, "Send")
        } else if self.send_now(cx) == Delivery::Queue {
            ("thread-send", Symbol::Clock, "Queue")
        } else {
            ("thread-send", Symbol::ArrowRight, "Steer")
        };
        let hint_theme = std::rc::Rc::new(theme.clone());
        let hint: SharedString = if stop && self.goal_goes_on(cx) {
            format!("{label}. {}", super::goal::GOES_ON).into()
        } else {
            label.into()
        };
        let el = kit::message::send_control(theme, self.zoom, id, icon, label)
            .map(kit::hint_timing)
            .tooltip(move |_window, cx| {
                cx.new(|_| kit::Hint::new(hint.clone(), "", std::rc::Rc::clone(&hint_theme))).into()
            });
        let el = el.on_click(cx.listener(move |this, _ev, window, cx| {
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
        crate::a11y::tab_stop(el, s.focus).into_any_element()
    }

    /// The composer's shell, which what stands in its place shares: a message's frame
    /// ([`kit::message::shell`]), the one the board's message to its orchestrator wears, all
    /// its corners round, or with the tray as its head only the foot's. `focused`: the field
    /// has the keyboard, and the hairline says so.
    pub(super) fn shell<E: gpui::Styled>(&self, el: E, capped: bool, focused: bool) -> E {
        kit::message::shell(el, &self.theme, self.zoom, capped, focused)
    }

    /// The composer card. While the agent's own TUI holds the session, where it is instead.
    /// `capped`: the tray stands on it as its head, so its top corners are square and its top
    /// edge is the quieter hairline between the two.
    pub(super) fn composer_box(
        &self,
        capped: bool,
        focused: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        if self.tui_holds(cx) == Some(true) {
            return self.held_strip(capped, cx);
        }
        if let Some(strip) = self.exited_strip(capped, cx) {
            return strip;
        }
        let theme = &self.theme;
        let editing = self.composing.editing();
        let view = cx.weak_entity();
        div()
            .id("thread-composer")
            .debug_selector(|| "thread-composer".to_owned())
            .key_context(super::COMPOSER_CTX)
            .w_full()
            .flex()
            .flex_col()
            .map(|el| self.shell(el, capped, focused))
            // Under the tray the shell has no top edge: the tray's band over the field's raised
            // tone parts them, with no line between.
            .when(capped, gpui::Styled::border_t_0)
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(self.z(theme.spacing.xs))
                    .px(self.z(theme.spacing.md))
                    .pt(self.z(theme.spacing.sm))
                    // The strips over the field are a row's words.
                    .text_size(self.z(theme.roles().chrome.size))
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
                    .px(self.z(theme.spacing.sm))
                    .pb(self.z(theme.spacing.sm))
                    .pt(self.z(theme.spacing.xs))
                    .child(self.composer_foot(editing, cx)),
            )
            .into_any_element()
    }

    /// The composer's foot, one row that fits the room it is given (`kit::priority_row`): the
    /// "+" and the send never leave; the rest leave the least needed first (the handoff, the
    /// place, the screen, the pull request, the mode, effort and work, the changes, the meter,
    /// the model) and wait in the "+" menu, so Send never leaves the card
    /// (`docs/decisions/ui.md`, "How surfaces adapt to their room").
    fn composer_foot(&self, editing: bool, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let row = kit::priority_row("thread-foot")
            .h(self.z(theme.density.control))
            .gap(self.z(theme.spacing.xxs))
            .dropped(&self.foot_dropped);
        let item =
            |row: kit::PriorityRow, (key, priority): (&'static str, kit::Priority), el| match el {
                Some(el) => row.item(key, priority, el),
                None => row,
            };
        let row = item(row, FOOT_ADD, (!editing).then(|| self.add_button(cx)));
        let row = item(row, FOOT_PLACE, self.place_chip(cx));
        let row = item(row, FOOT_MODEL, self.model_chip(cx));
        let row = item(row, FOOT_EFFORT, self.effort_chip(cx));
        let row = item(row, FOOT_MODE, self.mode_chip(cx));
        let row = item(row, FOOT_TASKS, self.tasks_chip(cx));
        let row = item(row, FOOT_PULL, self.pull_chip(cx));
        let row = item(row, FOOT_CHANGES, self.changes(cx));
        let row = item(row, FOOT_SCREEN, self.screen_chip(cx));
        let row = item(row.end(), FOOT_METER, self.meter(cx));
        let row = item(row, FOOT_HANDOFF, self.handoff_button(cx));
        let row = item(row, FOOT_INTERRUPT, self.interrupt_send(cx));
        let row = item(row, FOOT_SEND, Some(self.send_button(cx)));
        row.into_any_element()
    }
}

/// The composer foot's items: each one's key in [`kit::Dropped`] and how much it is needed.
/// The "+" holds what leaves, and the send is the one solid; neither ever leaves.
const FOOT_ADD: (&str, kit::Priority) = ("add", kit::Priority::ESSENTIAL);
/// Where an empty thread works: the first to leave.
const FOOT_PLACE: (&str, kit::Priority) = ("place", kit::Priority(40));
/// The model.
const FOOT_MODEL: (&str, kit::Priority) = ("model", kit::Priority(176));
/// How full the context is.
const FOOT_METER: (&str, kit::Priority) = ("meter", kit::Priority(160));
/// "Interrupt and send", while something is typed during a turn.
const FOOT_INTERRUPT: (&str, kit::Priority) = ("interrupt", kit::Priority(152));
/// What the thread changed, the way to the review.
const FOOT_CHANGES: (&str, kit::Priority) = ("changes", kit::Priority(144));
/// The effort.
const FOOT_EFFORT: (&str, kit::Priority) = ("effort", kit::Priority::MEDIUM);
/// The mode.
const FOOT_MODE: (&str, kit::Priority) = ("mode", kit::Priority::MEDIUM);
/// The background work.
const FOOT_TASKS: (&str, kit::Priority) = ("tasks", kit::Priority::MEDIUM);
/// The branch's pull request.
const FOOT_PULL: (&str, kit::Priority) = ("pull", kit::Priority(112));
/// The agent's screen.
const FOOT_SCREEN: (&str, kit::Priority) = ("screen", kit::Priority(96));
/// "Continue in the terminal".
const FOOT_HANDOFF: (&str, kit::Priority) = ("handoff", kit::Priority(48));
/// The send.
const FOOT_SEND: (&str, kit::Priority) = ("send", kit::Priority::ESSENTIAL);

impl ThreadView {
    /// The "+" at the foot's start and the menu it opens: attach files, then the two menus
    /// the field takes, "/" for commands and "@" for files and symbols, so the field's own
    /// words can be an invitation rather than a lesson.
    fn add_button(&self, cx: &Context<Self>) -> AnyElement {
        let menu = self.add_open.then(|| self.add_menu(cx));
        div()
            .relative()
            .flex_none()
            .child(self.icon_button(ADD_BUTTON, Symbol::Plus, ADD_LABEL).on_click(cx.listener(
                |this, _ev, window, cx| {
                    this.add_open = !this.add_open;
                    // Closed by its button as by Esc: the keyboard goes back to the field.
                    if !this.add_open {
                        this.focus(window, cx);
                    }
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
        // A phone's and a tablet's pictures are in Photos, which the Files picker cannot reach.
        if self.photos {
            let to = this.clone();
            menu.push(kit::MenuItem::new("photos", PICK_PHOTOS, move |_w, cx| {
                let _gone = to.update(cx, |this, cx| {
                    this.add_open = false;
                    cx.emit(ThreadViewEvent::PickPhotos);
                    cx.notify();
                });
            }));
        }
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
        self.also_rows(&mut menu, cx);
        if self.runs > 1 && self.draft.is_none() {
            let to = this.clone();
            let thread = self.thread;
            menu.separate();
            menu.push(kit::MenuItem::new(
                "runs",
                format!("Review the {} runs side by side", self.runs),
                move |_w, cx| {
                    let _gone = to.update(cx, |this, cx| {
                        this.add_open = false;
                        cx.emit(ThreadViewEvent::ReviewRuns { thread });
                        cx.notify();
                    });
                },
            ));
        }
        // The mode and the effort switch here while their chips are away (the default, or one
        // the agent names none of), and beside them as well.
        let meta = self.state(cx).map(|state| &state.meta);
        let modes = meta.is_some_and(|m| m.can(Cap::SET_MODE) && !m.modes.is_empty());
        let efforts = meta.is_some_and(|m| m.can(Cap::SET_EFFORT) && !m.efforts.is_empty());
        if modes || efforts {
            menu.separate();
        }
        if modes {
            let to = this.clone();
            menu.push(kit::MenuItem::new("modes", MODES, move |_w, cx| {
                let _gone = to.update(cx, |this, cx| {
                    this.add_open = false;
                    this.toggle_modes(cx);
                });
            }));
        }
        if efforts {
            let to = this.clone();
            menu.push(kit::MenuItem::new("efforts", EFFORTS, move |_w, cx| {
                let _gone = to.update(cx, |this, cx| {
                    this.add_open = false;
                    this.toggle_efforts(cx);
                });
            }));
        }
        self.left_out(&mut menu, cx);
        // A row chosen closes it before it runs, so the row may take the keyboard elsewhere
        // (the commit sheet does).
        let closed = this.clone();
        let panel = kit::MenuPanel::new(
            ADD_MENU,
            ADD_LABEL,
            std::rc::Rc::new(menu),
            &self.theme,
            move |window, cx| {
                let _gone = closed.update(cx, |this, cx| {
                    this.add_open = false;
                    this.focus(window, cx);
                    cx.notify();
                });
            },
        )
        .on_dismiss(move |window, cx| {
            let _gone = this.update(cx, |this, cx| {
                this.add_open = false;
                cx.notify();
            });
            // Once the press that dismissed it is done with: a press outside lands on what it
            // pressed, which would take the keyboard after a focus given here.
            let to = this.clone();
            window.defer(cx, move |window, cx| {
                let _gone = to.update(cx, |this, cx| this.focus(window, cx));
            });
        });
        gpui::deferred(gpui::anchored().anchor(gpui::Anchor::BottomLeft).child(panel))
            .with_priority(crate::palette::Layer::Submenu.priority())
            .into_any_element()
    }
}

impl ThreadView {
    /// A start's other agents, after a separator, each ticked while it is to run the same
    /// message too, in a new worktree of its own: Orca's parallel worktrees, one prompt on
    /// several agents to compare. Only a draft in a new worktree offers them.
    fn also_rows(&self, menu: &mut kit::Menu, cx: &Context<Self>) {
        let Some(draft) = self.draft.clone() else { return };
        let (others, also) = {
            let d = draft.read(cx);
            (d.others().to_vec(), d.also().to_vec())
        };
        if others.is_empty() {
            return;
        }
        menu.separate();
        for agent in others {
            let on = also.contains(&agent);
            let label = format!("{ALSO_RUN} {}", super::agent_label(&agent));
            let to = draft.downgrade();
            let key = format!("also-{}", agent.0);
            menu.push(
                kit::MenuItem::new(key, label, move |_w, cx| {
                    let _gone = to.update(cx, |d, cx| d.toggle_also(&agent, cx));
                })
                .mark(kit::menu::Mark::Check(on)),
            );
        }
    }

    /// What the foot left out for want of room, at the "+" menu's end after a separator, each
    /// doing what its chip does ([`Self::composer_foot`]). The mode and the effort are in the menu
    /// already.
    fn left_out(&self, menu: &mut kit::Menu, cx: &Context<Self>) {
        let dropped = self.foot_dropped.keys();
        let this = cx.entity().downgrade();
        let state = self.state(cx);
        let mut rows: Vec<kit::MenuItem> = Vec::new();
        for key in &dropped {
            let to = this.clone();
            let row = match key.as_ref() {
                k if k == FOOT_MODEL.0 => state
                    .filter(|st| st.meta.can(Cap::SET_MODEL) && !st.meta.models.is_empty())
                    .map(|st| {
                        let name = model_said(&st.meters)
                            .unwrap_or_else(|| agent_name(&st.meta.agent).to_owned());
                        kit::MenuItem::new("model", MODEL, move |_w, cx| {
                            let _gone = to.update(cx, |this, cx| {
                                this.add_open = false;
                                this.toggle_models(cx);
                            });
                        })
                        .detail(name)
                    }),
                k if k == FOOT_METER.0 => state.map(|st| {
                    let words = meter_words(&st.meters, crate::clock::now(cx));
                    let first = words.into_iter().next().unwrap_or_else(|| CONTEXT.to_owned());
                    kit::MenuItem::new("meter", first, move |_w, cx| {
                        let _gone = to.update(cx, |this, cx| {
                            this.add_open = false;
                            this.meter_open = true;
                            cx.notify();
                        });
                    })
                }),
                k if k == FOOT_TASKS.0 => state.map(|st| {
                    kit::MenuItem::new(
                        "tasks",
                        super::tray::tasks_words(&st.tasks),
                        move |_w, cx| {
                            let _gone = to.update(cx, |this, cx| {
                                this.add_open = false;
                                this.tasks_open = true;
                                cx.notify();
                            });
                        },
                    )
                }),
                k if k == FOOT_CHANGES.0 => {
                    let thread = self.thread;
                    Some(kit::MenuItem::new("changes", REVIEW_CHANGES, move |_w, cx| {
                        let _gone = to.update(cx, |this, cx| {
                            this.add_open = false;
                            cx.emit(ThreadViewEvent::Review { thread });
                            cx.notify();
                        });
                    }))
                }
                k if k == FOOT_SCREEN.0 => self.agent_screen(cx).map(|screen| {
                    let words = format!("Watch {}", super::screens::short(&screen));
                    kit::MenuItem::new("screen", words, move |_w, cx| {
                        let _gone = to.update(cx, |this, cx| {
                            this.add_open = false;
                            this.watch_screen(cx);
                        });
                    })
                }),
                k if k == FOOT_HANDOFF.0 => {
                    Some(kit::MenuItem::new("handoff", HANDOFF, move |_w, cx| {
                        let _gone = to.update(cx, |this, cx| {
                            this.add_open = false;
                            let _id = this.intent(Intent::Handoff, cx);
                        });
                    }))
                }
                k if k == FOOT_INTERRUPT.0 => {
                    Some(kit::MenuItem::new("interrupt", INTERRUPT_SEND, move |window, cx| {
                        let _gone = to.update(cx, |this, cx| {
                            this.add_open = false;
                            this.submit(Delivery::Interrupt, window, cx);
                        });
                    }))
                }
                _ => None,
            };
            rows.extend(row);
        }
        // The way to commit where the thread works: the tile's header says where, and the
        // sheet opens from here.
        if self.repo(cx).is_some() {
            rows.push(kit::MenuItem::new("commit", COMMIT, move |window, cx| {
                let _gone = this.update(cx, |this, cx| {
                    this.add_open = false;
                    this.open_commit(window, cx);
                });
            }));
        }
        if rows.is_empty() {
            return;
        }
        menu.separate();
        for row in rows {
            menu.push(row);
        }
    }
}

/// The "+" menu's row that lists the agent's models, while the foot has no room for the chip.
const MODEL: &str = "Model";

/// The "+" menu's row for the meter, before the agent has said how full its context is.
const CONTEXT: &str = "Context";

/// The "+" menu's row that opens the review, while the foot has no room for the changes.
const REVIEW_CHANGES: &str = "Review the changes";

/// The "+" menu's row that opens the commit sheet on the repository the thread works in.
const COMMIT: &str = "Commit\u{2026}";

/// The words of the way to hand the session to the agent's own TUI.
const HANDOFF: &str = "Continue in the terminal";

/// The words of the way to stop the turn and send the draft.
const INTERRUPT_SEND: &str = "Interrupt and send";

/// The selector of the composer's "+".
pub(crate) const ADD_BUTTON: &str = "thread-attach";

/// The selector of the menu the "+" opens.
pub(crate) const ADD_MENU: &str = "thread-add-menu";

/// What the "+" is called.
const ADD_LABEL: &str = "Add to the message";

/// The "+" menu's row that picks files to send.
pub(crate) const ATTACH_FILES: &str = "Attach files\u{2026}";

/// The "+" menu's row that picks photos and videos to send, on iOS.
pub(crate) const PICK_PHOTOS: &str = "Photos\u{2026}";

/// The "+" menu's row that starts a command, as typing "/" does.
pub(crate) const COMMANDS: &str = "Commands";

/// The "+" menu's row that names a file or a symbol, as typing "@" does.
pub(crate) const MENTIONS: &str = "Files and symbols";
/// The "+" menu's row that starts a draft's message on another agent too: "Also run Codex".
pub(crate) const ALSO_RUN: &str = "Also run";

/// The "+" menu's row that lists the agent's modes.
pub(crate) const MODES: &str = "Mode";
/// The "+" menu's row that lists how hard the model can think.
pub(crate) const EFFORTS: &str = "Effort";

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

/// The share of the context in use from which the meter says it in figures beside its ring.
const FIGURE_FROM: f64 = 50.0;

/// Whether `name`, a mode or an effort as the agent says it, is the agent's default, which the
/// composer leaves unsaid.
pub(super) fn is_default(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case("default")
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
    use super::{meter_words, sentence, with_command, with_mention};

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
}
