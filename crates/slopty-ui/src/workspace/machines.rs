//! What can be done to each machine, and what it says of itself: the menu on its row in the
//! navigator ("…", beside the chevron and "+").
//!
//! The menu leads with the machine as it reports itself, read-only: its system and load, then
//! each coding agent installed there with its version (`WorkerCaps::agents`), so which agent
//! runs where, and at what version, is one look away. Nothing there signs in or installs: an
//! agent's own word is all Slopty shows of it. Then what the app lets the person do to the
//! machine: "Update" while it runs another build, "Connect" while its link is down, "Wake"
//! while the server can wake it, sharing the clipboard with it or not, editing its settings
//! file in a file tile, and "Forget" for one the server lists as away. The app says which of
//! the connections and the forgetting it can do ([`HostActions`]); the workspace only shows
//! machines.

use std::collections::HashMap;

use gpui::{Context, SharedString, WeakEntity};
use slopty_client::layout::WorkerKey;

use super::{MenuEntry, MenuGroup, MenuRun, WorkspaceView};

/// A machine row's way to stop sharing the clipboard with it.
pub(super) const STOP_SHARING_CLIPBOARD: &str = "Unshare clipboard";

/// A machine row's way to share the clipboard with it again.
pub(super) const SHARE_CLIPBOARD: &str = "Share clipboard";

/// A machine row's way to a new shell on it, in its home.
pub(super) const NEW_SHELL_HERE: &str = "New shell here";

/// A machine row's way to its settings file.
pub(super) const EDIT_SETTINGS: &str = "Edit settings";

/// What a machine's menu is called, after its name.
pub(super) const MACHINE_MENU: &str = "Machine actions";

/// What the app lets the person do to one worker.
#[derive(Clone, Default)]
pub struct HostActions {
    /// Dial it now rather than at the end of the backoff; offered while its link is down.
    pub connect: Option<MenuRun>,
    /// Forget it: offered while the server lists it as not online, and asked of the server.
    pub forget: Option<MenuRun>,
    /// Wake it from sleep: offered while the server can send it a magic packet
    /// (`slopty_client::directory::Directory::can_wake`). The palette offers it too.
    pub wake: Option<MenuRun>,
    /// Take the worker and everything else of Slopty's off it, then forget it: offered where
    /// the app can reach machines to change them. Its confirm comes first
    /// (the remove sheet); the palette offers it too.
    pub remove: Option<MenuRun>,
    /// It is this Mac, which Slopty then no longer opens at login on.
    pub here: bool,
}

impl std::fmt::Debug for HostActions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostActions")
            .field("connect", &self.connect.is_some())
            .field("forget", &self.forget.is_some())
            .field("wake", &self.wake.is_some())
            .field("remove", &self.remove.is_some())
            .field("here", &self.here)
            .finish()
    }
}

/// What the app says can be done to the machines, and how a machine is added.
#[derive(Default)]
pub(super) struct Machines {
    /// What can be done to each worker, as the app says.
    hosts: HashMap<WorkerKey, HostActions>,
    /// The way to add a worker, as the app says: the empty workspace's.
    add: Option<MenuRun>,
}

impl std::fmt::Debug for Machines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Machines")
            .field("hosts", &self.hosts)
            .field("add", &self.add.is_some())
            .finish()
    }
}

impl WorkspaceView {
    /// What can be done to each worker, and the way to add one. The app says, since
    /// connecting and forgetting are its: the workspace only shows workers.
    pub fn set_host_actions(
        &mut self,
        hosts: HashMap<WorkerKey, HostActions>,
        add: Option<MenuRun>,
        cx: &mut Context<Self>,
    ) {
        self.machines.hosts = hosts;
        self.machines.add = add;
        cx.notify();
    }

    /// The app's way to add a worker, if it gave one.
    pub(super) fn add_worker_run(&self) -> Option<MenuRun> {
        self.machines.add.clone()
    }

    /// What the app lets this client do to `key`.
    pub(super) fn host_actions(&self, key: WorkerKey) -> Option<&HostActions> {
        self.machines.hosts.get(&key)
    }

    /// The lines that lead `key`'s menu, read-only: its system and load, what to run there
    /// while its services stop at logout, then each agent installed there with its version.
    /// Empty until the worker has said what it is.
    pub(super) fn machine_facts(&self, key: WorkerKey) -> Vec<String> {
        let Some(caps) = self.workers.get(&key).and_then(|w| Some((w.caps.as_ref()?, w.load)))
        else {
            return Vec::new();
        };
        let (caps, load) = caps;
        let system = (!caps.os_version.is_empty()).then(|| super::navigator::host_line(caps, load));
        // The worker's words start lower case, to follow the machine's name in a sentence.
        let stops = caps.stops_at_logout.as_deref().map(|how| {
            let mut chars = how.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        });
        let agents = caps.agents.iter().map(|installed| {
            let name = super::projects::agent_label(&installed.agent);
            // `claude --version` says "2.1.3 (Claude Code)" and `codex --version` "codex-cli
            // 0.48.0": the number is what is news, wherever it stands.
            let mut words = installed.version.split_whitespace();
            let number = words.clone().find(|w| w.starts_with(|c: char| c.is_ascii_digit()));
            match number.or_else(|| words.next()) {
                Some(version) => format!("{name} {version}"),
                None => name,
            }
        });
        system.into_iter().chain(stops).chain(agents).collect()
    }

