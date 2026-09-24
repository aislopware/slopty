//! Socket server: one task per connection, shared session table.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, Result};
use nix::sys::signal::Signal;
use parking_lot::Mutex;
use slopty_core::SessionId;
use slopty_proto::codec;
use slopty_pty::fdpass;
use slopty_pty::protocol::{PTYD_PROTOCOL, PtydEvent, PtydRequest};
use slopty_pty::shell_integration::ShellIntegration;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;

use crate::session::{Broadcast, Session};

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
        let _ignored = s.signal(Signal::SIGHUP);
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
    greeted: bool,
}

impl Connection {
    fn new(id: u64, stream: UnixStream, state: Arc<State>) -> Self {
        Self {
            id,
            stream,
            state,
            inbox: fdpass::Inbox::default(),
            attached: HashSet::new(),
            greeted: false,
        }
    }

    async fn serve(mut self) -> Result<()> {
        let mut events = self.state.events.subscribe();
        let result = loop {
            tokio::select! {
                read = self.inbox.recv(&self.stream) => {
                    match read {
                        Ok(0) => break Ok(()),
                        Ok(_) => {}
                        Err(e) => break Err(e.into()),
                    }
                    // Stray fds from a client are closed on drop; we never expect any.
                    self.inbox.close_fds();
                    loop {
                        let req = match self.inbox.decode::<PtydRequest>() {
                            Ok(Some(req)) => req,
                            Ok(None) => break,
                            Err(e) => return Err(e.into()),
                        };
                        if let Err(e) = self.handle(req).await {
                            tracing::debug!(conn = self.id, error = %e, "request failed");
                            return Err(e);
                        }
                    }
                }
                ev = events.recv() => match ev {
                    Ok(Broadcast::Exited { id, status }) => {
                        self.reply(&PtydEvent::Exited { id, status }, None).await?;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(conn = self.id, lagged = n, "event stream lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => break Ok(()),
                },
            }
        };
        // Whatever we held goes back to ptyd's care.
        for id in std::mem::take(&mut self.attached) {
            let session = self.state.sessions.lock().get(&id).cloned();
            if let Some(s) = session {
                *s.attached_by.lock() = None;
                s.resume_reader();
            }
        }
        result
    }

    async fn reply(&self, ev: &PtydEvent, fd: Option<std::os::fd::BorrowedFd<'_>>) -> Result<()> {
        let frame = codec::encode(ev)?;
        fdpass::send(&self.stream, &frame, fd).await?;
        Ok(())
    }

    async fn error(&self, id: Option<SessionId>, message: impl Into<String>) -> Result<()> {
        self.reply(&PtydEvent::Error { id, message: message.into() }, None).await
    }

    fn session(&self, id: SessionId) -> Option<Arc<Session>> {
        self.state.sessions.lock().get(&id).cloned()
    }

    async fn handle(&mut self, req: PtydRequest) -> Result<()> {
        if !self.greeted {
            return match req {
                PtydRequest::Hello { protocol } if protocol == PTYD_PROTOCOL => {
                    self.greeted = true;
                    self.reply(
                        &PtydEvent::Hello { protocol: PTYD_PROTOCOL, pid: std::process::id() },
                        None,
                    )
                    .await
                }
                PtydRequest::Hello { protocol } => {
                    self.reply(
                        &PtydEvent::Hello { protocol: PTYD_PROTOCOL, pid: std::process::id() },
                        None,
                    )
                    .await?;
                    anyhow::bail!("protocol mismatch: client {protocol}, ours {PTYD_PROTOCOL}")
                }
                _ => anyhow::bail!("first message must be Hello"),
            };
        }
        match req {
            PtydRequest::Hello { .. } => self.error(None, "already greeted").await,
            PtydRequest::Spawn { id, spec } => {
                if self.session(id).is_some() {
                    return self.error(Some(id), "session id already exists").await;
                }
                match Session::spawn(
                    id,
                    &spec,
                    self.state.backlog_bytes,
                    self.state.integration.as_ref(),
                    self.state.events.clone(),
                ) {
                    Ok(session) => {
                        let pid = session.pid;
                        tracing::info!(session = %id, pid, tty = %session.tty.display(), "spawned");
                        self.state.sessions.lock().insert(id, session);
                        self.reply(&PtydEvent::Spawned { id, pid }, None).await
                    }
                    Err(e) => self.error(Some(id), e.to_string()).await,
                }
            }
            PtydRequest::Attach { id } => {
                let Some(session) = self.session(id) else {
                    return self.error(Some(id), "no such session").await;
                };
                let claimed = {
                    let mut holder = session.attached_by.lock();
                    match *holder {
                        Some(other) if other != self.id => false,
                        _ => {
                            *holder = Some(self.id);
                            true
                        }
                    }
                };
                if !claimed {
                    return self.error(Some(id), "attached by another connection").await;
                }
                let (backlog, dropped) = session.pause_reader().await;
                self.attached.insert(id);
                let size = *session.size.lock();
                let checkpoint = session.checkpoint.lock().clone();
                let ev = PtydEvent::Attached { id, checkpoint, backlog, dropped, size };
                self.reply(&ev, Some(session.master_fd())).await
            }
            PtydRequest::Output { id, bytes } => {
                // No reply by contract. Only the connection holding the master may tap: its
                // frames and its EOF arrive in one order, so everything a dying worker tapped is
                // in the ring before our reader resumes, and nothing it sends can land after
                // the next worker attaches.
                if let Some(session) = self.session(id)
                    && *session.attached_by.lock() == Some(self.id)
                {
                    session.tap(&bytes);
                }
                Ok(())
            }
            PtydRequest::Checkpoint { id, state } => {
                if let Some(session) = self.session(id)
                    && *session.attached_by.lock() == Some(self.id)
                {
                    session.set_checkpoint(state);
                }
                Ok(())
            }
            PtydRequest::Resize { id, size } => {
                let Some(session) = self.session(id) else {
                    return self.error(Some(id), "no such session").await;
                };
                *session.size.lock() = size;
                match slopty_pty::pty::set_size(session.master_fd(), size) {
                    Ok(()) => self.reply(&PtydEvent::Ok, None).await,
                    Err(e) => self.error(Some(id), e.to_string()).await,
                }
            }
            PtydRequest::Close { id } => {
                let Some(session) = self.state.sessions.lock().remove(&id) else {
                    return self.error(Some(id), "no such session").await;
                };
                self.attached.remove(&id);
                if session.exited.lock().is_none() {
                    let _hup = session.signal(Signal::SIGHUP);
                    let s = Arc::clone(&session);
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                        if s.exited.lock().is_none() {
                            let _kill = s.signal(Signal::SIGKILL);
                        }
                    });
                }
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
