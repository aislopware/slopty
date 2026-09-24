//! Files both ways: an upload of dropped files, a download of a worker file dragged out, and the
//! pure pieces around them (which files a drop is, how a path is typed into a shell).
//!
//! An upload announces itself with [`XferMsg::Begin`], then sends one bulk stream per file,
//! one after another, at bulk priority. A stream cut short is resumed: the sender asks the worker
//! how much of the file is durable ([`XferMsg::Resume`] → [`XferMsg::Offset`]) and sends the rest.
//! A download is the worker's bulk streams landing in a directory as `name.partial`, renamed when
//! whole.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use parking_lot::Mutex;
use slopty_core::XferId;
use slopty_net::streams::RawRecv;
use slopty_net::{ClientMsg, Connection, NetError};
use slopty_proto::transfer::{BulkHeader, Dest, Purpose, XferMsg};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _, AsyncWriteExt as _};
use tokio::sync::{mpsc, oneshot};

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
    /// Downloads being received.
    downloads: HashMap<XferId, Download>,
}

#[derive(Debug)]
struct Download {
    into: PathBuf,
    /// Files the worker said it sends ([`XferMsg::Begin`]), once it has said.
    expected: Option<u32>,
    landed: Vec<PathBuf>,
    done: Option<oneshot::Sender<Result<Vec<PathBuf>, String>>>,
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
    ) -> oneshot::Receiver<Result<Vec<PathBuf>, String>> {
        let (tx, rx) = oneshot::channel();
        let download = Download { into, expected: None, landed: Vec::new(), done: Some(tx) };
        self.inner.lock().downloads.insert(xfer, download);
        rx
    }

    fn download_dir(&self, xfer: XferId) -> Option<PathBuf> {
        self.inner.lock().downloads.get(&xfer).map(|d| d.into.clone())
    }

    /// A control message about a transfer this link knows. Returns `true` when it was only
    /// the tasks' business (an [`XferMsg::Offset`]), so the UI need not hear it.
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
            XferMsg::Failed { xfer, error, .. } => {
                let failed = self.inner.lock().downloads.remove(xfer);
                if let Some(done) = failed.and_then(|d| d.done) {
                    let _gone = done.send(Err(error.clone()));
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    fn landed(&self, xfer: XferId, path: PathBuf) {
        let mut inner = self.inner.lock();
        if let Some(d) = inner.downloads.get_mut(&xfer) {
            d.landed.push(path);
        }
        let whole = take_if_whole(&mut inner, xfer);
        drop(inner);
        finish(whole);
    }
}

/// The download, taken out of the table, when every file it promised has landed.
fn take_if_whole(inner: &mut Tables, xfer: XferId) -> Option<Download> {
    let whole = inner.downloads.get(&xfer).is_some_and(|d| {
        d.expected.is_some_and(|n| usize::try_from(n).is_ok_and(|n| d.landed.len() >= n))
    });
    if whole { inner.downloads.remove(&xfer) } else { None }
}

fn finish(whole: Option<Download>) {
    if let Some(Download { done: Some(done), landed, .. }) = whole {
        let _gone = done.send(Ok(landed));
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
/// wait until every file of it has landed in `into`. Returns the top-level paths landed.
pub async fn download(
    up: &Uplink,
    xfer: XferId,
    path: String,
    into: PathBuf,
) -> Result<Vec<PathBuf>, String> {
    let done = up.table.expect_download(xfer, into);
    send(up, XferMsg::Fetch { xfer, path }).await?;
    done.await.map_err(|_dropped| "link closed".to_owned())?
}

/// A bulk stream of a download: written to `name.partial` in the transfer's directory, synced,
/// and renamed into place.
pub async fn receive(table: &Table, header: BulkHeader, mut rx: RawRecv) {
    let Some(dir) = table.download_dir(header.xfer) else {
        tracing::debug!(xfer = %header.xfer, "bulk for no download here; stopping it");
        rx.stop();
        return;
    };
    let xfer = header.xfer;
    match write_file(&dir, &header, &mut rx).await {
        Ok(path) => table.landed(xfer, path),
        Err(error) => {
            rx.stop();
            let failed = XferMsg::Failed { xfer, name: Some(header.name), error };
            table.on_control(&failed);
        }
    }
}

async fn write_file(dir: &Path, header: &BulkHeader, rx: &mut RawRecv) -> Result<PathBuf, String> {
    let rel = safe_relative(&header.name).ok_or_else(|| format!("bad name {:?}", header.name))?;
    let target = dir.join(rel);
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| e.to_string())?;
    }
    let mut partial_name = target.clone().into_os_string();
    partial_name.push(".partial");
    let partial = PathBuf::from(partial_name);
    let mut file = tokio::fs::File::create(&partial).await.map_err(|e| e.to_string())?;
    let mut got = 0_u64;
    while let Some(chunk) = rx.chunk(CHUNK).await.map_err(|e| e.to_string())? {
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        got = got.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
    }
    let expected = header.size.saturating_sub(header.offset);
    if got != expected {
        return Err(format!("{}: {got} of {expected} bytes", header.name));
    }
    file.sync_all().await.map_err(|e| e.to_string())?;
    drop(file);
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::Permissions::from_mode(header.mode & 0o777);
        tokio::fs::set_permissions(&partial, mode).await.map_err(|e| e.to_string())?;
    }
    tokio::fs::rename(&partial, &target).await.map_err(|e| e.to_string())?;
    Ok(target)
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
        let mut done = table.expect_download(xfer, PathBuf::from("/tmp"));
        table.landed(xfer, PathBuf::from("/tmp/a"));
        assert!(done.try_recv().is_err(), "the count is not known yet");
        assert!(table.on_control(&XferMsg::Begin { xfer, dest: None, files: 2, bytes: 3 }));
        assert!(done.try_recv().is_err(), "one of two");
        table.landed(xfer, PathBuf::from("/tmp/b"));
        assert_eq!(done.try_recv().unwrap().unwrap().len(), 2);
    }
}
