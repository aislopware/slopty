//! Remote window streaming: one pipeline per open stream.
//!
//! ```text
//! ScreenCaptureKit queue ──frame──▶ encoder.encode ──VideoToolbox thread──▶ packetize ──▶ sink
//! ```
//!
//! The pipeline ([`Pipeline`]) is generic over the worker's [`Platform`]: capture, encoders and
//! input are the platform crates' traits, called directly. [`ScreenStream`] is the pipeline on
//! the platform this build serves.
//!
//! Nothing here knows the network: datagrams go to a [`DatagramSink`] the transport (the worker)
//! implements, from whichever thread produced them, in one call per frame — except while audio
//! is flowing, when video waits in a lane of its own and is handed over a slice of the link at a
//! time so audio can go ahead of it (`Lane`). Everything the client sends back (reports, NACKs,
//! refresh requests, quality changes) lands on [`ScreenStream`] and [`StreamControl`] methods.
//! When the transport already holds more than the guard allows the capture callback drops whole
//! frames rather than letting latency build up; the client notices the gap and asks for a
//! refresh. The newest capture is kept, so a picture that went still still gets the frame the
//! cadence or the guard held back, and a refresh asked for on it is answered (`repair_loop`).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use bytes::Bytes;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use slopty_capture::host_now_us;
use slopty_capture::{
    AxError, CaptureConfig, CaptureError, CaptureSource, CapturedAudio, CapturedFrame, Crop,
    PixelFormat, Rect, TargetWindow, Went, WindowState, crop_for,
};
use slopty_codec::{
    AudioEncoder as _, CodecError, EncodedPacket, EncoderConfig, FrameOptions, VideoEncoder as _,
};
use slopty_core::StreamId;
use slopty_input::{InputError, InputSink as _, Pointer, PointerWatch};
pub use slopty_media::PathSample;
use slopty_media::{
    Cadence, Decision, EncodedFrame, HEARTBEAT_AFTER, MediaError, Packetizer, RateController,
    Redundancy, audio_datagram, cursor_datagram, frame_due, heartbeat_datagram,
};
use slopty_proto::media::MAX_DATAGRAM;
use slopty_proto::screen::{
    CaptureTarget, CursorShape, Quality, ReceiverReport, ScreenEvent, ScreenInput, SourceState,
    VideoCodec,
};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::platform::{Native, Platform};

/// A platform's capture.
type Source<P> = <P as Platform>::Capture;
/// A platform's enumeration of shareable content.
type Content<P> = <Source<P> as CaptureSource>::Content;
/// A platform's resolved capture target.
type Resolved<P> = <Source<P> as CaptureSource>::Target;
/// A frame as a platform's capture delivers it.
type Frame<P> = CapturedFrame<<Source<P> as CaptureSource>::Image>;

/// Now on the clock `P`'s frames are stamped with, microseconds.
fn now<P: Platform>() -> u64 {
    Source::<P>::now_us()
}

/// Where a stream's datagrams go: the connection's unreliable datagram channel.
///
/// Called from the thread that produced the datagrams (VideoToolbox's callback, the capture
/// queue, a timer task), never through a queue of this crate's own: a datagram handed to the
/// transport at once is one that cannot wait for a task to be scheduled.
pub trait DatagramSink: Send + Sync {
    /// Hand `datagrams` to the transport, in order, in one call. Older datagrams the transport
    /// still holds may be dropped to make room: video is unreliable by design.
    ///
    /// # Errors
    ///
    /// [`Refused`], for the whole batch.
    fn send(&self, datagrams: &[Bytes]) -> Result<(), Refused>;
    /// The largest datagram the path carries now; `None` when the peer takes none.
    fn max_size(&self) -> Option<usize>;
    /// Bytes of datagrams the transport holds, waiting for the congestion window or the pacer.
    fn held(&self) -> usize;
    /// The path's congestion window, bytes; `0` when unknown.
    fn cwnd(&self) -> u64;
    /// The connection is gone: nothing sent now arrives.
    fn is_closed(&self) -> bool;
}

/// Why the transport would not take a batch of datagrams.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// One of them is larger than the path carries: cut for a size the path no longer has.
    TooLarge,
    /// The connection is gone, or never took datagrams.
    Closed,
}

/// Cursor sample period (120 Hz); a datagram goes out only when the position changed.
const CURSOR_PERIOD: Duration = Duration::from_micros(8_333);
/// How often the cursor's picture is read while the pointer is over the target (30 Hz). It
/// goes to the client only when it changed.
const SHAPE_PERIOD: Duration = Duration::from_millis(33);

/// What a stream tells its owner outside the datagram path.
#[derive(Debug)]
pub enum StreamEvent {
    /// Capture ended (ScreenCaptureKit stopped it, or the cropped window closed).
    Stopped(CaptureError),
    /// The worker's cursor picture changed while the pointer was over the target.
    Cursor(CursorShape),
}

/// How long a window may not be served as a crop after the accessibility API says a window of
/// its application went away, if the window list has not confirmed it by then.
///
/// The accessibility signal cannot name the window, so it is a suspicion; the window list is
/// the confirmation and it lags the order-out by 256–266 ms (MEASUREMENTS.md, "which signal
/// knows first"), read on the owner's geometry tick (the worker: every 100 ms). The
/// watch matches the target to its element once, so another window of the application going
/// is not a suspicion (`ScreenStats::siblings`); only when it could not match — the
/// application does not list the window — does any window's going cost a freeze this long. A
/// true suspicion is confirmed inside the hold and never leaks a frame of the desktop.
pub const SUSPICION_HOLD: Duration = Duration::from_millis(400);

/// Screen pipeline errors.
#[derive(Debug, thiserror::Error)]
pub enum ScreenError {
    /// ScreenCaptureKit.
    #[error(transparent)]
    Capture(#[from] CaptureError),
    /// VideoToolbox.
    #[error(transparent)]
    Codec(#[from] CodecError),
    /// Packetizer.
    #[error(transparent)]
    Media(#[from] MediaError),
    /// Event injection.
    #[error(transparent)]
    Input(#[from] InputError),
    /// The accessibility API would not resize the window.
    #[error(transparent)]
    Resize(#[from] AxError),
    /// The window is gone from the window list.
    #[error("the window is gone")]
    WindowGone,
    /// A callback-based framework call never completed.
    #[error("screen pipeline closed")]
    Closed,
}

/// Frames' worth of bytes (at the current target rate) QUIC may hold before a captured frame
/// is dropped instead of encoded.
///
/// The datagram send buffer is 4 MiB; a link whose window collapsed held 275–365 KB of
/// frames for 4–7 s and delivered every one of them stale (MEASUREMENTS.md, "start-up over
/// the mesh"). Two frames keep a keyframe's tail flowing and stop the queue there: the next
/// capture is fresher than anything that would wait behind it.
const HELD_FRAMES: u64 = 2;

/// Whether a captured frame should be encoded given what the transport holds ahead of it.
///
/// `held` bytes in QUIC's send buffer against a congestion window of `cwnd`, at `target_bps`
/// and `fps`.
///
/// Two frames' worth is a *time* budget written in bytes, so it has to be recomputed from the
/// rate actually in force: at 30 Mbit/s and 60 fps it is 125 KB, at the 1 Mbit/s floor it is
/// 4 KB. A fixed byte floor under it turns into a fixed queue of *seconds* as the path slows —
/// 32 KB is 33 ms at 8 Mbit/s and 260 ms at 1 Mbit/s, charged to every frame behind it
/// (MEASUREMENTS.md and `docs/decisions/transport.md`, "the two controllers fail in opposite
/// ways": holds peaking at the floor plus one frame with the guard dropping throughout).
///
/// One congestion window is the floor instead. Bytes inside the window are not a standing
/// queue — QUIC sends them on the next acknowledgement — so refusing a frame below `cwnd`
/// drops one the link would have carried. That case is real where the round trip is long:
/// `per_frame × 2` falls under one window once `rtt × fps` passes 1.8.
#[must_use]
pub const fn frame_fits(held: usize, cwnd: u64, target_bps: u64, fps: u16) -> bool {
    let fps = if fps == 0 { 1 } else { fps as u64 };
    let per_frame = match (target_bps / 8).checked_div(fps) {
        Some(bytes) => bytes,
        None => 0,
    };
    let frames = per_frame.saturating_mul(HELD_FRAMES);
    let limit = if frames < cwnd { cwnd } else { frames };
    (held as u64) <= limit
}

/// How long the link may take to drain a keyframe before one is deferred instead of encoded.
///
/// The encoder sizes a keyframe from the picture and the average rate, never from what the path
/// can carry right now, so a collapsed link is handed one that does not fit. The shaped ladder's
/// keyframes ran 87–134 kB (MEASUREMENTS.md, 2026-09-15), and 134 kB at the 1 Mbit/s floor is
/// 1.07 s of link in a single frame. [`frame_fits`] cannot reach it: that gate runs before the
/// encode, so it bounds what is queued *ahead* of a frame and never the frame itself.
const KEYFRAME_DRAIN_MS: u64 = 400;

/// How long a keyframe may be deferred before it goes out whatever it costs.
///
/// The safety valve. Without one a link that never recovers never gets a keyframe and the client
/// sits on a hole for good, which is worse than the stall the deferral prevents. A second bounds
/// the wait against the 4 796 ms hold that motivated the rule, and a picture goes out meanwhile:
/// deferral only happens when an LTR refresh is available to send instead.
const KEYFRAME_VALVE_US: u64 = 1_000_000;

/// Whether a keyframe of `estimate` bytes should be encoded now.
///
/// `held` bytes are in QUIC's send buffer already and the link is running at `target_bps`. True
/// while the two together drain inside 400 ms, and true whenever there is no estimate yet: the
/// first keyframe of a stream is the one the client has no picture without.
#[must_use]
pub const fn keyframe_fits(estimate: u64, held: usize, target_bps: u64) -> bool {
    if estimate == 0 {
        return true;
    }
    let drainable = (target_bps / 8).saturating_mul(KEYFRAME_DRAIN_MS) / 1000;
    estimate.saturating_add(held as u64) <= drainable
}

/// Fold an encoded keyframe's size into the running estimate: up at once, down by an eighth.
///
/// Rising immediately is what makes the rule safe — one cheap keyframe (a blank screen, a
/// scrolled-off window) must not license the next expensive one — and decaying at all is what
/// lets a stream whose picture genuinely got simpler stop deferring.
#[must_use]
pub const fn keyframe_estimate(previous: u64, observed: u64) -> u64 {
    if observed >= previous {
        return observed;
    }
    previous.saturating_sub(previous / 8).saturating_add(observed / 8)
}

/// The share of a `target_bps` link budget the encoder gets when the packetizer adds
/// `parity_permille` of Reed–Solomon parity on top of every frame.
///
/// The rate controller's target is what the path carries, and parity rides the same path, so
/// a target handed to the encoder whole put `1 + parity` of it on the wire: 20 % over at the
/// default ratio, 50 % at the ceiling. The encoder gets `target × 1000 / (1000 + parity)` and
/// the frame plus its parity fits the target. Proportional and nothing else; whether the
/// picture is better served by less parity at a higher encoder rate is an A/B still to run.
#[must_use]
pub fn encoder_bps(target_bps: u32, parity_permille: u16) -> u32 {
    let whole = 1000_u64.saturating_add(u64::from(parity_permille));
    let share = u64::from(target_bps).saturating_mul(1000).checked_div(whole).unwrap_or(0);
    u32::try_from(share).unwrap_or(u32::MAX)
}

/// How long after a capture the next one would have arrived had the picture kept changing;
/// past it a capture that was not encoded is the last one there will be.
///
/// ScreenCaptureKit delivers only frames whose content changed, so a picture that goes still
/// leaves nothing behind its last frame to carry what that frame was skipped for. Half again
/// the capture period absorbs the display beat's jitter, and never less than a 60 Hz display's:
/// a ceiling above the panel's rate would otherwise call the gap between two ordinary frames a
/// stop and send the older one.
const fn quiet_after_us(ceiling_fps: u16) -> u64 {
    let fps = if ceiling_fps < 60 { ceiling_fps } else { 60 };
    match 1_500_000_u64.checked_div(fps as u64) {
        Some(us) => us,
        None => 1_500_000,
    }
}

/// What the held capture could answer: it never reached the encoder, a refresh is pending, a
/// keyframe is pending.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Asks {
    owed: bool,
    refresh: bool,
    keyframe: bool,
}

/// When the held capture should be encoded, if nothing newer arrives first; `None` when
/// nothing is owed.
///
/// `asks` says what the held capture could answer. The capture at `captured_us` goes out once the
/// picture has gone quiet ([`quiet_after_us`]) and, unless a keyframe is wanted, once the
/// cadence at `fps` is due again after the frame encoded at `last_us`: the same gate a fresh
/// capture passes, so a repair never spends more than the rung allows.
const fn repair_at(
    asks: Asks,
    captured_us: u64,
    last_us: u64,
    fps: u16,
    ceiling_fps: u16,
) -> Option<u64> {
    if !asks.owed && !asks.refresh && !asks.keyframe {
        return None;
    }
    let quiet = captured_us.saturating_add(quiet_after_us(ceiling_fps));
    if asks.keyframe {
        return Some(quiet);
    }
    let period = period_us(fps);
    let due = last_us.saturating_add(period).saturating_sub(period / 8);
    Some(if due > quiet { due } else { quiet })
}

/// One frame's period at `fps`, microseconds; a second at 0.
const fn period_us(fps: u16) -> u64 {
    match 1_000_000_u64.checked_div(fps as u64) {
        Some(us) => us,
        None => 1_000_000,
    }
}

/// A long-term reference the client can be refreshed from: the tokens this encoder session
/// offered and which of them the client acknowledged.
///
/// An IDR empties the decoder's reference list, so a token acknowledged before the latest
/// keyframe names a picture neither end can predict from any more, and a refresh asked for then
/// is answered with another IDR. That is what `ltr_acked` could not say: it latched on the
/// first acknowledgement of the stream and stayed true through every keyframe after it
/// (MEASUREMENTS.md, 2026-09-15, "the LTR reference is starved"). Here a token is usable only
/// when it was offered after the latest keyframe of the current session and acknowledged.
#[derive(Debug, Default)]
struct LtrBook {
    /// Tokens offered this session, oldest first: the token, the keyframe epoch it was offered
    /// in, and when.
    offered: VecDeque<(u64, u64, u64)>,
    /// Keyframes (and rebuilds) seen: each one starts a new epoch.
    epoch: u64,
    /// The newest acknowledged token of the current epoch and when it was offered.
    usable: Option<(u64, u64)>,
}

/// Offered tokens remembered; VideoToolbox offers about one per fifteen frames, so this is
/// seconds of them.
const LTR_OFFERED_KEEP: usize = 64;

impl LtrBook {
    /// An encoded frame: a keyframe starts a new epoch, and a token it carries is offered in it.
    /// True when the frame offered a token.
    fn on_packet(&mut self, keyframe: bool, token: Option<u64>, now: u64) -> bool {
        if keyframe {
            self.epoch = self.epoch.wrapping_add(1);
            self.usable = None;
        }
        let Some(token) = token else { return false };
        if self.offered.len() >= LTR_OFFERED_KEEP {
            self.offered.pop_front();
        }
        self.offered.push_back((token, self.epoch, now));
        true
    }

    /// The client acknowledged `token`. `true` when this session offered it, so the encoder may
    /// be told; it becomes the usable reference when it belongs to the current epoch and is
    /// newer than the one on record.
    fn on_ack(&mut self, token: u64) -> bool {
        let Some(&(_token, epoch, at)) = self.offered.iter().rev().find(|(t, ..)| *t == token)
        else {
            return false;
        };
        if epoch == self.epoch && self.usable.is_none_or(|(_t, newest)| at >= newest) {
            self.usable = Some((token, at));
        }
        true
    }

    /// A new encoder session: nothing it will offer can be predicted from the old one's
    /// references, and an acknowledgement still in flight for the old one names nothing.
    fn reset(&mut self) {
        self.offered.clear();
        self.epoch = self.epoch.wrapping_add(1);
        self.usable = None;
    }

    /// The client's decoder lost every reference it held: nothing acknowledged so far can be
    /// predicted from, nor anything acknowledged late for this epoch, until a keyframe starts the
    /// next one.
    const fn client_lost(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.usable = None;
    }

    /// How long ago the usable reference was encoded, microseconds; `None` when there is none.
    fn usable_age(&self, now: u64) -> Option<u64> {
        self.usable.map(|(_token, at)| now.saturating_sub(at))
    }
}

/// What the long-term reference machinery did on a stream, for the control socket.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct LtrStats {
    /// Frames the encoder marked as long-term references (tokens offered).
    pub offered: u64,
    /// Tokens the client acknowledged that this encoder session had offered.
    pub acked: u64,
    /// Refreshes the encoder answered with an IDR: no usable reference behind them.
    pub refreshes_idr: u64,
    /// Refreshes the encoder answered with a delta off an acknowledged reference.
    pub refreshes_delta: u64,
    /// Whether an acknowledged reference newer than the latest keyframe is on record now, so a
    /// refresh would be a delta.
    pub usable: bool,
    /// Age of that reference, microseconds; 0 when there is none.
    pub usable_age_us: u64,
}

/// How much of the link, in time, video may hold in QUIC ahead of an audio packet.
///
/// QUIC's datagram queue is first in, first out, so audio sent behind a frame waits for the
/// whole frame: a 130 kB keyframe is 52 ms of a 20 Mbit/s link, longer than the player's jitter
/// slack. While audio is flowing, video is handed to QUIC a slice at a time and audio, sent
/// straight in, waits behind at most this much. The slice has to outlast the lane's tick with
/// room to spare, or the link idles between two top-ups and the frame arrives later than QUIC
/// alone would have delivered it.
const LANE_SLICE_US: u64 = 5_000;
/// How often the lane tops QUIC up while video waits in it.
const LANE_TICK: Duration = Duration::from_millis(1);
/// The shortest span a drain rate is measured over.
const LANE_SAMPLE_US: u64 = 1_000;
/// After this long without an audio packet video goes straight to QUIC again: with nothing to
/// let ahead, the lane would only cost the frame its ticks.
const LANE_AUDIO_HOLD_US: u64 = 200_000;

/// Video datagrams waiting to be handed to QUIC, so audio can go ahead of them.
///
/// The budget is a slice of the rate QUIC has been seen to drain at, measured from its held
/// bytes between two looks rather than taken from the rate controller: a LAN drains many times
/// the target, and a budget sized from the target would pace a fast link down to it.
#[derive(Debug, Default)]
struct Lane {
    queue: VecDeque<Bytes>,
    /// Bytes in `queue`.
    bytes: usize,
    /// What QUIC would hold now had nothing drained since the last look.
    expected: usize,
    /// Bytes QUIC drained since `mark_us`.
    drained: usize,
    mark_us: u64,
    /// QUIC was seen empty since `mark_us`, so what drained is a floor and not the link's rate.
    dry: bool,
    /// Bytes a second QUIC has been seen to drain.
    rate: u64,
}

impl Lane {
    /// QUIC holds `held` bytes at `now`: fold what drained since the last look into the rate.
    /// The rate rises to a sample at once; a sample taken while QUIC never ran dry is the link's
    /// own rate and the estimate falls an eighth of the way to it, and one taken across a dry
    /// spell only says the link is at least that fast.
    fn observe(&mut self, held: usize, now: u64) {
        self.drained = self.drained.saturating_add(self.expected.saturating_sub(held));
        self.expected = held;
        let span = now.saturating_sub(self.mark_us);
        if span < LANE_SAMPLE_US {
            self.dry |= held == 0;
            return;
        }
        let drained = u64::try_from(self.drained).unwrap_or(u64::MAX);
        let sample = drained.saturating_mul(1_000_000).checked_div(span).unwrap_or(0);
        self.rate = if sample >= self.rate || self.dry || held == 0 {
            self.rate.max(sample)
        } else {
            self.rate.saturating_sub(self.rate / 8).saturating_add(sample / 8)
        };
        self.drained = 0;
        self.mark_us = now;
        self.dry = held == 0;
    }

    /// Bytes QUIC may hold before video waits here: a slice of the drain rate, and never less
    /// than a slice of `floor_rate` (bytes a second), what the rate controller already knows
    /// the path carries.
    fn budget(&self, floor_rate: u64) -> usize {
        let rate = self.rate.max(floor_rate);
        usize::try_from(rate.saturating_mul(LANE_SLICE_US) / 1_000_000).unwrap_or(usize::MAX)
    }

    /// Queue a frame's datagrams behind whatever is waiting.
    fn push(&mut self, datagrams: &[Bytes]) {
        self.bytes = datagrams.iter().fold(self.bytes, |sum, d| sum.saturating_add(d.len()));
        self.queue.extend(datagrams.iter().cloned());
    }

    /// Take the datagrams that fit under `budget` with `held` already in QUIC. One always goes
    /// when QUIC is empty, whatever the budget, so the lane cannot stall. What QUIC then takes
    /// of them is added to `expected` by the caller: a refused datagram never drains.
    fn take(&mut self, held: usize, budget: usize) -> Vec<Bytes> {
        let mut room = budget.saturating_sub(held);
        let mut out = Vec::new();
        while let Some(front) = self.queue.front() {
            let first_into_empty = out.is_empty() && held == 0;
            if front.len() > room && !first_into_empty {
                break;
            }
            room = room.saturating_sub(front.len());
            self.bytes = self.bytes.saturating_sub(front.len());
            out.extend(self.queue.pop_front());
        }
        out
    }
}

/// What the transport took of a batch.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Taken {
    datagrams: u64,
    bytes: usize,
}

impl Taken {
    fn of(datagrams: &[Bytes]) -> Self {
        Self {
            datagrams: u64::try_from(datagrams.len()).unwrap_or(u64::MAX),
            bytes: datagrams.iter().map(Bytes::len).sum(),
        }
    }
}

/// p50 / p95 / max of a latency over the last `LATENCY_WINDOW` samples, microseconds.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Quantiles {
    /// Samples in the window.
    pub n: u32,
    /// Median.
    pub p50_us: u64,
    /// 95th percentile.
    pub p95_us: u64,
    /// Worst in the window.
    pub max_us: u64,
}

impl Quantiles {
    /// Quantiles of `samples` (any order).
    #[must_use]
    pub fn of(samples: &[u64]) -> Self {
        Self::of_owned(samples.to_vec())
    }

    /// Quantiles of `samples` (any order), sorted in place.
    #[must_use]
    pub fn of_owned(mut sorted: Vec<u64>) -> Self {
        sorted.sort_unstable();
        let last = sorted.len().saturating_sub(1);
        let at = |q: f64| -> u64 {
            #[expect(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "an index below 2^53"
            )]
            let i = (last as f64 * q).round() as usize;
            sorted.get(i.min(last)).copied().unwrap_or(0)
        };
        Self {
            n: u32::try_from(sorted.len()).unwrap_or(u32::MAX),
            p50_us: at(0.5),
            p95_us: at(0.95),
            max_us: sorted.last().copied().unwrap_or(0),
        }
    }

    /// `p50 / p95 / max ms (n)`.
    #[must_use]
    pub fn describe(&self) -> String {
        #[expect(clippy::cast_precision_loss, reason = "microseconds well below 2^53")]
        let ms = |us: u64| us as f64 / 1e3;
        format!(
            "{:.2} / {:.2} / {:.2} ms (n={})",
            ms(self.p50_us),
            ms(self.p95_us),
            ms(self.max_us),
            self.n
        )
    }
}

