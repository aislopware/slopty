//! What the projects cost at a large fleet's state (10 projects of 200 tasks, every timeline
//! full): the keeper's work for one change off the hub's lock (its log line and its replica),
//! a compaction's write of the whole file, one change to a task and one agent report under the
//! lock, the snapshot a new client link is sent, and one placement over 32 workers' facts in
//! CEL. `cargo xtask bench` runs them;
//! the numbers are in `docs/MEASUREMENTS.md`.

use slopty_core::SessionId;
use slopty_proto::project::{Fact, Facts, Placement, Preference};
use slopty_testkit::bench::Bench;

use super::*;
use crate::placement::{Candidate, Ranking, rank};

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
            review: None,
            verifier: Some("cargo gate".to_owned()),
            push: false,
            ask_to_start: false,
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
                owns: vec![format!("crates/p{p}/t{t}"), format!("docs/p{p}/t{t}.md")],
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
                    placed: None,
                };
                large.projects.assign(&id, task.id, who, &HashSet::from([term]), now()).unwrap();
                for a in 0..SUBAGENTS {
                    for report in native(term, a) {
                        large.projects.report(term.worker, &report, now());
                    }
                }
            }
        }
        let kept = large.projects.limits(&id).unwrap().timeline_kept;
        for n in 0..kept {
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

/// A worker as the server sees it: its built-in facts, a label and a probe.
fn candidate(n: u8) -> Candidate {
    let text = |t: &str| Fact::Text(t.to_owned());
    let os = if n.is_multiple_of(3) { "macos" } else { "linux" };
    let facts: Facts = BTreeMap::from([
        ("name".to_owned(), text(&format!("worker-{n}"))),
        ("os".to_owned(), text(os)),
        ("arch".to_owned(), text("aarch64")),
        ("cpus".to_owned(), Fact::Int([8, 16, 24, 32][usize::from(n % 4)])),
        ("memory_mb".to_owned(), Fact::Int(65_536)),
        ("load".to_owned(), Fact::Float(f64::from(n % 5))),
        ("live_agents".to_owned(), Fact::Int(i64::from(n % 3))),
        ("agents".to_owned(), Fact::List(vec![text("claude_code")])),
        ("labels".to_owned(), Fact::Map(BTreeMap::from([("rack".to_owned(), text("b2"))]))),
        ("probes".to_owned(), Fact::Map(BTreeMap::from([("cuda".to_owned(), text("12.8"))]))),
    ]);
    Candidate {
        worker: WorkerId::new(),
        name: format!("worker-{n}"),
        online: true,
        reported: true,
        facts,
        live: 0,
        fleet_live: 0,
    }
}

#[test]
#[ignore = "measurement"]
fn placement_cost() {
    let fleet: Vec<Candidate> = (0..32).map(candidate).collect();
    let placement = Placement {
        require: vec![
            r#"os == "linux" && cpus >= 16"#.to_owned(),
            r#"labels.rack == "b2" && "claude_code" in agents"#.to_owned(),
        ],
        prefer: vec![
            Preference { expr: "cpus".to_owned(), weight: 1 },
            Preference { expr: "load / double(cpus)".to_owned(), weight: -20 },
        ],
        ..Placement::default()
    };
    let ranking = Ranking { per_worker: Some(4), comprehensions: 2, ..Ranking::default() };
    let bench = Bench::new("server.placement_cost");
    let mut compiled = bench.series("compile_4_rules");
    for _ in 0..SAMPLES {
        compiled.time(|| placement::check(&placement, ranking.comprehensions)).unwrap();
    }
    compiled.report().unwrap();
    let mut ranked = bench.series("rank_32_workers");
    for _ in 0..SAMPLES {
        let suggestions = ranked.time(|| rank(&placement, &fleet, &BTreeMap::new(), ranking));
        assert!(
            suggestions.as_ref().is_ok_and(|s| s.first().is_some_and(|w| w.fits)),
            "{suggestions:?}"
        );
    }
    ranked.report().unwrap();
}
