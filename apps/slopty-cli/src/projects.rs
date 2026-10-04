//! `slopty project …` and `slopty task …`: the project verbs as subcommands
//! (`docs/decisions/projects.md`). Inside a session Slopty started for a task, the project and
//! the task default to its own, as they do for the same agent's MCP tools.

use std::collections::HashMap;

use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use slopty_proto::orchestration::IdempotencyKey;
use slopty_proto::project::{
    LimitsChange, Preference, ProjectStatus, Report, ReportKind, RunOn, Runner, Script, Task,
    TaskChange, TaskState, VerifierRun,
};
use slopty_tools::ops::{
    self, LaunchSpec, NewTask, PlacementSpec, ProjectEdit, ProjectSpec, ScheduleWhen,
};
use slopty_tools::resolve::Resolver;
use slopty_tools::view::projects as view;

use crate::link::Link;
use crate::verbs::{SizeArgs, key_value, print_json};

/// A project's limits, within the bounds the person set in the server's settings.
#[derive(Args, Debug, Default)]
pub struct LimitArgs {
    /// Most of its live agents on one worker.
    #[arg(long)]
    live_per_worker: Option<u16>,
    /// Most of its live agents in all.
    #[arg(long)]
    live_per_project: Option<u16>,
    /// How deep its tree of tasks may go.
    #[arg(long)]
    depth: Option<u16>,
    /// How many timeline entries it keeps.
    #[arg(long)]
    timeline_kept: Option<u32>,
    /// What its agents may spend, a meter at a time: `usd=50` caps the estimated cost in
    /// dollars, `five-hour=80%` a plan window. At a cap it starts no task until it is raised.
    /// `none` takes the budget away. Yours to set: an agent's is refused.
    #[arg(long, value_name = "METER=CAP")]
    budget: Vec<String>,
}

impl LimitArgs {
    fn change(&self) -> Result<LimitsChange> {
        let budget = if self.budget.is_empty() {
            None
        } else {
            Some(slopty_tools::budget::parse(&self.budget)?)
        };
        Ok(LimitsChange {
            live_per_worker: self.live_per_worker,
            live_per_project: self.live_per_project,
            depth: self.depth,
            timeline_kept: self.timeline_kept,
            budget,
        })
    }
}

/// `slopty project …`.
#[derive(Subcommand, Debug)]
pub enum ProjectCmd {
    /// Make a project: one goal many agents work on across the workers.
    Create {
        /// Its name: lowercase letters, digits and dashes.
        project: String,
        /// What it is for, in a line.
        #[arg(long)]
        title: String,
        /// The repository its tasks work in.
        #[arg(long)]
        repo: String,
        /// The branch finished work lands on.
        #[arg(long, default_value = "main")]
        target: String,
        /// The command that says a task's work is right (`cargo gate`).
        #[arg(long)]
        verifier: Option<String>,
        /// Have a fresh-context reviewer read each task's work before it merges, looking for
        /// this as well as for what would be wrong to merge.
        #[arg(long)]
        review: Option<String>,
        /// Push the target branch to the orchestrator's clone's `origin` after each merge.
        #[arg(long)]
        push: bool,
        /// Hold each task's start until you start it: the orchestrator's starts only propose.
        #[arg(long)]
        ask_to_start: bool,
        /// The orchestrator's terminal; this one when run inside a Slopty terminal.
        #[arg(long)]
        orchestrator: Option<String>,
        #[command(flatten)]
        limits: LimitArgs,
        /// Anything to keep with it, as a JSON object.
        #[arg(long)]
        metadata: Option<String>,
    },
    /// Change a project's orchestrator, verifier, pushing, limits or metadata.
    Update {
        /// The project (this session's own when omitted).
        project: Option<String>,
        /// A new orchestrator terminal.
        #[arg(long)]
        orchestrator: Option<String>,
        /// A new verifier command; empty for none.
        #[arg(long)]
        verifier: Option<String>,
        /// A new reviewer's brief; empty for no reviewer.
        #[arg(long)]
        review: Option<String>,
        /// Push the target after each merge (`true`), or stop (`false`).
        #[arg(long)]
        push: Option<bool>,
        /// Hold each task's start until you start it (`true`), or let the orchestrator start
        /// them (`false`).
        #[arg(long)]
        ask_to_start: Option<bool>,
        #[command(flatten)]
        limits: LimitArgs,
        /// New metadata, a JSON object.
        #[arg(long)]
        metadata: Option<String>,
    },
    /// Say what one kind of a project's work needs of the machine it runs on, in place of
    /// what it was said to need: tasks owning its paths (every task with none) get its rules.
    Need {
        /// The project.
        project: String,
        /// The need's name, in a few words: "Apple work".
        name: String,
        /// A path whose owners have it; repeatable. Every task when none.
        #[arg(long = "path")]
        paths: Vec<String>,
        /// A CEL rule a worker must hold, such as `os == "macos"`; repeatable.
        #[arg(long)]
        require: Vec<String>,
        /// A CEL rule that scores a worker, `WEIGHT:CEL` or `CEL`; repeatable.
        #[arg(long, value_parser = preference)]
        prefer: Vec<Preference>,
        /// Take the need away instead.
        #[arg(long, conflicts_with_all = ["paths", "require", "prefer"])]
        remove: bool,
    },
    /// Tell a project's orchestrator something, as the person: the words reach it through its
    /// hooks, and wake it when it is idle.
    Tell {
        /// The project.
        project: String,
        /// What to say.
        #[arg(required = true, num_args = 1..)]
        words: Vec<String>,
    },
    /// Let a project go: its tasks, queue and timeline. The terminals that worked in it stay.
    Delete {
        /// The project.
        project: String,
    },
    /// Every project.
    List,
    /// A project's schedules: tasks it makes and starts at set times, as the person sets them.
    Schedule {
        #[command(subcommand)]
        cmd: Box<ScheduleCmd>,
    },
    /// A project's scripts: the commands you name for it (dev, test, build), each run in a
    /// terminal of your own.
    Script {
        #[command(subcommand)]
        cmd: ScriptCmd,
    },
    /// A project's tree, bounds and timeline.
    Status {
        /// The project (this session's own when omitted).
        project: Option<String>,
        /// Timeline cursor: the `next` of an earlier call; 0 for everything kept.
        #[arg(long)]
        since: Option<u64>,
        /// Wait this many milliseconds for a change past `--since`.
        #[arg(long, default_value_t = 0)]
        timeout: u32,
    },
}

