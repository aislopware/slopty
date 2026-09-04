//! `xtask run`: launch the host daemons or the app from the dev tree.
//!
//! `run host` builds and starts `slopty-ptyd` then `slopty-hostd` in the foreground (Ctrl-C
//! stops both: they share this process group). `run app` builds and starts the macOS app.
//! Both honour `--data-dir` so several isolated setups can coexist on one machine.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use clap::{Args, Subcommand};
use xshell::{Shell, cmd};

use crate::tools::step;

/// What to run.
#[derive(Subcommand, Debug)]
pub enum RunCmd {
    /// The host side: `slopty-ptyd` + `slopty-hostd` (prints a pairing ticket).
    Host(RunOpts),
    /// The macOS app.
    App(RunOpts),
}

/// Shared options.
#[derive(Args, Debug, Clone)]
pub struct RunOpts {
    /// Build with `--release`.
    #[arg(long)]
    release: bool,
    /// Data directory (`SLOPTY_DATA_DIR`); sockets go next to it under `<dir>/run/`.
    #[arg(long)]
    data_dir: Option<String>,
    /// `RUST_LOG` filter.
    #[arg(long, default_value = "info")]
    log: String,
}

impl RunOpts {
    const fn profile_dir(&self) -> &'static str {
        if self.release { "release" } else { "debug" }
    }

    const fn cargo_flags(&self) -> &'static [&'static str] {
        if self.release { &["--release"] } else { &[] }
    }

    /// Environment applied to every child.
    fn env(&self) -> Vec<(&'static str, String)> {
        let mut out = vec![("RUST_LOG", self.log.clone())];
        if let Some(dir) = &self.data_dir {
            out.push(("SLOPTY_DATA_DIR", dir.clone()));
            out.push(("SLOPTY_PTYD_SOCKET", format!("{dir}/run/ptyd.sock")));
            out.push(("SLOPTY_HOSTD_SOCKET", format!("{dir}/run/hostd.sock")));
        }
        out
    }
}

pub fn run(sh: &Shell, what: &RunCmd) -> Result<()> {
    match what {
        RunCmd::Host(opts) => host(sh, opts),
        RunCmd::App(opts) => app(sh, opts),
    }
}

fn spawn(sh: &Shell, bin: &str, args: &[&str], opts: &RunOpts) -> Result<Child> {
    let path = sh.current_dir().join("target").join(opts.profile_dir()).join(bin);
    let mut command = Command::new(&path);
    command.args(args).envs(opts.env()).stdin(Stdio::null());
    if let Some(dir) = &opts.data_dir {
        std::fs::create_dir_all(format!("{dir}/run")).with_context(|| format!("mkdir {dir}"))?;
    }
    command.spawn().with_context(|| format!("spawn {}", path.display()))
}

fn ptyd_socket(opts: &RunOpts) -> std::path::PathBuf {
    opts.data_dir.as_ref().map_or_else(
        || std::env::temp_dir().join("slopty").join("ptyd.sock"),
        |dir| std::path::PathBuf::from(format!("{dir}/run/ptyd.sock")),
    )
}

/// Block until `path` exists (ptyd binds it at startup) or the child dies.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling a socket")]
fn wait_for_socket(path: &std::path::Path, child: &mut Child) -> Result<()> {
    let started = std::time::Instant::now();
    while !path.exists() {
        if let Some(status) = child.try_wait().context("poll slopty-ptyd")? {
            bail!("slopty-ptyd exited early with {status}");
        }
        if started.elapsed() > Duration::from_secs(5) {
            bail!("slopty-ptyd did not bind {} within 5 s", path.display());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

fn host(sh: &Shell, opts: &RunOpts) -> Result<()> {
    let flags = opts.cargo_flags();
    step(
        "build host daemons",
        &cmd!(sh, "cargo build {flags...} -p slopty-ptyd -p slopty-hostd -p slopty-cli"),
    )?;
    let mut ptyd = spawn(sh, "slopty-ptyd", &[], opts)?;
    wait_for_socket(&ptyd_socket(opts), &mut ptyd)?;
    let mut hostd = spawn(sh, "slopty-hostd", &["--print-ticket"], opts)?;
    let status = hostd.wait().context("wait for slopty-hostd")?;
    let _killed = ptyd.kill();
    let _reaped = ptyd.wait();
    if !status.success() {
        bail!("slopty-hostd exited with {status}");
    }
    Ok(())
}

fn app(sh: &Shell, opts: &RunOpts) -> Result<()> {
    let flags = opts.cargo_flags();
    step("build app", &cmd!(sh, "cargo build {flags...} -p slopty --bin slopty-app"))?;
    let status = spawn(sh, "slopty-app", &[], opts)?.wait().context("wait for slopty-app")?;
    if !status.success() {
        bail!("slopty-app exited with {status}");
    }
    Ok(())
}
