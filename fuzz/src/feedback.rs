//! The worker's side of a client's loss feedback: NACKs answered from the packetizer's history,
//! and receiver reports folded into the bitrate controller and the parity ratio.
//!
//! Every field is the client's to choose. A retransmission must be a datagram of the frame asked
//! for, a data fragment of it, marked as a retransmission; the bitrate must stay between the
//! floor and the client's ceiling; the parity ratio must stay within its bounds.

use arbitrary::{Arbitrary, Unstructured};
use slopty_core::{Duration, StreamId};
use slopty_media::{
    AudioCopies, EncodedFrame, MAX_AUDIO_COPIES, Packetizer, PathSample, RateController, Redundancy,
};
use slopty_proto::datagram::ClientDatagram;
use slopty_proto::media::{MediaHeader, flags};
use slopty_proto::screen::{Feedback, ReceiverReport};

const STREAM: StreamId = StreamId(3);

/// The floor under every target (`slopty_media::rate::MIN_BPS`).
const MIN_BPS: u32 = 1_000_000;

#[derive(Arbitrary, Debug)]
struct Plan {
    max_bps: u32,
    parity_permille: u16,
    ops: Vec<Op>,
}

#[derive(Arbitrary, Debug)]
enum Op {
    /// The worker sends a frame of `len` bytes.
    Frame { len: u16, keyframe: bool },
    /// A NACK, as its fields.
    Nack { frame: u32, fragments: Vec<u16> },
    /// A client datagram, as its bytes: a NACK or a refresh among them is acted on.
    Datagram(Vec<u8>),
    /// A receiver report, with the path the transport sees when it arrives.
    Report { report: Report, datagrams_sent: u32, rtt_ns: Option<(u64, u64)> },
    /// The client's ceiling moves.
    Ceiling(u32),
}

/// [`ReceiverReport`]'s fields, as the client may fill them.
#[derive(Arbitrary, Debug)]
struct Report {
    frames_ok: u32,
    frames_fec: u32,
    frames_lost: u32,
    datagrams_lost: u32,
    last_worker_send_ts_us: u32,
    hold_p50_ns: u64,
    hold_p95_ns: u64,
    owd_jitter_ns: u64,
    queue_depth: u8,
    late_frames: u32,
    acked_ltr: [u64; 4],
    acked_ltr_len: u8,
    stalled_ms: u16,
    stalls: u16,
    audio_received: u16,
    audio_lost: u16,
}

impl From<&Report> for ReceiverReport {
    fn from(r: &Report) -> Self {
        Self {
            frames_ok: r.frames_ok,
            frames_fec: r.frames_fec,
            frames_lost: r.frames_lost,
            datagrams_lost: r.datagrams_lost,
            last_worker_send_ts_us: r.last_worker_send_ts_us,
            hold_p50: Duration::from_nanos(r.hold_p50_ns),
            hold_p95: Duration::from_nanos(r.hold_p95_ns),
            owd_jitter: Duration::from_nanos(r.owd_jitter_ns),
            queue_depth: r.queue_depth,
            late_frames: r.late_frames,
            acked_ltr: r.acked_ltr,
            acked_ltr_len: r.acked_ltr_len,
            stalled_ms: r.stalled_ms,
            stalls: r.stalls,
            audio_received: r.audio_received,
            audio_lost: r.audio_lost,
        }
    }
}

/// Run the plan the fuzzer's bytes describe.
pub fn run(data: &[u8]) {
    let input = Unstructured::new(data);
    let Ok(plan) = Plan::arbitrary_take_rest(input) else { return };
    let mut worker = Worker::new(&plan);
    for op in &plan.ops {
        worker.step(op);
    }
}

struct Worker {
    packetizer: Packetizer,
    rate: RateController,
    ceiling: u32,
    redundancy: Redundancy,
    audio: AudioCopies,
    /// Reports taken, each a 50 ms report period on the audio copies' clock.
    reports: u64,
}

impl Worker {
    fn new(plan: &Plan) -> Self {
        let mut packetizer = Packetizer::new(STREAM);
        packetizer.set_parity_permille(plan.parity_permille);
        Self {
            packetizer,
            rate: RateController::new(plan.max_bps),
            ceiling: plan.max_bps.max(MIN_BPS),
            redundancy: Redundancy::default(),
            audio: AudioCopies::default(),
            reports: 0,
        }
    }

    fn step(&mut self, op: &Op) {
        match op {
            Op::Frame { len, keyframe } => {
                let data = vec![0xa5; usize::from(*len)];
                let frame = EncodedFrame {
                    data: &data,
                    keyframe: *keyframe,
                    ltr_token: None,
                    ltr_refresh: false,
                    capture_ts_us: 0,
                    discardable: false,
                };
                let _sent = self.packetizer.packetize(&frame, 0, |_| {});
            }
            Op::Nack { frame, fragments } => self.nack(*frame, fragments),
            Op::Datagram(bytes) => {
                if let Some(ClientDatagram::Feedback(Feedback::Nack { frame, fragments, .. })) =
                    ClientDatagram::decode(bytes)
                {
                    self.nack(frame, &fragments);
                }
            }
            Op::Report { report, datagrams_sent, rtt_ns } => {
                let report = ReceiverReport::from(report);
                let path =
                    rtt_ns.map(|(rtt, cwnd)| PathSample { rtt: Duration::from_nanos(rtt), cwnd });
                if let Some(decision) = self.rate.on_report(&report, *datagrams_sent, path) {
                    assert!(
                        (MIN_BPS..=self.ceiling).contains(&decision.target_bps),
                        "target {} outside {MIN_BPS}..={}",
                        decision.target_bps,
                        self.ceiling
                    );
                }
                let permille = self.redundancy.on_report(&report, *datagrams_sent);
                assert!(
                    (Redundancy::MIN..=Redundancy::MAX).contains(&permille),
                    "parity ratio {permille}‰ outside its bounds"
                );
                self.reports = self.reports.saturating_add(1);
                let copies = self.audio.on_report(&report, self.reports.saturating_mul(50_000));
                assert!(copies <= MAX_AUDIO_COPIES, "{copies} audio copies");
            }
            Op::Ceiling(max) => {
                self.rate.set_max(*max);
                self.ceiling = (*max).max(MIN_BPS);
            }
        }
    }

    fn nack(&mut self, frame: u32, fragments: &[u16]) {
        let data_count = self
            .packetizer
            .retransmit(frame, &[])
            .iter()
            .find_map(|d| MediaHeader::parse(d).map(|(h, _)| h.data_count.get()));
        for datagram in self.packetizer.retransmit(frame, fragments) {
            let Some((header, _payload)) = MediaHeader::parse(&datagram) else {
                panic!("a retransmission that is not a media datagram");
            };
            assert_eq!(header.frame.get(), frame, "a retransmission of another frame");
            assert!(header.flags & flags::RETRANSMIT != 0, "a retransmission not marked as one");
            assert!(
                data_count.is_some_and(|count| header.index.get() < count),
                "a retransmission of a parity fragment or past the frame"
            );
        }
    }
}
