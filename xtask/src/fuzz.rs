//! `cargo xtask fuzz`: libFuzzer over every decoder that faces a peer or an agent (`fuzz/`).
//!
//! The targets are built once with cargo-fuzz (nightly, `AddressSanitizer`, debug assertions) and
//! each binary then runs for `--time` seconds under `nice`, in fork mode so one crash does not
//! end the run. Each target's corpus grows under `target/fuzz/corpus/<target>` from run to run,
//! from two starts beside it: the committed corpus (`fuzz/corpus/<target>`, see `--cmin`), and
//! seeds rewritten every run from real inputs (the wire goldens in
//! `crates/slopty-proto/tests/snapshots`, the Claude Code fixtures in
//! `crates/slopty-agent/tests/fixtures`, and the files under `fuzz/seeds/<target>`), so a
//! target starts from real messages rather than from nothing. A crash, a leak, an out-of-memory
//! or a timeout lands under `target/fuzz/artifacts/<target>/` and fails the run.
//!
//! `--ziggy` then runs AFL++ on each target for as long again, through ziggy (cargo-afl, on
//! Linux), started from the committed corpus and the seeds: a second engine finds paths the
//! first does not, and what it queued joins the grown corpus libFuzzer starts from next.
//!
//! `--keep <artifact>` minimises one of those inputs and files it under
//! `fuzz/regressions/<target>/`. `--cmin` makes `fuzz/corpus/<target>` the fewest bytes, at most
//! 2 MiB, that reach every edge its grown corpus, its committed one and its seeds reach
//! together. The fuzz crate's `tests/regressions.rs` replays both on an ordinary build, in the
//! gate: `--replay` runs exactly that, optimised, with no nightly and no instrumentation.

use std::fmt::Write as _;
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime};

use anyhow::{Context as _, Result, bail, ensure};
use camino::{Utf8Path, Utf8PathBuf};
use clap::Args;
use xshell::{Shell, cmd};

use crate::tools::{HOST, quiet_step, repo_root};

/// Where the goldens every seed corpus starts from live.
const SNAPSHOTS: &str = "crates/slopty-proto/tests/snapshots";

/// Where each target's committed corpus lives: what `--cmin` kept, which every run starts from
/// and the gate replays.
const CORPUS: &str = "fuzz/corpus";

/// Claude Code's records as a real one wrote them: the seeds of the agent targets.
const AGENT_FIXTURES: &str = "crates/slopty-agent/tests/fixtures";

/// Seeds kept as files, one directory per target (the terminal's are captures, made scripts).
const FILE_SEEDS: &str = "fuzz/seeds";

/// The variable that moves Claude Code's own directory, which the `registry` target would then
/// read instead of its own: never passed to a target.
const CLAUDE_CONFIG_DIR: &str = "CLAUDE_CONFIG_DIR";

/// The byte a stream target reads as its piece size before the stream (`fuzz/src/stream.rs`).
const PIECE: u8 = 61;

#[derive(Args, Debug, Clone)]
pub struct FuzzOpts {
    /// One target (default: every one, one after another).
    pub target: Option<String>,
    /// Seconds each target runs.
    #[arg(long, default_value_t = 60)]
    pub time: u64,
    /// Fuzzing processes per target (libFuzzer's `-fork`).
    #[arg(long, default_value_t = 1)]
    pub jobs: u32,
    /// Only replay `fuzz/regressions/` and `fuzz/corpus/` on an ordinary build.
    #[arg(long, conflicts_with_all = ["keep", "target", "cmin"])]
    pub replay: bool,
    /// Instead of fuzzing, make each target's committed corpus (`fuzz/corpus/<target>`) the
    /// fewest bytes, at most 2 MiB, that reach every edge its grown corpus, its committed one
    /// and its seeds reach.
    #[arg(long, conflicts_with = "keep")]
    pub cmin: bool,
    /// After libFuzzer, run AFL++ on each target for `--time` seconds more, `--jobs` instances,
    /// through ziggy (`cargo ziggy` over cargo-afl; Linux).
    #[arg(long, conflicts_with_all = ["replay", "cmin", "keep"])]
    pub ziggy: bool,
    /// Minimise this artifact (a crash under `target/fuzz/artifacts/<target>/`) and keep it as
    /// a regression input of its target.
    #[arg(long, value_name = "ARTIFACT")]
    pub keep: Option<Utf8PathBuf>,
}

pub fn run(sh: &Shell, opts: &FuzzOpts) -> Result<()> {
    let root = repo_root()?;
    let targets = targets(&root)?;
    if opts.replay {
        return replay(sh);
    }
    if let Some(artifact) = &opts.keep {
        return keep(sh, &root, &targets, artifact);
    }
    let chosen: Vec<String> = match &opts.target {
        Some(one) if targets.contains(one) => vec![one.clone()],
        Some(one) => bail!("no fuzz target named {one}; the targets are {}", targets.join(", ")),
        None => targets,
    };
    if opts.cmin {
        seed(&root, &chosen)?;
        build(sh)?;
        return chosen.iter().try_for_each(|target| cmin(&root, target));
    }
    ensure!(
        !opts.ziggy || cfg!(target_os = "linux"),
        "AFL++ fuzzes on Linux (`deep fuzz --ziggy` on the arm runner); a Mac fuzzes with libFuzzer"
    );
    // A regression that still fails is a known bug: say so first, but fuzz the rest anyway.
    let replayed = replay(sh);
    seed(&root, &chosen)?;
    build(sh)?;
    let mut summary = String::new();
    let mut found = Vec::new();
    for target in &chosen {
        let artifacts = fuzz(&root, target, opts.time, opts.jobs)?;
        let _line = writeln!(
            summary,
            "  {} {target} ({} s): {}",
            if artifacts.is_empty() { "✓" } else { "✘" },
            opts.time,
            if artifacts.is_empty() { "nothing found".to_owned() } else { artifacts.join(", ") }
        );
        found.extend(artifacts);
    }
    if opts.ziggy {
        for target in &chosen {
            let artifacts = afl(sh, &root, target, opts.time, opts.jobs)?;
            let _line = writeln!(
                summary,
                "  {} {target} under AFL++ ({} s): {}",
                if artifacts.is_empty() { "✓" } else { "✘" },
                opts.time,
                if artifacts.is_empty() {
                    "nothing found".to_owned()
                } else {
                    artifacts.join(", ")
                }
            );
            found.extend(artifacts);
        }
    }
    print!("fuzz summary\n{summary}");
    if !found.is_empty() {
        println!(
            "  minimise one into a regression input with `cargo xtask fuzz --keep <artifact>`"
        );
    }
    replayed?;
    ensure!(found.is_empty(), "fuzzing found {} input(s) that fail", found.len());
    Ok(())
}

