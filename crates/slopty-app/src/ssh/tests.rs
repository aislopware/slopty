//! The SSH sheet and a tile's "Update" driven through a stand-in [`Deployer`] the test plays:
//! it hands the test the deploy's event sender and its ending, so each step is shown as the
//! test says it happened, and no machine is reached.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{Modifiers, TestAppContext, VisualTestContext, px, size};
use slopty_deploy::{Arch, Os, Platform};
use slopty_proto::ctl::PasteboardAccess;
use slopty_proto::server::{Os as WorkerOs, WorkerCaps};
use tokio::sync::oneshot;

use super::actions::{RegisterWithServer, UpdateAllWorkers, UpdateServer};
use super::*;
use crate::tests::{shell, workspace};

/// Set when the deploy it guards is dropped: a cancelled run's `ssh` is killed then.
struct Dropped(Arc<AtomicBool>);

impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A deployer the test plays: what it was asked, the deploy's events and its ending, and the
/// targets it was told to keep.
#[derive(Debug, Default)]
struct StandIn {
    asked: RefCell<Vec<String>>,
    kept: RefCell<HashMap<WorkerId, Target>>,
    events: RefCell<Option<mpsc::UnboundedSender<Event>>>,
    finish: RefCell<Option<oneshot::Sender<Result<Deployed, Failure>>>>,
    served: RefCell<Option<oneshot::Sender<Result<Served, Failure>>>>,
    dropped: Arc<AtomicBool>,
    /// The machines whose host keys the person trusted.
    trusted: RefCell<Vec<String>>,
    /// The last password a run was handed, as the deployer would hand it to `ssh`.
    password: RefCell<Option<String>>,
    /// The one address that answers an add, and the worker it adds.
    answers: String,
    worker: WorkerId,
    /// The servers' targets it was told to keep, by address.
    servers: RefCell<HashMap<String, Target>>,
    /// It plays an installed app, which brings its own Mac's daemons to its build.
    installed: std::cell::Cell<bool>,
}

impl StandIn {
    fn new(answers: &str) -> Rc<Self> {
        Rc::new(Self { answers: answers.to_owned(), worker: WorkerId::new(), ..Self::default() })
    }

    fn asked(&self) -> Vec<String> {
        std::mem::take(&mut *self.asked.borrow_mut())
    }

    fn say(&self, cx: &VisualTestContext, events: impl IntoIterator<Item = Event>) {
        let sender = self.events.borrow().clone().expect("a deploy runs");
        for event in events {
            sender.send(event).unwrap();
        }
        cx.run_until_parked();
    }

    fn end(&self, cx: &VisualTestContext, done: Result<Deployed, Failure>) {
        let finish = self.finish.borrow_mut().take().expect("a deploy runs");
        finish.send(done).unwrap();
        cx.run_until_parked();
    }
}

impl Deployer for StandIn {
    fn deploy(
        &self,
        to: &Target,
        server: Option<Server>,
        said: Said,
        events: mpsc::UnboundedSender<Event>,
    ) -> Pending<Result<Deployed, Failure>> {
        let Target { host, user, port } = to;
        let server = server.map(|s| format!("{}:{}", s.host, s.port));
        let ends = if said.end_sessions { " end_sessions" } else { "" };
        let signs = if said.password.is_some() { " password" } else { "" };
        let key = if said.add_key { " add_key" } else { "" };
        *self.password.borrow_mut() = said
            .password
            .as_ref()
            .map(|p| slopty_deploy::ExposeSecret::expose_secret(p).to_owned());
        self.asked
            .borrow_mut()
            .push(format!("deploy {host} {user:?} {port:?} server={server:?}{ends}{signs}{key}"));
        *self.events.borrow_mut() = Some(events);
        let (tx, rx) = oneshot::channel();
        *self.finish.borrow_mut() = Some(tx);
        self.dropped.store(false, Ordering::SeqCst);
        let guard = Dropped(Arc::clone(&self.dropped));
        Box::pin(async move {
            let _guard = guard;
            rx.await.unwrap_or_else(|_| Err(failure("the test let go")))
        })
    }

    fn add(&self, address: &str) -> Pending<Result<net::Added, String>> {
        self.asked.borrow_mut().push(format!("add {address}"));
        let added = (address == self.answers)
            .then(|| net::Added { id: self.worker, name: "mini".to_owned() })
            .ok_or_else(|| format!("nothing answered at {address}"));
        Box::pin(std::future::ready(added))
    }

    fn remember(&self, worker: WorkerId, to: &Target) {
        self.kept.borrow_mut().insert(worker, to.clone());
    }

    fn target_of(&self, worker: WorkerId) -> Option<Target> {
        self.kept.borrow().get(&worker).cloned()
    }

    fn serve(
        &self,
        to: &Target,
        _said: Said,
        events: mpsc::UnboundedSender<Event>,
    ) -> Pending<Result<Served, Failure>> {
        match &to.user {
            Some(user) => self.asked.borrow_mut().push(format!("serve {user}@{}", to.host)),
            None => self.asked.borrow_mut().push(format!("serve {}", to.host)),
        }
        *self.events.borrow_mut() = Some(events);
        let (tx, rx) = oneshot::channel();
        *self.served.borrow_mut() = Some(tx);
        Box::pin(async move { rx.await.unwrap_or_else(|_| Err(failure("the test let go"))) })
    }

    fn link_server(&self, address: &HostAddr) -> Pending<Result<ServerLink, String>> {
        self.asked.borrow_mut().push(format!("link {address}"));
        Box::pin(std::future::ready(Err(format!("nothing answered at {address}"))))
    }

    fn register_here(&self, server: &HostAddr) {
        self.asked.borrow_mut().push(format!("register {server}"));
    }

    fn remember_server(&self, address: &HostAddr, to: &Target) {
        self.servers.borrow_mut().insert(address.to_string(), to.clone());
    }

    fn server_target(&self, address: &HostAddr) -> Option<Target> {
        self.servers.borrow().get(&address.to_string()).cloned()
    }

    fn installed(&self) -> bool {
        self.installed.get()
    }

