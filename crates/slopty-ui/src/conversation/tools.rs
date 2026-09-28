//! Each tool's words: the title line every call has, and what a group of calls adds up to.
//!
//! One renderer per tool, at three levels (`rows::Level`): the title is here, as data a test
//! can read; the summary and the full body are drawn by the view from the same detail.

use slopty_proto::conversation::{
    AgentRun, Body, Entry, ResultStatus, ShellStatus, Task, ToolCall, ToolDetail, WriteKind,
};

use crate::icons::IconName;

/// How a call is doing, as its title's mark shows it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// Running, or waiting on its result.
    Running,
    /// Done.
    Done,
    /// Failed or was refused.
    Failed,
    /// Stopped by Esc, or refused by the person.
    Stopped,
}

/// A call's title line.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Title {
    /// What kind of thing it did.
    pub icon: IconName,
    /// What it did, in the past tense once done ("Read", "Edited", "Ran").
    pub verb: String,
    /// To what: a path, a pattern, a command's description.
    pub subject: Option<String>,
    /// The subject is code (a pattern, a command), set in the mono face.
    pub code: bool,
    /// A fact after it: a count, an exit code, lines changed as `+a −r`.
    pub meta: Option<String>,
    /// How it is doing.
    pub state: State,
}

/// The last component of a path, for a title; the whole path is the tooltip's.
#[must_use]
pub fn file_name(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or(path)
}

