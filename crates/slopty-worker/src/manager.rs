//! Session table + ptyd connection.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use slopty_core::SessionId;
use slopty_proto::agent::AgentStatus;
use slopty_proto::terminal::{OpenSession, SessionState, SessionSummary, TermSize};
use slopty_pty::protocol::socket_path;
use slopty_pty::{PtydClient, SpawnSpec};
use tokio::sync::mpsc;

use crate::WorkerError;
use crate::orchestrate::Agents;
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

/// The worker's session table. Cheap to clone; shared by every connection handler.
#[derive(Clone)]
pub struct Worker {
    inner: Arc<Inner>,
}

struct Inner {
    ptyd: tokio::sync::Mutex<PtydClient>,
    /// Output copies and checkpoints for ptyd, drained onto `ptyd` by [`tap_loop`].
    tap: mpsc::Sender<Tap>,
    sessions: Mutex<HashMap<SessionId, Entry>>,
    exits: Mutex<Option<mpsc::UnboundedReceiver<(SessionId, i32)>>>,
    /// Environment every session gets on top of the request's (`SLOPTY_WORKER_SOCKET`).
    session_env: Mutex<Vec<(String, String)>>,
    /// Sessions whose output named a local server ([`SessionStart::port_hints`]).
    port_hints: mpsc::UnboundedSender<SessionId>,
    port_hints_rx: Mutex<Option<mpsc::UnboundedReceiver<SessionId>>>,
    /// The coding agents seen in the sessions, which every summary carries.
    agents: Arc<dyn Agents>,
}

impl std::fmt::Debug for Worker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Worker").field("sessions", &self.inner.sessions.lock().len()).finish()
    }
}

impl Worker {
    /// Connect to ptyd (default socket or `$SLOPTY_PTYD_SOCKET`) and adopt every session it
    /// already holds. `agents` is the daemon's agent table, read into every summary.
    pub async fn connect(
        socket: Option<PathBuf>,
        agents: Arc<dyn Agents>,
    ) -> Result<Self, WorkerError> {
        let path = socket.unwrap_or_else(socket_path);
        let (mut client, exits) = PtydClient::connect(&path).await?;
        let existing = client.list().await?;
        let (tap, tap_rx) = mpsc::channel(TAP_QUEUE);
        let (port_hints, port_hints_rx) = mpsc::unbounded_channel();
        let worker = Self {
            inner: Arc::new(Inner {
                ptyd: tokio::sync::Mutex::new(client),
                tap,
                sessions: Mutex::new(HashMap::new()),
                exits: Mutex::new(Some(exits)),
                session_env: Mutex::new(Vec::new()),
                port_hints,
                port_hints_rx: Mutex::new(Some(port_hints_rx)),
                agents,
            }),
        };
        tokio::spawn(tap_loop(Arc::downgrade(&worker.inner), tap_rx));
        for info in existing {
            tracing::info!(session = %info.id, pid = info.pid, "adopting session from ptyd");
            if let Err(e) = worker.adopt(info.id, info.size, Vec::new(), info.exited).await {
                tracing::warn!(session = %info.id, error = %e, "adopt failed");
            }
        }
        Ok(worker)
    }

    /// The daemon's agent table.
    #[must_use]
    pub fn agents(&self) -> &dyn Agents {
        &*self.inner.agents
    }

    /// Take the exit-notification receiver (once); the caller pumps it into `on_exit`.
    #[must_use]
    pub fn take_exits(&self) -> Option<mpsc::UnboundedReceiver<(SessionId, i32)>> {
        self.inner.exits.lock().take()
    }

    /// Take the receiver of port hints (once): the sessions whose output named a local server.
    #[must_use]
    pub fn take_port_hints(&self) -> Option<mpsc::UnboundedReceiver<SessionId>> {
        self.inner.port_hints_rx.lock().take()
    }

