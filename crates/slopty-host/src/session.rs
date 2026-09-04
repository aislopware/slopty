//! The session actor.

use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use slopty_core::{ClientId, SessionId};
use slopty_engine::{EngineConfig, EngineEvent, GhosttyEngine, VtEngine};
use slopty_proto::terminal::{TermEvent, TermRequest, TermSize};
use slopty_pty::PtyMaster;
use tokio::sync::{mpsc, oneshot};

use crate::HostError;

/// Output is coalesced into at most one frame per this interval while it keeps arriving; an
/// idle burst is flushed immediately on the next tick.
const COALESCE: Duration = Duration::from_millis(2);
/// PTY read buffer.
const READ_BUF: usize = 64 << 10;

/// Where a client's events go. Bounded: a client that cannot keep up gets dropped rather than
/// stalling the session (it can reattach and receive a full frame).
pub type ClientSink = mpsc::Sender<TermEvent>;

/// Commands into the actor.
enum Cmd {
    Attach { client: ClientId, size: TermSize, sink: ClientSink },
    Detach { client: ClientId, sink: Option<ClientSink> },
    Request { client: ClientId, req: TermRequest },
    Snapshot { reply: oneshot::Sender<Snapshot> },
    Close,
}

/// What `Host` reports about a session.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Snapshot {
    /// Title from OSC 0/2, if any.
    pub title: Option<String>,
    /// Cwd from OSC 7, if any.
    pub cwd: Option<String>,
    /// Size.
    pub size: TermSize,
    /// Attached clients.
    pub viewers: u16,
    /// Exit status if the child is gone.
    pub exited: Option<i32>,
}

/// Cloneable handle to a running session actor.
#[derive(Clone, Debug)]
pub struct SessionHandle {
    id: SessionId,
    tx: mpsc::UnboundedSender<Cmd>,
}

impl std::fmt::Debug for Cmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Attach { .. } => "Attach",
            Self::Detach { .. } => "Detach",
            Self::Request { .. } => "Request",
            Self::Snapshot { .. } => "Snapshot",
            Self::Close => "Close",
        };
        f.write_str(name)
    }
}

impl SessionHandle {
    /// Session id.
    #[must_use]
    pub const fn id(&self) -> SessionId {
        self.id
    }

    /// Attach a client; it receives a full frame first.
    pub fn attach(
        &self,
        client: ClientId,
        size: TermSize,
        sink: ClientSink,
    ) -> Result<(), HostError> {
        self.send(Cmd::Attach { client, size, sink })
    }

    /// Detach a client, whatever sink it is attached through.
    pub fn detach(&self, client: ClientId) -> Result<(), HostError> {
        self.send(Cmd::Detach { client, sink: None })
    }

    /// Detach a client only if it is still attached through `sink`.
    ///
    /// A connection that dies after the same client reconnected (a relaunched app whose old
    /// QUIC connection idles out) must not evict the new connection's viewer.
    pub fn detach_sink(&self, client: ClientId, sink: &ClientSink) -> Result<(), HostError> {
        self.send(Cmd::Detach { client, sink: Some(sink.clone()) })
    }

    /// Forward a terminal request from a client.
    pub fn request(&self, client: ClientId, req: TermRequest) -> Result<(), HostError> {
        self.send(Cmd::Request { client, req })
    }

    /// Current state.
    pub async fn snapshot(&self) -> Result<Snapshot, HostError> {
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::Snapshot { reply })?;
        rx.await.map_err(|_gone| HostError::SessionClosed)
    }

    /// Stop the actor (the PTY master closes; ptyd decides the child's fate).
    pub fn close(&self) {
        let _ignored = self.tx.send(Cmd::Close);
    }

    fn send(&self, cmd: Cmd) -> Result<(), HostError> {
        self.tx.send(cmd).map_err(|_gone| HostError::SessionClosed)
    }
}

/// What the actor needs to start.
#[derive(Debug)]
pub struct SessionStart {
    /// Id.
    pub id: SessionId,
    /// The PTY master from ptyd.
    pub master: OwnedFd,
    /// Output produced before we attached (replayed through the engine, never sent raw).
    pub backlog: Vec<u8>,
    /// Size of record.
    pub size: TermSize,
    /// Scrollback lines to retain.
    pub scrollback_lines: u32,
}

