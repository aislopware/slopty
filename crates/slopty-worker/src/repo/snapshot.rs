//! Snapshots of a working tree at a thread's turn edges, in private refs, and the review over
//! them: what changed between two, and a change kept or put back.
//!
//! A snapshot is the working tree as `git add -A` sees it (untracked files in, ignored ones
//! out), written as a tree through an index of the thread's own, so the person's index, `HEAD`
//! and stash are never touched. The index stays between snapshots, so each one hashes only
//! what changed since the last (git's stat cache); the first starts from `HEAD`. Each tree is
//! kept alive by a commit under `refs/slopty/threads/<thread>/<turn>-{before,after}`, and what
//! the person kept under `refs/slopty/threads/<thread>/kept`. The change an agent was last asked
//! to review is one commit on a base of its own, under `review-base` and `review-head`, which
//! the next review overwrites. A push names the person's branches, never `refs/slopty`, and none
//! of these commits is an ancestor of a branch, so none leaves the machine.
//!
//! A folder's working tree is reviewed the same way with no thread ([`working_tree`]), against
//! `HEAD` or the branch's base, through a scratch copy of the person's index that goes with the
//! review.
//!
//! Hunks are cut here, with three lines of context, the same way every time: a hunk named by
//! its place ([`Pick::hunks`]) is the hunk the review showed, as long as both of its sides are
//! the blobs it showed. Each change is checked against them before anything is written.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use similar::{Algorithm, DiffOp, DiffTag};
use slopty_proto::thread::detail::{Hunk, heading};
use slopty_proto::thread::wire::{Against, FileDiff, Pick, Review, ReviewScope, reading_order};
use slopty_proto::thread::{Edge, Patch, ThreadId, TreeRef, TurnId};
use tokio::io::AsyncWriteExt as _;

/// Lines of context around each change in a hunk.
pub const CONTEXT: usize = 3;

/// The most diff lines a review carries in all; the files past it come without hunks.
pub const REVIEW_LINES: u32 = 20_000;

/// The largest file cut into hunks; a larger one is shown as changed, with no hunks.
pub const TEXT_BYTES: usize = 4 << 20;

/// The turns whose snapshots a thread keeps; an older turn's refs go.
pub const KEEP_TURNS: u32 = 256;

/// How long one git run may take.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(60);

/// Who the snapshot commits say made them: no person's identity is needed or used.
const IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "Slopty"),
    ("GIT_AUTHOR_EMAIL", "slopty@localhost"),
    ("GIT_COMMITTER_NAME", "Slopty"),
    ("GIT_COMMITTER_EMAIL", "slopty@localhost"),
];

/// Something git would not do, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failed(pub String);

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Failed {}

/// One repository's snapshots: its root, the git that runs on it, and where the thread's own
/// index is kept.
#[derive(Clone, Debug)]
pub struct Repo {
    /// The git to run.
    pub git: PathBuf,
    /// The repository's root.
    pub root: PathBuf,
    /// The thread's index, made on the first snapshot.
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
            .envs(IDENTITY)
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

    /// The working tree as a tree, through the thread's index.
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

    /// Keep `tree` as `thread`'s snapshot at `edge` of `turn`, and let the snapshots of the
    /// turn [`KEEP_TURNS`] before it go.
    ///
    /// # Errors
    ///
    /// When git fails.
    pub async fn record(
        &self,
        thread: ThreadId,
        turn: TurnId,
        edge: Edge,
        tree: &TreeRef,
    ) -> Result<(), Failed> {
        let name = format!("{}-{}", turn.0, edge_name(edge));
        self.pin(thread, &name, tree).await?;
        if let Some(old) = turn.0.checked_sub(KEEP_TURNS) {
            for edge in [Edge::Before, Edge::After] {
                let gone = format!("{}/{old}-{}", refs(thread), edge_name(edge));
                let _missing = self.run(&["update-ref", "-d", &gone], None, None).await;
            }
        }
        Ok(())
    }

    /// Keep `tree` alive under `thread`'s ref `name`.
    async fn pin(&self, thread: ThreadId, name: &str, tree: &TreeRef) -> Result<(), Failed> {
        let message = format!("slopty: thread {thread} {name}");
        let commit = self.line(&["commit-tree", &tree.0, "-m", &message], None).await?;
        let reference = format!("{}/{name}", refs(thread));
        self.run(&["update-ref", &reference, &commit], None, None).await?;
        Ok(())
    }

