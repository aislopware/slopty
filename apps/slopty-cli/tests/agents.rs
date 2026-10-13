//! Agents end to end with stand-ins: a real server in this process, a real ptyd and worker
//! registered with it, and `slopty-stub-claude` on the worker's `PATH` as `claude`. No real
//! Claude Code runs, and nobody's plan is spent.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};
    use slopty_proto::server::Liveness;
    use slopty_proto::thread::AgentId;
    use slopty_server::{Config, Server};
    use tokio::io::{AsyncBufReadExt as _, BufReader};
    use tokio::process::{Child, Command};

    /// Long enough for any step on a loaded runner; the test judges what arrives.
    const STEP: Duration = Duration::from_secs(60);

    /// The worker's name, as an agent names the machine.
    const MACHINE: &str = "agents-test";

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
    /// `programs` first; every terminal ptyd starts has `env` beside its own. Killed with the
    /// test.
    async fn daemons(
        dir: &Path,
        server: SocketAddr,
        programs: &Path,
        env: &[(String, String)],
    ) -> Vec<Child> {
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let path = slopty_testkit::env::path_with(programs);
        let ptyd_sock = dir.join("ptyd.sock");
        let mut ptyd = scrubbed(bin("slopty-ptyd"), &dir.join("home"))
            .env("PATH", &path)
            .env("RUST_LOG", "warn")
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
        let mut worker = scrubbed(bin("slopty-worker"), &dir.join("home"))
            .env("PATH", &path)
            .env("RUST_LOG", "warn")
            .env("SLOPTY_WORKER_NAME", MACHINE)
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
        vec![ptyd, worker]
    }

    /// Wait until `done` holds, checking every 50 ms.
    async fn until<T>(what: &str, mut done: impl FnMut() -> Option<T>) -> T {
        let waited = tokio::time::timeout(STEP, async {
            loop {
                if let Some(found) = done() {
                    return found;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        waited.unwrap_or_else(|_| panic!("{what}"))
    }

    /// `slopty --json <args>` against `server`, its own data under `root`: what it printed.
    async fn slopty(root: &Path, server: SocketAddr, args: &[&str]) -> Value {
        let ran = scrubbed(bin("slopty"), &root.join("home"))
            .arg("--server")
            .arg(server.to_string())
            .arg("--data-dir")
            .arg(root.join("cli"))
            .arg("--json")
            .args(args)
            .env("RUST_LOG", "warn")
            .kill_on_drop(true)
            .output();
        let out = tokio::time::timeout(STEP, ran).await.expect("slopty answers").unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "slopty {args:?}: {stderr}");
        serde_json::from_slice(&out.stdout).unwrap()
    }

    /// What the stub started with `prompt` recorded, once it has.
    fn record_of(dir: &Path, prompt: &str) -> Option<Value> {
        std::fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".record.json") {
                return None;
            }
            let record: Value = serde_json::from_slice(&std::fs::read(entry.path()).ok()?).ok()?;
            let argv = record["argv"].as_array()?;
            (argv.last()?.as_str()? == prompt).then_some(record)
        })
    }

    /// The messages the inbox of the agent in terminal `session` took.
    fn inbox_of(dir: &Path, session: &str) -> Vec<String> {
        let text =
            std::fs::read_to_string(dir.join(format!("{session}.inbox"))).unwrap_or_default();
        text.lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter_map(|got| got["content"].as_str().map(str::to_owned))
            .collect()
    }

    /// The JSON an MCP tool result carries as its text.
    fn tool_answer(result: &Value) -> Value {
        assert_ne!(result["isError"], json!(true), "{result}");
        let text = result.pointer("/content/0/text").and_then(Value::as_str).unwrap();
        serde_json::from_str(text).unwrap()
    }

    /// Where a stand-in's tool call names the agent its turn last started.
    const STARTED: &str = concat!("{", "started", "}");

    /// The person starts a lead agent; the lead, through Slopty's tools, starts a child on the
    /// machine, waits until it comes to rest, reads its turn and tells it more. The child takes
    /// the words through its own hooks, marked as an agent's, and once it rests again the lead
    /// hears it through its hooks, unasked. The fleet's list names the lead as the child's
    /// parent.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_agent_starts_a_child_waits_reads_it_and_hears_it_through_its_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let agents = root.join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        let folder = root.join("app");
        std::fs::create_dir_all(&folder).unwrap();
        let folder = folder.to_string_lossy().into_owned();
        let at = |what: &str| agents.join(what).to_string_lossy().into_owned();
        // Where Claude Code keeps the folder's conversations, under the daemons' home: where the
        // worker finds what an agent said.
        let escaped: String =
            folder.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        let transcripts = root.join("home/.claude/projects").join(escaped);
        std::fs::create_dir_all(&transcripts).unwrap();
        let turns = json!({
            "Lead": [
                {
                    "name": "agent_start",
                    "arguments": { "machine": MACHINE, "folder": folder, "prompt": "Fix the login" },
                },
                {
                    "name": "agent_wait",
                    "arguments": { "agents": [STARTED], "all": true, "timeout_ms": 50_000 },
                },
                { "name": "agent_read", "arguments": { "agent": STARTED } },
                {
                    "name": "agent_tell",
                    "arguments": { "agent": STARTED, "text": "Now the logout too." },
                },
            ],
        });
        let env = [
            ("STUB_RECORD".to_owned(), at("{session}.record.json")),
            (
                "STUB_TRANSCRIPT".to_owned(),
                transcripts.join("{conversation}.jsonl").to_string_lossy().into_owned(),
            ),
            ("STUB_INBOX".to_owned(), at("{session}.inbox")),
            ("STUB_TURNS".to_owned(), turns.to_string()),
        ];

        let server = Server::start(Config {
            name: "agents-test".to_owned(),
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
        let _daemons = daemons(&root, server.quic_addr(), &programs, &env).await;
        until("the worker registers with Claude Code installed", || {
            hub.directory().into_iter().find(|w| {
                w.liveness == Liveness::Online
                    && w.caps.agents.iter().any(|a| a.agent.is(AgentId::CLAUDE_CODE))
            })
        })
        .await;
        let quic = server.quic_addr();

        // The person starts the lead.
        let begun = Instant::now();
        let started =
            slopty(&root, quic, &["agent", "start", MACHINE, "--folder", &folder, "Lead"]).await;
        let lead = started["agent"].as_str().unwrap().to_owned();

        // The lead's turn: start, wait, read, tell, each answered.
        let record = until("the lead's four calls answered", || {
            record_of(&agents, "Lead").filter(|r| r["mcp"].as_array().is_some_and(|m| m.len() == 4))
        })
        .await;
        let calls = record["mcp"].as_array().unwrap();
        let child = tool_answer(&calls[0])["agent"].as_str().unwrap().to_owned();
        let waited = tool_answer(&calls[1]);
        assert_ne!(waited["timed_out"], json!(true), "{waited}");
        let seen = &waited["agents"][0];
        assert_eq!((seen["id"].as_str(), seen["parent"].as_str()), (Some(&*child), Some(&*lead)));
        assert_eq!(seen["rests"], json!(true), "{waited}");
        let read = calls[2].pointer("/content/0/text").and_then(Value::as_str).unwrap();
        assert!(read.contains("Fix the login") && read.contains("Done: Fix the login"), "{read}");
        assert_eq!(tool_answer(&calls[3]), json!({ "ok": true }));
        let lead_turn = begun.elapsed();

        // The child took the words through its hooks, marked as the lead's.
        let child_record = record_of(&agents, "Fix the login").expect("the child's record");
        let child_seat = child_record["env"]["SLOPTY_SESSION"].as_str().unwrap().to_owned();
        let told = until("the child takes the lead's words", || {
            inbox_of(&agents, &child_seat).into_iter().find(|m| m.contains("Now the logout too."))
        })
        .await;
        assert!(told.contains("an agent, not the person") && told.contains(&lead), "{told}");

        // Once the child rests again, the lead hears it through its own hooks.
        let lead_record = record_of(&agents, "Lead").unwrap();
        let lead_seat = lead_record["env"]["SLOPTY_SESSION"].as_str().unwrap().to_owned();
        let heard_at = Instant::now();
        let heard = until("the lead hears the child rest", || {
            inbox_of(&agents, &lead_seat).into_iter().find(|m| m.contains("came to rest"))
        })
        .await;
        assert!(heard.contains(&child) && heard.contains("Now the logout too."), "{heard}");
        let heard_after = heard_at.elapsed();

        // The fleet's list names the lead as the child's parent.
        let listed = slopty(&root, quic, &["agents"]).await;
        let entries = listed["agents"].as_array().unwrap();
        let parent_of =
            |id: &str| entries.iter().find(|a| a["id"] == id).map(|a| a["parent"].clone());
        assert_eq!(parent_of(&child), Some(json!(lead)), "{listed}");
        assert_eq!(parent_of(&lead), Some(Value::Null), "the person's lead: {listed}");

        println!(
            "lead's turn (start, wait, read, tell) {lead_turn:?}; heard the child rest {heard_after:?} \
             after it took the words"
        );
        server.shutdown().await;
    }
}
