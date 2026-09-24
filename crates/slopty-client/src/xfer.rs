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

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use parking_lot::Mutex;
use slopty_core::XferId;
use slopty_net::streams::RawRecv;
use slopty_net::{ClientMsg, Connection, NetError};
use slopty_proto::transfer::{BulkHeader, Dest, Hash, Purpose, XferMsg};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _, AsyncWriteExt as _};
use tokio::sync::{Notify, mpsc, oneshot};

/// Bytes read from disk per write to a bulk stream.
const CHUNK: usize = 256 * 1024;

/// Attempts at one file before the upload gives up on it: the first and two resumes.
const ATTEMPTS: u32 = 3;

/// How long the worker has to answer a [`XferMsg::Resume`].
const OFFSET_WAIT: Duration = Duration::from_secs(10);

/// One file of a transfer, as the sender found it on disk.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    /// Where it is here.
    pub path: PathBuf,
    /// Its name in the transfer: relative to the dropped item's parent, `/`-separated.
    pub name: String,
    /// Size in bytes.
    pub size: u64,
    /// Last modification, milliseconds since the Unix epoch.
    pub mtime_ms: u64,
    /// Unix permission bits.
    pub mode: u32,
}

/// The files a drop of `paths` sends: each file as itself, each directory walked.
///
/// Every name is relative to the dropped item's parent, so a directory arrives as a directory.
/// Symbolic links are followed for a dropped item and skipped inside a directory, so a link
/// loop cannot make a drop endless.
pub fn entries(paths: &[PathBuf]) -> std::io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| std::io::Error::other(format!("no name: {}", path.display())))?;
        walk(path, name.to_owned(), true, &mut out)?;
    }
    Ok(out)
}

fn walk(path: &Path, name: String, top: bool, out: &mut Vec<Entry>) -> std::io::Result<()> {
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
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        out.push(Entry {
            path: path.to_owned(),
            name,
            size: meta.len(),
            mtime_ms,
            mode: meta.permissions().mode() & 0o7777,
        });
    }
    Ok(())
}

