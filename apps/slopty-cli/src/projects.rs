//! `slopty project …` and `slopty task …`: the project verbs as subcommands
//! (`docs/decisions/projects.md`). Inside a session Slopty started for a task, the project and
//! the task default to its own, as they do for the same agent's MCP tools.

use std::collections::HashMap;

use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use slopty_proto::orchestration::IdempotencyKey;
use slopty_proto::project::{
    LimitsChange, Preference, ProjectStatus, Report, ReportKind, RunOn, Runner, Task, TaskChange,
    TaskState, VerifierRun,
};
use slopty_tools::ops::{self, LaunchSpec, NewTask, PlacementSpec, ProjectEdit, ProjectSpec};
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
}

impl LimitArgs {
    const fn change(&self) -> LimitsChange {
        LimitsChange {
            live_per_worker: self.live_per_worker,
            live_per_project: self.live_per_project,
            depth: self.depth,
            timeline_kept: self.timeline_kept,
            budget: None,
        }
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
        /// Run the person's Codex rather than Claude Code, with the same tools.
        #[arg(long, conflicts_with = "command")]
        codex: bool,
        /// An environment variable, `KEY=VALUE`; repeatable.
        #[arg(long = "env", value_name = "KEY=VALUE", value_parser = key_value)]
        env: Vec<(String, String)>,
        #[command(flatten)]
        size: SizeArgs,
        /// Start it though a task it depends on is not done yet.
        #[arg(long)]
        ignore_dependencies: bool,
        /// Arguments for `claude`, or with `--command` the program and its arguments.
        #[arg(last = true)]
        args: Vec<String>,
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
    /// Tell a task's agent something, as the person: the words reach it through its hooks as a
    /// report does, never typed into its terminal (fix CI, address the comments, resolve the
    /// conflicts).
    Tell {
        #[command(flatten)]
        which: TaskRef,
        /// What to say.
        #[arg(required = true, num_args = 1..)]
        words: Vec<String>,
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
                limits: limits.change(),
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
            let limits = limits.change();
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
            codex,
            env,
            size,
            ignore_dependencies,
            args,
        } => {
            let run = if command {
                Runner::Command { argv: args }
            } else if codex {
                Runner::Codex { prompt, args }
            } else {
                Runner::Claude { prompt, args }
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