    fn trust(&self, key: &HostKey) -> Pending<Result<(), Failure>> {
        self.trusted.borrow_mut().push(key.target.clone());
        Box::pin(std::future::ready(Ok(())))
    }
}

fn failure(title: &str) -> Failure {
    Failure::new(title.to_owned(), None, Vec::new())
}

fn deployed() -> Deployed {
    Deployed {
        platform: Platform { os: Os::MacOs, arch: Arch::Arm64 },
        health: Health {
            version: "0.1.0".to_owned(),
            exe: "/Users/me/.slopty/deploy/slopty-worker".to_owned(),
            caps: WorkerCaps {
                can_capture: true,
                can_inject: true,
                ..WorkerCaps::bare(WorkerOs::MacOs)
            },
            listen: "[::]:45550".to_owned(),
            allow: Vec::new(),
            tailscale: Tailscale::Up { node: "mini.tail1234.ts.net.".to_owned(), ip: None },
            pasteboard: PasteboardAccess::Allowed,
            clients: 0,
            sessions: 0,
            uptime_secs: 1,
        },
        server: None,
        ptyd: None,
        stops_at_logout: None,
        console: slopty_deploy::Console::default(),
        key: None,
    }
}

fn sheet_progress(ws: &Entity<Workspace>, cx: &VisualTestContext) -> Option<Progress> {
    ws.read_with(cx, |ws, _| ws.adding.as_ref()?.ssh.as_ref()?.progress().cloned())
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(at.center(), Modifiers::none());
    cx.run_until_parked();
}

fn type_host(ws: &Entity<Workspace>, cx: &mut VisualTestContext, text: &str) {
    let host = ws.read_with(cx, |ws, _| {
        ws.adding.as_ref().and_then(|a| a.ssh.as_ref()).map(|s| s.host.clone())
    });
    let host = host.expect("the sheet is up");
    let text = text.to_owned();
    cx.update(|window, cx| host.update(cx, |input, cx| input.set_value(text, window, cx)));
}

#[test]
fn the_fields_read_as_ssh_takes_them() {
    let read = Target::read;
    assert_eq!(
        read(" me@mini ", "", ""),
        Ok(Target { host: "mini".to_owned(), user: Some("me".to_owned()), port: None })
    );
    assert_eq!(
        read("mini", "admin", "2222").map(|t| (t.user, t.port)),
        Ok((Some("admin".to_owned()), Some(2222)))
    );
    assert!(read("", "", "").is_err(), "a host is needed");
    assert!(read("-oProxyCommand=x", "", "").is_err(), "not an option in disguise");
    assert!(read("mini", "", "0").is_err() && read("mini", "", "ssh").is_err());
}

/// The worker is added at its tailnet name first, then the host `ssh` reached, each with the
/// port it listens on when that is not the default.
#[test]
fn the_new_worker_is_reached_at_its_tailnet_name_first() {
    let target = Target { host: "10.0.0.7".to_owned(), user: None, port: Some(2222) };
    let mut health = deployed().health;
    assert_eq!(addresses(&target, &health), ["mini.tail1234.ts.net", "10.0.0.7"]);
    health.listen = "0.0.0.0:7000".to_owned();
    health.tailscale = Tailscale::Absent;
    assert_eq!(addresses(&target, &health), ["10.0.0.7:7000"]);
}

/// Each stage is a line: done ones ticked with what they found, the one under way with the
/// bar (a share while the binaries go up), and a failure marks where it stopped.
#[test]
fn the_steps_follow_the_deploy() {
    let mut progress = Progress::new("mini".to_owned(), Kind::Install);
    let marks = |p: &Progress| p.view().steps.iter().map(|s| s.mark).collect::<Vec<_>>();
    assert_eq!(marks(&progress).first(), Some(&Mark::Running));
    assert_eq!(progress.view().bar, Bar::Busy);
    progress.apply(Event::Machine(Platform { os: Os::Linux, arch: Arch::X86_64 }));
    progress.apply(Event::Step(Step::Upload { name: "slopty-ptyd" }));
    progress.apply(Event::Sent { sent: 1_048_576, total: 4_194_304 });
    let view = progress.view();
    assert_eq!(view.bar, Bar::Share(0.25));
    let details: Vec<_> = view.steps.iter().map(|s| s.detail.clone()).collect();
    assert_eq!(details.first(), Some(&Some("Linux x86_64".to_owned())));
    assert_eq!(details.get(1), Some(&Some("1.0 MB of 4.0 MB".to_owned())));
    progress.apply(Event::Step(Step::Install));
    progress.apply(Event::Line("  installed slopty-worker  ".to_owned()));
    progress.apply(Event::Line(String::new()));
    let install = progress.view().steps.get(2).cloned().unwrap();
    assert_eq!(install.detail.as_deref(), Some("installed slopty-worker"), "the last said");
    assert_eq!(install.title, "Install and start it");
    progress.failed(failure("The install on mini failed"));
    let view = progress.view();
    assert_eq!(
        view.steps.iter().map(|s| s.mark).collect::<Vec<_>>(),
        [Mark::Done, Mark::Done, Mark::Failed, Mark::Waiting, Mark::Waiting]
    );
    assert_eq!(view.bar, Bar::Hidden);
    assert!(view.failed.is_some());
    assert_eq!(
        Progress::new("mini".to_owned(), Kind::Update)
            .view()
            .steps
            .last()
            .map(|s| s.title.as_str()),
        Some("Reconnect")
    );
}

