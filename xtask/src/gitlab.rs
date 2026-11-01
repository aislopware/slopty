//! `cargo xtask gitlab`: Slopty's macOS jobs on jmango360's two office Macs, GitLab runners 834
//! and 842 on `gitlab.jmango360.com` (tart's custom executor), only while they are idle. Their
//! owners' iOS builds come first: a job of Slopty's starts on a Mac that runs nothing and has
//! nothing waiting for it, and gives the Mac back the moment another project's job waits for it
//! or runs on it. The caller then runs the job on GitHub's hosted runners instead. The gate does
//! not call it yet: a fresh guest's build comes over the Mac mini's 100 Mbit/s port, slower than
//! a hosted runner's, until a runner keeps a build between jobs.
//!
//! - `idle` says which Macs are idle now (with `--github-output`, for a GitHub job routing to one).
//! - `run` triggers a pipeline of `cong.tran/slopty-ci` for one commit on an idle Mac, follows its
//!   job to its verdict with the job's log, cancels it the moment the Mac is wanted (or when the
//!   Mac does not take it, or its guest does not come up), and says whether the caller falls back.
//! - `job` runs inside the guest: the commit's xtask commands, one after another.
//! - `guard` runs inside the guest, built into the image, and runs the job's steps under watch: it
//!   ends them and all they started the moment the Mac is wanted, in seconds, and yields (exit
//!   [`YIELDED`]), so the Mac comes back even when the caller is gone. GitLab's cancel reaches the
//!   runner only with its next log update, every 60 s by default (`ci_job_trace_update_interval`);
//!   a jmango360 job waited 31 s behind one.
//! - `warm` brings the image onto an idle Mac (a first pull is an hour or more), preemptibly too.
//! - `publish` makes `gitlab/ci.yml` the GitLab project's pipeline.
//!
//! What a Mac's runner does comes from GitLab's GraphQL API in one request ([`Runner`]): the jobs
//! it runs and the pending jobs it would take (`jobs(compatibleRunnerId:)`), by id. A token with
//! only `read_runner` on the instance sees those ids but not their projects, so a job is the
//! caller's own by its id, and the project's other jobs by the project's own list
//! ([`Gitlab::active`]). Tokens come from the environment, each with one power:
//! `GITLAB_READ_TOKEN` (the runners, and the project's pipelines, jobs, logs and artifacts),
//! `GITLAB_TRIGGER_TOKEN` (the project's pipeline trigger) and `GITLAB_CANCEL_TOKEN` (the
//! project's jobs); curl gets them on stdin, never on its command line.
//! `docs/decisions/tooling.md`, "The office Macs run Slopty's jobs while idle", says why.

use std::fmt::Write as _;
use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use camino::{Utf8Path, Utf8PathBuf};
use clap::{Args, Subcommand};
use serde::Deserialize;

/// The GitLab instance.
const GITLAB: &str = "https://gitlab.jmango360.com";

/// The project whose pipelines run Slopty's jobs, `cong.tran/slopty-ci`: it holds [`CI`] alone.
const PROJECT: u64 = 2131;

/// The project's pipeline, published by `cargo xtask gitlab publish`.
const CI: &str = include_str!("gitlab/ci.yml");

/// The job [`CI`] defines.
const JOB: &str = "slopty";

/// An office Mac: its GitLab runner, the tag that sends a job to it alone, and what it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mac {
    /// The runner's id.
    pub runner: u64,
    /// The runner's tag: a job with it goes to this Mac alone.
    pub tag: &'static str,
    /// The machine, and the guest it gives a job.
    pub what: &'static str,
}

/// The office Macs, the faster first: `run` takes the first idle one.
pub const MACS: [Mac; 2] = [
    Mac {
        runner: 842,
        tag: "office-mac-runner-02",
        what: "MacBook Pro M5 Max (a 16-core, 44 GB guest)",
    },
    Mac { runner: 834, tag: "office-mac-runner", what: "Mac mini M4 Pro (a 14-core, 48 GB guest)" },
];

/// How often the queue and the job are read: a waiting jmango360 job waits at most this, the
/// cancel and the guest's teardown.
const POLL: Duration = Duration::from_secs(3);

/// How often the guard sees whether its work has ended: a job ends this long after its steps.
const TICK: Duration = Duration::from_millis(100);

/// How often a running job's log is read and shown: its whole log comes each time.
const LOG_EVERY: Duration = Duration::from_secs(15);

/// How long a dispatched job may wait for its Mac, idle a moment before: one still waiting was
/// beaten to the Mac, or the runner is down.
const PENDING_LIMIT: Duration = Duration::from_secs(30);

/// How long the executor may take to start the guest (clone, boot, SSH: about 10 s when the Mac
/// holds the image). One without the image pulls it first, an hour or more, which no caller waits
/// for: `warm` brings it.
const PREPARE_LIMIT: Duration = Duration::from_secs(120);

/// What the executor logs once the guest is up.
const READY: &str = "VM is ready.";

/// The exit status of a job that gave its Mac back or could not start (`EX_TEMPFAIL`): the caller
/// runs the job elsewhere. [`CI`] allows it.
pub const YIELDED: i32 = 75;

/// Where the CI image keeps its own xtask, which runs the guard (`xtask vm ci-image`).
pub const GUARD: &str = "/usr/local/bin/slopty-xtask";

/// The line a job that yields logs before it ends, then why.
const YIELD_MARK: &str = "slopty-gitlab: yielded";

/// The variables a trigger sets, which [`CI`] reads.
const VAR_TAG: &str = "SLOPTY_TAG";
const VAR_SHA: &str = "SLOPTY_SHA";
const VAR_ARGS: &str = "SLOPTY_ARGS";

