//! Answering an agent's yes or no away from its thread: "Allow" and "Deny" on its row under
//! the navigator's *Needs you*, and the same buttons on the note posted while the app is away
//! (`attention`).
//!
//! Every request an agent waits on shows in its worker's thread table, which this client keeps
//! for every worker it links to; the worker holds a yes or no that nobody follows for the
//! clients that keep the table, for a bounded time. An answer goes through the worker's thread
//! hub as the thread's own tray sends it (`Intent::Answer`), once. Where the person has the
//! agent's terminal in front of them (the focused tile while the app is frontmost, showing the
//! TUI rather than the thread) the request goes back to the TUI at once (`Intent::Release`):
//! the agent's own dialog shows where they look, and nothing waits on a button they will not
//! press.
//!
//! A note's button can come before the request it answers is here: the app was launched by
//! the tap, or its link to the worker is new and the worker's table has not come yet. The
//! verdict then waits for the request until the table has had time to come ([`SYNCED`] after
//! the link came up), or at most [`HOLD_VERDICT`] for a worker not reached; one that finds no
//! request says so, in a toast with the app in front and in a note of its own while it is away.
//!
//! The handing back follows the workspace's changes, never a frame: it sends messages, which
//! nothing drawing does.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::Context;
use slopty_client::layout::WorkerKey;
use slopty_core::{ClientId, SessionId};
use slopty_proto::thread::wire::{Intent, RequestCard};
use slopty_proto::thread::{AskId, ThreadId};

use crate::workspace::attention::{About, NO_LONGER_WAITING, Route};
use crate::workspace::{WorkspaceEvent, WorkspaceView};

/// How long after a worker's link comes up its thread table, with every request it holds, has
/// arrived: a round trip on a slow link, with room to spare.
pub(in crate::workspace) const SYNCED: Duration = Duration::from_secs(3);

/// How long a note's "Allow" or "Deny" waits for its worker at all: a cold launch dials, and a
/// link can take a few tries to come up.
pub(in crate::workspace) const HOLD_VERDICT: Duration = Duration::from_secs(15);

/// What a note's answer says when its worker was not reached in time.
pub(in crate::workspace) const NOT_REACHED: &str = "Couldn't reach that agent's machine";

/// A note's "Allow" or "Deny" whose request is not here yet.
#[derive(Debug)]
struct Tapped {
    route: Route,
    /// The request's id, as the note carried it.
    ask: AskId,
    /// "Allow", else "Deny".
    allow: bool,
    /// When it was tapped: it waits [`HOLD_VERDICT`] from then.
    at: Instant,
}

/// What the workspace keeps of the requests answered here.
#[derive(Debug, Default)]
pub(in crate::workspace) struct Approvals {
    /// The link each worker came up on, and when: its table is surely here [`SYNCED`] later.
    linked: HashMap<WorkerKey, (ClientId, Instant)>,
    /// Verdicts from notes waiting for their request, oldest first.
    tapped: Vec<Tapped>,
    /// The request each thread was answered or handed back here on, until its worker's table
    /// moves past it: each goes once.
    sent: HashMap<ThreadId, AskId>,
    /// Looks at [`Self::tapped`] again when the next of them is due.
    wake: Option<gpui::Task<()>>,
}

/// Whether a thread's open request is a yes or no that "Allow" and "Deny" answer whole
/// ([`RequestCard::answerable`]).
#[must_use]
pub(in crate::workspace) fn answerable(card: &RequestCard) -> bool {
    card.answerable()
}

impl Approvals {
    /// Whether a note's verdict waits for its request.
    pub(in crate::workspace) const fn taps_waiting(&self) -> bool {
        !self.tapped.is_empty()
    }
}

impl WorkspaceView {
    /// Answer `thread`'s open request `ask` on `worker` with its plain allow or deny, through
    /// the worker's thread hub as the thread's own tray would; `false` when it does not wait on
    /// that request any more, or it was answered here already.
    pub fn answer_thread(
        &mut self,
        worker: WorkerKey,
        thread: ThreadId,
        ask: &AskId,
        allow: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let open = self.thread_request(thread).filter(|a| a.id == *ask);
        let choice = open
            .filter(|a| answerable(a))
            .and_then(|a| slopty_proto::thread::once(&a.options, allow));
        let Some(choice) = choice.map(|c| c.id.clone()) else {
            tracing::debug!(%thread, ask = ask.0, "a request no longer open here");
            return false;
        };
        if self.approvals.sent.get(&thread) == Some(ask) {
            return false;
        }
        tracing::info!(%thread, ask = ask.0, allow, "request answered");
        self.approvals.sent.insert(thread, ask.clone());
        let intent = Intent::Answer { ask: ask.clone(), choice, message: None };
        let hub = self.thread_hub(worker, cx);
        let _id = hub.update(cx, |hub, cx| hub.intent(thread, intent, cx));
        cx.notify();
        true
    }

