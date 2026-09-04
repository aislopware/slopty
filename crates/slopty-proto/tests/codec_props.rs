//! Property tests for the framing codec: any byte stream split at any boundary decodes to the
//! same sequence of messages, and garbage never panics.

#[cfg(test)]
mod props {
    use bytes::BytesMut;
    use proptest::prelude::*;
    use slopty_core::MonoTime;
    use slopty_proto::terminal::TermRequest;
    use slopty_proto::{ClientMsg, codec};

    fn msg_strategy() -> impl Strategy<Value = ClientMsg> {
        prop_oneof![
            any::<u64>().prop_map(|n| ClientMsg::Ping { sent_at: MonoTime::from_nanos(n) }),
            proptest::collection::vec(any::<u8>(), 0..512).prop_map(|raw| ClientMsg::Term {
                session: slopty_core::SessionId::nil(),
                req: TermRequest::Raw(raw),
            }),
            ".{0,64}".prop_map(|s| ClientMsg::Term {
                session: slopty_core::SessionId::nil(),
                req: TermRequest::Paste(s),
            }),
        ]
    }

    proptest! {
        #[test]
        fn split_anywhere_round_trips(
            msgs in proptest::collection::vec(msg_strategy(), 1..8),
            cuts in proptest::collection::vec(1_usize..64, 0..32),
        ) {
            let mut wire = Vec::new();
            for m in &msgs {
                wire.extend_from_slice(&codec::encode(m).expect("encode"));
            }
            let mut buf = BytesMut::new();
            let mut out = Vec::new();
            let mut pos = 0;
            let mut cuts = cuts.into_iter();
            while pos < wire.len() {
                let take = cuts.next().unwrap_or(usize::MAX).min(wire.len().saturating_sub(pos));
                buf.extend_from_slice(wire.get(pos..pos.saturating_add(take)).expect("in range"));
                pos = pos.saturating_add(take);
                while let Some(m) = codec::try_decode::<ClientMsg>(&mut buf).expect("decode") {
                    out.push(m);
                }
            }
            prop_assert_eq!(out, msgs);
            prop_assert!(buf.is_empty());
        }

        #[test]
        fn garbage_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
            let mut buf = BytesMut::from(&bytes[..]);
            // Either decodes, needs more, or errors — all are fine; a panic is not.
            let _outcome = codec::try_decode::<ClientMsg>(&mut buf);
        }
    }
}
