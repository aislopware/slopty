//! How what the person sends reaches its agent, through the workspace: each chat's link to its
//! machine, a Stop's answer, a message sent again, a draft kept on its machine, a failed turn
//! tried again, the chats searched, and a chat with no project started in a folder of its own.

use gpui::AppContext as _;
use slopty_core::WallMs;
use slopty_proto::thread::wire::{
    Intent, IntentDone, Outcome, TableFrame, ThreadFrame, ThreadRequest,
};
use slopty_proto::thread::{
    Cursor, Delivery, IntentId, Pending, PendingState, ThreadState, TurnState,
};
use slopty_proto::{ClientMsg, WorkerMsg};
use slopty_view_doc::SessionCommandPayload;
use slopty_view_proto::{ChatSyncState, ConnectivityState};
use slopty_view_rpc::{RpcError, RpcReply, methods};
use tokio::sync::mpsc::Receiver;

use crate::conversation::thread::fixtures;
use crate::workspace::rig::{KEY, link, mac};
use crate::workspace::{WorkerStatus, WorkspaceView};

/// A workspace linked to one worker whose table holds `state`'s thread (its row as `row` makes
/// it), its state here, serving a feed, and what the worker has been sent since.
fn holding<'a>(
    state: &ThreadState,
    row: impl FnOnce(&mut slopty_proto::thread::wire::ThreadRow),
    cx: &'a mut gpui::TestAppContext,
) -> (
    gpui::Entity<WorkspaceView>,
    &'a mut gpui::VisualTestContext,
    Receiver<ClientMsg>,
    std::sync::Arc<crate::view_model::Feed>,
) {
    let workspace = cx.new(|_| WorkspaceView::new(slopty_theme::Theme::default()));
    let cx = cx.add_empty_window();
    workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
    let mut rx = link(&workspace, cx, mac(|_| {}), Vec::new());
    let (_, feed, _asks) = crate::view_model::ViewModel::new("me".to_owned());
    let cursor = Cursor { epoch: 1, seq: 1 };
    let thread = state.meta.id;
    let mut shown = state.row(WallMs::ZERO);
    row(&mut shown);
    workspace.update(cx, |v, cx| {
        v.view_model.feed = Some(std::sync::Arc::clone(&feed));
        v.threads_linked(KEY, cx);
        v.thread_table(KEY, &TableFrame::Snapshot { cursor, rows: vec![shown] }, cx);
        v.thread_hub(KEY, cx).update(cx, |hub, cx| hub.open(thread, cx));
        let frame = ThreadFrame::Snapshot { cursor, state: Box::new(state.clone()) };
        v.thread_frame(KEY, thread, frame, cx);
    });
    cx.run_until_parked();
    while rx.try_recv().is_ok() {}
    (workspace, cx, rx, feed)
}

/// The intents the worker was sent, in order.
fn intents(rx: &mut Receiver<ClientMsg>) -> Vec<(IntentId, Intent)> {
    std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|msg| match msg {
            ClientMsg::Thread(ThreadRequest::Intent { id, intent, .. }) => Some((id, intent)),
            _ => None,
        })
        .collect()
}

/// A message to `chat`, as the shell sends one.
fn run(chat: &str, prompt: &str) -> serde_json::Value {
    run_as(chat, prompt, "m-1")
}

/// A message to `chat` the shell named `message_id`.
fn run_as(chat: &str, prompt: &str, message_id: &str) -> serde_json::Value {
    let request: slopty_view_proto::RunRequest = serde_json::from_value(serde_json::json!({
        "prompt": prompt, "model": null, "reasoning": null, "cwd": "/w",
        "sandbox": "workspace-write", "resume": null,
    }))
    .expect("a run");
    let command = SessionCommandPayload::Run { request, message_id: message_id.to_owned() };
    serde_json::json!({ "chatId": chat, "command": command })
}

