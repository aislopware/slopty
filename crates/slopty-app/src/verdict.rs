//! "Allow" and "Deny" on an approval note, a reply typed on an agent's other notes, and
//! "Merge" on a note of work ready to merge, answered with no window.
//!
//! The press may have launched the iOS app in the background, or woken it from suspension
//! before anything listens for taps.
//!
//! iOS starts GPUI, and with it the links to the workers, only once a window scene connects,
//! which a press in the background never brings. So a press that finds nobody listening
//! ([`slopty_platform::notify::answer_unheard`]) is answered here instead, through the server:
//! a link of its own to the server the settings name, the thread's open requests read for the
//! choice that allows or denies once, or the pick's at its place, and that choice sent, as the
//! workspace answers a note whose worker it is not linked to. "Merge" puts the task the note names
//! in its project's merge queue (`Verb::TaskMerge`), the person's word. The system is told the
//! press is done once the answer is out or given up ([`slopty_platform::notify::taps_finished`]),
//! within the time it grants.
//!
//! An answer that did not land is said in a note in place of the one pressed ([`missed_note`]),
//! so the person does not walk away believing it went: the machine was not reached, or the
//! prompt no longer waits. A tap on it goes to the agent, as the pressed note's did.

use std::time::Duration;

use slopty_client::server::ServerCaller;
use slopty_core::{SessionId, WorkerId};
use slopty_platform::notify::{self, Note, Pressed, Tap, info};
use slopty_proto::orchestration::{ErrorCode, Outcome, TermRef, ThreadOf, ThreadView, Verb};
use slopty_proto::project::{ProjectId, TaskId};
use slopty_proto::thread::{AskId, ThreadId};

/// The most a background answer takes before it gives up: well inside the half minute iOS
/// grants an app it woke for a notification's button.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(20);

/// What a pressed "Allow", "Deny" or pick answers.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Verdict {
    /// The thread, as the server finds it: by its id, or by the terminal its agent runs in.
    pub of: ThreadOf,
    /// Its request.
    pub ask: AskId,
    /// The button pressed.
    pub pressed: Pressed,
}

/// What a press with no window does.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Press {
    /// "Allow" or "Deny".
    Verdict(Verdict),
    /// A reply typed on the note, sent to the thread as a message (`Verb::SendMessage`).
    Reply {
        /// The thread.
        of: ThreadOf,
        /// What the person typed.
        text: String,
    },
    /// "Merge" on work ready to merge: the task the note names, into its project's merge
    /// queue (`Verb::TaskMerge`).
    Merge {
        /// In which project.
        project: ProjectId,
        /// Which task.
        task: TaskId,
    },
}

/// What `tap` does: a verdict ([`verdict_of`]), or a reply with words in it to the thread
/// the note names.
#[must_use]
pub fn press_of(tap: &Tap) -> Option<Press> {
    if let Some((project, task)) = notify::merge_of(tap) {
        return Some(Press::Merge { project, task });
    }
    if tap.action.as_deref() == Some(notify::REPLY) {
        let text = tap.text.as_deref().map(str::trim).filter(|t| !t.is_empty())?;
        return Some(Press::Reply { of: thread_of(tap)?, text: text.to_owned() });
    }
    verdict_of(tap).map(Press::Verdict)
}

/// What `tap` answers, when it is a press of "Allow", "Deny" or a pick on a note that names
/// its request and where it was asked.
#[must_use]
pub fn verdict_of(tap: &Tap) -> Option<Verdict> {
    let pressed = Pressed::of(tap)?;
    let ask = AskId(tap.info.get(info::ASK)?.clone());
    Some(Verdict { of: thread_of(tap)?, ask, pressed })
}

