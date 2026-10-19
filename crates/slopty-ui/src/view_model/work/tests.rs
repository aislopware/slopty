use slopty_core::WallMs;
use slopty_proto::thread::detail::Clipped;
use slopty_proto::thread::wire::RequestCard;
use slopty_proto::thread::{
    AskId, BackgroundTask, Cap, ContextUsage, Item, ItemBody, ItemId, Link, Meters, Mode, Notice,
    Phase, Retry, Rule, Rules, Status, ThreadState, Turn, TurnId, TurnState, Usage,
};
use slopty_view_doc::{ModeChip, WorkRow};

use super::{Child, chat_row, dock, rules, status_spans, task_row, tray};
use crate::conversation::thread::fixtures;

/// A wall time `ms` after a fixed moment well past the year 2000.
fn ms(ms: u64) -> WallMs {
    WallMs::from_millis(1_800_000_000_000_u64.saturating_add(ms))
}

/// A thread whose agent can do `caps`.
fn thread_with(caps: &[&str]) -> ThreadState {
    let mut state = fixtures::empty();
    state.meta.caps = caps.iter().map(|cap| Cap::named(cap)).collect();
    state
}

/// A background task of `kind`.
fn task(id: &str, kind: &str, started: u64) -> BackgroundTask {
    BackgroundTask::new(id, kind, &format!("{kind} {id}"), ms(started))
}

/// The id the shell knows a thread by, in these tests its own.
fn chat_of(thread: slopty_proto::thread::ThreadId) -> String {
    thread.to_string()
}

/// A chat started under `parent`'s call `item`, where it stands `phase`.
fn child_of(parent: &ThreadState, item: &str, phase: Phase) -> ThreadState {
    let mut child = fixtures::empty();
    child.meta.parent = Some(Link { thread: parent.meta.id, item: ItemId(item.to_owned()) });
    child.meta.title = "reviewer".to_owned();
    child.meta.caps = vec![Cap::named(Cap::INTERRUPT)];
    child.status = Status {
        phase,
        wait: None,
        liveness: slopty_proto::thread::Liveness::default(),
        since_ms: ms(5),
    };
    child
}

/// Two commands and a subagent make the tray's header "2 commands · 1 subagent"; a command
/// left running beside the turn keeps a still dot while a monitor's pulses; what ended counts
/// for nothing.
#[test]
fn tasks_reach_the_tray_by_kind() {
    let mut state = thread_with(&[Cap::STOP_TASK]);
    let mut dev = task("b1", BackgroundTask::SHELL, 10);
    dev.title = "npm run dev".to_owned();
    let build = task("b2", BackgroundTask::SHELL, 20);
    let mut agent = task("a1", BackgroundTask::AGENT, 30);
    agent.agent_type = Some("Explore".to_owned());
    agent.usage = Some(slopty_proto::thread::TaskUsage {
        tokens: 41_000,
        tool_uses: 12,
        duration_ms: 125_000,
    });
    let mut gone = task("m0", BackgroundTask::MONITOR, 1);
    gone.state = BackgroundTask::COMPLETED.to_owned();
    let watch = task("m1", BackgroundTask::MONITOR, 40);
    state.tasks = vec![dev, build, agent, gone, watch];
    let tray = tray(&state, &[], &chat_of);
    assert_eq!(tray.summary, "2 commands · 1 monitor · 1 subagent");
    assert_eq!(tray.counts.total(), 4);
    let row = |id: &str| tray.rows.iter().find(|row| row.id == task_row(id)).expect("its row");
    assert!(!row("b1").wakes, "a dev server does not pulse");
    assert!(row("m1").wakes, "a monitor brings news");
    assert_eq!(row("a1").figures, "12 tool uses · 41k tokens");
    assert_eq!(row("a1").noun, "subagent");
    assert_eq!(row("m0").state_words, "done");
    assert!(row("b1").can_stop && !row("m0").can_stop);
    // Live by when they began, then what ended.
    let ids: Vec<&str> = tray.rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, ["task:b1", "task:b2", "task:a1", "task:m1", "task:m0"]);
    assert!(tray.can_stop_all);
}

/// A command the thread names twice (its start, then its stream) is one row and one in the
/// header's count: the header never says "2 commands" over one running.
#[test]
fn a_task_named_twice_counts_once() {
    let mut state = thread_with(&[Cap::STOP_TASK]);
    let first = task("b1", BackgroundTask::SHELL, 10);
    let mut again = task("b1", BackgroundTask::SHELL, 10);
    again.title = "sleep 600".to_owned();
    state.tasks = vec![first, again];
    let tray = tray(&state, &[], &chat_of);
    assert_eq!(tray.summary, "1 command");
    assert_eq!(tray.rows.len(), 1);
    assert_eq!(tray.rows[0].title, "sleep 600", "the last word on it");
}

