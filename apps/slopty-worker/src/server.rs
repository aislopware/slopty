//! The link to the server (`docs/decisions/topology.md`): register as a worker, hold the
//! lease, pass on what happens here, and answer the verbs the server forwards.
//!
//! The link is one task beside everything else the daemon does. Terminals and clients never
//! wait on it: it hears the daemon's events on its own broadcast subscription (falling behind
//! costs only this link a re-registration), answers every request in a task of its own (a
//! long `WaitFor` holds up nothing), and when the server is down it dials again by the one
//! redial rule every link follows ([`slopty_net::redial`]), forever. A server on a different
//! build is asked again only after [`slopty_net::redial::WRONG_BUILD`].
//!
//! It also tells the server what this worker is and has ([`slopty_worker::facts`]), which the
//! link only carries: gathered on a task of their own, they go out after each registration and
//! whenever they change.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use slopty_agent::status::SessionAgent;
use slopty_core::SessionId;
use slopty_net::framed::FramedSend;
use slopty_net::redial::Redial;
use slopty_net::server::{DialError, ServerLink};
use slopty_net::{HostAddr, NetError};
use slopty_proto::WorkerMsg;
use slopty_proto::codec::CodecError;
use slopty_proto::ctl::{LinkState, ServerHealth};
use slopty_proto::orchestration::{ErrorCode, Outcome, Verb};
use slopty_proto::project::{AgentReport, Facts};
use slopty_proto::server::{FromServer, Refusal, Registration, Role, ToServer, WorkerCaps};
use slopty_worker::orchestrate::{Agents, Orchestrator};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinSet;

use crate::Daemon;

/// Messages queued for the server before the link applies backpressure to itself.
const OUT_DEPTH: usize = 256;

/// The server to register with: `--server` (or `SLOPTY_SERVER`, which clap folds into it),
/// else `[worker] server` in `settings.toml`; `None` runs the worker on its own.
pub fn configured(
    flag: Option<&str>,
    settings: &slopty_settings::Settings,
) -> Result<Option<HostAddr>> {
    let Some(text) = flag.map(str::trim) else {
        return Ok(settings.worker.server.clone());
    };
    if text.is_empty() {
        return Ok(None);
    }
    let addr = HostAddr::parse_with_port(text, slopty_net::endpoint::SERVER_PORT)
        .with_context(|| format!("server address {text:?}"))?;
    Ok(Some(addr))
}

/// The daemon's agent table, as orchestration reads it.
pub struct DaemonAgents(pub Arc<parking_lot::Mutex<slopty_agent::AgentTable>>);

impl Agents for DaemonAgents {
    fn status(&self, session: SessionId) -> Option<SessionAgent> {
        let event = self.0.lock().snapshot().into_iter().find(|e| e.session == session)?;
        Some(SessionAgent::from(&event))
    }

    fn forget(&self, session: SessionId) {
        self.0.lock().forget(session);
    }

    fn ended(&self, session: SessionId) -> bool {
        self.0.lock().ended(session)
    }

    fn interrupted(&self, session: SessionId) -> bool {
        self.0.lock().interrupted(session)
    }
}

/// What a registration keeps sending as it changes.
#[derive(Clone)]
pub struct Watched {
    /// What this worker can do.
    pub caps: watch::Receiver<WorkerCaps>,
    /// What it is and has; empty until first gathered.
    pub facts: watch::Receiver<Facts>,
}

/// Say how the link to the server at `addr` stands, for the doctor.
pub fn stands(daemon: &Daemon, addr: &HostAddr, link: LinkState) {
    daemon.server_link.send_replace(Some(ServerHealth { address: addr.to_string(), link }));
}

