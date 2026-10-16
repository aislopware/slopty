//! Settings → Devices' Add device and the server this app follows, run by the app for the
//! shell's dialog ([`slopty_ui::workspace::AddDevice`]): this Mac shared ([`this_mac`]), a
//! machine installed over ssh or a server set up there ([`ssh`]), a server connected to, and a
//! look on the tailnet for servers and machines to offer. This Mac's own worker is told to the
//! interface as its "This device".
//!
//! Each run sends its progress as frames the dialog draws. When it stops to ask (a host key
//! `ssh` does not know yet, a password, which server to join, a checklist line's fix) it waits
//! for the person's answer and goes on with it; a password goes to that one run's `ssh` and is
//! not kept. The dialog closing drops the run's stream, which ends the run and drops what it
//! was doing (a deploy's `ssh`).

use std::rc::Rc;

use gpui::{AsyncApp, Context, WeakEntity};
use slopty_deploy::{Failure, Server, Target};
use slopty_net::HostAddr;
use slopty_net::endpoint::SERVER_PORT;
use slopty_net::server::ServerLink;
use slopty_ui::workspace::{
    AddAnswer, AddChoice, AddDevice, AddFailure, AddFix, AddKind, AddMark, AddProgress, AddStep,
    Found, FoundNode,
};
use tokio::sync::mpsc;

use crate::ssh::{self, Bar, Deployer, Kind, Mark, Progress, Said};
use crate::{Workspace, net, this_mac};

/// What a run says where this app cannot put Slopty on a machine.
const NO_SSH: &str = "Add a device from Slopty on a Mac: it installs over ssh from there";
/// What a This Mac run says where there is no Mac to share.
const NO_THIS_MAC: &str = "Only Slopty on a Mac shares that Mac";
/// The fix that starts a server on this Mac rather than join one.
const SERVE_HERE: &str = "serve-here";
/// The fix that looks for this Mac in the server's list again.
const LIST_AGAIN: &str = "list-again";
/// How often a This Mac run short of ready asks the worker again while it waits.
const REREAD: std::time::Duration = std::time::Duration::from_secs(3);
/// Where Tailscale's Mac app is installed, from the App Store and from Tailscale's site alike.
const TAILSCALE_APP: &str = "/Applications/Tailscale.app";
/// Where Tailscale is downloaded for a Mac.
const TAILSCALE_DOWNLOAD: &str = "https://tailscale.com/download/mac";

/// `typed` as `ssh` takes it: an ssh config name, `host`, `user@host` or `user@host:port`.
fn target(typed: &str) -> Result<Target, String> {
    let typed = typed.trim();
    let (host, port) = match typed.rsplit_once(':') {
        Some((host, port))
            if !host.contains(':')
                && !port.is_empty()
                && port.chars().all(|c| c.is_ascii_digit()) =>
        {
            (host, port)
        }
        _ => (typed, ""),
    };
    Target::read(host, "", port)
}

/// A deploy's progress as the dialog draws it.
fn frame(progress: &Progress) -> AddProgress {
    let install = progress.view();
    let steps = install
        .steps
        .into_iter()
        .map(|step| AddStep {
            title: step.title,
            detail: step.detail,
            mark: match step.mark {
                Mark::Done => AddMark::Done,
                Mark::Running => AddMark::Running,
                Mark::Failed => AddMark::Failed,
                Mark::Waiting => AddMark::Waiting,
            },
            fix: None,
        })
        .collect();
    let share = match install.bar {
        Bar::Share(share) => Some(share),
        Bar::Hidden | Bar::Busy => None,
    };
    AddProgress { steps, share, failure: progress.failure().map(failure), ..AddProgress::default() }
}

/// Why a deploy stopped, as the dialog says it.
fn failure(failure: &Failure) -> AddFailure {
    let trust = failure
        .trust
        .as_ref()
        .map(|key| key.keys.iter().map(|key| format!("{} {}", key.kind, key.sha256)).collect());
    let password = failure.password.as_ref().map(|ask| {
        if ask.user.is_empty() { ask.host.clone() } else { format!("{}@{}", ask.user, ask.host) }
    });
    AddFailure {
        title: failure.title.clone(),
        hint: failure.hint.clone(),
        lines: failure.lines.clone(),
        trust,
        password,
        refused: failure.password.as_ref().is_some_and(|ask| ask.refused),
    }
}

