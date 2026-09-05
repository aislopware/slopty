//! One authenticated client connection.

use std::collections::HashMap;

use bytes::Bytes;
use slopty_agent::transcript::Tail;
use slopty_core::{ClientId, SessionId, StreamId};
use slopty_host::HostError;
use slopty_host::screen::{DATAGRAM_QUEUE, DatagramBudget, ScreenStream, listing};
use slopty_host::session::ClientSink;
use slopty_net::host::{AuthenticatedClient, open_session_stream};
use slopty_net::{ClientMsg, Connection, HostMsg, NetError};
use slopty_proto::PROTOCOL_VERSION;
use slopty_proto::agent::{TranscriptFollow, TranscriptUpdate};
use slopty_proto::handshake::{Caps, HelloAck};
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
/// How often followed transcripts are re-read for new lines.
const TRANSCRIPT_PERIOD: std::time::Duration = std::time::Duration::from_millis(400);
/// Entries the first snapshot of a conversation carries (older ones are not sent).
const TRANSCRIPT_SNAPSHOT: usize = 200;

/// Events buffered per attached session before the client is considered stuck.
const SINK_DEPTH: usize = 256;
/// Outbound control messages buffered before the writer applies backpressure.
const CONTROL_DEPTH: usize = 1024;
/// Loss feedback datagrams buffered between the reader and the peer loop.
const FEEDBACK_DEPTH: usize = 256;

pub async fn serve(daemon: Daemon, client: AuthenticatedClient) {
    let remote = client.remote;
    let id = client.hello.client;
    tracing::info!(%remote, client = %id, name = %client.hello.name, "client connected");
    match run(&daemon, client).await {
        Ok(why) => tracing::info!(%remote, client = %id, why, "client finished"),
        Err(e) => tracing::info!(%remote, client = %id, error = %e, "client finished"),
    }
}