/// Don't ask refuses what was not allowed beforehand: quiet on the chip, never amber.
#[test]
fn dont_ask_is_not_loud() {
    let mut state = thread_with(&[Cap::SET_MODE]);
    state.meta.modes = ["auto", "dontAsk"]
        .iter()
        .map(|id| Mode { id: (*id).to_owned(), label: super::humane(id), description: None })
        .collect();
    state.meters = Meters { mode: Some("dontAsk".to_owned()), ..Meters::default() };
    let counts = tray(&state, &[], &chat_of).counts;
    let chip = dock(&state, &counts, 0).mode.expect("a chip");
    assert_ne!(chip.tone, ModeChip::DANGER);
}

/// Without the agent's stop, nothing in the tray offers one.
#[test]
fn work_an_agent_cannot_stop_offers_no_stop() {
    let mut state = thread_with(&[]);
    state.tasks = vec![task("b1", BackgroundTask::SHELL, 1)];
    let tray = tray(&state, &[], &chat_of);
    assert!(!tray.rows.iter().any(|row| row.can_stop));
    assert!(!tray.can_stop_all);
}

/// A chat started under this one that waits on the person sits first, with what it asks; one
/// a task stands for is that task's row, not a second one.
#[test]
fn a_child_that_needs_you_sits_first() {
    let mut state = thread_with(&[Cap::STOP_TASK]);
    state.tasks = vec![task("b1", BackgroundTask::SHELL, 1)];
    let asking = child_of(&state, "call-7", Phase::NeedsYou);
    let mut asking_row = asking.row(ms(9));
    asking_row.requests = vec![RequestCard {
        id: AskId("r1".to_owned()),
        item: None,
        kind: "approval".to_owned(),
        title: "reviewer wants to run cargo test".to_owned(),
        options: Vec::new(),
        buttons: Vec::new(),
        opened_ms: ms(8),
        needs_presence: false,
    }];
    let tasked = child_of(&state, "call-8", Phase::Working);
    let tasked_row = tasked.row(ms(9));
    let mut agent = task("a1", BackgroundTask::AGENT, 2);
    agent.child = Some(tasked.meta.id);
    state.tasks.push(agent);
    let children = [Child { row: &asking_row }, Child { row: &tasked_row }];
    let tray = tray(&state, &children, &chat_of);
    let first = tray.rows.first().expect("a row");
    assert_eq!(first.id, chat_row(&asking.meta.id.to_string()));
    assert_eq!(first.needs_you.as_deref(), Some("reviewer wants to run cargo test"));
    assert_eq!(first.state_words, "needs you");
    assert_eq!(first.call_part_id.as_deref(), Some("call-7"));
    assert!(first.can_stop, "a child is stopped by its own interrupt");
    assert_eq!(tray.counts.needs_you, 1);
    assert_eq!(tray.rows.len(), 3, "the tasked child is its task's row alone");
    let tasked_task = tray.rows.iter().find(|row| row.id == "task:a1").expect("the task");
    assert_eq!(tasked_task.chat_id, Some(tasked.meta.id.to_string()));
}

/// A subagent's task that names no chat (one taken up again) is its chat's row still, found by
/// the call that started both: the tray lists it once.
#[test]
fn a_task_is_its_calls_chat() {
    let mut state = thread_with(&[Cap::STOP_TASK]);
    let child = child_of(&state, "call-3", Phase::Working);
    let child_row = child.row(ms(9));
    let mut agent = task("a3", BackgroundTask::AGENT, 2);
    agent.item = Some(ItemId("call-3".to_owned()));
    state.tasks = vec![agent];
    let tray = tray(&state, &[Child { row: &child_row }], &chat_of);
    assert_eq!(tray.rows.len(), 1, "{:?}", tray.rows.iter().map(|row| &row.id).collect::<Vec<_>>());
    assert_eq!(tray.rows[0].chat_id, Some(child.meta.id.to_string()));
}