/// The tokens, by the environment variable each is in.
const READ_TOKEN: &str = "GITLAB_READ_TOKEN";
const TRIGGER_TOKEN: &str = "GITLAB_TRIGGER_TOKEN";
const CANCEL_TOKEN: &str = "GITLAB_CANCEL_TOKEN";
/// The person's own API token, which `publish` commits with.
const PUBLISH_TOKEN: &str = "JMANGO_GITLAB_API_PAT";

/// `cargo xtask gitlab`.
#[derive(Subcommand, Debug)]
pub enum GitlabCmd {
    /// Which office Macs are idle now, the faster first.
    Idle {
        /// Write `mac` (the first idle Mac's tag, or empty) and `macs` (every idle one's) to
        /// `$GITHUB_OUTPUT`.
        #[arg(long)]
        github_output: bool,
    },
    /// Run xtask commands for a commit on an idle office Mac and follow them to their verdict. A
    /// Mac wanted by another project's job, or none idle, is a fallback: exit 75, or with
    /// `--github-output` exit 0 with `fallback=true`.
    Run(RunOpts),
    /// Bring the image onto an idle office Mac: its guest pulled and booted once, preemptibly.
    Warm {
        /// The Mac, by its runner's id or tag; by default the first idle one.
        #[arg(long)]
        mac: Option<String>,
    },
    /// Inside the guest: the commands in `SLOPTY_ARGS`, one per line, one after another.
    Job {
        /// Copy the runs' reports (`JUnit`, nextest's pseudo-terminal log, cargo's timings, the
        /// e2e's records) here once they end.
        #[arg(long)]
        reports: Option<Utf8PathBuf>,
    },
    /// Inside the guest, the image's own build: run the job's steps and end them the moment
    /// another project's job wants this Mac (exit 75).
    Guard {
        /// The steps' command and its arguments.
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Make `xtask/src/gitlab/ci.yml` the project's `.gitlab-ci.yml` (with
    /// `JMANGO_GITLAB_API_PAT`).
    Publish,
}

/// `cargo xtask gitlab run`.
#[derive(Args, Debug)]
pub struct RunOpts {
    /// The commit of `github.com/aislopware/slopty`.
    #[arg(long)]
    pub sha: String,
    /// An xtask command line (`gate --ci --lane tests`); repeat for several, run in turn.
    #[arg(long = "job", required = true)]
    pub jobs: Vec<String>,
    /// The Mac, by its runner's id or tag; by default the first idle one.
    #[arg(long)]
    pub mac: Option<String>,
    /// A variable for the job, `KEY=VALUE`; repeat for several. Trigger variables win over the
    /// pipeline's: `SLOPTY_IMAGE=<ref>` runs another image than `gitlab/ci.yml` pins,
    /// `TART_EXECUTOR_HOST_DIR=true` puts the builds directory on the host.
    #[arg(long = "var", value_parser = parse_var)]
    pub vars: Vec<(String, String)>,
    /// Unpack the job's artifacts (its reports) here.
    #[arg(long)]
    pub artifacts: Option<Utf8PathBuf>,
    /// Write `fallback`, `mac`, `job` and `why` to `$GITHUB_OUTPUT`, and exit 0 on a fallback.
    #[arg(long)]
    pub github_output: bool,
}

/// `cargo xtask gitlab`.
pub fn run(cmd: &GitlabCmd) -> Result<()> {
    match cmd {
        GitlabCmd::Idle { github_output } => idle(*github_output),
        GitlabCmd::Run(opts) => run_job(opts),
        GitlabCmd::Warm { mac } => warm(mac.as_deref()),
        GitlabCmd::Job { reports } => job(reports.as_deref()),
        GitlabCmd::Guard { command } => guard_command(command),
        GitlabCmd::Publish => publish(),
    }
}

/// The Macs `named` names (its runner's id or tag), or all of them.
fn candidates(named: Option<&str>) -> Result<Vec<Mac>> {
    let Some(named) = named else { return Ok(MACS.to_vec()) };
    let mac = MACS
        .iter()
        .find(|mac| mac.tag == named || mac.runner.to_string() == named)
        .with_context(|| format!("no office Mac {named}: {}", tags(&MACS)))?;
    Ok(vec![*mac])
}

/// The Macs' tags, space-separated.
fn tags(macs: &[Mac]) -> String {
    macs.iter().map(|mac| mac.tag).collect::<Vec<_>>().join(" ")
}

/// A Mac's runner at one moment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Runner {
    /// It is online and not paused: it takes jobs.
    takes: bool,
    /// The jobs it runs, and their number (which counts any the answer left out).
    running: Vec<u64>,
    running_count: u64,
    /// The pending jobs it would take.
    pending: Vec<u64>,
    pending_count: u64,
}

impl Runner {
    /// Free for a job of Slopty's: it takes jobs, runs none and none waits for it.
    const fn idle(&self) -> bool {
        self.takes && self.running_count == 0 && self.pending_count == 0
    }

    /// How many jobs other than `ours` and the project's `own` run on it or wait for it.
    fn others(&self, ours: u64, own: &[u64]) -> u64 {
        let known = |ids: &[u64]| {
            u64::try_from(ids.iter().filter(|id| **id == ours || own.contains(*id)).count())
                .unwrap_or(u64::MAX)
        };
        self.running_count
            .saturating_sub(known(&self.running))
            .saturating_add(self.pending_count.saturating_sub(known(&self.pending)))
    }

    /// One line on it, for the log.
    fn describe(&self) -> String {
        if !self.takes {
            return "offline or paused".to_owned();
        }
        if self.idle() {
            return "idle".to_owned();
        }
        format!("busy: {} running, {} waiting", self.running_count, self.pending_count)
    }
}

