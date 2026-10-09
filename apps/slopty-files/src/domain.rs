//! One worker's domain: its link, opened when the system first asks and again once it drops,
//! the folders the system was given, kept watched, and the changes the worker reports.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::Mutex;
use slopty_core::{WorkerId, XferId};
use slopty_platform::files::Directory;
use tokio::sync::mpsc;

use crate::changes::{Change, Changes, Expired};
use crate::item::{self, Item};
use crate::pages::Cursor;
use crate::worker::{FilesError, Pushed, Worker};

/// The first wait before dialing a worker again that did not answer, doubled up to
/// [`REDIAL_MOST`] while a fetch waits for it.
const REDIAL_FIRST: Duration = Duration::from_secs(1);

/// The longest wait between two dials of a worker a fetch waits for.
const REDIAL_MOST: Duration = Duration::from_secs(10);

/// Called when there are changes for the system to ask for.
pub type Signal = Arc<dyn Fn() + Send + Sync>;

/// One worker's domain.
pub struct Domain {
    id: WorkerId,
    /// The container the app writes the directory to.
    shared: PathBuf,
    link: tokio::sync::Mutex<Option<Arc<Worker>>>,
    changes: Arc<Mutex<Changes>>,
    signal: Signal,
}

impl std::fmt::Debug for Domain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Domain").field("id", &self.id).finish_non_exhaustive()
    }
}

impl Domain {
    /// The domain of worker `id`, found in the directory in `shared`; `signal` is called
    /// whenever the worker reports a change in a folder the system was given.
    #[must_use]
    pub fn new(id: WorkerId, shared: PathBuf, signal: Signal) -> Self {
        Self { id, shared, link: tokio::sync::Mutex::default(), changes: Arc::default(), signal }
    }

    /// The worker this domain is for.
    #[must_use]
    pub const fn id(&self) -> WorkerId {
        self.id
    }

    /// The item `id`.
    ///
    /// # Errors
    ///
    /// The worker is out of reach, or holds no such item.
    pub async fn item(&self, id: &str) -> Result<Item, FilesError> {
        self.worker().await?.item(id).await
    }

    /// The items of folder `id`, every page of them ([`Self::list_page`]).
    ///
    /// # Errors
    ///
    /// The worker is out of reach, or the folder is not there or not one.
    pub async fn list(&self, id: &str) -> Result<Vec<Item>, FilesError> {
        let mut all = Vec::new();
        let mut from = None;
        loop {
            let page = self.list_page(id, from.as_ref()).await?;
            all.extend(page.items);
            match page.next {
                Some(next) => from = Some(next),
                None => return Ok(all),
            }
        }
    }

    /// A page of the items of folder `id`, which the system is given: its first, or the one
    /// at `from`. Once its last page is given, the worker's changes to it are logged for
    /// [`Self::since`].
    ///
    /// # Errors
    ///
    /// The worker is out of reach, or the folder is not there or not one.
    pub async fn list_page(&self, id: &str, from: Option<&Cursor>) -> Result<Page, FilesError> {
        let worker = self.worker().await?;
        let path = item::on_worker(worker.home(), id);
        let listed = worker.listing(id, from.map(|c| &c.after)).await.and_then(|listing| {
            let next = Cursor::next(from.map_or(0, |c| c.held), &listing);
            Ok(Page { items: crate::worker::items(id, &path, listing)?, next })
        });
        let watch = {
            let mut changes = self.changes.lock();
            match &listed {
                Ok(page) => {
                    let new = !changes.folders().any(|folder| folder == id);
                    let last = page.next.is_none();
                    let _changed = changes.page(id, from.is_none(), page.items.clone(), last);
                    let given = changes.folders().any(|folder| folder == id);
                    (new && given).then(|| changes.folders().map(str::to_owned).collect::<Vec<_>>())
                }
                Err(FilesError::NotFolder(_) | FilesError::Refused { .. }) => {
                    changes.gone(id).then(|| changes.folders().map(str::to_owned).collect())
                }
                Err(_) => None,
            }
        };
        if let Some(folders) = watch {
            worker.watch(folders.iter().map(String::as_str)).await?;
        }
        listed
    }

    /// Bring the file `id` down into `into` as transfer `xfer`; where it landed, and the item
    /// as its folder lists it now, its version with it.
    ///
    /// A link that goes meanwhile is dialed again, and the transfer goes on over the new one
    /// from what it holds (`slopty_client::xfer::Line`).
    ///
    /// # Errors
    ///
    /// The worker is out of reach, the file is gone, or the transfer failed or was
    /// cancelled.
    pub async fn fetch(
        &self,
        id: &str,
        into: &Path,
        xfer: XferId,
    ) -> Result<(PathBuf, Item), FilesError> {
        let worker = self.worker().await?;
        let landed = tokio::select! {
            landed = worker.fetch(id, into, xfer) => landed?,
            never = self.keep_linked() => match never {},
        };
        Ok((landed, self.worker().await?.item(id).await?))
    }

