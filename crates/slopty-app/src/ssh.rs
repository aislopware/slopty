//! Workers put on other machines over SSH from the app.
//!
//! "Install on a machine over SSH" on the add panel, and "Update" on the tiles of a worker on a
//! different build, run [`slopty_deploy`] against the person's own `ssh` (their config, their
//! agent), each step drawn as a line (`slopty_ui::add_worker`). Both bring the machine to this
//! build (an install where there is none, else an update that puts the old worker back if the
//! new one fails), and both register it with the server this app uses, so every client of it
//! lists the machine. The SSH target an install used is kept per worker, so its "Update" reaches
//! it the same way; this Mac's own worker is updated in place, with no `ssh`.
//!
//! Everything that touches a machine goes through a [`Deployer`]: [`native`] on the Mac, a
//! stand-in under test, so no test reaches a host. A run is a GPUI task that owns the deploy;
//! dropping it (Cancel, the panel closed) drops the deploy, which kills its `ssh`.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px,
};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use slopty_core::WorkerId;
pub use slopty_deploy::Target;
use slopty_deploy::{Deployed, Event, Failure, Server, Step};
use slopty_proto::ctl::{Health, Tailscale};
use slopty_ui::add_worker::{self, Bar, Install, Mark, StepLine, Updates};
use slopty_ui::colors::hsla;
use slopty_ui::kit::{self, ButtonKind};
use slopty_ui::workspace::WorkerStatus;
use tokio::sync::mpsc;

use crate::this_mac::Pending;
use crate::{FIELD_H, Workspace, net};

pub mod actions {
    //! The palette's way to the sheet.
    #![expect(
        clippy::derive_partial_eq_without_eq,
        reason = "gpui::actions! derives PartialEq only"
    )]
    use gpui::actions;

    actions!(
        workers,
        [
            /// Put the worker on a machine over SSH and add it.
            InstallOverSsh,
        ]
    );
}

/// Where the app can run `ssh`: the Mac.
pub const OFFERED: bool = cfg!(target_os = "macos");
/// The entry's words and the palette's line.
pub const TITLE: &str = "Install on a machine over SSH";
/// The sheet's heading.
pub const HEADING: &str = "Install over SSH";
/// What the entry's row says under its words.
pub const ROW_META: &str = "Copies the worker there with ssh, then adds it";
/// The sheet's line under its heading.
pub const BLURB: &str = "Slopty copies its worker to a Mac or Linux machine you reach with ssh.";
/// The foot of the form: whose `ssh` it is.
pub const USES: &str = "Uses your ssh config and agent.";

/// What the flows do to other machines.
pub trait Deployer: std::fmt::Debug {
    /// Bring `to` to this build's worker, registered with `server`, each event sent to `events`
    /// as it happens. Dropping what it returns stops it.
    fn deploy(
        &self,
        to: &Target,
        server: Option<Server>,
        events: mpsc::UnboundedSender<Event>,
    ) -> Pending<Result<Deployed, Failure>>;
    /// Add the worker at `address` to this app's workers.
    fn add(&self, address: &str) -> Pending<Result<net::Added, String>>;
    /// Keep `to` as the way to reach `worker`'s machine.
    fn remember(&self, worker: WorkerId, to: &Target);
    /// The way `worker`'s machine was reached, when it was installed from here.
    fn target_of(&self, worker: WorkerId) -> Option<Target>;
}

/// Where a run is.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Stage {
    /// `ssh` there, `uname`.
    Reach,
    /// The binaries going up.
    Copy,
    /// The install there.
    Install,
    /// The new worker's doctor.
    Check,
    /// Adding it, or dialling it again after an update.
    Join,
}

/// Every stage, in order.
const STAGES: [Stage; 5] = [Stage::Reach, Stage::Copy, Stage::Install, Stage::Check, Stage::Join];

impl Stage {
    const fn slug(self) -> &'static str {
        match self {
            Self::Reach => "reach",
            Self::Copy => "copy",
            Self::Install => "install",
            Self::Check => "check",
            Self::Join => "join",
        }
    }

    fn title(self, host: &str, update: bool) -> String {
        match (self, update) {
            (Self::Reach, _) => format!("Connect to {host}"),
            (Self::Copy, _) => "Copy the worker".to_owned(),
            (Self::Install, false) => "Install and start it".to_owned(),
            (Self::Install, true) => "Install the new build".to_owned(),
            (Self::Check, _) => "Check that it answers".to_owned(),
            (Self::Join, false) => "Add it to Slopty".to_owned(),
            (Self::Join, true) => "Reconnect".to_owned(),
        }
    }
}

