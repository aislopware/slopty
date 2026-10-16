//! What an agent shows in its thread beyond words and pictures.
//!
//! A native visual (a Luau script the client runs and draws), a page (HTML, a file, or an MCP App's
//! UI), the links and structured content of an MCP result, and the relay a page uses to read its
//! app's resource and call its app's tools through the agent that holds the connection.
//!
//! - **Visuals and pages are tool calls.** Slopty's own `show_visual` and `show_page` (its MCP
//!   server, `slopty mcp`) are calls in the agent's own record with their whole input, so the
//!   worker's decoder turns them into a [`VisualDetail`] or a [`PageDetail`] and nothing is stored
//!   twice. An MCP App is a call whose tool names a `ui://` resource ([`McpApp`]).
//! - **Paths are the worker's.** A visual's assets and a page's file are absolute paths on the
//!   worker, read through the worker link like any file.
//! - **The relay never dials a server.** A page's `resources/read` and `tools/call` go to the
//!   worker as an [`AppAsk`] on the thread, and the worker passes them through the agent's own door
//!   (Codex's `mcpServer/resource/read` and `mcpServer/tool/call`, Claude Code's
//!   `mcp_read_resource` and `mcp_call`); the answer comes back as an [`AppAnswer`]
//!   (`docs/decisions/agents.md`, "MCP Apps through the agent's doors").

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::thread::Clipped;

/// The tool, on Slopty's MCP server, that shows a native visual.
pub const SHOW_VISUAL: &str = "show_visual";
/// The tool, on Slopty's MCP server, that shows a page.
pub const SHOW_PAGE: &str = "show_page";

/// The MIME type of an MCP App's UI resource (SEP-1865).
pub const APP_MIME: &str = "text/html;profile=mcp-app";
/// The MCP extension an MCP Apps host declares (SEP-1865).
pub const APP_EXTENSION: &str = "io.modelcontextprotocol/ui";

/// A native visual: a Luau script the client runs in its sandbox and draws with its own
/// elements (`slopty_visual`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisualDetail {
    /// Its short name.
    pub title: String,
    /// The script, as the agent wrote it; its whole by `full` when clipped.
    pub script: Clipped,
    /// The files it uses, by the name the script gives each, to its absolute path on the worker.
    pub assets: BTreeMap<String, String>,
}

/// A page shown inline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageDetail {
    /// Its short name.
    pub title: String,
    /// Where it comes from.
    pub source: PageSource,
}

/// Where a page comes from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageSource {
    /// HTML (or SVG) the agent wrote into the call; its whole by `full` when clipped.
    Html(Clipped),
    /// A file on the worker, by its absolute path; its relative sub-resources are under its
    /// folder.
    File(String),
    /// An MCP App's UI, read through the agent ([`AppAsk::Read`]).
    App(McpApp),
}

/// An MCP App: a tool whose descriptor names a UI resource.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpApp {
    /// The MCP server, by the agent's name for it.
    pub server: String,
    /// The tool called.
    pub tool: String,
    /// The UI resource, a `ui://` URI.
    pub resource_uri: String,
    /// How the tool prefers to be shown (`inline`, `fullscreen`, `pip`), when it says.
    pub display_mode: Option<String>,
    /// The call's result as MCP spells it (`content`, `structuredContent`, `_meta`), as JSON,
    /// for the page's `ui/notifications/tool-result`; `None` until the call ends.
    pub result: Option<Clipped>,
}

/// A link to a resource an MCP result returned (`resource_link`), or a resource it embedded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLink {
    /// The resource's URI.
    pub uri: String,
    /// Its name.
    pub name: String,
    /// A title for people, when it has one.
    pub title: Option<String>,
    /// Its MIME type, when said.
    pub mime_type: Option<String>,
}

/// What a page asks of its app, relayed by the worker through the agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppAsk {
    /// Read a UI resource (`resources/read`); only `ui://` URIs are read.
    Read {
        /// The MCP server.
        server: String,
        /// The resource.
        uri: String,
    },
    /// Call one of the app's tools (`tools/call`), its result not handed to the model.
    Call {
        /// The MCP server.
        server: String,
        /// The tool.
        tool: String,
        /// Its arguments, a JSON object.
        arguments: String,
    },
}

/// What the agent answered an [`AppAsk`] with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppAnswer {
    /// The resource read.
    Resource {
        /// Its URI.
        uri: String,
        /// Its MIME type, when said ([`APP_MIME`] for an app's UI).
        mime_type: Option<String>,
        /// Its content.
        body: AppBody,
        /// Its `_meta`, as JSON (`ui.csp`, `ui.permissions`, `ui.domain`, `ui.prefersBorder`);
        /// empty when it has none.
        meta: String,
    },
    /// The tool's result: the server's `CallToolResult` as JSON, its `content`,
    /// `structuredContent`, `_meta` and `isError` unfiltered.
    Called {
        /// The result.
        result: String,
    },
    /// The agent would not, or could not: why, in words.
    Refused(String),
}

/// A resource's content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppBody {
    /// Text, as most UI resources are.
    Text(String),
    /// Bytes (a `blob`, decoded).
    Bytes(#[serde(with = "serde_bytes")] Vec<u8>),
}

/// Whether `uri` names an MCP App's UI resource: the only kind a page's read is relayed for.
#[must_use]
pub fn is_ui_uri(uri: &str) -> bool {
    uri.strip_prefix("ui://").is_some_and(|rest| !rest.is_empty())
}
