//! Remote window streaming: one pipeline per open stream.
//!
//! ```text
//! ScreenCaptureKit queue ──frame──▶ mailbox ──encode thread──▶ encoder.encode
//!     ──VideoToolbox callback (often inside the submit)──▶ packetize ──▶ sink
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
//!
//! [`synthetic::Drawn`] and [`synthetic::Synthetic`] are platforms whose pictures are drawn
//! instead of captured: the first times this path end to end where ScreenCaptureKit cannot run,
//! the second is the whole screen a worker serves under test ([`synthetic_screen`]).
//!
//! # Locks
//!
//! An encode (`Shared::try_encode`) takes the stream's turn (`Gate`) from reading the requests
//! to the end of the submit, so encodes go one at a time. It reads and writes `held` and then
//! `encoder` under their locks, lets both go, and only then calls into VideoToolbox: what it
//! tells the session (bitrate, frame rate, layers) and the submits. No lock is held across a
//! call into the encoder, so a call that never returns holds nothing anyone waits on but the
//! turn, and the beat takes the turn from it (`ENCODE_STUCK`, `Shared::unstick`). A session
//! whose sides are multiples of 16 codes the frame inside the submit and runs its output
//! callback there, on the encoding thread, before the submit returns. The callback path
//! (`on_session_packet` → `on_packet`) takes `counters.in_flight`, `counters.encode`, `watch`,
//! `refine`, `rate` and `ltr` one at a time, then `packetizer` → `lane` → the sink. `refine` is
//! a leaf: whoever takes it takes nothing else under it. It moves the rung in atomics; the next
//! encode tells the session.
//!
//! Nothing but an encode waits for the turn, so no runtime task waits on one (15 ms at
//! 3024 × 1968, about 38 at 5K). The runtime reads the held capture's time from `held_us`,
//! leaves a new session in `staged` for the next encode to put in, and leaves the bitrate and
//! the rung in atomics for the next encode to tell the session. The capture leaves frames in
//! the mailbox. A rebuild that waited on the session behind an encode, while that encode's
//! callback waited to tell the session a new rung, was a deadlock (MEASUREMENTS.md, "Stream
//! sides padded to 16").
//!
//! Where two are held together the order is the turn → `held` → `encoder` → any of `staged`,
//! `ltr` → `pending`, `rate`, `watch`, `counters.*` and `packetizer` → `lane`. `rate` is never
//! held while another is taken, nor while anything is asked of the session. Every other lock is
//! taken alone.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use bytes::Bytes;
use parking_lot::Mutex;
#[cfg(all(test, target_vendor = "apple"))]
use slopty_capture::host_now_us;
use slopty_capture::{
    AxError, CaptureConfig, CaptureError, CaptureSource, CapturedFrame, Console, Crop,
    FocusedField, Heard, PixelFormat, Rect, TargetWindow, Went, WindowState, crop_for,
};
use slopty_codec::{
    CodecError, EncodedPacket, EncoderConfig, FrameOptions, VideoEncoder as _, conformance,
};
use slopty_core::{DisplayId, StreamId};
use slopty_input::{InputError, InputSink as _, Pointer, PointerChanges, PointerWatch};
pub use slopty_media::PathSample;
use slopty_media::{
    Cadence, ChromaGate, Decision, EncodedFrame, EncoderWatch, Fed, HEARTBEAT_AFTER, LayerGate,
    MediaError, Pace, Packetizer, RateController, Redundancy, Refine, cursor_datagram,
    heartbeat_datagram, slower_rung,
};
use slopty_proto::ctl::{LtrStats, Quantiles, ScreenStats, ScreenSummary};
use slopty_proto::media::{ClockEcho, MAX_DATAGRAM};
use slopty_proto::screen::{
    CaptureTarget, Caret, Chroma, CursorShape, Quality, ReceiverReport, Region, ScreenEvent,
    ScreenInput, SourceState, Stripe, TextField, VideoCodec,
};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::platform::{Native, Platform};

pub mod drag;
mod engines;
mod region;
pub mod sized;
pub mod sound;
mod stripes;
#[cfg(target_os = "macos")]
pub mod synthetic;

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

/// Cursor sample period (120 Hz) while the real pointer is the one shown; a datagram goes out
/// only when the position changed.
const CURSOR_PERIOD: Duration = Duration::from_micros(8_333);
/// How long the cursor loop sleeps when nothing wakes it: a placed pointer, or a target that
/// is hidden or not yet probed, moves only on an event, and this only looks at whether the
/// stream has closed.
const CURSOR_BACKSTOP: Duration = Duration::from_secs(1);
/// How long a new size must hold before the stream is rebuilt for it ([`ResizeDebounce`]).
const RESIZE_HOLD: Duration = Duration::from_millis(100);
/// How often the cursor's seed is read while the pointer is over the target (120 Hz): a shape
/// change is seen within this, and the picture is read only when the seed moved. It goes to
/// the client only when it changed.
const SHAPE_PERIOD: Duration = Duration::from_micros(8_333);

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
    /// This process may not record the screen (`slopty worker doctor` says how to grant it).
    #[error("this worker has no Screen Recording permission")]
    NotPermitted,
    /// A callback-based framework call never completed.
    #[error("screen pipeline closed")]
    Closed,
    /// ScreenCaptureKit lists no display to stream.
    #[error("no display to stream")]
    NoDisplay,
    /// Only a display stream moves to another display.
    #[error("not a display stream")]
    NotDisplay,
    /// The stream's encode thread could not be started.
    #[error("the encode thread would not start: {0}")]
    EncodeThread(std::io::Error),
    /// A session build stayed inside VideoToolbox past its patience (`BUILD_STUCK`) and was
    /// given up on; its thread is left there.
    #[error("VideoToolbox did not answer a session build in {0:?}")]
    BuildStuck(Duration),
    /// A session build's thread ended without an answer: it would not start, or it panicked.
    #[error("the session build's thread ended without an answer")]
    BuildLost,
}

impl ScreenError {
    /// What the client is told of it: the kinds it answers differently, else these words.
    #[must_use]
    pub fn failure(&self) -> slopty_proto::screen::ScreenFailure {
        use slopty_proto::screen::ScreenFailure;
        /// ScreenCaptureKit's "the user declined": no Screen Recording grant.
        const DECLINED: i64 = -3801;
        match self {
            Self::NotPermitted | Self::Capture(CaptureError::Sck { code: DECLINED, .. }) => {
                ScreenFailure::NotPermitted
            }
            Self::WindowGone | Self::NoDisplay | Self::Capture(CaptureError::NotFound(_)) => {
                ScreenFailure::Gone
            }
            Self::Capture(CaptureError::Unsupported) => ScreenFailure::Unsupported,
            other => ScreenFailure::Failed(other.to_string()),
        }
    }
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

/// Frames an encoder session codes before it may write temporal layers: five seconds at 60.
/// Switched on after the keyframe alone, the session lost 8 dB with room and 17 where the rate
/// bound, for all the 600 frames measured after; after ten, 29 % more bytes with room and 16 dB
/// where the rate bound; after 60 it was still 1.7 dB short where the rate bound; after 300 it
/// matched a session switched after eleven seconds (MEASUREMENTS.md, "temporal layers switched on a
/// live session").
const LAYERS_AFTER_FRAMES: u64 = 300;

/// The most a still picture's refinement frames may take together, in milliseconds of the
/// encoder's rate: half a second. The measured 8 Mbit/s refinements took 170 kB in twelve frames
/// (MEASUREMENTS.md, "a still picture refined"), under this at 500 kB.
const REFINE_BUDGET_MS: u64 = 500;

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

/// How long a keyframe handed to the encoder answers the refreshes asked for before it comes out.
///
/// A request that arrives while a keyframe is being encoded was sent before the client could have
/// had it, and that keyframe answers it; setting the request again had the encoder make a second
/// one behind the first. The first keyframe of a session takes 60–130 ms at 3024 × 1964, longer
/// than the client's first refresh repeat, so every stream that opened at that size encoded two
/// (MEASUREMENTS.md, "a refresh asked for while the keyframe is encoded"). Past this the keyframe
/// is taken as dropped by the encoder and a request is answered again.
const KEYFRAME_IN_FLIGHT_US: u64 = 400_000;

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
/// keyframe is pending, or the still picture is worth coding again ([`Refine`]), from `refine_us`
/// on.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Asks {
    owed: bool,
    refresh: bool,
    keyframe: bool,
    refine: Option<u64>,
}

/// When the held capture should be encoded, if nothing newer arrives first; `None` when
/// nothing is owed.
///
/// `asks` says what the held capture could answer. The capture at `captured_us` goes out once the
/// picture has gone quiet ([`quiet_after_us`]) and, unless a keyframe is wanted, once the
/// cadence gate lets a capture through again at `due_us` ([`Pace::due_at`]): the same gate a
/// fresh capture passes, so a repair never spends more than the rung allows.
///
/// A refinement of the still picture waits for the quiet, the cadence, and its own spacing
/// ([`Refine::spacing_us`]).
const fn repair_at(asks: Asks, captured_us: u64, due_us: u64, ceiling_fps: u16) -> Option<u64> {
    let refine_us = match asks.refine {
        Some(at) => at,
        None if !asks.owed && !asks.refresh && !asks.keyframe => return None,
        None => 0,
    };
    let quiet = captured_us.saturating_add(quiet_after_us(ceiling_fps));
    if asks.keyframe {
        return Some(quiet);
    }
    let at = if due_us > quiet { due_us } else { quiet };
    let refining = !asks.owed && !asks.refresh;
    Some(if refining && refine_us > at { refine_us } else { at })
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
    /// What the Mac's screens show of the session, as the geometry probe last read it.
    console: Console,
    /// `encoded` when a frame was last seen, and when that was.
    seen: u64,
    last_frame: Option<Instant>,
    opened_at: Instant,
}

impl SourceTracker {
    /// A tracker for a stream opened at `now`.
    #[must_use]
    pub const fn new(now: Instant) -> Self {
        Self { reported: None, seen: 0, last_frame: None, opened_at: now, console: Console::Shown }
    }

    /// What the Mac's screens show of the session now: a locked Mac, or a session off the
    /// screens, is what the client is told from the next [`Self::poll`] on, whatever the target
    /// draws, since nothing of it can be seen there.
    pub const fn set_console(&mut self, console: Console) {
        self.console = console;
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
            _ if self.console == Console::Locked => SourceState::Locked,
            _ if self.console == Console::Away => SourceState::Away,
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

    /// The client was last told the source is idle.
    #[must_use]
    pub fn idle(&self) -> bool {
        self.reported == Some(SourceState::Idle)
    }
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

/// Whether this worker serves the drawn screen of [`synthetic::Synthetic`] in place of its own.
///
/// [`synthetic::SWITCH`] turns it on, a test knob. Every stream the worker opens, its listing,
/// its warm-up and its window resizes then go there.
#[must_use]
#[cfg_attr(
    not(target_os = "macos"),
    expect(clippy::missing_const_for_fn, reason = "on macOS it reads the drawn screen's switch")
)]
pub fn synthetic_screen() -> bool {
    #[cfg(target_os = "macos")]
    {
        synthetic::serving()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Start and stop one small capture of the first display; returns how long that took.
///
/// The first stream a client opens then does not pay ScreenCaptureKit's first start in this
/// process: ~300 ms cold against ~115 ms warm (MEASUREMENTS.md, "start-up on a cold
/// connection").
pub async fn warm_up() -> Result<Duration, ScreenError> {
    #[cfg(target_os = "macos")]
    if synthetic::serving() {
        return Pipeline::<synthetic::Synthetic>::warm_up().await;
    }
    ScreenStream::warm_up().await
}

/// The `Listing` event for the current windows and displays.
pub async fn listing() -> Result<ScreenEvent, ScreenError> {
    #[cfg(target_os = "macos")]
    if synthetic::serving() {
        return Pipeline::<synthetic::Synthetic>::listing().await;
    }
    ScreenStream::listing().await
}

/// A coder's session in force, and what it was last told ([`Shared::tell`]).
struct Live<V> {
    /// Shared with the encode's calls into it for the length of its turn, the lower stripe's on
    /// its [`Helper`]; a turn given up on there keeps its own reference.
    session: Option<Arc<V>>,
    /// The bitrate it was last set to; 0 for the one it was built with.
    bps: u32,
    /// The frame rate it was last set to; 0 for the one it was built with.
    fps: u16,
    /// Whether it was last told to write temporal layers; a session is built without them.
    layers: bool,
    /// Frames it was handed; layers wait for [`LAYERS_AFTER_FRAMES`] of them.
    frames: u64,
    /// Frames it had dropped when it was last asked.
    dropped: u64,
}

impl<V> Live<V> {
    const fn new() -> Self {
        Self { session: None, bps: 0, fps: 0, layers: false, frames: 0, dropped: 0 }
    }
}

/// What an encode tells a session before its frame ([`Shared::tell`]); `None` for what is
/// unchanged.
#[derive(Clone, Copy, Default, Debug)]
struct Told {
    /// The session's share of the bitrate.
    bps: Option<u32>,
    /// The stream's whole bitrate the share is of.
    whole: u32,
    fps: Option<u16>,
    /// Whether it writes temporal layers.
    layers: Option<bool>,
}

/// The sessions in force for the stream's coders, and what codes the lower stripe: what an
/// encode reads and writes in its turn, and lets go before it calls into the encoder
/// ([`Shared::try_encode`]).
struct Lives<V> {
    /// The whole picture, or the top stripe.
    top: Live<V>,
    /// The lower stripe while the stream is striped; no session otherwise.
    lower: Live<V>,
    /// The stripes in force, top first; `None` while the stream is coded as one picture.
    layout: Option<[CodedStripe; 2]>,
    /// The thread that submits the lower stripe while the encode's own thread submits the top
    /// one; started with the stream's first stripes.
    helper: Option<Helper>,
}

impl<V> Lives<V> {
    /// How many coders code the stream: 1, or 2 while it is striped.
    const fn count(&self) -> usize {
        if self.layout.is_some() { Stripe::MAX } else { 1 }
    }

    /// Coder `index`'s session and what it was told.
    const fn live(&mut self, index: usize) -> &mut Live<V> {
        if index == 0 { &mut self.top } else { &mut self.lower }
    }

    /// The share of the stream's bitrate coder `index` gets: its shown rows over the picture's.
    fn share(&self, index: usize) -> (u32, u32) {
        let Some(layout) = &self.layout else { return (1, 1) };
        let [top, lower] = layout;
        let rows = top.shown_rows.saturating_add(lower.shown_rows).max(1);
        (if index == 0 { top.shown_rows } else { lower.shown_rows }, rows)
    }
}

/// Sessions built for the stream and not put in yet ([`Shared::install`]).
struct Staged<V> {
    top: (V, u64),
    lower: Option<(V, u64)>,
    layout: Option<[CodedStripe; 2]>,
}

/// What an encode takes out of [`Lives`] for its calls into the encoder ([`Shared::hand`]).
struct Handed<V> {
    /// The sessions of the coders with a frame to code, top first.
    sessions: [Option<Arc<V>>; Stripe::MAX],
    told: [Told; Stripe::MAX],
    /// The lower stripe's thread, while both stripes are coded.
    helper: Option<Helper>,
}

/// A job for the [`Helper`]: the lower stripe's submit.
type Job = Box<dyn FnOnce() -> Result<(), CodecError> + Send>;

/// The thread that submits the lower stripe of each capture while the encode thread submits the
/// top one, so the two engines code the two stripes at once.
///
/// An aligned low-latency session codes its frame inside the submit, so one thread submitting
/// both stripes would code them one after the other. The helper is told the job, the encode
/// thread submits its own stripe, then waits for the helper's answer: both are in before the
/// encode lets its locks go, as one submit was. Each stripe's packets go out from inside its
/// own submit, so neither waits for the other to be sent.
struct Helper {
    jobs: std::sync::mpsc::SyncSender<Job>,
    done: std::sync::mpsc::Receiver<Result<(), CodecError>>,
}

impl Helper {
    fn start(id: StreamId) -> Result<Self, ScreenError> {
        let (jobs, taken) = std::sync::mpsc::sync_channel::<Job>(1);
        let (answer, done) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name(format!("slopty-encode-{id}-lower"))
            .spawn(move || {
                slopty_platform::user_interactive_thread();
                for job in taken {
                    if answer.send(job()).is_err() {
                        break;
                    }
                }
            })
            .map_err(ScreenError::EncodeThread)?;
        Ok(Self { jobs, done })
    }

    /// Hand the helper `job`; `false` when its thread is gone.
    fn submit(&self, job: Job) -> bool {
        self.jobs.send(job).is_ok()
    }

    /// The answer to the job last handed over.
    fn wait(&self) -> Result<(), CodecError> {
        self.done
            .recv()
            .unwrap_or(Err(CodecError::Os { call: "the lower stripe's thread", status: -1 }))
    }
}

/// Where a stripe sits in the stream's padded picture ([`slopty_codec::stripes::layout`]).
type CodedStripe = slopty_codec::stripes::Stripe;

/// A frame handed to a session, until it comes back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Submitted {
    /// Its presentation time, which names it in the session's callback.
    pts_us: u64,
    /// When it went in.
    at_us: u64,
    /// The stripes coded from its capture.
    stripes: u8,
    /// The region of the target its capture shows; `None` for all of it.
    region: Option<Region>,
}

/// Requests folded into the next encoded frame.
#[derive(Default)]
struct Pending {
    keyframe: bool,
    refresh: bool,
    acked: Vec<u64>,
}

/// One coded picture of a stream: the whole picture, or one of its two stripes, each a media
/// stream of its own on the wire ([`slopty_proto::screen::Stripe`]).
///
/// Everything that describes one encoder session and the frames it made is here: the requests
/// for its next frame, its long-term references, its frame numbers and parity, and its
/// keyframes. A refresh or a NACK on a stripe's media stream is answered by that stripe alone.
struct Coder {
    /// Stream id on the wire.
    media: StreamId,
    pending: Mutex<Pending>,
    packetizer: Mutex<Packetizer>,
    redundancy: Mutex<Redundancy>,
    /// The long-term references this coder's session offered and the client acknowledged.
    ltr: Mutex<LtrBook>,
    /// The session whose packets are this coder's, as [`Shared::install`] put it in; a
    /// replaced session's late packets are dropped ([`Shared::on_session_packet`]).
    session: AtomicU64,
    /// What a keyframe costs on this coder, as [`keyframe_estimate`] tracks it; `0` until one
    /// has been encoded.
    keyframe_bytes: AtomicU64,
    /// `host_now_us()` when the current run of deferrals began, `0` when none is running: the
    /// valve's clock.
    keyframe_deferred_us: AtomicU64,
    /// `host_now_us()` when a keyframe was handed to the session that has not come out yet, `0`
    /// when none is in flight ([`KEYFRAME_IN_FLIGHT_US`]).
    keyframe_submitted_us: AtomicU64,
    /// Whether the still picture is worth coding again, and how far apart ([`Refine`]). Taken
    /// alone, or last under an encode's locks.
    refine: Mutex<Refine>,
    /// Wire bytes of the refinement frame last packetized, until a frame that is not one is:
    /// the one queue a refinement may leave ahead of the next change, which the guard lets
    /// that change pass ([`Shared::frame_fits`]).
    refine_wire: AtomicU64,
    /// Frames inside the session, oldest first.
    in_flight: Mutex<VecDeque<Submitted>>,
    /// `datagrams_sent` at the previous receiver report.
    sent_at_report: AtomicU64,
}

impl Coder {
    fn new(media: StreamId) -> Self {
        Self {
            media,
            pending: Mutex::new(Pending { keyframe: true, ..Pending::default() }),
            packetizer: Mutex::new(Packetizer::new(media)),
            redundancy: Mutex::new(Redundancy::new()),
            ltr: Mutex::new(LtrBook::default()),
            session: AtomicU64::new(0),
            keyframe_bytes: AtomicU64::new(0),
            keyframe_deferred_us: AtomicU64::new(0),
            keyframe_submitted_us: AtomicU64::new(0),
            refine: Mutex::new(Refine::default()),
            refine_wire: AtomicU64::new(0),
            in_flight: Mutex::new(VecDeque::with_capacity(IN_FLIGHT_MAX)),
            sent_at_report: AtomicU64::new(0),
        }
    }

    /// A frame went into the session as `submitted` says.
    fn submitted(&self, submitted: Submitted) {
        let mut in_flight = self.in_flight.lock();
        if in_flight.len() >= IN_FLIGHT_MAX {
            in_flight.pop_front();
        }
        in_flight.push_back(submitted);
    }

    /// The session returned the frame with `pts_us` at `now`: its encode latency, and its
    /// submission, when that is on record.
    fn returned(&self, pts_us: u64, now: u64) -> Option<(u64, Submitted)> {
        let submitted = {
            let mut in_flight = self.in_flight.lock();
            let at = in_flight.iter().position(|frame| frame.pts_us == pts_us);
            let found = at.and_then(|i| in_flight.remove(i));
            drop(in_flight);
            found
        };
        submitted.map(|frame| (now.saturating_sub(frame.at_us), frame))
    }

    /// Whether a refresh would come back as a delta: an acknowledged reference newer than the
    /// latest keyframe is on record.
    fn ltr_usable(&self) -> bool {
        self.ltr.lock().usable.is_some()
    }

    /// A new session replaced the old one: its references, the acknowledgements still queued
    /// for them, the keyframe estimate and the valve's clock all described the old session. The
    /// new session starts on a keyframe. The book stays locked while the queued
    /// acknowledgements go, as [`Shared::report`] holds it while queueing them, so a report is
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
        self.refine.lock().rebuilt();
        self.keyframe_bytes.store(0, Ordering::Relaxed);
        self.keyframe_deferred_us.store(0, Ordering::Relaxed);
        self.keyframe_submitted_us.store(0, Ordering::Relaxed);
    }
}

/// The stripes of one capture still to come back from their sessions, for what the stream
/// counts once a capture (its encode time, the slowest stripe's; its latency; the watch).
#[derive(Clone, Copy, Default, Debug)]
struct Join {
    pts: u64,
    /// Coders still to return it, bit `i` for coder `i`.
    waiting: u8,
    /// The slowest encode so far.
    took: Option<u64>,
    keyframe: bool,
}

impl Join {
    /// Coder `index` returned the frame `pts` after `took`: the capture's slowest encode and
    /// whether any of its stripes was a keyframe once it was the last, else `None`.
    fn returned(
        &mut self,
        index: usize,
        pts: u64,
        took: Option<u64>,
        keyframe: bool,
    ) -> Option<(Option<u64>, bool)> {
        let bit = 1_u8 << index;
        if pts != self.pts || self.waiting & bit == 0 {
            return None;
        }
        self.waiting &= !bit;
        self.took = self.took.max(took);
        self.keyframe |= keyframe;
        (self.waiting == 0).then_some((self.took, self.keyframe))
    }
}

struct Counters {
    captured: AtomicU64,
    /// Captures a newer one replaced in the mailbox before the encode thread took them.
    superseded: AtomicU64,
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
    /// Refinement frames of a still picture ([`Refine`]).
    refined: AtomicU64,
    laned: AtomicU64,
    ltr_offered: AtomicU64,
    ltr_acked: AtomicU64,
    refreshes_idr: AtomicU64,
    refreshes_delta: AtomicU64,
    /// Encodes given up on inside the encoder, their sessions replaced ([`Shared::unstick`]).
    encoders_replaced: AtomicU64,
    capture: Mutex<LatencyRing>,
    encode: Mutex<LatencyRing>,
    beat_gap: Mutex<LatencyRing>,
    beat_gap_worst_us: AtomicU64,
    bounds: Mutex<LatencyRing>,
    /// Bytes of the frames of the source the encoder made, refinements aside: what the stripe
    /// gate reads the encoder's spend from ([`stripes::Gate`]).
    video_bytes: AtomicU64,
}

/// Frames the encoder may hold before the oldest submit record is forgotten (the encoder
/// runs with `MaxFrameDelayCount` 0, so this is only a bound against a callback that never
/// comes).
const IN_FLIGHT_MAX: usize = 16;

impl Counters {
    fn new() -> Self {
        Self {
            captured: AtomicU64::new(0),
            superseded: AtomicU64::new(0),
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
            refined: AtomicU64::new(0),
            laned: AtomicU64::new(0),
            ltr_offered: AtomicU64::new(0),
            ltr_acked: AtomicU64::new(0),
            refreshes_idr: AtomicU64::new(0),
            refreshes_delta: AtomicU64::new(0),
            encoders_replaced: AtomicU64::new(0),
            capture: Mutex::new(LatencyRing::default()),
            encode: Mutex::new(LatencyRing::default()),
            beat_gap: Mutex::new(LatencyRing::default()),
            beat_gap_worst_us: AtomicU64::new(0),
            bounds: Mutex::new(LatencyRing::default()),
            video_bytes: AtomicU64::new(0),
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
            encoders_replaced: self.encoders_replaced.load(Ordering::Relaxed),
            refined: self.refined.load(Ordering::Relaxed),
            superseded: self.superseded.load(Ordering::Relaxed),
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
            audio_packets: 0,
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
/// busy rebuilding its encoder or reading the window server. It names no platform, so one
/// connection holds the controls of streams on either of the platforms a worker may serve
/// ([`synthetic_screen`]).
///
/// A striped stream's lower stripe is a media stream of its own: its feedback reaches the
/// stream through [`Self::of_media`], which answers it on that stripe alone.
#[derive(Clone)]
pub struct StreamControl {
    stream: Arc<dyn Controlled>,
    /// The coder the feedback is about: 0 for the whole picture or the top stripe.
    coder: usize,
}

/// What the client's feedback does to a stream, whatever platform it runs on.
trait Controlled: Counted {
    fn take_report(
        &self,
        coder: usize,
        report: &ReceiverReport,
        path: Option<PathSample>,
    ) -> Option<Decision>;
    fn take_refresh(&self, coder: usize, last_good_frame: u32, keyframe: bool);
    fn take_nack(&self, coder: usize, frame: u32, fragments: &[u16]);
    fn take_clock(&self, sent_us: u64, arrived: Instant);
    fn zoom_now(&self) -> f64;
}

impl<P: Platform> Controlled for Shared<P> {
    fn take_report(
        &self,
        coder: usize,
        report: &ReceiverReport,
        path: Option<PathSample>,
    ) -> Option<Decision> {
        self.report(coder, report, path)
    }

    fn take_refresh(&self, coder: usize, last_good_frame: u32, keyframe: bool) {
        self.request_refresh(coder, last_good_frame, keyframe);
    }

    fn take_nack(&self, coder: usize, frame: u32, fragments: &[u16]) {
        self.nack(coder, frame, fragments);
    }

    fn take_clock(&self, sent_us: u64, arrived: Instant) {
        self.echo_clock(sent_us, arrived);
    }

    fn zoom_now(&self) -> f64 {
        self.zoom()
    }
}

impl std::fmt::Debug for StreamControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamControl")
            .field("stream", &self.stream.stream())
            .field("coder", &self.coder)
            .finish()
    }
}

impl StreamControl {
    /// This stream's control for the feedback that came on `media`, one of its media streams
    /// ([`Stripe::media_of`]): a lower stripe's NACKs, refreshes and reports are its own.
    #[must_use]
    pub fn of_media(&self, media: StreamId) -> Self {
        Self { stream: Arc::clone(&self.stream), coder: Stripe::stream_of(media).1 }
    }

    /// See [`ScreenStream::report`].
    #[must_use]
    pub fn report(&self, report: &ReceiverReport, path: Option<PathSample>) -> Option<Decision> {
        self.stream.take_report(self.coder, report, path)
    }

    /// See [`ScreenStream::request_refresh`].
    pub fn request_refresh(&self, last_good_frame: u32, keyframe: bool) {
        self.stream.take_refresh(self.coder, last_good_frame, keyframe);
    }

    /// See [`ScreenStream::nack`].
    pub fn nack(&self, frame: u32, fragments: &[u16]) {
        self.stream.take_nack(self.coder, frame, fragments);
    }

    /// Answer the client's clock probe stamped `sent_us`, which `arrived` then, at once on the
    /// connection's task. See [`ScreenStream::echo_clock`].
    pub fn clock(&self, sent_us: u64, arrived: Instant) {
        self.stream.take_clock(sent_us, arrived);
    }

    /// See [`ScreenStream::zoom`]; current across a rebuild, readable from the connection's task.
    #[must_use]
    pub fn zoom(&self) -> f64 {
        self.stream.zoom_now()
    }
}

/// How long one encode may stay inside the encoder before its sessions are given up on and new
/// ones built. A frame takes 5–23 ms one at a time on the M1 Max, and a session whose sides are
/// off 16 holds about five, so the longest a submit waited in any measurement was 110 ms
/// (MEASUREMENTS.md, "encode time against frame size"). Two seconds is eighteen times that and
/// 120 periods at 60 fps, and the client gives up on a decoder after the same two seconds
/// (`DECODE_STUCK`), so a frozen picture is bounded alike on both ends. On a hosted virtual Mac
/// one submit never returned (MEASUREMENTS.md, "an encode that never returned").
const ENCODE_STUCK: Duration = Duration::from_secs(2);
/// The most of an encode's time inside the encoder one look of the beat charges it: four
/// beats. A look later than that found the worker itself held up, as a starved machine holds it
/// for seconds, and an encode that waited out the same hold has not stopped.
const STUCK_CREDIT: Duration = Duration::from_millis(100);
/// The longest [`ENCODE_STUCK`] grows to while sessions in a row are given up on without a
/// frame: each one given up on leaves a thread waiting inside the encoder.
const ENCODE_STUCK_MAX: Duration = Duration::from_secs(32);

/// How long a session build may stay inside VideoToolbox, on the worker's own time, before the
/// wait for it is given up ([`Building::built`]). A build takes 3.5–42 ms (MEASUREMENTS.md,
/// "encoder sessions off the runtime"), and a process's first one, which also starts Metal's
/// device list, IOSurface's connection and, for the low-latency rate control's reaction
/// observer, the audio and camera device lists (`VCPReactionObserverCreate` under
/// `VTCompressionSessionCreate`), codes its first keyframe in 0.28–0.34 s. But while other
/// processes kept this Mac's engines busy, a first build passed 10 s and a test of one build
/// and one keyframe took 20.7 s, so a build is given up on only once nothing else could be
/// what holds it: a minute, past the slowest seen and past any test's bound on CI.
const BUILD_STUCK: Duration = Duration::from_secs(60);

/// How often the wait for a build looks at it, charging each look at most [`STUCK_CREDIT`]: the
/// beat's period, so a starved machine's late looks charge no more than the encode watch's do.
const BUILD_LOOK: Duration = HEARTBEAT_AFTER;

/// The one encode at a time, from reading its requests to the end of its submits: what keeps the
/// presentation times the encoder sees in order and a rebuild wholly before or after a frame.
///
/// It is not a lock held across the call into the encoder. An encode takes its turn, reads and
/// writes `held` and `encoder` under their locks, lets them go, and only then calls into
/// VideoToolbox. A call that does not return ([`ENCODE_STUCK`]) has its turn taken from it
/// ([`Shared::unstick`]), and the stream goes on without it.
#[derive(Default)]
struct Gate {
    turn: Mutex<Turn>,
    free: parking_lot::Condvar,
    /// The turn inside the encoder now, `0` while none is: what the beat's watch reads.
    inside: AtomicU64,
}

#[derive(Default)]
struct Turn {
    /// The turn being taken, `0` while the gate is free.
    holder: u64,
    /// The last turn handed out.
    issued: u64,
}

impl Gate {
    /// Wait for the gate, and take it: the turn's number.
    fn take(&self) -> u64 {
        let mut turn = self.turn.lock();
        while turn.holder != 0 {
            self.free.wait(&mut turn);
        }
        turn.issued = turn.issued.wrapping_add(1).max(1);
        turn.holder = turn.issued;
        turn.holder
    }

    /// Turn `n` calls into the encoder.
    fn enter(&self, n: u64) {
        self.inside.store(n, Ordering::Release);
    }

    /// Turn `n` is back from the encoder: whether it still holds the gate, `false` once it was
    /// given up on there.
    fn back(&self, n: u64) -> bool {
        let turn = self.turn.lock();
        let held = turn.holder == n;
        if held {
            self.inside.store(0, Ordering::Release);
        }
        drop(turn);
        held
    }

    /// Let the gate go after turn `n`, unless it was given up on.
    fn leave(&self, n: u64) {
        let mut turn = self.turn.lock();
        if turn.holder != n {
            return;
        }
        turn.holder = 0;
        drop(turn);
        self.free.notify_one();
    }

