//! hostd's connection to ptyd.

use std::collections::VecDeque;
use std::os::fd::OwnedFd;
use std::path::Path;

use bytes::BytesMut;
use slopty_core::SessionId;
use slopty_proto::codec;
use slopty_proto::terminal::TermSize;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use crate::protocol::{PTYD_PROTOCOL, PtydEvent, PtydRequest, SessionInfo};
use crate::pty::SpawnSpec;
use crate::{PtyError, fdpass};

/// A master handed over by ptyd.
#[derive(Debug)]
pub struct Attached {
    /// The PTY master.
    pub master: OwnedFd,
    /// The last host's terminal state (empty if none); replay it before `backlog`.
    pub checkpoint: Vec<u8>,
    /// Output since the checkpoint: tapped by the last host, then read by ptyd while nobody
    /// was attached.
    pub backlog: Vec<u8>,
    /// Bytes lost before `backlog`.
    pub dropped: u64,
    /// Size of record.
    pub size: TermSize,
}

/// One connection. Requests are answered in order; unsolicited `Exited` events are delivered
/// through the channel returned by [`PtydClient::connect`].
#[derive(Debug)]
pub struct PtydClient {
    stream: UnixStream,
    buf: BytesMut,
    fds: VecDeque<OwnedFd>,
    exits: mpsc::UnboundedSender<(SessionId, i32)>,
}

impl PtydClient {
    /// Connect and complete the hello exchange.
    pub async fn connect(
        path: &Path,
    ) -> Result<(Self, mpsc::UnboundedReceiver<(SessionId, i32)>), PtyError> {
        let stream =
            UnixStream::connect(path).await.map_err(|e| PtyError::os("connect ptyd", e))?;
        let (exits, rx) = mpsc::unbounded_channel();
        let mut client =
            Self { stream, buf: BytesMut::with_capacity(64 << 10), fds: VecDeque::new(), exits };
        match client.call(&PtydRequest::Hello { protocol: PTYD_PROTOCOL }).await? {
            PtydEvent::Hello { protocol, .. } if protocol == PTYD_PROTOCOL => Ok((client, rx)),
            PtydEvent::Hello { protocol, .. } => {
                Err(PtyError::ProtocolMismatch { ours: PTYD_PROTOCOL, theirs: protocol })
            }
            _ => Err(PtyError::UnexpectedReply),
        }
    }

    /// Spawn a session; returns the child pid.
    pub async fn spawn(&mut self, id: SessionId, spec: SpawnSpec) -> Result<u32, PtyError> {
        match self.call(&PtydRequest::Spawn { id, spec }).await? {
            PtydEvent::Spawned { pid, .. } => Ok(pid),
            other => Self::unexpected(other),
        }
    }

    /// Take the master.
    pub async fn attach(&mut self, id: SessionId) -> Result<Attached, PtyError> {
        match self.call(&PtydRequest::Attach { id }).await? {
            PtydEvent::Attached { checkpoint, backlog, dropped, size, .. } => {
                let master = self.fds.pop_front().ok_or(PtyError::UnexpectedReply)?;
                Ok(Attached { master, checkpoint, backlog, dropped, size })
            }
            other => Self::unexpected(other),
        }
    }

    /// Return the master to ptyd's care (drop your copy after this).
    pub async fn detach(&mut self, id: SessionId) -> Result<(), PtyError> {
        self.expect_ok(&PtydRequest::Detach { id }).await
    }

    /// Hand ptyd a copy of output just read from an attached master. Fire-and-forget: ptyd
    /// never replies, so this only waits for the socket to take the bytes. Use a connection of
    /// its own for these, so they never sit between a request and its reply.
    pub async fn output(&mut self, id: SessionId, bytes: Vec<u8>) -> Result<(), PtyError> {
        self.send_nowait(&PtydRequest::Output { id, bytes }).await
    }

    /// Hand ptyd the session's current terminal state; it replaces the previous checkpoint and
    /// empties the ring. Fire-and-forget, like [`Self::output`].
    pub async fn checkpoint(&mut self, id: SessionId, state: Vec<u8>) -> Result<(), PtyError> {
        self.send_nowait(&PtydRequest::Checkpoint { id, state }).await
    }

    async fn send_nowait(&mut self, req: &PtydRequest) -> Result<(), PtyError> {
        self.drain_unsolicited()?;
        let frame = codec::encode(req)?;
        fdpass::send(&self.stream, &frame, None).await
    }

    /// Route whatever ptyd has already sent (child exits). Requests that carry no reply never
    /// read, so without this a run of exits with no other traffic would fill ptyd's send buffer
    /// and stall it on us while we stall on it.
    fn drain_unsolicited(&mut self) -> Result<(), PtyError> {
        loop {
            match self.stream.try_read_buf(&mut self.buf) {
                Ok(0) => return Err(PtyError::Closed),
                Ok(_read) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(PtyError::os("read ptyd", e)),
            }
        }
        while let Some(ev) = codec::try_decode::<PtydEvent>(&mut self.buf)? {
            self.route_unsolicited(&ev);
        }
        Ok(())
    }

    /// Record a resize.
    pub async fn resize(&mut self, id: SessionId, size: TermSize) -> Result<(), PtyError> {
        self.expect_ok(&PtydRequest::Resize { id, size }).await
    }

    /// Signal the child.
    pub async fn signal(&mut self, id: SessionId, signal: i32) -> Result<(), PtyError> {
        self.expect_ok(&PtydRequest::Signal { id, signal }).await
    }

    /// Kill and forget.
    pub async fn close(&mut self, id: SessionId) -> Result<(), PtyError> {
        self.expect_ok(&PtydRequest::Close { id }).await
    }

    /// Enumerate.
    pub async fn list(&mut self) -> Result<Vec<SessionInfo>, PtyError> {
        match self.call(&PtydRequest::List).await? {
            PtydEvent::Sessions(list) => Ok(list),
            other => Self::unexpected(other),
        }
    }

    /// Ask the daemon to exit.
    pub async fn shutdown(&mut self) -> Result<(), PtyError> {
        self.expect_ok(&PtydRequest::Shutdown).await
    }

    /// Wait for the next unsolicited event without sending anything (call from a dedicated
    /// task when idle so `Exited` events flow promptly).
    pub async fn pump(&mut self) -> Result<(), PtyError> {
        let ev = self.next_event().await?;
        self.route_unsolicited(&ev);
        Ok(())
    }

    async fn expect_ok(&mut self, req: &PtydRequest) -> Result<(), PtyError> {
        match self.call(req).await? {
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

    async fn call(&mut self, req: &PtydRequest) -> Result<PtydEvent, PtyError> {
        let frame = codec::encode(req)?;
        fdpass::send(&self.stream, &frame, None).await?;
        loop {
            let ev = self.next_event().await?;
            if let PtydEvent::Exited { id, status } = ev {
                let _ignored = self.exits.send((id, status));
                continue;
            }
            return Ok(ev);
        }
    }

    fn route_unsolicited(&self, ev: &PtydEvent) {
        if let PtydEvent::Exited { id, status } = *ev {
            let _ignored = self.exits.send((id, status));
        } else {
            tracing::warn!(?ev, "ptyd sent an unexpected unsolicited event");
        }
    }

    async fn next_event(&mut self) -> Result<PtydEvent, PtyError> {
        loop {
            if let Some(ev) = codec::try_decode::<PtydEvent>(&mut self.buf)? {
                return Ok(ev);
            }
            if fdpass::recv(&self.stream, &mut self.buf, &mut self.fds).await? == 0 {
                return Err(PtyError::Closed);
            }
        }
    }
}