/// A frame that says only that the run stopped, and why.
fn stopped(why: impl Into<String>) -> AddProgress {
    let failure = AddFailure { title: why.into(), ..AddFailure::default() };
    AddProgress { failure: Some(failure), ..AddProgress::default() }
}

/// A one-step run's frame: `title` at `mark`.
fn one_step(title: String, mark: AddMark) -> AddProgress {
    let step = AddStep { title, detail: None, mark, fix: None };
    AddProgress { steps: vec![step], ..AddProgress::default() }
}

/// Send `frame`; the dialog may have gone.
fn send(frames: &mpsc::UnboundedSender<AddProgress>, frame: AddProgress) {
    let _closed = frames.send(frame);
}

impl Workspace {
    /// Start the Add device run the view model holds ([`slopty_ui::workspace::WorkspaceEvent`]).
    pub(crate) fn add_device(&self, cx: &mut Context<Self>) {
        let Some(run) = self.view.update(cx, |view, _| view.take_add_device()) else { return };
        match run.kind {
            AddKind::Server => self.connect_server_run(run, cx),
            AddKind::ThisMac => {
                let Some(host) = self.this_mac.clone() else {
                    return send(&run.frames, stopped(NO_THIS_MAC));
                };
                cx.spawn(async move |this, cx| this_mac_run(this, cx, host, run).await).detach();
            }
            AddKind::Ssh | AddKind::ServeSsh => {
                let Some(deployer) = self.deployer.clone() else {
                    return send(&run.frames, stopped(NO_SSH));
                };
                let to = match target(&run.target) {
                    Ok(to) => to,
                    Err(why) => return send(&run.frames, stopped(why)),
                };
                if run.kind == AddKind::ServeSsh {
                    cx.spawn(async move |this, cx| serve_run(this, cx, deployer, to, run).await)
                        .detach();
                    return;
                }
                let Some(server) = self.register_with() else {
                    return send(&run.frames, stopped(ssh::NO_SERVER));
                };
                cx.spawn(async move |this, cx| add(this, cx, deployer, server, to, run).await)
                    .detach();
            }
        }
    }

    /// Look on the tailnet for the look the view model holds, and say what answered: servers,
    /// and machines with a worker the server does not list.
    pub(crate) fn look_on_tailnet_for_devices(&self, cx: &mut Context<Self>) {
        let Some(look) = self.view.update(cx, |view, _| view.take_look()) else { return };
        let _closed = look.frames.send(Found { looking: true, ..Found::default() });
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.runtime.spawn(async move {
            let _sent = tx.send(net::find_on_tailnet().await);
        });
        cx.spawn(async move |this, cx| {
            let found = rx.await.unwrap_or_default();
            let listed = this.update(cx, |ws, _| {
                ws.directory
                    .workers()
                    .filter_map(|w| w.address.parse::<std::net::SocketAddr>().ok())
                    .map(|a| a.ip())
                    .collect::<Vec<_>>()
            });
            let Ok(listed) = listed else { return };
            let node = |found: slopty_net::discover::Found| {
                use slopty_net::discover::Answer;
                let note = match found.answer.as_ref() {
                    None | Some(Answer::Ready) => None,
                    Some(Answer::OtherBuild(build)) if build.is_empty() => {
                        Some("Runs an older build".to_owned())
                    }
                    Some(Answer::OtherBuild(build)) => Some(format!("Runs {build}")),
                    Some(Answer::NotGranted) => {
                        Some("The tailnet's policy does not let this device in".to_owned())
                    }
                };
                FoundNode {
                    name: found.name,
                    at: found.addr.ip().to_string(),
                    ready: note.is_none(),
                    note,
                }
            };
            let workers = found
                .workers
                .into_iter()
                .filter(|w| !listed.contains(&w.addr.ip()))
                .map(node)
                .collect();
            let servers = found.servers.into_iter().map(node).collect();
            let _closed = look.frames.send(Found {
                looking: false,
                running: found.running,
                servers,
                workers,
            });
        })
        .detach();
    }

    /// Tell the interface this Mac's own worker, once its `doctor` said, or that there is none.
    pub(crate) fn tell_local_device(&self) {
        let Some(feed) = &self.interface_feed else { return };
        if self.this_mac.is_some() && !self.this_mac_asked {
            return;
        }
        let worker =
            self.this_mac_worker.map(|id| slopty_ui::workspace::worker_key(id).to_string());
        feed.local_device(worker);
    }

    /// The app is in front again, perhaps from System Settings: a This Mac run reads the
    /// worker and the app's own lines again.
    pub(crate) fn wake_this_mac_run(&self) {
        if let Some(wake) = &self.this_mac_wake {
            let _gone = wake.send(());
        }
    }

