//! One running Claude Code and its thread: the task that carries its stream-json both ways
//! until it ends or the worker goes.
//!
//! The task is the session's one writer. What a client asks becomes control requests and user
//! messages through the codec (`slopty_agent::claude::driven`); what Claude Code writes becomes
//! the thread's actions. A worker that goes leaves Claude Code running in ptyd's custody
//! ([`super::custody`]); a stop Claude Code does not acknowledge in time, or the end of the
//! agent, closes its session there.

use std::path::PathBuf;
use std::time::Duration;

use slopty_agent::claude::driven::{Answered, Driven};
use slopty_agent::claude::history::Out;
use slopty_core::{SessionId, WallMs};
use slopty_proto::thread::{Delivery, ThreadId, ThreadState};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::WriteHalf;
use tokio::sync::mpsc;

use super::custody::Custody;
use super::{Ended, ThreadAsk};
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
    /// to `stderr`.
    pub(super) const fn new(
        host: Host,
        thread: ThreadId,
        driven: Driven,
        custody: Custody,
        session: SessionId,
        stderr: PathBuf,
    ) -> Self {
        Self { host, thread, driven, custody, session, stderr }
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
        let mut status = None;
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
            tokio::select! {
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
                        status = Some(exit.status);
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

    /// Put `outs` into the host.
    fn take(&self, outs: Vec<Out>) {
        super::take(&self.host, outs);
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
            ThreadAsk::StopTask { agent } => self.driven.stop_task(&agent),
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
            // Handled by the serving loop.
            ThreadAsk::Close => {}
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