/// `slopty project schedule …`.
#[derive(Subcommand, Debug)]
pub enum ScheduleCmd {
    /// Set a schedule: a task made and started each time its rule comes round, in your time
    /// zone. With `--schedule` it sets that one anew.
    Set(Box<SetSchedule>),
    /// Take a schedule away; the tasks it made stay.
    Rm {
        /// The project (this session's own when omitted).
        #[arg(long)]
        project: Option<String>,
        /// The schedule's number.
        schedule: u32,
    },
    /// Run a schedule now, paused or not, and print the task it made.
    Run {
        /// The project (this session's own when omitted).
        #[arg(long)]
        project: Option<String>,
        /// The schedule's number.
        schedule: u32,
    },
    /// List a project's schedules.
    Ls {
        /// The project (this session's own when omitted).
        #[arg(long)]
        project: Option<String>,
    },
}

/// `slopty project script …`.
#[derive(Subcommand, Debug)]
pub enum ScriptCmd {
    /// Name a command for the project, in place of one of the same name.
    Set {
        /// The project (this session's own when omitted).
        #[arg(long)]
        project: Option<String>,
        /// Where under the project's folder it runs, relative (`web`).
        #[arg(long)]
        dir: Option<String>,
        /// Its name: `dev`, `test`, anything of letters, digits, `-`, `_` and `.`.
        name: String,
        /// The command line, as you would type it in your shell.
        command: String,
    },
    /// Take a script away.
    Rm {
        /// The project (this session's own when omitted).
        #[arg(long)]
        project: Option<String>,
        /// Its name.
        name: String,
    },
    /// List a project's scripts.
    Ls {
        /// The project (this session's own when omitted).
        #[arg(long)]
        project: Option<String>,
    },
    /// Run a script in a terminal of your own on a worker, and print its TERM: in a task's
    /// worktree with `--task`, else in the project's folder.
    Run {
        /// The project (this session's own when omitted).
        #[arg(long)]
        project: Option<String>,
        /// The worker (the task's, or the orchestrator's, when omitted).
        #[arg(long)]
        worker: Option<String>,
        /// The task whose worktree it runs in.
        #[arg(long)]
        task: Option<String>,
        /// Its name.
        name: String,
    },
}

/// Run a `slopty project script …`.
async fn script(
    cmd: ScriptCmd,
    res: &mut Resolver<'_, Link>,
    link: &Link,
    (json, key): (bool, Option<IdempotencyKey>),
) -> Result<()> {
    let status = match cmd {
        ScriptCmd::Set { project, dir, name, command } => {
            let script = Script { name, command, dir };
            ops::script_set(link, project.as_deref(), script, key).await?
        }
        ScriptCmd::Rm { project, name } => {
            ops::script_delete(link, project.as_deref(), name, key).await?
        }
        ScriptCmd::Ls { project } => ops::project_status(link, project.as_deref(), None, 0).await?,
        ScriptCmd::Run { project, worker, task, name } => {
            let (project, worker, task) = (project.as_deref(), worker.as_deref(), task.as_deref());
            let term = ops::script_run(res, project, name, worker, task, key).await?;
            return crate::verbs::print_term(term, json);
        }
    };
    if json {
        return print_json(&status.project.scripts);
    }
    if status.project.scripts.is_empty() {
        println!("{} has no script", status.project.id);
    }
    for s in &status.project.scripts {
        println!("{}", view::script_text(s));
    }
    Ok(())
}

