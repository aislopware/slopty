use std::sync::Arc;

use chrono::Utc;
use slopty_view_doc::{MessagePart, MessageRole, MessageStatus, TranscriptUpdate};
use slopty_view_rpc::{RpcError, memory_client, methods};

use super::{Ask, ViewModel, rows, transcript};
use crate::conversation::thread::fixtures;
use crate::view_model::transcript::shown_call;

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
        ssh_host: None,
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
    let entries = transcript::entries(&state, &transcript::Look::new("w").named(named));
    assert_eq!(entries.first().map(|e| e.id.as_str()), Some("m-1"));
    let unnamed = transcript::entries(&state, &transcript::Look::new("w"));
    assert_ne!(unnamed.first().map(|e| e.id.as_str()), Some("m-1"), "not sent from here");
}

/// A recorded session's thread, as the shell's transcript, applied by the shell's own reducer.
#[test]
fn a_recorded_session_reads_as_the_shells_transcript() {
    let state = fixtures::thread("tools");
    let entries = transcript::entries(&state, &transcript::Look::new("w"));

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
    let entries = transcript::entries(&state, &transcript::Look::new("w"));
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

/// A thread's phase reads as the shell's session status, each of its own.
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
        (Phase::Waiting, SessionStatus::Waiting),
        (Phase::NeedsYou, SessionStatus::AwaitingInput),
        (Phase::Failed, SessionStatus::Errored),
        (Phase::Idle, SessionStatus::Idle),
        (Phase::Done, SessionStatus::Done),
        (Phase::Stopped, SessionStatus::Stopped),
    ] {
        assert_eq!(rows::session(&row(phase), "w", 0, now, None).status, status, "{phase:?}");
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
    let all = transcript::entries(&state, &transcript::Look::new("w"));
    let (head, _) = all.split_at(1);
    // A watch holds the latest transcript only, so each is read before the next is said.
    let mut rows = Vec::new();
    for said in [head.to_vec(), all.clone()] {
        feed.transcript("c", super::Transcript::of(said.clone()));
        let value = watch.recv().await.expect("a frame");
        let update: TranscriptUpdate = serde_json::from_value(value).expect("the shell's frame");
        slopty_view_doc::apply_transcript_frame(&mut rows, update.frame).expect("in step");
        assert_eq!(rows, said);
    }

    drop(watch);
    assert!(matches!(asks.recv().await, Some(Ask::Unfollow(chat)) if chat == "c"));
    assert_eq!(feed.followed(), Vec::<String>::new());
}

/// slopty: a chat's tray is watched as its transcript is: the watch follows the chat, says the
/// tray once it is published and again only when it changed, and lets the chat go when it closes;
/// a watch with no chat is refused.
#[tokio::test]
async fn a_tray_watch_follows_its_chat() {
    let (view_model, feed, mut asks) = ViewModel::new("me".to_owned());
    let client = memory_client(view_model);
    let params = serde_json::json!({ "chatId": "c" });
    let calls = slopty_view_doc::calls::WATCH_WORK;
    let mut watch = client.subscribe_scoped(calls, params).await.expect("the watch");
    assert!(matches!(asks.recv().await, Some(Ask::Follow(chat)) if chat == "c"));
    let mut views = super::ChatViews::default();
    views.work.summary = "1 command".to_owned();
    feed.chat_views("c", views.clone());
    let value = watch.recv().await.expect("the tray");
    assert_eq!(value["summary"], "1 command");
    views.dock.goal = None;
    feed.chat_views("c", views);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), watch.recv()).await.is_err(),
        "nothing changed in the tray"
    );
    drop(watch);
    assert!(matches!(asks.recv().await, Some(Ask::Unfollow(chat)) if chat == "c"));
    let (bare, _feed, _asks) = ViewModel::new("me".to_owned());
    let refused = slopty_view_rpc::RpcService::handle(&*bare, calls, serde_json::json!({})).await;
    assert!(matches!(refused, Err(RpcError::BadParams(_))));
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
    let none = std::collections::BTreeSet::new();
    assert_eq!(rows::running_children(parent.id, [&parent, &child].into_iter(), &none), 1);
    child.status.phase = Phase::Done;
    assert_eq!(rows::running_children(parent.id, [&parent, &child].into_iter(), &none), 0);
}

/// A subagent of a call runs inside its agent: once that agent exited it counts in nothing and
/// stands stopped, whatever its own row last said. An agent another agent started runs on its
/// own, and outlives it.
#[test]
fn a_subagent_stops_with_the_agent_it_ran_in() {
    use slopty_proto::thread::{Link, Phase};
    let state = fixtures::thread("tools");
    let parent = state.row(slopty_core::WallMs::ZERO);
    let mut child = parent.clone();
    child.id = slopty_proto::thread::ThreadId::new();
    child.parent =
        Some(Link { thread: parent.id, item: slopty_proto::thread::ItemId("call".into()) });
    child.status.phase = Phase::Working;
    let mut started = child.clone();
    started.id = slopty_proto::thread::ThreadId::new();
    started.parent = None;
    started.facts.insert(slopty_proto::agent::PARENT_FACT.to_owned(), parent.id.to_string());
    let exited = std::collections::BTreeSet::from([parent.id]);

    let rows = || [&parent, &child, &started].into_iter();
    assert_eq!(rows::running_children(parent.id, rows(), &std::collections::BTreeSet::new()), 2);
    assert_eq!(rows::running_children(parent.id, rows(), &exited), 1);
    assert_eq!(rows::standing(&child, &exited).status.phase, Phase::Stopped);
    assert_eq!(rows::standing(&started, &exited).status.phase, Phase::Working);
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
        reason: None,
        state: RequestState::Open,
        opened_ms: slopty_core::WallMs::from_millis(1),
        until_ms: None,
        needs_presence: false,
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
    let mut entries = transcript::entries(&state, &transcript::Look::new("w"));
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
        transcript::entries(state, &transcript::Look::new("w"))
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
    let mut entries = transcript::entries(&state, &transcript::Look::new("w"));
    assert!(entries.iter().any(|e| e.status == Some(MessageStatus::Streaming)));
    transcript::settle(&mut entries, true);
    assert!(entries.iter().any(|e| e.status == Some(MessageStatus::Streaming)), "live: as it is");
    transcript::settle(&mut entries, false);
    assert!(entries.iter().all(|e| e.status != Some(MessageStatus::Streaming)));
}

/// The shell may queue messages with their files kept as chips, send its chips, and start a
/// chat with no project in a scratch folder of its own: Slopty answers each. A scratch folder
/// is no project.
#[test]
fn the_shell_is_told_what_slopty_answers() {
    use slopty_view_proto::capabilities as can;
    let claimed = super::capabilities();
    for capability in [
        can::COMPOSER_REFERENCES_V1,
        can::MESSAGE_QUEUE_V1,
        can::MESSAGE_QUEUE_ACTIONS_V1,
        can::MESSAGE_QUEUE_CLEAN_ATTACHMENT_TEXT_V1,
        can::PROJECTLESS_SCRATCH_V1,
    ] {
        assert!(claimed.iter().any(|c| c == capability), "{capability}");
    }
    let scratch = slopty_view_proto::projectless::scratch_cwd("c-1", Utc::now());
    assert_eq!(rows::place(Some(&scratch), None, Some("/Users/me"), &[]), None, "as written");
    let expanded = scratch.replacen('~', "/Users/me", 1);
    assert_eq!(rows::place(Some(&expanded), None, Some("/Users/me"), &[]), None, "expanded");
    assert_eq!(
        rows::place(Some("/Users/me/dev/app"), None, Some("/Users/me"), &[]).as_deref(),
        Some("/Users/me/dev/app"),
        "a folder of its own is a project"
    );
}

