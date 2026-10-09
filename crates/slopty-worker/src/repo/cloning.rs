//! Clones made for tasks placed on a worker that has none of their repository.
//!
//! A clone is made from the repository's origin with the worker's own git and credentials, and
//! nothing else: no token is passed in or kept. It goes to one place per origin under the
//! worker's home ([`place`]), so a second task of the repository finds it there. It is made
//! beside that place under a name of its own and moved in only once git finished, so a clone
//! that fails, times out or is cut short leaves nothing a later one could take for a clone.
//! At most [`AT_ONCE`] run at a time on one worker; one waiting for its turn says so.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use slopty_proto::terminal::RepoId;
use tokio::io::AsyncReadExt as _;
use tokio::sync::Semaphore;

use super::{identify, normalize_origin, root_of};

/// How many clones one worker makes at once.
pub const AT_ONCE: usize = 2;
/// How long one clone may take before it is given up, its wait for a turn included.
pub const TIMEOUT: Duration = Duration::from_mins(30);
/// The most bytes of git's own words kept to say why a clone failed.
const SAID_MAX: usize = 4096;

/// How far a clone has come, as git says it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Progress {
    /// What git is doing (`Receiving objects`), or waiting its turn.
    pub phase: String,
    /// How far that is, when git says.
    pub percent: Option<u8>,
}

/// The folder the clones the server asks for go under: `~/slopty/clones`.
#[must_use]
pub fn clones_root(home: &Path) -> PathBuf {
    home.join("slopty").join("clones")
}

/// Where the clone of `origin` (a [`normalize_origin`] answer, `host/path`) goes under
/// `home`: `host/path` under [`clones_root`]. `None` for an origin with a part that would climb
/// out of that place.
#[must_use]
pub fn place(home: &Path, origin: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = origin.split('/').collect();
    let unsafe_part = |p: &&str| p.is_empty() || *p == "." || *p == ".." || p.contains('\\');
    if parts.iter().any(unsafe_part) {
        return None;
    }
    Some(parts.iter().fold(clones_root(home), |dir, part| dir.join(part)))
}

/// Trust the clone at `path` for Claude Code ([`slopty_agent::trust`]), since Slopty made it.
///
/// An agent in a folder nobody trusted waits at a dialog with no hook running. Kept only
/// inside [`clones_root`]. A trust that cannot be kept (Claude Code never ran for this person,
/// a config it did not write) leaves the dialog to the person.
pub fn trust(home: &Path, path: &Path) {
    let config = slopty_agent::trust::this_config_path();
    match slopty_agent::trust::trust(&config, home, path, &clones_root(home)) {
        Ok(_) => {}
        Err(e) => tracing::info!(path = %path.display(), error = %e, "a clone not trusted"),
    }
}

/// The clones under way on a worker: their turns, and one at a time per place. Cheap to clone.
#[derive(Clone)]
pub struct Cloner {
    turns: Arc<Semaphore>,
    places: Arc<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>>,
}

impl Default for Cloner {
    fn default() -> Self {
        Self { turns: Arc::new(Semaphore::new(AT_ONCE)), places: Arc::default() }
    }
}

impl std::fmt::Debug for Cloner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cloner")
            .field("free", &self.turns.available_permits())
            .field("places", &self.places.lock().len())
            .finish()
    }
}

/// Why there is no clone.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Missed {
    /// Not tried: the address names no remote, the place holds something else, or it is no
    /// place to clone into.
    Refused(String),
    /// git failed, timed out or could not start, in its words. Nothing is left behind.
    Failed(String),
}

impl std::fmt::Display for Missed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(why) | Self::Failed(why) => f.write_str(why),
        }
    }
}

impl Cloner {
    /// Clone `url` with `git` into its place under `home`, telling `progress` how it goes:
    /// where the clone is and which repository it is. A clone of the same origin there already
    /// is answered as it is.
    ///
    /// # Errors
    /// Why there is no clone, in words: `url` names no remote, something else is in its place,
    /// or git failed, timed out or could not start. Nothing is left behind.
    pub async fn clone_repo(
        &self,
        git: &Path,
        url: &str,
        home: &Path,
        progress: impl Fn(Progress),
    ) -> Result<(PathBuf, RepoId), String> {
        let origin = origin_of(url).map_err(|m| m.to_string())?;
        let dest =
            place(home, &origin).ok_or_else(|| format!("{origin} is no place to clone into"))?;
        self.clone_at(git, url, &origin, dest, progress).await.map_err(|m| m.to_string())
    }

