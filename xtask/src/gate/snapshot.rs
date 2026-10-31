//! The tree a check builds: the index, HEAD's or a commit's, synced into `<dir>/tree` beside
//! the checkout ([`snapshot`], [`snapshot_commit`]).
//!
//! Several agents edit this one checkout at once, so only a recorded state is a unit anyone
//! vouched for, and a check builds exactly that. A sync rewrites only what changed, so cargo in
//! the snapshot rebuilds only that. `tree.index` beside the tree vouches for what is on disk:
//! the blob each path was last written from. Whatever a sync is about to write leaves it first,
//! so a sync killed midway (a lane's warm stopped, the Mac put to sleep) leaves its half-written
//! paths unvouched, and the next sync compares and rewrites them.

use std::collections::{BTreeMap, HashSet};
use std::time::Instant;

use anyhow::{Context as _, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use xshell::{Shell, cmd};

use super::pass;

/// What [`snapshot`] copies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Source {
    /// The index: what the next commit records, which the gate checks.
    Index,
    /// HEAD's tree: the commits `land` pushes.
    Head,
}

/// The `git ls-tree` format of a listing in `git ls-files --stage -z`'s, `<mode> <sha>
/// <stage>\t<path>`: a commit's entries are all at stage 0.
const TREE_FORMAT: &str = "--format=%(objectmode) %(objectname) 0%x09%(path)";

/// `source`'s entries as `git ls-files --stage -z` lists the index's.
pub(super) fn list(sh: &Shell, source: Source) -> Result<std::process::Output> {
    Ok(match source {
        Source::Index => cmd!(sh, "git ls-files --stage -z").quiet().output()?,
        Source::Head => {
            cmd!(sh, "git ls-tree -r -z --full-tree {TREE_FORMAT} HEAD").quiet().output()?
        }
    })
}

/// Commit `sha`'s entries as [`list`] lists the index's.
fn list_commit(root: &Utf8Path, sha: &str) -> Result<std::process::Output> {
    let sh = Shell::new()?;
    sh.change_dir(root);
    Ok(cmd!(sh, "git ls-tree -r -z --full-tree {TREE_FORMAT} {sha}").quiet().output()?)
}

/// Sync the **index** (what `git commit` would record, not the working tree), or HEAD's tree,
/// into `target/gate/tree`: what passes is exactly what the next commit records. Returns the
/// tree and the source's entries it holds.
pub(super) fn snapshot(root: &Utf8Path, source: Source) -> Result<(Utf8PathBuf, Vec<pass::Entry>)> {
    let sh = Shell::new()?;
    sh.change_dir(root);
    let what = if source == Source::Index { "the index" } else { "HEAD" };
    sync(root, &root.join("target").join("gate"), &list(&sh, source)?, what)
}

/// Commit `sha`'s tree synced into `<dir>/tree` as [`snapshot`] syncs the index: a local check
/// builds exactly what a runner would check out, beside the other lanes' unfinished edits.
pub fn snapshot_commit(root: &Utf8Path, dir: &Utf8Path, sha: &str) -> Result<Utf8PathBuf> {
    Ok(sync(root, dir, &list_commit(root, sha)?, "the check's commit")?.0)
}

/// Sync the entries `listed` names (`git ls-files --stage -z` lines) into `<dir>/tree`, saying
/// they are `what`. Blobs are read in one `git cat-file --batch` pass; paths the listing no
/// longer names are removed; a submodule (`vendor/ghostty`) is a symlink to a checkout at the
/// commit the listing pins ([`submodule_at`]).
fn sync(
    root: &Utf8Path,
    dir: &Utf8Path,
    listed: &std::process::Output,
    what: &str,
) -> Result<(Utf8PathBuf, Vec<pass::Entry>)> {
    let started = Instant::now();
    let syncing = begin(dir, listed)?;
    let written = syncing.write(root, dir)?;
    let (removed, files) = (syncing.removed, syncing.listing.len());
    let synced = syncing.finish()?;
    println!(
        "  snapshot of {what}, {files} files: {written} written, {removed} removed ({:.1?})",
        started.elapsed()
    );
    Ok(synced)
}

/// One entry of a listing.
#[derive(Clone, Debug)]
struct Listed {
    path: String,
    /// Its git mode: `100644`, `100755`, `120000` (a symlink) or `160000` (a submodule).
    mode: String,
    sha: String,
}

