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
use slopty_proto::drag::{DragEvent, DragInput};
use slopty_proto::screen::{CaptureTarget, OpenAsk, Quality, ScreenEvent, ScreenInput, TextField};
use slopty_worker::platform::{Native, Platform};
use slopty_worker::screen::drag::Heard;
use slopty_worker::screen::sound::{Listen, Sound};
#[cfg(target_os = "macos")]
use slopty_worker::screen::synthetic::Synthetic;
use slopty_worker::screen::{
    Pipeline, Rebuild, Refused, ScreenError, StreamControl, StreamEvent, sized,
};
use tokio::sync::mpsc;

use crate::Daemon;
use crate::dnd::{Carrying, Dnd, OutNext, Outward};

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

/// How long after the client's typing, click or focus the stream reads which text field has
/// the keyboard ([`ScreenEvent::Field`]): long enough for the app to have moved its caret for
/// the last of a burst of keys, so a burst costs one read.
const FIELD_AFTER: std::time::Duration = std::time::Duration::from_millis(60);

/// Whether `command` may move the worker's caret or its keyboard focus: a key, text, a click's
/// release, the tile taking the keyboard.
const fn moves_field(command: &Command) -> bool {
    matches!(
        command,
        Command::Focus
            | Command::Input(
                ScreenInput::Key { .. }
                    | ScreenInput::PasteChord { .. }
                    | ScreenInput::Text { .. }
                    | ScreenInput::Button { down: false, .. }
            )
    )
}

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
    Focused(bool),
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
    /// The client's one sound from this worker, which the stream listens through.
    pub sound: Arc<dyn Listen>,
}

/// The client's one sound from this worker, sent on `conn`, on the platform its streams are
/// drawn or captured on ([`slopty_worker::screen::sound`]).
pub fn sound(conn: &Connection) -> Arc<dyn Listen> {
    let sink = Arc::new(QuicSink(conn.clone()));
    #[cfg(target_os = "macos")]
    if slopty_worker::screen::synthetic_screen() {
        return drawn_sound(sink, slopty_worker::screen::synthetic::sounding());
    }
    Arc::new(Sound::<Native>::new(sink))
}

/// The drawn screen's sound into `sink`: its tone only when a test asks for it (`sounding`,
/// `SLOPTY_SYNTHETIC_SOUND`), else nothing, so no client of it opens a player and tests make
/// no sound on this Mac.
#[cfg(target_os = "macos")]
fn drawn_sound(sink: Arc<dyn slopty_worker::DatagramSink>, sounding: bool) -> Arc<dyn Listen> {
    if sounding {
        Arc::new(Sound::<Synthetic>::new(sink))
    } else {
        Arc::new(slopty_worker::screen::sound::Silent::default())
    }
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
    let shield = shield(&link);
    let opened = Pipeline::<P>::open(id, target, quality, sink, on_event).await;
    let opened = opened.map(|(stream, opened)| (stream, vec![opened], None));
    serve_opened(link, id, OpenAsk::Target(target), opened, shield, commands).await;
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
    let made = OpenAsk::Made(asked.key);
    let shield = shield(&link);
    #[cfg(target_os = "macos")]
    if slopty_worker::screen::synthetic_screen() {
        let opened = sized::open::<Synthetic, sized::Cg>(None, id, asked, sink, on_event).await;
        let opened = opened.map(|(stream, told, sized)| (stream, told.into(), sized));
        return serve_opened(link, id, made, opened, shield, commands).await;
    }
    let displays = link.daemon.displays.as_ref();
    let opened = sized::open::<Native, sized::Cg>(displays, id, asked, sink, on_event).await;
    let opened = opened.map(|(stream, told, sized)| (stream, told.into(), sized));
    serve_opened(link, id, made, opened, shield, commands).await;
}

/// What says the curtain's shield moved, taken before a stream resolves its filter: a shield
/// raised while the stream opens is then heard, and the stream takes its filter again, rather
/// than keep one made before the shield was listed.
fn shield(link: &Link) -> Option<tokio::sync::watch::Receiver<u64>> {
    link.daemon.curtain.as_ref().map(slopty_worker::screen::curtain::Curtain::shield_moved)
}

/// What a stream tells its client outside its commands' answers. A cursor lost to a full link
/// is drawn over by the next; a stream's end is never lost, or the client would wait on a
/// stream that is gone: one that finds the link full is sent on the daemon's runtime, which
/// waits for room.
fn on_event(id: StreamId, events: mpsc::Sender<WorkerMsg>) -> impl Fn(StreamEvent) + Send + Sync {
    let runtime = tokio::runtime::Handle::try_current().ok();
    move |e: StreamEvent| match e {
        StreamEvent::Stopped(e) => {
            let closed = ScreenEvent::Closed { stream: id, reason: e.to_string() };
            match events.try_send(WorkerMsg::Screen(closed)) {
                Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => {}
                Err(mpsc::error::TrySendError::Full(msg)) => {
                    let Some(runtime) = &runtime else { return };
                    let events = events.clone();
                    drop(runtime.spawn(async move {
                        let _gone = events.send(msg).await;
                    }));
                }
            }
        }
        StreamEvent::Cursor(shape) => {
            let cursor = ScreenEvent::Cursor { stream: id, shape: Some(shape) };
            let _full = events.try_send(WorkerMsg::Screen(cursor));
        }
    }
}

/// A stream as it opened: the stream, what to tell the client first, and for a display made
/// for the client what serves its resizes.
type Opened<P> = Result<(Pipeline<P>, Vec<ScreenEvent>, Option<sized::Sized>), ScreenError>;

