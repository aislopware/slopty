//! Opus over `AudioToolbox`.
//!
//! The system encoder and decoder behind `AudioConverter`, and an `AudioQueue` output for
//! playback. No third-party codec: Apple ships Opus in the OS and the same calls work on macOS
//! and iOS.
//!
//! Everything is 48 kHz stereo float, interleaved, 20 ms packets (960 frames): the one Opus
//! configuration every decoder accepts, and one packet fits a datagram at any sane bitrate.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::time::{Duration, Instant};

use objc2_audio_toolbox::{
    AudioConverterDispose, AudioConverterFillComplexBuffer, AudioConverterGetProperty,
    AudioConverterNew, AudioConverterRef, AudioConverterSetProperty, AudioQueueAllocateBuffer,
    AudioQueueBufferRef, AudioQueueDispose, AudioQueueEnqueueBuffer, AudioQueueNewOutput,
    AudioQueueRef, AudioQueueStart, kAudioConverterEncodeBitRate,
    kAudioConverterPropertyMaximumOutputPacketSize,
};
use objc2_core_audio_types::{
    AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioStreamPacketDescription,
    kAudioFormatFlagsNativeFloatPacked, kAudioFormatLinearPCM, kAudioFormatOpus,
};
use parking_lot::Mutex;

use crate::CodecError;
use crate::video::AudioEncoder;

/// Sample rate, Hz.
pub const SAMPLE_RATE: u32 = 48_000;
/// Channels.
pub const CHANNELS: u32 = 2;
/// Frames per Opus packet (20 ms).
pub const FRAME_SAMPLES: u32 = 960;
/// Encoder target, bits per second.
pub const BITRATE: u32 = 96_000;
/// Samples (both channels) per packet.
const PACKET_SAMPLES: usize = FRAME_SAMPLES as usize * CHANNELS as usize;
/// Bytes per f32 sample.
const SAMPLE_BYTES: usize = size_of::<f32>();

/// Longest gap concealed, in packets (60 ms). A longer one is a pause: replaying anything for
/// longer sounds worse than silence, and the ring underruns to silence on its own.
pub const MAX_CONCEALED: u32 = 3;

/// Packet-loss concealment without the decoder's help.
///
/// Apple's Opus decoder takes no empty packet for it, so the last decoded packet stands in for
/// a short gap, replayed with a linear fade to silence across the gap: a lost packet is a dip
/// instead of a click. The stand-in keeps the ring's timing too: the gap took 20 ms per packet
/// on the worker, and playing that long keeps the next real packet from arriving early.
#[derive(Debug, Default)]
pub struct Conceal {
    last: Vec<f32>,
}

impl Conceal {
    /// Remember the packet just decoded.
    pub fn remember(&mut self, pcm: &[f32]) {
        self.last.clear();
        self.last.extend_from_slice(pcm);
    }

    /// Append samples standing in for `missing` packets, oldest first: the remembered packet
    /// fading from full level to silence over `min(missing, MAX_CONCEALED)` packets. Nothing
    /// when nothing was remembered or the gap is longer than that.
    pub fn fill(&self, missing: u32, out: &mut Vec<f32>) {
        if missing == 0 || missing > MAX_CONCEALED || self.last.is_empty() {
            return;
        }
        let len = self.last.len();
        let total = usize::try_from(missing).unwrap_or(1).saturating_mul(len).max(1);
        out.reserve(total);
        #[expect(clippy::cast_precision_loss, reason = "sample counts are small")]
        let total_f = total as f32;
        for i in 0..total {
            let sample = self.last.get(i.checked_rem(len).unwrap_or(0)).copied().unwrap_or(0.0);
            #[expect(clippy::cast_precision_loss, reason = "sample counts are small")]
            let gain = 1.0 - (i as f32 + 1.0) / total_f;
            out.push(sample * gain);
        }
    }
}

/// A byte or sample count as the `u32` the C structs carry; saturates on nonsense sizes.
fn u32_of(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}
/// Status the input procs return once they have handed over everything they hold; the
/// converter surfaces it when it wanted more, and the caller treats it as "done for now".
const NO_MORE_INPUT: i32 = 0x534c_4f50; // 'SLOP'

fn pcm_format() -> AudioStreamBasicDescription {
    let bytes_per_frame = CHANNELS * 4;
    AudioStreamBasicDescription {
        mSampleRate: f64::from(SAMPLE_RATE),
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagsNativeFloatPacked,
        mBytesPerPacket: bytes_per_frame,
        mFramesPerPacket: 1,
        mBytesPerFrame: bytes_per_frame,
        mChannelsPerFrame: CHANNELS,
        mBitsPerChannel: 32,
        mReserved: 0,
    }
}

fn opus_format() -> AudioStreamBasicDescription {
    AudioStreamBasicDescription {
        mSampleRate: f64::from(SAMPLE_RATE),
        mFormatID: kAudioFormatOpus,
        mFormatFlags: 0,
        mBytesPerPacket: 0,
        mFramesPerPacket: FRAME_SAMPLES,
        mBytesPerFrame: 0,
        mChannelsPerFrame: CHANNELS,
        mBitsPerChannel: 0,
        mReserved: 0,
    }
}

const fn check(call: &'static str, status: i32) -> Result<(), CodecError> {
    if status == 0 { Ok(()) } else { Err(CodecError::Os { call, status }) }
}

/// An `AudioConverter` that is disposed with the struct.
struct Converter(AudioConverterRef);

// SAFETY: AudioConverter objects have no thread affinity; every call goes through `&mut`.
unsafe impl Send for Converter {}

impl Converter {
    fn new(
        from: &AudioStreamBasicDescription,
        to: &AudioStreamBasicDescription,
    ) -> Result<Self, CodecError> {
        let mut from = *from;
        let mut to = *to;
        let mut raw: AudioConverterRef = ptr::null_mut();
        // SAFETY: AudioToolbox rule: both descriptions and the out pointer are valid for the call.
        let status = unsafe {
            AudioConverterNew(
                NonNull::from(&mut from),
                NonNull::from(&mut to),
                NonNull::from(&mut raw),
            )
        };
        check("AudioConverterNew", status)?;
        Ok(Self(raw))
    }

    fn set_u32(&mut self, property: u32, value: u32) -> Result<(), CodecError> {
        let mut value = value;
        // SAFETY: AudioToolbox rule: a live converter and a property buffer of the stated size.
        let status = unsafe {
            AudioConverterSetProperty(
                self.0,
                property,
                u32_of(size_of::<u32>()),
                NonNull::from(&mut value).cast::<c_void>(),
            )
        };
        check("AudioConverterSetProperty", status)
    }

    fn get_u32(&mut self, property: u32) -> Result<u32, CodecError> {
        let mut value: u32 = 0;
        let mut size = u32_of(size_of::<u32>());
        // SAFETY: AudioToolbox rule: a live converter and a property buffer of the stated size.
        let status = unsafe {
            AudioConverterGetProperty(
                self.0,
                property,
                NonNull::from(&mut size),
                NonNull::from(&mut value).cast::<c_void>(),
            )
        };
        check("AudioConverterGetProperty", status)?;
        Ok(value)
    }
}

impl Drop for Converter {
    fn drop(&mut self) {
        // SAFETY: AudioToolbox rule: dispose once, after the last call; `&mut` guarantees that.
        let _ignored = unsafe { AudioConverterDispose(self.0) };
    }
}

