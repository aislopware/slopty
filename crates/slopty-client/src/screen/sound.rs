//! A worker's sound on this client: one decoder and one player for every stream of the
//! connection (`docs/decisions/audio.md`, "One sound per worker on a client, not one per
//! stream").
//!
//! The worker captures the sound once for this client, from every application behind its
//! streams, and sends it on [`SOUND`]. The first stream a view opens starts it here
//! ([`ScreenRouter::sound`]); every stream of the connection holds it, and the last one let go
//! ends it. Its mute is the worker's: one switch, which every one of its tiles reads.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use slopty_codec::audio::{Arrival as AudioArrival, Conceal, OpusDecoder, Player};
use slopty_media::{MAX_AUDIO_COPIES, SoundCount, parse_audio};
use slopty_proto::ClientMsg;
use slopty_proto::media::{Kind, MediaHeader, SOUND};
use slopty_proto::screen::ScreenRequest;
use tokio::sync::{mpsc, oneshot, watch};

use super::{Arrival, REPORT_EVERY, ScreenRouter, ScreenStats};

/// Whether a worker's sound is silenced here, by the person's pill.
#[derive(Debug, Default)]
pub(super) struct Muted(AtomicBool);

impl Muted {
    /// Whether playback is silenced.
    pub(super) fn get(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Silence or resume playback.
    pub(super) fn set(&self, muted: bool) {
        self.0.store(muted, Ordering::Relaxed);
    }
}

/// What the sound has done so far, laid over each of the worker's streams' counters.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) struct SoundStats {
    /// Datagrams that came on [`SOUND`].
    pub datagrams: u64,
    /// Packets decoded and played (or held, while muted).
    pub packets: u64,
    /// Packets missing from the sequence, late, or landed before the player was open.
    pub lost: u64,
    /// Lost packets stood in for.
    pub concealed: u64,
    /// Lost packets decoded from a copy a later datagram carried.
    pub recovered: u64,
    /// Times playback ran dry under sound.
    pub underruns: u64,
    /// Audio dropped to keep the delay at the target.
    pub trimmed: Duration,
    /// Audio played twice to keep the depth at the target.
    pub stretched: Duration,
    /// Depth the player aims for.
    pub target: Duration,
}

impl SoundStats {
    /// `stats` with the sound's counters in its audio fields.
    pub(super) const fn over(self, mut stats: ScreenStats) -> ScreenStats {
        stats.audio_packets = self.packets;
        stats.audio_lost = self.lost;
        stats.audio_concealed = self.concealed;
        stats.audio_recovered = self.recovered;
        stats.audio_underruns = self.underruns;
        stats.audio_trimmed = self.trimmed;
        stats.audio_stretched = self.stretched;
        stats.audio_target = self.target;
        stats
    }
}

/// The worker's sound, held by each of its streams' handles. The last one dropped ends it.
#[derive(Debug)]
pub(super) struct Sound {
    muted: Arc<Muted>,
    stats: watch::Receiver<SoundStats>,
    /// Dropped with the last handle, which ends the task.
    _ends: oneshot::Sender<()>,
    /// The router [`SOUND`] is attached on; `None` for a sound with nothing behind it.
    router: Option<ScreenRouter>,
}

impl Drop for Sound {
    /// Let go of [`SOUND`], so what the worker still sends is dropped rather than kept for a
    /// sound to come. Under the router's sound slot, where a new sound is made: one a new
    /// stream started meanwhile keeps the lane it attached.
    fn drop(&mut self) {
        let Some(router) = &self.router else { return };
        let slot = router.sound.lock();
        if slot.upgrade().is_none() {
            router.detach(SOUND);
        }
    }
}

impl Sound {
    /// Start playing what `router` brings on [`SOUND`], reporting what arrived on `control`.
    pub(super) fn spawn(
        runtime: &tokio::runtime::Handle,
        router: &ScreenRouter,
        muted: Arc<Muted>,
        control: mpsc::Sender<ClientMsg>,
    ) -> Self {
        let (stats_tx, stats) = watch::channel(SoundStats::default());
        let (ends, ended) = oneshot::channel();
        let task = Task {
            datagrams: router.attach(SOUND),
            out: control,
            muted: Arc::clone(&muted),
            stats: stats_tx,
            counters: SoundStats::default(),
            count: SoundCount::default(),
            player: Slot::Unopened,
            seq: None,
            conceal: Conceal::default(),
            stand_in: Vec::new(),
        };
        // Not kept: the task ends on its own once `ends` drops.
        let _task = runtime.spawn(task.run(ended));
        Self { muted, stats, _ends: ends, router: Some(router.clone()) }
    }