    /// Follow the server at `address`, which `link` proved: kept as `[network] server`, and the
    /// machine `typed` reached it at remembered for its updates. Its name, or why it was not
    /// kept.
    fn follow_server(
        &mut self,
        address: HostAddr,
        link: ServerLink,
        typed: Option<&Target>,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        if let Err(e) = self.save_server(Some(&address)) {
            link.close();
            return Err(e);
        }
        if let (Some(deployer), Some(typed)) = (&self.deployer, typed) {
            deployer.remember_server(&address, typed);
        }
        let name = link.name.clone();
        if self.server_address() == Some(&address) {
            self.relink_server(link, cx);
        } else {
            self.set_server(Some(address), Some(link), cx);
        }
        cx.notify();
        Ok(name)
    }

    /// Connect to the server the run names: prove it answers, then follow it.
    fn connect_server_run(&self, run: AddDevice, cx: &Context<Self>) {
        let AddDevice { target: typed, frames, .. } = run;
        let address = match HostAddr::parse_with_port(&typed, SERVER_PORT) {
            Ok(address) => address,
            Err(e) => return send(&frames, stopped(e.to_string())),
        };
        let title = format!("Connect to {address}");
        send(&frames, one_step(title.clone(), AddMark::Running));
        let (tx, rx) = tokio::sync::oneshot::channel();
        let dial = address.clone();
        self.runtime.spawn(async move {
            let _sent = tx.send(net::link_server(&dial).await);
        });
        cx.spawn(async move |this, cx| {
            let linked = tokio::select! {
                () = frames.closed() => return,
                linked = rx => linked,
            };
            let followed = match linked {
                Ok(Ok(link)) => this
                    .update(cx, |ws, cx| ws.follow_server(address, link, None, cx))
                    .unwrap_or_else(|_gone| Err("the app is closing".to_owned())),
                Ok(Err(e)) => Err(format!("{e:#}")),
                Err(_dropped) => Err("the connection task died".to_owned()),
            };
            match followed {
                Ok(name) => send(
                    &frames,
                    AddProgress {
                        added: Some(format!("Connected to {name}")),
                        ..one_step(title, AddMark::Done)
                    },
                ),
                Err(why) => {
                    let failure = AddFailure {
                        title: format!("Slopty couldn't connect to {typed}"),
                        lines: vec![why],
                        ..AddFailure::default()
                    };
                    send(
                        &frames,
                        AddProgress { failure: Some(failure), ..one_step(title, AddMark::Failed) },
                    );
                }
            }
        })
        .detach();
    }
}

/// Wait for the person's answer to a run that stopped to ask; `None` once the dialog is gone.
async fn answer(
    frames: &mpsc::UnboundedSender<AddProgress>,
    answers: &mut mpsc::UnboundedReceiver<AddAnswer>,
) -> Option<AddAnswer> {
    tokio::select! {
        () = frames.closed() => None,
        answer = answers.recv() => answer,
    }
}

/// What the person said to a deploy that stopped at a host key or a password, as the next run
/// takes it; `None` once the dialog is gone or the trust failed (said in a frame).
async fn said_again(
    deployer: &Rc<dyn Deployer>,
    progress: &mut Progress,
    key: Option<slopty_deploy::HostKey>,
    frames: &mpsc::UnboundedSender<AddProgress>,
    answers: &mut mpsc::UnboundedReceiver<AddAnswer>,
) -> Option<Said> {
    match answer(frames, answers).await? {
        AddAnswer::Trust => {
            if let Some(key) = key
                && let Err(failure) = deployer.trust(&key).await
            {
                progress.failed(failure);
                send(frames, frame(progress));
                return None;
            }
            Some(Said::default())
        }
        AddAnswer::Password { password, add_key } => Some(Said {
            end_sessions: false,
            password: Some(slopty_deploy::SecretString::from(password)),
            add_key,
        }),
        AddAnswer::Fix(_) | AddAnswer::Choose(_) => Some(Said::default()),
    }
}

