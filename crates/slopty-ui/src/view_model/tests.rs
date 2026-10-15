use std::sync::Arc;

use chrono::Utc;
use slopty_view_doc::{MessagePart, MessageRole, MessageStatus, TranscriptUpdate};
use slopty_view_rpc::{RpcError, memory_client, methods};

use super::{Ask, ViewModel, rows, transcript};
use crate::conversation::thread::fixtures;

/// A worker as the shell's device: `linked` whether this client reaches it now.
fn device(
    id: &str,
    name: &str,
    linked: bool,
    now: chrono::DateTime<Utc>,
) -> slopty_view_proto::Device {
    slopty_view_proto::Device {
        id: id.to_owned(),
        name: name.to_owned(),
        platform: String::new(),
        last_seen_at: linked.then_some(now),
        created_at: None,
        version: None,
        cursor_sdk_version: None,
        capabilities: Vec::new(),
        actions: Vec::new(),
    }
}

/// A message the shell sent goes by the id the shell gave it, so the shell's echo of it gives
/// way to it instead of waiting below it as "Sending…" for good.
#[test]
fn a_message_the_shell_sent_goes_by_the_id_it_gave() {
    let mut state = fixtures::thread("tools");
    let sent = slopty_proto::thread::IntentId::new();
    let item = state
        .items
        .iter_mut()
        .find(|item| matches!(item.body, slopty_proto::thread::ItemBody::User(_)))
        .expect("the person's message");
    if let slopty_proto::thread::ItemBody::User(message) = &mut item.body {
        message.intent = Some(sent);
    }
    let named = |intent| (intent == sent).then(|| "m-1".to_owned());
    let entries = transcript::entries(&state, "w", |_| None, named);
    assert_eq!(entries.first().map(|e| e.id.as_str()), Some("m-1"));
    let unnamed = transcript::entries(&state, "w", |_| None, |_| None);
    assert_ne!(unnamed.first().map(|e| e.id.as_str()), Some("m-1"), "not sent from here");
}

/// A recorded session's thread, as the shell's transcript, applied by the shell's own reducer.
#[test]
fn a_recorded_session_reads_as_the_shells_transcript() {
    let state = fixtures::thread("tools");
    let entries = transcript::entries(&state, "w", |_| None, |_| None);

    let first = entries.first().expect("the person's message");
    assert_eq!(first.role, MessageRole::User);
    let answers: Vec<_> = entries.iter().filter(|e| e.role == MessageRole::Assistant).collect();
    assert!(!answers.is_empty(), "the agent's answer");
    assert!(answers.iter().all(|e| e.status == Some(MessageStatus::Complete)));
    let tools = answers
        .iter()
        .flat_map(|e| &e.parts)
        .filter(|p| matches!(p, MessagePart::Tool { resolved: true, .. }))
        .count();
    assert!(tools > 0, "its tool calls, each resolved");
    let mut ids: Vec<_> = entries.iter().map(|e| e.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), entries.len(), "every entry's id is its own");

    let mut rows = Vec::new();
    let frame = slopty_view_doc::TranscriptFrame::Reset { reset: entries.clone() };
    slopty_view_doc::apply_transcript_frame(&mut rows, frame).expect("the shell takes the frame");
    assert_eq!(rows, entries);
}

/// An edit reads as the shell's edit, with its counts.
#[test]
fn an_edit_carries_its_counts() {
    let state = fixtures::thread("edit");
    let entries = transcript::entries(&state, "w", |_| None, |_| None);
    let edit = entries
        .iter()
        .flat_map(|e| &e.parts)
        .find_map(|p| match p {
            MessagePart::Tool {
                call: slopty_view_proto::ToolCall::EditFile { .. },
                diff_stats,
                ..
            } => Some(diff_stats.clone()),
            _ => None,
        })
        .expect("the edit");
    let stats = edit.expect("its counts");
    assert!(stats.iter().any(|s| s.additions + s.deletions > 0));
}

