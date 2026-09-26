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
use slopty_client::server::{ServerEvent, ServerTask};
use slopty_core::WorkerId;
use slopty_net::HostAddr;
use slopty_net::server::ServerLink;
use slopty_proto::server::{Event, FromServer, Liveness, Refusal};
use slopty_ui::workspace::WorkerStatus;

use crate::net::DialFailed;
use crate::workers::{WorkerSlot, worker_key};
use crate::{Workspace, net};

/// The status bar's word while the server does not answer.
pub const UNREACHABLE: &str = "Server unreachable";
/// The titlebar's line while the server turns this app away: the tailnet policy grants this
/// device no client role there.
pub const NOT_GRANTED: &str = "Server access not granted by the tailnet policy";

/// A worker the server says is away is still dialled this often: the server's view of it can
/// be wrong (its path to the worker broken while this client's works).
pub const HOLD_RETRY: Duration = Duration::from_secs(10);

/// The server this app is using.
#[derive(Debug)]
pub struct ServerSlot {
    /// Where it is.
    address: HostAddr,
    /// The link once it is started; dropping it ends the link and its redials.
    task: Option<ServerTask>,
}

/// What a worker's connect loop does next.
#[derive(Debug)]
pub struct Plan {
    /// The worker's key in the workspace.
    pub key: slopty_client::layout::WorkerKey,
    /// Wakes the loop out of a wait.
    pub wake: Arc<tokio::sync::Notify>,
    /// Where to dial: the directory's address, or `None` for the one it was added with.
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

/// How a failed dial shows. A worker that answered to turn this device away is reachable, so
/// that is said whatever the server thinks of it; otherwise the server's word when it has one,
/// else the reason.
fn failure_status(dial: &Dial, failed: DialFailed) -> WorkerStatus {
    let why = match failed {
        DialFailed::NotGranted => return WorkerStatus::NotGranted,
        DialFailed::Other(why) => why,
    };
    match dial {
        Dial::Hold(liveness) => away(*liveness).unwrap_or(WorkerStatus::Reconnecting(why)),
        Dial::At(_) | Dial::Unlisted => WorkerStatus::Reconnecting(why),
    }
}

/// The titlebar's line while the server turns this app away.
const fn refused_status(why: Refusal) -> &'static str {
    match why {
        Refusal::NotGranted => NOT_GRANTED,
        Refusal::DuplicateWorker => why.text(),
    }
}

/// Where the cached directory lives: the client's data directory.
pub fn cache_path() -> std::path::PathBuf {
    slopty_settings::data_dir().join(directory::CACHE_FILE)
}

/// What the cached directory should hold next.
#[derive(Clone, Debug)]
pub enum Cache {
    /// This server's directory.
    Keep(HostAddr, Directory),
    /// Nothing: the server was disconnected, so the file goes.
    Remove,
}

/// The one task that writes the cached directory at `path`, in the order it was asked for.
///
/// A write per directory change, each on its own blocking task, raced: two in flight shared
/// the temporary file, so one renamed the other's half-written file over the cache and the
/// second failed, and an older directory could land last. Here each write finishes before the
/// next starts, and a burst of changes costs the one write of the latest. The value the
/// channel starts with is never written.
pub async fn write_cache(path: std::path::PathBuf, mut next: tokio::sync::watch::Receiver<Cache>) {
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
        self.server_generation = self.server_generation.wrapping_add(1);
        let unlisted = self.directory.clear();
        self.view.update(cx, |v, cx| {
            v.set_server_status(None, cx);
            v.forget_server_agents(None, cx);
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
            self.add_worker(id, name, false, cx);
        }
        tracing::info!(server = %address, cached = self.directory.workers().count(), "server");
        let (tx, rx) = tokio::sync::oneshot::channel();
        let dial = address.clone();
        self.runtime.spawn(async move {
            let _sent = tx.send(net::serve_directory(dial, first));
        });
        let generation = self.server_generation;
        self.server = Some(ServerSlot { address, task: None });
        cx.spawn(async move |this, cx| {
            let (task, mut events) = match rx.await {
                Ok(Ok(started)) => started,
                Ok(Err(why)) => {
                    tracing::error!(%why, "server link");
                    return;
                }
                Err(_dropped) => return,
            };
            let kept = this.update(cx, |ws, _cx| match &mut ws.server {
                Some(slot) if ws.server_generation == generation => {
                    slot.task = Some(task);
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
            if self.directory.get(*id).is_none() && self.slot(*id).is_some_and(|s| !s.added) {
                self.drop_slot(*id, cx);
            }
        }
    }

    /// One thing the server link said.
    fn server_event(&mut self, event: ServerEvent, cx: &mut Context<Self>) {
        match event {
            ServerEvent::Linked { name } => {
                tracing::info!(%name, "server linked");
                self.directory.set_server(ServerState::Linked { name });
                self.view.update(cx, |v, cx| v.set_server_status(None, cx));
            }
            ServerEvent::Unlinked { why } => self.server_down(why, UNREACHABLE, cx),
            ServerEvent::Refused(why) => {
                self.server_down(why.text().to_owned(), refused_status(why), cx);
            }
            ServerEvent::Message(msg) => {
                let listing = matches!(*msg, FromServer::Directory(_) | FromServer::Worker(_));
                let changes = self.directory.apply(*msg);
                for change in changes {
                    self.directory_change(change, cx);
                }
                if listing {
                    self.save_directory();
                }
            }
        }
        self.refresh_menu(cx);
        cx.notify();
    }

    /// The server link is down, for `why`; the titlebar says `status` until it is back.
    fn server_down(&mut self, why: String, status: &str, cx: &mut Context<Self>) {
        let was_linked = self.directory.linked();
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
                self.add_worker(id, name, false, cx);
                if let Some(slot) = self.slot(id) {
                    slot.wake();
                }
            }
            Change::Liveness { worker, now, .. } => {
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
            Change::Unlisted(id) => {
                if self.slot(id).is_some_and(|s| !s.added) {
                    self.drop_slot(id, cx);
                }
            }
            Change::Event(event) => match *event {
                Event::Agent { worker, event } => {
                    let key = worker_key(worker);
                    self.view.update(cx, |v, cx| v.server_agent_event(key, event, cx));
                }
                Event::SessionClosed { session, .. } => {
                    self.view.update(cx, |v, cx| v.server_session_closed(session, cx));
                }
                Event::SessionOpened { .. } => {}
            },
        }
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
            Dial::Unlisted if slot.added => (None, None),
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
}

#[cfg(test)]
mod tests {
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
        assert!(!path.with_extension("json.tmp").exists(), "no temporary file is left behind");

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
        for dial in [at.clone(), Dial::Hold(Liveness::Unreachable), Dial::Unlisted] {
            assert_eq!(failure_status(&dial, DialFailed::NotGranted), WorkerStatus::NotGranted);
        }
        assert_eq!(
            failure_status(&Dial::Hold(Liveness::Gone), other()),
            WorkerStatus::Gone,
            "the server's word"
        );
        assert_eq!(failure_status(&at, other()), WorkerStatus::Reconnecting("no answer".into()));
        assert_eq!(refused_status(Refusal::NotGranted), NOT_GRANTED);
    }
}