/// `slopty project schedule set`.
#[derive(Args, Debug)]
pub struct SetSchedule {
    /// The project (this session's own when omitted).
    #[arg(long)]
    project: Option<String>,
    /// The schedule to set anew, by its number.
    #[arg(long)]
    schedule: Option<u32>,
    /// When: five cron fields (minute, hour, day of the month, month, day of the week),
    /// such as `0 9 * * 1-5`, or `@daily`, `@weekly` and the like.
    #[arg(long)]
    when: String,
    /// The IANA time zone it is read in; this machine's own when omitted.
    #[arg(long)]
    zone: Option<String>,
    /// It runs only when you say (`schedule run`).
    #[arg(long)]
    paused: bool,
    /// Each run's task, in a line.
    #[arg(long)]
    title: String,
    /// What its agent is told to do.
    #[arg(long, default_value = "")]
    brief: String,
    /// What sort of work it is.
    #[arg(long, default_value = "")]
    kind: String,
    /// A repository-relative path it alone may write; repeatable.
    #[arg(long = "owns", value_name = "PATH")]
    owns: Vec<String>,
    /// It only reads: it owns nothing.
    #[arg(long, conflicts_with = "owns")]
    read_only: bool,
    #[command(flatten)]
    placement: PlacementArgs,
    /// Its own verifier, over the project's.
    #[arg(long)]
    verifier: Option<String>,
    /// The agent: `claude` (the default), `codex`, `pi`, or an ACP agent by name.
    #[arg(long)]
    agent: Option<String>,
    /// The model, by the agent's own id.
    #[arg(long)]
    model: Option<String>,
    /// The agent's first prompt.
    #[arg(long)]
    prompt: Option<String>,
    /// This worker over the task's placement (name or id).
    #[arg(long)]
    worker: Option<String>,
    /// Working directory on the worker; beside a clone of the project's repository when
    /// omitted.
    #[arg(long)]
    cwd: Option<String>,
    /// Run the words after `--` as the program, not as the agent's arguments.
    #[arg(long, conflicts_with_all = ["agent", "model", "prompt"])]
    command: bool,
    /// Arguments for the agent, or with `--command` the program and its arguments.
    #[arg(last = true)]
    args: Vec<String>,
}

/// This machine's IANA time zone: `TZ` when it names one, else where `/etc/localtime` points
/// in the zone database; empty when neither says, for the server's own.
fn local_zone() -> String {
    let named = std::env::var("TZ").ok().map(|tz| tz.trim_start_matches(':').to_owned());
    if let Some(tz) = named.filter(|tz| tz.contains('/') && !tz.starts_with('/')) {
        return tz;
    }
    std::fs::read_link("/etc/localtime")
        .ok()
        .and_then(|target| zone_of(&target.to_string_lossy()))
        .unwrap_or_default()
}

/// The zone a path into the zone database names: `…/zoneinfo/Europe/Berlin` is `Europe/Berlin`.
fn zone_of(path: &str) -> Option<String> {
    path.split_once("zoneinfo/").map(|(_, zone)| zone.to_owned()).filter(|z| !z.is_empty())
}

/// Run a `slopty project schedule …`.
async fn schedule(
    cmd: ScheduleCmd,
    res: &mut Resolver<'_, Link>,
    link: &Link,
    (json, key): (bool, Option<IdempotencyKey>),
) -> Result<()> {
    let status = match cmd {
        ScheduleCmd::Set(set) => {
            let SetSchedule {
                project,
                schedule,
                when,
                zone,
                paused,
                title,
                brief,
                kind,
                owns,
                read_only,
                placement,
                verifier,
                agent,
                model,
                prompt,
                worker,
                cwd,
                command,
                args,
            } = *set;
            let new = NewTask {
                parent: None,
                depends_on: Vec::new(),
                kind,
                title,
                brief,
                owns,
                read_only,
                placement: placement.spec(),
                verifier,
                metadata: None,
            };
            let launch = LaunchSpec {
                pin: worker,
                cwd: cwd.unwrap_or_default(),
                run: if command {
                    Runner::Command { argv: args }
                } else {
                    ops::agent_runner(agent.as_deref(), prompt, model, args)
                },
                env: Vec::new(),
                size: None,
                ignore_dependencies: false,
            };
            let zone = zone.unwrap_or_else(local_zone);
            let when = ScheduleWhen { when, zone, paused };
            ops::schedule_set(res, project.as_deref(), schedule, (new, launch, when), key).await?
        }
        ScheduleCmd::Rm { project, schedule } => {
            ops::schedule_delete(link, project.as_deref(), schedule, key).await?
        }
        ScheduleCmd::Run { project, schedule } => {
            let task = ops::schedule_run(link, project.as_deref(), schedule, key).await?;
            return print_task(&task, json);
        }
        ScheduleCmd::Ls { project } => {
            ops::project_status(link, project.as_deref(), None, 0).await?
        }
    };
    if json {
        return print_json(&view::status(&status));
    }
    if status.project.schedules.is_empty() {
        println!("{} has no schedule", status.project.id);
    }
    for s in &status.project.schedules {
        println!("{}", view::schedule_text(s));
    }
    Ok(())
}

/// Which project and task: this session's own when omitted.
#[derive(Args, Debug)]
pub struct TaskRef {
    /// The project (this session's own when omitted).
    #[arg(long)]
    project: Option<String>,
    /// The task's number (this session's own when omitted).
    #[arg(long)]
    task: Option<String>,
}

