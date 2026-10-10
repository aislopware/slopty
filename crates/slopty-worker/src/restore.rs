//! Sessions kept on disk, so they come back after their shell is lost to a reboot or to ptyd
//! ending (`docs/decisions/terminal.md`, "Sessions come back after a reboot").
//!
//! ptyd keeps shells alive across a worker restart, and across its own update (it runs the new
//! build in place) or its own crash (the worker hands the masters it holds back to the ptyd
//! that starts again). Not across a reboot, or a ptyd that ends while no worker holds its
//! sessions. For that the worker keeps, per session, a recipe (`<id>.json`: the command, the
//! request's environment, the directory, the title, the size) and the newest checkpoint of its
//! screen and scrollback (`<id>.vt`, the VT bytes ptyd is handed). A worker that finds a recipe
//! ptyd no longer holds reopens the session under the same id, so every workspace item keeps its
//! tile: a new shell in the old directory, below the old screen and a divider. What the old shell
//! was running is never started again, with one exception: a Claude Code conversation the person
//! left running comes back with `claude --resume` (`docs/decisions/claude-code.md`, "An agent comes
//! back after a reboot"). The daemon's agent tick tells the keeper which conversation each session
//! holds ([`Keeper::agent`]); a tile opened on `claude` runs it again resumed, and a shell the
//! person typed `claude` into gets the resuming line typed at its first prompt.
//! A conversation Claude Code still runs in the background (`claude --bg`, in its session
//! registry) is opened with `claude attach <id>` instead: `--resume` refuses it while it runs.
//!
//! The checkpoints reach the keeper as ptyd gets them, and each session's is written at most
//! every [`KEEP_EVERY`], off the session's thread: a state is megabytes, and the formatter
//! already made one twice a second at most. A worker going down writes what it holds first.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use slopty_agent::resume::Resume;
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
    /// The Claude Code conversation running in it, to resume.
    pub agent: Option<Resume>,
}

/// What an agent the worker resumes is given besides its own flags.
#[derive(Clone, Debug, Default)]
pub struct AgentLaunch {
    /// The `slopty hook` relay, given again to an agent started with it on its `--settings`.
    pub relay: Option<String>,
    /// Slopty's Claude Code mod, loaded into an agent the worker starts itself. One typed at
    /// a prompt gets it from the shell's `claude` function.
    pub claude_mod: Option<slopty_agent::claude_mod::Installed>,
    /// The conversations Claude Code runs in the background now ([`background`]): one of them is
    /// opened with `claude attach`, since `claude --resume` refuses it while it runs.
    pub background: Vec<String>,
}

