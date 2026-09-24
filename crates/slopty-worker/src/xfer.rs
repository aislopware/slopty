//! File transfer, the worker's half: where an upload lands, and writing it so that a cut never
//! leaves half a file under the real name.
//!
//! [`Transfers`] holds every transfer a client began ([`Transfers::begin`]) until its last file
//! landed. Each top-level entry of a drop (a file, or a directory and everything under it) goes
//! to the transfer's base directory, or to `<drop>/<xfer>/` when that name is taken there; the
//! choice is made once per entry, on its first file. A file is written to `name.partial`
//! ([`Receiving`]), synced, and renamed into place, so a retried file resumes from the bytes the
//! partial holds ([`durable`]).
//!
//! A download goes the other way, and resumes the same way: a retried fetch names the bytes the
//! client holds of each file, and [`Transfers::resume_points`] sends a file from there when
//! this worker sent that very version of it before, from the start otherwise.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use parking_lot::Mutex;
use slopty_core::XferId;
use slopty_proto::transfer::{Dest, Hash};
use tokio::sync::{Notify, watch};

/// Progress is reported at most this often per transfer.
pub const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// Why a transfer or one of its files failed.
#[derive(Debug, thiserror::Error)]
pub enum XferError {
    /// No transfer by that id began (or it finished).
    #[error("no such transfer")]
    Unknown,
    /// A name that is absolute, empty, or climbs out with `..`.
    #[error("refused name {0:?}")]
    Name(String),
    /// The stream ended before the file did.
    #[error("the file ended at {got} of {size} bytes")]
    Incomplete {
        /// Bytes held.
        got: u64,
        /// Bytes announced.
        size: u64,
    },
    /// More bytes came than the header announced.
    #[error("more than the {size} bytes announced")]
    Overrun {
        /// Bytes announced.
        size: u64,
    },
    /// A resume asked to start past what the partial file holds.
    #[error("resume at {asked} but only {held} bytes are held")]
    ResumePast {
        /// Offset asked for.
        asked: u64,
        /// Bytes in the partial file.
        held: u64,
    },
    /// The disk.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A file whole and in place.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Landed {
    /// Where.
    pub path: PathBuf,
    /// Its size.
    pub size: u64,
    /// Its BLAKE3 digest.
    pub hash: Hash,
}

/// Every file of a transfer landed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Finished {
    /// The top-level entries, in the order their first file arrived.
    pub paths: Vec<PathBuf>,
    /// The transfer was for a streamed window: the paths go on the pasteboard.
    pub staging: bool,
}

#[derive(Debug)]
struct Transfer {
    /// Where top-level entries go unless their name is taken there.
    base: PathBuf,
    /// Where they go when it is: `<drop>/<xfer>/`.
    fallback: PathBuf,
    staging: bool,
    files: u32,
    /// Each top-level entry and the directory it landed in, in first-sight order.
    roots: Vec<(String, PathBuf)>,
    landed: HashMap<String, Landed>,
    received: u64,
    reported: Option<Instant>,
    cancel: watch::Sender<bool>,
}

impl Transfer {
    /// The directory top-level entry `top` lands in, chosen on its first sight.
    fn root(&mut self, top: &str) -> PathBuf {
        if let Some((_top, root)) = self.roots.iter().find(|(n, _root)| n == top) {
            return root.clone();
        }
        let taken = std::fs::symlink_metadata(self.base.join(top)).is_ok();
        let root = if taken { self.fallback.clone() } else { self.base.clone() };
        self.roots.push((top.to_owned(), root.clone()));
        root
    }

    /// Record a landed file; `true` once every file has.
    fn land(&mut self, name: &str, landed: Landed) -> bool {
        self.landed.insert(name.to_owned(), landed);
        self.landed.len() >= usize::try_from(self.files).unwrap_or(usize::MAX)
    }

    fn progress(&mut self, bytes: u64, now: Instant) -> Option<u64> {
        self.received = self.received.saturating_add(bytes);
        let due =
            self.reported.is_none_or(|at| now.saturating_duration_since(at) >= PROGRESS_EVERY);
        if due {
            self.reported = Some(now);
        }
        due.then_some(self.received)
    }
}