    /// Clone `url` with `git` into `into`, where the person asked for it, telling `progress` how
    /// it goes. A clone of the same origin there already is answered as it is.
    ///
    /// # Errors
    /// [`Missed::Refused`] for a `url` that names no remote, an `into` that is not absolute, or
    /// one that holds anything else; [`Missed::Failed`] when git failed, timed out or could not
    /// start. Nothing is left behind.
    pub async fn clone_into(
        &self,
        git: &Path,
        url: &str,
        into: &Path,
        progress: impl Fn(Progress),
    ) -> Result<(PathBuf, RepoId), Missed> {
        let origin = origin_of(url)?;
        let named = into.file_name().is_some_and(|n| n != "." && n != "..");
        if !into.is_absolute() || !named {
            return Err(Missed::Refused(format!("{} is no place to clone into", into.display())));
        }
        self.clone_at(git, url, &origin, into.to_path_buf(), progress).await
    }

    /// Clone `url`, of `origin`, into `dest`, one at a time per place and [`AT_ONCE`] at most.
    async fn clone_at(
        &self,
        git: &Path,
        url: &str,
        origin: &str,
        dest: PathBuf,
        progress: impl Fn(Progress),
    ) -> Result<(PathBuf, RepoId), Missed> {
        let held = Arc::clone(self.places.lock().entry(dest.clone()).or_default());
        let _place = held.lock().await;

        if dest.exists() {
            return existing(&dest, origin).await.map_err(Missed::Refused);
        }
        let now = tokio::time::Instant::now();
        let deadline = now.checked_add(TIMEOUT).unwrap_or(now);
        let _turn = if let Ok(turn) = Arc::clone(&self.turns).try_acquire_owned() {
            turn
        } else {
            progress(Progress { phase: "Waiting for another clone".to_owned(), percent: None });
            let turn = Arc::clone(&self.turns).acquire_owned();
            tokio::time::timeout_at(deadline, turn)
                .await
                .map_err(|_elapsed| Missed::Failed(too_long()))?
                .map_err(|e| Missed::Failed(e.to_string()))?
        };
        let parent = dest
            .parent()
            .ok_or_else(|| Missed::Refused(format!("{} has no parent", dest.display())))?;
        let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| Missed::Failed(format!("{}: {e}", parent.display())))?;
        // Whatever a worker that stopped mid-clone left beside the place is nobody's now: this
        // one holds the place.
        sweep_partials(parent, &name).await;
        let partial = parent.join(format!(".{name}.partial"));

        let cloned = run_clone(git, url, &partial, &progress, deadline).await;
        let moved = match cloned {
            Ok(()) => tokio::fs::rename(&partial, &dest).await.map_err(|e| e.to_string()),
            Err(why) => Err(why),
        };
        if let Err(why) = moved {
            remove(&partial).await;
            return Err(Missed::Failed(why));
        }
        let id = identify(dest.clone()).await;
        Ok((dest, id))
    }
}

/// The origin `url` names, normalized, or why it names none.
fn origin_of(url: &str) -> Result<String, Missed> {
    normalize_origin(url).ok_or_else(|| {
        Missed::Refused(format!(
            "{url} names no remote to clone from, and nothing is cloned without one"
        ))
    })
}

/// What is in a clone's place already: the clone, when it is one of `origin`.
async fn existing(dest: &Path, origin: &str) -> Result<(PathBuf, RepoId), String> {
    let real = std::fs::canonicalize(dest).ok();
    let is_root = real.is_some() && root_of(dest) == real;
    if is_root {
        let id = identify(dest.to_path_buf()).await;
        if id.origin.as_deref() == Some(origin) {
            return Ok((dest.to_path_buf(), id));
        }
    }
    Err(format!("{} is there already and is not a clone of {origin}", dest.display()))
}

