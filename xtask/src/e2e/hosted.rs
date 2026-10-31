//! The e2e cases on GitHub's hosted Macs, asked for from this one: `e2e.yml` dispatched on
//! `gate` for a commit already on GitHub, followed to its end, and its artifacts brought here
//! (`docs/decisions/tooling.md`, "The gate and e2e run on free hosted runners; this Mac runs
//! none", item 4): `cargo xtask e2e mac|ios --hosted` runs a case through it, as a lane's
//! `remote --e2e` and a round's whole suite are to, and a golden accept applies the patch a run
//! leaves (`changes.patch` in each case's artifact).
//!
//! Dispatched on `gate`, a run reads the compile cache the gate's test shards fill there, as the
//! nightly does. Each case is one job that builds once: `mac` runs the app's and the server's
//! suites, `ios` the simulator's on the iPhone and then the iPad's one test.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use camino::{Utf8Path, Utf8PathBuf};
use xshell::{Shell, cmd};

/// The workflow it dispatches.
const WORKFLOW: &str = "e2e.yml";

/// The branch it dispatches on, whose compile cache the run reads.
const REF: &str = "gate";

/// How often it asks GitHub about the run.
const POLL: Duration = Duration::from_secs(20);

/// How many questions about a run may fail in a row, a network's blip, before following it does.
const MISSES: u32 = 6;

/// The longest it waits for the dispatched run to show up.
const FIND: Duration = Duration::from_secs(120);

/// The longest it follows a run: a cold build of the app is over 30 minutes, and each job's
/// own timeout is 90.
const FINISH: Duration = Duration::from_hours(2);

/// A kind of hosted run, one job of `e2e.yml` each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The app's suites and the server's, from one build.
    Mac,
    /// The simulator's suite on the iPhone, then the iPad's launch and layout, from one build.
    Ios,
}

impl Kind {
    /// Its name in the workflow's matrix and its job's name (`e2e (mac)`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::Mac => "mac",
            Self::Ios => "ios",
        }
    }
}

/// What a hosted run does with the goldens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Goldens {
    /// A changed frame fails nothing and leaves its render and diff in the artifact: a runner's
    /// GPU draws apart from the Macs the goldens were drawn on.
    #[default]
    Review,
    /// The changed frames of the tests that passed become the goldens, as a patch.
    Accept,
    /// Every changed frame becomes its golden, as a patch.
    AcceptAll,
}

impl Goldens {
    /// Its `xtask e2e` flag, without the dashes, which the workflow's `goldens` input takes.
    pub const fn flag(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Accept => "accept",
            Self::AcceptAll => "accept-all",
        }
    }
}

/// One hosted run to ask for.
#[derive(Debug)]
pub struct Request<'a> {
    /// The commit, which must be on GitHub (a branch's, or `check/<name>`'s while it lives).
    pub sha: &'a str,
    /// The kinds of run, each one job; every kind when empty.
    pub kinds: &'a [Kind],
    /// A nextest filterset narrowing every kind's suites.
    pub filter: Option<&'a str>,
    /// What the run does with the goldens.
    pub goldens: Goldens,
}

/// A dispatched run.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct Run {
    /// Its id, by which [`follow`] reads it.
    #[serde(rename = "databaseId")]
    pub id: u64,
    /// Its page.
    pub url: String,
}

/// How a run ended.
#[derive(Debug)]
pub struct Verdict {
    /// The run.
    pub run: Run,
    /// Each of its case jobs' name (`e2e (mac)`) and conclusion.
    pub jobs: Vec<(String, String)>,
    /// Where its artifacts came: one directory per case's (`e2e-mac`), with its changed frames'
    /// renders and diffs and, when it accepted any, `changes.patch`.
    pub artifacts: Utf8PathBuf,
}

impl Verdict {
    /// Whether every case ran and passed.
    pub fn passed(&self) -> bool {
        !self.jobs.is_empty() && self.jobs.iter().all(|(_, done)| done == "success")
    }

