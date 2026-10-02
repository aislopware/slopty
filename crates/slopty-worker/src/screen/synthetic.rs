//! Worker platforms whose pictures are drawn, not captured: [`Drawn`] and [`Synthetic`].
//!
//! Their capture is [`slopty_capture::synthetic::Canvas`], which draws a picture on each
//! target's beat: a dark desktop with a page of 5×7 glyphs scrolling in its middle, so every
//! frame moves and has sharp edges. Their encoders are the real ones, and so is everything
//! after them, so a [`Pipeline`](super::Pipeline) on either runs the same encode, packetize
//! and send path as a real capture. Their input sink ([`Poke`]) stands in for the application
//! a click lands in: every event it takes changes the next picture
//! ([`slopty_capture::synthetic::take_input`]).
//!
//! [`Drawn`] stands in for this Mac's own displays, at their sizes; the measurement in its
//! tests times the frame path on it (`docs/MEASUREMENTS.md`, "Capture to the glass").
//!
//! [`Synthetic`] is a whole screen of its own: one display and two windows ([`DISPLAY`],
//! [`WINDOWS`]), the same on every Mac, whose windows take a resize. A worker started with
//! [`SWITCH`] set to `1` serves every window and display stream from it and lists nothing of
//! the Mac it runs on, so the app self-test streams real video without the Screen Recording
//! grant and its goldens do not depend on the machine (`docs/TESTING.md`).

use std::sync::LazyLock;
use std::time::Instant;

use parking_lot::Mutex;
use slopty_capture::synthetic::{Canvas, CanvasStream, CanvasTarget, Tone};
use slopty_capture::{
    AudioSink, AxError, CaptureConfig, CaptureError, CaptureSource, CapturedFrame, Console, Crop,
    Heard, Rect, TargetWindow, Went, WindowState,
};
use slopty_codec::PixelBuffer;
use slopty_core::{DisplayId, WindowId};
use slopty_input::{DragStep, InputError, InputSink, PointerWatch};
use slopty_proto::screen::{CaptureTarget, CursorShape, DisplayInfo, ScreenInput, WindowInfo};

use crate::platform::Platform;

/// Drawn pictures, the real encoders, and input that changes the picture.
#[derive(Clone, Copy, Debug)]
pub enum Drawn {}

impl Platform for Drawn {
    type Audio = slopty_codec::Opus;
    type Capture = Canvas;
    type Input = Poke;
    type Video = slopty_codec::VideoToolbox;
}

/// Input that changes the canvas: each event is one more input its strip counts.
#[derive(Debug, Default)]
pub struct Poke {
    pointer: PointerWatch,
}

impl InputSink for Poke {
    fn new(_target: CaptureTarget, _scale: f64) -> Self {
        Self::default()
    }

    fn set_scale(&mut self, _scale: f64) {}

    fn set_bounds(&mut self, _bounds: Option<Rect>, _at: Instant) {}

    fn inject(&mut self, _input: &ScreenInput) -> Result<(), InputError> {
        slopty_capture::synthetic::take_input();
        Ok(())
    }

    fn focus(&mut self) -> Result<(), InputError> {
        Ok(())
    }

    fn release_all(&mut self) {}

    fn pointer(&self) -> PointerWatch {
        self.pointer.clone()
    }

    /// A drawn screen has no desktop to drag on: each step counts as input, and the entry's
    /// point, and any point located, is its own pixel, as if the stream were at the origin at
    /// one pixel a point.
    fn drag(&mut self, step: DragStep) {
        slopty_capture::synthetic::take_input();
        if let DragStep::Enter { x, y, answer } | DragStep::Locate { x, y, answer } = step {
            let _gone = answer.send(Ok((f64::from(x), f64::from(y))));
        }
    }
}

/// The variable that makes a worker serve [`Synthetic`]: `1` turns it on, anything else leaves
/// the worker on its own screen. A test knob, read once.
pub const SWITCH: &str = "SLOPTY_SYNTHETIC_SCREEN";

/// Whether this process serves [`Synthetic`] ([`SWITCH`]).
#[must_use]
pub fn serving() -> bool {
    static ON: LazyLock<bool> =
        LazyLock::new(|| switched_on(std::env::var(SWITCH).ok().as_deref()));
    *ON
}

/// The variable that gives [`Synthetic`] a sound, the canvas's tone: `1` turns it on.
///
/// Off, its worker captures no sound ([`crate::screen::sound::Silent`]), so no client opens a
/// player for it: tests make no sound on this Mac. A test knob, read once.
pub const SOUND_SWITCH: &str = "SLOPTY_SYNTHETIC_SOUND";

/// Whether this process's [`Synthetic`] screen sounds ([`SOUND_SWITCH`]).
#[must_use]
pub fn sounding() -> bool {
    static ON: LazyLock<bool> =
        LazyLock::new(|| switched_on(std::env::var(SOUND_SWITCH).ok().as_deref()));
    *ON
}

/// [`SWITCH`]'s value read as on or off.
fn switched_on(value: Option<&str>) -> bool {
    value == Some("1")
}

/// A drawn screen: [`Studio`]'s display and windows, the real encoders, and [`Poke`].
#[derive(Clone, Copy, Debug)]
pub enum Synthetic {}

impl Platform for Synthetic {
    type Audio = slopty_codec::Opus;
    type Capture = Studio;
    type Input = Poke;
    type Video = slopty_codec::VideoToolbox;
}

/// The one display [`Synthetic`] lists: a 14-inch laptop panel at its default scaling, 2× and
/// 60 Hz.
pub const DISPLAY: DisplayInfo =
    DisplayInfo { id: DisplayId(1), w: 1512.0, h: 982.0, scale: 2.0, hz: 60.0 };

/// [`DISPLAY`] as [`StudioAt`] lists and draws it: refreshing at `HZ`, `W` × `H` points.
fn shown_display<const HZ: u16, const W: u16, const H: u16>() -> DisplayInfo {
    DisplayInfo { hz: f32::from(HZ), w: f32::from(W), h: f32::from(H), ..DISPLAY }
}

/// The application every [`Synthetic`] window belongs to.
pub const APP: &str = "Slopty synthetic";

/// A window of [`Synthetic`] as a worker starts.
#[derive(Clone, Copy, Debug)]
pub struct Placed {
    /// Its id.
    pub id: WindowId,
    /// Its title.
    pub title: &'static str,
    /// Its top-left corner on [`DISPLAY`], in points.
    pub origin: (f32, f32),
    /// Its size in points.
    pub size: (f32, f32),
}

/// [`Synthetic`]'s windows as a worker starts.
pub const WINDOWS: [Placed; 2] = [
    Placed {
        id: WindowId(7001),
        title: "Synthetic editor",
        origin: (96.0, 72.0),
        size: (1280.0, 800.0),
    },
    Placed {
        id: WindowId(7002),
        title: "Synthetic terminal",
        origin: (360.0, 280.0),
        size: (800.0, 500.0),
    },
];

/// The smallest a window is resized to, in points.
const MIN_WINDOW: (f64, f64) = (160.0, 120.0);

/// The windows as they are now: a resize changes them for every stream and every listing after.
static SCENE: LazyLock<Mutex<Vec<WindowInfo>>> = LazyLock::new(|| {
    let windows =
        WINDOWS.iter().map(|&Placed { id, title, origin: (x, y), size: (w, h) }| WindowInfo {
            id,
            app: APP.to_owned(),
            bundle_id: Some("io.slopty.synthetic".to_owned()),
            title: title.to_owned(),
            x,
            y,
            w,
            h,
            display: DISPLAY.id,
            on_screen: true,
        });
    Mutex::new(windows.collect())
});

fn window(id: WindowId) -> Option<WindowInfo> {
    SCENE.lock().iter().find(|w| w.id == id).cloned()
}

fn bounds(window: &WindowInfo) -> Rect {
    Rect {
        x: f64::from(window.x),
        y: f64::from(window.y),
        w: f64::from(window.w),
        h: f64::from(window.h),
    }
}

fn display_rect(display: &DisplayInfo) -> Rect {
    Rect { x: 0.0, y: 0.0, w: f64::from(display.w), h: f64::from(display.h) }
}

/// The process every window is owned by: this one, as far as anyone asks.
fn owner() -> i32 {
    i32::try_from(std::process::id()).unwrap_or(0)
}

/// What [`Studio`] shares: its windows when it was asked.
#[derive(Clone, Debug)]
pub struct Scene {
    windows: Vec<WindowInfo>,
}

/// The capture of [`Synthetic`]: [`Canvas`] pictures of [`DISPLAY`] and the windows, at each
/// one's size and at the display's beat, with a window list only it keeps.
pub type Studio = StudioAt<60>;

/// [`Studio`] on a display refreshing at `HZ`, `W` × `H` points at 2×: a measurement's 120 Hz
/// panel or 5K display, whatever the panels of the Mac it runs on.
#[derive(Clone, Copy, Debug)]
pub enum StudioAt<const HZ: u16, const W: u16 = 1512, const H: u16 = 982> {}

impl<const HZ: u16, const W: u16, const H: u16> CaptureSource for StudioAt<HZ, W, H> {
    type Content = Scene;
    type HideWatch = ();
    type Image = PixelBuffer;
    type Sound = Tone;
    type Stream = CanvasStream;
    type Target = CanvasTarget;

    fn can_capture() -> bool {
        true
    }

    fn enumerate(done: impl FnOnce(Result<Scene, CaptureError>) + Send + 'static) {
        done(Ok(Scene { windows: SCENE.lock().clone() }));
    }

    fn windows(content: &Scene) -> Vec<WindowInfo> {
        content.windows.clone()
    }

    fn displays(_content: &Scene) -> Vec<DisplayInfo> {
        vec![shown_display::<HZ, W, H>()]
    }

    fn resolve(content: &Scene, kind: CaptureTarget) -> Result<CanvasTarget, CaptureError> {
        let (w, h) = match kind {
            CaptureTarget::Display(id) if id == DISPLAY.id => (f32::from(W), f32::from(H)),
            CaptureTarget::Window(id) => content
                .windows
                .iter()
                .find(|w| w.id == id)
                .map(|w| (w.w, w.h))
                .ok_or(CaptureError::NotFound(kind))?,
            CaptureTarget::Display(_) => return Err(CaptureError::NotFound(kind)),
        };
        // A window is drawn as a canvas of its own size, at the display's scale.
        Canvas::resolve(&vec![DisplayInfo { w, h, ..DISPLAY }], CaptureTarget::Display(DISPLAY.id))
    }

    fn resolve_crop(_content: &Scene, _id: WindowId) -> Result<Option<CanvasTarget>, CaptureError> {
        Ok(None)
    }

    fn crop(_target: &CanvasTarget) -> Option<Crop> {
        None
    }

    fn pixel_size(target: &CanvasTarget) -> (u32, u32) {
        Canvas::pixel_size(target)
    }

    fn point_scale(target: &CanvasTarget) -> f32 {
        Canvas::point_scale(target)
    }

    fn start(
        target: &CanvasTarget,
        config: &CaptureConfig,
        sink: impl Fn(CapturedFrame<PixelBuffer>) + Send + Sync + 'static,
        on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) -> Result<CanvasStream, CaptureError> {
        // The beat is the listed display's, not that of whichever panel shares its id here.
        let config =
            CaptureConfig { fps: if config.fps == 0 { HZ } else { config.fps }, ..*config };
        Canvas::start(target, &config, sink, on_stop, done)
    }

    fn start_sound(
        heard: &Heard,
        sink: AudioSink,
        on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
        done: impl FnOnce(Result<Tone, CaptureError>) + Send + 'static,
    ) {
        Canvas::start_sound(heard, sink, on_stop, done);
    }

    fn hear(
        sound: &Tone,
        heard: &Heard,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) {
        Canvas::hear(sound, heard, done);
    }

    fn stop_sound(sound: &Tone, done: impl FnOnce(Result<(), CaptureError>) + Send + 'static) {
        Canvas::stop_sound(sound, done);
    }

    fn update(
        stream: &CanvasStream,
        config: &CaptureConfig,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) {
        Canvas::update(stream, config, done);
    }

    fn retarget(
        stream: &CanvasStream,
        target: &CanvasTarget,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) {
        Canvas::retarget(stream, target, done);
    }

    fn stop(stream: &CanvasStream, done: impl FnOnce(Result<(), CaptureError>) + Send + 'static) {
        Canvas::stop(stream, done);
    }

    fn now_us() -> u64 {
        Canvas::now_us()
    }

    fn console() -> Option<Console> {
        Canvas::console()
    }

    fn target_bounds(target: CaptureTarget) -> Option<Rect> {
        match target {
            CaptureTarget::Display(id) => {
                (id == DISPLAY.id).then(|| display_rect(&shown_display::<HZ, W, H>()))
            }
            CaptureTarget::Window(id) => window(id).as_ref().map(bounds),
        }
    }

    fn refresh_hz(_target: CaptureTarget) -> Option<f64> {
        Some(f64::from(HZ))
    }

    fn window_state(id: WindowId) -> Option<WindowState> {
        let window = window(id)?;
        Some(WindowState {
            bounds: bounds(&window),
            on_screen: window.on_screen,
            owner_pid: owner(),
        })
    }

    fn window_bounds(id: WindowId) -> Option<Rect> {
        window(id).as_ref().map(bounds)
    }

    fn window_owner(id: WindowId) -> Option<i32> {
        window(id).map(|_| owner())
    }

    fn window_on_screen(id: WindowId) -> bool {
        window(id).is_some_and(|w| w.on_screen)
    }

    fn window_title(id: WindowId) -> Option<String> {
        window(id).map(|w| w.title)
    }

    fn occluded(_id: WindowId, _bounds: &Rect, _owner: i32) -> bool {
        false
    }

    // No window is ever served as a crop of the display: each is a canvas of its own.
    fn display_enclosing(_rect: &Rect) -> Option<u32> {
        None
    }

    fn display_bounds(_id: u32) -> Rect {
        display_rect(&shown_display::<HZ, W, H>())
    }

    fn resize_window(
        _pid: i32,
        target: &TargetWindow,
        width: f64,
        height: f64,
    ) -> Result<(), AxError> {
        #[expect(clippy::cast_possible_truncation, reason = "points, clamped to the display")]
        let side = |asked: f64, min: f64, max: f32| asked.clamp(min, f64::from(max)).round() as f32;
        let (w, h) =
            (side(width, MIN_WINDOW.0, f32::from(W)), side(height, MIN_WINDOW.1, f32::from(H)));
        SCENE
            .lock()
            .iter_mut()
            .find(|window| Some(&window.title) == target.title.as_ref())
            .map(|window| (window.w, window.h) = (w, h))
            .ok_or(AxError::Unsupported)
    }

    fn watch_hides(
        _pid: i32,
        _target: TargetWindow,
        _on_went: impl Fn(Went) + Send + Sync + 'static,
    ) -> Result<(), AxError> {
        Err(AxError::Unsupported)
    }

    fn watch_targeted((): &()) -> bool {
        false
    }

    fn pointer_moves() -> u32 {
        0
    }