/// Work a subagent started follows it, one deeper; ended rows are kept to a few.
#[test]
fn a_tasks_own_work_follows_it() {
    let mut state = thread_with(&[]);
    let parent = task("a1", BackgroundTask::AGENT, 1);
    let mut inner = task("b9", BackgroundTask::SHELL, 2);
    inner.parent = Some("a1".to_owned());
    state.tasks = vec![inner, parent];
    for n in 0..12 {
        let mut ended = task(&format!("x{n}"), BackgroundTask::SHELL, n);
        ended.state = BackgroundTask::FAILED.to_owned();
        ended.ended_ms = Some(ms(100_u64.saturating_add(n)));
        state.tasks.push(ended);
    }
    let tray = tray(&state, &[], &chat_of);
    let (first, second) = (&tray.rows[0], &tray.rows[1]);
    assert_eq!((first.id.as_str(), first.depth), ("task:a1", 0));
    assert_eq!((second.id.as_str(), second.depth), ("task:b9", 1));
    let ended = tray.rows.iter().filter(|row| !row.live).count();
    assert_eq!(ended, slopty_view_doc::WorkTray::ENDED_KEPT);
    assert_eq!(tray.rows[2].id, "task:x11", "the latest ended first");
    assert_eq!(tray.rows[2].kind, WorkRow::COMMAND);
}

/// The mode chip says the mode as the TUI does, loudest when nothing is asked; the default says
/// nothing.
#[test]
fn the_mode_chip_is_loud_for_bypass() {
    let mut state = thread_with(&[Cap::SET_MODE]);
    state.meta.modes = ["default", "acceptEdits", "plan", "bypassPermissions"]
        .iter()
        .map(|id| Mode { id: (*id).to_owned(), label: super::humane(id), description: None })
        .collect();
    state.meters = Meters { mode: Some("bypassPermissions".to_owned()), ..Meters::default() };
    let counts = tray(&state, &[], &chat_of).counts;
    let chip = dock(&state, &counts, 0).mode.expect("a chip");
    assert_eq!(chip.words, "Bypass permissions on");
    assert_eq!(chip.tone, ModeChip::DANGER);
    assert_eq!(chip.choices.len(), 4);
    state.meters.mode = Some("default".to_owned());
    let quiet = dock(&state, &counts, 0).mode.expect("a chip");
    assert_eq!((quiet.words.as_str(), quiet.tone.as_str()), ("", ModeChip::QUIET));
    assert!(dock(&state, &counts, 0).can.set_mode);
}

/// Auto, the recommended default, is quiet on the chip, and its menu row says it is recommended.
#[test]
fn the_recommended_auto_mode_is_quiet() {
    let mut state = thread_with(&[Cap::SET_MODE]);
    state.meta.modes = ["auto", "acceptEdits", "plan"]
        .iter()
        .map(|id| Mode { id: (*id).to_owned(), label: super::humane(id), description: None })
        .collect();
    state.meters = Meters { mode: Some("auto".to_owned()), ..Meters::default() };
    let counts = tray(&state, &[], &chat_of).counts;
    let chip = dock(&state, &counts, 0).mode.expect("a chip");
    assert_eq!((chip.words.as_str(), chip.tone.as_str()), ("", ModeChip::QUIET));
    let recommended: Vec<&str> =
        chip.choices.iter().filter(|c| c.recommended).map(|c| c.id.as_str()).collect();
    assert_eq!(recommended, ["auto"]);
}

/// The context card warns as the TUI does: before it compacts on its own, or when it is nearly
/// full with nothing to compact it; the last turn's tokens ride along.
#[test]
fn context_usage_keeps_its_categories_and_warns() {
    let mut state = thread_with(&[]);
    state.context = Some(ContextUsage {
        categories: vec![
            slopty_proto::thread::ContextCategory {
                name: "Messages".to_owned(),
                tokens: 140_000,
                free: false,
                color: None,
            },
            slopty_proto::thread::ContextCategory {
                name: "Free space".to_owned(),
                tokens: 60_000,
                free: true,
                color: None,
            },
        ],
        tokens: 140_000,
        window: 200_000,
        auto_compact_bp: Some(8_000),
    });
    let mut usage = Usage::default();
    usage.0.insert(Usage::OUTPUT.to_owned(), 900);
    state.turns = vec![Turn {
        id: TurnId(1),
        input: None,
        state: TurnState::Complete,
        started_ms: ms(0),
        ended_ms: Some(ms(10)),
        usage,
        models: Vec::new(),
        changed: slopty_proto::thread::Changed::default(),
        before: None,
        after: None,
    }];
    let counts = slopty_view_doc::WorkCounts::default();
    let ctx = dock(&state, &counts, 0).context.expect("a context");
    assert_eq!(ctx.used_percent, 70);
    assert_eq!(ctx.categories.len(), 2);
    assert_eq!(ctx.categories[0].share_bp, 7_000);
    assert_eq!(ctx.words.as_deref(), Some("10% until auto-compact"));
    assert!(!ctx.urgent);
    assert_eq!(ctx.last_turn.map(|t| t.output), Some(900));
    let mut full = state;
    if let Some(ctx) = full.context.as_mut() {
        ctx.auto_compact_bp = None;
        ctx.tokens = 170_000;
    }
    let low = dock(&full, &counts, 0).context.expect("a context");
    assert_eq!(
        low.words.as_deref(),
        Some("Context low (15% remaining) · Run /compact to compact & continue")
    );
    assert!(low.urgent);
}

