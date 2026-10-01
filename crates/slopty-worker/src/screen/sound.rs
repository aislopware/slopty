//! A client's one sound from this worker (`docs/decisions/audio.md`, "One sound per worker on a
//! client, not one per stream").
//!
//! ScreenCaptureKit filters sound by application, not by window, so two tiles of one app, or a
//! display and any window, carry the same sound. A connection therefore holds one [`Sound`] for
//! its client. Every stream it opens [`Listen::listen`]s for what its target sounds like, and
//! the sound captures the union of those ([`Heard::with`]) once, with one Opus encoder, on the
//! [`SOUND`] lane. When a stream closes the capture hears what the others still want, changed
//! in place with no gap; it stops with the last.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use parking_lot::Mutex;
use slopty_capture::{
    AUDIO_CHANNELS, AUDIO_RATE, CaptureError, CaptureSource as _, CapturedAudio, Heard,
};
use slopty_codec::AudioEncoder as _;
use slopty_media::{AudioCopies, MAX_AUDIO_COPIES, audio_datagram};
use slopty_proto::media::SOUND;
use slopty_proto::screen::SoundReport;
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinHandle;

use super::{DatagramSink, Source, now, send_ms_lo};
use crate::platform::Platform;

/// Audio gate: after this long without a sample above [`AUDIO_FLOOR`] the sound stops sending
/// until something is heard again.
const AUDIO_HOLD_US: u64 = 300_000;
/// What counts as heard: below this a sample is silence.
const AUDIO_FLOOR: f32 = 1e-4;
/// How long a capture that would not start waits before it is tried again, unless the streams
/// change what they hear first.
const RETRY: Duration = Duration::from_secs(2);

/// When the sound last went out, and how many packets so far: what the client's streams read
/// of it. A stream's video lane lets audio go ahead while it flows.
#[derive(Debug, Default)]
pub struct SoundClock {
    last_us: AtomicU64,
    packets: AtomicU64,
}

impl SoundClock {
    /// When the last packet went out, on the capture clock; zero before the first.
    #[must_use]
    pub fn last_us(&self) -> u64 {
        self.last_us.load(Ordering::Relaxed)
    }

    /// Packets sent so far.
    #[must_use]
    pub fn packets(&self) -> u64 {
        self.packets.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(super) fn sent_at(&self, now_us: u64) {
        self.last_us.store(now_us, Ordering::Relaxed);
    }
}

/// What a stream asks of its client's sound.
pub trait Listen: Send + Sync {
    /// Hear `heard` while the answer is held.
    fn listen(&self, heard: Heard) -> Listening;
    /// What the client's player heard since its last report: the copies the packets carry
    /// follow its loss.
    fn report(&self, report: SoundReport);
    /// When the sound goes out.
    fn clock(&self) -> Arc<SoundClock>;
}

/// A stream's place among those the sound is for; dropped with the stream, it hears no more.
#[derive(Debug)]
pub struct Listening {
    members: Arc<Members>,
    id: u64,
}

impl Drop for Listening {
    fn drop(&mut self) {
        self.members.heard.lock().remove(&self.id);
        self.members.changed.notify_one();
    }
}

/// The streams the sound is for, and what each hears.
#[derive(Debug, Default)]
struct Members {
    heard: Mutex<BTreeMap<u64, Heard>>,
    next: AtomicU64,
    changed: Notify,
    /// The sound is gone: the capture stops whoever still listens.
    closed: AtomicBool,
    /// The capture stopped by itself.
    died: AtomicBool,
}

impl Members {
    /// What every stream hears together; `None` with none, or once the sound is gone.
    fn wanted(&self) -> Option<Heard> {
        if self.closed.load(Ordering::Acquire) {
            return None;
        }
        let heard = self.heard.lock();
        let mut all = heard.values();
        let first = all.next()?.clone();
        let wanted = all.fold(first, Heard::with);
        drop(heard);
        Some(wanted)
    }
}

/// What the sound has done, for a test or the worker's report.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SoundStats {
    /// Captures started.
    pub starts: u64,
    /// Changes of what a running capture hears.
    pub hears: u64,
    /// Captures stopped.
    pub stops: u64,
    /// Starts, changes and stops that failed, and captures that stopped by themselves.
    pub failures: u64,
    /// Packets sent.
    pub packets: u64,
    /// Times a capture's audio skipped ahead of where its last chunk ended, by more than half
    /// a chunk: a hole in the sound's own timeline, whatever the scheduler did to its delivery.
    pub holes: u64,
    /// What the running capture hears; `None` while none runs.
    pub heard: Option<Heard>,
}

