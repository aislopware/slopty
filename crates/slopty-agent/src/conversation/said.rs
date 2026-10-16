//! What Claude Code's TUI says of a call beyond what its kind's chip says: the header it words
//! from the call's input where that says more ("Read report.pdf · pages 1-3", "Create notes.md"),
//! and the line under the call (its `⎿` line) from the call's structured result ("Read PDF
//! (1.2MB)", "Did 3 searches in 4s", "Scheduled 7f2a (every 5 minutes, next run in 4m)").
//!
//! A tool the TUI draws as a list (`CronList`, `ReadNotifications`, a code review's findings, a
//! question's answers) gives its list as the call's body, in place of the text the model read.
//! Each template is the TUI's own (2.1.301, `.research/tui-strings-2026-10-16.md` §1A–1B); the
//! terminal's key hints are left out, since the GUI has its own affordances.

use std::fmt::Write as _;

use serde_json::Value;

use super::ResultStatus;

/// What the TUI says of a call's result: its line, and a body that stands for the model's text.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Said {
    /// The line under the call.
    pub line: Option<String>,
    /// What the call's body shows in place of the text the model read.
    pub body: Option<String>,
}

impl Said {
    fn line(line: impl Into<String>) -> Self {
        Self { line: Some(line.into()), body: None }
    }

    fn with_body(line: Option<String>, body: String) -> Self {
        Self { line, body: Some(body).filter(|b| !b.trim().is_empty()) }
    }
}

/// A string field of `value`, trimmed, when it says something.
fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str().map(str::trim).filter(|t| !t.is_empty())
}

/// A number field of `value`.
fn number(value: &Value, key: &str) -> Option<u64> {
    let field = value.get(key)?;
    field.as_u64().or_else(|| field.as_f64().filter(|n| *n >= 0.0).map(round))
}

/// A float's nearest whole number, at least 0.
fn round(n: f64) -> u64 {
    let rounded = n.round().max(0.0);
    // A float's whole part, read back through its text: no lossy cast.
    format!("{rounded:.0}").parse().unwrap_or(u64::MAX)
}

/// The array field `key` of `value`, empty when it is none.
fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value.get(key).and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// `n` `one`, or `n` of the plural.
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// A path's last part, as a chip names a file.
fn base(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path)
}

/// `text` on one line, cut to `max` characters with an ellipsis.
fn short(text: &str, max: usize) -> String {
    let line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= max {
        return line;
    }
    let mut cut: String = line.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// A size in bytes as the TUI says it: `512 bytes`, `12.3KB`, `1.4MB` (1024 base, a trailing `.0`
/// dropped).
pub(super) fn size(bytes: u64) -> String {
    let unit = |shift: u32, name: &str| {
        // Rounded to the nearest tenth, as the TUI's `toFixed(1)`.
        let half = 1_u64.checked_shl(shift).unwrap_or(0) / 2;
        let tenths = bytes.saturating_mul(10).saturating_add(half).checked_shr(shift).unwrap_or(0);
        let (whole, tenth) = (tenths / 10, tenths % 10);
        if tenth == 0 { format!("{whole}{name}") } else { format!("{whole}.{tenth}{name}") }
    };
    if bytes >= 1 << 30 {
        unit(30, "GB")
    } else if bytes >= 1 << 20 {
        unit(20, "MB")
    } else if bytes >= 1 << 10 {
        unit(10, "KB")
    } else {
        format!("{bytes} bytes")
    }
}

/// How long, as the TUI says it: `450ms`, `12s`, `5m 3s`, `1h 2m`.
fn span(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0 => format!("{ms}ms"),
        1..60 => format!("{secs}s"),
        60..3600 => match secs % 60 {
            0 => format!("{}m", secs / 60),
            s => format!("{}m {s}s", secs / 60),
        },
        _ => match (secs / 60) % 60 {
            0 => format!("{}h", secs / 3600),
            m => format!("{}h {m}m", secs / 3600),
        },
    }
}

/// The header the TUI words for a call of `name` from its `input`, where it says more than the
/// kind's chip: a read's page or line range, an edit that creates its file, a notebook's cell.
pub(super) fn header(name: &str, input: &Value) -> Option<String> {
    let path = text(input, "file_path").or_else(|| text(input, "notebook_path"));
    match name {
        "Read" => {
            let file = base(path?);
            if let Some(pages) = text(input, "pages") {
                return Some(format!("Read {file} · pages {pages}"));
            }
            match (number(input, "offset"), number(input, "limit")) {
                (Some(from), Some(limit)) if limit > 0 => Some(format!(
                    "Read {file} · lines {from}-{}",
                    from.saturating_add(limit).saturating_sub(1)
                )),
                (Some(from), _) if from > 1 => Some(format!("Read {file} · from line {from}")),
                (None, Some(limit)) if limit > 0 => Some(format!("Read {file} · lines 1-{limit}")),
                _ => None,
            }
        }
        // An edit whose old text is empty makes the file: the TUI's `Create(path)`.
        "Edit" if input.get("old_string").and_then(Value::as_str) == Some("") => {
            Some(format!("Create {}", base(path?)))
        }
        "NotebookEdit" => {
            let file = base(path?);
            let mode = match text(input, "edit_mode") {
                Some("insert") => " · insert",
                Some("delete") => " · delete",
                _ => "",
            };
            Some(match text(input, "cell_id") {
                Some(cell) => format!("Edit Notebook {file}@{cell}{mode}"),
                None => format!("Edit Notebook {file}{mode}"),
            })
        }
        _ => None,
    }
}

