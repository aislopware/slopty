//! `xtask remote`: the checks of a working tree on a GitHub runner instead of this Mac.
//!
//! This Mac's cores and disk are the person's, so nothing heavy compiles here
//! (`docs/decisions/tooling.md`, "Every compile runs on a runner"). `remote` commits the working
//! tree as it is, staged or not, beside the real index, pushes that commit to `check/<name>`,
//! and starts `.github/workflows/check.yml` on it: `cargo xtask check -p …` for the crates
//! named, and an `xtask e2e` case. It waits for the run, prints what failed, and after an e2e
//! case that accepts goldens, or a check that accepts snapshots, applies what the run wrote to the
//! tree. The real index, `HEAD`
//! and the tree are never touched, so lanes editing this checkout keep working meanwhile.
//!
//! The run is dispatched on `gate`, so it reads and fills the compile cache the gate's lanes
//! use; a newer check of the same name cancels the older one.

use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use camino::{Utf8Path as Path, Utf8PathBuf as PathBuf};
use xshell::{Shell, cmd};

/// The workflow that runs the checks.
const WORKFLOW: &str = "check.yml";

/// The ref it is dispatched on, whose compile cache it shares.
const REF: &str = "gate";

/// How often the run is asked about.
const POLL: Duration = Duration::from_secs(20);

/// The longest wait for GitHub to list the run after dispatching it.
const FIND: Duration = Duration::from_secs(180);

/// The longest wait for the run to finish, queueing behind the gate's macOS jobs included.
const FINISH: Duration = Duration::from_hours(3);

/// The most lines of a failed log printed; the whole log stays under `target/remote/<name>/`.
const SHOWN: usize = 400;

/// The artifacts `check.yml`'s jobs leave their logs in, one a job: the lints, the tests, and
/// the probe or e2e case. Each holds that job's `*.log`, and the tests' and the run's a
/// `changes.patch` of what they accepted.
const LOG_ARTIFACTS: [&str; 3] = ["logs-lint", "logs-test", "logs-run"];

/// The runner a check's tests take: Linux when every crate named tests there, as CI's linux lane
/// tests it, so the check takes none of the five macOS runners; a Mac otherwise.
fn test_runner(packages: &[String]) -> &'static str {
    let linux = crate::gate::linux_tested();
    if !packages.is_empty() && packages.iter().all(|p| linux.contains(&p.as_str())) {
        "ubuntu-24.04"
    } else {
        "macos-26"
    }
}

/// What a run does with the goldens of its e2e case.
#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Goldens {
    /// Compare every frame with its golden.
    #[default]
    Compare,
    /// Write missing and failing goldens, and bring them back into the tree.
    Accept,
    /// Rewrite every golden, and bring them back into the tree.
    AcceptAll,
    /// Fail on no golden and write none: every changed frame's render and diff come back.
    Review,
}

impl Goldens {
    const fn input(self) -> &'static str {
        match self {
            Self::Compare => "compare",
            Self::Accept => "accept",
            Self::AcceptAll => "accept-all",
            Self::Review => "review",
        }
    }
}

