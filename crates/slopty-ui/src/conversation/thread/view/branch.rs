//! "Branch from here": one way to go on from a point of the thread in a new thread, under a
//! message of the person's.
//!
//! Three doors had led there, each its own control: a fork on a settled turn's fold, "Edit from
//! here" on a message, and "Continue in…" in the model chip's menu. They are one choice with
//! four settings, so they are one panel:
//! - **As**: a thread of its own, or an aside: a question beside the work, asked of a fork of the
//!   whole thread in a sheet over it (`super::aside`), where the agent forks.
//! - **Agent**: this thread's own, or another the worker can start.
//! - **From**: this message (the new thread starts just before it, the message waiting in its
//!   composer) or the end (everything so far).
//! - **Files**: keep them as they are, or put them back as they were before this message.
//!
//! What each setting asks of the worker is the agent's own door, read from its caps
//! ([`branch_intent`]): an edit from a turn ([`Cap::REWIND`]), a fork ([`Cap::FORK`]), or a
//! fresh thread ([`Cap::CONTINUE`]), the only door to another agent. A setting the agent has no
//! door for is not offered.
//!
//! A fresh thread carries nothing over itself. Its composer opens on a short pointer back
//! ([`pointer()`]): the old thread's id, folder and branch, and the command that reads it. The
//! new agent reads what it needs with its own tools, and the person sends, changes or clears
//! the pointer first; an account written by rule would only guess at what matters.

use gpui::accesskit::{Role, Toggled};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Div, ElementId, InteractiveElement as _,
    IntoElement as _, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, div,
};
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{AgentId, Cap, ItemBody, ItemId, ThreadMeta, ThreadState, TurnId};

use super::{ThreadView, agent_label, message_group};
use crate::colors::hsla;
use crate::icons::IconName;
use crate::kit::{self, ButtonKind};

/// The panel open under a message: its settings.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Branching {
    /// The message it opened under.
    pub item: ItemId,
    /// Its turn.
    pub turn: TurnId,
    /// The agent the new thread runs.
    pub agent: AgentId,
    /// From the end, rather than from this message.
    pub from_end: bool,
    /// Put the files back as they were before this message.
    pub revert: bool,
    /// Ask aside, rather than branch a thread of its own.
    pub aside: bool,
}

/// The turn before `turn`, which a fork from just before `turn`'s message shares through.
fn turn_before(state: &ThreadState, turn: TurnId) -> Option<TurnId> {
    let at = state.turns.iter().position(|t| t.id == turn)?;
    at.checked_sub(1).and_then(|before| state.turns.get(before)).map(|t| t.id)
}

/// Whether the new thread can start from just before the message of `turn` on this agent: by
/// an edit from the turn, or a fork through the turn before it.
fn from_message(meta: &ThreadMeta, state: &ThreadState, turn: TurnId) -> bool {
    meta.can(Cap::REWIND) || (meta.can(Cap::FORK) && turn_before(state, turn).is_some())
}

/// What the panel's settings ask of the worker, by the agent's own doors; `None` for settings
/// it has no door for.
pub(super) fn branch_intent(state: &ThreadState, b: &Branching) -> Option<Intent> {
    let meta = &state.meta;
    if b.agent != meta.agent {
        return meta.can(Cap::CONTINUE).then(|| Intent::Continue { agent: b.agent.clone() });
    }
    if !b.from_end {
        if meta.can(Cap::REWIND) {
            return Some(Intent::Rewind { turn: b.turn, files: b.revert });
        }
        let before = turn_before(state, b.turn)?;
        return meta.can(Cap::FORK).then_some(Intent::Fork { after: Some(before) });
    }
    if meta.can(Cap::FORK) {
        Some(Intent::Fork { after: None })
    } else {
        meta.can(Cap::CONTINUE).then(|| Intent::Continue { agent: b.agent.clone() })
    }
}

/// What the new thread's composer opens with: the message it starts before, or for a fresh
/// thread a pointer back to this one.
fn branch_seed(state: &ThreadState, b: &Branching, intent: &Intent) -> Option<String> {
    match intent {
        Intent::Continue { .. } => Some(pointer(&state.meta)),
        Intent::Rewind { .. } | Intent::Fork { after: Some(_) } => message_of(state, b.turn),
        _ => None,
    }
}

/// The words the person sent to start `turn`: its input, else its first message of theirs.
pub(super) fn message_of(state: &ThreadState, turn: TurnId) -> Option<String> {
    let input = state.turns.iter().find(|t| t.id == turn)?.input.as_ref();
    let mine = |body: &ItemBody| match body {
        ItemBody::User(message) => Some(message.text.text.clone()),
        _ => None,
    };
    let items = || state.items.iter().filter(|i| i.turn == turn);
    input
        .and_then(|input| items().find(|i| i.id == *input).and_then(|i| mine(&i.body)))
        .or_else(|| items().find_map(|i| mine(&i.body)))
}