/// A stop is a quiet "Interrupted" row where the turn ended; what a command printed or the
/// agent said reads as words; a failure stays red.
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
    let entries = transcript::entries(&state, &transcript::Look::new("w"));
    let part = |id: &str| entries.iter().flat_map(|e| &e.parts).find(|p| p.id() == id).cloned();
    assert!(
        matches!(part(&ids[0]), Some(MessagePart::Note { note, .. }) if note.text == "Interrupted"),
        "a stop: where the turn ended"
    );
    assert!(matches!(part(&ids[1]), Some(MessagePart::Text { .. })));
    assert!(matches!(part(&ids[2]), Some(MessagePart::Text { .. })));
    assert!(matches!(part(&ids[3]), Some(MessagePart::Error { .. })));
}

/// A slash command reads `/name args`, a skill run as one `Skill(name)`, a shell command
/// `! command`, as the TUI's rows say them; words the shell sent stay the person's own.
#[test]
fn a_users_command_keeps_its_mark() {
    use slopty_proto::thread::detail::Clipped;
    use slopty_proto::thread::{Command, ItemBody};
    let mut state = fixtures::thread("tools");
    state.commands = vec![Command {
        name: "review".to_owned(),
        description: String::new(),
        argument_hint: None,
        source: "user".to_owned(),
        skill: true,
    }];
    let last = state.items.last().cloned().expect("an item");
    let mut ids = Vec::new();
    for (n, (command, words)) in
        [("/compact", "focus on tests"), ("!", "ls -la"), ("review", "")].into_iter().enumerate()
    {
        let id = format!("u-mark-{n}");
        state.items.push(slopty_proto::thread::Item {
            id: slopty_proto::thread::ItemId(id.clone()),
            turn: last.turn,
            at_ms: last.at_ms,
            body: ItemBody::User(slopty_proto::thread::UserMessage {
                text: Clipped::whole(words),
                images: Vec::new(),
                command: Some(command.to_owned()),
                intent: None,
            }),
        });
        ids.push(id);
    }
    let entries = transcript::entries(&state, &transcript::Look::new("w"));
    let said = |id: &str| {
        entries.iter().flat_map(|e| &e.parts).find_map(|p| match p {
            MessagePart::Text { id: at, text } if *at == format!("{id}:text") => Some(text.clone()),
            _ => None,
        })
    };
    assert_eq!(said(&ids[0]).as_deref(), Some("/compact focus on tests"));
    assert_eq!(said(&ids[1]).as_deref(), Some("! ls -la"));
    assert_eq!(said(&ids[2]).as_deref(), Some("Skill(review)"));
}

