//! Opus over `AudioToolbox`.
//!
//! The system encoder and decoder behind `AudioConverter`, and an output audio unit for
//! playback that pulls from a jitter ring on the device's I/O thread. No third-party codec:
//! Apple ships Opus in the OS and the same calls work on macOS and iOS.
//!
//! Everything is 48 kHz stereo float, interleaved, 20 ms packets (960 frames): the one Opus
//! configuration every decoder accepts, and one packet fits a datagram at any sane bitrate.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
use objc2_audio_toolbox::kAudioUnitSubType_DefaultOutput;
use objc2_audio_toolbox::{
    AURenderCallbackStruct, AudioComponentDescription, AudioComponentFindNext,
    AudioComponentInstanceDispose, AudioComponentInstanceNew, AudioConverterDispose,
    AudioConverterFillComplexBuffer, AudioConverterGetProperty, AudioConverterNew,
    AudioConverterRef, AudioConverterSetProperty, AudioOutputUnitStart, AudioOutputUnitStop,
    AudioUnit, AudioUnitInitialize, AudioUnitRenderActionFlags, AudioUnitSetProperty,
    AudioUnitUninitialize, kAudioComponentErr_UnsupportedType, kAudioConverterEncodeBitRate,
    kAudioConverterPropertyMaximumOutputPacketSize, kAudioUnitManufacturer_Apple,
    kAudioUnitProperty_MaximumFramesPerSlice, kAudioUnitProperty_SetRenderCallback,
    kAudioUnitProperty_StreamFormat, kAudioUnitScope_Global, kAudioUnitScope_Input,
    kAudioUnitType_Output,
};
use objc2_core_audio_types::{
    AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioStreamPacketDescription,
    AudioTimeStamp, kAudioFormatFlagsNativeFloatPacked, kAudioFormatLinearPCM, kAudioFormatOpus,
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
/// Samples per frame, one a channel.
const SAMPLES_PER_FRAME: usize = CHANNELS as usize;
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
            // Encoded in place: `make_contiguous` only moves samples once the queue has wrapped.
            let frame = self.pending.make_contiguous().get(..PACKET_SAMPLES).unwrap_or_default();
            let n = encode_one(&mut self.converter, &mut self.packet, frame)?;
            self.pending.drain(..PACKET_SAMPLES);
            if let Some(packet) = self.packet.get(..n).filter(|p| !p.is_empty()) {
                out(packet);
            }
        }
        Ok(())
    }
}

/// Encode one 20 ms packet of `frame` into `packet`; the packet's length, 0 when the converter held
/// it back.
fn encode_one(
    converter: &mut Converter,
    packet: &mut [u8],
    frame: &[f32],
) -> Result<usize, CodecError> {
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
            mDataByteSize: u32_of(packet.len()),
            mData: packet.as_mut_ptr().cast::<c_void>(),
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
            converter.0,
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
/// Part of the decoder's [`Feed`], never touched by the device's callback.
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

/// Samples waiting to be played, between the thread that decodes packets and the device's
/// I/O thread: a single-producer, single-consumer ring of atomics, allocated once.
///
/// The device's side ([`Ring::pull`]) is wait-free: a handful of atomic loads and stores, one
/// compare-and-swap it never retries, and a copy. It takes no lock, so a decoder descheduled
/// mid-packet cannot hold up a buffer. Everything that steers the depth is the decoder's
/// ([`Steer`]), done to a packet before it is published or to samples the device is not
/// reading.
///
/// Ownership, in positions that count samples since the ring was made:
/// - `write` is the decoder's. Samples below it are published; the decoder writes past it and
///   publishes with a release store.
/// - `read` is the device's. The decoder writes nothing at or past `read + RING_SAMPLES`.
/// - `run` says whether the device plays: its epoch times two, plus one while playing. The decoder
///   starts a run (a new, odd epoch word) and stops one when the stream is muted; the device stops
///   one when it runs dry. The epoch keeps a stale stop from ending a newer run.
/// - `from` is where the current run starts, the decoder's: a new run begins there, and a stopped
///   ring frees everything below it. While stopped, the samples from `from` on are the decoder's to
///   fade in.
///
/// Samples are `f32` bits in relaxed `AtomicU32`s, which on Apple silicon are the plain loads
/// and stores a copy would make: the ordering comes from `write`, `read` and `run`, and a race
/// the protocol rules out could not be undefined behaviour even if it happened.
struct Ring {
    samples: Box<[AtomicU32]>,
    write: AtomicUsize,
    read: AtomicUsize,
    from: AtomicUsize,
    run: AtomicU32,
    /// The device thread's own state, atomics so the callback needs only a shared reference:
    /// the `run` word it last played under, the last frame it gave the device, and how many
    /// frames of the ramp from that frame down to silence are still to play. A ring that runs
    /// dry at a buffer's edge has nothing left to fade, so the fade continues from what was
    /// last heard.
    seen: AtomicU32,
    tail: [AtomicU32; 2],
    ramp: AtomicUsize,
}

/// The decoder's side of the ring: when to start, and the depth, steered from the level the
/// device's `read` shows.
///
/// The ring starts, and restarts after running dry, only once it holds the target. The depth
/// an arrival reveals is the ring's level before the packet goes in plus how late the packet
/// ran: a late packet finds the ring lower by exactly its lateness, so a clump of late packets
/// reads the same depth as an on-time stream and costs nothing. A depth that stays off the
/// target is corrected one [`SLICE_FRAMES`] slice per packet, dropped from the arriving packet
/// or played twice in it with a [`FADE_FRAMES`] crossfade, which is how a worker clock running
/// ±100 ppm off the device's is absorbed. A depth past the target by more than [`BURST_US`] is
/// the backlog a stall releases, and is cut from each packet as it arrives, crossfaded, rather
/// than kept as lasting delay.
#[derive(Debug)]
struct Steer {
    /// A run this side started and has not seen end.
    playing: bool,
    /// The last run's epoch (see [`Ring`]).
    epoch: u32,
    /// The decoder's copies of `write` and `from`.
    write: usize,
    from: usize,
    /// Cutting a stall's backlog (see [`Steer::converge`]).
    cutting: bool,
    /// Depths the recent arrivals read, microseconds, newest last.
    recent: VecDeque<i64>,
    /// The last packet pushed had a sample above the host's silence floor.
    last_loud: bool,
    /// The ring ran dry under sound: the next packet is the late one.
    starved: bool,
    /// The ring ran dry after silence, or the stream was muted (see [`Since::gated`]).
    gated: bool,
    /// The arriving packet, sliced or stretched before it is published.
    packet: Vec<f32>,
    underruns: u64,
    trimmed_frames: u64,
    stretched_frames: u64,
}

/// The decoder's half of playback: the jitter estimate and the ring's steering. Only the thread
/// that decodes packets takes it, never the device's callback, so neither the sort behind the
/// percentile nor a slice's crossfade can hold up a buffer.
#[derive(Debug, Default)]
struct Feed {
    jitter: Jitter,
    steer: Steer,
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
}

impl Feed {
    /// Time one decoded packet against what the ring saw since the last one, then queue it.
    fn push(&mut self, ring: &Ring, pcm: &[f32], arrival: Arrival) {
        let since = self.steer.since(ring);
        let timing = self.jitter.time(arrival, since);
        self.steer.push(ring, pcm, timing);
    }

    /// A packet that is timed but not played (the stream is muted).
    fn hold(&mut self, ring: &Ring, arrival: Arrival) {
        let since = self.steer.since(ring);
        let _timing = self.jitter.time(arrival, since);
        self.steer.hold(ring);
    }

    /// Stand-ins for lost packets: they keep the time the gap took, and read no depth.
    fn conceal(&mut self, ring: &Ring, pcm: &[f32]) {
        self.steer.publish(ring, pcm);
    }

    /// What playback has done, from the ring's counts and the estimate's depth.
    fn stats(&mut self, ring: &Ring) -> PlayoutStats {
        self.steer.observe(ring);
        let frames = |n: u64| {
            let us = n.saturating_mul(1_000_000).checked_div(u64::from(SAMPLE_RATE));
            Duration::from_micros(us.unwrap_or(0))
        };
        PlayoutStats {
            underruns: self.steer.underruns,
            trimmed: frames(self.steer.trimmed_frames),
            stretched: frames(self.steer.stretched_frames),
            target: duration_us(self.jitter.target_us),
            jitter: duration_us(self.jitter.lateness_us),
        }
    }
}

/// Whether a `run` word is a playing run's (see [`Ring`]).
const fn playing(run: u32) -> bool {
    run & 1 == 1
}

/// The `run` word of the same run, stopped.
const fn stopped(run: u32) -> u32 {
    run & !1
}

impl std::fmt::Debug for Ring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ring")
            .field("run", &self.run.load(Ordering::Relaxed))
            .field("queued", &self.queued())
            .finish_non_exhaustive()
    }
}

