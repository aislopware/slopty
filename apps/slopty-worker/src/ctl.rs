//! Local control socket: newline-delimited JSON, one request per connection.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result};
use slopty_agent::Hook;
use slopty_core::SessionId;
use slopty_proto::WorkerMsg;
use slopty_proto::ctl::{CtlReply, CtlRequest, Health, NotUp, PermissionAnswer, Tailscale};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::Daemon;

pub async fn serve(daemon: Daemon, path: PathBuf) {
    if let Err(e) = serve_inner(daemon, &path).await {
        tracing::error!(error = %e, "control socket");
    }
}

async fn serve_inner(daemon: Daemon, path: &Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    }
    if path.exists() && UnixStream::connect(path).await.is_err() {
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))?;
    tracing::info!(path = %path.display(), "control socket ready");
    loop {
        let (stream, _addr) = listener.accept().await?;
        let daemon = daemon.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(daemon, stream).await {
                tracing::debug!(error = %e, "control request failed");
            }
        });
    }
}

async fn handle(daemon: Daemon, stream: UnixStream) -> Result<()> {
    let (rd, mut wr) = stream.into_split();
    let mut rd = BufReader::new(rd);
    let mut line = String::new();
    rd.read_line(&mut line).await?;
    let req: CtlRequest = serde_json::from_str(line.trim())?;
    let reply = match req {
        CtlRequest::Status => CtlReply::Status {
            id: daemon.id,
            name: daemon.name.clone(),
            sessions: daemon.worker.summaries().await,
        },
        CtlRequest::Doctor => CtlReply::Doctor(doctor(&daemon).await),
        CtlRequest::Screens => {
            let (live, closed) = daemon.screens.summaries();
            CtlReply::Screens { live, closed }
        }
        CtlRequest::Hook { session, payload } => match heard(&daemon, session, &payload).await {
            Ok((_hook, changed)) => CtlReply::Ok { changed },
            Err(message) => CtlReply::Error { message },
        },
        // Taken in like any hook, then held. The relay keeps its end open while it waits; its
        // closing withdraws the question.
        CtlRequest::Permission(ask) => match heard(&daemon, ask.session, &ask.payload).await {
            Ok((hook, _changed)) => {
                let wait = Duration::from_millis(ask.wait_ms);
                let decision = crate::follow::ask(&daemon, ask.session, &hook, wait, closed(rd));
                CtlReply::Permission(PermissionAnswer { decision: decision.await })
            }
            Err(message) => CtlReply::Error { message },
        },
    };
    let mut out = serde_json::to_vec(&reply)?;
    out.push(b'\n');
    wr.write_all(&out).await?;
    wr.shutdown().await?;
    Ok(())
}

/// Finishes when the peer closes its end (or it fails); anything more it sends is passed over.
async fn closed(mut rd: BufReader<tokio::net::unix::OwnedReadHalf>) {
    let mut scratch = [0_u8; 256];
    while rd.read(&mut scratch).await.is_ok_and(|n| n > 0) {}
}

async fn tailscale(api: Option<&slopty_tailnet::LocalApi>) -> Tailscale {
    let Some(api) = api else { return Tailscale::Absent };
    match api.status().await {
        Ok(status) => tailscale_of(&status),
        Err(e) => Tailscale::Unreachable { error: e.to_string() },
    }
}

/// What the doctor says of a Tailscale that answered with `status`.
fn tailscale_of(status: &slopty_tailnet::Status) -> Tailscale {
    use slopty_tailnet::BackendState as State;
    let backend = match status.backend_state {
        State::Running => {
            let me = status.me.as_ref();
            return Tailscale::Up {
                node: me.map(|n| n.name().to_owned()).unwrap_or_default(),
                ip: me.and_then(slopty_tailnet::Node::ipv4),
            };
        }
        State::NoState => NotUp::NoState,
        State::InUseOtherUser => NotUp::InUseOtherUser,
        State::NeedsLogin => NotUp::NeedsLogin,
        State::NeedsMachineAuth => NotUp::NeedsMachineAuth,
        State::Stopped => NotUp::Stopped,
        State::Starting => NotUp::Starting,
        State::Other => NotUp::Other,
    };
    Tailscale::Down { backend }
}

