//! What Slopty asks of Claude Code beside messages and settings, each through its control
//! protocol: an MCP App's resource read and tool called for its page, the permission rules, the
//! context's categories, a running call moved to the background, files put back to before a turn,
//! a `!` command's output handed over, and the surfaces its mods draw.
//!
//! - **MCP Apps.** `mcp_status` names the tools whose descriptor points at a `ui://` resource; a
//!   call of one shows as its app's page. A page's `resources/read` goes as `mcp_read_resource`,
//!   and its `tools/call` as `mcp_call` with `result_not_for_model`, so the result reaches the page
//!   whole and never the model; the server is never dialled by Slopty.
//! - **Mods' surfaces.** Slopty attaches as a `desktop` surface ([`SURFACE_CLIENT`]), asks for the
//!   band above the prompt and each pane the roster names, and shows each tree with its own
//!   components; a toast, a status segment and a log line come as they are pushed. Each pressable
//!   or editable element gets an `id` (`<plugin>:<handle>`) that the person's act names, and a
//!   surface is drawn again after each act. A Claude Code without mods' surfaces refuses the asks,
//!   and nothing is said of it.
//! - **MCP servers.** A server `system/init` lists as failed or wanting authentication is a passing
//!   note in the TUI's words ("MCP server \"github\" disconnected").

use std::collections::HashMap;
use std::fmt::Write as _;

use serde_json::{Value, json};
use slopty_core::WallMs;
use slopty_proto::thread::{
    Action, AgentNote, ContextCategory, ContextUsage, Delivery, IntentId, Notice, Pending,
    PendingState, Rule, Rules, Surface, SurfaceEvent, TurnId,
};
use slopty_proto::visual::{AppAnswer, AppAsk, AppBody, McpApp};

use super::{Control, Driven, bool_at, message_id, str_at, u64_at};
use crate::claude::history::Out;

/// How long a note about an MCP server shows, as the TUI's notification lasts.
const MCP_NOTE_MS: u64 = 12_000;

/// The client Slopty attaches to a session's mods' surfaces as.
pub const SURFACE_CLIENT: &str = "slopty";

/// The surface Slopty draws mods' trees for, as Claude Code names its kinds.
const SURFACE: &str = "desktop";

/// The band above the prompt, by its instance and the id it shows under.
const BAND: &str = "above-prompt";

/// How long a toast shows when the mod says nothing.
const TOAST_MS: u64 = 5_000;

/// What a `!` command did: what Claude Code is handed as the person's next message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ran {
    /// The command line.
    pub command: String,
    /// What it printed.
    pub stdout: String,
    /// What it printed as errors.
    pub stderr: String,
}

/// One of the asks a mod's surfaces make.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Ui {
    /// Slopty attaches as a surface.
    Attach,
    /// The pane roster.
    Panes,
    /// One surface drawn: its id in the thread, the component and instance Claude Code knows.
    Render { surface: String, component: String, instance: String },
    /// The person acted in a surface, which is drawn again after.
    Act { surface: String },
}

/// An element of a drawn tree the person can act in.
#[derive(Clone, Debug)]
struct Element {
    plugin: String,
    handle: i64,
    key: Option<String>,
    component: String,
    instance: String,
}

/// The surfaces the session's mods draw, as Slopty shows them.
#[derive(Debug, Default)]
pub(super) struct Surfaces {
    /// Each pane shown, by its id, with its title and mod.
    panes: HashMap<String, (Option<String>, String)>,
    /// The band shows.
    band: bool,
    /// The elements of each surface, by its id and theirs.
    elements: HashMap<String, HashMap<String, Element>>,
    /// Toasts shown so far, each a surface of its own.
    toasts: u64,
}

/// A server's name as Claude Code names its tools (`mcp__<server>__<tool>`).
fn normalized(server: &str) -> String {
    server
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect()
}