/// `cargo xtask remote` options.
#[derive(clap::Args, Debug)]
pub struct RemoteOpts {
    /// Who asks: a lane's name. A newer check of the same name cancels the older one, and the
    /// commit goes to the branch `check/<name>`.
    #[arg(long, env = "SLOPTY_LANE", default_value = "main")]
    pub name: String,
    /// A crate for `cargo xtask check` (fmt, clippy for the host, iOS and Linux, nextest,
    /// rustdoc, shear, typos); repeat for several.
    #[arg(short = 'p', long = "package")]
    pub packages: Vec<String>,
    /// An `xtask e2e` case to run after the checks: app, ios, worker, …
    #[arg(long)]
    pub e2e: Option<String>,
    /// The e2e case's nextest filterset, such as `test(a_folder_tile_browses_the_worker)`.
    #[arg(short = 'E', long, requires = "e2e")]
    pub filter: Option<String>,
    /// What the e2e case does with the goldens; an accept applies the run's to the tree.
    #[arg(long, value_enum, default_value_t, requires = "e2e")]
    pub goldens: Goldens,
    /// The simulator the ios case uses: iphone or ipad.
    #[arg(long, default_value = "iphone", requires = "e2e")]
    pub sim: String,
    /// Take the tree's edits under these paths only (files or directories, repeatable), on top
    /// of `HEAD`: another lane's unfinished edits elsewhere stay out of the check.
    #[arg(long = "only", value_name = "PATH")]
    pub only: Vec<String>,
    /// Every check: iOS and Linux clippy and rustdoc too, not only fmt, host clippy and the
    /// tests.
    #[arg(long)]
    pub full: bool,
    /// Accept every insta snapshot the tests change, and apply them to the tree.
    #[arg(long)]
    pub accept_snapshots: bool,
    /// Follow run `ID` already started (a wrapper that was stopped, say) instead of starting
    /// one: wait for it, print what failed and apply what it accepted, as the same options say.
    #[arg(long = "run", value_name = "ID")]
    pub attach: Option<u64>,
    /// The runner image: macos-26, or ubuntu-24.04 / ubuntu-24.04-arm for what needs no Mac.
    #[arg(long, default_value = "macos-26")]
    pub runner: String,
    /// A shell command to run on the runner after its setup, to try a step there.
    #[arg(long)]
    pub probe: Option<String>,
    /// Check `HEAD` as committed, without the tree's edits: other lanes' unfinished work stays
    /// out of it.
    #[arg(long)]
    pub head: bool,
    /// Dispatch the `check.yml` of this branch instead of `gate`'s, to try a change to the
    /// workflow before it lands. Its compile cache is that branch's, so it starts cold.
    #[arg(long, value_name = "BRANCH")]
    pub workflow_ref: Option<String>,
}

/// A run as `gh run list` describes it.
#[derive(serde::Deserialize)]
struct Listed {
    #[serde(rename = "databaseId")]
    id: u64,
    #[serde(rename = "displayTitle")]
    title: String,
    url: String,
}

