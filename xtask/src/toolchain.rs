//! `xtask toolchain`: move the pinned Rust toolchain everywhere it is named, in one step.
//!
//! `rust-toolchain.toml` pins the channel for every build here. CI names the same version in
//! each job's `dtolnay/rust-toolchain` step and, so rustup never installs the file's every
//! component and target into a job, in the gate workflows' `RUSTUP_TOOLCHAIN`
//! (`gate::tests::the_workflows_run_the_pinned_toolchain_by_name` holds them together). A
//! toolchain update rewrites them all at once, so the gate never goes red on one.

use anyhow::{Context as _, Result, ensure};
use xshell::{Shell, cmd};

use crate::tools::repo_root;

/// The file that pins the toolchain.
const FILE: &str = "rust-toolchain.toml";

/// The workflows that name it.
const WORKFLOWS: &str = ".github/workflows";

/// Whether `text` is a Rust release version, `1.<minor>.<patch>`.
fn is_release(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    parts.len() == 3
        && parts.first() == Some(&"1")
        && parts.iter().all(|p| p.parse::<u32>().is_ok())
}

/// The channel `rust-toolchain.toml`'s text pins.
pub fn pinned(file: &str) -> Option<&str> {
    file.lines().find_map(|line| line.strip_prefix("channel = ")).map(|v| v.trim_matches('"'))
}

/// `text` (the toolchain file or a workflow) with every pin of a release moved to `version`:
/// the file's `channel`, a step's `toolchain: 1.x.y` and `RUSTUP_TOOLCHAIN: 1.x.y`. A
/// `toolchain: nightly` or `stable` is left as it is.
fn repin(text: &str, version: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches('\n');
        let trimmed = body.trim_start();
        let indent: String = body.chars().take_while(|c| c.is_whitespace()).collect();
        let moved = [("channel = ", true), ("toolchain: ", false), ("RUSTUP_TOOLCHAIN: ", false)]
            .into_iter()
            .find_map(|(key, quoted)| {
                let value = trimmed.strip_prefix(key)?;
                is_release(value.trim_matches('"')).then(|| {
                    if quoted {
                        format!("{indent}{key}\"{version}\"")
                    } else {
                        format!("{indent}{key}{version}")
                    }
                })
            });
        match moved {
            Some(new) => {
                out.push_str(&new);
                if line.ends_with('\n') {
                    out.push('\n');
                }
            }
            None => out.push_str(line),
        }
    }
    out
}

/// The newest stable release rustup has here (`rustup update` fetches it).
pub fn stable(sh: &Shell) -> Result<String> {
    let said = cmd!(sh, "rustc +stable --version").quiet().read().context("rustc +stable")?;
    let version = said.split_whitespace().nth(1).context("rustc said no version")?;
    ensure!(is_release(version), "rustc +stable says {said}");
    Ok(version.to_owned())
}

/// `cargo xtask toolchain [version]`: pin `version`, the newest stable by default, in the
/// toolchain file and every workflow that names the pinned one.
pub fn run(sh: &Shell, version: Option<&str>) -> Result<()> {
    let version = match version {
        Some(v) => v.to_owned(),
        None => stable(sh)?,
    };
    ensure!(is_release(&version), "{version} is no Rust release (1.x.y)");
    let root = repo_root()?;
    let mut paths = vec![root.join(FILE)];
    for entry in std::fs::read_dir(root.join(WORKFLOWS))? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "yml") {
            paths.push(
                camino::Utf8PathBuf::from_path_buf(path)
                    .map_err(|p| anyhow::anyhow!("{} is not UTF-8", p.display()))?,
            );
        }
    }
    let was = pinned(&std::fs::read_to_string(root.join(FILE))?).map(str::to_owned);
    let mut changed = 0_usize;
    for path in paths {
        let text = std::fs::read_to_string(&path).with_context(|| format!("read {path}"))?;
        let new = repin(&text, &version);
        if new != text {
            std::fs::write(&path, new).with_context(|| format!("write {path}"))?;
            println!("  {}", path.strip_prefix(&root).unwrap_or(&path));
            changed = changed.saturating_add(1);
        }
    }
    match was {
        Some(was) if was == version => println!("the toolchain is {version} already"),
        Some(was) => println!("the toolchain moved from {was} to {version} in {changed} files"),
        None => println!("{version} pinned in {changed} files"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{pinned, repin};

    #[test]
    fn every_pin_of_a_release_moves_and_nothing_else() {
        let file = "[toolchain]\nchannel = \"1.99.0\"\ncomponents = [ \"rustfmt\" ]\n";
        assert_eq!(pinned(&repin(file, "1.100.1")), Some("1.100.1"));
        assert!(repin(file, "1.100.1").ends_with("components = [ \"rustfmt\" ]\n"));
        let workflow = "env:\n  RUSTUP_TOOLCHAIN: 1.99.0\njobs:\n  a:\n    steps:\n      \
                        - uses: dtolnay/rust-toolchain@stable\n        with:\n          \
                        toolchain: 1.99.0\n      - uses: dtolnay/rust-toolchain@nightly\n        \
                        with:\n          toolchain: nightly\n          components: rustfmt";
        let moved = repin(workflow, "1.100.1");
        assert!(moved.contains("\n  RUSTUP_TOOLCHAIN: 1.100.1\n"), "{moved}");
        assert!(moved.contains("\n          toolchain: 1.100.1\n"), "{moved}");
        assert!(moved.contains("\n          toolchain: nightly\n"), "nightly stays: {moved}");
        assert!(moved.ends_with("components: rustfmt"), "the last line keeps no newline");
        assert_eq!(repin(&moved, "1.100.1"), moved, "twice is once");
    }
}
