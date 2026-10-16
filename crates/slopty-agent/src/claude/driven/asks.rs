//! Claude Code's own asks beside its permissions: the dialogs its tools raise
//! (`request_user_dialog`) and its MCP servers' forms (`elicitation`), each a request of the thread
//! the person answers. Slopty never answers one for them.
//!
//! - **Dialogs.** `initialize` declares every kind 2.1.301 has that a request can carry
//!   ([`DIALOG_KINDS`]); one Claude Code raises is a request of kind `dialog:<kind>` with its
//!   payload as it came. The answers a kind takes are its choices, from the payload's own `options`
//!   when it gives them, else the kind's ([`choices`]); any dialog also takes a JSON object as its
//!   result, and [`Request::CANCEL`]. The permission prompts Claude Code can raise as dialogs are
//!   not declared: they come as `can_use_tool`, answered as approvals. Nor are its Ink dialogs
//!   (`local_jsx`), which carry no data to show, nor the elicitation dialogs, which come as
//!   `elicitation`.
//! - **Forms.** An MCP server's elicitation is a request of kind [`Request::ELICITATION`]: its
//!   message, its server, its schema (its properties in the server's order), or the page to fill it
//!   in at. It is answered with the filled-in object, [`Request::DECLINE`] or [`Request::CANCEL`];
//!   a page's form is answered "accept" once the person is done there.

use serde_json::{Value, json};
use slopty_core::WallMs;
use slopty_proto::thread::{Answerer, AskId, Choice, Clipped, Effect, Request, RequestState};

use super::{Answered, Asked, Driven, str_at};
use crate::claude::history::Out;

/// Every dialog kind Claude Code 2.1.301 raises over the stream that Slopty shows
/// (`initialize`'s `supportedDialogKinds`).
pub const DIALOG_KINDS: [&str; 21] = [
    "auto_mode_flagged_allow",
    "auto_mode_outside_reads",
    "auto_mode_setup_review",
    "chrome_install_setup",
    "chrome_install_upsell",
    "computer_use_approval",
    "device_mcp_consent",
    "fable_overage_consent_prompt",
    "goal_proposal",
    "ide_onboarding",
    "left_arrow_confirm",
    "lsp_recommendation",
    "managed_settings_security",
    "mcp_elicitation_waiting",
    "peer_inbound_approval",
    "plugin_hint",
    "refusal_fallback_prompt",
    "remote_callout",
    "sandbox_network_access",
    "ultraplan_launch",
    "unattended_serving_consent",
];

/// What kind of ask an open request is.
#[derive(Clone, Debug)]
pub(super) enum Form {
    /// A tool call's permission, or `AskUserQuestion`.
    Tool,
    /// A dialog of kind `kind`, the results its choices give.
    Dialog {
        /// Its title.
        title: String,
        /// Each choice's id, and the result it gives.
        results: Vec<(String, Value)>,
    },
    /// An MCP server's form.
    Elicitation {
        /// Its message.
        message: String,
        /// Filled in at a page, not here.
        url: bool,
    },
}

impl Form {
    /// What the thread waits on while it is open.
    pub(super) fn waits_with(&self) -> String {
        match self {
            Self::Tool => String::new(),
            Self::Dialog { title, .. } => title.clone(),
            Self::Elicitation { message, .. } => message.clone(),
        }
    }
}

/// The response `choice` answers an ask of `form` with, as Claude Code takes it; `None` for a
/// choice it does not take.
pub(super) fn result_of(form: &Form, choice: &str) -> Option<Value> {
    let object = || serde_json::from_str::<Value>(choice).ok().filter(Value::is_object);
    match form {
        Form::Tool => None,
        Form::Dialog { .. } if choice == Request::CANCEL => {
            Some(json!({ "behavior": "cancelled" }))
        }
        Form::Dialog { results, .. } => results
            .iter()
            .find(|(id, _)| id == choice)
            .map(|(_, result)| result.clone())
            .or_else(object)
            .map(|result| json!({ "behavior": "completed", "result": result })),
        Form::Elicitation { .. } if choice == Request::CANCEL => {
            Some(json!({ "action": "cancel" }))
        }
        Form::Elicitation { .. } if choice == Request::DECLINE => {
            Some(json!({ "action": "decline" }))
        }
        Form::Elicitation { url: true, .. } if choice == "accept" => {
            Some(json!({ "action": "accept" }))
        }
        Form::Elicitation { .. } => {
            object().map(|content| json!({ "action": "accept", "content": content }))
        }
    }
}