/// The thread a note is about: by its id, or by the terminal its agent runs in.
fn thread_of(tap: &Tap) -> Option<ThreadOf> {
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
    Some(of)
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

/// Do `press` through the server `caller` reaches: a verdict as [`answer`] does, a reply sent
/// to its thread.
pub async fn answer_press(caller: &ServerCaller, press: Press) -> Answered {
    match press {
        Press::Verdict(verdict) => answer(caller, verdict).await,
        Press::Reply { of, text } => match caller.call(Verb::SendMessage { of, text }).await {
            Outcome::Error { message, .. } => Answered::Failed(message),
            _ => Answered::Sent,
        },
        Press::Merge { project, task } => {
            merged(caller.call(Verb::TaskMerge { project, task }).await)
        }
    }
}

/// How a "Merge" went, from what the server answered: a task already merged, or no longer
/// there, waits on nobody's merge; any other refusal did not land.
fn merged(outcome: Outcome) -> Answered {
    match outcome {
        Outcome::Error {
            code: ErrorCode::Invalid | ErrorCode::UnknownTask | ErrorCode::UnknownProject,
            ..
        } => Answered::Gone,
        Outcome::Error { message, .. } => Answered::Failed(message),
        _ => Answered::Sent,
    }
}

/// Answer `verdict` through the server `caller` reaches: read the thread's open requests for
/// the choice that allows or denies once, or for the request a pick answers, then answer with
/// it.
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
            .and_then(|r| verdict.pressed.choice(&r.choices, &r.picks)),
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

/// What a note says when the work a background "Merge" names no longer waits on a merge.
pub const NO_LONGER_READY: &str = "That work no longer waits to merge";
/// Under it.
pub const MERGED_ELSEWHERE: &str = "It was merged already, or its task is gone.";

/// What a note says when a background answer prompt is no longer waiting.
pub const NO_LONGER_WAITING: &str = "That prompt is no longer waiting";
/// Under it.
pub const ANSWERED_ELSEWHERE: &str = "It was answered elsewhere, or it ended.";

/// The note that says `answered` did not land, in place of the note `tap` pressed.
///
/// `None` once it was sent. `machine` is the name of the machine the agent runs on, where this
/// device knows it. The note carries the pressed one's way to the agent, without its request,
/// so a tap on it shows the agent and no button answers twice. A reply that did not go keeps
/// its field, to be sent again.
#[must_use]
pub fn missed_note(tap: &Tap, answered: &Answered, machine: Option<&str>) -> Option<Note> {
    let mut info = tap.info.clone();
    info.remove(info::ASK);
    let note = Note { id: tap.id.clone(), info, ..Note::default() };
    let merge = tap.action.as_deref() == Some(notify::MERGE);
    match answered {
        Answered::Sent => None,
        Answered::Gone if merge => Some(Note {
            title: NO_LONGER_READY.to_owned(),
            body: MERGED_ELSEWHERE.to_owned(),
            silent: true,
            ..note
        }),
        Answered::Gone => Some(Note {
            title: NO_LONGER_WAITING.to_owned(),
            body: ANSWERED_ELSEWHERE.to_owned(),
            silent: true,
            ..note
        }),
        Answered::Failed(why) => {
            let (pressed, category) = match tap.action.as_deref() {
                Some(notify::REPLY) => ("Your reply", Some(notify::REPLYING)),
                Some(notify::MERGE) => ("Merge", Some(notify::MERGING)),
                Some(notify::DENY) => ("Deny", None),
                Some(action) if action.starts_with(notify::PICK) => ("Your answer", None),
                _ => ("Allow", None),
            };
            let title = machine.map_or_else(
                || "Couldn't reach your server".to_owned(),
                |name| format!("Couldn't reach {name}"),
            );
            tracing::info!(why, "a background answer failed");
            Some(Note {
                title,
                body: if merge {
                    format!("{pressed} was not sent. The work still waits.")
                } else if category.is_some() {
                    format!("{pressed} was not sent.")
                } else {
                    format!("{pressed} was not sent. The agent is still waiting.")
                },
                category,
                urgent: category.is_none(),
                ..note
            })
        }
    }
}

/// The name this device last knew for the machine `tap` is about, from the server's directory
/// as it was kept.
fn machine_of(tap: &Tap) -> Option<String> {
    let worker: u128 = tap.info.get(info::WORKER)?.parse().ok()?;
    let worker: WorkerId = format!("{worker:032x}").parse().ok()?;
    let server =
        slopty_settings::Settings::load(&slopty_settings::path()).settings.network.server?;
    slopty_client::directory::Directory::load(&crate::server::cache_path(), &server)
        .into_iter()
        .find(|w| w.worker == worker)
        .map(|w| w.name)
}

/// Answer `tap` with no window, say so in a note when the answer did not land, then tell the
/// system the press is done.
///
/// It is what [`slopty_platform::notify::answer_unheard`] calls for a press nobody listens to,
/// and it answers on a thread of its own ([`answer_alone`]).
pub fn answer_unheard(tap: Tap) {
    let spawned = std::thread::Builder::new().name("slopty-verdict".to_owned()).spawn(move || {
        let answered = answer_alone(&tap);
        tracing::info!(id = tap.id, ?answered, "a verdict answered in the background");
        match missed_note(&tap, &answered, machine_of(&tap).as_deref()) {
            Some(note) => notify::post_alone(&note, notify::taps_finished),
            None => notify::taps_finished(),
        }
    });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "the background answer's thread");
        notify::taps_finished();
    }
}

