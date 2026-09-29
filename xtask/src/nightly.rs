//! `cargo xtask nightly`: the heavy lanes, run unattended on this Mac, one after another.
//!
//! Each check runs under `nice -n 10` with its output in `target/nightly/<date>/<check>.log`, and
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
//! - `miri`, `sanitize-address`, `sanitize-thread`, `coverage`, `features`, `fuzz`: `cargo xtask
//!   deep` (`fuzz` runs every fuzz target for 30 s).
//!
//! `cargo xtask nightly install` writes a `LaunchAgent` that runs it at 03:00, at background
//! priority; `uninstall` removes it. Nothing here plays a sound or draws on the screen.

use std::fmt::Write as _;
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
    /// Write and load a `LaunchAgent` that runs `cargo xtask nightly` at 03:00.
    Install,
    /// Unload and remove that `LaunchAgent`.
    Uninstall,
}

#[derive(Args, Debug, Clone)]
pub struct NightlyOpts {
    /// Only these checks (repeat the flag).
    #[arg(long)]
    pub only: Vec<String>,
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
        Some(NightlyCmd::Install) => install(sh),
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
        deep("miri", &["miri"], Some(Need::Nightly)),
        deep("sanitize-address", &["sanitize", "address"], Some(Need::Nightly)),
        deep("sanitize-thread", &["sanitize", "thread"], Some(Need::Nightly)),
        deep("coverage", &["coverage"], Some(Need::Tool("cargo-llvm-cov"))),
        deep("features", &["features"], Some(Need::Tool("cargo-hack"))),
        deep("fuzz", &["fuzz"], Some(Need::NightlyTool("cargo-fuzz"))),
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
        let status = Command::new("nice")
            .args(["-n", "10", &check.program])
            .args(&check.args)
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

fn plist_path() -> Result<Utf8PathBuf> {
    let home = std::env::var("HOME").context("HOME")?;
    Ok(Utf8PathBuf::from(home).join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
}

/// `s` as XML character data.
fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The `LaunchAgent` that runs `cargo xtask nightly` in `root` at 03:00 with `path` for `PATH`.
fn plist(cargo: &str, root: &Utf8Path, path: &str, home: &str) -> String {
    let log = root.join("target/nightly/launchd.log");
    let mut text = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n",
    );
    let _label = writeln!(text, "  <key>Label</key><string>{LABEL}</string>");
    let _program = writeln!(
        text,
        "  <key>ProgramArguments</key>\n  <array>\n    <string>{}</string>\n    \
         <string>xtask</string>\n    <string>nightly</string>\n  </array>",
        xml(cargo)
    );
    let _dir =
        writeln!(text, "  <key>WorkingDirectory</key><string>{}</string>", xml(root.as_str()));
    let _env = writeln!(
        text,
        "  <key>EnvironmentVariables</key>\n  <dict>\n    <key>PATH</key><string>{}</string>\n    \
         <key>HOME</key><string>{}</string>\n  </dict>",
        xml(path),
        xml(home)
    );
    let _when = writeln!(
        text,
        "  <key>StartCalendarInterval</key>\n  <dict>\n    <key>Hour</key><integer>3</integer>\n    \
         <key>Minute</key><integer>0</integer>\n  </dict>"
    );
    let _quiet = writeln!(
        text,
        "  <key>ProcessType</key><string>Background</string>\n  \
         <key>LowPriorityIO</key><true/>\n  <key>Nice</key><integer>10</integer>"
    );
    let _logs = writeln!(
        text,
        "  <key>StandardOutPath</key><string>{0}</string>\n  \
         <key>StandardErrorPath</key><string>{0}</string>",
        xml(log.as_str())
    );
    text.push_str("</dict>\n</plist>\n");
    text
}

fn install(sh: &Shell) -> Result<()> {
    let root = repo_root()?;
    let cargo = cmd!(sh, "which cargo").quiet().read().context("cargo on PATH")?;
    let path = std::env::var("PATH").context("PATH")?;
    let home = std::env::var("HOME").context("HOME")?;
    let file = plist_path()?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {dir}"))?;
    }
    std::fs::create_dir_all(root.join("target/nightly"))?;
    std::fs::write(&file, plist(&cargo, &root, &path, &home))
        .with_context(|| format!("writing {file}"))?;
    let uid = cmd!(sh, "id -u").quiet().read()?;
    let domain = format!("gui/{uid}");
    let _unloaded = cmd!(sh, "launchctl bootout {domain}/{LABEL}").quiet().ignore_stderr().run();
    cmd!(sh, "launchctl bootstrap {domain} {file}").run().context("launchctl bootstrap")?;
    println!("✔ {LABEL} runs `cargo xtask nightly` at 03:00 ({file})");
    Ok(())
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

    #[test]
    fn the_launch_agent_runs_the_nightly_at_three() {
        let text = plist(
            "/Users/me/.cargo/bin/cargo",
            Utf8Path::new("/src/slopty"),
            "/usr/bin:/bin",
            "/Users/me",
        );
        for wanted in [
            "<key>Label</key><string>dev.aislopware.slopty.nightly</string>",
            "<string>/Users/me/.cargo/bin/cargo</string>",
            "<string>nightly</string>",
            "<key>WorkingDirectory</key><string>/src/slopty</string>",
            "<key>Hour</key><integer>3</integer>",
            "<key>Minute</key><integer>0</integer>",
            "<key>ProcessType</key><string>Background</string>",
            "/src/slopty/target/nightly/launchd.log",
        ] {
            assert!(text.contains(wanted), "{wanted} in\n{text}");
        }
        assert_eq!(xml("a<b&c"), "a&lt;b&amp;c");
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
                .all(|c| c.needs.is_some())
        );
    }
}
