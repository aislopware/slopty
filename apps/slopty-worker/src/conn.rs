//! One client connection.
//!
//! The loop here only routes. A terminal's keys, a window's input and the client's loss feedback
//! are handled where they arrive; everything that waits on something slow (a window stream, the
//! disk, ptyd, the pasteboard) runs on a task of its own and reports back through [`Done`]. What
//! one request costs therefore never lands on another terminal's echo (MEASUREMENTS.md, "echo
//! behind slow requests").

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use slopty_core::{ClientId, SessionId, StreamId, XferId};
use slopty_net::worker::AcceptedClient;
use slopty_net::{ClientMsg, Connection, NetError, WorkerMsg};
use slopty_proto::PROTOCOL_VERSION;
use slopty_proto::handshake::{Caps, HelloAck};
use slopty_proto::input::{KeyAction, KeyCode, Mods};
use slopty_proto::items::ItemSync;
use slopty_proto::screen::{Feedback, ScreenInput, ScreenRequest};
use slopty_proto::terminal::{CloseReason, TermEvent, TermRequest, TermSize};
use slopty_proto::transfer::{ClipMsg, Dest, XferMsg};
use slopty_worker::WorkerError;
use slopty_worker::clip::Paste;
use slopty_worker::screen::{StreamControl, listing};
use slopty_worker::session::Outbound;
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};

use crate::Daemon;
use crate::screens::{Command, Told};

/// The weak end of a session's sink, as the connection keeps it.
type WeakClientSink = mpsc::WeakSender<Outbound>;

/// Events buffered per attached session before the client is considered stuck.
const SINK_DEPTH: usize = 256;
/// Outbound control messages buffered before the writer applies backpressure.
const CONTROL_DEPTH: usize = 1024;
/// Loss feedback datagrams buffered between the reader and the peer loop.
const FEEDBACK_DEPTH: usize = 256;
/// Clipboard representations the client sent up as streams, queued for the peer loop.
const CLIP_DEPTH: usize = 8;
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

    let ack = HelloAck {
        protocol: PROTOCOL_VERSION,
        worker: daemon.id,
        name: daemon.name.clone(),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        caps: Caps::empty(),
        sessions: daemon.worker.summaries().await,
    };
    let sessions: HashSet<SessionId> = ack.sessions.iter().map(|s| s.id).collect();
    out.send(WorkerMsg::HelloAck(ack)).await.map_err(|_gone| NetError::Closed)?;
    out.send(WorkerMsg::Items(daemon.items.snapshot())).await.map_err(|_gone| NetError::Closed)?;
    // Collected first: the lock must not be held across the sends.
    let agents = daemon.agents.lock().snapshot();
    for event in agents {
        out.send(WorkerMsg::Agent(event)).await.map_err(|_gone| NetError::Closed)?;
    }

    let mut events = daemon.events.subscribe();
    let mut tasks = JoinSet::new();
    tasks.spawn(slopty_net::endpoint::trace_path_health(conn.clone(), "worker"));
    let (feedback_tx, mut feedback_rx) = mpsc::channel::<Feedback>(FEEDBACK_DEPTH);
    tasks.spawn(read_feedback(conn.clone(), feedback_tx));
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
    let known = daemon.ports.lock().known();
    for (session, ports) in known {
        out.send(WorkerMsg::Ports { session, ports }).await.map_err(|_gone| NetError::Closed)?;
    }
    let (done, mut done_rx) = mpsc::unbounded_channel();
    let (told, mut told_rx) = mpsc::unbounded_channel();
    let link = conn.stable_id();
    let mut peer = Peer {
        daemon,
        conn,
        client: hello.client,
        name: hello.name,
        out,
        sessions,
        items_floor: 0,
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
                peer.handle(msg).await;
            }
            Some(feedback) = feedback_rx.recv() => peer.feedback(feedback),
            Some(clip) = clips_rx.recv() => peer.clip_data(clip.generation, &clip.uti, clip.bytes),
            Some(done) = done_rx.recv() => peer.done(done).await,
            Some(told) = told_rx.recv() => peer.told(told),
            () = fetching_until(peer.held.as_ref()) => {
                tracing::debug!(client = %peer.client, "paste went on without the clipboard");
                peer.release_held();
            }
            Some(_finished) = peer.tasks.join_next(), if !peer.tasks.is_empty() => {}
            Some(_finished) = peer.streams.join_next(), if !peer.streams.is_empty() => {}
            ev = events.recv() => {
                match ev {
                    // The worker's clipboard goes only to the clients that want it now.
                    Ok(WorkerMsg::Clip(ClipMsg::Offer(_))) if !daemon.clip.is_watching(peer.link) => {}
                    // Already in the snapshot a resync sent.
                    Ok(WorkerMsg::Items(ItemSync::Delta { version, .. })) if version <= peer.items_floor => {}
                    Ok(msg) => {
                        peer.heard(&msg);
                        if peer.out.send(msg).await.is_err() {
                            break Ok("writer gone");
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(client = %peer.client, lagged = n, "missed worker events; sending the state again");
                        // What is queued is older than the state about to be read; skip it.
                        events = events.resubscribe();
                        if peer.resync().await.is_err() {
                            break Ok("writer gone");
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break Ok("daemon shutting down");
                    }
                }
            }
            reason = peer.conn.closed() => {
                tracing::info!(client = %peer.client, %reason, "connection closed");
                break Ok("connection closed");
            }
        }
    };
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

