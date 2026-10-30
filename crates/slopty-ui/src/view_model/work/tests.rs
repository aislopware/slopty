use slopty_core::WallMs;
use slopty_proto::thread::detail::Clipped;
use slopty_proto::thread::{
    BackgroundTask, ItemId, Link, Notice, Phase, Retry, Status, ThreadState, Wait,
};
use slopty_view_doc::WorkRow;

use super::{Child, chat_row, task_row, tray};
use crate::conversation::thread::fixtures;

/// A wall time `ms` after a fixed moment well past the year 2000.
fn ms(ms: u64) -> WallMs {
    WallMs::from_millis(1_800_000_000_000_u64.saturating_add(ms))
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
    child.status = Status {
        phase,
        wait: None,
        liveness: slopty_proto::thread::Liveness::default(),
        since_ms: ms(5),
    };
    child
}

/// What is at work reaches the tray by kind and by when it began; what ended and the agent's
/// own housekeeping are left out.
#[test]
fn tasks_reach_the_tray_by_kind() {
    let mut state = fixtures::empty();
    let mut dev = task("b1", BackgroundTask::SHELL, 10);
    dev.title = "npm run dev".to_owned();
    let build = task("b2", BackgroundTask::SHELL, 20);
    let mut agent = task("a1", BackgroundTask::AGENT, 30);
    agent.step = Some("Reading main.rs".to_owned());
    let mut gone = task("m0", BackgroundTask::MONITOR, 1);
    gone.state = BackgroundTask::COMPLETED.to_owned();
    let watch = task("m1", BackgroundTask::MONITOR, 40);
    let mut memory = task("d1", BackgroundTask::AGENT, 5);
    memory.ambient = true;
    state.tasks = vec![dev, build, agent, gone, watch, memory];
    let tray = tray(&state, &[], &chat_of);
    let ids: Vec<&str> = tray.rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, ["task:b1", "task:b2", "task:a1", "task:m1"]);
    let row = |id: &str| tray.rows.iter().find(|row| row.id == task_row(id)).expect("its row");
    assert_eq!(row("b1").title, "npm run dev");
    assert_eq!((row("a1").kind.as_str(), row("a1").noun.as_str()), (WorkRow::SUBAGENT, "subagent"));
    assert_eq!(row("a1").step.as_deref(), Some("Reading main.rs"));
    assert_eq!(row("m1").state_words, "running");
}

/// A command the thread names twice (its start, then its stream) is one row, the last word on
/// it; named again as ended, it is gone.
#[test]
fn a_task_named_twice_is_one_row() {
    let mut state = fixtures::empty();
    let first = task("b1", BackgroundTask::SHELL, 10);
    let mut again = task("b1", BackgroundTask::SHELL, 10);
    again.title = "sleep 600".to_owned();
    state.tasks = vec![first.clone(), again];
    let tray = tray(&state, &[], &chat_of);
    assert_eq!(tray.rows.len(), 1);
    assert_eq!(tray.rows[0].title, "sleep 600", "the last word on it");
    let mut ended = first.clone();
    ended.state = BackgroundTask::COMPLETED.to_owned();
    state.tasks = vec![first, ended];
    assert!(super::tray(&state, &[], &chat_of).rows.is_empty(), "ended since");
}

