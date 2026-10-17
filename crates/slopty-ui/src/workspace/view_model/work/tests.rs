use futures::FutureExt as _;
use gpui::AppContext as _;
use slopty_core::WallMs;
use slopty_proto::ClientMsg;
use slopty_proto::thread::wire::{
    Intent, IntentDone, Outcome, TableFrame, ThreadFrame, ThreadRequest,
};
use slopty_proto::thread::{
    AskId, BackgroundTask, Cap, Choice, Cursor, Effect, Item, ItemBody, ItemId, Link, Request,
    RequestState, ThreadId, ThreadState, TreeRef, TurnId, UserMessage,
};
use slopty_view_doc::{SessionCommandPayload, calls};
use slopty_view_rpc::{RpcError, RpcReply};
use tokio::sync::mpsc::Receiver;

use crate::conversation::thread::fixtures;
use crate::workspace::WorkspaceView;
use crate::workspace::rig::{KEY, link, mac};

/// A workspace linked to one worker whose table holds `states`' threads, the first followed
/// as a chat, and what the worker has been sent since.
fn holding<'a>(
    states: &[ThreadState],
    cx: &'a mut gpui::TestAppContext,
) -> (gpui::Entity<WorkspaceView>, &'a mut gpui::VisualTestContext, Receiver<ClientMsg>) {
    let workspace = cx.new(|_| WorkspaceView::new(slopty_theme::Theme::default()));
    let cx = cx.add_empty_window();
    workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
    let mut rx = link(&workspace, cx, mac(|_| {}), Vec::new());
    let cursor = Cursor { epoch: 1, seq: 1 };
    let rows = states.iter().map(|state| state.row(WallMs::ZERO)).collect();
    let first = states.first().map(|state| state.meta.id).expect("a thread");
    workspace.update(cx, |v, cx| {
        v.threads_linked(KEY, cx);
        v.thread_table(KEY, &TableFrame::Snapshot { cursor, rows }, cx);
        v.view_follow(first.to_string(), cx);
        v.view_open_asking(cx);
        for state in states {
            let frame = ThreadFrame::Snapshot { cursor, state: Box::new(state.clone()) };
            v.thread_frame(KEY, state.meta.id, frame, cx);
        }
    });
    cx.run_until_parked();
    while rx.try_recv().is_ok() {}
    (workspace, cx, rx)
}

/// The intents sent to the worker since, with the thread each went to.
fn intents(
    rx: &mut Receiver<ClientMsg>,
) -> Vec<(ThreadId, slopty_proto::thread::IntentId, Intent)> {
    std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|msg| match msg {
            ClientMsg::Thread(ThreadRequest::Intent { id, thread, intent }) => {
                Some((thread, id, intent))
            }
            _ => None,
        })
        .collect()
}

/// A call's value.
fn value(answer: Option<Result<RpcReply, RpcError>>) -> serde_json::Value {
    match answer.expect("answered") {
        Ok(RpcReply::Value(value)) => value,
        Ok(RpcReply::Stream(_)) => panic!("a stream, not a value"),
        Err(e) => panic!("refused: {e:?}"),
    }
}

/// A thread whose agent can stop tasks, with a command left running.
fn with_task() -> ThreadState {
    let mut state = fixtures::empty();
    state.meta.caps = vec![Cap::named(Cap::STOP_TASK)];
    state.tasks =
        vec![BackgroundTask::new("b1", BackgroundTask::SHELL, "npm run dev", WallMs::ZERO)];
    state
}

/// A tray row's stop goes to the agent as a stop of its task and is answered once the worker
/// said how it went; one the agent turned down says why; a row Slopty does not know fails.
#[gpui::test]
fn a_rows_stop_stops_its_task(cx: &mut gpui::TestAppContext) {
    let state = with_task();
    let thread = state.meta.id;
    let (workspace, cx, mut rx) = holding(&[state], cx);
    let params = serde_json::json!({ "chatId": thread.to_string(), "rowId": "task:b1" });
    let answer = workspace
        .update(cx, |v, cx| v.view_work_call(calls::STOP_WORK, &params, cx))
        .expect("a call of Slopty's");
    cx.run_until_parked();
    let sent = intents(&mut rx);
    let [(to, id, Intent::StopTask { task })] = sent.as_slice() else {
        panic!("one stop: {sent:?}");
    };
    assert_eq!((*to, task.as_str()), (thread, "b1"));
    let done = IntentDone { id: *id, outcome: Outcome::Refused { reason: "It ended".into() } };
    workspace.update(cx, |v, cx| v.thread_done(KEY, &done, cx));
    cx.run_until_parked();
    let refused = answer.now_or_never().expect("answered");
    assert!(matches!(refused, Err(RpcError::Failed(why)) if why == "It ended"));

    let params = serde_json::json!({ "chatId": thread.to_string(), "rowId": "bogus" });
    let answer = workspace.update(cx, |v, cx| v.view_work_call(calls::STOP_WORK, &params, cx));
    assert!(matches!(
        answer.and_then(futures::FutureExt::now_or_never),
        Some(Err(RpcError::BadParams(_)))
    ));
    assert!(
        workspace.update(cx, |v, cx| v.view_work_call("slopty.Nope", &params, cx)).is_none(),
        "another call goes on to the rest"
    );
}