/// Stay registered with the server at `addr`, dialing from `endpoint`, until the daemon stops.
/// Each try's end is said for the doctor ([`stands`]) before the next.
pub async fn run(
    daemon: Daemon,
    orchestrator: Orchestrator,
    endpoint: slopty_net::Endpoint,
    addr: HostAddr,
    watched: Watched,
) -> ! {
    let mut redial = Redial::default();
    loop {
        let ended =
            session(&daemon, &orchestrator, &endpoint, &addr, watched.clone(), &mut redial).await;
        // No phone's answer comes through a server this worker is not linked to.
        crate::threads::hold::pushed(&daemon, false);
        let redialling = |why: String| stands(&daemon, &addr, LinkState::Redialling { why });
        match ended {
            Ok(why) => {
                tracing::info!(server = %addr, why, "server link ended");
                redialling(why.to_owned());
            }
            // It lets go of that link within the lease's idle timeout; the redials, two
            // seconds apart at most, find it gone.
            Err(Ended::Duplicate) => {
                tracing::info!(server = %addr, "the server still holds our last link; retrying");
                redialling("the server still holds this worker's last link".to_owned());
            }
            // The tailnet policy may grant it later; the redials find out.
            Err(Ended::NotGranted) => {
                let why = "the tailnet policy does not grant this machine the worker role";
                tracing::warn!(server = %addr, "{why}");
                stands(&daemon, &addr, LinkState::Refused { why: why.to_owned() });
            }
            // It changes only when someone updates it or this worker: asked again after a while.
            Err(Ended::WrongBuild(wrong)) => {
                let (server_build, this) = (wrong.peer_build(), slopty_proto::wire::BUILD);
                tracing::warn!(
                    server = %addr, server_build, this,
                    "the server runs a different build; update whichever is behind"
                );
                let why = format!(
                    "the server runs build {server_build} and this worker {this}; update \
                     whichever is behind"
                );
                stands(&daemon, &addr, LinkState::Refused { why });
                tokio::time::sleep(slopty_net::redial::WRONG_BUILD).await;
                continue;
            }
            Err(Ended::Failed(e)) => {
                tracing::info!(server = %addr, error = %e, "server link failed");
                redialling(format!("{e:#}"));
            }
        }
        tokio::time::sleep(redial.next(std::time::Instant::now())).await;
    }
}