    /// The change from `from` to `to` as one commit, for an agent's own review of it: `from`
    /// as a base commit of its own and `to` as a commit on it, kept under `thread`'s
    /// `review-base` and `review-head`, which each review overwrites. Their ids, base first, so
    /// `base...head` and the head commit alone both say exactly the change. They are made at
    /// a fixed time ([`Self::commit_at`]), so the same change is always the same two ids.
    ///
    /// # Errors
    ///
    /// When git fails, or either side is no tree here.
    pub async fn review_pair(
        &self,
        thread: ThreadId,
        from: &TreeRef,
        to: &TreeRef,
    ) -> Result<(String, String), Failed> {
        let base_message = format!("slopty: thread {thread} review base");
        let at = slopty_core::WallMs::ZERO;
        let base = self.commit_at(from, None, &base_message, at).await?;
        let message = format!("slopty: thread {thread} review");
        let head = self.commit_at(to, Some(&base), &message, at).await?;
        let refs = refs(thread);
        let updates = format!(
            "start\nupdate {refs}/review-base {base}\nupdate {refs}/review-head {head}\n\
             prepare\ncommit\n"
        );
        self.run(&["update-ref", "--stdin"], None, Some(updates.as_bytes())).await?;
        Ok((base, head))
    }

    /// A commit of `tree` on `parent` that says `message`, authored at `at` by Slopty: the same
    /// arguments always make the same commit, so a history of a thread's turns is made once
    /// and found again. Nothing names it; it stays while something it is in is asked about.
    ///
    /// # Errors
    ///
    /// When git fails, or `tree` is no tree here.
    pub async fn commit_at(
        &self,
        tree: &TreeRef,
        parent: Option<&str>,
        message: &str,
        at: slopty_core::WallMs,
    ) -> Result<String, Failed> {
        // `hash-object` writes whatever it is given: a commit of a tree not here would break
        // the repository.
        let tree_here = format!("{}^{{tree}}", tree.0);
        self.run(&["rev-parse", "-q", "--verify", &tree_here], None, None).await?;
        let secs = at.as_millis() / 1_000;
        let parent = parent.map(|p| format!("parent {p}\n")).unwrap_or_default();
        let who = format!("Slopty <slopty@localhost> {secs} +0000");
        let object =
            format!("tree {}\n{parent}author {who}\ncommitter {who}\n\n{message}\n", tree.0);
        let out = self
            .run(&["hash-object", "-t", "commit", "-w", "--stdin"], None, Some(object.as_bytes()))
            .await?;
        Ok(String::from_utf8_lossy(&out).trim().to_owned())
    }

    /// The commit each line of the working file `path` (relative to the root) came from, as
    /// `git blame` reads the file against the history ending at `head`, in the file's order. A
    /// line changed since `head` has no commit.
    ///
    /// # Errors
    ///
    /// When git fails, or `head` has no such file.
    pub async fn blame(&self, head: &str, path: &str) -> Result<Vec<Option<String>>, Failed> {
        let file = self.root.join(path);
        let file = file.to_string_lossy();
        let args = ["blame", "--porcelain", "--contents", &file, head, "--", path];
        let out = self.run(&args, None, None).await?;
        Ok(blamed(&String::from_utf8_lossy(&out)))
    }

    /// The thread each of `commits` names in its `Slopty-Thread` trailer, with when it was
    /// made; a commit that names none is left out.
    ///
    /// # Errors
    ///
    /// When git fails.
    pub async fn trailed(
        &self,
        commits: &[String],
    ) -> Result<std::collections::HashMap<String, (ThreadId, slopty_core::WallMs)>, Failed> {
        if commits.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let format =
            "--format=%H%x1f%ct%x1f%(trailers:key=Slopty-Thread,valueonly,separator=%x2C)%x1e";
        let mut args = vec!["show", "-s", "--no-walk", format];
        args.extend(commits.iter().map(String::as_str));
        let out = self.run(&args, None, None).await?;
        Ok(trailers(&String::from_utf8_lossy(&out)))
    }

    /// The id git gives the working file `path` (relative to the root).
    ///
    /// # Errors
    ///
    /// When git fails.
    pub async fn blob_of(&self, path: &str) -> Result<String, Failed> {
        self.line(&["hash-object", "--", path], None).await
    }