/// A thread's phase reads as the shell's session status; one waiting on its background work is
/// at rest.
#[test]
fn a_threads_phase_reads_as_a_session() {
    use slopty_proto::thread::Phase;
    use slopty_view_proto::SessionStatus;
    let state = fixtures::thread("tools");
    let row = |phase| {
        let mut row = state.row(slopty_core::WallMs::ZERO);
        row.status.phase = phase;
        row
    };
    let now = Utc::now();
    for (phase, status) in [
        (Phase::Working, SessionStatus::Working),
        (Phase::Waiting, SessionStatus::Idle),
        (Phase::NeedsYou, SessionStatus::AwaitingInput),
        (Phase::Failed, SessionStatus::Errored),
        (Phase::Idle, SessionStatus::Idle),
        (Phase::Done, SessionStatus::Idle),
    ] {
        assert_eq!(rows::session(&row(phase), "w", 0, now).status, status, "{phase:?}");
    }
}

/// A list's watch says the list, then each change; an unchanged list says nothing.
#[tokio::test]
async fn a_list_watch_follows_the_feed() {
    let (view_model, feed, _asks) = ViewModel::new("me".to_owned());
    let client = memory_client(view_model);
    let mut devices = client
        .subscribe_scoped(methods::WATCH_DEVICES, serde_json::json!({}))
        .await
        .expect("the watch");
    assert_eq!(devices.recv().await, Some(serde_json::json!([])));
    let mac = device("1", "Mac", true, Utc::now());
    feed.devices(vec![mac.clone()]);
    feed.devices(vec![mac.clone()]);
    let next = devices.recv().await.expect("the change");
    let read: Vec<slopty_view_proto::Device> = serde_json::from_value(next).expect("devices");
    assert_eq!(read, vec![mac]);
}

/// A checkout's pull request watch says the one its threads show for that folder and branch,
/// and says it again only when it changes.
#[tokio::test]
async fn a_checkouts_pull_request_follows_its_threads() {
    let (view_model, feed, _asks) = ViewModel::new("me".to_owned());
    feed.local_device(Some("w".to_owned()));
    let client = memory_client(view_model);
    let params = serde_json::json!({ "cwd": "/w/slopty/", "branch": "fix" });
    let mut watch = client
        .subscribe_scoped(methods::WATCH_CHECKOUT_CHANGE_REQUEST, params)
        .await
        .expect("the watch");
    let read = |value: Option<serde_json::Value>| {
        let status: slopty_view_proto::CheckoutChangeRequestStatus =
            serde_json::from_value(value.expect("a status")).expect("reads");
        status.change_request.map(|pr| pr.number)
    };
    assert_eq!(read(watch.recv().await), None, "none yet");
    let pull = |number, branch: &str| super::Pull {
        device: "w".to_owned(),
        folders: vec!["/w/slopty".to_owned()],
        branch: Some(branch.to_owned()),
        summary: slopty_view_proto::ChangeRequestSummary {
            provider: "github".to_owned(),
            number,
            title: "Fix".to_owned(),
            url: format!("https://github.com/o/r/pull/{number}"),
            state: slopty_view_proto::ChangeRequestState::Open,
            base_ref: "main".to_owned(),
            head_ref: branch.to_owned(),
        },
    };
    feed.pulls(vec![pull(7, "other")]);
    feed.pulls(vec![pull(7, "other"), pull(42, "fix")]);
    assert_eq!(read(watch.recv().await), Some(42), "the branch's own, once");
}

/// A transcript watch follows its chat, says the whole of it once and then what changed, and
/// lets it go when it closes.
#[tokio::test]
async fn a_transcript_watch_follows_and_lets_go() {
    let (view_model, feed, mut asks) = ViewModel::new("me".to_owned());
    let client = memory_client(view_model);
    let params = serde_json::json!({ "chatId": "c" });
    let mut watch =
        client.subscribe_scoped(methods::WATCH_DOC_MESSAGES, params).await.expect("the watch");
    assert!(matches!(asks.recv().await, Some(Ask::Follow(chat)) if chat == "c"));

    let state = fixtures::thread("tools");
    let all = transcript::entries(&state, "w", |_| None, |_| None);
    let (head, _) = all.split_at(1);
    // A watch holds the latest transcript only, so each is read before the next is said.
    let mut rows = Vec::new();
    for said in [head.to_vec(), all.clone()] {
        feed.transcript("c", said.clone());
        let value = watch.recv().await.expect("a frame");
        let update: TranscriptUpdate = serde_json::from_value(value).expect("the shell's frame");
        slopty_view_doc::apply_transcript_frame(&mut rows, update.frame).expect("in step");
        assert_eq!(rows, said);
    }

    drop(watch);
    assert!(matches!(asks.recv().await, Some(Ask::Unfollow(chat)) if chat == "c"));
    assert_eq!(feed.followed(), Vec::<String>::new());
}

