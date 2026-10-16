//! Answering an agent's yes or no away from its thread: "Allow" and "Deny" on the note posted
//! while the app is away (`attention`).
//!
//! Every request an agent waits on shows in its worker's thread table, which this client keeps
//! for every worker it links to; the worker holds a yes or no that nobody follows for the
//! clients that keep the table, for a bounded time. An answer goes through the worker's thread
//! hub as the thread's own tray sends it (`Intent::Answer`), once.
//!
//! A note's button can come before the request it answers is here: the app was launched by
//! the tap, or its link to the worker is new and the worker's table has not come yet. The
//! verdict then waits for the request until the table has had time to come ([`SYNCED`] after
//! the link came up), or at most [`HOLD_VERDICT`] for a worker not reached; one that finds no
//! request says so, in a toast with the app in front and in a note of its own while it is away.
//!
//! A note's answer is done when its worker says it took it, not when it leaves: the tap settles
//! ([`WorkspaceEvent::TapsSettled`]) once each answer sent for one is acknowledged, turned down,
//! or out of time. One turned down, or never acknowledged within [`HOLD_VERDICT`], says
//! [`ANSWER_NOT_SENT`], apart from a request that no longer waits, and may be answered again.
//! Through the server, a lost link is tried again within the same time.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::Context;
use slopty_client::WorkerKey;
use slopty_core::ClientId;
use slopty_platform::notify::Pressed;
use slopty_proto::orchestration::{ErrorCode, Outcome, ThreadOf, ThreadView, Verb};
use slopty_proto::thread::attention::Rung;
use slopty_proto::thread::wire::{Intent, RequestCard};
use slopty_proto::thread::{AskId, Delivery, IntentId, ThreadId};

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

/// What a note's reply says when it did not reach the agent.
pub(in crate::workspace) const REPLY_NOT_SENT: &str = "Your reply was not sent";

/// What a note's answer says when it did not reach the agent: turned down, or never taken in
/// time. The request may still wait, unlike [`NO_LONGER_WAITING`].
pub(in crate::workspace) const ANSWER_NOT_SENT: &str = "Your answer was not sent";

/// How long a call through the server waits before it is tried again after the link to the
/// server dropped.
const RETRY_SERVER: Duration = Duration::from_secs(1);

/// A note's "Allow", "Deny" or pick whose request is not here yet.
#[derive(Debug)]
struct Tapped {
    route: Route,
    /// The request's id, as the note carried it.
    ask: AskId,
    /// The button pressed.
    pressed: Pressed,
    /// When it was tapped: it waits [`HOLD_VERDICT`] from then.
    at: Instant,
}

/// A note's answer sent to its worker, waiting for the worker to say it took it.
#[derive(Debug)]
struct Awaited {
    route: Route,
    thread: ThreadId,
    ask: AskId,
    /// The intent that carries it.
    id: IntentId,
    /// When its note was tapped: it is taken within [`HOLD_VERDICT`] of then, or not sent.
    at: Instant,
}

/// What the workspace keeps of the requests answered here.
#[derive(Debug, Default)]
pub(in crate::workspace) struct Approvals {
    /// The link each worker came up on, and when: its table is surely here [`SYNCED`] later.
    linked: HashMap<WorkerKey, (ClientId, Instant)>,
    /// Verdicts from notes waiting for their request, oldest first.
    tapped: Vec<Tapped>,
    /// Verdicts from notes sent to their worker, not acknowledged yet.
    awaiting: Vec<Awaited>,
    /// The request each thread was answered or handed back here on, until its worker's table
    /// moves past it: each goes once.
    sent: HashMap<ThreadId, AskId>,
    /// Looks at [`Self::tapped`] again when the next of them is due.
    wake: Option<gpui::Task<()>>,
    /// Verdicts on their way through the server, not answered yet.
    through_server: usize,
}

/// Whether a thread's open request is a yes or no that "Allow" and "Deny" answer whole
/// ([`RequestCard::answerable`]).
#[must_use]
pub(in crate::workspace) fn answerable(card: &RequestCard) -> bool {
    card.answerable()
}

/// What a note's `pressed` answers `card` with: Allow and Deny its plain allow or deny while
/// it is a yes or no they answer whole, a pick its own choice while it offers that pick. None
/// for a request only the person at its card answers ([`RequestCard::needs_presence`]): a
/// note's button is not the card.
#[must_use]
pub(in crate::workspace) fn tapped_choice(card: &RequestCard, pressed: Pressed) -> Option<String> {
    if card.needs_presence {
        return None;
    }
    match pressed {
        Pressed::Allow | Pressed::Deny => {
            answerable(card).then(|| pressed.choice(&card.options, &card.buttons))?
        }
        Pressed::Pick(_) => pressed.choice(&card.options, &card.buttons),
    }
}

