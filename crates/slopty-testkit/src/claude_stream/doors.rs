//! The stand-in's doors beside messages, each as Claude Code 2.1.301 answers it.
//!
//! The doors are its background work, its dialogs and forms, its file checkpoints, its MCP
//! servers' apps, its permission rules and its mods' surfaces.
//!
//! - **Background work.** `background <cmd>` starts a background command (`task_started`,
//!   `background_tasks_changed`) that runs until `stop_task` names it, then ends killed
//!   (`task_updated`, a `stopped` `task_notification`). `wait <secs>` is a foreground command with
//!   `tool_progress` each step, which `background_tasks` moves to the background. `monitor` is work
//!   no call started: its progress and its notice name no call.
//! - **Signals.** `signals` says a `notification`, an `active_goal`, the system rows Claude Code
//!   derives (memories saved, a permission retried, a model fallback, a hook failed, long
//!   instructions) and a `prompt_suggestion`.
//! - **Dialogs and forms.** `dialog <kind>` raises `request_user_dialog` with two options; `elicit`
//!   an MCP server's `elicitation` with a schema. The answer is the turn's words; an interrupt
//!   withdraws the ask (`control_cancel_request`).
//! - **Checkpoints.** `write <path> <words>` writes a file in the folder, kept against the turn's
//!   message; `rewind_files` puts back every file written from that message on, and refuses a
//!   message it holds no checkpoint for. `plan` asks `ExitPlanMode` with a plan.
//! - **MCP Apps.** `mcp_status` lists each `--mcp-config` server with its tools, asked as a host
//!   with the UI extension; `mcp_read_resource` and `mcp_call` go to the server named, its error
//!   refused.
//! - **Rules.** `list_permission_rules` reads the `permissions` of the person's, the project's and
//!   the local settings files, and `--allowedTools` as the session's.
//! - **Surfaces.** With `STUB_UI` set, a mod `ship` draws a pane `deploy` and the band above the
//!   prompt; pressing its button says a toast and a status segment and draws again; closing the
//!   pane takes it off the roster. Without it nothing is hooked.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Cut, MCP_PATIENCE, Stub, epoch_s, merge, refuse, reply, say, timestamp, uuid};

/// The UI extension's id, as a host declares it.
const UI_EXTENSION: &str = "io.modelcontextprotocol/ui";

/// The mod the surfaces are drawn for.
const MOD: &str = "ship";

/// Its pane.
const PANE: &str = "deploy";

/// One file written by a turn: what it held before (`None` when it was not there).
#[derive(Clone, Debug)]
pub(super) struct Checkpoint {
    /// The message whose turn wrote it.
    message: String,
    path: PathBuf,
    before: Option<String>,
}

/// A background command running.
#[derive(Clone, Debug)]
pub(super) struct Running {
    tool_use: String,
    description: String,
    output: PathBuf,
}

/// What the doors keep between lines.
#[derive(Debug, Default)]
pub(super) struct Doors {
    /// Background commands running, by task id.
    running: std::collections::BTreeMap<String, Running>,
    /// Background tasks started so far.
    tasks: u64,
    /// The foreground call a `wait` holds, and whether `background_tasks` moved it.
    waiting: Option<(String, bool)>,
    /// Every file written, in order.
    checkpoints: Vec<Checkpoint>,
    /// The messages of the turns so far.
    prompts: Vec<String>,
    /// `STUB_UI`: a mod draws surfaces.
    ui: bool,
    /// The mod's pane was closed.
    closed: bool,
    /// The mod's button was pressed.
    shipped: bool,
}

impl Doors {
    pub(super) fn new() -> Self {
        Self { ui: std::env::var_os("STUB_UI").is_some(), ..Self::default() }
    }

    /// The turn of message `id` begins.
    pub(super) fn prompted(&mut self, id: String) {
        self.prompts.push(id);
    }
}