    /// A sound with nothing behind it, for a handle with no worker.
    #[cfg(feature = "headless")]
    pub(super) fn detached(muted: Arc<Muted>) -> Self {
        let (_stats_tx, stats) = watch::channel(SoundStats::default());
        let (ends, _ended) = oneshot::channel();
        Self { muted, stats, _ends: ends, router: None }
    }

    pub(super) fn muted(&self) -> &Muted {
        &self.muted
    }

    pub(super) fn stats(&self) -> SoundStats {
        *self.stats.borrow()
    }
}

/// Playback, created on the first packet.
enum Slot {
    Unopened,
    /// The player is being created on a blocking thread: `CoreAudio`'s first client in a process
    /// initialises the HAL, which took ~7 s on the mac-studio (`docs/MEASUREMENTS.md`,
    /// 2026-09-13), and the task must keep counting and reporting meanwhile.
    Opening(oneshot::Receiver<Result<Playback, slopty_codec::CodecError>>),
    Open(Playback),
    /// Playback could not start; packets are dropped.
    Failed,
}

struct Playback {
    decoder: OpusDecoder,
    player: Player,
}

impl Playback {
    fn new() -> Result<Self, slopty_codec::CodecError> {
        Ok(Self { decoder: OpusDecoder::new()?, player: Player::new()? })
    }
}

struct Task {
    datagrams: mpsc::Receiver<Arrival>,
    out: mpsc::Sender<ClientMsg>,
    muted: Arc<Muted>,
    stats: watch::Sender<SoundStats>,
    counters: SoundStats,
    /// What arrived since the last report.
    count: SoundCount,
    player: Slot,
    /// The last packet played.
    seq: Option<u32>,
    /// The previous packet, for short gaps.
    conceal: Conceal,
    /// Scratch for the stand-in samples.
    stand_in: Vec<f32>,
}

