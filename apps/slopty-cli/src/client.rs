//! Known workers, adding one by address, and connecting as a client.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use slopty_core::ClientId;
use slopty_net::client::{WorkerConn, bind_client, connect};
use slopty_net::known::{KnownWorker, KnownWorkers};
use slopty_net::{Endpoint, HostAddr};
use slopty_proto::handshake::{Caps, ClientKind, Hello};

/// How long a closing endpoint may take to tell its peers.
const CLOSE_GRACE: Duration = Duration::from_millis(500);

/// `$SLOPTY_DATA_DIR`, else `~/Library/Application Support/Slopty`.
pub fn data_dir() -> PathBuf {
    slopty_settings::data_dir()
}

fn known(data_dir: &Path) -> Result<KnownWorkers> {
    Ok(KnownWorkers::open_in(data_dir)?)
}

fn hello(client: ClientId) -> Hello {
    Hello {
        client,
        kind: ClientKind::Tool,
        name: format!("slopty cli @ {}", host_name()),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        caps: Caps::empty(),
    }
}

fn host_name() -> String {
    std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "mac".to_owned())
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
    pub client: ClientId,
    /// Our endpoint (closed with the session).
    pub endpoint: Endpoint,
    /// From before the endpoint was bound to the worker's `HelloAck`.
    pub connect_time: Duration,
}

impl Session {
    /// Close cleanly.
    pub async fn close(self) {
        self.conn.close();
        close_endpoint(&self.endpoint).await;
    }
}

pub async fn connect_to(data_dir: &Path, needle: Option<&str>) -> Result<Session> {
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
