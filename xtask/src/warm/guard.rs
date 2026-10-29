//! `xtask warm guard`: the warm runners' job-started hook (`ACTIONS_RUNNER_HOOK_JOB_STARTED`).
//!
//! The repository is public, and a pull request's workflow runs on whatever runner its `runs-on`
//! names, its own edits to the workflow included. So the runner itself refuses every job but
//! the gate's: a push to `gate` or to a `v*` tag running `ci.yml`, or a `check.yml` dispatch,
//! by one of the repository's own actors (those with push access when the runner was set up,
//! `guard/warm.toml`). A refused job fails before its first step, so nothing of a pull request,
//! a fork or another workflow ever runs here. The hook reads only what the runner hands it: the
//! job's `GITHUB_*` variables and the event's payload (`GITHUB_EVENT_PATH`).

use anyhow::{Context as _, Result};
use camino::Utf8Path;

/// What the guard holds every job to, as `xtask warm mac install` writes it from the repository.
#[derive(Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Config {
    /// `owner/name`.
    pub repository: String,
    /// The logins that may start a job here: the repository's collaborators with push access.
    pub actors: Vec<String>,
}

/// A job as the runner describes it to the hook.
#[derive(Debug, Default)]
pub struct Job {
    pub event: String,
    pub git_ref: String,
    pub repository: String,
    /// `owner/name/.github/workflows/<file>@<ref>`.
    pub workflow_ref: String,
    pub actor: String,
    pub triggering_actor: String,
    /// Set for a pull request's run alone.
    pub head_ref: String,
    /// The webhook payload.
    pub payload: serde_json::Value,
}

impl Job {
    /// The job this hook runs before, from the runner's environment; anything missing is empty,
    /// and so refused.
    fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).unwrap_or_default();
        let payload = std::env::var("GITHUB_EVENT_PATH")
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(serde_json::Value::Null);
        Self {
            event: var("GITHUB_EVENT_NAME"),
            git_ref: var("GITHUB_REF"),
            repository: var("GITHUB_REPOSITORY"),
            workflow_ref: var("GITHUB_WORKFLOW_REF"),
            actor: var("GITHUB_ACTOR"),
            triggering_actor: var("GITHUB_TRIGGERING_ACTOR"),
            head_ref: var("GITHUB_HEAD_REF"),
            payload,
        }
    }
}

/// The workflow file a job's `workflow_ref` names, when it is one of `repository`'s.
fn workflow_file<'a>(workflow_ref: &'a str, repository: &str) -> Option<&'a str> {
    let rest = workflow_ref.strip_prefix(repository)?.strip_prefix("/.github/workflows/")?;
    let (file, _ref) = rest.split_once('@')?;
    (!file.is_empty() && !file.contains('/')).then_some(file)
}

/// Why `job` may not run here, or `Ok` when it may.
pub fn verdict(config: &Config, job: &Job) -> Result<(), String> {
    let repo = config.repository.as_str();
    if job.repository != repo {
        return Err(format!("the job is {:?}'s, and this runner serves {repo}", job.repository));
    }
    let payload_repo = job.payload.pointer("/repository/full_name").and_then(|v| v.as_str());
    if payload_repo != Some(repo) {
        return Err(format!("the event's repository is {payload_repo:?}, not {repo}"));
    }
    if job.payload.pointer("/repository/fork").and_then(serde_json::Value::as_bool) == Some(true) {
        return Err("the event comes from a fork".to_owned());
    }
    if !job.head_ref.is_empty() || job.payload.get("pull_request").is_some() {
        return Err("a pull request's job runs on GitHub's runners, never here".to_owned());
    }
    let Some(file) = workflow_file(&job.workflow_ref, repo) else {
        return Err(format!("{:?} names no workflow of {repo}", job.workflow_ref));
    };
    let gate = job.git_ref == "refs/heads/gate" || job.git_ref.starts_with("refs/tags/v");
    let allowed = match (job.event.as_str(), file) {
        ("push", "ci.yml") => gate,
        ("workflow_dispatch", "check.yml") => true,
        _ => false,
    };
    if !allowed {
        return Err(format!(
            "{} of {} on {} is not the gate's: only a push to `gate` or a `v*` tag (ci.yml) or a \
             check.yml dispatch runs here",
            job.event, file, job.git_ref
        ));
    }
    for (role, login) in [("actor", &job.actor), ("triggering actor", &job.triggering_actor)] {
        if login.is_empty() && role == "triggering actor" {
            continue;
        }
        if !config.actors.iter().any(|a| a == login) {
            return Err(format!("the {role} {login:?} is not one of {repo}'s own"));
        }
    }
    Ok(())
}

