//! The `slopty` binary against an in-process fake server over real QUIC on loopback: verbs as
//! subcommands (text, `--json`, errors) and `slopty mcp` speaking JSON-RPC on its stdio.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::process::Stdio;
    use std::time::Duration;

    use serde_json::{Value, json};
    use slopty_core::{SessionId, WorkerId};
    use slopty_net::admission::Admission;
    use slopty_net::server::ServerListener;
    use slopty_proto::agent::{
        AgentEvent, AgentKind, AgentSource, AgentStatus, BlockReason, SessionAgent,
    };
    use slopty_proto::orchestration::{ErrorCode, Input, Outcome, TermRef, Verb};
    use slopty_proto::server::{
        Event, FromServer, Liveness, Os, Role, ToServer, WorkerCaps, WorkerInfo,
    };
    use slopty_proto::terminal::{SessionState, SessionSummary};
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
    use tokio::process::{Child, ChildStdin, ChildStdout, Command};
    use tokio::sync::{broadcast, mpsc};

    const PATIENCE: Duration = Duration::from_secs(20);

    fn studio() -> WorkerId {
        "0199a000-0000-7000-8000-000000000001".parse().unwrap()
    }

    fn shell() -> SessionId {
        "0199a1b1-c3d4-7000-8000-00000000abcd".parse().unwrap()
    }

    fn agent() -> SessionId {
        "0199a1b2-c3d4-7000-8000-00000000abcd".parse().unwrap()
    }

    fn directory() -> Vec<WorkerInfo> {
        vec![WorkerInfo {
            worker: studio(),
            name: "mac-studio".to_owned(),
            address: "127.0.0.1:45550".to_owned(),
            liveness: Liveness::Online,
            caps: WorkerCaps {
                os: Os::MacOs,
                os_version: "26.5".to_owned(),
                arch: "aarch64".to_owned(),
                cpus: 24,
                memory: 64 << 30,
                encoders: Vec::new(),
                displays: Vec::new(),
                agents: Vec::new(),
                can_capture: true,
                can_inject: true,
                load: 0.5,
                version: "0.1.0".to_owned(),
            },
            last_seen_ms: 1,
        }]
    }

    fn summary(id: SessionId, title: &str) -> SessionSummary {
        SessionSummary {
            id,
            title: title.to_owned(),
            cwd: Some("/tmp".to_owned()),
            repo: None,
            cols: 80,
            rows: 24,
            state: SessionState::Running,
            viewers: 0,
            command: Vec::new(),
            agent: None,
        }
    }

    /// A Claude Code waiting on a permission, as its hook said.
    fn blocked() -> SessionAgent {
        SessionAgent {
            kind: AgentKind::ClaudeCode,
            status: AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".to_owned() }),
            source: AgentSource::Hook,
        }
    }

    /// What the fake answers: one online worker with a shell and a Claude Code waiting on a
    /// permission; closing anything else is an unknown terminal.
    fn answer(verb: &Verb) -> Outcome {
        match verb {
            Verb::ListWorkers => Outcome::Workers(directory()),
            Verb::ListTerminals { .. } => Outcome::Terminals(vec![
                (studio(), summary(shell(), "zsh")),
                (studio(), SessionSummary { agent: Some(blocked()), ..summary(agent(), "claude") }),
            ]),
            Verb::AgentStatus { term } if term.session == agent() => {
                Outcome::Agent(Some(blocked()))
            }
            Verb::AgentStatus { .. } => Outcome::Agent(None),
            Verb::Close { term } if term.session != shell() => Outcome::Error {
                code: ErrorCode::UnknownTerminal,
                message: format!("no terminal {}", term.session),
            },
            _ => Outcome::Done,
        }
    }

    /// A server that answers every link's requests from [`answer`], reports each verb and each
    /// dialer's role, and sends links what the test pushes.
    struct Fake {
        port: u16,
        verbs: mpsc::UnboundedReceiver<Verb>,
        roles: mpsc::UnboundedReceiver<Role>,
        push: broadcast::Sender<Option<FromServer>>,
    }

    impl Fake {
        fn start() -> Self {
            let local = SocketAddr::from(([127, 0, 0, 1], 0));
            let listener = ServerListener::bind(local, Admission::default()).unwrap();
            let port = listener.local_addr().unwrap().port();
            let (verb_tx, verbs) = mpsc::unbounded_channel();
            let (role_tx, roles) = mpsc::unbounded_channel();
            let (push, _) = broadcast::channel(16);
            let pushes = push.clone();
            tokio::spawn(async move {
                while let Some(mut link) = listener.accept().await {
                    role_tx.send(link.role.clone()).unwrap();
                    let welcome = FromServer::Welcome { name: "fake".to_owned() };
                    link.tx.send(&welcome).await.unwrap();
                    link.tx.send(&FromServer::Directory(directory())).await.unwrap();
                    let verb_tx = verb_tx.clone();
                    let mut pushed = pushes.subscribe();
                    tokio::spawn(async move {
                        loop {
                            tokio::select! {
                                msg = link.rx.recv() => match msg {
                                    Ok(ToServer::Request { id, verb }) => {
                                        let outcome = answer(&verb);
                                        verb_tx.send(verb).unwrap();
                                        let reply = FromServer::Reply { id, outcome };
                                        if link.tx.send(&reply).await.is_err() {
                                            return;
                                        }
                                    }
                                    Ok(other) => panic!("a client sent {other:?}"),
                                    Err(_closed) => return,
                                },
                                // `None` drops the link, as a restarting server would.
                                push = pushed.recv() => {
                                    if let Ok(Some(msg)) = push {
                                        link.tx.send(&msg).await.unwrap();
                                    } else {
                                        link.conn.close(0_u32.into(), b"restart");
                                        return;
                                    }
                                }
                            }
                        }
                    });
                }
            });
            Self { port, verbs, roles, push }
        }

        fn address(&self) -> String {
            format!("127.0.0.1:{}", self.port)
        }

        async fn next_verb(&mut self) -> Verb {
            tokio::time::timeout(PATIENCE, self.verbs.recv()).await.unwrap().unwrap()
        }

        async fn next_role(&mut self) -> Role {
            tokio::time::timeout(PATIENCE, self.roles.recv()).await.unwrap().unwrap()
        }
    }

    fn slopty(data: &tempfile::TempDir, server: &str, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_slopty"));
        cmd.arg("--server")
            .arg(server)
            .arg("--data-dir")
            .arg(data.path())
            .args(args)
            .env_remove("SLOPTY_SERVER")
            .env("RUST_LOG", "warn")
            .kill_on_drop(true);
        cmd
    }

    struct Ran {
        ok: bool,
        stdout: String,
        stderr: String,
    }

    async fn run(data: &tempfile::TempDir, server: &str, args: &[&str]) -> Ran {
        let out = tokio::time::timeout(PATIENCE, slopty(data, server, args).output())
            .await
            .unwrap()
            .unwrap();
        Ran {
            ok: out.status.success(),
            stdout: String::from_utf8(out.stdout).unwrap(),
            stderr: String::from_utf8(out.stderr).unwrap(),
        }
    }

    #[tokio::test]
    async fn workers_round_trips_as_stable_json() {
        let mut fake = Fake::start();
        let data = tempfile::tempdir().unwrap();
        let ran = run(&data, &fake.address(), &["--json", "workers"]).await;
        assert!(ran.ok, "{}", ran.stderr);
        let Role::Client { name } = fake.next_role().await else { panic!("a client") };
        assert!(name.starts_with("slopty @ "), "{name}");
        let workers: Value = serde_json::from_str(&ran.stdout).unwrap();
        let term = format!("{}/{}", studio(), agent());
        assert_eq!(
            workers,
            json!([{
                "worker": studio().to_string(),
                "name": "mac-studio",
                "liveness": "online",
                "address": "127.0.0.1:45550",
                "os": "macos",
                "os_version": "26.5",
                "arch": "aarch64",
                "cpus": 24,
                "memory": 64_u64 << 30,
                "load": 0.5,
                "version": "0.1.0",
                "can_capture": true,
                "can_inject": true,
                "last_seen_ms": 1,
                "terminals": 2,
                "waiting": [{
                    "term": term, "agent": "claude_code", "reason": "permission", "tool": "Bash"
                }],
            }])
        );
        // The directory and the terminals, each carrying its agent: no status request follows.
        let verbs = [fake.next_verb().await, fake.next_verb().await];
        assert!(verbs.contains(&Verb::ListWorkers), "{verbs:?}");
        assert!(verbs.contains(&Verb::ListTerminals { worker: None }), "{verbs:?}");
        assert!(fake.verbs.try_recv().is_err(), "no status request per terminal");
    }

    #[tokio::test]
    async fn a_worker_name_and_a_session_prefix_resolve_to_ids() {
        let mut fake = Fake::start();
        let data = tempfile::tempdir().unwrap();
        let ran =
            run(&data, &fake.address(), &["send", "mac-studio/0199a1b2", "--keys", "ctrl+c"]).await;
        assert!(ran.ok, "{}", ran.stderr);
        assert_eq!(ran.stdout, "", "a done verb prints nothing");
        assert_eq!(fake.next_verb().await, Verb::ListWorkers);
        assert_eq!(fake.next_verb().await, Verb::ListTerminals { worker: Some(studio()) });
        assert_eq!(
            fake.next_verb().await,
            Verb::SendInput {
                term: TermRef { worker: studio(), session: agent() },
                input: Input::Keys(vec!["ctrl+c".to_owned()]),
            }
        );

        let ran = run(&data, &fake.address(), &["terminals"]).await;
        assert!(ran.ok, "{}", ran.stderr);
        assert!(ran.stdout.starts_with("TERM "), "{}", ran.stdout);
        assert!(ran.stdout.contains("mac-studio/0199a1b2  running"), "{}", ran.stdout);
        assert!(ran.stdout.contains("waiting: permission for Bash (hooks)"), "{}", ran.stdout);
    }

    #[tokio::test]
    async fn an_error_outcome_exits_non_zero_with_its_message() {
        let fake = Fake::start();
        let data = tempfile::tempdir().unwrap();
        let term = format!("{}/{}", studio(), SessionId::nil());
        let ran = run(&data, &fake.address(), &["close", &term]).await;
        assert!(!ran.ok, "an error outcome fails the command");
        assert!(ran.stderr.contains("no terminal"), "{}", ran.stderr);
        assert_eq!(ran.stdout, "");
    }

    #[tokio::test]
    async fn no_server_is_a_readable_error() {
        let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let data = tempfile::tempdir().unwrap();
        let address = silent.local_addr().unwrap().to_string();
        let ran = run(&data, &address, &["workers"]).await;
        assert!(!ran.ok);
        assert!(ran.stderr.contains("cannot reach the server"), "{}", ran.stderr);
    }

    /// `slopty mcp` as a child, spoken to over its stdio.
    struct Mcp {
        _child: Child,
        stdin: ChildStdin,
        stdout: tokio::io::Lines<BufReader<ChildStdout>>,
    }

    impl Mcp {
        fn spawn(data: &tempfile::TempDir, server: &str) -> Self {
            let mut child = slopty(data, server, &["mcp"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            let stdin = child.stdin.take().unwrap();
            let stdout = BufReader::new(child.stdout.take().unwrap()).lines();
            Self { _child: child, stdin, stdout }
        }

        /// A request in the 2026-07-28 inline lifecycle: no `initialize`, the protocol version
        /// and capabilities in every request's `_meta`.
        async fn request(&mut self, id: u64, method: &str, mut params: Value) {
            params["_meta"] = json!({
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {},
                "io.modelcontextprotocol/clientInfo": { "name": "test", "version": "0" },
            });
            let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
            self.stdin.write_all(format!("{msg}\n").as_bytes()).await.unwrap();
            self.stdin.flush().await.unwrap();
        }

        async fn next(&mut self) -> Value {
            let line = tokio::time::timeout(PATIENCE, self.stdout.next_line())
                .await
                .expect("an answer in time")
                .unwrap()
                .expect("the shim is still running");
            serde_json::from_str(&line).unwrap()
        }

        /// The reply to `id`, skipping notifications.
        async fn reply(&mut self, id: u64) -> Value {
            loop {
                let msg = self.next().await;
                if msg["id"] == id {
                    return msg;
                }
                assert!(msg.get("method").is_some(), "only notifications come between: {msg}");
            }
        }
    }

    /// The text of a tool result, parsed as JSON.
    fn tool_json(reply: &Value) -> Value {
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn mcp_lists_tools_calls_them_and_announces_a_blocked_agent() {
        let mut fake = Fake::start();
        let data = tempfile::tempdir().unwrap();
        let mut mcp = Mcp::spawn(&data, &fake.address());

        mcp.request(1, "tools/list", json!({})).await;
        let list = mcp.reply(1).await;
        let tools = list["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 19, "{list}");
        assert_eq!(tools[0]["name"], "list_workers");
        let read_output = tools.iter().find(|t| t["name"] == "read_output").unwrap();
        assert!(read_output["description"].as_str().unwrap().contains("`next`"), "{read_output}");

        mcp.request(2, "tools/call", json!({ "name": "list_workers", "arguments": {} })).await;
        let workers = tool_json(&mcp.reply(2).await);
        assert_eq!(workers[0]["name"], "mac-studio");
        assert_eq!(workers[0]["waiting"][0]["reason"], "permission");
        let Role::Agent { name } = fake.next_role().await else { panic!("an agent") };
        assert!(name.starts_with("slopty mcp @ "), "{name}");

        // An agent that comes to need a human is announced unasked.
        let event = AgentEvent {
            session: shell(),
            kind: AgentKind::ClaudeCode,
            status: AgentStatus::Blocked(BlockReason::Question),
            agent_session: None,
            detail: Some("Which branch?".to_owned()),
            attention: true,
            source: AgentSource::Hook,
        };
        let pushed = FromServer::Event(Event::Agent { worker: studio(), event });
        fake.push.send(Some(pushed)).unwrap();
        let note = mcp.next().await;
        assert_eq!(note["method"], "notifications/message", "{note}");
        assert_eq!(note["params"]["level"], "warning");
        assert_eq!(note["params"]["data"]["term"], format!("{}/{}", studio(), shell()));
        assert_eq!(note["params"]["data"]["agent"]["reason"], "question");
        assert!(
            note["params"]["data"]["message"].as_str().unwrap().contains("mac-studio"),
            "{note}"
        );

        // A tool error is the model's to read, not a protocol error.
        let unknown = format!("{}/{}", studio(), SessionId::nil());
        let args = json!({ "name": "close_terminal", "arguments": { "term": unknown } });
        mcp.request(3, "tools/call", args).await;
        let failed = mcp.reply(3).await;
        assert_eq!(failed["result"]["isError"], true, "{failed}");
        let text = failed["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("no terminal"), "{text}");

        // The server restarts: the shim redials and the next call goes through.
        fake.push.send(None).unwrap();
        let Role::Agent { .. } = fake.next_role().await else { panic!("the agent again") };
        let args = json!({ "name": "list_terminals", "arguments": { "worker": "mac-studio" } });
        mcp.request(4, "tools/call", args).await;
        let terminals = tool_json(&mcp.reply(4).await);
        assert_eq!(terminals[1]["term"], format!("{}/{}", studio(), agent()));
        assert_eq!(terminals[1]["worker_name"], "mac-studio");
        assert_eq!(terminals[1]["agent"]["source"], "hook", "{terminals}");
        assert!(terminals[0].get("agent").is_none(), "a shell has no agent: {terminals}");
    }
}