/// Any other call goes to the workspace, whose answer comes back.
#[tokio::test]
async fn other_calls_go_to_the_workspace() {
    let (view_model, _feed, mut asks) = ViewModel::new("me".to_owned());
    let client = Arc::new(memory_client(view_model));
    let answering = tokio::spawn(async move {
        while let Some(ask) = asks.recv().await {
            if let Ask::Call { method, reply, .. } = ask {
                let answer = if method == "Known" {
                    Ok(slopty_view_rpc::RpcReply::Value(serde_json::json!({ "ok": true })))
                } else {
                    Err(RpcError::UnknownMethod(method))
                };
                let _gone = reply.send(answer);
            }
        }
    });
    assert_eq!(
        client.call("Known", serde_json::json!({})).await.expect("an answer"),
        serde_json::json!({ "ok": true })
    );
    assert!(matches!(
        client.call("Other", serde_json::json!({})).await,
        Err(RpcError::UnknownMethod(method)) if method == "Other"
    ));
    answering.abort();
}

/// An agent another agent started nests under it, by the parent its row names, and counts in
/// its parent's running subagents while it works.
#[test]
fn a_started_agent_nests_under_its_parent() {
    use slopty_proto::thread::Phase;
    let state = fixtures::thread("tools");
    let parent = state.row(slopty_core::WallMs::ZERO);
    let mut child = parent.clone();
    child.id = slopty_proto::thread::ThreadId::new();
    child.parent = None;
    child.facts.insert(slopty_proto::agent::PARENT_FACT.to_owned(), parent.id.to_string());
    child.status.phase = Phase::Working;

    assert_eq!(rows::chat(&child, "w").parent_chat_id, Some(parent.id.to_string()));
    assert_eq!(rows::running_children(parent.id, [&parent, &child].into_iter()), 1);
    child.status.phase = Phase::Done;
    assert_eq!(rows::running_children(parent.id, [&parent, &child].into_iter()), 0);
}

/// A request open on a thread, as an agent's worker holds it.
fn request(
    kind: &str,
    options: &[(&str, &str, slopty_proto::thread::Effect)],
) -> slopty_proto::thread::Request {
    use slopty_proto::thread::{AskId, Choice, Request, RequestState};
    Request {
        id: AskId("ask-1".to_owned()),
        item: None,
        kind: kind.to_owned(),
        title: "Run cargo test?".to_owned(),
        text: None,
        options: options
            .iter()
            .map(|(id, label, effect)| Choice {
                id: (*id).to_owned(),
                label: (*label).to_owned(),
                effect: *effect,
                scope: None,
                stops: false,
            })
            .collect(),
        questions: Vec::new(),
        proposed: None,
        schema_json: None,
        url: None,
        state: RequestState::Open,
        opened_ms: slopty_core::WallMs::from_millis(1),
        until_ms: None,
    }
}

/// The request the shell's question panel serves (`slopty_shell::composer::pending_input_request`):
/// an unanswered input on the last answer.
fn served(
    entries: &[slopty_view_doc::SessionMessageEntry],
) -> Option<(String, Vec<slopty_view_proto::UserInputQuestion>)> {
    entries.iter().rev().find(|e| e.role == MessageRole::Assistant).and_then(|entry| {
        entry.parts.iter().find_map(|part| match part {
            MessagePart::Input { request_id, questions, resolved: false, .. } => {
                Some((request_id.clone(), questions.clone()))
            }
            _ => None,
        })
    })
}

/// The question panel's answer to the question `id`.
fn picked(id: &str, label: &str) -> slopty_view_proto::UserInputAnswer {
    slopty_view_proto::UserInputAnswer {
        question_id: id.to_owned(),
        labels: vec![label.to_owned()],
    }
}