/// The sound's encoder and its way out: fed from the capture's thread.
struct Voice<P: Platform> {
    sink: Arc<dyn DatagramSink>,
    audio: Mutex<AudioState<P::Audio>>,
    /// How many earlier Opus packets each datagram carries, from the loss the client reports
    /// ([`AudioCopies`]), and the packets sent last, nearest first.
    copies: Mutex<AudioCopies>,
    sent: Mutex<[Bytes; MAX_AUDIO_COPIES]>,
    clock: Arc<SoundClock>,
    /// [`SoundStats::holes`].
    holes: AtomicU64,
}

struct AudioState<A> {
    encoder: Option<A>,
    seq: u32,
    last_loud_us: u64,
    /// Where the running capture's last chunk ended on its own clock; `None` before its first.
    next_pts_us: Option<u64>,
}

impl<P: Platform> Voice<P> {
    fn new(sink: Arc<dyn DatagramSink>) -> Self {
        Self {
            sink,
            audio: Mutex::new(AudioState {
                encoder: None,
                seq: 0,
                last_loud_us: 0,
                next_pts_us: None,
            }),
            copies: Mutex::new(AudioCopies::default()),
            sent: Mutex::new(Default::default()),
            clock: Arc::default(),
            holes: AtomicU64::new(0),
        }
    }

    /// A capture starts: its timeline is its own.
    fn starts(&self) {
        self.audio.lock().next_pts_us = None;
    }

    /// Follow the capture's timeline with `chunk`, counting a hole when it starts later than
    /// the last one ended by more than half its own length.
    fn timeline(&self, chunk: &CapturedAudio) {
        let samples = u64::try_from(chunk.samples.len()).unwrap_or(u64::MAX);
        let frames = samples.checked_div(u64::from(AUDIO_CHANNELS)).unwrap_or(0);
        let length_us =
            frames.saturating_mul(1_000_000).checked_div(u64::from(AUDIO_RATE)).unwrap_or(0);
        let expected =
            self.audio.lock().next_pts_us.replace(chunk.pts_us.saturating_add(length_us));
        if expected.is_some_and(|at| chunk.pts_us > at.saturating_add(length_us / 2)) {
            self.holes.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The capture delivered PCM: encode and send unless the sound has gone quiet. Audio goes
    /// to QUIC at once, ahead of any video the streams hold in their lanes.
    fn on_audio(&self, chunk: &CapturedAudio) {
        self.timeline(chunk);
        let now = now::<P>();
        let Some(packets) = self.encode(&chunk.samples, now) else { return };
        let datagrams = self.datagrams(&packets, now);
        if datagrams.is_empty() {
            return;
        }
        self.clock.last_us.store(now, Ordering::Relaxed);
        if self.sink.send(&datagrams).is_ok() {
            let n = u64::try_from(datagrams.len()).unwrap_or(u64::MAX);
            self.clock.packets.fetch_add(n, Ordering::Relaxed);
        }
    }

    /// Opus `packets` as datagrams on the [`SOUND`] lane, each carrying again the packets
    /// before it the client's reported loss asks for.
    fn datagrams(&self, packets: &[(u32, Bytes)], now: u64) -> Vec<Bytes> {
        let copies = self.copies.lock().copies();
        let mut sent = self.sent.lock();
        let mut datagrams = Vec::with_capacity(packets.len());
        for (seq, packet) in packets {
            let [near, far] = &*sent;
            let earlier: [&[u8]; MAX_AUDIO_COPIES] = [near, far];
            let earlier = earlier.get(..copies).unwrap_or_default();
            datagrams.extend(audio_datagram(SOUND, *seq, send_ms_lo(now), packet, earlier));
            sent.rotate_right(1);
            if let Some(nearest) = sent.first_mut() {
                nearest.clone_from(packet);
            }
        }
        datagrams
    }

    /// Run the silence gate and the encoder under the audio lock; `None` when nothing goes out.
    fn encode(&self, samples: &[f32], now: u64) -> Option<Vec<(u32, Bytes)>> {
        let loud = samples.iter().any(|s| s.abs() > AUDIO_FLOOR);
        let mut audio = self.audio.lock();
        if loud {
            audio.last_loud_us = now;
        } else if now.saturating_sub(audio.last_loud_us) > AUDIO_HOLD_US {
            return None;
        }
        if audio.encoder.is_none() {
            match P::Audio::new() {
                Ok(encoder) => audio.encoder = Some(encoder),
                Err(e) => {
                    tracing::warn!(error = %e, "no Opus encoder; no sound");
                    // Never retried: keep the gate closed for good.
                    audio.last_loud_us = 0;
                    return None;
                }
            }
        }
        let AudioState { encoder: Some(encoder), seq, .. } = &mut *audio else { return None };
        let mut packets = Vec::new();
        let encoded = encoder.push(samples, |packet| {
            *seq = seq.wrapping_add(1);
            packets.push((*seq, Bytes::copy_from_slice(packet)));
        });
        drop(audio);
        match encoded {
            Ok(()) => Some(packets),
            Err(e) => {
                tracing::debug!(error = %e, "opus encode");
                None
            }
        }
    }
}

/// A client's one sound from this worker, on platform `P`. Dropped with the connection: the
/// capture stops.
pub struct Sound<P: Platform> {
    members: Arc<Members>,
    voice: Arc<Voice<P>>,
    stats: Arc<Mutex<SoundStats>>,
}

impl<P: Platform> std::fmt::Debug for Sound<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sound").field("stats", &*self.stats.lock()).finish_non_exhaustive()
    }
}

