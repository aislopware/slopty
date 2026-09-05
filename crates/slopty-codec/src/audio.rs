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

/// Samples waiting to be played, shared with the queue's callback thread.
#[derive(Debug, Default)]
struct Ring {
    samples: VecDeque<f32>,
    /// Buffers played from an empty ring since the last sample arrived.
    underruns: u64,
}

/// Longest backlog kept (both channels): beyond it the oldest samples are dropped so a stall
/// never turns into lasting delay. 200 ms.
const RING_MAX: usize = PACKET_SAMPLES * 10;
/// Bytes in one output buffer (one packet of PCM).
const BUFFER_BYTES: usize = PACKET_SAMPLES * SAMPLE_BYTES;
/// Output buffers in flight; each holds one packet's worth (20 ms).
const QUEUE_BUFFERS: usize = 3;

/// An `AudioQueue` playing interleaved stereo float at 48 kHz from a ring the decoder fills.
pub struct Player {
    queue: AudioQueueRef,
    ring: Arc<Mutex<Ring>>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player").finish_non_exhaustive()
    }
}

// SAFETY: AudioQueue calls are thread-safe; the ring is behind a mutex.
unsafe impl Send for Player {}

/// `AudioQueueOutputCallback`: refill a buffer from the ring, silence when it is empty.
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
    let want = capacity.min(PACKET_SAMPLES);
    // SAFETY: `mAudioData` points at `mAudioDataBytesCapacity` bytes owned by the queue.
    let out =
        unsafe { std::slice::from_raw_parts_mut(buf.mAudioData.as_ptr().cast::<f32>(), want) };
    {
        let mut ring = ring.lock();
        let have = ring.samples.len().min(want);
        for (slot, sample) in out.iter_mut().zip(ring.samples.drain(..have)) {
            *slot = sample;
        }
        if have < want {
            if let Some(rest) = out.get_mut(have..) {
                rest.fill(0.0);
            }
            ring.underruns = ring.underruns.saturating_add(1);
        }
    }
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
        let player = Self { queue, ring };
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

    /// Queue interleaved samples for playback; the oldest are dropped past [`RING_MAX`].
    pub fn push(&self, samples: &[f32]) {
        let mut ring = self.ring.lock();
        ring.samples.extend(samples);
        let excess = ring.samples.len().saturating_sub(RING_MAX);
        if excess > 0 {
            ring.samples.drain(..excess);
        }
        ring.underruns = 0;
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
        player.push(&vec![0.1; PACKET_SAMPLES * 2]);
        assert!(player.queued() <= PACKET_SAMPLES * 2);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(player.queued(), 0);
    }
}