    /// The request this client answered on `thread` that its worker's table still shows open.
    #[must_use]
    pub(in crate::workspace) fn thread_answered_here(&self, thread: ThreadId) -> Option<&AskId> {
        let open = self.thread_request(thread)?;
        self.approvals.sent.get(&thread).filter(|ask| **ask == open.id)
    }

    /// A note's "Allow" (`allow`) or "Deny" for `route`'s request `ask`: answered now when the
    /// request is open here, else once it arrives, or said to have found none once its
    /// worker's table has come or the worker could not be reached.
    pub(in crate::workspace) fn verdict_tapped(
        &mut self,
        route: Route,
        ask: AskId,
        allow: bool,
        cx: &mut Context<Self>,
    ) {
        let at = cx.background_executor().now();
        self.approvals.tapped.push(Tapped { route, ask, allow, at });
        self.settle_taps(cx);
    }

    /// Answer each waiting verdict whose request is here now; let go of one whose worker's
    /// table came without it, or whose time ran out, saying so. Looks again when the next is
    /// due.
    pub(in crate::workspace) fn settle_taps(&mut self, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        let mut due: Option<Duration> = None;
        for tap in std::mem::take(&mut self.approvals.tapped) {
            // A terminal's note answers the request of the thread its agent runs.
            let thread = match tap.route.about {
                About::Session(session) => self.session_thread(session),
                About::Thread(thread) => Some(thread),
            };
            if let Some(thread) = thread
                && self.answer_thread(tap.route.worker, thread, &tap.ask, tap.allow, cx)
            {
                continue;
            }
            let answered = thread.is_some_and(|t| self.approvals.sent.get(&t) == Some(&tap.ask));
            let me = self.me(tap.route.worker);
            // How long until the worker's table has surely come, once its link was up.
            let sync = self
                .approvals
                .linked
                .get(&tap.route.worker)
                .filter(|(link, _)| Some(*link) == me)
                .map(|(_, at)| SYNCED.saturating_sub(now.saturating_duration_since(*at)));
            let hold = HOLD_VERDICT.saturating_sub(now.saturating_duration_since(tap.at));
            if answered || sync.is_some_and(|left| left.is_zero()) {
                self.unanswered(tap.route, NO_LONGER_WAITING, cx);
            } else if hold.is_zero() {
                self.unanswered(tap.route, NOT_REACHED, cx);
            } else {
                let next = sync.map_or(hold, |sync| sync.min(hold));
                due = Some(due.map_or(next, |due| due.min(next)));
                self.approvals.tapped.push(tap);
            }
        }
        if self.approvals.tapped.is_empty() {
            cx.emit(WorkspaceEvent::TapsSettled);
        }
        self.approvals.wake = due.map(|due| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(due).await;
                let _gone = this.update(cx, Self::settle_taps);
            })
        });
    }

    /// A note's verdict found no request to answer: a toast with the app in front, else the
    /// app says it in a note of its own.
    fn unanswered(&mut self, route: Route, why: &'static str, cx: &mut Context<Self>) {
        tracing::info!(about = ?route.about, why, "a note's answer found no request");
        if self.app_active {
            self.show_notice(why.to_owned(), cx);
        } else {
            cx.emit(WorkspaceEvent::Unanswered { route, why });
        }
    }

    /// The workspace changed: note when each worker's link came up, and hand the TUI a yes or
    /// no whose terminal the person has in front of them.
    pub(in crate::workspace) fn sync_approvals(&mut self, cx: &mut Context<Self>) {
        let links: HashMap<WorkerKey, ClientId> =
            self.workers.iter().filter_map(|(key, w)| Some((*key, w.link.as_ref()?.me))).collect();
        self.approvals.linked.retain(|key, (me, _)| links.get(key) == Some(me));
        let mut linked = false;
        let now = cx.background_executor().now();
        for (key, me) in links {
            if let std::collections::hash_map::Entry::Vacant(at) = self.approvals.linked.entry(key)
            {
                at.insert((me, now));
                linked = true;
            }
        }
        // A verdict waiting on a worker just linked waits for its table from now.
        if linked && !self.approvals.tapped.is_empty() {
            self.settle_taps(cx);
        }
        let in_front = self
            .focused_session()
            .filter(|session| self.app_active && !self.face_shown(*session))
            .and_then(|session| {
                let worker = self.worker_of_session(session)?;
                let thread = self.session_thread(session)?;
                let ask = self.session_request(session).filter(|a| answerable(a))?.id.clone();
                (self.approvals.sent.get(&thread) != Some(&ask)).then_some((worker, thread, ask))
            });
        if let Some((worker, thread, ask)) = in_front {
            tracing::info!(%thread, ask = ask.0, "the terminal is in front: its own dialog asks");
            self.approvals.sent.insert(thread, ask.clone());
            let hub = self.thread_hub(worker, cx);
            let _id = hub.update(cx, |hub, cx| hub.intent(thread, Intent::Release { ask }, cx));
            cx.notify();
        }
    }
}