/// Run `deploy` until it ends, each event in `progress` and a frame; `None` once the dialog is
/// gone, which drops the deploy.
async fn follow<T>(
    mut deploy: this_mac::Pending<Result<T, Failure>>,
    mut events: mpsc::UnboundedReceiver<slopty_deploy::Event>,
    progress: &mut Progress,
    frames: &mpsc::UnboundedSender<AddProgress>,
) -> Option<Result<T, Failure>> {
    let done = loop {
        tokio::select! {
            biased;
            () = frames.closed() => return None,
            Some(event) = events.recv() => {
                progress.apply(event);
                send(frames, frame(progress));
            }
            done = &mut deploy => break done,
        }
    };
    while let Ok(event) = events.try_recv() {
        progress.apply(event);
    }
    Some(done)
}

/// Install on `to` until the server lists it, asking the person through `run` when a run stops
/// to ask; over once it is added, it fails for good, or the dialog lets it go.
async fn add(
    this: WeakEntity<Workspace>,
    cx: &mut AsyncApp,
    deployer: Rc<dyn Deployer>,
    server: Server,
    to: Target,
    run: AddDevice,
) {
    let AddDevice { frames, mut answers, .. } = run;
    let mut said = Said::default();
    loop {
        let (events_to, events) = mpsc::unbounded_channel();
        let deploy = deployer.deploy(&to, server.clone(), said, events_to);
        let mut progress = Progress::new(to.host.clone(), Kind::Install);
        send(&frames, frame(&progress));
        let Some(done) = follow(deploy, events, &mut progress, &frames).await else { return };
        let stopped = match done {
            Ok(deployed) => {
                progress.deployed(deployed.platform);
                send(&frames, frame(&progress));
                let worker = deployed.health.worker;
                deployer.remember(worker, &to);
                if ssh::listed(&this, cx, worker).await {
                    let name = this.update(cx, |ws, _| {
                        ws.directory
                            .get(worker)
                            .map_or_else(|| worker.to_string(), |w| w.name.clone())
                    });
                    let Ok(name) = name else { return };
                    let added = Some(ssh::added_notice(&name, &deployed));
                    send(&frames, AddProgress { added, ..frame(&progress) });
                    return;
                }
                ssh::not_listed(&to.host, &deployed)
            }
            Err(failure) => failure,
        };
        let key = stopped.trust.as_deref().cloned();
        let asks = key.is_some() || stopped.password.is_some();
        progress.failed(stopped);
        send(&frames, frame(&progress));
        if !asks {
            return;
        }
        let Some(again) = said_again(&deployer, &mut progress, key, &frames, &mut answers).await
        else {
            return;
        };
        said = again;
    }
}

/// Put this build's server on `to` and follow it, asking the person through `run` when it
/// stops to ask.
async fn serve_run(
    this: WeakEntity<Workspace>,
    cx: &mut AsyncApp,
    deployer: Rc<dyn Deployer>,
    to: Target,
    run: AddDevice,
) {
    let AddDevice { frames, mut answers, .. } = run;
    let mut said = Said::default();
    loop {
        let (events_to, events) = mpsc::unbounded_channel();
        let serve = deployer.serve(&to, said, events_to);
        let mut progress = Progress::new(to.host.clone(), Kind::Server);
        send(&frames, frame(&progress));
        let Some(done) = follow(serve, events, &mut progress, &frames).await else { return };
        let stopped = match done {
            Ok(served) => {
                progress.deployed(served.platform);
                send(&frames, frame(&progress));
                let mut why = String::new();
                for address in &served.addresses {
                    let address = match HostAddr::parse_with_port(address, SERVER_PORT) {
                        Ok(address) => address,
                        Err(e) => {
                            why = e.to_string();
                            continue;
                        }
                    };
                    match deployer.link_server(&address).await {
                        Ok(link) => {
                            let followed = this.update(cx, |ws, cx| {
                                ws.follow_server(address, link, Some(&to), cx)
                            });
                            let Ok(followed) = followed else { return };
                            match followed {
                                Ok(name) => {
                                    let added = Some(format!("Connected to {name}"));
                                    send(&frames, AddProgress { added, ..frame(&progress) });
                                    return;
                                }
                                Err(e) => why = e,
                            }
                        }
                        Err(e) => why = e,
                    }
                }
                Failure::new(
                    format!("The server runs on {}, and Slopty couldn't connect to it", to.host),
                    Some(format!("Check that UDP {SERVER_PORT} reaches it from this Mac.")),
                    vec![why],
                )
            }
            Err(failure) => failure,
        };
        let key = stopped.trust.as_deref().cloned();
        let asks = key.is_some() || stopped.password.is_some();
        progress.failed(stopped);
        send(&frames, frame(&progress));
        if !asks {
            return;
        }
        let Some(again) = said_again(&deployer, &mut progress, key, &frames, &mut answers).await
        else {
            return;
        };
        said = again;
    }
}

