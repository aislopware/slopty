//! The conversation face: a client following an agent's session, and the permission prompts
//! held for it.
//!
//! **The stream.** Each followed session gets a task of its own on the connection
//! ([`stream`]). It opens a conversation stream below the terminals and video
//! (`slopty_net::streams::CONVERSATION_PRIORITY`) and sends the conversation: first everything
//! there is, behind a reset and closed by `ConversationEvent::Current`, then each change, and
//! the status line's meters when they move. The session's transcripts are read once for all
//! its followers (`slopty_worker::conversation::Reader`, off the runtime on the blocking pool):
//! a follower that joins reads what they gained and takes the conversation as it stands, and a
//! task of the session's own reads them on a tick and at once when a hook fires, sending each
//! read's changes to every follower. A follower that falls too far behind is sent everything
//! again. Where the agent runs Slopty's mod, a follower also sends the blocks the model is
//! writing (`ConversationEvent::Live`) as the mod reports them (`slopty_agent::live::Overlay`),
//! each cleared after the transcript change that settles it. The same reads tail the files the
//! session's background commands write (`slopty_agent::conversation::output`), and a follower
//! sends what they printed (`ConversationEvent::Output`) and answers a request for an image's
//! bytes (`ConversationEvent::Image`). Nothing here is on a terminal's path: only the reader's
//! own lock, which only the session's followers wait on, is held across a read, none across a
//! send, every file is read on the blocking pool, and the stream's writes wait on nobody but
//! this client.
//!
//! **Held prompts.** The relay's `CtlRequest::Permission`, once the control socket has taken
//! its hook in, comes to [`ask`]. The daemon's
//! [`Follows::holds`] (`slopty_worker::conversation::Holds`) decides: undecided at once when
//! nobody follows, else held and shown to the followers (`WorkerMsg::Permission`, on the control
//! stream: small and urgent), until a follower answers ([`answer`]), the last one leaves, the
//! wait runs out or the relay goes away ([`release`]).
//!
//! **Orchestration** follows too, as `ORCHESTRATION` ([`Orchestrated`]): the sessions whose
//! conversation a verb read or whose agent a verb started, until they end.

use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use slopty_agent::live::Overlay;
use slopty_agent::{Hook, permission};
use slopty_core::{ClientId, SessionId, WallMs};
use slopty_net::framed::FramedSend;
use slopty_net::{Connection, NetError, WorkerMsg};
use slopty_proto::codec::CodecError;
use slopty_proto::conversation::{
    Blob, Cap, Change, Clipped, ConversationEvent, EXPAND_CHARS, Meters, Part, PermissionEvent,
    PermissionPrompt, Settled, TextRef, ThreadId, Verdict,
};
use slopty_proto::ctl::Decision;
use slopty_worker::clip::Link;
use slopty_worker::conversation::{
    Board, Held, Holds, ORCHESTRATION, Read, Reader, Seen, SharedReader,
};
use slopty_worker::orchestrate::{Conversations, Sources};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::Daemon;

/// How often a followed session's transcripts are looked at between hooks: text and thinking
/// blocks arrive with no hook of their own.
const TICK: Duration = Duration::from_millis(250);

/// Changes per frame: a snapshot of a long session goes in pieces, so no frame nears the
/// codec's limit and the client applies the first before the last has come.
const FRAME_CHANGES: usize = 64;

/// How much sooner than the relay gives up the worker answers, so the answer reaches it.
const HOLD_MARGIN: Duration = Duration::from_secs(1);

/// Who follows what, the prompts held for them, and what the hooks tell the followers.
#[derive(Debug, Default)]
pub struct Follows {
    /// Followers and held prompts.
    pub holds: Holds<Pending>,
    /// Each session's latest meters and named subagent files, a wake-up per hook, and the
    /// reader its followers share.
    pub board: Board,
}

/// What the worker keeps with a held prompt.
#[derive(Debug)]
pub struct Pending {
    /// As shown to the followers.
    pub prompt: PermissionPrompt,
    /// Claude Code's `permission_suggestions`, handed back on "allow always".
    pub suggestions: Option<serde_json::Value>,
    /// Where the decision goes: the relay's connection.
    pub reply: oneshot::Sender<Decision>,
}

/// A request a follow task takes from its connection.
#[derive(Debug)]
pub enum Command {
    /// Send the whole of a clipped text.
    Expand {
        /// Its thread.
        thread: ThreadId,
        /// Where it is.
        reference: TextRef,
    },
}

