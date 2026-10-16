//! One running Claude Code and its thread: the task that carries its stream-json both ways
//! until it ends or the worker goes.
//!
//! The task is the session's one writer. What a client asks becomes control requests and user
//! messages through the codec (`slopty_agent::claude::driven`); what Claude Code writes becomes
//! the thread's actions. A worker that goes leaves Claude Code running in ptyd's custody
//! ([`super::custody`]); a stop Claude Code does not acknowledge in time, or the end of the
//! agent, closes its session there.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use slopty_agent::claude::driven::{Answered, Driven};
use slopty_agent::claude::history::Out;
use slopty_core::{SessionId, WallMs};
use slopty_proto::thread::{Action, Delivery, ThreadId, ThreadState};
use slopty_proto::visual::AppAnswer;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::WriteHalf;
use tokio::sync::{mpsc, oneshot};

use super::custody::Custody;
use super::{Ended, ThreadAsk, settings};
use crate::thread::Host;

/// How long Claude Code has, once its stdout closed, to be reported gone with its status.
const REAPED: Duration = Duration::from_secs(2);

/// How much of the end of Claude Code's standard error says why it failed.
const STDERR_TAIL: u64 = 4096;

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
    /// When the person's status line runs again, if it is due.
    status_due: Option<tokio::time::Instant>,
    /// Whether a status line command still runs: the next waits for it.
    status_busy: Arc<AtomicBool>,
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
            status_due: Some(tokio::time::Instant::now()),
            status_busy: Arc::new(AtomicBool::new(false)),
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
        // Borrowed halves: an owned write half shuts the socket's writing down when dropped,
        // which is Claude Code's standard input ending for ptyd's copy too, so a worker that
        // goes would end the turn it means to leave running.
        let (read, write) = socket.split();
        let mut write = Some(write);
        let mut exits = self.custody.exits();
        let mut lines = BufReader::new(std::io::Cursor::new(backlog).chain(read)).split(b'\n');
        self.driven.initialize();
        self.flush(&mut write).await;
        for ask in first {
            self.ask(ask, &mut write).await;
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
                        self.flush(&mut write).await;
                        self.next(&mut write).await;
                    }
                    Ok(None) | Err(_) => break End::Gone,
                },
                ask = asks.recv() => match ask {
                    Some(ThreadAsk::Close) => {
                        let outs = self.driven.ending(WallMs::now());
                        self.take(outs);
                        self.custody.close(self.session).await;
                    }
                    Some(ask) => self.ask(ask, &mut write).await,
                    None => break End::Left,
                },
                () = expiry => {
                    let (outs, escalate) = self.driven.tick(WallMs::now());
                    self.take(outs);
                    self.flush(&mut write).await;
                    if escalate {
                        tracing::info!(thread = %self.thread, "Claude Code did not stop in time: ended");
                        self.custody.close(self.session).await;
                    }
                    self.next(&mut write).await;
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
        if matches!(end, End::Left) {
            // Claude Code goes on in ptyd's keeping; the next worker takes it up.
            self.custody.let_go(self.session);
            let _gone = ended.send((self.thread, left));
            return;
        }
        // Its standard input ends: one whose output closed and that still runs hears it.
        if let Some(mut out) = write.take() {
            let _ended = out.shutdown().await;
        }
        let mut status = exit_status;
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
        let failed = status.flatten().is_some_and(|s| s != 0);
        let why = if failed { Some(self.why().await) } else { None };
        // ptyd forgets the session, and the thread its custody.
        self.custody.close(self.session).await;
        let mut outs = self.driven.exited(why.as_deref(), WallMs::now());
        outs.extend(self.driven.fact(slopty_agent::claude::driven::CUSTODY_FACT, None));
        self.take(outs);
        self.answer_apps();
        for (_app, reply) in self.apps.0.drain() {
            let _gone = reply.send(AppAnswer::Refused("Claude Code ended".to_owned()));
        }
        let _gone = ended.send((self.thread, left));
    }

    /// Why Claude Code failed: the last line of its standard error, else its code.
    async fn why(&self) -> String {
        let path = self.stderr.clone();
        let tail = tokio::task::spawn_blocking(move || tail_of(&path)).await.ok().flatten();
        tail.map_or_else(
            || "Claude Code ended".to_owned(),
            |line| format!("Claude Code ended: {line}"),
        )
    }

    /// Put `outs` into the host; the status line runs again a moment after anything changed.
    fn take(&mut self, outs: Vec<Out>) {
        if !outs.is_empty() && self.status_due.is_none() {
            self.status_due = tokio::time::Instant::now().checked_add(settings::STATUS_EVERY);
        }
        super::take(&self.host, outs);
        self.answer_apps();
    }

    /// Give the pages what their MCP Apps answered.
    fn answer_apps(&mut self) {
        for (app, answer) in self.driven.take_app_answers() {
            if let Some(reply) = self.apps.0.remove(&app) {
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

    /// Write what the codec has for Claude Code.
    async fn flush(&mut self, write: &mut Option<WriteHalf<'_>>) {
        let lines = self.driven.take_writes();
        let Some(out) = write.as_mut() else { return };
        for line in lines {
            let mut bytes = line.into_bytes();
            bytes.push(b'\n');
            if let Err(e) = out.write_all(&bytes).await {
                tracing::debug!(thread = %self.thread, "Claude Code's stdin is closed: {e}");
                *write = None;
                return;
            }
        }
    }

    async fn ask(&mut self, ask: ThreadAsk, write: &mut Option<WriteHalf<'_>>) {
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
        self.flush(write).await;
        self.next(write).await;
    }

    /// The next message held goes, once Claude Code has nothing else to do.
    async fn next(&mut self, write: &mut Option<WriteHalf<'_>>) {
        let Some(next) = self.driven.next_queued() else { return };
        let attached = crate::thread::attach::read(&next.attachments).await;
        let outs = self.driven.send(&next.text, &attached, next.intent, WallMs::now());
        self.take(outs);
        self.flush(write).await;
    }
}

/// The last line of the file at `path` that says anything, from its last [`STDERR_TAIL`] bytes.
fn tail_of(path: &std::path::Path) -> Option<String> {
    use std::io::{Read as _, Seek as _};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(std::io::SeekFrom::Start(len.saturating_sub(STDERR_TAIL))).ok()?;
    let mut bytes = Vec::new();
    file.take(STDERR_TAIL).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    text.lines().rev().map(str::trim).find(|l| !l.is_empty()).map(str::to_owned)
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
