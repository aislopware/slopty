//! A repository read for review: its working tree as a tree, the files that differ between two
//! trees cut into hunks, and the blobs either side names.
//!
//! The working tree is read as `git add -A` sees it (untracked files in, ignored ones out),
//! written as a tree through a scratch copy of the person's index ([`working_tree`]), so their
//! index, `HEAD` and refs are never touched and only what changed since git last looked is
//! hashed. It is compared against `HEAD` or the branch's base; a range of commits is read from
//! the object store alone ([`range`]). No snapshot is taken of an agent's turn
//! (`docs/decisions/agents.md`, "TUI-first").
//!
//! Hunks are cut here, with three lines of context, the same way every time.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use similar::{Algorithm, DiffOp, DiffTag};
use slopty_proto::git::BLOB_MAX;
use slopty_proto::thread::detail::{Hunk, heading};
use slopty_proto::thread::wire::{
    Against, FileDiff, FileKind, Modes, Review, ReviewScope, reading_order,
};
use slopty_proto::thread::{Patch, TreeRef};
use tokio::io::AsyncWriteExt as _;

/// Lines of context around each change in a hunk.
pub const CONTEXT: usize = 3;

/// The most diff lines a review carries in all; the files past it come without hunks.
pub const REVIEW_LINES: u32 = 20_000;

/// The largest file cut into hunks; a larger one is shown as changed, with no hunks.
pub const TEXT_BYTES: usize = 4 << 20;

/// How long one git run may take.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(60);

/// Something git would not do, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failed(pub String);

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Failed {}

/// One repository read for review: its root, the git that runs on it, and the index its working
/// tree is written through.
#[derive(Clone, Debug)]
pub struct Repo {
    /// The git to run.
    pub git: PathBuf,
    /// The repository's root.
    pub root: PathBuf,
    /// The index the working tree is written through, made on the first take.
    pub index: PathBuf,
}

impl Repo {
    async fn run(
        &self,
        args: &[&str],
        index: Option<&Path>,
        input: Option<&[u8]>,
    ) -> Result<Vec<u8>, Failed> {
        let mut command = tokio::process::Command::new(&self.git);
        command
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(index) = index {
            command.env("GIT_INDEX_FILE", index);
        }
        let run = async {
            let mut child = command.spawn().map_err(|e| Failed(format!("git: {e}")))?;
            if let (Some(bytes), Some(mut stdin)) = (input, child.stdin.take()) {
                stdin.write_all(bytes).await.map_err(|e| Failed(format!("git: {e}")))?;
            }
            child.wait_with_output().await.map_err(|e| Failed(format!("git: {e}")))
        };
        let out = tokio::time::timeout(GIT_TIMEOUT, run).await.map_err(|_elapsed| {
            Failed(format!("git {} took too long", args.first().unwrap_or(&"")))
        })??;
        if !out.status.success() {
            let why = String::from_utf8_lossy(&out.stderr);
            return Err(Failed(format!("git {}: {}", args.join(" "), why.trim())));
        }
        Ok(out.stdout)
    }

    async fn line(&self, args: &[&str], index: Option<&Path>) -> Result<String, Failed> {
        let out = self.run(args, index, None).await?;
        Ok(String::from_utf8_lossy(&out).trim().to_owned())
    }