/// The decision the relay waiting on `session`'s permission prompt `hook`, for at most
/// `relay_wait`, gets.
///
/// Undecided at once when nobody follows the session. Held, it is decided by a follower's
/// answer, or released undecided when the last follower leaves or the wait is up;
/// `relay_gone` finishing means Claude Code gave up on the hook, and the prompt is withdrawn.
pub async fn ask(
    daemon: &Daemon,
    session: SessionId,
    hook: &Hook,
    relay_wait: Duration,
    relay_gone: impl Future<Output = ()>,
) -> Decision {
    let wait = relay_wait.saturating_sub(HOLD_MARGIN);
    let asked_ms = WallMs::now();
    let until_ms = asked_ms.saturating_add(wait);
    let (reply, decided) = oneshot::channel();
    let Some(prompt) = hold(daemon, session, hook, reply, (asked_ms, until_ms)) else {
        return Decision::Pass;
    };
    let id = prompt.ask;
    tracing::info!(%session, ask = id, tool = %prompt.tool, "permission held for the followers");
    let _sent = daemon.events.send(WorkerMsg::Permission(PermissionEvent::Asked(Box::new(prompt))));
    tokio::pin!(decided);
    let outcome = tokio::select! {
        decision = &mut decided => return decision.unwrap_or(Decision::Pass),
        () = tokio::time::sleep(wait) => Settled::Released,
        () = relay_gone => Settled::Withdrawn,
    };
    let released = daemon.follows.lock().holds.release(id);
    match released {
        Some(held) => {
            tracing::info!(%session, ask = id, ?outcome, "permission no longer held");
            settle(daemon, id, &held, outcome);
            Decision::Pass
        }
        // An answer or a release took it first and has sent its decision.
        None => decided.try_recv().unwrap_or(Decision::Pass),
    }
}

/// Hold a prompt for `session`'s followers, and the prompt as they are to be shown it; `None`
/// when nobody follows.
fn hold(
    daemon: &Daemon,
    session: SessionId,
    hook: &Hook,
    reply: oneshot::Sender<Decision>,
    (asked_ms, until_ms): (WallMs, WallMs),
) -> Option<PermissionPrompt> {
    let mut follows = daemon.follows.lock();
    let id = follows.holds.ask(session, |id| Pending {
        prompt: permission::prompt(session, id, hook, asked_ms, until_ms),
        suggestions: hook.permission_suggestions.clone(),
        reply,
    })?;
    follows.holds.get(id).map(|held| held.reply.prompt.clone())
}

/// A follower answers: the first answer to a prompt still held goes to its relay, and the
/// followers hear it was answered. Any other answer is dropped; `false` says so.
pub fn answer(
    daemon: &Daemon,
    link: Link,
    by: ClientId,
    session: SessionId,
    ask: u64,
    verdict: Verdict,
) -> bool {
    let taken = daemon.follows.lock().holds.answer(link, session, ask);
    let Some(held) = taken else {
        tracing::debug!(%session, ask, %by, "an answer to a prompt no longer held");
        return false;
    };
    tracing::info!(%session, ask, %by, ?verdict, "permission answered");
    let decision = permission::decision(&verdict, held.reply.suggestions.as_ref());
    let _gone = held.reply.reply.send(decision);
    let outcome = Settled::Answered { verdict, by };
    let _sent = daemon.events.send(WorkerMsg::Permission(PermissionEvent::Settled {
        session,
        ask,
        outcome,
    }));
    true
}

/// The daemon's followed conversations as orchestration's verbs reach them: orchestration
/// follows, reads and answers as [`ORCHESTRATION`], by the nil client.
pub struct Orchestrated(pub Daemon);

impl std::fmt::Debug for Orchestrated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Orchestrated").field("worker", &self.0.id).finish_non_exhaustive()
    }
}

impl Conversations for Orchestrated {
    fn follow(&self, session: SessionId) -> Vec<PermissionPrompt> {
        let mut follows = self.0.follows.lock();
        let ids = follows.holds.follow(session, ORCHESTRATION);
        let held = ids
            .into_iter()
            .filter_map(|ask| follows.holds.get(ask).map(|held| held.reply.prompt.clone()))
            .collect();
        drop(follows);
        held
    }

    fn sources(&self, session: SessionId) -> Sources {
        let main = self.0.agents.lock().transcript_path(session);
        let seen = self.0.follows.lock().board.current(session);
        Sources { main, subagents: seen.subagents.into_iter().collect(), meters: seen.meters }
    }

    fn answer(&self, session: SessionId, ask: u64, verdict: Verdict) -> bool {
        answer(&self.0, ORCHESTRATION, ClientId::nil(), session, ask, verdict)
    }

    fn forget(&self, session: SessionId) {
        let released = {
            let mut follows = self.0.follows.lock();
            follows.board.forget(session);
            follows.holds.forget(session)
        };
        release(&self.0, released);
    }
}

