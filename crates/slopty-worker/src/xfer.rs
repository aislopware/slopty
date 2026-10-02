//! File transfer, the worker's half: where an upload lands, and writing it so that a cut never
//! leaves half a file under the real name.
//!
//! [`Transfers`] holds every transfer a client began ([`Transfers::begin`]) until its last file
//! landed. Each top-level entry of a drop (a file, or a directory and everything under it) goes
//! to the transfer's base directory, or to `<drop>/<xfer>/` when that name is taken there; the
//! choice is made once per entry, on its first file, and a name another transfer in flight
//! claimed there counts as taken, so two drops of the same name never share a partial file. A
//! file is written to `name.partial` ([`Receiving`]), synced, and renamed into place, so a
//! retried file resumes from the bytes the partial holds ([`durable`]).
//!
//! The entries of unfinished transfers are listed in a ledger in the drop directory, and
//! [`Transfers::sweep`] removes the partial files under them that nothing wrote to for
//! [`STALE_PARTIAL`]: an upload cut for good leaves nothing behind in the directory it went to.
//! A drag's landing that nothing landed from goes at once ([`Transfers::discard_drag`]).
//!
//! A download goes the other way, and resumes the same way: a retried fetch names the bytes the
//! client holds of each file and the version they are of, and [`resume_points`] sends a file from
//! there while it is still that version, from the start otherwise.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use parking_lot::Mutex;
use slopty_core::{WallMs, XferId};
use slopty_proto::drag::DragId;
use slopty_proto::transfer::{Dest, Hash, Held, MAX_FILES, MODE_BITS, partial_of, relative_path};
use tokio::sync::{Notify, watch};

/// Progress is reported at most this often per transfer.
pub const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// A partial file nothing wrote to for this long belongs to an upload nobody will resume.
pub const STALE_PARTIAL: Duration = Duration::from_hours(24);

/// The ledger of unfinished transfers' entries, in the drop directory: one JSON array of the
/// transfer id and the entry's path per line.
const LEDGER: &str = ".partials";

/// Discarded drags remembered, so an upload into one that comes late is not begun: a drag's
/// uploads begin on the control stream, which nothing orders against its end on the stream's.
const DISCARDED: usize = 32;

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
    /// The drag whose drop the files are, for an upload into its landing.
    pub drag: Option<DragId>,
}

#[derive(Debug)]
struct Transfer {
    /// Where top-level entries go unless their name is taken there.
    base: PathBuf,
    /// Where they go when it is: `<drop>/<xfer>/`.
    fallback: PathBuf,
    staging: bool,
    drag: Option<DragId>,
    files: u32,
    /// Each top-level entry and the directory it landed in, in first-sight order.
    roots: Vec<(String, PathBuf)>,
    landed: HashMap<String, Landed>,
    received: u64,
    reported: Option<Instant>,
    cancel: watch::Sender<bool>,
}

impl Transfer {
    /// The directory top-level entry `top` landed in, once chosen.
    fn known_root(&self, top: &str) -> Option<&PathBuf> {
        self.roots.iter().find(|(n, _root)| n == top).map(|(_top, root)| root)
    }

    /// Whether this transfer put a top-level entry at `entry`.
    fn claims(&self, entry: &Path) -> bool {
        self.roots.iter().any(|(top, root)| root.join(top) == entry)
    }