impl Listed {
    /// What `tree.index` records of it.
    fn key(&self) -> String {
        format!("{}:{}", self.mode, self.sha)
    }

    fn is_module(&self) -> bool {
        self.mode == "160000"
    }
}

/// The entries of `git ls-files --stage -z` output.
fn entries(listed: &std::process::Output) -> Result<Vec<Listed>> {
    let mut entries = Vec::new();
    for entry in listed.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let entry = std::str::from_utf8(entry).context("an index path is not UTF-8")?;
        let Some((meta, path)) = entry.split_once('\t') else { continue };
        let mut fields = meta.split(' ');
        let (Some(mode), Some(sha), Some(stage)) = (fields.next(), fields.next(), fields.next())
        else {
            bail!("unexpected `git ls-files --stage` line: {entry}");
        };
        if stage != "0" {
            bail!("{path} is unmerged in the index");
        }
        entries.push(Listed { path: path.to_owned(), mode: mode.to_owned(), sha: sha.to_owned() });
    }
    Ok(entries)
}

/// A sync under way, between [`begin`] and [`Syncing::finish`].
#[derive(Debug)]
struct Syncing {
    tree: Utf8PathBuf,
    /// `tree.index`: each vouched path's `<mode>:<sha>`, one `<key> <path>` line each.
    index: Utf8PathBuf,
    /// What `tree.index` vouches for, of what the listing names.
    vouched: BTreeMap<String, String>,
    /// Whether `vouched` differs from `tree.index` on disk.
    dirty: bool,
    /// What the listing names that `tree.index` does not vouch for: what this sync writes.
    stale: Vec<Listed>,
    /// Every submodule the listing names, with whether `tree.index` vouches for its checkout.
    modules: Vec<(Listed, bool)>,
    listing: Vec<pass::Entry>,
    /// How many files the listing no longer names went.
    removed: usize,
}

/// Start syncing the entries `listed` names into `<dir>/tree`: remove what it no longer names
/// (first, so a path that turned from a directory into a file, or back, is free to write), find
/// what `tree.index` does not vouch for, and take that out of `tree.index` before anything is
/// written.
fn begin(dir: &Utf8Path, listed: &std::process::Output) -> Result<Syncing> {
    let tree = dir.join("tree");
    let index = dir.join("tree.index");
    std::fs::create_dir_all(&tree).with_context(|| format!("create {tree}"))?;
    let entries = entries(listed)?;
    let wanted: HashSet<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    let removed = prune(&tree, &tree, &wanted)?;
    let mut vouched = read_index(&index);
    let known = vouched.len();
    vouched.retain(|path, _| wanted.contains(path.as_str()));
    let mut dirty = vouched.len() != known;
    let mut stale = Vec::new();
    let mut modules = Vec::new();
    for entry in &entries {
        let on_disk = std::fs::symlink_metadata(tree.join(&entry.path)).is_ok();
        let mut fresh = on_disk && vouched.get(&entry.path) == Some(&entry.key());
        if entry.is_module() {
            fresh &= own_git_dir(&module_dir(dir, &entry.path)).is_some();
            modules.push((entry.clone(), fresh));
        }
        if !fresh {
            stale.push(entry.clone());
        }
    }
    let mut unvouched = false;
    for entry in &stale {
        unvouched |= vouched.remove(&entry.path).is_some();
    }
    if unvouched {
        write_index(&index, &vouched)?;
        dirty = false;
    }
    let listing = entries
        .into_iter()
        .map(|e| pass::Entry { id: format!("{} {}", e.mode, e.sha), path: e.path })
        .collect();
    Ok(Syncing { tree, index, vouched, dirty, stale, modules, listing, removed })
}

impl Syncing {
    /// Link every submodule's checkout and write the stale files. Returns how many paths it
    /// changed.
    fn write(&self, root: &Utf8Path, dir: &Utf8Path) -> Result<usize> {
        let mut written = 0_usize;
        for (module, fresh) in &self.modules {
            let checkout = submodule_at(root, dir, &module.path, &module.sha, *fresh)?;
            let link = self.tree.join(&module.path);
            if std::fs::read_link(&link).ok().as_deref() != Some(checkout.as_std_path()) {
                if let Some(parent) = link.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let _removed = std::fs::remove_file(&link);
                std::os::unix::fs::symlink(&checkout, &link)
                    .with_context(|| format!("link {link}"))?;
                written = written.saturating_add(1);
            }
        }
        let files: Vec<&Listed> = self.stale.iter().filter(|e| !e.is_module()).collect();
        Ok(written.saturating_add(write_blobs(root, &self.tree, &files)?))
    }