/// A run's progress: where it is and what it said, drawn as [`Install`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Progress {
    host: String,
    update: bool,
    stage: Stage,
    /// Bytes of the binaries sent, of all of them.
    sent: Option<(u64, u64)>,
    /// The install's last line with something on it.
    line: Option<String>,
    /// What the machine runs, once it said.
    machine: Option<String>,
    failed: Option<Failure>,
}

impl Progress {
    /// A run against `host` that has not begun.
    #[must_use]
    pub const fn new(host: String, update: bool) -> Self {
        Self {
            host,
            update,
            stage: Stage::Reach,
            sent: None,
            line: None,
            machine: None,
            failed: None,
        }
    }

    /// Where it is.
    #[must_use]
    pub const fn stage(&self) -> Stage {
        self.stage
    }

    /// Why it stopped, once it has.
    #[must_use]
    pub const fn failure(&self) -> Option<&Failure> {
        self.failed.as_ref()
    }

    /// Take in what the deploy said.
    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Step(Step::Reach) => self.stage = Stage::Reach,
            Event::Machine(platform) => self.machine = Some(platform.to_string()),
            Event::Step(Step::Upload { .. }) => self.stage = Stage::Copy,
            Event::Sent { sent, total } => self.sent = Some((sent, total)),
            Event::Step(Step::Install) => self.stage = Stage::Install,
            Event::Line(line) => {
                let line = line.trim();
                if !line.is_empty() {
                    self.line = Some(line.to_owned());
                }
            }
            Event::Step(Step::Check) => self.stage = Stage::Check,
        }
    }

    /// The deploy is done: on to adding it, or dialling it again.
    pub fn deployed(&mut self, deployed: &Deployed) {
        self.machine = Some(deployed.platform.to_string());
        self.stage = Stage::Join;
    }

    /// It stopped at the stage it was at.
    pub fn failed(&mut self, failure: Failure) {
        self.failed = Some(failure);
    }

    /// It is going on.
    #[must_use]
    pub const fn running(&self) -> bool {
        self.failed.is_none()
    }

    /// As drawn: a line per stage, the bar under the one that runs, and why it stopped.
    #[must_use]
    pub fn view(&self) -> Install {
        let steps = STAGES
            .into_iter()
            .map(|stage| {
                let mark = match stage.cmp(&self.stage) {
                    std::cmp::Ordering::Less => Mark::Done,
                    std::cmp::Ordering::Equal if self.failed.is_some() => Mark::Failed,
                    std::cmp::Ordering::Equal => Mark::Running,
                    std::cmp::Ordering::Greater => Mark::Waiting,
                };
                StepLine {
                    slug: stage.slug(),
                    title: stage.title(&self.host, self.update),
                    detail: self.detail(stage, mark),
                    mark,
                }
            })
            .collect();
        let bar = match (self.failed.is_some(), self.stage, self.sent) {
            (true, ..) => Bar::Hidden,
            (false, Stage::Copy, Some((sent, total))) if total > 0 => {
                #[expect(clippy::cast_precision_loss, reason = "a share for a bar")]
                let share = sent as f32 / total as f32;
                Bar::Share(share)
            }
            (false, ..) => Bar::Busy,
        };
        let failed = self.failed.as_ref().map(|f| add_worker::Failed {
            title: f.title.clone(),
            hint: f.hint.clone(),
            lines: f.lines.clone(),
        });
        Install { steps, bar, failed }
    }

    /// What `stage`'s line says beside its title.
    fn detail(&self, stage: Stage, mark: Mark) -> Option<String> {
        match (stage, mark) {
            (Stage::Reach, Mark::Done) => self.machine.clone(),
            (Stage::Copy, Mark::Running) => self.sent.map(|(sent, total)| {
                format!("{} of {}", kit::size_label(sent), kit::size_label(total))
            }),
            (Stage::Copy, Mark::Done) => self.sent.map(|(_, total)| kit::size_label(total)),
            (Stage::Install, Mark::Running | Mark::Failed) => self.line.clone(),
            _ => None,
        }
    }
}

