//! `slopty hook`: the Claude Code hook relay and its installer.
//!
//! Claude Code runs `slopty hook` for every registered event with a JSON payload on stdin. The
//! relay reads `SLOPTY_SESSION` (set by the worker for every session it spawns), forwards the
//! payload to `slopty-worker` over the control socket and exits 0 whatever happens: it must never
//! slow down or block the agent. `slopty hook install` registers it in `~/.claude/settings.json`
//! as an asynchronous exec-form command hook; `uninstall` removes exactly those entries.
//! `slopty hook report <status> [message]` is the same relay for any program: a wrapper around
//! another agent reports `working|blocked|done|idle|gone` and gets Claude Code's treatment.
//!
//! A `PermissionRequest` is the one hook Claude Code waits on (it is registered without
//! `async`). The relay sends it as the question itself, which the worker takes in as any hook,
//! waits for the decision and prints what Claude Code expects. The worker decides only while a
//! client follows the session; no decision prints nothing and Claude Code shows its own dialog
//! ([`slopty_agent::permission`]). `slopty hook statusline` is the status-line wrapper
//! ([`crate::statusline`]).

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use clap::{Subcommand, ValueEnum};
use slopty_agent::hooks::{self, Outcome};
use slopty_agent::permission::{self, hook_output};
use slopty_agent::{HOOK_EVENTS, HookEvent};
use slopty_core::SessionId;
use slopty_proto::ctl::{CtlReply, CtlRequest, Decision, PermissionAsk, SESSION_ENV};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;

use crate::{statusline, workerctl};

/// How long the relay waits for the daemon before giving up silently.
const RELAY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Subcommand, Debug)]
pub enum HookCmd {
    /// Register the relay in Claude Code's user settings.
    Install {
        /// Settings file (default: `~/.claude/settings.json`).
        #[arg(long)]
        settings: Option<PathBuf>,
    },
    /// Remove the relay from Claude Code's user settings.
    Uninstall {
        /// Settings file (default: `~/.claude/settings.json`).
        #[arg(long)]
        settings: Option<PathBuf>,
    },
    /// Show whether the relay is registered.
    Status {
        /// Settings file (default: `~/.claude/settings.json`).
        #[arg(long)]
        settings: Option<PathBuf>,
    },
    /// Claude Code's status line: forwards its meters to the worker, then runs your own
    /// status-line command and prints what it prints.
    Statusline {
        /// Your status-line command (default: the one your Claude Code settings name).
        #[arg(long)]
        command: Option<String>,
    },
    /// Report this session's agent status yourself (any program, from inside the session).
    Report {
        /// What the agent is doing.
        status: ReportStatus,
        /// One line of detail for the badge.
        message: Vec<String>,
    },
}

/// The words `slopty hook report` takes.
#[derive(ValueEnum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReportStatus {
    /// Thinking or running something.
    Working,
    /// Needs the human.
    Blocked,
    /// A turn finished.
    Done,
    /// At rest, at its prompt.
    Idle,
    /// No agent here any more.
    Gone,
}

impl ReportStatus {
    const fn word(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Idle => "idle",
            Self::Gone => "gone",
        }
    }
}

/// The hook payload a report relays: the shape `slopty_agent::Hook` reads as a `Report`.
fn report_payload(status: ReportStatus, message: &[String]) -> String {
    serde_json::json!({
        "hook_event_name": HookEvent::Report,
        "status": status.word(),
        "message": message.join(" "),
    })
    .to_string()
}

/// Relay stdin to the daemon. Never fails: Claude Code must not notice us.
pub async fn relay(data_dir: &Path) {
    let mut payload = String::new();
    let read = std::io::stdin().lock().read_to_string(&mut payload);
    if let Err(e) = read {
        tracing::debug!(error = %e, "hook relay");
        return;
    }
    let session = match session() {
        Ok(Some(session)) => session,
        Ok(None) => return,
        Err(e) => {
            tracing::debug!(error = %e, "hook relay");
            return;
        }
    };
    if let Some(output) =
        relay_at(&workerctl::socket(data_dir), session, &payload, permission::WAIT).await
    {
        println!("{output}");
    }
}