/// On the first run the SSH entry opens a form in the address's place; Install with no host
/// says what is missing; with one, each step shows as the deploy says it, the bar filling while
/// the binaries go up, and once the worker answers it is added at its tailnet name and the page
/// gives way to the workspace.
#[gpui::test]
fn the_sheet_installs_step_by_step_then_adds_the_worker(cx: &mut TestAppContext) {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (ws, cx) = shell(cx, &runtime, &dir, false);
    cx.simulate_resize(size(px(900.0), px(800.0)));
    let deployer = StandIn::new("mini.tail1234.ts.net");
    let shared: Rc<dyn Deployer> = Rc::<StandIn>::clone(&deployer);
    ws.update(cx, |ws, cx| {
        ws.deployer = Some(shared);
        cx.notify();
    });
    cx.run_until_parked();
    assert!(ws.read_with(cx, |ws, _| ws.welcome()), "the first run");

    let entry = cx.debug_bounds("install-over-ssh").expect("the entry, a row to press");
    let field = cx.debug_bounds("add-worker-field").expect("the address");
    assert!(entry.bottom() <= field.top(), "a place to add, over the address to type");
    click(cx, "install-over-ssh");
    assert!(cx.debug_bounds("ssh-form").is_some(), "the form");
    assert!(cx.debug_bounds("add-worker-field").is_none(), "in the address's place");
    assert!(cx.debug_bounds("install-over-ssh").is_none(), "the entry has done its part");

    click(cx, "ssh-install");
    assert!(deployer.asked().is_empty(), "nothing to reach yet");
    assert!(cx.debug_bounds("ssh-note").is_some(), "the form says what is missing");

    type_host(&ws, cx, "me@mini");
    click(cx, "ssh-install");
    assert_eq!(deployer.asked(), ["deploy mini Some(\"me\") None server=None"]);
    assert!(cx.debug_bounds("install-steps").is_some(), "the steps");
    assert!(cx.debug_bounds("ssh-form").is_none(), "in the form's place");
    assert!(cx.debug_bounds("install-bar").is_some(), "a busy bar while it connects");

    deployer.say(
        cx,
        [
            Event::Step(Step::Reach),
            Event::Machine(Platform { os: Os::MacOs, arch: Arch::Arm64 }),
            Event::Step(Step::Upload { name: "slopty-ptyd" }),
            Event::Sent { sent: 50, total: 100 },
        ],
    );
    let view = sheet_progress(&ws, cx).expect("a run").view();
    assert_eq!(view.bar, Bar::Share(0.5));
    assert_eq!(view.current().map(|s| s.slug), Some("copy"));
    assert!(cx.debug_bounds("install-step-copy").is_some());

    deployer.say(
        cx,
        [Event::Step(Step::Install), Event::Line("up".to_owned()), Event::Step(Step::Check)],
    );
    assert_eq!(sheet_progress(&ws, cx).map(|p| p.stage()), Some(Stage::Check));
    deployer.end(cx, Ok(deployed()));
    assert_eq!(deployer.asked(), ["add mini.tail1234.ts.net"], "at its tailnet name");
    let (adding, added) = ws.read_with(cx, |ws, _| {
        (ws.adding.is_some(), ws.workers.iter().any(|w| w.id == deployer.worker && w.added))
    });
    assert!(!adding && added, "the page gave way to the new worker");
    let kept = deployer.target_of(deployer.worker);
    assert_eq!(kept.and_then(|t| t.user).as_deref(), Some("me"), "how it was reached, kept");
}

/// A machine named already (an address that answered as another build) opens the sheet with
/// its host filled in, and Install brings it to this build.
#[gpui::test]
fn the_sheet_opens_on_a_named_host(cx: &mut TestAppContext) {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (ws, cx) = shell(cx, &runtime, &dir, true);
    cx.simulate_resize(size(px(900.0), px(800.0)));
    let deployer = StandIn::new("mini");
    let shared: Rc<dyn Deployer> = Rc::<StandIn>::clone(&deployer);
    ws.update(cx, |ws, _cx| ws.deployer = Some(shared));
    cx.update(|window, cx| ws.update(cx, |ws, cx| ws.open_ssh_at("me@mini", window, cx)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("ssh-form").is_some(), "the form");
    click(cx, "ssh-install");
    assert_eq!(deployer.asked(), ["deploy mini Some(\"me\") None server=None"]);
}

/// Cancel stops the run (its deploy is dropped, which kills `ssh`) and brings the form back;
/// a failure shows where it stopped, what to do and the machine's last lines, with the form
/// there to fix and "Try again".
#[gpui::test]
fn a_run_can_be_cancelled_and_a_failure_says_why(cx: &mut TestAppContext) {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (ws, cx) = shell(cx, &runtime, &dir, true);
    cx.simulate_resize(size(px(900.0), px(800.0)));
    let deployer = StandIn::new("mini");
    let shared: Rc<dyn Deployer> = Rc::<StandIn>::clone(&deployer);
    ws.update(cx, |ws, _cx| ws.deployer = Some(shared));
    cx.dispatch_action(actions::InstallOverSsh);
    cx.run_until_parked();
    type_host(&ws, cx, "mini");
    click(cx, "ssh-install");
    deployer.say(cx, [Event::Step(Step::Reach)]);
    assert!(!deployer.dropped.load(Ordering::SeqCst));

    click(cx, "ssh-cancel");
    assert!(deployer.dropped.load(Ordering::SeqCst), "the deploy is dropped, and its ssh");
    assert!(cx.debug_bounds("ssh-form").is_some(), "the form is back");
    assert!(cx.debug_bounds("ssh-note").is_some(), "saying it stopped");

    click(cx, "ssh-install");
    deployer.asked();
    deployer.end(
        cx,
        Err(Failure::new(
            "mini did not accept your SSH key".to_owned(),
            Some("Add your key to your ssh agent, or to its authorized_keys.".to_owned()),
            vec!["me@mini: Permission denied (publickey).".to_owned()],
        )),
    );
    assert!(cx.debug_bounds("install-failure").is_some(), "why");
    assert!(cx.debug_bounds("install-output").is_some(), "the machine's last lines");
    assert!(cx.debug_bounds("ssh-form").is_some(), "the form to fix");
    let failed = sheet_progress(&ws, cx).and_then(|p| p.failure().cloned());
    assert_eq!(failed.map(|f| f.title).as_deref(), Some("mini did not accept your SSH key"));
    click(cx, "ssh-install");
    assert_eq!(deployer.asked().len(), 1, "Try again runs it again");
    assert!(ws.read_with(cx, |ws, _| ws.workers.len()) == 1, "nothing added");
}

/// A machine whose host key this Mac's `ssh` does not know yet stops the install with the key's
/// fingerprint and an offer to trust it. The offer holds while the fields name that machine:
/// trusted, the key is kept and the install runs again.
#[gpui::test]
fn a_new_machine_s_key_is_offered_and_trusted_from_the_sheet(cx: &mut TestAppContext) {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (ws, cx) = shell(cx, &runtime, &dir, true);
    cx.simulate_resize(size(px(900.0), px(800.0)));
    let deployer = StandIn::new("mini");
    let shared: Rc<dyn Deployer> = Rc::<StandIn>::clone(&deployer);
    ws.update(cx, |ws, _cx| ws.deployer = Some(shared));
    cx.dispatch_action(actions::InstallOverSsh);
    cx.run_until_parked();
    type_host(&ws, cx, "mini");
    click(cx, "ssh-install");
    assert_eq!(deployer.asked(), ["deploy mini None None server=None"]);
    let key = HostKey {
        target: "mini".to_owned(),
        keys: vec![slopty_deploy::Fingerprint {
            kind: "ED25519".to_owned(),
            sha256: "SHA256:xaI/Xz1JNrchRfup".to_owned(),
        }],
        lines: "mini ssh-ed25519 AAAA".to_owned(),
        file: dir.path().join("known_hosts"),
    };
    deployer.end(cx, Err(key.offer()));
    assert!(cx.debug_bounds("install-failure").is_some(), "why it stopped");
    assert!(cx.debug_bounds("install-output").is_some(), "the fingerprint");
    let shown = sheet_progress(&ws, cx).and_then(|p| p.view().failed).expect("the failure");
    assert_eq!(shown.title, "mini is new to this Mac");
    assert_eq!(shown.lines, ["ED25519 SHA256:xaI/Xz1JNrchRfup"]);
    let offered = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, cx| {
            ws.adding.as_ref()?.ssh.as_ref()?.offered(cx).map(|k| k.target.clone())
        })
    };
    assert_eq!(offered(&ws, cx).as_deref(), Some("mini"));

    type_host(&ws, cx, "studio");
    assert_eq!(offered(&ws, cx), None, "not for another machine");
    type_host(&ws, cx, "mini");
    assert_eq!(offered(&ws, cx).as_deref(), Some("mini"));
    click(cx, "ssh-install");
    assert_eq!(*deployer.trusted.borrow(), ["mini"], "trusted as offered");
    assert_eq!(deployer.asked(), ["deploy mini None None server=None"], "and run again");
}

