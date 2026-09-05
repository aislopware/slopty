//! ptyd + hostd + the app, all from this build, in a temporary directory.
//!
//! Binaries come from `SLOPTY_E2E_BIN_DIR` (set by `cargo xtask e2e`), else `target/debug`
//! next to the workspace. Every process gets its own data directory under the temp dir, so
//! nothing installed on the machine is read or written, and everything is killed on drop.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::process::{Child, Command};

use crate::driver::Driver;

/// How long a daemon or the app may take to come up.
const STARTUP: Duration = Duration::from_secs(30);
/// Socket poll interval.
const POLL: Duration = Duration::from_millis(50);

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

        let ptyd_sock = root.join("ptyd.sock");
        let mut ptyd = Command::new(bin("slopty-ptyd")?)
            .arg("--socket")
            .arg(&ptyd_sock)
            .env("RUST_LOG", &log)
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
            .env("RUST_LOG", &log)
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
        let mut driver = Driver::connect(&app_sock).await?;
        driver.ok(&crate::Command::Ping).await?;

        let children = vec![ptyd, hostd, app];
        let mut stack = Self { dir, ticket, driver, children };
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

    /// Ask the app to quit, then kill whatever is left.
    pub async fn shutdown(mut self) {
        let _quit = self.driver.call(&crate::Command::Quit).await;
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
