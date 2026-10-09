//! A pushed note's "Allow" or "Deny" whose worker is not linked here: the server answers it.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use slopty_core::WorkerId;
use slopty_platform::notify::info::{ASK, THREAD, WORKER};
use slopty_platform::notify::{self, Tap};
use slopty_proto::orchestration::{Outcome, RequestRead, ThreadOf, ThreadRead, Verb};
use slopty_proto::thread::attention::{Ladder, Ranked, Rung, ThreadAt};
use slopty_proto::thread::{AgentId, AskId, Choice, Effect, Phase, Request, ThreadId, TurnId};

use super::super::projects::worker_key;
use super::*;
use crate::workspace::WorkspaceEvent;
use crate::workspace::attention::{About, NO_LONGER_WAITING, Route};

/// A note's button on `thread`'s request `ask` on `worker`, as the pushed note carries it.
fn tapped(worker: WorkerKey, thread: ThreadId, ask: &str, action: &str) -> Tap {
    let info = BTreeMap::from([
        (WORKER.to_owned(), worker.value().to_string()),
        (THREAD.to_owned(), thread.to_string()),
        (ASK.to_owned(), ask.to_owned()),
    ]);
    Tap { id: format!("thread-{thread}"), info, action: Some(action.to_owned()) }
}

/// `thread` on `worker` read through the server, its request `ask` open with a plain allow
/// once, an allow for the session and a deny.
fn read(worker: WorkerId, thread: ThreadId, ask: &str) -> Outcome {
    let choice = |id: &str, effect, scope: Option<&str>| Choice {
        id: id.to_owned(),
        label: id.to_owned(),
        effect,
        scope: scope.map(str::to_owned),
        stops: false,
    };
    Outcome::Thread(Box::new(ThreadRead {
        worker,
        thread,
        agent: AgentId("claude".to_owned()),
        title: "edit".to_owned(),
        parent: None,
        phase: Phase::default(),
        wait: None,
        turns: Vec::new(),
        requests: vec![RequestRead {
            ask: AskId(ask.to_owned()),
            kind: Request::APPROVAL.to_owned(),
            title: "Run `cargo test`".to_owned(),
            choices: vec![
                choice("always", Effect::Allow, Some("this session")),
                choice("yes", Effect::Allow, None),
                choice("no", Effect::Deny, None),
            ],
            questions: Vec::new(),
        }],
        next: TurnId(0),
        truncated: false,
        skipped: false,
    }))
}

/// A note's "Allow" for a thread whose worker this client has no link to, while the server's
/// ladder has it waiting on the person, goes through the server: it reads the request's
/// choices there and answers with the plain allow, once, and the app may sleep once it is out.
/// A second tap of it sends nothing more, and a request the server no longer finds is said.
#[gpui::test]
fn a_notes_answer_goes_through_the_server_while_its_worker_is_away(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let laptop_id = WorkerId::new();
    let laptop = worker_key(laptop_id);
    let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
    let (thread, gone) = (ThreadId::new(), ThreadId::new());
    let ranked = |thread| Ranked {
        at: ThreadAt { worker: laptop_id, thread },
        rung: Rung::NeedsYou,
        since_ms: WallMs::from_millis(1),
        terminal: None,
    };
    let ladder = Ladder { threads: vec![ranked(thread), ranked(gone)], ..Ladder::default() };
    view.update_in(cx, |v, _w, cx| {
        v.set_server_caller(Some(caller));
        v.add_worker(laptop, "laptop".into(), cx);
        v.server_ladder(&ladder, cx);
        v.set_app_active(false, cx);
    });
    cx.run_until_parked();
    let events = Rc::new(RefCell::new(Vec::new()));
    let heard = Rc::clone(&events);
    cx.update(|_window, cx| {
        cx.subscribe(&view, move |_view, event: &WorkspaceEvent, _cx| {
            heard.borrow_mut().push(*event);
        })
        .detach();
    });

    view.update_in(cx, |v, _w, cx| {
        v.open_notification(&tapped(laptop, thread, "ask-5", notify::ALLOW), cx);
    });
    cx.run_until_parked();
    let (verb, reply) = queue.try_next().expect("the thread is read through the server");
    let Verb::ReadThread { of: ThreadOf::Thread(of), hold: false, .. } = verb else {
        panic!("a read of the thread: {verb:?}");
    };
    assert_eq!(of, thread);
    assert!(!events.borrow().contains(&WorkspaceEvent::TapsSettled), "not out yet");
    let _gone = reply.send(read(laptop_id, thread, "ask-5"));
    cx.run_until_parked();
    let (verb, reply) = queue.try_next().expect("then answered");
    assert_eq!(
        verb,
        Verb::AnswerRequest {
            of: ThreadOf::Thread(thread),
            ask: AskId("ask-5".to_owned()),
            choice: "yes".to_owned(),
            message: None,
        },
        "with the plain allow, once"
    );
    let _gone = reply.send(Outcome::Done);
    cx.run_until_parked();
    assert_eq!(events.borrow().as_slice(), [WorkspaceEvent::TapsSettled], "out: it may sleep");

    events.borrow_mut().clear();
    view.update_in(cx, |v, _w, cx| {
        v.open_notification(&tapped(laptop, thread, "ask-5", notify::ALLOW), cx);
    });
    cx.run_until_parked();
    assert!(queue.try_next().is_none(), "a second tap of it sends nothing more");

    // The server no longer finds the request: said, as a note of the app's own while away.
    events.borrow_mut().clear();
    view.update_in(cx, |v, _w, cx| {
        v.open_notification(&tapped(laptop, gone, "ask-6", notify::DENY), cx);
    });
    cx.run_until_parked();
    let (_, reply) = queue.try_next().expect("read");
    let _gone = reply.send(read(laptop_id, gone, "ask-7"));
    cx.run_until_parked();
    assert!(queue.try_next().is_none(), "no answer for a request not there");
    let route = Route { worker: laptop, item: None, about: About::Thread(gone) };
    assert_eq!(
        events.borrow().as_slice(),
        [WorkspaceEvent::Unanswered { route, why: NO_LONGER_WAITING }, WorkspaceEvent::TapsSettled]
    );
}
