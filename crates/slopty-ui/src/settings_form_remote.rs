//! Another machine's settings in the form: the keys a worker or the server reads itself
//! (`[worker]`, `[server]`), read and edited on that machine (`Verb::Settings`).
//!
//! A page that holds such keys (Agents, Network) leads with the machine they are for: this
//! device's own file where it runs a worker or a server, else the server or a worker it lists.
//! A phone or an iPad runs neither, so there the page starts on the server. Picking another
//! machine reads its file once; its rows then show what that file holds, and an edit goes to
//! that machine as the edit this device would make to its own file ([`SettingEdit`]), batched
//! as a hand's pause batches a local write. Its answer is the file as it then stands, which the
//! rows show. While the file is read the rows wait; a machine that cannot answer says why.
//!
//! Only the machine's own table is shown for it: a worker's `[worker]`, the server's
//! `[server]`. Rows of this app's own keys on the same page stay this device's.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Task, Window, div, px,
};
use slopty_client::server::ServerCaller;
use slopty_core::WorkerId;
use slopty_proto::orchestration::{Outcome, Verb};
use slopty_proto::settings::{DaemonSettings, SettingEdit};

use super::SettingsForm;
use super::schema::{Row, Section, daemons, rows};
use crate::colors::hsla;
use crate::icons::{IconSize, Symbol};

/// A machine whose daemon's settings the form can edit.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Machine {
    /// A worker, or the server when `None`.
    pub of: Option<WorkerId>,
    /// Its name, as the workspace shows it.
    pub name: String,
}

/// The name the picker gives this device's own file.
pub const THIS_DEVICE: &str = "This Mac";

/// The name the picker gives the server.
pub const SERVER: &str = "Server";

/// What a page says while another machine's file is read.
#[must_use]
pub fn reading_words(name: &str) -> String {
    format!("Reading {name}'s settings\u{2026}")
}

/// What a page says when another machine's file could not be read.
#[must_use]
pub fn failed_words(name: &str, why: &str) -> String {
    format!("{name}'s settings could not be read: {why}")
}

/// What a page says when an edit to another machine's file did not go.
#[must_use]
pub fn unsaved_words(name: &str, why: &str) -> String {
    format!("Not changed on {name}: {why}")
}

/// How another machine's file stands.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum Reading {
    /// Asked, not answered.
    Asking,
    /// Its text is here.
    Ready,
    /// It could not be read, in the server's or the machine's words.
    Failed(String),
}

/// Another machine's settings file, as the form holds it.
#[derive(Debug)]
pub(super) struct Remote {
    machine: Machine,
    /// Its text as last read, with the edits made since written into it.
    text: String,
    reading: Reading,
    /// What in the file the machine could not read.
    problems: Vec<String>,
    /// Edits made since the last went, in order.
    edits: Vec<SettingEdit>,
    /// Why the last edits did not go; the file is read again to show what it holds.
    unsaved: Option<String>,
}

impl Remote {
    /// Whether `table` is this machine's own: `worker…` for a worker, `server…` for the server.
    fn owns(&self, table: &str) -> bool {
        let root = table.split_once('.').map_or(table, |(root, _)| root);
        if self.machine.of.is_some() { root == "worker" } else { root == "server" }
    }
}

/// The machines and the way to them, as the workspace last handed them over.
#[derive(Default)]
pub(super) struct Machines {
    list: Vec<Machine>,
    caller: Option<ServerCaller>,
    /// The picker's menu is open.
    open: bool,
    /// A read or a write on its way; dropping it lets its answer go.
    asking: Option<Task<()>>,
    /// The rows' fields wait to show another file's text, which needs the window.
    stale: bool,
}

impl std::fmt::Debug for Machines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Machines").field("list", &self.list).finish_non_exhaustive()
    }
}

/// Whether this device runs a worker or a server, whose file the form edits as its own.
pub(super) const HOSTS_DAEMONS: bool = !cfg!(target_os = "ios");

