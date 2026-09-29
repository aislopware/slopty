//! Streams: the control stream both ways, the server's links, the unidirectional streams and the
//! worker's control socket.
//!
//! A stream's bytes arrive in pieces of any size. The first input byte picks the piece size,
//! and the rest is fed in pieces of that size through [`codec::try_take`], the reader
//! `slopty_net::framed::FramedRecv` runs, and each body is decoded as the reading side decodes
//! it. As there, the first frame that is too large or does not decode ends the stream.

use bytes::BytesMut;
use serde::Serialize;
use serde::de::DeserializeOwned;
use slopty_grid::Cell;
use slopty_proto::conversation::ConversationEvent;
use slopty_proto::ctl::{CtlReply, CtlRequest};
use slopty_proto::server::{FromServer, ToServer};
use slopty_proto::terminal::TermEvent;
use slopty_proto::transfer::UniHead;
use slopty_proto::{ClientMsg, WorkerMsg, codec};
use slopty_testkit::alloc;

/// The worker reading a client's control stream.
pub fn client_msg(data: &[u8]) {
    stream::<ClientMsg>(data);
}

/// A client reading a worker's control stream.
pub fn worker_msg(data: &[u8]) {
    stream::<WorkerMsg>(data);
}

/// The server reading a dialer's link (first byte even), or a dialer reading the server's.
pub fn server_msg(data: &[u8]) {
    let Some((&side, rest)) = data.split_first() else { return };
    if side.is_multiple_of(2) {
        stream::<ToServer>(rest);
    } else {
        stream::<FromServer>(rest);
    }
}

/// Either end reading a unidirectional stream: its [`UniHead`], then what the head says
/// follows, as the reader retypes it (terminal events, conversation events, or raw bytes).
pub fn uni_stream(data: &[u8]) {
    let mut feed = Feed::new(data);
    let Some(head) = feed.next::<UniHead>() else { return };
    match head {
        UniHead::Session { .. } => while feed.next::<TermEvent>().is_some() {},
        UniHead::Conversation { .. } => while feed.next::<ConversationEvent>().is_some() {},
        UniHead::Bulk(_) => {}
    }
}

/// The worker's control socket: one line of JSON (`apps/slopty-worker/src/ctl.rs` reads a
/// [`CtlRequest`] from it), and the CLI and hook relay reading the [`CtlReply`] line back.
pub fn ctl(data: &[u8]) {
    let Ok(text) = std::str::from_utf8(data) else { return };
    let line = text.split_inclusive('\n').next().unwrap_or_default().trim();
    json_round_trips::<CtlRequest>(line);
    json_round_trips::<CtlReply>(line);
}

/// Decode every message of type `T` from `data` fed in pieces, and check that feeding the same
/// bytes in one piece reads the same messages.
fn stream<T: Serialize + DeserializeOwned>(data: &[u8]) {
    let mut pieces = Feed::new(data);
    let mut whole = Feed::whole(data.get(1..).unwrap_or_default());
    loop {
        let piecewise = pieces.next::<T>().map(|m| encode(&m));
        let at_once = whole.next::<T>().map(|m| encode(&m));
        assert_eq!(piecewise, at_once, "the piece size changed what was read");
        if piecewise.is_none() {
            return;
        }
    }
}

/// A stream's bytes, handed to the framing reader a piece at a time.
struct Feed<'a> {
    pieces: std::slice::Chunks<'a, u8>,
    buf: BytesMut,
}

impl<'a> Feed<'a> {
    /// The first byte of `data` is the piece size (0 reads as 1); the rest is the stream.
    fn new(data: &'a [u8]) -> Self {
        let (size, rest) = data.split_first().map_or((1, data), |(&s, r)| (s.max(1), r));
        Self { pieces: rest.chunks(usize::from(size)), buf: BytesMut::new() }
    }

    /// All of `data` as one piece.
    fn whole(data: &'a [u8]) -> Self {
        Self { pieces: data.chunks(data.len().max(1)), buf: BytesMut::new() }
    }

    /// The next message, checked to encode back to itself; `None` once the stream ends or a
    /// frame is too large or does not decode.
    fn next<T: Serialize + DeserializeOwned>(&mut self) -> Option<T> {
        loop {
            match codec::try_take(&mut self.buf) {
                Ok(Some(body)) => {
                    let msg = decode::<T>(&body).ok()?;
                    round_trips(&msg);
                    return Some(msg);
                }
                Ok(None) => self.buf.extend_from_slice(self.pieces.next()?),
                Err(_) => return None,
            }
        }
    }
}

/// Heap bytes a decode may take per byte of the body, besides its terminal cells: a `Vec` of
/// the smallest wire items that are the largest in memory, grown by doubling.
const HEAP_PER_BYTE: u64 = 64;

/// Heap bytes any decode may take: serde preallocates a sequence up to 1 MiB before it has
/// read the items it announced, so a truncated one costs that much.
const HEAP_SLACK: u64 = 2 << 20;

/// Decode `body` as the reading side does ([`codec::decode_body`]), and check that it took
/// no more heap than its bytes bound, plus the cells its terminal lines put back: the
/// trailing blanks a line leaves off the wire, which the decode's cell budget bounds instead.
/// Counted only where [`alloc::Counting`] is the allocator: the fuzz targets and the replay.
pub(crate) fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, codec::CodecError> {
    let ((decoded, cells), used) = alloc::measure(|| {
        slopty_grid::with_cell_budget(usize::MAX, || codec::decode_body::<T>(body))
    });
    let len = u64::try_from(body.len()).unwrap_or(u64::MAX);
    let cells = u64::try_from(cells.saturating_mul(size_of::<Cell>())).unwrap_or(u64::MAX);
    let bound = HEAP_PER_BYTE.saturating_mul(len).saturating_add(cells).saturating_add(HEAP_SLACK);
    assert!(
        used.bytes <= bound,
        "{} body bytes decoded into {} heap bytes, past {bound} ({} of cells)",
        body.len(),
        used.bytes,
        cells
    );
    decoded
}

fn encode<T: Serialize>(msg: &T) -> Vec<u8> {
    codec::encode_body(msg).expect("a decoded message encodes")
}

/// A decoded message encodes to bytes that decode to a message encoding to the same bytes: what
/// a relay (the server passing a verb on, a client re-sending a request) would send is what it
/// read.
pub(crate) fn round_trips<T: Serialize + DeserializeOwned>(msg: &T) {
    let once = encode(msg);
    let back = codec::decode_body::<T>(&once).expect("an encoded message decodes");
    assert_eq!(once, encode(&back), "a message did not encode back to the same bytes");
}

fn json_round_trips<T: Serialize + DeserializeOwned>(line: &str) {
    let Ok(msg) = serde_json::from_str::<T>(line) else { return };
    let once = serde_json::to_string(&msg).expect("a parsed line serialises");
    let back = serde_json::from_str::<T>(&once).expect("a serialised line parses");
    let twice = serde_json::to_string(&back).expect("a parsed line serialises");
    assert_eq!(once, twice, "a control line did not serialise back to the same text");
}