    /// Choose where `top` lands: the base directory, unless the name is there already or
    /// `claimed` by another transfer.
    fn choose_root(&mut self, top: &str, claimed: bool) -> PathBuf {
        let taken = claimed || std::fs::symlink_metadata(self.base.join(top)).is_ok();
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

/// Where each of `files` starts in a download: at the bytes the client holds of it (`held`).
///
/// A claim counts while it is of the version the file is now (its size and modification time),
/// else the file starts at 0, so one that changed since starts over. The claim carries its
/// version, so a resume holds on any link and across a restart of this worker; the client still
/// checks the whole file against its digest.
#[must_use]
pub fn resume_points(files: &[Outgoing], held: &[Held]) -> Vec<u64> {
    files
        .iter()
        .map(|file| {
            held.iter()
                .find(|h| h.name == file.name)
                .filter(|h| {
                    h.bytes <= file.size && (h.size, h.mtime_ms) == (file.size, file.mtime_ms)
                })
                .map_or(0, |h| h.bytes)
        })
        .collect()
}

/// The transfers in flight.
#[derive(Debug)]
pub struct Transfers {
    drop_root: PathBuf,
    inner: Mutex<HashMap<XferId, Transfer>>,
    /// Woken on every [`Transfers::begin`]: a file's stream can overtake its transfer's
    /// `Begin`, which rides the control stream.
    begun: Notify,
    /// Held while the ledger is read or written.
    ledger: Mutex<()>,
    /// The drags whose landing was discarded, newest last, up to [`DISCARDED`].
    discarded: Mutex<VecDeque<DragId>>,
}

/// `name` as a path under its transfer's root ([`relative_path`]).
fn relative(name: &str) -> Result<PathBuf, XferError> {
    relative_path(name).ok_or_else(|| XferError::Name(name.to_owned()))
}

/// One ledger line: `["<xfer>","<entry>"]` and a newline; `None` for a path that is not UTF-8.
fn ledger_line(xfer: XferId, entry: &Path) -> Option<String> {
    let mut line = serde_json::to_string(&(xfer.to_string(), entry.to_str()?)).ok()?;
    line.push('\n');
    Some(line)
}

/// The partial files of top-level entry `entry`: its own, and every one under it when it is a
/// directory. Symbolic links are not followed.
fn partials_under(entry: &Path, out: &mut Vec<PathBuf>) {
    let own = partial_of(entry);
    if std::fs::symlink_metadata(&own).is_ok_and(|m| m.is_file()) {
        out.push(own);
    }
    if !std::fs::symlink_metadata(entry).is_ok_and(|m| m.is_dir()) {
        return;
    }
    let Ok(dir) = std::fs::read_dir(entry) else { return };
    for child in dir.flatten() {
        let Ok(kind) = child.file_type() else { continue };
        let path = child.path();
        if kind.is_dir() {
            partials_under(&path, out);
        } else if path.extension().is_some_and(|x| x == "partial") {
            out.push(path);
        }
    }
}

/// Remove `dir` and every directory under it that holds nothing else, bottom up.
fn remove_empty_dirs(dir: &Path) {
    if let Ok(children) = std::fs::read_dir(dir) {
        for child in children.flatten() {
            if child.file_type().is_ok_and(|k| k.is_dir()) {
                remove_empty_dirs(&child.path());
            }
        }
    }
    // Fails, as it should, while anything is left in it.
    let _not_empty = std::fs::remove_dir(dir);
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
        Self {
            drop_root,
            inner: Mutex::default(),
            begun: Notify::new(),
            ledger: Mutex::default(),
            discarded: Mutex::default(),
        }
    }

    /// `~/.slopty/drop`.
    #[must_use]
    pub fn default_drop_root() -> PathBuf {
        crate::file::expand_home(Path::new("~/.slopty/drop"))
    }

    /// A client begins transfer `xfer` of `files` files to `dest`. `cwd` is the session's
    /// directory for [`Dest::SessionCwd`] (`None` when it never said: the drop directory).
    /// Beginning a transfer that is known already (a retry) keeps what it has and re-arms it.
    /// One into a discarded drag's landing is not begun, so its files are refused.
    pub fn begin(&self, xfer: XferId, dest: &Dest, cwd: Option<&str>, files: u32) {
        if let Dest::Drag(drag) = dest
            && self.discarded.lock().contains(drag)
        {
            tracing::debug!(%xfer, %drag, "an upload into a discarded drag");
            return;
        }
        let fallback = self.drop_root.join(xfer.to_string());
        let (base, staging) = match dest {
            Dest::SessionCwd(_) => (cwd.map(|c| crate::file::expand_home(Path::new(c))), false),
            Dest::Staging => (None, true),
            Dest::Attachment => (None, false),
            Dest::Path(p) => (Some(crate::file::expand_home(Path::new(p))), false),
            Dest::Drag(drag) => (Some(self.drag_dir(*drag)), false),
        };
        let drag = match dest {
            Dest::Drag(drag) => Some(*drag),
            Dest::SessionCwd(_) | Dest::Staging | Dest::Attachment | Dest::Path(_) => None,
        };
        let transfer = Transfer {
            base: base.unwrap_or_else(|| fallback.clone()),
            fallback,
            staging,
            drag,
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

    /// Where the files of a drop from the client's drag `drag` land: `drop_root/<drag>/`.
    #[must_use]
    pub fn drag_dir(&self, drag: DragId) -> PathBuf {
        self.drop_root.join(drag.to_string())
    }

    /// `drag` ended with nothing landed from its landing, so nothing will read it: the uploads
    /// into it stop and are forgotten, one that begins later is refused, and the landing is
    /// deleted with whatever reached it, whole files and partial ones. The ledger keeps their
    /// entries, so the sweep still finds a partial a stream wrote after this.
    pub fn discard_drag(&self, drag: DragId) {
        {
            let mut discarded = self.discarded.lock();
            if !discarded.contains(&drag) {
                if discarded.len() == DISCARDED {
                    discarded.pop_front();
                }
                discarded.push_back(drag);
            }
        }
        self.inner.lock().retain(|_, t| {
            let into = t.drag == Some(drag);
            if into {
                t.cancel.send_replace(true);
            }
            !into
        });
        let dir = self.drag_dir(drag);
        match std::fs::remove_dir_all(&dir) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                tracing::warn!(%drag, dir = %dir.display(), error = %e, "a drag's landing");
            }
            _ => {}
        }
    }

    /// The drag `xfer` uploads the drop of, while it is in flight.
    #[must_use]
    pub fn drag_of(&self, xfer: XferId) -> Option<DragId> {
        self.inner.lock().get(&xfer)?.drag
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
    /// whole entry: the base directory, or the fallback when the name is taken there or
    /// another transfer in flight put an entry of that name there.
    pub fn target(&self, xfer: XferId, name: &str) -> Result<PathBuf, XferError> {
        let rel = relative(name)?;
        let top = name.split('/').next().unwrap_or(name);
        let mut inner = self.inner.lock();
        let transfer = inner.get(&xfer).ok_or(XferError::Unknown)?;
        if let Some(root) = transfer.known_root(top) {
            return Ok(root.join(rel));
        }
        let entry = transfer.base.join(top);
        let claimed = inner.iter().any(|(id, other)| *id != xfer && other.claims(&entry));
        let root = inner.get_mut(&xfer).ok_or(XferError::Unknown)?.choose_root(top, claimed);
        drop(inner);
        self.record(xfer, &root.join(top));
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
        self.unrecord(xfer);
        let paths = t.roots.iter().map(|(top, root)| root.join(top)).collect();
        Some(Finished { paths, staging: t.staging, drag: t.drag })
    }

    /// Remove the partial files of unfinished transfers that nothing wrote to for `stale`, and
    /// the directories in the drop directory that leaves empty. The ledger keeps the entries
    /// that still hold a younger partial. Returns the partial files removed.
    pub fn sweep(&self, stale: Duration) -> usize {
        // Not held over the walk: a transfer placing an entry meanwhile only appends.
        let listed = {
            let _held = self.ledger.lock();
            self.read_ledger()
        };
        let now = SystemTime::now();
        let mut removed: usize = 0;
        let mut kept: Vec<(XferId, PathBuf)> = Vec::new();
        for (xfer, entry) in listed.iter().cloned() {
            let mut young = false;
            let mut partials = Vec::new();
            partials_under(&entry, &mut partials);
            for partial in partials {
                let written = std::fs::symlink_metadata(&partial).and_then(|m| m.modified());
                let age = written
                    .map_or(Duration::MAX, |at| now.duration_since(at).unwrap_or(Duration::ZERO));
                if age >= stale && std::fs::remove_file(&partial).is_ok() {
                    removed = removed.saturating_add(1);
                } else {
                    young = true;
                }
            }
            if let Ok(inside) = entry.strip_prefix(&self.drop_root)
                && let Some(Component::Normal(first)) = inside.components().next()
            {
                let dir = self.drop_root.join(first);
                remove_empty_dirs(&dir);
            }
            if young && !kept.contains(&(xfer, entry.clone())) {
                kept.push((xfer, entry));
            }
        }
        let _held = self.ledger.lock();
        let appended: Vec<(XferId, PathBuf)> =
            self.read_ledger().into_iter().filter(|line| !listed.contains(line)).collect();
        for line in appended {
            if !kept.contains(&line) {
                kept.push(line);
            }
        }
        self.write_ledger(&kept);
        removed
    }

    fn ledger_path(&self) -> PathBuf {
        self.drop_root.join(LEDGER)
    }

    /// List `entry` of `xfer` in the ledger, so a sweep finds its partial files if it never
    /// finishes. A path that is not UTF-8 is not listed.
    fn record(&self, xfer: XferId, entry: &Path) {
        use std::io::Write as _;
        let Some(line) = ledger_line(xfer, entry) else { return };
        let _held = self.ledger.lock();
        let appended = std::fs::create_dir_all(&self.drop_root).and_then(|()| {
            let mut file = File::options().create(true).append(true).open(self.ledger_path())?;
            file.write_all(line.as_bytes())
        });
        if let Err(e) = appended {
            tracing::warn!(%xfer, entry = %entry.display(), error = %e, "partial ledger");
        }
    }

    /// Drop `xfer`'s entries from the ledger: every file of it landed.
    fn unrecord(&self, xfer: XferId) {
        let _held = self.ledger.lock();
        let rest: Vec<(XferId, PathBuf)> =
            self.read_ledger().into_iter().filter(|(x, _entry)| *x != xfer).collect();
        self.write_ledger(&rest);
    }

    /// The ledger's entries; a line that does not read is skipped.
    fn read_ledger(&self) -> Vec<(XferId, PathBuf)> {
        let Ok(text) = std::fs::read_to_string(self.ledger_path()) else { return Vec::new() };
        text.lines()
            .filter_map(|line| serde_json::from_str::<(String, PathBuf)>(line).ok())
            .filter_map(|(xfer, entry)| Some((xfer.parse().ok()?, entry)))
            .collect()
    }

    /// Replace the ledger with `entries` (`slopty_platform::fs::replace`), so a crash leaves the
    /// old list or the new. No entries removes it.
    fn write_ledger(&self, entries: &[(XferId, PathBuf)]) {
        let path = self.ledger_path();
        if entries.is_empty() {
            let _absent = std::fs::remove_file(&path);
            return;
        }
        let text: String = entries.iter().filter_map(|(x, e)| ledger_line(*x, e)).collect();
        if let Err(e) = slopty_platform::fs::replace(&path, text.as_bytes()) {
            tracing::warn!(path = %path.display(), error = %e, "partial ledger");
        }
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

    /// The file is whole: give it `mode` (when not 0) and its modification time, hand its
    /// bytes to the drive and rename it into place.
    pub fn finish(self, mode: u32, mtime: WallMs) -> Result<Landed, XferError> {
        use std::os::unix::fs::PermissionsExt as _;
        if self.at != self.size {
            return Err(XferError::Incomplete { got: self.at, size: self.size });
        }
        if let Some(mtime) = mtime.to_system() {
            self.file.set_modified(mtime)?;
        }
        if mode & MODE_BITS != 0 {
            self.file.set_permissions(std::fs::Permissions::from_mode(mode & MODE_BITS))?;
        }
        land(&self.file, &partial_of(&self.target), &self.target)?;
        Ok(Landed { path: self.target, size: self.size, hash: self.hasher.finalize().into() })
    }

    /// The stream was cut or cancelled: make what was written durable for a resume. Returns
    /// the bytes held.
    pub fn keep(self) -> u64 {
        let _synced = self.file.sync_all();
        self.at
    }
}

/// Hand a whole, checked file's bytes to the drive, then rename its partial file into place.
///
/// A plain `fsync`, not the drive-cache flush `sync_all` is on Apple platforms, and no
/// directory sync: 0.2 ms a file against 7.6 (MEASUREMENTS.md, "syncing a landed file").
/// What a resume claims to hold is still fully synced ([`durable`]).
///
/// # Errors
///
/// The sync or the rename failing.
pub fn land(file: &File, partial: &Path, target: &Path) -> std::io::Result<()> {
    rustix::fs::fsync(file).map_err(std::io::Error::from)?;
    std::fs::rename(partial, target)
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
    /// Modification time.
    pub mtime_ms: WallMs,
    /// Permission bits.
    pub mode: u32,
}

/// The files under `path` (itself when it is a file), symbolic links left out, up to
/// [`MAX_FILES`]. A name that is not UTF-8 cannot travel and is skipped.
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
            let Some(name) = rel.to_str().map(str::to_owned) else {
                tracing::warn!(path = %at.display(), "skipped: name is not UTF-8");
                continue;
            };
            let mtime_ms = meta.modified().map_or(WallMs::ZERO, WallMs::of);
            out.push(Outgoing {
                name,
                path: at,
                size: meta.len(),
                mtime_ms,
                mode: meta.permissions().mode() & MODE_BITS,
            });
            if out.len() >= MAX_FILES {
                tracing::warn!(path = %path.display(), "the fetch stops at {MAX_FILES} files");
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
        assert!(matches!(relative("a/../../x"), Err(XferError::Name(_))));
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
        let landed = rx.finish(0o600, WallMs::from_millis(1_700_000_000_000)).unwrap();
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
        let short = Receiving::finish(rx, 0, WallMs::ZERO);
        assert!(matches!(short, Err(XferError::Incomplete { got: 90_000, .. })));
        assert!(!target.exists(), "a short file keeps its partial name");
        let mut rx = Receiving::open(&target, durable(&target), body.len() as u64).unwrap();
        assert_eq!(rx.at(), 90_000);
        rx.write(&body[90_000..]).unwrap();
        let landed = rx.finish(0, WallMs::ZERO).unwrap();
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

    /// Two drops of one name into one directory at once: the second lands in its own drop
    /// directory, so their partial files are two.
    #[test]
    fn two_drops_of_one_name_never_share_a_partial() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("cwd");
        let t = transfers(dir.path());
        let (first, second) = (XferId::new(), XferId::new());
        let dest = Dest::SessionCwd(SessionId::new());
        t.begin(first, &dest, cwd.to_str(), 1);
        t.begin(second, &dest, cwd.to_str(), 1);
        let a = t.target(first, "a.txt").unwrap();
        let b = t.target(second, "a.txt").unwrap();
        assert_eq!(a, cwd.join("a.txt"));
        assert_eq!(b, dir.path().join("drop").join(second.to_string()).join("a.txt"));
        assert_ne!(partial_of(&a), partial_of(&b));
        assert_eq!(t.target(first, "a.txt").unwrap(), a, "a retry keeps its place");
    }

    /// A worker start sweeps the partial files of transfers that never finished once nothing
    /// wrote to them for a day: in the directory they went to and in the drop directory, whose
    /// emptied transfer directory goes too. A younger partial stays, listed for the next sweep;
    /// a finished transfer leaves nothing listed.
    #[test]
    fn a_sweep_removes_the_stale_partials_of_unfinished_transfers() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("cwd");
        let dest = Dest::SessionCwd(SessionId::new());
        let t = transfers(dir.path());
        let cut = |xfer: XferId, name: &str, age: Duration| {
            let target = t.target(xfer, name).unwrap();
            let mut rx = Receiving::open(&target, 0, 10).unwrap();
            rx.write(b"half").unwrap();
            let _held = rx.keep();
            let written = SystemTime::now().checked_sub(age).unwrap();
            File::options()
                .write(true)
                .open(partial_of(&target))
                .unwrap()
                .set_modified(written)
                .unwrap();
            partial_of(&target)
        };
        let day = STALE_PARTIAL;
        let (old, young, staged, done) =
            (XferId::new(), XferId::new(), XferId::new(), XferId::new());
        t.begin(old, &dest, cwd.to_str(), 2);
        t.begin(young, &dest, cwd.to_str(), 1);
        t.begin(staged, &Dest::Staging, None, 1);
        t.begin(done, &dest, cwd.to_str(), 1);
        let old_file = cut(old, "report.pdf", day.saturating_mul(2));
        let old_nested = cut(old, "proj/src/lib.rs", day.saturating_mul(2));
        let young_file = cut(young, "notes.txt", Duration::from_secs(60));
        let staged_file = cut(staged, "shot.png", day.saturating_mul(3));
        let landed = t.target(done, "whole.txt").unwrap();
        let mut rx = Receiving::open(&landed, 0, 4).unwrap();
        rx.write(b"four").unwrap();
        let whole = rx.finish(0, WallMs::ZERO).unwrap();
        assert!(t.landed(done, "whole.txt", whole).is_some());

        let fresh = transfers(dir.path());
        assert_eq!(fresh.sweep(day), 3);
        assert!(!old_file.exists() && !old_nested.exists() && !staged_file.exists());
        assert!(cwd.join("proj/src").is_dir(), "a directory the drop made in place stays");
        assert!(!dir.path().join("drop").join(staged.to_string()).exists());
        assert!(young_file.exists() && landed.exists());
        assert_eq!(fresh.sweep(day), 0, "the young one waits");
        assert_eq!(fresh.sweep(Duration::ZERO), 1);
        assert!(!young_file.exists());
        assert!(!dir.path().join("drop").join(LEDGER).exists(), "nothing left to list");
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

    /// A drag's files land in its own landing, where the worker's drag named them before they
    /// came, and the finish says whose drop they are; other transfers belong to no drag.
    #[test]
    fn a_drags_files_land_in_its_own_landing() {
        let dir = tempfile::tempdir().unwrap();
        let t = transfers(dir.path());
        let (xfer, drag) = (XferId::new(), DragId::new());
        t.begin(xfer, &Dest::Drag(drag), None, 1);
        assert_eq!(t.drag_of(xfer), Some(drag));
        let target = t.target(xfer, "shot.png").unwrap();
        assert_eq!(target, t.drag_dir(drag).join("shot.png"));
        assert_eq!(t.drag_dir(drag), dir.path().join("drop").join(drag.to_string()));
        let mut rx = Receiving::open(&target, 0, 3).unwrap();
        rx.write(b"png").unwrap();
        let landed = rx.finish(0o644, WallMs::ZERO).unwrap();
        let finished = t.landed(xfer, "shot.png", landed).unwrap();
        assert_eq!(finished, Finished { paths: vec![target], staging: false, drag: Some(drag) });
        assert_eq!(t.drag_of(xfer), None, "done and forgotten");
        let staged = XferId::new();
        t.begin(staged, &Dest::Staging, None, 1);
        assert_eq!(t.drag_of(staged), None);
    }

    /// A drag that ended with nothing landed takes its landing with it: the file that landed,
    /// the one still going up (stopped and forgotten) and an upload that begins after; another
    /// drag's landing stays.
    #[test]
    fn a_discarded_drags_landing_goes_and_takes_no_more() {
        let dir = tempfile::tempdir().unwrap();
        let t = transfers(dir.path());
        let (drag, other) = (DragId::new(), DragId::new());
        let (landed, going, kept) = (XferId::new(), XferId::new(), XferId::new());
        for (xfer, into) in [(landed, drag), (going, drag), (kept, other)] {
            t.begin(xfer, &Dest::Drag(into), None, 1);
        }
        let whole = t.target(landed, "a.txt").unwrap();
        let mut rx = Receiving::open(&whole, 0, 1).unwrap();
        rx.write(b"a").unwrap();
        let done = rx.finish(0, WallMs::ZERO).unwrap();
        assert!(t.landed(landed, "a.txt", done).is_some());
        let mut part = Receiving::open(&t.target(going, "b.bin").unwrap(), 0, 4).unwrap();
        part.write(b"bb").unwrap();
        let _held = part.keep();
        let mut cancelled = t.cancelled(going).unwrap();
        let theirs = t.target(kept, "c.txt").unwrap();
        let mut rx = Receiving::open(&theirs, 0, 2).unwrap();
        rx.write(b"c").unwrap();
        let _held = rx.keep();

        t.discard_drag(drag);
        assert!(!t.drag_dir(drag).exists(), "landed and partial alike");
        assert!(*cancelled.borrow_and_update(), "its stream stops");
        assert_eq!(t.drag_of(going), None, "forgotten");
        let late = XferId::new();
        t.begin(late, &Dest::Drag(drag), None, 1);
        assert!(matches!(t.target(late, "d.txt"), Err(XferError::Unknown)), "not begun");
        assert!(!t.drag_dir(drag).exists());
        assert_eq!(t.drag_of(kept), Some(other));
        assert!(partial_of(&theirs).exists(), "another drag's landing stays");
        t.discard_drag(drag);
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

    /// A retried download continues a held file only when the claim is of the version the file
    /// is now; a changed one, a claim past its end and an unheld file start over. Nothing of
    /// this worker's past is asked, so a claim holds after a restart.
    #[test]
    fn a_download_resumes_only_the_version_held() {
        let dir = tempfile::tempdir().unwrap();
        let file = |name: &str, size: u64, mtime_ms: u64| Outgoing {
            name: name.to_owned(),
            path: dir.path().join(name),
            size,
            mtime_ms: WallMs::from_millis(mtime_ms),
            mode: 0o644,
        };
        let held = |name: &str, bytes: u64, size: u64, mtime_ms: u64| Held {
            name: name.to_owned(),
            bytes,
            size,
            mtime_ms: WallMs::from_millis(mtime_ms),
        };
        let now = [file("a", 100, 1), file("b", 50, 2), file("c", 10, 1), file("d", 5, 1)];
        let claims = [held("a", 40, 100, 1), held("b", 50, 50, 1), held("c", 11, 10, 1)];
        assert_eq!(
            resume_points(&now, &claims),
            [40, 0, 0, 0],
            "the same version resumes; a newer one, a claim past the end, an unheld one do not"
        );
        assert_eq!(resume_points(&now, &[held("b", 50, 50, 2)]), [0, 50, 0, 0], "held whole");
        assert_eq!(resume_points(&now, &[held("a", 40, 99, 1)]), [0, 0, 0, 0], "another size");
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
