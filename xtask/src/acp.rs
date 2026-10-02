//! `cargo xtask acp`: an ACP agent's build pinned, and what Slopty records from it.
//!
//! - `acp fixtures` drives `OpenCode` over the Agent Client Protocol as Slopty's ACP adapter does,
//!   against a canned model, and records every message each way ([`fixtures`]).
//!
//! `OpenCode` stands for every ACP agent: it is in the registry, it speaks ACP on stdio as
//! `opencode acp`, and it runs with a model of the person's choosing, so a canned one on loopback
//! serves it with no account. The build is the npm registry's `opencode-darwin-arm64` at
//! [`VERSION`], its tarball checked against the registry's SHA-512 `integrity` and unpacked under
//! `target/opencode/<version>`. `SLOPTY_OPENCODE` names another binary instead, which must say it
//! is the same version. Nothing here signs in.

mod fixtures;

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context as _, Result, bail, ensure};
use clap::Subcommand;
use data_encoding::{BASE64, HEXLOWER};
use serde_json::Value;
use xshell::{Shell, cmd};

use crate::tools::repo_root;

/// The `OpenCode` version the ACP fixtures are recorded with. It is also what the stand-in agent
/// (`slopty-stub-acp`) says it is.
pub const VERSION: &str = "1.18.34";

/// Names an `opencode` binary to use instead of the downloaded one.
const OVERRIDE: &str = "SLOPTY_OPENCODE";

const PACKAGE: &str = "opencode-darwin-arm64";
const REGISTRY: &str = "https://registry.npmjs.org";
/// Where the binary sits in the package.
const BINARY: &str = "package/bin/opencode";

#[derive(Subcommand)]
pub enum AcpCmd {
    /// Record what the pinned `OpenCode` says over ACP, driven as Slopty drives an ACP agent,
    /// against a canned model: no account.
    Fixtures,
}

pub fn run(cmd: &AcpCmd) -> Result<()> {
    match cmd {
        AcpCmd::Fixtures => fixtures::record(),
    }
}

/// The `opencode` of [`VERSION`]: `SLOPTY_OPENCODE`, else the registry's build, downloaded and
/// verified on first use.
fn official() -> Result<PathBuf> {
    let binary = match std::env::var_os(OVERRIDE) {
        Some(path) => PathBuf::from(path),
        None => download()?,
    };
    let scratch = std::env::temp_dir().join("slopty-opencode-version");
    std::fs::create_dir_all(&scratch)?;
    let out = Command::new(&binary)
        .arg("--version")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &scratch)
        .output()
        .with_context(|| format!("run {}", binary.display()))?;
    let said = String::from_utf8_lossy(&out.stdout);
    ensure!(
        said.trim() == VERSION,
        "{} is {}, not OpenCode {VERSION}",
        binary.display(),
        said.trim()
    );
    Ok(binary)
}

/// The registry's build of [`VERSION`], from `target/opencode/<version>` or fetched there.
fn download() -> Result<PathBuf> {
    let dir = repo_root()?.join("target/opencode").join(VERSION).into_std_path_buf();
    let binary = dir.join(BINARY);
    if binary.is_file() {
        return Ok(binary);
    }
    let sh = Shell::new()?;
    let url = format!("{REGISTRY}/{PACKAGE}/{VERSION}");
    let meta: Value = serde_json::from_str(&cmd!(sh, "curl -fsSL {url}").read()?)
        .context("the registry's answer")?;
    let tarball = meta.pointer("/dist/tarball").and_then(Value::as_str).context("no tarball")?;
    let integrity =
        meta.pointer("/dist/integrity").and_then(Value::as_str).context("no integrity")?;
    let Some(want) = integrity.strip_prefix("sha512-") else {
        bail!("integrity {integrity} is not SHA-512");
    };
    let want = HEXLOWER.encode(&BASE64.decode(want.as_bytes()).context("integrity base64")?);
    let partial = dir.join(".partial");
    if partial.exists() {
        std::fs::remove_dir_all(&partial)?;
    }
    std::fs::create_dir_all(&partial)?;
    let archive = partial.join("package.tgz");
    println!("fetching OpenCode {VERSION} from {tarball}");
    cmd!(sh, "curl -fsSL -o {archive} {tarball}").run()?;
    let sum = cmd!(sh, "shasum -a 512 -b {archive}").read()?;
    let got = sum.split_whitespace().next().unwrap_or_default();
    ensure!(got == want, "{tarball}: SHA-512 {got}, the registry says {want}");
    cmd!(sh, "tar -xzf {archive} -C {partial}").run()?;
    std::fs::remove_file(&archive)?;
    ensure!(partial.join(BINARY).is_file(), "the package holds no opencode binary");
    std::fs::rename(partial.join("package"), dir.join("package"))?;
    std::fs::remove_dir_all(&partial)?;
    Ok(binary)
}