/// Post one hook payload or, for a permission request, ask with it and wait up to `wait` for
/// the worker's decision; what to print for Claude Code, if anything. The ask carries the hook
/// the worker takes in, so it goes once, on one connection.
async fn relay_at(
    socket: &Path,
    session: SessionId,
    payload: &str,
    wait: Duration,
) -> Option<serde_json::Value> {
    let (event, forwarded) = match forwardable(payload) {
        Ok(forwardable) => forwardable,
        Err(e) => {
            tracing::debug!(error = %e, "hook relay");
            return None;
        }
    };
    if event != HookEvent::PermissionRequest {
        if let Err(e) = post(socket, session, forwarded).await {
            tracing::debug!(error = %e, "hook relay");
        }
        return None;
    }
    let wait_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX);
    let ask = PermissionAsk { session, payload: forwarded, wait_ms };
    hook_output(&decide(socket, ask, wait).await)
}

/// The part of a hook payload the daemon reads, and the event it names. The payload is read
/// whole, since a `PostToolUse` carries the tool's output and that can run to megabytes, and
/// only the fields [`slopty_agent::Hook`] names go on, trimmed ([`slopty_agent::Hook::trimmed`]).
fn forwardable(payload: &str) -> Result<(HookEvent, String)> {
    let hook = slopty_agent::Hook::parse(payload).context("the hook payload is not JSON")?;
    let hook = hook.trimmed();
    Ok((hook.event, serde_json::to_string(&hook)?))
}

/// The worker's decision on a permission request, or [`Decision::Pass`] when it gives none
/// within `wait`: no reply, a closed connection, an error or anything unreadable.
async fn decide(socket: &Path, ask: PermissionAsk, wait: Duration) -> Decision {
    match tokio::time::timeout(wait, exchange(socket, &CtlRequest::Permission(ask))).await {
        Ok(Ok(CtlReply::Permission(answer))) => answer.decision,
        Ok(Ok(other)) => {
            tracing::debug!(reply = ?other, "no permission decision");
            Decision::Pass
        }
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "no permission decision");
            Decision::Pass
        }
        Err(_elapsed) => Decision::Pass,
    }
}

/// One line out, one line back, on the control socket. The sending half stays open until the
/// reply is in: the worker reads its closing as Claude Code having given up on the hook.
async fn exchange(socket: &Path, request: &CtlRequest) -> Result<CtlReply> {
    let stream = UnixStream::connect(socket).await?;
    let (rd, mut wr) = stream.into_split();
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    wr.write_all(&line).await?;
    let mut reply = String::new();
    BufReader::new(rd).read_line(&mut reply).await?;
    drop(wr);
    if reply.trim().is_empty() {
        bail!("the worker closed without a reply");
    }
    Ok(serde_json::from_str(reply.trim())?)
}

/// The session named by `SLOPTY_SESSION`; `None` outside one.
pub fn session() -> Result<Option<SessionId>> {
    let Some(session) = std::env::var_os(SESSION_ENV) else {
        return Ok(None);
    };
    Ok(Some(session.to_string_lossy().parse().context("SLOPTY_SESSION is not a session id")?))
}

/// Post one forwarded hook for `session`.
pub async fn post(socket: &Path, session: SessionId, payload: String) -> Result<()> {
    let request = workerctl::call_at(socket, CtlRequest::Hook { session, payload });
    let reply =
        tokio::time::timeout(RELAY_TIMEOUT, request).await.context("daemon did not answer")??;
    if let CtlReply::Error { message } = reply {
        bail!("{message}");
    }
    Ok(())
}

pub async fn run(cmd: HookCmd, data_dir: &Path) -> Result<()> {
    match cmd {
        HookCmd::Report { status, message } => {
            let Some(session) = session()? else {
                bail!("not inside a Slopty session ({SESSION_ENV} is unset)");
            };
            post(&workerctl::socket(data_dir), session, report_payload(status, &message)).await
        }
        HookCmd::Statusline { command } => statusline::run(data_dir, command).await,
        HookCmd::Install { settings } => {
            let path = settings.unwrap_or_else(default_settings);
            let outcome = hooks::install_at(&path, &relay_command()?)
                .with_context(|| format!("install into {}", path.display()))?;
            println!(
                "{} in {} ({} events)",
                match outcome {
                    Outcome::Changed => "installed",
                    Outcome::Unchanged => "already installed",
                },
                path.display(),
                HOOK_EVENTS.len()
            );
            Ok(())
        }
        HookCmd::Uninstall { settings } => {
            let path = settings.unwrap_or_else(default_settings);
            let outcome = hooks::uninstall_at(&path)
                .with_context(|| format!("uninstall from {}", path.display()))?;
            println!(
                "{}",
                match outcome {
                    Outcome::Changed => "removed",
                    Outcome::Unchanged => "not installed",
                }
            );
            Ok(())
        }
        HookCmd::Status { settings } => {
            let path = settings.unwrap_or_else(default_settings);
            let registered =
                hooks::registered(&path).with_context(|| format!("read {}", path.display()))?;
            if registered.is_empty() {
                println!("not installed in {}", path.display());
            } else {
                println!("{}/{} events in {}", registered.len(), HOOK_EVENTS.len(), path.display());
            }
            Ok(())
        }
    }
}

