//! Stream framing: `u32` little-endian length prefix, then a postcard-encoded message.
//!
//! Postcard is compact, deterministic, `no_std`-friendly and schema-less; the schema is the Rust
//! type, pinned by snapshot tests. The length prefix lets a reader allocate exactly once and
//! reject oversize frames before decoding.

use bytes::{Buf, BufMut, Bytes, BytesMut};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Largest frame a peer will accept. A full 500×200 screen of styled cells is well under this.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Bytes of the length prefix.
pub const PREFIX_BYTES: usize = 4;

/// Framing errors.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    /// Encoding failed (should be impossible for our own types; surfaced rather than panicking).
    #[error("encode: {0}")]
    Encode(postcard::Error),
    /// Decoding failed: the bytes are not a valid message of the expected type.
    #[error("decode: {0}")]
    Decode(postcard::Error),
    /// The prefix announced a frame larger than [`MAX_FRAME_BYTES`].
    #[error("frame of {len} bytes exceeds the {max} byte limit")]
    TooLarge {
        /// Announced length.
        len: usize,
        /// Limit.
        max: usize,
    },
}

/// Encode one message with its length prefix.
pub fn encode<T: Serialize>(msg: &T) -> Result<Bytes, CodecError> {
    let body = postcard::to_allocvec(msg).map_err(CodecError::Encode)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(CodecError::TooLarge { len: body.len(), max: MAX_FRAME_BYTES });
    }
    // MAX_FRAME_BYTES < u32::MAX, so this cannot fail; saturate rather than unwrap.
    let len = u32::try_from(body.len()).unwrap_or(u32::MAX);
    let mut out = BytesMut::with_capacity(PREFIX_BYTES.saturating_add(body.len()));
    out.put_u32_le(len);
    out.put_slice(&body);
    Ok(out.freeze())
}

/// Encode a message body without a prefix (for datagrams and tests).
pub fn encode_body<T: Serialize>(msg: &T) -> Result<Vec<u8>, CodecError> {
    postcard::to_allocvec(msg).map_err(CodecError::Encode)
}

/// Decode a message body (no prefix).
pub fn decode_body<T: DeserializeOwned>(body: &[u8]) -> Result<T, CodecError> {
    postcard::from_bytes(body).map_err(CodecError::Decode)
}

/// Try to take one complete frame from the front of `buf`. Returns `Ok(None)` when more bytes
/// are needed; on success the frame is removed from `buf`.
pub fn try_decode<T: DeserializeOwned>(buf: &mut BytesMut) -> Result<Option<T>, CodecError> {
    if buf.len() < PREFIX_BYTES {
        return Ok(None);
    }
    let len = usize::try_from(u32::from_le_bytes(prefix(buf))).unwrap_or(usize::MAX);
    if len > MAX_FRAME_BYTES {
        return Err(CodecError::TooLarge { len, max: MAX_FRAME_BYTES });
    }
    let total = PREFIX_BYTES.saturating_add(len);
    if buf.len() < total {
        buf.reserve(total.saturating_sub(buf.len()));
        return Ok(None);
    }
    buf.advance(PREFIX_BYTES);
    let body = buf.split_to(len);
    decode_body(&body).map(Some)
}

fn prefix(buf: &BytesMut) -> [u8; PREFIX_BYTES] {
    let mut p = [0_u8; PREFIX_BYTES];
    p.copy_from_slice(buf.get(..PREFIX_BYTES).unwrap_or(&[0; PREFIX_BYTES]));
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_partial_reads() {
        let msg = (7_u32, String::from("hello"), vec![1_u8, 2, 3]);
        let bytes = encode(&msg).unwrap();
        let mut buf = BytesMut::new();
        // Feed one byte at a time; nothing decodes until the last byte lands.
        for (i, b) in bytes.iter().enumerate() {
            buf.put_u8(*b);
            let got: Option<(u32, String, Vec<u8>)> = try_decode(&mut buf).unwrap();
            if i + 1 < bytes.len() {
                assert!(got.is_none(), "decoded early at byte {i}");
            } else {
                assert_eq!(got, Some(msg.clone()));
            }
        }
        assert!(buf.is_empty(), "the frame was consumed");
    }

    #[test]
    fn oversize_prefix_is_rejected_before_allocation() {
        let mut buf = BytesMut::new();
        buf.put_u32_le(u32::MAX);
        let err = try_decode::<u8>(&mut buf).unwrap_err();
        assert!(matches!(err, CodecError::TooLarge { .. }));
    }

    #[test]
    fn two_frames_back_to_back() {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&encode(&1_u16).unwrap());
        buf.extend_from_slice(&encode(&2_u16).unwrap());
        assert_eq!(try_decode::<u16>(&mut buf).unwrap(), Some(1));
        assert_eq!(try_decode::<u16>(&mut buf).unwrap(), Some(2));
        assert_eq!(try_decode::<u16>(&mut buf).unwrap(), None);
    }
}