    /// Vouch for what was written. Returns the tree and the listing's entries.
    fn finish(mut self) -> Result<(Utf8PathBuf, Vec<pass::Entry>)> {
        for entry in &self.stale {
            self.vouched.insert(entry.path.clone(), entry.key());
        }
        if self.dirty || !self.stale.is_empty() {
            write_index(&self.index, &self.vouched)?;
        }
        Ok((self.tree, self.listing))
    }
}

/// Where submodule `rel` of `<dir>/tree` is checked out ([`submodule_at`]).
fn module_dir(dir: &Utf8Path, rel: &str) -> Utf8PathBuf {
    dir.join("modules").join(rel.replace('/', "__"))
}

/// What `tree.index` at `path` vouches for: path to `<mode>:<sha>`. None when it is missing.
fn read_index(path: &Utf8Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once(' ').map(|(key, path)| (path.to_owned(), key.to_owned())))
        .collect()
}

/// Replace `tree.index` at `path` whole, through a rename: a sync killed while writing it leaves
/// the one before. Durable before it returns, past a power loss as well as a kill: the new
/// index's bytes and every file pushed to the disk before it ([`push_to_disk`]) go through the
/// drive's cache (`File::sync_all`, `F_FULLFSYNC` on macOS) before the rename, and the rename
/// before any write after it, so the index never vouches for bytes the disk lacks.
fn write_index(path: &Utf8Path, vouched: &BTreeMap<String, String>) -> Result<()> {
    let text = vouched.iter().fold(String::new(), |mut s, (path, key)| {
        s.push_str(key);
        s.push(' ');
        s.push_str(path);
        s.push('\n');
        s
    });
    let part = Utf8PathBuf::from(format!("{path}.part"));
    let mut file = std::fs::File::create(&part).with_context(|| format!("create {part}"))?;
    std::io::Write::write_all(&mut file, text.as_bytes())
        .with_context(|| format!("write {part}"))?;
    file.sync_all().with_context(|| format!("sync {part}"))?;
    std::fs::rename(&part, path).with_context(|| format!("replace {path}"))?;
    let dir = path.parent().context("tree.index's directory")?;
    std::fs::File::open(dir).and_then(|dir| dir.sync_all()).with_context(|| format!("sync {dir}"))
}

