//! Client side: put fragments back together, recover, deliver in decode order, and decide when
//! to NACK and when to give up and ask for a refresh.
//!
//! State machine per stream:
//!
//! * **Need keyframe** (start): only an IDR starts delivery.
//! * **Need frame *n***: frames leave in order. A frame with missing fragments gets a NACK after a
//!   short silence, retried at most [`Config::nack_retries`] times, and declared lost after the
//!   deadline those retries imply — but only while the link is *flowing*: a retry needs something
//!   to have arrived since the last NACK, and the deadline only counts when newer datagrams are
//!   still coming in. A path that stalls outright (Wi-Fi delay bursts of 100–300 ms hold every
//!   packet and then release them all) is waited out up to [`Config::max_hold`], because a refresh
//!   could not get through it either and everything usually arrives once it clears. When it clears,
//!   every pending frame's NACK clock restarts: the NACK sent into the stall only left with the
//!   release, so its answer is a round trip away from *now*, not from when it was written.
//! * **Need refresh** (after a loss): the host is asked for a refresh from the last good frame; the
//!   next IDR or LTR-refresh frame restarts delivery and everything older is dropped.
//!
//! Frames whose fragments all arrive are delivered without any parity work; parity is only
//! decoded when data fragments are missing and enough parity arrived.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use bytes::Bytes;
use reed_solomon_simd::ReedSolomonDecoder;
use slopty_core::StreamId;
use slopty_proto::media::{
    CursorUpdate, FramePrefix, HEADER_BYTES, Kind, MAX_PAYLOAD, MediaHeader, flags,
};
use slopty_proto::screen::ReceiverReport;

use crate::cursor::parse_cursor;

/// Shortest silence on a stream that counts as a stall (see [`Config::stall_gap`]); the host
/// heartbeats at half this while its source is quiet.
pub const STALL_GAP: Duration = Duration::from_millis(50);

/// Timing and bounds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Config {
    /// Silence after the last fragment of an incomplete frame before a NACK goes out. Fragments of
    /// one frame leave the host back to back, so a few milliseconds of silence means loss.
    pub nack_delay: Duration,
    /// NACK attempts per frame before it is given up.
    pub nack_retries: u8,
    /// Slack added to the loss deadline on top of the NACK round trips.
    pub grace: Duration,
    /// Most frames tracked at once; the oldest is dropped beyond this.
    pub max_pending: usize,
    /// How often a refresh request is repeated while no refresh frame arrives (plus two RTTs).
    /// Each unanswered repeat doubles the wait, up to [`Self::refresh_repeat_max`], so a target
    /// that produces no frames at all (a hidden window) is not asked eight times a second.
    pub refresh_repeat: Duration,
    /// Longest wait between refresh repeats.
    pub refresh_repeat_max: Duration,
    /// Longest an incomplete frame is held while nothing at all arrives on the stream (a
    /// stalled link); past this it is lost even though the deadline logic would keep waiting.
    pub max_hold: Duration,
    /// Shortest silence on the stream that counts as a stall when it ends (the effective gap is
    /// the larger of this and one NACK round trip). Must exceed a normal inter-frame gap, or
    /// every frame would restart the pending deadlines.
    pub stall_gap: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            nack_delay: Duration::from_millis(3),
            nack_retries: 2,
            grace: Duration::from_millis(10),
            max_pending: 64,
            refresh_repeat: Duration::from_millis(100),
            refresh_repeat_max: Duration::from_secs(2),
            max_hold: Duration::from_millis(500),
            stall_gap: STALL_GAP,
        }
    }
}

/// What a delivered frame is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FrameInfo {
    /// Frame number.
    pub frame: u32,
    /// IDR frame.
    pub keyframe: bool,
    /// LTR token to acknowledge once decoded.
    pub ltr_token: Option<u64>,
    /// Recovery frame from an acknowledged LTR.
    pub ltr_refresh: bool,
    /// Host capture timestamp, microseconds (low 32 bits).
    pub capture_ts_us: u32,
    /// Needed parity or a retransmission.
    pub recovered: bool,
}

