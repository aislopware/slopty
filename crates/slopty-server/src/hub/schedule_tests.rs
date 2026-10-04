//! A project's schedules through the hub: set by the person alone, run on the person's word
//! as when they fall due, and waking the delivery loop by their next run.

use slopty_proto::project::{ScheduleSpec, TaskSpec, TaskState};
use slopty_proto::server::Os;

use super::project_tests::{
    claude, create, opened, project, refused, request, spawn, status, worker_on,
};
use super::*;

fn nightly(paused: bool) -> ScheduleSpec {
    let task = TaskSpec {
        title: "Bump dependencies".to_owned(),
        brief: "cargo update, then the gate.".to_owned(),
        ..TaskSpec::default()
    };
    ScheduleSpec {
        task,
        launch: claude(&[]),
        when: "0 3 * * *".to_owned(),
        zone: "Asia/Ho_Chi_Minh".to_owned(),
        paused,
    }
}

/// Only the person sets or runs a schedule. One set has its next run, which the delivery
/// loop wakes for; run on the person's word it makes its task and starts it where the server
/// places it, and a second run while that task is under way is skipped, saying so on the
/// schedule. Taken away, its tasks stay.
#[tokio::test]
async fn a_schedule_is_the_person_s_and_runs_its_task_once_at_a_time() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (_linux, lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    create(&hub, None).await;
    let set = |paused| Verb::ScheduleSet {
        project: project(),
        schedule: None,
        spec: Box::new(nightly(paused)),
    };

    let by_agent = hub.dispatch_as(Speaker::Agent, None, set(false)).await;
    assert!(refused(&by_agent, ErrorCode::Forbidden).contains("only the person"));
    let run = || Verb::ScheduleRun { project: project(), schedule: 1 };
    let ran_by_agent = hub.dispatch_as(Speaker::Agent, None, run()).await;
    refused(&ran_by_agent, ErrorCode::Forbidden);

    let Outcome::Project(made) = hub.dispatch(set(false)).await else { panic!("not a project") };
    let [schedule] = &*made.project.schedules else { panic!("one schedule") };
    let next = schedule.next_ms.expect("its next run");
    let in_ms = next.millis_since(WallMs::now());
    assert!(in_ms <= 24 * 3_600_000, "within a day: {in_ms}");
    let wake = hub.deliver_due().expect("the loop wakes for it");
    let wakes_in = wake.saturating_duration_since(tokio::time::Instant::now());
    assert!(wakes_in.as_millis() <= u128::from(in_ms) + 1_000, "{wakes_in:?}");

    let asked = spawn(&hub, run());
    let start = request(&mut rx).await;
    opened(&lease, &start);
    let Outcome::Task(task) = asked.await.unwrap() else { panic!("not a task") };
    assert_eq!((task.title.as_str(), task.state), ("Bump dependencies", TaskState::Running));
    assert_eq!(task.metadata.as_deref(), Some("{\"schedule\":1}"));

    let again = hub.dispatch(run()).await;
    assert!(refused(&again, ErrorCode::Invalid).contains("still under way"));
    let last = status(&hub).await.project.schedules[0].last.clone().expect("its last run");
    assert!(last.why.is_some_and(|w| w.contains("still under way")));

    let gone = Verb::ScheduleDelete { project: project(), schedule: 1 };
    let Outcome::Project(left) = hub.dispatch(gone).await else { panic!("not a project") };
    assert_eq!(left.project.schedules, []);
    assert!(left.tasks.iter().any(|t| t.id == task.id), "its task stays");
}
