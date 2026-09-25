//! The streams beside the control stream: session rows, bulk bytes and TCP tunnels.
//!
//! Every unidirectional stream opens with a [`UniHead`]; a tunnel is a client-opened
//! bidirectional stream that opens with a [`TunnelOpen`]. After a bulk or tunnel header the
//! stream carries raw bytes, read through [`RawRecv`] so bytes the header's read buffered are
//! not lost.

use std::time::Duration;

use bytes::{Bytes, BytesMut};
use noq::{Connection, RecvStream, SendStream};
use slopty_core::SessionId;
use slopty_proto::terminal::TermEvent;
use slopty_proto::transfer::{BulkHeader, TunnelOpen, UniHead};

use crate::NetError;
use crate::framed::{FramedRecv, FramedSend};

/// Send priority of bulk streams: below the control stream, session streams and tunnels
/// (all at the default 0), so a file never queues ahead of a keystroke's echo.
pub const BULK_PRIORITY: i32 = -1;

/// A unidirectional stream the peer opened.
#[derive(Debug)]
pub enum Uni {
    /// A session's terminal events (worker → client).
    Session {
        /// The session.
        session: SessionId,
        /// Its events.
        rx: FramedRecv<TermEvent>,
    },
    /// Raw bytes of a file or clipboard representation.
    Bulk {
        /// What they are.
        header: BulkHeader,
        /// The bytes.
        rx: RawRecv,
    },
}

/// Raw bytes after a header.
#[derive(Debug)]
pub struct RawRecv {
    /// Bytes read past the header, handed out before the stream's.
    buf: BytesMut,
    stream: RecvStream,
}

impl RawRecv {
    pub(crate) const fn new(buf: BytesMut, stream: RecvStream) -> Self {
        Self { buf, stream }
    }

    /// The next piece of at most `max` bytes, or `None` at the end of the stream.
    pub async fn chunk(&mut self, max: usize) -> Result<Option<Bytes>, NetError> {
        if !self.buf.is_empty() {
            let n = self.buf.len().min(max);
            return Ok(Some(self.buf.split_to(n).freeze()));
        }
        self.stream.read_chunk(max).await.map_err(|e| NetError::stream(&e))
    }

    /// Give up on the rest (the transfer was cancelled): the sender's writes fail.
    pub fn stop(&mut self) {
        let _already = self.stream.stop(0_u32.into());
    }
}

/// How long a session stream may wait to open: for the peer to allow another stream, and for
/// room to write its header. Past it the attach fails rather than waiting for good.
pub const SESSION_STREAM_WAIT: Duration = Duration::from_secs(10);

/// Open a session stream to a client and write its header, giving up after `wait`
/// ([`NetError::TimedOut`]): a peer that never lets a stream close holds a new one back for
/// as long as it likes.
pub async fn open_session(
    conn: &Connection,
    session: SessionId,
    wait: Duration,
) -> Result<FramedSend<TermEvent>, NetError> {
    let open = async {
        let send = conn.open_uni().await.map_err(|e| NetError::stream(&e))?;
        let mut head = FramedSend::<UniHead>::new(send);
        head.send(&UniHead::Session { session }).await?;
        Ok(head.retype())
    };
    tokio::time::timeout(wait, open)
        .await
        .map_err(|_elapsed| NetError::TimedOut("opening a session stream"))?
}

/// Open a bulk stream, at [`BULK_PRIORITY`], and write its header; the raw bytes follow on the
/// returned stream.
pub async fn open_bulk(conn: &Connection, header: BulkHeader) -> Result<SendStream, NetError> {
    let send = conn.open_uni().await.map_err(|e| NetError::stream(&e))?;
    send.set_priority(BULK_PRIORITY).map_err(|e| NetError::stream(&e))?;
    let mut head = FramedSend::<UniHead>::new(send);
    head.send(&UniHead::Bulk(header)).await?;
    Ok(head.into_inner())
}

/// Accept the next unidirectional stream the peer opens and read its header.
pub async fn accept_uni(conn: &Connection) -> Result<Uni, NetError> {
    let recv = conn.accept_uni().await.map_err(|e| NetError::stream(&e))?;
    let mut head = FramedRecv::<UniHead>::new(recv);
    Ok(match head.recv().await? {
        UniHead::Session { session } => Uni::Session { session, rx: head.retype() },
        UniHead::Bulk(header) => Uni::Bulk { header, rx: head.into_raw() },
    })
}

/// Client: open a tunnel to `port` on the worker.
pub async fn open_tunnel(conn: &Connection, port: u16) -> Result<(SendStream, RawRecv), NetError> {
    let (send, recv) = conn.open_bi().await.map_err(|e| NetError::stream(&e))?;
    let mut head = FramedSend::<TunnelOpen>::new(send);
    head.send(&TunnelOpen { port }).await?;
    Ok((head.into_inner(), RawRecv::new(BytesMut::new(), recv)))
}

/// Worker: accept the next tunnel a client opens (every bidirectional stream after the control
/// stream is one).
pub async fn accept_tunnel(
    conn: &Connection,
) -> Result<(TunnelOpen, SendStream, RawRecv), NetError> {
    let (send, recv) = conn.accept_bi().await.map_err(|e| NetError::stream(&e))?;
    let mut head = FramedRecv::<TunnelOpen>::new(recv);
    let open = head.recv().await?;
    Ok((open, send, head.into_raw()))
}
