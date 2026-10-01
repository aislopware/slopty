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
        worker_named(dir, server, programs, settings, "projects-test").await
    }

    /// [`worker`] under `name`.
    async fn worker_named(
        dir: &Path,
        server: SocketAddr,
        programs: &Path,
        settings: &str,
        name: &str,
    ) -> Vec<Child> {
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
            .env("SLOPTY_WORKER_NAME", name)
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
                push: false,
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
                push: false,
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

    /// `git -C dir args…`, with nobody's git config, once it succeeded.
    fn git(dir: &Path, args: &[&str]) {
        let ran = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        assert!(ran.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&ran.stderr));
    }

    /// The project learns which repository it works in from the shell its orchestrator runs
    /// in, as the real worker identifies it (its origin, read from the config, and its first
    /// commit, from git). A task then started with no directory goes beside that clone and
    /// its agent starts there in a git worktree of its own, named for the task.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_task_with_no_directory_starts_in_a_worktree_of_the_project_s_clone() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let repo = root.join("demo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
        git(&repo, &["remote", "add", "origin", "git@github.com:aislopware/demo.git"]);
        let (server, _daemons, worker) = fleet(&root, "").await;
        let hub = server.hub().clone();

        let shell = Verb::OpenTerminal {
            worker,
            cwd: Some(repo.to_string_lossy().into_owned()),
            command: Vec::new(),
            env: Vec::new(),
            name: None,
            size: None,
            session: None,
        };
        let Outcome::Opened(orchestrator) = hub.dispatch(shell).await else { panic!("no shell") };
        let project = ProjectId::new("demo").unwrap();
        let made = hub
            .dispatch(Verb::ProjectCreate {
                project: project.clone(),
                title: "Demo".to_owned(),
                repo: "demo".to_owned(),
                target: "main".to_owned(),
                verifier: None,
                push: false,
                orchestrator: Some(orchestrator),
                limits: LimitsChange::default(),
                metadata: None,
            })
            .await;
        assert!(matches!(made, Outcome::Project(_)), "{made:?}");
        let id = until("the project learns its repository", async || {
            status(&hub, &project).await.project.repo_id.filter(|id| id.root.is_some())
        })
        .await;
        assert_eq!(id.origin.as_deref(), Some("github.com/aislopware/demo"));

        let task = hub
            .dispatch(Verb::TaskCreate {
                project: project.clone(),
                spec: Box::new(TaskSpec { title: "Write it".to_owned(), ..TaskSpec::default() }),
            })
            .await;
        assert!(matches!(&task, Outcome::Task(t) if t.id == TaskId(1)), "{task:?}");
        let record = root.join("record.json");
        let launch = TaskLaunch {
            pin: None,
            cwd: String::new(),
            run: Runner::Claude { prompt: None, args: Vec::new() },
            env: vec![("STUB_RECORD".to_owned(), record.to_string_lossy().into_owned())],
            size: None,
            ignore_dependencies: false,
        };
        let spawned = hub
            .dispatch(Verb::TaskSpawn { project: project.clone(), task: TaskId(1), launch })
            .await;
        assert!(matches!(spawned, Outcome::Task(_)), "{spawned:?}");
        let seen: Value = until("the agent starts", async || {
            std::fs::read(&record).ok().and_then(|b| serde_json::from_slice(&b).ok())
        })
        .await;
        assert_eq!(seen["cwd"].as_str().map(PathBuf::from), Some(repo), "beside the clone");
        let argv: Vec<&str> =
            seen["argv"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert!(argv.windows(2).any(|w| w == ["--worktree", "slopty-demo-1"]), "{argv:?}");

        server.shutdown().await;
    }

    /// `git -C dir args…` with nobody's config: what it printed.
    fn git_out(dir: &Path, args: &[&str]) -> String {
        let ran = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        assert!(ran.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&ran.stderr));
        String::from_utf8_lossy(&ran.stdout).trim().to_owned()
    }

    /// Two real workers, the orchestrator's with a clone of the project's repository and
    /// another with none. A task the rules put on the other gets a clone there first, made by
    /// that worker's own git from the address the orchestrator's clone names (its person's
    /// `insteadOf` reaching the forge), shown as a step. Once its agent reports done, the
    /// branch it committed comes home: bundled there, carried by the server, fetched into the
    /// orchestrator's clone at the same commit, and the card says it arrived.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_task_s_clone_is_made_where_it_runs_and_its_branch_comes_home() {
        use slopty_proto::project::{Placement, StepKind, StepState};
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        // The forge: a bare repository that both workers' git reaches as an https address.
        let seed = root.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        git_out(&seed, &["init", "-q", "-b", "main"]);
        git_out(&seed, &["commit", "-q", "--allow-empty", "-m", "first"]);
        git_out(&seed, &["commit", "-q", "--allow-empty", "-m", "second"]);
        git_out(&root, &["clone", "-q", "--bare", "seed", "forge.git"]);
        let url = "https://example.com/o/demo.git";
        let reach = format!("url.file://{}.insteadOf={url}", root.join("forge.git").display());
        git_out(&root, &["-c", &reach, "clone", "-q", url, "demo"]);
        let studio_clone = root.join("demo");
        // The target moves on after the orchestrator's clone was made, so the clone made later
        // forks from a commit this one has yet to fetch.
        git_out(&seed, &["commit", "-q", "--allow-empty", "-m", "third"]);
        let forge = root.join("forge.git");
        git_out(&seed, &["push", "-q", &forge.to_string_lossy(), "main"]);
        let forge_main = git_out(&forge, &["rev-parse", "main"]);

        // Each worker's person reaches the forge through their own git config.
        let config = format!("[url \"file://{}\"]\n\tinsteadOf = {url}\n", forge.display());
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::write(root.join("home/.gitconfig"), &config).unwrap();
        let (server, _daemons, studio) = fleet(&root, "").await;
        let hub = server.hub().clone();
        let linux_dir = root.join("linux");
        let linux_home = linux_dir.join("home");
        std::fs::create_dir_all(&linux_home).unwrap();
        std::fs::write(linux_home.join(".gitconfig"), config).unwrap();
        // Claude Code has run for this person, so a clone made for an agent is trusted.
        std::fs::write(linux_home.join(".claude.json"), "{}").unwrap();
        let programs = root.join("programs");
        let _linux_daemons =
            worker_named(&linux_dir, server.quic_addr(), &programs, "", "linux-box").await;
        until("the second worker registers", async || {
            (hub.directory().iter().filter(|w| w.liveness == Liveness::Online).count() == 2)
                .then_some(())
        })
        .await;

        let shell = Verb::OpenTerminal {
            worker: studio,
            cwd: Some(studio_clone.to_string_lossy().into_owned()),
            command: Vec::new(),
            env: Vec::new(),
            name: None,
            size: None,
            session: None,
        };
        let Outcome::Opened(orchestrator) = hub.dispatch(shell).await else { panic!("a shell") };
        let project = ProjectId::new("demo").unwrap();
        let made = hub
            .dispatch(Verb::ProjectCreate {
                project: project.clone(),
                title: "Demo".to_owned(),
                repo: "demo".to_owned(),
                target: "main".to_owned(),
                verifier: None,
                push: false,
                orchestrator: Some(orchestrator),
                limits: LimitsChange::default(),
                metadata: None,
            })
            .await;
        assert!(matches!(made, Outcome::Project(_)), "{made:?}");
        let id = until("the project learns its repository", async || {
            status(&hub, &project).await.project.repo_id.filter(|id| id.root.is_some())
        })
        .await;
        assert_eq!(id.url.as_deref(), Some(url), "the address to clone from, as its config has it");

        let on_linux = Placement {
            require: vec![r#"name == "linux-box""#.to_owned()],
            ..Placement::default()
        };
        let spec =
            TaskSpec { title: "Write it".to_owned(), placement: on_linux, ..TaskSpec::default() };
        let task =
            hub.dispatch(Verb::TaskCreate { project: project.clone(), spec: Box::new(spec) }).await;
        assert!(matches!(&task, Outcome::Task(t) if t.id == TaskId(1)), "{task:?}");
        let (record, gate) = (root.join("record.json"), root.join("go"));
        let branch = "worktree-slopty-demo-1";
        let calls = json!([{ "name": "task_report", "arguments": { "kind": "done", "note": "Wrote it.", "branch": branch } }]);
        let launch = TaskLaunch {
            pin: None,
            cwd: String::new(),
            run: Runner::Claude { prompt: None, args: Vec::new() },
            env: vec![
                ("STUB_RECORD".to_owned(), record.to_string_lossy().into_owned()),
                ("STUB_MCP_CALLS".to_owned(), calls.to_string()),
                ("STUB_MCP_AFTER".to_owned(), gate.to_string_lossy().into_owned()),
            ],
            size: None,
            ignore_dependencies: false,
        };
        let spawned = hub
            .dispatch(Verb::TaskSpawn { project: project.clone(), task: TaskId(1), launch })
            .await;
        let Outcome::Task(spawned) = spawned else { panic!("{spawned:?}") };
        let cloned = linux_home.join("slopty/clones/example.com/o/demo");
        let step = spawned.step.map(|s| (s.kind, s.state));
        let detail = cloned.to_string_lossy().into_owned();
        assert_eq!(step, Some((StepKind::Clone, StepState::Done { detail })), "shown as a step");
        let config: Value =
            serde_json::from_slice(&std::fs::read(linux_home.join(".claude.json")).unwrap())
                .unwrap();
        let key = std::fs::canonicalize(&cloned).unwrap().to_string_lossy().into_owned();
        assert_eq!(
            config["projects"][&key]["hasTrustDialogAccepted"],
            Value::Bool(true),
            "the clone is trusted for its agent: {config}"
        );
        let seen: Value = until("the agent starts in the clone", async || {
            std::fs::read(&record).ok().and_then(|b| serde_json::from_slice(&b).ok())
        })
        .await;
        assert_eq!(seen["cwd"].as_str().map(PathBuf::from), Some(cloned.clone()));

        // The agent's work: a commit on its branch in a worktree of the clone, as Claude Code's
        // `--worktree` makes it.
        let tree = cloned.join(".claude/worktrees/slopty-demo-1");
        let tree_text = tree.to_string_lossy().into_owned();
        git_out(&cloned, &["worktree", "add", "-q", "-b", branch, &tree_text, "origin/main"]);
        std::fs::write(tree.join("work.txt"), "done\n").unwrap();
        git_out(&tree, &["add", "."]);
        git_out(&tree, &["commit", "-q", "-m", "the work"]);
        let head = git_out(&tree, &["rev-parse", "HEAD"]);
        std::fs::write(&gate, "").unwrap();

        let started = std::time::Instant::now();
        let arrived = until("the branch arrives home", async || {
            let card = status(&hub, &project).await.tasks.into_iter().next()?;
            match card.step.map(|s| (s.kind, s.state)) {
                Some((StepKind::Home, StepState::Done { detail })) => Some(detail),
                Some((StepKind::Home, StepState::Failed { why })) => panic!("not home: {why}"),
                _ => None,
            }
        })
        .await;
        eprintln!("brought home in {:?} after the report was allowed", started.elapsed());
        let home_branch = "slopty/demo/1";
        assert!(
            arrived
                .starts_with(&format!("{branch} as {home_branch} at {}", head.get(..7).unwrap())),
            "{arrived}"
        );
        assert_eq!(
            git_out(&studio_clone, &["rev-parse", home_branch]),
            head,
            "in the orchestrator's clone"
        );
        assert_eq!(
            git_out(&studio_clone, &["rev-parse", "origin/main"]),
            forge_main,
            "which fetched its origin for the fork point, rather than take the whole history"
        );
        let bundles = linux_home.join(".cache/slopty/bundles");
        let studio_bundles = root.join("home/.cache/slopty/bundles");
        let left = std::fs::read_dir(&studio_bundles).map_or(0, Iterator::count);
        assert_eq!(left, 0, "the bundle fetched is gone");
        assert!(bundles.exists(), "made where the branch was");

        let moments: Vec<(StepKind, bool)> = status(&hub, &project)
            .await
            .timeline
            .into_iter()
            .filter_map(|e| match e.what {
                Moment::Step(s) => Some((s.kind, matches!(s.state, StepState::Done { .. }))),
                _ => None,
            })
            .collect();
        assert_eq!(
            moments,
            [
                (StepKind::Clone, false),
                (StepKind::Clone, true),
                (StepKind::Home, false),
                (StepKind::Home, true)
            ],
            "each step's start and end on the timeline"
        );
        server.shutdown().await;
    }

    /// The card of task 1 once `done` holds of it.
    async fn card_when(
        hub: &Hub,
        project: &ProjectId,
        what: &str,
        done: impl Fn(&slopty_proto::project::TaskCard) -> bool,
    ) -> slopty_proto::project::TaskCard {
        until(what, async || {
            let card = status(hub, project).await.tasks.into_iter().next()?;
            done(&card).then_some(card)
        })
        .await
    }

    /// Each step and verdict of task 1 on the timeline, in words.
    async fn moments(hub: &Hub, project: &ProjectId) -> Vec<String> {
        use slopty_proto::project::StepState;
        status(hub, project)
            .await
            .timeline
            .into_iter()
            .filter_map(|e| match e.what {
                Moment::Step(s) => {
                    let how = match s.state {
                        StepState::Running { .. } => "began",
                        StepState::Done { .. } => "done",
                        StepState::Failed { .. } => "failed",
                    };
                    Some(format!("{:?} {how}", s.kind))
                }
                Moment::Verified(run) => Some(format!("verified {}", run.passed)),
                _ => None,
            })
            .collect()
    }

    /// The whole way, across two machines: a task's agent on the Linux worker reports done,
    /// its branch comes home to the orchestrator's clone, the project's verifier runs on it
    /// there in a checkout of its own and passes, and the merge queue fast-forwards the
    /// clone's `main` to exactly the commit verified. Its checkout moves with it. Nothing is
    /// pushed, since pushing is off until the person turns it on, and the verifier's terminal
    /// is gone once it passed.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_finished_task_is_verified_and_merged_into_the_orchestrator_s_clone() {
        use slopty_proto::project::{Merge, Placement, StepKind};
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let seed = root.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        git_out(&seed, &["init", "-q", "-b", "main"]);
        git_out(&seed, &["commit", "-q", "--allow-empty", "-m", "first"]);
        git_out(&root, &["clone", "-q", "--bare", "seed", "forge.git"]);
        let url = "https://example.com/o/demo.git";
        let forge = root.join("forge.git");
        let reach = format!("url.file://{}.insteadOf={url}", forge.display());
        git_out(&root, &["-c", &reach, "clone", "-q", url, "demo"]);
        let studio_clone = root.join("demo");
        let forge_main = git_out(&forge, &["rev-parse", "main"]);

        let config = format!("[url \"file://{}\"]\n\tinsteadOf = {url}\n", forge.display());
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::write(root.join("home/.gitconfig"), &config).unwrap();
        let (server, _daemons, studio) = fleet(&root, "").await;
        let hub = server.hub().clone();
        let linux_dir = root.join("linux");
        let linux_home = linux_dir.join("home");
        std::fs::create_dir_all(&linux_home).unwrap();
        std::fs::write(linux_home.join(".gitconfig"), config).unwrap();
        std::fs::write(linux_home.join(".claude.json"), "{}").unwrap();
        let programs = root.join("programs");
        let _linux_daemons =
            worker_named(&linux_dir, server.quic_addr(), &programs, "", "linux-box").await;
        until("the second worker registers", async || {
            (hub.directory().iter().filter(|w| w.liveness == Liveness::Online).count() == 2)
                .then_some(())
        })
        .await;

        let shell = Verb::OpenTerminal {
            worker: studio,
            cwd: Some(studio_clone.to_string_lossy().into_owned()),
            command: Vec::new(),
            env: Vec::new(),
            name: None,
            size: None,
            session: None,
        };
        let Outcome::Opened(orchestrator) = hub.dispatch(shell).await else { panic!("a shell") };
        let project = ProjectId::new("demo").unwrap();
        let verifier = "cat work.txt && grep -qx done work.txt";
        let made = hub
            .dispatch(Verb::ProjectCreate {
                project: project.clone(),
                title: "Demo".to_owned(),
                repo: "demo".to_owned(),
                target: "main".to_owned(),
                verifier: Some(verifier.to_owned()),
                push: false,
                orchestrator: Some(orchestrator),
                limits: LimitsChange::default(),
                metadata: None,
            })
            .await;
        assert!(matches!(made, Outcome::Project(_)), "{made:?}");
        until("the project learns its repository", async || {
            status(&hub, &project).await.project.repo_id.filter(|id| id.root.is_some())
        })
        .await;

        let on_linux = Placement {
            require: vec![r#"name == "linux-box""#.to_owned()],
            ..Placement::default()
        };
        let spec =
            TaskSpec { title: "Write it".to_owned(), placement: on_linux, ..TaskSpec::default() };
        let task =
            hub.dispatch(Verb::TaskCreate { project: project.clone(), spec: Box::new(spec) }).await;
        assert!(matches!(&task, Outcome::Task(t) if t.id == TaskId(1)), "{task:?}");
        let gate = root.join("go");
        let branch = "worktree-slopty-demo-1";
        let calls = json!([{ "name": "task_report", "arguments": { "kind": "done", "note": "Wrote it.", "branch": branch } }]);
        let launch = TaskLaunch {
            pin: None,
            cwd: String::new(),
            run: Runner::Claude { prompt: None, args: Vec::new() },
            env: vec![
                ("STUB_MCP_CALLS".to_owned(), calls.to_string()),
                ("STUB_MCP_AFTER".to_owned(), gate.to_string_lossy().into_owned()),
            ],
            size: None,
            ignore_dependencies: false,
        };
        let spawned = hub
            .dispatch(Verb::TaskSpawn { project: project.clone(), task: TaskId(1), launch })
            .await;
        assert!(matches!(spawned, Outcome::Task(_)), "{spawned:?}");
        let cloned = linux_home.join("slopty/clones/example.com/o/demo");
        let tree = cloned.join(".claude/worktrees/slopty-demo-1");
        let tree_text = tree.to_string_lossy().into_owned();
        git_out(&cloned, &["worktree", "add", "-q", "-b", branch, &tree_text, "origin/main"]);
        std::fs::write(tree.join("work.txt"), "done\n").unwrap();
        git_out(&tree, &["add", "."]);
        git_out(&tree, &["commit", "-q", "-m", "the work"]);
        let head = git_out(&tree, &["rev-parse", "HEAD"]);
        std::fs::write(&gate, "").unwrap();

        let started = std::time::Instant::now();
        let card = card_when(&hub, &project, "the task merges", |card| {
            card.state == TaskState::Merged
                || card.step.as_ref().is_some_and(|s| {
                    matches!(s.state, slopty_proto::project::StepState::Failed { .. })
                })
        })
        .await;
        eprintln!("MEASURE done to merged across two workers: {:?}", started.elapsed());
        assert_eq!(card.state, TaskState::Merged, "{card:?}");
        let Some(Merge::Merged { target, head: merged, pushed, .. }) = card.merge else {
            panic!("{card:?}")
        };
        assert_eq!((target.as_str(), merged.as_str(), pushed), ("main", head.as_str(), false));
        let run = card.verified.unwrap();
        assert!(run.passed && run.head == head && run.exit == Some(0), "{run:?}");
        assert_eq!(run.base, forge_main, "where the work left main");
        assert_eq!(card.step.map(|s| s.kind), Some(StepKind::Merge));

        assert_eq!(git_out(&studio_clone, &["rev-parse", "main"]), head, "main fast-forwarded");
        assert_eq!(std::fs::read_to_string(studio_clone.join("work.txt")).unwrap(), "done\n");
        assert_eq!(git_out(&forge, &["rev-parse", "main"]), forge_main, "and not pushed");
        let place = root.join("home/slopty/verify/demo");
        assert_eq!(git_out(&place, &["rev-parse", "HEAD"]), head, "verified in its own checkout");
        assert_eq!(
            moments(&hub, &project).await,
            [
                "Clone began",
                "Clone done",
                "Home began",
                "Home done",
                "Verify began",
                "verified true",
                "Merge began",
                "Merge done"
            ]
        );
        let verifier_left = until("the passed verifier's terminal closes", async || {
            match hub.dispatch(Verb::ListTerminals { worker: Some(studio) }).await {
                Outcome::Terminals(list) => {
                    list.iter().all(|(_, s)| !s.title.starts_with("Verifier")).then_some(list)
                }
                _ => None,
            }
        })
        .await;
        assert_eq!(verifier_left.len(), 1, "the orchestrator's shell alone: {verifier_left:?}");
        server.shutdown().await;
    }

    /// One worker, with the task's agent in a worktree of the orchestrator's clone. Its
    /// verifier fails: the task is given back, its agent's next prompt brings the report through
    /// its hooks with the verifier's last lines, and the failed run's terminal stays to be read.
    /// The person meanwhile commits on `main` over the file the task changed. The agent fixes
    /// its work, the person asks for the merge with `slopty task merge`, the verifier passes,
    /// and the queue's rebase conflicts: the task is given back again with the path, and
    /// `main` keeps the person's commit.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_verifier_and_a_conflict_go_back_to_the_agent() {
        use slopty_proto::project::{StepKind, StepState};
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let repo = root.join("demo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "first"]);
        git(&repo, &["remote", "add", "origin", "git@github.com:aislopware/demo.git"]);
        let tree = repo.join(".claude/worktrees/t1");
        git(&repo, &["worktree", "add", "-q", "-b", "task-1", &tree.to_string_lossy(), "main"]);
        std::fs::write(tree.join("a.txt"), "the task's\n").unwrap();
        std::fs::write(tree.join("work.txt"), "not yet\n").unwrap();
        git(&tree, &["add", "."]);
        git(&tree, &["commit", "-q", "-m", "the work, unfinished"]);
        let (server, _daemons, worker) = fleet(&root, "").await;
        let hub = server.hub().clone();

        let shell = Verb::OpenTerminal {
            worker,
            cwd: Some(repo.to_string_lossy().into_owned()),
            command: Vec::new(),
            env: Vec::new(),
            name: None,
            size: None,
            session: None,
        };
        let Outcome::Opened(orchestrator) = hub.dispatch(shell).await else { panic!("a shell") };
        let project = ProjectId::new("demo").unwrap();
        let verifier = "cat work.txt && grep -qx done work.txt";
        let made = hub
            .dispatch(Verb::ProjectCreate {
                project: project.clone(),
                title: "Demo".to_owned(),
                repo: "demo".to_owned(),
                target: "main".to_owned(),
                verifier: Some(verifier.to_owned()),
                push: false,
                orchestrator: Some(orchestrator),
                limits: LimitsChange::default(),
                metadata: None,
            })
            .await;
        assert!(matches!(made, Outcome::Project(_)), "{made:?}");
        until("the project learns its repository", async || {
            status(&hub, &project).await.project.repo_id.filter(|id| id.root.is_some())
        })
        .await;
        let spec = TaskSpec { title: "Write it".to_owned(), ..TaskSpec::default() };
        let task =
            hub.dispatch(Verb::TaskCreate { project: project.clone(), spec: Box::new(spec) }).await;
        assert!(matches!(&task, Outcome::Task(t) if t.id == TaskId(1)), "{task:?}");
        let (record, gate, later) = (root.join("record.json"), root.join("go"), root.join("later"));
        std::fs::write(&gate, "").unwrap();
        let calls = json!([{ "name": "task_report", "arguments": { "kind": "done", "note": "Wrote it.", "branch": "task-1" } }]);
        let prompt = json!([{ "hook_event_name": "UserPromptSubmit", "prompt": "go on" }]);
        let launch = TaskLaunch {
            pin: Some(worker),
            cwd: tree.to_string_lossy().into_owned(),
            run: Runner::Claude { prompt: None, args: Vec::new() },
            env: vec![
                ("STUB_RECORD".to_owned(), record.to_string_lossy().into_owned()),
                ("STUB_MCP_CALLS".to_owned(), calls.to_string()),
                ("STUB_MCP_AFTER".to_owned(), gate.to_string_lossy().into_owned()),
                ("STUB_LATER".to_owned(), prompt.to_string()),
                ("STUB_LATER_AFTER".to_owned(), later.to_string_lossy().into_owned()),
            ],
            size: None,
            ignore_dependencies: false,
        };
        let spawned = hub
            .dispatch(Verb::TaskSpawn { project: project.clone(), task: TaskId(1), launch })
            .await;
        let Outcome::Task(spawned) = spawned else { panic!("{spawned:?}") };
        let agent = spawned.assignment.unwrap().term;

        let card = card_when(&hub, &project, "the verifier fails", |card| {
            card.verified.as_ref().is_some_and(|run| !run.passed)
                && card.state == TaskState::Waiting
        })
        .await;
        let run = card.verified.unwrap();
        assert_eq!(run.exit, Some(1));
        assert!(run.summary.contains("not yet"), "its last lines: {:?}", run.summary);
        assert_eq!(run.head, git_out(&tree, &["rev-parse", "HEAD"]));
        let step = card.step.unwrap();
        assert_eq!(step.kind, StepKind::Verify);
        let kept = step.term.expect("the failed run's terminal");
        let listed = match hub.dispatch(Verb::ListTerminals { worker: Some(worker) }).await {
            Outcome::Terminals(list) => list,
            other => panic!("{other:?}"),
        };
        assert!(listed.iter().any(|(_, s)| s.id == kept.session), "kept to be read");

        let reports = slopty_agent::reports::dir(&root.join("worker.sock"));
        until("the report waits for the agent's hooks", async || {
            reports.join(format!("{}.json", agent.session)).exists().then_some(())
        })
        .await;
        std::fs::write(&later, "").unwrap();
        let seen: Value = until("the agent's next prompt hands it the report", async || {
            let seen: Value = serde_json::from_slice(&std::fs::read(&record).ok()?).ok()?;
            (seen["hooks"].as_array()?.len() >= 2).then_some(seen)
        })
        .await;
        let handed = seen["hooks"][1]["outputs"][0]["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        for words in [&format!("The verifier `{verifier}` failed"), "exit 1", "not yet"] {
            assert!(handed.contains(words), "{words:?} in {handed}");
        }
        assert_eq!(seen["typed"], json!([]), "nothing typed into the agent");

        std::fs::write(repo.join("a.txt"), "main's own\n").unwrap();
        git(&repo, &["commit", "-q", "-am", "the person's change"]);
        let person_main = git_out(&repo, &["rev-parse", "main"]);
        std::fs::write(tree.join("work.txt"), "done\n").unwrap();
        git(&tree, &["commit", "-q", "-am", "the work, done"]);
        let fixed = git_out(&tree, &["rev-parse", "HEAD"]);
        let asked = slopty(
            &root,
            server.quic_addr(),
            &["task", "merge", "--project", "demo", "--task", "1"],
        )
        .await;
        assert!(asked.contains("#1"), "{asked}");

        let card = card_when(&hub, &project, "the queue gives it back", |card| {
            card.step.as_ref().is_some_and(|s| {
                s.kind == StepKind::Merge && matches!(s.state, StepState::Failed { .. })
            })
        })
        .await;
        let run = card.verified.unwrap();
        assert!(run.passed && run.head == fixed, "the fix passed first: {run:?}");
        let Some(StepState::Failed { why }) = card.step.map(|s| s.state) else { panic!("failed") };
        assert!(why.contains("a.txt"), "{why}");
        assert_eq!((card.state, card.merge), (TaskState::Waiting, None));
        assert_eq!(git_out(&repo, &["rev-parse", "main"]), person_main, "main keeps the person's");
        let place = root.join("home/slopty/verify/demo");
        let clean = git_out(&place, &["status", "--porcelain"]);
        assert!(clean.is_empty(), "no rebase left stopped: {clean}");
        let gone = match hub.dispatch(Verb::ListTerminals { worker: Some(worker) }).await {
            Outcome::Terminals(list) => list.iter().all(|(_, s)| s.id != kept.session),
            other => panic!("{other:?}"),
        };
        assert!(gone, "the kept terminal closed once it was verified again");
        assert_eq!(
            moments(&hub, &project).await,
            [
                "Verify began",
                "verified false",
                "Verify began",
                "verified true",
                "Merge began",
                "Merge failed"
            ]
        );
        server.shutdown().await;
    }
}
