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
use slopty_proto::datagram::ClientDatagram;
use slopty_proto::drag::{DragId, DragItem};
use slopty_proto::folder::Listing;
use slopty_proto::handoff::HandoffReply;
use slopty_proto::handshake::HelloAck;
use slopty_proto::items::ItemSync;
use slopty_proto::orchestration::ErrorCode;
use slopty_proto::screen::{
    Feedback, ReceiverReport, ScreenEvent, ScreenInput, ScreenRequest, Stripe,
};
use slopty_proto::terminal::{
    CloseReason, SessionSummary, TermError, TermEvent, TermRequest, TermSize,
};
use slopty_proto::transfer::{ClipMsg, Dest, RepRef, Source, XferMsg};
use slopty_worker::clip::{Paste, PasteKind};
use slopty_worker::screen::{StreamControl, listing};
use slopty_worker::session::{ClientSink, Outbound, SessionHandle};
use slopty_worker::xfer::Begun;
use slopty_worker::{DragData, WorkerError};
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
/// window or shell anyway (and pastes what the worker had).
const PASTE_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// Where an input goes: a streamed window, or a shell.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Target {
    Window(StreamId),
    Session(SessionId),
}

/// One input for a window or a shell, as a paste may hold it back.
#[derive(Debug, PartialEq)]
enum Input {
    Window(StreamId, ScreenInput),
    /// A request that writes to the PTY (`TermRequest::is_input`).
    Term(SessionId, TermRequest),
}

impl Input {
    const fn target(&self) -> Target {
        match self {
            Self::Window(stream, _) => Target::Window(*stream),
            Self::Term(session, _) => Target::Session(*session),
        }
    }

    /// Whether this input is a paste that must find the client's clipboard on the pasteboard,
    /// and which: ⌘V to a window, or a shell's paste of a picture.
    fn paste(&self) -> Option<PasteKind> {
        match self {
            Self::Window(_, input) => input.is_paste_chord().then_some(PasteKind::Window),
            Self::Term(_, req) => {
                matches!(req, TermRequest::PastePicture(_)).then_some(PasteKind::Picture)
            }
        }
    }
}

