//! Datagrams: media, loss feedback, and the copies that race a keystroke and its echo past a
//! lost packet.
//!
//! Every datagram, either way, opens with one [`Channel`] byte naming what follows, so a kind of
//! datagram is a channel of its own and never borrows another's header:
//!
//! * [`Channel::Media`], worker → client: the rest of a [`crate::media::MediaHeader`] (whose first
//!   byte the channel is) and its payload.
//! * [`Channel::Term`], worker → client: a copy of a session-stream event, [`TermDatagram`]'s
//!   fields in postcard.
//! * [`Channel::Feedback`], [`Channel::Input`] and [`Channel::ScreenInput`], client → worker: a
//!   [`ClientDatagram`]'s fields in postcard.
//!
//! A keystroke is one small packet on the control stream and its echo one small frame on the
//! session stream. When either packet is lost nothing behind it tells QUIC so, and the stream
//! waits for the probe timeout. Each may therefore also go once as a datagram, a few
//! milliseconds after the stream copy so the two never share a packet. Whichever copy arrives
//! first is used, and only in order: a copy is taken only when it is the very next one, and
//! anything else waits for its stream copy, which always comes.
//!
//! A window stream's input goes the same way, client to worker: a click or a key behind a lost
//! packet waited for QUIC's recovery, and so did every move after it, for every stream on the
//! connection. A move only says where the pointer is now, so its copy may overtake older moves;
//! everything else applies strictly in turn ([`ClientDatagram::ScreenInput`]).

use bytes::{BufMut as _, Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use slopty_core::{SessionId, StreamId};

use crate::codec::{self, CodecError};
#[cfg(doc)]
use crate::screen::ScreenRequest;
use crate::screen::{Feedback, ScreenInput};
use crate::terminal::{TermEvent, TermRequest};

/// The first byte of every datagram: what the rest is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Channel {
    /// Worker → client: video, audio, the cursor, heartbeats ([`crate::media`]).
    Media = 0,
    /// Worker → client: a copy of a session-stream frame ([`TermDatagram`]).
    Term = 1,
    /// Client → worker: loss feedback for a screen stream ([`ClientDatagram::Feedback`]).
    Feedback = 2,
    /// Client → worker: a copy of a terminal input ([`ClientDatagram::Input`]).
    Input = 3,
    /// Client → worker: a copy of a window stream's input ([`ClientDatagram::ScreenInput`]).
    ScreenInput = 4,
}

impl Channel {
    /// The channel `datagram` is on; `None` for an empty one or an unknown byte.
    #[must_use]
    pub fn of(datagram: &[u8]) -> Option<Self> {
        match datagram.first()? {
            0 => Some(Self::Media),
            1 => Some(Self::Term),
            2 => Some(Self::Feedback),
            3 => Some(Self::Input),
            4 => Some(Self::ScreenInput),
            _ => None,
        }
    }
}

/// Everything a client sends as a datagram.
#[derive(Clone, PartialEq, Debug)]
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
    /// A copy of input `seq` for window stream `stream`.
    ///
    /// Both ends number the requests [`ScreenRequest::numbered`] names (input and quality
    /// changes) per stream in control-stream order, from 1: that is `seq`. `ordered` counts
    /// those up to this one that apply only in their turn (everything but a move), this one
    /// included. The worker applies a copy of an in-order input only when it is the next
    /// in-order one not yet applied; a move, when every in-order input before it is applied and
    /// nothing newer is. The stream copy of an input already applied is skipped, and so is one
    /// of a move older than an input applied. A quality change and a paste chord have no copy,
    /// so input behind either waits for them.
    ScreenInput {
        /// The stream.
        stream: StreamId,
        /// Its place among the stream's numbered requests on this connection.
        seq: u64,
        /// The stream's in-order requests up to this one.
        ordered: u64,
        /// The input, as the control stream carries it.
        input: ScreenInput,
    },
}

/// The fields of a [`ClientDatagram::Input`], as postcard writes them after the channel byte.
#[derive(Serialize, Deserialize)]
struct InputFields<R> {
    session: SessionId,
    seq: u64,
    req: R,
}

/// The fields of a [`ClientDatagram::ScreenInput`], as postcard writes them after the channel
/// byte.
#[derive(Serialize, Deserialize)]
struct ScreenInputFields<I> {
    stream: StreamId,
    seq: u64,
    ordered: u64,
    input: I,
}

