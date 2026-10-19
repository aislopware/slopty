//! One running Claude Code and its thread: the task that carries its stream-json both ways
//! until it ends or the worker goes.
//!
//! The task is the session's one writer. What a client asks becomes control requests and user
//! messages through the codec (`slopty_agent::claude::driven`); what Claude Code writes becomes
//! the thread's actions. Its lines reach Claude Code's standard input through a writer of their
//! own ([`Stdin`]), so a Claude Code that stops reading never holds the thread: a stop still
//! lands, and past [`STDIN_MAX`] waiting it is ended. A worker that goes leaves Claude Code
//! running in ptyd's custody ([`super::custody`]); a stop Claude Code does not acknowledge in
//! time, or the end of the agent, closes its session there.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use slopty_agent::claude::driven::{Answered, Driven, RewindPreview};
use slopty_agent::claude::history::Out;
use slopty_core::{SessionId, WallMs};
use slopty_proto::thread::{Action, Delivery, ThreadId, ThreadState};
use slopty_proto::visual::AppAnswer;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};

use super::custody::Custody;
use super::{Ended, ThreadAsk, settings};
use crate::thread::Host;

/// How long Claude Code has, once its stdout closed, to be reported gone with its status.
const REAPED: Duration = Duration::from_secs(2);

/// How much of the end of Claude Code's standard error says why it failed.
const STDERR_TAIL: u64 = 4096;

/// How much may wait for a Claude Code to read its standard input before it is ended as stuck:
/// room for many messages with their images, never the worker's memory.
const STDIN_MAX: usize = 64 << 20;

/// Why a Claude Code that stopped reading was ended.
const STOPPED_READING: &str = "Claude Code stopped reading what it was sent, so Slopty ended it. \
                               Send a message to start it again.";

/// How long a worker that goes gives the writer to hand Claude Code the lines it has, so the
/// next worker does not find one cut short.
const LEFT_WRITES: Duration = Duration::from_secs(2);

/// Claude Code's standard input: lines go to a writer of their own on a duplicate of the socket.
struct Stdin {
    /// To the writer; `None` once it ended or Claude Code was ended as stuck.
    lines: Option<mpsc::UnboundedSender<Vec<u8>>>,
    /// Bytes sent to the writer and not yet written.
    queued: Arc<AtomicUsize>,
    writer: tokio::task::JoinHandle<()>,
}

impl Stdin {
    /// The writer of `thread`'s Claude Code on a duplicate of `socket`: a plain stream, not a
    /// write half, so it ending never ends Claude Code's input.
    fn new(socket: &UnixStream, thread: ThreadId) -> std::io::Result<Self> {
        use std::os::fd::AsFd as _;
        let fd = socket.as_fd().try_clone_to_owned()?;
        let stream = std::os::unix::net::UnixStream::from(fd);
        stream.set_nonblocking(true)?;
        let mut out = UnixStream::from_std(stream)?;
        let (lines, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let queued = Arc::new(AtomicUsize::new(0));
        let written = Arc::clone(&queued);
        let writer = tokio::spawn(async move {
            while let Some(bytes) = rx.recv().await {
                if let Err(e) = out.write_all(&bytes).await {
                    tracing::debug!(%thread, "Claude Code's stdin is closed: {e}");
                    return;
                }
                let _left = written.fetch_sub(bytes.len(), Ordering::AcqRel);
            }
        });
        Ok(Self { lines: Some(lines), queued, writer })
    }

    /// Hand `bytes` to the writer: `false` once more than [`STDIN_MAX`] waits.
    fn write(&mut self, bytes: Vec<u8>) -> bool {
        if self.lines.is_none() {
            return true;
        }
        let len = bytes.len();
        let queued = self.queued.fetch_add(len, Ordering::AcqRel).saturating_add(len);
        if queued > STDIN_MAX {
            self.lines = None;
            return false;
        }
        if self.lines.as_ref().is_some_and(|lines| lines.send(bytes).is_err()) {
            self.lines = None;
        }
        true
    }

    /// The writer, given [`LEFT_WRITES`] to write what it holds, and then stopped.
    async fn finish(mut self) {
        self.lines = None;
        if tokio::time::timeout(LEFT_WRITES, &mut self.writer).await.is_err() {
            tracing::warn!("Claude Code did not take its last lines: one may be cut short");
            self.writer.abort();
        }
    }
}

/// One running Claude Code and its thread.
pub(super) struct Task {
    host: Host,
    thread: ThreadId,
    driven: Driven,
    custody: Custody,
    /// Its session in ptyd.
    session: SessionId,
    /// Where its standard error goes.
    stderr: PathBuf,
    /// The home Claude Code keeps its settings and sessions under.
    home: PathBuf,
    /// The pages' asks of their MCP Apps under way, by number, and the next number.
    apps: (HashMap<u64, oneshot::Sender<AppAnswer>>, u64),
    /// The rewinds asked what they would put back, by number, and the next number.
    previews: (HashMap<u64, oneshot::Sender<RewindPreview>>, u64),
    /// When the person's status line runs again, if it is due.
    status_due: Option<tokio::time::Instant>,
    /// Whether a status line command still runs: the next waits for it.
    status_busy: Arc<AtomicBool>,
    /// Its standard input, once served.
    stdin: Option<Stdin>,
    /// Why it was ended, when the worker ended it for a reason of its own.
    ended_for: Option<&'static str>,
}

/// How a task ended.
enum End {
    /// Claude Code is gone.
    Gone,
    /// The worker is going: Claude Code stays with ptyd.
    Left,
}

impl Task {
    /// The task of `thread`'s Claude Code, session `session` in ptyd, its standard error going
    /// to `stderr`, its settings under `home`.
    pub(super) fn new(
        host: Host,
        thread: ThreadId,
        driven: Driven,
        custody: Custody,
        (session, stderr, home): (SessionId, PathBuf, PathBuf),
    ) -> Self {
        Self {
            host,
            thread,
            driven,
            custody,
            session,
            stderr,
            home,
            apps: (HashMap::new(), 0),
            previews: (HashMap::new(), 0),
            status_due: Some(tokio::time::Instant::now()),
            status_busy: Arc::new(AtomicBool::new(false)),
            stdin: None,
            ended_for: None,
        }
    }

