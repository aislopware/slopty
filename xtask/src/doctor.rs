//! `xtask doctor`: what on this Mac slows every build and test, and what only the user can fix.
//!
//! **`XProtect`.** macOS scans every new executable on its first launch, one at a time in
//! `XprotectService`: each build script, each relinked test binary, each re-signed daemon copy
//! of an e2e run. A process whose responsible app is listed and switched on under System
//! Settings → Privacy & Security → Developer Tools is not scanned. That is the terminal, or the
//! multiplexer when builds run inside one (`docs/decisions/tooling.md`, "`XProtect` and the
//! Developer Tools switch"). The probe links three fresh binaries, each with bytes no binary had
//! before, and times each one's first launch against its second.
//!
//! **Disk.** The free space on `target/`'s volume against the floor the gate keeps
//! (`xtask/src/prune.rs`).
//!
//! It changes no setting: the switch is the user's to flip.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use camino::Utf8Path;
use xshell::{Shell, cmd};

use crate::prune::{Limits, free_space};
use crate::tools::repo_root;

/// A first launch this much slower than the second is a scan.
const SCANNED: Duration = Duration::from_millis(30);

/// How many fresh binaries the probe times; the median is reported.
const PROBES: usize = 3;

pub fn run(sh: &Shell) -> Result<()> {
    let target = repo_root()?.join("target");
    let mut problems = 0_usize;
    let dir = target.join("doctor");
    std::fs::create_dir_all(&dir).with_context(|| format!("create {dir}"))?;
    let scan = first_launch_cost(sh, &dir)?;
    let chain = ancestors()?;
    let responsible = responsible(&chain);
    println!(
        "  first launch of a new binary: {scan:.0?} longer than its second (median of {PROBES})"
    );
    if scan > SCANNED {
        problems = problems.saturating_add(1);
        let who = responsible.unwrap_or("the terminal you run cargo from");
        println!(
            "⚠ XProtect scans every new build script and test binary. In System Settings → \
             Privacy & Security → Developer Tools, add {who} with + and switch it on, then quit \
             and reopen it. Then run `cargo xtask doctor` again from it."
        );
        let others: Vec<&str> =
            chain.iter().filter(|p| Some(p.as_str()) != responsible).map(String::as_str).collect();
        if !others.is_empty() {
            println!("  the processes above this one: {}", others.join(" ← "));
        }
    } else {
        println!("  ✓ builds from here are exempt from XProtect's first-launch scan");
    }
    let limits = Limits::from_env()?;
    let free = free_space(&target)?;
    if free < limits.floor {
        problems = problems.saturating_add(1);
        println!(
            "⚠ {:.0} GB free on target/'s volume, under the {:.0} GB floor: the gate will prune, \
             then refuse (`cargo xtask prune --dry-run` shows where it goes)",
            gb(free),
            gb(limits.floor)
        );
    } else {
        println!(
            "  ✓ {:.0} GB free on target/'s volume (floor {:.0} GB)",
            gb(free),
            gb(limits.floor)
        );
    }
    let _gone = std::fs::remove_dir_all(&dir);
    if problems == 0 {
        println!("✔ doctor: nothing to fix");
    }
    Ok(())
}

#[expect(clippy::cast_precision_loss, reason = "a size for people to read")]
fn gb(bytes: u64) -> f64 {
    bytes as f64 / 1e9
}

/// The median over [`PROBES`] fresh binaries of first launch minus second launch.
fn first_launch_cost(sh: &Shell, dir: &Utf8Path) -> Result<Duration> {
    let mut costs = Vec::with_capacity(PROBES);
    for n in 0..PROBES {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let source = dir.join(format!("probe{n}.rs"));
        let binary = dir.join(format!("probe-{nonce}"));
        // A constant no binary had before, so its code signature is new to the system.
        std::fs::write(&source, format!("fn main() {{ std::hint::black_box({nonce}_u128); }}\n"))
            .with_context(|| format!("write {source}"))?;
        cmd!(sh, "rustc --edition 2024 -C opt-level=0 {source} -o {binary}").quiet().run()?;
        let first = launch(&binary)?;
        let second = launch(&binary)?;
        costs.push(first.saturating_sub(second));
    }
    costs.sort();
    costs.get(PROBES / 2).copied().context("no probe ran")
}

fn launch(binary: &Utf8Path) -> Result<Duration> {
    let started = Instant::now();
    let status =
        std::process::Command::new(binary).status().with_context(|| format!("run {binary}"))?;
    if !status.success() {
        bail!("{binary} failed: {status}");
    }
    Ok(started.elapsed())
}

/// This process's ancestors, nearest first, by executable path, up to launchd.
fn ancestors() -> Result<Vec<String>> {
    let mut chain = Vec::new();
    let mut pid = std::process::id();
    for _ in 0..32 {
        let out = std::process::Command::new("ps")
            .args(["-o", "ppid=", "-o", "comm=", "-p", &pid.to_string()])
            .output()
            .context("ps")?;
        let line = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        let Some((ppid, _)) = line.split_once(char::is_whitespace) else { break };
        let Ok(ppid) = ppid.trim().parse::<u32>() else { break };
        if ppid <= 1 {
            break;
        }
        let parent = std::process::Command::new("ps")
            .args(["-o", "comm=", "-p", &ppid.to_string()])
            .output()
            .context("ps")?;
        chain.push(String::from_utf8_lossy(&parent.stdout).trim().to_owned());
        pid = ppid;
    }
    Ok(chain)
}

/// What macOS holds responsible for a build started here: the outermost app bundle among the
/// ancestors, else the outermost process (a multiplexer server that launchd started, such as
/// tmux or herdr).
fn responsible(chain: &[String]) -> Option<&str> {
    let app = chain.iter().rev().find_map(|path| {
        let end = path.find(".app/")?;
        path.get(..end.saturating_add(4))
    });
    app.or_else(|| chain.last().map(String::as_str))
}

#[cfg(test)]
mod tests {
    use super::responsible;

    /// The outermost app bundle wins; without one, the outermost process.
    #[test]
    fn the_responsible_process_is_the_outermost_app_or_process() {
        let owned = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let in_app = owned(&[
            "/bin/zsh",
            "/Applications/Ghostty.app/Contents/MacOS/ghostty",
            "/usr/libexec/login",
        ]);
        assert_eq!(responsible(&in_app), Some("/Applications/Ghostty.app"));
        let in_mux = owned(&["/bin/zsh", "claude", "-zsh", "/Users/me/.local/bin/herdr"]);
        assert_eq!(responsible(&in_mux), Some("/Users/me/.local/bin/herdr"));
        assert_eq!(responsible(&[]), None);
    }
}