/// A fresh thread's first words: where the thread it goes on from is, and how to read it.
fn pointer(meta: &ThreadMeta) -> String {
    let id = meta.id;
    let on = meta.facts.get("branch").map(|b| format!(" on branch {b}")).unwrap_or_default();
    format!(
        "This goes on from thread {id}, in {}{on}. Read it with `slopty agent read --thread \
         {id}` (add `--activity` for its tool calls) before going on.",
        meta.cwd
    )
}

impl ThreadView {
    /// The agents a new thread can run: this one's own first, then the others the worker can
    /// start where the agent can carry the thread over ([`Cap::CONTINUE`]).
    fn branch_agents(&self, cx: &App) -> Vec<AgentId> {
        let Some(meta) = self.state(cx).map(|st| &st.meta) else { return Vec::new() };
        let own = &meta.agent;
        let others = self
            .hub
            .read(cx)
            .agents()
            .iter()
            .filter(|a| *a != own && meta.can(Cap::CONTINUE))
            .cloned();
        std::iter::once(own.clone()).chain(others).collect()
    }

    /// Whether the thread can branch at all.
    pub(super) fn branches(&self, cx: &App) -> bool {
        self.state(cx).is_some_and(|st| {
            [Cap::FORK, Cap::REWIND, Cap::CONTINUE].iter().any(|c| st.meta.can(c))
        })
    }

    /// Open the panel under the message `item` of `turn`, or shut it when it is open there.
    pub(super) fn toggle_branch(&mut self, item: &ItemId, turn: TurnId, cx: &mut Context<Self>) {
        if self.branching.as_ref().is_some_and(|b| b.item == *item) {
            self.branching = None;
        } else if let Some(state) = self.state(cx) {
            let from_end = !from_message(&state.meta, state, turn);
            self.branching = Some(Branching {
                item: item.clone(),
                turn,
                agent: state.meta.agent.clone(),
                from_end,
                revert: false,
                aside: false,
            });
        }
        self.rebuild(cx);
    }

    fn set_branch(&mut self, change: impl FnOnce(&mut Branching), cx: &mut Context<Self>) {
        if let Some(branching) = &mut self.branching {
            change(branching);
            self.rebuild(cx);
        }
    }

    /// Ask for the new thread the panel says, and shut it: an aside asks the draft aside.
    fn branch(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if self.branching.as_ref().is_some_and(|b| b.aside) {
            self.branching = None;
            self.ask_aside(window, cx);
            self.rebuild(cx);
            return;
        }
        let asked = self.branching.as_ref().and_then(|b| {
            let state = self.state(cx)?;
            let intent = branch_intent(state, b)?;
            Some((branch_seed(state, b, &intent), intent))
        });
        if let Some((seed, intent)) = asked.filter(|_| !self.working(cx)) {
            self.branching = None;
            self.start(intent, seed, cx);
            self.rebuild(cx);
        }
    }

    /// Send `intent`, which starts a thread whose composer opens with `seed`.
    pub(super) fn start(&self, intent: Intent, seed: Option<String>, cx: &mut Context<Self>) {
        let thread = self.thread;
        let _id = self.hub.update(cx, |hub, cx| match seed {
            Some(seed) => hub.intent_seeded(thread, intent, seed, cx),
            None => hub.intent(thread, intent, cx),
        });
    }

    /// The branch mark under a message of the person's, while the pointer is on it (always
    /// under a finger), where the thread can branch.
    pub(super) fn branch_button(
        &self,
        id: &ItemId,
        turn: TurnId,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if !self.branches(cx) {
            return None;
        }
        let theme = &self.theme;
        let s = theme.surfaces;
        let touch = theme.density == slopty_theme::Density::TOUCH;
        let open = self.branching.as_ref().is_some_and(|b| b.item == *id);
        let item = id.clone();
        let selector = format!("branch-{}", id.0);
        let hint_theme = std::rc::Rc::new(theme.clone());
        Some(
            crate::a11y::tab_stop(
                div()
                    .id(ElementId::Name(selector.clone().into()))
                    .debug_selector(move || selector)
                    .role(Role::Button)
                    .aria_label("Branch from here")
                    .aria_expanded(open)
                    .flex_none()
                    .size(self.z(theme.typography.icon_large()))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(self.z(theme.radii.xs))
                    .cursor_pointer()
                    .map(kit::eased)
                    .when(open, |el| el.bg(hsla(s.hover)))
                    .hover(move |el| el.bg(hsla(s.hover)))
                    .active(move |el| el.bg(hsla(s.pressed)))
                    .when(!touch && !open, |el| {
                        el.invisible().group_hover(message_group(id), gpui::Styled::visible)
                    })
                    .child(self.icon(IconName::GitBranch, s.text_muted))
                    .map(kit::hint_timing)
                    .tooltip(move |_window, cx| {
                        let theme = std::rc::Rc::clone(&hint_theme);
                        cx.new(|_| kit::Hint::new("Branch from here", "", theme)).into()
                    }),
                s.focus,
            )
            .on_click(cx.listener(move |this, _ev, _w, cx| this.toggle_branch(&item, turn, cx)))
            .into_any_element(),
        )
    }

