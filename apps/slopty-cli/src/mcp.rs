//! `slopty mcp`: the verbs as an MCP server on stdio, for an AI agent such as Claude Code.
//!
//! The tools are [`slopty_tools::tools`], the ones the server's own endpoint serves. Each call
//! becomes one verb (after any name lookups) on a link to the server held for the process
//! lifetime as `Role::Agent`, redialled when it drops. Answers are the same JSON as
//! `slopty … --json`; a `wait_for` that carries a progress token reports every 10 s. An agent that
//! comes to need a human anywhere on the fleet is announced as a `notifications/message`, so the
//! orchestrating session hears of it without polling.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, Implementation, ListToolsResult,
    PaginatedRequestParams, ProgressNotificationParam, ProtocolVersion, ServerCapabilities,
    ServerConfig, Tool,
};
use rmcp::service::{MaybeSendFuture, RequestContext};
use rmcp::{ErrorData, Peer, RoleServer, ServerHandler, ServiceExt as _};
use serde_json::{Value, json};
use slopty_core::WorkerId;
use slopty_net::client::bind_client;
use slopty_proto::agent::{AgentEvent, AgentStatus, BlockReason, SessionAgent};
use slopty_proto::orchestration::TermRef;
use slopty_proto::server::{Event, FromServer, Role};
use slopty_tools::{tools, view};
use tokio::sync::broadcast;

use crate::link::{self, Link};

/// `slopty mcp --help` epilogue.
pub const REGISTER_HELP: &str = "\
Register Slopty with Claude Code:

  claude mcp add slopty -- slopty mcp

The server is --server, else $SLOPTY_SERVER, else `server` under [client] in settings.toml. \
To pin one: claude mcp add slopty -- slopty mcp --server studio";

/// The protocol revision this shim speaks; older ones are still negotiated for clients that
/// ask for them in `initialize`.
const REVISION: ProtocolVersion = ProtocolVersion::V_2026_07_28;

/// Serve MCP on stdio until the client hangs up.
pub async fn run(server: Option<&str>, data_dir: &Path) -> Result<()> {
    let address = link::locate(server, data_dir)?;
    let endpoint = bind_client()?;
    let role = Role::Agent { name: format!("slopty mcp @ {}", crate::client::machine_name()) };
    let (link, events) = Link::persistent(endpoint.clone(), address, role);
    let running = Slopty { link }
        .serve(rmcp::transport::stdio())
        .await
        .context("the MCP client did not open a session")?;
    let forward = tokio::spawn(forward_needs(events, running.peer().clone()));
    let quit = running.waiting().await;
    forward.abort();
    crate::client::close_endpoint(&endpoint).await;
    quit.context("MCP session")?;
    Ok(())
}

/// The MCP server: tools that forward to the link.
#[derive(Debug, Clone)]
struct Slopty {
    link: Link,
}

impl ServerHandler for Slopty {
    fn get_info(&self) -> ServerConfig {
        #[expect(
            deprecated,
            reason = "SEP-2577 deprecates logging, but a `notifications/message` is the one \
                      message a stdio client shows unprompted; Claude Code's channels are a \
                      preview on an older revision"
        )]
        let capabilities = ServerCapabilities::builder().enable_logging().enable_tools().build();
        ServerConfig::new(capabilities)
            .with_protocol_version(REVISION)
            .with_server_info(Implementation::new("slopty", env!("CARGO_PKG_VERSION")))
            .with_instructions(tools::INSTRUCTIONS)
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
        let report = context.meta.get_progress_token().map(|token| {
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
        tools::call(&self.link, &request.name, arguments, progress).await.map(Into::into)
    }
}

