//! A server, ptyd, a worker registered with the server, and the app, all from this build, in a
//! temporary directory.
//!
//! Binaries come from `SLOPTY_E2E_BIN_DIR` (set by `cargo xtask e2e`), else `target/debug`
//! next to the workspace, and run from a copy in the temporary directory ([`bin_dir`]). Every
//! process gets its own data directory under the temp dir, so nothing installed on the machine is
//! read or written, and everything is killed on drop.

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
/// Goldens show paths under it (a shell's prompt, a tile's title, the breadcrumb), so a random
/// name made each run's frame differ from its golden wherever a path is drawn. The name is a
/// hash of the test's name instead: the same on every run, different between tests. A lock
/// beside it keeps a second run of the same test, another session's, off it; that run takes a
/// random name, which costs it only its goldens.
///
/// A test that passes takes its root and its lock away with it. One that fails keeps its root
/// for inspection, says where on its way out, and marks it (`.kept-for-inspection`); only the
/// newest eight marked roots stay. What a run killed outright left behind goes once it is two
/// hours old, as the next root is made.
#[derive(Debug)]
pub struct StackDir {
    // Declared first so it is deleted before the lock is released.
    dir: tempfile::TempDir,
    lock: Option<(std::fs::File, PathBuf)>,
}

/// How many roots of failed tests stay for inspection: making one more removes the oldest.
const FAILED_KEPT: usize = 8;

/// The file that marks a root kept because its test failed.
const KEPT_MARK: &str = ".kept-for-inspection";

/// What every stack's root and lock is named from, in the temporary directory.
const ROOTS: &str = "slopty-e2e-";

/// Where the stacks' roots are made: one path on every Mac. A root's path shows in the
/// renders (a shell's prompt, a folder's path bar), and `TMPDIR` is a per-user
/// `/var/folders/…` path, so a golden taken on one Mac never matched another's render
/// (CI e2e run 37390615720, `folder`). The name under it is already the test's own.
pub(crate) fn roots_parent() -> PathBuf {
    if cfg!(target_os = "macos") { PathBuf::from("/private/tmp") } else { std::env::temp_dir() }
}

impl StackDir {
    fn new(prefix: &str) -> Result<Self> {
        let parent = roots_parent();
        sweep(&parent);
        // libtest runs each test on a thread named for it.
        let test = std::thread::current().name().filter(|name| *name != "main").map(fnv1a);
        if let Some(hash) = test {
            let name = format!("{prefix}{hash:08x}");
            let lock_path = parent.join(format!("{name}.lock"));
            let lock = std::fs::File::create(&lock_path)?;
            if lock.try_lock().is_ok() {
                match std::fs::remove_dir_all(parent.join(&name)) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e).context("clear the last run's root"),
                }
                let dir =
                    tempfile::Builder::new().prefix(&name).rand_bytes(0).tempdir_in(&parent)?;
                return Ok(Self { dir, lock: Some((lock, lock_path)) });
            }
        }
        let dir = tempfile::Builder::new().prefix(prefix).tempdir_in(&parent)?;
        Ok(Self { dir, lock: None })
    }

    /// The root.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

impl Drop for StackDir {
    #[expect(clippy::print_stderr, reason = "test helper; stderr is the failed test's log")]
    fn drop(&mut self) {
        if !std::thread::panicking() {
            // The root goes as the `TempDir` drops; its lock file goes with it.
            if let Some((_, path)) = &self.lock {
                let _gone = std::fs::remove_file(path);
            }
            return;
        }
        self.dir.disable_cleanup(true);
        let root = self.dir.path();
        let _marked = std::fs::write(root.join(KEPT_MARK), b"");
        eprintln!("the failed test's stack root is kept for inspection: {}", root.display());
        if let Some(parent) = root.parent() {
            drop_oldest_kept(parent);
        }
    }
}

/// The roots kept for inspection under `parent`, newest first, past the [`FAILED_KEPT`] newest
/// removed.
fn drop_oldest_kept(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else { return };
    let mut kept: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(ROOTS))
        .filter_map(|entry| {
            let marked = std::fs::metadata(entry.path().join(KEPT_MARK)).ok()?;
            Some((marked.modified().ok()?, entry.path()))
        })
        .collect();
    kept.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    for (_, old) in kept.into_iter().skip(FAILED_KEPT) {
        let _gone = std::fs::remove_dir_all(old);
    }
}

/// Remove what runs killed outright left under `parent`: roots not kept for inspection, and
/// locks no run holds, untouched for longer than [`PIN_KEPT`]. A test passing removes its own,
/// so these are only what a killed process could not.
fn sweep(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(ROOTS) || name.starts_with("slopty-e2e-bin-") {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .is_ok_and(|at| at.elapsed().is_ok_and(|age| age > PIN_KEPT));
        if !stale {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            if !path.join(KEPT_MARK).exists() {
                let _gone = std::fs::remove_dir_all(&path);
            }
        } else if path.extension().is_some_and(|ext| ext == "lock")
            && std::fs::File::open(&path).is_ok_and(|lock| lock.try_lock().is_ok())
        {
            let _gone = std::fs::remove_file(&path);
        }
    }
}

/// FNV-1a over `name`: short, and stable across toolchains, as `DefaultHasher` is not.
fn fnv1a(name: &str) -> u32 {
    name.bytes().fold(0x811c_9dc5, |h, b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193))
}

