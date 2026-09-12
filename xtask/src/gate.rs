//! `xtask gate`: everything that must be green before a commit lands.
//!
//! The gate checks a **snapshot** of the working tree (`target/gate/tree`, synced from
//! `git ls-files` before every run) rather than the tree itself, so the tree stays free to
//! edit while the gate runs and what passed is exactly what was synced. The cargo steps run as
//! parallel **lanes**, each on its own target dir under `target/gate/` (cargo serialises
//! concurrent builds that share one), so the wall time is the longest lane, not the sum.

use std::collections::HashSet;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use xshell::{Shell, cmd};

use crate::tools::{TRIPLES, has, host_only_present, quiet_step, repo_root};

/// Gate options.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Apply automatic fixes first.
    pub fix: bool,
    /// Fast subset only.
    pub quick: bool,
    /// Check the working tree in place instead of a snapshot (CI, or a tree nobody edits).
    pub in_place: bool,
}

/// The build lanes, each with its own target dir and a share of the cores. The host clippy
/// pass and the tests are the long ones; the rest fill the gaps.
const LANES: [(&str, u8); 5] =
    [("tools", 1), ("clippy host", 4), ("clippy ios", 4), ("tests", 6), ("rustdoc", 3)];

pub fn run(sh: &Shell, opts: Options) -> Result<()> {
    let started = Instant::now();
    crate::upstream::warn_if_stale();
    let root = repo_root()?;
    if opts.fix {
        // Fixers write to the working tree, which is what the snapshot then reads.
        fmt(sh, true)?;
        shear(sh, true)?;
        typos(sh, true)?;
    }
    // A formatting failure should not cost a build: checked first, alone, in a second.
    fmt(sh, false)?;
    let tree = if opts.in_place { root.clone() } else { snapshot(&root)? };
    let lanes = root.join("target").join("gate");
    let lane = |name: &str| -> Result<Shell> {
        let jobs = LANES.iter().find(|(n, _)| *n == name).map_or(4, |(_, j)| *j);
        let sh = Shell::new()?;
        sh.change_dir(&tree);
        sh.set_var("CARGO_TARGET_DIR", lanes.join(name.replace(' ', "-")));
        sh.set_var("CARGO_BUILD_JOBS", jobs.to_string());
        Ok(sh)
    };
    let main = || -> Result<Shell> {
        let sh = Shell::new()?;
        sh.change_dir(&root);
        Ok(sh)
    };
    let quick = opts.quick;
    let results: Vec<(&str, Result<()>)> = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        if !quick {
            handles.push((
                "tools",
                scope.spawn(|| -> Result<()> {
                    let sh = lane("tools")?;
                    deny(&sh)?;
                    shear(&sh, false)?;
                    typos(&sh, false)?;
                    // The git-backed ones read the working tree: same files, and the history.
                    let main = main()?;
                    taplo(&main, false)?;
                    commits(&main)
                }),
            ));
        }
        handles.push(("clippy host", scope.spawn(|| lint_host(&lane("clippy host")?))));
        if !quick {
            handles.push(("clippy ios", scope.spawn(|| lint_ios(&lane("clippy ios")?))));
        }
        handles.push(("tests", scope.spawn(|| test(&lane("tests")?, &[]))));
        if !quick {
            handles.push(("rustdoc", scope.spawn(|| doc(&lane("rustdoc")?, false))));
        }
        handles
            .into_iter()
            .map(|(name, handle)| {
                let result = handle
                    .join()
                    .unwrap_or_else(|panic| Err(anyhow::anyhow!("lane panicked: {panic:?}")));
                (name, result)
            })
            .collect()
    });
    let failed: Vec<&str> = results
        .into_iter()
        .filter_map(|(name, result)| match result {
            Ok(()) => None,
            Err(e) => {
                eprintln!("✘ {name}: {e:#}");
                Some(name)
            }
        })
        .collect();
    if !failed.is_empty() {
        bail!("gate failed: {} ({:.1?})", failed.join(", "), started.elapsed());
    }
    println!("✔ gate passed ({:.1?})", started.elapsed());
    Ok(())
}