    /// The patches of the goldens the run accepted, one a case that accepted any.
    pub fn patches(&self) -> Vec<Utf8PathBuf> {
        let Ok(dirs) = self.artifacts.read_dir_utf8() else { return Vec::new() };
        let mut patches: Vec<Utf8PathBuf> = dirs
            .filter_map(Result::ok)
            .map(|dir| dir.path().join("changes.patch"))
            .filter(|patch| patch.is_file())
            .collect();
        patches.sort();
        patches
    }
}

/// Dispatch `request`'s run, follow it to its end, and bring its artifacts into `into`, which
/// is emptied first.
///
/// # Errors
///
/// When GitHub refuses the dispatch, the run never shows up or outlives [`FINISH`], or `gh`
/// cannot read it; not when a case fails, which the verdict says.
pub fn run(sh: &Shell, request: &Request<'_>, into: &Utf8Path) -> Result<Verdict> {
    let run = dispatch(sh, request)?;
    follow(sh, &run, into)
}

/// Dispatch `request`'s run on [`REF`] and find it among the workflow's runs by a mark of its
/// own in its name.
///
/// # Errors
///
/// When GitHub has no such commit, refuses the dispatch or lists no run with its mark within
/// [`FIND`].
pub fn dispatch(sh: &Shell, request: &Request<'_>) -> Result<Run> {
    let mark = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_millis())
    );
    let sha = request.sha;
    // A commit GitHub lacks would fail the run's checkout minutes later, after a runner took it.
    let commit = format!("repos/{{owner}}/{{repo}}/commits/{sha}");
    cmd!(sh, "gh api {commit} --silent").quiet().ignore_stderr().run().with_context(|| {
        format!("GitHub has no commit {sha}, or `gh` cannot ask: push it (a branch, or a check's)")
    })?;
    let a11y = std::env::var("SLOPTY_E2E_A11Y").unwrap_or_default();
    let fields = inputs(request, &mark, &a11y);
    let mut args: Vec<String> = Vec::new();
    for (name, value) in &fields {
        args.push("-f".to_owned());
        args.push(format!("{name}={value}"));
    }
    cmd!(sh, "gh workflow run {WORKFLOW} --ref {REF} {args...}")
        .quiet()
        .run()
        .with_context(|| format!("dispatch {WORKFLOW} on {REF} for {sha}"))?;
    let started = Instant::now();
    loop {
        let listed = cmd!(
            sh,
            "gh run list --workflow {WORKFLOW} --event workflow_dispatch --limit 30 --json databaseId,url,displayTitle"
        )
        .quiet()
        .read()
        .context("gh run list");
        match listed {
            Ok(listed) => {
                if let Some(run) = marked(&listed, &mark)? {
                    println!("▶ e2e {} on a hosted Mac: {}", what(request), run.url);
                    return Ok(run);
                }
            }
            Err(error) if started.elapsed() >= FIND => return Err(error),
            Err(_) => {}
        }
        ensure!(
            started.elapsed() < FIND,
            "no {WORKFLOW} run marked {mark} after {FIND:?}: is the workflow on?"
        );
        pause();
    }
}

