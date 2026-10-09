//! One worker's domain: its link, opened when the system first asks and again once it drops,
//! the folders the system was given, kept watched, and the changes the worker reports.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::Mutex;
use slopty_core::{WorkerId, XferId};
use slopty_platform::files::Directory;
use slopty_proto::folder::{FileVersion, FsOp};
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

/// The most names a conflicted copy tries before it is sent up anew under the first.
const COPY_NAMES: u32 = 20;

/// How a file saved in Finder went back to the worker ([`Domain::replace`]).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Written {
    /// Its contents are the file's on the worker now.
    Replaced(Item),
    /// The file changed on the worker since it was opened here, so it is left as it is, and
    /// what was saved here lands beside it as a conflicted copy: nothing of either is lost.
    Kept {
        /// The file as the worker has it, which the system fetches again.
        now: Item,
        /// The copy holding what was saved here.
        copy: Item,
    },
}

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

    /// Make the folder `name` in the folder `parent`; the item it is.
    ///
    /// # Errors
    ///
    /// The worker is out of reach, `parent` is not there ([`FilesError::NoSuchItem`]),
    /// something is already called `name` there ([`FilesError::Clash`]), or the worker would
    /// not or could not make it.
    pub async fn make_folder(&self, parent: &str, name: &str) -> Result<Item, FilesError> {
        let id = named(parent, name)?;
        let worker = self.worker().await?;
        let op =
            FsOp::MakeDir { parent: item::on_worker(worker.home(), parent), name: name.to_owned() };
        worker.change(op).await?;
        worker.item(&id).await
    }

    /// Send the file `local` up into the folder `parent` as transfer `xfer`; the item it is
    /// there, under its own name or, when that was taken, the next free one, as a file dropped
    /// on the worker lands. Nothing on the worker is written over.
    ///
    /// A link that goes meanwhile is dialed again, and the transfer goes on over the new one
    /// from what the worker holds.
    ///
    /// # Errors
    ///
    /// The worker is out of reach, or the transfer failed or was cancelled.
    pub async fn create_file(
        &self,
        parent: &str,
        local: &Path,
        xfer: XferId,
    ) -> Result<Item, FilesError> {
        let worker = self.worker().await?;
        let landed = tokio::select! {
            landed = worker.upload(local, parent, xfer) => landed?,
            never = self.keep_linked() => match never {},
        };
        let name = landed.rsplit('/').next().unwrap_or(&landed);
        let id = named(parent, name)?;
        self.worker().await?.item(&id).await
    }

    /// Put the contents at `local`, saved in Finder, in place of the file `id` on the worker,
    /// only while it is still at `base`, the version they were made from. Sent up first into
    /// the worker's drop directory as transfer `xfer`, then swapped in by the worker, so the
    /// file is never half written. A file changed on the worker meanwhile (or a `base` not
    /// known) is left as it is, and what was saved lands beside it as a conflicted copy
    /// ([`Written::Kept`]), staged under its name in `temporary` when it has to be sent up
    /// anew.
    ///
    /// # Errors
    ///
    /// The worker is out of reach, the transfer failed or was cancelled, or the worker would
    /// not or could not put the file in place, or keep the copy.
    pub async fn replace(
        &self,
        id: &str,
        local: &Path,
        base: Option<FileVersion>,
        (temporary, xfer): (&Path, XferId),
    ) -> Result<Written, FilesError> {
        let worker = self.worker().await?;
        let landed = tokio::select! {
            landed = worker.upload_staged(local, xfer) => landed?,
            never = self.keep_linked() => match never {},
        };
        let worker = self.worker().await?;
        if let Some(base) = base {
            let path = item::on_worker(worker.home(), id);
            match worker.change(FsOp::Replace { path, with: landed.clone(), base }).await {
                Ok(_replaced) => return worker.item(id).await.map(Written::Replaced),
                Err(FilesError::Changed { now, .. }) => {
                    tracing::info!(%id, ?base, ?now, "saved over a change on the worker: kept a copy");
                }
                Err(e) => return Err(e),
            }
        }
        let copy = self.keep_copy(&worker, id, &landed, (local, temporary)).await?;
        let now = worker.item(id).await?;
        Ok(Written::Kept { now, copy })
    }

    /// Put the file `landed` in the worker's drop directory beside `id` as its conflicted
    /// copy, under the first free name ([`conflicted`]); sent up anew from `local`, staged in
    /// `temporary`, when it cannot be moved there (another volume).
    async fn keep_copy(
        &self,
        worker: &Worker,
        id: &str,
        landed: &str,
        (local, temporary): (&Path, &Path),
    ) -> Result<Item, FilesError> {
        let (parent, name) = (item::parent(id), item::name(id));
        for n in 1..=COPY_NAMES {
            let copy = named(parent, &conflicted(name, n))?;
            let op =
                FsOp::Move { from: landed.to_owned(), to: item::on_worker(worker.home(), &copy) };
            match worker.change(op).await {
                Ok(_moved) => return worker.item(&copy).await,
                Err(FilesError::Clash(_)) => {}
                Err(FilesError::Declined { .. }) => break,
                Err(e) => return Err(e),
            }
        }
        // Lands as the next free name when even that is taken.
        let xfer = XferId::new();
        let staging = temporary.join(xfer.to_string());
        let staged = staging.join(conflicted(name, 1));
        tokio::fs::create_dir_all(&staging)
            .await
            .map_err(|source| local_error(&staging, source))?;
        if tokio::fs::hard_link(local, &staged).await.is_err() {
            tokio::fs::copy(local, &staged).await.map_err(|source| local_error(&staged, source))?;
        }
        let sent = self.create_file(parent, &staged, xfer).await;
        if let Err(e) = tokio::fs::remove_dir_all(&staging).await {
            tracing::debug!(error = %e, "a staged copy not cleared");
        }
        sent
    }

    /// Move the item `id` into the folder `parent` as `name`, a rename when its folder stays;
    /// the item it is there. When `id` is gone and the item is already there (it went with a
    /// folder moved before it), it is that item.
    ///
    /// # Errors
    ///
    /// The worker is out of reach, `id` is not there ([`FilesError::NoSuchItem`]), something
    /// is already at the new place ([`FilesError::Clash`]), or the worker would not or could
    /// not move it.
    pub async fn rename(&self, id: &str, parent: &str, name: &str) -> Result<Item, FilesError> {
        let to = named(parent, name)?;
        let worker = self.worker().await?;
        if to == id {
            return worker.item(id).await;
        }
        let op = FsOp::Move {
            from: item::on_worker(worker.home(), id),
            to: item::on_worker(worker.home(), &to),
        };
        match worker.change(op).await {
            Ok(_moved) => worker.item(&to).await,
            Err(gone @ FilesError::NoSuchItem(_)) => {
                worker.item(&to).await.map_err(|_not_there| gone)
            }
            Err(e) => Err(e),
        }
    }

    /// Move the item `id` to the worker's own trash, where the person can put it back; where
    /// it went there, or `None` when it was already gone.
    ///
    /// # Errors
    ///
    /// The worker is out of reach, or would not or could not trash it (its volume has no
    /// trash, or it holds the home).
    pub async fn trash(&self, id: &str) -> Result<Option<String>, FilesError> {
        let worker = self.worker().await?;
        let op = FsOp::Trash { path: item::on_worker(worker.home(), id) };
        match worker.change(op).await {
            Ok(trashed) => Ok(Some(trashed)),
            Err(FilesError::NoSuchItem(_)) => Ok(None),
            Err(e) => Err(e),
        }
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

/// The identifier of the entry `name` of the folder `parent`.
///
/// # Errors
///
/// [`FilesError::Declined`] for a name no single path component can be.
/// The name of `name`'s `n`th conflicted copy: "notes (conflicted copy).txt", then
/// "notes (conflicted copy 2).txt"; a name with no extension, or a dot file's, takes it at its
/// end.
fn conflicted(name: &str, n: u32) -> String {
    let mark = if n <= 1 { "conflicted copy".to_owned() } else { format!("conflicted copy {n}") };
    match name.rsplit_once('.').filter(|(stem, _)| !stem.is_empty()) {
        Some((stem, ext)) => format!("{stem} ({mark}).{ext}"),
        None => format!("{name} ({mark})"),
    }
}

/// A local file the extension could not stage.
fn local_error(path: &Path, source: std::io::Error) -> FilesError {
    FilesError::Transfer(slopty_client::xfer::XferError::Local {
        path: path.display().to_string(),
        source,
    })
}

fn named(parent: &str, name: &str) -> Result<String, FilesError> {
    item::child(parent, name).ok_or_else(|| FilesError::Declined {
        path: name.to_owned(),
        why: format!("“{name}” cannot be a file’s name"),
    })
}

/// A page of a folder's items, and where the next starts; `None` after its last.
#[derive(Clone, Debug)]
pub struct Page {
    /// The items.
    pub items: Vec<Item>,
    /// Where the next page starts.
    pub next: Option<Cursor>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A conflicted copy keeps its file's extension, and a later one is numbered.
    #[test]
    fn a_conflicted_copy_is_named_beside_its_file() {
        assert_eq!(conflicted("notes.txt", 1), "notes (conflicted copy).txt");
        assert_eq!(conflicted("notes.txt", 2), "notes (conflicted copy 2).txt");
        assert_eq!(conflicted("Makefile", 1), "Makefile (conflicted copy)");
        assert_eq!(conflicted(".env", 1), ".env (conflicted copy)");
        assert_eq!(conflicted("a.tar.gz", 1), "a.tar (conflicted copy).gz");
    }
}