/// Why a registration did not get going.
enum Ended {
    /// The server has this worker on another connection still.
    Duplicate,
    /// The tailnet policy does not grant this machine the worker role.
    NotGranted,
    /// The server runs a different build, whose messages this one cannot read.
    WrongBuild(slopty_net::WrongBuild),
    /// Anything else.
    Failed(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for Ended {
    fn from(e: E) -> Self {
        Self::Failed(e.into())
    }
}

/// One registration: dial, register, then serve until the link or the daemon ends. `Ok`
/// carries why it ended.
async fn session(
    daemon: &Daemon,
    orchestrator: &Orchestrator,
    endpoint: &slopty_net::Endpoint,
    addr: &HostAddr,
    watched: Watched,
    redial: &mut Redial,
) -> Result<&'static str, Ended> {
    let Watched { mut caps, mut facts } = watched;
    // Subscribed before the registration is taken: whatever happens after it is sent after it.
    let mut events = daemon.events.subscribe();
    let mut cloning = orchestrator.clone_progress();
    let mut reports = daemon.reports.subscribe();
    let now = caps.borrow_and_update().clone();
    let registration = Registration {
        worker: daemon.id,
        name: daemon.name.clone(),
        listen: daemon.listen,
        caps: now,
        sessions: daemon.worker.summaries().await,
        session_key: daemon.session_key.bytes(),
    };
    let link =
        match slopty_net::server::connect(endpoint, addr, Role::Worker(Box::new(registration)))
            .await
        {
            Ok(link) => link,
            Err(DialError::Refused(Refusal::DuplicateWorker)) => return Err(Ended::Duplicate),
            Err(DialError::Refused(Refusal::NotGranted)) => return Err(Ended::NotGranted),
            Err(DialError::Net(NetError::WrongBuild(wrong))) => {
                return Err(Ended::WrongBuild(wrong));
            }
            Err(DialError::Net(e)) => return Err(e.into()),
        };
    let ServerLink { conn, remote, name, tx, mut rx, .. } = link;
    redial.linked(std::time::Instant::now());
    stands(daemon, addr, LinkState::Linked);
    tracing::info!(server = %name, %remote, "registered with the server");
    let (out, out_rx) = mpsc::channel::<ToServer>(OUT_DEPTH);
    let mut writer = tokio::spawn(write(tx, out_rx));
    // A registration carries no load: the server hears it first here, then each move of it.
    let load = *daemon.load.borrow();
    if out.send(ToServer::Load(load)).await.is_err() {
        return Ok("the writer stopped");
    }
    // Nor where each agent's work lands, which the status lines said before this link.
    let branches = daemon.agents.lock().branches();
    for branch in branches {
        if out.send(ToServer::Report(AgentReport::Branch(branch))).await.is_err() {
            return Ok("the writer stopped");
        }
    }
    // Nor what it is and has, once that is known.
    let known = facts.borrow_and_update().clone();
    if !known.is_empty() && out.send(ToServer::Facts(known)).await.is_err() {
        return Ok("the writer stopped");
    }
    // A registration outlives a facts task that ended: the facts it sent stand.
    let mut facts_watched = true;
    // Nor its threads: the whole table goes first on every registration.
    let mut threads = daemon.threads.as_ref().map(crate::threads::Publish::of);
    let mut requests = JoinSet::new();
    let why = loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(FromServer::Request { id, key, verb }) => {
                    let (orchestrator, out) = (orchestrator.clone(), out.clone());
                    requests.spawn(async move {
                        let resized = match &verb {
                            Verb::ResizeTerminal { term, .. } => Some(term.session),
                            _ => None,
                        };
                        let outcome = orchestrator.serve(key, verb).await;
                        // The server knows each terminal from its summaries: a terminal opened
                        // and a new size go ahead of the answer, so whatever its caller does
                        // next with the answer (name it a project's orchestrator, list it) finds
                        // it as it is.
                        let ahead = match (&outcome, resized) {
                            (Outcome::Opened(term) | Outcome::OpenedIn { term, .. }, _) => {
                                Some(term.session)
                            }
                            (Outcome::Done, resized) => resized,
                            _ => None,
                        };
                        if let Some(session) = ahead
                            && let Some(summary) = orchestrator.summary(session).await
                        {
                            let _sent = out.send(ToServer::SessionChanged(summary)).await;
                        }
                        let _sent = out.send(ToServer::Reply { id, outcome }).await;
                    });
                }
                // A task's thread that runs in no terminal is sent the reports as a message.
                Ok(FromServer::Deliver { session, batch, context })
                    if daemon
                        .threads
                        .as_ref()
                        .is_some_and(|t| t.seated_without_terminal(session).is_some()) =>
                {
                    let sent = daemon.threads.as_ref().map(|t| t.deliver(session, batch, &context));
                    match sent {
                        Some(
                            slopty_proto::thread::wire::Outcome::Done
                            | slopty_proto::thread::wire::Outcome::Accepted,
                        ) => {
                            tracing::debug!(%session, batch, "reports sent to the seat's thread");
                            let report = AgentReport::Delivered { session, batch };
                            if out.send(ToServer::Report(report)).await.is_err() {
                                break "the writer stopped";
                            }
                        }
                        other => tracing::warn!(%session, batch, ?other, "reports not sent"),
                    }
                }
                Ok(FromServer::Deliver { session, batch, context }) => {
                    let (dir, turn) =
                        (daemon.deliveries.clone(), Arc::clone(&daemon.reports_turn));
                    let kept = tokio::task::spawn_blocking(move || {
                        let batch = slopty_agent::reports::Batch { batch, context };
                        let _turn = turn.lock();
                        slopty_agent::reports::put(&dir, session, &batch)
                    });
                    match kept.await {
                        Ok(Ok(())) => match may_post(daemon, session) {
                            Ok(()) => {
                                tracing::debug!(%session, batch, "reports kept, and posted");
                                let (deliveries, inboxes) =
                                    (daemon.deliveries.clone(), daemon.inboxes.clone());
                                tokio::spawn(hand_over(deliveries, inboxes, session));
                            }
                            Err(why) => {
                                tracing::debug!(%session, batch, why, "reports kept for the hooks");
                            }
                        },
                        Ok(Err(e)) => tracing::warn!(%session, error = %e, "reports not kept"),
                        Err(e) => tracing::warn!(%session, error = %e, "reports not kept"),
                    }
                }
                // A prompt nobody here can answer waits for a pocketed phone while one can.
                Ok(FromServer::Pushes(pushed)) => crate::threads::hold::pushed(daemon, pushed),
                Ok(other) => tracing::debug!(?other, "server message a worker does not take"),
                Err(NetError::Closed) => break "the server closed the link",
                Err(e) => return Err(e.into()),
            },
            ev = events.recv() => {
                let msg = match ev {
                    Ok(WorkerMsg::SessionChanged(summary)) => ToServer::SessionChanged(summary),
                    Ok(WorkerMsg::SessionClosed { session, reason }) => {
                        let inboxes = daemon.inboxes.clone();
                        drop(tokio::task::spawn_blocking(move || {
                            slopty_agent::reports::forget_inbox(&inboxes, session)
                        }));
                        ToServer::SessionClosed { session, reason }
                    }
                    Ok(WorkerMsg::Load(load)) => ToServer::Load(load),
                    Ok(WorkerMsg::AgentBranch(branch)) => {
                        ToServer::Report(AgentReport::Branch(branch))
                    }
                    Ok(_) => continue,
                    // What was missed is in a fresh registration.
                    Err(broadcast::error::RecvError::Lagged(_)) => break "fell behind the daemon's events",
                    Err(broadcast::error::RecvError::Closed) => break "the daemon is stopping",
                };
                if out.send(msg).await.is_err() {
                    break "the writer stopped";
                }
            }
            step = cloning.recv() => {
                let msg = match step {
                    Ok((clone, p)) => ToServer::Cloning { clone, phase: p.phase, percent: p.percent },
                    // A step of progress missed is overtaken by the next.
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break "the daemon is stopping",
                };
                if out.send(msg).await.is_err() {
                    break "the writer stopped";
                }
            }
            report = reports.recv() => {
                let msg = match report {
                    Ok(report) => ToServer::Report(report),
                    // A subagent's start or stop the tree missed is only a leaf short.
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::warn!(missed, "agent reports dropped");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break "the daemon is stopping",
                };
                if out.send(msg).await.is_err() {
                    break "the writer stopped";
                }
            }
            changed = caps.changed() => {
                if changed.is_err() {
                    break "capabilities are no longer watched";
                }
                let now = caps.borrow_and_update().clone();
                if out.send(ToServer::Caps(now)).await.is_err() {
                    break "the writer stopped";
                }
            }
            changed = facts.changed(), if facts_watched => {
                if changed.is_err() {
                    tracing::warn!("this worker's facts are no longer gathered");
                    facts_watched = false;
                    continue;
                }
                let now = facts.borrow_and_update().clone();
                if out.send(ToServer::Facts(now)).await.is_err() {
                    break "the writer stopped";
                }
            }
            frame = next_frame(&mut threads) => match frame {
                Some(frame) => {
                    if out.send(ToServer::Threads(frame)).await.is_err() {
                        break "the writer stopped";
                    }
                }
                None => threads = None,
            },
            Some(done) = requests.join_next() => {
                if let Err(e) = done {
                    tracing::warn!(error = %e, "a forwarded verb's task failed");
                }
            }
            // A link that can no longer write is no lease: the server must see it end.
            _ended = &mut writer => break "the writer stopped",
            reason = conn.closed() => {
                tracing::info!(%reason, "server connection closed");
                break "the connection closed";
            }
        }
    };
    requests.abort_all();
    writer.abort();
    conn.close(slopty_net::worker::close_code::NORMAL.into(), b"worker link ended");
    Ok(why)
}

