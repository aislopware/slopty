//! Host packetizer → (lossy wire) → client reassembler, end to end.

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use bytes::Bytes;
    use pretty_assertions::assert_eq;
    use proptest::prelude::*;
    use slopty_core::StreamId;
    use slopty_media::{
        Action, Config, EncodedFrame, FrameOut, HEARTBEAT_AFTER, Ignored, Ingest, Packetizer,
        RateController, Reassembler, SentFrame, heartbeat_datagram,
    };
    use slopty_proto::media::{MediaHeader, flags};
    use slopty_proto::screen::RateVerdict;

    const STREAM: StreamId = StreamId(4);
    const RTT: Duration = Duration::from_millis(20);

    fn cfg() -> Config {
        Config::default()
    }

    /// The NACK delay the policy derives for this harness's round trip.
    fn nack_delay() -> Duration {
        cfg().nack_delay.for_rtt(RTT)
    }

    fn frame_bytes(seed: u32, len: usize) -> Vec<u8> {
        let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        std::iter::repeat_with(|| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state & 0xff) as u8
        })
        .take(len)
        .collect()
    }

    struct Harness {
        tx: Packetizer,
        rx: Reassembler,
        now: Instant,
        epoch: Instant,
    }

    impl Harness {
        fn new() -> Self {
            let now = Instant::now();
            Self {
                tx: Packetizer::new(STREAM),
                rx: Reassembler::new(STREAM, cfg(), now),
                now,
                epoch: now,
            }
        }

        /// The stamp the host puts on every datagram: the low byte of its millisecond clock.
        /// One clock drives both ends here, so a datagram packetized now and delivered later
        /// carries the send time, which is what tells a held link from a quiet source.
        fn send_ms_lo(&self) -> u8 {
            u8::try_from(self.now.saturating_duration_since(self.epoch).as_millis() % 256)
                .unwrap_or(0)
        }

        /// Time passes with the receiver's own loop not running — the machine descheduled it,
        /// or the test simply does not care.
        fn advance(&mut self, by: Duration) {
            self.now = self.now.checked_add(by).unwrap();
        }

        /// Time passes with the receiver running: a client ticks its policy at least every
        /// [`Config::tick_period`], and silence it *was* awake for is the only silence it may
        /// blame on the link. Returns nothing; a test that wants the actions calls `tick`.
        fn awake(&mut self, by: Duration) {
            let step = cfg().tick_period;
            let mut left = by;
            while !left.is_zero() {
                let chunk = left.min(step);
                self.advance(chunk);
                let _policy = self.tick();
                left = left.saturating_sub(chunk);
            }
        }

        fn send(&mut self, data: &[u8], keyframe: bool, ltr_refresh: bool) -> SentFrame {
            let frame = EncodedFrame {
                data,
                keyframe,
                ltr_token: keyframe.then_some(0xabcd),
                ltr_refresh,
                capture_ts_us: 1_000,
            };
            let stamp = self.send_ms_lo();
            self.tx.packetize(&frame, stamp).unwrap().clone()
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
        assert_eq!(out[0].info.ltr_token, Some(0xabcd));
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
        h.advance(nack_delay());
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
        assert!(out[0].hold >= nack_delay() + RTT);
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

        h.advance(nack_delay());
        assert_eq!(h.tick(), vec![Action::Nack { frame: 1, fragments: vec![] }]);
        // The link keeps flowing (a later frame lands), so the retry and the deadline apply.
        h.advance(RTT + nack_delay());
        let s3 = h.send(&frame_bytes(4, 2_000), false, false);
        h.deliver(&s3.datagrams);
        assert_eq!(h.tick(), vec![Action::Nack { frame: 1, fragments: vec![] }], "second try");
        h.advance(RTT + nack_delay() + cfg().grace);
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
        h.advance(nack_delay());
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
        h.advance(nack_delay());
        let nack = h.tick();
        assert_eq!(nack.len(), 1);
        // The host packetizes the next frame straight away; the link holds it for 120 ms with
        // everything else, which is what makes this a stall rather than a quiet source.
        let s2 = h.send(&frame_bytes(3, 2_000), false, false);
        h.awake(Duration::from_millis(120));
        // The stall clears with a *later* frame first: frame 1 is not lost on the spot.
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

    /// Silence past the stall gap is charged to the report window it falls in, whether the
    /// stall is still on at report time or released inside the window; a stall spanning two
    /// reports is split between them and its release is counted once.
    #[test]
    fn reports_carry_the_stall_time_and_count() {
        let mut h = Harness::new();
        // The host takes a while to send the first frame: that is not a stall.
        h.advance(Duration::from_millis(300));
        assert!(!h.rx.stalled(h.now));
        let r = h.rx.take_report(h.now, 0);
        assert_eq!((r.stalled_ms, r.stalls), (0, 0), "nothing before the first datagram");
        let s0 = h.send(&frame_bytes(1, 2_000), true, false);
        h.deliver(&s0.datagrams);
        h.drain();
        assert_eq!(h.rx.stats().stalls, 0, "the start-up wait is not a release either");
        // Both of the next frames leave the host now; the link is what holds them back.
        let s1 = h.send(&frame_bytes(2, 2_000), false, false);
        let s2 = h.send(&frame_bytes(3, 2_000), false, false);
        // 30 ms of silence: an ordinary inter-frame gap, not a stall.
        h.awake(Duration::from_millis(30));
        let r = h.rx.take_report(h.now, 0);
        assert_eq!((r.stalled_ms, r.stalls), (0, 0));
        assert!(!h.rx.stalled(h.now));
        // 120 ms with nothing at all: the report finds the stall in progress and charges all
        // the silence since the last datagram, including the 30 ms before the last report.
        h.awake(Duration::from_millis(120));
        assert!(h.rx.stalled(h.now));
        let r = h.rx.take_report(h.now, 0);
        assert_eq!((r.stalled_ms, r.stalls), (150, 0), "still on: time, no release yet");
        // 40 ms more, then the link moves again: only the part not yet charged, one release.
        // The frame was packetized before the silence — the link held it, the host did not.
        h.awake(Duration::from_millis(40));
        h.deliver(&s1.datagrams);
        assert!(!h.rx.stalled(h.now));
        let r = h.rx.take_report(h.now, 0);
        assert_eq!((r.stalled_ms, r.stalls), (40, 1));
        assert_eq!((h.rx.stats().stalls, h.rx.stats().stalled_ms), (1, 190));
        // A stall that starts and releases inside one window.
        h.awake(Duration::from_millis(80));
        h.deliver(&s2.datagrams);
        let r = h.rx.take_report(h.now, 0);
        assert_eq!((r.stalled_ms, r.stalls), (80, 1));
        let r = h.rx.take_report(h.now, 0);
        assert_eq!((r.stalled_ms, r.stalls), (0, 0), "the window is reset");
    }

    /// A source that produces no frames for a while is not a stalled link: the host's
    /// heartbeats keep the receiver's stall clock running only on silence from the link.
    #[test]
    fn heartbeats_keep_a_quiet_source_from_reading_as_a_stall() {
        let mut h = Harness::new();
        let s0 = h.send(&frame_bytes(1, 2_000), true, false);
        h.deliver(&s0.datagrams);
        h.drain();
        // 400 ms with nothing on screen, a heartbeat every HEARTBEAT_AFTER.
        for beat in 1..=16 {
            h.awake(HEARTBEAT_AFTER);
            let dg = heartbeat_datagram(STREAM, beat, 0);
            assert_eq!(h.rx.ingest(&dg, h.now), Ingest::Heartbeat);
            assert!(h.tick().is_empty());
        }
        assert!(!h.rx.stalled(h.now));
        let report = h.rx.take_report(h.now, 0);
        assert_eq!((report.stalled_ms, report.stalls), (0, 0), "no stall with heartbeats");
        assert_eq!((report.frames_lost, report.datagrams_lost, report.frames_ok), (0, 0, 1));
        // The controller sees a clean window: heartbeats are not frames, not loss, not queue.
        let mut c = RateController::new(30_000_000);
        let decision = (0..64).find_map(|_| c.on_report(&report, 0, None));
        assert_eq!(decision.map(|d| d.verdict), Some(RateVerdict::Grow));
        // The same 400 ms without heartbeats reads as a stall: past the send stamp's 256 ms
        // range the receiver cannot tell whose silence it was, and takes the pessimistic view.
        h.awake(Duration::from_millis(400));
        assert!(h.rx.stalled(h.now));
        let s1 = h.send(&frame_bytes(2, 2_000), false, false);
        h.deliver(&s1.datagrams);
        let report = h.rx.take_report(h.now, 0);
        assert_eq!((report.stalled_ms, report.stalls), (400, 1), "silence nobody can account for");
        assert_eq!(h.rx.stats().stalls, 1);
        assert_eq!(h.drain().len(), 1, "the frame after the stall is delivered");
    }

    /// A gap the host made itself — the capture had nothing to draw and its heartbeat was late
    /// — is not the link stalling. The send stamp on the datagram that ends the gap says the
    /// host was quiet for all of it, so nothing is charged and nothing is counted.
    #[test]
    fn a_quiet_source_whose_heartbeat_was_late_is_not_a_stall() {
        let mut h = Harness::new();
        let s0 = h.send(&frame_bytes(1, 2_000), true, false);
        h.deliver(&s0.datagrams);
        h.drain();
        // 150 ms with nothing on the wire at all, then the host draws again and sends.
        h.awake(Duration::from_millis(150));
        let s1 = h.send(&frame_bytes(2, 2_000), false, false);
        h.deliver(&s1.datagrams);
        assert_eq!(h.drain().len(), 1);
        let report = h.rx.take_report(h.now, 0);
        assert_eq!(
            (report.stalled_ms, report.stalls),
            (0, 0),
            "the host's silence, not the link's"
        );
        assert_eq!((h.rx.stats().stalls, h.rx.stats().stalled_ms), (0, 0));
        // While the host says the source is idle, an unfinished gap is not a stall either.
        h.rx.set_source_live(false);
        h.awake(Duration::from_millis(400));
        assert!(!h.rx.stalled(h.now));
        assert_eq!(h.rx.take_report(h.now, 0).stalled_ms, 0);
        h.rx.set_source_live(true);
        assert!(h.rx.stalled(h.now), "a live source owes the receiver datagrams again");
    }

    /// A retransmission carries the stamp of the frame it is a copy of, so it must not become
    /// the reference the next gap is measured against: the arrival would be the
    /// retransmission's and the stamp some older frame's, and the difference between them is
    /// not a host interval at all.
    #[test]
    fn a_retransmission_does_not_leave_its_stamp_behind() {
        let mut h = Harness::new();
        let s0 = h.send(&frame_bytes(1, 2_000), true, false);
        h.deliver(&s0.datagrams);
        h.drain();
        // 30 ms later a retransmission of that frame lands: under the stall gap, no stall, and
        // its stamp (frame 0's, so 0 ms) is not the host's latest.
        h.awake(Duration::from_millis(30));
        let resent = h.tx.retransmit(0, &[0]);
        h.deliver(&resent);
        assert_eq!(h.rx.stats().stalls, 0, "30 ms is not a stall");
        // 70 ms of silence, then a datagram the host sent 60 ms after frame 0. Paired with the
        // retransmission's stale stamp the host would look busy for 60 of those 70 ms; paired
        // with nothing, which is the truth, the whole gap is the link's.
        h.awake(Duration::from_millis(70));
        let beat = heartbeat_datagram(STREAM, 1, 60);
        assert_eq!(h.rx.ingest(&beat, h.now), Ingest::Heartbeat);
        let report = h.rx.take_report(h.now, 0);
        assert_eq!((report.stalled_ms, report.stalls), (70, 1), "the link held it");
    }

    /// A datagram whose stamp is older than the one before it — reordered, or delayed past its
    /// successor — must not read as a long host pause. The subtraction is unsigned and wraps,
    /// so 10 ms backwards looks like 246 ms forwards, which would forgive any stall.
    #[test]
    fn a_stamp_that_goes_backwards_is_not_evidence() {
        let mut h = Harness::new();
        let s0 = h.send(&frame_bytes(1, 2_000), true, false);
        h.deliver(&s0.datagrams);
        h.drain();
        let beat = heartbeat_datagram(STREAM, 1, 100);
        assert_eq!(h.rx.ingest(&beat, h.now), Ingest::Heartbeat);
        // 80 ms later, a datagram the host stamped *before* that one.
        h.awake(Duration::from_millis(80));
        let late = heartbeat_datagram(STREAM, 2, 90);
        assert_eq!(h.rx.ingest(&late, h.now), Ingest::Heartbeat);
        let report = h.rx.take_report(h.now, 0);
        assert_eq!((report.stalled_ms, report.stalls), (80, 1), "no free pass from a wrap");
    }

    /// A link that holds datagrams is still a stall when the source was a little slow too:
    /// only the host's own share of the gap is forgiven, the rest is charged.
    #[test]
    fn only_the_hosts_share_of_a_gap_is_forgiven() {
        let mut h = Harness::new();
        let s0 = h.send(&frame_bytes(1, 2_000), true, false);
        h.deliver(&s0.datagrams);
        h.drain();
        // The source takes 20 ms to draw, then the link holds the frame for 150 ms more.
        h.awake(Duration::from_millis(20));
        let s1 = h.send(&frame_bytes(2, 2_000), false, false);
        h.awake(Duration::from_millis(150));
        h.deliver(&s1.datagrams);
        let report = h.rx.take_report(h.now, 0);
        assert_eq!((report.stalled_ms, report.stalls), (150, 1), "170 ms of gap, 20 ms of it host");
        assert_eq!(h.drain().len(), 1);
    }

    /// The stamp is written when the host *builds* a datagram, not when it leaves, so the
    /// interval between two stamps can read a few milliseconds longer than the silence between
    /// their arrivals. That overshoot used to throw the whole reading away and charge the
    /// silence to the link, which is how a quiet loopback stream reported stalls with the host
    /// holding nothing (MEASUREMENTS, "a quiet loopback stream's stalls"). Read as a signed
    /// offset it says what it means: the host accounts for all of it.
    #[test]
    fn a_stamp_reading_just_past_the_silence_still_belongs_to_the_host() {
        let mut h = Harness::new();
        let s0 = h.send(&frame_bytes(1, 2_000), true, false);
        h.deliver(&s0.datagrams);
        h.drain();
        // 60 ms of silence ended by a beat the host stamped 67 ms after the frame.
        h.awake(Duration::from_millis(60));
        let covering = h.send_ms_lo().wrapping_add(7);
        assert_eq!(h.rx.ingest(&heartbeat_datagram(STREAM, 1, covering), h.now), Ingest::Heartbeat);
        let report = h.rx.take_report(h.now, 0);
        assert_eq!((report.stalled_ms, report.stalls), (0, 0), "the host's own silence");
        let silences = h.rx.stats().silences;
        assert_eq!((silences.host_covered, silences.in_flight), (1, 0));
        // A stamp *before* the one it follows is the unsigned subtraction wrapping, not
        // truncation: 246 ms of "host interval" inside a 60 ms gap is a datagram that overtook
        // its predecessor, and it buys no forgiveness.
        h.awake(Duration::from_millis(60));
        let backwards = covering.wrapping_sub(10);
        assert_eq!(
            h.rx.ingest(&heartbeat_datagram(STREAM, 2, backwards), h.now),
            Ingest::Heartbeat
        );
        let report = h.rx.take_report(h.now, 0);
        assert_eq!((report.stalled_ms, report.stalls), (60, 1), "no free pass from a wrap");
        assert_eq!(h.rx.stats().silences.stamp_backwards, 1);
    }

    /// A capture whose heartbeat runs at a third of its promised rate is still the host being
    /// quiet, not the link holding datagrams: the stamps say so, nothing is charged, and the
    /// bitrate controller keeps growing instead of freezing on a stall it never had.
    #[test]
    fn heartbeats_late_by_three_beats_are_charged_to_the_sender() {
        let mut h = Harness::new();
        let s0 = h.send(&frame_bytes(1, 2_000), true, false);
        h.deliver(&s0.datagrams);
        h.drain();
        let late = HEARTBEAT_AFTER * 3;
        assert!(late > cfg().stall_gap, "a beat this late outlasts the stall gap");
        for beat in 1..=8 {
            h.awake(late);
            let stamp = h.send_ms_lo();
            assert_eq!(
                h.rx.ingest(&heartbeat_datagram(STREAM, beat, stamp), h.now),
                Ingest::Heartbeat
            );
        }
        let report = h.rx.take_report(h.now, 0);
        assert_eq!((report.stalled_ms, report.stalls), (0, 0));
        let silences = h.rx.stats().silences;
        assert_eq!((silences.host_quiet, silences.ended_heartbeat), (8, 8));
        assert_eq!((silences.in_flight, silences.stamp_wrapped, silences.stamp_absent), (0, 0, 0));
        // What the counter is for: a sender-side silence must not hold the target down.
        let mut c = RateController::new(30_000_000);
        let decision = (0..64).find_map(|_| c.on_report(&report, 0, None));
        assert_eq!(decision.map(|d| d.verdict), Some(RateVerdict::Grow));
    }

    /// The other half of the same rule: 200 ms in which the host sent and the receiver was
    /// awake to see nothing arrive is one stall of 200 ms, and the controller freezes on it.
    #[test]
    fn two_hundred_milliseconds_in_flight_is_one_stall() {
        let mut h = Harness::new();
        let s0 = h.send(&frame_bytes(1, 2_000), true, false);
        h.deliver(&s0.datagrams);
        h.drain();
        // The host packetizes the next frame straight away; the link swallows it for 200 ms.
        let s1 = h.send(&frame_bytes(2, 2_000), false, false);
        h.awake(Duration::from_millis(200));
        h.deliver(&s1.datagrams);
        let report = h.rx.take_report(h.now, 0);
        assert_eq!((report.stalled_ms, report.stalls), (200, 1));
        let silences = h.rx.stats().silences;
        assert_eq!((silences.in_flight, silences.host_quiet, silences.receiver_dozed), (1, 0, 0));
        assert_eq!(h.drain().len(), 1, "the frame arrives late, not lost");
        let mut c = RateController::new(30_000_000);
        let decision = (0..64).find_map(|_| c.on_report(&report, 0, None));
        assert_eq!(decision.map(|d| d.verdict), Some(RateVerdict::Stall));
    }

    /// The same 200 ms with the receiver descheduled the whole way through is not evidence
    /// about the link at all: a task that never ran cannot say whether the datagrams were held
    /// on the wire or sat unread in its own socket, and blaming the link is what turns the
    /// machine's own load into a bitrate cut.
    #[test]
    fn a_silence_the_receiver_slept_through_is_not_a_stall() {
        let mut h = Harness::new();
        let s0 = h.send(&frame_bytes(1, 2_000), true, false);
        h.deliver(&s0.datagrams);
        h.drain();
        let s1 = h.send(&frame_bytes(2, 2_000), false, false);
        h.advance(Duration::from_millis(200));
        assert!(!h.rx.stalled(h.now), "not awake to see it");
        h.deliver(&s1.datagrams);
        let report = h.rx.take_report(h.now, 0);
        assert_eq!((report.stalled_ms, report.stalls), (0, 0));
        let silences = h.rx.stats().silences;
        assert_eq!((silences.receiver_dozed, silences.in_flight), (1, 0));
        assert_eq!(silences.dozed_ms_max, 175, "200 ms less the tick the loop owed");
        assert_eq!(h.drain().len(), 1);
    }

    /// The same sleep, but the executor polls the ready timer before the socket. `tick` must not
    /// spend the doze on itself: the arrival a moment later is what settles the silence, and if
    /// the credit did not survive the tick the whole 200 ms would be charged to the link.
    #[test]
    fn a_tick_that_wakes_first_does_not_hand_the_silence_to_the_link() {
        let mut h = Harness::new();
        let s0 = h.send(&frame_bytes(1, 2_000), true, false);
        h.deliver(&s0.datagrams);
        h.drain();
        let s1 = h.send(&frame_bytes(2, 2_000), false, false);
        h.advance(Duration::from_millis(200));
        // The timer wins the race; the datagrams are still in the socket.
        let _policy = h.tick();
        h.deliver(&s1.datagrams);
        let report = h.rx.take_report(h.now, 0);
        assert_eq!((report.stalled_ms, report.stalls), (0, 0), "the tick banked the sleep");
        let silences = h.rx.stats().silences;
        assert_eq!((silences.receiver_dozed, silences.in_flight), (1, 0));
        assert_eq!(silences.dozed_ms_max, 175, "200 ms less the tick the loop owed");
        assert_eq!(h.drain().len(), 1);
        // And the credit is spent, not carried. A frame straight away, so the stamp the next
        // silence is measured against is this instant's and not the one from before the sleep.
        let s2 = h.send(&frame_bytes(3, 2_000), false, false);
        h.deliver(&s2.datagrams);
        h.drain();
        let s3 = h.send(&frame_bytes(4, 2_000), false, false);
        h.awake(Duration::from_millis(200));
        h.deliver(&s3.datagrams);
        let report = h.rx.take_report(h.now, 0);
        assert_eq!(report.stalls, 1, "a silence the receiver watched is still a stall");
        assert_eq!(h.rx.stats().silences.in_flight, 1, "and it is charged to the link");
    }

    /// The sleep is credited once, not once per report. A stall that stays on has to keep
    /// reporting time, or the windows after the first read as healthy and growth resumes into
    /// a link that is still holding everything.
    #[test]
    fn a_sleep_is_forgiven_once_and_a_stall_that_stays_on_keeps_reporting() {
        let mut h = Harness::new();
        let s0 = h.send(&frame_bytes(1, 2_000), true, false);
        h.deliver(&s0.datagrams);
        h.drain();
        let _held = h.send(&frame_bytes(2, 2_000), false, false);
        // Asleep for 200 ms, then awake. The first tick of the waking loop is 225 ms after the
        // last one, so 200 ms of sleep is banked and 50 ms of the 250 ms silence is unexplained
        // — exactly a stall gap, and the whole of the bank is spent paying for the rest.
        h.advance(Duration::from_millis(200));
        assert!(!h.rx.stalled(h.now), "25 ms unexplained is under the gap");
        h.awake(Duration::from_millis(50));
        let first = h.rx.take_report(h.now, 0);
        assert_eq!((first.stalled_ms, first.stalls), (50, 0), "250 ms less the 200 ms slept");
        for window in 0..3 {
            h.awake(Duration::from_millis(50));
            let r = h.rx.take_report(h.now, 0);
            assert_eq!(
                (r.stalled_ms, r.stalls),
                (50, 0),
                "window {window}: awake and still stalled, the sleep is already spent"
            );
        }
    }

    /// The host's silence and the receiver's sleep can be the same stretch of time. Forgiving
    /// both in full excuses it twice and hides a real hold.
    #[test]
    fn sleep_inside_the_hosts_own_silence_is_not_forgiven_twice() {
        let mut h = Harness::new();
        let s0 = h.send(&frame_bytes(1, 2_000), true, false);
        h.deliver(&s0.datagrams);
        h.drain();
        // The receiver sleeps through the first 100 ms — which is also the host's own silence.
        h.advance(Duration::from_millis(100));
        let _woke = h.tick();
        // The host builds the frame here, 100 ms in; the link then holds it for 100 ms with the
        // receiver awake throughout.
        let held = h.send(&frame_bytes(2, 2_000), false, false);
        h.awake(Duration::from_millis(100));
        h.deliver(&held.datagrams);
        let report = h.rx.take_report(h.now, 0);
        assert_eq!(report.stalls, 1, "the link held it for 100 ms and the receiver watched");
        let silences = h.rx.stats().silences;
        assert_eq!((silences.in_flight, silences.receiver_dozed), (1, 0));
    }

    #[test]
    fn a_stall_longer_than_max_hold_gives_up() {
        let mut h = Harness::new();
        let s0 = h.send(&frame_bytes(1, 2_000), true, false);
        h.deliver(&s0.datagrams);
        h.drain();
        let s1 = h.send(&frame_bytes(2, 6_000), false, false);
        h.deliver(&s1.datagrams[..3]);
        h.advance(nack_delay());
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

    /// The host says the capture source has produced nothing yet: asking it for a refresh
    /// cannot help, so the receiver stops until the host says the source is live again. This is
    /// the storm guard — a window that has not drawn used to draw a refresh request every
    /// backoff period for as long as it stayed hidden.
    #[test]
    fn an_idle_source_stops_the_refresh_requests() {
        let mut h = Harness::new();
        assert!(h.tick().is_empty(), "the constructor counts as the first request");
        h.advance(cfg().refresh_repeat + RTT * 2);
        assert_eq!(h.tick(), vec![Action::RequestRefresh { last_good_frame: 0 }]);

        h.rx.set_source_live(false);
        assert!(!h.rx.source_live());
        for _ in 0..1000 {
            h.advance(Duration::from_millis(10));
            assert!(h.tick().is_empty(), "an idle source must not be asked again");
        }

        // The host reports the source live: asking resumes, and from the shortest wait, because
        // the first frame is now worth waiting for.
        h.rx.set_source_live(true);
        h.advance(cfg().refresh_repeat + RTT * 2);
        assert_eq!(h.tick(), vec![Action::RequestRefresh { last_good_frame: 0 }]);
    }

    /// What clears what: a heartbeat proves the host is alive, so the retry cap starts over, but
    /// nothing on the wire lifts the idle suppression while the host's word is that the source is
    /// idle. Control and video travel separately, so a fragment captured before the target went
    /// away arrives after the statement that it did; taking that as proof would put the receiver
    /// back to live behind the host's back, and the host — which reports the change, not the
    /// state — would never say it again. Only the host takes its own statement back.
    #[test]
    fn a_heartbeat_restarts_the_cap_and_only_the_host_lifts_the_idle_hint() {
        let mut h = Harness::new();
        h.rx.set_source_live(false);
        let mut sent = 0;
        for _ in 0..600 {
            h.advance(Duration::from_millis(10));
            sent += h.tick().len();
        }
        assert_eq!(sent, 0, "an idle source is not asked");

        // A heartbeat: the host is there, the source still is not. The cap is fresh, but
        // suppression holds, so still nothing goes out.
        assert_eq!(h.rx.ingest(&heartbeat_datagram(STREAM, 0, 0), h.now), Ingest::Heartbeat);
        for _ in 0..600 {
            h.advance(Duration::from_millis(10));
            assert!(h.tick().is_empty(), "a heartbeat does not make the source live");
        }
        assert!(!h.rx.source_live());

        // A video fragment does not either, while that word stands: this is the frame that was
        // already in flight when the window went away, and it says nothing about now.
        let s0 = h.send(&frame_bytes(1, 6_000), true, false);
        h.deliver(&s0.datagrams[..1]);
        assert!(!h.rx.source_live(), "a frame in flight before the host spoke is not proof");
        let mut asked = 0;
        for _ in 0..600 {
            h.advance(Duration::from_millis(10));
            asked += h.tick().iter().filter(|a| matches!(a, Action::RequestRefresh { .. })).count();
        }
        // One: giving up on that half-arrived frame asks once, because a frame that really was
        // on the wire is worth one question. What does not happen is the repeat — the backoff
        // loop stays off, so the heartbeats above cannot restart a cap that is never reached.
        assert_eq!(asked, 1, "an idle source is asked once for the frame it lost, then not again");
        assert!(!h.rx.source_live());

        // The host takes it back — which it does on the tick after the target draws again — and
        // asking resumes from the shortest wait.
        h.rx.set_source_live(true);
        assert!(h.rx.source_live());
        h.advance(cfg().max_hold);
        assert!(
            h.tick().contains(&Action::RequestRefresh { last_good_frame: 0 }),
            "asking resumes once the host says the source draws again"
        );
    }

    /// The same fragment against a host that never sends the hint at all: there the receiver has
    /// only the stream to go on, the suppression never comes on, and nothing is suppressed.
    #[test]
    fn a_host_that_never_sends_the_hint_keeps_asking() {
        let mut h = Harness::new();
        assert!(h.rx.source_live(), "live until the host says otherwise");
        let s0 = h.send(&frame_bytes(1, 6_000), true, false);
        h.deliver(&s0.datagrams[..1]);
        assert!(h.rx.source_live());
        h.advance(cfg().max_hold);
        assert!(h.tick().contains(&Action::RequestRefresh { last_good_frame: 0 }));
    }

    /// Fallback for a host that never sends the hint: the repeats stop on their own. With the
    /// doubling backoff the cap spans about 17 s of asking, then silence until the stream moves.
    #[test]
    fn refresh_requests_give_up_after_the_cap() {
        let mut h = Harness::new();
        let mut sent = 0;
        for _ in 0..6000 {
            h.advance(Duration::from_millis(10));
            sent += h.tick().len();
        }
        let cap = usize::try_from(cfg().refresh_max_repeats).unwrap();
        assert_eq!(sent, cap, "{sent} requests in a minute of silence, cap {cap}");

        // A frame arriving is what restarts it: the stream is alive again.
        let s0 = h.send(&frame_bytes(1, 2_000), true, false);
        h.deliver(&s0.datagrams);
        assert_eq!(h.drain().len(), 1);
        let s1 = h.send(&frame_bytes(2, 6_000), false, false);
        h.deliver(&s1.datagrams[..3]);
        h.advance(cfg().max_hold);
        let actions = h.tick();
        assert_eq!(
            actions.last(),
            Some(&Action::RequestRefresh { last_good_frame: 0 }),
            "{actions:?}"
        );
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
        let report = h.rx.take_report(h.now, 1);
        assert_eq!(
            (report.frames_ok, report.frames_fec, report.frames_lost, report.datagrams_lost),
            (2, 1, 0, 2)
        );
        assert_eq!(report.last_host_send_ts_us, 1_000);
        assert_eq!((report.acked_ltr_len, report.acked_ltr[0]), (1, 0xabcd));
        assert_eq!(report.late_frames, 1);
        assert_eq!(report.queue_depth, 0);
        assert_eq!((report.stalled_ms, report.stalls), (0, 0), "4 ms of silence is not a stall");
        let empty = h.rx.take_report(h.now, 0);
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
