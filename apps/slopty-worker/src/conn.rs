//! One client connection.
//!
//! The loop here only routes, and never awaits. A terminal's keys, a window's input and the
//! client's loss feedback are handled where they arrive; everything that waits on something
//! slow runs on a task of its own and reports back through [`Done`]: a window stream, the disk,
//! ptyd, the pasteboard, a session stream waiting for the client to allow it, the encoder's
//! rate changes, and the worker's events going to a client that reads slowly. What one request
//! costs therefore never lands on another terminal's echo (MEASUREMENTS.md, "echo behind slow
//! requests" and "nothing on the connection waits behind anything slow").

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use slopty_core::{ClientId, SessionId, StreamId, XferId};
use slopty_net::worker::AcceptedClient;
use slopty_net::{ClientMsg, Connection, NetError, WorkerMsg};
use slopty_proto::PROTOCOL_VERSION;
use slopty_proto::datagram::ClientDatagram;
use slopty_proto::handshake::{Caps, HelloAck};
use slopty_proto::input::{KeyAction, KeyCode, Mods};
use slopty_proto::items::ItemSync;
use slopty_proto::screen::{Feedback, ReceiverReport, ScreenEvent, ScreenInput, ScreenRequest};
use slopty_proto::terminal::{CloseReason, SessionState, TermEvent, TermRequest, TermSize};
use slopty_proto::transfer::{ClipMsg, Dest, XferMsg};
use slopty_worker::WorkerError;
use slopty_worker::clip::Paste;
use slopty_worker::screen::{StreamControl, listing};
use slopty_worker::session::{ClientSink, Outbound, SessionHandle};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};

use crate::Daemon;
use crate::screens::{Command, Told};

/// The weak end of a session's sink, as the connection keeps it.
type WeakClientSink = mpsc::WeakSender<Outbound>;

/// Events buffered per attached session. At most two of them are frames (`session::Outbound`'s
/// credit); an event that does not fit waits in the session actor.
const SINK_DEPTH: usize = 256;
/// Outbound control messages buffered before the writer applies backpressure.
const CONTROL_DEPTH: usize = 1024;
/// Loss feedback datagrams buffered between the reader and the peer loop.
const FEEDBACK_DEPTH: usize = 256;
/// Input copies buffered between the reader and the peer loop. One that finds the queue full
/// is dropped: its stream copy comes regardless.
const COPY_DEPTH: usize = 256;
/// Clipboard representations the client sent up as streams, queued for the peer loop.
const CLIP_DEPTH: usize = 8;
/// Receiver reports waiting for the task that applies them. Reports come a few times a second
/// per stream and each one stands alone, so one that finds the queue full is dropped.
const REPORT_DEPTH: usize = 64;
/// How long an upload into a terminal's directory waits to learn the directory before it
/// lands in the drop directory instead.
const CWD_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
/// How long a paste chord waits for the client's clipboard to arrive before it goes to the
/// window anyway (and pastes what the worker had).
const PASTE_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// Whether `input` is ⌘V, the chord a paste into a streamed window is.
fn is_paste_chord(input: &ScreenInput) -> bool {
    matches!(
        input,
        ScreenInput::Key { code: KeyCode::V, action: KeyAction::Press, mods, .. }
            if mods.contains(Mods::SUPER) && !mods.intersects(Mods::CTRL | Mods::ALT)
    )
}

/// Input for the windows a paste went to, held back in order while the client's clipboard is
/// put on the pasteboard. Every other window, and every terminal, carries on.
struct Held {
    streams: HashSet<StreamId>,
    inputs: Vec<(StreamId, ScreenInput)>,
    stage: Stage,
}

/// Where a held paste is.
enum Stage {
    /// Asking the clipboard what the paste needs (it writes at once when the offer is whole).
    Deciding,
    /// Waiting for the client's representations, until the chord goes on regardless.
    Fetching { until: tokio::time::Instant },
    /// Writing them to the pasteboard.
    Writing,
}

/// Where one window input goes while pastes may be holding some back.
#[derive(Debug, PartialEq)]
enum Route {
    /// To its window now.
    Now(ScreenInput),
    /// Behind the paste its window is waiting on.
    Held,
    /// A paste chord that starts a hold: the clipboard is to be asked what it needs.
    Began,
}

/// Route one input for `stream`. A paste chord holds its window, and joins a hold already under
/// way; input for a held window queues behind the chord, in order; anything else goes now.
fn route(held: &mut Option<Held>, stream: StreamId, input: ScreenInput) -> Route {
    let chord = is_paste_chord(&input);
    match held {
        Some(held) if chord || held.streams.contains(&stream) => {
            held.streams.insert(stream);
            held.inputs.push((stream, input));
            Route::Held
        }
        None if chord => {
            *held = Some(Held {
                streams: HashSet::from([stream]),
                inputs: vec![(stream, input)],
                stage: Stage::Deciding,
            });
            Route::Began
        }
        Some(_) | None => Route::Now(input),
    }
}

/// What a task the loop started reports back to it.
enum Done {
    /// A session this client opened asked to be attached at once.
    Attach { session: SessionId, size: TermSize },
    /// What a paste needs, or `None` when the clipboard could not be asked.
    PastePlan(Option<Paste>),
    /// The client's clipboard is on the pasteboard.
    PasteWritten,
    /// A session ended. Its pump is let go of rather than aborted: it sends what the actor left
    /// in the sink, then finishes its stream.
    SessionClosed(SessionId),
    /// The worker's events stopped reaching the client, and why: the connection is done.
    Ended(&'static str),
}

/// A receiver report for the task that applies it.
struct Asked {
    stream: StreamId,
    control: StreamControl,
    report: ReceiverReport,
}

/// What the client has been told of the sessions and the item registry, so an event it already
/// has (in the `HelloAck`, the first snapshot or a resync) is not sent to it twice.
#[derive(Debug, Default)]
struct Heard {
    /// Each session told of, in the state it was told in: a summary in another state (the
    /// program exited) is news.
    sessions: HashMap<SessionId, SessionState>,
    /// The item registry version of the last snapshot sent: deltas up to it are in it.
    items_floor: u64,
}

impl Heard {
    /// Whether `msg` still tells the client something; what it tells is noted either way.
    fn admit(&mut self, msg: &WorkerMsg) -> bool {
        match msg {
            WorkerMsg::SessionOpened(summary) => {
                self.sessions.insert(summary.id, summary.state) != Some(summary.state)
            }
            WorkerMsg::SessionClosed { session, .. } => {
                self.sessions.remove(session);
                true
            }
            WorkerMsg::Items(ItemSync::Delta { version, .. }) => *version > self.items_floor,
            WorkerMsg::Items(ItemSync::Snapshot { version, .. }) => {
                self.items_floor = self.items_floor.max(*version);
                true
            }
            _ => true,
        }
    }
}

/// One screen stream as the loop sees it: where its commands go, and once it is open, where
/// feedback goes.
struct Screen {
    commands: mpsc::UnboundedSender<Command>,
    control: Option<StreamControl>,
}

pub async fn serve(daemon: Daemon, client: AcceptedClient) {
    let remote = client.remote;
    let id = client.hello.client;
    tracing::info!(%remote, client = %id, name = %client.hello.name, "client connected");
    match run(&daemon, client).await {
        Ok(why) => tracing::info!(%remote, client = %id, why, "client finished"),
        Err(e) => tracing::info!(%remote, client = %id, error = %e, "client finished"),
    }
}

/// Serve one connection until it ends; `Ok` carries why it ended.
async fn run(daemon: &Daemon, client: AcceptedClient) -> Result<&'static str, NetError> {
    let AcceptedClient { conn, hello, mut tx, mut rx, .. } = client;

