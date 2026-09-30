//! Projects end to end with a stand-in agent: a real server in this process, a real ptyd and
//! worker registered with it, and `slopty-stub-claude` on the worker's `PATH` as `claude`. No
//! real Claude Code runs, and nobody's plan is spent.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::Duration;

    use serde_json::{Value, json};
    use slopty_core::WorkerId;
    use slopty_proto::agent::AgentKind;
    use slopty_proto::orchestration::{Outcome, Verb};
    use slopty_proto::project::{
        LimitsChange, Moment, Natives, ProjectId, ProjectStatus, Runner, TaskId, TaskLaunch,
        TaskSpec, TaskState,
    };
    use slopty_proto::server::Liveness;
    use slopty_server::{Config, Hub, Server};
    use tokio::io::{AsyncBufReadExt as _, BufReader};
    use tokio::process::{Child, Command};

    const STEP: Duration = Duration::from_secs(60);

    /// A binary of this build (`slopty_testkit::bins`).
    fn bin(name: &str) -> PathBuf {
        slopty_testkit::bins::bin(env!("CARGO_BIN_EXE_slopty"), name)
    }

    /// `program`, started from a clean environment with its home at `home`
    /// (`slopty_testkit::env::scrub`): nothing of the developer's reaches it, or the agents
    /// the daemons start.
    fn scrubbed(program: impl AsRef<std::ffi::OsStr>, home: &Path) -> Command {
        let mut command = Command::new(program);
        slopty_testkit::env::scrub(command.as_std_mut(), home);
        command
    }

    /// ptyd and a worker registered with the server at `server`, finding `claude` in
    /// `programs` first, with `settings` as its `settings.toml`; killed with the test.
    async fn worker(dir: &Path, server: SocketAddr, programs: &Path, settings: &str) -> Vec<Child> {
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(dir.join("data").join("settings.toml"), settings).unwrap();
        let path = slopty_testkit::env::path_with(programs);
        let ptyd_sock = dir.join("ptyd.sock");
        let mut ptyd = scrubbed(bin("slopty-ptyd"), &dir.join("home"))
            .env("PATH", &path)
            .arg("--socket")
            .arg(&ptyd_sock)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let ready = async {
            while tokio::net::UnixStream::connect(&ptyd_sock).await.is_err() {
                assert!(ptyd.try_wait().unwrap().is_none(), "ptyd exited early");
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        };
        tokio::time::timeout(STEP, ready).await.expect("ptyd socket");
        let mut worker = scrubbed(bin("slopty-worker"), &dir.join("home"))
            .env("PATH", &path)
            .env("SLOPTY_WORKER_NAME", "projects-test")
            .arg("--ptyd-socket")
            .arg(&ptyd_sock)
            .arg("--ctl-socket")
            .arg(dir.join("worker.sock"))
            .arg("--data-dir")
            .arg(dir.join("data"))
            .args(["--print-addr", "--port", "0", "--server", &server.to_string()])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut line = String::new();
        let stdout = worker.stdout.take().unwrap();
        tokio::time::timeout(STEP, BufReader::new(stdout).read_line(&mut line))
            .await
            .expect("the worker prints its address")
            .unwrap();
        vec![ptyd, worker]
    }

    /// Wait until `done` holds, checking every 50 ms.
    async fn until<T>(what: &str, mut done: impl AsyncFnMut() -> Option<T>) -> T {
        let waited = tokio::time::timeout(STEP, async {
            loop {
                if let Some(found) = done().await {
                    return found;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        waited.unwrap_or_else(|_| panic!("{what}"))
    }

    async fn status(hub: &Hub, project: &ProjectId) -> ProjectStatus {
        let verb = Verb::ProjectStatus { project: project.clone(), since: Some(0), timeout_ms: 0 };
        match hub.dispatch(verb).await {
            Outcome::Project(status) => *status,
            other => panic!("{other:?}"),
        }
    }

    fn hook(event: &str, rest: &Value) -> Value {
        let mut payload = json!({ "hook_event_name": event });
        if let (Some(payload), Some(rest)) = (payload.as_object_mut(), rest.as_object()) {
            payload.extend(rest.clone());
        }
        payload
    }

    /// A server on loopback keeping its state under `root`, and a worker registered with it
    /// that has the stub as `claude`, once it is online with Claude Code installed.
    async fn fleet(root: &Path, settings: &str) -> (Server, Vec<Child>, WorkerId) {
        let server = Server::start(Config {
            name: "projects-test".to_owned(),
            quic: "127.0.0.1:0".parse().unwrap(),
            mcp: "127.0.0.1:0".parse().unwrap(),
            data_dir: root.join("server"),
            admission: slopty_net::admission::Admission::default(),
        })
        .await
        .unwrap();
        let hub = server.hub().clone();
        let programs = root.join("programs");
        std::fs::create_dir_all(&programs).unwrap();
        std::os::unix::fs::symlink(bin("slopty-stub-claude"), programs.join("claude")).unwrap();
        let daemons = worker(root, server.quic_addr(), &programs, settings).await;
        let worker = until("the worker registers with Claude Code installed", async || {
            hub.directory().into_iter().find(|w| {
                w.liveness == Liveness::Online
                    && w.caps.agents.iter().any(|a| a.kind == AgentKind::ClaudeCode)
            })
        })
        .await
        .worker;
        (server, daemons, worker)
    }

    /// What Claude Code keeps in a task's node, as the server has it.
    async fn natives_of(hub: &Hub, project: &ProjectId, task: TaskId) -> Natives {
        let verb = Verb::TaskGet { project: project.clone(), task: Some(task) };
        match hub.dispatch(verb).await {
            Outcome::Node(node) => node.natives,
            other => panic!("{other:?}"),
        }
    }

    /// `slopty <args>` against `server`, its own data under `root`; what it printed, once it
    /// succeeded.
    async fn slopty(root: &Path, server: SocketAddr, args: &[&str]) -> String {
        let ran = scrubbed(bin("slopty"), &root.join("home"))
            .arg("--server")
            .arg(server.to_string())
            .arg("--data-dir")
            .arg(root.join("cli"))
            .args(args)
            .env("RUST_LOG", "warn")
            .kill_on_drop(true)
            .output();
        let out = tokio::time::timeout(STEP, ran).await.expect("slopty answers").unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "slopty {args:?}: {stderr}");
        String::from_utf8(out.stdout).unwrap()
    }

    /// An agent the server starts for a task gets Slopty's tools and knows its project, task
    /// and server: through the `slopty mcp` on its `--mcp-config`, with no flag, a tool call
    /// that names no project answers its own. What its hooks say becomes the tree: Claude
    /// Code's own subagent and to-do as leaves of its task, the worktree its status line names
    /// as the task's. Its first prompt is typed once it says it is ready.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_agent_started_for_a_task_has_the_tools_and_grows_the_tree() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (server, _daemons, worker) = fleet(&root, "").await;
        let hub = server.hub().clone();

        let project = ProjectId::new("demo").unwrap();
        let made = hub
            .dispatch(Verb::ProjectCreate {
                project: project.clone(),
                title: "Demo".to_owned(),
                repo: root.to_string_lossy().into_owned(),
                target: "main".to_owned(),
                verifier: None,
                orchestrator: None,
                limits: LimitsChange::default(),
                metadata: None,
            })
            .await;
        assert!(matches!(made, Outcome::Project(_)), "{made:?}");
        let task = hub
            .dispatch(Verb::TaskCreate {
                project: project.clone(),
                spec: Box::new(TaskSpec {
                    title: "Look around".to_owned(),
                    brief: "Read the code.".to_owned(),
                    owns: vec!["src".to_owned()],
                    ..TaskSpec::default()
                }),
            })
            .await;
        assert!(matches!(&task, Outcome::Task(t) if t.id == TaskId(1)), "{task:?}");

        let record = root.join("record.json");
        let worktree = json!({
            "name": "look",
            "path": root.join("wt").to_string_lossy(),
            "branch": "slopty/demo/1",
            "original_cwd": root.to_string_lossy(),
            "original_branch": "main",
        });
        let hooks = json!([
            hook("SessionStart", &json!({ "source": "startup" })),
            hook("SubagentStart", &json!({ "agent_id": "ag1", "agent_type": "Explore" })),
            hook(
                "SubagentStop",
                &json!({
                    "agent_id": "ag1",
                    "agent_type": "Explore",
                    "agent_transcript_path": "/t/ag1.jsonl",
                    "last_assistant_message": "Found the entry point.",
                }),
            ),
            hook("TaskCreated", &json!({ "task_id": "1", "task_subject": "Read main.rs" })),
            hook("Statusline", &json!({ "worktree": worktree })),
        ]);
        let calls = json!([
            { "name": "project_status", "arguments": {} },
            {
                "name": "task_report",
                "arguments": { "kind": "checkpoint", "note": "Read the entry point." },
            },
        ]);
        let launch = TaskLaunch {
            pin: None,
            cwd: root.to_string_lossy().into_owned(),
            run: Runner::Claude { prompt: Some("go".to_owned()), args: Vec::new() },
            env: vec![
                ("STUB_RECORD".to_owned(), record.to_string_lossy().into_owned()),
                ("STUB_HOOKS".to_owned(), hooks.to_string()),
                ("STUB_MCP_CALLS".to_owned(), calls.to_string()),
            ],
            size: None,
            ignore_dependencies: false,
        };
        let spawned = hub
            .dispatch(Verb::TaskSpawn { project: project.clone(), task: TaskId(1), launch })
            .await;
        let Outcome::Task(spawned) = spawned else { panic!("{spawned:?}") };
        let term = spawned.assignment.expect("the task has its agent").term;
        assert_eq!(term.worker, worker, "the only worker that fits");

        let seen: Value = tokio::time::timeout(STEP, async {
            loop {
                let seen: Option<Value> =
                    std::fs::read(&record).ok().and_then(|b| serde_json::from_slice(&b).ok());
                let typed = seen
                    .as_ref()
                    .and_then(|s| s["typed"].as_array())
                    .is_some_and(|t| t.iter().any(|l| l == "go"));
                let answered =
                    seen.as_ref().and_then(|s| s["mcp"].as_array()).is_some_and(|m| m.len() == 2);
                if let (true, true, Some(seen)) = (typed, answered, seen) {
                    return seen;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            let recorded = std::fs::read_to_string(&record).unwrap_or_default();
            panic!("the agent answered its tool call and took its prompt; it recorded {recorded}")
        });

        // Its environment: the server with no flag, its project and task, its terminal.
        let env = &seen["env"];
        assert_eq!(env["SLOPTY_SERVER"], server.quic_addr().to_string());
        assert_eq!(env["SLOPTY_PROJECT"], "demo");
        assert_eq!(env["SLOPTY_TASK"], "1");
        assert_eq!(env["SLOPTY_SESSION"], term.session.to_string());
        // Its tools: the `slopty` beside the worker, as an MCP server on stdio.
        let tools = &seen["mcp_config"][0]["mcpServers"]["slopty"];
        assert_eq!(tools["command"].as_str().map(PathBuf::from), Some(bin("slopty")));
        assert_eq!(tools["args"], json!(["mcp"]));
        let argv: Vec<&str> =
            seen["argv"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert!(argv.iter().any(|a| a.starts_with("--mcp-config=")), "{argv:?}");
        for fired in seen["hooks"].as_array().unwrap() {
            assert_eq!(fired["fired"], true, "{fired}");
        }
        // The tool named no project and answered the agent's own, from the real server.
        let answer = &seen["mcp"][0];
        assert_eq!(answer["isError"], false, "{answer}");
        let text = answer["content"][0]["text"].as_str().expect("a text answer");
        let told: Value = serde_json::from_str(text).unwrap();
        assert_eq!(told["project"]["project"], "demo");
        assert_eq!(told["tasks"][0]["term"], format!("{}/{}", term.worker, term.session));
        // Its report on its own task, proven by the token its terminal was given, lands.
        assert_eq!(seen["mcp"][1]["isError"], false, "{}", seen["mcp"][1]);
        assert!(seen["env"]["SLOPTY_SESSION_TOKEN"].as_str().is_some_and(|t| !t.is_empty()));

        // The tree: Claude Code's own subagent and to-do under the task, the worktree its
        // status line named, and the timeline saying so.
        let tree = until("the hooks reach the tree", async || {
            let now = status(&hub, &project).await;
            let natives = natives_of(&hub, &project, TaskId(1)).await;
            let stopped = natives.agents.first().is_some_and(|a| a.stopped_ms.is_some());
            let worktree = now.tasks.first()?.worktree.is_some();
            (stopped && !natives.tasks.is_empty() && worktree).then_some(now)
        })
        .await;
        let t = &tree.tasks[0];
        let natives = natives_of(&hub, &project, TaskId(1)).await;
        let ag1 = &natives.agents[0];
        assert_eq!((ag1.id.as_str(), ag1.kind.as_str()), ("ag1", "Explore"));
        assert_eq!(ag1.transcript.as_deref(), Some("/t/ag1.jsonl"));
        assert_eq!(ag1.last.as_deref(), Some("Found the entry point."));
        assert_eq!(natives.tasks[0].subject, "Read main.rs");
        assert_eq!(t.branch.as_deref(), Some("slopty/demo/1"));
        assert!(
            matches!(t.state, TaskState::Running | TaskState::Waiting),
            "its agent's status: {:?}",
            t.state
        );
        let moments: Vec<&Moment> = tree.timeline.iter().map(|e| &e.what).collect();
        assert!(moments.iter().any(|m| matches!(m, Moment::Assigned { spawned: true, .. })));
        assert!(moments.iter().any(|m| matches!(m, Moment::Branch { .. })), "{moments:?}");
        assert!(moments.iter().any(|m| matches!(m, Moment::Reported { .. })), "{moments:?}");

        server.shutdown().await;
    }

    /// What a worker's person says of it, labels and probe commands in its settings, reaches
    /// `slopty workers` as facts, and a task's placement rules read them. A command task made
    /// with the CLI runs where its rules place it, with its project and task in its
    /// environment.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_worker_s_own_facts_place_a_command_task_that_runs() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let settings = "[worker.labels]\nrack = \"b2\"\n[worker.probes]\nhello = \"echo hi\"\n";
        let (server, _daemons, worker) = fleet(&root, settings).await;
        let hub = server.hub().clone();
        let addr = server.quic_addr();

        let listed: Value = until("the labels and probes reach `slopty workers`", async || {
            let listed: Value =
                serde_json::from_str(&slopty(&root, addr, &["--json", "workers"]).await).ok()?;
            let facts = &listed.as_array()?.first()?["facts"];
            (facts["probes"]["hello"] == "hi").then_some(listed)
        })
        .await;
        let facts = &listed[0]["facts"];
        assert_eq!(facts["labels"]["rack"], "b2", "{facts}");
        assert_eq!(facts["os"], "macos", "{facts}");

        let repo = root.to_string_lossy().into_owned();
        slopty(&root, addr, &["project", "create", "demo", "--title", "Demo", "--repo", &repo])
            .await;
        slopty(
            &root,
            addr,
            &[
                "task",
                "create",
                "--project",
                "demo",
                "--title",
                "Check the rack",
                "--kind",
                "check",
                "--read-only",
                "--require",
                r#"labels.rack == "b2""#,
                "--require",
                r#"probes.hello == "hi""#,
                "--prefer",
                "5:cpus >= 1",
            ],
        )
        .await;
        let ranked: Value = serde_json::from_str(
            &slopty(
                &root,
                addr,
                &["--json", "task", "suggest", "--project", "demo", "--task", "1"],
            )
            .await,
        )
        .unwrap();
        assert_eq!(ranked[0]["worker"], worker.to_string(), "{ranked}");
        assert_eq!(ranked[0]["fits"], true, "{ranked}");
        assert_eq!(ranked[0]["score"], 5, "{ranked}");

        let out = root.join("ran");
        let script =
            format!("printf %s \"$SLOPTY_PROJECT/$SLOPTY_TASK\" > '{}'; exec cat", out.display());
        slopty(
            &root,
            addr,
            &[
                "task",
                "spawn",
                "--project",
                "demo",
                "1",
                "--command",
                "--cwd",
                &repo,
                "--",
                "/bin/sh",
                "-c",
                &script,
            ],
        )
        .await;
        let ran = until("the command ran", async || {
            std::fs::read_to_string(&out).ok().filter(|t| !t.is_empty())
        })
        .await;
        assert_eq!(ran, "demo/1");
        let project = ProjectId::new("demo").unwrap();
        let now = status(&hub, &project).await;
        let task = &now.tasks[0];
        assert_eq!((task.kind.as_str(), task.read_only), ("check", true));
        let term = task.assignment.as_ref().expect("placed and running").term;
        assert_eq!(term.worker, worker);
        assert_eq!(now.live.project, 1, "a command task counts: it may be any agent's CLI");

        server.shutdown().await;
    }

    /// An agent says which task it is on by the server's record of its session, not by its
    /// environment: one started with a stale `SLOPTY_TASK` and then put on another task
    /// updates the task it is on when a tool names none, and reports on it: its terminal's token
    /// proves where it speaks from, whoever started it.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_server_s_record_of_a_session_wins_over_its_slopty_task() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (server, _daemons, worker) = fleet(&root, "").await;
        let hub = server.hub().clone();
        let project = ProjectId::new("demo").unwrap();
        let made = hub
            .dispatch(Verb::ProjectCreate {
                project: project.clone(),
                title: "Demo".to_owned(),
                repo: root.to_string_lossy().into_owned(),
                target: "main".to_owned(),
                verifier: None,
                orchestrator: None,
                limits: LimitsChange::default(),
                metadata: None,
            })
            .await;
        assert!(matches!(made, Outcome::Project(_)), "{made:?}");
        for title in ["Review", "Decoy"] {
            let spec = TaskSpec { title: title.to_owned(), read_only: true, ..TaskSpec::default() };
            let made = hub
                .dispatch(Verb::TaskCreate { project: project.clone(), spec: Box::new(spec) })
                .await;
            assert!(matches!(made, Outcome::Task(_)), "{made:?}");
        }

        let record = root.join("record.json");
        let gate = root.join("assigned");
        let calls = json!([
            { "name": "task_update", "arguments": { "status": "reviewing" } },
            { "name": "task_report", "arguments": { "kind": "done", "note": "Reviewed." } },
        ]);
        let spawned = hub
            .dispatch(Verb::SpawnAgent {
                worker,
                agent: AgentKind::ClaudeCode,
                cwd: root.to_string_lossy().into_owned(),
                prompt: None,
                args: Vec::new(),
                env: vec![
                    ("SLOPTY_PROJECT".to_owned(), "demo".to_owned()),
                    ("SLOPTY_TASK".to_owned(), "2".to_owned()),
                    ("STUB_RECORD".to_owned(), record.to_string_lossy().into_owned()),
                    ("STUB_MCP_CALLS".to_owned(), calls.to_string()),
                    ("STUB_MCP_AFTER".to_owned(), gate.to_string_lossy().into_owned()),
                ],
                size: None,
                session: None,
                permission_flags: false,
            })
            .await;
        let Outcome::Opened(term) = spawned else { panic!("{spawned:?}") };
        let assigned = hub
            .dispatch(Verb::TaskAssign { project: project.clone(), task: TaskId(1), term })
            .await;
        assert!(matches!(assigned, Outcome::Task(_)), "{assigned:?}");
        std::fs::write(&gate, b"").unwrap();

        let seen: Value = until("the agent's tool call answered", async || {
            let seen: Value = serde_json::from_slice(&std::fs::read(&record).ok()?).ok()?;
            (seen["mcp"].as_array()?.len() == 2).then_some(seen)
        })
        .await;
        assert_eq!(seen["env"]["SLOPTY_TASK"], "2", "the stale environment it was given");
        assert_eq!(seen["mcp"][0]["isError"], false, "{}", seen["mcp"][0]);
        // Put on its task by hand, its terminal's token still proves it: it reports on it.
        let reported = &seen["mcp"][1];
        assert_eq!(reported["isError"], false, "{reported}");
        let now = status(&hub, &project).await;
        assert_eq!(now.tasks[0].status.as_deref(), Some("reviewing"), "the task it is on");
        assert_eq!(now.tasks[1].status, None, "not the one its environment named");

        server.shutdown().await;
    }
}
