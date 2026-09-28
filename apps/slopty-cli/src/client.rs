//! Known workers, adding one by address, and connecting as a client.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use slopty_core::{ClientId, SessionId};
use slopty_net::client::{WorkerConn, bind_client, connect};
use slopty_net::known::{KnownWorker, KnownWorkers};
use slopty_net::{ClientMsg, Endpoint, HostAddr, WorkerMsg};
use slopty_proto::RequestId;
use slopty_proto::handshake::Hello;
use slopty_proto::terminal::OpenSession;

/// How long a closing endpoint may take to tell its peers.
const CLOSE_GRACE: Duration = Duration::from_millis(500);

fn known(data_dir: &Path) -> Result<KnownWorkers> {
    Ok(KnownWorkers::open_in(data_dir)?)
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

/// Connect to the worker at `address` and remember it under the id it answers with.
pub async fn add(data_dir: &Path, address: &str) -> Result<()> {
    let address: HostAddr = address.parse()?;
    let mut me = known(data_dir)?;
    let endpoint = bind_client()?;
    eprintln!("connecting to {address}…");
    let conn = connect(&endpoint, &address, hello(me.client())).await?;
    me.remember(KnownWorker {
        address: address.clone(),
        name: conn.ack.name.clone(),
        worker_id: conn.ack.worker,
    })?;
    println!("added {} at {address} ({})", conn.ack.name, conn.ack.worker);
    conn.close();
    close_endpoint(&endpoint).await;
    Ok(())
}

pub fn forget(data_dir: &Path, needle: &str) -> Result<()> {
    let mut me = known(data_dir)?;
    let worker = me.find(needle).context("no unique worker matches")?.clone();
    me.forget(worker.worker_id)?;
    println!("forgot {} ({})", worker.name, worker.address);
    Ok(())
}

/// Where to connect: a known worker by name, address or id prefix; else `needle` itself as an
/// address; else the only known worker.
fn pick(me: &KnownWorkers, needle: Option<&str>) -> Result<HostAddr> {
    if let Some(n) = needle {
        if let Some(known) = me.find(n) {
            return Ok(known.address.clone());
        }
        return n
            .parse()
            .with_context(|| format!("{n:?} is neither a known worker nor an address"));
    }
    match me.workers() {
        [one] => Ok(one.address.clone()),
        [] => bail!("no workers; run `slopty add <host[:port]>`"),
        _many => bail!("several workers; pass --worker"),
    }
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

pub async fn connect_to(data_dir: &Path, needle: Option<&str>) -> Result<Session> {
    #[cfg(target_vendor = "apple")]
    slopty_client::warm_up_decoder();
    let me = known(data_dir)?;
    let address = pick(&me, needle)?;
    let client = me.client();
    let started = Instant::now();
    let endpoint = bind_client()?;
    let conn = connect(&endpoint, &address, hello(client)).await?;
    Ok(Session { conn, client, endpoint, connect_time: started.elapsed() })
}

pub async fn sessions(data_dir: &Path, needle: Option<&str>) -> Result<()> {
    let session = connect_to(data_dir, needle).await?;
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
pub async fn ping(data_dir: &Path, needle: Option<&str>, count: u32) -> Result<()> {
    use slopty_core::MonoTime;
    use slopty_proto::{ClientMsg, WorkerMsg};

    let mut session = connect_to(data_dir, needle).await?;
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
            agent: None,
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
}