    let (out, mut out_rx) = mpsc::channel::<WorkerMsg>(CONTROL_DEPTH);
    let writer: JoinHandle<Result<(), NetError>> = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            tx.send(&msg).await?;
        }
        Ok(())
    });

    // Subscribed before anything the greeting carries is read, so an event in between is
    // heard, not lost; `Heard` drops the ones the greeting already told.
    let events = daemon.events.subscribe();
    let ack = HelloAck {
        protocol: PROTOCOL_VERSION,
        worker: daemon.id,
        name: daemon.name.clone(),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        caps: Caps::empty(),
        sessions: daemon.worker.summaries().await,
    };
    let mut heard = Heard {
        sessions: ack.sessions.iter().map(|s| (s.id, s.state)).collect(),
        ..Heard::default()
    };
    let mut greeting = vec![WorkerMsg::HelloAck(ack), WorkerMsg::Items(daemon.items.snapshot())];
    // Collected first: the locks must not be held across the sends.
    let agents = daemon.agents.lock().snapshot();
    greeting.extend(agents.into_iter().map(WorkerMsg::Agent));
    let known = daemon.ports.lock().known();
    greeting.extend(known.into_iter().map(|(session, ports)| WorkerMsg::Ports { session, ports }));
    for msg in greeting {
        heard.admit(&msg);
        out.send(msg).await.map_err(|_gone| NetError::Closed)?;
    }

    let (done, mut done_rx) = mpsc::unbounded_channel();
    let link = conn.stable_id();
    let mut tasks = JoinSet::new();
    tasks.spawn(relay_events(daemon.clone(), link, heard, events, out.clone(), done.clone()));
    let (reports, asked) = mpsc::channel(REPORT_DEPTH);
    tasks.spawn(answer_reports(conn.clone(), out.clone(), asked));
    tasks.spawn(slopty_net::endpoint::trace_path_health(conn.clone(), "worker"));
    let (feedback_tx, mut feedback_rx) = mpsc::channel::<Feedback>(FEEDBACK_DEPTH);
    let (copies_tx, mut copies_rx) = mpsc::channel::<InputCopy>(COPY_DEPTH);
    tasks.spawn(read_datagrams(conn.clone(), feedback_tx, copies_tx));
    let (clips_tx, mut clips_rx) = mpsc::channel::<crate::xfer::ClipData>(CLIP_DEPTH);
    tasks.spawn(crate::xfer::accept(
        daemon.clone(),
        conn.clone(),
        hello.client,
        out.clone(),
        clips_tx,
    ));
    tasks.spawn(crate::tunnel::accept(conn.clone(), hello.client));
    let (watch_files, watched) = watch::channel(Vec::new());
    tasks.spawn(crate::files::watch(hello.client, out.clone(), watched));
    let (told, mut told_rx) = mpsc::unbounded_channel();
    let mut peer = Peer {
        daemon,
        conn,
        client: hello.client,
        name: hello.name,
        out,
        reports,
        attached: HashMap::new(),
        screens: HashMap::new(),
        streams: JoinSet::new(),
        next_stream: 1,
        tasks,
        done,
        told,
        watch_files,
        downloads: HashMap::new(),
        held: None,
        link,
        order: InputOrder::default(),
        copies: slopty_net::echo::Copies::from_env(),
    };
    daemon.wake.lock().client_joined();

    let result = loop {
        tokio::select! {
            msg = rx.recv() => {
                let msg = match msg {
                    Ok(m) => m,
                    Err(NetError::Closed) => break Ok("control stream closed"),
                    Err(e) => break Err(e),
                };
                peer.handle(msg);
            }
            Some(feedback) = feedback_rx.recv() => peer.feedback(feedback),
            Some(copy) = copies_rx.recv() => peer.input_copy(copy),
            Some(clip) = clips_rx.recv() => peer.clip_data(clip.generation, &clip.uti, clip.bytes),
            Some(done) = done_rx.recv() => {
                if let Some(why) = peer.done(done) {
                    break Ok(why);
                }
            }
            Some(told) = told_rx.recv() => peer.told(told),
            () = fetching_until(peer.held.as_ref()) => {
                tracing::debug!(client = %peer.client, "paste went on without the clipboard");
                peer.release_held();
            }
            Some(_finished) = peer.tasks.join_next(), if !peer.tasks.is_empty() => {}
            Some(_finished) = peer.streams.join_next(), if !peer.streams.is_empty() => {}
            reason = peer.conn.closed() => {
                tracing::info!(client = %peer.client, %reason, "connection closed");
                break Ok("connection closed");
            }
        }
    };
    let InputOrder { taken, late, .. } = peer.order;
    tracing::info!(client = %peer.client, taken, late, "input copies");
    // Nothing more goes to this client, and nothing the streams still say may wait on it.
    writer.abort();
    peer.close_screens().await;
    // Whatever ended the loop, the client must not be left on a live connection nobody
    // serves; a no-op when the connection is already closed.
    peer.conn.close(0_u32.into(), b"done");
    drop(peer);
    result
}

