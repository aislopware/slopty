//! `cargo xtask nightly`: the heavy lanes, run unattended on this Mac, one after another.
//!
//! Each check runs at a lowered priority (`nice -n 19` and four cores on this Mac, `nice -n 10` on
//! a CI runner) with its output in `target/nightly/<date>/<check>.log`, and
//! writes `<check>.json` beside it (what ran, how long, passed, failed or skipped and why). A
//! `summary.json` lists them all, and the command fails when any check failed. A check whose
//! tool is missing (the nightly toolchain, `cargo-llvm-cov`, `cargo-hack`) is skipped, not failed,
//! and says so.
//!
//! The checks, in order:
//! - `soak`: `cargo xtask soak` for `--soak-minutes`;
//! - `bench`: `cargo xtask bench --wall`, which also keeps the wall-time trend;
//! - `proptest`: the property tests at `PROPTEST_CASES` cases each;
//! - `gpui-iterations`: `slopty-ui`'s tests with `ITERATIONS`, so every `#[gpui::test]` runs under
//!   that many scheduler seeds (`SEED=<n>` replays a failure);
//! - `miri`, `sanitize-address`, `sanitize-thread`, `sanitize-realtime`, `coverage`, `features`,
//!   `fuzz`: `cargo xtask deep` (`fuzz` runs every fuzz target for 30 s).
//!
//! The bug-catching checks run every night on GitHub Actions (`.github/workflows/deep.yml`), not
//! here: someone works on this Mac, over Parsec, while other sessions gate on it. A full run here
//! is refused unless `--all-here` asks for it; `--only <check>` runs one. `uninstall` removes the
//! `LaunchAgent` an earlier version installed. Nothing here plays a sound or draws on the screen.