/// Most file versions remembered for resuming downloads; the oldest goes first.
pub const REMEMBERED_SENDS: usize = 4096;

/// Which version of each file downloads sent, so a resume only continues the bytes of the
/// same version.
#[derive(Debug, Default)]
struct Sent {
    /// By path on this worker: the size and modification time sent, and when (a sequence).
    files: HashMap<PathBuf, ((u64, u64), u64)>,
    next: u64,
}

impl Sent {
    fn same(&self, file: &Outgoing) -> bool {
        self.files
            .get(&file.path)
            .is_some_and(|(version, _at)| *version == (file.size, file.mtime_ms))
    }

    fn record(&mut self, file: &Outgoing) {
        self.next = self.next.saturating_add(1);
        self.files.insert(file.path.clone(), ((file.size, file.mtime_ms), self.next));
        if self.files.len() > REMEMBERED_SENDS
            && let Some(oldest) =
                self.files.iter().min_by_key(|(_path, (_version, at))| *at).map(|(p, _)| p.clone())
        {
            self.files.remove(&oldest);
        }
    }
}

/// The transfers in flight.
#[derive(Debug)]
pub struct Transfers {
    drop_root: PathBuf,
    inner: Mutex<HashMap<XferId, Transfer>>,
    sent: Mutex<Sent>,
    /// Woken on every [`Transfers::begin`]: a file's stream can overtake its transfer's
    /// `Begin`, which rides the control stream.
    begun: Notify,
}

/// `name` as a relative path: `/`-separated, no empty, `.` or `..` component, not absolute.
fn relative(name: &str) -> Result<PathBuf, XferError> {
    let refused = || XferError::Name(name.to_owned());
    if name.is_empty() || name.starts_with('/') || name.contains('\0') {
        return Err(refused());
    }
    let path = PathBuf::from(name);
    if name.split('/').any(|c| c.is_empty() || c == "." || c == "..")
        || !path.components().all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(refused());
    }
    Ok(path)
}

/// Where `target`'s bytes are written until it is whole.
#[must_use]
pub fn partial_of(target: &Path) -> PathBuf {
    let mut name = target.as_os_str().to_owned();
    name.push(".partial");
    PathBuf::from(name)
}

/// Bytes of `target` held durably in its partial file, synced now so the answer holds after a
/// crash; 0 when there is none.
#[must_use]
pub fn durable(target: &Path) -> u64 {
    let Ok(file) = File::options().write(true).open(partial_of(target)) else { return 0 };
    if file.sync_all().is_err() {
        return 0;
    }
    file.metadata().map_or(0, |m| m.len())
}

impl Transfers {
    /// Transfers whose clashing and staged entries go under `drop_root/<xfer>/`.
    #[must_use]
    pub fn new(drop_root: PathBuf) -> Self {
        Self { drop_root, inner: Mutex::default(), sent: Mutex::default(), begun: Notify::new() }
    }

    /// `~/.slopty/drop`.
    #[must_use]
    pub fn default_drop_root() -> PathBuf {
        crate::file::expand_home(Path::new("~/.slopty/drop"))
    }

    /// A client begins transfer `xfer` of `files` files to `dest`. `cwd` is the session's
    /// directory for [`Dest::SessionCwd`] (`None` when it never said: the drop directory).
    /// Beginning a transfer that is known already (a retry) keeps what it has and re-arms it.
    pub fn begin(&self, xfer: XferId, dest: &Dest, cwd: Option<&str>, files: u32) {
        let fallback = self.drop_root.join(xfer.to_string());
        let (base, staging) = match dest {
            Dest::SessionCwd(_) => (cwd.map(|c| crate::file::expand_home(Path::new(c))), false),
            Dest::Staging => (None, true),
            Dest::Path(p) => (Some(crate::file::expand_home(Path::new(p))), false),
        };
        let transfer = Transfer {
            base: base.unwrap_or_else(|| fallback.clone()),
            fallback,
            staging,
            files,
            roots: Vec::new(),
            landed: HashMap::new(),
            received: 0,
            reported: None,
            cancel: watch::Sender::new(false),
        };
        match self.inner.lock().entry(xfer) {
            std::collections::hash_map::Entry::Occupied(known) => {
                known.get().cancel.send_replace(false);
            }
            std::collections::hash_map::Entry::Vacant(new) => {
                new.insert(transfer);
            }
        }
        self.begun.notify_waiters();
    }