/// Why Claude Code asks about tool `tool` in `request` (`can_use_tool`), in its TUI's words: the
/// classifier, the rule, the hook or the check that wants the person's word, in permission mode
/// `mode`. `None` when it says nothing.
pub(super) fn reason(request: &Value, tool: &str, mode: Option<&str>) -> Option<String> {
    let field = |name: &str| str_at(request, name).map(str::trim).filter(|s| !s.is_empty());
    let what = if tool == "Bash" { "command" } else { "tool" };
    let said = field("decision_reason");
    let then = |head: String| match said {
        Some(said) if !head.contains(said) => format!("{head}\n{said}"),
        _ => head,
    };
    let words = match field("decision_reason_type") {
        Some("classifier") => {
            let head = match field("decision_reason_classifier").or_else(|| field("classifier")) {
                Some(name) if name != "auto-mode" && name != "auto" => {
                    format!("Classifier {name} requires confirmation for this {what}.")
                }
                _ => format!("Auto mode classifier requires confirmation for this {what}."),
            };
            then(head)
        }
        Some("rule") => {
            let rule = field("matched_ask_rule").or_else(|| field("decision_reason_rule"));
            match (rule, mode) {
                (Some(rule), Some("auto")) => {
                    format!("Ask rule {rule} overrides auto mode for this {what}.")
                }
                (Some(rule), _) => match field("decision_reason_rule_source") {
                    Some(source) => format!(
                        "Permission rule {rule} from {source} requires confirmation for this \
                         {what}."
                    ),
                    None => {
                        format!("Permission rule {rule} requires confirmation for this {what}.")
                    }
                },
                (None, _) => said?.to_owned(),
            }
        }
        Some("hook") => {
            let hook = field("decision_reason_hook_event").unwrap_or("A hook");
            let head = match said {
                Some(said) => format!("Hook {hook} requires confirmation for this {what}: {said}"),
                None => format!("Hook {hook} requires confirmation for this {what}."),
            };
            match field("decision_reason_hook_source") {
                Some(source) => format!("{head} [{source}]"),
                None => head,
            }
        }
        _ => said?.to_owned(),
    };
    Some(words)
}