impl Task {
    async fn run(mut self, mut ended: oneshot::Receiver<()>) {
        let mut report = tokio::time::interval(REPORT_EVERY);
        report.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                arrival = self.datagrams.recv() => {
                    let Some((at, datagram)) = arrival else { break };
                    self.ingest(&datagram, at);
                    while let Ok((at, datagram)) = self.datagrams.try_recv() {
                        self.ingest(&datagram, at);
                    }
                }
                _instant = report.tick() => {
                    if !self.report() {
                        break;
                    }
                }
                _ended = &mut ended => break,
            }
        }
        tracing::debug!("the worker's sound finished");
    }

    fn ingest(&mut self, datagram: &Bytes, at: Instant) {
        self.counters.datagrams = self.counters.datagrams.saturating_add(1);
        let Some((header, _payload)) = MediaHeader::parse(datagram) else { return };
        if header.kind() != Some(Kind::Audio) {
            return;
        }
        let header = *header;
        let payload = datagram.slice(slopty_proto::media::HEADER_BYTES..);
        let Some(packets) = parse_audio(header.data_count.get(), &payload) else { return };
        let seq = header.frame.get();
        self.count.arrived(seq);
        self.play(seq, &packets.packet, &packets.earlier, at);
    }

    /// Move the player towards open: start creating it on the first packet, pick it up once
    /// the blocking thread is done. Never waits.
    fn open(&mut self) {
        self.player = match std::mem::replace(&mut self.player, Slot::Failed) {
            Slot::Unopened => {
                let (tx, rx) = oneshot::channel();
                drop(tokio::task::spawn_blocking(move || {
                    let _no_task = tx.send(Playback::new());
                }));
                Slot::Opening(rx)
            }
            Slot::Opening(mut rx) => match rx.try_recv() {
                Ok(Ok(playback)) => Slot::Open(playback),
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "audio playback unavailable");
                    Slot::Failed
                }
                Err(oneshot::error::TryRecvError::Empty) => Slot::Opening(rx),
                Err(oneshot::error::TryRecvError::Closed) => Slot::Failed,
            },
            open_or_failed => open_or_failed,
        };
    }

    /// Decode and queue one Opus packet that arrived at `at`; late duplicates are dropped, gaps
    /// counted. The end of a gap that `earlier` (the packets before it, carried again, nearest
    /// first) covers is decoded from those copies, and only the rest is concealed. A packet that
    /// lands while the player is still opening (or after it failed) is lost to playback and
    /// counted as such.
    fn play(
        &mut self,
        seq: u32,
        payload: &Bytes,
        earlier: &[Option<Bytes>; MAX_AUDIO_COPIES],
        at: Instant,
    ) {
        self.open();
        let Slot::Open(playback) = &mut self.player else {
            self.counters.lost = self.counters.lost.saturating_add(1);
            return;
        };
        let muted = self.muted.get();
        if let Some(last) = self.seq {
            let gap = seq.wrapping_sub(last);
            if gap == 0 || gap > u32::MAX / 2 {
                self.counters.lost = self.counters.lost.saturating_add(1);
                return;
            }
            if gap > 1 {
                let missing = gap.saturating_sub(1);
                self.counters.lost = self.counters.lost.saturating_add(u64::from(missing));
                let copies = earlier.iter().take_while(|copy| copy.is_some()).count();
                let recovered = missing.min(u32::try_from(copies).unwrap_or(0));
                let concealed = missing.saturating_sub(recovered);
                self.stand_in.clear();
                if concealed > 0 {
                    self.conceal.fill(concealed, &mut self.stand_in);
                    if !self.stand_in.is_empty() {
                        self.counters.concealed =
                            self.counters.concealed.saturating_add(u64::from(concealed));
                    }
                }
                // Oldest first; like a stand-in, a recovered packet keeps the gap's time and
                // reads no depth: it arrived with the packet after it, not late.
                let copies =
                    earlier.get(..usize::try_from(recovered).unwrap_or(0)).unwrap_or_default();
                for copy in copies.iter().rev().flatten() {
                    match playback.decoder.decode(copy) {
                        Ok(pcm) => {
                            self.stand_in.extend_from_slice(self.conceal.take(pcm));
                            self.counters.recovered = self.counters.recovered.saturating_add(1);
                        }
                        Err(e) => tracing::debug!(error = %e, "opus decode of a copy"),
                    }
                }
                if !self.stand_in.is_empty() && !muted {
                    playback.player.conceal(&self.stand_in);
                }
            }
        }
        self.seq = Some(seq);
        let arrival = AudioArrival { seq, at };
        match playback.decoder.decode(payload) {
            Ok(pcm) => {
                let pcm = self.conceal.take(pcm);
                if muted {
                    playback.player.hold(arrival);
                } else {
                    playback.player.push(pcm, arrival);
                }
                self.counters.packets = self.counters.packets.saturating_add(1);
            }
            Err(e) => tracing::debug!(error = %e, "opus decode"),
        }
    }

    /// Publish the counters, and tell the worker what arrived while anything did, which is what
    /// sizes the copies its datagrams carry. `false` once the connection is gone.
    fn report(&mut self) -> bool {
        if let Slot::Open(playback) = &self.player {
            let playout = playback.player.stats();
            self.counters.underruns = playout.underruns;
            self.counters.trimmed = playout.trimmed;
            self.counters.stretched = playout.stretched;
            self.counters.target = playout.target;
        }
        let counters = self.counters;
        self.stats.send_if_modified(|stats| std::mem::replace(stats, counters) != counters);
        let report = self.count.take();
        if report.received == 0 && report.lost == 0 {
            return true;
        }
        // A full queue loses the window's count, and the next report brings the next one.
        let sent = self.out.try_send(ClientMsg::Screen(ScreenRequest::SoundReport(report)));
        !matches!(sent, Err(mpsc::error::TrySendError::Closed(_)))
    }
}

#[cfg(test)]
mod tests {
    use slopty_codec::audio::{CHANNELS, FRAME_SAMPLES, OpusEncoder};
    use slopty_media::audio_datagram;
    use slopty_proto::screen::SoundReport;

    use super::*;

    /// The sound on its own runtime, with what it sent back caught.
    struct Harness {
        rt: tokio::runtime::Runtime,
        router: ScreenRouter,
        sound: Sound,
        control: mpsc::Receiver<ClientMsg>,
        routed: u64,
        reports: Vec<SoundReport>,
    }

    impl Harness {
        fn start() -> Self {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let router = ScreenRouter::with_loss(0);
            let (control_tx, control) = mpsc::channel(64);
            let sound = Sound::spawn(rt.handle(), &router, Arc::default(), control_tx);
            Self { rt, router, sound, control, routed: 0, reports: Vec::new() }
        }

        fn route(&mut self, datagram: Bytes) {
            self.routed = self.routed.saturating_add(1);
            self.router.route(datagram, Instant::now());
        }

