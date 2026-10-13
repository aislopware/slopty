//! Add device's install over SSH and a Devices row's "Update" driven through a stand-in
//! [`Deployer`] the test plays: it hands the test the deploy's event sender and its ending, so each
//! step is shown as the test says it happened, and no machine is reached.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{Entity, TestAppContext, VisualTestContext};
use slopty_deploy::{Arch, Os, Platform};
use slopty_net::endpoint::SERVER_PORT;
use slopty_proto::ctl::{Health, PasteboardAccess, Tailscale};
use slopty_proto::server::{Os as WorkerOs, WorkerCaps};
use slopty_ui::workspace::WorkerStatus;
use tokio::sync::oneshot;

use super::*;
use crate::tests::{list, shell, workspace};

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
    /// The servers' targets it was told to keep, by address.
    servers: RefCell<HashMap<String, Target>>,
    /// It plays an installed app, which brings its own Mac's daemons to its build.
    installed: std::cell::Cell<bool>,
    /// The ending of the removal under way.
    removing: RefCell<Option<oneshot::Sender<Result<slopty_deploy::Removed, Failure>>>>,
}

impl StandIn {
    fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    fn asked(&self) -> Vec<String> {
        std::mem::take(&mut *self.asked.borrow_mut())
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
        server: Server,
        said: Said,
        events: mpsc::UnboundedSender<Event>,
    ) -> Pending<Result<Deployed, Failure>> {
        let Target { host, user, port } = to;
        let server = format!("{}:{}", server.host, server.port);
        let ends = if said.end_sessions { " end_sessions" } else { "" };
        let signs = if said.password.is_some() { " password" } else { "" };
        let key = if said.add_key { " add_key" } else { "" };
        *self.password.borrow_mut() = said
            .password
            .as_ref()
            .map(|p| slopty_deploy::ExposeSecret::expose_secret(p).to_owned());
        self.asked
            .borrow_mut()
            .push(format!("deploy {host} {user:?} {port:?} server={server}{ends}{signs}{key}"));
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

    fn remove(
        &self,
        to: &Target,
        events: mpsc::UnboundedSender<Event>,
    ) -> Pending<Result<slopty_deploy::Removed, Failure>> {
        let Target { host, user, port } = to;
        self.asked.borrow_mut().push(format!("remove {host} {user:?} {port:?}"));
        *self.events.borrow_mut() = Some(events);
        let (tx, rx) = oneshot::channel();
        *self.removing.borrow_mut() = Some(tx);
        Box::pin(async move { rx.await.unwrap_or_else(|_| Err(failure("the test let go"))) })
    }
}

impl StandIn {
    fn removed(&self, cx: &VisualTestContext, done: Result<slopty_deploy::Removed, Failure>) {
        let finish = self.removing.borrow_mut().take().expect("a removal runs");
        finish.send(done).unwrap();
        cx.run_until_parked();
    }
}

fn failure(title: &str) -> Failure {
    Failure::new(title.to_owned(), None, Vec::new())
}

fn deployed() -> Deployed {
    Deployed {
        platform: Platform { os: Os::MacOs, arch: Arch::Arm64 },
        health: Health {
            worker: WorkerId::nil(),
            server: None,
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
            turns: 0,
            uptime_secs: 1,
        },
        server: "hub:45560".to_owned(),
        ptyd: None,
        stops_at_logout: None,
        console: slopty_deploy::Console::default(),
        key: None,
    }
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
    assert!(progress.failure().is_some());
    assert_eq!(
        Progress::new("mini".to_owned(), Kind::Update)
            .view()
            .steps
            .last()
            .map(|s| s.title.as_str()),
        Some("Reconnect")
    );
}

/// A toast has no fingerprint to show and no password field: a machine new to this Mac, or one
/// that takes only a password, sends the person to the Add device dialog, keeping what it
/// stopped at.
#[test]
fn a_toast_sends_a_new_machine_s_key_to_the_dialog() {
    let key = HostKey {
        target: "mini".to_owned(),
        keys: Vec::new(),
        lines: String::new(),
        file: PathBuf::from("/nowhere/known_hosts"),
    };
    let said = in_a_toast(key.offer());
    assert_eq!(said.title, "mini's host key is not trusted yet");
    assert_eq!(
        said.hint.as_deref(),
        Some("Check and trust it in Settings \u{2192} Devices \u{2192} Add device.")
    );
    assert!(said.trust.is_some(), "kept for the dialog");
    let mut asks = failure("Permission denied (publickey,password)");
    asks.password = Some(Box::new(slopty_deploy::PasswordAsk {
        user: "me".to_owned(),
        host: "mini".to_owned(),
        refused: false,
    }));
    let said = in_a_toast(asks);
    assert_eq!(said.title, "mini asks for a password");
    assert_eq!(
        said.hint.as_deref(),
        Some("Sign in with it in Settings \u{2192} Devices \u{2192} Add device.")
    );
    assert!(said.password.is_some());
    assert_eq!(in_a_toast(failure("other")), failure("other"));
}

/// A server on another build is brought to this one from the palette with no sheet: through the
/// SSH target it was set up with, then linked again at the address in use first and then where
/// the deploy reached it. The cluster says it runs, and a failure says why in a notice and
/// puts the status back.
#[gpui::test]
fn the_server_s_row_updates_a_server_on_another_build(cx: &mut TestAppContext) {
    use slopty_client::update::{Of, UpdateNotice};
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (ws, cx) = shell(cx, &runtime, &dir, true);
    let deployer = StandIn::new();
    let shared: Rc<dyn Deployer> = Rc::<StandIn>::clone(&deployer);
    let address = HostAddr::new("hub", SERVER_PORT);
    ws.update(cx, |ws, _cx| ws.deployer = Some(shared));
    let toast = |cx: &mut VisualTestContext| {
        ws.read_with(cx, |_, cx| slopty_shell::toast::messages(cx).last().map(ToString::to_string))
    };
    let status = |cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, cx| ws.view.read(cx).server_status().map(str::to_owned))
    };

    ws.update(cx, |ws, _cx| ws.server = None);
    ws.update(cx, |ws, cx| ws.device_action(None, "update", cx));
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
            && said.contains(crate::server::UPDATE_HOW),
        "{said}"
    );

    deployer.remember_server(
        &address,
        &Target { user: Some("admin".to_owned()), ..Target::host("hub") },
    );
    ws.update(cx, |ws, cx| ws.device_action(None, "update", cx));
    assert_eq!(deployer.asked(), ["serve admin@hub"], "through the target it was set up with");
    assert_eq!(status(cx).as_deref(), Some(UPDATING_SERVER));
    ws.update(cx, |ws, cx| ws.device_action(None, "update", cx));
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
    ws.update(cx, |ws, cx| ws.device_action(None, "update", cx));
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
            format!("hub's host key is not trusted yet. Check and trust it in {ADD_DEVICE}.")
                .as_str()
        )
    );
}

