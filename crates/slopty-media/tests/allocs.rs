//! Allocation budgets on the media path, counted by `slopty_testkit::alloc` on the test's own
//! thread: cutting a frame into datagrams, putting it back together, and handing one frame to
//! many viewers. The counts are exact and do not depend on the machine's load, so they gate;
//! a budget that breaks names what grew. `docs/decisions/testing.md`, "Allocation budgets".

#[cfg(test)]
mod allocs {
    use std::time::{Duration, Instant};

    use bytes::Bytes;
    use slopty_core::StreamId;
    use slopty_media::{Config, EncodedFrame, Packetizer, Reassembler};
    use slopty_testkit::alloc::{self, Allocs, Counting};

    #[global_allocator]
    static ALLOC: Counting = Counting;

    const STREAM: StreamId = StreamId(9);
    /// A P-frame of the size a busy desktop stream sends (MEASUREMENTS, "packetize").
    const P_FRAME: usize = 62_000;
    /// Frames sent before counting: enough to fill the retransmit history and every buffer
    /// that grows to a steady size.
    const WARM_UP: u32 = 64;
    /// Frames counted.
    const COUNTED: u32 = 64;

    fn frame(data: &[u8], n: u32) -> EncodedFrame<'_> {
        EncodedFrame {
            data,
            keyframe: n == 0,
            ltr_token: None,
            ltr_refresh: false,
            discardable: false,
            capture_ts_us: n,
            stripes: 0,
            region: None,
        }
    }

    fn body(len: usize) -> Vec<u8> {
        (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect()
    }

    /// One frame of `len` bytes packetized at `parity` in steady state: what it allocated, the
    /// bytes it put on the wire and in how many datagrams.
    fn packetize_once(len: usize, parity: u16) -> (Allocs, u64, u64) {
        assert!(alloc::installed(), "the counting allocator is this binary's");
        let data = body(len);
        let mut p = Packetizer::new(STREAM);
        p.set_parity_permille(parity);
        for n in 0..WARM_UP {
            p.packetize(&frame(&data, n), 0, |_| {}).unwrap();
        }
        let ((wire, count), used) = alloc::measure(|| {
            let sent = p.packetize(&frame(&data, WARM_UP), 0, |_| {}).unwrap();
            (sent.bytes(), sent.datagrams.len())
        });
        (used, u64::try_from(wire).unwrap(), u64::try_from(count).unwrap())
    }

    /// A frame costs three blocks however big it is and whatever its parity: the one buffer
    /// every datagram is a slice of, its shared handle, and the list of slices. Its bytes are
    /// the wire's, plus a handle per datagram and the shared handle's few.
    #[test]
    fn packetizing_a_frame_allocates_three_blocks() {
        for parity in [200_u16, 0] {
            for len in [P_FRAME, 300_000] {
                let (used, wire, count) = packetize_once(len, parity);
                eprintln!("packetize {len} B at {parity}‰: {used}; {wire} B in {count} datagrams");
                assert_eq!(used.blocks, 3, "{len} B at {parity}‰: {used}");
                let handles = count * u64::try_from(size_of::<Bytes>()).unwrap();
                assert!(
                    used.bytes <= wire + handles + SHARED_HANDLE,
                    "{len} B at {parity}‰: {used} for {wire} B in {count} datagrams"
                );
            }
        }
    }

    /// What `Bytes` allocates to share a buffer, with room to spare.
    const SHARED_HANDLE: u64 = 64;

    /// Every datagram of `frames` P-frames, cut with no parity.
    fn datagrams(frames: u32) -> Vec<Vec<Bytes>> {
        let data = body(P_FRAME);
        let mut p = Packetizer::new(STREAM);
        p.set_parity_permille(0);
        (0..frames)
            .map(|n| p.packetize(&frame(&data, n), 0, |_| {}).unwrap().datagrams.clone())
            .collect()
    }

    /// Putting a frame back together when every data fragment arrives costs three blocks: its
    /// fragment table, its body and the body's shared handle, once the reassembler's own
    /// tables have grown. Its bytes are the wire's, within 5 %.
    #[test]
    fn reassembling_a_frame_allocates_a_fixed_few_blocks() {
        assert!(alloc::installed(), "the counting allocator is this binary's");
        let all = datagrams(WARM_UP + COUNTED);
        let (warm, counted) = all.split_at(usize::try_from(WARM_UP).unwrap());
        let start = Instant::now();
        let mut rx = Reassembler::new(STREAM, Config::default(), start);
        let mut delivered = 0_usize;
        let mut feed = |rx: &mut Reassembler, frames: &[Vec<Bytes>], from: u32| {
            for (n, datagrams) in (from..).zip(frames) {
                let now = start + Duration::from_millis(u64::from(n) * 16);
                for d in datagrams {
                    let _ingested = rx.ingest(d, now);
                }
                while let Some(out) = rx.next_frame() {
                    delivered += out.data.len();
                }
                if n % 16 == 15 {
                    let _report = rx.take_report(now, 0);
                }
            }
        };
        feed(&mut rx, warm, 0);
        let ((), used) = alloc::measure(|| feed(&mut rx, counted, WARM_UP));
        let per_frame = used.blocks / u64::from(COUNTED);
        eprintln!("reassemble {COUNTED} frames of {P_FRAME} B: {used} ({per_frame} per frame)");
        assert_eq!(delivered, P_FRAME * all.len(), "every frame came out whole");
        let budget = 3 * u64::from(COUNTED) + REASSEMBLE_AMORTISED;
        assert!(used.blocks <= budget, "{used}, budget {budget} blocks over {COUNTED} frames");
        let wire: u64 = counted.iter().flatten().map(|d| u64::try_from(d.len()).unwrap()).sum();
        assert!(used.bytes <= wire + wire / 20, "{used} for {wire} B on the wire");
    }

    /// Blocks beyond three a frame (its fragment table, its body and the body's shared
    /// handle) over the counted frames: the delivery queue's and the report window's amortised
    /// growth, and the frame index's nodes.
    const REASSEMBLE_AMORTISED: u64 = 12;

    /// A frame is cut once however many viewers take it: every viewer gets the same datagrams,
    /// shared, so eight viewers cost what one does.
    #[test]
    fn a_frame_for_many_viewers_is_cut_once() {
        assert!(alloc::installed(), "the counting allocator is this binary's");
        let data = body(P_FRAME);
        let fan_out = |viewers: usize| {
            let mut p = Packetizer::new(STREAM);
            for n in 0..WARM_UP {
                p.packetize(&frame(&data, n), 0, |_| {}).unwrap();
            }
            let mut queues: Vec<Vec<Bytes>> =
                std::iter::repeat_with(|| Vec::with_capacity(256)).take(viewers).collect();
            let ((), used) = alloc::measure(|| {
                let sent = p.packetize(&frame(&data, WARM_UP), 0, |_| {}).unwrap();
                for queue in &mut queues {
                    queue.extend(sent.datagrams.iter().cloned());
                }
            });
            assert!(queues.iter().all(|q| !q.is_empty()), "every viewer was handed the frame");
            used
        };
        let one = fan_out(1);
        let eight = fan_out(8);
        eprintln!("one viewer: {one}; eight viewers: {eight}");
        assert_eq!(eight, one, "eight viewers cost what one does");
    }
}
