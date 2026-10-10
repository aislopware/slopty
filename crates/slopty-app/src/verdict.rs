//! "Allow" and "Deny" on an approval note, answered with no window: the iOS app launched in the
//! background for the press, or woken from suspension before anything listens for taps.
//!
//! iOS starts GPUI, and with it the links to the workers, only once a window scene connects,
//! which a press in the background never brings. So a press that finds nobody listening
//! ([`slopty_platform::notify::answer_unheard`]) is answered here instead, through the server:
//! a link of its own to the server the settings name, the thread's open requests read for the
//! choice that allows or denies once, and that choice sent, as the workspace answers a note
//! whose worker it is not linked to. The system is told the press is done once the answer is
//! out or given up ([`slopty_platform::notify::taps_finished`]), within the time it grants.

use std::time::Duration;

use slopty_client::server::ServerCaller;
use slopty_core::{SessionId, WorkerId};
use slopty_platform::notify::{self, Tap, info};
use slopty_proto::orchestration::{Outcome, TermRef, ThreadOf, ThreadView, Verb};
use slopty_proto::thread::{AskId, ThreadId};

/// The most a background answer takes before it gives up: well inside the half minute iOS
/// grants an app it woke for a notification's button.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(20);

/// What a pressed "Allow" or "Deny" answers.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Verdict {
    /// The thread, as the server finds it: by its id, or by the terminal its agent runs in.
    pub of: ThreadOf,
    /// Its request.
    pub ask: AskId,
    /// Allow, or deny.
    pub allow: bool,
}

/// What `tap` answers, when it is a press of "Allow" or "Deny" on a note that names its
/// request and where it was asked.
#[must_use]
pub fn verdict_of(tap: &Tap) -> Option<Verdict> {
    let allow = match tap.action.as_deref()? {
        notify::ALLOW => true,
        notify::DENY => false,
        _ => return None,
    };
    let ask = AskId(tap.info.get(info::ASK)?.clone());
    let of = match tap.info.get(info::SESSION) {
        Some(session) => {
            // The app keys a worker by its id's 128 bits, in decimal; the id reads them in hex.
            let worker: u128 = tap.info.get(info::WORKER)?.parse().ok()?;
            let worker: WorkerId = format!("{worker:032x}").parse().ok()?;
            let session: SessionId = session.parse().ok()?;
            ThreadOf::Term(TermRef { worker, session })
        }
        None => ThreadOf::Thread(tap.info.get(info::THREAD)?.parse::<ThreadId>().ok()?),
    };
    Some(Verdict { of, ask, allow })
}

/// How a background answer went.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Answered {
    /// The server took the answer.
    Sent,
    /// The request is no longer open there: answered elsewhere, or ended.
    Gone,
    /// The server could not be asked, in its words.
    Failed(String),
}

/// Answer `verdict` through the server `caller` reaches: read the thread's open requests for
/// the choice that allows or denies once, then answer with it.
pub async fn answer(caller: &ServerCaller, verdict: Verdict) -> Answered {
    let read = Verb::ReadThread {
        of: verdict.of.clone(),
        view: ThreadView::Messages,
        after: None,
        hold: false,
    };
    let choice = match caller.call(read).await {
        Outcome::Thread(read) => read
            .requests
            .iter()
            .find(|r| r.ask == verdict.ask)
            .and_then(|r| slopty_proto::thread::once(&r.choices, verdict.allow))
            .map(|c| c.id.clone()),
        Outcome::Error { message, .. } => return Answered::Failed(message),
        _ => None,
    };
    let Some(choice) = choice else { return Answered::Gone };
    let verb = Verb::AnswerRequest { of: verdict.of, ask: verdict.ask, choice, message: None };
    match caller.call(verb).await {
        Outcome::Error { message, .. } => Answered::Failed(message),
        _ => Answered::Sent,
    }
}