/// Serve one connection until it ends; `Ok` carries why it ended.
async fn run(daemon: &Daemon, client: AuthenticatedClient) -> Result<&'static str, NetError> {
    let AuthenticatedClient { conn, hello, mut tx, mut rx, .. } = client;

    let (out, mut out_rx) = mpsc::channel::<HostMsg>(CONTROL_DEPTH);
    let writer: JoinHandle<Result<(), NetError>> = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            tx.send(&msg).await?;
        }
        Ok(())
    });

    let ack = HelloAck {
        protocol: PROTOCOL_VERSION,
        host: daemon.id,
        name: daemon.name.clone(),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        caps: Caps::empty(),
        sessions: daemon.host.summaries().await,
    };
    out.send(HostMsg::HelloAck(ack)).await.map_err(|_gone| NetError::Closed)?;
    out.send(HostMsg::Canvas(daemon.canvas.snapshot())).await.map_err(|_gone| NetError::Closed)?;
    let agents = daemon.agents.lock().snapshot();
    for event in agents {
        out.send(HostMsg::Agent(event)).await.map_err(|_gone| NetError::Closed)?;
    }

    let mut events = daemon.events.subscribe();
    let (datagrams, datagram_rx) = mpsc::channel::<Bytes>(DATAGRAM_QUEUE);
    let budget = DatagramBudget::new();
    let pump = tokio::spawn(pump_datagrams(conn.clone(), datagram_rx, budget.clone()));
    let paths = tokio::spawn(slopty_net::endpoint::log_path_events(conn.clone(), "host"));
    let health = tokio::spawn(slopty_net::endpoint::trace_path_health(conn.clone(), "host"));
    let (feedback_tx, mut feedback_rx) = mpsc::channel::<Feedback>(FEEDBACK_DEPTH);
    let feedback = tokio::spawn(read_feedback(conn.clone(), feedback_tx));
    let mut peer = Peer {
        daemon,
        conn,
        client: hello.client,
        out,
        attached: HashMap::new(),
        screens: HashMap::new(),
        next_stream: 1,
        datagrams,
        budget,
        pump,
        paths,
        health,
        feedback,
        transcripts: HashMap::new(),
    };

    // Window resizes on the host are polled: ScreenCaptureKit keeps scaling the old output
    // size until the capture is reconfigured.
    let mut geometry = tokio::time::interval(GEOMETRY_PERIOD);
    geometry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut transcripts = tokio::time::interval(TRANSCRIPT_PERIOD);
    transcripts.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let result = loop {
        tokio::select! {
            _ = geometry.tick(), if !peer.screens.is_empty() => {
                if !peer.check_geometry().await {
                    break Ok("writer gone");
                }
            }
            _ = transcripts.tick(), if !peer.transcripts.is_empty() => {
                if !peer.poll_transcripts().await {
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
async fn pump_datagrams(conn: Connection, mut rx: mpsc::Receiver<Bytes>, budget: DatagramBudget) {
    let mut last_budget = 0;
    let mut hold: Option<Hold> = None;
    let mut backlog: Option<Backlog> = None;
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
                "the datagram pump caught up"
            );
        }
        queued_before = queued;
        match (&mut hold, held) {
            (None, 0) => {}
            (None, bytes) => {
                let (_rtt, cwnd) = slopty_net::endpoint::selected_path(&conn).unwrap_or_default();
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
                    h.cwnd_at_max =
                        slopty_net::endpoint::selected_path(&conn).map_or(0, |(_rtt, cwnd)| cwnd);
                }
            }
        }
        let Some(datagram) = datagram else { continue };
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
    out: mpsc::Sender<HostMsg>,
    /// Per attached session: the stream pump and the sink the actor writes into.
    attached: HashMap<SessionId, (JoinHandle<()>, ClientSink)>,
    screens: HashMap<StreamId, ScreenStream>,
    next_stream: u32,
    datagrams: mpsc::Sender<Bytes>,
    budget: DatagramBudget,
    pump: JoinHandle<()>,
    paths: JoinHandle<()>,
    /// The periodic congestion-window sample; a no-op task unless the trace is on.
    health: JoinHandle<()>,
    feedback: JoinHandle<()>,
    /// Conversations this client follows, with the reader's position in each file.
    transcripts: HashMap<SessionId, Tail>,
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
        self.paths.abort();
        self.health.abort();
        self.feedback.abort();
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
                    if let Some(delta) = self.daemon.canvas.ensure_terminal(session, self.client) {
                        let _sent = self.daemon.events.send(HostMsg::Canvas(delta));
                    }
                    if req.attach {
                        self.attach(session, req.size).await;
                    }
                }
                Err(e) => self.report(SessionId::nil(), &e).await,
            },
            ClientMsg::Term { session, req } => self.term(session, req).await,
            ClientMsg::Canvas(op) => match self.daemon.canvas.apply(op, self.client) {
                Ok(delta) => {
                    let _sent = self.daemon.events.send(HostMsg::Canvas(delta));
                }
                Err(e) => self.report(SessionId::nil(), &e).await,
            },
            ClientMsg::Screen(req) => self.screen(req).await,
            ClientMsg::Transcript(TranscriptFollow { session, follow }) => {
                if follow {
                    self.transcripts.insert(session, Tail::default());
                    let _alive = self.poll_transcripts().await;
                } else {
                    self.transcripts.remove(&session);
                }
            }
            ClientMsg::InstallHooks => {
                let (ok, message) = install_hooks().await;
                tracing::info!(client = %self.client, ok, %message, "install hooks");
                let _sent = self.out.send(HostMsg::HooksInstalled { ok, message }).await;
            }
        }
    }

    /// Read every followed transcript for new lines and send what appeared. The first read of
    /// a file (and a read after the file was replaced) is a snapshot of its last entries.
    /// Returns `false` when the client's writer is gone.
    async fn poll_transcripts(&mut self) -> bool {
        let sessions: Vec<SessionId> = self.transcripts.keys().copied().collect();
        for session in sessions {
            let Some(path) = self.daemon.agents.lock().transcript_path(session) else {
                // No hook has named the file yet; try again on the next tick.
                continue;
            };
            let Some(mut tail) = self.transcripts.remove(&session) else { continue };
            let first = tail == Tail::default();
            let read = tokio::task::spawn_blocking(move || {
                let read = tail.read(&path);
                (tail, read)
            })
            .await;
            let Ok((tail, read)) = read else { continue };
            self.transcripts.insert(session, tail);
            let read = match read {
                Ok(read) => read,
                Err(e) => {
                    tracing::warn!(%session, error = %e, "transcript read");
                    continue;
                }
            };
            let reset = first || read.restarted;
            if read.entries.is_empty() && !reset {
                continue;
            }
            let mut entries = read.entries;
            if reset && entries.len() > TRANSCRIPT_SNAPSHOT {
                entries.drain(..entries.len().saturating_sub(TRANSCRIPT_SNAPSHOT));
            }
            let update = TranscriptUpdate { session, reset, entries };
            if self.out.send(HostMsg::Transcript(update)).await.is_err() {
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
                let on_stop = move |e: slopty_capture::CaptureError| {
                    let reason = e.to_string();
                    let event = ScreenEvent::Closed { stream: id, reason };
                    let _sent = out.try_send(HostMsg::Screen(event));
                };
                let opened = ScreenStream::open(
                    id,
                    target,
                    quality,
                    self.datagrams.clone(),
                    self.budget.clone(),
                    on_stop,
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
                        slopty_net::endpoint::selected_path(&self.conn).map(|(rtt, cwnd)| {
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
            ScreenRequest::Clipboard { text } => {
                if self.screens.is_empty() || text.len() > MAX_CLIPBOARD_BYTES {
                    tracing::debug!(client = %self.client, bytes = text.len(), "clipboard refused");
                } else if self.daemon.pasteboard.write(&text) {
                    tracing::debug!(client = %self.client, bytes = text.len(), "pasteboard set");
                } else {
                    tracing::warn!(client = %self.client, "pasteboard write failed");
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
                        for delta in self.daemon.canvas.remove_session(session, self.client) {
                            let _sent = self.daemon.events.send(HostMsg::Canvas(delta));
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
        let (sink, mut events) = mpsc::channel::<TermEvent>(SINK_DEPTH);
        if let Err(e) = handle.attach(self.client, size, sink.clone()) {
            return self.report(session, &e).await;
        }
        let client = self.client;
        let task = tokio::spawn(async move {
            let mut stream = stream;
            while let Some(ev) = events.recv().await {
                if let Err(e) = stream.send(&ev).await {
                    tracing::debug!(%client, %session, error = %e, "session stream ended");
                    break;
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

    use super::{Backlog, HOLD_POLL};

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
