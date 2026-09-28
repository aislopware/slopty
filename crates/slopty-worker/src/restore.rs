//! Sessions kept on disk, so they come back after their shell is lost to a reboot or to ptyd
//! ending (`docs/decisions/terminal.md`, "Sessions come back after a reboot").
//!
//! ptyd keeps shells alive across a worker restart, but not across its own end. For that the
//! worker keeps, per session, a recipe (`<id>.json`: the command, the request's environment,
//! the directory, the title, the size) and the newest checkpoint of its screen and scrollback
//! (`<id>.vt`, the VT bytes ptyd is handed). A worker that finds a recipe ptyd no longer holds
//! reopens the session under the same id, so every workspace item keeps its tile: a new shell
//! in the old directory, below the old screen and a divider. What the old shell was running is
//! never started again.
//!
//! The checkpoints reach the keeper as ptyd gets them, and each session's is written at most
//! every [`KEEP_EVERY`], off the session's thread: a state is megabytes, and the formatter
//! already made one twice a second at most. A worker going down writes what it holds first.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use slopty_core::{SessionId, WallMs};
use slopty_proto::terminal::{Restored, TermSize};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

/// The longest a session's newest checkpoint waits to be written.
///
/// What a crash of the whole machine loses is at most this much of the scrollback; a reboot
/// loses nothing, since the worker writes on its way down.
pub const KEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

/// Where a session stands, as its checkpoints carry it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Place {
    /// The directory its shell last reported (OSC 7), if any.
    pub cwd: Option<String>,
    /// Its title (OSC 0/2), if any.
    pub title: Option<String>,
    /// Its size.
    pub size: TermSize,
}

/// What reopening a session starts from.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Recipe {
    /// What it was opened to run; empty for the login shell.
    pub command: Vec<String>,
    /// The environment its request added (the worker's own is added afresh).
    pub env: Vec<(String, String)>,
    /// The directory its shell was last in, else the one it was opened in.
    pub cwd: Option<String>,
    /// Its last title.
    pub title: Option<String>,
    /// Its last size.
    pub size: TermSize,
    /// When its screen was last written to disk; zero before the first time.
    pub saved_ms: WallMs,
    /// It was itself reopened after a loss, and what it ran before that.
    pub restored: Option<Restored>,
}

impl Recipe {
    /// A recipe for a session ptyd handed over without one: its command and environment are
    /// not known, so it reopens as the login shell.
    const fn adopted(size: TermSize) -> Self {
        Self {
            command: Vec::new(),
            env: Vec::new(),
            cwd: None,
            title: None,
            size,
            saved_ms: WallMs::ZERO,
            restored: None,
        }
    }

    /// What the new shell runs, given the system's shells (`/etc/shells`): the session's own
    /// command when that is a listed shell alone, because a shell is where the human was, not
    /// something they ran; else the login shell. Anything else is a program, and a program the
    /// lost shell was running is never started again unasked.
    #[must_use]
    pub fn reopen_command(&self, shells: &str) -> Vec<String> {
        match self.command.as_slice() {
            [shell] if shells.lines().map(str::trim).any(|listed| listed == shell) => {
                self.command.clone()
            }
            _ => Vec::new(),
        }
    }

    /// What the reopened session's viewers are told, when it runs `reopened`: the command it
    /// ran before, unless that is what runs again (then the one before that, if it too was
    /// reopened).
    #[must_use]
    pub fn restored(&self, reopened: &[String]) -> Restored {
        let command = if self.command == reopened {
            self.restored.as_ref().map(|r| r.command.clone()).unwrap_or_default()
        } else {
            self.command.clone()
        };
        Restored { saved_ms: self.saved_ms, command }
    }
}

/// The system's list of shells, empty where there is none.
#[must_use]
pub fn system_shells() -> String {
    std::fs::read_to_string("/etc/shells").unwrap_or_default()
}

enum Job {
    Opened(SessionId, Recipe),
    Checkpoint(SessionId, Vec<u8>, Place),
    Forget(SessionId),
    Flush(oneshot::Sender<()>),
}

/// The sessions kept on disk. Cheap to clone; its writer runs on a task of its own until
/// every clone is gone.
#[derive(Clone, Debug)]
pub struct Keeper {
    dir: PathBuf,
    jobs: mpsc::UnboundedSender<Job>,
}