/// Samples the latency quantiles are computed over: 10 s at 60 fps.
const LATENCY_WINDOW: usize = 600;

/// A ring of the last `window` latency samples, microseconds, and their quantiles: the one
/// latency window the worker keeps, whatever it times.
#[derive(Debug)]
pub struct LatencyRing {
    samples: VecDeque<u64>,
    window: usize,
}

impl Default for LatencyRing {
    fn default() -> Self {
        Self::new(LATENCY_WINDOW)
    }
}

impl LatencyRing {
    /// An empty ring keeping the last `window` samples.
    #[must_use]
    pub fn new(window: usize) -> Self {
        Self { samples: VecDeque::with_capacity(window), window: window.max(1) }
    }

    /// Record one sample, forgetting the oldest past the window.
    pub fn push(&mut self, us: u64) {
        if self.samples.len() >= self.window {
            self.samples.pop_front();
        }
        self.samples.push_back(us);
    }

    /// Quantiles of the samples in the window.
    #[must_use]
    pub fn quantiles(&self) -> Quantiles {
        Quantiles::of_owned(self.samples.iter().copied().collect())
    }
}

/// Counters for logs and telemetry.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct ScreenStats {
    /// Frames ScreenCaptureKit delivered.
    pub captured: u64,
    /// Frames dropped because the transport already held more than the guard allows
    /// ([`frame_fits`]).
    pub dropped: u64,
    /// Frames captured and thrown away because the target was not on screen: the picture was of
    /// whatever is behind it. Zero on a stream whose target never left the screen.
    pub withheld: u64,
    /// Frames held because the accessibility API had just said a window of the target's
    /// application went away and the window list had not yet answered (see
    /// [`SUSPICION_HOLD`]). Counted apart from [`Self::withheld`] so a hide shows where it was
    /// caught: here in the first ~260 ms, there once core graphics agrees.
    pub suspected: u64,
    /// Accessibility notifications that raised a suspicion: the target hidden, minimised or
    /// destroyed — or, when the watch could not match the target to its accessibility element,
    /// any window of its application.
    pub suspicions: u64,
    /// Accessibility notifications for a window of the target's application that was not the
    /// target (a sibling window, a pop-up, a tooltip) going away. Never a hold: each one is a
    /// round trip through the window filter, because the capture framework stalls on it
    /// (`Shared::filter_stalled`).
    pub siblings: u64,
    /// Encoded frames packetized.
    pub encoded: u64,
    /// Datagrams the transport took (data, parity, retransmits, audio, cursor, heartbeats).
    pub datagrams: u64,
    /// Datagrams the transport refused: the connection was gone, or a datagram was larger
    /// than the path carries.
    pub queue_full: u64,
    /// Heartbeats sent while the source was quiet.
    pub heartbeats: u64,
    /// Refresh requests the client sent for this stream (what the receiver's cap bounds).
    pub refreshes: u64,
    /// Times a wanted keyframe was put off because the link could not drain one, counted once
    /// per episode rather than per frame (see [`keyframe_fits`]). Each one sent an LTR refresh in
    /// its place and ended either when the link could carry the keyframe or when the one-second
    /// valve opened.
    pub keyframes_deferred: u64,
    /// Worst capture-to-packet latency seen, microseconds.
    pub latency_max_us: u64,
    /// Sum of capture-to-packet latencies, microseconds (divide by `encoded`).
    pub latency_sum_us: u64,
    /// Opus packets sent.
    pub audio_packets: u64,
    /// Bitrate the controller last asked the encoder for.
    pub bitrate_bps: u64,
    /// Capture latency: the window server's display time of a frame → ScreenCaptureKit's
    /// callback (what SCK adds), over the last `LATENCY_WINDOW` frames.
    pub capture: Quantiles,
    /// Encode latency: `VTCompressionSessionEncodeFrame` → the output callback.
    pub encode: Quantiles,
    /// Time between two heartbeats, over the last `LATENCY_WINDOW` of them. The beat is what
    /// tells the receiver the worker is alive while nothing is being drawn, and the receiver calls
    /// a silence of `STALL_GAP` a stall, so this is the number that says whether the worker is
    /// keeping its own promise.
    pub beat_gap: Quantiles,
    /// How long the geometry probe took (its window-server reads, off the runtime), over the
    /// last `LATENCY_WINDOW` of them: the work the beat used to wait behind.
    pub bounds: Quantiles,
    /// The longest gap between two beats since the stream opened, microseconds. The quantiles
    /// above are over a sliding window of `LATENCY_WINDOW` beats — about twenty seconds — so
    /// a single late beat early in a long stream would be gone from them by the end. This is
    /// the one that cannot forget, and it is what a rule about the beat has to be written on.
    pub beat_gap_worst_us: u64,
    /// Frames sent from the display-crop path (a window served as a `sourceRect` of its display
    /// rather than through the window filter). Frames the crop delivered after it stopped holding
    /// the target are not among them — those are [`Self::withheld`].
    pub cropped: u64,
    /// Long-term references: offered, acknowledged, whether one is usable now, and how the
    /// refreshes were answered.
    ///
    /// What it costs to ask for a refresh. With a usable reference the encoder answers
    /// `force_ltr_refresh` with a delta off it — 733 B against a 3 998 B IDR in
    /// `a_forced_ltr_refresh_is_a_delta_not_an_idr` — and without one it falls back to a full
    /// keyframe.
    pub ltr: LtrStats,
    /// Bitrate the encoder was last given: the controller's target less the parity share
    /// ([`encoder_bps`]).
    pub encoder_bps: u64,
    /// Frames encoded from the held capture rather than a fresh one: the last capture of a
    /// picture that went still, or a refresh or keyframe answered while nothing changed.
    pub repaired: u64,
    /// Video datagrams that waited in the audio lane past their frame's own hand-over.
    pub laned: u64,
    /// Whether the stream is on the display-crop path *right now*. The counter above says how
    /// many frames came that way; this says where the next one will come from, which is what a
    /// test asking "did the crop go away when the window did" has to look at.
    pub on_crop: bool,
}

/// How long a stream that has drawn may go without a frame before the client is told the source
/// is idle again.
///
/// Longer than [`SOURCE_IDLE_AFTER`] on purpose: a target that draws slowly (a clock, a log that
/// scrolls once a second) would otherwise flap between the two states and spend a control message
/// on every change, and the receiver only needs to know before it starts asking for refreshes.
pub const SOURCE_QUIET_AFTER: Duration = Duration::from_secs(2);

/// What the client is told about the capture source, decided from the frames it has actually
/// produced lately rather than from whether it ever produced one.
///
/// The distinction matters because the receiver acts on it: while the source is idle it stops
/// asking for refreshes no refresh can answer, and it does not charge the silence to the link.
/// A latch on "has ever encoded a frame" gets the first answer right and every later one wrong —
/// a window that draws once and then hides, or is closed and left up, stays `Live` for the rest
/// of the stream.
#[derive(Clone, Copy, Debug)]
pub struct SourceTracker {
    /// The last state the client was told.
    reported: Option<SourceState>,
    /// `encoded` when a frame was last seen, and when that was.
    seen: u64,
    last_frame: Option<Instant>,
    opened_at: Instant,
}

impl SourceTracker {
    /// A tracker for a stream opened at `now`.
    #[must_use]
    pub const fn new(now: Instant) -> Self {
        Self { reported: None, seen: 0, last_frame: None, opened_at: now }
    }

    /// The state to report, or `None` when it has not changed (or is not yet knowable).
    ///
    /// `hidden` is the geometry tick's verdict on the target: a window that is not on screen
    /// cannot be drawing, whatever the frame counter says, and saying so at once is better than
    /// waiting out the quiet period.
    pub fn poll(&mut self, encoded: u64, hidden: bool, now: Instant) -> Option<SourceState> {
        if encoded > self.seen {
            self.seen = encoded;
            self.last_frame = Some(now);
        }
        let state = match self.last_frame {
            _ if hidden => SourceState::Idle,
            None if now.saturating_duration_since(self.opened_at) < SOURCE_IDLE_AFTER => {
                // Still inside the grace: say nothing rather than call a slow start idle.
                return None;
            }
            None => SourceState::Idle,
            Some(at) if now.saturating_duration_since(at) >= SOURCE_QUIET_AFTER => {
                SourceState::Idle
            }
            Some(_drew) => SourceState::Live,
        };
        (self.reported != Some(state)).then(|| {
            self.reported = Some(state);
            state
        })
    }
}

/// One stream as the control socket lists it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ScreenSummary {
    /// The client that opened it.
    pub client: String,
    /// Stream id on that connection.
    pub stream: u32,
    /// What it captures.
    pub target: CaptureTarget,
    /// Counters (final ones for a closed stream).
    pub stats: ScreenStats,
}

/// Closed streams the registry remembers.
const CLOSED_KEEP: usize = 8;

/// Every live stream in the daemon plus the last few closed ones, for the control socket.
#[derive(Clone, Default, Debug)]
pub struct Registry {
    inner: Arc<Mutex<RegistryInner>>,
    observer: Arc<Mutex<Observer>>,
}

#[derive(Default, Debug)]
struct RegistryInner {
    live: Vec<(String, CaptureTarget, StatsHandle)>,
    closed: VecDeque<ScreenSummary>,
}

/// Told the live-stream count after every change (the daemon's sleep policy listens). Called
/// with the registry's own lock held, so counts arrive in mutation order: two closes racing
/// an open could otherwise report `0` after `1` and release a hold with a stream still live.
/// The listener must therefore never call back into the registry.
#[derive(Default)]
struct Observer(Option<Box<dyn Fn(usize) + Send>>);

impl std::fmt::Debug for Observer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() { "Observer(set)" } else { "Observer(none)" })
    }
}

impl Registry {
    /// Call `f` with the number of live streams after every open and close.
    pub fn observe(&self, f: impl Fn(usize) + Send + 'static) {
        self.observer.lock().0 = Some(Box::new(f));
    }

    fn changed(&self, inner: &RegistryInner) {
        if let Some(f) = &self.observer.lock().0 {
            f(inner.live.len());
        }
    }

    /// Track a stream `client` opened.
    pub fn insert(
        &self,
        client: &impl std::fmt::Display,
        target: CaptureTarget,
        handle: StatsHandle,
    ) {
        let mut inner = self.inner.lock();
        inner.live.push((client.to_string(), target, handle));
        self.changed(&inner);
        drop(inner);
    }

    /// The stream closed: keep its final counters.
    pub fn remove(&self, client: &impl std::fmt::Display, id: StreamId) {
        let client = client.to_string();
        let mut inner = self.inner.lock();
        let Some(at) = inner.live.iter().position(|(c, _, h)| *c == client && h.id() == id) else {
            return;
        };
        let (client, target, handle) = inner.live.remove(at);
        if inner.closed.len() >= CLOSED_KEEP {
            inner.closed.pop_front();
        }
        inner.closed.push_back(ScreenSummary {
            client,
            stream: id.0,
            target,
            stats: handle.stats(),
        });
        self.changed(&inner);
        drop(inner);
    }

    /// Live streams with their counters right now, then the closed ones (oldest first).
    #[must_use]
    pub fn summaries(&self) -> (Vec<ScreenSummary>, Vec<ScreenSummary>) {
        let inner = self.inner.lock();
        let live = inner
            .live
            .iter()
            .map(|(client, target, handle)| ScreenSummary {
                client: client.clone(),
                stream: handle.id().0,
                target: *target,
                stats: handle.stats(),
            })
            .collect();
        (live, inner.closed.iter().cloned().collect())
    }
}

/// How long an enumeration of shareable content is reused. A client lists, picks and opens
/// within a few seconds, and `SCShareableContent` costs 60–75 ms per call (MEASUREMENTS.md,
/// "start-up on a cold connection"); the second call would return the same objects.
const SHAREABLE_TTL: Duration = Duration::from_secs(2);

/// The last enumeration and when it was taken. Typed per platform, and one platform runs in a
/// process, so the downcast back is exact.
static SHAREABLE: Mutex<Option<(Instant, Arc<dyn std::any::Any + Send + Sync>)>> = Mutex::new(None);

/// Enumerate shareable content, reusing an enumeration younger than `SHAREABLE_TTL`.
pub async fn shareable() -> Result<Arc<Content<Native>>, ScreenError> {
    ScreenStream::shareable().await
}

/// Start and stop one small capture of the first display; returns how long that took.
///
/// The first stream a client opens then does not pay ScreenCaptureKit's first start in this
/// process: ~300 ms cold against ~115 ms warm (MEASUREMENTS.md, "start-up on a cold
/// connection").
pub async fn warm_up() -> Result<Duration, ScreenError> {
    ScreenStream::warm_up().await
}

/// The `Listing` event for the current windows and displays.
pub async fn listing() -> Result<ScreenEvent, ScreenError> {
    ScreenStream::listing().await
}

/// Requests folded into the next encoded frame.
#[derive(Default)]
struct Pending {
    keyframe: bool,
    refresh: bool,
    acked: Vec<u64>,
}

struct Counters {
    audio_packets: AtomicU64,
    captured: AtomicU64,
    dropped: AtomicU64,
    withheld: AtomicU64,
    suspected: AtomicU64,
    suspicions: AtomicU64,
    siblings: AtomicU64,
    encoded: AtomicU64,
    datagrams: AtomicU64,
    queue_full: AtomicU64,
    /// Heartbeats sent while the source was quiet.
    heartbeats: AtomicU64,
    /// Refresh requests received from the client.
    refreshes: AtomicU64,
    /// Deferral episodes, one per run of put-off keyframes.
    keyframes_deferred: AtomicU64,
    latency_max_us: AtomicU64,
    latency_sum_us: AtomicU64,
    bitrate_bps: AtomicU64,
    encoder_bps: AtomicU64,
    cropped: AtomicU64,
    repaired: AtomicU64,
    laned: AtomicU64,
    ltr_offered: AtomicU64,
    ltr_acked: AtomicU64,
    refreshes_idr: AtomicU64,
    refreshes_delta: AtomicU64,
    capture: Mutex<LatencyRing>,
    encode: Mutex<LatencyRing>,
    beat_gap: Mutex<LatencyRing>,
    beat_gap_worst_us: AtomicU64,
    bounds: Mutex<LatencyRing>,
    /// `(pts, submitted at)` of frames inside the encoder, oldest first.
    in_flight: Mutex<VecDeque<(u64, u64)>>,
}

/// Frames the encoder may hold before the oldest submit record is forgotten (the encoder
/// runs with `MaxFrameDelayCount` 0, so this is only a bound against a callback that never
/// comes).
const IN_FLIGHT_MAX: usize = 16;

impl Counters {
    fn new() -> Self {
        Self {
            audio_packets: AtomicU64::new(0),
            captured: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            withheld: AtomicU64::new(0),
            suspected: AtomicU64::new(0),
            suspicions: AtomicU64::new(0),
            siblings: AtomicU64::new(0),
            encoded: AtomicU64::new(0),
            datagrams: AtomicU64::new(0),
            queue_full: AtomicU64::new(0),
            heartbeats: AtomicU64::new(0),
            refreshes: AtomicU64::new(0),
            keyframes_deferred: AtomicU64::new(0),
            latency_max_us: AtomicU64::new(0),
            latency_sum_us: AtomicU64::new(0),
            bitrate_bps: AtomicU64::new(0),
            encoder_bps: AtomicU64::new(0),
            cropped: AtomicU64::new(0),
            repaired: AtomicU64::new(0),
            laned: AtomicU64::new(0),
            ltr_offered: AtomicU64::new(0),
            ltr_acked: AtomicU64::new(0),
            refreshes_idr: AtomicU64::new(0),
            refreshes_delta: AtomicU64::new(0),
            capture: Mutex::new(LatencyRing::default()),
            encode: Mutex::new(LatencyRing::default()),
            beat_gap: Mutex::new(LatencyRing::default()),
            beat_gap_worst_us: AtomicU64::new(0),
            bounds: Mutex::new(LatencyRing::default()),
            in_flight: Mutex::new(VecDeque::with_capacity(IN_FLIGHT_MAX)),
        }
    }

    /// A frame went into the encoder at `now`.
    fn submitted(&self, pts_us: u64, now: u64) {
        let mut in_flight = self.in_flight.lock();
        if in_flight.len() >= IN_FLIGHT_MAX {
            in_flight.pop_front();
        }
        in_flight.push_back((pts_us, now));
    }

    /// The encoder returned the frame with `pts_us` at `now`: record its encode latency.
    fn returned(&self, pts_us: u64, now: u64) {
        let submitted = {
            let mut in_flight = self.in_flight.lock();
            let at = in_flight.iter().position(|&(pts, _)| pts == pts_us);
            let found = at.and_then(|i| in_flight.remove(i)).map(|(_, at)| at);
            drop(in_flight);
            found
        };
        if let Some(at) = submitted {
            self.encode.lock().push(now.saturating_sub(at));
        }
    }

    fn snapshot(&self) -> ScreenStats {
        ScreenStats {
            captured: self.captured.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            withheld: self.withheld.load(Ordering::Relaxed),
            keyframes_deferred: self.keyframes_deferred.load(Ordering::Relaxed),
            // `usable` and its age are the stream's state: `Shared::stats` fills them.
            ltr: LtrStats {
                offered: self.ltr_offered.load(Ordering::Relaxed),
                acked: self.ltr_acked.load(Ordering::Relaxed),
                refreshes_idr: self.refreshes_idr.load(Ordering::Relaxed),
                refreshes_delta: self.refreshes_delta.load(Ordering::Relaxed),
                usable: false,
                usable_age_us: 0,
            },
            encoder_bps: self.encoder_bps.load(Ordering::Relaxed),
            repaired: self.repaired.load(Ordering::Relaxed),
            laned: self.laned.load(Ordering::Relaxed),
            suspected: self.suspected.load(Ordering::Relaxed),
            suspicions: self.suspicions.load(Ordering::Relaxed),
            siblings: self.siblings.load(Ordering::Relaxed),
            encoded: self.encoded.load(Ordering::Relaxed),
            datagrams: self.datagrams.load(Ordering::Relaxed),
            queue_full: self.queue_full.load(Ordering::Relaxed),
            heartbeats: self.heartbeats.load(Ordering::Relaxed),
            refreshes: self.refreshes.load(Ordering::Relaxed),
            latency_max_us: self.latency_max_us.load(Ordering::Relaxed),
            latency_sum_us: self.latency_sum_us.load(Ordering::Relaxed),
            audio_packets: self.audio_packets.load(Ordering::Relaxed),
            bitrate_bps: self.bitrate_bps.load(Ordering::Relaxed),
            capture: self.capture.lock().quantiles(),
            encode: self.encode.lock().quantiles(),
            beat_gap: self.beat_gap.lock().quantiles(),
            beat_gap_worst_us: self.beat_gap_worst_us.load(Ordering::Relaxed),
            bounds: self.bounds.lock().quantiles(),
            cropped: self.cropped.load(Ordering::Relaxed),
            // Which path is live is the stream's state, not a counter: `Shared::stats` fills it,
            // and nothing else may hand this snapshot out (it would claim the window filter).
            on_crop: false,
        }
    }
}

/// What the registry reads of a stream, whatever platform it runs on.
trait Counted: Send + Sync {
    fn stream(&self) -> StreamId;
    fn counters(&self) -> ScreenStats;
}

impl<P: Platform> Counted for Shared<P> {
    fn stream(&self) -> StreamId {
        self.id
    }

    fn counters(&self) -> ScreenStats {
        self.stats()
    }
}

/// A handle on one stream's counters that outlives the [`ScreenStream`]'s owner borrow, for
/// the daemon's control socket (`slopty bench screen` reads the worker side through it).
#[derive(Clone)]
pub struct StatsHandle(Arc<dyn Counted>);

impl std::fmt::Debug for StatsHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatsHandle").field("stream", &self.0.stream()).finish()
    }
}

impl StatsHandle {
    /// The stream.
    #[must_use]
    pub fn id(&self) -> StreamId {
        self.0.stream()
    }

    /// Counters right now.
    #[must_use]
    pub fn stats(&self) -> ScreenStats {
        self.0.counters()
    }
}

/// The part of a stream the client's feedback reaches directly.
///
/// Loss and reports are answered on the connection's own task, never behind a stream that is
/// busy rebuilding its encoder or reading the window server.
pub struct StreamControl<P: Platform = Native>(Arc<Shared<P>>);

impl<P: Platform> Clone for StreamControl<P> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<P: Platform> std::fmt::Debug for StreamControl<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamControl").field("stream", &self.0.id).finish()
    }
}

impl<P: Platform> StreamControl<P> {
    /// See [`ScreenStream::report`].
    #[must_use]
    pub fn report(&self, report: &ReceiverReport, path: Option<PathSample>) -> Option<Decision> {
        self.0.report(report, path)
    }

    /// See [`ScreenStream::request_refresh`].
    pub fn request_refresh(&self, last_good_frame: u32, keyframe: bool) {
        self.0.request_refresh(last_good_frame, keyframe);
    }

    /// See [`ScreenStream::nack`].
    pub fn nack(&self, frame: u32, fragments: &[u16]) {
        self.0.nack(frame, fragments);
    }

    /// See [`ScreenStream::zoom`]; current across a rebuild, readable from the connection's task.
    #[must_use]
    pub fn zoom(&self) -> f64 {
        self.0.zoom()
    }
}

/// Audio gate: after this long without a sample above [`AUDIO_FLOOR`] the stream stops
/// sending packets (silent apps cost nothing on the wire; the client pads silence).
const AUDIO_HOLD_US: u64 = 300_000;
/// Anything quieter than this is silence (-80 dBFS).
const AUDIO_FLOOR: f32 = 1e-4;

/// The encoder and the silence gate.
struct AudioState<A> {
    encoder: Option<A>,
    seq: u32,
    last_loud_us: u64,
}