/// Where a task may run: rules in CEL over the workers' facts (`slopty workers --json` shows
/// them).
#[derive(Args, Debug, Default)]
pub struct PlacementArgs {
    /// Run it on this worker (name or id) and no other.
    #[arg(long)]
    pin: Option<String>,
    /// A rule every worker must meet, such as `os == "linux" && cpus >= 16`; repeatable.
    #[arg(long = "require", value_name = "CEL")]
    require: Vec<String>,
    /// A rule that scores a worker, `WEIGHT:CEL` or `CEL` (weight 1); repeatable.
    #[arg(long = "prefer", value_name = "[WEIGHT:]CEL", value_parser = preference)]
    prefer: Vec<Preference>,
    /// Run beside this task (`#3`) or worker; repeatable.
    #[arg(long = "near")]
    near: Vec<String>,
    /// Keep away from this task (`#3`) or worker; repeatable.
    #[arg(long = "avoid")]
    avoid: Vec<String>,
}

impl PlacementArgs {
    const fn is_empty(&self) -> bool {
        self.pin.is_none()
            && self.require.is_empty()
            && self.prefer.is_empty()
            && self.near.is_empty()
            && self.avoid.is_empty()
    }

    fn spec(self) -> PlacementSpec {
        PlacementSpec {
            pin: self.pin,
            require: self.require,
            prefer: self.prefer,
            near: self.near,
            avoid: self.avoid,
        }
    }
}

/// `WEIGHT:CEL` or `CEL`.
fn preference(s: &str) -> Result<Preference, String> {
    let (weight, expr) = match s.split_once(':') {
        Some((w, e)) if w.trim().parse::<i32>().is_ok() => {
            (w.trim().parse().map_err(|e: std::num::ParseIntError| e.to_string())?, e)
        }
        _ => (1, s),
    };
    if expr.trim().is_empty() {
        return Err("a preference needs a rule".to_owned());
    }
    Ok(Preference { expr: expr.trim().to_owned(), weight })
}