fn default_settings() -> PathBuf {
    hooks::settings_path(&slopty_platform::dirs::home())
}

/// The exec-form command for this binary.
fn relay_command() -> Result<String> {
    let exe = std::env::current_exe().context("current exe")?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    Ok(exe.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use serde_json::json;
    use slopty_proto::ctl::PermissionAnswer;
    use tokio::io::AsyncReadExt as _;
    use tokio::net::UnixListener;

    use super::*;

    /// A tool's output of several megabytes is cut, not into invalid JSON: what goes on reads
    /// as the same hook with the response's head.
    #[test]
    fn a_large_tool_payload_is_forwarded_trimmed() {
        let payload = json!({
            "session_id": "s1",
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "tool_use_id": "t1",
            "tool_input": { "command": "cat big.log" },
            "tool_response": { "stdout": "x".repeat(8 << 20), "interrupted": false },
            "duration_ms": 12,
            "transcript_path": "/tmp/t.jsonl",
        })
        .to_string();
        let (event, forwarded) = forwardable(&payload).expect("json");
        assert_eq!(event, HookEvent::PostToolUse);
        assert!(forwarded.len() < slopty_agent::HOOK_JSON_BUDGET, "{} bytes", forwarded.len());
        let hook = slopty_agent::Hook::parse(&forwarded).expect("json");
        let original = slopty_agent::Hook::parse(&payload).expect("json");
        assert_eq!(
            (&hook.tool_use_id, &hook.tool_input, hook.duration_ms),
            (&original.tool_use_id, &original.tool_input, Some(12))
        );
        let response = hook.tool_response.expect("response");
        assert_eq!(response["interrupted"], json!(false));
        assert!(response["stdout"].as_str().is_some_and(|s| s.ends_with('…') && s.len() < 8 << 10));
        forwardable("{\"hook_event_name\": \"Stop\"").unwrap_err();
    }

    #[test]
    fn a_report_is_a_hook_payload_the_tracker_reads() {
        let payload =
            report_payload(ReportStatus::Blocked, &["approve".to_owned(), "it?".to_owned()]);
        let hook = slopty_agent::Hook::parse(&payload).expect("json");
        assert_eq!(hook.event, HookEvent::Report);
        assert_eq!(hook.status.as_deref(), Some("blocked"));
        assert_eq!(hook.message.as_deref(), Some("approve it?"));
    }

    /// What a stand-in worker does with one connection.
    enum Reply {
        /// Answer this line.
        Line(String),
        /// Close without answering, as a worker going down does.
        Close,
        /// Keep the connection and say nothing.
        Hold,
        /// Check the relay keeps its end open while it waits, then answer this line.
        Open(String),
    }

    /// A worker on a socket in `dir` that answers connections in order and hands back the
    /// lines it was sent.
    fn worker(dir: &Path, replies: Vec<Reply>) -> (PathBuf, tokio::task::JoinHandle<Vec<String>>) {
        let socket = dir.join("worker.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let task = tokio::spawn(async move {
            let mut heard = Vec::new();
            for reply in replies {
                let (stream, _) = listener.accept().await.expect("accept");
                let (rd, mut wr) = stream.into_split();
                let mut rd = BufReader::new(rd);
                let mut line = String::new();
                rd.read_line(&mut line).await.expect("read");
                heard.push(line.trim().to_owned());
                match reply {
                    Reply::Open(answer) => {
                        let mut byte = [0_u8; 1];
                        let read =
                            tokio::time::timeout(Duration::from_millis(200), rd.read(&mut byte));
                        assert!(read.await.is_err(), "the relay closed its end while waiting");
                        wr.write_all(format!("{answer}\n").as_bytes()).await.expect("answer");
                    }
                    Reply::Line(answer) => {
                        wr.write_all(format!("{answer}\n").as_bytes()).await.expect("answer");
                    }
                    Reply::Close => drop(wr),
                    Reply::Hold => {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        drop(wr);
                    }
                }
            }
            heard
        });
        (socket, task)
    }

    fn ok() -> Reply {
        Reply::Line(serde_json::to_string(&CtlReply::Ok { changed: true }).expect("json"))
    }

    fn permission_request() -> String {
        json!({
            "session_id": "s1",
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash",
            "tool_input": { "command": "touch x", "description": "Make x" },
            "permission_suggestions": [
                { "type": "addRules", "rules": [{ "toolName": "Bash", "ruleContent": "touch x" }],
                  "behavior": "allow", "destination": "localSettings" }
            ],
        })
        .to_string()
    }

    /// A permission request goes once, as the question, and waits, its end of the socket open,
    /// for the worker's decision, which comes out as the hook output Claude Code reads.
    #[tokio::test]
    async fn a_permission_request_prints_the_workers_decision() {
        let dir = tempfile::tempdir().expect("tempdir");
        let suggested = json!([{ "type": "addRules", "rules": [{ "toolName": "Bash", "ruleContent": "touch x" }],
            "behavior": "allow", "destination": "localSettings" }]);
        let always = Decision::AllowAlways {
            updated_permissions: suggested.as_array().cloned().unwrap_or_default(),
        };
        let answer = CtlReply::Permission(PermissionAnswer { decision: always.clone() });
        let (socket, heard) =
            worker(dir.path(), vec![Reply::Open(serde_json::to_string(&answer).expect("json"))]);
        let session = SessionId::new();
        let output = relay_at(&socket, session, &permission_request(), permission::WAIT).await;
        assert_eq!(output, hook_output(&always));
        assert_eq!(
            output.map(|o| o["hookSpecificOutput"]["decision"]["updatedPermissions"].clone()),
            Some(suggested),
            "the suggested rule goes back"
        );
        let heard = heard.await.expect("worker");
        let [ask] = heard.as_slice() else { panic!("one request, not {heard:?}") };
        let Ok(CtlRequest::Permission(ask)) = serde_json::from_str(ask) else {
            panic!("an ask: {ask}");
        };
        assert_eq!((ask.session, ask.wait_ms), (session, 595_000));
        let asked = slopty_agent::Hook::parse(&ask.payload).expect("hook");
        assert_eq!(
            asked.tool_input,
            Some(json!({ "command": "touch x", "description": "Make x" }))
        );
        assert!(asked.permission_suggestions.is_some());
    }

    /// A worker that closes the connection without a decision, or answers with an error:
    /// no decision, at once, and nothing printed, so Claude Code shows its dialog.
    #[tokio::test]
    async fn a_worker_that_does_not_decide_leaves_the_dialog_to_claude_code() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (socket, _heard) = worker(dir.path(), vec![Reply::Close]);
        let started = Instant::now();
        assert_eq!(
            relay_at(&socket, SessionId::new(), &permission_request(), permission::WAIT).await,
            None
        );
        assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());

        let error = serde_json::to_string(&CtlReply::Error { message: "unknown".to_owned() })
            .expect("json");
        let dir = tempfile::tempdir().expect("tempdir");
        let (socket, _heard) = worker(dir.path(), vec![Reply::Line(error)]);
        assert_eq!(
            relay_at(&socket, SessionId::new(), &permission_request(), permission::WAIT).await,
            None
        );

        // No worker at all: nothing asked.
        let dir = tempfile::tempdir().expect("tempdir");
        let started = Instant::now();
        let absent = dir.path().join("none.sock");
        assert_eq!(
            relay_at(&absent, SessionId::new(), &permission_request(), permission::WAIT).await,
            None
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// A worker that holds the request is let go when the wait is up.
    #[tokio::test]
    async fn the_wait_for_a_decision_is_bounded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (socket, _heard) = worker(dir.path(), vec![Reply::Hold]);
        let started = Instant::now();
        let wait = Duration::from_millis(300);
        assert_eq!(relay_at(&socket, SessionId::new(), &permission_request(), wait).await, None);
        let took = started.elapsed();
        assert!(took >= wait && took < Duration::from_secs(3), "{took:?}");
    }

    /// Any other event is posted and done with: no second connection, nothing printed.
    #[tokio::test]
    async fn other_events_ask_for_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (socket, heard) = worker(dir.path(), vec![ok()]);
        let stop =
            json!({ "hook_event_name": "Stop", "last_assistant_message": "done" }).to_string();
        assert_eq!(relay_at(&socket, SessionId::new(), &stop, permission::WAIT).await, None);
        assert_eq!(heard.await.expect("worker").len(), 1);
    }
}
