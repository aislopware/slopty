//! A project's scripts through the hub: the person's to set and run, listed to agents, and run
//! where the person points: a task's worktree, or the project's folder on a worker.

use slopty_proto::agent::{AgentBranch, Worktree};
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::{AgentReport, SCRIPTS_MAX, Script};
use slopty_proto::server::{Os, ToServer};

use super::super::super::project_tests::{
    create, new_task, project, refused, request, spawn, worker_on,
};
use super::super::super::tests::summary;
use super::*;
use crate::hub::Speaker;

fn script(name: &str, command: &str, dir: Option<&str>) -> Script {
    Script { name: name.to_owned(), command: command.to_owned(), dir: dir.map(str::to_owned) }
}

fn set(script: Script) -> Verb {
    Verb::ScriptSet { project: project(), script }
}

/// Scripts are kept by name, sorted, one of a name in place of the last; a name, command or
/// folder that cannot be kept is refused saying why, as is one past the most; taken away, it
/// is gone. An agent sees them in the project's status but sets, takes away and runs none.
#[tokio::test]
async fn scripts_are_the_person_s_and_an_agent_reads_them() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    create(&hub, None).await;
    let by_agent = hub.dispatch_as(Speaker::Agent, None, set(script("dev", "bun dev", None))).await;
    assert!(refused(&by_agent, ErrorCode::Forbidden).contains("person's shortcuts"));

    for s in [script("test", "cargo test", None), script("dev", "bun dev", Some("web/"))] {
        assert!(matches!(hub.dispatch(set(s)).await, Outcome::Project(_)));
    }
    let Outcome::Project(status) =
        hub.dispatch(set(script("dev", " bun run dev ", Some("web")))).await
    else {
        panic!("not a project")
    };
    assert_eq!(
        status.project.scripts,
        [script("dev", "bun run dev", Some("web")), script("test", "cargo test", None)]
    );
    let read = hub
        .dispatch_as(
            Speaker::Agent,
            None,
            Verb::ProjectStatus { project: project(), since: None, timeout_ms: 0 },
        )
        .await;
    assert!(
        matches!(&read, Outcome::Project(s) if s.project.scripts.len() == 2),
        "an agent reads them"
    );

    for (bad, why) in [
        (script("my dev", "x", None), "letters, digits"),
        (script("dev", "  ", None), "runs no command"),
        (script("dev", "x", Some("../up")), "under the project's"),
        (script("dev", "x", Some("/abs")), "under the project's"),
    ] {
        let said = hub.dispatch(set(bad)).await;
        assert!(refused(&said, ErrorCode::Invalid).contains(why), "{said:?}");
    }
    for n in 2..SCRIPTS_MAX {
        assert!(matches!(
            hub.dispatch(set(script(&format!("s{n}"), "x", None))).await,
            Outcome::Project(_)
        ));
    }
    let full = hub.dispatch(set(script("one-more", "x", None))).await;
    assert!(refused(&full, ErrorCode::Invalid).contains("at most"));

    let gone = Verb::ScriptDelete { project: project(), name: "test".to_owned() };
    let Outcome::Project(status) = hub.dispatch(gone.clone()).await else { panic!("not deleted") };
    assert!(status.project.scripts.iter().all(|s| s.name != "test"));
    assert!(refused(&hub.dispatch(gone).await, ErrorCode::Invalid).contains("no script test"));
    let timeline = format!("{:?}", status.timeline.iter().map(|e| &e.what).collect::<Vec<_>>());
    assert!(timeline.contains("Script test taken away."), "{timeline}");
}

/// A script runs on the orchestrator's worker in the project's folder, under the folder its
/// script names, or in a task's worktree on the task's worker; the worker is asked to open it
/// as the person's terminal. With no worker to run on, no such script, or a task with no
/// worktree yet, it is refused saying why; an agent runs none.
#[tokio::test]
async fn a_script_runs_in_the_project_s_folder_or_a_task_s_worktree() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (orchestrating, building) = (SessionId::new(), SessionId::new());
    let sessions = vec![summary(orchestrating), summary(building)];
    let (worker, lease, mut rx) = worker_on(&hub, "studio", Os::MacOs, sessions);
    create(&hub, None).await;
    assert!(matches!(
        hub.dispatch(set(script("dev", "bun run dev", Some("web")))).await,
        Outcome::Project(_)
    ));
    let run =
        |worker, task| Verb::ScriptRun { project: project(), name: "dev".to_owned(), worker, task };

    let nowhere = hub.dispatch(run(None, None)).await;
    assert!(refused(&nowhere, ErrorCode::Invalid).contains("name a worker"));
    let by_agent = hub.dispatch_as(Speaker::Agent, None, run(Some(worker), None)).await;
    refused(&by_agent, ErrorCode::Forbidden);
    let unknown = Verb::ScriptRun {
        project: project(),
        name: "deploy".to_owned(),
        worker: Some(worker),
        task: None,
    };
    assert!(refused(&hub.dispatch(unknown).await, ErrorCode::Invalid).contains("it has dev"));

    let edit = Verb::ProjectSet {
        project: project(),
        members: None,
        orchestrator: Some(TermRef { worker, session: orchestrating }),
        verifier: None,
        push: None,
        limits: slopty_proto::project::LimitsChange::default(),
        metadata: None,
    };
    assert!(matches!(hub.dispatch(edit).await, Outcome::Project(_)), "the orchestrator named");
    let asked = spawn(&hub, run(None, None));
    let (_, verb) = request(&mut rx).await;
    let Verb::RunScript { worker: on, cwd, line, name, .. } = verb else { panic!("{verb:?}") };
    assert_eq!(
        (on, cwd.as_str(), line.as_str(), name.as_str()),
        (worker, "~/src/slopty/web", "bun run dev", "dev · slopty")
    );
    asked.abort();

    let task = new_task(&hub, None).await;
    let early = hub.dispatch(run(None, Some(task))).await;
    assert!(refused(&early, ErrorCode::Invalid).contains("no worktree"));
    let tree = Worktree {
        name: "rows".to_owned(),
        path: "/w/.claude/worktrees/rows".to_owned(),
        branch: Some("worktree-rows".to_owned()),
        original_cwd: "/w".to_owned(),
        original_branch: Some("main".to_owned()),
    };
    let branch = AgentBranch { session: building, pr: None, worktree: Some(tree) };
    lease.handle(ToServer::Report(AgentReport::Branch(branch)));
    let assigned = hub.assign_for_test(&project(), task, TermRef { worker, session: building });
    assert!(matches!(assigned, Outcome::Task(_)));
    let elsewhere = hub.dispatch(run(Some(WorkerId::new()), Some(task))).await;
    assert!(refused(&elsewhere, ErrorCode::Invalid).contains("another worker"));
    let asked = spawn(&hub, run(None, Some(task)));
    let (_, verb) = request(&mut rx).await;
    let Verb::RunScript { worker: on, cwd, .. } = verb else { panic!("{verb:?}") };
    assert_eq!((on, cwd.as_str()), (worker, "/w/.claude/worktrees/rows/web"));
    asked.abort();
}
