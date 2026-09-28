//! The tools as an MCP server over any [`Dispatch`]: the one handler the server's endpoint and
//! `slopty mcp` both serve, so they differ only in what carries the verbs and how each names
//! itself.

use std::borrow::Cow;
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, Implementation, ListToolsResult,
    PaginatedRequestParams, ProgressNotificationParam, ProtocolVersion, ServerCapabilities,
    ServerConfig, Tool,
};
use rmcp::service::{MaybeSendFuture, RequestContext};
use rmcp::{ErrorData, RoleServer, ServerHandler};

use crate::{Dispatch, tools};

/// The newest protocol revision served. Older ones are still negotiated for a client that asks
/// for one in `initialize`.
pub const REVISION: ProtocolVersion = ProtocolVersion::V_2026_07_28;

/// [`tools::list`] and [`tools::call`] over `D`.
#[derive(Clone, Debug)]
pub struct Handler<D> {
    dispatch: D,
    info: ServerConfig,
    progress: bool,
}

impl<D> Handler<D> {
    /// The tools over `dispatch`, introduced as `server_info` with `capabilities`, which enable
    /// the tools.
    #[must_use]
    pub fn new(dispatch: D, server_info: Implementation, capabilities: ServerCapabilities) -> Self {
        let info = ServerConfig::new(capabilities)
            .with_protocol_version(REVISION)
            .with_server_info(server_info)
            .with_instructions(tools::INSTRUCTIONS);
        Self { dispatch, info, progress: false }
    }

    /// Tell a caller that sent a progress token how long a `wait_for` has waited, every
    /// [`tools::PROGRESS_EVERY`]. Only a transport that keeps a stream open to the caller has
    /// anywhere to send it.
    #[must_use]
    pub const fn with_progress(mut self) -> Self {
        self.progress = true;
        self
    }
}

impl<D: Dispatch + 'static> ServerHandler for Handler<D> {
    fn get_info(&self) -> ServerConfig {
        self.info.clone()
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(ProtocolVersion::known_up_to(&REVISION))
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(tools::list())))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools::get(name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let token = if self.progress { context.meta.get_progress_token() } else { None };
        let report = token.map(|token| {
            let peer = context.peer.clone();
            move |waited: Duration| {
                let waited = waited.as_secs_f64();
                let note = ProgressNotificationParam::new(token.clone(), waited)
                    .with_message(format!("waited {waited:.0} s"));
                let peer = peer.clone();
                tokio::spawn(async move {
                    if let Err(e) = peer.notify_progress(note).await {
                        tracing::debug!(error = %e, "progress");
                    }
                });
            }
        });
        let progress = report.as_ref().map(|r| -> tools::Progress<'_> { r });
        let arguments = request.arguments.unwrap_or_default();
        tools::call(&self.dispatch, &request.name, arguments, progress).await.map(Into::into)
    }
}
