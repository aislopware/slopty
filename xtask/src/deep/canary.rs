//! `cargo xtask deep canary <check>`: each deep check run against a defect planted for it, which
//! it must catch. A check that cannot fail says nothing when it passes: from 2026-10-04 to
//! 2026-10-11 no sanitized test ran and the jobs were green (run 38001943825). So every Deep job
//! runs its canary beside its check, through the same environment and command, and the canary
//! passes only when the check failed for the planted reason: the sanitizer's report, Miri's
//! undefined behaviour, the fuzzer's crash input, the mutant that lived. Its log shows the
//! planted failure as the check printed it.
//!
//! The defects live where no real run trips them. Most are tests of the fixture crate
//! (`fixtures/canary`, a workspace of its own) that do nothing unless `SLOPTY_CANARY` names them;
//! the fuzz canary is a crash every target's entry point plants under `SLOPTY_CANARY=fuzz`
//! (`fuzz/src/lib.rs`), the loom canary the audio ring's publishing store weakened to `Relaxed`
//! (`slopty-codec`), and the proptest and gpui-seed canaries one property and one test of the
//! crates those checks run (`slopty-grid`, `slopty-kit`).

use std::fmt::Write as _;

use anyhow::{Context as _, Result, bail, ensure};
use camino::Utf8PathBuf;
use clap::ValueEnum;
use xshell::{Cmd, Shell, cmd};

use super::{Sanitizer, TRIPLES};

/// The check whose canary runs.
#[derive(Clone, Copy, ValueEnum, Debug, PartialEq, Eq)]
pub enum Check {
    /// Miri: a read past the end of a heap allocation.
    Miri,
    /// `AddressSanitizer`: the same read, natively.
    Address,
    /// `ThreadSanitizer`: two threads writing one counter; and a child's `SIGCHLD` heard at once.
    Thread,
    /// `RealtimeSanitizer`: the codec's own probe, an allocation in the render callback.
    Realtime,
    /// The feature powerset: a feature that builds only beside another.
    Features,
    /// Coverage: one function a test calls, which must read covered, and one nothing calls.
    Coverage,
    /// The fuzzers: a crash every target plants for inputs that start with one byte.
    Fuzz,
    /// The vendored suites' runner: a test that fails.
    Noq,
    /// loom: the audio ring publishing its samples with a `Relaxed` store.
    Loom,
    /// `leaks --atExit`: 64 KiB no pointer reaches.
    Leaks,
    /// The property tests: a property that fails past a bound, shrunk to it.
    Proptest,
    /// The GPUI seeds: a test that fails on the second seed of its process.
    GpuiSeeds,
    /// Mutation testing: functions no test checks, whose mutants live.
    Mutants,
    /// The Claude Code mod's weekly check: a relay that marks the newest draft sent when it sent
    /// an older one.
    ClaudeMod,
}

/// The fuzz canary's planted byte: under `SLOPTY_CANARY=fuzz` every target crashes on an input
/// that starts with it (`slopty_fuzz::canary`, `fuzz/src/lib.rs`, which holds the same value).
pub const FUZZ_PLANTED: u8 = 0xc4;

/// The fuzz target the canary runs: a small decoder, whose corpus starts with no planted byte.
const FUZZ_TARGET: &str = "cursor";

