//! `xtask gate`: everything that must be green before a commit lands.
//!
//! The gate checks a **snapshot of the index** (`target/gate/tree`, synced from the staged
//! blobs before every run), not the working tree: several agents edit this one checkout at
//! once, so the tree stays free to edit while the gate runs and what passed is exactly what
//! the next `git commit` records. Stage what you mean to land, then gate it. The cargo steps run as
//! parallel **lanes**, each on its own target dir under `target/gate/` (cargo serialises
//! concurrent builds that share one), so the wall time is the longest lane, not the sum.
//!
//! The tool checks (fmt, deny, hakari, shear, typos, taplo, committed) take seconds, so they run
//! first, side by side, and a failure among them stops the gate before a compile starts.
//!
//! A lane whose inputs are exactly those of its last pass is not run again ([`pass`]): the
//! index entries it reads, the toolchain, the environment and the tools. `--since-pass` goes
//! further for the tests: when only files inside packages changed since that pass, nextest runs
//! the tests of those packages and of every package that depends on them. Either way the lanes
//! run on the snapshot of the index, and a lane that is not run passed on the same inputs.

mod pass;

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::time::Instant;

use anyhow::{Context as _, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use pass::{Inputs, Plan, Scope};
use xshell::{Shell, cmd};

use crate::tools::{LINUX_CRATES, TRIPLES, has, host_only_present, quiet_step, repo_root};

/// Gate options.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Apply automatic fixes first.
    pub fix: bool,
    /// Fast subset only.
    pub quick: bool,
    /// Check the working tree in place instead of a snapshot (CI, or a tree nobody edits).
    pub in_place: bool,
    /// Run only the tests of the packages changed since the tests lane last passed, and of
    /// their dependents.
    pub since_pass: bool,
}

/// The build lanes, each with its own target dir and a share of the cores. The host clippy
/// pass and the tests are the long ones; the rest fill the gaps.
const LANES: [(&str, u8); 5] =
    [("tools", 1), ("clippy host", 4), ("clippy ios", 4), ("tests", 6), ("rustdoc", 3)];

/// The nextest profile of the gate's tests lane (`.config/nextest.toml`).
const NEXTEST_PROFILE: &str = "gate";

/// A shell in `tree` on lane `name`'s target dir with its share of the cores.
fn lane_shell(tree: &Utf8Path, gate_dir: &Utf8Path, name: &str) -> Result<Shell> {
    let jobs = LANES.iter().find(|(n, _)| *n == name).map_or(4, |(_, j)| *j);
    let sh = Shell::new()?;
    sh.change_dir(tree);
    sh.set_var("CARGO_TARGET_DIR", gate_dir.join(name.replace(' ', "-")));
    sh.set_var("CARGO_BUILD_JOBS", jobs.to_string());
    Ok(sh)
}