pub fn run(sh: &Shell, opts: &RemoteOpts) -> Result<()> {
    ensure!(
        opts.attach.is_some()
            || !opts.packages.is_empty()
            || opts.e2e.is_some()
            || opts.probe.is_some(),
        "name what to check: `-p <crate>` (repeatable), `--e2e <case>`, `--probe <command>`"
    );
    ensure!(
        opts.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "a check's name is letters, digits, `-` and `_`: {}",
        opts.name
    );
    let root = crate::tools::repo_root()?;
    let dir = root.join("target").join("remote").join(&opts.name);
    std::fs::create_dir_all(&dir)?;
    let started = Instant::now();
    let what = describe(opts);
    let (run, short) = if let Some(id) = opts.attach {
        let run_id = id.to_string();
        let url = cmd!(sh, "gh run view {run_id} --json url --jq .url").read()?;
        (Listed { id, title: String::new(), url }, format!("run {id}"))
    } else {
        let sha = if opts.head {
            cmd!(sh, "git rev-parse HEAD").read()?
        } else {
            snapshot(sh, &root, &dir, &opts.name, &opts.only)?
        };
        let branch = format!("check/{}", opts.name);
        cmd!(sh, "git push --quiet --force --no-verify origin {sha}:refs/heads/{branch}")
            .run()
            .context("pushing the snapshot")?;
        let title = format!("check {} {sha}", opts.name);
        // A check of the same commit under the same name ran before (a retry on `--head`): its
        // runs are known now, so the one found below is the one this dispatch starts.
        let before: Vec<u64> = listed(sh)?.into_iter().map(|r| r.id).collect();
        dispatch(sh, opts, &sha)?;
        let run = find(sh, &title, &before)?;
        (run, sha.get(..10).unwrap_or(&sha).to_owned())
    };
    println!("▶ {short} ({what}) on a runner: {}", run.url);
    println!("  if this is stopped: cargo xtask remote --name {} --run {} …", opts.name, run.id);
    let conclusion = finish(sh, run.id)?;
    let logs = dir.join("logs");
    if logs.exists() {
        std::fs::remove_dir_all(&logs)?;
    }
    let jobs: Vec<PathBuf> = LOG_ARTIFACTS
        .iter()
        .filter_map(|name| download(sh, run.id, name, &logs.join(name)))
        .collect();
    if conclusion != "success" {
        for job in &jobs {
            for name in ["lint.log", "test.log", "probe.log", "e2e.log"] {
                let path = job.join(name);
                if path.exists() {
                    show(&path)?;
                }
            }
        }
    }
    if opts.e2e.is_some() {
        let renders = root.join("target").join("e2e").join("artifacts");
        if download(sh, run.id, "renders", &renders).is_some() {
            println!("  renders and diffs of changed frames: {renders}");
        }
    }
    let accepts =
        opts.accept_snapshots || matches!(opts.goldens, Goldens::Accept | Goldens::AcceptAll);
    if accepts {
        let patches: Vec<PathBuf> =
            jobs.iter().map(|j| j.join("changes.patch")).filter(|p| p.exists()).collect();
        ensure!(!patches.is_empty(), "the run left no patch of what it accepted");
        let mut files = 0_usize;
        for patch in patches.iter().filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() > 0)) {
            files = files.saturating_add(apply_accepted(sh, patch)?);
        }
        if files == 0 {
            println!("  the run accepted nothing new");
        } else {
            println!("  {files} accepted files applied to the tree");
        }
    }
    let took = started.elapsed().as_secs();
    if conclusion == "success" {
        println!("✔ {what} passed on {short} ({}m{:02}s)", took / 60, took % 60);
        return Ok(());
    }
    bail!(
        "{what} {conclusion} on {short} ({}m{:02}s): {}\n  every log: {}",
        took / 60,
        took % 60,
        run.url,
        dir
    )
}

/// The paths in `git apply --numstat`'s report that no accept writes: an accept writes the e2e
/// goldens and insta's snapshots, nothing else. A patch with others was cut from more of the
/// runner's tree than the accepted files (run 38110915311 brought 61 MB of zig's cache).
fn unaccepted(numstat: &str) -> Vec<&str> {
    numstat
        .lines()
        .filter_map(|line| line.splitn(3, '\t').nth(2))
        .filter(|path| {
            !(path.starts_with("crates/slopty-e2e/golden/")
                || Path::new(path).extension() == Some("snap"))
        })
        .collect()
}

/// Applies the patch of what a run accepted to the tree, once it is shown to touch only what an
/// accept writes, and says how many files it changed.
fn apply_accepted(sh: &Shell, patch: &Path) -> Result<usize> {
    let numstat = cmd!(sh, "git apply --numstat {patch}").read()?;
    let others = unaccepted(&numstat);
    ensure!(
        others.is_empty(),
        "{patch} touches {} paths no accept writes, so none of it is applied: {}",
        others.len(),
        others.iter().take(8).copied().collect::<Vec<_>>().join(", ")
    );
    cmd!(sh, "git apply --binary {patch}")
        .run()
        .context("applying what the run accepted to the tree")?;
    Ok(numstat.lines().count())
}

/// What the run checks, in a few words.
fn describe(opts: &RemoteOpts) -> String {
    let mut parts = Vec::new();
    if !opts.packages.is_empty() {
        let depth = if opts.full { "full" } else { "quick" };
        let tests = test_runner(&opts.packages);
        parts.push(format!("{depth} check {} (tests on {tests})", opts.packages.join(" ")));
    }
    if let Some(case) = &opts.e2e {
        let filter = opts.filter.as_deref().map(|f| format!(" {f}")).unwrap_or_default();
        parts.push(format!("e2e {case}{filter} ({})", opts.goldens.input()));
    }
    if let Some(probe) = &opts.probe {
        parts.push(format!("probe `{probe}`"));
    }
    if opts.runner != "macos-26" {
        parts.push(format!("on {}", opts.runner));
    }
    parts.join(", ")
}