impl<P: Platform> Sound<P> {
    /// A sound for the client behind `sink`, capturing nothing until a stream listens. Spawned
    /// on the current runtime.
    #[must_use]
    pub fn new(sink: Arc<dyn DatagramSink>) -> Self {
        let members = Arc::new(Members::default());
        let voice = Arc::new(Voice::new(sink));
        let stats = Arc::new(Mutex::new(SoundStats::default()));
        // Ends by itself once the sound is dropped and its capture stopped.
        let _follows: JoinHandle<()> =
            tokio::spawn(follow::<P>(Arc::clone(&members), Arc::clone(&voice), Arc::clone(&stats)));
        Self { members, voice, stats }
    }

    /// What it has done so far.
    #[must_use]
    pub fn stats(&self) -> SoundStats {
        let mut stats = self.stats.lock().clone();
        stats.packets = self.voice.clock.packets();
        stats.holes = self.voice.holes.load(Ordering::Relaxed);
        stats
    }
}

impl<P: Platform> Drop for Sound<P> {
    fn drop(&mut self) {
        // The task stops the capture and ends; aborting it would leave the capture running.
        self.members.closed.store(true, Ordering::Release);
        self.members.changed.notify_one();
    }
}

impl<P: Platform> Listen for Sound<P> {
    fn listen(&self, heard: Heard) -> Listening {
        let id = self.members.next.fetch_add(1, Ordering::Relaxed);
        self.members.heard.lock().insert(id, heard);
        self.members.changed.notify_one();
        Listening { members: Arc::clone(&self.members), id }
    }

    fn report(&self, report: SoundReport) {
        let (was, copies) = {
            let mut control = self.voice.copies.lock();
            (control.copies(), control.on_report(report, now::<P>()))
        };
        if copies != was {
            tracing::debug!(was, copies, lost = report.lost, "audio copies");
        }
    }

    fn clock(&self) -> Arc<SoundClock> {
        Arc::clone(&self.voice.clock)
    }
}

/// Change the sound's counters under their lock, and nothing else.
fn note(stats: &Mutex<SoundStats>, change: impl FnOnce(&mut SoundStats)) {
    change(&mut stats.lock());
}

/// A sound that never captures.
///
/// The streams listen and leave as through a [`Sound`], and nothing goes out. A worker serving
/// the drawn screen has it unless a test asks for its tone (`synthetic::SOUND_SWITCH`), so no
/// client of it opens a player.
#[derive(Debug, Default)]
pub struct Silent {
    members: Arc<Members>,
    clock: Arc<SoundClock>,
}

impl Listen for Silent {
    fn listen(&self, heard: Heard) -> Listening {
        let id = self.members.next.fetch_add(1, Ordering::Relaxed);
        self.members.heard.lock().insert(id, heard);
        Listening { members: Arc::clone(&self.members), id }
    }

    fn report(&self, _report: SoundReport) {}

