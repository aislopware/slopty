//! Datagrams and the small fixed-layout parsers: each input is one datagram (or one value) as
//! the receiving side gets it.

use slopty_media::parse_cursor;
use slopty_proto::datagram::{Channel, ClientDatagram, split_term_datagram};
use slopty_proto::media::{CURSOR_BYTES, FramePrefix, HEADER_BYTES, MediaHeader};
use slopty_proto::terminal::{TermEvent, frame_head};
use slopty_proto::transfer::{origin_bytes, parse_origin};

use crate::stream::round_trips;

/// The worker reading a client's datagram: it decodes to what it encodes back to.
pub fn client_datagram(data: &[u8]) {
    let Some(decoded) = ClientDatagram::decode(data) else { return };
    let once = decoded.encode().expect("a decoded datagram encodes");
    let back = ClientDatagram::decode(&once).expect("an encoded datagram decodes");
    assert_eq!(
        back.encode().expect("a decoded datagram encodes"),
        once,
        "a client datagram did not encode back to the same bytes"
    );
}

/// A client reading a worker's terminal datagram: the session, the frame head it reads to drop
/// a second copy without decoding the rows, and the event itself. The head must agree with the
/// event it heads.
pub fn term_datagram(data: &[u8]) {
    let Some((_session, body)) = split_term_datagram(data) else { return };
    let head = frame_head(body);
    let Ok(event) = crate::stream::decode::<TermEvent>(body) else { return };
    round_trips(&event);
    match &event {
        TermEvent::Frame(frame) => {
            assert_eq!(head, Some((frame.seq, frame.full)), "the frame head disagrees with it");
        }
        _ => assert_eq!(head, None, "a frame head read from another event"),
    }
}

/// A client's first look at a media datagram: the fixed header, then the frame prefix a
/// reassembled body opens with.
pub fn media_header(data: &[u8]) {
    let parsed = MediaHeader::parse(data);
    let on_media = Channel::of(data) == Some(Channel::Media);
    assert_eq!(
        parsed.is_some(),
        on_media && data.len() >= HEADER_BYTES,
        "a header parsed off the media channel or from too few bytes"
    );
    if let Some((header, payload)) = parsed {
        assert_eq!(payload.len(), data.len().saturating_sub(HEADER_BYTES), "payload length");
        let _kind = header.kind();
        let _parity = header.is_parity();
    }
    if let Some((_prefix, rest)) = FramePrefix::parse(data) {
        assert!(rest.len() < data.len(), "the prefix took no bytes");
    }
}

/// A client reading a cursor datagram's payload.
pub fn cursor(data: &[u8]) {
    assert_eq!(
        parse_cursor(data).is_some(),
        data.len() >= CURSOR_BYTES,
        "a cursor parsed from too few bytes, or not from enough"
    );
}

/// Either end reading the origin marker on a pasteboard it did not write.
pub fn origin(data: &[u8]) {
    let Some((peer, generation)) = parse_origin(data) else { return };
    assert_eq!(
        parse_origin(&origin_bytes(peer, generation)),
        Some((peer, generation)),
        "an origin did not survive its own encoding"
    );
}