impl ClientDatagram {
    /// The datagram: its channel byte, then its fields in postcard.
    ///
    /// # Errors
    /// When postcard cannot encode it (never for these types).
    pub fn encode(&self) -> Result<Bytes, CodecError> {
        let (channel, fields) = match self {
            Self::Feedback(feedback) => (Channel::Feedback, codec::encode_body(feedback)?),
            Self::Input { session, seq, req } => (
                Channel::Input,
                codec::encode_body(&InputFields { session: *session, seq: *seq, req })?,
            ),
            Self::ScreenInput { stream, seq, ordered, input } => (
                Channel::ScreenInput,
                codec::encode_body(&ScreenInputFields {
                    stream: *stream,
                    seq: *seq,
                    ordered: *ordered,
                    input,
                })?,
            ),
        };
        let mut out = BytesMut::with_capacity(fields.len().saturating_add(1));
        out.put_u8(channel as u8);
        out.put_slice(&fields);
        Ok(out.freeze())
    }

    /// A client's datagram; `None` on a worker → client channel, or when it does not decode.
    #[must_use]
    pub fn decode(datagram: &[u8]) -> Option<Self> {
        let fields = datagram.get(1..)?;
        match Channel::of(datagram)? {
            Channel::Feedback => codec::decode_body(fields).ok().map(Self::Feedback),
            Channel::Input => {
                let InputFields { session, seq, req } = codec::decode_body(fields).ok()?;
                Some(Self::Input { session, seq, req })
            }
            Channel::ScreenInput => {
                let ScreenInputFields { stream, seq, ordered, input } =
                    codec::decode_body(fields).ok()?;
                Some(Self::ScreenInput { stream, seq, ordered, input })
            }
            Channel::Media | Channel::Term => None,
        }
    }
}

/// A copy of one session-stream event, after a [`Channel::Term`] byte.
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

/// Bytes a [`Channel::Term`] datagram adds to an event's body: the channel byte and the
/// session (a UUID, which postcard writes as a length byte and its 16 bytes).
pub const TERM_OVERHEAD: usize = 1 + 17;

/// A [`Channel::Term`] datagram carrying `session` and an event already encoded for the
/// session stream.
///
/// `event` is its postcard body, the stream's length prefix taken off. Postcard encodes a
/// struct as its fields one after another, so this is a [`TermDatagram`] without decoding and
/// encoding the event again.
///
/// # Errors
/// When postcard cannot encode the session (never).
pub fn term_datagram(session: SessionId, event: &[u8]) -> Result<Bytes, CodecError> {
    let session = codec::encode_body(&session)?;
    let mut out =
        BytesMut::with_capacity(session.len().saturating_add(event.len()).saturating_add(1));
    out.put_u8(Channel::Term as u8);
    out.put_slice(&session);
    out.put_slice(event);
    Ok(out.freeze())
}

/// The [`TermDatagram`] in a worker's datagram, or `None` when it is media or does not decode.
#[cfg(test)]
fn parse_term_datagram(datagram: &[u8]) -> Option<TermDatagram> {
    (Channel::of(datagram)? == Channel::Term)
        .then(|| codec::decode_body(datagram.get(1..)?).ok())
        .flatten()
}

/// A worker's term datagram as its session and the event's postcard body, undecoded.
///
/// `None` when it is on another channel or the session does not decode. Read the event with
/// [`codec::decode_body`]; a copy the stream already brought is dropped from the body's head
/// ([`crate::terminal::frame_head`]), without decoding its rows.
#[must_use]
pub fn split_term_datagram(datagram: &[u8]) -> Option<(SessionId, &[u8])> {
    if Channel::of(datagram)? != Channel::Term {
        return None;
    }
    postcard::take_from_bytes::<SessionId>(datagram.get(1..)?).ok()
}

#[cfg(test)]
mod tests {
    use slopty_grid::{Cursor, CursorShape, LineIndex, TermModes};
    use zerocopy::IntoBytes as _;

