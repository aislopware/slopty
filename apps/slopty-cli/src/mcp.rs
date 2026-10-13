//! `slopty mcp`: the agents' tools as an MCP server on stdio, for an AI agent such as Claude Code.
//!
//! The tools are [`slopty_tools::tools`], the ones the server's own endpoint serves. Each call
//! becomes one verb (after any name lookups) on a link to the server held for the process
//! lifetime as `Role::Agent`, redialled when it drops. Answers are the same JSON as
//! `slopty … --json`; a long wait that carries a progress token reports every 10 s. An agent that
//! comes to need a human anywhere on the fleet is announced as a `notifications/message`, so the
//! orchestrating session hears of it without polling.
//!
//! An agent outlives an update of Slopty: its `slopty mcp` keeps running the build it started
//! with, and the server, updated, runs another one the old build cannot talk to. Then each call
//! goes through the binary installed in this one's place, run once for it (`slopty mcp --call`),
//! which speaks the server's build ([`Bridged`]); the agent's tools keep working without its
//! restart. The tool's name and arguments are what crosses, the agent's own words, which both
//! builds read alike.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context as _, Result, anyhow};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerConfig,
    Tool,
};
use rmcp::service::{MaybeSendFuture, RequestContext};
use rmcp::{ErrorData, Peer, RoleServer, ServerHandler, ServiceExt as _};
use serde_json::{Value, json};
use slopty_core::{SessionId, WorkerId};
use slopty_net::client::bind_client;
use slopty_proto::orchestration::{Happening, HubEvent, TermAgent, TermRef};
use slopty_proto::server::{FromServer, Role};
use slopty_proto::thread::attention::{Rung, ThreadAt};
use slopty_tools::mcp::Handler;
use slopty_tools::view;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
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
    // Where this build is installed, read now: an update puts another build there.
    let installed = installed_exe();
    let mut flags = vec!["--data-dir".to_owned(), data_dir.to_string_lossy().into_owned()];
    flags.extend(server.map(|s| ["--server".to_owned(), s.to_owned()]).into_iter().flatten());
    let bridged = Bridged { tools: handler(link.clone()), link, installed, flags };
    // Boxed: the handshake's future holds the tools' verbs, too large to keep on the stack.
    let running = Box::pin(bridged.serve(rmcp::transport::stdio()))
        .await
        .context("the MCP client did not open a session")?;
    let forward = tokio::spawn(forward_needs(events, running.peer().clone()));
    let quit = running.waiting().await;
    forward.abort();
    crate::client::close_endpoint(&endpoint).await;
    quit.context("MCP session")?;
    Ok(())
}

/// `slopty mcp --call <tool>`: one call of `tool`, its arguments a JSON object on stdin, its
/// result printed as MCP's `CallToolResult` JSON: what an older `slopty mcp` runs to reach a
/// server on this build ([`Bridged`]). Its link waits out a server it cannot reach as
/// `slopty mcp`'s does.
pub async fn call_once(server: Option<&str>, data_dir: &Path, tool: &str) -> Result<()> {
    let mut input = String::new();
    tokio::io::stdin().read_to_string(&mut input).await.context("the arguments")?;
    let arguments: serde_json::Map<String, Value> = if input.trim().is_empty() {
        serde_json::Map::new()
    } else {
        serde_json::from_str(&input).context("the arguments are a JSON object")?
    };
    let endpoint = bind_client()?;
    let address = link::locate(server, data_dir, &endpoint).await?;
    let name = format!("slopty mcp @ {} (for an older build)", crate::client::machine_name());
    let role = Role::Agent { name, vouch: crate::verbs::vouch() };
    let (link, _events) = Link::persistent(endpoint.clone(), address, role);
    let called = slopty_tools::tools::call(&link, tool, arguments, None).await;
    crate::client::close_endpoint(&endpoint).await;
    let result = called.map_err(|e| anyhow!("{}", e.message))?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

/// This binary's path, for the build an update installs in its place; `None` when the system
/// does not say. Linux names a replaced binary's old file `… (deleted)`: the path is the
/// file without it.
fn installed_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let text = exe.to_string_lossy();
    Some(text.strip_suffix(" (deleted)").map_or_else(|| exe.clone(), PathBuf::from))
}