/// What an input proc hands the converter: one slice, given once.
struct Input<'a> {
    data: &'a [u8],
    frames: u32,
    packet: AudioStreamPacketDescription,
    given: bool,
}

/// `AudioConverterComplexInputDataProc`: supplies the input once, then reports no more data.
unsafe extern "C-unwind" fn feed_input(
    _converter: AudioConverterRef,
    io_packets: NonNull<u32>,
    list: NonNull<AudioBufferList>,
    descriptions: *mut *mut AudioStreamPacketDescription,
    user: *mut c_void,
) -> i32 {
    // SAFETY: AudioToolbox rule: `user` is the `Input` passed to `FillComplexBuffer`, alive for
    // the duration of that call, and the proc runs synchronously inside it.
    let input = unsafe { &mut *user.cast::<Input<'_>>() };
    // SAFETY: the converter hands a list with at least one buffer; we only fill the first.
    let list = unsafe { list.as_ptr().as_mut() };
    let Some(list) = list else { return NO_MORE_INPUT };
    if input.given {
        // SAFETY: `io_packets` is the converter's own out-parameter.
        unsafe {
            *io_packets.as_ptr() = 0;
        }
        return NO_MORE_INPUT;
    }
    input.given = true;
    list.mNumberBuffers = 1;
    list.mBuffers[0] = AudioBuffer {
        mNumberChannels: CHANNELS,
        mDataByteSize: u32_of(input.data.len()),
        mData: input.data.as_ptr().cast_mut().cast::<c_void>(),
    };
    if !descriptions.is_null() {
        input.packet.mDataByteSize = u32_of(input.data.len());
        // SAFETY: the converter asked for packet descriptions; it reads them within this call.
        unsafe {
            *descriptions = &raw mut input.packet;
        }
    }
    // SAFETY: as above.
    unsafe {
        *io_packets.as_ptr() = input.frames;
    }
    0
}

/// [`OpusEncoder`] as the worker's [`AudioEncoder`].
#[derive(Debug)]
pub struct Opus(OpusEncoder);

impl AudioEncoder for Opus {
    fn new() -> Result<Self, CodecError> {
        OpusEncoder::new().map(Self)
    }

    fn push(&mut self, samples: &[f32], out: impl FnMut(&[u8])) -> Result<(), CodecError> {
        self.0.push(samples, out)
    }
}

/// PCM in, Opus packets out.
pub struct OpusEncoder {
    converter: Converter,
    pending: VecDeque<f32>,
    packet: Vec<u8>,
}

impl std::fmt::Debug for OpusEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpusEncoder").field("pending", &self.pending.len()).finish_non_exhaustive()
    }
}

impl OpusEncoder {
    /// A stereo 48 kHz encoder at [`BITRATE`].
    pub fn new() -> Result<Self, CodecError> {
        let mut converter = Converter::new(&pcm_format(), &opus_format())?;
        converter.set_u32(kAudioConverterEncodeBitRate, BITRATE)?;
        let max = converter.get_u32(kAudioConverterPropertyMaximumOutputPacketSize)?;
        let max = usize::try_from(max.max(1)).unwrap_or(1);
        Ok(Self { converter, pending: VecDeque::new(), packet: vec![0; max] })
    }

    /// Queue interleaved stereo samples and encode every whole 20 ms packet they complete.
    pub fn push(&mut self, samples: &[f32], mut out: impl FnMut(&[u8])) -> Result<(), CodecError> {
        self.pending.extend(samples);
        while self.pending.len() >= PACKET_SAMPLES {
            let frame: Vec<f32> = self.pending.drain(..PACKET_SAMPLES).collect();
            let n = self.encode_one(&frame)?;
            if let Some(packet) = self.packet.get(..n).filter(|p| !p.is_empty()) {
                out(packet);
            }
        }
        Ok(())
    }

    fn encode_one(&mut self, frame: &[f32]) -> Result<usize, CodecError> {
        let bytes: &[u8] = bytemuck_cast(frame);
        let mut input = Input {
            data: bytes,
            frames: FRAME_SAMPLES,
            packet: AudioStreamPacketDescription {
                mStartOffset: 0,
                mVariableFramesInPacket: 0,
                mDataByteSize: 0,
            },
            given: false,
        };
        let mut packets: u32 = 1;
        let mut list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [AudioBuffer {
                mNumberChannels: CHANNELS,
                mDataByteSize: u32_of(self.packet.len()),
                mData: self.packet.as_mut_ptr().cast::<c_void>(),
            }],
        };
        let mut description = AudioStreamPacketDescription {
            mStartOffset: 0,
            mVariableFramesInPacket: 0,
            mDataByteSize: 0,
        };
        // SAFETY: AudioToolbox rule: the proc, its user data and the output list outlive the
        // call; the output buffer is at least `MaximumOutputPacketSize` bytes.
        let status = unsafe {
            AudioConverterFillComplexBuffer(
                self.converter.0,
                Some(feed_input),
                (&raw mut input).cast::<c_void>(),
                NonNull::from(&mut packets),
                NonNull::from(&mut list),
                &raw mut description,
            )
        };
        if status != 0 && status != NO_MORE_INPUT {
            return Err(CodecError::Os { call: "AudioConverterFillComplexBuffer", status });
        }
        Ok(if packets == 0 { 0 } else { usize::try_from(description.mDataByteSize).unwrap_or(0) })
    }
}

/// Opus packets in, interleaved stereo PCM out.
pub struct OpusDecoder {
    converter: Converter,
    pcm: Vec<f32>,
}

impl std::fmt::Debug for OpusDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpusDecoder").finish_non_exhaustive()
    }
}

impl OpusDecoder {
    /// A stereo 48 kHz decoder.
    pub fn new() -> Result<Self, CodecError> {
        let converter = Converter::new(&opus_format(), &pcm_format())?;
        Ok(Self { converter, pcm: vec![0.0; PACKET_SAMPLES * 3] })
    }

    /// Decode one packet; the samples are valid until the next call.
    pub fn decode(&mut self, packet: &[u8]) -> Result<&[f32], CodecError> {
        let mut input = Input {
            data: packet,
            frames: 1,
            packet: AudioStreamPacketDescription {
                mStartOffset: 0,
                mVariableFramesInPacket: 0,
                mDataByteSize: 0,
            },
            given: false,
        };
        let mut frames = u32_of(self.pcm.len().checked_div(CHANNELS as usize).unwrap_or(0));
        let mut list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [AudioBuffer {
                mNumberChannels: CHANNELS,
                mDataByteSize: u32_of(self.pcm.len().saturating_mul(SAMPLE_BYTES)),
                mData: self.pcm.as_mut_ptr().cast::<c_void>(),
            }],
        };
        // SAFETY: as in the encoder; the output buffer holds `frames` frames of PCM.
        let status = unsafe {
            AudioConverterFillComplexBuffer(
                self.converter.0,
                Some(feed_input),
                (&raw mut input).cast::<c_void>(),
                NonNull::from(&mut frames),
                NonNull::from(&mut list),
                ptr::null_mut(),
            )
        };
        if status != 0 && status != NO_MORE_INPUT {
            return Err(CodecError::Os { call: "AudioConverterFillComplexBuffer", status });
        }
        let n = usize::try_from(frames).unwrap_or(0).saturating_mul(CHANNELS as usize);
        Ok(self.pcm.get(..n).unwrap_or_default())
    }
}

