//! The SSH sheet and a tile's "Update" driven through a stand-in [`Deployer`] the test plays:
//! it hands the test the deploy's event sender and its ending, so each step is shown as the
//! test says it happened, and no machine is reached.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{Modifiers, TestAppContext, VisualTestContext, px, size};
use slopty_deploy::{Arch, Os, Platform};
use slopty_proto::ctl::PasteboardAccess;
use slopty_proto::server::{Os as WorkerOs, WorkerCaps};
use tokio::sync::oneshot;

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
    dropped: Arc<AtomicBool>,
    /// The one address that answers an add, and the worker it adds.
    answers: String,
    worker: WorkerId,
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
        events: mpsc::UnboundedSender<Event>,
    ) -> Pending<Result<Deployed, Failure>> {
        let Target { host, user, port } = to;
        let server = server.map(|s| format!("{}:{}", s.host, s.port));
        self.asked.borrow_mut().push(format!("deploy {host} {user:?} {port:?} server={server:?}"));
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
}

fn failure(title: &str) -> Failure {
    Failure { title: title.to_owned(), hint: None, lines: Vec::new() }
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
    let mut progress = Progress::new("mini".to_owned(), false);
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
        Progress::new("mini".to_owned(), true).view().steps.last().map(|s| s.title.as_str()),
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
        Err(Failure {
            title: "mini did not accept your SSH key".to_owned(),
            hint: Some("Add your key to your ssh agent, or to its authorized_keys.".to_owned()),
            lines: vec!["me@mini: Permission denied (publickey).".to_owned()],
        }),
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
    assert_eq!(run.current().map(|s| s.title.as_str()), Some("Copy the worker"));
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
