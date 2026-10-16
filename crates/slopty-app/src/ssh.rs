//! Workers put on other machines over SSH from the app.
//!
//! The Add device dialog's install over SSH, and "Update" on the Devices row of a machine on a
//! different build, run [`slopty_deploy`] against the person's own `ssh` (their config, their
//! agent), each step a line of the dialog's progress ([`Install`]). Both bring the machine to this
//! build (an install where there is none, else an update that puts the old worker back if the
//! new one fails), and both register it with the server this app uses, so every client of it
//! lists the machine: an install is done when the server's directory lists it. The SSH target
//! an install used is kept per worker, so its "Update" reaches it the same way; this Mac's own
//! worker is updated in place, with no `ssh`.
//!
//! Everything that touches a machine goes through a [`Deployer`]: [`native`] on the Mac, a
//! stand-in under test, so no test reaches a host. A run is a GPUI task that owns the deploy;
//! dropping it (Cancel, the panel closed) drops the deploy, which kills its `ssh`.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::Context;
use slopty_core::WorkerId;
pub use slopty_deploy::Target;
use slopty_deploy::{Deployed, Event, Failure, HostKey, Served, Server, Step};
use slopty_kit::kit;
use slopty_net::HostAddr;
use slopty_net::server::ServerLink;
use tokio::sync::mpsc;

use crate::Workspace;
use crate::this_mac::{self, Pending};

/// Where a host key is checked and trusted, or a password typed: the dialog over SSH.
const ADD_DEVICE: &str = "Settings \u{2192} Devices \u{2192} Add device";
/// How a run on this Mac names it.
const THIS_MAC: &str = "this Mac";
/// The cluster's word while the server is being brought to this build.
pub const UPDATING_SERVER: &str = "Updating the server\u{2026}";

/// What the person said to one deploy, after a run stopped to ask.
#[derive(Clone, Default, Debug)]
pub struct Said {
    /// Go on when the update restarts `slopty-ptyd` there, ending its sessions: the run before
    /// stopped on [`Failure::ends_sessions`] and the person pressed again.
    pub end_sessions: bool,
    /// The password the machine asked for ([`Failure::password`]), typed into the sheet for
    /// this one run: handed to the system `ssh` through its askpass, never kept.
    pub password: Option<slopty_deploy::SecretString>,
    /// Add the person's public key there, so the machine stops asking.
    pub add_key: bool,
}

/// What the toast says once `name` is added: then what may keep it from running for good, and
/// what became of the person's key.
pub fn added_notice(name: &str, deployed: &Deployed) -> String {
    let caps = &deployed.health.caps;
    let mut said = vec![format!("Added {name}")];
    if deployed.platform.os == slopty_deploy::Os::MacOs && !(caps.can_capture && caps.can_inject) {
        said.push("Its screen waits for someone there to allow Slopty".to_owned());
    }
    if deployed.console.logged_in == Some(false) {
        said.push(format!("{name} runs Slopty once someone is logged in there"));
    }
    if deployed.console.filevault == Some(true) {
        said.push(
            "After a restart it waits for its disk password before anyone can log in".to_owned(),
        );
    }
    if let Some(how) = &deployed.stops_at_logout {
        said.push(format!("Its shells stop when you log out there: {}", how.trim_end_matches('.')));
    }
    match &deployed.key {
        Some(slopty_deploy::Key::Added) => {
            said.push("Your key is on it now, so it will not ask for a password again".to_owned());
        }
        Some(slopty_deploy::Key::NoPublicKey) => said.push(NO_KEY.to_owned()),
        Some(slopty_deploy::Key::Failed(why)) => {
            said.push(format!("Your key was not added: {why}"));
        }
        Some(slopty_deploy::Key::AlreadyThere) | None => {}
    }
    match said.as_slice() {
        [only] => only.clone(),
        all => all.iter().map(|s| format!("{s}.")).collect::<Vec<_>>().join(" "),
    }
}

/// What the sheet says when the machine took a password and this Mac has no key to give it.
pub const NO_KEY: &str =
    "Make a key with ssh-keygen, or use Tailscale SSH, so updates do not ask again";

/// What the flows do to other machines.
pub trait Deployer: std::fmt::Debug {
    /// Bring `to` to this build's worker, registered with `server`, as the person `said`, each
    /// event sent to `events` as it happens. Dropping what it returns stops it.
    fn deploy(
        &self,
        to: &Target,
        server: Server,
        said: Said,
        events: mpsc::UnboundedSender<Event>,
    ) -> Pending<Result<Deployed, Failure>>;
    /// Keep `to` as the way to reach `worker`'s machine.
    fn remember(&self, worker: WorkerId, to: &Target);
    /// The way `worker`'s machine was reached, when it was installed from here.
    fn target_of(&self, worker: WorkerId) -> Option<Target>;
    /// Put this build's server on `to` and start it, each event sent to `events` as it
    /// happens. Dropping what it returns stops it. A deployer that sets up no server says so.
    fn serve(
        &self,
        _to: &Target,
        _said: Said,
        _events: mpsc::UnboundedSender<Event>,
    ) -> Pending<Result<Served, Failure>> {
        let none = Failure::new("This build sets up no server".to_owned(), None, Vec::new());
        Box::pin(std::future::ready(Err(none)))
    }
    /// Take the worker and everything else of Slopty's off `to` (`slopty worker uninstall
    /// --purge` there), each event sent to `events` as it happens; on this Mac, Slopty no
    /// longer opens at login after. Dropping what it returns stops it. A deployer that removes
    /// nothing says so.
    fn remove(
        &self,
        _to: &Target,
        _events: mpsc::UnboundedSender<Event>,
    ) -> Pending<Result<slopty_deploy::Removed, Failure>> {
        let none = Failure::new("This build removes no machine".to_owned(), None, Vec::new());
        Box::pin(std::future::ready(Err(none)))
    }
    /// Trust `key` for its machine from now on, as the person said after checking it.
    fn trust(&self, key: &HostKey) -> Pending<Result<(), Failure>> {
        let none =
            Failure::new(format!("This build trusts no key for {}", key.target), None, Vec::new());
        Box::pin(std::future::ready(Err(none)))
    }
    /// Link to the server at `address`, as the panel's Connect does.
    fn link_server(&self, address: &HostAddr) -> Pending<Result<ServerLink, String>> {
        Box::pin(std::future::ready(Err(format!("this build links to no server at {address}"))))
    }
    /// Keep `to` as the way to reach the machine of the server at `address`.
    fn remember_server(&self, _address: &HostAddr, _to: &Target) {}
    /// The way the machine of the server at `address` was reached, when it was set up from
    /// here.
    fn server_target(&self, _address: &HostAddr) -> Option<Target> {
        None
    }
    /// Whether this app is an installed build, whose daemons on this Mac it may bring to its
    /// own build unasked; a build run from a source tree never does.
    fn installed(&self) -> bool {
        false
    }
}

