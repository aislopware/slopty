//! ptyd + hostd + the app, all from this build, in a temporary directory.
//!
//! Binaries come from `SLOPTY_E2E_BIN_DIR` (set by `cargo xtask e2e`), else `target/debug`
//! next to the workspace. Every process gets its own data directory under the temp dir, so
//! nothing installed on the machine is read or written, and everything is killed on drop.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
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

/// The lines [`TRANSCRIPT`] shows as, in the dump's words.
pub const TRANSCRIPT_LINES: [&str; 6] = [
    "user: fix the failing test",
    "thinking",
    "assistant: Looking at the test.",
    "tool Bash: cargo test -p demo",
    "result Bash: running 2 tests",
    "assistant: Fixed: `b` compared **id** with *name*.",
];

/// The running stack.
#[derive(Debug)]
pub struct Stack {
    /// The temporary directory (sockets, data dirs, artifacts).
    pub dir: tempfile::TempDir,
    /// The host's pairing ticket.
    pub ticket: String,
    /// Connected to the app's test socket.
    pub driver: Driver,
    /// ptyd, hostd, app; killed on drop.
    pub children: Vec<Child>,
    /// The simulator the app runs in, when it does; the app is terminated there on shutdown.
    pub simulator: Option<Simulator>,
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

/// One request over hostd's control socket at `path` (newline-delimited JSON, one request
/// per connection), and its reply.
async fn ctl(path: &Path, request: &Value) -> Result<Value> {
    let stream = UnixStream::connect(path)
        .await
        .with_context(|| format!("connect to hostd at {}", path.display()))?;
    let (rd, mut wr) = stream.into_split();
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    wr.write_all(&line).await?;
    wr.shutdown().await?;
    let mut reply = String::new();
    BufReader::new(rd).read_line(&mut reply).await?;
    serde_json::from_str(reply.trim()).context("hostd's reply is not JSON")
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

/// The daemons of a stack: ptyd and hostd (named `host_name`) under `root`, and the ticket
/// hostd printed.
async fn daemons(root: &Path, host_name: &str, log: &str) -> Result<(Vec<Child>, String)> {
    let ptyd_sock = root.join("ptyd.sock");
    let mut ptyd = Command::new(bin("slopty-ptyd")?)
        .arg("--socket")
        .arg(&ptyd_sock)
        .env("RUST_LOG", log)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .context("spawn slopty-ptyd")?;
    wait_for_path(&ptyd_sock, &mut ptyd, "slopty-ptyd").await?;

    let ctl_sock = root.join("hostd.sock");
    let mut hostd = Command::new(bin("slopty-hostd")?)
        .arg("--ptyd-socket")
        .arg(&ptyd_sock)
        .arg("--ctl-socket")
        .arg(&ctl_sock)
        .arg("--data-dir")
        .arg(root.join("host"))
        .arg("--print-ticket")
        .arg("--port")
        .arg("0")
        .env("RUST_LOG", log)
        .env("SLOPTY_HOST_NAME", host_name)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .context("spawn slopty-hostd")?;
    let stdout = hostd.stdout.take().context("hostd stdout")?;
    let mut ticket = String::new();
    tokio::time::timeout(STARTUP, BufReader::new(stdout).read_line(&mut ticket))
        .await
        .context("hostd did not print a ticket in time")??;
    let ticket = ticket.trim().to_owned();
    anyhow::ensure!(!ticket.is_empty(), "hostd printed an empty ticket");
    Ok((vec![ptyd, hostd], ticket))
}

impl Stack {
    /// Start ptyd, hostd (named `host_name`) and the app; pair the app with the host and wait
    /// until its canvas is up.
    ///
    /// # Errors
    ///
    /// When a binary is missing, a process dies, or the app does not come up in time.
    pub async fn launch(host_name: &str) -> Result<Self> {
        let dir = tempfile::Builder::new().prefix("slopty-e2e-").tempdir()?;
        let root = dir.path();
        let log = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_owned());
        let (mut children, ticket) = daemons(root, host_name, &log).await?;

        let app_dir = root.join("app");
        std::fs::create_dir_all(&app_dir)?;
        let app_sock = root.join("app.sock");
        let mut app = Command::new(bin("slopty-app")?)
            .env("RUST_LOG", &log)
            .env("SLOPTY_DATA_DIR", &app_dir)
            .env(crate::SOCKET_ENV, &app_sock)
            // Local echo would put predicted text in the rows before the host confirms it.
            .env("SLOPTY_PREDICT", "never")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .context("spawn slopty-app")?;
        wait_for_path(&app_sock, &mut app, "slopty-app").await?;
        children.push(app);
        let driver = Driver::connect(&app_sock).await?;
        Self::pair(Self { dir, ticket, driver, children, simulator: None }).await
    }

    /// Start ptyd and hostd here and the app in a booted simulator (`simctl launch` with the
    /// socket and data dir in its environment; the simulator shares this file system), then
    /// pair and wait as [`Self::launch`] does.
    ///
    /// # Errors
    ///
    /// When a daemon is missing, `simctl` fails, or the app does not bind its socket in time.
    pub async fn launch_on_simulator(host_name: &str, simulator: Simulator) -> Result<Self> {
        let dir = tempfile::Builder::new().prefix("slopty-e2e-ios-").tempdir()?;
        let root = dir.path();
        let log = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_owned());
        let (children, ticket) = daemons(root, host_name, &log).await?;

        let app_dir = root.join("app");
        std::fs::create_dir_all(&app_dir)?;
        let app_sock = root.join("app.sock");
        let launch = Command::new("xcrun")
            .args(["simctl", "launch", "--terminate-running-process"])
            .arg(&simulator.udid)
            .arg(&simulator.bundle_id)
            .env("SIMCTL_CHILD_RUST_LOG", &log)
            .env("SIMCTL_CHILD_SLOPTY_DATA_DIR", &app_dir)
            .env(format!("SIMCTL_CHILD_{}", crate::SOCKET_ENV), &app_sock)
            .env("SIMCTL_CHILD_SLOPTY_PREDICT", "never")
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
        let driver = Driver::connect(&app_sock).await?;
        Self::pair(Self { dir, ticket, driver, children, simulator: Some(simulator) }).await
    }