/// The GraphQL question [`Runner`] answers for runner `id`.
fn runner_query(id: u64) -> String {
    let runner = format!("\"gid://gitlab/Ci::Runner/{id}\"");
    format!(
        "{{ p: jobs(statuses: [PENDING], compatibleRunnerId: {runner}) {{ count nodes {{ id }} }} \
         r: runner(id: {runner}) {{ status paused jobs(statuses: [RUNNING]) {{ count nodes {{ id }} }} }} }}"
    )
}

#[derive(Deserialize)]
struct Answer {
    data: Option<AnswerData>,
    #[serde(default)]
    errors: Vec<AnswerError>,
}

#[derive(Deserialize)]
struct AnswerError {
    message: String,
}

#[derive(Deserialize)]
struct AnswerData {
    p: Option<Connection>,
    r: Option<RunnerNode>,
}

#[derive(Deserialize)]
struct Connection {
    count: u64,
    nodes: Vec<Node>,
}

#[derive(Deserialize)]
struct Node {
    id: String,
}

#[derive(Deserialize)]
struct RunnerNode {
    status: String,
    #[serde(default)]
    paused: bool,
    jobs: Option<Connection>,
}

/// [`runner_query`]'s answer.
fn parse_runner(body: &str) -> Result<Runner> {
    let answer: Answer = serde_json::from_str(body).context("GitLab's answer on a runner")?;
    if let Some(error) = answer.errors.first() {
        bail!("GitLab: {}", error.message);
    }
    let data = answer.data.context("GitLab answered no data")?;
    let pending = data.p.context("no pending jobs in GitLab's answer")?;
    let runner = data.r.context("GitLab shows no such runner to this token")?;
    let running = runner.jobs.context("no running jobs in GitLab's answer")?;
    let ids = |nodes: &[Node]| -> Result<Vec<u64>> {
        nodes
            .iter()
            .map(|node| {
                node.id
                    .rsplit('/')
                    .next()
                    .and_then(|id| id.parse().ok())
                    .with_context(|| format!("a job id: {}", node.id))
            })
            .collect()
    };
    Ok(Runner {
        takes: runner.status == "ONLINE" && !runner.paused,
        running: ids(&running.nodes)?,
        running_count: running.count,
        pending: ids(&pending.nodes)?,
        pending_count: pending.count,
    })
}

/// A job of the project, as its REST API shows it.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct Job {
    id: u64,
    name: String,
    status: String,
    web_url: String,
    failure_reason: Option<String>,
}

#[derive(Deserialize)]
struct Pipeline {
    id: u64,
}

/// What `run`, `warm` and `job` ask of GitLab, and their clock: a trait, so the tests script
/// GitLab's answers and the order things happen in.
trait Gitlab {
    /// `mac`'s runner now.
    fn runner(&mut self, mac: Mac) -> Result<Runner>;
    /// The ids of the project's jobs that wait or run.
    fn active(&mut self) -> Result<Vec<u64>>;
    /// A pipeline with these variables; its id.
    fn trigger(&mut self, vars: &[(&str, &str)]) -> Result<u64>;
    /// `pipeline`'s jobs.
    fn jobs(&mut self, pipeline: u64) -> Result<Vec<Job>>;
    fn job(&mut self, id: u64) -> Result<Job>;
    /// The job's log so far.
    fn trace(&mut self, id: u64) -> Result<String>;
    fn cancel(&mut self, id: u64) -> Result<()>;
    /// The time since an arbitrary start.
    fn now(&self) -> Duration;
    /// Wait `span`; an error once a signal asked this process to end.
    fn pause(&mut self, span: Duration) -> Result<()>;
}

/// Why a job ended as it did.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    Passed,
    Failed,
    /// It gave the Mac back, or never got one: the caller runs it elsewhere.
    Yielded(String),
}

/// What a dispatch came to: the Mac and job, when it got that far.
#[derive(Debug)]
struct Outcome {
    mac: Option<Mac>,
    job: Option<Job>,
    verdict: Verdict,
}

/// What to dispatch.
#[derive(Debug)]
struct Plan<'a> {
    /// The commit, or none to only bring the image onto the Mac.
    sha: Option<&'a str>,
    /// The xtask command lines.
    commands: &'a [String],
    /// The Macs it may go to, in order.
    macs: &'a [Mac],
    /// Variables beside the plan's own.
    vars: &'a [(String, String)],
    /// How long the guest may take to come up; none for `warm`, which pulls.
    prepare: Option<Duration>,
}

/// Dispatch `plan` to the first idle Mac and follow it to its verdict.
fn dispatch(
    api: &mut impl Gitlab,
    plan: &Plan<'_>,
    out: &mut impl std::io::Write,
) -> Result<Outcome> {
    let mut chosen = None;
    for &mac in plan.macs {
        let runner = api.runner(mac)?;
        writeln!(out, "{} (runner {}, {}): {}", mac.tag, mac.runner, mac.what, runner.describe())?;
        if runner.idle() {
            chosen = Some(mac);
            break;
        }
    }
    let Some(mac) = chosen else {
        return Ok(Outcome {
            mac: None,
            job: None,
            verdict: Verdict::Yielded("no office Mac is idle".to_owned()),
        });
    };
    let args = plan.commands.join("\n");
    let mut vars = vec![
        (VAR_TAG, mac.tag),
        (VAR_SHA, plan.sha.unwrap_or_default()),
        (VAR_ARGS, args.as_str()),
    ];
    vars.extend(plan.vars.iter().map(|(key, value)| (key.as_str(), value.as_str())));
    let pipeline = api.trigger(&vars)?;
    let job = pipeline_job(api, pipeline)?;
    writeln!(out, "▶ {} on {}: {}", plan.commands.join("; "), mac.tag, job.web_url)?;
    let verdict = follow(api, mac, &job, plan.prepare, out)?;
    Ok(Outcome { mac: Some(mac), job: Some(job), verdict })
}

