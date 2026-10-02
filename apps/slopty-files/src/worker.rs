//! One worker's files over a link the extension opens itself.
//!
//! The system runs the extension whether the app is open or not, so it is a client of its own:
//! it dials the worker at the addresses the app wrote (`slopty_platform::files::Directory`),
//! lists folders with the worker's `ListFolder`, watches the folders the system was given with
//! `WatchFolders`, and brings a file down with a transfer, as a file tile does.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use slopty_client::xfer::XferError;
use slopty_client::{LinkEvent, WorkerLink};
use slopty_core::{ClientId, WorkerId, XferId};
use slopty_net::HostAddr;
use slopty_net::client::{WorkerConn, bind_client, connect};
use slopty_net::endpoint::WORKER_PORT;
use slopty_platform::files::Known;
use slopty_proto::folder::Listing;
use slopty_proto::handshake::Hello;
use slopty_proto::{ClientMsg, WorkerMsg};
use tokio::sync::{mpsc, oneshot};

use crate::item::{self, Item};

/// How long a worker may take to answer a dial or a listing.
pub const ANSWER: Duration = Duration::from_secs(15);

/// The name the worker shows for the extension among its clients.
pub const CLIENT_NAME: &str = "Finder";

/// Why the extension could not do what the system asked.
#[derive(Debug, thiserror::Error)]
pub enum FilesError {
    /// Nothing is at that path on the worker.
    #[error("{0}: no such file or folder")]
    NoSuchItem(String),
    /// A folder was asked of something that is not one.
    #[error("{0}: not a folder")]
    NotFolder(String),
    /// The worker could not be reached, or stopped answering.
    #[error("the worker is out of reach: {0}")]
    Unreachable(String),
    /// Another worker answered at the address the app wrote for this one.
    #[error("{name} answered for another worker")]
    WrongWorker {
        /// The name of the worker that answered.
        name: String,
    },
    /// The worker refused the listing, in these words.
    #[error("{path}: {error}")]
    Refused {
        /// The path.
        path: String,
        /// The worker's word for it.
        error: String,
    },
    /// The transfer of a file's bytes failed.
    #[error(transparent)]
    Transfer(#[from] XferError),
}

/// A folder's listing the worker sent unasked: a watched folder changed.
#[derive(Debug)]
pub struct Pushed {
    /// The folder, as the watch named it.
    pub path: String,
    /// What is there now.
    pub listing: Listing,
}

type Waiting = Arc<Mutex<HashMap<String, Vec<oneshot::Sender<Listing>>>>>;

/// A link to one worker.
#[derive(Debug)]
pub struct Worker {
    link: WorkerLink,
    name: String,
    waiting: Waiting,
    alive: Arc<AtomicBool>,
    _endpoint: slopty_net::Endpoint,
    reader: tokio::task::JoinHandle<()>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl Worker {
    /// Dial `known` at each of its addresses in turn; what the watched folders send unasked
    /// goes to `pushes`.
    ///
    /// # Errors
    ///
    /// [`FilesError::Unreachable`] when none answers, and [`FilesError::WrongWorker`] when
    /// another worker does.
    pub async fn open(known: &Known, pushes: mpsc::Sender<Pushed>) -> Result<Self, FilesError> {
        let endpoint = bind_client().map_err(|e| FilesError::Unreachable(e.to_string()))?;
        let mut last = FilesError::Unreachable(format!("{} has no address", known.name));
        for addr in &known.addrs {
            let addr = match HostAddr::parse_with_port(addr, WORKER_PORT) {
                Ok(addr) => addr,
                Err(e) => {
                    last = FilesError::Unreachable(format!("{addr}: {e}"));
                    continue;
                }
            };
            let hello = Hello { client: ClientId::new(), name: CLIENT_NAME.to_owned() };
            match tokio::time::timeout(ANSWER, connect(&endpoint, &addr, hello)).await {
                Ok(Ok(conn)) if conn.ack.worker == known.id => {
                    return Ok(Self::start(conn, endpoint, &known.name, pushes));
                }
                Ok(Ok(conn)) => last = FilesError::WrongWorker { name: conn.ack.name },
                Ok(Err(e)) => last = FilesError::Unreachable(format!("{addr}: {e}")),
                Err(_elapsed) => last = FilesError::Unreachable(format!("{addr}: no answer")),
            }
        }
        Err(last)
    }

    fn start(
        conn: WorkerConn,
        endpoint: slopty_net::Endpoint,
        name: &str,
        pushes: mpsc::Sender<Pushed>,
    ) -> Self {
        let mut link = WorkerLink::start(conn);
        let waiting: Waiting = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));
        let events = link.events();
        let (answered, living) = (Arc::clone(&waiting), Arc::clone(&alive));
        let reader = tokio::spawn(async move {
            if let Some(mut events) = events {
                while let Some(event) = events.recv().await {
                    match event {
                        LinkEvent::Control(WorkerMsg::Folder { path, listing }) => {
                            let waiters = answered.lock().remove(&path);
                            match waiters {
                                Some(waiters) => {
                                    for waiter in waiters {
                                        let _gone = waiter.send(listing.clone());
                                    }
                                }
                                None => {
                                    let _gone = pushes.send(Pushed { path, listing }).await;
                                }
                            }
                        }
                        LinkEvent::Disconnected(why) => {
                            tracing::info!(%why, "the worker's link closed");
                            break;
                        }
                        _other => {}
                    }
                }
            }
            living.store(false, Ordering::Release);
            // Every listing still waiting hears the link went as its sender drops.
            answered.lock().clear();
        });
        Self { link, name: name.to_owned(), waiting, alive, _endpoint: endpoint, reader }
    }