/// How a checklist line wears its mark: a grant not given yet is the person's to do, red is for
/// what failed, and a line not in the way is quiet.
const fn mark(line: &this_mac::Line) -> AddMark {
    use this_mac::{Fix, Mark as Line};
    match (line.mark, line.fix) {
        (Line::Ok, _) => AddMark::Done,
        (
            Line::Missing | Line::Advisory,
            Some(Fix::Open(_) | Fix::AllowNotes | Fix::OpenAtLogin),
        ) => AddMark::NeedsYou,
        (Line::Missing, _) => AddMark::Failed,
        (Line::Busy, _) => AddMark::Running,
        (Line::Advisory | Line::Unknown, _) => AddMark::Waiting,
    }
}

/// This Mac's checklist as it stands.
fn checklist(flow: &this_mac::Flow, push: this_mac::Push) -> Vec<this_mac::Line> {
    let session = slopty_platform::service::Session::native();
    let logs = session.logs(slopty_platform::service::WORKER);
    let server_logs = session.logs(slopty_platform::service::SERVER);
    this_mac::checklist(flow, &logs, &server_logs)
        .into_iter()
        .chain(this_mac::push_line(flow, push))
        .chain(this_mac::app_lines(flow))
        .collect()
}

/// The step that waits for the server to list this Mac, at `mark`.
fn listing_step(mark: AddMark, fix: Option<AddFix>) -> AddStep {
    AddStep { title: "Wait for the server to list this Mac".to_owned(), detail: None, mark, fix }
}

/// This Mac's flow as the dialog draws it: its checklist, then the wait for the server's list.
fn this_mac_frame(flow: &this_mac::Flow, push: this_mac::Push) -> AddProgress {
    let mut steps: Vec<AddStep> = checklist(flow, push)
        .into_iter()
        .map(|line| AddStep {
            title: line.check.title().to_owned(),
            detail: Some(line.detail.clone()),
            mark: mark(&line),
            fix: line.fix.map(|fix| AddFix {
                id: line.check.fix_id().to_owned(),
                label: fix.label().to_owned(),
            }),
        })
        .collect();
    if flow.listing {
        steps.push(listing_step(AddMark::Running, None));
    }
    let failure = flow.error.clone().map(|why| AddFailure {
        title: why,
        hint: Some("Try again once its worker is up".to_owned()),
        ..AddFailure::default()
    });
    if failure.is_some() {
        let again = AddFix { id: LIST_AGAIN.to_owned(), label: "Try again".to_owned() };
        steps.push(listing_step(AddMark::Failed, Some(again)));
    }
    let added = flow.listed.then(|| flow.ready_words().to_owned());
    AddProgress { steps, failure, added, ..AddProgress::default() }
}

/// What a This Mac run asks when the app cannot tell the server: why, and the servers to choose
/// between.
type Ask = (String, Vec<AddChoice>);

