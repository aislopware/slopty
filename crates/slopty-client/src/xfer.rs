//! Files both ways: an upload of dropped files, a download of a worker file dragged out, and the
//! pure pieces around them (which files a drop is, how a path is typed into a shell).
//!
//! An upload announces itself with [`XferMsg::Begin`], then sends one bulk stream per file,
//! one after another, at bulk priority. A stream cut short is resumed: the sender asks the worker
//! how much of the file is durable ([`XferMsg::Resume`] → [`XferMsg::Offset`]) and sends the rest.
//! A download is the worker's bulk streams landing in a directory as `name.partial`, synced as
//! they go, checked against the digest in the worker's [`XferMsg::Done`] and renamed when whole.
//! A cut download fetches again, naming what it holds of each file, and each file resumes from
//! there ([`download`]).
//!
//! Either keeps running while the app is off screen, with the system's progress UI
//! (`offscreen`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use slopty_core::{WallMs, XferId, shell_quote};
use slopty_net::streams::RawRecv;
use slopty_net::{ClientMsg, Connection, NetError};
use slopty_proto::transfer::{
    BulkHeader, Dest, Hash, MAX_FILES, MODE_BITS, Purpose, XferMsg, partial_of, relative_path,
};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _, AsyncWriteExt as _};
use tokio::sync::{Notify, mpsc, oneshot};

mod offscreen;

/// Bytes read from disk per write to a bulk stream.
const CHUNK: usize = 256 * 1024;

/// Attempts at one file before the upload gives up on it: the first and two resumes.
const ATTEMPTS: u32 = 3;

/// How long the worker has to answer a [`XferMsg::Resume`].
const OFFSET_WAIT: Duration = Duration::from_secs(10);

/// Why a transfer stopped on this side.
#[derive(Debug, thiserror::Error)]
pub enum XferError {
    /// A file or directory on this machine could not be read or written. Sending or fetching
    /// again would fail the same way, so it never is.
    #[error("{path}: {source}")]
    Local {
        /// What was being read or written.
        path: String,
        /// The OS's error.
        #[source]
        source: std::io::Error,
    },
    /// A stream was cut: the link dropped, or the worker stopped it. What arrived is kept, and
    /// the rest is sent again from there.
    #[error("the stream was cut: {0}")]
    Cut(String),
    /// What arrived is not what was sent: more than announced, or a digest that does not
    /// match. Fetched again.
    #[error("{0}")]
    Mismatch(String),
    /// The worker said the transfer failed, in these words. Fetched again.
    #[error("{0}")]
    Worker(String),
    /// The worker did not answer in time.
    #[error("the worker did not answer: {0}")]
    Unanswered(&'static str),
    /// Somebody cancelled it.
    #[error("cancelled")]
    Cancelled,
    /// The link to the worker is gone.
    #[error("the worker went away")]
    LinkClosed,
}

impl XferError {
    fn local(path: impl std::fmt::Display, source: std::io::Error) -> Self {
        Self::Local { path: path.to_string(), source }
    }

    /// Whether fetching again can do better: a stream cut short, a copy that went wrong, a
    /// worker that failed or went quiet. Never a file here that cannot be read or written, a
    /// cancel or a link that is gone.
    #[must_use]
    pub const fn worth_retrying(&self) -> bool {
        matches!(self, Self::Cut(_) | Self::Mismatch(_) | Self::Worker(_) | Self::Unanswered(_))
    }
}

/// One file of a transfer, as the sender found it on disk.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    /// Where it is here.
    pub path: PathBuf,
    /// Its name in the transfer: relative to the dropped item's parent, `/`-separated.
    pub name: String,
    /// Size in bytes.
    pub size: u64,
    /// Last modification.
    pub mtime_ms: WallMs,
    /// Unix permission bits.
    pub mode: u32,
}

/// The files a drop of `paths` sends: each file as itself, each directory walked, up to
/// [`MAX_FILES`] in all.
///
/// Every name is relative to the dropped item's parent, so a directory arrives as a directory.
/// Symbolic links are followed for a dropped item and skipped inside a directory, so a link
/// loop cannot make a drop endless. A name that is not UTF-8 cannot travel and is skipped.
pub fn entries(paths: &[PathBuf]) -> std::io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| std::io::Error::other(format!("no name: {}", path.display())))?;
        walk(path, name.to_owned(), true, &mut out)?;
    }
    if out.len() >= MAX_FILES {
        tracing::warn!("the drop stops at {MAX_FILES} files");
    }
    Ok(out)
}

