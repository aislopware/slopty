//! The app's side of the server: the directory that lists the workers, the one link that keeps
//! it current, and what each change means for the workers' own links
//! (`docs/decisions/topology.md`).
//!
//! The server only says which workers exist, where, and whether they are there. Terminals and
//! streams go to each worker directly, so a server that goes away costs the list its updates
//! and nothing else: the cached directory stands and every worker in it is dialled as before.

use std::sync::Arc;
use std::time::Duration;

use gpui::Context;
use slopty_client::directory::{self, Change, Dial, Directory, ServerState};
use slopty_client::layout::WorkerKey;
use slopty_client::server::{ServerCaller, ServerEvent, ServerTask};
use slopty_client::update::UpdateNotice;
use slopty_core::WorkerId;
use slopty_net::HostAddr;
use slopty_net::server::ServerLink;
use slopty_proto::orchestration::{Happening, Outcome};
use slopty_proto::server::{FromServer, Liveness, Refusal, WorkerCaps};
use slopty_ui::workspace::WorkerStatus;

use crate::net::DialFailed;
use crate::workers::{WorkerSlot, worker_key};
use crate::{Workspace, net};

/// The title bar's word while the server does not answer.
pub(crate) const UNREACHABLE: &str = "Server offline";
/// The title bar's word while the server answers on a different build.
pub(crate) const OTHER_BUILD: &str = "Server runs a different build";
/// The palette's line that brings the server to this build.
pub(crate) const UPDATE_SERVER: &str = "Update the server";
/// The title bar's line while the server turns this app away: the tailnet policy grants this
/// device no client role there.
pub(crate) const NOT_GRANTED: &str = "Server needs a tailnet grant for this device";
/// Said once as the server starts turning this app away: where a grant is added, and where it
/// is copied from. Headscale keeps the same grants in its policy file.
pub(crate) const GRANT_WHERE: &str =
    "Copy the tailnet grant from the palette into Tailscale's Access controls";
/// The palette's line that copies [`client_grant`].
pub(crate) const COPY_GRANT: &str = "Copy the tailnet grant for Slopty's clients";
/// Said once the grant is on the clipboard.
const GRANT_COPIED: &str = "Copied: paste it into the grants of Tailscale's Access controls";
/// The tag a tailnet gives the nodes that run a Slopty worker, which [`client_grant`] names
/// beside the server's.
pub(crate) const WORKER_TAG: &str = "tag:slopty-worker";

/// The grant that lets the tailnet's members in as clients of a server tagged as discovery
/// prefers one, and of workers tagged [`WORKER_TAG`] (`docs/decisions/topology.md`), as the
/// policy file's `grants` takes it.
pub(crate) fn client_grant() -> String {
    grant_to(&[slopty_net::discover::SERVER_TAG, WORKER_TAG])
}

/// The grant that lets the tailnet's members in as clients of the nodes `dst` names: tags,
/// or a node's address.
pub(crate) fn grant_to(dst: &[&str]) -> String {
    let cap = slopty_tailnet::policy::CAP;
    let dst: Vec<String> = dst.iter().map(|d| format!("\"{d}\"")).collect();
    let dst = dst.join(", ");
    format!(
        r#"{{"src": ["autogroup:member"], "dst": [{dst}], "ip": ["*"], "app": {{"{cap}": [{{"roles": ["client"]}}]}}}}"#
    )
}

/// A worker the server says is away is still dialled this often: the server's view of it can
/// be wrong (its path to the worker broken while this client's works).
pub(crate) const HOLD_RETRY: Duration = Duration::from_secs(10);

/// The server this app is using.
#[derive(Debug)]
pub(crate) struct ServerSlot {
    /// Where it is.
    address: HostAddr,
    /// The link once it is started; dropping it ends the link and its redials.
    task: Option<ServerTask>,
    /// It answered last on a different build: what to say of it, and what updates it.
    other_build: Option<UpdateNotice>,
    /// A test's own end of the link: the verbs sent up it wait in the test's queue
    /// ([`ServerCaller::queued`]).
    #[cfg(test)]
    caller: Option<ServerCaller>,
}

