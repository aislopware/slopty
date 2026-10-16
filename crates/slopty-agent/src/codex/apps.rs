//! MCP Apps through Codex.
//!
//! The host Slopty declares itself at the handshake, and the reads and calls a page makes of its
//! app, through Codex's own `mcpServer/resource/read` and `mcpServer/tool/call`. Slopty never
//! dials an MCP server itself; Codex holds the connection.
//!
//! Sans-IO: the params that go and the answer that comes back, as [`AppAsk`] and [`AppAnswer`].

use std::collections::BTreeMap;

use serde_json::{Value, json};
use slopty_proto::visual::{APP_EXTENSION, APP_MIME, AppAnswer, AppAsk, AppBody};

use super::protocol as p;

/// What Slopty says it can do when it joins the app-server.
///
/// The experimental API (the methods it speaks are experimental), and the MCP Apps extension,
/// which Codex passes on to the MCP servers so they offer their apps' UI.
#[must_use]
pub fn capabilities() -> p::InitializeCapabilities {
    let mut extensions = BTreeMap::new();
    extensions.insert(APP_EXTENSION.to_owned(), json!({ "mimeTypes": [APP_MIME] }));
    p::InitializeCapabilities {
        experimental_api: Some(true),
        extensions: Some(extensions),
        ..p::InitializeCapabilities::default()
    }
}

/// A page's ask, as the request that goes to Codex.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// `mcpServer/resource/read`.
    Read(p::McpResourceReadParams),
    /// `mcpServer/tool/call`.
    Call(p::McpServerToolCallParams),
}

/// The request for `ask` in Codex thread `native`.
///
/// # Errors
///
/// When the read is of anything but an app's page (`ui://`), or the call's arguments are not a
/// JSON object.
pub fn request(native: &str, ask: AppAsk) -> Result<Request, String> {
    match ask {
        AppAsk::Read { server, uri } => {
            if !slopty_proto::visual::is_ui_uri(&uri) {
                return Err(format!("{uri} is no app's page (ui://)"));
            }
            Ok(Request::Read(p::McpResourceReadParams {
                server,
                uri,
                thread_id: Some(native.to_owned()),
                ..p::McpResourceReadParams::default()
            }))
        }
        AppAsk::Call { server, tool, arguments } => {
            let arguments = match serde_json::from_str::<Value>(&arguments) {
                Ok(Value::Object(object)) => Value::Object(object),
                Ok(_) => return Err("a tool's arguments are a JSON object".to_owned()),
                Err(e) => return Err(format!("a tool's arguments did not read: {e}")),
            };
            Ok(Request::Call(p::McpServerToolCallParams {
                server,
                tool,
                thread_id: native.to_owned(),
                arguments: Some(arguments),
                meta: None,
            }))
        }
    }
}

/// Codex's answer to a read of `uri`: the content for `uri`, else its first.
#[must_use]
pub fn read(uri: &str, said: &p::McpResourceReadResponse) -> AppAnswer {
    let found =
        said.contents.iter().find(|c| content_uri(c) == uri).or_else(|| said.contents.first());
    let Some(content) = found else {
        return AppAnswer::Refused(format!("{uri} came back empty"));
    };
    let meta = |meta: Option<&Value>| meta.map(Value::to_string).unwrap_or_default();
    match content {
        p::ResourceContent::Variant0(text) => AppAnswer::Resource {
            uri: text.uri.clone(),
            mime_type: text.mime_type.clone(),
            body: AppBody::Text(text.text.clone()),
            meta: meta(text.meta.as_ref()),
        },
        p::ResourceContent::Variant1(blob) => match crate::media::decode(&blob.blob) {
            Some(bytes) => AppAnswer::Resource {
                uri: blob.uri.clone(),
                mime_type: blob.mime_type.clone(),
                body: AppBody::Bytes(bytes),
                meta: meta(blob.meta.as_ref()),
            },
            None => AppAnswer::Refused(format!("{uri} came back as a blob that is not base64")),
        },
    }
}

fn content_uri(content: &p::ResourceContent) -> &str {
    match content {
        p::ResourceContent::Variant0(text) => &text.uri,
        p::ResourceContent::Variant1(blob) => &blob.uri,
    }
}