/// Announce each agent that comes to need a human, once per episode, until the client leaves.
async fn forward_needs(mut pushed: broadcast::Receiver<FromServer>, peer: Peer<RoleServer>) {
    let mut names: HashMap<WorkerId, String> = HashMap::new();
    let mut last: HashMap<TermRef, AgentStatus> = HashMap::new();
    loop {
        let msg = match pushed.recv().await {
            Ok(msg) => msg,
            Err(broadcast::error::RecvError::Lagged(missed)) => {
                tracing::warn!(missed, "server events dropped");
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => return,
        };
        match msg {
            FromServer::Directory(list) => {
                names = list.into_iter().map(|w| (w.worker, w.name)).collect();
            }
            FromServer::Worker(w) => {
                names.insert(w.worker, w.name);
            }
            FromServer::Event(Event::SessionClosed { worker, session }) => {
                last.remove(&TermRef { worker, session });
            }
            FromServer::Event(Event::Agent { worker, event }) => {
                let term = TermRef { worker, session: event.session };
                let before = last.insert(term, event.status.clone());
                let name = names.get(&worker).map(String::as_str);
                if let Some(note) = needs_human(term, name, &event, before.as_ref())
                    && let Err(e) = notify(&peer, note).await
                {
                    tracing::debug!(error = %e, "the MCP client is gone");
                    return;
                }
            }
            _other => {}
        }
    }
}

#[expect(deprecated, reason = "see `get_info`: logging is how a stdio client hears of it")]
async fn notify(peer: &Peer<RoleServer>, (level, data): (Level, Value)) -> Result<()> {
    use rmcp::model::{LoggingLevel, LoggingMessageNotificationParam};
    let level = match level {
        Level::Warning => LoggingLevel::Warning,
        Level::Notice => LoggingLevel::Notice,
    };
    let note = LoggingMessageNotificationParam::new(level, data).with_logger("slopty");
    peer.notify_logging_message(note).await?;
    Ok(())
}

/// How loudly to say it: a question or a permission blocks work, an idle prompt only waits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Level {
    Warning,
    Notice,
}

/// The note for an agent status that newly needs a human; `None` for one that does not, or
/// that already did the same way before.
fn needs_human(
    term: TermRef,
    worker_name: Option<&str>,
    event: &AgentEvent,
    before: Option<&AgentStatus>,
) -> Option<(Level, Value)> {
    let reason = view::blocked(&event.status)?;
    if before == Some(&event.status) {
        return None;
    }
    let level = match reason {
        BlockReason::IdlePrompt => Level::Notice,
        BlockReason::Permission { .. } | BlockReason::Question | BlockReason::Elicitation => {
            Level::Warning
        }
    };
    let reported = SessionAgent::from(event);
    let agent = view::agent(Some(&reported));
    let where_ = worker_name.map_or_else(|| term.worker.to_string(), str::to_owned);
    let what = view::agent_text(Some(&reported));
    let message = event.detail.as_ref().map_or_else(
        || format!("{what} (on {where_})"),
        |detail| format!("{what} (on {where_}): {detail}"),
    );
    Some((
        level,
        json!({
            "message": message,
            "term": view::term_string(term),
            "worker_name": worker_name,
            "agent": agent,
            "detail": event.detail,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use slopty_core::SessionId;
    use slopty_proto::agent::{AgentKind, AgentSource};

    use super::*;

    fn event(status: AgentStatus) -> AgentEvent {
        AgentEvent {
            session: SessionId::nil(),
            kind: AgentKind::ClaudeCode,
            status,
            agent_session: None,
            detail: Some("Waiting for permission: Bash".to_owned()),
            attention: true,
            source: AgentSource::Hook,
        }
    }

    #[test]
    fn a_blocked_agent_is_announced_once() {
        let term = TermRef { worker: WorkerId::nil(), session: SessionId::nil() };
        let blocked = AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".to_owned() });
        let (level, data) =
            needs_human(term, Some("mac-studio"), &event(blocked.clone()), None).unwrap();
        assert_eq!(level, Level::Warning);
        assert_eq!(data["agent"]["reason"], "permission");
        assert_eq!(data["agent"]["tool"], "Bash");
        assert_eq!(data["worker_name"], "mac-studio");
        let message = data["message"].as_str().unwrap();
        assert!(
            message.contains("permission for Bash") && message.contains("mac-studio"),
            "{message}"
        );
        assert_eq!(needs_human(term, None, &event(blocked.clone()), Some(&blocked)), None);
        assert_eq!(needs_human(term, None, &event(AgentStatus::Working), None), None);
        let idle = AgentStatus::Blocked(BlockReason::IdlePrompt);
        let (level, _) = needs_human(term, None, &event(idle), Some(&blocked)).unwrap();
        assert_eq!(level, Level::Notice, "an idle prompt waits, it does not block");
    }
}
