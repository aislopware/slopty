//! `xtask warm guard`: the warm runners' job-started hook (`ACTIONS_RUNNER_HOOK_JOB_STARTED`).
//!
//! The repository is public, and a pull request's workflow runs on whatever runner its `runs-on`
//! names, its own edits to the workflow included. So the runner itself refuses every job but
//! the gate's: the warm job of a push to `gate` or to a `v*` tag running `ci.yml`, or of a
//! `check.yml` dispatch ([`WARM_JOBS`]), by one of the repository's own actors (those with push
//! access when the runner was set up, `guard/warm.toml`). A refused job fails before its first
//! step, so nothing of a pull request, a fork or another workflow ever runs here. The hook reads
//! only what the runner hands it: the job's `GITHUB_*` variables and the event's payload
//! (`GITHUB_EVENT_PATH`).
//!
//! It then proves the job can reach no credential of the person's ([`isolation`]): no SSH agent
//! in its environment or behind any login session's socket, no keychain, none of the person's
//! SSH keys, `gh` login or keychain files. A job that could fails as a refused one does.

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

/// The jobs of each workflow that name the warm runners: `ci.yml`'s and `check.yml`'s `warm`, and
/// `check.yml`'s `wake`, an empty Mac job whose start wakes the Linux runner. Any other job of
/// theirs is a hosted runner's, and one of `check.yml`'s (its probe's, its e2e case's) checks
/// out clean, which would delete the warm target dir.
const WARM_JOBS: [(&str, &[&str]); 2] = [("ci.yml", &["warm"]), ("check.yml", &["warm", "wake"])];