/// A message is one id from the shell to the agent and back: it goes under the id the shell
/// gave it, and its entry in the transcript goes by that id however the thread is read again
/// (its link to the intent lost, as a worker reading the session's file again has it; this
/// client's own keeping lost, as after a restart), so the shell's echo always gives way. An id
/// already asked under goes as a new intent, since the worker acts on one once.
#[gpui::test]
fn a_message_is_one_id_from_the_shell_to_the_transcript(cx: &mut gpui::TestAppContext) {
    use slopty_proto::thread::{Item, ItemBody, ItemId};
    let state = fixtures::thread("tools");
    let chat = state.meta.id.to_string();
    let thread = state.meta.id;
    let (workspace, cx, mut rx, _feed) = holding(&state, |_| {}, cx);
    workspace.update(cx, |v, _| {
        v.view_model.followed.insert(chat.clone(), (KEY, thread));
    });
    let message = IntentId::new();
    let params = run_as(&chat, "hi", &message.to_string());
    let _sent = workspace.update(cx, |v, cx| v.view_command(&params, cx)).expect("sent");
    cx.run_until_parked();
    let sent: Vec<IntentId> = intents(&mut rx)
        .into_iter()
        .filter(|(_, intent)| matches!(intent, Intent::Send { .. }))
        .map(|(id, _)| id)
        .collect();
    assert_eq!(sent, [message], "it goes under the shell's id");

    // Claude Code's transcript holds it under the uuid it was given, which is that id, with
    // nothing linking it to the intent once its file is read again; another agent's under an
    // id of its own, linked to the intent. And then this client keeps nothing of it.
    let prompt = state
        .items
        .iter()
        .find(|item| matches!(item.body, ItemBody::User(_)))
        .cloned()
        .expect("a message of the person's");
    let read = |id: String, intent: Option<IntentId>| {
        let mut item = Item { id: ItemId(id), ..prompt.clone() };
        if let ItemBody::User(user) = &mut item.body {
            user.intent = intent;
        }
        item
    };
    let reads = [
        (read(message.to_string(), Some(message)), true),
        (read(message.to_string(), None), false),
        (read("item-7".to_owned(), Some(message)), false),
    ];
    for (seq, (item, kept)) in reads.into_iter().enumerate() {
        let mut read = state.clone();
        read.items.retain(|i| !matches!(i.body, ItemBody::User(_)));
        read.items.insert(0, item);
        let shown = workspace.update(cx, |v, cx| {
            if !kept {
                v.view_model.named.clear();
                v.view_model.said.clear();
            }
            let seq = 2_u64.saturating_add(u64::try_from(seq).expect("a few"));
            let frame =
                ThreadFrame::Snapshot { cursor: Cursor { epoch: 1, seq }, state: Box::new(read) };
            v.thread_frame(KEY, thread, frame, cx);
            v.view_transcript(&chat, cx).expect("its transcript")
        });
        let ids: Vec<&str> = shown
            .entries
            .iter()
            .filter(|e| e.role == slopty_view_doc::MessageRole::User)
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(ids, [message.to_string()], "read again ({seq}): under the shell's id");
    }

    let again = workspace.update(cx, |v, cx| v.view_command(&params, cx));
    let _again = again.expect("sent again");
    cx.run_until_parked();
    let (id, _) = intents(&mut rx).pop().expect("the second");
    assert_ne!(id, message, "an id asked under already goes as a new intent");
}

/// A chat whose worker drops says so, by the chat, and recovers once the link is back; with no
/// server, the whole is as reachable as its workers.
#[gpui::test]
fn a_chat_whose_worker_drops_says_so_and_recovers(cx: &mut gpui::TestAppContext) {
    let state = fixtures::thread("tools");
    let chat = state.meta.id.to_string();
    let (workspace, cx, _rx, feed) = holding(&state, |_| {}, cx);
    let link_of = |status: WorkerStatus, cx: &mut gpui::VisualTestContext| {
        workspace.update(cx, |v, cx| {
            v.set_worker_status(KEY, status, cx);
            v.view_publish(cx);
        });
        let said = feed.said_connectivity();
        let chat = said.chats.into_iter().find(|c| c.chat_id == chat).expect("the chat's link");
        (said.state, chat.sync_state, chat.connected, chat.delivery_live)
    };
    assert_eq!(
        link_of(WorkerStatus::Relinking, cx),
        (ConnectivityState::Reconnecting, ChatSyncState::Connecting, false, false)
    );
    assert_eq!(
        link_of(WorkerStatus::NoAnswer, cx),
        (ConnectivityState::Reconnecting, ChatSyncState::Offline, false, false)
    );
    assert_eq!(
        link_of(WorkerStatus::Connected, cx),
        (ConnectivityState::Connected, ChatSyncState::Synced, true, true)
    );
    feed.server(Some(slopty_view_proto::ServerPresence {
        name: "hub".to_owned(),
        online: false,
        ..Default::default()
    }));
    assert_eq!(feed.said_connectivity().state, ConnectivityState::Reconnecting, "the server's");
}