/// `slopty task …`.
#[derive(Subcommand, Debug)]
pub enum TaskCmd {
    /// Make a task in a project.
    Create {
        /// The project (this session's own when omitted).
        #[arg(long)]
        project: Option<String>,
        /// The task it is split from (this session's own task when omitted).
        #[arg(long)]
        parent: Option<String>,
        /// A task it needs first; repeatable.
        #[arg(long = "depends-on", value_name = "TASK")]
        depends_on: Vec<String>,
        /// What sort of work it is, in your words.
        #[arg(long, default_value = "")]
        kind: String,
        /// What it is, in a line.
        #[arg(long)]
        title: String,
        /// What its agent is told to do.
        #[arg(long, default_value = "")]
        brief: String,
        /// A repository-relative path it alone may write; repeatable.
        #[arg(long = "owns", value_name = "PATH")]
        owns: Vec<String>,
        /// It only reads: it owns nothing.
        #[arg(long, conflicts_with = "owns")]
        read_only: bool,
        #[command(flatten)]
        placement: PlacementArgs,
        /// Its own verifier, over the project's.
        #[arg(long)]
        verifier: Option<String>,
        /// Anything to keep with it, as a JSON object.
        #[arg(long)]
        metadata: Option<String>,
    },
    /// Take more paths for a task to own.
    Claim {
        #[command(flatten)]
        which: TaskRef,
        /// Repository-relative paths.
        #[arg(required = true)]
        paths: Vec<String>,
    },
    /// Change a task: move it, say what it is doing, record its branch or verifier, or note
    /// something on the timeline.
    Update {
        #[command(flatten)]
        which: TaskRef,
        /// planned, running, waiting, blocked, verifying, done, merged or failed.
        #[arg(long)]
        state: Option<String>,
        /// What it is doing, in its own words; empty clears it.
        #[arg(long)]
        status: Option<String>,
        /// The branch its work is on.
        #[arg(long)]
        branch: Option<String>,
        /// The verifier passed on `--head` over `--base`.
        #[arg(long, conflicts_with = "failed", requires_all = ["head", "base"])]
        passed: bool,
        /// The verifier failed on `--head` over `--base`.
        #[arg(long, requires_all = ["head", "base"])]
        failed: bool,
        /// What the verifier said.
        #[arg(long)]
        summary: Option<String>,
        /// The commit the verifier ran on, in hex.
        #[arg(long)]
        head: Option<String>,
        /// The commit the task's work starts from, in hex.
        #[arg(long)]
        base: Option<String>,
        /// Words for the timeline.
        #[arg(long)]
        note: Option<String>,
        /// Its dependencies, in place of the old; repeatable.
        #[arg(long = "depends-on", value_name = "TASK")]
        depends_on: Vec<String>,
        /// It depends on nothing now.
        #[arg(long, conflicts_with = "depends_on")]
        no_dependencies: bool,
        /// A placement in place of the old, when any of its flags is given.
        #[command(flatten)]
        placement: PlacementArgs,
        /// Run it on this worker over its placement's rules, or `anywhere` to let them choose.
        #[arg(long, value_name = "WORKER")]
        run_on: Option<String>,
        /// Its own verifier; empty for the project's.
        #[arg(long)]
        verifier: Option<String>,
        /// New metadata, a JSON object.
        #[arg(long)]
        metadata: Option<String>,
    },
    /// Put a terminal (this one when omitted) on a task.
    Assign {
        /// The project (this session's own when omitted).
        #[arg(long)]
        project: Option<String>,
        /// The task's number.
        task: String,
        /// The terminal.
        #[arg(long)]
        term: Option<String>,
    },
    /// Start what runs for a task where the server places it, and print the task: Claude Code,
    /// or with `--command` the program after `--`.
    Spawn {
        /// The project (this session's own when omitted).
        #[arg(long)]
        project: Option<String>,
        /// The task's number.
        task: String,
        /// This worker over the task's placement (name or id).
        #[arg(long)]
        worker: Option<String>,
        /// Working directory on the worker; its home when omitted.
        #[arg(long)]
        cwd: Option<String>,
        /// The agent's first prompt.
        #[arg(long, conflicts_with = "command")]
        prompt: Option<String>,
        /// Run the words after `--` as the program, not as the agent's arguments.
        #[arg(long)]
        command: bool,
        /// The agent: `claude` (the default), `codex`, `pi`, or an ACP agent by the registry's
        /// name. Each gets Slopty's tools and its role.
        #[arg(long, conflicts_with = "command")]
        agent: Option<String>,
        /// The model, by the agent's own id.
        #[arg(long, conflicts_with = "command")]
        model: Option<String>,
        /// An environment variable, `KEY=VALUE`; repeatable.
        #[arg(long = "env", value_name = "KEY=VALUE", value_parser = key_value)]
        env: Vec<(String, String)>,
        #[command(flatten)]
        size: SizeArgs,
        /// Start it though a task it depends on is not done yet.
        #[arg(long)]
        ignore_dependencies: bool,
        /// Arguments for the agent, or with `--command` the program and its arguments.
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Have several agents try a task at once, an attempt each, and print the task: each
    /// attempt goes to the worker it names, or to one that fits and no other attempt took.
    Attempts {
        /// The project (this session's own when omitted).
        #[arg(long)]
        project: Option<String>,
        /// The task's number.
        task: String,
        /// One attempt: the agent, then `model=` and `on=` (a worker) when wanted, such as
        /// `codex,model=o3,on=studio` or `pi`; repeatable, up to 6.
        #[arg(long = "try", value_name = "AGENT[,model=M][,on=WORKER]", required = true,
              value_parser = attempt)]
        tries: Vec<Attempt>,
        /// Working directory on each worker; beside a clone of the project's repository when
        /// omitted.
        #[arg(long)]
        cwd: Option<String>,
        /// Each agent's first prompt.
        #[arg(long)]
        prompt: Option<String>,
        /// Start them though a task it depends on is not done yet.
        #[arg(long)]
        ignore_dependencies: bool,
    },
    /// Pick the attempt that lands, and print the task it tries: every other attempt stops,
    /// its agent closed and its worktree freed, its branch kept.
    Pick {
        /// The project (this session's own when omitted).
        #[arg(long)]
        project: Option<String>,
        /// The attempt's number.
        attempt: String,
    },
    /// Report on a task's work to whoever split it off (its parent task's agent, or the
    /// orchestrator), through that agent's hooks.
    Report {
        #[command(flatten)]
        which: TaskRef,
        /// What it is: progress, a question, a block or the finish.
        #[arg(long, value_enum)]
        kind: KindArg,
        /// What there is to say, in a few lines.
        #[arg(long, default_value = "")]
        note: String,
        /// Something made: a path, a commit, a link; repeatable.
        #[arg(long = "artifact", value_name = "WHAT")]
        artifacts: Vec<String>,
        /// The branch the work is on.
        #[arg(long)]
        branch: Option<String>,
        /// The pull request opened, by number.
        #[arg(long)]
        pr: Option<u32>,
    },
    /// Put a task in the merge queue, as the person: its verifier runs on its branch first
    /// when one applies; how a task with no verifier merges, or one given back is tried again.
    Merge {
        #[command(flatten)]
        which: TaskRef,
    },
    /// Start a task its orchestrator proposed, as the person: where the server places it, or on
    /// the worker named.
    Start {
        #[command(flatten)]
        which: TaskRef,
        /// This worker, over the proposal's and the task's placement.
        #[arg(long, value_name = "WORKER")]
        on: Option<String>,
    },
    /// Tell a task's agent something: the words reach it through its hooks as a report does,
    /// never typed into its terminal (fix CI, address the comments, resolve the conflicts).
    /// Inside an agent's session they are that agent's, to a task under it, and marked so.
    Tell {
        #[command(flatten)]
        which: TaskRef,
        /// What to say.
        #[arg(required = true, num_args = 1..)]
        words: Vec<String>,
    },
    /// Wait for news of tasks: a report, a move of state, a terminal gone, a verdict, checks,
    /// a step ended. Running out of time cancels nothing.
    Wait {
        /// The project (this session's own when omitted).
        #[arg(long)]
        project: Option<String>,
        /// The tasks, `3` or `#3`.
        #[arg(required = true, num_args = 1..)]
        tasks: Vec<String>,
        /// Answer once each has news, not at the first.
        #[arg(long)]
        all: bool,
        /// The timeline cursor to wait on from: the `next` a wait printed.
        #[arg(long)]
        since: Option<u64>,
        /// How long to wait, in seconds.
        #[arg(long, default_value_t = 50)]
        timeout: u32,
    },
    /// Say whether a task's work may merge, as the person, over its reviewer's word or in its
    /// place: `--approve` puts verified work in the merge queue, `--changes` gives it back to
    /// its agent with the summary and findings.
    Review {
        #[command(flatten)]
        which: TaskRef,
        /// The work may merge.
        #[arg(long, conflicts_with = "changes", required_unless_present = "changes")]
        approve: bool,
        /// The work needs changes first.
        #[arg(long)]
        changes: bool,
        /// The review in a few lines.
        #[arg(long, default_value = "")]
        summary: String,
        /// A finding that blocks, as `path:line: words`, `path: words` or just words; again
        /// for more.
        #[arg(long = "finding")]
        findings: Vec<String>,
    },
    /// One node in full: a task with its brief and Claude Code's own subagents and to-dos, or
    /// with `--task orchestrator` the orchestrator's.
    Get {
        #[command(flatten)]
        which: TaskRef,
    },
    /// Rank every worker for a task's placement, or for the rules given, with the reasons.
    Suggest {
        #[command(flatten)]
        which: TaskRef,
        /// Rules to try in place of the task's.
        #[command(flatten)]
        placement: PlacementArgs,
    },
}

