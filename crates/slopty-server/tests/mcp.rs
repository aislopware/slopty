//! The MCP endpoint over real HTTP on loopback: every tool is listed, a call runs through the same
//! dispatch as the QUIC links, and a browser's request or a peer outside the admitted ranges is
//! turned away.

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    use std::net::SocketAddr;
    use std::time::Duration;

    use serde_json::{Value, json};
    use slopty_core::WorkerId;
    use slopty_net::HostAddr;
    use slopty_net::admission::Admission;
    use slopty_net::client::bind_client;
    use slopty_proto::server::{Os, Registration, Role, WorkerCaps};
    use slopty_server::{Config, Hub, Server};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    const REVISION: &str = "2026-07-28";

    /// POST one JSON-RPC message to `/mcp` and read the whole response: status and body.
    async fn post(addr: SocketAddr, body: &Value, extra: &[(&str, &str)]) -> (u16, String) {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let body = body.to_string();
        let mut request = format!(
            "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
             Accept: application/json, text/event-stream\r\nContent-Length: {}\r\n\
             Connection: close\r\n",
            body.len()
        );
        for (name, value) in extra {
            write!(request, "{name}: {value}\r\n").unwrap();
        }
        request.push_str("\r\n");
        request.push_str(&body);
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        tokio::time::timeout(Duration::from_secs(10), stream.read_to_string(&mut response))
            .await
            .unwrap()
            .unwrap();
        let status = response.split(' ').nth(1).unwrap().parse().unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        let head = head.to_ascii_lowercase();
        assert!(!head.contains("transfer-encoding: chunked"), "a whole JSON body: {head}");
        (status, body.to_owned())
    }

    fn message(id: u64, method: &str, params: Value) -> Value {
        let mut params = params;
        params["_meta"] = json!({
            "io.modelcontextprotocol/protocolVersion": REVISION,
            "io.modelcontextprotocol/clientInfo": { "name": "test", "version": "0" },
            "io.modelcontextprotocol/clientCapabilities": {},
        });
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    async fn rpc(
        addr: SocketAddr,
        id: u64,
        method: &str,
        name: Option<&str>,
        params: Value,
    ) -> Value {
        let mut headers = vec![("MCP-Protocol-Version", REVISION), ("Mcp-Method", method)];
        if let Some(name) = name {
            headers.push(("Mcp-Name", name));
        }
        let (status, body) = post(addr, &message(id, method, params), &headers).await;
        assert_eq!(status, 200, "{body}");
        serde_json::from_str(&body).unwrap()
    }

    fn caps() -> WorkerCaps {
        WorkerCaps {
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
        }
    }

    #[tokio::test]
    async fn every_tool_is_listed_and_a_call_runs_the_verb() {
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
        let role = Role::Worker(Registration {
            worker,
            name: "fake-worker".to_owned(),
            port: 45550,
            caps: caps(),
            sessions: Vec::new(),
        });
        let _lease =
            slopty_net::server::connect(&endpoint, &HostAddr::from(server.quic_addr()), role)
                .await
                .unwrap();
        let mcp = server.mcp_addr();

        let listed = rpc(mcp, 1, "tools/list", None, json!({})).await;
        let names: Vec<&str> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "list_workers",
                "list_terminals",
                "open_terminal",
                "spawn_agent",
                "send_input",
                "read_screen",
                "read_output",
                "list_commands",
                "wait_for",
                "agent_status",
                "close_terminal",
                "read_file",
                "write_file",
                "list_ports",
            ]
        );
        let wait = &listed["result"]["tools"][8];
        assert_eq!(wait["inputSchema"]["required"], json!(["term"]));
        let cap = slopty_server::WAIT_CAP_MS.to_string();
        assert!(wait["description"].as_str().unwrap().contains(&cap), "the cap it names is ours");

        let params = json!({ "name": "list_workers", "arguments": {} });
        let called = rpc(mcp, 2, "tools/call", Some("list_workers"), params).await;
        let result = &called["result"];
        assert_ne!(result["isError"], json!(true), "{called}");
        let text = result["content"][0]["text"].as_str().unwrap();
        let workers: Value = serde_json::from_str(text).unwrap();
        assert_eq!(workers[0]["worker"], json!(worker.to_string()));
        assert_eq!(workers[0]["name"], json!("fake-worker"));
        assert_eq!(workers[0]["liveness"], json!("online"), "the tools' view, not the wire enum");
        assert!(!text.contains('\n'), "compact: {text}");

        // A verb for a worker nobody knows is a tool error the model can read.
        let params = json!({
            "name": "read_screen",
            "arguments": { "term": format!("{}/{}", WorkerId::new(), WorkerId::new()) },
        });
        let called = rpc(mcp, 3, "tools/call", Some("read_screen"), params).await;
        assert_eq!(called["result"]["isError"], json!(true), "{called}");
        assert!(called["result"]["content"][0]["text"].as_str().unwrap().contains("UnknownWorker"));

        // A browser page (anything that sends Origin) is refused.
        let (status, _body) = post(
            mcp,
            &message(4, "tools/list", json!({})),
            &[
                ("MCP-Protocol-Version", REVISION),
                ("Mcp-Method", "tools/list"),
                ("Origin", "http://evil.example"),
            ],
        )
        .await;
        assert_eq!(status, 403);
        server.shutdown().await;
    }

    /// A `write_file` of more bytes than one message to a worker carries gets through HTTP (the
    /// body limit holds a whole message's worth in base64, as stdio does) and is refused with an
    /// error a model can read. The worker's link stays up: nothing of the file went down it, and
    /// the next verb reaches the worker.
    #[tokio::test]
    async fn a_file_too_large_for_the_link_is_refused_and_the_link_stays_up() {
        use slopty_proto::codec::MAX_FRAME_BYTES;
        use slopty_proto::orchestration::{Outcome, Screen, Verb};
        use slopty_proto::server::{FromServer, ToServer};

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
        let role = Role::Worker(Registration {
            worker,
            name: "fake-worker".to_owned(),
            port: 45550,
            caps: caps(),
            sessions: Vec::new(),
        });
        let mut link =
            slopty_net::server::connect(&endpoint, &HostAddr::from(server.quic_addr()), role)
                .await
                .unwrap();
        let mcp = server.mcp_addr();

        // A frame's worth of zero bytes, a multiple of three so the base64 has no padding.
        let bytes = MAX_FRAME_BYTES - 1;
        assert_eq!(bytes % 3, 0);
        let content = "A".repeat(bytes / 3 * 4);
        let arguments = json!({
            "worker": worker.to_string(),
            "path": "/tmp/too-large",
            "content": content,
            "encoding": "base64",
        });
        let params = json!({ "name": "write_file", "arguments": arguments });
        let called = rpc(mcp, 1, "tools/call", Some("write_file"), params).await;
        let text = called["result"]["content"][0]["text"].as_str().unwrap();
        assert_eq!(called["result"]["isError"], json!(true), "{text}");
        assert!(text.contains("more than the"), "{text}");

        let term = format!("{worker}/{}", WorkerId::new());
        let params = json!({ "name": "read_screen", "arguments": { "term": term } });
        let worker_side = async {
            let msg = tokio::time::timeout(Duration::from_secs(10), link.rx.recv()).await;
            let Ok(Ok(FromServer::Request { id, verb: Verb::ReadScreen { .. } })) = msg else {
                panic!("the next request down the link is the read: {msg:?}")
            };
            let screen = Screen {
                lines: Vec::new(),
                cursor: (0, 0),
                title: String::new(),
                cwd: None,
                alternate: false,
            };
            link.tx.send(&ToServer::Reply { id, outcome: Outcome::Screen(screen) }).await.unwrap();
        };
        let (called, ()) =
            tokio::join!(rpc(mcp, 2, "tools/call", Some("read_screen"), params), worker_side);
        assert_ne!(called["result"]["isError"], json!(true), "{called}");
        server.shutdown().await;
    }

    /// This machine's link-local address on `lo0` is not loopback, so a listener that admits
    /// only 10/8 turns it away; loopback is let in whatever the list says.
    #[tokio::test]
    async fn a_peer_outside_the_admitted_ranges_gets_no_answer() {
        let listener = slopty_server::mcp::bind("[::]:0".parse().unwrap()).unwrap();
        let port = listener.local_addr().unwrap().port();
        let admission = Admission::new(vec!["10.0.0.0/8".parse().unwrap()]);
        let serving = tokio::spawn(slopty_server::mcp::serve(
            listener,
            admission,
            Hub::new("test".to_owned(), Vec::new()),
        ));
        let outside: SocketAddr = format!("[fe80::1%1]:{port}").parse().unwrap();
        let mut stream = tokio::net::TcpStream::connect(outside).await.unwrap();
        let _sent = stream.write_all(b"POST /mcp HTTP/1.1\r\nHost: x\r\n\r\n").await;
        let mut response = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut response))
            .await
            .unwrap();
        assert!(read.is_err() || response.is_empty(), "closed unanswered: {response:?}");

        let inside = SocketAddr::from(([127, 0, 0, 1], port));
        let listed = rpc(inside, 1, "tools/list", None, json!({})).await;
        assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 14);
        serving.abort();
    }
}
