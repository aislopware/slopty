//! `xtask remote`: the checks of a working tree on a GitHub runner instead of this Mac.
//!
//! This Mac's cores and disk are the person's, so nothing heavy compiles here
//! (`docs/decisions/tooling.md`, "Every compile runs on a runner"). `remote` commits the working
//! tree as it is, staged or not, beside the real index, pushes that commit to `check/<name>`,
//! and starts `.github/workflows/check.yml` on it: `cargo xtask check -p …` for the crates
//! named, and an `xtask e2e` case. It waits for the run, prints what failed, and after an e2e
//! case that accepts goldens copies the run's goldens over the tree's. The real index, `HEAD`
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
    /// What the e2e case does with the goldens; an accept copies the run's back into the tree.
    #[arg(long, value_enum, default_value_t, requires = "e2e")]
    pub goldens: Goldens,
    /// The simulator the ios case uses: iphone or ipad.
    #[arg(long, default_value = "iphone", requires = "e2e")]
    pub sim: String,
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
        !opts.packages.is_empty() || opts.e2e.is_some(),
        "name what to check: `-p <crate>` (repeatable), `--e2e <case>`, or both"
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
    let sha = snapshot(sh, &root, &dir, &opts.name)?;
    let short = sha.get(..10).unwrap_or(&sha);
    let branch = format!("check/{}", opts.name);
    cmd!(sh, "git push --quiet --force --no-verify origin {sha}:refs/heads/{branch}")
        .run()
        .context("pushing the snapshot")?;
    dispatch(sh, opts, &sha)?;
    let run = find(sh, &format!("check {} {sha}", opts.name))?;
    let what = describe(opts);
    println!("▶ {short} ({what}) on a runner: {}", run.url);
    let conclusion = finish(sh, run.id)?;
    let logs = download(sh, run.id, "logs", &dir.join("logs"));
    if let Some(logs) = logs.as_deref() {
        for name in ["check.log", "e2e.log"] {
            let path = logs.join(name);
            if conclusion != "success" && path.exists() {
                show(&path)?;
            }
        }
    }
    if opts.e2e.is_some() {
        let renders = root.join("target").join("e2e").join("artifacts");
        if download(sh, run.id, "renders", &renders).is_some() {
            println!("  renders and diffs of changed frames: {renders}");
        }
        if matches!(opts.goldens, Goldens::Accept | Goldens::AcceptAll) {
            let golden = root.join("crates").join("slopty-e2e").join("golden");
            let fetched = download(sh, run.id, "goldens", &dir.join("goldens"))
                .context("the run left no goldens to bring back")?;
            let copied = copy_changed(&fetched, &golden)?;
            println!("  {copied} goldens brought back into crates/slopty-e2e/golden");
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

/// What the run checks, in a few words.
fn describe(opts: &RemoteOpts) -> String {
    let mut parts = Vec::new();
    if !opts.packages.is_empty() {
        parts.push(format!("check {}", opts.packages.join(" ")));
    }
    if let Some(case) = &opts.e2e {
        let filter = opts.filter.as_deref().map(|f| format!(" {f}")).unwrap_or_default();
        parts.push(format!("e2e {case}{filter} ({})", opts.goldens.input()));
    }
    parts.join(", ")
}

/// The working tree as a commit on `HEAD`, written through an index of its own: a copy of the
/// real one, so its stat cache spares hashing what did not change, and the real index stays as
/// the person or a lane staged it. Files `.gitignore` names stay out, as they would from a
/// commit; the submodules go at the commits checked out in them.
fn snapshot(sh: &Shell, root: &Path, dir: &Path, name: &str) -> Result<String> {
    let index = dir.join("index");
    let real = cmd!(sh, "git rev-parse --git-path index").read()?;
    std::fs::copy(root.join(real), &index).context("copying the index")?;
    cmd!(sh, "git add --all")
        .env("GIT_INDEX_FILE", &index)
        .quiet()
        .run()
        .context("adding the tree to the snapshot")?;
    let tree = cmd!(sh, "git write-tree").env("GIT_INDEX_FILE", &index).quiet().read()?;
    let message = format!("check: {name}");
    let sha = cmd!(sh, "git commit-tree --no-gpg-sign -p HEAD -m {message} {tree}").read()?;
    Ok(sha)
}

/// Start the workflow on [`REF`] for `sha`.
fn dispatch(sh: &Shell, opts: &RemoteOpts, sha: &str) -> Result<()> {
    let fields = [
        format!("name={}", opts.name),
        format!("sha={sha}"),
        format!("packages={}", opts.packages.join(" ")),
        format!("e2e={}", opts.e2e.as_deref().unwrap_or_default()),
        format!("filter={}", opts.filter.as_deref().unwrap_or_default()),
        format!("goldens={}", opts.goldens.input()),
        format!("sim={}", opts.sim),
    ];
    let args: Vec<String> = fields.iter().flat_map(|f| ["-f".to_owned(), f.clone()]).collect();
    cmd!(sh, "gh workflow run {WORKFLOW} --ref {REF} {args...}")
        .quiet()
        .ignore_stdout()
        .run()
        .context("gh workflow run (is `gh` logged in, and check.yml on main and gate?)")
}

/// The run titled `title`, once GitHub lists it.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling GitHub")]
fn find(sh: &Shell, title: &str) -> Result<Listed> {
    let started = Instant::now();
    loop {
        let listed = cmd!(
            sh,
            "gh run list --workflow {WORKFLOW} --event workflow_dispatch --limit 30 --json databaseId,displayTitle,url"
        )
        .quiet()
        .read()
        .context("gh run list")?;
        let runs: Vec<Listed> = serde_json::from_str(&listed).context("gh run list's JSON")?;
        if let Some(run) = runs.into_iter().find(|r| r.title == title) {
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

/// Copy every file of `from` whose bytes differ from its namesake in `to`; how many went.
fn copy_changed(from: &Path, to: &Path) -> Result<usize> {
    let mut copied: usize = 0;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            continue;
        }
        let target = to.as_std_path().join(entry.file_name());
        let new = std::fs::read(entry.path())?;
        if std::fs::read(&target).ok().as_deref() != Some(new.as_slice()) {
            std::fs::write(&target, new)?;
            copied = copied.saturating_add(1);
        }
    }
    Ok(copied)
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
    use super::noise;

    #[test]
    fn progress_lines_are_noise_and_diagnostics_are_not() {
        assert!(noise("   Compiling slopty-ui v0.1.0"));
        assert!(noise("    Checking serde v1.0.0"));
        assert!(!noise("error[E0308]: mismatched types"));
        assert!(!noise("        FAIL [   0.1s] slopty-ui kit::tests::x"));
    }
}