fn walk(path: &Path, name: String, top: bool, out: &mut Vec<Entry>) -> std::io::Result<()> {
    if out.len() >= MAX_FILES {
        return Ok(());
    }
    let meta = if top { std::fs::metadata(path)? } else { std::fs::symlink_metadata(path)? };
    if meta.is_dir() {
        let mut children: Vec<_> = std::fs::read_dir(path)?.collect::<Result<_, _>>()?;
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            let child_path = child.path();
            let Some(child_name) = child.file_name().to_str().map(str::to_owned) else {
                tracing::warn!(path = %child_path.display(), "skipped: name is not UTF-8");
                continue;
            };
            walk(&child_path, format!("{name}/{child_name}"), false, out)?;
        }
    } else if meta.is_file() {
        use std::os::unix::fs::PermissionsExt as _;
        let mtime_ms = meta.modified().map_or(WallMs::ZERO, WallMs::of);
        out.push(Entry {
            path: path.to_owned(),
            name,
            size: meta.len(),
            mtime_ms,
            mode: meta.permissions().mode() & MODE_BITS,
        });
    }
    Ok(())
}

/// What a drop on a terminal types: every path quoted, a space after each, as Terminal.app
/// does, so the next word can follow.
#[must_use]
pub fn paste_paths(paths: &[String]) -> String {
    paths.iter().fold(String::new(), |mut out, p| {
        out.push_str(&shell_quote(p));
        out.push(' ');
        out
    })
}

/// Transfers in flight on one link, shared by the control reader, the bulk acceptor and the
/// tasks that send and receive.
#[derive(Debug, Default)]
pub struct Table {
    inner: Mutex<Tables>,
    /// Woken when uploads are released or one is cancelled, for the uploads held before a chunk.
    unheld: Notify,
}

#[derive(Debug, Default)]
struct Tables {
    /// Uploads waiting for the worker's [`XferMsg::Offset`], by transfer and file.
    offsets: HashMap<(XferId, String), oneshot::Sender<u64>>,
    /// Uploads that were cancelled; their tasks stop at the next chunk.
    cancelled: std::collections::HashSet<XferId>,
    /// Uploads wait before their next chunk ([`Table::hold_uploads`]).
    held: bool,
    /// Download attempts being received, by the transfer each fetch named.
    downloads: HashMap<XferId, Attempt>,
    /// Uploads being sent, shown off screen as the worker reports them received, of how many
    /// bytes.
    uploads: HashMap<XferId, (Arc<slopty_platform::continued::Work>, u64)>,
}

/// One fetch of a download: a first one, or a retry after a cut.
#[derive(Debug)]
struct Attempt {
    into: PathBuf,
    /// The download across its attempts.
    fetch: Arc<Fetch>,
    /// Files the worker said it sends ([`XferMsg::Begin`]), once it has said.
    expected: Option<u32>,
    /// Files of this attempt in place.
    arrived: u32,
    /// The digest of each file, from the worker's [`XferMsg::Done`].
    digests: HashMap<String, Hash>,
    done: Option<oneshot::Sender<Result<(), XferError>>>,
}

/// A download across its attempts: what each file came to, and who is still writing.
#[derive(Debug, Default)]
struct Fetch {
    state: Mutex<FetchState>,
    /// Woken when a digest arrives or a writer ends.
    changed: Notify,
    /// The download's work off screen, told what has landed.
    work: Option<Arc<slopty_platform::continued::Work>>,
}

#[derive(Debug, Default)]
struct FetchState {
    /// Every file a header named, and where it landed once whole and checked.
    files: HashMap<String, Option<PathBuf>>,
    /// The landed files, in the order they landed.
    landed: Vec<PathBuf>,
    /// Streams being written now.
    writing: usize,
    /// Bytes the worker said the download is; the most any attempt said.
    total: u64,
    /// Bytes held of each file named so far, and their sum.
    held: HashMap<String, u64>,
    received: u64,
}

impl Fetch {
    /// `name` now holds `bytes`; the work off screen hears the whole download's count.
    fn received(&self, name: &str, bytes: u64) {
        let Some(work) = &self.work else { return };
        let mut state = self.state.lock();
        let before = state.held.insert(name.to_owned(), bytes).unwrap_or_default();
        state.received = state.received.saturating_sub(before).saturating_add(bytes);
        let (received, total) = (state.received, state.total);
        drop(state);
        work.progress(received, total);
    }
}

/// A stream being written into a download; the count drops when it ends.
struct Writing(Arc<Fetch>);

impl Drop for Writing {
    fn drop(&mut self) {
        let mut state = self.0.state.lock();
        state.writing = state.writing.saturating_sub(1);
        drop(state);
        self.0.changed.notify_waiters();
    }
}

impl Table {
    /// The worker answered a resume.
    fn offset(&self, xfer: XferId, name: String, durable: u64) {
        let waiting = self.inner.lock().offsets.remove(&(xfer, name));
        if let Some(tx) = waiting {
            let _gone = tx.send(durable);
        }
    }