impl Stub {
    /// Answer control request `id` of `subtype`, one of the doors: whether it was one.
    pub(super) fn door(&mut self, id: &str, subtype: &str, request: &Value) -> bool {
        let field = |name: &str| request.get(name).and_then(Value::as_str).map(str::to_owned);
        match subtype {
            "background_tasks" => self.backgrounded(id, field("tool_use_id").as_deref()),
            "rewind_files" => self.rewind(id, &field("user_message_id").unwrap_or_default()),
            "list_permission_rules" => reply(id, Some(json!({ "state": self.rules() }))),
            "mcp_status" => {
                let servers: Vec<Value> = self.servers().iter().map(status).collect();
                reply(id, Some(json!({ "mcpServers": servers })));
            }
            "mcp_read_resource" => {
                let uri = field("uri").unwrap_or_default();
                let server = field("serverName").unwrap_or_default();
                let read = json!({ "uri": uri });
                match self.server(&server).and_then(|s| mcp(&s, "resources/read", &read)) {
                    Ok(result) => reply(id, Some(result)),
                    Err(_) => refuse(id, &format!("mcp_read_resource: no resource Some({uri:?})")),
                }
            }
            "mcp_call" => self.mcp_call(id, request),
            "ui_attach" => reply(id, Some(json!({ "surfaces": ["desktop"] }))),
            "ui_panes" => reply(id, Some(self.roster())),
            "ui_render" => {
                let component = field("component").unwrap_or_default();
                reply(id, Some(self.drawn(&component)));
            }
            "ui_press" => self.pressed(id, request),
            "ui_input" | "ui_select" if self.doors.ui => reply(id, Some(json!({}))),
            "ui_close" if self.doors.ui && field("id").as_deref() == Some(PANE) => {
                self.doors.closed = true;
                reply(id, Some(json!({ "closed": true })));
            }
            _ => return false,
        }
        true
    }

    /// `stop_task` of a background command: it ends killed, its notice says it stopped.
    pub(super) fn stop_running(&mut self, id: &str, task: &str) -> bool {
        let Some(running) = self.doors.running.remove(task) else { return false };
        let end = epoch_s().saturating_mul(1000);
        self.system(
            "task_updated",
            &json!({ "task_id": task, "patch": { "status": "killed", "end_time": end } }),
        );
        self.system(
            "task_notification",
            &json!({
                "task_id": task, "tool_use_id": running.tool_use, "status": "stopped",
                "summary": running.description, "output_file": running.output,
            }),
        );
        self.tasks_changed();
        reply(id, Some(json!({})));
        true
    }

    /// The background tasks running, as `background_tasks_changed` says them whole.
    fn tasks_changed(&self) {
        let tasks: Vec<Value> = self
            .doors
            .running
            .iter()
            .map(|(task, r)| {
                json!({ "task_id": task, "description": r.description, "task_type": "local_bash" })
            })
            .collect();
        self.system("background_tasks_changed", &json!({ "tasks": tasks }));
    }

    /// A background task of its own for call `tool_use`: its id and where it writes.
    fn background_task(&mut self, tool_use: &str, description: &str) -> (String, PathBuf) {
        self.doors.tasks = self.doors.tasks.saturating_add(1);
        let task = format!("b{:08}", self.doors.tasks);
        let output =
            std::env::temp_dir().join(format!("stub-claude-{}-{task}.output", self.session));
        drop(std::fs::write(&output, format!("{description}: started\n")));
        let running = Running {
            tool_use: tool_use.to_owned(),
            description: description.to_owned(),
            output: output.clone(),
        };
        self.doors.running.insert(task.clone(), running);
        self.tasks_changed();
        self.system(
            "task_started",
            &json!({
                "task_id": task, "tool_use_id": tool_use, "description": description,
                "task_type": "local_bash", "is_backgrounded": true,
            }),
        );
        (task, output)
    }

    /// `background <command>`: a command run in the background, until it is stopped.
    pub(super) fn background_command(&mut self, command: &str) -> Result<String, Cut> {
        let tool_use = format!("toolu_bg{}", self.turns);
        let description = format!("Run {command}");
        let input =
            json!({ "command": command, "description": description, "run_in_background": true });
        self.assistant(
            &json!({ "type": "tool_use", "id": tool_use, "name": "Bash", "input": input }),
            None,
            None,
        );
        let (task, output) = self.background_task(&tool_use, &description);
        let said = format!(
            "Command running in background with ID: {task}. Output is being written to: {}. You \
             will be notified when it completes.",
            output.display()
        );
        let structured = json!({ "stdout": "", "stderr": "", "backgroundTaskId": task });
        self.tool_result_with(&tool_use, &said, false, &structured);
        self.pause(self.gap)?;
        let said = "Done: it runs in the background.";
        self.assistant(&json!({ "type": "text", "text": said }), None, None);
        Ok(said.to_owned())
    }

