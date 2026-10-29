//! Settings → Devices and Archived: each machine as the shell's device, from what its worker
//! says of itself, and the threads the person archived (Add device is [`super::adding`]).
//!
//! Archiving is the person's own filing on this app: a thread archived leaves the sidebar and
//! is listed under Settings → Archived until it is brought back. It stops nothing; its agent
//! goes on where it is. The archived threads are kept by thread id in
//! `interface/archived.json` under Slopty's data directory, so they stay archived across
//! launches.

use std::cell::OnceCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use gpui::{AppContext as _, Context};
use slopty_client::WorkerKey;
use slopty_proto::server::Os;
use slopty_view_proto::Device;
use slopty_view_rpc::RpcError;

use crate::workspace::{WorkerStatus, WorkspaceView};

/// A set of names the person filed on this app (the threads archived, the agents switched off),
/// read from its file under Slopty's data directory the first time it is asked of.
#[derive(Debug, Default)]
pub(super) struct Filed(OnceCell<BTreeSet<String>>);

/// Where the archived threads are kept.
fn archived_path() -> PathBuf {
    slopty_platform::dirs::data_dir().join("interface").join("archived.json")
}

impl Filed {
    /// The names, read from `path` the first time.
    pub(super) fn set(&self, path: &Path) -> &BTreeSet<String> {
        self.0.get_or_init(|| match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), %e, "a filed list does not read");
                BTreeSet::new()
            }),
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(path = %path.display(), %e, "a filed list does not read");
                }
                BTreeSet::new()
            }
        })
    }

    /// File `name`, or take it out; whether that changed anything.
    pub(super) fn file(&mut self, path: &Path, name: String, filed: bool) -> bool {
        self.set(path);
        let Some(set) = self.0.get_mut() else { return false };
        if filed { set.insert(name) } else { set.remove(&name) }
    }

    /// Write the names to `path`, off the UI thread.
    pub(super) fn keep<T>(&self, path: PathBuf, cx: &Context<T>) {
        let kept: Vec<String> = self.set(&path).iter().cloned().collect();
        cx.background_spawn(async move {
            let written = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| {
                let text = serde_json::to_vec(&kept).map_err(std::io::Error::other)?;
                std::fs::write(&path, text)
            });
            if let Err(e) = written {
                tracing::warn!(path = %path.display(), %e, "a filed list was not kept");
            }
        })
        .detach();
    }
}

/// The host of an address as the app dials it: `100.64.0.5:7400` is `100.64.0.5`,
/// `[fd7a::1]:7400` is `fd7a::1`, `studio.tail.ts.net` is itself; `None` for nothing.
fn host_of(address: &str) -> Option<String> {
    let address = address.trim();
    let host = if let Some(rest) = address.strip_prefix('[') {
        rest.split_once(']').map_or(rest, |(host, _)| host)
    } else if address.matches(':').count() == 1 {
        address.split_once(':').map_or(address, |(host, _)| host)
    } else {
        address
    };
    (!host.is_empty()).then(|| host.to_owned())
}

/// The id `DeviceAction` names the server by.
const SERVER: &str = "server";

/// The platform a worker's machine runs, as the shell names it.
const fn platform(os: Os) -> &'static str {
    match os {
        Os::MacOs => "macos",
        Os::Linux => "linux",
    }
}

/// The version a build string starts with (`0.4.0` of `0.4.0+wire.…`).
fn version(build: &str) -> Option<String> {
    let version = build.split('+').next().unwrap_or_default().trim();
    (!version.is_empty()).then(|| version.to_owned())
}

impl WorkspaceView {
    /// The worker `key` as the shell's device: its name, what it runs, and whether this app
    /// reaches it now (seen `now`) or since when it has been away.
    pub(in crate::workspace) fn surface_device_row(
        &self,
        key: WorkerKey,
        now: DateTime<Utc>,
    ) -> Option<Device> {
        let worker = self.workers.get(&key)?;
        let linked = matches!(worker.status, WorkerStatus::Connected);
        let away = worker
            .away_since
            .and_then(|since| self.now().checked_sub(since))
            .and_then(|ago| chrono::TimeDelta::from_std(ago).ok())
            .and_then(|ago| now.checked_sub_signed(ago));
        let caps = worker.caps.as_ref();
        Some(Device {
            id: key.to_string(),
            name: worker.name.clone(),
            platform: caps.map(|caps| platform(caps.os)).unwrap_or_default().to_owned(),
            last_seen_at: if linked { Some(now) } else { away },
            created_at: None,
            version: caps.and_then(|caps| version(&caps.build)),
            cursor_sdk_version: None,
            capabilities: Vec::new(),
            actions: self.device_actions.get(&key).cloned().unwrap_or_default(),
            // An editor reaches a folder there over SSH by it; this machine's own needs none.
            ssh_host: worker.address.clone().filter(|_| !self.surface_is_local(key)),
        })
    }

    /// Whether `key` is this machine's own worker.
    fn surface_is_local(&self, key: WorkerKey) -> bool {
        let local = self.view_model.feed.as_ref().and_then(|feed| feed.local_worker());
        local.is_some_and(|local| local == key.to_string())
    }

