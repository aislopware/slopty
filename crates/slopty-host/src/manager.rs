//! Session table + ptyd connection.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use slopty_core::{ClientId, SessionId};
use slopty_proto::terminal::{OpenSession, SessionState, SessionSummary, TermSize};
use slopty_pty::protocol::socket_path;
use slopty_pty::{PtydClient, SpawnSpec};
use tokio::sync::mpsc;

use crate::HostError;
use crate::session::{self, SessionHandle, SessionStart};

/// Scrollback lines the engine retains per session.
pub const SCROLLBACK_LINES: u32 = 50_000;

struct Entry {
    handle: SessionHandle,
    command: Vec<String>,
    exited: Option<i32>,
}

/// The host's session table. Cheap to clone; shared by every connection handler.
#[derive(Clone)]
pub struct Host {
    inner: Arc<Inner>,
}

struct Inner {
    ptyd: tokio::sync::Mutex<PtydClient>,
    sessions: Mutex<HashMap<SessionId, Entry>>,
    exits: Mutex<Option<mpsc::UnboundedReceiver<(SessionId, i32)>>>,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host").field("sessions", &self.inner.sessions.lock().len()).finish()
    }
}

impl Host {
    /// Connect to ptyd (default socket or `$SLOPTY_PTYD_SOCKET`) and adopt every session it
    /// already holds.
    pub async fn connect(socket: Option<PathBuf>) -> Result<Self, HostError> {
        let path = socket.unwrap_or_else(socket_path);
        let (mut client, exits) = PtydClient::connect(&path).await?;
        let existing = client.list().await?;
        let host = Self {
            inner: Arc::new(Inner {
                ptyd: tokio::sync::Mutex::new(client),
                sessions: Mutex::new(HashMap::new()),
                exits: Mutex::new(Some(exits)),
            }),
        };
        for info in existing {
            tracing::info!(session = %info.id, pid = info.pid, "adopting session from ptyd");
            if let Err(e) = host.adopt(info.id, info.size, Vec::new()).await {
                tracing::warn!(session = %info.id, error = %e, "adopt failed");
            }
        }
        Ok(host)
    }

    /// Take the exit-notification receiver (once); the caller pumps it into `on_exit`.
    #[must_use]
    pub fn take_exits(&self) -> Option<mpsc::UnboundedReceiver<(SessionId, i32)>> {
        self.inner.exits.lock().take()
    }

    /// Record a child exit reported by ptyd.
    pub fn on_exit(&self, id: SessionId, status: i32) {
        if let Some(e) = self.inner.sessions.lock().get_mut(&id) {
            e.exited = Some(status);
        }
    }

    /// Create a session.
    pub async fn open(&self, req: &OpenSession) -> Result<SessionHandle, HostError> {
        let id = SessionId::new();
        let spec = SpawnSpec {
            command: req.command.clone(),
            cwd: req.cwd.clone().map(PathBuf::from),
            env: req.env.clone(),
            size: req.size,
        };
        self.inner.ptyd.lock().await.spawn(id, spec).await?;
        self.adopt(id, req.size, req.command.clone()).await
    }

    async fn adopt(
        &self,
        id: SessionId,
        size: TermSize,
        command: Vec<String>,
    ) -> Result<SessionHandle, HostError> {
        let attached = self.inner.ptyd.lock().await.attach(id).await?;
        let handle = session::spawn(SessionStart {
            id,
            master: attached.master,
            backlog: attached.backlog,
            size: if attached.size == TermSize::default() { size } else { attached.size },
            scrollback_lines: SCROLLBACK_LINES,
        })?;
        self.inner
            .sessions
            .lock()
            .insert(id, Entry { handle: handle.clone(), command, exited: None });
        Ok(handle)
    }

    /// Look up.
    pub fn get(&self, id: SessionId) -> Result<SessionHandle, HostError> {
        self.inner
            .sessions
            .lock()
            .get(&id)
            .map(|e| e.handle.clone())
            .ok_or(HostError::NoSuchSession)
    }

    /// Ids.
    #[must_use]
    pub fn ids(&self) -> Vec<SessionId> {
        self.inner.sessions.lock().keys().copied().collect()
    }

    /// Summaries for the session list.
    pub async fn summaries(&self) -> Vec<SessionSummary> {
        let entries: Vec<(SessionId, SessionHandle, Vec<String>, Option<i32>)> = self
            .inner
            .sessions
            .lock()
            .iter()
            .map(|(id, e)| (*id, e.handle.clone(), e.command.clone(), e.exited))
            .collect();
        let mut out = Vec::with_capacity(entries.len());
        for (id, handle, command, exited) in entries {
            let Ok(snap) = handle.snapshot().await else { continue };
            let state = match exited.or(snap.exited) {
                Some(status) => SessionState::Exited { status },
                None => SessionState::Running,
            };
            out.push(SessionSummary {
                id,
                title: snap.title.clone().unwrap_or_else(|| {
                    command.first().cloned().unwrap_or_else(|| "shell".to_owned())
                }),
                cwd: snap.cwd,
                cols: snap.size.cols,
                rows: snap.size.rows,
                state,
                viewers: snap.viewers,
                command,
            });
        }
        out
    }

    /// Kill the child and drop the session everywhere.
    pub async fn close(&self, id: SessionId) -> Result<(), HostError> {
        let entry = self.inner.sessions.lock().remove(&id).ok_or(HostError::NoSuchSession)?;
        entry.handle.close();
        self.inner.ptyd.lock().await.close(id).await?;
        Ok(())
    }

    /// Tell ptyd about a size change so a future host sees the truth.
    pub async fn record_size(&self, id: SessionId, size: TermSize) -> Result<(), HostError> {
        self.inner.ptyd.lock().await.resize(id, size).await?;
        Ok(())
    }

    /// Detach every client of `client` from every session (connection dropped).
    pub fn client_gone(&self, client: ClientId) {
        for e in self.inner.sessions.lock().values() {
            let _ignored = e.handle.detach(client);
        }
    }
}