/// What a run puts on the machine, and how its steps are named.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// A worker where there may be none: install it, then wait for the server to list it.
    Install,
    /// A worker on another build: replace it, then dial it again.
    Update,
    /// The server: install it, then connect to it.
    Server,
}

/// What a sheet sets up: the add panel's worker, or the server panel's server.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sets {
    /// A worker, done once the server lists it.
    Worker,
    /// The server, connected to once it answers.
    Server,
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
    /// The server listing it, connecting to the server, or dialling it again after an update.
    Join,
}

/// Every stage, in order.
const STAGES: [Stage; 5] = [Stage::Reach, Stage::Copy, Stage::Install, Stage::Check, Stage::Join];

impl Stage {
    fn title(self, host: &str, kind: Kind) -> String {
        match (self, kind) {
            (Self::Reach, _) if host == THIS_MAC => "Look at this Mac".to_owned(),
            (Self::Reach, _) => format!("Connect to {host}"),
            (Self::Copy, Kind::Server) => "Copy the server".to_owned(),
            (Self::Copy, _) => "Copy Slopty".to_owned(),
            (Self::Install, Kind::Install | Kind::Server) => "Install and start it".to_owned(),
            (Self::Install, Kind::Update) => "Install the new build".to_owned(),
            (Self::Check, _) => "Check that it answers".to_owned(),
            (Self::Join, Kind::Install) => "Wait for the server to list it".to_owned(),
            (Self::Join, Kind::Update) => "Reconnect".to_owned(),
            (Self::Join, Kind::Server) => "Connect to it".to_owned(),
        }
    }
}

/// How a step stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    /// Not reached yet.
    Waiting,
    /// Under way.
    Running,
    /// Done.
    Done,
    /// Where it stopped.
    Failed,
}

/// One step's line.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StepLine {
    /// What it does, sentence case.
    pub title: String,
    /// What it is at: the bytes sent, the line the machine printed last, what it found.
    pub detail: Option<String>,
    /// How it stands.
    pub mark: Mark,
}

/// The bar under the steps.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Bar {
    /// Nothing runs.
    Hidden,
    /// This share of the step is done, from 0 to 1.
    Share(f32),
    /// A step whose length nobody can tell.
    Busy,
}

/// An install or an update, as the dialog draws it: every step in order and the bar under them.
#[derive(Clone, PartialEq, Debug)]
pub struct Install {
    /// Every step, in order.
    pub steps: Vec<StepLine>,
    /// The bar under them.
    pub bar: Bar,
}

/// A run's progress: where it is and what it said, drawn as [`Install`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Progress {
    host: String,
    kind: Kind,
    stage: Stage,
    /// Bytes of the binaries sent, of all of them.
    sent: Option<(u64, u64)>,
    /// The install's last line with something on it.
    line: Option<String>,
    /// What the machine runs, once it said.
    machine: Option<String>,
    /// What the install does to the worker's shell keeper, once the machine said.
    ptyd: Option<slopty_deploy::Ptyd>,
    failed: Option<Failure>,
}

impl Progress {
    /// A run against `host` that has not begun.
    #[must_use]
    pub const fn new(host: String, kind: Kind) -> Self {
        Self {
            host,
            kind,
            stage: Stage::Reach,
            sent: None,
            line: None,
            machine: None,
            ptyd: None,
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
            Event::Step(Step::Install | Step::Remove) => self.stage = Stage::Install,
            Event::Line(line) => {
                let line = line.trim();
                if !line.is_empty() {
                    self.line = Some(line.to_owned());
                }
            }
            Event::Step(Step::Check) => self.stage = Stage::Check,
            Event::Ptyd(ptyd) => self.ptyd = Some(ptyd),
        }
    }