/// Codex's answer to a call: the server's result as MCP spells it, unfiltered.
#[must_use]
pub fn called(said: p::McpServerToolCallResponse) -> AppAnswer {
    let mut result = serde_json::Map::new();
    result.insert("content".to_owned(), Value::Array(said.content));
    if let Some(structured) = said.structured_content {
        result.insert("structuredContent".to_owned(), structured);
    }
    if let Some(meta) = said.meta {
        result.insert("_meta".to_owned(), meta);
    }
    if let Some(is_error) = said.is_error {
        result.insert("isError".to_owned(), Value::Bool(is_error));
    }
    AppAnswer::Called { result: Value::Object(result).to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Slopty declares itself an MCP Apps host, for HTML apps.
    #[test]
    fn the_handshake_declares_the_mcp_apps_extension() {
        let said = serde_json::to_value(capabilities()).unwrap();
        assert_eq!(said["extensions"][APP_EXTENSION]["mimeTypes"], json!([APP_MIME]));
        assert_eq!(said["experimentalApi"], json!(true));
    }

    /// A read goes for a `ui://` page alone, in the thread; a call's arguments are an object.
    #[test]
    fn a_page_reads_its_ui_and_calls_with_an_object() {
        let read = AppAsk::Read { server: "apps".to_owned(), uri: "ui://apps/echo".to_owned() };
        let Ok(Request::Read(params)) = request("thread-1", read) else { panic!() };
        assert_eq!(
            (params.thread_id.as_deref(), params.uri.as_str()),
            (Some("thread-1"), "ui://apps/echo")
        );
        let file = AppAsk::Read { server: "apps".to_owned(), uri: "file:///etc/passwd".to_owned() };
        assert!(request("thread-1", file).is_err(), "no file read through a page");
        let call = |arguments: &str| AppAsk::Call {
            server: "apps".to_owned(),
            tool: "echo".to_owned(),
            arguments: arguments.to_owned(),
        };
        let Ok(Request::Call(params)) = request("thread-1", call(r#"{"text":"hi"}"#)) else {
            panic!()
        };
        assert_eq!(params.arguments, Some(json!({ "text": "hi" })));
        request("thread-1", call("[1]")).unwrap_err();
        request("thread-1", call("{")).unwrap_err();
    }

    /// A read's answer is its content, text or bytes, with its `_meta`; a call's is MCP's
    /// result unfiltered.
    #[test]
    fn answers_are_the_servers_own() {
        let said: p::McpResourceReadResponse = serde_json::from_value(json!({
            "contents": [
                { "uri": "ui://apps/other", "text": "no" },
                { "uri": "ui://apps/echo", "mimeType": APP_MIME, "text": "<p>hi</p>",
                  "_meta": { "ui": { "prefersBorder": true } } },
            ],
        }))
        .unwrap();
        let AppAnswer::Resource { uri, mime_type, body, meta } = read("ui://apps/echo", &said)
        else {
            panic!()
        };
        assert_eq!((uri.as_str(), mime_type.as_deref()), ("ui://apps/echo", Some(APP_MIME)));
        assert_eq!(body, AppBody::Text("<p>hi</p>".to_owned()));
        assert_eq!(
            serde_json::from_str::<Value>(&meta).unwrap()["ui"]["prefersBorder"],
            json!(true)
        );
        let blob: p::McpResourceReadResponse = serde_json::from_value(json!({
            "contents": [{ "uri": "ui://apps/echo", "blob": "PHA+aGk8L3A+" }],
        }))
        .unwrap();
        let AppAnswer::Resource { body, .. } = read("ui://apps/echo", &blob) else { panic!() };
        assert_eq!(body, AppBody::Bytes(b"<p>hi</p>".to_vec()));
        let empty = p::McpResourceReadResponse::default();
        assert!(matches!(read("ui://apps/echo", &empty), AppAnswer::Refused(_)));

        let result: p::McpServerToolCallResponse = serde_json::from_value(json!({
            "content": [{ "type": "text", "text": "hi" }],
            "structuredContent": { "echo": "hi" },
            "isError": false,
        }))
        .unwrap();
        let AppAnswer::Called { result } = called(result) else { panic!() };
        let result: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["structuredContent"]["echo"], json!("hi"));
        assert_eq!(result["content"][0]["text"], json!("hi"));
    }
}
