//! Custody of the Claude Codes the worker drives: ptyd holds each one, so its turn outlives the
//! worker (`docs/decisions/agents.md`, "Each agent's drive").
//!
//! Claude Code runs as a piped session of ptyd (`PtydRequest::SpawnPiped`): its standard input
//! and output are one end of a socket pair whose other end ptyd keeps, its standard error a file
//! of the worker's. The worker attaches the session and speaks stream-json over the socket. When
//! the worker goes, its link to ptyd drops and ptyd takes the socket back, reading what Claude
//! Code writes meanwhile into the session's backlog; the next worker attaches it again, reads
//! the backlog, and goes on. Each thread names its session by a fact
//! ([`slopty_agent::claude::driven::CUSTODY_FACT`]).
//!
//! The link is a connection of its own, beside the terminals' (`crate::manager`), which leave
//! piped sessions alone. One that is lost (ptyd handing itself to a new build) is dialled again,
//! and every session this worker holds is handed back to the ptyd it reaches, as the terminals'
//! link does.

use std::collections::HashMap;
use std::os::fd::{AsFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use slopty_core::SessionId;
use slopty_proto::ptyd::PtydError;
use slopty_pty::client::Attached;
use slopty_pty::protocol::{Exit, SessionInfo};
use slopty_pty::{Adoptee, PtyError, PtydClient, SpawnSpec};
use tokio::sync::broadcast;

/// The first wait before a lost ptyd is dialled again, doubled up to [`REDIAL_MAX`].
const REDIAL_FIRST: Duration = Duration::from_millis(50);

/// The longest wait between two dials of a lost ptyd.
const REDIAL_MAX: Duration = Duration::from_secs(5);

/// How many child exits wait for a task that is slow to hear them.
const EXITS: usize = 64;

/// The worker's link to ptyd for the Claude Codes it drives. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Custody {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    socket: PathBuf,
    /// The link, once dialled; `None` while a lost one is dialled again.
    link: tokio::sync::Mutex<Option<PtydClient>>,
    /// A lost link is being dialled again: calls meanwhile are refused at once rather than
    /// dialling a second link or waiting out the backoff.
    redialing: std::sync::atomic::AtomicBool,
    /// Each child exit ptyd reports, of any session.
    exits: broadcast::Sender<(SessionId, Exit)>,
    /// The sessions this worker holds, each with a copy of its socket: what a ptyd reached
    /// again is handed back.
    held: parking_lot::Mutex<HashMap<SessionId, (OwnedFd, Adoptee)>>,
}

/// A session attached: its socket, and what ptyd read of it while nobody held it.
#[derive(Debug)]
pub struct Taken {
    /// The socket: Claude Code's standard input and output.
    pub socket: OwnedFd,
    /// What Claude Code wrote while no worker held it, oldest first.
    pub backlog: Vec<u8>,
}

impl Custody {
    /// The link to the ptyd listening at `socket`, dialled when first used.
    #[must_use]
    pub fn new(socket: PathBuf) -> Self {
        let (exits, _none) = broadcast::channel(EXITS);
        let inner = Inner {
            socket,
            link: tokio::sync::Mutex::new(None),
            redialing: std::sync::atomic::AtomicBool::new(false),
            exits,
            held: parking_lot::Mutex::new(HashMap::new()),
        };
        Self { inner: Arc::new(inner) }
    }

