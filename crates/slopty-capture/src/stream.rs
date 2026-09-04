//! One `SCStream` delivering pixel buffers to a sink.

use std::ptr::NonNull;

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{CMClock, CMSampleBuffer, CMTime, CMTimeFlags};
use objc2_core_video::{
    CVPixelBuffer, kCVPixelFormatType_32BGRA, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
    kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange,
};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_screen_capture_kit::{
    SCCaptureDynamicRange, SCContentFilter, SCDisplay, SCFrameStatus, SCStream,
    SCStreamConfiguration, SCStreamDelegate, SCStreamFrameInfoStatus, SCStreamOutput,
    SCStreamOutputType, SCWindow,
};
use parking_lot::Mutex;
use slopty_codec::PixelBuffer;
use slopty_proto::screen::CaptureTarget;

use crate::{CaptureError, Shareable};

/// Pixel layout of captured frames.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PixelFormat {
    /// 8-bit 4:2:0 bi-planar, video range (`420v`); what the HEVC Main encoder wants.
    Nv12,
    /// 10-bit 4:2:0 bi-planar, video range (`x420`); for Main10 / HDR.
    P010,
    /// 8-bit BGRA; for debugging and screenshots.
    Bgra,
}

impl PixelFormat {
    const fn os_type(self) -> u32 {
        match self {
            Self::Nv12 => kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            Self::P010 => kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange,
            Self::Bgra => kCVPixelFormatType_32BGRA,
        }
    }
}

/// Stream settings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CaptureConfig {
    /// Output width in pixels (even).
    pub width: u32,
    /// Output height in pixels (even).
    pub height: u32,
    /// Frame rate ceiling.
    pub fps: u16,
    /// Pixel layout.
    pub format: PixelFormat,
    /// Buffers in flight between ScreenCaptureKit and us; 2 is the latency floor.
    pub queue_depth: u8,
}

/// A frame from the stream.
#[derive(Debug)]
pub struct CapturedFrame {
    /// The picture, IOSurface-backed; hand it straight to the encoder.
    pub image: PixelBuffer,
    /// Capture time on the host clock (`CMClockGetHostTimeClock`), microseconds.
    pub capture_ts_us: u64,
    /// Time between capture and this callback, microseconds.
    pub age_us: u64,
}

/// What to capture, resolved from a [`Shareable`] snapshot.
pub struct Target {
    kind: CaptureTarget,
    filter: Retained<SCContentFilter>,
    pixel_size: (u32, u32),
}

// SAFETY: `SCContentFilter` is an immutable description object that ScreenCaptureKit reads
// from its own queues; nothing here mutates it after construction.
#[expect(clippy::non_send_fields_in_send_ty, reason = "immutable SCK value object")]
unsafe impl Send for Target {}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("kind", &self.kind)
            .field("pixel_size", &self.pixel_size)
            .finish_non_exhaustive()
    }
}

impl Target {
    /// Resolve a target against a content snapshot.
    pub fn resolve(content: &Shareable, kind: CaptureTarget) -> Result<Self, CaptureError> {
        match kind {
            CaptureTarget::Window(id) => {
                let window = content.window(id).ok_or(CaptureError::NotFound(kind))?;
                Ok(Self::window(kind, &window))
            }
            CaptureTarget::Display(id) => {
                let display = content.display(id).ok_or(CaptureError::NotFound(kind))?;
                Ok(Self::display(kind, &display))
            }
        }
    }

    fn window(kind: CaptureTarget, window: &SCWindow) -> Self {
        // SAFETY: `SCContentFilter` has no subclassing or thread requirements for this init.
        let filter = unsafe {
            SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), window)
        };
        Self { kind, pixel_size: filter_pixel_size(&filter), filter }
    }

    fn display(kind: CaptureTarget, display: &SCDisplay) -> Self {
        let none: Retained<NSArray<SCWindow>> = NSArray::from_slice(&[]);
        // SAFETY: as above.
        let filter = unsafe {
            SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                display,
                &none,
            )
        };
        Self { kind, pixel_size: filter_pixel_size(&filter), filter }
    }

    /// The target this resolves.
    #[must_use]
    pub const fn kind(&self) -> CaptureTarget {
        self.kind
    }

    /// Native pixel size of the target, rounded up to even.
    #[must_use]
    pub const fn pixel_size(&self) -> (u32, u32) {
        self.pixel_size
    }
}