/// What a call's result is, as [`said`] reads it.
#[derive(Clone, Copy, Debug)]
pub(super) struct Outcome<'a> {
    /// Its input, where the call keeps it.
    pub input: Option<&'a Value>,
    /// Its structured result (`tool_use_result`).
    pub result: Option<&'a Value>,
    /// What the model read.
    pub text: &'a str,
    /// How it went.
    pub status: ResultStatus,
    /// When it came, in ms since the epoch: what a "next run in" counts from.
    pub at_ms: u64,
}

/// What the TUI says of the result of a call of `name`.
pub(super) fn said(name: &str, outcome: Outcome<'_>) -> Said {
    let Outcome { input, result, text: text_seen, status, at_ms } = outcome;
    if status == ResultStatus::Rejected {
        return Said::default();
    }
    let none = Value::Null;
    let r = result.unwrap_or(&none);
    match name {
        "advisor" => advisor(r),
        "Bash" | "PowerShell" => shell(r, status),
        "Read" => read(r).map(Said::line).unwrap_or_default(),
        "Edit" | "MultiEdit" | "Write" if r.get("staged").is_some_and(truthy) => Said::line(
            "Staged for your review · not applied until you accept it in Claude Code's \
             /settings-review",
        ),
        "NotebookEdit" if status == ResultStatus::Ok => notebook(r),
        "WebSearch" if status == ResultStatus::Ok && result.is_some() => web_search(r),
        "Agent" | "Task" => agent(r),
        "Skill" if status == ResultStatus::Ok && result.is_some() => skill(r),
        "TaskStop" | "KillShell" | "KillBash" if status == ResultStatus::Ok => text(r, "command")
            .map(|c| Said::line(format!("{} · stopped", short(c, 60))))
            .unwrap_or_default(),
        "GetTask" if status == ResultStatus::Ok => get_task(r),
        "SendMessage" => send_message(r),
        "SendFile" => send_file(r),
        "AskUserQuestion" if status == ResultStatus::Ok => answered(input, r),
        "ExitPlanMode" if status == ResultStatus::Ok => plan(r, input),
        "CronCreate" if status == ResultStatus::Ok => scheduled(r, at_ms),
        "CronDelete" if status == ResultStatus::Ok => {
            text(r, "id").map(|id| Said::line(format!("Cancelled {id}"))).unwrap_or_default()
        }
        "CronList" if status == ResultStatus::Ok => jobs(r, at_ms),
        "EnterWorktree" if status == ResultStatus::Ok => worktree_in(r),
        "ExitWorktree" if status == ResultStatus::Ok => worktree_out(r),
        "ListMcpResourcesTool" | "ReadMcpResourceTool" | "ReadMcpResourceDirTool" => {
            resources(name, result)
        }
        "Workflow" => workflow(r),
        "ReportFindings" => findings(r, input),
        "RemoteTrigger" => trigger(r),
        "ReadNotifications" => notifications(r),
        "FetchInboxMessage" => inbox(r),
        "EndConversation" => text(r, "message").map(Said::line).unwrap_or_default(),
        "propose_skills" => number(r, "proposalCount")
            .map(|n| {
                let n = usize::try_from(n).unwrap_or(usize::MAX);
                Said::line(format!(
                    "Shown {} to the person for review",
                    count(n, "skill proposal", "skill proposals")
                ))
            })
            .unwrap_or_default(),
        "ListPlugins" | "SearchPlugins" | "SuggestPluginInstall" => listed(r, "plugin", "plugins"),
        "ListSkills" | "SearchSkills" | "SuggestSkills" => listed(r, "skill", "skills"),
        mcp if mcp.starts_with("mcp__") => large(text_seen).map(Said::line).unwrap_or_default(),
        _ => Said::default(),
    }
}

/// The advisor's word on the conversation (its server tool's result), as the TUI's row says
/// it: reviewed and applied, with the advice under it; declined; or unavailable and why.
fn advisor(r: &Value) -> Said {
    let advice = text(r, "text").map(str::to_owned).unwrap_or_default();
    match text(r, "type") {
        Some("advisor_tool_result_error") => Said::line(format!(
            "Advisor unavailable ({})",
            text(r, "error_code").unwrap_or("error")
        )),
        Some("advisor_result" | "advisor_redacted_result")
            if text(r, "stop_reason") == Some("refusal") =>
        {
            Said::with_body(Some("Advisor declined to advise on this request".to_owned()), advice)
        }
        Some("advisor_result" | "advisor_redacted_result") => Said::with_body(
            Some("Advisor has reviewed the conversation and will apply the feedback".to_owned()),
            advice,
        ),
        _ => Said::default(),
    }
}