/// "Update all workers" updates each worker that answered on another build, each as its row
/// would; with none, it says so. A worker on this Mac on another build is brought to this build
/// unasked, once, and only by an installed app.
#[gpui::test]
fn every_worker_on_another_build_is_updated_and_this_mac_s_unasked(cx: &mut TestAppContext) {
    use slopty_client::update::{Of, UpdateNotice};
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (ws, cx) = shell(cx, &runtime, &dir, true);
    let deployer = StandIn::new();
    let shared: Rc<dyn Deployer> = Rc::<StandIn>::clone(&deployer);
    ws.update(cx, |ws, _cx| ws.deployer = Some(shared));
    let toast = |cx: &mut VisualTestContext| {
        ws.read_with(cx, |_, cx| slopty_shell::toast::messages(cx).last().map(ToString::to_string))
    };
    ws.update(cx, |ws, cx| ws.device_action(None, "update-all", cx));
    assert_eq!(toast(cx).as_deref(), Some("Every machine runs this build"));

    let notice =
        |host: &str| UpdateNotice { of: Of::Worker, host: host.to_owned(), peer: String::new() };
    let view = ws.read_with(cx, |ws, _| ws.view.clone());
    for host in ["mini", "box"] {
        let id = WorkerId::new();
        list(&ws, cx, id, host);
        let key = crate::workers::worker_key(id);
        view.update(cx, |v, cx| {
            v.set_worker_status(key, WorkerStatus::NeedsUpdate(notice(host)), cx);
        });
    }
    cx.run_until_parked();
    let _dials = deployer.asked();
    ws.update(cx, |ws, cx| ws.device_action(None, "update-all", cx));
    let mut asked = deployer.asked();
    asked.sort();
    assert_eq!(
        asked,
        ["deploy box None None server=hub:45560", "deploy mini None None server=hub:45560"]
    );
    assert_eq!(toast(cx).as_deref(), Some("Updating 2 machines"));

    let here = WorkerId::new();
    let local = notice("127.0.0.1");
    ws.update(cx, |ws, cx| ws.heard_other_build(here, &local, cx));
    assert!(deployer.asked().is_empty(), "a build from a source tree leaves it alone");
    deployer.installed.set(true);
    ws.update(cx, |ws, cx| ws.heard_other_build(here, &local, cx));
    assert_eq!(
        deployer.asked(),
        ["deploy 127.0.0.1 None None server=hub:45560"],
        "in place, unasked"
    );
    ws.update(cx, |ws, cx| {
        if let Some(run) = ws.updates.get_mut("127.0.0.1") {
            run.task = None;
        }
        ws.heard_other_build(here, &local, cx);
    });
    assert!(deployer.asked().is_empty(), "once a launch: a failed one waits for the person");
}