/// Answer `tap` through the server the settings name, within [`ANSWER_WITHIN`].
///
/// It runs on a runtime and a link of the call's own and blocks the calling thread, so it is
/// called on one of its own. A tap that is no verdict, or settings that name no server, fail
/// at once.
#[must_use]
pub fn answer_alone(tap: &Tap) -> Answered {
    let Some(press) = press_of(tap) else {
        return Answered::Failed(
            "not an Allow, a Deny, a reply that names its thread, or a Merge that names its task"
                .to_owned(),
        );
    };
    let server = slopty_settings::Settings::load(&slopty_settings::path()).settings.network.server;
    let Some(server) = server else {
        return Answered::Failed("no server is set".to_owned());
    };
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(e) => return Answered::Failed(e.to_string()),
    };
    runtime.block_on(async move {
        let (task, _events) = match crate::net::serve_alone(server) {
            Ok(link) => link,
            Err(why) => return Answered::Failed(why),
        };
        let caller = task.caller();
        let answered = tokio::time::timeout(ANSWER_WITHIN, answer_press(&caller, press)).await;
        answered.unwrap_or_else(|_late| Answered::Failed("no answer in time".to_owned()))
    })
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
            text: None,
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
            Some(Verdict {
                of: ThreadOf::Thread(thread),
                ask: AskId("a1".into()),
                pressed: Pressed::Allow
            })
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
            Some(Verdict { of, ask: AskId("a1".into()), pressed: Pressed::Deny })
        );
        assert_eq!(
            verdict_of(&tap(notify::SHOW, &[(info::THREAD, thread.to_string()), ask])),
            None
        );
        assert_eq!(verdict_of(&tap(notify::ALLOW, &[(info::THREAD, thread.to_string())])), None);
    }

    /// "Merge" on a ready note merges the task it names, through the server: a task merged
    /// already or gone waits on nobody, and is said so quietly; a refusal that did not land
    /// keeps the button to press again; one that went says nothing.
    #[test]
    fn a_merge_pressed_on_a_ready_note_merges_its_task() {
        let pressed = tap(
            notify::MERGE,
            &[(info::PROJECT, "store".to_owned()), (info::TASK, "4".to_owned())],
        );
        let project = ProjectId::new("store").expect("a name");
        assert_eq!(press_of(&pressed), Some(Press::Merge { project, task: TaskId(4) }));
        assert_eq!(press_of(&tap(notify::MERGE, &[(info::TASK, "4".to_owned())])), None);

        let refused = |code| Outcome::Error { code, message: "no".to_owned() };
        assert_eq!(merged(refused(ErrorCode::Invalid)), Answered::Gone, "merged already");
        assert_eq!(merged(refused(ErrorCode::UnknownTask)), Answered::Gone);
        assert_eq!(merged(refused(ErrorCode::Failed)), Answered::Failed("no".to_owned()));

        assert_eq!(missed_note(&pressed, &Answered::Sent, None), None);
        let gone = missed_note(&pressed, &Answered::Gone, None).expect("said");
        assert_eq!((gone.title.as_str(), gone.silent), (NO_LONGER_READY, true));
        let failed = Answered::Failed("no answer in time".to_owned());
        let again = missed_note(&pressed, &failed, Some("studio")).expect("said");
        assert_eq!(again.body, "Merge was not sent. The work still waits.");
        assert_eq!(again.category, Some(notify::MERGING), "to press again");
        assert_eq!(again.info.get(info::TASK), Some(&"4".to_owned()), "naming the same task");
    }

    /// A background answer that did not land is said in place of the pressed note, by the
    /// machine where this device knows it, and leads to the agent with no button to answer
    /// again; a sent one says nothing.
    #[test]
    fn a_background_answer_that_did_not_land_is_said() {
        let thread = ThreadId::new();
        let pressed =
            tap(notify::DENY, &[(info::THREAD, thread.to_string()), (info::ASK, "a1".to_owned())]);
        assert_eq!(missed_note(&pressed, &Answered::Sent, Some("studio")), None);

        let failed = Answered::Failed("no answer in time".to_owned());
        let note = missed_note(&pressed, &failed, Some("studio")).expect("said");
        assert_eq!(note.id, pressed.id, "in place of the pressed note");
        assert_eq!(note.title, "Couldn't reach studio");
        assert_eq!(note.body, "Deny was not sent. The agent is still waiting.");
        assert!(note.urgent && !note.silent, "the agent still needs the person");
        assert_eq!(note.category, None, "no button answers twice");
        assert_eq!(
            note.info,
            BTreeMap::from([(info::THREAD.to_owned(), thread.to_string())]),
            "a tap shows the agent"
        );
        let unknown = missed_note(&pressed, &failed, None).expect("said");
        assert_eq!(unknown.title, "Couldn't reach your server");

        let gone = missed_note(&pressed, &Answered::Gone, Some("studio")).expect("said");
        assert_eq!((gone.title.as_str(), gone.silent), (NO_LONGER_WAITING, true));
    }

    /// A reply carries its words to the thread the note names, and goes as one message; a
    /// refusal is said, with the field kept to send it again. An empty reply is no press.
    #[tokio::test]
    async fn a_reply_goes_to_its_thread_as_a_message() {
        let thread = ThreadId::new();
        let mut typed = tap(notify::REPLY, &[(info::THREAD, thread.to_string())]);
        assert_eq!(press_of(&typed), None, "no words");
        typed.text = Some("  run the tests too ".to_owned());
        let press = press_of(&typed).expect("a reply");
        let of = ThreadOf::Thread(thread);
        assert_eq!(press, Press::Reply { of: of.clone(), text: "run the tests too".to_owned() });
        let (caller, mut queue) = ServerCaller::queued();
        let sending = tokio::spawn(async move { answer_press(&caller, press).await });
        let sent = loop {
            if let Some(next) = queue.try_next() {
                break next;
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(sent.0, Verb::SendMessage { of, text: "run the tests too".to_owned() });
        let _sent = sent.1.send(Outcome::Done);
        assert_eq!(sending.await.ok(), Some(Answered::Sent));

        let failed = Answered::Failed("no answer in time".to_owned());
        let note = missed_note(&typed, &failed, Some("studio")).expect("said");
        assert_eq!(note.body, "Your reply was not sent.");
        assert_eq!(note.category, Some(notify::REPLYING), "to send again");
        assert!(!note.urgent, "nothing waits on it");
    }

    fn choice(id: &str, effect: Effect) -> Choice {
        Choice { id: id.to_owned(), label: id.to_owned(), effect, scope: None, stops: false }
    }

    /// The answer is the choice that allows or denies once, by the request the note named; a
    /// request the server no longer holds is gone, and its refusal is said.
    #[tokio::test]
    async fn a_verdict_answers_with_the_choice_that_allows_or_denies_once() {
        let thread = ThreadId::new();
        let verdict = Verdict {
            of: ThreadOf::Thread(thread),
            ask: AskId("a1".into()),
            pressed: Pressed::Deny,
        };
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
            picks: Vec::new(),
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

    /// A question's option pressed on its note with no window answers the request with the
    /// choice at the pick's place among the request's, read through the server while the
    /// request is still open there. One that did not land is said with no pick left on it to
    /// answer twice.
    #[tokio::test]
    async fn a_pick_answers_with_its_own_choice_with_no_window() {
        let thread = ThreadId::new();
        let picked = r#"[{"question":"Layout?","answer":"Unified"}]"#;
        let note = Note {
            id: "note".to_owned(),
            info: BTreeMap::from([
                (info::THREAD.to_owned(), thread.to_string()),
                (info::ASK.to_owned(), "a2".to_owned()),
            ]),
            ..Note::default()
        }
        .picking(&["Split".to_owned(), "Unified".to_owned()]);
        let pressed = Tap {
            id: note.id.clone(),
            info: note.info.clone(),
            action: Some(notify::pick_id(1)),
            text: None,
        };
        let Some(Press::Verdict(verdict)) = press_of(&pressed) else {
            panic!("a pick is a verdict: {pressed:?}")
        };
        assert_eq!(verdict.pressed, Pressed::Pick(1));
        let (caller, mut queue) = ServerCaller::queued();
        let answering = tokio::spawn(async move { answer(&caller, verdict).await });
        let read = loop {
            if let Some(next) = queue.try_next() {
                break next;
            }
            tokio::task::yield_now().await;
        };
        let request = RequestRead {
            ask: AskId("a2".into()),
            kind: "question".to_owned(),
            title: "Layout?".to_owned(),
            choices: Vec::new(),
            questions: Vec::new(),
            picks: vec![
                slopty_proto::thread::wire::NoteChoice {
                    label: "Split".to_owned(),
                    choice: r#"[{"question":"Layout?","answer":"Split"}]"#.to_owned(),
                },
                slopty_proto::thread::wire::NoteChoice {
                    label: "Unified".to_owned(),
                    choice: picked.to_owned(),
                },
            ],
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
                ask: AskId("a2".into()),
                choice: picked.to_owned(),
                message: None,
            }
        );
        let _sent = sent.1.send(Outcome::Done);
        assert_eq!(answering.await.ok(), Some(Answered::Sent));

        let failed = Answered::Failed("no link".to_owned());
        let missed = missed_note(&pressed, &failed, Some("studio")).expect("said");
        assert_eq!(missed.body, "Your answer was not sent. The agent is still waiting.");
        assert!(missed.picks.is_empty(), "no pick to press again");
    }
}
