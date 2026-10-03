//! File transfer, the worker's half: where an upload lands, and writing it so that a cut never
//! leaves half a file under the real name.
//!
//! [`Transfers`] holds every transfer a client began ([`Transfers::begin`]) until its last file
//! landed. Each top-level entry of a drop (a file, or a directory and everything under it) goes
//! to the transfer's base directory under its own name, or, when that name is taken there, under
//! the next free one as Finder's "Keep Both" names it: `report 2.pdf`, `proj 2` ([`numbered`]).
//! The choice is made once per entry, on its first file. A name another transfer in flight
//! claimed there, or a partial file of it, counts as taken, so two drops of the same name never
//! share a partial file; a directory entry is made on the spot, so nothing else takes its name
//! meanwhile, and a file entry is renamed into place only where nothing is, so nothing is ever
//! written over. A file is written to `name.partial` ([`Receiving`]), synced, and renamed into
//! place, so a retried file resumes from the bytes the partial holds ([`durable`]).
//!
//! The entries of unfinished transfers are listed in a ledger in the drop directory, and
//! [`Transfers::sweep`] removes the partial files under them that nothing wrote to for
//! [`STALE_PARTIAL`]: an upload cut for good leaves nothing behind in the directory it went to.
//! A drag's landing that nothing landed from goes at once ([`Transfers::discard_drag`]).
//!
//! An upload outlives the link it began on: the client begins it again on its next link under
//! the same id and sends each file it has not heard landed from what [`Transfers::durable`]
//! says is held. A transfer is the daemon's, not a connection's, so the next link finds it as
//! it was; a restarted worker finds the places its entries went in the ledger. A file's later
//! stream takes it over from an earlier one still open on a link that went
//! ([`Transfers::claim`]), and one that already landed is said again rather than written
//! twice; a transfer that finished is remembered for a while ([`FINISHED_KEPT`]), so a client
//! that never heard its end hears it again.
//!
//! A download goes the other way, and resumes the same way: a retried fetch names the bytes the
//! client holds of each file and the version they are of, and [`resume_points`] sends a file from
//! there while it is still that version, from the start otherwise.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
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
/// transfer id, the entry's name as the client sent it and the path it lands at, per line.
const LEDGER: &str = ".partials";

/// Discarded drags remembered, so an upload into one that comes late is not begun: a drag's
/// uploads begin on the control stream, which nothing orders against its end on the stream's.
const DISCARDED: usize = 32;

/// The most names [`numbered`] tries for one entry before it gives up.
const NUMBERED_MOST: u32 = 10_000;

/// Finished transfers remembered, newest last: a client whose link went before it heard the
/// end begins the transfer again on its next link and is told the end again.
pub const FINISHED_KEPT: usize = 64;

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
    /// Every name [`numbered`] gives an entry, up to the ten thousandth, is taken.
    #[error("no free name for {0:?}")]
    Crowded(String),
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
    /// The top-level entries, in the order their first file arrived, where they landed.
    pub paths: Vec<PathBuf>,
    /// Their names as the client sent them, in the same order: a name taken where it went
    /// lands under another ([`numbered`]).
    pub names: Vec<String>,
    /// The transfer was for a streamed window: the paths go on the pasteboard.
    pub staging: bool,
    /// The drag whose drop the files are, for an upload into its landing.
    pub drag: Option<DragId>,
}

#[derive(Debug)]
struct Transfer {
    /// Where top-level entries go.
    base: PathBuf,
    staging: bool,
    drag: Option<DragId>,
    files: u32,
    /// Each top-level entry as the client named it, and where it lands, in first-sight order.
    entries: Vec<(String, PathBuf)>,
    landed: HashMap<String, Landed>,
    received: u64,
    reported: Option<Instant>,
    cancel: watch::Sender<bool>,
    /// The stream writing each file, one at a time ([`Transfers::claim`]).
    writers: HashMap<String, Slot>,
}

/// Who writes one file of a transfer: the lock its stream holds while it writes, and the turn
/// of the latest stream to claim it, which tells an earlier one to stop.
#[derive(Debug)]
struct Slot {
    lock: Arc<tokio::sync::Mutex<()>>,
    turn: watch::Sender<u64>,
}

/// One file of a transfer held by the stream writing it, until it drops.
#[derive(Debug)]
pub struct Claim {
    _held: tokio::sync::OwnedMutexGuard<()>,
    mine: u64,
    turn: watch::Receiver<u64>,
}