/// The running stack: the app reaches its worker as every app does, through the server's
/// directory.
#[derive(Debug)]
pub struct Stack {
    /// The temporary directory (sockets, data dirs, artifacts).
    pub dir: StackDir,
    /// Where the server's directory lists the worker, and so where the app dials it:
    /// `127.0.0.1:<port>`, the port the worker picked, or `[::1]:<port>` where a relay or a cut
    /// stands in front of it.
    pub address: String,
    /// The server the worker registers with; the app follows it unless it is at its first run.
    pub server: ServerDaemon,
    /// The app's settings name the server: written so at launch, or by the app itself once
    /// [`Self::connect_server`] connected it.
    pub follows: bool,
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
/// its own data directory, identity and test socket, following the same server.
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

/// Where the binaries are: under nextest, the run's own pinned set; else a copy of the
/// build's in the temporary directory, or the build's own when the copy cannot be made.
///
/// # Errors
///
/// When neither the environment nor the default location holds them.
pub fn bin_dir() -> Result<PathBuf> {
    static STAGED: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    let built = built_dir()?;
    let run = std::env::var("NEXTEST_RUN_ID").ok().filter(|run| !run.is_empty());
    Ok(STAGED
        .get_or_init(|| {
            let runs = runs_dir(&built);
            if let Some(pin) = run.as_deref().map(|run| runs.join(run)).filter(|pin| pin.is_dir()) {
                return Some(pin);
            }
            let cache = staged(&built)?;
            Some(run.and_then(|run| pinned(&cache, &runs, &run)).unwrap_or(cache))
        })
        .clone()
        .unwrap_or(built))
}

/// Where each run's set of binaries is pinned, beside the copies [`staged`] keeps.
fn runs_dir(built: &Path) -> PathBuf {
    std::env::temp_dir()
        .join(format!("slopty-e2e-bin-{:08x}.runs", fnv1a(&built.to_string_lossy())))
}

/// How long a run's pinned set is kept: longer than any run takes.
const PIN_KEPT: Duration = Duration::from_hours(2);

/// The binaries of nextest run `run`, as the run's first test found them: hard links to the
/// copies in `cache`, in `runs/<run>`, made once and read by every later test of the run.
///
/// Every test is a process of its own, and `target/debug` is shared with every other build on
/// the machine. A build of the workspace's tests rebuilds `slopty-app` there without the `e2e`
/// feature (`apps/slopty/tests/crash.rs` runs the binary), and that app never opens the test
/// socket: each test that copied it after that waited 30 s and failed. With the set pinned, a
/// build elsewhere can no longer change the binaries under a run that has started. A link
/// keeps the file the run started with when [`staged`] replaces a copy, since it renames a new
/// file over the old name. Pins older than [`PIN_KEPT`] are removed as a new one is made.
fn pinned(cache: &Path, runs: &Path, run: &str) -> Option<PathBuf> {
    std::fs::create_dir_all(runs).ok()?;
    let pin = runs.join(run);
    let part = runs.join(format!(".{run}.{}", std::process::id()));
    let linked = std::fs::create_dir_all(&part).is_ok()
        && std::fs::read_dir(cache).ok()?.all(|entry| {
            entry.is_ok_and(|entry| {
                let name = entry.file_name();
                name.to_string_lossy().starts_with('.')
                    || std::fs::hard_link(entry.path(), part.join(&name)).is_ok()
            })
        });
    // Another test of the run may have pinned it first: its set is the run's.
    let placed = linked && std::fs::rename(&part, &pin).is_ok();
    if !placed {
        let _gone = std::fs::remove_dir_all(&part);
    }
    if let Ok(entries) = std::fs::read_dir(runs) {
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .is_ok_and(|at| at.elapsed().is_ok_and(|age| age > PIN_KEPT));
            if old && entry.path() != pin {
                let _gone = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    pin.is_dir().then_some(pin)
}

/// The app the tests start: `slopty-app` built with the self-test.
///
/// It goes under a name only that build makes (`apps/slopty`'s `slopty-app-e2e`, which requires
/// the `e2e` feature). A plain build of the workspace's tests rebuilds `slopty-app` itself
/// without it, and that app never opens the test socket.
pub const APP: &str = "slopty-app-e2e";

/// A Claude Code hook a test plays: `event` in terminal `session`, with `fields` (more JSON
/// members, each led by a comma).
struct Hook<'a> {
    session: &'a str,
    event: &'a str,
    fields: &'a str,
}

/// Write [`TRANSCRIPT`] to `transcript` and hand the worker at `socket` the payload for `hook`
/// naming it, exactly what `slopty hook` relays from the agent's shell.
async fn play_hook(socket: &Path, transcript: &Path, hook: Hook<'_>) -> Result<()> {
    let Hook { session, event, fields } = hook;
    std::fs::write(transcript, TRANSCRIPT)?;
    let payload = format!(
        r#"{{"hook_event_name":"{event}","session_id":"{}","transcript_path":"{}"{fields}}}"#,
        agent_session(session),
        transcript.display()
    );
    let request = json!({ "cmd": "hook", "session": session, "payload": payload });
    let reply = ctl(socket, &request).await?;
    anyhow::ensure!(
        reply.get("reply").and_then(Value::as_str) == Some("ok"),
        "the worker refused the hook: {reply}"
    );
    Ok(())
}

/// The Claude Code session id a played hook in terminal `session` carries: one per test's
/// terminal, so no two tests' agents share a session the worker keeps state for.
#[must_use]
pub fn agent_session(session: &str) -> String {
    format!("e2e-{session}")
}

/// Where the build put the binaries.
fn built_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("SLOPTY_E2E_BIN_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = manifest.join("../../target/debug");
    if dir.join(APP).exists() {
        return Ok(dir);
    }
    bail!("no built binaries: run `cargo xtask e2e app` (builds them and sets SLOPTY_E2E_BIN_DIR)")
}

/// Copy every `slopty*` executable in `built` into a directory of the temporary one, kept
/// between runs and brought up to date by size and modification time; `None` when it cannot be.
///
/// Each VideoToolbox session a process opens checks its binary's signature, and on a volume
/// mounted without ownership (as the repository's is here) that check is not cached: a session
/// took 28–31 s against 0.5 s from the boot volume (`docs/decisions/testing.md`), which no
/// stream test's wait covers. They are copied together because ptyd finds the CLI beside itself.
fn staged(built: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = std::env::temp_dir()
        .join(format!("slopty-e2e-bin-{:08x}", fnv1a(&built.to_string_lossy())));
    std::fs::create_dir_all(&dir).ok()?;
    for entry in std::fs::read_dir(built).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let meta = entry.metadata().ok()?;
        let executable = meta.is_file() && meta.permissions().mode() & 0o111 != 0;
        if !executable || !name.to_string_lossy().starts_with("slopty") {
            continue;
        }
        let target = dir.join(&name);
        let current = std::fs::metadata(&target)
            .is_ok_and(|t| t.len() == meta.len() && t.modified().ok() == meta.modified().ok());
        if current {
            continue;
        }
        // Another test process may be copying the same file: each writes its own and renames it
        // into place, which is atomic.
        let part = dir.join(format!(".{}.{}", name.to_string_lossy(), std::process::id()));
        std::fs::copy(entry.path(), &part).ok()?;
        std::fs::File::options()
            .write(true)
            .open(&part)
            .ok()?
            .set_modified(meta.modified().ok()?)
            .ok()?;
        std::fs::rename(&part, &target).ok()?;
    }
    Some(dir)
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

/// How long a killed process may take to be reaped before it is left behind.
///
/// A process that coded video through a virtual Mac's encoder after the encoder stopped stalls
/// in its exit inside the driver, where no signal ends it (`docs/decisions/video.md`). Waiting on
/// it held a test whose checks had all passed to its timeout, and so every test after it
/// (CI run 37955284234). A process gone in time is gone at once; one still there after this is
/// named with its state, for the run's log, and left.
const REAP: Duration = Duration::from_secs(10);

/// Kill `child` (SIGKILL) and reap it, waiting at most [`REAP`]; one still there then is named
/// as `what`, with its state as `ps` reads it, and left.
#[expect(clippy::print_stderr, reason = "test helper; stderr is the test's log")]
async fn reap(child: &mut Child, what: &str) {
    let pid = child.id();
    let _killed = child.start_kill();
    if tokio::time::timeout(REAP, child.wait()).await.is_ok() {
        return;
    }
    let Some(pid) = pid else { return };
    let state = tokio::time::timeout(
        REAP,
        Command::new("/bin/ps")
            .args(["-o", "pid,stat,wchan,etime,command", "-p", &pid.to_string()])
            .output(),
    )
    .await;
    let state = match state {
        Ok(Ok(out)) => String::from_utf8_lossy(&out.stdout).into_owned(),
        Ok(Err(e)) => format!("ps: {e}"),
        Err(_elapsed) => "ps did not answer".to_owned(),
    };
    eprintln!(
        "e2e: {what} (pid {pid}) was killed but is not gone after {REAP:?}; left as it is:\n{state}"
    );
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
    // The worker may have answered and hung up already, which makes the half-close fail with
    // ENOTCONN; the reply is still there to read.
    let _half_closed = wr.shutdown().await;
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

/// A stack's daemons ([`daemons`]).
struct Daemons {
    server: ServerDaemon,
    /// ptyd, then the worker.
    children: Vec<Child>,
    /// Where the worker listens.
    worker: std::net::SocketAddr,
    /// Where the server's directory lists it.
    listed: String,
}

/// A stack's server in `root/server`, ptyd, and the worker named `worker_name` registered with
/// the server.
///
/// The worker listens on IPv4 loopback alone, so the server lists it there and never at a
/// tailnet address this machine may have. `fronted`, it registers over IPv6 instead: the server
/// lists a worker at the address it registered from and the port it listens on, so the
/// directory then says `[::1]:<its port>`, where a relay or a cut in front of it binds, as
/// [`SecondWorker`]'s does.
async fn daemons(
    root: &Path,
    worker_name: &str,
    log: &str,
    env: &[(&str, &str)],
    fronted: bool,
) -> Result<Daemons> {
    let server_dir = root.join("server");
    std::fs::create_dir_all(&server_dir)?;
    let server = ServerDaemon::start(&server_dir, "e2e-server", log).await?;
    let ptyd = spawn_ptyd(root, log, env).await?;
    let register = if fronted { server.address_v6() } else { server.address().to_owned() };
    let mut worker_env = vec![("SLOPTY_BIND", "127.0.0.1")];
    worker_env.extend_from_slice(env);
    let (worker, address) =
        spawn_worker(root, worker_name, log, &worker_env, Some(&register), 0).await?;
    let direct: std::net::SocketAddr = address.parse().context("the worker's address")?;
    let listed = if fronted {
        std::net::SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, direct.port())).to_string()
    } else {
        address
    };
    Ok(Daemons { server, children: vec![ptyd, worker], worker: direct, listed })
}

/// The daemons' log level: the run's `RUST_LOG`, else `info`.
fn log_level() -> String {
    std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_owned())
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

/// The pasteboard `who` (`worker`, or an app's name) of the run under `root` uses.
///
/// Named after the run and this test process, so no two runs and no two processes share one,
/// and nothing touches the human's clipboard. The process is in the name because a root's name
/// repeats from run to run ([`StackDir`]), and a named pasteboard outlives the run that wrote it.
#[must_use]
pub fn pasteboard_name(root: &Path, who: &str) -> String {
    let run = root.file_name().map_or_else(|| "run".into(), |n| n.to_string_lossy());
    format!("com.aislopware.slopty.e2e.{run}.{}.{who}", std::process::id())
}

/// The shells' zsh configuration: a fixed prompt and nothing else, in `root/zsh`, which ptyd's
/// bootstrap hands its shells as their `ZDOTDIR`.
///
/// A login shell otherwise reads the developer's own rc files. Their prompt landed in every
/// golden, and a plugin run on every key (syntax highlighting) put 2 to 19 ms of its own between
/// a key and its echo (MEASUREMENTS, "keystroke to glass"): numbers about one person's zsh.
fn zsh_env(root: &Path) -> Result<(&'static str, PathBuf)> {
    let dir = root.join("zsh");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(".zshrc"), "PROMPT='%~ %# '\n")?;
    Ok(("ZDOTDIR", dir))
}

/// `program`, started from a clean environment whose home is `home`
/// (`slopty_testkit::env::scrub`): nothing of the developer's (their Claude Code settings and
/// credentials, the Slopty terminal the run may be inside, their `PATH` and dotfiles) reaches a
/// daemon, or the shells and agents it starts. What a run needs it sets after.
fn scrubbed(program: impl AsRef<std::ffi::OsStr>, home: &Path) -> Command {
    let mut command = Command::new(program);
    slopty_testkit::env::scrub(command.as_std_mut(), home);
    // The home the scrub made, by its real path (`/private/var`, not the `/var` link): a shell
    // started with no `PWD` to inherit shortens its directory to `~` only when `HOME` is spelled
    // as `getcwd` spells it. The root keeps the short spelling, for its sockets' sake.
    if let Ok(real) = std::fs::canonicalize(home) {
        command.env("HOME", real);
    }
    command
}