/// Where to reach the worker just deployed, best first: its tailnet name and IP, then the host
/// `ssh` reached, each with its port when it is not the default.
#[must_use]
pub fn addresses(target: &Target, health: &Health) -> Vec<String> {
    let port = health
        .listen
        .parse::<std::net::SocketAddr>()
        .ok()
        .map(|a| a.port())
        .filter(|p| *p != slopty_net::endpoint::WORKER_PORT);
    let at = |host: &str| match port {
        Some(port) => slopty_net::HostAddr::new(host, port).to_string(),
        None => host.to_owned(),
    };
    let mut hosts = Vec::new();
    if let Tailscale::Up { node, ip } = &health.tailscale {
        let node = node.trim_end_matches('.');
        if !node.is_empty() {
            hosts.push(at(node));
        }
        hosts.extend(ip.map(|ip| at(&ip.to_string())));
    }
    hosts.push(at(&target.host));
    let mut seen = std::collections::HashSet::new();
    hosts.retain(|h| seen.insert(h.clone()));
    hosts
}

/// The sheet: its fields, and the run under way or the one that failed.
#[derive(Debug)]
pub struct Sheet {
    host: Entity<InputState>,
    user: Entity<InputState>,
    port: Entity<InputState>,
    /// Why the fields do not read, or that a run was stopped.
    note: Option<(String, bool)>,
    run: Option<Run>,
    /// Return in a field installs; they go with the sheet.
    _enter: [gpui::Subscription; 3],
}

impl Sheet {
    /// Whether an input method is composing in one of the fields: its keys are its own.
    pub fn composing(&self, cx: &gpui::App) -> bool {
        [&self.host, &self.user, &self.port].into_iter().any(|f| f.read(cx).is_composing())
    }
}

/// A run from the sheet, numbered so an answer for one left behind is dropped.
#[derive(Debug)]
struct Run {
    id: u64,
    progress: Progress,
    /// Owns the deploy; `None` once it ended.
    task: Option<gpui::Task<()>>,
}

impl Sheet {
    /// The run's progress, for tests and the panel.
    #[must_use]
    pub fn progress(&self) -> Option<&Progress> {
        self.run.as_ref().map(|r| &r.progress)
    }

    /// A run is under way.
    fn running(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.task.is_some())
    }
}

/// An update from a tile, by the host it runs against.
#[derive(Debug)]
pub struct UpdateRun {
    worker: WorkerId,
    progress: Progress,
    task: Option<gpui::Task<()>>,
}

/// Every update under way or failed.
pub type Updating = HashMap<String, UpdateRun>;