/// A tile has no fingerprint to show: a machine new to this Mac sends the person to the sheet.
#[test]
fn a_tile_sends_a_new_machine_s_key_to_the_sheet() {
    let key = HostKey {
        target: "mini".to_owned(),
        keys: Vec::new(),
        lines: String::new(),
        file: PathBuf::from("/nowhere/known_hosts"),
    };
    let said = on_a_tile(key.offer());
    assert_eq!(said.title, "mini's host key is not trusted yet");
    assert_eq!(
        said.hint.as_deref(),
        Some("Check and trust it from Install on a machine over SSH.")
    );
    assert!(said.trust.is_none());
    assert_eq!(on_a_tile(failure("other")), failure("other"));
}

/// A tile of a worker on a different build offers "Update" beside "Copy command": it deploys
/// with `--update` through the SSH target it was installed with, the pill follows each step
/// with its bar, and
/// once the deploy ends the worker is dialled again at once rather than after the long wait.
/// Answering as another build still, the update says so and offers another try; the link
/// coming up ends it.
#[gpui::test]
fn update_deploys_to_the_worker_then_dials_it_again(cx: &mut TestAppContext) {
    use slopty_client::update::{Of, UpdateNotice};
    use slopty_proto::items::{Item, ItemKind, ItemOp, ItemSync};
    use slopty_proto::terminal::{SessionState, SessionSummary};

    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace(cx, &runtime, &dir);
    let root = ws.clone();
    let (_root, cx) =
        cx.add_window_view(move |window, cx| gpui_kit::component::Root::new(root, window, cx));
    cx.simulate_resize(size(px(1200.0), px(800.0)));
    let notice = UpdateNotice {
        of: Of::Worker,
        host: "mini".to_owned(),
        peer: "0.0.9+wire.0badf00d".to_owned(),
    };
    let dials = Rc::new(std::cell::Cell::new(0_u32));
    let (counted, said) = (Rc::clone(&dials), notice.clone());
    let deployer = StandIn::new("mini");
    let shared: Rc<dyn Deployer> = Rc::<StandIn>::clone(&deployer);
    ws.update(cx, |ws, cx| {
        ws.dial = crate::Dialer(Rc::new(move |_id, _address, _cx| {
            counted.set(counted.get().saturating_add(1));
            gpui::Task::ready(Err(net::DialFailed::WrongBuild(said.clone())))
        }));
        ws.deployer = Some(shared);
        ws.publish_updates(cx);
    });
    let id = WorkerId::new();
    ws.update(cx, |ws, cx| ws.add_worker(id, "mini".to_owned(), true, cx));
    cx.run_until_parked();
    assert_eq!(dials.get(), 1);

    // A shell of it on the strip, as its last link left it.
    let key = crate::workers::worker_key(id);
    let session = slopty_core::SessionId::new();
    let item = Item {
        id: slopty_core::ItemId::new(),
        kind: ItemKind::Terminal { session },
        sleeping: false,
        name: None,
        facts: std::collections::BTreeMap::new(),
    };
    let item_id = item.id;
    let view = ws.read_with(cx, |ws, _| ws.view.clone());
    let (out, _sent) = mpsc::channel(64);
    view.update(cx, |v, cx| {
        let me = slopty_core::ClientId::new();
        #[expect(clippy::unreachable, reason = "a shell's tile opens no screen stream")]
        let open_screen: slopty_ui::screen::ScreenFactory =
            Arc::new(|_stream, _codec| unreachable!("a shell opens no screen"));
        let link = slopty_ui::workspace::WorkerLink { me, out, open_screen, remote: None };
        let hello = slopty_proto::handshake::HelloAck {
            worker: id,
            name: "mini".to_owned(),
            home: String::new(),
            caps: WorkerCaps::bare(WorkerOs::MacOs),
            load: 0.0,
            sessions: Vec::new(),
        };
        v.connect_worker(key, link, hello, cx);
        let summary = SessionSummary {
            id: session,
            title: "shell".into(),
            cwd: None,
            repo: None,
            branch: None,
            changes: None,
            started_ms: slopty_core::WallMs::ZERO,
            cols: 80,
            rows: 24,
            state: SessionState::Running,
            viewers: 1,
            command: Vec::new(),
            agent: None,
            progress: None,
            restored: None,
            repo_id: None,
        };
        v.session_opened(key, summary, cx);
        v.apply_sync(key, ItemSync::Delta { version: 1, by: me, op: ItemOp::Add(item) }, cx);
        v.disconnect_worker(key, WorkerStatus::NeedsUpdate(notice.clone()), cx);
    });
    cx.run_until_parked();
    let selector = |part: &str| -> &'static str {
        Box::leak(format!("{part}-{}", item_id.as_uuid()).into_boxed_str())
    };
    assert!(cx.debug_bounds(selector("copy-command")).is_some(), "the command stays, second");
    let installed =
        Target { user: Some("admin".to_owned()), port: Some(2222), ..Target::host("mini") };
    deployer.remember(id, &installed);
    click(cx, selector("update-worker"));
    assert_eq!(
        deployer.asked(),
        ["deploy mini Some(\"admin\") Some(2222) server=None"],
        "through the user and port it was installed with"
    );

    deployer.say(
        cx,
        [Event::Step(Step::Upload { name: "slopty-worker" }), Event::Sent { sent: 1, total: 4 }],
    );
    let run = cx.update(|_, cx| cx.global::<Updates>().runs.get("mini").cloned()).expect("a run");
    assert_eq!(run.current().map(|s| s.title.as_str()), Some("Copy Slopty"));
    assert!(cx.debug_bounds("update-progress").is_some(), "the bar in the pill");
    assert!(cx.debug_bounds(selector("update-worker")).is_none(), "no second update while it runs");

    deployer.end(cx, Ok(deployed()));
    assert_eq!(dials.get(), 2, "dialled again at once, not after the long wait");
    let run = cx.update(|_, cx| cx.global::<Updates>().runs.get("mini").cloned()).expect("kept");
    assert_eq!(
        run.failed.map(|f| f.title).as_deref(),
        Some("It still runs a different build"),
        "the same build answered"
    );
    assert!(cx.debug_bounds(selector("update-worker")).is_some(), "another try");

    ws.update(cx, |ws, cx| ws.update_linked(id, cx));
    let runs = cx.update(|_, cx| cx.global::<Updates>().runs.len());
    assert_eq!(runs, 0, "the link coming up ends it");
    ws.update(cx, |ws, cx| ws.drop_slot(id, cx));
    cx.run_until_parked();
}