/// Sync the working tree (tracked and untracked-but-not-ignored files, as `git ls-files`
/// reads it) into `target/gate/tree`: a file is copied when it is missing or differs, so
/// cargo in the snapshot rebuilds exactly what changed; files the tree no longer has are
/// removed; a submodule (`vendor/ghostty`, pinned, never edited here) is a symlink to the
/// real one.
fn snapshot(root: &Utf8Path) -> Result<Utf8PathBuf> {
    let started = Instant::now();
    let tree = root.join("target").join("gate").join("tree");
    std::fs::create_dir_all(&tree).with_context(|| format!("create {tree}"))?;
    let sh = Shell::new()?;
    sh.change_dir(root);
    let listed =
        cmd!(sh, "git ls-files -z --cached --others --exclude-standard").quiet().output()?;
    let mut wanted: HashSet<String> = HashSet::new();
    let mut copied = 0_usize;
    for rel in listed.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let rel = std::str::from_utf8(rel).context("a path in the tree is not UTF-8")?;
        let src = root.join(rel);
        let dst = tree.join(rel);
        // Deleted in the tree but still in the index: not part of the snapshot.
        let Ok(meta) = std::fs::symlink_metadata(&src) else { continue };
        wanted.insert(rel.to_owned());
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if meta.is_dir() {
            if std::fs::symlink_metadata(&dst).is_err() {
                std::os::unix::fs::symlink(&src, &dst).with_context(|| format!("link {dst}"))?;
            }
            continue;
        }
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&src)?;
            if std::fs::read_link(&dst).ok().as_deref() != Some(&target) {
                let _removed = std::fs::remove_file(&dst);
                std::os::unix::fs::symlink(&target, &dst)?;
                copied = copied.saturating_add(1);
            }
            continue;
        }
        if !same_file(&src, &dst, &meta)? {
            std::fs::copy(&src, &dst).with_context(|| format!("copy {rel}"))?;
            // `copy` clones the source's mtime on APFS. A lane may have built the old bytes
            // *after* that mtime, so cargo would call the new bytes fresh: stamp them now.
            std::fs::File::options()
                .write(true)
                .open(&dst)?
                .set_modified(std::time::SystemTime::now())
                .with_context(|| format!("touch {rel}"))?;
            copied = copied.saturating_add(1);
        }
    }
    let removed = prune(&tree, &tree, &wanted)?;
    println!(
        "  snapshot {} files: {copied} copied, {removed} removed ({:.1?})",
        wanted.len(),
        started.elapsed()
    );
    Ok(tree)
}

/// Whether the snapshot's copy already matches: same length and not older than the source
/// (a copy is always newer than what it copied), else the bytes decide.
fn same_file(src: &Utf8Path, dst: &Utf8Path, src_meta: &std::fs::Metadata) -> Result<bool> {
    let Ok(dst_meta) = std::fs::metadata(dst) else { return Ok(false) };
    if dst_meta.len() != src_meta.len() {
        return Ok(false);
    }
    if dst_meta.modified()? >= src_meta.modified()? {
        return Ok(true);
    }
    Ok(std::fs::read(src)? == std::fs::read(dst)?)
}

/// Remove every file under `dir` that the tree no longer lists, and the directories that
/// empties. Returns how many files went.
fn prune(tree: &Utf8Path, dir: &Utf8Path, wanted: &HashSet<String>) -> Result<usize> {
    let mut removed = 0_usize;
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {dir}"))? {
        let entry = entry?;
        let path = Utf8PathBuf::try_from(entry.path())
            .map_err(|e| anyhow::anyhow!("a snapshot path is not UTF-8: {e}"))?;
        let rel = path.strip_prefix(tree)?.as_str().to_owned();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            removed = removed.saturating_add(prune(tree, &path, wanted)?);
            if std::fs::read_dir(&path)?.next().is_none() {
                std::fs::remove_dir(&path)?;
            }
        } else if !wanted.contains(&rel) {
            std::fs::remove_file(&path).with_context(|| format!("remove {path}"))?;
            removed = removed.saturating_add(1);
        }
    }
    Ok(removed)
}

/// Format check (or apply). Uses nightly rustfmt when available for the unstable options.
pub fn fmt(sh: &Shell, apply: bool) -> Result<()> {
    let nightly =
        cmd!(sh, "rustup run nightly rustfmt --version").quiet().ignore_stderr().read().is_ok();
    let toolchain: &[&str] = if nightly { &["+nightly"] } else { &[] };
    let check: &[&str] = if apply { &[] } else { &["--check"] };
    quiet_step("cargo fmt", cmd!(sh, "cargo {toolchain...} fmt --all -- {check...}"))?;
    if has(sh, "taplo") {
        taplo(sh, apply)?;
    }
    Ok(())
}