use std::fs::File;
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{Context as _, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use clap::{Args, Subcommand};
use serde_json::json;
use xshell::{Shell, cmd};

use crate::tools::{WORKSPACE_HACK, has, repo_root};

/// The `LaunchAgent`'s label, under the prefix the dev daemons are signed with.
const LABEL: &str = "dev.aislopware.slopty.nightly";

/// The crates with property tests, and the filter that picks those tests out.
const PROPTEST_PACKAGES: [&str; 3] = ["slopty-proto", "slopty-media", "slopty-grid"];
const PROPTEST_FILTER: &str = "binary(/_props$/) or (package(slopty-media) and binary(=pipeline))";

#[derive(Subcommand, Debug)]
pub enum NightlyCmd {
    /// Run the checks now (the default).
    Run(NightlyOpts),
    /// Unload and remove the `LaunchAgent` that ran `cargo xtask nightly` at 03:00.
    Uninstall,
}

#[derive(Args, Debug, Clone)]
pub struct NightlyOpts {
    /// Only these checks (repeat the flag).
    #[arg(long)]
    pub only: Vec<String>,
    /// Run every check on this Mac, which a run without `--only` otherwise refuses outside CI.
    #[arg(long)]
    pub all_here: bool,
    /// Not these checks (repeat the flag).
    #[arg(long)]
    pub skip: Vec<String>,
    /// How long the soak runs.
    #[arg(long, default_value_t = 20)]
    pub soak_minutes: u64,
    /// Cases per property test.
    #[arg(long, default_value_t = 4_096)]
    pub proptest_cases: u32,
    /// Scheduler seeds per `#[gpui::test]`.
    #[arg(long, default_value_t = 50)]
    pub iterations: u32,
}

impl Default for NightlyOpts {
    fn default() -> Self {
        Self {
            only: Vec::new(),
            all_here: false,
            skip: Vec::new(),
            soak_minutes: 20,
            proptest_cases: 4_096,
            iterations: 50,
        }
    }
}

/// One check: its name, the command, extra environment, and what it needs to run at all.
struct Check {
    name: &'static str,
    program: String,
    args: Vec<String>,
    env: Vec<(&'static str, String)>,
    needs: Option<Need>,
}

#[derive(Clone, Copy)]
enum Need {
    Nightly,
    Tool(&'static str),
    /// The nightly toolchain and a cargo subcommand built for it.
    NightlyTool(&'static str),
}

pub fn run(sh: &Shell, cmd: Option<&NightlyCmd>) -> Result<()> {
    match cmd {
        None => nightly(sh, &NightlyOpts::default()),
        Some(NightlyCmd::Run(opts)) => nightly(sh, opts),
        Some(NightlyCmd::Uninstall) => uninstall(sh),
    }
}

fn checks(opts: &NightlyOpts, dir: &Utf8Path, xtask: &str) -> Vec<Check> {
    let own = |args: &[&str]| -> Vec<String> { args.iter().map(|a| (*a).to_owned()).collect() };
    let selected = |packages: &[&str]| -> Vec<String> {
        packages
            .iter()
            .chain([&WORKSPACE_HACK])
            .flat_map(|p| ["-p".to_owned(), (*p).to_owned()])
            .collect()
    };
    let deep = |name: &'static str, args: &[&str], needs: Option<Need>| Check {
        name,
        program: xtask.to_owned(),
        args: [&["deep"][..], args].concat().into_iter().map(str::to_owned).collect(),
        env: Vec::new(),
        needs,
    };
    let soak_seconds = opts.soak_minutes.saturating_mul(60).to_string();
    vec![
        Check {
            name: "soak",
            program: xtask.to_owned(),
            args: own(&["soak", "--seconds", &soak_seconds, "--out", dir.join("soak").as_str()]),
            env: Vec::new(),
            needs: None,
        },
        Check {
            name: "bench",
            program: xtask.to_owned(),
            args: own(&["bench", "--wall", "--out", dir.join("bench.jsonl").as_str()]),
            env: Vec::new(),
            needs: None,
        },
        Check {
            name: "proptest",
            program: "cargo".to_owned(),
            args: [
                own(&["nextest", "run"]),
                selected(&PROPTEST_PACKAGES),
                own(&["-E", PROPTEST_FILTER, "--no-fail-fast"]),
            ]
            .concat(),
            env: vec![("PROPTEST_CASES", opts.proptest_cases.to_string())],
            needs: None,
        },
        Check {
            name: "gpui-iterations",
            program: "cargo".to_owned(),
            args: [own(&["nextest", "run"]), selected(&["slopty-ui"]), own(&["--no-fail-fast"])]
                .concat(),
            env: vec![("ITERATIONS", opts.iterations.to_string())],
            needs: None,
        },
        Check {
            name: "app-soak",
            program: "cargo".to_owned(),
            args: own(&[
                "nextest",
                "run",
                "-p",
                "slopty-ui",
                "-E",
                "test(every_tile_kind_opened_and_closed_many_times_leaves_nothing)",
            ]),
            env: vec![("SLOPTY_SOAK_CYCLES", "1000".to_owned())],
            needs: None,
        },
        Check {
            name: "tailnet",
            program: xtask.to_owned(),
            args: own(&["tailnet", "test", "--no-fail-fast"]),
            env: Vec::new(),
            // `tailscaled` is built from source once; Headscale is a pinned release binary.
            needs: Some(Need::Tool("go")),
        },
        deep("miri", &["miri"], Some(Need::Nightly)),
        deep("sanitize-address", &["sanitize", "address"], Some(Need::Nightly)),
        deep("sanitize-thread", &["sanitize", "thread"], Some(Need::Nightly)),
        deep("sanitize-realtime", &["sanitize", "realtime"], Some(Need::Nightly)),
        deep("coverage", &["coverage"], Some(Need::Tool("cargo-llvm-cov"))),
        deep("features", &["features"], Some(Need::Tool("cargo-hack"))),
        deep("fuzz", &["fuzz"], Some(Need::NightlyTool("cargo-fuzz"))),
        deep("loom", &["loom"], None),
        deep("leaks", &["leaks"], Some(Need::Tool("leaks"))),
        deep("metal", &["metal"], Some(Need::Tool("xcodebuild"))),
    ]
}

/// Why `need` is not met here, if it is not.
fn unmet(sh: &Shell, need: Option<Need>) -> Option<String> {
    match need? {
        Need::Nightly => {
            let listed = cmd!(sh, "rustup toolchain list").quiet().read().unwrap_or_default();
            (!listed.lines().any(|l| l.starts_with("nightly")))
                .then(|| "no nightly toolchain (`rustup toolchain install nightly`)".to_owned())
        }
        Need::Tool(tool) => (!has(sh, tool)).then(|| format!("`{tool}` is not installed")),
        Need::NightlyTool(tool) => {
            unmet(sh, Some(Need::Nightly)).or_else(|| unmet(sh, Some(Need::Tool(tool))))
        }
    }
}

fn nightly(sh: &Shell, opts: &NightlyOpts) -> Result<()> {
    if let Some(refusal) = refused(opts, std::env::var_os("CI").is_some()) {
        bail!("{refusal}");
    }
    let root = repo_root()?;
    let date = cmd!(sh, "date +%Y-%m-%d").quiet().read()?;
    let dir = root.join("target/nightly").join(&date);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {dir}"))?;
    let xtask = std::env::current_exe().context("this xtask's path")?;
    let xtask =
        Utf8PathBuf::from_path_buf(xtask).map_err(|p| anyhow::anyhow!("{}", p.display()))?;
    let rev = cmd!(sh, "git rev-parse --short HEAD").quiet().read().unwrap_or_default();
    let all = checks(opts, &dir, xtask.as_str());
    let unknown: Vec<&String> =
        opts.only.iter().chain(&opts.skip).filter(|n| !all.iter().any(|c| c.name == *n)).collect();
    if !unknown.is_empty() {
        let names: Vec<&str> = all.iter().map(|c| c.name).collect();
        bail!("no check named {unknown:?}; the checks are {}", names.join(", "));
    }
    let started = Instant::now();
    let mut results = Vec::new();
    for check in all {
        let wanted = (opts.only.is_empty() || opts.only.iter().any(|o| o == check.name))
            && !opts.skip.iter().any(|s| s == check.name);
        if !wanted {
            continue;
        }
        let result = run_check(sh, &check, &dir)?;
        println!(
            "{} {} ({}s){}",
            match result.get("status").and_then(serde_json::Value::as_str) {
                Some("passed") => "✓",
                Some("skipped") => "–",
                _ => "✘",
            },
            check.name,
            result.get("seconds").and_then(serde_json::Value::as_u64).unwrap_or(0),
            result
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .map_or_else(String::new, |r| format!(": {r}")),
        );
        results.push(result);
    }
    let failed: Vec<&str> = results
        .iter()
        .filter(|r| r.get("status").is_some_and(|s| s == "failed"))
        .filter_map(|r| r.get("check").and_then(serde_json::Value::as_str))
        .collect();
    let summary = json!({
        "date": date,
        "rev": rev,
        "seconds": started.elapsed().as_secs(),
        "checks": results,
        "failed": failed,
    });
    std::fs::write(dir.join("summary.json"), serde_json::to_string_pretty(&summary)?)?;
    if !failed.is_empty() {
        bail!("nightly: {} failed ({dir})", failed.join(", "));
    }
    println!("✔ nightly passed ({dir})");
    Ok(())
}

/// Why a run with `opts` does not start here: every check at once, on a machine that is not a CI
/// runner, without `--all-here`.
fn refused(opts: &NightlyOpts, ci: bool) -> Option<&'static str> {
    (!ci && opts.only.is_empty() && !opts.all_here).then_some(
        "the deep checks run on GitHub Actions (`.github/workflows/deep.yml`, `gh workflow run \
         deep.yml`); run one here with `--only <check>`, or all of them with `--all-here`",
    )
}

/// Run `check` under `nice`, its output in `<check>.log`; its result, also in `<check>.json`.
fn run_check(sh: &Shell, check: &Check, dir: &Utf8Path) -> Result<serde_json::Value> {
    let log = dir.join(format!("{}.log", check.name));
    let command = format!("{} {}", check.program, check.args.join(" "));
    let env: serde_json::Map<String, serde_json::Value> =
        check.env.iter().map(|(k, v)| ((*k).to_owned(), json!(v))).collect();
    let started = Instant::now();
    let result = if let Some(reason) = unmet(sh, check.needs) {
        json!({ "check": check.name, "status": "skipped", "reason": reason, "command": command,
                "env": env, "seconds": 0 })
    } else {
        println!("▶ {}", check.name);
        let out = File::create(&log).with_context(|| format!("creating {log}"))?;
        let share = Share::here();
        let status = Command::new("nice")
            .args(["-n", share.niceness, &check.program])
            .args(&check.args)
            .envs(share.env.iter().copied())
            .envs(check.env.iter().map(|(k, v)| (*k, v.as_str())))
            .current_dir(repo_root()?)
            .stdin(Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out)
            .status()
            .with_context(|| format!("starting {command}"))?;
        json!({
            "check": check.name,
            "status": if status.success() { "passed" } else { "failed" },
            "exit_code": status.code(),
            "command": command,
            "env": env,
            "seconds": started.elapsed().as_secs(),
            "log": log.as_str(),
        })
    };
    let path = dir.join(format!("{}.json", check.name));
    std::fs::write(&path, serde_json::to_string_pretty(&result)?)
        .with_context(|| format!("writing {path}"))?;
    Ok(result)
}

/// How much of the machine a check may take. On a CI runner, which is the check's alone, all of
/// it at a lowered priority. On this Mac, where someone works (over Parsec) and other sessions
/// gate beside it, the lowest priority and four cores: cargo's jobs and nextest's test threads
/// capped (MEASUREMENTS 2026-10-01, "a nightly that shares the Mac").
struct Share {
    niceness: &'static str,
    env: &'static [(&'static str, &'static str)],
}

impl Share {
    fn here() -> Self {
        Self::on_ci(std::env::var_os("CI").is_some())
    }

    const fn on_ci(ci: bool) -> Self {
        if ci {
            Self { niceness: "10", env: &[] }
        } else {
            Self {
                niceness: "19",
                env: &[("CARGO_BUILD_JOBS", "4"), ("NEXTEST_TEST_THREADS", "4")],
            }
        }
    }
}

fn plist_path() -> Result<Utf8PathBuf> {
    let home = std::env::var("HOME").context("HOME")?;
    Ok(Utf8PathBuf::from(home).join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
}

fn uninstall(sh: &Shell) -> Result<()> {
    let file = plist_path()?;
    let uid = cmd!(sh, "id -u").quiet().read()?;
    let domain = format!("gui/{uid}");
    let _unloaded = cmd!(sh, "launchctl bootout {domain}/{LABEL}").quiet().ignore_stderr().run();
    if file.exists() {
        std::fs::remove_file(&file).with_context(|| format!("removing {file}"))?;
    }
    println!("✔ {LABEL} removed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full run on this Mac needs `--all-here`; one check, or a CI runner, does not.
    #[test]
    fn a_full_local_run_is_refused_without_its_flag() {
        let full = NightlyOpts::default();
        assert!(refused(&full, false).is_some_and(|r| r.contains("deep.yml")));
        assert!(refused(&full, true).is_none(), "a runner runs everything");
        let one = NightlyOpts { only: vec!["miri".to_owned()], ..NightlyOpts::default() };
        assert!(refused(&one, false).is_none());
        let asked = NightlyOpts { all_here: true, ..NightlyOpts::default() };
        assert!(refused(&asked, false).is_none());
    }

    #[test]
    fn every_check_is_named_once_and_the_deep_ones_say_what_they_need() {
        let checks = checks(&NightlyOpts::default(), Utf8Path::new("/tmp/n"), "xtask");
        let mut names: Vec<&str> = checks.iter().map(|c| c.name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "{names:?}");
        let proptest = checks.iter().find(|c| c.name == "proptest").unwrap();
        assert_eq!(proptest.env, [("PROPTEST_CASES", "4096".to_owned())]);
        let soak = checks.iter().find(|c| c.name == "soak").unwrap();
        assert!(soak.args.windows(2).any(|w| w == ["--seconds", "1200"]), "{:?}", soak.args);
        assert!(
            checks
                .iter()
                .filter(|c| c.args.first().is_some_and(|a| a == "deep"))
                // loom is a crate the check builds on stable: there is nothing to install.
                .filter(|c| c.name != "loom")
                .all(|c| c.needs.is_some())
        );
    }

    /// On this Mac a check yields to everything and takes four cores; on a runner, the runner.
    #[test]
    fn a_local_run_yields_and_a_runner_does_not_cap() {
        let here = Share::on_ci(false);
        assert_eq!(here.niceness, "19");
        assert!(here.env.contains(&("CARGO_BUILD_JOBS", "4")), "{:?}", here.env);
        assert!(here.env.contains(&("NEXTEST_TEST_THREADS", "4")), "{:?}", here.env);
        let runner = Share::on_ci(true);
        assert_eq!((runner.niceness, runner.env.len()), ("10", 0));
    }
}
