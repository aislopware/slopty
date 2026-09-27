//! What each repository's working tree has changed against `HEAD`, for the summaries
//! ([`RepoChanges`]).
//!
//! Counting runs git, which takes milliseconds to seconds on a large tree, so it never runs
//! where a key waits. A session actor only says which repository it is in, at the moments it
//! reads the branch (a directory reported, a command ended), over a channel. One task per
//! repository counts, at most one at a time and no sooner than [`MIN_INTERVAL`] after the last
//! count began; touches meanwhile fold into one more count. When the counts move, the sessions
//! that touched the repository are sent on `moves`, so their summaries go out again.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use parking_lot::Mutex;
use slopty_core::SessionId;
use slopty_proto::terminal::RepoChanges;
use tokio::sync::mpsc;
use tokio::time::Instant;

/// The least time between two counts of one repository.
pub const MIN_INTERVAL: Duration = Duration::from_secs(2);
/// How long one git run may take before the count is given up.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(10);

/// One count under way.
pub type Counting = Pin<Box<dyn Future<Output = Option<RepoChanges>> + Send>>;

/// Counts a repository's changes; [`count`] in the daemon, a stand-in in tests.
pub type Counter = Arc<dyn Fn(PathBuf) -> Counting + Send + Sync>;

/// The counts of every repository a session has been in, and the task keeping them current.
/// Cheap to clone.
#[derive(Clone)]
pub struct Changes {
    repos: Arc<Mutex<HashMap<String, Entry>>>,
    touched: mpsc::UnboundedSender<(SessionId, String)>,
}

impl std::fmt::Debug for Changes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Changes").field("repos", &self.repos.lock().len()).finish_non_exhaustive()
    }
}

#[derive(Default)]
struct Entry {
    value: Option<RepoChanges>,
    sessions: HashSet<SessionId>,
    running: bool,
    again: bool,
    last_start: Option<Instant>,
}

impl Changes {
    /// Start counting with `counter`; a session whose repository's counts moved is sent on
    /// `moves`. Must be called inside a Tokio runtime.
    #[must_use]
    pub fn start(counter: Counter, moves: mpsc::UnboundedSender<SessionId>) -> Self {
        let (touched, rx) = mpsc::unbounded_channel();
        let changes = Self { repos: Arc::default(), touched };
        tokio::spawn(route(Arc::clone(&changes.repos), rx, counter, moves));
        changes
    }

    /// Where a session actor says it is in `repo`: a send, never a wait.
    #[must_use]
    pub fn toucher(&self) -> mpsc::UnboundedSender<(SessionId, String)> {
        self.touched.clone()
    }

    /// The last counts of `repo`, if one finished.
    #[must_use]
    pub fn get(&self, repo: &str) -> Option<RepoChanges> {
        self.repos.lock().get(repo).and_then(|e| e.value)
    }

    /// `session` is gone: no count announces it again.
    pub fn forget(&self, session: SessionId) {
        for entry in self.repos.lock().values_mut() {
            entry.sessions.remove(&session);
        }
    }
}

/// Take touches until every sender is gone, starting a count for a repository not counting
/// already and marking one that is to count once more.
async fn route(
    repos: Arc<Mutex<HashMap<String, Entry>>>,
    mut rx: mpsc::UnboundedReceiver<(SessionId, String)>,
    counter: Counter,
    moves: mpsc::UnboundedSender<SessionId>,
) {
    while let Some((session, repo)) = rx.recv().await {
        tracing::trace!(%session, %repo, "touched");
        if touch(&mut repos.lock(), session, &repo) {
            let task = run(Arc::clone(&repos), repo, Arc::clone(&counter), moves.clone());
            tokio::spawn(task);
        }
    }
}

/// Note that `session` is in `repo`; whether a count must start (none runs for it now).
fn touch(repos: &mut HashMap<String, Entry>, session: SessionId, repo: &str) -> bool {
    let entry = repos.entry(repo.to_owned()).or_default();
    entry.sessions.insert(session);
    if entry.running {
        entry.again = true;
        false
    } else {
        entry.running = true;
        true
    }
}

