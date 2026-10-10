//! `slopty project …` and `slopty task …`: the project verbs as subcommands
//! (`docs/decisions/projects.md`). Inside a session Slopty started for a task, the project and
//! the task default to its own, as they do for the same agent's MCP tools.

use std::collections::HashMap;

use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use slopty_proto::orchestration::IdempotencyKey;
use slopty_proto::project::{
    Autonomy, LimitsChange, ProjectStatus, Report, RunOn, Task, TaskChange, TaskState, VerifierRun,
};
use slopty_tools::ops::{self, NewTask, ProjectEdit, ProjectSpec, Which};
use slopty_tools::resolve::Resolver;
use slopty_tools::view::projects as view;

use crate::link::Link;
use crate::verbs::print_json;

/// A project's limits.
#[derive(Args, Debug, Default)]
pub struct LimitArgs {
    /// How many of its tasks may wait on you, ready to merge or asking you something, before
    /// its agents start no more work; at least 1. Yours to set: an agent's is refused.
    #[arg(long)]
    review_limit: Option<u16>,
}

impl LimitArgs {
    const fn change(&self) -> LimitsChange {
        LimitsChange { review: self.review_limit }
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
        /// The branch finished work lands on; left out, a branch of the project's own
        /// (`slopty/<project>/goal`) made where the orchestrator's checkout is.
        #[arg(long)]
        target: Option<String>,
        /// The command that says a task's work is right (`cargo gate`).
        #[arg(long)]
        verifier: Option<String>,
        /// Push the target branch to the orchestrator's clone's `origin` after each merge.
        #[arg(long)]
        push: bool,
        /// The orchestrator's terminal; this one when run inside a Slopty terminal.
        #[arg(long)]
        orchestrator: Option<String>,
        #[command(flatten)]
        limits: LimitArgs,
        /// Anything to keep with it, as a JSON object.
        #[arg(long)]
        metadata: Option<String>,
        /// The goal to hand its orchestrator.
        #[arg(long)]
        goal: Option<String>,
        /// How far its agents go before they ask you: `ask`, `edits` or `own`.
        #[arg(long, default_value = "ask", value_parser = autonomy)]
        autonomy: Autonomy,
    },
    /// Change a project's orchestrator, verifier, pushing, autonomy, limits or metadata.
    Update {
        /// The project (this session's own when omitted).
        project: Option<String>,
        /// A new orchestrator terminal.
        #[arg(long)]
        orchestrator: Option<String>,
        /// A new verifier command; empty for none.
        #[arg(long)]
        verifier: Option<String>,
        /// Push the target after each merge (`true`), or stop (`false`).
        #[arg(long)]
        push: Option<bool>,
        #[command(flatten)]
        limits: LimitArgs,
        /// New metadata, a JSON object.
        #[arg(long)]
        metadata: Option<String>,
        /// How far its agents go before they ask you: `ask`, `edits` or `own`.
        #[arg(long, value_parser = autonomy)]
        autonomy: Option<Autonomy>,
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
    },
}

