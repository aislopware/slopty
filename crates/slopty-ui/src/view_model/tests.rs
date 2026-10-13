use std::sync::Arc;

use chrono::Utc;
use slopty_view_doc::{MessagePart, MessageRole, MessageStatus, TranscriptUpdate};
use slopty_view_rpc::{RpcError, memory_client, methods};

use super::{Ask, ViewModel, rows, transcript};
use crate::conversation::thread::fixtures;

/// A recorded session's thread, as the shell's transcript, applied by the shell's own reducer.
#[test]
fn a_recorded_session_reads_as_the_shells_transcript() {
    let state = fixtures::thread("tools");
    let entries = transcript::entries(&state, "w", |_| None);

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
    let entries = transcript::entries(&state, "w", |_| None);
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

/// A thread's phase reads as the shell's session status.
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
        (Phase::Waiting, SessionStatus::Working),
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
    let mac = rows::device("1", "Mac", true, Utc::now());
    feed.devices(vec![mac.clone()]);
    feed.devices(vec![mac.clone()]);
    let next = devices.recv().await.expect("the change");
    let read: Vec<slopty_view_proto::Device> = serde_json::from_value(next).expect("devices");
    assert_eq!(read, vec![mac]);
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
    let all = transcript::entries(&state, "w", |_| None);
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