    /// Wait up to `wait` for `xfer` to begin; `false` when it did not.
    pub async fn begun(&self, xfer: XferId, wait: Duration) -> bool {
        let deadline = tokio::time::Instant::now().checked_add(wait);
        loop {
            let notified = self.begun.notified();
            let mut notified = std::pin::pin!(notified);
            notified.as_mut().enable();
            if self.inner.lock().contains_key(&xfer) {
                return true;
            }
            let Some(deadline) = deadline else { return false };
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return false;
            }
        }
    }

    /// Where file `name` of `xfer` lands. The first file of a top-level entry decides for the
    /// whole entry: the base directory, or the fallback when the name is taken there.
    pub fn target(&self, xfer: XferId, name: &str) -> Result<PathBuf, XferError> {
        let rel = relative(name)?;
        let top = name.split('/').next().unwrap_or(name);
        let root = self.inner.lock().get_mut(&xfer).ok_or(XferError::Unknown)?.root(top);
        Ok(root.join(rel))
    }

    /// Bytes of `name` the worker holds durably: all of them once it landed, else what its
    /// partial file holds.
    pub fn durable(&self, xfer: XferId, name: &str) -> Result<u64, XferError> {
        if let Some(landed) = self.inner.lock().get(&xfer).and_then(|t| t.landed.get(name)) {
            return Ok(landed.size);
        }
        Ok(durable(&self.target(xfer, name)?))
    }

    /// `name` of `xfer` is whole and in place; once every file is, the transfer is done and
    /// forgotten.
    pub fn landed(&self, xfer: XferId, name: &str, landed: Landed) -> Option<Finished> {
        if !self.inner.lock().get_mut(&xfer)?.land(name, landed) {
            return None;
        }
        let t = self.inner.lock().remove(&xfer)?;
        let paths = t.roots.iter().map(|(top, root)| root.join(top)).collect();
        Some(Finished { paths, staging: t.staging })
    }

    /// `bytes` more of `xfer` arrived; the total when a progress report is due (at most every
    /// [`PROGRESS_EVERY`]).
    pub fn progress(&self, xfer: XferId, bytes: u64, now: Instant) -> Option<u64> {
        self.inner.lock().get_mut(&xfer)?.progress(bytes, now)
    }

    /// Stop `xfer`'s streams; its partial files stay for a resume.
    pub fn cancel(&self, xfer: XferId) {
        if let Some(t) = self.inner.lock().get(&xfer) {
            t.cancel.send_replace(true);
        }
    }

    /// Where each of `files` starts in a download: at the bytes the client holds of it (`held`,
    /// by name) when this worker sent the same version of it (size and modification time)
    /// before, else at 0, so a file that changed since starts over. Remembers the versions
    /// sent now for the next resume.
    pub fn resume_points(&self, files: &[Outgoing], held: &[(String, u64)]) -> Vec<u64> {
        let mut sent = self.sent.lock();
        files
            .iter()
            .map(|file| {
                let holds = held.iter().find(|(name, _)| *name == file.name).map(|(_, n)| *n);
                let at = match holds {
                    Some(n) if n <= file.size && sent.same(file) => n,
                    _ => 0,
                };
                sent.record(file);
                at
            })
            .collect()
    }

    /// Changes to `true` when `xfer` is cancelled.
    #[must_use]
    pub fn cancelled(&self, xfer: XferId) -> Option<watch::Receiver<bool>> {
        self.inner.lock().get(&xfer).map(|t| t.cancel.subscribe())
    }
}

/// One file being written: into its partial file, hashed as it goes.
#[derive(Debug)]
pub struct Receiving {
    file: File,
    target: PathBuf,
    hasher: blake3::Hasher,
    at: u64,
    size: u64,
}