/// The job of a new pipeline, once GitLab has made it.
fn pipeline_job(api: &mut impl Gitlab, pipeline: u64) -> Result<Job> {
    for _ in 0..10 {
        if let Some(job) = api.jobs(pipeline)?.into_iter().find(|job| job.name == JOB) {
            return Ok(job);
        }
        api.pause(Duration::from_secs(1))?;
    }
    bail!(
        "pipeline {pipeline} has no `{JOB}` job: is gitlab/ci.yml published (`cargo xtask gitlab publish`)?"
    )
}

/// Follow `job` on `mac` to its verdict, printing its log as it comes, and cancel it the moment
/// another project's job wants the Mac, when the Mac leaves it waiting past [`PENDING_LIMIT`], or
/// when its guest is not up within `prepare`. A signal to this process cancels it too.
fn follow(
    api: &mut impl Gitlab,
    mac: Mac,
    job: &Job,
    prepare: Option<Duration>,
    out: &mut impl std::io::Write,
) -> Result<Verdict> {
    let started = api.now();
    let mut running_since = None;
    let mut ready = false;
    let mut shown = 0;
    let mut logged: Option<Duration> = None;
    loop {
        let state = api.job(job.id)?;
        let waiting =
            matches!(state.status.as_str(), "created" | "pending" | "waiting_for_resource");
        let at = api.now();
        let due = !ready || logged.is_none_or(|then| at.saturating_sub(then) >= LOG_EVERY);
        if !waiting && (ended(&state) || due) {
            let trace = api.trace(job.id)?;
            logged = Some(at);
            shown = show(&trace, shown, out)?;
            ready = ready || trace.contains(READY);
            if let Some(verdict) = verdict(&state, &trace) {
                return Ok(verdict);
            }
        }
        // Asked after the job's state, so a job that ended is never cancelled as wanted.
        if let Some(why) = wanted(api, mac, job.id)? {
            api.cancel(job.id)?;
            writeln!(out, "✘ gave {} back: {why}", mac.tag)?;
            return Ok(Verdict::Yielded(why));
        }
        let why = if waiting {
            (at.saturating_sub(started) > PENDING_LIMIT)
                .then(|| format!("{} did not take it in {PENDING_LIMIT:?}", mac.tag))
        } else {
            let since = *running_since.get_or_insert(at);
            prepare.filter(|limit| !ready && at.saturating_sub(since) > *limit).map(|limit| {
                format!(
                    "no guest on {} in {limit:?}: the image is not on it yet (`cargo xtask gitlab \
                     warm --mac {}`)",
                    mac.tag, mac.tag
                )
            })
        };
        if let Some(why) = why {
            api.cancel(job.id)?;
            writeln!(out, "✘ {why}")?;
            return Ok(Verdict::Yielded(why));
        }
        if let Err(e) = api.pause(POLL) {
            api.cancel(job.id)?;
            return Err(e.context(format!("cancelled {}", job.web_url)));
        }
    }
}

/// Why `mac` is wanted by a job other than `ours` and the project's own, if it is.
fn wanted(api: &mut impl Gitlab, mac: Mac, ours: u64) -> Result<Option<String>> {
    let runner = api.runner(mac)?;
    if runner.others(ours, &[]) == 0 {
        return Ok(None);
    }
    let own = api.active()?;
    let others = runner.others(ours, &own);
    Ok((others > 0).then(|| format!("{others} job(s) of another project want {}", mac.tag)))
}

/// Whether `job` has ended.
fn ended(job: &Job) -> bool {
    matches!(
        job.status.as_str(),
        "success" | "failed" | "canceled" | "canceling" | "skipped" | "manual"
    )
}

/// The verdict of a job in `status` with `trace`, once it has ended.
fn verdict(job: &Job, trace: &str) -> Option<Verdict> {
    match job.status.as_str() {
        "success" => Some(Verdict::Passed),
        "failed" => {
            // The mark at a line's start: GitLab echoes each line of the script first, the
            // definition of the script's own `yield` among them.
            if let Some(line) = trace.lines().map(untimed).find(|line| line.starts_with(YIELD_MARK))
            {
                let why = line.split_once(YIELD_MARK).map_or("", |(_, why)| why);
                return Some(Verdict::Yielded(why.trim_start_matches(':').trim().to_owned()));
            }
            match job.failure_reason.as_deref() {
                Some("script_failure") | None => Some(Verdict::Failed),
                Some(reason) => Some(Verdict::Yielded(format!("GitLab ended it: {reason}"))),
            }
        }
        "canceled" | "canceling" => Some(Verdict::Yielded("cancelled on GitLab".to_owned())),
        "skipped" | "manual" => Some(Verdict::Yielded(format!("GitLab left it {}", job.status))),
        _ => None,
    }
}

/// Print `trace` past byte `shown`, whole lines only, without the runner's timestamps; how far
/// it has been shown.
fn show(trace: &str, shown: usize, out: &mut impl std::io::Write) -> Result<usize> {
    let Some(new) = trace.get(shown..) else { return Ok(shown) };
    let Some(end) = new.rfind('\n') else { return Ok(shown) };
    let whole = new.get(..=end).unwrap_or_default();
    let mut text = String::with_capacity(whole.len());
    for line in whole.lines() {
        text.push_str(untimed(line));
        text.push('\n');
    }
    out.write_all(text.as_bytes())?;
    out.flush()?;
    Ok(shown.saturating_add(end).saturating_add(1))
}