/// A Stop its worker leaves unanswered fails after its time, so the shell may ask again; one
/// the worker accepts is answered at once.
#[gpui::test]
fn a_stop_left_unanswered_fails(cx: &mut gpui::TestAppContext) {
    use futures::FutureExt as _;
    let state = fixtures::thread("tools");
    let (workspace, cx, mut rx, _feed) = holding(&state, |_| {}, cx);
    let params = serde_json::json!({
        "chatId": state.meta.id.to_string(), "command": { "kind": "interrupt" },
    });
    let answer = workspace.update(cx, |v, cx| v.view_interrupt(&params, cx));
    cx.run_until_parked();
    let past = super::INTERRUPT_WITHIN.saturating_add(std::time::Duration::from_secs(1));
    cx.executor().advance_clock(past);
    cx.run_until_parked();
    let answer = answer.now_or_never().expect("answered");
    assert!(matches!(answer, Err(RpcError::Failed(why)) if why.contains("did not stop")));

    let _sent = intents(&mut rx);
    let answer = workspace.update(cx, |v, cx| v.view_interrupt(&params, cx));
    cx.run_until_parked();
    let (id, _) = intents(&mut rx).pop().expect("the stop");
    let done = IntentDone { id, outcome: Outcome::Accepted };
    workspace.update(cx, |v, cx| v.thread_done(KEY, &done, cx));
    cx.run_until_parked();
    assert!(matches!(answer.now_or_never(), Some(Ok(_))), "accepted: under way");
}

/// A message that may not have reached its agent, or that its agent refused, goes again on
/// Retry as a new message under the same row, once; one waiting its turn is left as it is.
#[gpui::test]
fn a_message_that_may_not_have_gone_goes_again_on_retry(cx: &mut gpui::TestAppContext) {
    let mut state = fixtures::thread("tools");
    let (lost, refused, waiting) = (IntentId::new(), IntentId::new(), IntentId::new());
    let held = |intent, text: &str, state| Pending {
        intent,
        text: text.to_owned(),
        attachments: Vec::new(),
        delivery: Delivery::Queue,
        state,
    };
    state.pending = vec![
        held(lost, "first", PendingState::Held { reason: Pending::UNKNOWN.to_owned() }),
        held(refused, "second", PendingState::Held { reason: Pending::refused("busy") }),
        held(waiting, "third", PendingState::Waiting),
    ];
    let (workspace, cx, mut rx, _feed) = holding(&state, |_| {}, cx);
    workspace.update(cx, |v, _| {
        v.view_model.named.insert(lost, "m-1".to_owned());
        v.view_model.queue_rows.insert(lost, "m-1".to_owned());
    });
    let params = serde_json::json!({ "chatId": state.meta.id.to_string() });
    let answer =
        workspace.update(cx, |v, cx| v.view_surface_call(methods::RETRY_DELIVERY, &params, cx));
    cx.run_until_parked();
    let answer = futures::FutureExt::now_or_never(answer.expect("answered")).expect("at once");
    assert!(matches!(answer, Ok(RpcReply::Value(v)) if v["retried"] == 2), "the two held");
    let sent = intents(&mut rx);
    let withdrawn: Vec<IntentId> = sent
        .iter()
        .filter_map(|(_, intent)| match intent {
            Intent::Withdraw { pending } => Some(*pending),
            _ => None,
        })
        .collect();
    assert_eq!(withdrawn, [lost, refused]);
    let again: Vec<(IntentId, String)> = sent
        .into_iter()
        .filter_map(|(id, intent)| match intent {
            Intent::Send { text, delivery: Delivery::Queue, .. } => Some((id, text)),
            _ => None,
        })
        .collect();
    assert_eq!(
        again.iter().map(|(_, text)| text.as_str()).collect::<Vec<_>>(),
        ["first", "second"]
    );
    let first = again[0].0;
    assert_ne!(first, lost, "a new intent");
    workspace.update(cx, |v, _| {
        assert_eq!(v.view_model.named.get(&first).map(String::as_str), Some("m-1"), "same row");
        assert_eq!(v.view_model.queue_rows.get(&first).map(String::as_str), Some("m-1"));
    });
}