/// The autonomy level `--autonomy` names.
fn autonomy(word: &str) -> Result<Autonomy, String> {
    view::autonomy_named(word).ok_or_else(|| format!("{word} is not one of ask, edits, own"))
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

/// `slopty task …`.
#[derive(Subcommand, Debug)]
pub enum TaskCmd {
    /// Start a task, the one way work starts: a new one made from `--title` and the flags after
    /// it, or one the project has with `--task` (made before and refused its start, stopped, or
    /// given back). Claude Code runs it, or another `--agent`, or with `--command` the program
    /// after `--`, on `--worker` or where the server places it.
    Start(Box<StartTask>),
    /// Change a task: move it, say what it is doing, record its branch or verifier, or note
    /// something on the timeline.
    Update(Box<UpdateTask>),
    /// Report a task's work done to the project's orchestrator, through its hooks.
    Report {
        #[command(flatten)]
        which: TaskRef,
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
    /// Start a task's work again with a new agent, on the machine it ran on and in its worktree
    /// there, the work so far kept: the agent on it now is closed, and the new one is told the
    /// task's brief and where the earlier agent's thread is.
    Restart {
        #[command(flatten)]
        which: TaskRef,
        /// Hand it to this agent (`claude`, `codex`, `pi`, an ACP agent); the one it ran last
        /// when omitted.
        #[arg(long)]
        agent: Option<String>,
    },
    /// Push a merged task's target to the forge, as the person: a merge the project did not
    /// push, or one whose push failed. What the queue merged since goes with it.
    Push {
        #[command(flatten)]
        which: TaskRef,
    },
    /// Tell a task's agent something: the words reach it through its hooks as a report does,
    /// never typed into its terminal (fix CI, address the comments, resolve the conflicts).
    /// Inside the orchestrator's session they are the orchestrator's, and marked so.
    Tell {
        #[command(flatten)]
        which: TaskRef,
        /// What to say.
        #[arg(required = true, num_args = 1..)]
        words: Vec<String>,
    },
    /// Wait for news of tasks: a report, a move of state, a terminal gone, checks, a step
    /// ended. Running out of time cancels nothing.
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
    /// One node in full: a task with its brief and Claude Code's own subagents and to-dos, or
    /// with `--task orchestrator` the orchestrator's.
    Get {
        #[command(flatten)]
        which: TaskRef,
    },
}

/// `slopty task update`.
#[derive(Args, Debug)]
pub struct UpdateTask {
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
    /// Run it on this worker, or `anywhere` to let the server choose.
    #[arg(long, value_name = "WORKER")]
    run_on: Option<String>,
    /// Its own verifier; empty for the project's.
    #[arg(long)]
    verifier: Option<String>,
    /// New metadata, a JSON object.
    #[arg(long)]
    metadata: Option<String>,
}

/// `slopty task start`.
#[derive(Args, Debug)]
pub struct StartTask {
    /// The project (this session's own when omitted).
    #[arg(long)]
    project: Option<String>,
    /// A task the project has, by its number, in place of a new one.
    #[arg(long, conflicts_with_all = [
        "title", "brief", "kind", "depends_on", "start_from", "read_only", "verifier", "metadata",
    ])]
    task: Option<String>,
    /// A new task: what it is, in a line.
    #[arg(long, required_unless_present = "task")]
    title: Option<String>,
    /// A new task: what its agent is told to do, as its first prompt.
    #[arg(long, default_value = "")]
    brief: String,
    /// A new task: what sort of work it is, in your words.
    #[arg(long, default_value = "")]
    kind: String,
    /// A new task: a task it needs first; repeatable.
    #[arg(long = "depends-on", value_name = "TASK")]
    depends_on: Vec<String>,
    /// A new task: the one of its dependencies whose work it starts from once that is done and
    /// verified, before it merges; it merges after that task.
    #[arg(long, value_name = "TASK")]
    start_from: Option<String>,
    /// A new task: it only reads, and has nothing to merge.
    #[arg(long)]
    read_only: bool,
    /// A new task: its own verifier, over the project's.
    #[arg(long)]
    verifier: Option<String>,
    /// A new task: anything to keep with it, as a JSON object.
    #[arg(long)]
    metadata: Option<String>,
    /// This worker (name or id), over the one the task names; the server places it when
    /// omitted.
    #[arg(long)]
    worker: Option<String>,
    /// The agent: `claude` (the default), `codex`, `pi`, or an ACP agent by the registry's
    /// name. Each gets Slopty's tools and its role, and the task's brief as its first prompt.
    #[arg(long)]
    agent: Option<String>,
}