/// Wake when a paste waiting for the client's clipboard is due to go on regardless.
async fn fetching_until(held: Option<&Held>) {
    match held {
        Some(Held { stage: Stage::Fetching { until }, .. }) => {
            tokio::time::sleep_until(*until).await;
        }
        _ => std::future::pending().await,
    }
}

/// Register `slopty hook` in the worker's Claude Code settings, for a client that saw an agent
/// the hooks are not reporting. Answers with the line the client shows as a notice.
///
/// The relay to register is the `slopty` binary shipped beside this daemon: in a bundle both
/// live in `Contents/MacOS`, and in a build tree both live in `target/<profile>`.
async fn install_hooks() -> (bool, String) {
    let Some(relay) = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("slopty")))
        .filter(|path| path.exists())
    else {
        return (false, "the slopty command is not installed beside the worker".to_owned());
    };
    let installed = tokio::task::spawn_blocking(move || {
        let path = slopty_agent::hooks::settings_path(&slopty_agent::hooks::home_dir());
        let outcome = slopty_agent::hooks::install_at(&path, &relay.to_string_lossy())?;
        Ok::<_, std::io::Error>((outcome, path))
    })
    .await;
    match installed {
        Ok(Ok((slopty_agent::hooks::Outcome::Changed, path))) => {
            (true, format!("hooks installed in {}", path.display()))
        }
        Ok(Ok((slopty_agent::hooks::Outcome::Unchanged, _path))) => {
            (true, "hooks were already installed".to_owned())
        }
        Ok(Err(e)) => (false, format!("could not install hooks: {e}")),
        Err(e) => (false, format!("could not install hooks: {e}")),
    }
}

/// A copy of one of a session's inputs (`ClientDatagram::Input`).
struct InputCopy {
    session: SessionId,
    seq: u64,
    req: TermRequest,
}

/// Read the client's datagrams until the connection ends: loss feedback for the loop, and
/// input copies, which the loop takes only in order ([`InputOrder`]).
async fn read_datagrams(
    conn: Connection,
    feedback: mpsc::Sender<Feedback>,
    copies: mpsc::Sender<InputCopy>,
) {
    loop {
        let datagram = match conn.read_datagram().await {
            Ok(d) => d,
            Err(e) => {
                tracing::debug!(error = %e, "read_datagram ended");
                break;
            }
        };
        match slopty_proto::codec::decode_body::<ClientDatagram>(&datagram) {
            Ok(ClientDatagram::Feedback(f)) => {
                if feedback.send(f).await.is_err() {
                    break;
                }
            }
            Ok(ClientDatagram::Input { session, seq, req }) if req.is_input() => {
                match copies.try_send(InputCopy { session, seq, req }) {
                    Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
                    Err(mpsc::error::TrySendError::Closed(_)) => break,
                }
            }
            Ok(ClientDatagram::Input { .. }) => {
                tracing::debug!(len = datagram.len(), "input copy of a request that is not input");
            }
            Err(e) => tracing::debug!(error = %e, len = datagram.len(), "bad datagram"),
        }
    }
}

/// Where each session's inputs stand on this connection, numbered as `TermRequest::is_input`
/// says both ends number them. The stream brings every input, in order; a datagram copy may
/// bring one sooner. A copy is applied only when it is the next input not yet applied, and the
/// stream copy of an input already applied is skipped, so each input reaches the PTY once and
/// in the order it was sent.
#[derive(Debug, Default)]
struct InputOrder {
    sessions: HashMap<SessionId, Applied>,
    /// Copies applied ahead of their stream copy.
    taken: u64,
    /// Copies dropped: their stream copy had come, or an input before them had not.
    late: u64,
}

/// One session's inputs: how many the stream has brought, and the highest number applied.
#[derive(Clone, Copy, Debug, Default)]
struct Applied {
    heard: u64,
    applied: u64,
}

impl InputOrder {
    /// The stream brought `session`'s next input: whether it is to be applied, which it is
    /// unless its copy was.
    fn stream(&mut self, session: SessionId) -> bool {
        let at = self.sessions.entry(session).or_default();
        at.heard = at.heard.saturating_add(1);
        let fresh = at.heard > at.applied;
        at.applied = at.applied.max(at.heard);
        fresh
    }

    /// A datagram brought a copy of `session`'s input `seq`: whether it is to be applied, which
    /// it is only when it is the next one.
    fn copy(&mut self, session: SessionId, seq: u64) -> bool {
        let at = self.sessions.entry(session).or_default();
        let next = seq == at.applied.saturating_add(1);
        if next {
            at.applied = seq;
            self.taken = self.taken.saturating_add(1);
        } else {
            self.late = self.late.saturating_add(1);
        }
        next
    }

    fn forget(&mut self, session: SessionId) {
        self.sessions.remove(&session);
    }
}

/// Tell the client a request about `session` failed, from a task that may wait for room.
async fn report(
    out: &mpsc::Sender<WorkerMsg>,
    client: ClientId,
    session: SessionId,
    e: &(dyn std::fmt::Display + Sync),
) {
    tracing::warn!(%client, %session, error = %e, "request failed");
    let event = TermEvent::Error(e.to_string());
    let _sent = out.send(WorkerMsg::Term { session, event }).await;
}

/// Queue `msg` for the client without waiting. A client whose control queue is full has not
/// read a thousand messages; what the loop answers it (a pong, an error, a clipboard fetch) is
/// dropped rather than let it hold every other terminal on the connection.
fn post(out: &mpsc::Sender<WorkerMsg>, client: ClientId, msg: WorkerMsg) {
    match out.try_send(msg) {
        Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => {}
        Err(mpsc::error::TrySendError::Full(msg)) => {
            tracing::warn!(%client, ?msg, "control queue full; answer dropped");
        }
    }
}