impl Workspace {
    /// Open the sheet on the panel (the panel too, if it is not up), keeping what was typed.
    pub(crate) fn open_ssh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.deployer.is_none() {
            return;
        }
        if self.adding.is_none() {
            self.show_add_worker(crate::Panel::Worker, window, cx);
        }
        let Some(adding) = &mut self.adding else { return };
        adding.this_mac = None;
        if adding.ssh.is_none() {
            let field = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
                let enter = cx.subscribe(&input, |this: &mut Self, _input, event, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.install_over_ssh(cx);
                    }
                });
                (input, enter)
            };
            let (host, a) = field("mac-mini or 100.64.0.7", window, cx);
            let (user, b) = field("From your ssh config", window, cx);
            let (port, c) = field("22", window, cx);
            host.update(cx, |input, cx| input.focus(window, cx));
            if let Some(adding) = &mut self.adding {
                let enter = [a, b, c];
                adding.ssh = Some(Sheet { host, user, port, note: None, run: None, _enter: enter });
            }
        }
        cx.notify();
    }

    /// The sheet with `host` in its host field, for a machine the person already named: a
    /// worker there that runs another build is brought to this one by installing over it.
    pub fn open_ssh_at(&mut self, host: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open_ssh(window, cx);
        let Some(sheet) = self.adding.as_ref().and_then(|a| a.ssh.as_ref()) else { return };
        let field = sheet.host.clone();
        let host = host.to_owned();
        field.update(cx, |input, cx| input.set_value(host, window, cx));
        cx.notify();
    }

    /// Back from the sheet to the panel; a run under way stops.
    pub(crate) fn leave_ssh(&mut self, cx: &mut Context<Self>) {
        if let Some(adding) = &mut self.adding {
            adding.ssh = None;
        }
        cx.notify();
    }

    /// Stop the run under way; the fields stay as typed.
    fn cancel_ssh(&mut self, cx: &mut Context<Self>) {
        let Some(sheet) = self.adding.as_mut().and_then(|a| a.ssh.as_mut()) else { return };
        if sheet.running() {
            sheet.run = None;
            sheet.note = Some(("Stopped. Install again to start over.".to_owned(), false));
        }
        cx.notify();
    }

    /// The sheet's run `id`, if it is still the one on screen.
    fn ssh_run(&mut self, id: u64) -> Option<&mut Run> {
        self.adding.as_mut()?.ssh.as_mut()?.run.as_mut().filter(|r| r.id == id)
    }

    /// Install on the machine the fields name: deploy, then add it.
    pub(crate) fn install_over_ssh(&mut self, cx: &mut Context<Self>) {
        let Some(deployer) = self.deployer.clone() else { return };
        let Some(sheet) = self.adding.as_mut().and_then(|a| a.ssh.as_mut()) else { return };
        if sheet.running() {
            return;
        }
        let read = |input: &Entity<InputState>| input.read(cx).value().to_string();
        let target = match Target::read(&read(&sheet.host), &read(&sheet.user), &read(&sheet.port))
        {
            Ok(target) => target,
            Err(why) => {
                sheet.note = Some((why, true));
                cx.notify();
                return;
            }
        };
        sheet.note = None;
        self.ssh_runs = self.ssh_runs.wrapping_add(1);
        let id = self.ssh_runs;
        let (tx, events) = mpsc::unbounded_channel();
        let deploy = deployer.deploy(&target, self.register_with(), tx);
        let progress = Progress::new(target.host.clone(), false);
        let task = cx.spawn(async move |this, cx| {
            let apply = |ws: &mut Self, event: Event, cx: &mut Context<Self>| {
                if let Some(run) = ws.ssh_run(id) {
                    run.progress.apply(event);
                    cx.notify();
                }
            };
            let Some(done) = drive(deploy, events, &this, cx, apply).await else { return };
            let deployed = match done {
                Ok(deployed) => deployed,
                Err(failure) => {
                    let _gone = this.update(cx, |ws, cx| ws.ssh_ended(id, Some(failure), cx));
                    return;
                }
            };
            let joined = this.update(cx, |ws, cx| {
                let run = ws.ssh_run(id)?;
                run.progress.deployed(&deployed);
                cx.notify();
                Some(())
            });
            if !matches!(joined, Ok(Some(()))) {
                return;
            }
            let mut why = String::new();
            for address in addresses(&target, &deployed.health) {
                match deployer.add(&address).await {
                    Ok(added) => {
                        deployer.remember(added.id, &target);
                        let _gone =
                            this.update(cx, |ws, cx| ws.ssh_added(id, added, &deployed, cx));
                        return;
                    }
                    Err(e) => why = e,
                }
            }
            let failure = Failure {
                title: format!("The worker runs on {}, and Slopty could not add it", target.host),
                hint: Some("Add it by an address this Mac reaches.".to_owned()),
                lines: vec![why],
            };
            let _gone = this.update(cx, |ws, cx| ws.ssh_ended(id, Some(failure), cx));
        });
        if let Some(sheet) = self.adding.as_mut().and_then(|a| a.ssh.as_mut()) {
            sheet.run = Some(Run { id, progress, task: Some(task) });
        }
        cx.notify();
    }

    /// The sheet's run `id` ended with `failure`.
    fn ssh_ended(&mut self, id: u64, failure: Option<Failure>, cx: &mut Context<Self>) {
        let Some(run) = self.ssh_run(id) else { return };
        run.task = None;
        if let Some(failure) = failure {
            run.progress.failed(failure);
        }
        cx.notify();
    }

    /// The machine is a worker of this app now: the panel gives way to it.
    fn ssh_added(
        &mut self,
        id: u64,
        added: net::Added,
        deployed: &Deployed,
        cx: &mut Context<Self>,
    ) {
        if self.ssh_run(id).is_none() {
            return;
        }
        let net::Added { id: worker, name } = added;
        self.adding = None;
        let caps = &deployed.health.caps;
        let mac = deployed.platform.os == slopty_deploy::Os::MacOs;
        self.show_notice(
            if mac && !(caps.can_capture && caps.can_inject) {
                format!("Added {name}. Its screen waits for someone there to allow Slopty.")
            } else {
                format!("Added {name}")
            },
            cx,
        );
        self.add_worker(worker, name, true, cx);
        cx.notify();
    }

    /// The server a worker installed from here registers with: the one this app is linked to.
    fn register_with(&self) -> Option<Server> {
        self.server_address().map(|a| Server { host: a.host().to_owned(), port: a.port() })
    }

    /// Replace the worker at `host` (a tile's "Update"): deploy with `--update`, through the SSH
    /// target it was installed with or else the host it was dialled at, then dial it again at
    /// once. The pill follows each step; the link coming up ends it.
    pub(crate) fn update_worker(&mut self, host: &str, cx: &mut Context<Self>) {
        let Some(deployer) = self.deployer.clone() else { return };
        if self.updates.get(host).is_some_and(|run| run.task.is_some()) {
            return;
        }
        let view = self.view.read(cx);
        let key = view.workers().find_map(|(key, _, status)| match status {
            WorkerStatus::NeedsUpdate(notice) if notice.host == host => Some(key),
            _ => None,
        });
        let Some(worker) = key.and_then(|key| self.workers.iter().find(|w| w.key == key)) else {
            return;
        };
        let worker = worker.id;
        let target = deployer.target_of(worker).unwrap_or_else(|| Target::host(host));
        let (tx, events) = mpsc::unbounded_channel();
        let deploy = deployer.deploy(&target, self.register_with(), tx);
        let owned = host.to_owned();
        let task = cx.spawn(async move |this, cx| {
            let host = owned.clone();
            let apply = move |ws: &mut Self, event: Event, cx: &mut Context<Self>| {
                if let Some(run) = ws.updates.get_mut(&host) {
                    run.progress.apply(event);
                    ws.publish_updates(cx);
                }
            };
            let Some(done) = drive(deploy, events, &this, cx, apply).await else { return };
            let _gone = this.update(cx, |ws, cx| ws.update_deployed(&owned, done, cx));
        });
        let progress = Progress::new(host.to_owned(), true);
        self.updates.insert(host.to_owned(), UpdateRun { worker, progress, task: Some(task) });
        self.publish_updates(cx);
    }

    /// An update's deploy ended: dial the worker again at once, or say why it stopped.
    fn update_deployed(
        &mut self,
        host: &str,
        done: Result<Deployed, Failure>,
        cx: &mut Context<Self>,
    ) {
        let Some(run) = self.updates.get_mut(host) else { return };
        run.task = None;
        match done {
            Ok(deployed) => {
                run.progress.deployed(&deployed);
                let worker = run.worker;
                self.connect_now(worker);
            }
            Err(failure) => run.progress.failed(failure),
        }
        self.publish_updates(cx);
    }

    /// `worker` linked: an update that brought it back is done.
    pub(crate) fn update_linked(&mut self, worker: WorkerId, cx: &mut Context<Self>) {
        let before = self.updates.len();
        self.updates.retain(|_, run| run.worker != worker || run.task.is_some());
        if self.updates.len() != before {
            self.publish_updates(cx);
        }
    }

    /// A dial to `worker` after its update found another build still: the update says so.
    pub(crate) fn update_still_wrong(&mut self, worker: WorkerId, cx: &mut Context<Self>) {
        let joining = self
            .updates
            .values_mut()
            .find(|run| run.worker == worker && run.task.is_none() && run.progress.running());
        let Some(run) = joining else { return };
        run.progress.failed(Failure {
            title: "It still runs a different build".to_owned(),
            hint: Some("The machine kept its old worker; see its install log there.".to_owned()),
            lines: Vec::new(),
        });
        self.publish_updates(cx);
    }

    /// Tell the tiles what an update can do and where each stands.
    pub(crate) fn publish_updates(&self, cx: &mut Context<Self>) {
        let start = self.deployer.as_ref().map(|_| {
            let this = cx.weak_entity();
            let run: add_worker::Update = Rc::new(move |host, _window, cx| {
                let _gone = this.update(cx, |ws, cx| ws.update_worker(host, cx));
            });
            run
        });
        let runs = self.updates.iter().map(|(h, r)| (h.clone(), r.progress.view())).collect();
        cx.set_global(Updates { start, runs });
        self.view.update(cx, |_, cx| cx.notify());
    }

    /// The sheet in the panel's place: the form, or the steps of the run under way.
    pub(crate) fn ssh_sheet(&self, sheet: &Sheet, cx: &Context<Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let (s, spacing) = (theme.surfaces, theme.spacing);
        if sheet.running() {
            let install = sheet.progress().map(Progress::view);
            let steps = install.as_ref().map(|i| add_worker::steps(theme, i));
            let bar = install.and_then(|i| add_worker::bar(theme, i.bar, "install-bar", cx));
            let stop = kit::button(theme, "ssh-cancel", "Cancel", ButtonKind::Ghost)
                .on_click(cx.listener(|this, _ev, _window, cx| this.cancel_ssh(cx)));
            return div()
                .flex()
                .flex_col()
                .gap(px(spacing.md))
                .children(steps)
                .children(bar)
                .child(div().flex().child(div().flex_1()).child(stop))
                .into_any_element();
        }
        let failed = sheet.progress().and_then(|p| p.view().failed);
        let well =
            |id: &'static str, label: (&'static str, &'static str), input: &Entity<InputState>| {
                let (label_id, label) = label;
                div()
                    .flex()
                    .flex_col()
                    .gap(px(spacing.xs))
                    .child(crate::panel_label(theme, label_id, label))
                    .child(
                        div()
                            .id(id)
                            .debug_selector(move || id.to_owned())
                            .h(px(FIELD_H))
                            .flex()
                            .items_center()
                            .pl(px(spacing.xs))
                            .rounded(px(theme.radii.sm))
                            .bg(hsla(s.raised))
                            .text_size(px(theme.typography.ui_size))
                            .child(Input::new(input).appearance(false).aria_label(label)),
                    )
            };
        let go = if failed.is_some() { "Try again" } else { "Install" };
        let install = kit::button(theme, "ssh-install", go, ButtonKind::Primary)
            .h(px(FIELD_H))
            .on_click(cx.listener(|this, _ev, _window, cx| this.install_over_ssh(cx)));
        let note = sheet.note.as_ref().map(|(text, wrong)| {
            kit::meta(div(), theme)
                .id("ssh-note")
                .debug_selector(|| "ssh-note".to_owned())
                .role(Role::Status)
                .aria_label(SharedString::from(text.clone()))
                .when(*wrong, |el| el.text_color(hsla(s.error)))
                .child(SharedString::from(text.clone()))
        });
        div()
            .id("ssh-form")
            .debug_selector(|| "ssh-form".to_owned())
            .flex()
            .flex_col()
            .gap(px(spacing.md))
            .child(well("ssh-host", ("ssh-host-label", "Host"), &sheet.host))
            .child(
                div()
                    .flex()
                    .gap(px(spacing.sm))
                    .child(div().flex_1().min_w_0().child(well(
                        "ssh-user",
                        ("ssh-user-label", "User"),
                        &sheet.user,
                    )))
                    .child(div().w_1_4().child(well(
                        "ssh-port",
                        ("ssh-port-label", "Port"),
                        &sheet.port,
                    ))),
            )
            .children(failed.map(|f| add_worker::failure(theme, &f)))
            .children(note)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(spacing.sm))
                    .child(kit::meta(div(), theme).flex_1().child(USES))
                    .child(install),
            )
            .into_any_element()
    }

    /// The panel's entry to the sheet, a row to press as this Mac's is; none where it is not
    /// offered.
    pub(crate) fn ssh_row(&self, cx: &Context<Self>) -> Option<gpui::Stateful<gpui::Div>> {
        self.deployer.as_ref()?;
        let glyph = slopty_ui::icons::IconName::Terminal;
        let row = crate::entry_row(&self.theme, "install-over-ssh", glyph, TITLE, ROW_META)
            .on_click(cx.listener(|this, _ev, window, cx| this.open_ssh(window, cx)));
        Some(row)
    }
}