/// The targets, from the files under `fuzz/fuzz_targets` (the fuzz crate's own test holds them
/// equal to its `TARGETS`).
fn targets(root: &Utf8Path) -> Result<Vec<String>> {
    let dir = root.join("fuzz/fuzz_targets");
    let mut names: Vec<String> = dir
        .read_dir_utf8()
        .with_context(|| format!("reading {dir}"))?
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().strip_suffix(".rs").map(str::to_owned))
        .collect();
    names.sort();
    Ok(names)
}

/// The regression inputs and the committed corpora through their targets, on a plain build of
/// the fuzz crate: what the gate runs. Its target dir is the shell's (a gate lane's own), else
/// `target/fuzz/test`, named by flag: a `CARGO_TARGET_DIR` set here would reach every rustc's
/// environment, which the compile cache keys units by. Never under `DOCS_RS`, which an Apple
/// lint off a Mac sets: libghostty-vt-sys then builds no library to link.
pub fn replay(sh: &Shell) -> Result<()> {
    ensure!(sh.var_os("DOCS_RS").is_none(), "the fuzz replay links libghostty-vt: unset DOCS_RS");
    let dir: &[&str] = if sh.var_os("CARGO_TARGET_DIR").is_some() {
        &[]
    } else {
        &["--target-dir", "target/fuzz/test"]
    };
    quiet_step(
        "fuzz regressions and corpora",
        cmd!(sh, "cargo test --manifest-path fuzz/Cargo.toml --locked --tests {dir...}")
            .env_remove(CLAUDE_CONFIG_DIR),
    )
}

/// The nightly toolchain and the flags every cargo-fuzz call shares: libFuzzer's entry points
/// (`slopty_fuzz::target!`), the host triple named (cargo-fuzz otherwise takes the triple it was
/// itself built for, x86-64 when it came as a Rosetta binary), debug assertions on, and a target
/// dir of its own. No target calls Apple code, so CI fuzzes on Linux.
fn cargo_fuzz<'a>(sh: &'a Shell, verb: &str) -> xshell::Cmd<'a> {
    let host = HOST;
    cmd!(
        sh,
        "cargo +nightly fuzz {verb} --features libfuzzer --debug-assertions --target {host} --target-dir target/fuzz/build"
    )
    .env("RUSTC_WRAPPER", "")
    // libghostty-vt is Zig, which ASan does not see into: built `ReleaseSafe`, an out-of-bounds
    // index, an overflow or a bad cast in the parser traps instead of going on quietly, as the
    // shipped `ReleaseFast` would.
    .env("LIBGHOSTTY_VT_SYS_OPTIMIZE", "ReleaseSafe")
}

fn build(sh: &Shell) -> Result<()> {
    quiet_step("cargo fuzz build", cargo_fuzz(sh, "build"))
}

fn binary(root: &Utf8Path, target: &str) -> Utf8PathBuf {
    root.join("target/fuzz/build").join(HOST).join("release").join(target)
}

/// Run one target for `seconds`; the artifacts it left.
fn fuzz(root: &Utf8Path, target: &str, seconds: u64, jobs: u32) -> Result<Vec<String>> {
    let corpus = root.join("target/fuzz/corpus").join(target);
    let committed = root.join(CORPUS).join(target);
    let seeds = root.join("target/fuzz/seeds").join(target);
    let artifacts = root.join("target/fuzz/artifacts").join(target);
    let logs = root.join("target/fuzz/logs");
    for dir in [&corpus, &seeds, &artifacts, &logs] {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {dir}"))?;
    }
    let log = logs.join(format!("{target}.log"));
    let started = SystemTime::now();
    let clock = Instant::now();
    println!("▶ fuzz {target} ({seconds} s)");
    let out = std::fs::File::create(&log).with_context(|| format!("creating {log}"))?;
    let status = Command::new("nice")
        .args(["-n", "10"])
        .arg(binary(root, target))
        // libFuzzer writes what it finds into the first directory; the others only start it.
        .arg(&corpus)
        .args(committed.exists().then_some(&committed))
        .arg(&seeds)
        .args(libfuzzer_flags(seconds, jobs, &artifacts))
        .arg(format!("-rss_limit_mb={}", rss_limit_mb(target)))
        .args(dictionary(root, target))
        .env_remove(CLAUDE_CONFIG_DIR)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out)
        .status()
        .with_context(|| format!("starting the {target} fuzzer"))?;
    let found = new_artifacts(&artifacts, started)?;
    println!(
        "  {} fuzz {target} ({:.0?}, {}; log {log})",
        if found.is_empty() && status.success() { "✓" } else { "✘" },
        clock.elapsed(),
        stats(&log),
    );
    if found.is_empty() && !status.success() {
        bail!("the {target} fuzzer failed without leaving an artifact ({status}); see {log}");
    }
    Ok(found)
}

/// Where ziggy builds the targets for AFL++ and each run's instances write (`out/<target>`).
const ZIGGY: &str = "target/fuzz/ziggy";

