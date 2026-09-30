//! `cargo xtask bench`: the `*_cost` measurements, run in release and held to their budgets.
//!
//! A measurement is an ignored test named `*_cost` in a crate that takes `slopty-testkit` as a
//! dev-dependency. It times its samples with `slopty_testkit::bench` and appends one JSON line per
//! series to the file `SLOPTY_BENCH_OUT` names. This command runs them all in release with
//! `--run-ignored only`, reads the lines back and compares each series with
//! [`BUDGETS`]:
//! - retired instructions per operation are the gating number: more than [`SLACK_PERCENT`] over the
//!   budget fails, and a series with no budget fails until `--update-budgets` records it;
//! - wall time is printed, and with `--wall` (the nightly run) appended to
//!   `target/nightly/bench.jsonl` and compared with the last run there, as a trend that never
//!   fails.
//!
//! Instructions, unlike wall time, hardly move on a loaded machine (well under 1 % between runs,
//! `docs/MEASUREMENTS.md`), so a budget on them can be tight. A change that makes a path cheaper
//! is reported as such; `--update-budgets` locks the new number in.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::{Context as _, Result, bail, ensure};
use camino::{Utf8Path, Utf8PathBuf};
use clap::Args;
use serde::Deserialize;
use xshell::{Shell, cmd};

use crate::tools::{WORKSPACE_HACK, repo_root, step, workspace_packages};

/// The budget file, from the repository root.
pub const BUDGETS: &str = "xtask/budgets.toml";

/// How far over its budget a series may go before it fails, in percent.
const SLACK_PERCENT: u64 = 5;

/// How far a nightly wall time may move from the last before it is called out, in percent.
const WALL_TREND_PERCENT: u64 = 25;

/// The crate that provides the measuring; a crate is measured when it takes it.
const TESTKIT: &str = "slopty-testkit";

#[derive(Args, Debug, Clone, Default)]
pub struct BenchOpts {
    /// Only the measurements whose test name contains this.
    #[arg(long)]
    pub filter: Option<String>,
    /// Write what this run measured into the budget file (all of it without `--filter`, which
    /// also drops the budgets of measurements that are gone).
    #[arg(long)]
    pub update_budgets: bool,
    /// Keep the wall times: append them to `target/nightly/bench.jsonl` and compare them with
    /// the last run there.
    #[arg(long)]
    pub wall: bool,
    /// The file this run's measurements are written to (default `target/bench/measured.jsonl`).
    #[arg(long)]
    pub out: Option<Utf8PathBuf>,
}

/// One series as `slopty_testkit::bench` writes it.
#[derive(Deserialize, Debug, Clone)]
struct Measured {
    name: String,
    samples: u64,
    ops: u64,
    instructions: Option<u64>,
    wall_ns: Wall,
}

#[derive(Deserialize, serde::Serialize, Debug, Clone, Copy)]
struct Wall {
    p50: u64,
    p95: u64,
    p99: u64,
    p999: u64,
    max: u64,
}

/// A series against its budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Within the slack.
    Held,
    /// Cheaper by more than the slack: worth recording.
    Cheaper,
    /// Over the budget by more than the slack.
    Over,
    /// No budget yet.
    New,
    /// Wall time only; nothing to hold.
    Unbudgeted,
}