/// Follow `run` to its end, saying each change of its jobs, and bring its artifacts into
/// `into`, emptied first.
///
/// # Errors
///
/// When `gh` cannot read the run [`MISSES`] times in a row, `into` cannot be emptied, or the run
/// outlives [`FINISH`]; not when it brings no artifact, as a run that changed no frame uploads
/// none.
pub fn follow(sh: &Shell, run: &Run, into: &Utf8Path) -> Result<Verdict> {
    #[derive(serde::Deserialize)]
    struct State {
        status: String,
        jobs: Vec<Job>,
    }
    #[derive(serde::Deserialize)]
    struct Job {
        name: String,
        status: String,
        conclusion: String,
    }
    let id = run.id.to_string();
    let started = Instant::now();
    let mut said = String::new();
    let mut missed: u32 = 0;
    let jobs = loop {
        let state = match cmd!(sh, "gh run view {id} --json status,jobs").quiet().read() {
            Ok(state) => state,
            Err(error) => {
                missed = missed.saturating_add(1);
                ensure!(missed < MISSES, "gh run view {id}, {missed} times in a row: {error:#}");
                pause();
                continue;
            }
        };
        missed = 0;
        let state: State = serde_json::from_str(&state).context("gh run view's JSON")?;
        let cases: Vec<&Job> = state.jobs.iter().filter(|j| j.name.starts_with("e2e (")).collect();
        let now = cases.iter().fold(String::new(), |mut now, job| {
            let state = if job.conclusion.is_empty() { &job.status } else { &job.conclusion };
            let _ok = write!(now, "{} {state}; ", job.name);
            now
        });
        if now != said {
            println!("  {}({:.0?})", now, started.elapsed());
            said = now;
        }
        if state.status == "completed" {
            break cases.into_iter().map(|j| (j.name.clone(), j.conclusion.clone())).collect();
        }
        ensure!(started.elapsed() < FINISH, "run {id} still going after {FINISH:?}: {}", run.url);
        pause();
    };
    if into.exists() {
        std::fs::remove_dir_all(into).with_context(|| format!("empty {into}"))?;
    }
    std::fs::create_dir_all(into).with_context(|| format!("create {into}"))?;
    // A run that changed no frame and accepted nothing uploads no artifact, which `gh` calls an
    // error: the directory then stays empty.
    let downloaded = cmd!(sh, "gh run download {id} --dir {into}").quiet().ignore_stderr().run();
    if downloaded.is_err() {
        println!("  no artifacts from run {id}");
    }
    Ok(Verdict { run: run.clone(), jobs, artifacts: into.to_owned() })
}

/// The workflow's inputs for `request`, marked `mark`.
fn inputs(request: &Request<'_>, mark: &str, a11y: &str) -> Vec<(&'static str, String)> {
    let kinds: &[Kind] =
        if request.kinds.is_empty() { &[Kind::Mac, Kind::Ios] } else { request.kinds };
    let names: Vec<String> = kinds.iter().map(|kind| format!("\"{}\"", kind.name())).collect();
    let mut fields = vec![
        ("sha", request.sha.to_owned()),
        ("cases", format!("[{}]", names.join(", "))),
        ("filter", request.filter.unwrap_or_default().to_owned()),
        ("goldens", request.goldens.flag().to_owned()),
        ("id", mark.to_owned()),
    ];
    // `SLOPTY_E2E_A11Y` as set here, which a hosted run cannot read otherwise; only when set,
    // so a dispatch never names an input the workflow on `gate` may not have yet.
    if !a11y.is_empty() {
        fields.push(("a11y", a11y.to_owned()));
    }
    fields
}

/// The run among `listed` (`gh run list`'s JSON) whose name ends with `(mark)`, as e2e.yml's
/// `run-name` writes it.
fn marked(listed: &str, mark: &str) -> Result<Option<Run>> {
    #[derive(serde::Deserialize)]
    struct Listed {
        #[serde(flatten)]
        run: Run,
        #[serde(rename = "displayTitle")]
        title: String,
    }
    let runs: Vec<Listed> = serde_json::from_str(listed).context("gh run list's JSON")?;
    let tail = format!("({mark})");
    Ok(runs.into_iter().find(|listed| listed.title.ends_with(&tail)).map(|listed| listed.run))
}

/// What `request` runs, for its first line.
fn what(request: &Request<'_>) -> String {
    let kinds: Vec<&str> = if request.kinds.is_empty() {
        vec!["mac", "ios"]
    } else {
        request.kinds.iter().map(|kind| kind.name()).collect()
    };
    let mut what =
        format!("{} at {}", kinds.join(" and "), request.sha.get(..10).unwrap_or(request.sha));
    if let Some(filter) = request.filter {
        let _ok = write!(what, ", {filter}");
    }
    if request.goldens != Goldens::Review {
        let _ok = write!(what, ", --{}", request.goldens.flag());
    }
    what
}

/// Wait between two questions to GitHub.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling GitHub")]
fn pause() {
    std::thread::sleep(POLL);
}