/// AFL++ on `target` for `seconds` through ziggy: `jobs` instances (its main, then secondaries
/// on other power schedules), started from the committed corpus and the seeds alone, which are
/// small, since AFL++ runs every start before it fuzzes. What its instances queued joins the
/// grown corpus, and what crashed lands under `target/fuzz/artifacts/<target>/` as libFuzzer's
/// finds do; the crashes.
fn afl(sh: &Shell, root: &Utf8Path, target: &str, seconds: u64, jobs: u32) -> Result<Vec<String>> {
    let work = root.join(ZIGGY);
    let output = work.join("out");
    let start = work.join("start").join(target);
    for dir in [output.join(target), start.clone()] {
        if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("clearing {dir}"))?;
        }
    }
    std::fs::create_dir_all(&start).with_context(|| format!("creating {start}"))?;
    for from in [root.join(CORPUS).join(target), root.join("target/fuzz/seeds").join(target)] {
        copy_inputs(&from, &start, "")?;
    }
    let logs = root.join("target/fuzz/logs");
    std::fs::create_dir_all(&logs).with_context(|| format!("creating {logs}"))?;
    let log = logs.join(format!("{target}-afl.log"));
    let build = work.join("build");
    // As cargo-fuzz builds: optimised with debug assertions on, the parser `ReleaseSafe`, and no
    // compile cache.
    let env = [
        ("CARGO_TARGET_DIR", build.as_str()),
        ("CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS", "true"),
        ("LIBGHOSTTY_VT_SYS_OPTIMIZE", "ReleaseSafe"),
        ("RUSTC_WRAPPER", ""),
    ];
    let fuzz_dir = root.join("fuzz");
    {
        let _dir = sh.push_dir(&fuzz_dir);
        quiet_step(
            &format!("cargo ziggy build {target}"),
            cmd!(sh, "cargo ziggy build {target} --no-honggfuzz --release")
                .envs(env)
                .env_remove(CLAUDE_CONFIG_DIR),
        )?;
    }
    let clock = Instant::now();
    println!("▶ AFL++ {target} ({seconds} s, {jobs} instances)");
    let out = std::fs::File::create(&log).with_context(|| format!("creating {log}"))?;
    let status = Command::new("timeout")
        // SIGINT to the group: ziggy stops its instances and AFL++ writes out what it holds.
        .args(["--signal=INT", "--kill-after=60"])
        .arg(seconds.to_string())
        .args(["nice", "-n", "10", "cargo", "ziggy", "fuzz", target, "--no-honggfuzz", "--release"])
        .arg(format!("--jobs={}", jobs.max(1)))
        .arg(format!("--corpus={start}"))
        .arg(format!("--ziggy-output={output}"))
        .arg("--maxlength=65536")
        .args(dictionary(root, target).map(|d| d.replacen("-dict=", "--dict=", 1)))
        .envs(env)
        // Off a machine of its own: no CPU governor to set, crashes reported to a core handler,
        // and cores AFL++ may find busy.
        .env("AFL_SKIP_CPUFREQ", "1")
        .env("AFL_I_DONT_CARE_ABOUT_MISSING_CRASHES", "1")
        .env("AFL_NO_AFFINITY", "1")
        .env_remove(CLAUDE_CONFIG_DIR)
        .current_dir(&fuzz_dir)
        .stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out)
        .status()
        .with_context(|| format!("starting ziggy on {target}"))?;
    let instances = output.join(target).join("afl");
    let stats = instances.join("mainaflfuzzer/fuzzer_stats");
    // `timeout` answers 124 when the time ran out, and 137 when the stop took past its grace and
    // the group was killed; either way the instances wrote what they found.
    ensure!(
        stats.exists() && (status.success() || matches!(status.code(), Some(124 | 137))),
        "AFL++ did not run {target} ({status}); see {log} and {}",
        output.join(target).join("logs"),
    );
    let corpus = root.join("target/fuzz/corpus").join(target);
    let artifacts = root.join("target/fuzz/artifacts").join(target);
    let mut found = Vec::new();
    for instance in instances.read_dir_utf8().with_context(|| format!("reading {instances}"))? {
        let instance = instance?.into_path();
        if instance.is_dir() {
            copy_inputs(&instance.join("queue"), &corpus, "afl-")?;
            found.extend(copy_inputs(&instance.join("crashes"), &artifacts, "crash-afl-")?);
        }
    }
    found.sort();
    found.dedup();
    println!(
        "  {} AFL++ {target} ({:.0?}, {}; log {log})",
        if found.is_empty() { "✓" } else { "✘" },
        clock.elapsed(),
        afl_stats(&stats),
    );
    Ok(found)
}

/// Copy each input in `from` into `into`, named `prefix` and a hash of its bytes, so the same
/// input is never kept twice; the copies' paths. Of AFL++'s files, its `README.txt`, its state
/// and the starts it queued as they were (`orig:` in the name) are passed over. A missing `from`
/// holds nothing.
fn copy_inputs(from: &Utf8Path, into: &Utf8Path, prefix: &str) -> Result<Vec<String>> {
    if !from.exists() {
        return Ok(Vec::new());
    }
    std::fs::create_dir_all(into).with_context(|| format!("creating {into}"))?;
    let mut copied = Vec::new();
    for entry in from.read_dir_utf8().with_context(|| format!("reading {from}"))? {
        let entry = entry?;
        let name = entry.file_name();
        if !entry.path().is_file()
            || name.starts_with('.')
            || name == "README.txt"
            || name.contains(",orig:")
        {
            continue;
        }
        let bytes =
            std::fs::read(entry.path()).with_context(|| format!("reading {}", entry.path()))?;
        let copy = into.join(format!("{prefix}{:016x}", fnv1a(&bytes)));
        if !copy.exists() {
            std::fs::write(&copy, &bytes).with_context(|| format!("writing {copy}"))?;
        }
        copied.push(copy.to_string());
    }
    Ok(copied)
}