    /// The tree `against` names: `HEAD`'s, the empty tree on an unborn branch; or that of where
    /// the branch left its base, the first of `origin`'s default branch, `origin/main`,
    /// `origin/master`, `main` and `master` that shares history with it; or where it left the
    /// branch [`Against::Branch`] names. `None` when no base does.
    ///
    /// # Errors
    ///
    /// When git fails.
    pub async fn against(&self, against: &Against) -> Result<Option<TreeRef>, Failed> {
        let verify = async |rev: &str| {
            let spec = format!("{rev}^{{commit}}");
            self.line(&["rev-parse", "-q", "--verify", "--end-of-options", &spec], None).await.ok()
        };
        let commit = match against {
            Against::Head => verify("HEAD").await,
            Against::Base => {
                if verify("HEAD").await.is_none() {
                    return Ok(None);
                }
                let mut found = None;
                for base in BASES {
                    let Some(base) = verify(base).await else { continue };
                    if let Ok(fork) = self.line(&["merge-base", "HEAD", &base], None).await {
                        found = Some(fork);
                        break;
                    }
                }
                let Some(fork) = found else { return Ok(None) };
                Some(fork)
            }
            Against::Branch(branch) => {
                if verify("HEAD").await.is_none() {
                    return Ok(None);
                }
                let (remote, local) =
                    (verify(&format!("origin/{branch}")).await, verify(branch).await);
                let base = match (remote, local) {
                    (Some(remote), Some(local)) => {
                        let holds = ["merge-base", "--is-ancestor", &local, &remote];
                        if self.run(&holds, None, None).await.is_ok() { remote } else { local }
                    }
                    (Some(one), None) | (None, Some(one)) => one,
                    (None, None) => return Ok(None),
                };
                let Ok(fork) = self.line(&["merge-base", "HEAD", &base], None).await else {
                    return Ok(None);
                };
                Some(fork)
            }
        };
        let tree = if let Some(commit) = commit {
            self.line(&["rev-parse", &format!("{commit}^{{tree}}")], None).await?
        } else {
            let empty = self.run(&["hash-object", "-t", "tree", "--stdin"], None, Some(b""));
            String::from_utf8_lossy(&empty.await?).trim().to_owned()
        };
        Ok(Some(TreeRef(tree)))
    }

    /// The commit the branch `name` stands at: `origin`'s copy when it holds every commit of
    /// the local one, else the local one. `None` when neither is.
    async fn branch(&self, name: &str) -> Option<String> {
        let verify = async |rev: &str| {
            let spec = format!("{rev}^{{commit}}");
            self.line(&["rev-parse", "-q", "--verify", "--end-of-options", &spec], None).await.ok()
        };
        let remote = verify(&format!("refs/remotes/origin/{name}")).await;
        let local = verify(&format!("refs/heads/{name}")).await;
        match (remote, local) {
            (Some(remote), Some(local)) => {
                let holds = ["merge-base", "--is-ancestor", &local, &remote];
                Some(if self.run(&holds, None, None).await.is_ok() { remote } else { local })
            }
            (Some(one), None) | (None, Some(one)) => Some(one),
            (None, None) => None,
        }
    }

    /// The working tree as a tree, through [`Self::index`].
    ///
    /// # Errors
    ///
    /// When git fails.
    pub async fn take(&self) -> Result<TreeRef, Failed> {
        let index = Some(self.index.as_path());
        if !self.index.exists() {
            if let Some(dir) = self.index.parent() {
                tokio::fs::create_dir_all(dir)
                    .await
                    .map_err(|e| Failed(format!("{}: {e}", dir.display())))?;
            }
            let head = self.run(&["rev-parse", "-q", "--verify", "HEAD^{tree}"], None, None).await;
            match head {
                Ok(_) => self.run(&["read-tree", "HEAD"], index, None).await?,
                Err(_unborn) => self.run(&["read-tree", "--empty"], index, None).await?,
            };
        }
        self.run(&["add", "-A", "--", "."], index, None).await?;
        Ok(TreeRef(self.line(&["write-tree"], index).await?))
    }

    /// Every file that differs from `from` to `to`, by path, cut into hunks while the review
    /// has room for them ([`REVIEW_LINES`]), spent in the order the files are read
    /// ([`reading_order`]).
    ///
    /// # Errors
    ///
    /// When git fails.
    ///
    /// A file moved with its content mostly kept is one rename (`-M`), its old path named. Its
    /// sides are read in one `git cat-file --batch` for the whole review, and only a text side
    /// small enough to cut is held whole: a picture or a larger file says its size instead.
    pub async fn changes(&self, from: &TreeRef, to: &TreeRef) -> Result<Vec<FileDiff>, Failed> {
        self.changes_with(from, to, false).await
    }

