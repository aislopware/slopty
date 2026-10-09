//! The approval cards: what an agent out of sight waits on, stacked at the top right of the
//! panes, under the title bar (`MonoCode`'s `ApprovalToasts`, audit row 4).
//!
//! A card stands for one request an agent waits on while its tile is not on show. An approval
//! says which agent and which thread, "Approval" in the warn tone, up to three lines of what it
//! asks, then "Deny" and "Allow" across the card's foot, which answer it where it is
//! (`approvals`). A question, a plan or a form is answered in its thread, so its card only opens
//! the thread. The newest is on top; at most [`CARDS`] are up, the rest wait under *Needs you*.
//!
//! A card goes once its request is answered, here or anywhere, once its tile comes on show, or
//! when its close puts it away (that request's card does not come back). A project muted in the
//! navigator raises no card. A phone has no room for them: its notes and *Needs you* say it.
//! A card rises into place over [`Pace::Toast`] as it fades in; under Reduce Motion it is there
//! at once.

use std::collections::HashSet;

use gpui::accesskit::Role;
use gpui::{
    InteractiveElement as _, IntoElement as _, MouseButton, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_proto::thread::wire::RequestCard;
use slopty_proto::thread::{AskId, ThreadId};

use super::WorkspaceView;
use super::approvals::{ALLOW, Answer, DENY};
use super::titlebar::titlebar_height;
use crate::colors::hsla;
use crate::draw::Draw;
use crate::icons::{Drawn, IconSize};
use crate::kit::{self, ButtonKind, Pace};

/// A card's width, in points: `MonoCode`'s, room for three lines of a command.
pub(super) const CARD_W: f32 = 360.0;

/// The most cards up at once.
pub(super) const CARDS: usize = 3;

/// The most lines of a request a card shows.
const REQUEST_LINES: usize = 3;

/// The word an approval's card leads with.
pub(super) const APPROVAL: &str = "Approval";

/// The word a question's card leads with.
pub(super) const QUESTION: &str = "Question";

/// What a question's card offers: its thread.
pub(super) const OPEN_THREAD: &str = "Open thread";

/// What a card's close says.
const PUT_AWAY: &str = "Put away";

/// One card: the request, whose it is, and what its foot answers.
#[derive(Clone, Debug)]
pub(super) struct Card {
    pub worker: WorkerKey,
    pub thread: ThreadId,
    pub request: RequestCard,
    /// The yes or no "Allow" and "Deny" answer; `None` for a question, which opens its thread.
    pub answer: Option<Answer>,
}

impl WorkspaceView {
    /// The cards up now, the newest first, at most [`CARDS`]: every request an agent waits on
    /// whose tile is not on show, not put away and not in a muted project.
    pub(super) fn approval_cards(&self) -> Vec<Card> {
        if self.phone {
            return Vec::new();
        }
        let mut threads: Vec<(WorkerKey, ThreadId)> = self
            .needs_you()
            .into_iter()
            .filter_map(|w| Some((w.worker, self.session_thread(w.session)?)))
            .collect();
        threads.extend(
            self.thread_stands()
                .filter(|(_, stand)| stand.rung == slopty_proto::thread::attention::Rung::NeedsYou)
                .map(|(thread, stand)| (stand.worker, thread)),
        );
        let mut seen = HashSet::new();
        threads.retain(|(_, thread)| seen.insert(*thread));
        let muted = self.muted_threads(&threads);
        let mut cards: Vec<Card> = threads
            .into_iter()
            .filter(|(_, thread)| !muted.contains(thread))
            .filter(|(_, thread)| {
                self.tile_of_thread(*thread).is_none_or(|tile| !self.layout.on_show(tile))
            })
            .filter_map(|(worker, thread)| {
                let request = self.thread_request(thread)?.clone();
                if self.approvals.is_put_away(&request.id)
                    || self.thread_answered_here(thread) == Some(&request.id)
                {
                    return None;
                }
                let answer = self.thread_answer(worker, thread);
                Some(Card { worker, thread, request, answer })
            })
            .collect();
        cards.sort_by_key(|c| (std::cmp::Reverse(c.request.opened_ms), c.thread));
        cards.truncate(CARDS);
        cards
    }

    /// The threads of `threads` in a project the navigator mutes.
    fn muted_threads(&self, threads: &[(WorkerKey, ThreadId)]) -> HashSet<ThreadId> {
        let muted = &self.navigator().muted;
        if muted.is_empty() || threads.is_empty() {
            return HashSet::new();
        }
        let projects = self.project_groups();
        let claims = self.claims();
        threads
            .iter()
            .filter(|(worker, thread)| {
                let tiled = self
                    .tile_of_thread(*thread)
                    .and_then(|t: TileRef| projects.group_of(t).map(|g| g.key.clone()));
                let key = tiled.or_else(|| {
                    let facts = self.thread_listing_facts(*worker, *thread);
                    super::grouping::listing_group(&projects, &claims, &facts).map(|g| g.key)
                });
                key.is_some_and(|key| muted.contains(&key))
            })
            .map(|(_, thread)| *thread)
            .collect()
    }

    /// Put away the card of request `ask`: it does not come back.
    fn put_card_away(&mut self, ask: AskId, cx: &mut gpui::Context<Self>) {
        self.approvals.put_away(ask);
        cx.notify();
    }

    /// The cards, stacked at the panes' top right under the title bar; nothing while none is
    /// up.
    pub(super) fn render_approval_cards(
        &self,
        window: &Window,
        cx: &Draw<'_, Self>,
    ) -> Option<gpui::AnyElement> {
        let cards = self.approval_cards();
        if cards.is_empty() {
            return None;
        }
        let theme = &self.theme;
        let spacing = theme.spacing;
        let safe = window.insets().effective();
        let stack = div()
            .id("approval-cards")
            .debug_selector(|| "approval-cards".to_owned())
            .role(Role::Group)
            .aria_label("Waiting on you")
            .absolute()
            .top(px(titlebar_height(theme) + spacing.sm) + safe.top)
            .right(px(spacing.sm) + safe.right)
            .w(px(CARD_W))
            .flex()
            .flex_col()
            .gap(px(spacing.xs))
            .children(cards.into_iter().map(|card| self.render_card(card, cx)));
        Some(
            gpui::deferred(stack)
                .with_priority(crate::palette::Layer::Popover.priority())
                .into_any_element(),
        )
    }

    /// One card: its agent and thread over the request's kind, the request in up to three
    /// lines, then its answers.
    fn render_card(&self, card: Card, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let Card { worker, thread, request, answer } = card;
        let of = thread.to_string();
        let agent = self
            .thread_agent(thread)
            .map(|a| super::projects::agent_label(&slopty_proto::thread::AgentId(a.to_owned())));
        let who = match &agent {
            Some(agent) => format!("{agent} on {}", self.worker_name(worker)),
            None => self.worker_name(worker),
        };
        let title = self.thread_title(thread);
        let word = if answer.is_some() { APPROVAL } else { QUESTION };
        let role = theme.roles().chrome;
        let mark = Drawn::beside(theme, self.thread_mark(thread), role)
            .slot(px(IconSize::beside_slot(theme, role)), hsla(s.text_secondary));
        let ask = request.id.clone();
        let close = kit::close_box(theme, format!("card-close-{of}"), PUT_AWAY)
            .debug_selector({
                let of = of.clone();
                move || format!("card-close-{of}")
            })
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                cx.stop_propagation();
                this.put_card_away(ask.clone(), cx);
            }));
        let head = div()
            .flex()
            .items_center()
            .gap(px(spacing.xs))
            .child(mark)
            .child(
                kit::typed(div(), theme.roles().action)
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(hsla(s.text))
                    .child(SharedString::from(title.clone())),
            )
            .child(kit::meta(div(), theme).flex_none().text_color(hsla(s.warn)).child(word))
            .child(close);
        let body = kit::typed(div(), theme.roles().chrome)
            .debug_selector({
                let of = of.clone();
                move || format!("card-request-{of}")
            })
            .text_color(hsla(s.text_secondary))
            .line_clamp(REQUEST_LINES)
            .child(SharedString::from(request.title.clone()));
        let foot = kit::meta(div(), theme).truncate().child(SharedString::from(who));
        let answers = if let Some(answer) = answer {
            let answered = |allow: bool| {
                let answer = answer.clone();
                cx.listener(move |this: &mut Self, _ev: &gpui::ClickEvent, _w, cx| {
                    cx.stop_propagation();
                    this.answer_row(&answer, allow, cx);
                })
            };
            let deny = kit::button(theme, "card-deny", DENY, ButtonKind::Secondary)
                .debug_selector({
                    let of = of.clone();
                    move || format!("card-deny-{of}")
                })
                .flex_1()
                .on_click(answered(false));
            let allow = kit::button(theme, "card-allow", ALLOW, ButtonKind::Primary)
                .debug_selector({
                    let of = of.clone();
                    move || format!("card-allow-{of}")
                })
                .flex_1()
                .on_click(answered(true));
            div().flex().gap(px(spacing.xs)).child(deny).child(allow)
        } else {
            let open = kit::button(theme, "card-open", OPEN_THREAD, ButtonKind::Secondary)
                .debug_selector({
                    let of = of.clone();
                    move || format!("card-open-{of}")
                })
                .flex_1()
                .on_click(cx.listener(move |this, _ev, _window, cx| {
                    cx.stop_propagation();
                    this.open_thread(worker, thread, cx);
                }));
            div().flex().child(open)
        };
        let label = format!("{word}: {title}, {}", request.title);
        let card = kit::elevate(div(), theme)
            .id(SharedString::from(format!("card-{of}")))
            .debug_selector({
                let of = of.clone();
                move || format!("card-{of}")
            })
            .role(Role::Group)
            .aria_label(SharedString::from(label))
            .occlude()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(spacing.xs))
            .p(px(spacing.md))
            .rounded(px(theme.radii.lg))
            .font_family(theme.typography.ui_family.clone())
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .child(head)
            .child(body)
            .child(foot)
            .child(answers);
        kit::slide_fade(
            card,
            SharedString::from(format!("card-in-{of}")),
            spacing.xs,
            Pace::Toast,
            cx,
        )
    }
}
