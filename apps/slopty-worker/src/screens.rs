//! One screen stream on a client's connection: the QUIC datagrams it sends into, and the task
//! that owns it.
//!
//! The task is the only thing that touches the [`ScreenStream`], so everything the stream does
//! slowly — the 115–300 ms open, an encoder rebuild, a close waiting on ScreenCaptureKit, the
//! geometry probe's window-server reads — waits only for that stream. The connection's own loop
//! hands commands over and goes straight back to the terminals.

use std::sync::Arc;

use bytes::Bytes;
use slopty_core::{ClientId, StreamId};
use slopty_net::{Connection, WorkerMsg};
use slopty_proto::screen::{CaptureTarget, Quality, ScreenEvent, ScreenInput};
use slopty_worker::platform::Platform;
use slopty_worker::screen::{Pipeline, Rebuild, Refused, ScreenStream, StreamControl, StreamEvent};
use tokio::sync::mpsc;

use crate::Daemon;

/// How often a stream's target is checked for a size change.
/// Also how fast a window on the display-crop path follows a drag: the crop lags a move by
/// one period plus ScreenCaptureKit's ~20 ms configuration update (MEASUREMENTS.md, "capture
/// floor"), during which one edge of the picture shows the desktop the window left.
const GEOMETRY_PERIOD: std::time::Duration = std::time::Duration::from_millis(100);

/// A connection's datagrams, written from whichever thread produced them.
pub struct QuicSink(pub Connection);

impl slopty_worker::DatagramSink for QuicSink {
    fn send(&self, datagrams: &[Bytes]) -> Result<(), Refused> {
        let sent = match datagrams {
            [one] => self.0.send_datagram(one.clone()),
            many => self.0.send_many_datagrams(many).map(|_queued| ()),
        };
        sent.map_err(|e| match e {
            noq::SendDatagramError::TooLarge => Refused::TooLarge,
            noq::SendDatagramError::UnsupportedByPeer
            | noq::SendDatagramError::Disabled
            | noq::SendDatagramError::ConnectionLost(_) => Refused::Closed,
        })
    }

    fn max_size(&self) -> Option<usize> {
        self.0.max_datagram_size()
    }

    fn held(&self) -> usize {
        slopty_net::endpoint::DATAGRAM_BUFFER.saturating_sub(self.0.datagram_send_buffer_space())
    }

    fn cwnd(&self) -> u64 {
        slopty_net::endpoint::path_rtt_cwnd(&self.0).map_or(0, |(_rtt, cwnd)| cwnd)
    }

    fn is_closed(&self) -> bool {
        self.0.close_reason().is_some()
    }
}

/// What the connection asks of one stream, in the order it asked.
#[derive(Debug)]
pub enum Command {
    Input(ScreenInput),
    Focus,
    SetQuality(Quality),
    Resize { width: u32, height: u32 },
    Close,
}

/// What a stream's task tells the connection back.
#[derive(Debug)]
pub enum Told {
    /// Open: feedback and reports go straight to it from now on.
    Opened(StreamId, StreamControl),
    /// Ended, however it ended; the connection forgets it.
    Gone(StreamId),
}

/// Everything a stream's task needs from its connection.
pub struct Link {
    pub daemon: Daemon,
    pub client: ClientId,
    pub conn: Connection,
    pub out: mpsc::Sender<WorkerMsg>,
    pub told: mpsc::UnboundedSender<Told>,
}