    /// [`Repo::changes`], whitespace not counting when `ignore_whitespace`, as `git diff -w`
    /// reads it: a line that differs only in spaces is no change, and a file whose content
    /// changed only so is left out (one added, removed, renamed or given a new mode stays).
    ///
    /// # Errors
    ///
    /// When git fails.
    pub async fn changes_with(
        &self,
        from: &TreeRef,
        to: &TreeRef,
        ignore_whitespace: bool,
    ) -> Result<Vec<FileDiff>, Failed> {
        let raw = self.run(&["diff-tree", "-r", "-z", "-M", &from.0, &to.0], None, None).await?;
        let entries = parse_raw(&raw);
        let ids: Vec<&str> = entries
            .iter()
            .flat_map(|e| e.from.iter().chain(e.to.iter()))
            .map(String::as_str)
            .collect();
        let read = self.read_blobs(&ids).await?;
        let none = Read::default();
        let side =
            |id: &Option<String>| id.as_ref().map_or(&none, |id| read.get(id).unwrap_or(&none));
        let mut files = Vec::new();
        for entry in entries {
            let (old, new) = (side(&entry.from), side(&entry.to));
            let bytes = old.size.max(new.size);
            let (kind, patch) = match (old.text(), new.text()) {
                _ if is_picture(&entry.path) => (FileKind::Image { bytes }, empty_patch()),
                (Some(old), Some(new)) => {
                    (FileKind::Text, diff_with(&lossy(old), &lossy(new), ignore_whitespace))
                }
                _ if old.binary || new.binary => (FileKind::Binary, empty_patch()),
                _ => (FileKind::TooLarge { bytes }, empty_patch()),
            };
            let modes = entry.modes.filter(|m| m.from != 0 && m.to != 0 && m.from != m.to);
            let only_spaces = ignore_whitespace
                && kind == FileKind::Text
                && patch.hunks.is_empty()
                && entry.from.is_some()
                && entry.to.is_some()
                && entry.old_path.is_none()
                && modes.is_none();
            if only_spaces {
                continue;
            }
            files.push(FileDiff {
                path: entry.path,
                old_path: entry.old_path,
                from: entry.from,
                to: entry.to,
                kind,
                modes,
                patch,
            });
        }
        spend(&mut files, REVIEW_LINES);
        Ok(files)
    }

    /// The blobs `ids` names, read by one `git cat-file --batch`: each one's size, whether its
    /// head holds a NUL (git's own test for binary), and its bytes when it is text no larger
    /// than [`TEXT_BYTES`]. A larger blob streams past without being held.
    async fn read_blobs(&self, ids: &[&str]) -> Result<HashMap<String, Read>, Failed> {
        let mut unique: Vec<&str> = ids.to_vec();
        unique.sort_unstable();
        unique.dedup();
        if unique.is_empty() {
            return Ok(HashMap::new());
        }
        let mut command = tokio::process::Command::new(&self.git);
        command
            .arg("-C")
            .arg(&self.root)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut asked = unique.join("\n");
        asked.push('\n');
        let run = async {
            let mut child = command.spawn().map_err(|e| Failed(format!("git: {e}")))?;
            let mut stdin = child.stdin.take();
            let feed = async move {
                if let Some(stdin) = stdin.as_mut() {
                    stdin.write_all(asked.as_bytes()).await?;
                }
                drop(stdin);
                Ok::<(), std::io::Error>(())
            };
            let stdout = child.stdout.take().ok_or_else(|| Failed("git: no output".to_owned()))?;
            let (fed, read) = tokio::join!(feed, batch(stdout, unique.len()));
            fed.map_err(|e| Failed(format!("git cat-file: {e}")))?;
            let read = read.map_err(|e| Failed(format!("git cat-file: {e}")))?;
            let status = child.wait().await.map_err(|e| Failed(format!("git: {e}")))?;
            if !status.success() {
                return Err(Failed(format!("git cat-file --batch ended {status}")));
            }
            Ok(read)
        };
        tokio::time::timeout(GIT_TIMEOUT, run)
            .await
            .map_err(|_elapsed| Failed("git cat-file took too long".to_owned()))?
    }

    /// One file's hunks whole, from blob `from` to blob `to` (none for a side the file lacks),
    /// and whether a side is not text: binary, or past [`TEXT_BYTES`], which has no hunks.
    async fn patch(
        &self,
        from: Option<&str>,
        to: Option<&str>,
        ignore_whitespace: bool,
    ) -> Result<(bool, Patch), Failed> {
        let old = self.blob(from).await?;
        let new = self.blob(to).await?;
        if !is_text(&old) || !is_text(&new) {
            return Ok((true, empty_patch()));
        }
        Ok((false, diff_with(&lossy(&old), &lossy(&new), ignore_whitespace)))
    }

