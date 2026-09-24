//! Typed messages over QUIC streams: u32 length prefix + postcard, as in [`slopty_proto::codec`].

use std::marker::PhantomData;

use bytes::BytesMut;
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
        let frame = codec::encode(msg)?;
        self.stream.write_all(&frame).await.map_err(|e| NetError::stream(&e))
    }

    /// Write pre-encoded frames (fan-out: encode once, send to many).
    pub async fn send_raw(&mut self, frame: &[u8]) -> Result<(), NetError> {
        self.stream.write_all(frame).await.map_err(|e| NetError::stream(&e))
    }

    /// Reuse the stream for a different message type (after a header).
    #[must_use]
    pub fn retype<U>(self) -> FramedSend<U> {
        FramedSend { stream: self.stream, _t: PhantomData }
    }

    /// Finish the stream gracefully.
    pub fn finish(&mut self) -> Result<(), NetError> {
        self.stream.finish().map_err(|e| NetError::stream(&e))
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

    /// Read the next message; `Err(Closed)` at a clean end of stream.
    pub async fn recv(&mut self) -> Result<T, NetError> {
        loop {
            if let Some(msg) = codec::try_decode::<T>(&mut self.buf)? {
                return Ok(msg);
            }
            let n = self.stream.read_buf(&mut self.buf).await.map_err(|e| NetError::stream(&e))?;
            if n == 0 {
                return Err(NetError::Closed);
            }
        }
    }
}
