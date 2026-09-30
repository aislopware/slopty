//! The link to the server: where it is, and one QUIC connection that carries verbs.
//!
//! Requests are pipelined: each gets an id, the server answers in any order, and the one task
//! that owns the stream matches replies to callers. Everything else the server pushes
//! (directory, worker changes, events) fans out on a broadcast channel.
//!
//! A verb that changes something always goes with an idempotency key, the caller's or a fresh
//! one, and a verb whose answer a dropped link lost ([`ErrorCode::Interrupted`]) is sent again
//! under the same key, which the worker does not do twice.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use slopty_client::update::UpdateNotice;
use slopty_net::endpoint::SERVER_PORT;
use slopty_net::redial::Redial;
use slopty_net::server::{DialError, ServerLink, connect};
use slopty_net::{Endpoint, HostAddr, NetError};
use slopty_proto::RequestId;
use slopty_proto::codec::CodecError;
use slopty_proto::orchestration::{ErrorCode, IdempotencyKey, Outcome, Verb};
use slopty_proto::server::{FromServer, Role, ToServer};
use slopty_tools::Dispatch;
use tokio::sync::{broadcast, mpsc, oneshot};

/// Environment variable naming the server, between `--server` and the settings file. Every
/// session a worker runs has it, so `slopty` and `slopty mcp` inside one need no flag.
pub const SERVER_ENV: &str = slopty_proto::project::SERVER_ENV;

/// Where the server is: [`configured`], else the first server that answers on the tailnet,
/// dialled from `endpoint` (`slopty_net::discover`).
pub async fn locate(flag: Option<&str>, data_dir: &Path, endpoint: &Endpoint) -> Result<HostAddr> {
    if let Some(configured) = configured(flag, data_dir)? {
        return Ok(configured);
    }
    if let Some(found) = slopty_net::discover::find(endpoint).await {
        tracing::info!(server = %found.name, addr = %found.addr, "found the server on the tailnet");
        return Ok(found.host_addr());
    }
    bail!(
        "no server: none answered on the tailnet; pass --server host[:port], set {SERVER_ENV}, \
         or set `server` under [client] in {}",
        slopty_settings::path_in(data_dir).display()
    )
}

