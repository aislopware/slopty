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
    let entries = transcript::entries(&state, &transcript::Look::new("w"));
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
    let item = state
        .items
        .iter_mut()
        .find(|item| matches!(item.body, ItemBody::Tool(_)))
        .expect("a tool call");
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
    let item = state
        .items
        .iter_mut()
        .find(|item| matches!(item.body, ItemBody::Tool(_)))
        .expect("a tool call");
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
            .filter(|(_, _, p)| matches!(p, MessagePart::Error { message, .. } if message == "Overloaded"))
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
    let tool = state
        .items
        .iter_mut()
        .find(|item| matches!(item.body, ItemBody::Tool(_)))
        .expect("a tool call");
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
    let said =
        super::Transcript { entries: tail, context_usage: Some(usage), history_pending: true };
    feed.transcript("c", said);
    let value = watch.recv().await.expect("the tail");
    assert_eq!(value["historyPending"], true);
    feed.transcript(
        "c",
        super::Transcript {
            entries: all.clone(),
            context_usage: Some(usage),
            history_pending: false,
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
