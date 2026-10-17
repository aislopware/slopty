//! What a thread's items say beyond zeron's parts, in the TUI's words.
//!
//! A call's facts (its line, what came of it, its timer, the task it became, a subagent's
//! progress), a background task's notice row, a quiet notice row and a request tried again, and
//! the card of a page a call showed.
//!
//! The transcript ([`super::transcript`]) puts them on its parts; the tray's ids
//! ([`super::work::task_row`], [`super::work::chat_row`]) link the two.

use slopty_proto::thread::detail::{Clipped, ExecStatus};
use slopty_proto::thread::{
    BackgroundTask, Cap, ItemBody, Notice, Phase, TaskNote, ThreadId, ThreadState, ToolCall,
    ToolDetail, ToolState, TurnId, TurnState, kind,
};
use slopty_proto::visual::{PageDetail, PageSource};
use slopty_view_doc::{
    LinkRow, NoteLine, NoteRow, PageCard, RetryLine, SubagentProgress, TaskRow, ToolFacts,
    ToolTimer, bytes_short, duration_words, tokens_short,
};

use super::transcript::at;
use super::work::{chat_row, figures, kind_of, retry_line, task_row};

/// A chat another agent started under a call, as the call's card reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChildRun {
    /// The id the shell knows its chat by.
    pub chat: String,
    /// It is at work, or waits on the person.
    pub running: bool,
    /// Where it stands.
    pub phase: Phase,
    /// What it is doing now: its newest call that runs or waits ("Edit src/main.rs").
    pub step: Option<String>,
    /// What it asks of the person, while it does.
    pub needs_you: Option<String>,
    /// When its turn under way began, else when it began.
    pub started_ms: Option<slopty_core::WallMs>,
}

impl ChildRun {
    /// A child as its row says it, known to the shell as `chat`.
    #[must_use]
    pub fn of(row: &slopty_proto::thread::wire::ThreadRow, chat: String) -> Self {
        Self {
            chat,
            running: super::rows::running(row) || row.status.phase == Phase::NeedsYou,
            phase: row.status.phase,
            step: row.doing.clone(),
            needs_you: super::work::child_asks(row),
            started_ms: row.began_ms.or(Some(row.created_ms)).filter(|ms| !ms.is_zero()),
        }
    }
}

/// How a page path begins ([`page_key`]).
pub const PAGE_SCHEME: &str = "slopty-page:";

/// The key the app hosts the page `thread`'s call `item` showed by.
#[must_use]
pub fn page_key(thread: ThreadId, item: &str) -> String {
    format!("{PAGE_SCHEME}{thread}/{item}")
}

/// The thread and call a page key names.
#[must_use]
pub fn page_of(key: &str) -> Option<(ThreadId, &str)> {
    let (thread, item) = key.strip_prefix(PAGE_SCHEME)?.split_once('/')?;
    Some((thread.parse().ok()?, item)).filter(|(_, item)| !item.is_empty())
}

/// The card of the page `thread`'s call `item` shows, when it shows one.
#[must_use]
pub fn page_card(thread: ThreadId, item: &str, call: &ToolCall) -> Option<PageCard> {
    let Some(ToolDetail::Page(PageDetail { title, source })) = &call.detail else { return None };
    let mut card = PageCard {
        id: format!("{item}:page"),
        page: page_key(thread, item),
        title: title.clone(),
        streaming: call.state == ToolState::Streaming,
        failed: matches!(
            call.state,
            ToolState::Failed | ToolState::Cancelled | ToolState::Rejected
        ),
        ..PageCard::default()
    };
    match source {
        PageSource::Html(_) => {
            PageCard::HTML.clone_into(&mut card.kind);
            slopty_proto::visual::SHOW_PAGE.clone_into(&mut card.source);
        }
        PageSource::File(path) => {
            PageCard::FILE.clone_into(&mut card.kind);
            card.source.clone_from(path);
            card.path = Some(path.clone());
        }
        PageSource::App(app) => {
            PageCard::APP.clone_into(&mut card.kind);
            card.source = format!("{} · {}", app.server, app.tool);
            card.server = Some(app.server.clone());
            card.tool = Some(app.tool.clone());
            card.resource_uri = Some(app.resource_uri.clone());
            card.display_mode.clone_from(&app.display_mode);
        }
    }
    if card.title.trim().is_empty() {
        card.title = match card.kind.as_str() {
            PageCard::FILE => card.source.rsplit('/').next().unwrap_or_default().to_owned(),
            PageCard::APP => card.tool.clone().unwrap_or_default(),
            _ => "Page".to_owned(),
        };
    }
    Some(card)
}