/// "Allow with rule" adds no rule for a request only the person at its card answers, and sends
/// nothing: no rule answers its like again without them.
#[gpui::test]
fn no_rule_allows_what_needs_the_person_at_the_card(cx: &mut gpui::TestAppContext) {
    let mut state = fixtures::empty();
    let choice = |id: &str, effect| Choice {
        id: id.to_owned(),
        label: id.to_owned(),
        effect,
        scope: None,
        stops: false,
    };
    state.requests = vec![Request {
        id: AskId("ask-p".to_owned()),
        item: None,
        kind: Request::APPROVAL.to_owned(),
        title: "Run rm -rf build?".to_owned(),
        text: None,
        options: vec![choice("Yes", Effect::Allow), choice("No", Effect::Deny)],
        questions: Vec::new(),
        proposed: None,
        schema_json: None,
        url: None,
        reason: None,
        state: RequestState::Open,
        opened_ms: WallMs::ZERO,
        until_ms: None,
        needs_presence: true,
    }];
    let chat = state.meta.id.to_string();
    let (workspace, cx, mut rx) = holding(&[state], cx);
    let params = serde_json::json!({
        "chatId": chat, "requestId": "ask-p", "rule": "Bash(rm:*)", "scope": "local",
    });
    let answer = workspace
        .update(cx, |v, cx| v.view_work_call(calls::ALLOW_WITH_RULE, &params, cx))
        .expect("a call of Slopty's");
    cx.run_until_parked();
    let Some(Err(RpcError::Failed(why))) = answer.now_or_never() else {
        panic!("refused at once");
    };
    assert_eq!(why, Request::NEEDS_PRESENCE_REFUSED);
    assert!(intents(&mut rx).is_empty(), "no rule and no answer go");
}

/// A followed chat's tray, dock and rules go out beside its transcript.
#[gpui::test]
fn a_followed_chat_publishes_its_tray(cx: &mut gpui::TestAppContext) {
    let state = with_task();
    let chat = state.meta.id.to_string();
    let (workspace, cx, _rx) = holding(&[state], cx);
    let views = workspace.update(cx, |v, cx| v.view_chat_views(&chat, cx)).expect("its views");
    assert_eq!(views.work.summary, "1 command");
    assert_eq!(views.work.rows.first().map(|row| row.id.as_str()), Some("task:b1"));
    assert!(views.dock.can.stop_work);
    assert_eq!(views.rules.chat_id, chat);
}

/// A subagent's approval is asked in its parent's chat, under its name, and the person's
/// answer goes to the subagent's thread, not the parent's.
#[gpui::test]
fn a_subagents_request_is_asked_and_answered_in_its_parent(cx: &mut gpui::TestAppContext) {
    let parent = fixtures::empty();
    let mut child = fixtures::empty();
    child.meta.parent = Some(Link { thread: parent.meta.id, item: ItemId("t1".into()) });
    child.meta.title = "reviewer".to_owned();
    let choice = |id: &str, effect| Choice {
        id: id.to_owned(),
        label: id.to_owned(),
        effect,
        scope: None,
        stops: false,
    };
    child.requests = vec![Request {
        id: AskId("ask-c".to_owned()),
        item: None,
        kind: Request::APPROVAL.to_owned(),
        title: "Run cargo test?".to_owned(),
        text: None,
        options: vec![choice("Yes", Effect::Allow), choice("No", Effect::Deny)],
        questions: Vec::new(),
        proposed: None,
        schema_json: None,
        url: None,
        reason: None,
        state: RequestState::Open,
        opened_ms: WallMs::from_millis(1_800_000_000_000),
        until_ms: None,
        needs_presence: false,
    }];
    let (parent_id, child_id) = (parent.meta.id, child.meta.id);
    let (workspace, cx, mut rx) = holding(&[parent, child], cx);
    let said = workspace
        .update(cx, |v, cx| v.view_transcript(&parent_id.to_string(), cx))
        .expect("the parent's transcript");
    let asked = said
        .entries
        .iter()
        .flat_map(|entry| &entry.parts)
        .find_map(|part| match part {
            slopty_view_doc::MessagePart::Input { request_id, questions, .. } => {
                Some((request_id.clone(), questions.first()?.sent_by.clone()))
            }
            _ => None,
        })
        .expect("the subagent's request, asked in its parent");
    assert_eq!(asked.0, "ask-c");
    assert_eq!(asked.1.as_deref(), Some("reviewer"), "sent by the subagent");
    let params = serde_json::json!({
        "chatId": parent_id.to_string(),
        "command": SessionCommandPayload::RespondInput {
            request_id: "ask-c".to_owned(),
            answers: vec![slopty_view_proto::UserInputAnswer {
                question_id: "choice".to_owned(),
                labels: vec!["Yes".to_owned()],
                notes: None,
            }],
        },
    });
    workspace.update(cx, |v, cx| v.view_command(&params, cx)).expect("the answer goes");
    cx.run_until_parked();
    let sent = intents(&mut rx);
    let answered = sent.iter().find_map(|(to, _, intent)| match intent {
        Intent::Answer { ask, choice, .. } => Some((*to, ask.0.as_str(), choice.as_str())),
        _ => None,
    });
    assert_eq!(answered, Some((child_id, "ask-c", "Yes")));
}