/// Open the stream, then serve its commands and its geometry until it is closed or the
/// connection lets go of it.
pub async fn run(
    link: Link,
    id: StreamId,
    target: CaptureTarget,
    quality: Quality,
    mut commands: mpsc::UnboundedReceiver<Command>,
) {
    let Link { daemon, client, conn, out, told } = link;
    let events = out.clone();
    let on_event = move |e: StreamEvent| {
        let event = match e {
            StreamEvent::Stopped(e) => ScreenEvent::Closed { stream: id, reason: e.to_string() },
            StreamEvent::Cursor(shape) => ScreenEvent::Cursor { stream: id, shape: Some(shape) },
        };
        let _sent = events.try_send(WorkerMsg::Screen(event));
    };
    let sink = Arc::new(QuicSink(conn.clone()));
    let mut stream = match ScreenStream::open(id, target, quality, sink, on_event).await {
        Ok((stream, opened)) => {
            tracing::info!(%client, %id, ?target, "screen opened");
            daemon.screens.insert(&client, target, stream.stats_handle());
            let _told = told.send(Told::Opened(id, stream.control()));
            let _sent = out.send(WorkerMsg::Screen(opened)).await;
            stream
        }
        Err(e) => {
            tracing::warn!(%client, ?target, error = %e, "screen open");
            let event = ScreenEvent::Closed { stream: id, reason: e.to_string() };
            let _sent = out.send(WorkerMsg::Screen(event)).await;
            let _told = told.send(Told::Gone(id));
            return;
        }
    };

    let by_client = serve(&mut stream, client, &mut commands, &out).await;
    if by_client {
        tracing::info!(
            %client,
            %id,
            path = %slopty_net::endpoint::describe_health(&conn),
            "screen closing"
        );
    }
    // Whatever the client still holds down on the worker is let go now, not after the close has
    // waited on ScreenCaptureKit; a lost connection ends here too.
    stream.release_input();
    stream.close().await;
    daemon.screens.remove(&client, id);
    if by_client {
        let event = ScreenEvent::Closed { stream: id, reason: "closed by client".to_owned() };
        let _sent = out.send(WorkerMsg::Screen(event)).await;
    }
    let _told = told.send(Told::Gone(id));
}

/// Serve `stream`'s commands and follow its geometry until it is closed (`true`) or the
/// connection lets go of it (`false`).
///
/// Nothing here waits in front of the next command but the command itself. The window-server
/// probe and every encoder build (a resize's or a quality change's) run beside the commands,
/// and what the stream tells the client waits for room in the connection's queue in an arm of
/// its own, so input for the window never waits on VideoToolbox, the window server or a client
/// that is slow to read (MEASUREMENTS.md, "input behind a quality change").
pub async fn serve<P: Platform>(
    stream: &mut Pipeline<P>,
    client: ClientId,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    out: &mpsc::Sender<WorkerMsg>,
) -> bool {
    let mut geometry = tokio::time::interval(GEOMETRY_PERIOD);
    geometry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut probing: Option<tokio::task::JoinHandle<slopty_worker::screen::Probe>> = None;
    // The geometry is not probed again until the new encoder is in.
    let mut rebuilding: Option<Rebuild<P>> = None;
    let mut telling = Telling::default();
    let by_client = loop {
        tokio::select! {
            command = commands.recv() => match command {
                None => break false,
                Some(Command::Close) => break true,
                // Applied on top of a resize's build under way, not under it.
                Some(Command::SetQuality(quality)) => {
                    rebuilding = stream.set_quality(&quality, rebuilding.take());
                }
                Some(command) => apply(stream, client, command),
            },
            probe = async { probing.as_mut()?.await.ok() }, if probing.is_some() => {
                probing = None;
                let Some(probe) = probe else { continue };
                // Read before a quality change started a build: the next tick after it reads
                // again, and this one must not take the build's place.
                if rebuilding.is_some() {
                    continue;
                }
                rebuilding = stream.check_geometry(&probe);
                if let Some(event) = stream.check_source() {
                    telling.push(event);
                }
            }
            built = async { Some(rebuilding.as_mut()?.built().await) }, if rebuilding.is_some() => {
                if let (Some(rebuild), Some(built)) = (rebuilding.take(), built) {
                    match built {
                        Ok(encoder) => {
                            if let Some(event) = stream.finish_rebuild(rebuild, encoder) {
                                telling.push(event);
                            }
                        }
                        // The stream stays at its old size; a resize is seen again on the next
                        // ticks, and the next quality change asks again.
                        Err(e) => tracing::warn!(stream = %stream.id(), error = %e, "encoder rebuild"),
                    }
                }
            }
            permit = out.reserve(), if !telling.is_empty() => match permit {
                Ok(permit) => telling.send(permit),
                Err(_gone) => telling.clear(),
            },
            _ = geometry.tick(), if probing.is_none() && rebuilding.is_none() => {
                probing = Some(tokio::task::spawn_blocking(stream.prober()));
            }
        }
    };
    if let Some(probe) = probing {
        probe.abort();
    }
    // What is still untold is moot: the stream is ending, and `Closed` says so.
    by_client
}

/// What a stream's task has yet to tell its client, oldest first. A newer event of a kind
/// replaces the one waiting: each says where the stream is now (its size, whether its source
/// draws), so only the last matters, and the queue never holds more than one of each.
#[derive(Debug, Default)]
struct Telling(std::collections::VecDeque<ScreenEvent>);