/// The working tree as a commit on `HEAD`, written through an index of its own, so the real
/// index stays as the person or a lane staged it. With no `only`, it starts from a copy of the
/// real index, whose stat cache spares hashing what did not change, and takes every edit; with
/// `only`, it starts from `HEAD` and takes the edits under those paths alone. Files
/// `.gitignore` names stay out, as they would from a commit; the submodules go at the commits
/// checked out in them.
fn snapshot(sh: &Shell, root: &Path, dir: &Path, name: &str, only: &[String]) -> Result<String> {
    let index = dir.join("index");
    if only.is_empty() {
        let real = cmd!(sh, "git rev-parse --git-path index").read()?;
        std::fs::copy(root.join(real), &index).context("copying the index")?;
        cmd!(sh, "git add --all")
            .env("GIT_INDEX_FILE", &index)
            .quiet()
            .run()
            .context("adding the tree to the snapshot")?;
    } else {
        cmd!(sh, "git read-tree HEAD").env("GIT_INDEX_FILE", &index).quiet().run()?;
        cmd!(sh, "git add --all -- {only...}")
            .env("GIT_INDEX_FILE", &index)
            .quiet()
            .run()
            .context("adding the paths to the snapshot")?;
    }
    let tree = cmd!(sh, "git write-tree").env("GIT_INDEX_FILE", &index).quiet().read()?;
    let message = format!("check: {name}");
    let sha = cmd!(sh, "git commit-tree --no-gpg-sign -p HEAD -m {message} {tree}").read()?;
    Ok(sha)
}

/// Start the workflow for `sha`, on [`REF`] unless `--workflow-ref` names a branch whose
/// `check.yml` to try. An input goes only when it differs from the workflow's default, so a tree
/// whose xtask knows an input `gate`'s `check.yml` does not yet have still dispatches.
fn dispatch(sh: &Shell, opts: &RemoteOpts, sha: &str) -> Result<()> {
    let snapshots = if opts.accept_snapshots { "accept" } else { "compare" };
    let fields: [(&str, String, &str); 12] = [
        ("name", opts.name.clone(), ""),
        ("sha", sha.to_owned(), ""),
        ("packages", opts.packages.join(" "), ""),
        ("e2e", opts.e2e.clone().unwrap_or_default(), ""),
        ("filter", opts.filter.clone().unwrap_or_default(), ""),
        ("goldens", opts.goldens.input().to_owned(), "compare"),
        ("sim", opts.sim.clone(), "iphone"),
        ("snapshots", snapshots.to_owned(), "compare"),
        ("full", (if opts.full { "yes" } else { "no" }).to_owned(), "no"),
        ("runner", opts.runner.clone(), "macos-26"),
        ("test_runner", test_runner(&opts.packages).to_owned(), "macos-26"),
        ("probe", opts.probe.clone().unwrap_or_default(), ""),
    ];
    let args: Vec<String> = fields
        .iter()
        .filter(|(key, value, default)| matches!(*key, "name" | "sha") || value != default)
        .flat_map(|(key, value, _)| ["-f".to_owned(), format!("{key}={value}")])
        .collect();
    let on = opts.workflow_ref.as_deref().unwrap_or(REF);
    cmd!(sh, "gh workflow run {WORKFLOW} --ref {on} {args...}")
        .quiet()
        .ignore_stdout()
        .run()
        .context("gh workflow run (is `gh` logged in, and check.yml on main and gate?)")
}

/// The newest dispatched runs of [`WORKFLOW`].
fn listed(sh: &Shell) -> Result<Vec<Listed>> {
    let listed = cmd!(
        sh,
        "gh run list --workflow {WORKFLOW} --event workflow_dispatch --limit 30 --json databaseId,displayTitle,url"
    )
    .quiet()
    .read()
    .context("gh run list")?;
    serde_json::from_str(&listed).context("gh run list's JSON")
}

