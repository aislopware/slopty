//! Slopty's Claude Code mod (`crates/slopty-agent/assets/claude-mod`), checked as Claude Code
//! itself loads it (`docs/decisions/claude-code.md`, "The inspector mirrors the TUI's prompt box
//! through a mod of Slopty's").
//!
//! The mod is TypeScript that the engine transpiles and runs; it has no dependency, and its
//! development tools come from the npm registry at the exact versions its `bun.lock` pins, under
//! a Bun of the version [`BUN`] names, fetched as the pinned Claude Code is (`crate::claude`).
//! Everything runs in a copy of the mod under `target/claude-mod/<job>`, so the checkout holds
//! only its sources. `check`, the gate's, runs:
//!
//! - `oxfmt --check`;
//! - `tsc` 7 at its strictest against the pinned build's own declarations, which [`types`] lays by
//!   starting that build's TUI once in a pseudo-terminal (no account, no model) and keeps under
//!   `target/claude/<version>/types`;
//! - oxlint with its type-aware rules;
//! - `claude plugin validate --strict`;
//! - `claude plugin test`, the engine's own kit on a mocked clock (`tests/`);
//! - and Stryker's mutation score, which must kill every mutant not disabled inline with its
//!   reason, with no more timeouts than the known endless relay loops. Stryker's command runner
//!   names the mutant in an environment a hooks module has none of, so each of its runs is `xtask
//!   claude-mod mutant`: a copy of the mod of its own with the mutant's id written in.
//!
//! `--quick` leaves Stryker out; `--deep` adds the seeded model-based property
//! (`deep/model.ts`), the weekly job's. A check whose inputs (the mod's files, this file, the
//! pinned versions) are the same as at its last pass is skipped; on CI, where no pass is kept,
//! it runs only when the push changes them. [`Tui`] drives a TUI in a pseudo-terminal, for
//! [`types`] and the recorder (`cargo xtask fixtures claude-mod`).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::io::Write as _;
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use clap::Subcommand;
use serde_json::{Value, json};

use crate::tools::repo_root;

/// The mod, from the repository root.
pub const MOD: &str = "crates/slopty-agent/assets/claude-mod";

/// The Bun the tools are installed and Stryker is run with.
const BUN: &str = "1.4.4";

/// Bun's package for this host.
const BUN_PACKAGE: &str = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    "@oven/bun-linux-x64"
} else if cfg!(target_os = "linux") {
    "@oven/bun-linux-aarch64"
} else if cfg!(target_arch = "x86_64") {
    "@oven/bun-darwin-x64"
} else {
    "@oven/bun-darwin-aarch64"
};

/// The platform directory of the native packages the tools install.
const NATIVE: &str = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    "linux-x64"
} else if cfg!(target_os = "linux") {
    "linux-arm64"
} else if cfg!(target_arch = "x86_64") {
    "darwin-x64"
} else {
    "darwin-arm64"
};

/// Mutants the kit can only time out on, each a relay loop that never ends (`busy` never set,
/// a flush that calls itself whatever the text): Stryker counts a timeout as detected, so more
/// than these means a loaded machine passed off timeouts as kills.
const KNOWN_TIMEOUTS: u64 = 3;

/// Names in the mod's folder that are no source of it: what its own `.gitignore` names.
const NOT_SOURCES: [&str; 4] = ["node_modules", "types", ".stryker-tmp", "reports"];

/// What a check's inputs include besides the mod: its own rules.
const THIS_FILE: &str = include_str!("claude_mod.rs");

/// The paths whose change makes CI run the check.
pub const WATCHED: [&str; 3] = [MOD, "xtask/src/claude_mod.rs", "xtask/src/claude.rs"];

/// The environment a mutant's run reads the pinned `claude` and its home from.
const MUTANT_CLAUDE: &str = "SLOPTY_CLAUDE_MOD_CLAUDE";
const MUTANT_HOME: &str = "SLOPTY_CLAUDE_MOD_HOME";

/// The API key a scratch home approves: never a real one, and every request it makes goes to a
/// port nobody listens on, or to the recorder's canned API.
pub const PLACEHOLDER_KEY: &str = "sk-ant-placeholder-0000000000000000000000";