impl StartTask {
    /// Which task it starts, and on which worker and agent.
    fn which(self) -> (Option<String>, Which, (Option<String>, Option<String>)) {
        let which = match (self.task, self.title) {
            (Some(task), _) => Which::Made(task),
            (None, title) => Which::New(Box::new(NewTask {
                depends_on: self.depends_on,
                start_from: self.start_from,
                kind: self.kind,
                title: title.unwrap_or_default(),
                brief: self.brief,
                read_only: self.read_only,
                verifier: self.verifier,
                metadata: self.metadata,
            })),
        };
        (self.project, which, (self.worker, self.agent))
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
            push,
            orchestrator,
            limits,
            metadata,
            goal,
            autonomy,
        } => {
            let spec = ProjectSpec {
                project,
                title,
                repo,
                target,
                verifier,
                push,
                orchestrator,
                limits: limits.change(),
                metadata,
                goal,
                autonomy,
            };
            let status = ops::project_create(&mut res, spec, key).await?;
            print_status(&mut res, &status, json).await
        }
        ProjectCmd::Update {
            project,
            orchestrator,
            verifier,
            push,
            limits,
            metadata,
            autonomy,
        } => {
            let limits = limits.change();
            let edit = ProjectEdit { orchestrator, verifier, push, limits, metadata, autonomy };
            let status = ops::project_set(&mut res, project.as_deref(), edit, key).await?;
            print_status(&mut res, &status, json).await
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
        ProjectCmd::Status { project, since } => {
            let status = ops::project_status(link, project.as_deref(), since).await?;
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
        TaskCmd::Start(start) => {
            let (project, which, (worker, agent)) = start.which();
            let on = (worker.as_deref(), agent.as_deref());
            ops::task_start(&mut res, project.as_deref(), which, on, key).await?
        }
        TaskCmd::Update(update) => {
            let UpdateTask {
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
                run_on,
                verifier,
                metadata,
            } = *update;
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
                run_on: match run_on.as_deref() {
                    None => None,
                    Some("anywhere") => Some(RunOn::Anywhere),
                    Some(name) => Some(RunOn::Worker(res.worker(Some(name)).await?)),
                },
                verifier,
                metadata,
            };
            let (project, task) = (which.project.as_deref(), which.task.as_deref());
            ops::task_update(&res, project, task, change, key).await?
        }
        TaskCmd::Report { which, note, artifacts, branch, pr } => {
            let report = Report { note, artifacts, branch, pr };
            let (project, task) = (which.project.as_deref(), which.task.as_deref());
            ops::task_report(link, project, task, report, key).await?
        }
        TaskCmd::Merge { which } => {
            let (project, task) = (which.project.as_deref(), which.task.as_deref());
            ops::task_merge(link, project, task, key).await?
        }
        TaskCmd::Restart { which, agent } => {
            let (project, task) = (which.project.as_deref(), which.task.as_deref());
            ops::task_restart(link, project, task, agent.as_deref(), key).await?
        }
        TaskCmd::Push { which } => {
            let (project, task) = (which.project.as_deref(), which.task.as_deref());
            ops::task_push(link, project, task, key).await?
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser, Debug)]
    struct Cli {
        #[command(subcommand)]
        cmd: TaskCmd,
    }

    /// `task restart` names the task and, when it hands it on, the agent; `task push` names
    /// the task.
    #[test]
    fn restart_and_push_name_their_task() {
        let parse = |args: &[&str]| {
            Cli::try_parse_from(std::iter::once("task").chain(args.iter().copied())).map(|c| c.cmd)
        };
        let TaskCmd::Restart { which, agent } =
            parse(&["restart", "--project", "demo", "--task", "3", "--agent", "codex"]).unwrap()
        else {
            panic!()
        };
        assert_eq!((which.project.as_deref(), which.task.as_deref()), (Some("demo"), Some("3")));
        assert_eq!(agent.as_deref(), Some("codex"));
        let TaskCmd::Restart { agent, .. } = parse(&["restart"]).unwrap() else { panic!() };
        assert_eq!(agent, None, "the agent it ran last");
        let TaskCmd::Push { which } = parse(&["push", "--task", "#4"]).unwrap() else { panic!() };
        assert_eq!(which.task.as_deref(), Some("#4"));
    }
}