    /// The deploy is done: on to adding it, dialling it again, or connecting to the server.
    pub fn deployed(&mut self, platform: slopty_deploy::Platform) {
        self.machine = Some(platform.to_string());
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

    /// As drawn: a line per stage and the bar under the one that runs.
    #[must_use]
    pub fn view(&self) -> Install {
        // A server has no doctor to read: its install waits until it answers.
        let steps = STAGES
            .into_iter()
            .filter(|stage| !(self.kind == Kind::Server && *stage == Stage::Check))
            .map(|stage| {
                let mark = match stage.cmp(&self.stage) {
                    std::cmp::Ordering::Less => Mark::Done,
                    std::cmp::Ordering::Equal if self.failed.is_some() => Mark::Failed,
                    std::cmp::Ordering::Equal => Mark::Running,
                    std::cmp::Ordering::Greater => Mark::Waiting,
                };
                StepLine {
                    title: stage.title(&self.host, self.kind),
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
        Install { steps, bar }
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
            (Stage::Install, Mark::Done) => self.ptyd.and_then(|ptyd| match ptyd {
                slopty_deploy::Ptyd::Kept | slopty_deploy::Ptyd::HandsOver => {
                    Some("Every shell kept".to_owned())
                }
                slopty_deploy::Ptyd::Restarts { .. } => Some("Shells started afresh".to_owned()),
                slopty_deploy::Ptyd::Starts => None,
            }),
            _ => None,
        }
    }
}

/// An update from a Devices row, by the host it runs against.
#[derive(Debug)]
pub struct UpdateRun {
    worker: WorkerId,
    progress: Progress,
    task: Option<gpui::Task<()>>,
}

impl UpdateRun {
    /// Its deploy is under way.
    pub(crate) const fn running(&self) -> bool {
        self.task.is_some()
    }
}

/// Every update under way or failed.
pub type Updating = HashMap<String, UpdateRun>;

/// What "Update all" says it did: how many machines it is `updating`, and how many it found
/// `away`, each checked, and updated if older, once it is back.
fn update_all_words(updating: usize, away: usize) -> String {
    let machines = |n: usize| if n == 1 { "1 machine".to_owned() } else { format!("{n} machines") };
    let back = |n: usize| if n == 1 { "is" } else { "are" };
    match (updating, away) {
        (0, 0) => "Every machine runs this build".to_owned(),
        (n, 0) => format!("Updating {}", machines(n)),
        (0, a) => format!("{} away {} updated once back, if older", machines(a), back(a)),
        (n, a) => {
            format!("Updating {}; {a} away {} updated once back, if older", machines(n), back(a))
        }
    }
}

/// What a failed server update says: why, in `said`, and when "Update all" was to update the
/// workers after it (`held`), that they were left as they are.
fn update_failed_words(said: &str, held: bool) -> String {
    if held { format!("{said}. The machines were not updated") } else { said.to_owned() }
}

/// How long a removed machine's worker has to go off the server's list before the removal
/// says it still answers.
const GOES_WITHIN: std::time::Duration = std::time::Duration::from_secs(10);

/// A machine's removal under way, by its worker.
#[derive(Debug)]
pub struct RemoveRun {
    /// The machine, as the person knows it.
    name: String,
    /// The removal there, then the wait for the server to see its worker go.
    task: Option<gpui::Task<()>>,
    /// The worker there is gone: the server is to forget it once it lists it as away.
    removed: bool,
}

/// Every removal under way.
pub type Removing = HashMap<WorkerId, RemoveRun>;

impl Workspace {
    /// The server a worker installed from here registers with: the one this app is linked to.
    pub(crate) fn register_with(&self) -> Option<Server> {
        self.server_address().map(|a| Server { host: a.host().to_owned(), port: a.port() })
    }

    /// Replace the worker at `host` (its Devices row's "Update"): deploy with `--update`, through
    /// the SSH target it was installed with or else the host it was dialled at, then dial it
    /// again at once. The pill follows each step; the link coming up ends it. A worker on a
    /// newer build is never taken back to this one.
    pub(crate) fn update_worker(&mut self, host: &str, cx: &mut Context<Self>) {
        let Some((worker, notice)) = self.update_notice_at(host, cx) else { return };
        // A machine on a newer build is not taken back to this one: this device is the older.
        if notice.this_is_older() {
            Self::show_notice(format!("{host} runs a newer build: update Slopty here"), cx);
            return;
        }
        // Pressed again after the run stopped to say it ends sessions there: that is the yes.
        let stopped = self.updates.get(host).filter(|run| run.task.is_none());
        let end_sessions = stopped
            .is_some_and(|run| run.progress.failure().is_some_and(|f| f.ends_sessions.is_some()));
        self.start_update(worker, host, Said { end_sessions, ..Said::default() }, cx);
    }

    /// The worker at `host` that runs another build, and what updates it: one that refused
    /// this build, or one that links on an older one.
    fn update_notice_at(
        &self,
        host: &str,
        cx: &Context<Self>,
    ) -> Option<(WorkerId, slopty_client::update::UpdateNotice)> {
        let view = self.view.read(cx);
        self.workers.iter().find_map(|slot| {
            let notice = view.update_notice(slot.key).filter(|n| n.host == host)?;
            Some((slot.id, notice.clone()))
        })
    }

    /// Bring every machine on an older build to this one: the server first, then each worker
    /// as its row's "Update" does, each following its own run. A worker away now is
    /// updated once it answers again.
    pub(crate) fn update_all_workers(&mut self, cx: &mut Context<Self>) {
        if self.deployer.is_none() {
            Self::show_notice("Machines are updated from Slopty on a Mac".to_owned(), cx);
            return;
        }
        // Away, and not known to need an update: what it runs is told once it answers.
        let away: Vec<WorkerId> = {
            let view = self.view.read(cx);
            let statuses: HashMap<_, _> = view.workers().map(|(key, _, s)| (key, s)).collect();
            self.workers
                .iter()
                .filter(|slot| statuses.get(&slot.key).is_some_and(|s| !s.is_up()))
                .filter(|slot| view.update_notice(slot.key).is_none())
                .map(|slot| slot.id)
                .collect()
        };
        self.update_when_back.extend(away.iter().copied());
        let server = self.server_update_notice().is_some_and(|n| !n.this_is_older());
        if server {
            self.workers_after_server = true;
            self.update_server(cx);
        }
        let older = self.older_workers(cx);
        // The server's run is under way: the workers follow once it ends.
        if !(server && self.server_update.is_some()) {
            self.workers_after_server = false;
            for (worker, host) in &older {
                self.start_update(*worker, host, Said::default(), cx);
            }
        }
        Self::show_notice(
            update_all_words(older.len().saturating_add(usize::from(server)), away.len()),
            cx,
        );
    }

    /// Each worker on an older build than this one, and the host it is updated at. One that
    /// cannot be told older or newer is left to its own row.
    pub(crate) fn older_workers(&self, cx: &gpui::App) -> Vec<(WorkerId, String)> {
        let view = self.view.read(cx);
        self.workers
            .iter()
            .filter_map(|slot| {
                let notice = view.update_notice(slot.key)?;
                (notice.newer() == Some(slopty_client::update::Newer::Here))
                    .then(|| (slot.id, notice.host.clone()))
            })
            .collect()
    }

    /// The server's update that "Update all" started came back on this build: the workers'
    /// turn. One that failed stops "Update all" there ([`Self::server_update_failed`]).
    pub(crate) fn server_update_ended(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.workers_after_server) {
            for (worker, host) in self.older_workers(cx) {
                self.start_update(worker, &host, Said::default(), cx);
            }
        }
    }

    /// Worker `worker` answered on a different build: an update that brought it back says so,
    /// and it is brought to this build unasked where it is older and on this Mac or left by
    /// "Update all" for its return ([`Self::update_unasked`]).
    pub(crate) fn heard_other_build(
        &mut self,
        worker: WorkerId,
        notice: &slopty_client::update::UpdateNotice,
        cx: &mut Context<Self>,
    ) {
        self.update_still_wrong(worker, cx);
        self.update_unasked(worker, notice, cx);
    }

    /// Worker `worker` said its build on this wire, on its link (`linked`) or in the server's
    /// list: one older than this app has its rows offer Update, and once it links it is
    /// brought to this build unasked where [`Self::update_unasked`] says so.
    pub(crate) fn heard_build(
        &mut self,
        worker: WorkerId,
        build: &str,
        linked: bool,
        cx: &mut Context<Self>,
    ) {
        use slopty_client::update::{Newer, Of, UpdateNotice};
        let Some(key) = self.workers.iter().find(|w| w.id == worker).map(|w| w.key) else {
            return;
        };
        // A listing that says no build (a machine the server has not heard from yet) tells
        // nothing, unlike a wire prefix without one.
        let older = !build.is_empty()
            && slopty_proto::wire::newer(&slopty_proto::wire::this_build(), build)
                == Some(Newer::Here);
        let notice = older.then(|| self.directory_address(worker)).flatten().map(|at| {
            UpdateNotice { of: Of::Worker, host: at.host().to_owned(), peer: build.to_owned() }
        });
        self.view.update(cx, |v, cx| v.set_worker_behind(key, notice.clone(), cx));
        if !linked {
            return;
        }
        match notice {
            Some(notice) => self.update_unasked(worker, &notice, cx),
            None => {
                self.update_when_back.remove(&worker);
            }
        }
    }

    /// Bring `worker`, on another build (`notice`), to this one without a press, where this
    /// build is known to be the newer: on this Mac where this app is an installed build, once
    /// a launch, so an app update carries this Mac's worker with it; elsewhere once, where
    /// "Update all" found it away.
    fn update_unasked(
        &mut self,
        worker: WorkerId,
        notice: &slopty_client::update::UpdateNotice,
        cx: &mut Context<Self>,
    ) {
        if notice.newer() != Some(slopty_client::update::Newer::Here) {
            return;
        }
        let here = notice.here() && self.updates_itself() && self.updated_unasked.insert(worker);
        let back = self.update_when_back.remove(&worker);
        if here || back {
            tracing::info!(%worker, peer = %notice.peer, here, "update a worker on an older build");
            self.start_update(worker, &notice.host, Said::default(), cx);
        }
    }

    /// Whether this app brings its own Mac's daemons to its build unasked.
    pub(crate) fn updates_itself(&self) -> bool {
        self.deployer.as_ref().is_some_and(|d| d.installed())
    }

    /// Replace `worker`, dialled at `host`, as [`Self::update_worker`] does.
    fn start_update(&mut self, worker: WorkerId, host: &str, said: Said, cx: &mut Context<Self>) {
        let Some(deployer) = self.deployer.clone() else { return };
        if self.updates.get(host).is_some_and(|run| run.task.is_some()) {
            return;
        }
        let Some(server) = self.register_with() else {
            Self::show_notice(NO_SERVER.to_owned(), cx);
            return;
        };
        let target = deployer.target_of(worker).unwrap_or_else(|| Target::host(host));
        let (tx, events) = mpsc::unbounded_channel();
        let deploy = deployer.deploy(&target, server, said, tx);
        let owned = host.to_owned();
        let task = cx.spawn(async move |this, cx| {
            let host = owned.clone();
            let apply = move |ws: &mut Self, event: Event, cx: &mut Context<Self>| {
                if let Some(run) = ws.updates.get_mut(&host) {
                    run.progress.apply(event);
                    ws.publish_device_actions(cx);
                }
            };
            let Some(done) = drive(deploy, events, &this, cx, apply).await else { return };
            let _gone = this.update(cx, |ws, cx| ws.update_deployed(&owned, done, cx));
        });
        let progress = Progress::new(host.to_owned(), Kind::Update);
        let run = UpdateRun { worker, progress, task: Some(task) };
        self.updates.insert(host.to_owned(), run);
        self.publish_device_actions(cx);
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
                run.progress.deployed(deployed.platform);
                let worker = run.worker;
                self.connect_now(worker);
            }
            Err(failure) => {
                let failure = in_a_toast(failure);
                let hint = failure.hint.as_ref().map(|h| format!(" {h}")).unwrap_or_default();
                let said = format!("Could not update {host}: {}.{hint}", failure.title);
                run.progress.failed(failure);
                Self::show_failure(said, cx);
            }
        }
        self.publish_device_actions(cx);
    }