    fn wait_offset(&self, xfer: XferId, name: String) -> oneshot::Receiver<u64> {
        let (tx, rx) = oneshot::channel();
        self.inner.lock().offsets.insert((xfer, name), tx);
        rx
    }

    /// Stop an upload's tasks at their next chunk.
    pub fn cancel(&self, xfer: XferId) {
        let mut inner = self.inner.lock();
        inner.cancelled.insert(xfer);
        let attempt = inner.downloads.remove(&xfer);
        drop(inner);
        self.unheld.notify_waiters();
        if let Some(Attempt { done: Some(done), .. }) = attempt {
            let _gone = done.send(Err(XferError::Cancelled));
        }
    }

    /// Hold every upload before its next chunk while `held`: a test's way to draw an upload at
    /// a known point, since how far one got by a given frame is up to the machine. A cancel
    /// still stops a held upload.
    pub fn hold_uploads(&self, held: bool) {
        self.inner.lock().held = held;
        self.unheld.notify_waiters();
    }

    /// Wait until `xfer` may send its next chunk: at once unless uploads are held, and an error
    /// once it is cancelled.
    async fn may_send(&self, xfer: XferId) -> Result<(), XferError> {
        loop {
            // Made before the check, so a release or a cancel between the two still wakes it.
            let woken = self.unheld.notified();
            {
                let inner = self.inner.lock();
                if inner.cancelled.contains(&xfer) {
                    return Err(XferError::Cancelled);
                }
                if !inner.held {
                    return Ok(());
                }
            }
            woken.await;
        }
    }

    fn cancelled(&self, xfer: XferId) -> bool {
        self.inner.lock().cancelled.contains(&xfer)
    }

    fn expect_download(
        &self,
        xfer: XferId,
        into: PathBuf,
        fetch: Arc<Fetch>,
    ) -> oneshot::Receiver<Result<(), XferError>> {
        let (tx, rx) = oneshot::channel();
        let attempt = Attempt {
            into,
            fetch,
            expected: None,
            arrived: 0,
            digests: HashMap::new(),
            done: Some(tx),
        };
        self.inner.lock().downloads.insert(xfer, attempt);
        rx
    }

    /// A stream of `xfer` starts being written: where to, and its download. Counted under the
    /// table's lock, so an attempt given up after this waits for the stream to end.
    fn start_writing(&self, xfer: XferId) -> Option<(PathBuf, Writing)> {
        let inner = self.inner.lock();
        let attempt = inner.downloads.get(&xfer)?;
        let mut state = attempt.fetch.state.lock();
        state.writing = state.writing.saturating_add(1);
        drop(state);
        let started = (attempt.into.clone(), Writing(Arc::clone(&attempt.fetch)));
        drop(inner);
        Some(started)
    }

    /// Whether `xfer` is still an attempt being received.
    fn current(&self, xfer: XferId) -> bool {
        self.inner.lock().downloads.contains_key(&xfer)
    }

    fn digest(&self, xfer: XferId, name: &str) -> Option<Hash> {
        self.inner.lock().downloads.get(&xfer)?.digests.get(name).copied()
    }

    /// A control message about a transfer this link knows. Returns `true` when it was only
    /// the tasks' business (an [`XferMsg::Offset`], a download's own), so the UI need not
    /// hear it.
    pub fn on_control(&self, msg: &XferMsg) -> bool {
        match msg {
            XferMsg::Offset { xfer, name, durable } => {
                self.offset(*xfer, name.clone(), *durable);
                true
            }
            XferMsg::Begin { xfer, dest: None, files, bytes } => {
                let mut inner = self.inner.lock();
                if let Some(d) = inner.downloads.get_mut(xfer) {
                    d.expected = Some(*files);
                    let mut state = d.fetch.state.lock();
                    state.total = state.total.max(*bytes);
                }
                let whole = take_if_whole(&mut inner, *xfer);
                drop(inner);
                finish(whole);
                true
            }
            XferMsg::Done { xfer, name, hash, .. } => {
                let mut inner = self.inner.lock();
                let Some(d) = inner.downloads.get_mut(xfer) else { return false };
                d.digests.insert(name.clone(), *hash);
                let fetch = Arc::clone(&d.fetch);
                drop(inner);
                fetch.changed.notify_waiters();
                true
            }
            XferMsg::Failed { xfer, error, .. } => {
                self.fail_download(*xfer, XferError::Worker(error.clone()))
            }
            XferMsg::Progress { xfer, done } => {
                let shown = self.inner.lock().uploads.get(xfer).map(|(w, t)| (Arc::clone(w), *t));
                if let Some((work, total)) = shown {
                    work.progress(*done, total);
                }
                false
            }
            _ => false,
        }
    }

