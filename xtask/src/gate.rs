//! `xtask gate`: everything that must be green before a commit lands.
//!
//! The gate checks a **snapshot of the index** (`target/gate/tree`, synced from the staged
//! blobs before every run), not the working tree: several agents edit this one checkout at
//! once, so the tree stays free to edit while the gate runs and what passed is exactly what
//! the next `git commit` records. Stage what you mean to land, then gate it. The cargo steps run as
//! parallel **lanes**, each on its own target dir under `target/gate/` (cargo serialises
//! concurrent builds that share one), so the wall time is the longest lane, not the sum.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use anyhow::{Context as _, Result, bail};
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
        // Fixers write to the working tree; the snapshot reads the index, so stage their edits.
        fmt(sh, true)?;
        shear(sh, true)?;
        typos(sh, true)?;
    }
    let tree = if opts.in_place { root.clone() } else { snapshot(&root)? };
    // A formatting failure should not cost a build: checked first, alone, in a second.
    let tree_sh = Shell::new()?;
    tree_sh.change_dir(&tree);
    fmt(&tree_sh, false)?;
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
                    // `committed` reads the history, which only the checkout has.
                    commits(&main()?)
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

/// Sync the **index** (what `git commit` would record, not the working tree) into
/// `target/gate/tree`. Several agents edit this one checkout at once, so only the staged state
/// is a unit anyone vouched for: what passes is exactly what the next commit records. Blobs are
/// read in one `git cat-file --batch` pass; a manifest of the blob each path was last written
/// from keeps unchanged files untouched, so cargo in the snapshot rebuilds exactly what changed.
/// Paths the index no longer lists are removed; a submodule (`vendor/ghostty`) is a symlink to
/// a checkout at the commit the index pins ([`submodule_at`]).
fn snapshot(root: &Utf8Path) -> Result<Utf8PathBuf> {
    let started = Instant::now();
    let gate = root.join("target").join("gate");
    let tree = gate.join("tree");
    let manifest_path = gate.join("tree.index");
    std::fs::create_dir_all(&tree).with_context(|| format!("create {tree}"))?;
    let sh = Shell::new()?;
    sh.change_dir(root);
    let listed = cmd!(sh, "git ls-files --stage -z").quiet().output()?;
    let before: HashMap<String, String> = std::fs::read_to_string(&manifest_path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once(' ').map(|(sha, rel)| (rel.to_owned(), sha.to_owned())))
        .collect();
    let mut after: HashMap<String, String> = HashMap::new();
    let mut wanted: HashSet<String> = HashSet::new();
    let mut fetch: Vec<(String, String, bool)> = Vec::new();
    for entry in listed.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let entry = std::str::from_utf8(entry).context("an index path is not UTF-8")?;
        let Some((meta, rel)) = entry.split_once('\t') else { continue };
        let mut fields = meta.split(' ');
        let (Some(mode), Some(sha), Some(stage)) = (fields.next(), fields.next(), fields.next())
        else {
            bail!("unexpected `git ls-files --stage` line: {entry}");
        };
        if stage != "0" {
            bail!("{rel} is unmerged in the index");
        }
        wanted.insert(rel.to_owned());
        let dst = tree.join(rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if mode == "160000" {
            let module = submodule_at(root, &gate, rel, sha)?;
            if std::fs::read_link(&dst).ok().as_deref() != Some(module.as_std_path()) {
                let _removed = std::fs::remove_file(&dst);
                std::os::unix::fs::symlink(&module, &dst).with_context(|| format!("link {dst}"))?;
            }
            continue;
        }
        let key = format!("{mode}:{sha}");
        let present = std::fs::symlink_metadata(&dst).is_ok();
        if !present || before.get(rel) != Some(&key) {
            fetch.push((rel.to_owned(), sha.to_owned(), mode == "120000"));
        }
        after.insert(rel.to_owned(), key);
    }
    let written = write_blobs(root, &tree, &fetch, &after)?;
    let removed = prune(&tree, &tree, &wanted)?;
    let manifest = after.iter().fold(String::new(), |mut s, (rel, key)| {
        s.push_str(key);
        s.push(' ');
        s.push_str(rel);
        s.push('\n');
        s
    });
    std::fs::write(&manifest_path, manifest).with_context(|| format!("write {manifest_path}"))?;
    println!(
        "  snapshot of the index, {} files: {written} written, {removed} removed ({:.1?})",
        wanted.len(),
        started.elapsed()
    );
    Ok(tree)
}