/// Whether this worker may capture the screen: Screen Recording on macOS.
#[cfg(target_os = "macos")]
fn can_capture() -> bool {
    slopty_capture::can_capture()
}

/// A worker without a desktop to stream captures nothing.
#[cfg(not(target_os = "macos"))]
const fn can_capture() -> bool {
    false
}

/// Whether this worker may post input events: Accessibility on macOS.
#[cfg(target_os = "macos")]
fn can_post() -> bool {
    slopty_input::can_post()
}

/// A worker without a desktop to stream injects no input.
#[cfg(not(target_os = "macos"))]
const fn can_post() -> bool {
    false
}

async fn doctor(daemon: &Daemon) -> Health {
    Health {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        exe: std::env::current_exe().map_or_else(|_| "?".to_owned(), |p| p.display().to_string()),
        screen_recording: can_capture(),
        post_events: can_post(),
        listen: daemon.listen.to_string(),
        allow: daemon.listener.admission().ranges().iter().map(ToString::to_string).collect(),
        tailscale: tailscale(daemon.listener.admission().local_api().as_ref()).await,
        clients: daemon.wake.lock().counts().0,
        sessions: daemon.worker.session_count(),
        uptime_secs: daemon.started_at.elapsed().as_secs(),
    }
}

/// A hook fired in `session`: the followers hear of it and the agent table takes it in. The
/// hook as read, and whether the agent's status changed; or why it was not taken.
async fn heard(daemon: &Daemon, session: SessionId, payload: &str) -> Result<(Hook, bool), String> {
    let hook = Hook::parse(payload).map_err(|e| format!("bad hook payload: {e}"))?;
    if daemon.worker.get(session).is_err() {
        return Err("no such session".to_owned());
    }
    daemon.follows.lock().board.heard(session, &hook);
    let mut event = daemon.agents.lock().apply(session, &hook);
    // A question or an elicitation the notification did not spell out (a `Stop` always carries
    // its last message): the transcript tail has the line. Read off the runtime's blocking
    // pool; the table lock is not held meanwhile.
    if let Some(ev) = &mut event
        && slopty_agent::wants_transcript(ev)
        && let Some(path) = hook.transcript_path.clone()
    {
        let line = tokio::task::spawn_blocking(move || {
            slopty_agent::transcript::last_assistant_line(Path::new(&path))
        })
        .await
        .ok()
        .flatten();
        if let Some(line) = line
            && daemon.agents.lock().set_detail(session, &line)
        {
            ev.detail = Some(slopty_agent::truncate(&line));
        }
    }
    let changed = event.is_some();
    if let Some(event) = event {
        tracing::debug!(%session, status = ?event.status, detail = ?event.detail, "agent");
        let _sent = daemon.events.send(WorkerMsg::Agent(event));
    }
    Ok((hook, changed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(json: &str) -> slopty_tailnet::Status {
        serde_json::from_str(json).unwrap()
    }

    /// A running Tailscale names this node and its IPv4; any other state is down, by name;
    /// no `LocalAPI` at all is absent.
    #[tokio::test]
    async fn the_doctor_reads_tailscale_as_up_down_or_absent() {
        let up = status(
            r#"{"BackendState":"Running","Self":{"ID":"n1","HostName":"studio",
            "DNSName":"studio.tail1234.ts.net.","OS":"macOS",
            "TailscaleIPs":["fd7a:115c:a1e0::3","100.64.0.3"]}}"#,
        );
        assert_eq!(
            tailscale_of(&up),
            Tailscale::Up {
                node: "studio.tail1234.ts.net".to_owned(),
                ip: Some([100, 64, 0, 3].into())
            }
        );
        let signed_out = status(r#"{"BackendState":"NeedsLogin","Self":null}"#);
        assert_eq!(tailscale_of(&signed_out), Tailscale::Down { backend: NotUp::NeedsLogin });
        let unknown = status(r#"{"BackendState":"Rebooting"}"#);
        assert_eq!(tailscale_of(&unknown), Tailscale::Down { backend: NotUp::Other });
        assert_eq!(tailscale(None).await, Tailscale::Absent);
    }
}