    /// `worker` linked: an update that brought it back is done.
    pub(crate) fn update_linked(&mut self, worker: WorkerId, cx: &mut Context<Self>) {
        let before = self.updates.len();
        self.updates.retain(|_, run| run.worker != worker || run.task.is_some());
        if self.updates.len() != before {
            self.publish_device_actions(cx);
        }
    }

    /// A dial to `worker` after its update found another build still: the update says so.
    pub(crate) fn update_still_wrong(&mut self, worker: WorkerId, cx: &mut Context<Self>) {
        let joining = self
            .updates
            .values_mut()
            .find(|run| run.worker == worker && run.task.is_none() && run.progress.running());
        let Some(run) = joining else { return };
        run.progress.failed(Failure::new(
            "It still runs a different build".to_owned(),
            Some("The machine kept its old slopty-worker; see its install log there.".to_owned()),
            Vec::new(),
        ));
        self.publish_device_actions(cx);
    }

    /// A row's "Cancel update": the update under way at `host` stops. Its `ssh` goes with it
    /// (the deploy's children die with the future), and its row offers Update again.
    pub(crate) fn cancel_update(&mut self, host: &str, cx: &mut Context<Self>) {
        let Some(run) = self.updates.get_mut(host).filter(|run| run.task.is_some()) else {
            return;
        };
        run.task = None;
        run.progress.failed(Failure::new(
            "The update was stopped".to_owned(),
            Some("Try again to start it over.".to_owned()),
            Vec::new(),
        ));
        self.publish_device_actions(cx);
    }

