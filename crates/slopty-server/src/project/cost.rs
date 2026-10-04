//! What the projects cost at a large fleet's state (10 projects of 200 tasks, every timeline
//! full): the keeper's work for one change off the hub's lock (its log line and its replica),
//! a compaction's write of the whole file, one change to a task and one agent report under the
//! lock, and the snapshot a new client link is sent. `cargo xtask bench` runs them; the numbers
//! are in `docs/MEASUREMENTS.md`.

use slopty_core::SessionId;
use slopty_testkit::bench::Bench;

use super::*;

const PROJECTS: usize = 10;
const TASKS: u32 = 200;
/// Tasks with a live agent in each project: two of ten projects' worth stays under the
/// fleet's default bound.
const ASSIGNED: u32 = 2;
/// Subagents each live agent has started and stopped.
const SUBAGENTS: usize = 128;
const SAMPLES: usize = 100;

fn now() -> WallMs {
    WallMs::from_millis(1_790_000_000_000)
}

struct Large {
    projects: Projects,
    terminals: HashSet<TermRef>,
    agents: HashSet<TermRef>,
    busy: Vec<TermRef>,
}

impl Large {
    fn running(&self) -> Running<'_> {
        Running { terminals: &self.terminals, agents: &self.agents, starting: &[] }
    }
}

fn project(p: usize) -> ProjectId {
    ProjectId::new(format!("project-{p}")).unwrap()
}

fn large() -> Large {
    let mut large = Large {
        projects: Projects::default(),
        terminals: HashSet::new(),
        agents: HashSet::new(),
        busy: Vec::new(),
    };
    for p in 0..PROJECTS {
        let id = project(p);
        let new = NewProject {
            id: id.clone(),
            title: format!("Project {p}"),
            repo: "~/src/slopty".to_owned(),
            target: "main".to_owned(),
            verifier: Some("cargo gate".to_owned()),
            push: false,
            orchestrator: None,
            limits: LimitsChange::default(),
            metadata: Some(r#"{"ticket":"SLOP-1234","owner":"platform"}"#.to_owned()),
            members: Vec::new(),
        };
        let Large { projects, terminals, agents, .. } = &mut large;
        let running = Running { terminals, agents, starting: &[] };
        projects.create(new, &running, now()).unwrap();
        for t in 0..TASKS {
            let spec = TaskSpec {
                kind: "build".to_owned(),
                title: format!("Task {t}: move the hub's state onto deltas"),
                brief: "Read the hub, find where whole tasks go out, send what changed. ".repeat(3),
                ..TaskSpec::default()
            };
            let (task, _) = large.projects.create_task(&id, spec, now()).unwrap();
            if t < ASSIGNED {
                let term = TermRef { worker: WorkerId::new(), session: SessionId::new() };
                large.terminals.insert(term);
                large.agents.insert(term);
                large.busy.push(term);
                let who = Assignee {
                    term,
                    spawned: true,
                    branch: None,
                    conversation: None,
                    thread: None,
                };
                large.projects.assign(&id, task.id, who, &HashSet::from([term]), now()).unwrap();
                for a in 0..SUBAGENTS {
                    for report in native(term, a) {
                        large.projects.report(term.worker, &report, now());
                    }
                }
            }
        }
        for n in 0..TIMELINE_KEPT {
            let note = TaskChange { note: Some(format!("note {n}")), ..TaskChange::default() };
            large.projects.update_task(&id, TaskId(1), note, Caller::Person, now()).unwrap();
        }
    }
    large
}

/// Subagent `a` of the agent in `term` starting and stopping.
fn native(term: TermRef, a: usize) -> [AgentReport; 2] {
    let agent = format!("agent-{a}");
    [
        AgentReport::SubagentStarted {
            session: term.session,
            agent: agent.clone(),
            kind: "Explore".to_owned(),
        },
        AgentReport::SubagentStopped {
            session: term.session,
            agent,
            transcript: Some(format!("/Users/me/.claude/projects/x/{a}.jsonl")),
            last: Some("Found where the hub sends whole tasks.".to_owned()),
        },
    ]
}

fn bytes<T: Serialize>(value: &T) -> usize {
    serde_json::to_vec(value).map_or(0, |b| b.len())
}

#[test]
#[ignore = "measurement"]
fn projects_cost() {
    let mut large = large();
    let bench = Bench::new("server.projects_cost");

    let file = large.projects.file(Vec::new(), 0);
    let mut compact = bench.series("file_write_compact");
    let mut written = Vec::new();
    for _ in 0..SAMPLES {
        written = compact.time(|| serde_json::to_vec(&file).unwrap());
    }
    compact.report().unwrap();
    let pretty = serde_json::to_vec_pretty(&file).unwrap();
    eprintln!("projects file: {} bytes compact, {} pretty", written.len(), pretty.len());

    let id = project(0);
    let mut change = bench.series("task_change");
    let mut updates = Vec::new();
    for n in 0..SAMPLES {
        let status = TaskChange { status: Some(format!("step {n}")), ..TaskChange::default() };
        updates = change
            .time(|| large.projects.update_task(&id, TaskId(TASKS), status, Caller::Person, now()))
            .unwrap()
            .1;
    }
    change.report().unwrap();
    let mut keep = bench.series("log_append");
    let mut replica = file.clone();
    let mut line = Vec::new();
    let kept: Vec<Keep> = updates.iter().map(|u| Keep::Project(Box::new(u.kept.clone()))).collect();
    for k in kept.iter().cycle().take(SAMPLES) {
        line = keep.time(|| {
            replica.apply(k);
            serde_json::to_vec(k).unwrap()
        });
    }
    keep.report().unwrap();
    eprintln!("a task change's log line: {} bytes", line.len());
    let whole = large.projects.status(&id, Some(0), &large.running()).unwrap();
    let task = whole.tasks.iter().find(|t| t.id == TaskId(TASKS)).unwrap();
    let natives = (1..=TASKS)
        .map(|t| bytes(&large.projects.node(&id, Some(TaskId(t))).unwrap().natives))
        .max()
        .unwrap_or(0);
    eprintln!(
        "a task change pushes {} bytes; the task alone is {}, its project's status {}, \
         the largest node's natives {}",
        updates.iter().map(|u| bytes(&large.projects.pushed(&u.kept))).sum::<usize>(),
        bytes(task),
        bytes(&whole),
        natives,
    );

    let term = *large.busy.first().unwrap();
    let mut report = bench.series("agent_report");
    let mut pushed = Vec::new();
    for n in 0..SAMPLES {
        let [started, _] = native(term, SUBAGENTS + n);
        pushed = report.time(|| large.projects.report(term.worker, &started, now()));
    }
    report.report().unwrap();
    eprintln!(
        "a subagent's start pushes {} bytes",
        pushed.iter().map(|u| bytes(&large.projects.pushed(&u.kept))).sum::<usize>()
    );

    let mut snapshot = bench.series("snapshot");
    let mut sent = Vec::new();
    for _ in 0..SAMPLES {
        sent = snapshot.time(|| large.projects.snapshot(&large.running()));
    }
    snapshot.report().unwrap();
    eprintln!("a new link's snapshot: {} bytes", bytes(&sent));
}