/// Push `file`'s bytes to the disk: `fsync(2)`, which on macOS stops at the drive's cache, as
/// [`write_index`]'s full sync then flushes it once for every file written before.
fn push_to_disk(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;
    // SAFETY: `file` stays open through the call, and `fsync` only reads its descriptor.
    let status = unsafe { libc::fsync(file.as_raw_fd()) };
    if status == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

/// A checkout of submodule `rel` at commit `sha`, under `<dir>/modules/`: the working submodule
/// may be mid-bump, and the snapshot must build what the listing records. It is a worktree of
/// the submodule's own repository, so it shares its objects and moving it to another commit
/// rewrites only the files that differ.
///
/// One `tree.index` vouches for (`fresh`, which [`begin`] takes back from one whose git
/// directory is not its own) is moved only when its HEAD is elsewhere. Any other is checked out
/// by force, past the lock a killed checkout leaves, since a checkout killed midway leaves some
/// files moved and HEAD where it was; and made afresh when that fails, or when its git directory
/// is another worktree's (a copy of another lane's directory), whose index and HEAD that
/// worktree moves.
fn submodule_at(
    root: &Utf8Path,
    dir: &Utf8Path,
    rel: &str,
    sha: &str,
    fresh: bool,
) -> Result<Utf8PathBuf> {
    let module = module_dir(dir, rel);
    let sh = Shell::new()?;
    if let Some(git_dir) = own_git_dir(&module) {
        sh.change_dir(&module);
        let head = cmd!(sh, "git rev-parse --verify --quiet HEAD").quiet().ignore_status().read();
        if fresh && head.is_ok_and(|head| head.trim() == sha) {
            return Ok(module);
        }
        let _stale = std::fs::remove_file(git_dir.join("index.lock"));
        if cmd!(sh, "git checkout --quiet --force --detach {sha}").quiet().run().is_ok() {
            return Ok(module);
        }
    }
    if std::fs::symlink_metadata(&module).is_ok() {
        std::fs::remove_dir_all(&module).with_context(|| format!("remove {module}"))?;
    }
    std::fs::create_dir_all(dir.join("modules"))?;
    sh.change_dir(root.join(rel));
    cmd!(sh, "git worktree add --quiet --detach --force {module} {sha}").quiet().run()?;
    Ok(module)
}

/// The git directory of the worktree at `module` when it is that worktree's own: the one its
/// `.git` names, which names it back. A copy of another worktree names that one.
fn own_git_dir(module: &Utf8Path) -> Option<Utf8PathBuf> {
    let dot_git = module.join(".git");
    let named = std::fs::read_to_string(&dot_git).ok()?;
    let git_dir = module.join(named.strip_prefix("gitdir:")?.trim());
    let back = std::fs::read_to_string(git_dir.join("gitdir")).ok()?;
    let back = git_dir.join(back.trim());
    let same = back.canonicalize_utf8().ok()? == dot_git.canonicalize_utf8().ok()?;
    same.then_some(git_dir)
}

/// Write the blobs `fetch` names into `tree`, reading them all through one
/// `git cat-file --batch`. A file whose bytes already match keeps its mtime (and takes its mode
/// when only that changed); a rewritten one is stamped now, because a lane may have built the
/// old bytes after the blob's own time. Returns how many paths changed.
fn write_blobs(root: &Utf8Path, tree: &Utf8Path, fetch: &[&Listed]) -> Result<usize> {
    use std::io::{BufRead as _, Read as _, Write as _};
    use std::os::unix::fs::PermissionsExt as _;
    if fetch.is_empty() {
        return Ok(0);
    }
    let mut child = std::process::Command::new("git")
        .args(["cat-file", "--batch"])
        .current_dir(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .context("spawn git cat-file")?;
    let mut stdin = child.stdin.take().context("git cat-file stdin")?;
    let shas = fetch.iter().fold(String::new(), |mut s, entry| {
        s.push_str(&entry.sha);
        s.push('\n');
        s
    });
    let feeder = std::thread::spawn(move || stdin.write_all(shas.as_bytes()));
    let mut out = std::io::BufReader::new(child.stdout.take().context("git cat-file stdout")?);
    let mut written = 0_usize;
    for entry in fetch {
        let mut header = String::new();
        out.read_line(&mut header)?;
        let size: usize = header
            .split(' ')
            .nth(2)
            .and_then(|s| s.trim().parse().ok())
            .with_context(|| format!("git cat-file header for {}: {header:?}", entry.sha))?;
        let mut blob = vec![0_u8; size];
        out.read_exact(&mut blob)?;
        let mut newline = [0_u8; 1];
        out.read_exact(&mut newline)?;
        let dst = tree.join(&entry.path);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("create {parent}"))?;
        }
        if entry.mode == "120000" {
            let target = std::str::from_utf8(&blob).context("a symlink target is not UTF-8")?;
            if std::fs::read_link(&dst).ok().as_deref() != Some(Utf8Path::new(target).as_std_path())
            {
                let _removed = std::fs::remove_file(&dst);
                std::os::unix::fs::symlink(target, &dst)?;
                written = written.saturating_add(1);
            }
            continue;
        }
        if std::fs::symlink_metadata(&dst).is_ok_and(|m| m.file_type().is_symlink()) {
            let _removed = std::fs::remove_file(&dst);
        }
        let mode = if entry.mode == "100755" { 0o755 } else { 0o644 };
        let same = std::fs::read(&dst).is_ok_and(|old| old == blob);
        if !same {
            let mut file = std::fs::File::create(&dst)?;
            file.write_all(&blob)
                .and_then(|()| push_to_disk(&file))
                .with_context(|| format!("write {}", entry.path))?;
        }
        if !same || std::fs::metadata(&dst)?.permissions().mode() & 0o777 != mode {
            std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(mode))?;
            written = written.saturating_add(1);
        }
    }
    feeder.join().map_err(|panic| anyhow::anyhow!("git cat-file feeder panicked: {panic:?}"))??;
    let status = child.wait()?;
    if !status.success() {
        bail!("git cat-file failed: {status}");
    }
    Ok(written)
}