/// FNV-1a over `bytes`: a name for an input that stays the same from run to run.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The main AFL++ instance's runs, speed, queue and edges, from its `fuzzer_stats`.
fn afl_stats(stats: &Utf8Path) -> String {
    let text = std::fs::read_to_string(stats).unwrap_or_default();
    ["execs_done", "execs_per_sec", "corpus_count", "edges_found"]
        .iter()
        .filter_map(|key| {
            let line =
                text.lines().find(|l| l.split(':').next().is_some_and(|k| k.trim() == *key))?;
            Some(format!("{key}: {}", line.split_once(':')?.1.trim()))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// libFuzzer's flags for a smoke of `seconds`: fork mode, so a crash is written and the run
/// goes on to find the next one, and the memory limits the soak would call a leak.
fn libfuzzer_flags(seconds: u64, jobs: u32, artifacts: &Utf8Path) -> Vec<String> {
    vec![
        format!("-max_total_time={seconds}"),
        format!("-fork={}", jobs.max(1)),
        "-ignore_crashes=1".to_owned(),
        "-ignore_ooms=1".to_owned(),
        "-ignore_timeouts=1".to_owned(),
        "-timeout=10".to_owned(),
        "-malloc_limit_mb=2048".to_owned(),
        "-max_len=65536".to_owned(),
        format!("-artifact_prefix={artifacts}/"),
        "-print_final_stats=1".to_owned(),
    ]
}

/// The resident size past which libFuzzer calls an input out of memory. 2 GiB, except for the
/// `terminal` target: under `AddressSanitizer` its process grows about 0.4 MB with every run
/// (878 MB after 2 000 runs of one input, with the quarantine off), which no malloc accounts
/// for (`LeakSanitizer` finds nothing) and which a plain build does not show (8.5 MB after 200
/// runs and after 2 000, with the parser built `ReleaseFast` or `ReleaseSafe`). A single input's
/// own blow-up is still caught by `-malloc_limit_mb`.
fn rss_limit_mb(target: &str) -> u32 {
    if target == "terminal" { 8_192 } else { 2_048 }
}

/// `-dict=` with the target's dictionary, when `fuzz/dicts/<target>.dict` holds one.
fn dictionary(root: &Utf8Path, target: &str) -> Option<String> {
    let dict = root.join("fuzz/dicts").join(format!("{target}.dict"));
    dict.exists().then(|| format!("-dict={dict}"))
}

/// Crash, leak, out-of-memory and timeout inputs written under `dir` since `since`.
fn new_artifacts(dir: &Utf8Path, since: SystemTime) -> Result<Vec<String>> {
    let mut found: Vec<String> = dir
        .read_dir_utf8()
        .with_context(|| format!("reading {dir}"))?
        .filter_map(Result::ok)
        .filter(|e| e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t >= since))
        .map(|e| e.path().to_string())
        .collect();
    found.sort();
    Ok(found)
}

/// The run's coverage and speed, from libFuzzer's last status line.
fn stats(log: &Utf8Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let last = text
        .lines()
        .rev()
        .find(|l| l.contains(" cov: ") || l.starts_with('#') && l.contains("exec/s"))
        .unwrap_or("no status line");
    last.split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .filter(|w| matches!(w.first(), Some(&("cov:" | "ft:" | "corp:" | "exec/s:"))))
        .filter_map(|w| Some(format!("{} {}", w.first()?, w.get(1)?)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Minimise `artifact` and file it under `fuzz/regressions/<target>/`.
fn keep(sh: &Shell, root: &Utf8Path, targets: &[String], artifact: &Utf8Path) -> Result<()> {
    let target = artifact
        .parent()
        .and_then(Utf8Path::file_name)
        .filter(|t| targets.iter().any(|n| n == t))
        .with_context(|| {
            format!("{artifact} is not under target/fuzz/artifacts/<target>/ of a known target")
        })?;
    let name = artifact.file_name().context("the artifact has no file name")?;
    let dir = root.join("fuzz/regressions").join(target);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {dir}"))?;
    let kept = dir.join(name);
    build(sh)?;
    let bin = binary(root, target);
    let exact = format!("-exact_artifact_path={kept}");
    // `-minimize_crash` keeps cutting while the input still crashes; an input that does not
    // crash (a timeout, an out-of-memory) is kept as it is.
    let minimised = cmd!(sh, "nice -n 10 {bin} -minimize_crash=1 -runs=20000 {exact} {artifact}")
        .quiet()
        .ignore_status()
        .output()
        .context("minimising")?;
    if !kept.exists() || !minimised.status.success() && std::fs::metadata(&kept)?.len() == 0 {
        std::fs::copy(artifact, &kept).with_context(|| format!("copying to {kept}"))?;
    }
    let size = std::fs::metadata(&kept)?.len();
    println!("✔ kept {kept} ({size} bytes); `cargo xtask fuzz --replay` replays it");
    Ok(())
}

/// The most bytes a target's committed corpus may hold, as `fuzz/tests/regressions.rs` holds it.
const CORPUS_BYTES: u64 = 2 << 20;

/// What a committed input costs beside its bytes, in bytes: its file, and its replay in the gate.
const INPUT_COST: u64 = 64;

/// Make `target`'s committed corpus the fewest bytes, within [`CORPUS_BYTES`], that reach every
/// edge its grown corpus, its committed one and its seeds reach together.
///
/// libFuzzer runs every input once (`-merge=1 -set_cover_merge=1`, passing over one that
/// crashes) and writes each one's edges (`-use_counters=0`: what it reached, not how often) to
/// its control file. libFuzzer's own pick wants the fewest inputs, which takes the longest ones;
/// this one takes, again and again, the input that reaches the most edges not yet reached for
/// its bytes, until every edge is reached or the budget is spent. The grown corpus keeps every
/// count the fuzzer told apart, and every edge past the budget.
fn cmin(root: &Utf8Path, target: &str) -> Result<()> {
    let committed = root.join(CORPUS).join(target);
    let work = root.join("target/fuzz/cmin").join(target);
    if work.exists() {
        std::fs::remove_dir_all(&work).with_context(|| format!("clearing {work}"))?;
    }
    let (picked, fresh) = (work.join("libfuzzer"), work.join("corpus"));
    for dir in [&picked, &fresh] {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {dir}"))?;
    }
    let control = work.join("control.txt");
    let starts: Vec<Utf8PathBuf> = [
        committed.clone(),
        root.join("target/fuzz/corpus").join(target),
        root.join("target/fuzz/seeds").join(target),
    ]
    .into_iter()
    .filter(|dir| dir.exists())
    .collect();
    let log = root.join("target/fuzz/logs").join(format!("{target}-cmin.log"));
    std::fs::create_dir_all(root.join("target/fuzz/logs"))?;
    let out = std::fs::File::create(&log).with_context(|| format!("creating {log}"))?;
    let status = Command::new("nice")
        .args(["-n", "10"])
        .arg(binary(root, target))
        .args(["-merge=1", "-set_cover_merge=1", "-use_counters=0"])
        .arg(format!("-merge_control_file={control}"))
        .arg(&picked)
        .args(&starts)
        .args(["-timeout=10", "-malloc_limit_mb=2048", "-max_len=65536"])
        .arg(format!("-rss_limit_mb={}", rss_limit_mb(target)))
        .args(dictionary(root, target))
        .env_remove(CLAUDE_CONFIG_DIR)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out)
        .status()
        .with_context(|| format!("starting the {target} merge"))?;
    ensure!(status.success(), "the {target} merge failed ({status}); see {log}");
    let text = std::fs::read_to_string(&control).with_context(|| format!("reading {control}"))?;
    let runs = merge_runs(&text).with_context(|| format!("reading {control}"))?;
    let cover = cheapest_cover(&runs, CORPUS_BYTES);
    for &i in &cover.inputs {
        let run = runs.get(i).context("a chosen input is one of the runs")?;
        let mut bytes =
            std::fs::read(&run.path).with_context(|| format!("reading {}", run.path))?;
        // As libFuzzer ran it: cut at `-max_len`.
        bytes.truncate(usize::try_from(run.size)?);
        std::fs::write(fresh.join(format!("{:016x}", fnv1a(&bytes))), &bytes)?;
    }
    let (count, bytes) = size_of_dir(&fresh)?;
    if committed.exists() {
        std::fs::remove_dir_all(&committed).with_context(|| format!("clearing {committed}"))?;
    }
    std::fs::create_dir_all(committed.parent().context("the corpus has a parent")?)?;
    std::fs::rename(&fresh, &committed).with_context(|| format!("moving into {committed}"))?;
    std::fs::remove_dir_all(&work).with_context(|| format!("clearing {work}"))?;
    println!(
        "✔ {target}: {count} inputs, {bytes} bytes, {} of {} edges in {committed}",
        cover.reached, cover.edges
    );
    Ok(())
}

/// One input a merge ran: its file, the bytes it ran (cut at `-max_len`), and the edges it
/// reached.
#[derive(Debug)]
struct Run {
    path: Utf8PathBuf,
    size: u64,
    edges: Vec<u32>,
}

/// The inputs a libFuzzer merge's control file says it ran, each with its edges; one that
/// crashed (no `FT` line) is left out. The file is the input count, the count of the first
/// corpus, one path a line, then `STARTED <i> <size>`, `FT <i> <feature>…` and `COV <i> …` lines.
fn merge_runs(control: &str) -> Result<Vec<Run>> {
    let mut lines = control.lines();
    let count: usize = lines.next().context("no input count")?.trim().parse()?;
    let _first_corpus = lines.next().context("no first corpus count")?;
    let paths: Vec<Utf8PathBuf> = lines.by_ref().take(count).map(Utf8PathBuf::from).collect();
    ensure!(paths.len() == count, "{count} inputs named, {} paths", paths.len());
    let mut sizes = vec![None; count];
    let mut edges: Vec<Option<Vec<u32>>> = vec![None; count];
    for line in lines {
        let mut words = line.split_ascii_whitespace();
        let (Some(marker), Some(index)) = (words.next(), words.next()) else { continue };
        let index: usize = index.parse()?;
        match marker {
            "STARTED" => {
                let size: u64 = words.next().context("STARTED with no size")?.parse()?;
                *sizes.get_mut(index).context("STARTED past the inputs")? = Some(size);
            }
            "FT" => {
                let features = words.map(str::parse).collect::<Result<Vec<u32>, _>>()?;
                *edges.get_mut(index).context("FT past the inputs")? = Some(features);
            }
            _ => {}
        }
    }
    Ok(paths
        .into_iter()
        .zip(sizes)
        .zip(edges)
        .filter_map(|((path, size), edges)| Some(Run { path, size: size?, edges: edges? }))
        .collect())
}

/// The inputs [`cheapest_cover`] chose, how many edges they reach, and of how many.
#[derive(Debug, PartialEq, Eq)]
struct Cover {
    inputs: Vec<usize>,
    reached: usize,
    edges: usize,
}

/// A cover of every edge the runs reach, cheap in bytes, within `budget`: the greedy answer to
/// weighted set cover (within a factor of ln n of the cheapest), lazily, since what an input
/// adds only shrinks as others are taken. An input that would pass the budget is passed over.
fn cheapest_cover(runs: &[Run], budget: u64) -> Cover {
    use std::cmp::Reverse;
    use std::collections::{BinaryHeap, HashSet};

    let all: HashSet<u32> = runs.iter().flat_map(|run| run.edges.iter().copied()).collect();
    let mut reached: HashSet<u32> = HashSet::with_capacity(all.len());
    // New edges per byte, scaled to an integer so the heap orders it; the lower index wins a tie.
    let score = |gain: usize, run: &Run| {
        u64::try_from(gain)
            .unwrap_or(u64::MAX)
            .saturating_mul(1 << 20)
            .checked_div(run.size.saturating_add(INPUT_COST))
            .unwrap_or(0)
    };
    let mut heap: BinaryHeap<(u64, Reverse<usize>)> = runs
        .iter()
        .enumerate()
        .filter(|(_, run)| !run.edges.is_empty())
        .map(|(i, run)| (score(run.edges.len(), run), Reverse(i)))
        .collect();
    let (mut inputs, mut spent) = (Vec::new(), 0_u64);
    while let Some((_, Reverse(i))) = heap.pop() {
        let Some(run) = runs.get(i) else { continue };
        let gain = run.edges.iter().filter(|edge| !reached.contains(*edge)).count();
        if gain == 0 || spent.saturating_add(run.size) > budget {
            continue;
        }
        let now = score(gain, run);
        if heap.peek().is_some_and(|&(next, _)| next > now) {
            heap.push((now, Reverse(i)));
            continue;
        }
        reached.extend(run.edges.iter().copied());
        spent = spent.saturating_add(run.size);
        inputs.push(i);
        if reached.len() == all.len() {
            break;
        }
    }
    Cover { inputs, reached: reached.len(), edges: all.len() }
}

/// How many files `dir` holds, and their bytes.
fn size_of_dir(dir: &Utf8Path) -> Result<(usize, u64)> {
    let mut count = 0_usize;
    let mut bytes = 0_u64;
    for entry in dir.read_dir_utf8().with_context(|| format!("reading {dir}"))? {
        let entry = entry?;
        count = count.saturating_add(1);
        bytes = bytes.saturating_add(entry.metadata()?.len());
    }
    Ok((count, bytes))
}

/// Seed corpora from the wire goldens: each hex snapshot as the stream or datagram it is, the
/// JSON ones as control-socket lines; the `terminal` target's from the captures under
/// [`TERMINAL_SEEDS`]; the agent targets' from Claude Code's fixtures ([`agent_seeds`]); and any
/// other target's from its files under [`FILE_SEEDS`]. The fuzzer grows its own corpus beside
/// them.
fn seed(root: &Utf8Path, chosen: &[String]) -> Result<()> {
    let mut seeds = seeds_from(&root.join(SNAPSHOTS))?;
    seeds.extend(terminal_seeds(&root.join(TERMINAL_SEEDS))?);
    seeds.extend(agent_seeds(&root.join(AGENT_FIXTURES))?);
    seeds.extend(file_seeds(&root.join(FILE_SEEDS))?);
    for target in chosen {
        let dir = root.join("target/fuzz/seeds").join(target);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("clearing {dir}"))?;
        }
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {dir}"))?;
        for (i, input) in seeds.iter().filter(|(t, _)| t == target).map(|(_, b)| b).enumerate() {
            std::fs::write(dir.join(format!("golden-{i:04}")), input)?;
        }
    }
    Ok(())
}

/// Program output captured as it reached a terminal, one sequence family per file.
const TERMINAL_SEEDS: &str = "fuzz/seeds/terminal";

/// Each capture under `dir` as a `terminal` script (`fuzz/src/terminal.rs`): an 80 × 24 screen,
/// the capture written in pieces of at most 64 bytes, then a viewer joining, a checkpoint, a
/// resize to 40 × 12 and a checkpoint there. All the captures in one script too.
fn terminal_seeds(dir: &Utf8Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut captures = Vec::new();
    for entry in dir.read_dir_utf8().with_context(|| format!("reading {dir}"))? {
        let path = entry?.into_path();
        if path.extension() == Some("vt") {
            captures.push(std::fs::read(&path).with_context(|| format!("reading {path}"))?);
        }
    }
    captures.sort();
    let whole = captures.concat();
    Ok(captures
        .iter()
        .chain(std::iter::once(&whole))
        .map(|capture| ("terminal".to_owned(), terminal_script(capture)))
        .collect())
}

/// `bytes` as a `terminal` script: the size, the writes, then the closing operations.
fn terminal_script(bytes: &[u8]) -> Vec<u8> {
    let mut script = vec![79, 23];
    for piece in bytes.chunks(64) {
        // `op % 64 + 1` bytes follow a write's op byte; a piece is 1 to 64 long.
        script.push(u8::try_from(piece.len().saturating_sub(1)).unwrap_or(63));
        script.extend_from_slice(piece);
    }
    script.extend_from_slice(&[0xf4, 0xfc, 0xf0, 39, 11, 0xfc]);
    script
}

/// The `transcript` target's input (`fuzz/src/transcript.rs`): the lines a read hands over
/// (`per_read`, less one), then each file's text after the line naming it, `\x01` and the
/// subagent's id (`None` the main transcript, named only after another file).
fn transcript_input(per_read: u8, files: &[(Option<String>, String)]) -> Vec<u8> {
    let mut out = vec![per_read];
    for (agent, text) in files {
        if let Some(id) = agent {
            out.extend(format!("\u{1}{id}\n").into_bytes());
        } else if out.len() > 1 {
            out.extend(b"\x01\n");
        }
        out.extend_from_slice(text.as_bytes());
        if !text.ends_with('\n') {
            out.push(b'\n');
        }
    }
    out
}

/// The agent targets' seeds, from Claude Code's records under `dir` as a real one wrote them:
/// - `transcript`: each scenario's files, the main transcript and every subagent's, and each of
///   their records alone;
/// - `hooks`: each scenario's payloads in the order they came, and each alone; and the prompt box's
///   payloads Slopty's mod relayed, when its recording is there;
/// - `registry`: each live record of itself a Claude Code wrote.
fn agent_seeds(dir: &Utf8Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    let read =
        |path: &Utf8Path| std::fs::read_to_string(path).with_context(|| format!("reading {path}"));
    let conversation = dir.join("conversation");
    let mut scenarios: Vec<Utf8PathBuf> = conversation
        .read_dir_utf8()
        .with_context(|| format!("reading {conversation}"))?
        .filter_map(Result::ok)
        .map(camino::Utf8DirEntry::into_path)
        .filter(|path| path.is_dir())
        .collect();
    scenarios.sort();
    for scenario in &scenarios {
        let mut files = vec![(None, read(&scenario.join("transcript.jsonl"))?)];
        let agents = scenario.join("subagents");
        if agents.exists() {
            let mut named: Vec<Utf8PathBuf> = agents
                .read_dir_utf8()?
                .filter_map(Result::ok)
                .map(camino::Utf8DirEntry::into_path)
                .collect();
            named.sort();
            for path in named {
                let id = path.file_stem().and_then(|s| s.strip_prefix("agent-")).map(str::to_owned);
                files.push((id, read(&path)?));
            }
        }
        out.push(("transcript".to_owned(), transcript_input(2, &files)));
        for (_, text) in &files {
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                out.push((
                    "transcript".to_owned(),
                    transcript_input(0, &[(None, line.to_owned())]),
                ));
            }
        }
        let hooks = scenario.join("hooks.jsonl");
        if hooks.exists() {
            let payloads = recorded(&read(&hooks)?, "input");
            out.push(("hooks".to_owned(), payloads.join("\n").into_bytes()));
            out.extend(payloads.into_iter().map(|p| ("hooks".to_owned(), p.into_bytes())));
        }
    }
    let drafts = dir.join("claude-mod/drafts.jsonl");
    if drafts.exists() {
        out.push((
            "hooks".to_owned(),
            recorded(&read(&drafts)?, "payload").join("\n").into_bytes(),
        ));
    }
    let registry = dir.join("registry");
    if registry.exists() {
        let mut records: Vec<Utf8PathBuf> = registry
            .read_dir_utf8()?
            .filter_map(Result::ok)
            .map(camino::Utf8DirEntry::into_path)
            .filter(|path| path.extension() == Some("json"))
            .collect();
        records.sort();
        for path in records {
            out.push(("registry".to_owned(), std::fs::read(&path)?));
        }
    }
    Ok(out)
}

/// The `field` of each JSON line in `jsonl` (a fixture's recorded payload), as JSON.
fn recorded(jsonl: &str, field: &str) -> Vec<String> {
    jsonl
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|record| record.get(field).map(ToString::to_string))
        .collect()
}

