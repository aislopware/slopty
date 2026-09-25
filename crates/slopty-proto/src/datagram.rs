//! Datagrams that are not media: loss feedback, and the copies that race a keystroke and its
//! echo past a lost packet.
//!
//! A keystroke is one small packet on the control stream and its echo one small frame on the
//! session stream. When either packet is lost nothing behind it tells QUIC so, and the stream
//! waits for the probe timeout. Each may therefore also go once as a datagram, a few
//! milliseconds after the stream copy so the two never share a packet. Whichever copy arrives
//! first is used, and only in order: a copy is taken only when it is the very next one, and
//! anything else waits for its stream copy, which always comes.
//!
//! * Client → worker: [`ClientDatagram`], postcard, no prefix.
//! * Worker → client: a [`crate::media::MediaHeader`] of kind [`crate::media::Kind::Term`], then a
//!   [`TermDatagram`] in postcard.

use bytes::{BufMut as _, Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use slopty_core::SessionId;
use zerocopy::IntoBytes as _;

use crate::codec::{self, CodecError};
use crate::media::{HEADER_BYTES, Kind, MediaHeader};
use crate::screen::Feedback;
use crate::terminal::{TermEvent, TermRequest};

/// Everything a client sends as a datagram.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ClientDatagram {
    /// Loss feedback for a screen stream.
    Feedback(Feedback),
    /// A copy of input `seq` for `session`: the `seq`-th request with
    /// [`TermRequest::is_input`] this connection's control stream carries for that session,
    /// counted from 1. The worker applies it only when it is the next input it has not yet
    /// applied; its stream copy is then skipped.
    Input {
        /// The session.
        session: SessionId,
        /// Its place among the session's inputs on this connection.
        seq: u64,
        /// The request, as the control stream carries it.
        req: TermRequest,
    },
}

/// A copy of one session-stream event, after a [`Kind::Term`] header.
///
/// Only frames are copied: a diff that answers input and fits one datagram. The client applies
/// it only when its sequence number follows the last frame applied, and drops the stream copy
/// after it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TermDatagram {
    /// The session.
    pub session: SessionId,
    /// The event.
    pub event: TermEvent,
}

/// A [`Kind::Term`] datagram carrying `session` and an event already encoded for the session
/// stream.
///
/// `event` is its postcard body, the stream's length prefix taken off. Postcard encodes a
/// struct as its fields one after another, so this is a [`TermDatagram`] without decoding and
/// encoding the event again.
pub fn term_datagram(session: SessionId, event: &[u8]) -> Result<Bytes, CodecError> {
    let session = codec::encode_body(&session)?;
    let header = MediaHeader {
        stream: 0.into(),
        frame: 0.into(),
        index: 0.into(),
        data_count: 0.into(),
        parity_count: 0,
        kind: Kind::Term as u8,
        flags: 0,
        send_ms_lo: 0,
    };
    let len = HEADER_BYTES.saturating_add(session.len()).saturating_add(event.len());
    let mut out = BytesMut::with_capacity(len);
    out.put_slice(header.as_bytes());
    out.put_slice(&session);
    out.put_slice(event);
    Ok(out.freeze())
}

/// The [`TermDatagram`] in a worker's datagram, or `None` when it is media or does not decode.
#[must_use]
pub fn parse_term_datagram(datagram: &[u8]) -> Option<TermDatagram> {
    let (header, payload) = MediaHeader::parse(datagram)?;
    (header.kind() == Some(Kind::Term)).then(|| codec::decode_body(payload).ok()).flatten()
}

#[cfg(test)]
mod tests {
    use slopty_grid::{Cursor, CursorShape, LineIndex, TermModes};

    use super::*;
    use crate::terminal::Frame;

    fn frame(seq: u64) -> TermEvent {
        TermEvent::Frame(Frame {
            seq,
            full: false,
            epoch: 1,
            cols: 4,
            rows: 1,
            cursor: Cursor {
                row: 0,
                col: 1,
                shape: CursorShape::Block,
                visible: true,
                blink: false,
            },
            modes: TermModes::empty(),
            oldest_line: LineIndex(0),
            first_visible_line: LineIndex(0),
            total_lines: 1,
            input_ack: 2,
            updates: Vec::new(),
            images: Vec::new(),
        })
    }

    /// The worker builds the datagram from the bytes it already wrote to the session stream;
    /// the client decodes it as the struct. Both must agree byte for byte.
    #[test]
    fn a_stream_event_and_its_session_make_a_term_datagram() {
        let session = SessionId::new();
        let event = frame(9);
        let wire = codec::encode(&event).unwrap();
        let datagram = term_datagram(session, &wire[codec::PREFIX_BYTES..]).unwrap();
        let whole = codec::encode_body(&TermDatagram { session, event: event.clone() }).unwrap();
        assert_eq!(&datagram[HEADER_BYTES..], &*whole);
        assert_eq!(parse_term_datagram(&datagram), Some(TermDatagram { session, event }));
    }

    #[test]
    fn media_and_garbage_are_not_term_datagrams() {
        let heartbeat = MediaHeader {
            stream: 3.into(),
            frame: 1.into(),
            index: 0.into(),
            data_count: 0.into(),
            parity_count: 0,
            kind: Kind::Heartbeat as u8,
            flags: 0,
            send_ms_lo: 0,
        };
        assert_eq!(parse_term_datagram(heartbeat.as_bytes()), None);
        let mut cut = term_datagram(SessionId::new(), &codec::encode_body(&frame(1)).unwrap())
            .unwrap()
            .to_vec();
        cut.truncate(cut.len().saturating_sub(3));
        assert_eq!(parse_term_datagram(&cut), None, "a truncated copy is dropped");
        assert_eq!(parse_term_datagram(&[0; 4]), None);
    }
}