/// ptyd on `root/ptyd.sock`, up once its socket is.
async fn spawn_ptyd(root: &Path, log: &str, env: &[(&str, &str)]) -> Result<Child> {
    let ptyd_sock = root.join("ptyd.sock");
    let (zdotdir, zsh) = zsh_env(root)?;
    let mut ptyd = scrubbed(bin("slopty-ptyd")?, &root.join("home"))
        .arg("--socket")
        .arg(&ptyd_sock)
        // zsh, whatever the account's shell: every wait and golden here is written against the
        // zsh above, and a CI runner's account shell is bash.
        .env("SHELL", "/bin/zsh")
        .env(zdotdir, zsh)
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
/// `port` (0: one of its choosing), registered with `server` when one is given; and the loopback
/// address clients reach it on.
async fn spawn_worker(
    root: &Path,
    worker_name: &str,
    log: &str,
    env: &[(&str, &str)],
    server: Option<&str>,
    port: u16,
) -> Result<(Child, String)> {
    seed_worker_id(&root.join("worker"), worker_name)?;
    let mut command = scrubbed(bin("slopty-worker")?, &root.join("home"));
    command
        .arg("--ptyd-socket")
        .arg(root.join("ptyd.sock"))
        .arg("--ctl-socket")
        .arg(root.join("worker.sock"))
        .arg("--data-dir")
        .arg(root.join("worker"))
        .arg("--print-addr")
        .arg("--port")
        .arg(port.to_string());
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
    // Its output closed with nothing on it: it stopped first, and said why on its stderr.
    if listen.is_empty() {
        let status = tokio::time::timeout(STARTUP, worker.wait()).await;
        bail!("slopty-worker stopped before it printed its address ({status:?}); its log says why");
    }
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
# A run's directory goes when its test ends: a stage still waiting then would spin forever.
stage() {
    while [ ! -f "$SLOPTY_FAKE_CLAUDE_DIR/$1" ]; do
        [ -d "$SLOPTY_FAKE_CLAUDE_DIR" ] || exit 1
        sleep 0.05
    done
}
# The worker asks for the version and for the live sessions at start-up. Waiting on a stage
# there outlived the killed worker as an orphan; answer them at once instead.
[ "$1" = "--version" ] && exit 1
if [ "$1" = "agents" ]; then echo '[]'; exit 0; fi
echo "fake claude in $PWD"
# Its conversation's file is named for its session, as Claude Code's is: the id Slopty pinned
# with `--session-id`, when it did.
id=fake-session
pinned=
for arg in "$@"; do
    [ "$pinned" = "--session-id" ] && id="$arg"
    pinned="$arg"
done
stage working
# OSC 2 with U+25D0 CIRCLE WITH LEFT HALF BLACK, one of the frames Claude Code paints into
# the title while a turn runs (`slopty_agent::title::WORKING`).
printf '\033]2;\342\227\220 Claude Code\007'
stage transcript
project="$HOME/.claude/projects/$(printf '%s' "$PWD" | sed 's/[^a-zA-Z0-9]/-/g')"
mkdir -p "$project"
cat "$SLOPTY_FAKE_CLAUDE_DIR/transcript.jsonl" > "$project/$id.jsonl"
stage done
cat "$SLOPTY_FAKE_CLAUDE_DIR/transcript-done.jsonl" >> "$project/$id.jsonl"
# U+2733 EIGHT SPOKED ASTERISK and the conversation's summary: the title between turns.
printf '\033]2;\342\234\263 fix the tests\007'
stage quit
"#;

/// Start one app process named `name` under `root` (its data directory is `root/<name>`, its
/// test socket `root/<name>.sock`) and connect to its socket. `env` overrides the defaults.
/// The grants every app is told its workers hold unless a test says otherwise: all of them, the
/// state a working Mac is in, whatever this machine granted the worker's binary.
const ALL_GRANTED: &str = "screen-recording,accessibility";

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
    let tailnet = root.join("tailnet.json");
    std::fs::write(&tailnet, EMPTY_TAILNET)?;
    let mut app = Command::new(bin(APP)?)
        .env("RUST_LOG", log)
        .env("SLOPTY_DATA_DIR", &app_dir)
        .env(crate::SOCKET_ENV, &app_sock)
        // Local echo would put predicted text in the rows before the worker confirms it.
        .env("SLOPTY_PREDICT", "never")
        .env(crate::TAILNET_STATUS_ENV, &tailnet)
        .env(crate::WORKER_GRANTS_ENV, ALL_GRANTED)
        // A display that reports no scan-out (a remote-desktop host's virtual one, such as
        // Parsec's) gives every frame a zero presentation time, so a glass-timed wait never
        // ends; GPUI then takes the presented handler as the glass. A display that reports
        // scan-out keeps its own times.
        .env("GPUI_PRESENTED_AT_CALLBACK", "1")
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

/// The tailnet an app under test sees: up, with no node to try. What the machine's own tailnet
/// answers (a worker of its own, a server) would otherwise land in the first-run goldens.
const EMPTY_TAILNET: &str = r#"{"BackendState":"Running","Self":null,"Peer":null}"#;

/// The appearance every app under test starts in, whatever the machine's is: the default
/// (`system`) would make each golden depend on System Settings on the day it runs.
pub const APPEARANCE: &str = "light";

/// Write a `settings.toml` into `app_dir` that pins [`APPEARANCE`] and a steady cursor, unless
/// a test already put one there: a blinking cursor is in a golden or not by the 600 ms phase
/// the frame lands in.
fn pin_appearance(app_dir: &Path) -> Result<()> {
    let path = app_dir.join("settings.toml");
    if !path.exists() {
        std::fs::write(&path, pinned_settings(APPEARANCE))?;
    }
    Ok(())
}

/// An app's settings: [`pinned_settings`], and the server it follows when it has one.
fn app_settings(appearance: &str, server: Option<&ServerDaemon>) -> String {
    let pinned = pinned_settings(appearance);
    match server {
        Some(server) => format!("{pinned}\n[client]\nserver = \"{}\"\n", server.address()),
        None => pinned,
    }
}

/// Point the app whose data directory is `app_dir` at `server`, in [`APPEARANCE`], before it
/// starts: it comes up linked to it, as a set-up Mac's app does.
fn follow(app_dir: &Path, server: &ServerDaemon) -> Result<()> {
    std::fs::create_dir_all(app_dir)?;
    std::fs::write(app_dir.join("settings.toml"), app_settings(APPEARANCE, Some(server)))?;
    Ok(())
}

/// The settings an app under test runs with, in `appearance`.
#[must_use]
pub fn pinned_settings(appearance: &str) -> String {
    format!("[theme]\nappearance = \"{appearance}\"\n\n[terminal]\ncursor_blink = \"never\"\n")
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
        .env(format!("SIMCTL_CHILD_{}", crate::WORKER_GRANTS_ENV), ALL_GRANTED)
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

/// Ping, then wait for the app to connect to the worker its server lists.
async fn linked(driver: &mut Driver) -> Result<()> {
    driver.ok(&crate::Command::Ping).await?;
    driver
        .wait_for("the worker the server lists to connect", STARTUP, |d| {
            d.workers.iter().any(|w| w.status == "connected")
        })
        .await?;
    Ok(())
}

impl Stack {
    /// Start the server, ptyd, the worker (named `worker_name`) registered with the server, and
    /// the app following the server; wait until the app has connected to the worker.
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
        let path = slopty_testkit::env::path_with(&fake);
        let (home, fake_dir) = (home.to_string_lossy(), fake.to_string_lossy());
        let path = path.to_string_lossy();
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

    /// [`Self::launch`] with a `HOME` of the run's own for the daemons and the app (`home`
    /// under the run's root, its real path), so a shell's prompt, title and place name the
    /// directories under it from `~` and none of the machine's temporary path reaches a render.
    ///
    /// # Errors
    ///
    /// As [`Self::launch`], plus when the home cannot be made.
    pub async fn launch_at_home(worker_name: &str) -> Result<Self> {
        let dir = StackDir::new("slopty-e2e-")?;
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home)?;
        // The real path: a shell started in a directory under it reports that one, and its
        // prompt shortens only what starts with `HOME` as written.
        let home = std::fs::canonicalize(&home)?;
        let home = home.to_string_lossy().into_owned();
        Self::launch_in(dir, worker_name, &[("HOME", &home)]).await
    }

    /// [`Self::launch`] up to the app's first frame, before it is pointed at the server: what
    /// someone opening the app for the first time sees. [`Self::connect_server`] goes on from
    /// there.
    ///
    /// # Errors
    ///
    /// As [`Self::launch`].
    pub async fn launch_first_run(worker_name: &str) -> Result<Self> {
        Self::launch_first_run_with(worker_name, |_server, _root| Vec::new()).await
    }

    /// [`Self::launch_first_run`] with extra environment for the app, made once the server and
    /// the worker are up from the server and the run's root (a stand-in for this Mac's worker
    /// that names them, [`crate::THIS_MAC_ENV`]).
    ///
    /// # Errors
    ///
    /// As [`Self::launch`].
    pub async fn launch_first_run_with(
        worker_name: &str,
        env: impl FnOnce(&ServerDaemon, &Path) -> Vec<(String, String)>,
    ) -> Result<Self> {
        let dir = StackDir::new("slopty-e2e-")?;
        let log = log_level();
        let daemons = daemons(dir.path(), worker_name, &log, &[], false).await?;
        let env = env(&daemons.server, dir.path());
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let mut stack = Self::assemble(dir, daemons, &log, &env, false).await?;
        stack.driver.ok(&crate::Command::Ping).await?;
        Ok(stack)
    }

    /// [`Self::launch_with`], but the app reaches the worker through a relay in this process
    /// shaped as `link`, so the connection sees that round trip, jitter and loss: the worker
    /// registers over IPv6, so the server's directory lists `[::1]` on its port, where the relay
    /// binds, and [`Self::address`] is the relay's. The relay
    /// stops when the returned handle drops.
    ///
    /// # Errors
    ///
    /// As [`Self::launch`], or when the relay cannot bind.
    pub async fn launch_shaped(
        worker_name: &str,
        env: &[(&str, &str)],
        link: slopty_shape::Link,
    ) -> Result<(Self, ShapedLink)> {
        let dir = StackDir::new("slopty-e2e-")?;
        let log = log_level();
        let daemons = daemons(dir.path(), worker_name, &log, env, true).await?;
        let at: std::net::SocketAddr = daemons.listed.parse().context("the listed address")?;
        let from = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0));
        let relay =
            slopty_shape::relay::Relay::bind_apart(at, from, daemons.worker, link, 0x5107_7e2e)
                .await
                .context("bind the relay")?;
        let task = tokio::spawn(async move { relay.run().await });
        let stack = Self::assemble(dir, daemons, &log, env, true).await?;
        Ok((stack, ShapedLink { _relay: RelayTask(task) }))
    }

    /// Connect the app at its first run to the stack's server as a person does: its address
    /// typed into the panel's field and Return. Waits for the worker the server lists to
    /// connect.
    ///
    /// # Errors
    ///
    /// When the app refuses the keys or the worker does not connect in time.
    pub async fn connect_server(&mut self) -> Result<()> {
        let address = self.server.address().to_owned();
        self.driver.type_text(&address).await?;
        self.driver.keys("enter").await?;
        linked(&mut self.driver).await?;
        self.follows = true;
        Ok(())
    }

    /// [`Self::launch`] with the worker behind a path the test can cut ([`crate::cut::Cut`]):
    /// the server's directory lists the cut, as [`Self::launch_shaped`]'s relay.
    ///
    /// # Errors
    ///
    /// As [`Self::launch`], and when the cut cannot bind.
    pub async fn launch_behind_cut(worker_name: &str) -> Result<(Self, crate::cut::Cut)> {
        let dir = StackDir::new("slopty-e2e-cut-")?;
        let log = log_level();
        let daemons = daemons(dir.path(), worker_name, &log, &[], true).await?;
        let at: std::net::SocketAddr = daemons.listed.parse().context("the listed address")?;
        let cut = crate::cut::Cut::bind(at, daemons.worker).await?;
        let stack = Self::assemble(dir, daemons, &log, &[], true).await?;
        Ok((stack, cut))
    }

    /// `env` goes to the daemons and the app alike, on top of the defaults.
    async fn launch_in(dir: StackDir, worker_name: &str, env: &[(&str, &str)]) -> Result<Self> {
        let log = log_level();
        let daemons = daemons(dir.path(), worker_name, &log, env, false).await?;
        Self::assemble(dir, daemons, &log, env, true).await
    }

    /// The app beside `daemons`, with `env` on top of its defaults; when it `follows` the
    /// server, its settings say so and this waits for it to connect to the worker.
    async fn assemble(
        dir: StackDir,
        daemons: Daemons,
        log: &str,
        env: &[(&str, &str)],
        follows: bool,
    ) -> Result<Self> {
        let Daemons { server, mut children, listed, .. } = daemons;
        if follows {
            follow(&dir.path().join("app"), &server)?;
        }
        let (app, driver) = spawn_app(dir.path(), "app", log, env).await?;
        let app_ix = Some(children.len());
        children.push(app);
        let app_env = env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
        let mut stack = Self {
            dir,
            address: listed,
            server,
            follows,
            driver,
            children,
            app_ix,
            simulator: None,
            log: log.to_owned(),
            app_env,
        };
        if follows {
            linked(&mut stack.driver).await?;
        }
        Ok(stack)
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
        reap(&mut app, "slopty-app").await;
        Ok(())
    }

    /// Start the app again on the same data directory (same identity, same socket path): it
    /// knows the worker and connects by itself. Waits for the link to be up.
    ///
    /// # Errors
    ///
    /// When the binary is missing or the app does not connect in time.
    pub async fn relaunch_app(&mut self) -> Result<()> {
        self.relaunch_app_unlinked().await?;
        self.driver
            .wait_for("the relaunched app to reconnect", STARTUP, |d| {
                d.workers.iter().any(|w| w.status == "connected")
            })
            .await?;
        Ok(())
    }

    /// [`Self::relaunch_app`] without waiting for a link: for a worker that is not there to
    /// answer ([`Self::kill_worker`]).
    ///
    /// # Errors
    ///
    /// When the binary is missing or the app does not answer its test socket.
    pub async fn relaunch_app_unlinked(&mut self) -> Result<()> {
        let env: Vec<(&str, &str)> =
            self.app_env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        // The killed app left its socket file on disk; remove it so `wait_for_path` waits for
        // the new process to bind rather than connecting to the dead one (Connection refused).
        let _removed = std::fs::remove_file(self.dir.path().join("app.sock"));
        let (app, mut driver) = spawn_app(self.dir.path(), "app", &self.log, &env).await?;
        self.app_ix = Some(self.children.len());
        self.children.push(app);
        driver.ok(&crate::Command::Ping).await?;
        self.driver = driver;
        Ok(())
    }

    /// Kill the worker (SIGKILL, as a crash or a pulled cable would leave it) and reap it;
    /// ptyd and its shells live on. It stays down: the app's dials are refused.
    ///
    /// # Errors
    ///
    /// When there is no worker process to kill.
    pub async fn kill_worker(&mut self) -> Result<()> {
        // The worker is the second child, after ptyd ([`Self::worker_pid`]).
        let worker = self.children.get_mut(1).context("no worker process")?;
        worker.start_kill().context("kill slopty-worker")?;
        reap(worker, "slopty-worker").await;
        Ok(())
    }

    /// Wait until the server's directory lists the worker with `liveness` (`"unreachable"`, say),
    /// for at most `bound`. What the app shows of an away worker depends on whether the server
    /// has noticed yet, so a test that draws it waits for the server first.
    ///
    /// # Errors
    ///
    /// When the CLI fails, or the server does not within `bound`.
    pub async fn server_lists_worker(&self, liveness: &str, bound: Duration) -> Result<()> {
        let cli = self.dir.path().join("cli");
        let started = tokio::time::Instant::now();
        loop {
            let workers = slopty_json(self.server.address(), &cli, &["workers"], b"").await?;
            let listed =
                workers.as_array().is_some_and(|all| all.iter().any(|w| w["liveness"] == liveness));
            if listed {
                return Ok(());
            }
            ensure!(started.elapsed() < bound, "no worker {liveness} after {bound:?}: {workers}");
            tokio::time::sleep(POLL).await;
        }
    }

    /// [`Self::launch`] plus a second app on the same worker: `b` gets its own data directory,
    /// identity and socket, and follows the same server. The
    /// first app is left to open its first shell before the second comes up, so the two do not
    /// both find an empty workspace and open one each.
    ///
    /// # Errors
    ///
    /// As [`Self::launch`], for either app.
    pub async fn launch_pair(worker_name: &str) -> Result<Pair> {
        let mut stack = Self::launch(worker_name).await?;
        stack.wait_first_shell().await?;
        follow(&stack.path("b"), &stack.server)?;
        let (child, mut driver) = spawn_app(stack.dir.path(), "b", &stack.log, &[]).await?;
        linked(&mut driver).await?;
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
        follow(&stack.path("b"), &stack.server)?;
        let mut driver =
            spawn_simulator_app(stack.dir.path(), "b", &stack.log, &simulator, &[]).await?;
        linked(&mut driver).await?;
        Ok(Pair { stack, b: SecondApp { driver, child: None, simulator: Some(simulator) } })
    }

    /// Wait until the first app has its first shell in the workspace with a prompt.
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

    /// Start the server, ptyd and the worker here and the app in a booted simulator (`simctl
    /// launch` with the socket and data dir in its environment; the simulator shares this file
    /// system and its network), following the server, and wait as [`Self::launch`] does.
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
        let mut stack = Self::spawn_on_simulator(worker_name, simulator, env, true).await?;
        linked(&mut stack.driver).await?;
        Ok(stack)
    }

    /// [`Self::launch_on_simulator`] up to the first frame, before the app is pointed at the
    /// server, as [`Self::launch_first_run`].
    ///
    /// # Errors
    ///
    /// As [`Self::launch_on_simulator`].
    pub async fn launch_first_run_on_simulator(
        worker_name: &str,
        simulator: Simulator,
    ) -> Result<Self> {
        let mut stack = Self::spawn_on_simulator(worker_name, simulator, &[], false).await?;
        stack.driver.ok(&crate::Command::Ping).await?;
        Ok(stack)
    }

    /// The daemons here and the app in the simulator, its settings naming the server when it
    /// `follows` it.
    async fn spawn_on_simulator(
        worker_name: &str,
        simulator: Simulator,
        env: &[(&str, &str)],
        follows: bool,
    ) -> Result<Self> {
        let dir = StackDir::new("slopty-e2e-ios-")?;
        let root = dir.path();
        let log = log_level();
        let Daemons { server, children, listed, .. } =
            daemons(root, worker_name, &log, &[], false).await?;
        if follows {
            follow(&root.join("app"), &server)?;
        }
        let driver = spawn_simulator_app(root, "app", &log, &simulator, env).await?;
        let app_env = env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
        Ok(Self {
            dir,
            address: listed,
            server,
            follows,
            driver,
            children,
            app_ix: None,
            simulator: Some(simulator),
            log,
            app_env,
        })
    }

    /// Where to put a file for this run.
    #[must_use]
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Switch the app's theme to `appearance` (`dark`, `light`) by rewriting its
    /// `settings.toml`, the server it follows kept, as a person editing the file would; the app
    /// sees it within a second.
    ///
    /// # Errors
    ///
    /// When the file cannot be written.
    pub fn set_appearance(&self, appearance: &str) -> Result<()> {
        let path = self.path("app").join("settings.toml");
        std::fs::write(&path, app_settings(appearance, self.follows.then_some(&self.server)))?;
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

    /// The transcript a played agent in `session` writes ([`Self::play_hook`]): named for the
    /// agent's session, `<session id>.jsonl`, as Claude Code names its own, since the worker
    /// takes a session's id from either and two that differ would be two sessions.
    #[must_use]
    pub fn transcript_path(&self, session: &str) -> PathBuf {
        self.path(&format!("{}.jsonl", agent_session(session)))
    }

    /// Run the real relay, `slopty hook` (with `args`, such as `statusline --command true`),
    /// for `session` as Claude Code runs it: `payload` on its stdin, the worker's control socket
    /// in its environment, a home and a Claude config directory of the run's own. It is
    /// returned running, since a `PermissionRequest`'s relay waits for the answer; dropping it
    /// kills it. Its stdout, what Claude Code would read back, is piped: a few hundred bytes,
    /// which no pipe holds back.
    ///
    /// # Errors
    ///
    /// When the CLI is not built or does not start.
    pub fn relay_hook(&self, session: &str, args: &[&str], payload: &Value) -> Result<Child> {
        let home = self.path("home");
        std::fs::create_dir_all(&home)?;
        let mut child = scrubbed(bin("slopty")?, &home)
            .arg("--data-dir")
            .arg(self.path("hook-data"))
            .arg("hook")
            .args(args)
            .env("SLOPTY_SESSION", session)
            .env("SLOPTY_WORKER_SOCKET", self.path("worker.sock"))
            .env("CLAUDE_CONFIG_DIR", self.path("claude-config"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .context("spawn slopty hook")?;
        let mut stdin = child.stdin.take().context("the hook's stdin")?;
        let bytes = payload.to_string().into_bytes();
        tokio::spawn(async move {
            // A relay that exits before reading all of it is the relay's to report.
            let _written = stdin.write_all(&bytes).await;
        });
        Ok(child)
    }

    /// Post one batch of Slopty's Claude Code mod events to the worker's mod socket
    /// (`worker.mod.sock`, beside its control socket) as the mod does: `POST /v1/events` over
    /// HTTP/1.1. Returns the answer's status, `204` once the events are on the board.
    ///
    /// # Errors
    ///
    /// When the socket cannot be reached or does not answer within the start-up bound.
    pub async fn post_mod(&self, batch: &Value) -> Result<u16> {
        let body = batch.to_string();
        let head = format!(
            "POST /v1/events HTTP/1.1\r\nhost: slopty\r\ncontent-type: application/json\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        let socket = self.path("worker.mod.sock");
        let exchange = async {
            let mut stream = UnixStream::connect(&socket).await?;
            stream.write_all(head.as_bytes()).await?;
            stream.write_all(body.as_bytes()).await?;
            let mut answer = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut answer).await?;
            anyhow::Ok(answer)
        };
        let answer = tokio::time::timeout(STARTUP, exchange)
            .await
            .with_context(|| format!("{} did not answer", socket.display()))??;
        let answer = String::from_utf8_lossy(&answer);
        answer
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .with_context(|| format!("no HTTP status: {answer:?}"))
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
        let hook = Hook { session, event, fields };
        play_hook(&self.path("worker.sock"), &self.transcript_path(session), hook).await
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
            reap(child, "a process of the stack").await;
        }
        self.server.kill().await;
        #[cfg(target_os = "macos")]
        {
            release_pasteboards(self.dir.path());
            remove_page_store(self.dir.path());
        }
    }
}

/// Take the web data store the app kept for the run's worker out of `~/Library/WebKit`, once
/// the app is gone. Pages keep a store per worker, named by the worker's id
/// (`slopty_platform::web`), and a run's worker is a new one every time.
#[cfg(target_os = "macos")]
fn remove_page_store(root: &Path) {
    if let Some(store) = page_store(root) {
        let _gone = std::fs::remove_dir_all(store);
    }
}

/// Give the worker named `worker_name` in `data_dir` an id of the test's own before its first
/// start, so what is keyed by it (a machine's colour, which is the hash of its id) is the same
/// on every run of the test and a render holds still. The id is the test's name and the
/// worker's, hashed: two tests running side by side never share one, so neither's page store
/// is the other's. A worker started again keeps the id it wrote.
fn seed_worker_id(data_dir: &Path, worker_name: &str) -> Result<()> {
    let path = data_dir.join("worker-id");
    if path.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(data_dir)?;
    let thread = std::thread::current();
    let test = thread.name().unwrap_or_default();
    let key = format!("{test}\n{worker_name}");
    // FNV-1a, 64 bits, from two offsets: 128 bits that are the same in every build.
    let fnv = |offset: u64| {
        key.bytes()
            .fold(offset, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3))
    };
    let id =
        (u128::from(fnv(0xcbf2_9ce4_8422_2325)) << 64) | u128::from(fnv(0x8422_2325_cbf2_9ce4));
    let uuid = format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        id >> 96,
        (id >> 80) & 0xffff,
        (id >> 64) & 0xffff,
        (id >> 48) & 0xffff,
        id & 0xffff_ffff_ffff
    );
    std::fs::write(&path, uuid).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// The run's worker's id, as it writes it.