/// `n` and a noun, plural past one: "1 file", "3 files".
#[must_use]
pub fn count(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The first line of `text`, trimmed.
fn first_line(text: &str) -> &str {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default()
}

fn state_of(call: &ToolCall) -> State {
    match &call.detail {
        ToolDetail::Bash(bash) => match bash.status {
            ShellStatus::Running => State::Running,
            ShellStatus::Done => State::Done,
            ShellStatus::Failed => State::Failed,
            ShellStatus::Interrupted | ShellStatus::Killed => State::Stopped,
        },
        ToolDetail::Agent(agent) => match agent.status {
            AgentRun::Running => State::Running,
            AgentRun::Completed => State::Done,
            AgentRun::Failed => State::Failed,
            AgentRun::Killed => State::Stopped,
        },
        _ => match call.result.as_ref().map(|r| r.status) {
            None => State::Running,
            Some(ResultStatus::Ok) => State::Done,
            Some(ResultStatus::Error) => State::Failed,
            Some(ResultStatus::Rejected) => State::Stopped,
        },
    }
}

/// The title of `call`; `tasks` is its thread's task list, which names a task an update
/// refers to by id.
#[must_use]
#[expect(clippy::too_many_lines, reason = "one arm per tool, each short")]
pub fn title(call: &ToolCall, tasks: &[Task]) -> Title {
    let state = state_of(call);
    let running = state == State::Running;
    let past = |now: &str, then: &str| if running { now.to_owned() } else { then.to_owned() };
    let t = |icon, verb: String, subject: Option<String>, code, meta: Option<String>| Title {
        icon,
        verb,
        subject,
        code,
        meta,
        state,
    };
    match &call.detail {
        ToolDetail::Read(read) => {
            let ranged = read.offset.is_some() || read.limit.is_some();
            let range = |start: u64, lines: u64| {
                format!("lines {start}\u{2013}{}", start.saturating_add(lines).saturating_sub(1))
            };
            let meta = match (ranged, read.start_line, read.lines) {
                (true, Some(start), Some(lines)) if lines > 0 => Some(range(start, lines)),
                (true, ..) => read.offset.zip(read.limit).map(|(o, l)| range(o, l)),
                (false, ..) => read.total_lines.or(read.lines).map(|n| count(n, "line", "lines")),
            };
            let name = Some(file_name(&read.path).to_owned());
            t(IconName::FileText, past("Reading", "Read"), name, false, meta)
        }
        ToolDetail::Grep(grep) => {
            let meta = match (grep.lines, grep.files) {
                (Some(lines), _) if grep.mode.as_deref() == Some("content") => {
                    Some(count(lines, "match", "matches"))
                }
                (_, Some(files)) => Some(count(files, "file", "files")),
                _ => None,
            };
            t(
                IconName::Search,
                past("Searching", "Searched"),
                Some(grep.pattern.clone()),
                true,
                meta,
            )
        }
        ToolDetail::Glob(glob) => {
            let meta = glob.files.map(|n| {
                let n = count(n, "file", "files");
                if glob.truncated { format!("{n}+") } else { n }
            });
            t(
                IconName::FolderSearch,
                past("Finding", "Found"),
                Some(glob.pattern.clone()),
                true,
                meta,
            )
        }
        ToolDetail::Bash(bash) => {
            let subject = bash
                .description
                .clone()
                .filter(|d| !d.trim().is_empty())
                .unwrap_or_else(|| first_line(&bash.command.text).to_owned());
            let meta = match (bash.status, bash.exit_code, bash.background) {
                (ShellStatus::Running, _, true) => Some("In the background".to_owned()),
                (ShellStatus::Failed, Some(code), _) => Some(format!("Exit {code}")),
                (ShellStatus::Failed, None, _) => Some("Failed".to_owned()),
                (ShellStatus::Interrupted, ..) => Some("Interrupted".to_owned()),
                (ShellStatus::Killed, ..) => Some("Stopped".to_owned()),
                _ => None,
            };
            t(IconName::SquareTerminal, past("Running", "Ran"), Some(subject), false, meta)
        }
        ToolDetail::Edit(edit) => {
            let meta = crate::kit::changes_text(edit.patch.added, edit.patch.removed);
            t(
                IconName::FilePen,
                past("Editing", "Edited"),
                Some(file_name(&edit.path).to_owned()),
                false,
                meta,
            )
        }
        ToolDetail::Write(write) => {
            let (verb, meta) = match write.kind {
                WriteKind::Create => {
                    (past("Creating", "Created"), crate::kit::changes_text(write.lines, 0))
                }
                WriteKind::Overwrite | WriteKind::Unknown => (
                    past("Writing", "Wrote"),
                    crate::kit::changes_text(write.patch.added, write.patch.removed).or_else(
                        || (write.lines > 0).then(|| count(write.lines.into(), "line", "lines")),
                    ),
                ),
            };
            let icon = if write.kind == WriteKind::Create {
                IconName::FilePlus
            } else {
                IconName::FilePen
            };
            t(icon, verb, Some(file_name(&write.path).to_owned()), false, meta)
        }
        ToolDetail::WebFetch(fetch) => {
            let host = fetch
                .url
                .split("://")
                .nth(1)
                .and_then(|rest| rest.split('/').next())
                .unwrap_or(&fetch.url)
                .to_owned();
            let meta = fetch.code.map(|c| format!("{c}"));
            t(IconName::Globe, past("Fetching", "Fetched"), Some(host), false, meta)
        }
        ToolDetail::WebSearch(search) => {
            let meta = search.results.map(|n| count(n, "result", "results"));
            t(
                IconName::Globe,
                past("Searching the web", "Searched the web"),
                Some(search.query.clone()),
                false,
                meta,
            )
        }
        ToolDetail::Agent(agent) => {
            let subject = agent
                .description
                .clone()
                .unwrap_or_else(|| first_line(&agent.prompt.text).to_owned());
            let kind = agent.agent_type.clone().unwrap_or_else(|| "Agent".to_owned());
            let meta = agent.background.then(|| "In the background".to_owned());
            t(IconName::Bot, kind, Some(subject), false, meta)
        }
        ToolDetail::TaskCreate(task) => t(
            IconName::ListTodo,
            "Added a task".to_owned(),
            Some(task.subject.clone()),
            false,
            None,
        ),
        ToolDetail::TaskUpdate(update) => {
            let subject = update
                .subject
                .clone()
                .or_else(|| {
                    tasks.iter().find(|t| t.id == update.task_id).map(|t| t.subject.clone())
                })
                .unwrap_or_else(|| format!("Task {}", update.task_id));
            let meta = update.to.as_deref().map(|s| status_label(s).to_owned());
            t(IconName::ListChecks, "Updated a task".to_owned(), Some(subject), false, meta)
        }
        ToolDetail::TodoWrite { todos } => {
            let done = todos.iter().filter(|t| t.status == "completed").count();
            let meta = Some(format!("{done} of {} done", todos.len()));
            t(IconName::ListChecks, "Updated the tasks".to_owned(), None, false, meta)
        }
        ToolDetail::Question(question) => {
            let subject = question
                .questions
                .first()
                .map(|q| q.header.clone().unwrap_or_else(|| q.text.clone()));
            let meta = (!question.answers.is_empty()).then(|| "Answered".to_owned());
            t(IconName::MessageSquare, past("Asking", "Asked"), subject, false, meta)
        }
        ToolDetail::Plan { plan } => t(
            IconName::Map,
            "Proposed a plan".to_owned(),
            Some(first_line(&plan.text).trim_start_matches('#').trim().to_owned()),
            false,
            None,
        ),
        ToolDetail::Mcp(mcp) => {
            t(IconName::Plug, mcp.server.clone(), Some(mcp.tool.clone()), false, None)
        }
        ToolDetail::Other { .. } if call.name == "ToolSearch" => {
            t(IconName::Wrench, past("Loading tools", "Loaded tools"), None, false, None)
        }
        ToolDetail::Other { input } => {
            let subject = Some(first_line(&input.text).to_owned()).filter(|s| s != "{}");
            t(IconName::Wrench, call.name.clone(), subject, true, None)
        }
    }
}

/// A task status as the list says it.
#[must_use]
pub fn status_label(status: &str) -> &str {
    match status {
        "pending" => "To do",
        "in_progress" => "In progress",
        "completed" => "Done",
        "deleted" => "Removed",
        other => other,
    }
}

/// What a run of looking calls adds up to: "3 reads, 2 searches".
#[must_use]
pub fn explored(calls: &[&Entry]) -> String {
    let (mut reads, mut searches, mut fetches, mut other) = (0_u64, 0_u64, 0_u64, 0_u64);
    for entry in calls {
        let Body::Tool(call) = &entry.body else { continue };
        match &call.detail {
            ToolDetail::Read(_) => reads = reads.saturating_add(1),
            ToolDetail::Grep(_) | ToolDetail::Glob(_) | ToolDetail::WebSearch(_) => {
                searches = searches.saturating_add(1);
            }
            ToolDetail::WebFetch(_) => fetches = fetches.saturating_add(1),
            _ => other = other.saturating_add(1),
        }
    }
    let parts = [
        (reads > 0).then(|| count(reads, "read", "reads")),
        (searches > 0).then(|| count(searches, "search", "searches")),
        (fetches > 0).then(|| count(fetches, "fetch", "fetches")),
        (other > 0).then(|| count(other, "lookup", "lookups")),
    ];
    parts.into_iter().flatten().collect::<Vec<_>>().join(", ")
}

/// What a run of task keeping adds up to: "2 added, 1 updated".
#[must_use]
pub fn tasks_kept(calls: &[&Entry]) -> String {
    let (mut added, mut updated) = (0_u64, 0_u64);
    for entry in calls {
        let Body::Tool(call) = &entry.body else { continue };
        match &call.detail {
            ToolDetail::TaskCreate(_) => added = added.saturating_add(1),
            _ => updated = updated.saturating_add(1),
        }
    }
    let parts = [
        (added > 0).then(|| format!("{added} added")),
        (updated > 0).then(|| format!("{updated} updated")),
    ];
    parts.into_iter().flatten().collect::<Vec<_>>().join(", ")
}

/// A token count, short: `13.4k`.
#[must_use]
pub fn tokens(n: u64) -> String {
    #[expect(clippy::cast_precision_loss, reason = "a label, not arithmetic")]
    let k = n as f64 / 1000.0;
    match n {
        0..1_000 => format!("{n}"),
        1_000..100_000 => format!("{k:.1}k"),
        _ => format!("{k:.0}k"),
    }
}

/// What a call being prepared is about, from the input the model has written so far.
///
/// Once that is a whole JSON object, its subject (the command, the file's name, the pattern,
/// the address), as its title will say it; before that, the last line written.
#[must_use]
pub fn preparing(input: &str) -> String {
    const SUBJECTS: [&str; 8] =
        ["command", "file_path", "path", "pattern", "url", "query", "description", "prompt"];
    let whole = serde_json::from_str::<serde_json::Value>(input).ok();
    let subject = whole.as_ref().and_then(|v| {
        SUBJECTS.iter().find_map(|key| {
            let text = v.get(*key)?.as_str()?;
            Some(if key.ends_with("path") { file_name(text) } else { first_line(text) })
        })
    });
    subject.unwrap_or_else(|| input.lines().last().unwrap_or_default().trim()).to_owned()
}

#[cfg(test)]
mod tests {
    /// A call being prepared shows its subject once its input is whole, and the line being
    /// written before that.
    #[test]
    fn a_call_being_prepared_is_named_by_its_subject() {
        assert_eq!(preparing(r#"{"command": "echo hi", "description": "Say hi"}"#), "echo hi");
        assert_eq!(preparing(r#"{"file_path": "/work/notes.txt", "content": "x"}"#), "notes.txt");
        assert_eq!(preparing(r#"{"command": "echo"#), r#"{"command": "echo"#);
        assert_eq!(preparing(r#"{"todos": []}"#), r#"{"todos": []}"#);
        assert_eq!(preparing(""), "");
    }

    use slopty_proto::conversation::ThreadId;

    use super::*;
    use crate::conversation::fixtures::scenario;

    fn titles(name: &str) -> Vec<String> {
        let model = scenario(name);
        let main = model.thread(&ThreadId::Main).unwrap();
        main.entries()
            .iter()
            .filter_map(|e| match &e.body {
                Body::Tool(call) => {
                    let t = title(call.as_ref(), main.tasks());
                    Some(format!(
                        "{} | {} | {} | {:?}",
                        t.verb,
                        t.subject.unwrap_or_default(),
                        t.meta.unwrap_or_default(),
                        t.state
                    ))
                }
                _ => None,
            })
            .collect()
    }

    /// Every captured call reads as a line a person would write.
    #[test]
    fn each_call_reads_as_what_it_did() {
        assert_eq!(
            titles("tools"),
            [
                "Loaded tools |  |  | Done",
                "Added a task | Survey files |  | Done",
                "Added a task | Write notes |  | Done",
                "Updated a task | Survey files | In progress | Done",
                "Found | *.rs | 1 file | Done",
                "Searched | println | 1 match | Done",
                "Ran | Print two lines |  | Done",
                "Ran | Try to list a non-existent file | Exit 2 | Failed",
                "Ran | Background sleep and echo |  | Done",
                "Created | notes.md | +2 | Done",
                "general-purpose | Count lines |  | Done",
                "Updated a task | Survey files | Done | Done",
                "Updated a task | Write notes | Done | Done",
            ]
        );
        assert_eq!(
            titles("edit"),
            [
                "Read | notes.txt | 4 lines | Done",
                "Edited | notes.txt | +1 \u{2212}1 | Done",
                "Wrote | notes.txt | +1 | Done",
                "Read | notes.txt | lines 1\u{2013}2 | Done",
                "Read | notes.txt | lines 2\u{2013}3 | Done",
            ]
        );
        assert_eq!(titles("interrupt"), ["Ran | Wait | Interrupted | Stopped"]);
    }

    #[test]
    fn groups_add_up_what_they_hold() {
        let model = scenario("tools");
        let main = model.thread(&ThreadId::Main).unwrap();
        let pick =
            |ids: &[&str]| -> Vec<&Entry> { ids.iter().filter_map(|id| main.entry(id)).collect() };
        assert_eq!(explored(&pick(&["toolu_05", "toolu_06"])), "2 searches");
        assert_eq!(tasks_kept(&pick(&["toolu_02", "toolu_03", "toolu_04"])), "2 added, 1 updated");
        assert_eq!(tokens(13_422), "13.4k");
        assert_eq!(tokens(812), "812");
    }
}
