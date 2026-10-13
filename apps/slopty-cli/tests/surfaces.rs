//! The one MCP surface: `tools/call`s through `slopty mcp` dialled to a real server, with a fake
//! worker on loopback, answer with the tools' views, a tool's error as a result the model reads.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::process::Stdio;
    use std::time::Duration;

    use serde_json::{Value, json};
    use slopty_core::{SessionId, WallMs, WorkerId};
    use slopty_net::HostAddr;
    use slopty_net::admission::Admission;
    use slopty_net::client::bind_client;
    use slopty_net::server::{ServerLink, connect};
    use slopty_proto::orchestration::Outcome;
    use slopty_proto::server::{FromServer, Os, Registration, Role, ToServer, WorkerCaps};
    use slopty_proto::terminal::{SessionState, SessionSummary};
    use slopty_server::{Config, Server};
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
    use tokio::process::Command;

    /// The CLI, started from a clean environment with its home at `home`
    /// (`slopty_testkit::env::scrub`): nothing of the developer's reaches it.
    fn scrubbed(home: &std::path::Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_slopty"));
        slopty_testkit::env::scrub(command.as_std_mut(), home);
        command
    }

    const PATIENCE: Duration = Duration::from_secs(20);
    const REVISION: &str = "2026-07-28";

    fn shell() -> SessionId {
        "0199a1b1-c3d4-7000-8000-00000000abcd".parse().unwrap()
    }

    fn registration(worker: WorkerId) -> Registration {
        Registration {
            worker,
            name: "fake-worker".to_owned(),
            listen: SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 45550)),
            caps: WorkerCaps {
                os: Os::MacOs,
                os_version: "26.5".to_owned(),
                arch: "aarch64".to_owned(),
                form: slopty_proto::server::Form::Desktop,
                cpus: 8,
                memory: 16 << 30,
                encoders: Vec::new(),
                displays: Vec::new(),
                agents: Vec::new(),
                can_capture: false,
                can_inject: false,
                virtual_displays: false,
                curtain: false,
                build: "0".to_owned(),
                lan: Vec::new(),
                wake_on_lan: None,
                writes_failing: None,
                stops_at_logout: None,
            },
            sessions: vec![SessionSummary {
                id: shell(),
                title: "claude".to_owned(),
                cwd: Some("/tmp".to_owned()),
                repo: None,
                branch: None,
                changes: None,
                started_ms: WallMs::ZERO,
                cols: 80,
                rows: 24,
                state: SessionState::Running,
                viewers: 0,
                command: Vec::new(),
                progress: None,
                restored: None,
                program: Vec::new(),
                repo_id: None,
            }],
            session_key: [7; 32],
        }
    }

    /// Answer every request the server forwards as done, until the link ends.
    async fn work(mut link: ServerLink) {
        while let Ok(msg) = link.rx.recv().await {
            if let FromServer::Request { id, .. } = msg {
                let reply = ToServer::Reply { id, outcome: Outcome::Done };
                if link.tx.send(&reply).await.is_err() {
                    return;
                }
            }
        }
    }

    fn request(id: u64, name: &str, arguments: &Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments,
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": REVISION,
                    "io.modelcontextprotocol/clientInfo": { "name": "test", "version": "0" },
                    "io.modelcontextprotocol/clientCapabilities": {},
                },
            },
        })
    }

    /// The tool result of an answer: whether it is an error, and its text parsed.
    fn result(reply: &Value) -> (Value, Value) {
        let result = &reply["result"];
        assert!(result.is_object(), "a tool result: {reply}");
        let text = result["content"][0]["text"].as_str().unwrap();
        let parsed = serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned()));
        (result["isError"].clone(), parsed)
    }

    #[tokio::test]
    async fn slopty_mcp_answers_each_call_with_the_tools_views() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(Config {
            name: "test-server".to_owned(),
            quic: "127.0.0.1:0".parse().unwrap(),
            data_dir: dir.path().to_path_buf(),
            admission: Admission::default(),
            push: slopty_server::PushConfig::Off,
        })
        .await
        .unwrap();
        let worker = WorkerId::new();
        let endpoint = bind_client().unwrap();
        let quic = HostAddr::from(server.quic_addr());
        let link =
            connect(&endpoint, &quic, Role::Worker(Box::new(registration(worker)))).await.unwrap();
        let working = tokio::spawn(work(link));

        let data = tempfile::tempdir().unwrap();
        let mut child = scrubbed(data.path())
            .arg("--server")
            .arg(server.quic_addr().to_string())
            .arg("--data-dir")
            .arg(data.path())
            .arg("mcp")
            .env("RUST_LOG", "warn")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();

        let nobody = "0199a1b1-c3d4-7000-8000-00000000beef";
        let calls = [
            ("machines", json!({})),
            ("agents", json!({})),
            ("agent_read", json!({ "agent": "nope" })),
            ("agent_read", json!({ "agent": nobody })),
            ("agent_tell", json!({ "agent": nobody, "text": "hi" })),
            ("agent_start", json!({ "machine": "no-such-machine", "prompt": "Fix it" })),
        ];
        let mut answers = Vec::new();
        for (id, (name, arguments)) in (1_u64..).zip(&calls) {
            let line = format!("{}\n", request(id, name, arguments));
            stdin.write_all(line.as_bytes()).await.unwrap();
            stdin.flush().await.unwrap();
            let answer = loop {
                let line = tokio::time::timeout(PATIENCE, stdout.next_line())
                    .await
                    .expect("an answer in time")
                    .unwrap()
                    .expect("the shim is still running");
                let msg: Value = serde_json::from_str(&line).unwrap();
                if msg["id"] == id {
                    break result(&msg);
                }
            };
            println!("{name}: {}", answer.1);
            answers.push(answer);
        }

        // The answers are the tools' views, not the wire's; a tool's error is a result.
        let [machines, agents, garbled, read, told, started] = answers.as_slice() else {
            panic!("{answers:?}")
        };
        assert_ne!(machines.0, json!(true), "{machines:?}");
        assert!(machines.1.to_string().contains("fake-worker"), "{machines:?}");
        assert_ne!(agents.0, json!(true), "{agents:?}");
        assert_eq!(agents.1["agents"], json!([]), "{agents:?}");
        for (answered, code) in [
            (garbled, "(Invalid)"),
            (read, "(UnknownAgent)"),
            (told, "(UnknownAgent)"),
            (started, "(UnknownWorker)"),
        ] {
            assert_eq!(answered.0, json!(true), "{answered:?}");
            assert!(answered.1.as_str().unwrap().ends_with(code), "{answered:?}");
        }

        drop(stdin);
        let _exited = tokio::time::timeout(PATIENCE, child.wait()).await;
        working.abort();
        server.shutdown().await;
    }
}
