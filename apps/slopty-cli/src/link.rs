//! The link to the server: where it is, and one QUIC connection that carries verbs.
//!
//! Requests are pipelined: each gets an id, the server answers in any order, and the one task
//! that owns the stream matches replies to callers. Everything else the server pushes
//! (directory, worker changes, events) fans out on a broadcast channel.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use slopty_net::endpoint::SERVER_PORT;
use slopty_net::server::{DialError, ServerLink, connect};
use slopty_net::{Endpoint, HostAddr};
use slopty_proto::orchestration::{ErrorCode, Outcome, Verb};
use slopty_proto::server::{FromServer, Refusal, RequestId, Role, ToServer};
use slopty_tools::Dispatch;
use tokio::sync::{broadcast, mpsc, oneshot};

/// Environment variable naming the server, between `--server` and the settings file.
pub const SERVER_ENV: &str = "SLOPTY_SERVER";

/// First retry after the server drops; doubles up to [`MAX_BACKOFF`].
const MIN_BACKOFF: Duration = Duration::from_millis(250);
/// Longest wait between reconnect attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(5);
/// A link that lived this long was healthy, and its loss restarts the backoff.
const HEALTHY: Duration = Duration::from_secs(10);

/// Where the server is: `--server`, else [`SERVER_ENV`], else `[client] server` in the
/// settings file. The port is [`SERVER_PORT`] unless the address names one.
pub fn locate(flag: Option<&str>, data_dir: &Path) -> Result<HostAddr> {
    let env = std::env::var(SERVER_ENV).ok();
    let settings_path = slopty_settings::path_in(data_dir);
    let file = || {
        let loaded = slopty_settings::Settings::load(&settings_path);
        match loaded.error {
            Some(e) => Err(anyhow!(e)),
            None => Ok(loaded.settings.client.server),
        }
    };
    choose(flag, env.as_deref(), file)?.with_context(|| {
        format!(
            "no server: pass --server host[:port], set {SERVER_ENV}, or set `server` under \
             [client] in {}",
            settings_path.display()
        )
    })
}

/// The first of flag, environment and settings file that names a server. The file is read
/// only when neither of the first two does.
fn choose(
    flag: Option<&str>,
    env: Option<&str>,
    settings: impl FnOnce() -> Result<Option<HostAddr>>,
) -> Result<Option<HostAddr>> {
    fn given(s: Option<&str>) -> Option<&str> {
        s.map(str::trim).filter(|s| !s.is_empty())
    }
    match given(flag).or_else(|| given(env)) {
        Some(address) => Ok(Some(HostAddr::parse_with_port(address, SERVER_PORT)?)),
        None => settings(),
    }
}

/// A verb on its way, and where its answer goes.
#[derive(Debug)]
struct Call {
    verb: Verb,
    reply: oneshot::Sender<Result<Outcome>>,
}

/// A handle on the connection to the server; cheap to clone, every clone shares the stream.
#[derive(Debug, Clone)]
pub struct Link {
    calls: mpsc::Sender<Call>,
}

/// How a connection ended.
enum Ended {
    /// Every [`Link`] handle is gone.
    Released,
    /// The server went away.
    Lost(anyhow::Error),
}

impl Link {
    /// Connect once, failing when the server does not answer. After a loss every call fails.
    /// What the server pushes unasked goes unheard.
    pub async fn connect(endpoint: &Endpoint, server: &HostAddr, role: Role) -> Result<Self> {
        let link = dial(endpoint, server, role).await?;
        let (calls, mut rx) = mpsc::channel(64);
        let (unheard, _) = broadcast::channel(1);
        tokio::spawn(async move {
            if let Ended::Lost(e) = serve(link, None, &mut rx, &unheard).await {
                tracing::debug!(error = %e, "server link lost");
            }
        });
        Ok(Self { calls })
    }

    /// A link held for the process lifetime: it dials in the background and redials whenever
    /// the connection drops. A call made while the server is down tries a dial at once and
    /// fails with the reason when that does not get through.
    ///
    /// The receiver sees everything the server pushes from the first connection on.
    pub fn persistent(
        endpoint: Endpoint,
        server: HostAddr,
        role: Role,
    ) -> (Self, broadcast::Receiver<FromServer>) {
        let (calls, mut rx) = mpsc::channel(64);
        let (events, heard) = broadcast::channel(256);
        tokio::spawn(async move {
            let mut backoff = MIN_BACKOFF;
            let mut held: Option<Call> = None;
            loop {
                match dial(&endpoint, &server, role.clone()).await {
                    Ok(link) => {
                        tracing::info!(%server, "connected to the server");
                        let since = Instant::now();
                        match serve(link, held.take(), &mut rx, &events).await {
                            Ended::Released => return,
                            Ended::Lost(e) => tracing::warn!(%server, error = %e, "server lost"),
                        }
                        if since.elapsed() >= HEALTHY {
                            backoff = MIN_BACKOFF;
                            continue;
                        }
                    }
                    Err(e) => {
                        tracing::debug!(%server, error = %e, "server unreachable");
                        if let Some(call) = held.take() {
                            let _gone = call.reply.send(Err(e));
                        }
                    }
                }
                tokio::select! {
                    () = tokio::time::sleep(backoff) => {}
                    call = rx.recv() => match call {
                        Some(call) => held = Some(call),
                        None => return,
                    },
                }
                backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
            }
        });
        (Self { calls }, heard)
    }