/// An approval the agent waits on is asked where the person looks: the question panel serves
/// it from the last answer, and the pick goes back as the choice it names.
#[test]
fn an_open_approval_is_asked_and_answered_by_its_choice() {
    use slopty_proto::thread::Effect;
    let mut state = fixtures::thread("tools");
    let approval = request(
        slopty_proto::thread::Request::APPROVAL,
        &[("allow", "Allow", Effect::Allow), ("deny", "Deny", Effect::Deny)],
    );
    state.requests.push(approval.clone());
    let mut entries = transcript::entries(&state, "w", |_| None, |_| None);
    transcript::asks(&state, "w", &mut entries);

    let (asked, questions) = served(&entries).expect("the panel serves it");
    assert_eq!(asked, "ask-1");
    assert_eq!(questions[0].options, ["Allow", "Deny"]);
    assert_eq!(
        transcript::answer(&approval, &[picked(&questions[0].id, "Allow")]),
        Ok(("allow".to_owned(), None))
    );
    // Words of one's own turn the call down, with them as the reason.
    assert_eq!(
        transcript::answer(&approval, &[picked(&questions[0].id, "use nextest")]),
        Ok(("deny".to_owned(), Some("use nextest".to_owned())))
    );
}

/// Claude Code held at its trust dialog has said nothing yet: its request still gets an answer of
/// its own to be asked on.
#[test]
fn a_request_before_any_answer_is_asked_on_one_of_its_own() {
    use slopty_proto::thread::Effect;
    let mut state = fixtures::thread("tools");
    state.items.clear();
    state.turns.clear();
    state.requests.push(request("input", &[("trust", "Trust this folder", Effect::Answer)]));
    let mut entries = Vec::new();
    transcript::asks(&state, "w", &mut entries);
    let (_, questions) = served(&entries).expect("the panel serves it");
    assert_eq!(questions[0].options, ["Trust this folder"]);
}

/// A request that offers choices and asks no questions (Claude Code's trust dialog, an approval)
/// is one click, headed by its own title; an agent's own questions keep the words one may add.
#[test]
fn a_request_of_choices_is_one_click_under_its_title() {
    use slopty_proto::thread::Effect;
    let mut state = fixtures::thread("tools");
    state.items.clear();
    state.turns.clear();
    state.requests.push(request("input", &[("trust", "Trust this folder", Effect::Allow)]));
    let mut entries = Vec::new();
    transcript::asks(&state, "w", &mut entries);
    let (_, questions) = served(&entries).expect("the panel serves it");
    assert!(questions[0].choices_only);
    assert_eq!(questions[0].header, "Run cargo test?");

    let mut asked = request(slopty_proto::thread::Request::QUESTION, &[]);
    asked.questions = vec![slopty_proto::thread::detail::Question {
        header: Some("Scope".to_owned()),
        text: "Which tests?".to_owned(),
        options: Vec::new(),
        multi_select: false,
    }];
    state.requests = vec![asked];
    let mut entries = Vec::new();
    transcript::asks(&state, "w", &mut entries);
    let (_, questions) = served(&entries).expect("the panel serves it");
    assert!(!questions[0].choices_only, "a question takes words");
}

/// A time the agent never said (a resumed session's history) is no time, not 1970.
#[test]
fn an_unknown_time_is_no_time() {
    let mut state = fixtures::thread("tools");
    let times = |state: &mut slopty_proto::thread::ThreadState, ms| {
        for item in &mut state.items {
            item.at_ms = slopty_core::WallMs::from_millis(ms);
        }
        transcript::entries(state, "w", |_| None, |_| None)
            .iter()
            .map(|entry| entry.created_at)
            .collect::<Vec<_>>()
    };
    let real = times(&mut state, 1_760_000_000_000);
    assert!(!real.is_empty() && real.iter().all(|at| *at == 1_760_000_000_000));
    assert!(times(&mut state, 0).iter().all(|at| *at == 0), "never said: no time");
    assert!(times(&mut state, 1_000).iter().all(|at| *at == 0), "1970: no time");
}

/// A turn left open by a stale cache, or by an agent gone without ending it, reads as finished
/// while the thread does not work: its tool groups stay closed.
#[test]
fn an_open_turn_of_a_thread_at_rest_reads_as_finished() {
    let mut state = fixtures::thread("tools");
    let last = state.turns.last_mut().expect("a turn");
    last.state = slopty_proto::thread::TurnState::Active;
    let mut entries = transcript::entries(&state, "w", |_| None, |_| None);
    assert!(entries.iter().any(|e| e.status == Some(MessageStatus::Streaming)));
    transcript::settle(&mut entries, true);
    assert!(entries.iter().any(|e| e.status == Some(MessageStatus::Streaming)), "live: as it is");
    transcript::settle(&mut entries, false);
    assert!(entries.iter().all(|e| e.status != Some(MessageStatus::Streaming)));
}