    /// The bytes of the blob a review named on one side of a file ([`FileDiff::from`],
    /// [`FileDiff::to`]): the whole file there, for the lines between its hunks.
    ///
    /// # Errors
    ///
    /// When `id` is not an object id, or git has no such blob.
    pub async fn object(&self, id: &str) -> Result<Vec<u8>, Failed> {
        object_id(id)?;
        self.blob(Some(id)).await
    }

    /// The bytes of blob `id`, empty for none.
    async fn blob(&self, id: Option<&str>) -> Result<Vec<u8>, Failed> {
        match id {
            Some(id) => self.run(&["cat-file", "blob", id], None, None).await,
            None => Ok(Vec::new()),
        }
    }
}

/// The branches a working tree's base is looked for on, in order ([`Repo::against`]).
const BASES: [&str; 5] = ["origin/HEAD", "origin/main", "origin/master", "main", "master"];

/// Why a working tree's review against its base has nothing to compare.
pub const NO_BASE: &str = "This branch has no base branch to compare with";

/// Give the scratch `copy` of the index at `own` the original's modification time.
///
/// Git trusts an entry whose stat matches the file unless the entry is as new as the index
/// file itself, which it then reads again ("racy git"). A copy stamped now would make an edit of
/// the same size, in the same second as the index was written, look clean.
async fn same_age(own: &Path, copy: &Path) -> Result<(), Failed> {
    let stamp = |e: std::io::Error| Failed(format!("the scratch index's time: {e}"));
    let written = tokio::fs::metadata(own).await.and_then(|m| m.modified()).map_err(stamp)?;
    let copy = tokio::fs::OpenOptions::new().write(true).open(copy).await.map_err(stamp)?;
    let copy = copy.into_std().await;
    tokio::task::spawn_blocking(move || {
        copy.set_times(std::fs::FileTimes::new().set_modified(written))
    })
    .await
    .map_err(|e| Failed(format!("the scratch index's time: {e}")))?
    .map_err(stamp)
}

/// What a range's review says when either branch is in neither the clone nor its `origin`.
pub const NO_BRANCH: &str = "The branch is not in this clone or its origin";

/// What a range's review says when its two branches share no history.
pub const NO_FORK: &str = "The two branches share no history";

/// The working tree of the repository at `root` now, new files and all, as a review against
/// `against`.
///
/// The tree is written through a scratch copy of the person's index, so it hashes only what
/// changed since git last looked, and their index, `HEAD` and refs are never touched.
///
/// # Errors
///
/// When git fails, or no scratch index can be made.
pub async fn working_tree(git: &Path, root: &Path, against: Against) -> Result<Review, Failed> {
    working_tree_with(git, root, against, false).await
}

/// [`working_tree`], whitespace not counting when `ignore_whitespace` ([`Repo::changes_with`]).
///
/// # Errors
///
/// When git fails, or no scratch index can be made.
pub async fn working_tree_with(
    git: &Path,
    root: &Path,
    against: Against,
    ignore_whitespace: bool,
) -> Result<Review, Failed> {
    let (repo, _scratch) = scratch_repo(git, root).await?;
    let scope = ReviewScope::WorkingTree(against.clone());
    let Some(from) = repo.against(&against).await? else {
        return Ok(Review {
            scope,
            from: None,
            to: None,
            files: Vec::new(),
            absent: Some(NO_BASE.to_owned()),
        });
    };
    let to = repo.take().await?;
    let files = repo.changes_with(&from, &to, ignore_whitespace).await?;
    Ok(Review { scope, from: Some(from), to: Some(to), files, absent: None })
}

