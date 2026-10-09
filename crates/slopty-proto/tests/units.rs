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
        let ping = ClientMsg::Ping { sent_at: slopty_core::MonoTime::from_nanos(1) };
        assert_eq!(ping.kind(), "Ping");
        let stop = slopty_proto::search::SearchRequest::Stop { id: 3 };
        assert_eq!(ClientMsg::Search(stop).kind(), "Search");
        let failed = slopty_proto::search::SearchEvent::Failed { id: 3, error: String::new() };
        assert_eq!(failed.id(), 3);
        assert_eq!(WorkerMsg::Search(failed).kind(), "Search");
    }

    /// A note's Allow and Deny answer only an approval offering a plain allow and a deny; the
    /// deny they send is the plain one, else the first.
    #[test]
    fn a_notes_buttons_answer_only_a_plain_yes_or_no() {
        use slopty_core::WallMs;
        use slopty_proto::thread::wire::RequestCard;
        use slopty_proto::thread::{AskId, Choice, Effect, Request, once};
        let choice = |id: &str, effect, scope: Option<&str>, stops| Choice {
            id: id.to_owned(),
            label: id.to_owned(),
            effect,
            scope: scope.map(str::to_owned),
            stops,
        };
        let card = |kind: &str, options| RequestCard {
            id: AskId("toolu_1".to_owned()),
            item: None,
            kind: kind.to_owned(),
            title: "Run cargo test?".to_owned(),
            options,
            opened_ms: WallMs::ZERO,
        };
        let always = choice("always", Effect::Allow, Some("this session"), false);
        let yes = choice("yes", Effect::Allow, None, false);
        let stop = choice("stop", Effect::Deny, None, true);
        let no = choice("no", Effect::Deny, None, false);
        let ask = card(Request::APPROVAL, vec![always.clone(), yes, stop.clone(), no]);
        assert!(ask.answerable());
        assert_eq!(once(&ask.options, true).map(|c| c.id.as_str()), Some("yes"));
        assert_eq!(once(&ask.options, false).map(|c| c.id.as_str()), Some("no"), "the plain one");
        assert_eq!(
            once(std::slice::from_ref(&stop), false).map(|c| c.id.as_str()),
            Some("stop"),
            "else the first"
        );
        assert!(!card(Request::APPROVAL, vec![always, stop]).answerable(), "no plain allow");
        assert!(!card(Request::QUESTION, ask.options).answerable(), "a question");
    }

    /// A review is read the weightiest file first, tests, fixtures, locks and generated code
    /// after the rest, the same weight by path; a word that only holds "test" is no test.
    #[test]
    fn a_review_is_read_by_weight_with_the_quiet_files_below() {
        use slopty_proto::thread::Patch;
        use slopty_proto::thread::wire::{FileDiff, quiet, reading_order};

        let file = |path: &str, added, removed| FileDiff {
            path: path.to_owned(),
            from: None,
            to: None,
            binary: false,
            patch: Patch { hunks: Vec::new(), added, removed, clipped_lines: 0, full: None },
        };
        let files = [
            file("src/small.rs", 1, 0),
            file("Cargo.lock", 400, 300),
            file("crates/x/tests/e2e.rs", 90, 0),
            file("src/big.rs", 40, 12),
            file("src/__snapshots__/a.snap", 5, 5),
            file("src/also_small.rs", 1, 0),
        ];
        let paths: Vec<&str> =
            reading_order(&files).into_iter().map(|at| files[at].path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "src/big.rs",
                "src/also_small.rs",
                "src/small.rs",
                "Cargo.lock",
                "crates/x/tests/e2e.rs",
                "src/__snapshots__/a.snap"
            ]
        );
        assert!(!quiet("src/contest.rs"), "a word holding \"test\" is not a test");
        assert!(quiet("web/app.test.ts") && quiet("pkg/x_test.go") && quiet("test_x.py"));
    }
}