/// Reinterpret a float slice as bytes (native endian, which is what the PCM format says).
const fn bytemuck_cast(frame: &[f32]) -> &[u8] {
    // SAFETY: f32 has no padding or invalid bit patterns; the byte view covers exactly the
    // slice's memory and shares its lifetime.
    unsafe { std::slice::from_raw_parts(frame.as_ptr().cast::<u8>(), size_of_val(frame)) }
}

/// When a packet reached the client: what the jitter estimate is made of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Arrival {
    /// The packet's sequence number, which is the worker's 20 ms clock.
    pub seq: u32,
    /// When its datagram came off the connection.
    pub at: Instant,
}

/// What playback has done so far, for the stream's counters.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PlayoutStats {
    /// Times the ring ran dry while it was playing sound. Running dry after silence is the
    /// host's gate closing and is not counted.
    pub underruns: u64,
    /// Audio dropped to shed delay: a stall's backlog at once, a drifting clock a slice at a time.
    pub trimmed: Duration,
    /// Audio played twice to add depth, a slice at a time.
    pub stretched: Duration,
    /// Depth the ring aims to hold when a packet arrives on time.
    pub target: Duration,
    /// How late packets run, the 95th percentile over the estimate's window.
    pub jitter: Duration,
}

/// One packet's timing in the estimate's window.
#[derive(Clone, Copy, Debug)]
struct Timed {
    /// Arrival, microseconds since the first packet.
    at_us: i64,
    /// Arrival minus the packet's place on the worker's clock (`seq × 20 ms`), which is the
    /// one-way delay plus an unknown constant.
    delay_us: i64,
    /// The ring ran dry waiting for this packet.
    starved: bool,
    /// Released by a stall: later than any depth could cover, or in the same release as one
    /// that was. A stall's backlog comes out with every lateness from its length down to none,
    /// and none of it says how late packets run otherwise.
    stall: bool,
}

/// The jitter buffer policy: how deep the ring should be, from how late packets run.
///
/// Each packet's delay is its arrival minus its place on the worker's 20 ms clock; how late it
/// ran is that delay minus the smallest one over the last [`JITTER_WINDOW_US`]. The depth an
/// on-time packet should find is the 95th percentile of that lateness plus one device buffer,
/// held between [`TARGET_MIN_US`] and [`TARGET_MAX_US`].
///
/// Only the thread that decodes packets takes it, never the device's callback, so the sort
/// behind the percentile never holds up a buffer (see [`Ring`]).
#[derive(Debug)]
struct Jitter {
    /// The first arrival, which the delays are measured from.
    epoch: Option<Instant>,
    /// Timing of the packets in the window, oldest first.
    window: VecDeque<Timed>,
    /// When the last packet later than any depth could cover arrived, microseconds.
    released_at: Option<i64>,
    target_us: i64,
    lateness_us: i64,
    /// The window's lateness, sorted for the percentile; kept to be refilled for each packet.
    late: Vec<i64>,
}

/// What the ring saw since the last packet, which the next packet's timing reads.
#[derive(Clone, Copy, Debug, Default)]
struct Since {
    /// The ring ran dry under sound: this packet is the late one.
    starved: bool,
    /// The ring ran dry after silence, or the stream was muted: the host's gate may have
    /// closed meanwhile, and its sequence clock stood still, so this packet starts a new
    /// estimate.
    gated: bool,
    /// The last packet had nothing above the host's silence floor.
    quiet: bool,
}

/// How a packet ran, for the ring to steer by.
#[derive(Clone, Copy, Debug)]
struct Timing {
    /// How late it ran against the window's earliest.
    lateness: i64,
    /// The depth an on-time packet should find.
    target_us: i64,
    /// The estimate started over with this packet.
    rebased: bool,
}

/// Samples waiting to be played, shared with the device's callback.
///
/// The ring starts, and restarts after running dry, only once it holds the target. The depth
/// an arrival reveals is the ring's level before the packet goes in plus how late the packet
/// ran: a late packet finds the ring lower by exactly its lateness, so a clump of late packets
/// reads the same depth as an on-time stream and costs nothing. A depth that stays off the
/// target is corrected one [`SLICE_FRAMES`] slice per packet, dropped or played twice with a
/// [`FADE_FRAMES`] crossfade, which is how a worker clock running ±100 ppm off the device's is
/// absorbed. A depth past the target by more than [`BURST_US`] is the backlog a stall
/// releases, and goes at once, crossfaded, rather than as lasting delay.
///
/// Nothing done under its lock allocates beyond the ring's own growth, or sorts.
#[derive(Debug, Default)]
struct Ring {
    samples: VecDeque<f32>,
    /// Playing from the ring, as against filling it up to the target.
    playing: bool,
    /// Cutting a stall's backlog (see [`Ring::converge`]).
    cutting: bool,
    /// Depths the recent arrivals read, microseconds, newest last.
    recent: VecDeque<i64>,
    /// The last packet pushed had a sample above the host's silence floor.
    last_loud: bool,
    /// The ring ran dry under sound: the next packet is the late one.
    starved: bool,
    /// The ring ran dry after silence, or the stream was muted (see [`Since::gated`]).
    gated: bool,
    /// The last frame the device was given, and how many frames of the ramp from it down to
    /// silence are still to play: a ring that runs dry at a buffer's edge has nothing left to
    /// fade, so the fade continues from what was last heard.
    tail: [f32; 2],
    ramp: usize,
    underruns: u64,
    trimmed_frames: u64,
    stretched_frames: u64,
}

/// Microseconds of audio in one 20 ms packet.
const PACKET_US: i64 = 20_000;
/// Microseconds of audio in one device buffer.
const DEVICE_US: i64 = 10_000;
/// How far back the jitter estimate looks.
const JITTER_WINDOW_US: i64 = 5_000_000;
/// Arrivals the estimate needs before it is trusted over [`TARGET_DEFAULT_US`].
const ESTIMATE_AFTER_US: i64 = 1_000_000;
/// Depth before the estimate is trusted: two packets, what the ring held before it had one.
const TARGET_DEFAULT_US: i64 = 40_000;
/// Least depth: the device takes 10 ms at a time, and a packet arrives every 20.
const TARGET_MIN_US: i64 = 20_000;
/// Most depth. Lateness a buffer this deep cannot cover is a stall, and is left out of the
/// estimate: waiting for it would only hold the whole window's audio that much later.
const TARGET_MAX_US: i64 = 120_000;
/// Most lateness a depth can cover; anything later is a stall.
const COVERED_US: i64 = TARGET_MAX_US - DEVICE_US;
/// A delay this far past the window's least is not lateness: the worker's sequence restarted,
/// or the gate held it still. The estimate starts over.
const REBASE_US: i64 = 1_000_000;
/// Depth past the target by more than this is a stall's backlog, cut in one go.
const BURST_US: i64 = 40_000;
/// How far the recent depth may sit off the target before a slice corrects it. Past half a
/// device buffer plus half a slice, so the device's 10 ms phase against the packets' arrival
/// and one slice's correction cannot alternate a drop and a stretch.
const DEADBAND_US: i64 = 7_500;
/// Depths averaged for a slice's decision (half a second of packets), and how many it waits for.
const RECENT: usize = 25;
const RECENT_MIN: usize = 10;
/// Frames dropped or played twice per correction: 5 ms, a quarter of a packet.
const SLICE_FRAMES: usize = 240;
/// Frames each join is crossfaded over: 2.5 ms, long enough that no step is a click.
const FADE_FRAMES: usize = 120;
/// Least the ring keeps after a cut, so the device's next pull still finds a buffer.
const KEEP_US: i64 = DEVICE_US;
/// A sample above this is sound: the host's gate floor, -80 dBFS.
const LOUD: f32 = 1e-4;