/// Spawn the actor thread.
pub fn spawn(start: SessionStart) -> Result<SessionHandle, HostError> {
    let (tx, rx) = mpsc::unbounded_channel();
    let id = start.id;
    let handle = SessionHandle { id, tx };
    thread::Builder::new()
        .name(format!("session-{id}"))
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!(session = %id, error = %e, "runtime");
                    return;
                }
            };
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                match Actor::new(start, rx) {
                    Ok(actor) => actor.run().await,
                    Err(e) => tracing::error!(session = %id, error = %e, "session start failed"),
                }
            });
        })
        .map_err(|e| {
            HostError::Pty(slopty_pty::PtyError::Os { context: "spawn session thread", source: e })
        })?;
    Ok(handle)
}

struct Viewer {
    client: ClientId,
    sink: ClientSink,
    size: TermSize,
}

struct Actor {
    id: SessionId,
    engine: GhosttyEngine,
    master: Arc<PtyMaster>,
    rx: mpsc::UnboundedReceiver<Cmd>,
    viewers: Vec<Viewer>,
    driver: Option<ClientId>,
    title: Option<String>,
    cwd: Option<String>,
    exited: Option<i32>,
    /// Highest key seq written to the PTY.
    written_seq: u64,
    /// `written_seq` as of the most recent PTY read: what the next frame may acknowledge.
    ack_seq: u64,
    frame_due: Option<tokio::time::Instant>,
}

impl Actor {
    fn new(start: SessionStart, rx: mpsc::UnboundedReceiver<Cmd>) -> Result<Self, HostError> {
        let mut engine = GhosttyEngine::new(EngineConfig {
            size: start.size,
            scrollback_lines: start.scrollback_lines,
        })?;
        if !start.backlog.is_empty() {
            engine.write(&start.backlog);
        }
        let master = Arc::new(PtyMaster::new(start.master)?);
        Ok(Self {
            id: start.id,
            engine,
            master,
            rx,
            viewers: Vec::new(),
            driver: None,
            title: None,
            cwd: None,
            exited: None,
            written_seq: 0,
            ack_seq: 0,
            frame_due: None,
        })
    }