/// Whether a thinking block's `signature` marks it as narration: the model's words to the
/// person, which the TUI draws as the answer's text. The signature is base64 of a protobuf
/// message whose field 2, then field 1, then field 8 holds the class, as Claude Code reads it.
pub(super) fn narration(signature: Option<&str>) -> bool {
    let Some(signature) = signature.filter(|s| !s.is_empty()) else { return false };
    let Ok(bytes) = data_encoding::BASE64.decode(signature.trim().as_bytes()) else {
        return false;
    };
    let class = field(&bytes, 2).and_then(|m| field(m, 1)).and_then(|m| field(m, 8));
    class == Some(b"narration".as_slice())
}

/// The bytes of length-delimited field `number` of protobuf message `message`, the last one
/// when it repeats; `None` when it is absent or the message does not read.
fn field(message: &[u8], number: u64) -> Option<&[u8]> {
    let mut at = 0_usize;
    let mut found = None;
    while at < message.len() {
        let (key, next) = varint(message, at)?;
        at = next;
        match key & 7 {
            0 => at = varint(message, at)?.1,
            1 => at = at.checked_add(8).filter(|end| *end <= message.len())?,
            2 => {
                let (len, next) = varint(message, at)?;
                let end = next.checked_add(usize::try_from(len).ok()?)?;
                let bytes = message.get(next..end)?;
                if key >> 3 == number {
                    found = Some(bytes);
                }
                at = end;
            }
            5 => at = at.checked_add(4).filter(|end| *end <= message.len())?,
            _ => return None,
        }
    }
    found
}

/// A protobuf varint at `at` of `bytes`, and where the next value starts.
fn varint(bytes: &[u8], mut at: usize) -> Option<(u64, usize)> {
    let mut value = 0_u64;
    for shift in (0..64).step_by(7) {
        let byte = *bytes.get(at)?;
        at = at.checked_add(1)?;
        value |= u64::from(byte & 0x7f).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return Some((value, at));
        }
    }
    None
}

