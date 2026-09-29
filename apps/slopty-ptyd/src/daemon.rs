//! Socket server: one task per connection, shared session table.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, Result};
use parking_lot::Mutex;
use rustix::process::Signal;
use slopty_core::SessionId;
use slopty_proto::codec;
use slopty_proto::ptyd::PtydError;
use slopty_pty::fdpass;
use slopty_pty::protocol::{MAX_CHECKPOINT_BYTES, PtydEvent, PtydRequest};
use slopty_pty::shell_integration::ShellIntegration;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;

use crate::session::{Broadcast, Session};

/// How long a closed session's child has to exit after its hangup before it is killed.
const HANGUP_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

/// Everything shared between connections.
struct State {
    sessions: Mutex<HashMap<SessionId, Arc<Session>>>,
    events: broadcast::Sender<Broadcast>,
    backlog_bytes: usize,
    /// The shell integration scripts, when they could be written.
    integration: Option<ShellIntegration>,
    next_conn: AtomicU64,
    shutdown: tokio::sync::Notify,
}

/// Bind and serve until `Shutdown`. `shell_dir` is where the shell integration scripts go.
pub async fn run(socket: &Path, backlog_bytes: usize, shell_dir: &Path) -> Result<()> {
    let listener = bind(socket).await?;
    tracing::info!(path = %socket.display(), pid = std::process::id(), "slopty-ptyd listening");
    let integration = match slopty_pty::shell_integration::install(shell_dir) {
        Ok(si) => {
            tracing::info!(dir = %si.zdotdir.display(), enabled = si.enabled, "shell integration installed");
            Some(si)
        }
        Err(e) => {
            tracing::warn!(dir = %shell_dir.display(), error = %e, "shell integration not installed");
            None
        }
    };
    // ghostty's terminfo, so the shells we spawn can be told `TERM=xterm-ghostty`. Off the
    // critical path: `tic` takes a moment, and `default_term` reads the database per spawn, so
    // a shell that starts before this lands simply gets `xterm-256color`.
    let database = slopty_pty::terminfo::user_database();
    tokio::spawn(async move {
        match slopty_pty::terminfo::install(&database).await {
            Ok(slopty_pty::terminfo::Installed::Already) => {
                tracing::debug!(db = %database.display(), "terminfo already installed");
            }
            Ok(slopty_pty::terminfo::Installed::Compiled) => {
                tracing::info!(db = %database.display(), "terminfo installed");
            }
            Err(e) => {
                tracing::warn!(db = %database.display(), error = %e, "terminfo not installed");
            }
        }
    });
    let (events, _) = broadcast::channel(256);
    let state = Arc::new(State {
        sessions: Mutex::new(HashMap::new()),
        events,
        backlog_bytes,
        integration,
        next_conn: AtomicU64::new(1),
        shutdown: tokio::sync::Notify::new(),
    });

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted.context("accept")?;
                fdpass::widen_buffers(&stream);
                let st = Arc::clone(&state);
                tokio::spawn(async move {
                    let id = st.next_conn.fetch_add(1, Ordering::Relaxed);
                    if let Err(e) = Connection::new(id, stream, st).serve().await {
                        tracing::debug!(conn = id, error = %e, "connection ended");
                    }
                });
            }
            () = state.shutdown.notified() => break,
        }
    }

    tracing::info!("shutting down: hanging up every session");
    let sessions: Vec<Arc<Session>> = state.sessions.lock().values().cloned().collect();
    for s in sessions {
        let _ignored = s.signal(Signal::HUP);
    }
    let _ignored = std::fs::remove_file(socket);
    Ok(())
}

/// Create the socket directory (0700) and bind, replacing a dead socket file.
async fn bind(socket: &Path) -> Result<UnixListener> {
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        rustix::fs::chmod(dir, rustix::fs::Mode::RWXU)
            .with_context(|| format!("chmod {}", dir.display()))?;
    }
    if socket.exists() {
        if UnixStream::connect(socket).await.is_ok() {
            anyhow::bail!("another slopty-ptyd is already listening on {}", socket.display());
        }
        tracing::warn!(path = %socket.display(), "removing stale socket");
        std::fs::remove_file(socket).context("remove stale socket")?;
    }
    let listener =
        UnixListener::bind(socket).with_context(|| format!("bind {}", socket.display()))?;
    rustix::fs::chmod(socket, rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR)
        .context("chmod socket")?;
    Ok(listener)
}

struct Connection {
    id: u64,
    stream: UnixStream,
    state: Arc<State>,
    inbox: fdpass::Inbox,
    attached: HashSet<SessionId>,
}

/// Whatever a connection held goes back to ptyd's care when it goes, however it went: the
/// worker hung up, died mid-reply, or sent a frame that does not decode. A session left claimed
/// would refuse every later attach, and one left paused would stop draining its child.
impl Drop for Connection {
    fn drop(&mut self) {
        for id in std::mem::take(&mut self.attached) {
            let session = self.state.sessions.lock().get(&id).cloned();
            if let Some(s) = session {
                s.release(self.id);
                s.resume_reader();
            }
        }
    }
}