/// The panel's and the sheet's fields, and the settings dialog, are heard for as long as they
/// are up: opening them again and again leaves nothing behind in the shell.
#[gpui::test]
fn reopening_the_panel_and_the_sheet_keeps_no_subscription(cx: &mut TestAppContext) {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (ws, cx) = shell(cx, &runtime, &dir, true);
    let count = |cx: &mut VisualTestContext| ws.read_with(cx, |ws, _| ws.subscriptions.len());
    let deployer: Rc<dyn Deployer> = StandIn::new("mini.tail1234.ts.net");
    ws.update(cx, |ws, _cx| ws.deployer = Some(deployer));
    let before = count(cx);
    for _ in 0..3 {
        cx.update(|window, cx| {
            ws.update(cx, |ws, cx| {
                ws.open_ssh(window, cx);
                ws.leave_ssh(cx);
                ws.cancel_add_worker(window, cx);
                ws.show_add_worker(crate::Panel::Worker, window, cx);
                ws.open_settings(window, cx);
                ws.close_settings(window, cx);
            });
        });
        cx.run_until_parked();
    }
    assert_eq!(count(cx), before, "nothing left behind");
}

/// On the server panel the SSH entry sets up the server: its steps have no doctor to read, and
/// once it is up it is connected to at each address it was reached at in turn; when none
/// answers, the sheet says so with what the last said. This Mac's row runs the same steps here,
/// with nothing to type.
#[gpui::test]
fn the_server_panel_sets_up_the_server(cx: &mut TestAppContext) {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (ws, cx) = shell(cx, &runtime, &dir, true);
    cx.simulate_resize(size(px(900.0), px(800.0)));
    let deployer = StandIn::new("mini");
    let shared: Rc<dyn Deployer> = Rc::<StandIn>::clone(&deployer);
    ws.update(cx, |ws, _cx| ws.deployer = Some(shared));
    cx.update(|window, cx| {
        ws.update(cx, |ws, cx| ws.show_add_worker(crate::Panel::Server, window, cx));
    });
    cx.run_until_parked();
    let rows = ws.update(cx, |ws, cx| (ws.ssh_row(cx).is_some(), ws.serve_here_row(cx).is_some()));
    assert_eq!(rows, (true, true), "both ways to set it up");
    cx.update(|window, cx| ws.update(cx, |ws, cx| ws.open_ssh_at("mini", window, cx)));
    cx.run_until_parked();
    let heading = ws.read_with(cx, |ws, _| ws.adding.as_ref()?.ssh.as_ref().map(Sheet::heading));
    assert_eq!(heading, Some(SERVE_HEADING));
    click(cx, "ssh-install");
    assert_eq!(deployer.asked(), ["serve mini"]);
    let titles: Vec<String> =
        sheet_progress(&ws, cx).expect("a run").view().steps.into_iter().map(|s| s.title).collect();
    assert_eq!(
        titles,
        ["Connect to mini", "Copy the server", "Install and start it", "Connect to it"]
    );

    let finish = deployer.served.borrow_mut().take().expect("a server run");
    let platform = Platform { os: Os::Linux, arch: Arch::X86_64 };
    let addresses = vec!["100.64.0.9".to_owned(), "mini".to_owned()];
    finish.send(Ok(Served { platform, addresses })).unwrap();
    cx.run_until_parked();
    assert_eq!(deployer.asked(), ["link 100.64.0.9:45560", "link mini:45560"], "each in turn");
    let failed = sheet_progress(&ws, cx).and_then(|p| p.failure().cloned()).expect("none answered");
    assert_eq!(failed.title, "The server runs on mini, and Slopty could not connect to it");
    assert_eq!(failed.lines, ["nothing answered at mini:45560"]);

    cx.update(|window, cx| ws.update(cx, |ws, cx| ws.serve_here(window, cx)));
    cx.run_until_parked();
    assert_eq!(deployer.asked(), ["serve 127.0.0.1"], "this Mac, with nothing typed");
    let first =
        sheet_progress(&ws, cx).and_then(|p| p.view().steps.first().map(|s| s.title.clone()));
    assert_eq!(first.as_deref(), Some("Look at this Mac"));
}