#[must_use]
pub fn worker_id(root: &Path) -> Option<String> {
    let id = std::fs::read_to_string(root.join("worker").join("worker-id")).ok()?;
    Some(id.trim().to_owned())
}

/// Where the app keeps the run's worker's web data store.
#[cfg(target_os = "macos")]
#[must_use]
pub fn page_store(root: &Path) -> Option<PathBuf> {
    let id = worker_id(root)?;
    let home = std::env::var_os("HOME")?;
    // An app run from its binary, as here, keeps its stores under the binary's name.
    let stores = Path::new(&home).join("Library/WebKit").join(APP).join("WebsiteDataStore");
    Some(stores.join(id.to_ascii_lowercase()))
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
            reap(&mut child, "the second app").await;
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
    use super::{
        Duration, FAILED_KEPT, KEPT_MARK, PIN_KEPT, Path, ROOTS, StackDir, drop_oldest_kept,
        loopback_address, pinned, sweep,
    };

    #[test]
    fn the_app_dials_loopback_on_the_port_the_worker_printed() {
        assert_eq!(loopback_address("[::]:53211\n").unwrap(), "127.0.0.1:53211");
        assert_eq!(loopback_address("0.0.0.0:7").unwrap(), "127.0.0.1:7");
        loopback_address("not an address").unwrap_err();
    }

    /// A run's binaries are the ones its first test pinned: a copy staged over them later, as
    /// another build's would be, reaches the next run and not this one, and the run's later
    /// tests find the same pin.
    #[test]
    fn a_run_keeps_the_binaries_its_first_test_pinned() {
        let tmp = tempfile::tempdir().unwrap();
        let (cache, runs) = (tmp.path().join("cache"), tmp.path().join("runs"));
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("slopty-app"), "e2e").unwrap();
        std::fs::write(cache.join(".slopty-app.42"), "half copied").unwrap();
        let pin = pinned(&cache, &runs, "run-1").unwrap();
        std::fs::write(cache.join(".next"), "plain").unwrap();
        std::fs::rename(cache.join(".next"), cache.join("slopty-app")).unwrap();

        assert_eq!(std::fs::read_to_string(pin.join("slopty-app")).unwrap(), "e2e");
        assert!(!pin.join(".slopty-app.42").exists(), "a copy being made is not pinned");
        assert_eq!(pinned(&cache, &runs, "run-1").unwrap(), pin, "the run's later tests");
        let next = pinned(&cache, &runs, "run-2").unwrap();
        assert_eq!(std::fs::read_to_string(next.join("slopty-app")).unwrap(), "plain");
        assert_eq!(std::fs::read_dir(&runs).unwrap().count(), 2, "no half-made pin is left");
    }

    /// A passing test's root and lock go with it; a failing one's root stays, marked.
    #[test]
    fn a_passing_test_takes_its_root_away_and_a_failing_one_keeps_it() {
        let made = || {
            std::thread::Builder::new()
                .name("harness::a_root_for_this_test_alone".to_owned())
                .spawn(|| {
                    let dir = StackDir::new("slopty-e2e-selftest-").unwrap();
                    let root = dir.path().to_owned();
                    let lock = root.with_extension("lock");
                    (dir, root, lock)
                })
                .unwrap()
                .join()
                .unwrap()
        };
        let (dir, root, lock) = made();
        assert!(root.is_dir() && lock.is_file(), "{root:?}");
        drop(dir);
        assert!(!root.exists() && !lock.exists(), "a pass leaves nothing");

        let (dir, root, _) = made();
        let failed = std::thread::spawn(move || {
            let _dir = dir;
            panic!("the test fails");
        });
        assert!(failed.join().is_err());
        assert!(root.join(KEPT_MARK).is_file(), "a failure keeps its root, marked");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Only the newest roots kept for inspection stay; and what killed runs left goes once
    /// stale, unless it was kept.
    #[test]
    fn kept_roots_are_capped_and_stale_ones_swept() {
        let tmp = tempfile::tempdir().unwrap();
        let aged = |path: &Path, secs: u64| {
            let at = std::time::SystemTime::now() - Duration::from_secs(secs);
            std::fs::File::open(path).unwrap().set_modified(at).unwrap();
        };
        for n in 0..10_u64 {
            let root = tmp.path().join(format!("{ROOTS}{n:02}"));
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join(KEPT_MARK), b"").unwrap();
            aged(&root.join(KEPT_MARK), 100 - n);
        }
        drop_oldest_kept(tmp.path());
        let mut left: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left.len(), FAILED_KEPT);
        assert_eq!(left.first().map(String::as_str), Some("slopty-e2e-02"), "the oldest went");

        let stale = tmp.path().join(format!("{ROOTS}stale"));
        let fresh = tmp.path().join(format!("{ROOTS}fresh"));
        let lock = tmp.path().join(format!("{ROOTS}stale.lock"));
        let other = tmp.path().join("someone-else");
        for dir in [&stale, &fresh, &other] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(&lock, b"").unwrap();
        let old = PIN_KEPT.as_secs() + 60;
        for path in [&stale, &lock, &other, &tmp.path().join(format!("{ROOTS}05"))] {
            aged(path, old);
        }
        sweep(tmp.path());
        assert!(!stale.exists() && !lock.exists(), "stale leftovers go");
        assert!(fresh.exists() && other.exists(), "fresh ones and others' stay");
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), FAILED_KEPT + 2, "kept stay");
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
        let mut child = scrubbed(bin("slopty-server")?, &data_dir.join("home"))
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

    /// `[::1]:<port>`: the same listener over IPv6.
    #[must_use]
    pub fn address_v6(&self) -> String {
        let port = self.address.rsplit_once(':').map_or("", |(_ip, port)| port);
        format!("[::1]:{port}")
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
        reap(&mut self.child, "slopty-server").await;
    }
}

