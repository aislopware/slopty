//! Host packetizer → (lossy wire) → client reassembler, end to end.

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use bytes::Bytes;
    use pretty_assertions::assert_eq;
    use proptest::prelude::*;
    use slopty_core::StreamId;
    use slopty_media::{
        Action, Config, EncodedFrame, FrameOut, Ignored, Ingest, Packetizer, Reassembler, SentFrame,
    };
    use slopty_proto::media::{MediaHeader, flags};

    const STREAM: StreamId = StreamId(4);
    const RTT: Duration = Duration::from_millis(20);

    fn cfg() -> Config {
        Config::default()
    }

    fn frame_bytes(seed: u32, len: usize) -> Vec<u8> {
        let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state & 0xFF) as u8
            })
            .collect()
    }

    struct Harness {
        tx: Packetizer,
        rx: Reassembler,
        now: Instant,
    }

    impl Harness {
        fn new() -> Self {
            let now = Instant::now();
            Self { tx: Packetizer::new(STREAM), rx: Reassembler::new(STREAM, cfg(), now), now }
        }

        fn advance(&mut self, by: Duration) {
            self.now = self.now.checked_add(by).unwrap();
        }

        fn send(&mut self, data: &[u8], keyframe: bool, ltr_refresh: bool) -> SentFrame {
            let frame = EncodedFrame {
                data,
                keyframe,
                ltr_token: keyframe.then_some(0xABCD),
                ltr_refresh,
                capture_ts_us: 1_000,
            };
            self.tx.packetize(&frame, 0).unwrap().clone()
        }

        /// Feed datagrams. Parity arriving after its frame completed is a duplicate, and after
        /// the frame was delivered it is stale; both are fine.
        fn deliver(&mut self, datagrams: &[Bytes]) {
            for dg in datagrams {
                let r = self.rx.ingest(dg, self.now);
                assert!(
                    matches!(
                        r,
                        Ingest::Video | Ingest::Ignored(Ignored::Stale | Ignored::Duplicate)
                    ),
                    "{r:?}"
                );
            }
        }

        fn deliver_except(&mut self, sent: &SentFrame, dropped: &[usize]) {
            let kept: Vec<Bytes> = sent
                .datagrams
                .iter()
                .enumerate()
                .filter(|(i, _)| !dropped.contains(i))
                .map(|(_, d)| d.clone())
                .collect();
            self.deliver(&kept);
        }

        fn drain(&mut self) -> Vec<FrameOut> {
            std::iter::from_fn(|| self.rx.next_frame()).collect()
        }

        fn tick(&mut self) -> Vec<Action> {
            self.rx.tick(self.now, RTT)
        }
    }

    #[test]
    fn clean_frames_flow_in_order_without_parity_work() {
        let mut h = Harness::new();
        let key = frame_bytes(1, 40_000);
        let p1 = frame_bytes(2, 900);
        let p2 = frame_bytes(3, 3_000);
        let s0 = h.send(&key, true, false);
        let s1 = h.send(&p1, false, false);
        let s2 = h.send(&p2, false, false);
        // Frame 2 lands before frame 1; delivery still comes out in order.
        h.deliver(&s0.datagrams);
        h.deliver(&s2.datagrams);
        assert_eq!(h.drain().len(), 1, "frame 2 waits for frame 1");
        h.deliver(&s1.datagrams);
        let out = h.drain();
        assert_eq!(out.iter().map(|f| f.info.frame).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(out[0].data, p1);
        assert_eq!(out[1].data, p2);
        assert!(out.iter().all(|f| !f.info.recovered));
        let stats = h.rx.stats();
        assert_eq!((stats.frames_ok, stats.frames_fec, stats.datagrams_lost), (3, 0, 0));
        assert!(h.tick().is_empty());
    }

    #[test]
    fn first_frame_must_be_a_keyframe() {
        let mut h = Harness::new();
        let p = frame_bytes(9, 500);
        let s0 = h.send(&p, false, false);
        h.deliver(&s0.datagrams);
        assert!(h.drain().is_empty());
        let key = frame_bytes(1, 5_000);
        let s1 = h.send(&key, true, false);
        h.deliver(&s1.datagrams);
        let out = h.drain();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].info.frame, 1);
        assert!(out[0].info.keyframe);
        assert_eq!(out[0].info.ltr_token, Some(0xABCD));
        assert!(!h.rx.awaiting_refresh());
    }

    #[test]
    fn parity_recovers_lost_fragments_without_a_nack() {
        let mut h = Harness::new();
        h.tx.set_parity_permille(200);
        let key = frame_bytes(1, 30_000);
        let s0 = h.send(&key, true, false);
        let data_count = usize::from(s0.layout.data_count);
        let parity_count = usize::from(s0.layout.parity_count);
        assert_eq!((data_count, parity_count), (26, 6));
        // Drop as many data fragments as there is parity, including the first (the prefix).
        let dropped: Vec<usize> = (0..parity_count).collect();
        h.deliver_except(&s0, &dropped);
        let out = h.drain();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].data, key);
        assert!(out[0].info.recovered);
        assert!(h.tick().is_empty(), "nothing to NACK");
        let stats = h.rx.stats();
        assert_eq!((stats.frames_fec, stats.datagrams_lost), (1, 6));
    }

    #[test]
    fn beyond_parity_a_nack_and_retransmission_complete_the_frame() {
        let mut h = Harness::new();
        let key = frame_bytes(1, 2_000);
        let s0 = h.send(&key, true, false);
        h.deliver(&s0.datagrams);
        h.drain();

        let p = frame_bytes(2, 30_000);
        let s1 = h.send(&p, false, false);
        // 26 data + 6 parity; drop 8 data fragments.
        let dropped: Vec<usize> = vec![0, 3, 4, 10, 11, 12, 20, 25];
        h.deliver_except(&s1, &dropped);
        assert!(h.drain().is_empty());
        assert!(h.tick().is_empty(), "too early to NACK");
        h.advance(cfg().nack_delay);
        let actions = h.tick();
        let expected: Vec<u16> = dropped.iter().map(|&i| u16::try_from(i).unwrap()).collect();
        assert_eq!(actions, vec![Action::Nack { frame: 1, fragments: expected.clone() }]);
        assert!(h.tick().is_empty(), "one NACK per round trip");

        // Only two of the retransmissions make it: parity covers the rest.
        let resent = h.tx.retransmit(1, &expected);
        assert_eq!(resent.len(), 8);
        let (hdr, _) = MediaHeader::parse(&resent[0]).unwrap();
        assert_eq!(hdr.flags & flags::RETRANSMIT, flags::RETRANSMIT);
        h.advance(RTT);
        h.deliver(&resent[..2]);
        let out = h.drain();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].data, p);
        assert!(out[0].info.recovered);
        assert!(out[0].hold >= cfg().nack_delay + RTT);
        let stats = h.rx.stats();
        assert_eq!(
            (stats.frames_ok, stats.frames_fec, stats.frames_retransmit, stats.nacks),
            (2, 1, 1, 1)
        );
    }

    #[test]
    fn a_frame_that_never_arrives_is_nacked_whole_then_refreshed() {
        let mut h = Harness::new();
        let s0 = h.send(&frame_bytes(1, 2_000), true, false);
        let s1 = h.send(&frame_bytes(2, 2_000), false, false);
        let s2 = h.send(&frame_bytes(3, 2_000), false, false);
        h.deliver(&s0.datagrams);
        h.drain();
        h.deliver(&s2.datagrams);
        assert!(h.drain().is_empty(), "frame 2 waits for the missing frame 1");

        h.advance(cfg().nack_delay);
        assert_eq!(h.tick(), vec![Action::Nack { frame: 1, fragments: vec![] }]);
        // The link keeps flowing (a later frame lands), so the retry and the deadline apply.
        h.advance(RTT + cfg().nack_delay);
        let s3 = h.send(&frame_bytes(4, 2_000), false, false);
        h.deliver(&s3.datagrams);
        assert_eq!(h.tick(), vec![Action::Nack { frame: 1, fragments: vec![] }], "second try");
        h.advance(RTT + cfg().nack_delay + cfg().grace);
        let actions = h.tick();
        assert_eq!(actions, vec![Action::RequestRefresh { last_good_frame: 0 }]);
        assert!(h.rx.awaiting_refresh());
        assert!(h.drain().is_empty(), "frames 2 and 3 are dropped: they depended on frame 1");
        // The host answers with an LTR refresh; everything after it flows again.
        let refresh = frame_bytes(5, 4_000);
        let s4 = h.send(&refresh, false, true);
        let s5 = h.send(&frame_bytes(6, 1_000), false, false);
        h.deliver(&s4.datagrams);
        h.deliver(&s5.datagrams);
        let out = h.drain();
        assert_eq!(out.iter().map(|f| f.info.frame).collect::<Vec<_>>(), vec![4, 5]);
        assert!(out[0].info.ltr_refresh);
        assert_eq!(out[0].data, refresh);
        assert!(!h.rx.awaiting_refresh());
        assert_eq!(h.rx.stats().frames_lost, 1);
        // A straggler from the dropped frame is stale.
        assert_eq!(h.rx.ingest(&s1.datagrams[0], h.now), Ingest::Ignored(Ignored::Stale));
    }

    #[test]
    fn a_stalled_link_holds_the_frame_until_it_moves_again() {
        let mut h = Harness::new();
        let s0 = h.send(&frame_bytes(1, 2_000), true, false);
        h.deliver(&s0.datagrams);
        h.drain();
        let p = frame_bytes(2, 6_000);
        let s1 = h.send(&p, false, false);
        // 6 data + 2 parity; the tail of the frame is delayed, not dropped.
        h.deliver(&s1.datagrams[..3]);
        h.advance(cfg().nack_delay);
        assert_eq!(h.tick().len(), 1, "first NACK goes out on silence");
        // Nothing arrives for 200 ms: no retry, no refresh.
        for _ in 0..20 {
            h.advance(Duration::from_millis(10));
            assert!(h.tick().is_empty(), "stalled link: wait, do not give up");
        }
        assert!(!h.rx.awaiting_refresh());
        // The burst clears and the rest of the frame lands: delivered, nothing lost.
        h.deliver(&s1.datagrams[3..]);
        let out = h.drain();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].data, p);
        assert_eq!(h.rx.stats().frames_lost, 0);
    }

    #[test]
    fn a_stall_restarts_the_nack_clock_when_the_link_resumes() {
        let mut h = Harness::new();
        let s0 = h.send(&frame_bytes(1, 2_000), true, false);
        h.deliver(&s0.datagrams);
        h.drain();
        let p = frame_bytes(2, 6_000);
        let s1 = h.send(&p, false, false);
        // Drop 3 of 6 data fragments (beyond the 2 parity); the NACK goes out, then the link
        // stalls for 120 ms — longer than the whole loss deadline.
        h.deliver_except(&s1, &[0, 1, 2, 6, 7]);
        h.advance(cfg().nack_delay);
        let nack = h.tick();
        assert_eq!(nack.len(), 1);
        h.advance(Duration::from_millis(120));
        // The stall clears with a *later* frame first: frame 1 is not lost on the spot.
        let s2 = h.send(&frame_bytes(3, 2_000), false, false);
        h.deliver(&s2.datagrams);
        assert!(h.tick().is_empty(), "fresh deadline: no loss, no retry yet");
        assert!(h.drain().is_empty(), "frame 2 waits for frame 1");
        // One round trip later the retransmission (stuck behind the stall) lands.
        h.advance(RTT);
        let resent = h.tx.retransmit(1, &[0, 1, 2]);
        h.deliver(&resent);
        let out = h.drain();
        assert_eq!(out.iter().map(|f| f.info.frame).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(out[0].data, p);
        assert_eq!(h.rx.stats().frames_lost, 0);
        assert!(!h.rx.awaiting_refresh());
    }

    #[test]
    fn a_stall_longer_than_max_hold_gives_up() {
        let mut h = Harness::new();
        let s0 = h.send(&frame_bytes(1, 2_000), true, false);
        h.deliver(&s0.datagrams);
        h.drain();
        let s1 = h.send(&frame_bytes(2, 6_000), false, false);
        h.deliver(&s1.datagrams[..3]);
        h.advance(cfg().nack_delay);
        assert_eq!(h.tick().len(), 1);
        h.advance(cfg().max_hold);
        assert_eq!(h.tick(), vec![Action::RequestRefresh { last_good_frame: 0 }]);
        assert!(h.rx.awaiting_refresh());
    }

    #[test]
    fn refresh_requests_repeat_until_a_refresh_frame_arrives() {
        let mut h = Harness::new();
        // Nothing has arrived; the reassembler nudges the host for an IDR periodically.
        assert!(h.tick().is_empty(), "the constructor counts as the first request");
        h.advance(cfg().refresh_repeat + RTT * 2);
        assert_eq!(h.tick(), vec![Action::RequestRefresh { last_good_frame: 0 }]);
        assert!(h.tick().is_empty());
        // Each unanswered repeat doubles the wait: the second one is not due one period later.
        h.advance(cfg().refresh_repeat + RTT * 2);
        assert!(h.tick().is_empty(), "backoff");
        h.advance(cfg().refresh_repeat);
        assert_eq!(h.tick(), vec![Action::RequestRefresh { last_good_frame: 0 }]);
        // Over ten seconds a silent target sees a handful of requests, not eighty.
        let mut sent = 0;
        for _ in 0..1000 {
            h.advance(Duration::from_millis(10));
            sent += h.tick().len();
        }
        assert!(sent <= 6, "{sent} refresh repeats in 10 s");
        // A frame arriving resets the backoff.
        let s0 = h.send(&frame_bytes(1, 2_000), true, false);
        h.deliver(&s0.datagrams);
        assert_eq!(h.drain().len(), 1);
        assert_eq!(h.rx.stats().refreshes, 2 + u64::try_from(sent).unwrap());
    }

    #[test]
    fn garbage_duplicates_and_other_streams_are_ignored() {
        let mut h = Harness::new();
        let s0 = h.send(&frame_bytes(1, 2_000), true, false);
        assert_eq!(
            h.rx.ingest(&Bytes::from_static(&[1, 2, 3]), h.now),
            Ingest::Ignored(Ignored::Malformed)
        );
        let mut foreign = s0.datagrams[0].to_vec();
        foreign[0] ^= 1;
        assert_eq!(h.rx.ingest(&Bytes::from(foreign), h.now), Ingest::Ignored(Ignored::Foreign));
        let mut bad_kind = s0.datagrams[0].to_vec();
        bad_kind[13] = 200;
        assert_eq!(h.rx.ingest(&Bytes::from(bad_kind), h.now), Ingest::Ignored(Ignored::Malformed));
        let mut odd = s0.datagrams[0].to_vec();
        odd.push(0);
        assert_eq!(h.rx.ingest(&Bytes::from(odd), h.now), Ingest::Ignored(Ignored::Malformed));
        assert_eq!(h.rx.ingest(&s0.datagrams[0], h.now), Ingest::Video);
        assert_eq!(h.rx.ingest(&s0.datagrams[0], h.now), Ingest::Ignored(Ignored::Duplicate));
        let mut truncated = s0.datagrams[1].to_vec();
        truncated.truncate(truncated.len() - 2);
        assert_eq!(
            h.rx.ingest(&Bytes::from(truncated), h.now),
            Ingest::Ignored(Ignored::Malformed),
            "shard size changed mid-frame"
        );
        h.deliver(&s0.datagrams[1..]);
        assert_eq!(h.drain().len(), 1);
        assert_eq!(h.rx.ingest(&s0.datagrams[0], h.now), Ingest::Ignored(Ignored::Stale));
    }

    #[test]
    fn audio_and_cursor_pass_through() {
        let mut h = Harness::new();
        let audio = slopty_media::audio_datagram(STREAM, 5, 0, &[7; 64]).unwrap();
        assert_eq!(
            h.rx.ingest(&audio, h.now),
            Ingest::Audio { seq: 5, payload: Bytes::from(vec![7; 64]) }
        );
        let cursor = slopty_media::cursor_datagram(STREAM, 6, 0, 10, 20, true);
        match h.rx.ingest(&cursor, h.now) {
            Ingest::Cursor { seq: 6, update } => {
                assert_eq!((update.x.get(), update.y.get()), (10, 20));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn report_summarises_the_window() {
        let mut h = Harness::new();
        let s0 = h.send(&frame_bytes(1, 30_000), true, false);
        h.deliver_except(&s0, &[2, 3]);
        h.advance(Duration::from_millis(4));
        let s1 = h.send(&frame_bytes(2, 500), false, false);
        h.deliver(&s1.datagrams);
        let out = h.drain();
        assert_eq!(out.len(), 2);
        h.rx.ack_ltr(out[0].info.ltr_token.unwrap());
        let report = h.rx.take_report(1);
        assert_eq!(
            (report.frames_ok, report.frames_fec, report.frames_lost, report.datagrams_lost),
            (2, 1, 0, 2)
        );
        assert_eq!(report.last_host_send_ts_us, 1_000);
        assert_eq!((report.acked_ltr_len, report.acked_ltr[0]), (1, 0xABCD));
        assert_eq!(report.late_frames, 1);
        assert_eq!(report.queue_depth, 0);
        let empty = h.rx.take_report(0);
        assert_eq!((empty.frames_ok, empty.acked_ltr_len), (0, 0));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Any loss pattern within the parity budget, in any arrival order, recovers the exact
        /// frame.
        #[test]
        fn any_loss_within_parity_recovers(
            len in 1_usize..60_000,
            parity_permille in 0_u16..=500,
            seed in any::<u32>(),
            drops in prop::collection::vec(any::<prop::sample::Index>(), 0..=8),
            order in prop::collection::vec(any::<u16>(), 400),
        ) {
            let now = Instant::now();
            let mut tx = Packetizer::new(STREAM);
            tx.set_parity_permille(parity_permille);
            let mut rx = Reassembler::new(STREAM, cfg(), now);
            let data = frame_bytes(seed, len);
            let frame = EncodedFrame {
                data: &data,
                keyframe: true,
                ltr_token: None,
                ltr_refresh: false,
                capture_ts_us: 0,
            };
            let sent = tx.packetize(&frame, 0).unwrap().clone();
            let parity = usize::from(sent.layout.parity_count);
            let total = sent.datagrams.len();

            // Drop up to `parity` distinct datagrams, then shuffle what is left.
            let mut dropped: Vec<usize> = drops.iter().map(|i| i.index(total)).collect();
            dropped.sort_unstable();
            dropped.dedup();
            dropped.truncate(parity);
            let mut kept: Vec<(usize, Bytes)> = sent
                .datagrams
                .iter()
                .enumerate()
                .filter(|(i, _)| !dropped.contains(i))
                .map(|(i, d)| (i, d.clone()))
                .collect();
            kept.sort_by_key(|(i, _)| order.get(*i).copied().unwrap_or(0));
            for (_, dg) in kept {
                let r = rx.ingest(&dg, now);
                prop_assert!(
                    matches!(r, Ingest::Video | Ingest::Ignored(Ignored::Stale | Ignored::Duplicate)),
                    "{:?}",
                    r
                );
            }
            let out = rx.next_frame().expect("recovered");
            prop_assert_eq!(out.data, data);
            prop_assert!(rx.tick(now.checked_add(Duration::from_secs(1)).unwrap(), RTT).is_empty());
        }
    }
}
