//! Settings → Devices' actions: what this app can do to each machine and to the server from
//! here, offered on their rows (`slopty_view_proto::DeviceAction`) and run when pressed
//! ([`slopty_ui::workspace::WorkspaceEvent::DeviceAction`]). Each says how it went in a toast.
//!
//! A machine on an older build offers Update (Cancel update while one runs); one installed from
//! here offers Remove, which asks to be pressed again; one the server lists as away offers Wake
//! and Forget; a Mac's linked machines offer Show in Finder. The server offers Update when it is
//! older, Update all when more than one thing is, and Copy grant while its tailnet policy turns
//! this device away.

use gpui::{App, Context};
use slopty_client::layout::WorkerKey;
use slopty_proto::server::Liveness;
use slopty_view_proto::DeviceAction;

use crate::{Workspace, ssh};

/// The actions' ids, as the rows hand them back.
mod id {
    pub(super) const UPDATE: &str = "update";
    pub(super) const CANCEL_UPDATE: &str = "cancel-update";
    pub(super) const ASK_REMOVE: &str = "ask-remove";
    pub(super) const REMOVE: &str = "remove";
    pub(super) const WAKE: &str = "wake";
    pub(super) const FORGET: &str = "forget";
    pub(super) const FINDER: &str = "finder";
    pub(super) const UPDATE_ALL: &str = "update-all";
    pub(super) const COPY_GRANT: &str = "copy-grant";
}

/// An action named `id`, its button saying `label`.
fn action(id: &str, label: &str) -> DeviceAction {
    DeviceAction { id: id.to_owned(), label: label.to_owned() }
}

impl Workspace {
    /// Tell every machine's row and the server's what they offer now.
    pub(crate) fn publish_device_actions(&self, cx: &mut Context<Self>) {
        let rows: Vec<(WorkerKey, Vec<DeviceAction>)> = self
            .workers
            .iter()
            .map(|slot| (slot.key, self.machine_actions(slot.key, cx)))
            .collect();
        self.view.update(cx, |view, cx| {
            for (key, actions) in rows {
                view.set_device_actions(key, actions, cx);
            }
        });
        self.tell_server_presence(cx);
    }

    /// What the row of the machine `key` offers.
    pub(crate) fn machine_actions(&self, key: WorkerKey, cx: &App) -> Vec<DeviceAction> {
        let Some(slot) = self.workers.iter().find(|w| w.key == key) else { return Vec::new() };
        let view = self.view.read(cx);
        let mut actions = Vec::new();
        if let Some(notice) = view.update_notice(slot.key)
            && self.deployer.is_some()
            && !notice.this_is_older()
        {
            let running = self.updates.get(&notice.host).is_some_and(ssh::UpdateRun::running);
            actions.push(if running {
                action(id::CANCEL_UPDATE, "Cancel update")
            } else {
                action(id::UPDATE, "Update")
            });
        }
        let away = self.directory.get(slot.id).is_some_and(|w| w.liveness != Liveness::Online);
        if away {
            actions.push(action(id::WAKE, "Wake"));
            actions.push(action(id::FORGET, "Forget"));
        } else {
            if cfg!(target_os = "macos")
                && view.workers().any(|(key, _, status)| key == slot.key && status.is_up())
            {
                actions.push(action(id::FINDER, "Show in Finder"));
            }
            let installed = self.deployer.as_ref().is_some_and(|d| d.target_of(slot.id).is_some());
            if installed && !self.removing(slot.id) {
                actions.push(if self.confirm_remove == Some(slot.id) {
                    action(id::REMOVE, "Remove now")
                } else {
                    action(id::ASK_REMOVE, "Remove")
                });
            }
        }
        actions
    }

    /// What the server's row offers.
    pub(crate) fn server_actions(&self, cx: &App) -> Vec<DeviceAction> {
        let mut actions = Vec::new();
        let behind = self.server_update_notice().is_some_and(|n| !n.this_is_older());
        let deploys = self.deployer.is_some();
        if behind && deploys && self.server_update.is_none() {
            actions.push(action(id::UPDATE, "Update"));
        }
        let older = self.older_workers(cx).len();
        if deploys && older.saturating_add(usize::from(behind)) > 1 {
            actions.push(action(id::UPDATE_ALL, "Update all"));
        }
        if self.view.read(cx).server_status() == Some(crate::server::NOT_GRANTED) {
            actions.push(action(id::COPY_GRANT, "Copy grant"));
        }
        actions
    }

    /// One of the rows' actions pressed: `worker`'s, or the server's for `None`.
    pub(crate) fn device_action(
        &mut self,
        worker: Option<WorkerKey>,
        action: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = worker else {
            match action {
                id::UPDATE => self.update_server(cx),
                id::UPDATE_ALL => self.update_all_workers(cx),
                id::COPY_GRANT => Self::copy_grant(cx),
                other => tracing::warn!(other, "no such server action"),
            }
            self.publish_device_actions(cx);
            return;
        };
        let Some(slot) = self.workers.iter().find(|w| w.key == key) else { return };
        let worker = slot.id;
        // A Remove asked for and anything else pressed after: the ask is over.
        if action != id::REMOVE {
            self.confirm_remove = None;
        }
        let host = self.view.read(cx).update_notice(key).map(|notice| notice.host.clone());
        match (action, host) {
            (id::UPDATE, Some(host)) => self.update_worker(&host, cx),
            (id::CANCEL_UPDATE, Some(host)) => self.cancel_update(&host, cx),
            (id::ASK_REMOVE, _) => self.confirm_remove = Some(worker),
            (id::REMOVE, _) if self.confirm_remove.take() == Some(worker) => {
                self.remove_worker(worker, cx);
            }
            (id::WAKE, _) => self.wake_worker(worker, cx),
            (id::FORGET, _) => self.forget_worker(worker, cx),
            (id::FINDER, _) => self.show_workers_in_finder(cx),
            (other, _) => tracing::warn!(other, "no such machine action, or not now"),
        }
        self.publish_device_actions(cx);
    }
}