/// A checkout of submodule `rel` at the commit the index pins, under `target/gate/modules/`:
/// the working submodule may be mid-bump, and the snapshot must build what the index records.
/// It is a worktree of the submodule's own repository, so it shares its objects and moving it
/// to another commit rewrites only the files that differ.
fn submodule_at(root: &Utf8Path, gate: &Utf8Path, rel: &str, sha: &str) -> Result<Utf8PathBuf> {
    let module = gate.join("modules").join(rel.replace('/', "__"));
    let sh = Shell::new()?;
    if module.join(".git").exists() {
        sh.change_dir(&module);
        let head = cmd!(sh, "git rev-parse HEAD").quiet().read()?;
        if head.trim() != sha {
            cmd!(sh, "git checkout --quiet --detach {sha}").quiet().run()?;
        }
    } else {
        std::fs::create_dir_all(gate.join("modules"))?;
        sh.change_dir(root.join(rel));
        cmd!(sh, "git worktree add --quiet --detach --force {module} {sha}").quiet().run()?;
    }
    Ok(module)
}

/// Write the blobs `fetch` names into `tree`, reading them all through one
/// `git cat-file --batch`. A file whose bytes already match keeps its mtime; a rewritten one is
/// stamped now, because a lane may have built the old bytes after the blob's own time.
fn write_blobs(
    root: &Utf8Path,
    tree: &Utf8Path,
    fetch: &[(String, String, bool)],
    modes: &HashMap<String, String>,
) -> Result<usize> {
    use std::io::{BufRead as _, Read as _, Write as _};
    if fetch.is_empty() {
        return Ok(0);
    }
    let mut child = std::process::Command::new("git")
        .args(["cat-file", "--batch"])
        .current_dir(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .context("spawn git cat-file")?;
    let mut stdin = child.stdin.take().context("git cat-file stdin")?;
    let shas = fetch.iter().fold(String::new(), |mut s, (_, sha, _)| {
        s.push_str(sha);
        s.push('\n');
        s
    });
    let feeder = std::thread::spawn(move || stdin.write_all(shas.as_bytes()));
    let mut out = std::io::BufReader::new(child.stdout.take().context("git cat-file stdout")?);
    let mut written = 0_usize;
    for (rel, sha, link) in fetch {
        let mut header = String::new();
        out.read_line(&mut header)?;
        let size: usize = header
            .split(' ')
            .nth(2)
            .and_then(|s| s.trim().parse().ok())
            .with_context(|| format!("git cat-file header for {sha}: {header:?}"))?;
        let mut blob = vec![0_u8; size];
        out.read_exact(&mut blob)?;
        let mut newline = [0_u8; 1];
        out.read_exact(&mut newline)?;
        let dst = tree.join(rel);
        if *link {
            let target = std::str::from_utf8(&blob).context("a symlink target is not UTF-8")?;
            if std::fs::read_link(&dst).ok().as_deref() != Some(Utf8Path::new(target).as_std_path())
            {
                let _removed = std::fs::remove_file(&dst);
                std::os::unix::fs::symlink(target, &dst)?;
                written = written.saturating_add(1);
            }
            continue;
        }
        if std::fs::symlink_metadata(&dst).is_ok_and(|m| m.file_type().is_symlink() || m.is_dir()) {
            let _removed = std::fs::remove_file(&dst);
        }
        if std::fs::read(&dst).is_ok_and(|old| old == blob) {
            continue;
        }
        std::fs::write(&dst, &blob).with_context(|| format!("write {rel}"))?;
        let executable = modes.get(rel).is_some_and(|k| k.starts_with("100755"));
        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(&dst, std::os::unix::fs::PermissionsExt::from_mode(mode))?;
        written = written.saturating_add(1);
    }
    feeder.join().map_err(|panic| anyhow::anyhow!("git cat-file feeder panicked: {panic:?}"))??;
    let status = child.wait()?;
    if !status.success() {
        bail!("git cat-file failed: {status}");
    }
    Ok(written)
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
    let root = repo_root()?;
    let files = cmd!(sh, "git -C {root} ls-files *.toml").read()?;
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
