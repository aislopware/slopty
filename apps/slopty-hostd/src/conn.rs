//! One client connection.

use std::collections::HashMap;

use slopty_core::{ClientId, SessionId, StreamId};
use slopty_host::HostError;
use slopty_host::screen::{
    DATAGRAM_QUEUE, DatagramBudget, Quantiles, Queued, ScreenStream, StreamEvent, listing,
};
use slopty_host::session::{ClientSink, Outbound};
use slopty_net::host::{AcceptedClient, open_session_stream};
use slopty_net::{ClientMsg, Connection, HostMsg, NetError};
use slopty_proto::PROTOCOL_VERSION;
use slopty_proto::handshake::{Caps, HelloAck};
use slopty_proto::items::ItemSync;
use slopty_proto::screen::{Feedback, MAX_CLIPBOARD_BYTES, ScreenEvent, ScreenRequest};
use slopty_proto::terminal::{CloseReason, TermEvent, TermRequest, TermSize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::Daemon;

/// How often a stream's target is checked for a size change.
/// Also how fast a window on the display-crop path follows a drag: the crop lags a move by
/// one period plus ScreenCaptureKit's ~20 ms configuration update (MEASUREMENTS.md, "capture
/// floor"), during which one edge of the picture shows the desktop the window left.
const GEOMETRY_PERIOD: std::time::Duration = std::time::Duration::from_millis(100);
/// Paths the palette's quick open is answered with at most.
const FILES_LISTED: usize = 8;
/// How often the files behind a client's file cards are looked at for a change.
const FILES_PERIOD: std::time::Duration = std::time::Duration::from_millis(1000);

/// Events buffered per attached session before the client is considered stuck.
const SINK_DEPTH: usize = 256;
/// Outbound control messages buffered before the writer applies backpressure.
const CONTROL_DEPTH: usize = 1024;
/// Loss feedback datagrams buffered between the reader and the peer loop.
const FEEDBACK_DEPTH: usize = 256;

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

    let (out, mut out_rx) = mpsc::channel::<HostMsg>(CONTROL_DEPTH);
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
        sessions: daemon.host.summaries().await,
    };
    out.send(HostMsg::HelloAck(ack)).await.map_err(|_gone| NetError::Closed)?;
    out.send(HostMsg::Items(daemon.items.snapshot())).await.map_err(|_gone| NetError::Closed)?;
    // Collected first: the lock must not be held across the sends.
    let agents = daemon.agents.lock().snapshot();
    for event in agents {
        out.send(HostMsg::Agent(event)).await.map_err(|_gone| NetError::Closed)?;
    }

    let mut events = daemon.events.subscribe();
    let (datagrams, datagram_rx) = mpsc::channel::<Queued>(DATAGRAM_QUEUE);
    let budget = DatagramBudget::new();
    let pump = tokio::spawn(pump_datagrams(conn.clone(), datagram_rx, budget.clone()));
    let health = tokio::spawn(slopty_net::endpoint::trace_path_health(conn.clone(), "host"));
    let (feedback_tx, mut feedback_rx) = mpsc::channel::<Feedback>(FEEDBACK_DEPTH);
    let feedback = tokio::spawn(read_feedback(conn.clone(), feedback_tx));
    let mut peer = Peer {
        daemon,
        conn,
        client: hello.client,
        name: hello.name,
        out,
        attached: HashMap::new(),
        screens: HashMap::new(),
        next_stream: 1,
        datagrams,
        budget,
        pump,
        health,
        feedback,
        watched: HashMap::new(),
    };
    daemon.wake.lock().client_joined();

    // Window resizes on the host are polled: ScreenCaptureKit keeps scaling the old output
    // size until the capture is reconfigured.
    let mut geometry = tokio::time::interval(GEOMETRY_PERIOD);
    geometry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut files = tokio::time::interval(FILES_PERIOD);
    files.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let result = loop {
        tokio::select! {
            _ = files.tick(), if !peer.watched.is_empty() => {
                if !peer.poll_files().await {
                    break Ok("writer gone");
                }
            }
            _ = geometry.tick(), if !peer.screens.is_empty() => {
                if !peer.check_geometry().await {
                    break Ok("writer gone");
                }
            }
            msg = rx.recv() => {
                let msg = match msg {
                    Ok(m) => m,
                    Err(NetError::Closed) => break Ok("control stream closed"),
                    Err(e) => break Err(e),
                };
                peer.handle(msg).await;
            }
            Some(feedback) = feedback_rx.recv() => peer.feedback(feedback),
            ev = events.recv() => {
                match ev {
                    // The host clipboard is only shared with clients showing a window.
                    Ok(HostMsg::Screen(ScreenEvent::Clipboard { .. })) if peer.screens.is_empty() => {}
                    Ok(msg) => {
                        if peer.out.send(msg).await.is_err() {
                            break Ok("writer gone");
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(client = %peer.client, lagged = n, "missed host events");
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
    peer.close_screens().await;
    // Whatever ended the loop, the client must not be left on a live connection nobody
    // serves; a no-op when the connection is already closed.
    peer.conn.close(0_u32.into(), b"done");
    drop(peer);
    writer.abort();
    result
}

/// Register `slopty hook` in the host's Claude Code settings, for a client that saw an agent
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
        return (false, "the slopty command is not installed beside the host".to_owned());
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

/// How long the pump waits for the next datagram before looking again, while anything is
/// outstanding: QUIC still holding what it took, or datagrams still queued for it.
///
/// It is also the promise the pump's own lateness is measured against. A turn that asks for
/// this much and comes back far later is this task not being scheduled, which is the only way
/// to tell that from the path holding the bytes.
const HOLD_POLL: std::time::Duration = std::time::Duration::from_millis(1);

/// One stretch during which QUIC held datagrams in its send buffer (congestion window or
/// pacing), for the debug log: when it began, the most it held, the window at that moment.
struct Hold {
    since: std::time::Instant,
    max_bytes: usize,
    cwnd_at_max: u64,
}

/// One stretch during which datagrams waited in the pump's channel, ahead of QUIC.
///
/// `behind` sums only the turns the pump *owed* them — the gaps in which the queue was already
/// non-empty when the loop last looked — rather than the wall clock since the backlog was first
/// noticed. Timing from the moment `recv` hands over the first datagram measures how long the
/// drain took and misses the sleep before it entirely, which is how a pump descheduled for
/// 200 ms could wake, drain in 1 ms and report 1 ms behind.
///
/// `late` is the worst single turn: how far past [`HOLD_POLL`] one trip round the loop took
/// while work was outstanding. That is the pump not being scheduled, stated directly.
#[derive(Default, PartialEq, Eq, Debug)]
struct Backlog {
    behind: std::time::Duration,
    late: std::time::Duration,
    max_queued: usize,
}

impl Backlog {
    /// Fold in one turn of the pump loop: `since` is how long the turn took and `queued` is what
    /// was already waiting when the *previous* turn looked. Only called when that was non-zero.
    fn turn(&mut self, since: std::time::Duration, queued: usize) {
        self.behind = self.behind.saturating_add(since);
        self.late = self.late.max(since.saturating_sub(HOLD_POLL));
        self.max_queued = self.max_queued.max(queued);
    }
}

/// Samples the queue-wait quantiles are computed over: 10 s of frames at 60 fps and ~10
/// datagrams a frame would be more; this is enough to say what the last stretch looked like.
const WAIT_WINDOW: usize = 1024;

/// A datagram that waited longer than this in the pump's channel is logged on its own, even
/// with no backlog around it: a queue that was empty when the pump last looked and then held
/// one datagram for the length of a deschedule is invisible to the turn accounting above.
const LATE_WAIT: std::time::Duration = std::time::Duration::from_millis(20);

/// How long each datagram waited in the pump's channel, read from the stamp `Queued` carries,
/// over the last [`WAIT_WINDOW`] of them. This is the measurement the backlog accounting
/// approximates: not how the pump's turns went, but what each datagram actually paid.
#[derive(Debug, Default)]
struct Waits {
    ring: std::collections::VecDeque<u64>,
    /// The worst single wait since the last time it was reported.
    worst_us: u64,
}

impl Waits {
    fn push(&mut self, waited_us: u64) {
        if self.ring.len() >= WAIT_WINDOW {
            self.ring.pop_front();
        }
        self.ring.push_back(waited_us);
        self.worst_us = self.worst_us.max(waited_us);
    }

    fn quantiles(&self) -> Quantiles {
        let (a, b) = self.ring.as_slices();
        let mut all = Vec::with_capacity(a.len().saturating_add(b.len()));
        all.extend_from_slice(a);
        all.extend_from_slice(b);
        Quantiles::of(&all)
    }

    /// The worst wait since the last call, and forget it.
    fn take_worst_us(&mut self) -> u64 {
        std::mem::take(&mut self.worst_us)
    }
}

/// Drain the media queue into QUIC datagrams, tracking what the path can carry.
///
/// Logs every stretch during which QUIC held datagrams back (the send buffer was not empty):
/// media that waits there is latency the receiver sees as a stall, and the length and size
/// of those holds is what start-up and bitrate steps are judged by.
///
/// Logs the other half too. A datagram waits in one of two places, and only the second is the
/// path's doing: in this task's channel, because the pump has not run; or in QUIC's send buffer,
/// because the window or the pacer holds it. A receiver cannot tell them apart — both are
/// silence on the link — so a stall charged to the network needs the backlog line ruled out
/// first.
async fn pump_datagrams(conn: Connection, mut rx: mpsc::Receiver<Queued>, budget: DatagramBudget) {
    let mut last_budget = 0;
    let mut hold: Option<Hold> = None;
    let mut backlog: Option<Backlog> = None;
    let mut waits = Waits::default();
    // When a lone late datagram was last logged, so a stretch of them is one line a second.
    let mut late_logged: Option<std::time::Instant> = None;
    // What the previous turn of the loop saw, so this one can tell how long the pump owed a
    // turn to datagrams that were already waiting.
    let mut last_turn = std::time::Instant::now();
    let mut queued_before = 0_usize;
    loop {
        // Poll while anything is outstanding, so every turn with work to do has a deadline to
        // be late against; block only when there is nothing to be late for.
        let waiting = hold.is_some() || queued_before > 0;
        let datagram = if waiting {
            match tokio::time::timeout(HOLD_POLL, rx.recv()).await {
                Ok(Some(d)) => Some(d),
                Ok(None) => break,
                Err(_elapsed) => None,
            }
        } else {
            rx.recv().await
        };
        let now = std::time::Instant::now();
        let since = now.saturating_duration_since(last_turn);
        last_turn = now;
        let held =
            slopty_net::endpoint::DATAGRAM_BUFFER.saturating_sub(conn.datagram_send_buffer_space());
        budget.set_held(held);
        let queued = rx.len();
        if queued_before > 0 {
            backlog.get_or_insert_with(Backlog::default).turn(since, queued_before);
        }
        if queued == 0
            && let Some(b) = backlog.take()
        {
            tracing::debug!(
                behind_ms = b.behind.as_millis(),
                late_ms = b.late.as_millis(),
                max_queued = b.max_queued,
                waited = %waits.quantiles().describe(),
                worst_wait_ms = waits.take_worst_us() / 1000,
                "the datagram pump caught up"
            );
        }
        queued_before = queued;
        // The capture guard sizes its budget from the window (`frame_fits`), so it is sampled
        // when a hold starts, when the hold deepens, and on a poll turn — one that woke on
        // `HOLD_POLL` with no datagram, which is exactly a hold that is long and quiet, the
        // only case where the reading would otherwise go stale. Not on every turn: reading the
        // path takes the connection's lock the QUIC driver wants, and a 30 Mbit/s frame is
        // ~50 datagrams, so "every turn" would be thousands of samples a second.
        let deepening = hold.as_ref().is_some_and(|h: &Hold| held > h.max_bytes);
        let sample = held > 0 && (hold.is_none() || deepening || datagram.is_none());
        let cwnd = if sample {
            let (_rtt, cwnd) = slopty_net::endpoint::path_rtt_cwnd(&conn).unwrap_or_default();
            budget.set_cwnd(cwnd);
            cwnd
        } else {
            0
        };
        match (&mut hold, held) {
            (None, 0) => {}
            (None, bytes) => {
                hold = Some(Hold {
                    since: std::time::Instant::now(),
                    max_bytes: bytes,
                    cwnd_at_max: cwnd,
                });
            }
            (Some(h), 0) => {
                tracing::debug!(
                    held_ms = h.since.elapsed().as_millis(),
                    max_bytes = h.max_bytes,
                    cwnd = h.cwnd_at_max,
                    "quic released the datagrams it held"
                );
                hold = None;
            }
            (Some(h), bytes) => {
                if bytes > h.max_bytes {
                    h.max_bytes = bytes;
                    h.cwnd_at_max = cwnd;
                }
            }
        }
        let Some(queued_datagram) = datagram else { continue };
        let waited_us = queued_datagram.waited_us();
        let datagram = queued_datagram.datagram;
        waits.push(waited_us);
        if waited_us > u64::try_from(LATE_WAIT.as_micros()).unwrap_or(u64::MAX)
            && backlog.is_none()
            && late_logged.is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(1))
        {
            late_logged = Some(std::time::Instant::now());
            tracing::debug!(
                waited_ms = waited_us / 1000,
                queued,
                "a datagram waited in the pump's channel with no backlog to charge it to"
            );
        }
        if let Some(max) = conn.max_datagram_size() {
            if max != last_budget {
                tracing::debug!(max, "datagram budget");
                budget.set(max);
                last_budget = max;
            }
            if datagram.len() > max {
                // Cut for a larger budget than the path has now; the next frame adapts.
                continue;
            }
        }
        if let Err(e) = conn.send_datagram(datagram) {
            tracing::debug!(error = %e, "send_datagram");
            break;
        }
    }
}

/// One client's view of the daemon: its connection, outbound control queue, and attachments.
struct Peer<'d> {
    daemon: &'d Daemon,
    conn: Connection,
    client: ClientId,
    /// Its name from `Hello`, for the pointings it relays.
    name: String,
    out: mpsc::Sender<HostMsg>,
    /// Per attached session: the stream pump and the sink the actor writes into.
    attached: HashMap<SessionId, (JoinHandle<()>, ClientSink)>,
    screens: HashMap<StreamId, ScreenStream>,
    next_stream: u32,
    datagrams: mpsc::Sender<Queued>,
    budget: DatagramBudget,
    pump: JoinHandle<()>,
    /// The periodic congestion-window sample; a no-op task unless the trace is on.
    health: JoinHandle<()>,
    feedback: JoinHandle<()>,
    /// Files behind this client's file cards, each with the stamp it was last seen with.
    watched: HashMap<String, Option<(u64, u128)>>,
}

impl Drop for Peer<'_> {
    fn drop(&mut self) {
        // Detach only what this connection attached: the same client may already be back on a
        // new connection, and its viewers must survive the old one idling out.
        for (session, (task, sink)) in &self.attached {
            task.abort();
            if let Ok(handle) = self.daemon.host.get(*session) {
                let _ignored = handle.detach_sink(self.client, sink);
            }
        }
        self.pump.abort();
        self.health.abort();
        self.feedback.abort();
        self.daemon.wake.lock().client_left();
    }
}

impl Peer<'_> {
    async fn handle(&mut self, msg: ClientMsg) {
        match msg {
            ClientMsg::Hello(_) => {
                tracing::warn!(client = %self.client, "duplicate Hello ignored");
            }
            ClientMsg::Ping { sent_at } => {
                let _sent = self.out.send(HostMsg::Pong { sent_at }).await;
            }
            ClientMsg::OpenSession(req) => match self.daemon.host.open(&req).await {
                Ok(handle) => {
                    let session = handle.id();
                    // Whoever opened it sizes it, whichever client attaches first.
                    let _reserved = handle.reserve_driver(self.client);
                    let summaries = self.daemon.host.summaries().await;
                    if let Some(summary) = summaries.into_iter().find(|s| s.id == session) {
                        let _sent = self.daemon.events.send(HostMsg::SessionOpened(summary));
                    }
                    if let Some(delta) = self.daemon.items.ensure_terminal(session, self.client) {
                        let _sent = self.daemon.events.send(HostMsg::Items(delta));
                    }
                    if req.attach {
                        self.attach(session, req.size).await;
                    }
                }
                Err(e) => self.report(SessionId::nil(), &e).await,
            },
            ClientMsg::Term { session, req } => {
                if matches!(req, TermRequest::Raw(_) | TermRequest::Key(_)) {
                    tracing::trace!(client = %self.client, %session, "term input received");
                }
                self.term(session, req).await;
            }
            ClientMsg::Point { item } => {
                // Ephemeral: relayed as is, never in the registry. An item the host no longer
                // has is for each client to ignore.
                let pointed =
                    ItemSync::Pointed { client: self.client, name: self.name.clone(), item };
                let _sent = self.daemon.events.send(HostMsg::Items(pointed));
            }
            ClientMsg::Items(op) => match self.daemon.items.apply(op, self.client) {
                Ok(delta) => {
                    let _sent = self.daemon.events.send(HostMsg::Items(delta));
                }
                Err(e) => self.report(SessionId::nil(), &e).await,
            },
            ClientMsg::Screen(req) => self.screen(req).await,
            ClientMsg::FindFiles { root, query } => {
                let paths = if query.is_empty() {
                    Vec::new()
                } else {
                    let (dir, needle) = (root.clone(), query.clone());
                    tokio::task::spawn_blocking(move || {
                        let dir = slopty_host::file::expand_home(std::path::Path::new(&dir));
                        slopty_host::find::matching(&dir, &needle, FILES_LISTED)
                    })
                    .await
                    .unwrap_or_default()
                };
                let _sent = self.out.send(HostMsg::FoundFiles { root, query, paths }).await;
            }
            ClientMsg::ReadFile { path } => {
                // A paired client already has a shell here; a read is nothing it could not do.
                let _sent = self.send_file(path).await;
            }
            ClientMsg::WatchFiles { paths } => {
                let mut watched = HashMap::with_capacity(paths.len());
                for path in paths {
                    // A path kept keeps its stamp; a new one is stamped as it is now (the
                    // card's own read shows that state).
                    let stamp = if let Some(stamp) = self.watched.remove(&path) {
                        stamp
                    } else {
                        let target = path.clone();
                        tokio::task::spawn_blocking(move || {
                            slopty_host::file::stamp(std::path::Path::new(&target))
                        })
                        .await
                        .unwrap_or(None)
                    };
                    watched.insert(path, stamp);
                }
                tracing::debug!(client = %self.client, files = watched.len(), "watch files");
                self.watched = watched;
            }
            ClientMsg::InstallHooks => {
                let (ok, message) = install_hooks().await;
                tracing::info!(client = %self.client, ok, %message, "install hooks");
                let _sent = self.out.send(HostMsg::HooksInstalled { ok, message }).await;
            }
        }
    }

    /// Read `path` and send what is there; `false` when the writer is gone.
    async fn send_file(&self, path: String) -> bool {
        let target = path.clone();
        let read = tokio::task::spawn_blocking(move || {
            slopty_host::file::read(std::path::Path::new(&target))
        })
        .await
        .unwrap_or_else(|_| slopty_proto::file::FileRead::Missing {
            error: "read failed".to_owned(),
        });
        let kind = match &read {
            slopty_proto::file::FileRead::Text { .. } => "text",
            slopty_proto::file::FileRead::Binary { .. } => "binary",
            slopty_proto::file::FileRead::Missing { .. } => "missing",
        };
        tracing::info!(client = %self.client, %path, kind, "read file");
        self.out.send(HostMsg::File { path, read }).await.is_ok()
    }

    /// Look at every watched file; one whose stamp moved is read and sent again. `false`
    /// when the writer is gone.
    async fn poll_files(&mut self) -> bool {
        let paths: Vec<String> = self.watched.keys().cloned().collect();
        let stamps = tokio::task::spawn_blocking(move || {
            paths
                .into_iter()
                .map(|path| {
                    let stamp = slopty_host::file::stamp(std::path::Path::new(&path));
                    (path, stamp)
                })
                .collect::<Vec<_>>()
        })
        .await;
        let Ok(stamps) = stamps else { return true };
        for (path, stamp) in stamps {
            let Some(seen) = self.watched.get_mut(&path) else { continue };
            if *seen == stamp {
                continue;
            }
            *seen = stamp;
            if !self.send_file(path).await {
                return false;
            }
        }
        true
    }

    async fn screen(&mut self, req: ScreenRequest) {
        match req {
            ScreenRequest::List => {
                let out = self.out.clone();
                let client = self.client;
                tokio::spawn(async move {
                    match listing().await {
                        Ok(event) => {
                            let _sent = out.send(HostMsg::Screen(event)).await;
                        }
                        Err(e) => tracing::warn!(%client, error = %e, "screen listing"),
                    }
                });
            }
            ScreenRequest::Open { target, quality } => {
                let id = StreamId(self.next_stream);
                self.next_stream = self.next_stream.wrapping_add(1).max(1);
                let out = self.out.clone();
                let on_event = move |e: StreamEvent| {
                    let event = match e {
                        StreamEvent::Stopped(e) => {
                            ScreenEvent::Closed { stream: id, reason: e.to_string() }
                        }
                        StreamEvent::Cursor(shape) => {
                            ScreenEvent::Cursor { stream: id, shape: Some(shape) }
                        }
                    };
                    let _sent = out.try_send(HostMsg::Screen(event));
                };
                let opened = ScreenStream::open(
                    id,
                    target,
                    quality,
                    self.datagrams.clone(),
                    self.budget.clone(),
                    on_event,
                )
                .await;
                match opened {
                    Ok((stream, event)) => {
                        tracing::info!(client = %self.client, %id, ?target, "screen opened");
                        self.daemon.screens.insert(&self.client, target, stream.stats_handle());
                        self.screens.insert(id, stream);
                        let _sent = self.out.send(HostMsg::Screen(event)).await;
                    }
                    Err(e) => {
                        tracing::warn!(client = %self.client, ?target, error = %e, "screen open");
                        let event = ScreenEvent::Closed { stream: id, reason: e.to_string() };
                        let _sent = self.out.send(HostMsg::Screen(event)).await;
                    }
                }
            }
            ScreenRequest::Close(id) => {
                if let Some(stream) = self.screens.remove(&id) {
                    tracing::info!(
                        client = %self.client,
                        %id,
                        path = %slopty_net::endpoint::describe_health(&self.conn),
                        "screen closing"
                    );
                    stream.close().await;
                    self.daemon.screens.remove(&self.client, id);
                    let reason = "closed by client".to_owned();
                    let event = ScreenEvent::Closed { stream: id, reason };
                    let _sent = self.out.send(HostMsg::Screen(event)).await;
                }
            }
            ScreenRequest::SetQuality { stream, quality } => {
                if let Some(s) = self.screens.get_mut(&stream)
                    && let Err(e) = s.set_quality(&quality)
                {
                    tracing::warn!(client = %self.client, %stream, error = %e, "set quality");
                }
            }
            ScreenRequest::Report { stream, report } => {
                if let Some(s) = self.screens.get(&stream) {
                    let path =
                        slopty_net::endpoint::path_rtt_cwnd(&self.conn).map(|(rtt, cwnd)| {
                            slopty_host::screen::PathSample {
                                rtt: slopty_core::Duration::from_micros(
                                    u64::try_from(rtt.as_micros()).unwrap_or(u64::MAX),
                                ),
                                cwnd,
                            }
                        });
                    if let Some(decision) = s.report(&report, path) {
                        let event = ScreenEvent::Rate {
                            stream,
                            target_bps: decision.target_bps,
                            verdict: decision.verdict,
                            capped: decision.capped,
                        };
                        let _sent = self.out.send(HostMsg::Screen(event)).await;
                    }
                }
            }
            ScreenRequest::Input { stream, input } => {
                if let Some(s) = self.screens.get_mut(&stream)
                    && let Err(e) = s.inject(&input)
                {
                    tracing::debug!(client = %self.client, %stream, error = %e, "input");
                }
            }
            ScreenRequest::Focus(stream) => {
                if let Some(s) = self.screens.get_mut(&stream)
                    && let Err(e) = s.focus()
                {
                    tracing::debug!(client = %self.client, %stream, error = %e, "focus");
                }
            }
            ScreenRequest::Resize { stream, width, height } => {
                let Some(asked) =
                    self.screens.get(&stream).and_then(|s| s.resize_points(width, height))
                else {
                    tracing::debug!(client = %self.client, %stream, "resize: not a window stream");
                    return;
                };
                let client = self.client;
                // Off the runtime: a few accessibility round trips.
                drop(tokio::task::spawn_blocking(move || {
                    let (window, w, h) = asked;
                    match slopty_host::screen::resize_window(window, w, h) {
                        Ok(()) => tracing::debug!(%client, %stream, w, h, "window resized"),
                        Err(e) => tracing::debug!(%client, %stream, error = %e, "resize"),
                    }
                }));
            }
            ScreenRequest::Clipboard { text } => {
                if self.screens.is_empty() || text.len() > MAX_CLIPBOARD_BYTES {
                    tracing::debug!(client = %self.client, bytes = text.len(), "clipboard refused");
                } else if self.daemon.pasteboard.write(&text) {
                    tracing::debug!(client = %self.client, bytes = text.len(), "pasteboard set");
                } else {
                    tracing::warn!(client = %self.client, "pasteboard write failed");
                }
            }
            ScreenRequest::ClipboardImage { media_type, bytes } => {
                if self.screens.is_empty() {
                    tracing::debug!(client = %self.client, bytes = bytes.len(), "picture refused");
                } else if self.daemon.pasteboard.write_picture(&media_type, &bytes) {
                    tracing::debug!(client = %self.client, %media_type, bytes = bytes.len(), "pasteboard picture set");
                } else {
                    tracing::warn!(client = %self.client, %media_type, bytes = bytes.len(), "pasteboard picture refused");
                }
            }
        }
    }

    /// Loss feedback from a datagram: retransmit or refresh.
    fn feedback(&self, feedback: Feedback) {
        match feedback {
            Feedback::Nack { stream, frame, fragments } => {
                if let Some(s) = self.screens.get(&stream) {
                    tracing::debug!(
                        %stream,
                        frame,
                        missing = fragments.len(),
                        path = %slopty_net::endpoint::describe_health(&self.conn),
                        "nack"
                    );
                    s.nack(frame, &fragments);
                }
            }
            Feedback::Refresh { stream, last_good_frame } => {
                if let Some(s) = self.screens.get(&stream) {
                    s.request_refresh(last_good_frame);
                }
            }
        }
    }

    /// Tell the client about streams whose target changed size, and about targets that have
    /// drawn nothing yet (so the receiver stops asking for a refresh no frame can answer).
    /// False once the client is gone.
    async fn check_geometry(&mut self) -> bool {
        let mut events = Vec::new();
        for (id, stream) in &mut self.screens {
            match stream.check_geometry() {
                Ok(Some(event)) => events.push(event),
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(client = %self.client, stream = %id, error = %e, "geometry");
                }
            }
            events.extend(stream.check_source());
        }
        for event in events {
            if self.out.send(HostMsg::Screen(event)).await.is_err() {
                return false;
            }
        }
        true
    }

    async fn close_screens(&mut self) {
        for (id, stream) in self.screens.drain() {
            stream.close().await;
            self.daemon.screens.remove(&self.client, id);
        }
    }

    async fn term(&mut self, session: SessionId, req: TermRequest) {
        match req {
            TermRequest::Attach { size } => self.attach(session, size).await,
            TermRequest::Detach => {
                self.forget(session);
                if let Ok(h) = self.daemon.host.get(session) {
                    let _ignored = h.detach(self.client);
                }
            }
            TermRequest::Close => {
                self.forget(session);
                match self.daemon.host.close(session).await {
                    Ok(()) => {
                        tracing::debug!(client = %self.client, %session, "closed on request");
                        self.daemon.agents.lock().forget(session);
                        let reason = CloseReason::Requested;
                        let _sent =
                            self.daemon.events.send(HostMsg::SessionClosed { session, reason });
                        for delta in self.daemon.items.remove_session(session, self.client) {
                            let _sent = self.daemon.events.send(HostMsg::Items(delta));
                        }
                    }
                    Err(e) => self.report(session, &e).await,
                }
            }
            other => {
                let outcome =
                    self.daemon.host.get(session).and_then(|h| h.request(self.client, other));
                if let Err(e) = outcome {
                    self.report(session, &e).await;
                }
            }
        }
    }

    fn forget(&mut self, session: SessionId) {
        if let Some((task, _sink)) = self.attached.remove(&session) {
            task.abort();
        }
    }

    async fn report(&self, session: SessionId, e: &HostError) {
        tracing::warn!(client = %self.client, %session, error = %e, "request failed");
        let event = TermEvent::Error(e.to_string());
        let _sent = self.out.send(HostMsg::Term { session, event }).await;
    }

    /// Attach to `session`: open a uni stream and pump the actor's events into it.
    async fn attach(&mut self, session: SessionId, size: TermSize) {
        let handle = match self.daemon.host.get(session) {
            Ok(h) => h,
            Err(e) => return self.report(session, &e).await,
        };
        self.forget(session);
        let stream = match open_session_stream(&self.conn, session).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(client = %self.client, %session, error = %e, "open session stream");
                return;
            }
        };
        let (sink, mut events) = mpsc::channel::<Outbound>(SINK_DEPTH);
        if let Err(e) = handle.attach(self.client, size, sink.clone()) {
            return self.report(session, &e).await;
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
        self.attached.insert(session, (task, sink));
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Backlog, HOLD_POLL, WAIT_WINDOW, Waits};

    /// 200 ms of sleep, less the millisecond the loop was entitled to take.
    const LATE: Duration = match Duration::from_millis(200).checked_sub(HOLD_POLL) {
        Some(late) => late,
        None => Duration::ZERO,
    };

    /// The scenario the review named: the pump is descheduled for 200 ms while datagrams pile
    /// up, then wakes and drains them in a millisecond. Timing the stretch from the moment
    /// `recv` first hands one over would report 1 ms; the sleep is the whole of the lag.
    #[test]
    fn a_pump_descheduled_before_it_drains_reports_the_sleep_not_the_drain() {
        let mut b = Backlog::default();
        b.turn(Duration::from_millis(200), 40);
        assert_eq!(b.behind, Duration::from_millis(200));
        assert_eq!(b.late, LATE);
        assert_eq!(b.max_queued, 40);
        // Draining the rest quickly adds its own small cost and erases nothing.
        for _ in 0..40 {
            b.turn(Duration::from_micros(25), 1);
        }
        assert_eq!(b.behind, Duration::from_millis(201));
        assert_eq!(b.late, LATE, "the worst turn stands");
        assert_eq!(b.max_queued, 40);
    }

    /// The wait ring keeps the last window of stamps, reports their quantiles, and hands out
    /// the worst wait once: a 200 ms deschedule shows up as the max even after a thousand
    /// quick datagrams have pushed it out of the window's median.
    #[test]
    fn the_wait_ring_reports_the_last_window_and_the_worst_once() {
        let mut w = Waits::default();
        w.push(200_000);
        for _ in 0..WAIT_WINDOW {
            w.push(50);
        }
        let q = w.quantiles();
        assert_eq!(
            (q.n as usize, q.p50_us, q.max_us),
            (WAIT_WINDOW, 50, 50),
            "the window moved on"
        );
        assert_eq!(w.take_worst_us(), 200_000, "the worst wait does not");
        assert_eq!(w.take_worst_us(), 0, "and is reported once");
    }

    /// A pump that keeps its promise every turn is never late, however long the backlog lasts.
    #[test]
    fn a_pump_that_keeps_its_turn_is_never_late() {
        let mut b = Backlog::default();
        for _ in 0..100 {
            b.turn(HOLD_POLL, 3);
        }
        assert_eq!(b.late, Duration::ZERO);
        assert_eq!(b.behind, HOLD_POLL.saturating_mul(100), "still time datagrams spent queued");
    }
}