pub fn run(sh: &Shell, opts: &BenchOpts) -> Result<()> {
    let root = repo_root()?;
    // Absolute: each test runs in its crate's directory.
    let out = root
        .join(opts.out.as_deref().unwrap_or_else(|| Utf8Path::new("target/bench/measured.jsonl")));
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {dir}"))?;
    }
    std::fs::write(&out, "").with_context(|| format!("truncating {out}"))?;
    let packages = measured_packages()?;
    let selected: Vec<String> = packages
        .iter()
        .map(String::as_str)
        .chain([WORKSPACE_HACK])
        .flat_map(|p| ["-p".to_owned(), p.to_owned()])
        .collect();
    let filter = match &opts.filter {
        Some(f) => format!("test(/_cost$/) and test(~{f})"),
        None => "test(/_cost$/)".to_owned(),
    };
    let ran = step(
        &format!("measure {}", packages.join(", ")),
        &cmd!(
            sh,
            "nice -n 10 cargo nextest run --release {selected...} --run-ignored only -E {filter} --no-capture --no-fail-fast --no-tests=pass"
        )
        .env("SLOPTY_BENCH_OUT", &out),
    );
    let measured = read_measured(&out)?;
    let budgets = read_budgets(&root.join(BUDGETS))?;
    let judged: Vec<(Measured, Verdict, Option<u64>)> = measured
        .into_iter()
        .map(|m| {
            let budget = budgets.get(&m.name).copied();
            let verdict = judge(m.instructions, budget);
            (m, verdict, budget)
        })
        .collect();
    let stale: Vec<&String> = if opts.filter.is_none() && ran.is_ok() {
        budgets.keys().filter(|k| !judged.iter().any(|(m, ..)| &m.name == *k)).collect()
    } else {
        Vec::new()
    };
    print!("{}", table(&judged));
    for name in &stale {
        println!("  stale budget: {name} was not measured");
    }
    if opts.wall {
        trend(&root.join("target/nightly/bench.jsonl"), &judged, sh)?;
    }
    if opts.update_budgets {
        // A run that failed measured only part of the tree: rewriting from it would drop the
        // budget of every series it never reached.
        ensure!(ran.is_ok(), "the measurement failed, so {BUDGETS} is left as it was: {ran:?}");
        let next = next_budgets(&budgets, &judged, opts.filter.is_some());
        write_budgets(&root.join(BUDGETS), &next)?;
        println!("✔ wrote {} budgets to {BUDGETS}", next.len());
    }
    ran?;
    if judged.is_empty() {
        bail!("no measurement reported: is `{TESTKIT}` a dev-dependency of the measured crates?");
    }
    let over: Vec<&str> = judged
        .iter()
        .filter(|(_, v, _)| *v == Verdict::Over)
        .map(|(m, ..)| m.name.as_str())
        .collect();
    let new: Vec<&str> = judged
        .iter()
        .filter(|(_, v, _)| *v == Verdict::New)
        .map(|(m, ..)| m.name.as_str())
        .collect();
    if !opts.update_budgets {
        if !over.is_empty() {
            bail!("over budget by more than {SLACK_PERCENT}%: {}", over.join(", "));
        }
        if !new.is_empty() {
            bail!(
                "no budget (record with `cargo xtask bench --update-budgets`): {}",
                new.join(", ")
            );
        }
        if !stale.is_empty() {
            bail!("budgets of measurements that are gone (drop with `--update-budgets`)");
        }
    }
    if !opts.update_budgets {
        println!("✔ {} series within {SLACK_PERCENT}% of their budgets", judged.len());
    }
    Ok(())
}

/// The workspace crates that take [`TESTKIT`] as a dev-dependency, on any target.
fn measured_packages() -> Result<Vec<String>> {
    let mut found = Vec::new();
    for p in workspace_packages()? {
        let manifest = p.dir.join("Cargo.toml");
        let text =
            std::fs::read_to_string(&manifest).with_context(|| format!("reading {manifest}"))?;
        let parsed: toml::Table =
            toml::from_str(&text).with_context(|| format!("parsing {manifest}"))?;
        if p.name != TESTKIT && takes_testkit(&parsed) {
            found.push(p.name);
        }
    }
    found.sort();
    Ok(found)
}

fn takes_testkit(manifest: &toml::Table) -> bool {
    let in_dev = |t: &toml::Table| {
        t.get("dev-dependencies")
            .and_then(toml::Value::as_table)
            .is_some_and(|d| d.contains_key(TESTKIT))
    };
    in_dev(manifest)
        || manifest
            .get("target")
            .and_then(toml::Value::as_table)
            .is_some_and(|targets| targets.values().filter_map(toml::Value::as_table).any(in_dev))
}

fn read_measured(path: &Utf8Path) -> Result<Vec<Measured>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str(l).with_context(|| format!("a measurement line in {path}: {l}"))
        })
        .collect()
}

fn read_budgets(path: &Utf8Path) -> Result<BTreeMap<String, u64>> {
    #[derive(Deserialize)]
    struct File {
        instructions: BTreeMap<String, u64>,
    }
    let Ok(text) = std::fs::read_to_string(path) else { return Ok(BTreeMap::new()) };
    let file: File = toml::from_str(&text).with_context(|| format!("parsing {path}"))?;
    Ok(file.instructions)
}

/// The budgets a run records: every series it measured, and, when a filter picked only some,
/// the budgets it did not reach as they were. A whole run drops the budgets of series gone.
fn next_budgets(
    budgets: &BTreeMap<String, u64>,
    judged: &[(Measured, Verdict, Option<u64>)],
    filtered: bool,
) -> BTreeMap<String, u64> {
    let mut next = if filtered { budgets.clone() } else { BTreeMap::new() };
    for (m, ..) in judged {
        if let Some(i) = m.instructions {
            next.insert(m.name.clone(), i);
        }
    }
    next
}