/// Tell the client how the stream opened, serve it until it is closed or the connection lets
/// go of it, and tear it down: input released, capture stopped, then any display made for it
/// released on the main thread. One that did not open is told as what was `asked` and why,
/// since the client has not heard of `id`.
async fn serve_opened<P: Platform>(
    link: Link,
    id: StreamId,
    asked: OpenAsk,
    opened: Opened<P>,
    shield: Option<tokio::sync::watch::Receiver<u64>>,
    mut commands: mpsc::UnboundedReceiver<Command>,
) {
    let Link { daemon, client, conn, out, told, sound } = link;
    let (mut stream, mut sized) = match opened {
        Ok((mut stream, events, sized)) => {
            stream.listen(&*sound);
            if let Some(shield) = shield {
                stream.follow_shield(shield);
            }
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
            let event = ScreenEvent::OpenFailed { asked, why: e.failure() };
            let _sent = out.send(WorkerMsg::Screen(event)).await;
            let _told = told.send(Told::Gone(id));
            return;
        }
    };

    // Dropped however the task ends, a panic included, so the claim never outlives the stream.
    let claim = daemon.sources.claimant();
    let dnd = Some(&*daemon.dnd);
    let by_client =
        serve(&mut stream, client, &mut commands, &out, sized.as_mut(), &claim, dnd).await;
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
#[expect(
    clippy::too_many_arguments,
    reason = "the stream, its commands and what it borrows from its connection, each apart"
)]
pub async fn serve<P: Platform>(
    stream: &mut Pipeline<P>,
    client: ClientId,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    out: &mpsc::Sender<WorkerMsg>,
    mut sized: Option<&mut sized::Sized>,
    claim: &Claim,
    dnd: Option<&Dnd>,
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
    // A drag from the client crossing the worker from this stream's tile.
    let mut carrying: Option<Carrying> = None;
    // Drags out of the apps the stream shows, under the client's own press.
    let mut outward = Outward::new(dnd);
    // When the field that has the keyboard is next read, the read under way, and what the
    // client was last told of it (nothing, to begin with).
    let mut field_due: Option<tokio::time::Instant> = None;
    let mut reading: Option<tokio::task::JoinHandle<Option<TextField>>> = None;
    let mut field_told: Option<TextField> = None;
    // The curtain's shield moving, for a display stream: its filter is taken again, so the
    // client sees the session rather than the shield.
    let mut shield = stream.take_shield();
    let by_client = loop {
        tokio::select! {
            command = commands.recv() => match command {
                None => break false,
                Some(Command::Close) => break true,
                Some(Command::Input(ScreenInput::Drag(DragInput::Catch { drag }))) => {
                    let acts = outward.catch(drag);
                    act_out(stream, client, acts, (&mut input_at, &mut next_probe), (&mut outward, dnd), &mut telling);
                }
                Some(Command::Input(ScreenInput::Drag(input))) => {
                    let id = stream.id();
                    let mut tell = |event| telling.push(ScreenEvent::Drag { stream: id, event });
                    if let DragInput::Enter { .. } = input {
                        if let Some(mut was) = carrying.take() {
                            let leave = Heard::Input(DragInput::Leave { drag: was.drag() });
                            let acts = was.hear(leave);
                            was.act(acts, stream, dnd, &mut tell);
                        }
                        match Carrying::enter(dnd, client, input) {
                            Ok((mut entered, acts)) => {
                                if !entered.act(acts, stream, dnd, &mut tell) {
                                    carrying = Some(entered);
                                }
                            }
                            Err(refused) => tell(refused),
                        }
                    } else if let Some(on) = carrying.as_mut().filter(|c| c.drag() == input.drag()) {
                        let acts = on.hear(Heard::Input(input));
                        if on.act(acts, stream, dnd, &mut tell) {
                            carrying = None;
                        }
                    }
                }
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
                    if held.iter().any(moves_field) {
                        field_due = Some(field_after());
                    }
                    for command in std::mem::take(&mut held) {
                        feed(stream, client, command, (&mut input_at, &mut next_probe), (&mut outward, dnd), &mut telling);
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
                    if moves_field(&command) {
                        field_due = Some(field_after());
                    }
                    feed(stream, client, command, (&mut input_at, &mut next_probe), (&mut outward, dnd), &mut telling);
                }
            },
            () = async {
                match field_due {
                    Some(due) => tokio::time::sleep_until(due).await,
                    None => std::future::pending().await,
                }
            }, if field_due.is_some() && reading.is_none() => {
                field_due = None;
                reading = Some(tokio::task::spawn_blocking(stream.field_reader()));
            }
            field = async { reading.as_mut()?.await.ok() }, if reading.is_some() => {
                reading = None;
                let field = field.flatten();
                if field != field_told {
                    field_told = field;
                    telling.push(ScreenEvent::Field { stream: stream.id(), field });
                }
            }
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
                if held.iter().any(moves_field) {
                    field_due = Some(field_after());
                }
                for command in std::mem::take(&mut held) {
                    feed(stream, client, command, (&mut input_at, &mut next_probe), (&mut outward, dnd), &mut telling);
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
                if held.iter().any(moves_field) {
                    field_due = Some(field_after());
                }
                for command in std::mem::take(&mut held) {
                    feed(stream, client, command, (&mut input_at, &mut next_probe), (&mut outward, dnd), &mut telling);
                }
            }
            moved = async { shield.as_mut()?.changed().await.ok() }, if shield.is_some() => {
                if moved.is_none() {
                    shield = None;
                    continue;
                }
                if let Err(e) = stream.refilter().await {
                    tracing::warn!(stream = %stream.id(), error = %e, "the shield left in the picture");
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
            heard = async {
                match carrying.as_mut() {
                    Some(on) => on.next().await,
                    None => std::future::pending().await,
                }
            }, if carrying.is_some() => {
                let id = stream.id();
                if let Some(on) = carrying.as_mut() {
                    let acts = on.hear(heard);
                    let tell = |event| telling.push(ScreenEvent::Drag { stream: id, event });
                    if on.act(acts, stream, dnd, tell) {
                        carrying = None;
                    }
                }
            }
            next = outward.next() => {
                let acts = match next {
                    OutNext::Began(items) => outward.begin(dnd, client, items),
                    OutNext::Heard(heard) => outward.hear(heard),
                };
                act_out(stream, client, acts, (&mut input_at, &mut next_probe), (&mut outward, dnd), &mut telling);
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
    if let Some(read) = reading {
        read.abort();
    }
    // A client gone mid-drag, or its stream closed, leaves: nothing is dropped.
    if let Some(mut on) = carrying {
        let acts = on.hear(Heard::Input(DragInput::Leave { drag: on.drag() }));
        on.act(acts, stream, dnd, |_told| {});
    }
    outward.end(dnd);
    // What is still untold is moot: the stream is ending, and `Closed` says so.
    by_client
}

/// When a field read asked for now is due: [`FIELD_AFTER`] on.
fn field_after() -> tokio::time::Instant {
    let now = tokio::time::Instant::now();
    now.checked_add(FIELD_AFTER).unwrap_or(now)
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
    /// A drag's events each tell a step, except its operation, of which only the last
    /// matters.
    fn push(&mut self, event: ScreenEvent) {
        match &event {
            ScreenEvent::Drag { event: DragEvent::Operation { drag, .. }, .. } => {
                let drag = *drag;
                self.0.retain(|waiting| {
                    !matches!(waiting, ScreenEvent::Drag { event: DragEvent::Operation { drag: d, .. }, .. } if *d == drag)
                });
            }
            ScreenEvent::Drag { .. } => {}
            _ => {
                let kind = std::mem::discriminant(&event);
                self.0.retain(|waiting| std::mem::discriminant(waiting) != kind);
            }
        }
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
/// Whether `command` is read under the worker's input source: a key, the paste chord or text.
const fn typed(command: &Command) -> bool {
    matches!(
        command,
        Command::Input(
            ScreenInput::Key { .. } | ScreenInput::PasteChord { .. } | ScreenInput::Text { .. }
        )
    )
}

/// When input is taken and when the geometry is next probed ([`take_command`]).
type Clocks<'a> = (&'a mut Option<tokio::time::Instant>, &'a mut tokio::time::Instant);

/// The stream's drags out, and the worker's drags they go through.
type Outgoing<'a, 'b> = (&'a mut Outward, Option<&'b Dnd>);

/// Take `command` as [`take_command`] does, its input seen first by the stream's drags out,
/// and then the input a drag out posts as the client's own.
fn feed<P: Platform>(
    stream: &mut Pipeline<P>,
    client: ClientId,
    command: Command,
    clocks: Clocks<'_>,
    (outward, dnd): Outgoing<'_, '_>,
    telling: &mut Telling,
) {
    let (input_at, next_probe) = clocks;
    let mut queue = std::collections::VecDeque::from([command]);
    while let Some(command) = queue.pop_front() {
        if let Command::Input(input) = &command {
            let acts = outward.see(input);
            let id = stream.id();
            let tell = |event| telling.push(ScreenEvent::Drag { stream: id, event });
            let inputs = outward.act(acts, stream, dnd, tell);
            queue.extend(inputs.into_iter().map(Command::Input));
        }
        take_command(stream, client, command, input_at, next_probe);
    }
}

/// Do a drag out's `acts`, and post the input it asks for as the client's own.
fn act_out<P: Platform>(
    stream: &mut Pipeline<P>,
    client: ClientId,
    acts: Vec<slopty_worker::screen::drag::out::OutAct>,
    clocks: Clocks<'_>,
    (outward, dnd): Outgoing<'_, '_>,
    telling: &mut Telling,
) {
    let id = stream.id();
    let tell = |event| telling.push(ScreenEvent::Drag { stream: id, event });
    let inputs = outward.act(acts, stream, dnd, tell);
    let (input_at, next_probe) = clocks;
    for input in inputs {
        let clocks = (&mut *input_at, &mut *next_probe);
        feed(stream, client, Command::Input(input), clocks, (&mut *outward, dnd), telling);
    }
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
        Command::Focused(focused) => stream.set_focused(focused),
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

    /// A stream's end reaches its client through a link that is full at that moment, once
    /// there is room; a cursor that finds it full is dropped, the next drawing over it.
    #[tokio::test]
    async fn a_stream_end_waits_for_room_on_a_full_link() {
        use slopty_proto::screen::ScreenEvent;
        use slopty_worker::screen::StreamEvent;

        let id = slopty_core::StreamId(7);
        let (out, mut client) = tokio::sync::mpsc::channel(1);
        let told = super::on_event(id, out);
        let shape = slopty_proto::screen::CursorShape {
            w: 1,
            h: 1,
            hot_x: 0,
            hot_y: 0,
            bgra: vec![0; 4],
            scale: 1,
        };
        told(StreamEvent::Cursor(shape.clone()));
        told(StreamEvent::Cursor(shape));
        told(StreamEvent::Stopped(slopty_capture::CaptureError::NotFound(
            slopty_proto::screen::CaptureTarget::Display(slopty_core::DisplayId(1)),
        )));
        let first = client.recv().await.expect("the cursor");
        assert!(matches!(first, slopty_net::WorkerMsg::Screen(ScreenEvent::Cursor { .. })));
        let next = tokio::time::timeout(Duration::from_secs(5), client.recv()).await;
        let Ok(Some(slopty_net::WorkerMsg::Screen(ScreenEvent::Closed { stream, .. }))) = next
        else {
            panic!("the stream's end, after the one cursor: {next:?}");
        };
        assert_eq!(stream, id);
    }

    /// The client's end of a sound: how many datagrams came.
    #[cfg(target_os = "macos")]
    #[derive(Default)]
    struct Heard(std::sync::atomic::AtomicUsize);

    #[cfg(target_os = "macos")]
    impl slopty_worker::DatagramSink for Heard {
        fn send(&self, datagrams: &[bytes::Bytes]) -> Result<(), slopty_worker::screen::Refused> {
            self.0.fetch_add(datagrams.len(), std::sync::atomic::Ordering::Relaxed);
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

    /// The drawn screen's worker captures no sound unless a test asks for its tone: a stream
    /// hearing every app sends nothing, so no client opens a player. Asked, the same stream's
    /// tone comes.
    #[cfg(target_os = "macos")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_drawn_screen_makes_no_sound_unless_asked() {
        use std::sync::Arc;
        use std::sync::atomic::Ordering;

        use slopty_capture::Heard as Hears;

        for sounding in [false, true] {
            let client = Arc::new(Heard::default());
            let sink: Arc<dyn slopty_worker::DatagramSink> = Arc::<Heard>::clone(&client);
            let sound = super::drawn_sound(sink, sounding);
            let listening = sound.listen(Hears::Every);
            // Long enough for the tone's first chunks, and for `AudioToolbox` to make the Opus
            // encoder on a loaded machine (`docs/decisions/testing.md`).
            let deadline =
                std::time::Instant::now() + Duration::from_secs(if sounding { 100 } else { 1 });
            while std::time::Instant::now() < deadline && client.0.load(Ordering::Relaxed) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let sent = client.0.load(Ordering::Relaxed);
            assert_eq!(sent > 0, sounding, "sounding {sounding}: {sent} datagrams");
            assert_eq!(sound.clock().packets() > 0, sounding);
            drop(listening);
        }
    }

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
            settings: String::new(),
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
        AudioSink, AxError, CaptureConfig, CaptureError, CaptureSource, CapturedFrame, Crop, Heard,
        Rect, TargetWindow, Went, WindowState,
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
    /// The displays the fake enumeration lists; set through [`listing`].
    static LISTED: Mutex<Vec<u32>> = Mutex::new(Vec::new());
    /// Held by the test whose listing [`LISTED`] holds.
    static LISTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Make the fake enumeration list `displays` for as long as the guard is held. The tests of
    /// one process run at once and share this listing, so each holds it until it ends; a test
    /// that set it and let go read another test's displays.
    pub async fn listing(displays: &[u32]) -> tokio::sync::MutexGuard<'static, ()> {
        let held = LISTING.lock().await;
        *LISTED.lock() = displays.to_vec();
        held
    }
    /// The text field the fake Mac says has the keyboard.
    pub static FIELD: Mutex<Option<slopty_capture::FocusedField>> = Mutex::new(None);

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
        type Sound = ();
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
            _on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
            done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
        ) -> Result<(), CaptureError> {
            done(Ok(()));
            Ok(())
        }

        fn start_sound(
            _heard: &Heard,
            _sink: AudioSink,
            _on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
            done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
        ) {
            done(Ok(()));
        }

        fn hear(
            (): &(),
            _heard: &Heard,
            done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
        ) {
            done(Ok(()));
        }

        fn stop_sound((): &(), done: impl FnOnce(Result<(), CaptureError>) + Send + 'static) {
            done(Ok(()));
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

        fn focused_field() -> Option<slopty_capture::FocusedField> {
            *FIELD.lock()
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

        fn cursor_seed() -> Option<i32> {
            None
        }

        fn cursor_shape() -> Option<CursorShape> {
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

    /// What the sink of display `display` queues from now on. The map is the process's, so each
    /// test notes its own display numbers: a second `note` of one cuts the first one off.
    pub fn note(display: u32) -> mpsc::UnboundedReceiver<Queued> {
        let (tx, rx) = mpsc::unbounded_channel();
        NOTED.lock().insert(display, tx);
        rx
    }

    static DRAGGED: LazyLock<Mutex<HashMap<u32, mpsc::UnboundedSender<&'static str>>>> =
        LazyLock::new(Mutex::default);

    /// The drag steps the sink of display `display` takes from now on, by name.
    pub fn note_drags(display: u32) -> mpsc::UnboundedReceiver<&'static str> {
        let (tx, rx) = mpsc::unbounded_channel();
        DRAGGED.lock().insert(display, tx);
        rx
    }

    /// An input sink that notes each event instead of posting it: the point where the real one
    /// hands it to its thread. A drag's entry is answered with its point as it came.
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

        fn drag(&mut self, step: slopty_input::DragStep) {
            use slopty_input::DragStep;
            let name = match step {
                DragStep::Enter { x, y, answer } => {
                    let _gone = answer.send(Ok((f64::from(x), f64::from(y))));
                    "enter"
                }
                DragStep::Press { .. } => "press",
                DragStep::Move { .. } => "move",
                DragStep::Release => "release",
                DragStep::Cancel => "cancel",
                DragStep::Locate { x, y, answer } => {
                    let _gone = answer.send(Ok((f64::from(x), f64::from(y))));
                    "locate"
                }
            };
            if let Some(tx) = DRAGGED.lock().get(&self.display) {
                let _gone = tx.send(name);
            }
        }

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

    /// A display switch keeps the tile's word on its trackpad gestures: the input sink made
    /// for the new display is told they are sent, as the old one was, so a trackpad scroll
    /// there still comes with its gesture while the tile shows them sent.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_display_switch_keeps_the_gestures_sent() {
        const FROM: u32 = 61;
        const TO: u32 = 62;
        let mut from = note(FROM);
        let mut to = note(TO);
        let sink: Arc<dyn slopty_worker::DatagramSink> = Arc::new(Nowhere);
        let target = CaptureTarget::Display(slopty_core::DisplayId(FROM));
        let (mut stream, _opened) =
            Pipeline::<Fake<Plain>>::open(StreamId(FROM), target, Quality::default(), sink, |_| {})
                .await
                .unwrap();
        stream.inject(&ScreenInput::Gestures { remote: true }).unwrap();
        let told = from.recv().await.expect("the first sink heard it");
        assert_eq!(told.input, ScreenInput::Gestures { remote: true });
        stream.switch_display(slopty_core::DisplayId(TO)).await.unwrap();
        let told = tokio::time::timeout(Duration::from_secs(2), to.recv())
            .await
            .expect("the new sink is told")
            .expect("a sink");
        assert_eq!(told.input, ScreenInput::Gestures { remote: true });
        stream.close().await;
    }

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
            serve(&mut stream, ClientId::new(), &mut commands, &out, None, &unsourced(), None)
                .await;
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
            serve(&mut stream, ClientId::new(), &mut commands, &out, None, &unsourced(), None)
                .await;
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

    use super::fake::{Fake, Gated, Nowhere, listing, unsourced};
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
            .unwrap_or_else(|e| panic!("the sized stream did not open: {e}"))
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
        let _listing = listing(&[1, 101]).await;
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
        let _listing = listing(&[1]).await;
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
        let _listing = listing(&[1]).await;
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
        let _listing = listing(&[1, 101, 102]).await;
        let factory = Instantly { max: 3200, ..Instantly::default() };
        let alive = Arc::clone(&factory.alive);
        let displays = displays(factory);
        let (mut stream, _events, sized) = open(Some(&displays), asked(2560, 1600)).await;
        let mut sized = sized.expect("no display was made: the stream fell back to a physical one");
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
                None,
            )
            .await;
            let target = stream.target();
            stream.close().await;
            drop(sized);
            target
        });
        commands_tx
            .send(Command::Resize { width: 6016, height: 3384, scale: None })
            .expect("the stream ended before the resize");
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
        .expect("no display event within 5 s of the resize");
        assert_eq!(told(&switched), VirtualDisplay::Made(DisplayId(102)));
        assert_eq!(*alive.lock(), [102], "the outgrown display went first");
        drop(commands_tx);
        let target = task.await.expect("the serving task panicked");
        assert_eq!(target, CaptureTarget::Display(DisplayId(102)));
        until("the remade display outlived its stream", || alive.lock().is_empty()).await;
    }
}

/// The text field that has the keyboard, told to the client after its typing and clicks, and
/// only when it changed.
#[cfg(test)]
#[cfg(target_vendor = "apple")]
mod fields {
    use std::sync::Arc;
    use std::time::Duration;

    use slopty_capture::{FocusedField, Rect};
    use slopty_core::{ClientId, DisplayId, StreamId};
    use slopty_input::sources::{Sources, Unsupported};
    use slopty_net::WorkerMsg;
    use slopty_proto::input::{KeyAction, KeyCode, Mods, MouseButton};
    use slopty_proto::screen::{
        CaptureTarget, Caret, Quality, ScreenEvent, ScreenInput, TextField,
    };
    use slopty_worker::screen::Pipeline;
    use tokio::sync::mpsc;

    use super::fake::{FIELD, Fake, Nowhere, Plain};
    use super::{Command, serve};

    /// The next field the stream tells its client.
    async fn told(events: &mut mpsc::Receiver<WorkerMsg>) -> Option<TextField> {
        let wait = async {
            loop {
                if let Some(WorkerMsg::Screen(ScreenEvent::Field { field, .. })) =
                    events.recv().await
                {
                    break field;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(5), wait).await.unwrap()
    }

    #[tokio::test]
    async fn the_field_with_the_keyboard_is_told_after_typing_and_only_as_it_changes() {
        let target = CaptureTarget::Display(DisplayId(41));
        let sink: Arc<dyn slopty_worker::DatagramSink> = Arc::new(Nowhere);
        let (mut pipeline, _opened) =
            Pipeline::<Fake<Plain>>::open(StreamId(1), target, Quality::default(), sink, |_| {})
                .await
                .unwrap();
        let (commands, mut commanded) = mpsc::unbounded_channel();
        let (out, mut events) = mpsc::channel(64);
        let claim = Sources::new(Unsupported).claimant();
        let task = tokio::spawn(async move {
            serve(&mut pipeline, ClientId::new(), &mut commanded, &out, None, &claim, None).await;
            pipeline.close().await;
        });
        let key = |code| {
            Command::Input(ScreenInput::Key { code, action: KeyAction::Press, mods: Mods::empty() })
        };
        // The fake display is 800 × 500 points at the origin.
        let caret = Rect { x: 100.0, y: 50.0, w: 1.0, h: 17.0 };
        *FIELD.lock() = Some(FocusedField { pid: 9, caret: Some(caret), secure: true });
        for code in [KeyCode::A, KeyCode::B, KeyCode::C] {
            commands.send(key(code)).unwrap();
        }
        let first = told(&mut events).await.expect("a field");
        assert!(first.secure, "a password field");
        let at = first.caret.expect("its caret");
        let scale = at.x / 100.0;
        assert!(scale > 0.0, "{at:?}");
        assert_eq!(
            at,
            Caret { x: 100.0 * scale, y: 50.0 * scale, width: scale, height: 17.0 * scale }
        );
        // The same field again tells nothing; a click elsewhere tells the change.
        commands.send(key(KeyCode::D)).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        *FIELD.lock() = None;
        let up = ScreenInput::Button {
            button: MouseButton::Left,
            down: false,
            x: 4.0,
            y: 4.0,
            clicks: 1,
            mods: Mods::empty(),
        };
        commands.send(Command::Input(up)).unwrap();
        assert_eq!(told(&mut events).await, None, "the next told is the change, not a repeat");
        commands.send(Command::Close).unwrap();
        task.await.unwrap();
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
            serve(&mut pipeline, client, &mut commanded, &out, None, &claim, None).await;
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

/// The Mac's lock state from the capture to the client, end to end: the drawn Mac is locked
/// ([`slopty_capture::synthetic::set_console`]), the stream's geometry probe reads it, the
/// source check ranks it over the frames, and the client's link hears it over QUIC.
#[cfg(test)]
#[cfg(target_os = "macos")]
mod console {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use slopty_capture::Console;
    use slopty_capture::synthetic::set_console;
    use slopty_client::{LinkEvent, WorkerLink};
    use slopty_core::{ClientId, StreamId, WorkerId};
    use slopty_net::WorkerMsg;
    use slopty_net::worker::WorkerListener;
    use slopty_proto::handshake::{Hello, HelloAck};
    use slopty_proto::screen::{CaptureTarget, Quality, ScreenEvent, SourceState};
    use slopty_worker::screen::Pipeline;
    use slopty_worker::screen::synthetic::{DISPLAY, Synthetic};
    use tokio::sync::mpsc;

    use super::fake::{Nowhere, unsourced};
    use super::{Command, serve};

    /// The next source state the client hears that `wanted` accepts, and how long it took.
    async fn heard(
        events: &mut mpsc::Receiver<LinkEvent>,
        wanted: impl Fn(SourceState) -> bool,
    ) -> (SourceState, Duration) {
        let from = Instant::now();
        let deadline =
            tokio::time::Instant::now().checked_add(Duration::from_secs(10)).expect("a deadline");
        loop {
            let event = tokio::time::timeout_at(deadline, events.recv()).await;
            let Ok(Some(event)) = event else { panic!("the client never heard it: {event:?}") };
            if let LinkEvent::Control(WorkerMsg::Screen(ScreenEvent::Source { state, .. })) = event
                && wanted(state)
            {
                return (state, from.elapsed());
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_locked_mac_reaches_the_client_and_so_does_its_return() {
        let listener = WorkerListener::bind(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            slopty_net::admission::Admission::default(),
        )
        .unwrap();
        let addr = listener.local_addr().unwrap();
        let endpoint = slopty_net::client::bind_client().unwrap();
        let hello = Hello { client: ClientId::new(), name: "console".to_owned() };
        let dialing = endpoint.clone();
        let dialed =
            tokio::spawn(
                async move { slopty_net::client::connect_addr(&dialing, addr, hello).await },
            );
        let mut accepted = listener.accept().await.expect("the client connects");
        let ack = HelloAck {
            settings: String::new(),
            worker: WorkerId::new(),
            name: "console".to_owned(),
            home: String::new(),
            caps: slopty_proto::server::WorkerCaps::bare(slopty_proto::server::Os::MacOs),
            load: 0.0,
            sessions: Vec::new(),
        };
        accepted.tx.send(&WorkerMsg::HelloAck(ack)).await.unwrap();
        let mut link = WorkerLink::start(dialed.await.unwrap().unwrap());
        let mut events = link.events().expect("the link's events");

        let sink: Arc<dyn slopty_worker::DatagramSink> = Arc::new(Nowhere);
        let target = CaptureTarget::Display(DISPLAY.id);
        let (mut stream, _opened) =
            Pipeline::<Synthetic>::open(StreamId(1), target, Quality::default(), sink, |_| {})
                .await
                .unwrap();
        let (commands, mut commanded) = mpsc::unbounded_channel::<Command>();
        let (out, mut told) = mpsc::channel::<WorkerMsg>(64);
        // The connection's writer: what the stream tells goes out on the control stream.
        let mut tx = accepted.tx;
        let writer = tokio::spawn(async move {
            while let Some(msg) = told.recv().await {
                if tx.send(&msg).await.is_err() {
                    break;
                }
            }
        });
        let serving = tokio::spawn(async move {
            serve(&mut stream, ClientId::new(), &mut commanded, &out, None, &unsourced(), None)
                .await;
            stream.close().await;
        });

        // A loaded machine may encode the first frame past the idle grace, and say Idle first.
        let (drawing, _) = heard(&mut events, |s| s != SourceState::Idle).await;
        assert_eq!(drawing, SourceState::Live, "the drawn Mac draws");
        set_console(Console::Locked);
        let (locked, took) = heard(&mut events, |s| s != SourceState::Live).await;
        assert_eq!(locked, SourceState::Locked, "the frames still come, and it is locked");
        eprintln!("locked → client: {:.0} ms", took.as_secs_f64() * 1e3);
        set_console(Console::Away);
        assert_eq!(heard(&mut events, |_| true).await.0, SourceState::Away);
        set_console(Console::Shown);
        assert_eq!(heard(&mut events, |_| true).await.0, SourceState::Live, "back, drawing");

        drop(commands);
        serving.await.unwrap();
        writer.abort();
        link.close();
        endpoint.close(0_u32.into(), b"done");
    }
}

/// A drop from the client carried through a stream's task: the drag helper is a channel, the
/// input sink answers where the point is, and the client is what the task tells. Over the
/// Apple-only [`fake`] platform, as the other stream tests are.
#[cfg(test)]
#[cfg(target_vendor = "apple")]
mod dragging {
    use std::sync::Arc;
    use std::time::Duration;

    use slopty_core::{ClientId, StreamId};
    use slopty_net::WorkerMsg;
    use slopty_proto::dnd::{FromHelper, ToHelper};
    use slopty_proto::drag::{DragEvent, DragId, DragInput, DragOp, DragOps};
    use slopty_proto::screen::{CaptureTarget, Quality, ScreenEvent, ScreenInput};
    use slopty_worker::screen::Pipeline;
    use slopty_worker::screen::drag::{BUSY, Heard};
    use tokio::sync::mpsc;

    use super::fake::{Fake, Nowhere, Plain, note, note_drags, unsourced};
    use super::{Command, serve};
    use crate::dnd::Dnd;

    async fn within<T>(what: impl Future<Output = Option<T>>) -> T {
        tokio::time::timeout(Duration::from_secs(5), what).await.expect("in time").expect("one")
    }

    /// The next drag event the client is told.
    async fn told(events: &mut mpsc::Receiver<WorkerMsg>) -> DragEvent {
        within(async {
            loop {
                if let WorkerMsg::Screen(ScreenEvent::Drag { event, .. }) = events.recv().await? {
                    return Some(event);
                }
            }
        })
        .await
    }

    fn drag(input: DragInput) -> Command {
        Command::Input(ScreenInput::Drag(input))
    }

    /// What waits to be told keeps every step of a drag, its end above all, and only the last
    /// of its operations; another kind of event waiting goes as before.
    #[test]
    fn a_drags_end_is_never_coalesced_away() {
        let (one, two) = (DragId::new(), DragId::new());
        let ev = |event| ScreenEvent::Drag { stream: StreamId(1), event };
        let op = |drag, op| ev(DragEvent::Operation { drag, op });
        let mut telling = super::Telling::default();
        telling.push(op(one, DragOp::Copy));
        telling.push(op(two, DragOp::Copy));
        telling.push(op(one, DragOp::None));
        telling.push(ev(DragEvent::Ended { drag: one, op: DragOp::None, error: None }));
        telling.push(ScreenEvent::Cursor { stream: StreamId(1), shape: None });
        assert_eq!(
            telling.0.iter().cloned().collect::<Vec<_>>(),
            [
                op(two, DragOp::Copy),
                op(one, DragOp::None),
                ev(DragEvent::Ended { drag: one, op: DragOp::None, error: None }),
                ScreenEvent::Cursor { stream: StreamId(1), shape: None },
            ]
        );
    }

    /// The client's drag enters, the helper's source goes to the point, and the session it
    /// begins carries the target's answer back to the client; the drop with nothing still to
    /// come lets go at once, and the helper's end is the client's. A second drag meanwhile is
    /// refused: the worker has one pointer.
    #[tokio::test]
    async fn a_drop_is_carried_from_entry_to_end_and_a_second_waits_its_turn() {
        let _queued = note(71);
        let mut steps = note_drags(71);
        let target = CaptureTarget::Display(slopty_core::DisplayId(71));
        let sink: Arc<dyn slopty_worker::DatagramSink> = Arc::new(Nowhere);
        let (mut pipeline, _opened) =
            Pipeline::<Fake<Plain>>::open(StreamId(71), target, Quality::default(), sink, |_e| {})
                .await
                .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let transfers = Arc::new(slopty_worker::xfer::Transfers::new(dir.path().to_path_buf()));
        let (to_helper, mut helper) = mpsc::unbounded_channel();
        let dnd = Arc::new(Dnd::with_helper(transfers, to_helper));
        let (commands, mut commanded) = mpsc::unbounded_channel();
        let (out, mut events) = mpsc::channel(64);
        let serving = Arc::clone(&dnd);
        let task = tokio::spawn(async move {
            let claim = unsourced();
            serve(
                &mut pipeline,
                ClientId::new(),
                &mut commanded,
                &out,
                None,
                &claim,
                Some(&serving),
            )
            .await;
            pipeline.close().await;
        });

        let id = DragId::new();
        let enter = DragInput::Enter {
            drag: id,
            x: 40.0,
            y: 30.0,
            allowed: DragOps::COPY,
            items: Vec::new(),
        };
        commands.send(drag(enter)).unwrap();
        let ToHelper::SourceAt { drag: placed, x, y, .. } = within(helper.recv()).await else {
            panic!("the source goes up first")
        };
        assert_eq!((placed, x, y), (id, 40.0, 30.0), "at the point the input sink mapped");
        let says = |said| dnd.drags().tell(id, Heard::Helper(said));
        assert_eq!(within(steps.recv()).await, "enter");
        says(FromHelper::Ready { drag: id });
        assert_eq!(within(steps.recv()).await, "press", "into the source, once it is up");
        says(FromHelper::Began { drag: id });
        says(FromHelper::Operation { drag: id, op: DragOp::Copy });
        assert_eq!(told(&mut events).await, DragEvent::Operation { drag: id, op: DragOp::Copy });

        let other = DragId::new();
        let second = DragInput::Enter {
            drag: other,
            x: 0.0,
            y: 0.0,
            allowed: DragOps::COPY,
            items: Vec::new(),
        };
        let mut busy_events = {
            let (_queued, target) = (note(72), CaptureTarget::Display(slopty_core::DisplayId(72)));
            let sink: Arc<dyn slopty_worker::DatagramSink> = Arc::new(Nowhere);
            let (mut pipeline, _opened) = Pipeline::<Fake<Plain>>::open(
                StreamId(72),
                target,
                Quality::default(),
                sink,
                |_e| {},
            )
            .await
            .unwrap();
            let (commands, mut commanded) = mpsc::unbounded_channel();
            let (out, events) = mpsc::channel(64);
            let serving = Arc::clone(&dnd);
            drop(tokio::spawn(async move {
                let claim = unsourced();
                serve(
                    &mut pipeline,
                    ClientId::new(),
                    &mut commanded,
                    &out,
                    None,
                    &claim,
                    Some(&serving),
                )
                .await;
                pipeline.close().await;
            }));
            commands.send(drag(second)).unwrap();
            (events, commands)
        };
        let refused = told(&mut busy_events.0).await;
        assert_eq!(
            refused,
            DragEvent::Ended { drag: other, op: DragOp::None, error: Some(BUSY.to_owned()) }
        );

        commands
            .send(drag(DragInput::Drop { drag: id, x: 41.0, y: 30.0, promised: Vec::new() }))
            .unwrap();
        assert_eq!(within(steps.recv()).await, "move", "to the drop's point");
        assert_eq!(within(steps.recv()).await, "release", "nothing to wait for");
        says(FromHelper::Ended { drag: id, op: DragOp::Copy });
        assert_eq!(
            told(&mut events).await,
            DragEvent::Ended { drag: id, op: DragOp::Copy, error: None }
        );
        assert_eq!(within(helper.recv()).await, ToHelper::Stop { drag: id });
        assert_eq!(dnd.drags().live(), None, "the worker is free again");
        commands.send(Command::Close).unwrap();
        task.await.unwrap();
        drop(busy_events);
    }

    /// An app's drag under the client's press is seen on the drag pasteboard once the button
    /// moves held, and told with what it carries; the client's catch puts the helper's catcher
    /// where the pointer is, carries the drag out and back over it, lets go once the catcher
    /// says the drag is over it, and tells the client what was caught, files by their path
    /// here and data past the event's budget kept for its fetch.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_drag_out_is_seen_caught_and_told() {
        use slopty_input::pasteboard::clip_type;
        use slopty_proto::dnd::CaughtData;
        use slopty_proto::input::{Mods, MouseButton};

        let mut queued = note(73);
        let target = CaptureTarget::Display(slopty_core::DisplayId(73));
        let sink: Arc<dyn slopty_worker::DatagramSink> = Arc::new(Nowhere);
        let (mut pipeline, _opened) =
            Pipeline::<Fake<Plain>>::open(StreamId(73), target, Quality::default(), sink, |_e| {})
                .await
                .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let transfers = Arc::new(slopty_worker::xfer::Transfers::new(dir.path().to_path_buf()));
        let (to_helper, mut helper) = mpsc::unbounded_channel();
        let name = format!("com.aislopware.slopty.test.worker.dragout.{}", std::process::id());
        let dnd = Arc::new(Dnd::with_helper(transfers, to_helper).watching(&name));
        let (commands, mut commanded) = mpsc::unbounded_channel();
        let (out, mut events) = mpsc::channel(64);
        let serving = Arc::clone(&dnd);
        let task = tokio::spawn(async move {
            let claim = unsourced();
            serve(
                &mut pipeline,
                ClientId::new(),
                &mut commanded,
                &out,
                None,
                &claim,
                Some(&serving),
            )
            .await;
            pipeline.close().await;
        });
        let next_input = async |queued: &mut mpsc::UnboundedReceiver<super::fake::Queued>| {
            within(queued.recv()).await.input
        };
        let left = |down, x| ScreenInput::Button {
            button: MouseButton::Left,
            down,
            x,
            y: 30.0,
            clicks: 1,
            mods: Mods::empty(),
        };
        commands.send(Command::Input(left(true, 40.0))).unwrap();
        commands.send(Command::Input(ScreenInput::Move { x: 44.0, y: 30.0 })).unwrap();
        assert_eq!(next_input(&mut queued).await, left(true, 40.0));
        assert_eq!(next_input(&mut queued).await, ScreenInput::Move { x: 44.0, y: 30.0 });
        // The app begins its drag: what it carries goes on the drag pasteboard.
        let board = slopty_platform::pasteboard::MacPasteboard::named(&name);
        board.copy(&[("public.utf8-plain-text", b"dragged words")]);
        let DragEvent::OutBegan { drag, items } = told(&mut events).await else {
            panic!("the drag out is told first")
        };
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].reps[0].inline.as_deref(), Some(&b"dragged words"[..]));

        commands.send(Command::Input(ScreenInput::Drag(DragInput::Catch { drag }))).unwrap();
        let ToHelper::CatcherAt { drag: at, x, y, .. } = within(helper.recv()).await else {
            panic!("the catcher goes up")
        };
        assert_eq!((at, x, y), (drag, 44.0, 30.0), "where the pointer last was");
        let says = |said| dnd.drags().tell(drag, Heard::Helper(said));
        says(FromHelper::Ready { drag });
        assert_eq!(next_input(&mut queued).await, ScreenInput::Move { x: 48.0, y: 30.0 });
        assert_eq!(next_input(&mut queued).await, ScreenInput::Move { x: 44.0, y: 30.0 });
        says(FromHelper::Operation { drag, op: DragOp::Copy });
        assert_eq!(next_input(&mut queued).await, left(false, 44.0), "let go over the catcher");

        let file = dir.path().join("kept.txt");
        std::fs::write(&file, b"kept").unwrap();
        let big = vec![1_u8; slopty_proto::transfer::INLINE_CLIP_BYTES + 1];
        let size = big.len() as u64;
        let data = vec![CaughtData {
            item: 1,
            uti: "public.png".to_owned(),
            bytes: Some(big.clone()),
            size,
        }];
        let files = vec![file.to_string_lossy().into_owned()];
        says(FromHelper::Caught { drag, files, data, promises: 0 });
        let DragEvent::OutCaught { drag: caught, items } = told(&mut events).await else {
            panic!("what was caught")
        };
        assert_eq!(caught, drag);
        let path = items[0].file.as_ref().and_then(|f| f.path.clone());
        assert_eq!(path, Some(file.to_string_lossy().into_owned()), "by its path here");
        let png = clip_type("public.png");
        assert_eq!(items[1].reps[0].inline, None, "past the budget: fetched");
        assert_eq!(dnd.drags().kept(drag, 1, &png).as_deref(), Some(&*big));
        assert_eq!(within(helper.recv()).await, ToHelper::Stop { drag });
        assert_eq!(dnd.drags().live(), None, "the worker is free again");
        board.release();
        commands.send(Command::Close).unwrap();
        task.await.unwrap();
    }
}