impl Claim {
    /// Resolves once a later stream claims the file: this one is to stop, keeping what it
    /// wrote. Never, once the transfer is gone.
    pub async fn superseded(&mut self) {
        let mine = self.mine;
        if self.turn.wait_for(|turn| *turn != mine).await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// A finished transfer, remembered ([`FINISHED_KEPT`]).
#[derive(Debug)]
struct Past {
    xfer: XferId,
    finished: Finished,
    landed: HashMap<String, Landed>,
}

/// What [`Transfers::begin`] made of a begin.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Begun {
    /// A transfer this worker did not know: new, or one a restart forgot, whose entries go
    /// where the ledger says they went.
    New,
    /// One in flight, begun again from the client's next link: it keeps what it has.
    Again,
    /// One that finished, its end not heard: it is told again.
    Finished(Finished),
    /// One into a discarded drag's landing: its files are refused.
    Refused,
}

/// A file that landed before its stream came: the stream writes nothing and the client is told
/// again.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Again {
    /// The file as it landed.
    pub landed: Landed,
    /// The transfer's end, when it has ended.
    pub finished: Option<Finished>,
}

impl Transfer {
    /// Where top-level entry `top` lands, once chosen.
    fn known_entry(&self, top: &str) -> Option<&PathBuf> {
        self.entries.iter().find(|(n, _entry)| n == top).map(|(_top, entry)| entry)
    }

    /// Whether this transfer put a top-level entry at `entry`.
    fn claims(&self, entry: &Path) -> bool {
        self.entries.iter().any(|(_top, mine)| mine == entry)
    }

    /// Record a landed file; `true` once every file has. A file entry that had to land under
    /// another name than the one chosen ([`Receiving::finish`]) is where it landed from now on.
    fn land(&mut self, name: &str, landed: Landed) -> bool {
        if let Some((_top, entry)) = self.entries.iter_mut().find(|(top, _entry)| top == name) {
            entry.clone_from(&landed.path);
        }
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
    /// The transfers that finished, newest last, up to [`FINISHED_KEPT`].
    past: Mutex<VecDeque<Past>>,
}

/// `name` as a path under its transfer's root ([`relative_path`]).
fn relative(name: &str) -> Result<PathBuf, XferError> {
    relative_path(name).ok_or_else(|| XferError::Name(name.to_owned()))
}

/// The `n`th name Finder's "Keep Both" gives `name` in a directory that holds it already:
/// `name` itself for 0, then `report 2.pdf`, `report 3.pdf`, and so on.
///
/// A name that ends in a number goes on from it (`report 2.pdf` gives `report 3.pdf`). A
/// directory, and a file with no extension (`Makefile`, `.zshrc`), is numbered at its end
/// (`proj 2`, `.zshrc 2`). A tarball keeps both its extensions (`archive 2.tar.gz`).
#[must_use]
pub fn numbered(name: &str, n: u32, dir: bool) -> String {
    if n == 0 {
        return name.to_owned();
    }
    let (stem, ext) = if dir { (name, "") } else { split_extension(name) };
    let (base, from) = match stem.rsplit_once(' ') {
        Some((base, digits))
            if !base.is_empty()
                && !digits.starts_with('0')
                && digits.bytes().all(|b| b.is_ascii_digit())
                && let Ok(from) = digits.parse::<u32>() =>
        {
            (base, from)
        }
        _ => (stem, 1),
    };
    format!("{base} {}{ext}", from.saturating_add(n))
}

/// `name` as its stem and its extension, dot included: none for a name whose only dot leads it
/// (`.zshrc`) or ends it, nor for one whose last part has a space (`v1. final draft`), and both
/// of a tarball's (`.tar.gz`).
fn split_extension(name: &str) -> (&str, &str) {
    let Some((stem, ext)) = name.rsplit_once('.') else { return (name, "") };
    if stem.is_empty() || ext.is_empty() || ext.contains(' ') {
        return (name, "");
    }
    let stem = match stem.rsplit_once('.') {
        Some((before, tar)) if !before.is_empty() && tar.eq_ignore_ascii_case("tar") => before,
        _ => stem,
    };
    name.split_at_checked(stem.len()).unwrap_or((name, ""))
}

/// Where top-level entry `top` lands in `base`: under the first of its [`numbered`] names that
/// nothing is at, no partial file is at, and no other transfer in flight `claimed`. A directory
/// entry is made there at once, so nothing takes its name before its files come; a file entry
/// is renamed into place only where nothing is ([`Receiving::keep_both`]).
fn choose(
    base: &Path,
    top: &str,
    dir: bool,
    claimed: impl Fn(&Path) -> bool,
) -> Result<PathBuf, XferError> {
    if dir {
        std::fs::create_dir_all(base)?;
    }
    for n in 0..NUMBERED_MOST {
        let entry = base.join(numbered(top, n, dir));
        let there = std::fs::symlink_metadata(&entry).is_ok()
            || std::fs::symlink_metadata(partial_of(&entry)).is_ok();
        if there || claimed(&entry) {
            continue;
        }
        if !dir {
            return Ok(entry);
        }
        #[expect(
            clippy::create_dir,
            reason = "made only if absent: the name is claimed in one step"
        )]
        let made = std::fs::create_dir(&entry);
        match made {
            Ok(()) => return Ok(entry),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    Err(XferError::Crowded(top.to_owned()))
}

/// Rename `from` to `to` unless something is at `to` (`AlreadyExists` then), in one step. A
/// file system that cannot (`renameat2` refused on some Linux ones) links the file there,
/// which fails the same way, and removes the old name.
fn rename_new(from: &Path, to: &Path) -> std::io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    use rustix::io::Errno;
    match renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE) {
        Ok(()) => Ok(()),
        Err(e) if [Errno::INVAL, Errno::NOSYS, Errno::NOTSUP].contains(&e) => {
            std::fs::hard_link(from, to)?;
            std::fs::remove_file(from)
        }
        Err(e) => Err(e.into()),
    }
}

