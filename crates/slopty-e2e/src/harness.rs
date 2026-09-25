//! ptyd + worker + the app, all from this build, in a temporary directory.
//!
//! Binaries come from `SLOPTY_E2E_BIN_DIR` (set by `cargo xtask e2e`), else `target/debug`
//! next to the workspace. Every process gets its own data directory under the temp dir, so
//! nothing installed on the machine is read or written, and everything is killed on drop.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};

use crate::driver::Driver;

/// How long a daemon or the app may take to come up.
const STARTUP: Duration = Duration::from_secs(30);
/// Socket poll interval.
const POLL: Duration = Duration::from_millis(50);

/// A Claude Code transcript in the JSONL shape the hooks name: one exchange with thinking, a
/// tool call, its result and the answer (a fixture, never a real session's file).
pub const TRANSCRIPT: &str = concat!(
    r#"{"type":"user","timestamp":"2026-09-05T10:00:00.000Z","message":{"role":"user","content":"fix the failing test"}}"#,
    "\n",
    r#"{"type":"assistant","timestamp":"2026-09-05T10:00:02.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"The assertion compares the wrong field."},{"type":"text","text":"Looking at the test."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test -p demo"}}]}}"#,
    "\n",
    r#"{"type":"user","timestamp":"2026-09-05T10:00:05.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"running 2 tests\ntest a ... ok\ntest b ... FAILED"}]}}"#,
    "\n",
    r#"{"type":"assistant","timestamp":"2026-09-05T10:00:09.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Fixed: `b` compared **id** with *name*.\n\n- one edit\n- both tests pass"}]}}"#,
    "\n",
);

/// [`TRANSCRIPT`] plus the record that ends the turn, for an agent whose state is read off
/// its transcript rather than its hooks: `stop_reason` is what says a turn finished.
pub const TRANSCRIPT_DONE: &str = concat!(
    r#"{"type":"assistant","timestamp":"2026-09-05T10:00:10.000Z","message":{"role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"Both tests pass now."}]}}"#,
    "\n",
);

/// A stack's temporary root, named for the test that made it.
///
/// Goldens show paths under it (a shell's prompt, a tile's title, the status bar), so a random
/// name made each run's frame differ from its golden wherever a path is drawn. The name is a
/// hash of the test's name instead: the same on every run, different between tests. A lock
/// beside it keeps a second run of the same test, another session's, off it; that run takes a
/// random name, which costs it only its goldens.
#[derive(Debug)]
pub struct StackDir {
    // Declared first so it is deleted before the lock is released.
    dir: tempfile::TempDir,
    _lock: Option<std::fs::File>,
}

impl StackDir {
    fn new(prefix: &str) -> Result<Self> {
        let parent = std::env::temp_dir();
        // libtest runs each test on a thread named for it.
        let test = std::thread::current().name().filter(|name| *name != "main").map(fnv1a);
        if let Some(hash) = test {
            let name = format!("{prefix}{hash:08x}");
            let lock = std::fs::File::create(parent.join(format!("{name}.lock")))?;
            if lock.try_lock().is_ok() {
                match std::fs::remove_dir_all(parent.join(&name)) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e).context("clear the last run's root"),
                }
                let dir =
                    tempfile::Builder::new().prefix(&name).rand_bytes(0).tempdir_in(&parent)?;
                return Ok(Self { dir, _lock: Some(lock) });
            }
        }
        let dir = tempfile::Builder::new().prefix(prefix).tempdir()?;
        Ok(Self { dir, _lock: None })
    }

    /// The root.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// FNV-1a over `name`: short, and stable across toolchains, as `DefaultHasher` is not.
fn fnv1a(name: &str) -> u32 {
    name.bytes().fold(0x811c_9dc5, |h, b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193))
}

/// The running stack.
#[derive(Debug)]
pub struct Stack {
    /// The temporary directory (sockets, data dirs, artifacts).
    pub dir: StackDir,
    /// Where the app reaches the worker: `127.0.0.1:<port>`, the port the worker picked.
    pub address: String,
    /// Connected to the app's test socket.
    pub driver: Driver,
    /// ptyd, the worker, app, and any helper windows; killed on drop.
    pub children: Vec<Child>,
    /// Which of [`Self::children`] is the app process, so [`Self::kill_app`] kills the app by
    /// role and not the last-pushed child (e.g. an idle-window helper). `None` in a simulator,
    /// where the app is not a child here, and between [`Self::kill_app`] and its relaunch.
    pub app_ix: Option<usize>,
    /// The simulator the app runs in, when it does; the app is terminated there on shutdown.
    pub simulator: Option<Simulator>,
    /// The daemons' log level.
    pub log: String,
    /// Extra environment the app was launched with, so [`Self::relaunch_app`] repeats it.
    pub app_env: Vec<(String, String)>,
}

/// A second client of the same worker: another app process (or the app in a simulator) with
/// its own data directory, identity and test socket, that added the same worker address.
#[derive(Debug)]
pub struct SecondApp {
    /// Connected to its test socket.
    pub driver: Driver,
    /// The process, when it runs here (killed on drop); `None` in a simulator.
    pub child: Option<Child>,
    /// The simulator it runs in, when it does.
    pub simulator: Option<Simulator>,
}

/// Two clients on one worker: the stack's own app (`a`) and a [`SecondApp`] (`b`).
#[derive(Debug)]
pub struct Pair {
    /// ptyd, the worker and the first app.
    pub stack: Stack,
    /// The second app.
    pub b: SecondApp,
}

/// The window [`Stack::start_idle_window`] opened: a target that draws nothing until it is
/// told to.
#[derive(Debug, Clone)]
pub struct IdleWindow {
    markers: PathBuf,
    title: String,
}

impl IdleWindow {
    /// Its window title, which is how the app's picker lists it.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Take the window off screen. It stays in the worker's window list (the worker enumerates with
    /// `onScreenWindowsOnly: false`), so a stream opened on it captures nothing at all.
    ///
    /// # Errors
    ///
    /// When the marker cannot be written.
    pub fn hide(&self) -> Result<()> {
        std::fs::write(self.markers.join("hide"), b"")?;
        Ok(())
    }

    /// Put it back and keep repainting it, which is what makes the worker call the source live.
    ///
    /// # Errors
    ///
    /// When the marker cannot be written.
    pub fn show(&self) -> Result<()> {
        std::fs::write(self.markers.join("show"), b"")?;
        Ok(())
    }
}

/// A booted iOS simulator and the app installed in it.
#[derive(Debug, Clone)]
pub struct Simulator {
    /// `simctl` device UDID.
    pub udid: String,
    /// The app's bundle identifier.
    pub bundle_id: String,
}

/// Where the binaries are.
///
/// # Errors
///
/// When neither the environment nor the default location holds them.
pub fn bin_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("SLOPTY_E2E_BIN_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = manifest.join("../../target/debug");
    if dir.join("slopty-app").exists() {
        return Ok(dir);
    }
    bail!("no built binaries: run `cargo xtask e2e app` (builds them and sets SLOPTY_E2E_BIN_DIR)")
}

fn bin(name: &str) -> Result<PathBuf> {
    let path = bin_dir()?.join(name);
    anyhow::ensure!(path.exists(), "{} is not built", path.display());
    Ok(path)
}

/// Where artifacts (renders, diffs) go: `SLOPTY_E2E_ARTIFACTS`, else `target/e2e/artifacts`.
#[must_use]
pub fn artifacts_dir() -> PathBuf {
    std::env::var_os("SLOPTY_E2E_ARTIFACTS").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/e2e/artifacts"),
        PathBuf::from,
    )
}

async fn wait_for_path(path: &Path, child: &mut Child, what: &str) -> Result<()> {
    let waited = tokio::time::timeout(STARTUP, async {
        while !path.exists() {
            if let Some(status) = child.try_wait()? {
                bail!("{what} exited early: {status}");
            }
            tokio::time::sleep(POLL).await;
        }
        Ok(())
    })
    .await;
    match waited {
        Ok(result) => result,
        Err(_elapsed) => bail!("{what} did not create {} within {STARTUP:?}", path.display()),
    }
}

/// One request over the worker's control socket at `path` (newline-delimited JSON, one request
/// per connection), and its reply.
async fn ctl(path: &Path, request: &Value) -> Result<Value> {
    let stream = UnixStream::connect(path)
        .await
        .with_context(|| format!("connect to the worker at {}", path.display()))?;
    let (rd, mut wr) = stream.into_split();
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    wr.write_all(&line).await?;
    wr.shutdown().await?;
    let mut reply = String::new();
    BufReader::new(rd).read_line(&mut reply).await?;
    serde_json::from_str(reply.trim()).context("the worker's reply is not JSON")
}

/// Wait for a socket path from a process that is not our child (the simulator's).
async fn wait_for_socket(path: &Path, what: &str) -> Result<()> {
    let waited = tokio::time::timeout(STARTUP, async {
        while !path.exists() {
            tokio::time::sleep(POLL).await;
        }
    })
    .await;
    match waited {
        Ok(()) => Ok(()),
        Err(_elapsed) => bail!("{what} did not create {} within {STARTUP:?}", path.display()),
    }
}