    /// Attempt `xfer` failed with `error`: its caller hears it, and a stream waiting for one of
    /// its digests stops waiting. `false` when it was no attempt here.
    fn fail_download(&self, xfer: XferId, error: XferError) -> bool {
        let Some(failed) = self.inner.lock().downloads.remove(&xfer) else { return false };
        failed.fetch.changed.notify_waiters();
        if let Some(done) = failed.done {
            let _gone = done.send(Err(error));
        }
        true
    }

    /// One more file of attempt `xfer` is in place.
    fn arrived(&self, xfer: XferId) {
        let mut inner = self.inner.lock();
        if let Some(d) = inner.downloads.get_mut(&xfer) {
            d.arrived = d.arrived.saturating_add(1);
        }
        let whole = take_if_whole(&mut inner, xfer);
        drop(inner);
        finish(whole);
    }

    /// Give attempt `xfer` up: its streams stop at their next chunk.
    fn abandon(&self, xfer: XferId) {
        let gone = self.inner.lock().downloads.remove(&xfer);
        if let Some(gone) = gone {
            gone.fetch.changed.notify_waiters();
        }
    }
}

/// The attempt, taken out of the table, when every file it promised has landed.
fn take_if_whole(inner: &mut Tables, xfer: XferId) -> Option<Attempt> {
    let whole =
        inner.downloads.get(&xfer).is_some_and(|d| d.expected.is_some_and(|n| d.arrived >= n));
    if whole { inner.downloads.remove(&xfer) } else { None }
}

fn finish(whole: Option<Attempt>) {
    if let Some(Attempt { done: Some(done), .. }) = whole {
        let _gone = done.send(Ok(()));
    }
}

/// What an upload task needs from its link.
#[derive(Clone, Debug)]
pub struct Uplink {
    /// The connection the bulk streams open on.
    pub conn: Connection,
    /// The control stream.
    pub out: mpsc::Sender<ClientMsg>,
    /// The link's transfers.
    pub table: Arc<Table>,
}

/// Send `files` up as transfer `xfer`, to `dest`.
///
/// Every file is sent before this returns; the worker's `Done`, `Progress` and `Finished` arrive
/// on the control stream. An error is the reason the transfer stopped, already reported to the
/// worker as [`XferMsg::Failed`].
///
/// A file's bytes follow the last one's at once: up to 16 finished streams wait for
/// the worker's acknowledgement together, where waiting on each would cost a round trip per
/// file. A stream that fails is sent again from what the worker holds.
pub async fn upload(
    up: &Uplink,
    xfer: XferId,
    files: &[PathBuf],
    dest: Dest,
) -> Result<(), XferError> {
    // The walk of a dropped tree is blocking I/O: off the runtime that carries the keystrokes.
    let walked = files.to_vec();
    let found = tokio::task::spawn_blocking(move || entries(&walked))
        .await
        .map_err(|e| XferError::local("the dropped files", std::io::Error::other(e)))
        .and_then(|walked| walked.map_err(|e| XferError::local("the dropped files", e)));
    let list = match found {
        Ok(list) => list,
        Err(error) => {
            fail(up, xfer, None, &error).await;
            return Err(error);
        }
    };
    let bytes = list.iter().fold(0_u64, |sum, e| sum.saturating_add(e.size));
    let count = u32::try_from(list.len()).unwrap_or(u32::MAX);
    send(up, XferMsg::Begin { xfer, dest: Some(dest), files: count, bytes }).await?;
    let shown = offscreen::upload(&up.table, &up.out, xfer, &list);
    shown.work().progress(0, bytes);
    up.table.inner.lock().uploads.insert(xfer, (Arc::clone(shown.work()), bytes));
    let sent = send_all(up, xfer, &list).await;
    up.table.inner.lock().uploads.remove(&xfer);
    shown.work().end(sent.is_ok());
    if let Err((entry, error)) = &sent
        && !up.table.cancelled(xfer)
    {
        fail(up, xfer, entry.map(|e| e.name.clone()), error).await;
    }
    sent.map_err(|(_, error)| error)
}

/// Finished streams waiting for the worker's acknowledgement at once.
const IN_FLIGHT: usize = 16;

/// A finished stream's acknowledgement as its task hands it back: the file's place in the
/// list, and whether the worker took every byte.
type Acked = Option<Result<(usize, Result<(), XferError>), tokio::task::JoinError>>;

/// Every file of `list`, the next one's bytes following the last's; the file that could not be
/// sent and why.
async fn send_all<'a>(
    up: &Uplink,
    xfer: XferId,
    list: &'a [Entry],
) -> Result<(), (Option<&'a Entry>, XferError)> {
    let mut confirming = tokio::task::JoinSet::new();
    for (ix, entry) in list.iter().enumerate() {
        match send_file(up, xfer, entry, 0).await {
            Ok(acked) => {
                confirming.spawn(async move { (ix, acked.await) });
            }
            Err(error) => resume(up, xfer, entry, error).await.map_err(|e| (Some(entry), e))?,
        }
        while confirming.len() >= IN_FLIGHT {
            acknowledged(up, xfer, list, confirming.join_next().await).await?;
        }
    }
    while let Some(done) = confirming.join_next().await {
        acknowledged(up, xfer, list, Some(done)).await?;
    }
    Ok(())
}

