//! Typed messages over QUIC streams: u32 length prefix + postcard, as in [`slopty_proto::codec`].

use std::marker::PhantomData;

use bytes::{Bytes, BytesMut};
use noq::{RecvStream, SendStream};
use serde::Serialize;
use serde::de::DeserializeOwned;
use slopty_proto::codec;
use tokio::io::AsyncReadExt as _;

use crate::NetError;

/// Sending half.
#[derive(Debug)]
pub struct FramedSend<T> {
    stream: SendStream,
    _t: PhantomData<fn(T)>,
}

/// Receiving half.
#[derive(Debug)]
pub struct FramedRecv<T> {
    stream: RecvStream,
    buf: BytesMut,
    _t: PhantomData<fn() -> T>,
}

impl<T: Serialize> FramedSend<T> {
    /// Wrap a stream.
    #[must_use]
    pub const fn new(stream: SendStream) -> Self {
        Self { stream, _t: PhantomData }
    }

    /// Encode and write one message.
    pub async fn send(&mut self, msg: &T) -> Result<(), NetError> {
        self.send_raw(codec::encode(msg)?).await
    }

    /// Write a pre-encoded frame (fan-out: encode once, send to many). The stream keeps the
    /// buffer itself until the peer acknowledges it, rather than a copy.
    pub async fn send_raw(&mut self, frame: Bytes) -> Result<(), NetError> {
        self.stream.write_chunk(frame).await.map_err(|e| NetError::stream(&e))
    }

    /// Reuse the stream for a different message type (after a header).
    #[must_use]
    pub fn retype<U>(self) -> FramedSend<U> {
        FramedSend { stream: self.stream, _t: PhantomData }
    }

    /// The stream itself, for raw bytes after a header.
    #[must_use]
    pub fn into_inner(self) -> SendStream {
        self.stream
    }

    /// Finish the stream gracefully.
    pub fn finish(&mut self) -> Result<(), NetError> {
        self.stream.finish().map_err(|e| NetError::stream(&e))
    }

    /// Set the send priority of what is written next: noq sends the highest first and
    /// round-robins equal ones a packet each.
    pub fn set_priority(&self, priority: i32) -> Result<(), NetError> {
        self.stream.set_priority(priority).map_err(|e| NetError::stream(&e))
    }

    /// The send priority of what is written next.
    pub fn priority(&self) -> Result<i32, NetError> {
        self.stream.priority().map_err(|e| NetError::stream(&e))
    }
}

impl<T: DeserializeOwned> FramedRecv<T> {
    /// Wrap a stream.
    #[must_use]
    pub fn new(stream: RecvStream) -> Self {
        Self { stream, buf: BytesMut::with_capacity(64 << 10), _t: PhantomData }
    }

    /// Reuse the stream for a different message type (after a header). Buffered bytes carry over.
    #[must_use]
    pub fn retype<U>(self) -> FramedRecv<U> {
        FramedRecv { stream: self.stream, buf: self.buf, _t: PhantomData }
    }

    /// Raw bytes from here on, the buffered ones first.
    #[must_use]
    pub fn into_raw(self) -> crate::streams::RawRecv {
        crate::streams::RawRecv::new(self.buf, self.stream)
    }

    /// Read the next message; `Err(Closed)` at a clean end of stream.
    pub async fn recv(&mut self) -> Result<T, NetError> {
        self.recv_unless(|_| false).await
    }

    /// Read the next message `skip` does not pass over; `skip` sees each body before it is
    /// decoded, so a message the reader already has costs no decode. Cancel-safe, as
    /// [`Self::recv`] is: a message skipped was never wanted.
    pub async fn recv_unless(&mut self, skip: impl Fn(&[u8]) -> bool) -> Result<T, NetError> {
        loop {
            while let Some(body) = codec::try_take(&mut self.buf)? {
                if !skip(&body) {
                    return Ok(codec::decode_body(&body)?);
                }
            }
            let n = self.stream.read_buf(&mut self.buf).await.map_err(|e| NetError::stream(&e))?;
            if n == 0 {
                return Err(NetError::Closed);
            }
        }
    }
}