/// `line` without the stamp the runner puts before each line of a log
/// (`2026-10-31T11:41:26.412686Z 00O `, `+` in place of the space on a line's continuation).
fn untimed(line: &str) -> &str {
    let stamp = line.as_bytes();
    let stamped = stamp.len() >= 32
        && stamp.get(26) == Some(&b'Z')
        && stamp.get(27) == Some(&b' ')
        && matches!(stamp.get(30), Some(b'O' | b'E'))
        && matches!(stamp.get(31), Some(b' ' | b'+'));
    if stamped { line.get(32..).unwrap_or(line) } else { line }
}

/// The work a guarded job does: polled, and stopped when the Mac is wanted.
trait Work {
    /// Its exit status, once it has ended.
    fn poll(&mut self) -> Result<Option<i32>>;
    /// End it now.
    fn stop(&mut self);
}

/// Run `work` until it ends (its exit status, seen within a [`TICK`]), unless another project's
/// job wants `mac` first (asked every [`POLL`]): then stop it, log [`YIELD_MARK`] and say so
/// (`None`). A question GitLab does not answer is
/// logged, never a reason to yield: the caller cancels the job if it must.
fn guard(
    api: &mut impl Gitlab,
    mac: Mac,
    ours: u64,
    work: &mut impl Work,
    out: &mut impl std::io::Write,
) -> Result<Option<i32>> {
    let mut asked: Option<Duration> = None;
    loop {
        if let Some(status) = work.poll()? {
            return Ok(Some(status));
        }
        let now = api.now();
        if asked.is_none_or(|at| now.saturating_sub(at) >= POLL) {
            asked = Some(now);
            match wanted(api, mac, ours) {
                Ok(Some(why)) => {
                    writeln!(out, "{YIELD_MARK}: {why}")?;
                    out.flush()?;
                    work.stop();
                    return Ok(None);
                }
                Ok(None) => {}
                Err(e) => {
                    writeln!(out, "slopty-gitlab: could not read {}'s queue: {e:#}", mac.tag)?;
                }
            }
        }
        api.pause(TICK)?;
    }
}

/// The xtask command lines in `lines`, one per line, each split on whitespace.
fn command_lines(lines: &str) -> Vec<Vec<String>> {
    lines
        .lines()
        .map(|line| line.split_whitespace().map(str::to_owned).collect::<Vec<_>>())
        .filter(|args| !args.is_empty())
        .collect()
}

/// Run `xtask` with each of `commands` in turn, a failed one no reason to skip the next; the
/// ones that failed.
fn run_steps(xtask: &Utf8Path, commands: &[Vec<String>]) -> Result<Vec<String>> {
    let mut failed = Vec::new();
    for args in commands {
        let line = args.join(" ");
        println!("▶ xtask {line}");
        let started = Instant::now();
        let status =
            Command::new(xtask).args(args).status().with_context(|| format!("xtask {line}"))?;
        let mark = if status.success() { "✓" } else { "✘" };
        println!("{mark} xtask {line} ({:.0?}, {status})", started.elapsed());
        if !status.success() {
            failed.push(line);
        }
    }
    Ok(failed)
}

/// A command the guard runs, and ends with every process it started when the Mac is wanted.
#[derive(Debug)]
struct Supervised {
    child: std::process::Child,
}

impl Work for Supervised {
    fn poll(&mut self) -> Result<Option<i32>> {
        Ok(self.child.try_wait()?.map(code))
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "xtask is a script, not a library; a grace period"
    )]
    fn stop(&mut self) {
        let root = self.child.id();
        // The whole tree, not a process group: the e2e harness starts its apps in groups of their
        // own, and one left holding the log's pipe would keep the job's SSH session open.
        let mut tree = process_tree(root);
        signal_all(&tree, libc::SIGTERM);
        for _ in 0..20 {
            if matches!(self.child.try_wait(), Ok(Some(_)) | Err(_)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        tree.extend(process_tree(root));
        signal_all(&tree, libc::SIGKILL);
        let _status: std::io::Result<_> = self.child.wait();
    }
}

/// An exit status as a shell reports it: the code, or 128 and the signal that ended it.
fn code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt as _;
    status.code().or_else(|| status.signal().map(|signal| signal.saturating_add(128))).unwrap_or(1)
}

/// `root` and every process under it, by `ps`; just `root` if `ps` cannot say.
fn process_tree(root: u32) -> Vec<u32> {
    let table = Command::new("/bin/ps")
        .args(["-A", "-o", "pid=", "-o", "ppid="])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    descendants(&table, root)
}

/// `root` and every process under it in `table`, `ps`'s `pid ppid` lines.
fn descendants(table: &str, root: u32) -> Vec<u32> {
    let parents: Vec<(u32, u32)> = table
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace().map(str::parse::<u32>);
            Some((fields.next()?.ok()?, fields.next()?.ok()?))
        })
        .collect();
    let mut tree = vec![root];
    let mut next = 0_usize;
    while let Some(&parent) = tree.get(next) {
        next = next.saturating_add(1);
        let children: Vec<u32> = parents
            .iter()
            .filter(|&&(pid, ppid)| ppid == parent && !tree.contains(&pid))
            .map(|&(pid, _)| pid)
            .collect();
        tree.extend(children);
    }
    tree
}

/// Send `signal` to each of `pids`; one already gone is no matter.
fn signal_all(pids: &[u32], signal: libc::c_int) {
    for &pid in pids {
        if let Ok(pid) = libc::pid_t::try_from(pid) {
            // SAFETY: kill(2) only sends a signal; one to a process that is gone returns an error.
            let _sent: libc::c_int = unsafe { libc::kill(pid, signal) };
        }
    }
}

/// `KEY=VALUE`, as `--var` takes it.
fn parse_var(text: &str) -> Result<(String, String)> {
    let (key, value) = text.split_once('=').context("a variable is KEY=VALUE")?;
    ensure!(!key.is_empty(), "a variable needs a name: {text}");
    Ok((key.to_owned(), value.to_owned()))
}

