//! The worker's connection to ptyd.

use std::os::fd::{BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use slopty_core::SessionId;
use slopty_proto::codec;
use slopty_proto::terminal::TermSize;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::protocol::{CheckpointFrame, Exit, OutputFrame, PtydEvent, PtydRequest, SessionInfo};
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
    /// When ptyd spawned the child.
    pub started_ms: slopty_core::WallMs,
    /// The terminfo name ptyd gave the child as `TERM`.
    pub term: String,
}

/// What ptyd keeps of a session handed back to it ([`PtydClient::adopt`]) besides its master.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Adoptee {
    /// Its child.
    pub pid: u32,
    /// Size of record.
    pub size: TermSize,
    /// When the child was spawned.
    pub started_ms: slopty_core::WallMs,
    /// The terminfo name the child was given as `TERM`.
    pub term: String,
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
    ) -> Result<(Self, mpsc::UnboundedReceiver<(SessionId, Exit)>), PtyError> {
        let stream =
            UnixStream::connect(path).await.map_err(|e| PtyError::os("connect ptyd", e))?;
        fdpass::widen_buffers(&stream);
        let stream = Arc::new(stream);
        let (exits, exits_rx) = mpsc::unbounded_channel();
        let (replies, replies_rx) = mpsc::unbounded_channel();
        let reader = tokio::spawn(read_loop(Arc::clone(&stream), replies_rx, exits));
        Ok((Self { stream, replies, reader }, exits_rx))
    }

    /// The process at the other end: the ptyd this connection reached.
    #[must_use]
    pub fn peer_pid(&self) -> Option<u32> {
        let pid = self.stream.peer_cred().ok()?.pid()?;
        u32::try_from(pid).ok()
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
                PtydEvent::Attached {
                    checkpoint, backlog, dropped, size, started_ms, term, ..
                },
                Some(master),
            ) => Ok(Attached { master, checkpoint, backlog, dropped, size, started_ms, term }),
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
    /// empties the ring. Fire-and-forget, like [`Self::output`]; the state goes to the socket
    /// from where it is, behind the frame's head.
    #[expect(clippy::needless_pass_by_ref_mut, reason = "one sender at a time; see the type")]
    pub async fn checkpoint(&mut self, id: SessionId, state: &[u8]) -> Result<(), PtyError> {
        let frame = CheckpointFrame::new(id, state)?;
        fdpass::send_parts(&self.stream, &frame.parts(), None).await
    }

    /// Record a resize, so the next worker to attach starts at this size. Fire-and-forget, like
    /// [`Self::output`]: the taps of every session queue behind it on this connection.
    #[expect(clippy::needless_pass_by_ref_mut, reason = "one sender at a time; see the type")]
    pub async fn resize(&mut self, id: SessionId, size: TermSize) -> Result<(), PtyError> {
        self.send(&codec::encode(&PtydRequest::Resize { id, size })?).await
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

    /// Take back a session whose master this worker holds already
    /// ([`PtydRequest::Reclaim`]).
    pub async fn reclaim(&mut self, id: SessionId) -> Result<(), PtyError> {
        self.expect_ok(&PtydRequest::Reclaim { id }).await
    }

    /// Hand ptyd a session it does not hold, with its master ([`PtydRequest::Adopt`]).
    pub async fn adopt(
        &mut self,
        id: SessionId,
        master: BorrowedFd<'_>,
        child: Adoptee,
    ) -> Result<(), PtyError> {
        let Adoptee { pid, size, started_ms, term } = child;
        let req = PtydRequest::Adopt { id, pid, size, started_ms, term };
        match self.call_with(&req, Some(master)).await?.0 {
            PtydEvent::Ok => Ok(()),
            other => Self::unexpected(other),
        }
    }

    /// Ask the daemon to run the build at `program` in place, keeping every session
    /// ([`PtydRequest::Succeed`]). `Ok` once the connection closed without an answer, which is
    /// what a daemon that went ahead does: whether the new build came up is for the caller to
    /// read beside the socket.
    pub async fn succeed(&mut self, program: PathBuf) -> Result<(), PtyError> {
        match self.call(&PtydRequest::Succeed { program }).await {
            Err(PtyError::Closed) => Ok(()),
            Ok((other, _)) => Self::unexpected(other),
            Err(e) => Err(e),
        }
    }

    async fn expect_ok(&mut self, req: &PtydRequest) -> Result<(), PtyError> {
        match self.call(req).await?.0 {
            PtydEvent::Ok => Ok(()),
            other => Self::unexpected(other),
        }
    }

    fn unexpected<T>(ev: PtydEvent) -> Result<T, PtyError> {
        match ev {
            PtydEvent::Error { error, .. } => Err(PtyError::Daemon(error)),
            _ => Err(PtyError::UnexpectedReply),
        }
    }

    async fn send(&self, frame: &[u8]) -> Result<(), PtyError> {
        fdpass::send(&self.stream, frame, None).await
    }

    /// Send a request and wait for its reply. `&mut self` keeps one request in flight per
    /// sender, so the reader's queue of reply slots is in request order.
    async fn call(&mut self, req: &PtydRequest) -> Result<Reply, PtyError> {
        self.call_with(req, None).await
    }

    /// [`Self::call`], with `fd` riding on the request's frame.
    #[expect(clippy::needless_pass_by_ref_mut, reason = "one sender at a time; see the type")]
    async fn call_with(
        &mut self,
        req: &PtydRequest,
        fd: Option<BorrowedFd<'_>>,
    ) -> Result<Reply, PtyError> {
        let frame = codec::encode(req)?;
        let (slot, reply) = oneshot::channel();
        self.replies.send(slot).map_err(|_closed| PtyError::Closed)?;
        fdpass::send(&self.stream, &frame, fd).await?;
        reply.await.map_err(|_closed| PtyError::Closed)
    }
}

/// Read ptyd until it hangs up: exits go to `exits`, anything else answers the oldest request.
/// Returning drops `replies`, which fails every request still waiting.
async fn read_loop(
    stream: Arc<UnixStream>,
    mut replies: mpsc::UnboundedReceiver<oneshot::Sender<Reply>>,
    exits: mpsc::UnboundedSender<(SessionId, Exit)>,
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
            if let PtydEvent::Exited { id, exit } = ev {
                let _ignored = exits.send((id, exit));
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slopty_proto::input::CellMetrics;

    use super::*;

    /// A resize and a checkpoint go out without waiting for ptyd, which answers neither: a
    /// daemon that reads and never replies holds up no tap behind them.
    #[tokio::test]
    async fn a_resize_and_a_checkpoint_wait_for_no_reply() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ptyd.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let (got_tx, mut got) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut inbox = fdpass::Inbox::default();
            loop {
                while let Some(req) = inbox.decode::<PtydRequest>().unwrap() {
                    got_tx.send(req).unwrap();
                }
                if inbox.recv(&stream).await.unwrap() == 0 {
                    return;
                }
            }
        });
        let (mut client, _exits) = PtydClient::connect(&path).await.unwrap();
        let id = SessionId::new();
        let size = TermSize { cols: 90, rows: 40, metrics: CellMetrics::default() };
        let state = b"\x1b[31mstate\x1b[0m".repeat(20_000);
        let sent = async {
            client.resize(id, size).await.unwrap();
            client.checkpoint(id, &state).await.unwrap();
        };
        tokio::time::timeout(Duration::from_secs(5), sent).await.expect("nothing waits for ptyd");
        assert_eq!(got.recv().await, Some(PtydRequest::Resize { id, size }));
        assert_eq!(got.recv().await, Some(PtydRequest::Checkpoint { id, state }));
    }
}