    /// The worker's id, which names its domain.
    #[must_use]
    pub const fn id(&self) -> WorkerId {
        self.link.ack().worker
    }

    /// The worker's home, the domain's root.
    #[must_use]
    pub fn home(&self) -> &str {
        &self.link.ack().home
    }

    /// The link is still up.
    #[must_use]
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    /// The folder `id`'s files and folders.
    ///
    /// # Errors
    ///
    /// The folder is not there or not one, the worker refused it, or the link went.
    pub async fn list(&self, id: &str) -> Result<Vec<Item>, FilesError> {
        let path = item::on_worker(self.home(), id);
        let (tx, rx) = oneshot::channel();
        self.waiting.lock().entry(path.clone()).or_default().push(tx);
        self.send(ClientMsg::ListFolder { path: path.clone() }).await?;
        let listing = tokio::time::timeout(ANSWER, rx)
            .await
            .map_err(|_elapsed| FilesError::Unreachable(format!("{path}: no answer")))?
            .map_err(|_gone| FilesError::Unreachable("the link closed".to_owned()))?;
        items(id, &path, listing)
    }

    /// The item `id`, as its folder lists it; the root is the worker's home.
    ///
    /// # Errors
    ///
    /// [`FilesError::NoSuchItem`] when its folder does not hold it, and what [`Self::list`]
    /// returns.
    pub async fn item(&self, id: &str) -> Result<Item, FilesError> {
        if id == item::ROOT {
            return Ok(Item::root(&self.name));
        }
        let name = item::name(id);
        self.list(item::parent(id))
            .await?
            .into_iter()
            .find(|item| item.name == name)
            .ok_or_else(|| FilesError::NoSuchItem(item::on_worker(self.home(), id)))
    }

    /// Watch the folders `ids`, and only those: a change in one comes as a [`Pushed`].
    ///
    /// # Errors
    ///
    /// The link went.
    pub async fn watch(&self, ids: impl Iterator<Item = &str>) -> Result<(), FilesError> {
        let paths = ids.map(|id| item::on_worker(self.home(), id)).collect();
        self.send(ClientMsg::WatchFolders { paths }).await
    }

    /// Bring the file `id` down into the directory `into` as transfer `xfer`; the path it
    /// landed at. [`Self::cancel`] with the same `xfer` stops it.
    ///
    /// # Errors
    ///
    /// The transfer failed or was cancelled, or nothing landed.
    pub async fn fetch(&self, id: &str, into: &Path, xfer: XferId) -> Result<PathBuf, FilesError> {
        let path = item::on_worker(self.home(), id);
        let landed = self.link.download(xfer, path.clone(), into.to_path_buf()).await?;
        landed.into_iter().next().ok_or(FilesError::NoSuchItem(path))
    }

    /// Stop transfer `xfer`.
    pub fn cancel(&self, xfer: XferId) {
        self.link.remote().cancel(xfer);
    }

    async fn send(&self, msg: ClientMsg) -> Result<(), FilesError> {
        self.link.send(msg).await.map_err(|e| FilesError::Unreachable(e.to_string()))
    }
}

/// The items of folder `id`, at `path` on the worker, from its listing.
///
/// # Errors
///
/// The listing says it is no folder, or is not there.
pub fn items(id: &str, path: &str, listing: Listing) -> Result<Vec<Item>, FilesError> {
    match listing {
        Listing::Listed { entries, total, .. } => {
            if usize::try_from(total).is_ok_and(|total| total > entries.len()) {
                tracing::info!(path, total, shown = entries.len(), "a folder listed in part");
            }
            Ok(entries.iter().filter_map(|e| Item::of_entry(id, e)).collect())
        }
        Listing::NotFolder => Err(FilesError::NotFolder(path.to_owned())),
        Listing::Missing { error } => Err(FilesError::Refused { path: path.to_owned(), error }),
    }
}
