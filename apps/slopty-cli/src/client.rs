//! Connecting straight to a worker as a client: one the server lists, or any `host:port`.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use slopty_client::update::UpdateNotice;
use slopty_core::{ClientId, SessionId};
use slopty_net::client::{WorkerConn, bind_client, connect};
use slopty_net::{ClientMsg, Endpoint, HostAddr, WorkerMsg};
use slopty_proto::RequestId;
use slopty_proto::handshake::Hello;
use slopty_proto::server::Role;
use slopty_proto::terminal::OpenSession;
use slopty_tools::Dispatch;
use slopty_tools::resolve::Resolver;

/// How long a closing endpoint may take to tell its peers.
const CLOSE_GRACE: Duration = Duration::from_millis(500);

fn client_id(data_dir: &Path) -> Result<ClientId> {
    Ok(slopty_net::known::client_id_in(data_dir)?)
}

fn hello(client: ClientId) -> Hello {
    Hello { client, name: format!("slopty cli @ {}", machine_name()) }
}

/// This machine's name as its peers' logs show it: the host name, one `uname` call.
pub fn machine_name() -> String {
    rustix::system::uname().nodename().to_string_lossy().into_owned()
}

/// Close `endpoint`, giving its connections a moment to tell their peers.
pub async fn close_endpoint(endpoint: &Endpoint) {
    endpoint.close(0_u32.into(), b"bye");
    let _drained = tokio::time::timeout(CLOSE_GRACE, endpoint.wait_idle()).await;
}

/// Connect to the worker at `address`. One on a different build fails with what updates it,
/// the same words the app shows on it.
async fn dial(endpoint: &Endpoint, address: &HostAddr, hello: Hello) -> Result<WorkerConn> {
    connect(endpoint, address, hello).await.map_err(|e| {
        match UpdateNotice::for_worker_dial(address.host(), &e) {
            Some(notice) => notice.into(),
            None => e.into(),
        }
    })
}

/// `needle` as an address of its own when it names a port (`host:port`, `[v6]:port`): a
/// measurement's way to a worker no server lists.
fn direct(needle: &str) -> Option<HostAddr> {
    let needle = needle.trim();
    let port = needle.rsplit_once(':').map(|(_, port)| port)?;
    if port.parse::<u16>().is_err() {
        return None;
    }
    needle.parse().ok()
}

/// Where the worker `needle` names is dialled, as the server's directory lists it: by name,
/// id or id prefix, else the only one online.
async fn listed(server: &impl Dispatch, needle: Option<&str>) -> Result<HostAddr> {
    let mut res = Resolver::new(server);
    let id = res.worker(needle).await?;
    let info = res.workers().await?.iter().find(|w| w.worker == id).cloned();
    let info = info.with_context(|| format!("the server lists no worker {id}"))?;
    HostAddr::parse_with_port(&info.address, slopty_net::endpoint::WORKER_PORT)
        .with_context(|| format!("the server lists {} at {:?}", info.name, info.address))
}

/// Where to connect: `needle` itself when it is a `host:port`, else the worker the server
/// lists under it ([`listed`]), the server found as every verb finds it.
async fn pick(data_dir: &Path, server: Option<&str>, needle: Option<&str>) -> Result<HostAddr> {
    if let Some(address) = needle.and_then(direct) {
        return Ok(address);
    }
    let endpoint = bind_client()?;
    let found = async {
        let at = crate::link::locate(server, data_dir, &endpoint).await?;
        let role = Role::Client { name: format!("slopty @ {}", machine_name()) };
        let link = crate::link::Link::connect(&endpoint, &at, role).await?;
        listed(&link, needle).await
    }
    .await;
    close_endpoint(&endpoint).await;
    found
}

/// A live connection to a worker.
pub struct Session {
    /// The connection.
    pub conn: WorkerConn,
    /// Who we are to the worker (the key of its per-client registries).
    #[cfg_attr(
        not(target_vendor = "apple"),
        expect(dead_code, reason = "read by the screen bench, which is Apple's")
    )]
    pub client: ClientId,
    /// Our endpoint (closed with the session).
    pub endpoint: Endpoint,
    /// From before the endpoint was bound to the worker's `HelloAck`.
    pub connect_time: Duration,
}

impl Session {
    /// Open a terminal as `spec` says, and wait for the worker's answer to this request: the
    /// news of other clients' terminals, which may come first, is not it.
    pub async fn open(&mut self, spec: OpenSession) -> Result<SessionId> {
        let request = OPEN_REQUEST;
        self.conn.tx.send(&ClientMsg::OpenSession { request, spec }).await?;
        loop {
            if let Some(answer) = open_answer(request, self.conn.rx.recv().await?) {
                return answer;
            }
        }
    }