/// Remove every file under `dir` that the tree no longer lists, and the directories that
/// empties. Returns how many files went.
fn prune(tree: &Utf8Path, dir: &Utf8Path, wanted: &HashSet<&str>) -> Result<usize> {
    let mut removed = 0_usize;
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {dir}"))? {
        let entry = entry?;
        let path = Utf8PathBuf::try_from(entry.path())
            .map_err(|e| anyhow::anyhow!("a snapshot path is not UTF-8: {e}"))?;
        let rel = path.strip_prefix(tree)?.as_str();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            removed = removed.saturating_add(prune(tree, &path, wanted)?);
            if std::fs::read_dir(&path)?.next().is_none() {
                std::fs::remove_dir(&path)?;
            }
        } else if !wanted.contains(rel) {
            std::fs::remove_file(&path).with_context(|| format!("remove {path}"))?;
            removed = removed.saturating_add(1);
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use camino::{Utf8Path, Utf8PathBuf};
    use xshell::{Shell, cmd};

    use super::{Listed, Source, begin, list, list_commit, own_git_dir, sync, write_blobs};
    use crate::tools::repo_root;

    /// `land` snapshots HEAD as the gate snapshots the index: the same entries, in the same form,
    /// for every file the index holds as HEAD has it.
    #[test]
    fn head_is_listed_as_the_index_is() {
        let sh = Shell::new().expect("a shell");
        // `land` runs this inside its HEAD snapshot (`target/gate/tree`), which is no checkout of
        // its own: git answers there for the repository around it, and from that ignored folder
        // `ls-files` would list nothing. The listings are compared at the repository's top.
        sh.change_dir(repo_root().expect("repo root"));
        let top = cmd!(sh, "git rev-parse --show-toplevel").quiet().read().expect("a checkout");
        sh.change_dir(top);
        let entries = |source| {
            let listed = list(&sh, source).expect("git lists it");
            assert!(listed.status.success(), "{source:?}: {listed:?}");
            listed
                .stdout
                .split(|b| *b == 0)
                .filter(|e| !e.is_empty())
                .map(|e| String::from_utf8(e.to_vec()).expect("UTF-8"))
                .collect::<std::collections::BTreeSet<String>>()
        };
        let (head, index) = (entries(Source::Head), entries(Source::Index));
        for entry in &head {
            let (meta, _path) = entry.split_once('\t').expect("a tab before the path");
            let fields: Vec<&str> = meta.split(' ').collect();
            assert!(
                matches!(fields.as_slice(), [mode, sha, "0"] if mode.len() == 6 && sha.len() == 40),
                "{entry}"
            );
        }
        let shared = head.intersection(&index).count();
        let alike = shared.saturating_mul(10) > head.len().saturating_mul(9);
        assert!(alike, "{shared} of {} entries alike", head.len());
    }

    /// A fresh directory for test `name`, gone when it drops.
    struct Scratch(Utf8PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let temp = Utf8PathBuf::try_from(std::env::temp_dir()).expect("a UTF-8 temp dir");
            // Canonical, as git writes a worktree's paths (`/var` is `/private/var` on macOS).
            let temp = temp.canonicalize_utf8().expect("the temp dir");
            let dir = temp.join(format!("xtask-snapshot-{name}-{}", std::process::id()));
            let _gone = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a scratch dir");
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _gone = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A shell in a new repository at `dir`, with no user configuration.
    fn repo(dir: &Utf8Path) -> Shell {
        std::fs::create_dir_all(dir).expect("the repository's dir");
        let sh = Shell::new().expect("a shell");
        sh.change_dir(dir);
        sh.set_var("GIT_CONFIG_GLOBAL", "/dev/null");
        sh.set_var("GIT_CONFIG_NOSYSTEM", "1");
        for (key, value) in [
            ("GIT_AUTHOR_NAME", "test"),
            ("GIT_AUTHOR_EMAIL", "test@example.com"),
            ("GIT_COMMITTER_NAME", "test"),
            ("GIT_COMMITTER_EMAIL", "test@example.com"),
        ] {
            sh.set_var(key, value);
        }
        cmd!(sh, "git init -q -b main").run().expect("init");
        sh
    }

    /// One change to a repository's files.
    enum Change<'a> {
        Write(&'a str, &'a str),
        Executable(&'a str, &'a str),
        Link(&'a str, &'a str),
        Remove(&'a str),
    }

    /// Commit `changes` (and whatever else the work tree holds). Returns the commit.
    fn commit(sh: &Shell, changes: &[Change<'_>]) -> String {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = Utf8PathBuf::try_from(sh.current_dir()).expect("a UTF-8 dir");
        for change in changes {
            match *change {
                Change::Write(path, text) | Change::Executable(path, text) => {
                    let _gone = std::fs::remove_file(dir.join(path));
                    sh.write_file(path, text).expect("write");
                    let mode = if matches!(change, Change::Executable(..)) { 0o755 } else { 0o644 };
                    let permissions = std::fs::Permissions::from_mode(mode);
                    std::fs::set_permissions(dir.join(path), permissions).expect("chmod");
                }
                Change::Link(path, target) => {
                    let _gone = std::fs::remove_file(dir.join(path));
                    std::os::unix::fs::symlink(target, dir.join(path)).expect("link");
                }
                Change::Remove(path) => sh.remove_path(path).expect("remove"),
            }
        }
        cmd!(sh, "git add -A").ignore_stderr().run().expect("add");
        cmd!(sh, "git commit -q --allow-empty -m change").run().expect("commit");
        cmd!(sh, "git rev-parse HEAD").read().expect("the commit")
    }

    /// `<dir>/tree` holds commit `sha` of the repository `sh` is in exactly: every path, its bytes,
    /// its mode and its link, and nothing else; each submodule a clean checkout of the commit
    /// pinned, with a git directory of its own.
    fn assert_exact(sh: &Shell, dir: &Utf8Path, sha: &str) {
        let tree = dir.join("tree");
        let root = Utf8PathBuf::try_from(sh.current_dir()).expect("a UTF-8 dir");
        let modules: Vec<(String, String)> = cmd!(sh, "git ls-tree -r {sha}")
            .read()
            .expect("the commit's tree")
            .lines()
            .filter(|line| line.starts_with("160000 "))
            .map(|line| {
                let (meta, path) = line.split_once('\t').expect("a tab before the path");
                let pinned = meta.split(' ').nth(2).expect("an object");
                (path.to_owned(), pinned.to_owned())
            })
            .collect();
        let index = dir.join("exact.index");
        let _gone = std::fs::remove_file(&index);
        let git = Shell::new().expect("a shell");
        git.change_dir(&tree);
        git.set_var("GIT_DIR", root.join(".git"));
        git.set_var("GIT_WORK_TREE", &tree);
        git.set_var("GIT_INDEX_FILE", &index);
        let mut paths = vec![".".to_owned()];
        paths.extend(modules.iter().map(|(path, _)| format!(":!{path}")));
        let paths = paths.as_slice();
        cmd!(git, "git add --all -- {paths...}").run().expect("the tree, indexed");
        let differ = cmd!(git, "git diff --cached --name-status {sha} -- {paths...}")
            .read()
            .expect("the tree against the commit");
        assert_eq!(differ, "", "{tree} against {sha}");
        for (path, pinned) in modules {
            let checkout = tree.join(&path);
            let module = Shell::new().expect("a shell");
            module.change_dir(&checkout);
            let head = cmd!(module, "git rev-parse HEAD").read().expect("its HEAD");
            assert_eq!(head, pinned, "{path} at the commit pinned");
            let status = cmd!(module, "git status --porcelain").read().expect("its status");
            assert_eq!(status, "", "{path} clean");
            let real = checkout.canonicalize_utf8().expect("the checkout");
            assert!(own_git_dir(&real).is_some(), "{path} has a git directory of its own");
        }
    }

    /// The stale files of a sync from `dir`'s state to `sha`, which it is about to write.
    fn stale(root: &Utf8Path, dir: &Utf8Path, sha: &str) -> Vec<Listed> {
        let listed = list_commit(root, sha).expect("the commit's listing");
        let syncing = begin(dir, &listed).expect("a sync begins");
        syncing.stale.into_iter().filter(|e| !e.is_module()).collect()
    }

    /// The next sync after one killed anywhere writes exactly its commit: killed after any
    /// number of its files (one more torn), back to where it started or on to where it was going.
    /// Every change a commit can make to a path comes and goes: bytes, mode alone, a link's
    /// target, a file into a directory and back, a file added and removed.
    #[test]
    fn a_sync_killed_midway_leaves_the_next_one_exact() {
        let scratch = Scratch::new("killed");
        let (root, dir) = (scratch.0.join("repo"), scratch.0.join("dir"));
        let sh = repo(&root);
        let sync_to = |sha: &str| {
            sync(&root, &dir, &list_commit(&root, sha).expect("a listing"), "a commit")
                .expect("a sync");
            assert_exact(&sh, &dir, sha);
        };
        let a = commit(
            &sh,
            &[
                Change::Write("a.rs", "a1"),
                Change::Write("b.rs", "b1"),
                Change::Write("c/d.rs", "d1"),
                Change::Executable("e.sh", "e1"),
                Change::Link("link", "a.rs"),
                Change::Write("f", "f1"),
                Change::Write("same.rs", "same"),
            ],
        );
        let b = commit(
            &sh,
            &[
                Change::Write("a.rs", "a2"),
                Change::Write("b.rs", "b2"),
                Change::Remove("c"),
                Change::Write("c", "c2"),
                Change::Write("e.sh", "e1"),
                Change::Link("link", "b.rs"),
                Change::Remove("f"),
                Change::Write("f/g.rs", "g2"),
                Change::Write("new.rs", "new"),
            ],
        );
        sync_to(&a);
        sync_to(&b);
        sync_to(&a);
        let count = stale(&root, &dir, &b).len();
        assert_eq!(count, 7, "every path but `same.rs` is written");
        sync_to(&a);
        for then in [&a, &b] {
            for killed_after in 0..=count {
                let stale = stale(&root, &dir, &b);
                let (done, rest) = stale.split_at(killed_after);
                let done: Vec<&Listed> = done.iter().collect();
                write_blobs(&root, &dir.join("tree"), &done).expect("some files written");
                if let Some(torn) = rest.first().filter(|e| e.mode != "120000") {
                    let path = dir.join("tree").join(&torn.path);
                    std::fs::create_dir_all(path.parent().expect("a parent")).expect("its dir");
                    std::fs::write(&path, "torn").expect("a torn file");
                }
                sync_to(then);
                sync_to(&a);
            }
        }
    }

    /// A submodule checkout killed midway (some files moved, HEAD where it was, its lock left)
    /// is made exact by the next sync, either way, in place: a file both commits share stays
    /// the one it was. A copy of a synced directory (a lane's clone of another's) gets a checkout
    /// of its own, so the two move apart.
    #[test]
    fn a_killed_submodule_checkout_is_redone_and_a_copy_s_is_its_own() {
        let scratch = Scratch::new("module");
        let (root, dir) = (scratch.0.join("repo"), scratch.0.join("dir"));
        let sh = repo(&root);
        let module = repo(&root.join("vendor/m"));
        let m1 = commit(&module, &[Change::Write("x", "x1"), Change::Write("kept", "k")]);
        let m2 = commit(&module, &[Change::Write("x", "x2"), Change::Write("y", "y2")]);
        cmd!(module, "git checkout -q --detach {m1}").run().expect("m1");
        let a = commit(&sh, &[Change::Write("top.rs", "a")]);
        cmd!(module, "git checkout -q --detach {m2}").run().expect("m2");
        let b = commit(&sh, &[Change::Write("top.rs", "b")]);
        let sync_to = |dir: &Utf8Path, sha: &str| {
            sync(&root, dir, &list_commit(&root, sha).expect("a listing"), "a commit")
                .expect("a sync");
            assert_exact(&sh, dir, sha);
        };
        let checkout = dir.join("modules/vendor__m");
        let kill_moving_to_b = || {
            let listed = list_commit(&root, &b).expect("a listing");
            drop(begin(&dir, &listed).expect("a sync begins"));
            std::fs::write(checkout.join("x"), "x2").expect("a file moved");
            let git_dir = own_git_dir(&checkout).expect("its git directory");
            std::fs::write(git_dir.join("index.lock"), "").expect("a lock left");
        };
        let kept = || {
            use std::os::unix::fs::MetadataExt as _;
            std::fs::metadata(checkout.join("kept")).expect("a file both commits share").ino()
        };
        sync_to(&dir, &a);
        let first = kept();
        kill_moving_to_b();
        sync_to(&dir, &a);
        kill_moving_to_b();
        sync_to(&dir, &b);
        sync_to(&dir, &a);
        assert_eq!(kept(), first, "redone in place");

        let copy = scratch.0.join("copy");
        cmd!(sh, "cp -R {dir} {copy}").run().expect("a copy");
        sync_to(&copy, &b);
        sync_to(&dir, &a);
        sync_to(&copy, &b);
        let (own, copy_s) = (own_git_dir(&checkout), own_git_dir(&copy.join("modules/vendor__m")));
        assert_ne!(own, copy_s, "each checkout has its own git directory");
    }
}