/// Each file under `dir/<target>/` as that target's seed, for every target but the terminal
/// (whose captures [`terminal_seeds`] makes scripts of); [`seed_inputs`] says what a file
/// becomes.
fn file_seeds(dir: &Utf8Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut targets: Vec<Utf8PathBuf> = dir
        .read_dir_utf8()
        .with_context(|| format!("reading {dir}"))?
        .filter_map(Result::ok)
        .map(camino::Utf8DirEntry::into_path)
        .filter(|path| path.is_dir() && !path.ends_with("terminal"))
        .collect();
    targets.sort();
    for target_dir in targets {
        let target = target_dir.file_name().context("a seed directory's name")?.to_owned();
        let mut files: Vec<Utf8PathBuf> = target_dir
            .read_dir_utf8()?
            .filter_map(Result::ok)
            .map(camino::Utf8DirEntry::into_path)
            .filter(|path| path.is_file())
            .collect();
        files.sort();
        for path in files {
            let bytes = std::fs::read(&path).with_context(|| format!("reading {path}"))?;
            for input in seed_inputs(&target, path.extension(), &bytes) {
                out.push((target.clone(), input));
            }
        }
    }
    Ok(out)
}

/// What a seed file becomes for `target`. A `markdown` text is streamed in deltas of 7 bytes and
/// of 64; a `visual` file's leading byte says what it is by its extension (a script, an image,
/// a model, a shader's GLSL; `fuzz/src/visual.rs`); any other target takes the file as it is.
fn seed_inputs(target: &str, extension: Option<&str>, bytes: &[u8]) -> Vec<Vec<u8>> {
    let led = |lead: u8| [&[lead][..], bytes].concat();
    match (target, extension) {
        ("markdown", _) => vec![led(7), led(64)],
        ("visual", Some("luau")) => vec![led(0)],
        ("visual", Some("png" | "jpg" | "webp" | "hdr")) => vec![led(1)],
        ("visual", Some("glb" | "gltf")) => vec![led(2)],
        ("visual", Some("glsl")) => vec![led(3)],
        _ => vec![bytes.to_vec()],
    }
}