/// What a report is.
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum KindArg {
    /// Progress worth knowing.
    Checkpoint,
    /// An answer is needed to go on.
    NeedsInput,
    /// It cannot go on.
    Stuck,
    /// It finished.
    Done,
}

impl KindArg {
    const fn kind(self) -> ReportKind {
        match self {
            Self::Checkpoint => ReportKind::Checkpoint,
            Self::NeedsInput => ReportKind::NeedsInput,
            Self::Stuck => ReportKind::Stuck,
            Self::Done => ReportKind::Done,
        }
    }
}

/// Run a `slopty project …`.
pub async fn project(
    cmd: ProjectCmd,
    link: &Link,
    json: bool,
    key: Option<IdempotencyKey>,
) -> Result<()> {
    let mut res = Resolver::new(link);
    match cmd {
        ProjectCmd::Create {
            project,
            title,
            repo,
            target,
            verifier,
            review,
            push,
            ask_to_start,
            orchestrator,
            limits,
            metadata,
        } => {
            let spec = ProjectSpec {
                project,
                title,
                repo,
                target: Some(target),
                verifier,
                review,
                push,
                ask_to_start,
                orchestrator,
                limits: limits.change()?,
                metadata,
            };
            let status = ops::project_create(&mut res, spec, key).await?;
            print_status(&mut res, &status, json).await
        }
        ProjectCmd::Update {
            project,
            orchestrator,
            verifier,
            review,
            push,
            ask_to_start,
            limits,
            metadata,
        } => {
            let limits = limits.change()?;
            let edit = ProjectEdit {
                orchestrator,
                verifier,
                review,
                push,
                ask_to_start,
                limits,
                metadata,
            };
            let status = ops::project_set(&mut res, project.as_deref(), edit, key).await?;
            print_status(&mut res, &status, json).await
        }
        ProjectCmd::Need { project, name, paths, require, prefer, remove } => {
            let mut needs = ops::project_status(link, Some(&project), None, 0).await?.project.needs;
            let had = needs.len();
            needs.retain(|n| n.name != name.trim());
            if remove && needs.len() == had {
                bail!("{project} has no need named {name:?}");
            }
            if !remove {
                needs.push(slopty_proto::project::Need { name, paths, require, prefer });
            }
            let status = ops::project_needs(link, Some(&project), needs, key).await?;
            if json {
                return print_json(&view::status(&status));
            }
            let names: Vec<&str> = status.project.needs.iter().map(|n| n.name.as_str()).collect();
            if names.is_empty() {
                println!("{project} needs nothing of its machines");
            } else {
                println!("{project} needs: {}", names.join(", "));
            }
            Ok(())
        }
        ProjectCmd::Tell { project, words } => {
            ops::orchestrator_tell(link, &project, words.join(" "), key).await?;
            println!("Told {project}'s orchestrator");
            Ok(())
        }
        ProjectCmd::Delete { project } => {
            ops::project_delete(link, &project, key).await?;
            println!("Let {project} go");
            Ok(())
        }
        ProjectCmd::List => {
            let list = ops::projects(link).await?;
            if json {
                print_json(&view::projects(&list))
            } else {
                print!("{}", view::projects_text(&list));
                Ok(())
            }
        }
        ProjectCmd::Schedule { cmd } => schedule(*cmd, &mut res, link, (json, key)).await,
        ProjectCmd::Script { cmd } => script(cmd, &mut res, link, (json, key)).await,
        ProjectCmd::Status { project, since, timeout } => {
            let status = ops::project_status(link, project.as_deref(), since, timeout).await?;
            print_status(&mut res, &status, json).await
        }
    }
}

fn state(word: Option<&str>) -> Result<Option<TaskState>> {
    match word {
        Some(word) => match view::state_named(word) {
            Some(state) => Ok(Some(state)),
            None => bail!(
                "--state is not one of planned, running, waiting, blocked, verifying, done, merged, failed"
            ),
        },
        None => Ok(None),
    }
}

/// One attempt of `slopty task attempts`: which agent, with which model, on which worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attempt {
    /// The agent, as `--agent` names one.
    pub agent: String,
    /// The model, by the agent's own id.
    pub model: Option<String>,
    /// The worker, by name or id.
    pub on: Option<String>,
}