    /// Let every ref of `thread` go, its snapshots and its review alike: the thread is gone.
    ///
    /// # Errors
    ///
    /// When git fails.
    pub async fn forget(&self, thread: ThreadId) -> Result<(), Failed> {
        let prefix = format!("{}/", refs(thread));
        let listed = self.line(&["for-each-ref", "--format=%(refname)", &prefix], None).await?;
        let deletes: String = listed.lines().flat_map(|name| ["delete ", name, "\n"]).collect();
        if deletes.is_empty() {
            return Ok(());
        }
        self.run(&["update-ref", "--stdin"], None, Some(deletes.as_bytes())).await.map(|_| ())
    }

    /// Put the working tree back to `tree`, as a snapshot sees it, keeping what it held first
    /// under `thread`'s ref `name`: every file a snapshot holds goes back to its blob and mode,
    /// and a file `tree` lacks goes. Ignored files, the person's index, `HEAD` and stash are
    /// never touched.
    ///
    /// # Errors
    ///
    /// When git fails.
    pub async fn restore(
        &self,
        thread: ThreadId,
        name: &str,
        tree: &TreeRef,
    ) -> Result<(), Failed> {
        let now = self.take().await?;
        self.pin(thread, name, &now).await?;
        // Through the thread's index, which holds every file of `now`: a file it holds that
        // `tree` lacks is removed, which the person's index would not know of.
        let source = format!("--source={}", tree.0);
        let index = Some(self.index.as_path());
        self.run(&["restore", &source, "--worktree", "--", "."], index, None).await?;
        self.take().await.map(|_now| ())
    }

    /// What `thread`'s person has kept, if they have kept anything.
    ///
    /// # Errors
    ///
    /// When git fails.
    pub async fn kept(&self, thread: ThreadId) -> Result<Option<TreeRef>, Failed> {
        let reference = format!("{}/kept^{{tree}}", refs(thread));
        match self.line(&["rev-parse", "-q", "--verify", &reference], None).await {
            Ok(tree) if !tree.is_empty() => Ok(Some(TreeRef(tree))),
            _ => Ok(None),
        }
    }

    /// Every file that differs from `from` to `to`, by path, cut into hunks while the review
    /// has room for them ([`REVIEW_LINES`]), spent in the order the files are read
    /// ([`reading_order`]).
    ///
    /// # Errors
    ///
    /// When git fails.
    pub async fn changes(&self, from: &TreeRef, to: &TreeRef) -> Result<Vec<FileDiff>, Failed> {
        let raw = self
            .run(&["diff-tree", "-r", "-z", "--no-renames", &from.0, &to.0], None, None)
            .await?;
        let mut files = Vec::new();
        for entry in parse_raw(&raw) {
            let (binary, patch) = self.patch(entry.from.as_deref(), entry.to.as_deref()).await?;
            files.push(FileDiff {
                path: entry.path,
                from: entry.from,
                to: entry.to,
                binary,
                patch,
            });
        }
        spend(&mut files, REVIEW_LINES);
        Ok(files)
    }