/// ptyd + worker from this build, registered with a server as one worker when one is named.
/// Killed on drop.
///
/// Its sockets and data live under the root it was started in, and a restart binds the port the
/// first start was given, so a restarted worker keeps its worker id, finds the same ptyd and is
/// reached at the same address.
#[derive(Debug)]
pub struct Worker {
    ptyd: Child,
    daemon: Option<Child>,
    root: PathBuf,
    name: String,
    server: Option<String>,
    log: String,
    env: Vec<(String, String)>,
    address: String,
}

impl Worker {
    /// Start ptyd and the worker named `name` in `root` on a free port, registering with the
    /// server at `server` when one is given; `env` goes to both daemons, and so to the shells.
    ///
    /// # Errors
    ///
    /// When a binary is missing or a daemon does not come up.
    pub async fn start(
        root: &Path,
        name: &str,
        server: Option<&str>,
        log: &str,
        env: &[(&str, &str)],
    ) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        let ptyd = spawn_ptyd(root, log, env).await?;
        let (worker, address) = spawn_worker(root, name, log, env, server, 0).await?;
        Ok(Self {
            ptyd,
            daemon: Some(worker),
            root: root.to_path_buf(),
            name: name.to_owned(),
            server: server.map(str::to_owned),
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

    /// Where clients reach the worker directly, `127.0.0.1:<port>`; a restart keeps it.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The worker's control socket: what `slopty` and the hook relay talk to.
    #[must_use]
    pub fn ctl_socket(&self) -> PathBuf {
        self.root.join("worker.sock")
    }

    /// Kill the worker (SIGKILL: no goodbye to the server or the clients, as a crash or a pulled
    /// cable would leave them) and reap it. ptyd and its shells live on.
    pub async fn kill_worker(&mut self) {
        if let Some(mut worker) = self.daemon.take() {
            reap(&mut worker, "slopty-worker").await;
        }
    }

    /// Start the worker again on the same ptyd, data directory and port (so the same worker id,
    /// at the same address).
    ///
    /// # Errors
    ///
    /// When the worker does not come up, or its port was taken while it was down.
    pub async fn restart_worker(&mut self) -> Result<()> {
        self.kill_worker().await;
        let env: Vec<(&str, &str)> = self.env.iter().map(|(k, v)| (&**k, &**v)).collect();
        let port = self.address.parse::<std::net::SocketAddr>()?.port();
        let server = self.server.as_deref();
        let (worker, address) =
            spawn_worker(&self.root, &self.name, &self.log, &env, server, port).await?;
        self.daemon = Some(worker);
        self.address = address;
        Ok(())
    }

    /// Kill both daemons, reap them and give the worker's pasteboard back.
    pub async fn shutdown(mut self) {
        self.kill_worker().await;
        reap(&mut self.ptyd, "slopty-ptyd").await;
        #[cfg(target_os = "macos")]
        slopty_platform::pasteboard::MacPasteboard::named(&pasteboard_name(&self.root, "worker"))
            .release();
    }
}

/// The tailnet path to another Mac, as the 2026-09-25 mesh run measured it (MEASUREMENTS, "BBR3's
/// bound on the Tailscale mesh"): a 10 ms echo median over an ICMP round trip of 7 to 16 ms.
///
/// 4 ms each way plus up to 2 ms of jitter is a round trip of 8 to 12 ms. That run's ICMP pings
/// lost 13 to 21 % each way, but the worker's QUIC counted no loss in 15 of its 16 runs, so the
/// ICMP figure is about how ICMP is treated, not what the UDP path drops. 1 % independent loss
/// still puts a retransmission (and the keystroke's datagram copy) into a test step that sends a
/// few hundred packets, and stays under the 2 % loss threshold of BBR version 3, which a path
/// that loses nothing never crosses: at 3 % every bulk transfer slid as no real tailnet makes it
/// (`docs/MEASUREMENTS.md`, "a connection's second bulk stream"). A lost handshake or close
/// costs a step one timeout, not the run. A link that loses a fifth of its packets is the worker
/// e2e's own test (`typing_through_a_lossy_link_lands_once_in_order`).
pub const TAILNET: slopty_shape::Link = slopty_shape::Link {
    delay: Duration::from_millis(4),
    jitter: Duration::from_millis(2),
    loss: 0.01,
    ..slopty_shape::Link::CLEAR
};

/// The relay's loop, stopped when the handle is dropped: [`slopty_shape::relay::Relay::run`]
/// never returns on its own.
#[derive(Debug)]
struct RelayTask(tokio::task::JoinHandle<std::io::Result<()>>);

impl Drop for RelayTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The relay in front of a [`Stack::launch_shaped`] worker; dropping it stops the relay.
#[derive(Debug)]
pub struct ShapedLink {
    _relay: RelayTask,
}

/// A second worker on this Mac, reached the way a worker on another Mac would be.
///
/// ptyd and the worker run from this build under a [`StackDir`] of their own, beside a
/// [`Stack`]'s, with a private `HOME` there, so they never read the real `~/.claude` and
/// `slopty hook install` is never run. It registers with a server, whose directory lists a
/// [`slopty_shape::relay::Relay`] in this process in front of it: the app dials the relay,
/// which carries every packet over a shaped link ([`TAILNET`]), so the connection sees a mesh's
/// round trip, jitter and loss. The worker keeps its port across [`Self::restart_worker`] and
/// the relay outlives it, so the app redials the address listed.
#[derive(Debug)]
pub struct SecondWorker {
    worker: Worker,
    relay: std::sync::Arc<slopty_shape::relay::Relay>,
    _relay_task: RelayTask,
    address: String,
    home: PathBuf,
    // Last, so the root goes after the daemons in it are killed.
    dir: StackDir,
}

impl SecondWorker {
    /// Start ptyd and the worker named `name` under a root of their own, registered with
    /// `server`, and a relay in front of the worker shaped as `link`, which the server's
    /// directory lists.
    ///
    /// The server lists a worker at the IP it registered from and the port it listens on. So the
    /// worker listens on `127.0.0.1` only and registers over IPv6: the directory then says
    /// `[::1]:<its port>`, where the relay listens
    /// ([`slopty_shape::relay::Relay::bind_apart`]).
    ///
    /// # Errors
    ///
    /// When a binary is missing, a daemon does not come up or the relay cannot bind.
    pub async fn launch(
        name: &str,
        link: slopty_shape::Link,
        server: &ServerDaemon,
    ) -> Result<Self> {
        Self::launch_env(name, link, server, &[]).await
    }

    /// [`Self::launch`] with `env` on top of the daemons' own (such as the drawn screen,
    /// `SLOPTY_SYNTHETIC_SCREEN`).
    ///
    /// # Errors
    ///
    /// As [`Self::launch`].
    pub async fn launch_env(
        name: &str,
        link: slopty_shape::Link,
        server: &ServerDaemon,
        extra: &[(&str, &str)],
    ) -> Result<Self> {
        let dir = StackDir::new("slopty-e2e-second-")?;
        let root = dir.path();
        let home = root.join("home");
        std::fs::create_dir_all(&home)?;
        // `/var` is a link to `/private/var`: a shell's working directory is the resolved path,
        // and zsh shortens it to `~` only when `HOME` is spelled the same way.
        let home = std::fs::canonicalize(&home)?;
        let log = log_level();
        let home_env = home.to_string_lossy();
        let mut env = vec![("HOME", &*home_env), ("SLOPTY_BIND", "127.0.0.1")];
        env.extend_from_slice(extra);
        let worker = Worker::start(root, name, Some(&server.address_v6()), &log, &env).await?;
        let direct: std::net::SocketAddr =
            worker.address().parse().context("the worker's address")?;
        // A fixed seed, so a run's losses fall where the last run's did.
        let seed = 0x5107_7e2e;
        let at = std::net::SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, direct.port()));
        let from = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0));
        let relay = slopty_shape::relay::Relay::bind_apart(at, from, direct, link, seed)
            .await
            .context("bind the relay")?;
        let address = relay.addr()?.to_string();
        let relay = std::sync::Arc::new(relay);
        let task = tokio::spawn({
            let relay = std::sync::Arc::clone(&relay);
            async move { relay.run().await }
        });
        Ok(Self { worker, relay, _relay_task: RelayTask(task), address, home, dir })
    }

    /// Where the server's directory lists it: the relay, `[::1]:<port>`.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The named pasteboard it keeps its clipboard on ([`pasteboard_name`]).
    #[must_use]
    pub fn pasteboard(&self) -> String {
        pasteboard_name(self.dir.path(), "worker")
    }

    /// Where to put a file for this worker's run: its root, which holds its `home` and the
    /// `zsh` configuration its shells start with.
    #[must_use]
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// One request over the worker's control socket (`{"cmd": …}`), and its reply, as
    /// [`Stack::ctl`] sends it.
    ///
    /// # Errors
    ///
    /// When the worker cannot be reached or answers something that is not JSON.
    pub async fn ctl(&self, request: &Value) -> Result<Value> {
        ctl(&self.worker.ctl_socket(), request).await
    }

    /// What the relay has carried and dropped each way so far.
    pub async fn carried(&self) -> slopty_shape::relay::Carried {
        self.relay.carried().await
    }

    /// Kill the worker (SIGKILL, as a crash would); ptyd and the sessions stay.
    pub async fn kill_worker(&mut self) {
        self.worker.kill_worker().await;
    }

    /// Start the worker again on the same data directory and port (same identity, reachable
    /// through the same relay): the app reconnects on its own.
    ///
    /// # Errors
    ///
    /// When the worker does not come back up.
    pub async fn restart_worker(&mut self) -> Result<()> {
        self.worker.restart_worker().await
    }

    /// Play a Claude Code hook in `session` (the id from the dump) through the real relay:
    /// write [`TRANSCRIPT`] under the root as the session's transcript, then run `slopty hook`
    /// with the session and the worker's control socket in its environment and the payload for
    /// `event` (plus `fields`, more JSON members) on its stdin, exactly what Claude Code does.
    /// Nothing is typed into a shell and no agent is started.
    ///
    /// # Errors
    ///
    /// When the transcript cannot be written or the relay does not run to the end. The relay
    /// exits 0 even when the worker refuses (it must never block an agent), so the effect is
    /// the app's to show.
    pub async fn play_hook(&self, session: &str, event: &str, fields: &str) -> Result<()> {
        let mut hook = self.spawn_hook(session, event, fields).await?;
        let status = tokio::time::timeout(STARTUP, hook.wait())
            .await
            .context("slopty hook did not finish")??;
        ensure!(status.success(), "slopty hook failed: {status}");
        Ok(())
    }

    /// Play a hook the worker holds for the person's answer (a `PermissionRequest`), as
    /// [`Self::play_hook`] does, and hand back the relay still waiting: dropping it kills it,
    /// as Claude Code's own hook timeout would.
    ///
    /// # Errors
    ///
    /// When the transcript cannot be written or the relay does not start.
    pub async fn hold_hook(&self, session: &str, event: &str, fields: &str) -> Result<Child> {
        self.spawn_hook(session, event, fields).await
    }

    /// `slopty hook` started with the payload for `event` written to its stdin. The transcript
    /// is named for the agent's session, `<session id>.jsonl`, as Claude Code names its own:
    /// the worker takes a session's id from either, and two that differ would be two sessions.
    async fn spawn_hook(&self, session: &str, event: &str, fields: &str) -> Result<Child> {
        let transcript = self.dir.path().join(format!("{}.jsonl", agent_session(session)));
        std::fs::write(&transcript, TRANSCRIPT)?;
        let payload = format!(
            r#"{{"hook_event_name":"{event}","session_id":"{}","transcript_path":"{}"{fields}}}"#,
            agent_session(session),
            transcript.display()
        );
        let mut hook = scrubbed(bin("slopty")?, &self.home)
            .arg("hook")
            .env("SLOPTY_SESSION", session)
            .env("SLOPTY_WORKER_SOCKET", self.worker.ctl_socket())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .context("spawn slopty hook")?;
        let mut stdin = hook.stdin.take().context("the hook's stdin")?;
        stdin.write_all(payload.as_bytes()).await?;
        stdin.shutdown().await?;
        drop(stdin);
        Ok(hook)
    }

    /// Kill both daemons; the relay stops and the root goes as the rest drops, after them.
    pub async fn shutdown(self) {
        self.worker.shutdown().await;
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
    let out = slopty_out(server, data_dir, &[&["--json"], args].concat(), stdin).await?;
    serde_json::from_slice(&out)
        .with_context(|| format!("slopty {args:?} printed {:?}", String::from_utf8_lossy(&out)))
}