impl Ring {
    /// An empty, stopped ring of [`RING_SAMPLES`] samples.
    fn new() -> Self {
        Self {
            samples: std::iter::repeat_with(|| AtomicU32::new(0)).take(RING_SAMPLES).collect(),
            write: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
            from: AtomicUsize::new(0),
            run: AtomicU32::new(0),
            seen: AtomicU32::new(0),
            tail: [AtomicU32::new(0), AtomicU32::new(0)],
            ramp: AtomicUsize::new(0),
        }
    }

    /// The `len` cells from position `pos` on, wrapping at the ring's end.
    fn cells(&self, pos: usize, len: usize) -> impl Iterator<Item = &AtomicU32> {
        let start = pos & RING_MASK;
        let first = len.min(RING_SAMPLES.saturating_sub(start));
        let head = self.samples.get(start..start.saturating_add(first)).unwrap_or_default();
        let wrapped = self.samples.get(..len.saturating_sub(first)).unwrap_or_default();
        head.iter().chain(wrapped)
    }

    /// Samples queued, both channels: published, and neither played nor discarded.
    fn queued(&self) -> usize {
        let from = self.from.load(Ordering::Acquire);
        let read = self.read.load(Ordering::Acquire);
        self.write.load(Ordering::Acquire).saturating_sub(read.max(from))
    }

    /// A run is playing.
    fn playing(&self) -> bool {
        playing(self.run.load(Ordering::Acquire))
    }

    /// Fill `out` for the device; the device's side, wait-free.
    ///
    /// A run plays what is published. One that runs dry stops itself and ramps from the last
    /// sample it played down to silence, and the ring plays again once the decoder has filled
    /// it to the target and started a new run.
    fn pull(&self, out: &mut [f32]) {
        let run = self.run.load(Ordering::Acquire);
        let seen = self.seen.load(Ordering::Relaxed);
        let mut ramp = self.ramp.load(Ordering::Relaxed);
        let mut have = 0;
        if playing(run) {
            let mut read = self.read.load(Ordering::Relaxed);
            if run != seen {
                // A new run starts at its front; what came before it was discarded. The front
                // was stored before the run, whose acquire load above makes it visible.
                read = read.max(self.from.load(Ordering::Relaxed));
            }
            let write = self.write.load(Ordering::Acquire);
            let n = write.saturating_sub(read).min(out.len());
            for (slot, cell) in out.iter_mut().zip(self.cells(read, n)) {
                *slot = f32::from_bits(cell.load(Ordering::Relaxed));
            }
            if self.run.load(Ordering::Acquire) == run {
                have = n;
                self.read.store(read.saturating_add(n), Ordering::Release);
                if n < out.len() {
                    // Dry. A failed swap is the decoder's mute stopping the run first.
                    let _stopped = self.run.compare_exchange(
                        run,
                        stopped(run),
                        Ordering::AcqRel,
                        Ordering::Relaxed,
                    );
                    self.seen.store(stopped(run), Ordering::Relaxed);
                    ramp = FADE_FRAMES;
                } else {
                    self.seen.store(run, Ordering::Relaxed);
                }
            } else {
                // Muted while this render copied. A `write` the copy saw past the mute would
                // show the mute in the second load of `run`, so what was copied may be the
                // next run's: none of it plays, and the decoder has discarded the rest.
                self.seen.store(stopped(run), Ordering::Relaxed);
                ramp = FADE_FRAMES;
            }
        } else {
            if playing(seen) {
                // Muted since the last render.
                ramp = FADE_FRAMES;
            }
            self.seen.store(run, Ordering::Relaxed);
            // What the stop discarded is free for the decoder to write over.
            let from = self.from.load(Ordering::Acquire);
            if from > self.read.load(Ordering::Relaxed) {
                self.read.store(from, Ordering::Release);
            }
        }
        let channels = CHANNELS as usize;
        if let Some(last) = out.get(..have).and_then(|played| played.chunks_exact(channels).last())
        {
            for (tail, &sample) in self.tail.iter().zip(last) {
                tail.store(sample.to_bits(), Ordering::Relaxed);
            }
        }
        let tail = self.tail.each_ref().map(|t| f32::from_bits(t.load(Ordering::Relaxed)));
        for frame in out.get_mut(have..).unwrap_or_default().chunks_mut(channels) {
            #[expect(clippy::cast_precision_loss, reason = "a few hundred frames")]
            let gain = ramp as f32 / (FADE_FRAMES as f32 + 1.0);
            ramp = ramp.saturating_sub(1);
            for (sample, &tail) in frame.iter_mut().zip(&tail) {
                *sample = tail * gain;
            }
        }
        self.ramp.store(ramp, Ordering::Relaxed);
    }
}