    /// `wait <secs>`: a foreground command, its progress each step, which `background_tasks`
    /// moves to the background.
    pub(super) fn waited(&mut self, secs: u64) -> Result<String, Cut> {
        let tool_use = format!("toolu_w{}", self.turns);
        let description = format!("Wait {secs} seconds");
        let input = json!({ "command": format!("sleep {secs}"), "description": description });
        self.assistant(
            &json!({ "type": "tool_use", "id": tool_use, "name": "Bash", "input": input }),
            None,
            None,
        );
        self.doors.waiting = Some((tool_use.clone(), false));
        let started = Instant::now();
        let until = started.checked_add(Duration::from_secs(secs)).unwrap_or(started);
        let step = self.gap.max(Duration::from_millis(100));
        while Instant::now() < until && !self.doors.waiting.as_ref().is_some_and(|(_, m)| *m) {
            if let Err(cut) = self.pause(step) {
                self.doors.waiting = None;
                return Err(cut);
            }
            say(&json!({
                "type": "tool_progress", "tool_use_id": tool_use, "tool_name": "Bash",
                "elapsed_time_seconds": started.elapsed().as_secs(), "parent_tool_use_id": null,
                "session_id": self.session, "uuid": uuid(),
            }));
        }
        let moved = self.doors.waiting.take().is_some_and(|(_, moved)| moved);
        if moved {
            let (task, output) = self.background_task(&tool_use, &description);
            let said = format!(
                "Command was manually backgrounded by user with ID: {task}. Output is being \
                 written to: {}",
                output.display()
            );
            let structured = json!({ "stdout": "", "stderr": "", "backgroundTaskId": task });
            self.tool_result_with(&tool_use, &said, false, &structured);
        } else {
            self.tool_result(&tool_use, "", false);
        }
        self.pause(self.gap)?;
        let said = if moved { "Done: it went on in the background." } else { "Done: waited." };
        self.assistant(&json!({ "type": "text", "text": said }), None, None);
        Ok(said.to_owned())
    }

    /// `background_tasks`: the call a `wait` holds goes to the background; one named that does
    /// not block the turn is not moved.
    fn backgrounded(&mut self, id: &str, call: Option<&str>) {
        match &mut self.doors.waiting {
            Some((held, moved)) if call.is_none_or(|c| c == held) => {
                *moved = true;
                reply(id, Some(json!({ "backgrounded": true })));
            }
            _ if call.is_some() => reply(id, Some(json!({ "backgrounded": false }))),
            _ => reply(id, Some(json!({}))),
        }
    }

    /// `monitor`: work no call started, its progress and its notice naming none.
    pub(super) fn monitor(&mut self) -> Result<String, Cut> {
        self.doors.tasks = self.doors.tasks.saturating_add(1);
        let task = format!("m{:08}", self.doors.tasks);
        self.system(
            "task_started",
            &json!({ "task_id": task, "description": "Watch the build log", "task_type": "monitor" }),
        );
        self.pause(self.gap)?;
        self.system(
            "task_progress",
            &json!({
                "task_id": task, "summary": "build: step 2 of 3",
                "usage": { "total_tokens": 0, "tool_uses": 0, "duration_ms": 20 },
            }),
        );
        self.pause(self.gap)?;
        self.system(
            "task_notification",
            &json!({
                "task_id": task, "status": "completed",
                "summary": "Monitor \"Watch the build log\" stream ended",
                "usage": { "total_tokens": 0, "tool_uses": 0, "duration_ms": 40 },
            }),
        );
        let said = "Done: the build log ended.";
        self.assistant(&json!({ "type": "text", "text": said }), None, None);
        Ok(said.to_owned())
    }

