//! The MCP front end: Streamable HTTP (protocol revision 2026-07-28, stateless), one tool per
//! [`slopty_proto::orchestration::Verb`], dispatched through the same [`Hub::dispatch`] as the
//! QUIC links.
//!
//! The listener admits TCP peers by address like the QUIC one. It does not check `Host`,
//! because a tailnet name or an IP literal is as legitimate as `localhost`; instead it refuses
//! every request carrying an `Origin`, which only a browser sends, and which is what a
//! DNS-rebinding page cannot leave out.

mod tools;

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{MaybeSendFuture, RequestContext};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use slopty_net::admission::Admission;
use tokio::net::TcpListener;
use tokio::task::JoinSet;

use crate::hub::Hub;

/// What the server tells a model about itself.
const INSTRUCTIONS: &str = "Slopty drives terminals on the user's machines (workers). \
Start with list_workers; every other tool takes a worker id from it, and terminal tools also \
take a session id (from list_terminals, open_terminal or spawn_agent). Handles are plain UUID \
strings and stay valid until the terminal closes. To run a command: send_input with text \
ending in \\n, then wait_for (until command_done or output), then read_output from the line \
list_commands or a previous read_output's `next` gave. Prefer wait_for to polling read_screen.";

/// The MCP tool server.
#[derive(Clone, Debug)]
pub struct Mcp {
    hub: Hub,
}

impl Mcp {
    /// Tools over `hub`.
    #[must_use]
    pub const fn new(hub: Hub) -> Self {
        Self { hub }
    }

    /// Every tool, one per verb.
    #[must_use]
    pub fn tools() -> Vec<Tool> {
        tools::all()
    }

    /// Run the tool `name` with `arguments` and render its outcome as compact JSON text.
    pub async fn call(
        &self,
        name: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<CallToolResult, ErrorData> {
        let verb = match tools::verb(name, arguments) {
            Ok(verb) => verb,
            Err(tools::BadCall::UnknownTool) => {
                return Err(ErrorData::invalid_params(format!("no tool named {name}"), None));
            }
            Err(tools::BadCall::Arguments(why)) => {
                let text = serde_json::json!({ "error": "Invalid", "message": why }).to_string();
                return Ok(CallToolResult::error(vec![ContentBlock::text(text)]));
            }
        };
        let outcome = self.hub.dispatch(verb).await;
        let (text, failed) = tools::render(outcome);
        let content = vec![ContentBlock::text(text)];
        Ok(if failed { CallToolResult::error(content) } else { CallToolResult::success(content) })
    }
}

impl ServerHandler for Mcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("slopty-server", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(Self::tools())))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        Self::tools().into_iter().find(|t| t.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let arguments = request.arguments.unwrap_or_default();
        self.call(&request.name, arguments).await.map(CallToolResponse::from)
    }
}

/// Serve MCP on `listener` to the peers `admission` admits. Connections run in a set this
/// future owns, so dropping it ends them all.
pub async fn serve(listener: TcpListener, admission: Admission, hub: Hub) {
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None)
        .disable_allowed_hosts()
        .enforce_origin_validation();
    let service = StreamableHttpService::new(
        move || Ok(Mcp::new(hub.clone())),
        Arc::new(NeverSessionManager::default()),
        config,
    );
    let mut connections = JoinSet::new();
    loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            Some(_done) = connections.join_next() => continue,
        };
        let (stream, peer) = match accepted {
            Ok(accepted) => accepted,
            Err(e) => {
                tracing::warn!(error = %e, "mcp accept failed");
                continue;
            }
        };
        let peer = slopty_net::endpoint::canonical(peer);
        if !admission.admits(peer.ip()) {
            tracing::info!(%peer, "mcp refused: outside the admitted ranges");
            continue;
        }
        let service = service.clone();
        connections.spawn(async move {
            let serve = service_fn(move |request| {
                let service = service.clone();
                async move { Ok::<_, Infallible>(service.handle(request).await) }
            });
            if let Err(e) =
                http1::Builder::new().serve_connection(TokioIo::new(stream), serve).await
            {
                tracing::debug!(%peer, error = %e, "mcp connection ended");
            }
        });
    }
}

/// Bind the MCP listener on `local`.
///
/// An unspecified IPv6 address is bound dual-stack, so one socket answers loopback, the tailnet
/// and the LAN in both families, including interfaces that come up after the server does.
pub fn bind(local: SocketAddr) -> std::io::Result<TcpListener> {
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(local),
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    if local.is_ipv6() {
        socket.set_only_v6(false)?;
    }
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&local.into())?;
    socket.listen(128)?;
    TcpListener::from_std(socket.into())
}