pub fn run(sh: &Shell, check: Check, ziggy: bool) -> Result<()> {
    // nextest's setup script runs `$SLOPTY_XTASK spawned-bins`, else `cargo xtask`, which under a
    // sanitizer's `RUSTFLAGS` builds xtask's proc macros with it, which rustc refuses: the setup
    // failed and the realtime canary with it. This binary is built.
    let _xtask = sh.push_env("SLOPTY_XTASK", std::env::current_exe().context("the xtask binary")?);
    let fixture = crate::tools::repo_root()?.join("xtask/src/deep/fixtures/canary");
    let manifest = fixture.join("Cargo.toml");
    let host = TRIPLES[0];
    let caught = match check {
        Check::Miri => {
            super::miri_ready(sh)?;
            let _env = super::miri_env(sh)?;
            let _plant = sh.push_env(PLANT, "overflow");
            fails(
                cmd!(
                    sh,
                    "cargo +nightly miri nextest run --manifest-path {manifest} -E test(=tests::an_out_of_bounds_read)"
                ),
                &["Undefined Behavior", "beyond the end of the allocation"],
            )
        }
        Check::Address => {
            let _env = super::sanitized_env(sh, Sanitizer::Address)?;
            let _plant = sh.push_env(PLANT, "overflow");
            fails(
                cmd!(
                    sh,
                    "cargo +nightly nextest run -Zbuild-std --target {host} --manifest-path {manifest} -E test(=tests::an_out_of_bounds_read)"
                ),
                &["AddressSanitizer: heap-buffer-overflow"],
            )
        }
        Check::Thread => {
            let _env = super::sanitized_env(sh, Sanitizer::Thread)?;
            let heard = passes(
                cmd!(
                    sh,
                    "cargo +nightly nextest run -Zbuild-std --target {host} --manifest-path {manifest} --no-capture -E test(=tests::sigchld_is_heard_at_once)"
                ),
                "SIGCHLD heard in",
            )?;
            let _plant = sh.push_env(PLANT, "race");
            fails(
                cmd!(
                    sh,
                    "cargo +nightly nextest run -Zbuild-std --target {host} --manifest-path {manifest} -E test(=tests::a_data_race)"
                ),
                &["ThreadSanitizer: data race"],
            )
            .map(|race| format!("{race}; and {heard}"))
        }
        Check::Realtime => {
            let _env = super::sanitized_env(sh, Sanitizer::Realtime)?;
            let _bins = sh.push_env(crate::gate::BINS_FRESH, "1");
            passes(
                cmd!(
                    sh,
                    "cargo +nightly nextest run --target {host} -p slopty-codec -E test(=audio::latency_tests::realtime_sanitizer_catches_an_allocation_in_render)"
                ),
                "1 passed",
            )
        }
        Check::Features => {
            let _dir = sh.push_env("CARGO_TARGET_DIR", super::deep_dir("features")?);
            let (cargo, _env) = crate::tools::apple_cargo(sh, false)?;
            let _cargo = (cargo != "cargo").then(|| sh.push_env("CARGO_HACK_CARGO_SRC", cargo));
            fails(
                cmd!(
                    sh,
                    "cargo hack check --each-feature --keep-going --target {host} --manifest-path {manifest}"
                ),
                &["cannot find function `helper`"],
            )
        }
        Check::Coverage => coverage(sh, &fixture),
        Check::Fuzz => fuzz(sh, ziggy),
        Check::Noq => {
            let target = super::deep_dir("noq")?;
            let _plant = sh.push_env(PLANT, "failure");
            fails(
                cmd!(sh, "cargo test --manifest-path {manifest} --locked --target-dir {target}"),
                &["the planted failure"],
            )
        }
        Check::Loom => {
            let _dir = sh.push_env("CARGO_TARGET_DIR", super::deep_dir("loom")?);
            let _flags = sh.push_env("RUSTFLAGS", "--cfg slopty_loom -C target-cpu=apple-m1");
            let _plant = sh.push_env(PLANT, "loom");
            let (package, filter) = *super::LOOM_MODELS.first().context("no loom model")?;
            fails(
                cmd!(sh, "cargo nextest run -p {package} --lib --no-fail-fast -E {filter}"),
                &["is not run"],
            )
        }
        Check::Leaks => leaks(sh, &manifest),
        Check::Proptest => {
            let _bins = sh.push_env(crate::gate::BINS_FRESH, "1");
            let _plant = sh.push_env(PLANT, "proptest");
            let packages = as_the_nightly("slopty-grid");
            fails(
                cmd!(
                    sh,
                    "cargo nextest run {packages...} -E binary(=line_props)&test(/a_planted_bound_is_found_and_shrunk$/)"
                ),
                &["minimal failing input", "n = 1000"],
            )
        }
        Check::GpuiSeeds => {
            let _bins = sh.push_env(crate::gate::BINS_FRESH, "1");
            let _seeds = sh.push_env("ITERATIONS", "50");
            let _plant = sh.push_env(PLANT, "gpui-seeds");
            let packages = as_the_nightly("slopty-kit");
            fails(
                cmd!(
                    sh,
                    "cargo nextest run {packages...} --profile iterations --no-fail-fast -E test(/planted_failure_on_a_later_seed$/)"
                ),
                &["failing seed: 1", "the planted later seed"],
            )
        }
        Check::Mutants => mutants(sh, &fixture),
        Check::ClaudeMod => claude_mod(sh),
    };
    match caught {
        Ok(what) => {
            println!("✓ canary {check:?}: the check caught its planted defect: {what}");
            Ok(())
        }
        Err(why) => bail!("✘ canary {check:?}: the check let its planted defect through: {why:#}"),
    }
}