/// One acknowledgement: a file the worker stopped or lost is sent again from what it holds.
async fn acknowledged<'a>(
    up: &Uplink,
    xfer: XferId,
    list: &'a [Entry],
    done: Acked,
) -> Result<(), (Option<&'a Entry>, XferError)> {
    match done {
        None | Some(Ok((_, Ok(())))) => Ok(()),
        Some(Ok((ix, Err(error)))) => {
            let entry = list
                .get(ix)
                .ok_or_else(|| (None, XferError::Mismatch("a file out of the list".to_owned())))?;
            resume(up, xfer, entry, error).await.map_err(|e| (Some(entry), e))
        }
        Some(Err(e)) => Err((None, XferError::Cut(e.to_string()))),
    }
}

async fn send(up: &Uplink, msg: XferMsg) -> Result<(), XferError> {
    up.out.send(ClientMsg::Xfer(msg)).await.map_err(|_closed| XferError::LinkClosed)
}

async fn fail(up: &Uplink, xfer: XferId, name: Option<String>, error: &XferError) {
    tracing::warn!(%xfer, ?name, %error, "upload failed");
    let _closed = send(up, XferMsg::Failed { xfer, name, error: error.to_string() }).await;
}

/// `entry` again after its stream failed with `error`, from what the worker holds, until it
/// lands or [`ATTEMPTS`] have failed. Only a cut stream is sent again: a file here that cannot
/// be read would fail the same way each time.
async fn resume(
    up: &Uplink,
    xfer: XferId,
    entry: &Entry,
    error: XferError,
) -> Result<(), XferError> {
    let mut error = error;
    for _attempt in 1..ATTEMPTS {
        if up.table.cancelled(xfer) {
            return Err(XferError::Cancelled);
        }
        if !matches!(error, XferError::Cut(_)) {
            return Err(error);
        }
        tracing::debug!(%xfer, name = %entry.name, %error, "stream cut; resuming");
        let answer = up.table.wait_offset(xfer, entry.name.clone());
        send(up, XferMsg::Resume { xfer, name: entry.name.clone() }).await?;
        let offset = tokio::time::timeout(OFFSET_WAIT, answer)
            .await
            .map_err(|_elapsed| XferError::Unanswered("how much of the file it holds"))?
            .map_err(|_dropped| XferError::LinkClosed)?
            .min(entry.size);
        let sent = match send_file(up, xfer, entry, offset).await {
            Ok(acked) => acked.await,
            Err(e) => Err(e),
        };
        match sent {
            Ok(()) => return Ok(()),
            Err(e) => error = e,
        }
    }
    if up.table.cancelled(xfer) { Err(XferError::Cancelled) } else { Err(error) }
}

/// Send `entry` from `offset` on a bulk stream of its own and finish it; what is returned
/// resolves once the worker has acknowledged every byte, or says why it did not.
async fn send_file(
    up: &Uplink,
    xfer: XferId,
    entry: &Entry,
    offset: u64,
) -> Result<impl Future<Output = Result<(), XferError>> + Send + 'static, XferError> {
    let header = BulkHeader {
        xfer,
        purpose: Purpose::Upload,
        name: entry.name.clone(),
        size: entry.size,
        mtime_ms: entry.mtime_ms,
        mode: entry.mode,
        offset,
    };
    let local = |e| XferError::local(entry.path.display(), e);
    // The file first: one that cannot be read opens no stream for the worker to wait on.
    let mut file = tokio::fs::File::open(&entry.path).await.map_err(local)?;
    file.seek(std::io::SeekFrom::Start(offset)).await.map_err(local)?;
    let cut = |e: NetError| XferError::Cut(e.to_string());
    let mut stream = slopty_net::streams::open_bulk(&up.conn, header).await.map_err(cut)?;
    let mut buf = vec![0_u8; CHUNK];
    loop {
        if let Err(stopped) = up.table.may_send(xfer).await {
            let _reset = stream.reset(0_u32.into());
            return Err(stopped);
        }
        let n = file.read(&mut buf).await.map_err(local)?;
        let Some(chunk) = buf.get(..n).filter(|c| !c.is_empty()) else { break };
        stream.write_all(chunk).await.map_err(|e| XferError::Cut(e.to_string()))?;
    }
    stream.finish().map_err(|e| XferError::Cut(e.to_string()))?;
    // A stream the worker stopped after the last write surfaces here rather than in a write.
    Ok(async move {
        match stream.stopped().await {
            Ok(None) => Ok(()),
            Ok(Some(code)) => Err(XferError::Cut(format!("the worker stopped it ({code})"))),
            Err(e) => Err(XferError::Cut(e.to_string())),
        }
    })
}