/// `cargo xtask gitlab idle`.
fn idle(github_output: bool) -> Result<()> {
    let mut api = Api::new();
    let mut idle = Vec::new();
    for mac in MACS {
        // A Mac that cannot be asked is not idle: the caller's plan never fails for it.
        let (said, free) = match api.runner(mac) {
            Ok(runner) => (runner.describe(), runner.idle()),
            Err(e) => (format!("not asked: {e:#}"), false),
        };
        println!("{} (runner {}, {}): {said}", mac.tag, mac.runner, mac.what);
        if free {
            idle.push(mac);
        }
    }
    if github_output {
        let first = idle.first().map_or("", |mac| mac.tag);
        outputs(&[("mac", first), ("macs", &tags(&idle))])?;
    }
    Ok(())
}

/// `cargo xtask gitlab run`.
fn run_job(opts: &RunOpts) -> Result<()> {
    watch_signals()?;
    let macs = candidates(opts.mac.as_deref())?;
    let plan = Plan {
        sha: Some(&opts.sha),
        commands: &opts.jobs,
        macs: &macs,
        vars: &opts.vars,
        prepare: Some(PREPARE_LIMIT),
    };
    let mut api = Api::new();
    let outcome = match dispatch(&mut api, &plan, &mut std::io::stdout()) {
        Ok(outcome) => outcome,
        // On GitHub a Mac that could not be asked or reached is one more reason to run there.
        Err(e) if opts.github_output => {
            Outcome { mac: None, job: None, verdict: Verdict::Yielded(format!("{e:#}")) }
        }
        Err(e) => return Err(e),
    };
    if let (Some(dir), Some(job), Verdict::Passed | Verdict::Failed) =
        (&opts.artifacts, &outcome.job, &outcome.verdict)
        && let Err(e) = api.artifacts(job.id, dir)
    {
        println!("no artifacts from {}: {e:#}", job.web_url);
    }
    let url = outcome.job.as_ref().map_or("", |job| job.web_url.as_str());
    let tag = outcome.mac.map_or("", |mac| mac.tag);
    let took = format!("{:.0?}", api.now());
    let (fallback, why) = match &outcome.verdict {
        Verdict::Passed => ("false", format!("passed on {tag} in {took}")),
        Verdict::Failed => ("false", format!("failed on {tag} after {took}")),
        Verdict::Yielded(why) => ("true", why.clone()),
    };
    if opts.github_output {
        outputs(&[("fallback", fallback), ("mac", tag), ("job", url), ("why", &why)])?;
        summary(&format!("Office Mac: {why}. {url}"))?;
    }
    match outcome.verdict {
        Verdict::Passed => {
            println!("✓ {why}");
            Ok(())
        }
        Verdict::Failed => bail!("{why}: {url}"),
        Verdict::Yielded(_) if opts.github_output => {
            println!("↩ fell back to GitHub: {why}");
            Ok(())
        }
        Verdict::Yielded(_) => yielded(&why),
    }
}

/// `cargo xtask gitlab warm`.
fn warm(mac: Option<&str>) -> Result<()> {
    watch_signals()?;
    let macs = candidates(mac)?;
    let plan = Plan { sha: None, commands: &[], macs: &macs, vars: &[], prepare: None };
    let mut api = Api::new();
    let outcome = dispatch(&mut api, &plan, &mut std::io::stdout())?;
    let took = format!("{:.0?}", api.now());
    match outcome.verdict {
        Verdict::Passed => {
            println!("✓ the image is on {} ({took})", outcome.mac.map_or("", |mac| mac.tag));
            Ok(())
        }
        Verdict::Failed => bail!("the image could not come onto the Mac"),
        Verdict::Yielded(why) => yielded(&format!("{why} (a later `warm` resumes the pull)")),
    }
}

/// `cargo xtask gitlab job`, in the guest.
fn job(reports: Option<&Utf8Path>) -> Result<()> {
    let lines = std::env::var(VAR_ARGS).with_context(|| {
        format!("{VAR_ARGS} is not set: `xtask gitlab job` runs in a GitLab job")
    })?;
    let xtask = Utf8PathBuf::try_from(std::env::current_exe()?)?;
    let failed = run_steps(&xtask, &command_lines(&lines))?;
    if let Some(reports) = reports {
        collect_reports(&crate::tools::repo_root()?.join("target"), reports)?;
    }
    ensure!(failed.is_empty(), "failed: xtask {}", failed.join("; xtask "));
    Ok(())
}

/// `cargo xtask gitlab guard`, in the guest: run `command` (the job's steps) and end it with every
/// process it started the moment another project's job wants this Mac, in seconds, where
/// GitLab's cancel reaches the runner only with its next log update (60 s by default); then exit
/// [`YIELDED`]. Otherwise exit as `command` did. A guard that cannot start yields too: unguarded,
/// the job could hold the Mac for a minute against a job of its owners'.
#[expect(
    clippy::exit,
    reason = "the guard's exit status is the command's, which gitlab/ci.yml reads"
)]
fn guard_command(command: &[String]) -> Result<()> {
    let (mac, ours, mut work) = match start_guard(command) {
        Ok(started) => started,
        Err(e) => give_back(&format!("the guard could not start: {e:#}")),
    };
    println!("slopty-gitlab: guarding job {ours} on runner {}", mac.runner);
    match guard(&mut Api::new(), mac, ours, &mut work, &mut std::io::stdout()) {
        Ok(Some(status)) => std::process::exit(status),
        Ok(None) => std::process::exit(YIELDED),
        Err(e) => {
            work.stop();
            Err(e)
        }
    }
}

