//! Which repository a working directory is in, and what it has checked out.
//!
//! The workspace names shells by repository, and the only machine
//! that can answer "which repository is this" is the one the shell runs on. This is that
//! answer: a walk up the directory tree for a `.git` entry, no `git` subprocess and no libgit,
//! so it costs a handful of `stat` calls per `cd` and cannot hang on a lock or an index. The
//! branch is the same kind of answer: `HEAD` read as a file, never `git branch`.
//!
//! Which repository it is *across machines* ([`RepoId`]) is asked once per repository and kept
//! ([`Identities`]): the origin is the config file read, the first commit one `git rev-list` in
//! the background.

pub mod branches;
pub mod bundle;
pub mod cloning;
pub mod commit;
pub mod pull;
pub mod run;
pub mod script;
pub mod setup;
pub mod snapshot;
pub mod verify;
pub mod worktrees;

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use parking_lot::Mutex;
use slopty_core::SessionId;
use slopty_proto::terminal::RepoId;
use tokio::sync::mpsc;

/// The repository root containing `cwd`, if any.
///
/// The root is the nearest ancestor — starting at `cwd` itself — holding a `.git` entry. That
/// entry is a **directory** in an ordinary checkout and a **file** in a worktree or a
/// submodule; the `gitdir:` link inside such a file is deliberately not followed, so a worktree
/// is its own repository and not the checkout it was made from. That is what the client wants:
/// two worktrees of one project are two places to work, not one.
///
/// A repository nested inside another wins over the outer one, because the walk stops at the
/// first entry it meets. `None` when nothing above `cwd` has one, or when `cwd` cannot be
/// resolved — a directory that has since been removed or renamed answers nothing rather than
/// guessing from the stale string.
#[must_use]
pub fn root_of(cwd: &Path) -> Option<PathBuf> {
    // Through symlinks first: the shell reports the path it walked in through, and two shells
    // that reached the same checkout by different names must land in the same block.
    let start = std::fs::canonicalize(cwd).ok()?;
    let mut dir = start.as_path();
    loop {
        // `symlink_metadata`, not `exists`: a `.git` that is a dangling symlink still marks a
        // checkout, and this way a broken link is not silently skipped for its parent's sake.
        if std::fs::symlink_metadata(dir.join(".git")).is_ok() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// [`root_of`] on a path that arrived as a string (OSC 7 gives us one), as a string.
#[must_use]
pub fn root_of_str(cwd: &str) -> Option<String> {
    root_of(Path::new(cwd)).map(|root| root.to_string_lossy().into_owned())
}

/// The `HEAD` file of the repository rooted at `root` (a [`root_of`] answer).
///
/// In an ordinary checkout that is `.git/HEAD`. In a worktree or a submodule `.git` is a file
/// whose `gitdir:` line names the git directory, relative to `root` or absolute, and `HEAD` is
/// in there: each worktree has its own.
#[must_use]
fn head_of(root: &Path) -> Option<PathBuf> {
    Some(git_dir(root)?.join("HEAD"))
}

/// The git directory of the repository rooted at `root`: `.git` itself, or where a worktree's
/// or a submodule's `.git` file links (`gitdir:`, relative to `root` or absolute).
fn git_dir(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if std::fs::metadata(&dot_git).ok()?.is_dir() {
        return Some(dot_git);
    }
    let link = std::fs::read_to_string(&dot_git).ok()?;
    let gitdir = link.lines().next()?.strip_prefix("gitdir:")?.trim();
    (!gitdir.is_empty()).then(|| root.join(gitdir))
}

/// The git directory every worktree of `root`'s repository shares, where its config lives:
/// a worktree's own git directory names it in `commondir`; any other is its own.
pub(crate) fn common_dir(root: &Path) -> Option<PathBuf> {
    let own = git_dir(root)?;
    match std::fs::read_to_string(own.join("commondir")) {
        Ok(common) if !common.trim().is_empty() => Some(own.join(common.trim())),
        _ => Some(own),
    }
}

/// The normalized ([`normalize_origin`]) fetch URL of the repository rooted at `root`: its
/// `origin` remote's, else the first remote's the config names. A config file read, no git.
#[must_use]
pub fn origin_of(root: &Path) -> Option<String> {
    normalize_origin(&origin_url(root)?)
}

/// Where the pull requests of the repository rooted at `root` live, as its `origin`'s host
/// says ([`Forge::of_host`](slopty_proto::git::Forge::of_host)); none for a repository with
/// no remote another machine can see.
#[must_use]
pub fn forge_of(root: &Path) -> Option<slopty_proto::git::Forge> {
    let origin = origin_of(root)?;
    let host = origin.split('/').next()?;
    Some(slopty_proto::git::Forge::of_host(host))
}

/// The fetch URL of the repository rooted at `root` as its config spells it: its `origin`
/// remote's, else the first remote's.
fn origin_url(root: &Path) -> Option<String> {
    let config = std::fs::read_to_string(common_dir(root)?.join("config")).ok()?;
    let mut urls = remote_urls(&config);
    let origin = urls.iter().position(|(name, _)| name == "origin").unwrap_or(0);
    (origin < urls.len()).then(|| urls.swap_remove(origin).1)
}

/// `url` to clone from, with what could be a secret left out.
///
/// That is an HTTP address's user and password (a token often stands in either), and another
/// scheme's password. An SSH user (`git@`) stays, since it names the account the worker's key
/// logs in as. `None` for a path on this machine's own disk ([`normalize_origin`]), which is
/// never cloned onto another.
#[must_use]
pub fn clone_url(url: &str) -> Option<String> {
    let url = url.trim();
    normalize_origin(url)?;
    let Some((scheme, rest)) = url.split_once("://") else { return Some(url.to_owned()) };
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let web = scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https");
    let host = match authority.rsplit_once('@') {
        None => authority.to_owned(),
        Some((_, host)) if web => host.to_owned(),
        Some((user, host)) => {
            let user = user.split_once(':').map_or(user, |(name, _)| name);
            format!("{user}@{host}")
        }
    };
    Some(format!("{scheme}://{host}/{path}"))
}

/// Each `[remote "name"]` section's `url`, in the order the config gives them.
fn remote_urls(config: &str) -> Vec<(String, String)> {
    let mut urls = Vec::new();
    let mut remote: Option<String> = None;
    for line in config.lines().map(str::trim) {
        if let Some(header) = line.strip_prefix('[') {
            let header = header.split(']').next().unwrap_or_default().trim();
            remote = header.split_once(char::is_whitespace).and_then(|(section, name)| {
                let name = name.trim().strip_prefix('"')?.strip_suffix('"')?;
                section.eq_ignore_ascii_case("remote").then(|| name.to_owned())
            });
            continue;
        }
        let Some(name) = &remote else { continue };
        let Some((key, value)) = line.split_once('=') else { continue };
        if key.trim().eq_ignore_ascii_case("url") {
            let value = value.trim();
            let value = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value);
            urls.push((name.clone(), value.to_owned()));
        }
    }
    urls
}

/// A remote URL as the same string on every machine that cloned it, however it was spelled.
///
/// It is `host/path`, the host lowercased, with no scheme, user, port, `.git` or slashes around the
/// path. `https://github.com/o/r.git`, `ssh://git@github.com:22/o/r` and `git@github.com:o/r`
/// are all `github.com/o/r`. `None` for a clone of a local path (`/w/r`, `file:///w/r`,
/// `../r`), which names nothing another machine can see.
#[must_use]
pub fn normalize_origin(url: &str) -> Option<String> {
    let url = url.trim();
    let (authority, path) = match url.split_once("://") {
        Some((scheme, _)) if scheme.eq_ignore_ascii_case("file") => return None,
        Some((_, rest)) => rest.split_once('/')?,
        // `[user@]host:path`, git's scp-like form; a colon after a slash is in a local path.
        None => url.split_once(':').filter(|(authority, _)| !authority.contains('/'))?,
    };
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = match host.rsplit_once(':') {
        Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => host,
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path).trim_end_matches('/');
    (!host.is_empty() && !path.is_empty()).then(|| format!("{}/{path}", host.to_ascii_lowercase()))
}

/// Whether the repository rooted at `root` is a shallow clone, whose oldest commit is where
/// the clone stopped, not where the history began.
fn is_shallow(root: &Path) -> bool {
    common_dir(root).is_some_and(|common| common.join("shallow").exists())
}

/// Which repository the one rooted at `root` is, on any machine.
///
/// That is its origin, and the commit its history began with. The first commit is the root of
/// `HEAD`'s first-parent chain, one every clone shares however far each has come since; a shallow
/// clone and a repository with no commit yet have none.
pub async fn identify(root: PathBuf) -> RepoId {
    let local = {
        let root = root.clone();
        tokio::task::spawn_blocking(move || (origin_url(&root), is_shallow(&root)))
    };
    let (url, shallow) = local.await.unwrap_or((None, true));
    let first = if shallow { None } else { first_commit(&root).await };
    let origin = url.as_deref().and_then(normalize_origin);
    RepoId { origin, root: first, url: url.as_deref().and_then(clone_url) }
}

async fn first_commit(root: &Path) -> Option<String> {
    let git = crate::changes::git()?;
    let args = ["rev-list", "--first-parent", "--max-parents=0", "HEAD"];
    let out = crate::changes::run_git(git, root, &args).await?;
    let first = out.lines().next()?.trim();
    let hash = matches!(first.len(), 40 | 64) && first.bytes().all(|b| b.is_ascii_hexdigit());
    hash.then(|| first.to_ascii_lowercase())
}

/// One identification under way.
pub type Identifying = Pin<Box<dyn Future<Output = RepoId> + Send>>;

/// Identifies a repository; [`identify`] in the daemon, a stand-in in tests.
pub type Identify = Arc<dyn Fn(PathBuf) -> Identifying + Send + Sync>;

/// The daemon's [`Identify`]: [`identify`].
#[must_use]
pub fn git_identify() -> Identify {
    Arc::new(|root| -> Identifying { Box::pin(identify(root)) })
}

/// Every repository a session has been in, identified once.
///
/// A repository is identified the
/// first time a summary asks, in the background; the sessions in it are sent on `moves` when
/// the answer is in, so their summaries go out again. One found without its first commit is
/// asked again ([`AGAIN`]). Forgotten once no session is left that was in it, so a checkout
/// replaced by another clone is identified afresh. Cheap to clone.
#[derive(Clone)]
pub struct Identities {
    known: Arc<Mutex<HashMap<String, Known>>>,
    identify: Identify,
    moves: mpsc::UnboundedSender<SessionId>,
}

impl std::fmt::Debug for Identities {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let known = self.known.lock().len();
        f.debug_struct("Identities").field("known", &known).finish_non_exhaustive()
    }
}