/// Microseconds of audio in `frames` frames.
fn frames_us(frames: usize) -> i64 {
    i64::try_from(frames)
        .unwrap_or(i64::MAX)
        .saturating_mul(1_000_000)
        .checked_div(i64::from(SAMPLE_RATE))
        .unwrap_or(0)
}

/// Frames of audio in `us` microseconds, rounded down.
fn us_frames(us: i64) -> usize {
    usize::try_from(us.max(0).saturating_mul(i64::from(SAMPLE_RATE)) / 1_000_000).unwrap_or(0)
}

/// A duration from microseconds, zero for anything negative.
fn duration_us(us: i64) -> Duration {
    Duration::from_micros(u64::try_from(us).unwrap_or(0))
}

/// The crossfade's weight on the incoming side at frame `k`: never quite 0 or 1, so both ends
/// join their neighbours without a step.
#[expect(clippy::cast_precision_loss, reason = "fade lengths are a few hundred frames")]
fn fade_in_weight(k: usize) -> f32 {
    (k as f32 + 1.0) / (FADE_FRAMES as f32 + 1.0)
}

impl Default for Jitter {
    fn default() -> Self {
        Self {
            epoch: None,
            window: VecDeque::new(),
            released_at: None,
            target_us: TARGET_DEFAULT_US,
            lateness_us: 0,
            late: Vec::new(),
        }
    }
}

impl Jitter {
    fn min_delay(&self) -> Option<i64> {
        self.window.iter().map(|t| t.delay_us).min()
    }

    /// Put an arrival into the estimate.
    fn time(&mut self, arrival: Arrival, since: Since) -> Timing {
        let epoch = *self.epoch.get_or_insert(arrival.at);
        let at_us = i64::try_from(arrival.at.saturating_duration_since(epoch).as_micros())
            .unwrap_or(i64::MAX);
        let delay_us = at_us.saturating_sub(i64::from(arrival.seq).saturating_mul(PACKET_US));
        // Later than a packet after a quiet one is the gate reopening even when the ring did not
        // run dry to say so: it held more than the gap, or the stream was muted.
        let jumped = self.min_delay().is_some_and(|min| {
            let late = delay_us.saturating_sub(min);
            late > REBASE_US || (since.quiet && late > PACKET_US)
        });
        let rebased = since.gated || jumped;
        if rebased {
            self.window.clear();
        }
        let horizon = at_us.saturating_sub(JITTER_WINDOW_US);
        while self.window.front().is_some_and(|t| t.at_us < horizon) {
            self.window.pop_front();
        }
        let min = self.min_delay().map_or(delay_us, |min| min.min(delay_us));
        let lateness = delay_us.saturating_sub(min);
        if lateness > COVERED_US {
            self.released_at = Some(at_us);
        }
        let stall = self.released_at.is_some_and(|t| at_us.saturating_sub(t) <= DEVICE_US);
        self.window.push_back(Timed { at_us, delay_us, starved: since.starved, stall });
        self.retarget(at_us, min);
        Timing { lateness, target_us: self.target_us, rebased }
    }

    /// The target from the window: the 95th percentile of lateness, and never less than a
    /// lateness that starved the ring in it, since that one is known not to be noise.
    fn retarget(&mut self, now_us: i64, min: i64) {
        self.late.clear();
        self.late.extend(
            self.window.iter().filter(|t| !t.stall).map(|t| t.delay_us.saturating_sub(min)),
        );
        self.late.sort_unstable();
        let late = &self.late;
        let p95 = late.get(late.len().saturating_mul(95) / 100).or_else(|| late.last()).copied();
        let held = self
            .window
            .iter()
            .filter(|t| t.starved && !t.stall)
            .map(|t| t.delay_us.saturating_sub(min))
            .max();
        self.lateness_us = p95.unwrap_or(0).max(held.unwrap_or(0));
        let target = self.lateness_us.saturating_add(DEVICE_US).clamp(TARGET_MIN_US, TARGET_MAX_US);
        let span = self.window.front().map_or(0, |t| now_us.saturating_sub(t.at_us));
        self.target_us =
            if span < ESTIMATE_AFTER_US { target.max(TARGET_DEFAULT_US) } else { target };
    }

    /// Time one decoded packet, then queue it. The ring is locked to read what it saw and again
    /// to queue: the estimate runs between, outside the lock the device's callback takes.
    fn push(&mut self, ring: &Mutex<Ring>, pcm: &[f32], arrival: Arrival) {
        let since = ring.lock().since();
        let timing = self.time(arrival, since);
        ring.lock().push(pcm, timing);
    }

    /// A packet that is timed but not played (the stream is muted).
    fn hold(&mut self, ring: &Mutex<Ring>, arrival: Arrival) {
        let since = ring.lock().since();
        let _timing = self.time(arrival, since);
        ring.lock().hold();
    }
}

impl Ring {
    /// Microseconds of audio in the ring.
    fn level_us(&self) -> i64 {
        frames_us(self.samples.len().checked_div(CHANNELS as usize).unwrap_or(0))
    }

    /// Hand what the ring saw since the last packet to the next one's timing.
    const fn since(&mut self) -> Since {
        let since = Since { starved: self.starved, gated: self.gated, quiet: !self.last_loud };
        self.starved = false;
        self.gated = false;
        since
    }

    /// Queue one decoded packet and steer the depth.
    fn push(&mut self, pcm: &[f32], timing: Timing) {
        if timing.rebased {
            self.recent.clear();
        }
        let before = self.level_us();
        self.samples.extend(pcm);
        self.last_loud = pcm.iter().any(|s| s.abs() > LOUD);
        if self.playing {
            self.converge(before.saturating_add(timing.lateness), timing.target_us);
        } else if self.level_us().saturating_add(timing.lateness).saturating_sub(PACKET_US)
            >= timing.target_us
        {
            // Full enough that the next packet, on time, finds the target.
            self.playing = true;
            self.recent.clear();
            self.fade_in();
        }
    }

    /// Stand-ins for lost packets: they keep the time the gap took, and read no depth.
    fn conceal(&mut self, pcm: &[f32]) {
        self.samples.extend(pcm);
    }

    /// The stream is muted: the ring empties, ramping down from what was last heard, and fills
    /// again to the target when sound is wanted. The host's gate may close meanwhile with
    /// nothing running dry to show it, so the next packet starts a new estimate.
    fn hold(&mut self) {
        if self.playing {
            self.ramp = FADE_FRAMES;
        }
        self.samples.clear();
        self.playing = false;
        self.last_loud = false;
        self.gated = true;
    }