    /// Close cleanly.
    pub async fn close(self) {
        self.conn.close();
        close_endpoint(&self.endpoint).await;
    }
}

/// The one open request a command's connection makes.
const OPEN_REQUEST: RequestId = 1;

/// What `msg` answers of open request `request`; `None` when it is about something else.
fn open_answer(request: RequestId, msg: WorkerMsg) -> Option<Result<SessionId>> {
    match msg {
        WorkerMsg::SessionOpened { request: r, summary } if r == request => Some(Ok(summary.id)),
        WorkerMsg::Failed { request: r, message, .. } if r == request => {
            Some(Err(anyhow::anyhow!("the worker could not open it: {message}")))
        }
        _other => None,
    }
}

/// Connect to the worker `needle` names ([`pick`]).
pub async fn connect_to(
    data_dir: &Path,
    server: Option<&str>,
    needle: Option<&str>,
) -> Result<Session> {
    #[cfg(target_vendor = "apple")]
    slopty_client::warm_up_decoder();
    let address = pick(data_dir, server, needle).await?;
    let client = client_id(data_dir)?;
    let started = Instant::now();
    let endpoint = bind_client()?;
    let conn = dial(&endpoint, &address, hello(client)).await?;
    Ok(Session { conn, client, endpoint, connect_time: started.elapsed() })
}

pub async fn sessions(data_dir: &Path, server: Option<&str>, needle: Option<&str>) -> Result<()> {
    let session = connect_to(data_dir, server, needle).await?;
    println!(
        "{}  {}",
        session.conn.ack.name,
        slopty_net::endpoint::describe_path(&session.conn.conn)
    );
    for s in &session.conn.ack.sessions {
        println!(
            "  {}  {}x{}  {:?}  {} viewer(s)  {}",
            s.id, s.cols, s.rows, s.state, s.viewers, s.title
        );
    }
    session.close().await;
    Ok(())
}