impl Approvals {
    /// Whether a note's verdict waits for its request.
    pub(in crate::workspace) const fn taps_waiting(&self) -> bool {
        !self.tapped.is_empty() || !self.awaiting.is_empty() || self.through_server > 0
    }
}

impl WorkspaceView {
    /// Answer `thread`'s open request `ask` on `worker` as `pressed` says: Allow and Deny with
    /// its plain allow or deny while it is a yes or no they answer whole, a pick with its own
    /// choice while it offers that pick: the intent that carries it, or `None` when it does
    /// not, or it was answered here already.
    fn answer_pressed(
        &mut self,
        worker: WorkerKey,
        thread: ThreadId,
        ask: &AskId,
        pressed: &Pressed,
        cx: &mut Context<Self>,
    ) -> Option<IntentId> {
        let open = self.thread_request(thread).filter(|a| a.id == *ask);
        let choice = open.and_then(|a| tapped_choice(a, *pressed));
        let Some(choice) = choice else {
            tracing::debug!(%thread, ask = ask.0, "a request no longer open here");
            return None;
        };
        if self.approvals.sent.get(&thread) == Some(ask) {
            return None;
        }
        tracing::info!(%thread, ask = ask.0, ?pressed, "request answered");
        self.approvals.sent.insert(thread, ask.clone());
        let intent = Intent::Answer { ask: ask.clone(), choice, message: None };
        let hub = self.thread_hub(worker, cx);
        let id = hub.update(cx, |hub, cx| hub.intent(thread, intent, cx));
        cx.notify();
        Some(id)
    }

    /// The request this client answered on `thread` that its worker's table still shows open.
    #[must_use]
    pub(in crate::workspace) fn thread_answered_here(&self, thread: ThreadId) -> Option<&AskId> {
        let open = self.thread_request(thread)?;
        self.approvals.sent.get(&thread).filter(|ask| **ask == open.id)
    }

    /// A note's "Allow", "Deny" or pick, `pressed`, for `route`'s request `ask`: answered now
    /// when the request is open here, else once it arrives, or said to have found none once
    /// its worker's table has come or the worker could not be reached.
    pub(in crate::workspace) fn verdict_tapped(
        &mut self,
        route: Route,
        ask: AskId,
        pressed: Pressed,
        cx: &mut Context<Self>,
    ) {
        let at = cx.background_executor().now();
        self.approvals.tapped.push(Tapped { route, ask, pressed, at });
        self.settle_taps(cx);
    }

    /// A note's reply, `text`, to `route`'s agent: sent as its own composer sends a message
    /// where its worker is linked here, else through the server, which finds the thread
    /// wherever it runs. One that cannot go is said as a note's answer that found nothing.
    pub(in crate::workspace) fn reply_tapped(
        &mut self,
        route: Route,
        text: String,
        cx: &mut Context<Self>,
    ) {
        let About::Thread(thread) = route.about else {
            Self::unanswered(route, REPLY_NOT_SENT, cx);
            self.settle_taps(cx);
            return;
        };
        let linked = self.workers.get(&route.worker).is_some_and(|w| w.link.is_some());
        if linked {
            tracing::info!(%thread, "a note's reply sent");
            let hub = self.thread_hub(route.worker, cx);
            // Sent now, as the thread's own composer sends (`ThreadMeta::delivery_now`).
            let delivery = hub
                .read(cx)
                .threads()
                .mirror(thread)
                .and_then(slopty_client::threads::Mirror::state)
                .map_or(Delivery::Steer, |s| s.meta.delivery_now());
            let send = Intent::Send { text, delivery, attachments: Vec::new() };
            let _id = hub.update(cx, |hub, cx| hub.intent(thread, send, cx));
            self.settle_taps(cx);
            return;
        }
        let of = ThreadOf::Thread(thread);
        let Some(caller) = self.server.clone() else {
            Self::unanswered(route, REPLY_NOT_SENT, cx);
            self.settle_taps(cx);
            return;
        };
        tracing::info!(?of, "a note's reply sent through the server");
        self.approvals.through_server = self.approvals.through_server.saturating_add(1);
        cx.spawn(async move |this, cx| {
            let sent = caller.call(Verb::SendMessage { of, text }).await;
            let _gone = this.update(cx, |this, cx| {
                if let Outcome::Error { message, .. } = sent {
                    tracing::info!(message, "a note's reply was refused");
                    Self::unanswered(route, REPLY_NOT_SENT, cx);
                }
                this.approvals.through_server = this.approvals.through_server.saturating_sub(1);
                if !this.approvals.taps_waiting() {
                    cx.emit(WorkspaceEvent::TapsSettled);
                }
            });
        })
        .detach();
    }