    /// `key`'s facts as its menu leads with them: a quiet line each, then a rule.
    pub(super) fn machine_facts_rows(&self, key: WorkerKey) -> Vec<gpui::AnyElement> {
        use gpui::accesskit::Role;
        use gpui::{
            InteractiveElement as _, IntoElement as _, ParentElement as _,
            StatefulInteractiveElement as _,
        };

        let theme = &self.theme;
        let facts = self.machine_facts(key);
        if facts.is_empty() {
            return Vec::new();
        }
        let mut rows: Vec<gpui::AnyElement> = facts
            .into_iter()
            .enumerate()
            .map(|(ix, line)| {
                let text = SharedString::from(line);
                crate::kit::meta(crate::kit::sheet_row(theme, crate::kit::Row::One), theme)
                    .id(("machine-fact", ix))
                    .debug_selector(move || format!("machine-fact-{ix}"))
                    .role(Role::Label)
                    .aria_label(text.clone())
                    .child(text)
                    .into_any_element()
            })
            .collect();
        rows.push(crate::kit::list_rule(theme));
        rows
    }

    /// What `key`'s menu can do, in the order it lists them.
    pub(super) fn machine_entries(
        &self,
        key: WorkerKey,
        entity: &WeakEntity<Self>,
        cx: &gpui::App,
    ) -> Vec<MenuEntry> {
        let Some(w) = self.workers.get(&key) else { return Vec::new() };
        let host = self.host_actions(key).cloned().unwrap_or_default();
        let entry = |group, label: &'static str, run: MenuRun| MenuEntry {
            group,
            label: SharedString::from(label),
            detail: SharedString::default(),
            run,
        };
        let shared = self.clipboard_shared(key);
        let share = super::actions::ShareClipboard { worker: key, share: !shared };
        let sharer = entity.clone();
        let share: MenuRun = std::rc::Rc::new(move |window, cx| {
            let _gone = sharer.update(cx, |this, cx| this.share_clipboard(&share, window, cx));
        });
        let clipboard = if shared { STOP_SHARING_CLIPBOARD } else { SHARE_CLIPBOARD };
        let settings = w.settings.is_some().then(|| {
            let entity = entity.clone();
            let edit = super::actions::EditMachineSettings { worker: key };
            let run: MenuRun = std::rc::Rc::new(move |window, cx| {
                let _gone =
                    entity.update(cx, |this, cx| this.edit_machine_settings(&edit, window, cx));
            });
            run
        });
        let update = self.update_run(key, cx);
        let remove = host.remove.is_some().then(|| {
            let entity = entity.clone();
            let run: MenuRun = std::rc::Rc::new(move |window, cx| {
                let _gone = entity.update(cx, |this, cx| this.ask_remove_machine(key, window, cx));
            });
            run
        });
        let connect = host.connect.filter(|_| !w.status.is_up());
        // Its header's "+" under the pointer, here for a finger, which has no hover.
        let shell = w.link.is_some().then(|| {
            let entity = entity.clone();
            let run: MenuRun = std::rc::Rc::new(move |_window, cx| {
                let _gone = entity.update(cx, |this, cx| {
                    this.open_session_on(key, None, Vec::new(), None, cx);
                });
            });
            run
        });
        [
            shell.map(|run| entry(MenuGroup::Tiles, NEW_SHELL_HERE, run)),
            update.map(|run| entry(MenuGroup::Connections, crate::add_worker::UPDATE, run)),
            connect.map(|run| entry(MenuGroup::Connections, "Connect", run)),
            host.wake.map(|run| entry(MenuGroup::Connections, "Wake", run)),
            Some(entry(MenuGroup::Settings, clipboard, share)),
            settings.map(|run| entry(MenuGroup::Settings, EDIT_SETTINGS, run)),
            host.forget.map(|run| entry(MenuGroup::Removal, "Forget", run)),
            remove.map(|run| entry(MenuGroup::Removal, super::machine_remove::REMOVE, run)),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// "Edit `<machine>`'s settings": its `settings.toml` in a file tile, an open one focused.
    pub(super) fn edit_machine_settings(
        &mut self,
        edit: &super::actions::EditMachineSettings,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.workers.get(&edit.worker).and_then(|w| w.settings.clone()) else {
            return;
        };
        let _shown = self.show_file(Some(edit.worker), &path, None, cx);
    }

    /// "Edit `<machine>`'s settings" for each machine whose greeting said where its file is.
    pub(super) fn settings_lines(&self) -> Vec<crate::palette::PaletteItem> {
        self.workers
            .iter()
            .filter(|(_, w)| w.settings.is_some())
            .map(|(key, w)| {
                crate::palette::PaletteItem::new(
                    &format!("Edit {}'s settings", w.name),
                    Box::new(super::actions::EditMachineSettings { worker: *key }),
                    &[],
                )
            })
            .collect()
    }
}
