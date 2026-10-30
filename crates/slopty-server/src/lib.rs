//! The Slopty server: the control plane (`docs/decisions/topology.md`).
//!
//! Workers dial it and hold one QUIC link each, which is their lease and the channel verbs go
//! down. Clients, the CLI and agents dial it for the worker directory, its changes, and to send
//! verbs; an agent reaches the same verbs as Slopty's MCP tools through `slopty mcp`, the CLI
//! dialled in from its terminal. It is never on the data path: terminal rows and video go
//! client ↔ worker directly.
//!
//! * [`hub`] — the registry, the leases and the one verb dispatch.
//! * [`store`] — the state files: known workers, words on their way to agents and the phones,
//!   across restarts.
//! * [`push`] — notices pushed to pocketed phones, straight to APNs.
//! * [`link`] — the QUIC front end, the only one.

#![forbid(unsafe_code)]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

pub mod hub;
pub mod link;
pub mod push;
pub mod store;

use std::net::SocketAddr;
use std::path::PathBuf;

pub use hub::{GONE_AFTER, Hold, Hub, KeepAwake, Lan, Lease, Speaker, SystemLan, WAIT_CAP_MS};
pub use push::PushConfig;
use slopty_net::admission::Admission;
use slopty_net::server::ServerListener;
pub use store::{PushStore, Store};
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

/// Push notices to phones as `config` says from now on: straight to APNs, or nowhere when it is
/// off. What was pushing before stops once its last push is sent.
///
/// # Errors
/// [`ServerError::Push`] when the system's certificates cannot be used.
pub fn push_as(hub: &Hub, config: PushConfig) -> Result<(), ServerError> {
    let pusher: std::sync::Arc<dyn push::Pusher> = match config {
        PushConfig::Off => {
            hub.push_to(None);
            return Ok(());
        }
        PushConfig::Direct(key) => {
            std::sync::Arc::new(push::DirectPusher::new(key, push::Https::new()?))
        }
        PushConfig::Through(pusher) => pusher,
    };
    let (out, queue) = tokio::sync::mpsc::channel(push::QUEUE);
    hub.push_to(Some(out));
    // It ends when the hub drops its end: pushing set up again, or the server gone.
    tokio::spawn(push::deliver(hub.downgrade(), queue, pusher));
    Ok(())
}

/// Server errors.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// The QUIC listener did not bind.
    #[error(transparent)]
    Net(#[from] slopty_net::NetError),
    /// Pushing to phones could not be set up.
    #[error("push: {0}")]
    Push(#[from] push::SetupError),
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
    /// Where the state file lives.
    pub data_dir: PathBuf,
    /// Who may connect.
    pub admission: Admission,
    /// How notices reach a pocketed phone.
    pub push: PushConfig,
}

/// A running server: its listener, the registry and its state file.
#[derive(Debug)]
pub struct Server {
    hub: Hub,
    listener: ServerListener,
    quic: SocketAddr,
    store: Store,
    tasks: Vec<JoinHandle<()>>,
}

impl Server {
    /// Load the state file, bind the listener and start serving.
    pub async fn start(config: Config) -> Result<Self, ServerError> {
        let listener = ServerListener::bind(config.quic, config.admission.clone())?;
        Self::start_on(config, listener).await
    }

    /// [`Self::start`] on a listener made elsewhere, which `config.quic` does not name: one on
    /// `slopty_shape::sim`'s network ([`ServerListener::on`]), where a test runs the server and
    /// its links on tokio's paused clock.
    pub async fn start_on(config: Config, listener: ServerListener) -> Result<Self, ServerError> {
        let store = Store::in_dir(&config.data_dir);
        let unreadable = |path: &std::path::Path| {
            let path = path.to_owned();
            move |source| ServerError::State { path, source }
        };
        let hub = Hub::new(config.name, store.load().await.map_err(unreadable(store.path()))?);
        let phones = PushStore::in_dir(&config.data_dir);
        let devices = phones.load().await.map_err(unreadable(phones.path()))?;
        let devices = hub.keep_phones(devices);
        push_as(&hub, config.push)?;
        let quic = listener.local_addr()?;
        if let Some(api) = config.admission.local_api() {
            tokio::spawn(say_relay(api));
        }
        let tasks = vec![
            tokio::spawn(phones.keep(devices)),
            tokio::spawn(store.clone().keep(hub.persisted())),
            tokio::spawn(Hub::publish_ladder(hub.downgrade())),
            tokio::spawn(link::serve(listener.clone(), hub.clone())),
        ];

        tracing::info!(name = %hub.name(), %quic, state = %store.path().display(), "serving");
        Ok(Self { hub, listener, quic, store, tasks })
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

    /// Stop: close every link (workers see their lease end and reconnect to the next server),
    /// stop the listener, and write the state file one last time.
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
    }
}