    /// `signals`: what Claude Code says beside the conversation.
    pub(super) fn signals(&mut self) -> Result<String, Cut> {
        self.system(
            "notification",
            &json!({
                "key": "stub-heads-up", "text": "Heads up from the stand-in", "priority": "high",
                "color": "warning", "timeout_ms": 8000,
            }),
        );
        say(&json!({
            "type": "active_goal", "session_id": self.session, "uuid": uuid(),
            "value": {
                "condition": "the tests pass", "iterations": 1,
                "set_at": epoch_s().saturating_mul(1000), "tokens_at_start": 0,
            },
        }));
        let rows = [
            ("memory_saved", json!({ "verb": "Saved", "written_paths": ["/m/a.md", "/m/b.md"] })),
            ("permission_retry", json!({ "commands": ["npm test"] })),
            ("model_fallback", json!({ "fallback_model": "claude-sonnet-5-5" })),
            (
                "hook_response",
                json!({ "hook_name": "PreToolUse", "outcome": "error", "stderr": "lint failed",
                        "exit_code": 1 }),
            ),
            (
                "instruction_size_warning",
                json!({ "total_chars": 50_000, "total_limit_chars": 40_000 }),
            ),
        ];
        for (subtype, fields) in rows {
            self.system(subtype, &fields);
            self.pause(self.gap)?;
        }
        let said = "Done: signalled.";
        self.assistant(&json!({ "type": "text", "text": said }), None, None);
        say(&json!({
            "type": "prompt_suggestion", "suggestion": "Run the tests",
            "session_id": self.session, "uuid": uuid(),
        }));
        Ok(said.to_owned())
    }

    /// `dialog <kind>`: a dialog of `kind` with two options; its answer is the turn's words.
    pub(super) fn dialog(&mut self, kind: &str) -> Result<String, Cut> {
        let request = json!({
            "subtype": "request_user_dialog", "dialog_kind": kind, "tool_use_id": null,
            "payload": {
                "title": "Go ahead?", "question": "Go ahead with the change?",
                "options": [{ "value": "yes", "label": "Yes" }, { "value": "no", "label": "No" }],
            },
        });
        let answer = self.request(&request, true)?;
        let said = match answer.get("subtype").and_then(Value::as_str) {
            Some("success") => {
                let response = answer.get("response").cloned().unwrap_or(Value::Null);
                let behavior = response.get("behavior").and_then(Value::as_str).unwrap_or("none");
                let result = response.get("result").cloned().unwrap_or(Value::Null);
                format!("Dialog {kind}: {behavior} {result}")
            }
            _ => format!("Dialog {kind} failed"),
        };
        self.assistant(&json!({ "type": "text", "text": said }), None, None);
        Ok(said)
    }

    /// `elicit`: an MCP server's form; its answer is the turn's words.
    pub(super) fn elicit(&mut self) -> Result<String, Cut> {
        let request = json!({
            "subtype": "elicitation", "mcp_server_name": "tracker", "message": "Which ticket?",
            "requested_schema": {
                "type": "object", "required": ["ticket"],
                "properties": { "ticket": { "type": "string", "title": "Ticket" } },
            },
        });
        let answer = self.request(&request, false)?;
        let response = answer.get("response").cloned().unwrap_or(Value::Null);
        let action = response.get("action").and_then(Value::as_str).unwrap_or("none");
        let content = response.get("content").cloned().unwrap_or(Value::Null);
        let said = format!("Elicitation: {action} {content}");
        self.assistant(&json!({ "type": "text", "text": said }), None, None);
        Ok(said)
    }

    /// `write <path> <words>`: a file written in the folder, kept against the turn's message.
    pub(super) fn write_file(&mut self, rest: &str) -> Result<String, Cut> {
        let (name, words) = rest.split_once(' ').unwrap_or((rest, "written"));
        let path = Path::new(&self.cwd).join(name);
        let tool_use = format!("toolu_f{}", self.turns);
        let input = json!({ "file_path": path, "content": format!("{words}\n") });
        self.assistant(
            &json!({ "type": "tool_use", "id": tool_use, "name": "Write", "input": input }),
            None,
            None,
        );
        let message = self.doors.prompts.last().cloned().unwrap_or_default();
        let before = std::fs::read_to_string(&path).ok();
        self.doors.checkpoints.push(Checkpoint { message, path: path.clone(), before });
        let written = std::fs::write(&path, format!("{words}\n"));
        let (said, failed) = match written {
            Ok(()) => (format!("File created successfully at: {}", path.display()), false),
            Err(e) => (format!("{}: {e}", path.display()), true),
        };
        self.tool_result(&tool_use, &said, failed);
        self.pause(self.gap)?;
        let said = format!("Done: wrote {name}.");
        self.assistant(&json!({ "type": "text", "text": said }), None, None);
        Ok(said)
    }