/// The server a person named: `--server`, else [`SERVER_ENV`], else `[client] server` in the
/// settings file. The port is [`SERVER_PORT`] unless the address names one.
pub fn configured(flag: Option<&str>, data_dir: &Path) -> Result<Option<HostAddr>> {
    let env = std::env::var(SERVER_ENV).ok();
    let settings_path = slopty_settings::path_in(data_dir);
    let file = || {
        let loaded = slopty_settings::Settings::load(&settings_path);
        match loaded.error {
            Some(e) => Err(anyhow!(e)),
            None => Ok(loaded.settings.client.server),
        }
    };
    choose(flag, env.as_deref(), file)
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

/// Between the attempts of a verb whose answer a dropped link lost: the server notices a
/// worker gone within its 5 s idle timeout, and a worker or server back redials in a second or
/// two, so the last try goes about 8 s after the first loss.
const RETRY_PAUSES: [Duration; 5] = [
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
];

/// A verb on its way, and where its answer goes. The answer brings the verb back, so a retry
/// sends it again without copying it.
#[derive(Debug)]
struct Call {
    key: Option<IdempotencyKey>,
    verb: Verb,
    reply: oneshot::Sender<Answer>,
}

impl Call {
    fn answer(self, outcome: Outcome) {
        let _gone = self.reply.send((outcome, Some(self.verb)));
    }
}

/// An outcome, and the verb it answers while that is still in hand.
type Answer = (Outcome, Option<Verb>);

/// A verb sent and waiting on its reply.
struct Waiter {
    verb: Option<Verb>,
    reply: oneshot::Sender<Answer>,
}

impl Waiter {
    fn answer(self, outcome: Outcome) {
        let _gone = self.reply.send((outcome, self.verb));
    }
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
    /// Connect, failing when the server does not answer. After a loss the next call dials
    /// again. What the server pushes unasked goes unheard.
    pub async fn connect(endpoint: &Endpoint, server: &HostAddr, role: Role) -> Result<Self> {
        let mut link = dial(endpoint, server, role.clone()).await?;
        let (endpoint, server) = (endpoint.clone(), server.clone());
        let (calls, mut rx) = mpsc::channel(64);
        let (unheard, _) = broadcast::channel(1);
        tokio::spawn(async move {
            let mut held = None;
            loop {
                match serve(link, held.take(), &mut rx, &unheard).await {
                    Ended::Released => return,
                    Ended::Lost(e) => tracing::debug!(error = %e, "server link lost"),
                }
                link = loop {
                    let Some(call) = rx.recv().await else { return };
                    match dial(&endpoint, &server, role.clone()).await {
                        Ok(link) => {
                            held = Some(call);
                            break link;
                        }
                        Err(e) => {
                            call.answer(unreachable(&e));
                        }
                    }
                };
            }
        });
        Ok(Self { calls })
    }

    /// A link held for the process lifetime: it dials in the background and redials whenever
    /// the connection drops, on the backoff every link follows ([`slopty_net::redial`]), or
    /// after [`slopty_net::redial::WRONG_BUILD`] for a server on a different build. A
    /// call made while the server is down tries a dial at once and fails with the reason when
    /// that does not get through.
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
            let mut redial = Redial::default();
            let mut held: Option<Call> = None;
            loop {
                let wait = match dial(&endpoint, &server, role.clone()).await {
                    Ok(link) => {
                        tracing::info!(%server, "connected to the server");
                        redial.linked(Instant::now());
                        match serve(link, held.take(), &mut rx, &events).await {
                            Ended::Released => return,
                            Ended::Lost(e) => tracing::warn!(%server, error = %e, "server lost"),
                        }
                        redial.next(Instant::now())
                    }
                    Err(e) => {
                        tracing::debug!(%server, error = %e, "server unreachable");
                        if let Some(call) = held.take() {
                            call.answer(unreachable(&e));
                        }
                        redial_after(&e, &mut redial)
                    }
                };
                tokio::select! {
                    () = tokio::time::sleep(wait) => {}
                    call = rx.recv() => match call {
                        Some(call) => held = Some(call),
                        None => return,
                    },
                }
            }
        });
        (Self { calls }, heard)
    }

    /// Send `verb` once and wait for its outcome, or for the reason it could not be sent.
    async fn request(&self, key: Option<IdempotencyKey>, verb: Verb) -> Answer {
        let (reply, answer) = oneshot::channel();
        if let Err(mpsc::error::SendError(call)) = self.calls.send(Call { key, verb, reply }).await
        {
            let closed = unreachable(&anyhow!("the connection to the server is closed"));
            return (closed, Some(call.verb));
        }
        answer.await.unwrap_or_else(|_dropped| (interrupted(), None))
    }
}

/// A link that cannot carry the verb answers with why, as a failure the caller reads like any
/// other.
impl Dispatch for Link {
    async fn send(&self, key: Option<IdempotencyKey>, verb: Verb) -> Outcome {
        retried(key, verb, |key, verb| self.request(key, verb)).await
    }

    /// The session's own: an agent started for a task finds its project and task here.
    fn scope(&self) -> slopty_tools::Scope {
        slopty_tools::Scope::from_env()
    }
}