/// The server a This Mac run registers with, as far as the app can tell: the one it follows or
/// its settings name, else what the tailnet answered. `Err` with the servers to choose between
/// when the person must say; `None` once the app is gone.
async fn this_mac_serve(
    this: &WeakEntity<Workspace>,
    cx: &mut AsyncApp,
) -> Option<Result<this_mac::Serve, Ask>> {
    let (set, runtime) = this
        .update(cx, |ws, _| {
            let set = ws.server_address().or(ws.settings.network.server.as_ref()).cloned();
            (set, ws.runtime.clone())
        })
        .ok()?;
    if let Some(server) = set {
        return Some(Ok(this_mac::Serve::Join(server)));
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    runtime.spawn(async move {
        let _sent = tx.send(net::find_on_tailnet().await);
    });
    let found = rx.await.unwrap_or_default();
    let choices: Vec<AddChoice> = found
        .servers
        .iter()
        .map(|s| AddChoice { name: s.name.clone(), at: s.addr.ip().to_string() })
        .collect();
    let ready = |s: &slopty_net::discover::Found| {
        matches!(s.answer, None | Some(slopty_net::discover::Answer::Ready))
    };
    Some(match found.servers.as_slice() {
        _ if !found.running => Err((
            "No tailnet to look for a server on. Choose one, or run one on this Mac.".to_owned(),
            choices,
        )),
        [] => Ok(this_mac::Serve::Here),
        [one] if ready(one) => HostAddr::parse_with_port(&one.addr.ip().to_string(), SERVER_PORT)
            .map(this_mac::Serve::Join)
            .map_err(|e| (e.to_string(), choices)),
        [_] => Err((
            "A server answered that this Mac can't join yet. Update it, or let this Mac in, rather than run a second one here."
                .to_owned(),
            choices,
        )),
        _ => Err(("Several servers answered. Choose the one this Mac joins.".to_owned(), choices)),
    })
}

/// Ask the person which server this Mac joins: one the tailnet answered with, or one started
/// here. `None` once the dialog is gone or the answer does not read.
async fn ask_serve(
    (why, choices): Ask,
    frames: &mpsc::UnboundedSender<AddProgress>,
    answers: &mut mpsc::UnboundedReceiver<AddAnswer>,
) -> Option<this_mac::Serve> {
    let here = AddFix { id: SERVE_HERE.to_owned(), label: "Run one on this Mac".to_owned() };
    let step = AddStep {
        title: this_mac::Check::Server.title().to_owned(),
        detail: Some("Choose the server this Mac joins".to_owned()),
        mark: AddMark::NeedsYou,
        fix: Some(here),
    };
    let failure = AddFailure { title: why, ..AddFailure::default() };
    send(
        frames,
        AddProgress {
            steps: vec![step],
            failure: Some(failure),
            choices,
            ..AddProgress::default()
        },
    );
    match answer(frames, answers).await? {
        AddAnswer::Choose(at) => match HostAddr::parse_with_port(&at, SERVER_PORT) {
            Ok(address) => Some(this_mac::Serve::Join(address)),
            Err(e) => {
                send(frames, stopped(e.to_string()));
                None
            }
        },
        AddAnswer::Fix(id) if id == SERVE_HERE => Some(this_mac::Serve::Here),
        AddAnswer::Fix(_) | AddAnswer::Trust | AddAnswer::Password { .. } => None,
    }
}

/// What a checklist fix asks of the run: install again (ending the sessions it said it would),
/// or keep waiting.
enum Then {
    /// Install again; `true` ends the sessions the last try said it would.
    Install(bool),
    /// Stay on the checklist.
    Wait,
    /// The app is restarting from Applications, or gone.
    Over,
}

/// Do the checklist fix `id` of `flow`, whatever touches the Mac or the app.
#[expect(clippy::too_many_arguments, reason = "one run's state, threaded through its loop")]
async fn fix(
    this: &WeakEntity<Workspace>,
    cx: &mut AsyncApp,
    host: &dyn this_mac::Host,
    flow: &mut this_mac::Flow,
    push: this_mac::Push,
    id: &str,
    frames: &mpsc::UnboundedSender<AddProgress>,
) -> Then {
    let fix = checklist(flow, push)
        .into_iter()
        .find(|line| line.check.fix_id() == id)
        .and_then(|l| l.fix);
    match fix {
        Some(this_mac::Fix::Retry) => return Then::Install(false),
        Some(this_mac::Fix::EndSessions) => return Then::Install(true),
        Some(this_mac::Fix::Open(place)) => host.open(place),
        Some(this_mac::Fix::AllowNotes) => flow.notes = Some(host.ask_notes().await),
        Some(this_mac::Fix::OpenAtLogin) => match host.open_at_login().await {
            Ok(now) => flow.login = Some(now),
            Err(why) => send(frames, stopped(format!("Slopty couldn't open at login: {why}"))),
        },
        Some(this_mac::Fix::MoveToApplications) => {
            flow.worker = this_mac::Worker::Installing;
            send(frames, this_mac_frame(flow, push));
            match host.move_to_applications().await {
                Ok(to) => {
                    tracing::info!(to = %to.display(), "moved to Applications; opening it there");
                    cx.update(|cx| {
                        cx.set_restart_path(to);
                        cx.restart();
                    });
                    return Then::Over;
                }
                Err(why) => {
                    flow.worker = this_mac::Worker::Failed(format!("Couldn't move Slopty: {why}"));
                }
            }
        }
        Some(this_mac::Fix::SetUpPush) => {
            let path = slopty_settings::path();
            cx.update(|cx| cx.open_with_system(&path));
        }
        Some(this_mac::Fix::OpenTailscale) => {
            cx.update(|cx| {
                let app = std::path::Path::new(TAILSCALE_APP);
                if app.exists() {
                    cx.open_with_system(app);
                } else {
                    cx.open_url(TAILSCALE_DOWNLOAD);
                }
            });
        }
        Some(this_mac::Fix::GetTailscale) => {
            cx.update(|cx| cx.open_url(TAILSCALE_DOWNLOAD));
        }
        Some(this_mac::Fix::CopyGrant)
            if this.update(cx, |_, cx| Workspace::copy_worker_grant(cx)).is_err() =>
        {
            return Then::Over;
        }
        Some(this_mac::Fix::CopyGrant) | None => {}
    }
    Then::Wait
}

/// The app's own lines of the checklist, read again: notifications, open at login, Finder.
async fn read_app_lines(host: &dyn this_mac::Host, flow: &mut this_mac::Flow) {
    flow.notes = Some(host.notes().await);
    flow.login = Some(host.login().await);
    flow.finder = Some(host.finder().await);
}

/// Share this Mac: decide the server (asking the person when the app cannot tell), install,
/// then read the worker's `doctor` as the checklist until the server lists this Mac, each
/// line's fix the person's to press; over when the dialog lets it go.
async fn this_mac_run(
    this: WeakEntity<Workspace>,
    cx: &mut AsyncApp,
    host: Rc<dyn this_mac::Host>,
    run: AddDevice,
) {
    let AddDevice { frames, mut answers, .. } = run;
    let (wake_to, mut wake) = mpsc::unbounded_channel();
    let Ok(push) = this.update(cx, |ws, _| {
        ws.this_mac_wake = Some(wake_to);
        this_mac::Push::of(&ws.settings.server.push)
    }) else {
        return;
    };
    let mut flow = this_mac::Flow::looking(0);
    send(&frames, this_mac_frame(&flow, push));
    let serve = match this_mac_serve(&this, cx).await {
        None => return,
        Some(Ok(serve)) => serve,
        Some(Err(ask)) => match ask_serve(ask, &frames, &mut answers).await {
            Some(serve) => serve,
            None => return,
        },
    };
    let mut end_sessions = false;
    loop {
        flow = this_mac::Flow::installing(0, serve.clone());
        read_app_lines(&*host, &mut flow).await;
        send(&frames, this_mac_frame(&flow, push));
        let installed = tokio::select! {
            () = frames.closed() => return,
            installed = host.install(&serve, end_sessions) => installed,
        };
        match installed {
            Ok(server) => {
                flow.worker = this_mac::Worker::Starting;
                let followed = this.update(cx, |ws, cx| {
                    if ws.server_address() != Some(&server) {
                        if let Err(e) = ws.save_server(Some(&server)) {
                            Workspace::show_failure(
                                format!("Couldn't save the server to settings.toml: {e}"),
                                cx,
                            );
                        }
                        ws.set_server(Some(server), None, cx);
                    }
                });
                if followed.is_err() {
                    return;
                }
                send(&frames, this_mac_frame(&flow, push));
                flow.worker = read_doctor(&*host, cx, false).await;
            }
            Err(this_mac::Stopped::Failed(why)) => flow.worker = this_mac::Worker::Failed(why),
            Err(this_mac::Stopped::EndsSessions(plan)) => {
                flow.worker = this_mac::Worker::EndsSessions(plan);
            }
            Err(this_mac::Stopped::Misplaced) => flow.worker = this_mac::Worker::Misplaced,
            Err(this_mac::Stopped::Server(why)) => flow.server = this_mac::Server::Failed(why),
        }
        match checklist_loop(&this, cx, &*host, &mut flow, push, &frames, &mut answers, &mut wake)
            .await
        {
            Some(again) => end_sessions = again,
            None => return,
        }
    }
}

/// The checklist until the person or a line asks for another install (its `end_sessions`), or
/// the dialog lets it go (`None`): the server's list once the worker is ready, each line's fix,
/// and a read again whenever the app comes back to the front and every few seconds.
#[expect(clippy::too_many_arguments, reason = "one run's state, threaded through its loop")]
async fn checklist_loop(
    this: &WeakEntity<Workspace>,
    cx: &mut AsyncApp,
    host: &dyn this_mac::Host,
    flow: &mut this_mac::Flow,
    push: this_mac::Push,
    frames: &mpsc::UnboundedSender<AddProgress>,
    answers: &mut mpsc::UnboundedReceiver<AddAnswer>,
    wake: &mut mpsc::UnboundedReceiver<()>,
) -> Option<bool> {
    loop {
        if flow.worker.ready() && !flow.listed && flow.error.is_none() {
            let this_mac::Worker::Up(doctor) = &flow.worker else { return None };
            let worker = doctor.worker;
            flow.listing = true;
            send(frames, this_mac_frame(flow, push));
            if wait_listed(this, cx, host, worker, flow).await {
                flow.listed = true;
                let told = this.update(cx, |ws, cx| {
                    ws.this_mac_worker = Some(worker);
                    ws.tell_local_device();
                    cx.notify();
                });
                told.ok()?;
                // A Mac that shares itself opens Slopty at login, unless the person turned it off.
                if host.login().await == this_mac::Login::Off
                    && let Ok(now) = host.open_at_login().await
                {
                    flow.login = Some(now);
                }
            } else {
                let logs = slopty_platform::service::Session::native()
                    .logs(slopty_platform::service::WORKER);
                flow.error = Some(format!("{} Its log is {logs}", this_mac::NOT_LISTED));
            }
            flow.listing = false;
        }
        send(frames, this_mac_frame(flow, push));
        let timer = cx.background_executor().timer(REREAD);
        let said = tokio::select! {
            () = frames.closed() => return None,
            said = answers.recv() => Some(said?),
            _woken = wake.recv() => None,
            () = timer => None,
        };
        let Some(said) = said else {
            // Back in front, or a while passed: what the person may have changed is read again.
            read_app_lines(host, flow).await;
            if flow.rereads() {
                let restart = flow.restarts();
                flow.worker = read_doctor(host, cx, restart).await;
            }
            continue;
        };
        let AddAnswer::Fix(id) = said else { continue };
        if id == LIST_AGAIN {
            flow.error = None;
            continue;
        }
        match fix(this, cx, host, flow, push, &id, frames).await {
            Then::Install(end_sessions) => return Some(end_sessions),
            Then::Wait => {}
            Then::Over => return None,
        }
    }
}

/// The worker's `doctor`, asked until it answers or [`this_mac::ATTEMPTS`] run out, started
/// again first when `restart`.
async fn read_doctor(host: &dyn this_mac::Host, cx: &AsyncApp, restart: bool) -> this_mac::Worker {
    if restart {
        host.restart().await;
    }
    for attempt in 0..this_mac::ATTEMPTS {
        if attempt > 0 {
            cx.background_executor().timer(this_mac::RETRY).await;
        }
        if let Some(doctor) = host.doctor().await {
            return this_mac::Worker::Up(doctor);
        }
    }
    this_mac::Worker::Silent
}

/// Look, at most [`this_mac::LISTED_TRIES`] times, for the server's directory to list `worker`,
/// reading the worker's own word on its link meanwhile so the Server line says why not.
async fn wait_listed(
    this: &WeakEntity<Workspace>,
    cx: &AsyncApp,
    host: &dyn this_mac::Host,
    worker: slopty_core::WorkerId,
    flow: &mut this_mac::Flow,
) -> bool {
    for attempt in 0..this_mac::LISTED_TRIES {
        if attempt > 0 {
            cx.background_executor().timer(this_mac::RETRY).await;
            if let Some(doctor) = host.doctor().await {
                flow.worker = this_mac::Worker::Up(doctor);
            }
        }
        match this.read_with(cx, |ws, _| ws.directory.get(worker).is_some()) {
            Ok(true) => return true,
            Ok(false) => {}
            Err(_gone) => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_machine_reads_as_ssh_takes_it() {
        let to = target("me@mac-mini:2222").unwrap();
        assert_eq!(
            (to.host.as_str(), to.user.as_deref(), to.port),
            ("mac-mini", Some("me"), Some(2222))
        );
        let to = target(" build-box ").unwrap();
        assert_eq!((to.host.as_str(), to.user, to.port), ("build-box", None, None));
        target("").unwrap_err();
        target("-oProxyCommand=x").unwrap_err();
    }

    #[test]
    fn a_grant_to_give_needs_the_person_and_a_failure_is_red() {
        use this_mac::{Check, Fix, Line, Mark as LineMark, Place};
        let line =
            |mark, fix| Line { check: Check::ScreenRecording, mark, detail: String::new(), fix };
        let grant = Fix::Open(Place::Privacy(this_mac::Pane::ScreenRecording));
        assert_eq!(mark(&line(LineMark::Missing, Some(grant))), AddMark::NeedsYou);
        assert_eq!(mark(&line(LineMark::Missing, Some(Fix::Retry))), AddMark::Failed);
        assert_eq!(mark(&line(LineMark::Ok, None)), AddMark::Done);
        assert_eq!(mark(&line(LineMark::Advisory, Some(Fix::OpenTailscale))), AddMark::Waiting);
    }
}