/// A complete frame in decode order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FrameOut {
    /// Metadata.
    pub info: FrameInfo,
    /// The bitstream.
    pub data: Vec<u8>,
    /// Time from the first fragment's arrival to delivery.
    pub hold: Duration,
}

/// Why a datagram was dropped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ignored {
    /// Header or payload did not make sense.
    Malformed,
    /// Another stream.
    Foreign,
    /// Older than what has been delivered or skipped.
    Stale,
    /// Already had it.
    Duplicate,
}

/// Result of feeding one datagram.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Ingest {
    /// A video fragment was stored; call [`Reassembler::next_frame`] for anything now ready.
    Video,
    /// An audio packet.
    Audio {
        /// Packet sequence.
        seq: u32,
        /// Opus bytes.
        payload: Bytes,
    },
    /// A cursor move.
    Cursor {
        /// Update sequence; keep the highest.
        seq: u32,
        /// Position.
        update: CursorUpdate,
    },
    /// The host had nothing to send: the link moved, the stall clock restarted, nothing else.
    Heartbeat,
    /// Dropped.
    Ignored(Ignored),
}

/// Something to tell the host.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    /// Retransmit fragments (`fragments` empty: the whole frame).
    Nack {
        /// Frame.
        frame: u32,
        /// Missing data fragment indices.
        fragments: Vec<u16>,
    },
    /// A frame is lost for good; refresh from an acknowledged LTR (or send an IDR).
    RequestRefresh {
        /// Highest frame delivered.
        last_good_frame: u32,
    },
}

/// Lifetime counters.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ReassemblerStats {
    /// Frames delivered.
    pub frames_ok: u64,
    /// Delivered frames that needed parity.
    pub frames_fec: u64,
    /// Delivered frames that needed a retransmission.
    pub frames_retransmit: u64,
    /// Frames given up on.
    pub frames_lost: u64,
    /// Fragments that never arrived (counted when a frame resolves).
    pub datagrams_lost: u64,
    /// NACKs sent.
    pub nacks: u64,
    /// Refresh requests sent.
    pub refreshes: u64,
    /// Stalls that released (silence past the stall gap, then datagrams again).
    pub stalls: u64,
    /// Time spent stalled, in milliseconds (released stalls plus the one in progress, if any).
    pub stalled_ms: u64,
}

struct Partial {
    data_count: usize,
    parity_count: usize,
    shard_bytes: usize,
    flags: u8,
    shards: Vec<Option<Bytes>>,
    have_data: usize,
    have_total: usize,
    first_seen: Instant,
    last_seen: Instant,
    nacks: u8,
    nacked_at: Option<Instant>,
    retransmitted: bool,
}

impl Partial {
    fn new(header: &MediaHeader, shard_bytes: usize, now: Instant) -> Self {
        let data_count = usize::from(header.data_count.get());
        let parity_count = usize::from(header.parity_count);
        Self {
            data_count,
            parity_count,
            shard_bytes,
            flags: header.flags,
            shards: vec![None; data_count.saturating_add(parity_count)],
            have_data: 0,
            have_total: 0,
            first_seen: now,
            last_seen: now,
            nacks: 0,
            nacked_at: None,
            retransmitted: false,
        }
    }

    const fn complete(&self) -> bool {
        self.have_data == self.data_count
            || (self.parity_count > 0 && self.have_total >= self.data_count)
    }

    fn missing_data(&self) -> Vec<u16> {
        self.shards
            .iter()
            .take(self.data_count)
            .enumerate()
            .filter(|(_i, s)| s.is_none())
            .map(|(i, _s)| u16::try_from(i).unwrap_or(u16::MAX))
            .collect()
    }

    /// Data fragments that never arrived. Parity still in flight when the frame completes is
    /// not counted: it may yet arrive, and losing it costs nothing.
    const fn missing_data_count(&self) -> u64 {
        self.data_count.saturating_sub(self.have_data) as u64
    }

