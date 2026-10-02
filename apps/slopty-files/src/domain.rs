//! One worker's domain: its link, opened when the system first asks and again once it drops,
//! the folders the system was given, kept watched, and the changes the worker reports.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use slopty_core::{WorkerId, XferId};
use slopty_platform::files::Directory;
use tokio::sync::mpsc;

use crate::changes::{Change, Changes, Expired};
use crate::item::{self, Item};
use crate::worker::{FilesError, Pushed, Worker};

/// How many unasked listings may wait for the domain to take them in.
const PUSHES: usize = 64;

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

    /// The items of folder `id`, which the system is given: from now on, the worker's changes
    /// to it are logged for [`Self::since`].
    ///
    /// # Errors
    ///
    /// The worker is out of reach, or the folder is not there or not one.
    pub async fn list(&self, id: &str) -> Result<Vec<Item>, FilesError> {
        let worker = self.worker().await?;
        let listed = worker.list(id).await;
        let watch = {
            let mut changes = self.changes.lock();
            match &listed {
                Ok(items) => {
                    let new = !changes.folders().any(|folder| folder == id);
                    let _changed = changes.listed(id, items.clone());
                    new.then(|| changes.folders().map(str::to_owned).collect::<Vec<_>>())
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
        let landed = worker.fetch(id, into, xfer).await?;
        Ok((landed, worker.item(id).await?))
    }

    /// Stop transfer `xfer`, if a link is up to stop it on.
    pub async fn cancel(&self, xfer: XferId) {
        if let Some(worker) = self.link.lock().await.as_ref() {
            worker.cancel(xfer);
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
        let (tx, rx) = mpsc::channel(PUSHES);
        let worker = Arc::new(Worker::open(known, tx).await?);
        let folders: Vec<String> = self.changes.lock().folders().map(str::to_owned).collect();
        if !folders.is_empty() {
            worker.watch(folders.iter().map(String::as_str)).await?;
        }
        tokio::spawn(take_in(
            rx,
            worker.home().to_owned(),
            Arc::clone(&self.changes),
            Arc::clone(&self.signal),
        ));
        *link = Some(Arc::clone(&worker));
        drop(link);
        Ok(worker)
    }
}

/// Log what each listing the worker sends unasked changed, and say so.
async fn take_in(
    mut pushes: mpsc::Receiver<Pushed>,
    home: String,
    changes: Arc<Mutex<Changes>>,
    signal: Signal,
) {
    while let Some(Pushed { path, listing }) = pushes.recv().await {
        let Some(folder) = item::of_worker_path(&home, &path) else { continue };
        let changed = match crate::worker::items(&folder, &path, listing) {
            Ok(items) => changes.lock().listed(&folder, items),
            Err(_) => changes.lock().gone(&folder),
        };
        if changed {
            signal();
        }
    }
}
