//! The conversation face: a client following an agent's session, and the permission prompts
//! held for it.
//!
//! **The stream.** Each followed session gets a task of its own on the connection
//! ([`stream`]). It opens a conversation stream below the terminals and video
//! (`slopty_net::streams::CONVERSATION_PRIORITY`), then reads the session's transcripts
//! (`slopty_agent::conversation::Transcripts`, off the runtime on the blocking pool) and sends
//! what changed: first everything there is, behind a reset and closed by
//! `ConversationEvent::Current`, then each change. It reads on a tick and at once when a hook
//! fires in the session, and sends the status line's meters when they move. Where the agent
//! runs Slopty's mod, it also sends the blocks the model is writing
//! (`ConversationEvent::Live`) as the mod reports them (`slopty_agent::live::Overlay`), each
//! cleared after the transcript change that settles it. Nothing here is on a terminal's path:
//! no lock is held across a read or a send, and the stream's writes wait on nobody but this
//! client.
//!
//! **Held prompts.** The relay's `CtlRequest::Permission` comes to [`ask`]. The daemon's
//! [`Follows::holds`] (`slopty_worker::conversation::Holds`) decides: undecided at once when
//! nobody follows, else held and shown to the followers (`WorkerMsg::Permission`, on the control
//! stream: small and urgent), until a follower answers ([`answer`]), the last one leaves, the
//! wait runs out or the relay goes away ([`release`]).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use slopty_agent::Hook;
use slopty_agent::conversation::Transcripts;
use slopty_agent::live::Overlay;
use slopty_agent::permission::{self, Decision, PermissionAsk};
use slopty_core::{ClientId, SessionId};
use slopty_net::framed::FramedSend;
use slopty_net::{Connection, NetError, WorkerMsg};
use slopty_proto::codec::CodecError;
use slopty_proto::conversation::{
    Cap, Change, Clipped, ConversationEvent, EXPAND_CHARS, Meters, PermissionEvent,
    PermissionPrompt, Settled, TextRef, ThreadId, Verdict,
};
use slopty_worker::clip::Link;
use slopty_worker::conversation::{Board, Held, Holds, Seen};
use tokio::sync::{mpsc, oneshot, watch};

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
    /// Each session's latest meters and named subagent files, and a wake-up per hook.
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

/// Now, in ms since the Unix epoch.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The decision the relay waiting on a permission prompt gets.
///
/// Undecided at once when nobody follows the session. Held, it is decided by a follower's
/// answer, or released undecided when the last follower leaves or the wait is up;
/// `relay_gone` finishing means Claude Code gave up on the hook, and the prompt is withdrawn.
pub async fn ask(
    daemon: &Daemon,
    ask: PermissionAsk,
    relay_gone: impl Future<Output = ()>,
) -> Decision {
    let session = ask.session;
    let Ok(hook) = Hook::parse(&ask.payload) else { return Decision::Pass };
    if daemon.worker.get(session).is_err() {
        return Decision::Pass;
    }
    let wait = Duration::from_millis(ask.wait_ms).saturating_sub(HOLD_MARGIN);
    let asked_ms = now_ms();
    let until_ms = asked_ms.saturating_add(u64::try_from(wait.as_millis()).unwrap_or(u64::MAX));
    let (reply, decided) = oneshot::channel();
    let Some(prompt) = hold(daemon, session, &hook, reply, (asked_ms, until_ms)) else {
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
    (asked_ms, until_ms): (u64, u64),
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
/// followers hear it was answered. Any other answer is dropped.
pub fn answer(
    daemon: &Daemon,
    link: Link,
    by: ClientId,
    session: SessionId,
    ask: u64,
    verdict: Verdict,
) {
    let taken = daemon.follows.lock().holds.answer(link, session, ask);
    let Some(held) = taken else {
        tracing::debug!(%session, ask, %by, "an answer to a prompt no longer held");
        return;
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
    let mut transcripts = Transcripts::default();
    let mut first = true;
    let mut current = false;
    let mut meters_sent: Option<Meters> = None;
    let mut overlay = Overlay::default();
    // The transcript is read on the tick and when a hook fired, not for every piece the mod
    // reports.
    let mut read_due = true;
    let mut hooks_read = None;
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let (known, meters, hooks) = {
            let now = seen.borrow_and_update();
            let known = now.subagents.iter().cloned().collect::<Vec<PathBuf>>();
            (known, now.meters.clone(), now.hooks)
        };
        read_due |= hooks_read != Some(hooks);
        hooks_read = Some(hooks);
        let main = if read_due { daemon.agents.lock().transcript_path(session) } else { None };
        let changes = match main {
            Some(main) => {
                let read = tokio::task::spawn_blocking(move || {
                    let changes = transcripts.read(&main, &known);
                    (transcripts, changes)
                })
                .await;
                let Ok((back, changes)) = read else { return Ok("the read failed") };
                transcripts = back;
                changes
            }
            // No transcript yet: the conversation is empty until the agent writes one.
            None if first => vec![Change::Reset { thread: None }],
            None => Vec::new(),
        };
        first = false;
        read_due = false;
        let whole = changes.iter().any(|c| matches!(c, Change::Reset { thread: None }));
        // Live blocks go after the changes, so a block is cleared only once its entry is there.
        let mut live = Vec::new();
        if current || whole {
            let now = Instant::now();
            if whole {
                live.extend(overlay.clear_all());
            }
            live.extend(overlay.update(&seen.borrow().live, now, now_ms()));
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
        if meters.is_some() && meters != meters_sent {
            if let Some(meters) = &meters {
                out.send(&ConversationEvent::Meters(meters.clone())).await?;
            }
            meters_sent = meters;
        }
        tokio::select! {
            _ = tick.tick() => read_due = true,
            changed = seen.changed() => {
                if changed.is_err() {
                    return Ok("the session is gone");
                }
            }
            command = commands.recv() => match command {
                None => return Ok("unfollowed"),
                Some(Command::Expand { thread, reference }) => {
                    let (back, event) = expand(transcripts, thread, reference).await;
                    let Some(back) = back else { return Ok("the read failed") };
                    transcripts = back;
                    out.send(&event).await?;
                }
            },
        }
    }
}

/// Resolve a clipped text on the blocking pool; the transcripts come back with the answer.
async fn expand(
    transcripts: Transcripts,
    thread: ThreadId,
    reference: TextRef,
) -> (Option<Transcripts>, ConversationEvent) {
    let read = tokio::task::spawn_blocking(move || {
        let text = transcripts.full_text(&thread, &reference).map(|text| {
            let cap = Cap { lines: usize::MAX, chars: EXPAND_CHARS };
            Clipped::head(&text, cap, Some(reference.clone()))
        });
        (transcripts, ConversationEvent::Expanded { thread, reference, text })
    })
    .await;
    match read {
        Ok((transcripts, event)) => (Some(transcripts), event),
        Err(_panicked) => (None, ConversationEvent::Current),
    }
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