impl Default for Steer {
    fn default() -> Self {
        Self {
            playing: false,
            epoch: 0,
            write: 0,
            from: 0,
            cutting: false,
            recent: VecDeque::with_capacity(RECENT + 1),
            last_loud: false,
            starved: false,
            gated: false,
            packet: Vec::with_capacity(PACKET_ROOM),
            underruns: 0,
            trimmed_frames: 0,
            stretched_frames: 0,
        }
    }
}

impl Steer {
    /// Microseconds of audio in the ring.
    fn level_us(ring: &Ring) -> i64 {
        frames_us(ring.queued() / SAMPLES_PER_FRAME)
    }

    /// The `run` word of this side's last run, playing.
    const fn run(&self) -> u32 {
        self.epoch.wrapping_shl(1) | 1
    }

    /// Take in a run the device ended by running dry since the last look: it is stopped, and
    /// the next packet carries the mark.
    fn observe(&mut self, ring: &Ring) {
        if !self.playing || ring.playing() {
            return;
        }
        self.playing = false;
        self.recent.clear();
        if self.last_loud {
            self.underruns = self.underruns.saturating_add(1);
            self.starved = true;
        } else {
            self.gated = true;
        }
    }

    /// Hand what the ring saw since the last packet to the next one's timing.
    fn since(&mut self, ring: &Ring) -> Since {
        self.observe(ring);
        let since = Since { starved: self.starved, gated: self.gated, quiet: !self.last_loud };
        self.starved = false;
        self.gated = false;
        since
    }

    /// Queue one decoded packet and steer the depth.
    fn push(&mut self, ring: &Ring, pcm: &[f32], timing: Timing) {
        // A run that ran dry since `since` is filled again from this packet on; the next
        // packet carries the mark.
        self.observe(ring);
        if timing.rebased {
            self.recent.clear();
        }
        let before = Self::level_us(ring);
        self.last_loud = pcm.iter().any(|s| s.abs() > LOUD);
        if self.playing {
            let level = before.saturating_add(frames_us(pcm.len() / SAMPLES_PER_FRAME));
            let mut packet = std::mem::take(&mut self.packet);
            packet.clear();
            packet.extend_from_slice(pcm);
            self.converge(
                &mut packet,
                before.saturating_add(timing.lateness),
                timing.target_us,
                level,
            );
            self.publish(ring, &packet);
            self.packet = packet;
        } else {
            self.publish(ring, pcm);
            if Self::level_us(ring).saturating_add(timing.lateness).saturating_sub(PACKET_US)
                >= timing.target_us
            {
                // Full enough that the next packet, on time, finds the target.
                self.start(ring);
            }
        }
    }

    /// Write `pcm` past `write` and publish it. What finds no room, which only a device that
    /// has stopped rendering leaves, is dropped and counted as trimmed.
    fn publish(&mut self, ring: &Ring, pcm: &[f32]) {
        let channels = CHANNELS as usize;
        let read = ring.read.load(Ordering::Acquire);
        let room = RING_SAMPLES.saturating_sub(self.write.saturating_sub(read));
        let fits = pcm.len().min(room);
        let n = fits.saturating_sub(fits.checked_rem(channels).unwrap_or(0));
        for (cell, &sample) in ring.cells(self.write, n).zip(pcm) {
            cell.store(sample.to_bits(), Ordering::Relaxed);
        }
        self.write = self.write.saturating_add(n);
        ring.write.store(self.write, Ordering::Release);
        let dropped = pcm.len().saturating_sub(n) / SAMPLES_PER_FRAME;
        self.trimmed_frames = self.trimmed_frames.saturating_add(dropped as u64);
    }

    /// Start a run from the front of what is queued, ramped up from silence. The device reads
    /// none of it until the run's word is stored.
    fn start(&mut self, ring: &Ring) {
        let channels = CHANNELS as usize;
        let front = ring.read.load(Ordering::Acquire).max(self.from);
        let fade = FADE_FRAMES.saturating_mul(channels).min(self.write.saturating_sub(front));
        for (i, cell) in ring.cells(front, fade).enumerate() {
            let sample = f32::from_bits(cell.load(Ordering::Relaxed));
            let faded = sample * fade_in_weight(i / SAMPLES_PER_FRAME);
            cell.store(faded.to_bits(), Ordering::Relaxed);
        }
        self.from = front;
        ring.from.store(front, Ordering::Relaxed);
        self.epoch = self.epoch.wrapping_add(1);
        ring.run.store(self.run(), Ordering::Release);
        self.playing = true;
        self.recent.clear();
    }

    /// The stream is muted: the run stops, what is queued is discarded, and the ring fills to
    /// the target again when sound is wanted. The device ramps down from what it last played.
    /// The host's gate may close meanwhile with nothing running dry to show it, so the next
    /// packet starts a new estimate.
    fn hold(&mut self, ring: &Ring) {
        self.observe(ring);
        if self.playing
            && ring
                .run
                .compare_exchange(
                    self.run(),
                    stopped(self.run()),
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                )
                .is_err()
        {
            // The device ran dry first.
            self.observe(ring);
        }
        // Stored after the stop: a device that sees this `from` sees the run stopped too.
        self.from = self.write;
        ring.from.store(self.write, Ordering::Release);
        self.playing = false;
        self.last_loud = false;
        self.gated = true;
    }