/// `slopty …` against the server at `server`, as [`slopty_json`] runs it but without `--json`:
/// what it printed, as bytes.
///
/// # Errors
///
/// When the CLI exits non-zero (its stderr is in the error).
pub async fn slopty_out(
    server: &str,
    data_dir: &Path,
    args: &[&str],
    stdin: &[u8],
) -> Result<Vec<u8>> {
    let mut child = scrubbed(bin("slopty")?, &data_dir.join("home"))
        .arg("--data-dir")
        .arg(data_dir)
        .args(["--server", server])
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
    Ok(out.stdout)
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
        let log = log_level();
        let server_dir = root.join("server");
        std::fs::create_dir_all(&server_dir)?;
        let server = ServerDaemon::start(&server_dir, "e2e-server", &log).await?;
        let env = [("BASH_SILENCE_DEPRECATION_WARNING", "1")];
        let worker =
            Worker::start(&root.join("worker"), worker_name, Some(server.address()), &log, &env)
                .await?;
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
        std::fs::write(&path, pinned_settings(appearance))?;
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

    /// [`Self::slopty`] with `stdin` on the CLI's standard input (`push -`).
    ///
    /// # Errors
    ///
    /// As [`slopty_json`].
    pub async fn slopty_with_stdin(&self, args: &[&str], stdin: &[u8]) -> Result<Value> {
        slopty_json(self.server.address(), &self.path("cli"), args, stdin).await
    }

    /// `slopty <args> --server <this server>` without `--json`, `stdin` on its standard input:
    /// the bytes it printed (`pull -`).
    ///
    /// # Errors
    ///
    /// As [`slopty_out`].
    pub async fn slopty_bytes(&self, args: &[&str], stdin: &[u8]) -> Result<Vec<u8>> {
        slopty_out(self.server.address(), &self.path("cli"), args, stdin).await
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

/// The app as it is normally used: a server, two workers registered with it, and the app at its
/// first run, which knows neither until it is pointed at the server. Everything is killed on drop.
///
/// Worker `near` runs under `root/near` and is dialled on loopback; worker `far` is a
/// [`SecondWorker`] whose directory entry is its relay's address, so the app reaches it over a
/// shaped link it learned from the server. The app's data directory is `root/app`.
#[derive(Debug)]
pub struct ServerFleet {
    /// Connected to the app's test socket.
    pub driver: Driver,
    /// The app process.
    pub app: Child,
    /// The worker on loopback.
    pub near: Worker,
    /// The worker behind the shaped link.
    pub far: SecondWorker,
    /// The server both register with.
    pub server: ServerDaemon,
    /// The temporary root, named for the test.
    pub dir: StackDir,
}

impl ServerFleet {
    /// Start the server, worker `near` on loopback and worker `far` behind a relay shaped as
    /// `link`, both registered with it; then the app, once the server lists both online.
    ///
    /// # Errors
    ///
    /// When a binary is missing, a daemon dies, a worker never comes online or the app does not
    /// come up.
    pub async fn launch(near: &str, far: &str, link: slopty_shape::Link) -> Result<Self> {
        let dir = StackDir::new("slopty-e2e-fleet-")?;
        let root = dir.path();
        let log = log_level();
        let server_dir = root.join("server");
        std::fs::create_dir_all(&server_dir)?;
        let server = ServerDaemon::start(&server_dir, "e2e-server", &log).await?;
        // On loopback alone, as `far` is: a worker on every interface of this machine would be
        // listed at its tailnet address when Tailscale runs here, and the run would depend on it.
        let bind = [("SLOPTY_BIND", "127.0.0.1")];
        let near =
            Worker::start(&root.join("near"), near, Some(server.address()), &log, &bind).await?;
        let far = SecondWorker::launch(far, link, &server).await?;
        let cli = root.join("cli");
        let started = tokio::time::Instant::now();
        loop {
            let workers = slopty_json(server.address(), &cli, &["workers"], b"").await?;
            let online = workers
                .as_array()
                .map_or(0, |all| all.iter().filter(|w| w["liveness"] == "online").count());
            if online == 2 {
                break;
            }
            ensure!(started.elapsed() < STARTUP, "both workers online: {workers}");
            tokio::time::sleep(POLL).await;
        }
        let (app, mut driver) = spawn_app(root, "app", &log, &[]).await?;
        driver.ok(&crate::Command::Ping).await?;
        Ok(Self { driver, app, near, far, server, dir })
    }

    /// The server's directory as `slopty workers --json` prints it.
    ///
    /// # Errors
    ///
    /// When the CLI fails.
    pub async fn directory(&self) -> Result<Value> {
        slopty_json(self.server.address(), &self.dir.path().join("cli"), &["workers"], b"").await
    }

    /// Ask the app to quit, then kill it, both workers and the server.
    pub async fn shutdown(mut self) {
        let _quit = self.driver.call(&crate::Command::Quit).await;
        reap(&mut self.app, "slopty-app").await;
        self.far.shutdown().await;
        self.near.shutdown().await;
        self.server.kill().await;
        #[cfg(target_os = "macos")]
        slopty_platform::pasteboard::MacPasteboard::named(&pasteboard_name(self.dir.path(), "app"))
            .release();
    }
}

/// The stand-in for `claude` (`slopty-stub-claude`), built beside the other binaries when it is
/// not there yet: `cargo xtask e2e` builds the daemons and the app, and this one is the test
/// kit's.
///
/// # Errors
///
/// When it is missing and cannot be built.
pub async fn stub_claude() -> Result<PathBuf> {
    const STUB: &str = "slopty-stub-claude";
    let built = built_dir()?;
    let path = built.join(STUB);
    if path.exists() {
        return Ok(path);
    }
    let target = built.parent().context("the binaries' directory has a parent")?;
    let mut build = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    build.args(["build", "-p", "slopty-testkit", "--bin", STUB]).arg("--target-dir").arg(target);
    if built.ends_with("release") {
        build.arg("--release");
    }
    let status = build.status().await.context("run cargo")?;
    ensure!(status.success() && path.exists(), "build {STUB}: {status}");
    Ok(path)
}

/// The app with a server and one registered worker behind it, as projects need.
///
/// The worker runs with `slopty-stub-claude` first on its `PATH` as `claude` and a `HOME` of its
/// own; the app starts at its first run, pointed at the server by its settings. Everything is
/// killed on drop.
///
/// `root/server` is the server's data directory, `root/worker` the worker's, `root/cli` the
/// CLI's and `root/app` the app's; `root/repo` is an empty directory for agents to work in.
#[derive(Debug)]
pub struct ProjectStack {
    /// Connected to the app's test socket.
    pub driver: Driver,
    /// The app process.
    pub app: Child,
    /// The worker.
    pub worker: Worker,
    /// The server.
    pub server: ServerDaemon,
    /// The temporary root, named for the test.
    pub dir: StackDir,
}

impl ProjectStack {
    /// Start everything, and return once the app has dialled the worker the server lists.
    ///
    /// # Errors
    ///
    /// When a binary is missing, a daemon dies, the worker never comes online or the app does
    /// not reach it.
    pub async fn launch(worker_name: &str) -> Result<Self> {
        Self::launch_with(worker_name, &[], &[]).await
    }

    /// [`Self::launch`] with `more_programs` (a name, and what it links to) beside the stand-in
    /// `claude` on the worker's `PATH`, and `more_env` added to the worker's environment.
    ///
    /// # Errors
    ///
    /// As [`Self::launch`].
    pub async fn launch_with(
        worker_name: &str,
        more_programs: &[(&str, &Path)],
        more_env: &[(&str, &str)],
    ) -> Result<Self> {
        let dir = StackDir::new("slopty-e2e-projects-")?;
        let root = dir.path();
        let log = log_level();
        let server_dir = root.join("server");
        std::fs::create_dir_all(&server_dir)?;
        let server = ServerDaemon::start(&server_dir, "e2e-server", &log).await?;
        let programs = root.join("programs");
        std::fs::create_dir_all(&programs)?;
        std::os::unix::fs::symlink(stub_claude().await?, programs.join("claude"))?;
        for (name, target) in more_programs {
            std::os::unix::fs::symlink(target, programs.join(name))?;
        }
        let home = root.join("home");
        std::fs::create_dir_all(&home)?;
        // By its real path, as `scrubbed` gives it: the shell's prompt then says `~`.
        let home = std::fs::canonicalize(&home)?;
        std::fs::create_dir_all(root.join("repo"))?;
        let path = format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", programs.display());
        let (home, path) = (home.to_string_lossy().into_owned(), path);
        let mut env = vec![
            ("HOME", home.as_str()),
            ("PATH", path.as_str()),
            ("SLOPTY_BIND", "127.0.0.1"),
            ("BASH_SILENCE_DEPRECATION_WARNING", "1"),
        ];
        env.extend_from_slice(more_env);
        let worker =
            Worker::start(&root.join("worker"), worker_name, Some(server.address()), &log, &env)
                .await?;
        let cli = root.join("cli");
        let started = tokio::time::Instant::now();
        loop {
            let workers = slopty_json(server.address(), &cli, &["workers"], b"").await?;
            let online =
                workers.as_array().is_some_and(|all| all.iter().any(|w| w["liveness"] == "online"));
            if online {
                break;
            }
            ensure!(started.elapsed() < STARTUP, "the worker online: {workers}");
            tokio::time::sleep(POLL).await;
        }
        let app_dir = root.join("app");
        std::fs::create_dir_all(&app_dir)?;
        std::fs::write(app_dir.join("settings.toml"), app_settings(APPEARANCE, Some(&server)))?;
        let (app, mut driver) = spawn_app(root, "app", &log, &[]).await?;
        driver.ok(&crate::Command::Ping).await?;
        // The first run's own shell is in before any test opens another, so the tiles stand in
        // one order on every run.
        driver
            .wait_for("the worker the server lists, and its first shell", STARTUP, |d| {
                d.workers.iter().any(|w| w.name == worker_name && w.status == "connected")
                    && d.items.iter().any(|i| i.session.is_some())
            })
            .await?;
        Ok(Self { driver, app, worker, server, dir })
    }

    /// Where to put a file for this run.
    #[must_use]
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Switch the app's theme to `appearance` (`dark`, `light`) by rewriting its
    /// `settings.toml`, the server it follows kept; the app sees it within a second.
    ///
    /// # Errors
    ///
    /// When the file cannot be written.
    pub fn set_appearance(&self, appearance: &str) -> Result<()> {
        let path = self.path("app").join("settings.toml");
        std::fs::write(&path, app_settings(appearance, Some(&self.server)))?;
        Ok(())
    }

    /// `slopty <args> --server <this server> --json`, parsed.
    ///
    /// # Errors
    ///
    /// As [`slopty_json`].
    pub async fn slopty(&self, args: &[&str]) -> Result<Value> {
        slopty_json(self.server.address(), &self.path("cli"), args, b"").await
    }

    /// Play a Claude Code hook in `session` on the worker, as [`Stack::play_hook`] does.
    ///
    /// # Errors
    ///
    /// When the transcript cannot be written or the worker refuses the hook.
    pub async fn play_hook(&self, session: &str, event: &str, fields: &str) -> Result<()> {
        let transcript = self.path(&format!("{}.jsonl", agent_session(session)));
        let hook = Hook { session, event, fields };
        play_hook(&self.worker.ctl_socket(), &transcript, hook).await
    }

    /// Ask the app to quit, then kill it, the worker and the server.
    pub async fn shutdown(mut self) {
        let _quit = self.driver.call(&crate::Command::Quit).await;
        reap(&mut self.app, "slopty-app").await;
        self.worker.shutdown().await;
        self.server.kill().await;
        #[cfg(target_os = "macos")]
        slopty_platform::pasteboard::MacPasteboard::named(&pasteboard_name(self.dir.path(), "app"))
            .release();
    }
}