    /// One file's hunks whole, from blob `from` to blob `to` (none for a side the file lacks),
    /// and whether a side is not text: binary, or past [`TEXT_BYTES`], which has no hunks.
    async fn patch(&self, from: Option<&str>, to: Option<&str>) -> Result<(bool, Patch), Failed> {
        let old = self.blob(from).await?;
        let new = self.blob(to).await?;
        if !is_text(&old) || !is_text(&new) {
            return Ok((true, empty_patch()));
        }
        Ok((false, diff(&lossy(&old), &lossy(&new))))
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

    /// The blob the file at `path` would be now; `None` when there is no file.
    ///
    /// # Errors
    ///
    /// When git fails.
    pub async fn stamp(&self, path: &str) -> Result<Option<String>, Failed> {
        if tokio::fs::symlink_metadata(self.root.join(path)).await.is_err() {
            return Ok(None);
        }
        Ok(Some(self.line(&["hash-object", "--", path], None).await?))
    }

    /// Put `pick` back in the working tree as its old side has it, once the file is checked to
    /// be what the review showed.
    ///
    /// # Errors
    ///
    /// The file changed since, a hunk that is not there, or git failing.
    pub async fn revert(&self, pick: &Pick) -> Result<(), Failed> {
        let path = self.inside(&pick.path)?;
        if self.stamp(&pick.path).await? != pick.stamp {
            return Err(Failed(format!("{} changed since it was reviewed", pick.path)));
        }
        let old = self.blob(pick.from.as_deref()).await?;
        let bytes = if pick.hunks.is_empty() {
            if pick.from.is_none() {
                return tokio::fs::remove_file(&path)
                    .await
                    .map_err(|e| Failed(format!("{}: {e}", pick.path)));
            }
            old
        } else {
            let new =
                tokio::fs::read(&path).await.map_err(|e| Failed(format!("{}: {e}", pick.path)))?;
            if !is_text(&old) || !is_text(&new) {
                return Err(Failed(format!("{} is not text, so it goes back whole", pick.path)));
            }
            swap(&lossy(&old), &lossy(&new), &pick.hunks, Side::Old)?.into_bytes()
        };
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|e| Failed(format!("{}: {e}", pick.path)))?;
        }
        tokio::fs::write(&path, bytes).await.map_err(|e| Failed(format!("{}: {e}", pick.path)))
    }

    /// Take `pick` into what `thread`'s person has kept, starting from `base` when they have
    /// kept nothing yet: its old side must be what is kept of the file.
    ///
    /// # Errors
    ///
    /// What is kept is not the review's old side, a hunk that is not there, or git failing.
    pub async fn keep(
        &self,
        thread: ThreadId,
        base: &TreeRef,
        pick: &Pick,
    ) -> Result<TreeRef, Failed> {
        let kept = self.kept(thread).await?.unwrap_or_else(|| base.clone());
        let now = self.entry(&kept, &pick.path).await?;
        if now.as_ref().map(|(_, blob)| blob) != pick.from.as_ref() {
            return Err(Failed(format!("{} was kept otherwise since it was reviewed", pick.path)));
        }
        let blob = if pick.hunks.is_empty() {
            pick.stamp.clone()
        } else {
            let (old, new) =
                (self.blob(pick.from.as_deref()).await?, self.blob(pick.stamp.as_deref()).await?);
            if !is_text(&old) || !is_text(&new) {
                return Err(Failed(format!("{} is not text, so it is kept whole", pick.path)));
            }
            let text = swap(&lossy(&old), &lossy(&new), &pick.hunks, Side::New)?;
            Some(self.line_with(&["hash-object", "-w", "--stdin"], text.as_bytes()).await?)
        };
        let index = self.index.with_extension("kept");
        let index = Some(index.as_path());
        self.run(&["read-tree", &kept.0], index, None).await?;
        match blob {
            Some(blob) => {
                let mode = now.map_or_else(|| "100644".to_owned(), |(mode, _)| mode);
                let info = format!("{mode},{blob},{}", pick.path);
                self.run(&["update-index", "--add", "--cacheinfo", &info], index, None).await?;
            }
            None => {
                self.run(&["update-index", "--force-remove", "--", &pick.path], index, None)
                    .await?;
            }
        }
        let tree = TreeRef(self.line(&["write-tree"], index).await?);
        self.pin(thread, "kept", &tree).await?;
        Ok(tree)
    }

    async fn line_with(&self, args: &[&str], input: &[u8]) -> Result<String, Failed> {
        let out = self.run(args, None, Some(input)).await?;
        Ok(String::from_utf8_lossy(&out).trim().to_owned())
    }

    /// The mode and blob of `path` in `tree`.
    async fn entry(&self, tree: &TreeRef, path: &str) -> Result<Option<(String, String)>, Failed> {
        let out = self.run(&["ls-tree", "-z", &tree.0, "--", path], None, None).await?;
        let line = String::from_utf8_lossy(&out);
        let Some(entry) = line.split('\0').next().filter(|e| !e.is_empty()) else {
            return Ok(None);
        };
        let mut words = entry.split_whitespace();
        let (mode, _kind, blob) = (words.next(), words.next(), words.next());
        Ok(mode.zip(blob).map(|(mode, blob)| (mode.to_owned(), blob.to_owned())))
    }

    /// `path` under the root, refused when it would climb out.
    fn inside(&self, path: &str) -> Result<PathBuf, Failed> {
        let relative = Path::new(path);
        let climbs = relative.components().any(|c| !matches!(c, std::path::Component::Normal(_)));
        if climbs || path.is_empty() {
            return Err(Failed(format!("{path} is not a path in the repository")));
        }
        Ok(self.root.join(relative))
    }
}