    /// Take the gate from turn `n`, still inside the encoder, running `first` before the next
    /// turn can be taken; whether it was.
    fn give_up(&self, n: u64, first: impl FnOnce()) -> bool {
        let mut turn = self.turn.lock();
        if n == 0 || turn.holder != n || self.inside.load(Ordering::Acquire) != n {
            return false;
        }
        first();
        self.inside.store(0, Ordering::Release);
        turn.holder = 0;
        drop(turn);
        self.free.notify_all();
        true
    }
}

/// How long the encode inside the encoder has been there on the worker's own time, from the
/// beat's looks ([`beat_loop`]), and how long it may be.
#[derive(Debug)]
struct StuckWatch {
    /// The turn seen inside at the last look; `0` for none.
    seen: u64,
    /// How long the beat ran on time since that turn went in, microseconds.
    charged_us: u64,
    /// When the watch last looked.
    looked_us: u64,
    /// How long a turn may stay: [`ENCODE_STUCK`], doubled for each in a row given up on whose
    /// sessions brought no frame.
    patience_us: u64,
    /// The stream's encoded frames when a turn was last given up on.
    encoded_at_give_up: u64,
}

impl StuckWatch {
    fn new(now_us: u64) -> Self {
        Self {
            seen: 0,
            charged_us: 0,
            looked_us: now_us,
            patience_us: micros(ENCODE_STUCK),
            encoded_at_give_up: 0,
        }
    }

    /// Look at `now_us`, turn `inside` inside the encoder (`0` for none): the turn to give up
    /// on, once it has been there its patience on time.
    fn look(&mut self, inside: u64, now_us: u64) -> Option<u64> {
        let ran = now_us.saturating_sub(self.looked_us).min(micros(STUCK_CREDIT));
        self.looked_us = now_us;
        if inside == 0 || inside != self.seen {
            self.seen = inside;
            self.charged_us = 0;
            return None;
        }
        self.charged_us = self.charged_us.saturating_add(ran);
        (self.charged_us >= self.patience_us).then_some(inside)
    }

    /// The turn seen was given up on with the stream at `encoded` frames: the next waits as
    /// long again if the sessions given up on brought none since the last.
    fn gave_up(&mut self, encoded: u64) {
        self.patience_us = if encoded > self.encoded_at_give_up {
            micros(ENCODE_STUCK)
        } else {
            self.patience_us.saturating_mul(2).min(micros(ENCODE_STUCK_MAX))
        };
        self.encoded_at_give_up = encoded;
        self.seen = 0;
        self.charged_us = 0;
    }
}

/// `d` in whole microseconds.
fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

/// How long the replacement of a lost session waits when the replacement before it was lost
/// too without coding a frame. Doubles for each such replacement in a row, up to
/// [`LOST_RETRY_MAX`]. A session the system took away once is replaced at once.
///
/// Built at once every time, a stream on an encoder that answers every frame with
/// `kVTVideoEncoderNotAvailableNowErr` built sessions as fast as VideoToolbox made them: 13 301
/// in one test run on a virtual Mac whose encoder had stopped (`docs/decisions/video.md`, "A
/// virtual Mac's encoder stops for good past its 1020th client").
const LOST_RETRY: Duration = Duration::from_millis(250);
/// The longest [`LOST_RETRY`] grows to: the same bound as a stuck encode's patience.
const LOST_RETRY_MAX: Duration = ENCODE_STUCK_MAX;

/// When the next replacement of a lost session may be built ([`Pipeline::follow_lost`]).
#[derive(Debug, Default)]
struct LostRetry {
    /// When the last replacement was started, `0` before one.
    started_us: u64,
    /// The stream's encoded frames then.
    encoded_at: u64,
    /// The wait that replacement kept after the one before it; `0` when it was the first in a
    /// row.
    waited_us: u64,
}

impl LostRetry {
    /// The wait the next replacement keeps after the last: none once a frame was coded since.
    fn next_wait(&self, encoded: u64) -> u64 {
        if self.started_us == 0 || encoded > self.encoded_at {
            0
        } else if self.waited_us == 0 {
            micros(LOST_RETRY)
        } else {
            self.waited_us.saturating_mul(2).min(micros(LOST_RETRY_MAX))
        }
    }

    /// When a replacement may be started, with the stream at `encoded` frames.
    fn due_us(&self, encoded: u64) -> u64 {
        self.started_us.saturating_add(self.next_wait(encoded))
    }