    async fn run(mut self) {
        let mut buf = vec![0_u8; READ_BUF];
        let reader = Arc::clone(&self.master);
        loop {
            let frame_timer = async {
                match self.frame_due {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                biased;
                cmd = self.rx.recv() => {
                    let Some(cmd) = cmd else { break };
                    if !self.handle(cmd).await {
                        break;
                    }
                }
                read = reader.read(&mut buf), if self.exited.is_none() => match read {
                    Ok(0) => {
                        tracing::info!(session = %self.id, "pty closed");
                        self.flush_frame();
                        self.exited = Some(self.exited.unwrap_or(0));
                        self.broadcast(&TermEvent::Exited { status: 0 });
                    }
                    Ok(n) => {
                        self.ack_seq = self.written_seq;
                        self.engine.write(buf.get(..n).unwrap_or_default());
                        self.after_output().await;
                        if self.frame_due.is_none() {
                            self.frame_due = Some(tokio::time::Instant::now().checked_add(COALESCE).unwrap_or_else(tokio::time::Instant::now));
                        }
                    }
                    Err(e) => {
                        tracing::warn!(session = %self.id, error = %e, "pty read failed");
                        self.exited = Some(-1);
                        self.broadcast(&TermEvent::Exited { status: -1 });
                    }
                },
                () = frame_timer => {
                    self.frame_due = None;
                    self.flush_frame();
                }
            }
        }
        tracing::debug!(session = %self.id, "actor stopped");
    }

    /// Side effects of the bytes just consumed.
    async fn after_output(&mut self) {
        for ev in self.engine.drain_events() {
            match ev {
                EngineEvent::PtyWrite(bytes) => {
                    if let Err(e) = self.master.write_all(&bytes).await {
                        tracing::warn!(session = %self.id, error = %e, "query response write failed");
                    }
                }
                EngineEvent::Bell => self.broadcast(&TermEvent::Bell),
                EngineEvent::Title(t) => {
                    self.title = Some(t.clone());
                    self.broadcast(&TermEvent::Title(t));
                }
                EngineEvent::Cwd(c) => {
                    self.cwd = Some(c.clone());
                    self.broadcast(&TermEvent::Cwd(c));
                }
                EngineEvent::ClipboardWrite { text } => {
                    self.broadcast(&TermEvent::ClipboardWrite { text });
                }
            }
        }
    }

    fn flush_frame(&mut self) {
        if self.viewers.is_empty() {
            // Still consume dirty state so the next attach gets a clean full frame.
            let _consumed = self.engine.take_frame(self.ack_seq);
            return;
        }
        match self.engine.take_frame(self.ack_seq) {
            Ok(Some(frame)) => self.broadcast(&TermEvent::Frame(frame)),
            Ok(None) => {}
            Err(e) => {
                tracing::error!(session = %self.id, error = %e, "frame build failed");
                self.broadcast(&TermEvent::Error(e.to_string()));
            }
        }
    }

    fn broadcast(&mut self, ev: &TermEvent) {
        let mut dead = Vec::new();
        for (i, v) in self.viewers.iter().enumerate() {
            match v.sink.try_send(ev.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::warn!(session = %self.id, client = %v.client, "client cannot keep up; detaching");
                    dead.push(i);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => dead.push(i),
            }
        }
        for i in dead.into_iter().rev() {
            let v = self.viewers.swap_remove(i);
            self.on_viewer_gone(v.client);
        }
    }

    fn send_to(&self, client: ClientId, ev: TermEvent) {
        if let Some(v) = self.viewers.iter().find(|v| v.client == client) {
            let _ignored = v.sink.try_send(ev);
        }
    }

    fn on_viewer_gone(&mut self, client: ClientId) {
        if self.driver == Some(client) {
            self.driver = None;
            // Hand the size to the first remaining viewer, so a phone that joined after the
            // laptop left still gets a fitting terminal.
            if let Some(next) = self.viewers.first().map(|v| (v.client, v.size)) {
                self.driver = Some(next.0);
                self.send_to(next.0, TermEvent::Driver { you: true });
                self.apply_size(next.1);
            }
        }
    }

    fn apply_size(&mut self, size: TermSize) {
        if size == self.engine.size() {
            return;
        }
        if let Err(e) = slopty_pty::pty::set_size(self.master.as_fd(), size) {
            tracing::warn!(session = %self.id, error = %e, "TIOCSWINSZ failed");
        }
        if let Err(e) = self.engine.resize(size) {
            tracing::error!(session = %self.id, error = %e, "engine resize failed");
            return;
        }
        self.broadcast(&TermEvent::Resized { cols: size.cols, rows: size.rows });
        self.send_full_frame_to_all();
    }

    fn send_full_frame_to_all(&mut self) {
        match self.engine.full_frame(self.ack_seq) {
            Ok(frame) => self.broadcast(&TermEvent::Frame(frame)),
            Err(e) => tracing::error!(session = %self.id, error = %e, "full frame failed"),
        }
    }

    /// Returns `false` when the actor should stop.
    async fn handle(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Attach { client, size, sink } => {
                self.viewers.retain(|v| v.client != client);
                self.viewers.push(Viewer { client, sink, size });
                if self.driver.is_none() {
                    self.driver = Some(client);
                    self.send_to(client, TermEvent::Driver { you: true });
                    self.apply_size(size);
                }
                if let Some(t) = &self.title {
                    self.send_to(client, TermEvent::Title(t.clone()));
                }
                if let Some(c) = &self.cwd {
                    self.send_to(client, TermEvent::Cwd(c.clone()));
                }
                match self.engine.full_frame(self.ack_seq) {
                    Ok(frame) => self.send_to(client, TermEvent::Frame(frame)),
                    Err(e) => self.send_to(client, TermEvent::Error(e.to_string())),
                }
                if let Some(status) = self.exited {
                    self.send_to(client, TermEvent::Exited { status });
                }
            }
            Cmd::Detach { client, sink } => {
                let before = self.viewers.len();
                self.viewers.retain(|v| {
                    v.client != client || sink.as_ref().is_some_and(|s| !s.same_channel(&v.sink))
                });
                if self.viewers.len() != before {
                    self.on_viewer_gone(client);
                }
            }
            Cmd::Request { client, req } => self.request(client, req).await,
            Cmd::Snapshot { reply } => {
                let _ignored = reply.send(Snapshot {
                    title: self.title.clone(),
                    cwd: self.cwd.clone(),
                    size: self.engine.size(),
                    viewers: u16::try_from(self.viewers.len()).unwrap_or(u16::MAX),
                    exited: self.exited,
                });
            }
            Cmd::Close => return false,
        }
        true
    }