    /// Record a child exit reported by ptyd, and tell the session's viewers.
    pub fn on_exit(&self, id: SessionId, status: i32) {
        if let Some(e) = self.inner.sessions.lock().get_mut(&id) {
            e.exited = Some(status);
            e.handle.exited(status);
        }
    }

    /// Environment variables every future session is spawned with, in addition to the
    /// request's. The daemon uses this to tell sessions where its control socket is.
    pub fn set_session_env(&self, env: Vec<(String, String)>) {
        *self.inner.session_env.lock() = env;
    }

    /// Create a session.
    pub async fn open(&self, req: &OpenSession) -> Result<SessionHandle, WorkerError> {
        let id = SessionId::new();
        // Programs in the session (the `slopty hook` relay above all) learn which session they
        // run in from the environment.
        let mut env = self.inner.session_env.lock().clone();
        env.extend(req.env.iter().cloned());
        env.push((SESSION_ENV.to_owned(), id.to_string()));
        let spec = SpawnSpec {
            command: req.command.clone(),
            // `~` is this worker's home: a client types it without knowing the path.
            cwd: req.cwd.as_deref().map(|cwd| crate::file::expand_home(Path::new(cwd))),
            env,
            size: req.size,
        };
        self.inner.ptyd.lock().await.spawn(id, spec).await?;
        self.adopt(id, req.size, req.command.clone(), None).await
    }

    async fn adopt(
        &self,
        id: SessionId,
        size: TermSize,
        command: Vec<String>,
        exited: Option<i32>,
    ) -> Result<SessionHandle, WorkerError> {
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
            exited,
            port_hints: Some(self.inner.port_hints.clone()),
        })?;
        self.inner.sessions.lock().insert(id, Entry { handle: handle.clone(), command, exited });
        Ok(handle)
    }

    /// The running session `id`.
    pub fn get(&self, id: SessionId) -> Result<SessionHandle, WorkerError> {
        self.inner
            .sessions
            .lock()
            .get(&id)
            .map(|e| e.handle.clone())
            .ok_or(WorkerError::NoSuchSession)
    }

    /// Summaries for the session list, each with the agent running in it now.
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
            let agent = self.inner.agents.status(id).filter(|a| a.status != AgentStatus::None);
            out.push(SessionSummary {
                id,
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
                agent,
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

    /// The process ptyd spawned for each session whose program still runs: the roots of the
    /// process trees `ListPorts` walks.
    pub async fn pids(&self) -> Result<Vec<(SessionId, u32)>, WorkerError> {
        let infos = self.inner.ptyd.lock().await.list().await?;
        Ok(infos.into_iter().filter(|i| i.exited.is_none()).map(|i| (i.id, i.pid)).collect())
    }

    /// Kill the child and drop the session everywhere.
    pub async fn close(&self, id: SessionId) -> Result<(), WorkerError> {
        let entry = self.inner.sessions.lock().remove(&id).ok_or(WorkerError::NoSuchSession)?;
        entry.handle.close();
        self.inner.ptyd.lock().await.close(id).await?;
        Ok(())
    }
}

/// Taps waiting for ptyd, from every session: 64 KiB reads at most, so this bounds the memory a
/// stalled ptyd can cost the worker at about 64 MiB. A session whose tap does not fit stops
/// tapping and checkpoints as soon as the queue has room, which replaces what it did not tap.
const TAP_QUEUE: usize = 1024;

/// Forward output copies, checkpoints and sizes to ptyd on the worker's connection until the
/// worker goes away. The taps ride the same connection as the requests, and only that connection
/// may tap (ptyd checks it holds the master), so a dying worker's last taps and its EOF reach ptyd
/// in order. A failed send is logged and the loop goes on: the next request will notice a dead
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
        Tap::Output { id, bytes } => ptyd.output(id, &bytes).await,
        Tap::Checkpoint { id, state } => ptyd.checkpoint(id, state).await,
        Tap::Resize { id, size } => ptyd.resize(id, size).await,
    }
}