/// A golden snapshot: its name (`golden__<file>__<name>`) and body.
struct Golden {
    name: String,
    body: Body,
}

enum Body {
    /// `hex(&bytes)`: a framed message, a datagram or a header.
    Bytes(Vec<u8>),
    /// `line`: a control-socket line.
    Line(String),
}

/// Every seed, as `(target, input)`.
fn seeds_from(dir: &Utf8Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut goldens = Vec::new();
    for entry in dir.read_dir_utf8().with_context(|| format!("reading {dir}"))? {
        let path = entry?.into_path();
        let Some(stem) = path.file_stem().filter(|_| path.extension() == Some("snap")) else {
            continue;
        };
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
        if let Some(golden) = parse_snapshot(stem, &text) {
            goldens.push(golden);
        }
    }
    goldens.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(route(&goldens))
}

fn parse_snapshot(stem: &str, text: &str) -> Option<Golden> {
    let name = stem.rsplit("__").next()?.to_owned();
    let mut parts = text.splitn(3, "---\n");
    let _before = parts.next()?;
    let header = parts.next()?;
    let body = parts.next()?.trim();
    let body = if header.contains("expression: line") {
        Body::Line(body.to_owned())
    } else {
        Body::Bytes(
            body.split_whitespace()
                .map(|b| u8::from_str_radix(b, 16).ok())
                .collect::<Option<_>>()?,
        )
    };
    Some(Golden { name, body })
}