/// `verb` sent by `attempt`, under `key` or a fresh key when it changes something, and sent
/// again under the same key while a link lost its answer, or cannot reach where it goes after
/// such a loss, for as long as [`RETRY_PAUSES`] lasts.
async fn retried<F: Future<Output = Answer>>(
    key: Option<IdempotencyKey>,
    mut verb: Verb,
    mut attempt: impl FnMut(Option<IdempotencyKey>, Verb) -> F,
) -> Outcome {
    let key = key.or_else(|| verb.changes().then(fresh_key));
    let mut pauses = RETRY_PAUSES.into_iter();
    let mut lost = false;
    loop {
        let (outcome, back) = attempt(key.clone(), verb).await;
        let Outcome::Error { code, message } = &outcome else { return outcome };
        lost |= *code == ErrorCode::Interrupted;
        let again = matches!(
            code,
            ErrorCode::Interrupted | ErrorCode::ServerUnreachable | ErrorCode::WorkerUnreachable
        );
        match pauses.next() {
            Some(pause) if lost && again => {
                // The link went down holding the verb, so there is nothing to send again.
                let Some(back) = back else { return outcome };
                verb = back;
                tokio::time::sleep(pause).await;
            }
            _ if lost && again => {
                let key = key.as_ref().map_or_else(String::new, |k| {
                    format!("; sent again under idempotency key {k}, it is not done twice")
                });
                return Outcome::Error {
                    code: ErrorCode::Interrupted,
                    message: format!("{message}, and the answer to it was lost{key}"),
                };
            }
            _ => return outcome,
        }
    }
}

/// A key for one call and its retries.
fn fresh_key() -> IdempotencyKey {
    IdempotencyKey::from_id(uuid::Uuid::new_v4().as_u128())
}

fn unreachable(why: &anyhow::Error) -> Outcome {
    Outcome::Error { code: ErrorCode::ServerUnreachable, message: format!("{why:#}") }
}

fn interrupted() -> Outcome {
    Outcome::Error {
        code: ErrorCode::Interrupted,
        message: "the connection to the server was lost before the answer came".to_owned(),
    }
}

/// The verb a request carried, back from the message that sent it.
fn sent_verb(request: ToServer) -> Option<Verb> {
    let ToServer::Request { verb, .. } = request else { return None };
    Some(verb)
}

async fn dial(endpoint: &Endpoint, server: &HostAddr, role: Role) -> Result<ServerLink> {
    connect(endpoint, server, role).await.map_err(|e| match e {
        DialError::Refused(why) => anyhow!("the server at {server} refused: {}", why.text()),
        DialError::Net(NetError::WrongBuild(wrong)) => {
            UpdateNotice::server(server.host(), &wrong).into()
        }
        DialError::Net(e) => anyhow!("cannot reach the server at {server}: {e}"),
    })
}

/// How long to wait before dialling again after `e`: the backoff, or for a server on a
/// different build, which changes only when someone updates it, a good while.
fn redial_after(e: &anyhow::Error, redial: &mut Redial) -> Duration {
    if e.downcast_ref::<UpdateNotice>().is_some() {
        slopty_net::redial::WRONG_BUILD
    } else {
        redial.next(Instant::now())
    }
}

/// Run one connection: send calls, route replies to their callers, fan the rest out.
async fn serve(
    link: ServerLink,
    first: Option<Call>,
    calls: &mut mpsc::Receiver<Call>,
    pushed: &broadcast::Sender<FromServer>,
) -> Ended {
    let ServerLink { conn, mut tx, mut rx, .. } = link;
    let mut pending: HashMap<RequestId, Waiter> = HashMap::new();
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
                                waiter.answer(outcome);
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
        if let Some(Call { key, verb, reply }) = call {
            next = next.wrapping_add(1);
            let request = ToServer::Request { id: next, key, verb };
            let sent = tx.send(&request).await;
            let waiter = Waiter { verb: sent_verb(request), reply };
            match sent {
                Ok(()) => {
                    pending.insert(next, waiter);
                }
                // Refused before a byte went out, so the link carries on.
                Err(NetError::Codec(CodecError::TooLarge { len, max })) => {
                    waiter.answer(Outcome::Error {
                        code: ErrorCode::Invalid,
                        message: format!(
                            "the message is {len} bytes, more than the {max} one message carries"
                        ),
                    });
                }
                // Part of it may have gone out, and the server may act on it.
                Err(e) => {
                    waiter.answer(interrupted());
                    break Ended::Lost(e.into());
                }
            }
        }
    };
    conn.close(0_u32.into(), b"bye");
    for (_, waiter) in pending {
        waiter.answer(interrupted());
    }
    ended
}