/// What names the defect a run plants.
const PLANT: &str = "SLOPTY_CANARY";

/// What the mod's flush marks sent once its relay returns, and what the claude-mod canary puts
/// there: the newest draft, though a draft typed during the relay never went (a lost update).
const MOD_SENT: &str = "relayed = text";
const MOD_PLANTED: &str = "relayed = latest";

/// The claude-mod canary: the weekly check (`cargo xtask claude-mod check --deep --force`) on a
/// copy of the mod with [`MOD_PLANTED`], whose seeded model must find a schedule whose last draft
/// never went. The copy is a root of its own whose `target` is the checkout's, so the check runs
/// on the pinned Claude Code, Bun and tools the check itself fetched.
fn claude_mod(sh: &Shell) -> Result<String> {
    let scratch = Utf8PathBuf::try_from(std::env::temp_dir())
        .context("a UTF-8 temporary directory")?
        .join(format!("slopty-claude-mod-canary-{}", std::process::id()));
    let caught = claude_mod_in(sh, &scratch);
    let _gone = std::fs::remove_dir_all(&scratch);
    caught
}

/// [`claude_mod`] with its root at `scratch`.
fn claude_mod_in(sh: &Shell, scratch: &camino::Utf8Path) -> Result<String> {
    let root = crate::tools::repo_root()?;
    let planted = scratch.join(crate::claude_mod::MOD);
    std::fs::create_dir_all(planted.parent().context("the mod in a folder")?)?;
    let from = root.join(crate::claude_mod::MOD);
    cmd!(sh, "cp -R {from} {planted}").quiet().run()?;
    let register = planted.join("hooks/register.ts");
    let source = std::fs::read_to_string(&register).with_context(|| format!("read {register}"))?;
    ensure!(
        source.matches(MOD_SENT).count() == 1,
        "the mod's flush no longer reads `{MOD_SENT}` once: plant the canary anew"
    );
    std::fs::write(&register, source.replacen(MOD_SENT, MOD_PLANTED, 1))?;
    std::fs::create_dir_all(root.join("target"))?;
    std::os::unix::fs::symlink(root.join("target"), scratch.join("target"))?;
    let xtask = std::env::current_exe().context("the xtask binary")?;
    let _root = sh.push_env(crate::tools::REPO_ROOT, scratch);
    fails(cmd!(sh, "{xtask} claude-mod check --deep --force"), &["(fail) schedule "])
}

/// `package` selected as `cargo xtask nightly` selects its packages: with the workspace hack on a
/// Mac, so the canary builds the units its check built, and without it elsewhere.
fn as_the_nightly(package: &str) -> Vec<String> {
    let mut packages = vec!["-p".to_owned(), package.to_owned()];
    if cfg!(target_os = "macos") {
        packages.extend(["-p".to_owned(), crate::tools::WORKSPACE_HACK.to_owned()]);
    }
    packages
}

/// Run `command`, show what it printed, and require that it failed and printed each of
/// `signatures`; the first line that holds the first signature.
fn fails(command: Cmd<'_>, signatures: &[&str]) -> Result<String> {
    let (ok, text) = shown(command)?;
    ensure!(!ok, "it passed");
    signed(&text, signatures)
}

/// Run `command`, show what it printed, and require that it passed and printed `signature`.
fn passes(command: Cmd<'_>, signature: &str) -> Result<String> {
    let (ok, text) = shown(command)?;
    ensure!(ok, "it failed");
    signed(&text, &[signature])
}