/// Which targets a golden seeds. A stream target's input opens with a piece size (and the
/// server's with the side); a stream seed is also offered whole, every golden of its kind one
/// after another.
fn route(goldens: &[Golden]) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut streams: Vec<(&str, Vec<u8>)> = Vec::new();
    let mut add = |target: &str, input: Vec<u8>| out.push((target.to_owned(), input));
    let framed = |bytes: &[u8]| [&[PIECE][..], bytes].concat();
    let unframed = |bytes: &[u8]| bytes.get(4..).map(<[u8]>::to_vec).unwrap_or_default();
    for golden in goldens {
        let kind = golden.name.split('_').next().unwrap_or_default();
        match (&golden.body, kind) {
            (Body::Line(line), _) => add("ctl", format!("{line}\n").into_bytes()),
            (Body::Bytes(bytes), "client") => {
                add("client_msg", framed(bytes));
                add("client_datagram", bytes.clone());
                add("client_datagram", unframed(bytes));
                streams.push(("client_msg", bytes.clone()));
            }
            (Body::Bytes(bytes), "worker") => {
                add("worker_msg", framed(bytes));
                add("term_datagram", bytes.clone());
                streams.push(("worker_msg", bytes.clone()));
            }
            (Body::Bytes(bytes), "server") => {
                add("server_msg", [&[0][..], &framed(bytes)].concat());
                add("server_msg", [&[1][..], &framed(bytes)].concat());
            }
            (Body::Bytes(bytes), "uni" | "conversation") => {
                add("uni_stream", framed(bytes));
                streams.push(("uni_stream", bytes.clone()));
            }
            (Body::Bytes(bytes), "media") => {
                add("media_header", bytes.clone());
                add("cursor", bytes.get(17..).map(<[u8]>::to_vec).unwrap_or_default());
            }
            (Body::Bytes(bytes), _) => add("client_datagram", bytes.clone()),
        }
    }
    for target in ["client_msg", "worker_msg", "uni_stream"] {
        let whole: Vec<u8> =
            streams.iter().filter(|(t, _)| *t == target).flat_map(|(_, b)| b.clone()).collect();
        add(target, framed(&whole));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hex_golden_seeds_its_stream_and_a_line_the_control_socket() {
        let hex = "---\nsource: x\nexpression: hex(&bytes)\n---\n05 00 00 00 00\n01 02 03 04\n";
        let golden = parse_snapshot("golden__golden__client_ping", hex).unwrap();
        assert_eq!(golden.name, "client_ping");
        let line = "---\nsource: x\nexpression: line\n---\n{\"cmd\":\"status\"}\n";
        let ctl = parse_snapshot("golden__ctl__ctl_request_status", line).unwrap();
        let seeds = route(&[golden, ctl]);
        let framed = [PIECE, 5, 0, 0, 0, 0, 1, 2, 3, 4];
        assert!(seeds.contains(&("client_msg".to_owned(), framed.to_vec())), "{seeds:?}");
        assert!(seeds.contains(&("client_datagram".to_owned(), vec![0, 1, 2, 3, 4])), "{seeds:?}");
        assert!(seeds.contains(&("ctl".to_owned(), b"{\"cmd\":\"status\"}\n".to_vec())));
    }

    #[test]
    fn every_golden_parses_and_every_seeded_target_exists() {
        let root = repo_root().unwrap();
        let targets = targets(&root).unwrap();
        let seeds = seeds_from(&root.join(SNAPSHOTS)).unwrap();
        let snapshots = root.join(SNAPSHOTS).read_dir_utf8().unwrap().count();
        assert!(seeds.len() >= snapshots, "{} seeds from {snapshots} snapshots", seeds.len());
        for (target, _) in &seeds {
            assert!(targets.contains(target), "a seed for {target}, which is not a target");
        }
    }

    /// The script matches `slopty_fuzz::terminal::script`: the size, each piece after its
    /// length less one, then join, checkpoint, resize to 40 × 12 and checkpoint.
    #[test]
    fn a_terminal_capture_becomes_its_script_and_every_capture_is_seeded() {
        let capture: Vec<u8> = (0..70).collect();
        let script = terminal_script(&capture);
        assert_eq!(script.get(..3), Some(&[79, 23, 63][..]));
        assert_eq!(script.get(67), Some(&5), "the second piece is 6 bytes long");
        assert!(script.ends_with(&[0xf4, 0xfc, 0xf0, 39, 11, 0xfc]), "{script:?}");
        let root = repo_root().unwrap();
        let captures = root.join(TERMINAL_SEEDS).read_dir_utf8().unwrap().count();
        let seeds = terminal_seeds(&root.join(TERMINAL_SEEDS)).unwrap();
        assert_eq!(seeds.len(), captures + 1, "each capture and all of them together");
        assert!(dictionary(&root, "terminal").is_some(), "the VT dictionary");
    }

    /// The transcript target's input names each file after the first: `\x01` and the
    /// subagent's id, a bare `\x01` for the main transcript, as `slopty_fuzz::transcript::input`
    /// writes it; and every agent target gets the fixtures, each record or payload alone too.
    #[test]
    fn the_agent_targets_are_seeded_from_claude_codes_records() {
        let files = [
            (None, "{\"a\":1}".to_owned()),
            (Some("x1".to_owned()), "{\"b\":2}\n".to_owned()),
            (None, "{\"c\":3}".to_owned()),
        ];
        assert_eq!(
            transcript_input(4, &files),
            b"\x04{\"a\":1}\n\x01x1\n{\"b\":2}\n\x01\n{\"c\":3}\n".to_vec()
        );
        let root = repo_root().unwrap();
        let seeds = agent_seeds(&root.join(AGENT_FIXTURES)).unwrap();
        for target in ["transcript", "hooks", "registry"] {
            let count = seeds.iter().filter(|(t, _)| t == target).count();
            assert!(count >= 4, "{count} {target} seeds");
        }
        if root.join(AGENT_FIXTURES).join("claude-mod/drafts.jsonl").exists() {
            let mut hooks = seeds.iter().filter(|(t, _)| t == "hooks").map(|(_, b)| b);
            let draft = |b: &&Vec<u8>| b.windows(13).any(|w| w == b"\"PromptDraft\"");
            assert!(hooks.any(|b| draft(&b)), "the mod's payloads");
        }
        let targets = targets(&root).unwrap();
        for (target, _) in file_seeds(&root.join(FILE_SEEDS)).unwrap() {
            assert!(targets.contains(&target), "seeds for {target}, which is not a target");
        }
    }

    /// A visual seed leads with what it is, by its extension; a markdown one with its delta.
    #[test]
    fn a_seed_file_becomes_its_targets_input() {
        assert_eq!(seed_inputs("visual", Some("luau"), b"x"), [b"\x00x".to_vec()]);
        assert_eq!(seed_inputs("visual", Some("png"), b"x"), [b"\x01x".to_vec()]);
        assert_eq!(seed_inputs("visual", Some("glb"), b"x"), [b"\x02x".to_vec()]);
        assert_eq!(seed_inputs("visual", Some("glsl"), b"x"), [b"\x03x".to_vec()]);
        assert_eq!(seed_inputs("markdown", Some("md"), b"x"), [b"\x07x".to_vec(), b"@x".to_vec()]);
        assert_eq!(seed_inputs("hooks", None, b"x"), [b"x".to_vec()]);
    }

    #[test]
    fn the_smoke_runs_in_fork_mode_within_its_time() {
        let flags = libfuzzer_flags(30, 0, Utf8Path::new("/a"));
        for wanted in ["-max_total_time=30", "-fork=1", "-ignore_crashes=1", "-artifact_prefix=/a/"]
        {
            assert!(flags.iter().any(|f| f == wanted), "{wanted} in {flags:?}");
        }
    }

    fn run(size: u64, edges: &[u32]) -> Run {
        Run { path: Utf8PathBuf::from(format!("in-{size}")), size, edges: edges.to_vec() }
    }

    /// The control file as libFuzzer 21's merge writes it: an input that crashed has no `FT`,
    /// and a `COV` line is not read for edges.
    #[test]
    fn a_merge_control_file_reads_as_its_runs() {
        let control = "3\n0\n/c/a\n/c/b\n/c/c\nSTARTED 0 5\nFT 0 1 4\nCOV 0 9\nSTARTED 1 7\n\
                       STARTED 2 3\nFT 2\nCOV 2\n";
        let runs = merge_runs(control).unwrap();
        let read: Vec<(&str, u64, &[u32])> =
            runs.iter().map(|r| (r.path.as_str(), r.size, r.edges.as_slice())).collect();
        assert_eq!(read, [("/c/a", 5, &[1, 4][..]), ("/c/c", 3, &[][..])]);
        assert!(merge_runs("2\n0\n/c/a\n").is_err(), "fewer paths than inputs");
        assert!(merge_runs("1\n0\n/c/a\nSTARTED 4 1\n").is_err(), "a run past the inputs");
        assert!(merge_runs("").is_err(), "an empty file");
    }

    /// The cover reaches every edge, cheaply in bytes rather than in inputs: one long input
    /// reaching everything loses to the short ones that reach it all for less.
    #[test]
    fn the_cover_reaches_every_edge_for_the_fewest_bytes() {
        let runs = [
            run(10_000, &[1, 2, 3, 4]),
            run(10, &[1, 2]),
            run(10, &[3]),
            run(10, &[4]),
            run(5, &[]),
        ];
        let cover = cheapest_cover(&runs, u64::MAX);
        let mut inputs = cover.inputs.clone();
        inputs.sort_unstable();
        assert_eq!((inputs, cover.reached, cover.edges), (vec![1, 2, 3], 4, 4));
        // An input that adds nothing is never taken, however cheap.
        let again = cheapest_cover(&[run(1, &[7]), run(1, &[7]), run(100, &[7, 8])], u64::MAX);
        assert_eq!(again, Cover { inputs: vec![0, 2], reached: 2, edges: 2 });
    }

    /// Past the budget, what fits reaches what it can, and the count says what is left out.
    #[test]
    fn the_cover_keeps_within_its_budget() {
        let runs = [run(60, &[1, 2, 3]), run(50, &[4]), run(30, &[5, 6])];
        let cover = cheapest_cover(&runs, 100);
        assert!(cover.inputs.iter().map(|&i| runs[i].size).sum::<u64>() <= 100, "{cover:?}");
        assert_eq!((cover.inputs.clone(), cover.reached, cover.edges), (vec![0, 2], 5, 6));
        assert_eq!(cheapest_cover(&runs, 0), Cover { inputs: vec![], reached: 0, edges: 6 });
        assert_eq!(cheapest_cover(&[], 100), Cover { inputs: vec![], reached: 0, edges: 0 });
    }
}