/// What a waiting row's "Deny" and "Allow" answer: a thread's open request, on its worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::workspace) struct Answer {
    worker: WorkerKey,
    thread: ThreadId,
    ask: AskId,
}

/// The button that allows a waiting agent's call.
pub(in crate::workspace) const ALLOW: &str = "Allow";

/// The button that refuses it.
pub(in crate::workspace) const DENY: &str = "Deny";

impl WorkspaceView {
    /// What "Deny" and "Allow" would answer for `session`'s agent: its thread's open request,
    /// while it is a plain yes or no not already answered here.
    pub(in crate::workspace) fn session_answer(&self, session: SessionId) -> Option<Answer> {
        self.thread_answer(self.worker_of_session(session)?, self.session_thread(session)?)
    }

    /// What "Deny" and "Allow" would answer for `thread` on `worker`: its open request, while
    /// it is a plain yes or no not already answered here.
    pub(in crate::workspace) fn thread_answer(
        &self,
        worker: WorkerKey,
        thread: ThreadId,
    ) -> Option<Answer> {
        let asks = self.thread_request(thread)?;
        (answerable(asks) && self.thread_answered_here(thread) != Some(&asks.id)).then(|| Answer {
            worker,
            thread,
            ask: asks.id.clone(),
        })
    }

    /// Allow (`allow`) or deny what a row's buttons answer.
    pub(in crate::workspace) fn answer_row(
        &mut self,
        answer: &Answer,
        allow: bool,
        cx: &mut Context<Self>,
    ) {
        self.answer_thread(answer.worker, answer.thread, &answer.ask, allow, cx);
    }

    /// "Deny" and "Allow" as quiet text buttons the height of a row's second line: the answer
    /// goes where the agent is, without going to it.
    pub(in crate::workspace) fn approval_buttons(
        &self,
        answer: &Answer,
        cx: &crate::draw::Draw<'_, Self>,
    ) -> gpui::Div {
        use gpui::accesskit::Role;
        use gpui::prelude::FluentBuilder as _;
        use gpui::{
            ElementId, InteractiveElement as _, ParentElement as _,
            StatefulInteractiveElement as _, Styled as _, div, px,
        };

        use crate::colors::hsla;

        let theme = &self.theme;
        let s = &theme.surfaces;
        let (_, second_h) = super::navigator::line_heights(theme);
        let button = |id: String, label: &'static str, ink| {
            let selector = id.clone();
            let el = div()
                .id(ElementId::Name(id.into()))
                .debug_selector(move || selector)
                .role(Role::Button)
                .aria_label(label)
                .flex_none()
                .h(px(second_h))
                .px(px(theme.spacing.xs))
                .flex()
                .items_center()
                .rounded(px(theme.radii.xs))
                .cursor_pointer()
                .text_color(hsla(ink))
                .map(crate::kit::eased)
                .hover(move |el| el.bg(hsla(s.hover)))
                .child(label);
            crate::a11y::tab_stop(el, s.focus)
        };
        let of = answer.thread.to_string();
        let answered = |allow: bool| {
            let answer = answer.clone();
            cx.listener(move |this: &mut Self, _ev: &gpui::ClickEvent, _w, cx| {
                cx.stop_propagation();
                this.answer_row(&answer, allow, cx);
            })
        };
        let deny =
            button(format!("nav-deny-{of}"), DENY, s.text_secondary).on_click(answered(false));
        let allow = button(format!("nav-allow-{of}"), ALLOW, s.accent).on_click(answered(true));
        div().flex_none().flex().items_center().gap(px(theme.spacing.xxs)).child(deny).child(allow)
    }
}