    /// `rewind_files`: every file written from message `message` on, as it was before; a
    /// message with no checkpoint is refused.
    fn rewind(&mut self, id: &str, message: &str) {
        if !self.doors.prompts.iter().any(|p| p == message) {
            refuse(id, "No file checkpoint found for this message.");
            return;
        }
        let from = self.doors.prompts.iter().position(|p| p == message).unwrap_or_default();
        let later: Vec<&String> = self.doors.prompts.iter().skip(from).collect();
        let (put_back, kept): (Vec<Checkpoint>, Vec<Checkpoint>) =
            self.doors.checkpoints.drain(..).partition(|c| later.iter().any(|p| **p == c.message));
        self.doors.checkpoints = kept;
        let mut files: Vec<PathBuf> = Vec::new();
        let (mut insertions, mut deletions) = (0_usize, 0_usize);
        for checkpoint in put_back {
            // The first write of a file holds what it was before them all.
            if files.contains(&checkpoint.path) {
                continue;
            }
            let now = std::fs::read_to_string(&checkpoint.path).unwrap_or_default();
            deletions = deletions.saturating_add(now.lines().count());
            match &checkpoint.before {
                Some(before) => {
                    insertions = insertions.saturating_add(before.lines().count());
                    drop(std::fs::write(&checkpoint.path, before));
                }
                None => drop(std::fs::remove_file(&checkpoint.path)),
            }
            files.push(checkpoint.path);
        }
        reply(
            id,
            Some(json!({
                "canRewind": true, "filesChanged": files, "insertions": insertions,
                "deletions": deletions,
            })),
        );
    }

    /// `plan`: `ExitPlanMode` asked with a plan; allowed, it goes on to carry it out.
    pub(super) fn plan(&mut self) -> Result<String, Cut> {
        let tool_use = format!("toolu_p{}", self.turns);
        let input = json!({ "plan": "1. Write the file\n2. Run the tests" });
        self.assistant(
            &json!({ "type": "tool_use", "id": tool_use, "name": "ExitPlanMode", "input": input }),
            None,
            None,
        );
        let answer = self.ask("ExitPlanMode", &input, &tool_use, &json!({}))?;
        let allowed = answer.pointer("/response/behavior").and_then(Value::as_str) == Some("allow");
        if !allowed {
            let message = answer
                .pointer("/response/message")
                .and_then(Value::as_str)
                .unwrap_or("The person said no.")
                .to_owned();
            self.tool_result(&tool_use, &message, true);
            if answer.pointer("/response/interrupt").and_then(Value::as_bool) == Some(true) {
                return Err(Cut::Interrupt);
            }
            let said = "Done: the plan was not approved.";
            self.assistant(&json!({ "type": "text", "text": said }), None, None);
            return Ok(said.to_owned());
        }
        self.tool_result(
            &tool_use,
            "User has approved your plan. You can now start coding.",
            false,
        );
        self.pause(self.gap)?;
        let said = "Done: carrying out the plan.";
        self.assistant(&json!({ "type": "text", "text": said }), None, None);
        Ok(said.to_owned())
    }

    /// The rules the settings files and `--allowedTools` give, as `list_permission_rules` says
    /// them.
    fn rules(&self) -> Value {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let cwd = Path::new(&self.cwd);
        let files = [
            ("userSettings", home.join(".claude").join("settings.json")),
            ("projectSettings", cwd.join(".claude").join("settings.json")),
            ("localSettings", cwd.join(".claude").join("settings.local.json")),
        ];
        let mut rules = Vec::new();
        for (source, file) in files {
            let doc = std::fs::read_to_string(&file)
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                .unwrap_or(Value::Null);
            for behavior in ["allow", "deny", "ask"] {
                let listed =
                    doc.pointer(&format!("/permissions/{behavior}")).and_then(Value::as_array);
                for rule in listed.into_iter().flatten().filter_map(Value::as_str) {
                    rules.push(json!({
                        "behavior": behavior, "source": source, "rule": rule,
                        "editability": "persistent",
                        "description": { "prefix": "", "emphasis": rule, "suffix": "" },
                    }));
                }
            }
        }
        let allowed = super::flag(&self.args, "--allowedTools").unwrap_or_default();
        for rule in allowed.split([',', ' ']).filter(|r| !r.is_empty()) {
            rules.push(json!({
                "behavior": "allow", "source": "cliArg", "rule": rule, "editability": "session",
                "description": { "prefix": "", "emphasis": rule, "suffix": "" },
            }));
        }
        json!({ "rules": rules, "workspaceDirectories": [], "managedOnly": false, "originalCwd": self.cwd })
    }