    /// Where ptyd listens.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.inner.socket
    }

    /// Each child exit from now on.
    #[must_use]
    pub fn exits(&self) -> broadcast::Receiver<(SessionId, Exit)> {
        self.inner.exits.subscribe()
    }

    /// Run `call` on the link, dialling it first when there is none. Each step on the link is
    /// bounded (`slopty_pty::client::ANSWER_WITHIN`), so the lock is never held for good, and
    /// while a lost link is dialled again the call is refused at once.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the link is held across the call: one request at a time on it"
    )]
    async fn with<T>(
        &self,
        call: impl AsyncFnOnce(&mut PtydClient) -> Result<T, PtyError>,
    ) -> Result<T, String> {
        let mut link = self.inner.link.lock().await;
        if link.is_none() && self.inner.redialing.load(std::sync::atomic::Ordering::Acquire) {
            return Err("slopty-ptyd is being reached again".to_owned());
        }
        if link.is_none() {
            *link =
                Some(self.dial().await.map_err(|e| format!("slopty-ptyd is not reached: {e}"))?);
        }
        let Some(client) = link.as_mut() else { return Err("slopty-ptyd is not reached".into()) };
        match call(client).await {
            Ok(value) => Ok(value),
            Err(PtyError::Closed) => {
                *link = None;
                Err("slopty-ptyd went away".to_owned())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// Dial ptyd, and hear its exits until the link drops; then dial it again and hand it back
    /// every session held. Boxed: dialling again is part of what it starts.
    fn dial(&self) -> Pin<Box<dyn Future<Output = Result<PtydClient, PtyError>> + Send + '_>> {
        Box::pin(async move {
            let (client, mut exits) = PtydClient::connect(&self.inner.socket).await?;
            let weak = Arc::downgrade(&self.inner);
            drop(tokio::spawn(async move {
                while let Some(exit) = exits.recv().await {
                    let Some(inner) = weak.upgrade() else { return };
                    let _none = inner.exits.send(exit);
                }
                let Some(inner) = weak.upgrade() else { return };
                let custody = Self { inner };
                custody.redial().await;
            }));
            Ok(client)
        })
    }

    /// The link was lost: dialled again until ptyd answers, each session held handed back. The
    /// link's lock is held only to take the lost link out and put the new one in, never while
    /// backing off, so a stop or a start meanwhile is answered at once.
    async fn redial(&self) {
        use std::sync::atomic::Ordering;
        self.inner.redialing.store(true, Ordering::Release);
        *self.inner.link.lock().await = None;
        let mut wait = REDIAL_FIRST;
        let mut client = loop {
            if Arc::strong_count(&self.inner) <= 1 {
                return;
            }
            match self.dial().await {
                Ok(client) => break client,
                Err(e) => {
                    tracing::debug!("slopty-ptyd is not back yet: {e}");
                    tokio::time::sleep(wait).await;
                    wait = wait.saturating_mul(2).min(REDIAL_MAX);
                }
            }
        };
        let mut link = self.inner.link.lock().await;
        let held: Vec<(SessionId, OwnedFd, Adoptee)> = self
            .inner
            .held
            .lock()
            .iter()
            .filter_map(|(id, (fd, child))| Some((*id, fd.try_clone().ok()?, child.clone())))
            .collect();
        // A ptyd that handed itself to a new build kept each session, which is taken back; one
        // that started afresh holds none, and is handed each.
        for (id, fd, child) in held {
            let taken = match client.reclaim(id).await {
                Err(PtyError::Daemon(PtydError::NoSuchSession)) => {
                    client.adopt(id, fd.as_fd(), child).await
                }
                other => other,
            };
            match taken {
                Ok(()) => {
                    tracing::info!(session = %id, "a Claude Code back in slopty-ptyd's keeping");
                }
                Err(e) => tracing::warn!(session = %id, "a Claude Code was not handed back: {e}"),
            }
        }
        *link = Some(client);
        drop(link);
        self.inner.redialing.store(false, Ordering::Release);
    }

    /// Every piped session ptyd holds: the Claude Codes of this worker and of those before it.
    ///
    /// # Errors
    ///
    /// When ptyd is not reached, in words.
    pub async fn sessions(&self) -> Result<Vec<SessionInfo>, String> {
        let all = self.with(async |c| c.list().await).await?;
        Ok(all.into_iter().filter(|s| s.piped).collect())
    }

    /// Have ptyd spawn `spec` as session `id`, its standard error into `stderr`, and take it.
    ///
    /// # Errors
    ///
    /// When ptyd is not reached or the spawn fails, in words.
    pub async fn spawn(
        &self,
        id: SessionId,
        spec: SpawnSpec,
        stderr: PathBuf,
    ) -> Result<Taken, String> {
        let pid = self.with(async |c| c.spawn_piped(id, spec, Some(stderr)).await).await?;
        self.take(id, pid).await
    }

    /// Take session `id`, whose child is `pid`.
    ///
    /// # Errors
    ///
    /// When ptyd is not reached, holds no such session, or another holds it, in words.
    pub async fn take(&self, id: SessionId, pid: u32) -> Result<Taken, String> {
        let Attached { master, backlog, size, started_ms, term, .. } =
            self.with(async |c| c.attach(id).await).await?;
        let kept = master.try_clone().map_err(|e| format!("its socket could not be kept: {e}"))?;
        let child = Adoptee { pid, size, started_ms, term, piped: true };
        self.inner.held.lock().insert(id, (kept, child));
        Ok(Taken { socket: master, backlog })
    }

    /// What ptyd read of session `id` is let go: the worker has it. ptyd's backlog then holds
    /// only what comes once the worker goes.
    pub async fn read(&self, id: SessionId) {
        if let Err(e) = self.with(async |c| c.checkpoint(id, &[]).await).await {
            tracing::debug!(session = %id, "its backlog was not let go: {e}");
        }
    }

    /// The worker no longer holds session `id`, which ptyd keeps.
    pub fn let_go(&self, id: SessionId) {
        self.inner.held.lock().remove(&id);
    }

    /// End session `id` (`SIGTERM`, then `SIGKILL` to its group after a grace) and forget it.
    pub async fn close(&self, id: SessionId) {
        self.let_go(id);
        if let Err(e) = self.with(async |c| c.close(id).await).await {
            tracing::debug!(session = %id, "a Claude Code's session did not close: {e}");
        }
    }
}