/// An attempt as typed: the agent, then `model=` and `on=` in any order, comma-separated.
fn attempt(text: &str) -> Result<Attempt, String> {
    let mut parts = text.split(',').map(str::trim);
    let agent = parts.next().filter(|a| !a.is_empty() && !a.contains('='));
    let Some(agent) = agent else {
        return Err(format!("{text:?} names no agent first, as in codex,model=o3,on=studio"));
    };
    let mut tried = Attempt { agent: agent.to_owned(), model: None, on: None };
    for part in parts {
        match part.split_once('=') {
            Some(("model", model)) if !model.is_empty() => tried.model = Some(model.to_owned()),
            Some(("on", worker)) if !worker.is_empty() => tried.on = Some(worker.to_owned()),
            _ => return Err(format!("{part:?} is not model=… or on=…")),
        }
    }
    Ok(tried)
}

/// Run a `slopty task …`.
pub async fn task(
    cmd: TaskCmd,
    link: &Link,
    json: bool,
    key: Option<IdempotencyKey>,
) -> Result<()> {
    let mut res = Resolver::new(link);
    let task = match cmd {
        TaskCmd::Create {
            project,
            parent,
            depends_on,
            kind,
            title,
            brief,
            owns,
            read_only,
            placement,
            verifier,
            metadata,
        } => {
            let new = NewTask {
                parent,
                depends_on,
                kind,
                title,
                brief,
                owns,
                read_only,
                placement: placement.spec(),
                verifier,
                metadata,
            };
            ops::task_create(&mut res, project.as_deref(), new, key).await?
        }
        TaskCmd::Claim { which, paths } => {
            ops::task_claim(link, which.project.as_deref(), which.task.as_deref(), paths, key)
                .await?
        }
        TaskCmd::Update {
            which,
            state: word,
            status,
            branch,
            passed,
            failed,
            summary,
            head,
            base,
            note,
            depends_on,
            no_dependencies,
            placement,
            run_on,
            verifier,
            metadata,
        } => {
            let verified = (passed || failed).then(|| VerifierRun {
                passed,
                summary: summary.clone().unwrap_or_default(),
                head: head.clone().unwrap_or_default(),
                base: base.clone().unwrap_or_default(),
                exit: None,
                took_ms: 0,
            });
            if verified.is_none() && summary.is_some() {
                bail!("--summary goes with --passed or --failed");
            }
            let depends_on = if no_dependencies {
                Some(Vec::new())
            } else if depends_on.is_empty() {
                None
            } else {
                Some(depends_on.iter().map(|d| ops::task_number(d)).collect::<Result<_, _>>()?)
            };
            let change = TaskChange {
                state: state(word.as_deref())?,
                status,
                branch,
                verified,
                base,
                note,
                depends_on,
                placement: None,
                run_on: match run_on.as_deref() {
                    None => None,
                    Some("anywhere") => Some(RunOn::Anywhere),
                    Some(name) => Some(RunOn::Worker(res.worker(Some(name)).await?)),
                },
                verifier,
                metadata,
            };
            let placement = (!placement.is_empty()).then(|| placement.spec());
            let (project, task) = (which.project.as_deref(), which.task.as_deref());
            ops::task_update(&mut res, project, task, change, placement, key).await?
        }
        TaskCmd::Assign { project, task, term } => {
            ops::task_assign(&mut res, project.as_deref(), Some(&task), term.as_deref(), key)
                .await?
        }
        TaskCmd::Spawn {
            project,
            task,
            worker,
            cwd,
            prompt,
            command,
            agent,
            model,
            env,
            size,
            ignore_dependencies,
            args,
        } => {
            let run = if command {
                Runner::Command { argv: args }
            } else {
                ops::agent_runner(agent.as_deref(), prompt, model, args)
            };
            let launch = LaunchSpec {
                pin: worker,
                cwd: cwd.unwrap_or_default(),
                run,
                env,
                size: size.size(),
                ignore_dependencies,
            };
            ops::task_spawn(&mut res, project.as_deref(), Some(&task), launch, key).await?
        }
        TaskCmd::Attempts { project, task, tries, cwd, prompt, ignore_dependencies } => {
            let specs = tries
                .into_iter()
                .map(|t| LaunchSpec {
                    pin: t.on,
                    cwd: cwd.clone().unwrap_or_default(),
                    run: ops::agent_runner(Some(&t.agent), prompt.clone(), t.model, Vec::new()),
                    env: Vec::new(),
                    size: None,
                    ignore_dependencies,
                })
                .collect();
            ops::task_attempts(&mut res, project.as_deref(), Some(&task), specs, key).await?
        }
        TaskCmd::Pick { project, attempt } => {
            ops::task_pick(link, project.as_deref(), &attempt, key).await?
        }
        TaskCmd::Report { which, kind, note, artifacts, branch, pr } => {
            let report = Report { kind: kind.kind(), note, artifacts, branch, pr };
            let (project, task) = (which.project.as_deref(), which.task.as_deref());
            ops::task_report(link, project, task, report, key).await?
        }
        TaskCmd::Merge { which } => {
            let (project, task) = (which.project.as_deref(), which.task.as_deref());
            ops::task_merge(link, project, task, key).await?
        }
        TaskCmd::Start { which, on } => {
            let (project, task) = (which.project.as_deref(), which.task.as_deref());
            ops::task_start(&mut res, project, task, on.as_deref(), key).await?
        }
        TaskCmd::Tell { which, words } => {
            let (project, task) = (which.project.as_deref(), which.task.as_deref());
            ops::task_tell(link, project, task, words.join(" "), key).await?;
            println!("Told its agent");
            return Ok(());
        }
        TaskCmd::Wait { project, tasks, all, since, timeout } => {
            let timeout_ms = timeout.saturating_mul(1_000);
            let waited =
                ops::task_wait(link, project.as_deref(), (&tasks, all), since, timeout_ms).await?;
            return if json {
                print_json(&view::task_wait(&waited))
            } else {
                print!("{}", view::task_wait_text(&waited));
                Ok(())
            };
        }
        TaskCmd::Review { which, approve, summary, findings, .. } => {
            let findings = findings.iter().map(|f| finding(f)).collect();
            let verdict =
                slopty_proto::project::ReviewVerdict { approved: approve, summary, findings };
            let (project, task) = (which.project.as_deref(), which.task.as_deref());
            ops::task_review(link, project, task, verdict, key).await?
        }
        TaskCmd::Get { which } => {
            let (project, task) = (which.project.as_deref(), which.task.as_deref());
            let node = ops::task_get(link, project, task).await?;
            return if json {
                print_json(&view::node(&node))
            } else {
                print!("{}", view::node_text(&node));
                Ok(())
            };
        }
        TaskCmd::Suggest { which, placement } => {
            let spec = (!placement.is_empty()).then(|| placement.spec());
            let (project, task) = (which.project.as_deref(), which.task.as_deref());
            let ranked = ops::placement_suggest(&mut res, project, task, spec).await?;
            return if json {
                print_json(&view::suggestions(&ranked))
            } else {
                print!("{}", view::suggestions_text(&ranked));
                Ok(())
            };
        }
    };
    print_task(&task, json)
}

