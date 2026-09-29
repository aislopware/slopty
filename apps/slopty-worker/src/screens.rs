//! One screen stream on a client's connection: the QUIC datagrams it sends into, and the task
//! that owns it.
//!
//! The task is the only thing that touches the [`Pipeline`], so everything the stream does
//! slowly — the 115–300 ms open, an encoder rebuild, a close waiting on ScreenCaptureKit, the
//! geometry probe's window-server reads — waits only for that stream. The connection's own loop
//! hands commands over and goes straight back to the terminals.

use std::sync::Arc;

use bytes::Bytes;
use slopty_core::{ClientId, StreamId};
use slopty_input::sources::Claim;
use slopty_net::{Connection, WorkerMsg};
use slopty_proto::screen::{CaptureTarget, Quality, ScreenEvent, ScreenInput};
use slopty_worker::platform::{Native, Platform};
#[cfg(target_os = "macos")]
use slopty_worker::screen::synthetic::Synthetic;
use slopty_worker::screen::{
    Pipeline, Rebuild, Refused, ScreenError, StreamControl, StreamEvent, sized,
};
use tokio::sync::mpsc;

use crate::Daemon;

/// The most a stream holds its client's keys and text behind an input-source switch it asked
/// for. The switch is answered once the worker hears it (`sources::SETTLE_MOST` bounds that),
/// and the keys typed meanwhile wait so they are read under the source they were typed for; a
/// main queue busy elsewhere (a display being made) lets them go under whatever source is
/// current. Tests hold far longer, so a loaded machine cannot let a key go before the answer a
/// test waits on.
const HOLD_MOST: std::time::Duration = if cfg!(test) {
    std::time::Duration::from_secs(10)
} else {
    std::time::Duration::from_millis(150)
};

/// Whether the worker types under the source a stream asked for, once it is known.
type Answer = std::pin::Pin<Box<dyn Future<Output = bool> + Send>>;

/// How often a stream's target is probed while it has something to follow
/// ([`Pipeline::geometry_quiet`]): a window served as a crop, a source that draws, a change
/// under way. The accessibility API wakes a probe at once when the target moves or is resized,
/// so a window on the display-crop path follows a drag within ScreenCaptureKit's ~20 ms
/// configuration update of the step (MEASUREMENTS.md, "capture floor"); another application's
/// window moving over it is seen within this period.
const GEOMETRY_PERIOD: std::time::Duration = std::time::Duration::from_millis(100);

/// How long a quiet stream's target goes unprobed when nothing wakes it: a display resized
/// under the stream, which announces nothing to the stream, is seen within this.
const GEOMETRY_BACKSTOP: std::time::Duration = std::time::Duration::from_secs(1);

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
    Resize { width: u32, height: u32, scale: Option<f32> },
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
/// connection lets go of it. A worker serving the drawn screen opens it there
/// ([`slopty_worker::screen::synthetic_screen`]).
pub async fn run(
    link: Link,
    id: StreamId,
    target: CaptureTarget,
    quality: Quality,
    commands: mpsc::UnboundedReceiver<Command>,
) {
    #[cfg(target_os = "macos")]
    if slopty_worker::screen::synthetic_screen() {
        return run_on::<Synthetic>(link, id, target, quality, commands).await;
    }
    run_on::<Native>(link, id, target, quality, commands).await;
}

async fn run_on<P: Platform>(
    link: Link,
    id: StreamId,
    target: CaptureTarget,
    quality: Quality,
    commands: mpsc::UnboundedReceiver<Command>,
) {
    let on_event = on_event(id, link.out.clone());
    let sink = Arc::new(QuicSink(link.conn.clone()));
    let opened = Pipeline::<P>::open(id, target, quality, sink, on_event).await;
    let opened = opened.map(|(stream, opened)| (stream, vec![opened], None));
    serve_opened(link, id, opened, commands).await;
}

/// Open a stream of a display made for the client (or, when none can be had, of a physical
/// display), then serve it as [`run`] does; the display goes when the stream does. A worker
/// serving the drawn screen makes no display and streams its drawn one.
pub async fn run_display(
    link: Link,
    id: StreamId,
    asked: sized::Asked,
    commands: mpsc::UnboundedReceiver<Command>,
) {
    let on_event = on_event(id, link.out.clone());
    let sink = Arc::new(QuicSink(link.conn.clone()));
    #[cfg(target_os = "macos")]
    if slopty_worker::screen::synthetic_screen() {
        let opened = sized::open::<Synthetic, sized::Cg>(None, id, asked, sink, on_event).await;
        let opened = opened.map(|(stream, told, sized)| (stream, told.into(), sized));
        return serve_opened(link, id, opened, commands).await;
    }
    let displays = link.daemon.displays.as_ref();
    let opened = sized::open::<Native, sized::Cg>(displays, id, asked, sink, on_event).await;
    let opened = opened.map(|(stream, told, sized)| (stream, told.into(), sized));
    serve_opened(link, id, opened, commands).await;
}