    /// Move the depth towards the target: a stall's backlog at once, a drift one slice at a time.
    fn converge(&mut self, depth: i64, target_us: i64) {
        let excess = depth.saturating_sub(target_us);
        // A backlog arrives packet by packet and the ring holds only so much of it at a time, so
        // the cut goes on with each packet until the depth is back at the target.
        self.cutting = excess > BURST_US || (self.cutting && excess > DEADBAND_US);
        if self.cutting {
            let room =
                self.level_us().saturating_sub(KEEP_US).saturating_sub(frames_us(FADE_FRAMES));
            let cut = us_frames(excess.min(room));
            if cut >= SLICE_FRAMES {
                self.drop_frames(cut);
            }
            self.recent.clear();
            return;
        }
        self.recent.push_back(depth);
        if self.recent.len() > RECENT {
            self.recent.pop_front();
        }
        if self.recent.len() < RECENT_MIN {
            return;
        }
        let sum = self.recent.iter().fold(0_i64, |sum, &d| sum.saturating_add(d));
        let mean = sum.checked_div(i64::try_from(self.recent.len()).unwrap_or(1)).unwrap_or(0);
        let slice = frames_us(SLICE_FRAMES);
        let spare =
            self.level_us().saturating_sub(KEEP_US) >= slice.saturating_add(frames_us(FADE_FRAMES));
        let shift = if mean.saturating_sub(target_us) > DEADBAND_US
            && spare
            && self.drop_frames(SLICE_FRAMES)
        {
            slice.saturating_neg()
        } else if target_us.saturating_sub(mean) > DEADBAND_US && self.stretch_frames(SLICE_FRAMES)
        {
            slice
        } else {
            return;
        };
        for d in &mut self.recent {
            *d = d.saturating_add(shift);
        }
    }

    /// Drop `n` frames from the front, crossfading what came before them into what follows.
    fn drop_frames(&mut self, n: usize) -> bool {
        let channels = CHANNELS as usize;
        let (cut, fade) = (n.saturating_mul(channels), FADE_FRAMES.saturating_mul(channels));
        if self.samples.len() < cut.saturating_add(fade) {
            return false;
        }
        let buf = self.samples.make_contiguous();
        for k in 0..fade {
            let w = fade_in_weight(k.checked_div(channels).unwrap_or(0));
            let outgoing = buf.get(k).copied().unwrap_or(0.0);
            if let Some(incoming) = buf.get_mut(cut.saturating_add(k)) {
                *incoming = incoming.mul_add(w, outgoing * (1.0 - w));
            }
        }
        self.samples.drain(..cut);
        self.trimmed_frames = self.trimmed_frames.saturating_add(n as u64);
        true
    }

    /// Play `n` frames at the front twice: after them, crossfade back to the start of the ring.
    /// Done in place, in room opened at the front; `n` is at least a fade long.
    fn stretch_frames(&mut self, n: usize) -> bool {
        let channels = CHANNELS as usize;
        let (span, fade) = (n.saturating_mul(channels), FADE_FRAMES.saturating_mul(channels));
        if self.samples.len() < span.saturating_add(fade) || span < fade {
            return false;
        }
        self.samples.resize(self.samples.len().saturating_add(span), 0.0);
        self.samples.rotate_right(span);
        // The old ring now starts at `span`: its first `span` samples are copied into the room,
        // then its first `fade` are crossfaded, in place, into the ones a span later. What
        // follows the crossfade is the old ring from the end of the fade, already in place.
        let buf = self.samples.make_contiguous();
        buf.copy_within(span..span.saturating_mul(2), 0);
        for k in 0..fade {
            let w = fade_in_weight(k.checked_div(channels).unwrap_or(0));
            let outgoing =
                buf.get(span.saturating_mul(2).saturating_add(k)).copied().unwrap_or(0.0);
            if let Some(incoming) = buf.get_mut(span.saturating_add(k)) {
                *incoming = incoming.mul_add(w, outgoing * (1.0 - w));
            }
        }
        self.stretched_frames = self.stretched_frames.saturating_add(n as u64);
        true
    }

    /// Ramp the front of the ring up from silence.
    fn fade_in(&mut self) {
        let channels = CHANNELS as usize;
        for (i, sample) in
            self.samples.iter_mut().take(FADE_FRAMES.saturating_mul(channels)).enumerate()
        {
            *sample *= fade_in_weight(i.checked_div(channels).unwrap_or(0));
        }
    }

    /// Fill `out` for the device. A ring that runs dry ramps from the last sample it played
    /// down to silence, and fills up to the target again before it plays.
    fn pull(&mut self, out: &mut [f32]) {
        let channels = CHANNELS as usize;
        let have = if self.playing { self.samples.len().min(out.len()) } else { 0 };
        for (slot, sample) in out.iter_mut().zip(self.samples.drain(..have)) {
            *slot = sample;
        }
        if self.playing && have < out.len() {
            self.playing = false;
            self.recent.clear();
            self.ramp = FADE_FRAMES;
            if self.last_loud {
                self.underruns = self.underruns.saturating_add(1);
                self.starved = true;
            } else {
                self.gated = true;
            }
        }
        if let Some(last) = out.get(..have).and_then(|played| played.chunks_exact(channels).last())
        {
            for (tail, &sample) in self.tail.iter_mut().zip(last) {
                *tail = sample;
            }
        }
        let rest = out.get_mut(have..).unwrap_or_default();
        for frame in rest.chunks_mut(channels) {
            #[expect(clippy::cast_precision_loss, reason = "a few hundred frames")]
            let gain = self.ramp as f32 / (FADE_FRAMES as f32 + 1.0);
            self.ramp = self.ramp.saturating_sub(1);
            for (sample, &tail) in frame.iter_mut().zip(&self.tail) {
                *sample = tail * gain;
            }
        }
    }
}

/// What playback has done, from the ring's counts and the estimate's depth.
fn playout_stats(ring: &Ring, jitter: &Jitter) -> PlayoutStats {
    let frames = |n: u64| {
        let us = n.saturating_mul(1_000_000).checked_div(u64::from(SAMPLE_RATE));
        Duration::from_micros(us.unwrap_or(0))
    };
    PlayoutStats {
        underruns: ring.underruns,
        trimmed: frames(ring.trimmed_frames),
        stretched: frames(ring.stretched_frames),
        target: duration_us(jitter.target_us),
        jitter: duration_us(jitter.lateness_us),
    }
}

/// Samples (both channels) in `ms` milliseconds.
const fn samples_in_ms(ms: usize) -> usize {
    ms.saturating_mul(SAMPLE_RATE as usize / 1000).saturating_mul(CHANNELS as usize)
}

/// Samples in one output buffer: 10 ms, half a packet, so the device holds little ahead of
/// the ring.
const BUFFER_SAMPLES: usize = samples_in_ms(10);
/// Bytes in one output buffer.
const BUFFER_BYTES: usize = BUFFER_SAMPLES * SAMPLE_BYTES;
/// Output buffers in flight: one playing, two queued behind it.
const QUEUE_BUFFERS: usize = 3;

/// An `AudioQueue` playing interleaved stereo float at 48 kHz from a ring the decoder fills.
pub struct Player {
    queue: AudioQueueRef,
    ring: Arc<Mutex<Ring>>,
    /// Taken by whoever pushes packets, never by the callback. Boxed so a player stays small
    /// beside the other states of a stream's audio.
    jitter: Box<Mutex<Jitter>>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player").finish_non_exhaustive()
    }
}