fn print_task(task: &Task, json: bool) -> Result<()> {
    if json {
        return print_json(&view::task(task));
    }
    let term = task
        .assignment
        .as_ref()
        .map_or_else(String::new, |a| format!("  {}", slopty_tools::view::term_string(a.term)));
    println!("#{} {}  {}{term}", task.id, view::state_word(task.state), task.title);
    if let Some(attempts) = &task.attempts {
        let tried: Vec<String> = attempts.tried.iter().map(|t| format!("#{t}")).collect();
        let picked = attempts.picked.map_or_else(String::new, |p| format!(", #{p} picked"));
        println!("  attempts {}{picked}", tried.join(" "));
    }
    Ok(())
}

async fn print_status(
    res: &mut Resolver<'_, Link>,
    status: &ProjectStatus,
    json: bool,
) -> Result<()> {
    if json {
        return print_json(&view::status(status));
    }
    let names: HashMap<_, _> =
        res.workers().await?.iter().map(|w| (w.worker, w.name.clone())).collect();
    print!("{}", view::status_text(status, &names));
    Ok(())
}

/// A finding the person writes, `path:line: words`, `path: words` or just words, as one that
/// blocks.
fn finding(text: &str) -> slopty_proto::project::Finding {
    let blocker =
        |path: Option<&str>, line: Option<u32>, body: &str| slopty_proto::project::Finding {
            path: path.map(str::to_owned),
            line,
            severity: "blocker".to_owned(),
            blocking: true,
            body: body.trim().to_owned(),
        };
    let Some((place, body)) = text.split_once(": ") else { return blocker(None, None, text) };
    match place.rsplit_once(':') {
        Some((path, line)) if line.parse::<u32>().is_ok() => {
            blocker(Some(path), line.parse().ok(), body)
        }
        _ if place.contains(char::is_whitespace) => blocker(None, None, text),
        _ => blocker(Some(place), None, body),
    }
}

#[cfg(test)]
mod tests {
    use super::{Attempt, attempt, zone_of};

    /// The zone this machine is in is read from where its zone file points.
    #[test]
    fn the_local_zone_is_read_from_its_zone_file() {
        let macos = "/var/db/timezone/zoneinfo/Asia/Ho_Chi_Minh";
        assert_eq!(zone_of(macos).as_deref(), Some("Asia/Ho_Chi_Minh"));
        assert_eq!(zone_of("/usr/share/zoneinfo/Europe/Berlin").as_deref(), Some("Europe/Berlin"));
        assert_eq!(zone_of("/etc/localtime"), None);
    }

    /// An attempt names its agent first, then a model and a worker in either order; anything
    /// else is refused saying what it takes.
    #[test]
    fn an_attempt_names_its_agent_then_its_model_and_worker() {
        let full = Attempt {
            agent: "codex".to_owned(),
            model: Some("o3".to_owned()),
            on: Some("studio".to_owned()),
        };
        assert_eq!(attempt("codex,model=o3,on=studio"), Ok(full.clone()));
        assert_eq!(attempt("codex, on=studio, model=o3"), Ok(full));
        let acp = Attempt { agent: "acp:gemini".to_owned(), model: None, on: None };
        assert_eq!(attempt("acp:gemini"), Ok(acp));
        assert!(attempt("model=o3").unwrap_err().contains("names no agent"));
        assert!(attempt("pi,size=9").unwrap_err().contains("not model="));
    }
}
