//! `slopty hook statusline`: the status-line wrapper an agent Slopty starts gets on
//! `--settings` (`slopty_agent::hooks::with_relay`).
//!
//! Claude Code runs it after each assistant message with its status-line JSON on stdin. It
//! forwards the meters the conversation face shows (context used, cost, rate limits, model,
//! session) to the worker as a `Statusline` hook, the same way the relay posts hooks, and runs
//! the person's own status-line command with the same input, printing exactly what that prints.
//! The two run side by side, and the forward gives up after [`FORWARD_TIMEOUT`], so a slow or
//! absent worker never holds the line back. Whose command runs is decided as
//! `slopty_agent::statusline` describes; with none, the line is empty, as it was.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, Result};
use serde_json::Value;
use slopty_agent::statusline;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::hook;

/// How long the forward may take before the line goes out without it.
const FORWARD_TIMEOUT: Duration = Duration::from_millis(500);

/// Set on the person's command, so a status line that runs `slopty hook statusline` itself
/// (the wrapper configured by hand) cannot run itself forever.
const NESTED_ENV: &str = "SLOPTY_STATUSLINE";

pub async fn run(data_dir: &Path, command: Option<String>) -> Result<()> {
    let mut input = Vec::new();
    tokio::io::stdin().read_to_end(&mut input).await.context("read the status line input")?;
    let status: Option<Value> = serde_json::from_slice(&input).ok();
    let theirs = if std::env::var_os(NESTED_ENV).is_some() {
        None
    } else {
        command.or_else(|| {
            let project =
                status.as_ref().and_then(project_dir).or_else(|| std::env::current_dir().ok())?;
            let user = statusline::user_settings(&slopty_agent::hooks::home_dir());
            let setting = statusline::configured(&project, &user)?;
            statusline::command_of(&setting).map(str::to_owned)
        })
    };
    let session = hook::session().ok().flatten();
    let socket = hook::socket(data_dir);
    let forward = async {
        if let (Some(status), Some(session)) = (status.as_ref(), session) {
            forward(&socket, session, status).await;
        }
    };
    let (output, ()) = tokio::join!(line(theirs.as_deref(), &input), forward);
    let mut stdout = tokio::io::stdout();
    stdout.write_all(&output?).await?;
    stdout.flush().await?;
    Ok(())
}

/// Where the agent was started: the project whose settings Claude Code reads.
fn project_dir(status: &Value) -> Option<std::path::PathBuf> {
    ["/workspace/project_dir", "/workspace/current_dir", "/cwd"]
        .iter()
        .find_map(|path| status.pointer(path)?.as_str())
        .map(std::path::PathBuf::from)
}

/// Post the meters; a worker that is away or slow is let go.
async fn forward(socket: &Path, session: slopty_core::SessionId, status: &Value) {
    let payload = match serde_json::to_string(&statusline::hook(status)) {
        Ok(payload) => payload,
        Err(e) => {
            tracing::debug!(error = %e, "status line");
            return;
        }
    };
    match tokio::time::timeout(FORWARD_TIMEOUT, hook::post(socket, session, payload)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::debug!(error = %e, "status line"),
        Err(_elapsed) => tracing::debug!("status line: the worker did not answer in time"),
    }
}

/// The person's status line for `input`: what their command prints, byte for byte, run by the
/// shell as Claude Code runs it. No command, no line.
async fn line(command: Option<&str>, input: &[u8]) -> Result<Vec<u8>> {
    let Some(command) = command else { return Ok(Vec::new()) };
    let mut child = tokio::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .env(NESTED_ENV, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("run the status line {command:?}"))?;
    let stdin = child.stdin.take();
    let feed = async move {
        // A command that never reads its input closes the pipe; that is its business.
        if let Some(mut stdin) = stdin
            && let Err(e) = stdin.write_all(input).await
        {
            tracing::debug!(error = %e, "status line input");
        }
    };
    let ((), output) = tokio::join!(feed, child.wait_with_output());
    Ok(output?.stdout)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use slopty_worker::ctl::{CtlReply, CtlRequest};
    use tokio::io::{AsyncBufReadExt as _, BufReader};
    use tokio::net::UnixListener;

    use super::*;

    /// The person's command gets the input and its output comes back unchanged: colours, no
    /// trailing newline, several lines. Without a command there is no line.
    #[tokio::test]
    async fn the_persons_line_passes_through_unchanged() {
        let input = br#"{"model":{"display_name":"Opus"}}"#;
        let echoed = line(Some("cat"), input).await.expect("cat");
        assert_eq!(echoed, input);
        let styled =
            line(Some(r"printf '\033[32mgreen\033[0m\nsecond'"), input).await.expect("printf");
        assert_eq!(styled, b"\x1b[32mgreen\x1b[0m\nsecond");
        let nested = line(Some(&format!("printf %s \"${NESTED_ENV}\"")), input).await.expect("env");
        assert_eq!(nested, b"1", "the command knows it runs under the wrapper");
        assert_eq!(line(None, input).await.expect("none"), b"");
        let failing = line(Some("printf partial; exit 3"), input).await.expect("ran");
        assert_eq!(failing, b"partial", "what a failing command printed still shows");
    }

    /// The meters reach the worker as a `Statusline` hook for the session.
    #[tokio::test]
    async fn the_meters_are_forwarded_as_a_hook() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("worker.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let worker = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (rd, mut wr) = stream.into_split();
            let mut request = String::new();
            BufReader::new(rd).read_line(&mut request).await.expect("read");
            let reply = serde_json::to_string(&CtlReply::Ok { changed: false }).expect("json");
            wr.write_all(format!("{reply}\n").as_bytes()).await.expect("reply");
            serde_json::from_str::<CtlRequest>(request.trim()).expect("a control request")
        });
        let session = slopty_core::SessionId::new();
        let status = json!({
            "session_id": "abc", "model": {"id": "claude-opus-5-5", "display_name": "Opus"},
            "context_window": {"used_percentage": 42.5}, "cost": {"total_cost_usd": 1.25},
        });
        forward(&socket, session, &status).await;
        let CtlRequest::Hook { session: posted, payload } = worker.await.expect("worker") else {
            panic!("not a hook")
        };
        assert_eq!(posted, session);
        let hook = slopty_agent::Hook::parse(&payload).expect("hook");
        assert_eq!(hook.event, statusline::STATUSLINE_EVENT);
        let meters = hook.meters.expect("meters");
        assert_eq!(
            (meters.model.as_deref(), meters.context_used_pct, meters.cost_usd),
            (Some("Opus"), Some(42.5), Some(1.25))
        );

        // No worker at all: the forward gives up at once and the line is not held.
        let started = std::time::Instant::now();
        forward(&dir.path().join("absent.sock"), session, &status).await;
        assert!(started.elapsed() < FORWARD_TIMEOUT, "{:?}", started.elapsed());
    }
}