/// The hook: the job in the environment held to `config` (`guard/warm.toml`); a refusal fails it.
pub fn run(config: &Utf8Path) -> Result<()> {
    let text = std::fs::read_to_string(config).with_context(|| format!("read {config}"))?;
    let config: Config = toml::from_str(&text).with_context(|| format!("parse {config}"))?;
    let job = Job::from_env();
    match verdict(&config, &job) {
        Ok(()) => {
            println!(
                "warm runner: {} of {} on {} by {}, allowed",
                job.event, job.workflow_ref, job.git_ref, job.actor
            );
            Ok(())
        }
        Err(why) => {
            println!("::error title=warm runner::refused: {why}");
            anyhow::bail!("this runner refuses the job: {why}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, Job, verdict, workflow_file};

    const REPO: &str = "aislopware/slopty";

    fn config() -> Config {
        Config { repository: REPO.to_owned(), actors: vec!["trancong12102".to_owned()] }
    }

    fn job(event: &str, file: &str, git_ref: &str) -> Job {
        Job {
            event: event.to_owned(),
            git_ref: git_ref.to_owned(),
            repository: REPO.to_owned(),
            workflow_ref: format!("{REPO}/.github/workflows/{file}@{git_ref}"),
            actor: "trancong12102".to_owned(),
            triggering_actor: "trancong12102".to_owned(),
            head_ref: String::new(),
            payload: serde_json::json!({ "repository": { "full_name": REPO, "fork": false } }),
        }
    }

    /// The gate's pushes, a release tag's and a remote check's dispatch run here.
    #[test]
    fn the_gate_a_tag_and_a_check_run_here() {
        let config = config();
        assert_eq!(verdict(&config, &job("push", "ci.yml", "refs/heads/gate")), Ok(()));
        assert_eq!(verdict(&config, &job("push", "ci.yml", "refs/tags/v0.4.0")), Ok(()));
        let check = job("workflow_dispatch", "check.yml", "refs/heads/gate");
        assert_eq!(verdict(&config, &check), Ok(()));
        let elsewhere = job("workflow_dispatch", "check.yml", "refs/heads/check/R");
        assert_eq!(verdict(&config, &elsewhere), Ok(()), "a check's workflow from its own branch");
    }

    /// A pull request, from a fork or not and whatever its workflow says, never runs here; nor
    /// does a push elsewhere, another workflow, another repository or an actor of no standing.
    #[test]
    fn a_pull_request_and_everything_else_is_refused() {
        let config = config();
        let refused = |job: &Job| verdict(&config, job).is_err();
        assert!(refused(&job("pull_request", "ci.yml", "refs/pull/7/merge")), "a pull request");
        assert!(refused(&job("pull_request_target", "ci.yml", "refs/heads/main")));
        let mut edited = job("push", "ci.yml", "refs/heads/gate");
        edited.head_ref = "feature".to_owned();
        assert!(refused(&edited), "a pull request's head ref");
        let mut carried = job("push", "ci.yml", "refs/heads/gate");
        carried.payload["pull_request"] = serde_json::json!({ "number": 7 });
        assert!(refused(&carried), "a payload that carries a pull request");
        assert!(refused(&job("push", "ci.yml", "refs/heads/main")), "a push to another branch");
        assert!(refused(&job("push", "check.yml", "refs/heads/gate")), "check.yml pushed");
        assert!(refused(&job("workflow_dispatch", "e2e.yml", "refs/heads/gate")), "e2e.yml");
        assert!(refused(&job("workflow_dispatch", "ci.yml", "refs/heads/gate")), "ci dispatched");
        assert!(refused(&job("schedule", "deep.yml", "refs/heads/main")), "a schedule");
        assert!(refused(&job("issue_comment", "check.yml", "refs/heads/main")), "a comment");
        let mut fork = job("push", "ci.yml", "refs/heads/gate");
        fork.payload["repository"]["fork"] = serde_json::json!(true);
        assert!(refused(&fork), "a fork's push");
        let mut other = job("push", "ci.yml", "refs/heads/gate");
        other.repository = "someone/slopty".to_owned();
        assert!(refused(&other), "another repository");
        let mut stranger = job("workflow_dispatch", "check.yml", "refs/heads/gate");
        stranger.actor = "stranger".to_owned();
        assert!(refused(&stranger), "an actor without push access");
        let mut rerun = job("push", "ci.yml", "refs/heads/gate");
        rerun.triggering_actor = "stranger".to_owned();
        assert!(refused(&rerun), "a re-run by an actor without push access");
        assert!(refused(&Job::default()), "nothing known is nothing allowed");
    }

    /// A workflow is named by its file in this repository's `.github/workflows`, never by a
    /// file elsewhere or in a subdirectory.
    #[test]
    fn a_workflow_is_its_file_in_this_repository() {
        fn named(workflow_ref: &str) -> Option<&str> {
            workflow_file(workflow_ref, REPO)
        }
        assert_eq!(
            named("aislopware/slopty/.github/workflows/ci.yml@refs/heads/gate"),
            Some("ci.yml")
        );
        assert_eq!(named("someone/slopty/.github/workflows/ci.yml@refs/heads/gate"), None);
        assert_eq!(named("aislopware/slopty/.github/workflows/x/ci.yml@refs/heads/gate"), None);
        assert_eq!(named("aislopware/slopty/other/ci.yml@refs/heads/gate"), None);
        assert_eq!(named(""), None);
    }
}
