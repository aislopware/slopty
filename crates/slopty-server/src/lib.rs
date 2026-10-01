//! The Slopty server: the control plane (`docs/decisions/topology.md`).
//!
//! Workers dial it and hold one QUIC link each, which is their lease and the channel verbs go
//! down. Clients, the CLI and agents dial it for the worker directory, its changes, and to send
//! verbs; AI agents also reach the same verbs over MCP. It is never on the data path: terminal
//! rows and video go client ↔ worker directly.
//!
//! * [`hub`] — the registry, the leases and the one verb dispatch.
//! * [`project`] — projects: their records, path claims, placement, and how agents move tasks.
//! * [`store`] — the state files: known workers and every project, across restarts.
//! * [`link`] — the QUIC front end.
//! * [`mcp`] — the MCP front end (Streamable HTTP).

#![forbid(unsafe_code)]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

mod deliver;
pub mod hub;
pub mod link;
pub mod mcp;
mod placement;
pub mod project;
pub mod store;

use std::net::SocketAddr;
use std::path::PathBuf;

pub use hub::{Acting, GONE_AFTER, Hub, Lan, Lease, Speaker, SystemLan, WAIT_CAP_MS};
pub use mcp::Mcp;
use slopty_net::admission::Admission;
use slopty_net::server::ServerListener;
pub use store::{ProjectStore, Store};
use tokio::task::JoinHandle;

/// Log whether this machine serves as a Tailscale peer relay: the server's machine is always
/// on, which makes it the one to relay links that cannot go direct
/// (`docs/decisions/transport.md`, "The server's machine as a peer relay").
async fn say_relay(api: slopty_tailnet::LocalApi) {
    match api.relay_server_port().await {
        Ok(Some(port)) => tracing::info!(port, "this machine is a Tailscale peer relay"),
        Ok(None) => tracing::info!(
            "this machine is no Tailscale peer relay; `slopty server relay` says why it helps"
        ),
        Err(e) => tracing::debug!(error = %e, "tailscale prefs"),
    }
}

/// Server errors.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// The QUIC listener did not bind.
    #[error(transparent)]
    Net(#[from] slopty_net::NetError),
    /// The MCP listener did not bind.
    #[error("mcp listener on {addr}: {source}")]
    Mcp {
        /// Where.
        addr: SocketAddr,
        /// Why.
        source: std::io::Error,
    },
    /// A state file could not be read: the server does not start rather than write over it.
    #[error("state file {path}: {source}")]
    State {
        /// Which.
        path: PathBuf,
        /// Why.
        source: std::io::Error,
    },
}

/// How to run a server.
#[derive(Clone, Debug)]
pub struct Config {
    /// The name every link's `Welcome` carries.
    pub name: String,
    /// Where the QUIC listener binds (UDP).
    pub quic: SocketAddr,
    /// Where the MCP listener binds (TCP).
    pub mcp: SocketAddr,
    /// Where the state file lives.
    pub data_dir: PathBuf,
    /// Who may connect, on both listeners.
    pub admission: Admission,
}

/// A running server: both listeners, the registry and its state file.
#[derive(Debug)]
pub struct Server {
    hub: Hub,
    listener: ServerListener,
    quic: SocketAddr,
    mcp: SocketAddr,
    store: Store,
    tasks: Vec<JoinHandle<()>>,
    /// Keeps the projects; finishes, writing them, once the hub stops sending it changes.
    keeper: JoinHandle<()>,
}

impl Server {
    /// Load the state file, bind both listeners and start serving.
    pub async fn start(config: Config) -> Result<Self, ServerError> {
        let store = Store::in_dir(&config.data_dir);
        let projects = ProjectStore::in_dir(&config.data_dir);
        let unreadable = |path: &std::path::Path| {
            let path = path.to_owned();
            move |source| ServerError::State { path, source }
        };
        let hub = Hub::new(config.name, store.load().await.map_err(unreadable(store.path()))?);
        let kept = projects.load().await.map_err(unreadable(projects.path()))?;
        hub.adopt_projects(kept.clone());
        let keeper = tokio::spawn(projects.keep(kept, hub.keep_projects()));
        let listener = ServerListener::bind(config.quic, config.admission.clone())?;
        let quic = listener.local_addr()?;
        let mcp_listener = mcp::bind(config.mcp)
            .map_err(|source| ServerError::Mcp { addr: config.mcp, source })?;
        let mcp = mcp_listener
            .local_addr()
            .map_err(|source| ServerError::Mcp { addr: config.mcp, source })?;
        if let Some(api) = config.admission.local_api() {
            tokio::spawn(say_relay(api));
        }
        let tasks = vec![
            tokio::spawn(store.clone().keep(hub.persisted())),
            tokio::spawn(Hub::deliver_reports(hub.downgrade())),
            tokio::spawn(Hub::publish_ladder(hub.downgrade())),
            tokio::spawn(link::serve(listener.clone(), hub.clone())),
            tokio::spawn(mcp::serve(mcp_listener, config.admission, hub.clone())),
        ];
        tracing::info!(name = %hub.name(), %quic, %mcp, state = %store.path().display(), "serving");
        Ok(Self { hub, listener, quic, mcp, store, tasks, keeper })
    }

    /// The registry.
    #[must_use]
    pub const fn hub(&self) -> &Hub {
        &self.hub
    }

    /// Where the QUIC listener listens.
    #[must_use]
    pub const fn quic_addr(&self) -> SocketAddr {
        self.quic
    }

    /// Where the MCP listener listens.
    #[must_use]
    pub const fn mcp_addr(&self) -> SocketAddr {
        self.mcp
    }

    /// Stop: close every link (workers see their lease end and reconnect to the next server),
    /// stop both listeners, and write the state file one last time.
    pub async fn shutdown(self) {
        self.listener
            .endpoint()
            .close(slopty_net::worker::close_code::NORMAL.into(), b"server stopping");
        for task in &self.tasks {
            task.abort();
        }
        let _drained = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            self.listener.endpoint().wait_idle(),
        )
        .await;
        let workers = self.hub.persisted().borrow().clone();
        if !workers.is_empty()
            && let Err(e) = self.store.save(&workers).await
        {
            tracing::warn!(error = %e, "state not saved at shutdown");
        }
        self.hub.stop_keeping();
        if let Err(e) = self.keeper.await {
            tracing::warn!(error = %e, "projects not saved at shutdown");
        }
    }
}