impl Receiving {
    /// Start writing `target` (`size` bytes) at `offset`: 0 starts over, anything else resumes
    /// the partial file, which must hold at least that much. Missing directories are made.
    pub fn open(target: &Path, offset: u64, size: u64) -> Result<Self, XferError> {
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(partial_of(target))?;
        let held = file.metadata()?.len();
        if offset > held || offset > size {
            return Err(XferError::ResumePast { asked: offset, held });
        }
        file.set_len(offset)?;
        let mut hasher = blake3::Hasher::new();
        file.seek(SeekFrom::Start(0))?;
        std::io::copy(&mut (&mut file).take(offset), &mut hasher)?;
        file.seek(SeekFrom::Start(offset))?;
        Ok(Self { file, target: target.to_path_buf(), hasher, at: offset, size })
    }

    /// Bytes of the file written so far.
    #[must_use]
    pub const fn at(&self) -> u64 {
        self.at
    }

    /// The next bytes.
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), XferError> {
        let at = self.at.saturating_add(bytes.len() as u64);
        if at > self.size {
            return Err(XferError::Overrun { size: self.size });
        }
        self.file.write_all(bytes)?;
        self.hasher.update(bytes);
        self.at = at;
        Ok(())
    }

    /// The file is whole: sync it, give it `mode` (when not 0) and its modification time,
    /// rename it into place and sync the directory.
    pub fn finish(self, mode: u32, mtime_ms: u64) -> Result<Landed, XferError> {
        use std::os::unix::fs::PermissionsExt as _;
        if self.at != self.size {
            return Err(XferError::Incomplete { got: self.at, size: self.size });
        }
        let mtime = SystemTime::UNIX_EPOCH.checked_add(Duration::from_millis(mtime_ms));
        if let Some(mtime) = mtime.filter(|_| mtime_ms > 0) {
            self.file.set_modified(mtime)?;
        }
        if mode & 0o777 != 0 {
            self.file.set_permissions(std::fs::Permissions::from_mode(mode & 0o777))?;
        }
        self.file.sync_all()?;
        std::fs::rename(partial_of(&self.target), &self.target)?;
        if let Some(dir) = self.target.parent() {
            File::open(dir)?.sync_all()?;
        }
        Ok(Landed { path: self.target, size: self.size, hash: self.hasher.finalize().into() })
    }

    /// The stream was cut or cancelled: make what was written durable for a resume. Returns
    /// the bytes held.
    pub fn keep(self) -> u64 {
        let _synced = self.file.sync_all();
        self.at
    }
}

/// One file of a download: its name relative to the fetched path's parent, where it is, and
/// what its header says.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Outgoing {
    /// `/`-separated, starting with the fetched entry's own name.
    pub name: String,
    /// On this worker.
    pub path: PathBuf,
    /// Bytes.
    pub size: u64,
    /// Modification time, milliseconds since the Unix epoch.
    pub mtime_ms: u64,
    /// Permission bits.
    pub mode: u32,
}

/// Most files one fetch sends: a drag of a home directory must not walk the disk.
pub const MAX_OUTGOING: usize = 10_000;