    /// One choice of a setting: a quiet pill, on the selected wash while chosen.
    fn branch_choice(
        &self,
        id: String,
        label: SharedString,
        on: bool,
        mark: Option<AnyElement>,
    ) -> gpui::Stateful<Div> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let selector = id.clone();
        crate::a11y::tab_stop(
            div()
                .id(ElementId::Name(id.into()))
                .debug_selector(move || selector)
                .role(Role::RadioButton)
                .aria_label(label.clone())
                .aria_toggled(if on { Toggled::True } else { Toggled::False })
                .flex_none()
                .flex()
                .items_center()
                .gap(self.z(theme.spacing.xxs))
                .h(self.z(theme.density.control))
                .px(self.z(theme.spacing.sm))
                .rounded(self.z(theme.radii.sm))
                .cursor_pointer()
                .text_color(hsla(if on { s.text } else { s.text_secondary }))
                .map(|el| if on { kit::selected(el, theme, true) } else { el })
                .when(!on, |el| el.hover(move |el| el.bg(hsla(s.hover))))
                .children(mark)
                .child(label),
            s.focus,
        )
    }

    /// A setting's row: its quiet name, then its choices.
    fn branch_setting(&self, name: &'static str, choices: Vec<AnyElement>) -> Div {
        let theme = &self.theme;
        div()
            .w_full()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(self.z(theme.spacing.xxs))
            .child(
                div()
                    .flex_none()
                    .w(self.z(theme.spacing.xxxl))
                    .text_color(hsla(theme.surfaces.text_muted))
                    .child(name),
            )
            .children(choices)
    }

    /// The panel under the message it opened at: agent, from where, the files, and Branch.
    pub(super) fn branch_panel(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let b = self.branching.as_ref()?;
        let state = self.state(cx)?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let own = b.agent == state.meta.agent;
        let aside = b.aside;
        let as_row = (own && self.can_aside(cx)).then(|| {
            let choice = |id: &str, label: &'static str, on: bool| {
                self.branch_choice(id.to_owned(), label.into(), b.aside == on, None)
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.set_branch(|b| b.aside = on, cx);
                    }))
                    .into_any_element()
            };
            self.branch_setting(
                "As",
                vec![
                    choice("branch-as-thread", "A thread", false),
                    choice("branch-as-aside", "An aside", true),
                ],
            )
        });
        let agents = self.branch_agents(cx);
        let agent_row = (!aside && agents.len() > 1).then(|| {
            let choices = agents
                .iter()
                .enumerate()
                .map(|(ix, agent)| {
                    let pick = agent.clone();
                    self.branch_choice(
                        format!("branch-agent-{ix}"),
                        SharedString::from(agent_label(agent)),
                        *agent == b.agent,
                        None,
                    )
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        let pick = pick.clone();
                        this.set_branch(
                            move |b| {
                                b.from_end = b.from_end || pick != b.agent;
                                b.agent = pick;
                            },
                            cx,
                        );
                    }))
                    .into_any_element()
                })
                .collect();
            self.branch_setting("Agent", choices)
        });
        // Another agent takes an account of the whole thread: there is no "before this
        // message" on it.
        let message_ok = !aside && own && from_message(&state.meta, state, b.turn);
        let from_row = message_ok.then(|| {
            let choice = |id: &str, label: &'static str, end: bool| {
                self.branch_choice(id.to_owned(), label.into(), b.from_end == end, None)
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.set_branch(|b| b.from_end = end, cx);
                    }))
                    .into_any_element()
            };
            self.branch_setting(
                "From",
                vec![
                    choice("branch-from-message", "This message", false),
                    choice("branch-from-end", "The end", true),
                ],
            )
        });
        let files_row = (!aside && own && !b.from_end && state.meta.can(Cap::REWIND)).then(|| {
            let choice = |id: &str, label: &'static str, revert: bool| {
                self.branch_choice(id.to_owned(), label.into(), b.revert == revert, None)
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.set_branch(|b| b.revert = revert, cx);
                    }))
                    .into_any_element()
            };
            self.branch_setting(
                "Files",
                vec![
                    choice("branch-keep", "Keep as they are", false),
                    choice("branch-revert", "Put back", true),
                ],
            )
        });
        let busy = self.working(cx);
        let ready = aside || (branch_intent(state, b).is_some() && !busy);
        let what = match (own, b.from_end) {
            _ if aside => {
                "Your draft, asked of a copy of this thread in a sheet over it; gone when it \
                 closes unless you keep it"
                    .to_owned()
            }
            (false, _) => {
                format!("{} starts with an account of this thread", agent_label(&b.agent))
            }
            (true, true) => "A new thread with everything so far".to_owned(),
            (true, false) => {
                "A new thread from just before this message, which waits to be edited".to_owned()
            }
        };
        let what = if busy && !aside { "Branches once the turn ends".to_owned() } else { what };
        let go = if aside { "Ask aside" } else { "Branch" };
        Some(
            kit::card(theme)
                .id("branch-panel")
                .debug_selector(|| "branch-panel".to_owned())
                .role(Role::Group)
                .aria_label("Branch from here")
                .w_full()
                .max_w(gpui::relative(super::BUBBLE))
                .mt(self.z(theme.spacing.xs))
                .p(self.z(theme.spacing.sm))
                .flex()
                .flex_col()
                .gap(self.z(theme.spacing.xs))
                .text_size(self.z(theme.typography.small()))
                .children(as_row)
                .children(agent_row)
                .children(from_row)
                .children(files_row)
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .gap(self.z(theme.spacing.xs))
                        .pt(self.z(theme.spacing.xxs))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(hsla(s.text_muted))
                                .child(SharedString::from(what)),
                        )
                        .child(self.button("branch-cancel", "Cancel", ButtonKind::Ghost).on_click(
                            cx.listener(|this, _ev, _w, cx| {
                                this.branching = None;
                                this.rebuild(cx);
                            }),
                        ))
                        .child(
                            self.button("branch-go", go, ButtonKind::Primary)
                                .when(!ready, |el| el.opacity(slopty_theme::alpha::PRESSED))
                                .on_click(
                                    cx.listener(|this, _ev, window, cx| this.branch(window, cx)),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::thread::wire::Intent;
    use slopty_proto::thread::{AgentId, Cap, ItemId, TurnId};

    use super::{Branching, branch_intent};
    use crate::conversation::thread::fixtures;

    fn turn(id: u32) -> slopty_proto::thread::Turn {
        slopty_proto::thread::Turn {
            id: TurnId(id),
            input: None,
            state: slopty_proto::thread::TurnState::Complete,
            started_ms: slopty_core::WallMs::ZERO,
            ended_ms: None,
            usage: slopty_proto::thread::Usage::default(),
            models: Vec::new(),
            changed: slopty_proto::thread::Changed::default(),
            before: None,
            after: None,
        }
    }

    /// Each setting asks the agent's own door, and a setting it has no door for asks nothing.
    #[test]
    fn a_branch_asks_the_agent_s_own_door() {
        let mut state = fixtures::empty();
        state.turns = vec![turn(1), turn(2)];
        let own = state.meta.agent.clone();
        let codex = AgentId::named(AgentId::CODEX);
        let b = |agent: &AgentId, from_end, revert| Branching {
            item: ItemId("u".to_owned()),
            turn: TurnId(2),
            agent: agent.clone(),
            from_end,
            revert,
            aside: false,
        };
        state.meta.caps = vec![Cap::named(Cap::REWIND), Cap::named(Cap::CONTINUE)];
        assert_eq!(
            branch_intent(&state, &b(&own, false, true)),
            Some(Intent::Rewind { turn: TurnId(2), files: true })
        );
        assert_eq!(
            branch_intent(&state, &b(&codex, true, false)),
            Some(Intent::Continue { agent: codex.clone() })
        );
        assert_eq!(
            branch_intent(&state, &b(&own, true, false)),
            Some(Intent::Continue { agent: own.clone() }),
            "with no fork, the end is a fresh thread with the account"
        );
        state.meta.caps = vec![Cap::named(Cap::FORK)];
        assert_eq!(
            branch_intent(&state, &b(&own, false, false)),
            Some(Intent::Fork { after: Some(TurnId(1)) }),
            "before this message is through the turn before it"
        );
        assert_eq!(
            branch_intent(&state, &b(&own, true, false)),
            Some(Intent::Fork { after: None })
        );
        assert_eq!(branch_intent(&state, &b(&codex, true, false)), None, "no door to Codex");
    }
}