impl Telling {
    fn push(&mut self, event: ScreenEvent) {
        let kind = std::mem::discriminant(&event);
        self.0.retain(|waiting| std::mem::discriminant(waiting) != kind);
        self.0.push_back(event);
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn send(&mut self, permit: mpsc::Permit<'_, WorkerMsg>) {
        if let Some(event) = self.0.pop_front() {
            permit.send(WorkerMsg::Screen(event));
        }
    }

    fn clear(&mut self) {
        self.0.clear();
    }
}

fn apply<P: Platform>(stream: &mut Pipeline<P>, client: ClientId, command: Command) {
    let id = stream.id();
    match command {
        Command::Input(input) => {
            if let Err(e) = stream.inject(&input) {
                tracing::debug!(%client, stream = %id, error = %e, "input");
            }
        }
        Command::Focus => {
            if let Err(e) = stream.focus() {
                tracing::debug!(%client, stream = %id, error = %e, "focus");
            }
        }
        Command::Resize { width, height } => {
            let Some((window, w, h)) = stream.resize_points(width, height) else {
                tracing::debug!(%client, stream = %id, "resize: not a window stream");
                return;
            };
            // Off the runtime: a few accessibility round trips.
            drop(tokio::task::spawn_blocking(move || {
                match slopty_worker::screen::resize_window(window, w, h) {
                    Ok(()) => tracing::debug!(%client, stream = %id, w, h, "window resized"),
                    Err(e) => tracing::debug!(%client, stream = %id, error = %e, "resize"),
                }
            }));
        }
        Command::SetQuality(_) | Command::Close => {}
    }
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "measurement arithmetic on small counts and microseconds"
)]
mod tests {
    use std::time::Duration;

    use slopty_worker::DatagramSink as _;

    use super::QuicSink;

    /// Frames of datagrams handed to QUIC from a plain thread, as the encoder's callback does, one
    /// call per frame, to a loopback client: the one-way time each datagram took and what the
    /// process spent per frame. `datagram_send_cost` in MEASUREMENTS.md.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "measurement"]
    async fn datagram_send_cost() {
        let (server, client, _endpoints) = pair().await;
        let sink = QuicSink(server);
        let epoch = std::time::Instant::now();
        let before = cpu_us();
        let producer = std::thread::spawn(move || {
            for n in 0..FRAMES {
                let started = std::time::Instant::now();
                let frame: Vec<bytes::Bytes> =
                    std::iter::repeat_with(|| stamped(epoch, n)).take(PER_FRAME).collect();
                sink.send(&frame).unwrap();
                #[expect(clippy::disallowed_methods, reason = "the frame clock of a test thread")]
                std::thread::sleep(FRAME.saturating_sub(started.elapsed()));
            }
        });
        let (latencies, whole) = receive(&client, epoch).await;
        let cpu = cpu_us().saturating_sub(before);
        producer.join().unwrap();
        report("direct send", &latencies, cpu);
        report("  whole frames", &whole, cpu);
    }

    const FRAMES: usize = 300;
    const PER_FRAME: usize = 64;
    const FRAME: Duration = Duration::from_micros(16_667);

    fn stamped(epoch: std::time::Instant, frame: usize) -> bytes::Bytes {
        let mut datagram = vec![0_u8; 1150];
        let us = u64::try_from(epoch.elapsed().as_micros()).unwrap();
        datagram[..8].copy_from_slice(&us.to_le_bytes());
        datagram[8..16].copy_from_slice(&(frame as u64).to_le_bytes());
        bytes::Bytes::from(datagram)
    }

    /// Each datagram's one-way time, and each frame's: from its first datagram's stamp to
    /// its last datagram's arrival, which is when a receiver could decode it.
    async fn receive(
        client: &slopty_net::Connection,
        epoch: std::time::Instant,
    ) -> (Vec<u64>, Vec<u64>) {
        let mut latencies = Vec::with_capacity(FRAMES * PER_FRAME);
        let mut frames: std::collections::HashMap<u64, (u64, u64, usize)> =
            std::collections::HashMap::new();
        let quiet = Duration::from_millis(500);
        while let Ok(Ok(datagram)) = tokio::time::timeout(quiet, client.read_datagram()).await {
            let sent = u64::from_le_bytes(datagram[..8].try_into().unwrap());
            let frame = u64::from_le_bytes(datagram[8..16].try_into().unwrap());
            let now = u64::try_from(epoch.elapsed().as_micros()).unwrap();
            latencies.push(now.saturating_sub(sent));
            let entry = frames.entry(frame).or_insert((sent, now, 0));
            *entry = (entry.0.min(sent), entry.1.max(now), entry.2.saturating_add(1));
        }
        let whole = frames
            .values()
            .filter(|(_, _, n)| *n == PER_FRAME)
            .map(|(first, last, _)| last.saturating_sub(*first))
            .collect();
        (latencies, whole)
    }

