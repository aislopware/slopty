use std::collections::BTreeMap;

use slopty_core::WallMs;
use slopty_proto::thread::detail::{
    AgentDetail, Clipped, ExecDetail, ExecStatus, FetchDetail, ReadDetail, SearchDetail,
};
use slopty_proto::thread::{
    BackgroundTask, CallNote, Cap, ItemId, Notice, Retry, TaskNote, TaskUsage, ThreadState,
    ToolCall, ToolDetail, ToolState, Turn, TurnId, TurnState, Usage,
};
use slopty_proto::visual::{McpApp, PageDetail, PageSource};
use slopty_view_doc::{NoteLine, NoteRow, PageCard, SubagentProgress};

use super::{ChildRun, note_row, page_card, page_key, page_of, task_part, tool_facts};
use crate::conversation::thread::fixtures;

/// A wall time `ms` after a fixed moment well past the year 2000.
fn ms(ms: u64) -> WallMs {
    WallMs::from_millis(1_800_000_000_000_u64.saturating_add(ms))
}

/// A call named `name` of `kind`, with `detail`, where it stands `state`.
fn call(name: &str, kind: &str, detail: Option<ToolDetail>, state: ToolState) -> ToolCall {
    ToolCall {
        name: name.to_owned(),
        kind: kind.to_owned(),
        title: name.to_owned(),
        input: Clipped::whole("{}"),
        state,
        output: None,
        images: Vec::new(),
        detail,
        child: None,
        ended_ms: None,
    }
}

/// The result line of `call` in an empty thread.
fn result_of(call: &ToolCall) -> Option<String> {
    tool_facts("t1", call, &fixtures::empty(), None).and_then(|facts| facts.result)
}