/// The repository at `root` written through a scratch copy of the person's index, so a tree of
/// the working tree hashes only what changed since git last looked, and their index, `HEAD` and
/// refs are never touched; the scratch directory, which goes when dropped.
async fn scratch_repo(git: &Path, root: &Path) -> Result<(Repo, tempfile::TempDir), Failed> {
    let scratch = tempfile::tempdir().map_err(|e| Failed(format!("a scratch index: {e}")))?;
    let index = scratch.path().join("index");
    let repo = Repo { git: git.to_owned(), root: root.to_owned(), index: index.clone() };
    let own = ["rev-parse", "--path-format=absolute", "--git-path", "index"];
    let own = PathBuf::from(repo.line(&own, None).await?);
    // No index yet (nothing ever added): the scratch one starts from `HEAD` instead.
    if tokio::fs::copy(&own, &index).await.is_err() {
        let _partial = tokio::fs::remove_file(&index).await;
    } else {
        same_age(&own, &index).await?;
    }
    Ok((repo, scratch))
}

/// What the commits of `head` brought since it left `base`, as committed, in the repository at
/// `root` ([`ReviewScope::Range`]): from where `head` forked from `base` to `head`'s tree.
///
/// Either branch is read as `origin` holds it when that holds every commit of the local one,
/// else as the local one, so a goal pushed from another machine still reviews. No index,
/// checkout or ref is touched. When either branch or their fork is missing the review says so
/// in `absent`.
///
/// # Errors
///
/// When git fails.
pub async fn range(git: &Path, root: &Path, base: &str, head: &str) -> Result<Review, Failed> {
    // Only committed objects are read: no index is ever named.
    let repo = Repo { git: git.to_owned(), root: root.to_owned(), index: PathBuf::new() };
    let scope = ReviewScope::Range { base: base.to_owned(), head: head.to_owned() };
    let absent = |why: &str| Review {
        scope: scope.clone(),
        from: None,
        to: None,
        files: Vec::new(),
        absent: Some(why.to_owned()),
    };
    let (Some(old), Some(new)) = (repo.branch(base).await, repo.branch(head).await) else {
        return Ok(absent(NO_BRANCH));
    };
    let Ok(fork) = repo.line(&["merge-base", &old, &new], None).await else {
        return Ok(absent(NO_FORK));
    };
    let from = TreeRef(repo.line(&["rev-parse", &format!("{fork}^{{tree}}")], None).await?);
    let to = TreeRef(repo.line(&["rev-parse", &format!("{new}^{{tree}}")], None).await?);
    let files = repo.changes(&from, &to).await?;
    Ok(Review { scope, from: Some(from), to: Some(to), files, absent: None })
}

/// One file of a review whole, past the review's own budget.
///
/// It is read by the blobs the review named on its two sides (none for a side the file lacks)
/// in the repository at `root`: its hunks, or none when a side is not text.
///
/// # Errors
///
/// When a side is not an object id, or git has no such blob.
pub async fn file_diff(
    git: &Path,
    root: &Path,
    (from, to): (Option<&str>, Option<&str>),
    ignore_whitespace: bool,
) -> Result<Patch, Failed> {
    for id in from.iter().chain(to.iter()) {
        object_id(id)?;
    }
    // Blobs are read from the object store alone: no index is ever named.
    let repo = Repo { git: git.to_owned(), root: root.to_owned(), index: PathBuf::new() };
    repo.patch(from, to, ignore_whitespace).await.map(|(_binary, patch)| patch)
}

/// The bytes of blob `id` in the repository at `root`, a review's picture side
/// ([`slopty_proto::git::GitOp::Blob`]): refused past [`BLOB_MAX`] bytes, read from the
/// object store alone.
///
/// # Errors
///
/// An id that is not a whole object id, a blob too large, or git failing.
pub async fn blob(git: &Path, root: &Path, id: &str) -> Result<Vec<u8>, Failed> {
    object_id(id)?;
    let repo = Repo { git: git.to_owned(), root: root.to_owned(), index: PathBuf::new() };
    let size = repo.line(&["cat-file", "-s", id], None).await?;
    let size: u64 = size.parse().map_err(|e| Failed(format!("git sized {id} as {size:?}: {e}")))?;
    if size > BLOB_MAX {
        return Err(Failed(format!("{id} is {size} bytes, past the {BLOB_MAX} sent whole")));
    }
    repo.blob(Some(id)).await
}