    async fn request(&mut self, client: ClientId, req: TermRequest) {
        let mut bytes = Vec::new();
        let result = match req {
            TermRequest::Attach { .. } | TermRequest::Detach | TermRequest::Close => Ok(()),
            TermRequest::Resize(size) => {
                if let Some(v) = self.viewers.iter_mut().find(|v| v.client == client) {
                    v.size = size;
                }
                if self.driver == Some(client) {
                    self.apply_size(size);
                }
                Ok(())
            }
            TermRequest::Drive { drive } => {
                if drive {
                    if let Some(old) = self.driver.replace(client)
                        && old != client
                    {
                        self.send_to(old, TermEvent::Driver { you: false });
                    }
                    self.send_to(client, TermEvent::Driver { you: true });
                    if let Some(size) =
                        self.viewers.iter().find(|v| v.client == client).map(|v| v.size)
                    {
                        self.apply_size(size);
                    }
                } else if self.driver == Some(client) {
                    self.driver = None;
                    self.send_to(client, TermEvent::Driver { you: false });
                }
                Ok(())
            }
            TermRequest::Key(key) => {
                let seq = key.seq;
                let r = self.engine.encode_key(&key, &mut bytes);
                if r.is_ok() && !bytes.is_empty() {
                    self.written_seq = self.written_seq.max(seq);
                }
                r
            }
            TermRequest::Mouse(m) => self.engine.encode_mouse(&m, &mut bytes),
            TermRequest::Paste(text) => self.engine.encode_paste(&text, &mut bytes),
            TermRequest::Raw(raw) => {
                bytes = raw;
                Ok(())
            }
            TermRequest::Focus { focused } => self.engine.encode_focus(focused, &mut bytes),
            TermRequest::FetchLines { start, count } => {
                match self.engine.lines(start, count.min(4096)) {
                    Ok((start, lines)) => self.send_to(client, TermEvent::Lines { start, lines }),
                    Err(e) => self.send_to(client, TermEvent::Error(e.to_string())),
                }
                Ok(())
            }
            TermRequest::ClipboardRead { text } => {
                // OSC 52 reply: base64 of the text, standard clipboard selection.
                bytes = format!("\x1b]52;c;{}\x1b\\", base64(text.as_bytes())).into_bytes();
                Ok(())
            }
        };
        if let Err(e) = result {
            self.send_to(client, TermEvent::Error(e.to_string()));
            return;
        }
        if !bytes.is_empty()
            && let Err(e) = self.master.write_all(&bytes).await
        {
            tracing::warn!(session = %self.id, error = %e, "pty write failed");
            self.send_to(client, TermEvent::Error(e.to_string()));
        }
    }
}

fn base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3).saturating_mul(4));
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk.first().copied().unwrap_or(0));
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        let idx = |shift: u32| usize::try_from((n >> shift) & 63).unwrap_or(0);
        let ch = |i: usize| char::from(TABLE.get(i).copied().unwrap_or(b'='));
        out.push(ch(idx(18)));
        out.push(ch(idx(12)));
        out.push(if chunk.len() > 1 { ch(idx(6)) } else { '=' });
        out.push(if chunk.len() > 2 { ch(idx(0)) } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