impl SettingsForm {
    /// The machines another's settings can be edited on, and the way to ask them. A phone
    /// starts on the server's, its own file holding no such keys.
    pub fn set_machines(
        &mut self,
        list: Vec<Machine>,
        caller: Option<ServerCaller>,
        cx: &mut Context<Self>,
    ) {
        self.machines.list = list;
        self.machines.caller = caller;
        let gone = self
            .remote
            .as_ref()
            .is_some_and(|r| !self.machines.list.iter().any(|m| m.of == r.machine.of));
        if gone {
            self.remote = None;
            self.machines.stale = true;
        }
        if !HOSTS_DAEMONS
            && self.remote.is_none()
            && let Some(server) = self.machines.list.iter().find(|m| m.of.is_none()).cloned()
        {
            self.pick_machine(Some(server), cx);
        }
        cx.notify();
    }

    /// Whether `list`, and whether there is a way to them, differ from what the form holds.
    #[must_use]
    pub fn machines_differ(&self, list: &[Machine], linked: bool) -> bool {
        self.machines.list != list || self.machines.caller.is_some() != linked
    }

    /// The machine whose settings the daemons' rows show: `None` for this device's own file.
    #[must_use]
    pub fn machine(&self) -> Option<&Machine> {
        self.remote.as_ref().map(|r| &r.machine)
    }

    /// The other machine's file a row of `table` reads, when it is one.
    pub(super) fn remote_of(&self, table: &str) -> Option<&Remote> {
        self.remote.as_ref().filter(|_| daemons(table))
    }

    /// The text row `row` reads its value from: another machine's file for its daemon's keys,
    /// else this device's.
    pub(super) fn text_for(&self, row: &Row) -> &str {
        self.remote_of(row.table()).map_or(self.text.as_str(), |r| r.text.as_str())
    }

    /// Whether row `row` is shown for the machine picked: a daemon's key only for that
    /// machine's own table, once its file is read, and on this device only where it runs one.
    pub(super) fn shown_for_machine(&self, row: &Row) -> bool {
        if !daemons(row.table()) {
            return true;
        }
        match &self.remote {
            Some(remote) => remote.owns(row.table()) && remote.reading == Reading::Ready,
            None => HOSTS_DAEMONS,
        }
    }

    /// Write an edit into the text row `row` reads, as `write` makes it, and keep it to send
    /// when that text is another machine's.
    pub(super) fn edit_text(
        &mut self,
        row: &Row,
        entry: Option<&str>,
        literal: Option<&str>,
        write: impl FnOnce(&str) -> String,
    ) {
        match self.remote.as_mut().filter(|_| daemons(row.table())) {
            Some(remote) => {
                remote.text = write(&remote.text);
                remote.edits.push(SettingEdit {
                    table: row.table().to_owned(),
                    key: row.key().to_owned(),
                    entry: entry.map(str::to_owned),
                    literal: literal.map(str::to_owned),
                });
            }
            None => self.text = write(&self.text),
        }
    }

    /// Show `machine`'s settings, or this device's own with `None`: another's file is read
    /// first, and its rows wait for it.
    pub(super) fn pick_machine(&mut self, machine: Option<Machine>, cx: &mut Context<Self>) {
        self.machines.open = false;
        self.send_edits(cx);
        let same = self.remote.as_ref().map(|r| &r.machine) == machine.as_ref();
        if same {
            cx.notify();
            return;
        }
        self.machines.stale = true;
        self.remote = machine.map(|machine| Remote {
            machine,
            text: String::new(),
            reading: Reading::Asking,
            problems: Vec::new(),
            edits: Vec::new(),
            unsaved: None,
        });
        if self.remote.is_some() {
            self.ask_remote(Vec::new(), cx);
        }
        cx.notify();
    }

    /// Send the edits made to another machine's file since the last went.
    pub(super) fn send_edits(&mut self, cx: &Context<Self>) {
        let Some(remote) = self.remote.as_mut() else { return };
        if remote.edits.is_empty() {
            return;
        }
        let edits = std::mem::take(&mut remote.edits);
        self.ask_remote(edits, cx);
    }

    /// Ask the picked machine for its file after `edits`; its answer is what its rows show.
    fn ask_remote(&mut self, edits: Vec<SettingEdit>, cx: &Context<Self>) {
        let Some(remote) = self.remote.as_ref() else { return };
        let of = remote.machine.of;
        let Some(caller) = self.machines.caller.clone() else {
            let why = "this device is not linked to the server".to_owned();
            if let Some(remote) = self.remote.as_mut() {
                remote.reading = Reading::Failed(why);
            }
            return;
        };
        let wrote = !edits.is_empty();
        let verb = Verb::Settings { of, edits };
        self.machines.asking = Some(cx.spawn(async move |this, cx| {
            let outcome = caller.call(verb).await;
            let _gone = this.update(cx, |this, cx| this.remote_answered(of, wrote, outcome, cx));
        }));
    }