/// A string field of a call's input.
fn input_text(input: Option<&serde_json::Value>, keys: &[&str]) -> Option<String> {
    let input = input?;
    keys.iter().find_map(|key| match input.get(*key)? {
        serde_json::Value::String(text) => Some(text.trim().to_owned()).filter(|t| !t.is_empty()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

/// A number field of a call's input.
fn input_number(input: Option<&serde_json::Value>, keys: &[&str]) -> Option<u64> {
    let input = input?;
    keys.iter().find_map(|key| input.get(*key)?.as_u64())
}

/// A text cut to one line of at most 60 characters, for a call's line.
fn short(text: &str) -> String {
    super::text::line(text, 60)
}

/// The file a `sed -i` command edits in place with one substitution, as the TUI shows such a
/// command: an edit of that file (`Update(src/lib.rs)`).
fn sed_edit(command: &str) -> Option<String> {
    let words: Vec<&str> = command.split_whitespace().collect();
    let (first, rest) = words.split_first()?;
    if *first != "sed" || command.contains(['|', ';', '&', '>', '<', '`', '$']) {
        return None;
    }
    let mut in_place = false;
    let mut script = None;
    let mut files = Vec::new();
    let mut args = rest.iter();
    while let Some(arg) = args.next() {
        match *arg {
            // macOS's `-i ''` takes the empty suffix as its own word.
            "-i" | "--in-place" => {
                in_place = true;
                if args.as_slice().first().is_some_and(|next| matches!(*next, "''" | "\"\"")) {
                    args.next();
                }
            }
            "-e" | "--expression" => script = args.next().copied(),
            "-E" | "-r" | "--regexp-extended" => {}
            flag if flag.starts_with("-i") || flag.starts_with("--in-place=") => in_place = true,
            flag if flag.starts_with('-') => return None,
            word if script.is_none() => script = Some(word),
            file => files.push(file),
        }
    }
    let script = script?.trim_matches(['\'', '"']);
    let substitutions = script.split(';').filter(|s| !s.trim().is_empty()).count();
    let one = script.starts_with('s') && substitutions == 1 && script.len() > 3;
    match files.as_slice() {
        [file] if in_place && one => {
            let file = file.trim_matches(['\'', '"']);
            Some(format!("Update({})", file.rsplit('/').next().unwrap_or(file)))
        }
        _ => None,
    }
}

/// A call's line in the TUI's words, for the tools zeron draws as `Tool · {name}`.
fn title(call: &ToolCall, input: Option<&serde_json::Value>) -> Option<String> {
    let arg = |keys: &[&str]| input_text(input, keys).map(|t| short(&t));
    let command = match &call.detail {
        Some(ToolDetail::Exec(exec)) => Some(exec.command.text.as_str()),
        _ => None,
    };
    if let Some(edit) = command.and_then(sed_edit) {
        return Some(edit);
    }
    Some(match call.name.as_str() {
        "PowerShell" => format!("PowerShell({})", short(command?)),
        "SendFile" => {
            let files = input.and_then(|i| i.get("files")).and_then(serde_json::Value::as_array);
            let names: Vec<String> = files
                .into_iter()
                .flatten()
                .filter_map(|f| f.as_str().or_else(|| f.get("path")?.as_str()))
                .map(|f| f.rsplit('/').next().unwrap_or(f).to_owned())
                .collect();
            let to = arg(&["to"]).map(|to| format!(" → {to}")).unwrap_or_default();
            format!("SendFile({}{to})", short(&names.join(", ")))
        }
        "PushNotification" => format!("PushNotification({})", arg(&["message"])?),
        "Workflow" => {
            let what = arg(&["scriptPath", "script_path", "name"]);
            let script = arg(&["script"]);
            match (what, script) {
                (Some(what), Some(script)) => format!("Workflow({what} · {script})"),
                (Some(one), None) | (None, Some(one)) => format!("Workflow({one})"),
                (None, None) => "Workflow".to_owned(),
            }
        }
        "ReportFindings" => {
            let found = input
                .and_then(|i| i.get("findings"))
                .and_then(serde_json::Value::as_array)
                .map_or(0, Vec::len);
            let found = count(u64::try_from(found).unwrap_or(0), "finding", "findings");
            match arg(&["level"]) {
                Some(level) => format!("Code review({level} · {found})"),
                None => format!("Code review({found})"),
            }
        }
        "RemoteTrigger" => {
            let parts: Vec<String> =
                [arg(&["action"]), arg(&["trigger_id", "id"])].into_iter().flatten().collect();
            format!("RemoteTrigger({})", parts.join(" "))
        }
        "FetchInboxMessage" => {
            let id: String = arg(&["file_id", "id"])?.chars().take(13).collect();
            format!("FetchInboxMessage(read {id})")
        }
        "propose_skills" => {
            let proposals = input
                .and_then(|i| i.get("proposals"))
                .and_then(serde_json::Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let names: Vec<&str> = proposals
                .iter()
                .filter_map(|p| p.get("name").and_then(serde_json::Value::as_str))
                .collect();
            let n = count(u64::try_from(proposals.len()).unwrap_or(0), "skill", "skills");
            format!("propose_skills(Propose {n}: {})", short(&names.join(", ")))
        }
        "ListMcpResourcesTool" => match arg(&["server"]) {
            Some(server) => format!("listMcpResources({server})"),
            None => "listMcpResources".to_owned(),
        },
        "ReadMcpResourceTool" | "ReadMcpResourceDirTool" => {
            let verb = if call.name == "ReadMcpResourceTool" {
                "readMcpResource"
            } else {
                "readMcpResourceDir"
            };
            let parts: Vec<String> =
                [arg(&["server"]), arg(&["uri"])].into_iter().flatten().collect();
            format!("{verb}({})", parts.join(", "))
        }
        "GetTask" => format!("GetTask({})", arg(&["taskId", "task_id"])?),
        // The advisor model's review of the conversation (a server tool).
        "advisor" => "Advisor".to_owned(),
        "Monitor" => format!("Monitor({})", arg(&["description", "command"])?),
        "SendMessage" => match arg(&["to", "recipient", "target"]) {
            Some(to) => format!("SendMessage → {to}"),
            None => "SendMessage".to_owned(),
        },
        "ScheduleWakeup" => {
            match input_number(input, &["delaySeconds", "delay_seconds", "seconds"]) {
                Some(secs) => {
                    format!("ScheduleWakeup · in {}", duration_words(secs.saturating_mul(1_000)))
                }
                None => match arg(&["at", "when"]) {
                    Some(when) => format!("ScheduleWakeup · at {when}"),
                    None => "ScheduleWakeup".to_owned(),
                },
            }
        }
        "CronCreate" => format!("Cron({})", arg(&["cron", "schedule"])?),
        "CronDelete" => format!("CronDelete({})", arg(&["id", "job_id"])?),
        "Skill" => format!("Skill({})", arg(&["skill", "name", "command"])?),
        "TaskStop" | "KillShell" | "KillBash" => {
            format!("Stop {}", arg(&["task_id", "shell_id", "id"])?)
        }
        "TaskOutput" | "BashOutput" => {
            format!("Read output of {}", arg(&["task_id", "bash_id", "shell_id", "id"])?)
        }
        "TeamCreate" => format!("TeamCreate({})", arg(&["team_name", "name"])?),
        "EnterWorktree" => format!("EnterWorktree({})", arg(&["name", "path"])?),
        "ExitWorktree" => "ExitWorktree".to_owned(),
        "EnterPlanMode" => "Entered plan mode".to_owned(),
        _ => return None,
    })
}

/// An HTTP status's reason, as the TUI's fetch line says it.
const fn reason(code: u64) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "",
    }
}

/// `n` `one`, or `n` of the plural.
fn count(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Claude Code's "Shell cwd was reset to {dir}", which it writes to a command's standard error
/// when the command left the folders it may work in.
fn cwd_reset(exec: &slopty_proto::thread::detail::ExecDetail) -> Option<String> {
    let stderr = exec.stderr.as_ref()?;
    stderr
        .text
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| line.starts_with("Shell cwd was reset to "))
        .map(str::to_owned)
}

/// A call the person turned down, as the TUI's line says it: "User rejected update to main.rs",
/// "User rejected write to notes.md", else "Tool use rejected".
fn rejected(call: &ToolCall) -> String {
    let input: Option<serde_json::Value> = serde_json::from_str(&call.input.text).ok();
    let path = match &call.detail {
        Some(ToolDetail::Edit(edit)) => Some(edit.path.clone()),
        Some(ToolDetail::Write(write)) => Some(write.path.clone()),
        _ => input_text(input.as_ref(), &["file_path", "notebook_path", "path"]),
    };
    let name = path.as_deref().map(|p| p.rsplit('/').next().unwrap_or(p).to_owned());
    match (call.name.as_str(), name) {
        ("Write", Some(name)) => match &call.detail {
            Some(ToolDetail::Write(write)) if write.created == Some(false) => {
                format!("User rejected update to {name}")
            }
            _ => format!("User rejected write to {name}"),
        },
        ("Edit" | "MultiEdit" | "NotebookEdit", Some(name)) => {
            format!("User rejected update to {name}")
        }
        _ => "Tool use rejected".to_owned(),
    }
}

/// What came of a call, as the TUI's `⎿` line says it, and whether that reads as a failure.
fn result(call: &ToolCall, task: Option<&BackgroundTask>) -> Option<(String, bool)> {
    let finished = call.state.is_final();
    match call.state {
        // The person's own no reads quiet, as the TUI's dim line.
        ToolState::Rejected => return Some((rejected(call), false)),
        ToolState::Cancelled => return Some(("Interrupted".to_owned(), true)),
        ToolState::Pending { .. } => return Some(("Waiting for permission…".to_owned(), false)),
        _ => {}
    }
    let line = match &call.detail {
        Some(ToolDetail::Write(write)) if finished && write.created == Some(true) => {
            format!("Wrote {}", count(u64::from(write.lines), "line", "lines"))
        }
        Some(ToolDetail::Read(read)) if finished => {
            let lines = read.lines.or(read.total_lines)?;
            format!("Read {}", count(lines, "line", "lines"))
        }
        Some(ToolDetail::Search(search)) if finished => {
            let mut line = match (search.matches, search.files) {
                // No files beside lines found is a count the agent did not make, not zero.
                (Some(m), Some(0)) if m > 0 => format!("Found {}", count(m, "line", "lines")),
                (Some(m), Some(f)) if m != f && m > 0 => {
                    format!(
                        "Found {} across {}",
                        count(m, "line", "lines"),
                        count(f, "file", "files")
                    )
                }
                (_, Some(f)) => format!("Found {}", count(f, "file", "files")),
                (Some(m), None) => format!("Found {}", count(m, "match", "matches")),
                (None, None) => return None,
            };
            if search.truncated {
                line.push_str(" (cut short)");
            }
            line
        }
        Some(ToolDetail::Fetch(fetch)) if finished => {
            let code = fetch.code?;
            let size = fetch.bytes.map(|b| format!("{} ", bytes_short(b))).unwrap_or_default();
            let words = reason(code);
            let status =
                if words.is_empty() { code.to_string() } else { format!("{code} {words}") };
            return Some((format!("Received {size}({status})"), code >= 400));
        }
        Some(ToolDetail::WebSearch(search)) if finished => {
            let results = search.results.or_else(|| {
                (!search.links.is_empty()).then(|| u64::try_from(search.links.len()).unwrap_or(0))
            });
            match results {
                Some(n) => format!("Did 1 search · {}", count(n, "result", "results")),
                None => "Did 1 search".to_owned(),
            }
        }
        Some(ToolDetail::Exec(exec)) => {
            if exec.background {
                return Some((
                    match &exec.task {
                        Some(task) => format!("Running in the background · task {task}"),
                        None => "Running in the background".to_owned(),
                    },
                    false,
                ));
            }
            // What the agent made of its exit code: no failure ("No matches found").
            if let Some(meant) = exec.interpretation.as_ref().filter(|_| finished) {
                return Some((meant.clone(), false));
            }
            match (exec.status, exec.exit_code) {
                (ExecStatus::Interrupted, _) => return Some(("Interrupted".to_owned(), true)),
                (_, Some(code)) if code != 0 => {
                    return Some((format!("exit code {code}"), true));
                }
                // Claude Code put the shell back where it may work: its own line.
                (ExecStatus::Done, _) if cwd_reset(exec).is_some() => cwd_reset(exec)?,
                (ExecStatus::Done, _)
                    if call.output.as_ref().is_none_or(|out| out.text.trim().is_empty())
                        && exec.stderr.as_ref().is_none_or(|e| e.text.trim().is_empty()) =>
                {
                    "(No output)".to_owned()
                }
                _ => return None,
            }
        }
        _ if call.name == "Monitor" => {
            let task = task?;
            let mut line = format!("Monitor started · task {}", task.id);
            if task.persistent {
                line.push_str(" · persistent");
            } else if let Some(ms) = task.timeout_ms {
                line.push_str(" · timeout ");
                line.push_str(&duration_words(ms));
            }
            line
        }
        _ => return None,
    };
    Some((line, false))
}

/// Characters a chip's path takes before its middle is cut.
const PATH_CHARS: usize = 48;

/// A path as the TUI's header shows it.
///
/// Inside the folder `cwd` relative to it, else under a home folder as `~/…`, else whole; past
/// 48 characters its middle cut so the file's name stays (`crates/…/view_model/facts.rs`).
#[must_use]
pub fn path_words(path: &str, cwd: &str) -> String {
    // A NUL byte hides what follows it in a terminal; the TUI says so instead.
    if path.contains('\0') {
        return "Path hidden (unsupported characters)".to_owned();
    }
    let cwd = cwd.trim_end_matches('/');
    let relative = path
        .strip_prefix(cwd)
        .filter(|_| !cwd.is_empty())
        .and_then(|rest| rest.strip_prefix('/'))
        .filter(|rest| !rest.is_empty());
    let homed = || {
        let (root, rest) = ["/Users/", "/home/"]
            .iter()
            .find_map(|root| Some((*root, path.strip_prefix(root)?)))
            .or_else(|| {
                Some(("/root", path.strip_prefix("/root").filter(|r| r.starts_with('/'))?))
            })?;
        let rest = if root == "/root" { rest } else { rest.split_once('/').map(|(_, r)| r)? };
        Some(format!("~/{}", rest.trim_start_matches('/')))
    };
    let words = relative.map(str::to_owned).or_else(homed).unwrap_or_else(|| path.to_owned());
    middle_cut(&words, PATH_CHARS)
}

/// `path` cut in its middle to `max` characters, its last part kept whole where it fits.
fn middle_cut(path: &str, max: usize) -> String {
    let chars = path.chars().count();
    if chars <= max {
        return path.to_owned();
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    let tail_chars = name.chars().count().saturating_add(1);
    if tail_chars.saturating_add(4) > max {
        // A name too long to keep whole: its start, then the cut.
        let mut cut: String = path.chars().take(max.saturating_sub(1)).collect();
        cut.push('…');
        return cut;
    }
    let head: String =
        path.chars().take(max.saturating_sub(tail_chars).saturating_sub(1)).collect();
    format!("{head}…/{name}")
}

/// What a command wrote to its standard error, Claude Code's own notes in it left out: the
/// sandbox's report and "Shell cwd was reset to …" (which is the call's line).
fn stderr_words(exec: &slopty_proto::thread::detail::ExecDetail) -> Option<String> {
    let text = &exec.stderr.as_ref()?.text;
    let mut kept = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(start) = rest.find("<sandbox_violations>") {
        kept.push_str(rest.get(..start).unwrap_or_default());
        rest = rest
            .get(start..)
            .and_then(|from| from.find("</sandbox_violations>").map(|end| (from, end)))
            .and_then(|(from, end)| from.get(end.saturating_add("</sandbox_violations>".len())..))
            .unwrap_or_default();
    }
    kept.push_str(rest);
    let lines: Vec<&str> =
        kept.lines().filter(|line| !line.trim().starts_with("Shell cwd was reset to ")).collect();
    let words = lines.join("\n");
    Some(words.trim_end().to_owned()).filter(|w| !w.trim().is_empty())
}

/// What `call` (item `item` of `state`) says beyond zeron's chip; `child` its subagent's run,
/// where it started one. `None` when it says nothing more.
#[must_use]
pub fn tool_facts(
    item: &str,
    call: &ToolCall,
    state: &ThreadState,
    child: Option<&ChildRun>,
) -> Option<ToolFacts> {
    let input: Option<serde_json::Value> = serde_json::from_str(&call.input.text).ok();
    let note = state.calls.iter().find_map(|(id, note)| (id.0 == item).then_some(note));
    let task_id = note.and_then(|n| n.task.clone()).or_else(|| match &call.detail {
        Some(ToolDetail::Exec(exec)) => exec.task.clone(),
        _ => None,
    });
    let task = state.tasks.iter().find(|task| {
        task_id.as_deref() == Some(task.id.as_str())
            || task.item.as_ref().is_some_and(|i| i.0 == item)
    });
    let (mut result_line, mut failed) = result(call, task).unzip();
    // How it ended in the agent's own words, where its state alone does not say.
    if let Some(said) = note.and_then(|n| n.said.clone()).filter(|s| !s.trim().is_empty()) {
        // The person's own no is not a failure, whatever words it ended with.
        failed = Some(call.state == ToolState::Failed);
        result_line = Some(said);
    }
    let running = matches!(call.state, ToolState::Running | ToolState::Pending { .. });
    let timer = note.filter(|_| running).and_then(|note| {
        Some(ToolTimer {
            elapsed_ms: note.elapsed_ms?,
            at: at(note.at_ms),
            timeout_ms: input_number(input.as_ref(), &["timeout", "timeout_ms"])
                .or_else(|| task.and_then(|t| t.timeout_ms)),
        })
    });
    let background = match &call.detail {
        Some(ToolDetail::Exec(exec)) => Some(exec.background),
        Some(ToolDetail::Agent(agent)) => Some(agent.background),
        _ => None,
    };
    let backgroundable = call.state == ToolState::Running
        && background == Some(false)
        && state.meta.can(Cap::BACKGROUND);
    let retry = note.and_then(|note| {
        let notice = Notice {
            kind: Notice::API_ERROR.to_owned(),
            text: Clipped::whole(""),
            retry: note.retry,
        };
        retry_line(&notice, at(note.at_ms))
    });
    let spawn = matches!(call.detail, Some(ToolDetail::Agent(_))) || call.kind == kind::AGENT;
    let subagent_progress = spawn.then(|| progress(call, state, task, child));
    let facts = ToolFacts {
        title: note.and_then(|n| n.title.clone()).or_else(|| title(call, input.as_ref())),
        result: result_line,
        failed: failed.unwrap_or(false),
        rejected: call.state == ToolState::Rejected,
        timer,
        task_row: task_id.as_deref().map(task_row),
        backgroundable,
        retry,
        subagent_progress,
        detail: file_path(call)
            .map(|path| (path, path_words(path, &state.meta.cwd)))
            .and_then(|(path, words)| (words != path).then_some(words)),
        stderr: match &call.detail {
            Some(ToolDetail::Exec(exec)) => stderr_words(exec),
            _ => None,
        },
        changed: match &call.detail {
            Some(ToolDetail::Edit(edit)) => Some(edit.patch.clone()),
            Some(ToolDetail::Write(write)) => Some(write.patch.clone()),
            _ => None,
        }
        .filter(|_| call.state == ToolState::Completed)
        .map(|patch| [u64::from(patch.added), u64::from(patch.removed)])
        .filter(|[added, removed]| added.saturating_add(*removed) > 0),
        landed: match &call.detail {
            Some(ToolDetail::Exec(exec)) if call.state == ToolState::Completed => {
                landed(&exec.command.text, call.output.as_ref().map_or("", |out| out.text.as_str()))
            }
            _ => None,
        },
        thought_ms: None,
    };
    (facts != ToolFacts::default()).then_some(facts)
}

/// What `command` landed by what it printed (`output`), as the TUI's group line says it.
///
/// A commit's short sha ("committed 1a2b3c4", "amended commit …", "cherry-picked …"), where a push
/// went ("pushed to main"), a merge or rebase ("merged feature", "rebased onto main"), and a
/// pull or merge request the GitHub or GitLab CLI made or changed ("created PR #12", "merged MR
/// !7"). A dry run lands nothing.
pub fn landed(command: &str, output: &str) -> Option<String> {
    let words: Vec<&str> = command.split_whitespace().collect();
    if words.iter().any(|w| *w == "--dry-run" || *w == "-n") && words.contains(&"push") {
        return None;
    }
    let mut said = Vec::new();
    // The git subcommand after `git` and its `-c k=v` / `--opt=value` flags.
    let git_verb = |verb: &str| {
        words.windows(2).any(|pair| matches!(pair, ["git", v] if *v == verb))
            || words.iter().position(|w| *w == "git").is_some_and(|at| {
                words
                    .iter()
                    .skip(at.saturating_add(1))
                    .find(|w| !w.starts_with('-') && !w.contains('='))
                    .is_some_and(|w| *w == verb)
            })
    };
    let committed = |kind: &str| {
        output.lines().find_map(|line| {
            let inner = line.trim().strip_prefix('[')?.split_once(']')?.0;
            let sha = inner.split_whitespace().last()?;
            (sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit()))
                .then(|| format!("{kind} {sha}"))
        })
    };
    if git_verb("commit") {
        let kind = if words.contains(&"--amend") { "amended commit" } else { "committed" };
        said.extend(committed(kind));
    }
    if git_verb("cherry-pick") {
        said.extend(committed("cherry-picked"));
    }
    if git_verb("push") {
        let to = output.lines().find_map(|line| {
            let (_, dest) = line.split_once(" -> ")?;
            let dest = dest.split_whitespace().next()?;
            Some(dest.strip_prefix("refs/heads/").unwrap_or(dest).to_owned())
        });
        said.extend(to.map(|branch| format!("pushed to {branch}")));
    }
    let target = |verb: &str| {
        let at = words.iter().position(|w| *w == verb)?;
        words
            .iter()
            .skip(at.saturating_add(1))
            .find(|w| !w.starts_with('-'))
            .map(|w| (*w).to_owned())
    };
    let refused = |output: &str| output.contains("CONFLICT") || output.contains("error:");
    if git_verb("merge") && !refused(output) {
        said.extend(target("merge").map(|r| format!("merged {r}")));
    }
    if git_verb("rebase") && !refused(output) {
        said.extend(target("rebase").map(|r| format!("rebased onto {r}")));
    }
    said.extend(request_landed(&words, output));
    (!said.is_empty()).then(|| said.join(" · "))
}

/// A pull request (`gh pr …`) or merge request (`glab mr …`) a command made or changed, by its
/// number from the command or the address it printed.
fn request_landed(words: &[&str], output: &str) -> Option<String> {
    let at = words.windows(2).position(|pair| matches!(pair, ["gh", "pr"] | ["glab", "mr"]))?;
    let (mark, noun) = if words.get(at) == Some(&"glab") { ("!", "MR") } else { ("#", "PR") };
    let verb = *words.get(at.saturating_add(2))?;
    let rest = words.get(at.saturating_add(3)..).unwrap_or_default();
    let flag = |name: &str| rest.contains(&name);
    let action = match verb {
        "create" => "created",
        "edit" | "update" if flag("--ready") => "marked ready",
        "edit" | "update" if flag("--draft") => "marked draft",
        "edit" | "update" => "edited",
        "merge" if flag("--disable-auto") => "disabled auto-merge on",
        "merge" if flag("--auto") => "enabled auto-merge on",
        "merge" => "merged",
        "comment" | "note" => "commented on",
        "close" => "closed",
        "reopen" => "reopened",
        "ready" if flag("--undo") => "marked draft",
        "ready" => "marked ready",
        _ => return None,
    };
    let printed = output.split_whitespace().find_map(|word| {
        let (_, tail) = word
            .split_once("/pull/")
            .or_else(|| word.split_once("/merge_requests/"))
            .or_else(|| word.split_once("/pull-requests/"))?;
        let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
        (!digits.is_empty()).then_some(digits)
    });
    let named = rest
        .iter()
        .find(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_digit()))
        .map(|n| (*n).to_owned());
    let number = printed.or(named)?;
    Some(format!("{action} {noun} {mark}{number}"))
}

/// The file a call's chip names.
fn file_path(call: &ToolCall) -> Option<&str> {
    match &call.detail {
        Some(ToolDetail::Read(read)) => Some(&read.path),
        Some(ToolDetail::Edit(edit)) => Some(&edit.path),
        Some(ToolDetail::Write(write)) => Some(&write.path),
        _ => None,
    }
    .filter(|path| !path.is_empty())
    .map(String::as_str)
}

/// A subagent's progress on its spawn card: its call's figures once it ended, its task's as it
/// goes, its run's step and whether it waits on the person.
fn progress(
    call: &ToolCall,
    state: &ThreadState,
    task: Option<&BackgroundTask>,
    child: Option<&ChildRun>,
) -> SubagentProgress {
    let agent = match &call.detail {
        Some(ToolDetail::Agent(agent)) => Some(agent),
        _ => None,
    };
    let usage = task.and_then(|t| t.usage);
    let tool_uses =
        agent.and_then(|a| a.tool_uses).or_else(|| usage.map(|u| u.tool_uses)).filter(|n| *n > 0);
    let tokens =
        agent.and_then(|a| a.tokens).or_else(|| usage.map(|u| u.tokens)).filter(|n| *n > 0);
    let duration_ms = agent
        .and_then(|a| a.duration_ms)
        .or_else(|| usage.map(|u| u.duration_ms))
        .filter(|n| *n > 0);
    let agent_type = agent
        .and_then(|a| a.agent_type.clone())
        .or_else(|| task.and_then(|t| t.agent_type.clone()));
    let model = agent_type.as_deref().and_then(|name| {
        state.agents.iter().find(|kind| kind.name == name).and_then(|kind| kind.model.clone())
    });
    let needs_you = child.and_then(|c| c.needs_you.clone());
    let task_live = task.is_some_and(BackgroundTask::is_live);
    let child_running = child.is_some_and(|c| c.running);
    let state_now = if needs_you.is_some() {
        SubagentProgress::NEEDS_YOU
    } else if task.is_some_and(|t| t.state == BackgroundTask::WAITING) {
        SubagentProgress::WAITING
    } else if task_live || child_running || matches!(call.state, ToolState::Running) {
        SubagentProgress::RUNNING
    } else if call.state == ToolState::Failed
        || task.is_some_and(|t| t.state == BackgroundTask::FAILED)
    {
        SubagentProgress::FAILED
    } else if matches!(call.state, ToolState::Cancelled | ToolState::Rejected)
        || task.is_some_and(|t| t.state == BackgroundTask::KILLED)
    {
        SubagentProgress::STOPPED
    } else {
        SubagentProgress::DONE
    };
    let running = matches!(state_now, SubagentProgress::RUNNING | SubagentProgress::NEEDS_YOU);
    let outcome = match state_now {
        // The time it ran is the card's figures' to say, once: the outcome is the word.
        SubagentProgress::DONE => Some("Done".to_owned()),
        SubagentProgress::FAILED => Some(match task.and_then(|t| t.outcome.as_deref()) {
            Some(why) => format!("Failed: {why}"),
            None => "Failed".to_owned(),
        }),
        SubagentProgress::STOPPED => Some("Stopped".to_owned()),
        SubagentProgress::WAITING => Some("Waiting on its own background work".to_owned()),
        _ => None,
    };
    let step = task
        .and_then(|t| t.step.clone())
        .or_else(|| child.and_then(|c| c.step.clone()))
        .filter(|_| running);
    let started_at = task
        .map(|t| at(t.started_ms))
        .or_else(|| child.and_then(|c| c.started_ms).map(at))
        .filter(|ms| *ms > 0);
    let row = task.map(|t| task_row(&t.id)).or_else(|| child.map(|c| chat_row(&c.chat)));
    SubagentProgress {
        agent_type,
        model,
        running,
        state: state_now.to_owned(),
        step,
        tool_uses,
        tokens,
        duration_ms,
        started_at,
        figures: subagent_figures(tool_uses, tokens),
        outcome,
        needs_you,
        chat_id: child.map(|c| c.chat.clone()),
        row,
    }
}

/// A subagent card's figures, as the TUI's agent row says them: "12 tools · 41k tokens".
fn subagent_figures(tool_uses: Option<u64>, tokens: Option<u64>) -> String {
    let mut out = Vec::new();
    if let Some(n) = tool_uses {
        out.push(count(n, "tool", "tools"));
    }
    if let Some(n) = tokens {
        out.push(format!("{} tokens", tokens_short(n)));
    }
    out.join(" · ")
}

/// A background task's notice as its row; `chat_of` names a thread by its chat's id.
#[must_use]
pub fn task_part(
    note: &TaskNote,
    state: &ThreadState,
    words: &dyn Fn(&Clipped) -> String,
    chat_of: &dyn Fn(ThreadId) -> String,
) -> TaskRow {
    let usage = note.usage;
    let duration = usage.map(|u| u.duration_ms).filter(|ms| *ms > 0);
    // slopty: a teammate that ended reads as the TUI's row names it.
    let summary = match (note.kind.as_str(), note.status.as_str()) {
        (BackgroundTask::TEAMMATE, TaskNote::COMPLETED) if !note.title.trim().is_empty() => {
            format!("Teammate @{} shut down gracefully", note.title.trim().trim_start_matches('@'))
        }
        _ => note.summary.clone(),
    };
    let text = match duration {
        Some(ms) if !summary.contains(" · ") => format!("{summary} · {}", duration_words(ms)),
        _ => summary,
    };
    let tone = match note.status.as_str() {
        TaskNote::FAILED => NoteLine::ERROR,
        TaskNote::KILLED | TaskNote::EXPIRED => NoteLine::WARNING,
        _ if note.exit_code.is_some_and(|code| code != 0) => NoteLine::ERROR,
        _ => NoteLine::INFO,
    };
    let task = state.tasks.iter().find(|task| task.id == note.task);
    TaskRow {
        task: note.task.clone(),
        row: task_row(&note.task),
        kind: kind_of(&note.kind).0.to_owned(),
        status: note.status.clone(),
        text,
        title: note.title.clone(),
        tone: tone.to_owned(),
        body: note.body.as_ref().map(words).filter(|body| !body.trim().is_empty()),
        call_part_id: note.call.as_ref().map(|item| item.0.clone()),
        figures: figures(usage.map(|u| u.tool_uses), usage.map(|u| u.tokens)),
        tokens: usage.map(|u| u.tokens).filter(|n| *n > 0),
        tool_uses: usage.map(|u| u.tool_uses).filter(|n| *n > 0),
        duration_ms: duration,
        exit_code: note.exit_code,
        chat_id: task.and_then(|task| task.child).map(chat_of),
        links: note
            .links
            .iter()
            .map(|link| LinkRow {
                uri: link.uri.clone(),
                label: link.title.clone().unwrap_or_else(|| link.name.clone()),
                mime_type: link.mime_type.clone(),
            })
            .collect(),
    }
}

/// Whether the notice `item` of turn `turn` was followed by more of its turn than requests tried
/// again: the request went through.
fn went_on(state: &ThreadState, item: &str, turn: TurnId) -> bool {
    let after = state.items.iter().skip_while(|i| i.id.0 != item).skip(1);
    let more = after
        .filter(|i| i.turn == turn)
        .any(|i| !matches!(&i.body, ItemBody::Notice(n) if n.kind == Notice::API_ERROR));
    more || state.turns.iter().any(|t| t.id == turn && t.state == TurnState::Complete)
}

/// Whether the turn `turn` failed.
fn failed(state: &ThreadState, turn: TurnId) -> bool {
    state.turns.iter().any(|t| t.id == turn && matches!(t.state, TurnState::Failed { .. }))
}

/// `note` with its first line the row and the rest opening under it.
fn split(mut note: NoteRow) -> NoteRow {
    if let Some((line, more)) = note.text.split_once('\n') {
        let more = more.trim();
        if !more.is_empty() {
            note.detail = Some(more.to_owned());
        }
        note.text = line.trim().to_owned();
    }
    note
}

/// A notice that is no failure as a quiet row, with `text` its words; `None` for one that is a
/// failure, which reads in the one error grammar.
#[must_use]
pub fn note_row(
    notice: &Notice,
    text: String,
    item: &str,
    turn: TurnId,
    at_ms: i64,
    state: &ThreadState,
) -> Option<NoteRow> {
    let row = |tone: &str, text: String| NoteRow {
        kind: notice.kind.clone(),
        text,
        tone: tone.to_owned(),
        detail: None,
        retry: None,
        recovered: false,
    };
    Some(match notice.kind.as_str() {
        Notice::API_ERROR => {
            let retry: RetryLine = retry_line(notice, at_ms)?;
            let mut note = row(NoteLine::WARNING, String::new());
            "retry".clone_into(&mut note.kind);
            note.detail = Some(text).filter(|t| !t.trim().is_empty());
            if went_on(state, item, turn) {
                note.text = format!("{} · recovered on attempt {}", retry.what, retry.attempt);
                NoteRow::SUCCESS.clone_into(&mut note.tone);
                note.recovered = true;
            } else if failed(state, turn) {
                note.text = format!(
                    "{} · gave up after {}",
                    retry.what,
                    count(u64::from(retry.attempt), "attempt", "attempts")
                );
                NoteLine::ERROR.clone_into(&mut note.tone);
            } else {
                note.text = retry.words(None);
                note.retry = Some(retry);
            }
            note
        }
        // Where the person stopped the turn; the composer says what to do next.
        Notice::INTERRUPTED => row(NoteLine::INFO, "Interrupted".to_owned()),
        Notice::ALLOWED | Notice::MEMORY | Notice::MODE | Notice::STOPPED | Notice::TURN => {
            row(NoteLine::INFO, text)
        }
        Notice::FALLBACK => row(NoteLine::WARNING, text),
        // slopty: a hook's word, as the TUI keeps it in the turn: what it printed reads quiet,
        // one that failed, blocked a call or stopped the turn warns. Its first line is the row;
        // more of it (a stop hook's feedback) opens under it.
        Notice::HOOK => {
            let lower = text.to_lowercase();
            let wrong = ["failed", "error", "stopped the turn", "timed out", "deferred"]
                .iter()
                .any(|word| lower.contains(word));
            split(row(if wrong { NoteLine::WARNING } else { NoteLine::INFO }, text))
        }
        // slopty: a prompt's origin, an MCP server's update, a message from another session.
        Notice::NOTE => split(row(NoteLine::INFO, text)),
        // slopty: what a command printed as its errors, in red as the TUI's.
        Notice::COMMAND_ERROR => split(row(NoteLine::ERROR, text)),
        // slopty: the goal's check, its first line the row and the rest ("Goal: …", "Reason:
        // …") opening under it, as the TUI's verbose view adds them.
        Notice::GOAL => {
            let (line, more) = text.split_once('\n').unwrap_or((text.as_str(), ""));
            let lower = line.to_lowercase();
            let tone = if lower.contains("could not")
                || lower.contains("couldn't")
                || lower.contains("failed")
            {
                NoteLine::ERROR
            } else if lower.contains("not yet") || lower.contains("paused") {
                NoteLine::INFO
            } else if lower.contains("achieved") {
                NoteRow::SUCCESS
            } else {
                NoteLine::INFO
            };
            let mut note = row(tone, line.trim().to_owned());
            note.detail = Some(more.trim().to_owned()).filter(|more| !more.is_empty());
            note
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