    /// Answer each waiting verdict whose request is here now; let go of one whose worker's
    /// table came without it, or whose time ran out, saying so. Looks again when the next is
    /// due.
    pub(in crate::workspace) fn settle_taps(&mut self, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        let mut due: Option<Duration> = None;
        for tap in std::mem::take(&mut self.approvals.tapped) {
            let thread = match tap.route.about {
                About::Session(_) => None,
                About::Thread(thread) => Some(thread),
            };
            // A request only the person at its card answers takes no note's button: said so,
            // and it waits for them at the card.
            if let Some(thread) = thread
                && self.thread_request(thread).is_some_and(|a| a.id == tap.ask && a.needs_presence)
            {
                Self::unanswered(tap.route, slopty_proto::thread::Request::NEEDS_PRESENCE, cx);
                continue;
            }
            if let Some(thread) = thread
                && let Some(id) =
                    self.answer_pressed(tap.route.worker, thread, &tap.ask, &tap.pressed, cx)
            {
                let Tapped { route, ask, at, .. } = tap;
                self.approvals.awaiting.push(Awaited { route, thread, ask, id, at });
                continue;
            }
            // Its worker not linked here yet, while the server's ladder has its thread waiting
            // on the person: answered through the server, which holds a pushed phone's yes or no.
            if let Some(thread) = thread.filter(|t| self.on_servers_ladder(&tap.route, *t)) {
                self.answer_through_server(tap, thread, cx);
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
                Self::unanswered(tap.route, NO_LONGER_WAITING, cx);
            } else if hold.is_zero() {
                Self::unanswered(tap.route, NOT_REACHED, cx);
            } else {
                let next = sync.map_or(hold, |sync| sync.min(hold));
                due = Some(due.map_or(next, |due| due.min(next)));
                self.approvals.tapped.push(tap);
            }
        }
        for awaited in std::mem::take(&mut self.approvals.awaiting) {
            let hold = HOLD_VERDICT.saturating_sub(now.saturating_duration_since(awaited.at));
            match self.answer_heard(&awaited, cx) {
                Some(true) => {}
                Some(false) => self.answer_not_sent(&awaited, cx),
                None if hold.is_zero() => self.answer_not_sent(&awaited, cx),
                None => {
                    due = Some(due.map_or(hold, |due| due.min(hold)));
                    self.approvals.awaiting.push(awaited);
                }
            }
        }
        if !self.approvals.taps_waiting() {
            cx.emit(WorkspaceEvent::TapsSettled);
        }
        self.approvals.wake = due.map(|due| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(due).await;
                let _gone = this.update(cx, Self::settle_taps);
            })
        });
    }

    /// What the worker said of `awaited`'s answer: `Some(true)` once it took it, `Some(false)`
    /// once it turned it down, `None` while it has said nothing.
    fn answer_heard(&mut self, awaited: &Awaited, cx: &mut Context<Self>) -> Option<bool> {
        let hub = self.thread_hub(awaited.route.worker, cx);
        let hub = hub.read(cx);
        if hub.refusals(awaited.thread).any(|r| r.id == awaited.id) {
            return Some(false);
        }
        // Gone from the outbox without a refusal: taken, and the thread shows it.
        let sent = hub.threads().outbox().all().iter().find(|s| s.id == awaited.id);
        match sent.map(|s| (s.outcome.is_some(), s.failed())) {
            None | Some((true, false)) => Some(true),
            Some((true, true)) => Some(false),
            Some((false, _)) => None,
        }
    }

    /// `awaited`'s answer did not reach the agent: said so, and its request may be answered
    /// again.
    fn answer_not_sent(&mut self, awaited: &Awaited, cx: &mut Context<Self>) {
        if self.approvals.sent.get(&awaited.thread) == Some(&awaited.ask) {
            self.approvals.sent.remove(&awaited.thread);
        }
        Self::unanswered(awaited.route, ANSWER_NOT_SENT, cx);
    }

    /// A worker's threads moved: a note's answer waiting for it may have been taken.
    pub(in crate::workspace) fn answers_heard(&mut self, cx: &mut Context<Self>) {
        if !self.approvals.awaiting.is_empty() {
            self.settle_taps(cx);
        }
    }

    /// Whether `thread`, of `route`'s note, is to be answered through the server: its worker is
    /// not linked here, the server's ladder has it waiting on the person, and a server is there
    /// to answer it through.
    fn on_servers_ladder(&self, route: &Route, thread: ThreadId) -> bool {
        if self.server.is_none()
            || self.workers.get(&route.worker).is_some_and(|w| w.link.is_some())
        {
            return false;
        }
        self.thread_stands().any(|(id, stand)| {
            id == thread && stand.worker == route.worker && stand.rung == Rung::NeedsYou
        })
    }

    /// Answer `tap` on `thread` through the server: read the thread's open requests there for
    /// the choice that allows or denies once, or find the request a pick answers, then answer
    /// with it, once. A lost link to the server is tried again until [`HOLD_VERDICT`] from the
    /// tap. A request the server no longer finds is one that no longer waits; a call that
    /// fails is an answer not sent, and the request may be answered again.
    fn answer_through_server(&mut self, tap: Tapped, thread: ThreadId, cx: &Context<Self>) {
        let Some(caller) = self.server.clone() else { return };
        if self.approvals.sent.get(&thread) == Some(&tap.ask) {
            return;
        }
        tracing::info!(%thread, ask = tap.ask.0, pressed = ?tap.pressed, "answered through the server");
        self.approvals.sent.insert(thread, tap.ask.clone());
        self.approvals.through_server = self.approvals.through_server.saturating_add(1);
        let Tapped { route, ask, pressed, at } = tap;
        let deadline = at.checked_add(HOLD_VERDICT).unwrap_or(at);
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let call = async |verb: Verb| loop {
                let out = caller.call(verb.clone()).await;
                let lost = matches!(out, Outcome::Error { code: ErrorCode::ServerUnreachable, .. });
                if !lost || executor.now() >= deadline {
                    break out;
                }
                executor.timer(RETRY_SERVER).await;
            };
            let read = Verb::ReadThread {
                of: ThreadOf::Thread(thread),
                view: ThreadView::Messages,
                after: None,
            };
            let why = match call(read).await {
                Outcome::Thread(read) => {
                    let choice = read
                        .requests
                        .iter()
                        .find(|r| r.ask == ask)
                        .and_then(|r| pressed.choice(&r.choices, &r.picks));
                    match choice {
                        Some(choice) => {
                            let verb = Verb::AnswerRequest {
                                of: ThreadOf::Thread(thread),
                                ask: ask.clone(),
                                choice,
                                message: None,
                            };
                            match call(verb).await {
                                Outcome::Error { .. } => Some(ANSWER_NOT_SENT),
                                _ => None,
                            }
                        }
                        None => Some(NO_LONGER_WAITING),
                    }
                }
                _ => Some(ANSWER_NOT_SENT),
            };
            let _gone = this.update(cx, |this, cx| {
                if let Some(why) = why {
                    if why == ANSWER_NOT_SENT && this.approvals.sent.get(&thread) == Some(&ask) {
                        this.approvals.sent.remove(&thread);
                    }
                    Self::unanswered(route, why, cx);
                }
                this.approvals.through_server = this.approvals.through_server.saturating_sub(1);
                if !this.approvals.taps_waiting() {
                    cx.emit(WorkspaceEvent::TapsSettled);
                }
            });
        })
        .detach();
    }

    /// A note's verdict found no request to answer: the app says it in a note of its own.
    fn unanswered(route: Route, why: &'static str, cx: &mut Context<Self>) {
        tracing::info!(about = ?route.about, why, "a note's answer found no request");
        cx.emit(WorkspaceEvent::Unanswered { route, why });
    }

    /// The workspace changed: note when each worker's link came up.
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
    }
}

