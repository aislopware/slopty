//! Frames plus file descriptors over a tokio Unix stream.
//!
//! Ancillary data attaches to the first byte of a `sendmsg`; the receiver collects fds from every
//! `recvmsg` into a queue, and whoever decodes an `Attached` frame pops the next fd. Because a
//! frame cannot decode before all of its bytes (including the first) arrived, the fd is always
//! queued by the time its frame is.

use std::collections::VecDeque;
use std::io::{IoSlice, IoSliceMut};
use std::mem::MaybeUninit;
use std::os::fd::{BorrowedFd, OwnedFd};

use bytes::BytesMut;
use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, recvmsg, sendmsg,
};
use serde::de::DeserializeOwned;
use slopty_proto::codec::{self, CodecError};
use tokio::io::Interest;
use tokio::net::UnixStream;

use crate::PtyError;

/// Send `frame` (already length-prefixed), optionally with one fd on its first byte.
pub async fn send(
    stream: &UnixStream,
    frame: &[u8],
    fd: Option<BorrowedFd<'_>>,
) -> Result<(), PtyError> {
    send_parts(stream, &[frame], fd).await
}

/// Send one frame written as `parts`, one after the other, without joining them first,
/// optionally with one fd on its first byte.
pub async fn send_parts(
    stream: &UnixStream,
    parts: &[&[u8]],
    fd: Option<BorrowedFd<'_>>,
) -> Result<(), PtyError> {
    let total = parts.iter().fold(0_usize, |sum, part| sum.saturating_add(part.len()));
    let mut sent = 0_usize;
    while sent < total {
        let rest = unsent(parts, sent);
        let fds = fd.filter(|_| sent == 0).map(|f| [f]);
        let n = stream
            .async_io(Interest::WRITABLE, || {
                let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
                let mut cmsg = SendAncillaryBuffer::new(&mut space);
                if let Some(fds) = &fds {
                    cmsg.push(SendAncillaryMessage::ScmRights(fds));
                }
                sendmsg(stream, &rest, &mut cmsg, SendFlags::empty()).map_err(std::io::Error::from)
            })
            .await
            .map_err(|e| PtyError::os("sendmsg", e))?;
        sent = sent.saturating_add(n);
    }
    Ok(())
}

/// What is left of `parts` once `sent` bytes of them went.
fn unsent<'a>(parts: &[&'a [u8]], sent: usize) -> Vec<IoSlice<'a>> {
    let mut skip = sent;
    let mut rest = Vec::with_capacity(parts.len());
    for part in parts {
        if skip >= part.len() {
            skip = skip.saturating_sub(part.len());
            continue;
        }
        rest.push(IoSlice::new(part.get(skip..).unwrap_or_default()));
        skip = 0;
    }
    rest
}

/// A received fd is close-on-exec from the moment it arrives where the kernel can do that
/// (`MSG_CMSG_CLOEXEC` on Linux), so a fork by another thread never hands it to that child.
/// macOS has no such flag, and [`Inbox::recv`] sets it right after instead.
#[cfg(target_os = "linux")]
const RECV_FLAGS: RecvFlags = RecvFlags::CMSG_CLOEXEC;
#[cfg(not(target_os = "linux"))]
const RECV_FLAGS: RecvFlags = RecvFlags::empty();

/// Socket buffer each way. macOS gives a Unix stream 8 KiB, which cut a 3.5 MB checkpoint into
/// some 430 reads; the tap stream carries every byte a session prints and its checkpoints.
const SOCKET_BUFFER: usize = 1 << 20;

/// Most bytes one `recvmsg` takes.
const READ_CHUNK: usize = 256 << 10;

/// Raise both socket buffers of a worker↔ptyd connection to `SOCKET_BUFFER`. A connection
/// whose peer already hung up refuses (`EINVAL`), and it has nothing left to carry anyway.
pub fn widen_buffers(stream: &UnixStream) {
    let widened = rustix::net::sockopt::set_socket_send_buffer_size(stream, SOCKET_BUFFER)
        .and_then(|()| rustix::net::sockopt::set_socket_recv_buffer_size(stream, SOCKET_BUFFER));
    if let Err(e) = widened {
        tracing::debug!(error = %e, "socket buffers left as they are");
    }
}