#[derive(Subcommand)]
pub enum ClaudeModCmd {
    /// fmt, tsc, oxlint, `claude plugin validate --strict`, the kit's tests and Stryker's
    /// mutation score on the mod, as the gate runs them, when it, this check or the pinned
    /// versions changed since they last passed.
    Check {
        /// Also the seeded model-based property (the weekly deep job).
        #[arg(long)]
        deep: bool,
        /// Without Stryker: every check but the mutation score.
        #[arg(long)]
        quick: bool,
        /// Run even when nothing changed since the last pass.
        #[arg(long)]
        force: bool,
    },
    /// Lay the pinned Claude Code's TypeScript declarations for mods, and print where.
    Types,
    /// Stryker's command for one mutant (its id in `__STRYKER_ACTIVE_MUTANT__`): the kit on a
    /// copy of the mod with the id written in.
    #[command(hide = true)]
    Mutant,
    /// The recorder's relay: append the payload on stdin to `to`, as `slopty hook` would hand
    /// it to the worker.
    #[command(hide = true)]
    Sink {
        /// The JSON-lines file.
        to: PathBuf,
        /// The relay's own arguments (`hook`).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
}

/// `cargo xtask claude-mod`.
pub fn run(cmd: &ClaudeModCmd) -> Result<()> {
    match cmd {
        &ClaudeModCmd::Check { deep: false, quick: false, force: false } => {
            check_tree(&repo_root()?)
        }
        &ClaudeModCmd::Check { deep, quick, force } => {
            check(&repo_root()?.into_std_path_buf(), Job { deep, mutants: !quick, force })
        }
        ClaudeModCmd::Types => {
            let (claude, _) = claude()?;
            println!("{}", types(&claude)?.display());
            Ok(())
        }
        ClaudeModCmd::Mutant => mutant(),
        ClaudeModCmd::Sink { to, .. } => sink(to),
    }
}

/// The gate's: the checks and the mutation score on the tree at `root` (the gate's snapshot of
/// the index), when the mod or its checks changed since they last passed. About 8 s on this
/// Mac then, nothing otherwise.
pub fn check_tree(root: &camino::Utf8Path) -> Result<()> {
    check(root.as_std_path(), Job { mutants: true, ..Job::default() })
}

/// What a check runs.
#[derive(Clone, Copy, Default)]
struct Job {
    deep: bool,
    mutants: bool,
    force: bool,
}

impl Job {
    /// The work directory and pass record's name.
    const fn name(self) -> &'static str {
        match (self.deep, self.mutants) {
            (false, false) => "check",
            (true, false) => "deep",
            (false, true) => "mutants",
            (true, true) => "all",
        }
    }
}

/// The pinned Claude Code (`SLOPTY_CLAUDE` or the official build) and its version.
fn claude() -> Result<(PathBuf, &'static str)> {
    let version = crate::claude::VERSION;
    Ok((crate::claude::official(version)?, version))
}