fn write_budgets(path: &Utf8Path, budgets: &BTreeMap<String, u64>) -> Result<()> {
    let mut text = String::from(
        "# Retired instructions per operation of each `*_cost` series, the median of its samples on\n\
         # this Mac in release (`cargo xtask bench`, `xtask/src/bench.rs`). A run more than 5 %\n\
         # over fails; `cargo xtask bench --update-budgets` rewrites this file from a run.\n\n\
         [instructions]\n",
    );
    for (name, value) in budgets {
        let _written = writeln!(text, "\"{name}\" = {value}");
    }
    std::fs::write(path, text).with_context(|| format!("writing {path}"))
}

const fn judge(instructions: Option<u64>, budget: Option<u64>) -> Verdict {
    match (instructions, budget) {
        (None, _) => Verdict::Unbudgeted,
        (Some(_), None) => Verdict::New,
        (Some(got), Some(budget)) => {
            let slack = budget.saturating_mul(SLACK_PERCENT) / 100;
            if got > budget.saturating_add(slack) {
                Verdict::Over
            } else if got < budget.saturating_sub(slack) {
                Verdict::Cheaper
            } else {
                Verdict::Held
            }
        }
    }
}

/// The change from `budget` to `got`, in percent with a sign and one decimal.
fn delta(got: u64, budget: u64) -> String {
    let change = i128::from(got).saturating_sub(i128::from(budget)).saturating_mul(1000);
    let Some(permille) = change.checked_div(i128::from(budget)) else { return "—".to_owned() };
    let sign = if change < 0 { "-" } else { "+" };
    let magnitude = permille.unsigned_abs();
    format!("{sign}{}.{}%", magnitude / 10, magnitude % 10)
}

fn table(judged: &[(Measured, Verdict, Option<u64>)]) -> String {
    let mut text = String::new();
    let width = judged.iter().map(|(m, ..)| m.name.len()).max().unwrap_or(4).max(4);
    let _header = writeln!(
        text,
        "{:width$}  {:>12}  {:>12}  {:>8}  {:>10}  {:>10}  {:>10}  verdict",
        "series", "instr/op", "budget", "Δ", "wall p50", "p95", "p99"
    );
    for (m, verdict, budget) in judged {
        let instructions = m.instructions.map_or_else(|| "—".to_owned(), |i| i.to_string());
        let budget_text = budget.map_or_else(|| "—".to_owned(), |b| b.to_string());
        let change =
            m.instructions.zip(*budget).map_or_else(|| "—".to_owned(), |(g, b)| delta(g, b));
        let _row = writeln!(
            text,
            "{:width$}  {instructions:>12}  {budget_text:>12}  {change:>8}  {:>10}  {:>10}  {:>10}  {}",
            m.name,
            ns(m.wall_ns.p50),
            ns(m.wall_ns.p95),
            ns(m.wall_ns.p99),
            match verdict {
                Verdict::Held => "held",
                Verdict::Cheaper => "cheaper: --update-budgets",
                Verdict::Over => "OVER",
                Verdict::New => "no budget",
                Verdict::Unbudgeted => "wall only",
            }
        );
    }
    text
}

/// Nanoseconds, readable.
fn ns(ns: u64) -> String {
    if ns >= 10_000_000 {
        format!("{} ms", ns / 1_000_000)
    } else if ns >= 10_000 {
        format!("{} us", ns / 1_000)
    } else {
        format!("{ns} ns")
    }
}

/// One nightly record of a series' wall time.
#[derive(Deserialize, serde::Serialize, Debug, Clone)]
struct WallRecord {
    date: String,
    rev: String,
    name: String,
    samples: u64,
    ops: u64,
    instructions: Option<u64>,
    wall_ns: Wall,
}