/// Await `deploy`, handing each event to `apply` on the workspace as it comes; what it ended
/// with, or `None` when the workspace is gone.
async fn drive(
    mut deploy: Pending<Result<Deployed, Failure>>,
    mut events: mpsc::UnboundedReceiver<Event>,
    this: &gpui::WeakEntity<Workspace>,
    cx: &mut gpui::AsyncApp,
    apply: impl Fn(&mut Workspace, Event, &mut Context<Workspace>),
) -> Option<Result<Deployed, Failure>> {
    let done = loop {
        tokio::select! {
            biased;
            Some(event) = events.recv() => {
                this.update(cx, |ws, cx| apply(ws, event, cx)).ok()?;
            }
            done = &mut deploy => break done,
        }
    };
    while let Ok(event) = events.try_recv() {
        this.update(cx, |ws, cx| apply(ws, event, cx)).ok()?;
    }
    Some(done)
}

/// The deployer on this Mac: the system `ssh` from the networking runtime, the binaries from
/// beside the app; `None` in the self-test, which reaches no machine.
#[cfg(target_os = "macos")]
pub fn native(runtime: &tokio::runtime::Handle) -> Option<Rc<dyn Deployer>> {
    if crate::self_test() {
        return None;
    }
    let data = slopty_platform::dirs::data_dir();
    Some(Rc::new(mac::Native { runtime: runtime.clone(), data }))
}