/// How a lost session starts again ([`Recipe::reopen`]).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Reopen {
    /// What its PTY runs.
    pub command: Vec<String>,
    /// Where: the agent's directory when one is resumed, else the shell's last. `None` when
    /// that is gone, which leaves the shell at home.
    pub cwd: Option<PathBuf>,
    /// The line that resumes the agent, to type at the new shell's first prompt, when the
    /// agent ran in the shell rather than as the session's own command.
    pub launch: Option<String>,
    /// The conversation resumed.
    pub agent: Option<Resume>,
    /// What its viewers are told.
    pub restored: Restored,
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
            agent: None,
        }
    }

    /// How session `id` starts again, given the system's shells and the home directory
    /// Claude Code keeps its conversations under. Blocking: it looks at the directories and
    /// the transcript.
    ///
    /// The shell comes back as [`Self::reopen_command`] says. A conversation kept with it is
    /// resumed only while both its directory and its transcript are there; otherwise the
    /// session is the shell alone, with one line in the log.
    #[must_use]
    pub fn reopen(&self, id: SessionId, shells: &str, home: &Path, launch: &AgentLaunch) -> Reopen {
        let shell = self.reopen_command(shells);
        let agent = self.agent.as_ref().and_then(|agent| {
            let Some(cwd) = existing_dir(&agent.cwd) else {
                tracing::info!(session = %id, cwd = %agent.cwd, "agent not resumed: its directory is gone");
                return None;
            };
            let transcript = agent.transcript(home);
            if !transcript.is_file() {
                tracing::info!(session = %id, transcript = %transcript.display(), "agent not resumed: its conversation is gone");
                return None;
            }
            Some((agent, cwd))
        });
        let Some((agent, cwd)) = agent else {
            let again = shell == self.command;
            return Reopen {
                command: shell,
                cwd: self.cwd.as_deref().and_then(existing_dir),
                launch: None,
                agent: None,
                restored: self.restored(again),
            };
        };
        // What Slopty gave it is given again: the relay, the lock, its tools, and its role,
        // which a line typed at a prompt cannot carry when it spans lines.
        let args = |in_shell: bool| {
            let mut args = agent.args();
            if let Some(role) = agent.role.as_ref().filter(|_| !in_shell) {
                args.push(format!("--append-system-prompt={role}"));
            }
            if agent.relay
                && let Some(relay) = &launch.relay
            {
                args = slopty_agent::hooks::with_relay(args, relay, &cwd);
            }
            if agent.locked {
                args = slopty_agent::hooks::held_to(args, &cwd, agent.auto);
            }
            if agent.mcp
                && let Some(relay) = &launch.relay
            {
                args = slopty_agent::hooks::with_mcp(args, relay);
            }
            if let Some(installed) = launch.claude_mod.as_ref().filter(|_mod| !in_shell) {
                installed.args(args)
            } else {
                args
            }
        };
        // Still running in the background: it is opened where it runs, with nothing given anew.
        let attach = launch.background.contains(&agent.session);
        let args = |in_shell: bool| {
            if attach {
                vec![slopty_agent::roster::ATTACH.to_owned(), agent.session.clone()]
            } else {
                args(in_shell)
            }
        };
        let ran_it = self.command.first().filter(|program| {
            slopty_agent::detect::is_claude(
                program.rsplit('/').next().unwrap_or(program),
                &self.command,
            )
        });
        if let Some(program) = ran_it {
            let program =
                if program.rsplit('/').next() == Some("claude") { program } else { "claude" };
            return Reopen {
                command: std::iter::once(program.to_owned()).chain(args(false)).collect(),
                cwd: Some(cwd),
                launch: None,
                agent: Some(agent.clone()),
                restored: self.restored(true),
            };
        }
        let words: Vec<String> = std::iter::once("claude".to_owned()).chain(args(true)).collect();
        let again = shell == self.command;
        // Quoting cannot keep a control character from the line editor: it would act on it as
        // it is typed. A kept recipe is a file anybody with the account can write.
        if !words.iter().all(|word| slopty_agent::resume::typeable(word)) {
            tracing::warn!(session = %id, "agent not resumed: its line has a control character");
            return Reopen {
                cwd: self.cwd.as_deref().and_then(existing_dir),
                launch: None,
                agent: None,
                restored: self.restored(again),
                command: shell,
            };
        }
        let words: Vec<String> = words.iter().map(|word| slopty_core::shell_quote(word)).collect();
        Reopen {
            command: shell,
            cwd: Some(cwd),
            launch: Some(words.join(" ")),
            agent: Some(agent.clone()),
            restored: self.restored(again),
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

    /// What the reopened session's viewers are told: the command it ran before, unless that
    /// runs `again` (then the one before that, if it too was reopened).
    #[must_use]
    fn restored(&self, again: bool) -> Restored {
        let command = if again {
            self.restored.as_ref().map(|r| r.command.clone()).unwrap_or_default()
        } else {
            self.command.clone()
        };
        Restored { saved_ms: self.saved_ms, command }
    }
}

/// The conversations of `recipes` that Claude Code runs in the background now, as its session
/// registry in `sessions` lists them ([`slopty_agent::roster::registered`]).
///
/// The registry is read only when a recipe holds a conversation; one that cannot be read is
/// none.
pub async fn background<'a>(
    sessions: PathBuf,
    recipes: impl IntoIterator<Item = &'a Recipe>,
) -> Vec<String> {
    if !recipes.into_iter().any(|recipe| recipe.agent.is_some()) {
        return Vec::new();
    }
    tokio::task::spawn_blocking(move || {
        let listed = slopty_agent::roster::registered(&sessions, crate::ports::alive);
        slopty_agent::roster::background(&listed).map(str::to_owned).collect()
    })
    .await
    .unwrap_or_default()
}

/// `dir` (a leading `~` being this worker's home) when it is a directory now.
fn existing_dir(dir: &str) -> Option<PathBuf> {
    Some(crate::file::expand_home(Path::new(dir))).filter(|dir| dir.is_dir())
}

