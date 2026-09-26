//! How each client's packets travel, as this machine's Tailscale sees it.
//!
//! Straight, through a peer relay, or through DERP. One task reads the daemon's status while any
//! client listens; each client on the tailnet is told its path when it has one and again when it
//! changes.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use slopty_proto::WorkerMsg;
use slopty_proto::tailnet::LinkPath;
use slopty_tailnet::{LocalApi, Path, Status};
use tokio::sync::{mpsc, watch};

/// How often the status is read while a client listens: a path moves from DERP to direct
/// within a few seconds of traffic starting, and a status read costs the daemon a few ms.
const POLL: Duration = Duration::from_secs(2);

/// The daemon's latest status, for every client's path.
#[derive(Clone, Debug)]
pub struct Paths {
    status: Option<Arc<watch::Sender<Option<Arc<Status>>>>>,
}

impl Paths {
    /// Read the status through `api` while any client listens; without one, no client is
    /// told a path.
    pub fn spawn(api: Option<LocalApi>) -> Self {
        let status = api.map(|api| {
            let status = Arc::new(watch::Sender::new(None));
            tokio::spawn(poll(api, Arc::clone(&status)));
            status
        });
        Self { status }
    }

    /// Tell the client at `remote` its path through `out`, each time it changes, until the
    /// client goes. A client that is not on the tailnet is told nothing.
    pub async fn report(&self, remote: SocketAddr, out: mpsc::Sender<WorkerMsg>) {
        let Some(status) = &self.status else { return };
        let ip = remote.ip().to_canonical();
        if !slopty_net::admission::on_tailnet(ip) {
            return;
        }
        report(status.subscribe(), ip, out).await;
    }
}

async fn poll(api: LocalApi, status: Arc<watch::Sender<Option<Arc<Status>>>>) -> ! {
    let mut every = tokio::time::interval(POLL);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        if status.receiver_count() == 0 {
            continue;
        }
        match api.status().await {
            Ok(read) => {
                status.send_replace(Some(Arc::new(read)));
            }
            Err(e) => tracing::debug!(error = %e, "tailscale status"),
        }
    }
}

async fn report(
    mut status: watch::Receiver<Option<Arc<Status>>>,
    ip: IpAddr,
    out: mpsc::Sender<WorkerMsg>,
) {
    let mut told = None;
    loop {
        let path = status.borrow_and_update().as_deref().and_then(|s| link_path(s, ip));
        if let Some(path) = path
            && told.as_ref() != Some(&path)
        {
            tracing::info!(%ip, ?path, "client path");
            if out.send(WorkerMsg::Path(path.clone())).await.is_err() {
                return;
            }
            told = Some(path);
        }
        if status.changed().await.is_err() {
            return;
        }
    }
}

/// The path to the node at `ip`, `None` while none is chosen or no node has it.
fn link_path(status: &Status, ip: IpAddr) -> Option<LinkPath> {
    match status.node_at(ip)?.path() {
        Path::Direct(_) => Some(LinkPath::Direct),
        Path::PeerRelay(_) => Some(LinkPath::PeerRelay),
        Path::Derp(region) => Some(LinkPath::Derp { region }),
        Path::Idle => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(peer_cur_addr: &str, active: bool) -> Arc<Status> {
        let json = format!(
            r#"{{"BackendState":"Running","Peer":{{"k":{{"ID":"n2","HostName":"laptop",
            "DNSName":"laptop.ts.net.","OS":"macOS","TailscaleIPs":["100.64.0.4"],
            "CurAddr":"{peer_cur_addr}","Relay":"fra","Active":{active}}}}}}}"#
        );
        Arc::new(serde_json::from_str(&json).unwrap())
    }

    /// A client is told nothing while no path is chosen, then DERP, then direct once disco
    /// finds one, and nothing again for a status that says the same.
    #[tokio::test]
    async fn a_client_hears_its_path_when_it_changes_and_only_then() {
        let tx = watch::Sender::new(Some(status("", false)));
        let (out, mut heard) = mpsc::channel(8);
        let task = tokio::spawn(report(tx.subscribe(), "100.64.0.4".parse().unwrap(), out));
        let next = async |heard: &mut mpsc::Receiver<WorkerMsg>| match tokio::time::timeout(
            Duration::from_secs(5),
            heard.recv(),
        )
        .await
        {
            Ok(Some(WorkerMsg::Path(path))) => path,
            other => panic!("expected a path, got {other:?}"),
        };
        tx.send_replace(Some(status("", true)));
        assert_eq!(next(&mut heard).await, LinkPath::Derp { region: "fra".into() });
        tx.send_replace(Some(status("", true)));
        tx.send_replace(Some(status("192.168.1.20:41641", true)));
        assert_eq!(next(&mut heard).await, LinkPath::Direct, "no repeat of DERP in between");
        tx.send_replace(Some(status("192.168.1.20:41641", true)));
        drop(tx);
        task.await.unwrap();
        assert!(heard.try_recv().is_err(), "the same path is not told twice");
    }

    /// An address no node has, and one idle node, give no path.
    #[test]
    fn an_unknown_or_idle_node_has_no_path() {
        let idle = status("", false);
        assert_eq!(link_path(&idle, "100.64.0.4".parse().unwrap()), None);
        assert_eq!(link_path(&idle, "100.64.0.9".parse().unwrap()), None);
    }
}