pub fn run(sh: &Shell, opts: Options) -> Result<()> {
    let started = Instant::now();
    crate::upstream::warn_if_stale();
    let root = repo_root()?;
    // Two gates share `target/gate/tree`: a second one would rewrite the snapshot under the
    // first one's build and both logs would lie. The lock is released when the file closes.
    let gate_dir = root.join("target").join("gate");
    std::fs::create_dir_all(&gate_dir)?;
    let lock = std::fs::File::create(gate_dir.join(".lock"))?;
    if lock.try_lock().is_err() {
        bail!("another `cargo gate` is running on this checkout; wait for it");
    }
    if opts.in_place && opts.since_pass {
        bail!("--since-pass compares snapshots of the index; it cannot check the tree in place");
    }
    if opts.fix {
        // Fixers write to the working tree; the snapshot reads the index, so stage their edits.
        fmt(sh, true)?;
        hakari(sh, true)?;
        shear(sh, true)?;
        typos(sh, true)?;
    }
    // In place, the tree is whatever is on disk, which no record describes: every lane runs.
    let (tree, inputs) = if opts.in_place {
        (root.clone(), None)
    } else {
        let (tree, listing) = snapshot(&root)?;
        let inputs = Inputs::gather(&gate_dir, &tree, listing)?;
        (tree, Some(inputs))
    };
    let inputs = inputs.as_ref();
    let lane = |name: &str| lane_shell(&tree, &gate_dir, name);
    let checkout = || -> Result<Shell> {
        let sh = Shell::new()?;
        sh.change_dir(&root);
        Ok(sh)
    };

    // Seconds of tool checks, then minutes of compiles: a tool failure stops the gate first.
    let quick = opts.quick;
    let first: Vec<(&str, Result<()>)> = std::thread::scope(|scope| {
        let fmt_check = scope.spawn(|| -> Result<()> {
            let sh = Shell::new()?;
            sh.change_dir(&tree);
            fmt(&sh, false)
        });
        let tools = (!quick).then(|| {
            scope.spawn(|| {
                let tools = Lane {
                    name: "tools",
                    scope: Scope::Tree,
                    extra: tools_extra(&checkout()?)?,
                    since_pass: false,
                };
                cached(inputs, &tree, &tools, |_| tools_lane(|| lane("tools"), &checkout()?))
            })
        });
        let mut results = vec![("fmt", join(fmt_check))];
        if let Some(tools) = tools {
            results.push(("tools", join(tools)));
        }
        results
    });
    report(&first, started)?;

    let build = |name| Lane { name, scope: Scope::Build, extra: String::new(), since_pass: false };
    let second: Vec<(&str, Result<()>)> = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        handles.push((
            "clippy host",
            scope.spawn(|| {
                cached(inputs, &tree, &build("clippy host"), |_| lint_host(&lane("clippy host")?))
            }),
        ));
        if !quick {
            handles.push((
                "clippy ios",
                scope.spawn(|| {
                    cached(inputs, &tree, &build("clippy ios"), |_| {
                        let sh = lane("clippy ios")?;
                        lint_ios(&sh)?;
                        lint_linux(&sh)
                    })
                }),
            ));
        }
        handles.push((
            "tests",
            scope.spawn(|| {
                let tests = Lane {
                    name: "tests",
                    scope: Scope::Build,
                    extra: pass::tool_id("cargo-nextest"),
                    since_pass: opts.since_pass,
                };
                cached(inputs, &tree, &tests, |only| {
                    test_lane(&lane("tests")?, lane("tests")?, only)
                })
            }),
        ));
        if !quick {
            handles.push((
                "rustdoc",
                scope.spawn(|| {
                    cached(inputs, &tree, &build("rustdoc"), |_| doc(&lane("rustdoc")?, false))
                }),
            ));
        }
        handles.into_iter().map(|(name, handle)| (name, join(handle))).collect()
    });
    report(&second, started)?;
    println!("✔ gate passed ({:.1?})", started.elapsed());
    Ok(())
}

fn join(handle: std::thread::ScopedJoinHandle<'_, Result<()>>) -> Result<()> {
    handle.join().unwrap_or_else(|panic| Err(anyhow::anyhow!("lane panicked: {panic:?}")))
}

/// Print every failed lane and fail if there was one.
fn report(results: &[(&str, Result<()>)], started: Instant) -> Result<()> {
    let failed: Vec<&str> = results
        .iter()
        .filter_map(|(name, result)| {
            let e = result.as_ref().err()?;
            eprintln!("✘ {name}: {e:#}");
            Some(*name)
        })
        .collect();
    if !failed.is_empty() {
        bail!("gate failed: {} ({:.1?})", failed.join(", "), started.elapsed());
    }
    Ok(())
}

/// A lane as the pass records see it.
struct Lane<'a> {
    name: &'a str,
    /// The index entries it reads.
    scope: Scope,
    /// What it reads besides them and the inputs common to every lane.
    extra: String,
    /// It may run on the packages changed since its last pass only.
    since_pass: bool,
}

/// Run `lane` through `check` unless its inputs are those of its last pass, and record them
/// when it passes. `check` gets the packages to narrow to under `--since-pass`, or `None` for
/// all of them.
fn cached(
    inputs: Option<&Inputs>,
    tree: &Utf8Path,
    lane: &Lane<'_>,
    check: impl FnOnce(Option<&[String]>) -> Result<()>,
) -> Result<()> {
    let Some(inputs) = inputs else { return check(None) };
    let name = lane.name;
    let key = inputs.key(name, lane.scope, &lane.extra);
    let plan = inputs.plan(name, &key, lane.since_pass, tree)?;
    let only = match &plan {
        Plan::Skip => {
            println!("  ✓ {name}: inputs unchanged since it last passed");
            return Ok(());
        }
        Plan::Only(packages) => {
            println!("  {name}: changed since its last pass: {}", packages.join(" "));
            Some(packages.as_slice())
        }
        Plan::Run => None,
    };
    check(only)?;
    inputs.record(name, &key)
}