/// Answer `tap` with no window, then tell the system the press is done.
///
/// It is what [`slopty_platform::notify::answer_unheard`] calls for a press nobody listens to,
/// and it answers on a thread and a runtime of its own. A tap that is no verdict, or settings
/// that name no server, is done at once.
pub fn answer_unheard(tap: Tap) {
    let Some(verdict) = verdict_of(&tap) else {
        notify::taps_finished();
        return;
    };
    let server = slopty_settings::Settings::load(&slopty_settings::path()).settings.client.server;
    let Some(server) = server else {
        tracing::info!(id = tap.id, "a verdict pressed with no server set: not answered");
        notify::taps_finished();
        return;
    };
    let spawned = std::thread::Builder::new().name("slopty-verdict".to_owned()).spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
        let answered = match runtime {
            Ok(runtime) => runtime.block_on(async move {
                let (task, _events) = match crate::net::serve_alone(server) {
                    Ok(link) => link,
                    Err(why) => return Answered::Failed(why),
                };
                let caller = task.caller();
                let answered = tokio::time::timeout(ANSWER_WITHIN, answer(&caller, verdict)).await;
                answered.unwrap_or_else(|_late| Answered::Failed("no answer in time".to_owned()))
            }),
            Err(e) => Answered::Failed(e.to_string()),
        };
        tracing::info!(id = tap.id, ?answered, "a verdict answered in the background");
        notify::taps_finished();
    });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "the background answer's thread");
        notify::taps_finished();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use slopty_proto::orchestration::{RequestRead, ThreadRead};
    use slopty_proto::thread::{Choice, Effect};

    use super::*;

    fn tap(action: &str, info: &[(&str, String)]) -> Tap {
        Tap {
            id: "note".to_owned(),
            info: info
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect::<BTreeMap<_, _>>(),
            action: Some(action.to_owned()),
        }
    }

    /// A press names its request and where it was asked: a thread by its id, a terminal's agent
    /// by its worker and session. Anything else, or a note's own tap, is no verdict.
    #[test]
    fn a_press_names_its_thread_or_terminal_and_its_request() {
        let thread = ThreadId::new();
        let ask = (info::ASK, "a1".to_owned());
        let allowed = tap(notify::ALLOW, &[(info::THREAD, thread.to_string()), ask.clone()]);
        assert_eq!(
            verdict_of(&allowed),
            Some(Verdict { of: ThreadOf::Thread(thread), ask: AskId("a1".into()), allow: true })
        );
        let worker = WorkerId::new();
        let session = SessionId::new();
        let key = worker.as_uuid().as_u128().to_string();
        let denied = tap(
            notify::DENY,
            &[(info::WORKER, key), (info::SESSION, session.to_string()), ask.clone()],
        );
        let of = ThreadOf::Term(TermRef { worker, session });
        assert_eq!(
            verdict_of(&denied),
            Some(Verdict { of, ask: AskId("a1".into()), allow: false })
        );
        assert_eq!(
            verdict_of(&tap(notify::SHOW, &[(info::THREAD, thread.to_string()), ask])),
            None
        );
        assert_eq!(verdict_of(&tap(notify::ALLOW, &[(info::THREAD, thread.to_string())])), None);
    }

    fn choice(id: &str, effect: Effect) -> Choice {
        Choice { id: id.to_owned(), label: id.to_owned(), effect, scope: None, stops: false }
    }

    /// The answer is the choice that allows or denies once, by the request the note named; a
    /// request the server no longer holds is gone, and its refusal is said.
    #[tokio::test]
    async fn a_verdict_answers_with_the_choice_that_allows_or_denies_once() {
        let thread = ThreadId::new();
        let verdict =
            Verdict { of: ThreadOf::Thread(thread), ask: AskId("a1".into()), allow: false };
        let (caller, mut queue) = ServerCaller::queued();
        let answering = tokio::spawn(async move { answer(&caller, verdict).await });
        let read = loop {
            if let Some(next) = queue.try_next() {
                break next;
            }
            tokio::task::yield_now().await;
        };
        assert!(
            matches!(&read.0, Verb::ReadThread { of: ThreadOf::Thread(t), .. } if *t == thread)
        );
        let request = RequestRead {
            ask: AskId("a1".into()),
            kind: "permission".to_owned(),
            title: "Run cargo test".to_owned(),
            choices: vec![choice("yes", Effect::Allow), choice("no", Effect::Deny)],
            questions: Vec::new(),
        };
        let held = ThreadRead {
            worker: WorkerId::new(),
            thread,
            agent: slopty_proto::thread::AgentId::named(slopty_proto::thread::AgentId::CLAUDE_CODE),
            title: String::new(),
            parent: None,
            phase: slopty_proto::thread::Phase::NeedsYou,
            wait: None,
            turns: Vec::new(),
            requests: vec![request],
            next: slopty_proto::thread::TurnId(0),
            truncated: false,
            skipped: false,
        };
        let _sent = read.1.send(Outcome::Thread(Box::new(held)));
        let sent = loop {
            if let Some(next) = queue.try_next() {
                break next;
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(
            sent.0,
            Verb::AnswerRequest {
                of: ThreadOf::Thread(thread),
                ask: AskId("a1".into()),
                choice: "no".to_owned(),
                message: None,
            }
        );
        let _sent = sent.1.send(Outcome::Done);
        assert_eq!(answering.await.ok(), Some(Answered::Sent));
    }
}