    fn clock(&self) -> Arc<SoundClock> {
        Arc::clone(&self.clock)
    }
}

/// Keep the capture hearing what the members want: started with the first, changed in place as
/// they come and go, stopped with the last or once the sound is gone.
async fn follow<P: Platform>(
    members: Arc<Members>,
    voice: Arc<Voice<P>>,
    stats: Arc<Mutex<SoundStats>>,
) {
    let mut running: Option<(<Source<P> as slopty_capture::CaptureSource>::Sound, Heard)> = None;
    loop {
        let changed = members.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if members.died.swap(false, Ordering::AcqRel) {
            running = None;
            stats.lock().heard = None;
        }
        let wanted = members.wanted();
        let mut settled = true;
        match (running.take(), wanted) {
            (None, None) => {}
            (Some((sound, _)), None) => {
                let (tx, rx) = oneshot::channel();
                Source::<P>::stop_sound(&sound, move |done| {
                    let _waiting = tx.send(done);
                });
                let stopped = rx.await.unwrap_or(Ok(()));
                note(&stats, |stats| {
                    stats.stops = stats.stops.saturating_add(1);
                    stats.heard = None;
                    stats.failures = stats.failures.saturating_add(u64::from(stopped.is_err()));
                });
                if let Err(e) = stopped {
                    tracing::debug!(error = %e, "the sound's capture would not stop");
                }
            }
            (Some((sound, heard)), Some(wanted)) if heard == wanted => {
                running = Some((sound, heard));
            }
            (Some((sound, heard)), Some(wanted)) => {
                let (tx, rx) = oneshot::channel();
                Source::<P>::hear(&sound, &wanted, move |done| {
                    let _waiting = tx.send(done);
                });
                match rx.await.unwrap_or(Ok(())) {
                    Ok(()) => {
                        note(&stats, |stats| {
                            stats.hears = stats.hears.saturating_add(1);
                            stats.heard = Some(wanted.clone());
                        });
                        running = Some((sound, wanted));
                    }
                    Err(e) => {
                        note(&stats, |stats| stats.failures = stats.failures.saturating_add(1));
                        tracing::warn!(error = %e, ?wanted, "the sound would not hear what the streams do");
                        running = Some((sound, heard));
                        settled = false;
                    }
                }
            }
            (None, Some(wanted)) => match start::<P>(&members, &voice, &wanted).await {
                Ok(sound) => {
                    note(&stats, |stats| {
                        stats.starts = stats.starts.saturating_add(1);
                        stats.heard = Some(wanted.clone());
                    });
                    running = Some((sound, wanted));
                }
                Err(e) => {
                    note(&stats, |stats| stats.failures = stats.failures.saturating_add(1));
                    tracing::warn!(error = %e, ?wanted, "no capture of the sound");
                    settled = false;
                }
            },
        }
        if running.is_none() && members.closed.load(Ordering::Acquire) {
            break;
        }
        if settled {
            changed.await;
        } else {
            let _retry = tokio::time::timeout(RETRY, changed).await;
        }
    }
}

