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
    use slopty_proto::orchestration::{ErrorCode, Outcome, Verb};
    use slopty_proto::project::{
        LimitsChange, Moment, Natives, ProjectId, ProjectStatus, TaskId, TaskLaunch, TaskSpec,
        TaskState,
    };
    use slopty_proto::server::Liveness;
    use slopty_proto::thread::AgentId;
    use slopty_server::{Config, Hub, Server};
    use tokio::io::{AsyncBufReadExt as _, BufReader};
    use tokio::process::{Child, Command};

    const STEP: Duration = Duration::from_secs(60);

    /// What the daemons log: enough of a question's path (each hook heard, a prompt held or
    /// left to the TUI, a brief waiting, ptyd taking and typing a delivery) that a run that
    /// fails names its cause. The runner shows a test's output only when it fails.
    const DAEMON_LOG: &str = "info,slopty_workerd::ctl=debug,slopty_workerd::threads::hold=debug,\
slopty_worker::orchestrate=debug,slopty_ptyd=debug";

    /// The in-process server's log on this test's output, beside its daemons'.
    fn server_log() {
        let _first = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
            .with_test_writer()
            .try_init();
    }

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
    /// Every terminal ptyd starts has `env` beside its own, as a stub started by the server
    /// rather than the test gets its script.
    async fn worker(
        dir: &Path,
        server: SocketAddr,
        programs: &Path,
        settings: &str,
        env: &[(String, String)],
    ) -> Vec<Child> {
        daemons(dir, server, programs, settings, "projects-test", env).await
    }

    /// [`worker`] under `name`.
    async fn daemons(
        dir: &Path,
        server: SocketAddr,
        programs: &Path,
        settings: &str,
        name: &str,
        env: &[(String, String)],
    ) -> Vec<Child> {
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(dir.join("data").join("settings.toml"), settings).unwrap();
        let path = slopty_testkit::env::path_with(programs);
        let ptyd_sock = dir.join("ptyd.sock");
        let mut ptyd = scrubbed(bin("slopty-ptyd"), &dir.join("home"))
            .env("PATH", &path)
            .env("RUST_LOG", DAEMON_LOG)
            .envs(env.iter().cloned())
            .arg("--socket")
            .arg(&ptyd_sock)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        slopty_testkit::group::ward(ptyd.id());
        let ready = async {
            while tokio::net::UnixStream::connect(&ptyd_sock).await.is_err() {
                assert!(ptyd.try_wait().unwrap().is_none(), "ptyd exited early");
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        };
        tokio::time::timeout(STEP, ready).await.expect("ptyd socket");
        let worker = worker_daemon(dir, server, &path, name).await;
        vec![ptyd, worker]
    }

    /// The worker of [`daemons`] alone, as it starts again over the ptyd it left: under
    /// `name`, with `path` as its `PATH`, once it says its address.
    async fn worker_daemon(
        dir: &Path,
        server: SocketAddr,
        path: &std::ffi::OsStr,
        name: &str,
    ) -> Child {
        let ptyd_sock = dir.join("ptyd.sock");
        let mut worker = scrubbed(bin("slopty-worker"), &dir.join("home"))
            .env("PATH", path)
            .env("RUST_LOG", DAEMON_LOG)
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
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        slopty_testkit::group::ward(worker.id());
        let mut line = String::new();
        let stdout = worker.stdout.take().unwrap();
        tokio::time::timeout(STEP, BufReader::new(stdout).read_line(&mut line))
            .await
            .expect("the worker prints its address")
            .unwrap();
        worker
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

    /// Wait until the worker `name` is online and has said Claude Code is installed there. A
    /// worker registers before it has looked for its agents, and until it says, placement
    /// counts it as having none: a task put on it then is refused as unplaced. Its id.
    async fn registered_with_claude(hub: &Hub, name: &str) -> WorkerId {
        until(&format!("{name} registers with Claude Code installed"), async || {
            hub.directory()
                .into_iter()
                .find(|w| {
                    w.name == name
                        && w.liveness == Liveness::Online
                        && w.caps.agents.iter().any(|a| a.agent.is(AgentId::CLAUDE_CODE))
                })
                .map(|w| w.worker)
        })
        .await
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
        fleet_with(root, settings, &[]).await
    }

    /// [`fleet`], with `env` in every terminal its worker starts ([`worker`]).
    async fn fleet_with(
        root: &Path,
        settings: &str,
        env: &[(String, String)],
    ) -> (Server, Vec<Child>, WorkerId) {
        let server = Server::start(Config {
            name: "projects-test".to_owned(),
            quic: "127.0.0.1:0".parse().unwrap(),
            data_dir: root.join("server"),
            admission: slopty_net::admission::Admission::default(),
            push: slopty_server::PushConfig::Off,
        })
        .await
        .unwrap();
        let hub = server.hub().clone();
        let programs = root.join("programs");
        std::fs::create_dir_all(&programs).unwrap();
        std::os::unix::fs::symlink(bin("slopty-stub-claude"), programs.join("claude")).unwrap();
        let daemons = worker(root, server.quic_addr(), &programs, settings, env).await;
        let worker = until("the worker registers with Claude Code installed", async || {
            hub.directory().into_iter().find(|w| {
                w.liveness == Liveness::Online
                    && w.caps.agents.iter().any(|a| a.agent.is(AgentId::CLAUDE_CODE))
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
    /// as the task's. Its first prompt, the task's brief, is typed once it says it is ready.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_agent_started_for_a_task_has_the_tools_and_grows_the_tree() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
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
            hook(
                "Statusline",
                &json!({
                    "worktree": worktree,
                    "meters": { "five_hour": { "used_pct": 40.0, "resets_at": null } },
                }),
            ),
        ]);
        let calls = json!([
            { "name": "project_status", "arguments": {} },
            {
                "name": "task_report",
                "arguments": { "note": "Read the entry point." },
            },
        ]);
        let env = [
            ("STUB_RECORD".to_owned(), record.to_string_lossy().into_owned()),
            ("STUB_HOOKS".to_owned(), hooks.to_string()),
            ("STUB_MCP_CALLS".to_owned(), calls.to_string()),
        ];
        let (server, _daemons, worker) = fleet_with(&root, "", &env).await;
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
                goal: None,
                autonomy: slopty_proto::project::Autonomy::Ask,
            })
            .await;
        assert!(matches!(made, Outcome::Project(_)), "{made:?}");
        let task = hub
            .dispatch(Verb::TaskCreate {
                project: project.clone(),
                spec: Box::new(TaskSpec {
                    title: "Look around".to_owned(),
                    brief: "Read the code.".to_owned(),
                    ..TaskSpec::default()
                }),
            })
            .await;
        assert!(matches!(&task, Outcome::Task(t) if t.id == TaskId(1)), "{task:?}");

        let launch = TaskLaunch { pin: None, agent: AgentId::named(AgentId::CLAUDE_CODE) };
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
                    .is_some_and(|t| t.iter().any(|l| l == "Read the code."));
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
            panic!("the agent answered its tool call and took its brief; it recorded {recorded}")
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

    /// `slopty <args>` against `server` that fails: what it said on stderr.
    async fn slopty_refused(root: &Path, server: SocketAddr, args: &[&str]) -> String {
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
        assert!(!out.status.success(), "slopty {args:?} should fail");
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    /// A folder is made, moved and renamed on a real worker through the `slopty` verbs an
    /// agent's tools share, each printing where the entry now is. What must not happen is
    /// refused in plain words and touches nothing: a second folder where one is, a move onto
    /// something there, and the home to the trash.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_worker_s_folders_are_made_and_moved_and_a_refusal_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (server, _daemons, _worker) = fleet(&root, "").await;
        let addr = server.quic_addr();
        let home = root.join("home");

        let made = slopty(&root, addr, &["mkdir", "~/drafts"]).await;
        assert_eq!(made.trim(), home.join("drafts").to_string_lossy(), "{made}");
        assert!(home.join("drafts").is_dir());
        let again = slopty_refused(&root, addr, &["mkdir", "~/drafts"]).await;
        assert!(again.contains("something is already at"), "{again}");

        std::fs::write(home.join("drafts").join("note.md"), "kept").unwrap();
        let moved = slopty(&root, addr, &["--json", "mv", "~/drafts", "~/notes"]).await;
        let moved: Value = serde_json::from_str(&moved).unwrap();
        assert_eq!(moved["path"], home.join("notes").to_string_lossy().as_ref(), "{moved}");
        assert_eq!(std::fs::read_to_string(home.join("notes").join("note.md")).unwrap(), "kept");
        assert!(!home.join("drafts").exists());

        std::fs::create_dir_all(home.join("taken")).unwrap();
        let onto = slopty_refused(&root, addr, &["mv", "~/notes", "~/taken"]).await;
        assert!(onto.contains("nothing was touched") && onto.contains("taken"), "{onto}");
        assert!(home.join("notes").join("note.md").is_file(), "left where it was");
        let inside = slopty_refused(&root, addr, &["mv", "~/notes", "~/notes/deeper"]).await;
        assert!(inside.contains("into itself"), "{inside}");
        let the_home = slopty_refused(&root, addr, &["trash", "~"]).await;
        assert!(the_home.contains("never moved or trashed"), "{the_home}");
        assert!(home.join("notes").is_dir());

        server.shutdown().await;
    }

    /// What a worker has reaches `slopty workers` as facts. A task made and started in one
    /// `slopty task start` runs on the worker it names, with its project and task in its
    /// agent's environment.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_worker_s_own_facts_are_listed_and_a_task_runs_where_it_says() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let record = root.join("record.json");
        let env = [("STUB_RECORD".to_owned(), record.to_string_lossy().into_owned())];
        let (server, _daemons, worker) = fleet_with(&root, "", &env).await;
        let hub = server.hub().clone();
        let addr = server.quic_addr();

        let listed: Value = until("the worker's facts reach `slopty workers`", async || {
            let listed: Value =
                serde_json::from_str(&slopty(&root, addr, &["--json", "workers"]).await).ok()?;
            let facts = &listed.as_array()?.first()?["facts"];
            facts["toolchains"].is_object().then_some(listed)
        })
        .await;
        let facts = &listed[0]["facts"];
        assert_eq!(facts["os"], std::env::consts::OS, "{facts}");

        let repo = root.to_string_lossy().into_owned();
        let create = ["project", "create", "demo", "--title", "Demo", "--repo", &repo];
        slopty(&root, addr, &[&create[..], &["--target", "main"]].concat()).await;
        slopty(
            &root,
            addr,
            &[
                "task",
                "start",
                "--project",
                "demo",
                "--title",
                "Check the rack",
                "--kind",
                "check",
                "--read-only",
                "--worker",
                &worker.to_string(),
            ],
        )
        .await;
        let seen: Value = until("its agent started", async || {
            serde_json::from_slice(&std::fs::read(&record).ok()?).ok()
        })
        .await;
        let env = &seen["env"];
        assert_eq!(
            (env["SLOPTY_PROJECT"].as_str(), env["SLOPTY_TASK"].as_str()),
            (Some("demo"), Some("1"))
        );
        let project = ProjectId::new("demo").unwrap();
        let now = status(&hub, &project).await;
        let task = &now.tasks[0];
        assert_eq!((task.kind.as_str(), task.read_only), ("check", true));
        let term = task.assignment.as_ref().expect("placed and running").term;
        assert_eq!(term.worker, worker);
        assert_eq!(now.live.project, 1, "its agent counts");

        server.shutdown().await;
    }

    /// An agent's environment does not put it on a task: one started outside every task with
    /// a `SLOPTY_PROJECT` and `SLOPTY_TASK` of its own is refused changing that task and
    /// reporting on it, since its terminal's token proves it works on none, and the task is as
    /// it was.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_agent_s_environment_does_not_put_it_on_a_task() {
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
                goal: None,
                autonomy: slopty_proto::project::Autonomy::Ask,
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
        let calls = json!([
            { "name": "task_update", "arguments": { "status": "reviewing" } },
            { "name": "task_report", "arguments": { "note": "Reviewed." } },
        ]);
        let spawned = hub
            .dispatch(Verb::SpawnAgent {
                worker,
                cwd: root.to_string_lossy().into_owned(),
                prompt: None,
                args: Vec::new(),
                env: vec![
                    ("SLOPTY_PROJECT".to_owned(), "demo".to_owned()),
                    ("SLOPTY_TASK".to_owned(), "2".to_owned()),
                    ("STUB_RECORD".to_owned(), record.to_string_lossy().into_owned()),
                    ("STUB_MCP_CALLS".to_owned(), calls.to_string()),
                ],
                size: None,
                session: None,
                autonomy: Some(slopty_proto::project::Autonomy::Ask),
                worktree: None,
            })
            .await;
        assert!(matches!(spawned, Outcome::Opened(_)), "{spawned:?}");

        let seen: Value = until("the agent's tool call answered", async || {
            let seen: Value = serde_json::from_slice(&std::fs::read(&record).ok()?).ok()?;
            (seen["mcp"].as_array()?.len() == 2).then_some(seen)
        })
        .await;
        assert_eq!(seen["env"]["SLOPTY_TASK"], "2", "the environment it was given");
        assert_eq!(seen["mcp"][0]["isError"], true, "{}", seen["mcp"][0]);
        assert_eq!(seen["mcp"][1]["isError"], true, "{}", seen["mcp"][1]);
        let now = status(&hub, &project).await;
        assert!(now.tasks.iter().all(|t| t.status.is_none()), "{:?}", now.tasks);
        assert!(now.tasks.iter().all(|t| t.state == TaskState::Planned), "{:?}", now.tasks);

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
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
        git(&repo, &["remote", "add", "origin", "git@github.com:aislopware/demo.git"]);
        let record = root.join("record.json");
        let env = [("STUB_RECORD".to_owned(), record.to_string_lossy().into_owned())];
        let (server, _daemons, worker) = fleet_with(&root, "", &env).await;
        let hub = server.hub().clone();

        let shell = Verb::OpenTerminal {
            worker,
            cwd: Some(repo.to_string_lossy().into_owned()),
            command: Vec::new(),
            env: Vec::new(),
            name: None,
            size: None,
            session: None,
            worktree: None,
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
                goal: None,
                autonomy: slopty_proto::project::Autonomy::Ask,
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
        let launch = TaskLaunch { pin: None, agent: AgentId::named(AgentId::CLAUDE_CODE) };
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
    /// another with none. A task started on the other gets a clone there first, made by
    /// that worker's own git from the address the orchestrator's clone names (its person's
    /// `insteadOf` reaching the forge), shown as a step. Once its agent reports done, the
    /// branch it committed comes home: bundled there, carried by the server, fetched into the
    /// orchestrator's clone at the same commit, and the card says it arrived.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_task_s_clone_is_made_where_it_runs_and_its_branch_comes_home() {
        use slopty_proto::project::{StepKind, StepState};
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
        let (record, gate) = (root.join("record.json"), root.join("go"));
        let branch = "worktree-slopty-demo-1";
        let calls = json!([{ "name": "task_report", "arguments": { "note": "Wrote it.", "branch": branch } }]);
        let env = [
            ("STUB_RECORD".to_owned(), record.to_string_lossy().into_owned()),
            ("STUB_MCP_CALLS".to_owned(), calls.to_string()),
            ("STUB_MCP_AFTER".to_owned(), gate.to_string_lossy().into_owned()),
        ];
        let addr = server.quic_addr();
        let _linux_daemons = daemons(&linux_dir, addr, &programs, "", "linux-box", &env).await;
        let linux_worker = registered_with_claude(&hub, "linux-box").await;

        let shell = Verb::OpenTerminal {
            worker: studio,
            cwd: Some(studio_clone.to_string_lossy().into_owned()),
            command: Vec::new(),
            env: Vec::new(),
            name: None,
            size: None,
            session: None,
            worktree: None,
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
                goal: None,
                autonomy: slopty_proto::project::Autonomy::Ask,
            })
            .await;
        assert!(matches!(made, Outcome::Project(_)), "{made:?}");
        let id = until("the project learns its repository", async || {
            status(&hub, &project).await.project.repo_id.filter(|id| id.root.is_some())
        })
        .await;
        assert_eq!(id.url.as_deref(), Some(url), "the address to clone from, as its config has it");

        let spec = TaskSpec { title: "Write it".to_owned(), ..TaskSpec::default() };
        let task =
            hub.dispatch(Verb::TaskCreate { project: project.clone(), spec: Box::new(spec) }).await;
        assert!(matches!(&task, Outcome::Task(t) if t.id == TaskId(1)), "{task:?}");
        let launch =
            TaskLaunch { pin: Some(linux_worker), agent: AgentId::named(AgentId::CLAUDE_CODE) };
        let spawned = hub
            .dispatch(Verb::TaskSpawn { project: project.clone(), task: TaskId(1), launch })
            .await;
        let Outcome::Task(spawned) = spawned else { panic!("{spawned:?}") };
        let placed = spawned.step.map(|s| s.kind);
        assert_eq!(placed, Some(StepKind::Clone), "answered once placed, its clone to make");
        let cloned = linux_home.join("slopty/clones/example.com/o/demo");
        let seen: Value = until("the agent starts in the clone", async || {
            std::fs::read(&record).ok().and_then(|b| serde_json::from_slice(&b).ok())
        })
        .await;
        assert_eq!(seen["cwd"].as_str().map(PathBuf::from), Some(cloned.clone()));
        let card = status(&hub, &project).await.tasks.into_iter().next().unwrap();
        let step = card.step.map(|s| (s.kind, s.state));
        // The clone is made, and then the target is there as the forge has it: nothing was
        // merged in the orchestrator's clone that the forge lacks.
        let detail = "main from its origin".to_owned();
        assert_eq!(step, Some((StepKind::Send, StepState::Done { detail })), "shown as a step");
        let config: Value =
            serde_json::from_slice(&std::fs::read(linux_home.join(".claude.json")).unwrap())
                .unwrap();
        let key = std::fs::canonicalize(&cloned).unwrap().to_string_lossy().into_owned();
        assert_eq!(
            config["projects"][&key]["hasTrustDialogAccepted"],
            Value::Bool(true),
            "the clone is trusted for its agent: {config}"
        );

        // The agent's work: a commit on its branch in the worktree of the clone the worker made
        // for it, which Claude Code's `--worktree` reopens.
        let tree = cloned.join(".claude/worktrees/slopty-demo-1");
        assert_eq!(git_out(&tree, &["branch", "--show-current"]), branch);
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
                (StepKind::Send, false),
                (StepKind::Send, true),
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

    /// Two workers, as the person has them: the studio, with the orchestrator's shell in its
    /// clone of a forge, and a Linux box. One task, placed on the Linux box, whose stub agent
    /// reports done on `branch` once `gate` is there, with `env` beside it. Its worktree is
    /// made in the clone the server had made there, at the forge's `main`, for the test to
    /// commit the agent's work in.
    struct Across {
        _dir: tempfile::TempDir,
        root: PathBuf,
        server: Server,
        _daemons: Vec<Child>,
        _linux: Vec<Child>,
        studio: WorkerId,
        studio_clone: PathBuf,
        linux_dir: PathBuf,
        forge: PathBuf,
        forge_main: String,
        project: ProjectId,
        agent: slopty_proto::orchestration::TermRef,
        cloned: PathBuf,
        tree: PathBuf,
        gate: PathBuf,
    }

    const ACROSS_BRANCH: &str = "worktree-slopty-demo-1";

    async fn across(verifier: &str, env: Vec<(String, String)>) -> Across {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let seed = root.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        git_out(&seed, &["init", "-q", "-b", "main"]);
        std::fs::write(seed.join("a.txt"), "one\n").unwrap();
        git_out(&seed, &["add", "."]);
        git_out(&seed, &["commit", "-q", "-m", "first"]);
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
        let (server, studio_daemons, studio) = fleet(&root, "").await;
        let hub = server.hub().clone();
        let linux_dir = root.join("linux");
        let linux_home = linux_dir.join("home");
        std::fs::create_dir_all(&linux_home).unwrap();
        std::fs::write(linux_home.join(".gitconfig"), config).unwrap();
        std::fs::write(linux_home.join(".claude.json"), "{}").unwrap();
        let programs = root.join("programs");
        let gate = root.join("go");
        let calls = json!([{ "name": "task_report", "arguments": { "note": "Wrote it.", "branch": ACROSS_BRANCH } }]);
        let mut all = vec![
            ("STUB_MCP_CALLS".to_owned(), calls.to_string()),
            ("STUB_MCP_AFTER".to_owned(), gate.to_string_lossy().into_owned()),
        ];
        all.extend(env);
        let addr = server.quic_addr();
        let linux = daemons(&linux_dir, addr, &programs, "", "linux-box", &all).await;
        let linux_worker = registered_with_claude(&hub, "linux-box").await;

        let shell = Verb::OpenTerminal {
            worker: studio,
            cwd: Some(studio_clone.to_string_lossy().into_owned()),
            command: Vec::new(),
            env: Vec::new(),
            name: None,
            size: None,
            session: None,
            worktree: None,
        };
        let Outcome::Opened(orchestrator) = hub.dispatch(shell).await else { panic!("a shell") };
        let project = ProjectId::new("demo").unwrap();
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
                goal: None,
                autonomy: slopty_proto::project::Autonomy::Ask,
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
        let launch =
            TaskLaunch { pin: Some(linux_worker), agent: AgentId::named(AgentId::CLAUDE_CODE) };
        let spawned = hub
            .dispatch(Verb::TaskSpawn { project: project.clone(), task: TaskId(1), launch })
            .await;
        // Placed on the other machine, it answers before the target's trip there and its start.
        assert!(matches!(spawned, Outcome::Task(_)), "{spawned:?}");
        let agent = until("its agent started", async || {
            status(&hub, &project).await.tasks.first()?.assignment.as_ref().map(|a| a.term)
        })
        .await;
        let cloned = linux_home.join("slopty/clones/example.com/o/demo");
        let tree = cloned.join(".claude/worktrees/slopty-demo-1");
        assert_eq!(git_out(&tree, &["branch", "--show-current"]), ACROSS_BRANCH, "made for it");
        Across {
            _dir: dir,
            root,
            server,
            _daemons: studio_daemons,
            _linux: linux,
            studio,
            studio_clone,
            linux_dir,
            forge,
            forge_main,
            project,
            agent,
            cloned,
            tree,
            gate,
        }
    }

    /// The whole way, across two machines: a task's agent on the Linux worker reports done,
    /// its branch comes home to the orchestrator's clone, the project's verifier runs on it
    /// there in a checkout of its own and passes, and the task waits ready to merge. On the
    /// person's Merge the queue fast-forwards the clone's `main` to the tree verified, its
    /// commits carrying the task they came from. Its checkout moves with it. Nothing is
    /// pushed, since pushing is off until the person turns it on, and the verifier's terminal
    /// is gone once it passed.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_finished_task_is_verified_and_merged_into_the_orchestrator_s_clone() {
        use slopty_proto::project::{Merge, StepKind};
        let fix = across("cat work.txt && grep -qx done work.txt", Vec::new()).await;
        let Across {
            root,
            server,
            studio,
            studio_clone,
            forge,
            forge_main,
            project,
            tree,
            gate,
            ..
        } = &fix;
        let hub = server.hub().clone();
        std::fs::write(tree.join("work.txt"), "done\n").unwrap();
        git_out(tree, &["add", "."]);
        git_out(tree, &["commit", "-q", "-m", "the work"]);
        let head = git_out(tree, &["rev-parse", "HEAD"]);
        std::fs::write(gate, "").unwrap();

        let failed = |card: &slopty_proto::project::TaskCard| {
            card.step
                .as_ref()
                .is_some_and(|s| matches!(s.state, slopty_proto::project::StepState::Failed { .. }))
        };
        let started = std::time::Instant::now();
        let ready = card_when(&hub, project, "the task is ready to merge", |card| {
            let checked = card.verified.as_ref().is_some_and(|r| r.passed);
            (card.state == TaskState::Done && checked) || failed(card)
        })
        .await;
        eprintln!("MEASURE done to ready across two workers: {:?}", started.elapsed());
        assert_eq!((ready.state, &ready.merge), (TaskState::Done, &None), "waits: {ready:?}");
        let asked = hub.dispatch(Verb::TaskMerge { project: project.clone(), task: TaskId(1) });
        assert!(matches!(asked.await, Outcome::Task(_)), "the person's Merge");
        let card = card_when(&hub, project, "the task merges", |card| {
            card.state == TaskState::Merged || failed(card)
        })
        .await;
        assert_eq!(card.state, TaskState::Merged, "{card:?}");
        let Some(Merge::Merged { target, head: merged, pushed, .. }) = card.merge else {
            panic!("{card:?}")
        };
        assert_eq!((target.as_str(), pushed), ("main", false));
        let tree_of =
            |dir: &Path, commit: &str| git_out(dir, &["rev-parse", &format!("{commit}^{{tree}}")]);
        assert_eq!(tree_of(studio_clone, &merged), tree_of(tree, &head), "the tree verified");
        let trailer = git_out(
            studio_clone,
            &["log", "-1", "--format=%(trailers:key=Slopty-Task,valueonly)", &merged],
        );
        assert_eq!(trailer.trim(), "demo#1", "where it came from");
        let run = card.verified.unwrap();
        assert!(run.passed && run.head == head && run.exit == Some(0), "{run:?}");
        assert_eq!(&run.base, forge_main, "where the work left main");
        assert_eq!(card.step.map(|s| s.kind), Some(StepKind::Merge));

        assert_eq!(git_out(studio_clone, &["rev-parse", "main"]), merged, "main fast-forwarded");
        assert_eq!(std::fs::read_to_string(studio_clone.join("work.txt")).unwrap(), "done\n");
        assert_eq!(git_out(forge, &["rev-parse", "main"]), *forge_main, "and not pushed");
        let home = until("the work brought home goes with its merge", async || {
            let left = git_out(studio_clone, &["branch", "--list", "slopty/demo/1"]);
            left.is_empty().then_some(left)
        })
        .await;
        assert_eq!(home, "", "slopty/demo/1 is gone from the orchestrator's clone");
        let place = root.join("home/slopty/verify/demo");
        let verified_tree = tree_of(&place, "HEAD");
        assert_eq!(verified_tree, tree_of(tree, &head), "verified in its own checkout");
        assert_eq!(
            moments(&hub, project).await,
            [
                "Clone began",
                "Clone done",
                "Send began",
                "Send done",
                "Home began",
                "Home done",
                "Verify began",
                "verified true",
                "Home began",
                "Home done",
                "Merge began",
                "Merge done"
            ]
        );
        let verifier_left = until("the passed verifier's terminal closes", async || {
            match hub.dispatch(Verb::ListTerminals { worker: Some(*studio) }).await {
                Outcome::Terminals { terminals: list, .. } => {
                    list.iter().all(|(_, s)| !s.title.starts_with("Verifier")).then_some(list)
                }
                _ => None,
            }
        })
        .await;
        assert_eq!(verifier_left.len(), 1, "the orchestrator's shell alone: {verifier_left:?}");
        fix.server.shutdown().await;
    }

    /// Across two machines with pushing off, the queue's rebase conflicts with what the person
    /// committed on the orchestrator's `main`, which the forge never saw. The task goes back,
    /// and that `main` is sent to the agent's clone as `slopty/demo/target`. The agent's report
    /// names it, so the agent can rebase onto it there. Once it has, the person asks for the
    /// merge again, the newer work comes home first, it is verified, and `main` fast-forwards
    /// to its tree: the person's commit and the agent's on top, nothing pushed.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_conflict_on_another_machine_brings_it_the_target_to_rebase_onto() {
        use slopty_proto::project::{StepKind, StepState};
        let fix = across("test -f work.txt", Vec::new()).await;
        let hub = fix.server.hub().clone();
        let project = &fix.project;

        std::fs::write(fix.studio_clone.join("a.txt"), "main's own\n").unwrap();
        git_out(&fix.studio_clone, &["commit", "-q", "-am", "the person's change"]);
        let person = git_out(&fix.studio_clone, &["rev-parse", "main"]);
        std::fs::write(fix.tree.join("a.txt"), "the task's\n").unwrap();
        std::fs::write(fix.tree.join("work.txt"), "done\n").unwrap();
        git_out(&fix.tree, &["add", "."]);
        git_out(&fix.tree, &["commit", "-q", "-m", "the work"]);
        std::fs::write(&fix.gate, "").unwrap();

        card_when(&hub, project, "the task is ready to merge", |card| {
            card.state == TaskState::Done && card.verified.as_ref().is_some_and(|r| r.passed)
        })
        .await;
        let asked = hub.dispatch(Verb::TaskMerge { project: project.clone(), task: TaskId(1) });
        assert!(matches!(asked.await, Outcome::Task(_)), "the person's Merge");
        let card = card_when(&hub, project, "the queue gives it back", |card| {
            card.step.as_ref().is_some_and(|s| {
                s.kind == StepKind::Rebase && matches!(s.state, StepState::Failed { .. })
            })
        })
        .await;
        assert_eq!(card.state, TaskState::Waiting, "{card:?}");
        let arrived = git_out(&fix.cloned, &["rev-parse", "slopty/demo/target"]);
        assert_eq!(arrived, person, "the orchestrator's main, in the agent's clone");
        assert_eq!(git_out(&fix.forge, &["rev-parse", "main"]), fix.forge_main, "never pushed");

        let reports = slopty_agent::reports::dir(&fix.linux_dir.join("worker.sock"));
        let batch = until("the report waits for the agent's hooks", async || {
            std::fs::read_to_string(reports.join(format!("{}.json", fix.agent.session))).ok()
        })
        .await;
        assert!(batch.contains("slopty/demo/target"), "the report says where: {batch}");

        let rebase = std::process::Command::new("git")
            .current_dir(&fix.tree)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "rebase",
                "-q",
                "slopty/demo/target",
            ])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(!rebase.status.success(), "the agent meets the same conflict");
        std::fs::write(fix.tree.join("a.txt"), "main's own\nthe task's\n").unwrap();
        git_out(&fix.tree, &["add", "a.txt"]);
        let go_on = std::process::Command::new("git")
            .current_dir(&fix.tree)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "core.editor=true"])
            .args(["rebase", "--continue"])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(go_on.status.success(), "{}", String::from_utf8_lossy(&go_on.stderr));
        let resolved = git_out(&fix.tree, &["rev-parse", "HEAD"]);
        assert_eq!(git_out(&fix.tree, &["rev-parse", "HEAD~1"]), person);

        let asked = slopty(
            &fix.root,
            fix.server.quic_addr(),
            &["task", "merge", "--project", "demo", "--task", "1"],
        )
        .await;
        assert!(asked.contains("#1"), "{asked}");
        card_when(&hub, project, "the resolved work merges", |card| {
            card.state == TaskState::Merged
        })
        .await;
        let tree_of =
            |dir: &Path, commit: &str| git_out(dir, &["rev-parse", &format!("{commit}^{{tree}}")]);
        assert_eq!(tree_of(&fix.studio_clone, "main"), tree_of(&fix.tree, &resolved));
        assert_eq!(git_out(&fix.studio_clone, &["rev-parse", "main~1"]), person, "on top");
        assert_eq!(
            std::fs::read_to_string(fix.studio_clone.join("a.txt")).unwrap(),
            "main's own\nthe task's\n"
        );
        assert_eq!(git_out(&fix.forge, &["rev-parse", "main"]), fix.forge_main, "never pushed");
        let steps = moments(&hub, project).await;
        assert_eq!(
            steps,
            [
                "Clone began",
                "Clone done",
                "Send began",
                "Send done",
                "Home began",
                "Home done",
                "Verify began",
                "verified true",
                "Home began",
                "Home done",
                "Merge began",
                "Send began",
                "Rebase failed",
                "Home began",
                "Home done",
                "Verify began",
                "verified true",
                "Merge began",
                "Merge done"
            ],
            "{steps:?}"
        );
        fix.server.shutdown().await;
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
        // The worktree the worker makes for the task, and the branch in it.
        let tree = repo.join(".claude/worktrees/slopty-demo-1");
        let branch = "worktree-slopty-demo-1";
        let (record, gate, later) = (root.join("record.json"), root.join("go"), root.join("later"));
        let calls = json!([{ "name": "task_report", "arguments": { "note": "Wrote it.", "branch": branch } }]);
        let prompt = json!([{ "hook_event_name": "UserPromptSubmit", "prompt": "go on" }]);
        let env = [
            ("STUB_RECORD".to_owned(), record.to_string_lossy().into_owned()),
            ("STUB_MCP_CALLS".to_owned(), calls.to_string()),
            ("STUB_MCP_AFTER".to_owned(), gate.to_string_lossy().into_owned()),
            ("STUB_LATER".to_owned(), prompt.to_string()),
            ("STUB_LATER_AFTER".to_owned(), later.to_string_lossy().into_owned()),
        ];
        let (server, _daemons, worker) = fleet_with(&root, "", &env).await;
        let hub = server.hub().clone();

        let shell = Verb::OpenTerminal {
            worker,
            cwd: Some(repo.to_string_lossy().into_owned()),
            command: Vec::new(),
            env: Vec::new(),
            name: None,
            size: None,
            session: None,
            worktree: None,
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
                goal: None,
                autonomy: slopty_proto::project::Autonomy::Ask,
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
        let launch = TaskLaunch { pin: Some(worker), agent: AgentId::named(AgentId::CLAUDE_CODE) };
        let spawned = hub
            .dispatch(Verb::TaskSpawn { project: project.clone(), task: TaskId(1), launch })
            .await;
        let Outcome::Task(spawned) = spawned else { panic!("{spawned:?}") };
        let agent = spawned.assignment.unwrap().term;
        // Its agent's work, unfinished, in the worktree made for it; then it reports.
        assert_eq!(git_out(&tree, &["branch", "--show-current"]), branch, "made for it");
        std::fs::write(tree.join("a.txt"), "the task's\n").unwrap();
        std::fs::write(tree.join("work.txt"), "not yet\n").unwrap();
        git(&tree, &["add", "."]);
        git(&tree, &["commit", "-q", "-m", "the work, unfinished"]);
        std::fs::write(&gate, "").unwrap();

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
            Outcome::Terminals { terminals: list, .. } => list,
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
                s.kind == StepKind::Rebase && matches!(s.state, StepState::Failed { .. })
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
            Outcome::Terminals { terminals: list, .. } => {
                list.iter().all(|(_, s)| s.id != kept.session)
            }
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
                "Rebase failed"
            ]
        );
        server.shutdown().await;
    }

    /// A goal's place is checked on its machine before anything starts for it: a folder that is
    /// no repository, or a branch it lacks, is refused in plain words through the server, and a
    /// clone is fine, as it is with the branch named.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_goal_s_place_is_checked_on_its_machine_before_anything_starts() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let clone = root.join("demo");
        std::fs::create_dir_all(&clone).unwrap();
        git_out(&clone, &["init", "-q", "-b", "main"]);
        git_out(&clone, &["commit", "-q", "--allow-empty", "-m", "first"]);
        let plain = root.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let (server, _daemons, worker) = fleet(&root, "").await;
        let hub = server.hub().clone();
        let check = |repo: &Path, target: &str| Verb::GoalCheck {
            worker,
            repo: repo.to_string_lossy().into_owned(),
            target: target.to_owned(),
        };

        let said = hub.dispatch(check(&plain, "")).await;
        let Outcome::Error { code: ErrorCode::Invalid, message } = said else { panic!("{said:?}") };
        assert!(message.contains("is not in a git repository"), "{message}");
        assert!(!message.contains("fatal"), "not git's words: {message}");
        let said = hub.dispatch(check(&clone, "release")).await;
        let Outcome::Error { code: ErrorCode::Invalid, message } = said else { panic!("{said:?}") };
        assert!(message.contains("has no branch release"), "{message}");
        assert_eq!(hub.dispatch(check(&clone, "")).await, Outcome::Done);
        assert_eq!(hub.dispatch(check(&clone, "main")).await, Outcome::Done);
        server.shutdown().await;
    }

    /// What the walk of a person's day found wrong, each said as it is found; the walk fails
    /// with all of them at its end, so one run names every one.
    #[derive(Default)]
    struct Findings(Vec<String>);

    impl Findings {
        /// `holds`, or `what` is found.
        fn check(&mut self, holds: bool, what: &str) {
            if !holds {
                eprintln!("FINDING {what}");
                self.0.push(what.to_owned());
            }
        }

        /// Everything found, for a walk that cannot go on.
        fn so_far(&self) -> String {
            format!("found so far: {:#?}", self.0)
        }
    }

    /// [`until`] for at most `within`, without failing: what it found, if it did.
    async fn soon<T>(within: Duration, mut done: impl AsyncFnMut() -> Option<T>) -> Option<T> {
        let waited = tokio::time::timeout(within, async {
            loop {
                if let Some(found) = done().await {
                    return found;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });
        waited.await.ok()
    }

    /// The card of task `task` once `done` holds of it, within `within`.
    async fn card_of(
        hub: &Hub,
        project: &ProjectId,
        task: u32,
        within: Duration,
        done: impl Fn(&slopty_proto::project::TaskCard) -> bool,
    ) -> Option<slopty_proto::project::TaskCard> {
        soon(within, async || {
            let card =
                status(hub, project).await.tasks.into_iter().find(|c| c.id == TaskId(task))?;
            done(&card).then_some(card)
        })
        .await
    }

    /// A stand-in `gh` in `programs`: no pull request is open, a new one is number 1, and
    /// reading it says it is open; each call is kept in `log`. No one's GitHub is reached.
    fn stand_in_gh(programs: &Path, log: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        let view = r#"{"number":1,"url":"https://example.com/o/demo/pull/1","title":"Ship",
"state":"OPEN","isDraft":false,"headRefName":"slopty/demo/goal","headRefOid":"0123",
"baseRefName":"main","reviewDecision":"","mergeable":"MERGEABLE",
"mergeStateStatus":"CLEAN","statusCheckRollup":[]}"#;
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> \"{log}\"\ncase \"$1 $2\" in\n\
             'pr list') echo '[]' ;;\n\
             'pr create') echo 'https://example.com/o/demo/pull/1' ;;\n\
             'pr view') echo '{view}' ;;\n\
             *) echo \"unexpected: $*\" >&2; exit 2 ;;\nesac\n",
            log = log.display(),
            view = view.replace('\n', ""),
        );
        let gh = programs.join("gh");
        std::fs::write(&gh, script).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A person's day, walked on the server, two workers and the CLI with stand-in agents, to
    /// find what breaks, stalls, needs a nudge or shows the wrong state. A Linux box is added
    /// beside the studio; a goal is handed to an orchestrator in the studio's clone, which
    /// starts a task on each machine; the Linux one asks the person a question, and its worker
    /// restarts under it; both finish, come home, are verified, reviewed and merged into the
    /// goal's branch; the goal lands as a pull request; and another device reads it all.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_person_s_day_across_two_workers() {
        use std::time::Instant;

        use slopty_proto::orchestration::{ThreadOf, ThreadView};
        use slopty_proto::project::Task;

        const GOAL: &str = "Ship the Linux part and the Mac part.";
        let soonish = Duration::from_secs(20);
        server_log();
        let mut found = Findings::default();
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let seed = root.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        git_out(&seed, &["init", "-q", "-b", "main"]);
        std::fs::write(seed.join("a.txt"), "one\n").unwrap();
        git_out(&seed, &["add", "."]);
        git_out(&seed, &["commit", "-q", "-m", "first"]);
        git_out(&root, &["clone", "-q", "--bare", "seed", "forge.git"]);
        let url = "https://example.com/o/demo.git";
        let forge = root.join("forge.git");
        let reach = format!("url.file://{}.insteadOf={url}", forge.display());
        git_out(&root, &["-c", &reach, "clone", "-q", url, "demo"]);
        let studio_clone = root.join("demo");
        let person_main = git_out(&studio_clone, &["rev-parse", "main"]);
        let config = format!("[url \"file://{}\"]\n\tinsteadOf = {url}\n", forge.display());
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::write(root.join("home/.gitconfig"), &config).unwrap();
        std::fs::write(root.join("home/.claude.json"), "{}").unwrap();
        let programs = root.join("programs");
        std::fs::create_dir_all(&programs).unwrap();
        let gh_log = root.join("gh.log");
        stand_in_gh(&programs, &gh_log);

        // Each machine's task agent reports done on its branch once its gate is there.
        let reporting = |task: u32, gate: &Path, record: &Path| {
            let branch = format!("worktree-slopty-demo-{task}");
            let calls = json!([{ "name": "task_report", "arguments": { "note": "Wrote it.", "branch": branch } }]);
            vec![
                ("STUB_MCP_CALLS".to_owned(), calls.to_string()),
                ("STUB_MCP_AFTER".to_owned(), gate.to_string_lossy().into_owned()),
                ("STUB_RECORD".to_owned(), record.to_string_lossy().into_owned()),
            ]
        };
        let (go_studio, go_linux) = (root.join("go-studio"), root.join("go-linux"));
        let studio_env = reporting(2, &go_studio, &root.join("studio-task.json"));
        let started = Instant::now();
        let (server, _studio, studio) = fleet_with(&root, "", &studio_env).await;
        eprintln!("MEASURE the studio online with Claude Code: {:?}", started.elapsed());
        let hub = server.hub().clone();
        let addr = server.quic_addr();

        // A Linux box is added. Its task's agent asks the person a question as it starts.
        let linux_dir = root.join("linux");
        let linux_home = linux_dir.join("home");
        std::fs::create_dir_all(&linux_home).unwrap();
        std::fs::write(linux_home.join(".gitconfig"), &config).unwrap();
        std::fs::write(linux_home.join(".claude.json"), "{}").unwrap();
        let question = json!({
            "questions": [{
                "question": "Which file holds the Linux part?",
                "header": "File",
                "options": [{ "label": "linux.txt" }, { "label": "work.txt" }],
                "multiSelect": false,
            }],
        });
        let hooks = json!([
            hook("SessionStart", &json!({ "source": "startup" })),
            hook(
                "PermissionRequest",
                &json!({ "tool_name": "AskUserQuestion", "tool_input": question }),
            ),
            hook("PostToolUse", &json!({ "tool_name": "AskUserQuestion", "tool_input": question })),
            hook("Stop", &json!({ "stop_hook_active": false })),
        ]);
        let mut linux_env = reporting(1, &go_linux, &root.join("linux-task.json"));
        linux_env.push(("STUB_HOOKS".to_owned(), hooks.to_string()));
        let added = Instant::now();
        let mut linux = daemons(&linux_dir, addr, &programs, "", "linux-box", &linux_env).await;
        let linux_worker = registered_with_claude(&hub, "linux-box").await;
        eprintln!("MEASURE a worker added, to online with Claude Code: {:?}", added.elapsed());
        let listed = slopty(&root, addr, &["workers"]).await;
        found.check(listed.contains("linux-box"), "`slopty workers` does not list the box added");

        // The goal, handed to an orchestrator started in the studio's clone, as the app does.
        let (orch_record, orch_inbox) = (root.join("orchestrator.json"), root.join("inbox.jsonl"));
        let go_orch = root.join("go-orchestrator");
        let starts = json!([
            { "name": "task_start", "arguments": {
                "title": "The Linux part", "brief": "Write linux.txt.", "worker": "linux-box" } },
            { "name": "task_start", "arguments": {
                "title": "The Mac part", "brief": "Write mac.txt.", "worker": "projects-test" } },
        ]);
        let spawn = Verb::SpawnAgent {
            worker: studio,
            cwd: studio_clone.to_string_lossy().into_owned(),
            prompt: None,
            args: Vec::new(),
            env: vec![
                ("STUB_MCP_CALLS".to_owned(), starts.to_string()),
                ("STUB_MCP_AFTER".to_owned(), go_orch.to_string_lossy().into_owned()),
                ("STUB_RECORD".to_owned(), orch_record.to_string_lossy().into_owned()),
                ("STUB_INBOX".to_owned(), orch_inbox.to_string_lossy().into_owned()),
            ],
            size: None,
            session: None,
            autonomy: None,
            worktree: None,
        };
        let opened = hub.dispatch(spawn).await;
        let Outcome::Opened(orchestrator) = opened else { panic!("the orchestrator: {opened:?}") };
        let project = ProjectId::new("demo").unwrap();
        let made = hub
            .dispatch(Verb::ProjectCreate {
                project: project.clone(),
                title: "Ship both parts".to_owned(),
                goal: Some(GOAL.to_owned()),
                autonomy: slopty_proto::project::Autonomy::Ask,
                repo: studio_clone.to_string_lossy().into_owned(),
                target: String::new(),
                verifier: Some("test -f linux.txt || test -f mac.txt".to_owned()),
                push: false,
                orchestrator: Some(orchestrator),
                limits: LimitsChange::default(),
                metadata: None,
            })
            .await;
        assert!(matches!(made, Outcome::Project(_)), "{made:?}");
        let goal_branch = Task::goal_branch(&project);
        let target = status(&hub, &project).await.project.target;
        found.check(target == goal_branch, &format!("the goal lands on {target:?}"));
        let branched = soon(soonish, async || {
            let made = std::process::Command::new("git")
                .current_dir(&studio_clone)
                .args(["rev-parse", "--verify", "--quiet", &goal_branch])
                .output()
                .ok()?;
            made.status.success().then_some(())
        })
        .await;
        found.check(branched.is_some(), "the goal's branch is not made in the clone");
        let heard = soon(soonish, async || {
            let inbox = std::fs::read_to_string(&orch_inbox).unwrap_or_default();
            let record = std::fs::read_to_string(&orch_record).unwrap_or_default();
            (inbox.contains(GOAL) || record.contains(GOAL)).then_some(())
        })
        .await;
        found.check(heard.is_some(), "the goal never reaches the orchestrator");

        // The orchestrator starts a task on each machine.
        let known = status(&hub, &project).await.project.repo_id.is_some();
        eprintln!("the project knows its repository as its orchestrator starts tasks: {known}");
        std::fs::write(&go_orch, "").unwrap();
        let asked = Instant::now();
        let both = soon(STEP, async || {
            let tasks = status(&hub, &project).await.tasks;
            let on = |task: u32| {
                tasks.iter().find(|c| c.id == TaskId(task))?.assignment.as_ref().map(|a| a.term)
            };
            Some((on(1)?, on(2)?))
        })
        .await;
        let Some((linux_agent, studio_agent)) = both else {
            let now = status(&hub, &project).await;
            let steps: Vec<_> = now.tasks.iter().map(|c| (c.id, c.state, &c.step)).collect();
            let timeline: Vec<_> = now.timeline.iter().map(|e| &e.what).collect();
            panic!("the tasks never started: {steps:#?}\n{timeline:#?}\n{}", found.so_far());
        };
        eprintln!("MEASURE the orchestrator's two tasks started: {:?}", asked.elapsed());
        let learned = soon(soonish, async || status(&hub, &project).await.project.repo_id).await;
        found.check(learned.is_some(), "the project never learns its repository");
        found.check(linux_agent.worker == linux_worker, "the Linux part runs off the Linux box");
        found.check(studio_agent.worker == studio, "the Mac part runs off the studio");

        // The Linux task's question waits on the person, and the board can answer it.
        let blocked = card_of(&hub, &project, 1, soonish, |c| c.state == TaskState::Blocked).await;
        found.check(blocked.is_some(), "a task's question does not show it waits on the person");
        let read = Verb::ReadThread {
            of: ThreadOf::Task { project: project.clone(), task: TaskId(1) },
            view: ThreadView::Messages,
            after: None,
            hold: true,
        };
        let request = match hub.dispatch(read).await {
            Outcome::Thread(read) => read.requests.into_iter().next(),
            other => {
                found.check(false, &format!("the question's thread is not read: {other:?}"));
                None
            }
        };
        found.check(request.is_some(), "the question is not open to answer from the board");
        if let Some(request) = request {
            eprintln!("the question as the board reads it: {request:?}");
            let given: Vec<Value> = request
                .questions
                .iter()
                .map(|q| json!({ "question": q, "answer": "linux.txt" }))
                .collect();
            let pick = Value::Array(given).to_string();
            let answer = Verb::AnswerRequest {
                of: ThreadOf::Task { project: project.clone(), task: TaskId(1) },
                ask: request.ask,
                choice: pick,
                message: None,
            };
            let answered = hub.dispatch(answer).await;
            let took = matches!(answered, Outcome::Done);
            found.check(took, &format!("the board's answer is refused: {answered:?}"));
            let heard = soon(soonish, async || {
                let seen: Value =
                    serde_json::from_slice(&std::fs::read(root.join("linux-task.json")).ok()?)
                        .ok()?;
                let asked =
                    seen["hooks"].as_array()?.iter().find(|h| h["event"] == "PermissionRequest")?;
                asked["outputs"].to_string().contains("allow").then_some(())
            })
            .await;
            found.check(heard.is_some(), "the board's answer never reaches the agent");
            let going =
                card_of(&hub, &project, 1, soonish, |c| c.state != TaskState::Blocked).await;
            found.check(going.is_some(), "answered, the task still waits on the person");
        }

        // The Linux worker restarts mid-task, right after the answer: its brief is being
        // typed, or waits to be. The agent lives on in ptyd, and the brief reaches it once,
        // whole, whichever side of the restart it was typed on.
        let worker_pid = linux[1].id().unwrap();
        slopty_testkit::group::kill(worker_pid);
        let _ended = linux[1].wait().await;
        let away = soon(soonish, async || {
            let listed = hub.directory().into_iter().find(|w| w.worker == linux_worker)?;
            (listed.liveness != Liveness::Online).then_some(())
        })
        .await;
        found.check(away.is_some(), "the box's worker gone still reads online");
        let card = status(&hub, &project).await.tasks.into_iter().find(|c| c.id == TaskId(1));
        let kept = card.as_ref().is_some_and(|c| c.state != TaskState::Failed);
        found.check(kept, "its worker away, the task is failed");
        let back = Instant::now();
        let path = slopty_testkit::env::path_with(&programs);
        linux[1] = worker_daemon(&linux_dir, addr, &path, "linux-box").await;
        let again = registered_with_claude(&hub, "linux-box").await;
        eprintln!("MEASURE the box's worker back online: {:?}", back.elapsed());
        found.check(again == linux_worker, "the box restarted is another worker");
        let terms = soon(soonish, async || {
            match hub.dispatch(Verb::ListTerminals { worker: Some(linux_worker) }).await {
                Outcome::Terminals { terminals, .. } => {
                    terminals.iter().any(|(_, s)| s.id == linux_agent.session).then_some(())
                }
                _ => None,
            }
        })
        .await;
        found.check(terms.is_some(), "the task's agent is gone after its worker restarted");
        let card = status(&hub, &project).await.tasks.into_iter().find(|c| c.id == TaskId(1));
        let same = card.as_ref().and_then(|c| c.assignment.as_ref()).map(|a| a.term);
        found.check(same == Some(linux_agent), "the restart lost the task's agent");

        // Both agents commit their part, where each was started, and report.
        let records = [root.join("linux-task.json"), root.join("studio-task.json")];
        for (record, file) in records.iter().zip(["linux.txt", "mac.txt"]) {
            let cwd = soon(soonish, async || {
                let seen: Value = serde_json::from_slice(&std::fs::read(record).ok()?).ok()?;
                let argv: Vec<&str> =
                    seen["argv"].as_array()?.iter().filter_map(Value::as_str).collect();
                let named = argv.iter().enumerate().find_map(|(at, arg)| {
                    arg.strip_prefix("--worktree=")
                        .or_else(|| (*arg == "--worktree").then(|| argv.get(at + 1).copied())?)
                })?;
                let cwd = PathBuf::from(seen["cwd"].as_str()?);
                Some(cwd.join(".claude/worktrees").join(named))
            })
            .await;
            let Some(tree) = cwd else {
                let seen = std::fs::read_to_string(record).unwrap_or_default();
                let cards = status(&hub, &project).await.tasks;
                panic!("no worktree in {record:?}: {seen}\n{cards:#?}\n{}", found.so_far());
            };
            let branch = git_out(&tree, &["branch", "--show-current"]);
            eprintln!("an agent runs in {} on {branch:?}", tree.display());
            std::fs::write(tree.join(file), "done\n").unwrap();
            git_out(&tree, &["add", "."]);
            git_out(&tree, &["commit", "-q", "-m", file]);
        }
        let reported = Instant::now();
        std::fs::write(&go_linux, "").unwrap();
        std::fs::write(&go_studio, "").unwrap();
        let ready = |c: &slopty_proto::project::TaskCard| {
            c.state == TaskState::Done && c.verified.as_ref().is_some_and(|r| r.passed)
        };
        for task in [1, 2] {
            let card = card_of(&hub, &project, task, STEP, ready).await;
            found.check(card.is_some(), &format!("task {task} is never ready to merge"));
        }
        eprintln!("MEASURE both reported to both ready: {:?}", reported.elapsed());
        // Its brief went in once the question was answered and its turn ended, once, though
        // its worker restarted as it was typed.
        let briefed = soon(soonish, async || {
            let seen: Value =
                serde_json::from_slice(&std::fs::read(root.join("linux-task.json")).ok()?).ok()?;
            let typed = seen["typed"].as_array()?;
            let times = typed.iter().filter(|t| *t == "Write linux.txt.").count();
            (times > 0).then_some(times)
        })
        .await;
        found.check(
            briefed.is_some(),
            "its brief never reaches the agent that asked a question first",
        );
        found.check(briefed.is_none_or(|times| times == 1), "its brief is typed twice");

        // The person reviews each task's change in the orchestrator's clone: the Linux one
        // brought home as `slopty/demo/1`, the studio's on its own branch there.
        for (task, file, branch) in
            [(1, "linux.txt", "slopty/demo/1"), (2, "mac.txt", "worktree-slopty-demo-2")]
        {
            let range = format!("{goal_branch}...{branch}");
            let changed = std::process::Command::new("git")
                .current_dir(&studio_clone)
                .args(["diff", "--name-only", &range])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .unwrap_or_default();
            found.check(changed == file, &format!("task {task}'s change reads {changed:?}"));
        }

        // Merged one after the other into the goal's branch, never the person's main.
        for task in [1, 2] {
            let asked =
                hub.dispatch(Verb::TaskMerge { project: project.clone(), task: TaskId(task) });
            let took = matches!(asked.await, Outcome::Task(_));
            found.check(took, &format!("the merge of task {task} is refused"));
            let merged =
                card_of(&hub, &project, task, STEP, |c| c.state == TaskState::Merged).await;
            found.check(merged.is_some(), &format!("task {task} never merges"));
        }
        for file in ["linux.txt", "mac.txt"] {
            let shown = std::process::Command::new("git")
                .current_dir(&studio_clone)
                .args(["cat-file", "-e", &format!("{goal_branch}:{file}")])
                .status()
                .is_ok_and(|s| s.success());
            found.check(shown, &format!("{file} is not on the goal's branch"));
        }
        let main_now = git_out(&studio_clone, &["rev-parse", "main"]);
        found.check(main_now == person_main, "the person's main moved");

        // The goal lands as a pull request on the forge.
        let landed = hub.dispatch(Verb::ProjectLand { project: project.clone(), into: None }).await;
        let opened = matches!(&landed, Outcome::PullOpened { number: 1, .. });
        found.check(opened, &format!("the goal does not land: {landed:?}"));
        let pushed = std::process::Command::new("git")
            .current_dir(&forge)
            .args(["rev-parse", &goal_branch])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default();
        let ours = git_out(&studio_clone, &["rev-parse", &goal_branch]);
        found.check(pushed == ours, "the goal's branch is not on the forge as merged");
        let gh = std::fs::read_to_string(&gh_log).unwrap_or_default();
        let asked_into = gh.contains(&format!("pr create --head {goal_branch} --base main"));
        found.check(asked_into, &format!("the pull request is not into main: {gh}"));
        let shown = soon(soonish, async || status(&hub, &project).await.project.landed).await;
        found.check(shown.is_some(), "the board does not show the pull request");

        // Another device: a CLI with nothing of its own reads the same day.
        let other = root.join("other-device");
        let read = scrubbed(bin("slopty"), &other)
            .arg("--server")
            .arg(addr.to_string())
            .arg("--data-dir")
            .arg(other.join("data"))
            .args(["--json", "project", "status", "demo"])
            .env("RUST_LOG", "warn")
            .kill_on_drop(true)
            .output();
        let read = tokio::time::timeout(STEP, read).await.expect("slopty answers").unwrap();
        let said = String::from_utf8_lossy(&read.stdout);
        found.check(read.status.success(), "another device cannot read the project");
        let all = said.matches("\"merged\"").count() + said.matches("\"Merged\"").count();
        found.check(all >= 2, "another device does not see both tasks merged");
        found.check(said.contains("pull/1"), "another device does not see the pull request");

        server.shutdown().await;
        assert!(found.0.is_empty(), "{}", found.so_far());
    }

    /// `kill -<signal> <pid>`, as the system sends it.
    fn signal(pid: u32, signal: &str) {
        let sent = std::process::Command::new("kill")
            .args([&format!("-{signal}"), &pid.to_string()])
            .status()
            .is_ok_and(|s| s.success());
        assert!(sent, "kill -{signal} {pid}");
    }

    /// The terminals the server lists on `worker`.
    async fn terminals_on(hub: &Hub, worker: WorkerId) -> Vec<slopty_core::SessionId> {
        match hub.dispatch(Verb::ListTerminals { worker: Some(worker) }).await {
            Outcome::Terminals { terminals, .. } => terminals.iter().map(|(_, s)| s.id).collect(),
            _ => Vec::new(),
        }
    }

    /// The answers a hook's outputs hand back, wherever in them they are.
    fn answers_in(outputs: &Value) -> Option<&serde_json::Map<String, Value>> {
        match outputs {
            Value::Object(fields) => fields
                .get("answers")
                .and_then(Value::as_object)
                .or_else(|| fields.values().find_map(answers_in)),
            Value::Array(items) => items.iter().find_map(answers_in),
            _ => None,
        }
    }

    /// Task `task`'s agent, as the stub's record names its worktree, commits its work there and
    /// reports: `edit` written to `a.txt` (or, empty, `notes.txt` written instead), `done.txt`
    /// added when `done`, and then the gate its report waits on opened.
    async fn commit_task(root: &Path, task: u32, edit: &str, done: bool, found: &Findings) {
        let record = root.join(format!("task-{task}.json"));
        let tree = soon(Duration::from_secs(20), async || {
            let seen: Value = serde_json::from_slice(&std::fs::read(&record).ok()?).ok()?;
            let argv: Vec<&str> =
                seen["argv"].as_array()?.iter().filter_map(Value::as_str).collect();
            let named = argv.iter().enumerate().find_map(|(at, arg)| {
                arg.strip_prefix("--worktree=").or_else(|| {
                    (*arg == "--worktree").then(|| argv.get(at.checked_add(1)?).copied())?
                })
            })?;
            let cwd = PathBuf::from(seen["cwd"].as_str()?);
            Some(cwd.join(".claude/worktrees").join(named))
        })
        .await;
        let Some(tree) = tree else {
            panic!("task {task} has no worktree: {}", found.so_far());
        };
        if edit.is_empty() {
            std::fs::write(tree.join("notes.txt"), "notes\n").unwrap();
        } else {
            std::fs::write(tree.join("a.txt"), edit).unwrap();
        }
        if done {
            std::fs::write(tree.join("done.txt"), "done\n").unwrap();
        }
        git_out(&tree, &["add", "."]);
        git_out(&tree, &["commit", "-q", "-m", &format!("task {task}")]);
        std::fs::write(root.join(format!("go-{task}")), "").unwrap();
    }

    /// A person's hard day, walked as [`a_person_s_day_across_two_workers`] walks the good one,
    /// to find what a person meets when things go wrong: two devices answer a task's question at
    /// once; the server restarts mid-goal; the Linux box loses the network for 30 s, its agent
    /// reporting done meanwhile; a task fails its verifier; two tasks' merges conflict on the
    /// goal's branch; the orchestrator's agent exits mid-goal; and the person cancels the goal.
    #[tokio::test(flavor = "multi_thread")]
    #[expect(
        clippy::literal_string_with_formatting_args,
        reason = "`{task}` is the stub's own placeholder, filled in by each task's agent"
    )]
    async fn a_person_s_hard_day_across_two_workers() {
        use std::time::Instant;

        use slopty_proto::orchestration::{ThreadOf, ThreadView};
        use slopty_proto::project::Task;

        const GOAL: &str = "Ship the Linux part, the Mac part and the notes.";
        let soonish = Duration::from_secs(20);
        server_log();
        let mut found = Findings::default();
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let seed = root.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        git_out(&seed, &["init", "-q", "-b", "main"]);
        std::fs::write(seed.join("a.txt"), "one\n").unwrap();
        git_out(&seed, &["add", "."]);
        git_out(&seed, &["commit", "-q", "-m", "first"]);
        git_out(&root, &["clone", "-q", "--bare", "seed", "forge.git"]);
        let url = "https://example.com/o/demo.git";
        let forge = root.join("forge.git");
        let reach = format!("url.file://{}.insteadOf={url}", forge.display());
        git_out(&root, &["-c", &reach, "clone", "-q", url, "demo"]);
        let studio_clone = root.join("demo");
        let person_main = git_out(&studio_clone, &["rev-parse", "main"]);
        let config = format!("[url \"file://{}\"]\n\tinsteadOf = {url}\n", forge.display());
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::write(root.join("home/.gitconfig"), &config).unwrap();
        std::fs::write(root.join("home/.claude.json"), "{}").unwrap();
        let programs = root.join("programs");
        std::fs::create_dir_all(&programs).unwrap();
        stand_in_gh(&programs, &root.join("gh.log"));

        // Every task's agent, on either machine, keeps its own record and inbox and reports
        // done on its own branch once its own gate is there.
        let at = |name: &str| root.join(name).to_string_lossy().into_owned();
        let calls = json!([{ "name": "task_report", "arguments": {
            "note": "Wrote it.", "branch": "worktree-slopty-demo-{task}" } }]);
        let tasks_env = vec![
            ("STUB_MCP_CALLS".to_owned(), calls.to_string()),
            ("STUB_MCP_AFTER".to_owned(), at("go-{task}")),
            ("STUB_RECORD".to_owned(), at("task-{task}.json")),
            ("STUB_INBOX".to_owned(), at("inbox-{task}.jsonl")),
        ];
        let (server, _studio, studio) = fleet_with(&root, "", &tasks_env).await;
        let hub = server.hub().clone();
        let addr = server.quic_addr();

        // The Linux box, whose task's agent asks the person a question as it starts.
        let linux_dir = root.join("linux");
        let linux_home = linux_dir.join("home");
        std::fs::create_dir_all(&linux_home).unwrap();
        std::fs::write(linux_home.join(".gitconfig"), &config).unwrap();
        std::fs::write(linux_home.join(".claude.json"), "{}").unwrap();
        let question = json!({
            "questions": [{
                "question": "Which file holds the Linux part?",
                "header": "File",
                "options": [{ "label": "linux.txt" }, { "label": "work.txt" }],
                "multiSelect": false,
            }],
        });
        let hooks = json!([
            hook("SessionStart", &json!({ "source": "startup" })),
            hook(
                "PermissionRequest",
                &json!({ "tool_name": "AskUserQuestion", "tool_input": question }),
            ),
            hook("PostToolUse", &json!({ "tool_name": "AskUserQuestion", "tool_input": question })),
            hook("Stop", &json!({ "stop_hook_active": false })),
        ]);
        let mut linux_env = tasks_env.clone();
        linux_env.push(("STUB_HOOKS".to_owned(), hooks.to_string()));
        // The box reaches the server through a link the walk can cut, as its network would.
        let link = slopty_testkit::relay::Relay::start(addr).await.unwrap();
        let mut linux =
            daemons(&linux_dir, link.addr(), &programs, "", "linux-box", &linux_env).await;
        let linux_worker = registered_with_claude(&hub, "linux-box").await;

        // The goal, handed to an orchestrator in the studio's clone, which starts three tasks.
        let (orch_record, orch_inbox) = (root.join("orchestrator.json"), root.join("inbox.jsonl"));
        let go_orch = root.join("go-orchestrator");
        let starts = json!([
            { "name": "task_start", "arguments": {
                "title": "The Linux part", "brief": "Write linux.txt.", "worker": "linux-box" } },
            { "name": "task_start", "arguments": {
                "title": "The Mac part", "brief": "Write mac.txt.", "worker": "projects-test" } },
            { "name": "task_start", "arguments": {
                "title": "The notes", "brief": "Write notes.txt.", "worker": "projects-test" } },
        ]);
        let spawn = Verb::SpawnAgent {
            worker: studio,
            cwd: studio_clone.to_string_lossy().into_owned(),
            prompt: None,
            args: Vec::new(),
            env: vec![
                ("STUB_MCP_CALLS".to_owned(), starts.to_string()),
                ("STUB_MCP_AFTER".to_owned(), go_orch.to_string_lossy().into_owned()),
                ("STUB_RECORD".to_owned(), orch_record.to_string_lossy().into_owned()),
                ("STUB_INBOX".to_owned(), orch_inbox.to_string_lossy().into_owned()),
                ("STUB_TRANSCRIPT".to_owned(), at("orchestrator.jsonl")),
            ],
            size: None,
            session: None,
            autonomy: None,
            worktree: None,
        };
        let opened = hub.dispatch(spawn).await;
        let Outcome::Opened(orchestrator) = opened else { panic!("the orchestrator: {opened:?}") };
        let project = ProjectId::new("demo").unwrap();
        let made = hub
            .dispatch(Verb::ProjectCreate {
                project: project.clone(),
                title: "Ship three parts".to_owned(),
                goal: Some(GOAL.to_owned()),
                autonomy: slopty_proto::project::Autonomy::Ask,
                repo: studio_clone.to_string_lossy().into_owned(),
                target: String::new(),
                verifier: Some("test -f done.txt".to_owned()),
                push: false,
                orchestrator: Some(orchestrator),
                limits: LimitsChange::default(),
                metadata: None,
            })
            .await;
        assert!(matches!(made, Outcome::Project(_)), "{made:?}");
        let goal_branch = Task::goal_branch(&project);
        until("the project learns its repository", async || {
            status(&hub, &project).await.project.repo_id.filter(|id| id.root.is_some())
        })
        .await;
        std::fs::write(&go_orch, "").unwrap();
        let three = soon(STEP, async || {
            let tasks = status(&hub, &project).await.tasks;
            let on = |task: u32| {
                tasks.iter().find(|c| c.id == TaskId(task))?.assignment.as_ref().map(|a| a.term)
            };
            Some([on(1)?, on(2)?, on(3)?])
        })
        .await;
        let Some(agents) = three else {
            let now = status(&hub, &project).await;
            panic!("the tasks never started: {:#?}\n{}", now.tasks, found.so_far());
        };

        // Two devices answer the Linux task's question at once, each with its own pick.
        let blocked = card_of(&hub, &project, 1, soonish, |c| c.state == TaskState::Blocked).await;
        found.check(blocked.is_some(), "a task's question does not show it waits on the person");
        let of = ThreadOf::Task { project: project.clone(), task: TaskId(1) };
        let read = Verb::ReadThread {
            of: of.clone(),
            view: ThreadView::Messages,
            after: None,
            hold: true,
        };
        let request = soon(soonish, async || match hub.dispatch(read.clone()).await {
            Outcome::Thread(read) => read.requests.into_iter().next(),
            _ => None,
        })
        .await;
        found.check(request.is_some(), "the question is not open to answer from the board");
        if let Some(request) = request {
            let answer = |pick: &str| {
                let given: Vec<Value> = request
                    .questions
                    .iter()
                    .map(|q| json!({ "question": q, "answer": pick }))
                    .collect();
                Verb::AnswerRequest {
                    of: of.clone(),
                    ask: request.ask.clone(),
                    choice: Value::Array(given).to_string(),
                    message: None,
                }
            };
            let (first, second) =
                tokio::join!(hub.dispatch(answer("linux.txt")), hub.dispatch(answer("work.txt")));
            eprintln!("two answers at once: {first:?} / {second:?}");
            let took = [&first, &second].iter().filter(|o| matches!(o, Outcome::Done)).count();
            found.check(took == 1, &format!("{took} of two answers at once are taken"));
            let told = [&first, &second].iter().any(
                |o| matches!(o, Outcome::Error { message, .. } if message.contains("answered already")),
            );
            found.check(told, "the second answer is not told the question was answered");
            let picks = soon(soonish, async || {
                let seen: Value =
                    serde_json::from_slice(&std::fs::read(root.join("task-1.json")).ok()?).ok()?;
                let asked =
                    seen["hooks"].as_array()?.iter().find(|h| h["event"] == "PermissionRequest")?;
                let answers = answers_in(&asked["outputs"])?;
                Some(answers.values().filter_map(Value::as_str).map(str::to_owned).collect())
            })
            .await
            .unwrap_or_else(Vec::<String>::new);
            eprintln!("the agent heard: {picks:?}");
            found.check(!picks.is_empty(), "no answer reaches the agent");
            found.check(picks.len() <= 1, "the agent hears both answers");
        }

        // The server restarts mid-goal, on the same address and the same state.
        let restarted = Instant::now();
        server.shutdown().await;
        let server = Server::start(Config {
            name: "projects-test".to_owned(),
            quic: addr,
            data_dir: root.join("server"),
            admission: slopty_net::admission::Admission::default(),
            push: slopty_server::PushConfig::Off,
        })
        .await
        .unwrap();
        let hub = server.hub().clone();
        let back = soon(STEP, async || {
            let listed = hub.directory();
            let online = |w: WorkerId| {
                listed.iter().any(|d| d.worker == w && d.liveness == Liveness::Online)
            };
            (online(studio) && online(linux_worker)).then_some(())
        })
        .await;
        found.check(back.is_some(), "the workers are not back once the server restarted");
        eprintln!(
            "MEASURE the server restarted, to both workers online: {:?}",
            restarted.elapsed()
        );
        let after = status(&hub, &project).await;
        let kept: Vec<_> = (1..=3)
            .map(|t| {
                after.tasks.iter().find(|c| c.id == TaskId(t)).and_then(|c| {
                    c.assignment.as_ref().map(|a| a.term).filter(|_| c.state != TaskState::Failed)
                })
            })
            .collect();
        found.check(
            kept == agents.map(Some),
            &format!("the restart loses the tasks' agents: {kept:?}"),
        );
        found.check(
            after.project.orchestrator == Some(orchestrator),
            "the restart loses the orchestrator",
        );
        let running = soon(soonish, async || {
            let on = terminals_on(&hub, linux_worker).await;
            on.contains(&agents[0].session).then_some(())
        })
        .await;
        found.check(running.is_some(), "after the restart the server lists no Linux agent");

        // The studio's agents commit in their worktrees and report: the Mac part changes `a.txt`
        // as the Linux part will, and the notes forget the file the verifier wants.
        commit_task(&root, 2, "mac\n", true, &found).await;
        commit_task(&root, 3, "", false, &found).await;

        // The Linux box loses the network for 30 s: its link to the server goes silent while
        // the box, its worker and its agent run on. Its agent finishes meanwhile, and reports
        // done with no server to hear it.
        link.cut();
        let lost = Instant::now();
        let away = soon(Duration::from_secs(30), async || {
            let listed = hub.directory().into_iter().find(|w| w.worker == linux_worker)?;
            (listed.liveness != Liveness::Online).then(|| lost.elapsed())
        })
        .await;
        found.check(away.is_some(), "a box off the network for 30 s still reads online");
        if let Some(seen) = away {
            eprintln!("MEASURE a box off the network, to shown away: {seen:?}");
        }
        commit_task(&root, 1, "linux\n", true, &found).await;
        tokio::time::sleep(Duration::from_secs(30).saturating_sub(lost.elapsed())).await;
        let card = status(&hub, &project).await.tasks.into_iter().find(|c| c.id == TaskId(1));
        found.check(
            card.is_some_and(|c| c.state != TaskState::Failed),
            "its box off the network, the task is failed",
        );
        link.restore();
        eprintln!("datagrams lost while the box was off the network: {}", link.dropped());
        let resumed = Instant::now();
        let online = soon(STEP, async || {
            let listed = hub.directory().into_iter().find(|w| w.worker == linux_worker)?;
            (listed.liveness == Liveness::Online).then(|| resumed.elapsed())
        })
        .await;
        found.check(online.is_some(), "the box back on the network never reads online again");
        if let Some(seen) = online {
            eprintln!("MEASURE the network back, to the box online: {seen:?}");
        }
        let card = status(&hub, &project).await.tasks.into_iter().find(|c| c.id == TaskId(1));
        let same = card.and_then(|c| c.assignment).map(|a| a.term);
        found.check(same == Some(agents[0]), "the network loss lost the task's agent");
        let ready = |c: &slopty_proto::project::TaskCard| {
            c.state == TaskState::Done && c.verified.as_ref().is_some_and(|r| r.passed)
        };
        for task in [1, 2] {
            let card = card_of(&hub, &project, task, STEP, ready).await;
            let why = if task == 1 { ", having reported done off the network" } else { "" };
            if card.is_none() {
                let record = std::fs::read_to_string(root.join(format!("task-{task}.json")));
                let seen: Value =
                    serde_json::from_str(&record.unwrap_or_default()).unwrap_or_default();
                eprintln!("task {task}'s agent's tool calls: {}", seen["mcp"]);
            }
            found.check(card.is_some(), &format!("task {task} is never ready to merge{why}"));
        }

        // The notes fail their verifier: the card says so, the agent is told why, and the
        // task cannot be merged.
        let failed =
            card_of(&hub, &project, 3, STEP, |c| c.verified.as_ref().is_some_and(|r| !r.passed))
                .await;
        found.check(failed.is_some(), "a failed verifier does not show on its card");
        let told = soon(soonish, async || {
            let inbox = std::fs::read_to_string(root.join("inbox-3.jsonl")).ok()?;
            inbox.contains("done.txt").then_some(())
        })
        .await;
        found.check(told.is_some(), "the agent whose verifier failed is not told why");
        // The person merges it anyway: it is checked again first, fails again, and is not merged.
        let merge3 =
            hub.dispatch(Verb::TaskMerge { project: project.clone(), task: TaskId(3) }).await;
        eprintln!("the merge of a task that failed its verifier, asked: {merge3:?}");
        let checked_again = card_of(&hub, &project, 3, STEP, |c| {
            c.state != TaskState::Verifying && c.verified.as_ref().is_some_and(|r| !r.passed)
        })
        .await;
        found.check(
            checked_again.is_some_and(|c| c.state != TaskState::Merged),
            "a task that failed its verifier is merged when the person asks",
        );

        // The Linux part merges; the Mac part then conflicts on the goal's branch, goes back
        // to its agent with the path, and the goal's branch keeps the Linux part.
        let merge1 =
            hub.dispatch(Verb::TaskMerge { project: project.clone(), task: TaskId(1) }).await;
        found.check(matches!(merge1, Outcome::Task(_)), "the first merge is refused");
        let merged = card_of(&hub, &project, 1, STEP, |c| c.state == TaskState::Merged).await;
        found.check(merged.is_some(), "the first task never merges");
        let merge2 =
            hub.dispatch(Verb::TaskMerge { project: project.clone(), task: TaskId(2) }).await;
        eprintln!("the conflicting merge, asked: {merge2:?}");
        let given_back = card_of(&hub, &project, 2, STEP, |c| {
            c.step.as_ref().is_some_and(|s| {
                matches!(&s.state, slopty_proto::project::StepState::Failed { why } if why.contains("a.txt"))
            })
        })
        .await;
        found.check(given_back.is_some(), "a conflict on the goal's branch does not show its path");
        let conflict_told = soon(soonish, async || {
            let inbox = std::fs::read_to_string(root.join("inbox-2.jsonl")).ok()?;
            inbox.contains("a.txt").then_some(())
        })
        .await;
        found.check(conflict_told.is_some(), "the agent whose merge conflicts is not told");
        let on_goal = std::process::Command::new("git")
            .current_dir(&studio_clone)
            .args(["show", &format!("{goal_branch}:a.txt")])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        found.check(on_goal == "linux\n", &format!("the goal's branch holds {on_goal:?}"));
        found.check(
            git_out(&studio_clone, &["rev-parse", "main"]) == person_main,
            "the person's main moved",
        );

        // The orchestrator's agent exits mid-goal.
        let orch_pid = soon(soonish, async || {
            let seen: Value = serde_json::from_slice(&std::fs::read(&orch_record).ok()?).ok()?;
            u32::try_from(seen["pid"].as_u64()?).ok()
        })
        .await;
        if let Some(pid) = orch_pid {
            signal(pid, "TERM");
        }
        let ended = soon(soonish, async || {
            let now = status(&hub, &project).await;
            let gone = |e: &slopty_proto::project::TimelineEntry| {
                matches!(e.what, Moment::AgentGone { term } if term == orchestrator)
            };
            now.timeline.iter().any(gone).then_some(())
        })
        .await;
        found.check(ended.is_some(), "the board does not say its orchestrator ended");
        let still = terminals_on(&hub, studio).await;
        found.check(
            still.contains(&agents[1].session) && still.contains(&agents[2].session),
            "the orchestrator's end takes its tasks' agents with it",
        );
        let resumed = hub.dispatch(Verb::ProjectResume { project: project.clone() }).await;
        eprintln!("the person resumes the orchestrator: {resumed:?}");
        found.check(
            !matches!(resumed, Outcome::Error { .. }),
            &format!("the person cannot resume an orchestrator that ended: {resumed:?}"),
        );

        // The person cancels the goal: it goes, and so does the work it started.
        let cancelled = hub.dispatch(Verb::ProjectDelete { project: project.clone() }).await;
        found.check(matches!(cancelled, Outcome::Done), &format!("cancel: {cancelled:?}"));
        let listed = match hub.dispatch(Verb::ProjectList).await {
            Outcome::Projects(list) => list.iter().any(|p| p.id == project),
            _ => true,
        };
        found.check(!listed, "a cancelled goal is still listed");
        let stopped = soon(soonish, async || {
            let (studio_now, linux_now) =
                (terminals_on(&hub, studio).await, terminals_on(&hub, linux_worker).await);
            let left = agents
                .iter()
                .filter(|a| studio_now.contains(&a.session) || linux_now.contains(&a.session))
                .count();
            (left == 0).then_some(())
        })
        .await;
        found.check(stopped.is_some(), "a cancelled goal's task agents keep running");

        drop(linux.pop());
        drop(link);
        server.shutdown().await;
        assert!(found.0.is_empty(), "{}", found.so_far());
    }

    /// A reboot mid-goal, walked to find what a person meets after one: the machine running the
    /// orchestrator and a task's agent goes down (ptyd and the worker end, and every program in
    /// their terminals with them) while the task's agent works, and comes back on the same
    /// data. Each agent should come back in its own terminal, on its own conversation, and be
    /// told it was cut off, so the goal goes on without the person.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_reboot_mid_goal_picks_the_goal_up_again() {
        use std::time::Instant;

        const AFTER_BOOT: Duration = Duration::from_secs(30);
        server_log();
        let mut found = Findings::default();
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let clone = root.join("demo");
        std::fs::create_dir_all(&clone).unwrap();
        git_out(&clone, &["init", "-q", "-b", "main"]);
        std::fs::write(clone.join("a.txt"), "one\n").unwrap();
        git_out(&clone, &["add", "."]);
        git_out(&clone, &["commit", "-q", "-m", "first"]);
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::write(root.join("home/.claude.json"), "{}").unwrap();
        let at = |name: &str| root.join(name).to_string_lossy().into_owned();
        let calls = json!([{ "name": "task_report", "arguments": {
            "note": "Wrote it.", "branch": "worktree-slopty-demo-{task}" } }]);
        let tasks_env = vec![
            ("STUB_MCP_CALLS".to_owned(), calls.to_string()),
            ("STUB_MCP_AFTER".to_owned(), at("go-{task}")),
            ("STUB_RECORD".to_owned(), at("task-{task}.json")),
            ("STUB_INBOX".to_owned(), at("inbox-{task}.jsonl")),
            ("STUB_TRANSCRIPT".to_owned(), at("transcript-{task}.jsonl")),
        ];
        let (server, mut machine, studio) = fleet_with(&root, "", &tasks_env).await;
        let hub = server.hub().clone();

        let starts = json!([{ "name": "task_start", "arguments": {
            "title": "The notes", "brief": "Write notes.txt.", "worker": "projects-test" } }]);
        let go_orch = root.join("go-orchestrator");
        let spawn = Verb::SpawnAgent {
            worker: studio,
            cwd: clone.to_string_lossy().into_owned(),
            prompt: None,
            args: Vec::new(),
            env: vec![
                ("STUB_MCP_CALLS".to_owned(), starts.to_string()),
                ("STUB_MCP_AFTER".to_owned(), go_orch.to_string_lossy().into_owned()),
                ("STUB_RECORD".to_owned(), at("orchestrator.json")),
                ("STUB_INBOX".to_owned(), at("inbox-orchestrator.jsonl")),
                ("STUB_TRANSCRIPT".to_owned(), at("transcript-orchestrator.jsonl")),
            ],
            size: None,
            session: None,
            autonomy: None,
            worktree: None,
        };
        let Outcome::Opened(orchestrator) = hub.dispatch(spawn).await else {
            panic!("the orchestrator")
        };
        let project = ProjectId::new("demo").unwrap();
        let made = hub
            .dispatch(Verb::ProjectCreate {
                project: project.clone(),
                title: "Write the notes".to_owned(),
                goal: Some("Write the notes.".to_owned()),
                autonomy: slopty_proto::project::Autonomy::Ask,
                repo: clone.to_string_lossy().into_owned(),
                target: String::new(),
                verifier: None,
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
        std::fs::write(&go_orch, "").unwrap();
        let agent = until("the task's agent starts", async || {
            let tasks = status(&hub, &project).await.tasks;
            tasks.iter().find(|c| c.id == TaskId(1))?.assignment.as_ref().map(|a| a.term)
        })
        .await;
        let record = |name: &str| -> Option<Value> {
            serde_json::from_slice(&std::fs::read(root.join(name)).ok()?).ok()
        };
        let pid_of = |name: &str| record(name).and_then(|r| r["pid"].as_u64());
        let task_pid = until("the task's agent is at work", async || pid_of("task-1.json")).await;
        let orch_pid = pid_of("orchestrator.json").expect("the orchestrator's record");
        let inbox_lines = |name: &str| {
            std::fs::read_to_string(root.join(name)).map_or(0, |text| text.lines().count())
        };
        let (task_said, orch_said) =
            (inbox_lines("inbox-1.jsonl"), inbox_lines("inbox-orchestrator.jsonl"));
        // What the worker keeps of each terminal for a reboot: its recipe, which names the
        // conversation to resume once the agent tick has found it.
        let kept_agent = |session: slopty_core::SessionId| -> Value {
            fn find(dir: &Path, name: &str) -> Option<PathBuf> {
                for entry in std::fs::read_dir(dir).ok()?.flatten() {
                    let path = entry.path();
                    if path.file_name().is_some_and(|n| n == name) {
                        return Some(path);
                    }
                    if path.is_dir()
                        && let Some(found) = find(&path, name)
                    {
                        return Some(found);
                    }
                }
                None
            }
            find(&root.join("data"), &format!("{session}.json"))
                .and_then(|path| std::fs::read(path).ok())
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .map_or(Value::Null, |recipe| recipe["agent"].clone())
        };
        let kept = soon(Duration::from_secs(10), async || {
            let both = [agent.session, orchestrator.session].map(kept_agent);
            both.iter().all(|agent| !agent.is_null()).then_some(())
        })
        .await;
        found.check(
            kept.is_some(),
            &format!(
                "the worker keeps each agent's conversation for a reboot: task {}, orchestrator {}",
                kept_agent(agent.session),
                kept_agent(orchestrator.session)
            ),
        );

        // The machine goes down: ptyd and the worker end, and their terminals' programs with
        // them, as a reboot ends them.
        let ended = slopty_testkit::group::end(&mut machine, Child::id, |child| {
            child.try_wait().is_ok_and(|status| status.is_some())
        });
        assert!(ended, "the machine's daemons ended");
        let left = soon(Duration::from_secs(10), async || {
            let alive = |pid: u64| {
                std::process::Command::new("kill")
                    .args(["-0", &pid.to_string()])
                    .status()
                    .is_ok_and(|s| s.success())
            };
            (!alive(task_pid) && !alive(orch_pid)).then_some(())
        })
        .await;
        found.check(left.is_some(), "the agents ended with their terminals");
        until("the server sees the machine away", async || {
            let away = hub
                .directory()
                .iter()
                .any(|w| w.worker == studio && w.liveness != Liveness::Online);
            away.then_some(())
        })
        .await;

        // It comes back on the same data.
        let booted = Instant::now();
        let programs = root.join("programs");
        let _machine =
            daemons(&root, server.quic_addr(), &programs, "", "projects-test", &tasks_env).await;
        let back = registered_with_claude(&hub, "projects-test").await;
        eprintln!(
            "MEASURE reboot: the machine online with Claude Code after {:?}",
            booted.elapsed()
        );
        found.check(back == studio, "the machine comes back as the same worker");

        let again =
            soon(AFTER_BOOT, async || pid_of("task-1.json").filter(|pid| *pid != task_pid)).await;
        found.check(again.is_some(), "the task's agent runs again after the reboot");
        if again.is_some() {
            eprintln!(
                "MEASURE reboot: the task's agent running again after {:?}",
                booted.elapsed()
            );
            let argv = record("task-1.json").map(|r| r["argv"].to_string()).unwrap_or_default();
            found.check(argv.contains("--resume"), &format!("on its own conversation: {argv}"));
        }
        let orch_again =
            soon(AFTER_BOOT, async || pid_of("orchestrator.json").filter(|pid| *pid != orch_pid))
                .await;
        found.check(orch_again.is_some(), "the orchestrator runs again after the reboot");
        let card = status(&hub, &project).await;
        let on = card.tasks.iter().find(|c| c.id == TaskId(1)).and_then(|c| c.assignment.clone());
        found.check(
            on.as_ref().is_some_and(|a| a.term == agent && a.open()),
            &format!("the task keeps its agent's terminal: {on:?}"),
        );
        // Told through its inbox, or by a hook as the conversation started again, whichever
        // the worker reached first.
        let told_by = |inbox: &str, said: usize, record_of: &str, words: &str| {
            let hooked = record(record_of).is_some_and(|r| r["hooks"].to_string().contains(words));
            (hooked || inbox_lines(inbox) > said).then_some(())
        };
        let told = soon(AFTER_BOOT, async || {
            told_by("inbox-1.jsonl", task_said, "task-1.json", "Carry on with the task")
        })
        .await;
        found.check(told.is_some(), "the task's agent is told it was cut off and to carry on");
        let orch_told = soon(AFTER_BOOT, async || {
            let words = "while you orchestrated this goal";
            told_by("inbox-orchestrator.jsonl", orch_said, "orchestrator.json", words)
        })
        .await;
        found.check(orch_told.is_some(), "the orchestrator is told the machine came back");

        // The task's agent goes on and reports: the goal goes on.
        std::fs::write(root.join("go-1"), "").unwrap();
        let done = soon(STEP, async || {
            let tasks = status(&hub, &project).await.tasks;
            let card = tasks.into_iter().find(|c| c.id == TaskId(1))?;
            (card.state == TaskState::Done).then_some(())
        })
        .await;
        found.check(done.is_some(), "the task's report after the reboot reaches its card");
        assert!(found.0.is_empty(), "{}", found.so_far());
        server.shutdown().await;
    }
}
