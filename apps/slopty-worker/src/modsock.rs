//! The mod socket: where Slopty's Claude Code mod posts what the model is writing.
//!
//! The mod has no socket of its own to write to, only `fetch`, so it speaks HTTP/1.1 over this
//! Unix socket: `POST /v1/events` with a JSON [`Batch`], answered `204` once the events are on
//! the board (`slopty_worker::conversation::Board::reported`). Connections are kept alive and
//! serve one request at a time, which is how the mod sends: one batch in flight, the next
//! queued behind it. The socket sits beside the control socket, in the same directory only
//! this user can open.

use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context as _, Result};
use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty, Limited};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use slopty_agent::live::Batch;
use slopty_core::SessionId;
use tokio::net::{UnixListener, UnixStream};

use crate::Daemon;

/// The one path the mod posts to.
pub const EVENTS_PATH: &str = "/v1/events";

/// The largest batch taken. A batch is what the model wrote while the last one was on its
/// way, a few kilobytes; a tool's whole input can be more.
const MAX_BATCH: usize = 8 << 20;

/// The mod socket for the control socket at `ctl`: `worker.sock` → `worker.mod.sock`.
#[must_use]
pub fn beside(ctl: &Path) -> PathBuf {
    ctl.with_extension("mod.sock")
}

/// Serve the mod socket at `path` for the daemon's life.
pub async fn serve(daemon: Daemon, path: PathBuf) {
    if let Err(e) = serve_inner(daemon, &path).await {
        tracing::error!(error = %e, "mod socket");
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
    tracing::info!(path = %path.display(), "mod socket ready");
    loop {
        let (stream, _addr) = listener.accept().await?;
        let daemon = daemon.clone();
        tokio::spawn(async move {
            let service = service_fn(move |request| {
                let daemon = daemon.clone();
                async move { Ok::<_, Infallible>(answer(&daemon, request).await) }
            });
            if let Err(e) =
                http1::Builder::new().serve_connection(TokioIo::new(stream), service).await
            {
                tracing::debug!(error = %e, "mod connection");
            }
        });
    }
}

async fn answer(daemon: &Daemon, request: Request<Incoming>) -> Response<Empty<Bytes>> {
    let status = if request.method() != Method::POST || request.uri().path() != EVENTS_PATH {
        StatusCode::NOT_FOUND
    } else {
        match Limited::new(request.into_body(), MAX_BATCH).collect().await {
            Ok(body) => take(daemon, &body.to_bytes()).await,
            Err(e) => {
                tracing::debug!(error = %e, "a mod batch that did not arrive whole");
                StatusCode::PAYLOAD_TOO_LARGE
            }
        }
    };
    let mut response = Response::new(Empty::new());
    *response.status_mut() = status;
    response
}

/// Put a batch on the board of the live session it names.
async fn take(daemon: &Daemon, body: &[u8]) -> StatusCode {
    let Ok(batch) = serde_json::from_slice::<Batch>(body) else {
        return StatusCode::BAD_REQUEST;
    };
    let session = batch.session.as_deref().and_then(|s| s.parse::<SessionId>().ok());
    // A batch may come before the open that started its agent returns.
    let here = match session {
        Some(s) => daemon.worker.get_opened(s).await.is_ok(),
        None => false,
    };
    let Some(session) = session.filter(|_| here) else {
        tracing::debug!(session = ?batch.session, "a mod batch for no session here");
        return StatusCode::NOT_FOUND;
    };
    let events = batch.decoded();
    daemon.follows.lock().board.reported(session, &events, Instant::now());
    StatusCode::NO_CONTENT
}