/// While the turn's last word is a request tried again, the line under way counts down to the
/// next attempt; once anything follows, it does not.
#[test]
fn a_retry_counts_down_in_the_line_under_way() {
    let mut state = thread_with(&[]);
    state.status.phase = Phase::Working;
    state.turns = vec![Turn {
        id: TurnId(1),
        input: None,
        state: TurnState::Active,
        started_ms: ms(0),
        ended_ms: None,
        usage: Usage::default(),
        models: Vec::new(),
        changed: slopty_proto::thread::Changed::default(),
        before: None,
        after: None,
    }];
    let mut notice = Notice::new(Notice::API_ERROR, Clipped::whole("529 Overloaded"));
    notice.retry = Some(Retry { attempt: 3, max: Some(10), in_ms: Some(5_000) });
    state.items = vec![Item {
        id: ItemId("n1".to_owned()),
        turn: TurnId(1),
        at_ms: ms(1_000),
        body: ItemBody::Notice(notice),
    }];
    let counts = slopty_view_doc::WorkCounts::default();
    let retry = dock(&state, &counts, 0).activity.and_then(|a| a.retry).expect("a retry");
    assert_eq!(retry.what, "Overloaded");
    assert_eq!(retry.at, Some(super::at(ms(6_000))));
    assert!(retry.hint.is_some(), "the third attempt of an overload points at the status page");
    assert_eq!(retry.words(Some(5_000)), "Overloaded · Retrying in 5s · attempt 3/10");
    // No answer at all says how long it waited.
    let mut silent =
        Notice::new(Notice::API_ERROR, Clipped::whole("No response from the API after 1m 30s"));
    silent.retry = Some(Retry { attempt: 2, max: Some(10), in_ms: None });
    let line = super::retry_line(&silent, 1).expect("a retry");
    assert_eq!(line.what, "No response from the API after 1m 30s");
    state.items.push(Item {
        id: ItemId("t1".to_owned()),
        turn: TurnId(1),
        at_ms: ms(7_000),
        body: ItemBody::Text(Clipped::whole("back")),
    });
    assert!(dock(&state, &counts, 0).activity.and_then(|a| a.retry).is_none());
}

/// The person's status line keeps its colours as spans; links and titles are not drawn.
#[test]
fn a_status_line_keeps_its_colours() {
    let spans =
        status_spans("\u{1b}[1;31mfail\u{1b}[0m · \u{1b}[38;2;1;2;3mrgb\u{1b}]8;;https://x\u{7}");
    assert_eq!(spans.len(), 3);
    assert_eq!(spans[0].text, "fail");
    assert!(spans[0].bold);
    assert_eq!(spans[0].fg.as_deref(), Some("ansi:1"));
    assert_eq!(spans[1].text, " · ");
    assert!(spans[1].fg.is_none() && !spans[1].bold);
    assert_eq!(spans[2].fg.as_deref(), Some("#010203"));
    assert_eq!(spans[2].text, "rgb");
    assert_eq!(status_spans(""), []);
}

/// The rules read in the person's words; scopes are offered only where the agent writes them.
#[test]
fn rules_say_where_they_come_from() {
    let mut state = thread_with(&[Cap::RULES]);
    state.rules = Some(Rules {
        rules: vec![Rule {
            behavior: Rule::ALLOW.to_owned(),
            text: "Bash(cargo test:*)".to_owned(),
            source: Rule::LOCAL.to_owned(),
            description: None,
            editable: true,
            in_effect: true,
        }],
        directories: vec!["/tmp".to_owned()],
    });
    let read = rules(&state, "c1");
    assert!(read.known);
    assert_eq!(read.rules[0].source_label, "This project, this machine");
    assert_eq!(read.scopes.len(), 3);
    assert_eq!(read.scopes[0].id, Rule::LOCAL);
    let blind = rules(&thread_with(&[]), "c2");
    assert!(!blind.known && blind.scopes.is_empty());
}