/// `tree` with an `id` on each element the person can act in, and those elements.
fn ided(tree: &mut Value, component: &str, instance: &str, out: &mut HashMap<String, Element>) {
    match tree {
        Value::Object(node) => {
            let press = node.get("press").cloned();
            if let Some(press) = press
                && let (Some(plugin), Some(handle)) =
                    (str_at(&press, "plugin"), press.get("handle").and_then(Value::as_i64))
            {
                let id = format!("{plugin}:{handle}");
                let key = node.get("props").and_then(|p| str_at(p, "key")).map(str::to_owned);
                out.insert(
                    id.clone(),
                    Element {
                        plugin: plugin.to_owned(),
                        handle,
                        key,
                        component: component.to_owned(),
                        instance: instance.to_owned(),
                    },
                );
                node.insert("id".to_owned(), Value::String(id));
            }
            for child in node.values_mut() {
                ided(child, component, instance, out);
            }
        }
        Value::Array(children) => {
            for child in children {
                ided(child, component, instance, out);
            }
        }
        _ => {}
    }
}

/// The first mod named in `tree`, as the elements it draws say.
fn plugin_in(tree: &Value) -> Option<String> {
    match tree {
        Value::Object(node) => node
            .get("press")
            .and_then(|p| str_at(p, "plugin"))
            .map(str::to_owned)
            .or_else(|| node.values().find_map(plugin_in)),
        Value::Array(children) => children.iter().find_map(plugin_in),
        _ => None,
    }
}

/// The thread fact the MCP servers' counts are kept in, which the context card shows.
const SERVERS_FACT: &str = "mcp-servers";

/// The servers `init` lists, counted by status as the TUI's `/status` row counts them: "3
/// connected, 1 pending, 1 failed"; `None` with none.
fn servers_words(servers: &[Value]) -> Option<String> {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for server in servers {
        let status = match str_at(server, "status").unwrap_or("pending") {
            "needs-auth" => "need auth",
            other => other,
        };
        match counts.iter_mut().find(|(s, _)| *s == status) {
            Some((_, n)) => *n = n.saturating_add(1),
            None => counts.push((status, 1)),
        }
    }
    let order = ["connected", "pending", "need auth", "disabled", "failed"];
    counts.sort_by_key(|(s, _)| order.iter().position(|o| o == s).unwrap_or(order.len()));
    let words: Vec<String> = counts.iter().map(|(s, n)| format!("{n} {s}")).collect();
    (!words.is_empty()).then(|| words.join(", "))
}

impl Driven {
    /// What only a question answers after `initialize`: the permission rules, the MCP servers'
    /// apps, and the surfaces the mods draw.
    pub(super) fn after_initialize(&mut self) {
        self.list_rules();
        self.control(Control::McpStatus, &json!({ "subtype": "mcp_status" }));
        let attach =
            json!({ "subtype": "ui_attach", "surface": SURFACE, "client_id": SURFACE_CLIENT });
        let id = self.control(Control::Ui(Ui::Attach), &attach);
        self.quiet.insert(id);
        let id = self.control(
            Control::Ui(Ui::Panes),
            &json!({ "subtype": "ui_panes", "client_id": SURFACE_CLIENT }),
        );
        self.quiet.insert(id);
        self.render(BAND, "AbovePrompt", BAND);
    }

    /// A control request Claude Code did not answer, said nowhere: a page's ask is answered
    /// with its reason.
    pub(super) fn unanswered(&mut self, control: &Control, error: &str) -> Vec<Out> {
        if let Control::App(app) = control {
            self.app_answers.push((*app, AppAnswer::Refused(error.to_owned())));
        }
        Vec::new()
    }