/// Application-level round trips: `Ping` on the control stream, `Pong` back. Prints the time to
/// the first `HelloAck`, per-probe and summary numbers, and QUIC's own view of the path, so
/// transport and app latency can be compared.
pub async fn ping(
    data_dir: &Path,
    server: Option<&str>,
    needle: Option<&str>,
    count: u32,
) -> Result<()> {
    use slopty_core::MonoTime;
    use slopty_proto::{ClientMsg, WorkerMsg};

    let mut session = connect_to(data_dir, server, needle).await?;
    println!(
        "{}  connected in {:.1} ms",
        session.conn.ack.name,
        session.connect_time.as_secs_f64() * 1e3
    );
    let mut samples = Vec::with_capacity(count as usize);
    for i in 0..count {
        let sent = MonoTime::now();
        session.conn.tx.send(&ClientMsg::Ping { sent_at: sent }).await?;
        let rtt = loop {
            match session.conn.rx.recv().await? {
                WorkerMsg::Pong { sent_at } if sent_at == sent => {
                    break Duration::from_nanos(MonoTime::now().since(sent).as_nanos());
                }
                _other => {}
            }
        };
        println!("  #{i:<3} app rtt {:>8.3} ms", rtt.as_secs_f64() * 1e3);
        samples.push(rtt);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    samples.sort();
    if let (Some(min), Some(max)) = (samples.first(), samples.last()) {
        let total: Duration = samples.iter().sum();
        let avg = total.checked_div(u32::try_from(samples.len()).unwrap_or(1)).unwrap_or_default();
        let median = samples.get(samples.len() / 2).copied().unwrap_or_default();
        println!(
            "  app rtt min {:.3} ms  median {:.3} ms  avg {:.3} ms  max {:.3} ms",
            min.as_secs_f64() * 1e3,
            median.as_secs_f64() * 1e3,
            avg.as_secs_f64() * 1e3,
            max.as_secs_f64() * 1e3,
        );
    }
    println!("  quic path: {}", slopty_net::endpoint::describe_path(&session.conn.conn));
    session.close().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::orchestration::ErrorCode;
    use slopty_proto::terminal::{SessionState, SessionSummary};

    use super::*;

    fn summary(id: SessionId) -> SessionSummary {
        SessionSummary {
            id,
            title: "zsh".to_owned(),
            cwd: Some("/w".to_owned()),
            repo: None,
            branch: None,
            changes: None,
            started_ms: WallMs::from_millis(1),
            cols: 80,
            rows: 24,
            state: SessionState::Running,
            viewers: 1,
            command: Vec::new(),
            progress: None,
            restored: None,
            repo_id: None,
        }
    }

    /// While an open waits, another client's terminal changes its directory and a second
    /// open's answer goes by: the open takes neither, only the answer to its own request.
    #[test]
    fn an_open_takes_its_own_answer_when_another_session_changes_meanwhile() {
        let (theirs, mine) = (SessionId::new(), SessionId::new());
        let heard = [
            WorkerMsg::SessionChanged(summary(theirs)),
            WorkerMsg::SessionOpened { request: OPEN_REQUEST + 1, summary: summary(theirs) },
            WorkerMsg::SessionOpened { request: OPEN_REQUEST, summary: summary(mine) },
            WorkerMsg::SessionChanged(summary(mine)),
        ];
        let answered: Vec<Option<SessionId>> = heard
            .into_iter()
            .map(|msg| open_answer(OPEN_REQUEST, msg).map(|a| a.unwrap()))
            .collect();
        assert_eq!(answered, [None, None, Some(mine), None]);
    }

    /// A failed open is an error of that request only, with the worker's words.
    #[test]
    fn a_failed_open_is_its_own_requests_error() {
        let failed = |request| WorkerMsg::Failed {
            request,
            code: ErrorCode::Failed,
            message: "no such directory".to_owned(),
        };
        assert!(open_answer(OPEN_REQUEST, failed(OPEN_REQUEST + 1)).is_none());
        let error = open_answer(OPEN_REQUEST, failed(OPEN_REQUEST)).unwrap().unwrap_err();
        assert!(error.to_string().contains("no such directory"), "{error}");
    }

    /// A directory with two workers, one of them away.
    struct Directory(Vec<slopty_proto::server::WorkerInfo>);

    impl Dispatch for Directory {
        fn send(
            &self,
            _key: Option<slopty_proto::orchestration::IdempotencyKey>,
            verb: slopty_proto::orchestration::Verb,
        ) -> impl Future<Output = slopty_proto::orchestration::Outcome> + Send {
            use slopty_proto::orchestration::{Outcome, Verb};
            std::future::ready(match verb {
                Verb::ListWorkers => Outcome::Workers(self.0.clone()),
                other => Outcome::Error { code: ErrorCode::Invalid, message: format!("{other:?}") },
            })
        }
    }

    /// A worker name, id prefix or nothing (the only one online) resolves to the address the
    /// server lists it at; a `host:port` is dialled as it is, with no server asked.
    #[tokio::test]
    async fn a_worker_name_resolves_through_the_server() {
        use slopty_proto::server::{Liveness, Os, WorkerCaps, WorkerInfo};
        let info = |id: &str, name: &str, address: &str, liveness| WorkerInfo {
            worker: id.parse().unwrap(),
            name: name.to_owned(),
            address: address.to_owned(),
            liveness,
            caps: WorkerCaps::bare(Os::MacOs),
            load: 0.0,
            last_seen_ms: WallMs::from_millis(1),
        };
        let studio = info(
            "01a10707-dc02-7034-b011-654084fa9cb9",
            "studio",
            "100.64.0.3:45551",
            Liveness::Online,
        );
        let mini = info(
            "01a10707-dc02-7034-b011-65418dc3a71e",
            "mini",
            "[fd7a:115c:a1e0::9]:45550",
            Liveness::Unreachable,
        );
        let server = Directory(vec![studio, mini]);
        let at = |a: &HostAddr| (a.host().to_owned(), a.port());
        let named = listed(&server, Some("studio")).await.unwrap();
        assert_eq!(at(&named), ("100.64.0.3".to_owned(), 45551));
        let by_id = listed(&server, Some("01a10707-dc02-7034-b011-65418")).await.unwrap();
        assert_eq!(at(&by_id), ("fd7a:115c:a1e0::9".to_owned(), 45550));
        let only = listed(&server, None).await.unwrap();
        assert_eq!(at(&only), ("100.64.0.3".to_owned(), 45551), "the only one online");
        listed(&server, Some("nowhere")).await.unwrap_err();

        assert_eq!(
            direct("127.0.0.1:45551").map(|a| at(&a)),
            Some(("127.0.0.1".to_owned(), 45551))
        );
        assert_eq!(direct("[::1]:7").map(|a| at(&a)), Some(("::1".to_owned(), 7)));
        assert_eq!(direct("studio"), None, "a name is the server's to resolve");
        assert_eq!(direct("Mac Studio"), None);
    }
}