/// How soon a repository identified without a first commit (none made yet, a shallow clone)
/// is asked again, when a summary in it goes out.
pub const AGAIN: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Default)]
struct Known {
    /// `None` until identified, and after when nothing identifies it.
    id: Option<RepoId>,
    /// When the last identification began; `None` before the first.
    began: Option<tokio::time::Instant>,
    done: bool,
    sessions: HashSet<SessionId>,
}

impl Known {
    /// Whether an ask now starts identifying: the first ask, or one [`AGAIN`] after an answer
    /// that lacked the first commit.
    fn due(&self) -> bool {
        match self.began {
            None => true,
            Some(began) => {
                self.done
                    && self.id.as_ref().is_none_or(|id| id.root.is_none())
                    && began.elapsed() >= AGAIN
            }
        }
    }
}

impl Identities {
    /// Identify with `identify`; a session asked about a repository identified since is sent
    /// on `moves`.
    #[must_use]
    pub fn new(identify: Identify, moves: mpsc::UnboundedSender<SessionId>) -> Self {
        Self { known: Arc::default(), identify, moves }
    }

    /// Which repository `repo` (a [`root_of`] answer) is, as far as known, for `session`
    /// in it. The first ask starts identifying it, and must be inside a Tokio runtime.
    #[must_use]
    pub fn get(&self, session: SessionId, repo: &str) -> Option<RepoId> {
        let mut known = self.known.lock();
        let entry = known.entry(repo.to_owned()).or_default();
        entry.sessions.insert(session);
        let id = entry.id.clone();
        let due = entry.due();
        if due {
            entry.began = Some(tokio::time::Instant::now());
            entry.done = false;
        }
        drop(known);
        if due {
            let this = self.clone();
            let repo = repo.to_owned();
            tokio::spawn(async move {
                let id = (this.identify)(PathBuf::from(&repo)).await;
                tracing::debug!(%repo, ?id, "identified the repository");
                this.settle(&repo, id);
            });
        }
        id
    }