    /// Ping, pair with the host and wait for the connection.
    async fn pair(mut stack: Self) -> Result<Self> {
        stack.driver.ok(&crate::Command::Ping).await?;
        stack.driver.ok(&crate::Command::Pair { ticket: stack.ticket.clone() }).await?;
        stack
            .driver
            .wait_for("the host to connect", STARTUP, |d| {
                d.hosts.iter().any(|h| h.active && h.status == "connected")
            })
            .await?;
        Ok(stack)
    }

    /// Where to put a file for this run.
    #[must_use]
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// The transcript a played agent writes ([`Self::play_hook`]).
    #[must_use]
    pub fn transcript_path(&self) -> PathBuf {
        self.path("agent.jsonl")
    }

    /// Play a Claude Code hook in `session` (the id from the dump): write [`TRANSCRIPT`] under
    /// the run's directory and hand hostd the payload for `event` (plus `fields`, more JSON
    /// members) naming it over its control socket, exactly what `slopty hook` relays from the
    /// agent's shell. Nothing is typed into the shell: the agent is simulated from the test.
    ///
    /// # Errors
    ///
    /// When the transcript cannot be written or hostd refuses the hook.
    pub async fn play_hook(&self, session: &str, event: &str, fields: &str) -> Result<()> {
        let transcript = self.transcript_path();
        std::fs::write(&transcript, TRANSCRIPT)?;
        let payload = format!(
            r#"{{"hook_event_name":"{event}","session_id":"e2e","transcript_path":"{}"{fields}}}"#,
            transcript.display()
        );
        let request = json!({ "cmd": "hook", "session": session, "payload": payload });
        let reply = ctl(&self.path("hostd.sock"), &request).await?;
        anyhow::ensure!(
            reply.get("reply").and_then(Value::as_str) == Some("ok"),
            "hostd refused the hook: {reply}"
        );
        Ok(())
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
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _killed = child.start_kill();
        }
    }
}