    fn report(label: &str, latencies: &[u64], cpu_us: u64) {
        let mut sorted = latencies.to_vec();
        sorted.sort_unstable();
        let at = |q: f64| {
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "index")]
            let i = ((sorted.len().saturating_sub(1)) as f64 * q).round() as usize;
            sorted.get(i).copied().unwrap_or(0)
        };
        eprintln!(
            "{label}: {} of {} datagrams, one-way p50 {} / p99 {} / max {} µs, process cpu {} µs per frame",
            sorted.len(),
            FRAMES * PER_FRAME,
            at(0.5),
            at(0.99),
            sorted.last().copied().unwrap_or(0),
            cpu_us / FRAMES as u64,
        );
    }

    /// This process's CPU time so far, microseconds, from `ps` (`MM:SS.cc`).
    fn cpu_us() -> u64 {
        let out = std::process::Command::new("ps")
            .args(["-o", "time=", "-p", &std::process::id().to_string()])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        let mut parts = text.rsplit(':');
        let secs: f64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        let mins: f64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a test's cpu time"
        )]
        let us = (mins.mul_add(60.0, secs) * 1e6) as u64;
        us
    }

    /// A loopback worker connection and the client end of it.
    async fn pair() -> (
        slopty_net::Connection,
        slopty_net::Connection,
        (slopty_net::worker::WorkerListener, slopty_net::Endpoint),
    ) {
        use slopty_proto::handshake::Hello;
        let listener = slopty_net::worker::WorkerListener::bind(
            std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            slopty_net::admission::Admission::default(),
        )
        .unwrap();
        let addr = listener.local_addr().unwrap();
        let endpoint = slopty_net::client::bind_client().unwrap();
        let hello = Hello { client: slopty_core::ClientId::new(), name: "bench".to_owned() };
        let dialing = endpoint.clone();
        let client =
            tokio::spawn(
                async move { slopty_net::client::connect_addr(&dialing, addr, hello).await },
            );
        let mut accepted = listener.accept().await.unwrap();
        let ack = slopty_proto::handshake::HelloAck {
            worker: slopty_core::WorkerId::new(),
            name: "bench".to_owned(),
            home: String::new(),
            caps: slopty_proto::server::WorkerCaps::default(),
            sessions: Vec::new(),
        };
        accepted.tx.send(&slopty_net::WorkerMsg::HelloAck(ack)).await.unwrap();
        let client = client.await.unwrap().unwrap();
        (accepted.conn, client.conn, (listener, endpoint))
    }
}

/// The stream's task over a platform of this test's own: a capture that starts and never
/// delivers, input that notes when each event was queued and at what scale, and an encoder
/// that is VideoToolbox's or one whose build waits for the test. Nothing reaches the screen,
/// the window server or the worker's input.
#[cfg(test)]
#[cfg(target_vendor = "apple")]
pub mod fake {
    use std::collections::HashMap;
    use std::marker::PhantomData;
    use std::sync::LazyLock;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use parking_lot::Mutex;
    use slopty_capture::{
        AudioSink, AxError, CaptureConfig, CaptureError, CaptureSource, CapturedFrame, Crop, Rect,
        TargetWindow, Went, WindowState,
    };
    use slopty_codec::{
        CodecError, EncodedPacket, EncoderConfig, FrameOptions, PixelBuffer, VideoEncoder,
    };
    use slopty_core::WindowId;
    use slopty_input::{InputError, InputSink, PointerWatch};
    use slopty_proto::screen::{CaptureTarget, CursorShape, DisplayInfo, ScreenInput, WindowInfo};
    use slopty_worker::platform::Platform;
    use tokio::sync::mpsc;

    /// A display of 800 × 500 points at two pixels a point.
    pub const NATIVE: (u32, u32) = (1600, 1000);

    /// The test platform, with `V` for its encoder.
    pub struct Fake<V>(PhantomData<V>);