/// The first line of `text` holding `signatures[0]`, once every one is in it.
fn signed(text: &str, signatures: &[&str]) -> Result<String> {
    let missing: Vec<&&str> = signatures.iter().filter(|s| !text.contains(**s)).collect();
    ensure!(missing.is_empty(), "not for the planted reason: no {missing:?} in what it printed");
    let first = signatures.first().copied().unwrap_or_default();
    Ok(text.lines().find(|l| l.contains(first)).unwrap_or(first).trim().to_owned())
}

/// Run `command` as a step, its output captured and then printed under the step's title; whether
/// it passed, and the output without its escape sequences. A runner's tools print in colour
/// (`CARGO_TERM_COLOR=always`), and nextest's `1 passed` came as `1`, a colour code, and
/// `passed`, so the realtime canary found no `1 passed` in a run that passed (Deep run
/// 40989163091).
fn shown(command: Cmd<'_>) -> Result<(bool, String)> {
    let title = command.to_string();
    println!("▶ canary: {title}");
    let out = command.ignore_status().output().with_context(|| format!("starting {title}"))?;
    let text =
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    println!("{text}");
    println!("  canary run {}: {title}", if out.status.success() { "passed" } else { "failed" });
    Ok((out.status.success(), crate::claude_mod::strip_escapes(&text)))
}

/// The coverage canary: the fixture's tests under the coverage run's environment, then its report,
/// where the line of `covered` must read covered and the line of `uncovered` not.
fn coverage(sh: &Shell, fixture: &camino::Utf8Path) -> Result<String> {
    let _dir = sh.push_dir(fixture);
    let target = super::deep_dir("cov-canary")?;
    let _target = sh.push_env("CARGO_TARGET_DIR", &target);
    let _cov = super::llvm_cov_env(sh)?;
    let (ok, _) = shown(cmd!(sh, "cargo nextest run"))?;
    ensure!(ok, "the fixture's tests failed under coverage");
    let lcov = target.join("lcov.info");
    let (ok, _) = shown(cmd!(sh, "cargo llvm-cov report --lcov --output-path {lcov}"))?;
    ensure!(ok, "no report");
    let source = std::fs::read_to_string(fixture.join("src/lib.rs"))?;
    let report = std::fs::read_to_string(&lcov).with_context(|| format!("reading {lcov}"))?;
    let hits = |name: &str| -> Result<u64> {
        let line = source
            .lines()
            .position(|l| l.starts_with(&format!("pub fn {name}()")))
            .with_context(|| format!("no `pub fn {name}` in the fixture"))?;
        let line = line.saturating_add(1);
        lcov_hits(&report, "src/lib.rs", line)
            .with_context(|| format!("the report has no line {line} ({name})"))
    };
    let (covered, uncovered) = (hits("covered")?, hits("uncovered")?);
    ensure!(covered > 0, "the line a test runs reads uncovered");
    ensure!(uncovered == 0, "the line nothing runs reads covered ({uncovered} hits)");
    Ok(format!("`covered` ran {covered} time(s) and `uncovered` none"))
}

/// The hits lcov `report` gives line `line` of the file whose path ends with `file`.
fn lcov_hits(report: &str, file: &str, line: usize) -> Option<u64> {
    let mut inside = false;
    for entry in report.lines() {
        if let Some(path) = entry.strip_prefix("SF:") {
            inside = path.ends_with(file);
        } else if inside && let Some(da) = entry.strip_prefix("DA:") {
            let mut fields = da.split(',');
            if fields.next()?.parse::<usize>().ok()? == line {
                return fields.next()?.parse().ok();
            }
        }
    }
    None
}

