//! `cargo xtask fuzz`: libFuzzer over every decoder that faces a peer (`fuzz/`).
//!
//! The targets are built once with cargo-fuzz (nightly, `AddressSanitizer`, debug assertions) and
//! each binary then runs for `--time` seconds under `nice`, in fork mode so one crash does not
//! end the run. Each target's corpus lives under `target/fuzz/corpus/<target>` and grows from
//! run to run; the seeds beside it are rewritten every run from the wire goldens
//! (`crates/slopty-proto/tests/snapshots`), so a target starts from real messages rather than
//! from nothing. A crash, a leak, an out-of-memory or a timeout lands under
//! `target/fuzz/artifacts/<target>/` and fails the run.
//!
//! `--keep <artifact>` minimises one of those inputs and files it under
//! `fuzz/regressions/<target>/`, where the fuzz crate's `tests/regressions.rs` replays it on an
//! ordinary build: `--replay` runs exactly that, with no nightly and no instrumentation.

use std::fmt::Write as _;
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime};

use anyhow::{Context as _, Result, bail, ensure};
use camino::{Utf8Path, Utf8PathBuf};
use clap::Args;
use xshell::{Shell, cmd};

use crate::tools::{TRIPLES, quiet_step, repo_root};

/// Where the goldens every seed corpus starts from live.
const SNAPSHOTS: &str = "crates/slopty-proto/tests/snapshots";

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
    /// Only replay `fuzz/regressions/` on an ordinary build.
    #[arg(long, conflicts_with_all = ["keep", "target"])]
    pub replay: bool,
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

/// The regression inputs through their targets, on a plain build of the fuzz crate.
fn replay(sh: &Shell) -> Result<()> {
    let _dir = sh.push_env("CARGO_TARGET_DIR", "target/fuzz/test");
    quiet_step(
        "fuzz regressions",
        cmd!(sh, "cargo test --manifest-path fuzz/Cargo.toml --locked --tests"),
    )
}

/// The nightly toolchain and the flags every cargo-fuzz call shares: the host triple named
/// (cargo-fuzz otherwise takes the triple it was itself built for, x86-64 when it came as a
/// Rosetta binary), debug assertions on, and a target dir of its own.
fn cargo_fuzz<'a>(sh: &'a Shell, verb: &str) -> xshell::Cmd<'a> {
    let host = TRIPLES[0];
    cmd!(
        sh,
        "cargo +nightly fuzz {verb} --debug-assertions --target {host} --target-dir target/fuzz/build"
    )
    .env("RUSTC_WRAPPER", "")
}

fn build(sh: &Shell) -> Result<()> {
    quiet_step("cargo fuzz build", cargo_fuzz(sh, "build"))
}

fn binary(root: &Utf8Path, target: &str) -> Utf8PathBuf {
    root.join("target/fuzz/build").join(TRIPLES[0]).join("release").join(target)
}

/// Run one target for `seconds`; the artifacts it left.
fn fuzz(root: &Utf8Path, target: &str, seconds: u64, jobs: u32) -> Result<Vec<String>> {
    let corpus = root.join("target/fuzz/corpus").join(target);
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
        .arg(&corpus)
        .arg(&seeds)
        .args(libfuzzer_flags(seconds, jobs, &artifacts))
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
        "-rss_limit_mb=2048".to_owned(),
        "-max_len=65536".to_owned(),
        format!("-artifact_prefix={artifacts}/"),
        "-print_final_stats=1".to_owned(),
    ]
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

/// Seed corpora from the wire goldens: each hex snapshot as the stream or datagram it is, the
/// JSON ones as control-socket lines. The fuzzer grows its own corpus beside them.
fn seed(root: &Utf8Path, chosen: &[String]) -> Result<()> {
    let seeds = seeds_from(&root.join(SNAPSHOTS))?;
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

    #[test]
    fn the_smoke_runs_in_fork_mode_within_its_time() {
        let flags = libfuzzer_flags(30, 0, Utf8Path::new("/a"));
        for wanted in ["-max_total_time=30", "-fork=1", "-ignore_crashes=1", "-artifact_prefix=/a/"]
        {
            assert!(flags.iter().any(|f| f == wanted), "{wanted} in {flags:?}");
        }
    }
}