    impl<V: VideoEncoder<Image = PixelBuffer>> Platform for Fake<V> {
        type Audio = slopty_codec::Opus;
        type Capture = Still;
        type Input = Noted;
        type Video = V;
    }

    /// A capture that starts, updates and stops at once and never delivers a frame.
    pub enum Still {}

    /// The display whose geometry reads take [`SLOW_READ`], as a busy window server's do.
    pub static SLOW_DISPLAY: AtomicU32 = AtomicU32::new(0);
    /// How long each of [`SLOW_DISPLAY`]'s geometry reads takes.
    pub const SLOW_READ: Duration = Duration::from_millis(60);
    /// A read of [`SLOW_DISPLAY`] is under way.
    pub static READING: AtomicBool = AtomicBool::new(false);
    /// The widths each capture update asked for: what a new encoder going in asks of the capture.
    pub static UPDATED: Mutex<Vec<u32>> = Mutex::new(Vec::new());

    impl CaptureSource for Still {
        type Content = ();
        type HideWatch = ();
        type Image = PixelBuffer;
        type Stream = ();
        type Target = ();

        fn can_capture() -> bool {
            true
        }

        fn enumerate(done: impl FnOnce(Result<(), CaptureError>) + Send + 'static) {
            done(Ok(()));
        }

        fn windows((): &()) -> Vec<WindowInfo> {
            Vec::new()
        }

        fn displays((): &()) -> Vec<DisplayInfo> {
            Vec::new()
        }

        fn resolve((): &(), _kind: CaptureTarget) -> Result<(), CaptureError> {
            Ok(())
        }

        fn resolve_crop((): &(), _id: WindowId) -> Result<Option<()>, CaptureError> {
            Ok(None)
        }

        fn crop((): &()) -> Option<Crop> {
            None
        }

        fn pixel_size((): &()) -> (u32, u32) {
            NATIVE
        }

        fn point_scale((): &()) -> f32 {
            2.0
        }

        fn start(
            (): &(),
            _config: &CaptureConfig,
            _sink: impl Fn(CapturedFrame<PixelBuffer>) + Send + Sync + 'static,
            _audio: Option<AudioSink>,
            _on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
            done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
        ) -> Result<(), CaptureError> {
            done(Ok(()));
            Ok(())
        }

        fn update(
            (): &(),
            config: &CaptureConfig,
            done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
        ) {
            UPDATED.lock().push(config.width);
            done(Ok(()));
        }

        fn retarget(
            (): &(),
            (): &(),
            done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
        ) {
            done(Ok(()));
        }

        fn stop((): &(), done: impl FnOnce(Result<(), CaptureError>) + Send + 'static) {
            done(Ok(()));
        }

        fn now_us() -> u64 {
            static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);
            u64::try_from(EPOCH.elapsed().as_micros()).unwrap_or(u64::MAX)
        }

