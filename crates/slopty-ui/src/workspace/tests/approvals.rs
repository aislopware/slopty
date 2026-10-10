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
    Tap { id: format!("thread-{thread}"), info, action: Some(action.to_owned()), text: None }
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
            picks: Vec::new(),
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

/// A note's reply to a thread whose worker this client has no link to goes through the server
/// as one message, and the app may sleep once it is out; a refused one is said, and a reply
/// with no words sends nothing.
#[gpui::test]
fn a_notes_reply_goes_through_the_server_while_its_worker_is_away(cx: &mut TestAppContext) {
    use crate::workspace::approvals::REPLY_NOT_SENT;
    let (view, cx) = workspace(cx);
    let laptop = worker_key(WorkerId::new());
    let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
    let thread = ThreadId::new();
    view.update_in(cx, |v, _w, cx| {
        v.set_server_caller(Some(caller));
        v.add_worker(laptop, "laptop".into(), cx);
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
    let reply = |text: &str| Tap {
        text: Some(text.to_owned()),
        ..tapped(laptop, thread, "unused", notify::REPLY)
    };

    view.update_in(cx, |v, _w, cx| v.open_notification(&reply("  "), cx));
    cx.run_until_parked();
    assert!(queue.try_next().is_none(), "no words, nothing sent");
    assert_eq!(events.borrow().as_slice(), [WorkspaceEvent::TapsSettled]);

    events.borrow_mut().clear();
    view.update_in(cx, |v, _w, cx| v.open_notification(&reply("also bump the lockfile"), cx));
    cx.run_until_parked();
    let (verb, answer) = queue.try_next().expect("sent through the server");
    assert_eq!(
        verb,
        Verb::SendMessage {
            of: ThreadOf::Thread(thread),
            text: "also bump the lockfile".to_owned()
        }
    );
    assert!(events.borrow().is_empty(), "not out yet");
    let _gone = answer.send(Outcome::Done);
    cx.run_until_parked();
    assert_eq!(events.borrow().as_slice(), [WorkspaceEvent::TapsSettled], "out: it may sleep");

    events.borrow_mut().clear();
    view.update_in(cx, |v, _w, cx| v.open_notification(&reply("again"), cx));
    cx.run_until_parked();
    let (_, answer) = queue.try_next().expect("sent");
    let refused = Outcome::Error {
        code: slopty_proto::orchestration::ErrorCode::WorkerUnreachable,
        message: "no such thread".to_owned(),
    };
    let _gone = answer.send(refused);
    cx.run_until_parked();
    let route = Route { worker: laptop, item: None, about: About::Thread(thread) };
    assert_eq!(
        events.borrow().as_slice(),
        [WorkspaceEvent::Unanswered { route, why: REPLY_NOT_SENT }, WorkspaceEvent::TapsSettled]
    );
}

/// A note's reply to a thread whose worker is linked here goes straight to it, as the thread's
/// own composer sends a message, and nothing goes through the server; the app may sleep at once.
#[gpui::test]
fn a_notes_reply_goes_straight_to_a_linked_worker(cx: &mut TestAppContext) {
    use slopty_proto::thread::Cursor;
    use slopty_proto::thread::wire::{Intent, TableFrame, ThreadRequest};

    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
    let mut state = crate::conversation::thread::fixtures::thread("edit");
    state.meta.terminal = None;
    let row = state.row(WallMs::ZERO);
    let thread = row.id;
    view.update_in(cx, |v, _w, cx| {
        v.set_server_caller(Some(caller));
        v.threads_linked(key, cx);
        let table = TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 1 }, rows: vec![row] };
        v.thread_table(key, &table, cx);
        v.set_app_active(false, cx);
    });
    cx.run_until_parked();
    studio.drain();
    let events = Rc::new(RefCell::new(Vec::new()));
    let heard = Rc::clone(&events);
    cx.update(|_window, cx| {
        cx.subscribe(&view, move |_view, event: &WorkspaceEvent, _cx| {
            heard.borrow_mut().push(*event);
        })
        .detach();
    });

    let reply = Tap {
        text: Some("also bump the lockfile".to_owned()),
        ..tapped(key, thread, "", notify::REPLY)
    };
    view.update_in(cx, |v, _w, cx| v.open_notification(&reply, cx));
    cx.run_until_parked();
    let sent: Vec<String> = studio
        .drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Intent {
                thread: to,
                intent: Intent::Send { text, .. },
                ..
            }) if to == thread => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(sent, ["also bump the lockfile"], "one message, to its thread");
    assert!(queue.try_next().is_none(), "nothing through the server");
    assert_eq!(events.borrow().as_slice(), [WorkspaceEvent::TapsSettled], "it may sleep");
}

/// A ready note's "Merge" merges the task it names through the server, the person's word,
/// leaving the workspace where it is; the tap settles once the server has answered, and a
/// refusal is said in the server's words.
#[gpui::test]
fn a_ready_notes_merge_merges_the_task_it_names(cx: &mut TestAppContext) {
    use slopty_platform::notify::info::{PROJECT, TASK};
    use slopty_proto::orchestration::ErrorCode;
    use slopty_proto::project::{ProjectId, TaskId};

    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
    view.update_in(cx, |v, _w, _cx| v.set_server_caller(Some(caller)));
    cx.run_until_parked();
    let events = Rc::new(RefCell::new(Vec::new()));
    let heard = Rc::clone(&events);
    cx.update(|_window, cx| {
        cx.subscribe(&view, move |_view, event: &WorkspaceEvent, _cx| {
            heard.borrow_mut().push(*event);
        })
        .detach();
    });
    let merge = Tap {
        id: "project-store-7".to_owned(),
        info: BTreeMap::from([
            (PROJECT.to_owned(), "store".to_owned()),
            (TASK.to_owned(), "4".to_owned()),
        ]),
        action: Some(notify::MERGE.to_owned()),
        text: None,
    };
    assert!(attention::answers(&merge), "answered where the note is");
    view.update_in(cx, |v, _w, cx| v.open_notification(&merge, cx));
    cx.run_until_parked();
    let (verb, reply) = queue.try_next().expect("the merge went to the server");
    let store = ProjectId::new("store").expect("a name");
    assert_eq!(verb, Verb::TaskMerge { project: store, task: TaskId(4) });
    assert!(events.borrow().is_empty(), "not settled before the server answers");
    let _gone = reply.send(Outcome::Done);
    cx.run_until_parked();
    assert_eq!(events.borrow().as_slice(), [WorkspaceEvent::TapsSettled], "it may sleep");
    assert_eq!(focused(&view, cx), Some(shell), "the workspace stays where it was");

    view.update_in(cx, |v, _w, cx| v.open_notification(&merge, cx));
    cx.run_until_parked();
    let (_, reply) = queue.try_next().expect("asked again");
    let refused = "task 4 is merged already".to_owned();
    let _gone = reply.send(Outcome::Error { code: ErrorCode::Invalid, message: refused.clone() });
    cx.run_until_parked();
    let said = view.read_with(cx, |v, _| v.toast_texts());
    assert!(said.contains(&refused), "in the server's words: {said:?}");
}
