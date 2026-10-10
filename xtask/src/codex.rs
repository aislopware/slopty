//! `cargo xtask codex`: the pinned Codex build, and what Slopty takes from it.
//!
//! - `codex schema` runs the build's `codex app-server generate-json-schema --experimental` and
//!   writes the Rust types of the methods Slopty speaks into
//!   `crates/slopty-agent/src/codex/protocol.rs` ([`generate`]). The file is checked in, so a
//!   change to it is a change to Codex's wire.
//! - `codex fixtures` records the app-server's frames to two clients of one thread ([`fixtures`]).
//!
//! The build is fetched from the npm registry (`@openai/codex@<version>-darwin-arm64`), its
//! tarball checked against the registry's SHA-512 `integrity`, and unpacked under
//! `target/codex/<version>`. `SLOPTY_CODEX` names another binary instead, which must say it is
//! the same version. Nothing here signs in or talks to a model.

mod fixtures;
mod generate;

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail, ensure};
use clap::Subcommand;
use data_encoding::{BASE64, HEXLOWER};
use serde_json::Value;
use xshell::{Shell, cmd};

use crate::tools::repo_root;

/// The Codex version Slopty speaks: its types are generated from it, and its fixtures recorded
/// with it.
pub const VERSION: &str = "0.162.1";

/// Names a `codex` binary to use instead of the downloaded one.
const OVERRIDE: &str = "SLOPTY_CODEX";

const PACKAGE: &str = "@openai/codex";
const PLATFORM: &str = "darwin-arm64";
const REGISTRY: &str = "https://registry.npmjs.org";
/// Where the binary sits in the platform package.
const BINARY: &str = "package/vendor/aarch64-apple-darwin/bin/codex";
/// Where the generated types go.
const PROTOCOL: &str = "crates/slopty-agent/src/codex/protocol.rs";

#[derive(Subcommand)]
pub enum CodexCmd {
    /// Generate the app-server protocol's Rust types from the pinned build's schema.
    Schema {
        /// Only say whether the checked-in types are what the build generates.
        #[arg(long)]
        check: bool,
    },
    /// Record what the pinned app-server says to two clients of one thread, against a canned
    /// model: no account, no network.
    Fixtures,
}

pub fn run(cmd: &CodexCmd) -> Result<()> {
    match cmd {
        CodexCmd::Schema { check } => schema(*check),
        CodexCmd::Fixtures => fixtures::record(),
    }
}

/// The `codex` of [`VERSION`]: `SLOPTY_CODEX`, else the official build, downloaded and verified
/// on first use.
pub fn official() -> Result<PathBuf> {
    let binary = match std::env::var_os(OVERRIDE) {
        Some(path) => PathBuf::from(path),
        None => download()?,
    };
    let out = Command::new(&binary)
        .arg("--version")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .with_context(|| format!("run {}", binary.display()))?;
    let said = String::from_utf8_lossy(&out.stdout);
    ensure!(
        said.split_whitespace().nth(1) == Some(VERSION),
        "{} is {}, not codex-cli {VERSION}",
        binary.display(),
        said.trim()
    );
    Ok(binary)
}

/// The official build of [`VERSION`], from `target/codex/<version>` or fetched there.
fn download() -> Result<PathBuf> {
    let dir = repo_root()?.join("target/codex").join(VERSION).into_std_path_buf();
    let binary = dir.join(BINARY);
    if binary.is_file() {
        return Ok(binary);
    }
    let sh = Shell::new()?;
    let url = format!("{REGISTRY}/{PACKAGE}/{VERSION}-{PLATFORM}");
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
    println!("fetching Codex {VERSION} from {tarball}");
    cmd!(sh, "curl -fsSL -o {archive} {tarball}").run()?;
    let sum = cmd!(sh, "shasum -a 512 -b {archive}").read()?;
    let got = sum.split_whitespace().next().unwrap_or_default();
    ensure!(got == want, "{tarball}: SHA-512 {got}, the registry says {want}");
    cmd!(sh, "tar -xzf {archive} -C {partial}").run()?;
    std::fs::remove_file(&archive)?;
    ensure!(partial.join(BINARY).is_file(), "the package holds no codex binary");
    std::fs::rename(partial.join("package"), dir.join("package"))?;
    std::fs::remove_dir_all(&partial)?;
    Ok(binary)
}

/// The schema bundle the pinned build generates, experimental methods and fields included.
fn bundle(codex: &Path) -> Result<Value> {
    let scratch = repo_root()?.join("target/codex").join(VERSION).join("schema");
    let scratch = scratch.into_std_path_buf();
    if scratch.exists() {
        std::fs::remove_dir_all(&scratch)?;
    }
    let home = scratch.join("home");
    std::fs::create_dir_all(home.join("codex"))?;
    let out = scratch.join("out");
    let status = Command::new(codex)
        .args(["app-server", "generate-json-schema", "--experimental", "--out"])
        .arg(&out)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home)
        .env("CODEX_HOME", home.join("codex"))
        .status()
        .with_context(|| format!("run {}", codex.display()))?;
    ensure!(status.success(), "generate-json-schema failed: {status}");
    let path = out.join("codex_app_server_protocol.schemas.json");
    let text = std::fs::read_to_string(&path).with_context(|| path.display().to_string())?;
    serde_json::from_str(&text).with_context(|| path.display().to_string())
}

fn schema(check: bool) -> Result<()> {
    let codex = official()?;
    let bundle = bundle(&codex)?;
    let code = generate::generate(&bundle, VERSION)?;
    let formatted = rustfmt(&code)?;
    let path = repo_root()?.join(PROTOCOL).into_std_path_buf();
    let kept = std::fs::read_to_string(&path).unwrap_or_default();
    if check {
        ensure!(kept == formatted, "{PROTOCOL} is not what Codex {VERSION} generates");
        println!("{PROTOCOL} is Codex {VERSION}'s");
        return Ok(());
    }
    if kept == formatted {
        println!("{PROTOCOL} already is Codex {VERSION}'s");
    } else {
        std::fs::write(&path, formatted).with_context(|| path.display().to_string())?;
        println!("wrote {PROTOCOL} from Codex {VERSION}");
    }
    Ok(())
}

/// `code` as nightly rustfmt lays it out, the way the gate checks it.
fn rustfmt(code: &str) -> Result<String> {
    use std::io::Write as _;
    let mut child = Command::new("rustup")
        .args(["run", "nightly", "rustfmt", "--edition", "2024", "--emit", "stdout"])
        .current_dir(repo_root()?)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .context("run rustfmt")?;
    child.stdin.take().context("rustfmt's stdin")?.write_all(code.as_bytes())?;
    let out = child.wait_with_output()?;
    ensure!(out.status.success(), "rustfmt failed on the generated types");
    String::from_utf8(out.stdout).context("rustfmt's output")
}