struct Shared<P: Platform = Native> {
    id: StreamId,
    encoder: RwLock<Option<P::Video>>,
    audio: Mutex<AudioState<P::Audio>>,
    pending: Mutex<Pending>,
    packetizer: Mutex<Packetizer>,
    redundancy: Mutex<Redundancy>,
    rate: Mutex<RateController>,
    /// `datagrams_sent` at the previous receiver report.
    sent_at_report: AtomicU64,
    /// `host_now_us()` when the last datagram was queued; the heartbeat clock.
    last_push_us: AtomicU64,
    /// The cadence rung in force: how many of the captures reach the encoder, and the frame rate
    /// the held-bytes limit is computed from.
    fps: std::sync::atomic::AtomicU16,
    /// The cadence the client asked for; the ladder never climbs past it.
    fps_ceiling: std::sync::atomic::AtomicU16,
    /// `capture_ts_us` of the last frame handed to the encoder; the cadence gate's clock.
    last_encoded_us: AtomicU64,
    /// What a keyframe costs on this stream, as [`keyframe_estimate`] tracks it; `0` until one
    /// has been encoded.
    keyframe_bytes: AtomicU64,
    /// `host_now_us()` when the current run of deferrals began, `0` when none is running: the
    /// valve's clock.
    keyframe_deferred_us: AtomicU64,
    /// The long-term references this encoder session offered and the client acknowledged.
    ltr: Mutex<LtrBook>,
    /// The newest capture of the target, kept after it is encoded. ScreenCaptureKit sends
    /// nothing while the picture is still, so this is the only picture a skipped frame, a
    /// refresh or a keyframe asked for on a still screen can be answered with
    /// ([`repair_loop`]). Every encode goes through this lock, which also keeps the
    /// presentation timestamps the encoder sees in order.
    held: Mutex<Option<Frame<P>>>,
    /// The held capture has not reached the encoder.
    owed: AtomicBool,
    /// Wakes [`repair_loop`]: a capture was held back, or a request came in.
    repair: tokio::sync::Notify,
    /// Video datagrams waiting to be handed to QUIC behind a slice of the link, so audio goes
    /// ahead of them ([`Lane`]).
    lane: Mutex<Lane>,
    /// Wakes [`lane_loop`]: video is waiting in the lane.
    lane_wake: tokio::sync::Notify,
    /// `host_now_us()` of the last audio packet sent; the lane is used only while audio flows.
    last_audio_us: AtomicU64,
    /// Stream pixels per native pixel of the target (the `scale` of the quality in force), as
    /// `f64` bits: what the cursor loop multiplies a position by, kept current across a rebuild.
    zoom: AtomicU64,
    /// Whether frames come through the display-crop path right now.
    cropped: AtomicBool,
    /// The target window is not on screen. Nothing ScreenCaptureKit delivers can be a picture of
    /// it, so nothing is sent: under a display crop the rectangle holds whatever is behind the
    /// window, and a swap to the window filter that the framework rejected leaves the crop
    /// running while this side believes otherwise. Set from the geometry tick.
    target_hidden: AtomicBool,
    /// The worker's pointer is over the target, as the cursor loop last saw it; the shape loop
    /// reads the cursor's picture only while it is.
    pointer_over: AtomicBool,
    /// `host_now_us()` until which frames are held on the accessibility API's word alone
    /// (zero: no suspicion). Set by the hide watch's callback, read on every frame; the
    /// geometry tick's `target_hidden` is the confirmation that outlives it.
    suspect_until_us: AtomicU64,
    /// A window of the target's application other than the target went since the filter was
    /// last changed. ScreenCaptureKit stops delivering for an application-scoped display filter
    /// when any window of that application is ordered out, and only a change of filter kind
    /// wakes it (MEASUREMENTS.md, "a sibling window closing stalls the crop"); the geometry
    /// tick takes a stream on the crop through the window filter and back.
    filter_stalled: AtomicBool,
    /// Where the stream's datagrams go.
    sink: Arc<dyn DatagramSink>,
    /// A too-large datagram has been logged; it is a sizing bug, said once.
    too_large_logged: AtomicBool,
    /// The target's bounds as the last geometry probe read them, for the cursor loop.
    bounds: Mutex<Option<Rect>>,
    counters: Counters,
}

impl<P: Platform> Shared<P> {
    /// A stream's state before its encoder is built: a keyframe pending, the rate controller
    /// under `max_bps`, the cadence at `fps`.
    fn new(
        id: StreamId,
        sink: Arc<dyn DatagramSink>,
        max_bps: u32,
        fps: u16,
        cropped: bool,
    ) -> Self {
        Self {
            id,
            encoder: RwLock::new(None),
            audio: Mutex::new(AudioState { encoder: None, seq: 0, last_loud_us: 0 }),
            pending: Mutex::new(Pending { keyframe: true, ..Pending::default() }),
            packetizer: Mutex::new(Packetizer::new(id)),
            redundancy: Mutex::new(Redundancy::new()),
            rate: Mutex::new(RateController::new(max_bps)),
            sent_at_report: AtomicU64::new(0),
            last_push_us: AtomicU64::new(now::<P>()),
            fps: std::sync::atomic::AtomicU16::new(fps),
            fps_ceiling: std::sync::atomic::AtomicU16::new(fps),
            last_encoded_us: AtomicU64::new(0),
            keyframe_bytes: AtomicU64::new(0),
            keyframe_deferred_us: AtomicU64::new(0),
            ltr: Mutex::new(LtrBook::default()),
            held: Mutex::new(None),
            owed: AtomicBool::new(false),
            repair: tokio::sync::Notify::new(),
            lane: Mutex::new(Lane::default()),
            lane_wake: tokio::sync::Notify::new(),
            last_audio_us: AtomicU64::new(0),
            zoom: AtomicU64::new(1.0_f64.to_bits()),
            cropped: AtomicBool::new(cropped),
            target_hidden: AtomicBool::new(false),
            pointer_over: AtomicBool::new(false),
            suspect_until_us: AtomicU64::new(0),
            filter_stalled: AtomicBool::new(false),
            sink,
            too_large_logged: AtomicBool::new(false),
            bounds: Mutex::new(None),
            counters: Counters::new(),
        }
    }

    /// The counters plus the state only the stream knows: whether the display crop is what is
    /// being served right now, and whether a long-term reference is usable. Every reader goes
    /// through here, so no caller can publish the snapshot's placeholders.
    fn stats(&self) -> ScreenStats {
        let snapshot = self.counters.snapshot();
        let age = self.ltr.lock().usable_age(now::<P>());
        ScreenStats {
            on_crop: self.cropped.load(Ordering::Relaxed),
            ltr: LtrStats {
                usable: age.is_some(),
                usable_age_us: age.unwrap_or(0),
                ..snapshot.ltr
            },
            ..snapshot
        }
    }

    /// Stream pixels per native pixel right now.
    fn zoom(&self) -> f64 {
        f64::from_bits(self.zoom.load(Ordering::Relaxed))
    }

    /// Whether a refresh would come back as a delta: an acknowledged reference newer than the
    /// latest keyframe is on record.
    fn ltr_usable(&self) -> bool {
        self.ltr.lock().usable.is_some()
    }

    /// Point the live encoder at the share of `target` the parity leaves it ([`encoder_bps`]);
    /// a rebuild picks the controller's target up again. `target` stays the rate the guards
    /// read: the bytes they weigh are frames and their parity together.
    fn apply_bitrate(&self, target: u32) {
        let parity = self.packetizer.lock().parity_permille();
        let bps = encoder_bps(target, parity);
        let result = self.encoder.read().as_ref().map(|e| e.set_bitrate(bps));
        match result {
            Some(Ok(())) => {
                self.counters.bitrate_bps.store(u64::from(target), Ordering::Relaxed);
                self.counters.encoder_bps.store(u64::from(bps), Ordering::Relaxed);
                tracing::debug!(stream = %self.id, target, bps, parity, "bitrate");
            }
            Some(Err(e)) => tracing::warn!(stream = %self.id, bps, error = %e, "set bitrate"),
            None => {}
        }
    }

    /// Put a new encoder session in, in place of the old one, and reset what described the old
    /// one ([`Self::rebuilt`]) before the encoder's write lock goes. An encode holds the read
    /// lock from taking its requests to submitting the frame, so no frame reaches the new
    /// session with the old one's requests, and none of the new session's packets can be filed
    /// in the old book. The old session is invalidated in the swap, which ends its callbacks,
    /// so none of its packets lands after the reset either. The held capture is the old size.
    fn install(&self, encoder: P::Video) {
        let mut slot = self.encoder.write();
        *slot = Some(encoder);
        self.rebuilt();
        drop(slot);
        // Outside the write lock: an encode takes the held capture's lock before the encoder's.
        self.forget_held();
    }

    /// A new encoder session replaced the old one: its references, the acknowledgements still
    /// queued for them, the keyframe estimate and the valve's clock all described the old
    /// session. The new session starts on a keyframe. The book stays locked while the queued
    /// acknowledgements go, as [`Self::report`] holds it while queueing them, so a report is
    /// wholly before the reset or wholly after it.
    fn rebuilt(&self) {
        let mut ltr = self.ltr.lock();
        ltr.reset();
        {
            let mut pending = self.pending.lock();
            pending.acked.clear();
            pending.keyframe = true;
        }
        drop(ltr);
        self.keyframe_bytes.store(0, Ordering::Relaxed);
        self.keyframe_deferred_us.store(0, Ordering::Relaxed);
    }

    /// Drop the held capture: it is not a picture of the target any more (hidden, suspected, or
    /// another size).
    fn forget_held(&self) {
        *self.held.lock() = None;
        self.owed.store(false, Ordering::Relaxed);
    }

    /// Bytes waiting to leave: what QUIC holds plus what waits in the lane.
    fn held_bytes(&self) -> usize {
        self.sink.held().saturating_add(self.lane.lock().bytes)
    }

    /// Move the cadence to the rung `bps` affords, and tell the encoder it did.
    ///
    /// Capture is left at the ceiling either way: a change on screen is still seen within a display
    /// beat, only fewer of those captures are encoded, so the picture that does go out is worth its
    /// bandwidth (`docs/decisions/video.md`, the cadence ladder).
    fn apply_cadence(&self, bps: u32) {
        let ceiling = self.fps_ceiling.load(Ordering::Relaxed);
        let mut cadence = Cadence::resume(ceiling, self.fps.load(Ordering::Relaxed));
        let Some(fps) = cadence.update(bps) else { return };
        self.fps.store(fps, Ordering::Relaxed);
        let result = self.encoder.read().as_ref().map(|e| e.set_frame_rate(fps));
        if let Some(Err(e)) = result {
            tracing::warn!(stream = %self.id, fps, error = %e, "set frame rate");
        }
        tracing::debug!(stream = %self.id, fps, bps, "cadence");
    }

    /// Hand `datagrams` to the transport now, in one call; what it took.
    ///
    /// A datagram too large for the path is a sizing bug, not the end of the stream: the
    /// packetizer cut the frame for a size the path had when it started and has no longer. The
    /// ones that still fit go, the rest are counted as refused, and it is logged once.
    fn send(&self, datagrams: &[Bytes]) -> Taken {
        if datagrams.is_empty() {
            return Taken::default();
        }
        let now = now::<P>();
        let n = u64::try_from(datagrams.len()).unwrap_or(u64::MAX);
        let taken = match self.sink.send(datagrams) {
            Ok(()) => Taken::of(datagrams),
            Err(Refused::Closed) => Taken::default(),
            Err(Refused::TooLarge) => {
                let max = self.sink.max_size().unwrap_or(0);
                if !self.too_large_logged.swap(true, Ordering::Relaxed) {
                    let largest = datagrams.iter().map(Bytes::len).max().unwrap_or(0);
                    tracing::warn!(stream = %self.id, largest, max, "a datagram larger than the path carries: the frame was cut for a stale size");
                }
                let fitting: Vec<Bytes> =
                    datagrams.iter().filter(|d| d.len() <= max).cloned().collect();
                let sent = !fitting.is_empty() && self.sink.send(&fitting).is_ok();
                if sent { Taken::of(&fitting) } else { Taken::default() }
            }
        };
        if taken.datagrams > 0 {
            self.counters.datagrams.fetch_add(taken.datagrams, Ordering::Relaxed);
            self.last_push_us.store(now, Ordering::Relaxed);
        }
        self.counters.queue_full.fetch_add(n.saturating_sub(taken.datagrams), Ordering::Relaxed);
        taken
    }

    /// Whether the transport can take another frame now ([`frame_fits`]). The window is only
    /// asked for when something is held: with nothing held any frame fits.
    fn frame_fits(&self) -> bool {
        let held = self.held_bytes();
        let cwnd = if held == 0 { 0 } else { self.sink.cwnd() };
        frame_fits(
            held,
            cwnd,
            self.counters.bitrate_bps.load(Ordering::Relaxed),
            self.fps.load(Ordering::Relaxed),
        )
    }

    /// The accessibility API says a window of the target's application went at `now`: hold
    /// frames for [`SUSPICION_HOLD`] while the window list catches up; the geometry tick moves
    /// the stream to the window filter meanwhile.
    fn suspect(&self, now: u64) {
        let hold_us = u64::try_from(SUSPICION_HOLD.as_micros()).unwrap_or(u64::MAX);
        self.suspect_until_us.store(now.saturating_add(hold_us), Ordering::Relaxed);
        let suspicions = self.counters.suspicions.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(stream = %self.id, suspicions = suspicions.saturating_add(1), "hide suspected");
    }

    /// The accessibility API says a window of the target's application that is not the target
    /// went: no hold, but the capture may have stalled on it, so the next geometry tick takes
    /// the stream through the window filter.
    fn sibling_went(&self) {
        self.filter_stalled.store(true, Ordering::Relaxed);
        let siblings = self.counters.siblings.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(stream = %self.id, siblings = siblings.saturating_add(1), "another window of the application went");
    }

    /// Whether a suspicion raised by [`Self::suspect`] is still holding frames at `now`.
    fn suspected_at(&self, now: u64) -> bool {
        now < self.suspect_until_us.load(Ordering::Relaxed)
    }

    /// ScreenCaptureKit delivered a frame: hold it as the newest picture of the target and
    /// encode it if the cadence, the congestion guard and the keyframe rule let it through. One
    /// they hold back is owed, and [`repair_loop`] sends it if nothing newer comes.
    fn on_frame(&self, frame: Frame<P>) {
        self.counters.captured.fetch_add(1, Ordering::Relaxed);
        self.counters.capture.lock().push(frame.latency_us);
        if !self.is_of_target() {
            self.forget_held();
            return;
        }
        let mut held = self.held.lock();
        *held = Some(frame);
        self.owed.store(true, Ordering::Relaxed);
        let attempt = self.try_encode(held.as_ref(), true, now::<P>());
        drop(held);
        if attempt != Attempt::Sent {
            self.repair.notify_one();
        }
    }

    /// Whether a frame arriving now may be taken as a picture of the target, counting the ones
    /// that may not.
    fn is_of_target(&self) -> bool {
        // Before anything is counted as a picture of the target: while the window is off screen
        // no frame can be one, and a display crop keeps delivering the desktop behind it
        // (MEASUREMENTS.md, "a hidden window on the crop path").
        if self.target_hidden.load(Ordering::Relaxed) {
            self.counters.withheld.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        // And for the ~260 ms before the window list knows, the accessibility API's word: a
        // window of that application just went, so a crop may be the desktop already. The
        // geometry tick moves a suspected stream to the window filter meanwhile, but its first
        // frames are held too: ScreenCaptureKit still delivers a frame or two of the old filter
        // after the swap has settled, and with the swap landing before the window list knows,
        // those showed the backdrop (MEASUREMENTS.md, "a sibling window closing stalls the
        // crop"). Nothing is sent for the hold, whichever path it arrives on.
        if self.suspected_at(now::<P>()) {
            self.counters.suspected.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        if self.cropped.load(Ordering::Relaxed) {
            self.counters.cropped.fetch_add(1, Ordering::Relaxed);
        }
        true
    }

    /// Encode the held capture if the gates let it through. `fresh` is a capture that just
    /// arrived; otherwise it is [`repair_loop`] sending the held one again at `now`.
    fn try_encode(&self, held: Option<&Frame<P>>, fresh: bool, now: u64) -> Attempt {
        let Some(frame) = held else { return Attempt::Nothing };
        // Held from reading the requests to submitting the frame, so a rebuild is wholly before
        // or wholly after them ([`Self::install`]).
        let encoder = self.encoder.read();
        let owed = self.owed.load(Ordering::Relaxed);
        let (want_keyframe, want_refresh) = {
            let pending = self.pending.lock();
            (pending.keyframe, pending.refresh)
        };
        if !owed && !want_keyframe && !want_refresh {
            return Attempt::Nothing;
        }
        // A repair is a picture of the target as it is now, stamped now: the capture's own time
        // would put it behind the frame already encoded from it.
        let last = self.last_encoded_us.load(Ordering::Relaxed);
        let at = if fresh { frame.capture_ts_us } else { now };
        // The cadence rung, before the congestion guard: a capture the ladder is not asking for is
        // not a frame the link failed to carry, and skipping it is what gives the next one the
        // bytes to be worth sending. No refresh is owed, the client is missing nothing.
        // A keyframe is the one thing the rung does not hold back: it is what a client with no
        // picture at all is waiting on, and there is at most one in flight. A pending *refresh* is
        // not urgent in the same way — at a collapsed rate the guard below sets one on every frame
        // it drops, and letting those through would take the cadence off exactly where it is
        // needed.
        let due = frame_due(at.saturating_sub(last), self.fps.load(Ordering::Relaxed));
        if !due && !want_keyframe {
            return Attempt::NotDue;
        }
        if !self.frame_fits() {
            // A capture that did not fit is a hole in the stream; a repair that did not fit is
            // the same picture waiting another period.
            if fresh {
                self.dropped(now);
            }
            return Attempt::NoRoom;
        }
        // A keyframe the link cannot drain is put off and an LTR refresh encoded in its place: a
        // picture the client can decode, at a fraction of the bytes. The request stays pending, so
        // the keyframe follows as soon as the link can carry one or the valve opens. With it
        // deferred there is nothing urgent in this frame, so the cadence rung applies again.
        let defer = want_keyframe && !self.keyframe_admitted(now);
        if defer && !due {
            return Attempt::NotDue;
        }
        let options = {
            let mut pending = self.pending.lock();
            let force_keyframe = if defer { false } else { std::mem::take(&mut pending.keyframe) };
            FrameOptions {
                force_keyframe,
                force_ltr_refresh: std::mem::take(&mut pending.refresh) || defer,
                acked_ltr: std::mem::take(&mut pending.acked),
            }
        };
        // The encoder wants presentation times that only go forward.
        let pts = at.max(last.saturating_add(1));
        self.last_encoded_us.store(pts, Ordering::Relaxed);
        self.owed.store(false, Ordering::Relaxed);
        if !fresh {
            self.counters.repaired.fetch_add(1, Ordering::Relaxed);
        }
        self.counters.submitted(pts, now);
        let outcome = encoder.as_ref().map(|encoder| encoder.encode(&frame.image, pts, &options));
        if let Some(Err(e)) = outcome {
            self.counters.returned(pts, Source::<P>::now_us());
            tracing::warn!(stream = %self.id, error = %e, "encode failed");
            self.owed.store(true, Ordering::Relaxed);
            let mut pending = self.pending.lock();
            pending.keyframe |= options.force_keyframe;
            pending.refresh |= options.force_ltr_refresh;
            pending.acked.extend(options.acked_ltr);
            drop(pending);
            drop(encoder);
            return Attempt::Failed;
        }
        drop(encoder);
        Attempt::Sent
    }

    /// When [`repair_loop`] should next look at the held capture; `None` while nothing is owed
    /// or asked for.
    fn repair_at(&self) -> Option<u64> {
        let captured = self.held.lock().as_ref()?.capture_ts_us;
        let (keyframe, refresh) = {
            let pending = self.pending.lock();
            (pending.keyframe, pending.refresh)
        };
        repair_at(
            Asks { owed: self.owed.load(Ordering::Relaxed), refresh, keyframe },
            captured,
            self.last_encoded_us.load(Ordering::Relaxed),
            self.fps.load(Ordering::Relaxed),
            self.fps_ceiling.load(Ordering::Relaxed),
        )
    }

    /// Send the held capture again at `now`, unless the target is no longer on screen.
    fn repair_now(&self, now: u64) -> Attempt {
        if self.target_hidden.load(Ordering::Relaxed) || self.suspected_at(now) {
            self.forget_held();
            return Attempt::Nothing;
        }
        let held = self.held.lock();
        self.try_encode(held.as_ref(), false, now)
    }

    /// ScreenCaptureKit delivered PCM: encode and send unless the source has gone quiet.
    ///
    /// Audio goes to QUIC at once, ahead of any video waiting in the lane.
    fn on_audio(&self, chunk: &CapturedAudio) {
        let now = now::<P>();
        let Some(packets) = self.encode_audio(&chunk.samples, now) else { return };
        let datagrams: Vec<Bytes> = packets
            .iter()
            .filter_map(|(seq, packet)| audio_datagram(self.id, *seq, send_ms_lo(now), packet))
            .collect();
        self.last_audio_us.store(now, Ordering::Relaxed);
        let taken = self.send(&datagrams);
        self.counters.audio_packets.fetch_add(taken.datagrams, Ordering::Relaxed);
    }

    /// Hand a frame's video datagrams on: straight to QUIC while no audio flows, through the
    /// lane while it does, and always behind video already waiting there.
    fn send_video(&self, datagrams: &[Bytes], now: u64) {
        let mut lane = self.lane.lock();
        let audio = now.saturating_sub(self.last_audio_us.load(Ordering::Relaxed))
            < LANE_AUDIO_HOLD_US
            && self.last_audio_us.load(Ordering::Relaxed) != 0;
        if lane.queue.is_empty() && !audio {
            // Nothing to let ahead: a frame costs the lane nothing. The look keeps the drain rate
            // current for when audio starts.
            let held = self.sink.held();
            lane.observe(held, now);
            lane.expected = held.saturating_add(self.send(datagrams).bytes);
            return;
        }
        lane.push(datagrams);
        self.pump(&mut lane, now);
        // Of this frame's datagrams, the ones still waiting: the queue's tail.
        let waiting = lane.queue.len().min(datagrams.len());
        drop(lane);
        if waiting > 0 {
            self.counters
                .laned
                .fetch_add(u64::try_from(waiting).unwrap_or(u64::MAX), Ordering::Relaxed);
            self.lane_wake.notify_one();
        }
    }

    /// Hand QUIC as much of the lane as fits in its slice. Under the lane's lock, so video
    /// leaves in the order it was queued whichever thread tops it up.
    fn pump(&self, lane: &mut Lane, now: u64) {
        let held = self.sink.held();
        lane.observe(held, now);
        let floor = self.counters.bitrate_bps.load(Ordering::Relaxed) / 8;
        let batch = lane.take(held, lane.budget(floor));
        lane.expected = lane.expected.saturating_add(self.send(&batch).bytes);
    }

    /// Run the silence gate and the encoder under the audio lock; `None` when nothing goes out.
    fn encode_audio(&self, samples: &[f32], now: u64) -> Option<Vec<(u32, Bytes)>> {
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
                    tracing::warn!(stream = %self.id, error = %e, "no Opus encoder; audio off");
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
                tracing::debug!(stream = %self.id, error = %e, "opus encode");
                None
            }
        }
    }