/// An update that would end a machine's sessions stops before changing anything and says so.
/// Pressing the row's Update again is the person's yes, and only that press carries it: "Update
/// all" and the update this Mac runs unasked never do.
#[gpui::test]
fn an_update_that_ends_sessions_goes_on_only_when_pressed_again(cx: &mut TestAppContext) {
    use slopty_client::update::{Of, UpdateNotice};
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (ws, cx) = shell(cx, &runtime, &dir, true);
    let deployer = StandIn::new();
    let shared: Rc<dyn Deployer> = Rc::<StandIn>::clone(&deployer);
    ws.update(cx, |ws, _cx| ws.deployer = Some(shared));
    let id = WorkerId::new();
    let notice = UpdateNotice { of: Of::Worker, host: "mini".to_owned(), peer: String::new() };
    let view = ws.read_with(cx, |ws, _| ws.view.clone());
    list(&ws, cx, id, "mini");
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
    assert_eq!(deployer.asked(), ["deploy mini None None server=hub:45560"], "asked nothing yet");
    deployer.end(cx, Err(ends()));
    let failed = ws.read_with(cx, |ws, _| {
        ws.updates.get("mini").and_then(|run| run.progress.failure().map(|f| f.title.clone()))
    });
    assert_eq!(failed.as_deref(), Some("Updating ends 3 sessions on mini"));

    ws.update(cx, |ws, cx| ws.device_action(None, "update-all", cx));
    assert_eq!(
        deployer.asked(),
        ["deploy mini None None server=hub:45560"],
        "not a yes for this one"
    );
    deployer.end(cx, Err(ends()));

    ws.update(cx, |ws, cx| ws.update_worker("mini", cx));
    assert_eq!(deployer.asked(), ["deploy mini None None server=hub:45560 end_sessions"]);
    deployer.end(cx, Err(failure("ssh: connect to host mini port 22: Connection refused")));
    ws.update(cx, |ws, cx| ws.update_worker("mini", cx));
    assert_eq!(
        deployer.asked(),
        ["deploy mini None None server=hub:45560"],
        "a yes holds for the press after the question, not every one after"
    );
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

/// A machine's removal reaches it the way it was installed, takes the worker off there, and
/// only once the server lists that worker as away has the server forget it
/// (`Verb::ForgetWorker`). A removal that fails there says so and forgets nothing.
#[gpui::test]
fn a_removal_takes_the_worker_off_then_the_server_forgets_it(cx: &mut TestAppContext) {
    use slopty_proto::orchestration::{Outcome, Verb};
    use slopty_proto::server::{FromServer, Liveness, WorkerInfo};

    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace(cx, &runtime, &dir);
    let cx = cx.add_empty_window();
    let deployer = StandIn::new();
    let shared: Rc<dyn Deployer> = Rc::<StandIn>::clone(&deployer);
    let (slot, mut verbs) =
        crate::server::ServerSlot::answered_by(HostAddr::new("hub", SERVER_PORT));
    ws.update(cx, |ws, _cx| {
        ws.deployer = Some(shared);
        ws.server = Some(slot);
    });
    let (mini, other) = (WorkerId::new(), WorkerId::new());
    list(&ws, cx, mini, "mini");
    list(&ws, cx, other, "other");
    let installed = Target { user: Some("admin".to_owned()), ..Target::host("mini") };
    deployer.remember(mini, &installed);
    let key = crate::workers::worker_key(mini);
    let offered = |cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, cx| {
            ws.machine_actions(key, cx).into_iter().map(|a| a.label).collect::<Vec<_>>()
        })
    };
    assert!(offered(cx).contains(&"Remove".to_owned()), "a Mac's app offers the removal");
    ws.update(cx, |ws, cx| ws.device_action(Some(key), "ask-remove", cx));
    assert!(deployer.asked().is_empty(), "the first press only asks");
    assert!(offered(cx).contains(&"Remove now".to_owned()), "{:?}", offered(cx));
    let notice = |cx: &mut VisualTestContext| {
        ws.read_with(cx, |_, cx| slopty_shell::toast::messages(cx).last().map(ToString::to_string))
    };
    let away = |id: WorkerId, name: &str, cx: &mut VisualTestContext| {
        let info = WorkerInfo {
            worker: id,
            name: name.to_owned(),
            address: "100.64.0.2:45550".to_owned(),
            liveness: Liveness::Unreachable,
            caps: WorkerCaps::bare(WorkerOs::MacOs),
            load: 0.0,
            last_seen_ms: slopty_core::WallMs::ZERO,
        };
        let said = slopty_client::server::ServerEvent::Message(Box::new(FromServer::Worker(info)));
        ws.update(cx, |ws, cx| ws.server_event(said, cx));
        cx.run_until_parked();
    };

    ws.update(cx, |ws, cx| ws.device_action(Some(key), "remove", cx));
    cx.run_until_parked();
    assert_eq!(deployer.asked(), ["remove mini Some(\"admin\") None"], "as it was installed");
    assert_eq!(notice(cx).as_deref(), Some("Removing mini\u{2026}"));
    deployer.removed(cx, Ok(slopty_deploy::Removed::default()));
    assert!(verbs.try_next().is_none(), "nothing forgotten while the server lists it online");
    away(other, "other", cx);
    runtime.block_on(tokio::task::yield_now());
    assert!(verbs.try_next().is_none(), "another machine going away is not this one");

    // The verb goes up from the networking runtime, which this test turns by hand.
    let turn = |cx: &mut VisualTestContext| {
        runtime.block_on(tokio::task::yield_now());
        cx.run_until_parked();
    };
    away(mini, "mini", cx);
    turn(cx);
    let (verb, answer) = verbs.try_next().expect("the server is asked to forget it");
    assert_eq!(verb, Verb::ForgetWorker { worker: mini });
    answer.send(Outcome::Done).unwrap();
    turn(cx);
    assert_eq!(notice(cx).as_deref(), Some("Forgot mini"));
    assert!(!ws.read_with(cx, |ws, _| ws.removing(mini)), "done");

    ws.update(cx, |ws, cx| ws.remove_worker(other, cx));
    cx.run_until_parked();
    deployer.removed(cx, Err(failure("Could not reach other")));
    turn(cx);
    assert!(verbs.try_next().is_none(), "a failed removal forgets nothing");
    let said = notice(cx).unwrap_or_default();
    assert!(said.starts_with("Could not remove other: Could not reach other"), "{said}");
}