/// No `ssh` to run here.
#[cfg(not(target_os = "macos"))]
pub const fn native(_runtime: &tokio::runtime::Handle) -> Option<Rc<dyn Deployer>> {
    None
}

#[cfg(target_os = "macos")]
mod mac {
    //! Deploys from this Mac.

    use std::path::PathBuf;

    use slopty_core::WorkerId;
    use slopty_deploy::{Deployed, Event, Failure, Local, Plan, Remembered, Runner, Server, Ssh};
    use tokio::sync::mpsc;

    use super::{Deployer, Target};
    use crate::net;
    use crate::this_mac::Pending;

    /// This Mac's networking runtime, where the deploys run, and the data directory the SSH
    /// targets are kept in.
    #[derive(Debug)]
    pub(super) struct Native {
        pub runtime: tokio::runtime::Handle,
        pub data: PathBuf,
    }

    /// A runtime task stopped when its handle is dropped: dropping the deploy kills its `ssh`.
    struct Aborting<T>(tokio::task::JoinHandle<T>);

    impl<T> Drop for Aborting<T> {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    impl<T: Send + 'static> Aborting<T> {
        fn pending(self, died: T) -> Pending<T> {
            let mut this = self;
            Box::pin(async move { (&mut this.0).await.unwrap_or(died) })
        }
    }

