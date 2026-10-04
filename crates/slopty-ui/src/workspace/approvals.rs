//! Answering an agent's permission prompt away from its conversation: "Allow" and "Deny" on its
//! row under the navigator's *Needs you*, and the same buttons on the note posted while the app
//! is away (`attention`).
//!
//! The workspace asks every worker it links to for approvals (`ConversationRequest::Approvals`).
//! A worker then holds a yes or no that nobody follows for a bounded time and sends it here as it
//! sends a follower's (`WorkerMsg::Permission`); a question or a plan waits for the conversation
//! face, which shows it whole. An answer goes back once, as the face's does
//! (`ConversationRequest::Answer`). Where the person has that session's terminal in front of
//! them (the focused tile while the app is frontmost, showing the TUI rather than the face) the
//! prompt goes back to the TUI at once (`ConversationRequest::Release`): Claude Code's own dialog
//! shows where they look, and nothing waits on a button they will not press.
//!
//! A note's button can come before the prompt it answers is here: the app was launched by the
//! tap, or its link to the worker is new and the worker has not yet sent what it holds. The
//! verdict then waits for the prompt until the worker has had time to send it ([`SYNCED`]
//! after it was asked), or at most [`HOLD_VERDICT`] for a worker not reached; one that finds no
//! prompt says so, in a toast with the app in front and in a note of its own while it is away.
//!
//! The asking and the handing back follow the workspace's changes, never a frame: they send
//! messages, which nothing drawing does.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui::Context;
use slopty_client::layout::WorkerKey;
use slopty_core::{ClientId, SessionId};
use slopty_proto::ClientMsg;
use slopty_proto::conversation::{
    ConversationRequest, PermissionEvent, PermissionPrompt, ToolDetail, Verdict,
};
use slopty_proto::thread::wire::{Intent, RequestCard};
use slopty_proto::thread::{AskId, Choice, Effect, Request, ThreadId};

use crate::conversation::approval::{PLAN_TOOL, QUESTION_TOOL};
use crate::workspace::attention::{About, NO_LONGER_WAITING, Route};
use crate::workspace::{WorkspaceEvent, WorkspaceView};

/// How long after a worker is asked for approvals every prompt it holds has arrived: a round
/// trip on a slow link, with room to spare.
pub(in crate::workspace) const SYNCED: Duration = Duration::from_secs(3);

/// How long a note's "Allow" or "Deny" waits for its worker at all: a cold launch dials, and a
/// link can take a few tries to come up.
pub(in crate::workspace) const HOLD_VERDICT: Duration = Duration::from_secs(15);

/// What a note's answer says when its worker was not reached in time.
pub(in crate::workspace) const NOT_REACHED: &str = "Couldn't reach that agent's machine";

/// A note's "Allow" or "Deny" whose prompt is not here yet.
#[derive(Debug)]
struct Tapped {
    route: Route,
    /// The prompt's number, or the thread's request's id, as the note carried it.
    ask: String,
    verdict: Verdict,
    /// When it was tapped: it waits [`HOLD_VERDICT`] from then.
    at: Instant,
}

/// What the workspace keeps of the prompts held for it.
#[derive(Debug, Default)]
pub(in crate::workspace) struct Approvals {
    /// The yes-or-no prompts waiting, by session: Claude Code asks one at a time in each.
    held: HashMap<SessionId, PermissionPrompt>,
    /// The link each worker was asked for approvals on, and when: a new link asks again.
    asked: HashMap<WorkerKey, (ClientId, Instant)>,
    /// Prompts answered or handed back here, by session and ask: each goes once, and one the
    /// worker shows again after a resync is not taken up again.
    sent: HashSet<(SessionId, u64)>,
    /// Verdicts from notes waiting for their prompt, oldest first.
    tapped: Vec<Tapped>,
    /// The request each thread was answered here on, until its worker's table moves past it:
    /// each goes once.
    threads_sent: HashMap<ThreadId, AskId>,
    /// Looks at [`Self::tapped`] again when the next of them is due.
    wake: Option<gpui::Task<()>>,
}

/// Whether a thread's open request is a yes or no that "Allow" and "Deny" answer whole: an
/// approval offering a plain allow and a deny.
#[must_use]
pub(in crate::workspace) fn answerable(card: &RequestCard) -> bool {
    card.kind == Request::APPROVAL
        && request_choice(&card.options, true).is_some()
        && request_choice(&card.options, false).is_some()
}