/// A machine on a newer build is never updated from here, by its row or by "Update all": this
/// device is the one to update. One whose build cannot be told newer or older is updated on the
/// first press of its row, and "Update all" leaves it to that row.
#[gpui::test]
fn an_update_never_takes_a_machine_back_and_goes_when_it_cannot_tell(cx: &mut TestAppContext) {
    use slopty_client::update::{Of, UpdateNotice};
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (ws, cx) = shell(cx, &runtime, &dir, true);
    let deployer = StandIn::new();
    let shared: Rc<dyn Deployer> = Rc::<StandIn>::clone(&deployer);
    ws.update(cx, |ws, _cx| ws.deployer = Some(shared));
    let id = WorkerId::new();
    let view = ws.read_with(cx, |ws, _| ws.view.clone());
    list(&ws, cx, id, "mini");
    let key = crate::workers::worker_key(id);
    let notice =
        |peer: &str| UpdateNotice { of: Of::Worker, host: "mini".into(), peer: peer.into() };
    let set = |cx: &mut VisualTestContext, notice: UpdateNotice| {
        view.update(cx, |v, cx| v.set_worker_status(key, WorkerStatus::NeedsUpdate(notice), cx));
        cx.run_until_parked();
    };
    let _dials = deployer.asked();

    set(cx, notice("99.0.0+wire.0badf00d"));
    ws.update(cx, |ws, cx| ws.update_worker("mini", cx));
    ws.update(cx, |ws, cx| ws.device_action(None, "update-all", cx));
    assert!(deployer.asked().is_empty(), "a newer machine is not taken back");

    let same = slopty_proto::wire::BUILD.split('+').next().unwrap_or_default().to_owned();
    set(cx, notice(&same));
    ws.update(cx, |ws, cx| ws.device_action(None, "update-all", cx));
    assert!(deployer.asked().is_empty(), "update all leaves it to its row");
    ws.update(cx, |ws, cx| ws.update_worker("mini", cx));
    assert_eq!(
        deployer.asked(),
        ["deploy mini None None server=hub:45560"],
        "the first press goes"
    );
}

