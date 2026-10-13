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
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use gpui::{AppContext as _, Context};
use slopty_client::WorkerKey;
use slopty_proto::server::Os;
use slopty_view_proto::Device;
use slopty_view_rpc::RpcError;

use crate::workspace::{WorkerStatus, WorkspaceView};

/// The threads the person archived, read from their file the first time they are asked of.
#[derive(Debug, Default)]
pub(super) struct Archived(OnceCell<BTreeSet<String>>);

/// Where the archived threads are kept.
fn archived_path() -> PathBuf {
    slopty_platform::dirs::data_dir().join("interface").join("archived.json")
}

impl Archived {
    /// The archived threads, read from their file the first time.
    fn set(&self) -> &BTreeSet<String> {
        self.0.get_or_init(|| {
            let path = archived_path();
            match std::fs::read(&path) {
                Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                    tracing::warn!(path = %path.display(), %e, "the archived threads do not read");
                    BTreeSet::new()
                }),
                Err(e) => {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!(path = %path.display(), %e, "the archived threads do not read");
                    }
                    BTreeSet::new()
                }
            }
        })
    }

    /// Archive `thread`, or bring it back; whether that changed anything.
    fn file(&mut self, thread: String, archived: bool) -> bool {
        self.set();
        let Some(set) = self.0.get_mut() else { return false };
        if archived { set.insert(thread) } else { set.remove(&thread) }
    }
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
        })
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

    /// The thread a chat id names, as the archive keeps it: a chat this app minted for a
    /// thread is filed under the thread's own id, which outlives the app.
    fn archive_key(&self, chat: &str) -> String {
        self.view_model
            .aliases
            .iter()
            .find(|(_, alias)| alias.as_str() == chat)
            .map_or_else(|| chat.to_owned(), |(thread, _)| thread.to_string())
    }

    /// Whether the person archived the chat `chat`.
    pub(in crate::workspace) fn surface_archived(&self, chat: &str) -> bool {
        self.view_model.surfaces.archived.set().contains(&self.archive_key(chat))
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
        let key = self.archive_key(chat);
        if self.view_model.surfaces.archived.file(key, archived) {
            let kept: Vec<String> =
                self.view_model.surfaces.archived.set().iter().cloned().collect();
            cx.background_spawn(async move {
                let path = archived_path();
                let written = path
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| {
                        let text = serde_json::to_vec(&kept).map_err(std::io::Error::other)?;
                        std::fs::write(&path, text)
                    });
                if let Err(e) = written {
                    tracing::warn!(path = %path.display(), %e, "the archived threads were not kept");
                }
            })
            .detach();
            self.view_changed(cx);
        }
        Some(Ok(serde_json::json!({})))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut archived = Archived(OnceCell::from(BTreeSet::new()));
        assert!(archived.file("t1".to_owned(), true));
        assert!(!archived.file("t1".to_owned(), true));
        assert!(archived.set().contains("t1"));
        assert!(archived.file("t1".to_owned(), false));
        assert!(!archived.file("t1".to_owned(), false));
        assert!(archived.set().is_empty());
    }
}