    const fn restartable(&self, accept_refresh: bool) -> bool {
        self.flags & flags::KEYFRAME != 0
            || (accept_refresh && self.flags & flags::LTR_REFRESH != 0)
    }
}

enum Slot {
    /// Nothing of this frame arrived yet, but a later frame did.
    Unknown {
        first_seen: Instant,
        nacks: u8,
        nacked_at: Option<Instant>,
    },
    Partial(Partial),
    Complete {
        out: FrameOut,
        first_seen: Instant,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Need {
    Keyframe,
    Refresh,
    Frame(u32),
}

#[derive(Default)]
struct Window {
    frames_ok: u32,
    frames_fec: u32,
    frames_lost: u32,
    datagrams_lost: u32,
    holds_ns: Vec<u64>,
    /// Silence charged to this window (see `Reassembler::charge_stall`).
    stalled: Duration,
    /// Stalls that released in this window.
    stalls: u16,
}

/// Reassembles one stream's video datagrams and drives its loss policy.
pub struct Reassembler {
    stream: StreamId,
    cfg: Config,
    epoch: Instant,
    frames: BTreeMap<u32, Slot>,
    need: Need,
    last_good: Option<u32>,
    ready: VecDeque<FrameOut>,
    decoder: Option<ReedSolomonDecoder>,
    actions: Vec<Action>,
    refresh_requested_at: Option<Instant>,
    /// Unanswered refresh repeats in a row (backoff exponent).
    refresh_repeats: u32,
    stats: ReassemblerStats,
    window: Window,
    jitter_us: i64,
    last_arrival: Option<(u64, u32)>,
    /// When the last datagram of this stream (of any kind) came in.
    arrived_at: Instant,
    /// Silence before this instant has already been charged to a report window.
    stall_charged_to: Instant,
    /// A video datagram of this stream has arrived at least once; before that, silence is the
    /// stream starting up (the cursor arrives before the first frame), not a stall.
    any_arrived: bool,
    /// Round trip the last `tick` was given; sizes the gap that counts as a stall.
    last_rtt: Duration,
    last_host_ts_us: u32,
    ltr_acks: VecDeque<u64>,
}

impl std::fmt::Debug for Reassembler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reassembler")
            .field("stream", &self.stream)
            .field("need", &self.need)
            .field("pending", &self.frames.len())
            .field("ready", &self.ready.len())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl Reassembler {
    /// A reassembler for one stream. `now` anchors relative timestamps.
    #[must_use]
    pub fn new(stream: StreamId, cfg: Config, now: Instant) -> Self {
        Self {
            stream,
            cfg,
            epoch: now,
            frames: BTreeMap::new(),
            need: Need::Keyframe,
            last_good: None,
            ready: VecDeque::new(),
            decoder: None,
            actions: Vec::new(),
            refresh_requested_at: Some(now),
            refresh_repeats: 0,
            stats: ReassemblerStats::default(),
            window: Window::default(),
            jitter_us: 0,
            last_arrival: None,
            arrived_at: now,
            stall_charged_to: now,
            any_arrived: false,
            last_rtt: Duration::from_millis(20),
            last_host_ts_us: 0,
            ltr_acks: VecDeque::new(),
        }
    }

    /// Lifetime counters.
    #[must_use]
    pub const fn stats(&self) -> ReassemblerStats {
        self.stats
    }

    /// True while waiting for an IDR or refresh frame.
    #[must_use]
    pub const fn awaiting_refresh(&self) -> bool {
        !matches!(self.need, Need::Frame(_))
    }

    /// Frames complete and waiting to be taken plus frames still being assembled.
    #[must_use]
    pub fn queue_depth(&self) -> usize {
        self.ready.len().saturating_add(self.frames.len())
    }

    /// The next frame in decode order, if one is ready.
    pub fn next_frame(&mut self) -> Option<FrameOut> {
        self.ready.pop_front()
    }

    /// Record that a frame carrying `token` was decoded; the next report acknowledges it.
    pub fn ack_ltr(&mut self, token: u64) {
        if self.ltr_acks.len() >= 16 {
            self.ltr_acks.pop_front();
        }
        self.ltr_acks.push_back(token);
    }

    /// Feed one datagram.
    pub fn ingest(&mut self, datagram: &Bytes, now: Instant) -> Ingest {
        let Some((header, _payload)) = MediaHeader::parse(datagram) else {
            return Ingest::Ignored(Ignored::Malformed);
        };
        if header.stream.get() != self.stream.0 {
            return Ingest::Ignored(Ignored::Foreign);
        }
        let gap = now.saturating_duration_since(self.arrived_at);
        if self.stalled(now) {
            self.charge_stall(now);
            self.window.stalls = self.window.stalls.saturating_add(1);
            self.stats.stalls = self.stats.stalls.saturating_add(1);
            self.resume(now, gap);
        }
        self.arrived_at = now;
        let header = *header;
        let payload = datagram.slice(HEADER_BYTES..);
        match header.kind() {
            None => Ingest::Ignored(Ignored::Malformed),
            Some(Kind::Audio) => Ingest::Audio { seq: header.frame.get(), payload },
            Some(Kind::Heartbeat) => Ingest::Heartbeat,
            Some(Kind::Cursor) => {
                parse_cursor(&payload).map_or(Ingest::Ignored(Ignored::Malformed), |update| {
                    Ingest::Cursor { seq: header.frame.get(), update }
                })
            }
            Some(Kind::VideoData | Kind::VideoParity) => self.ingest_video(&header, payload, now),
        }
    }

    fn ingest_video(&mut self, header: &MediaHeader, payload: Bytes, now: Instant) -> Ingest {
        self.refresh_repeats = 0;
        self.any_arrived = true;
        let frame = header.frame.get();
        let data_count = usize::from(header.data_count.get());
        let total = data_count.saturating_add(usize::from(header.parity_count));
        let index = usize::from(header.index.get());
        let valid = data_count > 0
            && index < total
            && !payload.is_empty()
            && payload.len().is_multiple_of(2)
            && payload.len() <= MAX_PAYLOAD
            && header.is_parity() == (index >= data_count);
        if !valid {
            return Ingest::Ignored(Ignored::Malformed);
        }
        if let Need::Frame(next) = self.need {
            if frame < next {
                return Ingest::Ignored(Ignored::Stale);
            }
            let gap = usize::try_from(frame.wrapping_sub(next)).unwrap_or(usize::MAX);
            if gap > self.cfg.max_pending {
                // A whole second of frames vanished: no point NACKing each one.
                self.stats.frames_lost = self.stats.frames_lost.saturating_add(gap as u64);
                self.window.frames_lost =
                    self.window.frames_lost.saturating_add(u32::try_from(gap).unwrap_or(u32::MAX));
                self.frames.retain(|f, _| *f >= frame);
                self.enter_refresh(now);
            } else {
                for missing in next..frame {
                    self.frames.entry(missing).or_insert(Slot::Unknown {
                        first_seen: now,
                        nacks: 0,
                        nacked_at: None,
                    });
                }
            }
        }

        let slot = self.frames.entry(frame).or_insert(Slot::Unknown {
            first_seen: now,
            nacks: 0,
            nacked_at: None,
        });
        if let Slot::Unknown { .. } = slot {
            *slot = Slot::Partial(Partial::new(header, payload.len(), now));
        }
        let Slot::Partial(partial) = slot else {
            return Ingest::Ignored(Ignored::Duplicate);
        };
        if partial.data_count != data_count
            || partial.shards.len() != total
            || partial.shard_bytes != payload.len()
        {
            return Ingest::Ignored(Ignored::Malformed);
        }
        let Some(cell) = partial.shards.get_mut(index) else {
            return Ingest::Ignored(Ignored::Malformed);
        };
        if cell.is_some() {
            return Ingest::Ignored(Ignored::Duplicate);
        }
        *cell = Some(payload);
        partial.have_total = partial.have_total.saturating_add(1);
        if index < data_count {
            partial.have_data = partial.have_data.saturating_add(1);
        }
        partial.last_seen = now;
        if header.flags & flags::RETRANSMIT != 0 {
            partial.retransmitted = true;
        }
        if partial.complete() {
            self.finish(frame, now);
        }
        self.drain(now);
        self.evict(now);
        Ingest::Video
    }

    /// Move a complete partial into a `Complete` slot, or count it lost if it is corrupt.
    fn finish(&mut self, frame: u32, now: Instant) {
        let Some(Slot::Partial(partial)) = self.frames.remove(&frame) else {
            return;
        };
        let first_seen = partial.first_seen;
        let missing = partial.missing_data_count();
        match assemble(&mut self.decoder, frame, &partial) {
            Some(out) => {
                if out.info.recovered {
                    self.window.frames_fec = self.window.frames_fec.saturating_add(1);
                    if partial.have_data < partial.data_count {
                        self.stats.frames_fec = self.stats.frames_fec.saturating_add(1);
                    }
                    if partial.retransmitted {
                        self.stats.frames_retransmit =
                            self.stats.frames_retransmit.saturating_add(1);
                    }
                }
                self.count_lost_datagrams(missing);
                self.frames.insert(frame, Slot::Complete { out, first_seen });
            }
            None => self.lose(frame, missing, now),
        }
    }

    fn count_lost_datagrams(&mut self, missing: u64) {
        self.stats.datagrams_lost = self.stats.datagrams_lost.saturating_add(missing);
        self.window.datagrams_lost =
            self.window.datagrams_lost.saturating_add(u32::try_from(missing).unwrap_or(u32::MAX));
    }

    /// Give up on `frame`.
    fn lose(&mut self, frame: u32, missing: u64, now: Instant) {
        self.frames.remove(&frame);
        self.stats.frames_lost = self.stats.frames_lost.saturating_add(1);
        self.window.frames_lost = self.window.frames_lost.saturating_add(1);
        self.count_lost_datagrams(missing);
        match self.need {
            Need::Frame(_) => self.enter_refresh(now),
            Need::Keyframe | Need::Refresh => self.request_refresh(now),
        }
    }

    fn enter_refresh(&mut self, now: Instant) {
        self.need = Need::Refresh;
        self.refresh_requested_at = None;
        self.request_refresh(now);
    }

    fn request_refresh(&mut self, now: Instant) {
        self.refresh_requested_at = Some(now);
        self.stats.refreshes = self.stats.refreshes.saturating_add(1);
        self.actions.push(Action::RequestRefresh { last_good_frame: self.last_good.unwrap_or(0) });
    }

    /// Deliver everything that is in order (or restarts the stream).
    fn drain(&mut self, now: Instant) {
        loop {
            let next = match self.need {
                Need::Frame(n) => match self.frames.get(&n) {
                    Some(Slot::Complete { .. }) => n,
                    _ => break,
                },
                Need::Keyframe | Need::Refresh => {
                    let accept_refresh = matches!(self.need, Need::Refresh);
                    let found = self.frames.iter().find_map(|(f, slot)| match slot {
                        Slot::Complete { out, .. }
                            if out.info.keyframe || (accept_refresh && out.info.ltr_refresh) =>
                        {
                            Some(*f)
                        }
                        _ => None,
                    });
                    let Some(f) = found else { break };
                    self.frames.retain(|k, _| *k >= f);
                    self.refresh_requested_at = None;
                    f
                }
            };
            let Some(Slot::Complete { mut out, first_seen }) = self.frames.remove(&next) else {
                break;
            };
            out.hold = now.saturating_duration_since(first_seen);
            self.record_delivery(&out, now);
            self.ready.push_back(out);
            self.last_good = Some(next);
            self.need = Need::Frame(next.wrapping_add(1));
        }
    }

    fn record_delivery(&mut self, out: &FrameOut, now: Instant) {
        self.stats.frames_ok = self.stats.frames_ok.saturating_add(1);
        self.window.frames_ok = self.window.frames_ok.saturating_add(1);
        self.window.holds_ns.push(u64::try_from(out.hold.as_nanos()).unwrap_or(u64::MAX));
        self.last_host_ts_us = out.info.capture_ts_us;
        let arrival_us = u64::try_from(now.saturating_duration_since(self.epoch).as_micros())
            .unwrap_or(u64::MAX);
        if let Some((prev_arrival, prev_ts)) = self.last_arrival {
            // RFC 3550 interarrival jitter, on the host's capture clock.
            let transit =
                i64::try_from(arrival_us.saturating_sub(prev_arrival)).unwrap_or(i64::MAX);
            let sent = i64::from(out.info.capture_ts_us.wrapping_sub(prev_ts));
            let d = i64::try_from(transit.saturating_sub(sent).unsigned_abs()).unwrap_or(i64::MAX);
            self.jitter_us = self.jitter_us.saturating_add(d.saturating_sub(self.jitter_us) / 16);
        }
        self.last_arrival = Some((arrival_us, out.info.capture_ts_us));
    }

    /// Drop the oldest frames beyond `max_pending`.
    fn evict(&mut self, now: Instant) {
        while self.frames.len() > self.cfg.max_pending {
            let Some((&frame, slot)) = self.frames.first_key_value() else { break };
            let missing = match slot {
                Slot::Complete { .. } => {
                    self.frames.remove(&frame);
                    continue;
                }
                Slot::Unknown { .. } => 1,
                Slot::Partial(p) => p.missing_data_count(),
            };
            self.lose(frame, missing, now);
        }
    }

    /// Silence on the stream that counts as a stall: one NACK round trip, at least
    /// [`Config::stall_gap`].
    fn stall_threshold(&self) -> Duration {
        self.last_rtt.saturating_add(self.cfg.nack_delay).max(self.cfg.stall_gap)
    }

    /// Whether nothing has arrived for a stall's worth of time as of `now` (never before the
    /// stream's first video datagram: that wait is the host starting the stream).
    #[must_use]
    pub fn stalled(&self, now: Instant) -> bool {
        self.any_arrived && now.saturating_duration_since(self.arrived_at) >= self.stall_threshold()
    }

    /// Charge the silence since the last datagram (the part not yet charged) to the current
    /// report window, so a stall is reported whether it released or is still on.
    fn charge_stall(&mut self, now: Instant) {
        let from = self.arrived_at.max(self.stall_charged_to);
        let silence = now.saturating_duration_since(from);
        self.window.stalled = self.window.stalled.saturating_add(silence);
        self.stats.stalled_ms =
            self.stats.stalled_ms.saturating_add(u64::try_from(silence.as_millis()).unwrap_or(0));
        self.stall_charged_to = now;
    }

    /// The link moved again after a `gap` with nothing on it: restart every pending frame's
    /// NACK clock, so the answer to a NACK that was stuck in the stall gets its round trip.
    fn resume(&mut self, now: Instant, gap: Duration) {
        if self.frames.values().all(|slot| matches!(slot, Slot::Complete { .. })) {
            return;
        }
        tracing::debug!(?gap, pending = self.frames.len(), "link resumed; deadlines restart");
        let fresh = now.checked_sub(self.cfg.nack_delay).unwrap_or(now);
        for slot in self.frames.values_mut() {
            match slot {
                Slot::Complete { .. } => {}
                Slot::Unknown { first_seen, nacks, nacked_at } => {
                    *first_seen = fresh;
                    *nacked_at = (*nacks > 0).then_some(now);
                }
                Slot::Partial(p) => {
                    p.first_seen = fresh;
                    p.nacked_at = (p.nacks > 0).then_some(now);
                }
            }
        }
    }

    /// Time-driven policy: NACKs, loss deadlines, refresh repeats. Call every few milliseconds
    /// and after ingesting a batch. `rtt` is the current round-trip estimate.
    pub fn tick(&mut self, now: Instant, rtt: Duration) -> Vec<Action> {
        self.last_rtt = rtt;
        let retry_gap = rtt.saturating_add(self.cfg.nack_delay);
        let deadline = self
            .cfg
            .nack_delay
            .saturating_add(retry_gap.saturating_mul(u32::from(self.cfg.nack_retries)))
            .saturating_add(self.cfg.grace);
        let in_order = matches!(self.need, Need::Frame(_));
        let accept_refresh = matches!(self.need, Need::Refresh);
        let retries = self.cfg.nack_retries;
        let nack_delay = self.cfg.nack_delay;
        let max_hold = self.cfg.max_hold;
        let arrived_at = self.arrived_at;
        // Newer datagrams still coming in: a fragment that is missing was dropped, not delayed.
        let flowing = now.saturating_duration_since(arrived_at) < deadline;
        let retry_due = |nacked_at: Option<Instant>| {
            nacked_at
                .is_none_or(|t| now.saturating_duration_since(t) >= retry_gap && arrived_at > t)
        };

        let mut lost: Vec<(u32, u64)> = Vec::new();
        let mut nacks: Vec<Action> = Vec::new();
        for (&frame, slot) in &mut self.frames {
            match slot {
                Slot::Complete { .. } => {}
                Slot::Unknown { first_seen, nacks: tries, nacked_at } => {
                    let age = now.saturating_duration_since(*first_seen);
                    if age >= max_hold || (age >= deadline && flowing) {
                        tracing::debug!(
                            frame,
                            ?age,
                            tries,
                            ?rtt,
                            flowing,
                            "frame lost: nothing arrived"
                        );
                        lost.push((frame, 1));
                    } else if in_order
                        && *tries < retries
                        && age >= nack_delay
                        && retry_due(*nacked_at)
                    {
                        tracing::debug!(frame, ?age, try_ = *tries, "nack whole frame");
                        nacks.push(Action::Nack { frame, fragments: Vec::new() });
                        *tries = tries.saturating_add(1);
                        *nacked_at = Some(now);
                    }
                }
                Slot::Partial(p) => {
                    let age = now.saturating_duration_since(p.first_seen);
                    if age >= max_hold || (age >= deadline && flowing) {
                        tracing::debug!(
                            frame,
                            ?age,
                            tries = p.nacks,
                            missing = p.missing_data_count(),
                            of = p.data_count,
                            parity = p.parity_count,
                            retransmitted = p.retransmitted,
                            ?rtt,
                            flowing,
                            "frame lost"
                        );
                        lost.push((frame, p.missing_data_count()));
                    } else if (in_order || p.restartable(accept_refresh))
                        && p.nacks < retries
                        && now.saturating_duration_since(p.last_seen) >= nack_delay
                        && retry_due(p.nacked_at)
                    {
                        let fragments = p.missing_data();
                        tracing::debug!(
                            frame,
                            ?age,
                            try_ = p.nacks,
                            missing = fragments.len(),
                            of = p.data_count,
                            "nack"
                        );
                        nacks.push(Action::Nack { frame, fragments });
                        p.nacks = p.nacks.saturating_add(1);
                        p.nacked_at = Some(now);
                    }
                }
            }
        }
        self.stats.nacks = self.stats.nacks.saturating_add(nacks.len() as u64);
        self.actions.append(&mut nacks);
        for (frame, missing) in lost {
            self.lose(frame, missing, now);
        }
        if !in_order {
            let backoff = self
                .cfg
                .refresh_repeat
                .saturating_mul(1_u32 << self.refresh_repeats.min(16))
                .min(self.cfg.refresh_repeat_max);
            let repeat = backoff.saturating_add(rtt.saturating_mul(2));
            if self.refresh_requested_at.is_none_or(|t| now.saturating_duration_since(t) >= repeat)
            {
                self.refresh_repeats = self.refresh_repeats.saturating_add(1);
                self.request_refresh(now);
            }
        }
        self.drain(now);
        std::mem::take(&mut self.actions)
    }

    /// Build the periodic receiver report and reset the window counters. `late_frames` comes
    /// from the presenter, which alone knows about vsync; `now` charges a stall still in
    /// progress to this window.
    pub fn take_report(&mut self, now: Instant, late_frames: u32) -> ReceiverReport {
        if self.stalled(now) {
            self.charge_stall(now);
        }
        let window = std::mem::take(&mut self.window);
        let mut holds = window.holds_ns;
        holds.sort_unstable();
        let percentile = |p: usize| -> slopty_core::Duration {
            let index = holds.len().saturating_mul(p) / 100;
            let index = index.min(holds.len().saturating_sub(1));
            slopty_core::Duration::from_nanos(holds.get(index).copied().unwrap_or(0))
        };
        let mut acked_ltr = [0_u64; 4];
        let mut acked_ltr_len: u8 = 0;
        for slot in &mut acked_ltr {
            let Some(token) = self.ltr_acks.pop_front() else { break };
            *slot = token;
            acked_ltr_len = acked_ltr_len.saturating_add(1);
        }
        ReceiverReport {
            frames_ok: window.frames_ok,
            frames_fec: window.frames_fec,
            frames_lost: window.frames_lost,
            datagrams_lost: window.datagrams_lost,
            last_host_send_ts_us: self.last_host_ts_us,
            hold_p50: percentile(50),
            hold_p95: percentile(95),
            owd_jitter: slopty_core::Duration::from_micros(
                u64::try_from(self.jitter_us).unwrap_or(0),
            ),
            queue_depth: u8::try_from(self.queue_depth()).unwrap_or(u8::MAX),
            late_frames,
            acked_ltr,
            acked_ltr_len,
            stalled_ms: u16::try_from(window.stalled.as_millis()).unwrap_or(u16::MAX),
            stalls: window.stalls,
        }
    }
}

/// Concatenate (recovering with parity when needed) and strip the prefix. `None` when the
/// prefix is corrupt or the engine refuses the shards.
fn assemble(
    decoder: &mut Option<ReedSolomonDecoder>,
    frame: u32,
    partial: &Partial,
) -> Option<FrameOut> {
    let data_count = partial.data_count;
    let mut body = Vec::with_capacity(data_count.saturating_mul(partial.shard_bytes));
    let fec = partial.have_data < data_count;
    if fec {
        let dec = match decoder.as_mut() {
            Some(dec) => {
                dec.reset(data_count, partial.parity_count, partial.shard_bytes).ok()?;
                dec
            }
            None => decoder.insert(
                ReedSolomonDecoder::new(data_count, partial.parity_count, partial.shard_bytes)
                    .ok()?,
            ),
        };
        for (i, shard) in partial.shards.iter().enumerate() {
            let Some(bytes) = shard else { continue };
            if i < data_count {
                dec.add_original_shard(i, bytes).ok()?;
            } else {
                dec.add_recovery_shard(i.saturating_sub(data_count), bytes).ok()?;
            }
        }
        let restored = dec.decode().ok()?;
        for (i, shard) in partial.shards.iter().take(data_count).enumerate() {
            match shard {
                Some(bytes) => body.extend_from_slice(bytes),
                None => body.extend_from_slice(restored.restored_original(i)?),
            }
        }
    } else {
        for shard in partial.shards.iter().take(data_count) {
            body.extend_from_slice(shard.as_ref()?);
        }
    }
    let (prefix, rest) = FramePrefix::parse(&body)?;
    let len = usize::try_from(prefix.len.get()).ok()?;
    let data = rest.get(..len)?.to_vec();
    let f = partial.flags;
    Some(FrameOut {
        info: FrameInfo {
            frame,
            keyframe: f & flags::KEYFRAME != 0,
            ltr_token: (f & flags::LTR != 0).then_some(prefix.ltr_token.get()),
            ltr_refresh: f & flags::LTR_REFRESH != 0,
            capture_ts_us: prefix.capture_ts_us.get(),
            recovered: fec || partial.retransmitted,
        },
        data,
        hold: Duration::ZERO,
    })
}