/// A server on another build is brought to this one from the palette with no sheet: through the
/// SSH target it was set up with, then linked again at the address in use first and then where
/// the deploy reached it. The status bar says it runs, and a failure says why in a notice and
/// puts the status back.
#[gpui::test]
fn the_palette_updates_a_server_on_another_build(cx: &mut TestAppContext) {
    use slopty_client::update::{Of, UpdateNotice};
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (ws, cx) = shell(cx, &runtime, &dir, true);
    let deployer = StandIn::new("mini");
    let shared: Rc<dyn Deployer> = Rc::<StandIn>::clone(&deployer);
    let address = HostAddr::new("hub", SERVER_PORT);
    ws.update(cx, |ws, _cx| ws.deployer = Some(shared));
    let toast =
        |cx: &mut VisualTestContext| ws.read_with(cx, |ws, cx| ws.view.read(cx).toast_text());
    let status = |cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, cx| ws.view.read(cx).server_status().map(str::to_owned))
    };

    cx.dispatch_action(UpdateServer);
    assert_eq!(toast(cx).as_deref(), Some("No server is set"));
    ws.update(cx, |ws, _cx| ws.server = Some(crate::server::ServerSlot::stand_in(address.clone())));
    let notice = UpdateNotice { of: Of::Server, host: "hub".to_owned(), peer: "0.0.9".to_owned() };
    ws.update(cx, |ws, cx| {
        ws.server_event(slopty_client::server::ServerEvent::WrongBuild(notice), cx);
    });
    cx.run_until_parked();
    assert_eq!(status(cx).as_deref(), Some(crate::server::OTHER_BUILD), "not \"unreachable\"");
    let said = toast(cx).unwrap_or_default();
    assert!(
        said.contains("The server runs a different build")
            && said.contains(crate::server::UPDATE_SERVER),
        "{said}"
    );

    deployer.remember_server(
        &address,
        &Target { user: Some("admin".to_owned()), ..Target::host("hub") },
    );
    cx.dispatch_action(UpdateServer);
    assert_eq!(deployer.asked(), ["serve admin@hub"], "through the target it was set up with");
    assert_eq!(status(cx).as_deref(), Some(UPDATING_SERVER));
    cx.dispatch_action(UpdateServer);
    assert!(deployer.asked().is_empty(), "one update at a time");

    let finish = deployer.served.borrow_mut().take().expect("a server run");
    let platform = Platform { os: Os::Linux, arch: Arch::X86_64 };
    finish.send(Ok(Served { platform, addresses: vec!["100.64.0.9".to_owned()] })).unwrap();
    cx.run_until_parked();
    assert_eq!(
        deployer.asked(),
        [format!("link {address}"), "link 100.64.0.9:45560".to_owned()],
        "the address in use first"
    );
    let said = toast(cx).unwrap_or_default();
    assert!(
        said.starts_with("The server on hub runs this build, and Slopty could not connect"),
        "{said}"
    );
    assert_eq!(status(cx).as_deref(), Some(crate::server::OTHER_BUILD), "back to what it is");

    // A host key this Mac does not know sends the person to the sheet that shows it.
    cx.dispatch_action(UpdateServer);
    let _asked = deployer.asked();
    let finish = deployer.served.borrow_mut().take().expect("a server run");
    let key = HostKey {
        target: "hub".to_owned(),
        keys: Vec::new(),
        lines: String::new(),
        file: PathBuf::new(),
    };
    let mut unknown = failure("unknown key");
    unknown.trust = Some(Box::new(key));
    finish.send(Err(unknown)).unwrap();
    cx.run_until_parked();
    assert_eq!(
        toast(cx).as_deref(),
        Some(
            format!("hub's host key is not trusted yet. Check and trust it from {SERVE_TITLE}.")
                .as_str()
        )
    );
}