impl Keeper {
    /// Keep sessions in `dir` (made 0700, since a scrollback holds whatever was printed), and
    /// read every recipe it holds. A recipe that does not parse is logged and left alone.
    /// Must be called on a tokio runtime.
    ///
    /// # Errors
    ///
    /// The directory cannot be made or read.
    pub fn open(dir: &Path) -> io::Result<(Self, HashMap<SessionId, Recipe>)> {
        use std::os::unix::fs::PermissionsExt as _;

        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        let mut recipes = HashMap::new();
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()?.parse::<SessionId>().ok())
            else {
                continue;
            };
            let parsed = std::fs::read(&path).map_err(|e| e.to_string()).and_then(|bytes| {
                serde_json::from_slice::<Recipe>(&bytes).map_err(|e| e.to_string())
            });
            match parsed {
                Ok(recipe) => {
                    recipes.insert(id, recipe);
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "kept session unreadable");
                }
            }
        }
        let (jobs, rx) = mpsc::unbounded_channel();
        tokio::spawn(write_loop(dir.to_path_buf(), recipes.clone(), rx));
        Ok((Self { dir: dir.to_path_buf(), jobs }, recipes))
    }

    /// Session `id` was opened, or reopened, from `recipe`: written at once.
    pub fn opened(&self, id: SessionId, recipe: Recipe) {
        let _sent = self.jobs.send(Job::Opened(id, recipe));
    }

    /// Session `id`'s newest checkpoint, and where it stands.
    pub fn checkpoint(&self, id: SessionId, state: Vec<u8>, place: Place) {
        let _sent = self.jobs.send(Job::Checkpoint(id, state, place));
    }

    /// Session `id` was closed: nothing of it is kept.
    pub fn forget(&self, id: SessionId) {
        let _sent = self.jobs.send(Job::Forget(id));
    }

    /// Write every checkpoint waiting, and return once they are on disk.
    pub async fn flush(&self) {
        let (reply, done) = oneshot::channel();
        if self.jobs.send(Job::Flush(reply)).is_ok() {
            let _written = done.await;
        }
    }

    /// Session `id`'s kept screen: VT bytes to replay, empty when none was written.
    pub async fn screen(&self, id: SessionId) -> Vec<u8> {
        let path = screen_path(&self.dir, id);
        match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "kept screen unreadable");
                Vec::new()
            }
        }
    }
}

fn recipe_path(dir: &Path, id: SessionId) -> PathBuf {
    dir.join(format!("{id}.json"))
}

fn screen_path(dir: &Path, id: SessionId) -> PathBuf {
    dir.join(format!("{id}.vt"))
}

/// Apply the jobs in order, writing each session's newest checkpoint once [`KEEP_EVERY`] has
/// passed since its last, until every [`Keeper`] is gone; then write what is left.
async fn write_loop(
    dir: PathBuf,
    mut recipes: HashMap<SessionId, Recipe>,
    mut jobs: mpsc::UnboundedReceiver<Job>,
) {
    let mut waiting: HashMap<SessionId, Vec<u8>> = HashMap::new();
    let mut written: HashMap<SessionId, Instant> = HashMap::new();
    loop {
        let due = waiting
            .keys()
            .map(|id| written.get(id).map_or_else(Instant::now, |at| next_write(*at)))
            .min();
        let job = tokio::select! {
            job = jobs.recv() => job,
            () = sleep_until(due) => {
                let now = Instant::now();
                let ready: Vec<SessionId> = waiting
                    .keys()
                    .filter(|id| written.get(id).is_none_or(|at| next_write(*at) <= now))
                    .copied()
                    .collect();
                for id in ready {
                    if let Some(state) = waiting.remove(&id) {
                        write_screen(&dir, &mut recipes, id, state).await;
                        written.insert(id, Instant::now());
                    }
                }
                continue;
            }
        };
        match job {
            None => {
                for (id, state) in std::mem::take(&mut waiting) {
                    write_screen(&dir, &mut recipes, id, state).await;
                }
                return;
            }
            Some(Job::Opened(id, recipe)) => {
                write_recipe(&dir, id, &recipe).await;
                recipes.insert(id, recipe);
            }
            Some(Job::Checkpoint(id, state, place)) => {
                let recipe = recipes.entry(id).or_insert_with(|| Recipe::adopted(place.size));
                // A shell that never says where it is keeps the directory it was opened in.
                if place.cwd.is_some() {
                    recipe.cwd = place.cwd;
                }
                if place.title.is_some() {
                    recipe.title = place.title;
                }
                recipe.size = place.size;
                waiting.insert(id, state);
            }
            Some(Job::Forget(id)) => {
                recipes.remove(&id);
                waiting.remove(&id);
                written.remove(&id);
                let paths = [recipe_path(&dir, id), screen_path(&dir, id)];
                let removed = tokio::task::spawn_blocking(move || {
                    for path in paths {
                        match std::fs::remove_file(&path) {
                            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                                tracing::warn!(path = %path.display(), error = %e, "kept session not removed");
                            }
                            _ => {}
                        }
                    }
                })
                .await;
                if let Err(e) = removed {
                    tracing::warn!(session = %id, error = %e, "kept session not removed");
                }
            }
            Some(Job::Flush(reply)) => {
                for (id, state) in std::mem::take(&mut waiting) {
                    write_screen(&dir, &mut recipes, id, state).await;
                    written.insert(id, Instant::now());
                }
                let _told = reply.send(());
            }
        }
    }
}