/// The tools, carried to a server on another build through the binary installed since.
struct Bridged {
    tools: Handler<Link>,
    link: Link,
    /// The binary at this one's path, run once a call while the server is on another build.
    installed: Option<PathBuf>,
    /// The flags this one was started with that say where the server and the data are.
    flags: Vec<String>,
}

impl ServerHandler for Bridged {
    fn get_info(&self) -> ServerConfig {
        self.tools.get_info()
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        self.tools.supported_protocol_versions()
    }

    fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        self.tools.list_tools(request, context)
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools.get_tool(name)
    }

    /// Over this build's link while the server speaks it; through the build installed since,
    /// once the server answered on another one, the call made then included.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if !self.link.other_build() {
            let answered = self.tools.call_tool(request.clone(), context).await;
            if !self.link.other_build() {
                return answered;
            }
        }
        let Some(installed) = &self.installed else {
            let why = "the server runs another build, and this one cannot find its own binary";
            return Ok(CallToolResult::error(vec![ContentBlock::text(why)]).into());
        };
        let arguments = request.arguments.unwrap_or_default();
        Ok(bridge(installed, &self.flags, &request.name, &arguments).await.into())
    }
}

/// `tool` called once by `installed` (`slopty mcp --call`), with `flags` and `arguments`.
async fn bridge(
    installed: &Path,
    flags: &[String],
    tool: &str,
    arguments: &serde_json::Map<String, Value>,
) -> CallToolResult {
    let called = async {
        let mut child = tokio::process::Command::new(installed)
            .args(flags)
            .args(["mcp", "--call", tool])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("starting {}", installed.display()))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(serde_json::to_string(arguments)?.as_bytes()).await?;
        }
        let out = child.wait_with_output().await?;
        if !out.status.success() {
            return Err(anyhow!("it exited {}", out.status));
        }
        Ok::<_, anyhow::Error>(serde_json::from_slice::<CallToolResult>(&out.stdout)?)
    };
    called.await.unwrap_or_else(|e| {
        let why = format!(
            "the server runs another build, and the slopty installed here could not carry \
             the call ({e:#}); update this machine, or start the agent again"
        );
        CallToolResult::error(vec![ContentBlock::text(why)])
    })
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

    /// A stand-in for the build installed since, at `dir`: it keeps its arguments and stdin
    /// beside it and prints `prints`, exiting `status`.
    fn installed(dir: &Path, prints: &str, status: u8) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let program = dir.join("slopty");
        let script = format!(
            "#!/bin/sh\necho \"$@\" > \"$0.args\"\ncat > \"$0.in\"\nprintf '%s' '{prints}'\nexit {status}\n"
        );
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        program
    }

    /// A call carried by the build installed since: it is run once with where the server and
    /// the data are, the tool and its arguments, and what it prints is the result. One that
    /// fails says why, as the model reads a tool's failure.
    #[tokio::test]
    async fn a_call_is_carried_by_the_build_installed_since() {
        let dir = tempfile::tempdir().unwrap();
        let carried = r#"{"content":[{"type":"text","text":"{\"ok\":true}"}],"isError":false}"#;
        let program = installed(dir.path(), carried, 0);
        let flags =
            ["--data-dir".to_owned(), "/d".to_owned(), "--server".to_owned(), "s".to_owned()];
        let mut arguments = serde_json::Map::new();
        arguments.insert("agent".to_owned(), json!(7));
        let result = bridge(&program, &flags, "agent_tell", &arguments).await;
        assert_eq!(result.is_error, Some(false), "{result:?}");
        let args = std::fs::read_to_string(dir.path().join("slopty.args")).unwrap();
        assert_eq!(args.trim(), "--data-dir /d --server s mcp --call agent_tell");
        let given = std::fs::read_to_string(dir.path().join("slopty.in")).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&given).unwrap(), json!({ "agent": 7 }));

        let failing = installed(dir.path(), "", 1);
        let result = bridge(&failing, &flags, "agent_tell", &arguments).await;
        assert_eq!(result.is_error, Some(true));
        let said = serde_json::to_string(&result.content).unwrap();
        assert!(said.contains("could not carry the call"), "{said}");
    }

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