/// The fuzz canary: `cargo xtask fuzz` on one target, as the Deep job runs it, under the planted
/// crash; it must fail on inputs that start with [`FUZZ_PLANTED`] and on nothing else. It runs as
/// a process of its own: the fuzzers start through `std::process`, which hands them this
/// process's environment and not the shell's, so the plant pushed on the shell never reached
/// them and the canary found nothing.
fn fuzz(sh: &Shell, ziggy: bool) -> Result<String> {
    let root = crate::tools::repo_root()?;
    let artifacts = root.join("target/fuzz/artifacts").join(FUZZ_TARGET);
    let started = std::time::SystemTime::now();
    let xtask = std::env::current_exe().context("the xtask binary")?;
    let afl_too = ziggy.then_some("--ziggy");
    let (ok, failed) = {
        let _plant = sh.push_env(PLANT, "fuzz");
        shown(cmd!(sh, "{xtask} fuzz {FUZZ_TARGET} --time 20 --jobs 1 {afl_too...}"))?
    };
    ensure!(!ok, "fuzzing {FUZZ_TARGET} found nothing");
    let failed = failed.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or_default().trim();
    println!("  the fuzz run failed as planted: {failed}");
    let found: Vec<Utf8PathBuf> = artifacts
        .read_dir_utf8()
        .with_context(|| format!("reading {artifacts}"))?
        .filter_map(Result::ok)
        .filter(|e| e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t >= started))
        .map(camino::Utf8DirEntry::into_path)
        .collect();
    ensure!(!found.is_empty(), "it failed with no crash input: {failed}");
    let other: Vec<&Utf8PathBuf> = found
        .iter()
        .filter(|path| !std::fs::read(path).is_ok_and(|bytes| bytes.first() == Some(&FUZZ_PLANTED)))
        .collect();
    ensure!(other.is_empty(), "inputs that are not the planted crash: {other:?}");
    let afl =
        found.iter().filter(|p| p.file_name().is_some_and(|n| n.starts_with("crash-afl-"))).count();
    ensure!(!ziggy || afl > 0, "AFL++ found no planted crash");
    Ok(format!("{} planted crash input(s), {afl} of them AFL++'s", found.len()))
}

/// The leaks canary: the fixture's test binary under `leaks --atExit` as the leaks check runs each,
/// with the leak planted.
fn leaks(sh: &Shell, manifest: &camino::Utf8Path) -> Result<String> {
    let _dir = sh.push_env("CARGO_TARGET_DIR", super::deep_dir("leaks")?);
    let built = cmd!(sh, "cargo test --no-run --message-format=json --manifest-path {manifest}")
        .ignore_stderr()
        .read()
        .context("building the fixture's tests")?;
    let binaries = super::test_binaries(&built);
    let [binary] = binaries.as_slice() else { bail!("not one test binary: {binaries:?}") };
    let reports = super::deep_dir("leaks")?.join("reports");
    std::fs::create_dir_all(&reports)?;
    let report = reports.join("canary.txt");
    let ran = super::under_leaks(binary, report.as_std_path(), false, &[(PLANT, "leak")])?;
    let text = std::fs::read_to_string(&report).unwrap_or_default();
    println!("{text}");
    let super::Ran::Exited(code) = ran else { bail!("still running after its bound") };
    match super::leaks_verdict(&text, code) {
        Ok(clean) => bail!("leaks found nothing: {clean}"),
        Err(why) if why.starts_with("Process ") && !why.contains(": 0 leaks") => Ok(why),
        Err(why) => bail!("not for the planted reason: {why}"),
    }
}

/// The mutants canary: every function of the fixture mutated through the same run as a diff's,
/// which must say the unchecked ones lived and the checked one was caught.
fn mutants(sh: &Shell, fixture: &camino::Utf8Path) -> Result<String> {
    let diff = canary_diff(fixture)?;
    let out = crate::tools::repo_root()?.join("target/deep/mutants/canary");
    let opts = super::mutants::Opts {
        package: Vec::new(),
        rotate: false,
        in_diff: None,
        since: None,
        tier: Vec::new(),
        timeout: 60,
        shard: None,
        jobs: 1,
        cap: None,
        baseline: super::mutants::Baseline::Skip,
    };
    let scope = super::mutants::Scope {
        dir: fixture,
        packages: &["slopty-deep-canary".to_owned()],
        diff: Some(&diff),
        out: out.as_str(),
    };
    let ended = super::mutants::mutate(sh, &scope, &opts, None)?;
    let outcomes = super::mutants::Outcomes::read(&out);
    println!(
        "missed:\n  {}\ncaught:\n  {}",
        outcomes.missed.join("\n  "),
        outcomes.caught.join("\n  ")
    );
    ensure!(
        ended == super::mutants::Ended::Missed,
        "cargo mutants ended {ended:?}, not on a mutant that lived"
    );
    let lived = |name: &str, list: &[String]| list.iter().any(|l| l.contains(name));
    ensure!(lived("doubled", &outcomes.missed), "no mutant of `doubled` lived");
    ensure!(lived("covered", &outcomes.caught), "no mutant of `covered` was caught");
    Ok(format!("{} lived, {} caught", outcomes.missed.len(), outcomes.caught.len()))
}