    /// The context by category, as `get_context_usage` answered.
    pub(super) fn context_used(&mut self, said: &Value) -> Vec<Out> {
        let used = u64_at(said, "totalTokens");
        let window = u64_at(said, "maxTokens");
        let mut outs = self.meters(|m| {
            m.context_tokens = used.or(m.context_tokens);
            m.context_window = window.or(m.context_window);
        });
        let categories: Vec<ContextCategory> = said
            .get("categories")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|c| {
                Some(ContextCategory {
                    name: str_at(c, "name")?.to_owned(),
                    tokens: u64_at(c, "tokens").unwrap_or_default(),
                    free: str_at(c, "kind") == Some("free"),
                    color: str_at(c, "color").map(str::to_owned),
                })
            })
            .collect();
        if categories.is_empty() {
            return outs;
        }
        let window = window.unwrap_or_default();
        let threshold = u64_at(said, "autoCompactThreshold")
            .filter(|_| said.get("isAutoCompactEnabled").and_then(Value::as_bool) != Some(false));
        let auto_compact_bp = threshold.filter(|_| window > 0).and_then(|t| {
            u32::try_from(t.saturating_mul(10_000).checked_div(window)?.min(10_000)).ok()
        });
        let usage =
            ContextUsage { categories, tokens: used.unwrap_or_default(), window, auto_compact_bp };
        outs.extend(self.history.tell(Action::ContextSet(usage)));
        outs
    }

    /// Move call `item`, or every call that blocks the turn, to the background (the TUI's
    /// Ctrl+B).
    pub fn background(&mut self, item: Option<&str>) {
        let mut request = json!({ "subtype": "background_tasks" });
        if let (Some(item), Some(fields)) = (item, request.as_object_mut()) {
            fields.insert("tool_use_id".to_owned(), Value::String(item.to_owned()));
        }
        self.control(Control::Background(item.map(str::to_owned)), &request);
    }

    pub(super) fn backgrounded(
        &mut self,
        call: Option<&str>,
        said: &Value,
        now: WallMs,
    ) -> Vec<Out> {
        let moved = said.get("backgrounded").and_then(Value::as_bool);
        if call.is_some() && moved == Some(false) {
            return self.history.notice(
                Notice::INFO,
                "That call is not running in the foreground any more",
                now,
            );
        }
        Vec::new()
    }

    /// Ask Claude Code for its permission rules, as its `/permissions` lists them.
    pub fn list_rules(&mut self) {
        self.control(Control::Rules, &json!({ "subtype": "list_permission_rules" }));
    }

    pub(super) fn rules_listed(&mut self, said: &Value) -> Vec<Out> {
        let state = said.get("state").unwrap_or(said);
        let rules = state
            .get("rules")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|r| {
                let description = r.get("description").map(|d| {
                    ["prefix", "emphasis", "suffix"]
                        .iter()
                        .filter_map(|part| str_at(d, part))
                        .collect::<Vec<_>>()
                        .join("")
                });
                Some(Rule {
                    behavior: str_at(r, "behavior")?.to_owned(),
                    text: str_at(r, "rule")?.to_owned(),
                    source: str_at(r, "source").unwrap_or_default().to_owned(),
                    description: description.filter(|d| !d.is_empty()),
                    editable: str_at(r, "editability") == Some("persistent"),
                    in_effect: !bool_at(r, "notInEffect"),
                })
            })
            .collect();
        let directories = state
            .get("workspaceDirectories")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|d| str_at(d, "path").map(str::to_owned))
            .collect();
        self.history.tell(Action::RulesSet(Rules { rules, directories }))
    }

    /// What `system/init` says of the MCP servers, at `now`: when their statuses changed, their
    /// tools are asked for again, for the apps among them, and a server that failed or wants
    /// authentication says so, as the TUI's notifications do.
    pub(super) fn servers_seen(&mut self, init: &Value, now: WallMs) -> Vec<Out> {
        let Some(servers) = init.get("mcp_servers").and_then(Value::as_array) else {
            return Vec::new();
        };
        let mut seen = String::new();
        for server in servers {
            let field = |name: &str| str_at(server, name).unwrap_or_default();
            let _written = write!(seen, "{}={};", field("name"), field("status"));
        }
        if seen == self.servers {
            return Vec::new();
        }
        let before: HashMap<&str, &str> =
            self.servers.split_terminator(';').filter_map(|pair| pair.split_once('=')).collect();
        let mut notes = Vec::new();
        for server in servers {
            let (Some(name), Some(status)) = (str_at(server, "name"), str_at(server, "status"))
            else {
                continue;
            };
            let was = before.get(name).copied();
            if was == Some(status) {
                continue;
            }
            let said = match (was, status) {
                (Some("connected"), "failed") => format!("MCP server \"{name}\" disconnected"),
                (_, "failed") => format!("MCP server \"{name}\" failed to connect"),
                (Some("connected"), "needs-auth") => {
                    format!("MCP server \"{name}\" lost authentication")
                }
                (_, "needs-auth") => format!("MCP server \"{name}\" needs authentication"),
                _ => continue,
            };
            notes.push((name.to_owned(), said));
        }
        self.servers = seen;
        self.control(Control::McpStatus, &json!({ "subtype": "mcp_status" }));
        let mut outs = self.history.fact(SERVERS_FACT, servers_words(servers).as_deref());
        for (name, said) in notes {
            let note = AgentNote {
                key: format!("mcp:{name}"),
                text: said,
                priority: "high".to_owned(),
                color: Some("warning".to_owned()),
                at_ms: now,
                until_ms: Some(WallMs::from_millis(now.as_millis().saturating_add(MCP_NOTE_MS))),
            };
            outs.extend(self.history.tell(Action::Notified(note)));
        }
        outs
    }

    /// What `init` says did not load: a plugin's errors, an MCP server's config skipped, a
    /// server the organization's policy blocks. Each is a passing note, said once.
    pub(super) fn init_problems(&mut self, init: &Value, now: WallMs) -> Vec<Out> {
        let listed = |key: &str| -> Vec<&Value> {
            init.get(key).and_then(Value::as_array).into_iter().flatten().collect()
        };
        let mut said = Vec::new();
        for error in listed("plugin_errors") {
            let plugin = str_at(error, "plugin").unwrap_or("a plugin");
            let message = str_at(error, "message").unwrap_or("it did not load");
            said.push(format!("Failed to load plugin {plugin}: {message}"));
        }
        for error in listed("mcp_server_errors") {
            let name = str_at(error, "name").unwrap_or("an MCP server");
            let message = str_at(error, "message").unwrap_or("its config does not read");
            said.push(format!("MCP server \"{name}\" was skipped: {message}"));
        }
        let blocked: Vec<&str> = listed("mcp_servers")
            .into_iter()
            .filter(|s| str_at(s, "status").is_some_and(|status| status.contains("block")))
            .filter_map(|s| str_at(s, "name"))
            .collect();
        if !blocked.is_empty() {
            let noun = if blocked.len() == 1 { "server" } else { "servers" };
            said.push(format!("MCP {noun} blocked by enterprise policy: {}", blocked.join(", ")));
        }
        let mut outs = Vec::new();
        for words in said {
            if self.said_once.0.insert(format!("init:{words}")) {
                let key = format!("init-{}", self.said_once.0.len());
                outs.extend(self.passing(&key, &words, now));
            }
        }
        outs
    }

    /// The MCP servers' tools as `mcp_status` lists them: each that names a `ui://` resource is
    /// an app.
    pub(super) fn servers_listed(&mut self, said: &Value) -> Vec<Out> {
        let mut apps = HashMap::new();
        let servers = said.get("mcpServers").and_then(Value::as_array).into_iter().flatten();
        for server in servers {
            let Some(name) = str_at(server, "name") else { continue };
            let tools = server.get("tools").and_then(Value::as_array).into_iter().flatten();
            for tool in tools {
                let Some(tool_name) = str_at(tool, "name") else { continue };
                let meta = tool.get("_meta");
                let ui = meta.and_then(|m| m.get("ui"));
                let uri = ui
                    .and_then(|u| str_at(u, "resourceUri"))
                    .or_else(|| meta.and_then(|m| str_at(m, "ui/resourceUri")))
                    .filter(|u| slopty_proto::visual::is_ui_uri(u));
                let Some(uri) = uri else { continue };
                let display_mode = ui
                    .and_then(|u| {
                        str_at(u, "displayMode").or_else(|| str_at(u, "preferredDisplayMode"))
                    })
                    .map(str::to_owned);
                apps.insert(
                    (normalized(name), tool_name.to_owned()),
                    McpApp {
                        server: name.to_owned(),
                        tool: tool_name.to_owned(),
                        resource_uri: uri.to_owned(),
                        display_mode,
                        result: None,
                    },
                );
            }
        }
        self.history.set_apps(apps);
        Vec::new()
    }

    /// A page's ask `ask` of its app, by its number `app`: answered later
    /// ([`Self::take_app_answers`]).
    pub fn app(&mut self, app: u64, ask: &AppAsk) {
        let request = match ask {
            AppAsk::Read { server, uri } => {
                json!({ "subtype": "mcp_read_resource", "serverName": server, "uri": uri })
            }
            AppAsk::Call { server, tool, arguments } => {
                let arguments: Value = serde_json::from_str(arguments)
                    .ok()
                    .filter(Value::is_object)
                    .unwrap_or_else(|| json!({}));
                json!({
                    "subtype": "mcp_call",
                    "tool": format!("mcp__{}__{tool}", normalized(server)),
                    "arguments": arguments,
                    "result_not_for_model": true,
                })
            }
        };
        self.control(Control::App(app), &request);
    }

    /// The answers to pages' asks since last taken, by each ask's number.
    pub fn take_app_answers(&mut self) -> Vec<(u64, AppAnswer)> {
        std::mem::take(&mut self.app_answers)
    }

    pub(super) fn app_answered(&mut self, app: u64, said: &Value) {
        let answer = match said.get("contents").and_then(Value::as_array) {
            Some(contents) => {
                let Some(first) = contents.first() else {
                    self.app_answers
                        .push((app, AppAnswer::Refused("The resource is empty".to_owned())));
                    return;
                };
                let body = match (str_at(first, "text"), str_at(first, "blob")) {
                    (Some(text), _) => AppBody::Text(text.to_owned()),
                    (None, Some(blob)) => match data_encoding::BASE64.decode(blob.as_bytes()) {
                        Ok(bytes) => AppBody::Bytes(bytes),
                        Err(e) => {
                            let why = format!("The resource's bytes are not base64: {e}");
                            self.app_answers.push((app, AppAnswer::Refused(why)));
                            return;
                        }
                    },
                    (None, None) => AppBody::Text(String::new()),
                };
                AppAnswer::Resource {
                    uri: str_at(first, "uri").unwrap_or_default().to_owned(),
                    mime_type: str_at(first, "mimeType").map(str::to_owned),
                    body,
                    meta: first.get("_meta").map(Value::to_string).unwrap_or_default(),
                }
            }
            None => AppAnswer::Called { result: said.to_string() },
        };
        self.app_answers.push((app, answer));
    }

    /// Put the files back as they were before turn `turn` began, through Claude Code's own
    /// checkpoints; a turn whose message is not known says so at once.
    pub fn rewind(&mut self, turn: TurnId, now: WallMs) -> Vec<Out> {
        let Some(prompt) = self.history.prompt_of(turn) else {
            let text = "The files could not be put back: that turn's message is not known";
            return self.history.notice(Notice::INFO, text, now);
        };
        let request = json!({ "subtype": "rewind_files", "user_message_id": prompt });
        self.control(Control::Rewind(turn), &request);
        Vec::new()
    }

    pub(super) fn rewound(&mut self, _turn: TurnId, said: &Value, now: WallMs) -> Vec<Out> {
        if said.get("canRewind").and_then(Value::as_bool) == Some(false) {
            let why = str_at(said, "error").unwrap_or("no checkpoint for that message");
            return self.history.notice(
                Notice::INFO,
                &format!("The files could not be put back: {why}"),
                now,
            );
        }
        let files = said.get("filesChanged").and_then(Value::as_array).map_or(0, Vec::len);
        let added = u64_at(said, "insertions").unwrap_or_default();
        let removed = u64_at(said, "deletions").unwrap_or_default();
        let files = if files == 1 { "1 file".to_owned() } else { format!("{files} files") };
        let text = format!("Put back {files} (+{added} −{removed})");
        self.history.notice(Notice::REWOUND, &text, now)
    }

    /// Hand the `!` command `ran` to Claude Code as the person's message, intent `intent`: one
    /// it answers when `query`, else one it only takes in (its `respondToBashCommands`), which it
    /// never echoes, so the thread takes it in as sent.
    pub fn shell(&mut self, ran: &Ran, intent: IntentId, query: bool, now: WallMs) -> Vec<Out> {
        let input = format!("<bash-input>{}</bash-input>", ran.command);
        let output = format!(
            "<bash-stdout>{}</bash-stdout><bash-stderr>{}</bash-stderr>",
            ran.stdout, ran.stderr
        );
        let content =
            json!([{ "type": "text", "text": input }, { "type": "text", "text": output }]);
        let uuid = message_id(intent);
        self.history.sent_as(&uuid, intent);
        let pending = Pending {
            intent,
            text: format!("!{}", ran.command),
            attachments: Vec::new(),
            delivery: Delivery::Steer,
            state: PendingState::Sending,
        };
        let mut outs = self.give_as(uuid.clone(), pending, &content, query, now);
        if !query {
            let record = json!({
                "type": "user",
                "message": { "role": "user", "content": content },
                "parent_tool_use_id": null,
                "uuid": uuid,
            });
            self.last_echo = Some(uuid);
            outs.extend(self.one_record(&record, now));
        }
        outs
    }

    /// Draw `surface` again: component `component`'s instance `instance`.
    fn render(&mut self, surface: &str, component: &str, instance: &str) {
        let request = json!({
            "subtype": "ui_render",
            "surface": SURFACE,
            "client_id": SURFACE_CLIENT,
            "component": component,
            "instance_id": instance,
            "props": {},
            "viewport": { "columns": 100, "rows": 30, "isFullscreen": false },
        });
        let ui = Ui::Render {
            surface: surface.to_owned(),
            component: component.to_owned(),
            instance: instance.to_owned(),
        };
        let id = self.control(Control::Ui(ui), &request);
        self.quiet.insert(id);
    }

    /// The person acted in `surface`, on element `element`: what the thread shows of it at
    /// once, or `None` when no such element shows.
    pub fn surface(
        &mut self,
        surface: &str,
        element: &str,
        event: &SurfaceEvent,
    ) -> Option<Vec<Out>> {
        if *event == SurfaceEvent::Close {
            if self.surfaces.panes.contains_key(surface) {
                let request =
                    json!({ "subtype": "ui_close", "id": surface, "client_id": SURFACE_CLIENT });
                self.control(Control::Ui(Ui::Act { surface: surface.to_owned() }), &request);
                return Some(Vec::new());
            }
            // A toast or a status segment the person put away goes on this side alone.
            self.surfaces.elements.remove(surface);
            return Some(self.history.tell(Action::SurfaceGone { id: surface.to_owned() }));
        }
        let found = self.surfaces.elements.get(surface).and_then(|e| e.get(element)).cloned()?;
        let mut request = match event {
            SurfaceEvent::Press => json!({ "subtype": "ui_press" }),
            SurfaceEvent::Input(value) => {
                json!({ "subtype": "ui_input", "kind": "submit", "value": value })
            }
            SurfaceEvent::Select(value) => json!({ "subtype": "ui_select", "value": value }),
            SurfaceEvent::Close => return None,
        };
        if let Some(fields) = request.as_object_mut() {
            fields.insert("plugin".to_owned(), Value::String(found.plugin));
            fields.insert("handle".to_owned(), Value::from(found.handle));
            fields.insert("surface".to_owned(), Value::String(SURFACE.to_owned()));
            fields.insert("client_id".to_owned(), Value::String(SURFACE_CLIENT.to_owned()));
            if let Some(key) = found.key {
                fields.insert("key".to_owned(), Value::String(key));
            }
            if *event != SurfaceEvent::Press {
                fields.insert("component".to_owned(), Value::String(found.component));
                fields.insert("instance_id".to_owned(), Value::String(found.instance));
            }
        }
        self.control(Control::Ui(Ui::Act { surface: surface.to_owned() }), &request);
        Some(Vec::new())
    }

    /// What a mod pushed (`ui_toast`, `ui_status`, `ui_log`, `ui_panes`, `ui_invalidate`).
    pub(super) fn ui_pushed(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        let plugin = str_at(message, "plugin").unwrap_or_default().to_owned();
        let text = str_at(message, "text").map(str::to_owned);
        match str_at(message, "subtype").unwrap_or_default() {
            "ui_toast" => {
                self.surfaces.toasts = self.surfaces.toasts.saturating_add(1);
                let shows = u64_at(message, "timeout_ms").unwrap_or(TOAST_MS);
                let surface = Surface {
                    id: format!("toast:{plugin}:{}", self.surfaces.toasts),
                    kind: Surface::TOAST.to_owned(),
                    plugin,
                    title: None,
                    text,
                    tree: String::new(),
                    until_ms: Some(WallMs::from_millis(now.as_millis().saturating_add(shows))),
                };
                self.history.tell(Action::SurfaceSet(surface))
            }
            "ui_status" => {
                let id = format!("status:{plugin}");
                match text.filter(|t| !t.is_empty()) {
                    Some(text) => self.history.tell(Action::SurfaceSet(Surface {
                        id,
                        kind: Surface::STATUS.to_owned(),
                        plugin,
                        title: None,
                        text: Some(text),
                        tree: String::new(),
                        until_ms: None,
                    })),
                    None => self.history.tell(Action::SurfaceGone { id }),
                }
            }
            "ui_log" => {
                let Some(text) = text else { return Vec::new() };
                self.history.notice(Notice::INFO, &format!("{plugin}: {text}"), now)
            }
            "ui_panes" => self.panes(message),
            "ui_invalidate" => {
                let named: Vec<String> = message
                    .get("instances")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|i| {
                        str_at(i, "instance_id").or_else(|| i.as_str()).map(str::to_owned)
                    })
                    .collect();
                let mut panes: Vec<String> = self.surfaces.panes.keys().cloned().collect();
                panes.sort();
                for pane in panes.into_iter().filter(|p| named.is_empty() || named.contains(p)) {
                    self.render(&pane, "Pane", &pane);
                }
                if named.is_empty() || named.iter().any(|n| n == BAND) {
                    self.render(BAND, "AbovePrompt", BAND);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// The pane roster as Claude Code holds it: each pane drawn, a pane closed gone.
    fn panes(&mut self, roster: &Value) -> Vec<Out> {
        let listed: Vec<(String, Option<String>, String)> = roster
            .get("panes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|p| {
                Some((
                    str_at(p, "id")?.to_owned(),
                    str_at(p, "title").map(str::to_owned),
                    str_at(p, "plugin").unwrap_or_default().to_owned(),
                ))
            })
            .collect();
        let mut outs = Vec::new();
        let gone: Vec<String> = self
            .surfaces
            .panes
            .keys()
            .filter(|id| !listed.iter().any(|(l, ..)| l == *id))
            .cloned()
            .collect();
        for id in gone {
            self.surfaces.panes.remove(&id);
            self.surfaces.elements.remove(&id);
            outs.extend(self.history.tell(Action::SurfaceGone { id }));
        }
        for (id, title, plugin) in listed {
            self.surfaces.panes.insert(id.clone(), (title, plugin));
            self.render(&id, "Pane", &id);
        }
        outs
    }

    /// What Claude Code answered an ask of the surfaces.
    pub(super) fn ui_answered(&mut self, ui: &Ui, said: &Value, _now: WallMs) -> Vec<Out> {
        match ui {
            Ui::Attach => Vec::new(),
            Ui::Panes => self.panes(said),
            Ui::Act { surface } => {
                if self.surfaces.panes.contains_key(surface) {
                    let closed = said.get("closed").and_then(Value::as_bool) == Some(true);
                    if closed {
                        self.surfaces.panes.remove(surface);
                        self.surfaces.elements.remove(surface);
                        return self.history.tell(Action::SurfaceGone { id: surface.clone() });
                    }
                    let pane = surface.clone();
                    self.render(&pane, "Pane", &pane);
                } else if surface == BAND {
                    self.render(BAND, "AbovePrompt", BAND);
                }
                Vec::new()
            }
            Ui::Render { surface, component, instance } => {
                let tree = said.get("tree").filter(|t| !t.is_null());
                let hooked = said.get("hooked").and_then(Value::as_bool) != Some(false);
                let Some(tree) = tree.filter(|_| hooked) else {
                    let shown = if surface == BAND {
                        std::mem::take(&mut self.surfaces.band)
                    } else {
                        self.surfaces.elements.contains_key(surface)
                    };
                    self.surfaces.elements.remove(surface);
                    if !shown {
                        return Vec::new();
                    }
                    return self.history.tell(Action::SurfaceGone { id: surface.clone() });
                };
                let mut tree = tree.clone();
                let mut elements = HashMap::new();
                ided(&mut tree, component, instance, &mut elements);
                let pane = self.surfaces.panes.get(surface).cloned();
                let plugin = pane
                    .as_ref()
                    .map(|(_, p)| p.clone())
                    .filter(|p| !p.is_empty())
                    .or_else(|| plugin_in(&tree))
                    .unwrap_or_default();
                let kind = if surface == BAND { Surface::BAND } else { Surface::PANE };
                if surface == BAND {
                    self.surfaces.band = true;
                }
                self.surfaces.elements.insert(surface.clone(), elements);
                self.history.tell(Action::SurfaceSet(Surface {
                    id: surface.clone(),
                    kind: kind.to_owned(),
                    plugin,
                    title: pane.and_then(|(t, _)| t),
                    text: None,
                    tree: tree.to_string(),
                    until_ms: None,
                }))
            }
        }
    }

    /// Claude Code is gone: what pages asked of it is refused.
    pub(super) fn doors_gone(&mut self, _now: WallMs) -> Vec<Out> {
        let apps: Vec<u64> = self
            .controls
            .values()
            .filter_map(|c| if let Control::App(app) = c { Some(*app) } else { None })
            .collect();
        self.controls.retain(|_, c| !matches!(c, Control::App(_)));
        for app in apps {
            self.app_answers.push((app, AppAnswer::Refused("Claude Code ended".to_owned())));
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server's name reads as Claude Code names its tools; each pressable element gets an id
    /// its act names.
    #[test]
    fn names_and_elements_read_as_claude_code_has_them() {
        assert_eq!(normalized("my server.v2"), "my_server_v2");
        let mut tree = json!({ "type": "Box", "children": [
            { "type": "Button", "props": { "key": "go", "label": "Go" },
              "press": { "plugin": "ship", "handle": 7 } },
            "words",
        ]});
        let mut elements = HashMap::new();
        ided(&mut tree, "Pane", "deploy", &mut elements);
        assert_eq!(tree.pointer("/children/0/id").and_then(Value::as_str), Some("ship:7"));
        let element = elements.get("ship:7").expect("the button");
        assert_eq!((element.handle, element.key.as_deref()), (7, Some("go")));
        assert_eq!(plugin_in(&tree).as_deref(), Some("ship"));
    }
}