impl WorkspaceView {}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::thread::wire::NoteChoice;
    use slopty_proto::thread::{Choice, Effect, Request};

    use super::*;

    /// A yes or no with a note's pick, needing the person at its card when `present`.
    fn card(present: bool) -> RequestCard {
        let choice = |id: &str, effect| Choice {
            id: id.to_owned(),
            label: id.to_owned(),
            effect,
            scope: None,
            stops: false,
        };
        RequestCard {
            id: AskId("a".to_owned()),
            item: None,
            kind: Request::APPROVAL.to_owned(),
            title: "Run rm -rf build?".to_owned(),
            options: vec![choice("allow", Effect::Allow), choice("deny", Effect::Deny)],
            buttons: vec![NoteChoice { label: "Yes".to_owned(), choice: "allow".to_owned() }],
            opened_ms: WallMs::ZERO,
            needs_presence: present,
        }
    }

    /// A note's Allow, Deny and pick answer a request as it offers them, and none answers one
    /// that only the person at its card answers: a note's button is not the card.
    #[test]
    fn a_note_answers_nothing_that_needs_the_person_at_the_card() {
        let open = card(false);
        assert_eq!(tapped_choice(&open, Pressed::Allow).as_deref(), Some("allow"));
        assert_eq!(tapped_choice(&open, Pressed::Deny).as_deref(), Some("deny"));
        assert_eq!(tapped_choice(&open, Pressed::Pick(0)).as_deref(), Some("allow"));
        let present = card(true);
        for pressed in [Pressed::Allow, Pressed::Deny, Pressed::Pick(0)] {
            assert_eq!(tapped_choice(&present, pressed), None, "{pressed:?}");
        }
    }
}