/// The next frame of the thread table for the server; never, with no threads.
async fn next_frame(
    threads: &mut Option<crate::threads::Publish>,
) -> Option<slopty_proto::thread::wire::TableFrame> {
    match threads {
        Some(threads) => threads.next().await,
        None => std::future::pending().await,
    }
}

/// Send what is queued until the link fails. A reply too large for one message goes as an
/// error in its place: nothing of it was written, and the caller hears why.
async fn write(mut tx: FramedSend<ToServer>, mut rx: mpsc::Receiver<ToServer>) {
    while let Some(msg) = rx.recv().await {
        let sent = match tx.send(&msg).await {
            Err(NetError::Codec(CodecError::TooLarge { len, max })) => match msg {
                ToServer::Reply { id, .. } => {
                    tracing::warn!(id, len, max, "a reply too large for the link");
                    tx.send(&ToServer::Reply { id, outcome: too_large(len, max) }).await
                }
                _ => Err(NetError::Codec(CodecError::TooLarge { len, max })),
            },
            other => other,
        };
        if let Err(e) = sent {
            tracing::warn!(error = %e, "server link write failed");
            return;
        }
    }
}

/// The error sent instead of an answer of `len` bytes, over the `max` one message carries.
fn too_large(len: usize, max: usize) -> Outcome {
    Outcome::Error {
        code: ErrorCode::Failed,
        message: format!("the answer is {len} bytes, more than the {max} one message carries"),
    }
}

/// How long posting to an agent's inbox may take before it is given up; the batch waits for the
/// hooks either way.
const INBOX_PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);