/// The run titled `title` that is none of `before`, once GitHub lists it.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling GitHub")]
fn find(sh: &Shell, title: &str, before: &[u64]) -> Result<Listed> {
    let started = Instant::now();
    loop {
        let fresh = listed(sh)?.into_iter().find(|r| r.title == title && !before.contains(&r.id));
        if let Some(run) = fresh {
            return Ok(run);
        }
        ensure!(started.elapsed() < FIND, "GitHub listed no run `{title}` after {FIND:?}");
        std::thread::sleep(POLL);
    }
}

/// The run's conclusion once it completes, printing each change of its status meanwhile.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling GitHub")]
fn finish(sh: &Shell, id: u64) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct State {
        status: String,
        conclusion: String,
    }
    let id = id.to_string();
    let started = Instant::now();
    let mut last = String::new();
    loop {
        // A transient API error is asked again, not taken for the run's end.
        let state = cmd!(sh, "gh run view {id} --json status,conclusion").quiet().read();
        if let Ok(state) = state {
            let state: State = serde_json::from_str(&state).context("gh run view's JSON")?;
            if state.status == "completed" {
                return Ok(state.conclusion);
            }
            if state.status != last {
                let secs = started.elapsed().as_secs();
                println!("  {} ({}m{:02}s)", state.status, secs / 60, secs % 60);
                last = state.status;
            }
        }
        ensure!(started.elapsed() < FINISH, "run {id} still {last} after {FINISH:?}");
        std::thread::sleep(POLL);
    }
}

/// The run's artifact `name` unpacked into `into`, emptied first; `None` when the run left
/// none.
fn download(sh: &Shell, id: u64, name: &str, into: &Path) -> Option<PathBuf> {
    let id = id.to_string();
    if into.exists() && std::fs::remove_dir_all(into).is_err() {
        return None;
    }
    std::fs::create_dir_all(into).ok()?;
    cmd!(sh, "gh run download {id} --name {name} --dir {into}")
        .quiet()
        .ignore_stdout()
        .ignore_stderr()
        .run()
        .ok()?;
    Some(into.to_path_buf())
}

/// The end of a failed log, without the build's progress lines.
fn show(path: &Path) -> Result<()> {
    let text = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = text.lines().filter(|l| !noise(l)).collect();
    let skip = lines.len().saturating_sub(SHOWN);
    let tail = lines.get(skip..).unwrap_or_default();
    println!("── {path} (last {} lines of {}) ──", tail.len(), lines.len());
    for line in tail {
        println!("{line}");
    }
    Ok(())
}

/// A progress line cargo prints while it builds, which says nothing about a failure.
fn noise(line: &str) -> bool {
    let line = line.trim_start();
    ["Compiling ", "Checking ", "Downloaded ", "Downloading ", "Blocking ", "Fresh ", "Building "]
        .iter()
        .any(|p| line.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::{noise, unaccepted};

    #[test]
    fn progress_lines_are_noise_and_diagnostics_are_not() {
        assert!(noise("   Compiling slopty-ui v0.1.0"));
        assert!(noise("    Checking serde v1.0.0"));
        assert!(!noise("error[E0308]: mismatched types"));
        assert!(!noise("        FAIL [   0.1s] slopty-ui kit::tests::x"));
    }

    #[test]
    fn an_accept_patch_may_touch_only_goldens_and_snapshots() {
        let numstat = "-\t-\tcrates/slopty-e2e/golden/ipad/home.png\n\
                       3\t1\tcrates/slopty-proto/tests/snapshots/golden__hello.snap\n\
                       -\t-\t.zig-cache/o/1f/libc.a\n\
                       2\t0\tCargo.lock\n";
        assert_eq!(unaccepted(numstat), [".zig-cache/o/1f/libc.a", "Cargo.lock"]);
        assert_eq!(unaccepted(""), [""; 0]);
    }
}