/// "Update all workers" updates each worker that answered on another build, each as its tile
/// would; with none, it says so. A worker on this Mac on another build is brought to this build
/// unasked, once, and only by an installed app.
#[gpui::test]
fn every_worker_on_another_build_is_updated_and_this_mac_s_unasked(cx: &mut TestAppContext) {
    use slopty_client::update::{Of, UpdateNotice};
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (ws, cx) = shell(cx, &runtime, &dir, true);
    let deployer = StandIn::new("mini");
    let shared: Rc<dyn Deployer> = Rc::<StandIn>::clone(&deployer);
    ws.update(cx, |ws, _cx| ws.deployer = Some(shared));
    let toast =
        |cx: &mut VisualTestContext| ws.read_with(cx, |ws, cx| ws.view.read(cx).toast_text());
    cx.dispatch_action(UpdateAllWorkers);
    assert_eq!(toast(cx).as_deref(), Some("Every machine that answers runs this build"));

    let notice =
        |host: &str| UpdateNotice { of: Of::Worker, host: host.to_owned(), peer: String::new() };
    let view = ws.read_with(cx, |ws, _| ws.view.clone());
    for host in ["mini", "box"] {
        let id = WorkerId::new();
        ws.update(cx, |ws, cx| ws.add_worker(id, host.to_owned(), true, cx));
        let key = crate::workers::worker_key(id);
        view.update(cx, |v, cx| {
            v.set_worker_status(key, WorkerStatus::NeedsUpdate(notice(host)), cx);
        });
    }
    cx.run_until_parked();
    let _dials = deployer.asked();
    cx.dispatch_action(UpdateAllWorkers);
    let mut asked = deployer.asked();
    asked.sort();
    assert_eq!(asked, ["deploy box None None server=None", "deploy mini None None server=None"]);
    assert_eq!(toast(cx).as_deref(), Some("Updating 2 machines"));

    let here = WorkerId::new();
    let local = notice("127.0.0.1");
    ws.update(cx, |ws, cx| ws.heard_other_build(here, &local, cx));
    assert!(deployer.asked().is_empty(), "a build from a source tree leaves it alone");
    deployer.installed.set(true);
    ws.update(cx, |ws, cx| ws.heard_other_build(here, &local, cx));
    assert_eq!(deployer.asked(), ["deploy 127.0.0.1 None None server=None"], "in place, unasked");
    ws.update(cx, |ws, cx| {
        if let Some(run) = ws.updates.get_mut("127.0.0.1") {
            run.task = None;
        }
        ws.heard_other_build(here, &local, cx);
    });
    assert!(deployer.asked().is_empty(), "once a launch: a failed one waits for the person");
}

/// An update that would end a machine's sessions stops before changing anything and says so.
/// Pressing the tile's Update again is the person's yes, and only that press carries it: the
/// palette's "update all" and the update this Mac runs unasked never do.
#[gpui::test]
fn an_update_that_ends_sessions_goes_on_only_when_pressed_again(cx: &mut TestAppContext) {
    use slopty_client::update::{Of, UpdateNotice};
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (ws, cx) = shell(cx, &runtime, &dir, true);
    let deployer = StandIn::new("mini");
    let shared: Rc<dyn Deployer> = Rc::<StandIn>::clone(&deployer);
    ws.update(cx, |ws, _cx| ws.deployer = Some(shared));
    let id = WorkerId::new();
    let notice = UpdateNotice { of: Of::Worker, host: "mini".to_owned(), peer: String::new() };
    let view = ws.read_with(cx, |ws, _| ws.view.clone());
    ws.update(cx, |ws, cx| ws.add_worker(id, "mini".to_owned(), true, cx));
    let key = crate::workers::worker_key(id);
    view.update(cx, |v, cx| v.set_worker_status(key, WorkerStatus::NeedsUpdate(notice), cx));
    cx.run_until_parked();
    let _dials = deployer.asked();
    let ends = || {
        let mut said = failure("Updating ends 3 sessions on mini");
        said.ends_sessions = Some(slopty_deploy::Ptyd::Restarts { sessions: Some(3) });
        said
    };

    ws.update(cx, |ws, cx| ws.update_worker("mini", cx));
    assert_eq!(deployer.asked(), ["deploy mini None None server=None"], "asked nothing yet");
    deployer.end(cx, Err(ends()));
    let run = cx.update(|_, cx| cx.global::<Updates>().runs.get("mini").cloned()).expect("kept");
    assert_eq!(run.failed.map(|f| f.title).as_deref(), Some("Updating ends 3 sessions on mini"));

    cx.dispatch_action(UpdateAllWorkers);
    assert_eq!(deployer.asked(), ["deploy mini None None server=None"], "not a yes for this one");
    deployer.end(cx, Err(ends()));

    ws.update(cx, |ws, cx| ws.update_worker("mini", cx));
    assert_eq!(deployer.asked(), ["deploy mini None None server=None end_sessions"]);
    deployer.end(cx, Err(failure("ssh: connect to host mini port 22: Connection refused")));
    ws.update(cx, |ws, cx| ws.update_worker("mini", cx));
    assert_eq!(
        deployer.asked(),
        ["deploy mini None None server=None"],
        "a yes holds for the press after the question, not every one after"
    );
}

/// Machines installed from here before there was a server are not in its directory, so its
/// other clients never list them: once its directory comes, a notice says how many, and the
/// palette's line installs this build on each again, registered with it. One the directory
/// lists, or one not installed from here, is left alone.
#[gpui::test]
fn machines_added_before_the_server_are_registered_with_it(cx: &mut TestAppContext) {
    use slopty_proto::server::{FromServer, Liveness, WorkerInfo};
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (ws, cx) = shell(cx, &runtime, &dir, true);
    let deployer = StandIn::new("mini");
    let shared: Rc<dyn Deployer> = Rc::<StandIn>::clone(&deployer);
    ws.update(cx, |ws, _cx| ws.deployer = Some(shared));
    let toast =
        |cx: &mut VisualTestContext| ws.read_with(cx, |ws, cx| ws.view.read(cx).toast_text());
    let [mini, listed, typed] = [WorkerId::new(), WorkerId::new(), WorkerId::new()];
    for (id, name) in [(mini, "mini"), (listed, "box"), (typed, "typed")] {
        ws.update(cx, |ws, cx| ws.add_worker(id, name.to_owned(), true, cx));
    }
    deployer.remember(mini, &Target::host("mini"));
    deployer.remember(listed, &Target::host("box"));
    cx.run_until_parked();
    let _dials = deployer.asked();

    cx.dispatch_action(RegisterWithServer);
    assert_eq!(toast(cx).as_deref(), Some("No server is set"));
    let address = HostAddr::new("hub", SERVER_PORT);
    ws.update(cx, |ws, _cx| ws.server = Some(crate::server::ServerSlot::stand_in(address)));
    let info = WorkerInfo {
        worker: listed,
        name: "box".to_owned(),
        address: "100.64.0.2:45550".to_owned(),
        liveness: Liveness::Online,
        caps: WorkerCaps::bare(WorkerOs::MacOs),
        load: 0.0,
        last_seen_ms: slopty_core::WallMs::ZERO,
    };
    let listing = FromServer::Directory(vec![info]);
    ws.update(cx, |ws, cx| {
        ws.server_event(slopty_client::server::ServerEvent::Message(Box::new(listing)), cx);
    });
    cx.run_until_parked();
    assert_eq!(
        toast(cx).as_deref(),
        Some(
            "1 machine added here is not on this server. Run \u{201c}Register machines with the server\u{201d} from the palette."
        )
    );

    cx.dispatch_action(RegisterWithServer);
    assert_eq!(deployer.asked(), ["deploy mini None None server=Some(\"hub:45560\")"]);
    assert_eq!(toast(cx).as_deref(), Some("Registering 1 machine with the server"));
}

