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
use slopty_proto::conversation::ConversationEvent;
use slopty_proto::terminal::TermEvent;
use slopty_proto::transfer::{BulkHeader, TunnelOpen, UniHead};

use crate::NetError;
use crate::framed::{FramedRecv, FramedSend};

/// Lowest send priority that goes ahead of queued datagrams.
///
/// `noq::TransportConfig::stream_priority_before_datagrams` takes it: the control and session
/// streams, at the default 0, go first, so a keystroke's echo leaves in the next packet instead
/// of behind a keyframe's datagrams. A tunnel or a file, below it, cannot starve video.
pub const AHEAD_OF_DATAGRAMS: i32 = 0;

/// Send priority of a session stream while it carries a viewer's echo.
///
/// Every session stream and the control stream sit at [`AHEAD_OF_DATAGRAMS`], and noq
/// round-robins equal priorities a packet each, so an echo written beside other sessions' floods
/// waited a packet per busy stream. Raised, it leaves in the next packet ([`EchoLift`]).
///
/// It is also `noq::TransportConfig::stream_priority_unpaced`: an echo does not wait for the
/// pacer's millisecond timer behind paced video (MEASUREMENTS.md, "an echo behind paced
/// video").
pub const ECHO_PRIORITY: i32 = 1;

/// Send priority of tunnels: behind video, since a download through a forwarded port has no
/// bound but the link; ahead of files.
pub const TUNNEL_PRIORITY: i32 = -1;

/// Send priority of bulk streams: below the control stream, session streams and tunnels, so a
/// file never queues ahead of a keystroke's echo.
pub const BULK_PRIORITY: i32 = -2;

/// Send priority of conversation streams, level with tunnels.
///
/// Behind video, the terminals and the control stream, since a history of any length may be
/// on its way; ahead of files, since a person is reading it now.
pub const CONVERSATION_PRIORITY: i32 = TUNNEL_PRIORITY;

const _: () = assert!(
    BULK_PRIORITY < TUNNEL_PRIORITY
        && BULK_PRIORITY < CONVERSATION_PRIORITY
        && CONVERSATION_PRIORITY < AHEAD_OF_DATAGRAMS
        && TUNNEL_PRIORITY < AHEAD_OF_DATAGRAMS
        && AHEAD_OF_DATAGRAMS < ECHO_PRIORITY,
    "files, then tunnels and conversations, then everything that goes ahead of video, then an echo"
);

/// A stream's priority, following its frames.
///
/// At [`ECHO_PRIORITY`] from a frame on a keystroke's path (a session's echo, or the client's
/// input on the control stream) until one that is not, then back at [`AHEAD_OF_DATAGRAMS`].
///
/// noq files a stream by the priority it had when data was queued on it, so the priority is
/// set before the frame is written.
#[derive(Debug, Default, Clone, Copy)]
pub struct EchoLift {
    raised: bool,
}

impl EchoLift {
    /// Before a frame goes on `stream`: `echo` is whether it is on a keystroke's path.
    pub fn before_frame<T: serde::Serialize>(&mut self, stream: &FramedSend<T>, echo: bool) {
        if echo == self.raised {
            return;
        }
        let priority = if echo { ECHO_PRIORITY } else { AHEAD_OF_DATAGRAMS };
        // A stream that is gone fails the write that follows, which ends its pump.
        if stream.set_priority(priority).is_ok() {
            self.raised = echo;
        }
    }
}

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
    /// A followed session's conversation (worker → client).
    Conversation {
        /// The terminal session the agent runs in.
        session: SessionId,
        /// Its events.
        rx: FramedRecv<ConversationEvent>,
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

/// Open a followed agent session's conversation stream to a client, at
/// [`CONVERSATION_PRIORITY`], and write its header, giving up after `wait` as
/// [`open_session`] does.
pub async fn open_conversation(
    conn: &Connection,
    session: SessionId,
    wait: Duration,
) -> Result<FramedSend<ConversationEvent>, NetError> {
    let open = async {
        let send = conn.open_uni().await.map_err(|e| NetError::stream(&e))?;
        send.set_priority(CONVERSATION_PRIORITY).map_err(|e| NetError::stream(&e))?;
        let mut head = FramedSend::<UniHead>::new(send);
        head.send(&UniHead::Conversation { session }).await?;
        Ok(head.retype())
    };
    tokio::time::timeout(wait, open)
        .await
        .map_err(|_elapsed| NetError::TimedOut("opening a conversation stream"))?
}

/// Accept the next unidirectional stream the peer opens and read its header.
///
/// The header may be lost and retransmitted; an accept loop that must not wait on one stream's
/// loss for the next accepts with [`Connection::accept_uni`] and reads each header with
/// [`read_uni`] on a task of its own.
pub async fn accept_uni(conn: &Connection) -> Result<Uni, NetError> {
    let recv = conn.accept_uni().await.map_err(|e| NetError::stream(&e))?;
    read_uni(recv).await
}

/// Read the header of a unidirectional stream the peer opened.
pub async fn read_uni(recv: RecvStream) -> Result<Uni, NetError> {
    let mut head = FramedRecv::<UniHead>::new(recv);
    Ok(match head.recv().await? {
        UniHead::Session { session } => Uni::Session { session, rx: head.retype() },
        UniHead::Bulk(header) => Uni::Bulk { header, rx: head.into_raw() },
        UniHead::Conversation { session } => Uni::Conversation { session, rx: head.retype() },
    })
}

/// Client: open a tunnel to `open`'s host and port as the worker reaches them, at
/// [`TUNNEL_PRIORITY`].
pub async fn open_tunnel(
    conn: &Connection,
    open: &TunnelOpen,
) -> Result<(SendStream, RawRecv), NetError> {
    let (send, recv) = conn.open_bi().await.map_err(|e| NetError::stream(&e))?;
    send.set_priority(TUNNEL_PRIORITY).map_err(|e| NetError::stream(&e))?;
    let mut head = FramedSend::<TunnelOpen>::new(send);
    head.send(open).await?;
    Ok((head.into_inner(), RawRecv::new(BytesMut::new(), recv)))
}

/// Worker: accept the next tunnel a client opens and read where it asks to go.
///
/// Every bidirectional stream after the control stream is one. As with [`accept_uni`], an
/// accept loop reads each header with [`read_tunnel`] on a task of its own.
pub async fn accept_tunnel(
    conn: &Connection,
) -> Result<(TunnelOpen, SendStream, RawRecv), NetError> {
    let (send, recv) = conn.accept_bi().await.map_err(|e| NetError::stream(&e))?;
    read_tunnel(send, recv).await
}

/// Worker: put a tunnel's send half at [`TUNNEL_PRIORITY`] and read where it asks to go.
pub async fn read_tunnel(
    send: SendStream,
    recv: RecvStream,
) -> Result<(TunnelOpen, SendStream, RawRecv), NetError> {
    send.set_priority(TUNNEL_PRIORITY).map_err(|e| NetError::stream(&e))?;
    let mut head = FramedRecv::<TunnelOpen>::new(recv);
    let open = head.recv().await?;
    Ok((open, send, head.into_raw()))
}