/// Fail unless `verdict` passed, naming each case that did not.
///
/// # Errors
///
/// When a case failed, was cancelled or never ran.
pub fn passed(verdict: &Verdict) -> Result<()> {
    if verdict.passed() {
        return Ok(());
    }
    let failed: Vec<String> = verdict
        .jobs
        .iter()
        .filter(|(_, done)| done != "success")
        .map(|(name, done)| format!("{name} {done}"))
        .collect();
    if failed.is_empty() {
        bail!("run {} ran no case: {}", verdict.run.id, verdict.run.url);
    }
    bail!("{}: {}", failed.join(", "), verdict.run.url)
}

#[cfg(test)]
mod tests {
    use super::{Goldens, Kind, Request, Run, Verdict, inputs, marked};

    /// The inputs name the cases as the matrix's JSON list, every case when none is named, and
    /// carry the filterset, the goldens' mode, the mark, and `SLOPTY_E2E_A11Y` when it is set.
    #[test]
    fn a_request_becomes_the_workflow_s_inputs() {
        let request = Request {
            sha: "abc",
            kinds: &[Kind::Ios],
            filter: Some("test(=a::b)"),
            goldens: Goldens::AcceptAll,
        };
        assert_eq!(
            inputs(&request, "7-1", ""),
            [
                ("sha", "abc".to_owned()),
                ("cases", r#"["ios"]"#.to_owned()),
                ("filter", "test(=a::b)".to_owned()),
                ("goldens", "accept-all".to_owned()),
                ("id", "7-1".to_owned()),
            ]
        );
        let every = Request { sha: "abc", kinds: &[], filter: None, goldens: Goldens::Review };
        let fields = inputs(&every, "m", "always");
        assert!(fields.contains(&("cases", r#"["mac", "ios"]"#.to_owned())));
        assert!(fields.contains(&("a11y", "always".to_owned())));
        // Every input a dispatch can name is one e2e.yml takes, and the variable reaches the run.
        let root = crate::tools::repo_root().expect("repo root");
        let workflow =
            std::fs::read_to_string(root.join(".github/workflows/e2e.yml")).expect("e2e.yml");
        for (name, _) in &fields {
            assert!(workflow.contains(&format!("\n      {name}:\n")), "e2e.yml takes `{name}`");
        }
        assert!(workflow.contains("SLOPTY_E2E_A11Y: ${{ inputs.a11y }}"), "and passes it on");
        assert!(fields.contains(&("filter", String::new())));
        assert!(fields.contains(&("goldens", "review".to_owned())));
    }

    /// The run found is the one whose name ends with the mark, as e2e.yml's `run-name` puts it.
    #[test]
    fn the_dispatched_run_is_found_by_its_mark() {
        let listed = r#"[
            {"databaseId": 1, "url": "u1", "displayTitle": "E2E: abc (9-10)"},
            {"databaseId": 2, "url": "u2", "displayTitle": "E2E: abc (9-1)"},
            {"databaseId": 3, "url": "u3", "displayTitle": "E2E nightly"}
        ]"#;
        assert_eq!(marked(listed, "9-1").unwrap(), Some(Run { id: 2, url: "u2".to_owned() }));
        assert_eq!(marked(listed, "9-2").unwrap(), None);
    }

    /// A run passes when every case it ran passed, and not when it ran none.
    #[test]
    fn a_run_passes_on_every_case_passed() {
        let verdict = |jobs: &[(&str, &str)]| Verdict {
            run: Run { id: 1, url: String::new() },
            jobs: jobs.iter().map(|(n, d)| ((*n).to_owned(), (*d).to_owned())).collect(),
            artifacts: "/nonexistent".into(),
        };
        assert!(verdict(&[("e2e (mac)", "success"), ("e2e (ios)", "success")]).passed());
        assert!(!verdict(&[("e2e (mac)", "success"), ("e2e (ios)", "failure")]).passed());
        assert!(!verdict(&[("e2e (mac)", "cancelled")]).passed());
        assert!(!verdict(&[]).passed(), "no case ran");
        assert!(super::passed(&verdict(&[("e2e (ios)", "failure")])).is_err());
        assert_eq!(verdict(&[]).patches(), Vec::<camino::Utf8PathBuf>::new(), "no artifacts");
    }
}