/// The choice of `options` that allows (`allow`) or denies the call once and no more: the
/// plain allow; the plain deny, else the first.
fn request_choice(options: &[Choice], allow: bool) -> Option<&Choice> {
    if allow {
        options.iter().find(|c| c.effect == Effect::Allow && c.scope.is_none())
    } else {
        crate::conversation::thread::view::denying::plain_deny(options)
    }
}

/// Whether `prompt` is a yes or no that "Allow" or "Deny" answers whole.
#[must_use]
pub(in crate::workspace) fn approvable(prompt: &PermissionPrompt) -> bool {
    !matches!(prompt.detail, ToolDetail::Plan { .. } | ToolDetail::Question(_))
        && ![PLAN_TOOL, QUESTION_TOOL].contains(&prompt.tool.as_str())
}

impl Approvals {
    /// A prompt was asked or settled.
    fn heard(&mut self, event: &PermissionEvent) {
        match event {
            PermissionEvent::Asked(prompt) => {
                if approvable(prompt) && !self.sent.contains(&(prompt.session, prompt.ask)) {
                    self.held.insert(prompt.session, (**prompt).clone());
                }
            }
            PermissionEvent::Settled { session, ask, .. } => {
                if self.held.get(session).is_some_and(|p| p.ask == *ask) {
                    self.held.remove(session);
                }
                self.sent.remove(&(*session, *ask));
            }
        }
    }

    /// The prompt of `session` answered or handed back here and not settled yet, if any.
    fn answered(&self, session: SessionId) -> Option<u64> {
        self.sent.iter().find(|(s, _)| *s == session).map(|(_, ask)| *ask)
    }

    /// Take `session`'s prompt `ask` to answer or hand back: once.
    fn take(&mut self, session: SessionId, ask: u64) -> Option<PermissionPrompt> {
        if self.held.get(&session).is_none_or(|p| p.ask != ask) {
            return None;
        }
        self.sent.insert((session, ask));
        self.held.remove(&session)
    }
}

impl WorkspaceView {
    /// A permission prompt was asked or settled on a worker: *Needs you* and the notes follow it,
    /// and a note's verdict that waited for it answers it.
    pub(in crate::workspace) fn approval_event(
        &mut self,
        event: &PermissionEvent,
        cx: &mut Context<Self>,
    ) {
        self.approvals.heard(event);
        if !self.approvals.tapped.is_empty() {
            self.settle_taps(cx);
        }
    }

    /// The prompt of `session` this client answered or handed back that its worker has not yet
    /// said is settled: the agent may still read as waiting on it for a moment.
    #[must_use]
    pub(in crate::workspace) fn answered_here(&self, session: SessionId) -> Option<u64> {
        self.approvals.answered(session)
    }

    /// The yes-or-no prompt `session`'s agent waits on, when it is held for this client.
    #[must_use]
    pub fn approval(&self, session: SessionId) -> Option<&PermissionPrompt> {
        self.approvals.held.get(&session)
    }