/// Send the worker's events to one client until the connection ends, waiting on the client as
/// long as it takes: a slow reader holds only this task, and once it lags the broadcast it is
/// sent the state again. Tells the loop of each closed session, and why it stopped.
async fn relay_events(
    daemon: Daemon,
    link: slopty_worker::clip::Link,
    mut heard: Heard,
    mut events: broadcast::Receiver<WorkerMsg>,
    out: mpsc::Sender<WorkerMsg>,
    done: mpsc::UnboundedSender<Done>,
) {
    let why = loop {
        match events.recv().await {
            // The worker's clipboard goes only to the clients that want it now.
            Ok(WorkerMsg::Clip(ClipMsg::Offer(_))) if !daemon.clip.is_watching(link) => {}
            Ok(msg) => {
                if let WorkerMsg::SessionClosed { session, .. } = &msg {
                    let _sent = done.send(Done::SessionClosed(*session));
                }
                if heard.admit(&msg) && out.send(msg).await.is_err() {
                    break "writer gone";
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(lagged = n, "missed worker events; sending the state again");
                // What is queued is older than the state about to be read; skip it.
                events = events.resubscribe();
                if resync(&daemon, &mut heard, &out, &done).await.is_err() {
                    break "writer gone";
                }
            }
            Err(broadcast::error::RecvError::Closed) => break "daemon shutting down",
        }
    };
    let _sent = done.send(Done::Ended(why));
}

/// Send the client the state the broadcast carries, for events it missed: the sessions opened
/// and closed since, the item registry, the agents and the ports. Clipboard offers are not
/// repeated; the next copy offers again. `Err` when the writer is gone.
async fn resync(
    daemon: &Daemon,
    heard: &mut Heard,
    out: &mpsc::Sender<WorkerMsg>,
    done: &mpsc::UnboundedSender<Done>,
) -> Result<(), ()> {
    let summaries = daemon.worker.summaries().await;
    let now: HashSet<SessionId> = summaries.iter().map(|s| s.id).collect();
    let mut msgs: Vec<WorkerMsg> = heard
        .sessions
        .keys()
        .filter(|session| !now.contains(session))
        // Why each ended was among what was missed.
        .map(|&session| WorkerMsg::SessionClosed { session, reason: CloseReason::Exited })
        .collect();
    for msg in &msgs {
        if let WorkerMsg::SessionClosed { session, .. } = msg {
            let _sent = done.send(Done::SessionClosed(*session));
        }
    }
    msgs.extend(summaries.into_iter().map(WorkerMsg::SessionOpened));
    msgs.push(WorkerMsg::Items(daemon.items.snapshot()));
    // Collected first: the locks must not be held across the sends.
    let agents = daemon.agents.lock().snapshot();
    msgs.extend(agents.into_iter().map(WorkerMsg::Agent));
    let ports = daemon.ports.lock().known();
    msgs.extend(ports.into_iter().map(|(session, ports)| WorkerMsg::Ports { session, ports }));
    for msg in msgs {
        if heard.admit(&msg) {
            out.send(msg).await.map_err(|_gone| ())?;
        }
    }
    Ok(())
}

/// Apply receiver reports in the order they came, off the loop: a report can change the
/// encoder's bitrate and cadence, which are VideoToolbox property calls and wait on the
/// encoder's lock while the stream rebuilds it. A changed rate is told to the client.
async fn answer_reports(
    conn: Connection,
    out: mpsc::Sender<WorkerMsg>,
    mut asked: mpsc::Receiver<Asked>,
) {
    while let Some(Asked { stream, control, report }) = asked.recv().await {
        let path = slopty_net::endpoint::path_rtt_cwnd(&conn).map(|(rtt, cwnd)| {
            slopty_worker::screen::PathSample {
                rtt: slopty_core::Duration::from_micros(
                    u64::try_from(rtt.as_micros()).unwrap_or(u64::MAX),
                ),
                cwnd,
            }
        });
        let decided = tokio::task::spawn_blocking(move || control.report(&report, path)).await;
        let Ok(Some(decision)) = decided else { continue };
        let event = ScreenEvent::Rate {
            stream,
            target_bps: decision.target_bps,
            verdict: decision.verdict,
            capped: decision.capped,
        };
        if out.send(WorkerMsg::Screen(event)).await.is_err() {
            break;
        }
    }
}

/// Where `session`'s shell is: its OSC 7 directory, else its foreground process's.
async fn session_cwd(daemon: &Daemon, session: SessionId) -> Option<String> {
    let handle = daemon.worker.get(session).ok()?;
    if let Some(cwd) = handle.snapshot().await.ok()?.cwd {
        return Some(cwd);
    }
    let cwd = handle.probe().await.ok()?.foreground?.cwd?;
    Some(cwd.to_string_lossy().into_owned())
}

/// What a session's pump needs of its connection.
struct Pipe {
    conn: Connection,
    out: mpsc::Sender<WorkerMsg>,
    client: ClientId,
    /// How an echo's datagram copy goes, when it does.
    copies: Option<slopty_net::echo::Copies>,
}

/// Send a datagram copy of an echo the session stream has just taken (`wire`, as the stream
/// carries it).
fn copy_frame(
    conn: &Connection,
    copies: slopty_net::echo::Copies,
    session: SessionId,
    wire: &[u8],
) {
    let body = wire.get(slopty_proto::codec::PREFIX_BYTES..).unwrap_or_default();
    match slopty_proto::datagram::term_datagram(session, body) {
        Ok(datagram) => copies.send(conn, datagram),
        Err(e) => tracing::debug!(%session, error = %e, "echo copy not encoded"),
    }
}

/// Open `session`'s stream to the client and pump the actor's events into it. The actor already
/// has the sink, so events wait in it while the stream opens. A stream the client does not
/// allow within [`slopty_net::streams::SESSION_STREAM_WAIT`] detaches the sink and tells the
/// client the attach failed.
async fn pump(
    pipe: Pipe,
    handle: SessionHandle,
    sink: ClientSink,
    mut events: mpsc::Receiver<Outbound>,
) {
    let Pipe { conn, out, client, copies } = pipe;
    let session = handle.id();
    let wait = slopty_net::streams::SESSION_STREAM_WAIT;
    let mut stream = match slopty_net::streams::open_session(&conn, session, wait).await {
        Ok(stream) => stream,
        Err(e) => {
            let _ignored = handle.detach_sink(client, &sink);
            return report(&out, client, session, &e).await;
        }
    };
    // Only the actor holds the sink from here, so the pump ends when it lets go.
    drop(sink);
    // Something a later frame depends on went since the last frame: that frame is not copied,
    // since its copy could overtake it.
    let mut depended_on = false;
    while let Some(event) = events.recv().await {
        let send_from = std::time::Instant::now();
        if let Err(e) = stream.send_raw(event.wire()).await {
            tracing::debug!(%client, %session, error = %e, "session stream ended");
            break;
        }
        if event.is_frame() {
            tracing::trace!(
                %client,
                %session,
                send_us = send_from.elapsed().as_micros(),
                "frame sent"
            );
            if let Some(copies) = copies
                && event.is_echo()
                && !depended_on
            {
                copy_frame(&conn, copies, session, event.wire());
            }
            depended_on = false;
        } else if event.goes_ahead_of_frames() {
            depended_on = true;
        }
    }
    let _finished = stream.finish();
}

/// One client's view of the daemon: its connection, outbound control queue, and attachments.
struct Peer<'d> {
    daemon: &'d Daemon,
    conn: Connection,
    client: ClientId,
    /// Its name from `Hello`, for the pointings it relays.
    name: String,
    out: mpsc::Sender<WorkerMsg>,
    /// Receiver reports, for the task that applies them.
    reports: mpsc::Sender<Asked>,
    /// Per attached session: the stream pump and the sink the actor writes into. The sink is
    /// weak, so the pump ends once the session's actor lets go of its end, whoever closed it.
    attached: HashMap<SessionId, (JoinHandle<()>, WeakClientSink)>,
    screens: HashMap<StreamId, Screen>,
    /// The screen streams' own tasks, awaited when the connection ends.
    streams: JoinSet<()>,
    next_stream: u32,
    /// Everything else running for this connection: the readers, the slow requests.
    tasks: JoinSet<()>,
    done: mpsc::UnboundedSender<Done>,
    told: mpsc::UnboundedSender<Told>,
    /// The files behind this client's file cards, for the task that watches them.
    watch_files: watch::Sender<Vec<String>>,
    /// Files going down, by transfer.
    downloads: HashMap<XferId, JoinHandle<()>>,
    /// Window input waiting for a paste's clipboard.
    held: Option<Held>,
    /// This connection, as clipboard sync tells clients apart.
    link: slopty_worker::clip::Link,
    /// Which inputs of each session have been applied, from the stream or a copy.
    order: InputOrder,
    /// How echoes' datagram copies go, if they do.
    copies: Option<slopty_net::echo::Copies>,
}

