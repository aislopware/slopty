//! A stand-in MCP server with an MCP App (SEP-1865, `io.modelcontextprotocol/ui`), for tests
//! and recordings that show an app's page through an agent with no third party's server.
//!
//! It speaks MCP over stdio, one JSON-RPC message a line, as an agent starts its MCP servers:
//!
//! - **`echo {text}`**, the app's tool, for the model and the app alike: its result is a text
//!   block, a 1 × 1 PNG, a `resource_link`, and `structuredContent` `{"echo": text, "hostUi":
//!   <whether the client declared the UI extension>}`.
//! - **`refresh {}`**, read-only, for the app alone (`visibility: ["app"]`): a host must keep it
//!   off the model's list and call it only for the page. Its result counts its calls.
//! - **`ui://apps/echo`**, the app's page, `text/html;profile=mcp-app`, with its `_meta.ui`.
//!
//! A client that does not declare the UI extension at `initialize` is a host without UI, as the
//! spec has it: the tools carry no `_meta.ui` and the app-only tool is not listed.
//!
//! `--log <path>` appends every message it is sent as one JSON line, so a test counts what
//! reached the server (a call refused before it should never show there).

#![allow(clippy::print_stdout, clippy::print_stderr, reason = "stdout is its MCP; stderr its log")]

use std::io::{BufRead as _, Write as _};
use std::process::ExitCode;

use serde_json::{Value, json};

/// The UI extension's id.
const EXTENSION: &str = "io.modelcontextprotocol/ui";
/// An MCP App page's MIME type.
const APP_MIME: &str = "text/html;profile=mcp-app";
/// The app's page.
const PAGE: &str = "ui://apps/echo";
/// What the page is.
const HTML: &str = "<!doctype html><html><body><p id=\"echo\">echo</p></body></html>";
/// A 1 × 1 PNG.
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
/// JSON-RPC's code for a method not found.
const NO_METHOD: i64 = -32_601;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let log = args.iter().position(|a| a == "--log").and_then(|at| args.get(at.saturating_add(1)));
    let mut log =
        log.and_then(|path| std::fs::OpenOptions::new().create(true).append(true).open(path).ok());
    let mut host_ui = false;
    let mut refreshed = 0_u64;
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        if let Some(log) = log.as_mut() {
            let _logged = writeln!(log, "{msg}");
        }
        let Some(id) = msg.get("id").cloned() else { continue };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let answer = match method {
            "initialize" => {
                host_ui = params
                    .pointer("/capabilities/extensions")
                    .and_then(|e| e.get(EXTENSION))
                    .is_some();
                Ok(json!({
                    "protocolVersion": params.get("protocolVersion").cloned()
                        .unwrap_or_else(|| json!("2025-06-18")),
                    "capabilities": { "tools": {}, "resources": {} },
                    "serverInfo": { "name": "slopty-stub-mcp-apps", "version": "1.0.0" },
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools(host_ui) })),
            "resources/list" => Ok(json!({ "resources": [{
                "uri": PAGE, "name": "echo", "mimeType": APP_MIME,
            }] })),
            "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
            "resources/read" => match params.get("uri").and_then(Value::as_str) {
                Some(PAGE) => Ok(json!({ "contents": [{
                    "uri": PAGE,
                    "mimeType": APP_MIME,
                    "text": HTML,
                    "_meta": { "ui": {
                        "csp": { "connectDomains": [], "resourceDomains": [] },
                        "prefersBorder": true,
                    } },
                }] })),
                other => Err(format!("no resource {other:?}")),
            },
            "tools/call" => {
                let tool = params.get("name").and_then(Value::as_str).unwrap_or_default();
                let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
                match tool {
                    "echo" => {
                        let text = arguments.get("text").and_then(Value::as_str).unwrap_or("");
                        Ok(json!({
                            "content": [
                                { "type": "text", "text": format!("echo: {text}") },
                                { "type": "image", "data": PNG, "mimeType": "image/png" },
                                { "type": "resource_link", "uri": "file:///echo.txt",
                                  "name": "echo.txt", "mimeType": "text/plain" },
                            ],
                            "structuredContent": { "echo": text, "hostUi": host_ui },
                        }))
                    }
                    "refresh" => {
                        refreshed = refreshed.saturating_add(1);
                        Ok(json!({
                            "content": [{ "type": "text", "text": format!("refreshed {refreshed}") }],
                            "structuredContent": { "count": refreshed },
                        }))
                    }
                    other => Err(format!("no tool {other:?}")),
                }
            }
            other => Err(format!("no method {other:?}")),
        };
        let reply = match answer {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(message) => json!({
                "jsonrpc": "2.0", "id": id, "error": { "code": NO_METHOD, "message": message },
            }),
        };
        if writeln!(out, "{reply}").and_then(|()| out.flush()).is_err() {
            eprintln!("stub mcp apps: stdout closed");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

/// The tools a client sees: with their UI only for a host that declared it.
fn tools(host_ui: bool) -> Value {
    let echo_schema = json!({
        "type": "object",
        "properties": { "text": { "type": "string" } },
        "required": ["text"],
    });
    let empty = json!({ "type": "object", "properties": {} });
    if !host_ui {
        return json!([{ "name": "echo", "description": "Echo the text.", "inputSchema": echo_schema }]);
    }
    json!([
        {
            "name": "echo",
            "description": "Echo the text, and show it in the echo app.",
            "inputSchema": echo_schema,
            "_meta": { "ui": { "resourceUri": PAGE, "visibility": ["model", "app"] } },
        },
        {
            "name": "refresh",
            "description": "Refresh the echo app.",
            "inputSchema": empty,
            "annotations": { "readOnlyHint": true },
            "_meta": { "ui": { "resourceUri": PAGE, "visibility": ["app"] } },
        },
    ])
}