    /// VideoToolbox produced an access unit.
    fn on_packet(&self, packet: &EncodedPacket) {
        let now = now::<P>();
        self.counters.returned(packet.pts_us, now);
        let latency = now.saturating_sub(packet.pts_us);
        let encoded = self.counters.encoded.fetch_add(1, Ordering::Relaxed);
        if packet.keyframe {
            let bytes = u64::try_from(packet.data.len()).unwrap_or(u64::MAX);
            let estimate = keyframe_estimate(self.keyframe_bytes.load(Ordering::Relaxed), bytes);
            self.keyframe_bytes.store(estimate, Ordering::Relaxed);
            self.keyframe_deferred_us.store(0, Ordering::Relaxed);
        }
        if self.ltr.lock().on_packet(packet.keyframe, packet.ltr_token, now) {
            self.counters.ltr_offered.fetch_add(1, Ordering::Relaxed);
        }
        if packet.ltr_refresh {
            let answered = if packet.keyframe {
                &self.counters.refreshes_idr
            } else {
                &self.counters.refreshes_delta
            };
            answered.fetch_add(1, Ordering::Relaxed);
        }
        if encoded == 0 || packet.keyframe {
            tracing::debug!(
                stream = %self.id,
                frame = encoded,
                bytes = packet.data.len(),
                keyframe = packet.keyframe,
                encode_ms = latency / 1000,
                "keyframe encoded"
            );
        }
        self.counters.latency_max_us.fetch_max(latency, Ordering::Relaxed);
        self.counters.latency_sum_us.fetch_add(latency, Ordering::Relaxed);
        #[expect(clippy::cast_possible_truncation, reason = "low bits by design")]
        let capture_ts_us = packet.pts_us as u32;
        let frame = EncodedFrame {
            data: &packet.data,
            keyframe: packet.keyframe,
            ltr_token: packet.ltr_token,
            ltr_refresh: packet.ltr_refresh,
            capture_ts_us,
        };
        let max = self.sink.max_size().map_or(MAX_DATAGRAM, |m| m.min(MAX_DATAGRAM));
        let cut = {
            let mut packetizer = self.packetizer.lock();
            packetizer.set_max_datagram(max);
            packetizer.packetize(&frame, send_ms_lo(now)).map(|sent| sent.datagrams.clone())
        };
        match cut {
            Ok(datagrams) => self.send_video(&datagrams, now),
            Err(e) => tracing::warn!(stream = %self.id, error = %e, "packetize failed"),
        }
    }
}

/// What became of an attempt to encode the held capture.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Attempt {
    /// Submitted to the encoder.
    Sent,
    /// Nothing held, or the held capture was encoded and nothing is asked of it.
    Nothing,
    /// The cadence has not come round yet.
    NotDue,
    /// The transport holds more than the guard allows.
    NoRoom,
    /// The encoder refused it; the requests it carried are pending again.
    Failed,
}

impl<P: Platform> Shared<P> {
    /// A receiver report: acknowledged LTR tokens go to the encoder, the loss to the parity
    /// and rate controllers; a changed target is applied to the encoder.
    fn report(&self, report: &ReceiverReport, path: Option<PathSample>) -> Option<Decision> {
        let acked = {
            let mut ltr = self.ltr.lock();
            let was_usable = ltr.usable.is_some();
            // Only this session's tokens reach the encoder: one acknowledged for the session a
            // rebuild replaced names nothing the new one offered.
            let acked: Vec<u64> = report
                .acked_ltr
                .iter()
                .take(usize::from(report.acked_ltr_len))
                .copied()
                .filter(|&token| ltr.on_ack(token))
                .collect();
            if !was_usable && ltr.usable.is_some() {
                // From here a refresh is a delta off the reference rather than an IDR, so a
                // keyframe can be deferred for one (`keyframe_admitted`).
                tracing::debug!(stream = %self.id, token = ?ltr.usable.map(|(t, _at)| t), "LTR reference usable");
            }
            let count = u64::try_from(acked.len()).unwrap_or(u64::MAX);
            // Queued under the book that vouched for them: a rebuild between the two would
            // hand the new session the old one's tokens ([`Self::rebuilt`]).
            self.pending.lock().acked.extend(acked);
            drop(ltr);
            count
        };
        self.counters.ltr_acked.fetch_add(acked, Ordering::Relaxed);
        let sent_total = self.packetizer.lock().datagrams_sent();
        let previous = self.sent_at_report.swap(sent_total, Ordering::Relaxed);
        let sent = u32::try_from(sent_total.saturating_sub(previous)).unwrap_or(u32::MAX);
        let permille = self.redundancy.lock().on_report(report, sent);
        let parity_moved = {
            let mut packetizer = self.packetizer.lock();
            let moved = packetizer.parity_permille() != permille;
            packetizer.set_parity_permille(permille);
            moved
        };
        let decision = self.rate.lock().on_report(report, sent, path);
        let Some(decision) = decision else {
            if parity_moved {
                // The parity's share of the target moved, so the encoder's did too.
                self.apply_bitrate(self.rate.lock().target_bps());
            }
            return None;
        };
        tracing::debug!(
            stream = %self.id,
            verdict = ?decision.verdict,
            target_bps = decision.target_bps,
            capped = decision.capped,
            loss_permille = decision.window.loss_permille(),
            queue_max = decision.window.queue_max,
            hold_max_ms = decision.window.hold_max.as_millis(),
            stalled_ms = decision.window.stalled_ms,
            stalls = decision.window.stalls,
            "rate decision"
        );
        if decision.changed {
            self.apply_bitrate(decision.target_bps);
            self.apply_cadence(decision.target_bps);
        }
        Some(decision)
    }

    /// The guard dropped a frame: count it, and decide whether to ask for a picture that
    /// stands on its own.
    ///
    /// The client will see a hole, so the obvious answer is to ask on every drop. That is what
    /// this did, and it is the shape a slow link turning into a stalled one would take. A forced
    /// refresh comes back from this encoder as a full IDR — 133 960 B measured — and not the
    /// 733 B delta it produces with a fresh long-term reference on hand, because VideoToolbox
    /// offers about one LTR token per stream and the single reference goes stale
    /// (`docs/decisions/transport.md`). So a link already too slow for ordinary frames was being
    /// asked for keyframes twenty times their size, each of which filled the queue and caused the
    /// next drop. Ask only when the link could drain one; a held picture for a beat beats a
    /// picture that never arrives. Whether a reference is usable does not enter into it: a
    /// refresh is budgeted as the keyframe it may turn out to be, and until the stream's first
    /// keyframe has been measured the budget admits it.
    fn dropped(&self, now: u64) {
        self.counters.dropped.fetch_add(1, Ordering::Relaxed);
        if self.standalone_fits(now) {
            self.pending.lock().refresh = true;
        }
    }

    /// Whether a wanted keyframe should be encoded now rather than put off for an LTR refresh.
    ///
    /// Deferring needs somewhere to fall back to: with no usable long-term reference a refresh
    /// comes back as an IDR anyway, so the keyframe goes out. Past that the drain budget decides
    /// ([`Self::standalone_fits`]).
    fn keyframe_admitted(&self, now: u64) -> bool {
        !self.ltr_usable() || self.standalone_fits(now)
    }

    /// Whether a picture that stands on its own is worth the bytes right now: the link drains
    /// one inside [`KEYFRAME_DRAIN_MS`], or it has been put off for [`KEYFRAME_VALVE_US`].
    ///
    /// A link that can carry one ends the episode, and not only a keyframe encoded: the next
    /// collapse then starts its own clock and is counted as its own episode, where a stale start
    /// would open the valve on its first frame.
    fn standalone_fits(&self, now: u64) -> bool {
        if keyframe_fits(
            self.keyframe_bytes.load(Ordering::Relaxed),
            self.held_bytes(),
            self.counters.bitrate_bps.load(Ordering::Relaxed),
        ) {
            self.keyframe_deferred_us.store(0, Ordering::Relaxed);
            return true;
        }
        // Counted where the clock starts, so the number is deferral episodes and not the frames
        // each one spans — at 60 fps a one-second run would otherwise read as sixty.
        match self.keyframe_deferred_us.compare_exchange(
            0,
            now,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_started) => {
                self.counters.keyframes_deferred.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    stream = %self.id,
                    estimate = self.keyframe_bytes.load(Ordering::Relaxed),
                    held = self.held_bytes(),
                    target_bps = self.counters.bitrate_bps.load(Ordering::Relaxed),
                    "a picture that stands on its own is deferred: the link cannot drain one"
                );
                false
            }
            Err(started) => now.saturating_sub(started) >= KEYFRAME_VALVE_US,
        }
    }

    /// The client lost a frame it cannot recover: make the next frame stand on its own. On a
    /// still screen there is no next frame, so the held capture answers ([`repair_loop`]).
    ///
    /// `keyframe` says the client's decoder holds no reference any more (a new decoder session),
    /// so an LTR refresh would be predicted from a picture it does not have and fail to decode
    /// (-17694). Its acknowledged references are dropped and a keyframe is asked for, which then
    /// has no reference left to be deferred for ([`Self::keyframe_admitted`]).
    fn request_refresh(&self, last_good_frame: u32, keyframe: bool) {
        tracing::debug!(stream = %self.id, last_good_frame, keyframe, "refresh requested");
        self.counters.refreshes.fetch_add(1, Ordering::Relaxed);
        if keyframe {
            self.ltr.lock().client_lost();
            self.pending.lock().keyframe = true;
        } else {
            self.pending.lock().refresh = true;
        }
        self.repair.notify_one();
    }

    /// Retransmit fragments of a recent frame, unless the transport is holding more than the
    /// frame budget (see [`ScreenStream::nack`]).
    fn nack(&self, frame: u32, fragments: &[u16]) {
        if !self.frame_fits() {
            tracing::debug!(stream = %self.id, frame, held = self.held_bytes(), "nack not answered: transport is holding frames");
            return;
        }
        let datagrams = self.packetizer.lock().retransmit(frame, fragments);
        if datagrams.is_empty() {
            tracing::debug!(stream = %self.id, frame, "nack for a frame outside the history");
        }
        self.send_video(&datagrams, now::<P>());
    }
}

/// Low byte of the worker millisecond clock.
const fn send_ms_lo(now_us: u64) -> u8 {
    #[expect(clippy::cast_possible_truncation, reason = "low byte by design")]
    let lo = (now_us / 1000) as u8;
    lo
}

/// Build an encoder whose packets flow back into `shared`.
fn build_encoder<P: Platform>(
    shared: &Weak<Shared<P>>,
    config: EncoderConfig,
) -> Result<P::Video, CodecError> {
    let weak = Weak::clone(shared);
    P::Video::new(config, move |packet| {
        if let Some(shared) = weak.upgrade() {
            shared.on_packet(&packet);
        }
    })
}

/// How a window target is served by ScreenCaptureKit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WindowPath {
    /// `SCContentFilter(desktopIndependentWindow:)`: the window alone, wherever it is.
    Filter,
    /// The display filter with `sourceRect` at the window's frame: the composited desktop
    /// cropped, which ScreenCaptureKit serves without the per-window pass.
    DisplayCrop,
}

/// Whether windows are served as a crop of their display when they are entirely on one and
/// nothing of another process overlaps them. `SLOPTY_WINDOW_CAPTURE=window|crop` overrides
/// it (the measurement and support knob).
const CROP_WINDOWS: bool = true;

fn crop_windows() -> bool {
    crop_windows_from(std::env::var("SLOPTY_WINDOW_CAPTURE").ok().as_deref())
}

/// [`crop_windows`] for the knob's value: an unknown or missing value is the default.
fn crop_windows_from(knob: Option<&str>) -> bool {
    match knob {
        Some("window") => false,
        Some("crop") => true,
        _other => CROP_WINDOWS,
    }
}

/// The rule of the display-crop path.
///
/// A window is served as a crop of its display only while it is on screen (not minimised,
/// not on another Space), entirely on one display (`crop` is the geometry's answer) and
/// nothing counts as covering it. Anything else is the window filter, which shows the
/// window and nothing but the window wherever it is.
#[must_use]
pub const fn crop_allowed(on_screen: bool, crop: Option<Crop>, occluded: bool) -> Option<Crop> {
    if on_screen && !occluded { crop } else { None }
}

/// The crop a window wants right now, or `None` when it must go through the window filter.
fn wanted_crop<P: Platform>(
    id: slopty_core::WindowId,
    state: &WindowState,
    point_scale: f64,
) -> Option<Crop> {
    let bounds = &state.bounds;
    let crop = Source::<P>::display_enclosing(bounds).and_then(|display| {
        let display = Source::<P>::display_bounds(display);
        crop_for(bounds, &display, point_scale).map(|(crop, _pixels)| crop)
    });
    let occluded = Source::<P>::occluded(id, bounds, state.owner_pid);
    crop_allowed(state.on_screen, crop, occluded)
}

/// What the window server says about a stream's target: everything [`ScreenStream::check_geometry`]
/// decides from, read off the runtime by [`ScreenStream::prober`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Probe {
    /// The target's bounds in points; `None` when a window is gone.
    bounds: Option<Rect>,
    /// When the reads began.
    at: Instant,
    /// For a window that may be served from a display crop: whether it is on screen, and the crop
    /// it could be served from (on one display, uncovered), before any suspicion.
    window: Option<(bool, Option<Crop>)>,
}

/// Read the target's geometry: one window-list description for the window's bounds, on-screen
/// state and owner, the occlusion list above it, and the display under it. Blocking; timed into
/// [`ScreenStats::bounds`], and the bounds are left where the cursor loop reads them.
fn probe<P: Platform>(
    target: CaptureTarget,
    point_scale: f64,
    crop: bool,
    shared: &Shared<P>,
) -> Probe {
    let started = now::<P>();
    let at = Instant::now();
    let probe = match target {
        CaptureTarget::Display(_) => {
            Probe { bounds: Source::<P>::target_bounds(target), at, window: None }
        }
        CaptureTarget::Window(id) => match Source::<P>::window_state(id) {
            None => Probe { bounds: None, at, window: None },
            Some(state) => Probe {
                bounds: Some(state.bounds),
                at,
                window: crop.then(|| (state.on_screen, wanted_crop::<P>(id, &state, point_scale))),
            },
        },
    };
    shared.counters.bounds.lock().push(now::<P>().saturating_sub(started));
    *shared.bounds.lock() = probe.bounds;
    probe
}

/// Resolve a target, choosing the path for a window.
fn resolve<P: Platform>(
    content: &Content<P>,
    target: CaptureTarget,
) -> Result<(Resolved<P>, WindowPath), ScreenError> {
    if let CaptureTarget::Window(id) = target
        && crop_windows()
        && let Some(bounds) = Source::<P>::window_bounds(id)
        && let Some(owner) = Source::<P>::window_owner(id)
        && let Some(candidate) = Source::<P>::resolve_crop(content, id)?
    {
        let on_screen = Source::<P>::window_on_screen(id);
        let occluded = Source::<P>::occluded(id, &bounds, owner);
        let crop = Source::<P>::crop(&candidate);
        if crop_allowed(on_screen, crop, occluded).is_some() {
            return Ok((candidate, WindowPath::DisplayCrop));
        }
        tracing::debug!(%id, on_screen, occluded, ?crop, "window filter");
    }
    Ok((Source::<P>::resolve(content, target)?, WindowPath::Filter))
}

/// What the stream is asking ScreenCaptureKit to become: a path and a whole configuration (size,
/// rate, crop) that are committed only once every asynchronous call for them has completed
/// without error. Shared with the completion callbacks.
#[derive(Debug)]
struct Transition {
    path: WindowPath,
    config: CaptureConfig,
    /// Completion callbacks still to come.
    outstanding: u8,
    failed: bool,
}

/// What [`Transitions::settle`] found.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Settled {
    /// Nothing in flight.
    Idle,
    /// Callbacks still to come: leave the stream alone this tick.
    Busy,
    /// Every call completed; the stream is now on this path with this configuration.
    Commit(WindowPath, CaptureConfig),
    /// A call failed: the stream is wherever it was; the next tick asks again.
    Failed(WindowPath, CaptureConfig),
}

/// The in-flight transition, if any, behind a lock the callbacks can take.
#[derive(Clone, Default, Debug)]
struct Transitions(Arc<Mutex<Option<Transition>>>);

impl Transitions {
    /// Start a transition that `calls` completion callbacks will finish. False (and nothing
    /// started) while one is still in flight.
    fn begin(&self, path: WindowPath, config: CaptureConfig, calls: u8) -> bool {
        let mut slot = self.0.lock();
        if slot.as_ref().is_some_and(|t| t.outstanding > 0) {
            return false;
        }
        *slot = Some(Transition { path, config, outstanding: calls, failed: false });
        true
    }

    /// One call completed.
    fn on_result(&self, ok: bool) {
        if let Some(t) = self.0.lock().as_mut() {
            t.outstanding = t.outstanding.saturating_sub(1);
            t.failed |= !ok;
        }
    }

    /// Take the outcome once every call has completed.
    fn settle(&self) -> Settled {
        let mut slot = self.0.lock();
        match slot.as_ref() {
            None => Settled::Idle,
            Some(t) if t.outstanding > 0 => Settled::Busy,
            Some(t) => {
                let outcome = if t.failed {
                    Settled::Failed(t.path, t.config)
                } else {
                    Settled::Commit(t.path, t.config)
                };
                *slot = None;
                outcome
            }
        }
    }
}

/// A new size the target has to hold for a tick before the stream is rebuilt for it.
///
/// A rebuild is a fresh encoder session and a keyframe, and a live drag of a window's corner
/// changes its size on every 100 ms geometry tick: rebuilt at once, that is ten IDRs a second
/// for as long as the drag lasts. Until the size holds, the capture keeps its old output size
/// and scales the window into it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct ResizeDebounce {
    candidate: Option<(u32, u32)>,
}

impl ResizeDebounce {
    /// The target measures `native` pixels while the stream is built for `current`: the size to
    /// rebuild for, once the same new size is seen on two ticks in a row.
    fn observe(&mut self, native: (u32, u32), current: (u32, u32)) -> Option<(u32, u32)> {
        if native == current {
            self.candidate = None;
            return None;
        }
        if self.candidate == Some(native) {
            self.candidate = None;
            return Some(native);
        }
        self.candidate = Some(native);
        None
    }
}

/// Surfaces in ScreenCaptureKit's pool.
///
/// The stream keeps the newest capture (`Shared::held`) so a still picture can be sent again,
/// and the encoder holds the one it is compressing for its ~7 ms. At 2 those two could be every
/// surface there is, and the next capture would wait for the encoder to let go. 3 leaves one
/// free for ScreenCaptureKit to render into whatever the other two are doing. Measured on the
/// window filter against 2 (MEASUREMENTS.md, "capture floor"): p50 0.49 ms against 0.46, p95
/// 1.48 against 2.35 — the same within the run-to-run spread the table records.
const QUEUE_DEPTH: u8 = 3;

/// Capture and encoder settings for a target at a requested quality.
fn configs(native: (u32, u32), quality: &Quality) -> (CaptureConfig, EncoderConfig) {
    let scale =
        if quality.scale.is_finite() { f64::from(quality.scale).clamp(0.05, 1.0) } else { 1.0 };
    let side = |px: u32| -> u32 {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped")]
        let v = (f64::from(px) * scale).round().clamp(2.0, 16_384.0) as u32;
        v.next_multiple_of(2)
    };
    let (width, height) = (side(native.0), side(native.1));
    let fps = quality.fps.clamp(1, 240);
    let format = PixelFormat::Nv12Full;
    let capture = CaptureConfig {
        width,
        height,
        fps,
        format,
        queue_depth: QUEUE_DEPTH,
        audio: true,
        crop: None,
    };
    let encoder = EncoderConfig {
        width,
        height,
        codec: quality.codec,
        fps,
        bitrate_bps: quality.bitrate_bps.max(100_000),
    };
    (capture, encoder)
}

/// One live stream on the platform this build serves.
pub type ScreenStream = Pipeline<Native>;

/// One live stream: capture → encode → packetize → the transport.
pub struct Pipeline<P: Platform> {
    id: StreamId,
    target: CaptureTarget,
    native: (u32, u32),
    capture: <Source<P> as CaptureSource>::Stream,
    shared: Arc<Shared<P>>,
    /// The configuration ScreenCaptureKit has committed (see `transitions`).
    capture_config: CaptureConfig,
    /// The configuration the stream wants: size, rate and crop together. Every change goes
    /// through here and reaches ScreenCaptureKit whole, through one transition at a time, so
    /// a size change never carries a crop older than the one a move just asked for.
    desired: CaptureConfig,
    /// The path the stream wants.
    desired_path: WindowPath,
    /// A new size waiting to hold for a tick before the stream is rebuilt for it.
    resize: ResizeDebounce,
    encoder_config: EncoderConfig,
    cursor: JoinHandle<()>,
    /// The task that reads the cursor's picture and reports a change.
    shape: JoinHandle<()>,
    beat: JoinHandle<()>,
    /// The task that sends the held capture when nothing newer comes.
    repair: JoinHandle<()>,
    /// The task that tops QUIC up from the audio lane.
    lane: JoinHandle<()>,
    /// The accessibility observer on the window's application, for a window target of a
    /// trusted worker; `None` for a display, an untrusted process or an application that would
    /// not be observed. Dropped with the stream.
    hide_watch: Option<<Source<P> as CaptureSource>::HideWatch>,
    /// Client input aimed at this stream, in its pixel coordinates.
    injector: P::Input,
    point_scale: f64,
    /// Last requested quality; re-applied when the target changes size.
    quality: Quality,
    /// What the client has been told about the source, and the frame history it follows.
    source: SourceTracker,
    /// The enumeration the target was resolved from; filters for a path switch come from it.
    content: Arc<Content<P>>,
    /// How a window is served right now (committed; see `transitions`).
    path: WindowPath,
    /// The path switch or crop move waiting for ScreenCaptureKit's completion callbacks.
    transitions: Transitions,
    /// What tells the connection the stream ended; the crop path calls it when its window
    /// closes, since a display stream does not stop by itself.
    on_stop: Arc<dyn Fn(CaptureError) + Send + Sync>,
    /// `on_stop` has been called.
    stopped: bool,
}

/// How long a stream may produce no frame at all before the client is told the target is idle.
///
/// Long enough that a normally starting stream never reports `Idle` first (first frame on
/// loopback is ~130 ms, MEASUREMENTS.md), short enough that a hidden window costs the receiver
/// only a couple of refresh requests before it stops asking.
pub const SOURCE_IDLE_AFTER: Duration = Duration::from_millis(400);

impl<P: Platform> std::fmt::Debug for Pipeline<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pipeline")
            .field("id", &self.id)
            .field("target", &self.target)
            .field("capture", &self.capture_config)
            .finish_non_exhaustive()
    }
}

impl<P: Platform> Pipeline<P> {
    /// Enumerate shareable content, reusing an enumeration younger than `SHAREABLE_TTL`.
    pub async fn shareable() -> Result<Arc<Content<P>>, ScreenError> {
        let cached = SHAREABLE.lock().as_ref().and_then(|(taken, content)| {
            (taken.elapsed() < SHAREABLE_TTL).then(|| Arc::clone(content))
        });
        if let Some(content) = cached.and_then(|c| c.downcast::<Content<P>>().ok()) {
            return Ok(content);
        }
        let (tx, rx) = oneshot::channel();
        Source::<P>::enumerate(move |result| {
            let _receiver_gone = tx.send(result);
        });
        let content = Arc::new(rx.await.map_err(|_dropped| ScreenError::Closed)??);
        let erased: Arc<dyn std::any::Any + Send + Sync> = Arc::<Content<P>>::clone(&content);
        *SHAREABLE.lock() = Some((Instant::now(), erased));
        Ok(content)
    }