/// A notification past its time is gone; a goal says what it cost since it was set; the
/// footer counts the work and the turn footer says what it waits on.
#[test]
fn the_dock_drops_what_expired_and_counts_the_work() {
    let mut state = thread_with(&[]);
    state.status.phase = Phase::Waiting;
    state.notes = vec![
        slopty_proto::thread::AgentNote {
            key: "a".to_owned(),
            text: "old".to_owned(),
            priority: "low".to_owned(),
            color: None,
            at_ms: ms(0),
            until_ms: Some(ms(10)),
        },
        slopty_proto::thread::AgentNote {
            key: "b".to_owned(),
            text: "Context low".to_owned(),
            priority: "high".to_owned(),
            color: Some("warning".to_owned()),
            at_ms: ms(0),
            until_ms: None,
        },
    ];
    state.goal = Some(slopty_proto::thread::Goal {
        condition: "tests pass".to_owned(),
        iterations: 2,
        set_ms: ms(0),
        tokens_at_start: 100,
        last_reason: None,
    });
    let mut usage = Usage::default();
    usage.0.insert(Usage::OUTPUT.to_owned(), 600);
    state.turns = vec![Turn {
        id: TurnId(1),
        input: None,
        state: TurnState::Complete,
        started_ms: ms(0),
        ended_ms: Some(ms(1)),
        usage,
        models: Vec::new(),
        changed: slopty_proto::thread::Changed::default(),
        before: None,
        after: None,
    }];
    state.tasks = vec![task("a1", BackgroundTask::AGENT, 1), task("a2", BackgroundTask::AGENT, 2)];
    let counts = tray(&state, &[], &chat_of).counts;
    let now = super::at(ms(100));
    let dock = dock(&state, &counts, now);
    assert_eq!(dock.notes.len(), 1);
    assert_eq!(dock.notes[0].tone, slopty_view_doc::NoteLine::WARNING);
    assert_eq!(dock.goal.as_ref().and_then(|g| g.tokens), Some(500));
    assert_eq!(dock.waiting.as_deref(), Some("Waiting for 2 subagents to finish"));
    assert!(dock.activity.is_none(), "no turn under way");
}

/// The side questions reach the dock as asked, answered or failed, by their intent; a chat whose
/// agent cannot take one is offered none.
#[test]
fn side_questions_reach_the_dock() {
    let mut state = thread_with(&[Cap::SIDE]);
    let intent = slopty_proto::thread::IntentId::new();
    state.side = vec![
        slopty_proto::thread::SideExchange {
            intent,
            question: "Which file?".to_owned(),
            answer: Some("`codec.rs`".to_owned()),
            failed: None,
            at_ms: ms(5),
        },
        slopty_proto::thread::SideExchange {
            intent: slopty_proto::thread::IntentId::new(),
            question: "And?".to_owned(),
            answer: None,
            failed: None,
            at_ms: ms(6),
        },
    ];
    let counts = tray(&state, &[], &chat_of).counts;
    let dock = dock(&state, &counts, 0);
    assert!(dock.can.side);
    assert_eq!(dock.side.len(), 2);
    assert_eq!(dock.side[0].id, intent.to_string());
    assert_eq!(dock.side[0].answer.as_deref(), Some("`codec.rs`"));
    assert_eq!(dock.side[0].at, super::at(ms(5)));
    assert!(dock.side[1].pending());
    assert!(!super::dock(&thread_with(&[]), &counts, 0).can.side);
}

/// The context card lists the agent's MCP servers as it last said them, a reconnect offered
/// for one that is on and not connected, and the controls only where the agent takes them.
#[test]
fn the_dock_lists_the_agents_mcp_servers() {
    let server = |name: &str, status: &str| slopty_proto::thread::McpServer {
        name: name.to_owned(),
        status: status.to_owned(),
        error: (status == "failed").then(|| "connection refused".to_owned()),
        source: Some("project".to_owned()),
    };
    let mut state = thread_with(&[Cap::MCP_SERVERS]);
    state.servers =
        vec![server("github", "connected"), server("db", "failed"), server("old", "disabled")];
    let counts = tray(&state, &[], &chat_of).counts;
    let got = dock(&state, &counts, 0);
    assert!(got.can.mcp_servers);
    let rows: Vec<(&str, bool, bool)> =
        got.servers.iter().map(|row| (row.name.as_str(), row.reconnects, row.disabled())).collect();
    assert_eq!(rows, [("github", false, false), ("db", true, false), ("old", false, true)]);
    assert_eq!(got.servers[1].error.as_deref(), Some("connection refused"));
    assert!(
        !dock(&thread_with(&[]), &counts, 0).can.mcp_servers,
        "no control the agent cannot take"
    );
}
