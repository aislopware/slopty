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
    /// Output ptyd read while nobody was attached.
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
            PtydEvent::Attached { backlog, dropped, size, .. } => {
                let master = self.fds.pop_front().ok_or(PtyError::UnexpectedReply)?;
                Ok(Attached { master, backlog, dropped, size })
            }
            other => Self::unexpected(other),
        }
    }

    /// Return the master to ptyd's care (drop your copy after this).
    pub async fn detach(&mut self, id: SessionId) -> Result<(), PtyError> {
        self.expect_ok(&PtydRequest::Detach { id }).await
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