    /// Move the depth towards the target, in the arriving `packet`: a stall's backlog at once,
    /// a drift one slice at a time. `level` is the ring's with the whole packet in it.
    fn converge(&mut self, packet: &mut Vec<f32>, depth: i64, target_us: i64, level: i64) {
        let excess = depth.saturating_sub(target_us);
        // A backlog arrives packet by packet, so the cut goes on with each packet until the
        // depth is back at the target. A packet gives all but the fade it joins over.
        self.cutting = excess > BURST_US || (self.cutting && excess > DEADBAND_US);
        if self.cutting {
            let room = level.saturating_sub(KEEP_US).saturating_sub(frames_us(FADE_FRAMES));
            let most = (packet.len() / SAMPLES_PER_FRAME).saturating_sub(FADE_FRAMES);
            let cut = us_frames(excess.min(room)).min(most);
            if cut >= SLICE_FRAMES && drop_frames(packet, cut) {
                self.trimmed_frames = self.trimmed_frames.saturating_add(cut as u64);
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
        let spare = level.saturating_sub(KEEP_US) >= slice.saturating_add(frames_us(FADE_FRAMES));
        let shift = if mean.saturating_sub(target_us) > DEADBAND_US
            && spare
            && drop_frames(packet, SLICE_FRAMES)
        {
            self.trimmed_frames = self.trimmed_frames.saturating_add(SLICE_FRAMES as u64);
            slice.saturating_neg()
        } else if target_us.saturating_sub(mean) > DEADBAND_US
            && stretch_frames(packet, SLICE_FRAMES)
        {
            self.stretched_frames = self.stretched_frames.saturating_add(SLICE_FRAMES as u64);
            slice
        } else {
            return;
        };
        for d in &mut self.recent {
            *d = d.saturating_add(shift);
        }
    }
}

/// Drop `n` frames from the front of `buf`, crossfading what came before them into what follows.
fn drop_frames(buf: &mut Vec<f32>, n: usize) -> bool {
    let channels = CHANNELS as usize;
    let (cut, fade) = (n.saturating_mul(channels), FADE_FRAMES.saturating_mul(channels));
    if buf.len() < cut.saturating_add(fade) {
        return false;
    }
    for k in 0..fade {
        let w = fade_in_weight(k / SAMPLES_PER_FRAME);
        let outgoing = buf.get(k).copied().unwrap_or(0.0);
        if let Some(incoming) = buf.get_mut(cut.saturating_add(k)) {
            *incoming = incoming.mul_add(w, outgoing * (1.0 - w));
        }
    }
    buf.drain(..cut);
    true
}

/// Play `n` frames at the front of `buf` twice: after them, crossfade back to its start. `n` is
/// at least a fade long, and `buf` has room for `n` more frames.
fn stretch_frames(buf: &mut Vec<f32>, n: usize) -> bool {
    let channels = CHANNELS as usize;
    let (span, fade) = (n.saturating_mul(channels), FADE_FRAMES.saturating_mul(channels));
    let len = buf.len();
    if len < span.saturating_add(fade) || span < fade {
        return false;
    }
    // The packet moves up a span, leaving its first span in place ahead of itself. Its first
    // `fade` samples, now a span in, are crossfaded into what follows the span the first time,
    // a span later again. What follows the crossfade is the packet from the end of the fade.
    buf.resize(len.saturating_add(span), 0.0);
    buf.copy_within(..len, span);
    for k in 0..fade {
        let w = fade_in_weight(k / SAMPLES_PER_FRAME);
        let outgoing = buf.get(span.saturating_mul(2).saturating_add(k)).copied().unwrap_or(0.0);
        if let Some(incoming) = buf.get_mut(span.saturating_add(k)) {
            *incoming = incoming.mul_add(w, outgoing * (1.0 - w));
        }
    }
    true
}

/// The output unit's subtype: on macOS the system's default output, which follows the device
/// the user picks.
#[cfg(target_os = "macos")]
const OUTPUT_SUBTYPE: u32 = kAudioUnitSubType_DefaultOutput;
/// The output unit's subtype on iOS: `kAudioUnitSubType_RemoteIO` ('rioc'), from
/// `AudioToolbox/AUComponent.h`. It sits in the header's iOS-only branch, which the objc2
/// bindings, generated from the macOS SDK, do not carry.
#[cfg(not(target_os = "macos"))]
const OUTPUT_SUBTYPE: u32 = 0x7269_6f63;
/// Most frames a render may ask for: iOS asks up to 4 096 at a time with the screen locked, and a
/// unit asked for more than its maximum fails the render.
const MAX_FRAMES_PER_SLICE: u32 = 4096;
/// Samples the ring holds, allocated once: 341 ms, past the deepest it gets (the 120 ms ceiling
/// and a packet more while it fills, a stall's backlog as it is being cut, 60 ms of
/// concealment). A power of two, so a position finds its cell with a mask.
const RING_SAMPLES: usize = 1 << 15;
const RING_MASK: usize = RING_SAMPLES - 1;
const _: () = assert!(RING_SAMPLES >= PACKET_SAMPLES * 13, "260 ms at least");
/// Room for the largest packet a decoder returns, played with a slice twice.
const PACKET_ROOM: usize = PACKET_SAMPLES * 3 + SLICE_FRAMES * SAMPLES_PER_FRAME;

/// An output audio unit playing interleaved stereo float at 48 kHz from the decoder's ring.
///
/// Its render callback runs on the device's I/O thread and fills each I/O buffer straight from
/// the ring, so nothing is queued between the ring and the device.
pub struct Player {
    unit: AudioUnit,
    ring: Arc<Ring>,
    /// Taken by whoever pushes packets, never by the callback. Boxed so a player stays small
    /// beside the other states of a stream's audio.
    feed: Box<Mutex<Feed>>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player").finish_non_exhaustive()
    }
}

// SAFETY: an audio unit's property and start/stop calls may come from any thread; the ring is
// atomics only, and the decoder's side of it is behind a mutex.
unsafe impl Send for Player {}

/// `AURenderCallback`: fill the device's I/O buffer from the ring (see [`Ring::pull`]).
///
/// Wait-free: no lock, no allocation, no system call. It never waits on the thread that decodes
/// packets, whatever that thread is doing (see [`Ring`]).
unsafe extern "C-unwind" fn render(
    user: NonNull<c_void>,
    _flags: NonNull<AudioUnitRenderActionFlags>,
    _stamp: NonNull<AudioTimeStamp>,
    _bus: u32,
    frames: u32,
    data: *mut AudioBufferList,
) -> i32 {
    // SAFETY: `user` is the `Arc<Ring>` leaked into the unit when it was opened, and reclaimed
    // in `Drop` only after the unit is stopped and disposed, which ends all renders.
    let ring = unsafe { user.cast::<Ring>().as_ref() };
    // SAFETY: AudioUnit rule: `ioData` is the unit's own list, valid for this call. The stream
    // format is interleaved, so the list has one buffer.
    let Some(list) = (unsafe { data.as_mut() }) else { return 0 };
    if list.mNumberBuffers == 0 {
        return 0;
    }
    let Some(buffer) = list.mBuffers.first_mut() else { return 0 };
    let Some(samples) = NonNull::new(buffer.mData.cast::<f32>()) else { return 0 };
    let capacity = usize::try_from(buffer.mDataByteSize).unwrap_or(0) / SAMPLE_BYTES;
    let want = usize::try_from(frames).unwrap_or(0).saturating_mul(CHANNELS as usize).min(capacity);
    // SAFETY: the buffer holds `mDataByteSize` bytes of f32 samples, and `want` is within them.
    let out = unsafe { std::slice::from_raw_parts_mut(samples.as_ptr(), want) };
    ring.pull(out);
    buffer.mDataByteSize = u32_of(want.saturating_mul(SAMPLE_BYTES));
    0
}

impl Player {
    /// Open the output unit and start it (it plays silence until samples arrive).
    pub fn new() -> Result<Self, CodecError> {
        let player = Self::open()?;
        // SAFETY: AudioToolbox rule: start an initialised output unit.
        check("AudioOutputUnitStart", unsafe { AudioOutputUnitStart(player.unit) })?;
        Ok(player)
    }