/// Keep the hunks of the files read first while `budget` lines last, in the order the review
/// lists them ([`reading_order`]): the weightiest first, the quiet ones (tests, locks,
/// generated code) after the rest. A file past what is left keeps its counts and says how many
/// lines it has ([`Patch::clipped_lines`]), and a lighter one after it may still fit.
fn spend(files: &mut [FileDiff], budget: u32) {
    let mut room = budget;
    for at in reading_order(files) {
        let Some(file) = files.get_mut(at) else { continue };
        let lines = file.patch.hunks.iter().map(|h| h.lines.len()).sum::<usize>();
        let lines = u32::try_from(lines).unwrap_or(u32::MAX);
        if lines > room {
            file.patch.clipped_lines = lines;
            file.patch.hunks.clear();
        } else {
            room = room.saturating_sub(lines);
        }
    }
}

/// `id`, when it is a whole object id: a client names blobs, and git takes nothing else.
fn object_id(id: &str) -> Result<(), Failed> {
    let hex = id.bytes().all(|b| b.is_ascii_hexdigit());
    if !hex || !matches!(id.len(), 40 | 64) {
        return Err(Failed(format!("{id:?} is no object id")));
    }
    Ok(())
}

const fn empty_patch() -> Patch {
    Patch { hunks: Vec::new(), added: 0, removed: 0, clipped_lines: 0, full: None }
}

/// One line of `git diff-tree -r -z` output.
struct Entry {
    path: String,
    /// Where a rename came from.
    old_path: Option<String>,
    from: Option<String>,
    to: Option<String>,
    /// Each side's mode, zero for a side that is not there.
    modes: Option<Modes>,
}

/// `:old_mode new_mode old_blob new_blob status\0path\0`, a blob of zeros for a side that is
/// not there; a rename's status (`R<score>`) is followed by its old path, then its new one.
fn parse_raw(raw: &[u8]) -> Vec<Entry> {
    let text = String::from_utf8_lossy(raw);
    let mut fields = text.split('\0');
    let mut entries = Vec::new();
    while let (Some(head), Some(path)) = (fields.next(), fields.next()) {
        let words: Vec<&str> = head.trim_start_matches(':').split(' ').collect();
        let side = |at: usize| {
            words.get(at).filter(|b| !b.bytes().all(|c| c == b'0')).map(|b| (*b).to_owned())
        };
        let mode = |at: usize| words.get(at).and_then(|m| u32::from_str_radix(m, 8).ok());
        let Some(status) = words.get(4) else { break };
        let (old_path, path) = if status.starts_with(['R', 'C']) {
            let Some(new) = fields.next() else { break };
            (Some(path.to_owned()), new.to_owned())
        } else {
            (None, path.to_owned())
        };
        let modes = mode(0).zip(mode(1)).map(|(from, to)| Modes { from, to });
        entries.push(Entry { path, old_path, from: side(2), to: side(3), modes });
    }
    entries
}

/// One blob as a review reads it ([`Repo::read_blobs`]).
#[derive(Debug, Default)]
struct Read {
    /// Its size, in bytes.
    size: u64,
    /// Its head holds a NUL, so it is not text.
    binary: bool,
    /// Its bytes, when it is text no larger than [`TEXT_BYTES`].
    bytes: Option<Vec<u8>>,
}

impl Read {
    /// Its bytes when they are text to cut into hunks; a side that is not there is empty text.
    fn text(&self) -> Option<&[u8]> {
        if self.size == 0 && !self.binary {
            return Some(&[]);
        }
        self.bytes.as_deref()
    }
}

/// How far into a blob git looks for a NUL to call it binary.
const BINARY_HEAD: usize = 8000;