    /// A replacement starts at `now_us`, with the stream at `encoded` frames.
    fn started(&mut self, now_us: u64, encoded: u64) {
        self.waited_us = self.next_wait(encoded);
        self.started_us = now_us.max(1);
        self.encoded_at = encoded;
    }
}

struct Shared<P: Platform = Native> {
    id: StreamId,
    /// The sessions in force and what they were last told. Only an encode takes it, in its
    /// turn and never across a call into the encoder ([`Self::try_encode`]; the module's
    /// "Locks").
    encoder: Mutex<Lives<P::Video>>,
    /// Sessions built for the stream and not yet put in: the next encode puts them in, in
    /// place of the ones in force ([`Self::install`]).
    staged: Mutex<Option<Staged<P::Video>>>,
    /// The whole picture, or the top stripe: its media stream is the stream's own.
    top: Coder,
    /// The lower stripe, while the stream is striped.
    lower: Coder,
    /// The capture whose stripes are still being coded ([`Join`]).
    join: Mutex<Join>,
    /// The newest capture, left by ScreenCaptureKit's queue for the encode thread
    /// ([`start_encode_thread`]).
    mailbox: Mailbox<Frame<P>>,
    rate: Mutex<RateController>,
    /// Whether the encoder should write temporal layers, from the link's loss and the frames
    /// a layered session drops ([`LayerGate`]); the next encode tells the session.
    layers: Mutex<LayerGate>,
    /// What [`Self::layers`] last decided, for the encode to read without the gate's lock.
    layers_wanted: AtomicBool,
    /// Frames the stream's encoder sessions dropped, all sessions together; the encode keeps it
    /// ([`Self::tell`]), since only an encode may ask the session.
    encoder_dropped: AtomicU64,
    /// Frames a replaced session gave back after the new one was put in, and so dropped
    /// ([`Self::on_session_packet`]). An aligned session codes inside the submit and has none
    /// in flight when it is replaced; a session that codes after its submit returns may.
    replaced_dropped: AtomicU64,
    /// [`Self::encoder_dropped`] at the last receiver report.
    dropped_reported: AtomicU64,
    /// Whether the stream carries 4:4:4 or 4:2:0, following the rate's decisions; the pipeline
    /// rebuilds for a change on its geometry tick ([`Pipeline::check_geometry`]).
    chroma: Mutex<ChromaGate>,
    /// `host_now_us()` when the last datagram was queued; the heartbeat clock.
    last_push_us: AtomicU64,
    /// The cadence rung in force: how many of the captures reach the encoder, and the frame rate
    /// the encoder is told (at its next frame).
    fps: std::sync::atomic::AtomicU16,
    /// The cadence the client asked for, or the rung the encoder was seen to keep up with when
    /// that is lower ([`Shared::watch_encoder`], [`Shared::fed`]); the ladder never climbs past
    /// it.
    fps_ceiling: std::sync::atomic::AtomicU16,
    /// The cadence the client asked for: the ceiling rises back to it and no further.
    fps_asked: std::sync::atomic::AtomicU16,
    /// Whether the encoder keeps up with the rung in force ([`EncoderWatch`]).
    watch: Mutex<EncoderWatch>,
    /// A client has this stream's tile focused: its encoder falling behind steps the other
    /// streams down first ([`engines`]).
    focused: AtomicBool,
    /// `host_now_us()` until which the ceiling stays where it stepped down for a focused
    /// stream, `0` when it holds for none ([`engines::Contender::give_way`]).
    give_way_until_us: AtomicU64,
    /// The watch's windows closed since the others last gave way for this stream: it asks
    /// again only after [`engines::SETTLE_WINDOWS`].
    windows_since_gave: AtomicU32,
    /// Frames a second the encoder was fed over the last window at the rung in force, `0` until
    /// one closes ([`Fed`]): what the congestion guard budgets a frame from.
    fed_fps: std::sync::atomic::AtomicU16,
    /// The bitrate the encoder is to be told at its next frame ([`Self::apply_bitrate`]).
    encoder_bps: AtomicU32,
    /// The presentation time of the last frame handed to a session; the next must be later.
    /// Every coder of one capture codes it under one stamp, which names the capture on the wire.
    last_encoded_us: AtomicU64,
    /// The cadence gate's next slot ([`Pace::next_us`]); read and moved under `held`.
    pace_us: AtomicU64,
    /// The last session number handed out ([`Self::next_session`]).
    sessions: AtomicU64,
    /// The newest capture of the target, kept after it is encoded. ScreenCaptureKit sends
    /// nothing while the picture is still, so this is the only picture a skipped frame, a
    /// refresh or a keyframe asked for on a still screen can be answered with
    /// ([`repair_loop`]). Only an encode takes it, inside its turn ([`Self::gate`]), and never
    /// across a call into the encoder.
    held: Mutex<Option<Frame<P>>>,
    /// The one encode at a time, which keeps the presentation timestamps the encoder sees in
    /// order; the beat takes it from an encode stuck inside VideoToolbox ([`Self::unstick`]).
    gate: Gate,
    /// The beat's watch on the encode inside the encoder ([`StuckWatch`]).
    stuck: Mutex<StuckWatch>,
    /// When the next replacement of a lost session may be built ([`LostRetry`]), and the moment
    /// the beat last woke the geometry tick for (`0` for none).
    lost_retry: Mutex<(LostRetry, u64)>,
    /// The encode thread that takes the mailbox now; one numbered otherwise ends at its next
    /// capture ([`start_encode_thread`]).
    encode_thread: AtomicU64,
    /// Wakes [`repair_loop`] out of waiting on a repair given up on inside the encoder.
    unstuck: tokio::sync::Notify,
    /// The held capture's time, `0` while none is held: what the runtime reads of it.
    held_us: AtomicU64,
    /// The region of the target the held capture shows ([`region::pack`]), written with it.
    held_region: AtomicU64,
    /// Which region each capture shows, by its display time ([`region::RegionClock`]).
    region: Mutex<region::RegionClock>,
    /// Captures not sent because they came while the capture was changing region, and may
    /// show either.
    between_regions: AtomicU64,
    /// The held capture has not reached the encoder.
    owed: AtomicBool,
    /// Wakes [`repair_loop`]: a capture was held back, or a request came in.
    repair: tokio::sync::Notify,
    /// Video datagrams waiting to be handed to QUIC behind a slice of the link, so audio goes
    /// ahead of them ([`Lane`]).
    lane: Mutex<Lane>,
    /// Wakes [`lane_loop`]: video is waiting in the lane.
    lane_wake: tokio::sync::Notify,
    /// When the client's sound last went out ([`sound::Sound`]): the lane is used only while
    /// it flows. Unset for a stream no sound listens through.
    sound: std::sync::OnceLock<Arc<sound::SoundClock>>,
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
    /// Wakes [`shape_loop`]: the pointer came over the target.
    shape_wake: tokio::sync::Notify,
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
    /// Wakes [`cursor_loop`]: the target's bounds, its visibility or the zoom changed.
    cursor_wake: tokio::sync::Notify,
    /// Times [`cursor_loop`] went round, said when the stream closes.
    cursor_wakes: AtomicU64,
    /// Wakes the owner's geometry probe ahead of its period: the accessibility API says the
    /// target moved, was resized or went, or a frame came while the client was told the source
    /// is idle ([`Pipeline::geometry_quiet`]).
    geometry_wake: Arc<tokio::sync::Notify>,
    /// The session the system took away last ([`CodecError::EncoderLost`]), zero for none: it
    /// codes nothing more, and while it is in force the geometry tick builds new ones at the
    /// size in force ([`Pipeline::check_geometry`]).
    encoder_lost: AtomicU64,
    /// The client was last told the source is idle ([`Pipeline::check_source`]).
    source_idle: AtomicBool,
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
            encoder: Mutex::new(Lives {
                top: Live::new(),
                lower: Live::new(),
                layout: None,
                helper: None,
            }),
            staged: Mutex::new(None),
            top: Coder::new(Stripe::media_of(id, 0)),
            lower: Coder::new(Stripe::media_of(id, 1)),
            join: Mutex::new(Join::default()),
            mailbox: Mailbox::default(),
            rate: Mutex::new(RateController::new(max_bps)),
            layers: Mutex::new(LayerGate::default()),
            layers_wanted: AtomicBool::new(false),
            encoder_dropped: AtomicU64::new(0),
            replaced_dropped: AtomicU64::new(0),
            dropped_reported: AtomicU64::new(0),
            chroma: Mutex::new(ChromaGate::new(Chroma::Subsampled, (0, 0), 0)),
            last_push_us: AtomicU64::new(now::<P>()),
            fps: std::sync::atomic::AtomicU16::new(fps),
            fps_ceiling: std::sync::atomic::AtomicU16::new(fps),
            fps_asked: std::sync::atomic::AtomicU16::new(fps),
            watch: Mutex::new(EncoderWatch::default()),
            focused: AtomicBool::new(false),
            give_way_until_us: AtomicU64::new(0),
            windows_since_gave: AtomicU32::new(engines::SETTLE_WINDOWS),
            fed_fps: std::sync::atomic::AtomicU16::new(0),
            encoder_bps: AtomicU32::new(0),
            last_encoded_us: AtomicU64::new(0),
            pace_us: AtomicU64::new(0),
            sessions: AtomicU64::new(0),
            held: Mutex::new(None),
            gate: Gate::default(),
            stuck: Mutex::new(StuckWatch::new(now::<P>())),
            lost_retry: Mutex::new((LostRetry::default(), 0)),
            encode_thread: AtomicU64::new(0),
            unstuck: tokio::sync::Notify::new(),
            held_us: AtomicU64::new(0),
            held_region: AtomicU64::new(0),
            region: Mutex::new(region::RegionClock::steady(None)),
            between_regions: AtomicU64::new(0),
            owed: AtomicBool::new(false),
            repair: tokio::sync::Notify::new(),
            lane: Mutex::new(Lane::default()),
            lane_wake: tokio::sync::Notify::new(),
            sound: std::sync::OnceLock::new(),
            zoom: AtomicU64::new(1.0_f64.to_bits()),
            cropped: AtomicBool::new(cropped),
            target_hidden: AtomicBool::new(false),
            pointer_over: AtomicBool::new(false),
            shape_wake: tokio::sync::Notify::new(),
            suspect_until_us: AtomicU64::new(0),
            filter_stalled: AtomicBool::new(false),
            sink,
            too_large_logged: AtomicBool::new(false),
            bounds: Mutex::new(None),
            cursor_wake: tokio::sync::Notify::new(),
            cursor_wakes: AtomicU64::new(0),
            geometry_wake: Arc::new(tokio::sync::Notify::new()),
            encoder_lost: AtomicU64::new(0),
            source_idle: AtomicBool::new(false),
            counters: Counters::new(),
        }
    }

    /// The counters plus the state only the stream knows: whether the display crop is what is
    /// being served right now, and whether a long-term reference is usable. Every reader goes
    /// through here, so no caller can publish the snapshot's placeholders.
    fn stats(&self) -> ScreenStats {
        let snapshot = self.counters.snapshot();
        let age = self.top.ltr.lock().usable_age(now::<P>());
        ScreenStats {
            on_crop: self.cropped.load(Ordering::Relaxed),
            audio_packets: self.sound.get().map_or(0, |sound| sound.packets()),
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

    /// Scale the stream by `zoom` stream pixels per native pixel; the cursor's place in the
    /// picture moves with it.
    fn set_zoom(&self, zoom: f64) {
        self.zoom.store(zoom.to_bits(), Ordering::Relaxed);
        self.cursor_wake.notify_one();
    }

    /// Whether the target is on screen, from the geometry probe; the cursor loop hears of a
    /// change.
    fn set_hidden(&self, hidden: bool) -> bool {
        let was = self.target_hidden.swap(hidden, Ordering::Relaxed);
        if was != hidden {
            self.cursor_wake.notify_one();
        }
        was
    }

    /// Coder `index`: the whole picture or the top stripe for 0, the lower stripe for 1.
    const fn coder(&self, index: usize) -> &Coder {
        if index == 0 { &self.top } else { &self.lower }
    }

    /// The coders of a stream coded by `count` of them, top first, with their indices.
    fn coders(&self, count: usize) -> impl Iterator<Item = (usize, &Coder)> {
        [&self.top, &self.lower].into_iter().enumerate().take(count)
    }

    /// How many coders code the stream now, without an encode's lock: the lower stripe's
    /// session is on record only while the stream is striped.
    fn coder_count(&self) -> usize {
        if self.lower.session.load(Ordering::Relaxed) == 0 { 1 } else { Stripe::MAX }
    }

    /// Decide the chroma for a session `config` describes, the client asking for `asked`: at
    /// once, from the rate target under the session's ceiling ([`ChromaGate::ask`]).
    fn ask_chroma(&self, asked: Chroma, config: &EncoderConfig) -> Chroma {
        let target = self.rate.lock().target_bps().min(config.bitrate_bps);
        self.chroma.lock().ask(asked, (config.width, config.height), target)
    }

    /// Point the encoder at the share of `target` the parity leaves it ([`encoder_bps`]), at its
    /// next frame ([`Self::tell`]); a rebuild picks the controller's target up again. `target`
    /// stays the rate the guards read, from now: the bytes they weigh are frames and their parity
    /// together.
    fn apply_bitrate(&self, target: u32) {
        let parity = self.top.packetizer.lock().parity_permille();
        let bps = encoder_bps(target, parity);
        self.counters.bitrate_bps.store(u64::from(target), Ordering::Relaxed);
        self.encoder_bps.store(bps, Ordering::Relaxed);
        tracing::debug!(stream = %self.id, target, bps, parity, "bitrate");
    }

    /// Leave new encoder sessions for the next encode to put in, in place of the ones in force
    /// ([`Self::put_in`]). The swap and the reset of what described the old sessions happen
    /// there, under the encode's own lock, before it reads its requests: no frame reaches a new
    /// session with an old one's requests, and none of a new session's packets can be filed in
    /// an old book. Nothing here waits on an encode, which may be coding a frame inside its
    /// submit for tens of milliseconds (the module's "Locks").
    ///
    /// Captures waiting in the mailbox were taken for the old sessions and are dropped. One held
    /// already is left to the encode, which drops it if it is not the new sessions' size; the
    /// repair loop is woken so a still picture of the right size answers the new keyframes.
    ///
    /// Returns sessions left here before and never put in, for the caller to drop off the
    /// runtime: invalidating one waits for its callbacks.
    ///
    /// Each session carries the number it was built under ([`Self::next_session`]): once it is
    /// put in, only its packets are its coder's. A frame an old session was still encoding
    /// comes out after the new session's keyframe; sent, it would be decoded against the new
    /// session and fail (MEASUREMENTS.md, "an old session's frame after the new keyframe").
    #[must_use = "sessions never put in are dropped off the runtime"]
    fn install(&self, built: Built<P>, sessions: [u64; 2]) -> Option<Built<P>> {
        let Built { top, lower, layout } = built;
        let [top_session, lower_session] = sessions;
        let staged = Staged {
            top: (top, top_session),
            lower: lower.map(|lower| (lower, lower_session)),
            layout,
        };
        let unused = self.staged.lock().replace(staged).map(|unused| Built {
            top: unused.top.0,
            lower: unused.lower.map(|(lower, _)| lower),
            layout: unused.layout,
        });
        self.mailbox.clear();
        self.repair.notify_one();
        unused
    }

    /// Put the sessions [`Self::install`] left in, if there are some; `lives` is the encode's
    /// lock. The old sessions are dropped on a thread of their own ([`retire`]).
    fn put_in(&self, lives: &mut Lives<P::Video>) {
        let Some(staged) = self.staged.lock().take() else { return };
        let Staged { top: (top, top_session), lower, layout } = staged;
        let old_top =
            std::mem::replace(&mut lives.top, Live { session: Some(Arc::new(top)), ..Live::new() });
        let (lower, lower_session) = lower.map_or((None, 0), |(v, n)| (Some(Arc::new(v)), n));
        let old_lower = std::mem::replace(&mut lives.lower, Live { session: lower, ..Live::new() });
        lives.layout = layout;
        self.top.session.store(top_session, Ordering::Relaxed);
        self.lower.session.store(lower_session, Ordering::Relaxed);
        // Both stripes' frames from here on name the build, so the client never puts one of
        // these beside a stripe of the sessions they replace
        // ([`slopty_proto::media::FramePrefix::build`]).
        let [build, ..] = top_session.to_le_bytes();
        for coder in [&self.top, &self.lower] {
            coder.packetizer.lock().set_build(build);
        }
        self.top.rebuilt();
        self.lower.rebuilt();
        *self.join.lock() = Join::default();
        retire(old_top.session);
        retire(old_lower.session);
    }

    /// What coder `index`'s session in force is to be told before its next frame: its share of
    /// the bitrate and the frame rate asked of the stream since it was last told, and whether
    /// it writes temporal layers; `lives` is the encode's lock. Decided here and recorded as
    /// told, and told once the lock is let go ([`Self::tell_session`]): once each, as a refusal
    /// is logged, not retried per frame. The frames the session dropped are read here, an
    /// atomic of the session's own.
    fn tell(&self, lives: &mut Lives<P::Video>, index: usize, fps: u16) -> Told {
        let (rows, of) = lives.share(index);
        let live = lives.live(index);
        let Some(session) = live.session.as_ref() else { return Told::default() };
        let whole = self.encoder_bps.load(Ordering::Relaxed);
        let bps = u64::from(whole)
            .saturating_mul(u64::from(rows))
            .checked_div(u64::from(of))
            .map_or(whole, |bps| u32::try_from(bps).unwrap_or(u32::MAX));
        let mut told = Told { whole, ..Told::default() };
        if bps != 0 && bps != live.bps {
            live.bps = bps;
            told.bps = Some(bps);
        }
        if fps != live.fps {
            live.fps = fps;
            told.fps = Some(fps);
        }
        let dropped = session.frames_dropped();
        self.encoder_dropped.fetch_add(dropped.saturating_sub(live.dropped), Ordering::Relaxed);
        live.dropped = dropped;
        // A session is never layered in its first frames: switched on there, the encoder spends
        // more and drops frames for the rest of the session (MEASUREMENTS.md, "temporal layers
        // switched on a live session").
        let layers =
            self.layers_wanted.load(Ordering::Relaxed) && live.frames >= LAYERS_AFTER_FRAMES;
        live.frames = live.frames.saturating_add(1);
        if layers != live.layers {
            live.layers = layers;
            told.layers = Some(layers);
        }
        told
    }

    /// Tell coder `index`'s `session` what [`Self::tell`] decided: calls into VideoToolbox, made
    /// with no lock held.
    fn tell_session(&self, index: usize, session: &P::Video, told: Told) {
        if let Some(bps) = told.bps {
            match session.set_bitrate(bps) {
                Ok(()) if index == 0 => {
                    self.counters.encoder_bps.store(u64::from(told.whole), Ordering::Relaxed);
                }
                Ok(()) => {}
                Err(e) => tracing::warn!(stream = %self.id, index, bps, error = %e, "set bitrate"),
            }
        }
        if let Some(fps) = told.fps
            && let Err(e) = session.set_frame_rate(fps)
        {
            tracing::warn!(stream = %self.id, index, fps, error = %e, "set frame rate");
        }
        if let Some(layers) = told.layers {
            match session.set_temporal_layers(layers) {
                Ok(written) => {
                    tracing::debug!(stream = %self.id, index, layers, written, "temporal layers");
                }
                Err(e) => {
                    tracing::warn!(stream = %self.id, index, layers, error = %e, "set temporal layers");
                }
            }
        }
    }

    /// Drop the held capture: it is not a picture of the target any more (hidden, suspected, or
    /// another size). Only an encode path calls it: the lock may be held across an encode.
    fn forget_held(&self) {
        let forgotten = {
            let mut held = self.held.lock();
            self.held_us.store(0, Ordering::Relaxed);
            held.take()
        };
        self.owed.store(false, Ordering::Relaxed);
        self.top.refine.lock().stop();
        self.lower.refine.lock().stop();
        drop(forgotten);
    }

    /// Bytes waiting to leave: what QUIC holds plus what waits in the lane.
    fn held_bytes(&self) -> usize {
        self.sink.held().saturating_add(self.lane.lock().bytes)
    }

    /// Move the cadence to the rung `bps` affords; the encoder is told at its next frame.
    ///
    /// Capture is left at the ceiling either way: a change on screen is still seen within a display
    /// beat, only fewer of those captures are encoded, so the picture that does go out is worth its
    /// bandwidth (`docs/decisions/video.md`, the cadence ladder).
    fn apply_cadence(&self, bps: u32) {
        let ceiling = self.fps_ceiling.load(Ordering::Relaxed);
        let mut cadence = Cadence::resume(ceiling, self.fps.load(Ordering::Relaxed));
        let Some(fps) = cadence.update(bps) else { return };
        self.fps.store(fps, Ordering::Relaxed);
        // What the encoder was fed at the old rung says nothing of the new one.
        self.fed_fps.store(0, Ordering::Relaxed);
        tracing::debug!(stream = %self.id, fps, bps, "cadence");
    }

    /// Start the ladder over under `ceiling`, the rate the stream asked for: a new quality, or
    /// a new session, which the watch has not seen yet.
    fn reset_rate(&self, ceiling: u16) {
        self.fps_asked.store(ceiling, Ordering::Relaxed);
        self.fps_ceiling.store(ceiling, Ordering::Relaxed);
        self.fps.store(ceiling, Ordering::Relaxed);
        self.fed_fps.store(0, Ordering::Relaxed);
        *self.watch.lock() = EncoderWatch::default();
    }

    /// The encoder cannot keep the rung in force: the ceiling comes down to `ceiling` and the
    /// cadence with it, until the next encoder session or until the encoder shows room again
    /// ([`Self::raise_ceiling`]). The output callback calls it inside an encode, so it touches
    /// the session not at all (the module's "Locks").
    fn lower_ceiling(&self, ceiling: u16, why: &'static str) {
        let fps = self.fps.load(Ordering::Relaxed);
        let ceiling = ceiling.min(self.fps_ceiling.load(Ordering::Relaxed));
        self.fps_ceiling.store(ceiling, Ordering::Relaxed);
        let target = self.rate.lock().target_bps();
        self.apply_cadence(target);
        tracing::info!(stream = %self.id, fps, ceiling, why);
    }

    /// A frame spent `encode_us` in the encoder. After a run of frames queueing inside it at
    /// the rung in force, the ceiling steps down a rung and the cadence with it: the encoder
    /// cannot turn frames out that fast, and a queue in it is latency on every frame
    /// ([`EncoderWatch`]).
    fn watch_encoder(&self, encode_us: u64, keyframe: bool) {
        let fps = self.fps.load(Ordering::Relaxed);
        let slower = self.watch.lock().returned(encode_us, fps, keyframe);
        if let Some(ceiling) = slower
            && !self.others_give_way()
        {
            self.lower_ceiling(ceiling, "encoder queueing: fewer frames");
        }
    }

    /// This stream's encoder fell behind: when a client focuses it, the streams nobody focuses
    /// step down in its place, or did so lately enough that what it sees is the queue from
    /// before ([`engines::SETTLE_US`]). Whether this stream keeps its rung.
    fn others_give_way(&self) -> bool {
        if !self.focused.load(Ordering::Relaxed) {
            return false;
        }
        if self.windows_since_gave.load(Ordering::Relaxed) < engines::SETTLE_WINDOWS {
            return true;
        }
        match engines::ENGINES.contended(now::<P>()) {
            engines::Claim::Gave => {
                self.windows_since_gave.store(0, Ordering::Relaxed);
                tracing::debug!(stream = %self.id, "encoder behind: the unfocused streams give way");
                true
            }
            engines::Claim::Settling => true,
            engines::Claim::Spent => false,
        }
    }

    /// A capture went into the encoder at the rung `fps`: at the end of a window, the rate the
    /// encoder was fed. The rung follows it down when the mailbox had to replace many of the
    /// captures the rung asked for, and the ceiling rises back towards what the client asked
    /// for when the encoder codes well over the rung ([`EncoderWatch::fed`]).
    fn fed(&self, fps: u16) {
        let verdict = self.watch.lock().fed(fps);
        let Some(Fed { fps: fed, ceiling }) = verdict else { return };
        let _windows =
            self.windows_since_gave
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| Some(n.saturating_add(1)));
        match ceiling {
            Some(ceiling) if ceiling < fps && self.others_give_way() => {
                self.fed_fps.store(fed, Ordering::Relaxed);
            }
            Some(ceiling) if ceiling < fps => {
                self.lower_ceiling(ceiling, "encoder busy: the rung follows what it was fed");
            }
            Some(ceiling) => {
                self.fed_fps.store(fed, Ordering::Relaxed);
                self.raise_ceiling(ceiling);
            }
            None => self.fed_fps.store(fed, Ordering::Relaxed),
        }
    }

    /// The encoder codes well over the rung: the ceiling rises to `ceiling`, no further than the
    /// client asked, and the cadence climbs under it as the rate allows.
    fn raise_ceiling(&self, ceiling: u16) {
        // Held down for a focused stream: the room it sees is the room given way.
        if now::<P>() < self.give_way_until_us.load(Ordering::Relaxed) {
            return;
        }
        let ceiling = ceiling.min(self.fps_asked.load(Ordering::Relaxed));
        let was = self.fps_ceiling.load(Ordering::Relaxed);
        if ceiling <= was {
            return;
        }
        self.fps_ceiling.store(ceiling, Ordering::Relaxed);
        let target = self.rate.lock().target_bps();
        self.apply_cadence(target);
        tracing::info!(stream = %self.id, was, ceiling, "encoder has room: the ceiling rises");
    }

    /// ScreenCaptureKit delivered a frame: leave it for the encode thread. One still waiting
    /// there is replaced and counted, and told to the watch when it cost the rung a slot: it was
    /// due, and the capture replacing it falls in the slot after the one it would have claimed.
    /// One replaced within its own slot costs nothing, as the newer capture fills that slot.
    fn post(&self, frame: Frame<P>) {
        let newer_us = frame.capture_ts_us;
        let Some(superseded) = self.mailbox.post(frame) else { return };
        self.counters.superseded.fetch_add(1, Ordering::Relaxed);
        let mut pace = Pace::resume(self.pace_us.load(Ordering::Relaxed));
        let fps = self.fps.load(Ordering::Relaxed);
        if pace.due(superseded.capture_ts_us, fps) {
            pace.sent(superseded.capture_ts_us, fps);
            if pace.due(newer_us, fps) {
                self.watch.lock().superseded(fps);
            }
        }
        drop(superseded);
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
    ///
    /// A frame is budgeted at the rate the encoder is fed, when the last window found that
    /// under the rung: the encoder sizes its frames from their timestamps, so a 120 rung fed 60
    /// makes frames of a 60th of the rate (16.0 KB at 8 Mbit/s, against the 8.3 KB a 120th
    /// allows), and two frames' worth at 120 was one frame of them.
    fn frame_fits(&self) -> bool {
        let refining = self
            .top
            .refine_wire
            .load(Ordering::Relaxed)
            .saturating_add(self.lower.refine_wire.load(Ordering::Relaxed));
        let held =
            self.held_bytes().saturating_sub(usize::try_from(refining).unwrap_or(usize::MAX));
        let cwnd = if held == 0 { 0 } else { self.sink.cwnd() };
        let rung = self.fps.load(Ordering::Relaxed);
        let fed = self.fed_fps.load(Ordering::Relaxed);
        let fps = if fed == 0 { rung } else { rung.min(fed) };
        frame_fits(held, cwnd, self.counters.bitrate_bps.load(Ordering::Relaxed), fps)
    }

    /// What the accessibility watch heard of the target's application: a suspicion, a sibling
    /// gone, or a move or resize of the target, which wakes the geometry probe.
    fn heard(&self, went: Went) {
        match went {
            Went::Target => self.suspect(now::<P>()),
            Went::Other => self.sibling_went(),
            Went::Moved => self.geometry_wake.notify_one(),
        }
    }

    /// The accessibility API says a window of the target's application went at `now`: hold
    /// frames for [`SUSPICION_HOLD`] while the window list catches up; the geometry tick moves
    /// the stream to the window filter meanwhile.
    fn suspect(&self, now: u64) {
        let hold_us = u64::try_from(SUSPICION_HOLD.as_micros()).unwrap_or(u64::MAX);
        self.suspect_until_us.store(now.saturating_add(hold_us), Ordering::Relaxed);
        let suspicions = self.counters.suspicions.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(stream = %self.id, suspicions = suspicions.saturating_add(1), "hide suspected");
        self.geometry_wake.notify_one();
    }

    /// The accessibility API says a window of the target's application that is not the target
    /// went: no hold, but the capture may have stalled on it, so the next geometry tick takes
    /// the stream through the window filter.
    fn sibling_went(&self) {
        self.filter_stalled.store(true, Ordering::Relaxed);
        let siblings = self.counters.siblings.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(stream = %self.id, siblings = siblings.saturating_add(1), "another window of the application went");
        self.geometry_wake.notify_one();
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
        let region::Shows::Region(region) = self.region.lock().shows(frame.capture_ts_us) else {
            self.between_regions.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let attempt = self.try_encode(Some((frame, region)), now::<P>());
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

    /// Encode the held capture if the gates let it through, in the stream's turn ([`Gate`]).
    /// `fresh` is a capture that just arrived, held from here on as the newest picture;
    /// without one it is [`repair_loop`] sending the held one again at `now`.
    fn try_encode(&self, fresh: Option<(Frame<P>, Option<Region>)>, now: u64) -> Attempt {
        let turn = self.gate.take();
        let attempt = self.encode_turn(turn, fresh, now);
        self.gate.leave(turn);
        attempt
    }

    /// [`Self::try_encode`] in turn `turn`. `held` and `encoder` are let go before the calls
    /// into the encoder: one that does not return holds nothing but the turn, which the beat
    /// takes from it ([`Self::unstick`]); it then answers [`Attempt::GivenUp`] and changes
    /// nothing.
    ///
    /// A striped stream codes a new capture in both stripes at once, under one stamp: the lower
    /// stripe on the [`Helper`]'s thread, the top one on this. A repair codes only the stripes
    /// that asked for one (a refresh or a keyframe, or a refinement of the still picture), and
    /// the frame prefix names the stripes coded from the capture, so the client keeps the other
    /// stripe's picture.
    ///
    /// A capture of another size than the session's (one taken before a rebuild) is dropped,
    /// not coded: VideoToolbox would take it and code it into the session's size. The session's
    /// keyframe stays wanted for the first capture of its own size.
    #[expect(clippy::too_many_lines, reason = "one decision, read top to bottom")]
    fn encode_turn(
        &self,
        turn: u64,
        fresh: Option<(Frame<P>, Option<Region>)>,
        now: u64,
    ) -> Attempt {
        // Declared ahead of the lock, so the capture a fresh one replaces is let go after it.
        let mut replaced = None;
        let mut held = self.held.lock();
        let is_fresh = fresh.is_some();
        if let Some((frame, region)) = fresh {
            self.held_us.store(frame.capture_ts_us.max(1), Ordering::Relaxed);
            self.held_region.store(region::pack(region), Ordering::Relaxed);
            self.owed.store(true, Ordering::Relaxed);
            replaced = held.replace(frame);
        }
        let fresh = is_fresh;
        let Some(frame) = held.as_ref() else { return Attempt::Nothing };
        // In the turn from reading the requests to the end of the submits, so a rebuild is
        // wholly before or wholly after them ([`Self::install`]).
        let mut lives = self.encoder.lock();
        self.put_in(&mut lives);
        // Sessions given up on inside the encoder code nothing more ([`Self::unstick`]): none
        // is in force until the geometry tick's replacements are put in.
        if self.top.session.load(Ordering::Relaxed) == 0 && lives.top.session.is_some() {
            retire(lives.top.session.take());
            retire(lives.lower.session.take());
            lives.helper = None;
        }
        let count = lives.count();
        let owed = self.owed.load(Ordering::Relaxed);
        // What each coder is asked for, bit `i` for coder `i`.
        let (mut keyframes, mut refreshes) = (0_u8, 0_u8);
        for (i, coder) in self.coders(count) {
            let pending = coder.pending.lock();
            keyframes |= u8::from(pending.keyframe) << i;
            refreshes |= u8::from(pending.refresh) << i;
        }
        let every = if count > 1 { 0b11 } else { 0b01 };
        // With nothing owed or asked, a repair can only be the still picture refined: coded
        // again while the encoder keeps gaining on it, never ahead of its spacing.
        let last = self.last_encoded_us.load(Ordering::Relaxed);
        let refining = !fresh && !owed && keyframes == 0 && refreshes == 0;
        let coding = if refining {
            let mut due = 0_u8;
            for (i, coder) in self.coders(count) {
                if self.refine_due(coder, count).is_some_and(|at| now >= at) {
                    due |= 1 << i;
                }
            }
            due
        } else if fresh || owed {
            every
        } else {
            keyframes | refreshes
        };
        if coding == 0 {
            return Attempt::Nothing;
        }
        // A refinement only goes onto an empty link, so it never queues behind a change nor
        // stands a change's worth of bytes in front of one.
        if refining && self.held_bytes() > 0 {
            return Attempt::NoRoom;
        }
        if fresh && !owed && keyframes == 0 && refreshes == 0 {
            return Attempt::Nothing;
        }
        // A repair is a picture of the target as it is now, stamped now: the capture's own time
        // would put it behind the frame already encoded from it.
        let at = if fresh { frame.capture_ts_us } else { now };
        let fps = self.fps.load(Ordering::Relaxed);
        let mut pace = Pace::resume(self.pace_us.load(Ordering::Relaxed));
        // The cadence rung, before the congestion guard: a capture the ladder is not asking for is
        // not a frame the link failed to carry, and skipping it is what gives the next one the
        // bytes to be worth sending. No refresh is owed, the client is missing nothing.
        // A keyframe is the one thing the rung does not hold back: it is what a client with no
        // picture at all is waiting on, and there is at most one in flight. A pending *refresh* is
        // not urgent in the same way — at a collapsed rate the guard below sets one on every frame
        // it drops, and letting those through would take the cadence off exactly where it is
        // needed.
        let due = pace.due(at, fps);
        if !due && keyframes & coding == 0 {
            return Attempt::NotDue;
        }
        if !self.frame_fits() {
            // A capture that did not fit is a hole in the stream; a repair that did not fit is
            // the same picture waiting another period.
            if fresh {
                self.dropped(now, count);
            }
            return Attempt::NoRoom;
        }
        // A keyframe the link cannot drain is put off and an LTR refresh encoded in its place: a
        // picture the client can decode, at a fraction of the bytes. The request stays pending, so
        // the keyframe follows as soon as the link can carry one or the valve opens. With it
        // deferred there is nothing urgent in this frame, so the cadence rung applies again.
        let mut deferred = 0_u8;
        for (i, coder) in self.coders(count) {
            if keyframes & coding & (1 << i) != 0 && !self.keyframe_admitted(coder, now) {
                deferred |= 1 << i;
            }
        }
        if !due && keyframes & coding & !deferred == 0 {
            return Attempt::NotDue;
        }
        let mut options: [Option<(FrameOptions, bool)>; Stripe::MAX] = [None, None];
        for ((i, coder), slot) in self.coders(count).zip(&mut options) {
            if coding & (1 << i) == 0 {
                continue;
            }
            let defer = deferred & (1 << i) != 0;
            // Read before the requests: the book is locked ahead of them where both are taken.
            let usable = coder.ltr_usable();
            let mut pending = coder.pending.lock();
            let refresh = std::mem::take(&mut pending.refresh) || defer;
            // A refresh with no acknowledged reference to be predicted from goes out as a
            // keyframe. VideoToolbox answers `ForceLTRRefresh` with nothing acknowledged with a
            // sync frame that the frames after it cannot be decoded against (-12909, every one
            // until the next keyframe, at 3024 × 1964): each failure asked for a refresh, and
            // each refresh made the next one (MEASUREMENTS.md, "a refresh with nothing
            // acknowledged").
            let standalone = refresh && !usable;
            let force_keyframe = !defer && (std::mem::take(&mut pending.keyframe) || standalone);
            *slot = Some((
                FrameOptions {
                    force_keyframe,
                    force_ltr_refresh: refresh && !standalone,
                    acked_ltr: std::mem::take(&mut pending.acked),
                },
                standalone,
            ));
        }
        // The encoder wants presentation times that only go forward. A refinement is stamped a
        // period early, so the capture after it keeps its own stamp ([`Refine::stamp`]).
        let pts = if refining {
            Refine::stamp(now, last, period_us(fps))
        } else {
            at.max(last.saturating_add(1))
        };
        self.last_encoded_us.store(pts, Ordering::Relaxed);
        // A refinement takes no slot of the cadence: the next change goes at once rather than
        // wait a period behind it. Its own spacing keeps it under the rung.
        if !refining {
            pace.sent(at, fps);
            self.pace_us.store(pace.next_us(), Ordering::Relaxed);
        }
        self.owed.store(false, Ordering::Relaxed);
        if refining {
            self.counters.refined.fetch_add(1, Ordering::Relaxed);
        } else if !fresh {
            self.counters.repaired.fetch_add(1, Ordering::Relaxed);
        }
        let stripes = if count > 1 { coding } else { 0 };
        let shown = region::unpack(self.held_region.load(Ordering::Relaxed));
        // Before the submits: an aligned session's callback runs inside them.
        if stripes.count_ones() > 1 && !refining {
            *self.join.lock() = Join { pts, waiting: coding, took: None, keyframe: false };
        }
        let mut told = [Told::default(); Stripe::MAX];
        for ((i, coder), told) in self.coders(count).zip(&mut told) {
            if coding & (1 << i) == 0 {
                continue;
            }
            if refining {
                coder.refine.lock().refinement_sent(pts, now);
            } else if fresh {
                coder.refine.lock().fresh(pts, now);
            } else {
                coder.refine.lock().other_sent(now);
            }
            *told = self.tell(&mut lives, i, fps);
            coder.submitted(Submitted { pts_us: pts, at_us: now, stripes, region: shown });
            engines::ENGINES.busy(now);
            // Marked in flight before the submit, as the callback that clears the mark may run
            // inside it: marked after, a keyframe already out read as one still being encoded
            // and the refreshes of the next 400 ms went unanswered.
            if options.get(i).and_then(Option::as_ref).is_some_and(|(o, _)| o.force_keyframe) {
                coder.keyframe_submitted_us.store(now.max(1), Ordering::Relaxed);
            }
        }
        let handed = self.hand(&mut lives, &options, told);
        let (image, captured) = (frame.image.clone(), frame.capture_ts_us);
        drop(lives);
        drop(held);
        drop(replaced);
        self.gate.enter(turn);
        let inside = engines::ENGINES.enter();
        let outcomes = self.submit(&handed, &image, pts, &options);
        drop(inside);
        if !self.gate.back(turn) {
            tracing::debug!(stream = %self.id, pts, "an encode given up on came back: dropped");
            return Attempt::GivenUp;
        }
        if let Some(helper) = handed.helper {
            self.encoder.lock().helper = Some(helper);
        }
        let mut stale = false;
        let mut failed = false;
        for (((i, coder), outcome), asked) in self.coders(count).zip(outcomes).zip(&options) {
            let (Some(outcome), Some((options, standalone))) = (outcome, asked) else { continue };
            match outcome {
                Ok(()) => {
                    if *standalone {
                        self.counters.refreshes_idr.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(e) => {
                    let _failed = coder.returned(pts, Source::<P>::now_us());
                    stale |= matches!(e, CodecError::WrongSize { .. });
                    failed = true;
                    match e {
                        // The 4:4:4 session went in before ScreenCaptureKit switched to `xf44`;
                        // the capture is on its way ([`Pipeline::start_rebuild`]).
                        CodecError::NotFullChroma(_) => {
                            tracing::debug!(stream = %self.id, i, error = %e, "a 4:2:0 capture ahead of the switch");
                        }
                        // The session was rebuilt for a size the capture has not switched to yet.
                        CodecError::WrongSize { .. } => {
                            tracing::debug!(stream = %self.id, i, error = %e, "a capture of the old size dropped");
                        }
                        CodecError::EncoderLost { .. } => {
                            let session = coder.session.load(Ordering::Relaxed);
                            if self.encoder_lost.swap(session, Ordering::Relaxed) != session {
                                tracing::warn!(stream = %self.id, i, session, error = %e, "the encoder session is gone: new ones");
                                self.geometry_wake.notify_one();
                            }
                        }
                        _ => tracing::warn!(stream = %self.id, i, error = %e, "encode failed"),
                    }
                    if options.force_keyframe {
                        coder.keyframe_submitted_us.store(0, Ordering::Relaxed);
                    }
                    let mut pending = coder.pending.lock();
                    pending.keyframe |= options.force_keyframe;
                    pending.refresh |= options.force_ltr_refresh;
                    pending.acked.extend(options.acked_ltr.iter().copied());
                }
            }
        }
        if stale {
            // The capture this turn coded, unless the target was let go of meanwhile
            // ([`Self::forget_held`]): no other encode can have held another.
            let mut held = self.held.lock();
            let dropped = if held.as_ref().is_some_and(|frame| frame.capture_ts_us == captured) {
                self.held_us.store(0, Ordering::Relaxed);
                self.owed.store(false, Ordering::Relaxed);
                held.take()
            } else {
                None
            };
            drop(held);
            drop(dropped);
            return Attempt::Nothing;
        }
        if failed {
            self.owed.store(true, Ordering::Relaxed);
            return Attempt::Failed;
        }
        if fresh {
            self.fed(fps);
        }
        Attempt::Sent
    }

    /// What the encode's submits need once its locks are let go, taken out of `lives`: the
    /// sessions of the coders `options` has a frame for, what each is to be told, and the
    /// [`Helper`] when both stripes are coded, which the encode puts back once it is done.
    fn hand(
        &self,
        lives: &mut Lives<P::Video>,
        options: &[Option<(FrameOptions, bool)>; Stripe::MAX],
        told: [Told; Stripe::MAX],
    ) -> Handed<P::Video> {
        let [top, lower] = options;
        let session = |live: &Live<P::Video>, asked: bool| {
            live.session.as_ref().filter(|_| asked).map(Arc::clone)
        };
        let sessions = [session(&lives.top, top.is_some()), session(&lives.lower, lower.is_some())];
        let both = sessions.iter().all(Option::is_some);
        if both && lives.helper.is_none() {
            lives.helper = Helper::start(self.id)
                .inspect_err(|e| tracing::warn!(stream = %self.id, error = %e, "no thread for the lower stripe: one after the other"))
                .ok();
        }
        let helper = if both { lives.helper.take() } else { None };
        Handed { sessions, told, helper }
    }

    /// Tell the sessions `handed` what they are to be told, then submit `image` stamped `pts`
    /// to each that `options` has a frame for: the lower stripe on the [`Helper`]'s thread
    /// while the top one is submitted here, when both are. No lock is held: these are the
    /// calls into the encoder. What became of each; `None` for a coder not asked or with no
    /// session in (a stream between its open and its first build, or a test's).
    fn submit(
        &self,
        handed: &Handed<P::Video>,
        image: &<Source<P> as CaptureSource>::Image,
        pts: u64,
        options: &[Option<(FrameOptions, bool)>; Stripe::MAX],
    ) -> [Option<Result<(), CodecError>>; Stripe::MAX] {
        for (i, (session, told)) in handed.sessions.iter().zip(handed.told).enumerate() {
            if let Some(session) = session {
                self.tell_session(i, session, told);
            }
        }
        let [top, lower] = &handed.sessions;
        let [top_options, lower_options] = options;
        let encode = |session: &Option<Arc<P::Video>>, options: &Option<(FrameOptions, bool)>| {
            session.as_ref().zip(options.as_ref()).map(|(s, (o, _))| s.encode(image, pts, o))
        };
        let lower_handed = match (&handed.helper, lower, lower_options) {
            (Some(helper), Some(session), Some((options, _))) => {
                let (session, image, options) =
                    (Arc::clone(session), image.clone(), options.clone());
                helper.submit(Box::new(move || session.encode(&image, pts, &options)))
            }
            _no_helper => false,
        };
        let top = encode(top, top_options);
        let lower = match (&handed.helper, lower_handed) {
            (Some(helper), true) => Some(helper.wait()),
            _inline => encode(lower, lower_options),
        };
        [top, lower]
    }

    /// When [`repair_loop`] should next look at the held capture; `None` while nothing is owed
    /// or asked for.
    fn repair_at(&self) -> Option<u64> {
        let captured = self.held_us.load(Ordering::Relaxed);
        if captured == 0 {
            return None;
        }
        let count = self.coder_count();
        let (mut keyframe, mut refresh, mut refine) = (false, false, None::<u64>);
        for (_i, coder) in self.coders(count) {
            {
                let pending = coder.pending.lock();
                keyframe |= pending.keyframe;
                refresh |= pending.refresh;
            }
            if let Some(due) = self.refine_due(coder, count) {
                refine = Some(refine.map_or(due, |at| at.min(due)));
            }
        }
        // A session waiting to be put in wants its keyframe from whatever is held.
        let keyframe = keyframe || self.staged.lock().is_some();
        repair_at(
            Asks { owed: self.owed.load(Ordering::Relaxed), refresh, keyframe, refine },
            captured,
            self.due_at(),
            self.fps_ceiling.load(Ordering::Relaxed),
        )
    }

    /// When `coder`'s still picture may next be refined, one of `count` coders; `None` when it
    /// is not worth one. The stream's refinements take at most [`REFINE_BUDGET_MS`] of the
    /// encoder's rate together, each coder its share.
    fn refine_due(&self, coder: &Coder, count: usize) -> Option<u64> {
        let period = period_us(self.fps.load(Ordering::Relaxed));
        let budget =
            (self.counters.encoder_bps.load(Ordering::Relaxed).saturating_mul(REFINE_BUDGET_MS)
                / 8_000)
                .checked_div(u64::try_from(count).unwrap_or(1))
                .unwrap_or(0);
        coder.refine.lock().due_us(period, budget)
    }

    /// The earliest the cadence gate lets a capture through at the rung in force.
    fn due_at(&self) -> u64 {
        Pace::resume(self.pace_us.load(Ordering::Relaxed)).due_at(self.fps.load(Ordering::Relaxed))
    }

    /// Send the held capture again at `now`, unless the target is no longer on screen.
    fn repair_now(&self, now: u64) -> Attempt {
        if self.target_hidden.load(Ordering::Relaxed) || self.suspected_at(now) {
            self.forget_held();
            return Attempt::Nothing;
        }
        self.try_encode(None, now)
    }

    /// While an encode is inside the encoder: how long the beat has charged it, and its
    /// patience, microseconds ([`StuckWatch`]).
    #[cfg(test)]
    fn coding(&self) -> Option<(u64, u64)> {
        let inside = self.gate.inside.load(Ordering::Acquire);
        let stuck = self.stuck.lock();
        (inside != 0)
            .then(|| (if stuck.seen == inside { stuck.charged_us } else { 0 }, stuck.patience_us))
    }

    /// The beat's look at a lost session still in force at `now`: once its replacement may be
    /// built ([`LostRetry`]), the geometry tick is woken for it, once for each moment.
    fn retry_lost(&self, now: u64) {
        let lost = self.encoder_lost.load(Ordering::Relaxed);
        if lost == 0 || self.top.session.load(Ordering::Relaxed) != lost {
            return;
        }
        let encoded = self.counters.encoded.load(Ordering::Relaxed);
        let mut retry = self.lost_retry.lock();
        let due = retry.0.due_us(encoded);
        if now >= due && retry.1 != due {
            retry.1 = due;
            drop(retry);
            self.geometry_wake.notify_one();
        }
    }

    /// The beat's look at the encode inside the encoder at `now` ([`StuckWatch`]): one that has
    /// been there [`ENCODE_STUCK`] of the worker's own time is given up on. Its sessions are
    /// numbered out, so what they return later is dropped as a replaced session's, and the next
    /// encode takes them out of force. The turn is taken from it, a new thread takes the
    /// captures in case it held the old one, the repair loop stops waiting on it, and the
    /// geometry tick builds new sessions at the size in force ([`Pipeline::follow_lost`]): a
    /// keyframe, and the stream goes on. The thread inside the encoder is left there, holding
    /// no lock; it ends once the call returns, if it ever does.
    fn unstick(self: &Arc<Self>, now: u64) {
        let inside = self.gate.inside.load(Ordering::Acquire);
        let Some(turn) = self.stuck.lock().look(inside, now) else { return };
        let given_up = self.gate.give_up(turn, || {
            self.top.session.store(0, Ordering::Relaxed);
            self.lower.session.store(0, Ordering::Relaxed);
        });
        if !given_up {
            return;
        }
        let encoded = self.counters.encoded.load(Ordering::Relaxed);
        let patience_us = {
            let mut stuck = self.stuck.lock();
            let patience_us = stuck.patience_us;
            stuck.gave_up(encoded);
            patience_us
        };
        let replaced = self.counters.encoders_replaced.fetch_add(1, Ordering::Relaxed);
        for coder in [&self.top, &self.lower] {
            coder.in_flight.lock().clear();
        }
        tracing::warn!(
            stream = %self.id,
            turn,
            patience_ms = patience_us / 1_000,
            replaced = replaced.saturating_add(1),
            "an encode has not come back from the encoder: new sessions"
        );
        if let Err(e) = start_encode_thread(self) {
            tracing::warn!(stream = %self.id, error = %e, "no new encode thread: captures wait on the old one");
        }
        self.unstuck.notify_waiters();
        self.geometry_wake.notify_one();
    }

    /// Hand a frame's video datagrams on: straight to QUIC while no audio flows, through the
    /// lane while it does, and always behind video already waiting there.
    fn send_video(&self, datagrams: &[Bytes], now: u64) {
        let mut lane = self.lane.lock();
        let last_audio = self.sound.get().map_or(0, |sound| sound.last_us());
        let audio = last_audio != 0 && now.saturating_sub(last_audio) < LANE_AUDIO_HOLD_US;
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

    /// A number for an encoder session about to be built, to [`Self::install`] it under.
    fn next_session(&self) -> u64 {
        self.sessions.fetch_add(1, Ordering::Relaxed).wrapping_add(1)
    }

    /// Coder `index`'s session `session` produced an access unit: the coder's when that session
    /// is the one in force, dropped when it was replaced.
    ///
    /// The session in force is set under the encoder's write lock, before the new session can be
    /// given a frame, so every packet of the new session finds it set.
    fn on_session_packet(&self, index: usize, session: u64, packet: &EncodedPacket) {
        if self.coder(index).session.load(Ordering::Relaxed) != session {
            let replaced = self.replaced_dropped.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(
                stream = %self.id,
                index,
                session,
                replaced,
                pts_us = packet.pts_us,
                keyframe = packet.keyframe,
                "a replaced encoder session's frame dropped"
            );
            return;
        }
        self.on_packet(index, packet);
    }

    /// Coder `index`'s session produced an access unit.
    ///
    /// A refinement of the still picture goes out like any frame but is no part of what the
    /// stream reports: not an encoded frame (so a still source still reads idle), not in the
    /// encode or latency figures, and not in the spend the layer gate weighs. A striped
    /// capture counts once, when its last stripe is back, with the slowest stripe's encode.
    fn on_packet(&self, index: usize, packet: &EncodedPacket) {
        let now = now::<P>();
        let coder = self.coder(index);
        let bytes = u64::try_from(packet.data.len()).unwrap_or(u64::MAX);
        let refined_at =
            coder.refine.lock().returned(packet.pts_us, packet.mse.map(|mse| mse.luma), bytes);
        let returned = coder.returned(packet.pts_us, now);
        engines::ENGINES.busy(now);
        let (took, stripes, region) = returned
            .map_or((None, 0, None), |(took, frame)| (Some(took), frame.stripes, frame.region));
        if let Some(took) = took.filter(|_| !packet.keyframe) {
            coder.refine.lock().took(took);
        }
        self.repair.notify_one();
        let latency = now.saturating_sub(packet.pts_us);
        let encoded = if refined_at.is_some() {
            self.counters.encoded.load(Ordering::Relaxed)
        } else {
            self.counters.video_bytes.fetch_add(bytes, Ordering::Relaxed);
            // A whole picture is its own capture; only stripes wait for their siblings. A
            // stripe's session is aligned, so its capture is back before the next is joined.
            let last = if stripes.count_ones() > 1 {
                self.join.lock().returned(index, packet.pts_us, took, packet.keyframe)
            } else {
                Some((took, packet.keyframe))
            };
            if let Some((took, keyframe)) = last {
                if let Some(took) = took {
                    self.counters.encode.lock().push(took);
                    self.watch_encoder(took, keyframe);
                }
                self.counters.latency_max_us.fetch_max(latency, Ordering::Relaxed);
                self.counters.latency_sum_us.fetch_add(latency, Ordering::Relaxed);
                // The quiet geometry probe learns the source draws again from this, not its
                // backstop.
                if self.source_idle.load(Ordering::Relaxed)
                    && self.source_idle.swap(false, Ordering::Relaxed)
                {
                    self.geometry_wake.notify_one();
                }
                self.counters.encoded.fetch_add(1, Ordering::Relaxed)
            } else {
                self.counters.encoded.load(Ordering::Relaxed)
            }
        };
        if packet.keyframe {
            coder.keyframe_submitted_us.store(0, Ordering::Relaxed);
            let estimate = keyframe_estimate(coder.keyframe_bytes.load(Ordering::Relaxed), bytes);
            coder.keyframe_bytes.store(estimate, Ordering::Relaxed);
            coder.keyframe_deferred_us.store(0, Ordering::Relaxed);
        }
        if coder.ltr.lock().on_packet(packet.keyframe, packet.ltr_token, now) {
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
                index,
                frame = encoded,
                bytes = packet.data.len(),
                keyframe = packet.keyframe,
                encode_ms = latency / 1000,
                "keyframe encoded"
            );
        }
        // A refinement's stamp is a period early for the encoder's sake; the client is told
        // when it was sent, which is what its delay trend reads.
        #[expect(clippy::cast_possible_truncation, reason = "low bits by design")]
        let capture_ts_us = refined_at.unwrap_or(packet.pts_us) as u32;
        let frame = EncodedFrame {
            data: &packet.data,
            keyframe: packet.keyframe,
            ltr_token: packet.ltr_token,
            ltr_refresh: packet.ltr_refresh,
            discardable: packet.discardable,
            capture_ts_us,
            stripes,
            region,
        };
        let max = self.sink.max_size().map_or(MAX_DATAGRAM, |m| m.min(MAX_DATAGRAM));
        let mut packetizer = coder.packetizer.lock();
        packetizer.set_max_datagram(max);
        // The data leaves before the parity is computed over it (MEASUREMENTS.md, "data before
        // parity"). Under the packetizer's lock, so a NACK's answer never overtakes the frame.
        let cut = packetizer
            .packetize(&frame, send_ms_lo(now), |datagrams| self.send_video(datagrams, now))
            .map(|sent| {
                let wire: usize = sent.datagrams.iter().map(Bytes::len).sum();
                let wire =
                    if refined_at.is_some() { u64::try_from(wire).unwrap_or(u64::MAX) } else { 0 };
                coder.refine_wire.store(wire, Ordering::Relaxed);
            });
        drop(packetizer);
        if let Err(e) = cut {
            tracing::warn!(stream = %self.id, index, error = %e, "packetize failed");
        }
    }
}

impl<P: Platform> engines::Contender for Shared<P> {
    fn focused(&self) -> bool {
        self.focused.load(Ordering::Relaxed)
    }

    fn give_way(&self, until_us: u64) -> bool {
        self.give_way_until_us.store(until_us, Ordering::Relaxed);
        if until_us == 0 {
            // Back to what the client asked: the encoder's own windows bring it down again if
            // it still cannot keep that.
            self.raise_ceiling(self.fps_asked.load(Ordering::Relaxed));
            return false;
        }
        let ceiling = self.fps_ceiling.load(Ordering::Relaxed);
        let slower = slower_rung(ceiling);
        if slower >= ceiling {
            return false;
        }
        self.lower_ceiling(slower, "a focused stream's encoder fell behind: giving way");
        true
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
    /// It stayed inside the encoder until its sessions were given up on ([`Shared::unstick`]),
    /// and came back after: nothing it did counts.
    GivenUp,
}

impl<P: Platform> Shared<P> {
    /// A receiver report on coder `index`'s media stream: acknowledged LTR tokens go to its
    /// session and its loss to its parity. The whole picture's or the top stripe's also goes
    /// to the rate controller, the audio copies and the layer gate, which serve the stream: the
    /// lower stripe's report carries the same link, and counted twice it would cut twice.
    fn report(
        &self,
        index: usize,
        report: &ReceiverReport,
        path: Option<PathSample>,
    ) -> Option<Decision> {
        let coder = self.coder(index);
        let acked = {
            let mut ltr = coder.ltr.lock();
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
            // hand the new session the old one's tokens ([`Coder::rebuilt`]).
            coder.pending.lock().acked.extend(acked);
            drop(ltr);
            count
        };
        self.counters.ltr_acked.fetch_add(acked, Ordering::Relaxed);
        let sent_total = coder.packetizer.lock().datagrams_sent();
        let previous = coder.sent_at_report.swap(sent_total, Ordering::Relaxed);
        let sent = u32::try_from(sent_total.saturating_sub(previous)).unwrap_or(u32::MAX);
        let permille = coder.redundancy.lock().on_report(report, sent);
        if index != 0 {
            coder.packetizer.lock().set_parity_permille(permille);
            return None;
        }
        self.follow_layers(permille > Redundancy::MIN);
        let parity_moved = {
            let mut packetizer = coder.packetizer.lock();
            let moved = packetizer.parity_permille() != permille;
            packetizer.set_parity_permille(permille);
            moved
        };
        let decision = self.rate.lock().on_report(report, sent, path);
        let Some(decision) = decision else {
            if parity_moved {
                // The parity's share of the target moved, so the encoder's did too. Read first:
                // nothing is held while the encoder is asked for anything.
                let target = self.rate.lock().target_bps();
                self.apply_bitrate(target);
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
        let mut gate = self.chroma.lock();
        if let Some(chroma) = gate.update(decision.target_bps) {
            let (enter_bps, leave_bps) = gate.band();
            tracing::info!(stream = %self.id, ?chroma, target_bps = decision.target_bps, enter_bps, leave_bps, "chroma follows the rate");
        }
        drop(gate);
        Some(decision)
    }

    /// Turn temporal layers on or off as the link's loss (`lossy`) and the frames the encoder
    /// dropped since the last report say ([`LayerGate`]); the next encode tells the session.
    fn follow_layers(&self, lossy: bool) {
        let total = self.encoder_dropped.load(Ordering::Relaxed);
        let dropped = total.saturating_sub(self.dropped_reported.swap(total, Ordering::Relaxed));
        let Some(on) = self.layers.lock().update(lossy, dropped) else { return };
        self.layers_wanted.store(on, Ordering::Relaxed);
        tracing::debug!(stream = %self.id, on, lossy, dropped, "temporal layers follow the link");
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
    fn dropped(&self, now: u64, count: usize) {
        self.counters.dropped.fetch_add(1, Ordering::Relaxed);
        for (_i, coder) in self.coders(count) {
            if self.standalone_fits(coder, now) {
                coder.pending.lock().refresh = true;
            }
        }
    }

    /// Whether a wanted keyframe should be encoded now rather than put off for an LTR refresh.
    ///
    /// Deferring needs somewhere to fall back to: with no usable long-term reference a refresh
    /// comes back as an IDR anyway, so the keyframe goes out. Past that the drain budget decides
    /// ([`Self::standalone_fits`]).
    fn keyframe_admitted(&self, coder: &Coder, now: u64) -> bool {
        !coder.ltr_usable() || self.standalone_fits(coder, now)
    }

    /// Whether a picture that stands on its own is worth the bytes right now: the link drains
    /// one inside [`KEYFRAME_DRAIN_MS`], or it has been put off for [`KEYFRAME_VALVE_US`].
    ///
    /// A link that can carry one ends the episode, and not only a keyframe encoded: the next
    /// collapse then starts its own clock and is counted as its own episode, where a stale start
    /// would open the valve on its first frame.
    fn standalone_fits(&self, coder: &Coder, now: u64) -> bool {
        if keyframe_fits(
            coder.keyframe_bytes.load(Ordering::Relaxed),
            self.held_bytes(),
            self.counters.bitrate_bps.load(Ordering::Relaxed),
        ) {
            coder.keyframe_deferred_us.store(0, Ordering::Relaxed);
            return true;
        }
        // Counted where the clock starts, so the number is deferral episodes and not the frames
        // each one spans — at 60 fps a one-second run would otherwise read as sixty.
        match coder.keyframe_deferred_us.compare_exchange(
            0,
            now,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_started) => {
                self.counters.keyframes_deferred.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    stream = %coder.media,
                    estimate = coder.keyframe_bytes.load(Ordering::Relaxed),
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
    ///
    /// A keyframe being encoded answers it ([`KEYFRAME_IN_FLIGHT_US`]).
    fn request_refresh(&self, index: usize, last_good_frame: u32, keyframe: bool) {
        let coder = self.coder(index);
        tracing::debug!(stream = %coder.media, last_good_frame, keyframe, "refresh requested");
        self.counters.refreshes.fetch_add(1, Ordering::Relaxed);
        let submitted = coder.keyframe_submitted_us.load(Ordering::Relaxed);
        if submitted != 0 && now::<P>().saturating_sub(submitted) < KEYFRAME_IN_FLIGHT_US {
            tracing::debug!(stream = %coder.media, "the keyframe being encoded answers the refresh");
            return;
        }
        if keyframe {
            coder.ltr.lock().client_lost();
            coder.pending.lock().keyframe = true;
        } else {
            coder.pending.lock().refresh = true;
        }
        self.repair.notify_one();
    }

    /// Answer a clock probe (see [`ScreenStream::echo_clock`]).
    fn echo_clock(&self, sent_us: u64, arrived: Instant) {
        let waited = arrived.elapsed();
        let echoed_us = now::<P>();
        let waited_us = u64::try_from(waited.as_micros()).unwrap_or(u64::MAX);
        let received_us = echoed_us.saturating_sub(waited_us);
        let echo = ClockEcho::new(sent_us, received_us, echoed_us);
        let _taken = self.send(&[echo.datagram(self.id.0, send_ms_lo(echoed_us))]);
    }

    /// Retransmit fragments of a recent frame, unless the transport is holding more than the
    /// frame budget (see [`ScreenStream::nack`]).
    fn nack(&self, index: usize, frame: u32, fragments: &[u16]) {
        let coder = self.coder(index);
        if !self.frame_fits() {
            tracing::debug!(stream = %coder.media, frame, held = self.held_bytes(), "nack not answered: transport is holding frames");
            return;
        }
        let datagrams = coder.packetizer.lock().retransmit(frame, fragments);
        if datagrams.is_empty() {
            tracing::debug!(stream = %coder.media, frame, "nack for a frame outside the history");
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

/// The sessions a build made for a stream: one for the whole picture, or one per stripe.
pub struct Built<P: Platform> {
    top: P::Video,
    lower: Option<P::Video>,
    /// Where the stripes sit, when there are two.
    layout: Option<[CodedStripe; 2]>,
}

impl<P: Platform> std::fmt::Debug for Built<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Built").field("layout", &self.layout).finish_non_exhaustive()
    }
}

/// Build the sessions whose packets flow back into `shared`, off the runtime: creating a
/// VideoToolbox session holds the calling thread for milliseconds, which on a runtime worker
/// holds up every task queued behind it (MEASUREMENTS.md, "encoder sessions off the runtime").
async fn build_encoder<P: Platform>(
    shared: &Weak<Shared<P>>,
    config: EncoderConfig,
    shown: (u32, u32),
    sessions: [u64; 2],
    layout: Option<[CodedStripe; 2]>,
) -> Result<Built<P>, ScreenError> {
    start_encoder(shared, config, shown, sessions, layout).built().await
}

/// [`build_encoder`] without waiting for it: the build runs on a thread of its own from here
/// on. Its packets are the stream's once it is installed under `sessions` ([`Shared::install`]).
///
/// The sessions code the capture's padded surface ([`CaptureConfig::surface`]); each keyframe's
/// SPS is rewritten to show the picture's own `shown` size, so the client decodes that size and
/// never sees the padding ([`slopty_codec::conformance`]). With a `layout`, one session codes
/// each stripe's rows, both built at once, and the lower one shows the picture's rows down to
/// its last.
///
/// The thread is not the runtime's blocking pool: a runtime waits for its blocking tasks as it
/// shuts down, so a build that never came back from VideoToolbox would hold the worker's exit,
/// or a test's end, for good.
fn start_encoder<P: Platform>(
    shared: &Weak<Shared<P>>,
    config: EncoderConfig,
    shown: (u32, u32),
    sessions: [u64; 2],
    layout: Option<[CodedStripe; 2]>,
) -> Building<P> {
    let weak = Weak::clone(shared);
    let (answer_tx, answer) = oneshot::channel();
    let build = move || {
        let [top_session, lower_session] = sessions;
        let Some([top, lower]) = layout else {
            let top = open_session(&weak, config, shown, top_session, 0, None)?;
            return Ok(Built { top, lower: None, layout: None });
        };
        let rows =
            |stripe: CodedStripe| shown.1.saturating_sub(stripe.coded_top).min(stripe.coded_rows);
        let (built_top, built_lower) = std::thread::scope(|scope| {
            let lower = std::thread::Builder::new()
                .name(format!("{BUILD_THREAD}-lower"))
                .spawn_scoped(scope, || {
                    open_session(
                        &weak,
                        config,
                        (shown.0, rows(lower)),
                        lower_session,
                        1,
                        Some(lower),
                    )
                });
            let built_top =
                open_session(&weak, config, (shown.0, rows(top)), top_session, 0, Some(top));
            let built_lower = match lower {
                Ok(lower) => lower.join().unwrap_or(Err(CodecError::Os {
                    call: "the lower stripe's build",
                    status: -1,
                })),
                Err(_spawn) => {
                    Err(CodecError::Os { call: "the lower stripe's thread", status: -1 })
                }
            };
            (built_top, built_lower)
        });
        Ok(Built { top: built_top?, lower: Some(built_lower?), layout })
    };
    let spawned = std::thread::Builder::new().name(BUILD_THREAD.to_owned()).spawn(move || {
        // A wait given up on is gone: what the build made is dropped here, on its own thread,
        // as a replaced session is ([`retire`]).
        if let Err(unclaimed) = answer_tx.send(build()) {
            let _inside = engines::ENGINES.enter();
            drop(unclaimed);
        }
    });
    if let Err(e) = spawned {
        // The answer's sender went with the closure: the wait answers `BuildLost`.
        tracing::warn!(error = %e, "no thread to build an encoder session on");
    }
    let stream = shared.upgrade().map(|shared| shared.id);
    Building { answer, stream, patience: BUILD_STUCK, charged: Duration::ZERO, looked_us: None }
}

/// The name of a session build's thread ([`start_encoder`]), for a stack read when one stays.
const BUILD_THREAD: &str = "slopty-build-encoder";

/// Sessions being built on their thread ([`start_encoder`]), and the wait for them: how long it
/// has been charged on the worker's own time, against its patience ([`BUILD_STUCK`]).
struct Building<P: Platform> {
    answer: oneshot::Receiver<Result<Built<P>, CodecError>>,
    /// The stream it builds for, to name in the log.
    stream: Option<StreamId>,
    patience: Duration,
    /// The worker's own time the wait has been charged, a look at most [`STUCK_CREDIT`] each.
    charged: Duration,
    /// When the wait last looked, `None` before its first look.
    looked_us: Option<u64>,
}

impl<P: Platform> Building<P> {
    /// The sessions once they are built. Cancel-safe: a dropped call leaves the build running,
    /// and its charge, for the next one.
    ///
    /// A build still inside VideoToolbox once its patience is charged is given up on, as the beat
    /// gives up an encode ([`Shared::unstick`]): its thread is left there, holding nothing anyone
    /// waits on, and drops what it made if it ever comes back. Only the time the wait itself ran
    /// on time is charged, so a starved machine that holds the worker for seconds does not give
    /// up a build that was only waiting with it.
    ///
    /// # Errors
    ///
    /// When VideoToolbox refuses a session or never answers, or the build's thread died.
    async fn built(&mut self) -> Result<Built<P>, ScreenError> {
        loop {
            tokio::select! {
                biased;
                answer = &mut self.answer => {
                    return Ok(answer.map_err(|_died| ScreenError::BuildLost)??);
                }
                () = tokio::time::sleep(BUILD_LOOK) => {
                    if self.look(now::<P>()) {
                        tracing::warn!(
                            stream = ?self.stream,
                            patience = ?self.patience,
                            "a session build has not come back from VideoToolbox: given up"
                        );
                        return Err(ScreenError::BuildStuck(self.patience));
                    }
                }
            }
        }
    }

    /// Charge the wait for a look at `now_us`: whether its patience has run out.
    fn look(&mut self, now_us: u64) -> bool {
        let ran = self.looked_us.map_or(0, |looked| now_us.saturating_sub(looked));
        self.looked_us = Some(now_us);
        self.charged = self.charged.saturating_add(Duration::from_micros(ran).min(STUCK_CREDIT));
        self.charged >= self.patience
    }
}

impl<P: Platform> Drop for Building<P> {
    /// Sessions built and never taken are dropped off the runtime ([`retire`]): invalidating one
    /// waits for its callbacks. A build still under way drops its own once it is done.
    fn drop(&mut self) {
        if let Ok(Ok(built)) = self.answer.try_recv() {
            retire(Some(built));
        }
    }
}

/// Coder `index`'s session for [`start_encoder`], made on the calling thread, which it holds
/// for as long as VideoToolbox takes: the whole picture, or `stripe`'s rows of it.
fn open_session<P: Platform>(
    shared: &Weak<Shared<P>>,
    config: EncoderConfig,
    shown: (u32, u32),
    session: u64,
    index: usize,
    stripe: Option<CodedStripe>,
) -> Result<P::Video, CodecError> {
    let weak = Weak::clone(shared);
    let rows = stripe.map_or(config.height, |stripe| stripe.coded_rows);
    let padded = shown != (config.width, rows);
    let sink = move |mut packet: EncodedPacket| {
        let Some(shared) = weak.upgrade() else { return };
        if padded
            && packet.keyframe
            && let Err(e) = conformance::crop_access_unit(&mut packet.data, shown)
        {
            tracing::warn!(stream = %shared.id, index, error = %e, ?shown, "keyframe shows its padding");
        }
        shared.on_session_packet(index, session, &packet);
    };
    let _inside = engines::ENGINES.enter();
    match stripe {
        None => P::Video::new(config, sink),
        Some(stripe) => P::Video::stripe(config, stripe, sink),
    }
}

/// New encoder sessions being built for the size a window settled on, the quality the client
/// asked for, or the stripes turning on or off.
///
/// [`Pipeline::check_geometry`] and [`Pipeline::set_quality`] start it, and the stream's task
/// waits for [`Self::built`] beside its commands, so input for the window is not held behind the
/// 3.5–42 ms of a VideoToolbox session (MEASUREMENTS.md, "encoder sessions off the runtime" and
/// "input behind a quality change"). [`Pipeline::finish_rebuild`] puts it in; until then the
/// stream goes on at its old size.
pub struct Rebuild<P: Platform> {
    encoder: Building<P>,
    /// The numbers the sessions are built under, to install them as.
    sessions: [u64; 2],
    native: (u32, u32),
    desired: CaptureConfig,
    config: EncoderConfig,
    /// The stripes the sessions code, when there are two.
    layout: Option<[CodedStripe; 2]>,
    /// The target changed size: the client is told the stream's new one once it is in.
    resized: bool,
}

impl<P: Platform> std::fmt::Debug for Rebuild<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rebuild")
            .field("native", &self.native)
            .field("striped", &self.layout.is_some())
            .finish_non_exhaustive()
    }
}

impl<P: Platform> Rebuild<P> {
    /// The new sessions once they are built. Cancel-safe: a dropped call leaves the build
    /// running for the next one. Not to be called again once it has answered.
    ///
    /// A build that stays inside VideoToolbox past its patience (`BUILD_STUCK`) answers
    /// [`ScreenError::BuildStuck`]: the caller goes on as for any failed build
    /// ([`Pipeline::rebuild_failed`]), and the geometry tick builds again where sessions are
    /// still wanted.
    ///
    /// # Errors
    ///
    /// When VideoToolbox refuses a session or does not answer, or the build's thread died.
    pub async fn built(&mut self) -> Result<Built<P>, ScreenError> {
        self.encoder.built().await
    }
}

/// The newest capture, waiting for the stream's encode thread ([`start_encode_thread`]).
struct Mailbox<F> {
    slot: Mutex<Option<F>>,
    wake: parking_lot::Condvar,
}

impl<F> Default for Mailbox<F> {
    fn default() -> Self {
        Self { slot: Mutex::new(None), wake: parking_lot::Condvar::new() }
    }
}

impl<F> Mailbox<F> {
    /// Leave `frame` for the encode thread, in place of any it has not taken yet: the encoder
    /// is behind, and the newer capture is the one worth its time. Returns the one replaced.
    fn post(&self, frame: F) -> Option<F> {
        let superseded = self.slot.lock().replace(frame);
        self.wake.notify_one();
        superseded
    }

    /// The frame left for the encode thread, waiting up to `patience` for one.
    fn take(&self, patience: Duration) -> Option<F> {
        let mut slot = self.slot.lock();
        if slot.is_none() {
            let _timed_out = self.wake.wait_for(&mut slot, patience);
        }
        slot.take()
    }

    /// Drop the frame waiting, if any.
    fn clear(&self) {
        let dropped = self.slot.lock().take();
        drop(dropped);
    }
}

/// How long the encode thread waits for a capture before it looks whether its stream is gone.
const ENCODE_THREAD_IDLE: Duration = Duration::from_millis(500);

/// The thread a stream's captures are encoded on, taking them from its mailbox
/// ([`Shared::post`]).
///
/// VideoToolbox may encode a frame inside `VTCompressionSessionEncodeFrame`
/// (`VTCompressionSession.h`: "The `kVTEncodeInfo_Asynchronous` bit may be set if the encode ran
/// asynchronously"), and the low-latency encoder does whenever the picture's sides are multiples
/// of 16: the call returns once the frame is out, 15.3 ms at 3024 × 1968 (MEASUREMENTS.md,
/// "Stream sides padded to 16"). On the capture's own queue that held back the next capture, so
/// a 60 Hz display at that size was captured at 30. Here the capture only leaves its frame; a
/// capture that arrives while the encoder is still busy replaces the one waiting, so the encoder
/// always takes the newest picture and nothing queues behind it.
///
/// The thread ends once the stream's [`Shared`] is gone, or once another took its place
/// ([`Shared::unstick`]).
fn start_encode_thread<P: Platform>(shared: &Arc<Shared<P>>) -> Result<(), ScreenError> {
    let weak = Arc::downgrade(shared);
    let this = shared.encode_thread.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    std::thread::Builder::new()
        .name(format!("slopty-encode-{}", shared.id))
        .spawn(move || {
            slopty_platform::user_interactive_thread();
            while let Some(shared) = weak.upgrade()
                && shared.encode_thread.load(Ordering::Relaxed) == this
            {
                if let Some(frame) = shared.mailbox.take(ENCODE_THREAD_IDLE) {
                    shared.on_frame(frame);
                }
            }
        })
        .map(drop)
        .map_err(ScreenError::EncodeThread)
}

/// Drop a replaced encoder session on a thread of its own: invalidating it waits for its
/// callbacks, and the thread that replaced it is an encode's, about to code the new session's
/// keyframe ([`Shared::put_in`]), or the runtime's ([`Shared::install`]).
fn retire<V: Send + 'static>(old: Option<V>) {
    let Some(old) = old else { return };
    let spawned =
        std::thread::Builder::new().name("slopty-retire-encoder".to_owned()).spawn(move || {
            let _inside = engines::ENGINES.enter();
            drop(old);
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "no thread to retire an encoder session on: dropped in place");
    }
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
    /// What the Mac's screens show of the session; `None` when the platform cannot tell.
    console: Option<Console>,
    /// When the reads began.
    at: Instant,
    /// For a window that may be served from a display crop: whether it is on screen, and the crop
    /// it could be served from (on one display, uncovered), before any suspicion.
    window: Option<(bool, Option<Crop>)>,
}

/// Read the target's geometry: one window-list description for the window's bounds, on-screen
/// state and owner, the occlusion list above it, and the display under it; then what the Mac's
/// screens show of the session. Blocking; the geometry is timed into [`ScreenStats::bounds`],
/// and the bounds are left where the cursor loop reads them.
fn probe<P: Platform>(
    target: CaptureTarget,
    point_scale: f64,
    crop: bool,
    shared: &Shared<P>,
) -> Probe {
    let started = now::<P>();
    let at = Instant::now();
    let mut probe = match target {
        CaptureTarget::Display(_) => {
            Probe { bounds: Source::<P>::target_bounds(target), console: None, at, window: None }
        }
        CaptureTarget::Window(id) => match Source::<P>::window_state(id) {
            None => Probe { bounds: None, console: None, at, window: None },
            Some(state) => Probe {
                bounds: Some(state.bounds),
                console: None,
                at,
                window: crop.then(|| (state.on_screen, wanted_crop::<P>(id, &state, point_scale))),
            },
        },
    };
    shared.counters.bounds.lock().push(now::<P>().saturating_sub(started));
    let moved = std::mem::replace(&mut *shared.bounds.lock(), probe.bounds) != probe.bounds;
    if moved {
        shared.cursor_wake.notify_one();
    }
    // After the bounds are timed and handed to the cursor loop, which neither waits for it: one
    // more window-server round trip, 71 µs at the median (MEASUREMENTS.md, "the Mac's lock
    // state").
    probe.console = Source::<P>::console();
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

    /// A transition is in flight or waits to be settled.
    fn pending(&self) -> bool {
        self.0.lock().is_some()
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

/// A new size the target has to hold for [`RESIZE_HOLD`] before the stream is rebuilt for it.
///
/// A rebuild is a fresh encoder session and a keyframe, and a live drag of a window's corner
/// changes its size on every probe, which the accessibility API wakes on each step of the
/// drag: rebuilt at once, that is an IDR per step for as long as the drag lasts. Until the
/// size holds, the capture keeps its old output size and scales the window into it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct ResizeDebounce {
    /// The new size, and when it was first seen.
    candidate: Option<((u32, u32), Instant)>,
}

impl ResizeDebounce {
    /// The target measures `native` pixels at `at` while the stream is built for `current`: the
    /// size to rebuild for, once the same new size is seen again [`RESIZE_HOLD`] after it was
    /// first seen, with no other size in between.
    fn observe(
        &mut self,
        native: (u32, u32),
        current: (u32, u32),
        at: Instant,
    ) -> Option<(u32, u32)> {
        if native == current {
            self.candidate = None;
            return None;
        }
        match self.candidate {
            Some((size, since)) if size == native => {
                if at.saturating_duration_since(since) < RESIZE_HOLD {
                    return None;
                }
                self.candidate = None;
                Some(native)
            }
            _new_size => {
                self.candidate = Some((native, at));
                None
            }
        }
    }

    /// A size is waiting to hold.
    const fn pending(&self) -> bool {
        self.candidate.is_some()
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

/// The multiple an HEVC stream's sides are padded to: the low-latency encoder copies any other
/// size into a padded buffer of its own and queues it, and a Retina panel's size then waits
/// 68–111 ms (MEASUREMENTS.md, "encode time against frame size" and "Stream sides padded to
/// 16"). H.264 keeps the picture's even size until its encoder is measured for the same.
const HEVC_ALIGN: u32 = 16;

/// The multiple a stream of `codec` pads its sides to.
const fn align(codec: VideoCodec) -> u32 {
    match codec {
        VideoCodec::Hevc => HEVC_ALIGN,
        VideoCodec::H264 => 2,
    }
}

/// [`configs_padded`] with the codec's own padding, as every stream but a measurement's has it,
/// for a target of two pixels a point.
#[cfg(test)]
fn configs(
    native: (u32, u32),
    quality: &Quality,
    refresh_hz: Option<f64>,
) -> (CaptureConfig, EncoderConfig) {
    configs_padded(native, quality, refresh_hz, None, 2.0)
}

/// The quality's scale as the stream applies it.
fn scale_of(quality: &Quality) -> f64 {
    if quality.scale.is_finite() { f64::from(quality.scale).clamp(0.05, 1.0) } else { 1.0 }
}

/// A side of `px` native pixels at `scale`: even, and at least 2.
fn scaled_side(px: u32, scale: f64) -> u32 {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped")]
    let v = (f64::from(px) * scale).round().clamp(2.0, 16_384.0) as u32;
    v.next_multiple_of(2)
}

/// The size of the whole target `native` pixels in size at the quality's scale: the stream's
/// pixels, which input, the cursor, `Opened` and `Geometry` are in whatever region is streamed.
fn whole_size(native: (u32, u32), quality: &Quality) -> (u32, u32) {
    let scale = scale_of(quality);
    (scaled_side(native.0, scale), scaled_side(native.1, scale))
}

/// The region of a target `native` pixels in size that `quality` streams; `None` for all of
/// it ([`Region::within`]).
fn region_of(native: (u32, u32), quality: &Quality) -> Option<Region> {
    quality.region.and_then(|region| region.within(native))
}

/// Capture and encoder settings for a target at a requested quality, on a display refreshing at
/// `refresh_hz` when that is known, `point_scale` pixels a point.
///
/// The capture's picture is the target at the quality's scale, sides even, or only the
/// quality's region of it ([`region_of`]), sampled in the target's points; the encoder codes
/// the capture's surface, the picture padded to [`HEVC_ALIGN`] for HEVC ([`start_encoder`]
/// crops the stream back to the picture). `pad_to` pads to another multiple in its place: a
/// measurement's A/B, one stream padded and one not in one process
/// ([`Pipeline::open_padded`]).
///
/// A display draws no faster than it refreshes, so the frame rate is the one asked for or the
/// display's, whichever is lower. The capture runs at the display's own beat: the cadence gate
/// ([`Pace`]) picks the rung's frames from it, which a capture throttled to the rung's interval
/// cannot give it on a display whose beat does not divide that interval (MEASUREMENTS.md,
/// "capture on a 75 Hz display").
fn configs_padded(
    native: (u32, u32),
    quality: &Quality,
    refresh_hz: Option<f64>,
    pad_to: Option<u32>,
    point_scale: f64,
) -> (CaptureConfig, EncoderConfig) {
    let scale = scale_of(quality);
    let region = region_of(native, quality);
    let shown = region.map_or(native, |r| (u32::from(r.w), u32::from(r.h)));
    let (width, height) = (scaled_side(shown.0, scale), scaled_side(shown.1, scale));
    let align = pad_to.unwrap_or_else(|| align(quality.codec));
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped")]
    let display =
        refresh_hz.filter(|hz| hz.is_finite()).map(|hz| hz.round().clamp(1.0, 240.0) as u16);
    let fps = display.map_or(quality.fps, |hz| quality.fps.min(hz)).clamp(1, 240);
    let format = PixelFormat::Nv12Full;
    let capture = CaptureConfig {
        width,
        height,
        align,
        fps: if display.is_some() { 0 } else { fps },
        format,
        queue_depth: QUEUE_DEPTH,
        crop: None,
        region: region.map(|region| region::to_points(region, point_scale)),
    };
    let (width, height) = capture.surface();
    let encoder = EncoderConfig {
        width,
        height,
        codec: quality.codec,
        fps,
        bitrate_bps: quality.bitrate_bps.max(100_000),
        chroma: Chroma::Subsampled,
    };
    (capture, encoder)
}

/// `configs` for a stream carrying `chroma`: a 4:4:4 session is fed the one 4:4:4 format
/// ScreenCaptureKit delivers.
const fn carrying(
    chroma: Chroma,
    (capture, encoder): (CaptureConfig, EncoderConfig),
) -> (CaptureConfig, EncoderConfig) {
    let format = capture_format(chroma);
    (CaptureConfig { format, ..capture }, EncoderConfig { chroma, ..encoder })
}

/// The capture format a session of `chroma` takes.
const fn capture_format(chroma: Chroma) -> PixelFormat {
    match chroma {
        Chroma::Subsampled => PixelFormat::Nv12Full,
        Chroma::Full => PixelFormat::Yuv444Full10,
    }
}

/// The chroma a client may have at `quality`: 4:4:4 is an HEVC profile.
fn asked(quality: &Quality) -> Chroma {
    if quality.codec == VideoCodec::Hevc { quality.chroma } else { Chroma::Subsampled }
}

/// The stripes a stream capturing as `capture`, carrying `chroma`, is coded as: always under
/// the knob's `On`, never under `Off`, and otherwise when stripes pay at the stream's size
/// ([`stripes::pays`], which times the engines for a size not yet known only where `may_time`
/// says they are idle) and the spend says stripes (`spend`; `None` before the stream has spent
/// anything, when the size alone decides). `None` for one picture.
fn striping<P: Platform>(
    knob: stripes::Knob,
    spend: Option<bool>,
    capture: &CaptureConfig,
    chroma: Chroma,
    may_time: bool,
) -> Option<[CodedStripe; 2]> {
    let (width, height) = capture.surface();
    let wanted = match knob {
        stripes::Knob::On => true,
        stripes::Knob::Off => false,
        stripes::Knob::Auto => {
            spend.unwrap_or(true)
                && stripes::pays::<P::Video>(width, height, chroma, may_time) == Some(true)
        }
    };
    if wanted { slopty_codec::stripes::layout(height) } else { None }
}

/// The stripes of `layout` as the client is told them, for stream `id` of a picture `height`
/// rows high: the padding under the picture is neither coded nor shown as far as the client
/// knows, as the lower stripe's decoded picture ends at the picture's last row. Empty for one
/// picture.
fn on_the_wire(id: StreamId, layout: Option<[CodedStripe; 2]>, height: u32) -> Vec<Stripe> {
    let Some(layout) = layout else { return Vec::new() };
    layout
        .iter()
        .enumerate()
        .map(|(index, stripe)| Stripe {
            media: Stripe::media_of(id, index),
            coded_top: stripe.coded_top,
            coded_rows: stripe.coded_rows.min(height.saturating_sub(stripe.coded_top)),
            shown_top: stripe.shown_top,
            shown_rows: stripe.shown_rows.min(height.saturating_sub(stripe.shown_top)),
        })
        .collect()
}

/// How a stream is coded where a measurement or a test sets it ([`Pipeline::open_padded`]).
#[derive(Clone, Copy, Debug)]
struct Coding {
    /// The multiple the stream's sides are padded to in place of its codec's own
    /// ([`configs_padded`]); `None` for the codec's.
    pad_to: Option<u32>,
    /// Whether it is striped.
    stripes: stripes::Knob,
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
    /// The stripes the sessions in force code, top first; `None` for one picture.
    layout: Option<[CodedStripe; 2]>,
    /// Whether stripes are wanted: the knob, or the gate's word ([`stripes`]).
    stripes: stripes::Knob,
    /// The spend half of the stripe gate.
    gate: stripes::Gate,
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
    /// The stream's place among those its client's sound is for ([`Self::listen`]).
    listening: Option<sound::Listening>,
    /// Client input aimed at this stream, in its pixel coordinates.
    injector: P::Input,
    /// The tile sends its trackpad gestures ([`ScreenInput::Gestures`]): told again to the
    /// input sink a display switch makes, as the tile still shows it.
    gestures: bool,
    point_scale: f64,
    /// Last requested quality; re-applied when the target changes size.
    quality: Quality,
    /// The refresh rate of the target's display when the stream opened ([`configs_padded`]).
    refresh_hz: Option<f64>,
    /// The multiple the stream's sides are padded to in place of its codec's own; `None` but
    /// for a measurement's A/B ([`Self::open_padded`]).
    pad_to: Option<u32>,
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
    /// Where the cursor's pictures go; kept for the shape task a display switch restarts.
    on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
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
        // Asked without the grant, ScreenCaptureKit puts a consent prompt on the screen of
        // whoever sits at this Mac; the preflight asks nobody.
        if !Source::<P>::can_capture() {
            return Err(ScreenError::NotPermitted);
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
        let config = EncoderConfig {
            width: 64,
            height: 64,
            codec: VideoCodec::Hevc,
            fps: 1,
            bitrate_bps: 100_000,
            chroma: Chroma::Subsampled,
        };
        tokio::task::spawn_blocking(move || P::Video::new(config, |_packet| {}).map(drop))
            .await
            .map_err(|_cancelled| ScreenError::Closed)??;
        let content = Self::shareable().await?;
        let display =
            Source::<P>::displays(&content).into_iter().next().ok_or(ScreenError::Closed)?;
        let resolved = Source::<P>::resolve(&content, CaptureTarget::Display(display.id))?;
        let config = CaptureConfig {
            width: 64,
            height: 64,
            align: align(VideoCodec::Hevc),
            fps: 1,
            format: PixelFormat::Nv12Full,
            queue_depth: 1,
            crop: None,
            region: None,
        };
        let (tx, rx) = oneshot::channel();
        let capture = Source::<P>::start(
            &resolved,
            &config,
            |_frame| {},
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

    /// Whether ScreenCaptureKit lists `display`, from an enumeration taken now: one taken
    /// before a display was made does not have it.
    pub async fn lists_display(display: DisplayId) -> Result<bool, ScreenError> {
        *SHAREABLE.lock() = None;
        let content = Self::shareable().await?;
        Ok(Source::<P>::displays(&content).iter().any(|d| d.id == display))
    }

    /// The first display ScreenCaptureKit lists other than `except`: the one a stream shows
    /// when the display made for its client cannot be had.
    pub async fn physical_display(except: Option<DisplayId>) -> Result<DisplayId, ScreenError> {
        let content = Self::shareable().await?;
        Source::<P>::displays(&content)
            .into_iter()
            .map(|d| d.id)
            .find(|id| Some(*id) != except)
            .ok_or(ScreenError::NoDisplay)
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
        let coding = Coding { pad_to: None, stripes: stripes::Knob::from_env() };
        Self::open_padded(id, target, quality, sink, on_event, coding).await
    }

    /// [`Self::open`], coded as `coding` says.
    async fn open_padded(
        id: StreamId,
        target: CaptureTarget,
        quality: Quality,
        sink: Arc<dyn DatagramSink>,
        on_event: impl Fn(StreamEvent) + Send + Sync + 'static,
        coding: Coding,
    ) -> Result<(Self, ScreenEvent), ScreenError> {
        let Coding { pad_to, stripes: knob } = coding;
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
        let point_scale = f64::from(Source::<P>::point_scale(&resolved));
        let refresh_hz = Source::<P>::refresh_hz(target);
        let sized = configs_padded(native, &quality, refresh_hz, pad_to, point_scale);
        let shared = Arc::new(Shared::new(
            id,
            sink,
            sized.1.bitrate_bps,
            sized.1.fps,
            path == WindowPath::DisplayCrop,
        ));
        let contender = Arc::downgrade(&shared);
        engines::ENGINES.join(contender);
        let chroma = shared.ask_chroma(asked(&quality), &sized.1);
        let (mut capture_config, mut encoder_config) = carrying(chroma, sized);
        capture_config.crop = Source::<P>::crop(&resolved);
        *shared.region.lock() = region::RegionClock::steady(region_of(native, &quality));
        let whole = whole_size(native, &quality);
        let zoom = f64::from(whole.0) / f64::from(native.0);
        shared.set_zoom(zoom);
        let t_encoder = Instant::now();
        let sessions = [shared.next_session(), shared.next_session()];
        let shown = (capture_config.width, capture_config.height);
        let at_open = stripes::time_at_open();
        let mut layout = striping::<P>(knob, None, &capture_config, encoder_config.chroma, at_open);
        let weak = Arc::downgrade(&shared);
        let built = match build_encoder(&weak, encoder_config, shown, sessions, layout).await {
            Err(e) if chroma == Chroma::Full => {
                tracing::warn!(stream = %id, error = %e, "no 4:4:4 session here: 4:2:0");
                shared.chroma.lock().refuse();
                (capture_config, encoder_config) =
                    carrying(Chroma::Subsampled, (capture_config, encoder_config));
                layout = striping::<P>(knob, None, &capture_config, encoder_config.chroma, false);
                build_encoder(&weak, encoder_config, shown, sessions, layout).await?
            }
            Err(e) if layout.is_some() => {
                tracing::warn!(stream = %id, error = %e, "no stripes here: one picture");
                layout = None;
                build_encoder(&weak, encoder_config, shown, sessions, layout).await?
            }
            built => built?,
        };
        let encoder_built = t_encoder.elapsed();
        retire(shared.install(built, sessions));
        let start = shared.rate.lock().target_bps();
        shared.apply_bitrate(start);
        shared.apply_cadence(start);

        let (started_tx, started_rx) = oneshot::channel();
        start_encode_thread(&shared)?;
        let sink = Arc::clone(&shared);
        let capture = Source::<P>::start(
            &resolved,
            &capture_config,
            move |frame| sink.post(frame),
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

        let injector = P::Input::new(target, point_scale * zoom);
        // Two tasks, not one. The beat is a promise about time and must never be behind work
        // that takes any: the pointer read in the cursor loop is a window-server round trip, and
        // those have been measured at 90 ms, three beats' worth.
        let beat = tokio::spawn(beat_loop(Arc::clone(&shared)));
        let cursor =
            tokio::spawn(cursor_loop(Arc::clone(&shared), point_scale, injector.pointer()));
        let repair = tokio::spawn(repair_loop(Arc::clone(&shared)));
        let lane = tokio::spawn(lane_loop(Arc::clone(&shared)));
        let shape = tokio::spawn(shape_loop(Arc::clone(&shared), Arc::clone(&on_event)));
        let hide_watch = hide_watch_for::<P>(id, target, &shared).await;
        #[expect(clippy::cast_possible_truncation, reason = "a small ratio")]
        let scale = (point_scale * zoom) as f32;
        let opened = ScreenEvent::Opened {
            stream: id,
            target,
            codec: encoder_config.codec,
            width: whole.0,
            height: whole.1,
            scale,
            stripes: on_the_wire(id, layout, capture_config.height),
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
            layout,
            stripes: knob,
            gate: stripes::Gate::new(layout.is_some()),
            cursor,
            shape,
            beat,
            repair,
            lane,
            hide_watch,
            listening: None,
            injector,
            gestures: false,
            point_scale,
            quality,
            refresh_hz,
            pad_to,
            source: SourceTracker::new(Instant::now()),
            content,
            path,
            transitions: Transitions::default(),
            on_stop,
            on_event,
            stopped: false,
        };
        Ok((stream, opened))
    }

    /// Stream id.
    #[must_use]
    pub const fn id(&self) -> StreamId {
        self.id
    }

    /// Hear the target through the client's one sound, `sound`: every application for a
    /// display, its own for a window. The sound goes ahead of this stream's video while it
    /// flows. Blocking for a window: one window-server read of its owner.
    pub fn listen(&mut self, sound: &dyn sound::Listen) {
        let heard = match self.target {
            CaptureTarget::Display(_) => Heard::Every,
            CaptureTarget::Window(id) => {
                Heard::Apps(Source::<P>::window_owner(id).into_iter().collect())
            }
        };
        let _first = self.shared.sound.set(sound.clock());
        self.listening = Some(sound.listen(heard));
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

    /// The chroma of the encoder session in force.
    #[must_use]
    pub const fn chroma(&self) -> Chroma {
        self.encoder_config.chroma
    }

    /// Counters.
    #[must_use]
    pub fn stats(&self) -> ScreenStats {
        self.shared.stats()
    }

    /// What the client's feedback reaches without going through this stream's owner.
    #[must_use]
    pub fn control(&self) -> StreamControl {
        StreamControl { stream: Arc::<Shared<P>>::clone(&self.shared), coder: 0 }
    }

    /// A handle on the counters for the daemon's registry.
    #[must_use]
    pub fn stats_handle(&self) -> StatsHandle {
        StatsHandle(Arc::<Shared<P>>::clone(&self.shared))
    }

    /// Change quality. A bitrate alone is applied in place, cadence included. A size, rate or
    /// codec change starts building the encoder it needs and returns the build, for the caller
    /// to wait for beside its input and hand to [`Self::finish_rebuild`], as a resize's
    /// ([`Self::check_geometry`]). It replaces `pending`, a build already under way, and keeps
    /// the size that one was for.
    ///
    /// Input and the pointer are mapped at the new scale from here on, not once the encoder is
    /// in: the client maps its pointer at the scale it asked for from the moment it asks, so
    /// the input behind the change must neither wait for the build nor be read at the old
    /// scale.
    pub fn set_quality(
        &mut self,
        quality: &Quality,
        pending: Option<Rebuild<P>>,
    ) -> Option<Rebuild<P>> {
        self.quality = *quality;
        let mut pending = pending;
        let native = pending.as_ref().map_or(self.native, |rebuild| rebuild.native);
        let sized = configs_padded(native, quality, self.refresh_hz, self.pad_to, self.point_scale);
        let chroma = self.shared.ask_chroma(asked(quality), &sized.1);
        let (capture_config, encoder_config) = carrying(chroma, sized);
        let desired = CaptureConfig { crop: self.desired.crop, ..capture_config };
        let (was, was_config) =
            pending.as_ref().map_or((self.desired, self.encoder_config), |rebuild| {
                (rebuild.desired, rebuild.config)
            });
        // A region that moves at the size in force needs no session of its own: the capture
        // samples elsewhere, the encoder goes on predicting from what it coded, and each frame
        // says where it goes. Only a region of another size is a rebuild.
        let moved = CaptureConfig { region: desired.region, ..was };
        if moved == desired && was.region != desired.region {
            if let Some(rebuild) = pending.as_mut() {
                rebuild.desired.region = desired.region;
            } else {
                self.desired.region = desired.region;
                self.apply_desired();
            }
        }
        let was = moved;
        // The rate is the encoder's alone while the capture follows the display's beat.
        let same_rate = encoder_config.fps == was_config.fps;
        if desired == was
            && same_rate
            && encoder_config.codec == was_config.codec
            && encoder_config.chroma == was_config.chroma
        {
            if let Some(mut rebuild) = pending {
                // The session under way is the one wanted; it goes in under the new ceiling.
                rebuild.config.bitrate_bps = encoder_config.bitrate_bps;
                return Some(rebuild);
            }
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
            return None;
        }
        if pending.is_none()
            && desired == was
            && encoder_config.codec == was_config.codec
            && encoder_config.chroma == was_config.chroma
        {
            // Only the rate moved (the client's view went to a screen of another refresh), and
            // the capture already follows the display's beat: the session takes the new rate as
            // it takes a rung, with no rebuild and no keyframe.
            self.set_rate_in_place(encoder_config);
            return None;
        }
        let resized = pending.as_ref().is_some_and(|rebuild| rebuild.resized);
        // A build under way for an older ask is dropped: it drops its sessions once made.
        drop(pending);
        self.map_at(native);
        let mut rebuild = self.start_rebuild(native, desired, encoder_config);
        rebuild.resized = resized;
        Some(rebuild)
    }

    /// Take a new frame rate ceiling (and bitrate ceiling) on the encoder session in force.
    fn set_rate_in_place(&mut self, encoder_config: EncoderConfig) {
        let target = {
            let mut rate = self.shared.rate.lock();
            rate.set_max(encoder_config.bitrate_bps);
            rate.target_bps()
        };
        self.shared.apply_bitrate(target);
        // The session is told the new rate at its next frame ([`Shared::tell`]).
        self.shared.reset_rate(encoder_config.fps);
        self.shared.apply_cadence(target);
        self.encoder_config = encoder_config;
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
    ///
    /// A size change comes back as the [`Rebuild`] it started: the encoder is built off the
    /// runtime, and the caller waits for it without holding anything else up, then hands it to
    /// [`Self::finish_rebuild`]. No probe is to be checked meanwhile.
    pub fn check_geometry(&mut self, probe: &Probe) -> Option<Rebuild<P>> {
        if let Some(console) = probe.console {
            self.source.set_console(console);
        }
        self.injector.set_bounds(probe.bounds, probe.at);
        let Some(rect) = probe.bounds else {
            self.window_gone();
            return None;
        };
        self.settle();
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped")]
        let px = |points: f64| (points * self.point_scale).round().clamp(2.0, 16_384.0) as u32;
        let native = (px(rect.w), px(rect.h));
        if let (CaptureTarget::Window(_), Some((on_screen, crop))) = (self.target, probe.window) {
            self.follow_window(on_screen, crop);
        }
        let spend = self.follow_spend();
        let Some(native) = self.resize.observe(native, self.native, probe.at) else {
            if let Some(rebuild) = self.follow_lost() {
                return Some(rebuild);
            }
            if let Some(rebuild) = self.follow_chroma() {
                return Some(rebuild);
            }
            if let Some(rebuild) = self.follow_stripes(spend) {
                return Some(rebuild);
            }
            self.apply_desired();
            return None;
        };
        tracing::info!(stream = %self.id, from = ?self.native, to = ?native, "target resized");
        let sized =
            configs_padded(native, &self.quality, self.refresh_hz, self.pad_to, self.point_scale);
        let asked = self.shared.chroma.lock().asked();
        let chroma = self.shared.ask_chroma(asked, &sized.1);
        let (capture_config, encoder_config) = carrying(chroma, sized);
        let desired = CaptureConfig { crop: self.desired.crop, ..capture_config };
        let mut rebuild = self.start_rebuild(native, desired, encoder_config);
        rebuild.resized = true;
        Some(rebuild)
    }

    /// Put in the encoder `rebuild` built ([`Rebuild::built`]) and ask the capture for its size:
    /// for a target that changed size, the `Geometry` to tell the client. A quality change tells
    /// nothing; the client already maps at the size it asked for.
    pub fn finish_rebuild(&mut self, rebuild: Rebuild<P>, built: Built<P>) -> Option<ScreenEvent> {
        let told = rebuild.resized || rebuild.layout != self.layout;
        self.install_rebuild(rebuild, built);
        let (width, height) = whole_size(self.native, &self.quality);
        told.then(|| ScreenEvent::Geometry {
            stream: self.id,
            width,
            height,
            stripes: on_the_wire(self.id, self.layout, self.desired.height),
        })
    }

    /// What the encoder spent since the last probe, into the stripe gate: whether it says
    /// stripes now.
    fn follow_spend(&mut self) -> bool {
        let bytes = self.shared.counters.video_bytes.load(Ordering::Relaxed);
        let target = u64::from(self.shared.encoder_bps.load(Ordering::Relaxed));
        let was = self.gate.on();
        let on = self.gate.observe(Instant::now(), bytes, target);
        if on != was {
            tracing::debug!(stream = %self.id, on, spend = ?self.gate.spend(), "stripe gate");
        }
        on
    }

    /// Sessions for the stripes the gate now wants, at the size, quality and chroma in force,
    /// when that is not what the sessions in force code: new sessions, so keyframes, and a
    /// `Geometry` for the client once they are in.
    fn follow_stripes(&mut self, spend: bool) -> Option<Rebuild<P>> {
        let idle = engines::ENGINES.quiet(now::<P>());
        let config = &self.encoder_config;
        let layout = striping::<P>(self.stripes, Some(spend), &self.desired, config.chroma, idle);
        if layout == self.layout {
            return None;
        }
        tracing::info!(stream = %self.id, striped = layout.is_some(), spend = ?self.gate.spend(), "rebuilding for the stripes");
        Some(self.start_rebuild(self.native, self.desired, self.encoder_config))
    }

    /// Sessions in place of one the system took away ([`Shared::encoder_lost`]) or one given up
    /// on inside the encoder ([`Shared::unstick`]), at the size,
    /// quality and chroma in force: new sessions, so keyframes, and nothing for the client to
    /// hear but the keyframes. Only while the lost session is in force: the frames it refuses
    /// while its replacements build, or before they go in at the next encode, ask for nothing
    /// more.
    fn follow_lost(&mut self) -> Option<Rebuild<P>> {
        let lost = self.shared.encoder_lost.load(Ordering::Relaxed);
        let in_force = [&self.shared.top, &self.shared.lower]
            .iter()
            .any(|coder| coder.session.load(Ordering::Relaxed) == lost);
        // Sessions given up on inside the encoder leave none numbered in force
        // ([`Shared::unstick`]); before the first is put in, the open's are staged.
        let given_up = self.shared.top.session.load(Ordering::Relaxed) == 0;
        if !(lost != 0 && in_force || given_up) || self.shared.staged.lock().is_some() {
            return None;
        }
        // A session given up on inside the encoder already waited out its patience, which
        // doubles in a row the same way ([`StuckWatch::gave_up`]).
        if !given_up {
            let encoded = self.shared.counters.encoded.load(Ordering::Relaxed);
            let mut retry = self.shared.lost_retry.lock();
            if now::<P>() < retry.0.due_us(encoded) {
                return None;
            }
            retry.0.started(now::<P>(), encoded);
        }
        tracing::info!(stream = %self.id, given_up, "rebuilding for a lost encoder session");
        Some(self.start_rebuild(self.native, self.desired, self.encoder_config))
    }

    /// A session the rate's decisions moved to the other chroma ([`ChromaGate::update`]), at the
    /// size and quality in force: a new encoder session, so a keyframe, and the capture format
    /// with it.
    fn follow_chroma(&mut self) -> Option<Rebuild<P>> {
        let chroma = self.shared.chroma.lock().chroma();
        if chroma == self.encoder_config.chroma {
            return None;
        }
        tracing::info!(stream = %self.id, ?chroma, "rebuilding for the chroma");
        let (desired, config) = carrying(chroma, (self.desired, self.encoder_config));
        Some(self.start_rebuild(self.native, desired, config))
    }

    /// Start building the session for `config`, to capture as `desired` once it is in.
    ///
    /// A 4:4:4 session refuses anything but `xf44`, while a 4:2:0 one takes `xf44` as well
    /// (MEASUREMENTS.md, "4:4:4 HEVC on the low-latency encoder"), so a switch to 4:4:4 asks
    /// ScreenCaptureKit for `xf44` now, while the session builds, and a switch back keeps it
    /// until the 4:2:0 session is in.
    fn start_rebuild(
        &mut self,
        native: (u32, u32),
        desired: CaptureConfig,
        config: EncoderConfig,
    ) -> Rebuild<P> {
        if desired.format == PixelFormat::Yuv444Full10 && self.desired.format != desired.format {
            self.desired.format = desired.format;
            self.apply_desired();
        }
        let sessions = [self.shared.next_session(), self.shared.next_session()];
        let shown = (desired.width, desired.height);
        let spend = self.gate.spend().map(|_| self.gate.on());
        let layout = striping::<P>(self.stripes, spend, &desired, config.chroma, false);
        let encoder = start_encoder(&Arc::downgrade(&self.shared), config, shown, sessions, layout);
        Rebuild { encoder, sessions, native, desired, config, layout, resized: false }
    }

    /// The session `rebuild` was for could not be made: the stream goes on with the one it has.
    /// A 4:4:4 session is not asked for again until the client asks, and the capture goes back
    /// to the format the session in force takes.
    pub fn rebuild_failed(&mut self, rebuild: &Rebuild<P>) {
        if rebuild.config.chroma == Chroma::Full {
            self.shared.chroma.lock().refuse();
        }
        self.desired.format = capture_format(self.encoder_config.chroma);
        self.apply_desired();
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

    /// A read of the text field that has the keyboard on the worker, for the client
    /// ([`ScreenEvent::Field`]); blocking, for the blocking pool. A window stream reports only
    /// its window's application's field; the caret is in stream pixels, and only while it is
    /// over the target.
    pub fn field_reader(&self) -> impl FnOnce() -> Option<TextField> + Send + 'static {
        let (target, point_scale, shared) =
            (self.target, self.point_scale, Arc::clone(&self.shared));
        move || {
            let field = Source::<P>::focused_field()?;
            if let CaptureTarget::Window(id) = target
                && Source::<P>::window_owner(id) != Some(field.pid)
            {
                return None;
            }
            let bounds = *shared.bounds.lock();
            Some(stream_field(field, bounds, point_scale * shared.zoom()))
        }
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
        self.shared.source_idle.store(self.source.idle(), Ordering::Relaxed);
        Some(ScreenEvent::Source { stream: self.id, state })
    }

    /// Whether the target may go unprobed until [`Self::geometry_wake`] fires or a backstop
    /// passes, rather than at the follow period.
    ///
    /// Only a stream with nothing to follow is: the client was told its source is idle (a
    /// frame wakes the probe), the target is on screen (nothing announces a window's return
    /// from another Space), and nothing is under way (a transition to settle, a size to hold,
    /// a suspicion, a stalled filter, a chroma to switch to). A window served as a crop of its
    /// display never is: a window of another application moving over it announces nothing to
    /// this worker, and the crop would show that window until the probe saw it.
    #[must_use]
    pub fn geometry_quiet(&self) -> bool {
        let window_crop = matches!(self.target, CaptureTarget::Window(_))
            && (self.path == WindowPath::DisplayCrop
                || self.desired_path == WindowPath::DisplayCrop);
        self.source.idle()
            && !window_crop
            && !self.shared.target_hidden.load(Ordering::Relaxed)
            && !self.transitions.pending()
            && !self.resize.pending()
            && self.desired_path == self.path
            && self.desired == self.capture_config
            && !self.shared.suspected_at(now::<P>())
            && !self.shared.filter_stalled.load(Ordering::Relaxed)
            && self.shared.chroma.lock().chroma() == self.encoder_config.chroma
    }

    /// What wakes a quiet stream's probe ([`Self::geometry_quiet`]): the accessibility API
    /// saying the target moved, was resized or went, and the first frame after the client was
    /// told the source is idle. One permit is kept for a wake while no one waits.
    #[must_use]
    pub fn geometry_wake(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.shared.geometry_wake)
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
        self.shared.set_hidden(true);
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
        if self.shared.set_hidden(!on_screen) == on_screen {
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
    /// is still in flight; the next tick asks again for whatever is still different. A
    /// transition that has completed is settled first, so the configuration asked is weighed
    /// against the one in force, not the one the last tick saw: a region asked and then given
    /// back between two ticks is otherwise taken for the one in force, and never asked.
    ///
    /// Order matters between the two calls of a path change: the crop is cleared before the swap
    /// to the window filter, since a `sourceRect` on a window stream would be read in the
    /// window's own space, and the display filter is in place before the crop is set on it.
    fn apply_desired(&mut self) {
        if self.stopped {
            return;
        }
        self.settle();
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

    /// Push `config` to the live stream; the completion lands in the transition, and in the
    /// clock that says which region each capture shows.
    fn update_capture(&self, config: CaptureConfig) {
        let id = self.id;
        let transitions = self.transitions.clone();
        let region = config.region.map(|points| region::to_pixels(points, self.point_scale));
        self.shared.region.lock().asked(region, now::<P>());
        let shared = Arc::downgrade(&self.shared);
        Source::<P>::update(&self.capture, &config, move |result| {
            if let Err(e) = &result {
                tracing::warn!(stream = %id, error = %e, "capture update failed");
            }
            transitions.on_result(result.is_ok());
            if let Some(shared) = shared.upgrade() {
                shared.region.lock().answered(result.is_ok(), now::<P>());
            }
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

    fn install_rebuild(&mut self, rebuild: Rebuild<P>, built: Built<P>) {
        let Rebuild {
            encoder: _answered,
            sessions,
            native,
            desired,
            config: encoder_config,
            layout,
            ..
        } = rebuild;
        self.native = native;
        self.layout = layout;
        retire(self.shared.install(built, sessions));
        let target = {
            let mut rate = self.shared.rate.lock();
            rate.set_max(encoder_config.bitrate_bps);
            rate.target_bps()
        };
        self.shared.apply_bitrate(target);
        // A new quality sets a new ceiling, and the ladder starts from it again: the rung that was
        // in force answered a bitrate the client has just replaced.
        self.shared.reset_rate(encoder_config.fps);
        self.shared.apply_cadence(target);
        self.map_at(native);
        self.desired = desired;
        self.encoder_config = encoder_config;
        self.apply_desired();
    }

    /// Map input and the pointer for the whole of a target `native` pixels in size at the
    /// quality in force, whatever region of it is streamed.
    fn map_at(&mut self, native: (u32, u32)) {
        let zoom = f64::from(whole_size(native, &self.quality).0) / f64::from(native.0);
        self.shared.set_zoom(zoom);
        self.injector.set_scale(self.point_scale * zoom);
    }

    /// Stream pixels per native pixel of the target: the quality's scale as input and the
    /// pointer are mapped, the one last asked for even while its encoder is being built. A client
    /// position in stream pixels over `zoom × point_scale` is a position in the target's points.
    #[must_use]
    pub fn zoom(&self) -> f64 {
        self.shared.zoom()
    }

    /// Deliver client input to the streamed window or display.
    pub fn inject(&mut self, input: &ScreenInput) -> Result<(), ScreenError> {
        if let ScreenInput::Gestures { remote } = input {
            self.gestures = *remote;
        }
        Ok(self.injector.inject(input)?)
    }

    /// Take a step of a drag from the client through the injector's drag mode, in order with
    /// the stream's other input (`drag`).
    pub fn drag(&mut self, step: slopty_input::DragStep) {
        self.injector.drag(step);
    }

    /// Give the streamed window's application keyboard focus on the worker.
    pub fn focus(&mut self) -> Result<(), ScreenError> {
        Ok(self.injector.focus()?)
    }

    /// Whether a client has this stream's tile focused. A focused stream's encoder falling
    /// behind steps the streams nobody focuses down before it steps down itself (`engines`).
    pub fn set_focused(&self, focused: bool) {
        let was = self.shared.focused.swap(focused, Ordering::Relaxed);
        if was && !focused {
            engines::ENGINES.unfocused();
        }
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
        self.shared.report(0, report, path)
    }

    /// The bitrate the controller is asking the encoder for right now.
    #[must_use]
    pub fn bitrate_bps(&self) -> u32 {
        self.shared.rate.lock().target_bps()
    }

    /// The client lost a frame it cannot recover: make the next frame stand on its own, and a
    /// keyframe when `keyframe` says the client holds no reference to predict from.
    pub fn request_refresh(&self, last_good_frame: u32, keyframe: bool) {
        self.shared.request_refresh(0, last_good_frame, keyframe);
    }

    /// Retransmit fragments of a recent frame, unless QUIC is already holding more than the
    /// frame budget: an answer that leaves behind seconds of queued frames arrives after the
    /// receiver has given up, and on a collapsed link every NACK answered that way stacked
    /// another copy of the frame into the queue (64 000 datagrams for 12 frames,
    /// MEASUREMENTS.md "start-up over the mesh").
    pub fn nack(&self, frame: u32, fragments: &[u16]) {
        self.shared.nack(0, frame, fragments);
    }

    /// Answer the client's clock probe stamped `sent_us` with a [`Kind::Clock`] datagram, at
    /// once and ahead of anything queued: the probe's stamp back, beside this stream's capture
    /// clock as the probe `arrived` (read off the connection, as the client stamps the echo) and
    /// as the echo leaves. The time the probe waited for this call is then no part of the round
    /// trip the client measures. The client places the capture
    /// timestamps on its own clock from these (`docs/decisions/video.md`, "Capture to glass on
    /// any link").
    ///
    /// [`Kind::Clock`]: slopty_proto::media::Kind::Clock
    pub fn echo_clock(&self, sent_us: u64, arrived: Instant) {
        self.shared.echo_clock(sent_us, arrived);
    }

    /// Stream `display` from now on, in place of the display this stream shows: the display
    /// made for its client was made anew or changed backing scale. Input and the pointer map
    /// through the new display's bounds and scale, and the geometry poll then takes its size
    /// (a rebuild and a `Geometry` event). Nothing happens when it is the display already shown
    /// at the scale it already has.
    ///
    /// # Errors
    ///
    /// [`ScreenError::NotDisplay`] for a window stream, else why ScreenCaptureKit would not
    /// switch; the stream then goes on showing what it showed.
    pub async fn switch_display(&mut self, display: DisplayId) -> Result<(), ScreenError> {
        if !matches!(self.target, CaptureTarget::Display(_)) {
            return Err(ScreenError::NotDisplay);
        }
        *SHAREABLE.lock() = None;
        let content = Self::shareable().await?;
        let target = CaptureTarget::Display(display);
        let resolved = Source::<P>::resolve(&content, target)?;
        let point_scale = f64::from(Source::<P>::point_scale(&resolved));
        if target == self.target && (point_scale - self.point_scale).abs() < f64::EPSILON {
            return Ok(());
        }
        let (tx, rx) = oneshot::channel();
        Source::<P>::retarget(&self.capture, &resolved, move |result| {
            let _receiver_gone = tx.send(result);
        });
        rx.await.map_err(|_dropped| ScreenError::Closed)??;
        tracing::info!(stream = %self.id, from = ?self.target, to = ?target, point_scale, "display switched");
        self.injector.release_all();
        self.target = target;
        self.content = content;
        self.point_scale = point_scale;
        self.refresh_hz = Source::<P>::refresh_hz(target);
        self.injector = P::Input::new(target, point_scale * self.zoom());
        if self.gestures
            && let Err(e) = self.injector.inject(&ScreenInput::Gestures { remote: true })
        {
            tracing::warn!(stream = %self.id, error = %e, "gestures after a display switch");
        }
        self.cursor.abort();
        self.cursor = tokio::spawn(cursor_loop(
            Arc::clone(&self.shared),
            point_scale,
            self.injector.pointer(),
        ));
        self.shape.abort();
        self.shape = tokio::spawn(shape_loop(Arc::clone(&self.shared), Arc::clone(&self.on_event)));
        Ok(())
    }

    /// Stop capturing and tear down.
    pub async fn close(mut self) {
        // First: the streams holding a lower rung for this one, or waiting their turn behind
        // it, are let go now rather than when their hold runs out.
        self.set_focused(false);
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
        let cursor_wakes = self.shared.cursor_wakes.load(Ordering::Relaxed);
        tracing::info!(stream = %self.id, ?stats, cursor_wakes, "screen stream closed");
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
    #[cfg(target_os = "macos")]
    if synthetic::serving() {
        return Pipeline::<synthetic::Synthetic>::resize_window(window, width, height);
    }
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
                shared.heard(went);
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
    // A silence of four times the promise, twice what the receiver already calls a stall: a
    // beat that ends one has not merely slipped, it has failed at the one thing it is for.
    let late_beat_us = heartbeat_after_us.saturating_mul(4);
    while !shared.sink.is_closed() {
        let now = now::<P>();
        shared.unstick(now);
        shared.retry_lost(now);
        // The beat itself counts as traffic even when the transport refused it, so a refusal is
        // retried a period later instead of spun on.
        let silence = silence_us(now, shared.last_push_us.load(Ordering::Relaxed), last_beat_us);
        if let Some(wait) = beat_due_in(silence, heartbeat_after_us) {
            tokio::time::sleep(wait).await;
            continue;
        }
        beats = beats.wrapping_add(1);
        tracing::trace!(stream = %shared.id, beats, "heartbeat");
        shared.counters.heartbeats.fetch_add(1, Ordering::Relaxed);
        // The silence this beat ends, from whatever left last. The time since the previous beat
        // would count the video that flowed between them and, on a moving picture, read as a
        // late beat when none was due. The first beat's is the stream's opening, which no
        // receiver waits through.
        if last_beat_us.is_some() {
            shared.counters.beat_gap.lock().push(silence);
            shared.counters.beat_gap_worst_us.fetch_max(silence, Ordering::Relaxed);
            if silence >= late_beat_us {
                tracing::info!(stream = %shared.id, silence_us = silence, beats, "late heartbeat");
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
        // Off the runtime: VideoToolbox may encode inside the submit ([`start_encode_thread`]).
        let repairing = Arc::clone(&shared);
        // Made before the repair starts, so a give-up while it is inside the encoder wakes it.
        let unstuck = shared.unstuck.notified();
        let attempt = tokio::select! {
            repaired = tokio::task::spawn_blocking(move || repairing.repair_now(now)) => {
                let Ok(attempt) = repaired else { break };
                attempt
            }
            // A repair given up on inside the encoder is left to its thread ([`Shared::unstick`]).
            () = unstuck => continue,
        };
        let wait = match attempt {
            Attempt::Sent | Attempt::Nothing => continue,
            // A keyframe put off for a refresh waits for the cadence like any frame.
            Attempt::NotDue => shared.due_at().saturating_sub(now).max(1_000),
            // The link or the encoder is not taking it; ask again a period on, not on a spin.
            Attempt::NoRoom | Attempt::Failed | Attempt::GivenUp => period,
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

/// The silence at `now`: since whatever left last, video (`last_push_us`) or a beat.
const fn silence_us(now: u64, last_push_us: u64, last_beat_us: Option<u64>) -> u64 {
    let last_beat = match last_beat_us {
        Some(at) => at,
        None => 0,
    };
    let last_out = if last_push_us > last_beat { last_push_us } else { last_beat };
    now.saturating_sub(last_out)
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
/// (`input`), and hidden before the first event. Nothing else moves it across the picture but
/// the target's bounds and the stream's zoom, so the loop sleeps until one of them changes
/// (`input`'s changes, [`Shared::cursor_wake`]), as it does while the target is hidden or not
/// yet probed.
///
/// A display stream's input moves the real pointer, and so does the worker's own user, which
/// nothing announces: that pointer is read every [`CURSOR_PERIOD`]. A still one costs one read
/// of the event system's move counters a tick ([`CaptureSource::pointer_moves`], tens of
/// nanoseconds): the pointer itself is only asked for when the counters moved, and the
/// target's bounds are the ones the owner's geometry probe last read. The pointer read is a
/// window-server round trip, so it runs on the blocking pool: what this loop must not do is
/// occupy a runtime worker, because [`beat_loop`] needs one on time (MEASUREMENTS.md, "the beat
/// behind the geometry call").
async fn cursor_loop<P: Platform>(shared: Arc<Shared<P>>, point_scale: f64, input: PointerWatch) {
    let mut ticks = tokio::time::interval(CURSOR_PERIOD);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut placed = input.changes();
    let mut pointer: Option<(u32, (f64, f64))> = None;
    let mut last: Option<(i32, i32, bool)> = None;
    let mut seq: u32 = 0;
    while !shared.sink.is_closed() {
        shared.cursor_wakes.fetch_add(1, Ordering::Relaxed);
        let shown =
            if shared.target_hidden.load(Ordering::Relaxed) { None } else { *shared.bounds.lock() };
        let Some(rect) = shown else {
            shared.pointer_over.store(false, Ordering::Relaxed);
            cursor_event(&mut placed, &shared.cursor_wake).await;
            continue;
        };
        // Read every round: a quality change rescales the stream under a still pointer.
        let pixels_per_point = point_scale * shared.zoom();
        let placed_at = placed_sample(&input, rect, pixels_per_point);
        let sample = if let Some(placed) = placed_at {
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
        if sample.2 && !shared.pointer_over.swap(true, Ordering::Relaxed) {
            shared.shape_wake.notify_one();
        } else if !sample.2 {
            shared.pointer_over.store(false, Ordering::Relaxed);
        }
        if last != Some(sample) {
            last = Some(sample);
            seq = seq.wrapping_add(1);
            let datagram = cursor_datagram(
                shared.id,
                seq,
                send_ms_lo(now::<P>()),
                sample.0,
                sample.1,
                sample.2,
            );
            shared.send(&[datagram]);
        }
        if placed_at.is_some() {
            cursor_event(&mut placed, &shared.cursor_wake).await;
        } else {
            ticks.tick().await;
        }
    }
}

/// Until the placed pointer moves, the target's bounds, its visibility or the zoom change
/// (`wake`), or [`CURSOR_BACKSTOP`] passes.
async fn cursor_event(placed: &mut PointerChanges, wake: &tokio::sync::Notify) {
    tokio::select! {
        () = placed.changed() => {}
        () = wake.notified() => {}
        () = tokio::time::sleep(CURSOR_BACKSTOP) => {}
    }
}

/// `field` as the client is told it: its caret in stream pixels at `pixels_per_point` from the
/// target's `bounds`, while the caret is over the target.
#[expect(clippy::cast_possible_truncation, reason = "stream pixels are well within f32")]
fn stream_field(field: FocusedField, bounds: Option<Rect>, pixels_per_point: f64) -> TextField {
    let caret = field.caret.zip(bounds).and_then(|(caret, rect)| {
        rect.contains(caret.x, caret.y).then_some(Caret {
            x: ((caret.x - rect.x) * pixels_per_point) as f32,
            y: ((caret.y - rect.y) * pixels_per_point) as f32,
            width: (caret.w * pixels_per_point) as f32,
            height: (caret.h * pixels_per_point) as f32,
        })
    });
    TextField { caret, secure: field.secure }
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

/// Follow the cursor's picture while the pointer is over the target, and report each change
/// through `on_event`. Every [`SHAPE_PERIOD`] it reads the window server's cursor seed, which
/// costs no round trip, and reads the picture only when the seed moved since the last read.
/// While the pointer is elsewhere it sleeps until the cursor loop sees it come over the target
/// (`shape_wake`), or [`CURSOR_BACKSTOP`] passes. Its own task: a picture read is a
/// window-server round trip, run on the blocking pool, and must never hold the position loop.
async fn shape_loop<P: Platform>(
    shared: Arc<Shared<P>>,
    on_event: Arc<dyn Fn(StreamEvent) + Send + Sync>,
) {
    follow_shape(&shared, &*on_event, Source::<P>::cursor_seed, Source::<P>::cursor_shape).await;
}

/// [`shape_loop`] with the seed and picture reads it makes: the capture source's, or a test's.
async fn follow_shape<P: Platform>(
    shared: &Shared<P>,
    on_event: &(dyn Fn(StreamEvent) + Send + Sync),
    cursor_seed: fn() -> Option<i32>,
    cursor_shape: fn() -> Option<CursorShape>,
) {
    let mut ticks = tokio::time::interval(SHAPE_PERIOD);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut sent = ShapeDedup::default();
    let mut read_at: Option<i32> = None;
    while !shared.sink.is_closed() {
        if !shared.pointer_over.load(Ordering::Relaxed) {
            tokio::select! {
                () = shared.shape_wake.notified() => {}
                () = tokio::time::sleep(CURSOR_BACKSTOP) => {}
            }
            continue;
        }
        ticks.tick().await;
        let seed = cursor_seed();
        if seed.is_some() && seed == read_at {
            continue;
        }
        // Taken before the picture: a change between the two is read again next tick.
        read_at = seed;
        let Ok(read) = tokio::task::spawn_blocking(cursor_shape).await else {
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

// These drive the macOS platform's types (ScreenCaptureKit, VideoToolbox, `CVPixelBuffer`); a
// build for another system streams no desktop.
#[cfg(test)]
#[cfg(target_vendor = "apple")]
mod tests {
    use slopty_core::DisplayId;

    use super::*;

    const CROP: Crop = Crop { x: 10.0, y: 20.0, w: 300.0, h: 200.0 };

    /// The focused field's caret goes to the client in stream pixels from the target's corner,
    /// at the stream's scale, and only while it is over the target; a password field says so
    /// with or without one.
    #[test]
    fn a_fields_caret_is_told_in_stream_pixels_over_the_target() {
        let bounds = Some(Rect { x: 100.0, y: 50.0, w: 800.0, h: 600.0 });
        let caret = Rect { x: 300.0, y: 150.0, w: 1.0, h: 17.0 };
        let field = |caret, secure| FocusedField { pid: 7, caret, secure };
        assert_eq!(
            stream_field(field(Some(caret), false), bounds, 2.0),
            TextField {
                caret: Some(Caret { x: 400.0, y: 200.0, width: 2.0, height: 34.0 }),
                secure: false,
            }
        );
        let off = Rect { x: 20.0, ..caret };
        assert_eq!(
            stream_field(field(Some(off), true), bounds, 2.0),
            TextField { caret: None, secure: true },
            "off the target"
        );
        assert_eq!(
            stream_field(field(Some(caret), false), None, 2.0),
            TextField { caret: None, secure: false },
            "no bounds yet"
        );
    }

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
        shared.top.pending.lock().keyframe = false;
        shared.last_push_us.store(0, Ordering::Relaxed);
        let shared = Arc::new(shared);
        (shared, wire)
    }

    /// A clock probe is answered at once with one datagram: the probe's stamp back, and the
    /// stream's capture clock as it came and as the echo left, in that order.
    #[test]
    fn a_clock_probe_is_echoed_with_the_capture_clock() {
        use slopty_proto::media::{Kind, MediaHeader};
        let (shared, wire) = shared_for_frames();
        // The probe came off the connection 3 ms before the stream got to it. Read after
        // `before`, and both clocks read in whole microseconds: a microsecond either way.
        let before = now::<Native>().saturating_sub(3_001);
        let arrived = Instant::now().checked_sub(Duration::from_millis(3)).expect("a past");
        shared.echo_clock(0x0123_4567_89ab, arrived);
        let after = now::<Native>();
        let sent = wire.drain();
        assert_eq!(sent.len(), 1, "one datagram");
        let (header, payload) = MediaHeader::parse(&sent[0]).expect("a media datagram");
        assert_eq!((header.kind(), header.stream.get()), (Some(Kind::Clock), 1));
        let echo = ClockEcho::parse(payload).expect("an echo");
        assert_eq!(
            header.send_ms_lo,
            send_ms_lo(echo.echoed.get()),
            "stamped as every datagram is"
        );
        assert_eq!(echo.sent.get(), 0x0123_4567_89ab);
        let (received, echoed) = (echo.received.get(), echo.echoed.get());
        assert!(before <= received && received <= echoed && echoed <= after, "{echo:?}");
        assert!(echoed - received >= 3_000, "stamped as it arrived, not as it was answered");
    }

    #[test]
    fn a_ring_keeps_the_window() {
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
        let q = Quality { fps: 500, bitrate_bps: 1, scale: 0.3333, ..Quality::default() };
        let (capture, encoder) = configs((1_001, 777), &q, None);
        assert_eq!((capture.width, capture.height), (334, 260), "scaled, rounded up to even");
        assert_eq!(capture.fps, 240, "fps clamped");
        assert_eq!(capture.format, PixelFormat::Nv12Full, "full range, what the client samples");
        assert_eq!(encoder.bitrate_bps, 100_000, "bitrate floor");
        assert_eq!((encoder.width, encoder.height, encoder.fps), (336, 272, 240), "padded to 16");

        let nan = Quality { scale: f32::NAN, codec: VideoCodec::H264, ..q };
        let (capture, encoder) = configs((100, 100), &nan, None);
        assert_eq!((capture.width, capture.height), (100, 100), "a NaN scale is native");
        assert_eq!(encoder.codec, VideoCodec::H264, "the codec asked for");
        assert_eq!((encoder.width, encoder.height), (100, 100), "H.264 is not padded");

        let tiny = Quality { scale: 0.0001, ..q };
        let (capture, _encoder) = configs((10, 10), &tiny, None);
        assert_eq!((capture.width, capture.height), (2, 2), "never below two pixels");
    }

    /// A region is captured at the quality's scale, sampled in the target's points, while the
    /// stream's own size stays the whole target's. A window that shrank under its region holds
    /// the region to what is left, and one that shrank past it streams the whole window.
    #[test]
    fn a_region_is_captured_at_the_scale_and_held_to_the_target() {
        let region = Region { x: 1000, y: 600, w: 1756, h: 988 };
        let native = Quality { region: Some(region), ..Quality::default() };
        let (capture, encoder) = configs((5120, 2880), &native, None);
        assert_eq!((capture.width, capture.height), (1756, 988), "the region at native");
        assert_eq!((encoder.width, encoder.height), (1760, 992), "padded to 16");
        let points = capture.region.expect("sampled in points");
        assert_eq!((points.x, points.y, points.w, points.h), (500.0, 300.0, 878.0, 494.0));
        assert_eq!(whole_size((5120, 2880), &native), (5120, 2880), "the stream's own size");

        let half = Quality { scale: 0.5, ..native };
        let (capture, _encoder) = configs((5120, 2880), &half, None);
        assert_eq!((capture.width, capture.height), (878, 494), "and at half");
        assert_eq!(whole_size((5120, 2880), &half), (2560, 1440));

        // The window shrank to 2000 × 1200: what is left of the region from its corner.
        let (capture, _encoder) = configs((2000, 1200), &native, None);
        assert_eq!((capture.width, capture.height), (1000, 600));
        let points = capture.region.expect("held");
        assert_eq!((points.x, points.y, points.w, points.h), (500.0, 300.0, 500.0, 300.0));
        // To 900 × 500: nothing of the region is on it, so all of the window.
        let (capture, _encoder) = configs((900, 500), &native, None);
        assert_eq!((capture.width, capture.height, capture.region), (900, 500, None));
    }

    /// An HEVC stream's encoder codes the capture's surface, each side the picture's rounded up
    /// to 16, and the picture keeps today's even size: the client is told that size, input maps
    /// through it, and the stream's SPS crops the surface back to it.
    #[test]
    fn hevc_codes_a_surface_padded_to_16_around_the_even_picture() {
        let at = |scale| Quality { scale, ..Quality::default() };
        for (native, scale, picture, coded) in [
            ((3024, 1964), 1.0, (3024, 1964), (3024, 1968)),
            ((3456, 2234), 1.0, (3456, 2234), (3456, 2240)),
            ((2880, 1800), 1.0, (2880, 1800), (2880, 1808)),
            ((1920, 1080), 1.0, (1920, 1080), (1920, 1088)),
            ((2560, 1440), 1.0, (2560, 1440), (2560, 1440)),
            ((3024, 1964), 0.5, (1512, 982), (1520, 992)),
            ((1001, 777), 1.0, (1002, 778), (1008, 784)),
        ] {
            for chroma in [Chroma::Subsampled, Chroma::Full] {
                let quality = Quality { chroma, ..at(scale) };
                let (capture, encoder) = carrying(chroma, configs(native, &quality, Some(60.0)));
                assert_eq!((capture.width, capture.height), picture, "{native:?} at {scale}");
                assert_eq!(capture.surface(), coded, "{native:?} at {scale}");
                assert_eq!((encoder.width, encoder.height), coded, "{native:?} at {scale}");
                assert!(coded.0 % 16 == 0 && coded.1 % 16 == 0, "{coded:?}");
                assert!(picture.0 % 2 == 0 && picture.1 % 2 == 0, "{picture:?}");
            }
        }
    }

    /// A 4:4:4 stream captures `xf44` into a 4:4:4 session; 4:4:4 is only ever HEVC.
    #[test]
    fn full_chroma_captures_xf44_for_a_444_session() {
        let full = Quality { chroma: Chroma::Full, ..Quality::default() };
        let (capture, encoder) = carrying(Chroma::Full, configs((1_920, 1_080), &full, None));
        assert_eq!(capture.format, PixelFormat::Yuv444Full10);
        assert_eq!(encoder.chroma, Chroma::Full);
        let (capture, encoder) = carrying(Chroma::Subsampled, (capture, encoder));
        assert_eq!((capture.format, encoder.chroma), (PixelFormat::Nv12Full, Chroma::Subsampled));
        assert_eq!(asked(&full), Chroma::Full);
        assert_eq!(asked(&Quality { codec: VideoCodec::H264, ..full }), Chroma::Subsampled);
        assert_eq!(asked(&Quality::default()), Chroma::Subsampled, "4:2:0 unless asked");
    }

    /// Known, the display's refresh bounds the rate and the capture follows its beat.
    #[test]
    fn configs_follow_the_displays_refresh() {
        let q = |fps| Quality { fps, ..Quality::default() };
        let (capture, encoder) = configs((1_920, 1_080), &q(60), Some(75.0));
        assert_eq!((capture.fps, encoder.fps), (0, 60), "the display's beat, the rung asked for");
        let (capture, encoder) = configs((1_920, 1_080), &q(120), Some(75.0));
        assert_eq!((capture.fps, encoder.fps), (0, 75), "no faster than the display draws");
        let (_capture, encoder) = configs((1_920, 1_080), &q(60), Some(59.94));
        assert_eq!(encoder.fps, 60);
        let (capture, encoder) = configs((1_920, 1_080), &q(60), Some(f64::NAN));
        assert_eq!((capture.fps, encoder.fps), (60, 60), "an unreadable refresh keeps the ask");
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
        registry.insert(&"alice", CaptureTarget::Display(DisplayId(1)), handle(1));
        registry.insert(&"bob", CaptureTarget::Display(DisplayId(2)), handle(2));
        let (live, closed) = registry.summaries();
        assert_eq!(
            live.iter().map(|s| (s.client.as_str(), s.stream)).collect::<Vec<_>>(),
            [("alice", 1), ("bob", 2)]
        );
        assert_eq!(closed, []);
        registry.remove(&"alice", StreamId(9));
        assert_eq!(registry.summaries().0.len(), 2, "an unknown stream is not removed");
        registry.remove(&"alice", StreamId(1));
        let (live, closed) = registry.summaries();
        assert_eq!(live.len(), 1);
        assert_eq!(closed.iter().map(|s| s.stream).collect::<Vec<_>>(), [1]);
        assert_eq!(*counts.lock(), [1, 2, 1], "the observer hears every change, in order");
        for id in 10..u32::try_from(CLOSED_KEEP).unwrap_or(u32::MAX).saturating_add(12) {
            registry.insert(&"carol", CaptureTarget::Display(DisplayId(id)), handle(id));
            registry.remove(&"carol", StreamId(id));
        }
        let (_live, closed) = registry.summaries();
        assert_eq!(closed.len(), CLOSED_KEEP, "only the last few closed are kept");
        assert_eq!(closed.first().map(|s| s.stream), Some(12), "oldest first");
    }

    /// One 16x16 frame. The contents do not matter: with no encoder nothing reads them, and
    /// what is being tested is whether `on_frame` gets that far at all.
    fn a_frame() -> CapturedFrame {
        a_frame_of(16, 16)
    }

    /// A blank `width` × `height` capture.
    /// `IOSurface`-backed and full range, as ScreenCaptureKit's are: VideoToolbox copies any
    /// other buffer and codes the copy off the submit.
    fn a_frame_of(width: usize, height: usize) -> CapturedFrame {
        use std::ptr::{self, NonNull};

        use objc2_core_foundation::{CFDictionary, CFString, CFType};

        // SAFETY: framework-provided constant string.
        let key = unsafe { objc2_core_video::kCVPixelBufferIOSurfacePropertiesKey };
        let none = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
        let attributes = CFDictionary::<CFString, CFType>::from_slices(&[key], &[&*none]);
        let mut raw: *mut objc2_core_video::CVPixelBuffer = ptr::null_mut();
        // SAFETY: CoreVideo rule for `CVPixelBufferCreate`: a valid out-pointer, and an
        // attributes dictionary of `kCVPixelBuffer*` keys.
        let status = unsafe {
            objc2_core_video::CVPixelBufferCreate(
                None,
                width,
                height,
                objc2_core_video::kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
                Some(attributes.as_opaque()),
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

    /// A beat is held to the silence it ends, not to the time since the beat before: a third of
    /// a second of video between two beats is no late beat.
    #[test]
    fn video_between_two_beats_is_not_a_late_beat() {
        let (beat, video_until) = (10_000, 350_000);
        assert_eq!(silence_us(370_000, video_until, Some(beat)), 20_000, "since the video");
        assert_eq!(silence_us(40_000, 0, Some(beat)), 30_000, "since the beat, with no video");
        assert_eq!(silence_us(5_000, 0, None), 5_000, "since the stream opened");
        assert_eq!(silence_us(1_000, 2_000, Some(beat)), 0, "a clock that steps back");
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

    /// An encoder that keeps returning frames three periods late at 120 fps takes the ceiling
    /// and the cadence to 60, where the same latency is on time, and a new ceiling from the
    /// client starts the watch again. One slow frame changes nothing.
    #[test]
    fn an_encoder_behind_its_rung_takes_the_ceiling_down() {
        let (shared, _wire) = shared_for_frames();
        shared.fps_ceiling.store(120, Ordering::Relaxed);
        shared.apply_cadence(30_000_000);
        assert_eq!(shared.fps.load(Ordering::Relaxed), 120);
        shared.watch_encoder(97_000, false);
        shared.watch_encoder(8_000, false);
        assert_eq!(shared.fps.load(Ordering::Relaxed), 120, "a lone slow frame");
        for _ in 0..12 {
            shared.watch_encoder(80_000, false);
        }
        assert_eq!(shared.fps_ceiling.load(Ordering::Relaxed), 60);
        assert_eq!(shared.fps.load(Ordering::Relaxed), 60);
        for _ in 0..12 {
            shared.watch_encoder(25_000, false);
        }
        assert_eq!(shared.fps.load(Ordering::Relaxed), 60, "25 ms is on time at 60");
        shared.apply_cadence(30_000_000);
        assert_eq!(shared.fps.load(Ordering::Relaxed), 60, "the rate does not climb past it");
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
        shared.top.pending.lock().keyframe = true;
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

    /// A locked Mac, or a session off the screens, is said at once and outranks what the
    /// target draws, grace or not; once the session is back, the frames decide again.
    #[test]
    fn a_locked_mac_or_a_session_away_outranks_the_frames() {
        let base = Instant::now();
        let mut t = SourceTracker::new(base);
        t.set_console(Console::Locked);
        assert_eq!(t.poll(0, false, at(base, 10)), Some(SourceState::Locked), "no grace for it");
        assert_eq!(t.poll(5, false, at(base, 20)), None, "the lock screen may draw; still locked");
        t.set_console(Console::Away);
        assert_eq!(t.poll(6, true, at(base, 30)), Some(SourceState::Away));
        t.set_console(Console::Shown);
        assert_eq!(t.poll(7, false, at(base, 40)), Some(SourceState::Live), "drawing again");
        t.set_console(Console::Locked);
        assert_eq!(t.poll(7, false, at(base, 50)), Some(SourceState::Locked));
        t.set_console(Console::Shown);
        assert_eq!(t.poll(7, false, at(base, 3_000)), Some(SourceState::Idle), "back, quiet");
        assert!(t.idle());
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
        let quality = Quality { fps: 60, bitrate_bps: 8_000_000, scale: 1.0, ..Quality::default() };
        CaptureConfig { crop, ..configs((600, 400), &quality, None).0 }
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

    /// A live drag changes the window's size on every probe; the stream is rebuilt only for a
    /// size that holds for [`RESIZE_HOLD`], so a drag costs one keyframe at its end rather than
    /// one for each step, however often the accessibility API wakes the probe.
    #[test]
    fn a_resize_rebuilds_only_once_the_size_holds() {
        let mut debounce = ResizeDebounce::default();
        let t0 = Instant::now();
        let at = |ms: u64| t0.checked_add(Duration::from_millis(ms)).expect("a time after t0");
        let built = (800, 600);
        assert_eq!(debounce.observe((800, 600), built, at(0)), None, "no change");
        // Dragging: a new size on every probe.
        assert_eq!(debounce.observe((820, 610), built, at(100)), None);
        assert_eq!(debounce.observe((840, 620), built, at(200)), None);
        assert_eq!(debounce.observe((860, 630), built, at(300)), None);
        // The same size again, woken sooner than the hold: not yet.
        assert_eq!(debounce.observe((860, 630), built, at(340)), None, "held 40 ms");
        // Released: the same size a hold later.
        assert_eq!(debounce.observe((860, 630), built, at(400)), Some((860, 630)));
        // Built for it now; a size that goes back before it holds rebuilds nothing.
        let built = (860, 630);
        assert_eq!(debounce.observe((900, 700), built, at(500)), None);
        assert_eq!(debounce.observe((860, 630), built, at(600)), None, "back where it was");
        assert_eq!(
            debounce.observe((900, 700), built, at(700)),
            None,
            "a fresh candidate, not the old one"
        );
        assert_eq!(debounce.observe((900, 700), built, at(800)), Some((900, 700)));
    }

    /// The encode latency is the time between a frame's submit and its return, matched by
    /// pts whatever order the encoder returns them in, with the stripes coded from its capture;
    /// a return with no submit on record is not a sample, and the record holds `IN_FLIGHT_MAX`
    /// frames at most.
    #[test]
    fn encode_latency_is_matched_by_pts_and_bounded_in_flight() {
        let coder = Coder::new(StreamId(1));
        let frame = |pts_us, at_us, stripes| Submitted { pts_us, at_us, stripes, region: None };
        let region = Some(Region { x: 64, y: 0, w: 640, h: 360 });
        coder.submitted(frame(1, 1_000, 0b11));
        coder.submitted(Submitted { region, ..frame(2, 2_000, 0b10) });
        assert_eq!(
            coder.returned(2, 2_500),
            Some((500, Submitted { region, ..frame(2, 2_000, 0b10) }))
        );
        assert_eq!(coder.returned(1, 4_000), Some((3_000, frame(1, 1_000, 0b11))));
        assert_eq!(coder.returned(9, 5_000), None);
        for pts in 0..u64::try_from(IN_FLIGHT_MAX).unwrap_or(u64::MAX) {
            coder.submitted(frame(100 + pts, 10_000, 0));
        }
        coder.submitted(frame(200, 10_000, 0));
        let oldest = coder.in_flight.lock().front().map(|f| f.pts_us);
        assert_eq!(oldest, Some(101), "the oldest forgotten");
        assert_eq!(coder.returned(100, 20_000), None, "a forgotten frame is not a sample");
    }

    /// A capture counts once its last stripe is back, with the slowest stripe's encode and a
    /// keyframe in either; a stripe of another capture, or one back twice, counts nothing.
    #[test]
    fn a_striped_capture_counts_when_its_last_stripe_is_back() {
        let mut join = Join { pts: 7, waiting: 0b11, took: None, keyframe: false };
        assert_eq!(join.returned(1, 7, Some(9_000), true), None, "the top stripe is still out");
        assert_eq!(join.returned(1, 7, Some(1), false), None, "back twice");
        assert_eq!(join.returned(0, 6, Some(1), false), None, "another capture");
        assert_eq!(join.returned(0, 7, Some(8_000), false), Some((Some(9_000), true)));
        let mut whole = Join { pts: 8, waiting: 0b01, took: None, keyframe: false };
        assert_eq!(whole.returned(0, 8, Some(5), false), Some((Some(5), false)), "one picture");
    }

    /// The media stream and frame prefix of each video datagram in `sent`, fragment 0 only.
    fn prefixes(sent: &[Bytes]) -> Vec<(StreamId, u8, u8)> {
        use slopty_proto::media::{FramePrefix, MediaHeader};
        sent.iter()
            .filter_map(|datagram| {
                let (header, payload) = MediaHeader::parse(datagram)?;
                if header.is_parity() || header.index.get() != 0 {
                    return None;
                }
                let (prefix, _) = FramePrefix::parse(payload)?;
                Some((StreamId(header.stream.get()), prefix.stripes, prefix.build))
            })
            .collect()
    }

    /// A striped capture goes to both stripes' coders under one stamp, its prefix naming both;
    /// a refresh the lower stripe's lane asks for, on the lower stripe's media stream, codes
    /// that stripe alone, its prefix naming it alone on that stream, and the top stripe is not
    /// coded again. A NACK there is answered from that stripe's frames alone.
    #[test]
    fn a_striped_capture_codes_both_and_a_refresh_only_its_stripe() {
        let (shared, wire) = shared_for_frames();
        let lower_media = Stripe::media_of(StreamId(1), 1);
        let control = StreamControl { stream: Arc::<Shared>::clone(&shared), coder: 0 };
        shared.fps.store(30, Ordering::Relaxed);
        shared.encoder.lock().layout = slopty_codec::stripes::layout(1968);
        shared.lower.session.store(1, Ordering::Relaxed);
        shared.lower.pending.lock().keyframe = false;
        let flight = |coder: &Coder| -> Vec<(u64, u8)> {
            coder.in_flight.lock().iter().map(|f| (f.pts_us, f.stripes)).collect()
        };
        let mut frame = a_frame();
        let base = host_now_us();
        frame.capture_ts_us = base;
        shared.on_frame(again(&frame));
        assert_eq!(flight(&shared.top), [(base, 0b11)], "both stripes, one stamp");
        assert_eq!(flight(&shared.lower), [(base, 0b11)]);

        // The sessions return both stripes inside their submits.
        let packet = |pts_us| EncodedPacket {
            data: vec![7; 2000],
            keyframe: false,
            ltr_token: None,
            ltr_refresh: false,
            discardable: false,
            mse: None,
            pts_us,
        };
        shared.on_packet(0, &packet(base));
        shared.on_packet(1, &packet(base));

        control.of_media(lower_media).request_refresh(3, false);
        assert!(!shared.top.pending.lock().refresh, "the top stripe was not asked");
        let at = shared.repair_at().expect("a refresh is owed");
        assert_eq!(shared.repair_now(at), Attempt::Sent);
        let pts = shared.last_encoded_us.load(Ordering::Relaxed);
        assert!(pts > base);
        assert!(flight(&shared.top).is_empty(), "the top stripe is not coded again");
        assert_eq!(flight(&shared.lower), [(pts, 0b10)], "the lower one alone");
        assert!(!shared.lower.pending.lock().refresh, "the refresh went out");
        shared.on_packet(1, &packet(pts));
        let sent: Vec<(StreamId, u8)> = prefixes(&wire.drain())
            .into_iter()
            .map(|(media, stripes, _)| (media, stripes))
            .collect();
        assert_eq!(sent, [(StreamId(1), 0b11), (lower_media, 0b11), (lower_media, 0b10)]);
        assert_eq!(shared.stats().encoded, 2, "the capture once, the refresh once");

        control.of_media(lower_media).nack(1, &[0]);
        let answered = wire.drain();
        assert_eq!(answered.len(), 1);
        assert_eq!(prefixes(&answered), [(lower_media, 0b10, 0)], "the lower stripe's frame 1");
    }

    /// Sessions put in name their build on both stripes' frames from then on, and a later
    /// build another: what keeps the client from putting a stripe of each together.
    #[test]
    fn both_stripes_frames_name_the_build_they_came_from() {
        let wire = Wire::new();
        let sink: Arc<dyn DatagramSink> = Arc::<Wire>::clone(&wire);
        let shared = Arc::new(Shared::<Recording>::new(StreamId(1), sink, 8_000_000, 60, false));
        let packet = |pts_us| EncodedPacket {
            data: vec![3; 500],
            keyframe: true,
            ltr_token: None,
            ltr_refresh: false,
            discardable: false,
            mse: None,
            pts_us,
        };
        let mut builds = Vec::new();
        for sessions in [[3, 4], [5, 6]] {
            let built = Built::<Recording> {
                top: recorder(sessions[0], Log::default()),
                lower: Some(recorder(sessions[1], Log::default())),
                layout: slopty_codec::stripes::layout(1968),
            };
            drop(shared.install(built, sessions));
            shared.put_in(&mut shared.encoder.lock());
            shared.on_packet(0, &packet(sessions[0]));
            shared.on_packet(1, &packet(sessions[0]));
            builds.extend(prefixes(&wire.drain()).into_iter().map(|(_, _, build)| build));
        }
        assert_eq!(builds, [3, 3, 5, 5]);
    }

    /// The sessions of a build for the whole picture.
    fn whole<P: Platform>(top: P::Video) -> Built<P> {
        Built { top, lower: None, layout: None }
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
        assert_eq!(wire.drain(), Vec::<Bytes>::new());
        let stats = shared.stats();
        assert_eq!((stats.datagrams, stats.queue_full), (5, 4));
    }

    /// An access unit from the encoder is packetized and handed over in two calls, its data and
    /// then the parity computed over it, and counted; a NACK for a frame in the packetizer's
    /// history answers with those fragments, one outside it with nothing, and none at all while
    /// QUIC holds more than the frame budget.
    #[test]
    fn an_encoded_packet_is_sent_and_a_nack_answers_from_history() {
        let (shared, wire) = shared_for_frames();
        shared.counters.bitrate_bps.store(30_000_000, Ordering::Relaxed);
        let packet = EncodedPacket {
            data: vec![7; 3000],
            keyframe: true,
            ltr_token: Some(1),
            ltr_refresh: false,
            discardable: false,
            mse: None,
            pts_us: host_now_us(),
        };
        shared.on_packet(0, &packet);
        let sent = wire.drain();
        assert!(sent.len() >= 3, "3 000 bytes under the MTU: {}", sent.len());
        assert_eq!(wire.calls.load(Ordering::Relaxed), 2, "the data, then its parity");
        let stats = shared.stats();
        assert_eq!((stats.encoded, stats.datagrams), (1, sent.len() as u64));
        shared.nack(0, 0, &[0, 1]);
        assert_eq!(wire.drain().len(), 2, "two fragments of frame 0 again");
        shared.nack(0, 0, &[]);
        assert!(!wire.drain().is_empty(), "no fragments named: the whole frame's data");
        shared.nack(0, 9, &[0]);
        assert!(wire.drain().is_empty(), "frame 9 was never sent");
        wire.held.store(10_000_000, Ordering::Relaxed);
        shared.nack(0, 0, &[0]);
        assert!(wire.drain().is_empty(), "QUIC is holding seconds of frames");
    }

    /// A refresh request marks the next frame; a report hands acknowledged LTR tokens to the
    /// encoder's options and counts the datagrams sent since the last one.
    #[test]
    fn a_refresh_and_a_report_reach_the_next_frame_options() {
        let (shared, _wire) = shared_for_frames();
        for token in [5, 6] {
            shared.top.ltr.lock().on_packet(false, Some(token), 0);
        }
        shared.request_refresh(0, 41, false);
        assert!(shared.top.pending.lock().refresh);
        assert_eq!(shared.stats().refreshes, 1);
        let mut acked_ltr = [0; 4];
        acked_ltr[..2].copy_from_slice(&[5, 6]);
        let report = ReceiverReport { acked_ltr, acked_ltr_len: 2, ..ReceiverReport::default() };
        let _decision = shared.report(0, &report, None);
        assert_eq!(shared.top.pending.lock().acked, vec![5, 6], "two of the four slots were valid");
        assert_eq!(shared.top.sent_at_report.load(Ordering::Relaxed), 0, "nothing sent yet");
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
            discardable: false,
            mse: None,
            pts_us: host_now_us(),
        }
    }

    /// A window stream's pointer is placed by its input, so the cursor loop sleeps until the
    /// input moves it, or the target's bounds, its visibility or the zoom change: with no
    /// pointer event it does not wake at all, where it used to sample 120 times a second.
    #[tokio::test]
    async fn the_cursor_loop_sleeps_until_the_placed_pointer_moves() {
        let (shared, wire) = shared_for_frames();
        *shared.bounds.lock() = Some(Rect { x: 100.0, y: 50.0, w: 800.0, h: 600.0 });
        let input = PointerWatch::default();
        let cursor = tokio::spawn(cursor_loop(Arc::clone(&shared), 1.0, input.clone()));
        let wakes = || shared.cursor_wakes.load(Ordering::Relaxed);
        let settle = || tokio::time::sleep(Duration::from_millis(300));
        settle().await;
        assert_eq!(
            (wakes(), wire.drain().len()),
            (1, 1),
            "one sample (nothing placed), then asleep"
        );
        input.place(110.0, 70.0);
        settle().await;
        assert_eq!((wakes(), wire.drain().len()), (2, 1), "one move, one wake, one sample");
        input.place(110.0, 70.0);
        settle().await;
        assert_eq!(wakes(), 2, "the same place is not a move");
        shared.set_zoom(2.0);
        settle().await;
        assert_eq!((wakes(), wire.drain().len()), (3, 1), "a zoom rescales a still pointer");
        *shared.bounds.lock() = Some(Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 });
        settle().await;
        assert_eq!(wakes(), 3, "bounds are news only from the probe, which wakes the loop");
        shared.set_hidden(true);
        settle().await;
        input.place(120.0, 70.0);
        settle().await;
        assert_eq!((wakes(), wire.drain().len()), (5, 0), "hidden: woken, nothing sampled");
        assert!(!shared.pointer_over.load(Ordering::Relaxed), "nothing is over a hidden target");
        cursor.abort();
    }

    /// Cursor loop rounds over ten seconds with nobody moving the pointer: a placed pointer (a
    /// window stream's) against the real one (a display stream's), which is read every period
    /// as every stream's was before the placed one waited on its input. `idle stream wakeups`
    /// in MEASUREMENTS.md.
    #[tokio::test]
    #[ignore = "measurement"]
    async fn idle_cursor_wakes() {
        const WINDOW: Duration = Duration::from_secs(10);
        // The first pointer read of a process takes seconds; it is paid before the count.
        tokio::task::spawn_blocking(Source::<Native>::pointer_location).await.expect("a read");
        let rounds = async |real: bool| {
            let (shared, _wire) = shared_for_frames();
            *shared.bounds.lock() = Some(Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 });
            let input = PointerWatch::default();
            if real {
                input.follow_real();
            }
            let cursor = tokio::spawn(cursor_loop(Arc::clone(&shared), 1.0, input));
            tokio::time::sleep(WINDOW).await;
            cursor.abort();
            shared.cursor_wakes.load(Ordering::Relaxed)
        };
        let (placed, real) = tokio::join!(rounds(false), rounds(true));
        let rate = |n: u64| {
            #[expect(clippy::cast_precision_loss, reason = "a small count")]
            let rate = n as f64 / WINDOW.as_secs_f64();
            rate
        };
        eprintln!(
            "idle_cursor_wakes: placed {placed} ({:.1}/s), real {real} ({:.1}/s) in {} s",
            rate(placed),
            rate(real),
            WINDOW.as_secs()
        );
    }

    /// The accessibility API's word that the target moved wakes the geometry probe, and is
    /// neither a suspicion nor a sibling gone; so does the first frame after the client was
    /// told the source is idle, once.
    #[tokio::test]
    async fn a_move_or_a_frame_after_idle_wakes_the_geometry_probe() {
        let (shared, _wire) = shared_for_frames();
        let wake = Arc::clone(&shared.geometry_wake);
        let woken = async |wake: &tokio::sync::Notify| {
            tokio::time::timeout(Duration::from_millis(100), wake.notified()).await.is_ok()
        };
        shared.heard(Went::Moved);
        assert!(woken(&wake).await, "a move wakes the probe");
        let seen = shared.stats();
        assert_eq!((seen.suspicions, seen.siblings), (0, 0), "a move is not a going");
        assert!(!shared.suspected_at(host_now_us()));

        shared.on_packet(0, &packet(900, false, None, false));
        assert!(!woken(&wake).await, "a frame while live is no news");
        shared.source_idle.store(true, Ordering::Relaxed);
        shared.on_packet(0, &packet(900, false, None, false));
        assert!(woken(&wake).await, "the first frame after idle wakes the probe");
        shared.on_packet(0, &packet(900, false, None, false));
        assert!(!woken(&wake).await, "and only the first");
    }

    #[test]
    fn a_keyframe_is_deferred_only_when_a_refresh_can_go_out_instead() {
        let (shared, _wire) = shared_for_frames();
        shared.top.keyframe_bytes.store(133_960, Ordering::Relaxed);
        shared.counters.bitrate_bps.store(1_000_000, Ordering::Relaxed);
        // No reference is usable, so a refresh would come back as an IDR anyway and the keyframe
        // goes out however badly it fits.
        assert!(shared.keyframe_admitted(&shared.top, 1_000_000));
        assert_eq!(shared.stats().keyframes_deferred, 0);

        shared.on_packet(0, &packet(900, false, Some(7), false));
        shared.report(0, &acking(&[7]), None);
        // The report moved the parity, and with it the target to the controller's own.
        shared.counters.bitrate_bps.store(1_000_000, Ordering::Relaxed);
        assert!(!shared.keyframe_admitted(&shared.top, 1_000_000), "now a refresh is a picture");
        assert_eq!(shared.stats().keyframes_deferred, 1);

        // The run is one episode however many frames it spans, and the valve opens a second in.
        assert!(!shared.keyframe_admitted(&shared.top, 1_016_000));
        assert!(!shared.keyframe_admitted(&shared.top, 1_999_999));
        assert_eq!(shared.stats().keyframes_deferred, 1);
        assert!(
            shared.keyframe_admitted(&shared.top, 2_000_000),
            "the valve opens rather than hold forever"
        );
    }

    /// A client whose decoder lost its session holds none of the references it acknowledged, so
    /// the refresh it asks for as a keyframe is an IDR: not an LTR delta off the reference, and
    /// not deferred for one however badly it fits the link.
    #[test]
    fn a_keyframe_refresh_is_an_idr_even_with_a_usable_reference() {
        let (shared, _wire) = shared_for_frames();
        shared.top.keyframe_bytes.store(133_960, Ordering::Relaxed);
        shared.counters.bitrate_bps.store(1_000_000, Ordering::Relaxed);
        shared.on_packet(0, &packet(900, false, Some(7), false));
        shared.report(0, &acking(&[7]), None);
        shared.request_refresh(0, 41, false);
        let (keyframe, refresh) = {
            let pending = shared.top.pending.lock();
            (pending.keyframe, pending.refresh)
        };
        assert!(refresh && !keyframe && shared.top.ltr_usable(), "a plain refresh: a delta off 7");
        shared.top.pending.lock().refresh = false;

        shared.request_refresh(0, 41, true);
        let (keyframe, refresh) = {
            let pending = shared.top.pending.lock();
            (pending.keyframe, pending.refresh)
        };
        assert!(keyframe && !refresh, "the encoder is asked for a keyframe");
        assert!(!shared.top.ltr_usable(), "reference 7 went with the client's session");
        assert!(
            shared.keyframe_admitted(&shared.top, 1_000_000),
            "so nothing is left to defer it for"
        );
        assert_eq!(shared.stats().keyframes_deferred, 0);
        shared.report(0, &acking(&[7]), None);
        assert!(!shared.top.ltr_usable(), "a late ack from the lost session names nothing");
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
        shared.dropped(1_000_000, 1);
        assert!(shared.top.pending.lock().refresh, "no estimate means ask anyway");
        assert_eq!(shared.counters.dropped.load(Ordering::Relaxed), 1);

        shared.top.pending.lock().refresh = false;
        shared.top.keyframe_bytes.store(133_960, Ordering::Relaxed);
        shared.dropped(1_000_000, 1);
        assert!(
            !shared.top.pending.lock().refresh,
            "1 Mbit/s cannot drain 134 kB: asking would only deepen the hole"
        );
        assert_eq!(shared.counters.dropped.load(Ordering::Relaxed), 2, "still counted as a drop");

        // The link recovers; the next hole is worth a picture again.
        shared.counters.bitrate_bps.store(12_000_000, Ordering::Relaxed);
        shared.dropped(1_100_000, 1);
        assert!(shared.top.pending.lock().refresh);
    }

    /// A link that recovers ends the episode by itself, not only a keyframe encoded: the next
    /// collapse is its own episode with its own clock. With the first episode's start left
    /// standing, the second one's first frame found the valve a second past and let the
    /// keyframe straight through.
    #[test]
    fn a_link_that_recovers_ends_the_deferral_without_the_valve() {
        let (shared, _wire) = shared_for_frames();
        shared.on_packet(0, &packet(900, false, Some(3), false));
        shared.report(0, &acking(&[3]), None);
        shared.top.keyframe_bytes.store(133_960, Ordering::Relaxed);
        shared.counters.bitrate_bps.store(1_000_000, Ordering::Relaxed);
        assert!(!shared.keyframe_admitted(&shared.top, 1_000_000));
        // The rate controller climbed back: 12 Mbit/s carries 600 kB in the drain window.
        shared.counters.bitrate_bps.store(12_000_000, Ordering::Relaxed);
        assert!(shared.keyframe_admitted(&shared.top, 1_100_000), "not the valve, the link");
        assert_eq!(shared.stats().keyframes_deferred, 1, "still the one episode");

        // Five seconds on the link collapses again: a new episode, deferred from its start.
        shared.counters.bitrate_bps.store(1_000_000, Ordering::Relaxed);
        assert!(
            !shared.keyframe_admitted(&shared.top, 6_000_000),
            "a stale start opened the valve at once"
        );
        assert_eq!(shared.stats().keyframes_deferred, 2, "counted as its own episode");
        assert!(!shared.keyframe_admitted(&shared.top, 6_999_999));
        assert!(
            shared.keyframe_admitted(&shared.top, 7_000_000),
            "its own valve, a second after its own start"
        );
    }

    /// A token is a reference a refresh can use only while nothing has emptied the reference
    /// list since it was offered: a keyframe does, and so does a new encoder session. Tokens the
    /// session never offered do not reach the encoder at all.
    #[test]
    fn a_reference_is_usable_until_a_keyframe_or_a_rebuild() {
        let (shared, _wire) = shared_for_frames();
        shared.on_packet(0, &packet(40_000, true, None, false));
        shared.on_packet(0, &packet(900, false, Some(11), false));
        assert!(!shared.top.ltr_usable(), "offered, not acknowledged");
        shared.report(0, &acking(&[11, 99]), None);
        assert!(shared.top.ltr_usable());
        assert_eq!(shared.top.pending.lock().acked, vec![11], "99 was never offered");
        let seen = shared.stats().ltr;
        assert_eq!((seen.offered, seen.acked, seen.usable), (1, 1, true));

        // A refresh answered as a delta, then one answered as an IDR: the IDR retires the token.
        shared.on_packet(0, &packet(700, false, None, true));
        assert!(shared.top.ltr_usable());
        shared.on_packet(0, &packet(40_000, true, None, true));
        assert!(!shared.top.ltr_usable(), "an IDR empties the reference list");
        shared.report(0, &acking(&[11]), None);
        assert!(!shared.top.ltr_usable(), "a late ack for the old epoch names nothing usable");
        let seen = shared.stats().ltr;
        assert_eq!((seen.refreshes_delta, seen.refreshes_idr), (1, 1));

        // A token offered after the keyframe is usable again, until the encoder is rebuilt.
        shared.on_packet(0, &packet(900, false, Some(12), false));
        shared.report(0, &acking(&[12]), None);
        assert!(shared.top.ltr_usable());
        shared.top.pending.lock().acked = vec![12];
        shared.top.keyframe_bytes.store(50_000, Ordering::Relaxed);
        shared.top.keyframe_deferred_us.store(1, Ordering::Relaxed);
        shared.top.rebuilt();
        assert!(!shared.top.ltr_usable(), "a new session predicts from none of the old references");
        assert!(shared.top.pending.lock().acked.is_empty(), "nor is it told about them");
        assert!(shared.top.pending.lock().keyframe, "and it starts on a keyframe");
        assert_eq!(shared.top.keyframe_bytes.load(Ordering::Relaxed), 0, "of a size not yet known");
        assert_eq!(shared.top.keyframe_deferred_us.load(Ordering::Relaxed), 0);
        shared.report(0, &acking(&[12]), None);
        assert!(shared.top.pending.lock().acked.is_empty(), "the old session's token is dropped");
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

        // A refresh on the still picture: answered from the held capture at the rung's next
        // slot, which the repair, 8 ms late in its own, brought nearer by that much.
        shared.request_refresh(0, 3, false);
        let refresh_at = shared.repair_at().expect("a refresh is owed");
        assert_eq!(refresh_at, base + 2 * 33_333 - 4_166);
        assert_eq!(shared.repair_now(refresh_at), Attempt::Sent);
        assert!(!shared.top.pending.lock().refresh, "the refresh went out with it");
        assert_eq!(shared.stats().repaired, 2);

        // A still target that is hidden is not repaired: the held capture is dropped.
        shared.request_refresh(0, 4, false);
        shared.target_hidden.store(true, Ordering::Relaxed);
        assert_eq!(shared.repair_now(refresh_at + 100_000), Attempt::Nothing);
        assert_eq!(shared.repair_at(), None, "nothing held any more");
    }

    /// A still picture is coded again once it has gone quiet, one frame at a time while the
    /// encoder's error keeps falling, and stops when it no longer does. Refinements are two
    /// periods apart and stamped one early, go only onto an empty link, and are no part of what
    /// the stream reports as encoded.
    #[test]
    fn a_still_picture_is_refined_until_the_encoder_stops_gaining() {
        let (shared, wire) = shared_for_frames();
        shared.counters.encoder_bps.store(8_000_000, Ordering::Relaxed);
        let period = period_us(60);
        let error = |db: f64| slopty_codec::Mse {
            luma: 255.0 * 255.0 / 10_f64.powf(db / 10.0),
            chroma: None,
        };
        let came_back = |pts_us: u64, db: f64| EncodedPacket {
            data: vec![7; 900],
            keyframe: false,
            ltr_token: None,
            ltr_refresh: false,
            discardable: false,
            mse: Some(error(db)),
            pts_us,
        };
        let mut frame = a_frame();
        let base = host_now_us();
        frame.capture_ts_us = base;
        shared.on_frame(again(&frame));
        shared.on_packet(0, &came_back(base, 48.5));
        let mut at = shared.repair_at().expect("worth refining");
        assert!(at >= base + 2 * period, "two periods after the picture: {}", at - base);
        wire.held.store(1, Ordering::Relaxed);
        assert_eq!(shared.repair_now(at), Attempt::NoRoom, "only onto an empty link");
        wire.held.store(0, Ordering::Relaxed);
        for db in [49.1, 50.5, 50.8, 51.1] {
            assert_eq!(shared.repair_now(at), Attempt::Sent);
            let pts = shared.last_encoded_us.load(Ordering::Relaxed);
            assert_eq!(pts, at - period, "stamped a period early");
            shared.on_packet(0, &came_back(pts, db));
            let next = shared.repair_at().expect("still gaining");
            assert!(next >= at + 2 * period, "two periods apart: {next} after {at}");
            at = next;
        }
        assert_eq!(shared.repair_now(at - 1), Attempt::Nothing, "not before its time");
        for db in [51.15, 51.2] {
            assert_eq!(shared.repair_now(at), Attempt::Sent);
            let pts = shared.last_encoded_us.load(Ordering::Relaxed);
            shared.on_packet(0, &came_back(pts, db));
            at = shared.repair_at().unwrap_or(at);
        }
        assert_eq!(shared.repair_at(), None, "the encoder stopped gaining");
        assert_eq!(shared.counters.refined.load(Ordering::Relaxed), 6);
        let stats = shared.stats();
        assert_eq!(stats.repaired, 0, "a refinement is not a repair");
        assert_eq!(stats.encoded, 1, "nor an encoded frame: a still source reads idle");
        assert_eq!(stats.encode.n, 1, "nor in the encode figures");

        // A new picture, one refinement of it that came back large, and a change taken 2 ms
        // before the refinement went and handed over after it: the change goes at once with its
        // own capture time as its stamp, though QUIC still holds more of the refinement than the
        // guard lets any other frame pass.
        frame.capture_ts_us = at + 100_000;
        shared.on_frame(again(&frame));
        shared.on_packet(0, &came_back(frame.capture_ts_us, 40.0));
        let refine_at = shared.repair_at().expect("a new still picture");
        assert_eq!(shared.repair_now(refine_at), Attempt::Sent);
        let refinement = shared.last_encoded_us.load(Ordering::Relaxed);
        shared
            .on_packet(0, &EncodedPacket { data: vec![7; 60_000], ..came_back(refinement, 41.0) });
        let queued = usize::try_from(shared.top.refine_wire.load(Ordering::Relaxed)).unwrap();
        assert!(queued > 60_000, "its datagrams are on record: {queued}");
        wire.held.store(queued, Ordering::Relaxed);
        frame.capture_ts_us = refine_at - 2_000;
        shared.on_frame(again(&frame));
        let change = shared.last_encoded_us.load(Ordering::Relaxed);
        assert_eq!(change, refine_at - 2_000, "sent at once, stamped when it was taken");
        assert!(change > refinement, "after the refinement's stamp");
        assert!(!shared.owed.load(Ordering::Relaxed));
        assert_eq!(shared.stats().dropped, 0, "nor dropped by the guard");
        wire.held.store(queued + 1, Ordering::Relaxed);
        shared.on_packet(0, &came_back(change, 42.0));
        assert_eq!(shared.top.refine_wire.load(Ordering::Relaxed), 0, "any other frame ends that");
    }

    /// A keyframe or refresh answered while the picture is still is a frame of that picture: it
    /// does not start refinement over, so refreshes cannot keep it going past its cap; and a
    /// forgotten capture or a new session stops it.
    #[test]
    fn refreshes_forgetting_and_a_rebuild_end_refinement() {
        let (shared, _wire) = shared_for_frames();
        shared.counters.encoder_bps.store(8_000_000, Ordering::Relaxed);
        let back = |pts_us: u64, db: f64| EncodedPacket {
            data: vec![7; 900],
            keyframe: false,
            ltr_token: None,
            ltr_refresh: false,
            discardable: false,
            mse: Some(slopty_codec::Mse {
                luma: 255.0 * 255.0 / 10_f64.powf(db / 10.0),
                chroma: None,
            }),
            pts_us,
        };
        let mut frame = a_frame();
        frame.capture_ts_us = host_now_us();
        shared.on_frame(again(&frame));
        shared.on_packet(0, &back(frame.capture_ts_us, 30.0));
        let mut db = 30.0;
        let mut rounds = 0;
        while let Some(at) = shared.repair_at() {
            assert_eq!(shared.repair_now(at), Attempt::Sent);
            db += 1.0;
            shared.on_packet(0, &back(shared.last_encoded_us.load(Ordering::Relaxed), db));
            // The client asks for a refresh after every refinement; with no reference
            // acknowledged it is answered with a keyframe.
            shared.request_refresh(0, 1, false);
            let refresh_at = shared.repair_at().expect("the refresh");
            assert_eq!(shared.repair_now(refresh_at), Attempt::Sent);
            let pts = shared.last_encoded_us.load(Ordering::Relaxed);
            shared.on_packet(0, &EncodedPacket { keyframe: true, ..back(pts, db - 2.0) });
            rounds += 1;
            assert!(rounds <= slopty_media::MAX_REFINEMENTS, "refreshes kept it going");
        }
        assert_eq!(
            shared.counters.refined.load(Ordering::Relaxed),
            u64::from(slopty_media::MAX_REFINEMENTS)
        );

        frame.capture_ts_us = host_now_us();
        shared.on_frame(again(&frame));
        shared.on_packet(0, &back(frame.capture_ts_us, 30.0));
        assert!(shared.repair_at().is_some());
        shared.forget_held();
        assert_eq!(shared.repair_at(), None, "forgotten");
        assert!(shared.refine_due(&shared.top, 1).is_none());
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
        let at = repair_at(owed, base + 16_667, base + 33_333 - 4_166, 60);
        assert_eq!(at, Some(base + 16_667 + 25_000));
        // A 120 Hz ceiling on a 60 Hz panel waits as long, not 12.5 ms.
        assert_eq!(repair_at(owed, base, base, 120), Some(base + 25_000));
        // A keyframe does not wait for the cadence, only for the quiet.
        assert_eq!(repair_at(keyframe, base, base + 66_667, 60), Some(base + 25_000));
        assert_eq!(repair_at(Asks::default(), base, base, 60), None, "nothing owed");
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
        let clock = Arc::new(sound::SoundClock::default());
        let _first = shared.sound.set(Arc::clone(&clock));
        clock.sent_at(if lane { base } else { 0 });
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
                clock.sent_at(if lane { now } else { 0 });
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
        /// The pictures it takes; any other size is refused, as VideoToolbox's session is.
        size: (usize, usize),
        /// What it was told of temporal layers, in order.
        layers: Arc<Mutex<Vec<bool>>>,
        /// Frames it says it dropped.
        dropped: Arc<AtomicU64>,
        /// Where it hands each frame's packet inside the submit, as an aligned VideoToolbox
        /// session does; `None` holds them.
        inline: Option<Emit>,
        /// What each submit waits for before it codes, as a call into VideoToolbox that does not
        /// return; `None` codes at once.
        hold: Option<Arc<Latch>>,
    }

    /// A gate a thread waits at until the test opens it.
    #[derive(Default)]
    struct Latch {
        open: Mutex<bool>,
        opened: parking_lot::Condvar,
        waiting: AtomicBool,
    }

    impl Latch {
        fn wait(&self) {
            self.waiting.store(true, Ordering::Release);
            let mut open = self.open.lock();
            while !*open {
                self.opened.wait(&mut open);
            }
        }

        fn open(&self) {
            *self.open.lock() = true;
            self.opened.notify_all();
        }

        /// Wait until a thread waits here.
        fn reached(&self) {
            let deadline = Instant::now().checked_add(Duration::from_secs(30)).expect("a deadline");
            while !self.waiting.load(Ordering::Acquire) {
                assert!(Instant::now() < deadline, "nothing reached the latch");
                std::thread::yield_now();
            }
        }
    }

    /// A packet handed on from inside a [`Recorder`]'s submit.
    type Emit = Arc<dyn Fn(EncodedPacket) + Send + Sync>;

    /// A [`Recorder`] session numbered `session`, logging to `log`, of [`a_frame`]'s size.
    fn recorder(session: u64, log: Log) -> Recorder {
        Recorder {
            session,
            log,
            size: (16, 16),
            layers: Arc::default(),
            dropped: Arc::default(),
            inline: None,
            hold: None,
        }
    }

    impl slopty_codec::VideoEncoder for Recorder {
        type Image = slopty_codec::PixelBuffer;

        fn new(
            _config: EncoderConfig,
            _sink: impl Fn(EncodedPacket) + Send + Sync + 'static,
        ) -> Result<Self, CodecError> {
            Ok(recorder(0, Log::default()))
        }

        fn encode(
            &self,
            image: &Self::Image,
            pts_us: u64,
            options: &FrameOptions,
        ) -> Result<(), CodecError> {
            let size = (image.width(), image.height());
            if size != self.size {
                return Err(CodecError::WrongSize { image: size, session: self.size });
            }
            self.log.lock().push((self.session, options.force_keyframe));
            if let Some(hold) = &self.hold {
                hold.wait();
            }
            if let Some(emit) = &self.inline {
                emit(EncodedPacket { pts_us, ..packet(900, options.force_keyframe, None, false) });
            }
            Ok(())
        }

        fn set_bitrate(&self, _bps: u32) -> Result<(), CodecError> {
            Ok(())
        }

        fn set_frame_rate(&self, _fps: u16) -> Result<(), CodecError> {
            Ok(())
        }

        fn set_temporal_layers(&self, on: bool) -> Result<bool, CodecError> {
            self.layers.lock().push(on);
            Ok(on)
        }

        fn frames_dropped(&self) -> u64 {
            self.dropped.load(Ordering::Relaxed)
        }
    }

    /// Hand `shared` the capture `frame` as the repair loop would, a second after the last one
    /// so the cadence never holds it back.
    fn encode_held(
        shared: &Shared<Recording>,
        frame: CapturedFrame,
        at: &std::cell::Cell<u64>,
    ) -> Attempt {
        at.set(at.get().saturating_add(1_000_000));
        shared.held_us.store(frame.capture_ts_us, Ordering::Relaxed);
        *shared.held.lock() = Some(frame);
        shared.owed.store(true, Ordering::Relaxed);
        shared.try_encode(None, at.get())
    }

    /// What the encode path costs around the call into the encoder: a fresh capture through
    /// [`Shared::on_frame`] to its datagrams on the wire, its session a [`Recorder`] that hands a
    /// 900-byte frame on inside the submit, as an aligned VideoToolbox session does. Retired
    /// instructions on this thread, in release (MEASUREMENTS.md, "an encode that never
    /// returned"):
    /// `cargo test -p slopty-worker --release --lib measure_the_encode_path -- --ignored`.
    #[test]
    #[ignore = "a measurement, run in release"]
    fn measure_the_encode_path_around_the_encoder() {
        let wire = Wire::new();
        let sink: Arc<dyn DatagramSink> = Arc::<Wire>::clone(&wire);
        let shared = Arc::new(Shared::<Recording>::new(StreamId(1), sink, 8_000_000, 60, false));
        let weak = Arc::downgrade(&shared);
        let inline: Emit = Arc::new(move |packet| {
            if let Some(shared) = weak.upgrade() {
                shared.on_session_packet(0, 1, &packet);
            }
        });
        let session = Recorder { inline: Some(inline), ..recorder(1, Log::default()) };
        drop(shared.install(whole(session), [1, 0]));
        let frame = a_frame();
        let base = host_now_us();
        let bench = slopty_testkit::bench::Bench::new("worker.encode_path");
        let mut series = bench.series("fresh_capture");
        for i in 1..=4_000_u64 {
            // A second apart, so the cadence never holds one back.
            let capture = CapturedFrame {
                capture_ts_us: base.saturating_add(i * 1_000_000),
                ..again(&frame)
            };
            series.time(|| shared.on_frame(capture));
            if i % 64 == 0 {
                drop(wire.drain());
            }
        }
        assert_eq!(shared.stats().encoded, 4_000, "every capture went out");
        series.report().unwrap();
    }

    /// A lost session is replaced at once; a replacement lost too without coding a frame is
    /// replaced after [`LOST_RETRY`], doubling in a row up to [`LOST_RETRY_MAX`]; and a frame
    /// coded since the last replacement brings the next back to at once.
    #[test]
    fn replacements_lost_without_a_frame_are_rebuilt_ever_more_slowly() {
        let mut retry = LostRetry::default();
        let (first, max) = (micros(LOST_RETRY), micros(LOST_RETRY_MAX));
        assert_eq!(retry.due_us(0), 0, "the first loss: at once");
        retry.started(10_000_000, 5);
        assert_eq!(retry.due_us(6), 10_000_000, "the replacement coded: at once");
        assert_eq!(retry.due_us(5), 10_000_000 + first, "it coded nothing");
        let mut at = 10_000_000 + first;
        let mut waits = Vec::new();
        for _ in 0..10 {
            retry.started(at, 5);
            let due = retry.due_us(5);
            waits.push(due - at);
            at = due;
        }
        assert_eq!(&waits[..4], &[2 * first, 4 * first, 8 * first, 16 * first]);
        assert_eq!(waits.last(), Some(&max), "held at the most");
        retry.started(at, 5);
        assert_eq!(retry.due_us(7), at, "a frame since: at once again");
        retry.started(at + 1, 7);
        assert_eq!(retry.due_us(7), at + 1 + first, "and the doubling starts over");
    }

    /// The watch charges an encode inside the encoder only the time the beat ran on time: a
    /// look later than [`STUCK_CREDIT`] charges the credit. One there [`ENCODE_STUCK`] is given
    /// up on; another encode going in starts its own charge; and the patience doubles, up to
    /// [`ENCODE_STUCK_MAX`], while the sessions given up on bring no frame.
    #[test]
    fn the_watch_charges_the_beats_own_time_and_waits_longer_on_sessions_that_coded_nothing() {
        let beat = micros(HEARTBEAT_AFTER);
        let mut watch = StuckWatch::new(0);
        assert_eq!(watch.look(0, beat), None, "nothing inside");
        assert_eq!(watch.look(7, 2 * beat), None, "first seen: nothing charged");
        // The worker was held up for five seconds: the look charges its credit.
        let mut now = 2 * beat + 5_000_000;
        assert_eq!(watch.look(7, now), None);
        assert_eq!(watch.charged_us, micros(STUCK_CREDIT));
        let mut looks = 0_u64;
        let given_up = loop {
            now += beat;
            looks += 1;
            if let Some(turn) = watch.look(7, now) {
                break turn;
            }
            assert!(looks < 1_000, "never given up");
        };
        assert_eq!(given_up, 7);
        assert_eq!(looks, (micros(ENCODE_STUCK) - micros(STUCK_CREDIT)) / beat, "on time");

        watch.gave_up(10);
        assert_eq!(watch.patience_us, micros(ENCODE_STUCK), "the stream coded since");
        assert_eq!(watch.look(8, now + beat), None);
        assert_eq!(watch.look(9, now + 2 * beat), None, "another encode: its own charge");
        assert_eq!(watch.charged_us, 0);
        let mut patience = Vec::new();
        for _ in 0..6 {
            watch.gave_up(10);
            patience.push(watch.patience_us / 1_000_000);
        }
        assert_eq!(patience, [4, 8, 16, 32, 32, 32], "seconds, while nothing is coded");
        watch.gave_up(11);
        assert_eq!(watch.patience_us, micros(ENCODE_STUCK), "back once a frame came");
    }

    /// An encode that never comes back from the encoder holds nothing but the stream's turn,
    /// and the beat takes that from it after [`ENCODE_STUCK`] of looks on time: the session's
    /// number goes out of force, the next capture waits for no one and finds no session in
    /// force, the replacement codes a keyframe, and what the stuck call coded when it comes
    /// back after all is dropped as a replaced session's.
    #[test]
    fn an_encode_that_never_comes_back_is_given_up_and_the_stream_goes_on() {
        let wire = Wire::new();
        let sink: Arc<dyn DatagramSink> = Arc::<Wire>::clone(&wire);
        let shared = Arc::new(Shared::<Recording>::new(StreamId(1), sink, 8_000_000, 60, false));
        let weak = Arc::downgrade(&shared);
        let inline: Emit = Arc::new(move |packet| {
            if let Some(shared) = weak.upgrade() {
                shared.on_session_packet(0, 1, &packet);
            }
        });
        let latch = Arc::new(Latch::default());
        let log = Log::default();
        let stuck = Recorder {
            inline: Some(inline),
            hold: Some(Arc::clone(&latch)),
            ..recorder(1, Arc::clone(&log))
        };
        drop(shared.install(whole(stuck), [1, 0]));
        let base = host_now_us();
        let capture = |k: u64| CapturedFrame { capture_ts_us: base + k * 1_000_000, ..a_frame() };
        let encoding = std::thread::spawn({
            let (shared, frame) = (Arc::clone(&shared), capture(1));
            move || shared.try_encode(Some((frame, None)), host_now_us())
        });
        latch.reached();
        let turn = shared.gate.inside.load(Ordering::Acquire);
        assert_ne!(turn, 0, "the encode is inside the encoder");
        assert!(shared.held.try_lock().is_some(), "the held capture is not locked across it");
        assert!(shared.encoder.try_lock().is_some(), "nor the sessions");

        let beat = micros(HEARTBEAT_AFTER);
        let mut now = base;
        shared.stuck.lock().looked_us = now;
        let mut looks = 0_u64;
        while shared.stats().encoders_replaced == 0 {
            now += beat;
            shared.unstick(now);
            looks += 1;
            assert!(looks < 1_000, "never given up");
        }
        assert_eq!(looks, 1 + micros(ENCODE_STUCK) / beat, "seen, then charged on time");
        assert_eq!(shared.top.session.load(Ordering::Relaxed), 0, "out of force");
        assert_eq!(shared.gate.inside.load(Ordering::Acquire), 0);

        assert_eq!(shared.try_encode(Some((capture(2), None)), host_now_us()), Attempt::Sent);
        assert_eq!(*log.lock(), vec![(1, true)], "nothing more for the session given up on");
        let replacement = Log::default();
        drop(shared.install(whole(recorder(2, Arc::clone(&replacement))), [2, 0]));
        assert_eq!(shared.try_encode(Some((capture(3), None)), host_now_us()), Attempt::Sent);
        assert_eq!(*replacement.lock(), vec![(2, true)], "the replacement's keyframe");

        latch.open();
        assert_eq!(encoding.join().unwrap(), Attempt::GivenUp, "came back after all");
        assert_eq!(shared.replaced_dropped.load(Ordering::Relaxed), 1, "what it coded, dropped");
        assert_eq!(shared.top.session.load(Ordering::Relaxed), 2, "the replacement stays");
        assert_eq!(shared.stats().encoders_replaced, 1);
    }

    /// Layers follow the gate only on a session that has coded [`LAYERS_AFTER_FRAMES`]; frames
    /// the layered session drops turn them off; a rebuilt session starts without them and waits
    /// its own frames.
    #[test]
    fn layers_wait_for_a_settled_session_and_follow_the_gate() {
        let wire = Wire::new();
        let sink: Arc<dyn DatagramSink> = Arc::<Wire>::clone(&wire);
        let shared = Arc::new(Shared::<Recording>::new(StreamId(1), sink, 8_000_000, 60, false));
        let at = std::cell::Cell::new(host_now_us());
        let first = recorder(1, Log::default());
        let (told, dropped) = (Arc::clone(&first.layers), Arc::clone(&first.dropped));
        drop(shared.install(whole(first), [1, 0]));
        shared.follow_layers(true);
        shared.follow_layers(true);
        assert!(shared.layers_wanted.load(Ordering::Relaxed), "a lossy link");
        for _ in 0..LAYERS_AFTER_FRAMES {
            assert_eq!(encode_held(&shared, a_frame(), &at), Attempt::Sent);
        }
        assert!(told.lock().is_empty(), "not in the session's first frames");
        assert_eq!(encode_held(&shared, a_frame(), &at), Attempt::Sent);
        assert_eq!(*told.lock(), vec![true], "on once it has settled");

        dropped.store(3, Ordering::Relaxed);
        assert_eq!(encode_held(&shared, a_frame(), &at), Attempt::Sent);
        shared.follow_layers(true);
        assert!(!shared.layers_wanted.load(Ordering::Relaxed), "a layered session dropped frames");
        assert_eq!(encode_held(&shared, a_frame(), &at), Attempt::Sent);
        assert_eq!(*told.lock(), vec![true, false]);

        let mut gate = LayerGate::default();
        gate.update(true, 0);
        gate.update(true, 0);
        *shared.layers.lock() = gate;
        shared.layers_wanted.store(true, Ordering::Relaxed);
        let second = recorder(2, Log::default());
        let told = Arc::clone(&second.layers);
        drop(shared.install(whole(second), [2, 0]));
        for _ in 0..LAYERS_AFTER_FRAMES {
            assert_eq!(encode_held(&shared, a_frame(), &at), Attempt::Sent);
        }
        assert!(told.lock().is_empty(), "a rebuilt session waits its own frames");
        assert_eq!(encode_held(&shared, a_frame(), &at), Attempt::Sent);
        assert_eq!(*told.lock(), vec![true]);
        assert_eq!(shared.encoder_dropped.load(Ordering::Relaxed), 3, "the old session's drops");
    }

    /// A rebuild swaps the encoder and resets what described the old session in one step. An
    /// encode caught between the two reached the new session with the old one's requests (no
    /// keyframe), and the rebuild's keyframe then went out as a second IDR. The next encode now
    /// puts the new session in and resets the book under its own lock, before it reads a
    /// request: an acknowledgement queued for the old session after the rebuild never reaches
    /// the new one, and the new session starts on one keyframe.
    #[test]
    fn a_rebuild_is_one_step_for_the_next_encode() {
        let wire = Wire::new();
        let sink: Arc<dyn DatagramSink> = Arc::<Wire>::clone(&wire);
        let shared = Arc::new(Shared::<Recording>::new(StreamId(1), sink, 8_000_000, 60, false));
        let log = Log::default();
        let at = std::cell::Cell::new(host_now_us());
        drop(shared.install(whole(recorder(1, Arc::clone(&log))), [1, 0]));
        assert_eq!(encode_held(&shared, a_frame(), &at), Attempt::Sent);
        shared.on_session_packet(0, 1, &packet(40_000, true, None, false));
        shared.on_session_packet(0, 1, &packet(900, false, Some(7), false));

        drop(shared.install(whole(recorder(2, Arc::clone(&log))), [2, 0]));
        let _decision = shared.report(0, &acking(&[7]), None);
        assert_eq!(shared.top.pending.lock().acked, vec![7], "the old session is still in force");
        assert_eq!(encode_held(&shared, a_frame(), &at), Attempt::Sent);
        assert_eq!(*log.lock(), vec![(1, true), (2, true)], "the new session's keyframe, once");
        let pending = shared.top.pending.lock();
        let clean = pending.acked.is_empty() && !pending.keyframe;
        drop(pending);
        assert!(clean, "nothing of the old session left");
    }

    /// A platform whose next encoder session takes as long as the test says: its build takes the
    /// receiver in [`SLOW_BUILDS`] and waits for one message on it.
    enum Slow {}

    impl Platform for Slow {
        type Audio = slopty_codec::Opus;
        type Capture = slopty_capture::ScreenCaptureKit;
        type Input = slopty_input::CgEvents;
        type Video = SlowBuild;
    }

    static SLOW_BUILDS: Mutex<Option<std::sync::mpsc::Receiver<()>>> = Mutex::new(None);

    struct SlowBuild;

    impl slopty_codec::VideoEncoder for SlowBuild {
        type Image = slopty_codec::PixelBuffer;

        fn new(
            _config: EncoderConfig,
            _sink: impl Fn(EncodedPacket) + Send + Sync + 'static,
        ) -> Result<Self, CodecError> {
            let release = SLOW_BUILDS.lock().take();
            let _released = release.as_ref().map(std::sync::mpsc::Receiver::recv);
            Ok(Self)
        }

        fn encode(&self, _: &Self::Image, _: u64, _: &FrameOptions) -> Result<(), CodecError> {
            Ok(())
        }

        fn set_bitrate(&self, _bps: u32) -> Result<(), CodecError> {
            Ok(())
        }

        fn set_frame_rate(&self, _fps: u16) -> Result<(), CodecError> {
            Ok(())
        }
    }

    /// A frame the replaced session was still encoding comes out after the new session is in,
    /// behind its keyframe: it is dropped, not sent to be decoded against the new session, and
    /// its token is not filed in the new session's book. The session in force is sent.
    #[test]
    fn a_replaced_sessions_late_frame_is_not_sent() {
        let wire = Wire::new();
        let sink: Arc<dyn DatagramSink> = Arc::<Wire>::clone(&wire);
        let shared = Arc::new(Shared::<Recording>::new(StreamId(1), sink, 8_000_000, 60, false));
        let at = std::cell::Cell::new(host_now_us());
        let old = shared.next_session();
        drop(shared.install(whole(recorder(old, Log::default())), [old, 0]));
        assert_eq!(encode_held(&shared, a_frame(), &at), Attempt::Sent, "puts it in");
        shared.on_session_packet(0, old, &packet(900, true, Some(0), false));
        assert!(!wire.drain().is_empty(), "the session in force is the stream's");

        let new = shared.next_session();
        assert_ne!(new, old);
        drop(shared.install(whole(recorder(new, Log::default())), [new, 0]));
        assert_eq!(encode_held(&shared, a_frame(), &at), Attempt::Sent, "puts it in");
        shared.on_session_packet(0, new, &packet(900, true, Some(0), false));
        let keyframe = wire.drain();
        assert_ne!(keyframe, Vec::<Bytes>::new());
        shared.on_session_packet(0, old, &packet(900, false, Some(31), false));
        assert!(wire.drain().is_empty(), "the old session's frame after the new keyframe");
        assert_eq!(shared.stats().encoded, 2);
        shared.report(0, &acking(&[31]), None);
        assert!(shared.top.pending.lock().acked.is_empty(), "31 was never offered by this session");
    }

    /// A refresh with no acknowledged reference goes to the encoder as a keyframe, not as
    /// `ForceLTRRefresh`, after which VideoToolbox's next frames do not decode; with one it goes
    /// as the LTR refresh, a delta. Either way the request is taken.
    #[test]
    fn a_refresh_with_nothing_acknowledged_goes_out_as_a_keyframe() {
        let wire = Wire::new();
        let sink: Arc<dyn DatagramSink> = Arc::<Wire>::clone(&wire);
        let shared = Arc::new(Shared::<Recording>::new(StreamId(1), sink, 8_000_000, 60, false));
        let log = Log::default();
        drop(shared.install(whole(recorder(1, Arc::clone(&log))), [1, 0]));
        let at = std::cell::Cell::new(host_now_us());
        // A second apart, so the cadence never holds one back.
        let encode = |shared: &Shared<Recording>| {
            *shared.held.lock() = Some(a_frame());
            shared.owed.store(true, Ordering::Relaxed);
            at.set(at.get() + 1_000_000);
            assert_eq!(shared.try_encode(None, at.get()), Attempt::Sent);
            let (keyframe, refresh) = {
                let pending = shared.top.pending.lock();
                (pending.keyframe, pending.refresh)
            };
            assert!(!keyframe && !refresh, "the request was taken");
        };
        encode(&shared);
        shared.on_packet(0, &packet(40_000, true, Some(0), false));

        shared.top.pending.lock().refresh = true;
        assert!(!shared.top.ltr_usable());
        encode(&shared);
        assert_eq!(log.lock().last(), Some(&(1, true)), "nothing acknowledged: a keyframe");
        assert_eq!(shared.stats().ltr.refreshes_idr, 1);
        shared.on_packet(0, &packet(40_000, true, None, false));

        shared.on_packet(0, &packet(900, false, Some(3), false));
        shared.report(0, &acking(&[3]), None);
        shared.top.pending.lock().refresh = true;
        encode(&shared);
        assert_eq!(log.lock().last(), Some(&(1, false)), "a reference: the LTR refresh");
    }

    /// A refresh asked for while a keyframe is being encoded is answered by it: the encoder is
    /// not asked for a second behind it. One asked for once it came out is answered again, and so
    /// is one asked for when it is taken as dropped.
    #[test]
    fn a_keyframe_being_encoded_answers_a_refresh() {
        let wire = Wire::new();
        let sink: Arc<dyn DatagramSink> = Arc::<Wire>::clone(&wire);
        let shared = Arc::new(Shared::<Recording>::new(StreamId(1), sink, 8_000_000, 60, false));
        let log = Log::default();
        drop(shared.install(whole(recorder(1, Arc::clone(&log))), [1, 0]));
        assert_eq!(shared.try_encode(Some((a_frame(), None)), host_now_us()), Attempt::Sent);
        assert_eq!(*log.lock(), vec![(1, true)], "the session's keyframe went in");

        shared.request_refresh(0, 0, true);
        shared.request_refresh(0, 0, false);
        let asked = |shared: &Shared<Recording>| {
            let pending = shared.top.pending.lock();
            (pending.keyframe, pending.refresh)
        };
        assert_eq!(asked(&shared), (false, false), "the keyframe in flight answers both");
        assert_eq!(shared.stats().refreshes, 2, "still counted");

        shared.on_packet(0, &packet(40_000, true, None, false));
        shared.request_refresh(0, 0, true);
        assert_eq!(asked(&shared), (true, false), "asked for after it came out: answered");

        shared.top.pending.lock().keyframe = false;
        let long_ago = host_now_us().saturating_sub(KEYFRAME_IN_FLIGHT_US);
        shared.top.keyframe_submitted_us.store(long_ago, Ordering::Relaxed);
        shared.request_refresh(0, 0, false);
        assert_eq!(asked(&shared), (false, true), "one that never came out answers nothing");
    }

    /// A session that codes inside the submit hands its keyframe on before the submit returns,
    /// as an aligned VideoToolbox session does: nothing is in flight once it is out, and a
    /// refresh asked for after it is answered.
    #[test]
    fn a_keyframe_coded_inside_the_submit_is_not_left_in_flight() {
        let wire = Wire::new();
        let sink: Arc<dyn DatagramSink> = Arc::<Wire>::clone(&wire);
        let shared = Arc::new(Shared::<Recording>::new(StreamId(1), sink, 8_000_000, 60, false));
        let log = Log::default();
        let weak = Arc::downgrade(&shared);
        let inline: Emit = Arc::new(move |packet| {
            if let Some(shared) = weak.upgrade() {
                shared.on_session_packet(0, 1, &packet);
            }
        });
        let session = Recorder { inline: Some(inline), ..recorder(1, Arc::clone(&log)) };
        drop(shared.install(whole(session), [1, 0]));
        assert_eq!(shared.try_encode(Some((a_frame(), None)), host_now_us()), Attempt::Sent);
        assert_eq!(*log.lock(), vec![(1, true)], "the session's keyframe went in");
        assert!(!wire.sent.lock().is_empty(), "and came out inside the submit");
        assert_eq!(shared.top.keyframe_submitted_us.load(Ordering::Relaxed), 0, "none in flight");

        shared.request_refresh(0, 0, true);
        assert!(shared.top.pending.lock().keyframe, "a keyframe asked for after it: answered");
    }

    /// A resize's encoder is built off the stream's task: starting it answers at once however
    /// long VideoToolbox takes, a wait for it that is given up leaves the build running, and the
    /// next wait gets the session. The stream's task relies on all three to keep taking input
    /// while it waits (`apps/slopty-worker/src/screens.rs`).
    #[tokio::test]
    async fn a_rebuild_starts_at_once_and_survives_a_dropped_wait() {
        let (release, builds) = std::sync::mpsc::channel();
        *SLOW_BUILDS.lock() = Some(builds);
        let sink: Arc<dyn DatagramSink> = Wire::new();
        let shared = Arc::new(Shared::<Slow>::new(StreamId(1), sink, 8_000_000, 60, false));
        let (capture, config) = configs((1280, 800), &Quality::default(), None);
        let started = Instant::now();
        let encoder = start_encoder(&Arc::downgrade(&shared), config, (1280, 800), [1, 0], None);
        let mut rebuild = Rebuild::<Slow> {
            encoder,
            sessions: [1, 0],
            native: (1280, 800),
            desired: capture,
            config,
            layout: None,
            resized: true,
        };
        assert!(started.elapsed() < Duration::from_millis(50), "{:?}", started.elapsed());

        let waited = tokio::time::timeout(Duration::from_millis(100), rebuild.built()).await;
        assert!(waited.is_err(), "built while the session was still being made");
        release.send(()).unwrap();
        let built = tokio::time::timeout(Duration::from_secs(5), rebuild.built()).await.unwrap();
        built.unwrap();
    }

    /// A platform whose encoder sessions are built only once the test lets them
    /// ([`WEDGE_OPEN`]), as a build that never came back from VideoToolbox on a hosted virtual
    /// Mac (CI run 37929905806), and counted as they are dropped.
    enum Wedge {}

    impl Platform for Wedge {
        type Audio = slopty_codec::Opus;
        type Capture = slopty_capture::ScreenCaptureKit;
        type Input = slopty_input::CgEvents;
        type Video = Wedged;
    }

    static WEDGE_OPEN: Mutex<bool> = Mutex::new(false);
    static WEDGE_OPENED: parking_lot::Condvar = parking_lot::Condvar::new();
    static WEDGED_DROPPED: AtomicU32 = AtomicU32::new(0);

    struct Wedged;

    impl Drop for Wedged {
        fn drop(&mut self) {
            WEDGED_DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl slopty_codec::VideoEncoder for Wedged {
        type Image = slopty_codec::PixelBuffer;

        fn new(
            _config: EncoderConfig,
            _sink: impl Fn(EncodedPacket) + Send + Sync + 'static,
        ) -> Result<Self, CodecError> {
            let mut open = WEDGE_OPEN.lock();
            while !*open {
                WEDGE_OPENED.wait(&mut open);
            }
            Ok(Self)
        }

        fn encode(&self, _: &Self::Image, _: u64, _: &FrameOptions) -> Result<(), CodecError> {
            Ok(())
        }

        fn set_bitrate(&self, _bps: u32) -> Result<(), CodecError> {
            Ok(())
        }

        fn set_frame_rate(&self, _fps: u16) -> Result<(), CodecError> {
            Ok(())
        }
    }

    /// A build that stays inside VideoToolbox is given up once its patience is charged, and
    /// answers what it waited on. Nothing waits on the thread left inside: the runtime shuts
    /// down at once, where a build on its blocking pool held the shutdown for as long as
    /// VideoToolbox did, and the sessions a build makes once it comes back, given up on or
    /// dropped unanswered, are dropped on its own thread.
    #[test]
    fn a_build_that_never_comes_back_is_given_up_and_holds_nothing() {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let sink: Arc<dyn DatagramSink> = Wire::new();
        let shared = Arc::new(Shared::<Wedge>::new(StreamId(1), sink, 8_000_000, 60, false));
        let (_capture, config) = configs((1280, 800), &Quality::default(), None);
        let weak = Arc::downgrade(&shared);
        let patience = Duration::from_millis(300);
        let (waited, next) = runtime.block_on(async {
            let mut building = start_encoder(&weak, config, (1280, 800), [1, 0], None);
            assert_eq!(building.patience, BUILD_STUCK);
            building.patience = patience;
            let started = Instant::now();
            let answer = building.built().await;
            let waited = started.elapsed();
            assert!(
                matches!(answer, Err(ScreenError::BuildStuck(given)) if given == patience),
                "{answer:?}"
            );
            let next = start_encoder(&weak, config, (1280, 800), [2, 0], None);
            (waited, next)
        });
        assert!(waited >= patience && waited < patience * 10, "given up after {waited:?}");
        drop(next);

        let shutdown = Instant::now();
        drop(runtime);
        assert!(shutdown.elapsed() < Duration::from_secs(1), "{:?}", shutdown.elapsed());

        *WEDGE_OPEN.lock() = true;
        WEDGE_OPENED.notify_all();
        let deadline = Instant::now() + Duration::from_secs(10);
        while WEDGED_DROPPED.load(Ordering::Relaxed) < 2 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(WEDGED_DROPPED.load(Ordering::Relaxed), 2, "both builds' sessions dropped");
    }

    /// The wait for a build charges a look at most [`STUCK_CREDIT`], however late it came: a
    /// worker held up for seconds by a starved machine has not seen VideoToolbox stop.
    #[test]
    fn a_build_wait_charges_only_the_time_it_ran_on_time() {
        let (_answer_tx, answer) = oneshot::channel();
        let mut building = Building::<Wedge> {
            answer,
            stream: None,
            patience: Duration::from_millis(200),
            charged: Duration::ZERO,
            looked_us: None,
        };
        assert!(!building.look(1_000_000), "the first look charges nothing");
        assert!(!building.look(1_025_000));
        assert_eq!(building.charged, Duration::from_millis(25));
        assert!(!building.look(6_025_000), "five seconds late");
        assert_eq!(building.charged, Duration::from_millis(125), "charged the credit");
        assert!(!building.look(6_050_000));
        assert!(!building.look(6_075_000));
        assert!(building.look(6_100_000), "200 ms charged: given up");
    }

    /// The window server's cursor as [`the_shape_loop_reads_the_picture_only_when_the_seed_moves`]
    /// fakes it: the seed, and how many times the seed and the picture were read.
    static FAKE_SEED: AtomicU64 = AtomicU64::new(1);
    static SEEDS_READ: AtomicU64 = AtomicU64::new(0);
    static SHAPES_READ: AtomicU64 = AtomicU64::new(0);

    fn fake_seed() -> Option<i32> {
        SEEDS_READ.fetch_add(1, Ordering::Relaxed);
        i32::try_from(FAKE_SEED.load(Ordering::Relaxed)).ok()
    }

    #[expect(clippy::unnecessary_wraps, reason = "the signature of the read it stands for")]
    fn fake_shape() -> Option<CursorShape> {
        SHAPES_READ.fetch_add(1, Ordering::Relaxed);
        let fill = u8::try_from(FAKE_SEED.load(Ordering::Relaxed) % 256).unwrap_or(0);
        Some(CursorShape { w: 1, h: 1, hot_x: 0, hot_y: 0, bgra: vec![fill; 4], scale: 1 })
    }

    /// With the pointer off the target the loop reads nothing, not even the seed. Once the
    /// cursor loop says it came over, it reads the seed every tick (120 a second) and the
    /// picture once, sends it, and reads the picture again only when the seed moves.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_shape_loop_reads_the_picture_only_when_the_seed_moves() {
        let wire = Wire::new();
        let sink: Arc<dyn DatagramSink> = Arc::<Wire>::clone(&wire);
        let shared = Arc::new(Shared::<Recording>::new(StreamId(1), sink, 8_000_000, 60, false));
        let sent = Arc::new(Mutex::new(Vec::new()));
        let on_event: Arc<dyn Fn(StreamEvent) + Send + Sync> = {
            let sent = Arc::clone(&sent);
            Arc::new(move |event| {
                if let StreamEvent::Cursor(shape) = event {
                    sent.lock().push(shape.bgra[0]);
                }
            })
        };
        let task = tokio::spawn({
            let shared = Arc::clone(&shared);
            async move { follow_shape(&shared, &*on_event, fake_seed, fake_shape).await }
        });

        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(SEEDS_READ.load(Ordering::Relaxed), 0, "off the target: nothing read");

        shared.pointer_over.store(true, Ordering::Relaxed);
        shared.shape_wake.notify_one();
        tokio::time::sleep(Duration::from_millis(250)).await;
        let seeds = SEEDS_READ.load(Ordering::Relaxed);
        assert!((10..=40).contains(&seeds), "about 30 seeds in 250 ms at 120 Hz: {seeds}");
        assert_eq!(SHAPES_READ.load(Ordering::Relaxed), 1, "the picture once, for the first seed");
        assert_eq!(*sent.lock(), [1], "and sent once");

        FAKE_SEED.store(2, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(SHAPES_READ.load(Ordering::Relaxed), 2, "read again when the seed moved");
        assert_eq!(*sent.lock(), [1, 2]);

        shared.pointer_over.store(false, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let off = SEEDS_READ.load(Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(SEEDS_READ.load(Ordering::Relaxed), off, "off the target again: no reads");

        wire.closed.store(true, Ordering::Relaxed);
        task.abort();
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

    /// A rebuild beside an encode whose frame moves the rung finishes. An aligned session codes
    /// the frame inside the submit, so the encoder's output callback runs while the encode holds
    /// the session. The callback used to take the session to tell it the new rung, and a rebuild
    /// waiting for the session behind that encode (`parking_lot` lets no reader past a waiting
    /// writer) made the three wait on each other for good: five runs in five before the fix.
    /// Real VideoToolbox sessions. Every frame is back late by the encode's clock, so the twelfth
    /// ends a run and moves the rung, while the last rebuild waits on that frame's encode.
    ///
    /// Every frame is accounted for: it came out, or it was still in a session the rebuild
    /// replaced and was dropped as that session's. On this Mac's encoder the second never
    /// happens, since an aligned session codes inside the submit. The hosted runner's virtual
    /// Mac returns some frames after the submit: one there was still in a replaced session
    /// (by design, [`Shared::install`]) or not yet back when the encodes ended, and a count of
    /// the frames out read 15 of 16 (run 36974916865).
    #[test]
    fn a_rebuild_beside_a_cadence_change_finishes() {
        use synthetic::Synthetic;

        const REBUILDS: usize = 2;
        const FRAMES: usize = 16;
        const RUNG_MOVES_AT: usize = 12;
        let wire = Wire::new();
        let sink: Arc<dyn DatagramSink> = Arc::<Wire>::clone(&wire);
        let shared = Arc::new(Shared::<Synthetic>::new(StreamId(1), sink, 20_000_000, 60, false));
        shared.apply_bitrate(20_000_000);
        let config = EncoderConfig {
            width: 1920,
            height: 1088,
            codec: VideoCodec::Hevc,
            fps: 60,
            bitrate_bps: 20_000_000,
            chroma: Chroma::Subsampled,
        };
        let weak = Arc::downgrade(&shared);
        let mut sessions: VecDeque<_> = std::iter::repeat_with(|| {
            let n = shared.next_session();
            (open_session(&weak, config, (1920, 1088), n, 0, None).unwrap(), n)
        })
        .take(REBUILDS + 1)
        .collect();
        let (first, n) = sessions.pop_front().unwrap();
        drop(shared.install(whole(first), [n, 0]));
        let picture = a_frame_of(1920, 1088);

        let (go_tx, go) = std::sync::mpsc::channel::<usize>();
        let (done_tx, done) = std::sync::mpsc::channel::<&str>();
        let _encodes = std::thread::spawn({
            let (shared, done_tx) = (Arc::clone(&shared), done_tx.clone());
            move || {
                let base = host_now_us();
                for i in 1..=FRAMES {
                    let at =
                        base.saturating_add(u64::try_from(i).unwrap().saturating_mul(1_000_000));
                    let frame = CapturedFrame { capture_ts_us: at, ..again(&picture) };
                    let _gone = go_tx.send(i);
                    // Handed over a tenth of a second ago as far as the encode's clock goes: every
                    // frame is back late at 60, however quickly this picture codes.
                    let now = host_now_us().saturating_sub(100_000);
                    let attempt = shared.try_encode(Some((frame, None)), now);
                    assert_eq!(attempt, Attempt::Sent, "frame {i}");
                }
                let _gone = done_tx.send("encodes");
            }
        });
        let _rebuilds = std::thread::spawn({
            let shared = Arc::clone(&shared);
            move || {
                for (session, n) in sessions {
                    // The rebuilds line up on the frames up to the one that moves the rung.
                    while go.recv().is_ok_and(|i| i <= RUNG_MOVES_AT - REBUILDS) {}
                    // Into the submit, which takes the session several milliseconds to return from.
                    #[expect(
                        clippy::disallowed_methods,
                        reason = "a test thread lining up a race"
                    )]
                    std::thread::sleep(Duration::from_millis(1));
                    retire(shared.install(whole(session), [n, 0]));
                }
                let _gone = done_tx.send("rebuilds");
            }
        });
        for _ in 0..2 {
            let finished = done.recv_timeout(Duration::from_secs(30));
            assert!(finished.is_ok(), "the rebuilds and the encodes wait on each other");
        }
        // A session that codes after its submit returns may still hold the last frame.
        let accounted = || {
            let out = shared.stats().encoded;
            let replaced = shared.replaced_dropped.load(Ordering::Relaxed);
            (out, replaced, out.saturating_add(replaced) == u64::try_from(FRAMES).unwrap())
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        while !accounted().2 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let (out, replaced, every) = accounted();
        assert!(every, "every frame came out or was a replaced session's: {out} and {replaced}");
        assert_eq!(shared.fps_ceiling.load(Ordering::Relaxed), 30, "the rung moved");
    }

    /// Nothing the stream's owner, its runtime tasks or the capture's queue call waits on an
    /// encode. An aligned session codes the frame inside the submit, 15 ms at 3024 × 1968 and
    /// about 38 ms at 5K, and a runtime worker waiting that long is one not answering input, the
    /// pointer or the link. Here the test holds what an encode holds across its submit: the
    /// stream's turn, inside the encoder.
    #[test]
    fn nothing_but_an_encode_waits_on_one() {
        let wire = Wire::new();
        let sink: Arc<dyn DatagramSink> = Arc::<Wire>::clone(&wire);
        let shared = Arc::new(Shared::<Recording>::new(StreamId(1), sink, 8_000_000, 60, false));
        let at = std::cell::Cell::new(host_now_us());
        drop(shared.install(whole(recorder(1, Log::default())), [1, 0]));
        assert_eq!(encode_held(&shared, a_frame(), &at), Attempt::Sent);

        let turn = shared.gate.take();
        shared.gate.enter(turn);
        let (tx, answered) = std::sync::mpsc::channel();
        let caller = std::thread::spawn({
            let shared = Arc::clone(&shared);
            move || {
                let _at = shared.repair_at();
                retire(shared.install(whole(recorder(2, Log::default())), [2, 0]));
                shared.apply_bitrate(4_000_000);
                shared.apply_cadence(1_000_000);
                let _decision = shared.report(0, &acking(&[]), None);
                shared.request_refresh(0, 0, true);
                shared.post(a_frame());
                let _stats = shared.stats();
                let _gone = tx.send(());
            }
        });
        let answered = answered.recv_timeout(Duration::from_secs(2)).is_ok();
        assert!(shared.gate.back(turn));
        shared.gate.leave(turn);
        caller.join().unwrap();
        assert!(answered, "a call off the encode path waited on the encode");
    }

    /// A capture still at the size before a rebuild never reaches the new session: one waiting
    /// in the mailbox is dropped with the rebuild, one already held is dropped by the encode, not
    /// repaired, and the session's keyframe waits for the first capture of its own size.
    #[test]
    fn a_capture_of_the_old_size_never_reaches_a_rebuilt_session() {
        let wire = Wire::new();
        let sink: Arc<dyn DatagramSink> = Arc::<Wire>::clone(&wire);
        let shared = Arc::new(Shared::<Recording>::new(StreamId(1), sink, 8_000_000, 60, false));
        let log = Log::default();
        shared.post(a_frame());
        let resized = Recorder { size: (32, 32), ..recorder(1, Arc::clone(&log)) };
        drop(shared.install(whole(resized), [1, 0]));
        assert!(shared.mailbox.take(Duration::ZERO).is_none(), "the mailbox is cleared");
        shared.on_frame(a_frame());
        assert!(log.lock().is_empty(), "the old size was coded: {:?}", log.lock());
        assert!(shared.top.pending.lock().keyframe, "the keyframe is still wanted");
        assert_eq!(shared.repair_at(), None, "the old capture is gone, not owed");
        shared.on_frame(a_frame_of(32, 32));
        assert_eq!(*log.lock(), vec![(1, true)], "the first capture of the new size");
    }

    /// A measurement's padding is its own stream's: the stream beside it keeps 16. It was a
    /// process-wide knob, which every test running beside the measurement read.
    #[test]
    fn a_streams_padding_is_its_own() {
        let quality = Quality::default();
        let (_capture, measured) = configs_padded((3024, 1964), &quality, Some(60.0), Some(2), 2.0);
        let (_capture, beside) = configs((3024, 1964), &quality, Some(60.0));
        assert_eq!((measured.width, measured.height), (3024, 1964), "the even size asked for");
        assert_eq!((beside.width, beside.height), (3024, 1968), "the codec's own 16");
    }

    /// Two captures a beat apart reach the mailbox while the encoder is busy with the one before,
    /// on a 120 rung: the first is replaced every time, and it was due. Half the rung is what the
    /// encoder was fed, and the rung comes down to it, as the gate, the guard and the encoder's
    /// own rate control budget a frame from it.
    #[test]
    fn a_rung_the_encoder_cannot_feed_comes_down_to_what_it_was_fed() {
        let wire = Wire::new();
        let sink: Arc<dyn DatagramSink> = Arc::<Wire>::clone(&wire);
        let shared = Arc::new(Shared::<Recording>::new(StreamId(1), sink, 30_000_000, 120, false));
        shared.apply_bitrate(30_000_000);
        drop(shared.install(whole(recorder(1, Log::default())), [1, 0]));
        let base = host_now_us();
        let beat = |k: u64| base.saturating_add(k.saturating_mul(8_333));
        let mut pairs = 0_u64;
        while shared.fps_ceiling.load(Ordering::Relaxed) == 120 && pairs < 40 {
            shared.post(CapturedFrame { capture_ts_us: beat(2 * pairs), ..a_frame() });
            shared.post(CapturedFrame { capture_ts_us: beat(2 * pairs + 1), ..a_frame() });
            let frame = shared.mailbox.take(Duration::ZERO).expect("the newer capture");
            shared.on_frame(frame);
            pairs = pairs.saturating_add(1);
        }
        assert_eq!(
            shared.counters.superseded.load(Ordering::Relaxed),
            pairs,
            "one replaced a pair"
        );
        assert_eq!(shared.fps_ceiling.load(Ordering::Relaxed), 60, "fed half of 120");
        assert_eq!(shared.fps.load(Ordering::Relaxed), 60);
    }

    /// A due capture replaced by one a millisecond newer, in the same slot of a 60 rung, costs
    /// the rung nothing: the newer capture fills the slot. Counted as lost, it took a 66 rung on
    /// 120 Hz captures down to 36 and then 32 (MEASUREMENTS.md, "the encoder watch behind the
    /// mailbox").
    #[test]
    fn a_capture_replaced_within_its_slot_costs_the_rung_nothing() {
        let wire = Wire::new();
        let sink: Arc<dyn DatagramSink> = Arc::<Wire>::clone(&wire);
        let shared = Arc::new(Shared::<Recording>::new(StreamId(1), sink, 30_000_000, 60, false));
        shared.apply_bitrate(30_000_000);
        drop(shared.install(whole(recorder(1, Log::default())), [1, 0]));
        let base = host_now_us();
        let slot = |k: u64| base.saturating_add(k.saturating_mul(16_667));
        for i in 0..40_u64 {
            shared.post(CapturedFrame { capture_ts_us: slot(i), ..a_frame() });
            shared
                .post(CapturedFrame { capture_ts_us: slot(i).saturating_add(1_000), ..a_frame() });
            let frame = shared.mailbox.take(Duration::ZERO).expect("the newer capture");
            shared.on_frame(frame);
        }
        assert_eq!(shared.counters.superseded.load(Ordering::Relaxed), 40, "one replaced a pair");
        assert_eq!(shared.fps_ceiling.load(Ordering::Relaxed), 60, "the rung kept");
        assert_eq!(shared.fps.load(Ordering::Relaxed), 60);
        assert_eq!(shared.fed_fps.load(Ordering::Relaxed), 60, "fed every slot");
    }

    /// A ceiling a slow stretch took down rises again once the encoder codes well over it, and
    /// no further than the client asked: 15.2 ms frames are 65 a second.
    #[test]
    fn a_ceiling_the_encoder_outgrew_rises_back() {
        let wire = Wire::new();
        let sink: Arc<dyn DatagramSink> = Arc::<Wire>::clone(&wire);
        let shared = Arc::new(Shared::<Recording>::new(StreamId(1), sink, 30_000_000, 120, false));
        shared.apply_bitrate(30_000_000);
        shared.lower_ceiling(55, "a slow stretch");
        assert_eq!(shared.fps.load(Ordering::Relaxed), 55);
        let window = |encode_us| {
            let fps = shared.fps.load(Ordering::Relaxed);
            // One window of the watch: thirty due captures.
            for _ in 0..30 {
                shared.watch_encoder(encode_us, false);
                shared.fed(fps);
            }
        };
        window(15_200);
        assert_eq!(shared.fps_ceiling.load(Ordering::Relaxed), 65, "what the encoder codes");
        assert_eq!(shared.fps.load(Ordering::Relaxed), 65);
        window(4_000);
        assert_eq!(shared.fps_ceiling.load(Ordering::Relaxed), 120, "what the client asked");
    }

    /// The guard budgets a frame at the rate the encoder is fed when that is under the rung:
    /// at 8 Mbit/s a 120 rung allows two frames of 8.3 KB, and an encoder fed 60 makes frames
    /// of 16.7 KB, one of which already filled that.
    #[test]
    fn the_guard_budgets_a_frame_at_the_rate_the_encoder_is_fed() {
        let (shared, wire) = shared_for_frames();
        shared.counters.bitrate_bps.store(8_000_000, Ordering::Relaxed);
        shared.fps.store(120, Ordering::Relaxed);
        wire.held.store(17_000, Ordering::Relaxed);
        assert!(
            !shared.frame_fits(),
            "two frames at 120 fps are 16.7 KB, less than the 17 KB held"
        );
        shared.fed_fps.store(60, Ordering::Relaxed);
        assert!(shared.frame_fits(), "two frames at the fed 60 fps are 33.3 KB");
    }
}