/// Whether a JSON value reads as yes: `true`, or any object or non-empty text.
const fn truthy(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// A command's outcome the TUI words from its structured result: an image it printed, and the
/// quiet "Done" of a command that prints nothing by design.
fn shell(r: &Value, status: ResultStatus) -> Said {
    if r.get("isImage").and_then(Value::as_bool) == Some(true) {
        return Said::line("Image data detected and sent to Claude");
    }
    let quiet = |key: &str| text(r, key).is_none();
    if status == ResultStatus::Ok
        && r.get("noOutputExpected").and_then(Value::as_bool) == Some(true)
        && quiet("stdout")
        && quiet("stderr")
        && text(r, "backgroundTaskId").is_none()
    {
        return Said::line("Done");
    }
    Said::default()
}

/// A read of something other than text, as the TUI's line says it.
fn read(r: &Value) -> Option<String> {
    let file = r.get("file")?;
    let bytes = || number(file, "originalSize").map(size);
    Some(match text(r, "type")? {
        "image" => format!("Read image ({})", bytes()?),
        "pdf" => format!("Read PDF ({})", bytes()?),
        "parts" => {
            let pages = usize::try_from(number(file, "count")?).unwrap_or(usize::MAX);
            let pages = count(pages, "page", "pages");
            match bytes() {
                Some(size) => format!("Read {pages} ({size})"),
                None => format!("Read {pages}"),
            }
        }
        "notebook" => match list(file, "cells").len() {
            0 => "No cells found in notebook".to_owned(),
            n => format!("Read {}", count(n, "cell", "cells")),
        },
        _ => return None,
    })
}

/// A notebook's cell edited, inserted or deleted.
fn notebook(r: &Value) -> Said {
    let Some(cell) = text(r, "cell_id") else { return Said::default() };
    let verb = match text(r, "edit_mode") {
        Some("insert") => "Inserted",
        Some("delete") => "Deleted",
        _ => "Updated",
    };
    Said::line(format!("{verb} cell {cell}"))
}

/// "Did 3 searches in 4s".
fn web_search(r: &Value) -> Said {
    let searches = number(r, "searchCount").map_or_else(
        || list(r, "results").iter().filter(|r| !r.is_string()).count(),
        |n| usize::try_from(n).unwrap_or(usize::MAX),
    );
    let took = r.get("durationSeconds").and_then(Value::as_f64).map(|secs| {
        if secs >= 1.0 {
            format!("{}s", round(secs))
        } else {
            format!("{}ms", round(secs * 1000.0))
        }
    });
    let did = format!("Did {}", count(searches.max(1), "search", "searches"));
    Said::line(match took {
        Some(took) => format!("{did} in {took}"),
        None => did,
    })
}

/// A subagent run in Claude Code's cloud: where it runs.
fn agent(r: &Value) -> Said {
    match text(r, "status") {
        Some("remote_launched") => {
            let parts: Vec<&str> =
                [text(r, "taskId"), text(r, "sessionUrl")].into_iter().flatten().collect();
            let mut line = "Cloud agent launched".to_owned();
            for part in parts {
                let _written = write!(line, " · {part}");
            }
            Said::line(line)
        }
        _ => Said::default(),
    }
}

/// A skill loaded inline, or run forked.
fn skill(r: &Value) -> Said {
    if text(r, "status") == Some("forked") {
        let background = r.get("background").and_then(Value::as_bool) == Some(true);
        return Said::line(if background { "Running in the background" } else { "Done" });
    }
    let mut parts = vec!["Successfully loaded skill".to_owned()];
    let allowed = list(r, "allowedTools").len();
    if allowed > 0 {
        parts.push(format!("{} allowed", count(allowed, "tool", "tools")));
    }
    if let Some(model) = text(r, "model") {
        parts.push(model.to_owned());
    }
    Said::line(parts.join(" · "))
}

/// A task's status, as `GetTask` read it.
fn get_task(r: &Value) -> Said {
    let Some(status) = text(r, "status") else { return Said::default() };
    let finished = r.get("result").filter(|_| status == "completed");
    let chars: usize = finished.map_or(0, |f| {
        list(f, "content").iter().filter_map(|c| text(c, "text")).map(str::len).sum()
    });
    let errors = finished.and_then(|f| f.get("isError")).and_then(Value::as_bool) == Some(true);
    let mut line = if errors { "completed with errors".to_owned() } else { status.to_owned() };
    if chars > 0 {
        let _written = write!(line, " · {chars} chars of output");
    }
    Said::line(line)
}

/// A message to an agent that resumed it and handed its result back inline.
fn send_message(r: &Value) -> Said {
    let Some(handback) = r.get("inlineHandback") else { return Said::default() };
    let name = text(handback, "displayName").unwrap_or("the agent");
    let words: Vec<&str> =
        list(handback, "content").iter().filter_map(|c| text(c, "text")).collect();
    let body = if words.is_empty() { "(no text output)".to_owned() } else { words.join("\n") };
    Said::with_body(Some(format!("Resumed agent {name}")), format!("Result:\n{body}"))
}

/// What `SendFile` said, and each file that did not go.
fn send_file(r: &Value) -> Said {
    let failed: Vec<String> = list(r, "files")
        .iter()
        .filter_map(|f| {
            let error = text(f, "error")?;
            Some(format!("{}: {error}", text(f, "path").or_else(|| text(f, "file")).unwrap_or("?")))
        })
        .collect();
    let line = text(r, "message").map(str::to_owned);
    if failed.is_empty() {
        return Said { line, body: None };
    }
    Said::with_body(line, failed.join("\n"))
}

/// A question's answers, as the TUI lists them under it: answered, more questions asked for, or
/// left unanswered past the time Claude Code waits.
fn answered(input: Option<&Value>, r: &Value) -> Said {
    let answers: Vec<(String, String)> = r
        .get("answers")
        .and_then(Value::as_object)
        .map(|answers| {
            answers
                .iter()
                .map(|(q, a)| (q.clone(), a.as_str().map_or_else(|| a.to_string(), str::to_owned)))
                .collect()
        })
        .unwrap_or_default();
    let response = text(r, "response");
    let mut body: Vec<String> = answers.iter().map(|(q, a)| format!("· {q} → {a}")).collect();
    if let Some(response) = response {
        body.push(format!("· {response}"));
    }
    if let Some(waited) = number(r, "afkTimeoutMs") {
        let asked = if answers.is_empty() {
            input.map_or(&[][..], |i| list(i, "questions")).iter().filter_map(asked).collect()
        } else {
            body.clone()
        };
        let went_on = if answers.is_empty() {
            "continued without an answer"
        } else {
            "continued with the answers selected so far"
        };
        let line = format!("No response after {}s — {went_on}", waited / 1000);
        return Said::with_body(Some(line), asked.join("\n"));
    }
    let answered = !answers.is_empty() || response.is_some();
    let line = if r.get("followUp").is_some_and(truthy) {
        if answered {
            "User asked Claude for more questions (so far):"
        } else {
            "User asked Claude for more questions"
        }
    } else if answered {
        "User answered Claude's questions:"
    } else {
        return Said::default();
    };
    Said::with_body(Some(line.trim_end_matches(':').to_owned()), body.join("\n"))
}

/// A question as asked, with its options: `· {question} ({a} / {b})`.
fn asked(question: &Value) -> Option<String> {
    let text_of = text(question, "question")?;
    let options: Vec<&str> = list(question, "options")
        .iter()
        .filter_map(|o| text(o, "label").or_else(|| o.as_str()))
        .collect();
    Some(if options.is_empty() {
        format!("· {text_of}")
    } else {
        format!("· {text_of} ({})", options.join(" / "))
    })
}

/// A plan approved, and where Claude Code saved it.
fn plan(r: &Value, input: Option<&Value>) -> Said {
    let plan = text(r, "plan").or_else(|| input.and_then(|i| text(i, "plan")));
    if plan.is_none() {
        return Said::line("Exited plan mode");
    }
    Said::line(match text(r, "filePath") {
        Some(path) => format!("User approved Claude's plan · saved to {path}"),
        None => "User approved Claude's plan".to_owned(),
    })
}

/// When a scheduled job next runs, from `now` (ms since the epoch): `next run in 4m`.
fn next_run(job: &Value, now: u64) -> Option<String> {
    let at = number(job, "nextFireAt").filter(|_| now > 0)?;
    Some(if at > now {
        format!("next run in {}", span(at.saturating_sub(now)))
    } else {
        "next run now".to_owned()
    })
}

/// "Scheduled 7f2a (every 5 minutes, next run in 4m)".
fn scheduled(r: &Value, now: u64) -> Said {
    let Some(id) = text(r, "id") else { return Said::default() };
    let when: Vec<String> = [text(r, "humanSchedule").map(str::to_owned), next_run(r, now)]
        .into_iter()
        .flatten()
        .collect();
    Said::line(if when.is_empty() {
        format!("Scheduled {id}")
    } else {
        format!("Scheduled {id} ({})", when.join(", "))
    })
}

/// The scheduled jobs, one a line.
fn jobs(r: &Value, now: u64) -> Said {
    let jobs = list(r, "jobs");
    if jobs.is_empty() {
        return Said::line("No scheduled jobs");
    }
    let lines: Vec<String> = jobs
        .iter()
        .map(|job| {
            let mut line = text(job, "id").unwrap_or("?").to_owned();
            if let Some(when) = text(job, "humanSchedule") {
                let _written = write!(line, " {when}");
            }
            let recurring = job.get("recurring").and_then(Value::as_bool) == Some(true);
            line.push_str(if recurring { " (recurring)" } else { " (one-shot)" });
            if job.get("durable").and_then(Value::as_bool) == Some(false) {
                line.push_str(" [session-only]");
            }
            if let Some(next) = next_run(job, now) {
                let _written = write!(line, ", {next}");
            }
            if let Some(prompt) = text(job, "prompt") {
                let _written = write!(line, ": {}", short(prompt, 60));
            }
            line
        })
        .collect();
    Said::with_body(Some(count(jobs.len(), "scheduled job", "scheduled jobs")), lines.join("\n"))
}

/// "Switched to worktree on branch fix · /w/.claude/worktrees/fix".
fn worktree_in(r: &Value) -> Said {
    let mut line = "Switched to worktree".to_owned();
    if let Some(branch) = text(r, "worktreeBranch") {
        let _written = write!(line, " on branch {branch}");
    }
    if let Some(path) = text(r, "worktreePath") {
        let _written = write!(line, " · {path}");
    }
    Said::line(line)
}

/// "Removed worktree (branch fix) · returned to /w".
fn worktree_out(r: &Value) -> Said {
    let mut line =
        if text(r, "action") == Some("keep") { "Kept worktree" } else { "Removed worktree" }
            .to_owned();
    if let Some(branch) = text(r, "worktreeBranch") {
        let _written = write!(line, " (branch {branch})");
    }
    if let Some(cwd) = text(r, "originalCwd") {
        let _written = write!(line, " · returned to {cwd}");
    }
    Said::line(line)
}

/// An MCP server's resources listed or read: what was found, as the TUI words an empty answer,
/// and the rest as JSON.
fn resources(name: &str, result: Option<&Value>) -> Said {
    let Some(r) = result else { return Said::default() };
    if let Some(error) = text(r, "error") {
        return Said::line(error);
    }
    let (found, empty) = match name {
        "ListMcpResourcesTool" => (r.as_array().map_or(0, Vec::len), "(No resources found)"),
        "ReadMcpResourceTool" => (list(r, "contents").len(), "(No content)"),
        _ => (list(r, "resources").len(), "(Empty directory)"),
    };
    if found == 0 {
        return Said::line(empty);
    }
    let line = match name {
        "ReadMcpResourceTool" => count(found, "content", "contents"),
        _ => count(found, "resource", "resources"),
    };
    Said::with_body(Some(line), serde_json::to_string_pretty(r).unwrap_or_default())
}

/// A workflow launched: in the background, or in a cloud session.
fn workflow(r: &Value) -> Said {
    if let Some(error) = text(r, "error") {
        return Said::line(error);
    }
    if text(r, "status") == Some("remote_launched") {
        let mut line = "Running in cloud session".to_owned();
        if let Some(url) = text(r, "sessionUrl") {
            let _written = write!(line, " · {url}");
        }
        if let Some(warning) = text(r, "warning") {
            let _written = write!(line, " · {warning}");
        }
        return Said::line(line);
    }
    match text(r, "taskId") {
        Some(task) => Said::line(format!("Running in the background · task {task}")),
        None => Said::default(),
    }
}

/// A code review's findings, by file: `● 12 [bug] summary`, `✓` for one settled.
fn findings(r: &Value, input: Option<&Value>) -> Said {
    let mut found = list(r, "findings");
    if found.is_empty() {
        found = input.map_or(&[][..], |i| list(i, "findings"));
    }
    if found.is_empty() {
        return Said::default();
    }
    let mut body = String::new();
    let mut file_now: Option<&str> = None;
    for finding in found {
        let file = text(finding, "file").unwrap_or("?");
        if file_now != Some(file) {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(file);
            file_now = Some(file);
        }
        let settled = text(finding, "outcome").is_some() || text(finding, "verdict").is_some();
        let _written = write!(body, "\n  {} ", if settled { "✓" } else { "●" });
        if let Some(line) = number(finding, "line") {
            let _written = write!(body, "{line} ");
        }
        if let Some(category) = text(finding, "category") {
            let _written = write!(body, "[{category}] ");
        }
        body.push_str(text(finding, "summary").unwrap_or_default());
        match text(finding, "outcome").or_else(|| text(finding, "verdict")) {
            Some("skipped") => body.push_str(" [skipped]"),
            Some("no_change_needed" | "no change needed") => body.push_str(" [no change needed]"),
            _ => {}
        }
    }
    Said::with_body(Some(count(found.len(), "finding", "findings")), body)
}

/// A remote trigger's answer: "HTTP 200 (12 lines)".
fn trigger(r: &Value) -> Said {
    let Some(status) = number(r, "status") else { return Said::default() };
    let lines = r.get("json").map_or(0, |json| match json {
        Value::String(s) => s.lines().count(),
        other => serde_json::to_string_pretty(other).map_or(0, |s| s.lines().count()),
    });
    Said::line(format!("HTTP {status} ({})", count(lines, "line", "lines")))
}

/// The notifications read, one a line, and how many still wait.
fn notifications(r: &Value) -> Said {
    let remaining = number(r, "remaining").unwrap_or(0);
    let more = (remaining > 0).then(|| {
        let n = usize::try_from(remaining).unwrap_or(usize::MAX);
        format!("{} still queued", count(n, "more notification", "more notifications"))
    });
    let read = list(r, "notifications");
    if read.is_empty() {
        return Said::line(more.unwrap_or_else(|| "No queued notifications".to_owned()));
    }
    let lines: Vec<String> = read
        .iter()
        .map(|n| {
            let origin = text(n, "origin").unwrap_or("notification");
            let summary = if origin == "mcp_send_message" {
                "message from another session".to_owned()
            } else {
                short(text(n, "content").unwrap_or_default(), 80)
            };
            format!("⟵ {} · {summary}", origin.replace('_', " "))
        })
        .collect();
    let line = match more {
        Some(more) => format!("{} · {more}", count(read.len(), "notification", "notifications")),
        None => count(read.len(), "notification", "notifications"),
    };
    Said::with_body(Some(line), lines.join("\n"))
}

/// A message read from the inbox, or why it could not be.
fn inbox(r: &Value) -> Said {
    if r.get("ok").and_then(Value::as_bool) == Some(false) {
        let why = text(r, "reason").or_else(|| text(r, "error")).unwrap_or("no reason given");
        return Said::line(format!("Inbox read failed · {why}"));
    }
    let sender = text(r, "sender_display").unwrap_or("unknown sender");
    let via = if text(r, "source") == Some("slack") { " via Slack" } else { "" };
    let mut line = format!("Inbox message from {sender}{via}");
    if let Some(kind) = text(r, "sender_kind") {
        line = format!("[{kind}] {line}");
    }
    let mut body = text(r, "body").unwrap_or_default().to_owned();
    let received: Vec<&str> =
        [text(r, "received_at"), text(r, "permalink")].into_iter().flatten().collect();
    if !received.is_empty() {
        let _written = write!(body, "\n\nreceived {}", received.join(" · "));
    }
    Said::with_body(Some(line), body)
}

/// "12 plugins", "3 skills".
fn listed(r: &Value, one: &str, many: &str) -> Said {
    let found = r
        .get("results")
        .or_else(|| r.get("plugins"))
        .or_else(|| r.get("skills"))
        .and_then(Value::as_array)
        .map(Vec::len);
    found.map(|n| Said::line(count(n, one, many))).unwrap_or_default()
}

/// Tokens past which the TUI warns an MCP answer is large.
const LARGE_MCP_TOKENS: usize = 10_000;

/// The TUI's warning for an MCP answer that fills the context: about four characters a token.
fn large(text_seen: &str) -> Option<String> {
    let tokens = text_seen.len() / 4;
    (tokens > LARGE_MCP_TOKENS).then(|| {
        format!(
            "Large MCP response (~{}k tokens), this can fill up context quickly",
            tokens.div_ceil(1000)
        )
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn outcome<'a>(input: Option<&'a Value>, result: Option<&'a Value>) -> Outcome<'a> {
        Outcome { input, result, text: "", status: ResultStatus::Ok, at_ms: 1_060_000 }
    }

    fn line(name: &str, input: &Value, result: &Value) -> Option<String> {
        said(name, outcome(Some(input), Some(result))).line
    }

    /// A signature whose class field says "narration" marks narration; any other, or one that
    /// does not read, does not.
    #[test]
    fn a_signature_marks_narration() {
        // field 2 { field 1 { field 8: class } }, as length-delimited fields.
        let signed = |class: &[u8]| {
            let mut inner = vec![0x42, u8::try_from(class.len()).expect("short")];
            inner.extend_from_slice(class);
            let mut middle = vec![0x0a, u8::try_from(inner.len()).expect("short")];
            middle.extend(inner);
            let mut outer = vec![0x08, 0x01, 0x12, u8::try_from(middle.len()).expect("short")];
            outer.extend(middle);
            data_encoding::BASE64.encode(&outer)
        };
        assert!(narration(Some(&signed(b"narration"))));
        assert!(!narration(Some(&signed(b"summary"))));
        assert!(!narration(Some("not base64 at all")));
        assert!(!narration(None));
    }

    /// Each header and line reads as the TUI's, and a call that says nothing more gets none.
    #[test]
    fn calls_say_what_the_tuis_rows_say() {
        let read = |input: Value| header("Read", &input);
        assert_eq!(
            read(json!({"file_path": "/w/doc/report.pdf", "pages": "1-3"})).as_deref(),
            Some("Read report.pdf · pages 1-3")
        );
        assert_eq!(
            read(json!({"file_path": "/w/a.rs", "offset": 10, "limit": 50})).as_deref(),
            Some("Read a.rs · lines 10-59")
        );
        assert_eq!(read(json!({"file_path": "/w/a.rs"})), None, "a whole read says no more");
        assert_eq!(
            header("Edit", &json!({"file_path": "/w/n.md", "old_string": "", "new_string": "x"}))
                .as_deref(),
            Some("Create n.md")
        );
        assert_eq!(
            header("NotebookEdit", &json!({"notebook_path": "/w/a.ipynb", "cell_id": "c3"}))
                .as_deref(),
            Some("Edit Notebook a.ipynb@c3")
        );
        let none = json!({});
        let pdf = json!({"type": "pdf", "file": {"originalSize": 1_258_291}});
        assert_eq!(line("Read", &none, &pdf).as_deref(), Some("Read PDF (1.2MB)"));
        let image = json!({"type": "image", "file": {"originalSize": 2048}});
        assert_eq!(line("Read", &none, &image).as_deref(), Some("Read image (2KB)"));
        let parts = json!({"type": "parts", "file": {"count": 3, "originalSize": 500}});
        assert_eq!(line("Read", &none, &parts).as_deref(), Some("Read 3 pages (500 bytes)"));
        let empty_nb = json!({"type": "notebook", "file": {"cells": []}});
        assert_eq!(line("Read", &none, &empty_nb).as_deref(), Some("No cells found in notebook"));
        let search = json!({"results": [{"tool_use_id": "s", "content": []}, "text"],
            "durationSeconds": 4.2});
        assert_eq!(line("WebSearch", &none, &search).as_deref(), Some("Did 1 search in 4s"));
        let quick = json!({"searchCount": 3, "durationSeconds": 0.45});
        assert_eq!(line("WebSearch", &none, &quick).as_deref(), Some("Did 3 searches in 450ms"));
        let image_out = json!({"stdout": "data:image/png;base64,AA", "isImage": true});
        assert_eq!(
            line("Bash", &none, &image_out).as_deref(),
            Some("Image data detected and sent to Claude")
        );
        let mkdir = json!({"stdout": "", "stderr": "", "noOutputExpected": true});
        assert_eq!(line("Bash", &none, &mkdir).as_deref(), Some("Done"));
        let printed = json!({"stdout": "x", "noOutputExpected": true});
        assert_eq!(line("Bash", &none, &printed), None, "output says itself");
        let staged = json!({"staged": true});
        assert!(line("Write", &none, &staged).is_some_and(|l| l.starts_with("Staged for")));
        let skill = json!({"success": true, "allowedTools": ["Bash", "Read"], "model": "sonnet"});
        assert_eq!(
            line("Skill", &none, &skill).as_deref(),
            Some("Successfully loaded skill · 2 tools allowed · sonnet")
        );
        let forked = json!({"status": "forked", "background": true});
        assert_eq!(line("Skill", &none, &forked).as_deref(), Some("Running in the background"));
        let stop = json!({"command": "npm run dev"});
        assert_eq!(line("TaskStop", &none, &stop).as_deref(), Some("npm run dev · stopped"));
        let task = json!({"status": "completed",
            "result": {"isError": true, "content": [{"type": "text", "text": "boom"}]}});
        assert_eq!(
            line("GetTask", &none, &task).as_deref(),
            Some("completed with errors · 4 chars of output")
        );
        let cloud =
            json!({"status": "remote_launched", "taskId": "t9", "sessionUrl": "https://c/s"});
        assert_eq!(
            line("Agent", &none, &cloud).as_deref(),
            Some("Cloud agent launched · t9 · https://c/s")
        );
        let worktree = json!({"worktreeBranch": "fix", "worktreePath": "/w/.wt/fix"});
        assert_eq!(
            line("EnterWorktree", &none, &worktree).as_deref(),
            Some("Switched to worktree on branch fix · /w/.wt/fix")
        );
        let out = json!({"action": "keep", "worktreeBranch": "fix", "originalCwd": "/w"});
        assert_eq!(
            line("ExitWorktree", &none, &out).as_deref(),
            Some("Kept worktree (branch fix) · returned to /w")
        );
        let cron = json!({"id": "7f2a", "humanSchedule": "every 5 minutes",
            "nextFireAt": 1_300_000});
        assert_eq!(
            line("CronCreate", &none, &cron).as_deref(),
            Some("Scheduled 7f2a (every 5 minutes, next run in 4m)")
        );
        assert_eq!(
            line("ListMcpResourcesTool", &none, &json!([])).as_deref(),
            Some("(No resources found)")
        );
        assert_eq!(line("Glob", &none, &json!({"numFiles": 3})), None);
        let rejected =
            said("Read", Outcome { status: ResultStatus::Rejected, ..outcome(None, Some(&pdf)) });
        assert_eq!(rejected, Said::default(), "a call turned down says only that");
    }

    /// A list the TUI draws under its call stands for the model's text; what it could not do
    /// reads as its line.
    #[test]
    fn lists_are_the_calls_body() {
        let none = json!({});
        let jobs = json!({"jobs": [
            {"id": "a1", "humanSchedule": "hourly", "recurring": true, "durable": false,
             "nextFireAt": 1_120_000, "prompt": "check the build"}]});
        let said_jobs = said("CronList", outcome(None, Some(&jobs)));
        assert_eq!(said_jobs.line.as_deref(), Some("1 scheduled job"));
        assert_eq!(
            said_jobs.body.as_deref(),
            Some("a1 hourly (recurring) [session-only], next run in 1m: check the build")
        );
        assert_eq!(
            line("CronList", &none, &json!({"jobs": []})).as_deref(),
            Some("No scheduled jobs")
        );
        let asked = json!({"questions": [{"question": "Which?", "options": [{"label": "A"},
            {"label": "B"}]}]});
        let waited = json!({"answers": {}, "afkTimeoutMs": 60_000});
        let afk = said("AskUserQuestion", outcome(Some(&asked), Some(&waited)));
        assert_eq!(
            afk.line.as_deref(),
            Some("No response after 60s — continued without an answer")
        );
        assert_eq!(afk.body.as_deref(), Some("· Which? (A / B)"));
        let follow = json!({"answers": {"Which?": "A"}, "followUp": true});
        let more = said("AskUserQuestion", outcome(Some(&asked), Some(&follow)));
        assert_eq!(more.line.as_deref(), Some("User asked Claude for more questions (so far)"));
        assert_eq!(more.body.as_deref(), Some("· Which? → A"));
        let resumed = json!({"inlineHandback": {"displayName": "reviewer",
            "content": [{"type": "text", "text": "All good."}]}});
        let handback = said("SendMessage", outcome(None, Some(&resumed)));
        assert_eq!(handback.line.as_deref(), Some("Resumed agent reviewer"));
        assert_eq!(handback.body.as_deref(), Some("Result:\nAll good."));
        let failed = json!({"ok": false, "reason": "expired"});
        assert_eq!(
            line("FetchInboxMessage", &none, &failed).as_deref(),
            Some("Inbox read failed · expired")
        );
        let notes = json!({"remaining": 2, "notifications": [
            {"origin": "mcp_send_message", "content": "hi"}]});
        let read = said("ReadNotifications", outcome(None, Some(&notes)));
        assert_eq!(
            read.line.as_deref(),
            Some("1 notification · 2 more notifications still queued")
        );
        assert_eq!(read.body.as_deref(), Some("⟵ mcp send message · message from another session"));
        let big = "x".repeat(48_000);
        assert_eq!(
            said("mcp__db__query", Outcome { text: &big, ..outcome(None, None) }).line.as_deref(),
            Some("Large MCP response (~12k tokens), this can fill up context quickly")
        );
    }
}
