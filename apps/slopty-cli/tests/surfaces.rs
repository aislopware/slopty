//! One contract on two surfaces: the same `tools/call`s through the server's MCP endpoint over
//! HTTP and through `slopty mcp` dialled to that server give the same results, over a real
//! server and a fake worker on loopback.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::process::Stdio;
    use std::time::Duration;

    use serde_json::{Value, json};
    use slopty_core::{SessionId, WorkerId};
    use slopty_net::HostAddr;
    use slopty_net::admission::Admission;
    use slopty_net::client::bind_client;
    use slopty_net::server::{ServerLink, connect};
    use slopty_proto::agent::{AgentKind, AgentStatus, BlockReason};
    use slopty_proto::orchestration::{ErrorCode, Line, Outcome, Screen, Verb, Waited};
    use slopty_proto::server::{FromServer, Os, Registration, Role, ToServer, WorkerCaps};
    use slopty_proto::terminal::{SessionKind, SessionState, SessionSummary};
    use slopty_server::{Config, Server};
    use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
    use tokio::process::Command;

    const PATIENCE: Duration = Duration::from_secs(20);
    const REVISION: &str = "2026-07-28";

    fn shell() -> SessionId {
        "0199a1b1-c3d4-7000-8000-00000000abcd".parse().unwrap()
    }

    fn registration(worker: WorkerId) -> Registration {
        Registration {
            worker,
            name: "fake-worker".to_owned(),
            port: 45550,
            caps: WorkerCaps {
                os: Os::MacOs,
                os_version: "26.5".to_owned(),
                arch: "aarch64".to_owned(),
                cpus: 8,
                memory: 16 << 30,
                encoders: Vec::new(),
                displays: Vec::new(),
                agents: Vec::new(),
                can_capture: false,
                can_inject: false,
                load: 0.1,
                version: "0".to_owned(),
            },
            sessions: vec![SessionSummary {
                id: shell(),
                kind: SessionKind::Terminal,
                title: "claude".to_owned(),
                cwd: Some("/tmp".to_owned()),
                repo: None,
                cols: 80,
                rows: 24,
                state: SessionState::Running,
                viewers: 0,
                command: Vec::new(),
            }],
        }
    }

    fn answer(verb: &Verb) -> Outcome {
        match verb {
            Verb::AgentStatus { .. } => Outcome::Agent(Some((
                AgentKind::ClaudeCode,
                AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".to_owned() }),
            ))),
            Verb::ReadScreen { .. } => Outcome::Screen(Screen {
                lines: vec![Line { index: 40, text: "$ ready".to_owned() }],
                cursor: (0, 7),
                title: "zsh".to_owned(),
                cwd: Some("/tmp".to_owned()),
                alternate: false,
            }),
            Verb::ReadFile { .. } => Outcome::File(vec![0xff, 0]),
            Verb::WaitFor { .. } => Outcome::Waited(Waited::TimedOut),
            Verb::Close { .. } => Outcome::Error {
                code: ErrorCode::UnknownTerminal,
                message: "no such terminal".to_owned(),
            },
            _ => Outcome::Done,
        }
    }

    /// Answer every request the server forwards, until the link ends.
    async fn work(mut link: ServerLink) {
        while let Ok(msg) = link.rx.recv().await {
            if let FromServer::Request { id, verb } = msg {
                let reply = ToServer::Reply { id, outcome: answer(&verb) };
                if link.tx.send(&reply).await.is_err() {
                    return;
                }
            }
        }
    }

    /// `tools/call` on the server's endpoint: one POST, one JSON body.
    async fn over_http(mcp: SocketAddr, id: u64, name: &str, arguments: &Value) -> Value {
        let body = request(id, name, arguments).to_string();
        let mut stream = tokio::net::TcpStream::connect(mcp).await.unwrap();
        let head = format!(
            "POST /mcp HTTP/1.1\r\nHost: {mcp}\r\nContent-Type: application/json\r\n\
             Accept: application/json, text/event-stream\r\nMCP-Protocol-Version: {REVISION}\r\n\
             Mcp-Method: tools/call\r\nMcp-Name: {name}\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(body.as_bytes()).await.unwrap();
        let mut response = String::new();
        tokio::time::timeout(PATIENCE, stream.read_to_string(&mut response))
            .await
            .unwrap()
            .unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("HTTP/1.1 200"), "{response}");
        serde_json::from_str(body).unwrap()
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

    /// The part of an answer both surfaces must agree on: the tool result, its text parsed.
    fn result(reply: &Value) -> (Value, Value) {
        let result = &reply["result"];
        assert!(result.is_object(), "a tool result: {reply}");
        let text = result["content"][0]["text"].as_str().unwrap();
        let parsed = serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned()));
        (result["isError"].clone(), parsed)
    }

    #[tokio::test]
    async fn the_server_endpoint_and_slopty_mcp_answer_a_call_alike() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(Config {
            name: "test-server".to_owned(),
            quic: "127.0.0.1:0".parse().unwrap(),
            mcp: "127.0.0.1:0".parse().unwrap(),
            data_dir: dir.path().to_path_buf(),
            admission: Admission::default(),
        })
        .await
        .unwrap();
        let worker = WorkerId::new();
        let endpoint = bind_client().unwrap();
        let quic = HostAddr::from(server.quic_addr());
        let link = connect(&endpoint, &quic, Role::Worker(registration(worker))).await.unwrap();
        let working = tokio::spawn(work(link));

        let data = tempfile::tempdir().unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_slopty"))
            .arg("--server")
            .arg(server.quic_addr().to_string())
            .arg("--data-dir")
            .arg(data.path())
            .arg("mcp")
            .env_remove("SLOPTY_SERVER")
            .env("RUST_LOG", "warn")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();

        let prefix = shell().to_string();
        let by_name = format!("fake-worker/{}", prefix.get(..8).unwrap());
        let unknown = format!("{worker}/{}", SessionId::nil());
        let calls = [
            ("list_workers", json!({})),
            ("list_terminals", json!({ "worker": "fake-worker" })),
            ("agent_status", json!({ "term": by_name })),
            ("read_screen", json!({ "term": by_name })),
            ("read_file", json!({ "path": "/bin/x" })),
            ("wait_for", json!({ "term": by_name, "exit": true, "timeout_ms": 1000 })),
            ("close_terminal", json!({ "term": unknown })),
            ("read_output", json!({ "term": "no-such-worker/0199" })),
        ];
        for (id, (name, arguments)) in (1_u64..).zip(&calls) {
            let http = result(&over_http(server.mcp_addr(), id, name, arguments).await);

            let line = format!("{}\n", request(id, name, arguments));
            stdin.write_all(line.as_bytes()).await.unwrap();
            stdin.flush().await.unwrap();
            let stdio = loop {
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

            let (mut http, mut stdio) = (http, stdio);
            if *name == "list_workers" {
                // The hub stamps a worker on every message it sends, and the first call's
                // agent-status answers land between the two listings.
                http.1[0].as_object_mut().unwrap().remove("last_seen_ms");
                stdio.1[0].as_object_mut().unwrap().remove("last_seen_ms");
            }
            assert_eq!(http, stdio, "{name} answers alike on both surfaces");
            println!("{name}: {}", http.1);
        }

        // Spot checks that the shared answers are the tools' views, not the wire's.
        let file = result(&over_http(server.mcp_addr(), 99, "read_file", &calls[4].1).await).1;
        assert_eq!(
            file,
            json!({ "path": "/bin/x", "size": 2, "encoding": "base64", "content": "/wA=" })
        );
        let closed = result(&over_http(server.mcp_addr(), 98, "close_terminal", &calls[6].1).await);
        assert_eq!(closed, (json!(true), json!("no such terminal (UnknownTerminal)")));

        drop(stdin);
        let _exited = tokio::time::timeout(PATIENCE, child.wait()).await;
        working.abort();
        server.shutdown().await;
    }
}