/// Pixel size of a filter's content rect, rounded up to even.
fn filter_pixel_size(filter: &SCContentFilter) -> (u32, u32) {
    // SAFETY: plain getters on a valid object.
    let rect = unsafe { filter.contentRect() };
    // SAFETY: as above.
    let scale = f64::from(unsafe { filter.pointPixelScale() });
    let even = |v: f64| -> u32 {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped")]
        let px = (v * scale).round().clamp(2.0, 16_384.0) as u32;
        px.next_multiple_of(2)
    };
    (even(rect.size.width), even(rect.size.height))
}

type FrameSink = Box<dyn Fn(CapturedFrame) + Send + Sync>;
type StopSink = Box<dyn Fn(CaptureError) + Send + Sync>;

struct Ivars {
    sink: FrameSink,
    on_stop: StopSink,
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `Output` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "SloptyStreamOutput"]
    #[ivars = Ivars]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind == SCStreamOutputType::Screen {
                self.screen_sample(sample);
            }
        }
    }

    unsafe impl SCStreamDelegate for Output {
        #[unsafe(method(stream:didStopWithError:))]
        fn did_stop(&self, _stream: &SCStream, error: &NSError) {
            (self.ivars().on_stop)(CaptureError::from_ns(error));
        }
    }
);

impl Output {
    fn new(sink: FrameSink, on_stop: StopSink) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars { sink, on_stop });
        // SAFETY: `NSObject`'s `init` on a freshly allocated instance with ivars set.
        unsafe { msg_send![super(this), init] }
    }

    fn screen_sample(&self, sample: &CMSampleBuffer) {
        if frame_status(sample) != Some(SCFrameStatus::Complete) {
            return;
        }
        // SAFETY: valid sample buffer.
        let Some(image) = (unsafe { sample.image_buffer() }) else { return };
        // SAFETY: valid sample buffer.
        let pts = unsafe { sample.presentation_time_stamp() };
        let capture_ts_us = micros(pts).unwrap_or(0);
        let now_us = host_now_us();
        let image: CFRetained<CVPixelBuffer> = image;
        (self.ivars().sink)(CapturedFrame {
            image: PixelBuffer::from_retained(image),
            capture_ts_us,
            age_us: now_us.saturating_sub(capture_ts_us),
        });
    }
}

/// `SCStreamFrameInfoStatus` from the sample's first attachment dictionary.
fn frame_status(sample: &CMSampleBuffer) -> Option<SCFrameStatus> {
    // SAFETY: valid sample buffer; `false` never allocates.
    let array = unsafe { sample.sample_attachments_array(false) }?;
    // SAFETY: CoreMedia documents the array's elements as CFDictionaries keyed by CFString.
    let array: CFRetained<CFArray<CFDictionary<CFString, CFType>>> =
        unsafe { CFRetained::cast_unchecked(array) };
    let dict = array.get(0)?;
    // SAFETY: framework-provided constant string.
    let key_ns: &NSString = unsafe { SCStreamFrameInfoStatus };
    let key_ptr: NonNull<NSString> = NonNull::from(key_ns);
    // SAFETY: `NSString` and `CFString` are toll-free bridged; the pointee is a constant.
    let key: &CFString = unsafe { key_ptr.cast::<CFString>().as_ref() };
    let status = dict.get(key)?.downcast::<CFNumber>().ok()?.as_i64()?;
    Some(SCFrameStatus(isize::try_from(status).ok()?))
}

fn micros(time: CMTime) -> Option<u64> {
    if !time.flags.contains(CMTimeFlags::Valid) || time.timescale <= 0 {
        return None;
    }
    let value = u64::try_from(time.value).ok()?;
    let scale = u64::try_from(time.timescale).ok()?;
    value.saturating_mul(1_000_000).checked_div(scale)
}

/// Now on the host time clock, microseconds; the clock ScreenCaptureKit stamps frames with.
#[must_use]
pub fn host_now_us() -> u64 {
    // SAFETY: the host time clock always exists.
    let clock = unsafe { CMClock::host_time_clock() };
    // SAFETY: valid clock.
    micros(unsafe { clock.time() }).unwrap_or(0)
}

/// A running capture stream.
pub struct Capture {
    stream: Retained<SCStream>,
    _output: Retained<Output>,
    _queue: DispatchRetained<DispatchQueue>,
    target: CaptureTarget,
}