/// The facts the TUI's `⎿` line reads are kept: lines read, files and lines found, what a
/// fetch received, a command's exit code; a call that says nothing more has no facts.
#[test]
fn a_calls_result_reads_as_the_tuis() {
    let read = ReadDetail {
        path: "/w/a.rs".into(),
        offset: None,
        limit: None,
        lines: Some(120),
        total_lines: None,
    };
    let done = ToolState::Completed;
    assert_eq!(
        result_of(&call("Read", "read", Some(ToolDetail::Read(read)), done.clone())).as_deref(),
        Some("Read 120 lines")
    );
    let search = SearchDetail {
        pattern: "fn".into(),
        path: None,
        glob: None,
        files: Some(3),
        matches: Some(12),
        truncated: false,
    };
    assert_eq!(
        result_of(&call("Grep", "search", Some(ToolDetail::Search(search.clone())), done.clone()))
            .as_deref(),
        Some("Found 12 lines across 3 files")
    );
    // No files counted beside a line found: the files are unknown, never "across 0 files".
    let unknown = SearchDetail { files: Some(0), matches: Some(1), ..search };
    assert_eq!(
        result_of(&call("Grep", "search", Some(ToolDetail::Search(unknown)), done.clone()))
            .as_deref(),
        Some("Found 1 line")
    );
    let fetch =
        FetchDetail { url: "https://x".into(), prompt: None, code: Some(200), bytes: Some(12_600) };
    assert_eq!(
        result_of(&call("WebFetch", "fetch", Some(ToolDetail::Fetch(fetch)), done.clone()))
            .as_deref(),
        Some("Received 12.3\u{a0}KB (200 OK)")
    );
    let exec = |code: Option<i32>, status: ExecStatus, background: bool| ExecDetail {
        command: Clipped::whole("cargo test"),
        description: None,
        cwd: None,
        background,
        task: background.then(|| "bx7".to_owned()),
        status,
        exit_code: code,
        stderr: None,
        duration_ms: None,
        edits: None,
        interpretation: None,
    };
    let failed = call(
        "Bash",
        "exec",
        Some(ToolDetail::Exec(exec(Some(2), ExecStatus::Failed, false))),
        ToolState::Failed,
    );
    let facts = tool_facts("t1", &failed, &fixtures::empty(), None).expect("facts");
    assert_eq!(facts.result.as_deref(), Some("exit code 2"));
    assert!(facts.failed);
    let bg = call(
        "Bash",
        "exec",
        Some(ToolDetail::Exec(exec(None, ExecStatus::Running, true))),
        done.clone(),
    );
    let facts = tool_facts("t1", &bg, &fixtures::empty(), None).expect("facts");
    assert_eq!(facts.result.as_deref(), Some("Running in the background · task bx7"));
    assert_eq!(facts.task_row.as_deref(), Some("task:bx7"));
    let quiet = call(
        "Bash",
        "exec",
        Some(ToolDetail::Exec(exec(Some(0), ExecStatus::Done, false))),
        done.clone(),
    );
    assert_eq!(result_of(&quiet).as_deref(), Some("(No output)"));
    assert!(
        tool_facts("t1", &call("Glob", "other", None, done), &fixtures::empty(), None).is_none()
    );
    let mut no = call("Edit", "edit", None, ToolState::Rejected);
    no.input = Clipped::whole(r#"{"file_path":"/w/src/main.rs"}"#);
    let facts = tool_facts("t1", &no, &fixtures::empty(), None).expect("facts");
    assert_eq!(facts.result.as_deref(), Some("User rejected update to main.rs"));
    assert!(!facts.failed, "the person's own no reads quiet");
    assert!(facts.rejected, "it says it was turned down, for the chip's quiet header");
    assert_eq!(
        result_of(&call("Bash", "exec", None, ToolState::Rejected)).as_deref(),
        Some("Tool use rejected")
    );
    let asking = ToolState::Pending { ask: slopty_proto::thread::AskId("1".to_owned()) };
    assert_eq!(
        result_of(&call("Bash", "exec", None, asking)).as_deref(),
        Some("Waiting for permission…")
    );
    let mut grep = exec(Some(0), ExecStatus::Done, false);
    grep.interpretation = Some("No matches found".to_owned());
    let facts = tool_facts(
        "t1",
        &call("Bash", "exec", Some(ToolDetail::Exec(grep)), ToolState::Completed),
        &fixtures::empty(),
        None,
    )
    .expect("facts");
    assert_eq!(facts.result.as_deref(), Some("No matches found"));
    assert!(!facts.failed);
    let mut moved = exec(Some(0), ExecStatus::Done, false);
    moved.stderr = Some(Clipped::whole("Shell cwd was reset to /w"));
    assert_eq!(
        result_of(&call("Bash", "exec", Some(ToolDetail::Exec(moved)), ToolState::Completed))
            .as_deref(),
        Some("Shell cwd was reset to /w")
    );
    let write = slopty_proto::thread::detail::WriteDetail {
        path: "/w/notes.md".into(),
        lines: 12,
        created: Some(true),
        patch: slopty_proto::thread::detail::Patch::default(),
    };
    assert_eq!(
        result_of(&call("Write", "edit", Some(ToolDetail::Write(write)), ToolState::Completed))
            .as_deref(),
        Some("Wrote 12 lines")
    );
}

/// A running call ticks from what its agent last said; it moves to the background where the
/// agent can; its own words win; a monitor says what it started.
#[test]
fn a_running_call_has_its_timer_and_task() {
    let mut state = fixtures::empty();
    state.meta.caps = vec![Cap::named(Cap::BACKGROUND)];
    let exec = ExecDetail {
        command: Clipped::whole("sleep 60"),
        description: None,
        cwd: None,
        background: false,
        task: None,
        status: ExecStatus::Running,
        exit_code: None,
        stderr: None,
        duration_ms: None,
        edits: None,
        interpretation: None,
    };
    let mut running = call("Bash", "exec", Some(ToolDetail::Exec(exec)), ToolState::Running);
    running.input = Clipped::whole(r#"{"command":"sleep 60","timeout":120000}"#);
    state.calls = BTreeMap::from([(
        ItemId("t1".into()),
        CallNote { elapsed_ms: Some(12_000), at_ms: ms(5), ..CallNote::default() },
    )]);
    let facts = tool_facts("t1", &running, &state, None).expect("facts");
    let timer = facts.timer.expect("a timer");
    assert_eq!((timer.elapsed_ms, timer.timeout_ms), (12_000, Some(120_000)));
    assert!(facts.backgroundable);
    state.calls.get_mut(&ItemId("t1".into())).expect("its note").said =
        Some("Denied by auto mode classifier · rm is destructive".into());
    running.state = ToolState::Failed;
    let facts = tool_facts("t1", &running, &state, None).expect("facts");
    assert_eq!(facts.result.as_deref(), Some("Denied by auto mode classifier · rm is destructive"));
    assert!(facts.failed && facts.timer.is_none() && !facts.backgroundable);

    let mut monitor = call("Monitor", "other", None, ToolState::Completed);
    monitor.input = Clipped::whole(r#"{"description":"lane stall watch"}"#);
    let mut task = BackgroundTask::new("bcui", BackgroundTask::MONITOR, "lane stall watch", ms(1));
    task.item = Some(ItemId("m1".into()));
    task.timeout_ms = Some(1_800_000);
    state.tasks = vec![task];
    let facts = tool_facts("m1", &monitor, &state, None).expect("facts");
    assert_eq!(facts.title.as_deref(), Some("Monitor(lane stall watch)"));
    assert_eq!(facts.result.as_deref(), Some("Monitor started · task bcui · timeout 30m"));
}

/// A subagent's card reads its step and figures while it runs, says when it waits on the
/// person, and settles to its outcome.
#[test]
fn a_subagent_card_follows_its_run() {
    let mut state = fixtures::empty();
    let agent = AgentDetail {
        agent_type: Some("Explore".into()),
        description: Some("look".into()),
        prompt: Clipped::whole("look"),
        background: true,
        report: None,
        tokens: None,
        tool_uses: None,
        duration_ms: None,
    };
    let spawn =
        call("Agent", "agent", Some(ToolDetail::Agent(agent.clone())), ToolState::Completed);
    let mut task = BackgroundTask::new("a1", BackgroundTask::AGENT, "look", ms(1));
    task.item = Some(ItemId("t1".into()));
    task.step = Some("Running full check".into());
    task.usage = Some(TaskUsage { tokens: 41_000, tool_uses: 12, duration_ms: 1_000 });
    state.tasks = vec![task];
    let progress = |state: &ThreadState, child: Option<&ChildRun>, call: &ToolCall| {
        tool_facts("t1", call, state, child).and_then(|f| f.subagent_progress).expect("progress")
    };
    let running = progress(&state, None, &spawn);
    assert_eq!(running.state, SubagentProgress::RUNNING);
    assert_eq!(running.step.as_deref(), Some("Running full check"));
    assert_eq!(running.figures, "12 tools · 41k tokens");
    assert_eq!(running.row.as_deref(), Some("task:a1"));
    let child = ChildRun {
        chat: "c9".into(),
        running: true,
        needs_you: Some("reviewer wants to run cargo test".into()),
        ..ChildRun::default()
    };
    let asking = progress(&state, Some(&child), &spawn);
    assert_eq!(asking.state, SubagentProgress::NEEDS_YOU);
    assert_eq!(asking.chat_id.as_deref(), Some("c9"));
    state.tasks.clear();
    let ended = AgentDetail {
        tokens: Some(9_000),
        tool_uses: Some(3),
        duration_ms: Some(125_000),
        ..agent
    };
    let done = call("Agent", "agent", Some(ToolDetail::Agent(ended)), ToolState::Completed);
    let settled = progress(&state, None, &done);
    assert_eq!(settled.state, SubagentProgress::DONE);
    assert_eq!(settled.outcome.as_deref(), Some("Done"), "the time is the figures' to say");
    assert_eq!(settled.duration_ms, Some(125_000));
    assert_eq!(settled.figures, "3 tools · 9k tokens");
    assert!(!settled.running && settled.step.is_none());
}

/// A task's notice is a row linked to its tray row, with its duration and tone.
#[test]
fn a_task_notification_is_a_row_linking_its_call() {
    let note = TaskNote {
        task: "a1".into(),
        kind: BackgroundTask::AGENT.into(),
        status: TaskNote::FAILED.into(),
        title: "look".into(),
        summary: "Agent \"look\" failed".into(),
        body: Some(Clipped::whole("out of turns")),
        call: Some(ItemId("t1".into())),
        usage: Some(TaskUsage { tokens: 0, tool_uses: 4, duration_ms: 125_000 }),
        exit_code: None,
        links: Vec::new(),
    };
    let words = |text: &Clipped| text.text.clone();
    let row = task_part(&note, &fixtures::empty(), &words, &|t| t.to_string());
    assert_eq!(row.row, "task:a1");
    assert_eq!(row.text, "Agent \"look\" failed · 2m 5s");
    assert_eq!(row.tone, NoteLine::ERROR);
    assert_eq!(row.call_part_id.as_deref(), Some("t1"));
    assert_eq!(row.figures, "4 tool uses");
    assert_eq!(row.kind, "subagent");
}

/// A request tried again counts down while it is the turn's last word, reads as recovered once
/// the turn goes on, and as given up when the turn failed.
#[test]
fn a_retry_reads_trying_again_then_recovered() {
    let mut state = fixtures::empty();
    let turn = |s: TurnState| Turn {
        id: TurnId(1),
        input: None,
        state: s,
        started_ms: ms(0),
        ended_ms: None,
        usage: Usage::default(),
        models: Vec::new(),
        changed: slopty_proto::thread::Changed::default(),
        before: None,
        after: None,
    };
    state.turns = vec![turn(TurnState::Active)];
    let mut notice = Notice::new(Notice::API_ERROR, Clipped::whole("API Error: 500"));
    notice.retry = Some(Retry { attempt: 2, max: Some(10), in_ms: Some(4_000) });
    state.items = vec![slopty_proto::thread::Item {
        id: ItemId("n1".into()),
        turn: TurnId(1),
        at_ms: ms(1),
        body: slopty_proto::thread::ItemBody::Notice(notice.clone()),
    }];
    let row = |state: &ThreadState| {
        note_row(&notice, "API Error: 500".into(), "n1", TurnId(1), 7, state).expect("a note")
    };
    let trying = row(&state);
    assert_eq!(trying.kind, "retry");
    assert_eq!(trying.retry.as_ref().and_then(|r| r.at), Some(4_007));
    assert_eq!(trying.text, "API error · Retrying now · attempt 2/10");
    state.turns = vec![turn(TurnState::Complete)];
    let recovered = row(&state);
    assert!(recovered.recovered && recovered.retry.is_none());
    assert_eq!(recovered.text, "API error · recovered on attempt 2");
    assert_eq!(recovered.tone, NoteRow::SUCCESS);
    state.turns = vec![turn(TurnState::Failed { error: "500".into(), until_ms: None })];
    assert_eq!(row(&state).text, "API error · gave up after 2 attempts");
    let plain = Notice::new(Notice::API_ERROR, Clipped::whole("boom"));
    assert!(note_row(&plain, "boom".into(), "n1", TurnId(1), 0, &state).is_none(), "a failure");
    let allowed = Notice::new(Notice::ALLOWED, Clipped::whole("Allowed cargo test"));
    let note = note_row(&allowed, "Allowed cargo test".into(), "n2", TurnId(1), 0, &state);
    assert_eq!(note.map(|n| n.tone), Some(NoteLine::INFO.to_owned()));
}

/// A hook's word is a row in the turn: what it printed reads quiet, one that blocked or failed
/// warns.
#[test]
fn a_hook_says_its_word_in_the_turn() {
    let state = fixtures::empty();
    let hook = |text: &str| {
        let notice = Notice::new(Notice::HOOK, Clipped::whole(text));
        note_row(&notice, text.into(), "n3", TurnId(1), 0, &state).expect("a hook's row")
    };
    let blocked = hook("PreToolUse hook returned blocking error: no pushes to main");
    assert_eq!((blocked.kind.as_str(), blocked.tone.as_str()), ("hook", NoteLine::WARNING));
    assert_eq!(blocked.text, "PreToolUse hook returned blocking error: no pushes to main");
    assert_eq!(hook("Stop hook stopped the turn: tests fail").tone, NoteLine::WARNING);
    assert_eq!(hook("SessionStart hook: loaded 3 notes").tone, NoteLine::INFO);
}

/// A page a call shows is a card by a key of its own, read back to its call; an MCP App's names
/// its server and tool.
#[test]
fn a_shown_page_is_a_card_by_its_key() {
    let thread = fixtures::empty().meta.id;
    let html =
        PageDetail { title: "Latency".into(), source: PageSource::Html(Clipped::whole("<p>")) };
    let mut shown =
        call("mcp__slopty__show_page", "mcp", Some(ToolDetail::Page(html)), ToolState::Streaming);
    let card = page_card(thread, "t1", &shown).expect("a card");
    assert_eq!(card.kind, PageCard::HTML);
    assert!(card.streaming);
    assert_eq!(page_of(&card.page), Some((thread, "t1")));
    assert_eq!(card.page, page_key(thread, "t1"));
    let app = McpApp {
        server: "charts".into(),
        tool: "plot".into(),
        resource_uri: "ui://charts/plot".into(),
        display_mode: Some("inline".into()),
        result: None,
    };
    shown.detail =
        Some(ToolDetail::Page(PageDetail { title: String::new(), source: PageSource::App(app) }));
    shown.state = ToolState::Failed;
    let card = page_card(thread, "t1", &shown).expect("a card");
    assert_eq!((card.kind.as_str(), card.source.as_str()), (PageCard::APP, "charts · plot"));
    assert_eq!(card.title, "plot");
    assert!(card.failed);
    assert_eq!(page_of("slopty-page:nope/t1"), None);
}

/// What a command landed reads as the TUI's group line says it: a commit's sha, where a push
/// went, a rebase, a pull or merge request by its number; a dry run or a refused merge lands
/// nothing.
#[test]
fn a_command_says_what_it_landed() {
    use super::landed;
    let commit = "[main 1a2b3c4] Fix the parser\n 1 file changed, 2 insertions(+)";
    assert_eq!(landed("git commit -m 'Fix'", commit).as_deref(), Some("committed 1a2b3c4"));
    assert_eq!(
        landed("git -c user.name=x commit --amend --no-edit", commit).as_deref(),
        Some("amended commit 1a2b3c4")
    );
    let push = "To github.com:o/r.git\n   9f8e7d6..1a2b3c4  main -> main";
    assert_eq!(
        landed("git commit -m x && git push", &format!("{commit}\n{push}")).as_deref(),
        Some("committed 1a2b3c4 · pushed to main")
    );
    assert_eq!(landed("git push --dry-run", push), None);
    assert_eq!(
        landed("git rebase main", "Successfully rebased").as_deref(),
        Some("rebased onto main")
    );
    assert_eq!(landed("git merge feature", "CONFLICT (content): x"), None);
    let pr = "https://github.com/o/r/pull/12\n";
    assert_eq!(landed("gh pr create --fill", pr).as_deref(), Some("created PR #12"));
    assert_eq!(landed("gh pr merge 7 --auto", "").as_deref(), Some("enabled auto-merge on PR #7"));
    assert_eq!(
        landed("glab mr create", "https://gitlab.com/o/r/-/merge_requests/3").as_deref(),
        Some("created MR !3")
    );
    assert_eq!(landed("git status", "On branch main"), None);
}

/// A path reads as the TUI's header shows it: relative inside the chat's folder, `~/` under a
/// home, its middle cut past the budget keeping the file's name, hidden with a NUL in it; a
/// command's errors leave Claude Code's own notes out; a lone `sed -i` substitution reads as
/// an update of its file.
#[test]
fn paths_errors_and_seds_read_as_the_tuis() {
    use super::{path_words, sed_edit, stderr_words};
    assert_eq!(path_words("/w/src/main.rs", "/w"), "src/main.rs");
    assert_eq!(path_words("/Users/ann/notes/todo.md", "/w"), "~/notes/todo.md");
    assert_eq!(path_words("/etc/hosts", "/w"), "/etc/hosts");
    let deep =
        format!("/w/{}/facts.rs", ["crates", "slopty-ui", "src", "view_model"].repeat(3).join("/"));
    let cut = path_words(&deep, "/w");
    assert!(cut.chars().count() <= 48 && cut.ends_with("/facts.rs") && cut.contains('…'), "{cut}");
    assert_eq!(path_words("/w/a\0b", "/w"), "Path hidden (unsupported characters)");
    let exec = ExecDetail {
        command: Clipped::whole("cargo build"),
        description: None,
        cwd: None,
        background: false,
        task: None,
        status: ExecStatus::Failed,
        exit_code: Some(1),
        stderr: Some(Clipped::whole(
            "error: boom\n<sandbox_violations>\nread /etc\n</sandbox_violations>\nShell cwd was reset to /w\n",
        )),
        duration_ms: None,
        edits: None,
        interpretation: None,
    };
    assert_eq!(stderr_words(&exec).as_deref(), Some("error: boom"));
    assert_eq!(sed_edit("sed -i '' 's/a/b/' src/lib.rs").as_deref(), Some("Update(lib.rs)"));
    assert_eq!(sed_edit("sed -i 's/a/b/;s/c/d/' src/lib.rs"), None);
    assert_eq!(sed_edit("sed 's/a/b/' src/lib.rs"), None);
}