/// `git clone --progress url into`, reading git's progress as it goes. Git asks nothing: no
/// terminal prompt, no input.
async fn run_clone(
    git: &Path,
    url: &str,
    into: &Path,
    progress: &impl Fn(Progress),
    deadline: tokio::time::Instant,
) -> Result<(), String> {
    let mut child = tokio::process::Command::new(git)
        .args(["clone", "--progress", "--", url])
        .arg(into)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("git did not start: {e}"))?;
    let mut stderr = child.stderr.take().ok_or("git's error stream")?;
    let read = async {
        let mut said = Said::default();
        let mut buf = [0_u8; 8192];
        loop {
            let n = stderr.read(&mut buf).await.map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            for line in said.take(buf.get(..n).unwrap_or_default()) {
                if let Some(now) = parse_progress(&line) {
                    if said.last.as_ref() != Some(&now) {
                        progress(now.clone());
                        said.last = Some(now);
                    }
                } else if !line.trim().is_empty() {
                    said.note(&line);
                }
            }
        }
        let status = child.wait().await.map_err(|e| e.to_string())?;
        if status.success() { Ok(()) } else { Err(said.why()) }
    };
    tokio::time::timeout_at(deadline, read).await.unwrap_or_else(|_| Err(too_long()))
}

fn too_long() -> String {
    format!("the clone took longer than {} minutes", TIMEOUT.as_secs() / 60)
}

/// What git wrote to its error stream, a line at a time: lines end in `\n`, progress redraws
/// end in `\r`.
#[derive(Default)]
struct Said {
    pending: Vec<u8>,
    /// Its last lines that were not progress, for the reason it failed.
    words: String,
    last: Option<Progress>,
}

impl Said {
    fn take(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        for &b in bytes {
            if b == b'\n' || b == b'\r' {
                lines.push(String::from_utf8_lossy(&self.pending).into_owned());
                self.pending.clear();
            } else {
                self.pending.push(b);
            }
        }
        lines
    }

    fn note(&mut self, line: &str) {
        self.words.push_str(line.trim());
        self.words.push('\n');
        if self.words.len() > SAID_MAX {
            let cut = self.words.len().saturating_sub(SAID_MAX);
            let cut = (cut..self.words.len()).find(|&i| self.words.is_char_boundary(i));
            self.words = self.words.split_off(cut.unwrap_or(0));
        }
    }

    /// Why git failed: its `fatal:` line when it said one, else its last words.
    fn why(&self) -> String {
        let lines: Vec<&str> = self.words.lines().collect();
        let fatal = lines.iter().rev().find(|l| l.starts_with("fatal:") || l.starts_with("error:"));
        fatal.or_else(|| lines.last()).map_or_else(|| "git failed".to_owned(), |l| (*l).to_owned())
    }
}

/// A progress line of git's (`Receiving objects:  45% (450/1000), 1.20 MiB | 2.00 MiB/s`): its
/// phase and percent. `remote: ` lines are the server's own count, and count too.
fn parse_progress(line: &str) -> Option<Progress> {
    let line = line.trim().strip_prefix("remote:").map_or_else(|| line.trim(), str::trim);
    let (phase, rest) = line.split_once(':')?;
    let digits: String = rest.trim_start().chars().take_while(char::is_ascii_digit).collect();
    let after = rest.trim_start().get(digits.len()..)?;
    if digits.is_empty() || !after.starts_with('%') {
        return None;
    }
    let percent = digits.parse::<u8>().ok().filter(|p| *p <= 100)?;
    Some(Progress { phase: phase.trim().to_owned(), percent: Some(percent) })
}

/// Drop the partial clones of `name` left beside it.
async fn sweep_partials(parent: &Path, name: &str) {
    let prefix = format!(".{name}.partial");
    let Ok(mut entries) = tokio::fs::read_dir(parent).await else { return };
    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            remove(&entry.path()).await;
        }
    }
}