/// The system's list of shells, empty where there is none.
#[must_use]
pub fn system_shells() -> String {
    std::fs::read_to_string("/etc/shells").unwrap_or_default()
}

enum Job {
    Opened(SessionId, Recipe),
    Checkpoint(SessionId, Vec<u8>, Place),
    Agent(SessionId, Option<Resume>),
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

    /// The Claude Code conversation session `id` holds now, or none: written at once when it
    /// differs from the one kept.
    pub fn agent(&self, id: SessionId, agent: Option<Resume>) {
        let _sent = self.jobs.send(Job::Agent(id, agent));
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
            Some(Job::Agent(id, agent)) => {
                if let Some(recipe) = recipes.get_mut(&id)
                    && recipe.agent != agent
                {
                    recipe.agent = agent;
                    let recipe = recipe.clone();
                    write_recipe(&dir, id, &recipe).await;
                }
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
        Err(e) => {
            tracing::warn!(session = %id, error = %e, "screen not kept");
            crate::caps::not_written(KEPT, &e);
        }
    }
}

/// What the terminals' kept screens are called when they cannot be written.
const KEPT: &str = "Terminals' kept screens";

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
    match written {
        Ok(()) => crate::caps::wrote(KEPT),
        Err(e) => {
            tracing::warn!(session = %id, error = %e, "recipe not kept");
            crate::caps::not_written(KEPT, &e);
        }
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

    fn reopen(recipe: &Recipe) -> Reopen {
        recipe.reopen(SessionId::new(), SHELLS, Path::new("/nowhere"), &AgentLaunch::default())
    }

    /// A tile opened on a shell gets that shell back; anything else, the login shell. The
    /// viewers hear of the command that is not run again, and a second loss remembers it.
    #[test]
    fn only_a_shell_runs_again() {
        assert_eq!(recipe(&["/bin/zsh"]).reopen_command(SHELLS), ["/bin/zsh"]);
        assert_eq!(recipe(&[]).reopen_command(SHELLS), Vec::<String>::new());
        assert_eq!(recipe(&["claude"]).reopen_command(SHELLS), Vec::<String>::new());
        assert_eq!(
            recipe(&["/bin/sh", "-c", "sleep 9"]).reopen_command(SHELLS),
            Vec::<String>::new()
        );
        assert!(recipe(&["/opt/fish"]).reopen_command(SHELLS).is_empty(), "not listed");

        let first = reopen(&recipe(&["claude", "--continue"]));
        assert!(first.command.is_empty() && first.launch.is_none(), "no conversation kept");
        assert_eq!(first.restored.command, ["claude", "--continue"]);
        let reopened = Recipe { restored: Some(first.restored), ..recipe(&[]) };
        assert_eq!(reopen(&reopened).restored.command, ["claude", "--continue"], "kept");
        assert_eq!(reopen(&recipe(&["/bin/zsh"])).restored.command, Vec::<String>::new());
    }

    /// A conversation kept with the session comes back resumed in its own directory: typed at
    /// the first prompt of a shell the person ran it in, or as the command of a tile opened on
    /// it, which then has nothing left to offer. Its transcript or its directory gone, the
    /// session is the shell alone.
    #[test]
    fn a_kept_conversation_comes_back_resumed() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let transcript = home.path().join("abc.jsonl");
        std::fs::write(&transcript, b"{}\n").unwrap();
        let agent = Resume {
            session: "abc".to_owned(),
            cwd: project.path().to_string_lossy().into_owned(),
            transcript: Some(transcript.to_string_lossy().into_owned()),
            args: vec!["--model".to_owned(), "opus 5".to_owned()],
            relay: false,
            mcp: false,
            locked: false,
            auto: false,
            role: None,
        };
        let open = |command: &[&str]| Recipe {
            agent: Some(agent.clone()),
            cwd: Some("/".to_owned()),
            ..recipe(command)
        };
        let at = |recipe: &Recipe| {
            recipe.reopen(SessionId::new(), SHELLS, home.path(), &AgentLaunch::default())
        };

        let shell = at(&open(&["/bin/zsh"]));
        assert_eq!(shell.command, ["/bin/zsh"]);
        assert_eq!(shell.launch.as_deref(), Some("claude --resume abc --model 'opus 5'"));
        assert_eq!(shell.cwd.as_deref(), Some(project.path()));
        assert_eq!(shell.agent.as_ref(), Some(&agent));

        let tile = at(&open(&["/opt/bin/claude", "--model", "opus 5", "hello"]));
        assert_eq!(tile.command, ["/opt/bin/claude", "--resume", "abc", "--model", "opus 5"]);
        assert!(tile.launch.is_none());
        assert!(tile.restored.command.is_empty(), "the agent runs again");

        std::fs::remove_file(&transcript).unwrap();
        let gone = at(&open(&["/bin/zsh"]));
        assert_eq!(
            (gone.command.as_slice(), gone.launch, gone.agent),
            (["/bin/zsh".to_owned()].as_slice(), None, None)
        );
        assert_eq!(gone.cwd.as_deref(), Some(Path::new("/")), "the shell's own directory");
        std::fs::write(&transcript, b"{}\n").unwrap();
        let moved = Recipe {
            agent: Some(Resume { cwd: "/nowhere/at/all".to_owned(), ..agent.clone() }),
            ..open(&["/bin/zsh"])
        };
        assert!(at(&moved).launch.is_none(), "its directory is gone");

        // A kept recipe is a file: one whose flags hold a control character is never typed.
        for word in ["x\ry", "\u{3}", "a\u{1b}[200~b"] {
            let tampered = Recipe {
                agent: Some(Resume {
                    args: vec!["--name".to_owned(), word.to_owned()],
                    ..agent.clone()
                }),
                ..open(&["/bin/zsh"])
            };
            let shell = at(&tampered);
            assert_eq!(
                (shell.command.as_slice(), shell.launch, shell.agent),
                (["/bin/zsh".to_owned()].as_slice(), None, None),
                "{word:?}"
            );
        }
    }

