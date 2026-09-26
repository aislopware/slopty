//! The wire crate's arithmetic and names, pinned: constants a peer relies on, the media
//! header's kinds and flags, the codec's limit, and the variant
//! names logs use.

#[cfg(test)]
mod units {
    use bytes::{BufMut as _, BytesMut};
    use slopty_core::{ClientId, ItemId};
    use slopty_proto::codec::{self, CodecError, MAX_FRAME_BYTES};
    use slopty_proto::file::FILE_BYTES;
    use slopty_proto::input::CellMetrics;
    use slopty_proto::items::ItemSync;
    use slopty_proto::media::{Kind, MediaHeader, flags};
    use slopty_proto::terminal::{MAX_FETCH_LINES, MAX_OSC52_BYTES, TermSize};
    use slopty_proto::{ClientMsg, WorkerMsg};
    use zerocopy::FromZeros as _;

    #[test]
    fn the_limits_are_the_numbers_the_docs_name() {
        assert_eq!(FILE_BYTES, 524_288);
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
            (5, Kind::Term),
        ] {
            assert_eq!(Kind::from_u8(v), Some(kind));
            assert_eq!(kind as u8, v);
        }
        assert_eq!(Kind::from_u8(6), None);
        assert_eq!(
            [flags::KEYFRAME, flags::LTR, flags::LTR_REFRESH, flags::RETRANSMIT],
            [1, 2, 4, 8]
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
        assert_eq!(ClientMsg::Point { item: ItemId::new() }.kind(), "Point");
        let pointed = ItemSync::Pointed {
            client: ClientId::new(),
            name: "x".to_owned(),
            item: ItemId::new(),
        };
        assert_eq!(WorkerMsg::Items(pointed).kind(), "Items");
    }
}
