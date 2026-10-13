//! The composer: a card at the foot of the column, full-bleed at the pane's foot when the pane
//! is narrow.
//!
//! The menu, the chips and the field; then Delta's foot row: where the agent works on its left
//! (the checkout and its branch, and what the thread changed), and
//! at its right how full the context is, the model and how hard it thinks, the mode, the
//! paperclip, and the one primary: send, or stop while a turn runs and nothing is typed. While
//! the agent's own TUI holds the session, a strip saying so stands in its place.

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Div, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled, Window, div, px,
};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::{Sizable as _, Size};
use slopty_kit::colors::hsla;
use slopty_kit::icons::{Mark, Symbol};
use slopty_kit::kit;
use slopty_proto::thread::{Cap, Delivery};
use slopty_theme::{Rgb, Theme};

use super::{ThreadView, ThreadViewEvent, agent_name};

/// What the composer says before anything is typed, while the thread has not said its agent.
pub(super) const PLACEHOLDER: &str = "Ask\u{2026}";

/// What the composer says before anything is typed: an invitation naming the agent, "Ask
/// Claude Code…". What it takes ("/" and "@") is taught by the paperclip's menu, not by the field.
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

/// How hard the model thinks as the model chip says it after the name: the label of the level
/// the agent published when one matches, else its own word in sentence case; nothing for the
/// default level, which goes unsaid.
pub(super) fn effort_said(
    meta: &slopty_proto::thread::ThreadMeta,
    meters: &slopty_proto::thread::Meters,
) -> Option<String> {
    let effort =
        meters.effort.as_deref().map(str::trim).filter(|e| !e.is_empty() && !is_default(e))?;
    Some(
        meta.efforts
            .iter()
            .find(|known| known.id == effort || known.label == effort)
            .map_or_else(|| sentence(effort), |known| known.label.clone()),
    )
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
    /// A quiet chip in the composer's foot: the model, the mode.
    fn chip(&self, id: &'static str, label: String) -> gpui::Stateful<Div> {
        let theme = &self.theme;
        let s = theme.surfaces;
        div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .aria_label(SharedString::from(label))
            .flex_none()
            .h(px(theme.density.control))
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            // The pad an icon button keeps round its glyph, so words and glyphs along the foot
            // stand the one gap apart.
            .px(px(foot_pad(theme)))
            .rounded(px(theme.radii.sm))
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text_secondary))
    }

    /// Where an empty thread works, before its model: "studio · slopty", the question over
    /// the composer asking what to do there. Once the thread has rows its tile's header says it.
    /// A draft in a repository switches there between the folder and a new worktree of it
    /// ([`Self::place_switch`]).
    fn place_chip(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let (_, place) = self.hero(cx)?;
        let theme = &self.theme;
        let s = theme.surfaces;
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
        let chip = self
            .chip("thread-place", format!("Place, {}", place.said()))
            // On the ledge the place gives up width before the meter does.
            .flex_shrink_1()
            .min_w_0()
            .text_color(hsla(s.text_muted))
            .child(kit::fit_label("thread-place-words", words, theme));
        if self.place_switch(cx).is_none() {
            return Some(chip.role(Role::Label).into_any_element());
        }
        Some(
            self.chip_switch(chip, cx.listener(|this, _ev, _w, cx| this.toggle_places(cx)))
                .into_any_element(),
        )
    }

    /// The composer's ledge, over its field, as `MonoCode`'s: where the work is on its left (the
    /// place and the branch), how full the context is at its right. Only while the tile's
    /// header does not say where ([`Self::on_ledge`]): a row for the meter alone would take
    /// the thread's height for one figure, so the foot keeps it then.
    fn ledge(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if !self.on_ledge(cx) {
            return None;
        }
        let place = self.place_chip(cx);
        let base = self.base_chip(cx);
        let meter = self.meter(cx);
        let theme = &self.theme;
        Some(
            div()
                .id("thread-ledge")
                .debug_selector(|| "thread-ledge".to_owned())
                .w_full()
                .h(px(theme.density.control))
                .flex()
                .items_center()
                .gap(px(theme.spacing.xxs))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .gap(px(theme.spacing.xxs))
                        .children(place)
                        .children(base),
                )
                .children(meter)
                .into_any_element(),
        )
    }

    /// Whether the composer has a ledge: while it says where the work is, which is while the
    /// thread asks its first message ([`Self::hero`]).
    fn on_ledge(&self, cx: &Context<Self>) -> bool {
        self.hero(cx).is_some()
    }

    /// The branch beside a draft's place: in a new worktree the one it starts from, "from
    /// main", a menu of the branches its clone knows ([`Self::base_switch`]); in the folder
    /// itself the one checked out, as words.
    fn base_chip(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        if self.base_switch(cx).is_none() {
            let current = self.place_switch(cx)?.current.clone()?;
            return Some(
                self.chip("thread-branch", format!("Branch, {current}"))
                    .role(Role::Label)
                    .flex_shrink_1()
                    .min_w_0()
                    .text_color(hsla(s.text_muted))
                    .child(self.icon(Symbol::ArrowTriangleBranch, s.text_muted))
                    .child(kit::fit_label("thread-branch-name", current, theme))
                    .into_any_element(),
            );
        }
        let base = self.draft_base(cx).unwrap_or_else(|| "HEAD".to_owned());
        let chip = self
            .chip("thread-base", format!("Base branch, {base}"))
            .text_color(hsla(s.text_muted))
            .child(self.icon(Symbol::ArrowTriangleBranch, s.text_muted))
            .child(kit::fit_label("thread-base-name", format!("from {base}"), theme));
        Some(
            self.chip_switch(chip, cx.listener(|this, _ev, _w, cx| this.toggle_bases(cx)))
                .into_any_element(),
        )
    }

    /// `chip` as a switch: a button that brightens under the pointer, its disclosure after it,
    /// `click` opening its menu; a stop for the keyboard.
    fn chip_switch(
        &self,
        chip: gpui::Stateful<Div>,
        click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> gpui::Stateful<Div> {
        let theme = &self.theme;
        let s = theme.surfaces;
        slopty_kit::a11y::tab_stop(
            chip.role(Role::Button)
                .cursor_pointer()
                .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
                .child(
                    slopty_kit::icons::Drawn::disclosure(theme, Symbol::ChevronDown)
                        .slot(px(theme.icon()), hsla(s.text_muted)),
                )
                .on_click(click),
            s.focus,
        )
    }

    /// The model chip: the model's name, and after it how hard it thinks where that is not
    /// the default ("Opus 5.5 · High"); a menu of the agent's models and levels when it can
    /// switch either ([`Self::switches`]). Effort has no chip of its own: it is the model's.
    fn model_chip(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let state = self.state(cx)?;
        let (models, efforts) = self.switches(cx);
        let switch = !models.is_empty() || !efforts.is_empty();
        let model =
            model_said(&state.meters).unwrap_or_else(|| agent_name(&state.meta.agent).to_owned());
        let name = match effort_said(&state.meta, &state.meters) {
            Some(effort) => format!("{model} \u{b7} {effort}"),
            None => model,
        };
        // The model by its name alone: the agent's mark leads the tile's header, and beside
        // "Opus 5.5" it only said again whose model it is.
        // The model, the mode and the "+" are the foot's controls, squares on the selected
        // wash, `MonoCode`'s composer controls; what is only read is quiet words.
        let chip = self
            .chip("thread-model", format!("Model, {name}"))
            .child(kit::fit_label("thread-model-name", name, theme).fixed());
        Some(if switch {
            slopty_kit::a11y::tab_stop(
                squared(chip, theme, false)
                    .role(Role::Button)
                    .cursor_pointer()
                    .child(
                        slopty_kit::icons::Drawn::disclosure(theme, Symbol::ChevronDown)
                            .slot(px(theme.icon()), hsla(s.text_muted)),
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
                slopty_kit::a11y::tab_stop(
                    squared(chip, theme, false)
                        .role(Role::Button)
                        .cursor_pointer()
                        .child(
                            slopty_kit::icons::Drawn::disclosure(theme, Symbol::ChevronDown)
                                .slot(px(theme.icon()), hsla(s.text_muted)),
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

    /// How full the context is, a ring and its share, with the plan's rate windows and when
    /// each resets in its hint: quiet words, read, not pressed.
    fn meter(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let meters = &self.state(cx)?.meters;
        let used = super::context_used(meters).filter(|u| *u >= RING_FROM);
        if used.is_none() && meters.limits.is_empty() {
            return None;
        }
        let hint = meter_words(meters, slopty_kit::clock::now(cx)).join("\n");
        let hint_theme = theme.clone();
        Some(
            slopty_kit::a11y::tab_stop(
                self.chip("thread-meter", hint.replace('\n', ", "))
                    .role(Role::Label)
                    .hover(move |el| el.text_color(hsla(s.text_secondary)))
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .children(used.map(|u| {
                        context_ring(theme, "thread-meter-ring", u, theme.typography.small())
                    }))
                    // The ring alone says a context well within its window; from half full its
                    // share is read too, and from 80 % it takes the warning tone.
                    .children(used.filter(|u| *u >= FIGURE_FROM).map(|u| {
                        div().debug_selector(|| "thread-meter-figure".to_owned()).child(
                            kit::Rolling::new(
                                "thread-meter-figure",
                                share(u),
                                px(theme.typography.small()),
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

    /// The square at the foot's end, in the tinted primary: stop the turn while one runs and
    /// nothing is typed; otherwise what ↵ will do with the draft, in its glyph and its name:
    /// Update a waiting message being changed, Steer into the turn under way, Queue after it
    /// (an agent that takes no steer), or Send.
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
            ("thread-send", Symbol::Send, "Send")
        } else if self.send_now(cx) == Delivery::Queue {
            ("thread-send", Symbol::Clock, "Queue")
        } else {
            ("thread-send", Symbol::ArrowRight, "Steer")
        };
        let hint_theme = std::rc::Rc::new(theme.clone());
        let hint = SharedString::from(label);
        let el = kit::message::send_control(theme, id, icon, label, stop || !empty)
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
        slopty_kit::a11y::tab_stop(el, s.focus).into_any_element()
    }

    /// The frame of what stands in the composer's place once its agent has gone: a message's
    /// ([`kit::message::shell`]), the one the board's message to its orchestrator wears, the
    /// pane's width under the lighter line.
    pub(super) fn shell<E: Styled>(&self, el: E) -> E {
        kit::message::shell(el, &self.theme)
    }

    /// The composer, Delta's next turn, in two parts: its field, the next turn's slot in the
    /// reading column with the person's mark hung beside it as it hangs beside their turns,
    /// which follows the last turn (`ThreadView::foot`); and its foot, the pane's frame with
    /// equal ends, pinned to the pane's bottom. No line runs over either: the field is a turn,
    /// and the foot a frame (`.research/warp-composition-2026-10-12.md` §1.12). An agent that
    /// has gone stands its strip in the field's place, with no foot.
    pub(super) fn composer_box(&self, cx: &Context<Self>) -> (AnyElement, Option<AnyElement>) {
        if let Some(strip) = self.exited_strip(cx) {
            return (strip, None);
        }
        let theme = &self.theme;
        let editing = self.composing.editing();
        let view = cx.weak_entity();
        // A field that asks what the agent should do is named by its question, so it is heard
        // as it is seen.
        let asks = self.hero(cx).is_some();
        let name =
            if asks { SharedString::from(self.placeholder.clone()) } else { "Message".into() };
        let field = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(theme.spacing.xs))
            .pt(px(theme.spacing.sm))
            // The strips over the field are a row's words.
            .text_size(px(theme.roles().chrome.size))
            .children(self.ledge(cx))
            .children(self.exited_line(cx))
            .children(self.menu_section(cx))
            .children(self.limit_strip(cx))
            .children(self.failed_strip(cx))
            .children(self.notice_strip())
            .children(self.editing_strip(cx))
            .children(self.attachment_chips(cx))
            .child(
                // The kit's least field, its pads and line fixed in points, the words
                // and lines at the prose's size.
                div()
                    .debug_selector(|| "thread-composer-field".to_owned())
                    .py(px(theme.spacing.xs))
                    .child(
                        self.hang(
                            // Its words on the content edge: the field's own pad goes.
                            Textarea::new(&self.composer)
                                .with_size(Size::XSmall)
                                .appearance(false)
                                .bordered(false)
                                .px_0()
                                .text_size(px(theme.typography.prose()))
                                .line_height(gpui::relative(theme.typography.prose_line_height))
                                .aria_label(name)
                                .on_paste(move |item, _window, cx| {
                                    view.update(cx, |v, cx| v.paste_attachment(item, cx))
                                        .unwrap_or(false)
                                })
                                .into_any_element(),
                            Some(Mark::Symbol(Symbol::Person)),
                            "turn-mark-composer".to_owned(),
                        ),
                    ),
            );
        // The foot is the pane's frame, as its header is, not the reading column's: it spans
        // the pane with the same inset at both ends, what the work is on the left and the means
        // of sending on the right (`.research/warp-composition-2026-10-12.md` §1.12).
        let inset = px(theme.spacing.inset_trailing());
        let foot = div()
            .id("thread-composer-foot")
            .debug_selector(|| "thread-composer-foot".to_owned())
            .w_full()
            .flex_none()
            .flex()
            .pl(inset)
            .pr(inset)
            .child(div().flex_1().min_w_0().child(self.composer_foot(editing, cx)));
        let field = div()
            .id("thread-composer")
            .debug_selector(|| "thread-composer".to_owned())
            .key_context(super::COMPOSER_CTX)
            .w_full()
            .child(
                self.measure_box()
                    // While its field asks what the agent should do ([`Self::hero`]).
                    .when(asks, |el| el.debug_selector(|| "thread-question".to_owned()))
                    .child(div().w_full().flex().flex_col().child(field)),
            );
        (field.into_any_element(), Some(foot.into_any_element()))
    }

    /// The composer's foot, one row that fits the room it is given (`kit::priority_row`): the
    /// "+" and the send never leave; the rest leave the least needed first (the mode, the meter,
    /// the model) and wait in the "+" menu, so Send never leaves the card (`docs/decisions/ui.md`,
    /// "How surfaces adapt to their room"). Where the work is and how full the context is stand
    /// on the ledge over the field ([`Self::ledge`]).
    fn composer_foot(&self, editing: bool, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        // One gap between every control, each keeping an icon button's pad round its glyph or
        // words: the foot reads as one rhythm, Delta's.
        let row = kit::priority_row("thread-foot")
            .h(px(theme.density.foot))
            .gap(px(theme.spacing.xxs))
            .dropped(&self.foot_dropped);
        let item =
            |row: kit::PriorityRow, (key, priority): (&'static str, kit::Priority), el| match el {
                Some(el) => row.item(key, priority, el),
                None => row,
            };
        // Where the work is: only once the thread has rows, while the ledge does not say it.
        let placed = !self.on_ledge(cx);
        let row = item(row, FOOT_CHECKOUT, placed.then(|| self.checkout_chip(cx)).flatten());
        let row = item(row, FOOT_DIFF, placed.then(|| self.diff_stat(cx)).flatten());
        let meter = placed.then(|| self.meter(cx)).flatten();
        let row = item(row.end(), FOOT_METER, meter);
        let row = item(row, FOOT_RUNNER, Some(self.runner_chip(cx)));
        let row = item(row, FOOT_MODEL, self.model_chip(cx));
        let row = item(row, FOOT_MODE, self.mode_chip(cx));
        let row = item(row, FOOT_ADD, (!editing).then(|| self.add_button(cx)));
        let row = item(row, FOOT_SEND, Some(self.send_button(cx)));
        row.into_any_element()
    }

    /// Where the agent works, Delta's foot: the repository's glyph, then the checkout by its
    /// folder's name and its branch, "slopty / main", in the secondary tone. Its pad is an icon
    /// button's, as every control of the foot keeps.
    fn checkout_chip(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let state = self.state(cx)?;
        let folder = std::path::Path::new(state.meta.cwd.trim_end_matches('/'))
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .filter(|f| !f.is_empty())?;
        let branch = state.meta.facts.get(BRANCH_FACT).map(|b| b.trim()).filter(|b| !b.is_empty());
        let words = match branch {
            Some(branch) => format!("{folder} / {branch}"),
            None => folder,
        };
        let chip = self
            .chip("thread-checkout", format!("{WORKS_IN} {words}"))
            .flex_shrink_1()
            .min_w_0()
            .child(self.icon(Symbol::GitRepo, s.text_muted))
            .child(kit::fit_label("thread-checkout-words", words, theme));
        Some(squared(chip, theme, false).role(Role::Label).into_any_element())
    }

    /// Where the agent runs, Delta's runner: the machine's glyph and its name in quiet words.
    /// The header leaves it to the foot, so it is said once.
    fn runner_chip(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let machine = self.hub.read(cx).worker().to_owned();
        self.chip("thread-runner", format!("{RUNS_ON} {machine}"))
            .role(Role::Label)
            .child(self.icon(slopty_kit::icons::machine(None), theme.surfaces.text_muted))
            .child(kit::fit_label("thread-runner-name", machine, theme).fixed())
            .into_any_element()
    }

    /// What the thread changed, in figures: "+12 −3", Delta's diff size (`kit::changes`, the
    /// added in the success tone and the removed in the error tone); nothing while it changed
    /// nothing.
    fn diff_stat(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let state = self.state(cx)?;
        let (added, removed) = state.turns.iter().fold((0_u32, 0_u32), |(a, r), turn| {
            (a.saturating_add(turn.changed.added), r.saturating_add(turn.changed.removed))
        });
        let figures = kit::changes(theme, added, removed)?;
        Some(
            div()
                .id("thread-diff-stat")
                .debug_selector(|| "thread-diff-stat".to_owned())
                .role(Role::Label)
                .aria_label(SharedString::from(format!("{added} added, {removed} removed")))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(foot_pad(theme)))
                .pr(px(foot_pad(theme)))
                .text_size(px(theme.typography.small()))
                // A divider parts it from the checkout before it, Delta's.
                .child(
                    div()
                        .flex_none()
                        .w(kit::HAIR)
                        .h(px(theme.glyph()))
                        .bg(hsla(theme.surfaces.stroke)),
                )
                .child(figures)
                .into_any_element(),
        )
    }
}

impl ThreadView {
    /// A square of the composer foot's around one 14 pt glyph, before [`squared`] gives it its
    /// wash: a button named `label`.
    fn control_square(
        &self,
        id: &'static str,
        icon: Symbol,
        label: &'static str,
    ) -> gpui::Stateful<Div> {
        let theme = &self.theme;
        let s = theme.surfaces;
        slopty_kit::a11y::tab_stop(
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .role(Role::Button)
                .aria_label(label)
                .flex_none()
                .size(px(kit::icon_button_side(theme)))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .child(slopty_kit::icons::icon(theme, icon, hsla(s.text_secondary))),
            s.focus,
        )
    }
}

/// The pad round a foot control's glyph or words: an icon button's, half the room its square
/// keeps round its glyph.
const fn foot_pad(theme: &Theme) -> f32 {
    (kit::icon_button_side(theme) - theme.glyph()) / 2.0
}

/// `el` as one of the composer foot's controls, Zed's subtle button: bare at rest at the small
/// radius, its words and glyph in the secondary tone, the pointer's wash under it and the
/// selected wash while its menu is `open`.
fn squared(el: gpui::Stateful<Div>, theme: &Theme, open: bool) -> gpui::Stateful<Div> {
    let s = theme.surfaces;
    el.rounded(px(theme.radii.sm))
        .when(open, move |el| el.bg(hsla(s.selected)))
        .text_color(hsla(s.text_secondary))
        .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
}

/// The composer foot's items: each one's key in [`kit::Dropped`] and how much it is needed.
/// The paperclip holds what leaves, and the send is the one primary; neither ever leaves.
const FOOT_ADD: (&str, kit::Priority) = ("add", kit::Priority::ESSENTIAL);
/// Where the agent works.
const FOOT_CHECKOUT: (&str, kit::Priority) = ("checkout", kit::Priority(168));
/// What the thread changed.
const FOOT_DIFF: (&str, kit::Priority) = ("diff", kit::Priority::MEDIUM);
/// Where the agent runs: only read, so it leaves first.
const FOOT_RUNNER: (&str, kit::Priority) = ("runner", kit::Priority::LOW);
/// What the runner chip is named, before the machine.
const RUNS_ON: &str = "Runs on";
/// The model.
const FOOT_MODEL: (&str, kit::Priority) = ("model", kit::Priority(176));
/// How full the context is, while the composer has no ledge for it.
const FOOT_METER: (&str, kit::Priority) = ("meter", kit::Priority(160));
/// The mode.
const FOOT_MODE: (&str, kit::Priority) = ("mode", kit::Priority::MEDIUM);
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
            .child(
                squared(
                    self.control_square(ADD_BUTTON, Symbol::Paperclip, ADD_LABEL),
                    &self.theme,
                    self.add_open,
                )
                .on_click(cx.listener(|this, _ev, window, cx| {
                    this.add_open = !this.add_open;
                    // Closed by its button as by Esc: the keyboard goes back to the field.
                    if !this.add_open {
                        this.focus(window, cx);
                    }
                    cx.notify();
                })),
            )
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
            let others = self.runs.saturating_sub(1);
            let closed =
                if others == 1 { "the other".to_owned() } else { format!("the other {others}") };
            menu.push(kit::MenuItem::new(
                "keep-run",
                format!("Keep this run, close {closed}"),
                move |_w, cx| {
                    let _gone = to.update(cx, |this, cx| {
                        this.add_open = false;
                        cx.emit(ThreadViewEvent::KeepRun { thread });
                        cx.notify();
                    });
                },
            ));
        }
        // The mode switches here while its chip is away (the default, or one the agent names
        // none of), and beside it as well.
        let meta = self.state(cx).map(|state| &state.meta);
        let modes = meta.is_some_and(|m| m.can(Cap::SET_MODE) && !m.modes.is_empty());
        if modes {
            menu.separate();
            let to = this.clone();
            menu.push(kit::MenuItem::new("modes", MODES, move |_w, cx| {
                let _gone = to.update(cx, |this, cx| {
                    this.add_open = false;
                    this.toggle_modes(cx);
                });
            }));
        }
        self.left_out(&mut menu, cx);
        // A row chosen closes it before it runs, so the row may take the keyboard elsewhere.
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
            .with_priority(slopty_kit::layer::Layer::Submenu.priority())
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

    /// What the foot left out for want of room, at the "+" menu's end after a separator: the
    /// model, doing what its chip does ([`Self::composer_foot`]). The mode is in the menu
    /// already; the meter is words to read, which wait for the room.
    fn left_out(&self, menu: &mut kit::Menu, cx: &Context<Self>) {
        let dropped = self.foot_dropped.keys();
        let this = cx.entity().downgrade();
        let (models, efforts) = self.switches(cx);
        let switches = !models.is_empty() || !efforts.is_empty();
        let mut rows: Vec<kit::MenuItem> = Vec::new();
        if dropped.iter().any(|k| k.as_ref() == FOOT_MODEL.0)
            && switches
            && let Some(st) = self.state(cx)
        {
            let name =
                model_said(&st.meters).unwrap_or_else(|| agent_name(&st.meta.agent).to_owned());
            rows.push(
                kit::MenuItem::new("model", MODEL, move |_w, cx| {
                    let _gone = this.update(cx, |this, cx| {
                        this.add_open = false;
                        this.toggle_models(cx);
                    });
                })
                .detail(name),
            );
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

/// The selector of the composer's "+".
pub(crate) const ADD_BUTTON: &str = "thread-attach";

/// The selector of the menu the "+" opens.
pub(crate) const ADD_MENU: &str = "thread-add-menu";

/// What the paperclip is called.
const ADD_LABEL: &str = "Attach";

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

/// The share of the context in use from which the meter shows at all: a thread barely begun
/// has nothing to watch (`.research/design-polish-2026-10-12.md` §4.7).
const RING_FROM: f64 = 10.0;

/// The thread's fact that names the branch it works on.
const BRANCH_FACT: &str = "branch";

/// What the checkout's chip is, to a screen reader, before where.
const WORKS_IN: &str = "Works in";

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