    fn settle(&self, repo: &str, id: RepoId) {
        let mut known = self.known.lock();
        let Some(entry) = known.get_mut(repo) else { return };
        let found = (id.origin.is_some() || id.root.is_some()).then_some(id);
        let moved = entry.id != found;
        entry.id = found;
        entry.done = true;
        let announce: Vec<SessionId> =
            if moved { entry.sessions.iter().copied().collect() } else { Vec::new() };
        drop(known);
        for session in announce {
            let _sent = self.moves.send(session);
        }
    }

    /// `session` is gone; a repository no session is left in is forgotten once identified.
    pub fn forget(&self, session: SessionId) {
        self.known.lock().retain(|_, entry| {
            entry.sessions.remove(&session);
            !entry.done || !entry.sessions.is_empty()
        });
    }
}

/// `branch_at` of `head_of`: what the repository rooted at `root` has checked out.
#[must_use]
pub fn branch_of(root: &Path) -> Option<String> {
    branch_at(&head_of(root)?)
}

/// How many hex digits of a detached `HEAD`'s commit name it, as `git`'s default abbreviation.
const SHORT_HASH: usize = 7;

/// What the `HEAD` file at `head` has checked out.
///
/// The branch name (`refs/heads/` dropped), any other ref as written below `refs/`, or the
/// commit abbreviated when `HEAD` is detached. `None` when the file cannot be read or holds
/// neither.
#[must_use]
fn branch_at(head: &Path) -> Option<String> {
    let text = std::fs::read_to_string(head).ok()?;
    let line = text.lines().next()?.trim();
    if let Some(target) = line.strip_prefix("ref:") {
        let target = target.trim();
        let name = target
            .strip_prefix("refs/heads/")
            .or_else(|| target.strip_prefix("refs/"))
            .unwrap_or(target);
        return (!name.is_empty()).then(|| name.to_owned());
    }
    let hash = line.get(..SHORT_HASH)?;
    (line.len() >= 40 && line.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hash.to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// `TempDir` paths go through `/var` → `/private/var` on macOS; compare canonical to
    /// canonical or every assertion here is about symlinks instead of repositories.
    fn real(path: &Path) -> PathBuf {
        fs::canonicalize(path).expect("the temp tree exists")
    }

    #[test]
    fn a_checkout_and_everything_under_it() {
        let tmp = tempfile::tempdir().expect("temp");
        let repo = tmp.path().join("project");
        fs::create_dir_all(repo.join(".git")).expect("mkdir");
        fs::create_dir_all(repo.join("crates/a/src")).expect("mkdir");

        assert_eq!(root_of(&repo), Some(real(&repo)));
        assert_eq!(root_of(&repo.join("crates/a/src")), Some(real(&repo)));
    }

    #[test]
    fn a_worktree_is_its_own_repository() {
        let tmp = tempfile::tempdir().expect("temp");
        let main = tmp.path().join("project");
        let tree = tmp.path().join("project-wt/feature");
        fs::create_dir_all(main.join(".git/worktrees/feature")).expect("mkdir");
        fs::create_dir_all(tree.join("src")).expect("mkdir");
        // What git writes in a worktree: a file, not a directory.
        fs::write(tree.join(".git"), "gitdir: ../../project/.git/worktrees/feature\n")
            .expect("write");

        assert_eq!(root_of(&tree.join("src")), Some(real(&tree)));
        assert_ne!(root_of(&tree.join("src")), root_of(&main), "not the checkout it came from");
    }

    #[test]
    fn the_innermost_repository_wins() {
        let tmp = tempfile::tempdir().expect("temp");
        let outer = tmp.path().join("outer");
        let inner = outer.join("vendor/inner");
        fs::create_dir_all(outer.join(".git")).expect("mkdir");
        fs::create_dir_all(inner.join(".git")).expect("mkdir");
        fs::create_dir_all(inner.join("src")).expect("mkdir");

        assert_eq!(root_of(&inner.join("src")), Some(real(&inner)));
        assert_eq!(root_of(&outer.join("src2")), None, "a path that does not exist");
        assert_eq!(root_of(&outer), Some(real(&outer)));
    }

    #[test]
    fn no_repository_and_no_directory() {
        let tmp = tempfile::tempdir().expect("temp");
        let plain = tmp.path().join("just/a/tree");
        fs::create_dir_all(&plain).expect("mkdir");

        // A temp directory is not in a repository — unless the machine's temp lives in one,
        // which would make every assertion here meaningless, so say so instead of failing oddly.
        assert_eq!(root_of(&plain), root_of(tmp.path()), "the tree above decides");

        let gone = plain.join("removed");
        assert_eq!(root_of(&gone), None, "a directory that is not there answers nothing");
    }

    /// A checkout at `dir` with `head` in its `.git/HEAD`.
    fn checkout(dir: &Path, head: &str) {
        fs::create_dir_all(dir.join(".git")).expect("mkdir");
        fs::write(dir.join(".git/HEAD"), head).expect("write");
    }

    fn branch_in(dir: &Path) -> Option<String> {
        branch_of(&root_of(dir)?)
    }

    #[test]
    fn the_branch_a_checkout_has_out() {
        let tmp = tempfile::tempdir().expect("temp");
        let repo = tmp.path().join("project");
        checkout(&repo, "ref: refs/heads/feature/rows\n");
        fs::create_dir_all(repo.join("src")).expect("mkdir");

        assert_eq!(branch_in(&repo.join("src")).as_deref(), Some("feature/rows"));
        assert_eq!(head_of(&real(&repo)), Some(real(&repo).join(".git/HEAD")));
    }

    #[test]
    fn a_worktree_reads_its_own_head_through_the_gitdir_link() {
        let tmp = tempfile::tempdir().expect("temp");
        let main = tmp.path().join("project");
        checkout(&main, "ref: refs/heads/main\n");
        let gitdir = main.join(".git/worktrees/feature");
        fs::create_dir_all(&gitdir).expect("mkdir");
        fs::write(gitdir.join("HEAD"), "ref: refs/heads/feature\n").expect("write");
        let relative = tmp.path().join("project-wt/feature");
        fs::create_dir_all(&relative).expect("mkdir");
        fs::write(relative.join(".git"), "gitdir: ../../project/.git/worktrees/feature\n")
            .expect("write");
        let absolute = tmp.path().join("elsewhere");
        fs::create_dir_all(&absolute).expect("mkdir");
        fs::write(absolute.join(".git"), format!("gitdir: {}\n", gitdir.display())).expect("write");

        assert_eq!(branch_in(&relative).as_deref(), Some("feature"));
        assert_eq!(branch_in(&absolute).as_deref(), Some("feature"));
        assert_eq!(branch_in(&main).as_deref(), Some("main"), "the main checkout keeps its own");
    }

    #[test]
    fn a_detached_head_is_its_short_hash() {
        let tmp = tempfile::tempdir().expect("temp");
        let repo = tmp.path().join("project");
        checkout(&repo, "4f1c2e9a0b7d3c5e8f6a1b2c3d4e5f60718293a4\n");
        assert_eq!(branch_in(&repo).as_deref(), Some("4f1c2e9"));

        // A SHA-256 repository's hashes are longer; the abbreviation is the same.
        checkout(&repo, &format!("{}\n", "ab".repeat(32)));
        assert_eq!(branch_in(&repo).as_deref(), Some("abababa"));
    }

    #[test]
    fn no_branch_without_a_readable_head() {
        let tmp = tempfile::tempdir().expect("temp");
        let plain = tmp.path().join("plain");
        fs::create_dir_all(&plain).expect("mkdir");
        let bare = tmp.path().join("bare");
        fs::create_dir_all(bare.join(".git")).expect("mkdir");
        let garbled = tmp.path().join("garbled");
        checkout(&garbled, "not a head\n");
        let dangling = tmp.path().join("dangling");
        fs::create_dir_all(&dangling).expect("mkdir");
        fs::write(dangling.join(".git"), "gitdir: nowhere\n").expect("write");

        assert_eq!(head_of(&plain), None, "no .git at all");
        assert_eq!(branch_in(&bare), None, ".git with no HEAD in it");
        assert_eq!(branch_in(&garbled), None, "a HEAD that names nothing");
        assert_eq!(branch_in(&dangling), None, "a gitdir link to nothing");
    }

    /// What the session actor pays when a command ends: the repository found again from the
    /// directory (the walk up and the canonicalisation) and its `HEAD` read. Run with
    /// `cargo nextest run -p slopty-worker --release --run-ignored only place_cost --no-capture`.
    #[test]
    #[ignore = "measurement, run by hand"]
    fn place_cost() {
        let tmp = tempfile::tempdir().expect("temp");
        let repo = tmp.path().join("project");
        checkout(&repo, "ref: refs/heads/main\n");
        let deep = repo.join("crates/slopty-worker/src/orchestrate");
        fs::create_dir_all(&deep).expect("mkdir");
        let cwd = deep.to_str().expect("utf-8 temp path");
        let root = real(&repo);
        let reads = 20_000_u32;

        let started = std::time::Instant::now();
        for _ in 0..reads {
            let found = root_of_str(cwd);
            std::hint::black_box(found.as_deref().map(Path::new).and_then(branch_of));
        }
        let whole = started.elapsed() / reads;
        let started = std::time::Instant::now();
        for _ in 0..reads {
            std::hint::black_box(root_of_str(cwd));
        }
        let walk = started.elapsed() / reads;
        let started = std::time::Instant::now();
        for _ in 0..reads {
            std::hint::black_box(branch_of(&root));
        }
        let head = started.elapsed() / reads;
        eprintln!(
            "place_cost: {} ns for root and branch from a directory four deep, {} ns for the root alone, {} ns for the branch alone",
            whole.as_nanos(),
            walk.as_nanos(),
            head.as_nanos()
        );
    }

    /// The address another worker clones from keeps no secret: an HTTP address loses its user
    /// and password, which a token often stands in, and any other scheme its password; an SSH
    /// account stays, and a local path is no address at all.
    #[test]
    fn an_address_to_clone_from_keeps_no_secret() {
        let url = |u: &str| clone_url(u);
        let plain = Some("https://github.com/o/r.git".to_owned());
        assert_eq!(url("https://ghp_token@github.com/o/r.git"), plain);
        assert_eq!(url("https://me:ghp_token@github.com/o/r.git"), plain);
        assert_eq!(url("  https://github.com/o/r.git "), plain);
        assert_eq!(
            url("ssh://git:secret@host.xz:2222/o/r"),
            Some("ssh://git@host.xz:2222/o/r".into())
        );
        assert_eq!(url("git@github.com:o/r.git"), Some("git@github.com:o/r.git".into()));
        assert_eq!(url("/w/slopty"), None);
        assert_eq!(url("file:///w/slopty"), None);
    }

    /// Every spelling of one remote is one string; a clone of a local path is none.
    #[test]
    fn an_origin_is_the_same_however_it_was_spelled() {
        for url in [
            "https://github.com/aislopware/slopty.git",
            "https://user@GitHub.com/aislopware/slopty/",
            "ssh://git@github.com:22/aislopware/slopty.git",
            "git@github.com:aislopware/slopty.git",
            "github.com:aislopware/slopty",
            "  git://github.com/aislopware/slopty  ",
        ] {
            assert_eq!(
                normalize_origin(url).as_deref(),
                Some("github.com/aislopware/slopty"),
                "{url}"
            );
        }
        assert_eq!(
            normalize_origin("ssh://studio/~/w/slopty").as_deref(),
            Some("studio/~/w/slopty")
        );
        for local in ["/w/slopty", "../slopty", "file:///w/slopty", "./a:b/c", "https://host", ""] {
            assert_eq!(normalize_origin(local), None, "{local}");
        }
    }

    /// The origin remote wins over one listed before it; with none named `origin` the first
    /// remote speaks; a worktree reads the config it shares through `commondir`.
    #[test]
    fn the_origin_is_read_from_the_config_worktrees_share() {
        let tmp = tempfile::tempdir().expect("temp");
        let main = tmp.path().join("project");
        checkout(&main, "ref: refs/heads/main\n");
        let config = "[core]\n\tbare = false\n[remote \"fork\"]\n\turl = git@github.com:me/slopty.git\n\
                      [remote \"origin\"]\n\tURL = \"https://github.com/aislopware/slopty.git\"\n\
                      \tfetch = +refs/heads/*:refs/remotes/origin/*\n[branch \"main\"]\n\tremote = origin\n";
        fs::write(main.join(".git/config"), config).expect("write");
        let gitdir = main.join(".git/worktrees/feature");
        fs::create_dir_all(&gitdir).expect("mkdir");
        fs::write(gitdir.join("commondir"), "../..\n").expect("write");
        let tree = tmp.path().join("feature");
        fs::create_dir_all(&tree).expect("mkdir");
        fs::write(tree.join(".git"), format!("gitdir: {}\n", gitdir.display())).expect("write");

        let origin = Some("github.com/aislopware/slopty".to_owned());
        assert_eq!(origin_of(&main), origin);
        assert_eq!(origin_of(&tree), origin, "the worktree's is its repository's");

        fs::write(
            main.join(".git/config"),
            "[remote \"fork\"]\n\turl = git@github.com:me/slopty\n",
        )
        .expect("write");
        assert_eq!(origin_of(&main).as_deref(), Some("github.com/me/slopty"), "the first remote");
        fs::write(main.join(".git/config"), "[remote \"origin\"]\n\turl = /w/slopty\n")
            .expect("write");
        assert_eq!(origin_of(&main), None, "a local clone's origin names nothing");
        assert_eq!(origin_of(tmp.path()), None, "no repository");
    }

    /// A real repository and a clone of it (a local one, so no origin) share the commit their
    /// history began with, however far each has come; a shallow clone and a repository with no
    /// commit say none.
    #[tokio::test]
    async fn clones_share_their_first_commit() {
        let Some(git) = crate::changes::git() else { return };
        let tmp = tempfile::tempdir().expect("temp");
        let run = |dir: &Path, args: &[&str]| {
            let status = std::process::Command::new(git)
                .arg("-C")
                .arg(dir)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "protocol.file.allow=always",
                ])
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("git runs");
            assert!(status.success(), "git {args:?}");
        };
        let first = tmp.path().join("first");
        fs::create_dir_all(&first).expect("mkdir");
        run(&first, &["init", "-q"]);
        assert_eq!(identify(first.clone()).await, RepoId::default(), "no commit yet");
        for n in ["one", "two"] {
            run(&first, &["commit", "-q", "--allow-empty", "-m", n]);
        }
        let other = tmp.path().join("other");
        run(tmp.path(), &["clone", "-q", "first", "other"]);
        run(&other, &["commit", "-q", "--allow-empty", "-m", "moved on"]);
        run(&first, &["remote", "add", "origin", "git@github.com:aislopware/slopty.git"]);

        let (a, b) = (identify(first.clone()).await, identify(other.clone()).await);
        assert!(a.root.as_ref().is_some_and(|r| r.len() == 40), "a full hash: {a:?}");
        assert_eq!(a.root, b.root);
        assert_eq!(a.origin.as_deref(), Some("github.com/aislopware/slopty"));
        assert_eq!(b.origin, None, "cloned from a local path");
        assert!(a.same(&b), "one repository");

        let url = format!("file://{}", first.display());
        run(tmp.path(), &["clone", "-q", "--depth", "1", &url, "shallow"]);
        assert_eq!(
            identify(tmp.path().join("shallow")).await.root,
            None,
            "where the clone stopped"
        );
    }

    /// A repository is identified once however many sessions ask; each session that asked is
    /// told when the answer is in, and a forgotten one is not.
    #[tokio::test]
    async fn a_repository_is_identified_once_and_its_sessions_told() {
        let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let (gate_tx, gate_rx) = tokio::sync::watch::channel(false);
        let identify: Identify = {
            let calls = Arc::clone(&calls);
            Arc::new(move |_root| -> Identifying {
                calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let mut gate = gate_rx.clone();
                Box::pin(async move {
                    let _open = gate.wait_for(|open| *open).await;
                    RepoId { origin: Some("github.com/o/r".to_owned()), root: None, url: None }
                })
            })
        };
        let (moves, mut moved) = mpsc::unbounded_channel();
        let ids = Identities::new(identify, moves);
        let (a, b, gone) = (SessionId::new(), SessionId::new(), SessionId::new());
        assert_eq!(ids.get(a, "/w/r"), None, "not yet");
        assert_eq!(ids.get(b, "/w/r"), None);
        assert_eq!(ids.get(gone, "/w/r"), None);
        ids.forget(gone);
        gate_tx.send_replace(true);

        let mut told = vec![moved.recv().await.expect("told"), moved.recv().await.expect("told")];
        told.sort();
        let mut want = vec![a, b];
        want.sort();
        assert_eq!(told, want);
        let id = ids.get(a, "/w/r").expect("identified");
        assert_eq!(id.origin.as_deref(), Some("github.com/o/r"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1, "once");
    }

    #[test]
    fn the_string_form_agrees_with_the_path_form() {
        let tmp = tempfile::tempdir().expect("temp");
        let repo = tmp.path().join("project");
        fs::create_dir_all(repo.join(".git")).expect("mkdir");
        let as_str = repo.to_str().expect("utf-8 temp path");

        assert_eq!(root_of_str(as_str), Some(real(&repo).to_string_lossy().into_owned()));
        assert_eq!(root_of_str("/definitely/not/here"), None);
    }
}