/// The files under `path` (itself when it is a file), symbolic links left out.
pub fn outgoing(path: &Path) -> std::io::Result<Vec<Outgoing>> {
    let base = path.parent().unwrap_or_else(|| Path::new("/"));
    let mut out = Vec::new();
    let mut stack = vec![path.to_path_buf()];
    while let Some(at) = stack.pop() {
        let meta = std::fs::symlink_metadata(&at)?;
        if meta.is_dir() {
            let mut entries: Vec<PathBuf> =
                std::fs::read_dir(&at)?.filter_map(|e| Some(e.ok()?.path())).collect();
            entries.sort_unstable_by(|a, b| b.cmp(a));
            stack.extend(entries);
        } else if meta.is_file() {
            use std::os::unix::fs::PermissionsExt as _;
            let rel = at.strip_prefix(base).unwrap_or(&at);
            let name = rel.to_string_lossy().into_owned();
            let mtime_ms = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
            out.push(Outgoing {
                name,
                path: at,
                size: meta.len(),
                mtime_ms,
                mode: meta.permissions().mode() & 0o777,
            });
            if out.len() >= MAX_OUTGOING {
                break;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use slopty_core::SessionId;

    use super::*;

    fn transfers(dir: &Path) -> Transfers {
        Transfers::new(dir.join("drop"))
    }

    #[test]
    fn names_that_climb_out_are_refused() {
        for bad in ["", "/etc/passwd", "../x", "a/../../x", "a//b", "./a", "a/.", "a\0b"] {
            assert!(matches!(relative(bad), Err(XferError::Name(_))), "{bad:?}");
        }
        assert_eq!(relative("dir/a b.txt").unwrap(), PathBuf::from("dir/a b.txt"));
    }

    /// A file is written beside its name, and only renamed once whole: its digest is of every
    /// byte, and its mode and time are the sender's.
    #[test]
    fn a_file_lands_under_its_name_only_when_whole() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sub/a.txt");
        let mut rx = Receiving::open(&target, 0, 10).unwrap();
        rx.write(b"hello").unwrap();
        assert!(!target.exists() && partial_of(&target).exists());
        assert!(matches!(rx.write(b"too many!!"), Err(XferError::Overrun { size: 10 })));
        rx.write(b"world").unwrap();
        let landed = rx.finish(0o600, 1_700_000_000_000).unwrap();
        assert_eq!(landed.hash, *blake3::hash(b"helloworld").as_bytes());
        assert_eq!(std::fs::read(&target).unwrap(), b"helloworld");
        assert!(!partial_of(&target).exists());
        let meta = std::fs::metadata(&target).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        let mtime = meta.modified().unwrap().duration_since(SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(mtime.as_millis(), 1_700_000_000_000);
    }

    /// A cut leaves the partial; a resume at its length finishes the file with a digest of the
    /// whole; a resume past it is refused; a short stream is not renamed.
    #[test]
    fn a_cut_file_resumes_from_what_is_durable() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("big.bin");
        let body: Vec<u8> = (0..100_000_u32).map(|i| (i % 251) as u8).collect();
        let mut rx = Receiving::open(&target, 0, body.len() as u64).unwrap();
        rx.write(&body[..40_000]).unwrap();
        assert_eq!(rx.keep(), 40_000);
        assert_eq!(durable(&target), 40_000);
        assert!(matches!(
            Receiving::open(&target, 50_000, body.len() as u64),
            Err(XferError::ResumePast { asked: 50_000, held: 40_000 })
        ));
        let mut rx = Receiving::open(&target, 40_000, body.len() as u64).unwrap();
        rx.write(&body[40_000..90_000]).unwrap();
        let short = Receiving::finish(rx, 0, 0);
        assert!(matches!(short, Err(XferError::Incomplete { got: 90_000, .. })));
        assert!(!target.exists(), "a short file keeps its partial name");
        let mut rx = Receiving::open(&target, durable(&target), body.len() as u64).unwrap();
        assert_eq!(rx.at(), 90_000);
        rx.write(&body[90_000..]).unwrap();
        let landed = rx.finish(0, 0).unwrap();
        assert_eq!(landed.hash, *blake3::hash(&body).as_bytes());
        assert_eq!(std::fs::read(&target).unwrap(), body);
    }

    /// Entries land in the session's directory, a clashing one in the drop directory with the
    /// rest of its entry, and the transfer finishes with the top-level paths once every file
    /// is in.
    #[test]
    fn a_clash_lands_in_the_drop_directory_and_finish_names_the_entries() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("cwd");
        std::fs::create_dir_all(cwd.join("taken")).unwrap();
        let t = transfers(dir.path());
        let xfer = XferId::new();
        t.begin(xfer, &Dest::SessionCwd(SessionId::new()), cwd.to_str(), 3);
        let fallback = dir.path().join("drop").join(xfer.to_string());
        assert_eq!(t.target(xfer, "new.txt").unwrap(), cwd.join("new.txt"));
        assert_eq!(t.target(xfer, "taken/a").unwrap(), fallback.join("taken/a"));
        std::fs::create_dir_all(cwd.join("dir")).unwrap();
        assert_eq!(t.target(xfer, "taken/b").unwrap(), fallback.join("taken/b"), "per entry");
        assert!(matches!(t.target(XferId::new(), "x"), Err(XferError::Unknown)));

        let land = |name: &str| Landed { path: PathBuf::from(name), size: 1, hash: [0; 32] };
        assert_eq!(t.landed(xfer, "new.txt", land("new.txt")), None);
        assert_eq!(t.durable(xfer, "new.txt").unwrap(), 1, "a landed file is all there");
        assert_eq!(t.landed(xfer, "taken/a", land("a")), None);
        let done = t.landed(xfer, "taken/b", land("b")).unwrap();
        assert_eq!(done.paths, [cwd.join("new.txt"), fallback.join("taken")]);
        assert!(!done.staging);
        assert!(matches!(t.target(xfer, "x"), Err(XferError::Unknown)), "forgotten once done");
    }

    #[test]
    fn staging_goes_to_the_drop_directory_and_progress_is_paced() {
        let dir = tempfile::tempdir().unwrap();
        let t = transfers(dir.path());
        let xfer = XferId::new();
        t.begin(xfer, &Dest::Staging, None, 1);
        let target = t.target(xfer, "shot.png").unwrap();
        assert_eq!(target, dir.path().join("drop").join(xfer.to_string()).join("shot.png"));
        let now = Instant::now();
        assert_eq!(t.progress(xfer, 10, now), Some(10));
        assert_eq!(t.progress(xfer, 10, now), None, "within 100 ms");
        assert_eq!(t.progress(xfer, 5, now.checked_add(PROGRESS_EVERY).unwrap()), Some(25));
        let mut cancelled = t.cancelled(xfer).unwrap();
        t.cancel(xfer);
        assert!(*cancelled.borrow_and_update());
        t.begin(xfer, &Dest::Staging, None, 1);
        assert!(!*cancelled.borrow_and_update(), "a retry re-arms it");
    }

    #[tokio::test]
    async fn a_stream_that_overtakes_its_begin_waits_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let t = std::sync::Arc::new(transfers(dir.path()));
        let xfer = XferId::new();
        assert!(!t.begun(xfer, Duration::from_millis(20)).await, "never begun");
        let waiter = tokio::spawn({
            let t = std::sync::Arc::clone(&t);
            async move { t.begun(xfer, Duration::from_secs(5)).await }
        });
        tokio::task::yield_now().await;
        t.begin(XferId::new(), &Dest::Staging, None, 1);
        t.begin(xfer, &Dest::Staging, None, 1);
        assert!(waiter.await.unwrap());
    }

    /// A retried download continues a held file only when it is the version sent before; a
    /// changed one, one past its size, or one this worker never sent starts over.
    #[test]
    fn a_download_resumes_only_the_version_it_sent() {
        let dir = tempfile::tempdir().unwrap();
        let t = transfers(dir.path());
        let file = |name: &str, size: u64, mtime_ms: u64| Outgoing {
            name: name.to_owned(),
            path: dir.path().join(name),
            size,
            mtime_ms,
            mode: 0o644,
        };
        let first = [file("a", 100, 1), file("b", 50, 1), file("c", 10, 1)];
        let held = [("a".to_owned(), 40), ("b".to_owned(), 50), ("c".to_owned(), 3)];
        assert_eq!(t.resume_points(&first, &held), [0, 0, 0], "never sent: from the start");
        let again = [file("a", 100, 1), file("b", 50, 2), file("c", 10, 1), file("d", 5, 1)];
        let held = [("a".to_owned(), 40), ("b".to_owned(), 50), ("c".to_owned(), 11)];
        assert_eq!(
            t.resume_points(&again, &held),
            [40, 0, 0, 0],
            "the same version resumes; a newer one, a claim past the end, an unheld one do not"
        );
        assert_eq!(t.resume_points(&again, &[("b".to_owned(), 50)]), [0, 50, 0, 0]);
    }

    #[test]
    fn a_fetch_lists_a_directory_under_its_own_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("a.txt"), b"a").unwrap();
        std::fs::write(root.join("src/b.rs"), b"bb").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("link")).unwrap();
        let files = outgoing(&root).unwrap();
        let names: Vec<(&str, u64)> = files.iter().map(|f| (f.name.as_str(), f.size)).collect();
        assert_eq!(names, [("proj/a.txt", 1), ("proj/src/b.rs", 2)]);
        let one = outgoing(&root.join("a.txt")).unwrap();
        assert_eq!(one[0].name, "a.txt");
    }
}
