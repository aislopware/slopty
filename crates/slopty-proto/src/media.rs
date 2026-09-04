//! Datagram header for media (video fragments, parity, audio, cursor position).
//!
//! Fixed 16-byte layout, little-endian, `zerocopy` so a receiver reads it in place with no parsing
//! and a sender writes it into the front of a buffer with no allocation. Everything after the
//! header is opaque to this module.

use zerocopy::little_endian::{U16, U32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// Largest datagram we will ever send. Under the 1280-byte IPv6 minimum MTU after QUIC/UDP
/// overhead, so it never fragments on a `WireGuard` or cellular path.
pub const MAX_DATAGRAM: usize = 1200;

/// Bytes of [`MediaHeader`].
pub const HEADER_BYTES: usize = 16;

/// Bytes available for payload after the header.
pub const MAX_PAYLOAD: usize = MAX_DATAGRAM - HEADER_BYTES;

/// Datagram kinds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Kind {
    /// A fragment of an encoded video frame.
    VideoData = 0,
    /// A Reed–Solomon parity shard for a video frame.
    VideoParity = 1,
    /// One Opus packet.
    Audio = 2,
    /// Cursor position update.
    Cursor = 3,
}

impl Kind {
    /// Parse.
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::VideoData),
            1 => Some(Self::VideoParity),
            2 => Some(Self::Audio),
            3 => Some(Self::Cursor),
            _ => None,
        }
    }
}

/// Header flag bits.
pub mod flags {
    /// The frame is an IDR / keyframe.
    pub const KEYFRAME: u8 = 1 << 0;
    /// The frame is a long-term reference; the receiver must ack its token.
    pub const LTR: u8 = 1 << 1;
    /// The frame was encoded as an LTR refresh (recovery frame).
    pub const LTR_REFRESH: u8 = 1 << 2;
    /// This datagram is a retransmission (NACK reply).
    pub const RETRANSMIT: u8 = 1 << 3;
}

/// The fixed header at the front of every media datagram.
#[derive(
    Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Debug, PartialEq, Eq,
)]
#[repr(C)]
pub struct MediaHeader {
    /// Stream this belongs to (matches `StreamId`).
    pub stream: U32,
    /// Frame number within the stream (video), packet number (audio), or update seq (cursor).
    pub frame: U32,
    /// Fragment index within the frame. For parity, `data_count + parity index`.
    pub index: U16,
    /// Number of data fragments in the frame.
    pub data_count: U16,
    /// Number of parity fragments in the frame.
    pub parity_count: u8,
    /// [`Kind`] as `u8`.
    pub kind: u8,
    /// [`flags`].
    pub flags: u8,
    /// Host send timestamp, low 8 bits of milliseconds (wraps; combined with the report's
    /// 32-bit field for OWD trend, never for absolute latency).
    pub send_ms_lo: u8,
}

impl MediaHeader {
    /// Parse the front of a datagram; `None` if it is too short.
    #[must_use]
    pub fn parse(datagram: &[u8]) -> Option<(&Self, &[u8])> {
        Self::ref_from_prefix(datagram).ok()
    }

    /// The kind, if valid.
    #[must_use]
    pub const fn kind(&self) -> Option<Kind> {
        Kind::from_u8(self.kind)
    }

    /// True when this datagram is a parity shard.
    #[must_use]
    pub const fn is_parity(&self) -> bool {
        self.kind == Kind::VideoParity as u8
    }
}

const _: () = assert!(size_of::<MediaHeader>() == HEADER_BYTES, "header layout drifted");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trips_through_bytes() {
        let h = MediaHeader {
            stream: U32::new(7),
            frame: U32::new(123_456),
            index: U16::new(3),
            data_count: U16::new(10),
            parity_count: 2,
            kind: Kind::VideoParity as u8,
            flags: flags::KEYFRAME | flags::LTR,
            send_ms_lo: 0xAB,
        };
        let mut buf = vec![0_u8; MAX_DATAGRAM];
        buf.get_mut(..HEADER_BYTES).unwrap().copy_from_slice(h.as_bytes());
        buf[HEADER_BYTES] = 0xEE;
        let (parsed, payload) = MediaHeader::parse(&buf).unwrap();
        assert_eq!(parsed, &h);
        assert_eq!(payload.len(), MAX_PAYLOAD);
        assert_eq!(payload[0], 0xEE);
        assert!(parsed.is_parity());
        assert_eq!(parsed.kind(), Some(Kind::VideoParity));
    }

    #[test]
    fn short_datagram_is_rejected() {
        assert!(MediaHeader::parse(&[0_u8; HEADER_BYTES - 1]).is_none());
    }

    #[test]
    fn layout_is_little_endian_and_packed() {
        let h = MediaHeader {
            stream: U32::new(0x0102_0304),
            frame: U32::new(0),
            index: U16::new(0x0506),
            data_count: U16::new(0),
            parity_count: 0,
            kind: 0,
            flags: 0,
            send_ms_lo: 0,
        };
        let b = h.as_bytes();
        assert_eq!(&b[..4], &[4, 3, 2, 1]);
        assert_eq!(&b[8..10], &[6, 5]);
    }
}