/// `path` as a shell reads it back as one word: as is when every character is plain, else in
/// single quotes (a quote inside closes, escapes and reopens them).
#[must_use]
pub fn shell_quote(path: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "/._-+,:@%=~".contains(c);
    if !path.is_empty() && path.chars().all(plain) {
        return path.to_owned();
    }
    let mut out = String::with_capacity(path.len().saturating_add(2));
    out.push('\'');
    for c in path.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
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

/// A transfer name as a relative path: `None` for one that could escape its root (absolute,
/// `..`, empty).
#[must_use]
pub fn safe_relative(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    let normal = path.components().all(|c| matches!(c, Component::Normal(_)));
    (normal && !name.is_empty()).then(|| path.to_owned())
}

/// Transfers in flight on one link, shared by the control reader, the bulk acceptor and the
/// tasks that send and receive.
#[derive(Debug, Default)]
pub struct Table {
    inner: Mutex<Tables>,
}

#[derive(Debug, Default)]
struct Tables {
    /// Uploads waiting for the worker's [`XferMsg::Offset`], by transfer and file.
    offsets: HashMap<(XferId, String), oneshot::Sender<u64>>,
    /// Uploads that were cancelled; their tasks stop at the next chunk.
    cancelled: std::collections::HashSet<XferId>,
    /// Download attempts being received, by the transfer each fetch named.
    downloads: HashMap<XferId, Attempt>,
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
    done: Option<oneshot::Sender<Result<(), String>>>,
}

/// A download across its attempts: what each file came to, and who is still writing.
#[derive(Debug, Default)]
struct Fetch {
    state: Mutex<FetchState>,
    /// Woken when a digest arrives or a writer ends.
    changed: Notify,
}

#[derive(Debug, Default)]
struct FetchState {
    /// Every file a header named, and where it landed once whole and checked.
    files: HashMap<String, Option<PathBuf>>,
    /// The landed files, in the order they landed.
    landed: Vec<PathBuf>,
    /// Streams being written now.
    writing: usize,
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
        if let Some(d) = inner.downloads.remove(&xfer)
            && let Some(done) = d.done
        {
            let _gone = done.send(Err("cancelled".to_owned()));
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
    ) -> oneshot::Receiver<Result<(), String>> {
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
            XferMsg::Begin { xfer, dest: None, files, .. } => {
                let mut inner = self.inner.lock();
                if let Some(d) = inner.downloads.get_mut(xfer) {
                    d.expected = Some(*files);
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
                let Some(failed) = self.inner.lock().downloads.remove(xfer) else { return false };
                // A stream waiting for a digest of this attempt stops waiting.
                failed.fetch.changed.notify_waiters();
                if let Some(done) = failed.done {
                    let _gone = done.send(Err(error.clone()));
                }
                true
            }
            _ => false,
        }
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
pub async fn upload(
    up: &Uplink,
    xfer: XferId,
    files: &[PathBuf],
    dest: Dest,
) -> Result<(), String> {
    let found = entries(files).map_err(|e| e.to_string());
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
    for entry in &list {
        if let Err(error) = send_with_resume(up, xfer, entry).await {
            if !up.table.cancelled(xfer) {
                fail(up, xfer, Some(entry.name.clone()), &error).await;
            }
            return Err(error);
        }
    }
    Ok(())
}

async fn send(up: &Uplink, msg: XferMsg) -> Result<(), String> {
    up.out.send(ClientMsg::Xfer(msg)).await.map_err(|_closed| "link closed".to_owned())
}

async fn fail(up: &Uplink, xfer: XferId, name: Option<String>, error: &str) {
    tracing::warn!(%xfer, ?name, error, "upload failed");
    let _closed = send(up, XferMsg::Failed { xfer, name, error: error.to_owned() }).await;
}

/// One file, resumed from what the worker holds after a cut stream.
async fn send_with_resume(up: &Uplink, xfer: XferId, entry: &Entry) -> Result<(), String> {
    let mut offset = 0_u64;
    let mut attempt = 0_u32;
    loop {
        attempt = attempt.saturating_add(1);
        match send_file(up, xfer, entry, offset).await {
            Ok(()) => return Ok(()),
            Err(_) if up.table.cancelled(xfer) => return Err("cancelled".to_owned()),
            Err(e) if attempt >= ATTEMPTS => return Err(e),
            Err(e) => {
                tracing::debug!(%xfer, name = %entry.name, error = %e, "stream cut; resuming");
                let answer = up.table.wait_offset(xfer, entry.name.clone());
                send(up, XferMsg::Resume { xfer, name: entry.name.clone() }).await?;
                offset = tokio::time::timeout(OFFSET_WAIT, answer)
                    .await
                    .map_err(|_elapsed| "the worker did not say how much it holds".to_owned())?
                    .map_err(|_dropped| "link closed".to_owned())?
                    .min(entry.size);
            }
        }
    }
}

/// Send `entry` from `offset` on a bulk stream of its own.
async fn send_file(up: &Uplink, xfer: XferId, entry: &Entry, offset: u64) -> Result<(), String> {
    let header = BulkHeader {
        xfer,
        purpose: Purpose::Upload,
        name: entry.name.clone(),
        size: entry.size,
        mtime_ms: entry.mtime_ms,
        mode: entry.mode,
        offset,
    };
    let net = |e: NetError| e.to_string();
    let mut stream = slopty_net::streams::open_bulk(&up.conn, header).await.map_err(net)?;
    let mut file = tokio::fs::File::open(&entry.path).await.map_err(|e| e.to_string())?;
    file.seek(std::io::SeekFrom::Start(offset)).await.map_err(|e| e.to_string())?;
    let mut buf = vec![0_u8; CHUNK];
    loop {
        if up.table.cancelled(xfer) {
            let _reset = stream.reset(0_u32.into());
            return Err("cancelled".to_owned());
        }
        let n = file.read(&mut buf).await.map_err(|e| e.to_string())?;
        let Some(chunk) = buf.get(..n).filter(|c| !c.is_empty()) else { break };
        stream.write_all(chunk).await.map_err(|e| e.to_string())?;
    }
    stream.finish().map_err(|e| e.to_string())?;
    // A stream the worker stopped after the last write surfaces here rather than in a write.
    match stream.stopped().await {
        Ok(None) => Ok(()),
        Ok(Some(code)) => Err(format!("the worker stopped the stream ({code})")),
        Err(e) => Err(e.to_string()),
    }
}

/// Ask the worker for `path` (a file, or a directory sent as its files) as transfer `xfer`, and
/// wait until every file of it has landed in `into`. Returns the files landed.
///
/// An attempt cut short (a stream ended early, a digest that does not match, the worker's
/// `Failed`) is given up and fetched again under a new transfer, naming what is held of each
/// file, three attempts in all.
pub async fn download(
    up: &Uplink,
    xfer: XferId,
    path: String,
    into: PathBuf,
) -> Result<Vec<PathBuf>, String> {
    let fetch = Arc::new(Fetch::default());
    let mut current = xfer;
    let mut attempt = 0_u32;
    loop {
        attempt = attempt.saturating_add(1);
        let held = held(&fetch, &into).await;
        let done = up.table.expect_download(current, into.clone(), Arc::clone(&fetch));
        send(up, XferMsg::Fetch { xfer: current, path: path.clone(), held }).await?;
        let error = match done.await.map_err(|_dropped| "link closed".to_owned())? {
            Ok(()) => return Ok(fetch.state.lock().landed.clone()),
            Err(error) => error,
        };
        if up.table.cancelled(xfer) || up.table.cancelled(current) || attempt >= ATTEMPTS {
            return Err(error);
        }
        tracing::info!(%current, %path, %error, "download cut; fetching the rest");
        up.table.abandon(current);
        send(up, XferMsg::Cancel { xfer: current }).await?;
        settle(&fetch).await?;
        current = XferId::new();
    }
}

/// How long a given-up attempt's streams have to stop before the next one starts.
const SETTLE_WAIT: Duration = Duration::from_secs(10);

/// Wait until no stream of the download is being written.
async fn settle(fetch: &Fetch) -> Result<(), String> {
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
        .map_err(|_elapsed| "an earlier stream did not stop".to_owned())
}

/// What a retry says it holds: every landed file whole, and the durable bytes of each partial.
async fn held(fetch: &Fetch, into: &Path) -> Vec<(String, u64)> {
    let files: Vec<(String, Option<PathBuf>)> =
        fetch.state.lock().files.iter().map(|(n, l)| (n.clone(), l.clone())).collect();
    let mut held = Vec::with_capacity(files.len());
    for (name, landed) in files {
        let bytes = match landed {
            Some(path) => tokio::fs::metadata(&path).await.map_or(0, |m| m.len()),
            None => match safe_relative(&name) {
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

/// Where `target`'s bytes are written until it is whole and checked.
#[must_use]
pub fn partial_of(target: &Path) -> PathBuf {
    let mut name = target.as_os_str().to_owned();
    name.push(".partial");
    PathBuf::from(name)
}

/// A download's partial file is synced after this many bytes, so what a retry claims to hold
/// is on the disk.
const SYNC_EVERY: u64 = 8 << 20;

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
            let failed = XferMsg::Failed { xfer, name: Some(header.name), error };
            table.on_control(&failed);
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
) -> Result<(), String> {
    let io = |e: std::io::Error| format!("{}: {e}", header.name);
    let rel = safe_relative(&header.name).ok_or_else(|| format!("bad name {:?}", header.name))?;
    let target = dir.join(rel);
    let had = fetch.state.lock().files.get(&header.name).cloned().flatten();
    if header.offset == header.size && had.as_ref() == Some(&target) {
        // Landed on an earlier attempt, and the worker still has that version.
        return match rx.chunk(1).await.map_err(|e| e.to_string())? {
            None => Ok(()),
            Some(_) => Err(format!("{}: more than announced", header.name)),
        };
    }
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(io)?;
    }
    let partial = partial_of(&target);
    let (mut file, mut hasher) = open_partial(&partial, header.offset).await.map_err(io)?;
    fetch.state.lock().files.insert(header.name.clone(), None);
    let expected = header.size.saturating_sub(header.offset);
    let (mut got, mut unsynced) = (0_u64, 0_u64);
    let cut = loop {
        if !table.current(header.xfer) {
            break Some("given up".to_owned());
        }
        let chunk = match rx.chunk(CHUNK).await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break None,
            Err(e) => break Some(e.to_string()),
        };
        let n = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
        if got.saturating_add(n) > expected {
            break Some("more than announced".to_owned());
        }
        if let Err(e) = file.write_all(&chunk).await {
            break Some(e.to_string());
        }
        hasher.update(&chunk);
        got = got.saturating_add(n);
        unsynced = unsynced.saturating_add(n);
        if unsynced >= SYNC_EVERY {
            file.sync_data().await.map_err(io)?;
            unsynced = 0;
        }
    };
    file.sync_all().await.map_err(io)?;
    if let Some(why) = cut {
        return Err(format!("{}: cut at {got} of {expected} bytes: {why}", header.name));
    }
    if got != expected {
        return Err(format!("{}: {got} of {expected} bytes", header.name));
    }
    let want = wait_digest(table, fetch, header.xfer, &header.name).await?;
    if want != *hasher.finalize().as_bytes() {
        drop(file);
        let _gone = tokio::fs::remove_file(&partial).await;
        return Err(format!("{}: the digest does not match the worker's", header.name));
    }
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::Permissions::from_mode(header.mode & 0o777);
        file.set_permissions(mode).await.map_err(io)?;
    }
    let mtime =
        std::time::SystemTime::UNIX_EPOCH.checked_add(Duration::from_millis(header.mtime_ms));
    if let Some(mtime) = mtime.filter(|_| header.mtime_ms > 0) {
        file.into_std().await.set_modified(mtime).map_err(io)?;
    }
    tokio::fs::rename(&partial, &target).await.map_err(io)?;
    if let Some(parent) = target.parent()
        && let Ok(parent) = tokio::fs::File::open(parent).await
    {
        let _synced = parent.sync_all().await;
    }
    let mut state = fetch.state.lock();
    state.files.insert(header.name.clone(), Some(target.clone()));
    if !state.landed.contains(&target) {
        state.landed.push(target);
    }
    drop(state);
    Ok(())
}

/// The partial file, ready to take bytes at `offset`: made afresh at 0, else cut back to
/// `offset` (which it must hold) with a hasher over what it keeps.
async fn open_partial(
    partial: &Path,
    offset: u64,
) -> std::io::Result<(tokio::fs::File, blake3::Hasher)> {
    let mut hasher = blake3::Hasher::new();
    if offset == 0 {
        return Ok((tokio::fs::File::create(partial).await?, hasher));
    }
    let mut file = tokio::fs::OpenOptions::new().read(true).write(true).open(partial).await?;
    let held = file.metadata().await?.len();
    if held < offset {
        return Err(std::io::Error::other(format!("resume at {offset}, but {held} bytes held")));
    }
    file.set_len(offset).await?;
    let mut buf = vec![0_u8; CHUNK];
    let mut left = offset;
    while left > 0 {
        let want = usize::try_from(left).unwrap_or(CHUNK).min(CHUNK);
        let n = file.read(buf.get_mut(..want).unwrap_or_default()).await?;
        if n == 0 {
            return Err(std::io::Error::other("the partial file shrank"));
        }
        hasher.update(buf.get(..n).unwrap_or_default());
        left = left.saturating_sub(n as u64);
    }
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    Ok((file, hasher))
}

/// The worker's digest of `name`, waiting for its `Done` if it has not come yet.
async fn wait_digest(
    table: &Table,
    fetch: &Fetch,
    xfer: XferId,
    name: &str,
) -> Result<Hash, String> {
    let waited = async {
        loop {
            let changed = fetch.changed.notified();
            let mut changed = std::pin::pin!(changed);
            changed.as_mut().enable();
            if let Some(hash) = table.digest(xfer, name) {
                return Ok(hash);
            }
            if !table.current(xfer) {
                return Err(format!("{name}: given up"));
            }
            changed.await;
        }
    };
    tokio::time::timeout(DIGEST_WAIT, waited)
        .await
        .map_err(|_elapsed| format!("{name}: the worker sent no digest"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_path_is_typed_as_is_and_anything_else_is_quoted_once() {
        assert_eq!(shell_quote("/tmp/a-b_c.txt"), "/tmp/a-b_c.txt");
        assert_eq!(shell_quote("/tmp/with space.png"), "'/tmp/with space.png'");
        assert_eq!(shell_quote("/tmp/it's"), "'/tmp/it'\\''s'");
        assert_eq!(shell_quote("/tmp/$HOME;rm"), "'/tmp/$HOME;rm'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(
            paste_paths(&["/a b".to_owned(), "/c".to_owned()]),
            "'/a b' /c ",
            "a space after each, as a drop on Terminal.app types"
        );
    }

    #[test]
    fn a_name_that_could_leave_its_root_is_refused() {
        assert_eq!(safe_relative("dir/f.txt"), Some(PathBuf::from("dir/f.txt")));
        for bad in ["", "/etc/passwd", "../up", "a/../../b", "./x"] {
            assert_eq!(safe_relative(bad), None, "{bad:?}");
        }
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
}