/// When a session last written at `at` may be written again.
fn next_write(at: Instant) -> Instant {
    at.checked_add(KEEP_EVERY).unwrap_or(at)
}

async fn sleep_until(due: Option<Instant>) {
    match due {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Write `state` as `id`'s screen, then its recipe with the time: a crash between the two
/// leaves the recipe's time older than the screen, never newer.
async fn write_screen(
    dir: &Path,
    recipes: &mut HashMap<SessionId, Recipe>,
    id: SessionId,
    state: Vec<u8>,
) {
    let Some(recipe) = recipes.get_mut(&id) else { return };
    recipe.saved_ms = WallMs::now();
    let recipe = recipe.clone();
    let path = screen_path(dir, id);
    let written = tokio::task::spawn_blocking(move || {
        let started = std::time::Instant::now();
        let written = slopty_platform::fs::replace(&path, &state);
        tracing::debug!(session = %id, bytes = state.len(), us = started.elapsed().as_micros(), "screen kept");
        written
    })
    .await
    .map_err(io::Error::other)
    .and_then(|written| written);
    match written {
        Ok(()) => write_recipe(dir, id, &recipe).await,
        Err(e) => tracing::warn!(session = %id, error = %e, "screen not kept"),
    }
}

async fn write_recipe(dir: &Path, id: SessionId, recipe: &Recipe) {
    let path = recipe_path(dir, id);
    let written = match serde_json::to_vec(recipe) {
        Ok(bytes) => {
            tokio::task::spawn_blocking(move || slopty_platform::fs::replace(&path, &bytes))
                .await
                .map_err(io::Error::other)
                .and_then(|written| written)
        }
        Err(e) => Err(io::Error::other(e)),
    };
    if let Err(e) = written {
        tracing::warn!(session = %id, error = %e, "recipe not kept");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe(command: &[&str]) -> Recipe {
        Recipe {
            command: command.iter().map(|s| (*s).to_owned()).collect(),
            ..Recipe::adopted(TermSize::default())
        }
    }

    const SHELLS: &str = "# List of acceptable shells\n/bin/bash\n/bin/sh\n/bin/zsh\n";

    /// A tile opened on a shell gets that shell back; anything else, the login shell. The
    /// viewers hear of the command that is not run again, and a second loss remembers it.
    #[test]
    fn only_a_shell_runs_again() {
        assert_eq!(recipe(&["/bin/zsh"]).reopen_command(SHELLS), ["/bin/zsh"]);
        assert!(recipe(&[]).reopen_command(SHELLS).is_empty());
        assert!(recipe(&["claude"]).reopen_command(SHELLS).is_empty());
        assert!(recipe(&["/bin/sh", "-c", "sleep 9"]).reopen_command(SHELLS).is_empty());
        assert!(recipe(&["/opt/fish"]).reopen_command(SHELLS).is_empty(), "not listed");

        let agent = recipe(&["claude", "--continue"]);
        let first = agent.restored(&[]);
        assert_eq!(first.command, ["claude", "--continue"]);
        let reopened = Recipe { restored: Some(first), ..recipe(&[]) };
        assert_eq!(reopened.restored(&[]).command, ["claude", "--continue"], "kept across losses");
        assert!(recipe(&["/bin/zsh"]).restored(&["/bin/zsh".to_owned()]).command.is_empty());
    }

    /// Recipes and screens round-trip through the directory; a checkpoint updates where the
    /// session stands but keeps the directory it was opened in when its shell names none;
    /// forgetting removes both files, and a stray file is skipped.
    #[tokio::test]
    async fn sessions_are_kept_and_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let (keeper, found) = Keeper::open(dir.path()).unwrap();
        assert!(found.is_empty());
        let (a, b) = (SessionId::new(), SessionId::new());
        let opened = Recipe { cwd: Some("/w".to_owned()), ..recipe(&["/bin/sh"]) };
        keeper.opened(a, opened.clone());
        keeper.opened(b, recipe(&[]));
        let size = TermSize { cols: 100, ..TermSize::default() };
        keeper.checkpoint(
            a,
            b"screen-a".to_vec(),
            Place { cwd: None, title: Some("t".to_owned()), size },
        );
        keeper.checkpoint(
            b,
            b"screen-b".to_vec(),
            Place { cwd: Some("/x".to_owned()), title: None, size },
        );
        keeper.flush().await;
        keeper.forget(b);
        keeper.flush().await;
        std::fs::write(dir.path().join("notes.json"), b"{}").unwrap();

        let (again, found) = Keeper::open(dir.path()).unwrap();
        assert_eq!(found.keys().copied().collect::<Vec<_>>(), [a]);
        let kept = &found[&a];
        assert_eq!(kept.cwd.as_deref(), Some("/w"));
        assert_eq!(kept.title.as_deref(), Some("t"));
        assert_eq!(kept.size, size);
        assert!(!kept.saved_ms.is_zero());
        assert_eq!(again.screen(a).await, b"screen-a");
        assert!(again.screen(b).await.is_empty());
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(dir.path()).unwrap().permissions(),
        );
        assert_eq!(mode & 0o777, 0o700);
    }

    /// A burst of checkpoints is one write now and one after [`KEEP_EVERY`], of the newest.
    #[tokio::test(start_paused = true)]
    async fn a_burst_of_checkpoints_is_written_twice() {
        let dir = tempfile::tempdir().unwrap();
        let (keeper, _found) = Keeper::open(dir.path()).unwrap();
        let id = SessionId::new();
        keeper.opened(id, recipe(&[]));
        let place = Place { cwd: None, title: None, size: TermSize::default() };
        keeper.checkpoint(id, b"one".to_vec(), place.clone());
        let screen = screen_path(dir.path(), id);
        until(|| std::fs::read(&screen).is_ok_and(|b| b == b"one")).await;
        keeper.checkpoint(id, b"two".to_vec(), place.clone());
        keeper.checkpoint(id, b"three".to_vec(), place);
        tokio::time::sleep(KEEP_EVERY / 2).await;
        assert_eq!(std::fs::read(&screen).unwrap(), b"one", "held back");
        until(|| std::fs::read(&screen).is_ok_and(|b| b == b"three")).await;
    }

    /// What keeping a full scrollback costs the keeper's writer: a 200-column session holding
    /// all of `SCROLLBACK_LINES`, checkpointed, then written as the keeper writes it. Run with
    /// `cargo nextest run -p slopty-worker --release --run-ignored only keep_cost --no-capture`.
    #[test]
    #[ignore = "measurement, run by hand"]
    fn keep_cost() {
        let lines = crate::manager::SCROLLBACK_LINES;
        let mut engine = slopty_engine::GhosttyEngine::new(slopty_engine::EngineConfig {
            size: TermSize { cols: 200, rows: 50, ..TermSize::default() },
            scrollback_lines: lines,
        })
        .unwrap();
        for i in 0..lines {
            engine.write(
                format!("\x1b[32m{i:>6}\x1b[0m {}\r\n", "lorem ipsum ".repeat(8)).as_bytes(),
            );
        }
        let mut state = Vec::new();
        engine.checkpoint(&mut state).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("screen.vt");
        let rounds = 20_u32;
        let mut worst = std::time::Duration::ZERO;
        let started = std::time::Instant::now();
        for _ in 0..rounds {
            let one = std::time::Instant::now();
            slopty_platform::fs::replace(&path, &state).unwrap();
            worst = worst.max(one.elapsed());
        }
        eprintln!(
            "keep_cost: {} lines, {} KiB state, replace mean {} us, worst {} us",
            lines,
            state.len() / 1024,
            (started.elapsed() / rounds).as_micros(),
            worst.as_micros()
        );
    }

    /// Poll `done` on the paused clock, letting the writer's blocking writes finish.
    async fn until(done: impl Fn() -> bool) {
        for _ in 0..10_000 {
            if done() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("never happened");
    }
}
