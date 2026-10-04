//! The wire crate's arithmetic and names, pinned: constants a peer relies on, the media
//! header's kinds and flags, the codec's limit, and the variant
//! names logs use.

#[cfg(test)]
mod units {
    use bytes::{BufMut as _, BytesMut};
    use slopty_proto::codec::{self, CodecError, MAX_FRAME_BYTES};
    use slopty_proto::file::{FILE_BYTES, INLINE_FILE_BYTES};
    use slopty_proto::input::CellMetrics;
    use slopty_proto::media::{Kind, MediaHeader, flags};
    use slopty_proto::terminal::{MAX_FETCH_LINES, MAX_OSC52_BYTES, TermSize};
    use slopty_proto::{ClientMsg, WorkerMsg};
    use zerocopy::FromZeros as _;

    /// A tunnel's refusal rides its reset code, pinned both ways; zero is a connection that
    /// broke once open, no refusal.
    #[test]
    fn a_tunnel_refusal_is_its_reset_code() {
        use slopty_proto::transfer::TunnelRefusal::{self, Refused, Unreachable, Unresolved};
        for (why, code) in [(Refused, 1), (Unresolved, 2), (Unreachable, 3)] {
            assert_eq!(why.code(), code);
            assert_eq!(TunnelRefusal::from_code(code.into()), Some(why));
        }
        assert_eq!(TunnelRefusal::from_code(0), None);
        assert_eq!(TunnelRefusal::from_code(4), None);
    }

    /// A region is held to its target: edges on even pixels, a side of at least
    /// `Region::MIN_SIDE`, moved in from an edge it would cross to keep that side, cut at an
    /// edge it runs past, nothing for one off the target, and nothing for the whole target.
    #[test]
    fn a_region_is_held_to_its_target() {
        use slopty_proto::screen::Region;
        let r = |x, y, w, h| Region { x, y, w, h };
        let native = (3024, 1964);
        assert_eq!(r(401, 301, 1755, 987).within(native), Some(r(400, 300, 1756, 988)));
        assert_eq!(r(2500, 1500, 1756, 988).within(native), Some(r(2500, 1500, 524, 464)));
        assert_eq!(r(3000, 1950, 10, 10).within(native), Some(r(2960, 1900, 64, 64)), "min");
        assert_eq!(r(100, 100, 0, 50).within(native), None, "no width");
        assert_eq!(r(3024, 0, 64, 64).within(native), None, "off the right edge");
        assert_eq!(r(0, 0, 3024, 1964).within(native), None, "the whole target");
        assert_eq!(r(0, 0, u16::MAX, u16::MAX).within(native), None, "more than the target");
        assert_eq!(r(0, 0, 32, 32).within((48, 40)), None, "a target under the minimum is all");
    }

    /// A stripe's media stream names its stream and its place, and the top stripe's is the
    /// stream's own.
    #[test]
    fn a_stripes_media_stream_names_its_stream() {
        use slopty_core::StreamId;
        use slopty_proto::screen::Stripe;
        for id in [1, 7, 0x7fff_ffff] {
            let stream = StreamId(id);
            assert_eq!(Stripe::media_of(stream, 0), stream);
            let lower = Stripe::media_of(stream, 1);
            assert_ne!(lower, stream);
            assert_eq!(Stripe::stream_of(lower), (stream, 1));
            assert_eq!(Stripe::stream_of(stream), (stream, 0));
        }
        assert_eq!(Stripe::MAX, 2);
        assert_eq!(slopty_proto::media::FRAME_PREFIX_BYTES, 28);
    }

    #[test]
    fn the_limits_are_the_numbers_the_docs_name() {
        assert_eq!(FILE_BYTES, 16_777_216);
        assert_eq!(INLINE_FILE_BYTES, 65_536);
        assert_eq!(MAX_OSC52_BYTES, 262_144);
        assert_eq!(MAX_FETCH_LINES, 4096);
        assert_eq!(slopty_proto::transfer::INLINE_CLIP_BYTES, 65_536);
        assert_eq!(MAX_FRAME_BYTES, 16_777_216);
        let size = TermSize {
            cols: 80,
            rows: 24,
            metrics: CellMetrics { cell_width: 8, cell_height: 16 },
        };
        assert_eq!((size.width_px(), size.height_px()), (640, 384));
    }

    #[test]
    fn media_kinds_and_flags_are_their_wire_values() {
        for (v, kind) in [
            (0, Kind::VideoData),
            (1, Kind::VideoParity),
            (2, Kind::Audio),
            (3, Kind::Cursor),
            (4, Kind::Heartbeat),
            (5, Kind::Clock),
        ] {
            assert_eq!(Kind::from_u8(v), Some(kind));
            assert_eq!(kind as u8, v);
        }
        assert_eq!(Kind::from_u8(6), None, "no kind past the clock echo");
        assert_eq!(
            [
                flags::KEYFRAME,
                flags::LTR,
                flags::LTR_REFRESH,
                flags::RETRANSMIT,
                flags::DISCARDABLE,
                flags::PREV_DISCARDABLE,
            ],
            [1, 2, 4, 8, 16, 32]
        );
        let mut header = MediaHeader::new_zeroed();
        header.kind = Kind::VideoData as u8;
        assert!(!header.is_parity());
        header.kind = Kind::VideoParity as u8;
        assert!(header.is_parity());
    }

    #[test]
    fn a_frame_at_the_limit_passes_and_one_past_it_is_refused() {
        // A `Vec<u8>` body is a 4-byte varint length then the bytes: exactly the limit.
        let body = vec![0_u8; MAX_FRAME_BYTES - 4];
        let frame = codec::encode(&body).expect("at the limit");
        assert_eq!(frame.len(), MAX_FRAME_BYTES + 4);
        let over = vec![0_u8; MAX_FRAME_BYTES - 3];
        assert!(matches!(codec::encode(&over), Err(CodecError::TooLarge { .. })));
        // A prefix announcing exactly the limit waits for the bytes; one past it is refused.
        let mut buf = BytesMut::new();
        buf.put_u32_le(u32::try_from(MAX_FRAME_BYTES).expect("fits"));
        assert!(matches!(codec::try_decode::<Vec<u8>>(&mut buf), Ok(None)));
        let mut buf = BytesMut::new();
        buf.put_u32_le(u32::try_from(MAX_FRAME_BYTES + 1).expect("fits"));
        assert!(matches!(codec::try_decode::<Vec<u8>>(&mut buf), Err(CodecError::TooLarge { .. })));
    }

    #[test]
    fn messages_name_their_variants_for_logs() {
        assert_eq!(ClientMsg::InstallHooks.kind(), "InstallHooks");
        let stop = slopty_proto::search::SearchRequest::Stop { id: 3 };
        assert_eq!(ClientMsg::Search(stop).kind(), "Search");
        let failed = slopty_proto::search::SearchEvent::Failed { id: 3, error: String::new() };
        assert_eq!(failed.id(), 3);
        assert_eq!(WorkerMsg::Search(failed).kind(), "Search");
    }
}