    /// A conversation Claude Code still runs in the background is opened where it runs,
    /// `claude attach <id>`, with nothing given anew: as a tile's command, or typed at the
    /// first prompt of the shell it ran in. One no longer there is resumed as before.
    #[test]
    fn a_conversation_running_in_the_background_is_attached_not_resumed() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let transcript = home.path().join("abc.jsonl");
        std::fs::write(&transcript, b"{}\n").unwrap();
        let agent = Resume {
            session: "abc".to_owned(),
            cwd: project.path().to_string_lossy().into_owned(),
            transcript: Some(transcript.to_string_lossy().into_owned()),
            args: vec!["--model".to_owned(), "opus".to_owned()],
            relay: true,
            mcp: false,
            locked: false,
            auto: false,
            role: None,
        };
        let open = |command: &[&str]| Recipe { agent: Some(agent.clone()), ..recipe(command) };
        let launch = AgentLaunch {
            relay: Some("/opt/slopty".to_owned()),
            background: vec!["abc".to_owned()],
            ..AgentLaunch::default()
        };
        let at = |recipe: &Recipe, launch: &AgentLaunch| {
            recipe.reopen(SessionId::new(), SHELLS, home.path(), launch)
        };
        let tile = at(&open(&["/opt/bin/claude", "--bg"]), &launch);
        assert_eq!(tile.command, ["/opt/bin/claude", "attach", "abc"]);
        assert_eq!(tile.cwd.as_deref(), Some(project.path()));
        let shell = at(&open(&["/bin/zsh"]), &launch);
        assert_eq!(shell.launch.as_deref(), Some("claude attach abc"));
        let ended = AgentLaunch { background: vec!["other".to_owned()], ..launch };
        let resumed = at(&open(&["/bin/zsh"]), &ended);
        assert!(resumed.launch.unwrap().contains("--resume abc"), "resumed as before");
    }

    /// Claude Code's registry is read for the conversations it runs in the background only when
    /// a lost session held one; a session whose process ended, and a registry that is not
    /// there, list none.
    #[tokio::test]
    async fn the_background_conversations_are_claude_codes_own_list() {
        let dir = tempfile::tempdir().unwrap();
        let live = std::process::id();
        let gone = i32::MAX;
        for (pid, kind, session) in
            [(live.to_string(), "bg", "abc"), (gone.to_string(), "bg", "ended")]
        {
            let file = dir.path().join(format!("{pid}.json"));
            std::fs::write(file, format!(r#"{{"sessionId":"{session}","kind":"{kind}"}}"#))
                .unwrap();
        }
        let held = Recipe {
            agent: Some(Resume {
                session: "abc".to_owned(),
                cwd: "/".to_owned(),
                transcript: None,
                args: Vec::new(),
                relay: false,
                mcp: false,
                locked: false,
                auto: false,
                role: None,
            }),
            ..recipe(&["/bin/zsh"])
        };
        let sessions = dir.path().to_path_buf();
        let shells = background(sessions.clone(), [&recipe(&["/bin/zsh"])]).await;
        assert_eq!(shells, Vec::<String>::new(), "not read for shells alone");
        assert_eq!(background(sessions, [&held]).await, ["abc"], "the ended one is passed over");
        let none = dir.path().join("none");
        assert_eq!(background(none, [&held]).await, Vec::<String>::new());
    }

    /// A project's agent comes back wired as Slopty started it: its relay, its tools, the lock
    /// on the mode that asks nothing, and its role. A shell the person ran it in gets all but
    /// the role, which spans lines a prompt cannot take.
    #[test]
    fn a_project_s_agent_comes_back_with_its_tools_role_and_lock() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let transcript = home.path().join("abc.jsonl");
        std::fs::write(&transcript, b"{}\n").unwrap();
        let agent = Resume {
            session: "abc".to_owned(),
            cwd: project.path().to_string_lossy().into_owned(),
            transcript: Some(transcript.to_string_lossy().into_owned()),
            args: Vec::new(),
            relay: true,
            mcp: true,
            locked: true,
            auto: false,
            role: Some("You work on task 3.\nReport with task_report.".to_owned()),
        };
        let launch =
            AgentLaunch { relay: Some("/opt/slopty".to_owned()), ..AgentLaunch::default() };
        let recipe = |command: &[&str]| Recipe { agent: Some(agent.clone()), ..recipe(command) };
        let tile = recipe(&["claude"]).reopen(SessionId::new(), SHELLS, home.path(), &launch);
        let kept = slopty_agent::resume::invocation(&tile.command[1..]);
        assert!(kept.relay && kept.mcp && kept.locked, "{:?}", tile.command);
        assert_eq!(kept.role, agent.role, "its role");
        assert!(tile.command.windows(2).any(|w| w == ["--resume", "abc"]), "{:?}", tile.command);

        let shell = recipe(&["/bin/zsh"]).reopen(SessionId::new(), SHELLS, home.path(), &launch);
        let line = shell.launch.expect("typed at its prompt");
        assert!(line.contains("--mcp-config") && line.contains("disableBypassPermissionsMode"));
        assert!(!line.contains("--append-system-prompt"), "{line}");
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
        assert_eq!(again.screen(b).await, b"");
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(dir.path()).unwrap().permissions(),
        );
        assert_eq!(mode & 0o777, 0o700);
    }

    /// Each session keeps the conversation it was last told of, on disk at once, and loses it
    /// when told there is none; a session the keeper does not hold is not written.
    #[tokio::test]
    async fn each_session_keeps_its_own_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let (keeper, _found) = Keeper::open(dir.path()).unwrap();
        let (a, b, stray) = (SessionId::new(), SessionId::new(), SessionId::new());
        keeper.opened(a, recipe(&[]));
        keeper.opened(b, recipe(&[]));
        let conversation = |session: &str| Resume {
            session: session.to_owned(),
            cwd: "/w".to_owned(),
            transcript: None,
            args: Vec::new(),
            relay: false,
            mcp: false,
            locked: false,
            auto: false,
            role: None,
        };
        keeper.agent(a, Some(conversation("first")));
        keeper.agent(b, Some(conversation("other")));
        keeper.agent(a, Some(conversation("second")));
        keeper.agent(stray, Some(conversation("stray")));
        keeper.flush().await;
        let (_again, found) = Keeper::open(dir.path()).unwrap();
        assert_eq!(found[&a].agent.as_ref().map(|r| r.session.as_str()), Some("second"));
        assert_eq!(found[&b].agent.as_ref().map(|r| r.session.as_str()), Some("other"));
        assert!(!found.contains_key(&stray));
        keeper.agent(b, None);
        keeper.flush().await;
        let (_again, found) = Keeper::open(dir.path()).unwrap();
        assert!(found[&b].agent.is_none());
        assert!(found[&a].agent.is_some());
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