/// What the person is writing to a chat is kept on its machine, shows on the chat for every
/// device, and is cleared there once a message goes.
#[gpui::test]
fn a_draft_is_kept_on_its_machine(cx: &mut gpui::TestAppContext) {
    let state = fixtures::thread("tools");
    let chat = state.meta.id.to_string();
    let (workspace, cx, mut rx, _feed) = holding(
        &state,
        |row| {
            row.draft = Some(slopty_proto::thread::wire::Draft {
                text: "half a thought".to_owned(),
                at_ms: WallMs::from_millis(1),
            });
        },
        cx,
    );
    let row = workspace.update(cx, |v, cx| {
        v.held_hub(KEY)
            .and_then(|hub| hub.read(cx).threads().rows().rows.get(&state.meta.id).cloned())
    });
    let shown = crate::view_model::rows::chat(&row.expect("its row"), &KEY.to_string());
    assert_eq!(shown.draft.as_deref(), Some("half a thought"));

    let save = serde_json::json!({ "chatId": chat, "text": "a whole thought" });
    let kept = workspace.update(cx, |v, cx| v.view_call(methods::SAVE_DRAFT, &save, cx));
    let _kept = kept.expect("kept");
    let too_long = serde_json::json!({ "chatId": chat, "text": "x".repeat(20_000) });
    let refused = workspace.update(cx, |v, cx| v.view_call(methods::SAVE_DRAFT, &too_long, cx));
    let _refused = refused.expect_err("too long");
    cx.run_until_parked();
    assert_eq!(
        intents(&mut rx).into_iter().map(|(_, i)| i).collect::<Vec<_>>(),
        [Intent::Draft { text: "a whole thought".to_owned() }]
    );
    let sent =
        workspace.update(cx, |v, cx| v.view_call(methods::QUEUE_COMMAND, &run(&chat, "go"), cx));
    assert!(sent.is_ok(), "{sent:?}");
    cx.run_until_parked();
    let after: Vec<Intent> = intents(&mut rx).into_iter().map(|(_, i)| i).collect();
    assert!(matches!(after.first(), Some(Intent::Send { .. })), "{after:?}");
    assert!(after.contains(&Intent::Draft { text: String::new() }), "cleared once it went");
}

/// A failed last turn is tried again once, with the words that began it.
#[gpui::test]
fn a_failed_turn_offers_retry_and_retries_once(cx: &mut gpui::TestAppContext) {
    let mut state = fixtures::thread("tools");
    let last = state.turns.last_mut().expect("a turn");
    last.state = TurnState::Failed { error: "Overloaded".to_owned(), until_ms: None };
    let input = last.input.clone().expect("the message that began it");
    let words = state
        .items
        .iter()
        .find_map(|item| match &item.body {
            slopty_proto::thread::ItemBody::User(message) if item.id == input => {
                Some(message.text.text.clone())
            }
            _ => None,
        })
        .expect("its words");
    let (workspace, cx, mut rx, _feed) = holding(&state, |_| {}, cx);
    let params = serde_json::json!({ "chatId": state.meta.id.to_string() });
    let tried = workspace.update(cx, |v, cx| v.view_call(methods::RETRY_TURN, &params, cx));
    assert!(tried.is_ok(), "{tried:?}");
    cx.run_until_parked();
    let sent: Vec<Intent> = intents(&mut rx).into_iter().map(|(_, i)| i).collect();
    assert!(
        matches!(&*sent, [Intent::Send { text, .. }] if *text == words),
        "once, as it was: {sent:?}"
    );
}

/// A turn that did not fail is not tried again, and nothing goes.
#[gpui::test]
fn a_turn_that_did_not_fail_is_not_tried_again(cx: &mut gpui::TestAppContext) {
    let fine = fixtures::thread("tools");
    let (workspace, cx, mut rx, _feed) = holding(&fine, |_| {}, cx);
    let params = serde_json::json!({ "chatId": fine.meta.id.to_string() });
    let refused = workspace.update(cx, |v, cx| v.view_call(methods::RETRY_TURN, &params, cx));
    assert!(matches!(refused, Err(RpcError::Failed(_))), "{refused:?}");
    cx.run_until_parked();
    assert!(intents(&mut rx).is_empty(), "nothing sent");
}