/// Compare the wall times with the last nightly record of each series, then append this run's.
fn trend(path: &Utf8Path, judged: &[(Measured, Verdict, Option<u64>)], sh: &Shell) -> Result<()> {
    let mut last: BTreeMap<String, WallRecord> = BTreeMap::new();
    if let Ok(text) = std::fs::read_to_string(path) {
        for line in text.lines() {
            if let Ok(r) = serde_json::from_str::<WallRecord>(line) {
                last.insert(r.name.clone(), r);
            }
        }
    }
    let date = cmd!(sh, "date +%Y-%m-%d").quiet().read()?;
    let rev = cmd!(sh, "git rev-parse --short HEAD").quiet().read().unwrap_or_default();
    let mut appended = String::new();
    println!("wall p50 against the last nightly run (±{WALL_TREND_PERCENT}% is called out):");
    for (m, ..) in judged {
        if let Some(prev) = last.get(&m.name) {
            let moved = delta(m.wall_ns.p50, prev.wall_ns.p50);
            let limit = prev.wall_ns.p50.saturating_mul(WALL_TREND_PERCENT) / 100;
            let flag = if m.wall_ns.p50 > prev.wall_ns.p50.saturating_add(limit) {
                "  slower"
            } else if m.wall_ns.p50 < prev.wall_ns.p50.saturating_sub(limit) {
                "  faster"
            } else {
                ""
            };
            println!(
                "  {}: {} → {} ({moved}, since {}){flag}",
                m.name,
                ns(prev.wall_ns.p50),
                ns(m.wall_ns.p50),
                prev.date
            );
        }
        let record = WallRecord {
            date: date.clone(),
            rev: rev.clone(),
            name: m.name.clone(),
            samples: m.samples,
            ops: m.ops,
            instructions: m.instructions,
            wall_ns: m.wall_ns,
        };
        appended.push_str(&serde_json::to_string(&record)?);
        appended.push('\n');
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {dir}"))?;
    }
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    std::io::Write::write_all(&mut file, appended.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filtered_run_keeps_the_budgets_it_did_not_reach() {
        let wall = Wall { p50: 0, p95: 0, p99: 0, p999: 0, max: 0 };
        let measured = |name: &str, i| Measured {
            name: name.to_owned(),
            samples: 1,
            ops: 1,
            instructions: Some(i),
            wall_ns: wall,
        };
        let budgets = BTreeMap::from([("a".to_owned(), 10_u64), ("b".to_owned(), 20)]);
        let judged = [(measured("a", 11), Verdict::Held, Some(10))];
        let filtered = next_budgets(&budgets, &judged, true);
        assert_eq!(filtered, BTreeMap::from([("a".to_owned(), 11), ("b".to_owned(), 20)]));
        let whole = next_budgets(&budgets, &judged, false);
        assert_eq!(whole, BTreeMap::from([("a".to_owned(), 11)]), "a whole run drops what is gone");
    }

    #[test]
    fn a_budget_holds_within_its_slack() {
        assert_eq!(judge(Some(105), Some(100)), Verdict::Held);
        assert_eq!(judge(Some(106), Some(100)), Verdict::Over);
        assert_eq!(judge(Some(94), Some(100)), Verdict::Cheaper);
        assert_eq!(judge(Some(1), None), Verdict::New);
        assert_eq!(judge(None, Some(100)), Verdict::Unbudgeted);
        assert_eq!(delta(1_055, 1_000), "+5.5%");
        assert_eq!(delta(900, 1_000), "-10.0%");
        assert_eq!(delta(9_242, 9_317), "-0.8%", "a fall under a percent keeps its sign");
        assert_eq!(delta(1, 0), "—");
    }

    #[test]
    fn the_measured_crates_are_the_ones_that_take_the_testkit() {
        let plain: toml::Table =
            toml::from_str("[dev-dependencies]\nslopty-testkit.workspace = true\n").unwrap();
        let targeted: toml::Table = toml::from_str(
            "[target.'cfg(target_os = \"macos\")'.dev-dependencies]\nslopty-testkit.workspace = true\n",
        )
        .unwrap();
        let none: toml::Table =
            toml::from_str("[dependencies]\nslopty-testkit.workspace = true\n").unwrap();
        assert!(takes_testkit(&plain) && takes_testkit(&targeted), "dev-dependencies, any target");
        assert!(!takes_testkit(&none), "a normal dependency is not a measured crate");
        let names = measured_packages().unwrap();
        assert!(names.iter().any(|n| n == "slopty-engine"), "{names:?}");
    }

    #[test]
    fn budgets_round_trip_through_their_file() {
        let dir = std::env::temp_dir().join(format!("xtask-bench-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.join("budgets.toml")).unwrap();
        let budgets = BTreeMap::from([("engine.frame_cost.write".to_owned(), 41_000_u64)]);
        write_budgets(&path, &budgets).unwrap();
        assert_eq!(read_budgets(&path).unwrap(), budgets);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_checked_in_budgets_parse() {
        let budgets = read_budgets(&repo_root().unwrap().join(BUDGETS)).unwrap();
        assert!(!budgets.is_empty(), "{BUDGETS} holds the budgets");
    }
}