/// The guard's Mac and job, from the job's variables, and `command` started. The Mac is the
/// job's runner by its id alone, so an image's guard outlives a change to [`MACS`].
fn start_guard(command: &[String]) -> Result<(Mac, u64, Supervised)> {
    let number = |name: &str| -> Result<u64> {
        let value = std::env::var(name).with_context(|| format!("{name} is not set"))?;
        value.parse().with_context(|| format!("{name}={value}"))
    };
    let tag = std::env::var(VAR_TAG).unwrap_or_else(|_| "this Mac".to_owned());
    let mac = Mac { runner: number("CI_RUNNER_ID")?, tag: tag.leak(), what: "" };
    let ours = number("CI_JOB_ID")?;
    let (program, args) = command.split_first().context("no command to guard")?;
    watch_signals()?;
    let child = Command::new(program).args(args).spawn().with_context(|| program.clone())?;
    Ok((mac, ours, Supervised { child }))
}

/// Give the Mac back from inside the job: log [`YIELD_MARK`] and why, and exit [`YIELDED`].
#[expect(clippy::exit, reason = "a yield is its own exit status, which gitlab/ci.yml allows")]
fn give_back(why: &str) -> ! {
    println!("{YIELD_MARK}: {why}");
    std::process::exit(YIELDED)
}

/// End this process as a yield.
#[expect(
    clippy::exit,
    reason = "a yield is its own exit status, which the caller and gitlab/ci.yml read"
)]
fn yielded(why: &str) -> Result<()> {
    println!("↩ {why}");
    std::process::exit(YIELDED)
}

/// The names of the files under `target` a run's reports are in.
const REPORTS: [&str; 3] = ["junit.xml", "ptys.log", "cargo-timing.html"];

/// The directories under `target` that hold builds, not reports: never walked.
const BUILDS: [&str; 6] = ["deps", "build", "incremental", ".fingerprint", "examples", "xtask"];

/// Copy the reports under `target` ([`REPORTS`], and `target/e2e` whole) into `to`, each at its
/// path under `target`.
fn collect_reports(target: &Utf8Path, to: &Utf8Path) -> Result<()> {
    let mut dirs = vec![target.to_path_buf()];
    let mut copied = 0_usize;
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = dir.read_dir_utf8() else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let kind = entry.file_type()?;
            let name = entry.file_name();
            let report = REPORTS.contains(&name)
                || path.strip_prefix(target).is_ok_and(|p| p.starts_with("e2e"));
            if kind.is_dir() && !BUILDS.contains(&name) {
                dirs.push(path.to_path_buf());
            } else if !kind.is_dir() && report {
                let dest = to.join(path.strip_prefix(target)?);
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(path, &dest).with_context(|| format!("copy {path}"))?;
                copied = copied.saturating_add(1);
            }
        }
    }
    println!("{copied} report file(s) into {to}");
    Ok(())
}

/// `cargo xtask gitlab publish`.
fn publish() -> Result<()> {
    ensure!(
        !CI.contains("PINNED_BY_PUSH"),
        "gitlab/ci.yml pins no image yet: `cargo xtask vm ci-image --push`"
    );
    let token =
        std::env::var(PUBLISH_TOKEN).with_context(|| format!("{PUBLISH_TOKEN} is not set"))?;
    let auth = config("header", &format!("PRIVATE-TOKEN: {token}"));
    let file =
        format!("{GITLAB}/api/v4/projects/{PROJECT}/repository/files/.gitlab-ci.yml/raw?ref=main");
    if curl(std::slice::from_ref(&auth), &[&file]).is_ok_and(|now| now == CI) {
        println!("✓ cong.tran/slopty-ci already runs this gitlab/ci.yml");
        return Ok(());
    }
    let body = serde_json::json!({
        "branch": "main",
        "commit_message": "ci: the pipeline of xtask/src/gitlab/ci.yml",
        "actions": [{ "action": "update", "file_path": ".gitlab-ci.yml", "content": CI }],
    });
    curl(
        &[
            auth,
            config("header", "Content-Type: application/json"),
            config("data-binary", &body.to_string()),
        ],
        &["--request", "POST", &format!("{GITLAB}/api/v4/projects/{PROJECT}/repository/commits")],
    )?;
    println!("✓ published gitlab/ci.yml to cong.tran/slopty-ci");
    Ok(())
}

/// Append `pairs` to the file `$GITHUB_OUTPUT` names.
fn outputs(pairs: &[(&str, &str)]) -> Result<()> {
    let mut text = String::new();
    for (key, value) in pairs {
        writeln!(text, "{key}={}", value.replace('\n', " "))?;
    }
    append("GITHUB_OUTPUT", &text)
}

/// Append a line to the job's summary on GitHub.
fn summary(line: &str) -> Result<()> {
    append("GITHUB_STEP_SUMMARY", &format!("{line}\n"))
}

fn append(var: &str, text: &str) -> Result<()> {
    let path =
        std::env::var(var).with_context(|| format!("{var} is not set: not on GitHub Actions"))?;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
        .with_context(|| format!("open {path}"))?;
    file.write_all(text.as_bytes())?;
    Ok(())
}

/// GitLab through curl.
#[derive(Debug)]
struct Api {
    started: Instant,
}

impl Api {
    fn new() -> Self {
        Self { started: Instant::now() }
    }

    /// The `Authorization` line for the token in `var`.
    fn bearer(var: &str) -> Result<String> {
        let token = std::env::var(var).with_context(|| format!("{var} is not set"))?;
        Ok(config("header", &format!("Authorization: Bearer {token}")))
    }

    /// A GET of the project's REST API at `path`, with the read token.
    fn get(path: &str) -> Result<String> {
        curl(
            &[Self::bearer(READ_TOKEN)?],
            &[
                "--retry",
                "3",
                "--retry-all-errors",
                &format!("{GITLAB}/api/v4/projects/{PROJECT}/{path}"),
            ],
        )
    }