/// The shell may queue messages and send its chips: Slopty answers both.
#[test]
fn the_shell_is_told_what_slopty_answers() {
    let can = super::capabilities();
    for capability in [
        slopty_view_proto::capabilities::COMPOSER_REFERENCES_V1,
        slopty_view_proto::capabilities::MESSAGE_QUEUE_V1,
        slopty_view_proto::capabilities::MESSAGE_QUEUE_ACTIONS_V1,
    ] {
        assert!(can.iter().any(|c| c == capability), "{capability}");
    }
}

/// A stop leaves no mark of its own (the answer says it was stopped); what a command printed
/// or the agent said reads as words; a failure stays red.
#[test]
fn a_notice_reads_by_its_kind() {
    use slopty_proto::thread::detail::Clipped;
    use slopty_proto::thread::{ItemBody, Notice};
    let mut state = fixtures::thread("tools");
    let notice = |kind: &str| ItemBody::Notice(Notice::new(kind, Clipped::whole(kind)));
    let kinds = [Notice::INTERRUPTED, Notice::INFO, Notice::COMMAND, Notice::API_ERROR];
    let last = state.items.len().saturating_sub(kinds.len());
    let ids: Vec<String> = state.items[last..].iter().map(|item| item.id.0.clone()).collect();
    for (item, kind) in state.items[last..].iter_mut().zip(kinds) {
        item.body = notice(kind);
    }
    let entries = transcript::entries(&state, "w", |_| None, |_| None);
    let part = |id: &str| entries.iter().flat_map(|e| &e.parts).find(|p| p.id() == id).cloned();
    assert!(part(&ids[0]).is_none(), "a stop: no mark");
    assert!(matches!(part(&ids[1]), Some(MessagePart::Text { .. })));
    assert!(matches!(part(&ids[2]), Some(MessagePart::Text { .. })));
    assert!(matches!(part(&ids[3]), Some(MessagePart::Error { .. })));
}

/// A file written shows what was written, and a long output can be fetched whole.
#[test]
fn a_write_shows_its_content_and_a_long_output_its_way_in() {
    use slopty_proto::thread::detail::{Clipped, WriteDetail};
    use slopty_proto::thread::{ItemBody, ToolDetail};
    let mut state = fixtures::thread("tools");
    let thread = state.meta.id;
    let item = state
        .items
        .iter_mut()
        .find(|item| matches!(item.body, ItemBody::Tool(_)))
        .expect("a tool call");
    let id = item.id.0.clone();
    if let ItemBody::Tool(call) = &mut item.body {
        call.detail = Some(ToolDetail::Write(WriteDetail {
            path: "/w/a.txt".to_owned(),
            lines: 1,
            created: Some(true),
            patch: slopty_proto::thread::Patch::default(),
        }));
        call.input = Clipped::whole(r#"{"file_path":"/w/a.txt","content":"hello"}"#);
        call.output = Some(Clipped::whole(&"a line of output\n".repeat(200)));
    }
    let entries = transcript::entries(&state, "w", |_| None, |_| None);
    let (call, output_ref) = entries
        .iter()
        .flat_map(|e| &e.parts)
        .find_map(|p| match p {
            MessagePart::Tool { id: at, call, output_ref, .. } if *at == id => {
                Some((call.clone(), output_ref.clone()))
            }
            _ => None,
        })
        .expect("the write");
    assert!(matches!(
        call,
        slopty_view_proto::ToolCall::WriteFile { content: Some(content), .. } if content == "hello"
    ));
    assert_eq!(output_ref, Some(format!("{thread}/{id}")));
}

/// A chat is made when its thread began, and has nothing to read until a turn ended.
#[test]
fn a_chat_is_made_when_its_thread_began() {
    let state = fixtures::thread("tools");
    let mut row = state.row(slopty_core::WallMs::from_millis(1_760_000_100_000));
    row.created_ms = slopty_core::WallMs::from_millis(1_760_000_000_000);
    row.ended = None;
    let chat = rows::chat(&row, "w");
    assert_eq!(chat.created_at.timestamp_millis(), 1_760_000_000_000);
    assert_eq!(chat.last_message_at, None, "nothing to read yet");
}