// SAFETY: AudioQueue calls are thread-safe; the ring is behind a mutex.
unsafe impl Send for Player {}

/// `AudioQueueOutputCallback`: refill a buffer from the ring (see [`Ring::pull`]).
unsafe extern "C-unwind" fn refill(
    user: *mut c_void,
    queue: AudioQueueRef,
    buffer: AudioQueueBufferRef,
) {
    // SAFETY: `user` is the `Arc<Mutex<Ring>>` leaked into the queue at creation and reclaimed
    // in `Drop` only after `AudioQueueDispose` returned, which ends all callbacks.
    let ring = unsafe { &*user.cast::<Mutex<Ring>>() };
    // SAFETY: the queue hands a buffer it allocated; the struct is valid until we re-enqueue it.
    let Some(buf) = (unsafe { buffer.as_mut() }) else { return };
    let capacity = usize::try_from(buf.mAudioDataBytesCapacity).unwrap_or(0) / SAMPLE_BYTES;
    let want = capacity.min(BUFFER_SAMPLES);
    // SAFETY: `mAudioData` points at `mAudioDataBytesCapacity` bytes owned by the queue.
    let out =
        unsafe { std::slice::from_raw_parts_mut(buf.mAudioData.as_ptr().cast::<f32>(), want) };
    ring.lock().pull(out);
    buf.mAudioDataByteSize = u32_of(want.saturating_mul(SAMPLE_BYTES));
    // SAFETY: re-enqueueing the queue's own buffer from its callback is the documented pattern.
    let _ignored = unsafe { AudioQueueEnqueueBuffer(queue, buffer, 0, ptr::null()) };
}

impl Player {
    /// Create and start the queue (it plays silence until samples arrive).
    pub fn new() -> Result<Self, CodecError> {
        let ring: Arc<Mutex<Ring>> = Arc::default();
        let user = Arc::into_raw(Arc::clone(&ring)).cast_mut().cast::<c_void>();
        let mut format = pcm_format();
        let mut queue: AudioQueueRef = ptr::null_mut();
        // SAFETY: AudioToolbox rule: the format and out pointer are valid; no run loop means the
        // callback runs on the queue's own thread, which only touches the ring.
        let status = unsafe {
            AudioQueueNewOutput(
                NonNull::from(&mut format),
                Some(refill),
                user,
                None,
                None,
                0,
                NonNull::from(&mut queue),
            )
        };
        if let Err(e) = check("AudioQueueNewOutput", status) {
            // SAFETY: the queue never took the pointer; reclaim the clone we leaked for it.
            drop(unsafe { Arc::from_raw(user.cast_const().cast::<Mutex<Ring>>()) });
            return Err(e);
        }
        let player = Self { queue, ring, jitter: Box::default() };
        let bytes = u32_of(BUFFER_BYTES);
        for _ in 0..QUEUE_BUFFERS {
            let mut buffer: AudioQueueBufferRef = ptr::null_mut();
            // SAFETY: AudioToolbox rule: a live queue and an out pointer.
            let status =
                unsafe { AudioQueueAllocateBuffer(queue, bytes, NonNull::from(&mut buffer)) };
            check("AudioQueueAllocateBuffer", status)?;
            // Prime with silence so the queue has something to play at start.
            // SAFETY: the buffer was just allocated by this queue and `user` is our ring.
            unsafe {
                refill(user, queue, buffer);
            }
        }
        // SAFETY: AudioToolbox rule: start after priming.
        check("AudioQueueStart", unsafe { AudioQueueStart(queue, ptr::null()) })?;
        Ok(player)
    }

    /// Queue one decoded packet, timed by its arrival (see [`Arrival`]).
    pub fn push(&self, samples: &[f32], arrival: Arrival) {
        self.jitter.lock().push(&self.ring, samples, arrival);
    }

    /// Queue stand-ins for lost packets (see [`Conceal`]); they take the gap's time.
    pub fn conceal(&self, samples: &[f32]) {
        self.ring.lock().conceal(samples);
    }

    /// A packet that arrived but is not to be heard (the stream is muted): its timing still
    /// counts, and what is queued goes.
    pub fn hold(&self, arrival: Arrival) {
        self.jitter.lock().hold(&self.ring, arrival);
    }

    /// What playback has done so far.
    #[must_use]
    pub fn stats(&self) -> PlayoutStats {
        let jitter = self.jitter.lock();
        playout_stats(&self.ring.lock(), &jitter)
    }

    /// Samples waiting to be played, both channels.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.ring.lock().samples.len()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // SAFETY: AudioToolbox rule: `inImmediate = true` stops and disposes synchronously; no
        // callback runs after it returns, so the ring pointer can be reclaimed.
        let _ignored = unsafe { AudioQueueDispose(self.queue, true) };
        let user = Arc::as_ptr(&self.ring);
        // SAFETY: this is the clone `new` leaked with `Arc::into_raw`; the callback is done.
        drop(unsafe { Arc::from_raw(user) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_decodes_a_tone() {
        let mut enc = OpusEncoder::new().unwrap();
        let mut dec = OpusDecoder::new().unwrap();
        // 100 ms of a 440 Hz tone, stereo interleaved.
        let frames = SAMPLE_RATE as usize / 10;
        #[expect(clippy::cast_precision_loss, reason = "small test counts")]
        let pcm: Vec<f32> = (0..frames)
            .flat_map(|i| {
                let s = (i as f32 * 440.0 * std::f32::consts::TAU / SAMPLE_RATE as f32).sin() * 0.5;
                [s, s]
            })
            .collect();
        let mut packets = Vec::new();
        enc.push(&pcm, |p| packets.push(p.to_vec())).unwrap();
        assert!(packets.len() >= 4, "got {} packets", packets.len());
        assert!(packets.iter().all(|p| !p.is_empty() && p.len() < 1200));
        let mut out = 0_usize;
        let mut energy = 0.0_f32;
        for p in &packets {
            let s = dec.decode(p).unwrap();
            out = out.saturating_add(s.len());
            energy += s.iter().map(|x| x * x).sum::<f32>();
        }
        // Opus pre-skip: the first packet comes back a few frames short.
        let whole = packets.len().saturating_mul(PACKET_SAMPLES);
        assert!(out >= whole.saturating_sub(PACKET_SAMPLES) && out <= whole, "{out}");
        assert!(energy > 1.0, "decoded audio is silent: {energy}");
    }

    #[test]
    #[expect(clippy::disallowed_methods, reason = "a test waiting on the audio thread")]
    fn player_starts_and_drains() {
        let player = Player::new().unwrap();
        let at = Instant::now();
        for seq in 1..=3 {
            let at = at + Duration::from_millis(u64::from(seq) * 20);
            player.push(&vec![0.1; PACKET_SAMPLES], Arrival { seq, at });
        }
        assert!(player.queued() <= PACKET_SAMPLES * 3);
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(player.queued(), 0);
    }
}

#[cfg(test)]
mod conceal_tests {
    use super::*;

    /// One lost packet is the last one replayed, fading to silence by its end.
    #[test]
    fn a_short_gap_is_the_last_packet_fading_out() {
        let mut c = Conceal::default();
        c.remember(&[1.0, 1.0, 1.0, 1.0]);
        let mut out = Vec::new();
        c.fill(1, &mut out);
        assert_eq!(out, vec![0.75, 0.5, 0.25, 0.0]);
        out.clear();
        c.fill(2, &mut out);
        assert_eq!(out.len(), 8);
        assert!((out[0] - 0.875).abs() < 1e-6 && out[7] == 0.0, "{out:?}");
        assert!(out.windows(2).all(|w| w[0] >= w[1]), "monotone fade: {out:?}");
    }

    /// Nothing to replay, no gap, or a gap too long to paper over: silence from the ring.
    #[test]
    fn nothing_is_concealed_without_a_packet_or_past_the_cap() {
        let mut out = Vec::new();
        Conceal::default().fill(1, &mut out);
        assert!(out.is_empty());
        let mut c = Conceal::default();
        c.remember(&[0.5, 0.5]);
        c.fill(0, &mut out);
        c.fill(MAX_CONCEALED + 1, &mut out);
        assert!(out.is_empty());
        c.fill(MAX_CONCEALED, &mut out);
        assert_eq!(out.len(), 2 * MAX_CONCEALED as usize);
    }
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "test fixture arithmetic on traces of a few minutes"
)]
mod latency_tests {
    use super::*;