/// What a stream tells its client outside its commands' answers.
fn on_event(id: StreamId, events: mpsc::Sender<WorkerMsg>) -> impl Fn(StreamEvent) + Send + Sync {
    move |e: StreamEvent| {
        let event = match e {
            StreamEvent::Stopped(e) => ScreenEvent::Closed { stream: id, reason: e.to_string() },
            StreamEvent::Cursor(shape) => ScreenEvent::Cursor { stream: id, shape: Some(shape) },
        };
        let _sent = events.try_send(WorkerMsg::Screen(event));
    }
}

/// A stream as it opened: the stream, what to tell the client first, and for a display made
/// for the client what serves its resizes.
type Opened<P> = Result<(Pipeline<P>, Vec<ScreenEvent>, Option<sized::Sized>), ScreenError>;

/// Tell the client how the stream opened, serve it until it is closed or the connection lets
/// go of it, and tear it down: input released, capture stopped, then any display made for it
/// released on the main thread.
async fn serve_opened<P: Platform>(
    link: Link,
    id: StreamId,
    opened: Opened<P>,
    mut commands: mpsc::UnboundedReceiver<Command>,
) {
    let Link { daemon, client, conn, out, told } = link;
    let (mut stream, mut sized) = match opened {
        Ok((stream, events, sized)) => {
            let target = stream.target();
            tracing::info!(%client, %id, ?target, made = sized.is_some(), "screen opened");
            daemon.screens.insert(&client, target, stream.stats_handle());
            let _told = told.send(Told::Opened(id, stream.control()));
            for event in events {
                let _sent = out.send(WorkerMsg::Screen(event)).await;
            }
            (stream, sized)
        }
        Err(e) => {
            tracing::warn!(%client, %id, error = %e, "screen open");
            let event = ScreenEvent::Closed { stream: id, reason: e.to_string() };
            let _sent = out.send(WorkerMsg::Screen(event)).await;
            let _told = told.send(Told::Gone(id));
            return;
        }
    };

    // Dropped however the task ends, a panic included, so the claim never outlives the stream.
    let claim = daemon.sources.claimant();
    let by_client = serve(&mut stream, client, &mut commands, &out, sized.as_mut(), &claim).await;
    if by_client {
        tracing::info!(
            %client,
            %id,
            path = %slopty_net::endpoint::describe_health(&conn),
            "screen closing"
        );
    }
    // Whatever the client still holds down on the worker is let go now, not after the close has
    // waited on ScreenCaptureKit; a lost connection ends here too. The input source it asked
    // for goes back, after its ask: both are queued in order.
    stream.release_input();
    drop(claim);
    stream.close().await;
    drop(sized);
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
///
/// A display made for the client (`sized`) takes the stream's resizes, and the stream follows
/// it to the display it switches to; that switch waits on ScreenCaptureKit (a filter update,
/// ~20 ms), once per rescale or remake.
///
/// The client's input source is asked through `claim` from here, in the command's order, so the
/// claim's release is queued after it. Keys and text after the ask wait for its answer (at most
/// [`HOLD_MOST`]) so they are read under it; the pointer does not, since no source changes what
/// a click or a scroll does, unless a key is already held ahead of it and order must hold. An
/// ask for the source the worker is under already is answered at once and holds nothing. A
/// switch another stream makes is heard here too, and the client is told whether the worker
/// still types under its source.
pub async fn serve<P: Platform>(
    stream: &mut Pipeline<P>,
    client: ClientId,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    out: &mpsc::Sender<WorkerMsg>,
    mut sized: Option<&mut sized::Sized>,
    claim: &Claim,
) -> bool {
    let woken = stream.geometry_wake();
    let mut next_probe = tokio::time::Instant::now();
    let mut input_at: Option<tokio::time::Instant> = None;
    let mut probing: Option<tokio::task::JoinHandle<slopty_worker::screen::Probe>> = None;
    // The geometry is not probed again until the new encoder is in.
    let mut rebuilding: Option<Rebuild<P>> = None;
    let mut telling = Telling::default();
    let mut heard = claim.subscribe();
    heard.mark_unchanged();
    // The source the client asked for, and whether it was last told the worker types under it.
    let mut claimed: Option<(String, bool)> = None;
    // The client's input source being selected: the ask and its answer to come.
    let mut sourcing: Option<(String, Answer)> = None;
    // Keys and text the client sent after its ask, and whatever came after them, held until the
    // answer or the deadline.
    let mut held = std::collections::VecDeque::new();
    let mut hold_until: Option<tokio::time::Instant> = None;
    let by_client = loop {
        tokio::select! {
            command = commands.recv() => match command {
                None => break false,
                Some(Command::Close) => break true,
                // The newest ask wins: the main queue selects in order, and only its answer goes.
                Some(Command::Input(ScreenInput::KeyboardSource { source })) => {
                    let answer = Box::pin(claim.ask(source.clone()));
                    sourcing = Some((source, answer));
                    let now = tokio::time::Instant::now();
                    hold_until.get_or_insert_with(|| now.checked_add(HOLD_MOST).unwrap_or(now));
                }
                // The tile has been idle: the worker's own source comes back unless another
                // stream asks, and what waited on the ask goes under it.
                Some(Command::Input(ScreenInput::KeyboardReleased)) => {
                    claim.release();
                    sourcing = None;
                    claimed = None;
                    hold_until = None;
                    for command in std::mem::take(&mut held) {
                        take_command(stream, client, command, &mut input_at, &mut next_probe);
                    }
                }
                // Applied on top of a resize's build under way, not under it.
                Some(Command::SetQuality(quality)) => {
                    rebuilding = stream.set_quality(&quality, rebuilding.take());
                }
                Some(Command::Resize { width, height, scale }) if sized.is_some() => {
                    if let Some(sized) = sized.as_deref() {
                        (sized.resize)(width, height, scale);
                    }
                }
                Some(command @ (Command::Input(_) | Command::Focus))
                    if hold_until.is_some() && (!held.is_empty() || typed(&command)) =>
                {
                    held.push_back(command);
                }
                Some(command) => {
                    take_command(stream, client, command, &mut input_at, &mut next_probe);
                }
            },
            applied = async {
                match sourcing.as_mut() {
                    Some((_, answer)) => answer.await,
                    None => std::future::pending().await,
                }
            }, if sourcing.is_some() => {
                if let Some((source, _)) = sourcing.take() {
                    let event = ScreenEvent::KeyboardSource {
                        stream: stream.id(),
                        source: source.clone(),
                        applied,
                    };
                    telling.push(event);
                    claimed = Some((source, applied));
                }
                hold_until = None;
                for command in std::mem::take(&mut held) {
                    take_command(stream, client, command, &mut input_at, &mut next_probe);
                }
            }
            () = async {
                match hold_until {
                    Some(until) => tokio::time::sleep_until(until).await,
                    None => std::future::pending().await,
                }
            }, if hold_until.is_some() => {
                tracing::debug!(stream = %stream.id(), "input source switch unanswered; typing on");
                hold_until = None;
                for command in std::mem::take(&mut held) {
                    take_command(stream, client, command, &mut input_at, &mut next_probe);
                }
            }
            changed = heard.changed(), if claimed.is_some() && sourcing.is_none() => {
                if changed.is_err() {
                    claimed = None;
                    continue;
                }
                let now = heard.borrow_and_update().clone();
                if let Some((source, told)) = claimed.as_mut() {
                    let applied = now.as_deref() == Some(source.as_str());
                    if applied != *told {
                        *told = applied;
                        let source = source.clone();
                        telling.push(ScreenEvent::KeyboardSource { stream: stream.id(), source, applied });
                    }
                }
            }
            switch = async { sized.as_deref_mut()?.switches.recv().await }, if sized.is_some() => {
                let key = sized.as_deref().map(|s| s.key);
                let (Some((display, told)), Some(key)) = (switch, key) else {
                    sized = None;
                    continue;
                };
                // A read under way is of the display being left.
                if let Some(probe) = probing.take() {
                    probe.abort();
                }
                match stream.switch_display(display).await {
                    Ok(()) => {
                        telling.push(ScreenEvent::Display { stream: stream.id(), key, display: told });
                    }
                    Err(e) => tracing::warn!(stream = %stream.id(), error = %e, "display switch"),
                }
            }
            probe = async { probing.as_mut()?.await.ok() }, if probing.is_some() => {
                probing = None;
                let Some(probe) = probe else { continue };
                // Read before a quality change started a build: the next probe after it reads
                // again, and this one must not take the build's place.
                if rebuilding.is_some() {
                    continue;
                }
                rebuilding = stream.check_geometry(&probe);
                if let Some(event) = stream.check_source() {
                    telling.push(event);
                }
                next_probe = geometry_due(stream, input_at);
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
                        Err(e) => {
                            tracing::warn!(stream = %stream.id(), error = %e, "encoder rebuild");
                            stream.rebuild_failed(&rebuild);
                        }
                    }
                }
            }
            permit = out.reserve(), if !telling.is_empty() => match permit {
                Ok(permit) => telling.send(permit),
                Err(_gone) => telling.clear(),
            },
            () = tokio::time::sleep_until(next_probe), if probing.is_none() && rebuilding.is_none() => {
                probing = Some(tokio::task::spawn_blocking(stream.prober()));
            }
            () = woken.notified(), if probing.is_none() && rebuilding.is_none() => {
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

/// When the probe after the one just checked is due: a period on while the stream has
/// something to follow or took input within the backstop (`input_at`), the backstop once it has
/// neither ([`Pipeline::geometry_quiet`]).
fn geometry_due<P: Platform>(
    stream: &Pipeline<P>,
    input_at: Option<tokio::time::Instant>,
) -> tokio::time::Instant {
    let now = tokio::time::Instant::now();
    let typing = input_at.is_some_and(|at| now.saturating_duration_since(at) < GEOMETRY_BACKSTOP);
    let wait = if stream.geometry_quiet() && !typing { GEOMETRY_BACKSTOP } else { GEOMETRY_PERIOD };
    now.checked_add(wait).unwrap_or(now)
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

/// Apply `command`; input also keeps the geometry probe at its period. Input maps through the
/// probe's bounds, which the injector reads again itself, in front of the event, once they are
/// older than its `BOUNDS_TTL`: a stream that takes input follows at the period.
/// Whether `command` is read under the worker's input source: a key or text.
const fn typed(command: &Command) -> bool {
    matches!(command, Command::Input(ScreenInput::Key { .. } | ScreenInput::Text { .. }))
}

fn take_command<P: Platform>(
    stream: &mut Pipeline<P>,
    client: ClientId,
    command: Command,
    input_at: &mut Option<tokio::time::Instant>,
    next_probe: &mut tokio::time::Instant,
) {
    if matches!(command, Command::Input(_)) {
        let now = tokio::time::Instant::now();
        *input_at = Some(now);
        if *next_probe > now.checked_add(GEOMETRY_PERIOD).unwrap_or(now) {
            *next_probe = now;
        }
    }
    apply(stream, client, command);
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
        Command::Resize { width, height, .. } => {
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
            caps: slopty_proto::server::WorkerCaps::bare(slopty_proto::server::Os::MacOs),
            load: 0.0,
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
    /// The displays the fake enumeration lists.
    pub static LISTED: Mutex<Vec<u32>> = Mutex::new(Vec::new());
    /// Geometry reads of each display so far.
    static PROBED: LazyLock<Mutex<HashMap<u32, u64>>> = LazyLock::new(Mutex::default);

    /// How many times the geometry of display `display` has been read.
    pub fn probed(display: u32) -> u64 {
        PROBED.lock().get(&display).copied().unwrap_or(0)
    }

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
            let listed = LISTED.lock().clone();
            listed
                .into_iter()
                .map(|id| DisplayInfo {
                    id: slopty_core::DisplayId(id),
                    w: 800.0,
                    h: 500.0,
                    scale: 2.0,
                    hz: 60.0,
                })
                .collect()
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
            if let CaptureTarget::Display(display) = target {
                PROBED
                    .lock()
                    .entry(display.0)
                    .and_modify(|n| *n = n.saturating_add(1))
                    .or_insert(1);
            }
            if target
                == CaptureTarget::Display(slopty_core::DisplayId(
                    SLOW_DISPLAY.load(Ordering::SeqCst),
                ))
            {
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
                CaptureTarget::Display(id) => id.0,
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

    /// A session that encodes nothing and is built at once, whatever [`GATE`] holds.
    pub struct Plain;

    impl VideoEncoder for Plain {
        type Image = PixelBuffer;

        fn new(
            _config: EncoderConfig,
            _sink: impl Fn(EncodedPacket) + Send + Sync + 'static,
        ) -> Result<Self, CodecError> {
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

    /// A claim on input sources no stream can take: the client composes.
    pub fn unsourced() -> slopty_input::sources::Claim {
        slopty_input::sources::Sources::new(slopty_input::sources::Unsupported).claimant()
    }

    /// A worker's input sources as a test plays them: every source selectable, run at once.
    #[derive(Clone, Default)]
    pub struct Selectable(pub std::sync::Arc<Mutex<Option<String>>>);

    impl slopty_input::sources::Tis for Selectable {
        fn on_main(&self, job: Box<dyn FnOnce() + Send>) {
            job();
        }

        fn current(&self) -> Option<String> {
            self.0.lock().clone()
        }

        fn select(&self, id: &str) -> Result<bool, String> {
            *self.0.lock() = Some(id.to_owned());
            Ok(false)
        }

        fn disable(&self, _id: &str) {}
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

    use super::fake::{BUILT, Fake, Gated, Nowhere, Plain, Queued, Toolbox, note, unsourced};
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
        let target = CaptureTarget::Display(slopty_core::DisplayId(display));
        let sink: Arc<dyn slopty_worker::DatagramSink> = Arc::new(Nowhere);
        let (mut stream, _opened) =
            Pipeline::<P>::open(StreamId(display), target, Quality::default(), sink, |_event| {})
                .await
                .unwrap();
        let (commands_tx, mut commands) = mpsc::unbounded_channel();
        let (out, events) = mpsc::channel(depth);
        while full && out.try_send(filler()).is_ok() {}
        let task = tokio::spawn(async move {
            serve(&mut stream, ClientId::new(), &mut commands, &out, None, &unsourced()).await;
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

    /// A stream of display `display` served on a task of its own, whose events are drained:
    /// its command queue, what wakes its geometry probe, and the task.
    async fn served_quiet(
        display: u32,
    ) -> (mpsc::UnboundedSender<Command>, Arc<tokio::sync::Notify>, tokio::task::JoinHandle<()>)
    {
        let target = CaptureTarget::Display(slopty_core::DisplayId(display));
        let sink: Arc<dyn slopty_worker::DatagramSink> = Arc::new(Nowhere);
        let (mut stream, _opened) = Pipeline::<Fake<Plain>>::open(
            StreamId(display),
            target,
            Quality::default(),
            sink,
            |_event| {},
        )
        .await
        .unwrap();
        let wake = stream.geometry_wake();
        let (commands_tx, mut commands) = mpsc::unbounded_channel();
        let (out, mut events) = mpsc::channel(1024);
        tokio::spawn(async move { while events.recv().await.is_some() {} });
        let task = tokio::spawn(async move {
            serve(&mut stream, ClientId::new(), &mut commands, &out, None, &unsourced()).await;
            stream.close().await;
        });
        (commands_tx, wake, task)
    }

    /// A stream with nothing to follow (the client told its source is idle, nothing under
    /// way) is probed at the backstop rather than every period; a wake (the accessibility
    /// API's word that the target moved) probes it at once, and input puts it back on the
    /// period.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_quiet_stream_is_probed_when_woken_not_every_period() {
        use super::fake::probed;
        const DISPLAY: u32 = 16;
        let (commands, wake, task) = served_quiet(DISPLAY).await;
        // Past the idle grace, and a probe after it that finds the stream quiet.
        tokio::time::sleep(slopty_worker::screen::SOURCE_IDLE_AFTER + Duration::from_millis(300))
            .await;
        let before = probed(DISPLAY);
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        let quiet = probed(DISPLAY) - before;
        assert!(quiet <= 2, "{quiet} probes in 1.5 s; the follow period makes 15");
        let before = probed(DISPLAY);
        wake.notify_one();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(probed(DISPLAY) - before, 1, "a wake probes at once");
        // Input maps through the probe's bounds, so a stream that takes it follows again.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let before = probed(DISPLAY);
        commands.send(Command::Input(at(1.0))).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(probed(DISPLAY) - before, 1, "the first input probes at once");
        tokio::time::sleep(Duration::from_millis(500)).await;
        let following = probed(DISPLAY) - before;
        assert!(following >= 5, "{following} probes in 550 ms after input; the period makes 6");
        drop(commands);
        task.await.unwrap();
    }

    /// Geometry probes of an idle stream over ten seconds on the test platform: a display
    /// whose source is idle and that nothing moves. `idle stream wakeups` in MEASUREMENTS.md.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "measurement"]
    async fn idle_stream_probes() {
        use super::fake::probed;
        const DISPLAY: u32 = 17;
        const WINDOW: Duration = Duration::from_secs(10);
        let (commands, _wake, task) = served_quiet(DISPLAY).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        let before = probed(DISPLAY);
        tokio::time::sleep(WINDOW).await;
        let probes = probed(DISPLAY) - before;
        eprintln!(
            "idle_stream_probes: {probes} probes in {} s, {:.1} a second",
            WINDOW.as_secs(),
            probes as f64 / WINDOW.as_secs_f64()
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

/// A stream of a display made for its client, over the test platform and a factory whose
/// displays settle at once: nothing here makes a real display.
#[cfg(test)]
#[cfg(target_vendor = "apple")]
mod made {
    use std::sync::Arc;
    use std::time::Duration;

    use parking_lot::Mutex;
    use slopty_core::{ClientId, DisplayId, StreamId};
    use slopty_net::WorkerMsg;
    use slopty_proto::screen::{
        CaptureTarget, DisplayKey, DisplayShape, NoVirtualDisplay, Quality, ScreenEvent,
        VirtualDisplay,
    };
    use slopty_worker::screen::sized::{
        self, Asked, DisplayError, Displays, Enforced, Factory, Job, Main, Plan, Registry,
    };
    use tokio::sync::mpsc;

    use super::fake::{Fake, Gated, LISTED, Nowhere, unsourced};
    use super::{Command, serve};

    /// Displays numbered from 101 that settle at once and hold up to `max` pixels a side; what
    /// is alive is in `alive`.
    #[derive(Clone, Default)]
    struct Instantly {
        next: u32,
        max: u32,
        alive: Arc<Mutex<Vec<u32>>>,
    }

    struct Made(u32, u32, Arc<Mutex<Vec<u32>>>);

    impl Drop for Made {
        fn drop(&mut self) {
            self.2.lock().retain(|id| *id != self.0);
        }
    }

    impl Factory for Instantly {
        type Display = Made;

        fn create(&mut self, _plan: &Plan) -> Result<Made, DisplayError> {
            self.next = self.next.max(100).saturating_add(1);
            self.alive.lock().push(self.next);
            Ok(Made(self.next, self.max, Arc::clone(&self.alive)))
        }

        fn resize(&mut self, display: &mut Made, plan: &Plan) -> Result<(), DisplayError> {
            if plan.mode.fits((display.1, display.1)) {
                Ok(())
            } else {
                Err(DisplayError::Outgrown {
                    wanted: plan.mode.pixels,
                    max: (display.1, display.1),
                })
            }
        }

        fn enforce(&mut self, _display: &Made) -> Result<Enforced, DisplayError> {
            Ok(Enforced::Settled)
        }

        fn id(&self, display: &Made) -> u32 {
            display.0
        }
    }

    struct TaskMain<S>(mpsc::UnboundedSender<Job<S>>);

    impl<S: Send + 'static> Main<S> for TaskMain<S> {
        fn run(&self, job: Job<S>) {
            let _gone = self.0.send(job);
        }

        fn run_after(&self, delay: Duration, job: Job<S>) {
            let tx = self.0.clone();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let _gone = tx.send(job);
            });
        }
    }

    fn displays(factory: Instantly) -> Displays<Instantly> {
        let (tx, mut rx) = mpsc::unbounded_channel::<Job<Registry<Instantly>>>();
        let mut registry = Registry::new(factory);
        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                job(&mut registry);
            }
        });
        Displays::new(Arc::new(TaskMain(tx)), Duration::from_secs(5))
    }

    const KEY: DisplayKey = DisplayKey(*b"slopty-made-key!");

    fn asked(width: u32, height: u32) -> Asked {
        Asked {
            key: KEY,
            shape: DisplayShape { width, height, scale: 2.0, refresh_hz: 60 },
            quality: Quality::default(),
        }
    }

    async fn open(
        displays: Option<&Displays<Instantly>>,
        asked: Asked,
    ) -> (slopty_worker::screen::Pipeline<Fake<Gated>>, [ScreenEvent; 2], Option<sized::Sized>)
    {
        let sink: Arc<dyn slopty_worker::DatagramSink> = Arc::new(Nowhere);
        sized::open::<Fake<Gated>, Instantly>(displays, StreamId(21), asked, sink, |_event| {})
            .await
            .unwrap()
    }

    fn told(event: &ScreenEvent) -> VirtualDisplay {
        match event {
            ScreenEvent::Display { key, display, .. } if *key == KEY => *display,
            other => panic!("not the display event: {other:?}"),
        }
    }

    async fn until(what: &str, mut done: impl FnMut() -> bool) {
        let started = std::time::Instant::now();
        while !done() {
            assert!(started.elapsed() < Duration::from_secs(5), "{what}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// The display made for the client is streamed, said so before `Opened`, and released once
    /// the stream is closed and its sizing dropped.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_made_display_is_streamed_and_released_with_the_stream() {
        *LISTED.lock() = vec![1, 101];
        let factory = Instantly { max: 8192, ..Instantly::default() };
        let alive = Arc::clone(&factory.alive);
        let displays = displays(factory);
        let (stream, [display, opened], sized) = open(Some(&displays), asked(2560, 1600)).await;
        assert_eq!(told(&display), VirtualDisplay::Made(DisplayId(101)));
        let made = CaptureTarget::Display(DisplayId(101));
        assert!(matches!(opened, ScreenEvent::Opened { target, .. } if target == made));
        assert_eq!(stream.target(), made);
        assert_eq!(*alive.lock(), [101]);
        stream.close().await;
        drop(sized);
        until("the display outlived its stream", || alive.lock().is_empty()).await;
    }

    /// A worker that makes no display streams a physical one and says why, typed.
    #[tokio::test(flavor = "multi_thread")]
    async fn without_displays_a_physical_display_is_streamed_and_the_client_told_why() {
        *LISTED.lock() = vec![1];
        let (stream, [display, _opened], sized) = open(None, asked(1920, 1080)).await;
        let physical =
            VirtualDisplay::Physical { display: DisplayId(1), why: NoVirtualDisplay::Unavailable };
        assert_eq!(told(&display), physical);
        assert_eq!(stream.target(), CaptureTarget::Display(DisplayId(1)));
        assert!(sized.is_none());
        stream.close().await;
    }

    /// A display ScreenCaptureKit never lists is given up on and released; the physical one is
    /// streamed.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_display_never_listed_falls_back_and_is_released() {
        *LISTED.lock() = vec![1];
        let factory = Instantly { max: 8192, ..Instantly::default() };
        let alive = Arc::clone(&factory.alive);
        let displays = displays(factory);
        let (stream, [display, _opened], sized) = open(Some(&displays), asked(1920, 1080)).await;
        let physical =
            VirtualDisplay::Physical { display: DisplayId(1), why: NoVirtualDisplay::Unlisted };
        assert_eq!(told(&display), physical);
        assert!(sized.is_none());
        until("the unlisted display was kept", || alive.lock().is_empty()).await;
        stream.close().await;
    }

    /// A resize the display outgrows makes it anew: the stream switches to the new display and
    /// tells the client which, and the old display is gone.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_outgrown_display_is_remade_and_the_stream_follows_it() {
        *LISTED.lock() = vec![1, 101, 102];
        let factory = Instantly { max: 3200, ..Instantly::default() };
        let alive = Arc::clone(&factory.alive);
        let displays = displays(factory);
        let (mut stream, _events, sized) = open(Some(&displays), asked(2560, 1600)).await;
        let mut sized = sized.unwrap();
        let (commands_tx, mut commands) = mpsc::unbounded_channel();
        let (out, mut events) = mpsc::channel(64);
        let task = tokio::spawn(async move {
            serve(
                &mut stream,
                ClientId::new(),
                &mut commands,
                &out,
                Some(&mut sized),
                &unsourced(),
            )
            .await;
            let target = stream.target();
            stream.close().await;
            drop(sized);
            target
        });
        commands_tx.send(Command::Resize { width: 6016, height: 3384, scale: None }).unwrap();
        let switched = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match events.recv().await {
                    Some(WorkerMsg::Screen(event @ ScreenEvent::Display { .. })) => break event,
                    Some(_) => {}
                    None => panic!("the stream ended"),
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(told(&switched), VirtualDisplay::Made(DisplayId(102)));
        assert_eq!(*alive.lock(), [102], "the outgrown display went first");
        drop(commands_tx);
        let target = task.await.unwrap();
        assert_eq!(target, CaptureTarget::Display(DisplayId(102)));
        until("the remade display outlived its stream", || alive.lock().is_empty()).await;
    }
}

/// A stream's keyboard input source: the ask queued in order, the input after it held until
/// the switch is heard, and a client told when another client's ask took its source.
#[cfg(test)]
#[cfg(target_vendor = "apple")]
mod sourcing {
    use std::sync::Arc;
    use std::time::Duration;

    use slopty_core::{ClientId, StreamId};
    use slopty_input::sources::Sources;
    use slopty_net::WorkerMsg;
    use slopty_proto::input::{KeyAction, KeyCode, Mods};
    use slopty_proto::screen::{CaptureTarget, Quality, ScreenEvent, ScreenInput};
    use slopty_worker::screen::Pipeline;
    use tokio::sync::mpsc;

    use super::fake::{Fake, Nowhere, Plain, Queued, Selectable, note};
    use super::{Command, serve};

    const US: &str = "com.apple.keylayout.US";
    const FRENCH: &str = "com.apple.keylayout.French";
    const TELEX: &str = "com.apple.inputmethod.VietnameseIM.VietnameseSimpleTelex";

    /// A stream served for `client` as `stream` over display `display`: its commands, what it
    /// tells the client, and what its input sink queued. It claims as the stream's task does, and
    /// lets go as it ends.
    struct Served {
        commands: mpsc::UnboundedSender<Command>,
        events: mpsc::Receiver<WorkerMsg>,
        queued: mpsc::UnboundedReceiver<Queued>,
        task: tokio::task::JoinHandle<()>,
    }

    async fn served(display: u32, client: ClientId, stream: StreamId, sources: &Sources) -> Served {
        let queued = note(display);
        let target = CaptureTarget::Display(slopty_core::DisplayId(display));
        let sink: Arc<dyn slopty_worker::DatagramSink> = Arc::new(Nowhere);
        let (mut pipeline, _opened) =
            Pipeline::<Fake<Plain>>::open(stream, target, Quality::default(), sink, |_event| {})
                .await
                .unwrap();
        let (commands, mut commanded) = mpsc::unbounded_channel();
        let (out, events) = mpsc::channel(64);
        let claim = sources.claimant();
        let task = tokio::spawn(async move {
            serve(&mut pipeline, client, &mut commanded, &out, None, &claim).await;
            drop(claim);
            pipeline.close().await;
        });
        Served { commands, events, queued, task }
    }

    fn ask(source: &str) -> Command {
        Command::Input(ScreenInput::KeyboardSource { source: source.to_owned() })
    }

    fn key() -> Command {
        Command::Input(ScreenInput::Key {
            code: KeyCode::A,
            action: KeyAction::Press,
            mods: Mods::empty(),
        })
    }

    /// The next input-source answer the stream tells its client.
    async fn told(events: &mut mpsc::Receiver<WorkerMsg>) -> (String, bool) {
        let wait = async {
            loop {
                if let Some(WorkerMsg::Screen(ScreenEvent::KeyboardSource {
                    source,
                    applied,
                    ..
                })) = events.recv().await
                {
                    break (source, applied);
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(5), wait).await.unwrap()
    }

    fn sources_on(current: &str) -> (Sources, Selectable) {
        let tis = Selectable::default();
        *tis.0.lock() = Some(current.to_owned());
        (Sources::new(tis.clone()), tis)
    }

    fn moved() -> Command {
        Command::Input(ScreenInput::Move { x: 1.0, y: 2.0 })
    }

    async fn next(queued: &mut mpsc::UnboundedReceiver<Queued>) -> Queued {
        tokio::time::timeout(Duration::from_secs(5), queued.recv()).await.unwrap().unwrap()
    }

    /// A key typed after the ask for a new source waits for the switch to be heard, so the
    /// worker reads it under that source; then it goes, after the answer. The order is what is
    /// checked, not a time: the key reaches the sink after the switch was reported.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_key_after_a_switch_waits_for_it_to_be_heard() {
        let (sources, _tis) = sources_on(US);
        sources.hearing();
        let mut s = served(31, ClientId::new(), StreamId(1), &sources).await;
        s.commands.send(ask(FRENCH)).unwrap();
        s.commands.send(key()).unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        let heard_at = std::time::Instant::now();
        sources.heard(Some(FRENCH.to_owned()));
        assert_eq!(told(&mut s.events).await, (FRENCH.to_owned(), true));
        let queued = next(&mut s.queued).await;
        assert!(matches!(queued.input, ScreenInput::Key { code: KeyCode::A, .. }));
        assert!(queued.at >= heard_at, "held until the switch was heard");
        drop(s.commands);
        s.task.await.unwrap();
    }

    /// The pointer is not held behind a switch: no source changes what it does. Once a key is
    /// held, the pointer after it waits behind it, so the worker sees them in the order sent.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_pointer_waits_only_behind_a_held_key() {
        let (sources, _tis) = sources_on(US);
        sources.hearing();
        let mut s = served(35, ClientId::new(), StreamId(1), &sources).await;
        s.commands.send(ask(FRENCH)).unwrap();
        s.commands.send(moved()).unwrap();
        let first = next(&mut s.queued).await;
        assert!(matches!(first.input, ScreenInput::Move { .. }), "the pointer went unheld");
        s.commands.send(key()).unwrap();
        s.commands.send(moved()).unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        let heard_at = std::time::Instant::now();
        sources.heard(Some(FRENCH.to_owned()));
        let key = next(&mut s.queued).await;
        let after = next(&mut s.queued).await;
        assert!(matches!(key.input, ScreenInput::Key { .. }) && key.at >= heard_at);
        assert!(matches!(after.input, ScreenInput::Move { .. }), "behind the key, in order");
        drop(s.commands);
        s.task.await.unwrap();
    }

    /// A client that reconnects opens its stream again as `StreamId(1)` before the old stream
    /// has ended; the old stream ending leaves the new one's claim, and the source, in place.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_reconnected_streams_source_outlives_the_old_stream() {
        let (sources, tis) = sources_on(US);
        let client = ClientId::new();
        let mut old = served(36, client, StreamId(1), &sources).await;
        old.commands.send(ask(FRENCH)).unwrap();
        assert_eq!(told(&mut old.events).await, (FRENCH.to_owned(), true));
        let mut new = served(37, client, StreamId(1), &sources).await;
        new.commands.send(ask(FRENCH)).unwrap();
        assert_eq!(told(&mut new.events).await, (FRENCH.to_owned(), true));
        drop(old.commands);
        old.task.await.unwrap();
        assert_eq!(tis.0.lock().as_deref(), Some(FRENCH), "the new stream still asks");
        drop(new.commands);
        new.task.await.unwrap();
        assert_eq!(tis.0.lock().as_deref(), Some(US));
    }

    /// A tile idle long enough lets its claim go: the worker's own source is back while the
    /// stream goes on, a key after it is not held, and asking again claims anew.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_released_keyboard_gives_the_source_back() {
        let (sources, tis) = sources_on(US);
        let mut s = served(38, ClientId::new(), StreamId(1), &sources).await;
        s.commands.send(ask(FRENCH)).unwrap();
        assert_eq!(told(&mut s.events).await, (FRENCH.to_owned(), true));
        s.commands.send(Command::Input(ScreenInput::KeyboardReleased)).unwrap();
        s.commands.send(key()).unwrap();
        assert!(matches!(next(&mut s.queued).await.input, ScreenInput::Key { .. }));
        assert_eq!(tis.0.lock().as_deref(), Some(US), "the worker's own is back");
        s.commands.send(ask(FRENCH)).unwrap();
        assert_eq!(told(&mut s.events).await, (FRENCH.to_owned(), true));
        assert_eq!(tis.0.lock().as_deref(), Some(FRENCH));
        drop(s.commands);
        s.task.await.unwrap();
    }

    /// Asking for the source the worker is under holds nothing: the answer and the key go at
    /// once.
    #[tokio::test(flavor = "multi_thread")]
    async fn asking_for_the_current_source_holds_nothing() {
        let (sources, _tis) = sources_on(US);
        sources.hearing();
        let mut s = served(32, ClientId::new(), StreamId(1), &sources).await;
        let sent = std::time::Instant::now();
        s.commands.send(ask(US)).unwrap();
        s.commands.send(key()).unwrap();
        assert!(next(&mut s.queued).await.at.duration_since(sent) < super::HOLD_MOST / 2);
        assert_eq!(told(&mut s.events).await, (US.to_owned(), true));
        drop(s.commands);
        s.task.await.unwrap();
    }

    /// Two clients' first streams, both `StreamId(1)`: the second client's ask takes the
    /// source, and the first client is told it lost it; once the second leaves, the first's
    /// comes back and it is told so.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_client_whose_source_another_took_is_told() {
        let (sources, tis) = sources_on(US);
        let mut a = served(33, ClientId::new(), StreamId(1), &sources).await;
        let mut b = served(34, ClientId::new(), StreamId(1), &sources).await;
        a.commands.send(ask(TELEX)).unwrap();
        assert_eq!(told(&mut a.events).await, (TELEX.to_owned(), true));
        b.commands.send(ask(FRENCH)).unwrap();
        assert_eq!(told(&mut b.events).await, (FRENCH.to_owned(), true));
        assert_eq!(told(&mut a.events).await, (TELEX.to_owned(), false), "taken by B");
        b.commands.send(Command::Close).unwrap();
        b.task.await.unwrap();
        assert_eq!(tis.0.lock().as_deref(), Some(TELEX), "A's claim held through B's");
        assert_eq!(told(&mut a.events).await, (TELEX.to_owned(), true), "A's is back");
        drop(a.commands);
        a.task.await.unwrap();
        assert_eq!(tis.0.lock().as_deref(), Some(US), "none asks: the worker's own");
    }
}