impl Drop for Peer<'_> {
    fn drop(&mut self) {
        // Detach only what this connection attached: the same client may already be back on a
        // new connection, and its viewers must survive the old one idling out.
        for (session, (task, sink)) in &self.attached {
            task.abort();
            if let Some(sink) = sink.upgrade()
                && let Ok(handle) = self.daemon.worker.get(*session)
            {
                let _ignored = handle.detach_sink(self.client, &sink);
            }
        }
        for task in self.downloads.values() {
            task.abort();
        }
        self.daemon.clip.forget(self.link);
        self.daemon.wake.lock().client_left();
    }
}

impl Peer<'_> {
    /// Queue `msg` for the client without waiting ([`post`]).
    fn post(&self, msg: WorkerMsg) {
        post(&self.out, self.client, msg);
    }

    /// Tell the client a request about `session` failed, without waiting.
    fn fail(&self, session: SessionId, e: &WorkerError) {
        tracing::warn!(client = %self.client, %session, error = %e, "request failed");
        self.post(WorkerMsg::Term { session, event: TermEvent::Error(e.to_string()) });
    }

    fn handle(&mut self, msg: ClientMsg) {
        match msg {
            ClientMsg::Hello(_) => {
                tracing::warn!(client = %self.client, "duplicate Hello ignored");
            }
            ClientMsg::Ping { sent_at } => self.post(WorkerMsg::Pong { sent_at }),
            ClientMsg::OpenSession(req) => {
                let (daemon, client) = (self.daemon.clone(), self.client);
                let (out, done) = (self.out.clone(), self.done.clone());
                self.tasks.spawn(async move {
                    let handle = match daemon.worker.open(&req).await {
                        Ok(handle) => handle,
                        Err(e) => return report(&out, client, SessionId::nil(), &e).await,
                    };
                    let session = handle.id();
                    // Whoever opened it sizes it, whichever client attaches first.
                    let _reserved = handle.reserve_driver(client);
                    let summaries = daemon.worker.summaries().await;
                    if let Some(summary) = summaries.into_iter().find(|s| s.id == session) {
                        let _sent = daemon.events.send(WorkerMsg::SessionOpened(summary));
                    }
                    if let Some(delta) = daemon.items.ensure_terminal(session, client) {
                        let _sent = daemon.events.send(WorkerMsg::Items(delta));
                    }
                    if req.attach {
                        let _sent = done.send(Done::Attach { session, size: req.size });
                    }
                });
            }
            ClientMsg::Term { session, req } => {
                if req.is_input() && !self.order.stream(session) {
                    tracing::trace!(client = %self.client, %session, "term input: its copy came first");
                    return;
                }
                if matches!(req, TermRequest::Raw(_) | TermRequest::Key(_)) {
                    tracing::trace!(client = %self.client, %session, "term input received");
                }
                self.term(session, req);
            }
            ClientMsg::Point { item } => {
                // Ephemeral: relayed as is, never in the registry. An item the worker no longer
                // has is for each client to ignore.
                let pointed =
                    ItemSync::Pointed { client: self.client, name: self.name.clone(), item };
                let _sent = self.daemon.events.send(WorkerMsg::Items(pointed));
            }
            ClientMsg::Items(op) => match self.daemon.items.apply(op, self.client) {
                Ok(delta) => {
                    let _sent = self.daemon.events.send(WorkerMsg::Items(delta));
                }
                Err(e) => self.fail(SessionId::nil(), &e),
            },
            ClientMsg::Screen(req) => self.screen(req),
            ClientMsg::FindFiles { root, query } => {
                self.tasks.spawn(crate::files::find(self.out.clone(), root, query));
            }
            ClientMsg::ReadFile { path } => {
                // A paired client already has a shell here; a read is nothing it could not do.
                let (client, out) = (self.client, self.out.clone());
                self.tasks.spawn(async move {
                    crate::files::send_file(client, &out, path).await;
                });
            }
            ClientMsg::WatchFiles { paths } => {
                self.watch_files.send_replace(paths);
            }
            ClientMsg::WriteFile { path, text, base_modified_ms } => {
                let (client, out) = (self.client, self.out.clone());
                self.tasks.spawn(async move {
                    crate::files::write(client, &out, path, text, base_modified_ms).await;
                });
            }
            ClientMsg::Clip(msg) => self.clip(msg),
            ClientMsg::Xfer(msg) => self.xfer(msg),
            ClientMsg::InstallHooks => {
                let (client, out) = (self.client, self.out.clone());
                self.tasks.spawn(async move {
                    let (ok, message) = install_hooks().await;
                    tracing::info!(%client, ok, %message, "install hooks");
                    let _sent = out.send(WorkerMsg::HooksInstalled { ok, message }).await;
                });
            }
        }
    }

    /// A task the loop started finished its part; `Some` with why when the connection is done.
    fn done(&mut self, done: Done) -> Option<&'static str> {
        match done {
            Done::Attach { session, size } => self.attach(session, size),
            Done::PastePlan(Some(Paste::Fetch { generation, utis })) => {
                tracing::debug!(client = %self.client, generation, ?utis, "paste waits for the clipboard");
                for uti in utis {
                    self.post(WorkerMsg::Clip(ClipMsg::Fetch { generation, uti }));
                }
                let until = tokio::time::Instant::now()
                    .checked_add(PASTE_WAIT)
                    .unwrap_or_else(tokio::time::Instant::now);
                if let Some(held) = &mut self.held {
                    held.stage = Stage::Fetching { until };
                }
            }
            Done::PastePlan(Some(Paste::Ready) | None) => self.release_held(),
            Done::PasteWritten => {
                tracing::debug!(client = %self.client, "pasteboard set for a paste");
                self.release_held();
            }
            Done::SessionClosed(session) => {
                self.order.forget(session);
                drop(self.attached.remove(&session));
            }
            Done::Ended(why) => return Some(why),
        }
        None
    }

    /// A screen stream's task says where it is.
    fn told(&mut self, told: Told) {
        match told {
            Told::Opened(id, control) => {
                if let Some(screen) = self.screens.get_mut(&id) {
                    screen.control = Some(control);
                }
            }
            Told::Gone(id) => {
                self.screens.remove(&id);
            }
        }
    }

    fn clip(&mut self, msg: ClipMsg) {
        match msg {
            ClipMsg::Watch(on) => {
                tracing::debug!(client = %self.client, on, "clipboard watch");
                self.daemon.clip.watch(self.link, on);
            }
            ClipMsg::Offer(offer) => {
                tracing::debug!(client = %self.client, generation = offer.generation, items = offer.items.len(), "client clipboard offered");
                self.daemon.clip.offered(self.link, offer);
            }
            ClipMsg::Fetch { generation, uti } => {
                let (daemon, conn, out) =
                    (self.daemon.clone(), self.conn.clone(), self.out.clone());
                self.tasks.spawn(async move {
                    crate::xfer::send_clip(&daemon, &conn, &out, generation, uti).await;
                });
            }
            ClipMsg::Data { generation, uti, bytes } => self.clip_data(generation, &uti, bytes),
            ClipMsg::Unavailable { generation } => {
                tracing::debug!(client = %self.client, generation, "client clipboard gone");
                self.write_held();
            }
        }
    }

    /// A representation of the client's clipboard arrived; the held paste goes on once all it
    /// waits for is here.
    fn clip_data(&mut self, generation: u64, uti: &str, bytes: Vec<u8>) {
        if self.daemon.clip.supply(self.link, generation, uti, bytes) {
            self.write_held();
        }
    }

    /// A paste waiting for the client's clipboard has all of it that is coming: put it on the
    /// pasteboard, off the runtime, and send the held input on once it is there.
    fn write_held(&mut self) {
        let Some(held) = &mut self.held else { return };
        if !matches!(held.stage, Stage::Fetching { .. }) {
            return;
        }
        held.stage = Stage::Writing;
        let (clip, link, done) = (Arc::clone(&self.daemon.clip), self.link, self.done.clone());
        self.tasks.spawn(async move {
            let _written = tokio::task::spawn_blocking(move || clip.write_incoming(link)).await;
            let _sent = done.send(Done::PasteWritten);
        });
    }

    /// Send the held input on to its windows, in the order it came.
    fn release_held(&mut self) {
        let Some(held) = self.held.take() else { return };
        for (stream, input) in held.inputs {
            self.command(stream, Command::Input(input));
        }
    }

    /// Input for a window. A paste chord first puts the client's clipboard on the pasteboard;
    /// until it is there, input for that window waits behind the chord.
    fn input(&mut self, stream: StreamId, input: ScreenInput) {
        match route(&mut self.held, stream, input) {
            Route::Now(input) => self.command(stream, Command::Input(input)),
            Route::Held => {}
            Route::Began => {
                let (clip, link) = (Arc::clone(&self.daemon.clip), self.link);
                let done = self.done.clone();
                self.tasks.spawn(async move {
                    let plan = tokio::task::spawn_blocking(move || clip.paste(link)).await;
                    let _sent = done.send(Done::PastePlan(plan.ok()));
                });
            }
        }
    }

    fn command(&self, stream: StreamId, command: Command) {
        if let Some(screen) = self.screens.get(&stream) {
            let _gone = screen.commands.send(command);
        }
    }

    fn xfer(&mut self, msg: XferMsg) {
        match msg {
            XferMsg::Begin { xfer, dest: Some(dest), files, bytes } => {
                let in_session = match &dest {
                    Dest::SessionCwd(session) => Some(*session),
                    Dest::Staging | Dest::Path(_) => None,
                };
                let (daemon, client) = (self.daemon.clone(), self.client);
                let begin = move |cwd: Option<String>| {
                    tracing::info!(%client, %xfer, ?dest, ?cwd, files, bytes, "upload begins");
                    daemon.transfers.begin(xfer, &dest, cwd.as_deref(), files);
                };
                // The directory is a question for the session's actor and then ptyd; the
                // upload's files wait for the `Begin` (`xfer::BEGIN_WAIT`), nothing else does.
                match in_session {
                    Some(session) => {
                        let daemon = self.daemon.clone();
                        self.tasks.spawn(async move {
                            let asked =
                                tokio::time::timeout(CWD_WAIT, session_cwd(&daemon, session));
                            begin(asked.await.ok().flatten());
                        });
                    }
                    None => begin(None),
                }
            }
            XferMsg::Resume { xfer, name } => {
                let transfers = Arc::clone(&self.daemon.transfers);
                let out = self.out.clone();
                self.tasks.spawn(async move {
                    let asked = name.clone();
                    let durable =
                        tokio::task::spawn_blocking(move || transfers.durable(xfer, &asked)).await;
                    let msg = match durable {
                        Ok(Ok(durable)) => XferMsg::Offset { xfer, name, durable },
                        Ok(Err(e)) => {
                            XferMsg::Failed { xfer, name: Some(name), error: e.to_string() }
                        }
                        Err(e) => XferMsg::Failed { xfer, name: Some(name), error: e.to_string() },
                    };
                    let _sent = out.send(WorkerMsg::Xfer(msg)).await;
                });
            }
            XferMsg::Cancel { xfer } => {
                tracing::info!(client = %self.client, %xfer, "transfer cancelled");
                self.daemon.transfers.cancel(xfer);
                if let Some(task) = self.downloads.remove(&xfer) {
                    task.abort();
                }
            }
            XferMsg::Fetch { xfer, path, held } => {
                self.downloads.retain(|_xfer, task| !task.is_finished());
                let transfers = Arc::clone(&self.daemon.transfers);
                let (conn, out) = (self.conn.clone(), self.out.clone());
                let task = crate::xfer::download(transfers, conn, out, xfer, path, held);
                if let Some(earlier) = self.downloads.insert(xfer, tokio::spawn(task)) {
                    earlier.abort();
                }
            }
            other @ (XferMsg::Begin { dest: None, .. }
            | XferMsg::Offset { .. }
            | XferMsg::Progress { .. }
            | XferMsg::Done { .. }
            | XferMsg::Finished { .. }
            | XferMsg::Failed { .. }) => {
                tracing::debug!(client = %self.client, ?other, "transfer receipt");
            }
        }
    }

    fn screen(&mut self, req: ScreenRequest) {
        match req {
            ScreenRequest::List => {
                let out = self.out.clone();
                let client = self.client;
                self.tasks.spawn(async move {
                    match listing().await {
                        Ok(event) => {
                            let _sent = out.send(WorkerMsg::Screen(event)).await;
                        }
                        Err(e) => tracing::warn!(%client, error = %e, "screen listing"),
                    }
                });
            }
            ScreenRequest::Open { target, quality } => {
                let id = StreamId(self.next_stream);
                self.next_stream = self.next_stream.wrapping_add(1).max(1);
                let (commands, rx) = mpsc::unbounded_channel();
                self.screens.insert(id, Screen { commands, control: None });
                let link = crate::screens::Link {
                    daemon: self.daemon.clone(),
                    client: self.client,
                    conn: self.conn.clone(),
                    out: self.out.clone(),
                    told: self.told.clone(),
                };
                self.streams.spawn(crate::screens::run(link, id, target, quality, rx));
            }
            ScreenRequest::Close(id) => {
                if let Some(screen) = self.screens.remove(&id) {
                    let _gone = screen.commands.send(Command::Close);
                }
            }
            ScreenRequest::SetQuality { stream, quality } => {
                self.command(stream, Command::SetQuality(quality));
            }
            ScreenRequest::Report { stream, report } => {
                if let Some(control) = self.screens.get(&stream).and_then(|s| s.control.clone()) {
                    let asked = Asked { stream, control, report };
                    if self.reports.try_send(asked).is_err() {
                        tracing::debug!(client = %self.client, %stream, "reports backed up; one dropped");
                    }
                }
            }
            ScreenRequest::Input { stream, input } => self.input(stream, input),
            ScreenRequest::Focus(stream) => self.command(stream, Command::Focus),
            ScreenRequest::Resize { stream, width, height } => {
                self.command(stream, Command::Resize { width, height });
            }
        }
    }

    /// A datagram copy of an input: applied now if it is the session's next, else left to its
    /// stream copy.
    fn input_copy(&mut self, InputCopy { session, seq, req }: InputCopy) {
        if self.order.copy(session, seq) {
            tracing::trace!(client = %self.client, %session, seq, "term input copy received");
            self.term(session, req);
        }
    }

    /// Loss feedback from a datagram: retransmit or refresh.
    fn feedback(&self, feedback: Feedback) {
        let control = |stream| self.screens.get(&stream).and_then(|s| s.control.as_ref());
        match feedback {
            Feedback::Nack { stream, frame, fragments } => {
                if let Some(control) = control(stream) {
                    tracing::debug!(
                        %stream,
                        frame,
                        missing = fragments.len(),
                        path = %slopty_net::endpoint::describe_health(&self.conn),
                        "nack"
                    );
                    control.nack(frame, &fragments);
                }
            }
            Feedback::Refresh { stream, last_good_frame, keyframe } => {
                if let Some(control) = control(stream) {
                    control.request_refresh(last_good_frame, keyframe);
                }
            }
        }
    }

    /// Let go of every stream and wait for each to stop capturing. The command channels
    /// closing, rather than a `Close`, tells a stream its client is gone: there is no one to
    /// say "closed" to.
    async fn close_screens(&mut self) {
        self.screens.clear();
        while self.streams.join_next().await.is_some() {}
    }

    fn term(&mut self, session: SessionId, req: TermRequest) {
        match req {
            TermRequest::Attach { size } => self.attach(session, size),
            TermRequest::Detach => {
                self.forget(session);
                if let Ok(h) = self.daemon.worker.get(session) {
                    let _ignored = h.detach(self.client);
                }
            }
            TermRequest::Close => {
                self.forget(session);
                let (daemon, client, out) = (self.daemon.clone(), self.client, self.out.clone());
                self.tasks.spawn(async move {
                    match daemon.end_session(session, CloseReason::Requested, client).await {
                        Ok(()) => tracing::debug!(%client, %session, "closed on request"),
                        // Gone already: most often its program exited, and a client closes a
                        // terminal on seeing that.
                        Err(WorkerError::NoSuchSession) => {}
                        Err(e) => report(&out, client, session, &e).await,
                    }
                });
            }
            other => {
                let outcome =
                    self.daemon.worker.get(session).and_then(|h| h.request(self.client, other));
                if let Err(e) = outcome {
                    self.fail(session, &e);
                }
            }
        }
    }

    fn forget(&mut self, session: SessionId) {
        if let Some((task, _sink)) = self.attached.remove(&session) {
            task.abort();
        }
    }

    /// Attach to `session`: hand its actor a sink now, and open the stream the sink is pumped
    /// into on a task, since the client may make the stream wait.
    fn attach(&mut self, session: SessionId, size: TermSize) {
        let handle = match self.daemon.worker.get(session) {
            Ok(h) => h,
            Err(e) => return self.fail(session, &e),
        };
        self.forget(session);
        let (sink, events) = mpsc::channel::<Outbound>(SINK_DEPTH);
        let weak = sink.downgrade();
        if let Err(e) = handle.attach(self.client, size, sink.clone()) {
            return self.fail(session, &e);
        }
        let pipe = Pipe {
            conn: self.conn.clone(),
            out: self.out.clone(),
            client: self.client,
            copies: self.copies,
        };
        let task = tokio::spawn(pump(pipe, handle, sink, events));
        self.attached.insert(session, (task, weak));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use slopty_core::{ClientId, ItemId, SessionId};
    use slopty_net::WorkerMsg;
    use slopty_proto::input::{KeyAction, KeyCode, Mods};
    use slopty_proto::items::{ItemOp, ItemSync};
    use slopty_proto::screen::ScreenInput;
    use slopty_proto::terminal::{CloseReason, SessionState, SessionSummary};

    use super::{Heard, InputOrder, Route, StreamId, route};

    fn key(code: KeyCode, mods: Mods) -> ScreenInput {
        ScreenInput::Key { code, action: KeyAction::Press, mods, text: None }
    }

    fn opened(id: SessionId) -> WorkerMsg {
        WorkerMsg::SessionOpened(SessionSummary {
            id,
            title: String::new(),
            cwd: None,
            repo: None,
            cols: 80,
            rows: 24,
            state: SessionState::Running,
            viewers: 0,
            command: Vec::new(),
            agent: None,
        })
    }

    fn delta(version: u64) -> WorkerMsg {
        let op = ItemOp::Remove(ItemId::new());
        WorkerMsg::Items(ItemSync::Delta { version, by: ClientId::new(), op })
    }

    /// The events are subscribed to before the greeting is read, so one that happened in
    /// between arrives after it: what the greeting (or a resync) already told is not told
    /// again, and what it did not is.
    #[test]
    fn an_event_the_greeting_carried_is_not_told_again() {
        let (told, new) = (SessionId::new(), SessionId::new());
        let mut heard =
            Heard { sessions: HashMap::from([(told, SessionState::Running)]), ..Heard::default() };
        let snapshot = WorkerMsg::Items(ItemSync::Snapshot { version: 5, items: Vec::new() });
        assert!(heard.admit(&snapshot));

        assert!(!heard.admit(&opened(told)), "in the HelloAck");
        assert!(!heard.admit(&delta(5)), "in the snapshot");
        assert!(heard.admit(&delta(6)));
        assert!(heard.admit(&opened(new)));
        assert!(!heard.admit(&opened(new)), "told once");
        let mut exited = opened(new);
        if let WorkerMsg::SessionOpened(summary) = &mut exited {
            summary.state = SessionState::Exited { status: 3 };
        }
        assert!(heard.admit(&exited), "its program exited: the new state is news");
        assert!(!heard.admit(&exited), "and told once");
        let closed = WorkerMsg::SessionClosed { session: told, reason: CloseReason::Exited };
        assert!(heard.admit(&closed));
        let exited = SessionState::Exited { status: 3 };
        assert_eq!(heard.sessions, HashMap::from([(new, exited)]));
    }

    /// Each input reaches the PTY once and in the order it was sent, whichever copy of it came
    /// first: a copy goes only when it is the next, and a stream copy after it is skipped.
    #[test]
    fn each_input_is_applied_once_in_order_from_the_first_copy() {
        let (a, b) = (SessionId::new(), SessionId::new());
        let mut order = InputOrder::default();
        // (from the stream, session, the copy's number), and whether it is applied.
        let arrivals = [
            (true, a, 0, true),
            (false, a, 2, true),
            (false, a, 4, false),
            (true, a, 0, false),
            (false, a, 3, true),
            (false, b, 1, true),
            (true, a, 0, false),
            (true, a, 0, true),
            (false, a, 4, false),
            (false, a, 1, false),
            (true, b, 0, false),
            (false, b, 3, false),
            (true, b, 0, true),
            (true, b, 0, true),
        ];
        let mut applied: Vec<(SessionId, u64)> = Vec::new();
        let mut counts = HashMap::<SessionId, u64>::new();
        for (i, (stream, session, seq, expected)) in arrivals.into_iter().enumerate() {
            let heard = counts.entry(session).or_default();
            let (goes, n) = if stream {
                *heard += 1;
                (order.stream(session), *heard)
            } else {
                (order.copy(session, seq), seq)
            };
            assert_eq!(goes, expected, "arrival {i}");
            if goes {
                applied.push((session, n));
            }
        }
        assert_eq!(applied, [(a, 1), (a, 2), (a, 3), (b, 1), (a, 4), (b, 2), (b, 3)]);
        assert_eq!((order.taken, order.late), (3, 4));
    }

    /// A paste holds only the window it went to, in order; another window's input goes on, and a
    /// second paste joins the hold behind the first. The release hands the held input back in the
    /// order it came.
    #[test]
    fn a_paste_holds_its_own_window_and_no_other() {
        let (a, b, c) = (StreamId(1), StreamId(2), StreamId(3));
        let paste = || key(KeyCode::V, Mods::SUPER);
        let typed = |code| key(code, Mods::empty());
        let mut held = None;

        assert_eq!(route(&mut held, a, typed(KeyCode::A)), Route::Now(typed(KeyCode::A)));
        assert_eq!(route(&mut held, a, paste()), Route::Began);
        assert_eq!(route(&mut held, a, typed(KeyCode::B)), Route::Held, "behind the chord");
        assert_eq!(route(&mut held, b, typed(KeyCode::C)), Route::Now(typed(KeyCode::C)));
        assert_eq!(route(&mut held, c, paste()), Route::Held, "a second paste waits too");
        assert_eq!(route(&mut held, c, typed(KeyCode::D)), Route::Held);
        assert_eq!(route(&mut held, b, typed(KeyCode::E)), Route::Now(typed(KeyCode::E)));
        let with_ctrl = key(KeyCode::V, Mods::SUPER | Mods::CTRL);
        assert_eq!(route(&mut held, b, with_ctrl.clone()), Route::Now(with_ctrl), "not a paste");

        let released = held.take().map(|h| h.inputs).unwrap_or_default();
        assert_eq!(
            released,
            [(a, paste()), (a, typed(KeyCode::B)), (c, paste()), (c, typed(KeyCode::D)),]
        );
    }
}
