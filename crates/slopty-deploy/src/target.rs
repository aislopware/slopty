//! Who a deploy reaches and whom the new worker reports to: the machine as `ssh` takes it
//! ([`Target`]), the server it registers with ([`Server`]), and the targets kept per worker so
//! a later update reaches the machine the same way ([`Remembered`]).

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A machine as `ssh` takes it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Target {
    /// The host: a config alias, a name, an address.
    pub host: String,
    /// `-l`, when not the config's.
    pub user: Option<String>,
    /// `-p`, when not the config's.
    pub port: Option<u16>,
}

impl Target {
    /// `host` with the config's user and port.
    #[must_use]
    pub fn host(host: &str) -> Self {
        Self { host: host.to_owned(), user: None, port: None }
    }

    /// The fields as a person typed them; `user@host` in the host field fills the user.
    ///
    /// # Errors
    ///
    /// The field to fix and why, as a sentence.
    pub fn read(host: &str, user: &str, port: &str) -> Result<Self, String> {
        let (mut host, mut user) = (host.trim(), user.trim());
        if let Some((named, at)) = host.split_once('@')
            && user.is_empty()
        {
            (user, host) = (named, at);
        }
        if host.is_empty() {
            return Err("Type the machine's name or address.".to_owned());
        }
        if host.starts_with('-') || host.contains(char::is_whitespace) || user.starts_with('-') {
            return Err(format!("{host} is not a host name."));
        }
        let port = match port.trim() {
            "" => None,
            port => Some(
                port.parse::<u16>()
                    .ok()
                    .filter(|p| *p > 0)
                    .ok_or_else(|| "The port is a number from 1 to 65535.".to_owned())?,
            ),
        };
        let user = (!user.is_empty()).then(|| user.to_owned());
        Ok(Self { host: host.to_owned(), user, port })
    }

    /// Whether the target is the machine this runs on, so it needs no `ssh` at all: a host
    /// with the config's user and port that is loopback, or that resolves to an address of
    /// this machine (its tailnet name, its LAN address), which only a local socket can bind.
    pub async fn is_this_machine(&self) -> bool {
        if self.user.is_some() || self.port.is_some() {
            return false;
        }
        if is_loopback(&self.host) {
            return true;
        }
        let host = self.host.trim_start_matches('[').trim_end_matches(']');
        let Ok(addresses) = tokio::net::lookup_host((host, 0)).await else { return false };
        addresses
            .into_iter()
            .any(|a| a.ip().is_loopback() || std::net::UdpSocket::bind((a.ip(), 0)).is_ok())
    }
}

/// Whether `host` names this machine from itself.
fn is_loopback(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    bare.eq_ignore_ascii_case("localhost")
        || bare.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// The server a new worker registers with, as the deploying machine dials it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Server {
    /// Its host: a name or an address.
    pub host: String,
    /// Its UDP port.
    pub port: u16,
}

impl Server {
    /// The address the worker on the far side dials: this one, unless it is loopback, which
    /// means the deploying machine itself; then `client`, the address that machine reached the
    /// target from (`$SSH_CONNECTION`'s first word there). `None` when it is loopback and that
    /// is not known, or when it holds what a shell would read as more than an address.
    #[must_use]
    pub fn seen_from(&self, client: Option<&str>) -> Option<String> {
        let host = if is_loopback(&self.host) {
            client.map(str::trim).filter(|c| !c.is_empty() && !is_loopback(c))?
        } else {
            self.host.as_str()
        };
        let plain = host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '%'));
        if host.is_empty() || !plain {
            return None;
        }
        Some(if host.contains(':') {
            format!("[{host}]:{}", self.port)
        } else {
            format!("{host}:{}", self.port)
        })
    }
}

/// The SSH target each worker was installed through, by worker id, so its tile's "Update"
/// reaches the machine as the install did (`<data dir>/deployed.json`).
#[derive(Debug)]
pub struct Remembered {
    path: PathBuf,
    targets: BTreeMap<String, Target>,
}

/// The file's name under the data directory.
pub const REMEMBERED: &str = "deployed.json";

impl Remembered {
    /// The targets kept under `data_dir`; none when the file is missing or unreadable, since
    /// a target is a convenience that the host it was dialled at stands in for.
    #[must_use]
    pub fn open_in(data_dir: &Path) -> Self {
        let path = data_dir.join(REMEMBERED);
        let targets = std::fs::read(&path)
            .ok()
            .and_then(|bytes| {
                serde_json::from_slice(&bytes)
                    .inspect_err(|e| tracing::warn!(path = %path.display(), error = %e, "read"))
                    .ok()
            })
            .unwrap_or_default();
        Self { path, targets }
    }

    /// The target `worker` was installed through.
    #[must_use]
    pub fn get(&self, worker: &str) -> Option<&Target> {
        self.targets.get(worker)
    }

    /// Keep `target` for `worker`, written through a file moved over the old one.
    ///
    /// # Errors
    ///
    /// When the file cannot be written.
    pub fn remember(&mut self, worker: &str, target: Target) -> std::io::Result<()> {
        self.targets.insert(worker.to_owned(), target);
        let bytes = serde_json::to_vec_pretty(&self.targets).map_err(std::io::Error::other)?;
        let part = self.path.with_extension("json.part");
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&part, bytes)?;
        std::fs::rename(&part, &self.path)
    }
}