    fn pointer_location() -> (f64, f64) {
        (0.0, 0.0)
    }

    fn cursor_seed() -> Option<i32> {
        None
    }

    fn cursor_shape() -> Option<CursorShape> {
        None
    }
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "measurement arithmetic on small counts, printed"
)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use bytes::Bytes;
    use slopty_capture::synthetic::{inputs_shown, inputs_taken};
    use slopty_client::pacing::{ClockAnchor, Pacer, Spread, percentile};
    use slopty_client::screen::{ScreenRouter, Uplink, spawn_screen};
    use slopty_codec::stripes::OVERLAP;
    use slopty_core::StreamId;
    use slopty_proto::ClientMsg;
    use slopty_proto::datagram::ClientDatagram;
    use slopty_proto::input::{Mods, MouseButton};
    use slopty_proto::media::MAX_DATAGRAM;
    use slopty_proto::screen::{Chroma, Feedback, Quality, ScreenEvent, ScreenRequest, Stripe};
    use tokio::sync::mpsc;

    use super::*;
    use crate::screen::stripes::Knob;
    use crate::screen::{Coding, DatagramSink, Pipeline, Refused, StreamControl};

    const STREAM: StreamId = StreamId(1);

    /// The worker's capture clock and this process's, read together. Both are mach absolute
    /// time, so on one machine this is exact to the width of the two reads.
    fn anchor() -> ClockAnchor {
        let before = slopty_capture::host_now_us();
        let at = Instant::now();
        let after = slopty_capture::host_now_us();
        ClockAnchor { at, host_us: before + (after - before) / 2 }
    }

    /// One direction of the in-process link: items handed to the far end in order, `delay`
    /// after they were sent.
    fn delay_line<T: Send + 'static>(
        delay: Duration,
        deliver: impl Fn(T, Instant) + Send + 'static,
    ) -> mpsc::UnboundedSender<(Instant, T)> {
        let (tx, mut rx) = mpsc::unbounded_channel::<(Instant, T)>();
        tokio::spawn(async move {
            while let Some((sent, item)) = rx.recv().await {
                tokio::time::sleep_until((sent + delay).into()).await;
                deliver(item, Instant::now());
            }
        });
        tx
    }

    /// The worker's end of the link: datagrams go straight into the client's router on
    /// loopback, or down a delay line; with a `rate`, each leaves when the link has sent the
    /// ones before it, and what has not left is what the transport holds.
    struct Wire {
        router: ScreenRouter,
        line: Option<mpsc::UnboundedSender<(Instant, Vec<Bytes>)>>,
        /// Bytes of every datagram sent: data, parity, retransmits, cursor and heartbeats.
        bytes: std::sync::atomic::AtomicU64,
        /// The link's rate in bits a second, and the batches not yet sent on it: when each
        /// leaves and its bytes.
        rate: Option<(u64, Mutex<Leaving>)>,
    }

    /// Batches handed to a rate-limited link and not yet sent on it: when each leaves, and its
    /// bytes.
    type Leaving = VecDeque<(Instant, usize)>;

    impl Wire {
        fn new(
            router: ScreenRouter,
            line: Option<mpsc::UnboundedSender<(Instant, Vec<Bytes>)>>,
            rate_bps: Option<u64>,
        ) -> Self {
            Self {
                router,
                line,
                bytes: std::sync::atomic::AtomicU64::new(0),
                rate: rate_bps.map(|bps| (bps, Mutex::new(VecDeque::new()))),
            }
        }

        fn bytes(&self) -> u64 {
            self.bytes.load(Ordering::Relaxed)
        }

        /// When `bytes` handed over at `now` have left the link.
        fn departure(&self, now: Instant, bytes: usize) -> Instant {
            let Some((bps, queue)) = &self.rate else { return now };
            let mut queue = queue.lock();
            let free = queue.back().map_or(now, |(last, _)| (*last).max(now));
            let at = free + Duration::from_secs_f64(bytes as f64 * 8.0 / *bps as f64);
            queue.push_back((at, bytes));
            at
        }
    }

    impl DatagramSink for Wire {
        fn send(&self, datagrams: &[Bytes]) -> Result<(), Refused> {
            let now = Instant::now();
            let sent: usize = datagrams.iter().map(Bytes::len).sum();
            self.bytes.fetch_add(sent as u64, Ordering::Relaxed);
            let left = self.departure(now, sent);
            match &self.line {
                None => self.router.route_many(datagrams.iter().cloned(), now),
                Some(line) => {
                    line.send((left, datagrams.to_vec())).map_err(|_gone| Refused::Closed)?;
                }
            }
            Ok(())
        }

        fn max_size(&self) -> Option<usize> {
            Some(MAX_DATAGRAM)
        }

        fn held(&self) -> usize {
            let Some((_, queue)) = &self.rate else { return 0 };
            let now = Instant::now();
            let mut queue = queue.lock();
            while queue.front().is_some_and(|(at, _)| *at <= now) {
                queue.pop_front();
            }
            queue.iter().map(|(_, bytes)| bytes).sum()
        }

        fn cwnd(&self) -> u64 {
            0
        }

        fn is_closed(&self) -> bool {
            false
        }
    }

    /// What a measurement run changes from the plain stream.
    #[derive(Clone, Copy, Debug)]
    struct Knobs {
        /// The stream's sides padded to this, in place of its codec's own multiple.
        pad_to: Option<u32>,
        /// The link's rate, bits a second; unlimited when `None`.
        link_bps: Option<u64>,
        /// Whether the worker refines a still picture.
        refine: bool,
        /// Whether the stream is coded as two stripes.
        stripes: Knob,
    }

    impl Default for Knobs {
        fn default() -> Self {
            Self { pad_to: None, link_bps: None, refine: true, stripes: Knob::Off }
        }
    }

    /// The client's feedback, as the worker's connection answers it the moment it lands.
    fn answer(control: &StreamControl, bytes: &[u8]) {
        match ClientDatagram::decode(bytes) {
            Some(ClientDatagram::Feedback(Feedback::Nack { stream, frame, fragments })) => {
                control.of_media(stream).nack(frame, &fragments);
            }
            Some(ClientDatagram::Feedback(Feedback::Refresh {
                stream,
                last_good_frame,
                keyframe,
            })) => {
                control.of_media(stream).request_refresh(last_good_frame, keyframe);
            }
            Some(ClientDatagram::Feedback(Feedback::Clock { sent_us, .. })) => {
                control.clock(sent_us, Instant::now());
            }
            _other => {}
        }
    }

    fn ms(d: Duration) -> f64 {
        d.as_secs_f64() * 1e3
    }

    fn spread(samples: &mut [Duration]) -> String {
        samples.sort_unstable();
        format!(
            "p50 {:.2} / p95 {:.2} / max {:.2} ms (n={})",
            ms(percentile(samples, 50)),
            ms(percentile(samples, 95)),
            ms(samples.last().copied().unwrap_or_default()),
            samples.len()
        )
    }

    fn ring(s: &Spread) -> String {
        format!(
            "p50 {:.2} / p95 {:.2} / max {:.2} ms (n={})",
            ms(s.p50),
            ms(s.p95),
            ms(s.max),
            s.count
        )
    }

    /// One run: the canvas streamed through the real encoder and packetizer into the client's
    /// reassembler and decoder at `fps` frames a second at most, `one_way` each way with
    /// `loss_permille` of the datagrams lost, a click every 80–150 ms, and a paint on the
    /// display's beat.
    async fn run<P: Platform>(
        label: &str,
        fps: u16,
        one_way: Duration,
        loss_permille: u32,
        seconds: u64,
    ) {
        run_padded::<P>(label, fps, one_way, loss_permille, seconds, Knobs::default()).await;
    }

    /// [`run`] with the [`Knobs`]: the stream's sides padded ([`Pipeline::open_padded`]), a link
    /// of a given rate, refinement off.
    #[expect(clippy::too_many_lines, reason = "one measurement, read top to bottom")]
    async fn run_padded<P: Platform>(
        label: &str,
        fps: u16,
        one_way: Duration,
        loss_permille: u32,
        seconds: u64,
        knobs: Knobs,
    ) {
        let Knobs { pad_to, link_bps, refine, stripes } = knobs;
        let ScreenEvent::Listing { displays, .. } = Pipeline::<P>::listing().await.unwrap() else {
            panic!("no listing")
        };
        let display = displays.first().expect("a display");
        let hz = f64::from(display.hz).clamp(24.0, 240.0);
        let router = if loss_permille > 0 {
            ScreenRouter::with_loss(loss_permille)
        } else {
            ScreenRouter::new()
        };
        let line = (!one_way.is_zero() || link_bps.is_some()).then(|| {
            let router = router.clone();
            delay_line(one_way, move |datagrams: Vec<Bytes>, at| router.route_many(datagrams, at))
        });
        let wire = Arc::new(Wire::new(router.clone(), line, link_bps));
        let (mut stream, opened) = Pipeline::<P>::open_padded(
            STREAM,
            CaptureTarget::Display(display.id),
            Quality { fps, ..Quality::default() },
            Arc::<Wire>::clone(&wire),
            |_event| {},
            Coding { pad_to, stripes },
        )
        .await
        .unwrap();
        let ScreenEvent::Opened { codec, width, height, stripes: coded, .. } = opened else {
            panic!("{opened:?}")
        };
        if !refine {
            stream.shared.top.refine.lock().disable();
            stream.shared.lower.refine.lock().disable();
        }

        let control = stream.control();
        let feedback = {
            let control = control.clone();
            delay_line(one_way, move |bytes: Bytes, _at| answer(&control, &bytes))
        };
        let (reports_tx, mut reports) = mpsc::channel::<ClientMsg>(64);
        let decisions = Arc::new(Mutex::new(Vec::new()));
        let reporting = {
            let control = control.clone();
            let decisions = Arc::clone(&decisions);
            let line = delay_line(one_way, move |msg: ClientMsg, _at| {
                if let ClientMsg::Screen(ScreenRequest::Report { stream, report }) = msg
                    && let Some(decision) = control.of_media(stream).report(&report, None)
                {
                    decisions.lock().push((decision.verdict, decision.target_bps));
                }
            });
            tokio::spawn(async move {
                while let Some(msg) = reports.recv().await {
                    let _gone = line.send((Instant::now(), msg));
                }
            })
        };
        let rtt = one_way * 2 + Duration::from_micros(200);
        let uplink = Uplink {
            control: reports_tx,
            feedback: Box::new(move |bytes| feedback.send((Instant::now(), bytes)).is_ok()),
            rtt: Box::new(move || Some(rtt)),
        };
        let handle =
            spawn_screen(&tokio::runtime::Handle::current(), &router, STREAM, codec, uplink);
        let mut frames = handle.frames();

        let clocks = anchor();
        let mut pacer = Pacer::default();
        pacer.share_clock(clocks);
        let mut paint = tokio::time::interval(Duration::from_secs_f64(1.0 / hz));
        paint.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        #[expect(clippy::cast_possible_truncation, reason = "the strip's 16 bits")]
        let base = inputs_taken() as u16;
        let warm_up = Duration::from_secs(1);
        let started = tokio::time::Instant::now();
        let deadline = started + warm_up + Duration::from_secs(seconds);
        let mut next_input = started + warm_up;
        let mut seq = 0_u64;
        let mut seen = 0_u64;
        let mut lcg = 0x2545_f491_4f6c_dd1d_u64;
        // Inputs on their way to the worker, and when each reached it.
        let mut in_flight: VecDeque<(tokio::time::Instant, u64)> = VecDeque::new();
        let mut reached: VecDeque<(u64, Instant)> = VecDeque::new();
        // Hop by hop, over the whole run after the warm-up.
        let (mut to_arrival, mut to_decoded, mut decode, mut to_paint, mut to_glass) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        // Capture → painted through the clock probes' estimate, and how far the estimate put
        // each capture from where the shared clock puts it.
        let (mut to_glass_estimated, mut estimate_error) = (Vec::new(), Vec::new());
        let mut reach_to_capture = Vec::new();
        // Bytes and frames at the end of the warm-up, so the rates cover the measured seconds.
        let mut settled: Option<(u64, u64)> = None;
        let click = ScreenInput::Button {
            button: MouseButton::Left,
            down: true,
            x: 10.0,
            y: 10.0,
            clicks: 1,
            mods: Mods::empty(),
        };
        loop {
            let delivery = in_flight.front().map_or(deadline, |(due, _)| *due);
            tokio::select! {
                _tick = paint.tick() => {
                    if settled.is_none() && tokio::time::Instant::now() > started + warm_up {
                        settled = Some((wire.bytes(), stream.stats().encoded));
                    }
                    if let Some(stamp) = pacer.painted() {
                        let at = Instant::now();
                        pacer.shown(stamp, at);
                        if tokio::time::Instant::now() > started + warm_up
                            && let Some(captured) = clocks.captured(stamp.pts_us)
                        {
                            to_paint.push(at.saturating_duration_since(stamp.decoded));
                            to_glass.push(at.saturating_duration_since(captured));
                            if let Some(estimated) = stamp.captured.map(|c| c.at) {
                                to_glass_estimated.push(at.saturating_duration_since(estimated));
                                estimate_error.push(estimated.duration_since(captured).max(captured.duration_since(estimated)));
                            }
                        }
                    }
                }
                changed = frames.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let Some(frame) = frames.borrow_and_update().clone() else { continue };
                    let stamp = frame.stamp;
                    let _pace = pacer.offer(stamp);
                    if tokio::time::Instant::now() > started + warm_up
                        && let Some(captured) = clocks.captured(stamp.pts_us)
                    {
                        to_arrival.push(stamp.arrived.saturating_duration_since(captured));
                        to_decoded.push(stamp.decoded.saturating_duration_since(captured));
                        decode.push(stamp.decoded.saturating_duration_since(stamp.arrived));
                    }
                    if let Some(shown) = inputs_shown(frame.frame.image.as_cv()) {
                        let count = u64::from(shown.wrapping_sub(base));
                        if count > seen && count <= seq {
                            seen = count;
                            pacer.input_visible(seen, stamp.pts_us);
                            let captured = clocks.captured(stamp.pts_us);
                            while let Some(&(s, at)) = reached.front() && s <= seen {
                                reached.pop_front();
                                if let Some(captured) = captured {
                                    reach_to_capture.push(captured.saturating_duration_since(at));
                                }
                            }
                        }
                    }
                }
                () = tokio::time::sleep_until(next_input) => {
                    seq += 1;
                    pacer.input_sent(seq, Instant::now());
                    in_flight.push_back((tokio::time::Instant::now() + one_way, seq));
                    lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                    next_input += Duration::from_millis(80 + (lcg >> 33) % 71);
                }
                () = tokio::time::sleep_until(delivery), if !in_flight.is_empty() => {
                    if let Some((_, s)) = in_flight.pop_front() {
                        stream.inject(&click).unwrap();
                        reached.push_back((s, Instant::now()));
                    }
                }
                () = tokio::time::sleep_until(deadline) => break,
            }
        }
        let glass = pacer.glass();
        let pacing = pacer.stats();
        let stats = handle.stats();
        let worker = stream.stats();
        eprintln!(
            "MEASURE glass {label}: {width}×{height} {codec:?} at {hz:.0} Hz, {} stripe(s), {fps} fps asked, one way {:.1} ms, loss {:.1} %, link {}, refinement {}, load {}",
            coded.len().max(1),
            ms(one_way),
            f64::from(loss_permille) / 10.0,
            link_bps.map_or_else(
                || "unlimited".to_owned(),
                |bps| format!("{:.0} Mbit/s", bps as f64 / 1e6)
            ),
            if refine { "on" } else { "off" },
            load_average(),
        );
        eprintln!("  capture → present (pacer ring):  {}", ring(&glass.capture));
        eprintln!(
            "  input sent → present (pacer):    {}  pending {}",
            ring(&glass.input),
            glass.inputs_pending
        );
        eprintln!(
            "  arrival → present (pacer ring):  p50 {:.2} / p95 {:.2} / max {:.2} ms",
            ms(pacing.latency_p50),
            ms(pacing.latency_p95),
            ms(pacing.latency_max)
        );
        eprintln!("  hops, whole run:");
        eprintln!(
            "    worker draw (beat → frame to sink): p50 {:.2} / p95 {:.2} ms",
            worker.capture.p50_us as f64 / 1e3,
            worker.capture.p95_us as f64 / 1e3
        );
        eprintln!(
            "    worker encode (submit → VT callback): p50 {:.2} / p95 {:.2} / max {:.2} ms",
            worker.encode.p50_us as f64 / 1e3,
            worker.encode.p95_us as f64 / 1e3,
            worker.encode.max_us as f64 / 1e3
        );
        eprintln!(
            "    worker capture → packetized: mean {:.2} / max {:.2} ms",
            worker.latency_sum_us as f64 / worker.encoded.max(1) as f64 / 1e3,
            worker.latency_max_us as f64 / 1e3
        );
        eprintln!("    capture → arrival:   {}", spread(&mut to_arrival));
        eprintln!("    arrival → decoded:   {}", spread(&mut decode));
        eprintln!("    capture → decoded:   {}", spread(&mut to_decoded));
        eprintln!("    decoded → painted:   {}", spread(&mut to_paint));
        eprintln!("    capture → painted:   {}", spread(&mut to_glass));
        eprintln!("    capture → painted, estimated clock: {}", spread(&mut to_glass_estimated));
        eprintln!(
            "    estimate off the shared clock by: {}  ({:?})",
            spread(&mut estimate_error),
            handle.stats().clock
        );
        eprintln!("    input at worker → capture showing it: {}", spread(&mut reach_to_capture));
        eprintln!(
            "  frames: captured {} encoded {} dropped {} | client decoded {} lost {} fec {} nacks {} refreshes {} decode errors {} | shown {} skipped {} repeats {} late {} | inputs sent {seq} seen {seen}",
            worker.captured,
            worker.encoded,
            worker.dropped,
            stats.frames,
            stats.frames_lost,
            stats.frames_fec,
            stats.nacks,
            stats.refreshes,
            stats.decode_errors,
            pacing.presented,
            pacing.skipped,
            pacing.repeats,
            pacing.late
        );
        if !coded.is_empty() {
            eprintln!(
                "  stripes: client striped {}, seam tears {}",
                stats.striped, stats.seam_tears
            );
        }
        let decisions = decisions.lock().clone();
        let verdicts: Vec<String> = decisions
            .chunk_by(|a, b| a == b)
            .map(|run| {
                let (verdict, bps) = run[0];
                format!("{:.1}({verdict:?}×{})", f64::from(bps) / 1e6, run.len())
            })
            .collect();
        let (bytes0, encoded0) = settled.unwrap_or_default();
        let measured = seconds as f64;
        let encoded = worker.encoded.saturating_sub(encoded0) as f64;
        let wire_bytes = wire.bytes().saturating_sub(bytes0) as f64;
        eprintln!(
            "  rate: {} → encoder at {:.1} Mbit/s, {:.1} frames/s encoded, {:.2} Mbit/s on the wire, {:.1} KiB a frame",
            verdicts.join(" "),
            worker.bitrate_bps as f64 / 1e6,
            encoded / measured,
            wire_bytes * 8.0 / measured / 1e6,
            wire_bytes / encoded.max(1.0) / 1024.0,
        );
        eprintln!(
            "  cadence: rung {} under a ceiling of {}, encoder fed {} a second at the last window, {} captures replaced in the mailbox, {} refinements",
            stream.shared.fps.load(Ordering::Relaxed),
            stream.shared.fps_ceiling.load(Ordering::Relaxed),
            stream.shared.fed_fps.load(Ordering::Relaxed),
            stream.shared.counters.superseded.load(Ordering::Relaxed),
            stream.shared.counters.refined.load(Ordering::Relaxed),
        );
        assert!(glass.capture.count > 0, "no frame was timed from its capture");
        assert!(glass.input.count > 0, "no input reached the glass");
        drop(handle);
        reporting.abort();
        stream.close().await;
    }

    /// The system's load average over the last minute, for the record.
    fn load_average() -> String {
        std::process::Command::new("/usr/sbin/sysctl")
            .args(["-n", "vm.loadavg"])
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .and_then(|text| text.split_whitespace().nth(1).map(str::to_owned))
            .unwrap_or_default()
    }

    /// Longer than any picture takes on a machine that is only busy, while nothing is inside
    /// VideoToolbox: past it the stream has stopped.
    const STALLED: Duration = Duration::from_secs(10);

    /// One geometry tick, as the app's loop runs it when the stream wakes it: the probe read
    /// off the runtime, and a build it starts waited for and put in.
    async fn follow<P: Platform>(stream: &mut Pipeline<P>) {
        let probe = tokio::task::spawn_blocking(stream.prober()).await.unwrap();
        let Some(mut rebuild) = stream.check_geometry(&probe) else { return };
        eprintln!("the geometry tick builds new sessions");
        match rebuild.built().await {
            Ok(built) => {
                let _told = stream.finish_rebuild(rebuild, built);
            }
            Err(e) => {
                eprintln!("the geometry tick's build failed: {e}");
                stream.rebuild_failed(&rebuild);
            }
        }
    }

    /// Wait for the client's next picture, on the stream and not on the clock: a slow machine
    /// only takes longer. Meanwhile the geometry is followed whenever the stream wakes it, as
    /// the app's loop follows it ([`follow`]), which is where a session the system took away
    /// is replaced.
    ///
    /// [`STALLED`] without a picture fails with what both ends saw (`what` says where the test
    /// was), unless the machine is what held the picture up. That is a frame inside
    /// VideoToolbox, being coded on the worker or decoded on the client (on a starved machine
    /// one keyframe at 756 × 492 has taken 12 s there), or a side that did not run at all over
    /// the wait: a worker that captured nothing, or a client task that read nothing or has not
    /// reported lately (`ScreenStats::reported_at`), which
    /// it does every 50 ms while it runs: a task that stopped just after a report still counts
    /// the datagrams it read before it. Waiting on the machine is
    /// the runner's to time out; a stream whose two sides ran and still sent no picture has
    /// stopped. A frame inside VideoToolbox counts as the machine's only until the worker's
    /// beat has charged it its patience (`ENCODE_STUCK`): past that the beat should have
    /// given it up and new sessions taken over, so one still inside has stopped the stream.
    /// `false` once the client's stream has ended.
    async fn next_or_stopped<P: Platform>(
        frames: &mut tokio::sync::watch::Receiver<Option<Arc<slopty_client::screen::Presentable>>>,
        stream: &mut Pipeline<P>,
        handle: &slopty_client::screen::ScreenHandle,
        what: impl Fn() -> String,
    ) -> bool {
        let wake = stream.geometry_wake();
        let ran = |stream: &Pipeline<P>| (stream.stats().captured, handle.stats().datagrams);
        let mut since = ran(stream);
        loop {
            tokio::select! {
                changed = frames.changed() => return changed.is_ok(),
                () = wake.notified() => follow(stream).await,
                () = tokio::time::sleep(STALLED) => {
                    let coding = stream.shared.coding();
                    // The beat gives an encode up once it has charged its patience; one charged
                    // well past it was not given up on, and the stream has stopped there.
                    if let Some((charged, patience)) = coding
                        && charged > patience + crate::screen::micros(crate::screen::STUCK_CREDIT)
                    {
                        panic!(
                            "no picture for {STALLED:?} {}: a frame inside VideoToolbox for {} ms of the worker's own time, past its {} ms, was not given up on: worker {:?}",
                            what(),
                            charged / 1_000,
                            patience / 1_000,
                            stream.stats()
                        );
                    }
                    let client = handle.stats();
                    let now = ran(stream);
                    let lately = client.reported_at.is_some_and(|at| at.elapsed() < Duration::from_secs(1));
                    let (captured, reported) = (now.0 > since.0, now.1 > since.1 && lately);
                    since = now;
                    let held = if coding.is_some() {
                        "a frame being coded in VideoToolbox"
                    } else if client.decoding > 0 {
                        "frames being decoded in VideoToolbox"
                    } else if !captured {
                        "a worker that captured nothing"
                    } else if !reported {
                        "a client task that reported nothing"
                    } else {
                        panic!(
                            "no picture for {STALLED:?} {}, both sides running and none inside VideoToolbox: client {client:?}, worker {:?}",
                            what(),
                            stream.stats()
                        );
                    };
                    eprintln!(
                        "no picture for {STALLED:?} {}: {held}, waiting on it ({} encodes given up on)",
                        what(),
                        stream.stats().encoders_replaced
                    );
                }
            }
        }
    }

    /// The pixel format of the next picture the client decodes whose format `wanted` accepts,
    /// and whether its input strip read back; `None` when none comes within 30 s.
    async fn next_picture(
        frames: &mut tokio::sync::watch::Receiver<Option<Arc<slopty_client::screen::Presentable>>>,
        wanted: impl Fn(u32) -> bool,
    ) -> Option<(u32, bool)> {
        let wait = async {
            while frames.changed().await.is_ok() {
                let Some(frame) = frames.borrow_and_update().clone() else { continue };
                let image = frame.frame.image.as_cv();
                let format = objc2_core_video::CVPixelBufferGetPixelFormatType(image);
                if wanted(format) {
                    return Some((format, inputs_shown(image).is_some()));
                }
            }
            None
        };
        tokio::time::timeout(Duration::from_secs(30), wait).await.ok().flatten()
    }

    /// A stream that asks for 4:4:4 is drawn as `xf44`, encoded as HEVC 4:4:4 and handed to the
    /// client's surface as the 4:4:4 picture it decoded, the strip still readable; once loss has
    /// cut the rate under the leave line, the geometry tick moves it to a 4:2:0 session and the
    /// client's pictures are NV12 again, and clean windows bring it back to 4:4:4 after the
    /// hold. Drawn, never captured.
    #[test]
    fn a_full_chroma_stream_arrives_as_444_and_follows_the_rate() {
        use objc2_core_video::{
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            kCVPixelFormatType_444YpCbCr10BiPlanarFullRange,
        };
        use slopty_proto::screen::{Chroma, ReceiverReport};
        let runtime =
            tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build();
        runtime.unwrap().block_on(async {
            let router = ScreenRouter::new();
            let wire = Arc::new(Wire::new(router.clone(), None, None));
            // A quarter of the drawn 3024 × 1964 panel, whatever display this Mac has: its enter
            // line is under the 12 Mbit/s a stream opens at, and eight cuts reach under its
            // leave line. A quarter of a 1024 × 768 panel (a CI runner's) never falls that far.
            let quality = Quality { scale: 0.25, chroma: Chroma::Full, ..Quality::default() };
            let (mut stream, opened) = Pipeline::<Synthetic>::open(
                STREAM,
                CaptureTarget::Display(DISPLAY.id),
                quality,
                wire,
                |_event| {},
            )
            .await
            .unwrap();
            let ScreenEvent::Opened { codec, width, height, .. } = opened else {
                panic!("{opened:?}")
            };
            assert_eq!(stream.chroma(), Chroma::Full, "{width}×{height} opens over the line");

            let control = stream.control();
            let answering = control.clone();
            // The client's own reports go unheard: the test drives the rate.
            let (reports_tx, mut reports) = mpsc::channel::<ClientMsg>(64);
            let drain = tokio::spawn(async move { while reports.recv().await.is_some() {} });
            let uplink = Uplink {
                control: reports_tx,
                feedback: Box::new(move |bytes| {
                    answer(&answering, &bytes);
                    true
                }),
                rtt: Box::new(|| Some(Duration::from_millis(1))),
            };
            let handle =
                spawn_screen(&tokio::runtime::Handle::current(), &router, STREAM, codec, uplink);
            let mut frames = handle.frames();

            let full = kCVPixelFormatType_444YpCbCr10BiPlanarFullRange;
            let got = next_picture(&mut frames, |_any| true).await;
            assert_eq!(got, Some((full, true)), "the first picture is 4:4:4 and reads back");

            // Eight decisions of heavy loss, each a cut to 75 %.
            let lossy =
                ReceiverReport { frames_ok: 3, frames_lost: 3, ..ReceiverReport::default() };
            let targets: Vec<u32> = (0..80)
                .filter_map(|_| control.report(&lossy, None))
                .map(|decision| decision.target_bps)
                .collect();
            let probe = tokio::task::spawn_blocking(stream.prober()).await.unwrap();
            let down = Instant::now();
            let mut rebuild =
                stream.check_geometry(&probe).expect("the geometry tick rebuilds for the chroma");
            let encoder = rebuild.built().await.unwrap();
            assert_eq!(stream.finish_rebuild(rebuild, encoder), None, "the size stays");
            assert_eq!(stream.chroma(), Chroma::Subsampled, "after cuts to {targets:?}");

            let nv12 = kCVPixelFormatType_420YpCbCr8BiPlanarFullRange;
            let got = next_picture(&mut frames, |format| format != full).await;
            assert_eq!(got, Some((nv12, true)), "back on 4:2:0");
            let down = down.elapsed();

            // Clean windows grow the rate back over the enter line; once the hold is out the
            // geometry tick moves the stream to a 4:4:4 session again, the capture ahead of it.
            let clean = ReceiverReport { frames_ok: 3, ..ReceiverReport::default() };
            let mut decisions = 0;
            let rebuild = loop {
                assert!(decisions < 60, "never back on 4:4:4");
                decisions += 1;
                let target = (0..10).find_map(|_| control.report(&clean, None));
                let probe = tokio::task::spawn_blocking(stream.prober()).await.unwrap();
                let up = Instant::now();
                if let Some(rebuild) = stream.check_geometry(&probe) {
                    break (rebuild, target, up);
                }
            };
            let (mut rebuild, target, up) = rebuild;
            let encoder = rebuild.built().await.unwrap();
            assert_eq!(stream.finish_rebuild(rebuild, encoder), None);
            assert_eq!(stream.chroma(), Chroma::Full, "after {decisions} decisions, {target:?}");
            let got = next_picture(&mut frames, |format| format == full).await;
            assert_eq!(got, Some((full, true)), "4:4:4 again");
            eprintln!(
                "MEASURE chroma switch at {width}×{height}, rebuild → first decoded picture: \
                 4:4:4 → 4:2:0 {:.1} ms, 4:2:0 → 4:4:4 {:.1} ms",
                ms(down),
                ms(up.elapsed())
            );
            drop(handle);
            drain.abort();
            stream.close().await;
        });
    }

    /// The clock probes place every capture where the shared clock does, on loopback and
    /// behind 5 ms each way: the drawn screen streamed through the real encoder and decoder,
    /// the probes answered by the stream's control as the connection answers them, and each
    /// decoded picture's estimated capture compared with the exact one (both clocks are this
    /// Mac's). Within a millisecond at the median, and each never further off than the bound
    /// the estimate that timed it stated, plus the width of the two clock reads: the estimate
    /// read at the end may be tighter than the one a frame early on was placed by.
    #[test]
    fn the_clock_probes_time_captures_as_the_shared_clock_does() {
        let runtime =
            tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build();
        runtime.unwrap().block_on(async {
            for one_way in [Duration::ZERO, Duration::from_millis(5)] {
                let router = ScreenRouter::new();
                let line = (!one_way.is_zero()).then(|| {
                    let router = router.clone();
                    delay_line(one_way, move |datagrams: Vec<Bytes>, at| {
                        router.route_many(datagrams, at);
                    })
                });
                let wire = Arc::new(Wire::new(router.clone(), line, None));
                let quality = Quality { scale: 0.25, ..Quality::default() };
                let (mut stream, opened) = Pipeline::<Synthetic>::open(
                    STREAM,
                    CaptureTarget::Display(DISPLAY.id),
                    quality,
                    wire,
                    |_event| {},
                )
                .await
                .unwrap();
                let ScreenEvent::Opened { codec, .. } = opened else { panic!("{opened:?}") };
                let control = stream.control();
                let feedback =
                    delay_line(one_way, move |bytes: Bytes, _at| answer(&control, &bytes));
                let (reports_tx, mut reports) = mpsc::channel::<ClientMsg>(64);
                let drain = tokio::spawn(async move { while reports.recv().await.is_some() {} });
                let rtt = one_way * 2;
                let uplink = Uplink {
                    control: reports_tx,
                    feedback: Box::new(move |bytes| feedback.send((Instant::now(), bytes)).is_ok()),
                    rtt: Box::new(move || Some(rtt)),
                };
                let handle = spawn_screen(
                    &tokio::runtime::Handle::current(),
                    &router,
                    STREAM,
                    codec,
                    uplink,
                );
                let mut frames = handle.frames();
                let clocks = anchor();
                // A stream that stops handing out pictures ([`next_or_stopped`]), or whose probes
                // never come back while it does, fails with what both ends saw.
                let mut errors = Vec::new();
                let mut widest = Duration::ZERO;
                let mut seen = 0_u32;
                while errors.len() < 60 {
                    let timed = errors.len();
                    let what = || format!("after {seen}, {timed} timed, {one_way:?} each way");
                    let next = next_or_stopped(&mut frames, &mut stream, &handle, what).await;
                    assert!(next, "the client's stream ended");
                    let Some(frame) = frames.borrow_and_update().clone() else { continue };
                    seen += 1;
                    let (Some(estimated), Some(exact)) =
                        (frame.stamp.captured, clocks.captured(frame.stamp.pts_us))
                    else {
                        assert!(seen < 600, "{seen} pictures and no probe back: {:?}", handle.stats());
                        continue;
                    };
                    let off = estimated.at.duration_since(exact).max(exact.duration_since(estimated.at));
                    assert!(
                        off <= estimated.within + Duration::from_millis(1),
                        "{off:?} off, placed within {:?}, {one_way:?} each way",
                        estimated.within
                    );
                    widest = widest.max(estimated.within);
                    errors.push(off);
                }
                let estimate = handle.stats().clock.expect("the probes came back");
                let max = errors.iter().max().copied().unwrap_or_default();
                errors.sort_unstable();
                let p50 = percentile(&errors, 50);
                eprintln!(
                    "MEASURE clock estimate at {:.0} ms each way: off the shared clock p50 {:.3} / max {:.3} ms over {} frames, widest bound a frame was placed within {:.3} ms, last bound {:.3} ms, rtt {:.3} ms, {} encodes given up on",
                    ms(one_way),
                    ms(p50),
                    ms(max),
                    errors.len(),
                    ms(widest),
                    ms(estimate.bound),
                    ms(estimate.rtt),
                    stream.stats().encoders_replaced
                );
                assert!(p50 <= Duration::from_millis(1), "p50 {p50:?}, {estimate:?}");
                drop(handle);
                drain.abort();
                stream.close().await;
            }
        });
    }

    /// The drawn screen whose first encoder session the system takes away at its
    /// [`TAKEN_AFTER`]th frame, as VideoToolbox took one on a starved machine
    /// (MEASUREMENTS.md, "an encoder session that malfunctions").
    enum Malfunctioning {}

    impl Platform for Malfunctioning {
        type Audio = slopty_codec::Opus;
        type Capture = Studio;
        type Input = Poke;
        type Video = TakenAway;
    }

    /// The frames [`Malfunctioning`]'s first session codes before it is taken away.
    const TAKEN_AFTER: u32 = 20;

    /// How long [`Malfunctioning`]'s replacement takes to build: a dozen captures, each refused
    /// by the session it replaces.
    const TAKEN_REBUILD: Duration = Duration::from_millis(200);

    /// Sessions built for [`Malfunctioning`].
    static TAKEN_BUILT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    /// VideoToolbox, the first session of which answers every frame from its
    /// [`TAKEN_AFTER`]th on as the codec answers for a session the system took away.
    struct TakenAway {
        session: slopty_codec::VideoToolbox,
        /// Frames it codes before it is taken away; `None` for one that keeps coding.
        lasts: Option<u32>,
        frames: std::sync::atomic::AtomicU32,
    }

    impl slopty_codec::VideoEncoder for TakenAway {
        type Image = PixelBuffer;

        fn new(
            config: slopty_codec::EncoderConfig,
            sink: impl Fn(slopty_codec::EncodedPacket) + Send + Sync + 'static,
        ) -> Result<Self, slopty_codec::CodecError> {
            let session = slopty_codec::VideoToolbox::new(config, sink)?;
            let built = TAKEN_BUILT.fetch_add(1, Ordering::Relaxed);
            if built == 1 {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "stands in for a replacement built on a busy machine, off the runtime"
                )]
                std::thread::sleep(TAKEN_REBUILD);
            }
            Ok(Self { session, lasts: (built == 0).then_some(TAKEN_AFTER), frames: 0.into() })
        }

        fn encode(
            &self,
            image: &PixelBuffer,
            pts_us: u64,
            options: &slopty_codec::FrameOptions,
        ) -> Result<(), slopty_codec::CodecError> {
            let frame = self.frames.fetch_add(1, Ordering::Relaxed);
            if self.lasts.is_some_and(|lasts| frame >= lasts) {
                let status = objc2_video_toolbox::kVTVideoEncoderMalfunctionErr;
                return Err(slopty_codec::CodecError::EncoderLost { status });
            }
            self.session.encode(image, pts_us, options)
        }

        fn set_bitrate(&self, bps: u32) -> Result<(), slopty_codec::CodecError> {
            self.session.set_bitrate(bps)
        }

        fn set_frame_rate(&self, fps: u16) -> Result<(), slopty_codec::CodecError> {
            self.session.set_frame_rate(fps)
        }

        fn set_temporal_layers(&self, on: bool) -> Result<bool, slopty_codec::CodecError> {
            self.session.set_temporal_layers(on)
        }

        fn frames_dropped(&self) -> u64 {
            self.session.frames_dropped()
        }
    }

    /// A session the system takes away is replaced once, and the stream goes on: the drawn
    /// screen through the real encoder and decoder, its first session refusing every frame from
    /// its twentieth on ([`TakenAway`]). The refusal wakes the geometry tick, which builds new
    /// sessions at the size in force; the captures the lost session refuses while they build
    /// ([`TAKEN_REBUILD`]) ask for no more, their keyframe answers the client, and a hundred
    /// pictures come after it.
    #[test]
    fn a_session_taken_away_is_replaced_and_the_stream_goes_on() {
        let runtime =
            tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build();
        runtime.unwrap().block_on(async {
            let router = ScreenRouter::new();
            let wire = Arc::new(Wire::new(router.clone(), None, None));
            let quality = Quality { scale: 0.25, ..Quality::default() };
            let (mut stream, opened) = Pipeline::<Malfunctioning>::open(
                STREAM,
                CaptureTarget::Display(DISPLAY.id),
                quality,
                wire,
                |_event| {},
            )
            .await
            .unwrap();
            let ScreenEvent::Opened { codec, width, height, .. } = opened else {
                panic!("{opened:?}")
            };
            let control = stream.control();
            let (reports_tx, mut reports) = mpsc::channel::<ClientMsg>(64);
            let drain = tokio::spawn(async move { while reports.recv().await.is_some() {} });
            let uplink = Uplink {
                control: reports_tx,
                feedback: Box::new(move |bytes| {
                    answer(&control, &bytes);
                    true
                }),
                rtt: Box::new(|| Some(Duration::from_millis(1))),
            };
            let handle =
                spawn_screen(&tokio::runtime::Handle::current(), &router, STREAM, codec, uplink);
            let mut frames = handle.frames();
            let (mut before, mut after) = (0_u32, 0_u32);
            while after < 100 {
                let what = || format!("after {before} pictures before the loss, {after} after");
                let next = next_or_stopped(&mut frames, &mut stream, &handle, what).await;
                assert!(next, "the client's stream ended");
                if TAKEN_BUILT.load(Ordering::Relaxed) > 1 {
                    after += 1;
                } else {
                    before += 1;
                }
            }
            let built = TAKEN_BUILT.load(Ordering::Relaxed);
            let worker = stream.stats();
            eprintln!(
                "{before} pictures, the session taken away, {after} on {built} sessions; worker {worker:?}"
            );
            assert!(before <= TAKEN_AFTER, "the first session's pictures: {before}");
            assert_eq!(built, 2, "one replacement, built once");
            let picture = frames.borrow().clone().expect("a picture");
            assert_eq!(
                picture.size(),
                (usize::try_from(width).unwrap(), usize::try_from(height).unwrap()),
                "at the size in force"
            );
            drop(handle);
            drain.abort();
            stream.close().await;
        });
    }

    /// The drawn screen whose first encoder session never comes back from its
    /// [`WEDGED_AT`]th submit until the test lets it, as one did on a hosted virtual Mac
    /// (MEASUREMENTS.md, "an encode that never returned").
    enum Wedging {}

    impl Platform for Wedging {
        type Audio = slopty_codec::Opus;
        type Capture = Studio;
        type Input = Poke;
        type Video = Wedged;
    }

    /// The submit of [`Wedging`]'s first session that does not come back.
    const WEDGED_AT: u32 = 20;

    /// Sessions built for [`Wedging`].
    static WEDGED_BUILT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    /// When the submit that does not come back went in.
    static WEDGED_SINCE: Mutex<Option<Instant>> = Mutex::new(None);

    /// Whether the test let the stuck submit come back, and the wait for it.
    static UNWEDGED: Mutex<bool> = Mutex::new(false);
    static UNWEDGE: parking_lot::Condvar = parking_lot::Condvar::new();

    /// VideoToolbox, the first session of which waits inside its [`WEDGED_AT`]th submit until
    /// [`UNWEDGED`].
    struct Wedged {
        session: slopty_codec::VideoToolbox,
        wedges: bool,
        frames: std::sync::atomic::AtomicU32,
    }

    impl slopty_codec::VideoEncoder for Wedged {
        type Image = PixelBuffer;

        fn new(
            config: slopty_codec::EncoderConfig,
            sink: impl Fn(slopty_codec::EncodedPacket) + Send + Sync + 'static,
        ) -> Result<Self, slopty_codec::CodecError> {
            let session = slopty_codec::VideoToolbox::new(config, sink)?;
            let built = WEDGED_BUILT.fetch_add(1, Ordering::Relaxed);
            Ok(Self { session, wedges: built == 0, frames: 0.into() })
        }

        fn encode(
            &self,
            image: &PixelBuffer,
            pts_us: u64,
            options: &slopty_codec::FrameOptions,
        ) -> Result<(), slopty_codec::CodecError> {
            if self.wedges && self.frames.fetch_add(1, Ordering::Relaxed) == WEDGED_AT {
                *WEDGED_SINCE.lock() = Some(Instant::now());
                let mut unwedged = UNWEDGED.lock();
                while !*unwedged {
                    UNWEDGE.wait(&mut unwedged);
                }
            }
            self.session.encode(image, pts_us, options)
        }

        fn set_bitrate(&self, bps: u32) -> Result<(), slopty_codec::CodecError> {
            self.session.set_bitrate(bps)
        }

        fn set_frame_rate(&self, fps: u16) -> Result<(), slopty_codec::CodecError> {
            self.session.set_frame_rate(fps)
        }

        fn set_temporal_layers(&self, on: bool) -> Result<bool, slopty_codec::CodecError> {
            self.session.set_temporal_layers(on)
        }

        fn frames_dropped(&self) -> u64 {
            self.session.frames_dropped()
        }
    }

    /// A submit that never comes back costs the stream its sessions and not its pictures: the
    /// drawn screen through the real encoder and decoder, its first session waiting inside its
    /// twentieth submit ([`Wedged`]). The beat gives that encode up once it has charged it
    /// [`ENCODE_STUCK`](crate::screen::ENCODE_STUCK) of the worker's own time, not before;
    /// the geometry tick builds new sessions, whose keyframe answers the client, and a hundred
    /// pictures come after it. [`next_or_stopped`] fails the test if the beat ever charges the
    /// encode past its patience without giving it up.
    #[test]
    fn a_submit_that_never_comes_back_is_given_up_and_the_pictures_go_on() {
        use crate::screen::{ENCODE_STUCK, STUCK_CREDIT};
        let runtime =
            tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build();
        runtime.unwrap().block_on(async {
            let router = ScreenRouter::new();
            let wire = Arc::new(Wire::new(router.clone(), None, None));
            let quality = Quality { scale: 0.25, ..Quality::default() };
            let (mut stream, opened) = Pipeline::<Wedging>::open(
                STREAM,
                CaptureTarget::Display(DISPLAY.id),
                quality,
                wire,
                |_event| {},
            )
            .await
            .unwrap();
            let ScreenEvent::Opened { codec, width, height, .. } = opened else {
                panic!("{opened:?}")
            };
            let control = stream.control();
            let (reports_tx, mut reports) = mpsc::channel::<ClientMsg>(64);
            let drain = tokio::spawn(async move { while reports.recv().await.is_some() {} });
            let uplink = Uplink {
                control: reports_tx,
                feedback: Box::new(move |bytes| {
                    answer(&control, &bytes);
                    true
                }),
                rtt: Box::new(|| Some(Duration::from_millis(1))),
            };
            let handle =
                spawn_screen(&tokio::runtime::Handle::current(), &router, STREAM, codec, uplink);
            let mut frames = handle.frames();
            let (mut before, mut after) = (0_u32, 0_u32);
            let mut resumed = None;
            while after < 100 {
                let what = || format!("after {before} pictures before the stuck submit, {after} after");
                let next = next_or_stopped(&mut frames, &mut stream, &handle, what).await;
                assert!(next, "the client's stream ended");
                // A picture of the first session's may still arrive after its stuck submit
                // went in; the replacement is built only once that was given up on.
                let since = *WEDGED_SINCE.lock();
                match since.filter(|_| WEDGED_BUILT.load(Ordering::Relaxed) > 1) {
                    Some(since) => {
                        resumed.get_or_insert_with(|| since.elapsed());
                        after += 1;
                    }
                    None => before += 1,
                }
            }
            let resumed = resumed.unwrap_or_default();
            let built = WEDGED_BUILT.load(Ordering::Relaxed);
            let worker = stream.stats();
            eprintln!(
                "MEASURE a submit that never came back: {before} pictures, then the first after it {:.0} ms on, {after} on {built} sessions; worker {worker:?}",
                ms(resumed)
            );
            assert!(before <= WEDGED_AT + 1, "the first session's pictures: {before}");
            assert_eq!(worker.encoders_replaced, 1, "given up on once");
            assert_eq!(built, 2, "one replacement, built once");
            assert!(
                resumed >= ENCODE_STUCK.saturating_sub(STUCK_CREDIT),
                "given up on after its patience, not {resumed:?}"
            );
            let picture = frames.borrow().clone().expect("a picture");
            assert_eq!(
                picture.size(),
                (usize::try_from(width).unwrap(), usize::try_from(height).unwrap()),
                "at the size in force"
            );
            *UNWEDGED.lock() = true;
            UNWEDGE.notify_all();
            drop(handle);
            drain.abort();
            stream.close().await;
        });
    }

    /// The worker's end of the link with one frame's bitstream garbled on the way: every data
    /// fragment of frame `frame` has its back half flipped, so it arrives whole and fails to
    /// decode.
    struct Garble {
        wire: Wire,
        frame: u32,
        garbled: std::sync::atomic::AtomicU32,
    }

    impl DatagramSink for Garble {
        fn send(&self, datagrams: &[Bytes]) -> Result<(), Refused> {
            let garble = |datagram: &Bytes| {
                let header = slopty_proto::media::MediaHeader::parse(datagram).map(|(h, _)| *h);
                let ours = header.is_some_and(|h| {
                    h.kind() == Some(slopty_proto::media::Kind::VideoData)
                        && h.frame.get() == self.frame
                });
                if !ours {
                    return datagram.clone();
                }
                self.garbled.fetch_add(1, Ordering::Relaxed);
                let mut bytes = datagram.to_vec();
                let half = bytes.len() / 2;
                for byte in bytes.iter_mut().skip(half) {
                    *byte ^= 0x5a;
                }
                Bytes::from(bytes)
            };
            let datagrams: Vec<Bytes> = datagrams.iter().map(garble).collect();
            self.wire.send(&datagrams)
        }

        fn max_size(&self) -> Option<usize> {
            self.wire.max_size()
        }

        fn held(&self) -> usize {
            self.wire.held()
        }

        fn cwnd(&self) -> u64 {
            self.wire.cwnd()
        }

        fn is_closed(&self) -> bool {
            false
        }
    }

    /// A frame that arrives whole and does not decode costs a refresh, and the stream goes on:
    /// the drawn screen through the real encoder and decoder, its twentieth frame garbled on the
    /// way ([`Garble`]), the client's refresh answered by the stream's control. The decoder
    /// fails on it, the client asks, and a hundred pictures come after it.
    #[test]
    fn a_frame_that_does_not_decode_is_refreshed_and_the_stream_goes_on() {
        let runtime =
            tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build();
        runtime.unwrap().block_on(async {
            let router = ScreenRouter::new();
            let wire = Wire::new(router.clone(), None, None);
            let garble = Arc::new(Garble { wire, frame: 20, garbled: 0.into() });
            let quality = Quality { scale: 0.25, ..Quality::default() };
            let (mut stream, opened) = Pipeline::<Synthetic>::open(
                STREAM,
                CaptureTarget::Display(DISPLAY.id),
                quality,
                Arc::<Garble>::clone(&garble),
                |_event| {},
            )
            .await
            .unwrap();
            let ScreenEvent::Opened { codec, .. } = opened else { panic!("{opened:?}") };
            let control = stream.control();
            let (reports_tx, mut reports) = mpsc::channel::<ClientMsg>(64);
            let drain = tokio::spawn(async move { while reports.recv().await.is_some() {} });
            let uplink = Uplink {
                control: reports_tx,
                feedback: Box::new(move |bytes| {
                    answer(&control, &bytes);
                    true
                }),
                rtt: Box::new(|| Some(Duration::from_millis(1))),
            };
            let handle =
                spawn_screen(&tokio::runtime::Handle::current(), &router, STREAM, codec, uplink);
            let mut frames = handle.frames();
            let (mut pictures, mut after) = (0_u32, 0_u32);
            while after < 100 {
                let what = || format!("after {pictures} pictures, {after} since the failure");
                let next = next_or_stopped(&mut frames, &mut stream, &handle, what).await;
                assert!(next, "the client's stream ended");
                pictures += 1;
                if handle.stats().decode_errors > 0 {
                    after += 1;
                }
            }
            let client = handle.stats();
            eprintln!(
                "garbled {} datagrams of frame 20: {} decode error(s), status {:?}, {} refresh(es), {pictures} pictures",
                garble.garbled.load(Ordering::Relaxed),
                client.decode_errors,
                client.decode_failure,
                client.refreshes
            );
            assert!(garble.garbled.load(Ordering::Relaxed) > 0, "frame 20 went by: {client:?}");
            assert!(client.refreshes > 0, "the failure asked for a refresh: {client:?}");
            assert!(client.decode_failure.is_some(), "VideoToolbox named it: {client:?}");
            drop(handle);
            drain.abort();
            stream.close().await;
        });
    }

    /// `SLOPTY_SYNTHETIC_SCREEN=1` and nothing else turns the drawn screen on.
    #[test]
    fn only_a_one_switches_the_drawn_screen_on() {
        assert!(switched_on(Some("1")));
        for off in [None, Some(""), Some("0"), Some("yes")] {
            assert!(!switched_on(off), "{off:?}");
        }
    }

    /// The drawn screen lists its one display and its windows, the same on every Mac, and a
    /// window takes a resize, held to the display and a floor: the window's bounds and the next
    /// listing have the new size.
    #[test]
    fn the_drawn_screen_lists_its_windows_and_takes_a_resize() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
        runtime.unwrap().block_on(async {
            let listing = Pipeline::<Synthetic>::listing().await.unwrap();
            let ScreenEvent::Listing { windows, displays } = listing else { panic!("{listing:?}") };
            assert_eq!(displays, [DISPLAY]);
            let listed: Vec<_> =
                windows.iter().map(|w| (w.id, w.title.as_str(), w.w, w.h)).collect();
            let expected: Vec<_> =
                WINDOWS.iter().map(|p| (p.id, p.title, p.size.0, p.size.1)).collect();
            assert_eq!(listed, expected);
            assert!(windows.iter().all(|w| w.on_screen && w.app == APP), "{windows:?}");

            let terminal = WINDOWS[1].id;
            Pipeline::<Synthetic>::resize_window(terminal, 640.0, 400.0).unwrap();
            let bounds = Studio::window_bounds(terminal).unwrap();
            assert_eq!((bounds.w, bounds.h), (640.0, 400.0));
            Pipeline::<Synthetic>::resize_window(terminal, 10.0, 1e6).unwrap();
            let state = Studio::window_state(terminal).unwrap();
            assert_eq!((state.bounds.w, state.bounds.h), (160.0, f64::from(DISPLAY.h)));
            let unknown = TargetWindow { bounds, title: Some("Mail".to_owned()) };
            assert!(Studio::resize_window(0, &unknown, 640.0, 400.0).is_err());
            assert!(Studio::window_bounds(WindowId(1)).is_none(), "no window of this Mac");
        });
    }

    /// Mean luma, 0–255, of a decoded NV12 picture's last row and last column: the drawn
    /// desktop is at [`DESKTOP_LEVEL`] there, and a band of padding that showed would be black.
    fn edge_luma(image: &objc2_core_video::CVPixelBuffer) -> f64 {
        use objc2_core_video::{
            CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
            CVPixelBufferGetHeight, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
            CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
        };
        let (w, h) = (CVPixelBufferGetWidth(image), CVPixelBufferGetHeight(image));
        // SAFETY: CoreVideo rule: the planes are read between a lock and its unlock.
        let locked =
            unsafe { CVPixelBufferLockBaseAddress(image, CVPixelBufferLockFlags::ReadOnly) };
        assert_eq!(locked, 0, "lock");
        let stride = CVPixelBufferGetBytesPerRowOfPlane(image, 0);
        let base = CVPixelBufferGetBaseAddressOfPlane(image, 0).cast::<u8>();
        // SAFETY: the buffer is locked, so its luma plane is mapped for `stride * h` bytes.
        let plane = unsafe { std::slice::from_raw_parts(base, stride * h) };
        let edge = (0..w).map(|x| (x, h - 1)).chain((0..h).map(|y| (w - 1, y)));
        let (sum, n) = edge.fold((0_u64, 0_u64), |(sum, n), (x, y)| {
            (sum + u64::from(plane[y * stride + x]), n + 1)
        });
        // SAFETY: matches the lock above.
        let _unlocked =
            unsafe { CVPixelBufferUnlockBaseAddress(image, CVPixelBufferLockFlags::ReadOnly) };
        sum as f64 / n.max(1) as f64
    }

    /// The level the canvas draws its desktop at, around its page.
    const DESKTOP_LEVEL: f64 = 40.0;

    /// A window of the drawn screen streams as a canvas of its own size through the real
    /// encoder, and the client decodes it at that size with its strip readable, whether or not
    /// the size is a multiple of 16: at a third of its size the editor's 854 × 534 is coded as
    /// 864 × 544 and its SPS crops it back, and its edges are the desktop, not the padding.
    /// Drawn, never captured.
    #[test]
    fn a_drawn_window_streams_at_its_own_size() {
        for (scale, size) in [(0.25, (640, 400)), (1.0 / 3.0, (854, 534))] {
            a_drawn_window_streams_at(scale, size);
        }
    }

    fn a_drawn_window_streams_at(scale: f32, size: (u32, u32)) {
        let runtime =
            tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build();
        runtime.unwrap().block_on(async {
            let Placed { id: editor, .. } = WINDOWS[0];
            let router = ScreenRouter::new();
            let wire = Arc::new(Wire::new(router.clone(), None, None));
            let quality = Quality { scale, ..Quality::default() };
            let (stream, opened) = Pipeline::<Synthetic>::open(
                STREAM,
                CaptureTarget::Window(editor),
                quality,
                wire,
                |_event| {},
            )
            .await
            .unwrap();
            let ScreenEvent::Opened { codec, width, height, .. } = opened else {
                panic!("{opened:?}")
            };
            assert_eq!((width, height), size, "at {scale}");
            let (reports_tx, mut reports) = mpsc::channel::<ClientMsg>(64);
            let drain = tokio::spawn(async move { while reports.recv().await.is_some() {} });
            let control = stream.control();
            let uplink = Uplink {
                control: reports_tx,
                feedback: Box::new(move |bytes| {
                    answer(&control, &bytes);
                    true
                }),
                rtt: Box::new(|| Some(Duration::from_millis(1))),
            };
            let handle =
                spawn_screen(&tokio::runtime::Handle::current(), &router, STREAM, codec, uplink);
            let mut frames = handle.frames();
            let got = next_picture(&mut frames, |_any| true).await;
            assert!(got.is_some_and(|(_format, strip)| strip), "a readable picture: {got:?}");
            let picture = frames.borrow().clone().expect("the picture");
            let image = picture.frame.image.as_cv();
            assert_eq!(
                (
                    objc2_core_video::CVPixelBufferGetWidth(image),
                    objc2_core_video::CVPixelBufferGetHeight(image)
                ),
                (usize::try_from(width).unwrap(), usize::try_from(height).unwrap())
            );
            let edge = edge_luma(image);
            assert!((edge - DESKTOP_LEVEL).abs() < 8.0, "{width}×{height}: edge luma {edge:.1}");
            drop(handle);
            drain.abort();
            stream.close().await;
        });
    }

    /// A decoded picture's luma plane, row by row with no stride padding, and its width.
    fn luma(image: &objc2_core_video::CVPixelBuffer) -> (usize, Vec<u8>) {
        use objc2_core_video::{
            CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
            CVPixelBufferGetHeight, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
            CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
        };
        let (w, h) = (CVPixelBufferGetWidth(image), CVPixelBufferGetHeight(image));
        // SAFETY: CoreVideo rule: the planes are read between a lock and its unlock.
        let locked =
            unsafe { CVPixelBufferLockBaseAddress(image, CVPixelBufferLockFlags::ReadOnly) };
        assert_eq!(locked, 0, "lock");
        let stride = CVPixelBufferGetBytesPerRowOfPlane(image, 0);
        let base = CVPixelBufferGetBaseAddressOfPlane(image, 0).cast::<u8>();
        // SAFETY: the buffer is locked, so its luma plane is mapped for `stride * h` bytes.
        let plane = unsafe { std::slice::from_raw_parts(base, stride * h) };
        let rows = plane.chunks(stride).flat_map(|row| &row[..w]).copied().collect();
        // SAFETY: matches the lock above.
        let _unlocked =
            unsafe { CVPixelBufferUnlockBaseAddress(image, CVPixelBufferLockFlags::ReadOnly) };
        (w, rows)
    }

    /// Mean absolute luma difference between the rows both stripes code, the lower stripe's
    /// picture moved `shift` rows against the top one's: row `y` of the picture is the top
    /// picture's row `y` and the lower picture's row `y - lower_top`.
    fn overlap_error(
        top: &[u8],
        lower: &[u8],
        width: usize,
        lower_top: usize,
        shift: isize,
    ) -> f64 {
        let (top_rows, lower_rows) = (top.len() / width, lower.len() / width);
        let (mut sum, mut n) = (0_u64, 0_u64);
        for y in lower_top..top_rows {
            let Some(from) = (y - lower_top).checked_add_signed(shift) else { continue };
            if from >= lower_rows {
                continue;
            }
            let (a, b) = (&top[y * width..][..width], &lower[from * width..][..width]);
            sum += a.iter().zip(b).map(|(a, b)| u64::from(a.abs_diff(*b))).sum::<u64>();
            n += width as u64;
        }
        sum as f64 / n.max(1) as f64
    }

    /// The overlap error of a capture's two stripe pictures at every shift from −6 to 6 rows,
    /// the lower stripe's coded from `lower_top`.
    fn overlap_errors(
        top: &[u8],
        lower: &[u8],
        width: usize,
        lower_top: usize,
    ) -> Vec<(isize, f64)> {
        (-6..=6).map(|shift| (shift, overlap_error(top, lower, width, lower_top, shift))).collect()
    }

    /// Whether the stripes meet unshifted: no shift matches as well as none, by a quarter.
    ///
    /// The unshifted error is the two sessions' coding noise, and that is not small next to the
    /// page's own difference from one row to the next in the first frames after the keyframe,
    /// or when a busy encoder is fed captures further apart: 3.96 against 7.78 at the second
    /// frame here, where the stream's later frames read 0.2 against 9. So the margin is a ratio
    /// and not a multiple plus a constant, which failed on that frame. A stripe a row off moves
    /// the curve's minimum off zero whatever the noise, and the test checks this rule says so
    /// on every picture it passes.
    fn meets_unshifted(errors: &[(isize, f64)]) -> bool {
        let aligned = errors.iter().find(|(shift, _)| *shift == 0).map_or(f64::INFINITY, |e| e.1);
        errors.iter().filter(|(shift, _)| *shift != 0).all(|&(_, error)| error > 1.25 * aligned)
    }

    /// A display coded as two stripes reaches the client as two pictures of one capture, each
    /// from a session of its own, that meet at the seam row for row. The stripes the worker
    /// announces tile the picture; the client shows the top one's rows above the seam and the
    /// lower one's from the seam down; and over the rows both code, a capture's two pictures
    /// match best unshifted, by a margin, on text that scrolls 3 rows a frame, where the same
    /// pictures with the lower one moved a row match best shifted ([`meets_unshifted`]). A
    /// capture whose stripes went up apart, one late past the stitch's wait, is compared once
    /// both have been shown. Drawn, never captured.
    #[test]
    fn two_stripes_meet_at_the_seam_row_for_row() {
        let runtime =
            tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build();
        runtime.unwrap().block_on(async {
            let router = ScreenRouter::new();
            let wire = Arc::new(Wire::new(router.clone(), None, None));
            let (mut stream, opened) = Pipeline::<Synthetic>::open_padded(
                STREAM,
                CaptureTarget::Display(DISPLAY.id),
                Quality { chroma: Chroma::Subsampled, ..Quality::default() },
                wire,
                |_event| {},
                Coding { pad_to: None, stripes: Knob::On },
            )
            .await
            .unwrap();
            let ScreenEvent::Opened { codec, width, height, stripes, .. } = opened else {
                panic!("{opened:?}")
            };
            let [top, lower]: [Stripe; 2] = stripes.try_into().expect("two stripes");
            assert_eq!((top.media, lower.media), (STREAM, Stripe::media_of(STREAM, 1)));
            assert_eq!((top.coded_top, top.shown_top), (0, 0));
            assert_eq!(top.shown_rows, lower.shown_top, "the lower stripe shows from the seam");
            assert_eq!(lower.shown_top + lower.shown_rows, height, "down to the last row");
            assert_eq!(top.coded_top + top.coded_rows - lower.coded_top, 2 * OVERLAP);

            let (reports_tx, mut reports) = mpsc::channel::<ClientMsg>(64);
            let drain = tokio::spawn(async move { while reports.recv().await.is_some() {} });
            let control = stream.control();
            let uplink = Uplink {
                control: reports_tx,
                feedback: Box::new(move |bytes| {
                    answer(&control, &bytes);
                    true
                }),
                rtt: Box::new(|| Some(Duration::from_millis(1))),
            };
            let handle =
                spawn_screen(&tokio::runtime::Handle::current(), &router, STREAM, codec, uplink);
            let mut frames = handle.frames();
            let lower_top = usize::try_from(lower.coded_top).unwrap();
            // Each stripe's pictures as they went up, newest last, until the other stripe's
            // picture of the same capture has gone up too.
            let (mut tops, mut lowers) = (VecDeque::new(), VecDeque::new());
            let (mut checked, mut apart, mut last, mut pictures) = (0, 0, None, 0_u32);
            while checked < 12 {
                let what =
                    || format!("after {pictures} pictures, {checked} compared, {apart} apart");
                let next = next_or_stopped(&mut frames, &mut stream, &handle, what).await;
                assert!(next, "the client's stream ended");
                pictures += 1;
                assert!(
                    pictures < 600,
                    "{pictures} pictures, {checked} pairs: {:?}",
                    handle.stats()
                );
                let Some(picture) = frames.borrow_and_update().clone() else { continue };
                let Some(stitched) = &picture.stripes else { continue };
                assert_eq!(stitched.top_rows, top.shown_rows);
                assert_eq!(stitched.lower_from, lower.shown_from());
                assert_eq!(
                    picture.size(),
                    (usize::try_from(width).unwrap(), usize::try_from(height).unwrap()),
                    "the stripes together are the picture"
                );
                // One stripe late past the stitch's wait, or coded alone for a refresh, goes
                // up beside the other's previous picture: the client counts that, and the
                // rows cannot match across two captures of a scroll.
                if stitched.lower.pts_us != picture.frame.pts_us {
                    apart += 1;
                }
                for (kept, frame) in [(&mut tops, &picture.frame), (&mut lowers, &stitched.lower)] {
                    let newer = |newest: &slopty_codec::DecodedFrame| newest.pts_us < frame.pts_us;
                    if kept.back().is_none_or(newer) {
                        kept.push_back(frame.clone());
                    }
                    while kept.len() > 8 {
                        kept.pop_front();
                    }
                }
                let both = tops.iter().rev().find(|t| {
                    last.is_none_or(|last| t.pts_us > last)
                        && lowers.iter().any(|l| l.pts_us == t.pts_us)
                });
                let Some(top_picture) = both else { continue };
                let pts = top_picture.pts_us;
                let lower_picture = lowers.iter().find(|l| l.pts_us == pts).unwrap();
                last = Some(pts);
                let (w, top_luma) = luma(top_picture.image.as_cv());
                let (lower_w, lower_luma) = luma(lower_picture.image.as_cv());
                assert_eq!(w, lower_w);
                let errors = overlap_errors(&top_luma, &lower_luma, w, lower_top);
                let at = |shift| errors.iter().find(|e| e.0 == shift).map_or(f64::NAN, |e| e.1);
                eprintln!("seam: unshifted {:.2}, a row off {:.2} / {:.2}", at(0), at(-1), at(1));
                assert!(meets_unshifted(&errors), "the stripes meet unshifted: {errors:?}");
                let a_row_off = overlap_errors(&top_luma, &lower_luma, w, lower_top + 1);
                assert!(!meets_unshifted(&a_row_off), "a stripe a row off: {a_row_off:?}");
                checked += 1;
            }
            assert_eq!(checked, 12);
            eprintln!("stripes of two captures: {apart}, seam tears {}", handle.stats().seam_tears);
            drop(handle);
            drain.abort();
            stream.close().await;
        });
    }

    /// The drawn display streamed as the app streams it: opened at its full size, the client's
    /// reports and loss feedback answered, then asked for a quarter of that and back, a few
    /// times, each change a new encoder session. Every frame of every session decodes: no
    /// decode error, no refresh, nothing lost. Drawn, never captured.
    #[test]
    fn quality_changes_decode_without_a_refresh() {
        let runtime =
            tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build();
        runtime.unwrap().block_on(async {
            let router = ScreenRouter::new();
            let wire = Arc::new(Wire::new(router.clone(), None, None));
            let full = Quality { scale: 1.0, ..Quality::default() };
            let (mut stream, opened) = Pipeline::<Synthetic>::open(
                STREAM,
                CaptureTarget::Display(DISPLAY.id),
                full,
                wire,
                |_event| {},
            )
            .await
            .unwrap();
            let ScreenEvent::Opened { codec, .. } = opened else { panic!("{opened:?}") };
            let control = stream.control();
            let (reports_tx, mut reports) = mpsc::channel::<ClientMsg>(64);
            let reporting = {
                let control = control.clone();
                tokio::spawn(async move {
                    while let Some(msg) = reports.recv().await {
                        if let ClientMsg::Screen(ScreenRequest::Report { stream, report }) = msg {
                            let _decision = control.of_media(stream).report(&report, None);
                        }
                    }
                })
            };
            let answering = control.clone();
            let uplink = Uplink {
                control: reports_tx,
                feedback: Box::new(move |bytes| {
                    answer(&answering, &bytes);
                    true
                }),
                rtt: Box::new(|| Some(Duration::from_millis(1))),
            };
            let handle =
                spawn_screen(&tokio::runtime::Handle::current(), &router, STREAM, codec, uplink);
            let mut frames = handle.frames();
            assert!(next_picture(&mut frames, |_any| true).await.is_some(), "a first picture");
            // Counted from the first picture on: before it, a session slow to open (a loaded
            // machine) rightly asks again for the keyframe it waits for.
            let before = handle.stats();
            for step in 0..6 {
                tokio::time::sleep(Duration::from_millis(400)).await;
                let scale = if step % 2 == 0 { 0.25 } else { 0.5 };
                let quality = Quality { scale, ..full };
                let mut rebuild = stream.set_quality(&quality, None).expect("a new session");
                let encoder = rebuild.built().await.unwrap();
                assert_eq!(stream.finish_rebuild(rebuild, encoder), None);
            }
            tokio::time::sleep(Duration::from_millis(600)).await;
            let stats = handle.stats();
            let worker = stream.stats();
            eprintln!(
                "MEASURE quality changes: {} frames decoded, {} decode errors, {} refreshes, {} \
                 lost, {} nacks | worker encoded {} refreshes {} ({} idr, {} delta)",
                stats.frames,
                stats.decode_errors,
                stats.refreshes,
                stats.frames_lost,
                stats.nacks,
                worker.encoded,
                worker.refreshes,
                worker.ltr.refreshes_idr,
                worker.ltr.refreshes_delta,
            );
            assert_eq!(
                (
                    stats.decode_errors.saturating_sub(before.decode_errors),
                    stats.refreshes.saturating_sub(before.refreshes),
                    stats.frames_lost.saturating_sub(before.frames_lost),
                ),
                (0, 0, 0),
                "{stats:#?}"
            );
            drop(handle);
            reporting.abort();
            stream.close().await;
        });
    }

    /// A 3024 × 1964 NV12 picture whose every pixel moves from one index to the next.
    fn moving_picture(
        index: usize,
    ) -> objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer> {
        use std::ptr::{self, NonNull};

        use objc2_core_video::{
            CVPixelBufferCreate, CVPixelBufferGetBaseAddressOfPlane,
            CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferLockBaseAddress,
            CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
        };
        let mut raw: *mut objc2_core_video::CVPixelBuffer = ptr::null_mut();
        // SAFETY: CoreVideo, `CVPixelBufferCreate`: a valid out-pointer and no attributes.
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                BIG.0,
                BIG.1,
                kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
                None,
                NonNull::from(&mut raw),
            )
        };
        assert_eq!(status, 0);
        // SAFETY: the create call returned a +1 reference.
        let buffer =
            unsafe { objc2_core_foundation::CFRetained::from_raw(NonNull::new(raw).unwrap()) };
        // SAFETY: CoreVideo: the planes are written only between a lock and its unlock.
        let locked =
            unsafe { CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags::empty()) };
        assert_eq!(locked, 0);
        for plane in 0..2 {
            let base = CVPixelBufferGetBaseAddressOfPlane(&buffer, plane).cast::<u8>();
            let stride = CVPixelBufferGetBytesPerRowOfPlane(&buffer, plane);
            let rows = if plane == 0 { BIG.1 } else { BIG.1 / 2 };
            for y in 0..rows {
                // SAFETY: the plane is locked and row `y < rows` starts `y * stride` bytes into it.
                let start = unsafe { base.add(y * stride) };
                // SAFETY: the same, and a row spans `stride >= width` bytes.
                let row = unsafe { std::slice::from_raw_parts_mut(start, BIG.0) };
                for (x, cell) in row.iter_mut().enumerate() {
                    #[expect(clippy::cast_possible_truncation, reason = "a byte of a pattern")]
                    let luma = ((x * 3 + y * 5 + index * 11) % 256) as u8;
                    *cell = if plane == 0 { luma } else { 128 };
                }
            }
        }
        // SAFETY: matches the lock above.
        let unlocked =
            unsafe { CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags::empty()) };
        assert_eq!(unlocked, 0);
        buffer
    }

    /// The drawn display's size, where the encoder drops frames under real-time pressure.
    const BIG: (usize, usize) = (3024, 1964);

    /// Encode `steps` (keyframe, LTR refresh, acknowledged tokens) a display period apart at
    /// [`BIG`], decode what comes out in order, and say what each frame was: `K` a sync frame,
    /// `P` a delta, `r` a refresh, `t` its token, `!` a frame the decoder failed.
    fn frames_through(steps: &[(bool, bool, Vec<u64>)]) -> (String, usize) {
        use std::fmt::Write as _;

        use slopty_codec::{Chroma, Decoder, Encoder, EncoderConfig, FrameOptions};
        use slopty_proto::screen::VideoCodec;

        let (tx, rx) = std::sync::mpsc::channel();
        let config = EncoderConfig {
            width: u32::try_from(BIG.0).unwrap(),
            height: u32::try_from(BIG.1).unwrap(),
            codec: VideoCodec::Hevc,
            fps: 60,
            bitrate_bps: 12_000_000,
            chroma: Chroma::Subsampled,
        };
        let encoder = Encoder::new(config, move |packet| {
            let _gone = tx.send(packet);
        })
        .unwrap();
        for (i, (keyframe, refresh, acked)) in steps.iter().enumerate() {
            let options = FrameOptions {
                force_keyframe: *keyframe,
                force_ltr_refresh: *refresh,
                acked_ltr: acked.clone(),
            };
            let pts = (u64::try_from(i).unwrap() + 1) * 16_667;
            encoder.encode(&moving_picture(i), pts, &options).unwrap();
            // Paced as a capture is, so the encoder drops what it cannot keep up with.
            #[expect(
                clippy::disallowed_methods,
                reason = "a test's pacing between blocking encodes"
            )]
            std::thread::sleep(Duration::from_millis(17));
        }
        encoder.flush().unwrap();
        let packets: Vec<_> = rx.try_iter().collect();
        let (dtx, drx) = std::sync::mpsc::channel();
        let mut decoder = Decoder::with_outcomes(VideoCodec::Hevc, move |outcome| {
            let _gone = dtx.send(outcome.is_ok());
        });
        let mut line = String::new();
        let mut failed = 0;
        for packet in &packets {
            let submitted = decoder.decode(&packet.data.clone().into(), packet.pts_us).is_ok();
            let decoded = submitted && drx.recv_timeout(Duration::from_secs(5)) == Ok(true);
            failed += usize::from(!decoded);
            let index = (packet.pts_us / 16_667).saturating_sub(1);
            let _infallible = write!(
                line,
                " {index}:{}{}{}{}",
                if packet.keyframe { "K" } else { "P" },
                if packet.ltr_refresh { "r" } else { "" },
                packet.ltr_token.map_or(String::new(), |t| format!("t{t}")),
                if decoded { "" } else { "!" },
            );
        }
        (line, failed)
    }

    /// Why a refresh with nothing acknowledged goes out as a keyframe
    /// (`Shared::try_encode`): VideoToolbox answers `ForceLTRRefresh` with no acknowledged
    /// reference with a sync frame the frames after it cannot be decoded against, every one of
    /// them until the next keyframe; asked for a keyframe instead, every frame decodes. At
    /// 3024 × 1964 with the encoder dropping frames, as the drawn display streamed in the app
    /// (MEASUREMENTS.md, "a refresh with nothing acknowledged").
    #[test]
    #[ignore = "measurement: prints what VideoToolbox makes of the two requests"]
    fn a_refresh_with_nothing_acknowledged_breaks_the_frames_after_it() {
        let plain = || (false, false, Vec::new());
        let with = |refresh: (bool, bool, Vec<u64>)| {
            let mut steps = vec![(true, false, Vec::new()), plain(), plain(), plain(), refresh];
            steps.extend(std::iter::repeat_with(plain).take(12));
            steps
        };
        let (line, failed) = frames_through(&with((false, true, Vec::new())));
        eprintln!("MEASURE an LTR refresh with nothing acknowledged: {failed} failed:{line}");
        let (line, keyframe_failed) = frames_through(&with((true, false, Vec::new())));
        eprintln!("MEASURE a keyframe in its place: {keyframe_failed} failed:{line}");
        assert_eq!(keyframe_failed, 0, "{line}");
    }

    /// A quality change that moves only the frame rate (the client's view went to a screen of
    /// another refresh) is taken by the encoder in place: no rebuild, the ceiling and the rung
    /// at the new rate, and the stream keeps sending.
    #[test]
    fn a_new_frame_rate_alone_is_taken_in_place() {
        let runtime =
            tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build();
        runtime.unwrap().block_on(async {
            // A known beat: a panel that reports no refresh would have the capture throttled
            // to the rate, and a new rate would be a new capture.
            slopty_capture::synthetic::set_beat(Some(120));
            let ScreenEvent::Listing { displays, .. } = Pipeline::<Drawn>::listing().await.unwrap()
            else {
                panic!("no listing")
            };
            let display = displays.first().expect("a display");
            let router = ScreenRouter::new();
            let wire = Arc::new(Wire::new(router, None, None));
            let quality = Quality { fps: 60, scale: 0.25, ..Quality::default() };
            let (mut stream, _opened) = Pipeline::<Drawn>::open(
                STREAM,
                CaptureTarget::Display(display.id),
                quality,
                Arc::<Wire>::clone(&wire),
                |_event| {},
            )
            .await
            .unwrap();
            slopty_capture::synthetic::set_beat(None);
            let ceiling =
                |stream: &Pipeline<Drawn>| stream.shared.fps_ceiling.load(Ordering::Relaxed);
            assert_eq!(ceiling(&stream), 60);
            let rebuild = stream.set_quality(&Quality { fps: 120, ..quality }, None);
            assert!(rebuild.is_none(), "a new rate alone builds nothing");
            assert_eq!(ceiling(&stream), 120);
            assert_eq!(stream.shared.fps.load(Ordering::Relaxed), 120);
            let sent = wire.bytes();
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(wire.bytes() > sent, "the stream keeps flowing");
            stream.close().await;
        });
    }

    /// Capture → glass and input → glass on this Mac, from drawn pictures. Run from a copy of the
    /// test binary off the repository volume (`docs/MEASUREMENTS.md`, "Capture to the glass");
    /// `SLOPTY_GLASS_SECONDS` sets the run length (default 20).
    #[test]
    #[ignore = "measurement"]
    fn capture_and_input_to_glass() {
        slopty_platform::user_interactive_thread();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .on_thread_start(slopty_platform::user_interactive_thread)
            .build()
            .unwrap();
        let seconds: u64 =
            std::env::var("SLOPTY_GLASS_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(20);
        runtime.block_on(async {
            run::<Drawn>("loopback", 60, Duration::ZERO, 0, seconds).await;
            run::<Drawn>("tailnet-shaped", 60, Duration::from_millis(5), 30, seconds).await;
        });
    }

    /// The same on a 120 Hz beat, as a 120 Hz Mac or external display gives:
    /// a stream asked for 60 against one asked for 120, on loopback and tailnet-shaped. The
    /// canvas scrolls at the same speed on either beat (`docs/MEASUREMENTS.md`, "120 fps
    /// against 60"). `SLOPTY_GLASS_SECONDS` sets the run length (default 20).
    #[test]
    #[ignore = "measurement"]
    fn capture_and_input_to_glass_at_120() {
        slopty_platform::user_interactive_thread();
        slopty_capture::synthetic::set_beat(Some(120));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .on_thread_start(slopty_platform::user_interactive_thread)
            .build()
            .unwrap();
        let seconds: u64 =
            std::env::var("SLOPTY_GLASS_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(20);
        runtime.block_on(async {
            run::<Drawn>("loopback", 60, Duration::ZERO, 0, seconds).await;
            run::<Drawn>("loopback", 120, Duration::ZERO, 0, seconds).await;
            run::<Drawn>("tailnet-shaped", 60, Duration::from_millis(5), 30, seconds).await;
            run::<Drawn>("tailnet-shaped", 120, Duration::from_millis(5), 30, seconds).await;
        });
        slopty_capture::synthetic::set_beat(None);
    }

    /// Capture → glass on [`Synthetic`]'s 3024 × 1964 display at native scale on a 120 Hz beat,
    /// asked for 120, on loopback and tailnet-shaped. The padded encoder turns a frame out in
    /// about 15 ms, so it cannot keep 120 however the link does (`docs/MEASUREMENTS.md`, "the
    /// encoder watch behind the mailbox"). `SLOPTY_GLASS_SECONDS` sets each run's length.
    #[test]
    #[ignore = "measurement"]
    fn capture_to_glass_at_120_past_the_encoder() {
        /// [`Synthetic`] on a 120 Hz panel.
        enum Synthetic120 {}

        impl Platform for Synthetic120 {
            type Audio = slopty_codec::Opus;
            type Capture = StudioAt<120>;
            type Input = Poke;
            type Video = slopty_codec::VideoToolbox;
        }

        slopty_platform::user_interactive_thread();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .on_thread_start(slopty_platform::user_interactive_thread)
            .build()
            .unwrap();
        let seconds: u64 =
            std::env::var("SLOPTY_GLASS_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(20);
        runtime.block_on(async {
            run::<Synthetic120>("loopback", 120, Duration::ZERO, 0, seconds).await;
            let shaped = Duration::from_millis(5);
            run::<Synthetic120>("tailnet-shaped", 120, shaped, 30, seconds).await;
        });
    }

    /// Capture → glass on [`Synthetic`]'s 3024 × 1964 display at native scale, on loopback,
    /// with the stream's sides padded to 16 and, alternately, at the picture's even size as
    /// before (`docs/MEASUREMENTS.md`, "Stream sides padded to 16"). `SLOPTY_GLASS_SECONDS`
    /// sets each run's length (default 20), `SLOPTY_GLASS_ROUNDS` the pairs (default 2).
    #[test]
    #[ignore = "measurement"]
    fn capture_to_glass_padded_against_even() {
        slopty_platform::user_interactive_thread();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .on_thread_start(slopty_platform::user_interactive_thread)
            .build()
            .unwrap();
        let knob = |name: &str, default: u64| {
            std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default)
        };
        let (seconds, rounds) = (knob("SLOPTY_GLASS_SECONDS", 20), knob("SLOPTY_GLASS_ROUNDS", 2));
        runtime.block_on(async {
            for _ in 0..rounds {
                for (label, pad_to) in [("even (before)", Some(2)), ("padded to 16", None)] {
                    let knobs = Knobs { pad_to, ..Knobs::default() };
                    run_padded::<Synthetic>(label, 60, Duration::ZERO, 0, seconds, knobs).await;
                }
            }
        });
    }

    /// Capture → glass on a drawn Retina (3024 × 1964), 4K and 5K display at 60 Hz, native
    /// scale, loopback: the picture coded whole on one engine against two stripes on both,
    /// alternated (`docs/MEASUREMENTS.md`, "Two stripes, capture to glass").
    /// `SLOPTY_GLASS_SECONDS` sets each run's length (default 20), `SLOPTY_GLASS_ROUNDS` the
    /// rounds (default 2).
    #[test]
    #[ignore = "measurement"]
    fn capture_to_glass_striped_against_whole() {
        /// [`Synthetic`] on a 3840 × 2160 display.
        enum Uhd {}

        impl Platform for Uhd {
            type Audio = slopty_codec::Opus;
            type Capture = StudioAt<60, 1920, 1080>;
            type Input = Poke;
            type Video = slopty_codec::VideoToolbox;
        }

        /// [`Synthetic`] on a 5120 × 2880 display.
        enum FiveK {}

        impl Platform for FiveK {
            type Audio = slopty_codec::Opus;
            type Capture = StudioAt<60, 2560, 1440>;
            type Input = Poke;
            type Video = slopty_codec::VideoToolbox;
        }

        slopty_platform::user_interactive_thread();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .on_thread_start(slopty_platform::user_interactive_thread)
            .build()
            .unwrap();
        let knob = |name: &str, default: u64| {
            std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default)
        };
        let (seconds, rounds) = (knob("SLOPTY_GLASS_SECONDS", 20), knob("SLOPTY_GLASS_ROUNDS", 2));
        runtime.block_on(async {
            for _ in 0..rounds {
                for (label, stripes) in [("one picture", Knob::Off), ("two stripes", Knob::On)] {
                    let knobs = Knobs { stripes, ..Knobs::default() };
                    let retina = format!("Retina, {label}");
                    run_padded::<Synthetic>(&retina, 60, Duration::ZERO, 0, seconds, knobs).await;
                    let label4 = format!("4K, {label}");
                    run_padded::<Uhd>(&label4, 60, Duration::ZERO, 0, seconds, knobs).await;
                    let label5 = format!("5K, {label}");
                    run_padded::<FiveK>(&label5, 60, Duration::ZERO, 0, seconds, knobs).await;
                }
            }
        });
    }

    /// Input → glass on a still screen, the worker refining it between inputs against the same
    /// with refinement off, alternated (`docs/MEASUREMENTS.md`, "a still picture refined"). The
    /// canvas stands still and changes only on a click, every 80–150 ms, so each click lands
    /// in the quiet where refinement runs. On loopback, and on a link of `SLOPTY_GLASS_LINK`
    /// Mbit/s (default 20) with 5 ms each way, where a refinement's bytes are still leaving when
    /// a change comes. `SLOPTY_GLASS_SECONDS` sets each run's length (default 20),
    /// `SLOPTY_GLASS_ROUNDS` the pairs (default 2).
    #[test]
    #[ignore = "measurement"]
    fn input_to_glass_on_a_still_screen_while_refining() {
        slopty_platform::user_interactive_thread();
        slopty_capture::synthetic::set_still(true);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .on_thread_start(slopty_platform::user_interactive_thread)
            .build()
            .unwrap();
        let knob = |name: &str, default: u64| {
            std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default)
        };
        let (seconds, rounds) = (knob("SLOPTY_GLASS_SECONDS", 20), knob("SLOPTY_GLASS_ROUNDS", 2));
        let link = knob("SLOPTY_GLASS_LINK", 20) * 1_000_000;
        runtime.block_on(async {
            for _ in 0..rounds {
                for refine in [false, true] {
                    let knobs = Knobs { refine, ..Knobs::default() };
                    run_padded::<Drawn>("still, loopback", 60, Duration::ZERO, 0, seconds, knobs)
                        .await;
                    let knobs = Knobs { refine, link_bps: Some(link), ..Knobs::default() };
                    let one_way = Duration::from_millis(5);
                    run_padded::<Drawn>("still, shaped", 60, one_way, 0, seconds, knobs).await;
                }
            }
        });
        slopty_capture::synthetic::set_still(false);
    }

    /// A transport that takes every datagram and counts it: the streams of
    /// [`measure_concurrent_streams`] are timed on the worker alone.
    #[derive(Default)]
    struct Counting {
        bytes: std::sync::atomic::AtomicU64,
    }

    impl DatagramSink for Counting {
        fn send(&self, datagrams: &[Bytes]) -> Result<(), Refused> {
            let sent: usize = datagrams.iter().map(Bytes::len).sum();
            self.bytes.fetch_add(sent as u64, Ordering::Relaxed);
            Ok(())
        }

        fn max_size(&self) -> Option<usize> {
            Some(MAX_DATAGRAM)
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

    /// This process's CPU time, user and system, all threads.
    fn cpu_time() -> Duration {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: POSIX rule: `getrusage` fills the `rusage` it is given for `RUSAGE_SELF`.
        let status = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
        assert_eq!(status, 0, "getrusage");
        // SAFETY: filled by the successful call above.
        let usage = unsafe { usage.assume_init() };
        let time = |t: libc::timeval| {
            Duration::from_secs(t.tv_sec.cast_unsigned())
                + Duration::from_micros(u64::from(t.tv_usec.cast_unsigned()))
        };
        time(usage.ru_utime) + time(usage.ru_stime)
    }

    /// Several streams served at once, each its own capture, encoder session, packetizer and
    /// timers (idea #14; `docs/MEASUREMENTS.md`, "several streams on the encode engines"). For
    /// 1, 2, 4 and 8 streams of the drawn display at about 1080p and 60 frames a second, opened
    /// one after another as a client opens tiles, into a transport that only counts: each
    /// stream's draw (the stand-in for ScreenCaptureKit), submit → packet, capture → packetized
    /// and frames encoded a second, and the worker's CPU a second, every thread. The first
    /// stream's encode against its time alone is the number the engines' sharing moves.
    /// `SLOPTY_STREAMS` picks the counts, `SLOPTY_GLASS_SECONDS` the run (default 4), after 3 s
    /// for the encoder watches to settle; `SLOPTY_FOCUSED=1` has a client focus the first
    /// stream (`engines`).
    #[test]
    #[ignore = "measurement"]
    fn measure_concurrent_streams() {
        slopty_platform::user_interactive_thread();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .on_thread_start(slopty_platform::user_interactive_thread)
            .build()
            .unwrap();
        let seconds: u64 =
            std::env::var("SLOPTY_GLASS_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(4);
        let counts: Vec<u32> = std::env::var("SLOPTY_STREAMS").map_or_else(
            |_unset| vec![1, 2, 4, 8],
            |s| s.split(',').filter_map(|n| n.trim().parse().ok()).collect(),
        );
        let focused = std::env::var("SLOPTY_FOCUSED").is_ok_and(|v| v == "1");
        runtime.block_on(async {
            let ScreenEvent::Listing { displays, .. } = Pipeline::<Drawn>::listing().await.unwrap()
            else {
                panic!("no listing")
            };
            let display = displays.first().expect("a display");
            let scale = (1920.0 / (display.w * display.scale)).min(1.0);
            for &n in &counts {
                let sink = Arc::new(Counting::default());
                let mut streams = Vec::new();
                for k in 0..n {
                    let (stream, opened) = Pipeline::<Drawn>::open(
                        StreamId(k + 1),
                        CaptureTarget::Display(display.id),
                        Quality { fps: 60, scale, ..Quality::default() },
                        Arc::<Counting>::clone(&sink),
                        |_event| {},
                    )
                    .await
                    .unwrap();
                    if k == 0 {
                        let ScreenEvent::Opened { width, height, .. } = opened else {
                            panic!("{opened:?}")
                        };
                        eprintln!(
                            "MEASURE concurrent streams at {width}×{height}, stream 0 {}, load {}",
                            if focused { "focused" } else { "like the rest" },
                            load_average()
                        );
                        stream.set_focused(focused);
                    }
                    streams.push(stream);
                }
                // The encoder watches settle the rungs over a few half-second windows.
                tokio::time::sleep(Duration::from_secs(3)).await;
                let before: Vec<u64> = streams.iter().map(|s| s.stats().encoded).collect();
                let (cpu0, bytes0, at) = (cpu_time(), sink.bytes.load(Ordering::Relaxed), Instant::now());
                tokio::time::sleep(Duration::from_secs(seconds)).await;
                let elapsed = at.elapsed().as_secs_f64();
                let cpu = cpu_time().saturating_sub(cpu0).as_secs_f64() / elapsed;
                let wire = (sink.bytes.load(Ordering::Relaxed) - bytes0) as f64 * 8.0 / elapsed / 1e6;
                let q = |q: &slopty_proto::ctl::Quantiles| {
                    format!("{:.2}/{:.2}/{:.2}", q.p50_us as f64 / 1e3, q.p95_us as f64 / 1e3, q.max_us as f64 / 1e3)
                };
                for (k, (stream, before)) in streams.iter().zip(&before).enumerate() {
                    let s = stream.stats();
                    eprintln!(
                        "  n={n} stream {k}: draw {} ms, encode {} ms, capture → packetized mean {:.2} max {:.2} ms, {:.1} fps encoded, {} superseded",
                        q(&s.capture),
                        q(&s.encode),
                        s.latency_sum_us as f64 / s.encoded.max(1) as f64 / 1e3,
                        s.latency_max_us as f64 / 1e3,
                        (s.encoded - before) as f64 / elapsed,
                        stream.shared.counters.superseded.load(Ordering::Relaxed),
                    );
                }
                eprintln!("  n={n}: worker CPU {cpu:.2} cores, {wire:.1} Mbit/s out");
                for stream in streams {
                    stream.close().await;
                }
            }
        });
    }

    /// A new stream's first two seconds: frames encoded in each, the lowest ceiling its encoder
    /// watch set and when the ceiling was back at the rung, for the drawn screen at a quarter
    /// (the size the stream tests use) or at `SLOPTY_SCALE`, its first frame `SLOPTY_COLD_MS`
    /// slower ([`ColdStart`]), or with `SLOPTY_WHOLE=1` this Mac's display drawn at its own
    /// size. Run each in a process of its own, so the first keyframe meets a cold encoder as
    /// a worker's first stream does (`docs/MEASUREMENTS.md`, "a keyframe charged to the rung").
    #[test]
    #[ignore = "measurement"]
    fn measure_a_new_streams_first_seconds() {
        let runtime =
            tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build();
        runtime.unwrap().block_on(async {
            if std::env::var("SLOPTY_WHOLE").is_ok_and(|v| v == "1") {
                let ScreenEvent::Listing { displays, .. } =
                    Pipeline::<Drawn>::listing().await.unwrap()
                else {
                    panic!("no listing")
                };
                let display = displays.first().expect("a display").id;
                first_seconds::<Drawn>(CaptureTarget::Display(display), Quality::default()).await;
            } else {
                let scale =
                    std::env::var("SLOPTY_SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(0.25);
                let quality = Quality { scale, ..Quality::default() };
                first_seconds::<ColdStart>(CaptureTarget::Display(DISPLAY.id), quality).await;
            }
        });
    }

    /// The drawn screen whose sessions each spend `SLOPTY_COLD_MS` (default none) more on their
    /// first frame, as a cold encoder does: a worker's first session, a fresh process, a busy
    /// machine.
    enum ColdStart {}

    impl Platform for ColdStart {
        type Audio = slopty_codec::Opus;
        type Capture = Studio;
        type Input = Poke;
        type Video = Cold;
    }

    /// VideoToolbox, slower by `SLOPTY_COLD_MS` on a session's first frame.
    struct Cold {
        session: slopty_codec::VideoToolbox,
        warm: std::sync::atomic::AtomicBool,
    }

    impl slopty_codec::VideoEncoder for Cold {
        type Image = PixelBuffer;

        fn new(
            config: slopty_codec::EncoderConfig,
            sink: impl Fn(slopty_codec::EncodedPacket) + Send + Sync + 'static,
        ) -> Result<Self, slopty_codec::CodecError> {
            let session = slopty_codec::VideoToolbox::new(config, sink)?;
            Ok(Self { session, warm: false.into() })
        }

        fn encode(
            &self,
            image: &PixelBuffer,
            pts_us: u64,
            options: &slopty_codec::FrameOptions,
        ) -> Result<(), slopty_codec::CodecError> {
            if !self.warm.swap(true, Ordering::Relaxed) {
                let cold: u64 =
                    std::env::var("SLOPTY_COLD_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
                #[expect(
                    clippy::disallowed_methods,
                    reason = "stands in for a cold session's first encode, which blocks the submit"
                )]
                std::thread::sleep(Duration::from_millis(cold));
            }
            self.session.encode(image, pts_us, options)
        }

        fn set_bitrate(&self, bps: u32) -> Result<(), slopty_codec::CodecError> {
            self.session.set_bitrate(bps)
        }

        fn set_frame_rate(&self, fps: u16) -> Result<(), slopty_codec::CodecError> {
            self.session.set_frame_rate(fps)
        }

        fn set_temporal_layers(&self, on: bool) -> Result<bool, slopty_codec::CodecError> {
            self.session.set_temporal_layers(on)
        }

        fn frames_dropped(&self) -> u64 {
            self.session.frames_dropped()
        }
    }

    /// [`measure_a_new_streams_first_seconds`] on `target` at `quality`, decoded by a client in
    /// this process whose loss feedback the stream answers, as the stream tests run it.
    async fn first_seconds<P: Platform>(target: CaptureTarget, quality: Quality) {
        let router = ScreenRouter::new();
        let wire = Arc::new(Wire::new(router.clone(), None, None));
        let (stream, opened) =
            Pipeline::<P>::open(STREAM, target, quality, wire, |_event| {}).await.unwrap();
        let ScreenEvent::Opened { codec, width, height, .. } = opened else { panic!("{opened:?}") };
        let control = stream.control();
        let (reports_tx, mut reports) = mpsc::channel::<ClientMsg>(64);
        let drain = tokio::spawn(async move { while reports.recv().await.is_some() {} });
        let uplink = Uplink {
            control: reports_tx,
            feedback: Box::new(move |bytes| {
                answer(&control, &bytes);
                true
            }),
            rtt: Box::new(|| Some(Duration::from_millis(1))),
        };
        let handle =
            spawn_screen(&tokio::runtime::Handle::current(), &router, STREAM, codec, uplink);
        let (opened_at, mut lowest, mut back_at) = (Instant::now(), u16::MAX, None);
        let mut encoded = [0_u64; 2];
        while opened_at.elapsed() < Duration::from_secs(2) {
            tokio::time::sleep(Duration::from_millis(5)).await;
            let ceiling = stream.shared.fps_ceiling.load(Ordering::Relaxed);
            if ceiling < lowest {
                (lowest, back_at) = (ceiling, None);
            } else if ceiling >= 60 && lowest < 60 && back_at.is_none() {
                back_at = Some(opened_at.elapsed());
            }
            let second = usize::from(opened_at.elapsed() >= Duration::from_secs(1));
            encoded[second] = stream.stats().encoded;
        }
        let worker = stream.stats();
        eprintln!(
            "MEASURE a new stream at {width}×{height}: {} frames encoded in its first second, {} in its second; lowest ceiling {lowest}, back at 60 after {back_at:?}; slowest encode {:.0} ms, slowest capture → packetized {:.0} ms, client decoded {}",
            encoded[0],
            encoded[1].saturating_sub(encoded[0]),
            worker.encode.max_us as f64 / 1e3,
            worker.latency_max_us as f64 / 1e3,
            handle.stats().frames,
        );
        drop(handle);
        drain.abort();
        stream.close().await;
    }

    /// The audio datagrams on `line`, as (when, media stream, sequence), from batches sent since
    /// the last look.
    fn sound_on(
        line: &mut mpsc::UnboundedReceiver<(Instant, Vec<Bytes>)>,
    ) -> Vec<(Instant, u32, u32)> {
        let mut heard = Vec::new();
        while let Ok((at, batch)) = line.try_recv() {
            for datagram in batch {
                let Some((header, _)) = slopty_proto::media::MediaHeader::parse(&datagram) else {
                    continue;
                };
                if header.kind() == Some(slopty_proto::media::Kind::Audio) {
                    heard.push((at, header.stream.get(), header.frame.get()));
                }
            }
        }
        heard
    }

    /// The desktop and two windows of one app, each its own stream hearing through `listen` as
    /// a worker's connection has them, are one sound on the wire: one capture hearing every
    /// app, one sequence on the sound's media stream, none on the streams' own. The windows
    /// closing changes nothing it hears and leaves no hole in its timeline.
    #[test]
    fn a_display_and_two_windows_of_one_app_are_one_sound_on_the_wire() {
        use slopty_capture::Heard;

        use crate::screen::sound::Sound;

        let runtime =
            tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build();
        runtime.unwrap().block_on(async {
            let (line_tx, mut line) = mpsc::unbounded_channel();
            let wire = Arc::new(Wire::new(ScreenRouter::new(), Some(line_tx), None));
            let sink: Arc<dyn DatagramSink> = Arc::<Wire>::clone(&wire);
            let sound = Sound::<Synthetic>::new(Arc::clone(&sink));
            let targets = [
                CaptureTarget::Display(DISPLAY.id),
                CaptureTarget::Window(WINDOWS[0].id),
                CaptureTarget::Window(WINDOWS[1].id),
            ];
            let mut streams = Vec::new();
            for (id, target) in (1..).map(StreamId).zip(targets) {
                let opened =
                    Pipeline::<Synthetic>::open(id, target, Quality::default(), Arc::clone(&sink), |_e| {});
                let (mut stream, _opened) = opened.await.unwrap();
                stream.listen(&sound);
                streams.push(stream);
            }
            // The first chunk makes the Opus encoder, which a loaded machine takes seconds over.
            let mut heard = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(100);
            while heard.is_empty() {
                assert!(Instant::now() < deadline, "no sound: {:?}", sound.stats());
                tokio::time::sleep(Duration::from_millis(5)).await;
                heard.extend(sound_on(&mut line));
            }
            tokio::time::sleep(Duration::from_millis(600)).await;
            heard.extend(sound_on(&mut line));
            let closed_at = Instant::now();
            let display = streams.remove(0);
            for window in streams {
                window.close().await;
            }
            tokio::time::sleep(Duration::from_millis(600)).await;
            heard.extend(sound_on(&mut line));
            let stats = sound.stats();
            assert_eq!(
                (stats.starts, stats.hears, stats.stops, stats.heard.clone()),
                (1, 0, 0, Some(Heard::Every)),
                "one capture of every app, changed by nothing"
            );
            assert!(heard.iter().all(|&(_, media, _)| media == slopty_proto::media::SOUND.0));
            let seqs: Vec<u32> = heard.iter().map(|&(_, _, seq)| seq).collect();
            assert_eq!(seqs, (1..).take(seqs.len()).collect::<Vec<u32>>(), "one sequence");
            // No gap, judged on the sound's own timeline: one capture whose chunks follow each
            // other with no hole, and one unbroken sequence. When the packets reached the test is
            // the scheduler's to say (75 ms apart on a loaded 3-core runner), so it is printed,
            // not judged.
            assert_eq!(stats.holes, 0, "the capture's audio skipped ahead: {stats:?}");
            let span = heard.last().unwrap().0.saturating_duration_since(heard[0].0);
            let per_second = (seqs.len() - 1) as f64 / span.as_secs_f64();
            let wait = heard
                .windows(2)
                .filter(|w| w[1].0 >= closed_at)
                .map(|w| w[1].0.saturating_duration_since(w[0].0))
                .max()
                .unwrap_or_default();
            eprintln!(
                "MEASURE one sound for a display and two windows: {} packets at {per_second:.1} a second, the longest wait after the windows closed {:.1} ms, {} holes",
                seqs.len(),
                wait.as_secs_f64() * 1e3,
                stats.holes
            );
            display.close().await;
        });
    }

    /// What a worker's one sound costs it for 1, 2, 4 and 8 streams hearing through it: the
    /// canvas's tone, the one Opus encoder and the datagrams, as the process's CPU over
    /// `SLOPTY_SOUND_SECONDS` (default 4) after a second to settle. Streams of one client used to
    /// cost a capture tap and an Opus encoder each (`docs/MEASUREMENTS.md`, "streams from one
    /// worker: what they could share", about 0.016 of a core a stream with its client's
    /// decoder); now they cost one between them.
    #[test]
    #[ignore = "a measurement: cargo test -p slopty-worker --release --lib -- --ignored --nocapture one_sound_costs"]
    fn one_sound_costs_the_same_for_any_number_of_streams() {
        use slopty_capture::Heard;

        use crate::screen::sound::{Listen as _, Sound};

        let seconds: u64 =
            std::env::var("SLOPTY_SOUND_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(4);
        let runtime =
            tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build();
        runtime.unwrap().block_on(async {
            for n in [1_usize, 2, 4, 8] {
                let (line_tx, mut line) = mpsc::unbounded_channel();
                let wire: Arc<dyn DatagramSink> =
                    Arc::new(Wire::new(ScreenRouter::new(), Some(line_tx), None));
                let sound = Sound::<Synthetic>::new(wire);
                // A display among them hears every app; the others are windows of two apps.
                let listening: Vec<_> = (0..n)
                    .map(|i| match i % 3 {
                        0 => Heard::Every,
                        pid => Heard::Apps(std::iter::once(i32::try_from(pid).unwrap_or(1)).collect()),
                    })
                    .map(|heard| sound.listen(heard))
                    .collect();
                tokio::time::sleep(Duration::from_secs(1)).await;
                let _settling = sound_on(&mut line);
                let (cpu, at) = (cpu_time(), Instant::now());
                tokio::time::sleep(Duration::from_secs(seconds)).await;
                let (cpu, wall) = (cpu_time().saturating_sub(cpu), at.elapsed());
                let packets = sound_on(&mut line).len();
                eprintln!(
                    "MEASURE one sound, {n} streams: {:.4} of a core, {:.1} packets a second, {} capture",
                    cpu.as_secs_f64() / wall.as_secs_f64(),
                    packets as f64 / wall.as_secs_f64(),
                    sound.stats().starts,
                );
                drop(listening);
            }
        });
    }
}