/// Start a capture of `heard` feeding `voice`; one that stops by itself is started again.
async fn start<P: Platform>(
    members: &Arc<Members>,
    voice: &Arc<Voice<P>>,
    heard: &Heard,
) -> Result<<Source<P> as slopty_capture::CaptureSource>::Sound, CaptureError> {
    voice.starts();
    let fed = Arc::clone(voice);
    let sink = Box::new(move |chunk: CapturedAudio| fed.on_audio(&chunk));
    let watched = Arc::clone(members);
    let on_stop = move |e: CaptureError| {
        tracing::warn!(error = %e, "the sound's capture stopped");
        watched.died.store(true, Ordering::Release);
        watched.changed.notify_one();
    };
    let (tx, rx) = oneshot::channel();
    Source::<P>::start_sound(heard, sink, on_stop, move |started| {
        let _waiting = tx.send(started);
    });
    rx.await.unwrap_or_else(|_gone| Err(CaptureError::Stopped("never answered".to_owned())))
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use std::time::Instant;

    use slopty_codec::audio::{CHANNELS, FRAME_SAMPLES};
    use slopty_media::parse_audio;
    use slopty_proto::media::{HEADER_BYTES, Kind, MediaHeader};

    use super::*;
    use crate::screen::Refused;
    use crate::screen::synthetic::Synthetic;

    /// The client's end: every datagram the sound sent, and when.
    #[derive(Default)]
    struct Client {
        got: Mutex<Vec<(Instant, Bytes)>>,
    }

    impl DatagramSink for Client {
        fn send(&self, datagrams: &[Bytes]) -> Result<(), Refused> {
            let now = Instant::now();
            self.got.lock().extend(datagrams.iter().map(|d| (now, d.clone())));
            Ok(())
        }

        fn max_size(&self) -> Option<usize> {
            Some(slopty_proto::media::MAX_DATAGRAM)
        }

        fn held(&self) -> usize {
            0
        }

        fn cwnd(&self) -> u64 {
            0
        }

        fn is_closed(&self) -> bool {
            false
        }
    }

    impl Client {
        fn count(&self) -> usize {
            self.got.lock().len()
        }

        /// The sequence number of every datagram so far, in the order they came, each checked
        /// to be an audio packet on the sound's lane.
        fn seqs(&self) -> Vec<u32> {
            let got = self.got.lock().clone();
            got.iter()
                .map(|(_, d)| {
                    let (header, _) = MediaHeader::parse(d).expect("a media datagram");
                    assert_eq!(header.stream.get(), SOUND.0, "on the sound lane");
                    assert_eq!(header.kind(), Some(Kind::Audio));
                    let payload = d.slice(HEADER_BYTES..);
                    assert!(parse_audio(header.data_count.get(), &payload).is_some());
                    header.frame.get()
                })
                .collect()
        }
    }

    fn apps(pids: &[i32]) -> Heard {
        Heard::Apps(pids.iter().copied().collect())
    }

    /// How long a wait behind the first chunk's Opus encoder may take: `AudioToolbox` making
    /// one took about 40 s in a loaded gate (`docs/decisions/testing.md`), and the tone's queue,
    /// which a stop answers on, waits behind it. A bound only, so a stuck framework fails here.
    const FOR_THE_MACHINE: Duration = Duration::from_secs(100);
    /// How long the sound's own logic may take.
    const OWN: Duration = Duration::from_secs(5);

    /// Wait, on the stream and not on a fixed sleep, until `done` holds, for at most `within`.
    async fn until(what: &str, within: Duration, done: impl Fn() -> bool) {
        let deadline = Instant::now().checked_add(within).unwrap();
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn sound() -> (Sound<Synthetic>, Arc<Client>) {
        let client = Arc::new(Client::default());
        let sink: Arc<dyn DatagramSink> = Arc::<Client>::clone(&client);
        (Sound::<Synthetic>::new(sink), client)
    }

    /// Two window streams of one app open one capture of that app's sound and one encoder:
    /// every packet comes on the sound lane, numbered in one sequence. The second stream changes
    /// nothing, and the capture stops with the last.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_windows_of_one_app_are_one_sound() {
        let (sound, client) = sound();
        let first = sound.listen(apps(&[7]));
        until("a start", OWN, || sound.stats().heard.is_some()).await;
        let second = sound.listen(apps(&[7]));
        until("packets", FOR_THE_MACHINE, || client.count() > 20).await;
        let stats = sound.stats();
        assert_eq!((stats.starts, stats.hears, stats.heard), (1, 0, Some(apps(&[7]))));
        let seqs = client.seqs();
        let expected: Vec<u32> = (1..).take(seqs.len()).collect();
        assert_eq!(seqs, expected, "one encoder, one sequence");
        drop(first);
        drop(second);
        until("a stop", FOR_THE_MACHINE, || sound.stats().stops == 1).await;
        assert_eq!(sound.stats().heard, None);
    }

    /// A display and a window stream hear every app, once. The window going changes nothing
    /// the capture hears and leaves no gap in the sound's timeline; the display going then
    /// narrows it to the window's app in place, still with no gap and no new capture.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_display_and_a_window_are_one_sound_with_no_gap_when_one_goes() {
        let (sound, client) = sound();
        let display = sound.listen(Heard::Every);
        let window = sound.listen(apps(&[7]));
        until("packets", FOR_THE_MACHINE, || client.count() > 10).await;
        assert_eq!(sound.stats().heard, Some(Heard::Every));
        drop(window);
        let before = client.count();
        until("packets after the window went", FOR_THE_MACHINE, || client.count() > before + 20)
            .await;
        let stats = sound.stats();
        assert_eq!((stats.starts, stats.hears, stats.heard.clone()), (1, 0, Some(Heard::Every)));
        let window = sound.listen(apps(&[7]));
        drop(display);
        until("the window's app alone", OWN, || sound.stats().heard == Some(apps(&[7]))).await;
        let before = client.count();
        until("packets after the display went", FOR_THE_MACHINE, || client.count() > before + 20)
            .await;
        let stats = sound.stats();
        assert_eq!((stats.starts, stats.hears, stats.stops), (1, 1, 0), "changed in place");
        // No gap, judged on the sound's own timeline and not on when packets reached the test,
        // which is the scheduler's (129 ms on a loaded 3-core runner): one capture whose
        // chunks follow each other with no hole, and one unbroken sequence of packets.
        assert_eq!(stats.holes, 0, "the capture's audio skipped ahead: {stats:?}");
        let seqs = client.seqs();
        assert_eq!(seqs, (1..).take(seqs.len()).collect::<Vec<u32>>(), "no packet missing");
        drop(window);
    }

    /// Windows of two apps hear both; when the last window of one goes, its app drops out.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn windows_of_two_apps_hear_both_until_one_goes() {
        let (sound, _client) = sound();
        let editor = sound.listen(apps(&[7]));
        let terminal = sound.listen(apps(&[9]));
        let other_terminal = sound.listen(apps(&[9]));
        until("both", OWN, || sound.stats().heard == Some(apps(&[7, 9]))).await;
        drop(terminal);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(sound.stats().heard, Some(apps(&[7, 9])), "a window of 9 is still open");
        drop(other_terminal);
        until("7 alone", OWN, || sound.stats().heard == Some(apps(&[7]))).await;
        drop(editor);
        until("stopped", FOR_THE_MACHINE, || sound.stats().stops == 1).await;
        assert_eq!(sound.stats().starts, 1);
    }

    /// A chunk that starts later than the last one ended, by more than half its length, is a
    /// hole in the capture's timeline; one a little late is not, and a new capture starts a
    /// timeline of its own.
    #[test]
    fn a_hole_in_the_captures_timeline_is_counted() {
        let voice = Voice::<Synthetic>::new(Arc::new(Client::default()));
        let samples = usize::try_from(FRAME_SAMPLES * CHANNELS).unwrap();
        let chunk = |pts_us| CapturedAudio { pts_us, samples: vec![0.0; samples] };
        let holes = || voice.holes.load(Ordering::Relaxed);
        for pts in [1_000_000, 1_010_000, 1_020_004] {
            voice.timeline(&chunk(pts));
        }
        assert_eq!(holes(), 0, "on time, and 4 µs late");
        voice.timeline(&chunk(1_060_000));
        assert_eq!(holes(), 1, "30 ms missing");
        voice.timeline(&chunk(1_070_000));
        voice.starts();
        voice.timeline(&chunk(9_000_000));
        assert_eq!(holes(), 1, "a new capture's own timeline");
    }

    /// Audio goes out while the source is loud and for the hold after it; silence past the
    /// hold sends nothing, and each packet carries the next sequence number.
    #[test]
    fn audio_is_gated_by_silence_and_numbered() -> Result<(), String> {
        let voice = Voice::<Synthetic>::new(Arc::new(Client::default()));
        let samples = usize::try_from(FRAME_SAMPLES * CHANNELS).map_err(|e| e.to_string())?;
        let quiet = vec![0.0_f32; samples];
        let loud: Vec<f32> = (0..samples).map(|i| if i % 2 == 0 { 0.5 } else { -0.5 }).collect();
        let start = 1_000_000_u64;
        assert!(voice.encode(&quiet, start).is_none(), "silence from the start");
        let first = voice.encode(&loud, start).ok_or("loud: a packet")?;
        let second = voice.encode(&loud, start + 20_000).ok_or("loud again")?;
        let seqs: Vec<u32> = first.iter().chain(&second).map(|(seq, _)| *seq).collect();
        assert_eq!(seqs, vec![1, 2], "one packet per frame, numbered from 1");
        assert!(
            voice.encode(&quiet, start + 20_000 + AUDIO_HOLD_US).is_some(),
            "silence inside the hold still goes out"
        );
        assert!(
            voice.encode(&quiet, start + 20_001 + AUDIO_HOLD_US).is_none(),
            "and past it, nothing"
        );
        Ok(())
    }
}
