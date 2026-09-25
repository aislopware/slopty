//! The link to the server (`docs/decisions/topology.md`): register as a worker, hold the
//! lease, pass on what happens here, and answer the verbs the server forwards.
//!
//! The link is one task beside everything else the daemon does. Terminals and clients never
//! wait on it: it hears the daemon's events on its own broadcast subscription (falling behind
//! costs only this link a re-registration), answers every request in a task of its own (a
//! long `WaitFor` holds up nothing), and when the server is down it dials again with capped
//! backoff, forever.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use slopty_core::SessionId;
use slopty_net::framed::FramedSend;
use slopty_net::server::{DialError, ServerLink};
use slopty_net::{HostAddr, NetError};
use slopty_proto::WorkerMsg;
use slopty_proto::agent::SessionAgent;
use slopty_proto::codec::CodecError;
use slopty_proto::orchestration::{ErrorCode, Outcome, Verb};
use slopty_proto::server::{FromServer, Refusal, Registration, Role, ToServer, WorkerCaps};
use slopty_worker::orchestrate::{Agents, Orchestrator};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinSet;

use crate::Daemon;

/// First wait before dialing again, doubled after each failure up to [`MAX_BACKOFF`].
const MIN_BACKOFF: Duration = Duration::from_millis(250);
/// Longest wait between dials: a server that comes back is found within this.
const MAX_BACKOFF: Duration = Duration::from_secs(5);
/// A link that lived this long was healthy: the next dial starts from [`MIN_BACKOFF`] again.
const HEALTHY: Duration = Duration::from_secs(10);
/// Between dials while the server still holds this worker's previous connection (a restart
/// inside the lease's idle timeout): it lets go within the timeout, so the retry is steady.
const DUPLICATE_RETRY: Duration = Duration::from_secs(1);
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
}

/// Stay registered with the server at `addr`, dialing from `endpoint`, until the daemon stops.
pub async fn run(
    daemon: Daemon,
    orchestrator: Orchestrator,
    endpoint: slopty_net::Endpoint,
    addr: HostAddr,
    caps: watch::Receiver<WorkerCaps>,
) -> ! {
    let mut backoff = MIN_BACKOFF;
    loop {
        let started = tokio::time::Instant::now();
        let ended = session(&daemon, &orchestrator, &endpoint, &addr, caps.clone()).await;
        if started.elapsed() >= HEALTHY {
            backoff = MIN_BACKOFF;
        }
        let wait = match ended {
            Ok(why) => {
                tracing::info!(server = %addr, why, "server link ended");
                backoff
            }
            Err(Ended::Duplicate) => {
                tracing::info!(server = %addr, "the server still holds our last link; retrying");
                DUPLICATE_RETRY
            }
            Err(Ended::Failed(e)) => {
                tracing::info!(server = %addr, error = %e, "server link failed");
                backoff
            }
        };
        tokio::time::sleep(wait).await;
        if wait == backoff {
            backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
        }
    }
}

/// Why a registration did not get going.
enum Ended {
    /// The server has this worker on another connection still.
    Duplicate,
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
    mut caps: watch::Receiver<WorkerCaps>,
) -> Result<&'static str, Ended> {
    // Subscribed before the registration is taken: whatever happens after it is sent after it.
    let mut events = daemon.events.subscribe();
    let now = caps.borrow_and_update().clone();
    let registration = Registration {
        worker: daemon.id,
        name: daemon.name.clone(),
        port: daemon.listen.port(),
        caps: now,
        sessions: daemon.worker.summaries().await,
    };
    let link = match slopty_net::server::connect(endpoint, addr, Role::Worker(registration)).await {
        Ok(link) => link,
        Err(DialError::Refused(Refusal::DuplicateWorker)) => return Err(Ended::Duplicate),
        Err(DialError::Net(e)) => return Err(e.into()),
    };
    let ServerLink { conn, remote, name, tx, mut rx } = link;
    tracing::info!(server = %name, %remote, "registered with the server");
    let (out, out_rx) = mpsc::channel::<ToServer>(OUT_DEPTH);
    let mut writer = tokio::spawn(write(tx, out_rx));
    let mut requests = JoinSet::new();
    let why = loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(FromServer::Request { id, verb }) => {
                    let (orchestrator, out) = (orchestrator.clone(), out.clone());
                    requests.spawn(async move {
                        let resized = match &verb {
                            Verb::ResizeTerminal { term, .. } => Some(term.session),
                            _ => None,
                        };
                        let outcome = orchestrator.serve(verb).await;
                        // The server lists each terminal's size from its summaries: the new one
                        // goes ahead of the answer, so a listing after it shows the size.
                        if let (Some(session), Outcome::Done) = (resized, &outcome)
                            && let Some(summary) = orchestrator.summary(session).await
                        {
                            let _sent = out.send(ToServer::SessionOpened(summary)).await;
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
                    Ok(WorkerMsg::SessionOpened(summary)) => ToServer::SessionOpened(summary),
                    Ok(WorkerMsg::SessionClosed { session, reason }) => {
                        ToServer::SessionClosed { session, reason }
                    }
                    Ok(WorkerMsg::Agent(event)) => ToServer::Agent(event),
                    Ok(_) => continue,
                    // What was missed is in a fresh registration.
                    Err(broadcast::error::RecvError::Lagged(_)) => break "fell behind the daemon's events",
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
