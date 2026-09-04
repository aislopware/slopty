//! Local control socket: newline-delimited JSON, one request per connection.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use slopty_host::ctl::{CtlReply, CtlRequest, PairedSummary};
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
        CtlRequest::Ticket => {
            CtlReply::Ticket { ticket: daemon.listener.pair_ticket().await.to_string() }
        }
        CtlRequest::Status => CtlReply::Status {
            id: daemon.listener.addr().id.to_string(),
            name: daemon.name.clone(),
            sessions: daemon.host.summaries().await,
        },
        CtlRequest::Paired => {
            let store = daemon.listener.store();
            let paired = store
                .lock()
                .await
                .paired()
                .into_iter()
                .map(|(id, p)| PairedSummary {
                    endpoint: id.to_string(),
                    client: p.client,
                    name: p.name,
                    paired_at: p.paired_at,
                })
                .collect();
            CtlReply::Paired { paired }
        }
        CtlRequest::Revoke { endpoint } => match endpoint.parse() {
            Ok(id) => {
                let store = daemon.listener.store();
                let revoked = store.lock().await.revoke(&id);
                match revoked {
                    Ok(removed) => CtlReply::Ok { changed: removed },
                    Err(e) => CtlReply::Error { message: e.to_string() },
                }
            }
            Err(e) => CtlReply::Error { message: format!("bad endpoint id: {e}") },
        },
    }
}