    /// Start and stop one small capture of the first display; returns how long that took
    /// ([`warm_up`]).
    pub async fn warm_up() -> Result<Duration, ScreenError> {
        let started = Instant::now();
        let encoder = P::Video::new(
            EncoderConfig {
                width: 64,
                height: 64,
                codec: VideoCodec::Hevc,
                fps: 1,
                bitrate_bps: 100_000,
            },
            |_packet| {},
        )?;
        drop(encoder);
        let content = Self::shareable().await?;
        let display =
            Source::<P>::displays(&content).into_iter().next().ok_or(ScreenError::Closed)?;
        let resolved = Source::<P>::resolve(&content, CaptureTarget::Display(display.id))?;
        let config = CaptureConfig {
            width: 64,
            height: 64,
            fps: 1,
            format: PixelFormat::Nv12Full,
            queue_depth: 1,
            audio: true,
            crop: None,
        };
        let (tx, rx) = oneshot::channel();
        let capture = Source::<P>::start(
            &resolved,
            &config,
            |_frame| {},
            Some(Box::new(|_chunk| {})),
            |_stopped| {},
            move |result| {
                let _receiver_gone = tx.send(result);
            },
        )?;
        rx.await.map_err(|_dropped| ScreenError::Closed)??;
        let (tx, rx) = oneshot::channel();
        Source::<P>::stop(&capture, move |result| {
            let _receiver_gone = tx.send(result);
        });
        let _stopped = rx.await;
        Ok(started.elapsed())
    }

    /// The `Listing` event for the current windows and displays.
    pub async fn listing() -> Result<ScreenEvent, ScreenError> {
        let content = Self::shareable().await?;
        Ok(ScreenEvent::Listing {
            windows: Source::<P>::windows(&content),
            displays: Source::<P>::displays(&content),
        })
    }

    /// Set a worker window's size in points ([`resize_window`]).
    pub fn resize_window(
        window: slopty_core::WindowId,
        width: f64,
        height: f64,
    ) -> Result<(), ScreenError> {
        let pid = Source::<P>::window_owner(window).ok_or(ScreenError::WindowGone)?;
        let bounds = Source::<P>::window_bounds(window).ok_or(ScreenError::WindowGone)?;
        let title = Source::<P>::window_title(window);
        let target = TargetWindow { bounds, title };
        Ok(Source::<P>::resize_window(pid, &target, width, height)?)
    }
}

impl<P: Platform> Pipeline<P> {
    /// Resolve `target`, start capturing at `quality`, and return the `Opened` event to send.
    /// Datagrams go to `sink`; `on_event` hears [`StreamEvent::Stopped`] if ScreenCaptureKit
    /// ends the stream (window closed, permission revoked).
    pub async fn open(
        id: StreamId,
        target: CaptureTarget,
        quality: Quality,
        sink: Arc<dyn DatagramSink>,
        on_event: impl Fn(StreamEvent) + Send + Sync + 'static,
    ) -> Result<(Self, ScreenEvent), ScreenError> {
        let on_event: Arc<dyn Fn(StreamEvent) + Send + Sync> = Arc::new(on_event);
        let on_stop: Arc<dyn Fn(CaptureError) + Send + Sync> = {
            let on_event = Arc::clone(&on_event);
            Arc::new(move |e| on_event(StreamEvent::Stopped(e)))
        };
        let t0 = Instant::now();
        let content = Self::shareable().await?;
        let enumerated = t0.elapsed();
        let (resolved, path) = resolve::<P>(&content, target)?;
        let native = Source::<P>::pixel_size(&resolved);
        let (mut capture_config, encoder_config) = configs(native, &quality);
        capture_config.crop = Source::<P>::crop(&resolved);

        let shared = Arc::new(Shared::new(
            id,
            sink,
            encoder_config.bitrate_bps,
            capture_config.fps,
            path == WindowPath::DisplayCrop,
        ));
        let zoom = f64::from(capture_config.width) / f64::from(native.0);
        shared.zoom.store(zoom.to_bits(), Ordering::Relaxed);
        let t_encoder = Instant::now();
        let encoder = build_encoder(&Arc::downgrade(&shared), encoder_config)?;
        let encoder_built = t_encoder.elapsed();
        shared.install(encoder);
        let start = shared.rate.lock().target_bps();
        shared.apply_bitrate(start);
        shared.apply_cadence(start);

        let (started_tx, started_rx) = oneshot::channel();
        let sink = Arc::clone(&shared);
        let audio_sink = Arc::clone(&shared);
        let capture = Source::<P>::start(
            &resolved,
            &capture_config,
            move |frame| sink.on_frame(frame),
            Some(Box::new(move |chunk| audio_sink.on_audio(&chunk))),
            {
                let on_stop = Arc::clone(&on_stop);
                move |e| on_stop(e)
            },
            move |result| {
                let _receiver_gone = started_tx.send(result);
            },
        )?;
        started_rx.await.map_err(|_dropped| ScreenError::Closed)??;
        tracing::debug!(
            stream = %id,
            enumerate_ms = enumerated.as_millis(),
            encoder_ms = encoder_built.as_millis(),
            total_ms = t0.elapsed().as_millis(),
            ?path,
            crop = ?capture_config.crop,
            "capture started"
        );

        let point_scale = f64::from(Source::<P>::point_scale(&resolved));
        let injector = P::Input::new(target, point_scale * zoom);
        // Two tasks, not one. The beat is a promise about time and must never be behind work
        // that takes any: the pointer read in the cursor loop is a window-server round trip, and
        // those have been measured at 90 ms, three beats' worth.
        let beat = tokio::spawn(beat_loop(Arc::clone(&shared)));
        let cursor =
            tokio::spawn(cursor_loop(Arc::clone(&shared), point_scale, injector.pointer()));
        let repair = tokio::spawn(repair_loop(Arc::clone(&shared)));
        let lane = tokio::spawn(lane_loop(Arc::clone(&shared)));
        #[expect(clippy::cast_possible_truncation, reason = "a display scale is 1 to 4")]
        #[expect(clippy::cast_sign_loss, reason = "a display scale is positive")]
        let backing = point_scale.round().clamp(1.0, 4.0) as u8;
        let shape = tokio::spawn(shape_loop(Arc::clone(&shared), backing, Arc::clone(&on_event)));
        let hide_watch = hide_watch_for::<P>(id, target, &shared).await;
        #[expect(clippy::cast_possible_truncation, reason = "a small ratio")]
        let scale = (point_scale * zoom) as f32;
        let opened = ScreenEvent::Opened {
            stream: id,
            target,
            codec: encoder_config.codec,
            width: capture_config.width,
            height: capture_config.height,
            scale,
        };
        let stream = Self {
            id,
            target,
            native,
            capture,
            shared,
            capture_config,
            desired: capture_config,
            desired_path: path,
            resize: ResizeDebounce::default(),
            encoder_config,
            cursor,
            shape,
            beat,
            repair,
            lane,
            hide_watch,
            injector,
            point_scale,
            quality,
            source: SourceTracker::new(Instant::now()),
            content,
            path,
            transitions: Transitions::default(),
            on_stop,
            stopped: false,
        };
        Ok((stream, opened))
    }

    /// Stream id.
    #[must_use]
    pub const fn id(&self) -> StreamId {
        self.id
    }

    /// What is being streamed.
    #[must_use]
    pub const fn target(&self) -> CaptureTarget {
        self.target
    }

    /// How a window is served right now (`Filter` for a display target).
    #[must_use]
    pub const fn path(&self) -> WindowPath {
        self.path
    }

    /// Counters.
    #[must_use]
    pub fn stats(&self) -> ScreenStats {
        self.shared.stats()
    }

    /// What the client's feedback reaches without going through this stream's owner.
    #[must_use]
    pub fn control(&self) -> StreamControl<P> {
        StreamControl(Arc::clone(&self.shared))
    }

    /// A handle on the counters for the daemon's registry.
    #[must_use]
    pub fn stats_handle(&self) -> StatsHandle {
        StatsHandle(Arc::<Shared<P>>::clone(&self.shared))
    }

    /// Change quality. A size, rate or codec change rebuilds the encoder and reconfigures the
    /// capture; a bitrate-only change is applied in place, cadence included.
    pub fn set_quality(&mut self, quality: &Quality) -> Result<(), ScreenError> {
        self.quality = *quality;
        let (capture_config, encoder_config) = configs(self.native, quality);
        let desired = CaptureConfig { crop: self.desired.crop, ..capture_config };
        if desired == self.desired && encoder_config.codec == self.encoder_config.codec {
            if encoder_config.bitrate_bps != self.encoder_config.bitrate_bps {
                // The client moved its ceiling; the controller keeps its place under it, and the
                // rung follows the target the way a rate decision moves it.
                let target = {
                    let mut rate = self.shared.rate.lock();
                    rate.set_max(encoder_config.bitrate_bps);
                    rate.target_bps()
                };
                self.shared.apply_bitrate(target);
                self.shared.apply_cadence(target);
                self.encoder_config = encoder_config;
            }
            return Ok(());
        }
        self.reconfigure(desired, encoder_config)
    }

    /// Follow the target: a window the user resized on the worker gets a stream of its new
    /// size (fresh encoder, keyframe) once the size has held for a tick, and the client hears
    /// `Geometry`; one on the display-crop path that moved gets its crop moved, and one that went
    /// under another window (or off its display) falls back to the window filter until it is
    /// clear again. Decided from `probe`, which [`Self::prober`] read off the runtime; nothing
    /// here waits on the window server. Call it a few times a second.
    ///
    /// The probe's bounds are also what the input sink maps the pointer through from here on,
    /// so input never reads them in front of an event.
    pub fn check_geometry(&mut self, probe: &Probe) -> Result<Option<ScreenEvent>, ScreenError> {
        self.injector.set_bounds(probe.bounds, probe.at);
        let Some(rect) = probe.bounds else {
            self.window_gone();
            return Ok(None);
        };
        self.settle();
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped")]
        let px = |points: f64| (points * self.point_scale).round().clamp(2.0, 16_384.0) as u32;
        let native = (px(rect.w), px(rect.h));
        if let (CaptureTarget::Window(_), Some((on_screen, crop))) = (self.target, probe.window) {
            self.follow_window(on_screen, crop);
        }
        let Some(native) = self.resize.observe(native, self.native) else {
            self.apply_desired();
            return Ok(None);
        };
        tracing::info!(stream = %self.id, from = ?self.native, to = ?native, "target resized");
        self.native = native;
        let (capture_config, encoder_config) = configs(native, &self.quality);
        let desired = CaptureConfig { crop: self.desired.crop, ..capture_config };
        self.reconfigure(desired, encoder_config)?;
        Ok(Some(ScreenEvent::Geometry {
            stream: self.id,
            width: self.desired.width,
            height: self.desired.height,
        }))
    }

    /// The window-server reads [`Self::check_geometry`] decides from, as a call to make off the
    /// runtime: one window-list description, the occlusion list and a display lookup, 0.2–0.6 ms
    /// (MEASUREMENTS.md, "the geometry tick off the connection").
    pub fn prober(&self) -> impl FnOnce() -> Probe + Send + 'static {
        let (target, point_scale, shared) =
            (self.target, self.point_scale, Arc::clone(&self.shared));
        let crop = crop_windows();
        move || probe::<P>(target, point_scale, crop, &shared)
    }

    /// Whether the target has drawn anything, when that answer changed since the client was
    /// last told. Polled beside [`Self::check_geometry`].
    ///
    /// A window that is hidden, minimised or has not drawn yields no frames, and the receiver
    /// cannot tell that from a stream whose frames are all being lost: it sits in "need refresh"
    /// and asks again every backoff period, forever, for something no refresh can produce
    /// (MEASUREMENTS.md, "a target that never produces a frame"). Saying so once stops it.
    pub fn check_source(&mut self) -> Option<ScreenEvent> {
        let encoded = self.shared.counters.encoded.load(Ordering::Relaxed);
        let hidden = self.shared.target_hidden.load(Ordering::Relaxed);
        let state = self.source.poll(encoded, hidden, Instant::now())?;
        tracing::debug!(stream = %self.id, ?state, "capture source state");
        Some(ScreenEvent::Source { stream: self.id, state })
    }

    /// The window list no longer knows the target. Through the window filter ScreenCaptureKit
    /// stops the stream itself and `on_stop` fires from its delegate; a display crop would keep
    /// streaming the desktop where the window was, so the stream stops itself and says so
    /// the same way, once.
    fn window_gone(&mut self) {
        if self.stopped || !matches!(self.target, CaptureTarget::Window(_)) {
            return;
        }
        if self.path != WindowPath::DisplayCrop {
            return;
        }
        // Nothing more goes out while the stop is in flight: the crop outlives the window.
        self.shared.target_hidden.store(true, Ordering::Relaxed);
        self.stopped = true;
        tracing::info!(stream = %self.id, "window closed under a display crop: stopping");
        let id = self.id;
        Source::<P>::stop(&self.capture, move |result| {
            if let Err(e) = result {
                tracing::debug!(stream = %id, error = %e, "capture stop");
            }
        });
        (self.on_stop)(CaptureError::Stopped("window closed".to_owned()));
    }

    /// Take the outcome of the transition in flight, if it has completed: the path and
    /// configuration it carried become the stream's, or, if a call failed, nothing changes and
    /// [`Self::apply_desired`] asks again.
    fn settle(&mut self) {
        match self.transitions.settle() {
            Settled::Busy | Settled::Idle => {}
            Settled::Commit(path, config) => {
                self.path = path;
                self.capture_config = config;
                self.shared.cropped.store(path == WindowPath::DisplayCrop, Ordering::Relaxed);
                tracing::debug!(stream = %self.id, ?path, crop = ?config.crop, "capture path settled");
            }
            Settled::Failed(path, config) => {
                tracing::warn!(stream = %self.id, ?path, crop = ?config.crop, "capture change failed; retrying");
            }
        }
    }

    /// Decide the path a window wants: crop where it is now, or the window filter while
    /// something covers it. On-screen state and occlusion come from every probe, since other
    /// windows move too; `available` is the crop the probe found, before any suspicion. Only the
    /// wish is recorded here; [`Self::apply_desired`] asks ScreenCaptureKit for it.
    fn follow_window(&mut self, on_screen: bool, available: Option<Crop>) {
        // First, and outside everything below: the guard must not depend on the transition state
        // machine. A swap that ScreenCaptureKit keeps rejecting leaves a transition busy for as
        // long as it keeps failing, and those are exactly the ticks where the crop is still
        // running over a window that is no longer there.
        if self.shared.target_hidden.swap(!on_screen, Ordering::Relaxed) == on_screen {
            tracing::info!(stream = %self.id, on_screen, "target visibility");
        }
        // Keyed on the target, not on the path this side believes it is on: a `retarget` the
        // framework rejects leaves the crop running while the bookkeeping says window filter,
        // and every frame it delivers is then a picture of the desktop.
        // A suspicion is the other reason the crop is not allowed: the accessibility API has
        // said a window of this application went, and for the hold the window list cannot be
        // trusted to say which. Frames are held for the hold either way (`Shared::on_frame`);
        // the swap is what wakes ScreenCaptureKit, which stops delivering for an
        // application-scoped display filter once a window of that application is ordered out
        // and is woken by no re-application of the same kind of filter, only by a change of
        // kind (MEASUREMENTS.md, "a sibling window closing stalls the crop"). A false suspicion
        // comes back to the crop on the first tick after the hold.
        let suspected = self.shared.suspected_at(now::<P>());
        let mut wanted = if suspected { None } else { available };
        // Another window of the application went (`Shared::sibling_went`): no suspicion, but
        // a crop that keeps its filter is a crop that never gets another frame. One tick on
        // the window filter is the change of kind that wakes the framework; the crop is
        // wanted again on the next. Any other path change is a change of kind as well and
        // clears the flag by itself.
        let stalled = self.shared.filter_stalled.load(Ordering::Relaxed);
        if stalled && wanted.is_some() && self.path == WindowPath::DisplayCrop {
            wanted = None;
        }
        self.desired_path =
            if wanted.is_some() { WindowPath::DisplayCrop } else { WindowPath::Filter };
        self.desired.crop = wanted;
    }

    /// Ask ScreenCaptureKit for the desired path and configuration, whole, unless a transition
    /// is still in flight; the next tick asks again for whatever is still different.
    ///
    /// Order matters between the two calls of a path change: the crop is cleared before the swap
    /// to the window filter, since a `sourceRect` on a window stream would be read in the
    /// window's own space, and the display filter is in place before the crop is set on it.
    fn apply_desired(&self) {
        if self.stopped {
            return;
        }
        let (path, config) = (self.desired_path, self.desired);
        if path == self.path && config == self.capture_config {
            return;
        }
        if path == self.path {
            // A move, a size or a rate: one configuration update carries all of it.
            if self.transitions.begin(path, config, 1) {
                self.update_capture(config);
            }
            return;
        }
        let CaptureTarget::Window(id) = self.target else { return };
        let target = match path {
            WindowPath::Filter => Source::<P>::resolve(&self.content, self.target).map(Some),
            WindowPath::DisplayCrop => Source::<P>::resolve_crop(&self.content, id),
        };
        let target = match target {
            Ok(Some(target)) => target,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(stream = %self.id, ?path, error = %e, "capture path");
                return;
            }
        };
        if !self.transitions.begin(path, config, 2) {
            return;
        }
        let waking = self.shared.filter_stalled.swap(false, Ordering::Relaxed);
        match path {
            WindowPath::Filter => {
                if waking {
                    tracing::info!(stream = %self.id, "another window of the application went: waking the capture through the window filter");
                } else {
                    tracing::info!(stream = %self.id, "window covered, hidden, off its display or suspected: window filter");
                }
                self.update_capture(config);
                self.retarget(&target);
            }
            WindowPath::DisplayCrop => {
                tracing::info!(stream = %self.id, crop = ?config.crop, "window clear: display crop");
                self.retarget(&target);
                self.update_capture(config);
            }
        }
    }

    /// Push `config` to the live stream; the completion lands in the transition.
    fn update_capture(&self, config: CaptureConfig) {
        let id = self.id;
        let transitions = self.transitions.clone();
        Source::<P>::update(&self.capture, &config, move |result| {
            if let Err(e) = &result {
                tracing::warn!(stream = %id, error = %e, "capture update failed");
            }
            transitions.on_result(result.is_ok());
        });
    }

    /// Swap the live stream's filter; the completion lands in the transition.
    fn retarget(&self, target: &Resolved<P>) {
        let id = self.id;
        let transitions = self.transitions.clone();
        Source::<P>::retarget(&self.capture, target, move |result| {
            if let Err(e) = &result {
                tracing::warn!(stream = %id, error = %e, "capture retarget failed");
            }
            transitions.on_result(result.is_ok());
        });
    }

    /// Rebuild the encoder for a new size, rate or codec and ask the capture for `desired`.
    /// The new session's long-term references start from nothing ([`Shared::rebuilt`]).
    fn reconfigure(
        &mut self,
        desired: CaptureConfig,
        encoder_config: EncoderConfig,
    ) -> Result<(), ScreenError> {
        let weak = Arc::downgrade(&self.shared);
        let encoder = build_encoder(&weak, encoder_config)?;
        self.shared.install(encoder);
        let target = {
            let mut rate = self.shared.rate.lock();
            rate.set_max(encoder_config.bitrate_bps);
            rate.target_bps()
        };
        self.shared.apply_bitrate(target);
        // A new quality sets a new ceiling, and the ladder starts from it again: the rung that was
        // in force answered a bitrate the client has just replaced.
        self.shared.fps_ceiling.store(desired.fps, Ordering::Relaxed);
        self.shared.fps.store(desired.fps, Ordering::Relaxed);
        self.shared.apply_cadence(target);
        let zoom = f64::from(desired.width) / f64::from(self.native.0);
        self.shared.zoom.store(zoom.to_bits(), Ordering::Relaxed);
        self.injector.set_scale(self.point_scale * zoom);
        self.desired = desired;
        self.encoder_config = encoder_config;
        self.apply_desired();
        Ok(())
    }

    /// Stream pixels per native pixel of the target: the quality's scale as the stream was last
    /// built for it. A client position in stream pixels over `zoom × point_scale` is a
    /// position in the target's points.
    #[must_use]
    pub fn zoom(&self) -> f64 {
        self.shared.zoom()
    }

    /// Deliver client input to the streamed window or display.
    pub fn inject(&mut self, input: &ScreenInput) -> Result<(), ScreenError> {
        Ok(self.injector.inject(input)?)
    }

    /// Give the streamed window's application keyboard focus on the worker.
    pub fn focus(&mut self) -> Result<(), ScreenError> {
        Ok(self.injector.focus()?)
    }

    /// Let go of every key and button the client holds down on the worker: the stream is
    /// ending. Returns at once, so call it ahead of [`Self::close`], which waits on
    /// ScreenCaptureKit's stop before it drops the input sink.
    pub fn release_input(&mut self) {
        self.injector.release_all();
    }

    /// What a client's resize to `(width, height)` native pixels asks of the worker: the window
    /// and the size in points, for [`resize_window`] off the runtime. A display stream asks
    /// nothing.
    #[must_use]
    pub fn resize_points(
        &self,
        width: u32,
        height: u32,
    ) -> Option<(slopty_core::WindowId, f64, f64)> {
        let CaptureTarget::Window(id) = self.target else { return None };
        if self.point_scale <= 0.0 || width == 0 || height == 0 {
            return None;
        }
        Some((id, f64::from(width) / self.point_scale, f64::from(height) / self.point_scale))
    }

    /// Fold in a receiver report: LTR acks feed the encoder, loss feeds the parity ratio, and
    /// loss, queueing, stalls and the QUIC path (`path`) drive the bitrate. Returns the
    /// controller's decision when this report completed a decision window.
    pub fn report(&self, report: &ReceiverReport, path: Option<PathSample>) -> Option<Decision> {
        self.shared.report(report, path)
    }

    /// The bitrate the controller is asking the encoder for right now.
    #[must_use]
    pub fn bitrate_bps(&self) -> u32 {
        self.shared.rate.lock().target_bps()
    }

    /// The client lost a frame it cannot recover: make the next frame stand on its own, and a
    /// keyframe when `keyframe` says the client holds no reference to predict from.
    pub fn request_refresh(&self, last_good_frame: u32, keyframe: bool) {
        self.shared.request_refresh(last_good_frame, keyframe);
    }

    /// Retransmit fragments of a recent frame, unless QUIC is already holding more than the
    /// frame budget: an answer that leaves behind seconds of queued frames arrives after the
    /// receiver has given up, and on a collapsed link every NACK answered that way stacked
    /// another copy of the frame into the queue (64 000 datagrams for 12 frames,
    /// MEASUREMENTS.md "start-up over the mesh").
    pub fn nack(&self, frame: u32, fragments: &[u16]) {
        self.shared.nack(frame, fragments);
    }

    /// Stop capturing and tear down.
    pub async fn close(mut self) {
        self.cursor.abort();
        self.shape.abort();
        self.beat.abort();
        self.repair.abort();
        self.lane.abort();
        drop(self.hide_watch.take());
        let (tx, rx) = oneshot::channel();
        Source::<P>::stop(&self.capture, move |result| {
            let _receiver_gone = tx.send(result);
        });
        if let Ok(Err(e)) = rx.await {
            tracing::debug!(stream = %self.id, error = %e, "capture stop");
        }
        let stats = self.stats();
        tracing::info!(stream = %self.id, ?stats, "screen stream closed");
    }
}

/// Set a worker window's size in points through the accessibility API.
///
/// The window is matched by the frame and title the window list gives it (the API has no
/// window number; see `slopty_capture::ax`). Blocking: call it off the runtime. The stream's
/// geometry poll sees the new frame within its period and tells the client with a `Geometry`
/// event.
pub fn resize_window(
    window: slopty_core::WindowId,
    width: f64,
    height: f64,
) -> Result<(), ScreenError> {
    ScreenStream::resize_window(window, width, height)
}