/// The toast on an add says what may keep the machine from running for good: nobody logged
/// in, a disk that waits for its password after a restart, a Linux user that does not
/// linger, and what became of the key, each once, after the plain news.
#[test]
fn an_add_says_what_keeps_the_machine_from_running_for_good() {
    assert_eq!(added_notice("mini", &deployed()), "Added mini");
    let mut headless = deployed();
    headless.console = slopty_deploy::Console { logged_in: Some(false), filevault: Some(true) };
    headless.key = Some(slopty_deploy::Key::NoPublicKey);
    assert_eq!(
        added_notice("mini", &headless),
        format!(
            "Added mini. mini runs Slopty once someone is logged in there. After a restart it waits \
             for its disk password before anyone can log in. {NO_KEY}."
        )
    );
    let mut linux = deployed();
    linux.platform = Platform { os: Os::Linux, arch: Arch::X86_64 };
    linux.stops_at_logout = Some("Run `loginctl enable-linger me` there.".to_owned());
    linux.key = Some(slopty_deploy::Key::Added);
    assert_eq!(
        added_notice("box", &linux),
        "Added box. Its shells stop when you log out there: Run `loginctl enable-linger me` there. \
         Your key is on it now, so it will not ask for a password again."
    );
}

/// A machine that takes only a password stops the install with one masked field for it, the
/// show toggle and the offer to add the person's key; "Sign in and install" hands the typed
/// password to that one run and empties the field, and secure event input is on only while
/// the field has the keyboard. Another machine named in the fields is not asked for.
#[gpui::test]
fn a_password_only_machine_takes_the_password_once_and_the_key(cx: &mut TestAppContext) {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (ws, cx) = shell(cx, &runtime, &dir, true);
    cx.simulate_resize(size(px(900.0), px(800.0)));
    let deployer = StandIn::new("mini");
    let shared: Rc<dyn Deployer> = Rc::<StandIn>::clone(&deployer);
    ws.update(cx, |ws, _cx| ws.deployer = Some(shared));
    cx.dispatch_action(actions::InstallOverSsh);
    cx.run_until_parked();
    type_host(&ws, cx, "mini");
    click(cx, "ssh-install");
    assert_eq!(deployer.asked(), ["deploy mini None None server=None"]);
    assert!(cx.debug_bounds("ssh-password").is_none(), "no field before it asks");
    let mut asks = failure("mini asks for me's password");
    asks.password = Some(Box::new(slopty_deploy::PasswordAsk {
        user: "me".to_owned(),
        host: "mini".to_owned(),
        refused: false,
    }));
    deployer.end(cx, Err(asks));
    assert!(cx.debug_bounds("ssh-password").is_some(), "the field");
    let sheet = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, _| {
            ws.adding.as_ref().and_then(|a| a.ssh.as_ref()).map(|s| s.password.clone())
        })
    };
    let field = sheet(&ws, cx).expect("the sheet");
    let secure = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, _| {
            ws.adding.as_ref().and_then(|a| a.ssh.as_ref()).is_some_and(|s| s.secure.0.is_on())
        })
    };
    cx.update(|window, _cx| window.activate_window());
    click(cx, "ssh-password");
    let focused = cx
        .update(|window, cx| gpui::Focusable::focus_handle(field.read(cx), cx).is_focused(window));
    assert!(focused, "the click gives it the keyboard");
    assert!(secure(&ws, cx), "secure event input while it has the keyboard");
    cx.update(|window, cx| field.update(cx, |input, cx| input.set_value("hunter2", window, cx)));
    let tree = {
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        cx.update(|window, _cx| slopty_ui::a11y::tree(window))
    };
    assert!(tree.iter().any(|n| n.is("Button", Some("Sign in and install"))), "{tree:#?}");
    assert!(!tree.iter().any(|n| n.value.as_deref() == Some("hunter2")), "never read out");
    let key = tree.iter().find(|n| n.is("CheckBox", Some("Add your key so mini stops asking")));
    assert!(key.is_some(), "the offer: {tree:#?}");

    type_host(&ws, cx, "studio");
    assert!(cx.debug_bounds("ssh-password").is_none(), "not for another machine");
    type_host(&ws, cx, "mini");
    click(cx, "ssh-install");
    assert_eq!(deployer.asked(), ["deploy mini None None server=None password add_key"]);
    assert_eq!(deployer.password.borrow().as_deref(), Some("hunter2"), "handed to the run");
    let left = cx.update(|_, cx| field.read(cx).value().to_string());
    assert_eq!(left, "", "not kept in the field");
    assert!(!secure(&ws, cx), "off once the field lets go");

    let mut refused = failure("mini did not take that password");
    refused.password = Some(Box::new(slopty_deploy::PasswordAsk {
        user: "me".to_owned(),
        host: "mini".to_owned(),
        refused: true,
    }));
    deployer.end(cx, Err(refused));
    click(cx, "ssh-install");
    assert!(deployer.asked().is_empty(), "nothing typed, nothing run");
    let said = ws.read_with(cx, |ws, _| ws.adding.as_ref()?.ssh.as_ref()?.note.clone());
    assert_eq!(said, Some(("Type the password first".to_owned(), true)));
    click(cx, "ssh-add-key");
    cx.update(|window, cx| field.update(cx, |input, cx| input.set_value("hunter3", window, cx)));
    click(cx, "ssh-install");
    assert_eq!(
        deployer.asked(),
        ["deploy mini None None server=None password"],
        "no key this time"
    );
}