/// The choices a dialog of `kind` with `payload` offers, each with the result it gives, and
/// its title and words.
fn choices(kind: &str, payload: &Value) -> (String, Option<String>, Vec<(Choice, Value)>) {
    let text = |name: &str| str_at(payload, name).map(str::to_owned);
    let pick = |id: &str, label: &str, effect: Effect| -> (Choice, Value) {
        let choice = Choice {
            id: id.to_owned(),
            label: label.to_owned(),
            effect,
            scope: None,
            stops: false,
        };
        (choice, Value::String(id.to_owned()))
    };
    // The payload's own options, where it gives them.
    let offered: Vec<(Choice, Value)> = payload
        .get("options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|o| {
            let value = str_at(o, "value")?;
            let label = str_at(o, "label").unwrap_or(value);
            Some(pick(value, label, Effect::Answer))
        })
        .collect();
    let title = text("title").or_else(|| text("question"));
    let words = text("question")
        .filter(|q| Some(q) != title.as_ref())
        .or_else(|| text("explainer"))
        .or_else(|| text("guidanceText"))
        .or_else(|| text("summary"))
        .or_else(|| text("pluginDescription"));
    let (fallback, options) = match kind {
        "refusal_fallback_prompt" => {
            let model = text("fallbackModel").unwrap_or_else(|| "another model".to_owned());
            (
                "The model declined: try another?".to_owned(),
                vec![
                    pick("retry_fallback", &format!("Retry with {model}"), Effect::Allow),
                    pick("edit_prompt", "Edit the prompt", Effect::Deny),
                ],
            )
        }
        "goal_proposal" => {
            let yes = Choice {
                id: "approve".to_owned(),
                label: "Set this goal".to_owned(),
                effect: Effect::Allow,
                scope: None,
                stops: false,
            };
            let no = Choice {
                id: "reject".to_owned(),
                label: "Not now".to_owned(),
                effect: Effect::Deny,
                ..yes.clone()
            };
            let condition = text("condition").unwrap_or_default();
            (
                format!("Work towards a goal: {condition}"),
                vec![
                    (yes, json!({ "approved": true, "explicit": true })),
                    (no, json!({ "approved": false })),
                ],
            )
        }
        "sandbox_network_access" => {
            let host = text("host").unwrap_or_default();
            let title = format!("Allow the sandbox to reach {host}?");
            let choice = |id: &str, label: &str, effect, scope| Choice {
                id: id.to_owned(),
                label: label.to_owned(),
                effect,
                scope,
                stops: false,
            };
            (
                title,
                vec![
                    (
                        choice("allow", "Allow", Effect::Allow, None),
                        json!({ "allow": true, "persistToSettings": false }),
                    ),
                    (
                        choice("always", "Always allow", Effect::Allow, Some(host)),
                        json!({ "allow": true, "persistToSettings": true }),
                    ),
                    (
                        choice("deny", "Deny", Effect::Deny, None),
                        json!({ "allow": false, "persistToSettings": false }),
                    ),
                ],
            )
        }
        "plugin_hint" => (
            format!("Install the plugin {}?", text("pluginName").unwrap_or_default()),
            vec![
                pick("yes", "Install", Effect::Allow),
                pick("no", "Not now", Effect::Deny),
                pick("disable", "Don't suggest plugins", Effect::Deny),
            ],
        ),
        "lsp_recommendation" => (
            format!("Install the plugin {}?", text("pluginName").unwrap_or_default()),
            vec![
                pick("yes", "Install", Effect::Allow),
                pick("no", "Not now", Effect::Deny),
                pick("never", "Never for this kind of file", Effect::Deny),
                pick("disable", "Don't suggest plugins", Effect::Deny),
            ],
        ),
        "fable_overage_consent_prompt" => (
            "Use extra usage for this model?".to_owned(),
            vec![
                pick("consent", "Use extra usage", Effect::Allow),
                pick("switch_default", "Switch to the default model", Effect::Deny),
            ],
        ),
        "ultraplan_launch" => (
            "Run the plan remotely?".to_owned(),
            vec![pick("run", "Run", Effect::Allow), pick("cancel", "Don't run", Effect::Deny)],
        ),
        "managed_settings_security" => (
            "Your organisation's settings changed: accept them?".to_owned(),
            vec![
                pick("approved", "Accept", Effect::Allow),
                pick("rejected", "Reject", Effect::Deny),
            ],
        ),
        "auto_mode_setup_review" => (
            "Turn on auto mode?".to_owned(),
            vec![
                pick("accept", "Turn on", Effect::Allow),
                pick("decline", "Not now", Effect::Deny),
            ],
        ),
        "chrome_install_setup" => (
            "Set up Claude in Chrome".to_owned(),
            vec![
                pick("continue", "Continue", Effect::Allow),
                pick("keep_waiting", "Keep waiting", Effect::Answer),
                pick("skip", "Skip", Effect::Deny),
            ],
        ),
        "chrome_install_upsell" => (
            "Install Claude in Chrome?".to_owned(),
            vec![
                pick("install", "Install", Effect::Allow),
                pick("not_now", "Not now", Effect::Deny),
                pick("dont_ask_again", "Don't ask again", Effect::Deny),
            ],
        ),
        "device_mcp_consent" => (
            "Let this machine's MCP servers be used?".to_owned(),
            vec![
                pick("allow", "Allow", Effect::Allow),
                pick("deny", "Deny", Effect::Deny),
                pick("not_now", "Not now", Effect::Deny),
            ],
        ),
        "unattended_serving_consent" => (
            "Serve sessions while you are away?".to_owned(),
            vec![
                pick("accept", "Accept", Effect::Allow),
                pick("decline", "Decline", Effect::Deny),
                pick("not_now", "Not now", Effect::Deny),
            ],
        ),
        "left_arrow_confirm" => (
            "Leave this session?".to_owned(),
            vec![pick("confirm", "Leave", Effect::Allow), pick("stay", "Stay", Effect::Deny)],
        ),
        "remote_callout" => (
            "Enable Remote Control?".to_owned(),
            vec![pick("enable", "Enable", Effect::Allow), pick("dismiss", "Not now", Effect::Deny)],
        ),
        "ide_onboarding" => {
            ("Connected to your IDE".to_owned(), vec![pick("dismissed", "OK", Effect::Answer)])
        }
        "mcp_elicitation_waiting" => (
            "Waiting on an MCP server's form".to_owned(),
            vec![
                pick("retry", "Retry", Effect::Allow),
                pick("dismiss", "Dismiss", Effect::Deny),
                pick("cancel", "Cancel the call", Effect::Deny),
            ],
        ),
        "peer_inbound_approval" => (
            "Another session wants to talk to this one".to_owned(),
            vec![
                (
                    Choice {
                        id: "approve".to_owned(),
                        label: "Allow".to_owned(),
                        effect: Effect::Allow,
                        scope: None,
                        stops: false,
                    },
                    json!({ "behavior": "approve" }),
                ),
                (
                    Choice {
                        id: "deny".to_owned(),
                        label: "Deny".to_owned(),
                        effect: Effect::Deny,
                        scope: None,
                        stops: false,
                    },
                    json!({ "behavior": "deny" }),
                ),
            ],
        ),
        // A kind with no choices of its own: shown from its payload, answered with a result.
        other => (other.replace('_', " "), Vec::new()),
    };
    let options = if offered.is_empty() { options } else { offered };
    (title.unwrap_or(fallback), words, options)
}

