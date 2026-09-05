//! Identity, pairing, and connecting as a client.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use slopty_core::ClientId;
use slopty_net::client::{HostConn, bind_client, connect, connect_with_ticket};
use slopty_net::identity::{Identity, KnownHost};
use slopty_net::pairing::PairTicket;
use slopty_net::{EndpointAddr, EndpointId, Reach};
use slopty_proto::PROTOCOL_VERSION;
use slopty_proto::handshake::{Caps, ClientKind, Hello};

/// `$SLOPTY_DATA_DIR`, else `~/Library/Application Support/Slopty`.
pub fn data_dir() -> PathBuf {
    slopty_settings::data_dir()
}

fn identity(data_dir: &Path) -> Result<Identity> {
    Ok(Identity::open(&data_dir.join("client.json"))?)
}

fn hello(client: ClientId) -> Hello {
    Hello {
        protocol: PROTOCOL_VERSION,
        client,
        kind: ClientKind::Tool,
        name: format!("slopty cli @ {}", host_name()),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        caps: Caps::empty(),
        pair_token: None,
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

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

pub async fn pair(data_dir: &Path, ticket: &str) -> Result<()> {
    let ticket: PairTicket = ticket.trim().parse().context("parse ticket")?;
    let mut me = identity(data_dir)?;
    let reach = Reach::from_env();
    let endpoint = bind_client(me.secret().clone(), reach).await?;
    eprintln!("connecting to {}…", ticket.addr.id);
    let conn = connect_with_ticket(&endpoint, reach, &ticket, hello(me.client())).await?;
    me.remember(KnownHost {
        host: conn.ack.host,
        name: conn.ack.name.clone(),
        addr: ticket.addr.clone(),
        paired_at: unix_now(),
    })?;
    println!("paired with {} ({})", conn.ack.name, ticket.addr.id);
    conn.conn.close(0_u32.into(), b"paired");
    endpoint.close().await;
    Ok(())
}

pub fn hosts(data_dir: &Path) -> Result<()> {
    let me = identity(data_dir)?;
    println!("client {}  endpoint {}", me.client(), me.secret().public());
    for (id, h) in me.hosts() {
        println!("{id}  {}  ({})", h.name, h.host);
    }
    Ok(())
}

pub fn forget(data_dir: &Path, needle: &str) -> Result<()> {
    let mut me = identity(data_dir)?;
    let (id, host) = me.find(needle).context("no unique host matches")?;
    me.forget(&id)?;
    println!("forgot {} ({id})", host.name);
    Ok(())
}

/// Pick a host: by prefix, or the only one.
fn pick(me: &Identity, needle: Option<&str>) -> Result<(EndpointId, KnownHost)> {
    if let Some(n) = needle {
        return me.find(n).context("no unique host matches");
    }
    let hosts = me.hosts();
    match hosts.as_slice() {
        [one] => Ok(one.clone()),
        [] => bail!("no paired hosts; run `slopty pair <ticket>`"),
        _many => bail!("several hosts; pass --host"),
    }
}

/// A live connection to a paired host.
pub struct Session {
    /// The connection.
    pub conn: HostConn,
    /// Who we are to the host (the key of its per-client registries).
    pub client: ClientId,
    /// Our endpoint (closed with the session).
    pub endpoint: slopty_net::Endpoint,
}

impl Session {
    /// Close cleanly.
    pub async fn close(self) {
        self.conn.conn.close(0_u32.into(), b"bye");
        self.endpoint.close().await;
    }
}

pub async fn connect_to(data_dir: &Path, needle: Option<&str>) -> Result<Session> {
    slopty_client::warm_up_decoder();
    let me = identity(data_dir)?;
    let (_id, known) = pick(&me, needle)?;
    let reach = Reach::from_env();
    let endpoint = bind_client(me.secret().clone(), reach).await?;
    let addr: EndpointAddr = known.addr;
    let client = me.client();
    let conn = connect(&endpoint, reach, addr, hello(client)).await?;
    Ok(Session { conn, client, endpoint })
}

pub async fn sessions(data_dir: &Path, needle: Option<&str>) -> Result<()> {
    let session = connect_to(data_dir, needle).await?;
    println!("{}", session.conn.ack.name);
    println!("  paths: {}", slopty_net::endpoint::describe_paths(&session.conn.conn));
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    println!("  paths after 1.5 s: {}", slopty_net::endpoint::describe_paths(&session.conn.conn));
    for s in &session.conn.ack.sessions {
        println!(
            "  {}  {}x{}  {:?}  {} viewer(s)  {}",
            s.id, s.cols, s.rows, s.state, s.viewers, s.title
        );
    }
    session.close().await;
    Ok(())
}

/// Application-level round trips: `Ping` on the control stream, `Pong` back. Prints per-probe
/// and summary numbers plus the QUIC path view, so transport and app latency can be compared.
pub async fn ping(data_dir: &Path, needle: Option<&str>, count: u32) -> Result<()> {
    use slopty_core::MonoTime;
    use slopty_proto::{ClientMsg, HostMsg};

    let mut session = connect_to(data_dir, needle).await?;
    println!("{}", session.conn.ack.name);
    let mut samples = Vec::with_capacity(count as usize);
    for i in 0..count {
        let sent = MonoTime::now();
        session.conn.tx.send(&ClientMsg::Ping { sent_at: sent }).await?;
        let rtt = loop {
            match session.conn.rx.recv().await? {
                HostMsg::Pong { sent_at } if sent_at == sent => {
                    break std::time::Duration::from_nanos(MonoTime::now().since(sent).as_nanos());
                }
                _other => {}
            }
        };
        println!("  #{i:<3} app rtt {:>8.3} ms", rtt.as_secs_f64() * 1e3);
        samples.push(rtt);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    samples.sort();
    if let (Some(min), Some(max)) = (samples.first(), samples.last()) {
        let total: std::time::Duration = samples.iter().sum();
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
    println!("  quic paths: {}", slopty_net::endpoint::describe_paths(&session.conn.conn));
    session.close().await;
    Ok(())
}