/// Ask the worker for `path` (a file, or a directory sent as its files) as transfer `xfer`, and
/// wait until every file of it has landed in `into`. Returns the files landed.
///
/// An attempt cut short (a stream ended early, a digest that does not match, the worker's
/// `Failed`) is given up and fetched again under a new transfer, naming what is held of each
/// file, three attempts in all. One that failed on this side (a file here that cannot be
/// written) is not.
pub async fn download(
    up: &Uplink,
    xfer: XferId,
    path: String,
    into: PathBuf,
) -> Result<Vec<PathBuf>, XferError> {
    let current = Arc::new(Mutex::new(xfer));
    let shown = offscreen::download(&up.table, &up.out, xfer, &path, Arc::clone(&current));
    let fetch = Arc::new(Fetch { work: Some(Arc::clone(shown.work())), ..Fetch::default() });
    let landed = attempts(up, xfer, &path, &into, &fetch, &current).await;
    shown.work().end(landed.is_ok());
    landed
}

/// The attempts of [`download`], each under the transfer `current` names.
async fn attempts(
    up: &Uplink,
    xfer: XferId,
    path: &str,
    into: &Path,
    fetch: &Arc<Fetch>,
    shared: &Mutex<XferId>,
) -> Result<Vec<PathBuf>, XferError> {
    let mut current = xfer;
    let mut attempt = 0_u32;
    loop {
        attempt = attempt.saturating_add(1);
        let held = held(fetch, into).await;
        let done = up.table.expect_download(current, into.to_path_buf(), Arc::clone(fetch));
        send(up, XferMsg::Fetch { xfer: current, path: path.to_owned(), held }).await?;
        let error = match done.await.map_err(|_dropped| XferError::LinkClosed)? {
            Ok(()) => return Ok(fetch.state.lock().landed.clone()),
            Err(error) => error,
        };
        let cancelled = up.table.cancelled(xfer) || up.table.cancelled(current);
        if cancelled || !error.worth_retrying() || attempt >= ATTEMPTS {
            return Err(error);
        }
        tracing::info!(%current, %path, %error, "download cut; fetching the rest");
        up.table.abandon(current);
        send(up, XferMsg::Cancel { xfer: current }).await?;
        settle(fetch).await?;
        current = XferId::new();
        *shared.lock() = current;
    }
}

/// How long a given-up attempt's streams have to stop before the next one starts.
const SETTLE_WAIT: Duration = Duration::from_secs(10);

/// Wait until no stream of the download is being written.
async fn settle(fetch: &Fetch) -> Result<(), XferError> {
    let settled = async {
        loop {
            let changed = fetch.changed.notified();
            let mut changed = std::pin::pin!(changed);
            changed.as_mut().enable();
            if fetch.state.lock().writing == 0 {
                return;
            }
            changed.await;
        }
    };
    tokio::time::timeout(SETTLE_WAIT, settled)
        .await
        .map_err(|_elapsed| XferError::Unanswered("an earlier stream did not stop"))
}

/// What a retry says it holds: every landed file whole, and the durable bytes of each partial.
async fn held(fetch: &Fetch, into: &Path) -> Vec<(String, u64)> {
    let files: Vec<(String, Option<PathBuf>)> =
        fetch.state.lock().files.iter().map(|(n, l)| (n.clone(), l.clone())).collect();
    let mut held = Vec::with_capacity(files.len());
    for (name, landed) in files {
        let bytes = match landed {
            Some(path) => tokio::fs::metadata(&path).await.map_or(0, |m| m.len()),
            None => match relative_path(&name) {
                Some(rel) => durable(&partial_of(&into.join(rel))).await,
                None => 0,
            },
        };
        if bytes > 0 {
            held.push((name, bytes));
        }
    }
    held
}

/// Bytes held durably in a partial file, synced now; 0 when there is none.
async fn durable(partial: &Path) -> u64 {
    let Ok(file) = tokio::fs::OpenOptions::new().write(true).open(partial).await else { return 0 };
    if file.sync_all().await.is_err() {
        return 0;
    }
    file.metadata().await.map_or(0, |m| m.len())
}