/// Rewinding to a message puts the files back before its turn and branches the conversation
/// before it, answering with its words and the branched chat.
#[gpui::test]
fn a_rewind_goes_back_to_before_the_message(cx: &mut gpui::TestAppContext) {
    let mut state = fixtures::empty();
    state.meta.caps = vec![Cap::named(Cap::REWIND), Cap::named(Cap::FORK)];
    state.meta.title = "Speed up the build".into();
    state.items = vec![Item {
        id: ItemId("u2".into()),
        turn: TurnId(2),
        at_ms: WallMs::from_millis(1_800_000_000_000),
        body: ItemBody::User(UserMessage {
            text: slopty_proto::thread::detail::Clipped::whole("make it faster"),
            images: Vec::new(),
            command: None,
            intent: None,
        }),
    }];
    // The folder as the worker took it before the rewind, as it names it before it is done.
    state.rewound = Some(slopty_proto::thread::Rewound {
        turn: TurnId(2),
        tree: TreeRef("before-rewind".into()),
        at_ms: WallMs::from_millis(1_800_000_000_500),
    });
    let thread = state.meta.id;
    // The chat the fork will branch, already on the worker's table.
    let branch = fixtures::empty();
    let branched = branch.meta.id;
    let (workspace, cx, mut rx) = holding(&[state, branch], cx);
    let params = serde_json::json!({
        "chatId": thread.to_string(), "messageId": "u2", "code": true, "conversation": true,
    });
    let answer = workspace
        .update(cx, |v, cx| v.view_work_call(calls::REWIND, &params, cx))
        .expect("a call of Slopty's");
    cx.run_until_parked();
    let sent = intents(&mut rx);
    let rewind = sent.iter().find(|(_, _, i)| *i == Intent::Rewind { turn: TurnId(2) });
    let fork = sent.iter().find(|(_, _, i)| *i == Intent::Fork { after: Some(TurnId(1)) });
    let (Some((_, rewind, _)), Some((_, fork, _))) = (rewind, fork) else {
        panic!("a rewind and a fork: {sent:?}");
    };
    workspace.update(cx, |v, cx| {
        v.thread_done(KEY, &IntentDone { id: *rewind, outcome: Outcome::Done }, cx);
        let started = Outcome::Started { thread: branched };
        v.thread_done(KEY, &IntentDone { id: *fork, outcome: started }, cx);
    });
    cx.run_until_parked();
    let answer = value(answer.now_or_never());
    assert_eq!(answer["prompt"], "make it faster");
    assert_eq!(answer["chatId"], branched.to_string());
    // What undoes it: back to this chat with its folder as it was, the branch let go.
    assert_eq!(answer["undo"]["chatId"], thread.to_string());
    assert_eq!(answer["undo"]["branchChatId"], branched.to_string());
    assert_eq!(answer["undo"]["tree"], "before-rewind");
    // The branch goes by the conversation's name, the words taken back its draft.
    let sent = intents(&mut rx);
    let to_branch: Vec<&Intent> =
        sent.iter().filter(|(to, ..)| *to == branched).map(|(_, _, i)| i).collect();
    assert!(to_branch.contains(&&Intent::Name { title: "Speed up the build".into() }), "{sent:?}");
    assert!(to_branch.contains(&&Intent::Draft { text: "make it faster".into() }), "{sent:?}");
    let undo = serde_json::json!({ "undo": answer["undo"].clone() });
    let put_back = Intent::Unrewind { tree: TreeRef("before-rewind".into()) };
    let discarded = |sent: &[(ThreadId, slopty_proto::thread::IntentId, Intent)]| {
        sent.iter().any(|(to, _, i)| *to == branched && *i == Intent::Discard)
    };
    // An undo the worker refuses lets nothing go and says why.
    let refused = workspace
        .update(cx, |v, cx| v.view_work_call(calls::UNDO_REWIND, &undo, cx))
        .expect("a call of Slopty's");
    cx.run_until_parked();
    let sent = intents(&mut rx);
    let Some((_, unrewind, _)) = sent.iter().find(|(to, _, i)| *to == thread && *i == put_back)
    else {
        panic!("the folder put back: {sent:?}");
    };
    let no = Outcome::Refused { reason: "There is no rewind to undo".into() };
    workspace
        .update(cx, |v, cx| v.thread_done(KEY, &IntentDone { id: *unrewind, outcome: no }, cx));
    cx.run_until_parked();
    let Some(Err(RpcError::Failed(why))) = refused.now_or_never() else {
        panic!("the undo refused");
    };
    assert_eq!(why, "There is no rewind to undo");
    assert!(!discarded(&intents(&mut rx)), "a refused undo keeps the branch");

    let mut undone = workspace
        .update(cx, |v, cx| v.view_work_call(calls::UNDO_REWIND, &undo, cx))
        .expect("a call of Slopty's");
    cx.run_until_parked();
    let sent = intents(&mut rx);
    assert!(!discarded(&sent), "the branch stays until the folder is back: {sent:?}");
    let Some((_, unrewind, _)) = sent.iter().find(|(to, _, i)| *to == thread && *i == put_back)
    else {
        panic!("the folder put back: {sent:?}");
    };
    // The answer waits for the folder.
    assert!((&mut undone).now_or_never().is_none(), "answered before the folder came back");
    workspace.update(cx, |v, cx| {
        v.thread_done(KEY, &IntentDone { id: *unrewind, outcome: Outcome::Done }, cx);
    });
    cx.run_until_parked();
    assert!(discarded(&intents(&mut rx)), "the branch is let go once the folder is back");
    let undone = value(undone.now_or_never());
    assert_eq!(undone["chatId"], thread.to_string());
}