    /// Bring the server to this build where it runs: in place on this Mac, else over `ssh`
    /// through the target it was set up with or the host it is dialled at, then link to it
    /// again at once, at the address in use first.
    ///
    /// The cluster says it runs; a notice says how it ended. A machine whose host key this
    /// Mac's `ssh` does not know yet is sent to the Add device dialog, which shows the key.
    pub(crate) fn update_server(&mut self, cx: &mut Context<Self>) {
        let Some(address) = self.server_address().cloned() else {
            Self::show_notice("No server is set".to_owned(), cx);
            return;
        };
        let Some(notice) = self.server_update_notice().cloned() else {
            let said = if self.directory.linked() {
                "The server runs this build"
            } else {
                "The server has not answered, so its build is not known"
            };
            Self::show_notice(said.to_owned(), cx);
            return;
        };
        // Never back to an older build: this device is the one to update.
        if notice.this_is_older() {
            Self::show_notice("The server runs a newer build: update Slopty here".to_owned(), cx);
            return;
        }
        let Some(deployer) = self.deployer.clone() else {
            Self::show_notice(format!("Update it from a Mac with {}", notice.command()), cx);
            return;
        };
        if self.server_update.is_some() {
            return;
        }
        let (target, label) = if notice.here() {
            (Target::host(this_mac::LOOPBACK), THIS_MAC.to_owned())
        } else {
            let target =
                deployer.server_target(&address).unwrap_or_else(|| Target::host(address.host()));
            let label = target.host.clone();
            (target, label)
        };
        tracing::info!(server = %address, host = %target.host, "update the server");
        let (tx, events) = mpsc::unbounded_channel();
        let serve = deployer.serve(&target, Said::default(), tx);
        self.view.update(cx, |v, cx| v.set_server_status(Some(UPDATING_SERVER.to_owned()), cx));
        let task = cx.spawn(async move |this, cx| {
            let Some(done) = drive(serve, events, &this, cx, |_ws, _event, _cx| {}).await else {
                return;
            };
            let served = match done {
                Ok(served) => served,
                Err(failure) => {
                    let _gone = this.update(cx, |ws, cx| ws.server_update_failed(failure, cx));
                    return;
                }
            };
            let mut why = String::new();
            let others = served
                .addresses
                .iter()
                .filter_map(|a| HostAddr::parse_with_port(a, address.port()).ok());
            for at in std::iter::once(address.clone()).chain(others) {
                match deployer.link_server(&at).await {
                    Ok(link) => {
                        let _gone = this.update(cx, |ws, cx| ws.server_updated(at, link, cx));
                        return;
                    }
                    Err(e) => why = e,
                }
            }
            let failure = Failure::new(
                format!(
                    "The server on {label} runs this build, and Slopty could not connect to it"
                ),
                Some(format!("Check that UDP {} reaches it from this Mac.", address.port())),
                vec![why],
            );
            let _gone = this.update(cx, |ws, cx| ws.server_update_failed(failure, cx));
        });
        self.server_update = Some(task);
    }

    /// The server came back on this build at `address`.
    fn server_updated(&mut self, address: HostAddr, link: ServerLink, cx: &mut Context<Self>) {
        self.server_update = None;
        let name = link.name.clone();
        if self.server_address() == Some(&address) {
            self.relink_server(link, cx);
        } else if let Err(e) = self.save_server(Some(&address)) {
            link.close();
            let failure = Failure::new(
                "The server runs, and Slopty could not save it".to_owned(),
                None,
                vec![e],
            );
            return self.server_update_failed(failure, cx);
        } else {
            self.set_server(Some(address), Some(link), cx);
        }
        slopty_shell::toast::success(cx, format!("Updated {name}"));
        self.server_update_ended(cx);
        cx.notify();
    }

