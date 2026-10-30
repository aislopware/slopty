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
        turned_away: None,
    }
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
    views.work.rows = vec![slopty_view_doc::WorkRow {
        id: "task:b1".to_owned(),
        kind: slopty_view_doc::WorkRow::COMMAND.to_owned(),
        noun: "command".to_owned(),
        title: "npm run dev".to_owned(),
        state_words: "running".to_owned(),
        step: None,
        chat_id: None,
    }];
    feed.chat_views("c", views.clone());
    let value = watch.recv().await.expect("the tray");
    assert_eq!(value["rows"][0]["title"], "npm run dev");
    views.dock.prompt = Some(slopty_view_doc::PromptBox {
        text: "and then".to_owned(),
        ..slopty_view_doc::PromptBox::default()
    });
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

/// A scratch folder is no project, as written or expanded; a folder of its own is one.
#[test]
fn a_scratch_folder_is_no_project() {
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

/// A slash command reads `/name args` and a shell command `! command`, as the TUI's rows say
/// them.
#[test]
fn a_users_command_keeps_its_mark() {
    use slopty_proto::thread::ItemBody;
    use slopty_proto::thread::detail::Clipped;
    let mut state = fixtures::thread("tools");
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
    assert_eq!(said(&ids[2]).as_deref(), Some("/review"));
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
    let (call, output_ref, stats) = entries
        .iter()
        .flat_map(|e| &e.parts)
        .find_map(|p| match p {
            MessagePart::Tool { id: at, call, output_ref, diff_stats, .. } if *at == id => {
                Some((call.clone(), output_ref.clone(), diff_stats.clone()))
            }
            _ => None,
        })
        .expect("the write");
    // A new file with no diff said yet added the line it wrote: never "+0 −0".
    let stat = stats.as_ref().and_then(|s| s.first()).expect("its counts");
    assert_eq!((stat.additions, stat.deletions), (1, 0));
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
    row.title = "Read [the guide](https://example.com/guide) and **fix** `it`".to_owned();
    assert_eq!(
        rows::chat(&row, "w").title.as_deref(),
        Some("Read the guide and fix it"),
        "plain words"
    );
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
    // The TUI draws no row per task call: the last is the list as it stands.
    assert_eq!(shown.len(), 1, "the last task call");
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
    turn.state = TurnState::Failed { error: "Overloaded".to_owned() };
    let entries = transcript::entries(&state, &transcript::Look::new("w"));
    let said = errors(&entries);
    assert_eq!(said.len(), 1, "once");
    assert!(said[0].1, "at the end of its answer");
    let last = state.items.last_mut().expect("an item");
    last.body = ItemBody::Notice(Notice::new(Notice::API_ERROR, Clipped::whole("Overloaded")));
    let entries = transcript::entries(&state, &transcript::Look::new("w"));
    assert_eq!(errors(&entries).len(), 1, "the notice says it");
}

/// A Claude Code too old to drive keeps the worker's words as an error, said as a notice or as
/// the turn's failure, for the shell to draw as a banner naming the machine.
#[test]
fn a_claude_code_too_old_stays_in_the_workers_words() {
    use slopty_proto::thread::detail::Clipped;
    use slopty_proto::thread::{ItemBody, Notice};
    let said = "Update Claude Code on this machine to 2.1.301 or later: it is 2.0.14";
    assert_eq!(
        transcript::error_words("turn-failed", said, Some("studio"), None),
        (said.to_owned(), None)
    );
    let mut state = fixtures::thread("tools");
    let last = state.items.last_mut().expect("an item");
    last.body = ItemBody::Notice(Notice::new(Notice::INFO, Clipped::whole(said)));
    let entries = transcript::entries(&state, &transcript::Look::new("w"));
    let errors: Vec<&str> = entries
        .iter()
        .flat_map(|e| &e.parts)
        .filter_map(|p| match p {
            MessagePart::Error { message, .. } => Some(message.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(errors, [said]);
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

/// A picture a call returned from reading a file is called by the file's name; one of several,
/// or of a call that names no file, by its place among the call's pictures.
#[test]
fn a_picture_of_a_file_read_is_named_by_the_file() {
    use slopty_proto::thread::detail::{Clipped, ReadDetail};
    use slopty_proto::thread::{ContentRef, Image, ItemBody, ToolDetail};
    let picture = |n: u8| Image {
        digest: format!("{n:064x}"),
        media_type: "image/png".to_owned(),
        bytes: 10,
        width: 1,
        height: 1,
        at: ContentRef(format!("img:{n}")),
    };
    let names = |input: &str, images: Vec<Image>| {
        let mut state = fixtures::thread("tools");
        let tool = state.items.iter_mut().find(|item| shown_call(&item.body)).expect("a tool call");
        if let ItemBody::Tool(call) = &mut tool.body {
            call.input = Clipped::whole(input);
            call.images = images;
        }
        let entries = transcript::entries(&state, &transcript::Look::new("w"));
        entries
            .iter()
            .flat_map(|e| &e.parts)
            .filter_map(|part| match part {
                MessagePart::Image { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(names(r#"{"file_path":"/w/app/shots/login.png"}"#, vec![picture(1)]), ["login.png"]);
    assert_eq!(names(r#"{"path":"C:\\shots\\home.png"}"#, vec![picture(1)]), ["home.png"]);
    assert_eq!(
        names(r#"{"file_path":"/w/app/deck.pdf"}"#, vec![picture(1), picture(2)]),
        ["Image 1", "Image 2"],
        "pages of one file go by their place"
    );
    assert_eq!(
        names(r#"{"command":"screencapture"}"#, vec![picture(1)]),
        ["Image 1"],
        "no file named"
    );
    assert_eq!(
        names(r#"{"file_path":"/w/"}"#, vec![picture(1)]),
        ["Image 1"],
        "a folder names nothing"
    );
    // Claude's read keeps its path in the detail and no input.
    let mut state = fixtures::thread("tools");
    let tool = state.items.iter_mut().find(|item| shown_call(&item.body)).expect("a tool call");
    if let ItemBody::Tool(call) = &mut tool.body {
        call.input = Clipped::default();
        call.detail = Some(ToolDetail::Read(ReadDetail {
            path: "/work/plot.png".to_owned(),
            offset: None,
            limit: None,
            lines: None,
            total_lines: None,
        }));
        call.images = vec![picture(1)];
    }
    let entries = transcript::entries(&state, &transcript::Look::new("w"));
    let named = entries.iter().flat_map(|e| &e.parts).find_map(|part| match part {
        MessagePart::Image { name, .. } => Some(name.as_str()),
        _ => None,
    });
    assert_eq!(named, Some("plot.png"), "a read's path names its picture");
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
/// history, once here, as one reset the shell takes as history.
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
    let said = super::Transcript { entries: tail, history_pending: true };
    feed.transcript("c", said);
    let value = watch.recv().await.expect("the tail");
    assert_eq!(value["historyPending"], true);
    feed.transcript("c", super::Transcript { entries: all.clone(), history_pending: false });
    let value = watch.recv().await.expect("the whole history");
    assert!(value.get("historyPending").is_none());
    let update: TranscriptUpdate = serde_json::from_value(value).expect("the shell's frame");
    assert!(
        matches!(&update.frame, slopty_view_doc::TranscriptFrame::Reset { reset } if *reset == all)
    );
    assert!(update.replay_baseline.is_some(), "taken as history");
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

/// A failure reads in one grammar, naming its machine, with the agent's words behind it; the
/// failed last turn offers to try it again, an earlier one does not.
#[test]
fn a_failure_reads_in_one_grammar() {
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
    // An end of Claude Code's names its machine after what happened, before what to do.
    let ended = "Claude Code ended with code 1: Error: bad. Send a message to start it again.";
    assert_eq!(
        transcript::error_words(Notice::ENDED, ended, Some("studio"), None),
        (
            "Claude Code ended with code 1: Error: bad (on studio). Send a message to start it again."
                .to_owned(),
            None
        )
    );
    assert_eq!(transcript::error_words(Notice::ENDED, ended, None, None).0, ended);

    let mut state = fixtures::thread("tools");
    for turn in &mut state.turns {
        turn.state = TurnState::Failed { error: "Overloaded".to_owned() };
    }
    let entries = transcript::entries(&state, &transcript::Look::new("w").host("studio"));
    let errors: Vec<&String> = entries
        .iter()
        .flat_map(|e| &e.parts)
        .filter_map(|p| match p {
            MessagePart::Error { message, detail: Some(raw), .. } if raw == "Overloaded" => {
                Some(message)
            }
            _ => None,
        })
        .collect();
    assert_ne!(errors.len(), 0, "the failed turns say so");
    assert!(errors.iter().all(|message| *message == "The turn failed on studio. Try again."));
}

/// The figures each turn's prompt carries ([`slopty_view_doc::TurnFacts`]), by prompt id.
fn turn_figures(
    entries: &[slopty_view_doc::SessionMessageEntry],
) -> Vec<(String, slopty_view_doc::TurnFacts)> {
    entries
        .iter()
        .flat_map(|entry| entry.parts.iter().map(move |part| (entry, part)))
        .filter_map(|(entry, part)| match part {
            MessagePart::Note { note, .. } => {
                note.turn.as_deref().map(|turn| (entry.id.clone(), turn.clone()))
            }
            _ => None,
        })
        .collect()
}

/// A goal's check is one row, what it checked opening under it; a finished turn's time is said
/// once, on the prompt that opened it, and no line at its end says it again.
#[test]
fn a_goal_row_opens_its_reason_and_a_turn_says_its_time_on_its_prompt() {
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

    // Each finished turn's figures ride its prompt, its start and end from its records.
    let figures = turn_figures(&entries);
    assert!(!figures.is_empty(), "the turns' prompts carry their figures");
    for (prompt, facts) in &figures {
        let entry = entries.iter().find(|e| &e.id == prompt).expect("the prompt's entry");
        assert_eq!(entry.role, MessageRole::User, "{prompt} is a prompt");
        let turn = state
            .turns
            .iter()
            .find(|t| t.input.as_ref().is_some_and(|input| &input.0 == prompt))
            .expect("the turn it opened");
        assert_eq!(facts.started, transcript::at(turn.started_ms));
        assert_eq!(facts.ended, turn.ended_ms.map(transcript::at));
        assert_eq!(facts.models, turn.models);
    }
    // No line at a turn's end says its time again.
    let worked: Vec<&str> = entries
        .iter()
        .flat_map(|e| &e.parts)
        .filter_map(|p| match p {
            MessagePart::Note { note, .. } if note.text.starts_with("Worked for ") => {
                Some(note.text.as_str())
            }
            _ => None,
        })
        .collect();
    assert!(worked.is_empty(), "{worked:?}");
}

/// A turn that left work running says so nowhere at its end: the agent's count at the turn's
/// end ("2 background tasks still running") is no row of its own, since the Background tray
/// says what runs now, by kind, and the two disagreed. Its time is said on its prompt alone.
#[test]
fn a_finished_turn_leaves_what_still_runs_to_the_tray() {
    use slopty_proto::thread::detail::Clipped;
    use slopty_proto::thread::{ItemBody, Notice};
    let mut state = fixtures::thread("tools");
    let last = state.items.len().saturating_sub(1);
    state.items[last].body = ItemBody::Notice(Notice::new(
        Notice::TURN,
        Clipped::whole("2 background tasks still running"),
    ));
    let entries = transcript::entries(&state, &transcript::Look::new("w"));
    let notes: Vec<(&str, &str)> = entries
        .iter()
        .flat_map(|e| &e.parts)
        .filter_map(|p| match p {
            MessagePart::Note { note, .. } => Some((note.kind.as_str(), note.text.as_str())),
            _ => None,
        })
        .collect();
    assert!(notes.iter().all(|(_, text)| !text.contains("still running")), "{notes:?}");
    // Only the prompts' figures say a turn's time: one per turn a prompt opened.
    let figures = turn_figures(&entries);
    let opened = state.turns.iter().filter(|t| t.input.is_some()).count();
    assert_eq!(figures.len(), opened, "{figures:?}");
    assert!(
        figures.iter().any(|(_, facts)| facts.state == slopty_view_doc::TurnFacts::COMPLETE
            && facts.ended.is_some()),
        "{figures:?}"
    );
    assert!(notes.iter().all(|(_, text)| !text.starts_with("Worked for ")), "{notes:?}");
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

/// An error whose first line is the agent's own headline (a start it refused, a context that
/// no longer fits, a spend limit) leads with it, its machine after it, the rest as detail.
#[test]
fn an_agents_headline_leads_its_error() {
    let raw =
        "Context limit reached · compact or clear the conversation to continue\nPrompt is too long";
    let (words, detail) = transcript::error_words("turn", raw, Some("studio"), None);
    assert_eq!(
        words,
        "Context limit reached · compact or clear the conversation to continue (on studio)"
    );
    assert_eq!(detail.as_deref(), Some(raw));
}

/// A thought says how long the model thought once something follows it: from the item before
/// it in its turn to it.
#[test]
fn a_thought_says_how_long_it_took() {
    use slopty_proto::thread::detail::Clipped;
    use slopty_proto::thread::{Item, ItemBody, ItemId};
    let mut state = fixtures::thread("tools");
    let turn = state.turns.last().expect("a turn").id;
    let first = state.items.iter().position(|i| i.turn == turn).expect("an item of the turn");
    let before = state.items[first].at_ms.as_millis();
    assert!(before > 0, "the fixture's times are known");
    state.items.insert(
        first + 1,
        Item {
            id: ItemId("thought-1".to_owned()),
            turn,
            at_ms: slopty_core::WallMs::from_millis(before + 12_000),
            body: ItemBody::Reasoning(Clipped::whole("planning the change")),
        },
    );
    let entries = transcript::entries(&state, &transcript::Look::new("w"));
    let took = entries.iter().flat_map(|e| e.parts.iter()).find_map(|part| match part {
        MessagePart::Reasoning { id, thought_ms, .. } if id == "thought-1" => Some(*thought_ms),
        _ => None,
    });
    assert_eq!(took, Some(Some(12_000)));
}

/// A subagent's end that lands in the turn of the call that started it is said by its card, so
/// it draws no notice of its own; a command's end, and an agent's whose call is not in the thread,
/// keep theirs.
#[test]
fn a_subagents_end_in_its_own_turn_is_said_by_its_card() {
    use slopty_proto::thread::{BackgroundTask, Item, ItemBody, ItemId, TaskNote};
    let mut state = fixtures::thread("tools");
    let call = state
        .items
        .iter()
        .find(|item| matches!(item.body, ItemBody::Tool(_)))
        .expect("a call")
        .clone();
    let note = |n: usize, kind: &str, of: Option<ItemId>| Item {
        id: ItemId(format!("note-{n}")),
        turn: call.turn,
        at_ms: call.at_ms,
        body: ItemBody::Task(Box::new(TaskNote {
            task: format!("task-{n}"),
            kind: kind.to_owned(),
            status: TaskNote::COMPLETED.to_owned(),
            title: "Look".to_owned(),
            summary: format!("finished {n}"),
            body: None,
            call: of,
            usage: None,
            exit_code: None,
            links: Vec::new(),
        })),
    };
    let notes = [
        note(1, BackgroundTask::AGENT, Some(call.id.clone())),
        note(2, BackgroundTask::SHELL, Some(call.id.clone())),
        note(3, BackgroundTask::AGENT, None),
    ];
    state.items.extend(notes);
    let said: Vec<String> = transcript::entries(&state, &transcript::Look::new("w"))
        .iter()
        .flat_map(|entry| &entry.parts)
        .filter_map(|part| match part {
            MessagePart::Task { task, .. } if task.task.starts_with("task-") => {
                Some(task.text.clone())
            }
            _ => None,
        })
        .collect();
    // slopty: a subagent's bare notice says who, and what became of it, as the TUI does.
    assert_eq!(said, ["finished 2", "Agent \"Look\" finished"]);
}