/// A job as the runner describes it to the hook.
#[derive(Debug, Default)]
pub struct Job {
    pub event: String,
    /// Its id in its workflow (`GITHUB_JOB`).
    pub id: String,
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
            id: var("GITHUB_JOB"),
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
    let warm = WARM_JOBS.iter().find(|(name, _)| *name == file).map_or(&[][..], |(_, jobs)| *jobs);
    if !warm.contains(&job.id.as_str()) {
        return Err(format!("{file}'s job {:?} is not one of its warm jobs {warm:?}", job.id));
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

/// Whether `job` has a half on the Linux runner, which its start wakes: a gate's push always, a
/// `check.yml` dispatch when its `warm` input asks for the Linux runner (`yes` or `linux`).
fn wakes(job: &Job) -> bool {
    match job.event.as_str() {
        "push" => true,
        "workflow_dispatch" => {
            let warm = job.payload.pointer("/inputs/warm").and_then(serde_json::Value::as_str);
            matches!(warm, Some("yes" | "linux"))
        }
        _ => false,
    }
}

/// The probes' exit codes that mean nothing answered: `ssh-add` found no agent behind a socket.
const NO_AGENT: i32 = 2;

/// The person's files that hold a credential, under each home but the runner's own: their SSH
/// keys, their `gh` login, their keychains.
const CREDENTIAL_FILES: [&str; 3] = [".ssh", ".config/gh", "Library/Keychains"];

/// What of the person's credentials the job could reach, each as the probe that reached it and
/// how it exited, never what it read; empty when none. A probe that cannot run counts as one that
/// reached, so a broken probe fails closed.
fn isolation() -> Vec<String> {
    let mut reached = Vec::new();
    if std::env::var_os("SSH_AUTH_SOCK").is_some() {
        reached.push("SSH_AUTH_SOCK is in the job's environment".to_owned());
    }
    if cfg!(target_os = "macos") {
        mac_isolation(&mut reached);
    }
    reached
}

/// [`isolation`]'s probes on a Mac: `ssh-add` on launchd's socket for every login session, where
/// the session's SSH agent listens; and in every person's home, `security` on their login
/// keychain, which answers its settings to whoever may open it, and the [`CREDENTIAL_FILES`].
fn mac_isolation(reached: &mut Vec<String>) {
    let mut sockets = Vec::new();
    for dir in ["/private/var/run", "/private/tmp"] {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if entry.file_name().to_string_lossy().starts_with("com.apple.launchd.") {
                sockets.push(entry.path().join("Listeners"));
            }
        }
    }
    for (index, socket) in sockets.iter().enumerate() {
        let code = exit_code("/usr/bin/ssh-add", &["-l"], Some(socket));
        if code != Some(NO_AGENT) {
            reached.push(format!("ssh-add -l on login session {index}'s socket exited {code:?}"));
        }
    }
    let listed = std::process::Command::new("/usr/bin/dscl")
        .args([".", "-list", "/Users", "NFSHomeDirectory"])
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(listed) = listed.map(|out| String::from_utf8_lossy(&out.stdout).into_owned()) else {
        reached.push("dscl could not list the homes".to_owned());
        return;
    };
    for home in homes(&listed) {
        let login = format!("{home}/Library/Keychains/login.keychain-db");
        let code = exit_code("/usr/bin/security", &["show-keychain-info", &login], None);
        if code.is_none_or(|code| code == 0) {
            reached
                .push(format!("security show-keychain-info on {home}'s keychain exited {code:?}"));
        }
        for file in CREDENTIAL_FILES {
            if std::fs::read_dir(std::path::Path::new(home).join(file)).is_ok() {
                reached.push(format!("{home}/{file} is readable"));
            }
        }
    }
}

/// The people's homes in `dscl`'s list of every account's (`name  home` lines): those under
/// `/Users` but the runner's own and the shared folder.
fn homes(listed: &str) -> Vec<&str> {
    listed
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter(|home| home.starts_with("/Users/"))
        .filter(|home| *home != super::mac::USER_HOME && *home != "/Users/Shared")
        .collect()
}

/// How `program` with `args` exited, nothing of its output kept, with `SSH_AUTH_SOCK` at `agent`
/// when one is named.
fn exit_code(program: &str, args: &[&str], agent: Option<&std::path::Path>) -> Option<i32> {
    use std::process::{Command, Stdio};

    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if let Some(agent) = agent {
        command.env("SSH_AUTH_SOCK", agent);
    }
    command.status().ok().and_then(|status| status.code())
}

/// The hook: the job in the environment held to `config` (`guard/warm.toml`); a refusal fails it.
/// An allowed job with a Linux half ([`wakes`]) sets `wake`'s time to now, where it names a file:
/// the Mac runner's, which wakes the Linux runner's VM (`super::linux::run`). A wake that fails
/// fails the job, since that half would otherwise wait for a runner that never comes.
pub fn run(config: &Utf8Path, wake: Option<&Utf8Path>) -> Result<()> {
    let text = std::fs::read_to_string(config).with_context(|| format!("read {config}"))?;
    let config: Config = toml::from_str(&text).with_context(|| format!("parse {config}"))?;
    let job = Job::from_env();
    let reached = isolation();
    if !reached.is_empty() {
        let reached = reached.join("; ");
        println!("::error title=warm runner::not isolated: {reached}");
        anyhow::bail!("this runner could reach the person's credentials: {reached}");
    }
    println!("warm runner: no SSH agent, keychain or credential file of the person's in reach");
    match verdict(&config, &job) {
        Ok(()) => {
            println!(
                "warm runner: {} of {}'s {} on {} by {}, allowed",
                job.event, job.workflow_ref, job.id, job.git_ref, job.actor
            );
            if let Some(wake) = wake.filter(|_| wakes(&job)) {
                super::touch(wake).context("wake the Linux runner's VM")?;
                println!("warm runner: the Linux runner's VM woken ({wake})");
            }
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
    use super::{Config, Job, WARM_JOBS, homes, verdict, wakes, workflow_file};

    const REPO: &str = "aislopware/slopty";

    fn config() -> Config {
        Config { repository: REPO.to_owned(), actors: vec!["trancong12102".to_owned()] }
    }

    /// The jobs the guard lets through are exactly those of each workflow that name a warm runner,
    /// so that renaming one never strands the gate.
    #[test]
    fn the_warm_jobs_are_the_workflows_own() {
        let root = crate::tools::repo_root().expect("the checkout");
        for (file, jobs) in WARM_JOBS {
            let path = root.join(".github/workflows").join(file);
            let text = std::fs::read_to_string(&path).expect("the workflow");
            let mut named = Vec::new();
            let mut current = "";
            for line in text.lines() {
                if let Some(id) = line.strip_prefix("  ").and_then(|l| l.strip_suffix(':'))
                    && !id.starts_with(' ')
                    && !id.contains(' ')
                {
                    current = id;
                }
                if line.trim_start().starts_with("runs-on:") && line.contains("slopty-warm") {
                    named.push(current);
                }
            }
            assert_eq!(named, jobs, "{file}");
        }
    }

    /// The homes probed are the people's, never the runner's own, a system account's or the
    /// shared folder.
    #[test]
    fn the_probed_homes_are_the_people_s() {
        let listed = "_www                 /Library/WebServer\n\
                      daemon               /var/root\n\
                      person               /Users/person\n\
                      second               /Users/second\n\
                      slopty-ci            /Users/slopty-ci\n\
                      nobody\n";
        assert_eq!(homes(listed), ["/Users/person", "/Users/second"]);
    }

    /// A gate's push wakes the Linux runner, and a check's dispatch when it asks for it.
    #[test]
    fn a_job_with_a_linux_half_wakes_the_vm() {
        assert!(wakes(&job("push", "ci.yml", "refs/heads/gate")));
        let dispatch = |warm: &str| {
            let mut check = job("workflow_dispatch", "check.yml", "refs/heads/check/x");
            check.payload["inputs"] = serde_json::json!({ "warm": warm });
            wakes(&check)
        };
        assert!(dispatch("yes") && dispatch("linux"));
        assert!(!dispatch("mac") && !dispatch("no"), "a Mac-only check or a probe does not");
    }

    fn job(event: &str, file: &str, git_ref: &str) -> Job {
        Job {
            event: event.to_owned(),
            id: "warm".to_owned(),
            git_ref: git_ref.to_owned(),
            repository: REPO.to_owned(),
            workflow_ref: format!("{REPO}/.github/workflows/{file}@{git_ref}"),
            actor: "trancong12102".to_owned(),
            triggering_actor: "trancong12102".to_owned(),
            head_ref: String::new(),
            payload: serde_json::json!({ "repository": { "full_name": REPO, "fork": false } }),
        }
    }

    /// The gate's pushes, a release tag's and a remote check's dispatch run here, by their warm
    /// jobs.
    #[test]
    fn the_gate_a_tag_and_a_check_run_here() {
        let config = config();
        assert_eq!(verdict(&config, &job("push", "ci.yml", "refs/heads/gate")), Ok(()));
        assert_eq!(verdict(&config, &job("push", "ci.yml", "refs/tags/v0.4.0")), Ok(()));
        let check = job("workflow_dispatch", "check.yml", "refs/heads/gate");
        assert_eq!(verdict(&config, &check), Ok(()));
        let elsewhere = job("workflow_dispatch", "check.yml", "refs/heads/check/R");
        assert_eq!(verdict(&config, &elsewhere), Ok(()), "a check's workflow from its own branch");
        let mut wake = job("workflow_dispatch", "check.yml", "refs/heads/check/R");
        wake.id = "wake".to_owned();
        assert_eq!(verdict(&config, &wake), Ok(()), "the empty job that wakes the VM");
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
        for (file, other) in [("check.yml", "run"), ("check.yml", "test"), ("ci.yml", "wake")] {
            let event = if file == "ci.yml" { "push" } else { "workflow_dispatch" };
            let mut named = job(event, file, "refs/heads/gate");
            named.id = other.to_owned();
            assert!(refused(&named), "{file}'s {other}, which a hosted runner takes");
        }
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
