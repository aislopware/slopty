//! `cargo xtask pi`: the pinned pi build, and what Slopty records from it.
//!
//! - `pi fixtures` drives the build over its RPC mode with Slopty's gate loaded, against a canned
//!   model, and records every record each way ([`fixtures`]).
//!
//! The build is `@earendil-works/pi-coding-agent` at [`VERSION`] from the npm registry, installed
//! with `bun` under `target/pi/<version>`, and the installed package's tarball checked against
//! the registry's SHA-512 `integrity`. It runs under `node`, as its `bin` does. `SLOPTY_PI` names
//! another `pi` instead, which must say it is the same version. Nothing here signs in or talks to
//! a model.

mod fixtures;

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context as _, Result, ensure};
use clap::Subcommand;
use serde_json::{Value, json};
use xshell::{Shell, cmd};

use crate::tools::repo_root;

/// The pi version Slopty speaks: its fixtures are recorded with it. It is also
/// `slopty_agent::pi::VERSION`, which the fixtures are held to.
pub const VERSION: &str = "1.0.0";

/// Names a `pi` to use instead of the installed one.
const OVERRIDE: &str = "SLOPTY_PI";

const PACKAGE: &str = "@earendil-works/pi-coding-agent";
const REGISTRY: &str = "https://registry.npmjs.org";
/// The package's command, as its `bin` names it.
const BIN: &str = "dist/bundle/cli.js";

#[derive(Subcommand)]
pub enum PiCmd {
    /// Record what the pinned pi says over RPC with Slopty's gate loaded, against a canned
    /// model: no account, no network.
    Fixtures,
}

pub fn run(cmd: &PiCmd) -> Result<()> {
    match cmd {
        PiCmd::Fixtures => fixtures::record(),
    }
}

/// How to run a pi: a program and the words before pi's own.
pub struct Pi {
    pub program: PathBuf,
    pub prefix: Vec<String>,
}

impl Pi {
    /// A command running this pi, with nothing of this environment.
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.prefix).env_clear();
        command
    }
}

/// The pi of [`VERSION`]: `SLOPTY_PI`, else the registry's build, installed and verified on
/// first use.
pub fn official() -> Result<Pi> {
    let pi = match std::env::var_os(OVERRIDE) {
        Some(path) => Pi { program: PathBuf::from(path), prefix: Vec::new() },
        None => Pi { program: node()?, prefix: vec![install()?.display().to_string()] },
    };
    let out = pi
        .command()
        .arg("--version")
        .env("PATH", "/usr/bin:/bin")
        .output()
        .with_context(|| format!("run {}", pi.program.display()))?;
    let said = String::from_utf8_lossy(&out.stdout);
    ensure!(
        said.trim() == VERSION,
        "{} is {}, not pi {VERSION}",
        pi.program.display(),
        said.trim()
    );
    Ok(pi)
}

/// The `node` on this `PATH`, as itself rather than a version manager's shim.
fn node() -> Result<PathBuf> {
    let out = Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .context("run node, which pi needs")?;
    ensure!(out.status.success(), "node -p process.execPath failed");
    Ok(PathBuf::from(String::from_utf8(out.stdout)?.trim()))
}

/// The registry's build of [`VERSION`], installed under `target/pi/<version>`: its command.
fn install() -> Result<PathBuf> {
    let dir = repo_root()?.join("target/pi").join(VERSION).into_std_path_buf();
    let package = dir.join("node_modules").join(PACKAGE);
    let bin = package.join(BIN);
    let installed = std::fs::read_to_string(package.join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .is_some_and(|meta| meta.get("version").is_some_and(|v| v == VERSION));
    if installed && bin.is_file() {
        return Ok(bin);
    }
    std::fs::create_dir_all(&dir)?;
    let manifest = json!({ "private": true, "dependencies": { PACKAGE: VERSION } });
    std::fs::write(dir.join("package.json"), serde_json::to_string_pretty(&manifest)?)?;
    let sh = Shell::new()?;
    sh.change_dir(&dir);
    println!("installing pi {VERSION} into {}", dir.display());
    cmd!(sh, "bun install --ignore-scripts").run()?;
    let url = format!("{REGISTRY}/{PACKAGE}/{VERSION}");
    let meta: Value = serde_json::from_str(&cmd!(sh, "curl -fsSL {url}").read()?)
        .context("the registry's answer")?;
    let integrity =
        meta.pointer("/dist/integrity").and_then(Value::as_str).context("no integrity")?;
    let lock = std::fs::read_to_string(dir.join("bun.lock")).context("bun.lock")?;
    let entry = format!("\"{PACKAGE}@{VERSION}\"");
    let line = lock.lines().find(|l| l.contains(&entry)).context("pi is not in bun.lock")?;
    ensure!(
        line.contains(integrity),
        "bun installed {PACKAGE}@{VERSION} with another integrity than the registry's {integrity}"
    );
    ensure!(bin.is_file(), "the package holds no {BIN}");
    Ok(bin)
}