/// A whole, checked file's bytes handed to the drive before it is renamed into place.
///
/// A plain `fsync`, not `F_FULLFSYNC` (which Rust's `sync_all` is on Apple platforms): the
/// full flush costs 3.8 ms a file on this Mac's SSD against 0.2 ms, and a directory sync as
/// much again, so ten thousand small files spent over a minute syncing (MEASUREMENTS.md,
/// "syncing a landed file"). What a resume claims to hold is still fully synced
/// ([`durable`]); a landed file has passed its digest, and cp, Finder and rsync sync nothing.
async fn landed_data(file: &tokio::fs::File) -> std::io::Result<()> {
    let file = file.try_clone().await?.into_std().await;
    tokio::task::spawn_blocking(move || rustix::fs::fsync(&file).map_err(std::io::Error::from))
        .await
        .map_err(std::io::Error::other)?
}

/// How long a whole file waits for the worker's digest, which rides the control stream.
const DIGEST_WAIT: Duration = Duration::from_secs(10);

/// A bulk stream of a download, landed in the transfer's directory.
///
/// It is written to `name.partial`, synced, checked against the worker's digest and renamed
/// into place. A stream of an attempt no longer wanted is stopped.
pub async fn receive(table: &Table, header: BulkHeader, mut rx: RawRecv) {
    let xfer = header.xfer;
    let Some((dir, writing)) = table.start_writing(xfer) else {
        tracing::debug!(%xfer, "bulk for no download here; stopping it");
        rx.stop();
        return;
    };
    match write_file(table, &writing.0, &dir, &header, &mut rx).await {
        Ok(()) => table.arrived(xfer),
        Err(error) => {
            rx.stop();
            tracing::debug!(%xfer, name = %header.name, %error, "download stream failed");
            table.fail_download(xfer, error);
        }
    }
    drop(writing);
}

async fn write_file(
    table: &Table,
    fetch: &Fetch,
    dir: &Path,
    header: &BulkHeader,
    rx: &mut RawRecv,
) -> Result<(), XferError> {
    let name = &header.name;
    let rel =
        relative_path(name).ok_or_else(|| XferError::Mismatch(format!("bad name {name:?}")))?;
    let target = dir.join(rel);
    let io = |e| XferError::local(target.display(), e);
    let had = fetch.state.lock().files.get(name).cloned().flatten();
    if header.offset == header.size && had.as_ref() == Some(&target) {
        // Landed on an earlier attempt, and the worker still has that version.
        fetch.received(name, header.size);
        return match rx.chunk(1).await.map_err(|e| XferError::Cut(e.to_string()))? {
            None => Ok(()),
            Some(_) => Err(XferError::Mismatch(format!("{name}: more than announced"))),
        };
    }
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(io)?;
    }
    let partial = partial_of(&target);
    let (mut file, mut hasher) = open_partial(&partial, header.offset).await?;
    fetch.state.lock().files.insert(header.name.clone(), None);
    let expected = header.size.saturating_sub(header.offset);
    let mut got = 0_u64;
    let cut_at =
        |got, why| XferError::Cut(format!("{name}: cut at {got} of {expected} bytes: {why}"));
    loop {
        if !table.current(header.xfer) {
            return Err(cut_at(got, "given up".to_owned()));
        }
        let chunk = match rx.chunk(CHUNK).await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(e) => return Err(cut_at(got, e.to_string())),
        };
        let n = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
        if got.saturating_add(n) > expected {
            return Err(XferError::Mismatch(format!("{name}: more than announced")));
        }
        file.write_all(&chunk).await.map_err(|e| XferError::local(partial.display(), e))?;
        hasher.update(&chunk);
        got = got.saturating_add(n);
        fetch.received(name, header.offset.saturating_add(got));
    }
    if got != expected {
        return Err(cut_at(got, "the stream ended".to_owned()));
    }
    let want = wait_digest(table, fetch, header.xfer, name).await?;
    if want != *hasher.finalize().as_bytes() {
        drop(file);
        let _gone = tokio::fs::remove_file(&partial).await;
        return Err(XferError::Mismatch(format!("{name}: the digest does not match the worker's")));
    }
    landed_data(&file).await.map_err(io)?;
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::Permissions::from_mode(header.mode & MODE_BITS);
        file.set_permissions(mode).await.map_err(io)?;
    }
    if let Some(mtime) = header.mtime_ms.to_system() {
        file.into_std().await.set_modified(mtime).map_err(io)?;
    }
    tokio::fs::rename(&partial, &target).await.map_err(io)?;
    let mut state = fetch.state.lock();
    state.files.insert(header.name.clone(), Some(target.clone()));
    if !state.landed.contains(&target) {
        state.landed.push(target);
    }
    drop(state);
    Ok(())
}

