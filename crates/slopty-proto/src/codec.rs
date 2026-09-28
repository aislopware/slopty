//! Stream framing: `u32` little-endian length prefix, then a postcard-encoded message.
//!
//! Postcard is compact, deterministic, `no_std`-friendly and schema-less; the schema is the Rust
//! type, pinned by snapshot tests. The length prefix lets a reader allocate exactly once and
//! reject oversize frames before decoding.

use std::cell::RefCell;

use bytes::{Buf as _, Bytes, BytesMut};
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

/// Frames up to this size are serialised in the thread's scratch buffer and copied out at their
/// exact size, so the thread keeps at most this much. A larger frame becomes the buffer it grew,
/// uncopied, and the thread's next frame starts a fresh one.
const SCRATCH_KEEP_BYTES: usize = 64 * 1024;

thread_local! {
    static SCRATCH: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Encode one message with its length prefix.
///
/// The message is serialised once, behind a reserved prefix, into a buffer the thread reuses,
/// then copied out at its exact size. Once that buffer has grown to hold a frame, the frame is
/// one allocation; a fresh buffer grown per frame took eight for a 230-byte echo. Sizing the
/// message first would also make it one, but walks the message twice: 58 % more instructions
/// for a full 200×60 screen (`docs/MEASUREMENTS.md`, "Encoding a frame"). A stream takes the
/// frame as a chunk (`SendStream::write_chunk`).
pub fn encode<T: Serialize>(msg: &T) -> Result<Bytes, CodecError> {
    SCRATCH.with(|scratch| match scratch.try_borrow_mut() {
        Ok(mut scratch) => encode_in(&mut scratch, msg),
        // Only a `Serialize` impl that itself encodes a frame gets here.
        Err(_) => encode_in(&mut Vec::new(), msg),
    })
}

fn encode_in<T: Serialize>(scratch: &mut Vec<u8>, msg: &T) -> Result<Bytes, CodecError> {
    let mut buf = std::mem::take(scratch);
    buf.clear();
    buf.extend_from_slice(&[0; PREFIX_BYTES]);
    let mut out = postcard::to_extend(msg, buf).map_err(CodecError::Encode)?;
    let len = out.len().saturating_sub(PREFIX_BYTES);
    if len > MAX_FRAME_BYTES {
        return Err(CodecError::TooLarge { len, max: MAX_FRAME_BYTES });
    }
    // MAX_FRAME_BYTES < u32::MAX, so this cannot fail; saturate rather than unwrap.
    let prefix = u32::try_from(len).unwrap_or(u32::MAX).to_le_bytes();
    if let Some(head) = out.get_mut(..PREFIX_BYTES) {
        head.copy_from_slice(&prefix);
    }
    if out.capacity() > SCRATCH_KEEP_BYTES {
        return Ok(Bytes::from(out));
    }
    let frame = Bytes::copy_from_slice(&out);
    *scratch = out;
    Ok(frame)
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
    try_take(buf)?.map(|body| decode_body(&body)).transpose()
}

/// Try to take one complete frame's body (the prefix off) from the front of `buf`, undecoded.
/// Returns `Ok(None)` when more bytes are needed; on success the frame is removed from `buf`.
pub fn try_take(buf: &mut BytesMut) -> Result<Option<BytesMut>, CodecError> {
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
    Ok(Some(buf.split_to(len)))
}

fn prefix(buf: &BytesMut) -> [u8; PREFIX_BYTES] {
    let mut p = [0_u8; PREFIX_BYTES];
    p.copy_from_slice(buf.get(..PREFIX_BYTES).unwrap_or(&[0; PREFIX_BYTES]));
    p
}

#[cfg(test)]
mod tests {
    use bytes::BufMut as _;

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

    /// A frame too big for the thread's scratch buffer leaves with that buffer, and the frames
    /// after it are encoded in a fresh one, each exactly its own size.
    #[test]
    fn frames_around_one_bigger_than_the_scratch_buffer() {
        let small = vec![7_u8; 100];
        let big = vec![9_u8; 2 * SCRATCH_KEEP_BYTES];
        let mut buf = BytesMut::new();
        for msg in [&small, &big, &small] {
            let frame = encode(msg).unwrap();
            assert!(frame.len() > msg.len(), "the prefix and the body");
            buf.extend_from_slice(&frame);
        }
        for msg in [&small, &big, &small] {
            assert_eq!(try_decode::<Vec<u8>>(&mut buf).unwrap().as_ref(), Some(msg));
        }
        assert!(buf.is_empty(), "every frame was consumed");
    }

    /// An image's pixels as the wire type held them before: a sequence of `u8`s.
    #[derive(Serialize, serde::Deserialize, PartialEq, Debug)]
    struct SeqPixels {
        width: u32,
        pixels: Vec<u8>,
    }

    /// The same, written as one byte string.
    #[derive(Serialize, serde::Deserialize, PartialEq, Debug)]
    struct BytePixels {
        width: u32,
        #[serde(with = "serde_bytes")]
        pixels: Vec<u8>,
    }

    /// A byte string is the wire a sequence of bytes was, both ways: a length, then the bytes.
    #[test]
    fn a_byte_string_is_the_wire_a_sequence_of_bytes_was() {
        let pixels: Vec<u8> = (0..=u8::MAX).cycle().take(1_000).collect();
        let seq = SeqPixels { width: 7, pixels: pixels.clone() };
        let bytes = BytePixels { width: 7, pixels };
        let wire = encode(&seq).unwrap();
        assert_eq!(wire, encode(&bytes).unwrap());
        assert_eq!(decode_body::<BytePixels>(&wire[PREFIX_BYTES..]).unwrap(), bytes);
        assert_eq!(decode_body::<SeqPixels>(&wire[PREFIX_BYTES..]).unwrap(), seq);
    }

    /// Encoding as it was: a body allocated on its own, then copied behind a prefix.
    fn encode_copied<T: Serialize>(msg: &T) -> Bytes {
        let body = postcard::to_allocvec(msg).unwrap();
        let mut out = BytesMut::with_capacity(PREFIX_BYTES.saturating_add(body.len()));
        out.put_u32_le(u32::try_from(body.len()).unwrap());
        out.put_slice(&body);
        out.freeze()
    }

    /// `min / median / max` in µs.
    fn spread(samples: &mut [std::time::Duration]) -> String {
        samples.sort_unstable();
        let us = |d: &std::time::Duration| d.as_secs_f64() * 1e6;
        let (min, mid, max) =
            (&samples[0], &samples[samples.len() / 2], &samples[samples.len().saturating_sub(1)]);
        format!("{:.2} / {:.2} / {:.0}", us(min), us(mid), us(max))
    }

    /// Time `a` and `b` in alternation, `rounds` each, so load on the machine falls on both.
    fn alternate(rounds: usize, mut a: impl FnMut(), mut b: impl FnMut()) -> (String, String) {
        let (mut ta, mut tb) = (Vec::new(), Vec::new());
        for _ in 0..rounds {
            let t = std::time::Instant::now();
            a();
            ta.push(t.elapsed());
            let t = std::time::Instant::now();
            b();
            tb.push(t.elapsed());
        }
        (spread(&mut ta), spread(&mut tb))
    }

    /// A 12 MiB kitty image (the largest the worker ships) encoded and decoded, its pixels as
    /// a sequence of `u8`s against one byte string. Prints the numbers MEASUREMENTS records:
    /// `cargo test -p slopty-proto --release --lib image_event_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "measurement, run by hand"]
    fn image_event_cost() {
        use crate::terminal::TermEvent;
        const WIDTH: u32 = 2048;
        const HEIGHT: u32 = 1536;
        let pixels: Vec<u8> = (0..=u8::MAX).cycle().take(12 << 20).collect();
        let seq = SeqPixels { width: WIDTH, pixels: pixels.clone() };
        let event =
            TermEvent::Image { id: 1, generation: 1, width: WIDTH, height: HEIGHT, bgra: pixels };
        let (before, after) = alternate(
            20,
            || {
                let wire = encode_copied(&seq);
                std::hint::black_box(decode_body::<SeqPixels>(&wire[PREFIX_BYTES..]).unwrap());
            },
            || {
                let wire = encode(&event).unwrap();
                std::hint::black_box(decode_body::<TermEvent>(&wire[PREFIX_BYTES..]).unwrap());
            },
        );
        println!(
            "MEASURE 12 MiB image, encode + decode µs (min / median / max, 20 each): \
             sequence of u8 {before} · byte string {after}"
        );
    }

    /// Frames encoded and handed to the stream as they were (a body, a copy behind the prefix,
    /// and the stream's copy of the slice) against once into the prefixed buffer, handed over
    /// shared: an echo's one row and a 200 × 60 screen. Prints the numbers
    /// MEASUREMENTS records:
    /// `cargo test -p slopty-proto --release --lib frame_encode_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "measurement, run by hand"]
    fn frame_encode_cost() {
        use slopty_grid::{Cursor, Line, RowUpdate, Style, TermModes};

        use crate::terminal::{Frame, TermEvent};
        let frame = |rows: u16| {
            let text = "fn main() { println!(\"hello, world\"); } // ".repeat(5);
            TermEvent::Frame(Frame {
                seq: 1,
                full: false,
                epoch: 0,
                cols: 200,
                rows: 60,
                cursor: Cursor::default(),
                modes: TermModes::empty(),
                oldest_line: slopty_grid::LineIndex(0),
                first_visible_line: slopty_grid::LineIndex(0),
                total_lines: 60,
                input_ack: 0,
                updates: (0..rows)
                    .map(|row| RowUpdate {
                        row,
                        line: Line::from_text(&text, 200, Style::DEFAULT).into(),
                    })
                    .collect(),
                images: Vec::new(),
            })
        };
        for (name, event, rounds) in
            [("echo, 1 row", frame(1), 20_000), ("screen, 60 rows", frame(60), 2_000)]
        {
            assert_eq!(encode_copied(&event), encode(&event).unwrap(), "the same frame");
            let (before, after) = alternate(
                rounds,
                // What a slice write made noq do with it (`Bytes::copy_from_slice`), against
                // the shared buffer a chunk write hands over.
                || {
                    let wire = encode_copied(&event);
                    std::hint::black_box((Bytes::copy_from_slice(&wire), wire));
                },
                || {
                    let wire = encode(&event).unwrap();
                    std::hint::black_box((wire.clone(), wire));
                },
            );
            println!(
                "MEASURE {name}: {} B, encode + hand-off µs (min / median / \
                 max, {rounds} each): copied twice {before} · once {after}",
                encode(&event).unwrap().len()
            );
        }
    }
}