    /// The server's update stopped for `failure`: said in a notice, the cluster back to
    /// what the server is.
    fn server_update_failed(&mut self, failure: Failure, cx: &mut Context<Self>) {
        self.server_update = None;
        tracing::warn!(title = %failure.title, lines = ?failure.lines, "update the server");
        let failure = match &failure.trust {
            Some(key) => Failure::new(
                format!("{}'s host key is not trusted yet", key.target),
                Some(format!("Check and trust it in {ADD_DEVICE}.")),
                Vec::new(),
            ),
            None => failure,
        };
        let status = self
            .server_other_build_notice()
            .map_or(super::server::UNREACHABLE, |_| super::server::OTHER_BUILD);
        self.view.update(cx, |v, cx| v.set_server_status(Some(status.to_owned()), cx));
        let said = match (&failure.hint, failure.lines.last()) {
            (Some(hint), _) => format!("{}. {hint}", failure.title),
            (None, Some(line)) => format!("{}: {line}", failure.title),
            (None, None) => failure.title,
        };
        // "Update all" stops here: workers brought to a build their server does not run would
        // lose it.
        let held = std::mem::take(&mut self.workers_after_server);
        Self::show_failure(update_failed_words(&said, held), cx);
    }
}

/// What an install or an update says with no server to register the machine with.
pub const NO_SERVER: &str = "Connect to a server first: machines register with it";

/// Look, at most [`this_mac::LISTED_TRIES`] times, for the server's directory to list `worker`;
/// whether it did.
pub(crate) async fn listed(
    this: &gpui::WeakEntity<Workspace>,
    cx: &mut gpui::AsyncApp,
    worker: WorkerId,
) -> bool {
    for attempt in 0..this_mac::LISTED_TRIES {
        if attempt > 0 {
            cx.background_executor().timer(this_mac::RETRY).await;
        }
        match this.update(cx, |ws, _cx| ws.directory.get(worker).is_some()) {
            Ok(true) => return true,
            Ok(false) => {}
            Err(_gone) => return false,
        }
    }
    false
}

/// The worker installed on `host` runs, and the server never listed it: why, as the worker
/// said of its link when it was checked.
pub(crate) fn not_listed(host: &str, deployed: &Deployed) -> Failure {
    use slopty_proto::ctl::LinkState;
    let why = match deployed.health.server.as_ref().map(|s| &s.link) {
        Some(LinkState::Redialling { why } | LinkState::Refused { why }) => why.clone(),
        // A tagged node needs a grant only the tailnet's policy gives; no allow list here
        // lets it in, so the hint names the grant and where it is copied from.
        Some(LinkState::NotGranted) => {
            return Failure::new(
                format!("The tailnet policy does not let {host} in as a machine"),
                Some(crate::server::WORKER_GRANT_WHERE.to_owned()),
                Vec::new(),
            );
        }
        Some(LinkState::Dialling | LinkState::Linked) | None => String::new(),
    };
    let hint = format!(
        "Check that it reaches the server at {}; on a VPN, list its address under [network] allow.",
        deployed.server
    );
    let lines = if why.is_empty() { Vec::new() } else { vec![why] };
    Failure::new(
        format!("slopty-worker runs on {host}, and the server does not list it"),
        Some(hint),
        lines,
    )
}

/// `failure` as a toast says it: a toast has no fingerprint to show or key to trust, and no
/// field for a password, so a machine new to this Mac, or one that takes only a password, sends
/// the person to the Add device dialog, which has them. What it stopped at is kept.
fn in_a_toast(failure: Failure) -> Failure {
    let (title, hint) = match (&failure.trust, &failure.password) {
        (Some(key), _) => (
            format!("{}'s host key is not trusted yet", key.target),
            format!("Check and trust it in {ADD_DEVICE}."),
        ),
        (None, Some(ask)) => (
            format!("{} asks for a password", ask.host),
            format!("Sign in with it in {ADD_DEVICE}."),
        ),
        (None, None) => return failure,
    };
    Failure { title, hint: Some(hint), lines: Vec::new(), ..failure }
}

impl Workspace {
    /// Take `worker`'s machine off, as its confirm said: the worker and everything else of
    /// Slopty's there goes ([`Deployer::remove`]), then, once the server lists the worker as
    /// away, the server forgets it ([`Self::forget_worker`]). The person's repositories,
    /// worktrees and agent sessions there stay.
    pub(crate) fn remove_worker(&mut self, worker: WorkerId, cx: &mut Context<Self>) {
        let Some(deployer) = self.deployer.clone() else {
            Self::show_notice("Machines are removed from Slopty on a Mac".to_owned(), cx);
            return;
        };
        if self.removals.contains_key(&worker) {
            return;
        }
        let listed = self.directory.get(worker);
        let name = listed.map_or_else(|| worker.to_string(), |w| w.name.clone());
        let host = listed
            .and_then(|w| w.address.parse::<std::net::SocketAddr>().ok())
            .map_or_else(|| name.clone(), |addr| addr.ip().to_string());
        let target = deployer.target_of(worker).unwrap_or_else(|| Target::host(&host));
        let (tx, events) = mpsc::unbounded_channel();
        let removal = deployer.remove(&target, tx);
        let task = cx.spawn(async move |this, cx| {
            let quiet = |_ws: &mut Self, _event: Event, _cx: &mut Context<Self>| {};
            let Some(done) = drive(removal, events, &this, cx, quiet).await else { return };
            let _gone = this.update(cx, |ws, cx| ws.worker_removed(worker, done, cx));
        });
        Self::show_notice(format!("Removing {name}\u{2026}"), cx);
        self.removals.insert(worker, RemoveRun { name, task: Some(task), removed: false });
        self.publish_device_actions(cx);
    }