    /// The `--mcp-config` servers, by name.
    fn servers(&self) -> Vec<(String, Value)> {
        self.mcp_configs
            .iter()
            .filter_map(|doc| doc.get("mcpServers").and_then(Value::as_object))
            .flat_map(|servers| servers.iter().map(|(name, s)| (name.clone(), s.clone())))
            .collect()
    }

    /// The server named `name`.
    fn server(&self, name: &str) -> Result<Value, String> {
        self.servers()
            .into_iter()
            .find(|(n, _)| n == name)
            .map(|(_, s)| s)
            .ok_or_else(|| format!("no MCP server {name}"))
    }

    /// `mcp_call`: `mcp__<server>__<tool>` called on the server, for the page alone.
    fn mcp_call(&self, id: &str, request: &Value) {
        let tool = request.get("tool").and_then(Value::as_str).unwrap_or_default();
        let arguments = request.get("arguments").cloned().unwrap_or_else(|| json!({}));
        let named = tool.strip_prefix("mcp__").and_then(|rest| {
            self.servers().into_iter().find_map(|(name, server)| {
                let tool = rest.strip_prefix(&format!("{}__", normalized(&name)))?;
                Some((server, tool.to_owned()))
            })
        });
        let Some((server, tool)) = named else {
            refuse(id, &format!("mcp_call: no tool {tool}"));
            return;
        };
        match mcp(&server, "tools/call", &json!({ "name": tool, "arguments": arguments })) {
            Ok(result) => reply(id, Some(result)),
            Err(why) => refuse(id, &format!("mcp_call: {why}")),
        }
    }

    /// The pane roster, as `ui_panes` answers it.
    fn roster(&self) -> Value {
        let panes = if self.doors.ui && !self.doors.closed {
            json!([{ "id": PANE, "title": "Deploy", "plugin": MOD }])
        } else {
            json!([])
        };
        json!({ "panes": panes, "focused_id": null, "shown_id": null, "focus_requested_id": null })
    }

    /// The tree the mod draws for `component`, as `ui_render` answers it.
    fn drawn(&self, component: &str) -> Value {
        if !self.doors.ui {
            return json!({ "tree": null, "hooked": false });
        }
        let state = if self.doors.shipped { "shipped" } else { "ready" };
        let tree = match component {
            "Pane" if !self.doors.closed => json!({
                "type": "Box",
                "children": [
                    { "type": "Text", "props": { "text": format!("Deploy: {state}") } },
                    { "type": "Button", "props": { "key": "go", "label": "Ship it" },
                      "press": { "plugin": MOD, "handle": 1 } },
                ],
            }),
            "AbovePrompt" => {
                json!({ "type": "Text", "props": { "text": format!("ship: {state}") } })
            }
            _ => return json!({ "tree": null, "hooked": false }),
        };
        json!({ "tree": tree, "hooked": true })
    }

    /// `ui_press`: the mod's button ships, says so as a toast and a status segment, and asks to
    /// be drawn again.
    fn pressed(&mut self, id: &str, request: &Value) {
        let plugin = request.get("plugin").and_then(Value::as_str);
        let handle = request.get("handle").and_then(Value::as_i64);
        if !self.doors.ui || plugin != Some(MOD) || handle != Some(1) {
            refuse(id, "ui_press: no such element");
            return;
        }
        self.doors.shipped = true;
        reply(id, Some(json!({})));
        self.system("ui_toast", &json!({ "plugin": MOD, "text": "Shipped", "timeout_ms": 4000 }));
        self.system("ui_status", &json!({ "plugin": MOD, "text": "shipped" }));
        self.system("ui_invalidate", &json!({ "plugin": MOD, "instances": [] }));
    }