    /// Stop transfer `xfer`, on whichever link it is, or while it waits for one.
    pub async fn cancel(&self, xfer: XferId) {
        if slopty_client::xfer::Line::of(self.id).cancel(xfer) {
            return;
        }
        if let Some(worker) = self.link.lock().await.as_ref() {
            worker.cancel(xfer);
        }
    }

    /// Dial the worker again each time its link goes, for as long as it is polled: a fetch
    /// waiting for the next link gets one even when the system asks nothing else meanwhile.
    #[expect(clippy::infinite_loop, reason = "polled only beside a fetch, which ends it")]
    async fn keep_linked(&self) -> std::convert::Infallible {
        let mut wait = REDIAL_FIRST;
        loop {
            match self.worker().await {
                Ok(worker) => {
                    worker.closed().await;
                    wait = REDIAL_FIRST;
                }
                Err(e) => {
                    tracing::debug!(error = %e, ?wait, "the worker is out of reach; dialing again");
                    tokio::time::sleep(wait).await;
                    wait = wait.saturating_mul(2).min(REDIAL_MOST);
                }
            }
        }
    }

    /// The anchor that stands for every change logged so far.
    #[must_use]
    pub fn anchor(&self) -> Vec<u8> {
        self.changes.lock().anchor()
    }

    /// The changes after `anchor`, and the anchor that stands for them read.
    ///
    /// # Errors
    ///
    /// [`Expired`] for an anchor of another run, or older than every change kept.
    pub fn since(&self, anchor: &[u8]) -> Result<(Vec<Change>, Vec<u8>), Expired> {
        self.changes.lock().since(anchor)
    }

    /// The worker's link: the one up, or a new one dialed at the addresses the directory has
    /// for it now, the folders given so far watched again on it.
    async fn worker(&self) -> Result<Arc<Worker>, FilesError> {
        // Held through the dial, so two asks at once open one link.
        let mut link = self.link.lock().await;
        if let Some(worker) = link.as_ref().filter(|w| w.alive()) {
            return Ok(Arc::clone(worker));
        }
        let directory = Directory::read(&self.shared)
            .map_err(|e| FilesError::Unreachable(format!("the app's list of workers: {e}")))?;
        let known = directory.get(self.id).ok_or_else(|| {
            FilesError::Unreachable(format!("worker {} is no longer in the app", self.id))
        })?;
        let (tx, rx) = mpsc::unbounded_channel();
        let worker = Arc::new(Worker::open(known, tx).await?);
        let folders: Vec<String> = self.changes.lock().folders().map(str::to_owned).collect();
        if !folders.is_empty() {
            worker.watch(folders.iter().map(String::as_str)).await?;
        }
        tokio::spawn(take_in(
            rx,
            Arc::downgrade(&worker),
            worker.home().to_owned(),
            Arc::clone(&self.changes),
            Arc::clone(&self.signal),
        ));
        *link = Some(Arc::clone(&worker));
        drop(link);
        Ok(worker)
    }
}

/// Log what each listing the worker sends unasked changed, and say so. A listing is a folder's
/// first page: one of a big folder is made whole with its pages first, over the link it came
/// on, and the listings that came meanwhile wait, all but each folder's last dropped.
async fn take_in(
    mut pushes: mpsc::UnboundedReceiver<Pushed>,
    worker: Weak<Worker>,
    home: String,
    changes: Arc<Mutex<Changes>>,
    signal: Signal,
) {
    while let Some(first) = pushes.recv().await {
        let mut came = vec![first];
        while let Ok(more) = pushes.try_recv() {
            came.retain(|held| held.path != more.path);
            came.push(more);
        }
        let mut changed = false;
        for Pushed { path, listing } in came {
            let Some(folder) = slopty_proto::folder::under_home(&home, &path) else { continue };
            let Some(link) = worker.upgrade() else { return };
            let listing = match link.rest(&folder, listing).await {
                Ok(listing) => listing,
                Err(e) => {
                    tracing::info!(%path, error = %e, "a changed folder not listed whole");
                    continue;
                }
            };
            drop(link);
            changed |= match crate::worker::items(&folder, &path, listing) {
                Ok(items) => changes.lock().listed(&folder, items),
                Err(_) => changes.lock().gone(&folder),
            };
        }
        if changed {
            signal();
        }
    }
}

/// A page of a folder's items, and where the next starts; `None` after its last.
#[derive(Clone, Debug)]
pub struct Page {
    /// The items.
    pub items: Vec<Item>,
    /// Where the next page starts.
    pub next: Option<Cursor>,
}