/// Put a window target's application under the accessibility watch, off the runtime (the
/// registration is a few window-server round trips). A display has no application to watch;
/// a worker that is not trusted for accessibility, or an application that will not be observed,
/// gets the window-list check alone, and says so once.
async fn hide_watch_for<P: Platform>(
    id: StreamId,
    target: CaptureTarget,
    shared: &Arc<Shared<P>>,
) -> Option<<Source<P> as CaptureSource>::HideWatch> {
    let CaptureTarget::Window(window) = target else {
        return None;
    };
    let weak = Arc::downgrade(shared);
    let started = tokio::task::spawn_blocking(move || {
        let pid = Source::<P>::window_owner(window)?;
        let bounds = Source::<P>::window_bounds(window)?;
        let title = Source::<P>::window_title(window);
        let target = TargetWindow { bounds, title };
        Some(Source::<P>::watch_hides(pid, target, move |went| {
            if let Some(shared) = weak.upgrade() {
                match went {
                    Went::Target => shared.suspect(now::<P>()),
                    Went::Other => shared.sibling_went(),
                }
            }
        }))
    })
    .await;
    match started {
        Ok(Some(Ok(watch))) => {
            let targeted = Source::<P>::watch_targeted(&watch);
            tracing::debug!(stream = %id, targeted, "accessibility hide watch on");
            Some(watch)
        }
        Ok(Some(Err(e))) => {
            tracing::info!(stream = %id, error = %e, "no accessibility hide watch");
            None
        }
        Ok(None) => {
            tracing::info!(stream = %id, "no accessibility hide watch: window owner unknown");
            None
        }
        Err(_panicked) => None,
    }
}

/// Send a heartbeat whenever nothing at all left for [`HEARTBEAT_AFTER`] (a quiet source must
/// not read as a stalled link). Sleeps until the moment one would be due rather than polling:
/// while video flows every datagram moves that moment on, so the task wakes at most once per
/// [`HEARTBEAT_AFTER`]. Runs until the task is aborted by [`ScreenStream::close`] or the
/// connection is gone.
async fn beat_loop<P: Platform>(shared: Arc<Shared<P>>) {
    let mut beats: u32 = 0;
    let mut last_beat_us: Option<u64> = None;
    let heartbeat_after_us = u64::try_from(HEARTBEAT_AFTER.as_micros()).unwrap_or(u64::MAX);
    // Four times the promise, which is twice the gap the receiver already calls a stall: past
    // this the beat has not merely slipped, it has failed at the one thing it is for.
    let late_beat_us = heartbeat_after_us.saturating_mul(4);
    while !shared.sink.is_closed() {
        let now = now::<P>();
        // The beat itself counts as traffic even when the transport refused it, so a refusal is
        // retried a period later instead of spun on.
        let last_out = shared.last_push_us.load(Ordering::Relaxed).max(last_beat_us.unwrap_or(0));
        if let Some(wait) = beat_due_in(now.saturating_sub(last_out), heartbeat_after_us) {
            tokio::time::sleep(wait).await;
            continue;
        }
        beats = beats.wrapping_add(1);
        tracing::trace!(stream = %shared.id, beats, "heartbeat");
        shared.counters.heartbeats.fetch_add(1, Ordering::Relaxed);
        if let Some(previous) = last_beat_us {
            let gap = now.saturating_sub(previous);
            shared.counters.beat_gap.lock().push(gap);
            shared.counters.beat_gap_worst_us.fetch_max(gap, Ordering::Relaxed);
            if gap >= late_beat_us {
                tracing::info!(stream = %shared.id, gap_us = gap, beats, "late heartbeat");
            }
        }
        last_beat_us = Some(now);
        shared.send(&[heartbeat_datagram(shared.id, beats, send_ms_lo(now))]);
    }
}

/// Send the held capture when nothing newer will: the last frame of a picture that went still,
/// skipped by the cadence, the guard or a deferral, and a refresh or keyframe asked for while the
/// picture stays still. Sleeps until the moment one is due ([`repair_at`]) and waits on
/// [`Shared::repair`] while nothing is owed, so a stream whose captures all go straight out
/// never wakes it for them.
async fn repair_loop<P: Platform>(shared: Arc<Shared<P>>) {
    while !shared.sink.is_closed() {
        let Some(at) = shared.repair_at() else {
            shared.repair.notified().await;
            continue;
        };
        let now = now::<P>();
        if at > now {
            // A capture or a request arriving meanwhile moves the moment; look again then.
            let wait = Duration::from_micros(at.saturating_sub(now));
            let _woken = tokio::time::timeout(wait, shared.repair.notified()).await;
            continue;
        }
        let period = period_us(shared.fps.load(Ordering::Relaxed));
        let wait = match shared.repair_now(now) {
            Attempt::Sent | Attempt::Nothing => continue,
            // A keyframe put off for a refresh waits for the cadence like any frame.
            Attempt::NotDue => {
                let due = shared
                    .last_encoded_us
                    .load(Ordering::Relaxed)
                    .saturating_add(period.saturating_sub(period / 8));
                due.saturating_sub(now).max(1_000)
            }
            // The link or the encoder is not taking it; ask again a period on, not on a spin.
            Attempt::NoRoom | Attempt::Failed => period,
        };
        let _woken =
            tokio::time::timeout(Duration::from_micros(wait), shared.repair.notified()).await;
    }
}

/// Top QUIC up from the audio lane every [`LANE_TICK`] while video waits in it; wait on
/// [`Shared::lane_wake`] while it is empty.
async fn lane_loop<P: Platform>(shared: Arc<Shared<P>>) {
    while !shared.sink.is_closed() {
        if shared.lane.lock().queue.is_empty() {
            shared.lane_wake.notified().await;
            continue;
        }
        tokio::time::sleep(LANE_TICK).await;
        let mut lane = shared.lane.lock();
        shared.pump(&mut lane, now::<P>());
    }
}

/// How long until a heartbeat is due after `silence_us` of nothing sent; `None` when it is due.
const fn beat_due_in(silence_us: u64, after_us: u64) -> Option<Duration> {
    if silence_us >= after_us {
        None
    } else {
        Some(Duration::from_micros(after_us.saturating_sub(silence_us)))
    }
}

/// Where the pointer is over the target, sent when it moves.
///
/// A window stream's input goes to the window's application and leaves the worker's pointer
/// wherever the worker's own user left it, so its pointer is where the input last put it
/// (`input`), and hidden before the first event. A display stream's input moves the real pointer,
/// which is read. A still pointer costs one read of the event system's move counters a tick
/// ([`CaptureSource::pointer_moves`], tens of nanoseconds): the pointer itself is only asked
/// for when the counters moved, and the target's bounds (which move a still pointer across the
/// picture) are the ones the owner's geometry probe last read. The pointer read is a
/// window-server round trip, so it runs on the blocking pool: what this loop must not do is
/// occupy a runtime worker, because [`beat_loop`] needs one on time (MEASUREMENTS.md, "the beat
/// behind the geometry call").
async fn cursor_loop<P: Platform>(shared: Arc<Shared<P>>, point_scale: f64, input: PointerWatch) {
    let mut ticks = tokio::time::interval(CURSOR_PERIOD);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut pointer: Option<(u32, (f64, f64))> = None;
    let mut last: Option<(i32, i32, bool)> = None;
    let mut seq: u32 = 0;
    while !shared.sink.is_closed() {
        ticks.tick().await;
        let Some(rect) = *shared.bounds.lock() else { continue };
        // Read every sample: a quality change rescales the stream under a still pointer.
        let pixels_per_point = point_scale * shared.zoom();
        let sample = if let Some(placed) = placed_sample(&input, rect, pixels_per_point) {
            placed
        } else {
            let moves = Source::<P>::pointer_moves();
            let at = match pointer {
                Some((seen, at)) if seen == moves => at,
                _moved_or_unread => {
                    let Ok(at) = tokio::task::spawn_blocking(Source::<P>::pointer_location).await
                    else {
                        return;
                    };
                    pointer = Some((moves, at));
                    at
                }
            };
            cursor_sample(rect, at, pixels_per_point)
        };
        shared.pointer_over.store(sample.2, Ordering::Relaxed);
        if last == Some(sample) {
            continue;
        }
        last = Some(sample);
        seq = seq.wrapping_add(1);
        let datagram =
            cursor_datagram(shared.id, seq, send_ms_lo(now::<P>()), sample.0, sample.1, sample.2);
        shared.send(&[datagram]);
    }
}

/// The pointer at `at` (global points) as a cursor sample for a target with bounds `rect`: its
/// place in stream pixels at `pixels_per_point`, and whether it is over the target.
fn cursor_sample(rect: Rect, at: (f64, f64), pixels_per_point: f64) -> (i32, i32, bool) {
    let to_pixels = |v: f64| -> i32 {
        #[expect(clippy::cast_possible_truncation, reason = "clamped")]
        let p = (v * pixels_per_point).round().clamp(-1.0e6, 1.0e6) as i32;
        p
    };
    (to_pixels(at.0 - rect.x), to_pixels(at.1 - rect.y), rect.contains(at.0, at.1))
}

/// The cursor sample of a stream whose input leaves the worker's pointer alone: where the input
/// last put it, hidden before the first event. `None` when the input moves the real pointer,
/// which is the one to read.
fn placed_sample(
    input: &PointerWatch,
    rect: Rect,
    pixels_per_point: f64,
) -> Option<(i32, i32, bool)> {
    match input.get() {
        Pointer::Real => None,
        Pointer::Placed(None) => Some((0, 0, false)),
        Pointer::Placed(Some(at)) => Some(cursor_sample(rect, at, pixels_per_point)),
    }
}

/// Read the cursor's picture at `SHAPE_PERIOD` while the pointer is over the target, and
/// report each change through `on_event`. Its own task: the first read in a process takes
/// seconds (`slopty_capture::warm_cursor` pays that at start-up), and a read must never
/// hold the position loop.
async fn shape_loop<P: Platform>(
    shared: Arc<Shared<P>>,
    backing: u8,
    on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
) {
    let mut ticks = tokio::time::interval(SHAPE_PERIOD);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut sent = ShapeDedup::default();
    while !shared.sink.is_closed() {
        ticks.tick().await;
        if !shared.pointer_over.load(Ordering::Relaxed) {
            continue;
        }
        let Ok(read) =
            tokio::task::spawn_blocking(move || Source::<P>::cursor_shape(backing)).await
        else {
            return;
        };
        if let Some(shape) = sent.observe(read) {
            on_event(StreamEvent::Cursor(shape));
        }
    }
}

/// Which cursor picture the client was last told, so one goes out only when it changed.
///
/// A read that says nothing (the cursor hidden, or the system not answering) changes
/// nothing: the client keeps the last picture, and the position channel says whether to
/// draw it.
#[derive(Debug, Default)]
pub struct ShapeDedup {
    last: Option<CursorShape>,
}

impl ShapeDedup {
    /// The picture to send for this reading, if any.
    pub fn observe(&mut self, read: Option<CursorShape>) -> Option<CursorShape> {
        let shape = read?;
        if self.last.as_ref() == Some(&shape) {
            return None;
        }
        self.last = Some(shape.clone());
        Some(shape)
    }
}

#[cfg(test)]
mod shape_tests {
    use super::*;

    fn shape(px: u8) -> CursorShape {
        CursorShape { w: 1, h: 1, hot_x: 0, hot_y: 0, bgra: vec![px, px, px, 255], scale: 2 }
    }

    #[test]
    fn a_picture_goes_out_once_per_change_and_a_blank_read_changes_nothing() {
        let mut dedup = ShapeDedup::default();
        assert_eq!(dedup.observe(None), None, "nothing read yet, nothing to send");
        assert_eq!(dedup.observe(Some(shape(1))), Some(shape(1)), "the first picture");
        assert_eq!(dedup.observe(Some(shape(1))), None, "the same again is not news");
        assert_eq!(dedup.observe(None), None, "a hidden or unreadable cursor keeps the last");
        assert_eq!(dedup.observe(Some(shape(1))), None, "still the one the client has");
        assert_eq!(dedup.observe(Some(shape(2))), Some(shape(2)), "a new picture");
    }

    /// A window stream's pointer is where its input put it, since that input leaves the
    /// worker's own pointer alone: hidden before the first event, then in the stream's pixels
    /// over the target's bounds. A display stream's input moves the real pointer, so its sample
    /// comes from reading that one.
    #[test]
    fn a_window_streams_cursor_is_where_its_input_put_the_pointer() {
        let rect = Rect { x: 100.0, y: 50.0, w: 800.0, h: 600.0 };
        let input = PointerWatch::default();
        assert_eq!(placed_sample(&input, rect, 1.0), Some((0, 0, false)), "none yet: hidden");
        input.place(110.0, 70.0);
        assert_eq!(placed_sample(&input, rect, 2.0), Some((20, 40, true)));
        input.place(900.0, 70.0);
        assert_eq!(
            placed_sample(&input, rect, 2.0),
            Some((1600, 40, false)),
            "its far edge is outside"
        );
        input.follow_real();
        assert_eq!(placed_sample(&input, rect, 2.0), None, "the real pointer is read");
    }
}

#[cfg(test)]
mod tests {
    use slopty_codec::audio::{CHANNELS, FRAME_SAMPLES};

    use super::*;

    const CROP: Crop = Crop { x: 10.0, y: 20.0, w: 300.0, h: 200.0 };

    /// A transport that keeps what it is sent: it holds what a test says it holds, carries
    /// datagrams up to `max` bytes, and can be closed.
    struct Wire {
        sent: Mutex<Vec<Bytes>>,
        calls: AtomicU64,
        held: std::sync::atomic::AtomicUsize,
        max: std::sync::atomic::AtomicUsize,
        closed: AtomicBool,
    }