/// Clippy on every triple: [`lint_host`] then [`lint_ios`].
pub fn lint(sh: &Shell) -> Result<()> {
    lint_host(sh)?;
    lint_ios(sh)
}

/// Clippy on the host with every target (tests, benches, examples) in one pass.
pub fn lint_host(sh: &Shell) -> Result<()> {
    let host = TRIPLES[0];
    quiet_step(
        &format!("clippy {host}"),
        cmd!(sh, "cargo clippy --workspace --all-targets --target {host} -- -D warnings"),
    )
}

/// Clippy on the two iOS triples together in one invocation (cargo builds them side by side
/// from one resolve), library and binary targets only — tests never run on iOS, and their code
/// is the same as the host's. Host-only crates are excluded.
pub fn lint_ios(sh: &Shell) -> Result<()> {
    let excludes: Vec<String> = host_only_present()?
        .into_iter()
        .flat_map(|c| ["--exclude".to_owned(), c.to_owned()])
        .collect();
    let ios: Vec<String> =
        TRIPLES[1..].iter().flat_map(|t| ["--target".to_owned(), (*t).to_owned()]).collect();
    quiet_step(
        "clippy ios + ios-sim",
        cmd!(sh, "cargo clippy --workspace {excludes...} {ios...} -- -D warnings"),
    )
}

pub fn test(sh: &Shell, extra: &[String]) -> Result<()> {
    quiet_step("nextest", cmd!(sh, "cargo nextest run --workspace {extra...}"))?;
    quiet_step("doctests", cmd!(sh, "cargo test --workspace --doc"))
}

pub fn doc(sh: &Shell, open: bool) -> Result<()> {
    let open: &[&str] = if open { &["--open"] } else { &[] };
    let _env = sh.push_env("RUSTDOCFLAGS", "-D warnings --cfg docsrs");
    quiet_step(
        "rustdoc",
        cmd!(sh, "cargo doc --workspace --no-deps --document-private-items {open...}"),
    )
}

fn deny(sh: &Shell) -> Result<()> {
    quiet_step("cargo deny", cmd!(sh, "cargo deny --workspace check"))
}

fn shear(sh: &Shell, fix: bool) -> Result<()> {
    let fix: &[&str] = if fix { &["--fix"] } else { &[] };
    quiet_step("cargo shear", cmd!(sh, "cargo shear {fix...}"))
}

fn typos(sh: &Shell, fix: bool) -> Result<()> {
    let write: &[&str] = if fix { &["-w"] } else { &[] };
    quiet_step("typos", cmd!(sh, "typos {write...}"))
}

/// Only the tracked TOML files: left to its own globs taplo walks `target/` and the vendored
/// trees before its `exclude` list applies, which took ~90 s.
fn taplo(sh: &Shell, apply: bool) -> Result<()> {
    let check: &[&str] = if apply { &[] } else { &["--check"] };
    let files = cmd!(sh, "git ls-files *.toml").read()?;
    let files: Vec<&str> = files.lines().collect();
    quiet_step("taplo", cmd!(sh, "taplo fmt {check...} {files...}"))
}

/// Every commit since the last tag (or the root) follows Conventional Commits.
///
/// The tag is peeled to its commit: `cargo xtask release` writes **annotated** tags, and
/// `committed` panics (`Option::unwrap()` on `None`, `committed/src/git.rs:10`) on a range whose
/// start is a tag object rather than a commit. `v0.1.0..HEAD` crashed the step for every branch
/// from the first release on; `v0.1.0^{commit}..HEAD` is the same range and does not.
fn commits(sh: &Shell) -> Result<()> {
    let last_tag = cmd!(sh, "git describe --tags --abbrev=0").quiet().ignore_stderr().read().ok();
    let range =
        last_tag.map_or_else(|| "HEAD".to_owned(), |t| format!("{}^{{commit}}..HEAD", t.trim()));
    quiet_step("committed", cmd!(sh, "committed {range} --no-merge-commit"))
}