    /// The player's two halves, driven the way [`Player`] drives them.
    #[derive(Default)]
    struct Playout {
        jitter: Jitter,
        ring: Mutex<Ring>,
    }

    impl Playout {
        fn push(&mut self, pcm: &[f32], arrival: Arrival) {
            self.jitter.push(&self.ring, pcm, arrival);
        }

        fn hold(&mut self, arrival: Arrival) {
            self.jitter.hold(&self.ring, arrival);
        }

        fn pull(&self, out: &mut [f32]) {
            self.ring.lock().pull(out);
        }

        fn stats(&self) -> PlayoutStats {
            playout_stats(&self.ring.lock(), &self.jitter)
        }
    }

    /// A 440 Hz tone at half scale, continuous across packets, so any step in the output larger
    /// than the tone's own is a join the playout made.
    fn tone(seq: u32) -> Vec<f32> {
        let first = u64::from(seq.saturating_sub(1)) * u64::from(FRAME_SAMPLES);
        (0..u64::from(FRAME_SAMPLES))
            .flat_map(|i| {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "sample indices stay far below 2^52"
                )]
                let t = (first + i) as f64 / f64::from(SAMPLE_RATE);
                #[expect(clippy::cast_possible_truncation, reason = "a sample")]
                let s = ((t * 440.0 * std::f64::consts::TAU).sin() * 0.5) as f32;
                [s, s]
            })
            .collect()
    }

    /// The largest step between neighbouring samples of that tone.
    fn tone_step() -> f32 {
        let one = tone(1);
        one.chunks(2)
            .zip(one.chunks(2).skip(1))
            .map(|(a, b)| (b[0] - a[0]).abs())
            .fold(0.0, f32::max)
    }

    /// What a run did.
    #[derive(Debug, Default)]
    struct Run {
        /// Largest step between neighbouring output samples, left channel.
        max_step: f32,
        /// What the listener hears behind the worker at each device pull, ms: the ring plus the
        /// device buffers queued ahead of it (the callback refills one as soon as it is played).
        heard: Vec<(i64, i64)>,
        stats: PlayoutStats,
    }

    impl Run {
        fn mean_heard(&self, from_us: i64, to_us: i64) -> i64 {
            let span: Vec<i64> = self
                .heard
                .iter()
                .filter(|&&(t, _)| t >= from_us && t < to_us)
                .map(|&(_, ms)| ms)
                .collect();
            span.iter().sum::<i64>() / i64::try_from(span.len().max(1)).unwrap()
        }

        fn max_heard(&self, from_us: i64, to_us: i64) -> i64 {
            self.heard
                .iter()
                .filter(|&&(t, _)| t >= from_us && t < to_us)
                .map(|&(_, ms)| ms)
                .max()
                .unwrap_or(0)
        }
    }

    /// Play `arrivals` (arrival µs, sequence), in arrival order, against a device that pulls a
    /// 10 ms buffer every 10 ms of its own clock, until `end_us`. Driven through the same
    /// `push` and `pull` the player runs; the clocks are the trace's, so nothing sleeps.
    fn play(arrivals: &[(i64, u32)], end_us: i64) -> Run {
        play_muted(arrivals, end_us, (0, 0))
    }

    /// [`play`], with the packets arriving in `muted` (from, to) held rather than played.
    fn play_muted(arrivals: &[(i64, u32)], end_us: i64, muted: (i64, i64)) -> Run {
        let epoch = Instant::now();
        let mut playout = Playout::default();
        let mut run = Run::default();
        let mut out = vec![0.0_f32; BUFFER_SAMPLES];
        let mut last = 0.0_f32;
        let mut next = arrivals.iter().peekable();
        let mut pull_at = 5_000_i64;
        while pull_at < end_us {
            while let Some(&&(at_us, seq)) = next.peek().filter(|&&&(at, _)| at <= pull_at) {
                let at = epoch + Duration::from_micros(u64::try_from(at_us).unwrap());
                if (muted.0..muted.1).contains(&at_us) {
                    playout.hold(Arrival { seq, at });
                } else {
                    playout.push(&tone(seq), Arrival { seq, at });
                }
                next.next();
            }
            playout.pull(&mut out);
            for frame in out.chunks(2) {
                run.max_step = run.max_step.max((frame[0] - last).abs());
                last = frame[0];
            }
            let queued = playout.ring.lock().samples.len() + QUEUE_BUFFERS * BUFFER_SAMPLES;
            let ms = i64::try_from(queued * 1000 / samples_in_ms(1000)).unwrap();
            run.heard.push((pull_at, ms));
            pull_at += DEVICE_US;
        }
        run.stats = playout.stats();
        run
    }

    /// A deterministic scatter of `0..spread_us` per packet (a 64-bit LCG).
    fn scatter(seq: u32, spread_us: i64) -> i64 {
        let x = u64::from(seq)
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        i64::try_from((x >> 33) % u64::try_from(spread_us.max(1)).unwrap()).unwrap()
    }

    /// Packets every 20 ms on a worker clock `ppm` parts per million fast, 3 ms of link delay
    /// and `spread_us` of scatter on top; a window `(from, to)` of the worker's clock in which
    /// the link held everything and let it go at `to`.
    fn trace(seconds: u32, ppm: i64, spread_us: i64, held: &[(i64, i64)]) -> Vec<(i64, u32)> {
        let mut out: Vec<(i64, u32)> = (1..=seconds * 50)
            .map(|seq| {
                let sent = i64::from(seq) * PACKET_US * (1_000_000 - ppm) / 1_000_000;
                let arrives = sent + 3_000 + scatter(seq, spread_us);
                let released =
                    held.iter().find(|&&(from, to)| sent >= from && sent < to).map(|&(_, to)| to);
                (released.map_or(arrives, |to| to.max(arrives)), seq)
            })
            .collect();
        out.sort_by_key(|&(at, seq)| (at, seq));
        out
    }

    /// Steady packets, a 250 ms stall the device keeps pulling through, the held packets in one
    /// burst, then steady again: one underrun, the backlog cut at once, no lasting delay, and
    /// no join sharper than the tone itself.
    #[test]
    fn a_stall_burst_does_not_leave_lasting_delay() {
        let run = play(&trace(3, 0, 0, &[(1_000_000, 1_250_000)]), 3_000_000);
        let (before, after_burst, settled) = (
            run.mean_heard(500_000, 1_000_000),
            run.max_heard(1_260_000, 1_400_000),
            run.mean_heard(1_500_000, 3_000_000),
        );
        eprintln!(
            "stall: {before} ms before, {after_burst} ms at most after the burst, {settled} ms mean over 1.5–3 s; {:?}, max step {:.4} (tone {:.4})",
            run.stats,
            run.max_step,
            tone_step()
        );
        assert_eq!(run.stats.underruns, 1, "the stall itself");
        assert!(after_burst <= 110, "the backlog goes at once: {after_burst} ms");
        assert!(settled <= before + 10, "and no delay stays: {settled} ms against {before} ms");
        assert!(run.max_step < 2.0 * tone_step(), "no click: {}", run.max_step);
    }

    /// A keyframe on the link every 2 s holds the audio behind it for 40 ms, on top of 3 ms of
    /// scatter. Two packets in a hundred are that late, which the 95th percentile calls noise,
    /// so a burst starves the ring; the lateness that starved it is then held for the window,
    /// and the bursts inside it land in the depth. At most one underrun per window, then.
    #[test]
    fn keyframe_bursts_starve_the_ring_at_most_once_a_window() {
        let bursts: Vec<(i64, i64)> =
            (1..10).map(|k| (k * 2_000_000, k * 2_000_000 + 40_000)).collect();
        let run = play(&trace(20, 0, 3_000, &bursts), 20_000_000);
        let mean = run.mean_heard(3_000_000, 20_000_000);
        eprintln!(
            "keyframe bursts: {mean} ms mean, {} ms at most after 3 s; {:?}, max step {:.4}",
            run.max_heard(3_000_000, 20_000_000),
            run.stats,
            run.max_step
        );
        let windows = 20_000_000 / JITTER_WINDOW_US;
        assert!(run.stats.underruns <= u64::try_from(windows).unwrap(), "bounded: {:?}", run.stats);
        assert!(mean <= 100, "{mean} ms");
        assert!(run.max_step < 2.0 * tone_step(), "no click: {}", run.max_step);
    }

    /// A worker clock 100 ppm fast, then one 100 ppm slow, for two minutes each: the depth is
    /// held by dropping or repeating a slice now and then, never by an underrun or a growing
    /// delay.
    #[test]
    fn clock_drift_is_absorbed_by_slices() {
        for ppm in [100, -100] {
            let run = play(&trace(120, ppm, 1_000, &[]), 120_000_000);
            let (early, late) =
                (run.mean_heard(5_000_000, 15_000_000), run.mean_heard(110_000_000, 120_000_000));
            eprintln!(
                "drift {ppm:+} ppm: {early} ms early, {late} ms late; {:?}, max step {:.4}",
                run.stats, run.max_step
            );
            assert_eq!(run.stats.underruns, 0, "{ppm:+} ppm: {:?}", run.stats);
            assert!((late - early).abs() <= 10, "{ppm:+} ppm: {early} ms → {late} ms");
            if ppm > 0 {
                assert!(
                    run.stats.trimmed > Duration::ZERO,
                    "a fast worker is trimmed: {:?}",
                    run.stats
                );
            } else {
                assert!(
                    run.stats.stretched > Duration::ZERO,
                    "a slow worker is stretched: {:?}",
                    run.stats
                );
            }
            assert!(run.max_step < 2.0 * tone_step(), "no click: {}", run.max_step);
        }
    }

    /// Muted, the host's gate closes for 400 ms and its sequence clock stands still; sound comes
    /// back after the unmute. Nothing ran dry to flag the gate, so without a fresh estimate
    /// every packet after it reads 400 ms late and the ring is cut to its floor on each one.
    #[test]
    fn a_gate_while_muted_leaves_no_lasting_cuts() {
        let gap_us = 400_000;
        let arrivals: Vec<(i64, u32)> = trace(6, 0, 1_000, &[])
            .into_iter()
            .map(|(at, seq)| if seq > 125 { (at + gap_us, seq) } else { (at, seq) })
            .collect();
        let end_us = 6_000_000 + gap_us;
        let run = play_muted(&arrivals, end_us, (2_000_000, 3_000_000));
        let settled = run.mean_heard(4_000_000, end_us);
        eprintln!(
            "gate while muted: {settled} ms mean from 4 s; {:?}, max step {:.4}",
            run.stats, run.max_step
        );
        assert_eq!(run.stats.underruns, 0, "{:?}", run.stats);
        assert!(run.stats.trimmed <= Duration::from_millis(50), "no lasting cuts: {:?}", run.stats);
        assert!(settled <= 70, "{settled} ms");
        assert!(run.max_step < 2.0 * tone_step(), "no click at the mute: {}", run.max_step);
    }

    /// A packet late by more than a packet after a quiet one is the host's gate reopening,
    /// even when the ring never ran dry to say so: the estimate starts over.
    #[test]
    fn a_late_packet_after_silence_starts_a_new_estimate() {
        let epoch = Instant::now();
        let at = |ms: u64| epoch + Duration::from_millis(ms);
        let mut playout = Playout::default();
        let silent = vec![0.0_f32; PACKET_SAMPLES];
        for seq in 1..=20_u32 {
            playout.push(&silent, Arrival { seq, at: at(u64::from(seq) * 20) });
        }
        playout.push(&tone(21), Arrival { seq: 21, at: at(21 * 20 + 60) });
        assert_eq!(playout.jitter.window.len(), 1, "a fresh estimate");
        // After sound the same lateness is lateness.
        playout.push(&tone(22), Arrival { seq: 22, at: at(22 * 20 + 60) });
        playout.push(&tone(23), Arrival { seq: 23, at: at(23 * 20 + 120) });
        assert_eq!(playout.jitter.window.len(), 3, "kept");
    }

    /// The ring runs dry after silence: that is the host's gate closing, not an underrun, and
    /// the next sound, whose sequence clock stood still meanwhile, starts a fresh estimate.
    #[test]
    fn the_silence_gate_is_not_an_underrun() {
        let epoch = Instant::now();
        let at = |ms: u64| epoch + Duration::from_millis(ms);
        let mut playout = Playout::default();
        let mut out = vec![0.0_f32; BUFFER_SAMPLES];
        let silent = vec![0.0_f32; PACKET_SAMPLES];
        for seq in 1..=20_u32 {
            playout.push(&silent, Arrival { seq, at: at(u64::from(seq) * 20) });
            playout.pull(&mut out);
            playout.pull(&mut out);
        }
        for _ in 0..10 {
            playout.pull(&mut out);
        }
        assert_eq!(playout.stats().underruns, 0, "{:?}", playout.stats());
        // Sound again 5 s later on the next sequence number: no 5 s of lateness in the estimate.
        playout.push(&tone(21), Arrival { seq: 21, at: at(5_420) });
        assert_eq!(playout.jitter.window.len(), 1, "a fresh estimate");
        assert_eq!(playout.stats().jitter, Duration::ZERO);
    }
}