    /// Answer `session`'s prompt `ask` with `verdict`, as the conversation face would; `false`
    /// when it is not waiting here any more (answered, handed back, or its time ran out).
    pub fn answer_approval(
        &mut self,
        session: SessionId,
        ask: u64,
        verdict: Verdict,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.approvals.take(session, ask).is_none() {
            tracing::debug!(%session, ask, "an approval no longer held here");
            return false;
        }
        tracing::info!(%session, ask, ?verdict, "approval answered");
        let answer = ConversationRequest::Answer { session, ask, verdict };
        self.send_session(session, ClientMsg::Conversation(answer));
        cx.notify();
        true
    }

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
        let open = self.thread_stand(thread).and_then(|s| s.asks.as_ref()).filter(|a| a.id == *ask);
        let choice = open.filter(|a| answerable(a)).and_then(|a| request_choice(&a.options, allow));
        let Some(choice) = choice.map(|c| c.id.clone()) else {
            tracing::debug!(%thread, ask = ask.0, "a request no longer open here");
            return false;
        };
        if self.approvals.threads_sent.get(&thread) == Some(ask) {
            return false;
        }
        tracing::info!(%thread, ask = ask.0, allow, "request answered");
        self.approvals.threads_sent.insert(thread, ask.clone());
        let intent = Intent::Answer { ask: ask.clone(), choice, message: None };
        let hub = self.thread_hub(worker, cx);
        let _id = hub.update(cx, |hub, cx| hub.intent(thread, intent, cx));
        cx.notify();
        true
    }

    /// The request this client answered on `thread` that its worker's table still shows open.
    #[must_use]
    pub(in crate::workspace) fn thread_answered_here(&self, thread: ThreadId) -> Option<&AskId> {
        let open = self.thread_stand(thread)?.asks.as_ref()?;
        self.approvals.threads_sent.get(&thread).filter(|ask| **ask == open.id)
    }

    /// A note's "Allow" or "Deny" for `route`'s prompt `ask`: answered now when the prompt is
    /// held here, else once it arrives, or said to have found none once its worker has sent
    /// what it holds or could not be reached.
    pub(in crate::workspace) fn verdict_tapped(
        &mut self,
        route: Route,
        ask: String,
        verdict: Verdict,
        cx: &mut Context<Self>,
    ) {
        let at = cx.background_executor().now();
        self.approvals.tapped.push(Tapped { route, ask, verdict, at });
        self.settle_taps(cx);
    }

    /// Answer each waiting verdict whose prompt is here now; let go of one whose worker has
    /// sent what it holds without it, or whose time ran out, saying so. Looks again when the
    /// next is due.
    pub(in crate::workspace) fn settle_taps(&mut self, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        let mut due: Option<Duration> = None;
        for tap in std::mem::take(&mut self.approvals.tapped) {
            let answered = match tap.route.about {
                About::Session(session) => {
                    let Ok(ask) = tap.ask.parse::<u64>() else { continue };
                    if self.approval(session).is_some_and(|p| p.ask == ask) {
                        self.answer_approval(session, ask, tap.verdict, cx);
                        continue;
                    }
                    self.approvals.sent.contains(&(session, ask))
                }
                About::Thread(thread) => {
                    let ask = AskId(tap.ask.clone());
                    let allow = matches!(tap.verdict, Verdict::Allow);
                    if self.answer_thread(tap.route.worker, thread, &ask, allow, cx) {
                        continue;
                    }
                    self.approvals.threads_sent.get(&thread) == Some(&ask)
                }
            };
            let me = self.me(tap.route.worker);
            // How long until the worker has surely sent what it holds, once it was asked.
            let sync = self
                .approvals
                .asked
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

    /// A note's verdict found no prompt to answer: a toast with the app in front, else the app
    /// says it in a note of its own.
    fn unanswered(&mut self, route: Route, why: &'static str, cx: &mut Context<Self>) {
        tracing::info!(about = ?route.about, why, "a note's answer found no prompt");
        if self.app_active {
            self.show_notice(why.to_owned(), cx);
        } else {
            cx.emit(WorkspaceEvent::Unanswered { route, why });
        }
    }

    /// The workspace changed: ask each newly linked worker for approvals, forget what a lost
    /// link held, and hand the TUI a prompt whose terminal the person has in front of them.
    pub(in crate::workspace) fn sync_approvals(&mut self, cx: &mut Context<Self>) {
        let links: HashMap<WorkerKey, ClientId> =
            self.workers.iter().filter_map(|(key, w)| Some((*key, w.link.as_ref()?.me))).collect();
        let lost: Vec<WorkerKey> = self
            .approvals
            .asked
            .iter()
            .filter(|(key, (me, _))| links.get(key) != Some(me))
            .map(|(key, _)| *key)
            .collect();
        for key in lost {
            self.approvals.asked.remove(&key);
            let gone: Vec<SessionId> = self
                .approvals
                .held
                .keys()
                .copied()
                .filter(|s| self.worker_of_session(*s).is_none_or(|w| w == key))
                .collect();
            for session in gone {
                self.approvals.held.remove(&session);
            }
            cx.notify();
        }
        let mut asked = false;
        for (key, me) in links {
            if self.approvals.asked.get(&key).map(|(link, _)| *link) != Some(me) {
                self.send(
                    key,
                    ClientMsg::Conversation(ConversationRequest::Approvals { on: true }),
                );
                let now = cx.background_executor().now();
                self.approvals.asked.insert(key, (me, now));
                asked = true;
            }
        }
        // A verdict waiting on a worker just asked waits for what it holds from now.
        if asked && !self.approvals.tapped.is_empty() {
            self.settle_taps(cx);
        }
        let in_front = self.focused_session().filter(|session| {
            self.app_active && !self.face_shown(*session) && self.approval(*session).is_some()
        });
        if let Some(session) = in_front
            && let Some(ask) = self.approval(session).map(|p| p.ask)
            && self.approvals.take(session, ask).is_some()
        {
            tracing::info!(%session, ask, "the terminal is in front: its own dialog asks");
            let release = ConversationRequest::Release { session, ask };
            self.send_session(session, ClientMsg::Conversation(release));
            cx.notify();
        }
    }
}

/// What a waiting row's "Deny" and "Allow" answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::workspace) enum Answer {
    /// A terminal's permission prompt held for this client: its session and number.
    Prompt(SessionId, u64),
    /// A thread's open request, on its worker.
    Request(WorkerKey, ThreadId, AskId),
}