    /// The output unit found, given the ring's format and callback, and initialised, but not
    /// started: nothing reaches the device yet.
    fn open() -> Result<Self, CodecError> {
        let mut description = AudioComponentDescription {
            componentType: kAudioUnitType_Output,
            componentSubType: OUTPUT_SUBTYPE,
            componentManufacturer: kAudioUnitManufacturer_Apple,
            componentFlags: 0,
            componentFlagsMask: 0,
        };
        // SAFETY: AudioToolbox rule: a null component searches from the first; the description
        // is valid for the call.
        let component =
            unsafe { AudioComponentFindNext(ptr::null_mut(), NonNull::from(&mut description)) };
        if component.is_null() {
            return Err(CodecError::Os {
                call: "AudioComponentFindNext",
                status: kAudioComponentErr_UnsupportedType,
            });
        }
        let mut unit: AudioUnit = ptr::null_mut();
        // SAFETY: AudioToolbox rule: a component the system returned and an out pointer.
        let status = unsafe { AudioComponentInstanceNew(component, NonNull::from(&mut unit)) };
        check("AudioComponentInstanceNew", status)?;
        let ring = Arc::new(Ring::new());
        let user = Arc::into_raw(Arc::clone(&ring)).cast_mut().cast::<c_void>();
        // From here `Drop` disposes the unit and reclaims `user`.
        let player = Self { unit, ring, feed: Box::default() };
        player.set(kAudioUnitProperty_StreamFormat, kAudioUnitScope_Input, &pcm_format())?;
        player.set(
            kAudioUnitProperty_MaximumFramesPerSlice,
            kAudioUnitScope_Global,
            &MAX_FRAMES_PER_SLICE,
        )?;
        let callback = AURenderCallbackStruct { inputProc: Some(render), inputProcRefCon: user };
        player.set(kAudioUnitProperty_SetRenderCallback, kAudioUnitScope_Input, &callback)?;
        // SAFETY: AudioToolbox rule: initialise a configured unit once, before starting it.
        check("AudioUnitInitialize", unsafe { AudioUnitInitialize(player.unit) })?;
        Ok(player)
    }

    /// Set one property on the unit's output element (element 0).
    fn set<T>(&self, property: u32, scope: u32, value: &T) -> Result<(), CodecError> {
        // SAFETY: AudioToolbox rule: a live unit, and a value of the property's documented type
        // and stated size, which the unit copies during the call.
        let status = unsafe {
            AudioUnitSetProperty(
                self.unit,
                property,
                scope,
                0,
                ptr::from_ref(value).cast::<c_void>(),
                u32_of(size_of::<T>()),
            )
        };
        check("AudioUnitSetProperty", status)
    }

    /// Queue one decoded packet, timed by its arrival (see [`Arrival`]).
    pub fn push(&self, samples: &[f32], arrival: Arrival) {
        self.feed.lock().push(&self.ring, samples, arrival);
    }

    /// Queue stand-ins for lost packets (see [`Conceal`]); they take the gap's time.
    pub fn conceal(&self, samples: &[f32]) {
        self.feed.lock().conceal(&self.ring, samples);
    }

    /// A packet that arrived but is not to be heard (the stream is muted): its timing still
    /// counts, and what is queued goes.
    pub fn hold(&self, arrival: Arrival) {
        self.feed.lock().hold(&self.ring, arrival);
    }

    /// What playback has done so far.
    #[must_use]
    pub fn stats(&self) -> PlayoutStats {
        self.feed.lock().stats(&self.ring)
    }

    /// Samples waiting to be played, both channels.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.ring.queued()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // SAFETY: AudioToolbox rule: stopping an output unit from outside its I/O thread returns
        // once the render in flight has finished; stopping one that never started only returns
        // an error.
        let _stopped = unsafe { AudioOutputUnitStop(self.unit) };
        // SAFETY: AudioToolbox rule: a live unit; one never initialised only returns an error.
        let _uninitialised = unsafe { AudioUnitUninitialize(self.unit) };
        // SAFETY: AudioToolbox rule: dispose once, last; a disposed unit renders no more.
        let _disposed = unsafe { AudioComponentInstanceDispose(self.unit) };
        let user = Arc::as_ptr(&self.ring);
        // SAFETY: this is the clone `open` leaked with `Arc::into_raw`; no render runs any more.
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