/// The refs of `thread`'s snapshots.
/// The commit of each line in `git blame --porcelain`'s answer, in the file's order; the
/// all-zero commit, a line not in the history, is none.
fn blamed(porcelain: &str) -> Vec<Option<String>> {
    let mut lines: Vec<Option<String>> = Vec::new();
    let mut current: Option<(String, usize)> = None;
    for line in porcelain.lines() {
        if line.starts_with('\t') {
            if let Some((commit, at)) = current.as_mut() {
                if lines.len() <= *at {
                    lines.resize(at.saturating_add(1), None);
                }
                if let Some(slot) = lines.get_mut(*at) {
                    *slot = (commit.bytes().any(|b| b != b'0')).then(|| commit.clone());
                }
                *at = at.saturating_add(1);
            }
            continue;
        }
        let mut words = line.split(' ');
        let (Some(commit), Some(_orig), Some(fin)) = (words.next(), words.next(), words.next())
        else {
            continue;
        };
        let header = commit.len() >= 40 && commit.bytes().all(|b| b.is_ascii_hexdigit());
        if let (true, Ok(fin)) = (header, fin.parse::<usize>()) {
            current = Some((commit.to_owned(), fin.saturating_sub(1)));
        }
    }
    lines
}

/// `trailed`'s answer read: each commit with a thread in its trailer, and its time.
fn trailers(out: &str) -> std::collections::HashMap<String, (ThreadId, slopty_core::WallMs)> {
    out.split('\x1e')
        .filter_map(|record| {
            let mut fields = record.trim().split('\x1f');
            let commit = fields.next()?.trim();
            let secs: u64 = fields.next()?.trim().parse().ok()?;
            let thread =
                fields.next()?.split(',').find_map(|t| t.trim().parse::<ThreadId>().ok())?;
            let at = slopty_core::WallMs::from_millis(secs.saturating_mul(1_000));
            Some((commit.to_owned(), (thread, at)))
        })
        .collect()
}

fn refs(thread: ThreadId) -> String {
    format!("refs/slopty/threads/{thread}")
}

/// The branches a working tree's base is looked for on, in order ([`Repo::against`]).
const BASES: [&str; 5] = ["origin/HEAD", "origin/main", "origin/master", "main", "master"];

/// Why a working tree's review against its base has nothing to compare.
pub const NO_BASE: &str = "This branch has no base branch to compare with";

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
    let scratch = tempfile::tempdir().map_err(|e| Failed(format!("a scratch index: {e}")))?;
    let index = scratch.path().join("index");
    let repo = Repo { git: git.to_owned(), root: root.to_owned(), index: index.clone() };
    let own = ["rev-parse", "--path-format=absolute", "--git-path", "index"];
    let own = PathBuf::from(repo.line(&own, None).await?);
    // No index yet (nothing ever added): the scratch one starts from `HEAD` instead.
    if tokio::fs::copy(&own, &index).await.is_err() {
        let _partial = tokio::fs::remove_file(&index).await;
    }
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
    from: Option<&str>,
    to: Option<&str>,
) -> Result<Patch, Failed> {
    for id in from.iter().chain(to.iter()) {
        object_id(id)?;
    }
    // Blobs are read from the object store alone: no index is ever named.
    let repo = Repo { git: git.to_owned(), root: root.to_owned(), index: PathBuf::new() };
    repo.patch(from, to).await.map(|(_binary, patch)| patch)
}

const fn edge_name(edge: Edge) -> &'static str {
    match edge {
        Edge::Before => "before",
        Edge::After => "after",
    }
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
    from: Option<String>,
    to: Option<String>,
}