    /// The job's artifacts, unpacked into `dir`.
    fn artifacts(&self, id: u64, dir: &Utf8Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        let zip = dir.join(format!("job-{id}.zip"));
        let url = format!("{GITLAB}/api/v4/projects/{PROJECT}/jobs/{id}/artifacts");
        curl(&[Self::bearer(READ_TOKEN)?], &["--location", "--output", zip.as_str(), &url])?;
        let status = Command::new("unzip")
            .args(["-oq", zip.as_str(), "-d", dir.as_str()])
            .status()
            .context("unzip")?;
        ensure!(status.success(), "unzip {zip}: {status}");
        std::fs::remove_file(&zip)?;
        println!("artifacts of job {id} in {dir} ({:.0?} in)", self.started.elapsed());
        Ok(())
    }
}

impl Gitlab for Api {
    fn runner(&mut self, mac: Mac) -> Result<Runner> {
        let body = serde_json::json!({ "query": runner_query(mac.runner) }).to_string();
        let said = curl(
            &[
                Self::bearer(READ_TOKEN)?,
                config("header", "Content-Type: application/json"),
                config("data-binary", &body),
            ],
            &["--retry", "3", "--retry-all-errors", &format!("{GITLAB}/api/graphql")],
        )?;
        parse_runner(&said)
    }

    fn active(&mut self) -> Result<Vec<u64>> {
        let said = Self::get("jobs?scope[]=pending&scope[]=running&per_page=100")?;
        let jobs: Vec<Job> = serde_json::from_str(&said).context("the project's jobs")?;
        Ok(jobs.into_iter().map(|job| job.id).collect())
    }

    fn trigger(&mut self, vars: &[(&str, &str)]) -> Result<u64> {
        let token =
            std::env::var(TRIGGER_TOKEN).with_context(|| format!("{TRIGGER_TOKEN} is not set"))?;
        let mut lines = vec![
            config("form-string", &format!("token={token}")),
            config("form-string", "ref=main"),
        ];
        lines.extend(
            vars.iter()
                .map(|(key, value)| config("form-string", &format!("variables[{key}]={value}"))),
        );
        let said =
            curl(&lines, &[&format!("{GITLAB}/api/v4/projects/{PROJECT}/trigger/pipeline")])?;
        let pipeline: Pipeline = serde_json::from_str(&said).context("the triggered pipeline")?;
        Ok(pipeline.id)
    }

    fn jobs(&mut self, pipeline: u64) -> Result<Vec<Job>> {
        serde_json::from_str(&Self::get(&format!("pipelines/{pipeline}/jobs"))?)
            .context("the pipeline's jobs")
    }

    fn job(&mut self, id: u64) -> Result<Job> {
        serde_json::from_str(&Self::get(&format!("jobs/{id}"))?).context("the job")
    }

    fn trace(&mut self, id: u64) -> Result<String> {
        Self::get(&format!("jobs/{id}/trace"))
    }

    fn cancel(&mut self, id: u64) -> Result<()> {
        let url = format!("{GITLAB}/api/v4/projects/{PROJECT}/jobs/{id}/cancel");
        curl(
            &[Self::bearer(CANCEL_TOKEN)?],
            &["--retry", "3", "--retry-all-errors", "--request", "POST", &url],
        )
        .map(|_said| ())
    }

    fn now(&self) -> Duration {
        self.started.elapsed()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "xtask is a script, not a library; polling GitLab"
    )]
    fn pause(&mut self, span: Duration) -> Result<()> {
        let until = Instant::now().checked_add(span).context("a pause past the clock's end")?;
        while Instant::now() < until {
            let signal = SIGNALLED.load(Ordering::SeqCst);
            ensure!(signal == 0, "signal {signal}");
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }
}

/// A line of curl's config (`--config -`): `key = "value"`, quoted as curl reads it.
fn config(key: &str, value: &str) -> String {
    let quoted =
        value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r");
    format!("{key} = \"{quoted}\"")
}

/// curl with `config` on its stdin (so no process listing shows a token) and `args`; the body.
fn curl(config: &[String], args: &[&str]) -> Result<String> {
    let mut child = Command::new("curl")
        .args(["--silent", "--show-error", "--fail-with-body", "--max-time", "60", "--config", "-"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("curl")?;
    let mut stdin = child.stdin.take().context("curl's stdin")?;
    stdin.write_all(config.join("\n").as_bytes())?;
    drop(stdin);
    let out = child.wait_with_output().context("curl")?;
    let body = String::from_utf8_lossy(&out.stdout).into_owned();
    let url = args.last().copied().unwrap_or_default();
    ensure!(
        out.status.success(),
        "{url}: {} {}",
        String::from_utf8_lossy(&out.stderr).trim(),
        body.chars().take(300).collect::<String>()
    );
    Ok(body)
}

/// The signal that asked this process to end, or 0.
static SIGNALLED: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_signal(signal: libc::c_int) {
    SIGNALLED.store(signal, Ordering::SeqCst);
}

/// On SIGINT, SIGTERM or SIGHUP (GitHub cancelling the caller's job), the next [`Gitlab::pause`]
/// fails, and `follow` cancels the job on GitLab before this process ends.
fn watch_signals() -> Result<()> {
    let handler: extern "C" fn(libc::c_int) = on_signal;
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        #[expect(
            clippy::fn_to_numeric_cast_any,
            reason = "signal(3) takes the handler's address as a sighandler_t"
        )]
        let address = handler as libc::sighandler_t;
        // SAFETY: `signal(3)` installs a handler; `on_signal` only stores to a lock-free atomic,
        // which POSIX allows a signal handler to do (async-signal-safe).
        let previous = unsafe { libc::signal(signal, address) };
        ensure!(previous != libc::SIG_ERR, "install a handler for signal {signal}");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