        fn target_bounds(target: CaptureTarget) -> Option<Rect> {
            if target == CaptureTarget::Display(SLOW_DISPLAY.load(Ordering::SeqCst)) {
                READING.store(true, Ordering::SeqCst);
                #[expect(
                    clippy::disallowed_methods,
                    reason = "a blocking read, on the blocking pool"
                )]
                std::thread::sleep(SLOW_READ);
                READING.store(false, Ordering::SeqCst);
            }
            Some(Rect { x: 0.0, y: 0.0, w: 800.0, h: 500.0 })
        }

        fn refresh_hz(_target: CaptureTarget) -> Option<f64> {
            Some(60.0)
        }

        fn window_state(_id: WindowId) -> Option<WindowState> {
            None
        }

        fn window_bounds(_id: WindowId) -> Option<Rect> {
            None
        }

        fn window_owner(_id: WindowId) -> Option<i32> {
            None
        }

        fn window_on_screen(_id: WindowId) -> bool {
            false
        }

        fn window_title(_id: WindowId) -> Option<String> {
            None
        }

        fn occluded(_id: WindowId, _bounds: &Rect, _owner: i32) -> bool {
            false
        }

        fn display_enclosing(_rect: &Rect) -> Option<u32> {
            None
        }

        fn display_bounds(_id: u32) -> Rect {
            Rect::default()
        }

        fn resize_window(
            _pid: i32,
            _target: &TargetWindow,
            _width: f64,
            _height: f64,
        ) -> Result<(), AxError> {
            Err(AxError::Unsupported)
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

    /// One event as the stream queued it for its input thread.
    #[derive(Debug)]
    pub struct Queued {
        pub at: Instant,
        /// Stream pixels per display point the sink maps it with.
        pub scale: f64,
        pub input: ScreenInput,
    }

    /// Where each display's input sink tells its test what was queued.
    static NOTED: LazyLock<Mutex<HashMap<u32, mpsc::UnboundedSender<Queued>>>> =
        LazyLock::new(Mutex::default);

    /// What the sink of display `display` queues from now on.
    pub fn note(display: u32) -> mpsc::UnboundedReceiver<Queued> {
        let (tx, rx) = mpsc::unbounded_channel();
        NOTED.lock().insert(display, tx);
        rx
    }

    /// An input sink that notes each event instead of posting it: the point where the real one
    /// hands it to its thread.
    pub struct Noted {
        display: u32,
        scale: f64,
        pointer: PointerWatch,
    }

    impl InputSink for Noted {
        fn new(target: CaptureTarget, scale: f64) -> Self {
            let display = match target {
                CaptureTarget::Display(id) => id,
                CaptureTarget::Window(_) => 0,
            };
            Self { display, scale, pointer: PointerWatch::default() }
        }

        fn set_scale(&mut self, scale: f64) {
            self.scale = scale;
        }

        fn set_bounds(&mut self, _bounds: Option<Rect>, _at: Instant) {}

        fn inject(&mut self, input: &ScreenInput) -> Result<(), InputError> {
            let queued = Queued { at: Instant::now(), scale: self.scale, input: input.clone() };
            if let Some(tx) = NOTED.lock().get(&self.display) {
                let _gone = tx.send(queued);
            }
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

    /// Encoder sessions built so far, by either encoder here.
    pub static BUILT: AtomicU64 = AtomicU64::new(0);

    /// A VideoToolbox session, counted when built.
    pub struct Toolbox(slopty_codec::VideoToolbox);

    impl VideoEncoder for Toolbox {
        type Image = PixelBuffer;

        fn new(
            config: EncoderConfig,
            sink: impl Fn(EncodedPacket) + Send + Sync + 'static,
        ) -> Result<Self, CodecError> {
            let built = slopty_codec::VideoToolbox::new(config, sink).map(Self);
            BUILT.fetch_add(1, Ordering::SeqCst);
            built
        }

        fn encode(
            &self,
            image: &PixelBuffer,
            pts_us: u64,
            options: &FrameOptions,
        ) -> Result<(), CodecError> {
            self.0.encode(image, pts_us, options)
        }

        fn set_bitrate(&self, bps: u32) -> Result<(), CodecError> {
            self.0.set_bitrate(bps)
        }

        fn set_frame_rate(&self, fps: u16) -> Result<(), CodecError> {
            self.0.set_frame_rate(fps)
        }
    }

    /// The next build of a [`Gated`] session waits for one message on this.
    pub static GATE: Mutex<Option<std::sync::mpsc::Receiver<()>>> = Mutex::new(None);

    /// A session that encodes nothing, whose build waits on [`GATE`] when one is set.
    pub struct Gated;

    impl VideoEncoder for Gated {
        type Image = PixelBuffer;

        fn new(
            _config: EncoderConfig,
            _sink: impl Fn(EncodedPacket) + Send + Sync + 'static,
        ) -> Result<Self, CodecError> {
            let gate = GATE.lock().take();
            let _opened = gate.as_ref().map(std::sync::mpsc::Receiver::recv);
            BUILT.fetch_add(1, Ordering::SeqCst);
            Ok(Self)
        }

        fn encode(&self, _: &PixelBuffer, _: u64, _: &FrameOptions) -> Result<(), CodecError> {
            Ok(())
        }

        fn set_bitrate(&self, _bps: u32) -> Result<(), CodecError> {
            Ok(())
        }

        fn set_frame_rate(&self, _fps: u16) -> Result<(), CodecError> {
            Ok(())
        }
    }

    /// Datagrams that go nowhere.
    pub struct Nowhere;

    impl slopty_worker::DatagramSink for Nowhere {
        fn send(&self, _datagrams: &[bytes::Bytes]) -> Result<(), slopty_worker::screen::Refused> {
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
}

#[cfg(test)]
#[cfg(target_vendor = "apple")]
#[expect(
    clippy::cast_precision_loss,
    clippy::arithmetic_side_effects,
    reason = "measurement arithmetic on small counts"
)]
mod serving {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use slopty_core::{ClientId, StreamId};
    use slopty_net::WorkerMsg;
    use slopty_proto::screen::{CaptureTarget, Quality, ScreenInput};
    use slopty_worker::platform::Platform;
    use slopty_worker::screen::Pipeline;
    use tokio::sync::mpsc;

    use super::fake::{BUILT, Fake, Gated, Nowhere, Queued, Toolbox, note};
    use super::{Command, serve};

    /// A stream of display `display` on `P`, served on a task of its own: its command queue, the
    /// events it sends the client, and what its input sink queued.
    async fn served<P: Platform>(
        display: u32,
        depth: usize,
        full: bool,
    ) -> (
        mpsc::UnboundedSender<Command>,
        mpsc::Receiver<WorkerMsg>,
        mpsc::UnboundedReceiver<Queued>,
        tokio::task::JoinHandle<()>,
    ) {
        let queued = note(display);
        let target = CaptureTarget::Display(display);
        let sink: Arc<dyn slopty_worker::DatagramSink> = Arc::new(Nowhere);
        let (mut stream, _opened) =
            Pipeline::<P>::open(StreamId(display), target, Quality::default(), sink, |_event| {})
                .await
                .unwrap();
        let (commands_tx, mut commands) = mpsc::unbounded_channel();
        let (out, events) = mpsc::channel(depth);
        while full && out.try_send(filler()).is_ok() {}
        let task = tokio::spawn(async move {
            serve(&mut stream, ClientId::new(), &mut commands, &out).await;
            stream.close().await;
        });
        (commands_tx, events, queued, task)
    }

    fn filler() -> WorkerMsg {
        WorkerMsg::Pong { sent_at: slopty_core::MonoTime::now() }
    }

    fn quality(scale: f32) -> Quality {
        Quality { scale, ..Quality::default() }
    }

    fn at(x: f32) -> ScreenInput {
        ScreenInput::Move { x, y: 0.0 }
    }

    async fn next(queued: &mut mpsc::UnboundedReceiver<Queued>) -> Queued {
        tokio::time::timeout(Duration::from_secs(5), queued.recv()).await.unwrap().unwrap()
    }

    async fn built(past: u64) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while BUILT.load(Ordering::SeqCst) <= past {
            assert!(Instant::now() < deadline, "no session was built");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    /// How long an input queued right behind a quality change that rebuilds the encoder waits
    /// before the stream hands it to its input thread, with VideoToolbox building the sessions.
    /// `input_behind_a_quality_change` in MEASUREMENTS.md.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "measurement"]
    async fn input_behind_a_quality_change() {
        const ROUNDS: usize = 40;
        let (commands, mut events, mut queued, task) =
            served::<Fake<Toolbox>>(11, 1024, false).await;
        tokio::spawn(async move { while events.recv().await.is_some() {} });
        let mut waits = Vec::with_capacity(ROUNDS);
        for round in 0..ROUNDS {
            let scale = if round % 2 == 0 { 0.5 } else { 1.0 };
            let before = BUILT.load(Ordering::SeqCst);
            commands.send(Command::SetQuality(quality(scale))).unwrap();
            let sent = Instant::now();
            commands.send(Command::Input(at(round as f32))).unwrap();
            let got = next(&mut queued).await;
            waits.push(got.at.saturating_duration_since(sent));
            built(before).await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        drop(commands);
        task.await.unwrap();
        waits.sort_unstable();
        let us = |d: Duration| d.as_micros();
        eprintln!(
            "input_behind_a_quality_change: {ROUNDS} rounds, queued after p50 {} / p90 {} / max {} µs",
            us(waits[ROUNDS / 2]),
            us(waits[ROUNDS * 9 / 10]),
            us(waits[ROUNDS - 1]),
        );
    }

    /// The encoder a quality change asks for is still being built, and the input sent after
    /// the change is already with the input thread, mapped at the scale the change asked for.
    #[tokio::test(flavor = "multi_thread")]
    async fn input_behind_a_quality_change_does_not_wait_for_the_encoder() {
        let (commands, mut events, mut queued, task) = served::<Fake<Gated>>(12, 1024, false).await;
        tokio::spawn(async move { while events.recv().await.is_some() {} });
        let (open, gate) = std::sync::mpsc::channel();
        *super::fake::GATE.lock() = Some(gate);
        let before = BUILT.load(Ordering::SeqCst);
        commands.send(Command::SetQuality(quality(0.5))).unwrap();
        commands.send(Command::Input(at(7.0))).unwrap();
        let got = tokio::time::timeout(Duration::from_secs(2), queued.recv()).await;
        let still_building = BUILT.load(Ordering::SeqCst) == before;
        open.send(()).unwrap();
        let got = got.expect("the input waited for the encoder").unwrap();
        assert!(still_building, "the build finished first; the test proves nothing");
        assert_eq!(got.input, at(7.0), "the input sent");
        assert!(
            (got.scale - 1.0).abs() < 1e-9,
            "two pixels a point at the asked half: {}",
            got.scale
        );
        built(before).await;
        drop(commands);
        task.await.unwrap();
    }

    /// A second quality change while the first one's encoder is still being built replaces
    /// that build: input maps at the newest scale at once, and the stream goes on serving once
    /// both builds are done.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_quality_change_replaces_the_build_of_the_one_before() {
        let (commands, mut events, mut queued, task) = served::<Fake<Gated>>(13, 1024, false).await;
        tokio::spawn(async move { while events.recv().await.is_some() {} });
        let (open, gate) = std::sync::mpsc::channel();
        *super::fake::GATE.lock() = Some(gate);
        let before = BUILT.load(Ordering::SeqCst);
        commands.send(Command::SetQuality(quality(0.5))).unwrap();
        commands.send(Command::SetQuality(quality(0.25))).unwrap();
        commands.send(Command::Input(at(1.0))).unwrap();
        let got = next(&mut queued).await;
        assert!(
            (got.scale - 0.5).abs() < 1e-9,
            "two pixels a point at the newest quarter: {}",
            got.scale
        );
        open.send(()).unwrap();
        built(before + 1).await;
        commands.send(Command::Input(at(2.0))).unwrap();
        assert_eq!(next(&mut queued).await.input, at(2.0), "still serving");
        drop(commands);
        task.await.unwrap();
    }

    /// The client's queue is full when the stream has news for it (here, that its source has
    /// not drawn): the news waits for room, and input goes on meanwhile.
    #[tokio::test(flavor = "multi_thread")]
    async fn news_the_client_has_no_room_for_does_not_hold_input() {
        use slopty_proto::screen::{ScreenEvent, SourceState};

        let (commands, mut events, mut queued, task) = served::<Fake<Gated>>(14, 1, true).await;
        // Past the idle grace, and a geometry tick after it.
        tokio::time::sleep(slopty_worker::screen::SOURCE_IDLE_AFTER + Duration::from_millis(250))
            .await;
        commands.send(Command::Input(at(3.0))).unwrap();
        assert_eq!(next(&mut queued).await.input, at(3.0), "the input went on");
        let first = events.recv().await.unwrap();
        assert!(matches!(first, WorkerMsg::Pong { .. }), "{first:?}");
        let told = tokio::time::timeout(Duration::from_secs(2), events.recv()).await.unwrap();
        assert!(
            matches!(
                told,
                Some(WorkerMsg::Screen(ScreenEvent::Source { state: SourceState::Idle, .. }))
            ),
            "the news, once there was room: {told:?}"
        );
        drop(commands);
        task.await.unwrap();
    }

    /// A geometry read that began before a quality change and lands while its encoder is still
    /// being built is let go: the build goes in, and the capture is asked for its size.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_read_landing_during_a_quality_build_does_not_drop_it() {
        use super::fake::{READING, SLOW_DISPLAY, SLOW_READ, UPDATED};

        SLOW_DISPLAY.store(15, Ordering::SeqCst);
        let (commands, mut events, _queued, task) = served::<Fake<Gated>>(15, 1024, false).await;
        tokio::spawn(async move { while events.recv().await.is_some() {} });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !READING.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "no geometry read began");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let (open, gate) = std::sync::mpsc::channel();
        *super::fake::GATE.lock() = Some(gate);
        let before = BUILT.load(Ordering::SeqCst);
        commands.send(Command::SetQuality(quality(0.5))).unwrap();
        // The read lands while the build waits.
        tokio::time::sleep(SLOW_READ + Duration::from_millis(40)).await;
        open.send(()).unwrap();
        built(before).await;
        let wanted = super::fake::NATIVE.0 / 2;
        let deadline = Instant::now() + Duration::from_secs(2);
        while !UPDATED.lock().contains(&wanted) {
            assert!(Instant::now() < deadline, "the build never went in: {:?}", UPDATED.lock());
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        drop(commands);
        task.await.unwrap();
    }
}