/// A build older than this one, on the same wire.
fn older_build() -> String {
    "0.0.1+wire.0badf00d.20240101T0000Z".to_owned()
}

/// A worker that links on this wire from an older build is offered Update on its rows, and
/// "Update all" brings it to this build; this Mac's is updated unasked by an installed app, once
/// a launch. A build this app cannot place, its own, or a newer one offers nothing.
#[gpui::test]
fn a_worker_on_an_older_build_of_this_wire_is_offered_update(cx: &mut TestAppContext) {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (ws, cx) = shell(cx, &runtime, &dir, true);
    let deployer = StandIn::new();
    let shared: Rc<dyn Deployer> = Rc::<StandIn>::clone(&deployer);
    ws.update(cx, |ws, _cx| ws.deployer = Some(shared));
    let id = WorkerId::new();
    list(&ws, cx, id, "mini");
    let key = crate::workers::worker_key(id);
    let _dials = deployer.asked();
    let offered = |cx: &mut VisualTestContext| {
        ws.read_with(cx, |ws, cx| ws.view.read(cx).update_notice(key).map(|n| n.peer.clone()))
    };

    for build in [String::new(), slopty_proto::wire::this_build(), "99.0.0+wire.1".to_owned()] {
        ws.update(cx, |ws, cx| ws.heard_build(id, &build, true, cx));
        assert_eq!(offered(cx), None, "{build:?} offers nothing");
    }
    ws.update(cx, |ws, cx| ws.heard_build(id, &older_build(), true, cx));
    assert_eq!(offered(cx), Some(older_build()), "its rows offer Update");
    assert!(deployer.asked().is_empty(), "elsewhere, only on the person's word");
    ws.update(cx, |ws, cx| ws.device_action(None, "update-all", cx));
    assert_eq!(deployer.asked(), ["deploy 100.64.0.2 None None server=hub:45560"]);
    ws.update(cx, |ws, cx| ws.heard_build(id, &slopty_proto::wire::this_build(), true, cx));
    assert_eq!(offered(cx), None, "back on this build, nothing left to offer");

    // This Mac's worker, dialled on loopback, goes unasked where the app is installed.
    let here = WorkerId::new();
    list(&ws, cx, here, "studio");
    let at_home = slopty_proto::server::WorkerInfo {
        worker: here,
        name: "studio".to_owned(),
        address: "127.0.0.1:45550".to_owned(),
        liveness: slopty_proto::server::Liveness::Online,
        caps: WorkerCaps::bare(WorkerOs::MacOs),
        load: 0.0,
        last_seen_ms: slopty_core::WallMs::ZERO,
    };
    let listing = slopty_proto::server::FromServer::Worker(at_home);
    ws.update(cx, |ws, cx| {
        ws.server_event(slopty_client::server::ServerEvent::Message(Box::new(listing)), cx);
    });
    cx.run_until_parked();
    let _dials = deployer.asked();
    ws.update(cx, |ws, cx| ws.heard_build(here, &older_build(), true, cx));
    assert!(deployer.asked().is_empty(), "a build from a source tree leaves it alone");
    deployer.installed.set(true);
    ws.update(cx, |ws, cx| ws.heard_build(here, &older_build(), true, cx));
    assert_eq!(deployer.asked(), ["deploy 127.0.0.1 None None server=hub:45560"], "unasked");
    ws.update(cx, |ws, cx| {
        if let Some(run) = ws.updates.get_mut("127.0.0.1") {
            run.task = None;
        }
        ws.heard_build(here, &older_build(), true, cx);
    });
    assert!(deployer.asked().is_empty(), "once a launch");
}

