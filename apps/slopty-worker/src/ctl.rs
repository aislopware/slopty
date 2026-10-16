//! Local control socket: newline-delimited JSON, one request per connection.

use std::path::Path;

use anyhow::{Context as _, Result};
use slopty_proto::ctl::{CtlReply, CtlRequest, Health, PasteboardAccess, Tailscale};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::Daemon;

/// Bind the control socket at `path`, taking over a stale one. Bound before the worker says it
/// is up, so whoever waits on that can ask at once.
pub async fn bind(path: &Path) -> Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    }
    if path.exists() && UnixStream::connect(path).await.is_err() {
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))?;
    tracing::info!(path = %path.display(), "control socket ready");
    Ok(listener)
}

/// Answer requests on a socket from [`bind`].
pub async fn serve(daemon: Daemon, listener: UnixListener) {
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _addr)) => stream,
            Err(e) => {
                tracing::error!(error = %e, "control socket");
                return;
            }
        };
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
        CtlRequest::Doctor => CtlReply::Doctor(Box::new(doctor(&daemon).await)),
        CtlRequest::Screens => {
            let (live, closed) = daemon.screens.summaries();
            CtlReply::Screens { live, closed }
        }
        CtlRequest::Open { session, url } => match slopty_proto::handoff::page(&url) {
            Some(page) => CtlReply::Handoff(crate::handoff::open(&daemon, session, page).await),
            None => CtlReply::Error {
                message: "only an http or https address with a host is opened".to_owned(),
            },
        },
        CtlRequest::Edit(ask) if !Path::new(&ask.path).is_absolute() => {
            CtlReply::Error { message: "the file is named by an absolute path".to_owned() }
        }
        // Held while the person edits. The CLI keeps its end open while it waits; its closing
        // gives the edit up.
        CtlRequest::Edit(ask) => {
            CtlReply::Handoff(crate::handoff::edit(&daemon, ask, closed(rd)).await)
        }
        CtlRequest::Wake => CtlReply::Wake(daemon.wake.lock().awake()),
        // Bytes follow the line, on either side.
        CtlRequest::Clip(ask) => return crate::clip::answer(&daemon, ask, &mut rd, &mut wr).await,
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
    if status.backend_state != slopty_tailnet::BackendState::Running {
        return Tailscale::Down { backend: status.backend_state };
    }
    let me = status.me.as_ref();
    Tailscale::Up {
        node: me.map(|n| n.name().to_owned()).unwrap_or_default(),
        ip: me.and_then(slopty_tailnet::Node::ipv4),
    }
}

/// What the doctor says of reading the pasteboard clipboard sync keeps in step.
const fn pasteboard_access(access: slopty_platform::pasteboard_access::Access) -> PasteboardAccess {
    use slopty_platform::pasteboard_access::Access;
    match access {
        Access::Allowed => PasteboardAccess::Allowed,
        Access::NotAskedYet => PasteboardAccess::NotAskedYet,
        Access::Asks => PasteboardAccess::Asks,
        Access::Denied => PasteboardAccess::Denied,
    }
}

async fn doctor(daemon: &Daemon) -> Health {
    let caps = daemon.caps.borrow().clone();
    let server = daemon.server_link.borrow().clone();
    Health {
        worker: daemon.id,
        server,
        exe: std::env::current_exe().map_or_else(|_| "?".to_owned(), |p| p.display().to_string()),
        caps,
        listen: daemon.listen.to_string(),
        allow: daemon.listener.admission().ranges().iter().map(ToString::to_string).collect(),
        tailscale: tailscale(daemon.listener.admission().local_api().as_ref()).await,
        pasteboard: pasteboard_access(daemon.clip.access()),
        clients: daemon.wake.lock().counts().0,
        sessions: daemon.worker.session_count(),
        turns: daemon.threads.as_ref().map_or(0, crate::threads::Threads::driven_turns),
        uptime_secs: daemon.started_at.elapsed().as_secs(),
    }
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
        assert_eq!(
            tailscale_of(&signed_out),
            Tailscale::Down { backend: slopty_tailnet::BackendState::NeedsLogin }
        );
        let unknown = status(r#"{"BackendState":"Rebooting"}"#);
        assert_eq!(
            tailscale_of(&unknown),
            Tailscale::Down { backend: slopty_tailnet::BackendState::Other }
        );
        assert_eq!(tailscale(None).await, Tailscale::Absent);
    }
}
