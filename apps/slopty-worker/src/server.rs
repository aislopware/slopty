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

use std::sync::Arc;

use anyhow::{Context as _, Result};
use slopty_agent::status::SessionAgent;
use slopty_core::SessionId;
use slopty_crash::log::{Repeats, Tell};
use slopty_net::framed::FramedSend;
use slopty_net::liveness::{Beat, Pulse};
use slopty_net::redial::Redial;
use slopty_net::server::{DialError, ServerLink};
use slopty_net::{HostAddr, NetError};
use slopty_proto::WorkerMsg;
use slopty_proto::codec::CodecError;
use slopty_proto::ctl::{LinkState, ServerHealth};
use slopty_proto::orchestration::{ErrorCode, Outcome, Verb};
use slopty_proto::server::{Facts, FromServer, Refusal, Registration, Role, ToServer, WorkerCaps};
use slopty_worker::orchestrate::{Agents, Orchestrator};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinSet;

use crate::Daemon;

/// Messages queued for the server before the link applies backpressure to itself.
const OUT_DEPTH: usize = 256;

/// The server to register with: `--server` (or `SLOPTY_SERVER`, which clap folds into it),
/// else `[network] server` in `settings.toml`; `None` runs the worker on its own.
pub fn configured(
    flag: Option<&str>,
    settings: &slopty_settings::Settings,
) -> Result<Option<HostAddr>> {
    let Some(text) = flag.map(str::trim) else {
        return Ok(settings.network.server.clone());
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
    // A server gone for weeks is redialled every couple of seconds for a minute, then every half
    // minute or so: its failure is logged when it starts or changes, then once an hour, not on
    // every redial (`slopty_crash::log`). A machine that wakes from sleep dials at once.
    let mut dialing = Dialing {
        redial: Redial::default(),
        repeats: Repeats::new(slopty_crash::log::STILL_EVERY),
    };
    loop {
        let ended =
            session(&daemon, &orchestrator, &endpoint, &addr, watched.clone(), &mut dialing).await;
        let redialling = |why: String| stands(&daemon, &addr, LinkState::Redialling { why });
        match ended {
            Ok(why) => {
                tracing::info!(server = %addr, why, "server link ended");
                redialling(why.to_owned());
            }
            // This worker's own last link is replaced at once, its key proving it is the
            // same worker; another machine with this id and another key is refused. The redials
            // find out whether it went.
            Err(Ended::Duplicate) => {
                let why = "another worker holds this worker's id on the server";
                told(&mut dialing.repeats, &addr, why, tracing::Level::WARN);
                redialling(why.to_owned());
            }
            // The tailnet policy may grant it later; the redials find out.
            Err(Ended::NotGranted) => {
                let why = "the tailnet policy does not grant this machine the worker role";
                told(&mut dialing.repeats, &addr, why, tracing::Level::WARN);
                stands(&daemon, &addr, LinkState::NotGranted);
            }
            // It changes only when someone updates it or this worker: asked again after a while.
            Err(Ended::WrongBuild(wrong)) => {
                let this = slopty_proto::wire::this_build();
                let server_build = wrong.peer_build();
                let update = match wrong.newer() {
                    Some(slopty_net::Newer::Here) => "update the server",
                    Some(slopty_net::Newer::There) => "update this worker",
                    None => "update whichever is behind",
                };
                let why = format!(
                    "the server runs build {server_build} and this worker {this}; {update}"
                );
                told(&mut dialing.repeats, &addr, &why, tracing::Level::WARN);
                stands(&daemon, &addr, LinkState::Refused { why });
                tokio::time::sleep(slopty_net::redial::WRONG_BUILD).await;
                continue;
            }
            Err(Ended::Failed(e)) => {
                let why = format!("{e:#}");
                told(&mut dialing.repeats, &addr, &why, tracing::Level::INFO);
                redialling(why);
            }
        }
        if slopty_net::redial::wait(dialing.redial.next(std::time::Instant::now())).await {
            tracing::info!(server = %addr, "woke from sleep; dialling the server now");
            dialing.redial.reset();
        }
    }
}

/// How the server link's redials go: when the next is due, and what the log has told of their
/// failures.
struct Dialing {
    /// When the next redial is due.
    redial: Redial,
    /// What the log has told of the failures.
    repeats: Repeats,
}

/// A redial's failure `why`, logged at `level` when it is new and once an hour while it goes on,
/// with how many redials have failed and for how many minutes.
fn told(repeats: &mut Repeats, addr: &HostAddr, why: &str, level: tracing::Level) {
    let (times, minutes) = match repeats.failed(why, std::time::Instant::now()) {
        Tell::Quiet => return,
        Tell::New => (None, None),
        Tell::Still { times, lasted } => (Some(times), Some(minutes(lasted))),
    };
    if level == tracing::Level::WARN {
        tracing::warn!(server = %addr, why, times, minutes, "server link failing");
    } else {
        tracing::info!(server = %addr, why, times, minutes, "server link failing");
    }
}

/// `d` in whole minutes.
fn minutes(d: std::time::Duration) -> u64 {
    d.as_secs().checked_div(60).unwrap_or_default()
}

/// Why a registration did not get going.
enum Ended {
    /// The server has another worker under this id, with another key.
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
    dialing: &mut Dialing,
) -> Result<&'static str, Ended> {
    let Watched { mut caps, mut facts } = watched;
    // Subscribed before the registration is taken: whatever happens after it is sent after it.
    let mut events = daemon.events.subscribe();
    let mut cloning = orchestrator.clone_progress();
    let now = caps.borrow_and_update().clone();
    let registration = Registration {
        worker: daemon.id,
        name: daemon.name.clone(),
        listen: daemon.listen,
        caps: now,
        sessions: daemon.worker.summaries().await,
        session_key: daemon.session_key.bytes(),
    };
    // The self-test's stalled control streams hold a new link's too (`stall`).
    crate::stall::hold().await;
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
    let now = std::time::Instant::now();
    dialing.redial.linked(now);
    stands(daemon, addr, LinkState::Linked);
    if let Some((failures, lasted)) = dialing.repeats.ended(now) {
        let minutes = minutes(lasted);
        tracing::info!(server = %name, %remote, failures, minutes, "registered with the server");
    } else {
        tracing::info!(server = %name, %remote, "registered with the server");
    }
    let (out, out_rx) = mpsc::channel::<ToServer>(OUT_DEPTH);
    let mut writer = tokio::spawn(write(tx, out_rx));
    // A registration carries no load: the server hears it first here, then each move of it.
    let load = *daemon.load.borrow();
    if out.send(ToServer::Load(load)).await.is_err() {
        return Ok("the writer stopped");
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
    // The control stream is the lease (`slopty_net::liveness`): pinged every 2 s, and given up
    // 6 s after its last answer however alive the connection under it looks, so the server
    // and this worker agree the lease ended and the redial finds a stream that carries.
    let mut beat = Beat::new(std::time::Instant::now());
    let mut look = tokio::time::interval(slopty_net::liveness::LOOK_EVERY);
    look.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let why = loop {
        tokio::select! {
            _ = look.tick() => {
                let now = std::time::Instant::now();
                if let Pulse::Dead(gap) = beat.pulse(now) {
                    tracing::warn!(server = %name, ?gap, "the server stopped answering on the control stream");
                    break "the server stopped answering";
                }
                if beat.ping_due(now) {
                    let ping = ToServer::Ping { sent_at: slopty_core::MonoTime::now() };
                    // A full queue already waits on the server; its answer to the last ping
                    // still counts.
                    let _full = out.try_send(ping);
                }
            }
            msg = async { crate::stall::hold().await; rx.recv().await } => match msg {
                Ok(FromServer::Pong { .. }) => beat.heard(std::time::Instant::now()),
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
                Ok(other) => tracing::debug!(?other, "server message a worker does not take"),
                Err(NetError::Closed) => break "the server closed the link",
                Err(e) => return Err(e.into()),
            },
            ev = events.recv() => {
                let msg = match ev {
                    Ok(WorkerMsg::SessionChanged(summary)) => ToServer::SessionChanged(summary),
                    Ok(WorkerMsg::SessionClosed { session, reason }) => {
                        ToServer::SessionClosed { session, reason }
                    }
                    Ok(WorkerMsg::Load(load)) => ToServer::Load(load),
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
        crate::stall::hold().await;
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

#[cfg(test)]
mod tests {
    use slopty_net::HostAddr;

    use super::configured;

    #[test]
    fn the_flag_wins_over_the_settings_and_the_port_defaults_to_the_servers() {
        let mut settings = slopty_settings::Settings::default();
        assert_eq!(configured(None, &settings).unwrap(), None, "on its own");
        settings.network.server = Some(HostAddr::parse_with_port("studio", 45560).unwrap());
        let from_file = configured(None, &settings).unwrap().unwrap();
        assert_eq!((from_file.host(), from_file.port()), ("studio", 45560));
        let flag = configured(Some("100.64.0.9:7000"), &settings).unwrap().unwrap();
        assert_eq!((flag.host(), flag.port()), ("100.64.0.9", 7000));
        assert_eq!(configured(Some(" "), &settings).unwrap(), None, "an empty flag opts out");
        configured(Some("a b"), &settings).unwrap_err();
    }
}
