//! The worker's connection to ptyd.

use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::Arc;

use slopty_core::SessionId;
use slopty_proto::codec;
use slopty_proto::terminal::TermSize;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::protocol::{OutputFrame, PtydEvent, PtydRequest, SessionInfo};
use crate::pty::SpawnSpec;
use crate::{PtyError, fdpass};

/// A master handed over by ptyd.
#[derive(Debug)]
pub struct Attached {
    /// The PTY master.
    pub master: OwnedFd,
    /// The last worker's terminal state (empty if none); replay it before `backlog`.
    pub checkpoint: Vec<u8>,
    /// Output since the checkpoint: tapped by the last worker, then read by ptyd while nobody
    /// was attached.
    pub backlog: Vec<u8>,
    /// Bytes lost before `backlog`.
    pub dropped: u64,
    /// Size of record.
    pub size: TermSize,
    /// Milliseconds since the Unix epoch when ptyd spawned the child.
    pub started_ms: u64,
}

/// A reply, with the fd that rode on it.
type Reply = (PtydEvent, Option<OwnedFd>);

/// One connection, answering requests in order.
///
/// A task reads the socket the whole time, so the unsolicited `Exited` events reach the
/// channel [`PtydClient::connect`] returns as ptyd sends them, whether or not a request is in
/// flight. Every method that sends takes `&mut self`: two frames written at once could
/// interleave on the stream, and a reply slot queued out of order would answer the wrong
/// request.
#[derive(Debug)]
pub struct PtydClient {
    stream: Arc<UnixStream>,
    /// Where the reader hands the next reply; queued before its request is sent.
    replies: mpsc::UnboundedSender<oneshot::Sender<Reply>>,
    reader: JoinHandle<()>,
}

impl Drop for PtydClient {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl PtydClient {
    /// Connect.
    ///
    /// The receiver yields each child exit as ptyd reports it, and ends once the connection to
    /// ptyd is gone: the worker then holds masters nobody keeps for the next worker, and can
    /// spawn nothing more.
    pub async fn connect(
        path: &Path,
    ) -> Result<(Self, mpsc::UnboundedReceiver<(SessionId, i32)>), PtyError> {
        let stream =
            UnixStream::connect(path).await.map_err(|e| PtyError::os("connect ptyd", e))?;
        fdpass::widen_buffers(&stream);
        let stream = Arc::new(stream);
        let (exits, exits_rx) = mpsc::unbounded_channel();
        let (replies, replies_rx) = mpsc::unbounded_channel();
        let reader = tokio::spawn(read_loop(Arc::clone(&stream), replies_rx, exits));
        Ok((Self { stream, replies, reader }, exits_rx))
    }

    /// Spawn a session; returns the child pid.
    pub async fn spawn(&mut self, id: SessionId, spec: SpawnSpec) -> Result<u32, PtyError> {
        match self.call(&PtydRequest::Spawn { id, spec }).await?.0 {
            PtydEvent::Spawned { pid, .. } => Ok(pid),
            other => Self::unexpected(other),
        }
    }

    /// Take the master.
    pub async fn attach(&mut self, id: SessionId) -> Result<Attached, PtyError> {
        match self.call(&PtydRequest::Attach { id }).await? {
            (
                PtydEvent::Attached { checkpoint, backlog, dropped, size, started_ms, .. },
                Some(master),
            ) => Ok(Attached { master, checkpoint, backlog, dropped, size, started_ms }),
            (other, _) => Self::unexpected(other),
        }
    }

    /// Hand ptyd a copy of output just read from an attached master, as the frame the reader
    /// built. Fire-and-forget: ptyd never replies, so this only waits for the socket to take
    /// the bytes.
    #[expect(clippy::needless_pass_by_ref_mut, reason = "one sender at a time; see the type")]
    pub async fn output(&mut self, frame: &OutputFrame) -> Result<(), PtyError> {
        self.send(frame.as_bytes()).await
    }

