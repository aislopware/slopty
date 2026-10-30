//! Agents end to end with stand-ins: a real server in this process, a real ptyd and worker
//! registered with it, and `slopty-stub-claude` on the worker's `PATH` as `claude`, each agent its
//! own TUI in a terminal (`docs/decisions/agents.md`, "TUI-first"). No real Claude Code runs, and
//! nobody's plan is spent.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::Duration;

    use serde_json::{Value, json};
    use slopty_proto::server::Liveness;
    use slopty_proto::thread::AgentId;
    use slopty_server::{Config, Server};
    use tokio::io::{AsyncBufReadExt as _, BufReader};
    use tokio::process::{Child, Command};
    use tokio::time::Instant;

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
        // What the observer did is in a failure's output.
        let mut worker = scrubbed(bin("slopty-worker"), &dir.join("home"))
            .env("PATH", &path)
            .env("RUST_LOG", "warn,slopty_worker::thread::claude=debug")
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

    /// The lines the stand-in at seat `seat` was typed, from its record.
    fn typed_at(dir: &Path, seat: &str) -> Vec<String> {
        let at = dir.join(format!("{seat}.record.json"));
        let record = std::fs::read(at)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .unwrap_or_default();
        prompts(&record)
    }

    /// The lines a stand-in's record says it took, in order: its first prompt, then what was
    /// typed.
    fn prompts(record: &Value) -> Vec<String> {
        let taken = record["prompts"].as_array().into_iter().flatten();
        taken.filter_map(|p| p["text"].as_str().map(str::to_owned)).collect()
    }

    /// What the stand-in whose first line was `prompt` recorded, once it has.
    fn record_of(dir: &Path, prompt: &str) -> Option<Value> {
        std::fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".record.json") {
                return None;
            }
            let record: Value = serde_json::from_slice(&std::fs::read(entry.path()).ok()?).ok()?;
            (prompts(&record).first()? == prompt).then_some(record)
        })
    }

    /// What a failure shows beside its own words: every file Claude Code's stand-ins wrote under
    /// the test's home, and what each stand-in recorded.
    fn evidence(root: &Path) -> String {
        let mut files = Vec::new();
        let mut dirs = vec![root.join("home").join(".claude")];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path);
                } else {
                    files.push(path.display().to_string());
                }
            }
        }
        files.sort();
        let records: Vec<String> = std::fs::read_dir(root.join("agents"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
            .collect();
        format!("files: {files:#?}\nrecords: {}", records.join("\n"))
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
    /// machine, waits until it comes to rest, reads its turn and tells it more: the words are
    /// typed into the child's terminal as one message, marked as an agent's. The lead stops the
    /// child's next turn, which presses Esc in its terminal, and waits for it to rest again. The
    /// fleet's list names the lead as the child's parent.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_agent_starts_a_child_waits_reads_tells_and_stops_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let agents = root.join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        let folder = root.join("app");
        std::fs::create_dir_all(&folder).unwrap();
        let folder = folder.to_string_lossy().into_owned();
        let at = |what: &str| agents.join(what).to_string_lossy().into_owned();
        let wait = json!({
            "name": "agent_wait",
            "arguments": { "agents": [STARTED], "all": true, "timeout_ms": 50_000 },
        });
        let turns = json!({
            "Lead": [
                {
                    "name": "agent_start",
                    "arguments": { "machine": MACHINE, "folder": folder, "prompt": "Fix the login" },
                },
                wait,
                { "name": "agent_read", "arguments": { "agent": STARTED } },
                {
                    "name": "agent_tell",
                    "arguments": { "agent": STARTED, "text": "hold the logout too." },
                },
                // Made again until the child's turn is under way: an Esc at rest stops nothing.
                { "name": "agent_stop", "arguments": { "agent": STARTED }, "until_ok": true },
                wait,
            ],
        });
        let env = [
            ("STUB_RECORD".to_owned(), at(concat!("{", "session", "}", ".record.json"))),
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

        // The lead's turn: start, wait, read, tell, stop, wait, each answered.
        let record = until("the lead's six calls answered", || {
            record_of(&agents, "Lead").filter(|r| r["mcp"].as_array().is_some_and(|m| m.len() == 6))
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
        assert!(
            read.contains("Fix the login") && read.contains("Done: Fix the login"),
            "{read}\n{}",
            evidence(&root)
        );
        assert_eq!(tool_answer(&calls[3]), json!({ "ok": true }));
        assert_ne!(calls[4]["isError"], json!(true), "the stop: {}", calls[4]);
        let rested = tool_answer(&calls[5]);
        let phase = rested["agents"][0]["phase"].as_str().unwrap_or_default();
        assert!(
            phase.eq_ignore_ascii_case("stopped"),
            "the Esc stopped its turn: {rested}\n{}",
            evidence(&root)
        );
        let lead_turn = begun.elapsed();

        // The child was typed the words as one message, marked as the lead's, and pressed Esc.
        let child_record = record_of(&agents, "Fix the login").expect("the child's record");
        let child_seat = child_record["seat"].as_str().unwrap().to_owned();
        let told = typed_at(&agents, &child_seat);
        let told = told.iter().find(|m| m.contains("hold the logout too.")).expect("told");
        assert!(told.contains("an agent, not the person") && told.contains(&lead), "{told}");
        let child_record = record_of(&agents, "Fix the login").unwrap();
        let typed: String = child_record["reads"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r["bytes"].as_str())
            .collect();
        assert!(typed.ends_with('\u{1b}'), "Esc came last: {typed:?}");

        // The fleet's list names the lead as the child's parent.
        let listed = slopty(&root, quic, &["agents"]).await;
        let entries = listed["agents"].as_array().unwrap();
        let parent_of =
            |id: &str| entries.iter().find(|a| a["id"] == id).map(|a| a["parent"].clone());
        assert_eq!(parent_of(&child), Some(json!(lead)), "{listed}");
        assert_eq!(parent_of(&lead), Some(Value::Null), "the person's lead: {listed}");

        println!("lead's turn (start, wait, read, tell, stop, wait) {lead_turn:?}");
        server.shutdown().await;
    }
}