/// Read `count` answers of `git cat-file --batch` from `out`, by object id.
async fn batch(
    out: tokio::process::ChildStdout,
    count: usize,
) -> std::io::Result<HashMap<String, Read>> {
    use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _};
    let mut out = tokio::io::BufReader::new(out);
    let mut read = HashMap::with_capacity(count);
    let mut header = String::new();
    for _ in 0..count {
        header.clear();
        if out.read_line(&mut header).await? == 0 {
            break;
        }
        let mut words = header.split_whitespace();
        let (Some(id), Some(_kind), Some(size)) = (words.next(), words.next(), words.next()) else {
            continue;
        };
        let size: u64 = size.parse().map_err(std::io::Error::other)?;
        let held = usize::try_from(size).is_ok_and(|n| n <= TEXT_BYTES);
        let mut bytes = Vec::new();
        let mut body = (&mut out).take(size);
        if held {
            body.read_to_end(&mut bytes).await?;
        } else {
            let mut head = vec![0; BINARY_HEAD];
            let n = body.read(&mut head).await?;
            head.truncate(n);
            bytes = head;
            tokio::io::copy(&mut body, &mut tokio::io::sink()).await?;
        }
        let binary = bytes.iter().take(BINARY_HEAD).any(|b| *b == 0);
        let mut end = [0_u8; 1];
        out.read_exact(&mut end).await?;
        let bytes = (held && !binary).then_some(bytes);
        read.insert(id.to_owned(), Read { size, binary, bytes });
    }
    Ok(read)
}

/// Whether the file at `path` is a picture the client draws: by its extension, as an editor
/// would open it.
fn is_picture(path: &str) -> bool {
    const PICTURES: [&str; 10] =
        ["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "ico", "svg"];
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| PICTURES.iter().any(|p| e.eq_ignore_ascii_case(p)))
}