impl Connection {
    fn new(id: u64, stream: UnixStream, state: Arc<State>) -> Self {
        Self { id, stream, state, inbox: fdpass::Inbox::default(), attached: HashSet::new() }
    }

    /// Serve until the worker hangs up or the connection fails. However it ends, dropping the
    /// connection hands back what it held (see its `Drop`).
    async fn serve(mut self) -> Result<()> {
        let mut events = self.state.events.subscribe();
        loop {
            tokio::select! {
                read = self.inbox.recv(&self.stream) => {
                    if read? == 0 {
                        return Ok(());
                    }
                    // Stray fds from a client are closed on drop; we never expect any.
                    self.inbox.close_fds();
                    while let Some(req) = self.inbox.decode::<PtydRequest>()? {
                        self.handle(req).await?;
                    }
                }
                ev = events.recv() => match ev {
                    Ok(Broadcast::Exited { id, status }) => {
                        self.reply(&PtydEvent::Exited { id, status }, None).await?;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(conn = self.id, lagged = n, "event stream lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
            }
        }
    }

    async fn reply(&self, ev: &PtydEvent, fd: Option<std::os::fd::BorrowedFd<'_>>) -> Result<()> {
        let frame = codec::encode(ev)?;
        fdpass::send(&self.stream, &frame, fd).await?;
        Ok(())
    }

    async fn error(&self, id: Option<SessionId>, error: PtydError) -> Result<()> {
        self.reply(&PtydEvent::Error { id, error }, None).await
    }

    fn session(&self, id: SessionId) -> Option<Arc<Session>> {
        self.state.sessions.lock().get(&id).cloned()
    }

    async fn handle(&mut self, req: PtydRequest) -> Result<()> {
        match req {
            PtydRequest::Spawn { id, spec } => {
                if self.session(id).is_some() {
                    return self.error(Some(id), PtydError::SessionExists).await;
                }
                match Session::spawn(
                    id,
                    &spec,
                    self.state.backlog_bytes,
                    self.state.integration.as_ref(),
                    self.state.events.clone(),
                ) {
                    Ok(session) => {
                        let info = session.info();
                        tracing::info!(session = %id, pid = info.pid, tty = %info.tty.display(), "spawned");
                        self.state.sessions.lock().insert(id, session);
                        self.reply(&PtydEvent::Spawned { id, pid: info.pid }, None).await
                    }
                    Err(e) => self.error(Some(id), PtydError::Os(e.to_string())).await,
                }
            }
            PtydRequest::Attach { id } => {
                let Some(session) = self.session(id) else {
                    return self.error(Some(id), PtydError::NoSuchSession).await;
                };
                if !session.claim(self.id) {
                    return self.error(Some(id), PtydError::AttachedElsewhere).await;
                }
                // Held from the claim on, so the connection's drop lets go of it whatever
                // happens from here.
                self.attached.insert(id);
                let handed = session.hand_over().await;
                let ev = PtydEvent::Attached {
                    id,
                    checkpoint: handed.checkpoint,
                    backlog: handed.backlog,
                    dropped: handed.dropped,
                    size: handed.size,
                    started_ms: handed.started_ms,
                    term: handed.term,
                };
                self.reply(&ev, Some(session.master_fd())).await
            }
            PtydRequest::Output { id, bytes } => {
                // No reply by contract.
                if let Some(session) = self.session(id) {
                    session.tap(self.id, &bytes);
                }
                Ok(())
            }
            PtydRequest::Checkpoint { id, state } => {
                if state.len() > MAX_CHECKPOINT_BYTES {
                    // It and the backlog would not fit the next `Attached`; the ring keeps
                    // everything since the checkpoint that did.
                    tracing::warn!(session = %id, bytes = state.len(), "checkpoint too large; ignored");
                    return Ok(());
                }
                if let Some(session) = self.session(id) {
                    session.set_checkpoint(self.id, state);
                }
                Ok(())
            }
            PtydRequest::Resize { id, size } => {
                // No reply by contract: the worker's taps queue behind it.
                let Some(session) = self.session(id) else {
                    tracing::debug!(session = %id, "resize of no session");
                    return Ok(());
                };
                session.set_size(size);
                if let Err(e) = slopty_pty::pty::set_size(session.master_fd(), size) {
                    tracing::warn!(session = %id, error = %e, "TIOCSWINSZ failed");
                }
                Ok(())
            }
            PtydRequest::Close { id } => {
                let Some(session) = self.state.sessions.lock().remove(&id) else {
                    return self.error(Some(id), PtydError::NoSuchSession).await;
                };
                self.attached.remove(&id);
                session.close(HANGUP_GRACE);
                self.reply(&PtydEvent::Ok, None).await
            }
            PtydRequest::List => {
                let list = self.state.sessions.lock().values().map(|s| s.info()).collect();
                self.reply(&PtydEvent::Sessions(list), None).await
            }
            PtydRequest::Shutdown => {
                self.reply(&PtydEvent::Ok, None).await?;
                self.state.shutdown.notify_one();
                Ok(())
            }
        }
    }
}