async fn remove(path: &Path) {
    if let Err(e) = tokio::fs::remove_dir_all(path).await
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), error = %e, "a failed clone not removed");
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn git_in(dir: &Path, args: &[&str]) {
        let git = crate::changes::git().expect("git");
        let ok = Command::new(git)
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git runs")
            .success();
        assert!(ok, "git {args:?}");
    }

    /// A `git` that reads `config` as the person's own and no other: a worker's git, with
    /// nothing of the developer's.
    fn git_with(dir: &Path, config: &str) -> PathBuf {
        let git = crate::changes::git().expect("git");
        let at = dir.join("gitconfig");
        std::fs::write(&at, config).expect("write");
        let wrapper = dir.join("git");
        let script = format!(
            "#!/bin/sh\nGIT_CONFIG_GLOBAL='{}' GIT_CONFIG_SYSTEM=/dev/null exec '{}' \"$@\"\n",
            at.display(),
            git.display()
        );
        std::fs::write(&wrapper, script).expect("write");
        std::fs::set_permissions(&wrapper, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");
        wrapper
    }

    #[test]
    fn git_s_progress_lines_are_read_and_others_are_not() {
        let read = |l: &str| parse_progress(l).map(|p| (p.phase, p.percent));
        assert_eq!(
            read("Receiving objects:  45% (450/1000), 1.20 MiB | 2.00 MiB/s"),
            Some(("Receiving objects".to_owned(), Some(45)))
        );
        assert_eq!(
            read("remote: Counting objects: 100% (12/12), done."),
            Some(("Counting objects".to_owned(), Some(100)))
        );
        assert_eq!(read("Cloning into '/w/r.partial'..."), None);
        assert_eq!(read("fatal: repository 'x' not found"), None);
        assert_eq!(read("Resolving deltas: 400% nonsense"), None);
    }

    #[test]
    fn a_clone_goes_to_its_origin_s_place_and_never_out_of_it() {
        let home = Path::new("/home/c");
        assert_eq!(
            place(home, "github.com/aislopware/slopty"),
            Some(PathBuf::from("/home/c/slopty/clones/github.com/aislopware/slopty"))
        );
        assert_eq!(place(home, "evil.com/../../etc"), None);
        assert_eq!(place(home, "host//x"), None);
    }

    /// No remote, no clone: a path on the worker's own disk is refused before git runs, and
    /// git's own failure says why and leaves nothing beside the place.
    #[tokio::test]
    async fn a_clone_without_an_origin_or_that_fails_leaves_nothing() {
        if crate::changes::git().is_none() {
            return;
        }
        let home = tempfile::tempdir().expect("temp");
        let git = git_with(home.path(), "");
        let cloner = Cloner::default();
        let said = cloner.clone_repo(&git, "/w/slopty", home.path(), |_| {}).await;
        assert!(said.is_err_and(|why| why.contains("no remote")));

        // A remote that is not there: git runs and fails, quickly and without asking.
        let url = "https://127.0.0.1:9/nobody/nothing.git";
        let failed = cloner.clone_repo(&git, url, home.path(), |_| {}).await;
        let why = failed.expect_err("nothing to clone");
        assert!(!why.is_empty(), "git says why");
        let parent = home.path().join("slopty/clones/127.0.0.1/nobody");
        let left: Vec<_> =
            std::fs::read_dir(&parent).map(|d| d.flatten().count()).into_iter().collect();
        assert_eq!(left, [0], "no partial clone left in {}", parent.display());
    }

    /// A real clone from a remote git reaches through `insteadOf` (the person's own git
    /// config, as on a worker): it lands in its place with its identity, says how far it is
    /// as it goes, and a second ask finds it there.
    #[tokio::test]
    async fn a_clone_is_made_once_and_found_again() {
        if crate::changes::git().is_none() {
            return;
        }
        let tmp = tempfile::tempdir().expect("temp");
        let source = tmp.path().join("source");
        std::fs::create_dir_all(&source).expect("mkdir");
        git_in(&source, &["init", "-q"]);
        for n in 0..3 {
            std::fs::write(source.join(format!("f{n}")), "x".repeat(10_000)).expect("write");
            git_in(&source, &["add", "."]);
            git_in(&source, &["commit", "-q", "-m", "c"]);
        }
        // The worker's own git config maps the forge address to the source, as a person's
        // `url.<base>.insteadOf` would.
        let map = format!(
            "[url \"file://{}\"]\n\tinsteadOf = https://example.com/o/r.git\n",
            source.display()
        );
        let wrapper = git_with(tmp.path(), &map);

        let home = tmp.path().join("home");
        let cloner = Cloner::default();
        let seen = Mutex::new(Vec::new());
        let (path, id) = cloner
            .clone_repo(&wrapper, "https://example.com/o/r.git", &home, |p| seen.lock().push(p))
            .await
            .expect("cloned");
        assert_eq!(path, home.join("slopty/clones/example.com/o/r"));
        assert!(path.join("f2").exists());
        assert_eq!(id.origin.as_deref(), Some("example.com/o/r"));
        assert!(id.root.is_some(), "{id:?}");
        assert!(seen.lock().iter().any(|p| p.percent == Some(100)), "{:?}", seen.lock());

        let again = cloner.clone_repo(&wrapper, "https://example.com/o/r", &home, |_| {}).await;
        assert_eq!(again.map(|(p, _)| p), Ok(path.clone()), "found, not cloned again");
        std::fs::write(home.join("slopty/clones/example.com/o/stray"), "x").expect("write");
        let stray = cloner.clone_repo(&wrapper, "https://example.com/o/stray", &home, |_| {}).await;
        assert!(stray.is_err_and(|why| why.contains("not a clone")));
    }

    /// A clone the person asks for goes where they said: made there, telling how far it is, and
    /// found there on a second ask. A place that is not absolute, or that holds anything else, is
    /// refused before git runs; a remote that is not there fails in git's words.
    #[tokio::test]
    async fn a_clone_goes_where_the_person_asks() {
        if crate::changes::git().is_none() {
            return;
        }
        let tmp = tempfile::tempdir().expect("temp");
        let source = tmp.path().join("source");
        std::fs::create_dir_all(&source).expect("mkdir");
        git_in(&source, &["init", "-q"]);
        std::fs::write(source.join("f"), "x".repeat(10_000)).expect("write");
        git_in(&source, &["add", "."]);
        git_in(&source, &["commit", "-q", "-m", "c"]);
        let map = format!(
            "[url \"file://{}\"]\n\tinsteadOf = https://example.com/o/r.git\n",
            source.display()
        );
        let wrapper = git_with(tmp.path(), &map);
        let url = "https://example.com/o/r.git";
        let into = tmp.path().join("home/work/r");
        let cloner = Cloner::default();

        let seen = Mutex::new(Vec::new());
        let (path, id) =
            cloner.clone_into(&wrapper, url, &into, |p| seen.lock().push(p)).await.expect("cloned");
        assert_eq!(path, into);
        assert!(into.join("f").exists());
        assert_eq!(id.origin.as_deref(), Some("example.com/o/r"));
        assert!(seen.lock().iter().any(|p| p.percent == Some(100)), "{:?}", seen.lock());
        let again = cloner.clone_into(&wrapper, url, &into, |_| {}).await;
        assert_eq!(again.map(|(p, _)| p), Ok(into.clone()), "found, not cloned again");

        let relative = cloner.clone_into(&wrapper, url, Path::new("work/r"), |_| {}).await;
        assert!(matches!(&relative, Err(Missed::Refused(why)) if why.contains("no place")));
        let stray = tmp.path().join("home/stray");
        std::fs::write(&stray, "x").expect("write");
        let held = cloner.clone_into(&wrapper, url, &stray, |_| {}).await;
        assert!(matches!(&held, Err(Missed::Refused(why)) if why.contains("not a clone")));

        let gone = "https://127.0.0.1:9/nobody/nothing.git";
        let nothing = tmp.path().join("home/nothing");
        let failed = cloner.clone_into(&wrapper, gone, &nothing, |_| {}).await;
        assert!(matches!(failed, Err(Missed::Failed(why)) if !why.is_empty()));
        let left: Vec<String> = std::fs::read_dir(tmp.path().join("home"))
            .expect("home")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("nothing"))
            .collect();
        assert_eq!(left, Vec::<String>::new(), "nothing left of the failed clone");
    }
}