/// Read the client's loss feedback datagrams until the connection ends.
async fn read_feedback(conn: Connection, out: mpsc::Sender<Feedback>) {
    loop {
        let datagram = match conn.read_datagram().await {
            Ok(d) => d,
            Err(e) => {
                tracing::debug!(error = %e, "read_datagram ended");
                break;
            }
        };
        match slopty_proto::codec::decode_body::<Feedback>(&datagram) {
            Ok(feedback) => {
                if out.send(feedback).await.is_err() {
                    break;
                }
            }
            Err(e) => tracing::debug!(error = %e, len = datagram.len(), "bad feedback datagram"),
        }
    }
}

/// Tell the client a request about `session` failed.
async fn report(
    out: &mpsc::Sender<WorkerMsg>,
    client: ClientId,
    session: SessionId,
    e: &WorkerError,
) {
    tracing::warn!(%client, %session, error = %e, "request failed");
    let event = TermEvent::Error(e.to_string());
    let _sent = out.send(WorkerMsg::Term { session, event }).await;
}

/// One client's view of the daemon: its connection, outbound control queue, and attachments.
struct Peer<'d> {
    daemon: &'d Daemon,
    conn: Connection,
    client: ClientId,
    /// Its name from `Hello`, for the pointings it relays.
    name: String,
    out: mpsc::Sender<WorkerMsg>,
    /// The sessions this client was told exist, for a resync to tell it which ones it missed
    /// opening or closing.
    sessions: HashSet<SessionId>,
    /// The item registry version of the last resync's snapshot: deltas up to it are in it.
    items_floor: u64,
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
    /// Keep track of what a broadcast event told the client. A closed session's pump is let go
    /// of rather than aborted: it sends what the actor left in the sink, then finishes its
    /// stream.
    fn heard(&mut self, msg: &WorkerMsg) {
        match msg {
            WorkerMsg::SessionOpened(summary) => {
                self.sessions.insert(summary.id);
            }
            WorkerMsg::SessionClosed { session, .. } => {
                self.sessions.remove(session);
                drop(self.attached.remove(session));
            }
            _ => {}
        }
    }

    /// Send the client the state the broadcast carries, for events it missed: the sessions
    /// opened and closed since, the item registry, the agents and the ports. Clipboard offers
    /// are not repeated; the next copy offers again. `Err` when the writer is gone.
    async fn resync(&mut self) -> Result<(), ()> {
        let summaries = self.daemon.worker.summaries().await;
        let now: HashSet<SessionId> = summaries.iter().map(|s| s.id).collect();
        let mut msgs: Vec<WorkerMsg> = self
            .sessions
            .difference(&now)
            // Why each ended was among what was missed.
            .map(|&session| WorkerMsg::SessionClosed { session, reason: CloseReason::Exited })
            .collect();
        msgs.extend(
            summaries
                .into_iter()
                .filter(|s| !self.sessions.contains(&s.id))
                .map(WorkerMsg::SessionOpened),
        );
        let items = self.daemon.items.snapshot();
        if let ItemSync::Snapshot { version, .. } = &items {
            self.items_floor = *version;
        }
        msgs.push(WorkerMsg::Items(items));
        // Collected first: the lock must not be held across the sends.
        let agents = self.daemon.agents.lock().snapshot();
        msgs.extend(agents.into_iter().map(WorkerMsg::Agent));
        let ports = self.daemon.ports.lock().known();
        msgs.extend(ports.into_iter().map(|(session, ports)| WorkerMsg::Ports { session, ports }));
        for msg in msgs {
            self.heard(&msg);
            self.out.send(msg).await.map_err(|_gone| ())?;
        }
        Ok(())
    }

    async fn handle(&mut self, msg: ClientMsg) {
        match msg {
            ClientMsg::Hello(_) => {
                tracing::warn!(client = %self.client, "duplicate Hello ignored");
            }
            ClientMsg::Ping { sent_at } => {
                let _sent = self.out.send(WorkerMsg::Pong { sent_at }).await;
            }
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
                if matches!(req, TermRequest::Raw(_) | TermRequest::Key(_)) {
                    tracing::trace!(client = %self.client, %session, "term input received");
                }
                self.term(session, req).await;
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
                Err(e) => report(&self.out, self.client, SessionId::nil(), &e).await,
            },
            ClientMsg::Screen(req) => self.screen(req).await,
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
            ClientMsg::Clip(msg) => self.clip(msg).await,
            ClientMsg::Xfer(msg) => self.xfer(msg).await,
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

    /// A task the loop started finished its part.
    async fn done(&mut self, done: Done) {
        match done {
            Done::Attach { session, size } => self.attach(session, size).await,
            Done::PastePlan(Some(Paste::Fetch { generation, utis })) => {
                tracing::debug!(client = %self.client, generation, ?utis, "paste waits for the clipboard");
                for uti in utis {
                    let fetch = ClipMsg::Fetch { generation, uti };
                    let _sent = self.out.send(WorkerMsg::Clip(fetch)).await;
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
        }
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

    async fn clip(&mut self, msg: ClipMsg) {
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
                crate::xfer::send_clip(self.daemon, &self.conn, &self.out, generation, uti).await;
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

    async fn xfer(&mut self, msg: XferMsg) {
        match msg {
            XferMsg::Begin { xfer, dest: Some(dest), files, bytes } => {
                let cwd = match &dest {
                    Dest::SessionCwd(session) => self.session_cwd(*session).await,
                    Dest::Staging | Dest::Path(_) => None,
                };
                tracing::info!(client = %self.client, %xfer, ?dest, ?cwd, files, bytes, "upload begins");
                self.daemon.transfers.begin(xfer, &dest, cwd.as_deref(), files);
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

    /// Where `session`'s shell is: its OSC 7 directory, else its foreground process's.
    async fn session_cwd(&self, session: SessionId) -> Option<String> {
        let handle = self.daemon.worker.get(session).ok()?;
        if let Some(cwd) = handle.snapshot().await.ok()?.cwd {
            return Some(cwd);
        }
        let cwd = handle.probe().await.ok()?.foreground?.cwd?;
        Some(cwd.to_string_lossy().into_owned())
    }

    async fn screen(&mut self, req: ScreenRequest) {
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
                if let Some(control) = self.screens.get(&stream).and_then(|s| s.control.as_ref()) {
                    let path =
                        slopty_net::endpoint::path_rtt_cwnd(&self.conn).map(|(rtt, cwnd)| {
                            slopty_worker::screen::PathSample {
                                rtt: slopty_core::Duration::from_micros(
                                    u64::try_from(rtt.as_micros()).unwrap_or(u64::MAX),
                                ),
                                cwnd,
                            }
                        });
                    if let Some(decision) = control.report(&report, path) {
                        let event = slopty_proto::screen::ScreenEvent::Rate {
                            stream,
                            target_bps: decision.target_bps,
                            verdict: decision.verdict,
                            capped: decision.capped,
                        };
                        let _sent = self.out.send(WorkerMsg::Screen(event)).await;
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
            Feedback::Refresh { stream, last_good_frame } => {
                if let Some(control) = control(stream) {
                    control.request_refresh(last_good_frame);
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

    async fn term(&mut self, session: SessionId, req: TermRequest) {
        match req {
            TermRequest::Attach { size } => self.attach(session, size).await,
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
                    report(&self.out, self.client, session, &e).await;
                }
            }
        }
    }

    fn forget(&mut self, session: SessionId) {
        if let Some((task, _sink)) = self.attached.remove(&session) {
            task.abort();
        }
    }

    /// Attach to `session`: open a uni stream and pump the actor's events into it.
    async fn attach(&mut self, session: SessionId, size: TermSize) {
        let handle = match self.daemon.worker.get(session) {
            Ok(h) => h,
            Err(e) => return report(&self.out, self.client, session, &e).await,
        };
        self.forget(session);
        let stream = match slopty_net::streams::open_session(&self.conn, session).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(client = %self.client, %session, error = %e, "open session stream");
                return;
            }
        };
        let (sink, mut events) = mpsc::channel::<Outbound>(SINK_DEPTH);
        let weak = sink.downgrade();
        if let Err(e) = handle.attach(self.client, size, sink) {
            return report(&self.out, self.client, session, &e).await;
        }
        let client = self.client;
        let task = tokio::spawn(async move {
            let mut stream = stream;
            while let Some(out) = events.recv().await {
                let send_from = std::time::Instant::now();
                if let Err(e) = stream.send_raw(out.wire()).await {
                    tracing::debug!(%client, %session, error = %e, "session stream ended");
                    break;
                }
                if out.is_frame() {
                    tracing::trace!(
                        %client,
                        %session,
                        send_us = send_from.elapsed().as_micros(),
                        "frame sent"
                    );
                }
            }
            let _finished = stream.finish();
        });
        self.attached.insert(session, (task, weak));
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::input::{KeyAction, KeyCode, Mods};
    use slopty_proto::screen::ScreenInput;

    use super::{Route, StreamId, route};

    fn key(code: KeyCode, mods: Mods) -> ScreenInput {
        ScreenInput::Key { code, action: KeyAction::Press, mods, text: None }
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