async fn daemons(
    root: &Path,
    worker_name: &str,
    log: &str,
    env: &[(&str, &str)],
) -> Result<(Vec<Child>, String)> {
    let ptyd = spawn_ptyd(root, log, env).await?;
    let (worker, address) = spawn_worker(root, worker_name, log, env, None).await?;
    Ok((vec![ptyd, worker], address))
}

/// ptyd compiles ghostty's terminfo on start-up; keep it out of the developer's own
/// `~/.terminfo` and let the shells it spawns read it back from here. The trailing separator
/// is ncurses' way of saying "then the system database".
fn terminfo_env(root: &Path) -> [(&'static str, std::ffi::OsString); 2] {
    let terminfo = root.join("terminfo");
    let dirs = format!("{}:", terminfo.display());
    [(slopty_pty::terminfo::DIR_ENV, terminfo.into_os_string()), ("TERMINFO_DIRS", dirs.into())]
}

/// The variable naming the pasteboard the worker and the app share the clipboard through.
pub const PASTEBOARD_ENV: &str = "SLOPTY_PASTEBOARD";

/// The pasteboard `who` (`worker`, or an app's name) of the run under `root` uses: named after
/// the run, so no two runs and no two processes share one, and nothing touches the human's
/// clipboard.
#[must_use]
pub fn pasteboard_name(root: &Path, who: &str) -> String {
    let run = root.file_name().map_or_else(|| "run".into(), |n| n.to_string_lossy());
    format!("com.aislopware.slopty.e2e.{run}.{who}")
}

/// ptyd on `root/ptyd.sock`, up once its socket is.
async fn spawn_ptyd(root: &Path, log: &str, env: &[(&str, &str)]) -> Result<Child> {
    let ptyd_sock = root.join("ptyd.sock");
    let mut ptyd = Command::new(bin("slopty-ptyd")?)
        .arg("--socket")
        .arg(&ptyd_sock)
        .envs(env.iter().copied())
        .envs(terminfo_env(root))
        .env("RUST_LOG", log)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .context("spawn slopty-ptyd")?;
    wait_for_path(&ptyd_sock, &mut ptyd, "slopty-ptyd").await?;
    Ok(ptyd)
}

/// The worker named `worker_name` on the ptyd of [`spawn_ptyd`], with its data in `root/worker`, on
/// a port of its choosing, registered with `server` when one is given; and the loopback address
/// clients reach it on.
async fn spawn_worker(
    root: &Path,
    worker_name: &str,
    log: &str,
    env: &[(&str, &str)],
    server: Option<&str>,
) -> Result<(Child, String)> {
    let mut command = Command::new(bin("slopty-worker")?);
    command
        .arg("--ptyd-socket")
        .arg(root.join("ptyd.sock"))
        .arg("--ctl-socket")
        .arg(root.join("worker.sock"))
        .arg("--data-dir")
        .arg(root.join("worker"))
        .arg("--print-addr")
        .arg("--port")
        .arg("0");
    if let Some(server) = server {
        command.arg("--server").arg(server);
    }
    let mut worker = command
        .envs(env.iter().copied())
        .envs(terminfo_env(root))
        .env("RUST_LOG", log)
        .env("SLOPTY_WORKER_NAME", worker_name)
        .env(PASTEBOARD_ENV, pasteboard_name(root, "worker"))
        // A drop whose name is taken in the shell's directory lands here, not in `~`.
        .env("SLOPTY_DROP_DIR", root.join("drops"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .context("spawn slopty-worker")?;
    let stdout = worker.stdout.take().context("worker stdout")?;
    let mut listen = String::new();
    tokio::time::timeout(STARTUP, BufReader::new(stdout).read_line(&mut listen))
        .await
        .context("the worker did not print its address in time")??;
    Ok((worker, loopback_address(&listen)?))
}

/// The loopback address a client dials for a worker that printed `listen` (`[::]:53211`, or a
/// specific IP when it was bound to one): the port is what matters.
fn loopback_address(listen: &str) -> Result<String> {
    let listen: std::net::SocketAddr =
        listen.trim().parse().with_context(|| format!("the worker printed {listen:?}"))?;
    Ok(format!("127.0.0.1:{}", listen.port()))
}

/// A stand-in for the `claude` binary, written into a run's own directory and put first on
/// ptyd's `PATH` so "+ agent" starts it instead of a real agent.
///
/// It behaves like Claude Code where Slopty looks: it paints the spinning title while a turn
/// runs, the sparkle and a summary when it ends, and writes its conversation as JSONL under
/// `$HOME/.claude/projects/<escaped cwd>`, with the same escaping the real one uses. It moves from
/// stage to stage when the test creates the marker file it is waiting for, so the run never depends
/// on a sleep, and it registers no hooks at all — which is the whole point.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
set -e
stage() { while [ ! -f "$SLOPTY_FAKE_CLAUDE_DIR/$1" ]; do sleep 0.05; done; }
echo "fake claude in $PWD"
stage working
# OSC 2 with U+25D0 CIRCLE WITH LEFT HALF BLACK, one of the frames Claude Code paints into
# the title while a turn runs (`slopty_agent::title::WORKING`).
printf '\033]2;\342\227\220 Claude Code\007'
stage transcript
project="$HOME/.claude/projects/$(printf '%s' "$PWD" | sed 's/[^a-zA-Z0-9]/-/g')"
mkdir -p "$project"
cat "$SLOPTY_FAKE_CLAUDE_DIR/transcript.jsonl" > "$project/fake-session.jsonl"
stage done
cat "$SLOPTY_FAKE_CLAUDE_DIR/transcript-done.jsonl" >> "$project/fake-session.jsonl"
# U+2733 EIGHT SPOKED ASTERISK and the conversation's summary: the title between turns.
printf '\033]2;\342\234\263 fix the tests\007'
stage quit
"#;

/// Start one app process named `name` under `root` (its data directory is `root/<name>`, its
/// test socket `root/<name>.sock`) and connect to its socket. `env` overrides the defaults.
async fn spawn_app(
    root: &Path,
    name: &str,
    log: &str,
    env: &[(&str, &str)],
) -> Result<(Child, Driver)> {
    let app_dir = root.join(name);
    std::fs::create_dir_all(&app_dir)?;
    pin_appearance(&app_dir)?;
    let app_sock = root.join(format!("{name}.sock"));
    let mut app = Command::new(bin("slopty-app")?)
        .env("RUST_LOG", log)
        .env("SLOPTY_DATA_DIR", &app_dir)
        .env(crate::SOCKET_ENV, &app_sock)
        // Local echo would put predicted text in the rows before the worker confirms it.
        .env("SLOPTY_PREDICT", "never")
        .envs(env.iter().copied())
        .env(PASTEBOARD_ENV, pasteboard_name(root, name))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .context("spawn slopty-app")?;
    wait_for_path(&app_sock, &mut app, "slopty-app").await?;
    let driver = connect_with_retry(&app_sock, &mut app).await?;
    Ok((app, driver))
}

/// The appearance every app under test starts in, whatever the machine's is: the default
/// (`system`) would make each golden depend on System Settings on the day it runs.
pub const APPEARANCE: &str = "light";

/// Write a `settings.toml` into `app_dir` that pins [`APPEARANCE`], unless a test already
/// put one there.
fn pin_appearance(app_dir: &Path) -> Result<()> {
    let path = app_dir.join("settings.toml");
    if !path.exists() {
        std::fs::write(&path, format!("[theme]\nappearance = \"{APPEARANCE}\"\n"))?;
    }
    Ok(())
}

/// Connect to `sock`, retrying for a few seconds: under heavy load the listener may not accept
/// in the instant after it binds the socket file (`Connection refused`). A process that has
/// actually died is caught between attempts and fails fast.
async fn connect_with_retry(sock: &Path, child: &mut Child) -> Result<Driver> {
    let deadline = tokio::time::Instant::now().checked_add(STARTUP);
    loop {
        match Driver::connect(sock).await {
            Ok(driver) => return Ok(driver),
            Err(e) => {
                if let Some(status) = child.try_wait()? {
                    bail!("slopty-app exited before it accepted: {status}");
                }
                if deadline.is_some_and(|d| tokio::time::Instant::now() >= d) {
                    return Err(e);
                }
                tokio::time::sleep(POLL).await;
            }
        }
    }
}

/// Launch the app in a booted simulator with its socket at `root/<name>.sock` and its data
/// under `root/<name>` (the simulator shares this file system), and connect to it. Each
/// variable of `env` is passed as `SIMCTL_CHILD_<name>`.
async fn spawn_simulator_app(
    root: &Path,
    name: &str,
    log: &str,
    simulator: &Simulator,
    env: &[(&str, &str)],
) -> Result<Driver> {
    let app_dir = root.join(name);
    std::fs::create_dir_all(&app_dir)?;
    pin_appearance(&app_dir)?;
    let app_sock = root.join(format!("{name}.sock"));
    let launch = Command::new("xcrun")
        .args(["simctl", "launch", "--terminate-running-process"])
        .arg(&simulator.udid)
        .arg(&simulator.bundle_id)
        .env("SIMCTL_CHILD_RUST_LOG", log)
        .env("SIMCTL_CHILD_SLOPTY_DATA_DIR", &app_dir)
        .env(format!("SIMCTL_CHILD_{}", crate::SOCKET_ENV), &app_sock)
        .env("SIMCTL_CHILD_SLOPTY_PREDICT", "never")
        // Glass only: the key bar stays in the frame whatever the simulator has attached.
        .env("SIMCTL_CHILD_SLOPTY_HARDWARE_KEYBOARD", "0")
        .envs(env.iter().map(|(k, v)| (format!("SIMCTL_CHILD_{k}"), *v)))
        // A named pasteboard: the simulator's general one follows this Mac's clipboard.
        .env(format!("SIMCTL_CHILD_{PASTEBOARD_ENV}"), pasteboard_name(root, name))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status();
    // `launch` blocks while the simulator is still booting; the xtask waits for
    // `bootstatus` first, so a long wait here is a broken simulator, not a slow one.
    let launched = tokio::time::timeout(STARTUP, launch)
        .await
        .context("simctl launch did not return (is the simulator booted?)")?
        .context("xcrun simctl launch")?;
    anyhow::ensure!(launched.success(), "simctl launch failed: {launched}");
    wait_for_socket(&app_sock, "the app in the simulator").await?;
    let deadline = tokio::time::Instant::now().checked_add(STARTUP);
    loop {
        match Driver::connect(&app_sock).await {
            Ok(driver) => return Ok(driver),
            Err(_) if deadline.is_some_and(|d| tokio::time::Instant::now() < d) => {
                tokio::time::sleep(POLL).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Ping, add the worker at `address` and wait for it to connect.
async fn add_worker(driver: &mut Driver, address: &str) -> Result<()> {
    driver.ok(&crate::Command::Ping).await?;
    driver.ok(&crate::Command::AddWorker { address: address.to_owned() }).await?;
    driver
        .wait_for("the worker to connect", STARTUP, |d| {
            d.workers.iter().any(|w| w.status == "connected")
        })
        .await?;
    Ok(())
}

impl Stack {
    /// Start ptyd, the worker (named `worker_name`) and the app; add the worker in the app and wait
    /// until its canvas is up.
    ///
    /// # Errors
    ///
    /// When a binary is missing, a process dies, or the app does not come up in time.
    pub async fn launch(worker_name: &str) -> Result<Self> {
        Self::launch_with(worker_name, &[]).await
    }

    /// [`Self::launch`] with a fake `claude` (`FAKE_CLAUDE`) first on the daemons' `PATH`
    /// and a `HOME` of their own, so "+ agent" opens a session the worker must attribute
    /// without any hook ever firing. Drive it with [`Self::fake_claude_stage`].
    ///
    /// # Errors
    ///
    /// As [`Self::launch`], plus when the fake cannot be written.
    pub async fn launch_with_fake_claude(worker_name: &str) -> Result<Self> {
        let dir = StackDir::new("slopty-e2e-agent-")?;
        let root = dir.path().to_path_buf();
        let home = root.join("home");
        let fake = root.join("fake");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&fake)?;
        let claude = fake.join("claude");
        std::fs::write(&claude, FAKE_CLAUDE)?;
        let mode = std::os::unix::fs::PermissionsExt::from_mode(0o755);
        std::fs::set_permissions(&claude, mode)?;
        std::fs::write(fake.join("transcript.jsonl"), TRANSCRIPT)?;
        std::fs::write(fake.join("transcript-done.jsonl"), TRANSCRIPT_DONE)?;
        let path = std::env::var("PATH").unwrap_or_default();
        let (home, fake_dir) = (home.to_string_lossy(), fake.to_string_lossy());
        let path = format!("{}:{path}", fake.display());
        let env = [("HOME", &*home), ("PATH", &*path), ("SLOPTY_FAKE_CLAUDE_DIR", &*fake_dir)];
        Self::launch_in(dir, worker_name, &env).await
    }

    /// The private `HOME` the daemons run with, when the launch gave them one (the
    /// fake-claude launch does): where the fake `claude` writes its transcripts.
    #[must_use]
    pub fn home(&self) -> Option<String> {
        self.app_env.iter().find(|(k, _v)| k == "HOME").map(|(_k, v)| v.clone())
    }

    /// Let the fake `claude` move past the stage it is waiting on (`working`, `transcript`,
    /// `done`, `quit`).
    ///
    /// # Errors
    ///
    /// When the marker file cannot be written.
    pub fn fake_claude_stage(&self, stage: &str) -> Result<()> {
        std::fs::write(self.dir.path().join("fake").join(stage), b"")?;
        Ok(())
    }

    /// [`Self::launch`] with extra environment for the daemons and the app (`SLOPTY_PREDICT`,
    /// `SLOPTY_FRAME_HZ`, …); the defaults are applied first, so `env` overrides them.
    ///
    /// # Errors
    ///
    /// As [`Self::launch`].
    pub async fn launch_with(worker_name: &str, env: &[(&str, &str)]) -> Result<Self> {
        let dir = StackDir::new("slopty-e2e-")?;
        Self::launch_in(dir, worker_name, env).await
    }

    /// [`Self::launch`] up to the app's first frame, before it knows any worker: what someone
    /// opening the app for the first time sees. [`Self::add_worker`] goes on from there.
    ///
    /// # Errors
    ///
    /// As [`Self::launch`].
    pub async fn launch_first_run(worker_name: &str) -> Result<Self> {
        let dir = StackDir::new("slopty-e2e-")?;
        let mut stack = Self::spawn_in(dir, worker_name, &[]).await?;
        stack.driver.ok(&crate::Command::Ping).await?;
        Ok(stack)
    }

    /// Add the stack's worker in the app, as the panel would, and wait for it to connect.
    ///
    /// # Errors
    ///
    /// When the worker does not connect in time.
    pub async fn add_worker(&mut self) -> Result<()> {
        let address = self.address.clone();
        add_worker(&mut self.driver, &address).await
    }

    /// `env` goes to the daemons and the app alike, on top of the defaults.
    async fn launch_in(dir: StackDir, worker_name: &str, env: &[(&str, &str)]) -> Result<Self> {
        Self::add(Self::spawn_in(dir, worker_name, env).await?).await
    }

    /// The daemons and the app, the worker not yet added.
    async fn spawn_in(dir: StackDir, worker_name: &str, env: &[(&str, &str)]) -> Result<Self> {
        let root = dir.path();
        let log = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_owned());
        let (mut children, address) = daemons(root, worker_name, &log, env).await?;
        let (app, driver) = spawn_app(root, "app", &log, env).await?;
        let app_ix = Some(children.len());
        children.push(app);
        let app_env = env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
        Ok(Self { dir, address, driver, children, app_ix, simulator: None, log, app_env })
    }

    /// Kill the app (SIGKILL: no goodbye to the worker, as a crash or a dead battery would
    /// leave it) and reap it. The daemons keep running; [`Self::relaunch_app`] brings it back.
    ///
    /// # Errors
    ///
    /// When the process cannot be signalled.
    pub async fn kill_app(&mut self) -> Result<()> {
        anyhow::ensure!(self.simulator.is_none(), "the app runs in a simulator");
        let ix = self.app_ix.take().context("no app process")?;
        let mut app = self.children.remove(ix);
        app.start_kill().context("kill slopty-app")?;
        let _status = app.wait().await;
        Ok(())
    }

    /// Start the app again on the same data directory (same identity, same socket path): it
    /// knows the worker and connects by itself. Waits for the link to be up.
    ///
    /// # Errors
    ///
    /// When the binary is missing or the app does not connect in time.
    pub async fn relaunch_app(&mut self) -> Result<()> {
        let env: Vec<(&str, &str)> =
            self.app_env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        // The killed app left its socket file on disk; remove it so `wait_for_path` waits for
        // the new process to bind rather than connecting to the dead one (Connection refused).
        let _removed = std::fs::remove_file(self.dir.path().join("app.sock"));
        let (app, mut driver) = spawn_app(self.dir.path(), "app", &self.log, &env).await?;
        self.app_ix = Some(self.children.len());
        self.children.push(app);
        driver.ok(&crate::Command::Ping).await?;
        driver
            .wait_for("the relaunched app to reconnect", STARTUP, |d| {
                d.workers.iter().any(|w| w.status == "connected")
            })
            .await?;
        self.driver = driver;
        Ok(())
    }

    /// [`Self::launch`] plus a second app on the same worker: `b` gets its own data directory,
    /// identity and socket, and adds the same worker address. The
    /// first app is left to open its first shell before the second comes up, so the two do not
    /// both find an empty canvas and open one each.
    ///
    /// # Errors
    ///
    /// As [`Self::launch`], for either app.
    pub async fn launch_pair(worker_name: &str) -> Result<Pair> {
        let mut stack = Self::launch(worker_name).await?;
        stack.wait_first_shell().await?;
        let (child, mut driver) = spawn_app(stack.dir.path(), "b", &stack.log, &[]).await?;
        add_worker(&mut driver, &stack.address).await?;
        Ok(Pair { stack, b: SecondApp { driver, child: Some(child), simulator: None } })
    }

    /// [`Self::launch_pair`] with the second app in a booted simulator: the Mac and the phone
    /// on one worker.
    ///
    /// # Errors
    ///
    /// As [`Self::launch_pair`] and [`Self::launch_on_simulator`].
    pub async fn launch_pair_with_simulator(
        worker_name: &str,
        simulator: Simulator,
    ) -> Result<Pair> {
        let mut stack = Self::launch(worker_name).await?;
        stack.wait_first_shell().await?;
        let mut driver =
            spawn_simulator_app(stack.dir.path(), "b", &stack.log, &simulator, &[]).await?;
        add_worker(&mut driver, &stack.address).await?;
        Ok(Pair { stack, b: SecondApp { driver, child: None, simulator: Some(simulator) } })
    }

    /// Wait until the first app has its first shell on the canvas with a prompt.
    async fn wait_first_shell(&mut self) -> Result<()> {
        self.driver
            .wait_for("the first shell with a prompt", STARTUP, |d| {
                d.status == "connected"
                    && d.item("terminal").is_some()
                    && d.terminals.iter().any(|t| t.rows.iter().any(|r| !r.is_empty()))
            })
            .await?;
        Ok(())
    }

    /// Start ptyd and the worker here and the app in a booted simulator (`simctl launch` with the
    /// socket and data dir in its environment; the simulator shares this file system), then
    /// add the worker and wait as [`Self::launch`] does.
    ///
    /// # Errors
    ///
    /// When a daemon is missing, `simctl` fails, or the app does not bind its socket in time.
    pub async fn launch_on_simulator(worker_name: &str, simulator: Simulator) -> Result<Self> {
        Self::launch_on_simulator_with(worker_name, simulator, &[]).await
    }

    /// [`Self::launch_on_simulator`] with extra environment for the app, as
    /// [`Self::launch_with`] (each variable is passed as `SIMCTL_CHILD_<name>`).
    ///
    /// # Errors
    ///
    /// As [`Self::launch_on_simulator`].
    pub async fn launch_on_simulator_with(
        worker_name: &str,
        simulator: Simulator,
        env: &[(&str, &str)],
    ) -> Result<Self> {
        Self::add(Self::spawn_on_simulator(worker_name, simulator, env).await?).await
    }

    /// [`Self::launch_on_simulator`] up to the first frame, before the app knows any worker,
    /// as [`Self::launch_first_run`].
    ///
    /// # Errors
    ///
    /// As [`Self::launch_on_simulator`].
    pub async fn launch_first_run_on_simulator(
        worker_name: &str,
        simulator: Simulator,
    ) -> Result<Self> {
        let mut stack = Self::spawn_on_simulator(worker_name, simulator, &[]).await?;
        stack.driver.ok(&crate::Command::Ping).await?;
        Ok(stack)
    }

    /// The daemons here and the app in the simulator, the worker not yet added.
    async fn spawn_on_simulator(
        worker_name: &str,
        simulator: Simulator,
        env: &[(&str, &str)],
    ) -> Result<Self> {
        let dir = StackDir::new("slopty-e2e-ios-")?;
        let root = dir.path();
        let log = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_owned());
        let (children, address) = daemons(root, worker_name, &log, &[]).await?;
        let driver = spawn_simulator_app(root, "app", &log, &simulator, env).await?;
        let app_env = env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
        Ok(Self {
            dir,
            address,
            driver,
            children,
            app_ix: None,
            simulator: Some(simulator),
            log,
            app_env,
        })
    }

    /// Ping, add the worker and wait for the connection.
    async fn add(mut stack: Self) -> Result<Self> {
        let address = stack.address.clone();
        add_worker(&mut stack.driver, &address).await?;
        Ok(stack)
    }

    /// Where to put a file for this run.
    #[must_use]
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Switch the app's theme to `appearance` (`dark`, `light`) by rewriting its
    /// `settings.toml`, as a person editing the file would; the app sees it within a second.
    ///
    /// # Errors
    ///
    /// When the file cannot be written.
    pub fn set_appearance(&self, appearance: &str) -> Result<()> {
        let path = self.path("app").join("settings.toml");
        std::fs::write(&path, format!("[theme]\nappearance = \"{appearance}\"\n"))?;
        Ok(())
    }

    /// One request over the worker's control socket (`{"cmd": …}`), and its reply.
    ///
    /// # Errors
    ///
    /// When the worker cannot be reached or answers something that is not JSON.
    pub async fn ctl(&self, request: &Value) -> Result<Value> {
        ctl(&self.path("worker.sock"), request).await
    }

    /// The worker's process id (for reading its CPU time).
    #[must_use]
    pub fn worker_pid(&self) -> Option<u32> {
        self.children.get(1).and_then(Child::id)
    }

    /// The transcript a played agent writes ([`Self::play_hook`]).
    #[must_use]
    pub fn transcript_path(&self) -> PathBuf {
        self.path("agent.jsonl")
    }

    /// Play a Claude Code hook in `session` (the id from the dump): write [`TRANSCRIPT`] under
    /// the run's directory and hand the worker the payload for `event` (plus `fields`, more JSON
    /// members) naming it over its control socket, exactly what `slopty hook` relays from the
    /// agent's shell. Nothing is typed into the shell: the agent is simulated from the test.
    ///
    /// # Errors
    ///
    /// When the transcript cannot be written or the worker refuses the hook.
    pub async fn play_hook(&self, session: &str, event: &str, fields: &str) -> Result<()> {
        let transcript = self.transcript_path();
        std::fs::write(&transcript, TRANSCRIPT)?;
        let payload = format!(
            r#"{{"hook_event_name":"{event}","session_id":"e2e","transcript_path":"{}"{fields}}}"#,
            transcript.display()
        );
        let request = json!({ "cmd": "hook", "session": session, "payload": payload });
        let reply = ctl(&self.path("worker.sock"), &request).await?;
        anyhow::ensure!(
            reply.get("reply").and_then(Value::as_str) == Some("ok"),
            "the worker refused the hook: {reply}"
        );
        Ok(())
    }

    /// Open a window on this machine that never draws, for the refresh-storm guard: the helper
    /// ([`crate`]'s `slopty-idle-window` binary) owns it, so no window belonging to anything else
    /// is touched. It is on screen when it returns — the app's picker lists on-screen windows
    /// only — and [`IdleWindow::hide`] takes it away again.
    ///
    /// # Errors
    ///
    /// When the helper is not built or its window does not open.
    pub async fn start_idle_window(&mut self) -> Result<IdleWindow> {
        let markers = self.path("idle-window");
        std::fs::create_dir_all(&markers)?;
        let title = format!("slopty idle {}", std::process::id());
        let child = Command::new(bin("slopty-idle-window")?)
            .arg(&markers)
            .arg(&title)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("spawn slopty-idle-window")?;
        self.children.push(child);
        let ready = markers.join("ready");
        let deadline = std::time::Instant::now()
            .checked_add(STARTUP)
            .ok_or_else(|| anyhow::anyhow!("a deadline inside the clock"))?;
        while !ready.exists() {
            anyhow::ensure!(std::time::Instant::now() < deadline, "the idle window never opened");
            tokio::time::sleep(POLL).await;
        }
        Ok(IdleWindow { markers, title })
    }

    /// The worker's live screen streams as `slopty worker screens` reads them, straight off the
    /// worker's control socket.
    ///
    /// # Errors
    ///
    /// When the socket is not there or the worker answers something else.
    pub async fn worker_screens(&self) -> Result<Vec<Value>> {
        let reply = ctl(&self.path("worker.sock"), &json!({ "cmd": "screens" })).await?;
        let live = reply.get("live").and_then(Value::as_array).cloned();
        live.ok_or_else(|| anyhow::anyhow!("the worker did not list its screens: {reply}"))
    }

    /// Ask the app to quit, then kill whatever is left.
    pub async fn shutdown(mut self) {
        let _quit = self.driver.call(&crate::Command::Quit).await;
        if let Some(simulator) = &self.simulator {
            let _terminated = Command::new("xcrun")
                .args(["simctl", "terminate", &simulator.udid, &simulator.bundle_id])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
        for child in &mut self.children {
            let _killed = child.start_kill();
            let _reaped = child.wait().await;
        }
        #[cfg(target_os = "macos")]
        release_pasteboards(self.dir.path());
    }
}

/// Give the run's named pasteboards back to the system once its processes are gone.
#[cfg(target_os = "macos")]
fn release_pasteboards(root: &Path) {
    for who in ["worker", "app", "b"] {
        slopty_platform::pasteboard::MacPasteboard::named(&pasteboard_name(root, who)).release();
    }
}

impl SecondApp {
    /// Ask the app to quit, then kill whatever is left.
    pub async fn shutdown(mut self) {
        let _quit = self.driver.call(&crate::Command::Quit).await;
        if let Some(simulator) = &self.simulator {
            let _terminated = Command::new("xcrun")
                .args(["simctl", "terminate", &simulator.udid, &simulator.bundle_id])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
        if let Some(mut child) = self.child.take() {
            let _killed = child.start_kill();
            let _reaped = child.wait().await;
        }
    }
}

impl Pair {
    /// Both drivers at once: `a` (the stack's app) and `b`.
    pub const fn drivers(&mut self) -> (&mut Driver, &mut Driver) {
        (&mut self.stack.driver, &mut self.b.driver)
    }

    /// Shut both apps down, then the daemons.
    pub async fn shutdown(self) {
        self.b.shutdown().await;
        self.stack.shutdown().await;
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _killed = child.start_kill();
        }
    }
}

/// A second worker on another machine, reached over ssh.
///
/// `slopty-ptyd` and `slopty-worker` run under one temporary root there, so the app can add
/// two workers at once and the cross-worker attention path can be driven against a real remote
/// daemon.
///
/// Everything it creates on the remote lives under `Self::root` and is torn down on
/// [`Self::shutdown`] (and best-effort on drop). The remote's own `~/.claude` is never touched:
/// the daemons and the hook relay run with a private `HOME` under the root, and
/// `slopty hook install` is never run — so no user settings file is written.
#[derive(Debug)]
pub struct RemoteWorker {
    /// ssh destination (`$SLOPTY_WORKER2`).
    ssh: String,
    /// The remote temp root; everything the worker creates lives under it.
    root: String,
    /// The remote binary directory (`root/bin`).
    bin: String,
    /// The remote worker control socket.
    ctl_sock: String,
    /// The remote private `HOME` (under [`Self::root`]).
    home: String,
    /// The remote transcript path, rewritten before each played hook.
    transcript: String,
    /// The fixed UDP port the worker binds, so a restart is reachable at the same address.
    port: u16,
    /// ptyd's and the worker's pids on the remote, killed on teardown; the worker's is replaced by
    /// [`Self::restart_worker`].
    ptyd_pid: u32,
    worker_pid: u32,
    /// The display name the worker was given (what the app's switcher shows for this worker).
    name: String,
}

/// Run `script` on `ssh` (through the login shell) and return its trimmed stdout.
async fn ssh_out(ssh: &str, script: &str) -> Result<String> {
    // `kill_on_drop`: when the timeout wins, the child inside the dropped `output()` future is
    // killed rather than left running under nobody.
    let output = tokio::time::timeout(
        STARTUP,
        Command::new("ssh")
            .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"])
            .arg(ssh)
            .arg(script)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .with_context(|| format!("ssh {ssh} timed out running: {script}"))?
    .with_context(|| format!("spawn ssh {ssh}"))?;
    ensure!(
        output.status.success(),
        "ssh {ssh} failed ({}): {script}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Feed `input` to `script`'s stdin on `ssh`; return once it exits.
async fn ssh_pipe(ssh: &str, script: &str, input: &[u8]) -> Result<()> {
    let mut child = Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"])
        .arg(ssh)
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawn ssh {ssh}"))?;
    let mut stdin = child.stdin.take().context("ssh stdin")?;
    stdin.write_all(input).await?;
    stdin.shutdown().await?;
    drop(stdin);
    let output = tokio::time::timeout(STARTUP, child.wait_with_output())
        .await
        .with_context(|| format!("ssh {ssh} timed out: {script}"))??;
    ensure!(
        output.status.success(),
        "ssh {ssh} failed ({}): {script}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// gzip a local binary and gunzip it into place on the remote, `chmod +x`.
async fn copy_bin(ssh: &str, local: &Path, remote: &str) -> Result<()> {
    let mut gz = Command::new("gzip")
        .arg("-1")
        .arg("-c")
        .arg(local)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .context("spawn gzip")?;
    let mut gz_out = gz.stdout.take().context("gzip stdout")?;
    let script =
        format!("gunzip -c > {remote}.tmp && mv {remote}.tmp {remote} && chmod +x {remote}");
    let mut ssh = Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"])
        .arg(ssh)
        .arg(&script)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("spawn ssh (copy)")?;
    let mut ssh_in = ssh.stdin.take().context("ssh stdin")?;
    tokio::io::copy(&mut gz_out, &mut ssh_in).await.context("stream binary over ssh")?;
    ssh_in.shutdown().await?;
    drop(ssh_in);
    let gz_status = gz.wait().await?;
    ensure!(gz_status.success(), "gzip {}", local.display());
    let output = ssh.wait_with_output().await?;
    ensure!(
        output.status.success(),
        "copy {} to {remote} ({}): {}",
        local.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// The remote teardown: kill the daemons we started, then anything else under `root`, then the
/// root itself. `gentle` sends TERM first and gives the daemons 300 ms before KILL.
///
/// A pid of 0 is "never started" (the launch failed before that daemon came up) and is left out:
/// `kill -9 0` would signal the cleanup shell's own process group and end the script before the
/// `pkill`/`rm -rf` that follow.
fn teardown_script(pids: &[u32], root: &str, gentle: bool) -> String {
    let pids: Vec<String> = pids.iter().filter(|&&p| p != 0).map(u32::to_string).collect();
    let kill = if pids.is_empty() {
        String::new()
    } else {
        let pids = pids.join(" ");
        let term =
            if gentle { format!("kill {pids} 2>/dev/null; sleep 0.3; ") } else { String::new() };
        format!("{term}kill -9 {pids} 2>/dev/null; ")
    };
    let pattern = self_excluding(root);
    format!("{kill}pkill -9 -f {pattern} 2>/dev/null; rm -rf {root}; true")
}

/// A `pgrep -f`/`pkill -f` pattern for everything started from `root`'s `bin/` that does not
/// match the shell running the script itself: the remote runs `zsh -c "<script>"`, whose
/// command line holds every literal in the script, so a plain `pkill -f /tmp/…` killed that
/// shell first and ssh came back with SIGKILL. `/[b]in/` matches the daemons' paths
/// (`…/bin/slopty-worker`) while the script's own text, which spells it with the brackets and
/// names the root bare only in `rm -rf`, no longer does.
fn self_excluding(root: &str) -> String {
    format!("{root}/[b]in/")
}

/// The two-worker suite's gate, from the two environment variables.
///
/// `Ok(None)` when `SLOPTY_WORKER2_E2E` is unset (the suite skips), `Ok(Some(ssh))` when it is set
/// and `SLOPTY_WORKER2` names the second machine, and an error when the gate is on but the machine
/// is missing, so an enabled suite can never pass by doing nothing.
///
/// # Errors
///
/// When `gate` is set and `worker2` is unset or empty.
pub fn worker2_gate(gate: Option<&str>, worker2: Option<&str>) -> Result<Option<String>> {
    if gate.is_none() {
        return Ok(None);
    }
    match worker2 {
        Some(name) if !name.trim().is_empty() => Ok(Some(name.trim().to_owned())),
        _ => bail!(
            "SLOPTY_WORKER2_E2E is set but SLOPTY_WORKER2 is empty: name the second machine \
             (its ssh destination) or unset SLOPTY_WORKER2_E2E"
        ),
    }
}

/// The address the app adds the second worker by: `explicit` (`SLOPTY_WORKER2_ADDR`) when set,
/// else the host part of the ssh destination `ssh` (`user@host` or `host`), with `port` unless
/// the address names its own.
fn remote_address(explicit: Option<&str>, ssh: &str, port: u16) -> String {
    let host = explicit
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| ssh.rsplit_once('@').map_or(ssh, |(_user, host)| host));
    if host.parse::<std::net::SocketAddr>().is_ok()
        || host.rsplit_once(':').is_some_and(|(h, p)| !h.contains(':') && p.parse::<u16>().is_ok())
    {
        host.to_owned()
    } else if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

impl RemoteWorker {
    /// Copy the daemons and the `slopty` CLI to `ssh`, start ptyd and the worker there under a
    /// fresh temp root with a private `HOME`, and return the worker with the address the app
    /// adds it by: `SLOPTY_WORKER2_ADDR` when set (a tailnet or LAN IP, when the ssh
    /// destination is an alias no resolver knows), else the worker part of the ssh destination,
    /// on the fixed port.
    ///
    /// # Errors
    ///
    /// When ssh is unreachable, a binary is missing, or a daemon does not come up.
    pub async fn launch(ssh: &str) -> Result<(Self, String)> {
        let root = format!("/tmp/slopty-e2e/worker2-{}", std::process::id());
        let bin = format!("{root}/bin");
        let home = format!("{root}/home");
        let ctl_sock = format!("{root}/worker.sock");
        let transcript = format!("{root}/agent.jsonl");
        // The daemons must never read the remote user's real HOME; assert the private one is
        // under our own temp root before anything runs there.
        ensure!(home.starts_with(&root), "private HOME {home} is not under the temp root {root}");
        // A fixed port so a restart (the mid-stream-kill scenario) is reachable at the same
        // address the app stored when it added the worker.
        let port = 45_560;
        let name = "macbook".to_owned();

        // The guard exists before the first byte lands on the remote: a missing binary or a
        // failed copy below drops it, and `Drop` removes whatever was already created there.
        let mut worker = Self {
            ssh: ssh.to_owned(),
            root,
            bin,
            ctl_sock,
            home,
            transcript,
            port,
            ptyd_pid: 0,
            worker_pid: 0,
            name,
        };
        let (root, bin) = (worker.root.clone(), worker.bin.clone());
        let home = worker.home.clone();
        ssh_out(
            ssh,
            &format!("rm -rf {root} && mkdir -p {bin} {home} {root}/data {root}/terminfo"),
        )
        .await
        .context("prepare the remote temp root")?;

        let dir = bin_dir()?;
        for name in ["slopty-ptyd", "slopty-worker", "slopty"] {
            let local = dir.join(name);
            ensure!(local.exists(), "{} is not built (add `-p slopty-cli`?)", local.display());
            copy_bin(ssh, &local, &format!("{bin}/{name}")).await?;
        }

        worker.ptyd_pid = worker.start_ptyd().await?;
        worker.worker_pid = worker.start_worker().await?;
        worker.wait_answering().await?;
        let address =
            remote_address(std::env::var("SLOPTY_WORKER2_ADDR").ok().as_deref(), ssh, worker.port);
        Ok((worker, address))
    }

    /// The common environment for a remote daemon: a private HOME, a terminfo dir and a
    /// pasteboard of its own, so the run never reads that Mac's clipboard.
    fn daemon_env(&self) -> String {
        let terminfo = format!("{}/terminfo", self.root);
        let board = pasteboard_name(Path::new(&self.root), "worker2");
        format!(
            "HOME={} {}={terminfo} TERMINFO_DIRS={terminfo}: RUST_LOG=info \
             {PASTEBOARD_ENV}={board} SLOPTY_DROP_DIR={}/drops",
            self.home,
            slopty_pty::terminfo::DIR_ENV,
            self.root,
        )
    }

    /// Start ptyd on the remote (backgrounded), returning its pid.
    async fn start_ptyd(&self) -> Result<u32> {
        let env = self.daemon_env();
        let (root, bin) = (&self.root, &self.bin);
        let pid = ssh_out(
            &self.ssh,
            &format!(
                "{env} nohup {bin}/slopty-ptyd --socket {root}/ptyd.sock \
                 >{root}/ptyd.log 2>&1 & echo $!"
            ),
        )
        .await?;
        self.wait_remote_socket(&format!("{root}/ptyd.sock"), "remote ptyd").await?;
        pid.trim().parse().with_context(|| format!("ptyd pid: {pid:?}"))
    }

    /// Start the worker on the remote (backgrounded, on a fixed port so a restart keeps the same
    /// address), returning its pid. It listens on every interface, so the app reaches it over
    /// the mesh or the LAN, whichever the address names.
    async fn start_worker(&self) -> Result<u32> {
        let env = self.daemon_env();
        let (root, bin, port, name) = (&self.root, &self.bin, self.port, &self.name);
        let pid = ssh_out(
            &self.ssh,
            &format!(
                "{env} SLOPTY_WORKER_NAME={name} \
                 nohup {bin}/slopty-worker --ptyd-socket {root}/ptyd.sock \
                 --ctl-socket {root}/worker.sock --data-dir {root}/data --port {port} \
                 >{root}/worker.log 2>&1 & echo $!"
            ),
        )
        .await?;
        self.wait_remote_socket(&self.ctl_sock, "remote worker").await?;
        pid.trim().parse().with_context(|| format!("worker pid: {pid:?}"))
    }

    /// Kill the remote worker (ptyd and the sessions stay), as a worker crash would.
    ///
    /// # Errors
    ///
    /// When the kill cannot be sent.
    pub async fn kill_worker(&self) -> Result<()> {
        ssh_out(&self.ssh, &format!("kill {} 2>/dev/null; true", self.worker_pid)).await?;
        Ok(())
    }

    /// Start the worker again on the same data dir and port (same identity, reachable at the same
    /// address): the app reconnects on its own.
    ///
    /// # Errors
    ///
    /// When the worker does not come back up.
    pub async fn restart_worker(&mut self) -> Result<()> {
        // The dead daemon left its control socket on disk; drop it so the readiness poll waits
        // for the new process to bind rather than seeing the stale file.
        ssh_out(&self.ssh, &format!("rm -f {}", self.ctl_sock)).await?;
        self.worker_pid = self.start_worker().await?;
        Ok(())
    }

    /// Poll for a socket file on the remote.
    async fn wait_remote_socket(&self, path: &str, what: &str) -> Result<()> {
        let deadline = tokio::time::Instant::now().checked_add(STARTUP);
        loop {
            if ssh_out(&self.ssh, &format!("test -S {path} && echo ok || true")).await? == "ok" {
                return Ok(());
            }
            if deadline.is_some_and(|d| tokio::time::Instant::now() >= d) {
                bail!("{what} did not bind {path} within {STARTUP:?}");
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Wait until the remote worker answers on its control socket. The socket file appears
    /// before the daemon answers on it (over a fast link the first call can land in that gap),
    /// so a refused call is retried within [`STARTUP`]; the last error carries the remote
    /// The worker's log tail, which the guard's teardown would otherwise take with it.
    async fn wait_answering(&self) -> Result<()> {
        let script =
            format!("SLOPTY_WORKER_SOCKET={} {}/slopty worker status", self.ctl_sock, self.bin);
        let deadline = tokio::time::Instant::now().checked_add(STARTUP);
        loop {
            match ssh_out(&self.ssh, &script).await {
                Ok(status) => {
                    ensure!(
                        status.contains(&self.name),
                        "remote worker answered as someone else: {status:?}"
                    );
                    return Ok(());
                }
                Err(e) if deadline.is_some_and(|d| tokio::time::Instant::now() >= d) => {
                    let log =
                        ssh_out(&self.ssh, &format!("tail -n 20 {}/worker.log || true", self.root))
                            .await
                            .unwrap_or_default();
                    return Err(e.context(format!("remote worker log:\n{log}")));
                }
                Err(_) => tokio::time::sleep(POLL).await,
            }
        }
    }

    /// Play a Claude Code hook for `session` (the id from the dump) against the remote worker,
    /// through the real `slopty hook` relay over ssh: write [`TRANSCRIPT`] on the remote, then
    /// run the relay with `SLOPTY_SESSION` set and the payload on its stdin, exactly what Claude
    /// Code does. Nothing is typed into a shell and no real agent is started.
    ///
    /// # Errors
    ///
    /// When the transcript cannot be written or the relay cannot be run.
    pub async fn play_hook(&self, session: &str, event: &str, fields: &str) -> Result<()> {
        ssh_pipe(&self.ssh, &format!("cat > {}", self.transcript), TRANSCRIPT.as_bytes()).await?;
        let payload = format!(
            r#"{{"hook_event_name":"{event}","session_id":"e2e","transcript_path":"{}"{fields}}}"#,
            self.transcript
        );
        // The relay always exits 0 (it must never block the agent); the effect is asserted
        // through the app's dump, not this call's status.
        ssh_pipe(
            &self.ssh,
            &format!(
                "SLOPTY_SESSION={session} SLOPTY_WORKER_SOCKET={} {}/slopty hook",
                self.ctl_sock, self.bin
            ),
            payload.as_bytes(),
        )
        .await
    }

    /// The remote's `slopty` processes still running under our root (should be empty after
    /// [`Self::shutdown`]); the `pgrep -lf` lines, for the report.
    ///
    /// # Errors
    ///
    /// When ssh cannot be reached.
    pub async fn stray_processes(&self) -> Result<String> {
        ssh_out(&self.ssh, &format!("pgrep -lf {} || true", self_excluding(&self.root))).await
    }

    /// Kill the remote daemons, remove the temp root, and confirm nothing of ours is left.
    ///
    /// # Errors
    ///
    /// When ssh cannot be reached or a process survives.
    pub async fn shutdown(self) -> Result<()> {
        let script = teardown_script(&[self.ptyd_pid, self.worker_pid], &self.root, true);
        ssh_out(&self.ssh, &script).await?;
        let stray =
            ssh_out(&self.ssh, &format!("pgrep -f {} | wc -l", self_excluding(&self.root))).await?;
        ensure!(stray.trim() == "0", "remote processes survived teardown: {stray}");
        Ok(())
    }
}

impl Drop for RemoteWorker {
    fn drop(&mut self) {
        // Best-effort synchronous cleanup if `shutdown` was not called (a panicking test).
        let script = teardown_script(&[self.ptyd_pid, self.worker_pid], &self.root, false);
        let _best_effort = std::process::Command::new("ssh")
            .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"])
            .arg(&self.ssh)
            .arg(&script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Check that the terminal face is the bundled `JetBrains Mono` as its tables say.
///
/// Measured through the platform text system: no line gap, the underline 0.155 em below the
/// baseline and 0.05 em thick (`post`), so the app derives the cell from the font, not from
/// ghostty's estimates. Same on macOS and iOS.
///
/// # Errors
/// When the grid has not been laid out or the face carries an estimate instead of the font.
pub fn check_jetbrains_mono_face(face: Option<&crate::FaceInfo>) -> Result<()> {
    let Some(face) = face else { bail!("the grid has not been laid out: no face") };
    ensure!(face.size > 0.0, "{face:?}");
    let em = |v: f32| v / face.size;
    ensure!((em(face.ascent) - 1.020).abs() < 0.01, "hhea ascender 1020: {face:?}");
    ensure!((em(face.descent) + 0.300).abs() < 0.01, "hhea descender -300: {face:?}");
    ensure!(face.line_gap.abs() < f32::EPSILON, "no line gap: {face:?}");
    let Some(position) = face.underline_position else {
        bail!("post underlinePosition not read: {face:?}")
    };
    let Some(thickness) = face.underline_thickness else {
        bail!("post underlineThickness not read: {face:?}")
    };
    ensure!((em(position) + 0.155).abs() < 0.005, "post underlinePosition -155: {face:?}");
    ensure!((em(thickness) - 0.050).abs() < 0.005, "post underlineThickness 50: {face:?}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{loopback_address, remote_address, self_excluding, teardown_script, worker2_gate};

    /// The kill pattern must match the daemons under the root but not the script that holds
    /// the pattern (the remote shell's own command line).
    #[test]
    fn the_kill_pattern_spares_the_shell_that_runs_it() {
        let pattern = self_excluding("/tmp/slopty-e2e/worker2-1");
        assert_eq!(pattern, "/tmp/slopty-e2e/worker2-1/[b]in/");
        let re = regex::Regex::new(&pattern).unwrap();
        assert!(re.is_match("/tmp/slopty-e2e/worker2-1/bin/slopty-worker --port 45560"));
        assert!(re.is_match("/tmp/slopty-e2e/worker2-1/bin/slopty hook"));
        assert!(!re.is_match(&teardown_script(&[7], "/tmp/slopty-e2e/worker2-1", true)));
        assert!(!re.is_match(&format!("pgrep -lf {pattern} || true")));
    }

    #[test]
    fn the_app_dials_loopback_on_the_port_the_worker_printed() {
        assert_eq!(loopback_address("[::]:53211\n").unwrap(), "127.0.0.1:53211");
        assert_eq!(loopback_address("0.0.0.0:7").unwrap(), "127.0.0.1:7");
        loopback_address("not an address").unwrap_err();
    }

    #[test]
    fn the_second_worker_is_added_by_its_ssh_host_unless_an_address_is_given() {
        assert_eq!(remote_address(None, "macbook-pro", 45_560), "macbook-pro:45560");
        assert_eq!(remote_address(None, "me@100.64.0.5", 45_560), "100.64.0.5:45560");
        assert_eq!(remote_address(Some("192.168.1.7"), "macbook-pro", 45_560), "192.168.1.7:45560");
        assert_eq!(remote_address(Some("10.0.0.2:9"), "x", 45_560), "10.0.0.2:9");
        assert_eq!(remote_address(Some("fd7a:115c:a1e0::5"), "x", 1), "[fd7a:115c:a1e0::5]:1");
        assert_eq!(remote_address(Some(" "), "host", 1), "host:1", "blank is unset");
    }

    #[test]
    fn an_unstarted_daemon_is_not_in_the_kill_list() {
        let s = teardown_script(&[0, 4242], "/tmp/slopty-e2e/worker2-1", false);
        assert_eq!(
            s,
            "kill -9 4242 2>/dev/null; pkill -9 -f /tmp/slopty-e2e/worker2-1/[b]in/ 2>/dev/null; rm -rf /tmp/slopty-e2e/worker2-1; true"
        );
        let s = teardown_script(&[0, 0], "/tmp/r", true);
        assert!(s.starts_with("pkill "), "no kill of pids when nothing started: {s}");
        assert!(!s.contains(" 0 "), "pid 0 must never be signalled: {s}");
        assert!(s.ends_with("rm -rf /tmp/r; true"));
    }

    #[test]
    fn a_gentle_teardown_terms_before_it_kills() {
        let s = teardown_script(&[7, 8], "/tmp/r", true);
        assert!(s.starts_with("kill 7 8 2>/dev/null; sleep 0.3; kill -9 7 8 2>/dev/null; "));
    }

    #[test]
    fn the_gate_refuses_to_run_without_a_second_machine() {
        assert!(worker2_gate(None, None).unwrap().is_none());
        assert!(worker2_gate(None, Some("mac")).unwrap().is_none());
        assert_eq!(
            worker2_gate(Some("1"), Some(" macbook-pro ")).unwrap().as_deref(),
            Some("macbook-pro")
        );
        worker2_gate(Some("1"), None).unwrap_err();
        worker2_gate(Some("1"), Some("")).unwrap_err();
    }
}

/// The MCP protocol revision the server speaks: stateless, no `initialize`.
pub const MCP_REVISION: &str = "2026-07-28";

/// `slopty-server` from this build, on ephemeral ports and with its own data directory. Killed
/// on drop.
#[derive(Debug)]
pub struct ServerDaemon {
    child: Child,
    address: String,
    mcp: std::net::SocketAddr,
    data_dir: PathBuf,
}

impl ServerDaemon {
    /// Start the server named `name` on ports of its choosing, keeping its state in `data_dir`,
    /// and return once it has printed where both listeners are bound.
    ///
    /// # Errors
    ///
    /// When the binary is missing, or the server dies or does not listen in time.
    pub async fn start(data_dir: &Path, name: &str, log: &str) -> Result<Self> {
        let mut child = Command::new(bin("slopty-server")?)
            .args(["--port", "0", "--mcp-port", "0", "--print-addr"])
            .arg("--data-dir")
            .arg(data_dir)
            .arg("--name")
            .arg(name)
            .env("RUST_LOG", log)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .context("spawn slopty-server")?;
        let stdout = child.stdout.take().context("slopty-server stdout")?;
        let mut line = String::new();
        tokio::time::timeout(STARTUP, BufReader::new(stdout).read_line(&mut line))
            .await
            .context("slopty-server did not print its addresses in time")??;
        let bound: Value = serde_json::from_str(line.trim())
            .with_context(|| format!("slopty-server printed {line:?}"))?;
        let port = |key: &str| -> Result<u16> {
            let addr = bound.get(key).and_then(Value::as_str).unwrap_or_default();
            let addr: std::net::SocketAddr =
                addr.parse().with_context(|| format!("slopty-server printed {line:?}"))?;
            Ok(addr.port())
        };
        let (quic, mcp) = (port("quic")?, port("mcp")?);
        let address = format!("127.0.0.1:{quic}");
        let mcp = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, mcp));
        Ok(Self { child, address, mcp, data_dir: data_dir.to_path_buf() })
    }

    /// `127.0.0.1:<port>`: what a worker's and the CLI's `--server` take.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The MCP endpoint on loopback (`http://<mcp>/mcp`).
    #[must_use]
    pub const fn mcp(&self) -> std::net::SocketAddr {
        self.mcp
    }

    /// Where it keeps `workers.json`.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Kill the server (SIGKILL) and reap it.
    pub async fn kill(&mut self) {
        let _killed = self.child.start_kill();
        let _reaped = self.child.wait().await;
    }
}

/// ptyd + worker from this build, registered with a server as one worker. Killed on drop.
///
/// Its sockets and data live under the root it was started in, so a restarted worker keeps its
/// worker id and finds the same ptyd.
#[derive(Debug)]
pub struct Worker {
    ptyd: Child,
    daemon: Option<Child>,
    root: PathBuf,
    name: String,
    server: String,
    log: String,
    env: Vec<(String, String)>,
    address: String,
}

impl Worker {
    /// Start ptyd and the worker named `name` in `root`, registering with the server at `server`;
    /// `env` goes to both daemons, and so to the shells.
    ///
    /// # Errors
    ///
    /// When a binary is missing or a daemon does not come up.
    pub async fn start(
        root: &Path,
        name: &str,
        server: &str,
        log: &str,
        env: &[(&str, &str)],
    ) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        let ptyd = spawn_ptyd(root, log, env).await?;
        let (worker, address) = spawn_worker(root, name, log, env, Some(server)).await?;
        Ok(Self {
            ptyd,
            daemon: Some(worker),
            root: root.to_path_buf(),
            name: name.to_owned(),
            server: server.to_owned(),
            log: log.to_owned(),
            env: env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect(),
            address,
        })
    }

    /// The name it registers under.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Where clients reach the worker directly, `127.0.0.1:<port>`; a restart picks a new port.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Kill the worker (SIGKILL: no goodbye to the server, as a crash or a pulled cable would leave
    /// it) and reap it. ptyd and its shells live on.
    pub async fn kill_worker(&mut self) {
        if let Some(mut worker) = self.daemon.take() {
            let _killed = worker.start_kill();
            let _reaped = worker.wait().await;
        }
    }

    /// Start the worker again on the same ptyd and data directory (so the same worker id).
    ///
    /// # Errors
    ///
    /// When the worker does not come up.
    pub async fn restart_worker(&mut self) -> Result<()> {
        self.kill_worker().await;
        let env: Vec<(&str, &str)> = self.env.iter().map(|(k, v)| (&**k, &**v)).collect();
        let (worker, address) =
            spawn_worker(&self.root, &self.name, &self.log, &env, Some(&self.server)).await?;
        self.daemon = Some(worker);
        self.address = address;
        Ok(())
    }

    /// Kill both daemons and reap them.
    pub async fn shutdown(mut self) {
        self.kill_worker().await;
        let _killed = self.ptyd.start_kill();
        let _reaped = self.ptyd.wait().await;
    }
}

/// `slopty … --json` against the server at `server`, with the CLI's own data directory
/// `data_dir` and `stdin` on its standard input: the answer parsed.
///
/// # Errors
///
/// When the CLI exits non-zero (its stderr is in the error), or prints something that is not
/// JSON.
pub async fn slopty_json(
    server: &str,
    data_dir: &Path,
    args: &[&str],
    stdin: &[u8],
) -> Result<Value> {
    let mut child = Command::new(bin("slopty")?)
        .arg("--data-dir")
        .arg(data_dir)
        .args(["--server", server, "--json"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("spawn slopty")?;
    let mut input = child.stdin.take().context("slopty stdin")?;
    input.write_all(stdin).await?;
    drop(input);
    let out = tokio::time::timeout(STARTUP, child.wait_with_output())
        .await
        .with_context(|| format!("slopty {args:?} did not finish within {STARTUP:?}"))??;
    let stderr = String::from_utf8_lossy(&out.stderr);
    ensure!(out.status.success(), "slopty {args:?}: {}: {}", out.status, stderr.trim());
    serde_json::from_slice(&out.stdout).with_context(|| {
        format!("slopty {args:?} printed {:?}", String::from_utf8_lossy(&out.stdout))
    })
}

/// One JSON-RPC request to the MCP endpoint at `addr`, over plain HTTP/1.1; the response.
///
/// It goes the way a stateless client of revision [`MCP_REVISION`] sends it: the revision in
/// `_meta` and the headers, no `initialize`. `params` is an object; `tools/call` names its tool
/// in `params.name`.
///
/// # Errors
///
/// When the endpoint cannot be reached, answers other than 200, or not with one JSON body.
pub async fn mcp_request(
    addr: std::net::SocketAddr,
    id: u64,
    method: &str,
    params: Value,
) -> Result<Value> {
    let mut params = params;
    let meta = json!({
        "io.modelcontextprotocol/protocolVersion": MCP_REVISION,
        "io.modelcontextprotocol/clientInfo": { "name": "slopty-e2e", "version": "0" },
        "io.modelcontextprotocol/clientCapabilities": {},
    });
    params.as_object_mut().context("MCP params are an object")?.insert("_meta".to_owned(), meta);
    let name = params.get("name").and_then(Value::as_str).map(str::to_owned);
    let body =
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
    let mut request = format!(
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nContent-Length: {}\r\n\
         Connection: close\r\nMCP-Protocol-Version: {MCP_REVISION}\r\nMcp-Method: {method}\r\n",
        body.len()
    );
    if let (Some(name), "tools/call") = (&name, method) {
        write!(request, "Mcp-Name: {name}\r\n")?;
    }
    request.push_str("\r\n");
    request.push_str(&body);
    let exchange = async {
        let mut stream = tokio::net::TcpStream::connect(addr).await?;
        stream.write_all(request.as_bytes()).await?;
        let mut response = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut response).await?;
        anyhow::Ok(response)
    };
    let response = tokio::time::timeout(STARTUP, exchange)
        .await
        .with_context(|| format!("MCP {method} did not answer within {STARTUP:?}"))??;
    let response = String::from_utf8(response).context("MCP answered non-UTF-8")?;
    let (head, body) =
        response.split_once("\r\n\r\n").with_context(|| format!("no HTTP head: {response:?}"))?;
    let status = head.split(' ').nth(1).unwrap_or_default();
    ensure!(status == "200", "MCP {method}: HTTP {status}: {body}");
    serde_json::from_str(body).with_context(|| format!("MCP {method} answered {body:?}"))
}

/// A server and one worker registered with it, in a temporary directory: the server, then
/// ptyd + worker dialing it. Everything is killed on drop.
///
/// `root/server` is the server's data directory, `root/worker` holds the worker's sockets and
/// data, and `root/cli` is the CLI's data directory.
#[derive(Debug)]
pub struct ServerStack {
    /// The temporary root.
    pub dir: StackDir,
    /// The server.
    pub server: ServerDaemon,
    /// The worker.
    pub worker: Worker,
}

impl ServerStack {
    /// Start a server, then a worker named `worker_name` registered with it, and return once
    /// the server lists the worker online. The worker's shells get
    /// `BASH_SILENCE_DEPRECATION_WARNING=1`, so a `/bin/bash` prints only its prompt.
    ///
    /// # Errors
    ///
    /// When a binary is missing, a daemon dies, or the worker never comes online.
    pub async fn launch(worker_name: &str) -> Result<Self> {
        let dir = StackDir::new("slopty-e2e-server-")?;
        let root = dir.path();
        let log = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_owned());
        let server_dir = root.join("server");
        std::fs::create_dir_all(&server_dir)?;
        let server = ServerDaemon::start(&server_dir, "e2e-server", &log).await?;
        let env = [("BASH_SILENCE_DEPRECATION_WARNING", "1")];
        let worker =
            Worker::start(&root.join("worker"), worker_name, server.address(), &log, &env).await?;
        let stack = Self { dir, server, worker };
        stack.worker_online(STARTUP).await?;
        Ok(stack)
    }

    /// Where to put a file for this run.
    #[must_use]
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Switch the app's theme to `appearance` (`dark`, `light`) by rewriting its
    /// `settings.toml`, as a person editing the file would; the app sees it within a second.
    ///
    /// # Errors
    ///
    /// When the file cannot be written.
    pub fn set_appearance(&self, appearance: &str) -> Result<()> {
        let path = self.path("app").join("settings.toml");
        std::fs::write(&path, format!("[theme]\nappearance = \"{appearance}\"\n"))?;
        Ok(())
    }

    /// `slopty <args> --server <this server> --json`, parsed.
    ///
    /// # Errors
    ///
    /// As [`slopty_json`].
    pub async fn slopty(&self, args: &[&str]) -> Result<Value> {
        self.slopty_with_stdin(args, b"").await
    }

    /// [`Self::slopty`] with `stdin` on the CLI's standard input (`put`).
    ///
    /// # Errors
    ///
    /// As [`slopty_json`].
    pub async fn slopty_with_stdin(&self, args: &[&str], stdin: &[u8]) -> Result<Value> {
        slopty_json(self.server.address(), &self.path("cli"), args, stdin).await
    }

    /// One MCP request to this server ([`mcp_request`]).
    ///
    /// # Errors
    ///
    /// As [`mcp_request`].
    pub async fn mcp(&self, id: u64, method: &str, params: Value) -> Result<Value> {
        mcp_request(self.server.mcp(), id, method, params).await
    }

    /// The worker as `slopty workers --json` lists it, if it does.
    ///
    /// # Errors
    ///
    /// When the CLI fails.
    pub async fn worker_entry(&self) -> Result<Option<Value>> {
        let workers = self.slopty(&["workers"]).await?;
        let name = self.worker.name();
        let all = workers.as_array().context("workers is a list")?;
        Ok(all.iter().find(|w| w["name"] == name).cloned())
    }

    /// Wait until the server lists the worker with `liveness`, for at most `bound`; its entry.
    ///
    /// # Errors
    ///
    /// When it does not within `bound`.
    pub async fn worker_is(&self, liveness: &str, bound: Duration) -> Result<Value> {
        let started = tokio::time::Instant::now();
        loop {
            let entry = self.worker_entry().await?;
            if let Some(entry) = entry.as_ref().filter(|w| w["liveness"] == liveness) {
                return Ok(entry.clone());
            }
            ensure!(
                started.elapsed() < bound,
                "the worker is not {liveness} after {bound:?}: {entry:?}"
            );
            tokio::time::sleep(POLL).await;
        }
    }

    /// [`Self::worker_is`] online.
    ///
    /// # Errors
    ///
    /// As [`Self::worker_is`].
    pub async fn worker_online(&self, bound: Duration) -> Result<Value> {
        self.worker_is("online", bound).await
    }

    /// Kill the worker, then the server, and reap them.
    pub async fn shutdown(mut self) {
        self.worker.shutdown().await;
        self.server.kill().await;
    }
}