        fn wait_for(&mut self, what: &str, secs: u64, mut done: impl FnMut(&mut Self) -> bool) {
            let deadline = Instant::now().checked_add(Duration::from_secs(secs)).unwrap();
            loop {
                while let Ok(msg) = self.control.try_recv() {
                    let ClientMsg::Screen(ScreenRequest::SoundReport(report)) = msg else {
                        panic!("{msg:?}");
                    };
                    self.reports.push(report);
                }
                if done(self) {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for {what}: {:?}",
                    self.sound.stats()
                );
                self.rt.block_on(async { tokio::time::sleep(Duration::from_millis(5)).await });
            }
        }

        /// Wait until the counters have every datagram routed so far.
        fn settle(&mut self) -> SoundStats {
            self.wait_for("the sound to catch up", 3, |h| h.sound.stats().datagrams == h.routed);
            self.sound.stats()
        }
    }

    /// Seconds to wait on `CoreAudio` opening a player: on a machine busy with a parallel test
    /// run it takes tens of seconds (`docs/decisions/testing.md`). A bound only so a stuck
    /// framework fails with the stats rather than at the harness's kill.
    const FOR_THE_MACHINE: u64 = 100;

    /// Muted: the first packet starts opening the player off the task, which keeps counting and
    /// reporting meanwhile; once it is open a gap is counted and concealed, a late duplicate is
    /// not played, and a gap the next datagram carries again is decoded from its copies. The
    /// worker hears of every packet that arrived. The client's only test that opens `CoreAudio`
    /// (the `coreaudio` test group), so nothing else waits on the machine's audio stack.
    #[test]
    fn audio_waits_for_the_player_and_counts_gaps() {
        let mut h = Harness::start();
        h.sound.muted().set(true);
        let mut enc = OpusEncoder::new().unwrap();
        let tone: Vec<f32> = (0..u16::try_from(FRAME_SAMPLES * CHANNELS).unwrap())
            .map(|i| (f32::from(i) * 0.05).sin() * 0.5)
            .collect();
        let mut packets = Vec::new();
        for _ in 0..3 {
            enc.push(&tone, |p| packets.push(p.to_vec())).unwrap();
        }
        assert_eq!(packets.len(), 3);
        let mut seq = 1;
        h.route(audio_datagram(SOUND, seq, 0, &packets[0], &[]).unwrap());
        h.wait_for("the first report while the player opens", 3, |h| !h.reports.is_empty());
        assert_eq!(h.reports[0], SoundReport { received: 1, lost: 0 });
        let opus = packets.clone();
        h.wait_for("the player", FOR_THE_MACHINE, |h| {
            seq += 1;
            h.route(audio_datagram(SOUND, seq, 0, &opus[seq as usize % 3], &[]).unwrap());
            h.sound.stats().packets >= 1
        });
        let before = h.settle();
        assert!(before.lost >= 1, "the packets that landed while opening: {before:?}");
        // A gap of one, then a late duplicate.
        seq += 2;
        h.route(audio_datagram(SOUND, seq, 0, &packets[0], &[]).unwrap());
        let after = h.settle();
        assert_eq!(after.packets, before.packets + 1);
        assert_eq!(after.lost, before.lost + 1, "one missing");
        assert_eq!(after.concealed, before.concealed + 1, "and concealed");
        h.route(audio_datagram(SOUND, seq - 1, 0, &packets[1], &[]).unwrap());
        let late = h.settle();
        assert_eq!(late.lost, after.lost + 1, "too late to play");
        assert_eq!(late.packets, after.packets, "not played");
        // A gap of two that the datagram after it carries again: decoded, not concealed.
        seq += 3;
        h.route(audio_datagram(SOUND, seq, 0, &packets[2], &[&packets[1], &packets[0]]).unwrap());
        let recovered = h.settle();
        assert_eq!(recovered.lost, late.lost + 2, "missing from the sequence");
        assert_eq!(recovered.recovered, late.recovered + 2, "both from the copies");
        assert_eq!(recovered.concealed, late.concealed, "nothing concealed");
        // The worker heard of every packet once, and of the gaps in the sequence.
        h.wait_for("the last report", 3, |h| {
            h.reports.iter().map(|r| u32::from(r.received)).sum::<u32>()
                == u32::try_from(h.routed).unwrap() - 1
        });
        let lost: u32 = h.reports.iter().map(|r| u32::from(r.lost)).sum();
        assert_eq!(lost, 1 + 2, "the gap of one and the gap of two");
    }
}
