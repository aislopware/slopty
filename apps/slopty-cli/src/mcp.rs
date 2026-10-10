//! `slopty mcp`: a project's tools as an MCP server on stdio, for an AI agent such as Claude Code.
//!
//! The tools are [`slopty_tools::tools`], the ones the server's own endpoint serves. Each call
//! becomes one verb (after any name lookups) on a link to the server held for the process
//! lifetime as `Role::Agent`, redialled when it drops. Answers are the same JSON as
//! `slopty … --json`; a long wait that carries a progress token reports every 10 s. An agent that
//! comes to need a human anywhere on the fleet is announced as a `notifications/message`, so the
//! orchestrating session hears of it without polling.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context as _, Result};
use rmcp::model::{Implementation, ServerCapabilities};
use rmcp::{Peer, RoleServer, ServiceExt as _};
use serde_json::{Value, json};
use slopty_core::{SessionId, WorkerId};
use slopty_net::client::bind_client;
use slopty_proto::orchestration::{Happening, HubEvent, TermAgent, TermRef};
use slopty_proto::server::{FromServer, Role};
use slopty_proto::thread::attention::{Rung, ThreadAt};
use slopty_tools::mcp::Handler;
use slopty_tools::view;
use tokio::sync::broadcast;

use crate::link::{self, Link};

/// `slopty mcp --help` epilogue.
pub const REGISTER_HELP: &str = "\
Register Slopty with Claude Code:

  claude mcp add slopty -- slopty mcp

The server is --server, else $SLOPTY_SERVER, else `server` under [network] in settings.toml, \
else the first that answers on the tailnet. \
To pin one: claude mcp add slopty -- slopty mcp --server studio";

/// Serve MCP on stdio until the client hangs up.
pub async fn run(server: Option<&str>, data_dir: &Path) -> Result<()> {
    let endpoint = bind_client()?;
    let address = link::locate(server, data_dir, &endpoint).await?;
    let name = format!("slopty mcp @ {}", crate::client::machine_name());
    let role = Role::Agent { name, vouch: crate::verbs::vouch() };
    let (link, events) = Link::persistent(endpoint.clone(), address, role);
    // Boxed: the handshake's future holds the tools' verbs, too large to keep on the stack.
    let running = Box::pin(handler(link).serve(rmcp::transport::stdio()))
        .await
        .context("the MCP client did not open a session")?;
    let forward = tokio::spawn(forward_needs(events, running.peer().clone()));
    let quit = running.waiting().await;
    forward.abort();
    crate::client::close_endpoint(&endpoint).await;
    quit.context("MCP session")?;
    Ok(())
}

/// The tools over `link`, reporting a long wait to a caller that asks.
fn handler(link: Link) -> Handler<Link> {
    #[expect(
        deprecated,
        reason = "SEP-2577 deprecates logging, but a `notifications/message` is the one message a \
                  stdio client shows unprompted; Claude Code's channels are a preview on an older \
                  revision"
    )]
    let capabilities = ServerCapabilities::builder().enable_logging().enable_tools().build();
    let info = Implementation::new("slopty", env!("CARGO_PKG_VERSION"));
    Handler::new(link, info, capabilities).with_progress()
}

/// Announce each agent that comes to need a human, once each time it does, until the client
/// leaves.
async fn forward_needs(mut pushed: broadcast::Receiver<FromServer>, peer: Peer<RoleServer>) {
    let mut names: HashMap<WorkerId, String> = HashMap::new();
    let mut last: HashMap<ThreadAt, Rung> = HashMap::new();
    // The ladder to take the state from: the first, and the next after a lag.
    let mut from_ladder = true;
    loop {
        let msg = match pushed.recv().await {
            Ok(msg) => msg,
            Err(broadcast::error::RecvError::Lagged(missed)) => {
                tracing::warn!(missed, "server events dropped");
                from_ladder = true;
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
            // The state: what it shows needs no note, only a change after it does.
            FromServer::Ladder(ladder) if from_ladder => {
                from_ladder = false;
                last = ladder.threads.iter().map(|r| (r.at, r.rung)).collect();
            }
            FromServer::Event(HubEvent {
                what: Happening::Rung { worker, terminal, agent },
                ..
            }) => {
                let before = last.insert(ThreadAt { worker, thread: agent.thread }, agent.rung);
                let name = names.get(&worker).map(String::as_str);
                if let Some(note) = needs_human((worker, terminal), name, &agent, before)
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

#[expect(deprecated, reason = "see `handler`: logging is how a stdio client hears of it")]
async fn notify(peer: &Peer<RoleServer>, data: Value) -> Result<()> {
    use rmcp::model::{LoggingLevel, LoggingMessageNotificationParam};
    let note =
        LoggingMessageNotificationParam::new(LoggingLevel::Warning, data).with_logger("slopty");
    peer.notify_logging_message(note).await?;
    Ok(())
}

/// The note for an agent on `worker`, in `terminal` when it runs in one, that newly needs a
/// human; `None` for one that does not, or that already did before.
fn needs_human(
    (worker, terminal): (WorkerId, Option<SessionId>),
    worker_name: Option<&str>,
    agent: &TermAgent,
    before: Option<Rung>,
) -> Option<Value> {
    if agent.rung != Rung::NeedsYou || before == Some(Rung::NeedsYou) {
        return None;
    }
    let where_ = worker_name.map_or_else(|| worker.to_string(), str::to_owned);
    let what = view::agent_text(Some(agent));
    let term = terminal.map(|session| view::term_string(TermRef { worker, session }));
    Some(json!({
        "message": format!("{what} (on {where_})"),
        "term": term,
        "worker_name": worker_name,
        "agent": view::agent(Some(agent)),
    }))
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::thread::{AgentId, Phase, ThreadId};

    use super::*;

    fn agent(rung: Rung, phase: Phase, asks: Option<&str>) -> TermAgent {
        TermAgent {
            thread: ThreadId::new(),
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            title: "Fix the build".to_owned(),
            rung,
            phase,
            wait: None,
            asks: asks.map(str::to_owned),
            since_ms: WallMs::ZERO,
        }
    }

    /// An agent that comes to need the person is announced once, with what it asks, until it
    /// stops needing them.
    #[test]
    fn an_agent_that_needs_the_person_is_announced_once() {
        let at = (WorkerId::nil(), Some(SessionId::nil()));
        let asking = agent(Rung::NeedsYou, Phase::NeedsYou, Some("Run cargo test?"));
        let data = needs_human(at, Some("mac-studio"), &asking, Some(Rung::Working)).unwrap();
        assert_eq!(data["agent"]["needs_human"], true);
        assert_eq!(data["agent"]["asks"], "Run cargo test?");
        assert_eq!(data["worker_name"], "mac-studio");
        let message = data["message"].as_str().unwrap();
        assert!(message.contains("Run cargo test?") && message.contains("mac-studio"), "{message}");
        assert_eq!(needs_human(at, None, &asking, Some(Rung::NeedsYou)), None, "said already");
        let working = agent(Rung::Working, Phase::Working, None);
        assert_eq!(needs_human(at, None, &working, Some(Rung::NeedsYou)), None);
        assert!(needs_human(at, None, &asking, None).is_some(), "news to a fresh link");
    }
}