/// "Update all" updates the server first and the workers once it is back on this build; a
/// server run that fails stops it there, saying the machines were not updated, since workers on
/// a build their server does not run would lose it. A machine away then is updated when it
/// answers again on an older build, and not on this one.
#[gpui::test]
fn update_all_goes_server_first_and_catches_machines_away(cx: &mut TestAppContext) {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (ws, cx) = shell(cx, &runtime, &dir, true);
    let deployer = StandIn::new();
    let shared: Rc<dyn Deployer> = Rc::<StandIn>::clone(&deployer);
    ws.update(cx, |ws, _cx| ws.deployer = Some(shared));
    let toast = |cx: &mut VisualTestContext| {
        ws.read_with(cx, |_, cx| slopty_shell::toast::messages(cx).last().map(ToString::to_string))
    };
    let address = HostAddr::new("hub", SERVER_PORT);
    ws.update(cx, |ws, _cx| ws.server = Some(crate::server::ServerSlot::stand_in(address)));
    let (mini, away) = (WorkerId::new(), WorkerId::new());
    list(&ws, cx, mini, "mini");
    list(&ws, cx, away, "box");
    let _dials = deployer.asked();
    ws.update(cx, |ws, cx| {
        ws.heard_build(mini, &older_build(), true, cx);
        let linked = slopty_client::server::ServerEvent::Linked {
            name: "hub".to_owned(),
            link: 1,
            build: older_build(),
        };
        ws.server_event(linked, cx);
    });
    cx.run_until_parked();

    ws.update(cx, |ws, cx| ws.device_action(None, "update-all", cx));
    assert_eq!(deployer.asked(), ["serve hub"], "the server first, alone");
    assert_eq!(
        toast(cx).as_deref(),
        Some("Updating 2 machines; 1 away is updated once back, if older")
    );
    let finish = deployer.served.borrow_mut().take().expect("a server run");
    finish.send(Err(failure("no route to hub"))).unwrap();
    cx.run_until_parked();
    assert!(deployer.asked().is_empty(), "no worker goes ahead of its server");
    let said = toast(cx).unwrap_or_default();
    assert!(said.ends_with(". The machines were not updated"), "{said}");
    ws.update(cx, |ws, cx| ws.heard_build(mini, &older_build(), true, cx));
    assert!(deployer.asked().is_empty(), "nor once it is heard again");

    ws.update(cx, |ws, cx| ws.heard_build(away, &older_build(), false, cx));
    assert!(deployer.asked().is_empty(), "a listing is not the machine answering");
    ws.update(cx, |ws, cx| ws.heard_build(away, &older_build(), true, cx));
    assert_eq!(deployer.asked(), ["deploy 100.64.0.2 None None server=hub:45560"], "back");
    let _done = deployer.finish.borrow_mut().take();
    ws.update(cx, |ws, cx| {
        ws.updates.clear();
        ws.heard_build(away, &older_build(), true, cx);
    });
    assert!(deployer.asked().is_empty(), "once");
}