// SAFETY: `SCStream` methods may be called from any thread (ScreenCaptureKit dispatches
// internally); the output object is only ever touched by ScreenCaptureKit's queue.
#[expect(clippy::non_send_fields_in_send_ty, reason = "SCStream is internally queued")]
unsafe impl Send for Capture {}
// SAFETY: as above; `&Capture` only exposes `SCStream` calls.
unsafe impl Sync for Capture {}

impl std::fmt::Debug for Capture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Capture").field("target", &self.target).finish_non_exhaustive()
    }
}

impl Capture {
    /// Build a stream and start it. `done` runs on a ScreenCaptureKit queue once the stream is
    /// live (or failed); frames follow on `sink`, and `on_stop` fires if the stream dies.
    pub fn start(
        target: &Target,
        config: &CaptureConfig,
        sink: impl Fn(CapturedFrame) + Send + Sync + 'static,
        on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) -> Result<Self, CaptureError> {
        let configuration = stream_configuration(config);
        let output = Output::new(Box::new(sink), Box::new(on_stop));
        let delegate: &ProtocolObject<dyn SCStreamDelegate> = ProtocolObject::from_ref(&*output);
        // SAFETY: filter, configuration and delegate are valid; the delegate is retained by the
        // stream (it is also kept alive by `Capture`).
        let stream = unsafe {
            SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &target.filter,
                &configuration,
                Some(delegate),
            )
        };
        let queue = DispatchQueue::new("io.slopty.capture", DispatchQueueAttr::SERIAL);
        let handler: &ProtocolObject<dyn SCStreamOutput> = ProtocolObject::from_ref(&*output);
        // SAFETY: valid stream, output and queue.
        unsafe {
            stream.addStreamOutput_type_sampleHandlerQueue_error(
                handler,
                SCStreamOutputType::Screen,
                Some(&queue),
            )
        }
        .map_err(|e| CaptureError::from_ns(&e))?;
        let block = completion(done);
        // SAFETY: the block is copied by the framework; it captures only `Send` data.
        unsafe {
            stream.startCaptureWithCompletionHandler(Some(&block));
        }
        Ok(Self { stream, _output: output, _queue: queue, target: target.kind() })
    }

    /// What is being captured.
    #[must_use]
    pub const fn target(&self) -> CaptureTarget {
        self.target
    }

    /// Change size, rate or format on the live stream.
    pub fn update(
        &self,
        config: &CaptureConfig,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) {
        let configuration = stream_configuration(config);
        let block = completion(done);
        // SAFETY: valid stream and configuration; the block is copied by the framework.
        unsafe { self.stream.updateConfiguration_completionHandler(&configuration, Some(&block)) }
    }

    /// Stop; `done` runs once ScreenCaptureKit has torn the stream down.
    pub fn stop(&self, done: impl FnOnce(Result<(), CaptureError>) + Send + 'static) {
        let block = completion(done);
        // SAFETY: valid stream; the block is copied by the framework.
        unsafe { self.stream.stopCaptureWithCompletionHandler(Some(&block)) }
    }
}

fn completion(
    done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
) -> RcBlock<dyn Fn(*mut NSError)> {
    let done = Mutex::new(Some(done));
    RcBlock::new(move |error: *mut NSError| {
        let Some(done) = done.lock().take() else { return };
        let result = match NonNull::new(error) {
            // SAFETY: ScreenCaptureKit passes a live error object for the callback's duration.
            Some(error) => Err(CaptureError::from_ns(unsafe { error.as_ref() })),
            None => Ok(()),
        };
        done(result);
    })
}

fn stream_configuration(config: &CaptureConfig) -> Retained<SCStreamConfiguration> {
    // SAFETY: plain constructor.
    let c = unsafe { SCStreamConfiguration::new() };
    let interval = CMTime {
        value: 1,
        timescale: i32::from(config.fps.max(1)),
        flags: CMTimeFlags::Valid,
        epoch: 0,
    };
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setWidth(usize::try_from(config.width).unwrap_or(usize::MAX));
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setHeight(usize::try_from(config.height).unwrap_or(usize::MAX));
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setMinimumFrameInterval(interval);
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setPixelFormat(config.format.os_type());
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setShowsCursor(false);
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setQueueDepth(isize::from(config.queue_depth.max(1)));
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setScalesToFit(true);
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setPreservesAspectRatio(true);
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setIgnoreShadowsSingleWindow(true);
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setIncludeChildWindows(true);
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setCapturesAudio(false);
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setCaptureDynamicRange(SCCaptureDynamicRange::SDR);
    }
    c
}