/// Hand back prompts the last follower left undecided: each relay answers nothing, and Claude
/// Code shows its own dialog.
pub fn release(daemon: &Daemon, released: Vec<(u64, Held<Pending>)>) {
    for (id, held) in released {
        tracing::info!(session = %held.session, ask = id, "permission released to the TUI");
        settle(daemon, id, &held, Settled::Released);
        let _gone = held.reply.reply.send(Decision::Pass);
    }
}

fn settle(daemon: &Daemon, ask: u64, held: &Held<Pending>, outcome: Settled) {
    let session = held.session;
    let _sent = daemon.events.send(WorkerMsg::Permission(PermissionEvent::Settled {
        session,
        ask,
        outcome,
    }));
}

/// Follow `session` for one client until `commands` closes (it unfollowed or left) or the
/// session goes: open its conversation stream and keep it current.
pub async fn stream(
    daemon: Daemon,
    conn: Connection,
    session: SessionId,
    mut seen: watch::Receiver<Seen>,
    mut commands: mpsc::UnboundedReceiver<Command>,
) {
    let wait = slopty_net::streams::SESSION_STREAM_WAIT;
    let mut out = match slopty_net::streams::open_conversation(&conn, session, wait).await {
        Ok(out) => out,
        Err(e) => {
            tracing::debug!(%session, error = %e, "no conversation stream");
            return;
        }
    };
    let why = match follow(&daemon, session, &mut out, &mut seen, &mut commands).await {
        Ok(why) => why,
        Err(e) => {
            tracing::debug!(%session, error = %e, "conversation stream ended");
            return;
        }
    };
    tracing::debug!(%session, why, "conversation stream finished");
    let _finished = out.finish();
}

/// The follow loop; `Ok` with why it ended, `Err` when the stream failed.
async fn follow(
    daemon: &Daemon,
    session: SessionId,
    out: &mut FramedSend<ConversationEvent>,
    seen: &mut watch::Receiver<Seen>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
) -> Result<&'static str, NetError> {
    let reader = shared_reader(daemon, session, seen);
    let Some((mut reads, whole)) = join(daemon, session, seen, &reader).await else {
        return Ok("the read failed");
    };
    let mut pending = Some(whole);
    let mut current = false;
    let mut meters_sent: Option<Meters> = None;
    let mut overlay = Overlay::default();
    // Live blocks the mod stops reporting expire on this tick; the reader has its own.
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let meters = seen.borrow_and_update().meters.clone();
        let Read { changes, outputs } =
            pending.take().map(Arc::unwrap_or_clone).unwrap_or_default();
        let whole = changes.iter().any(|c| matches!(c, Change::Reset { thread: None }));
        // Live blocks go after the changes, so a block is cleared only once its entry is there.
        let mut live = Vec::new();
        if current || whole {
            let now = Instant::now();
            if whole {
                live.extend(overlay.clear_all());
            }
            live.extend(overlay.update(&seen.borrow().live, now, WallMs::now()));
            live.extend(overlay.settle(&changes));
            live.extend(overlay.expire(now));
        }
        send_changes(out, session, changes).await?;
        if whole {
            out.send(&ConversationEvent::Current).await?;
            current = true;
        }
        if !live.is_empty() {
            out.send(&ConversationEvent::Live(live)).await?;
        }
        if !outputs.is_empty() {
            out.send(&ConversationEvent::Output(outputs)).await?;
        }
        if meters.is_some() && meters != meters_sent {
            if let Some(meters) = &meters {
                out.send(&ConversationEvent::Meters(meters.clone())).await?;
            }
            meters_sent = meters;
        }
        tokio::select! {
            _ = tick.tick() => {}
            changed = seen.changed() => {
                if changed.is_err() {
                    return Ok("the session is gone");
                }
            }
            read = reads.recv() => match read {
                Ok(read) => pending = Some(read),
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::debug!(%session, missed, "a follower fell behind; sending it all again");
                    let Some((again, whole)) = join(daemon, session, seen, &reader).await else {
                        return Ok("the read failed");
                    };
                    reads = again;
                    pending = Some(whole);
                }
                Err(broadcast::error::RecvError::Closed) => return Ok("the reader is gone"),
            },
            command = commands.recv() => match command {
                None => return Ok("unfollowed"),
                Some(Command::Expand { thread, reference }) => {
                    let Some(event) = expand(&reader, thread, reference).await else {
                        return Ok("the read failed");
                    };
                    out.send(&event).await?;
                }
            },
        }
    }
}