/// What the tools lane reads besides the tree: the tools themselves, the history `committed`
/// lints, and the day, so `cargo deny` sees new advisories at least daily.
fn tools_extra(checkout: &Shell) -> Result<String> {
    let mut extra: String = ["cargo-deny", "cargo-hakari", "cargo-shear", "typos", "committed"]
        .into_iter()
        .map(pass::tool_id)
        .collect();
    let head = cmd!(checkout, "git rev-parse HEAD").quiet().read()?;
    let tag = cmd!(checkout, "git describe --tags --abbrev=0").quiet().ignore_stderr().read();
    let day = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400);
    let _written = writeln!(extra, "head {head}\ntag {}\nday {day}", tag.unwrap_or_default());
    Ok(extra)
}

/// deny, hakari, shear and typos on the snapshot (each in a shell from `on_tree`) and
/// `committed` on the checkout's history, side by side; every failure is reported.
fn tools_lane(on_tree: impl Fn() -> Result<Shell> + Sync, checkout: &Shell) -> Result<()> {
    let on_tree = &on_tree;
    let results: Vec<Result<()>> = std::thread::scope(|scope| {
        let handles = [
            scope.spawn(move || deny(&on_tree()?)),
            scope.spawn(move || hakari(&on_tree()?, false)),
            scope.spawn(move || shear(&on_tree()?, false)),
            scope.spawn(move || typos(&on_tree()?, false)),
        ];
        // `committed` reads the history, which only the checkout has.
        let mut results = vec![commits(checkout)];
        results.extend(handles.into_iter().map(join));
        results
    });
    let errors: Vec<String> =
        results.into_iter().filter_map(Result::err).map(|e| format!("{e:#}")).collect();
    if errors.is_empty() { Ok(()) } else { bail!("{}", errors.join("; ")) }
}

/// The gate's tests: build every test binary, then run nextest (on `only`'s packages when
/// given) and the doctests side by side. Cargo holds the target dir's lock only while it
/// builds, and the build is done, so neither waits for the other.
fn test_lane(sh: &Shell, doc_sh: Shell, only: Option<&[String]>) -> Result<()> {
    quiet_step("nextest build", cmd!(sh, "cargo nextest run --workspace --no-run"))?;
    let filter: Vec<String> = only.map_or_else(Vec::new, |packages| {
        let expr = packages.iter().map(|p| format!("package(={p})")).collect::<Vec<_>>();
        vec!["--no-tests=warn".to_owned(), "-E".to_owned(), expr.join(" | ")]
    });
    std::thread::scope(|scope| {
        let doctests = scope
            .spawn(move || quiet_step("doctests", cmd!(doc_sh, "cargo test --workspace --doc")));
        let tests = quiet_step(
            "nextest",
            cmd!(sh, "cargo nextest run --workspace --profile {NEXTEST_PROFILE} {filter...}"),
        );
        let doctests = join(doctests);
        tests.and(doctests)
    })
}

/// Sync the **index** (what `git commit` would record, not the working tree) into
/// `target/gate/tree`. Several agents edit this one checkout at once, so only the staged state
/// is a unit anyone vouched for: what passes is exactly what the next commit records. Blobs are
/// read in one `git cat-file --batch` pass; a manifest of the blob each path was last written
/// from keeps unchanged files untouched, so cargo in the snapshot rebuilds exactly what changed.
/// Paths the index no longer lists are removed; a submodule (`vendor/ghostty`) is a symlink to
/// a checkout at the commit the index pins ([`submodule_at`]). Returns the tree and the index's
/// entries it holds.
fn snapshot(root: &Utf8Path) -> Result<(Utf8PathBuf, Vec<pass::Entry>)> {
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
    let mut listing = Vec::new();
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
        listing.push(pass::Entry { path: rel.to_owned(), id: format!("{mode} {sha}") });
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
    Ok((tree, listing))
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

/// Clippy on every triple: [`lint_host`], [`lint_ios`], then [`lint_linux`].
pub fn lint(sh: &Shell) -> Result<()> {
    lint_host(sh)?;
    lint_ios(sh)?;
    lint_linux(sh)
}

/// Clippy for Linux on every one of [`LINUX_CRATES`] (`tools::lint_linux`).
pub fn lint_linux(sh: &Shell) -> Result<()> {
    crate::tools::lint_linux(sh, &LINUX_CRATES)
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

/// `workspace-hack` carries the workspace's current feature union (`cargo xtask check` builds
/// against it): regenerated under `--fix`, a stale one fails the gate.
fn hakari(sh: &Shell, fix: bool) -> Result<()> {
    let diff: &[&str] = if fix { &[] } else { &["--diff"] };
    quiet_step("cargo hakari", cmd!(sh, "cargo hakari generate {diff...}"))
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