#[cfg(test)]
impl ServerSlot {
    /// The server at `address`, its link not started: what a test sets to have a server.
    pub(crate) const fn stand_in(address: HostAddr) -> Self {
        Self { address, task: None, other_build: None, caller: None }
    }

    /// [`Self::stand_in`], the verbs sent to it queued for the test to answer.
    pub(crate) fn answered_by(address: HostAddr) -> (Self, slopty_client::server::CallQueue) {
        let (caller, queue) = ServerCaller::queued();
        (Self { caller: Some(caller), ..Self::stand_in(address) }, queue)
    }
}

/// What a worker's connect loop does next.
#[derive(Debug)]
pub(crate) struct Plan {
    /// The worker's key in the workspace.
    pub key: WorkerKey,
    /// Wakes the loop out of a wait.
    pub wake: Arc<tokio::sync::Notify>,
    /// Where to dial: the directory's address, `None` while it lists none.
    pub address: Option<HostAddr>,
    /// The server says it is away: show this and wait before dialling.
    pub hold: Option<WorkerStatus>,
}

/// How the workspace shows a worker the server says is away.
const fn away(liveness: Liveness) -> Option<WorkerStatus> {
    match liveness {
        Liveness::Online => None,
        Liveness::Unreachable => Some(WorkerStatus::Unreachable),
        Liveness::Gone => Some(WorkerStatus::Gone),
    }
}

/// How a failed dial shows. A worker that answered to turn this device away, or to say it runs
/// another build, is reachable, so that is said whatever the server thinks of it; otherwise the
/// server's word when it has one, else the reason.
fn failure_status(dial: &Dial, failed: DialFailed) -> WorkerStatus {
    let own = match failed {
        DialFailed::NotGranted => return WorkerStatus::NotGranted,
        DialFailed::WrongBuild(notice) => return WorkerStatus::NeedsUpdate(notice),
        DialFailed::Refused(me) => return WorkerStatus::Refused(me.map(|ip| ip.to_string())),
        DialFailed::NoAnswer => WorkerStatus::NoAnswer,
        DialFailed::NoSuchHost => WorkerStatus::NoSuchHost,
        DialFailed::Dropped => WorkerStatus::Reconnecting(DROPPED.to_owned()),
        DialFailed::Other(why) => WorkerStatus::Reconnecting(why),
    };
    match dial {
        Dial::Hold(liveness) => away(*liveness).unwrap_or(own),
        Dial::At(_) | Dial::Unlisted => own,
    }
}

/// Why a link that was made and then ended is being dialled again.
pub(crate) const DROPPED: &str = "the link dropped";

/// The titlebar's line while the server turns this app away.
const fn refused_status(why: Refusal) -> &'static str {
    match why {
        Refusal::NotGranted => NOT_GRANTED,
        Refusal::DuplicateWorker => why.text(),
    }
}

/// Where the cached directory lives: the client's data directory.
pub(crate) fn cache_path() -> std::path::PathBuf {
    slopty_platform::dirs::data_dir().join(directory::CACHE_FILE)
}

/// What the cached directory should hold next.
#[derive(Clone, Debug)]
pub(crate) enum Cache {
    /// This server's directory.
    Keep(HostAddr, Directory),
    /// Nothing: the server was taken out of the settings, so the file goes.
    Remove,
}

/// The one task that writes the cached directory at `path`, in the order it was asked for.
///
/// A write per directory change, each on its own blocking task, raced: two in flight shared
/// the temporary file, so one renamed the other's half-written file over the cache and the
/// second failed, and an older directory could land last. Here each write finishes before the
/// next starts, and a burst of changes costs the one write of the latest. The value the
/// channel starts with is never written.
pub(crate) async fn write_cache(
    path: std::path::PathBuf,
    mut next: tokio::sync::watch::Receiver<Cache>,
) {
    while next.changed().await.is_ok() {
        let cache = next.borrow_and_update().clone();
        let path = path.clone();
        let written = tokio::task::spawn_blocking(move || match cache {
            Cache::Keep(server, directory) => directory.save(&path, &server),
            Cache::Remove => match std::fs::remove_file(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            },
        })
        .await;
        match written {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "cache the directory"),
            Err(e) => tracing::warn!(error = %e, "the directory cache's writer died"),
        }
    }
}