/// Keep what a count of `repo` found: the sessions to tell when the numbers moved, and whether
/// a touch during it wants one more count. `None` when the repository is no longer known.
fn settle(
    repos: &mut HashMap<String, Entry>,
    repo: &str,
    counted: Option<RepoChanges>,
) -> Option<(Vec<SessionId>, bool)> {
    let entry = repos.get_mut(repo)?;
    let moved = entry.value != counted;
    entry.value = counted;
    let announce = if moved { entry.sessions.iter().copied().collect() } else { Vec::new() };
    entry.running = entry.again;
    Some((announce, entry.again))
}

/// Count `repo` until no touch came in during the last count, each count at least
/// [`MIN_INTERVAL`] after the one before began.
async fn run(
    repos: Arc<Mutex<HashMap<String, Entry>>>,
    repo: String,
    counter: Counter,
    moves: mpsc::UnboundedSender<SessionId>,
) {
    loop {
        let last = repos.lock().get(&repo).and_then(|e| e.last_start);
        if let Some(due) = last.and_then(|at| at.checked_add(MIN_INTERVAL)) {
            tokio::time::sleep_until(due).await;
        }
        if let Some(entry) = repos.lock().get_mut(&repo) {
            entry.last_start = Some(Instant::now());
            entry.again = false;
        }
        let counted = counter(PathBuf::from(&repo)).await;
        tracing::debug!(%repo, ?counted, "counted the working tree's changes");
        let Some((announce, more)) = settle(&mut repos.lock(), &repo, counted) else { return };
        for session in announce {
            let _sent = moves.send(session);
        }
        if !more {
            return;
        }
    }
}

/// The daemon's counter: [`count`] with the git this machine has.
#[must_use]
pub fn git_counter() -> Counter {
    Arc::new(|root| -> Counting { Box::pin(count(root)) })
}

/// What `root`'s working tree changed against `HEAD`.
///
/// `git diff --numstat HEAD` counts the tracked files and `git ls-files --others
/// --exclude-standard` the untracked ones. `None` with no git, no `HEAD` (a repository with no
/// commit yet), or a git that failed or took longer than [`GIT_TIMEOUT`]. Neither run takes the
/// index lock.
pub async fn count(root: PathBuf) -> Option<RepoChanges> {
    let git = git()?;
    let (tracked, untracked) = tokio::join!(
        run_git(git, &root, &["diff", "--numstat", "HEAD"]),
        run_git(git, &root, &["ls-files", "--others", "--exclude-standard", "-z"]),
    );
    let mut changes = parse_numstat(&tracked?);
    let untracked = untracked?.split('\0').filter(|path| !path.is_empty()).count();
    changes.files = changes.files.saturating_add(u32::try_from(untracked).unwrap_or(u32::MAX));
    Some(changes)
}

async fn run_git(git: &Path, root: &Path, args: &[&str]) -> Option<String> {
    let mut command = tokio::process::Command::new(git);
    command
        .arg("-C")
        .arg(root)
        .arg("--no-optional-locks")
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(GIT_TIMEOUT, command.output()).await.ok()?.ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `git diff --numstat` output counted: one line per file, `added\tremoved\tpath`, with `-`
/// for both counts of a binary file.
#[must_use]
pub fn parse_numstat(out: &str) -> RepoChanges {
    let mut changes = RepoChanges::default();
    for line in out.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.splitn(3, '\t');
        let lines = |field: Option<&str>| field.and_then(|n| n.parse::<u32>().ok()).unwrap_or(0);
        let (added, removed) = (lines(fields.next()), lines(fields.next()));
        changes.files = changes.files.saturating_add(1);
        changes.added = changes.added.saturating_add(added);
        changes.removed = changes.removed.saturating_add(removed);
    }
    changes
}