    impl Wire {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                sent: Mutex::new(Vec::new()),
                calls: AtomicU64::new(0),
                held: std::sync::atomic::AtomicUsize::new(0),
                max: std::sync::atomic::AtomicUsize::new(MAX_DATAGRAM),
                closed: AtomicBool::new(false),
            })
        }

        fn drain(&self) -> Vec<Bytes> {
            std::mem::take(&mut *self.sent.lock())
        }
    }

    impl DatagramSink for Wire {
        fn send(&self, datagrams: &[Bytes]) -> Result<(), Refused> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.closed.load(Ordering::Relaxed) {
                return Err(Refused::Closed);
            }
            let max = self.max.load(Ordering::Relaxed);
            if datagrams.iter().any(|d| d.len() > max) {
                return Err(Refused::TooLarge);
            }
            self.sent.lock().extend_from_slice(datagrams);
            Ok(())
        }

        fn max_size(&self) -> Option<usize> {
            Some(self.max.load(Ordering::Relaxed))
        }

        fn held(&self) -> usize {
            self.held.load(Ordering::Relaxed)
        }

        fn cwnd(&self) -> u64 {
            0
        }

        fn is_closed(&self) -> bool {
            self.closed.load(Ordering::Relaxed)
        }
    }

    /// A stream's shared state with no encoder, sending to a [`Wire`]: enough to drive
    /// [`Shared::on_frame`] and read what it counted and sent.
    fn shared_for_frames() -> (Arc<Shared>, Arc<Wire>) {
        let wire = Wire::new();
        let sink: Arc<dyn DatagramSink> = Arc::<Wire>::clone(&wire);
        let shared = Shared::new(StreamId(1), sink, 8_000_000, 60, true);
        // A stream past its first keyframe, with nothing sent yet.
        shared.pending.lock().keyframe = false;
        shared.last_push_us.store(0, Ordering::Relaxed);
        let shared = Arc::new(shared);
        (shared, wire)
    }

    #[test]
    fn quantiles_read_any_order_and_a_ring_keeps_the_window() {
        assert_eq!(Quantiles::of(&[]), Quantiles::default());
        let q = Quantiles::of(&[5, 1, 3, 2, 4]);
        assert_eq!((q.n, q.p50_us, q.p95_us, q.max_us), (5, 3, 5, 5));
        assert_eq!(Quantiles::of(&[7]).describe(), "0.01 / 0.01 / 0.01 ms (n=1)");
        let mut ring = LatencyRing::default();
        for us in 0..u64::try_from(LATENCY_WINDOW).unwrap_or(u64::MAX).saturating_add(10) {
            ring.push(us);
        }
        let q = ring.quantiles();
        assert_eq!(usize::try_from(q.n).unwrap_or(0), LATENCY_WINDOW, "bounded to the window");
        assert_eq!(q.max_us, u64::try_from(LATENCY_WINDOW).unwrap_or(0).saturating_add(9));
        assert!(q.p50_us > 10, "the oldest samples left first: {q:?}");
    }

    #[test]
    fn configs_clamp_the_quality_and_keep_even_sides() {
        let q = Quality { fps: 500, bitrate_bps: 1, scale: 0.3333, codec: VideoCodec::Hevc };
        let (capture, encoder) = configs((1_001, 777), &q);
        assert_eq!((capture.width, capture.height), (334, 260), "scaled, rounded up to even");
        assert_eq!(capture.fps, 240, "fps clamped");
        assert_eq!(capture.format, PixelFormat::Nv12Full, "full range, what the client samples");
        assert_eq!(encoder.bitrate_bps, 100_000, "bitrate floor");
        assert_eq!((encoder.width, encoder.height, encoder.fps), (334, 260, 240));

        let nan = Quality { scale: f32::NAN, codec: VideoCodec::H264, ..q };
        let (capture, encoder) = configs((100, 100), &nan);
        assert_eq!((capture.width, capture.height), (100, 100), "a NaN scale is native");
        assert_eq!(encoder.codec, VideoCodec::H264, "the codec asked for");

        let tiny = Quality { scale: 0.0001, ..q };
        let (capture, _encoder) = configs((10, 10), &tiny);
        assert_eq!((capture.width, capture.height), (2, 2), "never below two pixels");
    }

    #[test]
    fn the_registry_lists_live_streams_then_the_last_closed_and_tells_its_observer() {
        let registry = Registry::default();
        let counts = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&counts);
        registry.observe(move |n| seen.lock().push(n));
        let handle = |id: u32| {
            let (shared, _wire) = shared_for_frames();
            // Fresh from the helper: nothing else holds it, so the unwrap cannot fail.
            let mut shared = Arc::try_unwrap(shared).ok().expect("fresh");
            shared.id = StreamId(id);
            StatsHandle(Arc::new(shared))
        };
        registry.insert(&"alice", CaptureTarget::Display(1), handle(1));
        registry.insert(&"bob", CaptureTarget::Display(2), handle(2));
        let (live, closed) = registry.summaries();
        assert_eq!(
            live.iter().map(|s| (s.client.as_str(), s.stream)).collect::<Vec<_>>(),
            [("alice", 1), ("bob", 2)]
        );
        assert!(closed.is_empty());
        registry.remove(&"alice", StreamId(9));
        assert_eq!(registry.summaries().0.len(), 2, "an unknown stream is not removed");
        registry.remove(&"alice", StreamId(1));
        let (live, closed) = registry.summaries();
        assert_eq!(live.len(), 1);
        assert_eq!(closed.iter().map(|s| s.stream).collect::<Vec<_>>(), [1]);
        assert_eq!(*counts.lock(), [1, 2, 1], "the observer hears every change, in order");
        for id in 10..u32::try_from(CLOSED_KEEP).unwrap_or(u32::MAX).saturating_add(12) {
            registry.insert(&"carol", CaptureTarget::Display(id), handle(id));
            registry.remove(&"carol", StreamId(id));
        }
        let (_live, closed) = registry.summaries();
        assert_eq!(closed.len(), CLOSED_KEEP, "only the last few closed are kept");
        assert_eq!(closed.first().map(|s| s.stream), Some(12), "oldest first");
    }

    /// One 16x16 frame. The contents do not matter: with no encoder nothing reads them, and
    /// what is being tested is whether `on_frame` gets that far at all.
    fn a_frame() -> CapturedFrame {
        use std::ptr::{self, NonNull};

        let mut raw: *mut objc2_core_video::CVPixelBuffer = ptr::null_mut();
        // SAFETY: the out-pointer is valid and no attributes dictionary is passed
        // (CoreVideo, `CVPixelBufferCreate`).
        let status = unsafe {
            objc2_core_video::CVPixelBufferCreate(
                None,
                16,
                16,
                objc2_core_video::kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                None,
                NonNull::from(&mut raw),
            )
        };
        assert_eq!(status, 0, "CVPixelBufferCreate");
        // SAFETY: `CVPixelBufferCreate` returned a +1 reference, which this takes over.
        let buffer = unsafe {
            objc2_core_foundation::CFRetained::from_raw(NonNull::new(raw).expect("a buffer"))
        };
        CapturedFrame {
            image: slopty_codec::PixelBuffer::from_retained(buffer),
            capture_ts_us: 1,
            display_ts_us: None,
            age_us: 0,
            latency_us: 0,
        }
    }

    /// Another reference to the same capture, as ScreenCaptureKit hands over a surface it keeps.
    fn again(frame: &CapturedFrame) -> CapturedFrame {
        CapturedFrame {
            image: slopty_codec::PixelBuffer::from_retained(objc2_core_foundation::Type::retain(
                frame.image.as_cv(),
            )),
            ..*frame
        }
    }

    /// The beat is a promise about time, so the thing that must be true of it is that nothing
    /// else the stream does can make it late. Geometry work that takes 300 ms — six times a
    /// stall gap, and three times the worst window-server round trip measured — runs beside it
    /// here, and the beats keep their cadence because they are no longer on that task.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_slow_geometry_call_does_not_make_the_beat_late() {
        let (shared, wire) = shared_for_frames();
        let beat = tokio::spawn(beat_loop(Arc::clone(&shared)));
        // Whatever the cursor loop does, it does it like this: off the runtime's workers. It
        // spins rather than sleeps because a sleeping thread is not what a window-server round
        // trip does to one — and because sleeping in this project is what the lint forbids.
        let slow = tokio::task::spawn_blocking(|| {
            let until = Instant::now()
                .checked_add(Duration::from_millis(300))
                .expect("a deadline inside the clock");
            while Instant::now() < until {
                std::hint::spin_loop();
            }
        });

        tokio::time::sleep(Duration::from_millis(500)).await;
        slow.await.expect("the slow call");
        beat.abort();

        let seen = shared.stats();
        assert!(seen.heartbeats >= 10, "a beat every 25 ms over half a second: {seen:?}");
        assert!(
            seen.beat_gap_worst_us <= 100_000,
            "the beat fell behind the geometry call: {} µs",
            seen.beat_gap_worst_us
        );
        // The beats really went out, rather than only being counted.
        let sent = wire.drain().len();
        assert!(sent >= 8, "only {sent} datagrams for {} beats", seen.heartbeats);
    }

    /// The beat waits exactly as long as the silence has left to run, and never polls.
    #[test]
    fn a_beat_is_due_when_the_silence_reaches_the_promise() {
        assert_eq!(beat_due_in(0, 25_000), Some(Duration::from_millis(25)));
        assert_eq!(beat_due_in(24_000, 25_000), Some(Duration::from_millis(1)));
        assert_eq!(beat_due_in(25_000, 25_000), None);
        assert_eq!(beat_due_in(u64::MAX, 25_000), None);
    }

    /// A transport that refuses the beat does not make the loop spin on the refusal: it waits a
    /// period before the next try.
    #[tokio::test]
    async fn a_refused_beat_is_retried_a_period_later() {
        let (shared, wire) = shared_for_frames();
        wire.max.store(0, Ordering::Relaxed);
        let beat = tokio::spawn(beat_loop(Arc::clone(&shared)));
        tokio::time::sleep(Duration::from_millis(200)).await;
        beat.abort();
        let seen = shared.stats();
        assert!(
            (4..=10).contains(&seen.heartbeats),
            "about one attempt per 25 ms over 200 ms, not a spin: {seen:?}"
        );
    }

    /// A frame that arrives inside the hold a suspicion opened is kept back and counted as
    /// suspected, not withheld: the window list has not confirmed anything yet. Once the hold
    /// lapses without confirmation, frames flow again.
    #[test]
    fn a_frame_captured_under_suspicion_is_held_until_the_hold_lapses() {
        let (shared, _wire) = shared_for_frames();
        let hold_us = u64::try_from(SUSPICION_HOLD.as_micros()).unwrap();
        // Built first: the first pixel buffer of a process takes longer than the hold.
        let frame = a_frame();

        shared.suspect(1_000);
        assert!(shared.suspected_at(1_000), "the hold opens at once");
        assert!(shared.suspected_at(1_000 + hold_us - 1), "and lasts the whole hold");
        assert!(!shared.suspected_at(1_000 + hold_us), "and no longer");
        assert_eq!(shared.stats().suspicions, 1);

        // A frame now is held as suspected and never reaches the crop count.
        shared.suspect(host_now_us());
        shared.on_frame(again(&frame));
        let held = shared.stats();
        assert_eq!((held.captured, held.suspected, held.withheld, held.cropped), (1, 1, 0, 0));

        // The window filter's frames are held just the same: the framework still delivers a
        // frame or two of the old filter after a swap has settled.
        shared.cropped.store(false, Ordering::Relaxed);
        shared.on_frame(again(&frame));
        let filtered = shared.stats();
        assert_eq!((filtered.captured, filtered.suspected, filtered.cropped), (2, 2, 0));
        shared.cropped.store(true, Ordering::Relaxed);

        // Confirmed by the window list: the frame is withheld, the older reason wins.
        shared.target_hidden.store(true, Ordering::Relaxed);
        shared.on_frame(again(&frame));
        let confirmed = shared.stats();
        assert_eq!((confirmed.suspected, confirmed.withheld), (2, 1));

        // The hold has lapsed and the window list says on screen: frames flow again.
        shared.target_hidden.store(false, Ordering::Relaxed);
        shared.suspect_until_us.store(0, Ordering::Relaxed);
        shared.on_frame(again(&frame));
        let flowing = shared.stats();
        assert_eq!((flowing.captured, flowing.suspected, flowing.cropped), (4, 2, 1));
    }

    /// The ladder is the worker's, not only the policy's: a target that has collapsed moves the
    /// rung the guard and the gate both read, and a target that recovers moves it back.
    #[test]
    fn a_collapsed_target_takes_the_stream_down_the_cadence_ladder() {
        let (shared, _wire) = shared_for_frames();

        shared.apply_cadence(1_000_000);
        assert_eq!(shared.fps.load(Ordering::Relaxed), 15, "2 KB a frame at 60 is not a picture");
        shared.apply_cadence(30_000_000);
        assert_eq!(shared.fps.load(Ordering::Relaxed), 60);
    }

    /// Captures keep arriving on the display's beat whatever the cadence is; the gate is what
    /// decides which of them the encoder is given, and it counts in time rather than in frames.
    #[test]
    fn the_cadence_gate_hands_the_encoder_one_capture_a_period() {
        let (shared, _wire) = shared_for_frames();
        shared.fps.store(30, Ordering::Relaxed);

        // One buffer, moved through the beats: building a `CVPixelBuffer` costs far more than the
        // gate this is about, and nothing downstream of here reads the pixels.
        let mut frame = a_frame();
        let mut encoded = 0_u32;
        for beat in 0..12_u64 {
            // Capture timestamps are the worker clock, so the first frame of a stream is always
            // due.
            frame.capture_ts_us = 1_000_000_u64.saturating_add(beat.saturating_mul(16_667));
            shared.on_frame(again(&frame));
            if shared.last_encoded_us.load(Ordering::Relaxed) == frame.capture_ts_us {
                encoded = encoded.saturating_add(1);
            }
        }

        assert_eq!(encoded, 6, "half of a 60 fps capture beat belongs to a 30 fps cadence");
        assert_eq!(shared.stats().captured, 12, "every capture is still counted");
        assert_eq!(shared.stats().dropped, 0, "a paced skip is not a congestion drop");

        // A client with no picture waits for a keyframe, not for the rung.
        let last = shared.last_encoded_us.load(Ordering::Relaxed);
        shared.pending.lock().keyframe = true;
        frame.capture_ts_us = last.saturating_add(1_000);
        shared.on_frame(again(&frame));
        assert_eq!(shared.last_encoded_us.load(Ordering::Relaxed), last.saturating_add(1_000));
    }

    /// What the crop must never hand on. The path is decided on the geometry tick, but the
    /// frames keep coming while ScreenCaptureKit works through the swap, so the decision is
    /// applied again where a frame arrives: while the target is off screen the frame is counted
    /// as withheld and goes no further — it is not a picture of the target, and under a display
    /// crop it is a picture of whatever is behind it.
    #[test]
    fn a_frame_captured_while_the_target_is_hidden_is_withheld() {
        let (shared, _wire) = shared_for_frames();

        shared.on_frame(a_frame());
        let seen = shared.stats();
        assert_eq!((seen.captured, seen.withheld, seen.cropped), (1, 0, 1));

        shared.target_hidden.store(true, Ordering::Relaxed);
        shared.on_frame(a_frame());
        let hidden = shared.stats();
        assert_eq!(
            (hidden.captured, hidden.withheld, hidden.cropped),
            (2, 1, 1),
            "a frame captured while the window was off screen was served from the crop"
        );

        // And the counter that says which path is live is the stream's, not the snapshot's
        // default: a reader of an active crop must not be told it is on the window filter.
        assert!(hidden.on_crop, "a stream on the display crop reported the window filter");
    }

    /// The clock the tracker is told about, so these run in no time and never flake.
    fn at(base: Instant, ms: u64) -> Instant {
        base.checked_add(Duration::from_millis(ms)).expect("inside the clock")
    }

    #[test]
    fn a_source_that_never_draws_is_idle_after_the_grace_and_not_before() {
        let base = Instant::now();
        let mut t = SourceTracker::new(base);
        assert_eq!(t.poll(0, false, at(base, 100)), None, "inside the grace, say nothing");
        assert_eq!(t.poll(0, false, at(base, 399)), None);
        assert_eq!(t.poll(0, false, at(base, 400)), Some(SourceState::Idle));
        assert_eq!(t.poll(0, false, at(base, 900)), None, "only changes are sent");
    }

    #[test]
    fn the_first_frame_makes_it_live_whenever_it_comes() {
        let base = Instant::now();
        let mut t = SourceTracker::new(base);
        assert_eq!(t.poll(0, false, at(base, 500)), Some(SourceState::Idle));
        assert_eq!(t.poll(1, false, at(base, 600)), Some(SourceState::Live));
        assert_eq!(t.poll(2, false, at(base, 700)), None);
    }

    #[test]
    fn a_source_that_drew_once_and_stopped_goes_idle_again() {
        // The latch this replaces reported `Live` for the rest of the stream, so a window that
        // drew a frame and was then hidden left the receiver asking for refreshes.
        let base = Instant::now();
        let mut t = SourceTracker::new(base);
        assert_eq!(t.poll(1, false, at(base, 100)), Some(SourceState::Live));
        assert_eq!(t.poll(1, false, at(base, 1_000)), None, "quiet, but not for long enough");
        assert_eq!(t.poll(1, false, at(base, 2_100)), Some(SourceState::Idle));
        assert_eq!(t.poll(2, false, at(base, 2_200)), Some(SourceState::Live), "it drew again");
    }

    #[test]
    fn a_slow_target_does_not_flap_between_the_two() {
        // One frame a second: quiet by the 400 ms rule, live by the one that matters.
        let base = Instant::now();
        let mut t = SourceTracker::new(base);
        assert_eq!(t.poll(1, false, at(base, 0)), Some(SourceState::Live));
        for second in 1..10 {
            let now = at(base, second * 1_000);
            assert_eq!(t.poll(second, false, now), None, "no change at {second} s");
        }
    }

    #[test]
    fn a_hidden_target_is_idle_at_once_however_much_it_drew() {
        let base = Instant::now();
        let mut t = SourceTracker::new(base);
        assert_eq!(t.poll(10, false, at(base, 100)), Some(SourceState::Live));
        assert_eq!(t.poll(10, true, at(base, 150)), Some(SourceState::Idle), "no grace for this");
        // And the frames that were already in flight when it went do not undo it.
        assert_eq!(t.poll(11, true, at(base, 200)), None);
        assert_eq!(t.poll(12, false, at(base, 250)), Some(SourceState::Live), "back on screen");
    }

    #[test]
    fn a_crop_is_allowed_only_on_screen_on_one_display_and_uncovered() {
        assert_eq!(crop_allowed(true, Some(CROP), false), Some(CROP));
        assert_eq!(crop_allowed(false, Some(CROP), false), None, "minimised or another Space");
        assert_eq!(crop_allowed(true, None, false), None, "partly off screen");
        assert_eq!(crop_allowed(true, Some(CROP), true), None, "covered");
    }

    /// A configuration as a transition carries it: `crop` on a 600×400 stream.
    fn config(crop: Option<Crop>) -> CaptureConfig {
        let quality =
            Quality { fps: 60, bitrate_bps: 8_000_000, scale: 1.0, codec: VideoCodec::Hevc };
        CaptureConfig { crop, ..configs((600, 400), &quality).0 }
    }

    #[test]
    fn a_transition_commits_only_when_every_call_succeeded() {
        let t = Transitions::default();
        assert_eq!(t.settle(), Settled::Idle);
        assert!(t.begin(WindowPath::DisplayCrop, config(Some(CROP)), 2));
        assert_eq!(t.settle(), Settled::Busy);
        assert!(!t.begin(WindowPath::Filter, config(None), 1), "one at a time");
        t.on_result(true);
        assert_eq!(t.settle(), Settled::Busy, "one callback still to come");
        t.on_result(true);
        assert_eq!(t.settle(), Settled::Commit(WindowPath::DisplayCrop, config(Some(CROP))));
        assert_eq!(t.settle(), Settled::Idle, "taken once");
    }

    #[test]
    fn a_failed_call_leaves_the_stream_where_it_was_and_the_next_tick_retries() {
        let t = Transitions::default();
        assert!(t.begin(WindowPath::Filter, config(None), 2));
        t.on_result(true);
        t.on_result(false);
        assert_eq!(t.settle(), Settled::Failed(WindowPath::Filter, config(None)));
        // Nothing committed, nothing in flight: the next tick may ask again.
        assert_eq!(t.settle(), Settled::Idle);
        assert!(t.begin(WindowPath::Filter, config(None), 2));
    }

    /// A transition commits the size and the crop together: what ScreenCaptureKit was sent is
    /// what the stream records, so a resize cannot land beside a crop move and leave the older
    /// crop in force.
    #[test]
    fn a_transition_carries_the_size_and_the_crop_as_one_configuration() {
        let t = Transitions::default();
        let moved = Crop { x: 40.0, ..CROP };
        let resized = CaptureConfig { width: 800, height: 500, ..config(Some(moved)) };
        assert!(t.begin(WindowPath::DisplayCrop, resized, 1));
        t.on_result(true);
        let Settled::Commit(path, committed) = t.settle() else { panic!("not committed") };
        assert_eq!(
            (path, committed.crop, committed.width),
            (WindowPath::DisplayCrop, Some(moved), 800)
        );
    }

    /// A live drag changes the window's size on every tick; the stream is rebuilt only for a
    /// size that holds for one, so a drag costs one keyframe at its end rather than ten a second.
    #[test]
    fn a_resize_rebuilds_only_once_the_size_holds_for_a_tick() {
        let mut debounce = ResizeDebounce::default();
        let built = (800, 600);
        assert_eq!(debounce.observe((800, 600), built), None, "no change");
        // Dragging: a new size every tick.
        assert_eq!(debounce.observe((820, 610), built), None);
        assert_eq!(debounce.observe((840, 620), built), None);
        assert_eq!(debounce.observe((860, 630), built), None);
        // Released: the same size twice.
        assert_eq!(debounce.observe((860, 630), built), Some((860, 630)));
        // Built for it now; a size that goes back before it holds rebuilds nothing.
        let built = (860, 630);
        assert_eq!(debounce.observe((900, 700), built), None);
        assert_eq!(debounce.observe((860, 630), built), None, "back where it was");
        assert_eq!(debounce.observe((900, 700), built), None, "a fresh candidate, not the old one");
    }

    /// The encode latency is the time between a frame's submit and its return, matched by
    /// pts whatever order the encoder returns them in; a return with no submit on record is
    /// not a sample, and the record holds `IN_FLIGHT_MAX` frames at most.
    #[test]
    fn encode_latency_is_matched_by_pts_and_bounded_in_flight() {
        let counters = Counters::new();
        counters.submitted(1, 1_000);
        counters.submitted(2, 2_000);
        counters.returned(2, 2_500);
        counters.returned(1, 4_000);
        counters.returned(9, 5_000);
        let encode = counters.snapshot().encode;
        assert_eq!((encode.n, encode.p50_us, encode.max_us), (2, 3_000, 3_000), "500 and 3 000 µs");
        for pts in 0..u64::try_from(IN_FLIGHT_MAX).unwrap_or(u64::MAX) {
            counters.submitted(100 + pts, 10_000);
        }
        counters.submitted(200, 10_000);
        assert_eq!(
            counters.in_flight.lock().front().map(|f| f.0),
            Some(101),
            "the oldest forgotten"
        );
        counters.returned(100, 20_000);
        assert_eq!(counters.snapshot().encode.n, 2, "a forgotten frame is not a sample");
    }

    /// A batch goes to the transport in one call. A datagram larger than the path now carries is
    /// a sizing bug, not the end of the stream: the ones that fit still go and the rest are
    /// counted as refused. A closed connection takes nothing, and the loops see it.
    #[test]
    fn a_too_large_datagram_costs_itself_and_a_closed_link_takes_nothing() {
        let (shared, wire) = shared_for_frames();
        let batch = [Bytes::from_static(b"a"), Bytes::from_static(b"bb"), Bytes::from_static(b"c")];
        assert_eq!(shared.send(&batch), Taken { datagrams: 3, bytes: 4 });
        assert_eq!(wire.calls.load(Ordering::Relaxed), 1, "one call for the whole batch");
        assert_eq!(wire.drain(), batch);

        wire.max.store(1, Ordering::Relaxed);
        assert_eq!(shared.send(&batch), Taken { datagrams: 2, bytes: 2 }, "the two that fit");
        assert_eq!(wire.drain(), [Bytes::from_static(b"a"), Bytes::from_static(b"c")]);
        assert!(!shared.sink.is_closed(), "the stream goes on");

        wire.closed.store(true, Ordering::Relaxed);
        assert_eq!(shared.send(&batch), Taken::default());
        assert!(wire.drain().is_empty());
        let stats = shared.stats();
        assert_eq!((stats.datagrams, stats.queue_full), (5, 4));
    }

    /// An access unit from the encoder is packetized and handed over in one call, and counted; a
    /// NACK for a frame in the packetizer's history answers with those fragments, one outside it
    /// with nothing, and none at all while QUIC holds more than the frame budget.
    #[test]
    fn an_encoded_packet_is_sent_and_a_nack_answers_from_history() {
        let (shared, wire) = shared_for_frames();
        shared.counters.bitrate_bps.store(30_000_000, Ordering::Relaxed);
        let packet = EncodedPacket {
            data: vec![7; 3000],
            keyframe: true,
            ltr_token: Some(1),
            ltr_refresh: false,
            pts_us: host_now_us(),
        };
        shared.on_packet(&packet);
        let sent = wire.drain();
        assert!(sent.len() >= 3, "3 000 bytes under the MTU: {}", sent.len());
        assert_eq!(wire.calls.load(Ordering::Relaxed), 1, "the frame in one call");
        let stats = shared.stats();
        assert_eq!((stats.encoded, stats.datagrams), (1, sent.len() as u64));
        shared.nack(0, &[0, 1]);
        assert_eq!(wire.drain().len(), 2, "two fragments of frame 0 again");
        shared.nack(0, &[]);
        assert!(!wire.drain().is_empty(), "no fragments named: the whole frame's data");
        shared.nack(9, &[0]);
        assert!(wire.drain().is_empty(), "frame 9 was never sent");
        wire.held.store(10_000_000, Ordering::Relaxed);
        shared.nack(0, &[0]);
        assert!(wire.drain().is_empty(), "QUIC is holding seconds of frames");
    }

    /// A refresh request marks the next frame; a report hands acknowledged LTR tokens to the
    /// encoder's options and counts the datagrams sent since the last one.
    #[test]
    fn a_refresh_and_a_report_reach_the_next_frame_options() {
        let (shared, _wire) = shared_for_frames();
        for token in [5, 6] {
            shared.ltr.lock().on_packet(false, Some(token), 0);
        }
        shared.request_refresh(41, false);
        assert!(shared.pending.lock().refresh);
        assert_eq!(shared.stats().refreshes, 1);
        let mut acked_ltr = [0; 4];
        acked_ltr[..2].copy_from_slice(&[5, 6]);
        let report = ReceiverReport { acked_ltr, acked_ltr_len: 2, ..ReceiverReport::default() };
        let _decision = shared.report(&report, None);
        assert_eq!(shared.pending.lock().acked, vec![5, 6], "two of the four slots were valid");
        assert_eq!(shared.sent_at_report.load(Ordering::Relaxed), 0, "nothing sent yet");
    }

    /// Audio goes out while the source is loud and for the hold after it; silence past the
    /// hold sends nothing, and each packet carries the next sequence number.
    #[test]
    fn audio_is_gated_by_silence_and_numbered() -> Result<(), String> {
        let (shared, _wire) = shared_for_frames();
        let samples = usize::try_from(FRAME_SAMPLES * CHANNELS).map_err(|e| e.to_string())?;
        let quiet = vec![0.0_f32; samples];
        let loud: Vec<f32> = (0..samples).map(|i| if i % 2 == 0 { 0.5 } else { -0.5 }).collect();
        let start = 1_000_000_u64;
        assert!(shared.encode_audio(&quiet, start).is_none(), "silence from the start");
        let first = shared.encode_audio(&loud, start).ok_or("loud: a packet")?;
        let second = shared.encode_audio(&loud, start + 20_000).ok_or("loud again")?;
        let seqs: Vec<u32> = first.iter().chain(&second).map(|(seq, _)| *seq).collect();
        assert_eq!(seqs, vec![1, 2], "one packet per frame, numbered from 1");
        assert!(
            shared.encode_audio(&quiet, start + 20_000 + AUDIO_HOLD_US).is_some(),
            "silence inside the hold still goes out"
        );
        assert!(
            shared.encode_audio(&quiet, start + 20_001 + AUDIO_HOLD_US).is_none(),
            "and past it, nothing"
        );
        Ok(())
    }

    #[test]
    fn the_clock_byte_and_the_crop_knob_are_plain_values() {
        assert_eq!(send_ms_lo(0), 0);
        assert_eq!(send_ms_lo(255_999), 255);
        assert_eq!(send_ms_lo(256_000), 0, "the low byte of the millisecond clock wraps");

        assert!(crop_windows_from(None), "the build default is the crop");
        assert!(crop_windows_from(Some("crop")));
        assert!(!crop_windows_from(Some("window")));
        assert!(crop_windows_from(Some("anything else")), "an unknown value is the default");

        let (shared, _wire) = shared_for_frames();
        assert!(!shared.filter_stalled.load(Ordering::Relaxed));
        shared.sibling_went();
        shared.sibling_went();
        assert!(shared.filter_stalled.load(Ordering::Relaxed), "the next tick re-filters");
        assert_eq!(shared.stats().siblings, 2, "and it is counted, not a suspicion");
        assert_eq!(shared.stats().suspicions, 0);
        assert!(!shared.suspected_at(host_now_us()), "a sibling holds no frames");
    }

    #[test]
    fn a_frame_fits_unless_quic_holds_too_much() {
        // 30 Mbit/s at 60 fps: 62.5 KB per frame, two frames may be held.
        assert!(frame_fits(0, 5_808, 30_000_000, 60));
        assert!(frame_fits(125_000, 5_808, 30_000_000, 60));
        assert!(!frame_fits(125_001, 5_808, 30_000_000, 60));
        // A collapsed path: 1.8 Mbit/s at 60 fps is 3.7 KB a frame, so the budget is 7.4 KB —
        // 33 ms of queue against the 146 ms a fixed 32 KB floor would have allowed at this
        // rate, which is the whole point of sizing it from the rate in force.
        assert!(frame_fits(7_500, 4_920, 1_800_000, 60));
        assert!(!frame_fits(7_501, 4_920, 1_800_000, 60));
        assert!(!frame_fits(32 * 1024, 4_920, 1_800_000, 60));
        // One window is the floor: past `rtt × fps` of 1.8 two frames fall under it, and bytes
        // inside the window leave on the next acknowledgement rather than standing in a queue.
        // 1 Mbit/s at 60 fps is 2 083 B a frame, two of them 4 166, under a 4 920 B window.
        assert!(frame_fits(4_920, 4_920, 1_000_000, 60));
        assert!(!frame_fits(4_921, 4_920, 1_000_000, 60));
        assert!(frame_fits(0, 0, 0, 0), "nothing known, nothing held");
    }

    #[test]
    fn a_keyframe_fits_while_the_link_drains_one_inside_the_budget() {
        // 30 Mbit/s is 3.75 MB/s, so 400 ms carries 1.5 MB: the ladder's biggest keyframe is
        // nothing to a healthy link and must not be deferred there.
        assert!(keyframe_fits(133_960, 0, 30_000_000));
        assert!(keyframe_fits(1_500_000, 0, 30_000_000));
        assert!(!keyframe_fits(1_500_001, 0, 30_000_000));
        // Bytes already queued come out of the same budget.
        assert!(!keyframe_fits(1_500_000, 1, 30_000_000));
        // The `lte` rung settles around 9 Mbit/s, which carries 450 kB: still no deferral. Only
        // the collapsed rung earns one.
        assert!(keyframe_fits(133_960, 0, 9_000_000));
        // The 1 Mbit/s floor is 125 kB/s, so 400 ms carries 50 kB, and the same 134 kB keyframe —
        // 1.07 s of that link on its own — is refused.
        assert!(!keyframe_fits(133_960, 0, 1_000_000));
        assert!(keyframe_fits(50_000, 0, 1_000_000));
        assert!(!keyframe_fits(50_001, 0, 1_000_000));
        // No estimate yet: a stream's first keyframe is never refused, whatever the link.
        assert!(keyframe_fits(0, 4 * 1024 * 1024, 0));
    }

    #[test]
    fn the_keyframe_estimate_rises_at_once_and_falls_by_an_eighth() {
        assert_eq!(keyframe_estimate(0, 133_960), 133_960, "the first one is the estimate");
        assert_eq!(keyframe_estimate(10_000, 133_960), 133_960, "a jump is taken whole");
        // Falling: 133 960 - 16 745 + 1 250 = 118 465. One cheap keyframe moves it 12 %, so a
        // blank screen between two busy ones cannot license the next expensive one.
        assert_eq!(keyframe_estimate(133_960, 10_000), 118_465);
        assert_eq!(keyframe_estimate(8, 8), 8, "a steady stream stays put");
    }

    /// A report acknowledging `tokens`.
    fn acking(tokens: &[u64]) -> ReceiverReport {
        let mut acked_ltr = [0; 4];
        let n = tokens.len().min(4);
        acked_ltr[..n].copy_from_slice(&tokens[..n]);
        ReceiverReport {
            acked_ltr,
            acked_ltr_len: u8::try_from(n).unwrap_or(4),
            ..ReceiverReport::default()
        }
    }

    /// An encoded frame of `bytes`, a keyframe or not, offering `token`.
    fn packet(bytes: usize, keyframe: bool, token: Option<u64>, refresh: bool) -> EncodedPacket {
        EncodedPacket {
            data: vec![7; bytes],
            keyframe,
            ltr_token: token,
            ltr_refresh: refresh,
            pts_us: host_now_us(),
        }
    }

    #[test]
    fn a_keyframe_is_deferred_only_when_a_refresh_can_go_out_instead() {
        let (shared, _wire) = shared_for_frames();
        shared.keyframe_bytes.store(133_960, Ordering::Relaxed);
        shared.counters.bitrate_bps.store(1_000_000, Ordering::Relaxed);
        // No reference is usable, so a refresh would come back as an IDR anyway and the keyframe
        // goes out however badly it fits.
        assert!(shared.keyframe_admitted(1_000_000));
        assert_eq!(shared.stats().keyframes_deferred, 0);

        shared.on_packet(&packet(900, false, Some(7), false));
        shared.report(&acking(&[7]), None);
        assert!(!shared.keyframe_admitted(1_000_000), "now a refresh is a picture");
        assert_eq!(shared.stats().keyframes_deferred, 1);

        // The run is one episode however many frames it spans, and the valve opens a second in.
        assert!(!shared.keyframe_admitted(1_016_000));
        assert!(!shared.keyframe_admitted(1_999_999));
        assert_eq!(shared.stats().keyframes_deferred, 1);
        assert!(shared.keyframe_admitted(2_000_000), "the valve opens rather than hold forever");
    }

    /// A client whose decoder lost its session holds none of the references it acknowledged, so
    /// the refresh it asks for as a keyframe is an IDR: not an LTR delta off the reference, and
    /// not deferred for one however badly it fits the link.
    #[test]
    fn a_keyframe_refresh_is_an_idr_even_with_a_usable_reference() {
        let (shared, _wire) = shared_for_frames();
        shared.keyframe_bytes.store(133_960, Ordering::Relaxed);
        shared.counters.bitrate_bps.store(1_000_000, Ordering::Relaxed);
        shared.on_packet(&packet(900, false, Some(7), false));
        shared.report(&acking(&[7]), None);
        shared.request_refresh(41, false);
        let (keyframe, refresh) = {
            let pending = shared.pending.lock();
            (pending.keyframe, pending.refresh)
        };
        assert!(refresh && !keyframe && shared.ltr_usable(), "a plain refresh: a delta off 7");
        shared.pending.lock().refresh = false;

        shared.request_refresh(41, true);
        let (keyframe, refresh) = {
            let pending = shared.pending.lock();
            (pending.keyframe, pending.refresh)
        };
        assert!(keyframe && !refresh, "the encoder is asked for a keyframe");
        assert!(!shared.ltr_usable(), "reference 7 went with the client's session");
        assert!(shared.keyframe_admitted(1_000_000), "so nothing is left to defer it for");
        assert_eq!(shared.stats().keyframes_deferred, 0);
        shared.report(&acking(&[7]), None);
        assert!(!shared.ltr_usable(), "a late ack from the lost session names nothing");
        assert_eq!(shared.stats().refreshes, 2);
    }

    /// The rule the keyframe path could not reach: `pending.keyframe` is set at stream open and
    /// on a quality change and almost nowhere else, so `keyframes_deferred` read 0 on every rung
    /// of the shaped ladder. The *drop* path runs on every dropped frame, which on a collapsed
    /// link is most of them, and a refresh is budgeted as the IDR it may be — so this is where
    /// the budget bites.
    #[test]
    fn a_dropped_frame_asks_for_a_refresh_only_when_the_link_could_carry_one() {
        let (shared, _wire) = shared_for_frames();
        shared.counters.bitrate_bps.store(1_000_000, Ordering::Relaxed);

        // No keyframe measured yet: the stream's first picture is never refused.
        shared.dropped(1_000_000);
        assert!(shared.pending.lock().refresh, "no estimate means ask anyway");
        assert_eq!(shared.counters.dropped.load(Ordering::Relaxed), 1);

        shared.pending.lock().refresh = false;
        shared.keyframe_bytes.store(133_960, Ordering::Relaxed);
        shared.dropped(1_000_000);
        assert!(
            !shared.pending.lock().refresh,
            "1 Mbit/s cannot drain 134 kB: asking would only deepen the hole"
        );
        assert_eq!(shared.counters.dropped.load(Ordering::Relaxed), 2, "still counted as a drop");

        // The link recovers; the next hole is worth a picture again.
        shared.counters.bitrate_bps.store(12_000_000, Ordering::Relaxed);
        shared.dropped(1_100_000);
        assert!(shared.pending.lock().refresh);
    }

    /// A link that recovers ends the episode by itself, not only a keyframe encoded: the next
    /// collapse is its own episode with its own clock. With the first episode's start left
    /// standing, the second one's first frame found the valve a second past and let the
    /// keyframe straight through.
    #[test]
    fn a_link_that_recovers_ends_the_deferral_without_the_valve() {
        let (shared, _wire) = shared_for_frames();
        shared.on_packet(&packet(900, false, Some(3), false));
        shared.report(&acking(&[3]), None);
        shared.keyframe_bytes.store(133_960, Ordering::Relaxed);
        shared.counters.bitrate_bps.store(1_000_000, Ordering::Relaxed);
        assert!(!shared.keyframe_admitted(1_000_000));
        // The rate controller climbed back: 12 Mbit/s carries 600 kB in the drain window.
        shared.counters.bitrate_bps.store(12_000_000, Ordering::Relaxed);
        assert!(shared.keyframe_admitted(1_100_000), "not the valve, the link");
        assert_eq!(shared.stats().keyframes_deferred, 1, "still the one episode");

        // Five seconds on the link collapses again: a new episode, deferred from its start.
        shared.counters.bitrate_bps.store(1_000_000, Ordering::Relaxed);
        assert!(!shared.keyframe_admitted(6_000_000), "a stale start opened the valve at once");
        assert_eq!(shared.stats().keyframes_deferred, 2, "counted as its own episode");
        assert!(!shared.keyframe_admitted(6_999_999));
        assert!(shared.keyframe_admitted(7_000_000), "its own valve, a second after its own start");
    }

    /// A token is a reference a refresh can use only while nothing has emptied the reference
    /// list since it was offered: a keyframe does, and so does a new encoder session. Tokens the
    /// session never offered do not reach the encoder at all.
    #[test]
    fn a_reference_is_usable_until_a_keyframe_or_a_rebuild() {
        let (shared, _wire) = shared_for_frames();
        shared.on_packet(&packet(40_000, true, None, false));
        shared.on_packet(&packet(900, false, Some(11), false));
        assert!(!shared.ltr_usable(), "offered, not acknowledged");
        shared.report(&acking(&[11, 99]), None);
        assert!(shared.ltr_usable());
        assert_eq!(shared.pending.lock().acked, vec![11], "99 was never offered");
        let seen = shared.stats().ltr;
        assert_eq!((seen.offered, seen.acked, seen.usable), (1, 1, true));

        // A refresh answered as a delta, then one answered as an IDR: the IDR retires the token.
        shared.on_packet(&packet(700, false, None, true));
        assert!(shared.ltr_usable());
        shared.on_packet(&packet(40_000, true, None, true));
        assert!(!shared.ltr_usable(), "an IDR empties the reference list");
        shared.report(&acking(&[11]), None);
        assert!(!shared.ltr_usable(), "a late ack for the old epoch names nothing usable");
        let seen = shared.stats().ltr;
        assert_eq!((seen.refreshes_delta, seen.refreshes_idr), (1, 1));

        // A token offered after the keyframe is usable again, until the encoder is rebuilt.
        shared.on_packet(&packet(900, false, Some(12), false));
        shared.report(&acking(&[12]), None);
        assert!(shared.ltr_usable());
        shared.pending.lock().acked = vec![12];
        shared.keyframe_bytes.store(50_000, Ordering::Relaxed);
        shared.keyframe_deferred_us.store(1, Ordering::Relaxed);
        shared.rebuilt();
        assert!(!shared.ltr_usable(), "a new session predicts from none of the old references");
        assert!(shared.pending.lock().acked.is_empty(), "nor is it told about them");
        assert!(shared.pending.lock().keyframe, "and it starts on a keyframe");
        assert_eq!(shared.keyframe_bytes.load(Ordering::Relaxed), 0, "of a size not yet known");
        assert_eq!(shared.keyframe_deferred_us.load(Ordering::Relaxed), 0);
        shared.report(&acking(&[12]), None);
        assert!(shared.pending.lock().acked.is_empty(), "the old session's token is dropped");
    }

    /// The last capture of a scroll arrives inside the cadence's period, so the gate holds it
    /// back; ScreenCaptureKit sends nothing more for a still picture, so the held capture is
    /// what goes out once the picture has gone quiet. A refresh asked for on the still picture
    /// is answered from it too, as a refresh.
    #[test]
    fn a_still_picture_sends_its_held_capture_when_owed_or_asked() {
        let (shared, _wire) = shared_for_frames();
        shared.fps.store(30, Ordering::Relaxed);
        let mut frame = a_frame();
        let base = host_now_us();
        frame.capture_ts_us = base;
        shared.on_frame(again(&frame));
        assert_eq!(shared.last_encoded_us.load(Ordering::Relaxed), base, "the first is due");
        assert_eq!(shared.repair_at(), None, "nothing owed");

        // The scroll's last frame, one display beat later: inside the 30 fps period.
        frame.capture_ts_us = base + 16_667;
        shared.on_frame(again(&frame));
        assert!(shared.owed.load(Ordering::Relaxed), "held back by the cadence");
        let at = shared.repair_at().expect("owed");
        // Due at the cadence (29.2 ms after the last) and once quiet (25 ms after the capture).
        assert_eq!(at, base + 16_667 + 25_000);
        assert_eq!(shared.repair_now(at), Attempt::Sent);
        assert_eq!(shared.last_encoded_us.load(Ordering::Relaxed), at, "stamped when it went");
        assert!(!shared.owed.load(Ordering::Relaxed));
        assert_eq!(shared.stats().repaired, 1);
        assert_eq!(shared.repair_at(), None, "sent once");

        // A refresh on the still picture: answered from the held capture a period on.
        shared.request_refresh(3, false);
        let refresh_at = shared.repair_at().expect("a refresh is owed");
        assert_eq!(refresh_at, at + 33_333 - 4_166);
        assert_eq!(shared.repair_now(refresh_at), Attempt::Sent);
        assert!(!shared.pending.lock().refresh, "the refresh went out with it");
        assert_eq!(shared.stats().repaired, 2);

        // A still target that is hidden is not repaired: the held capture is dropped.
        shared.request_refresh(4, false);
        shared.target_hidden.store(true, Ordering::Relaxed);
        assert_eq!(shared.repair_now(refresh_at + 100_000), Attempt::Nothing);
        assert_eq!(shared.repair_at(), None, "nothing held any more");
    }

    /// While the picture keeps changing a held-back capture is overtaken by the next one, which
    /// is fresher; the repair waits for the quiet so it never sends the older frame instead.
    #[test]
    fn a_moving_picture_is_never_repaired_ahead_of_its_next_capture() {
        let base = 10_000_000;
        // 30 fps cadence, 60 Hz capture: the capture at +16.7 ms is owed; the one at +33.3 ms
        // would be due at +29.2 ms, but the repair waits to +41.7 ms, past the next capture.
        let owed = Asks { owed: true, ..Asks::default() };
        let keyframe = Asks { keyframe: true, ..Asks::default() };
        let at = repair_at(owed, base + 16_667, base, 30, 60);
        assert_eq!(at, Some(base + 16_667 + 25_000));
        // A 120 Hz ceiling on a 60 Hz panel waits as long, not 12.5 ms.
        assert_eq!(repair_at(owed, base, base, 120, 120), Some(base + 25_000));
        // A keyframe does not wait for the cadence, only for the quiet.
        assert_eq!(repair_at(keyframe, base, base, 15, 60), Some(base + 25_000));
        assert_eq!(repair_at(Asks::default(), base, base, 60, 60), None, "nothing owed");
    }

    /// The parity rides the same link as the frame, so the encoder gets the target less its
    /// share.
    #[test]
    fn the_encoder_gets_the_target_less_the_parity_share() {
        assert_eq!(encoder_bps(12_000_000, 0), 12_000_000);
        assert_eq!(encoder_bps(12_000_000, 200), 10_000_000, "the default 20 %");
        assert_eq!(encoder_bps(12_000_000, 500), 8_000_000, "the 50 % ceiling");
        assert_eq!(encoder_bps(u32::MAX, 0), u32::MAX);
    }

    /// A link simulated a millisecond at a time: QUIC's datagram queue in order, `rate` bytes
    /// leaving each millisecond.
    struct Link {
        queue: Mutex<VecDeque<Bytes>>,
    }

    impl Link {
        fn drain(&self, mut bytes: usize) -> Vec<Bytes> {
            let mut queue = self.queue.lock();
            let mut out = Vec::new();
            while let Some(front) = queue.front() {
                if front.len() > bytes {
                    break;
                }
                bytes = bytes.saturating_sub(front.len());
                out.extend(queue.pop_front());
            }
            drop(queue);
            out
        }
    }

    impl DatagramSink for Link {
        fn send(&self, datagrams: &[Bytes]) -> Result<(), Refused> {
            self.queue.lock().extend(datagrams.iter().cloned());
            Ok(())
        }

        fn max_size(&self) -> Option<usize> {
            Some(MAX_DATAGRAM)
        }

        fn held(&self) -> usize {
            self.queue.lock().iter().map(Bytes::len).sum()
        }

        fn cwnd(&self) -> u64 {
            0
        }

        fn is_closed(&self) -> bool {
            false
        }
    }

    /// Milliseconds an audio packet sent `after_ms` into a 130 kB keyframe waits in QUIC's
    /// queue on a `rate` bytes-a-millisecond link, and when the keyframe's last byte leaves;
    /// with `lane` the audio lane is in use (audio flowing), without it every datagram goes
    /// straight in.
    fn audio_behind_a_keyframe(rate: usize, target_bps: u64, lane: bool) -> (usize, usize) {
        let link = Arc::new(Link { queue: Mutex::new(VecDeque::new()) });
        let sink: Arc<dyn DatagramSink> = Arc::<Link>::clone(&link);
        let shared = Shared::<Native>::new(StreamId(1), sink, 30_000_000, 60, false);
        shared.counters.bitrate_bps.store(target_bps, Ordering::Relaxed);
        let base = 1_000_000_u64;
        shared.last_audio_us.store(if lane { base } else { 0 }, Ordering::Relaxed);
        let keyframe: Vec<Bytes> =
            std::iter::repeat_n(Bytes::from(vec![1_u8; 1_150]), 113).collect();
        shared.send_video(&keyframe, base);
        let audio = Bytes::from_static(&[9_u8; 160]);
        let (mut audio_wait, mut last_video) = (0, 0);
        let mut audio_sent_at = None;
        for ms in 1..400_usize {
            let now = base.saturating_add(u64::try_from(ms).unwrap_or(0).saturating_mul(1_000));
            for gone in link.drain(rate) {
                if gone.len() == audio.len() {
                    audio_wait = ms.saturating_sub(audio_sent_at.unwrap_or(ms));
                } else {
                    last_video = ms;
                }
            }
            if ms == 5 {
                shared.last_audio_us.store(if lane { now } else { 0 }, Ordering::Relaxed);
                shared.send(std::slice::from_ref(&audio));
                audio_sent_at = Some(ms);
            }
            let mut lane = shared.lane.lock();
            if !lane.queue.is_empty() {
                shared.pump(&mut lane, now);
            }
        }
        (audio_wait, last_video)
    }

    /// A platform whose video encoder records what each of its sessions was handed.
    enum Recording {}

    impl Platform for Recording {
        type Audio = slopty_codec::Opus;
        type Capture = slopty_capture::ScreenCaptureKit;
        type Input = slopty_input::CgEvents;
        type Video = Recorder;
    }

    /// Frames handed to an encoder: the session, and whether a keyframe was forced.
    type Log = Arc<Mutex<Vec<(u64, bool)>>>;

    struct Recorder {
        session: u64,
        log: Log,
    }

    impl slopty_codec::VideoEncoder for Recorder {
        type Image = slopty_codec::PixelBuffer;

        fn new(
            _config: EncoderConfig,
            _sink: impl Fn(EncodedPacket) + Send + Sync + 'static,
        ) -> Result<Self, CodecError> {
            Ok(Self { session: 0, log: Log::default() })
        }

        fn encode(
            &self,
            _image: &Self::Image,
            _pts_us: u64,
            options: &FrameOptions,
        ) -> Result<(), CodecError> {
            self.log.lock().push((self.session, options.force_keyframe));
            Ok(())
        }

        fn set_bitrate(&self, _bps: u32) -> Result<(), CodecError> {
            Ok(())
        }

        fn set_frame_rate(&self, _fps: u16) -> Result<(), CodecError> {
            Ok(())
        }
    }

    /// A rebuild swaps the encoder and resets what described the old session in one step. An
    /// encode caught between the two reached the new session with the old one's requests (no
    /// keyframe), and the rebuild's keyframe then went out as a second IDR. Here the rebuild is
    /// held where it resets the book, and an encode is tried beside it.
    #[test]
    fn a_rebuild_is_one_step_for_an_encode_beside_it() {
        let wire = Wire::new();
        let sink: Arc<dyn DatagramSink> = Arc::<Wire>::clone(&wire);
        let shared = Arc::new(Shared::<Recording>::new(StreamId(1), sink, 8_000_000, 60, false));
        let log = Log::default();
        shared.install(Recorder { session: 1, log: Arc::clone(&log) });
        shared.pending.lock().keyframe = false;
        *shared.held.lock() = Some(a_frame());
        shared.owed.store(true, Ordering::Relaxed);

        let book = shared.ltr.lock();
        let rebuild = std::thread::spawn({
            let shared = Arc::clone(&shared);
            let log = Arc::clone(&log);
            move || shared.install(Recorder { session: 2, log })
        });
        let deadline = Instant::now().checked_add(Duration::from_secs(5)).unwrap();
        let swapping = || {
            shared.encoder.is_locked_exclusive()
                || shared
                    .encoder
                    .try_read()
                    .is_some_and(|e| e.as_ref().is_some_and(|e| e.session == 2))
        };
        while !swapping() {
            assert!(Instant::now() < deadline, "the rebuild never started");
            std::thread::yield_now();
        }
        let (done_tx, done) = std::sync::mpsc::channel();
        let encode = std::thread::spawn({
            let shared = Arc::clone(&shared);
            move || {
                let held = shared.held.lock();
                let attempt = shared.try_encode(held.as_ref(), false, 1_000_000);
                drop(held);
                let _sent = done_tx.send(attempt);
            }
        });
        // Finished while the rebuild is held: it ran between the swap and the reset.
        let between = done.recv_timeout(Duration::from_millis(200)).is_ok();
        drop(book);
        rebuild.join().unwrap();
        encode.join().unwrap();
        assert!(!between, "an encode ran between the swap and the reset: {:?}", log.lock());
        assert_eq!(
            *log.lock(),
            vec![(2, true)],
            "the new session starts on the rebuild's keyframe"
        );
        assert!(!shared.pending.lock().keyframe, "and no second one is pending");
    }

    /// A datagram the transport refused never left, so it says nothing of how fast the link
    /// drains: the lane expects QUIC to hold only what it took. Counting the refused ones read
    /// as bytes drained at the next look and widened the slice.
    #[test]
    fn a_refused_datagram_is_not_counted_as_sent() {
        let (shared, wire) = shared_for_frames();
        wire.max.store(500, Ordering::Relaxed);
        let small = Bytes::from(vec![1_u8; 400]);
        let large = Bytes::from(vec![2_u8; 1_000]);
        // Straight to QUIC, no audio flowing.
        shared.send_video(&[small.clone(), large.clone()], 1_000);
        assert_eq!(shared.lane.lock().expected, 400, "the one that fit");
        // Through the lane.
        let mut lane = shared.lane.lock();
        lane.expected = 0;
        lane.rate = 100_000_000;
        lane.push(&[small.clone(), large, small]);
        shared.pump(&mut lane, 1_000);
        assert!(lane.queue.is_empty(), "the refused one is not retried");
        assert_eq!(lane.expected, 800, "the two that fit");
        drop(lane);
        assert_eq!(wire.drain().len(), 3);
    }

    /// The number behind the lane. On a 20 Mbit/s link (2.5 kB a millisecond) a 130 kB keyframe
    /// is 52 ms of queue, and audio sent into its middle waited for all of what was left. With
    /// the lane it waits behind a slice, and the keyframe still leaves when the link would have
    /// let it; on a link forty times faster the lane learns the rate and costs the keyframe a
    /// few ticks at most.
    #[test]
    fn audio_waits_behind_a_slice_of_a_keyframe_not_all_of_it() {
        let (fifo_wait, fifo_done) = audio_behind_a_keyframe(2_500, 20_000_000, false);
        let (lane_wait, lane_done) = audio_behind_a_keyframe(2_500, 20_000_000, true);
        eprintln!(
            "20 Mbit/s: audio waits {fifo_wait} ms → {lane_wait} ms, keyframe done at {fifo_done} → {lane_done} ms"
        );
        assert!(fifo_wait >= 45, "the FIFO baseline: {fifo_wait} ms");
        assert!(lane_wait <= 6, "behind a 5 ms slice: {lane_wait} ms");
        assert!(
            lane_done <= fifo_done + 1,
            "the keyframe is not slowed: {lane_done} against {fifo_done} ms"
        );

        let (_fast_fifo_wait, fast_fifo_done) = audio_behind_a_keyframe(100_000, 20_000_000, false);
        let (fast_wait, fast_done) = audio_behind_a_keyframe(100_000, 20_000_000, true);
        eprintln!(
            "800 Mbit/s: audio waits {fast_wait} ms, keyframe done at {fast_fifo_done} → {fast_done} ms"
        );
        assert!(
            fast_done <= fast_fifo_done + 3,
            "a fast link: {fast_done} against {fast_fifo_done} ms"
        );
    }
}
