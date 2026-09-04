//! Frames plus file descriptors over a tokio Unix stream.
//!
//! Ancillary data attaches to the first byte of a `sendmsg`; the receiver collects fds from every
//! `recvmsg` into a queue, and whoever decodes an `Attached` frame pops the next fd. Because a
//! frame cannot decode before all of its bytes (including the first) arrived, the fd is always
//! queued by the time its frame is.

use std::collections::VecDeque;
use std::io::{IoSlice, IoSliceMut};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd as _, OwnedFd};

use bytes::BytesMut;
use nix::sys::socket::{ControlMessage, ControlMessageOwned, MsgFlags, UnixAddr, recvmsg, sendmsg};
use tokio::io::Interest;
use tokio::net::UnixStream;

use crate::PtyError;

/// Send `frame` (already length-prefixed), optionally with one fd on its first byte.
pub async fn send(
    stream: &UnixStream,
    frame: &[u8],
    fd: Option<BorrowedFd<'_>>,
) -> Result<(), PtyError> {
    let mut sent = 0_usize;
    while sent < frame.len() {
        let rest = frame.get(sent..).unwrap_or_default();
        let fds = fd.filter(|_| sent == 0).map(|f| [f.as_raw_fd()]);
        let cmsg: Vec<ControlMessage<'_>> =
            fds.iter().map(|f| ControlMessage::ScmRights(f)).collect();
        let n = stream
            .async_io(Interest::WRITABLE, || {
                sendmsg::<UnixAddr>(
                    stream.as_raw_fd(),
                    &[IoSlice::new(rest)],
                    &cmsg,
                    MsgFlags::empty(),
                    None,
                )
                .map_err(std::io::Error::from)
            })
            .await
            .map_err(|e| PtyError::os("sendmsg", e))?;
        sent = sent.saturating_add(n);
    }
    Ok(())
}

/// Receive into `buf`, appending any fds to `fds`. Returns bytes read; 0 means EOF.
pub async fn recv(
    stream: &UnixStream,
    buf: &mut BytesMut,
    fds: &mut VecDeque<OwnedFd>,
) -> Result<usize, PtyError> {
    buf.reserve(64 << 10);
    let spare = buf.spare_capacity_mut();
    let mut scratch = vec![0_u8; spare.len()];
    let mut cmsg_buf = nix::cmsg_space!([std::os::fd::RawFd; 4]);
    let (n, received) = stream
        .async_io(Interest::READABLE, || {
            let mut iov = [IoSliceMut::new(&mut scratch)];
            let msg = recvmsg::<UnixAddr>(
                stream.as_raw_fd(),
                &mut iov,
                Some(&mut cmsg_buf),
                MsgFlags::empty(),
            )
            .map_err(std::io::Error::from)?;
            let mut got = Vec::new();
            if let Ok(cmsgs) = msg.cmsgs() {
                for c in cmsgs {
                    if let ControlMessageOwned::ScmRights(list) = c {
                        got.extend(list);
                    }
                }
            }
            Ok((msg.bytes, got))
        })
        .await
        .map_err(|e| PtyError::os("recvmsg", e))?;
    buf.extend_from_slice(scratch.get(..n).unwrap_or_default());
    for raw in received {
        // SAFETY: the kernel just created this descriptor for us via SCM_RIGHTS; nothing else
        // owns it.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        // macOS has no MSG_CMSG_CLOEXEC; close the race window by hand.
        rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC)
            .map_err(|e| PtyError::os("fcntl", e))?;
        fds.push_back(fd);
    }
    Ok(n)
}