/// What a connection has received and not decoded yet: the bytes, and the fds that came with
/// them.
///
/// `recvmsg` reads into a scratch buffer kept for the connection's life, so a read costs a
/// copy of what arrived and nothing more.
#[derive(Debug)]
pub struct Inbox {
    buf: BytesMut,
    fds: VecDeque<OwnedFd>,
    scratch: Box<[u8]>,
}

/// Descriptors one `recvmsg` of a worker↔ptyd connection has room for: a frame carries one,
/// and a request waits for its answer.
const ROOM: usize = 4;

impl Default for Inbox {
    fn default() -> Self {
        Self {
            buf: BytesMut::with_capacity(READ_CHUNK),
            fds: VecDeque::new(),
            scratch: vec![0; READ_CHUNK].into_boxed_slice(),
        }
    }
}

impl Inbox {
    /// The next whole frame received, if one is.
    pub fn decode<T: DeserializeOwned>(&mut self) -> Result<Option<T>, CodecError> {
        codec::try_decode(&mut self.buf)
    }

    /// The oldest fd received and not taken yet: the one on the frame just decoded, when that
    /// frame carries one.
    pub fn take_fd(&mut self) -> Option<OwnedFd> {
        self.fds.pop_front()
    }

    /// Nothing received waits to be decoded: no frame is part way in, so no fd received waits
    /// for the rest of its frame.
    #[must_use]
    pub fn idle(&self) -> bool {
        self.buf.is_empty()
    }

    /// Close every fd received and not taken.
    pub fn close_fds(&mut self) {
        self.fds.clear();
    }

    /// Receive once, appending to the buffered bytes and descriptors. Returns bytes read; 0 means
    /// EOF.
    ///
    /// # Errors
    ///
    /// A failed read, and one that brought more descriptors than the inbox has room for: those
    /// are closed, and the frames they rode on would be taken without them.
    pub async fn recv(&mut self, stream: &UnixStream) -> Result<usize, PtyError> {
        let scratch = &mut self.scratch;
        let (n, received, cut) = stream
            .async_io(Interest::READABLE, || {
                let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(ROOM))];
                let mut cmsg = RecvAncillaryBuffer::new(&mut space);
                let mut iov = [IoSliceMut::new(scratch)];
                let msg = recvmsg(stream, &mut iov, &mut cmsg, RECV_FLAGS)
                    .map_err(std::io::Error::from)?;
                let mut got: Vec<OwnedFd> = Vec::new();
                for message in cmsg.drain() {
                    if let RecvAncillaryMessage::ScmRights(fds) = message {
                        got.extend(fds);
                    }
                }
                Ok((msg.bytes, got, msg.flags.contains(ReturnFlags::CTRUNC)))
            })
            .await
            .map_err(|e| PtyError::os("recvmsg", e))?;
        if cut {
            return Err(PtyError::os(
                "recvmsg",
                std::io::Error::other("more descriptors came than there was room for"),
            ));
        }
        self.buf.extend_from_slice(self.scratch.get(..n).unwrap_or_default());
        for fd in received {
            if RECV_FLAGS.is_empty() {
                rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC)
                    .map_err(|e| PtyError::os("fcntl", e))?;
            }
            self.fds.push_back(fd);
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd as _;

    use super::*;

    /// A descriptor comes through with the bytes it was sent on, close-on-exec on arrival.
    #[tokio::test]
    async fn a_received_descriptor_closes_on_exec() {
        let (sender, receiver) = UnixStream::pair().unwrap();
        let file = std::fs::File::open("/dev/null").unwrap();
        send(&sender, b"x", Some(file.as_fd())).await.unwrap();
        let mut inbox = Inbox::default();
        assert_eq!(inbox.recv(&receiver).await.unwrap(), 1);
        let fd = inbox.take_fd().expect("the descriptor sent");
        let flags = rustix::io::fcntl_getfd(&fd).unwrap();
        assert!(flags.contains(rustix::io::FdFlags::CLOEXEC), "{flags:?}");
        assert!(inbox.take_fd().is_none(), "one sent, one received");
    }
}
