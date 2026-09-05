//! ptyd + hostd + the app, all from this build, in a temporary directory.
//!
//! Binaries come from `SLOPTY_E2E_BIN_DIR` (set by `cargo xtask e2e`), else `target/debug`
//! next to the workspace. Every process gets its own data directory under the temp dir, so
//! nothing installed on the machine is read or written, and everything is killed on drop.

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

    /// Take the window off screen. It stays in the host's window list (the host enumerates with
    /// `onScreenWindowsOnly: false`), so a stream opened on it captures nothing at all.
    ///
    /// # Errors
    ///
    /// When the marker cannot be written.
    pub fn hide(&self) -> Result<()> {
        std::fs::write(self.markers.join("hide"), b"")?;
        Ok(())
    }

    /// Put it back and keep repainting it, which is what makes the host call the source live.
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
/// hostd printed. `env` goes to both, on top of this process's own.
async fn daemons(
    root: &Path,
    host_name: &str,
    log: &str,
    env: &[(&str, &str)],
) -> Result<(Vec<Child>, String)> {
    let ptyd_sock = root.join("ptyd.sock");
    // ptyd compiles ghostty's terminfo on start-up; keep it out of the developer's own
    // `~/.terminfo` and let the shells it spawns read it back from here. The trailing
    // separator is ncurses' way of saying "then the system database".
    let terminfo = root.join("terminfo");
    let terminfo_dirs = format!("{}:", terminfo.display());
    let mut ptyd = Command::new(bin("slopty-ptyd")?)
        .arg("--socket")
        .arg(&ptyd_sock)
        .envs(env.iter().copied())
        .env(slopty_pty::terminfo::DIR_ENV, &terminfo)
        .env("TERMINFO_DIRS", &terminfo_dirs)
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
        .envs(env.iter().copied())
        .env(slopty_pty::terminfo::DIR_ENV, &terminfo)
        .env("TERMINFO_DIRS", &terminfo_dirs)
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

impl Stack {
    /// Start ptyd, hostd (named `host_name`) and the app; pair the app with the host and wait
    /// until its canvas is up.
    ///
    /// # Errors
    ///
    /// When a binary is missing, a process dies, or the app does not come up in time.
    pub async fn launch(host_name: &str) -> Result<Self> {
        Self::launch_with(host_name, &[]).await
    }

    /// [`Self::launch`] with a fake `claude` ([`FAKE_CLAUDE`]) first on the daemons' `PATH`
    /// and a `HOME` of their own, so "+ agent" opens a session the host must attribute
    /// without any hook ever firing. Drive it with [`Self::fake_claude_stage`].
    ///
    /// # Errors
    ///
    /// As [`Self::launch`], plus when the fake cannot be written.
    pub async fn launch_with_fake_claude(host_name: &str) -> Result<Self> {
        let dir = tempfile::Builder::new().prefix("slopty-e2e-agent-").tempdir()?;
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
        Self::launch_in(dir, host_name, &env).await
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
    pub async fn launch_with(host_name: &str, env: &[(&str, &str)]) -> Result<Self> {
        let dir = tempfile::Builder::new().prefix("slopty-e2e-").tempdir()?;
        Self::launch_in(dir, host_name, env).await
    }

    /// `env` goes to the daemons and the app alike, on top of the defaults.
    async fn launch_in(
        dir: tempfile::TempDir,
        host_name: &str,
        env: &[(&str, &str)],
    ) -> Result<Self> {
        let root = dir.path();
        let log = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_owned());
        let (mut children, ticket) = daemons(root, host_name, &log, env).await?;

        let app_dir = root.join("app");
        std::fs::create_dir_all(&app_dir)?;
        let app_sock = root.join("app.sock");
        let mut app = Command::new(bin("slopty-app")?)
            .env("RUST_LOG", &log)
            .env("SLOPTY_DATA_DIR", &app_dir)
            .env(crate::SOCKET_ENV, &app_sock)
            // Local echo would put predicted text in the rows before the host confirms it.
            .env("SLOPTY_PREDICT", "never")
            .envs(env.iter().copied())
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
        Self::launch_on_simulator_with(host_name, simulator, &[]).await
    }

    /// [`Self::launch_on_simulator`] with extra environment for the app, as
    /// [`Self::launch_with`] (each variable is passed as `SIMCTL_CHILD_<name>`).
    ///
    /// # Errors
    ///
    /// As [`Self::launch_on_simulator`].
    pub async fn launch_on_simulator_with(
        host_name: &str,
        simulator: Simulator,
        env: &[(&str, &str)],
    ) -> Result<Self> {
        let dir = tempfile::Builder::new().prefix("slopty-e2e-ios-").tempdir()?;
        let root = dir.path();
        let log = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_owned());
        let (children, ticket) = daemons(root, host_name, &log, &[]).await?;

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
            // Glass only: the key bar stays in the frame whatever the simulator has attached.
            .env("SIMCTL_CHILD_SLOPTY_HARDWARE_KEYBOARD", "0")
            .envs(env.iter().map(|(k, v)| (format!("SIMCTL_CHILD_{k}"), *v)))
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

    /// The host's live screen streams as `slopty host screens` reads them, straight off hostd's
    /// control socket.
    ///
    /// # Errors
    ///
    /// When the socket is not there or hostd answers something else.
    pub async fn host_screens(&self) -> Result<Vec<Value>> {
        let reply = ctl(&self.path("hostd.sock"), &json!({ "cmd": "screens" })).await?;
        let live = reply.get("live").and_then(Value::as_array).cloned();
        live.ok_or_else(|| anyhow::anyhow!("hostd did not list its screens: {reply}"))
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