    /// Hand ptyd the session's current terminal state; it replaces the previous checkpoint and
    /// empties the ring. Fire-and-forget, like [`Self::output`].
    #[expect(clippy::needless_pass_by_ref_mut, reason = "one sender at a time; see the type")]
    pub async fn checkpoint(&mut self, id: SessionId, state: Vec<u8>) -> Result<(), PtyError> {
        self.send(&codec::encode(&PtydRequest::Checkpoint { id, state })?).await
    }

    /// Record a resize, so the next worker to attach starts at this size.
    pub async fn resize(&mut self, id: SessionId, size: TermSize) -> Result<(), PtyError> {
        self.expect_ok(&PtydRequest::Resize { id, size }).await
    }

    /// Kill and forget.
    pub async fn close(&mut self, id: SessionId) -> Result<(), PtyError> {
        self.expect_ok(&PtydRequest::Close { id }).await
    }

    /// Every session ptyd holds, attached or not.
    pub async fn list(&mut self) -> Result<Vec<SessionInfo>, PtyError> {
        match self.call(&PtydRequest::List).await?.0 {
            PtydEvent::Sessions(list) => Ok(list),
            other => Self::unexpected(other),
        }
    }

    /// Ask the daemon to exit.
    pub async fn shutdown(&mut self) -> Result<(), PtyError> {
        self.expect_ok(&PtydRequest::Shutdown).await
    }

    async fn expect_ok(&mut self, req: &PtydRequest) -> Result<(), PtyError> {
        match self.call(req).await?.0 {
            PtydEvent::Ok => Ok(()),
            other => Self::unexpected(other),
        }
    }

    fn unexpected<T>(ev: PtydEvent) -> Result<T, PtyError> {
        match ev {
            PtydEvent::Error { message, .. } => Err(PtyError::Daemon(message)),
            _ => Err(PtyError::UnexpectedReply),
        }
    }

    async fn send(&self, frame: &[u8]) -> Result<(), PtyError> {
        fdpass::send(&self.stream, frame, None).await
    }

    /// Send a request and wait for its reply. `&mut self` keeps one request in flight per
    /// sender, so the reader's queue of reply slots is in request order.
    #[expect(clippy::needless_pass_by_ref_mut, reason = "one sender at a time; see the type")]
    async fn call(&mut self, req: &PtydRequest) -> Result<Reply, PtyError> {
        let frame = codec::encode(req)?;
        let (slot, reply) = oneshot::channel();
        self.replies.send(slot).map_err(|_closed| PtyError::Closed)?;
        self.send(&frame).await?;
        reply.await.map_err(|_closed| PtyError::Closed)
    }
}

/// Read ptyd until it hangs up: exits go to `exits`, anything else answers the oldest request.
/// Returning drops `replies`, which fails every request still waiting.
async fn read_loop(
    stream: Arc<UnixStream>,
    mut replies: mpsc::UnboundedReceiver<oneshot::Sender<Reply>>,
    exits: mpsc::UnboundedSender<(SessionId, i32)>,
) {
    let mut inbox = fdpass::Inbox::default();
    loop {
        loop {
            let ev = match inbox.decode::<PtydEvent>() {
                Ok(Some(ev)) => ev,
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(error = %e, "ptyd sent a frame that does not decode");
                    return;
                }
            };
            if let PtydEvent::Exited { id, status } = ev {
                let _ignored = exits.send((id, status));
                continue;
            }
            // The fd rides on the first byte of its frame, so it is queued by now.
            let fd = matches!(ev, PtydEvent::Attached { .. }).then(|| inbox.take_fd()).flatten();
            match replies.try_recv() {
                Ok(slot) => {
                    let _ignored = slot.send((ev, fd));
                }
                Err(_none) => tracing::warn!(?ev, "ptyd replied to nothing"),
            }
        }
        match inbox.recv(&stream).await {
            Ok(0) => return,
            Ok(_read) => {}
            Err(e) => {
                tracing::debug!(error = %e, "ptyd connection ended");
                return;
            }
        }
    }
}