/// The reader of `session`'s transcripts that its followers share; the first follower's call
/// starts the task that reads them on the tick.
fn shared_reader(
    daemon: &Daemon,
    session: SessionId,
    seen: &watch::Receiver<Seen>,
) -> SharedReader {
    let (reader, fresh) = daemon.follows.lock().board.reader(session);
    if fresh {
        tokio::spawn(read_for_followers(
            daemon.clone(),
            session,
            Arc::downgrade(&reader),
            seen.clone(),
        ));
    }
    reader
}

/// Join the followers of `session`: read what the files gained, which the others are sent,
/// then take the conversation as it stands and the reads to come. `None` when the read failed.
async fn join(
    daemon: &Daemon,
    session: SessionId,
    seen: &watch::Receiver<Seen>,
    reader: &SharedReader,
) -> Option<(broadcast::Receiver<Arc<Read>>, Arc<Read>)> {
    let main = daemon.agents.lock().transcript_path(session);
    let known = seen.borrow().subagents.iter().cloned().collect::<Vec<PathBuf>>();
    let mut reader = Arc::clone(reader).lock_owned().await;
    let joined = tokio::task::spawn_blocking(move || {
        if let Some(main) = main {
            reader.read(&main, &known);
        }
        let (reads, whole) = reader.join();
        (reads, Arc::new(whole))
    });
    joined.await.ok()
}

/// Read `session`'s transcripts for its followers on every tick, and at once when a hook fires
/// in the session, until the last follower lets go of the reader or the session goes. A read
/// that finds nothing sends nothing.
async fn read_for_followers(
    daemon: Daemon,
    session: SessionId,
    reader: Weak<tokio::sync::Mutex<Reader>>,
    mut seen: watch::Receiver<Seen>,
) {
    let mut hooks_read = seen.borrow_and_update().hooks;
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first follower reads as it joins.
    tick.tick().await;
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            changed = seen.changed() => {
                if changed.is_err() {
                    return;
                }
                // The mod's blocks and the meters are the followers' to send, not a reason to
                // read.
                if seen.borrow_and_update().hooks == hooks_read {
                    continue;
                }
            }
        }
        let Some(reader) = reader.upgrade() else { return };
        let Some(main) = daemon.agents.lock().transcript_path(session) else { continue };
        let known = {
            let now = seen.borrow_and_update();
            hooks_read = now.hooks;
            now.subagents.iter().cloned().collect::<Vec<PathBuf>>()
        };
        let mut reader = reader.lock_owned().await;
        if tokio::task::spawn_blocking(move || reader.read(&main, &known)).await.is_err() {
            tracing::warn!(%session, "the conversation read failed; its followers hear no more");
            return;
        }
    }
}

/// Resolve a clipped text, or an image's bytes, on the blocking pool; `None` when the read
/// failed.
async fn expand(
    reader: &SharedReader,
    thread: ThreadId,
    reference: TextRef,
) -> Option<ConversationEvent> {
    let reader = Arc::clone(reader).lock_owned().await;
    let read = tokio::task::spawn_blocking(move || {
        let transcripts = reader.transcripts();
        if matches!(reference.part, Part::Image { .. }) {
            let blob = transcripts
                .image(&thread, &reference)
                .map(|data| Blob { digest: blake3::hash(&data).to_hex().to_string(), data });
            return ConversationEvent::Image { thread, reference, blob };
        }
        let text = transcripts.full_text(&thread, &reference).map(|text| {
            let cap = Cap { lines: usize::MAX, chars: EXPAND_CHARS };
            Clipped::head(&text, cap, Some(reference.clone()))
        });
        ConversationEvent::Expanded { thread, reference, text }
    });
    read.await.ok()
}

/// Send `changes` in frames of [`FRAME_CHANGES`]. A frame past the codec's limit (a patch of
/// very long lines) goes again one change at a time, and a single change past it is left out.
async fn send_changes(
    out: &mut FramedSend<ConversationEvent>,
    session: SessionId,
    changes: Vec<Change>,
) -> Result<(), NetError> {
    let mut changes = changes.into_iter().peekable();
    while changes.peek().is_some() {
        let frame = ConversationEvent::Changes(changes.by_ref().take(FRAME_CHANGES).collect());
        match out.send(&frame).await {
            Err(NetError::Codec(CodecError::TooLarge { .. })) => {}
            sent => {
                sent?;
                continue;
            }
        }
        let ConversationEvent::Changes(batch) = frame else { continue };
        for change in batch {
            match out.send(&ConversationEvent::Changes(vec![change])).await {
                Err(NetError::Codec(CodecError::TooLarge { len, .. })) => {
                    tracing::warn!(%session, len, "a conversation change too large to send");
                }
                sent => sent?,
            }
        }
    }
    Ok(())
}