    /// Send `verb` and wait for its outcome, or for the reason it could not be sent.
    async fn request(&self, verb: Verb) -> Result<Outcome> {
        let (reply, answer) = oneshot::channel();
        self.calls
            .send(Call { verb, reply })
            .await
            .map_err(|_closed| anyhow!("the connection to the server is closed"))?;
        answer.await.map_err(|_dropped| anyhow!("the connection to the server was lost"))?
    }
}

/// A link that cannot carry the verb answers with why, as a failure the caller reads like any
/// other.
impl Dispatch for Link {
    async fn call(&self, verb: Verb) -> Outcome {
        self.request(verb).await.unwrap_or_else(|e| Outcome::Error {
            code: ErrorCode::Failed,
            message: format!("{e:#}"),
        })
    }
}

async fn dial(endpoint: &Endpoint, server: &HostAddr, role: Role) -> Result<ServerLink> {
    connect(endpoint, server, role).await.map_err(|e| match e {
        DialError::Refused(Refusal::ProtocolVersion { server: theirs }) => anyhow!(
            "the server at {server} speaks protocol {theirs} and this slopty {}; update the \
             older one",
            slopty_proto::PROTOCOL_VERSION
        ),
        DialError::Refused(why) => anyhow!("the server at {server} refused: {why:?}"),
        DialError::Net(e) => anyhow!("cannot reach the server at {server}: {e}"),
    })
}

/// Run one connection: send calls, route replies to their callers, fan the rest out.
async fn serve(
    link: ServerLink,
    first: Option<Call>,
    calls: &mut mpsc::Receiver<Call>,
    pushed: &broadcast::Sender<FromServer>,
) -> Ended {
    let ServerLink { conn, mut tx, mut rx, .. } = link;
    let mut pending: HashMap<RequestId, oneshot::Sender<Result<Outcome>>> = HashMap::new();
    let mut next: RequestId = 0;
    let mut queued = first;
    let ended = loop {
        let call = if let Some(call) = queued.take() {
            Some(call)
        } else {
            tokio::select! {
                call = calls.recv() => match call {
                    Some(call) => Some(call),
                    None => break Ended::Released,
                },
                msg = rx.recv() => {
                    match msg {
                        Ok(FromServer::Reply { id, outcome }) => {
                            if let Some(waiter) = pending.remove(&id) {
                                let _gone = waiter.send(Ok(outcome));
                            }
                        }
                        Ok(other) => {
                            let _unheard = pushed.send(other);
                        }
                        Err(e) => break Ended::Lost(e.into()),
                    }
                    None
                }
            }
        };
        if let Some(Call { verb, reply }) = call {
            next = next.wrapping_add(1);
            if let Err(e) = tx.send(&ToServer::Request { id: next, verb }).await {
                let _gone = reply.send(Err(anyhow!("the connection to the server was lost")));
                break Ended::Lost(e.into());
            }
            pending.insert(next, reply);
        }
    };
    conn.close(0_u32.into(), b"bye");
    for (_, waiter) in pending {
        let _gone = waiter.send(Err(anyhow!("the connection to the server was lost")));
    }
    ended
}

#[cfg(test)]
mod tests {
    use anyhow::bail;

    use super::*;

    #[test]
    fn the_flag_beats_the_environment_beats_the_settings() {
        let file = || Ok(Some(HostAddr::parse_with_port("from-file", SERVER_PORT)?));
        let pick = |flag, env| choose(flag, env, file).unwrap().map(|a| a.host().to_owned());
        assert_eq!(pick(Some("flag"), Some("env")), Some("flag".to_owned()));
        assert_eq!(pick(None, Some("env")), Some("env".to_owned()));
        assert_eq!(pick(None, None), Some("from-file".to_owned()));
        assert_eq!(pick(Some(" "), Some("")), Some("from-file".to_owned()));
        assert!(choose(None, None, || Ok(None)).unwrap().is_none());
    }

    #[test]
    fn the_settings_file_is_not_read_when_a_flag_names_the_server() {
        let broken = || -> Result<Option<HostAddr>> { bail!("unreadable") };
        assert!(choose(Some("flag"), None, broken).unwrap().is_some());
        choose(None, None, broken).unwrap_err();
    }

    #[test]
    fn the_client_table_names_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let path = slopty_settings::path_in(dir.path());
        std::fs::write(&path, "[client]\nserver = \"studio:7\"\n").unwrap();
        let found = locate(None, dir.path()).unwrap();
        assert_eq!((found.host(), found.port()), ("studio", 7));
        std::fs::write(&path, "[client]\nserver = 7\n").unwrap();
        locate(None, dir.path()).unwrap_err();
    }

    #[test]
    fn a_server_address_takes_the_server_port_unless_it_names_one() {
        let dir = std::env::temp_dir();
        let bare = locate(Some("studio"), &dir).unwrap();
        assert_eq!((bare.host(), bare.port()), ("studio", SERVER_PORT));
        let explicit = locate(Some("100.64.0.3:7"), &dir).unwrap();
        assert_eq!(explicit.port(), 7, "an explicit port wins");
        let v6 = locate(Some("fd7a:115c:a1e0::1"), &dir).unwrap();
        assert_eq!(v6.port(), SERVER_PORT);
        locate(Some("not a worker"), &dir).unwrap_err();
    }
}
