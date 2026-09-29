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
use slopty_capture::synthetic::{Canvas, CanvasStream, CanvasTarget};
use slopty_capture::{
    AudioSink, AxError, CaptureConfig, CaptureError, CaptureSource, CapturedFrame, Crop, Rect,
    TargetWindow, Went, WindowState,
};
use slopty_codec::PixelBuffer;
use slopty_core::{DisplayId, WindowId};
use slopty_input::{InputError, InputSink, PointerWatch};
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

/// [`DISPLAY`] as [`StudioAt`] lists and draws it, refreshing at `HZ`.
fn shown_display<const HZ: u16>() -> DisplayInfo {
    DisplayInfo { hz: f32::from(HZ), ..DISPLAY }
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

fn display_rect() -> Rect {
    Rect { x: 0.0, y: 0.0, w: f64::from(DISPLAY.w), h: f64::from(DISPLAY.h) }
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

/// [`Studio`] on a display refreshing at `HZ`: a measurement's 120 Hz panel, whatever the
/// panels of the Mac it runs on.
#[derive(Clone, Copy, Debug)]
pub enum StudioAt<const HZ: u16> {}

impl<const HZ: u16> CaptureSource for StudioAt<HZ> {
    type Content = Scene;
    type HideWatch = ();
    type Image = PixelBuffer;
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
        vec![shown_display::<HZ>()]
    }

    fn resolve(content: &Scene, kind: CaptureTarget) -> Result<CanvasTarget, CaptureError> {
        let (w, h) = match kind {
            CaptureTarget::Display(id) if id == DISPLAY.id => (DISPLAY.w, DISPLAY.h),
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
        audio: Option<AudioSink>,
        on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) -> Result<CanvasStream, CaptureError> {
        // The beat is the listed display's, not that of whichever panel shares its id here.
        let config =
            CaptureConfig { fps: if config.fps == 0 { HZ } else { config.fps }, ..*config };
        Canvas::start(target, &config, sink, audio, on_stop, done)
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

    fn target_bounds(target: CaptureTarget) -> Option<Rect> {
        match target {
            CaptureTarget::Display(id) => (id == DISPLAY.id).then(display_rect),
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
        display_rect()
    }

    fn resize_window(
        _pid: i32,
        target: &TargetWindow,
        width: f64,
        height: f64,
    ) -> Result<(), AxError> {
        #[expect(clippy::cast_possible_truncation, reason = "points, clamped to the display")]
        let side = |asked: f64, min: f64, max: f32| asked.clamp(min, f64::from(max)).round() as f32;
        let (w, h) = (side(width, MIN_WINDOW.0, DISPLAY.w), side(height, MIN_WINDOW.1, DISPLAY.h));
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

    fn cursor_shape(_scale: u8) -> Option<CursorShape> {
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
    use slopty_core::StreamId;
    use slopty_proto::ClientMsg;
    use slopty_proto::datagram::ClientDatagram;
    use slopty_proto::input::{Mods, MouseButton};
    use slopty_proto::media::MAX_DATAGRAM;
    use slopty_proto::screen::{Feedback, Quality, ScreenEvent, ScreenRequest};
    use tokio::sync::mpsc;

    use super::*;
    use crate::screen::{DatagramSink, Pipeline, Refused, StreamControl};

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
    /// loopback, or down a delay line.
    struct Wire {
        router: ScreenRouter,
        line: Option<mpsc::UnboundedSender<(Instant, Vec<Bytes>)>>,
        /// Bytes of every datagram sent: data, parity, retransmits, cursor and heartbeats.
        bytes: std::sync::atomic::AtomicU64,
    }

    impl Wire {
        fn new(
            router: ScreenRouter,
            line: Option<mpsc::UnboundedSender<(Instant, Vec<Bytes>)>>,
        ) -> Self {
            Self { router, line, bytes: std::sync::atomic::AtomicU64::new(0) }
        }

        fn bytes(&self) -> u64 {
            self.bytes.load(Ordering::Relaxed)
        }
    }

    impl DatagramSink for Wire {
        fn send(&self, datagrams: &[Bytes]) -> Result<(), Refused> {
            let now = Instant::now();
            let sent: usize = datagrams.iter().map(Bytes::len).sum();
            self.bytes.fetch_add(sent as u64, Ordering::Relaxed);
            match &self.line {
                None => self.router.route_many(datagrams.iter().cloned(), now),
                Some(line) => {
                    line.send((now, datagrams.to_vec())).map_err(|_gone| Refused::Closed)?;
                }
            }
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

    /// The client's loss feedback, as the worker's connection answers it.
    fn answer(control: &StreamControl, bytes: &[u8]) {
        match ClientDatagram::decode(bytes) {
            Some(ClientDatagram::Feedback(Feedback::Nack { frame, fragments, .. })) => {
                control.nack(frame, &fragments);
            }
            Some(ClientDatagram::Feedback(Feedback::Refresh {
                last_good_frame, keyframe, ..
            })) => {
                control.request_refresh(last_good_frame, keyframe);
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
        run_padded::<P>(label, fps, one_way, loss_permille, seconds, None).await;
    }

    /// [`run`] with the stream's sides padded to `pad_to` in place of its codec's own multiple,
    /// when that is given ([`Pipeline::open_padded`]).
    #[expect(clippy::too_many_lines, reason = "one measurement, read top to bottom")]
    async fn run_padded<P: Platform>(
        label: &str,
        fps: u16,
        one_way: Duration,
        loss_permille: u32,
        seconds: u64,
        pad_to: Option<u32>,
    ) {
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
        let line = (!one_way.is_zero()).then(|| {
            let router = router.clone();
            delay_line(one_way, move |datagrams: Vec<Bytes>, at| router.route_many(datagrams, at))
        });
        let wire = Arc::new(Wire::new(router.clone(), line));
        let (mut stream, opened) = Pipeline::<P>::open_padded(
            STREAM,
            CaptureTarget::Display(display.id),
            Quality { fps, ..Quality::default() },
            Arc::<Wire>::clone(&wire),
            |_event| {},
            pad_to,
        )
        .await
        .unwrap();
        let ScreenEvent::Opened { codec, width, height, .. } = opened else { panic!("{opened:?}") };

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
                if let ClientMsg::Screen(ScreenRequest::Report { report, .. }) = msg
                    && let Some(decision) = control.report(&report, None)
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
            "MEASURE glass {label}: {width}×{height} {codec:?} at {hz:.0} Hz, {fps} fps asked, one way {:.1} ms, loss {:.1} %",
            ms(one_way),
            f64::from(loss_permille) / 10.0
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
            "  cadence: rung {} under a ceiling of {}, encoder fed {} a second at the last window, {} captures replaced in the mailbox",
            stream.shared.fps.load(Ordering::Relaxed),
            stream.shared.fps_ceiling.load(Ordering::Relaxed),
            stream.shared.fed_fps.load(Ordering::Relaxed),
            stream.shared.counters.superseded.load(Ordering::Relaxed),
        );
        assert!(glass.capture.count > 0, "no frame was timed from its capture");
        assert!(glass.input.count > 0, "no input reached the glass");
        drop(handle);
        reporting.abort();
        stream.close().await;
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
            let wire = Arc::new(Wire::new(router.clone(), None));
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
            let wire = Arc::new(Wire::new(router.clone(), None));
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
            let wire = Arc::new(Wire::new(router.clone(), None));
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
                        if let ClientMsg::Screen(ScreenRequest::Report { report, .. }) = msg {
                            let _decision = control.report(&report, None);
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
                (stats.decode_errors, stats.refreshes, stats.frames_lost),
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
            let submitted = decoder.decode(&packet.data, packet.pts_us).is_ok();
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
            let wire = Arc::new(Wire::new(router, None));
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
                    run_padded::<Synthetic>(label, 60, Duration::ZERO, 0, seconds, pad_to).await;
                }
            }
        });
    }
}
