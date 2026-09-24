//! Local control socket: newline-delimited JSON, one request per connection.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use slopty_agent::Hook;
use slopty_host::ctl::{CtlReply, CtlRequest, Health};
use slopty_proto::HostMsg;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
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
    let mut line = String::new();
    BufReader::new(rd).read_line(&mut line).await?;
    let req: CtlRequest = serde_json::from_str(line.trim())?;
    let reply = dispatch(&daemon, req).await;
    let mut out = serde_json::to_vec(&reply)?;
    out.push(b'\n');
    wr.write_all(&out).await?;
    wr.shutdown().await?;
    Ok(())
}

async fn dispatch(daemon: &Daemon, req: CtlRequest) -> CtlReply {
    match req {
        CtlRequest::Status => CtlReply::Status {
            id: daemon.id,
            name: daemon.name.clone(),
            sessions: daemon.host.summaries().await,
        },
        CtlRequest::Doctor => CtlReply::Doctor(Health {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            exe: std::env::current_exe()
                .map_or_else(|_| "?".to_owned(), |p| p.display().to_string()),
            screen_recording: slopty_capture::can_capture(),
            post_events: slopty_input::can_post(),
            listen: daemon.listen.to_string(),
            allow: daemon.listener.admission().ranges().iter().map(ToString::to_string).collect(),
            // Every connection subscribes to the event broadcast, plus the daemon's own keep.
            clients: daemon.events.receiver_count().saturating_sub(1),
            sessions: daemon.host.summaries().await.len(),
            uptime_secs: daemon.started_at.elapsed().as_secs(),
        }),
        CtlRequest::Screens => {
            let (live, closed) = daemon.screens.summaries();
            CtlReply::Screens { live, closed }
        }
        CtlRequest::Hook { session, payload } => match Hook::parse(&payload) {
            Ok(hook) => {
                if daemon.host.get(session).is_err() {
                    return CtlReply::Error { message: "no such session".to_owned() };
                }
                let mut event = daemon.agents.lock().apply(session, &hook);
                // A blocked or finished agent with nothing to say for itself: the transcript
                // tail has its last line. Read off the runtime's blocking pool; the table lock
                // is not held meanwhile.
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
                    let _sent = daemon.events.send(HostMsg::Agent(event));
                }
                CtlReply::Ok { changed }
            }
            Err(e) => CtlReply::Error { message: format!("bad hook payload: {e}") },
        },
    }
}