/// A file written shows what was written, and a long output can be fetched whole.
#[test]
fn a_write_shows_its_content_and_a_long_output_its_way_in() {
    use slopty_proto::thread::detail::{Clipped, WriteDetail};
    use slopty_proto::thread::{ItemBody, ToolDetail};
    let mut state = fixtures::thread("tools");
    let thread = state.meta.id;
    let item = state.items.iter_mut().find(|item| shown_call(&item.body)).expect("a tool call");
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
    let entries = transcript::entries(&state, &transcript::Look::new("w"));
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

/// The first item of `state` that is the agent's own words.
fn first_text(state: &mut slopty_proto::thread::ThreadState) -> &mut slopty_proto::thread::Item {
    state
        .items
        .iter_mut()
        .find(|item| matches!(item.body, slopty_proto::thread::ItemBody::Text(_)))
        .expect("the agent's words")
}

/// The text of the part `id` among `entries`.
fn text_of(entries: &[slopty_view_doc::SessionMessageEntry], id: &str) -> Option<String> {
    entries.iter().flat_map(|e| &e.parts).find_map(|part| match part {
        MessagePart::Text { id: at, text } if at == id => Some(text.clone()),
        _ => None,
    })
}

/// A text the worker cut says how much more there is, and reads whole once its whole came.
#[test]
fn a_cut_text_reads_whole_once_its_whole_came() {
    use slopty_proto::thread::ContentRef;
    use slopty_proto::thread::detail::{Clip, Clipped};
    let mut state = fixtures::thread("tools");
    let whole = (1..=500).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
    let full = ContentRef("t1:text".to_owned());
    let item = first_text(&mut state);
    let id = item.id.0.clone();
    item.body = slopty_proto::thread::ItemBody::Text(Clipped::head(
        &whole,
        Clip { lines: 400, chars: 32_000 },
        Some(full.clone()),
    ));
    assert_eq!(transcript::cut(&state).collect::<Vec<_>>(), [&full], "worth asking for");
    let entries = transcript::entries(&state, &transcript::Look::new("w"));
    let cut = text_of(&entries, &id).expect("the words");
    assert!(cut.starts_with("line 1\n") && cut.ends_with("… 100 more lines"), "{cut}");
    let came = |content: &ContentRef| (*content == full).then(|| whole.clone());
    let entries = transcript::entries(&state, &transcript::Look::new("w").whole(came));
    assert_eq!(text_of(&entries, &id), Some(whole));
}

/// A message the shell sent reads as the person wrote it, chips and all, not in the words the
/// agent was given.
#[test]
fn a_sent_message_reads_as_the_person_wrote_it() {
    let mut state = fixtures::thread("tools");
    let sent = slopty_proto::thread::IntentId::new();
    let item = state
        .items
        .iter_mut()
        .find(|item| matches!(item.body, slopty_proto::thread::ItemBody::User(_)))
        .expect("the person's message");
    if let slopty_proto::thread::ItemBody::User(message) = &mut item.body {
        message.intent = Some(sent);
        message.text = slopty_proto::thread::Clipped::whole("Check [a.rs](src/a.rs)");
    }
    let chip = slopty_view_proto::file_mentions::local_file_link("src/a.rs", false);
    let written = format!("Check {chip}");
    let look = transcript::Look::new("w")
        .named(|intent| (intent == sent).then(|| "m-1".to_owned()))
        .said(|intent| (intent == sent).then(|| written.clone()));
    let entries = transcript::entries(&state, &look);
    assert_eq!(text_of(&entries, "m-1:text"), Some(written.clone()));
}

/// An edit offers its diff by its own ref, read from its hunks: what it replaced and what with.
#[test]
fn an_edit_offers_its_diff_by_its_ref() {
    use slopty_proto::thread::detail::{EditDetail, Hunk, Patch};
    use slopty_proto::thread::{ItemBody, ToolDetail};
    let mut state = fixtures::thread("tools");
    let thread = state.meta.id;
    let item = state.items.iter_mut().find(|item| shown_call(&item.body)).expect("a tool call");
    let id = item.id.0.clone();
    let ItemBody::Tool(call) = &mut item.body else { panic!("a tool call") };
    let hunk = Hunk {
        old_start: 3,
        old_lines: 3,
        new_start: 3,
        new_lines: 3,
        heading: None,
        lines: vec![" fn a() {".into(), "-    old();".into(), "+    new();".into(), " }".into()],
    };
    call.detail = Some(ToolDetail::Edit(EditDetail {
        path: "/w/src/a.rs".to_owned(),
        edits: 1,
        replace_all: false,
        patch: Patch { hunks: vec![hunk], added: 1, removed: 1, clipped_lines: 0, full: None },
    }));
    let call = (**call).clone();
    let entries = transcript::entries(&state, &transcript::Look::new("w"));
    let diff_ref = entries.iter().flat_map(|e| &e.parts).find_map(|part| match part {
        MessagePart::Tool { id: at, diff_ref, .. } if *at == id => diff_ref.clone(),
        _ => None,
    });
    assert_eq!(diff_ref, Some(format!("{thread}/{id}.diff")));
    let diff = transcript::tool_diff(&call, None).expect("its diff");
    assert_eq!(diff.old_text.as_deref(), Some("fn a() {\n    old();\n}\n"));
    assert_eq!(diff.new_text, "fn a() {\n    new();\n}\n");
    let whole = "--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-a\n+b\n";
    let diff = transcript::tool_diff(&call, Some(whole)).expect("from the whole");
    assert_eq!((diff.old_text.as_deref(), diff.new_text.as_str()), (Some("a\n"), "b\n"));
}

/// The files a command changed are an edit's chips under it, each with its diff by its own ref,
/// and what the agent says of the rest is a quiet row.
#[test]
fn a_commands_edits_are_chips_under_it() {
    use slopty_proto::thread::detail::{Hunk, Patch, ShellEdit, ShellEdits};
    use slopty_proto::thread::{ItemBody, ToolDetail};
    use slopty_view_proto::ToolCall;
    let mut state = fixtures::thread("tools");
    let thread = state.meta.id;
    let item = state
        .items
        .iter_mut()
        .find(|item| {
            matches!(&item.body, ItemBody::Tool(call) if matches!(call.detail, Some(ToolDetail::Exec(_))))
        })
        .expect("a command call");
    let id = item.id.0.clone();
    let ItemBody::Tool(call) = &mut item.body else { panic!("a tool call") };
    let Some(ToolDetail::Exec(exec)) = &mut call.detail else { panic!("a command") };
    let hunk = Hunk {
        old_start: 1,
        old_lines: 1,
        new_start: 1,
        new_lines: 1,
        heading: None,
        lines: vec!["-version = 1".into(), "+version = 2".into()],
    };
    let patch = |hunks: Vec<Hunk>, added: u32, removed: u32| Patch {
        hunks,
        added,
        removed,
        clipped_lines: 0,
        full: None,
    };
    let edit = ShellEdit {
        path: "Cargo.toml".into(),
        created: false,
        deleted: false,
        patch: patch(vec![hunk], 1, 1),
    };
    let made = ShellEdit {
        path: "NOTES.md".into(),
        created: true,
        deleted: false,
        patch: patch(Vec::new(), 4, 0),
    };
    exec.edits = Some(ShellEdits {
        files: vec![edit.clone(), made],
        more: 3,
        unavailable: false,
        skipped: false,
        shared: true,
        moves_tree: false,
    });
    let entries = transcript::entries(&state, &transcript::Look::new("w"));
    let parts: Vec<&MessagePart> = entries
        .iter()
        .flat_map(|e| &e.parts)
        .skip_while(|part| part.id() != id)
        .skip(1)
        .take(3)
        .collect();
    let [
        MessagePart::Tool {
            id: first,
            call: ToolCall::EditFile { path, .. },
            diff_ref,
            diff_stats,
            ..
        },
        MessagePart::Tool {
            call: ToolCall::WriteFile { path: written, .. }, diff_ref: none, ..
        },
        MessagePart::Note { note, .. },
    ] = parts.as_slice()
    else {
        panic!("two edits and a row after the command: {parts:?}");
    };
    assert_eq!(path, "Cargo.toml");
    assert_eq!(diff_ref.as_deref(), Some(format!("{thread}/{first}.diff").as_str()));
    let stats = diff_stats.as_ref().and_then(|s| s.first()).expect("its counts");
    assert_eq!((stats.additions, stats.deletions), (1, 1));
    assert_eq!(written, "NOTES.md");
    assert!(none.is_none(), "no lines came, nothing to fetch");
    assert_eq!(note.kind, "edits");
    assert_eq!(note.text, "3 more files changed");
    assert!(note.detail.as_deref().is_some_and(|d| d.contains("at the same time")), "{note:?}");
    // Its diff, by the ref the chip names.
    assert_eq!(transcript::shell_edit_of(first), Some((id.as_str(), 0)));
    let diff = transcript::shell_edit_diff(&edit, None).expect("its diff");
    assert_eq!(
        (diff.old_text.as_deref(), diff.new_text.as_str()),
        (Some("version = 1\n"), "version = 2\n")
    );
    assert_eq!(transcript::shell_edit_of("plain"), None);
}

/// The agent's task list is the shell's checklist: a change to a task names it, a task call shows
/// the list as the thread holds it, and a plan no call wrote shows at the end of the last answer.
#[test]
fn a_task_list_is_the_shells_checklist() {
    use slopty_proto::thread::{ItemBody, Plan, Step, ToolDetail};
    use slopty_view_proto::{TodoStatus, ToolCall};
    let step = |text: &str, status: &str| Step {
        id: None,
        text: text.to_owned(),
        status: status.to_owned(),
    };
    let todos =
        |entries: &[slopty_view_doc::SessionMessageEntry]| -> Vec<Vec<(String, TodoStatus)>> {
            entries
                .iter()
                .flat_map(|e| &e.parts)
                .filter_map(|part| match part {
                    MessagePart::Tool { call: ToolCall::Todo { items }, .. } => {
                        Some(items.iter().map(|i| (i.text.clone(), i.status())).collect())
                    }
                    _ => None,
                })
                .collect()
        };
    let mut state = fixtures::thread("tools");
    state.plan = None;
    let shown = todos(&transcript::entries(&state, &transcript::Look::new("w")));
    assert_eq!(shown.len(), 5, "each task call");
    assert_eq!(
        shown.get(2),
        Some(&vec![("Survey files".to_owned(), TodoStatus::InProgress)]),
        "a change names the task it changes"
    );
    state.items.retain(|item| {
        !matches!(&item.body, ItemBody::Tool(call) if call.kind == slopty_proto::thread::kind::TASKS)
    });
    state.plan = Some(Plan {
        text: None,
        steps: vec![step("Build", "completed"), step("Test", "in_progress")],
    });
    let entries = transcript::entries(&state, &transcript::Look::new("w"));
    let shown = todos(&entries);
    assert_eq!(
        shown,
        [vec![
            ("Build".to_owned(), TodoStatus::Completed),
            ("Test".to_owned(), TodoStatus::InProgress)
        ]],
        "a plan no call wrote"
    );
    let item = state.items.iter_mut().find(|item| shown_call(&item.body)).expect("a tool call");
    let ItemBody::Tool(call) = &mut item.body else { panic!("a tool call") };
    call.kind = slopty_proto::thread::kind::TASKS.to_owned();
    call.detail = Some(ToolDetail::Tasks { steps: vec![step("Test", "pending")], whole: false });
    let entries = transcript::entries(&state, &transcript::Look::new("w"));
    assert_eq!(todos(&entries).len(), 1, "the call is the checklist, said once");
    assert_eq!(todos(&entries)[0].len(), 2, "the whole list as the thread holds it");
}

/// A failed turn says its error once, at the end of its answer: not again beside a notice that
/// says it already.
#[test]
fn a_failed_turn_says_its_error_once_at_its_end() {
    use slopty_proto::thread::detail::Clipped;
    use slopty_proto::thread::{ItemBody, Notice, TurnState};
    let errors = |entries: &[slopty_view_doc::SessionMessageEntry]| -> Vec<(usize, bool)> {
        entries
            .iter()
            .flat_map(|e| e.parts.iter().enumerate().map(move |(ix, p)| (ix, ix + 1 == e.parts.len(), p)))
            .filter(|(_, _, p)| matches!(p, MessagePart::Error { detail: Some(raw), .. } if raw == "Overloaded"))
            .map(|(ix, last, _)| (ix, last))
            .collect()
    };
    let mut state = fixtures::thread("tools");
    let turn = state.turns.last_mut().expect("a turn");
    turn.state = TurnState::Failed { error: "Overloaded".to_owned(), until_ms: None };
    let entries = transcript::entries(&state, &transcript::Look::new("w"));
    let said = errors(&entries);
    assert_eq!(said.len(), 1, "once");
    assert!(said[0].1, "at the end of its answer");
    let last = state.items.last_mut().expect("an item");
    last.body = ItemBody::Notice(Notice::new(Notice::API_ERROR, Clipped::whole("Overloaded")));
    let entries = transcript::entries(&state, &transcript::Look::new("w"));
    assert_eq!(errors(&entries).len(), 1, "the notice says it");
}

/// Pictures go by paths of their own: a message's with its words, a call's as parts after it,
/// each read back by its thread, item and place.
#[test]
fn pictures_go_by_paths_of_their_own() {
    use slopty_proto::thread::{ContentRef, Image, ItemBody};
    let picture = |n: u8| Image {
        digest: format!("{n:064x}"),
        media_type: "image/png".to_owned(),
        bytes: 10,
        width: 1,
        height: 1,
        at: ContentRef(format!("img:{n}")),
    };
    let mut state = fixtures::thread("tools");
    let thread = state.meta.id;
    let user = state
        .items
        .iter_mut()
        .find(|item| matches!(item.body, ItemBody::User(_)))
        .expect("the person's message");
    let user_id = user.id.0.clone();
    if let ItemBody::User(message) = &mut user.body {
        message.images = vec![picture(1)];
    }
    let tool = state.items.iter_mut().find(|item| shown_call(&item.body)).expect("a tool call");
    let tool_id = tool.id.0.clone();
    if let ItemBody::Tool(call) = &mut tool.body {
        call.images = vec![picture(2)];
    }
    let entries = transcript::entries(&state, &transcript::Look::new("w"));
    let words = text_of(&entries, &format!("{user_id}:text")).expect("the message");
    let path = transcript::image_path(thread, &user_id, 0, &picture(1));
    assert!(words.ends_with(&format!("\n- {path}")), "{words}");
    assert_eq!(transcript::image_of(&path), Some((thread, user_id.as_str(), 0)));
    let part = entries.iter().flat_map(|e| &e.parts).find_map(|part| match part {
        MessagePart::Image { path, mime_type, .. } => Some((path.clone(), mime_type.clone())),
        _ => None,
    });
    let path = transcript::image_path(thread, &tool_id, 0, &picture(2));
    assert_eq!(part, Some((path, "image/png".to_owned())));
}

/// A visual a call showed is a part after the call once the call completed, going by a path of
/// its own beside which the shell reads its files; a call that failed (its script did not pass
/// the check) or is still running shows none.
#[test]
fn a_shown_visual_follows_its_call_by_a_path_of_its_own() {
    use std::collections::BTreeMap;

    use slopty_proto::thread::detail::Clipped;
    use slopty_proto::thread::{ItemBody, ToolDetail, ToolState};
    use slopty_proto::visual::VisualDetail;
    use transcript::VisualRead;
    let mut state = fixtures::thread("tools");
    let thread = state.meta.id;
    let tool = state.items.iter_mut().find(|item| shown_call(&item.body)).expect("a tool call");
    let id = tool.id.0.clone();
    let set = |state: &mut slopty_proto::thread::ThreadState, to: ToolState| {
        let tool = state.items.iter_mut().find(|item| item.id.0 == id).expect("the call");
        if let ItemBody::Tool(call) = &mut tool.body {
            call.state = to;
            call.detail = Some(ToolDetail::Visual(VisualDetail {
                title: "Latency by region".to_owned(),
                script: Clipped::whole("return ui.text \"hi\""),
                assets: BTreeMap::from([("prism.glb".to_owned(), "/w/prism.glb".to_owned())]),
            }));
        }
    };
    let visuals = |state: &slopty_proto::thread::ThreadState| -> Vec<(String, String, String)> {
        transcript::entries(state, &transcript::Look::new("w"))
            .iter()
            .flat_map(|e| &e.parts)
            .filter_map(|part| match part {
                MessagePart::Visual { id, path, title } => {
                    Some((id.clone(), path.clone(), title.clone()))
                }
                _ => None,
            })
            .collect()
    };
    for not_shown in [ToolState::Running, ToolState::Failed] {
        set(&mut state, not_shown.clone());
        assert!(visuals(&state).is_empty(), "{not_shown:?}");
    }
    set(&mut state, ToolState::Completed);
    let path = transcript::visual_path(thread, &id);
    assert_eq!(
        visuals(&state),
        [(format!("{id}:visual"), path.clone(), "Latency by region".to_owned())]
    );
    // The shell reads the script there, and its files beside it.
    let stem = path.strip_suffix("-visual.luau").expect("the shell's script suffix");
    assert_eq!(transcript::visual_of(&path), Some((thread, id.as_str(), VisualRead::Script)));
    assert_eq!(
        transcript::visual_of(&format!("{stem}-assets.json")),
        Some((thread, id.as_str(), VisualRead::Assets))
    );
    assert_eq!(
        transcript::visual_of(&format!("{stem}-asset-prism.glb")),
        Some((thread, id.as_str(), VisualRead::Asset("prism.glb")))
    );
    assert_eq!(transcript::visual_of("slopty-visual:nope"), None);
    assert_eq!(transcript::visual_of(&format!("{stem}.txt")), None);
}

/// While older turns are on their way the latest go as the opening tail, and the whole
/// history, once here, as one reset the shell takes as history; how full the context is rides
/// along.
#[tokio::test]
async fn older_turns_arrive_as_history() {
    let (view_model, feed, mut asks) = ViewModel::new("me".to_owned());
    let client = memory_client(view_model);
    let params = serde_json::json!({ "chatId": "c" });
    let mut watch =
        client.subscribe_scoped(methods::WATCH_DOC_MESSAGES, params).await.expect("the watch");
    assert!(matches!(asks.recv().await, Some(Ask::Follow(_))));
    let all = transcript::entries(&fixtures::thread("tools"), &transcript::Look::new("w"));
    let tail = all.get(1..).expect("more than one entry").to_vec();
    let usage = slopty_view_proto::ContextUsage { tokens: Some(50), window: Some(200) };
    let said = super::Transcript {
        entries: tail,
        context_usage: Some(usage),
        history_pending: true,
        limits: Vec::new(),
        limit_resets_at: None,
    };
    feed.transcript("c", said);
    let value = watch.recv().await.expect("the tail");
    assert_eq!(value["historyPending"], true);
    feed.transcript(
        "c",
        super::Transcript {
            entries: all.clone(),
            context_usage: Some(usage),
            history_pending: false,
            limits: Vec::new(),
            limit_resets_at: None,
        },
    );
    let value = watch.recv().await.expect("the whole history");
    assert!(value.get("historyPending").is_none());
    let update: TranscriptUpdate = serde_json::from_value(value).expect("the shell's frame");
    assert!(
        matches!(&update.frame, slopty_view_doc::TranscriptFrame::Reset { reset } if *reset == all)
    );
    assert!(update.replay_baseline.is_some(), "taken as history");
    assert_eq!(update.context_usage, Some(usage));
}

/// A chat belongs to the project folder it works in: an added one it is under, its clone's
/// for one of Slopty's worktrees, its repository's root; none at home.
#[test]
fn a_chat_belongs_to_its_project_folder() {
    let home = Some("/Users/me");
    let place = |cwd: &str, repo: Option<&str>, added: &[&str]| {
        let added: Vec<(&str, &str)> = added.iter().map(|path| (*path, *path)).collect();
        rows::place(Some(cwd), repo, home, &added)
    };
    assert_eq!(place("/w/app/src", Some("/w/app"), &[]).as_deref(), Some("/w/app"));
    assert_eq!(
        place("/w/app/.claude/worktrees/fix-ab12", Some("/w/app/.claude/worktrees/fix-ab12"), &[])
            .as_deref(),
        Some("/w/app"),
        "a worktree is its clone's"
    );
    assert_eq!(
        place("/w/mono/web/src", Some("/w/mono"), &["/w/mono/web/"]).as_deref(),
        Some("/w/mono/web"),
        "the added folder it is under"
    );
    assert_eq!(place("/Users/me", None, &[]), None, "home has no project");
    assert_eq!(place("~", None, &[]), None);
    assert_eq!(place("~/code", None, &[]).as_deref(), Some("/Users/me/code"));
    // A project added through a link holds the chats its machine names by the real folder.
    let linked = [("/Users/me/work", "/Volumes/Data/work")];
    assert_eq!(
        rows::place(Some("/Volumes/Data/work/api"), Some("/Volumes/Data/work"), home, &linked)
            .as_deref(),
        Some("/Users/me/work"),
        "the added folder, by where it really is"
    );
    let mut row = fixtures::thread("tools").row(slopty_core::WallMs::ZERO);
    row.repo_id = Some(slopty_proto::terminal::RepoId {
        origin: Some("github.com/o/r".to_owned()),
        root: Some("abc".to_owned()),
        url: None,
    });
    assert_eq!(rows::repository_id(&row).as_deref(), Some("commit:abc"), "clones group by it");
}

/// A thread waiting on its own background work reads as waiting, since when, and on what, with
/// no live indicator (so no Stop); one that needs the person says which request, its words cut
/// to a line with what would hide or reorder them taken out.
#[test]
fn a_waiting_thread_reads_as_waiting_and_offers_no_stop() {
    use slopty_proto::thread::{Phase, Wait};
    use slopty_view_proto::view::{Indicator, effective_indicator};
    use slopty_view_proto::{SessionStatus, SessionWait};
    let state = fixtures::thread("tools");
    let since = slopty_core::WallMs::from_millis(1_760_000_000_000);
    let mut row = state.row(slopty_core::WallMs::ZERO);
    row.status.phase = Phase::Waiting;
    row.status.since_ms = since;
    row.status.wait = Some(Wait { kind: Wait::TASK.to_owned(), text: "1 subagent".to_owned() });
    let now = Utc::now();
    let session = rows::session(&row, "w", 1, now, None);
    assert_eq!(session.status, SessionStatus::Waiting);
    assert_eq!(session.status_since, Some(rows::time(since)));
    assert_eq!(session.waiting_on.as_ref().map(|w| w.kind.as_str()), Some(Wait::TASK));
    assert_eq!(effective_indicator(Some(&session), now), Indicator::None, "nothing to stop");

    row.status.phase = Phase::NeedsYou;
    let long = format!("Wants to run\n\tcargo test \u{202E}{}", "x".repeat(80));
    row.status.wait = Some(Wait { kind: "permission".to_owned(), text: long });
    let asking = rows::session(&row, "w", 0, now, None);
    assert_eq!(asking.status, SessionStatus::AwaitingInput);
    let wait = asking.waiting_on.expect("what it waits on");
    assert_eq!(wait.kind, SessionWait::APPROVAL);
    let summary = wait.summary.expect("its line");
    assert!(summary.starts_with("Wants to run cargo test x"), "{summary}");
    assert!(!summary.contains('\u{202E}'), "no bidi");
    assert_eq!(summary.chars().count(), 61, "60 and the ellipsis");
}

/// A turn stands as of its worker's last word that its agent lives: a lease 50 s old reads as
/// no longer known to run, a fresh one as working, whatever this client heard last.
#[test]
fn a_turn_whose_lease_is_old_goes_stale() {
    use slopty_proto::thread::Phase;
    use slopty_view_proto::view::{Indicator, effective_indicator};
    let state = fixtures::thread("tools");
    let now = Utc::now();
    let mut row = state.row(slopty_core::WallMs::ZERO);
    row.status.phase = Phase::Working;
    row.lease_ms = Some(slopty_core::WallMs::from_millis(1));
    let stale = rows::session(&row, "w", 0, now, Some(now - chrono::TimeDelta::seconds(50)));
    assert_eq!(effective_indicator(Some(&stale), now), Indicator::None, "its lease ran out");
    let fresh = rows::session(&row, "w", 0, now, Some(now - chrono::TimeDelta::seconds(5)));
    assert_eq!(effective_indicator(Some(&fresh), now), Indicator::Working);
    let heard = rows::session(&row, "w", 0, now, None);
    assert_eq!(heard.updated_at, now, "no lease: as heard");
}

/// The plan's rate windows ride the transcript's frames, the most used first, and a change in
/// them alone is a frame.
#[tokio::test]
async fn the_plans_rate_windows_ride_the_transcript() {
    use slopty_proto::thread::Limit;
    let windows = |five: u32, week: u32| {
        rows::limits(&[
            Limit { name: "seven-day".to_owned(), used_bp: week, resets_ms: None },
            Limit {
                name: "five-hour".to_owned(),
                used_bp: five,
                resets_ms: Some(slopty_core::WallMs::from_millis(1_760_000_000_000)),
            },
        ])
    };
    let first = windows(9_000, 2_500);
    assert_eq!(first[0].label, "Session");
    assert!((first[0].used_fraction - 0.9).abs() < 1e-6, "most used first");
    assert_eq!(first[1].label, "Week");

    let (view_model, feed, mut asks) = ViewModel::new("me".to_owned());
    let client = memory_client(view_model);
    let mut watch = client
        .subscribe_scoped(methods::WATCH_DOC_MESSAGES, serde_json::json!({ "chatId": "c" }))
        .await
        .expect("the watch");
    // The watch is open once the workspace is asked to follow the chat.
    assert!(matches!(asks.recv().await, Some(Ask::Follow(_))));
    let entries = transcript::entries(&fixtures::thread("tools"), &transcript::Look::new("w"));
    let said = |limits| super::Transcript { limits, ..super::Transcript::of(entries.clone()) };
    feed.transcript("c", said(first.clone()));
    let update: TranscriptUpdate =
        serde_json::from_value(watch.recv().await.expect("a frame")).expect("the shell's frame");
    assert_eq!(update.limits, first);
    let second = windows(9_500, 2_500);
    feed.transcript("c", said(second.clone()));
    let update: TranscriptUpdate =
        serde_json::from_value(watch.recv().await.expect("a frame")).expect("the shell's frame");
    assert_eq!(update.limits, second, "the windows moved: a frame of its own");
}

/// A plan reads as its own card in the transcript, its markdown whole, and the question panel
/// asks only for its choices.
#[test]
fn a_plan_reads_as_markdown_and_asks_only_its_choices() {
    use slopty_proto::thread::detail::Clipped;
    use slopty_proto::thread::{Effect, ItemId, Request};
    let mut state = fixtures::thread("tools");
    let call = state
        .items
        .iter()
        .find(|item| shown_call(&item.body))
        .map(|item| item.id.clone())
        .expect("a call");
    let mut plan = request(
        Request::PLAN,
        &[("yes", "Approve", Effect::Allow), ("no", "Keep planning", Effect::Deny)],
    );
    plan.item = Some(ItemId(call.0.clone()));
    plan.text = Some(Clipped::whole("# Plan\n\n1. Read\n2. Write"));
    state.requests.push(plan);
    let mut entries = transcript::entries(&state, &transcript::Look::new("w"));
    let at = entries.iter().position(|e| e.id == "plan-card:ask-1").expect("its card");
    let after_call = entries
        .iter()
        .position(|e| e.parts.iter().any(|p| p.id() == call.0))
        .expect("the call's answer");
    assert!(at > after_call, "after the call that asked");
    assert!(matches!(
        &*entries[at].parts,
        [MessagePart::Text { id, text }] if id == "plan:ask-1" && text.starts_with("# Plan")
    ));
    transcript::asks(&state, "w", &mut entries);
    let (_, questions) = served(&entries).expect("the panel serves it");
    assert_eq!(questions[0].header, "Plan ready");
    assert!(questions[0].question.is_empty(), "its text is the card's");
    assert_eq!(questions[0].options, ["Approve", "Keep planning"]);
}

/// What another agent told this one reads as a message from that agent, and a child's news as
/// one quiet line; nothing of Slopty's frame shows.
#[test]
fn an_agents_words_read_as_from_that_agent() {
    use slopty_proto::thread::detail::Clipped;
    use slopty_proto::thread::{ItemBody, Notice};
    let mut state = fixtures::thread("tools");
    let report = "<slopty-reports>\nThe person says:\n  ship it\nAgent ui-lane (t-7) says (an \
                  agent, not the person):\n  done with the sidebar\n  tests pass\nui-lane came to \
                  rest\n</slopty-reports>";
    let last = state.items.last_mut().expect("an item");
    last.body = ItemBody::Notice(Notice::new(Notice::REPORT, Clipped::whole(report)));
    let id = last.id.0.clone();
    let entries = transcript::entries(&state, &transcript::Look::new("w"));
    let of = |n: usize| entries.iter().find(|e| e.id == format!("{id}:report:{n}")).expect("one");
    let text = |n: usize| match &*of(n).parts {
        [MessagePart::Text { text, .. }] => text.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(of(0).role, MessageRole::User);
    assert_eq!(text(0), "ship it");
    assert_eq!(of(1).role, MessageRole::User);
    assert!(text(1).starts_with("[Message from Slopty chat ui-lane (t-7)."), "{}", text(1));
    assert!(text(1).ends_with("]\n\ndone with the sidebar\ntests pass"));
    assert!(matches!(
        &*of(2).parts,
        [MessagePart::Marker { text, .. }] if text == "ui-lane came to rest"
    ));
    let all = format!("{:?}", entries.iter().flat_map(|e| &e.parts).collect::<Vec<_>>());
    assert!(!all.contains("slopty-reports"), "no frame shows");
}

/// A chat forked off another shows the seam after the turns it shares, naming its source.
#[test]
fn a_fork_shows_where_it_was_cut() {
    use slopty_proto::thread::Fork;
    let mut state = fixtures::thread("tools");
    let source = slopty_proto::thread::ThreadId::new();
    let first = state.turns.first().map(|turn| turn.id).expect("a turn");
    state.meta.forked_from = Some(Fork { thread: source, turn: Some(first) });
    let look = transcript::Look::new("w")
        .source(|thread| (thread == source).then(|| ("c-src".to_owned(), "Sidebar".to_owned())));
    let entries = transcript::entries(&state, &look);
    let seam = entries
        .iter()
        .position(|e| matches!(&*e.parts, [MessagePart::Fork { .. }]))
        .expect("the seam");
    assert!(matches!(
        &*entries[seam].parts,
        [MessagePart::Fork { source_chat_id, source_title, .. }]
            if source_chat_id == "c-src" && source_title == "Sidebar"
    ));
    // Everything of the turns it shares is above the seam, everything after below.
    let holds = |entry: &slopty_view_doc::SessionMessageEntry, item: &str| {
        entry.id == item || entry.parts.iter().any(|p| p.id().split(':').next() == Some(item))
    };
    for item in &state.items {
        let Some(at) = entries.iter().position(|e| holds(e, &item.id.0)) else { continue };
        assert_eq!(at < seam, item.turn <= first, "{} of {:?}", item.id.0, item.turn);
    }
    let plain = transcript::entries(&fixtures::thread("tools"), &transcript::Look::new("w"));
    assert!(!plain.iter().any(|e| matches!(&*e.parts, [MessagePart::Fork { .. }])));
}

/// A compaction leaves a row saying what it did, its summary under it, and a thread compacting
/// says so.
#[test]
fn a_compaction_shows_while_it_runs_and_leaves_a_marker() {
    use slopty_proto::thread::{Compaction, ItemBody, Phase, Wait};
    let mut state = fixtures::thread("tools");
    let last = state.items.last_mut().expect("an item");
    last.body = ItemBody::Compaction(Compaction {
        trigger: Some("auto".to_owned()),
        before_tokens: Some(143_200),
        after_tokens: Some(12_000),
        summary: Some(slopty_proto::thread::detail::Clipped::whole("Fixed the build.")),
    });
    let id = last.id.0.clone();
    let entries = transcript::entries(&state, &transcript::Look::new("w"));
    let marker = entries.iter().find(|e| e.id == id).expect("its row");
    assert_eq!(marker.role, MessageRole::System);
    assert!(matches!(
        &*marker.parts,
        [MessagePart::Note { note, .. }] if note.text == "Compacted · 143k tokens → summary"
            && note.detail.as_deref() == Some("Fixed the build.")
    ));
    let mut row = state.row(slopty_core::WallMs::ZERO);
    row.status.phase = Phase::Working;
    row.status.wait = Some(Wait { kind: Wait::COMPACTING.to_owned(), text: String::new() });
    let session = rows::session(&row, "w", 0, Utc::now(), None);
    let wait = session.waiting_on.expect("compacting");
    assert_eq!(wait.kind, slopty_view_proto::SessionWait::COMPACTING);
    assert_eq!(wait.summary, None);
}

/// The fixture's first command call made to run `command`, and an approval of it.
fn approval_of(
    state: &mut slopty_proto::thread::ThreadState,
    command: &str,
) -> slopty_proto::thread::Request {
    use slopty_proto::thread::detail::Clipped;
    use slopty_proto::thread::{Effect, ItemBody, ToolDetail};
    let item = state
        .items
        .iter_mut()
        .find(|item| {
            matches!(&item.body, ItemBody::Tool(call) if matches!(call.detail, Some(ToolDetail::Exec(_))))
        })
        .expect("a command call");
    if let ItemBody::Tool(call) = &mut item.body
        && let Some(ToolDetail::Exec(exec)) = &mut call.detail
    {
        exec.command = Clipped::whole(command);
    }
    let mut approval = request(
        slopty_proto::thread::Request::APPROVAL,
        &[("allow", "Allow", Effect::Allow), ("deny", "Deny", Effect::Deny)],
    );
    approval.item = Some(item.id.clone());
    approval.text = Some(Clipped::whole(command));
    approval
}

/// The panel's one question for `state` with `approval` open.
fn asked_of(
    state: &mut slopty_proto::thread::ThreadState,
    approval: slopty_proto::thread::Request,
) -> slopty_view_proto::UserInputQuestion {
    state.requests = vec![approval];
    let mut entries = transcript::entries(state, &transcript::Look::new("w"));
    transcript::asks(state, "w", &mut entries);
    served(&entries).expect("the panel serves it").1.remove(0)
}

/// A long command starts on its deny, takes no key and is read through before it can be
/// allowed; a destructive one starts on its deny too; a plain read is allowed at a key.
#[test]
fn a_long_command_defaults_to_deny_and_ignores_enter() {
    let mut state = fixtures::thread("tools");
    let long = format!("echo {}", "a".repeat(700));
    let approval = approval_of(&mut state, &long);
    let asked = asked_of(&mut state, approval);
    assert!(asked.deny_first && asked.click_only && asked.read_gate, "{asked:?}");
    assert_eq!(asked.deny_option, Some(1), "Deny");

    let approval = approval_of(&mut state, "rm -rf target");
    let asked = asked_of(&mut state, approval);
    assert!(asked.deny_first && asked.click_only && !asked.read_gate, "{asked:?}");

    let approval = approval_of(&mut state, "git status");
    let asked = asked_of(&mut state, approval);
    assert!(!asked.deny_first && !asked.click_only && !asked.read_gate, "{asked:?}");

    let approval = approval_of(&mut state, "cargo test");
    let asked = asked_of(&mut state, approval);
    assert!(!asked.deny_first && asked.click_only, "not a known read: a click");

    state.meters.mode = Some("plan".to_owned());
    let approval = approval_of(&mut state, "git status");
    assert!(asked_of(&mut state, approval).click_only, "in plan mode: a click");
}

/// A request only the person at its card answers is served to the card as one: the card says
/// "Needs you at the card".
#[test]
fn a_request_needing_the_person_reaches_its_card_as_one() {
    let mut state = fixtures::thread("tools");
    let approval = approval_of(&mut state, "git status");
    assert!(!asked_of(&mut state, approval).needs_presence);
    let mut approval = approval_of(&mut state, "git status");
    approval.needs_presence = true;
    assert!(asked_of(&mut state, approval).needs_presence);
}

/// An approval says why the agent asks, in its words.
#[test]
fn an_approval_says_why_it_asks() {
    let mut state = fixtures::thread("tools");
    let mut approval = approval_of(&mut state, "rm -rf target");
    approval.reason = Some("Permission rule `Bash(rm:*)` requires confirmation".to_owned());
    let asked = asked_of(&mut state, approval);
    assert_eq!(asked.reason.as_deref(), Some("Permission rule `Bash(rm:*)` requires confirmation"));
    let approval = approval_of(&mut state, "git status");
    assert_eq!(asked_of(&mut state, approval).reason, None);
}

/// An approval shows a bidi override or a zero-width character as its code, and its answer
/// still goes as the agent's own choice.
#[test]
fn an_approval_shows_bidi_overrides_as_visible_escapes() {
    let mut state = fixtures::thread("tools");
    let mut approval = approval_of(&mut state, "cat notes\u{202E}txt.sh");
    approval.options[0].label = "Allow \u{200B}once".to_owned();
    let asked = asked_of(&mut state, approval.clone());
    assert_eq!(asked.question, "cat notes[U+202E]txt.sh");
    assert_eq!(asked.options[0], "Allow [U+200B]once");
    assert_eq!(
        transcript::answer(&approval, &[picked(&asked.id, &asked.options[0])]),
        Ok(("allow".to_owned(), None))
    );
}

/// An approval of an edit carries the change it would make.
#[test]
fn an_edit_approval_shows_its_diff() {
    use slopty_proto::thread::{Effect, ItemBody, Request, ToolDetail};
    let mut state = fixtures::thread("edit");
    let edit = state
        .items
        .iter()
        .find(|item| {
            matches!(&item.body, ItemBody::Tool(call) if matches!(call.detail, Some(ToolDetail::Edit(_))))
        })
        .map(|item| item.id.clone())
        .expect("an edit");
    let mut approval = request(
        Request::APPROVAL,
        &[("allow", "Allow", Effect::Allow), ("deny", "Deny", Effect::Deny)],
    );
    approval.item = Some(edit);
    let asked = asked_of(&mut state, approval);
    let diff = asked.proposed.expect("its change");
    assert_ne!(diff.path, "");
    assert!(!diff.new_text.is_empty() || diff.old_text.is_some());
}

/// A failure reads in one grammar, naming its machine, with the agent's words behind it; the
/// failed last turn offers to try it again, an earlier one does not.
#[test]
fn a_failure_reads_in_one_grammar_and_offers_retry() {
    use slopty_proto::thread::{Notice, TurnState};
    let (copy, raw) =
        transcript::error_words(Notice::API_ERROR, "529 Overloaded", Some("studio"), None);
    assert_eq!(copy, "The model's service returned an error on studio. Try again.");
    assert_eq!(raw.as_deref(), Some("529 Overloaded"));
    for kind in [Notice::LIMIT, Notice::DECLINED, Notice::HOOK] {
        let (copy, raw) = transcript::error_words(kind, "said", None, None);
        assert_ne!(copy, "said", "{kind}");
        assert_eq!(raw.as_deref(), Some("said"));
    }
    assert_eq!(transcript::error_words("something-new", "as said", None, None).0, "as said");

    let mut state = fixtures::thread("tools");
    for turn in &mut state.turns {
        turn.state = TurnState::Failed { error: "Overloaded".to_owned(), until_ms: None };
    }
    let entries = transcript::entries(&state, &transcript::Look::new("w").host("studio"));
    let errors: Vec<(String, bool)> = entries
        .iter()
        .flat_map(|e| &e.parts)
        .filter_map(|p| match p {
            MessagePart::Error { message, detail: Some(raw), retry, .. } if raw == "Overloaded" => {
                Some((message.clone(), *retry))
            }
            _ => None,
        })
        .collect();
    assert_ne!(errors.len(), 0, "the failed turns say so");
    assert!(errors.iter().all(|(message, _)| message == "The turn failed on studio. Try again."));
    assert_eq!(errors.iter().filter(|(_, retry)| *retry).count(), 1, "the last turn's alone");
    assert!(errors.last().is_some_and(|(_, retry)| *retry));
}

/// The files on their way to a machine are watched by their upload ids.
#[tokio::test]
async fn a_files_way_up_is_watched_by_its_upload() {
    let (view_model, feed, _asks) = ViewModel::new("me".to_owned());
    let client = memory_client(view_model);
    let mut watch = client
        .subscribe_scoped(methods::WATCH_TRANSFERS, serde_json::json!({}))
        .await
        .expect("the watch");
    assert_eq!(watch.recv().await, Some(serde_json::json!([])));
    let going = |done| slopty_view_proto::TransferProgress {
        upload_id: "u-1".to_owned(),
        file_name: "shot.png".to_owned(),
        done,
        total: 4_000_000,
    };
    feed.transfers(vec![going(1_000_000)]);
    let read: Vec<slopty_view_proto::TransferProgress> =
        serde_json::from_value(watch.recv().await.expect("a frame")).expect("transfers");
    assert_eq!(read, vec![going(1_000_000)]);
}

/// Claude Code's "Yes, and tell Claude what to do next": an approval says which choice takes the
/// person's words beside it, and the pick with words goes back as that allow and the words; a
/// question's option previews ride beside its options.
#[test]
fn an_allow_takes_words_and_options_carry_previews() {
    use slopty_proto::thread::Effect;
    use slopty_proto::thread::detail::{Offered, Question};
    let mut state = fixtures::thread("tools");
    let approval = request(
        slopty_proto::thread::Request::APPROVAL,
        &[("allow", "Yes", Effect::Allow), ("deny", "No", Effect::Deny)],
    );
    state.requests.push(approval.clone());
    let mut entries = transcript::entries(&state, &transcript::Look::new("w"));
    transcript::asks(&state, "w", &mut entries);
    let (_, questions) = served(&entries).expect("the panel serves it");
    assert_eq!(questions[0].allow_option, Some(0));
    let with_words = slopty_view_proto::UserInputAnswer {
        question_id: questions[0].id.clone(),
        labels: vec!["Yes".to_owned(), "then commit".to_owned()],
    };
    assert_eq!(
        transcript::answer(&approval, &[with_words]),
        Ok(("allow".to_owned(), Some("then commit".to_owned())))
    );

    let mut asked = request(slopty_proto::thread::Request::QUESTION, &[]);
    let offered = |label: &str, preview: Option<&str>| Offered {
        label: label.to_owned(),
        description: None,
        preview: preview.map(str::to_owned),
    };
    asked.questions = vec![Question {
        header: Some("Layout".to_owned()),
        text: "Which layout?".to_owned(),
        options: vec![offered("Grid", Some("[ ][ ]\n[ ][ ]")), offered("List", None)],
        multi_select: false,
    }];
    state.requests = vec![asked];
    let mut entries = Vec::new();
    transcript::asks(&state, "w", &mut entries);
    let (_, questions) = served(&entries).expect("the panel serves it");
    assert_eq!(questions[0].option_previews, ["[ ][ ]\n[ ][ ]", ""]);
    assert_eq!(questions[0].allow_option, None, "a question has no allow");
}

/// A goal's check is one row, what it checked opening under it; a finished turn ends on the
/// TUI's done line with its plain verb.
#[test]
fn a_goal_row_opens_its_reason_and_a_turn_ends_on_its_done_line() {
    use slopty_proto::thread::detail::Clipped;
    use slopty_proto::thread::{ItemBody, Notice};
    let mut state = fixtures::thread("tools");
    let last = state.items.len().saturating_sub(1);
    let id = state.items[last].id.0.clone();
    state.items[last].body = ItemBody::Notice(Notice::new(
        Notice::GOAL,
        Clipped::whole("Goal not yet met… continuing\nReason: 2 tests fail"),
    ));
    let entries = transcript::entries(&state, &transcript::Look::new("w"));
    let part = entries.iter().flat_map(|e| &e.parts).find(|p| p.id() == id).cloned();
    let Some(MessagePart::Note { note, .. }) = part else { panic!("the goal's row: {part:?}") };
    assert_eq!(note.text, "Goal not yet met… continuing");
    assert_eq!(note.detail.as_deref(), Some("Reason: 2 tests fail"));
    assert_eq!(note.tone, slopty_view_doc::NoteLine::INFO);

    let done: Vec<&str> = entries
        .iter()
        .flat_map(|e| &e.parts)
        .filter_map(|p| match p {
            MessagePart::Note { note, .. } if note.kind == crate::view_model::folds::DONE => {
                Some(note.text.as_str())
            }
            _ => None,
        })
        .collect();
    assert!(!done.is_empty() && done.iter().all(|d| d.starts_with("Worked for ")), "{done:?}");
}

/// An agent that is not signed in says so where it runs, its own words behind it; any other
/// failure of the model's service stays the general one.
#[test]
fn a_signed_out_agent_says_so() {
    use slopty_proto::thread::Notice;
    let (words, detail) = transcript::error_words(
        Notice::API_ERROR,
        "Not logged in · Please run /login",
        Some("mac"),
        None,
    );
    assert_eq!(words, "The agent is not signed in on mac. Sign it in there, then try again.");
    assert_eq!(detail.as_deref(), Some("Not logged in · Please run /login"));
    let (words, _) = transcript::error_words(Notice::API_ERROR, "Overloaded", None, None);
    assert_eq!(words, "The model's service returned an error. Try again.");
}