    /// Serve the thread over `socket`, starting with `backlog`, what Claude Code wrote while no
    /// worker held it; `first`, the asks that wait for it. Until Claude Code goes or the worker
    /// does; then what was asked and not taken goes back by `ended`.
    pub(super) async fn serve(
        mut self,
        mut socket: UnixStream,
        backlog: Vec<u8>,
        first: Vec<ThreadAsk>,
        mut asks: mpsc::UnboundedReceiver<ThreadAsk>,
        ended: Ended,
    ) {
        // Stamped alive while Claude Code runs, and not after: the lease goes before the end
        // is told.
        let lease = self.host.lease(self.thread);
        match Stdin::new(&socket, self.thread) {
            Ok(stdin) => self.stdin = Some(stdin),
            Err(e) => {
                tracing::warn!(thread = %self.thread, "Claude Code's stdin is not reached: {e}");
            }
        }
        // Borrowed halves: an owned write half shuts the socket's writing down when dropped,
        // which is Claude Code's standard input ending for ptyd's copy too, so a worker that
        // goes would end the turn it means to leave running. The write half only ends it.
        let (read, write) = socket.split();
        let mut exits = self.custody.exits();
        let mut lines = BufReader::new(std::io::Cursor::new(backlog).chain(read)).split(b'\n');
        self.driven.initialize();
        self.flush().await;
        for ask in first {
            self.ask(ask).await;
        }
        let mut exit_status = None;
        let mut reaped_at = None;
        let end = loop {
            // A Claude Code reported gone whose socket a child of its own keeps open is not
            // waited on past [`REAPED`].
            let drained = async {
                match reaped_at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            let deadline = self.driven.deadline();
            let expiry = async {
                match deadline.and_then(WallMs::to_system) {
                    Some(at) => {
                        let wait =
                            at.duration_since(std::time::SystemTime::now()).unwrap_or_default();
                        tokio::time::sleep(wait).await;
                    }
                    None => std::future::pending().await,
                }
            };
            let status_due = self.status_due;
            let status = async {
                match status_due {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                () = status => self.status_line(),
                line = lines.next_segment() => match line {
                    Ok(Some(line)) => {
                        let outs = self.driven.line(&line, WallMs::now());
                        self.take(outs);
                        self.flush().await;
                        self.next().await;
                    }
                    Ok(None) | Err(_) => break End::Gone,
                },
                ask = asks.recv() => match ask {
                    Some(ThreadAsk::Close) => {
                        let outs = self.driven.ending(WallMs::now());
                        self.take(outs);
                        self.custody.close(self.session).await;
                    }
                    Some(ask) => self.ask(ask).await,
                    None => break End::Left,
                },
                () = expiry => {
                    let (outs, escalate) = self.driven.tick(WallMs::now());
                    self.take(outs);
                    self.flush().await;
                    if escalate {
                        tracing::info!(thread = %self.thread, "Claude Code did not stop in time: ended");
                        self.custody.close(self.session).await;
                    }
                    self.next().await;
                }
                exit = exits.recv() => {
                    if let Ok((session, exit)) = exit && session == self.session {
                        // Gone: no stamp after this, whatever its output still holds.
                        lease.stop();
                        exit_status = Some(exit.status);
                        reaped_at = Some(
                            tokio::time::Instant::now()
                                .checked_add(REAPED)
                                .unwrap_or_else(tokio::time::Instant::now),
                        );
                    }
                }
                () = drained => break End::Gone,
            }
        };
        asks.close();
        drop(lease);
        let mut left = Vec::new();
        while let Ok(ask) = asks.try_recv() {
            left.push(ask);
        }
        let stdin = self.stdin.take();
        if matches!(end, End::Left) {
            // Claude Code goes on in ptyd's keeping; the next worker takes it up, from whole
            // lines.
            if let Some(stdin) = stdin {
                stdin.finish().await;
            }
            self.custody.let_go(self.session);
            let _gone = ended.send((self.thread, left));
            return;
        }
        // Its standard input ends: one whose output closed and that still runs hears it.
        if let Some(stdin) = stdin {
            stdin.writer.abort();
        }
        let mut write = write;
        let _ended = write.shutdown().await;
        // A Claude Code that ended before `exits` was heard (one that cannot start) is told by
        // what ptyd said of it lately.
        let mut status = exit_status.or_else(|| self.custody.ended(self.session).map(|e| e.status));
        if status.is_none() {
            let reaped = async {
                loop {
                    match exits.recv().await {
                        Ok((session, exit)) if session == self.session => return exit.status,
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                    }
                }
            };
            status = tokio::time::timeout(REAPED, reaped).await.ok();
        }
        let status = status.flatten();
        let why = match self.ended_for {
            Some(why) => Some(why.to_owned()),
            None if status.is_some_and(|s| s != 0) => Some(self.why(status).await),
            None => None,
        };
        // ptyd forgets the session, and the thread its custody.
        self.custody.close(self.session).await;
        let mut outs = self.driven.exited(why.as_deref(), WallMs::now());
        outs.extend(self.driven.fact(slopty_agent::claude::driven::CUSTODY_FACT, None));
        self.take(outs);
        self.answer_apps();
        for (_app, reply) in self.apps.0.drain() {
            let _gone = reply.send(AppAnswer::Refused("Claude Code ended".to_owned()));
        }
        for (_preview, reply) in self.previews.0.drain() {
            let _gone = reply.send(Err("Claude Code ended".to_owned()));
        }
        let _gone = ended.send((self.thread, left));
    }

    /// Why Claude Code, which exited with `status`, failed: what kind of end it was, from that
    /// and the end of its standard error, in plain words with the one thing to do
    /// ([`slopty_agent::claude::driven::ending()`]).
    async fn why(&self, status: Option<i32>) -> String {
        let path = self.stderr.clone();
        let tail = tokio::task::spawn_blocking(move || tail_of(&path)).await.ok().flatten();
        slopty_agent::claude::driven::ending(status, tail.as_deref().unwrap_or_default()).words
    }

    /// Put `outs` into the host; the status line runs again a moment after anything changed.
    fn take(&mut self, outs: Vec<Out>) {
        if !outs.is_empty() && self.status_due.is_none() {
            self.status_due = tokio::time::Instant::now().checked_add(settings::STATUS_EVERY);
        }
        super::take(&self.host, outs);
        self.answer_apps();
    }

    /// Give the pages what their MCP Apps answered, and the rewinds what they would put back.
    fn answer_apps(&mut self) {
        for (app, answer) in self.driven.take_app_answers() {
            if let Some(reply) = self.apps.0.remove(&app) {
                let _gone = reply.send(answer);
            }
        }
        for (preview, answer) in self.driven.take_rewind_previews() {
            if let Some(reply) = self.previews.0.remove(&preview) {
                let _gone = reply.send(answer);
            }
        }
    }

    /// Run the person's status line command, if they set one, on the thread as it is now, on a
    /// task of its own; its line goes into the thread when it changed. One runs at a time.
    fn status_line(&mut self) {
        self.status_due = None;
        let Some((state, _)) = self.host.state(self.thread) else { return };
        let cwd = PathBuf::from(&state.meta.cwd);
        let Some(command) = settings::status_command(&self.home, &cwd) else {
            if state.status_line.is_some() {
                self.host.apply(self.thread, vec![Action::StatusLine(None)]);
            }
            return;
        };
        if self.status_busy.swap(true, Ordering::AcqRel) {
            // The one running sees an older thread: run again after it.
            self.status_due = tokio::time::Instant::now().checked_add(settings::STATUS_EVERY);
            return;
        }
        let transcript =
            slopty_agent::claude::sessions::transcript(&self.home, &cwd, &state.meta.native);
        let input = settings::status_input(&state, &transcript);
        let (host, thread, busy) = (self.host.clone(), self.thread, Arc::clone(&self.status_busy));
        tokio::spawn(async move {
            let line = settings::status_line(&command, &input, &cwd).await;
            busy.store(false, Ordering::Release);
            if host.state(thread).is_some_and(|(s, _)| s.status_line != line) {
                host.apply(thread, vec![Action::StatusLine(line)]);
            }
        });
    }

    /// Hand what the codec has for Claude Code to its writer; one with more than [`STDIN_MAX`]
    /// unread is ended.
    async fn flush(&mut self) {
        let lines = self.driven.take_writes();
        let Some(stdin) = self.stdin.as_mut() else { return };
        for line in lines {
            let mut bytes = line.into_bytes();
            bytes.push(b'\n');
            if !stdin.write(bytes) {
                tracing::warn!(thread = %self.thread, "Claude Code stopped reading its stdin: ended");
                self.ended_for = Some(STOPPED_READING);
                self.custody.close(self.session).await;
                return;
            }
        }
    }

    async fn ask(&mut self, ask: ThreadAsk) {
        let now = WallMs::now();
        match ask {
            ThreadAsk::Send { text, attachments, intent, delivery } => {
                // The person speaking again lets what their stop held go, after this.
                let released = self.driven.queue().release();
                let outs = match delivery {
                    Delivery::Queue if self.driven.busy() || released => {
                        self.driven.hold(intent, &text, attachments)
                    }
                    _ => {
                        let attached = crate::thread::attach::read(&attachments).await;
                        self.driven.send(&text, &attached, intent, now)
                    }
                };
                self.take(outs);
            }
            ThreadAsk::Report { text } => {
                let (_uuid, outs) = self.driven.report(&text, now);
                self.take(outs);
            }
            ThreadAsk::Withdraw { intent } => {
                let _shown = self.driven.queue().withdraw(intent);
                let outs = self.driven.queue_changed();
                self.take(outs);
            }
            ThreadAsk::Edit { intent, text, attachments } => {
                let _shown = self.driven.queue().edit(intent, &text, attachments, ());
                let outs = self.driven.queue_changed();
                self.take(outs);
            }
            ThreadAsk::Move { intent, before } => {
                let _shown = self.driven.queue().move_to(intent, before);
                let outs = self.driven.queue_changed();
                self.take(outs);
            }
            // Into the turn under way, or a turn of its own; what a stop held goes after it.
            ThreadAsk::Promote { intent } => {
                let Some((held, ())) = self.driven.queue().take(intent) else { return };
                self.driven.queue().release();
                let attached = crate::thread::attach::read(&held.attachments).await;
                let outs = self.driven.send(&held.text, &attached, intent, now);
                self.take(outs);
            }
            ThreadAsk::Interrupt => {
                let outs = self.driven.interrupt(now);
                self.take(outs);
            }
            ThreadAsk::StopTask { task } => self.driven.stop_task(&task),
            ThreadAsk::Background { item } => self.driven.background(item.as_deref()),
            ThreadAsk::ShellDone { ran, intent, quiet } => {
                let outs = self.driven.shell(&ran, intent, !quiet, now);
                self.take(outs);
            }
            ThreadAsk::Rewind { turn } => {
                let outs = self.driven.rewind(turn, now);
                self.take(outs);
            }
            ThreadAsk::RewindConversation { turn } => {
                let outs = self.driven.rewind_conversation(turn, now);
                self.take(outs);
            }
            ThreadAsk::RewindPreview { turn, reply } => {
                let preview = self.previews.1;
                self.previews.1 = preview.wrapping_add(1);
                self.previews.0.insert(preview, reply);
                self.driven.preview_rewind(preview, turn);
                self.answer_apps();
            }
            ThreadAsk::Side { question, intent, history } => {
                let outs = self.driven.side(intent, &question, &history, now);
                self.take(outs);
            }
            ThreadAsk::Rename { title } => self.driven.rename(&title),
            ThreadAsk::McpReconnect { server } => self.driven.mcp_reconnect(&server),
            ThreadAsk::McpEnable { server, enabled } => self.driven.mcp_enable(&server, enabled),
            // Written into its file by now: what Claude Code holds is read again.
            ThreadAsk::Rule { .. } => self.driven.list_rules(),
            ThreadAsk::Surface { surface, element, event } => {
                match self.driven.surface(&surface, &element, &event) {
                    Some(outs) => self.take(outs),
                    None => {
                        tracing::debug!(thread = %self.thread, surface, element, "no such element");
                    }
                }
            }
            ThreadAsk::App { ask, reply } => {
                let app = self.apps.1;
                self.apps.1 = app.wrapping_add(1);
                self.apps.0.insert(app, reply);
                self.driven.app(app, &ask);
            }

            ThreadAsk::Answer { ask, choice, message, by } => {
                let answered = self.driven.answer(&ask, &choice, message.as_deref(), by, now);
                match answered {
                    Some(Answered { outs }) => self.take(outs),
                    None => {
                        tracing::debug!(thread = %self.thread, ask = ask.0, "an answer Claude Code no longer takes");
                    }
                }
            }
            ThreadAsk::SetModel { model } => self.driven.set_model(&model),
            ThreadAsk::SetMode { mode } => self.driven.set_mode(&mode),
            ThreadAsk::SetEffort { effort } => self.driven.set_effort(&effort),
            ThreadAsk::SetOption { option, choice } => {
                if !self.driven.set_option(&option, &choice) {
                    tracing::debug!(thread = %self.thread, option, choice, "an option Claude Code does not take");
                }
            }
            // Handled by the serving loop; a `!` command, by the worker before it comes here as
            // [`ThreadAsk::ShellDone`].
            ThreadAsk::Close | ThreadAsk::Shell { .. } => {}
        }
        self.flush().await;
        self.next().await;
    }

    /// The next message held goes, once Claude Code has nothing else to do.
    async fn next(&mut self) {
        let Some(next) = self.driven.next_queued() else { return };
        let attached = crate::thread::attach::read(&next.attachments).await;
        let outs = self.driven.send(&next.text, &attached, next.intent, WallMs::now());
        self.take(outs);
        self.flush().await;
    }
}

/// The end of the file at `path`: its last [`STDERR_TAIL`] bytes.
fn tail_of(path: &std::path::Path) -> Option<String> {
    use std::io::{Read as _, Seek as _};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(std::io::SeekFrom::Start(len.saturating_sub(STDERR_TAIL))).ok()?;
    let mut bytes = Vec::new();
    file.take(STDERR_TAIL).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The codec of thread `state` read again from its session's `records`, Claude Code still at
/// its turn when `running`: the codec, and the outs that tell the thread again.
pub(super) fn read_again(
    state: &ThreadState,
    records: &[serde_json::Value],
    running: bool,
) -> (Driven, Vec<Out>) {
    let now = WallMs::now();
    let mut driven = Driven::of(state, now);
    let outs = driven.replay(records, running, now);
    (driven, outs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Claude Code that reads nothing: its lines wait with the writer, never the task, until
    /// more than [`STDIN_MAX`] waits; then it is said stuck. A line on a closed input is let go.
    #[tokio::test]
    async fn lines_a_claude_code_does_not_read_wait_up_to_the_cap() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let mut stdin = Stdin::new(&ours, ThreadId::new()).unwrap();
        let half = vec![b'x'; STDIN_MAX / 2];
        let began = std::time::Instant::now();
        assert!(stdin.write(half.clone()), "the first half waits");
        assert!(stdin.write(half.clone()), "the second half too: the cap is not passed");
        assert!(!stdin.write(vec![b'x'; 2]), "past the cap it is stuck");
        assert!(began.elapsed() < Duration::from_secs(1), "never waited on: {:?}", began.elapsed());
        assert!(stdin.write(b"after\n".to_vec()), "once stuck, lines are let go");
        drop(theirs);
        stdin.finish().await;
    }
}