impl Workspace {
    /// A handle on the server link, while one runs.
    pub(crate) fn server_caller(&self) -> Option<ServerCaller> {
        let slot = self.server.as_ref()?;
        #[cfg(test)]
        if let Some(caller) = &slot.caller {
            return Some(caller.clone());
        }
        slot.task.as_ref().map(ServerTask::caller)
    }

    /// Something may have killed the server link: it is probed now, or dialled now if it is
    /// between dials ([`ServerTask::resume`]).
    pub(crate) fn resume_server(&self) {
        if let Some(task) = self.server.as_ref().and_then(|s| s.task.as_ref()) {
            task.resume();
        }
    }

    /// Use the server at `address`, or none. The same address again changes nothing; another
    /// one drops the old link and the workers only it listed. `first` is a link that already
    /// proved the address.
    pub(crate) fn set_server(
        &mut self,
        address: Option<HostAddr>,
        first: Option<ServerLink>,
        cx: &mut Context<Self>,
    ) {
        if self.server.as_ref().map(|s| &s.address) == address.as_ref() {
            return;
        }
        let previous = self.server.take();
        let had_server = previous.is_some();
        // The old link ends here, before the new one is dialled.
        drop(previous.and_then(|s| s.task));
        self.server_leads(false, cx);
        self.server_generation = self.server_generation.wrapping_add(1);
        let unlisted = self.directory.clear();
        self.view.update(cx, |v, cx| {
            v.set_server_status(None, cx);
            v.forget_server_agents(None, cx);
            v.forget_projects(cx);
            v.set_server_caller(None);
        });
        if had_server && address.is_none() {
            self.directory_cache.send_replace(Cache::Remove);
        }
        let Some(address) = address else {
            self.directory = Directory::default();
            self.drop_unlisted(&unlisted, cx);
            return;
        };
        self.directory = Directory::cached(Directory::load(&cache_path(), &address));
        let cached: Vec<(WorkerId, String)> =
            self.directory.workers().map(|w| (w.worker, w.name.clone())).collect();
        self.drop_unlisted(&unlisted, cx);
        for (id, name) in cached {
            self.add_worker(id, name, cx);
        }
        tracing::info!(server = %address, cached = self.directory.workers().count(), "server");
        let (tx, rx) = tokio::sync::oneshot::channel();
        let dial = address.clone();
        self.runtime.spawn(async move {
            let _sent = tx.send(net::serve_directory(dial, first));
        });
        let generation = self.server_generation;
        self.server = Some(ServerSlot {
            address,
            task: None,
            other_build: None,
            #[cfg(test)]
            caller: None,
        });
        cx.spawn(async move |this, cx| {
            let (task, mut events) = match rx.await {
                Ok(Ok(started)) => started,
                Ok(Err(why)) => {
                    tracing::error!(%why, "server link");
                    return;
                }
                Err(_dropped) => return,
            };
            let kept = this.update(cx, |ws, cx| match &mut ws.server {
                Some(slot) if ws.server_generation == generation => {
                    let caller = task.caller();
                    slot.task = Some(task);
                    ws.view.update(cx, |v, _cx| v.set_server_caller(Some(caller)));
                    #[cfg(target_os = "ios")]
                    {
                        ws.tell_phone(cx);
                        Self::look_at_notes(cx);
                    }
                    true
                }
                _ => false,
            });
            if !matches!(kept, Ok(true)) {
                return;
            }
            while let Some(event) = events.recv().await {
                let applied = this.update(cx, |ws, cx| {
                    if ws.server_generation == generation {
                        ws.server_event(event, cx);
                    }
                });
                if applied.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Workers that only a dropped directory listed leave with their tiles.
    fn drop_unlisted(&mut self, unlisted: &[WorkerId], cx: &mut Context<Self>) {
        for id in unlisted {
            if self.directory.get(*id).is_none() && self.slot(*id).is_some() {
                self.drop_slot(*id, cx);
            }
        }
    }

    /// Puts [`client_grant`] on the clipboard, for the tailnet's policy file.
    pub(crate) fn copy_grant(&self, cx: &mut Context<Self>) {
        self.copy_grant_text(client_grant(), cx);
    }

    /// Puts `grant` on the clipboard and says where it goes.
    pub(crate) fn copy_grant_text(&self, grant: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(grant));
        self.show_notice(GRANT_COPIED.to_owned(), cx);
    }

    /// One thing the server link said.
    pub(crate) fn server_event(&mut self, event: ServerEvent, cx: &mut Context<Self>) {
        match event {
            ServerEvent::Linked { name, link, .. } => {
                tracing::info!(%name, link, "server linked");
                if let Some(slot) = &mut self.server {
                    slot.other_build = None;
                }
                self.directory.set_server(ServerState::Linked { name, link });
                self.view.update(cx, |v, cx| v.set_server_status(None, cx));
                self.server_leads(true, cx);
            }
            ServerEvent::Unlinked { why } => self.server_down(why, UNREACHABLE, cx),
            ServerEvent::WrongBuild(notice) => self.server_other_build(&notice, cx),
            ServerEvent::Refused(why) => {
                let status = refused_status(why);
                // A refused app redials, and each redial is refused again: say it once.
                if why == Refusal::NotGranted && self.view.read(cx).server_status() != Some(status)
                {
                    tracing::warn!(
                        "the server's tailnet policy grants this device no client role; add the grant"
                    );
                    self.show_notice(GRANT_WHERE.to_owned(), cx);
                }
                self.server_down(why.text().to_owned(), status, cx);
            }
            ServerEvent::Message(msg) => {
                // The projects are the workspace's to mirror; the directory has no use for them.
                if let FromServer::Projects(part) = *msg {
                    self.view.update(cx, |v, cx| v.projects_part(*part, cx));
                    return;
                }
                let listing = matches!(*msg, FromServer::Directory(_) | FromServer::Worker(_));
                let changes = self.directory.apply(*msg);
                for change in changes {
                    self.directory_change(change, cx);
                }
                if listing {
                    self.directory_caps(cx);
                    self.tell_editor_machines(cx);
                    self.save_directory();
                }
            }
        }
        self.refresh_menu(cx);
        cx.notify();
    }

    /// The server answers on a different build: the title bar says so (not that it is
    /// unreachable, which sends the person looking for a machine that is up), and a notice says
    /// once how to bring it to this build. A server on this Mac is brought to it at once where
    /// this app is an installed build ([`Self::update_server`]).
    fn server_other_build(&mut self, notice: &UpdateNotice, cx: &mut Context<Self>) {
        let Some(slot) = &mut self.server else { return };
        let new = slot.other_build.as_ref() != Some(notice);
        slot.other_build = Some(notice.clone());
        self.server_down(notice.to_string(), OTHER_BUILD, cx);
        if !new {
            return;
        }
        tracing::warn!(server = %notice.host, peer = %notice.peer, "the server runs a different build");
        if notice.here() && self.updates_itself() {
            self.update_server(cx);
        } else {
            self.show_notice(other_build_notice(notice, self.deployer.is_some()), cx);
        }
    }

    /// What the server answered on a different build says, while it does.
    pub(crate) fn server_other_build_notice(&self) -> Option<&UpdateNotice> {
        self.server.as_ref()?.other_build.as_ref()
    }

    /// The link to the server at the address in use starts again on `link`, which reached it
    /// just now: the link it replaces waits out [`slopty_net::redial::WRONG_BUILD`] before it
    /// dials a server it last found on another build.
    pub(crate) fn relink_server(&mut self, link: ServerLink, cx: &mut Context<Self>) {
        let Some(address) = self.server_address().cloned() else {
            link.close();
            return;
        };
        drop(self.server.take());
        self.set_server(Some(address), Some(link), cx);
    }

    /// The server link is down, for `why`; the titlebar says `status` until it is back.
    fn server_down(&mut self, why: String, status: &str, cx: &mut Context<Self>) {
        let was_linked = self.directory.linked();
        self.server_leads(false, cx);
        self.directory.set_server(ServerState::Unreachable { why });
        self.view.update(cx, |v, cx| v.set_server_status(Some(status.to_owned()), cx));
        if was_linked {
            // Degraded: the cached addresses are all there is now, so try each at once.
            for slot in &self.workers {
                if !slot.linked() {
                    slot.wake();
                }
            }
        }
    }

    fn directory_change(&mut self, change: Change, cx: &mut Context<Self>) {
        match change {
            Change::Listed(id) | Change::Moved(id) => {
                let name = self.directory.get(id).map(|w| w.name.clone()).unwrap_or_default();
                // A slot new here dials as its loop starts; one waiting dials the new address.
                let waiting = self.slot(id).is_some();
                self.add_worker(id, name, cx);
                if let Some(slot) = self.slot(id).filter(|_| waiting) {
                    slot.wake();
                }
            }
            Change::Liveness { worker, now, .. } => {
                if now != Liveness::Online {
                    self.removal_went_away(worker, cx);
                }
                let key = worker_key(worker);
                let Some(slot) = self.slot(worker) else { return };
                match away(now) {
                    None => slot.wake(),
                    // A worker whose direct link still carries its tiles keeps them: that link
                    // is the data path's own judge, and a server restarted a moment ago lists
                    // every worker as gone until it registers again.
                    Some(status) if !slot.linked() => {
                        self.view.update(cx, |v, cx| v.set_worker_status(key, status, cx));
                    }
                    Some(_) => {}
                }
                if now == Liveness::Gone {
                    self.view.update(cx, |v, cx| v.forget_server_agents(Some(key), cx));
                }
            }
            // The server forgot it: it goes with its tiles, and its pages with their store.
            Change::Unlisted(id) => {
                if let Some(key) = self.slot(id).map(|slot| slot.key) {
                    self.drop_slot(id, cx);
                    Self::forget_pages(key);
                }
            }
            Change::Load(id) => {
                let load = self.directory.get(id).map(|w| w.load);
                if let (Some(load), Some(slot)) = (load, self.slot(id).filter(|s| !s.linked())) {
                    let key = slot.key;
                    self.view.update(cx, |v, cx| v.set_worker_load(key, load, cx));
                }
            }
            Change::Event(event) => match event.what {
                Happening::Project(update) => {
                    let seq = event.seq;
                    self.view.update(cx, |v, cx| v.project_update(seq, *update, cx));
                }
                // The directory carries liveness, a worker's own link its terminals, and the
                // ladder where every agent's thread stands, its rungs among them.
                Happening::Worker { .. }
                | Happening::WorkerRemoved { .. }
                | Happening::SessionOpened { .. }
                | Happening::SessionClosed { .. }
                | Happening::Rung { .. }
                | Happening::SessionExited { .. } => {}
            },
            Change::Present(present) => self.heard_present(&present),
            Change::Notice(notice) => self.heard_notice(&notice, cx),
            // What speaks for the agents and threads of the workers this client reaches only
            // through the server.
            Change::Ladder(ladder) => self.view.update(cx, |v, cx| v.server_ladder(&ladder, cx)),
        }
    }

    /// What the directory says each worker can do, for those whose own link is down: a link
    /// that is up says so itself, and more recently.
    fn directory_caps(&self, cx: &mut Context<Self>) {
        let listed: Vec<(WorkerKey, WorkerCaps, f32)> = self
            .directory
            .workers()
            .filter(|info| self.slot(info.worker).is_some_and(|slot| !slot.linked()))
            .map(|info| (worker_key(info.worker), info.caps.clone(), info.load))
            .collect();
        self.view.update(cx, |v, cx| {
            for (key, caps, load) in listed {
                v.set_worker_caps(key, caps, cx);
                v.set_worker_load(key, load, cx);
            }
        });
    }

    /// Write the directory for the next launch, off the main thread ([`write_cache`]).
    fn save_directory(&self) {
        let Some(server) = self.server.as_ref().map(|s| s.address.clone()) else { return };
        self.directory_cache.send_replace(Cache::Keep(server, self.directory.clone()));
    }

    /// What worker `id`'s connect loop does next; `None` once it is dropped.
    pub(crate) fn plan(&self, id: WorkerId) -> Option<Plan> {
        let slot: &WorkerSlot = self.slot(id)?;
        let (address, hold) = match self.directory.dial(id) {
            Dial::At(address) => (Some(address), None),
            Dial::Hold(liveness) => (self.directory_address(id), away(liveness)),
            Dial::Unlisted => (None, Some(WorkerStatus::Connecting)),
        };
        Some(Plan { key: slot.key, wake: Arc::clone(&slot.wake), address, hold })
    }

    /// The directory's address for `id`, whatever its liveness.
    fn directory_address(&self, id: WorkerId) -> Option<HostAddr> {
        let info = self.directory.get(id)?;
        HostAddr::parse_with_port(&info.address, slopty_net::endpoint::WORKER_PORT).ok()
    }

    /// How a failed dial to `id` shows ([`failure_status`]).
    pub(crate) fn failure_status(&self, id: WorkerId, failed: DialFailed) -> WorkerStatus {
        failure_status(&self.directory.dial(id), failed)
    }

    /// The server's address as shown.
    pub(crate) fn server_address(&self) -> Option<&HostAddr> {
        self.server.as_ref().map(|s| &s.address)
    }

    /// Ask the server to wake `id` from sleep; a notice says which machine sent the magic
    /// packet, or why none went. The worker coming online is the directory's news.
    pub(crate) fn wake_worker(&self, id: WorkerId, cx: &mut Context<Self>) {
        let name = self.directory.get(id).map_or_else(|| id.to_string(), |w| w.name.clone());
        let Some(caller) =
            self.server.as_ref().and_then(|s| s.task.as_ref()).map(ServerTask::caller)
        else {
            self.show_notice(format!("Could not wake {name}: the server does not answer yet"), cx);
            return;
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.runtime.spawn(async move {
            let _gone = tx.send(caller.wake(id).await);
        });
        cx.spawn(async move |this, cx| {
            let Ok(outcome) = rx.await else { return };
            let _gone = this.update(cx, |ws, cx| ws.show_notice(wake_notice(&name, outcome), cx));
        })
        .detach();
    }
}

/// What the person hears once of a server on a different build: what it runs, and how it is
/// brought to this build: from the palette where this app can run `ssh` (`deploys`), else by
/// the command on a Mac.
fn other_build_notice(notice: &UpdateNotice, deploys: bool) -> String {
    let how = if deploys {
        format!("Run \u{201c}{UPDATE_SERVER}\u{201d} from the palette.")
    } else {
        format!("Update it from a Mac with {}.", notice.command())
    };
    format!("{}. {} {how}", notice.title(), notice.detail())
}

/// What the person hears of a wake sent for the worker called `name`.
fn wake_notice(name: &str, outcome: Outcome) -> String {
    match outcome {
        Outcome::WakeSent { by, .. } => {
            format!("{by} sent {name} a wake; it shows online once it is up")
        }
        Outcome::Error { message, .. } => format!("Could not wake {name}: {message}"),
        other => {
            tracing::warn!(?other, "a wake answered with something else");
            format!("Could not wake {name}: the server answered something else")
        }
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::orchestration::ErrorCode;

    use super::*;

    /// The server named in the cache at `path`, once it is `want`; `None` for a file that is
    /// absent.
    async fn cached_server(path: &std::path::Path, want: Option<&str>) -> Option<String> {
        let read = || {
            let bytes = std::fs::read(path).ok()?;
            let cache: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
            cache["server"].as_str().map(str::to_owned)
        };
        for _ in 0..200 {
            if read().as_deref() == want {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        read()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_burst_of_directory_changes_leaves_the_last_one_cached_and_a_removal_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(directory::CACHE_FILE);
        let (tx, rx) = tokio::sync::watch::channel(Cache::Remove);
        let writer = tokio::spawn(write_cache(path.clone(), rx));
        let server = |n: u16| HostAddr::new(format!("server-{n}"), 45_560);
        for n in 0..50 {
            tx.send_replace(Cache::Keep(server(n), Directory::default()));
            tokio::task::yield_now().await;
        }
        let last = server(49).to_string();
        assert_eq!(cached_server(&path, Some(&last)).await.as_deref(), Some(last.as_str()));
        let left: Vec<_> =
            std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, [directory::CACHE_FILE], "no temporary file is left behind");

        tx.send_replace(Cache::Remove);
        assert_eq!(cached_server(&path, None).await, None);
        drop(tx);
        writer.await.unwrap();
    }

    /// A worker that turned this device away answered, so that is what shows, even where the
    /// server calls it away; any other failure shows the server's word, else its reason. A
    /// server that refuses the app says why in the titlebar.
    #[test]
    fn a_refusal_by_the_tailnet_policy_is_named_over_other_words() {
        let other = || DialFailed::Other("no answer".to_owned());
        let at = Dial::At(HostAddr::new("studio", 45_550));
        let notice = UpdateNotice {
            of: slopty_client::update::Of::Worker,
            host: "studio".to_owned(),
            peer: String::new(),
        };
        for dial in [at.clone(), Dial::Hold(Liveness::Unreachable), Dial::Unlisted] {
            assert_eq!(failure_status(&dial, DialFailed::NotGranted), WorkerStatus::NotGranted);
            assert_eq!(
                failure_status(&dial, DialFailed::WrongBuild(notice.clone())),
                WorkerStatus::NeedsUpdate(notice.clone()),
                "another build is said over the server's word"
            );
        }
        assert_eq!(
            failure_status(&Dial::Hold(Liveness::Gone), other()),
            WorkerStatus::Gone,
            "the server's word"
        );
        assert_eq!(failure_status(&at, other()), WorkerStatus::Reconnecting("no answer".into()));
        assert_eq!(refused_status(Refusal::NotGranted), NOT_GRANTED);
    }

    /// Each kind of failed dial shows as its own status: a machine that turned this device
    /// away by its ranges answered, so that is said over the server's word, with this device's
    /// address; silence and a name that does not resolve yield to the server's word.
    #[test]
    fn each_kind_of_failed_dial_is_its_own_status() {
        let at = Dial::At(HostAddr::new("studio", 45_550));
        let me = std::net::IpAddr::from([100, 64, 0, 9]);
        for dial in [at.clone(), Dial::Hold(Liveness::Unreachable)] {
            assert_eq!(
                failure_status(&dial, DialFailed::Refused(Some(me))),
                WorkerStatus::Refused(Some("100.64.0.9".to_owned()))
            );
        }
        assert_eq!(failure_status(&at, DialFailed::NoAnswer), WorkerStatus::NoAnswer);
        assert_eq!(failure_status(&at, DialFailed::NoSuchHost), WorkerStatus::NoSuchHost);
        assert_eq!(
            failure_status(&at, DialFailed::Dropped),
            WorkerStatus::Reconnecting(DROPPED.to_owned())
        );
        let held = Dial::Hold(Liveness::Unreachable);
        assert_eq!(failure_status(&held, DialFailed::NoAnswer), WorkerStatus::Unreachable);
    }

    /// A server that turns this device away says, once and not on every redial, that the
    /// tailnet policy needs a grant for it and where that is added; the grant names the
    /// capability the server reads.
    #[gpui::test]
    fn a_refused_device_is_told_where_to_grant_it_once(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = crate::tests::shell(cx, &runtime, &dir, true);
        let said = |cx: &mut gpui::VisualTestContext| {
            ws.read_with(cx, |ws, cx| {
                let view = ws.view.read(cx);
                (view.server_status().map(str::to_owned), view.toast_text())
            })
        };
        ws.update(cx, |ws, cx| ws.server_event(ServerEvent::Refused(Refusal::NotGranted), cx));
        cx.run_until_parked();
        assert_eq!(said(cx), (Some(NOT_GRANTED.to_owned()), Some(GRANT_WHERE.to_owned())));

        cx.executor().advance_clock(Duration::from_secs(20));
        cx.run_until_parked();
        assert_eq!(said(cx).1, None, "the notice went");
        ws.update(cx, |ws, cx| ws.server_event(ServerEvent::Refused(Refusal::NotGranted), cx));
        cx.run_until_parked();
        assert_eq!(said(cx), (Some(NOT_GRANTED.to_owned()), None), "said once, not per redial");

        let grant: serde_json::Value = serde_json::from_str(&client_grant()).unwrap();
        assert_eq!(grant["dst"][0], slopty_net::discover::SERVER_TAG);
        assert_eq!(grant["dst"][1], WORKER_TAG, "the workers' tag too");
        assert_eq!(grant["app"][slopty_tailnet::policy::CAP][0]["roles"][0], "client");
        let one: serde_json::Value = serde_json::from_str(&grant_to(&["100.64.0.9"])).unwrap();
        assert_eq!(one["dst"], serde_json::json!(["100.64.0.9"]), "one node, by its address");
    }

    /// The palette's command puts the grant on the clipboard, as the policy file takes it, and
    /// says so.
    #[gpui::test]
    fn the_grant_is_copied_for_the_policy_file(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (ws, cx) = crate::tests::shell(cx, &runtime, &dir, true);
        cx.dispatch_action(crate::CopyTailnetGrant);
        cx.run_until_parked();
        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(copied, Some(client_grant()));
        let said = ws.read_with(cx, |ws, cx| ws.view.read(cx).toast_text());
        assert_eq!(said.as_deref(), Some(GRANT_COPIED));
    }

    /// A server on another build is said so with both builds and the way on: the palette's
    /// line where this app runs `ssh`, else the command that brings it to this build from a
    /// Mac, which deploys over `ssh` and never reinstalls the server's own build there.
    #[test]
    fn a_server_on_another_build_is_told_with_its_way_on() {
        let notice = UpdateNotice {
            of: slopty_client::update::Of::Server,
            host: "hub".to_owned(),
            peer: "0.0.9+wire.0badf00d".to_owned(),
        };
        let here = other_build_notice(&notice, true);
        assert!(here.starts_with("The server runs a different build. It runs 0.0.9"), "{here}");
        assert!(here.ends_with(&format!("Run \u{201c}{UPDATE_SERVER}\u{201d} from the palette.")));
        let phone = other_build_notice(&notice, false);
        assert!(phone.ends_with("Update it from a Mac with slopty server deploy hub."), "{phone}");
    }

    /// A wake names the machine that sent it, and a refusal says why.
    #[test]
    fn a_wake_says_who_sent_it_or_why_none_went() {
        let sent = Outcome::WakeSent { by: "hub".to_owned(), to: vec!["en0".to_owned()] };
        assert_eq!(
            wake_notice("studio", sent),
            "hub sent studio a wake; it shows online once it is up"
        );
        let refused = Outcome::Error {
            code: ErrorCode::ServerUnreachable,
            message: "the link to the server has stopped".to_owned(),
        };
        assert_eq!(
            wake_notice("studio", refused),
            "Could not wake studio: the link to the server has stopped"
        );
    }
}