    /// `worker`'s removal there ended: on to the server forgetting it once it lists the worker
    /// as away, at once when it does already; or why it stopped.
    fn worker_removed(
        &mut self,
        worker: WorkerId,
        done: Result<slopty_deploy::Removed, Failure>,
        cx: &mut Context<Self>,
    ) {
        let Some(run) = self.removals.get_mut(&worker) else { return };
        match done {
            Err(failure) => {
                let name = run.name.clone();
                self.removals.remove(&worker);
                self.publish_device_actions(cx);
                let failure = in_a_toast(failure);
                let hint = failure.hint.map(|h| format!(" {h}")).unwrap_or_default();
                Self::show_failure(
                    format!("Could not remove {name}: {}.{hint}", failure.title),
                    cx,
                );
            }
            Ok(removed) => {
                tracing::info!(%worker, paths = removed.paths.len(), "worker removed there");
                run.removed = true;
                let online = self
                    .directory
                    .get(worker)
                    .is_some_and(|w| w.liveness == slopty_proto::server::Liveness::Online);
                if !online {
                    self.removal_went_away(worker, cx);
                    return;
                }
                run.task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(GOES_WITHIN).await;
                    let _gone = this.update(cx, |ws, cx| ws.removal_still_answers(worker, cx));
                }));
            }
        }
    }

    /// The server lists `worker` as away: a removal that took its worker off has the server
    /// forget it now.
    pub(crate) fn removal_went_away(&mut self, worker: WorkerId, cx: &mut Context<Self>) {
        if !self.removals.get(&worker).is_some_and(|run| run.removed) {
            return;
        }
        let Some(run) = self.removals.remove(&worker) else { return };
        self.forget_worker(worker, cx);
        tracing::info!(%worker, name = %run.name, "removed, and forgotten");
    }

    /// The server still lists `worker` as online a while after its removal there.
    fn removal_still_answers(&mut self, worker: WorkerId, cx: &mut Context<Self>) {
        let Some(run) = self.removals.remove(&worker) else { return };
        self.publish_device_actions(cx);
        let name = run.name;
        Self::show_failure(format!("{name} still answers; Slopty did not stop there"), cx);
    }

    /// Whether `worker`'s machine is being removed.
    pub(crate) fn removing(&self, worker: WorkerId) -> bool {
        self.removals.contains_key(&worker)
    }
}