    /// Where this client reaches `key`'s machine, as the app dials it (`host:port`, `[v6]:port`
    /// or a bare host): its host, which an editor's SSH remote names.
    pub fn set_worker_address(&mut self, key: WorkerKey, address: &str, cx: &mut Context<Self>) {
        let host = host_of(address);
        let Some(worker) = self.workers.get_mut(&key) else { return };
        if worker.address.as_deref() == host.as_deref() {
            return;
        }
        worker.address = host;
        cx.notify();
    }

    /// What Settings → Devices offers to do to `key`'s machine: each action's id comes back
    /// as [`crate::workspace::WorkspaceEvent::DeviceAction`] when it is pressed.
    pub fn set_device_actions(
        &mut self,
        key: WorkerKey,
        actions: Vec<slopty_view_proto::DeviceAction>,
        cx: &mut Context<Self>,
    ) {
        if self.device_actions.get(&key).map_or(actions.is_empty(), |had| *had == actions) {
            return;
        }
        if actions.is_empty() {
            self.device_actions.remove(&key);
        } else {
            self.device_actions.insert(key, actions);
        }
        cx.notify();
    }

    /// One of a machine's actions pressed (`DeviceAction { deviceId, action }`), or the
    /// server's (`deviceId: "server"`): handed to the app.
    pub(in crate::workspace) fn device_action(
        &self,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Result<slopty_view_rpc::RpcReply, RpcError> {
        let device = super::param(params, "deviceId")
            .ok_or_else(|| RpcError::BadParams("deviceId".to_owned()))?;
        let action = super::param(params, "action")
            .ok_or_else(|| RpcError::BadParams("action".to_owned()))?
            .to_owned();
        let worker = if device == SERVER {
            None
        } else {
            Some(
                self.view_worker(device)
                    .ok_or_else(|| RpcError::Failed(format!("No machine {device}")))?,
            )
        };
        cx.emit(crate::workspace::WorkspaceEvent::DeviceAction { worker, action });
        super::value(&serde_json::json!({}))
    }

    /// Whether the person archived the chat `chat`.
    pub(in crate::workspace) fn surface_archived(&self, chat: &str) -> bool {
        self.view_model.surfaces.archived.set(&archived_path()).contains(chat)
    }

    /// A change to the interface's lists the surfaces make: `setChatArchived`. `None` for any
    /// other.
    pub(in crate::workspace) fn surface_mutate(
        &mut self,
        op: &str,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Option<Result<serde_json::Value, RpcError>> {
        if op != "setChatArchived" {
            return None;
        }
        let Some(chat) = super::param(params, "chatId") else {
            return Some(Err(RpcError::BadParams("chatId".to_owned())));
        };
        let Some(archived) = params.get("archived").and_then(serde_json::Value::as_bool) else {
            return Some(Err(RpcError::BadParams("archived".to_owned())));
        };
        let key = chat.to_owned();
        if self.view_model.surfaces.archived.file(&archived_path(), key, archived) {
            self.view_model.surfaces.archived.keep(archived_path(), cx);
            self.view_changed(cx);
        }
        Some(Ok(serde_json::json!({})))
    }

    /// Archive the chats `chats`, kept once for them all.
    pub(in crate::workspace) fn surface_archive(&mut self, chats: Vec<String>, cx: &Context<Self>) {
        let path = archived_path();
        let mut changed = false;
        for chat in chats {
            changed |= self.view_model.surfaces.archived.file(&path, chat, true);
        }
        if changed {
            self.view_model.surfaces.archived.keep(path, cx);
            self.view_changed(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An address as the app dials it gives the host an editor's SSH remote names.
    #[test]
    fn an_address_gives_its_host() {
        assert_eq!(host_of("100.64.0.5:7400").as_deref(), Some("100.64.0.5"));
        assert_eq!(host_of("[fd7a:115c::1]:7400").as_deref(), Some("fd7a:115c::1"));
        assert_eq!(host_of("studio.tail.ts.net").as_deref(), Some("studio.tail.ts.net"));
        assert_eq!(host_of("fd7a:115c::1").as_deref(), Some("fd7a:115c::1"));
        assert_eq!(host_of(" ").as_deref(), None);
    }

    #[test]
    fn a_build_says_its_version() {
        assert_eq!(
            version("0.4.0+wire.0badf00d.20261009T2307Z.commit.1a2b").as_deref(),
            Some("0.4.0")
        );
        assert_eq!(version(""), None);
    }

    #[test]
    fn archiving_twice_changes_nothing_the_second_time() {
        let path = Path::new("/nonexistent/archived.json");
        let mut archived = Filed(OnceCell::from(BTreeSet::new()));
        assert!(archived.file(path, "t1".to_owned(), true));
        assert!(!archived.file(path, "t1".to_owned(), true));
        assert!(archived.set(path).contains("t1"));
        assert!(archived.file(path, "t1".to_owned(), false));
        assert!(!archived.file(path, "t1".to_owned(), false));
        assert!(archived.set(path).is_empty());
    }
}