/// Rename `partial` into place at `target`, or, when something took that name since it was
/// chosen, at the next [`numbered`] name nothing is at: never over anything. Returns where it
/// landed.
fn land_beside(partial: &Path, target: &Path) -> std::io::Result<PathBuf> {
    let name = target.file_name().and_then(|n| n.to_str());
    for n in 0..NUMBERED_MOST {
        let candidate = match (n, name) {
            (0, _) | (_, None) => target.to_path_buf(),
            (n, Some(name)) => target.with_file_name(numbered(name, n, false)),
        };
        if n > 0 && std::fs::symlink_metadata(partial_of(&candidate)).is_ok() {
            continue;
        }
        match rename_new(partial, &candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && name.is_some() => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free name to land under"))
}

/// One entry of an unfinished transfer, as the ledger lists it.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Listed {
    xfer: XferId,
    /// The top-level entry as the client named it.
    top: String,
    /// Where it lands.
    entry: PathBuf,
}

impl Listed {
    /// Its ledger line: `["<xfer>","<top>","<entry>"]` and a newline; `None` for a path that is
    /// not UTF-8.
    fn line(&self) -> Option<String> {
        let fields = (self.xfer.to_string(), &self.top, self.entry.to_str()?);
        let mut line = serde_json::to_string(&fields).ok()?;
        line.push('\n');
        Some(line)
    }

    /// The entry a ledger line lists; `None` for one that does not read.
    fn read(line: &str) -> Option<Self> {
        let (xfer, top, entry) = serde_json::from_str::<(String, String, PathBuf)>(line).ok()?;
        Some(Self { xfer: xfer.parse().ok()?, top, entry })
    }
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
    /// Transfers whose staged entries, and those with nowhere else to go, land under
    /// `drop_root/<xfer>/`.
    #[must_use]
    pub fn new(drop_root: PathBuf) -> Self {
        Self {
            drop_root,
            inner: Mutex::default(),
            begun: Notify::new(),
            ledger: Mutex::default(),
            discarded: Mutex::default(),
            past: Mutex::default(),
        }
    }

    /// `~/.slopty/drop`.
    #[must_use]
    pub fn default_drop_root() -> PathBuf {
        crate::file::expand_home(Path::new("~/.slopty/drop"))
    }

    /// A client begins transfer `xfer` of `files` files to `dest`. `cwd` is the session's
    /// directory for [`Dest::SessionCwd`] (`None` when it never said: the drop directory).
    ///
    /// A transfer known already (the client's next link begins it again) keeps what it has and
    /// is re-armed. One that finished is told again ([`Begun::Finished`]). One this worker
    /// does not know, but whose entries the ledger lists, was forgotten by a restart: its
    /// entries keep the places they went, so its partial files resume. One into a discarded
    /// drag's landing is not begun, so its files are refused.
    pub fn begin(&self, xfer: XferId, dest: &Dest, cwd: Option<&str>, files: u32) -> Begun {
        if let Dest::Drag(drag) = dest
            && self.discarded.lock().contains(drag)
        {
            tracing::debug!(%xfer, %drag, "an upload into a discarded drag");
            return Begun::Refused;
        }
        if let Some(past) = self.past.lock().iter().find(|p| p.xfer == xfer) {
            return Begun::Finished(past.finished.clone());
        }
        let known = self.inner.lock().get(&xfer).map(|t| t.cancel.send_replace(false)).is_some();
        if known {
            self.begun.notify_waiters();
            return Begun::Again;
        }
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
        let entries = self.ledger_entries(xfer);
        let transfer = Transfer {
            base: base.unwrap_or_else(|| self.drop_root.join(xfer.to_string())),
            staging,
            drag,
            files,
            entries,
            landed: HashMap::new(),
            received: 0,
            reported: None,
            cancel: watch::Sender::new(false),
            writers: HashMap::new(),
        };
        let begun = match self.inner.lock().entry(xfer) {
            std::collections::hash_map::Entry::Occupied(known) => {
                known.get().cancel.send_replace(false);
                Begun::Again
            }
            std::collections::hash_map::Entry::Vacant(new) => {
                new.insert(transfer);
                Begun::New
            }
        };
        self.begun.notify_waiters();
        begun
    }

    /// The top-level entries the ledger says `xfer` placed, each with where it lands, in the
    /// order they were placed: what a restart forgot of a transfer in flight.
    fn ledger_entries(&self, xfer: XferId) -> Vec<(String, PathBuf)> {
        let listed = {
            let _held = self.ledger.lock();
            self.read_ledger()
        };
        let mut entries: Vec<(String, PathBuf)> = Vec::new();
        for Listed { top, entry, .. } in listed.into_iter().filter(|l| l.xfer == xfer) {
            if !entries.iter().any(|(known, _entry)| *known == top) {
                entries.push((top, entry));
            }
        }
        entries
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

    /// Wait up to `wait` for `xfer` to begin; `false` when it did not. One that finished has
    /// begun.
    pub async fn begun(&self, xfer: XferId, wait: Duration) -> bool {
        let deadline = tokio::time::Instant::now().checked_add(wait);
        loop {
            let notified = self.begun.notified();
            let mut notified = std::pin::pin!(notified);
            notified.as_mut().enable();
            if self.inner.lock().contains_key(&xfer) || self.past(xfer).is_some() {
                return true;
            }
            let Some(deadline) = deadline else { return false };
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return false;
            }
        }
    }

    /// Where file `name` of `xfer` lands. The first file of a top-level entry decides for the
    /// whole entry: the base directory, under the entry's name or, when that is taken there,
    /// the next free one ([`numbered`]).
    ///
    /// # Errors
    ///
    /// [`XferError::Unknown`] when no such transfer is in flight, [`XferError::Name`] for a name
    /// that climbs out, and the disk's error when a directory entry cannot be made.
    pub fn target(&self, xfer: XferId, name: &str) -> Result<PathBuf, XferError> {
        let rel = relative(name)?;
        let top = name.split('/').next().unwrap_or(name);
        let inside: PathBuf = rel.components().skip(1).collect();
        let under = |entry: &Path| {
            if inside.as_os_str().is_empty() { entry.to_path_buf() } else { entry.join(&inside) }
        };
        let mut inner = self.inner.lock();
        let transfer = inner.get(&xfer).ok_or(XferError::Unknown)?;
        if let Some(entry) = transfer.known_entry(top) {
            return Ok(under(entry));
        }
        let base = transfer.base.clone();
        let theirs = |at: &Path| inner.iter().any(|(id, other)| *id != xfer && other.claims(at));
        let entry = choose(&base, top, name.contains('/'), theirs)?;
        let transfer = inner.get_mut(&xfer).ok_or(XferError::Unknown)?;
        transfer.entries.push((top.to_owned(), entry.clone()));
        drop(inner);
        self.record(&Listed { xfer, top: top.to_owned(), entry: entry.clone() });
        Ok(under(&entry))
    }

    /// Bytes of `name` the worker holds durably: all of them once it landed (in a transfer
    /// that finished too), else what its partial file holds.
    pub fn durable(&self, xfer: XferId, name: &str) -> Result<u64, XferError> {
        if let Some(again) = self.landed_before(xfer, name) {
            return Ok(again.landed.size);
        }
        Ok(durable(&self.target(xfer, name)?))
    }

    /// `name` of `xfer`, when it landed already: in the transfer in flight, or in one that
    /// finished, with its end.
    #[must_use]
    pub fn landed_before(&self, xfer: XferId, name: &str) -> Option<Again> {
        if let Some(landed) = self.inner.lock().get(&xfer).and_then(|t| t.landed.get(name)) {
            return Some(Again { landed: landed.clone(), finished: None });
        }
        self.past.lock().iter().find(|p| p.xfer == xfer).and_then(|past| {
            let landed = past.landed.get(name)?.clone();
            Some(Again { landed, finished: Some(past.finished.clone()) })
        })
    }

    /// The end of `xfer`, when it finished and is remembered.
    fn past(&self, xfer: XferId) -> Option<Finished> {
        self.past.lock().iter().find(|p| p.xfer == xfer).map(|p| p.finished.clone())
    }

    /// Take file `name` of `xfer` for a stream to write: a stream still writing it (on a link
    /// the client has since left) is told to stop ([`Claim::superseded`]), and this one waits
    /// until it has, so two never write one partial file. The latest claim wins.
    ///
    /// # Errors
    ///
    /// [`XferError::Unknown`] when no such transfer is in flight.
    pub async fn claim(&self, xfer: XferId, name: &str) -> Result<Claim, XferError> {
        let (lock, turn, mine) = {
            let mut inner = self.inner.lock();
            let transfer = inner.get_mut(&xfer).ok_or(XferError::Unknown)?;
            let slot = transfer
                .writers
                .entry(name.to_owned())
                .or_insert_with(|| Slot { lock: Arc::default(), turn: watch::Sender::new(0) });
            let mut mine = 0;
            slot.turn.send_modify(|turn| {
                *turn = turn.wrapping_add(1);
                mine = *turn;
            });
            (Arc::clone(&slot.lock), slot.turn.subscribe(), mine)
        };
        let held = lock.lock_owned().await;
        Ok(Claim { _held: held, mine, turn })
    }

    /// `name` of `xfer` is whole and in place; once every file is, the transfer is done: it
    /// leaves the transfers in flight and is remembered as finished ([`FINISHED_KEPT`]).
    pub fn landed(&self, xfer: XferId, name: &str, landed: Landed) -> Option<Finished> {
        if !self.inner.lock().get_mut(&xfer)?.land(name, landed) {
            return None;
        }
        let t = self.inner.lock().remove(&xfer)?;
        self.unrecord(xfer);
        let (names, paths) = t.entries.iter().cloned().unzip();
        let finished = Finished { paths, names, staging: t.staging, drag: t.drag };
        let mut past = self.past.lock();
        if past.len() >= FINISHED_KEPT {
            past.pop_front();
        }
        past.push_back(Past { xfer, finished: finished.clone(), landed: t.landed });
        drop(past);
        Some(finished)
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
        let mut kept: Vec<Listed> = Vec::new();
        for line in &listed {
            let entry = &line.entry;
            let mut young = false;
            let mut partials = Vec::new();
            partials_under(entry, &mut partials);
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
            if young && !kept.contains(line) {
                kept.push(line.clone());
            }
        }
        let _held = self.ledger.lock();
        let appended: Vec<Listed> =
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

    /// List an entry in the ledger, so a sweep finds its partial files if its transfer never
    /// finishes, and a restart where it went. A path that is not UTF-8 is not listed.
    fn record(&self, listed: &Listed) {
        use std::io::Write as _;
        let Some(line) = listed.line() else { return };
        let _held = self.ledger.lock();
        let appended = std::fs::create_dir_all(&self.drop_root).and_then(|()| {
            let mut file = File::options().create(true).append(true).open(self.ledger_path())?;
            file.write_all(line.as_bytes())
        });
        if let Err(e) = appended {
            let (xfer, entry) = (listed.xfer, listed.entry.display());
            tracing::warn!(%xfer, %entry, error = %e, "partial ledger");
        }
    }

    /// Drop `xfer`'s entries from the ledger: every file of it landed.
    fn unrecord(&self, xfer: XferId) {
        let _held = self.ledger.lock();
        let rest: Vec<Listed> =
            self.read_ledger().into_iter().filter(|listed| listed.xfer != xfer).collect();
        self.write_ledger(&rest);
    }

    /// The ledger's entries; a line that does not read is skipped.
    fn read_ledger(&self) -> Vec<Listed> {
        let Ok(text) = std::fs::read_to_string(self.ledger_path()) else { return Vec::new() };
        text.lines().filter_map(Listed::read).collect()
    }

    /// Replace the ledger with `entries` (`slopty_platform::fs::replace`), so a crash leaves the
    /// old list or the new. No entries removes it.
    fn write_ledger(&self, entries: &[Listed]) {
        let path = self.ledger_path();
        if entries.is_empty() {
            let _absent = std::fs::remove_file(&path);
            return;
        }
        let text: String = entries.iter().filter_map(Listed::line).collect();
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
    /// Land beside whatever took the target's name since it was chosen, never over it.
    keep_both: bool,
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
        Ok(Self { file, target: target.to_path_buf(), hasher, at: offset, size, keep_both: false })
    }

    /// Land beside whatever took the target's name since it was chosen, under the next free
    /// [`numbered`] name, rather than over it: for a top-level file entry, whose name was free
    /// when it was chosen, as a file inside an entry's own directory need not be.
    #[must_use]
    pub const fn keep_both(mut self) -> Self {
        self.keep_both = true;
        self
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
    /// bytes to the drive and rename it into place, beside a file that took its name meanwhile
    /// when it is to keep both ([`Self::keep_both`]). The landing says where it went.
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
        let partial = partial_of(&self.target);
        let path = if self.keep_both {
            rustix::fs::fsync(&self.file).map_err(std::io::Error::from)?;
            land_beside(&partial, &self.target)?
        } else {
            land(&self.file, &partial, &self.target)?;
            self.target
        };
        Ok(Landed { path, size: self.size, hash: self.hasher.finalize().into() })
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

    /// Finder's "Keep Both" names: a number before the extension, going on from one the name
    /// ends in; a directory and a name with no extension numbered at the end; a tarball's two
    /// extensions kept together.
    #[test]
    fn a_taken_name_is_numbered_as_finder_numbers_it() {
        let cases = [
            ("report.pdf", false, "report 2.pdf"),
            ("report 2.pdf", false, "report 3.pdf"),
            ("report 02.pdf", false, "report 02 2.pdf"),
            ("archive.tar.gz", false, "archive 2.tar.gz"),
            ("my.notes.v1.md", false, "my.notes.v1 2.md"),
            (".zshrc", false, ".zshrc 2"),
            (".env.local", false, ".env 2.local"),
            ("Makefile", false, "Makefile 2"),
            ("a.", false, "a. 2"),
            ("draft. final copy", false, "draft. final copy 2"),
            ("proj.v2", true, "proj.v2 2"),
            ("proj 9", true, "proj 10"),
            ("2024", false, "2024 2"),
        ];
        for (name, dir, second) in cases {
            assert_eq!(numbered(name, 0, dir), name, "the name itself first");
            assert_eq!(numbered(name, 1, dir), second, "{name}");
        }
        assert_eq!(numbered("report.pdf", 3, false), "report 4.pdf");
    }

    /// Entries land in the session's directory; one whose name is taken there lands under the
    /// next free name, the rest of its entry with it, and the transfer finishes with the paths
    /// they landed at once every file is in.
    #[test]
    fn a_clash_lands_under_the_next_free_name_and_finish_names_the_entries() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("cwd");
        std::fs::create_dir_all(cwd.join("taken")).unwrap();
        std::fs::write(cwd.join("a.txt"), "theirs").unwrap();
        std::fs::write(cwd.join("a 2.txt"), "theirs too").unwrap();
        let t = transfers(dir.path());
        let xfer = XferId::new();
        t.begin(xfer, &Dest::SessionCwd(SessionId::new()), cwd.to_str(), 4);
        assert_eq!(t.target(xfer, "new.txt").unwrap(), cwd.join("new.txt"));
        assert_eq!(t.target(xfer, "a.txt").unwrap(), cwd.join("a 3.txt"), "down the chain");
        assert_eq!(t.target(xfer, "taken/a").unwrap(), cwd.join("taken 2/a"));
        assert!(cwd.join("taken 2").is_dir(), "a directory entry is made at once");
        assert_eq!(t.target(xfer, "taken/b").unwrap(), cwd.join("taken 2/b"), "per entry");
        assert!(matches!(t.target(XferId::new(), "x"), Err(XferError::Unknown)));

        let land = |path: PathBuf| Landed { path, size: 1, hash: [0; 32] };
        assert_eq!(t.landed(xfer, "new.txt", land(cwd.join("new.txt"))), None);
        assert_eq!(t.durable(xfer, "new.txt").unwrap(), 1, "a landed file is all there");
        assert_eq!(t.landed(xfer, "a.txt", land(cwd.join("a 3.txt"))), None);
        assert_eq!(t.landed(xfer, "taken/a", land(cwd.join("taken 2/a"))), None);
        let done = t.landed(xfer, "taken/b", land(cwd.join("taken 2/b"))).unwrap();
        assert_eq!(done.paths, [cwd.join("new.txt"), cwd.join("a 3.txt"), cwd.join("taken 2")]);
        assert_eq!(done.names, ["new.txt", "a.txt", "taken"], "as the client named them");
        assert!(!done.staging);
        assert_eq!(std::fs::read_to_string(cwd.join("a.txt")).unwrap(), "theirs", "untouched");
        assert!(matches!(t.target(xfer, "x"), Err(XferError::Unknown)), "forgotten once done");
    }

    /// A file entry whose name something took after it was chosen lands beside it under the
    /// next free name, never over it, and the transfer's end names where it went. A file inside
    /// an entry's own directory lands under its name.
    #[test]
    fn a_name_taken_while_the_file_came_is_kept_and_the_file_lands_beside_it() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("cwd");
        let t = transfers(dir.path());
        let xfer = XferId::new();
        t.begin(xfer, &Dest::Path(cwd.to_string_lossy().into_owned()), None, 1);
        let target = t.target(xfer, "notes.md").unwrap();
        assert_eq!(target, cwd.join("notes.md"));
        let mut rx = Receiving::open(&target, 0, 4).unwrap().keep_both();
        rx.write(b"ours").unwrap();
        std::fs::write(cwd.join("notes.md"), "made meanwhile").unwrap();
        let landed = rx.finish(0, WallMs::ZERO).unwrap();
        assert_eq!(landed.path, cwd.join("notes 2.md"));
        assert_eq!(std::fs::read_to_string(cwd.join("notes.md")).unwrap(), "made meanwhile");
        assert_eq!(std::fs::read_to_string(cwd.join("notes 2.md")).unwrap(), "ours");
        assert!(!partial_of(&target).exists());
        let done = t.landed(xfer, "notes.md", landed).unwrap();
        assert_eq!(done.paths, [cwd.join("notes 2.md")], "where it went");

        let inner = cwd.join("proj/a.txt");
        let mut rx = Receiving::open(&inner, 0, 1).unwrap();
        rx.write(b"b").unwrap();
        std::fs::write(&inner, "a").unwrap();
        assert_eq!(rx.finish(0, WallMs::ZERO).unwrap().path, inner);
        assert_eq!(std::fs::read_to_string(&inner).unwrap(), "b");
    }

    /// A cut upload of a renamed entry resumes into the name it was given, not the next one:
    /// on the same transfer begun again, and after a restart, from the ledger.
    #[test]
    fn a_renamed_entry_resumes_into_the_name_it_was_given() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("cwd");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(cwd.join("big.bin"), "theirs").unwrap();
        let dest = Dest::SessionCwd(SessionId::new());
        let xfer = XferId::new();
        let body: Vec<u8> = (0..50_000_u32).map(|i| (i % 253) as u8).collect();
        let size = body.len() as u64;
        let before = transfers(dir.path());
        before.begin(xfer, &dest, cwd.to_str(), 1);
        let target = before.target(xfer, "big.bin").unwrap();
        assert_eq!(target, cwd.join("big 2.bin"));
        let mut rx = Receiving::open(&target, 0, size).unwrap().keep_both();
        rx.write(&body[..20_000]).unwrap();
        assert_eq!(rx.keep(), 20_000);
        assert_eq!(before.begin(xfer, &dest, cwd.to_str(), 1), Begun::Again);
        assert_eq!(before.target(xfer, "big.bin").unwrap(), target, "begun again: the same");
        drop(before);

        let after = transfers(dir.path());
        assert_eq!(after.begin(xfer, &dest, cwd.to_str(), 1), Begun::New);
        assert_eq!(after.target(xfer, "big.bin").unwrap(), target, "a restart: the same");
        assert_eq!(after.durable(xfer, "big.bin").unwrap(), 20_000);
        let mut rx = Receiving::open(&target, 20_000, size).unwrap().keep_both();
        rx.write(&body[20_000..]).unwrap();
        let landed = rx.finish(0, WallMs::ZERO).unwrap();
        assert_eq!(landed.hash, *blake3::hash(&body).as_bytes(), "the digest of the whole");
        assert_eq!(landed.path, target);
        assert_eq!(after.landed(xfer, "big.bin", landed).unwrap().paths, [&*target]);
        assert_eq!(std::fs::read(&target).unwrap(), body);
        assert_eq!(std::fs::read_to_string(cwd.join("big.bin")).unwrap(), "theirs");
    }

    /// A drop into a folder nothing may write to fails in the disk's words: a directory entry
    /// when its directory is made, a file when its partial is.
    #[test]
    fn a_drop_into_a_folder_that_takes_no_writes_fails_in_the_disks_words() {
        use std::io::ErrorKind::PermissionDenied;
        let dir = tempfile::tempdir().unwrap();
        let shut = dir.path().join("shut");
        std::fs::create_dir_all(&shut).unwrap();
        std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o555)).unwrap();
        let t = transfers(dir.path());
        let xfer = XferId::new();
        t.begin(xfer, &Dest::Path(shut.to_string_lossy().into_owned()), None, 2);
        let denied =
            |e: &XferError| matches!(e, XferError::Io(io) if io.kind() == PermissionDenied);
        let refused = t.target(xfer, "proj/a.txt");
        assert!(refused.as_ref().is_err_and(denied), "{refused:?}");
        let file = t.target(xfer, "a.txt").unwrap();
        let opened = Receiving::open(&file, 0, 1);
        assert!(opened.as_ref().is_err_and(denied), "{opened:?}");
        std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Two drops of one name into one directory at once: the second takes the next name, so
    /// their partial files are two, though nothing is on disk under the first's name yet. A
    /// partial file a transfer left there counts as taken too, for a restarted worker.
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
        assert_eq!(b, cwd.join("a 2.txt"));
        assert_ne!(partial_of(&a), partial_of(&b));
        assert_eq!(t.target(first, "a.txt").unwrap(), a, "a retry keeps its place");
        let mut rx = Receiving::open(&b, 0, 2).unwrap();
        rx.write(b"b").unwrap();
        let _held = rx.keep();
        drop(t);

        let fresh = transfers(dir.path());
        let third = XferId::new();
        fresh.begin(third, &dest, cwd.to_str(), 1);
        assert_eq!(fresh.target(third, "a.txt").unwrap(), cwd.join("a.txt"), "nothing there");
        let fourth = XferId::new();
        fresh.begin(fourth, &dest, cwd.to_str(), 1);
        assert_eq!(
            fresh.target(fourth, "a.txt").unwrap(),
            cwd.join("a 3.txt"),
            "a partial file another transfer left counts as taken"
        );
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
        let names = vec!["shot.png".to_owned()];
        let whole = Finished { paths: vec![target], names, staging: false, drag: Some(drag) };
        assert_eq!(finished, whole);
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

    /// A transfer begun again from the client's next link keeps what it has; one that finished
    /// is remembered, so a client that never heard the end is told it again, and what it asks
    /// of a landed file is that it is all there.
    #[test]
    fn a_transfer_begun_again_keeps_what_it_has_and_one_finished_is_told_again() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("cwd");
        let t = transfers(dir.path());
        let xfer = XferId::new();
        let dest = Dest::SessionCwd(SessionId::new());
        assert_eq!(t.begin(xfer, &dest, cwd.to_str(), 2), Begun::New);
        let land = |name: &str| Landed { path: cwd.join(name), size: 3, hash: [1; 32] };
        assert_eq!(t.target(xfer, "a.txt").unwrap(), cwd.join("a.txt"));
        assert_eq!(t.landed(xfer, "a.txt", land("a.txt")), None);
        assert_eq!(t.begin(xfer, &dest, cwd.to_str(), 1), Begun::Again, "the next link's begin");
        assert_eq!(t.durable(xfer, "a.txt").unwrap(), 3, "landed: all there");
        let again = t.landed_before(xfer, "a.txt").expect("landed before");
        assert_eq!((again.landed, again.finished), (land("a.txt"), None));
        assert_eq!(t.target(xfer, "b.txt").unwrap(), cwd.join("b.txt"));
        let finished = t.landed(xfer, "b.txt", land("b.txt")).expect("both in: finished");
        assert_eq!(finished.paths, [cwd.join("a.txt"), cwd.join("b.txt")]);

        assert_eq!(t.begin(xfer, &dest, cwd.to_str(), 0), Begun::Finished(finished.clone()));
        assert_eq!(t.durable(xfer, "b.txt").unwrap(), 3);
        let again = t.landed_before(xfer, "b.txt").expect("remembered");
        assert_eq!(again.finished, Some(finished), "with the transfer's end");
        assert!(t.landed_before(xfer, "c.txt").is_none());
    }

    /// A restarted worker forgot a transfer in flight; begun again, its entries go where the
    /// ledger says they went, though the directory is there now, so its partial file resumes
    /// rather than starting over under another name.
    #[test]
    fn a_restart_finds_where_a_transfers_entries_went() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("cwd");
        let xfer = XferId::new();
        let dest = Dest::SessionCwd(SessionId::new());
        let before = transfers(dir.path());
        before.begin(xfer, &dest, cwd.to_str(), 2);
        let first = before.target(xfer, "proj/a.bin").unwrap();
        assert_eq!(first, cwd.join("proj/a.bin"));
        let mut rx = Receiving::open(&first, 0, 100_000).unwrap();
        rx.write(&vec![9; 40_000]).unwrap();
        assert_eq!(rx.keep(), 40_000);
        drop(before);

        let after = transfers(dir.path());
        assert!(cwd.join("proj").is_dir(), "its directory is there now");
        assert_eq!(after.begin(xfer, &dest, cwd.to_str(), 2), Begun::New);
        assert_eq!(after.target(xfer, "proj/a.bin").unwrap(), first, "where it went");
        assert_eq!(after.target(xfer, "proj/b.bin").unwrap(), cwd.join("proj/b.bin"));
        assert_eq!(after.durable(xfer, "proj/a.bin").unwrap(), 40_000, "what it held");
        let other = XferId::new();
        after.begin(other, &dest, cwd.to_str(), 1);
        let clash = cwd.join("proj 2/a.bin");
        assert_eq!(after.target(other, "proj/a.bin").unwrap(), clash, "another drop's is apart");
    }

    /// A later stream of a file takes it over: the earlier one is told to stop and the later
    /// one waits until it has let go, so the two never write one partial file.
    #[tokio::test]
    async fn a_later_stream_of_a_file_takes_it_over() {
        let dir = tempfile::tempdir().unwrap();
        let t = Arc::new(transfers(dir.path()));
        let xfer = XferId::new();
        t.begin(xfer, &Dest::Staging, None, 1);
        let mut earlier = t.claim(xfer, "a").await.unwrap();
        let other = t.claim(xfer, "b").await.unwrap();
        let later = tokio::spawn({
            let t = Arc::clone(&t);
            async move { t.claim(xfer, "a").await.map(|_claim| ()) }
        });
        tokio::time::timeout(Duration::from_secs(5), earlier.superseded())
            .await
            .expect("the earlier stream is told");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!later.is_finished(), "the later one waits for the earlier to let go");
        drop(earlier);
        tokio::time::timeout(Duration::from_secs(5), later).await.unwrap().unwrap().unwrap();
        drop(other);
        assert!(matches!(t.claim(XferId::new(), "a").await, Err(XferError::Unknown)));
    }

    #[tokio::test]
    async fn a_stream_that_overtakes_its_begin_waits_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let t = Arc::new(transfers(dir.path()));
        let xfer = XferId::new();
        assert!(!t.begun(xfer, Duration::from_millis(20)).await, "never begun");
        let waiter = tokio::spawn({
            let t = Arc::clone(&t);
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