fn check(root: &Path, job: Job) -> Result<()> {
    let started = Instant::now();
    let source = root.join(MOD);
    let files = sources(&source)?;
    let record = inputs(&source, &files)?;
    let state = repo_root()?.into_std_path_buf().join("target/claude-mod");
    let pass = state.join("pass").join(job.name());
    if !job.force {
        if std::fs::read_to_string(&pass).is_ok_and(|last| last == record) {
            println!("  ✓ claude-mod: unchanged since it last passed");
            return Ok(());
        }
        if std::env::var_os("GITHUB_ACTIONS").is_some() && unchanged_on_ci(root) {
            println!("  ✓ claude-mod: this push leaves the mod and its checks alone");
            return Ok(());
        }
    }
    let work = state.join(job.name());
    std::fs::create_dir_all(&work)?;
    // One check at a time in a work directory: the gate's and a lane's may meet.
    let lock = std::fs::File::create(state.join(format!("{}.lock", job.name())))?;
    lock.lock()?;
    let (claude, version) = claude()?;
    let laid = types(&claude)?;
    sync(&source, &files, &work)?;
    copy_dir(&laid, &work.join(".claude-plugin/types"))?;
    if job.deep {
        std::fs::copy(work.join("deep/model.ts"), work.join("tests/model.test.ts"))?;
    }
    let bun = bun()?;
    let home = state.join("home");
    std::fs::create_dir_all(&home)?;
    run_in(&work, Command::new(&bun).args(["install", "--frozen-lockfile"]), "bun install")?;
    let ts = ts_files(&work)?;
    let tsgolint = work.join(format!("node_modules/@oxlint-tsgolint/{NATIVE}/tsgolint"));
    let claude_env = |args: &[&str]| {
        let mut command = Command::new(&claude);
        command.env_clear().env("PATH", "/usr/bin:/bin").env("HOME", &home).args(args);
        command
    };
    let steps: Vec<(&str, Command)> = vec![
        ("oxfmt", {
            let mut c = Command::new(&bun);
            c.args(["node_modules/oxfmt/bin/oxfmt", "--check"]).args(&ts);
            c
        }),
        ("tsc", {
            let mut c = Command::new(&bun);
            c.args(["node_modules/typescript/bin/tsc", "-p", "tsconfig.json"]);
            c
        }),
        ("oxlint", {
            let mut c = Command::new(&bun);
            c.env("OXLINT_TSGOLINT_PATH", &tsgolint)
                .args(["node_modules/oxlint/bin/oxlint", "--type-aware", "--deny-warnings"])
                .args(["-c", ".oxlintrc.json"])
                .args(&ts);
            c
        }),
        ("validate", claude_env(&["plugin", "validate", "--strict", "."])),
        ("kit", claude_env(&["plugin", "test", "."])),
    ];
    let results: Vec<(&str, Result<Duration>)> = std::thread::scope(|scope| {
        // Every step starts before the first is joined.
        let mut handles = Vec::new();
        for (name, mut command) in steps {
            let work = &work;
            handles.push((name, scope.spawn(move || timed(work, &mut command, name))));
        }
        handles
            .into_iter()
            .map(|(name, h)| {
                (name, h.join().unwrap_or_else(|_| Err(anyhow::anyhow!("{name} panicked"))))
            })
            .collect()
    });
    let mut failed = Vec::new();
    let mut line = String::new();
    for (name, result) in results {
        match result {
            Ok(took) => {
                let _written = write!(line, " {name} {:.2}s", took.as_secs_f64());
            }
            Err(e) => failed.push(format!("{name}: {e:#}")),
        }
    }
    ensure!(failed.is_empty(), "claude-mod (Claude Code {version}):\n{}", failed.join("\n"));
    if job.mutants {
        let took = mutants(&work, &bun, &claude, &home)?;
        let _written = write!(line, " stryker {:.1}s", took.as_secs_f64());
    }
    std::fs::create_dir_all(state.join("pass"))?;
    std::fs::write(&pass, record)?;
    println!(
        "  ✓ claude-mod ({}, Claude Code {version}, {:.1}s):{line}",
        job.name(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Whether, on CI, the commits this push brings (the checkout's `HEAD` against `origin/main`)
/// leave every [`WATCHED`] path alone; `false` when that cannot be told.
fn unchanged_on_ci(root: &Path) -> bool {
    Command::new("git")
        .current_dir(root)
        .args(["diff", "--quiet", "origin/main", "HEAD", "--"])
        .args(WATCHED)
        .status()
        .is_ok_and(|s| s.success())
}

/// The mod's source files, relative to `dir`, sorted.
fn sources(dir: &Path) -> Result<Vec<PathBuf>> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
            let entry = entry?;
            let name = entry.file_name();
            if NOT_SOURCES.iter().any(|n| name == *n) {
                continue;
            }
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                walk(&path, base, out)?;
            } else {
                out.push(path.strip_prefix(base)?.to_path_buf());
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out)?;
    out.sort();
    ensure!(!out.is_empty(), "{} holds no mod", dir.display());
    Ok(out)
}

/// A check's inputs as text: every source's path, length and hash, this file's, and the pinned
/// versions. Compared whole, as the gate's pass records are.
fn inputs(dir: &Path, files: &[PathBuf]) -> Result<String> {
    let hash = |bytes: &[u8]| {
        let mut h = DefaultHasher::new();
        bytes.hash(&mut h);
        h.finish()
    };
    let mut record = format!(
        "claude {}\nbun {BUN}\nself {:016x}\n",
        crate::claude::VERSION,
        hash(THIS_FILE.as_bytes())
    );
    for file in files {
        let bytes = std::fs::read(dir.join(file))?;
        let _written = writeln!(record, "{} {} {:016x}", file.display(), bytes.len(), hash(&bytes));
    }
    Ok(record)
}

/// `work` holding exactly `files` of `source`, its installed tools kept.
fn sync(source: &Path, files: &[PathBuf], work: &Path) -> Result<()> {
    for entry in std::fs::read_dir(work)? {
        let entry = entry?;
        if entry.file_name() == "node_modules" {
            continue;
        }
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(entry.path())?;
        } else {
            std::fs::remove_file(entry.path())?;
        }
    }
    for file in files {
        let to = work.join(file);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(source.join(file), &to)?;
    }
    Ok(())
}

/// Every TypeScript source of the mod in `work`, relative to it (oxfmt and oxlint skip the
/// folders a `.gitignore` above names, and `target/` is one).
fn ts_files(work: &Path) -> Result<Vec<PathBuf>> {
    let files = sources(work)?;
    Ok(files.into_iter().filter(|f| f.extension().is_some_and(|e| e == "ts")).collect())
}

/// `src`'s files copied into `dst`, which is made.
fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// `command` in `dir`, its output shown only when it fails.
fn run_in(dir: &Path, command: &mut Command, name: &str) -> Result<()> {
    timed(dir, command, name).map(|_took| ())
}

fn timed(dir: &Path, command: &mut Command, name: &str) -> Result<Duration> {
    let started = Instant::now();
    let out = command
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("start {name}"))?;
    if !out.status.success() {
        bail!(
            "{name} failed ({}):\n{}{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(started.elapsed())
}

/// The pinned Bun, fetched once.
fn bun() -> Result<PathBuf> {
    let dir = repo_root()?.into_std_path_buf().join("target/bun").join(BUN);
    let package = crate::claude::fetch(BUN_PACKAGE, BUN, &dir)?;
    let bun = package.join("bin/bun");
    ensure!(bun.is_file(), "{BUN_PACKAGE} {BUN} holds no bin/bun");
    // npm's tarballs keep no mode for a binary that a script was to link.
    let mut mode = std::fs::metadata(&bun)?.permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    std::fs::set_permissions(&bun, mode)?;
    Ok(bun)
}

/// Stryker over the kit in `work`; every mutant must be detected, no more than the known
/// endless loops by a timeout.
fn mutants(work: &Path, bun: &Path, claude: &Path, home: &Path) -> Result<Duration> {
    let started = Instant::now();
    let exe = std::env::current_exe()?;
    let exe = exe.to_str().context("xtask's path is not UTF-8")?;
    let config = std::fs::read_to_string(work.join("stryker.config.json"))?;
    ensure!(config.contains("XTASK claude-mod mutant"), "stryker.config.json runs no mutant");
    let quoted = format!("'{}'", exe.replace('\'', r"'\''"));
    std::fs::write(work.join("stryker.config.json"), config.replace("XTASK", &quoted))?;
    // At most half the cores: Stryker counts a timeout as a kill, and a loaded machine times
    // every mutant out.
    let cores = std::thread::available_parallelism().map_or(2, std::num::NonZero::get);
    let concurrency = (cores / 2).clamp(1, 6).to_string();
    let out = Command::new(bun)
        .current_dir(work)
        .env(MUTANT_CLAUDE, claude)
        .env(MUTANT_HOME, home)
        .args(["--bun", "node_modules/@stryker-mutator/core/bin/stryker.js", "run"])
        .args(["--concurrency", &concurrency])
        .stdin(Stdio::null())
        .output()
        .context("start Stryker")?;
    let report = std::fs::read_to_string(work.join("reports/mutation.json"));
    let shown = || {
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    };
    let report: Value = match report {
        Ok(text) => serde_json::from_str(&text)?,
        Err(_) => bail!("Stryker wrote no report ({}):\n{}", out.status, shown()),
    };
    let mut statuses: BTreeMap<String, u64> = BTreeMap::new();
    for (_, file) in report.get("files").and_then(Value::as_object).into_iter().flatten() {
        for mutant in file.get("mutants").and_then(Value::as_array).into_iter().flatten() {
            let status = mutant.get("status").and_then(Value::as_str).unwrap_or("?").to_owned();
            let count = statuses.entry(status).or_insert(0);
            *count = count.saturating_add(1);
        }
    }
    let count = |s: &str| statuses.get(s).copied().unwrap_or(0);
    let undetected: u64 = statuses
        .iter()
        .filter(|(s, _)| !matches!(s.as_str(), "Killed" | "Timeout" | "Ignored"))
        .map(|(_, n)| n)
        .sum();
    ensure!(
        out.status.success() && undetected == 0 && count("Timeout") <= KNOWN_TIMEOUTS,
        "Stryker: {statuses:?} (every mutant killed, at most {KNOWN_TIMEOUTS} timeouts):\n{}",
        shown()
    );
    println!(
        "  stryker: {} killed, {} timed out, {} disabled inline",
        count("Killed"),
        count("Timeout"),
        count("Ignored")
    );
    Ok(started.elapsed())
}

/// One mutant's run, in Stryker's sandbox (the current directory): the mod copied into a
/// folder of its own (two runs rewriting one module raced and passed off kills as survivors),
/// the active mutant's id written into the instrumented module where it reads the environment,
/// and the kit run there. Should the shim itself fail, it says so and ends as a survivor, so
/// the score fails rather than counting a broken run as a kill.
fn mutant() -> Result<()> {
    let id = std::env::var("__STRYKER_ACTIVE_MUTANT__").ok().filter(|id| !id.is_empty());
    let prepared = (|| -> Result<(PathBuf, Command)> {
        let claude = std::env::var_os(MUTANT_CLAUDE).context("no claude named for the mutant")?;
        let home = std::env::var_os(MUTANT_HOME).context("no home named for the mutant")?;
        let sandbox = std::env::current_dir()?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir =
            std::env::temp_dir().join(format!("slopty-mutant-{}-{nanos}", std::process::id()));
        for part in [".claude-plugin/plugin.json", "hooks/hooks.json", "tests"] {
            let from = sandbox.join(part);
            let to = dir.join(part);
            if from.is_dir() {
                copy_dir(&from, &to)?;
            } else {
                std::fs::create_dir_all(to.parent().context("a file in a folder")?)?;
                std::fs::copy(&from, &to)?;
            }
        }
        let module = std::fs::read_to_string(sandbox.join("hooks/register.ts"))?;
        std::fs::write(dir.join("hooks/register.ts"), with_mutant(&module, id.as_deref())?)?;
        let mut kit = Command::new(claude);
        kit.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .args(["plugin", "test"])
            .arg(&dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Ok((dir, kit))
    })();
    let (dir, mut kit) = match prepared {
        Ok(ready) => ready,
        Err(e) => {
            eprintln!("claude-mod mutant {id:?}: the shim failed, counted as a survivor: {e:#}");
            return Ok(());
        }
    };
    let status = kit.status();
    let _removed = std::fs::remove_dir_all(&dir);
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => bail!("the kit failed under mutant {id:?} ({s})"),
        Err(e) => {
            eprintln!(
                "claude-mod mutant {id:?}: the kit did not start, counted as a survivor: {e}"
            );
            Ok(())
        }
    }
}

/// Stryker's instrumented `module` with the active mutant `id` (or none) written where it
/// reads `process.env`, which a hooks module has none of.
fn with_mutant(module: &str, id: Option<&str>) -> Result<String> {
    const GUARDED: &str = "g.process && g.process.env && g.process.env.__STRYKER_ACTIVE_MUTANT__";
    const BARE: &str = "g.process.env.__STRYKER_ACTIVE_MUTANT__";
    let literal = id.map_or_else(|| "undefined".to_owned(), |id| Value::from(id).to_string());
    let written = module.replace(GUARDED, &literal).replace(BARE, &literal);
    ensure!(
        id.is_none() || written != module,
        "the instrumented module reads no active mutant where Stryker 10 writes it"
    );
    ensure!(!written.contains("__STRYKER_ACTIVE_MUTANT__"), "an active-mutant read is left");
    Ok(written)
}

/// The recorder's relay: one line per payload, `{"at_ms": …, "payload": …}`.
fn sink(to: &Path) -> Result<()> {
    let mut payload = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut payload)?;
    let at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let payload: Value = serde_json::from_str(&payload).unwrap_or(Value::String(payload));
    let line = format!("{}\n", json!({ "at_ms": at_ms, "payload": payload }));
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(to)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

/// The pinned build's declarations for mods, laid once per version under
/// `target/claude/<version>/types` (the engine writes them into a mod's
/// `.claude-plugin/types` at its first load in the TUI, never in `-p`).
pub fn types(claude: &Path) -> Result<PathBuf> {
    let cache = repo_root()?.into_std_path_buf().join("target/claude").join(crate::claude::VERSION);
    let laid = cache.join("types");
    if laid.join("tsconfig.json").is_file() && laid.join("claude-code/index.d.ts").is_file() {
        return Ok(laid);
    }
    let scratch = Scratch::new("types")?;
    let module = scratch.work.join("mod");
    for part in [".claude-plugin/plugin.json", "hooks/hooks.json", "hooks/register.ts"] {
        let to = module.join(part);
        std::fs::create_dir_all(to.parent().context("a file in a folder")?)?;
        std::fs::copy(repo_root()?.join(MOD).join(part), &to)?;
    }
    let debug = scratch.root.join("debug.log");
    let mut command = scratch.claude(claude);
    command.arg("--plugin-dir").arg(&module).arg("--debug-file").arg(&debug);
    let tui = Tui::start(command)?;
    let wrote = until(Duration::from_secs(90), || {
        std::fs::read_to_string(&debug).is_ok_and(|log| log.contains("type root of slopty"))
    });
    tui.stop();
    let log = std::fs::read_to_string(&debug).unwrap_or_default();
    ensure!(
        wrote && module.join(".claude-plugin/types/claude-code/index.d.ts").is_file(),
        "Claude Code laid no declarations into {} in 90 s; its debug log:\n{}",
        module.display(),
        log.lines()
            .filter(|l| l.contains("type root") || l.contains("ERROR"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let partial = cache.join(format!(".types-{}", std::process::id()));
    copy_dir(&module.join(".claude-plugin/types"), &partial)?;
    if std::fs::rename(&partial, &laid).is_err() {
        std::fs::remove_dir_all(&partial)?;
    }
    println!("laid Claude Code {}'s declarations in {}", crate::claude::VERSION, laid.display());
    Ok(laid)
}

/// A scratch home and project folder a Claude Code TUI starts in with no account: onboarding
/// done, the folder trusted, a placeholder API key approved, requests to a dead port.
pub struct Scratch {
    /// The scratch's own folder.
    pub root: PathBuf,
    /// `HOME`.
    pub home: PathBuf,
    /// The project folder, real path.
    pub work: PathBuf,
}

impl Scratch {
    /// `/tmp/slopty-claude-mod-<name>`, emptied first: outside the checkout, whose own
    /// `.claude` settings a session started below it would read.
    pub fn new(name: &str) -> Result<Self> {
        let root = PathBuf::from(format!("/tmp/slopty-claude-mod-{name}"));
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        let home = root.join("home");
        std::fs::create_dir_all(home.join(".claude"))?;
        std::fs::create_dir_all(root.join("work"))?;
        let work = std::fs::canonicalize(root.join("work"))?;
        let key_tail = PLACEHOLDER_KEY
            .get(PLACEHOLDER_KEY.len().saturating_sub(20)..)
            .context("a placeholder key")?;
        let config = json!({
            "hasCompletedOnboarding": true,
            "theme": "dark",
            "customApiKeyResponses": { "approved": [key_tail], "rejected": [] },
            "projects": {
                work.to_string_lossy(): {
                    "hasTrustDialogAccepted": true,
                    "hasCompletedProjectOnboarding": true,
                },
            },
        });
        std::fs::write(home.join(".claude.json"), serde_json::to_vec_pretty(&config)?)?;
        Ok(Self { root, home, work })
    }

    /// `claude` in the project folder with the scratch home and nothing of this environment.
    pub fn claude(&self, claude: &Path) -> Command {
        let mut command = Command::new(claude);
        command
            .current_dir(&self.work)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("TERM", "xterm-256color")
            .env("ANTHROPIC_API_KEY", PLACEHOLDER_KEY)
            .env("ANTHROPIC_BASE_URL", "http://127.0.0.1:9")
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .env("CLAUDE_CODE_TMPDIR", self.root.join("tmp"));
        command
    }
}

/// A program on a pseudo-terminal of 160 × 50, its output kept and its terminal queries (the
/// cursor's position, the device's attributes) answered as a terminal would.
pub struct Tui {
    master: Arc<OwnedFd>,
    child: Child,
    /// What it wrote, as the reader hands it over, and what was taken of that so far.
    output: RefCell<(mpsc::Receiver<Vec<u8>>, Vec<u8>)>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Tui {
    /// `command` started on a new pseudo-terminal, in a process group of its own.
    pub fn start(mut command: Command) -> Result<Self> {
        use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
        // Not the TUI's: macOS has no flag for it at open.
        rustix::io::fcntl_setfd(&master, rustix::io::FdFlags::CLOEXEC)?;
        grantpt(&master)?;
        unlockpt(&master)?;
        let name = ptsname(&master, Vec::new())?;
        let slave: OwnedFd = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(name.to_str().context("the terminal's name")?)?
            .into();
        let size = rustix::termios::Winsize { ws_row: 50, ws_col: 160, ws_xpixel: 0, ws_ypixel: 0 };
        rustix::termios::tcsetwinsize(&slave, size)?;
        command
            .env("COLUMNS", "160")
            .env("LINES", "50")
            .stdin(slave.try_clone()?)
            .stdout(slave.try_clone()?)
            .stderr(slave)
            .process_group(0);
        let child = command.spawn().context("start the TUI")?;
        let master = Arc::new(master);
        let (output, taken) = mpsc::channel();
        let reader = {
            let master = Arc::clone(&master);
            std::thread::spawn(move || {
                let mut buf = vec![0_u8; 65_536];
                while let Ok(n) = rustix::io::read(&*master, buf.as_mut_slice()) {
                    let Some(chunk) = buf.get(..n).filter(|c| !c.is_empty()) else { break };
                    for (query, answer) in
                        [(&b"\x1b[6n"[..], &b"\x1b[1;1R"[..]), (b"\x1b[c", b"\x1b[?62c")]
                    {
                        if chunk.windows(query.len()).any(|w| w == query) {
                            let _written = rustix::io::write(&*master, answer);
                        }
                    }
                    if output.send(chunk.to_vec()).is_err() {
                        break;
                    }
                }
            })
        };
        let output = RefCell::new((taken, Vec::new()));
        Ok(Self { master, child, output, reader: Some(reader) })
    }

    /// Type `bytes`.
    pub fn send(&self, bytes: &[u8]) -> Result<()> {
        let mut rest = bytes;
        while !rest.is_empty() {
            let n = rustix::io::write(&*self.master, rest)?;
            rest = rest.get(n..).unwrap_or_default();
        }
        Ok(())
    }

    /// Everything it wrote, as text without its escape sequences.
    pub fn screen_text(&self) -> String {
        let mut output = self.output.borrow_mut();
        let (taken, bytes) = &mut *output;
        bytes.extend(taken.try_iter().flatten());
        strip_escapes(&String::from_utf8_lossy(bytes))
    }

    /// End it and every process it started.
    pub fn stop(mut self) {
        self.end();
    }

    fn end(&mut self) {
        if let Some(pid) = rustix::process::Pid::from_raw(self.child.id().try_into().unwrap_or(0)) {
            let _killed = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let _killed = self.child.kill();
        let _waited = self.child.wait();
        if let Some(reader) = self.reader.take() {
            // The reader ends once the terminal's last writer is gone.
            let _joined = reader.join();
        }
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        if self.reader.is_some() {
            self.end();
        }
    }
}

/// Wait until `done` holds, for at most `limit`; whether it did.
pub fn until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now().checked_add(limit);
    loop {
        if done() {
            return true;
        }
        if deadline.is_none_or(|d| Instant::now() >= d) {
            return false;
        }
        pause(Duration::from_millis(50));
    }
}

/// Sleep `period`: a recording waits as a person types, and a check waits on a TUI.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library")]
pub fn pause(period: Duration) {
    std::thread::sleep(period);
}

/// `text` without CSI, OSC and charset escape sequences.
pub fn strip_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            Some('(' | ')') => {
                chars.next();
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mutant's id goes where Stryker 10's header reads the environment, quoted; with none
    /// active it reads `undefined`; a module with no such read is refused when one is active.
    #[test]
    fn a_mutant_s_id_is_written_into_the_module() {
        let header = "function stryNS(){var g=globalThis;g.__stryker__.activeMutant = \
                      g.process && g.process.env && g.process.env.__STRYKER_ACTIVE_MUTANT__;\
                      x = g.process.env.__STRYKER_ACTIVE_MUTANT__;}";
        let written = with_mutant(header, Some("12")).unwrap();
        assert!(written.contains(r#"activeMutant = "12";"#), "{written}");
        assert!(written.contains(r#"x = "12";"#), "{written}");
        assert!(with_mutant(header, None).unwrap().contains("activeMutant = undefined;"));
        with_mutant("let x = 1", Some("3")).unwrap_err();
        assert_eq!(with_mutant("let x = 1", None).unwrap(), "let x = 1");
    }

    /// Escapes go, words stay.
    #[test]
    fn escapes_are_stripped() {
        assert_eq!(
            strip_escapes("\x1b[1;32m❯ hi\x1b[0m\x1b]0;title\x07 there\x1b(B"),
            "❯ hi there"
        );
    }
}