/// The chats are found by their words, with the match marked in an excerpt.
#[gpui::test]
fn chats_are_found_by_their_words(cx: &mut gpui::TestAppContext) {
    let state = fixtures::thread("tools");
    let words = state
        .items
        .iter()
        .find_map(|item| match &item.body {
            slopty_proto::thread::ItemBody::User(message) => Some(message.text.text.clone()),
            _ => None,
        })
        .expect("the person's words");
    let word: String = words.split_whitespace().max_by_key(|w| w.len()).expect("a word").to_owned();
    let (workspace, cx, _rx, _feed) = holding(&state, |_| {}, cx);
    let search = |query: &str, cx: &mut gpui::VisualTestContext| {
        let params = serde_json::json!({ "query": query, "limit": 10 });
        workspace
            .update(cx, |v, cx| v.view_call(methods::SEARCH_CHATS, &params, cx))
            .expect("an answer")
    };
    let found = search(&word.to_uppercase(), cx);
    let hit = &found.as_array().expect("a list")[0];
    assert_eq!(hit["chatId"], state.meta.id.to_string());
    assert_eq!(hit["role"], "user");
    let excerpt = hit["excerpt"].as_str().expect("an excerpt");
    let (start, end) = (
        usize::try_from(hit["matchStart"].as_u64().expect("a start")).expect("small"),
        usize::try_from(hit["matchEnd"].as_u64().expect("an end")).expect("small"),
    );
    assert_eq!(excerpt.get(start..end).map(str::to_lowercase), Some(word.to_lowercase()));
    assert_eq!(search("zzqqxx-nowhere", cx), serde_json::json!([]));
    assert_eq!(search("a", cx), serde_json::json!([]), "one letter finds nothing");
}

/// A chat with no project starts in a scratch folder of its own, made on its machine first; the
/// start waits for it.
#[gpui::test]
fn a_projectless_chat_starts_in_its_own_scratch_folder(cx: &mut gpui::TestAppContext) {
    use slopty_proto::folder::{FsOp, FsOutcome, FsRefusal};
    let workspace = cx.new(|_| WorkspaceView::new(slopty_theme::Theme::default()));
    let cx = cx.add_empty_window();
    workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
    let mut rx = link(&workspace, cx, mac(|_| {}), Vec::new());
    let cwd = slopty_view_proto::projectless::scratch_cwd("c-9", chrono::Utc::now());
    let create = serde_json::json!({
        "op": "createChat", "chatId": "c-9", "deviceId": KEY.to_string(), "cwd": cwd,
    });
    let created = workspace.update(cx, |v, cx| v.view_call(methods::MUTATE, &create, cx));
    let _created = created.expect("created");
    let command: SessionCommandPayload =
        serde_json::from_value(run("c-9", "hi")["command"].clone()).expect("a run");
    let params = serde_json::json!({ "chatId": "c-9", "command": command });
    let queued = workspace.update(cx, |v, cx| v.view_call(methods::QUEUE_COMMAND, &params, cx));
    let _queued = queued.expect("queued");
    let mut made = Vec::new();
    for answer in 0..3 {
        cx.run_until_parked();
        let asked = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
        assert!(
            !asked.iter().any(|m| matches!(m, ClientMsg::Thread(ThreadRequest::Start { .. }))),
            "no start before its folder"
        );
        let (request, op) = asked
            .into_iter()
            .find_map(|msg| match msg {
                ClientMsg::FsOp { request, op } => Some((request, op)),
                _ => None,
            })
            .expect("a folder made");
        let FsOp::MakeDir { parent, name } = op else { panic!("a folder") };
        made.push(format!("{parent}/{name}"));
        // The first two are there already.
        let outcome = if answer < 2 {
            FsOutcome::Refused(FsRefusal::Clash { path: name })
        } else {
            FsOutcome::Done { path: format!("/Users/me/{name}") }
        };
        workspace.update(cx, |v, _| v.surface_heard(KEY, &WorkerMsg::FsDone { request, outcome }));
    }
    assert_eq!(made.last(), Some(&cwd), "{made:?}");
    cx.run_until_parked();
    let started = std::iter::from_fn(|| rx.try_recv().ok()).find_map(|msg| match msg {
        ClientMsg::Thread(ThreadRequest::Start { start, .. }) => Some(start.cwd),
        _ => None,
    });
    assert_eq!(started, Some(cwd), "then it starts there");
}

/// When a usage limit holding the agent lifts: its failed turn's own word.
#[test]
fn a_limit_says_when_it_lifts() {
    use slopty_proto::thread::{Phase, Wait};
    let mut state = fixtures::thread("tools");
    assert_eq!(super::limit_resets(&state), None);
    let until = WallMs::from_millis(1_760_000_000_000);
    state.turns.last_mut().expect("a turn").state =
        TurnState::Failed { error: "limit".to_owned(), until_ms: Some(until) };
    state.status.phase = Phase::Failed;
    state.status.wait = Some(Wait { kind: Wait::LIMIT.to_owned(), text: String::new() });
    assert_eq!(super::limit_resets(&state), Some(crate::view_model::rows::time(until)));
}