/// Whether `bytes` are cut into hunks: small enough, and with no NUL where git looks for one.
fn is_text(bytes: &[u8]) -> bool {
    bytes.len() <= TEXT_BYTES && !bytes.iter().take(8000).any(|b| *b == 0)
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `text`'s lines, each with its line end.
fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// The hunks from `old` to `new`, as a review shows them.
fn groups<T: std::hash::Hash + Eq + Ord>(old: &[T], new: &[T]) -> Vec<Vec<DiffOp>> {
    let ops = similar::capture_diff_slices(Algorithm::Myers, old, new);
    similar::group_diff_ops(ops, CONTEXT)
}

/// `line` with no whitespace in it, as `git diff -w` compares lines.
fn spaceless(line: &str) -> String {
    line.chars().filter(|c| !c.is_whitespace()).collect()
}

/// What a group of ops covers on each side.
fn span(group: &[DiffOp]) -> (Range<usize>, Range<usize>) {
    let first = group.first().map_or((0..0, 0..0), |op| (op.old_range(), op.new_range()));
    let last = group.last().map_or((0..0, 0..0), |op| (op.old_range(), op.new_range()));
    (first.0.start..last.0.end, first.1.start..last.1.end)
}

/// The diff from `old` to `new`.
#[must_use]
pub fn diff(old: &str, new: &str) -> Patch {
    diff_with(old, new, false)
}

/// The diff from `old` to `new`, whitespace not counting when `ignore_whitespace` (`git diff
/// -w`): lines are matched with their spaces taken out, and a line kept shows as the new side
/// has it.
#[must_use]
pub fn diff_with(old: &str, new: &str, ignore_whitespace: bool) -> Patch {
    let (old_lines, new_lines) = (lines(old), lines(new));
    let grouped = if ignore_whitespace {
        let key = |lines: &[&str]| lines.iter().map(|l| spaceless(l)).collect::<Vec<_>>();
        groups(&key(&old_lines), &key(&new_lines))
    } else {
        groups(&old_lines, &new_lines)
    };
    let mut patch = empty_patch();
    for group in grouped {
        let (old_span, new_span) = span(&group);
        let mut text = Vec::new();
        for op in &group {
            let (tag, old_range, new_range) = op.as_tag_tuple();
            let line = |mark: char, l: &str| format!("{mark}{}", l.strip_suffix('\n').unwrap_or(l));
            match tag {
                DiffTag::Equal => {
                    let same = new_lines.get(new_range).unwrap_or_default();
                    text.extend(same.iter().map(|l| line(' ', l)));
                }
                DiffTag::Delete | DiffTag::Insert | DiffTag::Replace => {
                    let gone = old_lines.get(old_range).unwrap_or_default();
                    let came = new_lines.get(new_range).unwrap_or_default();
                    patch.removed =
                        patch.removed.saturating_add(u32::try_from(gone.len()).unwrap_or(u32::MAX));
                    patch.added =
                        patch.added.saturating_add(u32::try_from(came.len()).unwrap_or(u32::MAX));
                    text.extend(gone.iter().map(|l| line('-', l)));
                    text.extend(came.iter().map(|l| line('+', l)));
                }
            }
        }
        let number = |r: &Range<usize>| {
            u32::try_from(r.start.saturating_add(usize::from(!r.is_empty()))).unwrap_or(u32::MAX)
        };
        let count = |r: &Range<usize>| u32::try_from(r.len()).unwrap_or(u32::MAX);
        let above = old_lines.get(..old_span.start).unwrap_or_default();
        patch.hunks.push(Hunk {
            old_start: number(&old_span),
            old_lines: count(&old_span),
            new_start: number(&new_span),
            new_lines: count(&new_span),
            heading: heading(above.iter().map(|l| l.trim_end_matches('\n'))),
            lines: text,
        });
    }
    patch
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\n";

    fn changed() -> String {
        OLD.replace("b\n", "B\n").replace("l\n", "L\nL2\n")
    }

    /// Two changes far apart are two hunks, each with its context, numbered as git numbers
    /// them.
    #[test]
    fn hunks_are_cut_with_three_lines_of_context() {
        let patch = diff(OLD, &changed());
        assert_eq!(patch.hunks.len(), 2, "{patch:#?}");
        let first = &patch.hunks[0];
        assert_eq!(
            (first.old_start, first.old_lines, first.new_start, first.new_lines),
            (1, 5, 1, 5)
        );
        assert_eq!(first.lines, [" a", "-b", "+B", " c", " d", " e"]);
        let second = &patch.hunks[1];
        assert_eq!(
            (second.old_start, second.old_lines, second.new_start, second.new_lines),
            (9, 5, 9, 6)
        );
        assert_eq!((patch.added, patch.removed), (3, 2));
    }

    /// Each hunk is headed as git heads it with no diff driver: the same file pair through
    /// `git diff --no-index` names the same enclosing lines, the first hunk with none above it.
    #[test]
    fn hunks_are_headed_as_git_heads_them() {
        let Some(git) = crate::changes::git() else { return };
        let body = |n: usize| {
            (0..n).map(|i| format!("    let v{i} = {i};\n")).collect::<Vec<_>>().concat()
        };
        let long = format!("fn {}(x: u32) {{   ", "very_long_name_".repeat(6));
        let old = format!(
            "use std::io;\nstruct S;\n\nimpl S {{\n    fn one(&self) {{\n{}    }}\n}}\n\n{long}\n{}}}\n",
            body(8),
            body(8)
        );
        let new = old
            .replace("let v4 = 4;", "let v4 = 40;")
            .replacen("let v6 = 6;", "", 2)
            .replace("io", "fmt");
        let ours = diff(&old, &new);
        let tmp = tempfile::tempdir().expect("temp");
        std::fs::write(tmp.path().join("old.txt"), &old).expect("write");
        std::fs::write(tmp.path().join("new.txt"), &new).expect("write");
        let out = std::process::Command::new(git)
            .current_dir(tmp.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(["diff", "--no-index", "--no-color", "-U3", "old.txt", "new.txt"])
            .output()
            .expect("git diff");
        let theirs: Vec<Option<String>> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.starts_with("@@ "))
            .map(slopty_proto::thread::detail::header_heading)
            .collect();
        let headed: Vec<Option<String>> = ours.hunks.iter().map(|h| h.heading.clone()).collect();
        assert_eq!(headed, theirs, "{ours:#?}");
        assert_eq!(theirs.len(), 3, "the pair makes three hunks");
        assert_eq!(theirs.first(), Some(&None), "nothing is above the first");
        assert!(theirs.iter().any(|h| h.as_deref().is_some_and(|h| h.len() == 80)), "{theirs:?}");
    }

    #[test]
    fn diff_tree_output_is_read_with_a_missing_side() {
        let zeros = "0".repeat(40);
        let raw = format!(
            ":100644 100644 {a} {b} M\0src/lib.rs\0:000000 100644 {zeros} {b} A\0new.txt\0",
            a = "a".repeat(40),
            b = "b".repeat(40)
        );
        let entries = parse_raw(raw.as_bytes());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].path, "new.txt");
        assert_eq!(entries[1].from, None);
        assert!(entries[0].from.is_some() && entries[0].to.is_some());
    }
}