/// Wake the agent in `session` with the batch kept for it, through its inbox when it noted one.
/// The batch stays for its hooks, which acknowledge it: Claude Code may hold or drop what
/// arrives on the inbox and says nothing back, so the post is noted with the mark its message
/// carries, and the next hook hands the batch over itself unless the mark shows the agent read
/// it ([`slopty_agent::reports::hand_over`]). An inbox whose socket is gone is forgotten.
async fn hand_over(deliveries: PathBuf, inboxes: PathBuf, session: SessionId) {
    use slopty_agent::reports;
    let found = tokio::task::spawn_blocking({
        let (deliveries, inboxes) = (deliveries.clone(), inboxes.clone());
        move || -> std::io::Result<Option<(reports::Inbox, reports::Batch)>> {
            let Some(inbox) = reports::inbox(&inboxes, session)? else { return Ok(None) };
            Ok(reports::peek(&deliveries, session)?.map(|batch| (inbox, batch)))
        }
    })
    .await;
    let (inbox, batch) = match found {
        Ok(Ok(Some(found))) => found,
        Ok(Ok(None)) => return,
        Ok(Err(e)) => return tracing::warn!(%session, error = %e, "reports not read"),
        Err(e) => return tracing::warn!(%session, error = %e, "reports not read"),
    };
    let posted = reports::Posted::new(batch.batch);
    // Noted before the post: a hook the message sets off at once finds the note.
    let noted = tokio::task::spawn_blocking({
        let (deliveries, posted) = (deliveries.clone(), posted.clone());
        move || reports::note_posted(&deliveries, session, &posted)
    })
    .await;
    if let Ok(Err(e)) | Err(e) = noted.map_err(std::io::Error::other) {
        return tracing::warn!(%session, error = %e, "post not noted, so not made");
    }
    let text = reports::message(&inbox, &batch.context, &posted.mark);
    match tokio::time::timeout(INBOX_PATIENCE, post(&inbox.socket, text.as_bytes())).await {
        Ok(Ok(())) => {
            tracing::debug!(%session, batch = batch.batch, "reports posted to the agent's inbox");
        }
        failed => {
            let gone = matches!(&failed, Ok(Err(e)) if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ));
            tracing::debug!(%session, ?failed, gone, "reports wait for the agent's hooks");
            if gone {
                let forgot =
                    tokio::task::spawn_blocking(move || reports::forget_inbox(&inboxes, session));
                if let Ok(Err(e)) | Err(e) = forgot.await.map_err(std::io::Error::other) {
                    tracing::warn!(%session, error = %e, "a gone inbox not forgotten");
                }
            }
        }
    }
}

/// Whether reports kept for `session` may be posted to its agent's inbox now
/// ([`slopty_worker::orchestrate::may_deliver`]): only when the agent could take typed input,
/// and not while the person's stop of its last turn stands. Not while a prompt is the person's
/// to answer or a draft of theirs is in its prompt, nor before its hooks say it is at its
/// prompt or after it is gone. The batch then waits for the hook that follows once that
/// clears: the person's prompt, the turn's end, the agent's start.
fn may_post(daemon: &Daemon, session: SessionId) -> Result<(), String> {
    let handle = daemon.worker.get(session).map_err(|e| e.to_string())?;
    let agents = DaemonAgents(Arc::clone(&daemon.agents));
    slopty_worker::orchestrate::may_deliver(&handle, &agents).map_err(|f| f.message)
}

/// Write `text` to the Unix socket at `socket` and close it.
async fn post(socket: &Path, text: &[u8]) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt as _;
    let mut stream = tokio::net::UnixStream::connect(socket).await?;
    stream.write_all(text).await?;
    stream.shutdown().await
}

#[cfg(test)]
mod tests {
    use slopty_net::HostAddr;

    use super::configured;

    #[test]
    fn the_flag_wins_over_the_settings_and_the_port_defaults_to_the_servers() {
        let mut settings = slopty_settings::Settings::default();
        assert_eq!(configured(None, &settings).unwrap(), None, "on its own");
        settings.worker.server = Some(HostAddr::parse_with_port("studio", 45560).unwrap());
        let from_file = configured(None, &settings).unwrap().unwrap();
        assert_eq!((from_file.host(), from_file.port()), ("studio", 45560));
        let flag = configured(Some("100.64.0.9:7000"), &settings).unwrap().unwrap();
        assert_eq!((flag.host(), flag.port()), ("100.64.0.9", 7000));
        assert_eq!(configured(Some(" "), &settings).unwrap(), None, "an empty flag opts out");
        configured(Some("a b"), &settings).unwrap_err();
    }
}
