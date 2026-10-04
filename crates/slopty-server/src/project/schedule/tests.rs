use std::collections::HashSet;

use slopty_core::{SessionId, WorkerId};
use slopty_proto::orchestration::{Outcome, TermRef};
use slopty_proto::project::{LimitsChange, Runner, TaskSpec};

use super::*;
use crate::project::NewProject;

/// 2026-10-05, a Monday, 08:00 UTC: 10:00 in Berlin.
fn monday() -> WallMs {
    WallMs::from_millis(1_791_187_200_000)
}

fn hours(n: u64) -> WallMs {
    WallMs::from_millis(monday().as_millis().saturating_add(n.saturating_mul(3_600_000)))
}

fn id() -> ProjectId {
    ProjectId::new("demo").unwrap()
}

fn none() -> HashSet<TermRef> {
    HashSet::new()
}

fn projects() -> Projects {
    let mut p = Projects::default();
    let none = none();
    let running = Running { terminals: &none, agents: &none, starting: &[] };
    let new = NewProject {
        id: id(),
        title: "Demo".to_owned(),
        repo: "demo".to_owned(),
        target: "main".to_owned(),
        review: None,
        verifier: None,
        push: false,
        ask_to_start: false,
        orchestrator: Some(TermRef { worker: WorkerId::new(), session: SessionId::new() }),
        limits: LimitsChange::default(),
        metadata: None,
        members: Vec::new(),
    };
    p.create(new, &running, monday()).unwrap();
    p
}

fn spec(when: &str, zone: &str) -> ScheduleSpec {
    let task = TaskSpec {
        title: "Bump dependencies".to_owned(),
        brief: "cargo update, then the gate.".to_owned(),
        ..TaskSpec::default()
    };
    let launch = TaskLaunch {
        pin: None,
        cwd: String::new(),
        run: Runner::Claude { prompt: Some("Read your brief.".to_owned()), args: Vec::new() },
        env: Vec::new(),
        size: None,
        ignore_dependencies: false,
    };
    ScheduleSpec { task, launch, when: when.to_owned(), zone: zone.to_owned(), paused: false }
}

fn set(
    p: &mut Projects,
    n: Option<u32>,
    spec: ScheduleSpec,
    now: WallMs,
) -> Changed<ProjectStatus> {
    let none = none();
    let running = Running { terminals: &none, agents: &none, starting: &[] };
    p.set_schedule(&id(), n, spec, &running, now)
}

fn schedules(p: &Projects) -> Vec<Schedule> {
    p.project(&id()).unwrap().schedules.clone()
}

fn message(refused: Outcome) -> String {
    match refused {
        Outcome::Error { message, .. } => message,
        other => panic!("not refused: {other:?}"),
    }
}

/// A schedule runs next at its time in the person's zone; paused it has no next run, and a
/// wrong rule, zone or task is refused saying what is wrong. A server-side zone is filled in
/// when none is named.
#[test]
fn a_schedule_runs_next_at_its_time_in_the_person_s_zone() {
    let mut p = projects();
    set(&mut p, None, spec("0 9 * * 1-5", "Europe/Berlin"), monday()).unwrap();
    let [held] = &*schedules(&p) else { panic!() };
    // 10:00 in Berlin now, so the next 09:00 there is Tuesday's: 07:00 UTC.
    assert_eq!(held.next_ms, Some(hours(23)));
    assert_eq!(held.id, 1);

    let paused = ScheduleSpec { paused: true, ..spec("0 9 * * *", "") };
    set(&mut p, Some(1), paused, monday()).unwrap();
    let [held] = &*schedules(&p) else { panic!() };
    assert_eq!(held.next_ms, None, "paused, it runs only when asked");
    assert!(!held.spec.zone.is_empty(), "the server's own zone, named");

    for (wrong, says) in [
        (spec("0 9 * *", "Europe/Berlin"), "five fields"),
        (spec("0 9 * * *", "Mars/Olympus"), "no time zone"),
        (spec("0 0 30 feb *", "UTC"), "no time in the next eight years"),
        (
            ScheduleSpec {
                task: TaskSpec { title: " ".to_owned(), ..TaskSpec::default() },
                ..spec("@daily", "UTC")
            },
            "title",
        ),
    ] {
        assert!(message(set(&mut p, None, wrong, monday()).unwrap_err()).contains(says), "{says}");
    }
    assert!(
        message(set(&mut p, Some(9), spec("@daily", "UTC"), monday()).unwrap_err())
            .contains("no schedule 9")
    );
}

/// When due, a schedule's run makes its task, marked as the schedule's, and moves its next
/// run past now; a run while the last one's task is under way is skipped saying so, and one
/// missed while the server was down runs once.
#[test]
fn a_due_schedule_makes_its_task_once_and_never_piles_up() {
    let mut p = projects();
    set(&mut p, None, spec("0 * * * *", "UTC"), monday()).unwrap();
    let (due, _, next) = p.schedules_due(monday());
    assert_eq!(due, []);
    assert_eq!(next, Some(hours(1)));

    // Three hours late: one run, the next an hour on.
    let (due, changes, next) = p.schedules_due(hours(3));
    assert_eq!(due, [(id(), 1)]);
    assert!(!changes.is_empty(), "its next run is kept");
    assert_eq!(next, Some(hours(4)));

    let ((task, launch), _) = p.run_schedule(&id(), 1, hours(3)).unwrap();
    assert_eq!(task.title, "Bump dependencies");
    assert_eq!(task.metadata.as_deref(), Some("{\"schedule\":1}"));
    assert!(matches!(launch.run, Runner::Claude { .. }));
    let last = schedules(&p)[0].last.clone().unwrap();
    assert_eq!((last.task, last.why), (Some(task.id), None));

    let skipped = message(p.run_schedule(&id(), 1, hours(4)).unwrap_err());
    assert!(skipped.contains(&format!("task {}, is still under way", task.id)), "{skipped}");
    p.schedule_ran(&id(), 1, None, Some(&skipped), hours(4));
    assert_eq!(schedules(&p)[0].last.clone().unwrap().why.as_deref(), Some(skipped.as_str()));

    let none = none();
    let running = Running { terminals: &none, agents: &none, starting: &[] };
    p.delete_schedule(&id(), 1, &running, hours(5)).unwrap();
    assert_eq!(schedules(&p), []);
}