/// Input for the windows and shells a paste went to, held back in order while the client's
/// clipboard is put on the pasteboard. Every other window and shell carries on.
struct Held {
    targets: HashSet<Target>,
    inputs: Vec<Input>,
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

/// Where one input goes while pastes may be holding some back.
#[derive(Debug, PartialEq)]
enum Route {
    /// To its window or shell now.
    Now(Input),
    /// Behind the paste its window is waiting on.
    Held,
    /// A paste chord of this kind that starts a hold: the clipboard is to be asked what it
    /// needs.
    Began(PasteKind),
}

/// Route one input. A paste holds its window or shell, and joins a hold already under way;
/// input for a held target queues behind the paste, in order; anything else goes now.
fn route(held: &mut Option<Held>, input: Input) -> Route {
    let (paste, target) = (input.paste(), input.target());
    match held {
        Some(held) if paste.is_some() || held.targets.contains(&target) => {
            held.targets.insert(target);
            held.inputs.push(input);
            Route::Held
        }
        None => match paste {
            Some(kind) => {
                *held = Some(Held {
                    targets: HashSet::from([target]),
                    inputs: vec![input],
                    stage: Stage::Deciding,
                });
                Route::Began(kind)
            }
            None => Route::Now(input),
        },
        Some(_) => Route::Now(input),
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
    /// Each session told of, as it was told: a summary that differs (the program exited, the
    /// directory, branch or the working tree's changes moved) is news.
    sessions: HashMap<SessionId, SessionSummary>,
    /// The item registry version of the last snapshot sent: deltas up to it are in it.
    items_floor: u64,
}

impl Heard {
    /// Whether `msg` still tells the client something; what it tells is noted either way.
    fn admit(&mut self, msg: &WorkerMsg) -> bool {
        match msg {
            WorkerMsg::SessionOpened { summary, .. } | WorkerMsg::SessionChanged(summary) => {
                self.sessions.insert(summary.id, summary.clone()).as_ref() != Some(summary)
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
    let AcceptedClient { conn, hello, mut tx, mut rx, remote: client_remote, .. } = client;

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
    let caps = daemon.caps.borrow().clone();
    let load = *daemon.load.borrow();
    let ack = HelloAck {
        worker: daemon.id,
        name: daemon.name.clone(),
        home: daemon.home.clone(),
        settings: daemon.settings.clone(),
        caps,
        load,
        sessions: daemon.worker.summaries().await,
    };
    let mut heard = Heard {
        sessions: ack.sessions.iter().map(|s| (s.id, s.clone())).collect(),
        ..Heard::default()
    };
    let mut greeting = vec![WorkerMsg::HelloAck(ack), WorkerMsg::Items(daemon.items.snapshot())];
    // Collected first: the locks must not be held across the sends.
    let (agents, branches) = {
        let table = daemon.agents.lock();
        (table.snapshot(), table.branches())
    };
    greeting.extend(agents.into_iter().map(WorkerMsg::Agent));
    greeting.extend(branches.into_iter().map(WorkerMsg::AgentBranch));
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
    let (feedback_tx, mut feedback_rx) = mpsc::channel::<Arrived>(FEEDBACK_DEPTH);
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
    tasks.spawn({
        let (paths, remote, out) = (daemon.paths.clone(), client_remote, out.clone());
        async move { paths.report(remote, out).await }
    });
    let (watch_files, watched) = watch::channel(Vec::new());
    tasks.spawn(crate::files::watch(hello.client, conn.clone(), out.clone(), watched));
    let (watch_folders, folders) = watch::channel(Vec::new());
    tasks.spawn(crate::files::watch_folders(hello.client, out.clone(), folders));
    let (told, mut told_rx) = mpsc::unbounded_channel();
    daemon.clip.attach(link, clip_sink(out.clone(), hello.client));
    let (saves, saved) = mpsc::unbounded_channel();
    // Not one of the connection's tasks: saves and edit ends already sent are written and
    // heard after the connection drops, so a waiting program still gets its answer.
    tokio::spawn(crate::files::save_in_order(
        Arc::clone(&daemon.handoffs),
        hello.client,
        out.clone(),
        saved,
    ));
    let unwatched = daemon.handoffs.lock().join(
        hello.client,
        link,
        hello.name.clone(),
        handoff_sink(out.clone(), hello.client),
    );
    for session in unwatched {
        let _sent = daemon.presence.send((session, false));
    }
    let mut peer = Peer {
        daemon,
        conn,
        client: hello.client,
        out,
        reports,
        attached: HashMap::new(),
        screens: HashMap::new(),
        sound: None,
        streams: JoinSet::new(),
        next_stream: 1,
        tasks,
        done,
        told,
        watch_files,
        watch_folders,
        downloads: HashMap::new(),
        held: None,
        link,
        order: InputOrder::default(),
        screen_order: ScreenOrder::default(),
        copies: slopty_net::echo::Copies::from_env(),
        threads: crate::threads::Following::default(),
        searches: slopty_worker::search::Searches::default(),
        saves,
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
            Some(Arrived { feedback, at }) = feedback_rx.recv() => peer.feedback(feedback, at),
            Some(copy) = copies_rx.recv() => peer.input_copy(copy),
            Some(clip) = clips_rx.recv() => peer.clip_data(&clip.rep, clip.bytes),
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
    let ScreenOrder { taken: window_taken, late: window_late, .. } = peer.screen_order;
    tracing::info!(client = %peer.client, taken, late, window_taken, window_late, "input copies");
    // Nothing more goes to this client, and nothing the streams still say may wait on it.
    writer.abort();
    peer.close_screens().await;
    // Whatever ended the loop, the client must not be left on a live connection nobody
    // serves; a no-op when the connection is already closed.
    peer.conn.close(0_u32.into(), b"done");
    drop(peer);
    result
}

/// Where the clipboard sends this client a fetch its promises need: the control stream, without
/// waiting, since a promise is answered on a thread AppKit chose.
fn clip_sink(out: mpsc::Sender<WorkerMsg>, client: ClientId) -> slopty_worker::clip::Sink {
    Arc::new(move |msg| post(&out, client, WorkerMsg::Clip(msg)))
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

/// A copy of one input from a datagram.
#[derive(Debug)]
enum InputCopy {
    /// A session's (`ClientDatagram::Input`).
    Term { session: SessionId, seq: u64, req: TermRequest },
    /// A window stream's (`ClientDatagram::ScreenInput`).
    Window { stream: StreamId, seq: u64, ordered: u64, input: ScreenInput },
}

/// A feedback datagram and when the reader took it off the connection: a clock probe is
/// stamped with that, not with when the loop gets to it.
#[derive(Debug)]
struct Arrived {
    feedback: Feedback,
    at: std::time::Instant,
}

/// Read the client's datagrams until the connection ends: loss feedback for the loop, and
/// input copies, which the loop takes only in order ([`InputOrder`]).
async fn read_datagrams(
    conn: Connection,
    feedback: mpsc::Sender<Arrived>,
    copies: mpsc::Sender<InputCopy>,
) {
    loop {
        let (datagram, at) = match conn.read_datagram().await {
            Ok(d) => (d, std::time::Instant::now()),
            Err(e) => {
                tracing::debug!(error = %e, "read_datagram ended");
                break;
            }
        };
        match ClientDatagram::decode(&datagram) {
            Some(ClientDatagram::Feedback(f)) => {
                if feedback.send(Arrived { feedback: f, at }).await.is_err() {
                    break;
                }
            }
            Some(ClientDatagram::Input { session, seq, req }) if req.is_input() => {
                if !hand_on(&copies, InputCopy::Term { session, seq, req }) {
                    break;
                }
            }
            Some(ClientDatagram::Input { .. }) => {
                tracing::debug!(len = datagram.len(), "input copy of a request that is not input");
            }
            Some(ClientDatagram::ScreenInput { stream, seq, ordered, input }) => {
                if !hand_on(&copies, InputCopy::Window { stream, seq, ordered, input }) {
                    break;
                }
            }
            None => tracing::debug!(len = datagram.len(), "bad datagram"),
        }
    }
}

/// Queue a copy for the loop; one that finds the queue full is dropped, since its stream copy
/// comes regardless. `false` once the loop is gone.
fn hand_on(copies: &mpsc::Sender<InputCopy>, copy: InputCopy) -> bool {
    !matches!(copies.try_send(copy), Err(mpsc::error::TrySendError::Closed(_)))
}

/// Where each window stream's input stands on this connection, numbered as
/// `ScreenRequest::numbered` says both ends number it: every input and quality change, and
/// apart, those that apply only in their turn (all but moves). The stream brings every one, in
/// order; a datagram copy may bring an input sooner.
///
/// An in-order copy is applied only when it is the next in-order request not yet applied. A
/// move's copy is applied when every in-order request before it is and nothing newer has been,
/// so a move may overtake older moves, never a click, a key or a change of scale. From the
/// stream, an in-order request is skipped when its copy was applied, and a move when anything
/// newer was. Each click and key therefore reaches the window once and in order, and the
/// pointer never goes back to a place it left.
#[derive(Debug, Default)]
struct ScreenOrder {
    streams: HashMap<StreamId, Placed>,
    /// Copies applied ahead of their stream copy.
    taken: u64,
    /// Copies dropped: their stream copy had come, something before them had not, or a newer
    /// input had been applied.
    late: u64,
}

/// One stream's numbered requests: how many the stream has brought, and of those how many are
/// in order; how many in-order ones have been applied, and the highest number applied.
#[derive(Clone, Copy, Debug, Default)]
struct Placed {
    heard: u64,
    heard_in_order: u64,
    in_order: u64,
    latest: u64,
}

impl ScreenOrder {
    /// The stream brought `stream`'s next numbered request: whether it is to be applied.
    fn stream(&mut self, stream: StreamId, in_order: bool) -> bool {
        let at = self.streams.entry(stream).or_default();
        at.heard = at.heard.saturating_add(1);
        let fresh = if in_order {
            at.heard_in_order = at.heard_in_order.saturating_add(1);
            let fresh = at.heard_in_order > at.in_order;
            at.in_order = at.in_order.max(at.heard_in_order);
            fresh
        } else {
            at.heard > at.latest
        };
        at.latest = at.latest.max(at.heard);
        fresh
    }

    /// A datagram brought a copy of `stream`'s request `seq`, the `ordered`-th in order:
    /// whether it is to be applied.
    fn copy(&mut self, stream: StreamId, seq: u64, ordered: u64, in_order: bool) -> bool {
        let at = self.streams.entry(stream).or_default();
        let goes = if in_order {
            ordered == at.in_order.saturating_add(1)
        } else {
            ordered == at.in_order && seq > at.latest
        };
        if goes {
            if in_order {
                at.in_order = ordered;
            }
            at.latest = at.latest.max(seq);
            self.taken = self.taken.saturating_add(1);
        } else {
            self.late = self.late.saturating_add(1);
        }
        goes
    }

    fn forget(&mut self, stream: StreamId) {
        self.streams.remove(&stream);
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

/// Where the handoffs send this client an ask: the control stream, without waiting.
fn handoff_sink(out: mpsc::Sender<WorkerMsg>, client: ClientId) -> slopty_worker::handoff::Sink {
    Arc::new(move |msg| post(&out, client, msg))
}

/// Tell the client a request about `session` failed, from a task that may wait for room.
async fn report(out: &mpsc::Sender<WorkerMsg>, client: ClientId, session: SessionId, e: TermError) {
    tracing::warn!(%client, %session, error = %e, "request failed");
    let _sent = out.send(WorkerMsg::Term { session, event: TermEvent::Error(e) }).await;
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
    msgs.extend(summaries.into_iter().map(WorkerMsg::SessionChanged));
    msgs.push(WorkerMsg::Items(daemon.items.snapshot()));
    msgs.push(WorkerMsg::Caps(daemon.caps.borrow().clone()));
    // Collected first: the locks must not be held across the sends.
    let (agents, branches) = {
        let table = daemon.agents.lock();
        (table.snapshot(), table.branches())
    };
    msgs.extend(agents.into_iter().map(WorkerMsg::Agent));
    msgs.extend(branches.into_iter().map(WorkerMsg::AgentBranch));
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
    // The copy is at least its header and the event. A frame past what the path carries (a
    // screenful redrawn) is not copied at all, rather than copied, held for the delay and
    // dropped at the send.
    let least = slopty_proto::datagram::TERM_OVERHEAD.saturating_add(body.len());
    if !slopty_net::echo::path_takes(conn, least) {
        return;
    }
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
            return report(&out, client, session, TermError::Stream(e.to_string())).await;
        }
    };
    // Only the actor holds the sink from here, so the pump ends when it lets go.
    drop(sink);
    // Something a later frame depends on went since the last frame: that frame is not copied,
    // since its copy could overtake it.
    let mut depended_on = false;
    let mut lift = slopty_net::streams::EchoLift::default();
    while let Some(event) = events.recv().await {
        let send_from = std::time::Instant::now();
        if event.is_frame() {
            lift.before_frame(&stream, event.is_echo());
        }
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
                copy_frame(&conn, copies, session, &event.wire());
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
    out: mpsc::Sender<WorkerMsg>,
    /// Receiver reports, for the task that applies them.
    reports: mpsc::Sender<Asked>,
    /// Per attached session: the stream pump and the sink the actor writes into. The sink is
    /// weak, so the pump ends once the session's actor lets go of its end, whoever closed it.
    attached: HashMap<SessionId, (JoinHandle<()>, WeakClientSink)>,
    screens: HashMap<StreamId, Screen>,
    /// The client's one sound from this worker, made with its first stream.
    sound: Option<Arc<dyn slopty_worker::screen::sound::Listen>>,
    /// The screen streams' own tasks, awaited when the connection ends.
    streams: JoinSet<()>,
    next_stream: u32,
    /// Everything else running for this connection: the readers, the slow requests.
    tasks: JoinSet<()>,
    done: mpsc::UnboundedSender<Done>,
    told: mpsc::UnboundedSender<Told>,
    /// The files behind this client's file tiles, for the task that watches them.
    watch_files: watch::Sender<Vec<String>>,
    /// The directories behind this client's folder tiles, for the task that watches them.
    watch_folders: watch::Sender<Vec<String>>,
    /// Files going down, by transfer.
    downloads: HashMap<XferId, JoinHandle<()>>,
    /// Window and shell input waiting for a paste's clipboard.
    held: Option<Held>,
    /// This connection, as clipboard sync tells clients apart.
    link: slopty_worker::clip::Link,
    /// Which inputs of each session have been applied, from the stream or a copy.
    order: InputOrder,
    /// The same for each window stream.
    screen_order: ScreenOrder,
    /// How echoes' datagram copies go, if they do.
    copies: Option<slopty_net::echo::Copies>,
    /// The threads this client keeps a table of and follows.
    threads: crate::threads::Following,
    /// The text search this client runs, stopped by its next one and with the connection.
    searches: slopty_worker::search::Searches,
    /// This client's saves and edit ends, taken in order ([`crate::files::save_in_order`]).
    saves: mpsc::UnboundedSender<crate::files::Save>,
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
        let unwatched = self.daemon.handoffs.lock().leave(self.client, self.link);
        for session in unwatched {
            let _sent = self.daemon.presence.send((session, false));
        }
        let released = self.daemon.follows.lock().holds.leave(self.link);
        crate::threads::hold::release(self.daemon, released);
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
        self.post(WorkerMsg::Term { session, event: TermEvent::Error(e.term_error()) });
    }

    fn handle(&mut self, msg: ClientMsg) {
        match msg {
            ClientMsg::Hello(_) => {
                tracing::warn!(client = %self.client, "duplicate Hello ignored");
            }
            ClientMsg::Ping { sent_at } => self.post(WorkerMsg::Pong { sent_at }),
            ClientMsg::OpenSession { request, spec: req } => {
                let (daemon, client) = (self.daemon.clone(), self.client);
                let (out, done) = (self.out.clone(), self.done.clone());
                self.tasks.spawn(async move {
                    let handle = match daemon.worker.open(&req).await {
                        Ok(handle) => handle,
                        Err(e) => {
                            tracing::warn!(%client, request, error = %e, "open failed");
                            let (code, message) = (ErrorCode::Failed, e.to_string());
                            let _sent =
                                out.send(WorkerMsg::Failed { request, code, message }).await;
                            return;
                        }
                    };
                    let session = handle.id();
                    // Whoever opened it sizes it, whichever client attaches first.
                    let _reserved = handle.reserve_driver(client);
                    if let Some(summary) = daemon.worker.summary(session).await {
                        // Its own answer first, then the news for every client, this one too.
                        let opened = WorkerMsg::SessionOpened { request, summary: summary.clone() };
                        let _sent = out.send(opened).await;
                        let _sent = daemon.events.send(WorkerMsg::SessionChanged(summary));
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
            ClientMsg::Items(op) => match self.daemon.items.apply(op, self.client) {
                Ok(delta) => {
                    let _sent = self.daemon.events.send(WorkerMsg::Items(delta));
                }
                Err(e) => {
                    // The proposer applied its op to its own copy already; the registry as it
                    // is puts that copy back in step, and that is all the answer it needs.
                    tracing::warn!(client = %self.client, error = %e, "item change refused");
                    self.post(WorkerMsg::Items(self.daemon.items.snapshot()));
                }
            },
            ClientMsg::Screen(req) => self.screen(req),
            ClientMsg::FindFiles { root, query } => {
                self.tasks.spawn(crate::files::find(self.out.clone(), root, query));
            }
            ClientMsg::Search(request) => self.searches.handle(request, &self.out),
            ClientMsg::ReadFile { path } => {
                // An admitted client can open a shell here; a read is nothing it could not do.
                let (client, conn, out) = (self.client, self.conn.clone(), self.out.clone());
                self.tasks.spawn(async move {
                    crate::files::send_file(client, &conn, &out, path).await;
                });
            }
            ClientMsg::ListFolder { path } => {
                let (client, out) = (self.client, self.out.clone());
                self.tasks.spawn(async move {
                    let dir = std::path::PathBuf::from(&path);
                    let listing =
                        tokio::task::spawn_blocking(move || slopty_worker::listing::folder(&dir))
                            .await
                            .unwrap_or_else(|_| Listing::Missing {
                                error: "list failed".to_owned(),
                            });
                    tracing::info!(%client, %path, "list folder");
                    let _sent = out.send(WorkerMsg::Folder { path, listing }).await;
                });
            }
            ClientMsg::FolderPage { path, after } => {
                let (client, out) = (self.client, self.out.clone());
                self.tasks.spawn(crate::files::folder_page(client, out, path, after));
            }
            ClientMsg::FsOp { request, op } => {
                let _gone = self.saves.send(crate::files::Save::Fs { request, op });
            }
            ClientMsg::Git { request, repo, op } => {
                let _gone = self.saves.send(crate::files::Save::Git { request, repo, op });
            }
            ClientMsg::WatchFiles { paths } => {
                self.watch_files.send_replace(paths);
            }
            ClientMsg::WatchFolders { paths } => {
                self.watch_folders.send_replace(paths);
            }
            ClientMsg::WriteFile { path, text, base_modified_ms } => {
                let save =
                    crate::files::Save::File { path, text: text.into_bytes(), base_modified_ms };
                let _gone = self.saves.send(save);
            }
            ClientMsg::Handoff(reply @ HandoffReply::Edited { .. }) => {
                let _gone = self.saves.send(crate::files::Save::Edited(reply));
            }
            ClientMsg::Handoff(reply) => self.daemon.handoffs.lock().replied(self.client, reply),
            ClientMsg::HandoffCaps(caps) => self.daemon.handoffs.lock().caps(self.client, caps),
            ClientMsg::Clip(msg) => self.clip(msg),
            ClientMsg::Xfer(msg) => self.xfer(msg),
            ClientMsg::Thread(req) => {
                let mut at = crate::threads::Origin {
                    daemon: self.daemon,
                    conn: &self.conn,
                    out: &self.out,
                    link: self.link,
                    client: self.client,
                    tasks: &mut self.tasks,
                };
                self.threads.handle(&mut at, req);
            }
        }
    }

    /// A task the loop started finished its part; `Some` with why when the connection is done.
    fn done(&mut self, done: Done) -> Option<&'static str> {
        match done {
            Done::Attach { session, size } => self.attach(session, size),
            Done::PastePlan(Some(Paste::Fetch(reps))) => {
                tracing::debug!(client = %self.client, reps = reps.len(), "paste waits for the clipboard");
                for rep in reps {
                    self.post(WorkerMsg::Clip(ClipMsg::Fetch { rep, max: None, urgent: true }));
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
                self.screen_order.forget(id);
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
                tracing::debug!(
                    client = %self.client,
                    generation = offer.generation,
                    items = offer.items.len(),
                    age_ms = offer.age_ms,
                    concealed = offer.concealed,
                    "client clipboard offered"
                );
                if self.daemon.clip.offered(self.link, offer, std::time::Instant::now()) {
                    let (clip, link) = (Arc::clone(&self.daemon.clip), self.link);
                    // Off the runtime: a write waits on the pasteboard server.
                    self.tasks.spawn(async move {
                        let _mirrored =
                            tokio::task::spawn_blocking(move || clip.mirror(link)).await;
                    });
                }
            }
            ClipMsg::Fetch { rep, max, urgent } => {
                let (daemon, conn, out) =
                    (self.daemon.clone(), self.conn.clone(), self.out.clone());
                self.tasks.spawn(async move {
                    crate::xfer::send_clip(&daemon, &conn, &out, rep, max, urgent).await;
                });
            }
            ClipMsg::Data { rep: RepRef { source: Source::Drag(drag), item, kind }, bytes }
                if self.daemon.dnd.term_session(drag).is_some() =>
            {
                self.term_drag_data(drag, DragData::Whole { item, kind, bytes });
            }
            ClipMsg::TooBig { rep: RepRef { source: Source::Drag(drag), item, kind }, size }
                if self.daemon.dnd.term_session(drag).is_some() =>
            {
                tracing::debug!(client = %self.client, item, size, "drag data too big for a drop");
                self.term_drag_data(drag, DragData::Gone { item, kind });
            }
            ClipMsg::Unavailable { source: Source::Drag(drag) }
                if self.daemon.dnd.term_session(drag).is_some() =>
            {
                self.term_drag_data(drag, DragData::AllGone);
            }
            ClipMsg::Data { rep, bytes } => self.clip_data(&rep, Some(bytes)),
            ClipMsg::TooBig { rep, size } => {
                tracing::debug!(client = %self.client, item = rep.item, size, "client clipboard too big");
                if self.daemon.clip.refused(self.link, &rep) {
                    self.write_held();
                }
            }
            ClipMsg::Unavailable { source } => {
                tracing::debug!(client = %self.client, "client clipboard gone");
                self.daemon.clip.unavailable(self.link, source);
                self.write_held();
            }
        }
    }

    /// A representation of the client's clipboard arrived, or its stream was refused or cut
    /// (`None`); the held paste goes on once all it waits for is here or not coming.
    fn clip_data(&mut self, rep: &RepRef, bytes: Option<Vec<u8>>) {
        let whole = match bytes {
            Some(bytes) => self.daemon.clip.supply(self.link, rep, bytes),
            None => self.daemon.clip.refused(self.link, rep),
        };
        if whole {
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
            let write = move || clip.write_incoming(link, std::time::Instant::now());
            let _written = tokio::task::spawn_blocking(write).await;
            let _sent = done.send(Done::PasteWritten);
        });
    }

    /// Send the held input on to its windows and shells, in the order it came.
    fn release_held(&mut self) {
        let Some(held) = self.held.take() else { return };
        for input in held.inputs {
            self.deliver(input);
        }
    }

    /// Input for a window or a shell. A paste first puts the client's clipboard on the
    /// pasteboard; until it is there, input for that window or shell waits behind it.
    fn input(&mut self, input: Input) {
        match route(&mut self.held, input) {
            Route::Now(input) => self.deliver(input),
            Route::Held => {}
            Route::Began(kind) => {
                let (clip, link) = (Arc::clone(&self.daemon.clip), self.link);
                let done = self.done.clone();
                self.tasks.spawn(async move {
                    let plan = tokio::task::spawn_blocking(move || clip.paste(link, kind)).await;
                    let _sent = done.send(Done::PastePlan(plan.ok()));
                });
            }
        }
    }

    /// Hand one input to its window or its session's actor.
    fn deliver(&self, input: Input) {
        match input {
            Input::Window(stream, input) => self.command(stream, Command::Input(input)),
            Input::Term(session, req) => self.request(session, req),
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
                    Dest::Staging | Dest::Path(_) | Dest::Attachment | Dest::Drag(_) => None,
                };
                let (daemon, client, out) = (self.daemon.clone(), self.client, self.out.clone());
                let begin = move |cwd: Option<String>| {
                    tracing::info!(%client, %xfer, ?dest, ?cwd, files, bytes, "upload begins");
                    let begun = daemon.transfers.begin(xfer, &dest, cwd.as_deref(), files);
                    // Begun again from a link after the one that heard it end: told again.
                    if let Begun::Finished(finished) = begun {
                        let paths = finished.paths.iter().map(|p| p.to_string_lossy().into_owned());
                        let msg = XferMsg::Finished { xfer, paths: paths.collect() };
                        if let Err(e) = out.try_send(WorkerMsg::Xfer(msg)) {
                            tracing::debug!(%client, %xfer, error = %e, "an upload's end not told");
                        }
                    }
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
                    // Asked from the client's next link, it may come ahead of the transfer's
                    // `Begin` there, which waits on the session's directory.
                    let _begun = transfers.begun(xfer, crate::xfer::BEGIN_WAIT).await;
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
                let (conn, out) = (self.conn.clone(), self.out.clone());
                let task = crate::xfer::download(conn, out, xfer, path, held);
                if let Some(earlier) = self.downloads.insert(xfer, tokio::spawn(task)) {
                    earlier.abort();
                }
            }
            // The client could not read a file of a drag's upload: the drop cannot land whole.
            XferMsg::Failed { xfer, name, error }
                if let Some(drag) = self.daemon.transfers.drag_of(xfer)
                    && self.daemon.dnd.term_session(drag).is_none() =>
            {
                let file = name.unwrap_or_else(|| "a file".to_owned());
                let heard = slopty_worker::screen::drag::Heard::Failed(format!("{file}: {error}"));
                self.daemon.dnd.drags().tell(drag, heard);
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

    /// Number a new stream and register it: its id, what its task needs, and its commands.
    fn new_stream(&mut self) -> (StreamId, crate::screens::Link, mpsc::UnboundedReceiver<Command>) {
        let id = StreamId(self.next_stream);
        self.next_stream = self.next_stream.wrapping_add(1).max(1);
        let (commands, rx) = mpsc::unbounded_channel();
        self.screens.insert(id, Screen { commands, control: None });
        let sound = self.sound.get_or_insert_with(|| crate::screens::sound(&self.conn));
        let link = crate::screens::Link {
            daemon: self.daemon.clone(),
            client: self.client,
            conn: self.conn.clone(),
            out: self.out.clone(),
            told: self.told.clone(),
            sound: Arc::clone(sound),
        };
        (id, link, rx)
    }

    fn screen(&mut self, req: ScreenRequest) {
        match req {
            ScreenRequest::List => {
                let out = self.out.clone();
                let client = self.client;
                self.tasks.spawn(async move {
                    let event = listing().await.unwrap_or_else(|e| {
                        tracing::warn!(%client, error = %e, "screen listing");
                        ScreenEvent::ListFailed { why: e.failure() }
                    });
                    let _sent = out.send(WorkerMsg::Screen(event)).await;
                });
            }
            ScreenRequest::Open { target, quality } => {
                let (id, link, rx) = self.new_stream();
                self.streams.spawn(crate::screens::run(link, id, target, quality, rx));
            }
            ScreenRequest::OpenDisplay { key, shape, quality } => {
                let (id, link, rx) = self.new_stream();
                let asked = slopty_worker::screen::sized::Asked { key, shape, quality };
                self.streams.spawn(crate::screens::run_display(link, id, asked, rx));
            }
            ScreenRequest::Close(id) => {
                self.screen_order.forget(id);
                if let Some(screen) = self.screens.remove(&id) {
                    let _gone = screen.commands.send(Command::Close);
                }
            }
            ScreenRequest::SetQuality { stream, quality } => {
                if self.screens.contains_key(&stream) {
                    self.screen_order.stream(stream, true);
                }
                self.command(stream, Command::SetQuality(quality));
            }
            ScreenRequest::SoundReport(report) => {
                if let Some(sound) = &self.sound {
                    sound.report(report);
                }
            }
            ScreenRequest::Report { stream, report } => {
                if let Some(control) = self.media_control(stream) {
                    let asked = Asked { stream, control, report };
                    if self.reports.try_send(asked).is_err() {
                        tracing::debug!(client = %self.client, %stream, "reports backed up; one dropped");
                    }
                }
            }
            ScreenRequest::Input { stream, input } => {
                // Only a live stream's input is numbered; any other still goes the way input
                // goes (a paste chord still fetches the clipboard), and reaches no window.
                let known = self.screens.contains_key(&stream);
                if known && !self.screen_order.stream(stream, input.in_order()) {
                    tracing::trace!(client = %self.client, %stream, "window input: its copy came first");
                    return;
                }
                self.input(Input::Window(stream, input));
            }
            ScreenRequest::Focus(stream) => self.command(stream, Command::Focus),
            ScreenRequest::Focused { stream, focused } => {
                self.command(stream, Command::Focused(focused));
            }
            ScreenRequest::Resize { stream, width, height, scale } => {
                self.command(stream, Command::Resize { width, height, scale });
            }
        }
    }

    /// A datagram copy of an input: applied now if it is the session's next, else left to its
    /// stream copy.
    fn input_copy(&mut self, copy: InputCopy) {
        match copy {
            InputCopy::Term { session, seq, req } => {
                if self.order.copy(session, seq) {
                    tracing::trace!(client = %self.client, %session, seq, "term input copy received");
                    self.term(session, req);
                }
            }
            InputCopy::Window { stream, seq, ordered, input } => {
                if !self.screens.contains_key(&stream) {
                    return;
                }
                if self.screen_order.copy(stream, seq, ordered, input.in_order()) {
                    tracing::trace!(client = %self.client, %stream, seq, "window input copy received");
                    self.input(Input::Window(stream, input));
                }
            }
        }
    }

    /// The control for feedback on `media`, a stream's own id or its lower stripe's
    /// ([`Stripe::media_of`]): a stripe's NACKs, refreshes and reports are answered by it alone.
    fn media_control(&self, media: StreamId) -> Option<StreamControl> {
        let (stream, _stripe) = Stripe::stream_of(media);
        let control = self.screens.get(&stream).and_then(|s| s.control.as_ref())?;
        Some(control.of_media(media))
    }

    /// Feedback from a datagram that arrived `at`: retransmit, refresh, or answer a clock probe.
    fn feedback(&self, feedback: Feedback, at: std::time::Instant) {
        let control = |stream| self.media_control(stream);
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
            Feedback::Clock { stream, sent_us } => {
                if let Some(control) = control(stream) {
                    control.clock(sent_us, at);
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
        match &req {
            TermRequest::Focus { focused } => self.focus(session, *focused),
            TermRequest::Detach => self.focus(session, false),
            input if input.is_input() => {
                self.daemon.handoffs.lock().typed(session, self.client, std::time::Instant::now());
            }
            _ => {}
        }
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
                        Err(e) => report(&out, client, session, e.term_error()).await,
                    }
                });
            }
            TermRequest::DragEnter { drag, items } => self.drag_enter(session, drag, items),
            TermRequest::DragLeave => {
                self.daemon.dnd.term_left(session, self.client);
                self.request(session, TermRequest::DragLeave);
            }
            input if input.is_input() => self.input(Input::Term(session, input)),
            other => self.request(session, other),
        }
    }

    /// This client's drag `drag` entered `session`'s terminal: its data is routed there, and
    /// what its program asks for is fetched over this connection.
    fn drag_enter(&self, session: SessionId, drag: DragId, items: Vec<DragItem>) {
        self.daemon.dnd.term_drag(drag, session, self.client);
        let entered = self
            .daemon
            .worker
            .get(session)
            .and_then(|h| h.drag_enter(self.client, drag, items, self.out.clone()));
        if let Err(e) = entered {
            self.fail(session, &e);
        }
    }

    /// `data` of drag `drag` goes to the terminal it is over, if it is a terminal's.
    fn term_drag_data(&self, drag: DragId, data: DragData) {
        let session = self.daemon.dnd.term_session(drag);
        if let Some(h) = session.and_then(|s| self.daemon.worker.get(s).ok()) {
            let _gone = h.drag_data(drag, data);
        }
    }

    /// This client focused on `session`'s tile, or let go of it: the handoffs learn who is in
    /// front of the session, and its presence file follows.
    fn focus(&self, session: SessionId, focused: bool) {
        let changed = self.daemon.handoffs.lock().focus(session, self.client, focused);
        if let Some(present) = changed {
            let _sent = self.daemon.presence.send((session, present));
        }
    }

    /// Hand `req` to `session`'s actor.
    fn request(&self, session: SessionId, req: TermRequest) {
        let outcome = self.daemon.worker.get(session).and_then(|h| h.request(self.client, req));
        if let Err(e) = outcome {
            self.fail(session, &e);
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
    use slopty_proto::terminal::{CloseReason, RepoChanges, SessionState, SessionSummary};

    use super::{Heard, Input, InputOrder, Route, ScreenOrder, StreamId, route};

    fn key(code: KeyCode, mods: Mods) -> ScreenInput {
        ScreenInput::Key { code, action: KeyAction::Press, mods }
    }

    fn opened(id: SessionId) -> WorkerMsg {
        WorkerMsg::SessionChanged(summary(id))
    }

    fn summary(id: SessionId) -> SessionSummary {
        SessionSummary {
            id,
            title: String::new(),
            cwd: None,
            repo: None,
            branch: None,
            changes: None,
            started_ms: slopty_core::WallMs::ZERO,
            cols: 80,
            rows: 24,
            state: SessionState::Running,
            viewers: 0,
            command: Vec::new(),
            agent: None,
            progress: None,
            restored: None,
            repo_id: None,
        }
    }

    fn delta(version: u64) -> WorkerMsg {
        let op = ItemOp::Remove(ItemId::new());
        WorkerMsg::Items(ItemSync::Delta { version, by: ClientId::new(), op })
    }

    /// The events are subscribed to before the greeting is read, so one that happened in
    /// between arrives after it: what the greeting (or a resync) already told is not told
    /// again, and what it did not is, a summary whose working tree moved included.
    #[test]
    fn an_event_the_greeting_carried_is_not_told_again() {
        let (told, new) = (SessionId::new(), SessionId::new());
        let mut heard =
            Heard { sessions: HashMap::from([(told, summary(told))]), ..Heard::default() };
        let snapshot = WorkerMsg::Items(ItemSync::Snapshot { version: 5, items: Vec::new() });
        assert!(heard.admit(&snapshot));

        assert!(!heard.admit(&opened(told)), "in the HelloAck");
        assert!(!heard.admit(&delta(5)), "in the snapshot");
        assert!(heard.admit(&delta(6)));
        assert!(heard.admit(&opened(new)));
        assert!(!heard.admit(&opened(new)), "told once");
        let mut exited = opened(new);
        if let WorkerMsg::SessionChanged(summary) = &mut exited {
            summary.state = SessionState::Exited { status: 3 };
        }
        assert!(heard.admit(&exited), "its program exited: the new state is news");
        assert!(!heard.admit(&exited), "and told once");
        let mut edited = exited.clone();
        if let WorkerMsg::SessionChanged(summary) = &mut edited {
            summary.changes = Some(RepoChanges { files: 1, added: 3, removed: 0 });
        }
        assert!(heard.admit(&edited), "its working tree moved");
        let closed = WorkerMsg::SessionClosed { session: told, reason: CloseReason::Exited };
        assert!(heard.admit(&closed));
        assert_eq!(heard.sessions.keys().collect::<Vec<_>>(), [&new]);
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

    /// A window's copy is applied once and its stream copy skipped; a click or key never goes
    /// ahead of one before it, and a move never ahead of a click or key before it nor after
    /// anything newer.
    #[test]
    fn a_window_input_copy_applies_once_and_clicks_and_keys_only_in_turn() {
        let (a, b) = (StreamId(1), StreamId(2));
        let mut order = ScreenOrder::default();
        // Stream a: 1 move, 2 click, 3 move, 4 move, 5 key; stream b: 1 key.
        // (from the stream, stream, in order, seq, ordered), and whether it is applied.
        let arrivals = [
            (false, a, true, 2, 1, true), // the click's copy: the move before it can wait
            (true, a, false, 1, 0, false), // that move, from the stream: older than the click
            (false, a, true, 5, 2, true), // the key's copy, next in turn
            (false, a, false, 4, 1, false), // an older move than the key
            (false, b, true, 1, 1, true), // another stream counts apart
            (true, a, true, 2, 1, false), // the click from the stream: its copy came
            (true, a, false, 3, 1, false), // older than the key
            (true, a, false, 4, 1, false),
            (true, a, true, 5, 2, false),  // the key from the stream
            (false, a, true, 5, 2, false), // a duplicate copy
            (true, b, true, 1, 1, false),
        ];
        for (i, (stream_copy, stream, in_order, seq, ordered, expected)) in
            arrivals.into_iter().enumerate()
        {
            let goes = if stream_copy {
                order.stream(stream, in_order)
            } else {
                order.copy(stream, seq, ordered, in_order)
            };
            assert_eq!(goes, expected, "arrival {i}");
        }
        assert_eq!((order.taken, order.late), (3, 2));

        // A key waits for the click before it; a move waits for the key before it.
        let mut order = ScreenOrder::default();
        assert!(!order.copy(a, 2, 2, true), "the key after a click not yet here");
        assert!(!order.copy(a, 3, 2, false), "a move after that key");
        assert!(order.stream(a, true), "the click");
        assert!(order.copy(a, 2, 2, true), "the key, once its turn came again");
        assert!(order.copy(a, 3, 2, false), "and the move after it");
    }

    /// Inputs sent through a link that loses and delays the stream and races datagram copies
    /// against it, in many random orders: every click and key reaches the window once and in
    /// order, the pointer never goes back, and the last move always lands.
    #[test]
    fn window_input_through_any_race_lands_in_order_and_never_goes_back() {
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut draw = |below: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % below
        };
        for _round in 0..500 {
            let count = 1 + draw(40);
            // (seq, ordered, in order) as the client numbers them.
            let mut inputs = Vec::new();
            let mut ordered = 0;
            for seq in 1..=count {
                let in_order = draw(3) == 0;
                if in_order {
                    ordered += 1;
                }
                inputs.push((seq, ordered, in_order));
            }
            // (arrival time, from the stream, input); the stream keeps its order and holds
            // everything behind a lost packet.
            let mut events = Vec::new();
            let mut stream_at = 0;
            for (i, &input) in inputs.iter().enumerate() {
                let sent = u64::try_from(i).unwrap() * 10;
                let held = if draw(5) == 0 { 50 + draw(100) } else { 0 };
                stream_at = (sent + 1 + held).max(stream_at);
                events.push((stream_at, true, input));
                if draw(4) != 0 {
                    events.push((sent + 2 + draw(60), false, input));
                }
            }
            events.sort_by_key(|&(at, from_stream, _)| (at, !from_stream));
            let mut order = ScreenOrder::default();
            let stream = StreamId(1);
            let mut applied = Vec::new();
            for (_at, from_stream, (seq, ordered, in_order)) in events {
                let goes = if from_stream {
                    order.stream(stream, in_order)
                } else {
                    order.copy(stream, seq, ordered, in_order)
                };
                if goes {
                    applied.push((seq, in_order));
                }
            }
            assert!(applied.windows(2).all(|w| w[0].0 < w[1].0), "went back: {applied:?}");
            let clicks: Vec<u64> =
                applied.iter().filter(|(_, in_order)| *in_order).map(|(seq, _)| *seq).collect();
            let sent: Vec<u64> = inputs.iter().filter(|i| i.2).map(|i| i.0).collect();
            assert_eq!(clicks, sent, "every click and key once, in order");
            assert_eq!(applied.last().map(|a| a.0), Some(count), "the last input lands");
        }
    }

    /// A paste holds only the window it went to, in order; another window's input goes on, and a
    /// second paste joins the hold behind the first. The release hands the held input back in the
    /// order it came. A paste is the chord the client named one, wherever its key sits: ⌘ at the
    /// V position is a Dvorak ⌘K, and holds nothing.
    #[test]
    fn a_paste_holds_its_own_window_and_no_other() {
        let (a, b, c) = (StreamId(1), StreamId(2), StreamId(3));
        let paste = |s| {
            Input::Window(s, ScreenInput::PasteChord { code: KeyCode::Period, mods: Mods::SUPER })
        };
        let typed = |s, code| Input::Window(s, key(code, Mods::empty()));
        let mut held = None;

        assert_eq!(route(&mut held, typed(a, KeyCode::A)), Route::Now(typed(a, KeyCode::A)));
        assert_eq!(
            route(&mut held, paste(a)),
            Route::Began(slopty_worker::clip::PasteKind::Window)
        );
        assert_eq!(route(&mut held, typed(a, KeyCode::B)), Route::Held, "behind the chord");
        assert_eq!(route(&mut held, typed(b, KeyCode::C)), Route::Now(typed(b, KeyCode::C)));
        assert_eq!(route(&mut held, paste(c)), Route::Held, "a second paste waits too");
        assert_eq!(route(&mut held, typed(c, KeyCode::D)), Route::Held);
        assert_eq!(route(&mut held, typed(b, KeyCode::E)), Route::Now(typed(b, KeyCode::E)));
        let at_v = || Input::Window(b, key(KeyCode::V, Mods::SUPER));
        assert_eq!(route(&mut held, at_v()), Route::Now(at_v()), "a key at V is not a paste");

        let released = held.take().map(|h| h.inputs).unwrap_or_default();
        assert_eq!(released, [paste(a), typed(a, KeyCode::B), paste(c), typed(c, KeyCode::D)],);
    }

    /// A shell's paste of a picture holds that shell's input behind it, in order, as a
    /// window's ⌘V holds the window's: a keystroke typed after ⌃V cannot reach Claude Code
    /// before the picture is on the pasteboard. Another shell, and a plain paste of text, go on.
    #[test]
    fn a_picture_paste_holds_its_shells_input_behind_it() {
        use slopty_proto::terminal::{PasteChord, TermRequest};

        let (a, b) = (SessionId::new(), SessionId::new());
        let picture = |s| Input::Term(s, TermRequest::PastePicture(PasteChord::Command));
        let raw = |s, text: &str| Input::Term(s, TermRequest::Raw(text.as_bytes().to_vec()));
        let text =
            |s| Input::Term(s, TermRequest::Paste { text: "words".to_owned(), confirmed: false });
        let mut held = None;

        assert_eq!(route(&mut held, text(a)), Route::Now(text(a)), "text pastes at once");
        assert_eq!(
            route(&mut held, picture(a)),
            Route::Began(slopty_worker::clip::PasteKind::Picture)
        );
        assert_eq!(route(&mut held, raw(a, "x")), Route::Held, "behind the picture");
        assert_eq!(route(&mut held, raw(b, "y")), Route::Now(raw(b, "y")), "another shell");
        let window = || Input::Window(StreamId(1), key(KeyCode::A, Mods::empty()));
        assert_eq!(route(&mut held, window()), Route::Now(window()), "a window");
        assert_eq!(route(&mut held, text(a)), Route::Held, "in order");

        let released = held.take().map(|h| h.inputs).unwrap_or_default();
        assert_eq!(released, [picture(a), raw(a, "x"), text(a)]);
    }
}

/// Window input from the real client link through a lossy shaper to the worker's datagram
/// reader and its ordering, and on through a stream's task to the point its input thread would
/// take each event. The stream runs on the test platform of `crate::screens::fake`: nothing
/// reaches the screen or the worker's input. The loop here does what [`Peer::screen`] and
/// [`Peer::input_copy`] do with window input, without a daemon behind it.
#[cfg(test)]
#[cfg(target_vendor = "apple")]
#[expect(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "measurement arithmetic on small counts"
)]
mod lossy {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use slopty_core::{ClientId, StreamId, WorkerId};
    use slopty_net::worker::{AcceptedClient, WorkerListener};
    use slopty_net::{ClientMsg, WorkerMsg};
    use slopty_proto::handshake::{Hello, HelloAck};
    use slopty_proto::input::{Mods, MouseButton};
    use slopty_proto::screen::{CaptureTarget, Quality, ScreenInput, ScreenRequest};
    use slopty_worker::screen::Pipeline;
    use tokio::sync::mpsc;

    use super::{COPY_DEPTH, FEEDBACK_DEPTH, InputCopy, ScreenOrder, read_datagrams};
    use crate::screens::fake::{Fake, Gated, Nowhere, Queued, note};
    use crate::screens::{Command, serve};

    const STREAM: StreamId = StreamId(1);

    /// How the inputs come.
    #[derive(Clone, Copy, Debug)]
    enum Pattern {
        /// A pointer moving at about 160 Hz, with a press or a release every tenth input.
        Drag,
        /// A click every 50 ms and nothing between, as keys typed into a window come: each is a
        /// lone packet, and nothing behind it tells QUIC it was lost.
        Clicks,
    }

    impl Pattern {
        const fn every(self) -> Duration {
            match self {
                Self::Drag => Duration::from_millis(6),
                Self::Clicks => Duration::from_millis(50),
            }
        }

        /// The input sent `i`-th. Its x is its index, which is how its hand-over is matched to
        /// its send.
        fn input(self, i: usize) -> ScreenInput {
            let x = i as f32;
            let button = |down| ScreenInput::Button {
                button: MouseButton::Left,
                down,
                x,
                y: 0.0,
                clicks: 1,
                mods: Mods::empty(),
            };
            match (self, i % 10) {
                (Self::Drag, 4) => button(true),
                (Self::Drag, 5) => button(false),
                (Self::Drag, _) => ScreenInput::Move { x, y: 0.0 },
                (Self::Clicks, _) => button(i.is_multiple_of(2)),
            }
        }
    }

    /// What one run measured.
    struct Run {
        /// Sent → handed to the input thread, per click and per move that was handed over.
        clicks: Vec<Duration>,
        moves: Vec<Duration>,
        /// Moves the ordering passed over for a newer one.
        moves_skipped: usize,
        taken: u64,
        late: u64,
        carried: slopty_shape::relay::Carried,
    }

    const fn x_of(input: &ScreenInput) -> f32 {
        match input {
            ScreenInput::Move { x, .. } | ScreenInput::Button { x, .. } => *x,
            _other => -1.0,
        }
    }

    /// The worker's end: the datagram reader, the ordering, and the stream's commands.
    async fn worker(
        accepted: AcceptedClient,
        commands: mpsc::UnboundedSender<Command>,
    ) -> (u64, u64) {
        let AcceptedClient { conn, mut rx, .. } = accepted;
        let (feedback, _feedback) = mpsc::channel(FEEDBACK_DEPTH);
        let (copies_tx, mut copies) = mpsc::channel(COPY_DEPTH);
        tokio::spawn(read_datagrams(conn.clone(), feedback, copies_tx));
        let mut order = ScreenOrder::default();
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Ok(ClientMsg::Screen(ScreenRequest::Input { stream, input })) => {
                        if order.stream(stream, input.in_order()) {
                            let _gone = commands.send(Command::Input(input));
                        }
                    }
                    Ok(_other) => {}
                    Err(_closed) => break,
                },
                Some(copy) = copies.recv() => {
                    if let InputCopy::Window { stream, seq, ordered, input } = copy
                        && order.copy(stream, seq, ordered, input.in_order())
                    {
                        let _gone = commands.send(Command::Input(input));
                    }
                }
            }
        }
        (order.taken, order.late)
    }

    /// Send `count` inputs through a shaper of `link` and time each to its hand-over.
    async fn run(display: u32, pattern: Pattern, count: usize, link: slopty_shape::Link) -> Run {
        let mut queued = note(display);
        let sink: Arc<dyn slopty_worker::DatagramSink> = Arc::new(Nowhere);
        let (mut stream, _opened) = Pipeline::<Fake<Gated>>::open(
            STREAM,
            CaptureTarget::Display(slopty_core::DisplayId(display)),
            Quality::default(),
            sink,
            |_event| {},
        )
        .await
        .unwrap();
        let (commands, mut commanded) = mpsc::unbounded_channel();
        let (out, mut told) = mpsc::channel(64);
        tokio::spawn(async move { while told.recv().await.is_some() {} });
        let serving = tokio::spawn(async move {
            let claim = crate::screens::fake::unsourced();
            serve(&mut stream, ClientId::new(), &mut commanded, &out, None, &claim, None).await;
            stream.close().await;
        });

        let listener = WorkerListener::bind(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            slopty_net::admission::Admission::default(),
        )
        .unwrap();
        let worker_addr = listener.local_addr().unwrap();
        let endpoint = slopty_net::client::bind_client().unwrap();
        // A handshake that the loss starves is given up and tried again over a shaper with
        // other draws: what is measured is the connection's life after it.
        let mut attempt = 0;
        let (relay, shaping, dialed, mut accepted) = loop {
            attempt += 1;
            assert!(attempt <= 8, "no handshake got through in 8 tries");
            let seed = u64::from(display) * 100 + attempt;
            let at = SocketAddr::from(([0, 0, 0, 0], 0));
            let relay = Arc::new(
                slopty_shape::relay::Relay::bind(at, worker_addr, link, seed).await.unwrap(),
            );
            let relay_addr = relay.addr().unwrap();
            let shaping = tokio::spawn({
                let relay = Arc::clone(&relay);
                async move { relay.run().await }
            });
            let hello = Hello { client: ClientId::new(), name: "lossy".to_owned() };
            let dialing = endpoint.clone();
            let dialed = tokio::spawn(async move {
                slopty_net::client::connect_addr(&dialing, relay_addr, hello).await
            });
            if let Ok(Some(accepted)) =
                tokio::time::timeout(Duration::from_secs(6), listener.accept()).await
            {
                break (relay, shaping, dialed, accepted);
            }
            dialed.abort();
            shaping.abort();
        };
        let ack = HelloAck {
            settings: String::new(),
            worker: WorkerId::new(),
            name: "lossy".to_owned(),
            home: String::new(),
            caps: slopty_proto::server::WorkerCaps::bare(slopty_proto::server::Os::MacOs),
            load: 0.0,
            sessions: Vec::new(),
        };
        accepted.tx.send(&WorkerMsg::HelloAck(ack)).await.unwrap();
        let worker_end = tokio::spawn(worker(accepted, commands));
        let mut link = slopty_client::WorkerLink::start(dialed.await.unwrap().unwrap());

        let sender = link.sender();
        let mut sent = Vec::with_capacity(count);
        let mut ticks = tokio::time::interval(pattern.every());
        for i in 0..count {
            ticks.tick().await;
            sent.push(Instant::now());
            let input = pattern.input(i);
            let msg = ClientMsg::Screen(ScreenRequest::Input { stream: STREAM, input });
            sender.send(msg).await.unwrap();
        }
        let mut handed: Vec<Option<Instant>> = vec![None; count];
        let mut order = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while handed.last().is_some_and(Option::is_none) {
            let Ok(Some(Queued { at, input, .. })) =
                tokio::time::timeout_at(deadline, queued.recv()).await
            else {
                panic!("the last input never arrived; {} of {count} did", order.len());
            };
            let i = x_of(&input) as usize;
            assert!(handed[i].is_none(), "input {i} handed over twice");
            handed[i] = Some(at);
            order.push(i);
        }
        assert!(order.windows(2).all(|w| w[0] < w[1]), "out of order: {order:?}");
        let (mut clicks, mut moves, mut moves_skipped) = (Vec::new(), Vec::new(), 0);
        for (i, at) in handed.iter().enumerate() {
            let is_move = !pattern.input(i).in_order();
            match (at, is_move) {
                (Some(at), true) => moves.push(at.saturating_duration_since(sent[i])),
                (Some(at), false) => clicks.push(at.saturating_duration_since(sent[i])),
                (None, true) => moves_skipped += 1,
                (None, false) => panic!("click {i} never handed over"),
            }
        }
        let carried = relay.carried().await;
        let mut worst: Vec<(Duration, usize)> = handed
            .iter()
            .enumerate()
            .filter_map(|(i, at)| Some((at.as_ref()?.saturating_duration_since(sent[i]), i)))
            .collect();
        worst.sort_unstable();
        let worst: Vec<String> = worst
            .iter()
            .rev()
            .take(8)
            .map(|(d, i)| format!("#{i} {:.0} ms", d.as_secs_f64() * 1e3))
            .collect();
        eprintln!("DIAG worst {worst:?}; client path {}", link.health());
        link.close();
        let (taken, late) = worker_end.await.unwrap();
        shaping.abort();
        endpoint.close(0_u32.into(), b"done");
        serving.abort();
        Run { clicks, moves, moves_skipped, taken, late, carried }
    }

    fn describe(label: &str, samples: &mut [Duration]) -> String {
        samples.sort_unstable();
        let at = |q: f64| {
            let i = ((samples.len().saturating_sub(1)) as f64 * q).round() as usize;
            samples.get(i).map_or(0.0, |d| d.as_secs_f64() * 1e3)
        };
        let over = samples.iter().filter(|d| **d > Duration::from_millis(50)).count();
        format!(
            "{label} {}: p50 {:.1} / p90 {:.1} / p99 {:.1} / max {:.1} ms, over 50 ms {:.1} %",
            samples.len(),
            at(0.5),
            at(0.9),
            at(0.99),
            at(1.0),
            over as f64 * 100.0 / samples.len().max(1) as f64,
        )
    }

    /// The shaped loopback of the keystroke copies' measurement: 8 ms round trip, a share of
    /// packets lost each way (`SLOPTY_E2E_LOSS`, default 0.13). `SLOPTY_E2E_PATTERN=clicks`
    /// sends lone clicks instead of a drag. Run once as it is and once with
    /// `SLOPTY_ECHO_COPY=off`, which sends no copies: the control stream alone. Prints the
    /// hand-over times and the eight worst inputs with the client's path at the end.
    /// `window input through a lossy link` in MEASUREMENTS.md.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "measurement"]
    async fn window_input_through_a_lossy_link() {
        let loss: f32 =
            std::env::var("SLOPTY_E2E_LOSS").ok().and_then(|v| v.parse().ok()).unwrap_or(0.13);
        let copies = slopty_net::echo::Copies::from_env().is_some();
        let link = slopty_shape::Link {
            delay: Duration::from_millis(4),
            loss,
            ..slopty_shape::Link::CLEAR
        };
        let pattern = match std::env::var("SLOPTY_E2E_PATTERN").as_deref() {
            Ok("clicks") => Pattern::Clicks,
            _drag => Pattern::Drag,
        };
        let count = match pattern {
            Pattern::Drag => 1500,
            Pattern::Clicks => 200,
        };
        let mut run = run(21, pattern, count, link).await;
        eprintln!(
            "MEASURE window input, {pattern:?}, loss {:.0} % each way, copies {}: {}; {}; {} moves \
             passed over; copies taken {} late {}; relay {:?}",
            loss * 100.0,
            if copies { "on" } else { "off" },
            describe("clicks", &mut run.clicks),
            describe("moves", &mut run.moves),
            run.moves_skipped,
            run.taken,
            run.late,
            run.carried
        );
    }

    /// Through a link that loses a fifth of its packets each way, every click reaches the
    /// window once and in order, nothing is handed over out of order or twice, the last move
    /// lands, and some input was taken from its copy.
    #[tokio::test(flavor = "multi_thread")]
    async fn window_input_through_a_lossy_link_lands_once_in_order() {
        let link = slopty_shape::Link {
            delay: Duration::from_millis(4),
            loss: 0.2,
            ..slopty_shape::Link::CLEAR
        };
        let run = run(22, Pattern::Drag, 300, link).await;
        assert!(
            run.carried.up.lost > 0 && run.carried.down.lost > 0,
            "the link lost packets: {:?}",
            run.carried
        );
        if slopty_net::echo::Copies::from_env().is_some() {
            assert!(run.taken > 0, "no input was taken from its copy (late {})", run.late);
        }
        assert_eq!(run.clicks.len(), 60, "every click");
    }
}
