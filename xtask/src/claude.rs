//! The official Claude Code build the fixtures are recorded with.
//!
//! The `claude` on a developer's `PATH` may be anything: a managed launcher, a patched build, a
//! wrapper. Fixtures pin what Anthropic ships, so the build is fetched from the npm registry
//! (`@anthropic-ai/claude-code-darwin-arm64@<version>`), its tarball checked against the
//! registry's SHA-512 `integrity`, and unpacked under `target/claude/<version>`.
//! `SLOPTY_CLAUDE` names another binary instead. [`latest`] asks the registry which release is
//! newest, for `cargo xtask upstream check`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail, ensure};
use data_encoding::{BASE64, HEXLOWER};
use serde_json::Value;
use xshell::{Shell, cmd};

use crate::tools::repo_root;

/// The Claude Code release the decoder's recordings are made with unless another is named.
/// `cargo xtask upstream check` compares it with npm's latest.
pub const VERSION: &str = "2.1.314";

/// Names a `claude` binary to use instead of the downloaded one.
const OVERRIDE: &str = "SLOPTY_CLAUDE";

/// Claude Code's package for this host.
const PACKAGE: &str = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    "@anthropic-ai/claude-code-linux-x64"
} else if cfg!(target_os = "linux") {
    "@anthropic-ai/claude-code-linux-arm64"
} else if cfg!(target_arch = "x86_64") {
    "@anthropic-ai/claude-code-darwin-x64"
} else {
    "@anthropic-ai/claude-code-darwin-arm64"
};
const REGISTRY: &str = "https://registry.npmjs.org";

/// The `claude` to record with: `SLOPTY_CLAUDE`, else the official build of `version`,
/// downloaded and verified on first use. Either way it must say it is `version`.
///
/// The downloaded build is asked in an empty environment. A `claude` the person names is asked
/// in theirs, as a capture runs it: a managed launcher signs the machine in from it and, with
/// none, answers that the machine is not signed in instead of its version.
pub fn official(version: &str) -> Result<PathBuf> {
    ensure!(
        !version.is_empty() && version.split('.').all(|p| p.parse::<u32>().is_ok()),
        "{version} is no Claude Code version"
    );
    let named = std::env::var_os(OVERRIDE).map(PathBuf::from);
    let binary = match &named {
        Some(path) => path.clone(),
        None => download(version)?,
    };
    let mut asked = Command::new(&binary);
    if named.is_none() {
        asked.env_clear().env("PATH", "/usr/bin:/bin");
    }
    let out =
        asked.arg("--version").output().with_context(|| format!("run {}", binary.display()))?;
    let said = String::from_utf8_lossy(&out.stdout);
    ensure!(
        said.split_whitespace().next() == Some(version),
        "{} is Claude Code {}, not {version}",
        binary.display(),
        said.trim()
    );
    Ok(binary)
}

/// The newest Claude Code release on the npm registry (its `latest` tag).
pub fn latest() -> Result<String> {
    let sh = Shell::new()?;
    let url = format!("{REGISTRY}/{PACKAGE}/latest");
    let meta: Value = serde_json::from_str(&cmd!(sh, "curl -fsSL {url}").read()?)
        .context("the registry's answer")?;
    meta.get("version").and_then(Value::as_str).map(str::to_owned).context("no version")
}

/// The official build of `version`, from `target/claude/<version>` or fetched there.
fn download(version: &str) -> Result<PathBuf> {
    let dir = repo_root()?.join("target/claude").join(version);
    let binary = dir.join("package/claude").into_std_path_buf();
    if binary.is_file() {
        return Ok(binary);
    }
    let package = fetch(PACKAGE, version, dir.as_std_path())?;
    ensure!(package.join("claude").is_file(), "the package holds no claude binary");
    Ok(binary)
}

/// npm's `package` at `version`, unpacked as `<dir>/package` and returned: fetched once, its
/// tarball checked against the registry's SHA-512 `integrity`, and kept.
pub fn fetch(package: &str, version: &str, dir: &Path) -> Result<PathBuf> {
    let unpacked = dir.join("package");
    if unpacked.join("package.json").is_file() {
        return Ok(unpacked);
    }
    let sh = Shell::new()?;
    let url = format!("{REGISTRY}/{package}/{version}");
    let meta: Value = serde_json::from_str(&cmd!(sh, "curl -fsSL {url}").read()?)
        .context("the registry's answer")?;
    let tarball = meta.pointer("/dist/tarball").and_then(Value::as_str).context("no tarball")?;
    let integrity =
        meta.pointer("/dist/integrity").and_then(Value::as_str).context("no integrity")?;
    let Some(want) = integrity.strip_prefix("sha512-") else {
        bail!("integrity {integrity} is not SHA-512");
    };
    let want = HEXLOWER.encode(&BASE64.decode(want.as_bytes()).context("integrity base64")?);
    // Each fetch unpacks in a place of its own: two checks may fetch at once.
    let partial = dir.join(format!(".partial-{}", std::process::id()));
    if partial.exists() {
        std::fs::remove_dir_all(&partial)?;
    }
    std::fs::create_dir_all(&partial)?;
    let archive = partial.join("package.tgz");
    println!("fetching {package} {version} from {tarball}");
    cmd!(sh, "curl -fsSL -o {archive} {tarball}").run()?;
    let sum = cmd!(sh, "shasum -a 512 -b {archive}").read()?;
    let got = sum.split_whitespace().next().unwrap_or_default();
    ensure!(got == want, "{tarball}: SHA-512 {got}, the registry says {want}");
    cmd!(sh, "tar -xzf {archive} -C {partial}").run()?;
    std::fs::remove_file(&archive)?;
    let fetched = partial.join("package");
    ensure!(fetched.join("package.json").is_file(), "{package} {version} holds no package.json");
    if std::fs::rename(&fetched, &unpacked).is_err() && !unpacked.join("package.json").is_file() {
        bail!("{package} {version} could not be kept at {}", unpacked.display());
    }
    std::fs::remove_dir_all(&partial)?;
    Ok(unpacked)
}