/// Await `deploy`, handing each event to `apply` on the workspace as it comes; what it ended
/// with, or `None` when the workspace is gone.
async fn drive<T>(
    mut deploy: Pending<Result<T, Failure>>,
    mut events: mpsc::UnboundedReceiver<Event>,
    this: &gpui::WeakEntity<Workspace>,
    cx: &mut gpui::AsyncApp,
    apply: impl Fn(&mut Workspace, Event, &mut Context<Workspace>),
) -> Option<Result<T, Failure>> {
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

    use std::path::{Path, PathBuf};

    use slopty_core::WorkerId;
    use slopty_deploy::{
        DeployError, Deployed, Event, Failure, HostKey, Local, Plan, Remembered, Served, Server,
        Ssh,
    };
    use slopty_net::HostAddr;
    use slopty_net::server::ServerLink;
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
            server: Server,
            said: super::Said,
            events: mpsc::UnboundedSender<Event>,
        ) -> Pending<Result<Deployed, Failure>> {
            let to = to.clone();
            let task = self.runtime.spawn(async move {
                let source = here()?;
                let mut on = |event| {
                    let _gone = events.send(event);
                };
                let plan = Plan {
                    sources: slopty_deploy::bundled(&source),
                    update: true,
                    server,
                    end_sessions: said.end_sessions,
                    password: said.password,
                    add_key: said.add_key,
                };
                let done = match runner(&to).await {
                    Runner::Here(_) if misplaced(&source) => return Err(move_first()),
                    Runner::Here(local) => slopty_deploy::deploy(&local, &plan, &mut on).await,
                    Runner::Ssh(ssh) => {
                        let done = slopty_deploy::deploy(&ssh, &plan, &mut on).await;
                        return explained(&ssh, &to, "deploy", done).await;
                    }
                };
                explained_here(&to, "deploy", done)
            });
            let died = Err(Failure::new("The install stopped".to_owned(), None, Vec::new()));
            Aborting(task).pending(died)
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

        fn serve(
            &self,
            to: &Target,
            said: super::Said,
            events: mpsc::UnboundedSender<Event>,
        ) -> Pending<Result<Served, Failure>> {
            let to = to.clone();
            let task = self.runtime.spawn(async move {
                let source = here()?;
                let sources = slopty_deploy::bundled(&source);
                let mut on = |event| {
                    let _gone = events.send(event);
                };
                let done = match runner(&to).await {
                    Runner::Here(_) if misplaced(&source) => return Err(move_first()),
                    Runner::Here(local) => slopty_deploy::serve(&local, &sources, &mut on).await,
                    Runner::Ssh(ssh) => {
                        let done = match &said.password {
                            Some(password) => match ssh.signed_in(password).await {
                                Ok(signed) => {
                                    let done =
                                        slopty_deploy::serve(&signed, &sources, &mut on).await;
                                    if said.add_key && done.is_ok() {
                                        add_key(&signed, &mut on).await;
                                    }
                                    signed.end().await;
                                    done
                                }
                                Err(e) => Err(e),
                            },
                            None => slopty_deploy::serve(&ssh, &sources, &mut on).await,
                        };
                        return explained(&ssh, &to, "serve", done).await;
                    }
                };
                explained_here(&to, "serve", done)
            });
            let died = Err(Failure::new("The install stopped".to_owned(), None, Vec::new()));
            Aborting(task).pending(died)
        }

        fn remove(
            &self,
            to: &Target,
            events: mpsc::UnboundedSender<Event>,
        ) -> Pending<Result<slopty_deploy::Removed, Failure>> {
            let to = to.clone();
            let task = self.runtime.spawn(async move {
                let source = here()?;
                let sources = slopty_deploy::bundled(&source);
                let mut on = |event| {
                    let _gone = events.send(event);
                };
                let done = match runner(&to).await {
                    Runner::Here(local) => {
                        let done = slopty_deploy::remove(&local, &sources, None, &mut on).await;
                        // "Use this Mac" opened Slopty at login so a Mac that shares itself
                        // says when an agent needs the person; that goes with the worker.
                        if done.is_ok()
                            && let Err(e) =
                                tokio::task::spawn_blocking(|| slopty_platform::login::set(false))
                                    .await
                                    .map_err(|e| e.to_string())
                                    .and_then(|set| set)
                        {
                            tracing::warn!(error = %e, "stop opening at login");
                        }
                        done
                    }
                    Runner::Ssh(ssh) => {
                        let done = slopty_deploy::remove(&ssh, &sources, None, &mut on).await;
                        return explained(&ssh, &to, "remove", done).await;
                    }
                };
                explained_here(&to, "remove", done)
            });
            let died = Err(Failure::new("The removal stopped".to_owned(), None, Vec::new()));
            Aborting(task).pending(died)
        }

        fn trust(&self, key: &HostKey) -> Pending<Result<(), Failure>> {
            let key = key.clone();
            let task = self.runtime.spawn(async move {
                key.trust().await.map_err(|e| {
                    tracing::warn!(host = %key.target, error = %e, "trust its host key");
                    Failure::new(
                        format!("Could not trust {}'s key", key.target),
                        None,
                        vec![e.to_string()],
                    )
                })
            });
            let died = Err(Failure::new("Trusting the key stopped".to_owned(), None, Vec::new()));
            Aborting(task).pending(died)
        }

        fn link_server(&self, address: &HostAddr) -> Pending<Result<ServerLink, String>> {
            let address = address.clone();
            let task = self.runtime.spawn(async move {
                net::link_server(&address).await.map_err(|e| format!("{e:#}"))
            });
            Aborting(task).pending(Err("connecting stopped".to_owned()))
        }

        fn remember_server(&self, address: &HostAddr, to: &Target) {
            let mut kept = Remembered::open_in(&self.data);
            if let Err(e) = kept.remember(&server_key(address), to.clone()) {
                tracing::warn!(server = %address, error = %e, "keep the server's ssh target");
            }
        }

        fn server_target(&self, address: &HostAddr) -> Option<Target> {
            Remembered::open_in(&self.data).get(&server_key(address)).cloned()
        }

        fn installed(&self) -> bool {
            here().is_ok_and(|dir| dir.ends_with("Contents/MacOS"))
        }
    }

    /// The key a server's SSH target is kept under, beside the workers' ids.
    fn server_key(address: &HostAddr) -> String {
        format!("server {address}")
    }

    /// Where Slopty's own binaries are: beside the app.
    /// Add the person's public key over `runner`, signed in, as a worker's deploy does; only
    /// logged, since the server it set up runs either way.
    async fn add_key(runner: &dyn slopty_deploy::Runner, on: &mut slopty_deploy::OnEvent<'_>) {
        let Some(key) = slopty_deploy::public_key().await else {
            tracing::info!("no public key to add to the server's machine");
            return;
        };
        let added = slopty_deploy::add_key(runner, &key, on).await;
        tracing::info!(?added, "the person's key on the server's machine");
    }

    /// Whether this Mac's daemons would run from a copy of Slopty a restart takes away.
    fn misplaced(source: &Path) -> bool {
        crate::this_mac::misplaced(source, &slopty_platform::dirs::home())
    }

    /// Why an install on this Mac waits for Slopty to move.
    fn move_first() -> Failure {
        Failure::new(
            "Move Slopty to Applications first".to_owned(),
            Some(format!("{} Use this Mac moves it.", crate::this_mac::MISPLACED)),
            Vec::new(),
        )
    }

    fn here() -> Result<PathBuf, Failure> {
        slopty_platform::service::sibling_dir().map_err(|e| {
            Failure::new(
                "Could not find slopty-worker inside Slopty".to_owned(),
                None,
                vec![e.to_string()],
            )
        })
    }

    /// What runs a plan against a machine.
    enum Runner {
        /// This Mac itself, which needs no Remote Login and installs in place.
        Here(Local),
        /// The system `ssh`.
        Ssh(Ssh),
    }

    /// What runs a plan against `to`: this Mac when it names this Mac, else the system `ssh`.
    async fn runner(to: &Target) -> Runner {
        if to.is_this_machine().await {
            Runner::Here(Local::here())
        } else {
            Runner::Ssh(Ssh::unattended(to))
        }
    }

    /// A run on this Mac as a window says it.
    fn explained_here<T>(
        to: &Target,
        what: &str,
        done: Result<T, DeployError>,
    ) -> Result<T, Failure> {
        done.map_err(|e| {
            tracing::warn!(host = %to.host, error = %e, "{what}");
            e.failure()
        })
    }

    /// A run over `ssh` as a window says it: a host key `ssh` does not know yet comes with the
    /// key, for the person to check and trust.
    async fn explained<T>(
        ssh: &Ssh,
        to: &Target,
        what: &str,
        done: Result<T, DeployError>,
    ) -> Result<T, Failure> {
        match done {
            Ok(done) => Ok(done),
            Err(e) => {
                tracing::warn!(host = %to.host, error = %e, "{what}");
                Err(ssh.explain(&e).await)
            }
        }
    }
}

#[cfg(test)]
mod tests;