/// A server that links on this wire from an older build is said once, with the way to update
/// it, and the palette's update goes. A newer server is left alone: this app is the older, and
/// neither the palette nor this Mac's own update takes the server back.
#[gpui::test]
fn an_older_server_is_offered_update_and_a_newer_one_never_taken_back(cx: &mut TestAppContext) {
    use slopty_client::update::{Of, UpdateNotice};
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (ws, cx) = shell(cx, &runtime, &dir, true);
    let deployer = StandIn::new();
    let shared: Rc<dyn Deployer> = Rc::<StandIn>::clone(&deployer);
    ws.update(cx, |ws, _cx| ws.deployer = Some(shared));
    deployer.installed.set(true);
    let toast = |cx: &mut VisualTestContext| {
        ws.read_with(cx, |_, cx| slopty_shell::toast::messages(cx).last().map(ToString::to_string))
    };
    let linked = |ws: &Entity<Workspace>, cx: &mut VisualTestContext, build: String| {
        let event =
            slopty_client::server::ServerEvent::Linked { name: "hub".to_owned(), link: 1, build };
        ws.update(cx, |ws, cx| ws.server_event(event, cx));
        cx.run_until_parked();
    };
    let address = HostAddr::new("hub", SERVER_PORT);
    ws.update(cx, |ws, _cx| ws.server = Some(crate::server::ServerSlot::stand_in(address)));

    linked(&ws, cx, "99.0.0+wire.1".to_owned());
    let said = toast(cx).unwrap_or_default();
    assert!(!said.starts_with(crate::server::OLDER_SERVER), "{said}");
    ws.update(cx, |ws, cx| ws.device_action(None, "update", cx));
    assert_eq!(toast(cx).as_deref(), Some("The server runs this build"), "nothing to update");

    linked(&ws, cx, older_build());
    let said = toast(cx).unwrap_or_default();
    assert!(
        said.starts_with(crate::server::OLDER_SERVER) && said.contains(crate::server::UPDATE_HOW),
        "{said}"
    );
    ws.update(cx, |ws, cx| ws.device_action(None, "update", cx));
    assert_eq!(deployer.asked(), ["serve hub"]);
    let _run = deployer.served.borrow_mut().take();
    ws.update(cx, |ws, _cx| ws.server_update = None);

    // On another wire too, a newer server is never updated, asked or not.
    let newer =
        UpdateNotice { of: Of::Server, host: "127.0.0.1".to_owned(), peer: "99.0.0".into() };
    ws.update(cx, |ws, cx| {
        ws.server_event(slopty_client::server::ServerEvent::WrongBuild(newer), cx);
    });
    cx.run_until_parked();
    assert!(deployer.asked().is_empty(), "this Mac's newer server is left as it is");
    ws.update(cx, |ws, cx| ws.device_action(None, "update", cx));
    assert!(deployer.asked().is_empty(), "nor from its row");
    assert_eq!(toast(cx).as_deref(), Some("The server runs a newer build: update Slopty here"));
}