    /// The machine `of` answered, after edits when it `wrote`: its file as it stands, or why
    /// not. Edits turned down are said, and the file is read again to show what it holds.
    fn remote_answered(
        &mut self,
        of: Option<WorkerId>,
        wrote: bool,
        outcome: Outcome,
        cx: &mut Context<Self>,
    ) {
        self.machines.asking = None;
        let Some(remote) = self.remote.as_mut().filter(|r| r.machine.of == of) else { return };
        match outcome {
            Outcome::Settings(file) => {
                let DaemonSettings { text, problems, .. } = *file;
                // Edits made while this was on its way are written over what came, and go next.
                let mut text = text;
                for edit in &remote.edits {
                    text = replay(&text, edit);
                }
                remote.text = text;
                remote.problems = problems;
                remote.reading = Reading::Ready;
                if wrote {
                    remote.unsaved = None;
                }
            }
            Outcome::Error { message, .. } if wrote => {
                remote.unsaved = Some(message);
                remote.edits.clear();
                self.ask_remote(Vec::new(), cx);
                return;
            }
            Outcome::Error { message, .. } => remote.reading = Reading::Failed(message),
            _ => remote.reading = Reading::Failed("it answered something else".to_owned()),
        }
        self.machines.stale = true;
        self.send_edits(cx);
        cx.notify();
    }