/// The git to run, found once.
///
/// The first on `PATH`, then Homebrew's, then the Command Line Tools' or Xcode's own. Never
/// `/usr/bin/git`: on a Mac without the developer tools that shim puts up the dialog offering to
/// install them, and a daemon must not.
#[must_use]
pub fn git() -> Option<&'static Path> {
    static GIT: OnceLock<Option<PathBuf>> = OnceLock::new();
    GIT.get_or_init(|| {
        let on_path = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
            .unwrap_or_default();
        let known = [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/Library/Developer/CommandLineTools/usr/bin",
            "/Applications/Xcode.app/Contents/Developer/usr/bin",
        ]
        .map(PathBuf::from);
        on_path
            .into_iter()
            .chain(known)
            .filter(|dir| dir != Path::new("/usr/bin"))
            .map(|dir| dir.join("git"))
            .find(|git| git.is_file())
    })
    .as_deref()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    #[test]
    fn numstat_counts_files_and_lines_and_a_binary_as_a_file() {
        let out = "12\t4\tsrc/main.rs\n0\t7\tREADME.md\n-\t-\tlogo.png\n3\t0\told.rs => new.rs\n";
        assert_eq!(parse_numstat(out), RepoChanges { files: 4, added: 15, removed: 11 });
        assert_eq!(parse_numstat(""), RepoChanges::default(), "a clean tree");
    }

    /// Touches while a count runs fold into one more count, begun no sooner than
    /// [`MIN_INTERVAL`] after the first; the sessions that touched hear of a change once each
    /// count moves the numbers, and not when a count finds what the last one did.
    #[tokio::test(start_paused = true)]
    async fn touches_fold_into_one_more_count_and_only_a_change_is_announced() {
        let runs = Arc::new(AtomicU32::new(0));
        let seen = Arc::clone(&runs);
        let counter: Counter = Arc::new(move |_root| -> Counting {
            let run = seen.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                Some(RepoChanges { files: 1, added: run.min(1), removed: 0 })
            })
        });
        let (moves, mut moved) = mpsc::unbounded_channel();
        let changes = Changes::start(counter, moves);
        let (a, b) = (SessionId::new(), SessionId::new());
        let touch = changes.toucher();
        for _ in 0..10 {
            touch.send((a, "/w/repo".to_owned())).unwrap();
            touch.send((b, "/w/repo".to_owned())).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        for _ in 0..10 {
            touch.send((a, "/w/repo".to_owned())).unwrap();
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 2, "one count, then one for every later touch");
        assert_eq!(changes.get("/w/repo"), Some(RepoChanges { files: 1, added: 1, removed: 0 }));
        let mut heard = Vec::new();
        while let Ok(session) = moved.try_recv() {
            heard.push(session);
        }
        heard.sort_unstable();
        let mut both = vec![a, b, a, b];
        both.sort_unstable();
        assert_eq!(heard, both, "both counts moved the numbers: both sessions, twice");

        touch.send((b, "/w/repo".to_owned())).unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 3);
        assert!(moved.try_recv().is_err(), "the same counts again announce nothing");

        changes.forget(a);
        assert_eq!(changes.get("/w/other"), None, "a repository never counted");
    }

    /// What one count costs: `SLOPTY_COUNT_REPO` (this repository by default), five counts
    /// after a warm-up, the fastest and the slowest.
    #[tokio::test]
    #[ignore = "timing, run by hand with --ignored --nocapture"]
    async fn counting_a_repository_costs() {
        let root = std::env::var_os("SLOPTY_COUNT_REPO")
            .map_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."), PathBuf::from);
        let warm = count(root.clone()).await;
        let mut took = Vec::new();
        for _ in 0..5 {
            let at = std::time::Instant::now();
            assert_eq!(count(root.clone()).await, warm);
            took.push(at.elapsed());
        }
        took.sort_unstable();
        eprintln!(
            "count {}: {warm:?}, fastest {:?}, slowest {:?}",
            root.display(),
            took.first().copied().unwrap_or_default(),
            took.last().copied().unwrap_or_default()
        );
    }

    /// A real repository: an edited tracked file, a new untracked one and an ignored one; a
    /// repository with no commit gives nothing.
    #[tokio::test]
    async fn a_working_tree_is_counted_against_head() {
        let Some(git) = git() else { return };
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let run = |args: &[&str]| {
            let status = std::process::Command::new(git)
                .arg("-C")
                .arg(repo)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        run(&["init", "-q"]);
        assert_eq!(count(repo.to_path_buf()).await, None, "no HEAD yet");
        std::fs::write(repo.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(repo.join(".gitignore"), "target/\n").unwrap();
        run(&["add", "."]);
        run(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "first"]);
        assert_eq!(count(repo.to_path_buf()).await, Some(RepoChanges::default()), "clean");
        std::fs::write(repo.join("a.txt"), "one\n2\nthree\nfour\n").unwrap();
        std::fs::write(repo.join("new.txt"), "fresh\n").unwrap();
        std::fs::create_dir_all(repo.join("target")).unwrap();
        std::fs::write(repo.join("target/out"), "ignored\n").unwrap();
        assert_eq!(
            count(repo.to_path_buf()).await,
            Some(RepoChanges { files: 2, added: 2, removed: 1 })
        );
    }
}