/// A rewind of the code alone is undone by putting the folder back, and one the worker could not
/// take the folder before has no undo.
#[gpui::test]
fn a_code_rewind_undoes_by_its_folder(cx: &mut gpui::TestAppContext) {
    let mut state = fixtures::empty();
    state.meta.caps = vec![Cap::named(Cap::REWIND)];
    state.items = vec![Item {
        id: ItemId("u2".into()),
        turn: TurnId(2),
        at_ms: WallMs::from_millis(1_800_000_000_000),
        body: ItemBody::User(UserMessage {
            text: slopty_proto::thread::detail::Clipped::whole("make it faster"),
            images: Vec::new(),
            command: None,
            intent: None,
        }),
    }];
    // An older rewind's folder is not this one's.
    state.rewound = Some(slopty_proto::thread::Rewound {
        turn: TurnId(1),
        tree: TreeRef("older".into()),
        at_ms: WallMs::from_millis(1_800_000_000_500),
    });
    let thread = state.meta.id;
    let (workspace, cx, mut rx) = holding(&[state], cx);
    let params =
        serde_json::json!({ "chatId": thread.to_string(), "messageId": "u2", "code": true });
    let answer = workspace
        .update(cx, |v, cx| v.view_work_call(calls::REWIND, &params, cx))
        .expect("a call of Slopty's");
    cx.run_until_parked();
    let sent = intents(&mut rx);
    let Some((_, rewind, _)) =
        sent.iter().find(|(_, _, i)| *i == Intent::Rewind { turn: TurnId(2) })
    else {
        panic!("a rewind: {sent:?}");
    };
    workspace.update(cx, |v, cx| {
        v.thread_done(KEY, &IntentDone { id: *rewind, outcome: Outcome::Done }, cx);
    });
    // It looks for the folder a second, then answers with no undo.
    for _ in 0..30 {
        cx.executor().advance_clock(std::time::Duration::from_millis(60));
        cx.run_until_parked();
    }
    let answer = value(answer.now_or_never());
    assert_eq!(answer["prompt"], "make it faster");
    assert!(answer.get("undo").is_none(), "no folder, no undo: {answer}");
    assert!(
        answer["noUndo"].as_str().is_some_and(|why| why.contains("can't be undone")),
        "{answer}"
    );
    assert!(answer.get("chatId").is_none(), "nothing branched: {answer}");
}