    impl Deployer for Native {
        fn deploy(
            &self,
            to: &Target,
            server: Option<Server>,
            events: mpsc::UnboundedSender<Event>,
        ) -> Pending<Result<Deployed, Failure>> {
            let to = to.clone();
            let task = self.runtime.spawn(async move {
                let source = slopty_platform::service::sibling_dir().map_err(|e| Failure {
                    title: "Could not find the worker inside Slopty".to_owned(),
                    hint: None,
                    lines: vec![e.to_string()],
                })?;
                // This Mac's own worker needs no Remote Login: the plan runs here, in place.
                let runner: Box<dyn Runner> = if to.is_this_machine().await {
                    Box::new(Local::here())
                } else {
                    Box::new(Ssh::unattended(&to))
                };
                let mut on = |event| {
                    let _gone = events.send(event);
                };
                let plan = Plan { source, update: true, server };
                slopty_deploy::deploy(runner.as_ref(), &plan, &mut on).await.map_err(|e| {
                    tracing::warn!(host = %to.host, error = %e, "deploy");
                    e.failure()
                })
            });
            let died = Err(Failure {
                title: "The install stopped".to_owned(),
                hint: None,
                lines: Vec::new(),
            });
            Aborting(task).pending(died)
        }

        fn add(&self, address: &str) -> Pending<Result<net::Added, String>> {
            let address = address.to_owned();
            let task = self.runtime.spawn(async move {
                net::add_worker(&address).await.map_err(|e| format!("{e:#}"))
            });
            Aborting(task).pending(Err("adding it stopped".to_owned()))
        }

        fn remember(&self, worker: WorkerId, to: &Target) {
            let mut kept = Remembered::open_in(&self.data);
            if let Err(e) = kept.remember(&worker.to_string(), to.clone()) {
                tracing::warn!(%worker, error = %e, "keep the worker's ssh target");
            }
        }

        fn target_of(&self, worker: WorkerId) -> Option<Target> {
            Remembered::open_in(&self.data).get(&worker.to_string()).cloned()
        }
    }
}

#[cfg(test)]
mod tests;