    /// A user message Claude Code takes in without a turn (`shouldQuery: false`): kept, never
    /// echoed, its result at once.
    pub(super) fn taken_in(&mut self, message: &Value, id: &str) {
        self.lifecycle(id, "started");
        let mut kept = message.clone();
        merge(&mut kept, &json!({ "timestamp": timestamp() }));
        if let Some(fields) = kept.as_object_mut() {
            fields.remove("shouldQuery");
        }
        self.keep(&kept);
        say(&json!({
            "type": "result", "subtype": "success", "is_error": false, "result": "",
            "duration_ms": 11, "duration_api_ms": 0, "num_turns": 0, "session_id": self.session,
            "total_cost_usd": 0.0, "usage": { "input_tokens": 0, "output_tokens": 0 },
            "modelUsage": {}, "permission_denials": [], "uuid": uuid(),
        }));
        self.lifecycle(id, "completed");
    }

    /// Ask the host `request`, Claude Code's own control request, and wait for its answer; a
    /// dialog is listed as one while it waits. An interrupt withdraws it.
    pub(super) fn request(&mut self, request: &Value, dialog: bool) -> Result<Value, Cut> {
        let id = uuid();
        let asked = json!({ "type": "control_request", "request_id": id, "request": request });
        if dialog {
            self.dialogs.insert(id.clone(), asked.clone());
        } else {
            self.asked.insert(id.clone(), asked.clone());
        }
        self.state("requires_action");
        say(&asked);
        loop {
            if let Some(answer) = self.answers.remove(&id) {
                self.dialogs.remove(&id);
                self.state("running");
                return Ok(answer);
            }
            if let Err(cut) = self.pause(self.gap) {
                let open = self.asked.remove(&id).is_some() || self.dialogs.remove(&id).is_some();
                if open {
                    say(&json!({ "type": "control_cancel_request", "request_id": id }));
                }
                return Err(cut);
            }
        }
    }
}

/// A server's name as Claude Code names its tools (`mcp__<server>__<tool>`).
fn normalized(server: &str) -> String {
    server
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect()
}

/// Server `name` as `mcp_status` says it, its tools asked for as a host with the UI extension.
fn status((name, server): &(String, Value)) -> Value {
    match mcp(server, "tools/list", &json!({})) {
        Ok(listed) => json!({
            "name": name, "status": "connected", "scope": "dynamic",
            "tools": listed.get("tools").cloned().unwrap_or_else(|| json!([])),
        }),
        Err(why) => json!({ "name": name, "status": "failed", "error": why }),
    }
}

/// `method` with `params` asked of the stdio MCP server `server` started for it, after an
/// `initialize` that declares the UI extension: its result, or its error in words.
fn mcp(server: &Value, method: &str, params: &Value) -> Result<Value, String> {
    let command =
        server.get("command").and_then(Value::as_str).ok_or("the server names no command")?;
    let args = server.get("args").and_then(Value::as_array).into_iter().flatten();
    let mut child = Command::new(command)
        .args(args.filter_map(Value::as_str))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{command}: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("the server's stdin")?;
    let stdout = child.stdout.take().ok_or("the server's stdout")?;
    let (answer, answered) = mpsc::channel();
    drop(std::thread::spawn(move || {
        for msg in BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        {
            if answer.send(msg).is_err() {
                return;
            }
        }
    }));
    let initialize = json!({
        "jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": { "extensions": { UI_EXTENSION: { "mimeTypes": ["text/html;profile=mcp-app"] } } },
            "clientInfo": { "name": "stub-claude", "version": super::VERSION },
        },
    });
    let asked = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    let wait = |id: u64| -> Result<Value, String> {
        let until = Instant::now().checked_add(MCP_PATIENCE).unwrap_or_else(Instant::now);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            let msg = answered.recv_timeout(left).map_err(|e| format!("no answer: {e}"))?;
            if msg.get("id").and_then(Value::as_u64) == Some(id) {
                return Ok(msg);
            }
        }
    };
    let result = (|| {
        writeln!(stdin, "{initialize}").map_err(|e| e.to_string())?;
        wait(0)?;
        writeln!(stdin, "{initialized}").map_err(|e| e.to_string())?;
        writeln!(stdin, "{asked}").map_err(|e| e.to_string())?;
        stdin.flush().map_err(|e| e.to_string())?;
        let msg = wait(1)?;
        match msg.get("error") {
            Some(error) => Err(error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the server failed")
                .to_owned()),
            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
        }
    })();
    drop(stdin);
    drop(child.kill());
    drop(child.wait());
    result
}
