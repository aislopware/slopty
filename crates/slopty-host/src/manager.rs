//! Session table + ptyd connection.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use slopty_core::SessionId;
use slopty_proto::terminal::{OpenSession, SessionState, SessionSummary, TermSize};
use slopty_pty::protocol::socket_path;
use slopty_pty::{PtydClient, SpawnSpec};
use tokio::sync::mpsc;

use crate::HostError;
use crate::session::{self, Probe, SessionHandle, SessionStart, Tap};

/// Scrollback lines the engine retains per session.
pub const SCROLLBACK_LINES: u32 = 50_000;

struct Entry {
    handle: SessionHandle,
    command: Vec<String>,
    exited: Option<i32>,
}

/// Environment variable naming the session a process runs in (its [`SessionId`]).
pub const SESSION_ENV: &str = "SLOPTY_SESSION";

/// The host's session table. Cheap to clone; shared by every connection handler.
#[derive(Clone)]
pub struct Host {
    inner: Arc<Inner>,
}

struct Inner {
    ptyd: tokio::sync::Mutex<PtydClient>,
    /// Output copies and checkpoints for ptyd, drained onto `ptyd` by [`tap_loop`].
    tap: mpsc::Sender<Tap>,
    sessions: Mutex<HashMap<SessionId, Entry>>,
    exits: Mutex<Option<mpsc::UnboundedReceiver<(SessionId, i32)>>>,
    /// Environment every session gets on top of the request's (`SLOPTY_HOSTD_SOCKET`).
    session_env: Mutex<Vec<(String, String)>>,
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
        let (tap, tap_rx) = mpsc::channel(TAP_QUEUE);
        let host = Self {
            inner: Arc::new(Inner {
                ptyd: tokio::sync::Mutex::new(client),
                tap,
                sessions: Mutex::new(HashMap::new()),
                exits: Mutex::new(Some(exits)),
                session_env: Mutex::new(Vec::new()),
            }),
        };
        tokio::spawn(tap_loop(Arc::downgrade(&host.inner), tap_rx));
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

    /// Environment variables every future session is spawned with, in addition to the
    /// request's. The daemon uses this to tell sessions where its control socket is.
    pub fn set_session_env(&self, env: Vec<(String, String)>) {
        *self.inner.session_env.lock() = env;
    }

    /// Create a session.
    pub async fn open(&self, req: &OpenSession) -> Result<SessionHandle, HostError> {
        let id = SessionId::new();
        // Programs in the session (the `slopty hook` relay above all) learn which session they
        // run in from the environment.
        let mut env = self.inner.session_env.lock().clone();
        env.extend(req.env.iter().cloned());
        env.push((SESSION_ENV.to_owned(), id.to_string()));
        let spec = SpawnSpec {
            command: req.command.clone(),
            cwd: req.cwd.clone().map(PathBuf::from),
            env,
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
        if attached.dropped > 0 {
            tracing::warn!(session = %id, dropped = attached.dropped, "output lost before the backlog; the replay starts mid-stream");
        }
        let handle = session::spawn(SessionStart {
            id,
            master: attached.master,
            checkpoint: attached.checkpoint,
            backlog: attached.backlog,
            tap: self.inner.tap.clone(),
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
                kind: slopty_proto::terminal::SessionKind::Terminal,
                title: snap.title.clone().unwrap_or_else(|| {
                    command.first().cloned().unwrap_or_else(|| "shell".to_owned())
                }),
                cwd: snap.cwd,
                repo: snap.repo,
                cols: snap.size.cols,
                rows: snap.size.rows,
                state,
                viewers: snap.viewers,
                command,
            });
        }
        out
    }

    /// What can be seen of every live session from outside it ([`Probe`]): the daemon's agent
    /// tick reads this to attribute sessions no hook has spoken for.
    pub async fn probe(&self) -> Vec<(SessionId, Probe)> {
        let handles: Vec<(SessionId, SessionHandle)> = self
            .inner
            .sessions
            .lock()
            .iter()
            .filter(|(_id, e)| e.exited.is_none())
            .map(|(id, e)| (*id, e.handle.clone()))
            .collect();
        let mut out = Vec::with_capacity(handles.len());
        for (id, handle) in handles {
            if let Ok(probe) = handle.probe().await {
                out.push((id, probe));
            }
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
}

/// Taps waiting for ptyd: 64 KiB reads at most, so this bounds the memory a stalled ptyd can
/// cost the host at about 64 MiB; a full queue makes the actor checkpoint early instead.
const TAP_QUEUE: usize = 1024;

/// Forward output copies and checkpoints to ptyd on the host's connection until the host goes
/// away. The taps ride the same connection as the requests, and only that connection may tap
/// (ptyd checks it holds the master), so a dying host's last taps and its EOF reach ptyd in
/// order. A failed send is logged and the loop goes on: the next request will notice a dead
/// ptyd, and a rejected frame (too large) must not stop the other sessions' taps.
async fn tap_loop(inner: Weak<Inner>, mut rx: mpsc::Receiver<Tap>) {
    while let Some(tap) = rx.recv().await {
        let Some(inner) = inner.upgrade() else { return };
        let sent = send_tap(&mut *inner.ptyd.lock().await, tap).await;
        if let Err(e) = sent {
            tracing::warn!(error = %e, "ptyd tap not sent");
        }
    }
}

async fn send_tap(ptyd: &mut PtydClient, tap: Tap) -> Result<(), slopty_pty::PtyError> {
    match tap {
        Tap::Output { id, bytes } => ptyd.output(id, bytes).await,
        Tap::Checkpoint { id, state } => ptyd.checkpoint(id, state).await,
    }
}