/// The mutants canary's tree and diff: the fixture crate, and a diff that adds all of its
/// `src/lib.rs`, so every function in it is mutated.
fn canary_diff(fixture: &camino::Utf8Path) -> Result<Utf8PathBuf> {
    let lib = std::fs::read_to_string(fixture.join("src/lib.rs"))?;
    let lines: Vec<&str> = lib.lines().collect();
    let mut diff = format!(
        "diff --git a/src/lib.rs b/src/lib.rs\nnew file mode 100644\n--- /dev/null\n+++ b/src/lib.rs\n@@ -0,0 +1,{} @@\n",
        lines.len()
    );
    for line in lines {
        let _line = writeln!(diff, "+{line}");
    }
    let out = crate::tools::repo_root()?.join("target/deep/mutants/canary");
    std::fs::create_dir_all(&out)?;
    let path = out.join("git.diff");
    std::fs::write(&path, diff)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_canary_passes_only_on_a_failure_for_the_planted_reason() {
        signed("x\nThreadSanitizer: data race (pid 1)\n", &["data race"]).unwrap();
        let wrong = signed("thread panicked: a timeout\n", &["ThreadSanitizer: data race"]);
        assert!(wrong.unwrap_err().to_string().contains("not for the planted reason"));
        // nextest's summary on a runner, which prints in colour.
        let coloured = "1 test run: \u{1b}[1m1\u{1b}[0m \u{1b}[32;1mpassed\u{1b}[0m";
        signed(&crate::claude_mod::strip_escapes(coloured), &["1 passed"]).unwrap();
        assert_eq!(
            signed(
                "a\n  minimal failing input: n = 1000\n",
                &["minimal failing input", "n = 1000"]
            )
            .unwrap(),
            "minimal failing input: n = 1000"
        );
    }

    #[test]
    fn the_coverage_canary_reads_a_line_s_hits_from_its_file_alone() {
        let report = "SF:/r/other/src/lib.rs\nDA:19,7\nend_of_record\n\
                      SF:/r/fixtures/canary/src/lib.rs\nDA:19,1\nDA:25,0\nend_of_record\n";
        assert_eq!(lcov_hits(report, "canary/src/lib.rs", 19), Some(1));
        assert_eq!(lcov_hits(report, "canary/src/lib.rs", 25), Some(0));
        assert_eq!(lcov_hits(report, "canary/src/lib.rs", 30), None);
    }

    #[test]
    fn the_fixture_plants_every_defect_a_canary_names() {
        let root = crate::tools::repo_root().unwrap();
        let lib = std::fs::read_to_string(root.join("xtask/src/deep/fixtures/canary/src/lib.rs"))
            .unwrap();
        for (plant, test) in [
            ("overflow", "fn an_out_of_bounds_read"),
            ("race", "fn a_data_race"),
            ("leak", "fn a_leak"),
            ("failure", "fn a_failure"),
        ] {
            assert!(
                lib.contains(test) && lib.contains(&format!("planted(\"{plant}\")")),
                "{plant}"
            );
        }
        assert!(lib.contains("fn sigchld_is_heard_at_once"));
        let fuzz = std::fs::read_to_string(root.join("fuzz/src/lib.rs")).unwrap();
        assert!(
            fuzz.contains(&format!("pub const PLANTED: u8 = 0x{FUZZ_PLANTED:x};")),
            "fuzz/src/lib.rs plants another byte"
        );
        for corpus in std::fs::read_dir(root.join("fuzz/corpus").join(FUZZ_TARGET)).unwrap() {
            let bytes = std::fs::read(corpus.unwrap().path()).unwrap();
            assert_ne!(bytes.first(), Some(&FUZZ_PLANTED), "a committed input starts planted");
        }
    }
}