impl Driven {
    /// A dialog Claude Code raises (`request_user_dialog`): a request of the thread.
    pub(super) fn dialog(&mut self, id: &str, request: &Value, now: WallMs) -> Vec<Out> {
        if self.asks.values().any(|a| a.request == id) {
            return Vec::new();
        }
        let kind = str_at(request, "dialog_kind").unwrap_or_default().to_owned();
        let payload = request.get("payload").cloned().unwrap_or_else(|| json!({}));
        let (title, words, options) = choices(&kind, &payload);
        let results =
            options.iter().map(|(choice, result)| (choice.id.clone(), result.clone())).collect();
        let request_item =
            str_at(request, "tool_use_id").map(|c| slopty_proto::thread::ItemId(c.to_owned()));
        let number = self.ask_number();
        let shown = Request {
            id: AskId(number.to_string()),
            item: request_item,
            kind: format!("{}{kind}", Request::DIALOG),
            title: title.clone(),
            text: words.map(|w| Clipped::whole(&w)),
            options: options.into_iter().map(|(choice, _)| choice).collect(),
            questions: Vec::new(),
            proposed: None,
            schema_json: Some(payload.to_string()),
            url: None,
            reason: None,
            state: RequestState::Open,
            opened_ms: now,
            until_ms: None,
            needs_presence: false,
        };
        let form = Form::Dialog { title, results };
        self.asks.insert(number, Asked::of(id, form));
        let mut outs = self.history.asked(shown);
        outs.extend(self.status_now(now));
        outs
    }

    /// An MCP server's form (`elicitation`): a request of the thread.
    pub(super) fn elicitation(&mut self, id: &str, request: &Value, now: WallMs) -> Vec<Out> {
        if self.asks.values().any(|a| a.request == id) {
            return Vec::new();
        }
        let server = str_at(request, "mcp_server_name").unwrap_or("An MCP server").to_owned();
        let message = str_at(request, "message").unwrap_or_default().to_owned();
        let url = str_at(request, "url").map(str::to_owned);
        let schema = request.get("requested_schema").map(|schema| {
            let mut schema = schema.clone();
            // The properties in the server's order, for a client whose JSON keeps none.
            if let (Some(order), Some(fields)) =
                (request.get("property_order"), schema.as_object_mut())
            {
                fields.insert("x-property-order".to_owned(), order.clone());
            }
            schema.to_string()
        });
        let title = str_at(request, "title")
            .or_else(|| str_at(request, "display_name"))
            .map_or_else(|| server.clone(), str::to_owned);
        let choice = |id: &str, label: &str, effect| Choice {
            id: id.to_owned(),
            label: label.to_owned(),
            effect,
            scope: None,
            stops: false,
        };
        let mut options = Vec::new();
        if url.is_some() {
            options.push(choice("accept", "Done", Effect::Allow));
        }
        options.push(choice(Request::DECLINE, "Decline", Effect::Deny));
        options.push(choice(Request::CANCEL, "Cancel", Effect::Deny));
        let number = self.ask_number();
        let shown = Request {
            id: AskId(number.to_string()),
            item: None,
            kind: Request::ELICITATION.to_owned(),
            title,
            text: Some(Clipped::whole(&message)),
            options,
            questions: Vec::new(),
            proposed: None,
            schema_json: schema,
            url: url.clone(),
            reason: None,
            state: RequestState::Open,
            opened_ms: now,
            until_ms: None,
            needs_presence: false,
        };
        let form = Form::Elicitation { message, url: url.is_some() };
        self.asks.insert(number, Asked::of(id, form));
        let mut outs = self.history.asked(shown);
        outs.extend(self.status_now(now));
        outs
    }

    const fn ask_number(&mut self) -> u64 {
        self.next_ask = self.next_ask.saturating_add(1);
        self.next_ask
    }

    /// The person's answer `choice` to dialog or form `ask`, given by `by`.
    pub(super) fn answer_ask(
        &mut self,
        ask: &AskId,
        asked: &Asked,
        choice: &str,
        by: Answerer,
    ) -> Answered {
        let response = result_of(&asked.form, choice).unwrap_or(Value::Null);
        let line = json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": asked.request, "response": response },
        });
        self.writes.push(line.to_string());
        let state = RequestState::Answered { by, choice: choice.to_owned() };
        let mut outs = self.history.resolved(ask, state);
        outs.extend(self.status_now(WallMs::now()));
        Answered { outs }
    }
}

impl Asked {
    /// An ask of `form`, under control request `request`.
    fn of(request: &str, form: Form) -> Self {
        Self {
            request: request.to_owned(),
            tool: String::new(),
            input: Value::Null,
            suggestions: Value::Null,
            form,
        }
    }
}