    use super::*;
    use crate::input::{Mods, MouseButton};
    use crate::media::{HEADER_BYTES, Kind, MediaHeader};
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
            above: None,
            blocks: None,
            updates: Vec::new(),
            images: Vec::new(),
        })
    }

    fn heartbeat() -> MediaHeader {
        MediaHeader {
            channel: Channel::Media as u8,
            stream: 3.into(),
            frame: 1.into(),
            index: 0.into(),
            data_count: 0.into(),
            parity_count: 0,
            kind: Kind::Heartbeat as u8,
            flags: 0,
            send_ms_lo: 0,
        }
    }

    /// The worker builds the datagram from the bytes it already wrote to the session stream;
    /// the client decodes it as the struct. Both must agree byte for byte.
    #[test]
    fn a_stream_event_and_its_session_make_a_term_datagram() {
        let session = SessionId::new();
        let event = frame(9);
        let wire = codec::encode(&event).unwrap();
        let body = &wire[codec::PREFIX_BYTES..];
        let datagram = term_datagram(session, body).unwrap();
        let whole = codec::encode_body(&TermDatagram { session, event: event.clone() }).unwrap();
        assert_eq!(datagram[0], Channel::Term as u8);
        assert_eq!(&datagram[1..], &*whole);
        assert_eq!(datagram.len(), TERM_OVERHEAD + body.len(), "one byte, not a media header");
        assert_eq!(parse_term_datagram(&datagram), Some(TermDatagram { session, event }));
    }

    /// A copy is split into its session and body, and the body's head read, without decoding
    /// the rows; any other event has no frame head.
    #[test]
    fn a_copy_is_read_to_its_frame_number_without_decoding_it() {
        let session = SessionId::new();
        let body = codec::encode_body(&frame(300)).unwrap();
        let datagram = term_datagram(session, &body).unwrap();
        let (got, event) = split_term_datagram(&datagram).unwrap();
        assert_eq!((got, event), (session, &*body));
        assert_eq!(crate::terminal::frame_head(event), Some((300, false)));
        // The head alone is enough: the rows past it are not read.
        assert_eq!(crate::terminal::frame_head(&event[..4]), Some((300, false)));
        let bell = codec::encode_body(&TermEvent::Bell).unwrap();
        assert_eq!(crate::terminal::frame_head(&bell), None);
    }

    #[test]
    fn media_and_garbage_are_not_term_datagrams() {
        assert_eq!(parse_term_datagram(heartbeat().as_bytes()), None);
        assert_eq!(split_term_datagram(heartbeat().as_bytes()), None);
        let mut cut = term_datagram(SessionId::new(), &codec::encode_body(&frame(1)).unwrap())
            .unwrap()
            .to_vec();
        cut.truncate(cut.len().saturating_sub(3));
        assert_eq!(parse_term_datagram(&cut), None, "a truncated copy is dropped");
        assert_eq!(parse_term_datagram(&[0; 4]), None);
        assert_eq!(split_term_datagram(&[]), None);
    }

    /// Each channel's datagram is read back as what was sent, on its own channel and no other:
    /// a media header is not a client's datagram, and a client's is neither media nor a copy.
    #[test]
    fn a_datagram_of_each_channel_round_trips() {
        let media = heartbeat();
        let (parsed, payload) = MediaHeader::parse(media.as_bytes()).unwrap();
        assert_eq!((parsed, payload.len()), (&media, 0));
        assert_eq!(media.as_bytes().len(), HEADER_BYTES);
        assert_eq!(Channel::of(media.as_bytes()), Some(Channel::Media));
        assert_eq!(ClientDatagram::decode(media.as_bytes()), None);

        let session = SessionId::new();
        let body = codec::encode_body(&frame(4)).unwrap();
        let term = term_datagram(session, &body).unwrap();
        assert_eq!(Channel::of(&term), Some(Channel::Term));
        assert_eq!(parse_term_datagram(&term), Some(TermDatagram { session, event: frame(4) }));
        assert_eq!(MediaHeader::parse(&term), None);
        assert_eq!(ClientDatagram::decode(&term), None);

        let refresh = Feedback::Refresh { stream: StreamId(2), last_good_frame: 5, keyframe: true };
        let raw = TermRequest::Raw(b"a".to_vec());
        let sent = [
            (Channel::Feedback, ClientDatagram::Feedback(refresh)),
            (Channel::Input, ClientDatagram::Input { session, seq: 7, req: raw }),
            (
                Channel::ScreenInput,
                ClientDatagram::ScreenInput {
                    stream: StreamId(2),
                    seq: 9,
                    ordered: 4,
                    input: ScreenInput::Move { x: 3.0, y: -1.5 },
                },
            ),
            (
                Channel::ScreenInput,
                ClientDatagram::ScreenInput {
                    stream: StreamId(2),
                    seq: 10,
                    ordered: 5,
                    input: ScreenInput::Button {
                        button: MouseButton::Left,
                        down: true,
                        x: 1.0,
                        y: 2.0,
                        clicks: 1,
                        mods: Mods::empty(),
                    },
                },
            ),
        ];
        for (channel, datagram) in sent {
            let wire = datagram.encode().unwrap();
            assert_eq!(Channel::of(&wire), Some(channel), "{datagram:?}");
            assert_eq!(ClientDatagram::decode(&wire), Some(datagram));
            assert_eq!(MediaHeader::parse(&wire), None);
            assert_eq!(split_term_datagram(&wire), None);
        }
        assert_eq!(ClientDatagram::decode(&[9, 0, 0]), None, "an unknown channel");
        assert_eq!(ClientDatagram::decode(&[Channel::Input as u8, 1]), None, "a cut one");
    }
}