/// The button that allows a waiting agent's call.
pub(in crate::workspace) const ALLOW: &str = "Allow";

/// The button that refuses it.
pub(in crate::workspace) const DENY: &str = "Deny";

impl WorkspaceView {
    /// What "Deny" and "Allow" would answer for `session`'s agent: its prompt, while one is held
    /// here.
    pub(in crate::workspace) fn session_answer(&self, session: SessionId) -> Option<Answer> {
        self.approval(session).map(|prompt| Answer::Prompt(session, prompt.ask))
    }

    /// What "Deny" and "Allow" would answer for `thread` on `worker`: its open request, while
    /// it is a plain yes or no not already answered here.
    pub(in crate::workspace) fn thread_answer(
        &self,
        worker: WorkerKey,
        thread: ThreadId,
    ) -> Option<Answer> {
        let asks = self.thread_stand(thread)?.asks.as_ref()?;
        (answerable(asks) && self.thread_answered_here(thread) != Some(&asks.id))
            .then(|| Answer::Request(worker, thread, asks.id.clone()))
    }

    /// Allow (`allow`) or deny what a row's buttons answer.
    pub(in crate::workspace) fn answer_row(
        &mut self,
        answer: &Answer,
        allow: bool,
        cx: &mut Context<Self>,
    ) {
        match answer {
            Answer::Prompt(session, ask) => {
                let verdict = if allow {
                    Verdict::Allow
                } else {
                    Verdict::Deny { message: String::new(), interrupt: false }
                };
                self.answer_approval(*session, *ask, verdict, cx);
            }
            Answer::Request(worker, thread, ask) => {
                self.answer_thread(*worker, *thread, ask, allow, cx);
            }
        }
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
            crate::a11y::tab_stop(el, s.accent)
        };
        let of = match answer {
            Answer::Prompt(session, _) => session.to_string(),
            Answer::Request(_, thread, _) => thread.to_string(),
        };
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

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::conversation::{Clipped, Settled};

    use super::*;

    fn prompt(session: SessionId, ask: u64, tool: &str, detail: ToolDetail) -> PermissionPrompt {
        PermissionPrompt {
            session,
            ask,
            tool: tool.to_owned(),
            detail,
            suggestions: Vec::new(),
            mode: None,
            asked_ms: WallMs::ZERO,
            until_ms: WallMs::ZERO,
        }
    }

    fn other() -> ToolDetail {
        ToolDetail::Other { input: Clipped { text: String::new(), lines: 0, chars: 0, full: None } }
    }

    fn bash(session: SessionId, ask: u64) -> PermissionPrompt {
        prompt(session, ask, "Bash", other())
    }

    /// A yes or no is kept until it settles or is taken, once; a plan or a question never is,
    /// and one taken here is not taken up again when the worker shows it again.
    #[test]
    fn a_yes_or_no_is_kept_until_it_settles_or_is_taken_once() {
        let mut approvals = Approvals::default();
        let s = SessionId::new();
        let plan = ToolDetail::Plan {
            plan: Clipped { text: String::new(), lines: 0, chars: 0, full: None },
        };
        approvals.heard(&PermissionEvent::Asked(Box::new(prompt(s, 1, PLAN_TOOL, plan))));
        approvals.heard(&PermissionEvent::Asked(Box::new(prompt(s, 1, QUESTION_TOOL, other()))));
        assert!(approvals.held.is_empty(), "a plan or a question waits for the face");
        approvals.heard(&PermissionEvent::Asked(Box::new(bash(s, 2))));
        assert_eq!(approvals.held.get(&s).map(|p| p.ask), Some(2));
        let settled = Settled::Released;
        approvals.heard(&PermissionEvent::Settled { session: s, ask: 1, outcome: settled.clone() });
        assert!(approvals.held.contains_key(&s), "another prompt's end");
        assert!(approvals.take(s, 1).is_none(), "not the one waiting");
        assert!(approvals.take(s, 2).is_some());
        assert!(approvals.take(s, 2).is_none(), "once");
        approvals.heard(&PermissionEvent::Asked(Box::new(bash(s, 2))));
        assert!(approvals.held.is_empty(), "shown again after a resync, still answered");
        approvals.heard(&PermissionEvent::Asked(Box::new(bash(s, 3))));
        approvals.heard(&PermissionEvent::Settled { session: s, ask: 3, outcome: settled });
        assert!(approvals.held.is_empty(), "settled elsewhere");
    }
}