/// `:old_mode new_mode old_blob new_blob status\0path\0`, a blob of zeros for a side that is
/// not there.
fn parse_raw(raw: &[u8]) -> Vec<Entry> {
    let text = String::from_utf8_lossy(raw);
    let mut fields = text.split('\0');
    let mut entries = Vec::new();
    while let (Some(head), Some(path)) = (fields.next(), fields.next()) {
        let words: Vec<&str> = head.trim_start_matches(':').split(' ').collect();
        let side = |at: usize| {
            words.get(at).filter(|b| !b.bytes().all(|c| c == b'0')).map(|b| (*b).to_owned())
        };
        if words.len() < 5 {
            break;
        }
        entries.push(Entry { path: path.to_owned(), from: side(2), to: side(3) });
    }
    entries
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

/// The hunks from `old` to `new`, as a review shows them and a pick names them.
fn groups(old: &[&str], new: &[&str]) -> Vec<Vec<DiffOp>> {
    let ops = similar::capture_diff_slices(Algorithm::Myers, old, new);
    similar::group_diff_ops(ops, CONTEXT)
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
    let (old_lines, new_lines) = (lines(old), lines(new));
    let mut patch = empty_patch();
    for group in groups(&old_lines, &new_lines) {
        let (old_span, new_span) = span(&group);
        let mut text = Vec::new();
        for op in &group {
            let (tag, old_range, new_range) = op.as_tag_tuple();
            let line = |mark: char, l: &str| format!("{mark}{}", l.strip_suffix('\n').unwrap_or(l));
            match tag {
                DiffTag::Equal => {
                    let same = old_lines.get(old_range).unwrap_or_default();
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

/// Which side a [`swap`] takes the picked hunks from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    /// `new`, with the picked hunks as `old` has them: a revert.
    Old,
    /// `old`, with the picked hunks as `new` has them: a keep.
    New,
}

/// One side with the hunks `picked` taken from the other.
fn swap(old: &str, new: &str, picked: &[u32], take: Side) -> Result<String, Failed> {
    let (old_lines, new_lines) = (lines(old), lines(new));
    let groups = groups(&old_lines, &new_lines);
    if let Some(missing) =
        picked.iter().find(|&&h| usize::try_from(h).map_or(true, |h| h >= groups.len()))
    {
        return Err(Failed(format!("there is no hunk {missing}")));
    }
    let (base, other) = match take {
        Side::Old => (&new_lines, &old_lines),
        Side::New => (&old_lines, &new_lines),
    };
    let mut out = String::with_capacity(old.len().max(new.len()));
    let mut at = 0;
    for (n, group) in groups.iter().enumerate() {
        let (old_span, new_span) = span(group);
        let (here, there) = match take {
            Side::Old => (new_span, old_span),
            Side::New => (old_span, new_span),
        };
        if !picked.iter().any(|&h| usize::try_from(h).is_ok_and(|h| h == n)) {
            continue;
        }
        out.extend(base.get(at..here.start).unwrap_or_default().iter().copied());
        out.extend(other.get(there).unwrap_or_default().iter().copied());
        at = here.end;
    }
    out.extend(base.get(at..).unwrap_or_default().iter().copied());
    Ok(out)
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

    /// Reverting one hunk leaves the other; keeping one takes only it; both of either give
    /// the other side whole.
    #[test]
    fn a_picked_hunk_goes_back_or_is_kept_alone() {
        let new = changed();
        let back = swap(OLD, &new, &[0], Side::Old).unwrap();
        assert_eq!(back, OLD.replace("l\n", "L\nL2\n"));
        let kept = swap(OLD, &new, &[1], Side::New).unwrap();
        assert_eq!(kept, OLD.replace("l\n", "L\nL2\n"));
        assert_eq!(swap(OLD, &new, &[0, 1], Side::Old).unwrap(), OLD);
        assert_eq!(swap(OLD, &new, &[0, 1], Side::New).unwrap(), new);
        assert!(swap(OLD, &new, &[2], Side::Old).is_err(), "no third hunk");
        let no_end = swap("a\nb", "a\nc", &[0], Side::Old).unwrap();
        assert_eq!(no_end, "a\nb", "a last line without its end stays so");
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

    /// Blame's porcelain names each line's commit once per group, its own line numbers after;
    /// a line not yet committed is the all-zero commit, which is no one's.
    #[test]
    fn blame_gives_each_line_its_commit() {
        let a = "a".repeat(40);
        let zero = "0".repeat(40);
        let porcelain = format!(
            "{a} 1 1 2\nauthor Slopty\nsummary slopty: thread\nfilename x\n\tone\n\
             {a} 2 2\n\ttwo\n\
             {zero} 3 3 1\nauthor Not Committed Yet\nprevious {a} x\nfilename x\n\tthree\n\
             {a} 4 4 1\n\tfour\n"
        );
        let lines = blamed(&porcelain);
        assert_eq!(lines, [Some(a.clone()), Some(a.clone()), None, Some(a)]);
    }

    /// Each commit's trailer names its thread, a list keeps its first thread, and a commit with
    /// no trailer or a word that is no thread is left out.
    #[test]
    fn trailers_name_the_thread_that_made_a_commit() {
        let thread = ThreadId::new();
        let other = ThreadId::new();
        let out = format!(
            "c1\x1f100\x1f{thread}\x1e\nc2\x1f200\x1f\x1e\nc3\x1f300\x1fnot a thread\x1e\n\
             c4\x1f400\x1f{other},{thread}\x1e\n"
        );
        let found = trailers(&out);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found.get("c1"), Some(&(thread, slopty_core::WallMs::from_millis(100_000))));
        assert_eq!(found.get("c4").map(|f| f.0), Some(other));
    }
}