    /// Silence: this opens the machine's real output, which may be carrying someone's audio.
    #[test]
    #[expect(clippy::disallowed_methods, reason = "a test waiting on the audio thread")]
    fn player_starts_and_drains() {
        let player = Player::new().unwrap();
        let at = Instant::now();
        for seq in 1..=3 {
            let at = at + Duration::from_millis(u64::from(seq) * 20);
            player.push(&vec![0.0; PACKET_SAMPLES], Arrival { seq, at });
        }
        assert!(player.queued() <= PACKET_SAMPLES * 3);
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(player.queued(), 0);
    }

    /// The output unit takes the ring's format and callback and initialises, and is not
    /// started, so nothing reaches the device. The ring is allocated whole, stopped and empty.
    #[test]
    fn the_output_unit_opens_without_starting() {
        let player = Player::open().unwrap();
        assert_eq!(player.ring.samples.len(), RING_SAMPLES);
        assert!(!player.ring.playing());
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
    struct Playout {
        feed: Feed,
        ring: Ring,
    }

    impl Default for Playout {
        fn default() -> Self {
            Self { feed: Feed::default(), ring: Ring::new() }
        }
    }

    impl Playout {
        fn push(&mut self, pcm: &[f32], arrival: Arrival) {
            self.feed.push(&self.ring, pcm, arrival);
        }

        fn hold(&mut self, arrival: Arrival) {
            self.feed.hold(&self.ring, arrival);
        }

        fn pull(&self, out: &mut [f32]) {
            self.ring.pull(out);
        }

        fn stats(&mut self) -> PlayoutStats {
            self.feed.stats(&self.ring)
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
        /// How far behind the worker the ring holds the listener at each device pull, ms. The
        /// callback writes straight into the device's I/O buffer, so the ring is all the player
        /// adds; the device's own terms come on top (17.3 ms on the Mac Studio's speakers,
        /// `tests/audio_latency.rs`).
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

    /// Frames a render asks for: the I/O buffer of the Mac Studio's speakers and most Mac
    /// output devices (`tests/audio_latency.rs`).
    const DEVICE_FRAMES: usize = 512;

    /// Play `arrivals` (arrival µs, sequence), in arrival order, against a device that renders
    /// [`DEVICE_FRAMES`] at a time on its own 48 kHz clock, until `end_us`. Driven through the
    /// same `push` and `pull` the player runs; the clocks are the trace's, so nothing sleeps.
    fn play(arrivals: &[(i64, u32)], end_us: i64) -> Run {
        play_with(arrivals, end_us, (0, 0), DEVICE_FRAMES)
    }

    /// [`play`], with the packets arriving in `muted` (from, to) held rather than played.
    fn play_muted(arrivals: &[(i64, u32)], end_us: i64, muted: (i64, i64)) -> Run {
        play_with(arrivals, end_us, muted, DEVICE_FRAMES)
    }

    /// [`play_muted`] against a device that renders `frames` at a time.
    fn play_with(arrivals: &[(i64, u32)], end_us: i64, muted: (i64, i64), frames: usize) -> Run {
        let epoch = Instant::now();
        let mut playout = Playout::default();
        let mut run = Run::default();
        let mut out = vec![0.0_f32; frames * CHANNELS as usize];
        let mut last = 0.0_f32;
        let mut next = arrivals.iter().peekable();
        // The device's clock in frames, so a render period that is not a whole number of
        // microseconds does not drift against the worker's.
        let mut rendered = 0_usize;
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
            let queued = playout.ring.queued() / CHANNELS as usize;
            run.heard.push((pull_at, frames_us(queued) / 1000));
            rendered += frames;
            pull_at = 5_000 + frames_us(rendered);
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
        assert!(after_burst <= 80, "the backlog goes at once: {after_burst} ms");
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
        assert!(mean <= 70, "{mean} ms");
        assert!(run.max_step < 2.0 * tone_step(), "no click: {}", run.max_step);
    }

    /// A worker clock 100 ppm fast, then one 100 ppm slow, for four minutes each: the depth is
    /// held by dropping or repeating a slice now and then, never by an underrun or a growing
    /// delay. Four minutes move a clock 24 ms, past the deadband either side of the target, so
    /// the slow clock too must be corrected by a slice whatever depth it started from.
    #[test]
    fn clock_drift_is_absorbed_by_slices() {
        for ppm in [100, -100] {
            let run = play(&trace(240, ppm, 1_000, &[]), 240_000_000);
            let (early, late) =
                (run.mean_heard(5_000_000, 15_000_000), run.mean_heard(230_000_000, 240_000_000));
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
        assert!(settled <= 40, "{settled} ms");
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
        assert_eq!(playout.feed.jitter.window.len(), 1, "a fresh estimate");
        // After sound the same lateness is lateness.
        playout.push(&tone(22), Arrival { seq: 22, at: at(22 * 20 + 60) });
        playout.push(&tone(23), Arrival { seq: 23, at: at(23 * 20 + 120) });
        assert_eq!(playout.feed.jitter.window.len(), 3, "kept");
    }

    /// The ring runs dry after silence: that is the host's gate closing, not an underrun, and
    /// the next sound, whose sequence clock stood still meanwhile, starts a fresh estimate.
    #[test]
    fn the_silence_gate_is_not_an_underrun() {
        let epoch = Instant::now();
        let at = |ms: u64| epoch + Duration::from_millis(ms);
        let mut playout = Playout::default();
        let mut out = vec![0.0_f32; DEVICE_FRAMES * CHANNELS as usize];
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
        assert_eq!(playout.feed.jitter.window.len(), 1, "a fresh estimate");
        assert_eq!(playout.stats().jitter, Duration::ZERO);
    }

    /// A render asks for the device's I/O buffer and the callback fills it straight from the
    /// ring: on-time packets come out whole and in order, 512 frames at a time, with no
    /// underrun, no slice, and nothing written past the frames asked for.
    #[test]
    fn the_render_callback_plays_the_ring_in_order_at_the_devices_size() {
        let epoch = Instant::now();
        let mut playout = Playout::default();
        // The ring starts on the third packet, and a slice waits for ten depths after that.
        let packets = 12_u32;
        let packet_frames = FRAME_SAMPLES as usize;
        let input: Vec<f32> = (1..=packets).flat_map(tone).collect();
        let samples = DEVICE_FRAMES * CHANNELS as usize;
        // A frame more room than the render asks for, marked, to see the callback stop at it.
        let mut buffer = vec![f32::MAX; samples + CHANNELS as usize];
        let mut output = Vec::new();
        let mut flags = AudioUnitRenderActionFlags::empty();
        // SAFETY: a plain C struct for which all zeroes is a valid value.
        let mut stamp: AudioTimeStamp = unsafe { std::mem::zeroed() };
        let (mut seq, mut rendered) = (1_u32, 0_usize);
        loop {
            // A packet arrives once the worker has captured all of it.
            while seq <= packets && seq as usize * packet_frames <= rendered {
                let at = epoch
                    + Duration::from_micros(
                        frames_us(seq as usize * packet_frames).cast_unsigned(),
                    );
                playout.push(&tone(seq), Arrival { seq, at });
                seq += 1;
            }
            let (playing, level) = (playout.ring.playing(), playout.ring.queued());
            if seq > packets && level < samples {
                break;
            }
            let mut list = AudioBufferList {
                mNumberBuffers: 1,
                mBuffers: [AudioBuffer {
                    mNumberChannels: CHANNELS,
                    mDataByteSize: u32_of(buffer.len() * SAMPLE_BYTES),
                    mData: buffer.as_mut_ptr().cast::<c_void>(),
                }],
            };
            let user = NonNull::from(&playout.ring).cast::<c_void>();
            // SAFETY: called as the unit calls it: the ring outlives the call, and the list's one
            // buffer holds the bytes it states.
            let status = unsafe {
                render(
                    user,
                    NonNull::from(&mut flags),
                    NonNull::from(&mut stamp),
                    0,
                    u32_of(DEVICE_FRAMES),
                    &raw mut list,
                )
            };
            assert_eq!(status, 0);
            assert_eq!(list.mBuffers[0].mDataByteSize as usize, samples * SAMPLE_BYTES);
            assert_eq!(buffer[samples..], [f32::MAX; CHANNELS as usize], "past the frames asked");
            if playing {
                output.extend_from_slice(&buffer[..samples]);
            } else {
                assert!(buffer[..samples].iter().all(|&s| s == 0.0), "silence while filling");
            }
            rendered += DEVICE_FRAMES;
        }
        let stats = playout.stats();
        eprintln!(
            "render callback: {} frames played of {}; {stats:?}",
            output.len() / 2,
            input.len() / 2
        );
        assert_eq!(stats.underruns, 0, "{stats:?}");
        assert_eq!((stats.trimmed, stats.stretched), (Duration::ZERO, Duration::ZERO));
        assert!(output.len() >= input.len() - samples, "{} of {}", output.len(), input.len());
        // The first frames are the start's fade-in; everything after is the input, sample for
        // sample.
        let faded = FADE_FRAMES * CHANNELS as usize;
        assert_eq!(output[faded..], input[faded..output.len()]);
    }

    /// The device's thread never waits on the decoder's. With the decoder's whole side held by
    /// another thread, the render callback still plays what was queued, in order, runs dry,
    /// stops the run and ramps to silence, all while the lock stays held.
    #[test]
    fn the_render_callback_never_waits_on_the_decoder() {
        let player = Player::open().unwrap();
        let epoch = Instant::now();
        // The run starts on the third packet; a sixth, unread, would be cut as a backlog.
        let packets = 5_u32;
        for seq in 1..=packets {
            let at = epoch + Duration::from_millis(u64::from(seq) * 20);
            player.push(&tone(seq), Arrival { seq, at });
        }
        assert!(player.ring.playing());
        let input: Vec<f32> = (1..=packets).flat_map(tone).collect();
        let feed = &*player.feed;
        let user = NonNull::from(&*player.ring).cast::<c_void>();
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let mut output = Vec::new();
        let took = std::thread::scope(|s| {
            s.spawn(move || {
                let _feed = feed.lock();
                held_tx.send(()).unwrap();
                // Let go once the renders are done, or after 5 s if they wait for it.
                let _done = done_rx.recv_timeout(Duration::from_secs(5));
            });
            held_rx.recv().unwrap();
            let started = Instant::now();
            let mut buffer = vec![0.0_f32; DEVICE_FRAMES * CHANNELS as usize];
            while output.len() < input.len() + buffer.len() {
                render_once(user, &mut buffer);
                output.extend_from_slice(&buffer);
            }
            let took = started.elapsed();
            assert!(feed.is_locked(), "held throughout");
            done_tx.send(()).unwrap();
            took
        });
        assert!(took < Duration::from_secs(1), "the renders waited: {took:?}");
        let faded = FADE_FRAMES * CHANNELS as usize;
        assert_eq!(output[faded..input.len()], input[faded..]);
        let last = input[input.len() - 1].abs();
        let ramp: Vec<f32> = output[input.len()..].chunks(2).map(|f| f[0].abs()).collect();
        assert!(ramp[0] < last && ramp.is_sorted_by(|a, b| a >= b), "a ramp down: {ramp:?}");
        assert!(ramp[FADE_FRAMES..].iter().all(|&s| s == 0.0), "then silence");
        assert!(!player.ring.playing(), "the device stopped the run itself");
        assert_eq!(player.stats().underruns, 1, "and the decoder counts it");
    }

    /// A mute stops the run at the next render, which ramps down from what it last played.
    /// What was queued is discarded, and the next run starts from the first packet after it.
    #[test]
    fn a_mute_discards_the_queue_and_the_next_run_starts_after_it() {
        let epoch = Instant::now();
        let at = |seq: u32| epoch + Duration::from_millis(u64::from(seq) * 20);
        let mut playout = Playout::default();
        let mut out = vec![0.0_f32; DEVICE_FRAMES * CHANNELS as usize];
        for seq in 1..=4 {
            playout.push(&tone(seq), Arrival { seq, at: at(seq) });
        }
        playout.pull(&mut out);
        let last = out[out.len() - 2];
        playout.hold(Arrival { seq: 5, at: at(5) });
        assert_eq!(playout.ring.queued(), 0, "discarded");
        playout.pull(&mut out);
        #[expect(clippy::cast_precision_loss, reason = "a fade's length")]
        let first = last * FADE_FRAMES as f32 / (FADE_FRAMES as f32 + 1.0);
        assert!((out[0] - first).abs() < 1e-6, "{} against {first}", out[0]);
        let ramp: Vec<f32> = out.chunks(2).map(|f| f[0].abs()).collect();
        assert!(ramp.is_sorted_by(|a, b| a >= b), "{ramp:?}");
        assert!(ramp[FADE_FRAMES..].iter().all(|&s| s == 0.0));
        for seq in 6..=8 {
            playout.push(&tone(seq), Arrival { seq, at: at(seq) });
        }
        assert!(playout.ring.playing(), "full again");
        playout.pull(&mut out);
        let next = tone(6);
        let faded = FADE_FRAMES * CHANNELS as usize;
        assert_eq!(out[faded..], next[faded..out.len()]);
        let first = next[0] * fade_in_weight(0);
        assert!((out[0] - first).abs() < 1e-6, "{} against {first}", out[0]);
        assert_eq!(playout.stats().underruns, 0);
    }

    /// One call of the render callback on a 512-frame buffer.
    fn render_once(user: NonNull<c_void>, buffer: &mut [f32]) {
        let mut flags = AudioUnitRenderActionFlags::empty();
        // SAFETY: a plain C struct for which all zeroes is a valid value.
        let mut stamp: AudioTimeStamp = unsafe { std::mem::zeroed() };
        let mut list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [AudioBuffer {
                mNumberChannels: CHANNELS,
                mDataByteSize: u32_of(buffer.len() * SAMPLE_BYTES),
                mData: buffer.as_mut_ptr().cast::<c_void>(),
            }],
        };
        // SAFETY: called as the unit calls it: the ring outlives the call, and the list's one
        // buffer holds the bytes it states.
        let status = unsafe {
            render(
                user,
                NonNull::from(&mut flags),
                NonNull::from(&mut stamp),
                0,
                u32_of(DEVICE_FRAMES),
                &raw mut list,
            )
        };
        assert_eq!(status, 0);
    }

    /// What the render callback costs on 512-frame buffers: alone, with the decoder's pushes
    /// between renders on the same thread, and with them on a thread of their own that keeps
    /// the ring a few packets ahead of the renders. `docs/MEASUREMENTS.md`, 2026-09-28.
    /// `cargo xtask bench --filter render_cost` runs it.
    #[test]
    #[ignore = "a measurement; run with `cargo xtask bench`"]
    fn render_cost() {
        const RENDERS: usize = 200_000;
        let epoch = Instant::now();
        let packet = |seq: u32| {
            let at = epoch
                + Duration::from_micros(
                    frames_us(seq as usize * FRAME_SAMPLES as usize).cast_unsigned(),
                );
            (tone(seq), Arrival { seq, at })
        };
        let packets: Vec<(Vec<f32>, Arrival)> = (1..=64).map(packet).collect();
        let arrival = |seq: u32| {
            let at = epoch
                + Duration::from_micros(
                    frames_us(seq as usize * FRAME_SAMPLES as usize).cast_unsigned(),
                );
            Arrival { seq, at }
        };
        let mut buffer = vec![0.0_f32; DEVICE_FRAMES * CHANNELS as usize];

        let bench = slopty_testkit::bench::Bench::new("codec.render_cost");
        let mut playout = Playout::default();
        let mut same = bench.series("same_thread");
        let mut seq = 1_u32;
        for i in 0..RENDERS {
            while seq as usize * FRAME_SAMPLES as usize <= (i + 3) * DEVICE_FRAMES {
                playout.push(&packets[seq as usize % packets.len()].0, arrival(seq));
                seq += 1;
            }
            let user = NonNull::from(&playout.ring).cast::<c_void>();
            same.time(|| render_once(user, &mut buffer));
        }
        let same = same.report().unwrap();
        eprintln!("same thread: p99.9 {} ns; {:?}", same.wall.p999, playout.stats());

        let Playout { mut feed, ring } = Playout::default();
        let rendered = AtomicUsize::new(0);
        // The pushes run on their own thread while a render is timed, and the process's
        // instruction count would be theirs too.
        let mut beside = bench.series("decoder_thread_beside").wall_only();
        std::thread::scope(|s| {
            s.spawn(|| {
                let mut seq = 1_u32;
                loop {
                    let done = rendered.load(Ordering::Acquire);
                    if done >= RENDERS {
                        break;
                    }
                    if seq as usize * FRAME_SAMPLES as usize <= (done + 3) * DEVICE_FRAMES {
                        feed.push(&ring, &packets[seq as usize % packets.len()].0, arrival(seq));
                        seq += 1;
                    } else {
                        std::thread::yield_now();
                    }
                }
            });
            for i in 0..RENDERS {
                let user = NonNull::from(&ring).cast::<c_void>();
                beside.time(|| render_once(user, &mut buffer));
                rendered.store(i + 1, Ordering::Release);
            }
        });
        let beside = beside.report().unwrap();
        eprintln!("decoder thread beside: p99.9 {} ns", beside.wall.p999);
    }

    /// iOS renders 1 024 frames at a time unless the session asks for fewer, twice what a Mac
    /// device takes. The ring's depth still covers a render while packets arrive every 20 ms.
    #[test]
    fn a_device_rendering_1024_frames_is_not_starved() {
        let run = play_with(&trace(20, 0, 1_000, &[]), 20_000_000, (0, 0), 1024);
        eprintln!(
            "1024-frame renders: {} ms mean after 3 s; {:?}",
            run.mean_heard(3_000_000, 20_000_000),
            run.stats
        );
        assert_eq!(run.stats.underruns, 0, "{:?}", run.stats);
        assert!(run.max_step < 2.0 * tone_step(), "no click: {}", run.max_step);
    }
}