    /// Show what the picked file holds in the daemons' rows' fields, once it changed.
    pub(super) fn sync_machine_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.machines.stale) {
            return;
        }
        for (ix, row) in rows().iter().enumerate() {
            if !daemons(row.table())
                || matches!(row.field.kind, slopty_settings::schema::Kind::Map(_))
            {
                continue;
            }
            let Some(Some(field)) = self.fields.get(ix).cloned() else { continue };
            let value = super::field_text(self.text_for(row), row);
            if field.read(cx).value().as_ref() != value {
                field.update(cx, |state, cx| state.set_value(value, window, cx));
            }
        }
        self.sync_entries(window, cx);
    }

    /// Whether `section` holds a daemon's keys, and so leads with the machine they are for.
    pub(super) fn picks_machine(section: Section) -> bool {
        rows().iter().any(|r| r.section == section && daemons(r.table()))
    }

    /// Whether there is a machine to pick: this device's own daemons, or another machine.
    pub(super) const fn picks_any(&self) -> bool {
        HOSTS_DAEMONS || !self.machines.list.is_empty()
    }

    /// A group's heading as it reads for the machine picked: "this Mac" becomes its name.
    pub(super) fn group_title(&self, group: &'static str) -> SharedString {
        match &self.remote {
            Some(remote) if group.contains("this Mac") || group.starts_with("This Mac") => {
                let name = &remote.machine.name;
                group
                    .replace("This Mac as a server", name)
                    .replace("this Mac", name)
                    .replace("This Mac", name)
                    .into()
            }
            _ => group.into(),
        }
    }

    /// The page's head where it holds a daemon's keys: whose settings they are, with the way to
    /// pick another machine, and how its file stands.
    pub(super) fn machine_bar(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let (s, spacing) = (theme.surfaces, theme.spacing);
        let name = self.remote.as_ref().map_or(THIS_DEVICE, |r| r.machine.name.as_str()).to_owned();
        let pick = div()
            .id("settings-machine")
            .debug_selector(|| "settings-machine".to_owned())
            .role(gpui::accesskit::Role::ComboBox)
            .aria_label("Settings of")
            .aria_value(SharedString::from(name.clone()))
            .aria_expanded(self.machines.open)
            .flex_none()
            .flex()
            .items_center()
            .gap(px(spacing.xs))
            .px(px(spacing.sm))
            .h(px(theme.density.row))
            .rounded(px(theme.radii.sm))
            .map(|el| crate::kit::field(el, theme))
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text))
            .cursor_pointer()
            .child(SharedString::from(name))
            .child(crate::icons::icon(
                theme,
                Symbol::ChevronDown,
                IconSize::Inline,
                hsla(s.text_muted),
            ))
            .on_click(cx.listener(|this, _ev, _w, cx| {
                this.machines.open = !this.machines.open;
                cx.notify();
            }));
        let menu = self.machines.open.then(|| self.machine_menu(cx));
        let said = self.remote.as_ref().and_then(|remote| {
            let name = &remote.machine.name;
            let (words, tone) = match (&remote.reading, &remote.unsaved) {
                (Reading::Failed(why), _) => (failed_words(name, why), s.error),
                (_, Some(why)) => (unsaved_words(name, why), s.error),
                (Reading::Asking, None) => (reading_words(name), s.text_muted),
                (Reading::Ready, None) => (remote.problems.first()?.clone(), s.error),
            };
            Some(
                crate::kit::meta(div(), theme)
                    .id("settings-machine-said")
                    .debug_selector(|| "settings-machine-said".to_owned())
                    .role(gpui::accesskit::Role::Status)
                    .aria_label(SharedString::from(words.clone()))
                    .text_color(hsla(tone))
                    .child(SharedString::from(words)),
            )
        });
        div()
            .flex()
            .flex_col()
            .gap(px(spacing.xs))
            .pt(px(spacing.sm))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(spacing.sm))
                    .child(crate::kit::meta(div(), theme).flex_none().child("Settings of"))
                    .child(
                        div()
                            .relative()
                            .flex_none()
                            .child(crate::a11y::tab_stop(pick, s.focus))
                            .children(menu),
                    ),
            )
            .children(said)
            .into_any_element()
    }

    /// The picker's menu: this device, where it runs a daemon, then the server and each
    /// worker.
    fn machine_menu(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let this = cx.entity().downgrade();
        let mut menu = crate::kit::Menu::new();
        if HOSTS_DAEMONS {
            let to = this.clone();
            menu.push(crate::kit::MenuItem::new("this", THIS_DEVICE, move |_w, cx| {
                let _gone = to.update(cx, |form, cx| form.pick_machine(None, cx));
            }));
        }
        for (n, machine) in self.machines.list.iter().enumerate() {
            let to = this.clone();
            let picked = machine.clone();
            menu.push(crate::kit::MenuItem::new(
                format!("machine-{n}"),
                machine.name.clone(),
                move |_w, cx| {
                    let picked = picked.clone();
                    let _gone = to.update(cx, |form, cx| form.pick_machine(Some(picked), cx));
                },
            ));
        }
        let panel = crate::kit::MenuPanel::new(
            "settings-machines",
            "Machines",
            std::rc::Rc::new(menu),
            theme,
            {
                move |_window, cx| {
                    let _gone = this.update(cx, |form, cx| {
                        form.machines.open = false;
                        cx.notify();
                    });
                }
            },
        );
        gpui::deferred(gpui::anchored().anchor(gpui::Anchor::TopLeft).child(panel))
            .with_priority(crate::palette::Layer::Submenu.priority())
            .into_any_element()
    }
}

#[cfg(test)]
impl SettingsForm {
    /// What the machine bar says under the picker, when it says something.
    pub(super) fn machine_said(&self) -> Option<String> {
        let remote = self.remote.as_ref()?;
        let name = &remote.machine.name;
        match (&remote.reading, &remote.unsaved) {
            (Reading::Failed(why), _) => Some(failed_words(name, why)),
            (_, Some(why)) => Some(unsaved_words(name, why)),
            (Reading::Asking, None) => Some(reading_words(name)),
            (Reading::Ready, None) => remote.problems.first().cloned(),
        }
    }
}

/// `edit` written into `text`, as the machine will make it.
fn replay(text: &str, edit: &SettingEdit) -> String {
    use slopty_settings::edit as file;
    match (&edit.entry, &edit.literal) {
        (None, Some(literal)) => file::write(text, &edit.table, &edit.key, literal),
        (None, None) => file::remove(text, &edit.table, &edit.key),
        (Some(name), Some(literal)) => {
            file::write_entry(text, &edit.table, &edit.key, name, literal)
        }
        (Some(name), None) => file::remove_entry(text, &edit.table, &edit.key, name),
    }
}