#[cfg(test)]
mod tests {
    use anyhow::bail;

    use super::*;

    /// A verb whose answer was lost goes again under the same key, a fresh one when the caller
    /// gave none, through a worker still coming back, and as the same bytes rather than a copy;
    /// one never sent is not retried, and one the link dropped is not sent again.
    #[tokio::test(start_paused = true)]
    async fn a_lost_answer_is_asked_again_under_the_same_key() {
        use slopty_core::{SessionId, WorkerId};
        use slopty_proto::orchestration::TermRef;

        let error = |code| Outcome::Error { code, message: "x".to_owned() };
        let mut answers =
            vec![Outcome::Done, error(ErrorCode::WorkerUnreachable), error(ErrorCode::Interrupted)];
        let mut keys = Vec::new();
        let mut sent = Vec::new();
        let write =
            Verb::WriteFile { worker: WorkerId::new(), path: "/f".to_owned(), bytes: vec![7; 64] };
        let outcome = retried(None, write, |key, verb| {
            keys.push(key);
            if let Verb::WriteFile { bytes, .. } = &verb {
                sent.push(bytes.as_ptr());
            }
            std::future::ready((answers.pop().unwrap(), Some(verb)))
        })
        .await;
        assert_eq!(outcome, Outcome::Done);
        assert_eq!(keys.len(), 3);
        assert!(keys[0].is_some() && keys.iter().all(|k| *k == keys[0]), "{keys:?}");
        assert!(sent.len() == 3 && sent.iter().all(|p| *p == sent[0]), "copied: {sent:?}");

        let term = TermRef { worker: WorkerId::new(), session: SessionId::new() };
        let mut tries = 0;
        let outcome = retried(None, Verb::Close { term }, |_key, verb| {
            tries += 1;
            std::future::ready((error(ErrorCode::WorkerUnreachable), Some(verb)))
        })
        .await;
        assert!(matches!(outcome, Outcome::Error { code: ErrorCode::WorkerUnreachable, .. }));
        assert_eq!(tries, 1, "an unreachable worker did nothing");

        let mut tries = 0;
        let outcome = retried(None, Verb::Close { term }, |_key, _verb| {
            tries += 1;
            std::future::ready((error(ErrorCode::Interrupted), None))
        })
        .await;
        assert!(matches!(outcome, Outcome::Error { code: ErrorCode::Interrupted, .. }));
        assert_eq!(tries, 1, "the verb went down with the link");
    }

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
        let found = configured(None, dir.path()).unwrap().unwrap();
        assert_eq!((found.host(), found.port()), ("studio", 7));
        std::fs::write(&path, "[client]\nserver = 7\n").unwrap();
        configured(None, dir.path()).unwrap_err();
    }

    #[test]
    fn a_server_address_takes_the_server_port_unless_it_names_one() {
        let dir = std::env::temp_dir();
        let bare = configured(Some("studio"), &dir).unwrap().unwrap();
        assert_eq!((bare.host(), bare.port()), ("studio", SERVER_PORT));
        let explicit = configured(Some("100.64.0.3:7"), &dir).unwrap().unwrap();
        assert_eq!(explicit.port(), 7, "an explicit port wins");
        let v6 = configured(Some("fd7a:115c:a1e0::1"), &dir).unwrap().unwrap();
        assert_eq!(v6.port(), SERVER_PORT);
        configured(Some("not a worker"), &dir).unwrap_err();
    }
}
