//! The MCP endpoint over real HTTP on loopback: every tool is listed, a call runs through the same
//! dispatch as the QUIC links, and a browser's request or a peer outside the admitted ranges is
//! turned away.

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    use std::net::SocketAddr;
    use std::time::Duration;

    use serde_json::{Value, json};
    use slopty_net::admission::{Admission, on_tailnet};
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

    #[tokio::test]
    async fn every_tool_is_listed_and_a_call_runs_the_verb() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(Config {
            name: "test-server".to_owned(),
            quic: "127.0.0.1:0".parse().unwrap(),
            mcp: "127.0.0.1:0".parse().unwrap(),
            data_dir: dir.path().to_path_buf(),
            admission: Admission::with_tailnet(Vec::new(), None),
        })
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
        let table: Vec<String> =
            slopty_tools::tools::list().into_iter().map(|t| t.name.into_owned()).collect();
        assert_eq!(names, table, "every tool in the table, in its order");
        let status = &listed["result"]["tools"][0];
        assert_eq!(status["name"], json!("project_status"));
        let timeout = status["inputSchema"]["properties"]["timeout_ms"]["description"].as_str();
        let cap = slopty_server::WAIT_CAP_MS.to_string();
        assert!(timeout.unwrap().contains(&cap), "the cap it names is ours");

        // The hub answers the verb; a project nobody made is a tool error the model can read.
        let params = json!({ "name": "project_status", "arguments": { "project": "nope" } });
        let called = rpc(mcp, 2, "tools/call", Some("project_status"), params).await;
        let result = &called["result"];
        assert_eq!(result["isError"], json!(true), "{called}");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("UnknownProject") && !text.contains('\n'), "{text}");

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

    /// `port` on an address of this machine's that is not loopback, so a peer reaching it from
    /// here comes from it: the link-local address macOS gives `lo0`, or on Linux, whose `lo` has
    /// none, the one it sends from on its default route (nothing leaves the machine).
    fn not_loopback(port: u16) -> SocketAddr {
        if cfg!(target_os = "macos") {
            return format!("[fe80::1%1]:{port}").parse().unwrap();
        }
        let probe = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        probe.connect("192.0.2.1:9").expect("a default route");
        let ip = probe.local_addr().unwrap().ip();
        assert!(!ip.is_loopback() && !on_tailnet(ip), "{ip} is let in whatever the ranges");
        SocketAddr::new(ip, port)
    }

    /// This machine at an address that is not loopback is turned away by a listener that admits
    /// only 192.0.2.0/24, a range no host is given (RFC 5737; not 10/8, where a runner's own
    /// address can be); loopback is let in whatever the list says.
    #[tokio::test]
    async fn a_peer_outside_the_admitted_ranges_gets_no_answer() {
        let listener = slopty_server::mcp::bind("[::]:0".parse().unwrap()).unwrap();
        let port = listener.local_addr().unwrap().port();
        let admission = Admission::with_tailnet(vec!["192.0.2.0/24".parse().unwrap()], None);
        let serving = tokio::spawn(slopty_server::mcp::serve(
            listener,
            admission,
            Hub::new("test".to_owned(), Vec::new()),
        ));
        let mut stream = tokio::net::TcpStream::connect(not_loopback(port)).await.unwrap();
        let _sent = stream.write_all(b"POST /mcp HTTP/1.1\r\nHost: x\r\n\r\n").await;
        let mut response = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut response))
            .await
            .unwrap();
        assert!(read.is_err() || response.is_empty(), "closed unanswered: {response:?}");

        // Over `::1`, not `127.0.0.1`: the listener is the IPv6 wildcard, and with address reuse
        // another test's listener bound to `127.0.0.1` on the same port would answer instead.
        let inside = SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port));
        let listed = rpc(inside, 1, "tools/list", None, json!({})).await;
        assert_eq!(
            listed["result"]["tools"].as_array().unwrap().len(),
            slopty_tools::tools::list().len()
        );
        serving.abort();
    }
}