/// A chat started under this one that waits on the person sits first, saying so; one a task
/// stands for is that task's row, not a second one.
#[test]
fn a_child_that_needs_you_sits_first() {
    let mut state = fixtures::empty();
    state.tasks = vec![task("b1", BackgroundTask::SHELL, 1)];
    let mut asking = child_of(&state, "call-7", Phase::NeedsYou);
    asking.status.wait = Some(Wait {
        kind: "permission".to_owned(),
        text: "reviewer wants to run cargo test".to_owned(),
    });
    let asking_row = asking.row(ms(9));
    let tasked = child_of(&state, "call-8", Phase::Working);
    let tasked_row = tasked.row(ms(9));
    let mut agent = task("a1", BackgroundTask::AGENT, 2);
    agent.child = Some(tasked.meta.id);
    state.tasks.push(agent);
    let children = [Child { row: &asking_row }, Child { row: &tasked_row }];
    let tray = tray(&state, &children, &chat_of);
    let first = tray.rows.first().expect("a row");
    assert_eq!(first.id, chat_row(&asking.meta.id.to_string()));
    assert_eq!(first.state_words, "needs you");
    assert_eq!(first.chat_id, Some(asking.meta.id.to_string()));
    assert_eq!(tray.rows.len(), 3, "the tasked child is its task's row alone");
    let tasked_task = tray.rows.iter().find(|row| row.id == "task:a1").expect("the task");
    assert_eq!(tasked_task.chat_id, Some(tasked.meta.id.to_string()));
}

/// A subagent's task that names no chat (one taken up again) is its chat's row still, found by
/// the call that started both: the tray lists it once.
#[test]
fn a_task_is_its_calls_chat() {
    let mut state = fixtures::empty();
    let child = child_of(&state, "call-3", Phase::Working);
    let child_row = child.row(ms(9));
    let mut agent = task("a3", BackgroundTask::AGENT, 2);
    agent.item = Some(ItemId("call-3".to_owned()));
    state.tasks = vec![agent];
    let tray = tray(&state, &[Child { row: &child_row }], &chat_of);
    assert_eq!(tray.rows.len(), 1, "{:?}", tray.rows.iter().map(|row| &row.id).collect::<Vec<_>>());
    assert_eq!(tray.rows[0].chat_id, Some(child.meta.id.to_string()));
}

/// Work a subagent started follows it; what ended is gone.
#[test]
fn a_tasks_own_work_follows_it() {
    let mut state = fixtures::empty();
    let parent = task("a1", BackgroundTask::AGENT, 1);
    let mut inner = task("b9", BackgroundTask::SHELL, 2);
    inner.parent = Some("a1".to_owned());
    let early = task("b0", BackgroundTask::SHELL, 0);
    state.tasks = vec![inner, parent, early];
    for n in 0..3 {
        let mut ended = task(&format!("x{n}"), BackgroundTask::SHELL, n);
        ended.state = BackgroundTask::FAILED.to_owned();
        ended.ended_ms = Some(ms(100_u64.saturating_add(n)));
        state.tasks.push(ended);
    }
    let tray = tray(&state, &[], &chat_of);
    let ids: Vec<&str> = tray.rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, ["task:b0", "task:a1", "task:b9"]);
    assert_eq!(tray.rows[0].kind, WorkRow::COMMAND);
}

/// A request tried again says what failed and counts down to its next attempt; the third
/// attempt of an overload points at the status page, and no answer at all says how long it
/// waited.
#[test]
fn a_retry_counts_down_to_its_next_attempt() {
    let mut notice = Notice::new(Notice::API_ERROR, Clipped::whole("529 Overloaded"));
    notice.retry = Some(Retry { attempt: 3, max: Some(10), in_ms: Some(5_000) });
    let at = super::at(ms(1_000));
    let retry = super::retry_line(&notice, at).expect("a retry");
    assert_eq!(retry.what, "Overloaded");
    assert_eq!(retry.at, Some(super::at(ms(6_000))));
    assert!(retry.hint.is_some(), "the third attempt of an overload points at the status page");
    assert_eq!(retry.words(Some(5_000)), "Overloaded · Retrying in 5s · attempt 3/10");
    let mut silent =
        Notice::new(Notice::API_ERROR, Clipped::whole("No response from the API after 1m 30s"));
    silent.retry = Some(Retry { attempt: 2, max: Some(10), in_ms: None });
    let line = super::retry_line(&silent, 1).expect("a retry");
    assert_eq!(line.what, "No response from the API after 1m 30s");
}