/// The partial file, ready to take bytes at `offset`: made afresh at 0, else cut back to
/// `offset` (which it must hold) with a hasher over what it keeps. A partial file that holds
/// less than that is a [`XferError::Mismatch`], fetched again from what it does hold.
async fn open_partial(
    partial: &Path,
    offset: u64,
) -> Result<(tokio::fs::File, blake3::Hasher), XferError> {
    let io = |e| XferError::local(partial.display(), e);
    let short = |held| {
        XferError::Mismatch(format!("{}: resume at {offset}, {held} bytes held", partial.display()))
    };
    let mut hasher = blake3::Hasher::new();
    if offset == 0 {
        return Ok((tokio::fs::File::create(partial).await.map_err(io)?, hasher));
    }
    let mut file =
        tokio::fs::OpenOptions::new().read(true).write(true).open(partial).await.map_err(io)?;
    let held = file.metadata().await.map_err(io)?.len();
    if held < offset {
        return Err(short(held));
    }
    file.set_len(offset).await.map_err(io)?;
    let mut buf = vec![0_u8; CHUNK];
    let mut left = offset;
    while left > 0 {
        let want = usize::try_from(left).unwrap_or(CHUNK).min(CHUNK);
        let n = file.read(buf.get_mut(..want).unwrap_or_default()).await.map_err(io)?;
        if n == 0 {
            return Err(short(offset.saturating_sub(left)));
        }
        hasher.update(buf.get(..n).unwrap_or_default());
        left = left.saturating_sub(n as u64);
    }
    file.seek(std::io::SeekFrom::Start(offset)).await.map_err(io)?;
    Ok((file, hasher))
}

/// The worker's digest of `name`, waiting for its `Done` if it has not come yet.
async fn wait_digest(
    table: &Table,
    fetch: &Fetch,
    xfer: XferId,
    name: &str,
) -> Result<Hash, XferError> {
    let waited = async {
        loop {
            let changed = fetch.changed.notified();
            let mut changed = std::pin::pin!(changed);
            changed.as_mut().enable();
            if let Some(hash) = table.digest(xfer, name) {
                return Ok(hash);
            }
            if !table.current(xfer) {
                return Err(XferError::Cut(format!("{name}: given up")));
            }
            changed.await;
        }
    };
    tokio::time::timeout(DIGEST_WAIT, waited)
        .await
        .map_err(|_elapsed| XferError::Unanswered("the file's digest"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_paths_are_typed_quoted_with_a_space_after_each() {
        assert_eq!(
            paste_paths(&["/a b".to_owned(), "/c".to_owned()]),
            "'/a b' /c ",
            "a space after each, as a drop on Terminal.app types"
        );
    }

    #[test]
    fn a_dropped_directory_is_its_files_named_under_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), b"fn main() {}").unwrap();
        std::fs::write(root.join("README"), b"hi").unwrap();
        let single = dir.path().join("one.txt");
        std::fs::write(&single, b"1").unwrap();
        std::os::unix::fs::symlink(&root, root.join("loop")).unwrap();
        let found = entries(&[root, single]).unwrap();
        let names: Vec<_> = found.iter().map(|e| (e.name.as_str(), e.size)).collect();
        assert_eq!(names, [("proj/README", 2), ("proj/src/main.rs", 12), ("one.txt", 1)]);
    }

    #[test]
    fn a_download_is_whole_when_the_files_the_worker_promised_have_landed_in_any_order() {
        let table = Table::default();
        let xfer = XferId::new();
        let mut done = table.expect_download(xfer, PathBuf::from("/tmp"), Arc::default());
        table.arrived(xfer);
        assert!(done.try_recv().is_err(), "the count is not known yet");
        assert!(table.on_control(&XferMsg::Begin { xfer, dest: None, files: 2, bytes: 3 }));
        assert!(done.try_recv().is_err(), "one of two");
        let digest =
            XferMsg::Done { xfer, name: "b".to_owned(), path: "/b".to_owned(), hash: [7; 32] };
        assert!(table.on_control(&digest), "a download's digest is not the UI's business");
        assert_eq!(table.digest(xfer, "b"), Some([7; 32]));
        table.arrived(xfer);
        done.try_recv().unwrap().unwrap();
    }

    /// A download's progress is what each file holds, counted once however often a file is
    /// resumed, against the most the worker said it is.
    #[test]
    fn a_download_counts_what_each_file_holds_once() {
        let work =
            slopty_platform::continued::Work::begin("Downloading x", "From the worker", || {});
        let fetch = Arc::new(Fetch { work: Some(Arc::new(work)), ..Fetch::default() });
        let table = Table::default();
        let xfer = XferId::new();
        let _done = table.expect_download(xfer, PathBuf::from("/tmp"), Arc::clone(&fetch));
        assert!(table.on_control(&XferMsg::Begin { xfer, dest: None, files: 2, bytes: 40 }));
        fetch.received("a", 10);
        fetch.received("a", 30);
        fetch.received("b", 5);
        fetch.received("a", 12);
        let state = fetch.state.lock();
        assert_eq!((state.received, state.total), (17, 40), "a resumed from 12, and b");
    }
}
